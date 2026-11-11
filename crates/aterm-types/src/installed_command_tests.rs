// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};

fn image(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"an inert executable fixture").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn installation_and_removal_keep_the_selected_runtime() {
    let root = crate::test_scratch_dir("instcmd");
    let uid = aterm_uds::peer::our_uid();
    let home = root.path().join("home");
    let first = root.path().join("source store/aterm");
    let second = root.path().join("release store/aterm");
    image(&first);
    image(&second);
    let declaration = path(&home);
    let dir = declaration.parent().unwrap();
    std::fs::create_dir_all(dir).unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(record(&home, &first, &second).is_err());
    assert!(
        !declaration.exists(),
        "another executable cannot select this runtime"
    );
    assert_eq!(
        selected(&home, uid).unwrap(),
        None,
        "no installer recorded one"
    );

    record(&home, &first, &first).unwrap();
    assert_eq!(
        selected(&home, uid).unwrap().unwrap().executable(),
        first,
        "the recorded selection reads back"
    );
    std::fs::set_permissions(&declaration, std::fs::Permissions::from_mode(0o622)).unwrap();
    assert_eq!(
        selected(&home, uid).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "a record others may write selects nothing"
    );
    std::fs::set_permissions(&declaration, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let before = std::fs::read(&declaration).unwrap();
    assert_eq!(Selection::decode(&before).unwrap().executable(), first);
    aterm_uds::atomic_file::write(&first, b"same-path update or rollback", 0o755).unwrap();
    assert_eq!(std::fs::read(&declaration).unwrap(), before);

    record(&home, &second, &second).unwrap();
    std::fs::remove_file(&first).unwrap();
    assert!(!remove_if_matches(&home, &first, uid).unwrap());
    assert!(
        !remove_if_matches(&home, &second, uid).unwrap(),
        "a kept runtime stays selected"
    );
    assert_eq!(
        Selection::decode(&std::fs::read(&declaration).unwrap())
            .unwrap()
            .executable(),
        second
    );
    std::fs::remove_file(&second).unwrap();
    symlink(root.path().join("not installed yet"), &second).unwrap();
    assert!(
        !remove_if_matches(&home, &second, uid).unwrap(),
        "a dangling entry is not an uninstall"
    );
    std::fs::remove_file(&second).unwrap();
    assert!(remove_if_matches(&home, &second, uid).unwrap());
    assert!(!declaration.exists());
}

#[test]
fn a_waiting_installer_preserves_a_newer_selection() {
    use aterm_uds::file_lock::{LockFile, with_lock_wait_observer};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let root = crate::test_scratch_dir("instcmd");
    let home = root.path().join("home");
    let first = root.path().join("first/aterm");
    let waiting = root.path().join("waiting/aterm");
    let newer = root.path().join("newer/aterm");
    for target in [&first, &waiting, &newer] {
        image(target);
    }
    record(&home, &first, &first).unwrap();
    let declaration = path(&home);
    let held = LockFile::take(
        &declaration
            .parent()
            .unwrap()
            .join(".installed-command.lock"),
        Instant::now() + Duration::from_secs(60),
        "fixture lock is busy",
    )
    .unwrap();
    let (started, observed) = mpsc::channel();
    let account = home.clone();
    let installer = std::thread::spawn(move || {
        let mut started = Some(started);
        with_lock_wait_observer(
            move |_| {
                if let Some(started) = started.take() {
                    started.send(()).unwrap();
                }
            },
            || record(&account, &waiting, &waiting),
        )
    });
    // Observe real lock contention; the minute is a hang detector, not a latency budget.
    let blocked = observed.recv_timeout(Duration::from_secs(60));
    let published = Selection::new(newer.clone()).unwrap().encode().unwrap();
    let write = aterm_uds::atomic_file::write(&declaration, &published, 0o600);
    drop(held);
    let result = installer.join().unwrap();
    blocked.unwrap();
    write.unwrap();
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(
        Selection::decode(&std::fs::read(&declaration).unwrap())
            .unwrap()
            .executable(),
        newer
    );
}
