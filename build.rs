//! Carries the files of `skills/` in the binary (Issue 182): writes `$OUT_DIR/skills.rs`, a list of
//! (path under `skills/`, content) that `src/agents/mod.rs` includes.

use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("a directory entry").path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else if p.is_file() {
            out.push(p);
        }
    }
}

fn main() {
    // Read when the script runs, not when it is compiled: a compiled build script is reused by
    // another checkout built with the same target folder (Issue 197).
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("skills");
    // Absolute, so that a build in another checkout sees a recorded path that is not its own.
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    walk(&root, &mut files);
    let mut code = String::from("pub const SKILLS: &[(&str, &[u8])] = &[\n");
    for f in files {
        let rel = f.strip_prefix(&root).expect("under skills/");
        code.push_str(&format!(
            "    ({:?}, include_bytes!({:?})),\n",
            rel.to_str().expect("a UTF-8 path"),
            f.to_str().expect("a UTF-8 path")
        ));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("skills.rs");
    std::fs::write(out, code).expect("write skills.rs");
}
