// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact-source harness: compiles the production file without requiring the
//! host owner's lib.rs registration. No mirrored implementation or helper script.
#![cfg(unix)]

pub use trust_build_cpu::Owner;
#[path = "../recovery_material.rs"]
mod material;

use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::num::NonZeroU32;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

use material::{RecoveryAuthority, RecoveryMaterial, commitment};

fn directory() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
fn parent(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().canonicalize().unwrap()
}
fn fresh() -> RecoveryMaterial {
    RecoveryMaterial::mint(
        [1; 32],
        Owner { pid: 42, birth: Some(73) },
        NonZeroU32::new(2).unwrap(),
        [2; 32],
    )
    .unwrap()
}

#[test]
fn persisted_original_commitments_and_one_shot() {
    let dir = directory();
    let path = parent(&dir);
    let mut prepared = fresh().persist(&path, "authority").unwrap();
    assert_eq!(prepared.width().get(), 2);
    assert_eq!(prepared.owner(), Owner { pid: 42, birth: Some(73) });
    assert_eq!(prepared.domain(), [1; 32]);
    assert_eq!(prepared.context(), [2; 32]);
    let nonces = prepared.nonces().unwrap();
    assert!(
        RecoveryAuthority::open(&path, "authority").is_err(),
        "original FD must hold exclusive lock"
    );
    let units = prepared.take_units().unwrap();
    assert!(prepared.take_units().is_err());
    assert_eq!(prepared.nonces().unwrap(), nonces);
    assert_eq!(units.iter().map(|u| u.nonce).collect::<Vec<_>>(), nonces);
    let expected: Vec<_> = units
        .iter()
        .enumerate()
        .map(|(slot, unit)| {
            commitment(
                &prepared.domain(),
                prepared.owner(),
                prepared.width(),
                &prepared.context(),
                unit,
                slot as u32,
            )
        })
        .collect();
    drop(prepared);
    let authority = RecoveryAuthority::open(&path, "authority").unwrap();
    assert_eq!(authority.width().get(), 2);
    assert_eq!(authority.owner(), Owner { pid: 42, birth: Some(73) });
    assert_eq!(authority.domain(), [1; 32]);
    assert_eq!(authority.context(), [2; 32]);
    for (slot, unit) in authority.units().iter().enumerate() {
        assert_eq!(unit.nonce, units[slot].nonce);
        assert!(unit.secret == units[slot].secret);
        assert_eq!(
            commitment(
                &authority.domain(),
                authority.owner(),
                authority.width(),
                &authority.context(),
                unit,
                slot as u32
            ),
            expected[slot]
        );
    }
}

#[test]
fn hash_binds_every_field_and_birth_presence() {
    let unit = material::RecoveryUnit { nonce: [3; 16], secret: [4; 32] };
    let owner = Owner { pid: 1, birth: None };
    let width = NonZeroU32::new(2).unwrap();
    let base = commitment(&[1; 32], owner, width, &[2; 32], &unit, 0);
    assert_ne!(base, commitment(&[9; 32], owner, width, &[2; 32], &unit, 0));
    assert_ne!(base, commitment(&[1; 32], owner, width, &[9; 32], &unit, 0));
    assert_ne!(base, commitment(&[1; 32], owner, width, &[2; 32], &unit, 1));
    assert_ne!(
        base,
        commitment(&[1; 32], Owner { pid: 1, birth: Some(0) }, width, &[2; 32], &unit, 0)
    );
    assert_ne!(
        base,
        commitment(&[1; 32], Owner { pid: 2, birth: None }, width, &[2; 32], &unit, 0)
    );
    assert_ne!(base, commitment(&[1; 32], owner, NonZeroU32::new(3).unwrap(), &[2; 32], &unit, 0));
    let changed = material::RecoveryUnit { nonce: [5; 16], secret: unit.secret };
    assert_ne!(base, commitment(&[1; 32], owner, width, &[2; 32], &changed, 0));
    let changed = material::RecoveryUnit { nonce: unit.nonce, secret: [5; 32] };
    assert_ne!(base, commitment(&[1; 32], owner, width, &[2; 32], &changed, 0));
}

#[test]
fn corrupt_truncated_oversized_and_existing_refuse() {
    for mutation in 0..4 {
        let dir = directory();
        let path = parent(&dir);
        drop(fresh().persist(&path, "authority").unwrap());
        let leaf = path.join("authority");
        let original = fs::read(&leaf).unwrap();
        assert!(fresh().persist(&path, "authority").is_err());
        assert!(fs::read(&leaf).unwrap() == original);
        let mut file = OpenOptions::new().write(true).open(&leaf).unwrap();
        match mutation {
            0 => {
                file.seek(SeekFrom::Start(100)).unwrap();
                file.write_all(&[original[100] ^ 0xff]).unwrap();
            }
            1 => file.set_len(50).unwrap(),
            2 => file.set_len(4_000_000).unwrap(),
            _ => {
                file.seek(SeekFrom::End(0)).unwrap();
                file.write_all(&[0]).unwrap();
            }
        }
        assert!(RecoveryAuthority::open(&path, "authority").is_err());
    }
}

#[test]
fn original_identity_contents_domain_and_modes_rechecked() {
    for mutation in 0..5 {
        let dir = directory();
        let path = parent(&dir);
        let mut prepared = fresh().persist(&path, "authority").unwrap();
        assert!(prepared.validate([9; 32]).is_err());
        prepared.validate([1; 32]).unwrap();
        let leaf = path.join("authority");
        match mutation {
            0 => fs::set_permissions(&leaf, fs::Permissions::from_mode(0o644)).unwrap(),
            1 => fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap(),
            2 => {
                let bytes = fs::read(&leaf).unwrap();
                fs::rename(&leaf, path.join("old")).unwrap();
                fs::write(&leaf, bytes).unwrap();
                fs::set_permissions(&leaf, fs::Permissions::from_mode(0o600)).unwrap();
            }
            3 => {
                let mut f = OpenOptions::new().write(true).open(&leaf).unwrap();
                f.write_all(b"BROKEN").unwrap();
            }
            _ => fs::hard_link(&leaf, path.join("alias")).unwrap(),
        }
        assert!(prepared.validate([1; 32]).is_err());
        assert!(prepared.take_units().is_err());
        assert!(prepared.nonces().is_err());
    }
}

#[test]
fn symlinks_nonregular_relative_and_broad_parent_refuse() {
    let dir = directory();
    let path = parent(&dir);
    drop(fresh().persist(&path, "original").unwrap());
    symlink(path.join("original"), path.join("link")).unwrap();
    assert!(RecoveryAuthority::open(&path, "link").is_err());
    fs::create_dir(path.join("subdir")).unwrap();
    assert!(RecoveryAuthority::open(&path, "subdir").is_err());
    assert!(fresh().persist(std::path::Path::new("."), "authority").is_err());
    assert!(fresh().persist(&path, "../escape").is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(fresh().persist(&path, "authority").is_err());
    assert!(!path.join("authority").exists());
}

#[test]
fn parent_replacement_refuses() {
    let dir = directory();
    let path = parent(&dir);
    let private = path.join("private");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let mut prepared = fresh().persist(&private, "authority").unwrap();
    fs::rename(&private, path.join("old")).unwrap();
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(prepared.take_units().is_err());
}

#[test]
fn foreign_storage_refuses_when_chown_is_available() {
    use std::os::fd::AsRawFd;
    let dir = directory();
    let path = parent(&dir);
    drop(fresh().persist(&path, "authority").unwrap());
    let file = File::open(path.join("authority")).unwrap();
    // SAFETY: live fd; this changes only this test's newly created file.
    let changed = unsafe { libc::fchown(file.as_raw_fd(), 1, !0) };
    if changed == 0 {
        assert!(RecoveryAuthority::open(&path, "authority").is_err());
    } else {
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
        eprintln!("foreign-file UID mutation not exercised: fchown refused EPERM");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn inherited_acl_refuses_before_secret_creation() {
    let dir = directory();
    let path = parent(&dir);
    let status = std::process::Command::new("/bin/chmod")
        .arg("+a")
        .arg("everyone allow read,write,file_inherit,directory_inherit")
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(fresh().persist(&path, "authority").is_err());
    assert!(!path.join("authority").exists());
    // Also reject an ACL added to an already prepared file.
    assert!(
        std::process::Command::new("/bin/chmod").arg("-N").arg(&path).status().unwrap().success()
    );
    let mut prepared = fresh().persist(&path, "authority").unwrap();
    assert!(
        std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg("everyone allow read")
            .arg(path.join("authority"))
            .status()
            .unwrap()
            .success()
    );
    assert!(prepared.take_units().is_err());
    drop(prepared);
    assert!(RecoveryAuthority::open(&path, "authority").is_err());
}
