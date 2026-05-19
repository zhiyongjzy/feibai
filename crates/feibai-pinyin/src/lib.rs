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

    fn make_engine() -> PinyinEngine {
        PinyinEngine::from_file("../../data/pinyin_table.tsv").unwrap()
    }

    #[test]
    fn test_type_pinyin_shows_candidates() {
        let mut engine = make_engine();
        let actions = press_key(&mut engine, 'n');
        assert!(actions.iter().any(|a| matches!(a, EngineAction::UpdatePreedit(s) if s == "n")));

        let actions = press_key(&mut engine, 'i');
        assert!(actions
            .iter()
            .any(|a| matches!(a, EngineAction::UpdateCandidates(c) if !c.is_empty())));
    }

    #[test]
    fn test_space_commits_first() {
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
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
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
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
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
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
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
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
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');
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
        let mut engine = make_engine();
        let key = KeyEvent {
            keysym: 'c' as u32,
            unicode: Some('c'),
            modifiers: Modifiers {
                ctrl: true,
                ..Default::default()
            },
            state: KeyState::Press,
        };
        let actions = engine.process_key(&key);
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Forward)));
    }

    const KEYSYM_SHIFT_L: u32 = 0xffe1;

    fn shift_press(engine: &mut PinyinEngine) -> Vec<EngineAction> {
        engine.process_key(&KeyEvent {
            keysym: KEYSYM_SHIFT_L,
            unicode: None,
            modifiers: Modifiers::default(),
            state: KeyState::Press,
        })
    }

    fn shift_release(engine: &mut PinyinEngine) -> Vec<EngineAction> {
        engine.process_key(&KeyEvent {
            keysym: KEYSYM_SHIFT_L,
            unicode: None,
            modifiers: Modifiers::default(),
            state: KeyState::Release,
        })
    }

    #[test]
    fn test_shift_toggles_chinese_english() {
        let mut engine = make_engine();
        assert!(engine.is_chinese_mode());

        // Shift press + release alone = toggle to english
        shift_press(&mut engine);
        shift_release(&mut engine);
        assert!(!engine.is_chinese_mode());

        // Again = toggle back to chinese
        shift_press(&mut engine);
        shift_release(&mut engine);
        assert!(engine.is_chinese_mode());
    }

    #[test]
    fn test_english_mode_forwards_all() {
        let mut engine = make_engine();

        // Switch to english
        shift_press(&mut engine);
        shift_release(&mut engine);
        assert!(!engine.is_chinese_mode());

        // All letter keys forward
        let actions = press_key(&mut engine, 'a');
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Forward)));

        let actions = press_key(&mut engine, 'z');
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Forward)));
    }

    #[test]
    fn test_shift_with_other_key_does_not_toggle() {
        let mut engine = make_engine();
        assert!(engine.is_chinese_mode());

        // Shift press, then another key, then shift release = no toggle
        shift_press(&mut engine);
        press_key(&mut engine, 'a'); // interrupts shift-alone
        shift_release(&mut engine);

        // Should still be chinese mode
        assert!(engine.is_chinese_mode());
    }

    #[test]
    fn test_switch_to_english_commits_preedit() {
        let mut engine = make_engine();
        press_key(&mut engine, 'n');
        press_key(&mut engine, 'i');

        // Switch to english — should commit "ni" as raw text
        shift_press(&mut engine);
        let actions = shift_release(&mut engine);
        assert!(actions.iter().any(|a| matches!(a, EngineAction::Commit(s) if s == "ni")));
        assert!(!engine.is_chinese_mode());
    }
}
