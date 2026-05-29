# 飞白 (feibai)

轻量级 Linux 中文拼音输入法，Rust 编写，单一二进制，开箱即用。

## 特性

- **双模式运行** — Wayland 原生（input-method-v2）+ IBus（GNOME/KDE）
- **整句输入** — Viterbi DP 分词，自动选择最优路径
- **用户词学习** — 去重 + 权重累加，常用词自动靠前
- **9 种主题** — Light / Dark / Flat / Blue / Sakura / Ocean / Lavender / Tangerine / Mint
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

```bash
./install.sh
```

脚本会：
1. 编译 release binary → `~/.local/bin/feibai`
2. 复制词库 → `~/.config/feibai/`
3. 安装 IBus 组件 XML（如检测到 IBus）

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

安装后在「设置 → 键盘 → 输入源」中添加 **Feibai Pinyin**。

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

## 配置

配置文件：`~/.config/feibai/config.toml`

```toml
theme = "dark"   # light/dark/flat/blue/sakura/ocean/lavender/tangerine/mint
```

## 词库

- `feibai.base.dict.yaml` — 基础词库（35 万词条）
- `feibai.extra.dict.yaml` — 扩展词库
- `user.dict.txt` — 用户词库（自动生成，选词后自动学习）

词库格式兼容 Rime YAML dict 格式。详见 [data/dicts/SOURCES.md](data/dicts/SOURCES.md)。

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
cargo test
```

## License

MIT
