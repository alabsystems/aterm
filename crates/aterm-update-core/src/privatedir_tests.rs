// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn an_already_private_dir_is_not_rewritten_and_a_drifted_one_is_hardened() {
    let scratch = crate::test_scratch_dir("privatedir");
    let dir = scratch.path().join("private");
    ensure_private_dir(&dir).unwrap();
    let stamp = || {
        let m = std::fs::symlink_metadata(&dir).unwrap();
        (
            m.dev(),
            m.ino(),
            m.ctime(),
            m.ctime_nsec(),
            m.mode() & 0o7777,
        )
    };
    let before = stamp();
    assert_eq!(before.4, 0o700);
    CHMOD_CALLS.with(|c| c.set(0));
    ensure_private_dir(&dir).unwrap();
    assert_eq!(
        CHMOD_CALLS.with(std::cell::Cell::get),
        0,
        "an unchanged 0700 directory must not run chmod"
    );
    assert_eq!(stamp(), before, "an unchanged directory was written");

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    CHMOD_CALLS.with(|c| c.set(0));
    ensure_private_dir(&dir).unwrap();
    assert_eq!(
        CHMOD_CALLS.with(std::cell::Cell::get),
        1,
        "a drifted mode triggers one chmod"
    );
    assert_eq!(stamp().4, 0o700, "a drifted mode is hardened");

    let link = scratch.path().join("private.link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    CHMOD_CALLS.with(|c| c.set(0));
    assert!(ensure_private_dir(&link).is_err(), "a symlink is refused");
    assert_eq!(CHMOD_CALLS.with(std::cell::Cell::get), 0);

    let file = scratch.path().join("regular.file");
    std::fs::write(&file, b"x").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    CHMOD_CALLS.with(|c| c.set(0));
    assert!(
        ensure_private_dir(&file).is_err(),
        "a regular file is refused"
    );
    assert_eq!(CHMOD_CALLS.with(std::cell::Cell::get), 0);
    assert_eq!(
        std::fs::symlink_metadata(&file).unwrap().mode() & 0o7777,
        0o644
    );
}

const OPEN_UMASK_ENV: &str = "ATUC_PRIVATEDIR_OPEN_UMASK_DIR";
const OPEN_UMASK_PROBE: &str =
    "privatedir::tests::missing_ancestors_are_never_shared_whatever_the_umask";

fn probe_missing_ancestors_are_never_shared_whatever_the_umask_if_asked() -> bool {
    let Some(root) = std::env::var_os(OPEN_UMASK_ENV) else {
        return false;
    };
    let mode = |p: &Path| {
        format!(
            "{:o}",
            std::fs::symlink_metadata(p).unwrap().mode() & 0o7777
        )
    };
    let existing = Path::new(&root).join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o775)).unwrap();
    for (umask, ancestor) in [(0o002, "755"), (0o000, "755"), (0o077, "700")] {
        // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail;
        // this process runs this one test (`--exact`), so no other create sees the mask.
        unsafe { libc::umask(umask) };
        let top = existing.join(format!("umask-{umask:03o}"));
        let leaf = top.join("aterm").join("pkg");
        ensure_private_dir(&leaf).unwrap();
        for made in [&top, &top.join("aterm")] {
            assert_eq!(
                mode(made),
                ancestor,
                "{}: a missing ancestor under umask {umask:03o}",
                made.display()
            );
        }
        assert_eq!(mode(&leaf), "700", "the leaf under umask {umask:03o}");
    }
    assert_eq!(mode(&existing), "775", "an existing ancestor was rewritten");
    true
}

#[test]
fn missing_ancestors_are_never_shared_whatever_the_umask() {
    if probe_missing_ancestors_are_never_shared_whatever_the_umask_if_asked() {
        return;
    }
    let scratch = crate::test_scratch_dir("open-umask");
    let out = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args(["--exact", "--nocapture", OPEN_UMASK_PROBE])
        .env(OPEN_UMASK_ENV, scratch.path().as_os_str())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the probe under open umasks: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}
