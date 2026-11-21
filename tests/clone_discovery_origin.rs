//! Issue 211: the discovery of a project's clone reads `origin` as configured.
//!
//! A project row without a URL is matched to the repository its clone's `origin` names. The
//! clone's own configuration is read as written (before any `url.<base>.insteadOf` rewrite), and
//! only when it holds exactly one `origin` URL:
//!
//! - a clone whose `origin` names another repository and is rewritten by `insteadOf` to the
//!   project's repository is not taken for the project;
//! - a clone whose `origin` names the project's repository is, even when it is rewritten;
//! - a clone with two `origin` URLs is not;
//! - a folder that is not a repository is not, though the global git configuration names an
//!   `origin` of the project's repository.
//!
//! No network and no daemon: `gate::clone_of` over a repositories folder in a temporary folder.

use makiba::daemon::gate;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Once, OnceLock};

const REPO: &str = "o/proj";
const PROJ: &str = "https://github.com/o/proj";

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The repositories folder. The global git configuration names `origin` as the project's
/// repository, so only the clone's own configuration may count.
fn repos() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let global = home.path().join("gitconfig");
        std::fs::write(&global, format!("[remote \"origin\"]\n\turl = {PROJ}\n")).unwrap();
        std::fs::create_dir_all(home.path().join("repos")).unwrap();
        // SAFETY: set once, before this file's tests run git.
        unsafe {
            std::env::set_var("GIT_CONFIG_GLOBAL", &global);
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        }
    });
    HOME.get().unwrap().path().join("repos")
}

fn git_in(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository `<repos>/<slug>` whose own configuration holds `origin` URLs `urls` and the
/// rewrites `(base, prefix)`.
fn clone(slug: &str, urls: &[&str], rewrites: &[(&str, &str)]) -> PathBuf {
    let dir = repos().join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    git_in(&dir, &["init", "-q"]);
    for u in urls {
        git_in(&dir, &["config", "--add", "remote.origin.url", u]);
    }
    for (base, prefix) in rewrites {
        git_in(&dir, &["config", &format!("url.{base}.insteadOf"), prefix]);
    }
    dir
}

fn found(slug: &str) -> Option<PathBuf> {
    gate::clone_of(&[(slug.to_string(), None)], &repos(), REPO)
}

#[test]
fn origin_of_another_repository_rewritten_to_the_project_is_not_taken() {
    clone(
        "rewritten-in",
        &["https://github.com/o/other"],
        &[(PROJ, "https://github.com/o/other")],
    );
    assert_eq!(found("rewritten-in"), None);
}

#[test]
fn origin_of_the_project_rewritten_elsewhere_is_taken() {
    let dir = clone("rewritten-out", &[PROJ], &[("/nowhere/served", PROJ)]);
    assert_eq!(found("rewritten-out"), Some(dir));
}

#[test]
fn two_origin_urls_are_not_taken() {
    clone("two-urls", &[PROJ, "https://github.com/o/other"], &[]);
    assert_eq!(found("two-urls"), None);
}

#[test]
fn a_folder_that_is_not_a_repository_is_not_taken() {
    std::fs::create_dir_all(repos().join("plain")).unwrap();
    assert_eq!(found("plain"), None);
}
