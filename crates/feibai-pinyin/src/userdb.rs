use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

use feibai_core::log_error;

/// 词条: 静态 base 词频 + 用户选中计数。用户信号是"次数",不是绝对权重——
/// 绝对权重会覆盖并抹平 base 词库的组内排序(band 坍缩,见 2026-09 日志分析)。
#[derive(Debug, Clone)]
pub struct DictEntry {
    pub word: String,
    pub base_weight: u64,
    pub selection_count: u32,
    pub last_week: u32,
}

/// 造词在两次选中前的沉底权重(误学碎片防护,沿用 0.1.1 语义)
pub const NEW_WORD_BASE: u64 = 10;
/// 每周衰减因子(约 4 周半衰,对齐 Google 拼音 4 周掉 80% 的粒度)
pub const WEEKLY_DECAY: f64 = 0.85;
/// 计数每翻一倍在 key_top 之上的加分
pub const COUNT_STEP: u64 = 2;

pub fn now_week() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 604_800) as u32)
        .unwrap_or(0)
}

pub fn key_top(entries: &[DictEntry]) -> u64 {
    entries.iter().map(|e| e.base_weight).max().unwrap_or(0)
}

/// 衰减后的有效计数(打分期计算,存储值永不减少)
fn effective_count(count: u32, last_week: u32, now: u32) -> f64 {
    if count == 0 {
        return 0.0;
    }
    let idle = now.saturating_sub(last_week);
    count as f64 * WEEKLY_DECAY.powi(idle as i32)
}

/// 排序分。规则见计划头部"核心打分设计"。
pub fn entry_score(e: &DictEntry, top: u64, now: u32) -> u64 {
    let eff = effective_count(e.selection_count, e.last_week, now);
    if e.selection_count == 0 || eff < 0.5 {
        return e.base_weight;
    }
    if e.base_weight == 0 && eff < 2.0 {
        return NEW_WORD_BASE;
    }
    let steps = eff.log2().max(0.0);
    top.saturating_sub(1).saturating_add((COUNT_STEP as f64 * steps).round() as u64)
}

/// 确定性排序: 分数降序 → base 降序 → 字数降序 → 字典序降序
pub fn cmp_ranked(a: &DictEntry, b: &DictEntry, top: u64, now: u32) -> Ordering {
    entry_score(b, top, now)
        .cmp(&entry_score(a, top, now))
        .then_with(|| b.base_weight.cmp(&a.base_weight))
        .then_with(|| b.word.chars().count().cmp(&a.word.chars().count()))
        .then_with(|| b.word.cmp(&a.word))
}

/// 用户 bigram: (前词, 当前词) 共现计数, 参与 lookup 排序的上下文加成。
/// 灵感来自 libime HistoryBigram(用户历史 bigram 与 base LM 插值)。
#[derive(Default)]
pub struct UserBigram {
    map: HashMap<(String, String), u32>,
}

impl UserBigram {
    pub fn bump(&mut self, prev: Option<&str>, cur: &str) {
        if let Some(p) = prev {
            if p != cur {
                *self.map.entry((p.to_string(), cur.to_string())).or_insert(0) += 1;
            }
        }
    }

    pub fn count(&self, prev: &str, cur: &str) -> u32 {
        self.map
            .get(&(prev.to_string(), cur.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// 上下文加成,与 entry_score 同量纲: 饱和函数 c/(c+2),
    /// c=1 → top/3 (足以翻转 #1/#2 的计数分差), c→∞ → 趋近 top。
    pub fn boost(&self, prev: &str, cur: &str, top: u64) -> u64 {
        let c = self.count(prev, cur);
        if c == 0 {
            return 0;
        }
        (top as f64 * (c as f64 / (c as f64 + 2.0))).round() as u64
    }

    /// NotFound 视为空(首启无文件),存在但不可读为 Err(防误覆盖已有数据)
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
        };
        let mut bg = Self::default();
        for line in content.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut parts = line.splitn(3, '\t');
            let (Some(p), Some(c), Some(n)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let count = n.strip_prefix("c=").and_then(|v| v.parse().ok()).unwrap_or(0);
            if count > 0 {
                bg.map.insert((p.to_string(), c.to_string()), count);
            }
        }
        Ok(bg)
    }

    pub fn save(&self, path: &Path) {
        let mut lines: Vec<String> = self
            .map
            .iter()
            .map(|((p, c), n)| format!("{}\t{}\tc={}", p, c, n))
            .collect();
        lines.sort(); // HashMap 迭代序不定,排序保证文件 diff 稳定
        // 空表写 0 字节文件(而非孤零零一个换行),调用方据此识别"无记录"
        let content = if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" };
        let tmp = path.with_extension("txt.tmp");
        if let Err(e) = std::fs::write(&tmp, content) {
            log_error!("failed to write {}: {e}", tmp.display());
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            log_error!("failed to rename {} -> {}: {e}", tmp.display(), path.display());
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(word: &str, base: u64, count: u32, week: u32) -> DictEntry {
        DictEntry { word: word.into(), base_weight: base, selection_count: count, last_week: week }
    }

    #[test]
    fn unselected_uses_base_weight() {
        // 旧: 6655, 组内 top=就 273122 —— 从未选过必须保持 base 排序
        assert_eq!(entry_score(&e("旧", 6655, 0, 0), 273122, 100), 6655);
    }

    #[test]
    fn first_selection_ranks_just_below_top() {
        // 选中 1 次 → key_top-1 (第2位), 对齐旧行为 "1st select: top-1"
        assert_eq!(entry_score(&e("旧", 6655, 1, 100), 273122, 100), 273121);
    }

    #[test]
    fn second_selection_overtakes_top() {
        // 选中 2 次 → 登顶 (保留 "又 反超 有" 的两次选中反超特性)
        assert!(entry_score(&e("旧", 6655, 2, 100), 273122, 100) > 273122);
    }

    #[test]
    fn count_ordering_is_stable_no_flip() {
        // 核心 bug 回归: 吧(191次) vs 把(76次), 把再选一次也必须仍低于 吧
        let top = 108066;
        let now = 100;
        let ba = e("吧", 25526, 191, now);
        let baa = e("把", 108066, 76, now);
        assert!(entry_score(&ba, top, now) > entry_score(&baa, top, now));
        let baa_again = e("把", 108066, 77, now);
        assert!(entry_score(&ba, top, now) > entry_score(&baa_again, top, now));
    }

    #[test]
    fn coined_word_sinks_until_second_selection() {
        assert_eq!(entry_score(&e("测释", 0, 1, 100), 2444, 100), NEW_WORD_BASE);
        assert!(entry_score(&e("测释", 0, 2, 100), 2444, 100) > 2444);
    }

    #[test]
    fn decay_pulls_stale_selections_back_to_base() {
        // 同样 count=8: 20 周未用的掉回 base, 常用的留在顶部
        let top = 108066;
        let fresh = e("吧", 25526, 8, 100);
        let stale = e("把", 108066, 8, 80); // now=100, 20 周前
        assert!(entry_score(&fresh, top, 100) > entry_score(&stale, top, 100));
    }

    #[test]
    fn tiebreak_is_deterministic() {
        let top = 108066;
        let now = 100;
        let a = e("甲", 100, 0, 0);
        let b = e("乙", 100, 0, 0);
        // 同分不同词: 排序必须确定且反对称(不要求 Equal——字不同则字典序分高下)
        assert_eq!(cmp_ranked(&a, &b, top, now), cmp_ranked(&a, &b, top, now));
        assert_eq!(cmp_ranked(&a, &b, top, now), cmp_ranked(&b, &a, top, now).reverse());
    }

    #[test]
    fn bigram_bump_and_count() {
        let mut bg = UserBigram::default();
        bg.bump(Some("我"), "把");
        bg.bump(Some("我"), "把");
        bg.bump(Some("我"), "吧");
        bg.bump(None, "吧"); // 无上文不记
        assert_eq!(bg.count("我", "把"), 2);
        assert_eq!(bg.count("我", "吧"), 1);
        assert_eq!(bg.count("我", "在"), 0);
    }

    #[test]
    fn bigram_self_pair_skipped() {
        let mut bg = UserBigram::default();
        bg.bump(Some("把"), "把");
        assert_eq!(bg.count("把", "把"), 0);
    }

    #[test]
    fn bigram_boost_saturates() {
        let mut bg = UserBigram::default();
        assert_eq!(bg.boost("我", "把", 108066), 0); // 无共现无加成
        bg.bump(Some("我"), "把");
        let b1 = bg.boost("我", "把", 108066);
        // c=1 → 恰为 top/3 (108066/3 整除), 断言"不越 top/3"须用 <=
        assert!(b1 > 0 && b1 <= 108066 / 3);
        for _ in 0..20 { bg.bump(Some("我"), "把"); }
        let b21 = bg.boost("我", "把", 108066);
        assert!(b21 > 108066 * 3 / 4 && b21 <= 108066); // 饱和但不越 key_top
    }

    #[test]
    fn bigram_save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("feibai-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("user.bigram.txt");
        let _ = std::fs::remove_file(&path);
        let mut bg = UserBigram::default();
        bg.bump(Some("我"), "把");
        bg.bump(Some("我"), "把");
        bg.bump(Some("好"), "吧");
        bg.save(&path);
        let loaded = UserBigram::load(&path).unwrap();
        assert_eq!(loaded.count("我", "把"), 2);
        assert_eq!(loaded.count("好", "吧"), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bigram_load_missing_file_is_empty() {
        let bg = UserBigram::load(std::path::Path::new("/nonexistent/feibai/x.txt")).unwrap();
        assert_eq!(bg.count("我", "把"), 0);
    }
}
