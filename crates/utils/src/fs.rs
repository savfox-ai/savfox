//! Filesystem helpers — atomic file replacement with optional Unix mode.
//!
//! `write_atomically` / `write_atomically_async` write to a sibling temp file
//! and `rename` it into place. `rename` is atomic on the same filesystem, so
//! either the existing file remains intact or it is replaced wholesale, even
//! if the process crashes mid-write.
//!
//! Pass `Some(mode)` (e.g. `0o600` for credentials) to set the temp file's
//! Unix permission bits *before* writing the contents — this avoids the
//! create-then-chmod race where another process could open the temp file
//! with the more permissive default mode. On non-Unix the mode is ignored.

use std::io::{self, Write};
use std::path::Path;

use tempfile::NamedTempFile;

/// Synchronously write `contents` to `path` via a sibling temp file + rename.
/// `mode` is applied with `O_CREAT | O_TRUNC` so any leftover temp file with
/// looser permissions is replaced rather than re-used.
pub fn write_atomically(path: &Path, contents: &[u8], mode: Option<u32>) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path {} has no parent directory", path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;

    let tmp = NamedTempFile::new_in(parent)?;

    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(tmp.path())?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    tmp.persist(path).map_err(io::Error::from)?;
    Ok(())
}

/// Async variant of `write_atomically`. Honors `mode` on Unix.
///
/// On Unix with `Some(mode)` the create+chmod is performed atomically (mode
/// is applied via `OpenOptionsExt::mode` so the file is never visible with
/// the default umask permissions).
pub async fn write_atomically_async(
    path: &Path,
    contents: Vec<u8>,
    mode: Option<u32>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path {} has no parent directory", path.display()),
            )
        })?
        .to_owned();
    tokio::fs::create_dir_all(&parent).await?;

    let pid = std::process::id();
    let suffix: u32 = rand_u32();
    let tmp_name = format!(".tmp-{pid}-{suffix:08x}");
    let tmp_path = match path.file_name() {
        Some(name) => parent.join(format!("{}{}", name.to_string_lossy(), &tmp_name)),
        None => parent.join(tmp_name),
    };

    let tmp_for_write = tmp_path.clone();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        let mut file = opts.open(&tmp_for_write)?;
        file.write_all(&contents)?;
        file.sync_all()?;
        Ok(())
    })
    .await
    .map_err(io::Error::other)??;

    tokio::fs::rename(&tmp_path, path).await
}

fn rand_u32() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    counter.wrapping_mul(0x9E37_79B9).wrapping_add(nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomically_replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("data.json");
        std::fs::write(&target, b"old").unwrap();

        write_atomically(&target, b"new", None).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomically_applies_unix_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("secret");

        write_atomically(&target, b"sk-secret", Some(0o600)).unwrap();
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn write_atomically_async_writes_contents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("async.json");

        write_atomically_async(&target, b"hello".to_vec(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_atomically_async_applies_unix_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("secret");

        write_atomically_async(&target, b"token".to_vec(), Some(0o600))
            .await
            .unwrap();
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
