# 飞白 (feibai) 开发计划

> 目标：一个 binary，Linux 全平台中文拼音输入法。简单安装，开箱即用。

## 架构

```
feibai (单一二进制)
├── Wayland 模式 (input-method-v2) → Sway/COSMIC/Hyprland/KDE Wayland
└── IBus 模式 (D-Bus) → GNOME Wayland/X11

crates/
├── feibai-core/       # Engine trait, KeyEvent, EngineAction
├── feibai-pinyin/     # 拼音引擎（分词、Viterbi整句、用户词学习）
├── feibai-ui/         # 共享 UI 渲染（cosmic-text + tiny-skia → pixel buffer）
└── feibai/            # 主 binary（Wayland frontend + IBus frontend）
```

## 已完成

- [x] Workspace 骨架 + core traits
- [x] 拼音引擎（HashMap 词典、音节分词）
- [x] Wayland frontend (input-method-v2 + keyboard grab + xkb)
- [x] 候选弹窗 (input_popup_surface_v2 + wl_shm)
- [x] feibai-ui 共享渲染（9 主题）
- [x] Shift 切换中英文
- [x] 自建 MIT 词库（35 万条）
- [x] 用户词学习（去重 + 权重累加）
- [x] XDG 路径规范（~/.config/feibai/）
- [x] IBus 模式支持 GNOME
- [x] 单一二进制 + 环境自动检测
- [x] 去除 tokio，改用 async-io（3 线程）
- [x] 翻页（=/- 或 PageUp/PageDown）
- [x] Viterbi DP 整句输入
- [x] install.sh 安装脚本

## 下一步

- [ ] 中文标点映射（中文模式下 , → ，  . → 。）
- [ ] 状态提示（当前中/英模式反馈）
- [ ] 模糊音支持（zh/z、sh/s、ang/an 等）
- [ ] 自启动（systemd user service 或 XDG autostart）

## 远期

- [ ] bigram 语言模型（提升整句准确度）
- [ ] 词库管理工具（导入/导出/合并）
- [ ] 皮肤/外观自定义
- [ ] 剪贴板词条快捷输入
