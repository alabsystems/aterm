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
fn context(root: &Path) -> Ctx {
    Ctx::new(
        root.to_owned(),
        Scope::workspace(),
        EnvSnapshot::default(),
        root.join("scratch"),
    )
}

#[cfg(unix)]
#[test]
fn offline_admission_checks_assertions_before_a_private_copy() {
    // A release image's admission, and a debug image's refusal under a release gate, have tests
    // of their own below.
    for (debug, expected, release_gate, allowed) in [
        (true, Profile::Debug, false, true),
        (false, Profile::Debug, false, false),
        (true, Profile::Release, true, false),
    ] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let app = bundle(tmp.path(), &executable(debug));
        let ctx = context(tmp.path()).with_release(release_gate);
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
            "debug={debug} expected={expected:?} release_gate={release_gate}: {result:?}"
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
        let ctx = context(tmp.path());
        let result = MeasureArtifact::for_profile(&app, Profile::Debug)
            .and_then(|artifact| artifact.with_admission(&ctx, |_| starts.set(starts.get() + 1)));
        assert!(result.is_err());
        assert_eq!(starts.get(), 0);
        assert!(!ctx.scratch.exists());
    }
}

/// A release cut runs on a Mac's own hardware: its release image is admitted for the scratch world
/// its measurement makes, under the release gate, `--as-release` and an ordinary run alike.
#[cfg(unix)]
#[test]
fn a_release_image_is_admitted_on_hardware_for_its_scratch_world_measurement() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let release =
        MeasureArtifact::for_profile(&bundle(tmp.path(), &executable(false)), Profile::Release)
            .unwrap();
    for (release_gate, as_release) in [(true, false), (true, true), (false, true), (false, false)] {
        let mut ctx = context(tmp.path()).with_release(release_gate);
        ctx.checkers_in_tests = as_release;
        assert_eq!(
            release.admit(&ctx),
            Ok(()),
            "release={release_gate} as_release={as_release}"
        );
        let _copy = Snapshot::prepare(&release.app, release.executable.as_ref().unwrap()).unwrap();
        assert!(!ctx.scratch.exists());
    }
}

#[cfg(unix)]
#[test]
fn a_debug_image_still_cannot_qualify_a_release() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let debug =
        MeasureArtifact::for_profile(&bundle(tmp.path(), &executable(true)), Profile::Debug)
            .unwrap();
    assert_eq!(debug.admit(&context(tmp.path())), Ok(()));
    for (release_gate, as_release) in [(true, false), (false, true), (true, true)] {
        let mut ctx = context(tmp.path()).with_release(release_gate);
        ctx.checkers_in_tests = as_release;
        assert_eq!(
            debug.admit(&ctx),
            Err("debug bundle validation cannot qualify a release".into()),
            "release={release_gate} as_release={as_release}"
        );
    }
}

/// Admission rests on custody, not on a platform: the image it admitted, once swapped for the
/// same bytes in a new file, changed in place or aliased to another file, is refused before
/// anything starts, a release image on the release path as a debug image at a desk.
#[cfg(unix)]
#[test]
fn file_replacement_and_alias_drift_revoke_admission_before_start() {
    for (debug, profile, release_gate) in [
        (true, Profile::Debug, false),
        (false, Profile::Release, true),
    ] {
        for swap in ["same bytes, new file", "changed in place", "alias"] {
            let tmp = aterm_tempfile::tempdir().unwrap();
            let bytes = executable(debug);
            let artifact =
                MeasureArtifact::for_profile(&bundle(tmp.path(), &bytes), profile).unwrap();
            let ctx = context(tmp.path()).with_release(release_gate);
            assert_eq!(artifact.admit(&ctx), Ok(()), "{profile:?} {swap}: before");
            match swap {
                "same bytes, new file" => {
                    let replacement = tmp.path().join("replacement");
                    std::fs::write(&replacement, &bytes).unwrap();
                    std::fs::rename(replacement, artifact.bin("aterm")).unwrap();
                }
                "changed in place" => {
                    let mut bytes = bytes;
                    bytes[255] = 1;
                    std::fs::write(artifact.bin("aterm"), bytes).unwrap();
                }
                _ => {
                    std::fs::remove_file(artifact.bin("aterm-gui")).unwrap();
                    std::fs::write(artifact.bin("aterm-gui"), &bytes).unwrap();
                }
            }
            let starts = Cell::new(0);
            let refused = artifact
                .with_admission(&ctx, |_| starts.set(starts.get() + 1))
                .expect_err(swap);
            assert_eq!(starts.get(), 0, "{profile:?} {swap}");
            assert!(
                refused.contains("no longer matches its admitted file")
                    || refused.contains("is not the admitted bundle executable"),
                "{profile:?} {swap}: {refused}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn changing_the_requested_profile_cannot_override_the_held_compile_facts() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let app = bundle(tmp.path(), &executable(true));
    let mut artifact = MeasureArtifact::for_profile(&app, Profile::Debug).unwrap();
    artifact.profile = Profile::Release;
    let ctx = context(tmp.path()).with_release(true);
    assert!(
        artifact
            .with_admission(&ctx, |_| panic!(
                "caller mutation must not override immutable assertions"
            ))
            .is_err()
    );
}
