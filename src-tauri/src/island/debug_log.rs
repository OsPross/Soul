//! Verbose Island/media logs gated by `SOUL_DEBUG=1` (or `true` / `yes`).

use std::sync::OnceLock;

pub fn soul_debug() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("SOUL_DEBUG")
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
            })
            .unwrap_or(false)
    })
}

#[macro_export]
macro_rules! soul_debug {
    ($($arg:tt)*) => {{
        if $crate::island::debug_log::soul_debug() {
            eprintln!($($arg)*);
        }
    }};
}
