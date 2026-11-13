// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::Cell;
use std::sync::Mutex;

#[test]
fn an_unreadable_floor_does_not_admit_the_installed_build() {
    use std::os::unix::fs::PermissionsExt;
    let staging = crate::paths::Staging::scratch("installed-yank-floor");
    let checks = |build: u64| crate::verify::BundleChecks {
        policy: Ok(()),
        build: Ok(build),
        commit: Ok("0123456789abcdef0123456789abcdef01234567".to_string()),
        team: None,
        short_version: Some(Ok(format!("0.{build}.0"))),
    };
    let facts = |build| super::installed_facts_from(checks(build), Some(&staging));
    let due = |build| super::installed_announcement_due(Some(&staging), build, 500, None);
    std::fs::write(staging.floor(), "min_build = 530\n").unwrap();
    assert_eq!(super::installed_build_yanked(Some(&staging), 520), Ok(true));
    assert_eq!(
        super::installed_build_yanked(Some(&staging), 530),
        Ok(false)
    );
    assert_eq!(super::installed_build_yanked(None, 520), Ok(false));
    assert!(facts(520).is_ok_and(|facts| facts.yanked));
    assert!(facts(530).is_ok_and(|facts| !facts.yanked && facts.build_number == 530));
    assert!(!due(520), "a yanked build is not announced");
    assert!(due(530));
    assert!(!super::installed_announcement_due(
        Some(&staging),
        530,
        500,
        Some(530)
    ));
    assert!(!super::installed_announcement_due(
        Some(&staging),
        530,
        530,
        None
    ));
    assert!(super::installed_announcement_due(None, 520, 500, None));
    let mode =
        |bits| std::fs::set_permissions(staging.floor(), std::fs::Permissions::from_mode(bits));
    mode(0o000).unwrap();
    if std::fs::File::open(staging.floor()).is_ok() {
        aterm_uds::host::could_not_run(
            "a mode-0 file opened anyway: root, or a file system that ignores modes",
        );
    }
    let unreadable = super::installed_build_yanked(Some(&staging), 520);
    let reported = facts(520);
    let announced = due(520);
    mode(0o600).unwrap();
    assert!(
        unreadable
            .as_ref()
            .is_err_and(|e| crate::is_passing_refusal(e)),
        "an unreadable floor must be a passing refusal, not a verdict: {unreadable:?}"
    );
    assert!(
        reported
            .as_ref()
            .is_err_and(|e| crate::is_passing_refusal(e)),
        "the facts reported an installed build an unreadable floor may yank: {reported:?}"
    );
    assert!(
        !announced,
        "the checker announced an installed build an unreadable floor may yank"
    );
}

#[test]
fn nonregular_installed_plists_never_reach_the_reader() {
    use std::os::unix::fs::OpenOptionsExt as _;

    let dir = crate::paths::scratch_dir("observed-plist-special");
    let app = dir.path().join("aterm.app");
    let plist = app.join("Contents/Info.plist");
    std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
    let path = std::ffi::CString::new(plist.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: path is NUL-terminated and remains live for the call.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&plist)
        .unwrap();
    let cache = Mutex::new(None);
    let reads = Cell::new(0);
    let refused = || {
        assert!(
            super::observed_build_with(&cache, &app, |_| {
                reads.set(reads.get() + 1);
                Ok(900)
            })
            .is_err()
        );
        assert_eq!(reads.get(), 0);
    };
    refused();
    drop(writer);
    refused();
    std::fs::remove_file(&plist).unwrap();
    std::fs::create_dir(&plist).unwrap();
    refused();
    std::fs::remove_dir(&plist).unwrap();
    let real = dir.path().join("real.plist");
    std::fs::write(&real, "900").unwrap();
    std::os::unix::fs::symlink(&real, &plist).unwrap();
    refused();
}

#[test]
fn a_bundle_swap_during_read_cannot_poison_the_restored_bundles_cache() {
    let model = aterm_models_update::native_update_installed_cache_model();
    let mut state = model.init_state();
    assert!(model.fire("Open", &mut state));
    let dir = crate::paths::scratch_dir("observed-plist-swap");
    let app = dir.path().join("aterm.app");
    let next = dir.path().join("next.app");
    let previous = dir.path().join("previous.app");
    for (root, build) in [(&app, "900"), (&next, "901")] {
        std::fs::create_dir_all(root.join("Contents")).unwrap();
        std::fs::write(root.join("Contents/Info.plist"), format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleVersion</key><string>{build}</string></dict></plist>"
        )).unwrap();
    }
    let cache = Mutex::new(None);
    let initial = super::observed_build_with(&cache, &app, |file| {
        std::fs::rename(&app, &previous).unwrap();
        std::fs::rename(&next, &app).unwrap();
        assert!(model.fire("Replace", &mut state));
        assert!(model.fire("Read", &mut state));
        super::verify::bundle_build_number_from_file(file)
    })
    .unwrap();
    assert_eq!(initial - 899, state["cached_build"] as u64);
    std::fs::rename(&app, &next).unwrap();
    std::fs::rename(&previous, &app).unwrap();
    assert!(model.fire("Restore", &mut state));
    assert!(model.fire("Observe", &mut state));
    assert_eq!(
        super::observed_build_with(&cache, &app, super::verify::bundle_build_number_from_file),
        Ok(state["observed_build"] as u64 + 899),
        "restoring the old bundle must not reuse the replacement's build"
    );
    assert!(model.check_invariant("ObservedBuildMatchesFile", &state));

    let mutant = aterm_spec::interp::with_buggy(&model, 1);
    let mut poisoned = mutant.init_state();
    for action in ["Open", "Replace", "Read", "Restore", "Observe"] {
        assert!(mutant.fire(action, &mut poisoned), "{action}");
    }
    assert!(!mutant.check_invariant("ObservedBuildMatchesFile", &poisoned));
}

#[test]
fn an_unchanged_plist_is_read_once_and_a_moved_one_again() {
    let dir = crate::paths::scratch_dir("observed-plist");
    let app = dir.path().join("aterm.app");
    let plist = app.join("Contents/Info.plist");
    std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
    let cache = Mutex::new(None);
    let reads = Cell::new(0u32);
    let answer = Cell::new(Some(900u64));
    let observe = || {
        super::observed_build_with(&cache, &app, |_| {
            reads.set(reads.get() + 1);
            answer
                .get()
                .ok_or_else(|| "PlistBuddy CFBundleVersion timed out".to_string())
        })
    };
    std::fs::write(&plist, "build 900").unwrap();
    for _ in 0..5 {
        assert_eq!(observe(), Ok(900));
    }
    assert_eq!(reads.get(), 1, "five cycles, one read");

    answer.set(Some(901));
    std::fs::write(&plist, "build 9010").unwrap();
    assert_eq!(observe(), Ok(901), "rewritten, another size");
    assert_eq!(reads.get(), 2);

    let mtime = std::fs::metadata(&plist).unwrap().modified().unwrap();
    answer.set(Some(902));
    std::fs::write(&plist, "build 9020").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&plist)
        .and_then(|f| f.set_modified(mtime))
        .unwrap();
    assert_eq!(observe(), Ok(902), "the same size under a restored mtime");
    assert_eq!(reads.get(), 3);

    answer.set(Some(903));
    let swapped = app.join("Contents/Info.plist.new");
    std::fs::write(&swapped, "build 9030").unwrap();
    std::fs::rename(&swapped, &plist).unwrap();
    assert_eq!(observe(), Ok(903), "a new inode");
    assert_eq!(reads.get(), 4);
    assert_eq!(observe(), Ok(903));
    assert_eq!(reads.get(), 4, "and then remembered");

    answer.set(None);
    std::fs::write(&plist, "build 9040").unwrap();
    assert!(observe().is_err());
    answer.set(Some(904));
    assert_eq!(observe(), Ok(904), "a failed read is not remembered");
    assert_eq!(reads.get(), 6);

    std::fs::remove_file(&plist).unwrap();
    answer.set(None);
    assert!(observe().is_err());
    assert!(observe().is_err());
    assert_eq!(reads.get(), 6, "a missing plist starts no reader");
}
