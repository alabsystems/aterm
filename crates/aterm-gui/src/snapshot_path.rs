// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a SIGUSR1 screen snapshot may land, and how it is written.
//!
//! The snapshot carries sensitive terminal/window content, so it gets the same
//! posture as the control socket's `image` verb (see `control_auth`): by default the
//! PNG/.txt/.done files land in the per-user `0700` control directory under a
//! per-process name (`aterm_snapshot-<pid>.png`) and are written `0600`.
//! `$ATERM_SNAPSHOT_PATH` still preserves the exact path for users who
//! explicitly opt into that single-writer compatibility contract, but an
//! override whose directory another user
//! owns or can write into (e.g. `/tmp`, the historical default) is refused —
//! that user could read the screen contents or swap the target for a symlink
//! between our check and our write. The owned-and-unshared decision itself is
//! engine-side ([`aterm_types::fs_restricted::dir_safe_for_private_write`]);
//! this module only stats and writes.

use std::path::{Path, PathBuf};

use crate::control_auth;

fn default_snapshot_file_name(pid: u32) -> String {
    format!("aterm_snapshot-{pid}.png")
}

/// Resolve the path the snapshot PNG may be written to (`.txt`/`.done` are
/// siblings), or `None` — with the refusal already logged — when no safe
/// destination exists.
#[must_use]
pub fn resolve() -> Option<String> {
    if let Some(over) = std::env::var_os("ATERM_SNAPSHOT_PATH") {
        let requested = PathBuf::from(over);
        return match validate_override(&requested) {
            Some(p) => Some(p.to_string_lossy().into_owned()),
            None => {
                // Platform-selected refusal text: the Windows validator only
                // requires an existing directory (no uid/mode semantics there).
                #[cfg(unix)]
                eprintln!(
                    "aterm-gui: refusing ATERM_SNAPSHOT_PATH {}: its directory must exist, \
                     be owned by uid {}, and not be group/other-writable; snapshot skipped",
                    requested.display(),
                    control_auth::our_uid()
                );
                #[cfg(windows)]
                eprintln!(
                    "aterm-gui: refusing ATERM_SNAPSHOT_PATH {}: its directory must exist; \
                     snapshot skipped",
                    requested.display()
                );
                None
            }
        };
    }
    match control_auth::socket_dir() {
        Some(dir) => Some(
            dir.join(default_snapshot_file_name(std::process::id()))
                .to_string_lossy()
                .into_owned(),
        ),
        None => {
            eprintln!(
                "aterm-gui: no per-user runtime dir (set XDG_RUNTIME_DIR, HOME, or \
                 ATERM_SNAPSHOT_PATH); snapshot skipped"
            );
            None
        }
    }
}

/// Validate an explicit `$ATERM_SNAPSHOT_PATH` override: the parent directory
/// (symlinks resolved, so the check binds to the real directory) must satisfy
/// the engine-side private-write predicate for our euid. Returns the
/// canonical-parent form of the path — the directory checked IS the directory
/// written to — or `None` when missing/unsafe.
#[cfg(unix)]
fn validate_override(requested: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let file_name = requested.file_name()?;
    let parent = match requested.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let canon = std::fs::canonicalize(parent).ok()?;
    let meta = std::fs::metadata(&canon).ok()?;
    let safe = aterm_types::fs_restricted::dir_safe_for_private_write(
        control_auth::our_uid(),
        meta.uid(),
        meta.mode(),
    );
    if safe {
        Some(canon.join(file_name))
    } else {
        None
    }
}

/// Windows variant: canonicalize the parent and require it to exist (the
/// checked directory IS the one written to). The uid/mode ownership predicate
/// and the symlink-swap hardening are POSIX-only — here an override is the
/// user's explicit opt-in and the per-user profile ACLs are the boundary.
#[cfg(windows)]
fn validate_override(requested: &Path) -> Option<PathBuf> {
    let file_name = requested.file_name()?;
    let parent = match requested.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let canon = std::fs::canonicalize(parent).ok()?;
    std::fs::metadata(&canon)
        .ok()?
        .is_dir()
        .then(|| canon.join(file_name))
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
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
pub fn write_private_at(
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

    #[test]
    fn override_into_private_dir_is_allowed() {
        let dir = std::env::temp_dir().join(format!("aterm-snap-ok-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let ok = validate_override(&dir.join("shot.png")).expect("0700 own dir allowed");
        assert!(ok.ends_with("shot.png"));
        // The returned path is canonical-parent based: its parent exists.
        assert!(ok.parent().unwrap().is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn override_into_tmp_is_refused() {
        // /tmp is root-owned and world-writable — the historical leak target.
        assert!(validate_override(Path::new("/tmp/aterm_snapshot.png")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn override_into_group_writable_dir_is_refused() {
        let dir = std::env::temp_dir().join(format!("aterm-snap-gw-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(validate_override(&dir.join("shot.png")).is_none());
        // Tightening the dir back to 0700 makes the same override valid.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(validate_override(&dir.join("shot.png")).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn override_with_missing_dir_is_refused() {
        let dir = std::env::temp_dir().join(format!("aterm-snap-none-{}", std::process::id()));
        assert!(validate_override(&dir.join("shot.png")).is_none());
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
