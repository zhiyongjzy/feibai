use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum, event_created_child};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use feibai_core::*;
use feibai_pinyin::PinyinEngine;

struct State {
    im_manager: Option<ZwpInputMethodManagerV2>,
    input_method: Option<ZwpInputMethodV2>,
    seat: Option<wl_seat::WlSeat>,
    engine: PinyinEngine,
    serial: u32,
    active: bool,
    xkb_context: xkbcommon::xkb::Context,
    xkb_state: Option<xkbcommon::xkb::State>,
    xkb_keymap: Option<xkbcommon::xkb::Keymap>,
}

impl State {
    fn handle_engine_actions(&self, actions: Vec<EngineAction>) {
        let im = match &self.input_method {
            Some(im) => im,
            None => return,
        };

        for action in &actions {
            match action {
                EngineAction::Commit(text) => {
                    im.commit_string(text.clone());
                    im.set_preedit_string(String::new(), 0, 0);
                    im.commit(self.serial);
                    eprintln!("[feibai] commit: {}", text);
                }
                EngineAction::UpdatePreedit(text) => {
                    let len = text.len() as i32;
                    im.set_preedit_string(text.clone(), len, len);
                    im.commit(self.serial);
                    eprintln!("[feibai] preedit: {}", text);
                }
                EngineAction::UpdateCandidates(candidates) => {
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
                        name,
                        version.min(1),
                        qh,
                        (),
                    );
                    state.im_manager = Some(mgr);
                    eprintln!("[feibai] bound zwp_input_method_manager_v2");
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
                im.grab_keyboard(qh, ());
            }
            zwp_input_method_v2::Event::Deactivate => {
                eprintln!("[feibai] deactivate");
                state.active = false;
                state.engine.reset();
            }
            zwp_input_method_v2::Event::Done => {
                state.serial += 1;
            }
            _ => {}
        }
    }

    event_created_child!(State, ZwpInputMethodV2, [
        zwp_input_method_v2::EVT_ACTIVATE_OPCODE => (ZwpInputMethodKeyboardGrabV2, ()),
    ]);
}

// --- zwp_input_method_keyboard_grab_v2 ---

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for State {
    fn event(
        state: &mut Self,
        _grab: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Keymap { format, fd, size } => {
                if format != WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) {
                    return;
                }
                let keymap = unsafe {
                    xkbcommon::xkb::Keymap::new_from_fd(
                        &state.xkb_context,
                        fd,
                        size as usize,
                        xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                }.ok().flatten();
                if let Some(keymap) = keymap {
                    let xkb_state = xkbcommon::xkb::State::new(&keymap);
                    state.xkb_keymap = Some(keymap);
                    state.xkb_state = Some(xkb_state);
                    eprintln!("[feibai] keymap loaded");
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Key {
                serial: _,
                time: _,
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

                let keycode = xkbcommon::xkb::Keycode::new(key + 8); // evdev offset
                let keysym = xkb_state.key_get_one_sym(keycode);
                let utf32 = xkb_state.key_get_utf32(keycode);
                let unicode = char::from_u32(utf32).filter(|c| !c.is_control());

                let pressed = key_state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                let ks = if pressed { KeyState::Press } else { KeyState::Release };

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

                let actions = state.engine.process_key(&key_event);
                state.handle_engine_actions(actions);
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
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    let pinyin_paths = [
        "data/pinyin_table.tsv",
        "/usr/share/feibai/pinyin_table.tsv",
        "/usr/local/share/feibai/pinyin_table.tsv",
    ];
    let engine = pinyin_paths
        .iter()
        .find_map(|p| PinyinEngine::from_file(p).ok())
        .expect("cannot find pinyin_table.tsv");

    let mut state = State {
        im_manager: None,
        input_method: None,
        seat: None,
        engine,
        serial: 0,
        active: false,
        xkb_context: xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS),
        xkb_state: None,
        xkb_keymap: None,
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

    event_queue.roundtrip(&mut state)?;

    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    WaylandSource::new(conn, event_queue).insert(event_loop.handle())?;

    eprintln!("[feibai] running");
    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}
