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
