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
        // Date included: the process runs for weeks, time-only lines can't be
        // told apart across sessions
        format!(
            "{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_timestamp_contains_date_and_time() {
        let ts = log_timestamp();
        // Expected format: "MM-DD HH:MM:SS" (date needed to tell sessions apart
        // in a log that spans weeks of uptime)
        assert_eq!(ts.len(), 14, "expected 'MM-DD HH:MM:SS', got '{ts}'");
        let b = ts.as_bytes();
        assert_eq!(b[2], b'-', "date separator at pos 2, got '{ts}'");
        assert_eq!(b[5], b' ', "date/time separator at pos 5, got '{ts}'");
        assert_eq!(b[8], b':', "hour separator, got '{ts}'");
        assert_eq!(b[11], b':', "minute separator, got '{ts}'");

        let month: u32 = ts[0..2].parse().expect("month numeric");
        let day: u32 = ts[3..5].parse().expect("day numeric");
        let hour: u32 = ts[6..8].parse().expect("hour numeric");
        let min: u32 = ts[9..11].parse().expect("minute numeric");
        let sec: u32 = ts[12..14].parse().expect("second numeric");
        assert!((1..=12).contains(&month), "month out of range: {month} in '{ts}'");
        assert!((1..=31).contains(&day), "day out of range: {day} in '{ts}'");
        assert!(hour < 24 && min < 60 && sec < 60, "time out of range in '{ts}'");
    }
}
