//! Kill points for tests (Issue M3-7): a test names the step at which `makibad` must die by
//! writing it into `<state>/makibad.die-at` before the daemon starts. The daemon's thread then
//! ends right after that step by an unwind that runs nothing further: no later write, no
//! `dispatcher_stopped`, and an open transaction is rolled back, as on a kill.
//!
//! Hold points (Issue 163): a test names in `<state>/makibad.hold-at` the stage update a pass must
//! stop before. There the daemon's thread writes `<state>/makibad.held` and waits until the test
//! removes it, once, so the test changes a row while the pass holds what it read.
//!
//! Only a debug build reads the files; in a release build `arm`, `point` and `hold` do nothing.

use std::path::Path;

#[cfg(debug_assertions)]
std::thread_local! {
    static AT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    /// The step to hold before, and the file that says the thread is held there.
    static HOLD: std::cell::RefCell<Option<(String, std::path::PathBuf)>> =
        const { std::cell::RefCell::new(None) };
}

/// Arm the calling thread with the step named in `<state>/makibad.die-at`, if any.
pub fn arm(state: &Path) {
    #[cfg(debug_assertions)]
    {
        let step = std::fs::read_to_string(state.join("makibad.die-at"))
            .ok()
            .map(|s| s.trim().to_string());
        AT.with(|at| *at.borrow_mut() = step);
        let hold = std::fs::read_to_string(state.join("makibad.hold-at"))
            .ok()
            .map(|s| (s.trim().to_string(), state.join("makibad.held")));
        HOLD.with(|h| *h.borrow_mut() = hold);
    }
    #[cfg(not(debug_assertions))]
    let _ = state;
}

/// Die here when the calling thread is armed for `step`. The thread ends with the payload
/// `"makibad died at <step>"`.
#[inline(always)]
pub fn point(step: &str) {
    #[cfg(debug_assertions)]
    if AT.with(|at| at.borrow().as_deref() == Some(step)) {
        std::panic::resume_unwind(Box::new(format!("makibad died at {step}")));
    }
    #[cfg(not(debug_assertions))]
    let _ = step;
}

/// Wait here, once, when the calling thread is armed to hold before `step`: `makibad.held` is
/// written and the thread goes on when it is removed.
#[inline(always)]
pub fn hold(step: &str) {
    #[cfg(debug_assertions)]
    {
        let held = HOLD.with(|h| {
            let mut h = h.borrow_mut();
            match h.as_ref() {
                Some((at, _)) if at == step => h.take().map(|(_, file)| file),
                _ => None,
            }
        });
        if let Some(file) = held {
            let _ = std::fs::write(&file, step);
            while file.exists() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = step;
}

#[cfg(test)]
mod tests {
    /// The kill points exist only where `debug_assertions` is on. A release build has it off unless
    /// a profile turns it on: no `[profile.release]` setting of this repository does (Cargo.toml,
    /// `.cargo/config.toml`, `.cargo/config`). (`CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS` in the
    /// environment of a build is not seen here.)
    #[test]
    fn no_release_profile_turns_debug_assertions_on() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for file in ["Cargo.toml", ".cargo/config.toml", ".cargo/config"] {
            let Ok(text) = std::fs::read_to_string(root.join(file)) else {
                continue;
            };
            let mut section = String::new();
            for line in text.lines().map(|l| l.replace(' ', "")) {
                if line.starts_with('[') {
                    section = line;
                    continue;
                }
                let key = format!("{}{line}", section.trim_matches(['[', ']']));
                let release =
                    key.starts_with("profile.release") || line.starts_with("profile.release");
                assert!(
                    !(release && line.contains("debug-assertions") && !line.contains("=false")),
                    "{file} turns debug-assertions on for release: {line}"
                );
            }
        }
    }
}
