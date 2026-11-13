// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use super::default_snapshot_file_name;
use crate::control_auth::ensure_private_dir;
use crate::pinned_dir::PinnedDir;
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn scratch_private_dir() -> (aterm_tempfile::TempDir, std::path::PathBuf) {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-snap-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let dir = tmp.path().join("priv");
    ensure_private_dir(&dir).unwrap();
    (tmp, dir)
}

fn write_in_pinned_dir(parent: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let dir = PinnedDir::open_resolved(parent)?;
    let file = dir.write_private(OsStr::new(name), bytes)?;
    dir.sync()?;
    dir.validate_path_identity()?;
    file.validate_path_identity()
}

#[cfg(unix)]
#[test]
fn default_snapshot_file_name_formats_pid_injectively() {
    assert_eq!(default_snapshot_file_name(0), "aterm_snapshot-0.png");
    assert_eq!(default_snapshot_file_name(1), "aterm_snapshot-1.png");
    assert_eq!(
        default_snapshot_file_name(u32::MAX),
        "aterm_snapshot-4294967295.png"
    );
    assert_ne!(
        default_snapshot_file_name(42),
        default_snapshot_file_name(43)
    );
}

/// NEGATIVE CONTROL: writing through a symlinked final component is refused without touching its target.
#[cfg(unix)]
#[test]
fn write_private_refuses_symlinked_final_component() {
    use std::os::unix::fs::symlink;
    let (_tmp, dir) = scratch_private_dir();
    let victim = dir.join("victim.txt");
    std::fs::write(&victim, b"original").unwrap();
    symlink(&victim, dir.join("evil.png")).unwrap();
    assert!(
        write_in_pinned_dir(&dir, "evil.png", b"attack").is_err(),
        "writing through a symlinked final component must be refused",
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"original");
}

/// NEGATIVE CONTROL: a file symlink, which an unelevated process may make only where Developer Mode is on
/// (a developer's usual Windows; the Windows lane turns it on), is refused without touching its target.
#[cfg(windows)]
#[test]
fn write_private_refuses_symlinked_target() {
    let (_tmp, dir) = scratch_private_dir();
    let victim = dir.join("victim.txt");
    std::fs::write(&victim, b"original").unwrap();
    let link = dir.join("evil.png");
    if let Err(e) = std::os::windows::fs::symlink_file(&victim, &link) {
        aterm_spec::host::could_not_run(format!(
            "this process may not make a symlink ({e}): Developer Mode is off and it is not \
             elevated, so no unelevated process can plant one here either"
        ));
    }
    assert!(
        write_in_pinned_dir(&dir, "evil.png", b"attack").is_err(),
        "writing through a reparse point must be refused",
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"original");
}

/// NEGATIVE CONTROL: the reparse point every Windows user can make: a junction, which needs no privilege
/// and no Developer Mode, as a parent of the snapshot redirects the write into its target
/// unless the writer refuses it.
#[cfg(windows)]
#[test]
fn write_private_refuses_a_junction_in_its_path() {
    let (_tmp, dir) = scratch_private_dir();
    let outside = dir.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let victim = outside.join("evil.png");
    std::fs::write(&victim, b"original").unwrap();
    let link = dir.join("link");
    crate::pinned_dir::windows_tests::junction(&link, &outside);
    assert!(
        write_in_pinned_dir(&link, "evil.png", b"attack").is_err(),
        "writing through a junction must be refused",
    );
    assert!(
        write_in_pinned_dir(&link, "planted.png", b"attack").is_err(),
        "so must creating a file through one",
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"original");
    assert!(
        !outside.join("planted.png").exists(),
        "nothing was made in the junction's target"
    );
    let _ = std::fs::remove_dir(&link);
}

/// NEGATIVE CONTROL: multi-component names are refused, while a single-component write creates 0600 and resets looser permissions on overwrite.
#[cfg(unix)]
#[test]
fn write_private_creates_0600_and_forces_mode_on_overwrite() {
    let (_tmp, dir) = scratch_private_dir();
    assert!(
        write_in_pinned_dir(&dir, "nested/snap.bin", b"no").is_err(),
        "multi-component names must be refused",
    );
    let path = dir.join("snap.bin");
    write_in_pinned_dir(&dir, "snap.bin", b"first").unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_in_pinned_dir(&dir, "snap.bin", b"x").unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(std::fs::read(&path).unwrap(), b"x");
}
