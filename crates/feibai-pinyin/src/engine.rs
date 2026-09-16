use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use feibai_core::*;

use crate::userdb::{DictEntry, UserBigram, cmp_ranked, entry_score, key_top, now_week};

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
    /// userdb 是否成功加载过。加载失败后内存态近乎为空,
    /// 落盘会整写覆盖真实文件 → 失败即禁写,直至进程重启
    userdb_persist_ok: bool,
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
            userdb_persist_ok: true,
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
        // 任何加载失败(含 bigram)都必须禁写: 否则近乎空的内存态
        // 会在下次 save 整写覆盖真实文件
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                self.userdb_persist_ok = false;
                return Err(format!("failed to read userdb {}: {e}", path.display()));
            }
        };
        let (migrated, dropped) = self.parse_userdb(&content);
        if migrated > 0 || dropped > 0 {
            // 首次迁移或破坏性丢弃前备份原文件(幂等: 已有备份则不覆盖)
            let bak = path.with_extension("txt.legacy-bak");
            if !bak.exists() {
                if let Err(e) = fs::copy(path, &bak) {
                    // 备份失败不必中断加载,但不可静默吞掉
                    log_error!("failed to backup userdb {} -> {}: {e}", path.display(), bak.display());
                }
            }
            if migrated > 0 {
                log_info!("userdb: migrated {migrated} legacy entries");
            }
            if dropped > 0 {
                log_info!("userdb: dropped {dropped} garbage/corrupted lines (backed up)");
            }
        }
        // bigram 与 unigram 同目录共存,重启后上下文延续
        let bigram_path = path.with_file_name("user.bigram.txt");
        match UserBigram::load(&bigram_path) {
            Ok(b) => self.bigram = b,
            Err(e) => {
                self.userdb_persist_ok = false;
                return Err(format!("failed to load user bigram: {e}"));
            }
        }
        Ok(())
    }

    /// 解析用户词典。返回 (迁移的旧格式行数, 破坏性丢弃的行数) ——
    /// 任一 > 0 都触发加载前备份(纯垃圾/纯损坏文件同样有备份再丢弃)。
    /// dropped 只计 c≥1M 垃圾行与损坏 t= 跳过行;纯新格式缺 c= 的静默跳过不计。
    /// 新格式: `拼音\t词\tc=次数 t=周`(拼音为连写形式,与 table key 一致)
    /// 旧格式: `拼音 空格分隔\t词\tc=绝对权重`(无 t= 字段,按 band 位置换算次数)
    fn parse_userdb(&mut self, content: &str) -> (usize, usize) {
        let now = now_week();
        let mut migrated = 0;
        let mut dropped = 0;
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
                dropped += 1;
                continue;
            }
            let key: String = pinyin_raw.split_whitespace().collect();

            let entries = self.table.entry(key).or_default();
            let (count, last_week) = match week {
                Some(w) => (c, w),
                None => {
                    // 旧格式绝对权重迁移(被丢弃的脏数据行不计入迁移数):
                    if c >= 1_000_000 {
                        entries.retain(|e| e.word != word); // 0.1.1 前误学脏数据,直接丢
                        dropped += 1;
                        continue;
                    }
                    migrated += 1;
                    // 统一按 band 位置换算: c ≥ top → 曾登顶 → 2; 否则 1。
                    // 不看 in_base —— 0.1.1/0.1.2 只选过一次的造词写的就是
                    // c=10(NEW_WORD_WEIGHT),若因不在 base 而给 2,
                    // 迁移后首周直接 #1 且一次再选就回 #1
                    let top = key_top(entries);
                    let migrated_count = if u64::from(c) < top { 1 } else { 2 };
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
        (migrated, dropped)
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

        // Exact match candidates for complete syllable spans.
        // 排序键 = 计数 score + 上下文 bigram 加成; 先捕获 bigram/ctx 的借用,
        // 排序辅助 rank_key 为自由函数, 避免与 table 迭代的整体 &self 借用冲突
        let now = now_week();
        let ctx: Option<String> = self.last_context.clone();
        let bigram = &self.bigram;
        let exact_end = if last_is_incomplete { n - 1 } else { n };
        for end in (1..=exact_end.min(8)).rev() {
            let key: String = remaining[..end].concat();
            if let Some(entries) = self.table.get(&key) {
                let top = key_top(entries);
                let mut ranked: Vec<&DictEntry> = entries.iter().collect();
                ranked.sort_by(|a, b| {
                    rank_key(bigram, b, top, now, ctx.as_deref())
                        .cmp(&rank_key(bigram, a, top, now, ctx.as_deref()))
                        .then_with(|| cmp_ranked(a, b, top, now))
                });
                for e in ranked {
                    if seen.insert(e.word.clone()) {
                        self.candidates.push(Candidate {
                            text: e.word.clone(),
                            comment: None,
                        });
                    }
                }
            }
        }

        // Prefix matching for trailing incomplete syllable (using BTreeMap range).
        // 条目来自多个 key, 各自带所属 key 的 top 参与排序。
        // 前缀范围可达数万条(z 前缀 ~2.8 万), rank_key 含 bigram 查表,
        // 排序闭包内逐次计算会放大到 O(n log n) 次 —— 先算好分再排序, 顺序不变
        if last_is_incomplete {
            let prefix: String = remaining.concat();
            let prefix_end = prefix_upper_bound(&prefix);
            let mut prefix_entries: Vec<(u64, &DictEntry, u64)> = self.table
                .range(prefix.clone()..prefix_end)
                .filter(|(k, _)| k.as_str() != prefix)
                .flat_map(|(_, entries)| {
                    let top = key_top(entries);
                    entries.iter().map(move |e| (e, top))
                })
                .map(|(e, top)| (rank_key(bigram, e, top, now, ctx.as_deref()), e, top))
                .collect();
            prefix_entries.sort_by(|(ra, a, ta), (rb, b, _tb)| {
                rb.cmp(ra).then_with(|| cmp_ranked(a, b, *ta, now))
            });
            for (_, e, _) in prefix_entries.iter().take(50) {
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
                let mut single_prefix_entries: Vec<(u64, &DictEntry, u64)> = self.table
                    .range(last_seg.clone()..seg_end)
                    .filter(|(k, _)| k.len() <= 6)
                    .flat_map(|(_, entries)| {
                        let top = key_top(entries);
                        entries.iter().map(move |e| (e, top))
                    })
                    .map(|(e, top)| (rank_key(bigram, e, top, now, ctx.as_deref()), e, top))
                    .collect();
                single_prefix_entries.sort_by(|(ra, a, ta), (rb, b, _tb)| {
                    rb.cmp(ra).then_with(|| cmp_ranked(a, b, *ta, now))
                });
                for (_, e, _) in single_prefix_entries.iter().take(50) {
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
        let now = now_week();

        for i in 0..n {
            if dp_score[i] == f64::NEG_INFINITY {
                continue;
            }
            let max_len = (n - i).min(8);
            for len in 1..=max_len {
                let key: String = segs[i..i + len].concat();
                if let Some(entries) = self.table.get(&key) {
                    // Task 3 起表内是插入序, first() 不再是最优 —— 按 cmp_ranked 取最优;
                    // DP 词分仍用 base_weight(排序位次与词分解耦)
                    let top = key_top(entries);
                    if let Some(best) = entries.iter().max_by(|a, b| cmp_ranked(a, b, top, now)) {
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
    /// 多字词含无效音节不学(单字不受限: 前缀选单字如 在@z 仍可学;
    /// 合法全拼多字词仍可学)
    fn learnable(&self, word: &str, segs: &[String]) -> bool {
        if word.is_ascii() {
            return false;
        }
        let char_count = word.chars().count();
        if char_count > 1 && segs.len() < char_count {
            return false;
        }
        if char_count > 1 && segs.iter().any(|s| !self.syllables.contains(s.as_str())) {
            return false;
        }
        true
    }

    /// 选中即计数: 次数+1, 刷新周时间戳。base 词库信息永不覆盖。
    fn learn_word(&mut self, key: &str, word: &str, segs: &[String]) {
        if !self.learnable(word, segs) {
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
        if !self.learnable(sentence, segs) {
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
        if !self.userdb_persist_ok {
            log_error!("userdb 未成功加载,跳过落盘以防覆盖");
            return;
        }
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

        // 学习: 选中即计数 + 上下文 bigram(前词=上次选中/上屏词)。
        // 仅对可学词记录: ASCII 原样上屏既不记共现也不更新上下文
        // (上下文语义 = 最近一个可学中文词)
        let word_key: String = remaining[..seg_count].concat();
        let word_segs: Vec<String> = remaining[..seg_count].to_vec();
        if self.learnable(&text, &word_segs) {
            self.bigram.bump(self.last_context.as_deref(), &text);
            self.last_context = Some(text.clone());
            self.learn_word(&word_key, &text, &word_segs);
        }

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

/// 排序键: score + 上下文 bigram 加成(与 score 同量纲)。
/// 自由函数而非 &self 方法 —— lookup 内迭代 self.table 的同时借用 self.bigram,
/// 挂在 &self 上会整体借用 self 产生借用冲突
fn rank_key(
    bigram: &UserBigram,
    e: &DictEntry,
    top: u64,
    now: u32,
    ctx: Option<&str>,
) -> u64 {
    let base = entry_score(e, top, now);
    match ctx {
        Some(prev) => base.saturating_add(bigram.boost(prev, &e.word, top)),
        None => base,
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

    #[test]
    fn test_mislearn_fragment_gets_low_weight() {
        // guard 半边: 含真正非法音节("nn")的碎片, learn_phrase 路径直接拒绝
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        let bad_segs: Vec<String> = vec!["yo".into(), "nn".into()]; // "nn" 非法音节
        engine.learn_phrase("哟嗯", &bad_segs);
        let has = engine.table.get(&bad_segs.concat())
            .is_some_and(|v| v.iter().any(|x| x.word == "哟嗯"));
        assert!(!has, "含无效音节的碎片必须被 guard 拒绝");
        // count=1 沉底半边: ["yo","n"] 两音节均合法 → 被存储、count=1、
        // NEW_WORD_BASE 沉底(该测试的经典场景: 音节合法的碎片沉底而非霸榜)
        let segs: Vec<String> = vec!["yo".into(), "n".into()];
        let key: String = segs.concat();
        engine.learn_word(&key, "哟嗯", &segs);
        // count=1 的造词 score = NEW_WORD_BASE, 排在所有 base 词之后
        let entries = engine.table.get(&key).unwrap();
        let top = crate::userdb::key_top(entries);
        let e = entries.iter().find(|x| x.word == "哟嗯").unwrap();
        assert_eq!(crate::userdb::entry_score(e, top, e.last_week), crate::userdb::NEW_WORD_BASE);
    }

    #[test]
    fn invalid_syllable_prefix_selection_not_learned() {
        // final review C1 回归: 输入 wob (切分 [wo,b], b 非完整音节),
        // 前缀面板出现 wob* key 的多字词(如 我把@woba); 一次性选中后
        // find_seg_count_for_word 前缀回退返回 remaining.len()=2,
        // 旧 learnable 放行 → 在垃圾键 "wob" 下建词条,永久落盘。
        // 恢复的 guard 必须拒绝: 多字词含无效音节不学。
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        for c in "wob".chars() { press_key(&mut engine, c); }
        // 前缀面板含多字词(单字 exact 匹配在前,多字词在前 50 内但不一定第一页)。
        // 必须选 2 字词(如 我把@woba): 字数==音节数,只有无效音节 guard 能拒绝;
        // 3+ 字词会被既有的 segs.len()<字数 分支拒绝,测不到本 guard
        let idx = engine.candidates.iter().position(|c| {
            !c.text.is_ascii() && c.text.chars().count() == 2
        }).expect("wob 前缀面板应含 2 字前缀候选 (如 我把@woba)");
        let text = engine.candidates[idx].text.clone();
        let actions = engine.select_candidate(idx);
        assert!(
            actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == &text)),
            "前缀多字词选中应消耗全部音节并上屏: {text}"
        );
        assert!(
            engine.table.get("wob").is_none_or(|v| v.is_empty()),
            "无效音节垃圾键 wob 不得学习词条: {:?}",
            engine.table.get("wob").map(|v| v.iter().map(|e| e.word.as_str()).collect::<Vec<_>>())
        );
    }


    #[test]
    fn test_new_real_word_promotes_via_selections() {
        // 真实新词(测释,不在词典): count=1 沉底 → count=2 反超 key_top → count=8 更高。
        // 计数制下不再有绝对权重(旧断言 10/2443/2445),改按 score 相对位置断言。
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        let segs: Vec<String> = vec!["ce".into(), "shi".into()];
        let key: String = segs.concat();
        // "测释" (2 chars, 2 syllables) 不在词典 — 代表任意用户造词。前置确认。
        assert!(engine.table.get(&key).is_some_and(|e| !e.iter().any(|x| x.word == "测释")),
            "precondition: word must not exist in dict");
        let score_of = |eng: &PinyinEngine| -> u64 {
            let entries = eng.table.get(&key).unwrap();
            let top = key_top(entries);
            let e = entries.iter().find(|x| x.word == "测释").unwrap();
            entry_score(e, top, e.last_week)
        };

        // 行为检查辅助: 输入 ceshi → 断言 → Escape 清空,避免 preedit 残留
        let escape = |eng: &mut PinyinEngine| {
            eng.process_key(&KeyEvent {
                keysym: 0xff1b,
                unicode: None,
                modifiers: Modifiers::default(),
                state: KeyState::Press,
            });
        };
        let type_ceshi = |eng: &mut PinyinEngine| {
            for c in "ceshi".chars() { press_key(eng, c); }
        };

        // 1st select: count=1 沉底 NEW_WORD_BASE, 低于 key_top(测试=2444)
        engine.learn_word(&key, "测释", &segs);
        let top = key_top(engine.table.get(&key).unwrap());
        assert_eq!(score_of(&engine), crate::userdb::NEW_WORD_BASE,
            "count=1 造词必须沉底 NEW_WORD_BASE");
        type_ceshi(&mut engine);
        let pos = |eng: &PinyinEngine, w: &str| {
            eng.candidates.iter().position(|c| c.text == w).unwrap()
        };
        assert!(pos(&engine, "测试") < pos(&engine, "测释"),
            "count=1 时测释必须排在测试之后");
        escape(&mut engine);

        // 2nd select: count=2 → score = top-1+2 反超登顶
        engine.learn_word(&key, "测释", &segs);
        assert!(score_of(&engine) > top, "count=2 须反超 key_top(={top})");
        type_ceshi(&mut engine);
        assert_eq!(engine.candidates[0].text, "测释", "count=2 后测释应登顶");
        escape(&mut engine);

        // count=8: 步进 2*log2(count) 更高,仍居首位
        for _ in 0..6 { engine.learn_word(&key, "测释", &segs); }
        assert!(score_of(&engine) > top + 2, "count=8 分数须高于 count=2 的 top+2");
        type_ceshi(&mut engine);
        assert_eq!(engine.candidates[0].text, "测释", "count=8 测释保持首位");
    }

    #[test]
    fn test_same_weight_tiebreak_stable() {
        // 同 key 条目排序必须确定且可重复。cmp_ranked 决胜链:
        // 分数降序 → base_weight 降序 → 字数降序 → 字典序降序。
        let mut engine = PinyinEngine::new_empty();
        let key = "xx".to_string();
        let mk = |word: &str, base: u64, count: u32| DictEntry {
            word: word.to_string(),
            base_weight: base,
            selection_count: count,
            last_week: 0,
        };
        // top=100。用户词 count=1 → score top-1=99, 与 base 99 的词同分,
        // 靠决胜链第二级(base_weight 降序)排在前面。
        engine.table.insert(key.clone(), vec![
            mk("乙", 100, 0),      // 与甲同分同 base 同字数 → 字典序降序(甲 > 乙)
            mk("甲", 100, 0),
            mk("用户词", 100, 1),  // score 99; base 100 > 同分词的 99 → 胜出
            mk("同分", 99, 0),     // score 99
            mk("词词", 50, 0),     // 与丁同分同 base → 字数降序(2字 > 1字)
            mk("丁", 50, 0),
        ]);
        let sort_once = |eng: &mut PinyinEngine| -> Vec<String> {
            let entries = eng.table.get_mut(&key).unwrap();
            let top = key_top(entries);
            entries.sort_by(|a, b| cmp_ranked(a, b, top, 0));
            entries.iter().map(|e| e.word.clone()).collect()
        };
        let order1 = sort_once(&mut engine);
        let order2 = sort_once(&mut engine);
        assert_eq!(order1, order2, "sort must be stable across repeated calls");
        assert_eq!(order1, vec!["甲", "乙", "用户词", "同分", "词词", "丁"],
            "tie-break chain: score → base_weight → chars → lexicographic, got {order1:?}");
    }

    #[test]
    fn userdb_new_format_parses_count_and_week() {
        let mut engine = PinyinEngine::from_file(TEST_BASE).unwrap();
        let (migrated, dropped) = engine.parse_userdb("jiu\t旧\tc=5 t=100\n");
        assert_eq!((migrated, dropped), (0, 0));
        let entries = engine.table.get("jiu").unwrap();
        let jiu = entries.iter().find(|e| e.word == "旧").unwrap();
        assert_eq!((jiu.selection_count, jiu.last_week), (5, 100));
    }

    #[test]
    fn legacy_absolute_weight_migrates_by_band_position() {
        let mut engine = PinyinEngine::from_file(TEST_BASE).unwrap();
        // 旧格式无 t= 字段。就=273122 为组内 top。
        // c ≥ key_top → 用户曾登顶 → count=2; 否则 count=1
        let (migrated, dropped) = engine.parse_userdb("jiu\t旧\tc=273121\njiu\t久\tc=273700\n");
        assert_eq!((migrated, dropped), (2, 0));
        let entries = engine.table.get("jiu").unwrap();
        let jiu = entries.iter().find(|e| e.word == "旧").unwrap();
        let jiu2 = entries.iter().find(|e| e.word == "久").unwrap();
        assert_eq!(jiu.selection_count, 1); // c=273121 < top 273122
        assert_eq!(jiu2.selection_count, 2); // c=273700 ≥ top
    }

    #[test]
    fn legacy_million_weight_garbage_dropped() {
        let mut engine = PinyinEngine::from_file(TEST_BASE).unwrap();
        let (migrated, dropped) =
            engine.parse_userdb("zhe bu he shi\t着不合适\tc=1000000\njiu\t就\tc=273122\n");
        assert_eq!((migrated, dropped), (1, 1)); // 只有"就"被迁移,1M 脏数据计入 dropped
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
    fn legacy_once_selected_coined_word_migrates_to_one() {
        // final review C2 回归: 0.1.1/0.1.2 对只选过一次的新词写的是 c=10
        // (NEW_WORD_WEIGHT)。旧规则 `in_base && c<top ? 1 : 2` 对不在 base 的
        // 造词恒给 2 → 迁移后首周直接 #1 且一次再选就回 #1。
        // 修正后统一按 band 位置: c=10 < top(测试=2444) → count=1 沉底。
        let mut engine = PinyinEngine::from_file(TEST_BASE).unwrap();
        let (migrated, dropped) = engine.parse_userdb("ce shi\t测释\tc=10\n");
        assert_eq!((migrated, dropped), (1, 0));
        let entries = engine.table.get("ceshi").unwrap();
        let ceshi = entries.iter().find(|e| e.word == "测释").unwrap();
        assert_eq!(ceshi.selection_count, 1, "只选过一次(c=10)的造词必须迁移为 count=1");
        // count=1 沉底: 输入 ceshi 时排在 base top(测试)之下
        for c in "ceshi".chars() { press_key(&mut engine, c); }
        let pos_ceshi = engine.candidates.iter().position(|c| c.text == "测释")
            .expect("迁移后的测释应出现在候选中");
        let pos_test = engine.candidates.iter().position(|c| c.text == "测试")
            .expect("测试应出现在候选中");
        assert!(pos_test < pos_ceshi, "count=1 迁移造词必须排在 base top(测试)之下");
    }

    #[test]
    fn corrupted_t_field_skipped_not_migrated() {
        let mut engine = PinyinEngine::from_file(TEST_BASE).unwrap();
        // c= 合法但 t= 存在且不可解析 → 损坏行: 跳过且不计迁移(计入 dropped),
        // 不得误入 legacy 分支把计数收缩成 1/2
        let (migrated, dropped) = engine.parse_userdb("ceshi\t测释\tc=5 t=abc\n");
        assert_eq!((migrated, dropped), (0, 1));
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
        // 吧 的选中次数(3) > 把(2) → 吧 必须 #1, 不因把被选而翻转
        type_ba(&mut engine);
        assert_eq!(engine.candidates[0].text, "吧", "吧 count=3 应稳定 #1");
    }

    #[test]
    fn context_bigram_promotes_expected_next_word() {
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        // 预学 吧 到 c=4 (score=top+4), 高于 把 学到 c=2 的 score(top+1):
        // 无 bigram 加成时 吧 必然在前 —— 把 的登顶只能来自 上下文加成
        for _ in 0..4 {
            for c in "ba".chars() { press_key(&mut engine, c); }
            let idx = engine.candidates.iter().position(|c| c.text == "吧").unwrap();
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        }
        // 连续两次 "我..把" 共现 → bigram (我,把)=2
        for _ in 0..2 {
            for c in "wo".chars() { press_key(&mut engine, c); }
            let idx = engine.candidates.iter().position(|c| c.text == "我").unwrap();
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
            for c in "ba".chars() { press_key(&mut engine, c); }
            let idx = engine.candidates.iter().position(|c| c.text == "把").unwrap();
            press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        }
        // 同一组合词流: 我 → ba, "把"应靠 bigram 加成压过 count 更高的 吧
        for c in "wo".chars() { press_key(&mut engine, c); }
        let idx = engine.candidates.iter().position(|c| c.text == "我").unwrap();
        press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        for c in "ba".chars() { press_key(&mut engine, c); }
        assert_eq!(engine.candidates[0].text, "把",
            "上下文 我→把 应让 把 登顶(压过 count=4 的 吧): {:?}",
            engine.candidates.iter().take(3).map(|c| c.text.as_str()).collect::<Vec<_>>());
        let ba_idx = engine.candidates.iter().position(|c| c.text == "把").unwrap();
        let ba_zui = engine.candidates.iter().position(|c| c.text == "吧").unwrap();
        assert!(ba_idx < ba_zui, "上下文 我→把 应让 把 排在 吧 前");
    }

    #[test]
    fn ascii_raw_selection_records_no_context() {
        // ASCII 原样上屏不可学: 既不记 bigram 共现,也不更新 last_context
        // (上下文语义 = 最近一个可学中文词,不能被 ASCII 打断)
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        // 建立非空上下文: 选中 我
        for c in "wo".chars() { press_key(&mut engine, c); }
        let idx = engine.candidates.iter().position(|c| c.text == "我").unwrap();
        press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        assert_eq!(engine.last_context.as_deref(), Some("我"));

        // 输入无中文匹配的 ASCII 串 → 首候选为原样 "abc",选中它上屏
        for c in "abc".chars() { press_key(&mut engine, c); }
        assert_eq!(engine.candidates[0].text, "abc", "首位应为原样 ASCII 候选");
        let actions = press_key(&mut engine, '1');
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == "abc")));

        // 不记 (我,abc) 共现; 上下文保持 我 不变
        assert_eq!(engine.bigram.count("我", "abc"), 0,
            "ASCII 原样上屏不得记入 bigram");
        assert_eq!(engine.last_context.as_deref(), Some("我"),
            "ASCII 原样上屏后上下文应保持不变");
    }

    #[test]
    fn abbreviated_multichar_selection_records_no_context() {
        // 简拼选择不可学 (learnable 的另一条 false 分支): 非 ASCII 多字词但音节数 < 字数。
        // 与 ASCII 原样上屏同为上下文盲区 —— 既不记 bigram 共现,也不更新 last_context
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        // 建立非空上下文: 选中 我
        for c in "wo".chars() { press_key(&mut engine, c); }
        let idx = engine.candidates.iter().position(|c| c.text == "我").unwrap();
        press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        assert_eq!(engine.last_context.as_deref(), Some("我"));

        // 简拼输入 "zh" (1 个音节段) → 前缀面板含多字词 (中国/这个/…);
        // 选中一个 2+ 字词: 字数 > 音节数 → learnable 为 false
        for c in "zh".chars() { press_key(&mut engine, c); }
        let idx = engine.candidates.iter().position(|c| {
            !c.text.is_ascii() && c.text.chars().count() > 1
        }).expect("zh 前缀面板应含多字词 (如 中国)");
        assert!(idx < 9, "多字词应在第一页, got idx {idx}");
        let text = engine.candidates[idx].text.clone();
        let actions = press_key(&mut engine, char::from_digit((idx + 1) as u32, 10).unwrap());
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == &text)));

        // 不记 (我, 该词) 共现; 上下文保持 我 不变
        assert_eq!(engine.bigram.count("我", &text), 0,
            "简拼选择多字词不得记入 bigram");
        assert_eq!(engine.last_context.as_deref(), Some("我"),
            "简拼选择多字词后上下文应保持不变");
    }

    #[test]
    fn single_letter_prefix_not_dominated_by_learned_fragment() {
        // 迁移日志中的真实案例: 造词"着不合适"曾被学到 1M 权重,污染每个 z 前缀面板 4920 次
        // 新机制: 造词 count<2 沉底;即便 count=2,也只在其自身 key("zhebuheshi"全拼)内登顶,
        // 对 "z" 前缀面板的贡献仍按 score 排序,长造词不再霸占首位。
        // 这里测可复现的最小行为: 未选稳的造词(count=1)不会出现在任何前缀面板前部
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        let this_week = crate::userdb::now_week();
        engine.parse_userdb(&format!("zhe bu he shi\t着不合适\tc=1 t={this_week}\n")); // count=1 本周,走沉底路径而非衰减路径
        for c in "z".chars() { press_key(&mut engine, c); }
        // 首候选必须是正常单字(在/再/这/做…),不能是"着不合适"。
        // (brief 原断言 assert_ne!(chars().count(), 5) 是笔误: 造词是 4 字,
        //  即使它霸占首位该断言也会通过 —— 改为直接点名造词)
        assert_ne!(engine.candidates[0].text, "着不合适",
            "count=1 的造词不得占据 z 前缀首位: {:?}", engine.candidates[0]);
        let first_page: Vec<&str> =
            engine.candidates.iter().take(9).map(|c| c.text.as_str()).collect();
        assert!(!first_page.contains(&"着不合适"),
            "count=1 的造词不得出现在 z 前缀第一页: {first_page:?}");
    }

    #[test]
    fn prefix_panel_ranks_by_learned_score() {
        // 前缀路径(仅输入 z)同样按 score 排序: 本周 c=8 的 再
        // score = key_top(zai)-1 + 2*log2(8) = 727920, 须越过裸 weight 更高的 在(727915)登顶。
        // 旧实现按裸 base_weight 排序, 在 恒为 #1 —— 锁定 score 排序行为。
        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        let this_week = crate::userdb::now_week();
        engine.parse_userdb(&format!("zai\t再\tc=8 t={this_week}\n"));
        for c in "z".chars() { press_key(&mut engine, c); }
        assert_eq!(engine.candidates[0].text, "再",
            "z 前缀面板应按 score 排序, 再(c=8) 须登顶: {:?}",
            engine.candidates.iter().take(3).map(|c| c.text.as_str()).collect::<Vec<_>>());
        assert_eq!(engine.candidates[1].text, "在",
            "在 是裸 weight 最高的 z 前缀词, 应紧随其后");
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

    #[test]
    fn failed_load_disables_save_to_protect_real_file() {
        // final review I3 回归: load_userdb Err 后(main.rs 只 log 继续),
        // 近乎空的内存态会在下一次 commit 的 save_userdb 整写覆盖真实文件。
        // 门闩: 加载失败 → userdb_persist_ok=false → save 全跳过(含 bigram)。
        let dir = std::env::temp_dir().join(format!("feibai-noload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ud = dir.join("user.dict.txt");
        let bg = dir.join("user.bigram.txt");
        let tmp = dir.join("user.dict.txt.tmp");
        // 把 user.dict.txt 路径建成目录 → read_to_string 失败 → load_userdb Err
        std::fs::create_dir_all(&ud).unwrap();

        let mut engine = PinyinEngine::from_files(&[TEST_BASE, TEST_EXTRA]).unwrap();
        engine.set_userdb_path(&ud);
        assert!(engine.load_userdb(&ud).is_err(), "路径为目录时加载必须失败");

        // 失败后走一次选中提交流程 → save_userdb 必须被跳过
        for c in "ba".chars() { press_key(&mut engine, c); }
        press_key(&mut engine, '1');
        assert!(!bg.exists(), "加载失败后不得写出 bigram 文件(防覆盖)");
        assert!(!tmp.exists(), "加载失败后不得写出 tmp 文件(防覆盖)");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_garbage_or_corrupt_file_backed_up_before_drop() {
        // final review I4 回归: 纯垃圾(c≥1M)/纯损坏(t=不可解析)文件整文件被丢弃,
        // 下次 save 直接清空 —— 破坏性丢弃前必须先备份(migrated=0 也要备份)。
        let root = std::env::temp_dir().join(format!("feibai-garbage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // 场景1: 仅含 1M 垃圾行 → 备份生成 + 词条不入库
        let dir1 = root.join("garbage");
        std::fs::create_dir_all(&dir1).unwrap();
        let ud1 = dir1.join("user.dict.txt");
        let line = "zhe bu he shi\t着不合适\tc=2000000\n";
        std::fs::write(&ud1, line).unwrap();
        let mut engine = PinyinEngine::from_files(&[TEST_BASE]).unwrap();
        engine.load_userdb(&ud1).unwrap();
        let bak1 = dir1.join("user.dict.txt.legacy-bak");
        assert!(bak1.exists(), "纯垃圾文件被丢弃前应生成备份");
        assert_eq!(std::fs::read_to_string(&bak1).unwrap(), line);
        assert_ne!(
            engine.table.get("zhebuheshi").map(|v| v.iter().any(|e| e.word == "着不合适")),
            Some(true),
            "垃圾行不得入库"
        );

        // 场景2: 仅含损坏 t= 行 → 同为破坏性丢弃,同样备份
        let dir2 = root.join("corrupt");
        std::fs::create_dir_all(&dir2).unwrap();
        let ud2 = dir2.join("user.dict.txt");
        std::fs::write(&ud2, "ceshi\t测释\tc=5 t=abc\n").unwrap();
        let mut engine2 = PinyinEngine::from_files(&[TEST_BASE]).unwrap();
        engine2.load_userdb(&ud2).unwrap();
        let bak2 = dir2.join("user.dict.txt.legacy-bak");
        assert!(bak2.exists(), "纯损坏文件被丢弃前应生成备份");
        assert_ne!(
            engine2.table.get("ceshi").map(|v| v.iter().any(|e| e.word == "测释")),
            Some(true),
            "损坏行不得入库"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
