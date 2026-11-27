//! The person's key files (spec sections 2 to 4), each only the person's user can read, in a folder
//! only that user can open.
//!
//! - The person key, whose SHA-256 is the person's row in `agents`. `state.db` never holds the key
//!   itself. The door calls `ensure` when it starts; `makiba up` (M4-4) calls it too.
//! - The approval key (Issue 590, docs/design/acceptance-approval.md section 3): an ed25519 SSH key
//!   `makiba merge approve` signs an acceptance approval with. It is no door key: no row, no hash.
//!   `makiba up` makes it with `ssh-keygen` in its staging folder `<key>.new` and links it into
//!   place (`make_approval`), only while no agent that could read it is live
//!   (`agents_without_approval_deny`); every role's settings deny the key and the staging folder.

use super::door::{random_key, sha256_hex};
use crate::db::writer::StateWriter;
use rusqlite::Connection;
use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cli::APPROVAL_NAMESPACE;

/// The folder of the key file `path`, made when absent and set to mode 0700.
fn private_folder_of<'a>(path: &'a Path, what: &str) -> Result<&'a Path, String> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or_else(|| format!("the {what} {} has no folder", path.display()))?;
    private_folder(dir)?;
    Ok(dir)
}

/// Make `dir` when absent (mode 0700) and set it to mode 0700.
fn private_folder(dir: &Path) -> Result<(), String> {
    let io = |what: &str, e: std::io::Error| format!("cannot {what} {}: {e}", dir.display());
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| io("create", e))?;
    std::fs::set_permissions(dir, Permissions::from_mode(0o700)).map_err(|e| io("chmod", e))
}

/// Create the key file at `path` when it is absent (mode 0600, in a folder of mode 0700), and make
/// its hash the one person row that is not revoked. A new file revokes the hash of the old one.
pub fn ensure(w: &StateWriter, path: &Path) -> Result<(), String> {
    let shown = path.display();
    private_folder_of(path, "person key")?;
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

/// The approval key at `path` when it exists: made private again (0600 in a 0700 folder), and
/// whatever a broken try left in the staging folder `staging` removed. False when it is absent.
pub fn keep_approval(path: &Path, staging: &Path) -> Result<bool, String> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(false);
    }
    private_folder_of(path, "approval key")?;
    std::fs::set_permissions(path, Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot chmod {}: {e}", path.display()))?;
    if staging_folder(staging)? {
        for e in std::fs::read_dir(staging)
            .map_err(|e| format!("cannot read {}: {e}", staging.display()))?
            .flatten()
        {
            // Only what `make_approval` writes there: `key-<pid>` and its `.pub`.
            if e.file_name().to_string_lossy().starts_with("key-") {
                std::fs::remove_file(e.path())
                    .map_err(|err| format!("cannot remove {}: {err}", e.path().display()))?;
            }
        }
    }
    Ok(true)
}

/// Whether the staging folder `staging` exists, refused when it is anything but a folder of its
/// own (a link is never followed: what it leads to is not the key's to chmod or clear).
fn staging_folder(staging: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(staging) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot read {}: {e}", staging.display())),
        Ok(m) if m.file_type().is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "the staging folder {} is not a folder (a link or a file): remove it",
            staging.display()
        )),
    }
}

/// Make the approval key at `path`: `ssh-keygen` writes it into the staging folder `staging`
/// (0700, denied to every agent as the key is), and a hard link puts it in place only where
/// nothing is yet, so a second `makiba up` at the same time keeps the first one's key. The copies
/// in the staging folder are removed; no `.pub` is kept beside the key.
pub fn make_approval(path: &Path, staging: &Path) -> Result<(), String> {
    private_folder_of(path, "approval key")?;
    staging_folder(staging)?;
    private_folder(staging)?;
    let tmp = staging.join(format!("key-{}", std::process::id()));
    let public = PathBuf::from(format!("{}.pub", tmp.display()));
    let clean = || {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&public);
    };
    clean();
    let out = Command::new("ssh-keygen")
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            APPROVAL_NAMESPACE,
            "-f",
        ])
        .arg(&tmp)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run ssh-keygen: {e}"));
    let made = match out {
        Ok(o) if o.status.success() => {
            std::fs::set_permissions(&tmp, Permissions::from_mode(0o600))
                .map_err(|e| format!("cannot chmod {}: {e}", tmp.display()))
                .and_then(|()| match std::fs::hard_link(&tmp, path) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                    Err(e) => Err(format!("cannot put the key at {}: {e}", path.display())),
                })
        }
        Ok(o) => Err(format!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(e),
    };
    clean();
    made
}

/// What of the approval key no output may show, longest first: the key file's content, trimmed,
/// and each of its lines of 16 characters or more that is not an armor line (`-----...-----`), so
/// one line quoted alone is caught too. Empty when the file cannot be read.
pub fn approval_key_literals() -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(crate::cli::approval_key_path()) else {
        return Vec::new();
    };
    let whole = text.trim().to_string();
    if whole.is_empty() {
        return Vec::new();
    }
    let lines: Vec<String> = whole
        .lines()
        .map(str::trim)
        .filter(|l| l.len() >= 16 && !l.starts_with("-----"))
        .map(String::from)
        .collect();
    std::iter::once(whole).chain(lines).collect()
}

/// The ids of the live agents (a key of theirs not revoked) whose launch's settings file in
/// `settings_dir` does not deny reading the approval key `key`: a launch of a makiba from before
/// Issue 590, or one made while `$MAKIBA_APPROVAL_KEY` named another place. An agent whose launch
/// or settings file cannot be found or read is counted too. In order, each once.
pub fn agents_without_approval_deny(
    conn: &Connection,
    settings_dir: &Path,
    key: &Path,
) -> rusqlite::Result<Vec<String>> {
    let mut q = conn.prepare(
        "SELECT agent_id, spawn_id FROM agents
         WHERE role <> 'person' AND revoked_at IS NULL ORDER BY id",
    )?;
    let rows = q
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out: Vec<String> = Vec::new();
    for (agent, spawn) in rows {
        let denies = match spawn {
            Some(spawn) => launch_denies_approval(conn, settings_dir, &spawn, key)?,
            None => false,
        };
        if !denies && !out.contains(&agent) {
            out.push(agent);
        }
    }
    Ok(out)
}

/// Whether the launch `spawn` of this organization has a settings file in `settings_dir` that
/// denies reading the approval key `key`. A launch the ledger does not hold, or a file that
/// cannot be read, does not.
pub fn launch_denies_approval(
    conn: &Connection,
    settings_dir: &Path,
    spawn: &str,
    key: &Path,
) -> rusqlite::Result<bool> {
    let key = std::path::absolute(key)
        .unwrap_or_else(|_| key.to_path_buf())
        .display()
        .to_string();
    let launch: Option<(String, Option<String>)> = match conn.query_row(
        "SELECT l.role, r.task_id FROM run_launch l LEFT JOIN runs r ON r.id = l.run_id
         WHERE l.spawn_id = ?1",
        [spawn],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ) {
        Ok(row) => Some(row),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e),
    };
    let Some((role, task)) = launch else {
        return Ok(false);
    };
    let file = super::launch::settings_file(settings_dir, task.as_deref(), &role, spawn);
    Ok(std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v["sandbox"]["filesystem"]["denyRead"]
                .as_array()
                .map(|a| a.iter().any(|e| e.as_str() == Some(key.as_str())))
        })
        .unwrap_or(false))
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

    /// A staging folder that is a link is neither followed nor cleared: the files where it leads
    /// stay, keeping or making the key is refused, and the folder it leads to keeps its mode.
    #[test]
    fn a_linked_staging_folder_is_never_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("cfg/approval.key");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, Permissions::from_mode(0o755)).unwrap();
        for f in ["notes.txt", "key-1"] {
            std::fs::write(elsewhere.join(f), "the person's\n").unwrap();
        }
        let staging = tmp.path().join("cfg/approval.key.new");
        std::os::unix::fs::symlink(&elsewhere, &staging).unwrap();
        assert!(make_approval(&key, &staging).is_err());
        assert!(!key.exists());
        std::fs::write(&key, "a key\n").unwrap();
        assert!(keep_approval(&key, &staging).is_err());
        for f in ["notes.txt", "key-1"] {
            assert!(elsewhere.join(f).exists(), "{f} was removed");
        }
        assert_eq!(mode(&elsewhere), 0o755);
    }

    /// Clearing a staging folder of its own removes what a broken try left (`key-*`) and nothing
    /// else.
    #[test]
    fn keeping_the_key_clears_only_the_staged_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("cfg/approval.key");
        let staging = tmp.path().join("cfg/approval.key.new");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(&key, "a key\n").unwrap();
        for f in ["key-7", "key-7.pub", "other"] {
            std::fs::write(staging.join(f), "x\n").unwrap();
        }
        assert!(keep_approval(&key, &staging).unwrap());
        assert!(!staging.join("key-7").exists());
        assert!(!staging.join("key-7.pub").exists());
        assert!(staging.join("other").exists());
        assert_eq!(mode(&key), 0o600);
    }
}
