#!/usr/bin/env bash
#
# sync_user_dict.sh — 把本地 user.dict.txt 的通用词同步回仓库词库
#
# 方向: ~/.config/feibai/user.dict.txt  →  data/dicts/feibai.extra.dict.yaml
#
# 同步规则 (保证通用性, 避免人名/称呼/杂质入库):
#   1. 只同步 word 不在任何仓库词典 (base/tech/extra) 中的新词
#      —— base/tech 已覆盖的词靠本地 user.dict 学习即可, 不重复入库
#   2. 含字母/数字杂质 的词 (如 "手doong")  → 过滤
#   3. 明显的人名/称呼模式 (X总/X哥/X姐/X先生/X老师/X同学/阿X/老X 等) → 过滤
#   4. 其余新词(用户新造的口语/实义词)一律同步
#
# 被过滤的词保留在本地 user.dict.txt (不影响个人使用), 只是不入仓库。
#
# 权重映射: 用户词的 c=1000000+n (n 为选中次数) → extra 区间 [10000, 49000],
# 保留相对排序, 不挤掉内置高频词。
set -euo pipefail

# --- 路径 ---
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
USER_DICT="${FEIBAI_USER_DICT:-$HOME/.config/feibai/user.dict.txt}"
EXTRA_DICT="$REPO_ROOT/data/dicts/feibai.extra.dict.yaml"
BASE_DICT="$REPO_ROOT/data/dicts/feibai.base.dict.yaml"
TECH_DICT="$REPO_ROOT/data/dicts/feibai.tech.dict.yaml"

if [ ! -f "$USER_DICT" ]; then
    echo "错误: 找不到用户词库 $USER_DICT" >&2
    exit 1
fi
if [ ! -f "$EXTRA_DICT" ]; then
    echo "错误: 找不到仓库词库 $EXTRA_DICT" >&2
    exit 1
fi

python3 - "$USER_DICT" "$EXTRA_DICT" "$BASE_DICT" "$TECH_DICT" << 'PYEOF'
import sys, os, re

user_dict, extra, base, tech = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]

def load_repo(paths):
    """返回 (known_words: set, exact: set of (word,pinyin))，跳过 YAML 头部和注释"""
    known_words = set()   # 所有已知 word (判断是否通用词)
    exact = set()         # (word, pinyin) 精确匹配
    for f in paths:
        if not os.path.exists(f):
            continue
        hdr = 0
        for line in open(f, encoding='utf-8'):
            s = line.strip()
            if s == '---':
                hdr += 1
                continue
            if s == '...':
                hdr = 0
                continue
            if hdr or s.startswith('#') or not s:
                continue
            parts = line.rstrip('\n').split('\t')
            if len(parts) >= 3 and parts[0] and parts[1]:
                known_words.add(parts[0])
                exact.add((parts[0], parts[1]))
    return known_words, exact

repo_words, repo_exact = load_repo([extra, base, tech])

# --- 通用性过滤规则 ---

# 1. 含字母/数字杂质 的词 (词本身混入拉丁字符, 如 手doong)
def has_junk(word, pinyin):
    if re.search(r'[a-zA-Z0-9]', word):
        return True  # 词本身含字母/数字
    # 拼音不得含非法字符 (只允许小写字母 + 空格)
    if not re.fullmatch(r'[a-z ]+', pinyin):
        return True
    # 拼音每个音节必须是小写字母 (允许 a/e/o 等单字母合法音节)
    for syl in pinyin.split():
        if not syl.islower() or not syl:
            return True
    return False

# 2. 明显的人名/称呼模式
#    X总 / X哥 / X姐 / X先生 / X老师 / X同学 / X主任 / X经理 / X秘书 / X老板
#    阿X(2字时, 阿在首) / 老X(2字时, 老在首, 且非普通词)
TITLE_SUFFIXES = ('总', '哥', '姐', '先生', '老师', '同学', '主任', '经理', '师傅', '老板', '秘书', '医生', '厅长', '局长', '科长', '处长', '教授')
def is_title_word(word):
    if len(word) >= 2:
        for suf in TITLE_SUFFIXES:
            if word.endswith(suf) and len(word) > len(suf):
                return True
    # 阿X / 老X (2字)
    if len(word) == 2 and word[0] in ('阿', '老'):
        return True
    return False

def map_weight(n):
    """c=1000000+n → [10000, 49000]；低于基准的给 30000"""
    base = 1000000
    if n < base:
        return 30000
    inc = (n - base) * 10
    if inc > 39000:
        inc = 39000
    return 10000 + inc

# --- 收集可同步的词 ---
new_words = {}          # (word, pinyin) -> n (权重)
filtered = {'junk': [], 'title': []}   # 统计被过滤的

for line in open(user_dict, encoding='utf-8'):
    if line.startswith('#') or not line.strip():
        continue
    parts = line.rstrip('\n').split('\t')
    if len(parts) < 3:
        continue
    pinyin, word, meta = parts[0], parts[1], parts[2]
    if not pinyin or not word:
        continue
    # 已在仓库词典中的词跳过 (按 word 判断, 避免对 base/tech 已覆盖的词重复入库)
    if word in repo_words:
        continue

    # 过滤: 杂质
    if has_junk(word, pinyin):
        filtered['junk'].append(word)
        continue
    # 过滤: 人名/称呼
    if is_title_word(word):
        filtered['title'].append(word)
        continue

    # 提取 c= 权重
    n = 1000000
    for tok in meta.split():
        if tok.startswith('c='):
            try:
                n = int(tok[2:])
            except ValueError:
                pass
            break
    key = (word, pinyin)
    if key not in new_words or new_words[key] < n:
        new_words[key] = n

# 生成新词条, 权重降序
entries = [(map_weight(n), w, p) for (w, p), n in new_words.items()]
entries.sort(key=lambda x: -x[0])

# 追加到 extra 末尾 (不重排已有词条)
new_lines = [f"{w}\t{p}\t{x}\n" for x, w, p in entries]
with open(extra, 'a', encoding='utf-8') as f:
    f.writelines(new_lines)

# --- 输出统计 ---
print(f"新增 {len(entries)} 个用户词到 feibai.extra.dict.yaml")
if entries:
    preview = "  ".join(f"{w}={x}" for x, w, p in entries[:5])
    print(f"  权重最高: {preview}")
print(f"已过滤 {len(filtered['junk'])} 个含杂质词: {', '.join(filtered['junk'][:10])}")
print(f"已过滤 {len(filtered['title'])} 个人名/称呼词: {', '.join(filtered['title'][:10])}")
PYEOF