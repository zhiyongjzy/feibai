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
}
