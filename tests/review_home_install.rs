//! Issue 568: a reader installed under a home that the read boundary hides can start.
//!
//! The read boundary of Issue 262 (line L-K10 of `docs/design/lines.md`) hides a home that lies
//! in a system folder (a home under `/usr`) with a tmpfs. A reader's program installed beneath
//! that home was then skipped as "already given by the system bind" and missing after the tmpfs.
//! These tests read the mount plan of the system folders and the program
//! (`makiba::daemon::review::program_mounts`) with a system folder the test owns, as bubblewrap
//! reads it: what the reader could see. A home under the system folder gets its program back, and
//! nothing more of the home than an ordinary home shows.

use makiba::daemon::review::program_mounts;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

enum Mount {
    Bind { src: PathBuf, rw: bool },
    Cover,
}

/// The mounts of a bubblewrap argument list, in order: (destination, mount). An option this
/// reading does not know fails the test, so that no option can hide a mount.
fn mounts(argv: &[String]) -> Vec<(PathBuf, Mount)> {
    let mut out = vec![];
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        let arg = |n: usize| -> &str {
            argv.get(i + n)
                .unwrap_or_else(|| panic!("{a} without its value in {argv:?}"))
        };
        match a {
            "--" => break,
            "--tmpfs" | "--dir" => {
                out.push((PathBuf::from(arg(1)), Mount::Cover));
                i += 2;
            }
            "--symlink" => {
                out.push((PathBuf::from(arg(2)), Mount::Cover));
                i += 3;
            }
            "--ro-bind" | "--bind" => {
                let (src, dst) = (PathBuf::from(arg(1)), PathBuf::from(arg(2)));
                let m = if src == Path::new("/dev/null") {
                    Mount::Cover
                } else {
                    Mount::Bind {
                        src,
                        rw: a == "--bind",
                    }
                };
                out.push((dst, m));
                i += 3;
            }
            other => panic!("an option this test does not read: {other} in {argv:?}"),
        }
    }
    out
}

/// The index of the mount that decides what the reader sees at `p`, and that mount's answer:
/// the source and whether it may write, or `None` when nothing is there.
fn deciding(argv: &[String], p: &Path) -> Option<(usize, Option<(PathBuf, bool)>)> {
    let all = mounts(argv);
    for (i, (dst, m)) in all.into_iter().enumerate().rev() {
        if let Ok(rel) = p.strip_prefix(&dst) {
            return Some((
                i,
                match m {
                    Mount::Bind { src, rw } => Some((
                        if rel.as_os_str().is_empty() {
                            src
                        } else {
                            src.join(rel)
                        },
                        rw,
                    )),
                    Mount::Cover => None,
                },
            ));
        }
    }
    None
}

/// What the reader sees at `p`, or `None` (an empty cover, or no mount at all).
fn visible(argv: &[String], p: &Path) -> Option<(PathBuf, bool)> {
    deciding(argv, p).and_then(|(_, v)| v)
}

/// Every bind whose source lies under `dir`, relative to it: (source, writable).
fn binds_under(argv: &[String], dir: &Path) -> BTreeSet<(PathBuf, bool)> {
    mounts(argv)
        .into_iter()
        .filter_map(|(_, m)| match m {
            Mount::Bind { src, rw } => src.strip_prefix(dir).ok().map(|r| (r.to_path_buf(), rw)),
            Mount::Cover => None,
        })
        .collect()
}

fn has(argv: &[String], words: &[String]) -> bool {
    argv.windows(words.len()).any(|w| w == words)
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

fn file(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A canonical scratch folder with a system folder `usr` holding `usr/lib/libx`.
fn scratch() -> (PathBuf, PathBuf) {
    let t = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
    let usr = t.join("usr");
    file(&usr.join("lib/libx"), "lib\n");
    (t, usr)
}

/// A home at `home` with a decoy beside its configuration, and the denied places of it: its
/// readers' configuration (each holding a decoy) and a state folder outside it.
fn a_home(t: &Path, home: &Path) -> Vec<PathBuf> {
    file(&home.join("secret.txt"), "decoy-home-secret\n");
    file(&home.join(".codex/auth.json"), "{\"decoy\": 1}\n");
    file(&home.join(".claude/.credentials.json"), "{\"decoy\": 1}\n");
    file(&t.join("state/state.db"), "decoy\n");
    vec![home.join(".codex"), home.join(".claude"), t.join("state")]
}

/// A reader's program, an empty file never run, at `p`.
fn program(p: &Path) {
    file(p, "");
}

fn plan(usr: &Path, program: &Path, home: &Path, deny: &[PathBuf]) -> Vec<String> {
    program_mounts(&[usr], program, Some(home), deny).unwrap()
}

/// The reader under a home in a system folder is bound back after the home's tmpfs; the rest of
/// the home and the readers' configuration stay hidden, the system folder stays given.
#[test]
fn a_reader_under_a_home_in_a_system_folder_is_given_back() {
    let (t, usr) = scratch();
    let home = usr.join("local/home/someone");
    let deny = a_home(&t, &home);
    let codex = home.join(".local/bin/codex");
    program(&codex);
    let args = plan(&usr, &codex, &home, &deny);

    assert_eq!(
        visible(&args, &codex),
        Some((codex.clone(), false)),
        "{args:?}"
    );
    assert_eq!(visible(&args, &home.join("secret.txt")), None, "{args:?}");
    assert_eq!(visible(&args, &home), None, "{args:?}");
    assert_eq!(
        visible(&args, &home.join(".codex/auth.json")),
        None,
        "{args:?}"
    );
    assert_eq!(
        visible(&args, &home.join(".claude/.credentials.json")),
        None,
        "{args:?}"
    );
    let libx = usr.join("lib/libx");
    assert_eq!(
        visible(&args, &libx),
        Some((libx.clone(), false)),
        "{args:?}"
    );
    let all = mounts(&args);
    let tmpfs = all
        .iter()
        .position(|(d, m)| d == &home && matches!(m, Mount::Cover))
        .unwrap_or_else(|| panic!("no cover of the home in {args:?}"));
    let (at, _) = deciding(&args, &codex).unwrap();
    assert!(
        tmpfs < at,
        "the program's bind comes before the home's tmpfs: {args:?}"
    );
}

/// The same through a home reached by a link: the program's real place, under the hidden real
/// home, is bound back; the real home stays hidden.
#[test]
fn a_reader_under_a_linked_home_in_a_system_folder_is_given_back() {
    let (t, usr) = scratch();
    let real_home = usr.join("local/home/someone");
    let deny = a_home(&t, &real_home);
    std::fs::create_dir_all(t.join("home")).unwrap();
    let home = t.join("home/someone");
    std::os::unix::fs::symlink(&real_home, &home).unwrap();
    let found = home.join(".local/bin/codex");
    let real_program = real_home.join(".local/bin/codex");
    program(&real_program);
    let args = plan(&usr, &found, &home, &deny);

    assert_eq!(
        visible(&args, &real_program),
        Some((real_program.clone(), false)),
        "{args:?}"
    );
    assert!(visible(&args, &found).is_some(), "{args:?}");
    assert_eq!(
        visible(&args, &real_home.join("secret.txt")),
        None,
        "{args:?}"
    );
    assert_eq!(visible(&args, &real_home), None, "{args:?}");
}

/// A reader installed by npm under a hidden home, whose prefix holds a denied place: the `bin`
/// link stays a link to the script, the package and the outermost node_modules are given back,
/// the denied place and the rest of the home stay hidden.
#[test]
fn an_npm_reader_under_a_home_in_a_system_folder_is_given_back() {
    let (t, usr) = scratch();
    let home = usr.join("local/home/someone");
    let mut deny = a_home(&t, &home);
    let local = home.join(".local");
    let state = local.join("state/makiba");
    file(&state.join("decoy.txt"), "decoy-state\n");
    deny.push(state.clone());
    let package = local.join("lib/node_modules/@openai/codex");
    let script = package.join("bin/codex.js");
    program(&script);
    let dep = local.join("lib/node_modules/dep/index.js");
    file(&dep, "");
    let found = local.join("bin/codex");
    std::fs::create_dir_all(found.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("../lib/node_modules/@openai/codex/bin/codex.js", &found).unwrap();
    let args = plan(&usr, &found, &home, &deny);

    assert!(
        has(&args, &["--symlink".into(), s(&script), s(&found)]),
        "{args:?}"
    );
    assert_eq!(
        visible(&args, &script),
        Some((script.clone(), false)),
        "{args:?}"
    );
    assert_eq!(visible(&args, &dep), Some((dep.clone(), false)), "{args:?}");
    assert_eq!(visible(&args, &state.join("decoy.txt")), None, "{args:?}");
    assert_eq!(visible(&args, &home.join("secret.txt")), None, "{args:?}");
}

/// Nothing new is shown: of a home under the system folder the plan binds the same places as of
/// an ordinary home holding the same reader.
#[test]
fn a_reader_under_a_hidden_home_is_shown_no_more_than_under_any_home() {
    let (t, usr) = scratch();
    let mut seen = vec![];
    for home in [usr.join("local/home/someone"), t.join("elsewhere/someone")] {
        let deny = a_home(&t, &home);
        let codex = home.join(".local/bin/codex");
        program(&codex);
        let args = plan(&usr, &codex, &home, &deny);
        assert_eq!(visible(&args, &home.join("secret.txt")), None, "{args:?}");
        seen.push(binds_under(&args, &home));
    }
    assert_eq!(seen[0], seen[1]);
    assert_eq!(seen[0], BTreeSet::from([(PathBuf::from(".local"), false)]));
}

/// A reader in the system folder itself is still given by the system bind alone.
#[test]
fn a_reader_in_the_system_folder_is_still_given_by_it() {
    let (t, usr) = scratch();
    let home = usr.join("local/home/someone");
    let deny = a_home(&t, &home);
    let codex = usr.join("bin/codex");
    program(&codex);
    let args = plan(&usr, &codex, &home, &deny);

    assert_eq!(
        binds_under(&args, &usr),
        BTreeSet::from([(PathBuf::new(), false)]),
        "{args:?}"
    );
    assert_eq!(
        visible(&args, &codex),
        Some((codex.clone(), false)),
        "{args:?}"
    );
}
