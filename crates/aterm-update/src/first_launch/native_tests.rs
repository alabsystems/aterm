// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn expected_proof() -> Proof {
    Proof {
        nonce: "52".repeat(16),
        pid: std::process::id(),
        birth: aterm_uds::process::read_birth(std::process::id() as i32)
            .unwrap()
            .0,
        executable: std::env::current_exe()
            .unwrap()
            .as_os_str()
            .as_bytes()
            .to_vec(),
        arguments_digest: arguments_digest(&[OsString::from("init")]),
    }
}

#[test]
fn first_launch_work_waits_for_the_native_successor_proof() {
    let expected = expected_proof();
    let bytes = aterm_json::to_vec(&expected).unwrap();
    let (mut parent, mut peer) = UnixStream::pair().unwrap();
    bound_proof_socket(&parent, Duration::from_secs(31)).unwrap();
    bound_proof_socket(&peer, Duration::ZERO).unwrap();
    peer.write_all(&bytes).unwrap();
    peer.write_all(b"\n").unwrap();
    assert_eq!(
        read_successor_proof(&mut parent, &expected).unwrap(),
        expected
    );
    parent.write_all(b"G").unwrap();
    let mut commit = [0];
    peer.read_exact(&mut commit).unwrap();
    assert_eq!(commit, *b"G");
    assert!(
        matches!(bound_proof_socket(&parent, Duration::from_secs(61)), Err(error) if error.kind() == std::io::ErrorKind::TimedOut)
    );

    for field in 0..5 {
        let mut changed = expected_proof();
        match field {
            0 => changed.nonce = "53".repeat(16),
            1 => changed.pid += 1,
            2 => changed.birth += 1,
            3 => changed.executable.push(b'x'),
            _ => changed.arguments_digest[0] ^= 1,
        }
        let (mut parent, mut peer) = UnixStream::pair().unwrap();
        bound_proof_socket(&parent, Duration::ZERO).unwrap();
        peer.write_all(&aterm_json::to_vec(&changed).unwrap())
            .unwrap();
        peer.write_all(b"\n").unwrap();
        drop(peer);
        assert!(
            read_successor_proof(&mut parent, &expected).is_err(),
            "custody field {field}"
        );
    }
    for incomplete in [&b""[..], &bytes[..]] {
        let (mut parent, mut peer) = UnixStream::pair().unwrap();
        bound_proof_socket(&parent, Duration::ZERO).unwrap();
        peer.write_all(incomplete).unwrap();
        drop(peer);
        assert!(read_successor_proof(&mut parent, &expected).is_err());
    }
}

#[test]
fn first_launch_work_waits_for_the_native_quarantine_child() {
    let executable = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let actual = first_launch_child(
        aterm_uds::spawnfd::command(&executable).args(["-c", "echo begun >&2; echo finished >&2"]),
        "private first-launch child",
        Duration::from_secs(31),
    );
    let (status, stderr) = actual.unwrap();
    assert!(status.success());
    assert_eq!(stderr, "begun\nfinished");
    let (status, stderr) = first_launch_child(
        aterm_uds::spawnfd::command(&executable).args(["-c", "echo refused >&2; exit 7"]),
        "private first-launch child",
        Duration::ZERO,
    )
    .unwrap();
    assert_eq!(status.code(), Some(7));
    assert_eq!(stderr, "refused");
    let scratch = crate::paths::scratch_dir("first-launch-child");
    let marker = scratch.path().join("never-started");
    assert!(
        first_launch_child(
            aterm_uds::spawnfd::command(&executable)
                .args(["-c", "echo started > \"$1\"", "fixture"])
                .arg(&marker),
            "private first-launch child",
            Duration::from_secs(61),
        )
        .is_err()
    );
    assert!(!marker.exists());
}

#[test]
fn required_first_launch_custody_waits_for_its_native_writer() {
    for age in [Duration::from_secs(11), Duration::from_secs(61)] {
        let expired = age >= aterm_spec::wait::MUST_HAPPEN;
        let root = crate::paths::scratch_dir("first-launch-lock");
        let path = root.path().join("apply.lock");
        let mut held = Some(aterm_update_core::FileLock::acquire(&path).unwrap());
        let busy = aterm_update_core::FileLock::acquire_within(&path, Duration::ZERO)
            .err()
            .expect("the actual native writer is held");
        assert_eq!(busy.kind(), std::io::ErrorKind::TimedOut);
        let mut observed = 0;
        let result = take_first_launch_custody_with(
            &path,
            std::time::Instant::now() - age,
            |path, until| {
                let busy = aterm_update_core::FileLock::acquire_within(path, Duration::ZERO)
                    .err()
                    .expect("the callback observes the actual held writer");
                assert_eq!(busy.kind(), std::io::ErrorKind::TimedOut);
                observed += 1;
                drop(held.take());
                aterm_update_core::FileLock::acquire_within(
                    path,
                    until.saturating_duration_since(std::time::Instant::now()),
                )
            },
        );
        assert_eq!(observed, usize::from(!expired));
        assert_eq!(held.is_some(), expired);
        assert_eq!(result.is_err(), expired, "{:?}", result.as_ref().err());
        let busy = aterm_update_core::FileLock::acquire_within(&path, Duration::ZERO)
            .err()
            .expect("the original or returned guard retains custody");
        assert_eq!(busy.kind(), std::io::ErrorKind::TimedOut);
        drop(result);
        drop(held);
        let fresh = take_first_launch_custody(&path).unwrap();
        drop(fresh);
        aterm_update_core::FileLock::acquire_within(&path, Duration::ZERO).unwrap();
    }
}

fn signed_bundle(root: &Path, relative: &str, build: u64) -> Bundle {
    let app_root = root.join(relative);
    let exe = app_root.join("Contents/MacOS/aterm");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    let stand_in = aterm_uds::deadline::stand_in(Path::new("/bin/echo"));
    aterm_tempfile::clone_tree(&stand_in, &exe).unwrap();
    std::fs::write(
        app_root.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
         <key>CFBundleIdentifier</key><string>com.aterm.first-launch-producer</string>\
         <key>CFBundleExecutable</key><string>aterm</string>\
         <key>CFBundlePackageType</key><string>APPL</string>\
         <key>CFBundleVersion</key><string>{build}</string></dict></plist>"
        ),
    )
    .unwrap();
    let signed = aterm_uds::deadline::output_within(
        aterm_uds::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&app_root),
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert!(signed.status.success(), "{signed:?}");
    Bundle { app_root, exe }
}

/// A real publisher exchanges newer B after facts named older A. The native
/// preparation must preserve B and return the existing-copy relaunch disposition.
#[test]
fn a_newer_destination_published_after_facts_is_preserved_for_relaunch() {
    let root = crate::paths::scratch_dir("first-launch-dest");
    let root_path = root.path().canonicalize().unwrap();
    let source = signed_bundle(&root_path, "AppTranslocation/source/aterm.app", 10);
    let old = signed_bundle(&root_path, "Applications/aterm.app", 9);
    let newer = signed_bundle(&root_path, "newer.app", 11);
    let initial_destination = destination_identity(&old.app_root).unwrap();
    let staging = crate::paths::Staging::scratch("first-launch-newer-publisher");
    let lock =
        aterm_update_core::FileLock::acquire_within(&staging.apply_lock, Duration::ZERO).unwrap();
    let mut host = NativeHost {
        source,
        args: Vec::new(),
        facts: Facts {
            posture: InstallPosture::Translocated,
            can_write_applications: true,
            applications_dest: old.app_root.clone(),
            home_dest: None,
            installed: Some(InstalledCopy {
                app_root: old.app_root.clone(),
                at_least_running: false,
                verified: Ok(()),
            }),
        },
        _lock: lock,
        source_build: 10,
        initial_destination,
        child: RefCell::new(None),
        fixture: Some(Fixture { root: root_path }),
    };
    crate::install::checked_bundle_exchange(
        &newer.app_root,
        &old.app_root,
        "test concurrent publisher",
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    let published = destination_identity(&old.app_root).unwrap();
    assert!(matches!(
        host.install_copy(&old.app_root, Some(&old.app_root))
            .unwrap(),
        CopyDisposition::UseExisting
    ));
    assert!(
        destination_identity(&old.app_root).unwrap() == published,
        "native preparation replaced the newer published image"
    );
    assert_eq!(
        crate::verify::bundle_build_number(&old.app_root).unwrap(),
        11
    );
    assert!(
        host.child.borrow().is_none(),
        "installation preparation executes no image"
    );
    // The other admission route selected B in its facts. A publisher can
    // still replace its path before dispatch; a signed older image must not run.
    host.facts.installed = Some(InstalledCopy {
        app_root: old.app_root.clone(),
        at_least_running: true,
        verified: Ok(()),
    });
    crate::install::checked_bundle_exchange(
        &newer.app_root,
        &old.app_root,
        "test stale existing-copy selection",
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    let outcome = super::super::install_and_relaunch(&host);
    if !matches!(&outcome, FirstLaunchOutcome::Refused { reason } if reason.contains("became older"))
    {
        let kept = root.keep();
        panic!(
            "stale existing copy reached dispatch; image custody retained at {}: {outcome:?}",
            kept.display()
        );
    }
    assert!(host.child.borrow().is_none());
}
