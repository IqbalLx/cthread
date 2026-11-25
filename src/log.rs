//! Opt-in debug log: set `CTHREAD_LOG=/path/to/file`.

use std::io::Write;
use std::sync::{Mutex, OnceLock};

fn file() -> &'static Option<Mutex<std::fs::File>> {
    static F: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    F.get_or_init(|| {
        let p = std::env::var_os("CTHREAD_LOG")?;
        std::fs::OpenOptions::new().create(true).append(true).open(p).ok().map(Mutex::new)
    })
}

pub fn enabled() -> bool {
    file().is_some()
}

pub fn write(msg: &str) {
    if let Some(f) = file() {
        let t = chrono::Local::now().format("%H:%M:%S%.3f");
        let _ = writeln!(f.lock().unwrap(), "{t} {msg}");
    }
}

#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        if $crate::log::enabled() { $crate::log::write(&format!($($arg)*)); }
    };
}
