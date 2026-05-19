use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use feibai_core::*;

struct DictEntry {
    word: String,
    weight: u64,
}

pub struct PinyinEngine {
    table: HashMap<String, Vec<DictEntry>>,
    syllables: HashSet<&'static str>,
    preedit: String,
    segments: Vec<String>,
    selected_words: Vec<String>,
    selected_seg_counts: Vec<usize>,
    candidates: Vec<Candidate>,
    chinese_mode: bool,
    shift_pressed_alone: bool,
    userdb_path: Option<PathBuf>,
}

impl PinyinEngine {
    fn new_empty() -> Self {
        Self {
            table: HashMap::new(),
            syllables: PINYIN_SYLLABLES.iter().copied().collect(),
            preedit: String::new(),
            segments: Vec::new(),
            selected_words: Vec::new(),
            selected_seg_counts: Vec::new(),
            candidates: Vec::new(),
            chinese_mode: true,
            shift_pressed_alone: false,
            userdb_path: None,
        }
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, String> {
        let mut engine = Self::new_empty();
        engine.load_dict(path)?;
        engine.sort_entries();
        Ok(engine)
    }

    pub fn from_files(paths: &[impl AsRef<Path>]) -> Result<Self, String> {
        let mut engine = Self::new_empty();
        for path in paths {
            engine.load_dict(path)?;
        }
        engine.sort_entries();
        Ok(engine)
    }

    pub fn set_userdb_path(&mut self, path: impl Into<PathBuf>) {
        self.userdb_path = Some(path.into());
    }

    fn load_dict(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| format!("failed to read dict {}: {e}", path.as_ref().display()))?;
        self.parse_rime_dict(&content);
        Ok(())
    }

    fn parse_rime_dict(&mut self, content: &str) {
        let mut in_header = false;
        let mut past_header = false;

        for line in content.lines() {
            let trimmed = line.trim();

            if !past_header {
                if trimmed == "---" {
                    in_header = true;
                    continue;
                }
                if trimmed == "..." {
                    in_header = false;
                    past_header = true;
                    continue;
                }
                if in_header {
                    continue;
                }
                // Lines before first `---` are comments
                continue;
            }

            // Past header: parse entries
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let mut parts = trimmed.splitn(3, '\t');
            let word = match parts.next() {
                Some(w) if !w.is_empty() => w,
                _ => continue,
            };
            let pinyin_raw = match parts.next() {
                Some(p) if !p.is_empty() => p,
                _ => continue,
            };
            let weight: u64 = parts
                .next()
                .and_then(|w| w.trim().parse().ok())
                .unwrap_or(1);

            // Concatenate pinyin syllables (remove spaces) as lookup key
            let key: String = pinyin_raw.split_whitespace().collect();

            self.table.entry(key).or_default().push(DictEntry {
                word: word.to_string(),
                weight,
            });
        }
    }

    pub fn load_userdb(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| format!("failed to read userdb {}: {e}", path.as_ref().display()))?;
        self.parse_userdb(&content);
        self.sort_entries();
        Ok(())
    }

    fn parse_userdb(&mut self, content: &str) {
        for line in content.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            // Format: "pinyin syllables \tword\tc=N d=... t=..."
            let mut parts = line.splitn(3, '\t');
            let pinyin_raw = match parts.next() {
                Some(p) if !p.is_empty() => p.trim(),
                _ => continue,
            };
            let word = match parts.next() {
                Some(w) if !w.is_empty() => w,
                _ => continue,
            };
            let meta = parts.next().unwrap_or("");
            let weight: u64 = meta
                .split_whitespace()
                .find_map(|s| s.strip_prefix("c="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);

            let key: String = pinyin_raw.split_whitespace().collect();

            self.table.entry(key).or_default().push(DictEntry {
                word: word.to_string(),
                weight,
            });
        }
    }

    fn sort_entries(&mut self) {
        for entries in self.table.values_mut() {
            entries.sort_by(|a, b| b.weight.cmp(&a.weight));
        }
    }

    fn update_segments(&mut self) {
        let preedit = self.preedit.clone();
        self.segments = self.segment(&preedit).iter().map(|s| s.to_string()).collect();
    }

    fn total_selected_segs(&self) -> usize {
        self.selected_seg_counts.iter().sum()
    }

    fn lookup(&mut self) {
        let remaining = &self.segments[self.total_selected_segs()..];
        if remaining.is_empty() {
            self.candidates = Vec::new();
            return;
        }

        let n = remaining.len();
        self.candidates = Vec::new();
        let mut seen = HashSet::new();

        // From longest match down to single syllable, collect candidates
        for end in (1..=n.min(8)).rev() {
            let key: String = remaining[..end].concat();
            if let Some(entries) = self.table.get(&key) {
                for e in entries.iter().take(9 - self.candidates.len()) {
                    if seen.insert(e.word.clone()) {
                        self.candidates.push(Candidate {
                            text: e.word.clone(),
                            comment: None,
                        });
                    }
                }
                if self.candidates.len() >= 9 {
                    break;
                }
            }
        }
    }

    /// Select a candidate: consume the corresponding syllables, or commit if done
    fn select_candidate(&mut self, idx: usize) -> Vec<EngineAction> {
        let text = match self.candidates.get(idx) {
            Some(c) => c.text.clone(),
            None => return vec![EngineAction::Noop],
        };

        let offset = self.total_selected_segs();
        let remaining = &self.segments[offset..];
        let seg_count = self.find_seg_count_for_word(&text, remaining);

        self.selected_words.push(text);
        self.selected_seg_counts.push(seg_count);

        // If all syllables consumed, commit the full sentence
        if self.total_selected_segs() >= self.segments.len() {
            let sentence: String = self.selected_words.concat();
            let all_segs = self.segments.clone();
            self.learn_from_commit(&sentence, &all_segs);
            self.clear_composition();
            return vec![EngineAction::Commit(sentence)];
        }

        // Otherwise, update preedit display and show new candidates
        self.lookup();
        let preedit_display = self.compose_preedit();
        vec![
            EngineAction::UpdatePreedit(preedit_display),
            EngineAction::UpdateCandidates(self.candidates.clone()),
        ]
    }

    /// Learn from a commit: only record if the combination is new (not in base dict)
    fn learn_from_commit(&mut self, sentence: &str, pinyin_segs: &[String]) {
        // Single char commits: skip (base dict covers them)
        if sentence.chars().count() <= 1 {
            return;
        }

        let key: String = pinyin_segs.concat();

        // Check if this exact word already exists with high weight (already in base dict)
        if let Some(entries) = self.table.get(&key) {
            if entries.iter().any(|e| e.word == sentence && e.weight >= 1000) {
                return;
            }
        }

        let pinyin_spaced = pinyin_segs.join(" ");
        let boost: u64 = 1_000_000;

        // Add/boost in memory
        if let Some(entries) = self.table.get_mut(&key) {
            if let Some(entry) = entries.iter_mut().find(|e| e.word == sentence) {
                entry.weight = entry.weight.saturating_add(boost);
            } else {
                entries.push(DictEntry { word: sentence.to_string(), weight: boost });
            }
            entries.sort_by(|a, b| b.weight.cmp(&a.weight));
        } else {
            self.table.insert(key, vec![DictEntry { word: sentence.to_string(), weight: boost }]);
        }

        // Write to user dict file
        if let Some(path) = &self.userdb_path {
            if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{}\t{}\tc={}", pinyin_spaced, sentence, boost);
            }
        }
    }

    fn find_seg_count_for_word(&self, word: &str, remaining: &[String]) -> usize {
        for end in 1..=remaining.len().min(8) {
            let key: String = remaining[..end].concat();
            if let Some(entries) = self.table.get(&key) {
                if entries.iter().any(|e| e.word == word) {
                    return end;
                }
            }
        }
        // Fallback: the Viterbi full-sentence candidate covers all remaining
        remaining.len()
    }

    fn compose_preedit(&self) -> String {
        let selected: String = self.selected_words.concat();
        let remaining_pinyin: String = self.segments[self.total_selected_segs()..].join("");
        format!("{}{}", selected, remaining_pinyin)
    }

    fn clear_composition(&mut self) {
        self.preedit.clear();
        self.segments.clear();
        self.selected_words.clear();
        self.selected_seg_counts.clear();
        self.candidates.clear();
    }

    /// Segment pinyin string into syllables using greedy longest-match
    fn segment<'a>(&self, input: &'a str) -> Vec<&'a str> {
        let mut result = Vec::new();
        let mut pos = 0;
        let len = input.len();

        while pos < len {
            let mut best_end = 0;
            let max_len = (len - pos).min(6);
            for l in (1..=max_len).rev() {
                let slice = &input[pos..pos + l];
                if self.syllables.contains(slice) {
                    best_end = l;
                    break;
                }
            }
            if best_end == 0 {
                best_end = 1;
            }
            result.push(&input[pos..pos + best_end]);
            pos += best_end;
        }
        result
    }

    pub fn is_chinese_mode(&self) -> bool {
        self.chinese_mode
    }
}

const PINYIN_SYLLABLES: &[&str] = &[
    "a", "ai", "an", "ang", "ao",
    "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi", "bian", "biang", "biao", "bie", "bin", "bing", "bo", "bu",
    "ca", "cai", "can", "cang", "cao", "ce", "cei", "cen", "ceng", "cha", "chai", "chan", "chang", "chao", "che", "chen", "cheng", "chi", "chong", "chou", "chu", "chua", "chuai", "chuan", "chuang", "chui", "chun", "chuo", "ci", "cong", "cou", "cu", "cuan", "cui", "cun", "cuo",
    "da", "dai", "dan", "dang", "dao", "de", "dei", "den", "deng", "di", "dia", "dian", "diao", "die", "ding", "diu", "dong", "dou", "du", "duan", "dui", "dun", "duo",
    "e", "ei", "en", "eng", "er",
    "fa", "fan", "fang", "fei", "fen", "feng", "fiao", "fo", "fou", "fu",
    "ga", "gai", "gan", "gang", "gao", "ge", "gei", "gen", "geng", "gong", "gou", "gu", "gua", "guai", "guan", "guang", "gui", "gun", "guo",
    "ha", "hai", "han", "hang", "hao", "he", "hei", "hen", "heng", "hong", "hou", "hu", "hua", "huai", "huan", "huang", "hui", "hun", "huo",
    "ji", "jia", "jian", "jiang", "jiao", "jie", "jin", "jing", "jiong", "jiu", "ju", "juan", "jue", "jun",
    "ka", "kai", "kan", "kang", "kao", "ke", "kei", "ken", "keng", "kong", "kou", "ku", "kua", "kuai", "kuan", "kuang", "kui", "kun", "kuo",
    "la", "lai", "lan", "lang", "lao", "le", "lei", "leng", "li", "lia", "lian", "liang", "liao", "lie", "lin", "ling", "liu", "lo", "long", "lou", "lu", "luan", "lun", "luo", "lv", "lve",
    "ma", "mai", "man", "mang", "mao", "me", "mei", "men", "meng", "mi", "mian", "miao", "mie", "min", "ming", "miu", "mo", "mou", "mu",
    "na", "nai", "nan", "nang", "nao", "ne", "nei", "nen", "neng", "ni", "nian", "niang", "niao", "nie", "nin", "ning", "niu", "nong", "nou", "nu", "nuan", "nuo", "nv", "nve",
    "o", "ou",
    "pa", "pai", "pan", "pang", "pao", "pei", "pen", "peng", "pi", "pian", "piao", "pie", "pin", "ping", "po", "pou", "pu",
    "qi", "qia", "qian", "qiang", "qiao", "qie", "qin", "qing", "qiong", "qiu", "qu", "quan", "que", "qun",
    "ran", "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru", "rua", "ruan", "rui", "run", "ruo",
    "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "sha", "shai", "shan", "shang", "shao", "she", "shei", "shen", "sheng", "shi", "shou", "shu", "shua", "shuai", "shuan", "shuang", "shui", "shun", "shuo", "si", "song", "sou", "su", "suan", "sui", "sun", "suo",
    "ta", "tai", "tan", "tang", "tao", "te", "tei", "teng", "ti", "tian", "tiao", "tie", "ting", "tong", "tou", "tu", "tuan", "tui", "tun", "tuo",
    "wa", "wai", "wan", "wang", "wei", "wen", "weng", "wo", "wu",
    "xi", "xia", "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong", "xiu", "xu", "xuan", "xue", "xun",
    "ya", "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yo", "yong", "you", "yu", "yuan", "yue", "yun",
    "za", "zai", "zan", "zang", "zao", "ze", "zei", "zen", "zeng", "zha", "zhai", "zhan", "zhang", "zhao", "zhe", "zhei", "zhen", "zheng", "zhi", "zhong", "zhou", "zhu", "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun", "zhuo", "zi", "zong", "zou", "zu", "zuan", "zui", "zun", "zuo",
];

const KEYSYM_SHIFT_L: u32 = 0xffe1;
const KEYSYM_SHIFT_R: u32 = 0xffe2;

impl Engine for PinyinEngine {
    fn process_key(&mut self, key: &KeyEvent) -> Vec<EngineAction> {
        let keysym = key.keysym;

        // Track Shift for toggle: press alone (no other key in between) = toggle
        if keysym == KEYSYM_SHIFT_L || keysym == KEYSYM_SHIFT_R {
            if key.state == KeyState::Press {
                self.shift_pressed_alone = true;
                return vec![EngineAction::Noop];
            } else {
                // Release — if no other key was pressed, toggle mode
                if self.shift_pressed_alone {
                    self.shift_pressed_alone = false;
                    self.chinese_mode = !self.chinese_mode;
                    // If switching to english, commit current composition
                    if !self.chinese_mode && !self.preedit.is_empty() {
                        let text = self.compose_preedit();
                        self.clear_composition();
                        return vec![
                            EngineAction::Commit(text),
                            EngineAction::UpdatePreedit(String::new()),
                            EngineAction::UpdateCandidates(Vec::new()),
                        ];
                    }
                    return vec![EngineAction::Noop];
                }
                return vec![EngineAction::Noop];
            }
        }

        // Any non-shift key press cancels the "shift alone" tracking
        if key.state == KeyState::Press {
            self.shift_pressed_alone = false;
        }

        // English mode — forward everything (both press and release)
        if !self.chinese_mode {
            return vec![EngineAction::Forward];
        }

        if key.state == KeyState::Release {
            return vec![EngineAction::Noop];
        }

        if key.modifiers.ctrl || key.modifiers.alt || key.modifiers.super_ {
            return vec![EngineAction::Forward];
        }

        // Escape — clear everything
        if keysym == 0xff1b {
            if self.preedit.is_empty() {
                return vec![EngineAction::Forward];
            }
            self.clear_composition();
            return vec![
                EngineAction::UpdatePreedit(String::new()),
                EngineAction::UpdateCandidates(Vec::new()),
            ];
        }

        // Backspace
        if keysym == 0xff08 {
            if self.preedit.is_empty() {
                return vec![EngineAction::Forward];
            }
            // If there are selected words, cancel the last selection
            if !self.selected_words.is_empty() {
                self.selected_words.pop();
                self.selected_seg_counts.pop();
            } else {
                // Delete last character from raw preedit
                self.preedit.pop();
                self.update_segments();
            }
            if self.preedit.is_empty() {
                self.clear_composition();
                return vec![
                    EngineAction::UpdatePreedit(String::new()),
                    EngineAction::UpdateCandidates(Vec::new()),
                ];
            }
            self.lookup();
            let preedit_display = self.compose_preedit();
            return vec![
                EngineAction::UpdatePreedit(preedit_display),
                EngineAction::UpdateCandidates(self.candidates.clone()),
            ];
        }

        // Enter — commit what we have (selected words + raw remaining pinyin)
        if keysym == 0xff0d {
            if self.preedit.is_empty() {
                return vec![EngineAction::Forward];
            }
            let text = self.compose_preedit();
            self.clear_composition();
            return vec![EngineAction::Commit(text)];
        }

        // Space — select first candidate (may partially commit or fully commit)
        if keysym == 0x20 {
            if !self.candidates.is_empty() {
                return self.select_candidate(0);
            }
            if self.preedit.is_empty() {
                return vec![EngineAction::Forward];
            }
            return vec![EngineAction::Noop];
        }

        // Digit 1-9 — select candidate by index
        if let Some(ch) = key.unicode
            && ('1'..='9').contains(&ch)
            && !self.candidates.is_empty()
        {
            let idx = (ch as usize) - ('1' as usize);
            if idx < self.candidates.len() {
                return self.select_candidate(idx);
            }
            return vec![EngineAction::Noop];
        }

        // Letter keys — append to preedit
        if let Some(ch) = key.unicode
            && ch.is_ascii_lowercase()
        {
            self.preedit.push(ch);
            self.update_segments();
            self.lookup();
            let preedit_display = self.compose_preedit();
            return vec![
                EngineAction::UpdatePreedit(preedit_display),
                EngineAction::UpdateCandidates(self.candidates.clone()),
            ];
        }

        if self.preedit.is_empty() {
            return vec![EngineAction::Forward];
        }

        vec![EngineAction::Noop]
    }

    fn reset(&mut self) {
        self.clear_composition();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press_key(engine: &mut PinyinEngine, c: char) -> Vec<EngineAction> {
        let key = KeyEvent {
            keysym: c as u32,
            unicode: Some(c),
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        };
        engine.process_key(&key)
    }

    fn make_engine() -> PinyinEngine {
        PinyinEngine::from_file("/home/zhiyjia/Downloads/rime-ice/cn_dicts/8105.dict.yaml").unwrap()
    }

    #[test]
    fn test_type_pinyin_shows_candidates() {
        let mut engine = make_engine();
        let actions = press_key(&mut engine, 'n');
        assert!(actions.iter().any(|a| matches!(a, EngineAction::UpdatePreedit(s) if s == "n")));

        let actions = press_key(&mut engine, 'i');
        assert!(actions
            .iter()
            .any(|a| matches!(a, EngineAction::UpdateCandidates(c) if !c.is_empty())));
    }

    #[test]
    fn test_space_commits_first() {
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
        let key = KeyEvent {
            keysym: 0x20,
            unicode: Some(' '),
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        };
        let actions = engine.process_key(&key);
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == "你")));
    }

    #[test]
    fn test_digit_selects_candidate() {
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
        let key = KeyEvent {
            keysym: '2' as u32,
            unicode: Some('2'),
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        };
        let actions = engine.process_key(&key);
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == "尼")));
    }

    #[test]
    fn test_backspace_deletes() {
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
        let key = KeyEvent {
            keysym: 0xff08,
            unicode: None,
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        };
        let actions = engine.process_key(&key);
        assert!(actions.iter().any(|a| matches!(a, EngineAction::UpdatePreedit(s) if s == "n")));
    }

    #[test]
    fn test_escape_clears() {
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
        let key = KeyEvent {
            keysym: 0xff1b,
            unicode: None,
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        };
        let actions = engine.process_key(&key);
        assert!(actions.iter().any(|a| matches!(a, EngineAction::UpdatePreedit(s) if s.is_empty())));
    }

    #[test]
    fn test_enter_commits_raw() {
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
        let key = KeyEvent {
            keysym: 0xff0d,
            unicode: None,
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        };
        let actions = engine.process_key(&key);
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == "ni")));
    }

    #[test]
    fn test_modifier_key_forwards() {
        let mut engine = make_engine();
        let key = KeyEvent {
            keysym: 'c' as u32,
            unicode: Some('c'),
            modifiers: Modifiers {
                ctrl: true,
                ..Default::default()
            },
            state: KeyState::Press,
        };
        let actions = engine.process_key(&key);
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Forward)));
    }

    fn shift_press(engine: &mut PinyinEngine) -> Vec<EngineAction> {
        engine.process_key(&KeyEvent {
            keysym: KEYSYM_SHIFT_L,
            unicode: None,
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        })
    }

    fn shift_release(engine: &mut PinyinEngine) -> Vec<EngineAction> {
        engine.process_key(&KeyEvent {
            keysym: KEYSYM_SHIFT_L,
            unicode: None,
            modifiers: Modifiers::default(),
            state: KeyState::Release,
        })
    }

    #[test]
    fn test_shift_toggles_chinese_english() {
        let mut engine = make_engine();
        assert!(engine.is_chinese_mode());

        shift_press(&mut engine);
        shift_release(&mut engine);
        assert!(!engine.is_chinese_mode());

        shift_press(&mut engine);
        shift_release(&mut engine);
        assert!(engine.is_chinese_mode());
    }

    #[test]
    fn test_english_mode_forwards_all() {
        let mut engine = make_engine();

        shift_press(&mut engine);
        shift_release(&mut engine);
        assert!(!engine.is_chinese_mode());

        let actions = press_key(&mut engine, 'a');
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Forward)));

        let actions = press_key(&mut engine, 'z');
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Forward)));
    }

    #[test]
    fn test_shift_with_other_key_does_not_toggle() {
        let mut engine = make_engine();
        assert!(engine.is_chinese_mode());

        shift_press(&mut engine);
        press_key(&mut engine, 'a');
        shift_release(&mut engine);

        assert!(engine.is_chinese_mode());
    }

    #[test]
    fn test_switch_to_english_commits_preedit() {
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');

        shift_press(&mut engine);
        let actions = shift_release(&mut engine);
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == "ni")));
        assert!(!engine.is_chinese_mode());
    }

    #[test]
    fn test_multi_dict_loading() {
        let engine = PinyinEngine::from_files(&[
            "/home/zhiyjia/Downloads/rime-ice/cn_dicts/8105.dict.yaml",
            "/home/zhiyjia/Downloads/rime-ice/cn_dicts/base.dict.yaml",
        ])
        .unwrap();
        assert!(engine.is_chinese_mode());
    }

    #[test]
    fn test_word_lookup() {
        let mut engine = PinyinEngine::from_files(&[
            "/home/zhiyjia/Downloads/rime-ice/cn_dicts/8105.dict.yaml",
            "/home/zhiyjia/Downloads/rime-ice/cn_dicts/base.dict.yaml",
        ])
        .unwrap();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
        press_key(&mut engine, 'h');
        press_key(&mut engine, 'a');
        let actions = press_key(&mut engine, 'o');
        let has_nihao = actions.iter().any(|a| {
            matches!(a, EngineAction::UpdateCandidates(c) if c.iter().any(|x| x.text == "你好"))
        });
        assert!(has_nihao, "expected '你好' in candidates for 'nihao'");
    }

    #[test]
    fn test_long_pinyin_segmentation() {
        let mut engine = PinyinEngine::from_files(&[
            "/home/zhiyjia/Downloads/rime-ice/cn_dicts/8105.dict.yaml",
            "/home/zhiyjia/Downloads/rime-ice/cn_dicts/base.dict.yaml",
        ])
        .unwrap();
        // Type "jintian" — should match "今天"
        for c in "jintian".chars() {
            press_key(&mut engine, c);
        }
        let has_jintian = engine.candidates.iter().any(|x| x.text == "今天");
        assert!(has_jintian, "expected '今天' in candidates for 'jintian'");
    }

    #[test]
    fn test_segmentation_helper() {
        let engine = PinyinEngine::new_empty();
        let segs = engine.segment("jintiantianqizenmeyang");
        assert_eq!(segs, vec!["jin", "tian", "tian", "qi", "zen", "me", "yang"]);
    }

    #[test]
    fn test_progressive_selection_flow() {
        let mut engine = PinyinEngine::from_files(&[
            "/home/zhiyjia/Downloads/rime-ice/cn_dicts/8105.dict.yaml",
            "/home/zhiyjia/Downloads/rime-ice/cn_dicts/base.dict.yaml",
        ])
        .unwrap();

        // Type "jintian"
        for c in "jintian".chars() {
            press_key(&mut engine, c);
        }

        // First candidate should be "今天" (longest match)
        assert!(!engine.candidates.is_empty());
        assert_eq!(engine.candidates[0].text, "今天");

        // Select "今天" — covers all syllables, should commit
        let key = KeyEvent {
            keysym: 0x20,
            unicode: Some(' '),
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        };
        let actions = engine.process_key(&key);
        assert!(
            actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == "今天")),
            "expected commit '今天', got: {:?}", actions
        );
    }

    #[test]
    fn test_progressive_word_selection() {
        let mut engine = PinyinEngine::from_files(&[
            "/home/zhiyjia/Downloads/rime-ice/cn_dicts/8105.dict.yaml",
            "/home/zhiyjia/Downloads/rime-ice/cn_dicts/base.dict.yaml",
        ])
        .unwrap();

        // Type "nihao"
        for c in "nihao".chars() {
            press_key(&mut engine, c);
        }

        // Should have candidates — first one should be 你好 (sentence)
        assert!(!engine.candidates.is_empty());

        // Select "你好" with space — since it covers all syllables, should commit
        let key = KeyEvent {
            keysym: 0x20,
            unicode: Some(' '),
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        };
        let actions = engine.process_key(&key);
        assert!(
            actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == "你好")),
            "expected commit '你好', got: {:?}", actions
        );
    }
}
