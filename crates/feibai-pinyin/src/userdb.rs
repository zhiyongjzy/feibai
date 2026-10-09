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
/// 上下文(bigram)加成的上限(分)。组内计数分跨度约 0~40
/// (count 1→1e6 经 2·log2 压缩), 上限 12 表示上下文最多能翻约 8 倍用量差。
pub const CTX_BOOST_MAX: u64 = 12;
/// 上下文加成的回退系数: 共现次数 c 的权重为 c/(c+K), 低计数不可靠故衰减。
pub const CTX_BOOST_K: f64 = 2.0;
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

/// 静态 tie-break: 不依赖 key top 的确定性决胜链(base 降序 → 字数降序 → 字典序降序)。
/// 跨 key 混排(前缀面板)必须用它做 rank 平分后的决胜 —— cmp_ranked 的分数层
/// 依赖单侧 key 的 top, 对同一对元素从两侧比较结论可相反, 违反全序,
/// Rust sort 检测到不一致比较器会 panic(生产 2026-09-17 实锤 39 次:
/// panic 吞掉该次按键的 dbus 回复, 超时窗口内字母直通漏进应用)。
pub fn cmp_tiebreak(a: &DictEntry, b: &DictEntry) -> Ordering {
    b.base_weight
        .cmp(&a.base_weight)
        .then_with(|| b.word.chars().count().cmp(&a.word.chars().count()))
        .then_with(|| b.word.cmp(&a.word))
}

/// viterbi 组句从组内取「可信最优」词条: base 词, 或已确认(c≥2)的用户词。
/// c=1 沉底造词不参与自动组句 —— 误上屏一次的碎片(如 侺呣@shenm)曾借
/// viterbi「词数少恒胜」霸占面板 #1 并自锁(生产 2026-09-17~10-09 实锤,
/// 46 次误上屏、45 词条污染 user.dict)。
///
/// 方向警告: cmp_ranked 是为 sort_by 设计的**降序**比较器, 而 max_by 期望
/// 升序 —— 必须交换参数写成 cmp_ranked(b, a)。0.3.0~0.3.2 直接
/// max_by(cmp_ranked(a, b)) 反转成取组内**最低分**, 组句尽是权重 1 的
/// 生僻字(shen→侺、zuo→咗), 垃圾句霸占 #1。
pub fn best_composable<'a>(entries: &'a [DictEntry], now: u32) -> Option<&'a DictEntry> {
    let top = key_top(entries);
    entries
        .iter()
        .filter(|e| e.base_weight > 0 || e.selection_count >= 2)
        .max_by(|a, b| cmp_ranked(b, a, top, now))
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

    /// 上下文加成(分)。上限与"组内计数分跨度"同量级, 不随 key_top 缩放 ——
    /// 这是 2026-09-17 生产事故的修复: 旧实现 boost = key_top·c/(c+2) 在
    /// key_top≈10 万时一次共现就给 36022 分, 而组内计数分跨度只有约 0~40,
    /// 结果上下文完全压倒计数证据(吧 33 次/把 20 次也会被一次共现掀翻),
    /// 且加成被自身推荐的结果反复喂大, 形成自我强化环路。
    ///
    /// 现按 Jelinek-Mercer 平滑的直觉: 低共现次数不可靠, 权重按 c/(c+K) 回退;
    /// 上限取 12 ≈ 计数差 8 倍的跨度 —— 近似平局时上下文能做倾向性调整,
    /// 但翻不动明显的使用习惯差异。
    pub fn boost(&self, prev: &str, cur: &str) -> u64 {
        let c = self.count(prev, cur);
        if c == 0 {
            return 0;
        }
        (CTX_BOOST_MAX as f64 * (c as f64 / (c as f64 + CTX_BOOST_K))).round() as u64
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
    fn tiebreak_is_top_free_total_order() {
        // 跨 key 混排的决胜链必须与 key top 无关 —— cmp_ranked 的分数层依赖
        // 单侧 top, 同一对元素两侧比较结论可相反 → 违反全序 → sort panic
        // (生产 2026-09-17 实锤)。cmp_tiebreak 签名即无 top,此处锁死
        // 反对称/自反/base 降序语义。
        let a = e("在啊", 900000, 0, 0);
        let b = e("造词", 0, 2, 100);
        assert_eq!(cmp_tiebreak(&a, &a), Ordering::Equal);
        assert_eq!(cmp_tiebreak(&a, &b), cmp_tiebreak(&b, &a).reverse());
        assert_eq!(cmp_tiebreak(&a, &b), Ordering::Less, "base 高者前");
        let c = e("甲", 100, 0, 0);
        let d = e("乙", 100, 0, 0);
        assert_eq!(
            cmp_tiebreak(&c, &d),
            cmp_tiebreak(&d, &c).reverse(),
            "base 相同也必须反对称"
        );
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
    fn bigram_boost_is_bounded_and_backs_off_at_low_counts() {
        let mut bg = UserBigram::default();
        assert_eq!(bg.boost("我", "把"), 0); // 无共现无加成

        // 低共现不可靠 → 回退: c=1 的加成须远小于上限
        bg.bump(Some("我"), "把");
        let b1 = bg.boost("我", "把");
        assert!(b1 > 0 && b1 <= CTX_BOOST_MAX / 2, "c=1 应显著回退: {b1}");

        for _ in 0..19 {
            bg.bump(Some("我"), "把");
        } // c=20
        let b20 = bg.boost("我", "把");
        assert!(b20 > b1 * 2, "c=20 加成应明显强于 c=1: {b20} vs {b1}");
        assert!(b20 <= CTX_BOOST_MAX, "不得越上限: {b20}");
    }

    /// 回归(生产 2026-09-17): 上下文加成必须与组内计数分跨度同量级。
    /// 旧实现按 key_top(≈10 万) 缩放, 一次共现给 36022 分而计数分跨度仅 ~40,
    /// 导致 吧(33 次)/把(20 次) 每次选完就翻转, 计数体系形同虚设。
    #[test]
    fn context_tips_near_ties_but_not_large_count_gaps() {
        let top = 108066;
        let now = 100;
        let mut bg = UserBigram::default();
        for _ in 0..12 {
            bg.bump(Some("我"), "把");
        } // c=12

        // 近似平局 吧(33) vs 把(20): 计数仅差 1 分 → 上下文应能翻盘
        let zui = e("吧", 25526, 33, now);
        let ba = e("把", 108066, 20, now);
        let (s_zui, s_ba) = (entry_score(&zui, top, now), entry_score(&ba, top, now));
        assert!(s_zui > s_ba, "纯计数: 吧 领先 ({s_zui} vs {s_ba})");
        assert!(
            s_ba + bg.boost("我", "把") > s_zui,
            "上下文应能翻转近似平局: {} + {} vs {}",
            s_ba,
            bg.boost("我", "把"),
            s_zui
        );

        // 明显差异 常用(1000 次) vs 冷门(1 次): 计数差 20 分 → 上下文不得推翻
        let hot = e("把", 108066, 1000, now);
        let cold = e("吧", 25526, 1, now);
        let (s_hot, s_cold) = (entry_score(&hot, top, now), entry_score(&cold, top, now));
        assert!(s_hot > s_cold, "纯计数: 常用词领先");
        assert!(
            s_cold + bg.boost("我", "把") < s_hot,
            "上下文不得推翻明显用量差异: {} + {} vs {}",
            s_cold,
            bg.boost("我", "把"),
            s_hot
        );
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
