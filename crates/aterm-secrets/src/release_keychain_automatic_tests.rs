// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fake::{Fake, Kc, World};
use super::*;

#[test]
fn a_mismatched_delivered_pair_changes_no_keychain() {
    struct WrongPair;
    impl Run for WrongPair {
        fn run(
            &self,
            cmd: &mut aterm_uds::spawnfd::Command,
            _: std::time::Duration,
        ) -> Result<Output, RunError> {
            assert_eq!(Path::new(cmd.get_program()).file_name().unwrap(), "openssl");
            Ok(super::fake::out(
                0,
                if cmd.get_args().any(|arg| arg == "pkey") {
                    "key A"
                } else {
                    "key B"
                },
                "",
            ))
        }
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let kc = Keychains::new(root.path(), &WrongPair);
    let refusal = prepare_existing_pair_with(
        &kc,
        &root.path().join("cert"),
        &root.path().join("key"),
        "A1B2C3D4E5F60718293A4B5C6D7E8F9012345678",
    )
    .unwrap_err();
    assert!(refusal.what.contains("do not match"));
    assert!(!kc.release().exists());
    assert!(!root.path().join(".aterm/apple/quarantine").exists());
}

#[test]
fn issuer_repair_requires_both_the_exact_issuer_and_issuing_key() {
    let leaf = "issuer= CN = Developer ID Certification Authority\nX509v3 Authority Key Identifier:\n keyid:AA:BB:CC\n";
    let ca = "subject= CN = Developer ID Certification Authority\nX509v3 Subject Key Identifier:\n AA:BB:CC\n";
    assert!(issuer_matches(leaf, ca));
    assert!(!issuer_matches(leaf, &ca.replace("AA:BB:CC", "AA:BB:DD")));
    assert!(!issuer_matches(leaf, &ca.replace("Authority", "Other")));
    assert!(!issuer_matches(
        leaf,
        "subject= CN = Developer ID Certification Authority\n"
    ));
}

#[test]
fn nested_same_thread_leases_do_not_admit_a_concurrent_thread() {
    let root = aterm_tempfile::tempdir().unwrap();
    let first = signing_lease(root.path()).unwrap();
    let nested = signing_lease(root.path()).unwrap();
    let home = root.path().to_path_buf();
    assert!(
        std::thread::spawn(
            move || signing_lease_before(&home, std::time::Instant::now())
                .unwrap_err()
                .contains("busy")
        )
        .join()
        .unwrap()
    );
    let home = root.path().to_path_buf();
    assert!(
        std::thread::spawn(move || signing_lease(&home).unwrap_err().contains("busy"))
            .join()
            .unwrap()
    );
    drop(nested);
    drop(first);
    let home = root.path().to_path_buf();
    assert!(
        std::thread::spawn(move || signing_lease(&home).is_ok())
            .join()
            .unwrap()
    );
}

#[test]
fn the_release_tools_keychains_wait_out_another_holders_lease_where_background_ones_defer() {
    struct NoTool;
    impl Run for NoTool {
        fn run(
            &self,
            _: &mut aterm_uds::spawnfd::Command,
            _: std::time::Duration,
        ) -> Result<Output, RunError> {
            unreachable!("taking the lease runs no tool")
        }
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let home = root.path().to_path_buf();
    let holder = std::thread::spawn(move || {
        let lease = signing_lease(&home).unwrap();
        held_tx.send(()).unwrap();
        go_rx.recv().unwrap();
        drop(lease);
    });
    held_rx.recv().unwrap();
    // The negative control: background convergence's keychains defer at once.
    assert!(
        Keychains::new(root.path(), &NoTool)
            .lock()
            .unwrap_err()
            .contains("busy")
    );
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let home = root.path().to_path_buf();
    let caller = std::thread::spawn(move || {
        done_tx
            .send(Keychains::waiting(&home, &NoTool).lock())
            .unwrap();
    });
    // While the lease stays held the call has not answered; keychains that deferred
    // would have answered busy at once.
    assert!(matches!(
        done_rx.recv_timeout(std::time::Duration::from_millis(500)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    go_tx.send(()).unwrap();
    holder.join().unwrap();
    assert_eq!(done_rx.recv_timeout(WAIT).unwrap(), Ok(()));
    caller.join().unwrap();
}

#[test]
fn signing_lease_process_child() {
    let Some(home) = std::env::var_os("ATERM_SIGNING_LEASE_TEST_HOME") else {
        return;
    };
    let _lease = signing_lease(Path::new(&home)).unwrap();
    // Keep the readiness proof on its own line even after libtest's single-thread prefix.
    println!("\nsigning-lease-held");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
}

#[test]
fn another_process_defers_while_the_signing_lease_is_held() {
    use aterm_uds::spawnfd::Stdio;
    let root = aterm_tempfile::tempdir().unwrap();
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "release_keychain::automatic_tests::signing_lease_process_child",
            "--nocapture",
        ])
        .env("ATERM_SIGNING_LEASE_TEST_HOME", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    aterm_uds::deadline::prints(&mut child, "signing-lease-held", WAIT);
    assert!(signing_lease(root.path()).unwrap_err().contains("busy"));
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + WAIT;
    let mut exited = aterm_uds::deadline::ExitWait::new(&child);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("signing lease child did not exit");
        }
        exited.wait(deadline.saturating_duration_since(std::time::Instant::now()));
    }
    assert!(signing_lease(root.path()).is_ok());
}

#[test]
fn the_owned_lock_is_secured_and_a_link_is_never_followed() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join(".aterm/apple");
    crate::fsx::ensure_private_dir(&dir).unwrap();
    let lock = dir.join(".signing.lock");
    std::fs::write(&lock, b"").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o666)).unwrap();
    drop(signing_lease(root.path()).unwrap());
    assert_eq!(
        std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::remove_file(&lock).unwrap();
    let other = root.path().join("untouched");
    std::fs::write(&other, b"keep").unwrap();
    symlink(&other, &lock).unwrap();
    assert!(signing_lease(root.path()).is_err());
    assert_eq!(std::fs::read(other).unwrap(), b"keep");
}

#[test]
fn a_busy_signing_transaction_defers_before_quarantining_any_material() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    std::fs::create_dir_all(home.join("Library/Keychains")).unwrap();
    std::fs::create_dir_all(home.join(".aterm/apple")).unwrap();
    let release = keychain_path(home);
    std::fs::write(&release, b"still in use").unwrap();
    let lock = std::fs::File::create(home.join(".aterm/apple/.signing.lock")).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "stand in for another process holding the signing transaction"
    )]
    lock.lock().unwrap();
    let fake = Fake::new(World::new("A1B2C3D4E5F60718293A4B5C6D7E8F9012345678"));
    let kc = Keychains::new(home, &fake);
    assert!(
        prepare_existing_pair_with(
            &kc,
            &home.join("cert"),
            &home.join("key"),
            "A1B2C3D4E5F60718293A4B5C6D7E8F9012345678"
        )
        .unwrap_err()
        .what
        .contains("busy")
    );
    assert_eq!(std::fs::read(release).unwrap(), b"still in use");
    assert!(!home.join(".aterm/apple/quarantine").exists());
    assert!(fake.calls("security", "create-keychain").is_empty());
}

#[test]
fn a_login_identity_never_skips_the_private_import_and_acl_repair() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    std::fs::create_dir_all(home.join("Library/Keychains")).unwrap();
    std::fs::create_dir_all(home.join(".aterm/apple")).unwrap();
    let sha1 = "A1B2C3D4E5F60718293A4B5C6D7E8F9012345678";
    let login = login_keychain_path(home);
    let original = Kc::holding(sha1);
    let mut world = World::new(sha1);
    world.keychains.insert(login.clone(), original.clone());
    world.search_list.push(login.clone());
    let fake = Fake::new(world);
    let kc = Keychains::new(home, &fake);
    let cert = home.join(".aterm/apple/devid-shared.cer");
    let key = home.join(".aterm/apple/devid-shared.key");
    prepare_existing_pair_with(&kc, &cert, &key, sha1).unwrap();
    assert_eq!(fake.0.borrow().keychains[&login], original);
    assert_eq!(fake.calls("security", "import").len(), 2);
    assert!(fake.calls("codesign", "--force").iter().all(|args| {
        args.windows(2)
            .any(|pair| pair == ["--keychain", kc.release().to_str().unwrap()])
    }));
    fake.0
        .borrow_mut()
        .keychains
        .get_mut(&kc.release())
        .unwrap()
        .partition = false;
    prepare_existing_pair_with(&kc, &cert, &key, sha1).unwrap();
    assert!(fake.0.borrow().keychains[&kc.release()].partition);
    assert_eq!(fake.calls("security", "import").len(), 2);
    assert_eq!(fake.calls("codesign", "--force").len(), 2);
}

#[test]
fn a_missing_or_rejected_password_preserves_the_old_keychain_and_rebuilds_without_input() {
    for wrong_password in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path();
        std::fs::create_dir_all(home.join("Library/Keychains")).unwrap();
        std::fs::create_dir_all(home.join(".aterm/apple")).unwrap();
        let sha1 = "A1B2C3D4E5F60718293A4B5C6D7E8F9012345678";
        let release = keychain_path(home);
        std::fs::write(&release, b"preserve this unknown old keychain").unwrap();
        let mut world = World::new(sha1);
        world.keychains.insert(release.clone(), Kc::holding(sha1));
        world.search_list.push(release.clone());
        if wrong_password {
            let _ = create_password(home).unwrap();
        }
        let fake = Fake::new(world);
        let kc = Keychains::new(home, &fake);
        prepare_existing_pair_with(&kc, &home.join("cert"), &home.join("key"), sha1).unwrap();
        let quarantine = home.join(".aterm/apple/quarantine");
        let slot = std::fs::read_dir(&quarantine)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::read(slot.join(FILE_NAME)).unwrap(),
            b"preserve this unknown old keychain"
        );
        assert!(password_path(home).is_file());
        assert_eq!(fake.calls("security", "create-keychain").len(), 1);
        assert_eq!(fake.calls("codesign", "--force").len(), 1);
    }

    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    std::fs::create_dir_all(home.join("Library/Keychains")).unwrap();
    let release = keychain_path(home);
    std::fs::write(&release, b"keep when password lstat fails").unwrap();
    std::fs::create_dir_all(home.join(".aterm")).unwrap();
    let apple_dir = home.join(".aterm/apple");
    std::os::unix::fs::symlink(&apple_dir, &apple_dir).unwrap();
    let fake = Fake::new(World::new("A1B2C3D4E5F60718293A4B5C6D7E8F9012345678"));
    let kc = Keychains::new(home, &fake);
    assert!(recover_owned_keychain(&kc).is_err());
    assert_eq!(
        std::fs::read(&release).unwrap(),
        b"keep when password lstat fails"
    );
}
