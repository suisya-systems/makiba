//! Kill points for tests (Issue M3-7): a test names the step at which `makibad` must die by
//! writing it into `<state>/makibad.die-at` before the daemon starts. The daemon's thread then
//! ends right after that step by an unwind that runs nothing further: no later write, no
//! `dispatcher_stopped`, and an open transaction is rolled back, as on a kill.
//!
//! Only a debug build reads the file; in a release build `arm` and `point` do nothing.

use std::path::Path;

#[cfg(debug_assertions)]
std::thread_local! {
    static AT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Arm the calling thread with the step named in `<state>/makibad.die-at`, if any.
pub fn arm(state: &Path) {
    #[cfg(debug_assertions)]
    {
        let step = std::fs::read_to_string(state.join("makibad.die-at"))
            .ok()
            .map(|s| s.trim().to_string());
        AT.with(|at| *at.borrow_mut() = step);
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
