//! One lock for the tests that have to move the process environment.
//!
//! `HOME` and `PATH` are what the daemon's program resolution reads, so the
//! tests for it are obliged to set them — and the environment is per *process*,
//! not per test thread. Two modules holding two different mutexes is therefore
//! no protection at all: the pane spawner's tests and the usage sampler's tests
//! live in the same test binary and would take turns overwriting each other's
//! `HOME`. Hence one guard, crate-wide.

/// Set environment variables for as long as the guard is held, then put back
/// exactly what was there — including "it was unset".
pub(crate) struct EnvGuard {
    prev: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    pub(crate) fn set(vars: &[(&'static str, &str)]) -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // Poisoning is not interesting here: a test that panicked mid-guard
        // still had its variables restored by `Drop`, so the next one can have
        // the lock.
        let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = vars
            .iter()
            .map(|(k, v)| {
                let prev = std::env::var_os(k);
                std::env::set_var(k, v);
                (*k, prev)
            })
            .collect();
        Self { prev, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, prev) in self.prev.drain(..) {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}
