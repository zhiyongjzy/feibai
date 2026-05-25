mod ibus;
mod popup;

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{
    delegate_noop, Connection, Dispatch, QueueHandle, WEnum,
};
use std::io::Write;
use std::os::unix::io::AsFd;
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use feibai_core::*;
use feibai_pinyin::PinyinEngine;
use feibai_ui::Theme;

use popup::PopupWindow;

pub struct State {
    im_manager: Option<ZwpInputMethodManagerV2>,
    vk_manager: Option<ZwpVirtualKeyboardManagerV1>,
    virtual_keyboard: Option<ZwpVirtualKeyboardV1>,
    input_method: Option<ZwpInputMethodV2>,
    seat: Option<wl_seat::WlSeat>,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    engine: PinyinEngine,
    popup: PopupWindow,
    theme: Theme,
    serial: u32,
    active: bool,
    preedit_text: String,
    candidates: Vec<Candidate>,
    xkb_context: xkbcommon::xkb::Context,
    xkb_state: Option<xkbcommon::xkb::State>,
    xkb_keymap: Option<xkbcommon::xkb::Keymap>,
    vk_keymap_fd: Option<std::fs::File>,
    forwarded_keys: std::collections::HashSet<u32>,
}

impl State {
    fn handle_engine_actions(&mut self, actions: Vec<EngineAction>, qh: &QueueHandle<State>) {
        let im = match &self.input_method {
            Some(im) => im.clone(),
            None => return,
        };

        let mut need_commit = false;

        for action in &actions {
            match action {
                EngineAction::Commit(text) => {
                    im.commit_string(text.clone());
                    im.set_preedit_string(String::new(), 0, 0);
                    self.preedit_text.clear();
                    self.candidates.clear();
                    self.popup.hide();
                    need_commit = true;
                    eprintln!("[feibai] commit: {}", text);
                }
                EngineAction::UpdatePreedit(text) => {
                    let len = text.len() as i32;
                    im.set_preedit_string(text.clone(), len, len);
                    self.preedit_text = text.clone();
                    self.update_popup(qh);
                    need_commit = true;
                    eprintln!("[feibai] preedit: {}", text);
                }
                EngineAction::UpdateCandidates(candidates) => {
                    self.candidates = candidates.clone();
                    self.update_popup(qh);
                    if !candidates.is_empty() {
                        eprint!("[feibai] candidates:");
                        for (i, c) in candidates.iter().take(9).enumerate() {
                            eprint!(" {}:{}", i + 1, c.text);
                        }
                        eprintln!();
                    }
                }
                EngineAction::Forward => {}
                EngineAction::Noop => {}
            }
        }

        if need_commit {
            im.commit(self.serial);
        }
    }

    fn update_popup(&mut self, qh: &QueueHandle<State>) {
        if self.candidates.is_empty() && self.preedit_text.is_empty() {
            self.popup.hide();
        } else if let Some(shm) = &self.shm {
            let shm = shm.clone();
            self.popup.show(&self.preedit_text, &self.candidates, 0, &shm, qh);
        }
    }
}

// --- wl_registry ---

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_seat" => {
                    let seat = registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(1), qh, ());
                    state.seat = Some(seat);
                    eprintln!("[feibai] bound wl_seat v{}", version);
                }
                "zwp_input_method_manager_v2" => {
                    let mgr = registry.bind::<ZwpInputMethodManagerV2, _, _>(
                        name, version.min(1), qh, (),
                    );
                    state.im_manager = Some(mgr);
                    eprintln!("[feibai] bound zwp_input_method_manager_v2");
                }
                "wl_compositor" => {
                    let comp = registry.bind::<wl_compositor::WlCompositor, _, _>(
                        name, version.min(4), qh, (),
                    );
                    state.compositor = Some(comp);
                    eprintln!("[feibai] bound wl_compositor v{}", version);
                }
                "wl_shm" => {
                    let shm = registry.bind::<wl_shm::WlShm, _, _>(name, version.min(1), qh, ());
                    state.shm = Some(shm);
                    eprintln!("[feibai] bound wl_shm");
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    let mgr = registry.bind::<ZwpVirtualKeyboardManagerV1, _, _>(
                        name, version.min(1), qh, (),
                    );
                    state.vk_manager = Some(mgr);
                    eprintln!("[feibai] bound zwp_virtual_keyboard_manager_v1");
                }
                _ => {}
            }
        }
    }
}

// --- wl_seat ---

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _state: &mut Self,
        _seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Name { name } = event {
            eprintln!("[feibai] seat name: {}", name);
        }
    }
}

// --- zwp_input_method_manager_v2 ---

delegate_noop!(State: ignore ZwpInputMethodManagerV2);

// --- wl_compositor ---

delegate_noop!(State: ignore wl_compositor::WlCompositor);

// --- wl_shm ---

delegate_noop!(State: ignore wl_shm::WlShm);

// --- virtual keyboard ---

delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);

// --- wl_shm_pool ---

delegate_noop!(State: ignore wl_shm_pool::WlShmPool);

// --- wl_buffer ---

delegate_noop!(State: ignore wl_buffer::WlBuffer);

// --- wl_surface ---

delegate_noop!(State: ignore wl_surface::WlSurface);

// --- zwp_input_popup_surface_v2 ---

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for State {
    fn event(
        _state: &mut Self,
        _popup: &ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwp_input_popup_surface_v2::Event::TextInputRectangle { x, y, width, height } = event
        {
            eprintln!(
                "[feibai] popup text_input_rectangle: {}x{} at ({},{})",
                width, height, x, y
            );
        }
    }
}

// --- zwp_input_method_v2 ---

impl Dispatch<ZwpInputMethodV2, ()> for State {
    fn event(
        state: &mut Self,
        im: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => {
                eprintln!("[feibai] activate");
                state.active = true;
                state.engine.reset();
                state.preedit_text.clear();
                state.candidates.clear();
                im.grab_keyboard(qh, ());

                // Create popup surface
                if let Some(compositor) = &state.compositor {
                    let compositor = compositor.clone();
                    state.popup.create_surface(&compositor, im, qh);
                }
            }
            zwp_input_method_v2::Event::Deactivate => {
                eprintln!("[feibai] deactivate");
                state.active = false;
                state.engine.reset();
                state.preedit_text.clear();
                state.candidates.clear();
                state.popup.destroy();
                state.forwarded_keys.clear();
            }
            zwp_input_method_v2::Event::Done => {
                state.serial += 1;
            }
            _ => {}
        }
    }

}

// --- zwp_input_method_keyboard_grab_v2 ---

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for State {
    fn event(
        state: &mut Self,
        _grab: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Keymap { format, fd, size } => {
                if format != WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) {
                    eprintln!("[feibai] keymap: unsupported format {:?}", format);
                    return;
                }
                eprintln!("[feibai] keymap event: size={}", size);

                // Parse keymap with xkb first
                let keymap = unsafe {
                    xkbcommon::xkb::Keymap::new_from_fd(
                        &state.xkb_context,
                        fd,
                        size as usize,
                        xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                }
                .ok()
                .flatten();
                if let Some(keymap) = keymap {
                    // Create independent memfd for virtual keyboard
                    // (can't reuse original fd — shared file offset would be at EOF after xkb read)
                    if let Some(vk) = &state.virtual_keyboard {
                        let keymap_str = keymap.get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1);
                        let data = keymap_str.as_bytes();
                        let vk_size = data.len() + 1; // null terminator required
                        if let Ok(memfd) = rustix::fs::memfd_create(
                            "feibai-keymap",
                            rustix::fs::MemfdFlags::CLOEXEC,
                        ) {
                            let mut file = std::fs::File::from(memfd);
                            let _ = rustix::fs::ftruncate(&file, vk_size as u64);
                            let _ = file.write_all(data);
                            let _ = file.write_all(&[0]);
                            // Seek not needed — compositor uses mmap from offset 0
                            vk.keymap(
                                wl_keyboard::KeymapFormat::XkbV1 as u32,
                                file.as_fd(),
                                vk_size as u32,
                            );
                            state.vk_keymap_fd = Some(file);
                            eprintln!("[feibai] vk keymap set ({} bytes)", vk_size);
                        } else {
                            eprintln!("[feibai] ERROR: memfd_create failed");
                        }
                    }

                    let xkb_state = xkbcommon::xkb::State::new(&keymap);
                    state.xkb_keymap = Some(keymap);
                    state.xkb_state = Some(xkb_state);
                    eprintln!("[feibai] xkb keymap loaded");
                } else {
                    eprintln!("[feibai] ERROR: xkb keymap parse failed");
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Key {
                serial: _,
                time,
                key,
                state: key_state,
            } => {
                if !state.active {
                    return;
                }
                let xkb_state = match &state.xkb_state {
                    Some(s) => s,
                    None => return,
                };

                let keycode = xkbcommon::xkb::Keycode::new(key + 8);
                let keysym = xkb_state.key_get_one_sym(keycode);
                let utf32 = xkb_state.key_get_utf32(keycode);
                let unicode = char::from_u32(utf32).filter(|c| !c.is_control());

                let pressed = key_state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                let ks = if pressed { KeyState::Press } else { KeyState::Release };
                let raw_state = if pressed { 1u32 } else { 0u32 };

                let modifiers = Modifiers {
                    ctrl: xkb_state.mod_name_is_active(
                        xkbcommon::xkb::MOD_NAME_CTRL,
                        xkbcommon::xkb::STATE_MODS_EFFECTIVE,
                    ),
                    alt: xkb_state.mod_name_is_active(
                        xkbcommon::xkb::MOD_NAME_ALT,
                        xkbcommon::xkb::STATE_MODS_EFFECTIVE,
                    ),
                    shift: xkb_state.mod_name_is_active(
                        xkbcommon::xkb::MOD_NAME_SHIFT,
                        xkbcommon::xkb::STATE_MODS_EFFECTIVE,
                    ),
                    super_: xkb_state.mod_name_is_active(
                        xkbcommon::xkb::MOD_NAME_LOGO,
                        xkbcommon::xkb::STATE_MODS_EFFECTIVE,
                    ),
                };

                let key_event = KeyEvent {
                    keysym: keysym.raw(),
                    unicode,
                    modifiers,
                    state: ks,
                };

                // Ctrl+Shift+T: cycle theme
                if pressed && modifiers.ctrl && modifiers.shift && keysym.raw() == 0x54 {
                    // 0x54 = 'T'
                    state.theme = state.theme.next();
                    state.popup.set_theme(state.theme);
                    eprintln!("[feibai] theme switched to: {}", theme_name(state.theme));
                    // Re-render popup if visible
                    if !state.candidates.is_empty() {
                        let qh = qh.clone();
                        state.update_popup(&qh);
                    }
                    return;
                }

                // For key release: if we previously forwarded this key's press,
                // forward the release too (so application stops repeat)
                if !pressed && state.forwarded_keys.remove(&key) {
                    if let (Some(vk), Some(_)) = (&state.virtual_keyboard, &state.vk_keymap_fd) {
                        vk.key(time, key, 0);
                    }
                    // Still let engine see it (for shift tracking etc)
                    state.engine.process_key(&key_event);
                    return;
                }

                let actions = state.engine.process_key(&key_event);

                let should_forward = actions.iter().any(|a| matches!(a, EngineAction::Forward));

                if should_forward {
                    if let (Some(vk), Some(_)) = (&state.virtual_keyboard, &state.vk_keymap_fd) {
                        vk.key(time, key, raw_state);
                        if pressed {
                            state.forwarded_keys.insert(key);
                        }
                    }
                } else {
                    let qh = qh.clone();
                    state.handle_engine_actions(actions, &qh);
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Modifiers {
                serial: _,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                if let Some(xkb_state) = &mut state.xkb_state {
                    xkb_state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                // Forward modifiers to virtual keyboard
                if let (Some(vk), Some(_)) = (&state.virtual_keyboard, &state.vk_keymap_fd) {
                    vk.modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            }
            _ => {}
        }
    }
}

fn load_engine() -> PinyinEngine {
    let feibai_dir = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap().join(".config"))
        .join("feibai");
    eprintln!("[feibai] dir: {}", feibai_dir.display());

    // Scan for all *.dict.yaml in feibai dir and fallback locations
    // Convention: *.en.dict.yaml → English dict, others → Chinese pinyin dict
    let mut cn_paths: Vec<String> = Vec::new();
    let mut en_paths: Vec<String> = Vec::new();

    let base_in_dir = feibai_dir.join("feibai.base.dict.yaml");
    if base_in_dir.exists() {
        cn_paths.push(base_in_dir.to_string_lossy().to_string());
    }
    if let Ok(entries) = std::fs::read_dir(&feibai_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if !name.ends_with(".dict.yaml") { continue; }
                if name == "feibai.base.dict.yaml" { continue; }
                let p = path.to_string_lossy().to_string();
                if name.contains(".en.") {
                    en_paths.push(p);
                } else if !cn_paths.contains(&p) {
                    cn_paths.push(p);
                }
            }
        }
    }

    if cn_paths.is_empty() {
        let fallback_dirs = [
            "data/dicts",
            "/usr/share/feibai/dicts",
            "/usr/local/share/feibai/dicts",
        ];
        for dir in &fallback_dirs {
            let p = format!("{}/feibai.base.dict.yaml", dir);
            if std::path::Path::new(&p).exists() {
                cn_paths.push(p);
                break;
            }
        }
    }

    if cn_paths.is_empty() {
        eprintln!("[feibai] ERROR: cannot find any dict files in {}", feibai_dir.display());
        eprintln!("[feibai] place feibai.base.dict.yaml in ~/.config/feibai/");
        std::process::exit(1);
    }
    for p in &cn_paths {
        eprintln!("[feibai] loading dict: {}", p);
    }
    let mut engine = PinyinEngine::from_files(
        &cn_paths.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    )
    .expect("failed to load dicts");

    for p in &en_paths {
        eprintln!("[feibai] loading en dict: {}", p);
        if let Err(e) = engine.load_en_dict(p) {
            eprintln!("[feibai] WARNING: {}", e);
        }
    }

    let user_dict_path = feibai_dir.join("user.dict.txt");
    engine.set_userdb_path(&user_dict_path);
    if user_dict_path.exists() {
        eprintln!("[feibai] loading user dict: {}", user_dict_path.display());
        if let Err(e) = engine.load_userdb(&user_dict_path) {
            eprintln!("[feibai] WARNING: {}", e);
        }
    }

    engine
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // --ibus flag forces IBus mode (launched by ibus-daemon)
    if args.iter().any(|a| a == "--ibus") {
        let engine = load_engine();
        futures_lite::future::block_on(ibus::run_ibus(engine))?;
        return Ok(());
    }

    // Auto-detect: try Wayland input-method-v2 first, fall back to IBus
    let wayland_ok = Connection::connect_to_env().is_ok()
        && std::env::var("WAYLAND_DISPLAY").is_ok();

    if !wayland_ok {
        eprintln!("[feibai] no Wayland display, trying IBus mode");
        let engine = load_engine();
        futures_lite::future::block_on(ibus::run_ibus(engine))?;
        return Ok(());
    }

    // Wayland mode
    let conn = Connection::connect_to_env()?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    let engine = load_engine();

    let feibai_dir = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap().join(".config"))
        .join("feibai");
    let theme = load_theme_from_config(&feibai_dir);
    eprintln!("[feibai] theme: {:?}", theme_name(theme));

    let mut state = State {
        im_manager: None,
        vk_manager: None,
        virtual_keyboard: None,
        input_method: None,
        seat: None,
        compositor: None,
        shm: None,
        engine,
        popup: PopupWindow::new_with_theme(theme),
        theme,
        serial: 0,
        active: false,
        preedit_text: String::new(),
        candidates: Vec::new(),
        xkb_context: xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS),
        xkb_state: None,
        xkb_keymap: None,
        vk_keymap_fd: None,
        forwarded_keys: std::collections::HashSet::new(),
    };

    display.get_registry(&qh, ());
    event_queue.roundtrip(&mut state)?;

    // Create input method from manager + seat
    if let (Some(mgr), Some(seat)) = (&state.im_manager, &state.seat) {
        let im = mgr.get_input_method(seat, &qh, ());
        state.input_method = Some(im);
        eprintln!("[feibai] created input method");
    } else {
        eprintln!("[feibai] ERROR: compositor does not support input-method-v2 or no seat found");
        std::process::exit(1);
    }

    // Create virtual keyboard for forwarding keys
    if let (Some(vk_mgr), Some(seat)) = (&state.vk_manager, &state.seat) {
        let vk = vk_mgr.create_virtual_keyboard(seat, &qh, ());
        state.virtual_keyboard = Some(vk);
        eprintln!("[feibai] created virtual keyboard");
    } else {
        eprintln!("[feibai] WARNING: no virtual keyboard support, key forwarding disabled");
    }

    // Virtual keyboard needs a keymap before it can send keys
    // We'll set it when we receive the keymap from the grab

    event_queue.roundtrip(&mut state)?;

    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    WaylandSource::new(conn, event_queue).insert(event_loop.handle())?;

    eprintln!("[feibai] running");
    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

fn load_theme_from_config(config_dir: &std::path::Path) -> Theme {
    let config_path = config_dir.join("config.toml");
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(table) = content.parse::<toml::Table>() {
            if let Some(name) = table.get("theme").and_then(|v| v.as_str()) {
                return theme_from_name(name);
            }
        }
    }
    Theme::Light
}

fn theme_from_name(name: &str) -> Theme {
    match name.to_lowercase().as_str() {
        "dark" => Theme::Dark,
        "flat" => Theme::Flat,
        "blue" => Theme::Blue,
        "sakura" => Theme::Sakura,
        "ocean" => Theme::Ocean,
        "lavender" => Theme::Lavender,
        "tangerine" => Theme::Tangerine,
        "mint" => Theme::Mint,
        _ => Theme::Light,
    }
}

fn theme_name(theme: Theme) -> &'static str {
    match theme {
        Theme::Light => "light",
        Theme::Dark => "dark",
        Theme::Flat => "flat",
        Theme::Blue => "blue",
        Theme::Sakura => "sakura",
        Theme::Ocean => "ocean",
        Theme::Lavender => "lavender",
        Theme::Tangerine => "tangerine",
        Theme::Mint => "mint",
    }
}
