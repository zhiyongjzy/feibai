# 飞白 (feibai)

轻量级 Linux 中文拼音输入法，Rust 编写，单一二进制，开箱即用。

## 特性

- **双模式运行** — Wayland 原生（input-method-v2）+ IBus（GNOME/KDE）
- **整句输入** — Viterbi DP 分词，自动选择最优路径
- **用户词学习** — 选中计数 + 周期衰减，常用词自动靠前；上下文共现（bigram）参与排序
- **9 种主题** — Light / Dark / Flat / Blue / Sakura / Ocean / Lavender / Tangerine / Mint（仅 Wayland 模式，IBus 使用系统候选窗）
- **超轻量** — 单一 ~8MB 二进制，3 线程 async-io，无 tokio 依赖
- **MIT 词库** — 35 万词条，无 GPL 污染

## 架构

```
feibai (单一二进制)
├── Wayland 模式 → Sway / COSMIC / Hyprland / KDE Wayland
│   └── input-method-v2 + keyboard grab + 自渲染候选弹窗
└── IBus 模式 → GNOME Wayland / X11 / KDE X11
    └── D-Bus 协议，系统级候选窗

crates/
├── feibai-core/       # Engine trait, KeyEvent, EngineAction
├── feibai-pinyin/     # 拼音引擎（分词、Viterbi、用户词）
├── feibai-ui/         # 共享 UI 渲染（cosmic-text + tiny-skia）
└── feibai/            # 主 binary（Wayland + IBus frontend）
```

## 构建

```bash
# 依赖（Debian/Ubuntu）
sudo apt install libwayland-dev libxkbcommon-dev pkg-config

# 编译
cargo build --release

# 输出
target/release/feibai
```

## 安装

### 快速安装（推荐）

无需 Rust 环境，自动下载预编译二进制。脚本会装二进制、词库、IBus 组件，并在 GNOME 上自动添加输入源：

```bash
curl -fsSL https://raw.githubusercontent.com/zhiyongjzy/feibai/main/install.sh | bash
```

> - 安装 IBus 组件需要 sudo 密码；若 sudo 失败，脚本会打印手动安装命令。
> - 预编译二进制要求 glibc ≥ 2.39（Ubuntu 24.04+）。旧系统报 `GLIBC_2.xx not found` 时改用 `--from-source` 本机编译。

### 从源码构建

```bash
git clone https://github.com/zhiyongjzy/feibai.git
cd feibai
./install.sh --from-source
```

### 选项

| 参数 | 说明 |
|------|------|
| `--from-source` | 使用 cargo 本地编译（需要 Rust 工具链） |
| `--force-dicts` | 覆盖已有词库文件（用于更新词库） |

### 运行时依赖

- `libxkbcommon0`（Ubuntu/Debian 桌面环境默认已安装）
- IBus（GNOME 默认已安装）
- glibc ≥ 2.39（预编译二进制；旧系统用 `--from-source` 本机编译）

## 使用

### Wayland 合成器（Sway / COSMIC / Hyprland）

```bash
feibai
```

在 Sway 中添加自启动：
```
# ~/.config/sway/config
exec feibai
```

### GNOME / KDE

安装脚本会自动把 Feibai Pinyin 加入 GNOME 输入源，装完用 Super+Space 切换即可。若未自动添加，手动到「设置 → 键盘 → 输入源 → + → 中文 → Feibai Pinyin」。

### 快捷键

| 按键 | 功能 |
|------|------|
| 字母 a-z | 输入拼音 |
| Space | 选择第一个候选 |
| 1-9 | 按编号选择候选 |
| Enter | 提交原始拼音 |
| Escape | 清空输入 |
| Backspace | 删除最后一个字符 |
| Shift（单独按下释放） | 中/英切换 |
| = / - 或 PageDown / PageUp | 翻页 |
| ' | 拼音分隔符（如 xi'an） |

`lue/lve`、`nue/nve` 均可输入，选词学习使用同一份计数；例如 `culue` 和 `culve` 都能查到“粗略”。原始拼音显示不变。

连续输入中孤立的 `m/n` 不参与自动组句，避免 `zenm` 生成“怎呣”；仍保留正常词和前缀补全候选。单独的 `m/n` 音节照常可用，需要明确组合时可用分隔符，如 `zen'm`。

## 配置

配置文件：`~/.config/feibai/config.toml`

```toml
theme = "dark"   # light/dark/flat/blue/sakura/ocean/lavender/tangerine/mint
```

## 词库

- `feibai.base.dict.yaml` — 基础词库（35 万词条）
- `feibai.extra.dict.yaml` — 扩展词库（互联网热词）
- `feibai.tech.dict.yaml` — 技术词库
- `feibai.en.dict.yaml` — 英文词库（Google 万词，混输）
- `user.dict.txt` — 用户词库（自动生成，选词后自动学习；格式 `拼音\t词\tc=次数 t=周`，旧格式自动迁移备份）
- `user.bigram.txt` — 用户上下文共现计数（自动生成，前词→当前词的搭配记录）

词库格式兼容 Rime YAML dict 格式。详见 [data/dicts/SOURCES.md](data/dicts/SOURCES.md)。

### 同步用户词库到仓库

`user.dict.txt` 里积累的用户新词（本地自动学习）可以同步回仓库的 `feibai.extra.dict.yaml`。**默认仅预览，不修改词库**，列出所有候选的词、拼音、次数和建议权重：

```bash
./tools/sync_user_dict.sh
```

人工审核后创建两列 TSV 词单（`词<TAB>拼音`，不是预览的四列输出；支持空行、`#` 注释和空格分隔拼音），再显式写入：

```bash
printf '晶振\tjing zhen\n' > /tmp/feibai-approved.tsv
./tools/sync_user_dict.sh --approved /tmp/feibai-approved.tsv  # 仍然只预览
./tools/sync_user_dict.sh --write --approved /tmp/feibai-approved.tsv
```

审核词必须来自合格用户词候选，拼音也必须匹配；任何不匹配都会在写入前报错。默认 `--min-count 3`，可用 `--min-count 1` 审核低频词。**次数只是预筛门槛，不保证语义正确**：短时间反复误选也可能超过门槛，不能整批照收。此工具不删除或更改本地学习记录。

特性：

- **只同步新词** — 只同步 `word` 不在任何仓库词典（base/tech/extra）中的词，base/tech 已覆盖的词靠本地 `user.dict.txt` 学习，不重复入库
- **通用性过滤** — 自动过滤含字母/数字杂质（如 `手doong`）和明显的人名/称呼模式（如 `X总`、`X老师`、`阿X`），被过滤的词保留在本地不影响个人使用
- **权重映射** — 用户词计数格式（`c=次数 t=周`，次数为较小的整数，从 1 起步，不再是旧版的百万级权重）按次数以对数曲线映射到 `[10000, 49000]` 区间，次数高者权重高；旧格式行（无 `t=`）按 1 次处理
- **幂等** — 重复运行不会重复添加
- **保留原词条顺序** — 只追加新词到文件末尾，不重排已有词条，git diff 清晰可审查

可用环境变量 `FEIBAI_USER_DICT` 覆盖用户词库路径：

```bash
FEIBAI_USER_DICT=/path/to/user.dict.txt ./tools/sync_user_dict.sh
```

## 日志与调试

日志文件位于 `~/.local/state/feibai/feibai.log`，自动轮转（>10MB 时重命名为 `.log.old`）。

启用 debug 详细日志（记录每次按键、候选列表、选词）：

```bash
# 方式一：创建 sentinel 文件（推荐，适用于 ibus-daemon 拉起的场景）
touch ~/.config/feibai/.debug
ibus restart

# 方式二：CLI flag（手动启动时）
feibai --ibus --debug

# 方式三：环境变量（直接运行时）
FEIBAI_DEBUG=1 feibai --ibus
```

关闭 debug：

```bash
rm ~/.config/feibai/.debug
ibus restart
```

## 测试

```bash
dbus-run-session -- cargo test --workspace
python3 -m unittest discover -s tools -p 'test_*.py'
```

## License

MIT
