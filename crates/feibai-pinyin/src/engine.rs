use std::collections::HashMap;
use std::fs;
use std::path::Path;

use feibai_core::*;

pub struct PinyinEngine {
    table: HashMap<String, Vec<String>>,
    preedit: String,
    candidates: Vec<Candidate>,
    chinese_mode: bool,
    shift_pressed_alone: bool,
}

impl PinyinEngine {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, String> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| format!("failed to read pinyin table: {e}"))?;
        let mut table: HashMap<String, Vec<String>> = HashMap::new();
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
                table.entry(pinyin).or_default().extend(chars);
            }
        }
        // Deduplicate
        for chars in table.values_mut() {
            let mut seen = std::collections::HashSet::new();
            chars.retain(|c| seen.insert(c.clone()));
        }
        Ok(Self {
            table,
            preedit: String::new(),
            candidates: Vec::new(),
            chinese_mode: true,
            shift_pressed_alone: false,
        })
    }

    fn lookup(&mut self) {
        self.candidates = self
            .table
            .get(&self.preedit)
            .map(|chars| {
                chars
                    .iter()
                    .map(|c| Candidate {
                        text: c.clone(),
                        comment: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
    }

    pub fn is_chinese_mode(&self) -> bool {
        self.chinese_mode
    }
}

const KEYSYM_SHIFT_L: u32 = 0xffe1;
const KEYSYM_SHIFT_R: u32 = 0xffe2;

impl Engine for PinyinEngine {
    fn process_key(&mut self, key: &KeyEvent) -> Vec<EngineAction> {
        let keysym = key.keysym;

        // Track Shift for toggle: press alone (no other key in between) = toggle
        if keysym == KEYSYM_SHIFT_L || keysym == KEYSYM_SHIFT_R {
            if key.state == KeyState::Press {
                self.shift_pressed_alone = true;
                return vec![EngineAction::Noop];
            } else {
                // Release — if no other key was pressed, toggle mode
                if self.shift_pressed_alone {
                    self.shift_pressed_alone = false;
                    self.chinese_mode = !self.chinese_mode;
                    // If switching to english, clear preedit
                    if !self.chinese_mode && !self.preedit.is_empty() {
                        let text = self.preedit.clone();
                        self.preedit.clear();
                        self.candidates.clear();
                        return vec![
                            EngineAction::Commit(text),
                            EngineAction::UpdatePreedit(String::new()),
                            EngineAction::UpdateCandidates(Vec::new()),
                        ];
                    }
                    return vec![EngineAction::Noop];
                }
                return vec![EngineAction::Noop];
            }
        }

        // Any non-shift key press cancels the "shift alone" tracking
        if key.state == KeyState::Press {
            self.shift_pressed_alone = false;
        }

        if key.state == KeyState::Release {
            return vec![EngineAction::Noop];
        }

        // English mode — forward everything
        if !self.chinese_mode {
            return vec![EngineAction::Forward];
        }

        if key.modifiers.ctrl || key.modifiers.alt || key.modifiers.super_ {
            return vec![EngineAction::Forward];
        }

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
        if let Some(ch) = key.unicode
            && ('1'..='9').contains(&ch)
            && !self.candidates.is_empty()
        {
            let idx = (ch as usize) - ('1' as usize);
            if let Some(c) = self.candidates.get(idx) {
                let text = c.text.clone();
                self.preedit.clear();
                self.candidates.clear();
                return vec![EngineAction::Commit(text)];
            }
            return vec![EngineAction::Noop];
        }

        // Letter keys — append to preedit
        if let Some(ch) = key.unicode
            && ch.is_ascii_lowercase()
        {
            self.preedit.push(ch);
            self.lookup();
            return vec![
                EngineAction::UpdatePreedit(self.preedit.clone()),
                EngineAction::UpdateCandidates(self.candidates.clone()),
            ];
        }

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
