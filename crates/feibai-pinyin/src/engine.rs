use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use feibai_core::*;

struct DictEntry {
    word: String,
    weight: u64,
    user: bool,
}

const PAGE_SIZE: usize = 9;

pub struct PinyinEngine {
    table: BTreeMap<String, Vec<DictEntry>>,
    syllables: HashSet<&'static str>,
    preedit: String,
    segments: Vec<String>,
    selected_words: Vec<String>,
    selected_seg_counts: Vec<usize>,
    candidates: Vec<Candidate>,
    page: usize,
    chinese_mode: bool,
    shift_pressed_alone: bool,
    userdb_path: Option<PathBuf>,
}

impl PinyinEngine {
    fn new_empty() -> Self {
        Self {
            table: BTreeMap::new(),
            syllables: PINYIN_SYLLABLES.iter().copied().collect(),
            preedit: String::new(),
            segments: Vec::new(),
            selected_words: Vec::new(),
            selected_seg_counts: Vec::new(),
            candidates: Vec::new(),
            page: 0,
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
                user: false,
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

            let entries = self.table.entry(key).or_default();
            if let Some(existing) = entries.iter_mut().find(|e| e.word == word) {
                existing.weight = weight;
                existing.user = true;
            } else {
                entries.push(DictEntry {
                    word: word.to_string(),
                    weight,
                    user: true,
                });
            }
        }
    }

    fn sort_entries(&mut self) {
        for entries in self.table.values_mut() {
            entries.sort_by(|a, b| Self::cmp_entry(b, a));
        }
    }

    fn cmp_entry(a: &DictEntry, b: &DictEntry) -> std::cmp::Ordering {
        // User entries always rank above non-user entries
        a.user.cmp(&b.user).then(a.weight.cmp(&b.weight))
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
            self.page = 0;
            return;
        }

        let n = remaining.len();
        self.candidates = Vec::new();
        self.page = 0;

        // Check if the last segment is incomplete (not a valid syllable)
        let last_seg = &remaining[n - 1];
        let last_is_incomplete = !self.syllables.contains(last_seg.as_str());

        // Viterbi DP: only on complete syllables
        let complete_segs = if last_is_incomplete { &remaining[..n - 1] } else { remaining };
        let sentence = if complete_segs.len() > 1 {
            self.viterbi_best_sentence(complete_segs)
        } else {
            None
        };

        let mut seen = HashSet::new();
        if let Some(ref s) = sentence {
            seen.insert(s.clone());
            self.candidates.push(Candidate { text: s.clone(), comment: None });
        }

        // Exact match candidates for complete syllable spans
        let exact_end = if last_is_incomplete { n - 1 } else { n };
        for end in (1..=exact_end.min(8)).rev() {
            let key: String = remaining[..end].concat();
            if let Some(entries) = self.table.get(&key) {
                for e in entries.iter() {
                    if seen.insert(e.word.clone()) {
                        self.candidates.push(Candidate {
                            text: e.word.clone(),
                            comment: None,
                        });
                    }
                }
            }
        }

        // Prefix matching for trailing incomplete syllable (using BTreeMap range)
        if last_is_incomplete {
            let prefix: String = remaining.concat();
            let prefix_end = prefix_upper_bound(&prefix);
            let mut prefix_entries: Vec<&DictEntry> = self.table
                .range(prefix.clone()..prefix_end)
                .filter(|(k, _)| k.as_str() != prefix)
                .flat_map(|(_, entries)| entries.iter())
                .collect();
            prefix_entries.sort_by(|a, b| b.weight.cmp(&a.weight));
            for e in prefix_entries.iter().take(50) {
                if seen.insert(e.word.clone()) {
                    self.candidates.push(Candidate {
                        text: e.word.clone(),
                        comment: None,
                    });
                }
            }

            // Also try prefix on just the incomplete segment alone (single char candidates)
            if n == 1 || self.candidates.is_empty() {
                let seg_end = prefix_upper_bound(last_seg);
                let mut single_prefix_entries: Vec<&DictEntry> = self.table
                    .range(last_seg.clone()..seg_end)
                    .filter(|(k, _)| k.len() <= 6)
                    .flat_map(|(_, entries)| entries.iter())
                    .collect();
                single_prefix_entries.sort_by(|a, b| b.weight.cmp(&a.weight));
                for e in single_prefix_entries.iter().take(50) {
                    if seen.insert(e.word.clone()) {
                        self.candidates.push(Candidate {
                            text: e.word.clone(),
                            comment: None,
                        });
                    }
                }
            }
        }

        let segs_str: String = remaining.join("'");
        let top5: Vec<&str> = self.candidates.iter().take(5).map(|c| c.text.as_str()).collect();
        eprintln!("[feibai] lookup: {} → [{}] ({}个)", segs_str, top5.join(", "), self.candidates.len());
    }

    /// Viterbi DP: find the best sentence that covers all remaining syllables
    fn viterbi_best_sentence(&self, segs: &[String]) -> Option<String> {
        let n = segs.len();
        if n <= 1 {
            return None;
        }

        // dp[i] = (best_score, num_words, prev_position) for position i
        // score = sum of log(weight) for each word in the path
        // prefer fewer words (longer matches), break ties by score
        let mut dp_score: Vec<f64> = vec![f64::NEG_INFINITY; n + 1];
        let mut dp_words: Vec<usize> = vec![usize::MAX; n + 1];
        let mut dp_prev: Vec<usize> = vec![0; n + 1];
        let mut dp_word_text: Vec<String> = vec![String::new(); n + 1];

        dp_score[0] = 0.0;
        dp_words[0] = 0;

        for i in 0..n {
            if dp_score[i] == f64::NEG_INFINITY {
                continue;
            }
            let max_len = (n - i).min(8);
            for len in 1..=max_len {
                let key: String = segs[i..i + len].concat();
                if let Some(entries) = self.table.get(&key) {
                    if let Some(best) = entries.first() {
                        let word_score = (best.weight.max(1) as f64).ln();
                        let new_score = dp_score[i] + word_score;
                        let new_words = dp_words[i] + 1;
                        let j = i + len;

                        // Prefer fewer words; if same word count, prefer higher score
                        let better = dp_score[j] == f64::NEG_INFINITY
                            || new_words < dp_words[j]
                            || (new_words == dp_words[j] && new_score > dp_score[j]);

                        if better {
                            dp_score[j] = new_score;
                            dp_words[j] = new_words;
                            dp_prev[j] = i;
                            dp_word_text[j] = best.word.clone();
                        }
                    }
                }
            }
        }

        if dp_score[n] == f64::NEG_INFINITY {
            return None;
        }

        // Backtrace
        let mut words = Vec::new();
        let mut pos = n;
        while pos > 0 {
            words.push(dp_word_text[pos].clone());
            pos = dp_prev[pos];
        }
        words.reverse();

        let sentence: String = words.concat();
        // Only return if sentence is longer than a single word (otherwise it duplicates normal candidates)
        if words.len() > 1 {
            Some(sentence)
        } else {
            None
        }
    }

    fn current_page_candidates(&self) -> Vec<Candidate> {
        let start = self.page * PAGE_SIZE;
        let end = (start + PAGE_SIZE).min(self.candidates.len());
        if start >= self.candidates.len() {
            return Vec::new();
        }
        self.candidates[start..end].to_vec()
    }

    fn total_pages(&self) -> usize {
        (self.candidates.len() + PAGE_SIZE - 1) / PAGE_SIZE
    }

    fn select_candidate(&mut self, idx: usize) -> Vec<EngineAction> {
        let text = match self.candidates.get(idx) {
            Some(c) => c.text.clone(),
            None => return vec![EngineAction::Noop],
        };
        eprintln!("[feibai] select[{}]: {}", idx, text);

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
            EngineAction::UpdateCandidates(self.current_page_candidates()),
        ]
    }

    fn learn_from_commit(&mut self, sentence: &str, pinyin_segs: &[String]) {
        let key: String = pinyin_segs.concat();
        let is_single_char = sentence.chars().count() <= 1;

        // For single chars: 2-selection promotion strategy
        // 1st select: jump to just below current top (top - 1)
        // 2nd select: overtake the top
        if is_single_char {
            let mut changed = false;
            let mut persist_weight = 0u64;
            if let Some(entries) = self.table.get_mut(&key) {
                let top_other = entries.iter()
                    .filter(|e| e.user && e.word != sentence)
                    .map(|e| e.weight)
                    .max();
                if let Some(entry) = entries.iter_mut().find(|e| e.word == sentence) {
                    let old_weight = entry.weight;
                    match top_other {
                        Some(top) if entry.weight < top => {
                            if entry.weight < top.saturating_sub(1) {
                                entry.weight = top.saturating_sub(1);
                            } else {
                                entry.weight = top.saturating_add(1);
                            }
                        }
                        _ => {
                            entry.weight = entry.weight.saturating_add(1);
                        }
                    }
                    changed = entry.weight != old_weight || !entry.user;
                    entry.user = true;
                    persist_weight = entry.weight;
                    entries.sort_by(|a, b| Self::cmp_entry(b, a));
                }
            }
            if changed {
                let pinyin_spaced = pinyin_segs.join(" ");
                self.save_userdb_entry(&pinyin_spaced, sentence, persist_weight);
            }
            return;
        }

        // Multi-char: skip if it's a base dict word (weight 1000~999999)
        if let Some(entries) = self.table.get(&key) {
            if entries.iter().any(|e| e.word == sentence && e.weight >= 1000 && e.weight < 1_000_000) {
                return;
            }
        }

        let pinyin_spaced = pinyin_segs.join(" ");
        let base_weight: u64 = 1_000_000;

        // Add/boost in memory
        let new_weight;
        if let Some(entries) = self.table.get_mut(&key) {
            if let Some(entry) = entries.iter_mut().find(|e| e.word == sentence) {
                entry.weight = entry.weight.saturating_add(1);
                entry.user = true;
                new_weight = entry.weight;
            } else {
                new_weight = base_weight;
                entries.push(DictEntry { word: sentence.to_string(), weight: new_weight, user: true });
            }
            entries.sort_by(|a, b| Self::cmp_entry(b, a));
        } else {
            new_weight = base_weight;
            self.table.insert(key, vec![DictEntry { word: sentence.to_string(), weight: new_weight, user: true }]);
        }

        // Write to user dict file
        self.save_userdb_entry(&pinyin_spaced, sentence, new_weight);
    }

    fn save_userdb_entry(&self, pinyin_spaced: &str, word: &str, weight: u64) {
        let Some(path) = &self.userdb_path else { return };
        let content = fs::read_to_string(path).unwrap_or_default();
        let prefix = format!("{}\t{}\t", pinyin_spaced, word);
        let mut found = false;
        let mut lines: Vec<String> = content
            .lines()
            .map(|line| {
                if line.starts_with(&prefix) {
                    found = true;
                    format!("{}c={}", prefix, weight)
                } else {
                    line.to_string()
                }
            })
            .collect();
        if !found {
            lines.push(format!("{}c={}", prefix, weight));
        }
        let _ = fs::write(path, lines.join("\n") + "\n");
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
        // Check if it's a prefix match (trailing incomplete syllable)
        let full_key: String = remaining.concat();
        for (key, entries) in self.table.iter() {
            if key.starts_with(&full_key) && entries.iter().any(|e| e.word == word) {
                return remaining.len();
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
        self.page = 0;
    }

    /// Segment pinyin string into syllables using greedy longest-match.
    /// Incomplete trailing input (like "zh", "sh") is kept as a single segment.
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
                // No valid syllable found — this is trailing incomplete input.
                // Keep the entire remaining string as one segment.
                result.push(&input[pos..]);
                return result;
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

fn prefix_upper_bound(prefix: &str) -> String {
    let mut bytes = prefix.as_bytes().to_vec();
    // Increment the last byte to get the exclusive upper bound for range query
    while let Some(last) = bytes.last_mut() {
        if *last < 0xFF {
            *last += 1;
            return String::from_utf8(bytes).unwrap_or_else(|_| format!("{}~", prefix));
        }
        bytes.pop();
    }
    format!("{}~", prefix)
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
                EngineAction::UpdateCandidates(self.current_page_candidates()),
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

        // Page Down: = or Page_Down
        if (keysym == 0x3d || keysym == 0xff56) && !self.candidates.is_empty() {
            if self.page + 1 < self.total_pages() {
                self.page += 1;
            }
            return vec![EngineAction::UpdateCandidates(self.current_page_candidates())];
        }

        // Page Up: - or Page_Up
        if (keysym == 0x2d || keysym == 0xff55) && !self.candidates.is_empty() {
            if self.page > 0 {
                self.page -= 1;
            }
            return vec![EngineAction::UpdateCandidates(self.current_page_candidates())];
        }

        // Space — select first candidate on current page
        if keysym == 0x20 {
            if !self.candidates.is_empty() {
                let abs_idx = self.page * PAGE_SIZE;
                return self.select_candidate(abs_idx);
            }
            if self.preedit.is_empty() {
                return vec![EngineAction::Forward];
            }
            return vec![EngineAction::Noop];
        }

        // Digit 1-9 — select candidate by index (within current page)
        if let Some(ch) = key.unicode
            && ('1'..='9').contains(&ch)
            && !self.candidates.is_empty()
        {
            let page_idx = (ch as usize) - ('1' as usize);
            let abs_idx = self.page * PAGE_SIZE + page_idx;
            if abs_idx < self.candidates.len() {
                return self.select_candidate(abs_idx);
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
                EngineAction::UpdateCandidates(self.current_page_candidates()),
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

    const TEST_BASE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/dicts/feibai.base.dict.yaml");
    const TEST_EXTRA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/dicts/feibai.extra.dict.yaml");

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
        PinyinEngine::from_file(TEST_BASE).unwrap()
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
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == "泥")));
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
            TEST_BASE,
            TEST_EXTRA,
        ])
        .unwrap();
        assert!(engine.is_chinese_mode());
    }

    #[test]
    fn test_word_lookup() {
        let mut engine = PinyinEngine::from_files(&[
            TEST_BASE,
            TEST_EXTRA,
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
            TEST_BASE,
            TEST_EXTRA,
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
            TEST_BASE,
            TEST_EXTRA,
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
            TEST_BASE,
            TEST_EXTRA,
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
