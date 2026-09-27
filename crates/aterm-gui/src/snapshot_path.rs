// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a SIGUSR1 screen snapshot may land, and how it is written.
//!
//! The snapshot carries sensitive terminal/window content, so it gets the same
//! posture as the control socket's `image` verb (see `control_auth`): by default the
//! PNG/.txt/.done files land in the per-user `0700` control directory under a
//! per-process name (`aterm_snapshot-<pid>.png`) and are written `0600`. That is
//! the ONE destination: the `$ATERM_SNAPSHOT_PATH` override was deleted
//! (2026-09-24, no environment variable changes a shipped aterm) — a capture
//! anywhere else is `aterm ctl image <file>`.

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use crate::control_auth;

#[cfg(any(unix, test))]
fn default_snapshot_file_name(pid: u32) -> String {
    format!("aterm_snapshot-{pid}.png")
}

/// Resolve the path the snapshot PNG may be written to (`.txt`/`.done` are
/// siblings), or `None` — with the reason already logged — when there is no
/// per-user control directory.
#[must_use]
#[cfg(unix)]
pub(crate) fn resolve() -> Option<String> {
    match control_auth::socket_dir() {
        Some(dir) => Some(
            dir.join(default_snapshot_file_name(std::process::id()))
                .to_string_lossy()
                .into_owned(),
        ),
        None => {
            crate::logging::stderr_line!(
                "aterm-gui: no per-user runtime dir (set XDG_RUNTIME_DIR or HOME); \
                 snapshot skipped"
            );
            None
        }
    }
}

// The two wrappers below — and the lexical-absolutization helper they share —
// have no production caller left: the snapshot writer now pins the directory
// itself and keeps the handle across the whole PNG/.txt/.done generation
// (`app_introspect::begin_snapshot_generation`), so it cannot re-derive the
// parent per file. They are kept, compiled for the TEST build only, because
// they are the smallest harness that drives the pinned-writer contract this
// module depends on end to end: `0600` on creation, mode re-tightened on
// overwrite, `O_NOFOLLOW` on the final component, and single-component names.
// Shipping them would be unreachable code in the binary.
#[cfg(test)]
fn absolute_lexical(path: &Path) -> std::io::Result<PathBuf> {
    let source = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in source.components() {
        match component {
            std::path::Component::Prefix(prefix) => result.push(prefix.as_os_str()),
            std::path::Component::RootDir => result.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !result.pop() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "private artifact path escapes its filesystem root",
                    ));
                }
            }
            std::path::Component::Normal(name) => result.push(name),
        }
    }
    Ok(result)
}

/// Compatibility wrapper around the retained-handle writer. The directory and
/// exact final file remain pinned until durable write and identity validation
/// have both completed.
#[cfg(test)]
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private artifact path has no filename",
        )
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = absolute_lexical(parent)?;
    let dir = crate::pinned_dir::PinnedDir::open_resolved(&parent)?;
    let file = dir.write_private(name, bytes)?;
    dir.sync()?;
    dir.validate_path_identity()?;
    file.validate_path_identity()
}

/// Compatibility wrapper for a single child of an already-authorized
/// directory. All mutation is relative to a retained directory handle.
#[cfg(test)]
pub(crate) fn write_private_at(
    dir: &Path,
    file_name: &std::ffi::OsString,
    bytes: &[u8],
) -> std::io::Result<()> {
    let dir = crate::pinned_dir::PinnedDir::open_resolved(&absolute_lexical(dir)?)?;
    let file = dir.write_private(file_name, bytes)?;
    dir.sync()?;
    dir.validate_path_identity()?;
    file.validate_path_identity()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_auth::ensure_private_dir;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn default_snapshot_name_is_per_process() {
        assert_eq!(
            default_snapshot_file_name(42),
            "aterm_snapshot-42.png",
            "parallel aterm instances must never share a completion marker"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_private_at_creates_inside_dir_via_openat() {
        let dir = std::env::temp_dir().join(format!("aterm-snap-at-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let name = std::ffi::OsString::from("shot.png");
        write_private_at(&dir, &name, b"png-bytes").unwrap();
        let path = dir.join(&name);
        assert_eq!(std::fs::read(&path).unwrap(), b"png-bytes");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Overwrite truncates and re-tightens.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private_at(&dir, &name, b"x").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"x");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_private_at_refuses_symlinked_final_component() {
        // A symlink planted at the final name must NOT be followed (O_NOFOLLOW):
        // the write must fail rather than clobber the link target.
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("aterm-snap-at-sym-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();
        symlink(&victim, dir.join("evil.png")).unwrap();
        let name = std::ffi::OsString::from("evil.png");
        assert!(
            write_private_at(&dir, &name, b"attack").is_err(),
            "writing through a symlinked final component must be refused",
        );
        // The victim is untouched.
        assert_eq!(std::fs::read(&victim).unwrap(), b"original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn write_private_refuses_symlinked_target() {
        // A symlink planted at the target name must NOT be followed
        // (FILE_FLAG_OPEN_REPARSE_POINT + reparse-attr reject): the write must fail
        // rather than clobber the link target. Skips when symlink creation is
        // unprivileged (no Developer Mode / SeCreateSymbolicLink), which is expected
        // on a locked-down CI box — the guard under test still compiles + links.
        let dir = std::env::temp_dir().join(format!("aterm-snap-win-sym-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();
        let link = dir.join("evil.png");
        if std::os::windows::fs::symlink_file(&victim, &link).is_err() {
            let _ = std::fs::remove_dir_all(&dir);
            return; // no symlink privilege — nothing to assert
        }
        assert!(
            write_private(&link, b"attack").is_err(),
            "writing through a reparse point must be refused",
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_private_at_rejects_multi_component_name() {
        let dir = std::env::temp_dir().join(format!("aterm-snap-at-multi-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let name = std::ffi::OsString::from("sub/shot.png");
        assert!(write_private_at(&dir, &name, b"x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_private_creates_0600_and_forces_mode_on_overwrite() {
        let dir = std::env::temp_dir().join(format!("aterm-snap-wr-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("snap.bin");
        write_private(&path, b"first").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        // A pre-existing loose file is truncated AND tightened to 0600.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, b"x").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
