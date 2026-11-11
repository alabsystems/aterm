// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fixed-only inert admission controls; no file made here is executed.

use super::*;

fn inert_hold(holds: &Path, exe: &Path, store: Option<&Path>) -> std::io::Result<Hold> {
    let forwarder = atpkg::platform::ProbeForwarder {
        executable: std::env::current_exe()?,
        parent_pid: std::process::id(),
    };
    let launch = Launch::inspect_using(exe, Some(&forwarder))?;
    register_launch(holds, launch, store)
}

#[test]
fn a_generated_apostrophe_wrapper_holds_its_store_target_and_native_bytes_stay_native() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let prefix = scratch.path().join("person's prefix");
    let store = prefix.join("store");
    let target = store.join("claude/17/bin/claude");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    // Inert non-UTF8 image-shaped bytes establish that inspection does not require UTF-8.
    std::fs::write(&target, [0xcf, 0xfa, 0xed, 0xfe, 0xff]).unwrap();
    assert_eq!(
        inspect_program_file(&target).unwrap(),
        std::fs::canonicalize(&target).unwrap()
    );

    let wrapper = prefix.join("agents/claude");
    std::fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
    // The actual producer renders data only: claude does not enter the targo embedder arm.
    let rendered = atpkg::platform::twin_executable_to_env(
        &wrapper,
        &target,
        &atpkg::shim_env::ShimEnv::NONE,
        "",
    )
    .unwrap();
    assert!(
        std::str::from_utf8(&rendered.body)
            .unwrap()
            .contains("'\\''")
    );
    std::fs::write(&wrapper, &rendered.body).unwrap();
    assert_eq!(
        inspect_program_file(&wrapper).unwrap(),
        std::fs::canonicalize(&target).unwrap()
    );
    let holds = scratch.path().join("native-holds");
    let hold = inert_hold(&holds, &wrapper, Some(&store)).unwrap();
    assert!(
        hold.main
            .iter()
            .all(|mark| mark.store.is_some() && mark.dir.is_none()),
        "registration belongs to the executed store build"
    );
    let marks = store.join("claude/17.unproven.d");
    assert!(marks.is_dir());
    hold.never_started();
    assert!(!marks.exists());

    let alias = scratch.path().join("prefix-alias");
    std::os::unix::fs::symlink(&prefix, &alias).unwrap();
    let aliased_hold = inert_hold(
        &holds,
        &alias.join("agents/claude"),
        Some(&alias.join("store")),
    )
    .unwrap();
    assert!(
        aliased_hold.main.iter().all(|mark| mark.store.is_some()),
        "store and target use the same physical prefix spelling"
    );
    aliased_hold.never_started();
    assert!(!marks.exists());

    std::fs::write(
        &wrapper,
        b"#!/bin/sh\n# atpkg shim unrecognized target\nexec dynamically-chosen\n",
    )
    .unwrap();
    assert!(
        inert_hold(&holds, &wrapper, Some(&store)).is_err(),
        "never hold just an unresolved managed wrapper"
    );
    for body in [
        "#!/bin/sh\n# unknown legacy policy\nexec \"$OTHER_PROGRAM\"\n",
        "#!/bin/sh\n# padded legacy policy\nexec \"$OTHER_PROGRAM\"\n:\n",
        "#!/bin/sh\nif [ -n \"$OTHER_PROGRAM\" ]; then\n  exec \"$OTHER_PROGRAM\"\nfi\n",
    ] {
        std::fs::write(&wrapper, body).unwrap();
        assert!(
            inert_hold(&holds, &wrapper, Some(&store)).is_err(),
            "a managed path cannot hide an unknown redirect: {body}"
        );
        assert!(
            !held(&holds, &wrapper, std::time::SystemTime::now()),
            "no native fallback mark was registered"
        );
    }
}

#[test]
fn malformed_nonempty_native_stamps_do_not_use_expiry_fallback() {
    use std::time::{Duration, SystemTime};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir = scratch.path().join("start");
    std::fs::create_dir(&dir).unwrap();
    let stamp = dir.join(aterm_uds::deadline::EXEC_STAMP);
    let now = SystemTime::now() + aterm_uds::deadline::FIRST_EXEC_HANG + Duration::from_secs(1);
    std::fs::write(&stamp, b"").unwrap();
    assert!(
        !directory_holds_at(&dir, now, None),
        "intentional blank uses old mtime"
    );
    std::fs::write(&stamp, b"1000").unwrap();
    assert!(directory_holds_at(&dir, now, Some(1000)));
    assert!(
        !directory_holds_at(&dir, now, Some(999)),
        "valid prior boot stamp expires"
    );
    assert!(
        !directory_holds_at(&dir, now, None),
        "unavailable clock keeps the mtime fallback"
    );
    let spent = u64::try_from(aterm_uds::deadline::FIRST_EXEC_HANG.as_millis()).unwrap();
    assert!(!directory_holds_at(&dir, now, Some(1000 + spent)));
    for malformed in ["garbage", "+1000", "18446744073709551616", "-1", " \n"] {
        std::fs::write(&stamp, malformed).unwrap();
        for awake in [Some(0), Some(u64::MAX), None] {
            assert!(
                directory_holds_at(&dir, now, awake),
                "{malformed:?}, {awake:?}"
            );
        }
    }
}

#[test]
fn both_probe_descriptors_keep_the_admitted_native_image_after_alias_retarget() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let a = root.join("person's native A");
    let b = root.join("native B");
    std::fs::write(&a, [0xcf, 0xfa, 0xed, 0xfe, 0xff]).unwrap();
    std::fs::write(&b, b"different inert image").unwrap();
    let holds = root.join("holds");
    let ordinary = prepare(&a, Some(&holds), ProbeWord::Version).unwrap();
    assert_eq!(ordinary.command.get_program(), a.as_os_str());
    assert_eq!(
        ordinary.command.get_args().collect::<Vec<_>>(),
        ["--version"]
    );
    assert!(held(&holds, &a, std::time::SystemTime::now()));
    ordinary.hold.unwrap().ran();
    assert!(!held(&holds, &a, std::time::SystemTime::now()));

    let alias = root.join("native-alias");
    for word in [ProbeWord::Version, ProbeWord::Help] {
        let expected = word.word();
        std::os::unix::fs::symlink(&a, &alias).unwrap();
        let prepared = prepare(&alias, Some(&holds), word).unwrap();
        // The deterministic publication is strictly after preparation and before any spawn.
        let next = root.join("next-alias");
        std::os::unix::fs::symlink(&b, &next).unwrap();
        std::fs::rename(next, &alias).unwrap();
        assert_eq!(std::fs::canonicalize(&alias).unwrap(), b);
        assert_eq!(prepared.command.get_program(), a.as_os_str());
        assert_eq!(prepared.command.get_args().collect::<Vec<_>>(), [expected]);
        let hold = prepared.hold.unwrap();
        match &hold.launch {
            Launch::Direct { program, argv0, .. } => {
                assert_eq!(program, &a);
                assert_eq!(argv0, &alias, "the same builder preserves native argv0");
            }
            Launch::Policy { .. } => panic!("native image became shell policy"),
        }
        assert!(held(&holds, &a, std::time::SystemTime::now()));
        assert!(!held(&holds, &b, std::time::SystemTime::now()));
        hold.ran();
        std::fs::remove_file(&alias).unwrap();
    }

    let a_dir = root.join("directory-A");
    let b_dir = root.join("directory-B");
    std::fs::create_dir(&a_dir).unwrap();
    std::fs::create_dir(&b_dir).unwrap();
    std::fs::write(b_dir.join("future"), b"inert later candidate").unwrap();
    std::os::unix::fs::symlink(&a_dir, &alias).unwrap();
    let missing = prepare(&alias.join("future"), Some(&holds), ProbeWord::Help).unwrap();
    let next = root.join("next-parent");
    std::os::unix::fs::symlink(&b_dir, &next).unwrap();
    std::fs::rename(next, &alias).unwrap();
    assert_eq!(
        missing.command.get_program(),
        a_dir.join("future").as_os_str(),
        "ordinary failed-spawn admission still pins its existing physical parent"
    );
    assert!(!held(&holds, &b_dir, std::time::SystemTime::now()));
    missing.hold.unwrap().never_started();
}

#[test]
fn both_probe_descriptors_keep_a_complete_factory_twin_after_normal_publication() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let prefix = root.join("person's managed prefix");
    let store = prefix.join("store");
    let a = store.join("claude/17/bin/claude");
    let b = store.join("claude/18/bin/claude");
    let wrapper = prefix.join("agents/claude");
    let atpkg = prefix.join("bin/atpkg");
    let tap = prefix.join("bin/aterm");
    for file in [&a, &b, &atpkg, &tap] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, b"inert owned file; never executed").unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
    let env = atpkg::shim_env::ShimEnv::admit(&[
        "DISABLE_AUTOUPDATER=1".to_owned(),
        "PROBE_LITERAL=a b'quoted".to_owned(),
    ])
    .unwrap();
    // These are precisely the pure factory calls used by activate::reconcile_agents.
    let mut prelude = atpkg::platform::twin_prelude(
        "claude",
        &prefix,
        Some(&atpkg),
        atpkg::selfupdate::verbs_of("claude"),
        &atpkg::yolo::policy_of("claude"),
    );
    prelude.push_str(&atpkg::launch_tap::sh_prelude("claude", Some(&atpkg)));
    assert!(
        prelude.contains("__atpkg_tap="),
        "the current usage tap is really present"
    );
    assert!(
        prelude.contains("case \"$1\" in"),
        "the current self-update policy is really present"
    );
    let rendered =
        |target| atpkg::platform::twin_executable_to_env(&wrapper, target, &env, &prelude).unwrap();
    let holds = root.join("holds");
    for word in [ProbeWord::Version, ProbeWord::Help] {
        atpkg::lay::write_in_process(&rendered(&a)).unwrap();
        let expected_word = word.word();
        let prepared = prepare_using(&wrapper, Some(&holds), word, |holds, exe| {
            inert_hold(holds, exe, Some(&store))
        })
        .unwrap();
        let args = prepared.command.get_args().collect::<Vec<_>>();
        assert_eq!(prepared.command.get_program(), "/bin/sh");
        assert_eq!(args[0], "-c");
        assert_eq!(args[2], wrapper.as_os_str(), "original shell $0");
        assert_eq!(args[3], expected_word, "one unchanged probe argument");
        let hold = prepared.hold.as_ref().unwrap();
        match &hold.launch {
            Launch::Policy { images, body, .. } => {
                assert!(
                    images
                        .iter()
                        .any(|image| image.physical == a && !image.helper)
                );
                assert!(
                    images
                        .iter()
                        .any(|image| image.physical == tap && image.helper)
                );
                assert!(body.contains("DISABLE_AUTOUPDATER='1'"));
                assert!(body.contains("PROBE_LITERAL='a b'\\''quoted'"));
            }
            Launch::Direct { .. } => panic!("full policy was discarded"),
        }
        // Ordinary positives above precede the actual normal publication race.
        atpkg::lay::write_in_process(&rendered(&b)).unwrap();
        assert_eq!(
            super::super::twin::twin_binary(&std::fs::read_to_string(&wrapper).unwrap()),
            Some(b.clone())
        );
        match &hold.launch {
            Launch::Policy { images, body, .. } => {
                assert!(
                    images
                        .iter()
                        .any(|image| image.physical == a && image.argv0 == a && !image.helper)
                );
                assert!(!images.iter().any(|image| image.physical == b));
                assert!(!body.contains("18/bin/claude"));
                assert_eq!(
                    prepared.command.get_args().nth(1),
                    Some(std::ffi::OsStr::new(body))
                );
            }
            Launch::Direct { .. } => unreachable!(),
        }
        assert!(store.join("claude/17.unproven.d").is_dir());
        assert!(!store.join("claude/18.unproven.d").exists());
        prepared.hold.unwrap().never_started();
        assert!(!store.join("claude/17.unproven.d").exists());
    }
}

#[test]
fn main_exit_never_borrows_proof_for_an_uncertain_policy_helper() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let main = root.join("main");
    let helper = root.join("helper");
    std::fs::write(&main, b"inert main").unwrap();
    std::fs::write(&helper, b"inert helper").unwrap();
    let holds = root.join("holds");
    let descriptor = || Launch::Policy {
        images: vec![
            atpkg::platform::ProbeImage {
                physical: main.clone(),
                argv0: main.clone(),
                helper: false,
            },
            atpkg::platform::ProbeImage {
                physical: helper.clone(),
                argv0: helper.clone(),
                helper: true,
            },
        ],
        body: String::new(),
        shell0: main.clone(),
    };
    let first = register_launch(&holds, descriptor(), None).unwrap();
    assert!(held(&holds, &main, std::time::SystemTime::now()));
    assert!(held(&holds, &helper, std::time::SystemTime::now()));
    first.ran();
    assert!(
        !held(&holds, &main, std::time::SystemTime::now()),
        "natural main proof releases its image"
    );
    assert!(
        held(&holds, &helper, std::time::SystemTime::now()),
        "a helper signal can be hidden by the shell"
    );

    let second = register_launch(&holds, descriptor(), None).unwrap();
    second.never_started();
    assert!(
        !held(&holds, &main, std::time::SystemTime::now()),
        "failed spawn releases every new main mark"
    );
    assert!(
        held(&holds, &helper, std::time::SystemTime::now()),
        "failed spawn cannot erase the earlier uncertain helper"
    );

    let same_image = Launch::Policy {
        images: vec![
            atpkg::platform::ProbeImage {
                physical: main.clone(),
                argv0: main.clone(),
                helper: false,
            },
            atpkg::platform::ProbeImage {
                physical: main.clone(),
                argv0: helper.clone(),
                helper: true,
            },
        ],
        body: String::new(),
        shell0: main.clone(),
    };
    register_launch(&holds, same_image, None).unwrap().ran();
    assert!(
        held(&holds, &main, std::time::SystemTime::now()),
        "main proof cannot release a different helper start even at the same physical path"
    );
}
