//! The person key (spec sections 2 to 4): a file only the person's user can read, in a folder only
//! that user can open, whose SHA-256 is the person's row in `agents`. `state.db` never holds the
//! key itself. The door calls `ensure` when it starts; `makiba up` (M4-4) calls it too.

use super::door::{random_key, sha256_hex};
use crate::db::writer::StateWriter;
use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Create the key file at `path` when it is absent (mode 0600, in a folder of mode 0700), and make
/// its hash the one person row that is not revoked. A new file revokes the hash of the old one.
pub fn ensure(w: &StateWriter, path: &Path) -> Result<(), String> {
    let shown = path.display();
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or_else(|| format!("the person key {shown} has no folder"))?;
    let io = |what: &str, e: std::io::Error| format!("cannot {what} {}: {e}", dir.display());
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| io("create", e))?;
    std::fs::set_permissions(dir, Permissions::from_mode(0o700)).map_err(|e| io("chmod", e))?;
    let key = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut f) => {
            let key = random_key().map_err(|e| format!("cannot make a key: {e}"))?;
            f.write_all(format!("{key}\n").as_bytes())
                .and_then(|()| f.sync_all())
                .map_err(|e| format!("cannot write {shown}: {e}"))?;
            key
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let key = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {shown}: {e}"))?
                .trim()
                .to_string();
            if key.is_empty() {
                return Err(format!(
                    "the person key {shown} is empty: delete it and start again"
                ));
            }
            std::fs::set_permissions(path, Permissions::from_mode(0o600))
                .map_err(|e| format!("cannot chmod {shown}: {e}"))?;
            key
        }
        Err(e) => return Err(format!("cannot create {shown}: {e}")),
    };
    let sha = sha256_hex(&key);
    w.transaction(|w| {
        w.conn().execute(
            "UPDATE agents SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE role = 'person' AND revoked_at IS NULL AND key_sha256 <> ?1",
            [&sha],
        )?;
        w.conn().execute(
            "INSERT INTO agents (agent_id, role, key_sha256) VALUES ('person', 'person', ?1)
             ON CONFLICT(key_sha256) DO UPDATE SET revoked_at = NULL WHERE agents.role = 'person'",
            [&sha],
        )?;
        Ok(())
    })
    .map_err(|e| format!("cannot store the person key's hash: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer() -> StateWriter {
        StateWriter::new(crate::db::open_in_memory().unwrap()).unwrap()
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn person_rows(w: &StateWriter) -> Vec<(String, bool)> {
        let mut q = w
            .conn()
            .prepare("SELECT key_sha256, revoked_at IS NULL FROM agents WHERE role = 'person' ORDER BY id")
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn an_existing_key_file_is_made_private_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("makiba");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, Permissions::from_mode(0o755)).unwrap();
        let file = dir.join("person.key");
        std::fs::write(&file, "the-old-key\n").unwrap();
        std::fs::set_permissions(&file, Permissions::from_mode(0o644)).unwrap();
        let w = writer();
        ensure(&w, &file).unwrap();
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "the-old-key\n");
        assert_eq!(person_rows(&w), vec![(sha256_hex("the-old-key"), true)]);
    }

    #[test]
    fn a_new_key_file_revokes_the_old_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("cfg/makiba/person.key");
        let w = writer();
        ensure(&w, &file).unwrap();
        let first = std::fs::read_to_string(&file).unwrap().trim().to_string();
        assert_eq!(first.len(), 64);
        std::fs::remove_file(&file).unwrap();
        ensure(&w, &file).unwrap();
        let second = std::fs::read_to_string(&file).unwrap().trim().to_string();
        assert_ne!(first, second);
        assert_eq!(
            person_rows(&w),
            vec![(sha256_hex(&first), false), (sha256_hex(&second), true)]
        );
    }

    #[test]
    fn an_empty_key_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("person.key");
        std::fs::write(&file, "\n").unwrap();
        let w = writer();
        assert!(ensure(&w, &file).is_err());
        assert!(person_rows(&w).is_empty());
    }
}
