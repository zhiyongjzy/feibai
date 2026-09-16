use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use feibai_core::*;

use crate::userdb::{DictEntry, UserBigram, key_top, now_week};

const PAGE_SIZE: usize = 9;

pub struct PinyinEngine {
    table: BTreeMap<String, Vec<DictEntry>>,
    en_dict: BTreeMap<String, u64>,
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
    bigram: UserBigram,
    last_context: Option<String>,
    userdb_dirty: bool,
}

impl PinyinEngine {
    fn new_empty() -> Self {
        Self {
            table: BTreeMap::new(),
            en_dict: BTreeMap::new(),
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
            bigram: UserBigram::default(),
            last_context: None,
            userdb_dirty: false,
        }
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, String> {
        let mut engine = Self::new_empty();
        engine.load_dict(path)?;
        Ok(engine)
    }

    pub fn from_files(paths: &[impl AsRef<Path>]) -> Result<Self, String> {
        let mut engine = Self::new_empty();
        for path in paths {
            engine.load_dict(path)?;
        }
        Ok(engine)
    }

    pub fn set_userdb_path(&mut self, path: impl Into<PathBuf>) {
        self.userdb_path = Some(path.into());
    }

    pub fn load_en_dict(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| format!("failed to read en dict {}: {e}", path.as_ref().display()))?;
        let mut past_header = false;
        let mut in_header = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if !past_header {
                if trimmed == "---" { in_header = true; continue; }
                if trimmed == "..." { in_header = false; past_header = true; continue; }
                if in_header { continue; }
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') { continue; }
            let mut parts = trimmed.splitn(2, '\t');
            let word = parts.next().unwrap_or("").to_lowercase();
            let weight: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            if word.len() >= 3 {
                self.en_dict.entry(word).or_insert(weight);
            }
        }
        Ok(())
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
                base_weight: weight,
                selection_count: 0,
                last_week: 0,
            });
        }
    }

    pub fn load_userdb(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("failed to read userdb {}: {e}", path.display()))?;
        let migrated = self.parse_userdb(&content);
        if migrated > 0 {
            // 首次迁移前备份原文件(幂等: 已有备份则不覆盖)
            let bak = path.with_extension("txt.legacy-bak");
            if !bak.exists() {
                if let Err(e) = fs::copy(path, &bak) {
                    // 备份失败不必中断加载,但不可静默吞掉
                    log_error!("failed to backup userdb {} -> {}: {e}", path.display(), bak.display());
                }
            }
            log_info!("userdb: migrated {migrated} legacy entries");
        }
        // bigram 与 unigram 同目录共存,重启后上下文延续
        let bigram_path = path.with_file_name("user.bigram.txt");
        self.bigram = UserBigram::load(&bigram_path)
            .map_err(|e| format!("failed to load user bigram: {e}"))?;
        Ok(())
    }

    /// 解析用户词典。返回迁移的旧格式行数(供备份决策)。
    /// 新格式: `拼音\t词\tc=次数 t=周`(拼音为连写形式,与 table key 一致)
    /// 旧格式: `拼音 空格分隔\t词\tc=绝对权重`(无 t= 字段,按 band 位置换算次数)
    fn parse_userdb(&mut self, content: &str) -> usize {
        let now = now_week();
        let mut migrated = 0;
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
            let mut count: Option<u32> = None;
            let mut week: Option<u32> = None;
            let mut has_t = false; // 区分"无 t= 字段(legacy)"与"t= 存在但非法(损坏)"
            for tok in meta.split_whitespace() {
                if let Some(v) = tok.strip_prefix("c=") {
                    count = v.parse().ok();
                } else if let Some(v) = tok.strip_prefix("t=") {
                    has_t = true;
                    week = v.parse().ok();
                }
            }
            let Some(c) = count else { continue };
            if has_t && week.is_none() {
                // 损坏行: 跳过,防止误入 legacy 迁移分支收缩计数
                log_error!("userdb: corrupted line (unparseable t=), skipped: {line}");
                continue;
            }
            let key: String = pinyin_raw.split_whitespace().collect();

            let entries = self.table.entry(key).or_default();
            let in_base = entries.iter().any(|e| e.word == word && e.selection_count == 0 && e.base_weight > 0);
            let (count, last_week) = match week {
                Some(w) => (c, w),
                None => {
                    // 旧格式绝对权重迁移(被丢弃的脏数据行不计入迁移数):
                    if c >= 1_000_000 {
                        entries.retain(|e| e.word != word); // 0.1.1 前误学脏数据,直接丢
                        continue;
                    }
                    migrated += 1;
                    let top = key_top(entries);
                    let migrated_count = if in_base && u64::from(c) < top { 1 } else { 2 };
                    (migrated_count, now)
                }
            };
            if let Some(existing) = entries.iter_mut().find(|e| e.word == word) {
                existing.selection_count = count;
                existing.last_week = last_week;
            } else {
                entries.push(DictEntry {
                    word: word.to_string(),
                    base_weight: 0,
                    selection_count: count,
                    last_week,
                });
            }
        }
        migrated
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

        // If any segment is not a valid syllable, offer raw input as first candidate
        let has_invalid_seg = remaining.iter().any(|s| !self.syllables.contains(s.as_str()));
        let raw_input: String = remaining.concat();
        if has_invalid_seg && raw_input.len() >= 3 {
            self.candidates.push(Candidate { text: raw_input.clone(), comment: None });
        }

        // Viterbi DP: only on complete syllables
        let complete_segs = if last_is_incomplete { &remaining[..n - 1] } else { remaining };
        let sentence = if complete_segs.len() > 1 {
            self.viterbi_best_sentence(complete_segs)
        } else {
            None
        };

        let mut seen = HashSet::new();
        if has_invalid_seg && raw_input.len() >= 3 {
            seen.insert(raw_input.clone());
        }
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
            prefix_entries.sort_by(|a, b| b.base_weight.cmp(&a.base_weight));
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
                single_prefix_entries.sort_by(|a, b| b.base_weight.cmp(&a.base_weight));
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

        // English dictionary matching (only when input is clearly non-pinyin AND >= 4 chars)
        if !self.en_dict.is_empty() && raw_input.len() >= 4 && has_invalid_seg {
            let en_upper = prefix_upper_bound(&raw_input);
            let mut en_candidates: Vec<(&String, &u64)> = self.en_dict
                .range(raw_input.clone()..en_upper)
                .collect();
            en_candidates.sort_by(|a, b| b.1.cmp(a.1));
            // Append after Chinese candidates instead of inserting near top
            for (word, _) in en_candidates.iter().take(5) {
                if seen.insert((*word).clone()) {
                    self.candidates.push(Candidate {
                        text: (*word).clone(),
                        comment: Some("en".to_string()),
                    });
                }
            }
        }

        let segs_str: String = remaining.join("'");
        let top5: Vec<&str> = self.candidates.iter().take(5).map(|c| c.text.as_str()).collect();
        log_debug!("lookup: {} → [{}] ({}个)", segs_str, top5.join(", "), self.candidates.len());
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
                        let word_score = (best.base_weight.max(1) as f64).ln();
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

    /// 防误学 guard: 沿用 0.1.1 规则 — 纯 ASCII 不学、多字词音节数不足不学、
    /// 含无效音节的碎片不学(碎片检查在 learn_phrase 内做)
    fn learnable(word: &str, segs: &[String]) -> bool {
        if word.chars().all(|c| c.is_ascii()) {
            return false;
        }
        let char_count = word.chars().count();
        if char_count > 1 {
            if segs.len() < char_count {
                return false;
            }
        }
        true
    }

    /// 选中即计数: 次数+1, 刷新周时间戳。base 词库信息永不覆盖。
    fn learn_word(&mut self, key: &str, word: &str, segs: &[String]) {
        if !Self::learnable(word, segs) {
            return;
        }
        let now = now_week();
        let entries = self.table.entry(key.to_string()).or_default();
        if let Some(e) = entries.iter_mut().find(|e| e.word == word) {
            e.selection_count = e.selection_count.saturating_add(1);
            e.last_week = now;
        } else {
            entries.push(DictEntry {
                word: word.to_string(),
                base_weight: 0,
                selection_count: 1,
                last_week: now,
            });
        }
        self.userdb_dirty = true;
    }

    /// 整句学习: 多词组合作为短语词条(淘词入库的数据源),同样计数制
    fn learn_phrase(&mut self, sentence: &str, segs: &[String]) {
        if !Self::learnable(sentence, segs) {
            return;
        }
        if segs.iter().any(|s| !self.syllables.contains(s.as_str())) {
            return;
        }
        let key: String = segs.concat();
        let now = now_week();
        let entries = self.table.entry(key).or_default();
        if let Some(e) = entries.iter_mut().find(|e| e.word == sentence) {
            e.selection_count = e.selection_count.saturating_add(1);
            e.last_week = now;
        } else {
            entries.push(DictEntry {
                word: sentence.to_string(),
                base_weight: 0,
                selection_count: 1,
                last_week: now,
            });
        }
        self.userdb_dirty = true;
    }

    /// 从内存全量写两个文件(旧实现是读改写整个文件,现在表即真相)
    fn save_userdb(&self) {
        let Some(path) = self.userdb_path.as_ref() else { return };
        let Some(dir) = path.parent().map(|d| d.to_path_buf()) else { return };
        let mut lines: Vec<String> = Vec::new();
        for (key, entries) in &self.table {
            for e in entries {
                if e.selection_count > 0 {
                    lines.push(format!("{}\t{}\tc={} t={}", key, e.word, e.selection_count, e.last_week));
                }
            }
        }
        lines.sort();
        // 空表写 0 字节文件,与 bigram 空文件语义一致
        let content = if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" };
        let tmp = path.with_extension("txt.tmp");
        if let Err(e) = fs::write(&tmp, content) {
            log_error!("failed to write {}: {e}", tmp.display());
            return;
        }
        if let Err(e) = fs::rename(&tmp, path) {
            log_error!("failed to rename {} -> {}: {e}", tmp.display(), path.display());
            let _ = fs::remove_file(&tmp);
        }
        self.bigram.save(&dir.join("user.bigram.txt"));
    }

    fn select_candidate(&mut self, idx: usize) -> Vec<EngineAction> {
        let text = match self.candidates.get(idx) {
            Some(c) => c.text.clone(),
            None => return vec![EngineAction::Noop],
        };
        log_debug!("select[{}]: {}", idx, text);

        let offset = self.total_selected_segs();
        let remaining = &self.segments[offset..];
        let seg_count = self.find_seg_count_for_word(&text, remaining);

        // 学习: 选中即计数 + 上下文 bigram(前词=上次选中/上屏词)
        let word_key: String = remaining[..seg_count].concat();
        let word_segs: Vec<String> = remaining[..seg_count].to_vec();
        self.bigram.bump(self.last_context.as_deref(), &text);
        self.learn_word(&word_key, &text, &word_segs);
        self.last_context = Some(text.clone());

        self.selected_words.push(text);
        self.selected_seg_counts.push(seg_count);

        // If all syllables consumed, commit the full sentence
        if self.total_selected_segs() >= self.segments.len() {
            let sentence: String = self.selected_words.concat();
            let all_segs = self.segments.clone();
            if self.selected_words.len() > 1 {
                self.learn_phrase(&sentence, &all_segs);
            }
            if self.userdb_dirty {
                self.save_userdb();
                self.userdb_dirty = false;
            }
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
        let upper = prefix_upper_bound(&full_key);
        for (_key, entries) in self.table.range(full_key..upper) {
            if entries.iter().any(|e| e.word == word) {
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
        let mut offset = 0;

        for part in input.split('\'') {
            if part.is_empty() {
                offset += 1;
                continue;
            }
            let part_start = offset;
            let segs = self.segment_dp(part);
            for seg in segs {
                let seg_start = part_start + (seg.as_ptr() as usize - part.as_ptr() as usize);
                result.push(&input[seg_start..seg_start + seg.len()]);
            }
            offset = part_start + part.len() + 1; // +1 for the '\''
        }
        result
    }

    fn segment_dp<'a>(&self, input: &'a str) -> Vec<&'a str> {
        let len = input.len();
        if len == 0 {
            return Vec::new();
        }

        // dp[i] = (chars_covered, segment_count, next_pos)
        // Goal: maximize chars_covered; on tie, minimize segment_count (prefer longer syllables)
        let mut dp: Vec<(usize, usize, usize)> = vec![(0, 0, 0); len + 1];
        // dp[len] = (0, 0, 0) — base case

        for i in (0..len).rev() {
            let max_syl_len = (len - i).min(6);
            let mut best: (usize, usize, usize) = (0, 0, i + 1); // (covered, segments, next)

            for l in 1..=max_syl_len {
                let slice = &input[i..i + l];
                if self.syllables.contains(slice) {
                    let (future_covered, future_segs, _) = dp[i + l];
                    let covered = future_covered + l;
                    let segs = future_segs + 1;
                    // Prefer: more chars covered, then fewer segments
                    if covered > best.0 || (covered == best.0 && segs < best.1) {
                        best = (covered, segs, i + l);
                    }
                }
            }

            dp[i] = best;
        }

        // If DP covers the entire input, trace the optimal path
        if dp[0].0 == len {
            let mut result = Vec::new();
            let mut pos = 0;
            while pos < len {
                let next = dp[pos].2;
                result.push(&input[pos..next]);
                pos = next;
            }
            return result;
        }

        // Fallback: greedy longest match with trailing incomplete segment
        let mut result = Vec::new();
        let mut pos = 0;
        while pos < len {
            let max_syl_len = (len - pos).min(6);
            let mut best_end = 0;
            for l in (1..=max_syl_len).rev() {
                let slice = &input[pos..pos + l];
                if self.syllables.contains(slice) {
                    best_end = l;
                    break;
                }
            }
            if best_end == 0 {
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
    "ha", "hai", "han", "hang", "hao", "he", "hei", "hen", "heng", "hm", "hng", "hong", "hou", "hu", "hua", "huai", "huan", "huang", "hui", "hun", "huo",
    "ji", "jia", "jian", "jiang", "jiao", "jie", "jin", "jing", "jiong", "jiu", "ju", "juan", "jue", "jun",
    "ka", "kai", "kan", "kang", "kao", "ke", "kei", "ken", "keng", "kong", "kou", "ku", "kua", "kuai", "kuan", "kuang", "kui", "kun", "kuo",
    "la", "lai", "lan", "lang", "lao", "le", "lei", "leng", "li", "lia", "lian", "liang", "liao", "lie", "lin", "ling", "liu", "lo", "long", "lou", "lu", "luan", "lun", "luo", "lv", "lve",
    "m", "ma", "mai", "man", "mang", "mao", "me", "mei", "men", "meng", "mi", "mian", "miao", "mie", "min", "ming", "miu", "mo", "mou", "mu",
    "n", "na", "nai", "nan", "nang", "nao", "ne", "nei", "nen", "neng", "ng", "ni", "nian", "niang", "niao", "nie", "nin", "ning", "niu", "nong", "nou", "nu", "nuan", "nuo", "nv", "nve",
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
            self.last_context = None; // 放弃当前输入流,上下文作废
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

        // Apostrophe — pinyin separator (e.g. xi'an)
        if key.keysym == 0x27 && !self.preedit.is_empty() {
            self.preedit.push('\'');
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

        // Punctuation or other keys during composition: commit preedit first, then forward
        if key.unicode.is_some() {
            let mut actions = self.select_candidate(0);
            actions.push(EngineAction::Forward);
            return actions;
        }

        vec![EngineAction::Noop]
    }

    fn reset(&mut self) {
        self.clear_composition();
        self.last_context = None; // FocusOut/Reset: 焦点丢失,上下文作废
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

        // DP resolves die+rtiao ambiguity → di+er+tiao (rtiao is invalid)
        let segs = engine.segment("diertiao");
        assert_eq!(segs, vec!["di", "er", "tiao"]);

        // Explicit separator forces die as a syllable
        let segs = engine.segment("die'ertiao");
        assert_eq!(segs, vec!["die", "er", "tiao"]);

        // Separator disambiguates: xi'an'anquan → 西安安全
        let segs = engine.segment("xi'an'anquan");
        assert_eq!(segs, vec!["xi", "an", "an", "quan"]);
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
    fn learn_promotion_overtakes_base_single_char() {
        // 又 (you) is blocked by base-dict 有/由; selecting it twice should overtake.
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        for _ in 0..2 {
            for c in "you".chars() { press_key(&mut engine, c); }
            let idx = engine.candidates.iter().position(|c| c.text == "又")
                .expect("又 should be a candidate for 'you'");
            assert!(idx < 9, "又 should be in first page, got idx {}", idx);
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        }
        for c in "you".chars() { press_key(&mut engine, c); }
        assert_eq!(engine.candidates[0].text, "又", "又 should rank #1 after 2 selections");
    }

    #[test]
    fn learn_promotion_overtakes_base_multichar() {
        // 复现 (fu xian) is blocked by base-dict 复线/浮现; selecting it twice should overtake.
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        for _ in 0..2 {
            for c in "fuxian".chars() { press_key(&mut engine, c); }
            let idx = engine.candidates.iter().position(|c| c.text == "复现")
                .expect("复现 should be a candidate for 'fuxian'");
            assert!(idx < 9, "复现 should be in first page, got idx {}", idx);
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        }
        for c in "fuxian".chars() { press_key(&mut engine, c); }
        assert_eq!(engine.candidates[0].text, "复现", "复现 should rank #1 after 2 selections");
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

    // TODO(task6): 依计划 Task 6 重写 — 引用已删除的 learn_from_commit/旧字段 weight·user
    /*
    #[test]
    fn test_mislearn_fragment_gets_low_weight() {
        // A multi-char word not present in any dictionary (a mislearn fragment like
        // 哟嗯 from yo+n segmentation) must land at NEW_WORD_WEIGHT, NOT the old
        // 1_000_000, so it cannot suppress real base-dict words.
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        let segs: Vec<String> = vec!["yo".into(), "n".into()];
        engine.learn_from_commit("哟嗯", &segs);
        let key: String = segs.concat();
        let entries = engine.table.get(&key).expect("fragment should be stored");
        let entry = entries.iter().find(|e| e.word == "哟嗯")
            .expect("哟嗯 entry should exist");
        assert_eq!(entry.weight, NEW_WORD_WEIGHT,
            "mislearned fragment must land at NEW_WORD_WEIGHT (10), got {}", entry.weight);
        assert!(entry.user, "learned fragment must be flagged user");
    }
    */

    // TODO(task6): 依计划 Task 6 重写 — 引用已删除的 learn_from_commit/旧字段 weight
    /*
    #[test]
    fn test_new_real_word_promotes_via_selections() {
        // A genuinely new multi-char word (not in dict) lands at NEW_WORD_WEIGHT,
        // then climbs via the promotion path: 1st select → NEW_WORD_WEIGHT (new entry),
        // 2nd select → top_other-1 (just below #1), 3rd select → top_other+1 (overtake).
        // This proves P0-1 does not block learning legitimate new words — it just
        // takes one extra selection vs. a word that already exists in the dict.
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        let segs: Vec<String> = vec!["ce".into(), "shi".into()];
        let key: String = segs.concat();
        // "测释" (2 chars, 2 syllables) is absent from the dict — stands in for any
        // user-coined word. Confirm precondition.
        assert!(engine.table.get(&key).map_or(true, |e| !e.iter().any(|x| x.word == "测释")),
            "precondition: word must not exist in dict");
        let top_other_of = |eng: &PinyinEngine| -> u64 {
            eng.table.get(&key).unwrap().iter()
                .filter(|e| e.word != "测释").map(|e| e.weight).max().unwrap_or(0)
        };

        // 1st select: new entry lands at NEW_WORD_WEIGHT (10).
        engine.learn_from_commit("测释", &segs);
        let w1 = engine.table.get(&key).unwrap().iter()
            .find(|e| e.word == "测释").unwrap().weight;
        assert_eq!(w1, NEW_WORD_WEIGHT, "1st select must land at NEW_WORD_WEIGHT");

        // 2nd select: jumps to top_other - 1 (just below 测试=2444 → 2443).
        let before2 = top_other_of(&engine);
        engine.learn_from_commit("测释", &segs);
        let w2 = engine.table.get(&key).unwrap().iter()
            .find(|e| e.word == "测释").unwrap().weight;
        assert_eq!(w2, before2.saturating_sub(1),
            "2nd select must climb to top_other-1, got {} (top_other={})", w2, before2);

        // 3rd select: overtakes → top_other + 1 (2445 > 2444).
        let before3 = top_other_of(&engine);
        engine.learn_from_commit("测释", &segs);
        let w3 = engine.table.get(&key).unwrap().iter()
            .find(|e| e.word == "测释").unwrap().weight;
        assert!(w3 > before3,
            "3rd select must overtake the top, got {} vs top_other {}", w3, before3);
    }
    */

    // TODO(task6): 依计划 Task 6 重写 — 引用已删除的旧 DictEntry 字面量构造/sort_entries
    /*
    #[test]
    fn test_same_weight_tiebreak_stable() {
        // Two entries under the same key with equal weight must sort deterministically:
        // user flag first, then longer word, then lexicographic. Sorting twice must
        // yield identical order (no instability).
        let mut engine = PinyinEngine::new_empty();
        let key = "xx".to_string();
        let mk = |word: &str, weight: u64, user: bool| DictEntry {
            word: word.to_string(), weight, user,
        };
        engine.table.insert(key.clone(), vec![
            mk("乙", 100, false),       // base, 1 char
            mk("甲", 100, false),       // base, 1 char — lexicographic before 乙
            mk("用户词", 100, true),    // user, 2 chars — should rank #1
            mk("丙", 100, false),       // base, 1 char
        ]);
        engine.sort_entries();
        let order1: Vec<String> = engine.table.get(&key).unwrap().iter()
            .map(|e| e.word.clone()).collect();
        // sort again — must be stable
        engine.sort_entries();
        let order2: Vec<String> = engine.table.get(&key).unwrap().iter()
            .map(|e| e.word.clone()).collect();
        assert_eq!(order1, order2, "sort must be stable across repeated calls");
        // user word (用户词, 2 chars) must rank ahead of all base 1-char words
        assert_eq!(order1[0], "用户词",
            "user word must rank first on weight tie, got {:?}", order1);
        // remaining base words sorted lexicographically (甲 < 乙 < 丙)
        assert_eq!(order1[1..], vec!["甲", "乙", "丙"],
            "base words tie-break must be lexicographic, got {:?}", order1);
    }
    */

    #[test]
    fn userdb_new_format_parses_count_and_week() {
        let mut engine = PinyinEngine::from_file(TEST_BASE).unwrap();
        let migrated = engine.parse_userdb("jiu\t旧\tc=5 t=100\n");
        assert_eq!(migrated, 0);
        let entries = engine.table.get("jiu").unwrap();
        let jiu = entries.iter().find(|e| e.word == "旧").unwrap();
        assert_eq!((jiu.selection_count, jiu.last_week), (5, 100));
    }

    #[test]
    fn legacy_absolute_weight_migrates_by_band_position() {
        let mut engine = PinyinEngine::from_file(TEST_BASE).unwrap();
        // 旧格式无 t= 字段。就=273122 为组内 top。
        // c ≥ key_top → 用户曾登顶 → count=2; 否则 count=1
        let migrated = engine.parse_userdb("jiu\t旧\tc=273121\njiu\t久\tc=273700\n");
        assert_eq!(migrated, 2);
        let entries = engine.table.get("jiu").unwrap();
        let jiu = entries.iter().find(|e| e.word == "旧").unwrap();
        let jiu2 = entries.iter().find(|e| e.word == "久").unwrap();
        assert_eq!(jiu.selection_count, 1); // c=273121 < top 273122
        assert_eq!(jiu2.selection_count, 2); // c=273700 ≥ top
    }

    #[test]
    fn legacy_million_weight_garbage_dropped() {
        let mut engine = PinyinEngine::from_file(TEST_BASE).unwrap();
        let migrated = engine.parse_userdb("zhe bu he shi\t着不合适\tc=1000000\njiu\t就\tc=273122\n");
        assert_eq!(migrated, 1); // 只有"就"被迁移,1M 脏数据直接丢弃
        let z = engine.table.get("zhebuheshi").map(|v| v.iter().any(|e| e.word == "着不合适"));
        assert_ne!(z, Some(true));
    }

    #[test]
    fn legacy_coined_word_gets_count_two() {
        let mut engine = PinyinEngine::from_file(TEST_BASE).unwrap();
        // 不在 base 的造词(曾被选中登顶过) → count=2
        engine.parse_userdb("ce shi\t测释\tc=2445\n");
        let entries = engine.table.get("ceshi").unwrap();
        let ceshi = entries.iter().find(|e| e.word == "测释").unwrap();
        assert_eq!(ceshi.selection_count, 2);
        assert_eq!(ceshi.base_weight, 0);
    }

    #[test]
    fn corrupted_t_field_skipped_not_migrated() {
        let mut engine = PinyinEngine::from_file(TEST_BASE).unwrap();
        // c= 合法但 t= 存在且不可解析 → 损坏行: 跳过且不计迁移,
        // 不得误入 legacy 分支把计数收缩成 1/2
        let migrated = engine.parse_userdb("ceshi\t测释\tc=5 t=abc\n");
        assert_eq!(migrated, 0);
        assert_ne!(engine.table.get("ceshi").map(|v| v.iter().any(|e| e.word == "测释")),
            Some(true), "损坏行不应入库");
    }

    #[test]
    fn bigram_persists_across_restart() {
        let dir = std::env::temp_dir().join(format!("feibai-bigram-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ud = dir.join("user.dict.txt");
        let bg = dir.join("user.bigram.txt");
        let _ = std::fs::remove_file(&ud); let _ = std::fs::remove_file(&bg);

        // 会话 1: 两次 "我…把" 共现 → 落盘
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        engine.set_userdb_path(&ud);
        for _ in 0..2 {
            for c in "wo".chars() { press_key(&mut engine, c); }
            let idx = engine.candidates.iter().position(|c| c.text == "我").unwrap();
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
            for c in "ba".chars() { press_key(&mut engine, c); }
            let idx = engine.candidates.iter().position(|c| c.text == "把").unwrap();
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        }
        assert!(bg.exists(), "commit 后应写出 bigram 文件");
        // 锁定 learn_word 计数递增 (0→1→2) 与周时间戳刷新
        let content = std::fs::read_to_string(&ud).unwrap();
        assert!(content.contains("wo\t我\tc=2 t="), "我 两次选中应 c=2: {content}");
        assert!(content.contains("ba\t把\tc=2 t="), "把 两次选中应 c=2: {content}");
        assert!(!content.contains("woba\t"), "单字 commit 不应学习短语: {content}");
        let week = content
            .lines()
            .find(|l| l.starts_with("wo\t我\tc=2 t="))
            .and_then(|l| l.split_whitespace().find_map(|tok| tok.strip_prefix("t=").and_then(|v| v.parse::<u32>().ok())))
            .expect("wo\t我 行应带 t=周");
        assert_eq!(week, now_week(), "t= 应刷新为当前周");

        // 渐进选择产生短语: "woba" 先选 我 再选 吧。
        // 我吧 不在词典 → 第一次走 learn_phrase 新条目 push, 第二次走 1→2 递增
        for _ in 0..2 {
            for c in "woba".chars() { press_key(&mut engine, c); }
            let idx = engine.candidates.iter().position(|c| c.text == "我").unwrap();
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
            let idx = engine.candidates.iter().position(|c| c.text == "吧").unwrap();
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        }
        let content = std::fs::read_to_string(&ud).unwrap();
        assert!(content.contains("woba\t我吧\tc=2 t="), "渐进两次应学习短语 我吧 c=2: {content}");

        // 会话 2: 重启加载,bigram 上下文延续 + 短语计数保留
        let mut engine2 = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        engine2.load_userdb(&ud).unwrap();
        assert_eq!(engine2.bigram.count("我", "把"), 2, "重启后 bigram 计数应保留");
        let phrase = engine2.table.get("woba").unwrap()
            .iter().find(|e| e.word == "我吧").expect("重启后短语词条应在");
        assert_eq!((phrase.selection_count, phrase.base_weight), (2, 0),
            "短语计数保留,且造词 base_weight 保持 0");
        let _ = std::fs::remove_file(&ud); let _ = std::fs::remove_file(&bg);
    }

    #[test]
    fn test_existing_promotion_unaffected_by_changes() {
        // Regression guard: the existing single-char promotion (又 over 有/由) must
        // still overtake after 2 selections with the new base_weight/tie-break.
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        for _ in 0..2 {
            for c in "you".chars() { press_key(&mut engine, c); }
            let idx = engine.candidates.iter().position(|c| c.text == "又")
                .expect("又 should be a candidate for 'you'");
            assert!(idx < 9, "又 should be in first page, got idx {}", idx);
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        }
        for c in "you".chars() { press_key(&mut engine, c); }
        assert_eq!(engine.candidates[0].text, "又",
            "又 should still rank #1 after 2 selections (promotion unaffected)");
    }

    #[test]
    fn ba_ba_oscillation_is_gone() {
        // 复现日志里的真实场景: 把/吧 都常用(吧 191 次 vs 把 76 次)
        // 旧代码: 每次选中 ±1 翻转,谁在 #1 抛硬币;新机制: 吧 稳定 #1
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        let type_ba = |eng: &mut PinyinEngine| {
            for c in "ba".chars() { press_key(eng, c); }
        };
        // 学习期: 选 1 次把, 3 次吧
        for pick in ["把", "吧", "吧", "吧"] {
            type_ba(&mut engine);
            let idx = engine.candidates.iter().position(|c| c.text == pick).unwrap();
            let d = char::from_digit((idx + 1) as u32, 10).unwrap();
            press_key(&mut engine, d);
        }
        // 验证期: 再选一次 把 (旧代码此处会把把顶到#1)
        type_ba(&mut engine);
        let idx = engine.candidates.iter().position(|c| c.text == "把").unwrap();
        press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        // 吧 的选中次数(4) > 把(2) → 吧 必须 #1, 不因把被选而翻转
        type_ba(&mut engine);
        assert_eq!(engine.candidates[0].text, "吧", "吧 count=4 应稳定 #1");
    }

    #[test]
    fn context_bigram_promotes_expected_next_word() {
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        // 连续两次 "我..把" 共现后, 组词中选中"我"再输入 ba, "把"应被上下文加成
        for _ in 0..2 {
            for c in "wo".chars() { press_key(&mut engine, c); }
            let idx = engine.candidates.iter().position(|c| c.text == "我").unwrap();
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
            for c in "ba".chars() { press_key(&mut engine, c); }
            let idx = engine.candidates.iter().position(|c| c.text == "把").unwrap();
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        }
        // 同一组合词流: 我 → ba, "把"应在 "吧" 之前(即使 吧 计数更高)
        for c in "wo".chars() { press_key(&mut engine, c); }
        let idx = engine.candidates.iter().position(|c| c.text == "我").unwrap();
        press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        for c in "ba".chars() { press_key(&mut engine, c); }
        let ba_idx = engine.candidates.iter().position(|c| c.text == "把").unwrap();
        let ba_zui = engine.candidates.iter().position(|c| c.text == "吧").unwrap();
        assert!(ba_idx < ba_zui, "上下文 我→把 应让 把 排在 吧 前");
    }

    #[test]
    fn userdb_save_writes_new_format_from_memory() {
        let dir = std::env::temp_dir().join(format!("feibai-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ud = dir.join("user.dict.txt");
        let bg = dir.join("user.bigram.txt");
        let _ = std::fs::remove_file(&ud); let _ = std::fs::remove_file(&bg);

        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        engine.set_userdb_path(&ud);
        for c in "ba".chars() { press_key(&mut engine, c); }
        press_key(&mut engine, '1'); // 选第一个候选并 commit → 触发保存
        let content = std::fs::read_to_string(&ud).unwrap();
        assert!(content.contains("\t把\tc=1 t=") || content.contains("\t吧\tc=1 t=") || content.contains("\t拔\tc=1 t="),
            "应写入 c=次数 t=周 新格式: {content}");
        let bigram_content = std::fs::read_to_string(&bg).unwrap();
        assert!(bigram_content.is_empty(), "单字 commit 无前词, bigram 应为空");
        let _ = std::fs::remove_file(&ud); let _ = std::fs::remove_file(&bg);
    }

    #[test]
    fn legacy_backup_created_once_and_idempotent() {
        let dir = std::env::temp_dir().join(format!("feibai-bak-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ud = dir.join("user.dict.txt");
        std::fs::write(&ud, "jiu\t旧\tc=273122\n").unwrap();
        let mut engine = PinyinEngine::from_files(&[TEST_BASE]).unwrap();
        engine.load_userdb(&ud).unwrap();
        let bak = dir.join("user.dict.txt.legacy-bak");
        assert!(bak.exists(), "legacy 行应触发备份");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "jiu\t旧\tc=273122\n");
        // 二次加载不覆盖已有备份
        let mut engine2 = PinyinEngine::from_files(&[TEST_BASE]).unwrap();
        engine2.load_userdb(&ud).unwrap();
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "jiu\t旧\tc=273122\n");
        // 纯新格式不触发备份
        std::fs::write(&ud, "jiu\t旧\tc=3 t=100\n").unwrap();
        std::fs::remove_file(&bak).unwrap();
        let mut engine3 = PinyinEngine::from_files(&[TEST_BASE]).unwrap();
        engine3.load_userdb(&ud).unwrap();
        assert!(!bak.exists(), "新格式行不应触发备份");
        let _ = std::fs::remove_file(&ud); let _ = std::fs::remove_file(&bak);
    }
}
