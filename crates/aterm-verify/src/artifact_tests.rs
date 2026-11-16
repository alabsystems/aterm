// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{Ctx, EnvSnapshot, MeasureArtifact, Scope};
use std::cell::Cell;

pub(crate) fn executable(debug: bool) -> Vec<u8> {
    let stamp = aterm_stamp::Stamp {
        build_profile: "debug",
        debug_assertions: Some(debug),
        target_arch: std::env::consts::ARCH,
        compiler_commit: "0123456789012345678901234567890123456789",
        compiler_host: "fixture-host",
        ..aterm_stamp::Stamp::UNSTAMPED
    };
    let mut bytes = vec![0; 256 + aterm_stamp::facts::LEN];
    let cpu = if std::env::consts::ARCH == "aarch64" {
        0x0100_000c
    } else {
        0x0100_0007
    };
    for (at, word) in [
        (0, 0xfeed_facf_u32),
        (4, cpu),
        (12, 2),
        (16, 1),
        (20, 152),
        (32, 0x19),
        (36, 152),
        (92, 5),
        (96, 1),
        (152, 256),
    ] {
        bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    let len = bytes.len() as u64;
    bytes[40..46].copy_from_slice(b"__TEXT");
    bytes[80..88].copy_from_slice(&len.to_le_bytes());
    bytes[104..117].copy_from_slice(aterm_stamp::facts::SECTION);
    bytes[120..126].copy_from_slice(b"__TEXT");
    bytes[144..152].copy_from_slice(&(aterm_stamp::facts::LEN as u64).to_le_bytes());
    bytes[256..].copy_from_slice(&aterm_stamp::facts::encode(stamp));
    bytes
}

#[cfg(unix)]
pub(crate) fn bundle(root: &Path, bytes: &[u8]) -> std::path::PathBuf {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let app = root.join("fixture.app");
    let dir = app.join("Contents/MacOS");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("aterm"), bytes).unwrap();
    std::fs::set_permissions(dir.join("aterm"), std::fs::Permissions::from_mode(0o755)).unwrap();
    for alias in ["aterm-gui", "aterm-ctl"] {
        symlink("aterm", dir.join(alias)).unwrap();
    }
    app
}

#[cfg(unix)]
#[test]
fn offline_admission_checks_assertions_and_vm_authority_before_a_private_copy() {
    for (debug, expected, release_gate, vm, allowed) in [
        (true, Profile::Debug, false, false, true),
        (false, Profile::Debug, false, false, false),
        (true, Profile::Release, true, true, false),
        (true, Profile::Debug, true, true, false),
        (false, Profile::Release, true, false, false),
        (false, Profile::Release, true, true, true),
    ] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let app = bundle(tmp.path(), &executable(debug));
        let mut ctx = Ctx::new(
            tmp.path().to_owned(),
            Scope::workspace(),
            EnvSnapshot::default(),
            tmp.path().join("scratch"),
        )
        .with_release(release_gate);
        ctx.virtual_machine = if vm { || Some(true) } else { || Some(false) };
        let starts = Cell::new(0);
        let result = MeasureArtifact::for_profile(&app, expected).and_then(|artifact| {
            artifact.admit(&ctx)?;
            let _copy = Snapshot::prepare(&artifact.app, artifact.executable.as_ref().unwrap())?;
            starts.set(starts.get() + 1);
            Ok(())
        });
        assert_eq!(
            result.is_ok(),
            allowed,
            "debug={debug} expected={expected:?} release_gate={release_gate} vm={vm}: {result:?}"
        );
        assert_eq!(
            starts.get(),
            u8::from(allowed),
            "no private image is created before the offline guards pass"
        );
        assert!(!ctx.scratch.exists());
    }
    for bytes in [b"not an executable or compile facts".to_vec(), {
        let mut bytes = executable(true);
        bytes[256 + 20] = b'2'; // Unknown record version, not a profile inference from its path.
        bytes
    }] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let app = bundle(tmp.path(), &bytes);
        let starts = Cell::new(0);
        let ctx = Ctx::new(
            tmp.path().to_owned(),
            Scope::workspace(),
            EnvSnapshot::default(),
            tmp.path().join("scratch"),
        );
        let result = MeasureArtifact::for_profile(&app, Profile::Debug)
            .and_then(|artifact| artifact.with_admission(&ctx, |_| starts.set(starts.get() + 1)));
        assert!(result.is_err());
        assert_eq!(starts.get(), 0);
        assert!(!ctx.scratch.exists());
    }
}

#[cfg(unix)]
#[test]
fn file_content_replacement_and_alias_drift_revoke_admission_before_start() {
    for same_bytes in [true, false] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let bytes = executable(true);
        let app = bundle(tmp.path(), &bytes);
        let artifact = MeasureArtifact::for_profile(&app, Profile::Debug).unwrap();
        let ctx = Ctx::new(
            tmp.path().to_owned(),
            Scope::workspace(),
            EnvSnapshot::default(),
            tmp.path().join("scratch"),
        );
        let replacement = tmp.path().join("replacement");
        if same_bytes {
            std::fs::write(&replacement, bytes).unwrap();
            std::fs::rename(replacement, artifact.bin("aterm")).unwrap();
        } else {
            let mut bytes = bytes;
            bytes[255] = 1;
            std::fs::write(artifact.bin("aterm"), bytes).unwrap();
        }
        let starts = Cell::new(0);
        assert!(
            artifact
                .with_admission(&ctx, |_| starts.set(starts.get() + 1))
                .is_err()
        );
        assert_eq!(starts.get(), 0);
    }
    let tmp = aterm_tempfile::tempdir().unwrap();
    let app = bundle(tmp.path(), &executable(true));
    let artifact = MeasureArtifact::for_profile(&app, Profile::Debug).unwrap();
    let ctx = Ctx::new(
        tmp.path().to_owned(),
        Scope::workspace(),
        EnvSnapshot::default(),
        tmp.path().join("scratch"),
    );
    std::fs::remove_file(artifact.bin("aterm-gui")).unwrap();
    std::fs::write(artifact.bin("aterm-gui"), executable(false)).unwrap();
    assert!(
        artifact
            .with_admission(&ctx, |_| panic!("an unadmitted alias must not start"))
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn changing_the_requested_profile_cannot_override_the_held_compile_facts() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let app = bundle(tmp.path(), &executable(true));
    let mut artifact = MeasureArtifact::for_profile(&app, Profile::Debug).unwrap();
    artifact.profile = Profile::Release;
    let mut ctx = Ctx::new(
        tmp.path().to_owned(),
        Scope::workspace(),
        EnvSnapshot::default(),
        tmp.path().join("scratch"),
    )
    .with_release(true);
    ctx.virtual_machine = || Some(true);
    assert!(
        artifact
            .with_admission(&ctx, |_| panic!(
                "caller mutation must not override immutable assertions"
            ))
            .is_err()
    );
    let release_root = tmp.path().join("release");
    let app = bundle(&release_root, &executable(false));
    let release = MeasureArtifact::for_profile(&app, Profile::Release).unwrap();
    ctx.virtual_machine = || None;
    assert!(
        release
            .with_admission(&ctx, |_| panic!(
                "an unread VM fact grants no release/scratch-world authority"
            ))
            .is_err()
    );
}
