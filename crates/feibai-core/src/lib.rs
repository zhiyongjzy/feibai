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

// --- Logging macros ---

/// Check if debug logging is enabled (FEIBAI_DEBUG=1 or debug build)
#[inline]
pub fn debug_enabled() -> bool {
    cfg!(debug_assertions) || {
        use std::sync::atomic::{AtomicU8, Ordering};
        // 0 = unchecked, 1 = disabled, 2 = enabled
        static STATE: AtomicU8 = AtomicU8::new(0);
        match STATE.load(Ordering::Relaxed) {
            2 => true,
            1 => false,
            _ => {
                let val = std::env::var("FEIBAI_DEBUG").is_ok_and(|v| v == "1");
                STATE.store(if val { 2 } else { 1 }, Ordering::Relaxed);
                val
            }
        }
    }
}

/// Format a timestamp prefix for log lines (local time)
#[inline]
pub fn log_timestamp() -> String {
    // Use libc localtime to get proper local time without extra deps
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    #[cfg(unix)]
    {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&secs, &mut tm) };
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
    #[cfg(not(unix))]
    {
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        let s = secs % 60;
        format!("{:02}:{:02}:{:02}", h, m, s)
    }
}

/// Always emits (errors, startup, important state changes)
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        eprintln!("[feibai {}] {}", $crate::log_timestamp(), format_args!($($arg)*))
    };
}

/// Always emits for errors
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        eprintln!("[feibai {} ERROR] {}", $crate::log_timestamp(), format_args!($($arg)*))
    };
}

/// Only emits in debug builds or when FEIBAI_DEBUG=1
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        if $crate::debug_enabled() {
            eprintln!("[feibai {} DBG] {}", $crate::log_timestamp(), format_args!($($arg)*))
        }
    };
}
