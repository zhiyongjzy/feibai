# 飞白 (feibai) Phase 1 实现计划

> 目标：在 COSMIC/Sway 下通过 input-method-v2 完成"拼音→选字→提交"完整流程。

**架构**：三层分离 — Frontend(Wayland) / Core(traits+dispatch) / Engine(pinyin HashMap)

**技术栈**：wayland-client 0.31, wayland-protocols-misc 0.3, calloop 0.14, xkbcommon 0.9

**开发环境**：本地编译，远程 `jzy@192.168.66.66` (Arch, COSMIC 1.0.13, Rust 1.95) 测试

---

## Task 1: Workspace 骨架 + feibai-core traits

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/feibai-core/Cargo.toml`
- Create: `crates/feibai-core/src/lib.rs`
- Create: `crates/feibai-pinyin/Cargo.toml`
- Create: `crates/feibai-pinyin/src/lib.rs`
- Create: `crates/feibai-wl/Cargo.toml`
- Create: `crates/feibai-wl/src/main.rs`

### Step 1: 创建 workspace Cargo.toml

```toml
# Cargo.toml
[workspace]
resolver = "2"
members = ["crates/*"]
```

### Step 2: 创建 feibai-core/Cargo.toml

```toml
# crates/feibai-core/Cargo.toml
[package]
name = "feibai-core"
version = "0.1.0"
edition = "2021"
```

### Step 3: 写 feibai-core/src/lib.rs

```rust
// crates/feibai-core/src/lib.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Press,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_: bool,
}

#[derive(Debug, Clone)]
pub struct KeyEvent {
    pub keysym: u32,
    pub unicode: Option<char>,
    pub modifiers: Modifiers,
    pub state: KeyState,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub text: String,
    pub comment: Option<String>,
}

#[derive(Debug, Clone)]
pub enum EngineAction {
    Commit(String),
    UpdatePreedit(String),
    UpdateCandidates(Vec<Candidate>),
    Forward,
    Noop,
}

pub trait Engine: Send {
    fn process_key(&mut self, key: &KeyEvent) -> Vec<EngineAction>;
    fn reset(&mut self);
}
```

### Step 4: 创建 feibai-pinyin 和 feibai-wl 占位

```toml
# crates/feibai-pinyin/Cargo.toml
[package]
name = "feibai-pinyin"
version = "0.1.0"
edition = "2021"

[dependencies]
feibai-core = { path = "../feibai-core" }
```

```toml
# crates/feibai-wl/Cargo.toml
[package]
name = "feibai-wl"
version = "0.1.0"
edition = "2021"

[dependencies]
feibai-core = { path = "../feibai-core" }
feibai-pinyin = { path = "../feibai-pinyin" }
```

```rust
// crates/feibai-pinyin/src/lib.rs
pub struct PinyinEngine;
```

```rust
// crates/feibai-wl/src/main.rs
fn main() {
    println!("feibai IME starting...");
}
```

### Step 5: 验证编译

```bash
cargo build
# Expected: Compiling feibai-core, feibai-pinyin, feibai-wl — all succeed
```

### Step 6: Commit

```bash
git init && git add -A && git commit -m "feat: workspace skeleton with core traits"
```

---

## Task 2: feibai-pinyin HashMap 引擎

**Files:**
- Modify: `crates/feibai-pinyin/src/lib.rs`
- Create: `crates/feibai-pinyin/src/engine.rs`
- Create: `data/pinyin_table.tsv` (minimal test data)

### Step 1: 写最小测试数据

```
# data/pinyin_table.tsv
ni	你 妮 尼 泥 逆
hao	好 号 浩 豪 耗
shi	是 时 事 十 世
jie	界 届 姐 借 解
zhong	中 种 重 终 众
guo	国 果 过 锅 裹
ren	人 仁 任 忍 认
da	大 打 达 搭
```

### Step 2: 写失败测试

```rust
// crates/feibai-pinyin/src/lib.rs
mod engine;
pub use engine::PinyinEngine;

#[cfg(test)]
mod tests {
    use super::*;
    use feibai_core::*;

    fn press_key(engine: &mut PinyinEngine, c: char) -> Vec<EngineAction> {
        let key = KeyEvent {
            keysym: c as u32,
            unicode: Some(c),
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        };
        engine.process_key(&key)
    }

    #[test]
    fn test_type_pinyin_shows_candidates() {
        let mut engine = PinyinEngine::from_file("../../data/pinyin_table.tsv").unwrap();
        let actions = press_key(&mut engine, 'n');
        // "n" alone has no match
        assert!(actions.iter().any(|a| matches!(a, EngineAction::UpdatePreedit(s) if s == "n")));

        let actions = press_key(&mut engine, 'i');
        // "ni" should match
        assert!(actions.iter().any(|a| matches!(a, EngineAction::UpdateCandidates(c) if !c.is_empty())));
    }

    #[test]
    fn test_space_commits_first() {
        let mut engine = PinyinEngine::from_file("../../data/pinyin_table.tsv").unwrap();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
        // press space (keysym 0x20)
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
        let mut engine = PinyinEngine::from_file("../../data/pinyin_table.tsv").unwrap();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
        // press '2' to select second candidate
        let key = KeyEvent {
            keysym: '2' as u32,
            unicode: Some('2'),
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        };
        let actions = engine.process_key(&key);
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == "妮")));
    }

    #[test]
    fn test_backspace_deletes() {
        let mut engine = PinyinEngine::from_file("../../data/pinyin_table.tsv").unwrap();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
        // backspace = keysym 0xff08
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
        let mut engine = PinyinEngine::from_file("../../data/pinyin_table.tsv").unwrap();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
        // escape = keysym 0xff1b
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
        let mut engine = PinyinEngine::from_file("../../data/pinyin_table.tsv").unwrap();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
        // enter = keysym 0xff0d
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
        let mut engine = PinyinEngine::from_file("../../data/pinyin_table.tsv").unwrap();
        let key = KeyEvent {
            keysym: 'c' as u32,
            unicode: Some('c'),
            modifiers: Modifiers { ctrl: true, ..Default::default() },
            state: KeyState::Press,
        };
        let actions = engine.process_key(&key);
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Forward)));
    }
}
```

### Step 3: 运行测试确认失败

```bash
cargo test -p feibai-pinyin
# Expected: FAIL — engine module doesn't exist yet
```

### Step 4: 实现引擎

```rust
// crates/feibai-pinyin/src/engine.rs
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use feibai_core::*;

pub struct PinyinEngine {
    table: HashMap<String, Vec<String>>,
    preedit: String,
    candidates: Vec<Candidate>,
}

impl PinyinEngine {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, String> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| format!("failed to read pinyin table: {e}"))?;
        let mut table = HashMap::new();
        for line in content.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut parts = line.splitn(2, '\t');
            let pinyin = parts.next().unwrap_or("").trim().to_string();
            let chars: Vec<String> = parts
                .next()
                .unwrap_or("")
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            if !pinyin.is_empty() && !chars.is_empty() {
                table.insert(pinyin, chars);
            }
        }
        Ok(Self {
            table,
            preedit: String::new(),
            candidates: Vec::new(),
        })
    }

    fn lookup(&mut self) {
        self.candidates = self
            .table
            .get(&self.preedit)
            .map(|chars| {
                chars.iter().map(|c| Candidate {
                    text: c.clone(),
                    comment: None,
                }).collect()
            })
            .unwrap_or_default();
    }
}

impl Engine for PinyinEngine {
    fn process_key(&mut self, key: &KeyEvent) -> Vec<EngineAction> {
        if key.state == KeyState::Release {
            return vec![EngineAction::Noop];
        }

        // Forward modifier combos
        if key.modifiers.ctrl || key.modifiers.alt || key.modifiers.super_ {
            return vec![EngineAction::Forward];
        }

        let keysym = key.keysym;

        // Escape
        if keysym == 0xff1b {
            if self.preedit.is_empty() {
                return vec![EngineAction::Forward];
            }
            self.preedit.clear();
            self.candidates.clear();
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
            self.preedit.pop();
            self.lookup();
            return vec![
                EngineAction::UpdatePreedit(self.preedit.clone()),
                EngineAction::UpdateCandidates(self.candidates.clone()),
            ];
        }

        // Enter — commit raw preedit
        if keysym == 0xff0d {
            if self.preedit.is_empty() {
                return vec![EngineAction::Forward];
            }
            let text = self.preedit.clone();
            self.preedit.clear();
            self.candidates.clear();
            return vec![EngineAction::Commit(text)];
        }

        // Space — commit first candidate
        if keysym == 0x20 {
            if let Some(c) = self.candidates.first() {
                let text = c.text.clone();
                self.preedit.clear();
                self.candidates.clear();
                return vec![EngineAction::Commit(text)];
            }
            if self.preedit.is_empty() {
                return vec![EngineAction::Forward];
            }
            return vec![EngineAction::Noop];
        }

        // Digit 1-9 — select candidate
        if let Some(ch) = key.unicode {
            if ch >= '1' && ch <= '9' && !self.candidates.is_empty() {
                let idx = (ch as usize) - ('1' as usize);
                if let Some(c) = self.candidates.get(idx) {
                    let text = c.text.clone();
                    self.preedit.clear();
                    self.candidates.clear();
                    return vec![EngineAction::Commit(text)];
                }
                return vec![EngineAction::Noop];
            }
        }

        // Letter keys — append to preedit
        if let Some(ch) = key.unicode {
            if ch.is_ascii_lowercase() {
                self.preedit.push(ch);
                self.lookup();
                return vec![
                    EngineAction::UpdatePreedit(self.preedit.clone()),
                    EngineAction::UpdateCandidates(self.candidates.clone()),
                ];
            }
        }

        // Anything else with empty preedit — forward
        if self.preedit.is_empty() {
            return vec![EngineAction::Forward];
        }

        vec![EngineAction::Noop]
    }

    fn reset(&mut self) {
        self.preedit.clear();
        self.candidates.clear();
    }
}
```

### Step 5: 运行测试确认通过

```bash
cargo test -p feibai-pinyin
# Expected: all 7 tests pass
```

### Step 6: Commit

```bash
git add -A && git commit -m "feat: pinyin HashMap engine with tests"
```

---

## Task 3: 准备完整 pinyin_table.tsv

从开源拼音数据源（如 rime-pinyin）抽取约 400 个合法音节 + 常用字词。
格式每行：`pinyin\tchar1 char2 char3 ...`

覆盖目标：
- 所有合法声母+韵母组合（~400 音节）
- 每音节 5-20 常用字
- ~2 万常用词组（如 `nihao\t你好`）

```bash
# 生成后验证
wc -l data/pinyin_table.tsv
# Expected: 400-2000 lines
cargo test -p feibai-pinyin
# Expected: still passes
git add data/ && git commit -m "data: complete pinyin lookup table"
```

---

## Task 4: Wayland Frontend (feibai-wl)

**Files:**
- Modify: `crates/feibai-wl/Cargo.toml`
- Modify: `crates/feibai-wl/src/main.rs`

### Step 1: 添加依赖

```toml
# crates/feibai-wl/Cargo.toml
[package]
name = "feibai-wl"
version = "0.1.0"
edition = "2021"

[dependencies]
feibai-core = { path = "../feibai-core" }
feibai-pinyin = { path = "../feibai-pinyin" }
wayland-client = "0.31"
wayland-protocols-misc = { version = "0.3", features = ["client"] }
wayland-protocols = { version = "0.32", features = ["client"] }
calloop = "0.14"
calloop-wayland-source = "0.3"
xkbcommon = { version = "0.9", features = ["wayland"] }
```

### Step 2: 实现 Wayland 状态结构

```rust
// crates/feibai-wl/src/main.rs
use wayland_client::{Connection, Dispatch, QueueHandle, EventQueue};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_protocols_misc::input_method_v2::client::{
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::ZwpInputMethodV2,
    zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use feibai_core::*;
use feibai_pinyin::PinyinEngine;

struct State {
    im_manager: Option<ZwpInputMethodManagerV2>,
    input_method: Option<ZwpInputMethodV2>,
    keyboard_grab: Option<ZwpInputMethodKeyboardGrabV2>,
    seat: Option<wl_seat::WlSeat>,
    engine: PinyinEngine,
    serial: u32,
    xkb_context: xkb::Context,
    xkb_state: Option<xkb::State>,
    active: bool,
}
```

### Step 3: 实现 wl_registry Dispatch（绑定 seat + im_manager）

### Step 4: 实现 ZwpInputMethodV2 Dispatch（activate/deactivate）

### Step 5: 实现 keyboard grab Dispatch（key 事件 → xkb → Engine）

### Step 6: Engine action → commit_string / set_preedit_string

### Step 7: 主函数 — calloop 事件循环

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    let engine = PinyinEngine::from_file("/usr/share/feibai/pinyin_table.tsv")
        .or_else(|_| PinyinEngine::from_file("data/pinyin_table.tsv"))
        .expect("cannot load pinyin table");

    let mut state = State {
        im_manager: None,
        input_method: None,
        keyboard_grab: None,
        seat: None,
        engine,
        serial: 0,
        xkb_context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
        xkb_state: None,
        active: false,
    };

    display.get_registry(&qh, ());
    event_queue.roundtrip(&mut state)?;

    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .unwrap();

    eprintln!("feibai: running on Wayland");
    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}
```

### Step 8: 编译验证

```bash
cargo build -p feibai-wl
# Expected: compiles (may warn about unused)
```

### Step 9: Commit

```bash
git add -A && git commit -m "feat: wayland frontend with input-method-v2"
```

---

## Task 5: 远程测试

```bash
# 本地交叉编译或在远程编译
ssh jzy@192.168.66.66 "cd feibai && cargo build --release"

# 在 COSMIC 下测试
ssh jzy@192.168.66.66 "WAYLAND_DISPLAY=wayland-1 ./target/release/feibai-wl"

# 预期：
# 1. 启动后 stderr 打印 "feibai: running on Wayland"
# 2. 打开终端/编辑器，输入 "nihao" 看到 preedit 下划线
# 3. 按空格提交 "你好"
# 4. Ctrl+C 等快捷键不被吃掉
```

---

## 后续 Phase（不在本次实现范围）

- Phase 2: DAG 音节切分 + Viterbi + cedarwood DARTS 词典
- Phase 3: Rime 词典导入工具
- Phase 4: IBus/XIM 前端
- Phase 5: Rime schema 兼容
