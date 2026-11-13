// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_required_setting_write_waits_for_its_scheduled_child() {
    let mut child =
        aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/sh")));
    child.args(["-c", "printf ready"]);
    assert_eq!(
        required_write_after(&mut child, std::time::Duration::from_secs(6)),
        Some(b"ready".to_vec())
    );

    let root = crate::testdir::dir("expired-setting-write-");
    let marker = root.join("must-not-run");
    let mut late = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/sh")));
    late.args(["-c", "printf late > \"$1\"", "fixture"])
        .arg(&marker);
    assert_eq!(
        required_write_after(&mut late, std::time::Duration::from_secs(61)),
        None
    );
    assert!(!marker.exists());

    let mut refused =
        aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/sh")));
    refused.args(["-c", "printf refused; exit 7"]);
    assert_eq!(
        required_write_after(&mut refused, std::time::Duration::from_secs(6)),
        None
    );
}

#[test]
fn the_rosetta_probe_reads_a_file_and_never_guesses_an_unsearchable_one() {
    let root = crate::testdir::dir("atpkg-rosetta-probe-");
    let runtime = root.join("oah").join("libRosettaRuntime");
    assert_eq!(rosetta_present(&runtime), Ok(false), "no parent");
    fs::create_dir_all(runtime.parent().unwrap()).unwrap();
    assert_eq!(rosetta_present(&runtime), Ok(false), "nothing there");
    fs::create_dir(&runtime).unwrap();
    assert_eq!(rosetta_present(&runtime), Ok(false), "a directory");
    fs::remove_dir(&runtime).unwrap();
    fs::write(&runtime, b"\xcf\xfa\xed\xfe").unwrap();
    assert_eq!(rosetta_present(&runtime), Ok(true));
    let under_a_file = runtime.join("libRosettaRuntime");
    assert_eq!(rosetta_present(&under_a_file), Ok(false), "ENOTDIR");
    let oah = runtime.parent().unwrap();
    fs::set_permissions(oah, Permissions::from_mode(0o000)).unwrap();
    if our_uid() != 0 {
        assert!(rosetta_present(&runtime).is_err(), "EACCES is no answer");
    }
    fs::set_permissions(oah, Permissions::from_mode(0o700)).unwrap();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_shared_dir_already_0755_is_not_rewritten_and_a_drifted_one_is_forced() {
    let root = crate::testdir::dir("atpkg-shared-dir-");
    let dir = root.join("prefix");
    let stamp = |p: &Path| {
        let m = fs::symlink_metadata(p).unwrap();
        (
            m.mode() & 0o7777,
            m.ctime(),
            m.ctime_nsec(),
            m.mtime(),
            m.mtime_nsec(),
        )
    };
    ensure_shared_dir(&dir).unwrap();
    let first = stamp(&dir);
    assert_eq!(first.0, 0o755, "created shared: {:o}", first.0);
    std::thread::sleep(std::time::Duration::from_millis(20));
    ensure_shared_dir(&dir).unwrap();
    assert_eq!(
        stamp(&dir),
        first,
        "an unchanged 0755 directory was written"
    );
    for drifted in [0o700, 0o775, 0o1755] {
        fs::set_permissions(&dir, Permissions::from_mode(drifted)).unwrap();
        ensure_shared_dir(&dir).unwrap();
        assert_eq!(stamp(&dir).0, 0o755, "{drifted:o} not forced to 0755");
    }
    let link = root.join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    fs::set_permissions(&dir, Permissions::from_mode(0o700)).unwrap();
    let refused = ensure_shared_dir(&link);
    let named = stamp(&dir).0;
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        refused.map_err(|e| e.kind()),
        Err(io::ErrorKind::PermissionDenied)
    );
    assert_eq!(named, 0o700, "the refused link's target was chmodded");
}

#[test]
fn the_account_home_resolves_on_the_machine_running_this_test() {
    let account = account_home().expect(
        "getpwuid_r must resolve this account's home; a None here disables the \
         [machine] doctor everywhere",
    );
    assert!(
        account.is_absolute(),
        "a passwd home is an absolute path: {}",
        account.display()
    );
    assert!(
        !account.as_os_str().is_empty(),
        "an empty pw_dir is not a home"
    );
    assert!(
        account.is_dir(),
        "the account home must exist: {}",
        account.display()
    );
    assert_eq!(account_home(), Some(account));
}
