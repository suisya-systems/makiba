//! M1-1: the base is a part that can be replaced. No file under `src/` outside `src/base/` names
//! `herdr` or `hc` (the module `herdr` is private to `base`, so the compiler stops an import; this
//! test holds the names, comments and strings included).

use std::fs;
use std::path::{Path, PathBuf};

/// The files under `root/src` outside `root/src/base` that name `herdr` or `hc` as a word.
fn violations(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, skip: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p == skip {
                continue;
            }
            if p.is_dir() {
                walk(&p, skip, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let src = root.join("src");
    let mut files = Vec::new();
    walk(&src, &src.join("base"), &mut files);
    files.sort();
    let mut found = Vec::new();
    for f in files {
        let text = fs::read_to_string(&f).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            let hit = line
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|w| w.eq_ignore_ascii_case("herdr") || w.eq_ignore_ascii_case("hc"));
            if hit {
                found.push(format!("{}:{}: {}", f.display(), n + 1, line.trim()));
            }
        }
    }
    found
}

#[test]
fn nothing_outside_the_base_names_herdr_or_hc() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let found = violations(root);
    assert!(
        found.is_empty(),
        "names outside src/base/:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_scan_fails_on_a_scratch_tree_with_an_offending_module() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(src.join("base")).unwrap();
    fs::write(src.join("base/herdr.rs"), "// allowed here: herdr hc\n").unwrap();
    fs::write(src.join("clean.rs"), "pub fn f() {}\n").unwrap();
    assert!(violations(dir.path()).is_empty());

    fs::write(src.join("leak.rs"), "use crate::base::herdr::Herdr;\n").unwrap();
    let found = violations(dir.path());
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("leak.rs"));

    fs::write(src.join("leak.rs"), "fn x() { run(\"hc\"); }\n").unwrap();
    assert_eq!(violations(dir.path()).len(), 1);
}
