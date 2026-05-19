use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use zbus::object_server::SignalEmitter;
use zbus::{connection, interface, zvariant};

use feibai_core::{Candidate, EngineAction, Engine, KeyEvent, KeyState, Modifiers};
use feibai_pinyin::PinyinEngine;

// IBus GVariant helpers

fn ibus_text(s: &str) -> zvariant::Value<'static> {
    // IBusText: (sa{sv}sv)
    // ("IBusText", {}, text_string, <IBusAttrList>)
    let attr_list = ibus_attr_list();
    zvariant::Value::new((
        "IBusText".to_string(),
        HashMap::<String, zvariant::OwnedValue>::new(),
        s.to_string(),
        zvariant::Value::from(attr_list),
    ))
}

fn ibus_attr_list() -> (String, HashMap<String, zvariant::OwnedValue>, Vec<zvariant::OwnedValue>) {
    (
        "IBusAttrList".to_string(),
        HashMap::<String, zvariant::OwnedValue>::new(),
        Vec::<zvariant::OwnedValue>::new(),
    )
}

fn ibus_lookup_table(candidates: &[Candidate]) -> zvariant::Value<'static> {
    // IBusLookupTable: (sa{sv}uubbiavav)
    let page_size: u32 = 9;
    let cursor_pos: u32 = 0;
    let cursor_visible: bool = true;
    let round: bool = false;
    let orientation: i32 = 1; // vertical

    let cand_texts: Vec<zvariant::OwnedValue> = candidates
        .iter()
        .map(|c| zvariant::OwnedValue::try_from(ibus_text(&c.text)).unwrap())
        .collect();

    let labels: Vec<zvariant::OwnedValue> = candidates
        .iter()
        .enumerate()
        .map(|(i, _)| {
            zvariant::OwnedValue::try_from(ibus_text(&format!("{}.", i + 1))).unwrap()
        })
        .collect();

    zvariant::Value::new((
        "IBusLookupTable".to_string(),
        HashMap::<String, zvariant::OwnedValue>::new(),
        page_size,
        cursor_pos,
        cursor_visible,
        round,
        orientation,
        cand_texts,
        labels,
    ))
}

// IBus Engine implementation

struct FeibaiEngine {
    engine: Arc<Mutex<PinyinEngine>>,
}

#[interface(name = "org.freedesktop.IBus.Engine")]
impl FeibaiEngine {
    #[zbus(name = "ProcessKeyEvent")]
    async fn process_key_event(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        keyval: u32,
        _keycode: u32,
        state: u32,
    ) -> zbus::fdo::Result<bool> {
        let is_release = (state & (1 << 30)) != 0;
        if is_release {
            return Ok(false);
        }

        let key_event = ibus_to_key_event(keyval, state);
        let mut engine = self.engine.lock().await;
        let actions = engine.process_key(&key_event);

        let mut consumed = false;
        for action in actions {
            match action {
                EngineAction::Commit(text) => {
                    let v = ibus_text(&text);
                    Self::commit_text(&emitter, v.try_into().unwrap()).await.ok();
                    Self::hide_preedit_text(&emitter).await.ok();
                    Self::hide_lookup_table(&emitter).await.ok();
                    consumed = true;
                }
                EngineAction::UpdatePreedit(text) => {
                    let v = ibus_text(&text);
                    let cursor = text.len() as u32;
                    Self::update_preedit_text(
                        &emitter,
                        v.try_into().unwrap(),
                        cursor,
                        !text.is_empty(),
                        0u32,
                    )
                    .await
                    .ok();
                    consumed = true;
                }
                EngineAction::UpdateCandidates(candidates) => {
                    if candidates.is_empty() {
                        Self::hide_lookup_table(&emitter).await.ok();
                    } else {
                        let v = ibus_lookup_table(&candidates);
                        Self::update_lookup_table(&emitter, v.try_into().unwrap(), true)
                            .await
                            .ok();
                    }
                    consumed = true;
                }
                EngineAction::Forward => {}
                EngineAction::Noop => {
                    consumed = true;
                }
            }
        }

        Ok(consumed)
    }

    #[zbus(name = "FocusIn")]
    async fn focus_in(&self) {}

    #[zbus(name = "FocusOut")]
    async fn focus_out(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        let mut engine = self.engine.lock().await;
        engine.reset();
        Self::hide_preedit_text(&emitter).await.ok();
        Self::hide_lookup_table(&emitter).await.ok();
    }

    #[zbus(name = "Reset")]
    async fn reset(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        let mut engine = self.engine.lock().await;
        engine.reset();
        Self::hide_preedit_text(&emitter).await.ok();
        Self::hide_lookup_table(&emitter).await.ok();
    }

    #[zbus(name = "Enable")]
    async fn enable(&self) {}

    #[zbus(name = "Disable")]
    async fn disable(&self) {}

    #[zbus(name = "SetCursorLocation")]
    async fn set_cursor_location(&self, _x: i32, _y: i32, _w: i32, _h: i32) {}

    #[zbus(name = "SetCapabilities")]
    async fn set_capabilities(&self, _caps: u32) {}

    #[zbus(name = "PageUp")]
    async fn page_up(&self) {}

    #[zbus(name = "PageDown")]
    async fn page_down(&self) {}

    #[zbus(name = "CursorUp")]
    async fn cursor_up(&self) {}

    #[zbus(name = "CursorDown")]
    async fn cursor_down(&self) {}

    #[zbus(name = "CandidateClicked")]
    async fn candidate_clicked(&self, _index: u32, _button: u32, _state: u32) {}

    // Signals
    #[zbus(signal, name = "CommitText")]
    async fn commit_text(
        emitter: &SignalEmitter<'_>,
        text: zvariant::OwnedValue,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "UpdatePreeditText")]
    async fn update_preedit_text(
        emitter: &SignalEmitter<'_>,
        text: zvariant::OwnedValue,
        cursor_pos: u32,
        visible: bool,
        mode: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "HidePreeditText")]
    async fn hide_preedit_text(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal, name = "UpdateLookupTable")]
    async fn update_lookup_table(
        emitter: &SignalEmitter<'_>,
        table: zvariant::OwnedValue,
        visible: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "HideLookupTable")]
    async fn hide_lookup_table(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

// IBus Factory implementation

struct FeibaiFactory {
    engine: Arc<Mutex<PinyinEngine>>,
    engine_count: Arc<Mutex<u32>>,
    conn: connection::Connection,
}

#[interface(name = "org.freedesktop.IBus.Factory")]
impl FeibaiFactory {
    #[zbus(name = "CreateEngine")]
    async fn create_engine(&self, name: &str) -> zbus::fdo::Result<zvariant::OwnedObjectPath> {
        eprintln!("[feibai] CreateEngine called: {}", name);

        let mut count = self.engine_count.lock().await;
        *count += 1;
        let path = format!("/org/freedesktop/IBus/Engine/{}", *count);

        let engine_obj = FeibaiEngine {
            engine: self.engine.clone(),
        };

        self.conn
            .object_server()
            .at(path.as_str(), engine_obj)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        eprintln!("[feibai] engine created at {}", path);
        Ok(zvariant::OwnedObjectPath::try_from(path).unwrap())
    }
}

fn get_ibus_address() -> Option<String> {
    // Check IBUS_ADDRESS env var first
    if let Ok(addr) = std::env::var("IBUS_ADDRESS") {
        return Some(addr);
    }

    // Read from ~/.config/ibus/bus/<machine-id>-unix-<display>
    let machine_id = std::fs::read_to_string("/etc/machine-id")
        .or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id"))
        .ok()?;
    let machine_id = machine_id.trim();

    let display = std::env::var("DISPLAY")
        .or_else(|_| std::env::var("WAYLAND_DISPLAY"))
        .unwrap_or_else(|_| ":0".to_string());
    // Normalize display: ":0" -> "unix-0", ":0.0" -> "unix-0"
    let display_norm = display
        .trim_start_matches(':')
        .split('.')
        .next()
        .unwrap_or("0");

    let bus_dir = dirs::config_dir()?.join("ibus/bus");
    let filename = format!("{}-unix-{}", machine_id, display_norm);
    let bus_file = bus_dir.join(&filename);

    let content = std::fs::read_to_string(&bus_file).ok()?;
    for line in content.lines() {
        if let Some(addr) = line.strip_prefix("IBUS_ADDRESS=") {
            return Some(addr.to_string());
        }
    }
    None
}

fn ibus_to_key_event(keyval: u32, state: u32) -> KeyEvent {
    KeyEvent {
        keysym: keyval,
        unicode: char::from_u32(keyval).filter(|c| c.is_ascii_graphic() || *c == ' '),
        modifiers: Modifiers {
            ctrl: (state & 4) != 0,
            shift: (state & 1) != 0,
            alt: (state & 8) != 0,
            super_: (state & 0x40) != 0,
        },
        state: KeyState::Press,
    }
}

pub async fn run_ibus(engine: PinyinEngine) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[feibai] starting IBus engine mode");

    let addr = get_ibus_address().ok_or("cannot find IBus address. Is ibus-daemon running?")?;
    eprintln!("[feibai] IBus address: {}", addr);

    let conn = connection::Builder::address(addr.as_str())?
        .build()
        .await?;

    eprintln!("[feibai] connected to IBus bus");

    let engine = Arc::new(Mutex::new(engine));

    let factory = FeibaiFactory {
        engine: engine.clone(),
        engine_count: Arc::new(Mutex::new(0)),
        conn: conn.clone(),
    };

    conn.object_server()
        .at("/org/freedesktop/IBus/Factory", factory)
        .await?;

    // Request our bus name
    conn.request_name("org.freedesktop.IBus.Feibai")
        .await?;

    eprintln!("[feibai] IBus factory registered, waiting for CreateEngine...");

    // Keep running
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
