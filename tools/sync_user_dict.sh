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
#   4. 个人排除清单 ~/.config/feibai/sync_exclude.txt 中的词 → 过滤
#      (每行一个词, # 注释; 人名/昵称/团队黑话等不便入库的个人词写在这里)
#   5. 选中次数 < min-count (默认 3) → 过滤; 次数仅作候选门槛, 不保证是真词
#   6. 默认只预览; --write --approved FILE 仅同步人工审核的词和拼音
#
# 被过滤的词保留在本地 user.dict.txt (不影响个人使用), 只是不入仓库。
#
# 权重映射: 用户词计数 c=N (新格式 `c=N t=周`; 旧格式无 t= 行按 N=1 过渡处理,
# 引擎重启后 user.dict.txt 会自动迁移成新格式) → log2 曲线映射到 extra 区间
# [10000, 49000]: 次数每翻倍约 +6500, c≥64 封顶 49000 (仍低于 extra 头词的 50000),
# 保留相对排序, 不挤掉内置高频词。
#   跳过两类行 (与引擎迁移规则对齐): 旧格式 c≥1000000 (0.1.1 前误学脏数据,
#   引擎迁移直接丢弃, 不得导出进共享词库); t= 值不可解析的损坏行。
#
# 拼音列: 统一输出连写形式 (与新格式 user.dict.txt 的拼音列一致; engine 的
# parse_rime_dict 对连写/空格分隔拼音均兼容), 旧格式行的空格分隔拼音先归一化。
set -euo pipefail

# --- 路径 ---
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
USER_DICT="${FEIBAI_USER_DICT:-$HOME/.config/feibai/user.dict.txt}"
EXCLUDE_FILE="${FEIBAI_SYNC_EXCLUDE:-$HOME/.config/feibai/sync_exclude.txt}"
EXTRA_DICT="$REPO_ROOT/data/dicts/feibai.extra.dict.yaml"
BASE_DICT="$REPO_ROOT/data/dicts/feibai.base.dict.yaml"
TECH_DICT="$REPO_ROOT/data/dicts/feibai.tech.dict.yaml"
SYLLABLES_FILE="$REPO_ROOT/data/pinyin_syllables.txt"

if [ ! -f "$USER_DICT" ]; then
    echo "错误: 找不到用户词库 $USER_DICT" >&2
    exit 1
fi
if [ ! -f "$EXTRA_DICT" ]; then
    echo "错误: 找不到仓库词库 $EXTRA_DICT" >&2
    exit 1
fi

python3 - "$USER_DICT" "$EXTRA_DICT" "$BASE_DICT" "$TECH_DICT" "$EXCLUDE_FILE" "$SYLLABLES_FILE" "$@" << 'PYEOF'
import argparse, sys, os, re, math, tempfile, stat

user_dict, extra, base, tech = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
exclude_file = sys.argv[5]

def nonnegative_int(value):
    if not re.fullmatch(r'[0-9]+', value):
        raise argparse.ArgumentTypeError("需要非负整数")
    return int(value)

parser = argparse.ArgumentParser(
    prog="sync_user_dict.sh",
    description="默认预览候选词; 写入必须提供人工审核词单 (词<TAB>拼音)。",
)
parser.add_argument("--min-count", type=nonnegative_int, default=3)
parser.add_argument("--approved", metavar="FILE", help="审核词单: 每行词<TAB>拼音, 支持 # 注释")
parser.add_argument("--write", action="store_true", help="仅写入审核词单中的合格候选")
args = parser.parse_args(sys.argv[7:])
if args.write and not args.approved:
    parser.error("--write 必须同时指定 --approved FILE")
min_count = args.min_count
with open(sys.argv[6], encoding='utf-8') as f:
    syllables = set(f.read().split())

def segment(part):
    """与 engine.rs segment_dp 一致: 完整覆盖优先, 否则贪心保留末尾片段。"""
    size = len(part)
    if not size:
        return []
    dp = [(0, 0, 0)] * (size + 1)
    for i in range(size - 1, -1, -1):
        best = (0, 0, i + 1)
        for length in range(1, min(size - i, 6) + 1):
            if part[i:i + length] in syllables:
                covered, count, _ = dp[i + length]
                candidate = (covered + length, count + 1, i + length)
                if candidate[0] > best[0] or (candidate[0] == best[0] and candidate[1] < best[1]):
                    best = candidate
        dp[i] = best
    result = []
    pos = 0
    while pos < size:
        if dp[0][0] == size:
            end = dp[pos][2]
        else:
            end = next((pos + n for n in range(min(size - pos, 6), 0, -1)
                        if part[pos:pos + n] in syllables), size)
        result.append(part[pos:end])
        pos = end
    return result

def normalize_pinyin(pinyin, word):
    compact = ''.join(pinyin.split())
    if 'lue' not in pinyin and 'nue' not in pinyin:
        return compact
    segments = [s for part in pinyin.split() for s in segment(part)]
    if len(segments) < len(word):
        return compact
    return ''.join({'lue': 'lve', 'nue': 'nve'}.get(s, s) for s in segments)

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
                exact.add((parts[0], normalize_pinyin(parts[1], parts[0])))
    return known_words, exact

repo_words, repo_exact = load_repo([extra, base, tech])

# 个人排除清单: 每行一个词, # 开头为注释 (人名/昵称/团队黑话等)
exclude_words = set()
if os.path.exists(exclude_file):
    for line in open(exclude_file, encoding='utf-8'):
        s = line.strip()
        if s and not s.startswith('#'):
            exclude_words.add(s)

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

def map_weight(count):
    """count(选中次数) → [10000, 49000], log2 曲线: 每翻倍约 +6500, c≥64 封顶。
    封顶 49000 仍低于 extra 头词(50000), 不挤掉内置高频词; count=1 沉底 10000。"""
    ratio = min(1.0, math.log2(count) / math.log2(64))
    return round(10000 + 39000 * ratio)

# --- 收集可同步的词 ---
new_words = {}          # (word, pinyin) -> count (选中次数)
filtered = {'junk': [], 'title': [], 'exclude': [], 'min_count': 0}   # 统计被过滤的

for line in open(user_dict, encoding='utf-8'):
    if line.startswith('#') or not line.strip():
        continue
    parts = line.rstrip('\n').split('\t')
    if len(parts) < 3:
        continue
    pinyin, word, meta = parts[0], parts[1], parts[2]
    if not pinyin or not word:
        continue
    # 拼音归一化成连写形式: 新格式本就连写, 旧格式空格分隔;
    # 同词的新旧格式行据此正确去重
    pinyin = normalize_pinyin(pinyin, word)
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
    # 过滤: 个人排除清单
    if word in exclude_words:
        filtered['exclude'].append(word)
        continue

    # 提取 c= 计数 (双格式, 跳过规则与引擎 parse_userdb 迁移逻辑对齐):
    #   新格式 "c=N t=W" → count=N
    #   旧格式 "c=M" (无 t=, 绝对权重) → count=1 (过渡兼容, 引擎重启后文件自动迁移)
    #   跳过: t= 存在但不可解析 (损坏行); 旧格式 c≥1000000 (0.1.1 前误学脏数据)
    raw_c = None
    week = None
    has_t = False
    for tok in meta.split():
        if tok.startswith('c='):
            try:
                raw_c = int(tok[2:])
            except ValueError:
                pass
        elif tok.startswith('t='):
            has_t = True
            try:
                week = int(tok[2:])
            except ValueError:
                week = None  # 多个 t= 时后者覆盖前者, 与引擎一致
    if raw_c is None:
        continue  # 无有效 c= 字段, 跳过
    if has_t and week is None:
        continue  # 损坏行 (t= 不可解析): 与引擎一致跳过
    if not has_t:
        if raw_c >= 1_000_000:
            continue  # 0.1.1 前误学脏数据: 引擎迁移丢弃, 不入共享词库
        count = 1
    else:
        count = raw_c
    if count < 1:
        count = 1  # c=0/负数防御: log2 需要 ≥1
    # 次数仅作为预筛门槛; 连续误选也可能达到门槛, 写入前仍需人工审核
    if count < min_count:
        filtered['min_count'] += 1
        continue
    key = (word, pinyin)
    if key not in new_words or new_words[key] < count:
        new_words[key] = count

# 审核词单不能绕过过滤; 全部校验通过后才允许写文件
if args.approved:
    approved = set()
    try:
        with open(args.approved, encoding='utf-8') as f:
            for lineno, line in enumerate(f, 1):
                if not line.strip() or line.lstrip().startswith('#'):
                    continue
                parts = line.rstrip('\n').split('\t')
                if len(parts) != 2 or not all(part.strip() for part in parts):
                    parser.error(f"{args.approved}:{lineno}: 需要 词<TAB>拼音 两列")
                word = parts[0].strip()
                pinyin = normalize_pinyin(parts[1], word)
                if has_junk(word, pinyin):
                    parser.error(f"{args.approved}:{lineno}: 词或拼音含非法字符")
                key = (word, pinyin)
                if key not in new_words and key not in repo_exact:
                    parser.error(f"{args.approved}:{lineno}: 非合格候选或拼音不匹配: {word} / {pinyin}")
                approved.add(key)
    except OSError as e:
        parser.error(f"读取审核词单失败: {e}")
    new_words = {key: count for key, count in new_words.items() if key in approved}

# 生成新词条, 权重降序, 同权重时保证输出稳定
entries = [(map_weight(c), w, p) for (w, p), c in new_words.items()]
entries.sort(key=lambda x: (-x[0], x[1], x[2]))

if args.write and entries:
    # 保留原顺序和权限, 原子替换避免写入中断留下半个词条
    with open(extra, encoding='utf-8') as f:
        content = f.read()
    if content and not content.endswith('\n'):
        content += '\n'
    tmp_path = None
    try:
        with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8',
                                         dir=os.path.dirname(extra), delete=False) as f:
            tmp_path = f.name
            os.fchmod(f.fileno(), stat.S_IMODE(os.stat(extra).st_mode))
            f.write(content)
            f.writelines(f"{w}\t{p}\t{x}\n" for x, w, p in entries)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp_path, extra)
        tmp_path = None
    finally:
        if tmp_path is not None:
            os.unlink(tmp_path)

# --- 输出统计 ---
mode = "已写入" if args.write else "仅预览, 未写入"
print(f"{mode}: {len(entries)} 个候选词 (min-count={min_count})")
print("词\t拼音\t次数\t权重")
for weight, word, pinyin in entries:
    print(f"{word}\t{pinyin}\t{new_words[(word, pinyin)]}\t{weight}")
print(f"已过滤 {len(filtered['junk'])} 个含杂质词: {', '.join(filtered['junk'][:10])}")
print(f"已过滤 {len(filtered['title'])} 个人名/称呼词: {', '.join(filtered['title'][:10])}")
print(f"已过滤 {len(filtered['exclude'])} 个排除清单词: {', '.join(filtered['exclude'][:10])}")
print(f"已过滤 {filtered['min_count']} 个低频词 (c<{min_count}; 次数不代表语义正确)")
PYEOF