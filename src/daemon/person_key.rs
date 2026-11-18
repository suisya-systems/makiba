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
