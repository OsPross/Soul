//! Warn when a command / unit of work exceeds 200ms.

use std::time::Instant;

pub struct Watch {
    name: &'static str,
    start: Instant,
}

impl Watch {
    pub fn start(name: &'static str) -> Self {
        Self {
            name,
            start: Instant::now(),
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let ms = self.start.elapsed().as_millis();
        if ms > 200 {
            eprintln!(
                "[soul][watchdog] WARN '{}' took {}ms (>200ms)",
                self.name, ms
            );
        }
    }
}

pub fn time_fn<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
    let _w = Watch::start(name);
    f()
}
