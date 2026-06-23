//! Integration tests — simulate full input flows end-to-end.

use feibai_core::*;
use feibai_pinyin::PinyinEngine;

const BASE_DICT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/dicts/feibai.base.dict.yaml");
const EXTRA_DICT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/dicts/feibai.extra.dict.yaml");

fn make_engine() -> PinyinEngine {
    PinyinEngine::from_files(&[BASE_DICT, EXTRA_DICT]).unwrap()
}

fn press(engine: &mut PinyinEngine, c: char) -> Vec<EngineAction> {
    engine.process_key(&KeyEvent {
        keysym: c as u32,
        unicode: Some(c),
        modifiers: Modifiers::default(),
        state: KeyState::Press,
    })
}

fn press_sym(engine: &mut PinyinEngine, keysym: u32) -> Vec<EngineAction> {
    engine.process_key(&KeyEvent {
        keysym,
        unicode: None,
        modifiers: Modifiers::default(),
        state: KeyState::Press,
    })
}

fn press_space(engine: &mut PinyinEngine) -> Vec<EngineAction> {
    engine.process_key(&KeyEvent {
        keysym: 0x20,
        unicode: Some(' '),
        modifiers: Modifiers::default(),
        state: KeyState::Press,
    })
}

fn shift_toggle(engine: &mut PinyinEngine) -> Vec<EngineAction> {
    const SHIFT_L: u32 = 0xffe1;
    engine.process_key(&KeyEvent {
        keysym: SHIFT_L,
        unicode: None,
        modifiers: Modifiers::default(),
        state: KeyState::Press,
    });
    engine.process_key(&KeyEvent {
        keysym: SHIFT_L,
        unicode: None,
        modifiers: Modifiers::default(),
        state: KeyState::Release,
    })
}

fn collect_commits(actions: &[EngineAction]) -> String {
    actions
        .iter()
        .filter_map(|a| match a {
            EngineAction::Commit(s) => Some(s.as_str()),
            _ => None,
        })
        .collect()
}

fn last_candidates(actions: &[EngineAction]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|a| match a {
            EngineAction::UpdateCandidates(c) => Some(c.iter().map(|x| x.text.clone()).collect()),
            _ => None,
        })
        .last()
        .unwrap_or_default()
}

// --- Full sentence input flows ---

#[test]
fn flow_type_nihao_and_commit() {
    let mut engine = make_engine();
    for c in "nihao".chars() {
        press(&mut engine, c);
    }
    let actions = press_space(&mut engine);
    let committed = collect_commits(&actions);
    assert_eq!(committed, "你好");
}

#[test]
fn flow_type_jintian_and_commit() {
    let mut engine = make_engine();
    for c in "jintian".chars() {
        press(&mut engine, c);
    }
    let actions = press_space(&mut engine);
    let committed = collect_commits(&actions);
    assert_eq!(committed, "今天");
}

#[test]
fn flow_digit_selection() {
    let mut engine = make_engine();
    for c in "ni".chars() {
        press(&mut engine, c);
    }
    // Select 2nd candidate with '2'
    let actions = press(&mut engine, '2');
    let committed = collect_commits(&actions);
    assert_eq!(committed, "泥");
}

#[test]
fn flow_escape_clears_state() {
    let mut engine = make_engine();
    for c in "zhongguo".chars() {
        press(&mut engine, c);
    }
    let actions = press_sym(&mut engine, 0xff1b); // Escape
    assert!(actions.iter().any(|a| matches!(a, EngineAction::UpdatePreedit(s) if s.is_empty())));
    assert!(actions
        .iter()
        .any(|a| matches!(a, EngineAction::UpdateCandidates(c) if c.is_empty())));
}

#[test]
fn flow_enter_commits_raw_pinyin() {
    let mut engine = make_engine();
    for c in "beijing".chars() {
        press(&mut engine, c);
    }
    let actions = press_sym(&mut engine, 0xff0d); // Enter
    let committed = collect_commits(&actions);
    assert_eq!(committed, "beijing");
}

#[test]
fn flow_backspace_then_commit() {
    let mut engine = make_engine();
    // Type "nia" then backspace to get "ni", then commit
    for c in "nia".chars() {
        press(&mut engine, c);
    }
    press_sym(&mut engine, 0xff08); // Backspace
    let actions = press_space(&mut engine);
    let committed = collect_commits(&actions);
    assert_eq!(committed, "你");
}

#[test]
fn flow_chinese_english_toggle() {
    let mut engine = make_engine();
    assert!(engine.is_chinese_mode());

    // Switch to English
    shift_toggle(&mut engine);
    assert!(!engine.is_chinese_mode());

    // In English mode, keys should forward
    let actions = press(&mut engine, 'h');
    assert!(actions.iter().any(|a| matches!(a, EngineAction::Forward)));

    // Switch back to Chinese
    shift_toggle(&mut engine);
    assert!(engine.is_chinese_mode());

    // Now typing should produce preedit
    let actions = press(&mut engine, 'h');
    assert!(actions
        .iter()
        .any(|a| matches!(a, EngineAction::UpdatePreedit(s) if s == "h")));
}

#[test]
fn flow_toggle_with_pending_commits_raw() {
    let mut engine = make_engine();
    for c in "wo".chars() {
        press(&mut engine, c);
    }
    // Shift toggle should commit raw preedit
    let actions = shift_toggle(&mut engine);
    let committed = collect_commits(&actions);
    assert_eq!(committed, "wo");
    assert!(!engine.is_chinese_mode());
}

#[test]
fn flow_long_sentence_viterbi() {
    let mut engine = make_engine();
    // Type "wohenhaode" — should produce a multi-word sentence via Viterbi
    for c in "wohenhao".chars() {
        press(&mut engine, c);
    }
    let actions = press_space(&mut engine);
    let committed = collect_commits(&actions);
    // Viterbi should produce a multi-char result (e.g. "我很好")
    assert!(
        committed.chars().count() >= 2,
        "expected multi-char sentence for 'wohenhao', got: {committed}"
    );
}

#[test]
fn flow_page_navigation() {
    let mut engine = make_engine();
    // "yi" has many candidates
    press(&mut engine, 'y');
    let actions = press(&mut engine, 'i');
    let page0 = last_candidates(&actions);
    assert!(!page0.is_empty(), "yi should have candidates");

    // Page down with '='
    let actions = press(&mut engine, '=');
    let page1 = last_candidates(&actions);

    // "yi" has enough candidates for multiple pages
    if !page1.is_empty() {
        assert_ne!(page0, page1, "page down should show different candidates");
    }
}

#[test]
fn flow_multiple_sentences() {
    let mut engine = make_engine();

    // First sentence: "你好"
    for c in "nihao".chars() {
        press(&mut engine, c);
    }
    let actions = press_space(&mut engine);
    assert_eq!(collect_commits(&actions), "你好");

    // Second sentence: "今天"
    for c in "jintian".chars() {
        press(&mut engine, c);
    }
    let actions = press_space(&mut engine);
    assert_eq!(collect_commits(&actions), "今天");
}

#[test]
fn flow_reset_clears_everything() {
    let mut engine = make_engine();
    for c in "hello".chars() {
        press(&mut engine, c);
    }
    engine.reset();
    // After reset, typing should start fresh
    let actions = press(&mut engine, 'n');
    assert!(actions
        .iter()
        .any(|a| matches!(a, EngineAction::UpdatePreedit(s) if s == "n")));
}

#[test]
fn flow_ctrl_key_forwards() {
    let mut engine = make_engine();
    let actions = engine.process_key(&KeyEvent {
        keysym: 'c' as u32,
        unicode: Some('c'),
        modifiers: Modifiers {
            ctrl: true,
            ..Default::default()
        },
        state: KeyState::Press,
    });
    assert!(actions.iter().any(|a| matches!(a, EngineAction::Forward)));
}

#[test]
fn flow_non_alpha_in_chinese_mode_forwards() {
    let mut engine = make_engine();
    // Without any preedit, non-alpha keys should forward
    let actions = engine.process_key(&KeyEvent {
        keysym: '/' as u32,
        unicode: Some('/'),
        modifiers: Modifiers::default(),
        state: KeyState::Press,
    });
    assert!(actions.iter().any(|a| matches!(a, EngineAction::Forward)));
}


#[test]
fn flow_punctuation_commits_preedit_then_forwards() {
    let mut engine = make_engine();
    // Type "ni" to get candidates
    press(&mut engine, 'n');
    press(&mut engine, 'i');
    // Now press comma — should commit first candidate AND forward the punctuation
    let actions = press(&mut engine, ',');
    let has_commit = actions.iter().any(|a| matches!(a, EngineAction::Commit(_)));
    let has_forward = actions.iter().any(|a| matches!(a, EngineAction::Forward));
    assert!(has_commit, "expected Commit action for preedit, got: {:?}", actions);
    assert!(has_forward, "expected Forward action for punctuation, got: {:?}", actions);
}
