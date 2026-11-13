// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn required_bootout_settlement_waits_for_a_scheduled_observation() {
    let mut manager = FakeLaunchd {
        loaded: Some(Domain::Gui),
        ..FakeLaunchd::default()
    };
    let mut first = true;
    let result = settle_launchd_bootout_at(
        Domain::Gui,
        Job::Subscriber.label(),
        &mut |call| {
            let out = manager.answer(call)?;
            if first {
                first = false;
                manager.loaded = None;
            }
            Ok(out)
        },
        Instant::now() - Duration::from_secs(31),
        || {},
    );
    assert!(result.is_ok(), "{result:?}");
    let print = launchctl_argv(0, Launchctl::Print(Domain::Gui, Job::Subscriber.label()));
    assert_eq!(manager.calls, [print.clone(), print.clone()]);

    let mut held = FakeLaunchd {
        loaded: Some(Domain::Gui),
        ..FakeLaunchd::default()
    };
    let result = settle_launchd_bootout_at(
        Domain::Gui,
        Job::Subscriber.label(),
        &mut |call| held.answer(call),
        Instant::now() - Duration::from_secs(61),
        || {},
    );
    assert!(
        result.is_err(),
        "an unsettled job still refuses after the hang detector"
    );
    assert_eq!(held.loaded, Some(Domain::Gui));
    assert_eq!(held.calls, [print]);
}

#[cfg(unix)]
#[test]
fn required_service_action_waits_for_a_scheduled_child() {
    let argv = ["-c".into(), "printf ready".into()];
    let program = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let out = run_at(&program, &argv, Instant::now() - Duration::from_secs(31))
        .expect("required service action completes");
    assert!(out.status.success());
    assert_eq!(out.stdout, b"ready");
    assert!(run_at(&program, &argv, Instant::now() - Duration::from_secs(61)).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn signed_repair_preserves_an_existing_inherited_path_and_exact_rollback() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    let old_program = home.join("old-aterm");
    let new_program = home.join("new-aterm");
    std::fs::write(&old_program, b"old image").unwrap();
    std::fs::write(&new_program, b"new image").unwrap();
    let args = vec!["secrets".to_owned(), "watch".to_owned()];
    let generated = plist_text_in(
        Job::Subscriber.label(),
        &old_program,
        &args,
        &log_path(home, Job::Subscriber),
        Domain::Gui,
        None,
    );
    // The installed subscriber inherited launchd's PATH: its owned definition sets
    // HOME alone. Remove only the explicit PATH field to reproduce that input.
    let path_at = generated.find("\t\t<key>PATH</key>\n").unwrap();
    let path_end =
        path_at + generated[path_at..].find("</string>\n").unwrap() + "</string>\n".len();
    let mut definition = generated.clone();
    definition.replace_range(path_at..path_end, "");
    write_unit_file(&unit_file(home, Job::Subscriber), &definition).unwrap();

    let observed = repair_observe_at(home, Job::Subscriber)
        .expect("an owned subscriber can inherit launchd's PATH");
    assert!(!observed.off);
    assert!(observed.pin.is_none());
    let old = observed.unit.unwrap();
    assert_eq!(old.definition, definition);
    let next = repair_unit(home, Job::Subscriber, &old, &new_program, &"a".repeat(64)).unwrap();
    assert_eq!(next.args, args);
    assert!(!next.definition.contains("<key>PATH</key>"));
    repair_check_unit(home, Job::Subscriber, &next).unwrap();

    let mut changed_path = old.clone();
    changed_path.definition = generated.replace(
        &aterm_codec::xml::escape(
            &home
                .join("Library/Application Support/aterm/pkg/bin")
                .to_string_lossy(),
        ),
        "/untrusted/bin",
    );
    assert!(repair_check_unit(home, Job::Subscriber, &changed_path).is_err());
    let mut changed_home = old.clone();
    changed_home.definition = definition.replace(
        &format!(
            "<string>{}</string>",
            aterm_codec::xml::escape(&home.to_string_lossy())
        ),
        "<string>/another-home</string>",
    );
    assert!(repair_check_unit(home, Job::Subscriber, &changed_home).is_err());

    let mut manager = FakeLaunchd {
        loaded: Some(Domain::Gui),
        no_gui: false,
        cached_subscriber: None,
        calls: Vec::new(),
    };
    for unit in [&next, &old] {
        repair_restart_launchd(home, Job::Subscriber, unit, &mut |call| {
            manager.answer(call)
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(plist_path(home, Job::Subscriber)).unwrap(),
            unit.definition
        );
    }
    assert_eq!(manager.loaded, Some(Domain::Gui));
    assert!(!subscriber_off_path(home).exists());
    assert!(!pin_path(home, Job::Subscriber).exists());
}

#[test]
fn signed_repair_observes_exact_unit_and_preserves_persons_records() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    let old = home.join("old-aterm");
    let new = home.join("new-aterm");
    std::fs::write(&old, b"old signed image").unwrap();
    std::fs::write(&new, b"new signed image").unwrap();
    let args = vec!["secrets".to_owned(), "watch".to_owned()];
    let definition = if cfg!(target_os = "macos") {
        plist_text_in(
            Job::Subscriber.label(),
            &old,
            &args,
            &log_path(home, Job::Subscriber),
            Domain::User,
            None,
        )
    } else {
        systemd_unit(Job::Subscriber.description(), &old, &args)
    };
    write_unit_file(&unit_file(home, Job::Subscriber), &definition).unwrap();
    crate::fsx::ensure_private_dir(&secrets_dir(home)).unwrap();
    crate::fsx::write_atomic(&pin_path(home, Job::Subscriber), b"explicit pin\n", 0o600).unwrap();
    crate::fsx::write_atomic(&subscriber_off_path(home), b"explicit off\n", 0o600).unwrap();
    let observed = repair_observe_at(home, Job::Subscriber).unwrap();
    assert!(observed.off);
    assert_eq!(observed.pin.as_deref(), Some("explicit pin\n"));
    let unit = observed.unit.unwrap();
    assert_eq!(unit.definition, definition);
    let next = repair_unit(home, Job::Subscriber, &unit, &new, &"a".repeat(64)).unwrap();
    assert_eq!(next.image, "a".repeat(64));
    assert_eq!(next.executable_sha256, repair_image_hash(&new).unwrap());
    assert_eq!(next.args, args);
    assert_eq!(
        repair_argv(&next.definition).unwrap()[0],
        new.to_str().unwrap()
    );
    repair_check_unit(home, Job::Subscriber, &next).unwrap();
    assert_eq!(
        std::fs::read_to_string(unit_file(home, Job::Subscriber)).unwrap(),
        definition
    );
    assert_eq!(
        repair_observe_at(home, Job::Subscriber).unwrap().pin,
        observed.pin
    );
}

#[test]
fn signed_repair_refuses_overridden_units_and_symlink_images() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    let image = home.join("aterm");
    std::fs::write(&image, b"image").unwrap();
    let args = vec!["secrets".to_owned(), "watch".to_owned()];
    let mut definition = if cfg!(target_os = "macos") {
        plist_text(
            Job::Subscriber.label(),
            &image,
            &args,
            &log_path(home, Job::Subscriber),
        )
    } else {
        systemd_unit(Job::Subscriber.description(), &image, &args)
    };
    definition.push_str("\nextra manager directive\n");
    write_unit_file(&unit_file(home, Job::Subscriber), &definition).unwrap();
    assert!(repair_observe_at(home, Job::Subscriber).is_err());
    let link = home.join("linked-image");
    std::os::unix::fs::symlink(&image, &link).unwrap();
    assert!(repair_image_hash(&link).is_err());
    assert!(repair_image_hash(home).is_err());
    let first = repair_image_hash(&image).unwrap();
    std::fs::write(&image, b"forged image").unwrap();
    assert_ne!(repair_image_hash(&image).unwrap(), first);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn signed_repair_proves_loaded_code_and_rejects_a_different_executable() {
    let pid = i32::try_from(std::process::id()).unwrap();
    let program = std::env::current_exe().unwrap();
    let hash = repair_image_hash(&program).unwrap();
    repair_prove_loaded(pid, &program, &hash).unwrap();
    #[cfg(target_os = "macos")]
    assert!(repair_prove_loaded(pid, Path::new("/usr/bin/true"), &hash).is_err());
    #[cfg(target_os = "linux")]
    assert!(repair_prove_loaded(pid, &program, &"0".repeat(64)).is_err());
    assert!(repair_prove_loaded(i32::MAX, &program, &hash).is_err());
}

#[test]
fn signed_repair_restarts_and_restores_the_exact_launchd_definition() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    let old_program = home.join("old");
    let new_program = home.join("new");
    let args = vec!["secrets".to_owned(), "watch".to_owned()];
    let unit = |program: &Path| crate::service_repair::Unit {
        program: program.to_str().unwrap().to_owned(),
        args: args.clone(),
        definition: plist_text_in(
            Job::Subscriber.label(),
            program,
            &args,
            &log_path(home, Job::Subscriber),
            Domain::User,
            None,
        ),
        image: "unused by manager seam".to_owned(),
        executable_sha256: "unused by manager seam".to_owned(),
    };
    let old = unit(&old_program);
    let new = unit(&new_program);
    let mut manager = FakeLaunchd {
        loaded: Some(Domain::User),
        no_gui: true,
        cached_subscriber: None,
        calls: Vec::new(),
    };
    repair_restart_launchd(home, Job::Subscriber, &new, &mut |call| {
        manager.answer(call)
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(plist_path(home, Job::Subscriber)).unwrap(),
        new.definition
    );
    repair_restart_launchd(home, Job::Subscriber, &old, &mut |call| {
        manager.answer(call)
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(plist_path(home, Job::Subscriber)).unwrap(),
        old.definition
    );
    assert_eq!(manager.loaded, Some(Domain::User));
    assert_eq!(
        manager
            .calls
            .iter()
            .filter(|call| call.first().is_some_and(|word| word == "bootout"))
            .count(),
        2
    );
    assert_eq!(
        manager
            .calls
            .iter()
            .filter(|call| call.first().is_some_and(|word| word == "bootstrap"))
            .count(),
        2
    );
    assert!(!subscriber_off_path(home).exists());
    assert!(!pin_path(home, Job::Subscriber).exists());
}

#[cfg(target_os = "macos")]
#[test]
fn signed_repair_retains_bundle_attribution_in_the_service_store() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    let old_bundle = home.join("Foo.app");
    let stored_bundle = home
        .join("service-store")
        .join("a".repeat(64))
        .join("image.app");
    let old_program = old_bundle.join("Contents/MacOS/aterm");
    let stored_program = stored_bundle.join("Contents/MacOS/aterm");
    let bundle_id = "com.aterm.aterm";
    for (bundle, executable) in [
        (&old_bundle, &old_program),
        (&stored_bundle, &stored_program),
    ] {
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(executable, b"inert signed image fixture").unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), format!(
            "<plist><dict><key>CFBundleIdentifier</key><string>{bundle_id}</string></dict></plist>"
        )).unwrap();
    }
    let args = vec!["secrets".to_owned(), "watch".to_owned()];
    let old = crate::service_repair::Unit {
        program: old_program.to_str().unwrap().to_owned(),
        args: args.clone(),
        definition: plist_text_in(
            Job::Subscriber.label(),
            &old_program,
            &args,
            &log_path(home, Job::Subscriber),
            Domain::User,
            Some(bundle_id),
        ),
        image: repair_image_hash(&old_program).unwrap(),
        executable_sha256: repair_image_hash(&old_program).unwrap(),
    };
    let new = repair_unit(
        home,
        Job::Subscriber,
        &old,
        &stored_program,
        &"a".repeat(64),
    )
    .unwrap();
    repair_check_unit(home, Job::Subscriber, &new).unwrap();
    assert_eq!(new.args, old.args);
    assert!(
        new.definition
            .contains("<key>AssociatedBundleIdentifiers</key>")
    );
    let mut manager = FakeLaunchd {
        loaded: Some(Domain::User),
        no_gui: true,
        cached_subscriber: None,
        calls: Vec::new(),
    };
    repair_restart_launchd(home, Job::Subscriber, &new, &mut |call| {
        manager.answer(call)
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(plist_path(home, Job::Subscriber)).unwrap(),
        new.definition
    );
    repair_restart_launchd(home, Job::Subscriber, &old, &mut |call| {
        manager.answer(call)
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(plist_path(home, Job::Subscriber)).unwrap(),
        old.definition
    );
    assert_eq!(manager.loaded, Some(Domain::User));
}

#[cfg(unix)]
#[test]
fn signed_repair_restarts_gui_and_user_domains_even_when_argv_names_aqua() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    let program = home.join("aterm");
    let gui_args = vec!["secrets".to_owned(), "watch".to_owned()];
    let gui_unit = crate::service_repair::Unit {
        program: program.to_str().unwrap().to_owned(),
        args: gui_args.clone(),
        definition: plist_text_in(
            Job::Subscriber.label(),
            &program,
            &gui_args,
            &log_path(home, Job::Subscriber),
            Domain::Gui,
            None,
        ),
        image: "unused by manager seam".to_owned(),
        executable_sha256: "unused by manager seam".to_owned(),
    };
    let mut gui_manager = FakeLaunchd {
        loaded: Some(Domain::Gui),
        no_gui: false,
        cached_subscriber: None,
        calls: Vec::new(),
    };
    repair_restart_launchd(home, Job::Subscriber, &gui_unit, &mut |call| {
        gui_manager.answer(call)
    })
    .unwrap();
    assert_eq!(gui_manager.loaded, Some(Domain::Gui));
    assert!(log_path(home, Job::Subscriber).parent().unwrap().is_dir());

    let user_args = vec![
        "secrets".to_owned(),
        "serve".to_owned(),
        "--advertise".to_owned(),
        "Aqua".to_owned(),
    ];
    let user_unit = crate::service_repair::Unit {
        program: program.to_str().unwrap().to_owned(),
        args: user_args.clone(),
        definition: plist_text_in(
            Job::Server.label(),
            &program,
            &user_args,
            &log_path(home, Job::Server),
            Domain::User,
            None,
        ),
        image: "unused by manager seam".to_owned(),
        executable_sha256: "unused by manager seam".to_owned(),
    };
    let mut user_manager = FakeLaunchd {
        loaded: Some(Domain::User),
        no_gui: true,
        cached_subscriber: None,
        calls: Vec::new(),
    };
    repair_restart_launchd(home, Job::Server, &user_unit, &mut |call| {
        user_manager.answer(call)
    })
    .unwrap();
    assert_eq!(user_manager.loaded, Some(Domain::User));
}

#[cfg(target_os = "macos")]
#[test]
fn signed_repair_updates_changed_bundle_attribution_and_rejects_empty_image() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    let dev_bundle = home.join("Dev.app");
    let release_bundle = home.join("Release.app");
    let dev_program = dev_bundle.join("Contents/MacOS/aterm");
    let release_program = release_bundle.join("Contents/MacOS/aterm");
    let bare_program = home.join("bare-aterm");
    for (bundle, executable, bundle_id) in [
        (&dev_bundle, &dev_program, "com.aterm.dev"),
        (&release_bundle, &release_program, "com.aterm.aterm"),
    ] {
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(executable, b"signed bundle fixture").unwrap();
        std::fs::write(
            bundle.join("Contents/Info.plist"),
            format!(
                "<plist><dict><key>CFBundleIdentifier</key><string>{bundle_id}</string></dict></plist>"
            ),
        )
        .unwrap();
    }
    std::fs::write(&bare_program, b"signed bare fixture").unwrap();
    let args = vec!["secrets".to_owned(), "watch".to_owned()];
    let old = crate::service_repair::Unit {
        program: dev_program.to_str().unwrap().to_owned(),
        args: args.clone(),
        definition: plist_text_in(
            Job::Subscriber.label(),
            &dev_program,
            &args,
            &log_path(home, Job::Subscriber),
            Domain::Gui,
            Some("com.aterm.dev"),
        ),
        image: repair_image_hash(&dev_program).unwrap(),
        executable_sha256: repair_image_hash(&dev_program).unwrap(),
    };
    assert!(repair_unit(home, Job::Subscriber, &old, &release_program, "").is_err());

    let to_release = repair_unit(
        home,
        Job::Subscriber,
        &old,
        &release_program,
        &"a".repeat(64),
    )
    .unwrap();
    assert!(
        to_release
            .definition
            .contains("<string>com.aterm.aterm</string>")
    );
    assert!(!to_release.definition.contains("com.aterm.dev"));
    repair_check_unit(home, Job::Subscriber, &to_release).unwrap();

    let to_bare = repair_unit(home, Job::Subscriber, &old, &bare_program, &"b".repeat(64)).unwrap();
    assert!(!to_bare.definition.contains("AssociatedBundleIdentifiers"));
    repair_check_unit(home, Job::Subscriber, &to_bare).unwrap();
}

#[test]
fn missing_subscriber_creation_is_guarded_against_off_pins_and_foreign_jobs() {
    use std::cell::Cell;
    for obstruction in [
        "none",
        "off",
        "pin",
        "foreign-file",
        "symlink",
        "loaded",
        "late-off",
        "late-pin",
        "late-policy",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path();
        let program = home.join("aterm");
        crate::fsx::ensure_private_dir(&secrets_dir(home)).unwrap();
        let unit = unit_file(home, Job::Subscriber);
        std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
        let obstruct = |kind: &str| match kind {
            "off" | "late-off" => std::fs::write(subscriber_off_path(home), b"off").unwrap(),
            "pin" | "late-pin" => std::fs::write(
                pin_path(home, Job::Subscriber),
                b"invalid pin still belongs to its writer",
            )
            .unwrap(),
            "foreign-file" => std::fs::write(&unit, b"foreign unit").unwrap(),
            "symlink" => std::os::unix::fs::symlink("absent-target", &unit).unwrap(),
            _ => {}
        };
        if !obstruction.starts_with("late-") {
            obstruct(obstruction);
        }
        let policy = Cell::new(true);
        let installs = Cell::new(0);
        let result = repair_absent_subscriber_with(
            home,
            &program,
            &mut || Ok(policy.get()),
            &mut || {
                obstruct(obstruction);
                if obstruction == "late-policy" {
                    policy.set(false);
                }
                Ok(if obstruction == "loaded" {
                    State::Installed {
                        pid: Some(123),
                        unit: unit.clone(),
                    }
                } else {
                    State::Absent
                })
            },
            &mut |_| {
                assert_job_decision_held(home, Job::Subscriber);
                installs.set(installs.get() + 1);
                Ok(Some("installed".into()))
            },
        )
        .unwrap();
        assert_eq!(result.is_some(), obstruction == "none", "{obstruction}");
        assert_eq!(
            installs.get(),
            usize::from(obstruction == "none"),
            "{obstruction}"
        );
        if obstruction == "foreign-file" {
            assert_eq!(std::fs::read(&unit).unwrap(), b"foreign unit");
        }
    }
}

#[test]
fn destination_release_is_actual_and_ignores_build_metadata() {
    assert_eq!(release_answer("aterm 0.147.0+g1234\n"), Some((0, 147, 0)));
    assert_eq!(release_answer("aterm 0.91.0\n"), Some((0, 91, 0)));
    for bad in [
        "atermx 0.147.0",
        "aterm 0.147",
        "aterm 0.147.0.1",
        "aterm nonsense",
    ] {
        assert_eq!(release_answer(bad), None);
    }
    let target = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    for version in [(0, 91, 0), (0, 146, 0)] {
        assert!(
            require_program_release(target, (0, 147, 0), &|path| {
                assert_eq!(path, target);
                Ok(version)
            })
            .is_err()
        );
    }
    for version in [(0, 147, 0), (0, 148, 0)] {
        assert!(require_program_release(target, (0, 147, 0), &|_| Ok(version)).is_ok());
    }
    assert!(
        require_program_release(target, (0, 147, 0), &|_| Err("unreadable".to_owned())).is_err()
    );
}

#[test]
fn automatic_server_restart_preserves_held_program_and_rechecks_off_and_pin() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path();
    let unit = unit_file(home, Job::Server);
    std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
    let kept = Path::new("/kept/aterm-0.148");
    let proposed = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let args: Vec<String> = ["secrets", "serve", "--advertise", "kept-name"]
        .map(String::from)
        .into();
    let unit_text = if cfg!(target_os = "macos") {
        plist_text(Job::Server.label(), kept, &args, Path::new("/tmp/log"))
    } else {
        systemd_unit(Job::Server.description(), kept, &args)
    };
    std::fs::write(&unit, &unit_text).unwrap();
    let absent = || Ok(State::Absent);
    let not_a_candidate =
        |_: &Path| -> crate::Result<()> { panic!("a retained program is not a new candidate") };
    let calls = std::cell::Cell::new(0);
    let mut install = |program: &Path, argv: &[String]| {
        calls.set(calls.get() + 1);
        assert_eq!(
            program, kept,
            "a stopped held program is restarted, not replaced"
        );
        assert_eq!(argv, args);
        Ok("restarted".to_owned())
    };
    assert!(
        ensure_server_with(home, proposed, &[], &absent, &not_a_candidate, &mut install)
            .unwrap()
            .is_some()
    );
    record_pin(home, Job::Server, kept).unwrap();
    assert!(
        ensure_server_with(home, proposed, &[], &absent, &not_a_candidate, &mut install)
            .unwrap()
            .is_some()
    );
    assert_eq!(calls.get(), 2);
    // A preceding eligibility read is now stale: a new Off or pin must win under the
    // repair lock, and automatic repair must not clear the person's decision.
    record_server_off(home).unwrap();
    assert!(
        ensure_server_with(home, proposed, &[], &absent, &not_a_candidate, &mut install)
            .unwrap()
            .is_none()
    );
    assert!(server_turned_off(home));
    clear_server_off(home).unwrap();
    record_pin(home, Job::Server, Path::new("/new/persons-pin")).unwrap();
    assert!(
        ensure_server_with(home, proposed, &[], &absent, &not_a_candidate, &mut install)
            .unwrap()
            .is_none()
    );
    assert_eq!(calls.get(), 2);
    assert_eq!(std::fs::read_to_string(&unit).unwrap(), unit_text);
    clear_pin(home, Job::Server).unwrap();
    assert!(
        ensure_server_with(
            home,
            proposed,
            &[],
            &absent,
            &not_a_candidate,
            &mut |program, _| {
                assert_eq!(program, kept);
                Err(Error::new("kept program cannot start"))
            }
        )
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&unit).unwrap(), unit_text);
    std::fs::remove_file(&unit).unwrap();
    assert!(
        ensure_server_with(
            home,
            proposed,
            &[],
            &absent,
            &|_| Err(Error::new("candidate is too old")),
            &mut |_, _| panic!("a rejected new candidate must not install")
        )
        .is_err()
    );
    assert!(
        ensure_server_with(
            home,
            proposed,
            &[],
            &absent,
            &|_| Ok(()),
            &mut |program, _| {
                assert_eq!(program, proposed);
                Ok("new job".to_owned())
            }
        )
        .unwrap()
        .is_some()
    );
    std::fs::write(&unit, vec![b' '; UNIT_FILE_MOST + 1]).unwrap();
    assert!(
        ensure_server_with(
            home,
            proposed,
            &[],
            &absent,
            &not_a_candidate,
            &mut |_, _| panic!("oversized unit file must not be read or installed")
        )
        .unwrap_err()
        .0
        .contains("cannot safely read the existing server job")
    );
    use std::os::unix::fs::PermissionsExt as _;
    let lock = decision_lock_path(home, Job::Server);
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o666)).unwrap();
    drop(JobDecision::take(home, Job::Server).unwrap());
    assert_eq!(
        std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let extra = home.join("extra-server.lock");
    std::fs::hard_link(&lock, &extra).unwrap();
    assert!(JobDecision::take(home, Job::Server).is_err());
}

/// A job is pointed only at a program that starts as aterm; one macOS kills at launch is
/// named: a loose copy of the app's executable (as m22's first snapshot swap met on
/// 2026-10-26) with the signing that lets it run, an app's own executable as a damaged
/// install, never to be signed again.
#[cfg(unix)]
#[test]
fn a_job_is_pointed_only_at_a_program_that_starts_as_aterm() {
    use std::os::unix::process::ExitStatusExt;
    let program = Path::new("/Users//x/.local/share/aterm-secrets/aterm");
    let out = |raw: i32, stdout: &str| {
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(raw),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    };
    let release = "aterm 0.142.0\nby Andrew Yates\n";
    assert_eq!(program_started(program, out(0, release)), Ok(()));
    assert_eq!(
        program_started(program, out(0, "aterm 0.142.0+g1a2b3c\n")),
        Ok(())
    );
    // Each refusal below answers as aterm but for the one fault it names, so only the check
    // of that fault refuses it.
    let killed = program_started(program, out(9, release)).expect_err("killed at launch");
    assert!(killed.0.contains("did not start as aterm"), "{killed}");
    #[cfg(target_os = "macos")]
    {
        assert!(
            killed.0.contains("macOS killed it at launch")
                && killed.0.contains(
                    "codesign --force --sign - /Users//x/.local/share/aterm-secrets/aterm"
                ),
            "{killed}"
        );
        let app = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        let damaged = program_started(app, out(9, release)).expect_err("a damaged app");
        assert!(
            damaged
                .0
                .contains("/Applications/aterm.app no longer matches its signature")
                && !damaged.0.contains("codesign"),
            "{damaged}"
        );
    }
    let exited = program_started(program, out(1 << 8, release)).expect_err("exit 1");
    assert!(
        exited
            .0
            .contains("did not start as aterm (--version: exit status: 1)")
            && !exited.0.contains("codesign"),
        "{exited}"
    );
    let other = program_started(program, out(0, "git version 2.50.1\n")).expect_err("git");
    assert!(other.0.contains("named no aterm release"), "{other}");
    let bare = program_started(program, out(0, "aterm \n")).expect_err("no release");
    assert!(bare.0.contains("named no aterm release"), "{bare}");
    // The live check runs `<program> --version`: a program that is not there is not run.
    let absent =
        check_program_starts(Path::new("/nonexistent/aterm-secrets/aterm")).expect_err("not there");
    assert!(
        absent
            .0
            .starts_with("/nonexistent/aterm-secrets/aterm did not start as aterm"),
        "{absent}"
    );
}

/// A program a job keeps (a held snapshot) is put back only while it starts, and one that
/// does not is refused, naming the job and the commands that move it, never swapped for
/// another aterm. Negative control: keeping it unchecked returns the program.
#[test]
fn a_kept_program_that_does_not_start_is_refused_not_kept_or_replaced() {
    let held = PathBuf::from("/Users//x/.local/share/aterm-secrets/aterm");
    let runs = |_: &Path| Ok(());
    let killed = |program: &Path| {
        Err(Error::new(format!(
            "{} did not start as aterm (macOS killed it at launch)",
            program.display()
        )))
    };
    assert_eq!(
        kept_starting(Some(held.clone()), Job::Subscriber, &runs),
        Ok(Some(held.clone()))
    );
    assert_eq!(kept_starting(None, Job::Subscriber, &killed), Ok(None));
    let refused = kept_starting(Some(held.clone()), Job::Subscriber, &killed)
        .expect_err("a held program that does not start");
    assert!(
        refused.0.contains("did not start as aterm")
            && refused
                .0
                .contains("the systems.alab.aterm-secrets job runs it")
            && refused
                .0
                .contains("aterm secrets agent on puts it on this aterm"),
        "{refused}"
    );
    let server = kept_starting(Some(held), Job::Server, &killed).expect_err("server");
    assert!(server.0.contains("aterm secrets server on"), "{server}");
}

#[test]
fn a_repair_keeps_the_lasting_program_its_job_names() {
    let held = Path::new("/Users//o/.local/share/aterm-secrets/aterm");
    let nothing = |_: &Path| false;
    let all = |_: &Path| true;
    // A held snapshot the job runs stays, whatever aterm repairs it.
    assert_eq!(
        kept(Some(held.into()), &nothing, &all).as_deref(),
        Some(held)
    );
    // CONTROL: gone or not executable, or no unit naming one, nothing is kept.
    assert_eq!(kept(Some(held.into()), &nothing, &nothing), None);
    assert_eq!(kept(None, &nothing, &all), None);
    // A build, or a copy that goes away, is no program to keep.
    for fleeting in [
        "/Users//o/aterm/target/release/aterm",
        "/Volumes/aterm/aterm.app/Contents/MacOS/aterm",
        "/tmp/aterm",
    ] {
        assert_eq!(
            kept(Some(fleeting.into()), &nothing, &all),
            None,
            "{fleeting}"
        );
    }
    // A file in a bundle that is not its executable names another program.
    let inner = Path::new("/Applications/b.app/Contents/Resources/aterm");
    let bundled = |path: &Path| path == Path::new("/Applications/b.app/Contents/MacOS/aterm");
    assert_eq!(kept(Some(inner.into()), &bundled, &all), None);
}

#[test]
fn a_unit_names_back_the_program_it_starts() {
    let program = Path::new("/Applications/A&B <\"q'>.app/Contents/MacOS/aterm");
    let args = ["secrets".to_string(), "watch".to_string()];
    let plist = plist_text("l", program, &args, Path::new("/var/log/x.log"));
    assert_eq!(plist_program(&plist).as_deref(), Some(program));
    let odd = Path::new("/home/o neil/50%$x\"q\\/aterm");
    let unit = systemd_unit("d", odd, &args);
    assert_eq!(exec_program(&unit).as_deref(), Some(odd));
    // A form neither writer makes names nothing.
    for unit in [
        "ExecStart=/bin/aterm watch",
        "ExecStart=\"/a\\x41\"",
        "ExecStart=\"/a%b\"",
        "",
    ] {
        assert_eq!(exec_program(unit), None, "{unit}");
    }
    for plist in [
        "<key>ProgramArguments</key><array><string>/a&nbsp;</string>",
        "<string>/a</string>",
    ] {
        assert_eq!(plist_program(plist), None, "{plist}");
    }
}

#[test]
fn a_unit_names_back_its_whole_argv() {
    let program = Path::new("/Applications/A&B <\"q'>.app/Contents/MacOS/aterm");
    let args = [
        "secrets".to_string(),
        "serve".to_string(),
        "--listen".to_string(),
        "0.0.0.0:7484".to_string(),
        "a&b <\"q'> 50%$".to_string(),
        String::new(),
    ];
    let whole: Vec<String> = std::iter::once(program.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .collect();
    let plist = plist_text("l", program, &args, Path::new("/var/log/x.log"));
    assert_eq!(plist_argv(&plist), Some(whole.clone()));
    assert_eq!(
        exec_argv(&systemd_unit("d", program, &args)),
        Some(whole),
        "systemd's words"
    );
    for unit in [
        "ExecStart=\"/a\" watch",
        "ExecStart=\"/a\"  \"b\"",
        "ExecStart=\"/a\"\"b\"",
        "ExecStart=\"/a",
        "",
    ] {
        assert_eq!(exec_argv(unit), None, "{unit}");
    }
    for plist in [
        "<key>ProgramArguments</key><array><string>/a</string>",
        "<key>ProgramArguments</key><array></array>",
        "<key>ProgramArguments</key><array><string>/a&nbsp;</string></array>",
    ] {
        assert_eq!(plist_argv(plist), None, "{plist}");
    }
}

/// A server job whose program is gone is installed again with the arguments it had; one
/// whose program is there, one switched off, one in another shape and none at all are left.
#[test]
fn a_server_job_whose_program_is_gone_is_installed_again_as_it_was() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path();
    let gone = Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
    let args: Vec<String> = ["secrets", "serve", "--listen", "0.0.0.0:7484"]
        .map(String::from)
        .into();
    let write = |text: String| {
        let unit = unit_file(home, Job::Server);
        std::fs::create_dir_all(unit.parent().expect("parent")).expect("dir");
        std::fs::write(&unit, text).expect("unit");
    };
    let unit_text = |program: &Path, args: &[String]| {
        if cfg!(target_os = "macos") {
            plist_text(
                Job::Server.label(),
                program,
                args,
                &log_path(home, Job::Server),
            )
        } else {
            systemd_unit(Job::Server.description(), program, args)
        }
    };
    let repair = |exists: bool| {
        let mut installed = None;
        let line = repair_server_with(
            home,
            &|path| exists && path == gone,
            &|| Ok(State::Absent),
            |args| {
                installed = Some(args.to_vec());
                Ok("installed".to_string())
            },
        )
        .expect("read");
        (line, installed)
    };
    assert_eq!(repair(false), (None, None), "no server job");
    write(unit_text(gone, &args));
    assert_eq!(
        repair(false),
        (Some("installed".to_string()), Some(args.clone())),
        "the program gone"
    );
    assert_eq!(repair(true), (None, None), "the program there");
    let watch = ["secrets".to_string(), "watch".to_string()];
    write(unit_text(gone, &watch));
    assert_eq!(repair(false), (None, None), "not a server's arguments");
    if cfg!(target_os = "macos") {
        let off = unit_text(gone, &args).replace(
            "<key>RunAtLoad</key>",
            "<key>Disabled</key>\n\t<true/>\n\t<key>RunAtLoad</key>",
        );
        write(off);
        assert_eq!(repair(false), (None, None), "switched off");
    }
    // A home not this login's own (this scratch one) is never repaired, nor its program
    // asked: launchd and systemd never read its jobs.
    write(unit_text(gone, &args));
    assert_eq!(
        repair_server(home, &|| panic!("a program asked for")).expect("no error"),
        None
    );
}

/// A held job is one `agent on` or `server on` wrote on another lasting aterm and nobody
/// switched off; it is pinned only where the person's pin record names its program, which
/// `on --program` writes and `on` or `off` removes.
/// NEGATIVE CONTROL: a [`held_in`] that ignores the pin reads the pinned job as unpinned.
#[test]
fn a_held_job_is_one_on_another_aterm_and_its_pin_is_the_persons_record() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path();
    let held = Path::new("/Users//x/.local/share/aterm-secrets/aterm");
    let installed = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let serve: Vec<String> = ["secrets", "serve", "--advertise", "alab-secrets"]
        .map(String::from)
        .into();
    let watch: Vec<String> = ["secrets", "watch"].map(String::from).into();
    let unit_text = |job: Job, program: &Path, args: &[String]| {
        if cfg!(target_os = "macos") {
            plist_text(job.label(), program, args, Path::new("/tmp/log"))
        } else {
            systemd_unit(job.description(), program, args)
        }
    };
    for (job, args) in [(Job::Server, &serve), (Job::Subscriber, &watch)] {
        let text = unit_text(job, held, args);
        let read = |text: &str, kept: Option<&Path>| {
            held_in(
                job,
                text,
                kept.map(Path::to_path_buf),
                Some(installed),
                pinned_to(home, job),
            )
        };
        let unpinned = Held {
            job,
            program: held.to_path_buf(),
            args: args.clone(),
            pin: None,
        };
        assert_eq!(read(&text, Some(held)), Some(unpinned.clone()), "{job:?}");
        assert!(!unpinned.pinned());
        record_pin(home, job, held).expect("pinned");
        let pinned = read(&text, Some(held)).expect("held");
        assert_eq!(
            pinned,
            Held {
                pin: Some(held.to_path_buf()),
                ..unpinned.clone()
            },
            "{job:?} pinned"
        );
        assert!(pinned.pinned(), "{job:?} pinned");
        record_pin(home, job, installed).expect("pinned elsewhere");
        let elsewhere = read(&text, Some(held)).expect("held");
        assert!(
            !elsewhere.pinned(),
            "{job:?}: a pin of another program pins nothing"
        );
        assert_eq!(elsewhere.pin.as_deref(), Some(installed));
        clear_pin(home, job).expect("unpinned");
        clear_pin(home, job).expect("none to remove");
        assert_eq!(read(&text, Some(held)), Some(unpinned));
        assert_eq!(
            read(&text, None),
            None,
            "{job:?}: a program gone or not lasting"
        );
        let on = unit_text(job, installed, args);
        assert_eq!(read(&on, Some(installed)), None, "{job:?} on this aterm");
        if cfg!(target_os = "macos") {
            let off = text.replace(
                "<key>RunAtLoad</key>",
                "<key>Disabled</key>\n\t<true/>\n\t<key>RunAtLoad</key>",
            );
            assert_eq!(read(&off, Some(held)), None, "{job:?} switched off");
        }
    }
    let odd = unit_text(Job::Subscriber, held, &serve);
    assert_eq!(
        held_in(
            Job::Subscriber,
            &odd,
            Some(held.into()),
            Some(installed),
            None
        ),
        None,
        "not a subscriber's arguments"
    );
}

/// A move whose install on this aterm fails (a bootstrap refused after the bootout) installs
/// the job again on the program it held, so it runs as before; the move is still an error.
/// NEGATIVE CONTROL: a [`moved_or_put_back`] that only reports the failure leaves the job
/// down, never installed on the held program again.
#[test]
fn a_move_that_does_not_start_puts_the_job_back_on_the_program_it_held() {
    let held = Held {
        job: Job::Server,
        program: PathBuf::from("/Users//x/.local/share/aterm-secrets/aterm"),
        args: ["secrets", "serve"].map(String::from).into(),
        pin: None,
    };
    let installed = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let mut tried = Vec::new();
    let mut install = |program: &Path| {
        tried.push(program.to_path_buf());
        if program == installed {
            Err(Error::new("Bootstrap failed: 5: Input/output error"))
        } else {
            Ok("installed".to_string())
        }
    };
    moved_or_put_back(&held, installed, &mut install).expect_err("the move failed");
    assert_eq!(tried, [installed.to_path_buf(), held.program.clone()]);

    let mut tried = 0;
    let mut down = |_: &Path| {
        tried += 1;
        Err(Error::new("Bootstrap failed"))
    };
    moved_or_put_back(&held, installed, &mut down).expect_err("both failed");
    assert_eq!(tried, 2, "one move, one put back");
    let mut fine = |program: &Path| Ok(program.display().to_string());
    assert_eq!(
        moved_or_put_back(&held, installed, &mut fine),
        Ok(installed.display().to_string())
    );
}

/// A move reads the job again under its decision lock and moves it only while it is still
/// the job the mend judged, so what a person did meanwhile stands: a `server off` (its unit
/// gone), an `agent on --program` (its pin record, written before its install, then the
/// install) and an `agent off` (its off record). A job left as it was moves, the lock held
/// across the install, so an `on` or `off` that comes then waits for it.
/// NEGATIVE CONTROL: a [`move_held_with`] that installs without reading the job again starts
/// the server turned off and moves the subscriber its person pinned since.
#[test]
fn a_move_never_undoes_an_off_or_a_pin_that_landed_after_the_mend_read_the_job() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path();
    // Lasting programs outside any temporary directory, as a held snapshot and the
    // installed aterm are.
    let snapshot = Path::new("/bin/sh");
    let installed = Path::new("/usr/bin/true");
    let named = Path::new("/bin/cat");
    let serve: Vec<String> = ["secrets", "serve"].map(String::from).into();
    let watch: Vec<String> = ["secrets", "watch"].map(String::from).into();
    let write_unit = |job: Job, program: &Path, args: &[String]| {
        let path = unit_file(home, job);
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("unit directory");
        let text = if cfg!(target_os = "macos") {
            plist_text(job.label(), program, args, Path::new("/tmp/log"))
        } else {
            systemd_unit(job.description(), program, args)
        };
        std::fs::write(path, text).expect("unit written");
    };
    let tried = std::cell::RefCell::new(Vec::new());
    let mut install = |program: &Path| -> crate::Result<String> {
        tried.borrow_mut().push(program.to_path_buf());
        Ok("installed".to_string())
    };
    let no_daemon = || Ok(State::Absent);

    write_unit(Job::Server, snapshot, &serve);
    let server = held_at(home, Job::Server, Some(installed)).expect("CONTROL: held");
    std::fs::remove_file(unit_file(home, Job::Server)).expect("`server off` removes it");
    assert_eq!(
        move_held_with(home, &server, installed, &no_daemon, &mut install),
        Ok(None),
        "the server turned off stays off"
    );

    write_unit(Job::Subscriber, snapshot, &watch);
    let subscriber = held_at(home, Job::Subscriber, Some(installed)).expect("CONTROL: held");
    record_pin(home, Job::Subscriber, named).expect("`agent on --program` pins first");
    assert_eq!(
        move_held_with(home, &subscriber, installed, &no_daemon, &mut install),
        Ok(None),
        "the pin stands while its install waits"
    );
    write_unit(Job::Subscriber, named, &watch);
    assert_eq!(
        move_held_with(home, &subscriber, installed, &no_daemon, &mut install),
        Ok(None),
        "its install stands"
    );
    assert_eq!(pinned_to(home, Job::Subscriber).as_deref(), Some(named));

    clear_pin(home, Job::Subscriber).expect("unpinned");
    write_unit(Job::Subscriber, snapshot, &watch);
    let subscriber = held_at(home, Job::Subscriber, Some(installed)).expect("CONTROL: held");
    record_subscriber_off(home).expect("`agent off` records it");
    assert_eq!(
        move_held_with(home, &subscriber, installed, &no_daemon, &mut install),
        Ok(None),
        "the subscriber turned off stays off"
    );
    clear_subscriber_off(home).expect("on again");
    assert!(
        tried.borrow().is_empty(),
        "nothing moved: {:?}",
        tried.borrow()
    );

    write_unit(Job::Server, snapshot, &serve);
    for job in [Job::Server, Job::Subscriber] {
        let held = held_at(home, job, Some(installed)).expect("CONTROL: held");
        let mut moving = |program: &Path| {
            assert_job_decision_held(home, job);
            install(program)
        };
        assert_eq!(
            move_held_with(home, &held, installed, &no_daemon, &mut moving),
            Ok(Some("installed".to_string())),
            "{job:?} left as it was moves"
        );
    }
    assert_eq!(
        *tried.borrow(),
        [installed.to_path_buf(), installed.to_path_buf()]
    );
}

/// While the system LaunchDaemon `aterm machines apply` installs is there, installed or
/// running, the mend never starts a user server beside it on its port: a held server job is
/// not moved and a server job whose program is gone is not repaired. The subscriber is no
/// server and moves as before; a daemon state that cannot be read moves nothing either.
/// NEGATIVE CONTROL: a [`move_held_with`] or [`repair_server_with`] that does not read the
/// daemon's state installs the user server beside the running daemon.
#[test]
fn no_user_server_is_moved_or_repaired_beside_the_system_launch_daemon() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path();
    // Lasting programs outside any temporary directory, as a held snapshot and the
    // installed aterm are.
    let snapshot = Path::new("/bin/sh");
    let installed = Path::new("/usr/bin/true");
    let gone = Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
    let serve: Vec<String> = ["secrets", "serve"].map(String::from).into();
    let watch: Vec<String> = ["secrets", "watch"].map(String::from).into();
    let write_unit = |job: Job, program: &Path, args: &[String]| {
        let path = unit_file(home, job);
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("unit directory");
        let text = if cfg!(target_os = "macos") {
            plist_text(job.label(), program, args, Path::new("/tmp/log"))
        } else {
            systemd_unit(job.description(), program, args)
        };
        std::fs::write(path, text).expect("unit written");
    };
    let daemon_plist = PathBuf::from(DAEMON_PLIST);
    let running = State::Installed {
        pid: Some(4242),
        unit: daemon_plist.clone(),
    };
    let stopped = State::Installed {
        pid: None,
        unit: daemon_plist,
    };
    let tried = std::cell::RefCell::new(Vec::new());
    let mut install = |program: &Path| -> crate::Result<String> {
        tried.borrow_mut().push(program.to_path_buf());
        Ok("installed".to_string())
    };

    write_unit(Job::Server, snapshot, &serve);
    let server = held_at(home, Job::Server, Some(installed)).expect("CONTROL: held");
    for daemon in [&running, &stopped] {
        assert_eq!(
            move_held_with(
                home,
                &server,
                installed,
                &|| Ok(daemon.clone()),
                &mut install
            ),
            Ok(None),
            "the held server stays beside {daemon:?}"
        );
    }
    move_held_with(
        home,
        &server,
        installed,
        &|| Err(Error::new("launchctl could not run")),
        &mut install,
    )
    .expect_err("the daemon's state unread");
    assert!(
        tried.borrow().is_empty(),
        "nothing moved: {:?}",
        tried.borrow()
    );
    write_unit(Job::Subscriber, snapshot, &watch);
    let subscriber = held_at(home, Job::Subscriber, Some(installed)).expect("CONTROL: held");
    assert_eq!(
        move_held_with(
            home,
            &subscriber,
            installed,
            &|| Ok(running.clone()),
            &mut install
        ),
        Ok(Some("installed".to_string())),
        "the subscriber is no server"
    );
    assert_eq!(
        move_held_with(
            home,
            &server,
            installed,
            &|| Ok(State::Absent),
            &mut install
        ),
        Ok(Some("installed".to_string())),
        "CONTROL: with no daemon the held server moves"
    );
    assert_eq!(
        *tried.borrow(),
        [installed.to_path_buf(), installed.to_path_buf()]
    );

    write_unit(Job::Server, gone, &serve);
    let repair = |daemon: &State| {
        repair_server_with(home, &|path| path != gone, &|| Ok(daemon.clone()), |args| {
            Ok(format!("installed {}", args.join(" ")))
        })
    };
    assert_eq!(
        repair(&running),
        Ok(None),
        "not repaired beside it, running"
    );
    assert_eq!(repair(&stopped), Ok(None), "nor installed, not running");
    assert_eq!(
        repair(&State::Absent),
        Ok(Some("installed secrets serve".to_string())),
        "CONTROL: with no daemon the server whose program is gone is repaired"
    );
}

/// The mend takes the server's decision lock only once a repair is due (a `secrets serve`
/// job whose program is gone) and holds it across the install, so a Mac with no server job,
/// or one whose program is there, gets no `~/.config/aterm/secrets` nor `server.lock` from
/// the mend that runs at every launch and every half hour.
/// NEGATIVE CONTROL: a [`repair_server_with`] that takes the lock before it reads the unit
/// file leaves `server.lock` in a home with no server job.
#[test]
fn a_mend_leaves_no_server_lock_on_a_mac_with_no_server_job() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path();
    let gone = Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
    let serve: Vec<String> = ["secrets", "serve"].map(String::from).into();
    let lock = decision_lock_path(home, Job::Server);
    let untouched = || {
        repair_server_with(
            home,
            &|path| path != gone,
            &|| panic!("the daemon read with no repair due"),
            |_| panic!("installed with no repair due"),
        )
    };
    assert_eq!(untouched(), Ok(None), "no server job");
    assert!(
        !lock.exists() && !secrets_dir(home).exists(),
        "{} made with no server job",
        lock.display()
    );
    let unit = unit_file(home, Job::Server);
    std::fs::create_dir_all(unit.parent().expect("a directory")).expect("unit directory");
    let text = |program: &Path| {
        if cfg!(target_os = "macos") {
            plist_text(Job::Server.label(), program, &serve, Path::new("/tmp/log"))
        } else {
            systemd_unit(Job::Server.description(), program, &serve)
        }
    };
    std::fs::write(&unit, text(Path::new("/bin/sh"))).expect("unit written");
    assert_eq!(untouched(), Ok(None), "its program there");
    assert!(!lock.exists(), "{} made with no repair due", lock.display());

    std::fs::write(&unit, text(gone)).expect("unit written");
    assert_eq!(
        repair_server_with(home, &|path| path != gone, &|| Ok(State::Absent), |_| {
            assert_job_decision_held(home, Job::Server);
            Ok("installed".to_string())
        },),
        Ok(Some("installed".to_string())),
        "CONTROL: a repair due runs under the lock"
    );
}

/// `on --program` records the person's pin before its install, so a mend that reads the job
/// while the install waits finds the pin; an install that fails puts the record back as it
/// was; `on` with no `--program` drops the record only once its install ran.
/// NEGATIVE CONTROL: an [`install_pinned`] that records the pin after the install leaves the
/// job unpinned while it installs.
#[test]
fn on_program_records_its_pin_before_the_install_and_a_failed_install_puts_it_back() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path();
    let first = Path::new("/Users//x/.local/share/aterm-secrets/aterm");
    let second = Path::new("/Users//x/held/aterm");
    let job = Job::Server;
    record_pin(home, job, first).expect("pinned");
    let (line, unrecorded) = install_pinned(home, job, Some(second), || {
        assert_eq!(
            pinned_to(home, job).as_deref(),
            Some(second),
            "the pin is there before the install"
        );
        Ok("installed".to_string())
    })
    .expect("installed");
    assert_eq!((line.as_str(), unrecorded), ("installed", None));
    install_pinned(home, job, Some(first), || {
        Err(Error::new("Bootstrap failed: 5: Input/output error"))
    })
    .expect_err("the install failed");
    assert_eq!(
        pinned_to(home, job).as_deref(),
        Some(second),
        "put back as it was"
    );
    clear_pin(home, job).expect("unpinned");
    install_pinned(home, job, Some(first), || {
        Err(Error::new("Bootstrap failed"))
    })
    .expect_err("the install failed");
    assert_eq!(pinned_to(home, job), None, "none, as before");
    record_pin(home, job, first).expect("pinned");
    install_pinned(home, job, None, || {
        assert_eq!(
            pinned_to(home, job).as_deref(),
            Some(first),
            "dropped only once the install ran"
        );
        Ok("installed".to_string())
    })
    .expect("installed");
    assert_eq!(pinned_to(home, job), None, "`on` unpins");
}

fn assert_subscriber_decision_held(home: &Path) {
    assert_job_decision_held(home, Job::Subscriber);
}

fn assert_job_decision_held(home: &Path, job: Job) {
    let independent = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(decision_lock_path(home, job))
        .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "an independent scratch handle proves the lifecycle callback still holds its decision lock"
    )]
    let taken = independent.try_lock();
    assert!(matches!(taken, Err(std::fs::TryLockError::WouldBlock)));
}

fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

#[test]
fn a_job_is_set_up_only_for_the_logins_own_home() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = root.path().join("own");
    let scratch = root.path().join("scratch");
    std::fs::create_dir_all(&own).expect("dir");
    std::fs::create_dir_all(&scratch).expect("dir");
    own_home(&own, Some(&own)).expect("its own");
    own_home(&own.join("."), Some(&own)).expect("spelt another way");
    #[cfg(unix)]
    {
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&own, &link).expect("link");
        own_home(&link, Some(&own)).expect("through a link");
    }
    let error = own_home(&scratch, Some(&own)).expect_err("another home");
    assert!(error.0.contains("not this login's home"), "{error}");
    for home in [&own, &scratch] {
        assert!(
            own_home(home, None).is_err(),
            "an unresolved account cannot authorize its per-login job"
        );
    }
}

/// A scratch HOME is never this login's, so its state is read without asking launchd.
#[cfg(unix)]
#[test]
fn a_home_that_is_not_the_logins_has_no_job() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    for job in [Job::Subscriber, Job::Server] {
        assert!(matches!(
            state(scratch.path(), job).expect("state"),
            State::Absent
        ));
    }
}

/// The record that the subscriber is off is private, goes once it is on again, and is no
/// error to remove when absent.
#[cfg(unix)]
#[test]
fn the_record_that_the_subscriber_was_turned_off_is_kept_until_it_is_on_again() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let home = scratch.path();
    assert!(!subscriber_turned_off(home));
    record_subscriber_off(home).expect("recorded");
    assert!(subscriber_turned_off(home));
    let path = subscriber_off_path(home);
    assert_eq!(path, secrets_dir(home).join("subscriber-off"));
    let mode = |path: &Path| {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&secrets_dir(home)), 0o700);
    record_subscriber_off(home).expect("again");
    clear_subscriber_off(home).expect("cleared");
    assert!(!subscriber_turned_off(home));
    clear_subscriber_off(home).expect("nothing to clear");
}

#[test]
fn subscriber_repair_preserves_off_but_fresh_on_can_replace_it() {
    use std::cell::Cell;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path();
    let installs = Cell::new(0);
    let install = || {
        assert_subscriber_decision_held(home);
        installs.set(installs.get() + 1);
        Ok("scratch subscriber installed".to_owned())
    };
    assert!(repair_subscriber_with(home, install).unwrap().is_some());
    assert_eq!(
        installs.get(),
        1,
        "an unchanged enabled repair still installs"
    );
    turn_off_subscriber_with(home, || {
        assert_subscriber_decision_held(home);
        assert!(subscriber_turned_off(home), "off is saved before removal");
        Ok(true)
    })
    .unwrap();
    assert!(repair_subscriber_with(home, install).unwrap().is_none());
    assert_eq!(installs.get(), 1, "the older repair calls no installer");
    assert!(subscriber_turned_off(home));
    install_subscriber_with(home, || {
        assert_subscriber_decision_held(home);
        Err(Error::new("injected install failure"))
    })
    .unwrap_err();
    assert!(
        subscriber_turned_off(home),
        "a failed On preserves the choice"
    );
    install_subscriber_with(home, install).unwrap();
    assert_eq!(installs.get(), 2, "explicit On or Join can install");
    assert!(!subscriber_turned_off(home));
}

#[test]
fn subscriber_off_after_a_repair_orders_removal_after_its_install() {
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().to_owned();
    let running = Arc::new(AtomicBool::new(false));
    let (entered, installing) = mpsc::sync_channel(1);
    let (release, finish_install) = mpsc::sync_channel(1);
    let repair_home = home.clone();
    let repair_running = Arc::clone(&running);
    let repair = std::thread::spawn(move || {
        repair_subscriber_with(&repair_home, || {
            assert_subscriber_decision_held(&repair_home);
            repair_running.store(true, Ordering::SeqCst);
            entered.send(()).unwrap();
            finish_install
                .recv_timeout(Duration::from_secs(60))
                .unwrap();
            Ok("scratch subscriber installed".into())
        })
    });
    installing.recv_timeout(Duration::from_secs(60)).unwrap();
    let off_home = home.clone();
    let off_running = Arc::clone(&running);
    let (requested, requesting) = mpsc::sync_channel(1);
    let off = std::thread::spawn(move || {
        requested.send(()).unwrap();
        turn_off_subscriber_with(&off_home, || {
            assert_subscriber_decision_held(&off_home);
            assert!(subscriber_turned_off(&off_home));
            off_running.store(false, Ordering::SeqCst);
            Ok(true)
        })
    });
    requesting.recv_timeout(Duration::from_secs(60)).unwrap();
    release.send(()).unwrap();
    assert!(repair.join().unwrap().unwrap().is_some());
    assert!(off.join().unwrap().unwrap());
    assert!(subscriber_turned_off(&home));
    assert!(!running.load(Ordering::SeqCst));
    let stale_install = Cell::new(false);
    assert!(
        repair_subscriber_with(&home, || {
            stale_install.set(true);
            Ok("must not install".into())
        })
        .unwrap()
        .is_none()
    );
    assert!(!stale_install.get());
}

#[test]
fn subscriber_off_write_failure_never_removes_the_job() {
    use std::cell::Cell;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path();
    let decision = JobDecision::take(home, Job::Subscriber).unwrap();
    std::fs::create_dir(subscriber_off_path(home)).unwrap();
    drop(decision);
    let removed = Cell::new(false);
    assert!(
        turn_off_subscriber_with(home, || {
            removed.set(true);
            Ok(true)
        })
        .is_err()
    );
    assert!(!removed.get());
}

#[test]
fn labels_and_paths_sit_where_launchd_systemd_and_the_design_say() {
    let home = Path::new("/Users//x");
    assert_eq!(Job::Subscriber.label(), "systems.alab.aterm-secrets");
    assert_eq!(Job::Server.label(), "systems.alab.aterm-secrets-server");
    assert_eq!(
        plist_path(home, Job::Subscriber),
        Path::new("/Users//x/Library/LaunchAgents/systems.alab.aterm-secrets.plist")
    );
    assert_eq!(
        unit_path(home, Job::Server),
        Path::new("/Users//x/.config/systemd/user/systems.alab.aterm-secrets-server.service")
    );
    assert_eq!(
        log_path(home, Job::Subscriber),
        Path::new("/Users//x/.config/aterm/secrets/systems.alab.aterm-secrets.log")
    );
}

#[test]
fn the_plist_names_the_job_keeps_it_alive_and_escapes_every_string() {
    let log = log_path(Path::new("/Users//a&b"), Job::Subscriber);
    let text = plist_text(
        Job::Subscriber.label(),
        Path::new("/Applications/A&B <\"q'>.app/Contents/MacOS/aterm"),
        &words(&["secrets", "watch", "<&>"]),
        &log,
    );
    assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
    assert!(text.ends_with("</dict>\n</plist>\n"));
    assert!(text.contains("\t<key>Label</key>\n\t<string>systems.alab.aterm-secrets</string>\n"));
    assert!(text.contains(
        "\t<key>ProgramArguments</key>\n\t<array>\n\
         \t\t<string>/Applications/A&amp;B &lt;&quot;q&apos;&gt;.app/Contents/MacOS/aterm</string>\n\
         \t\t<string>secrets</string>\n\
         \t\t<string>watch</string>\n\
         \t\t<string>&lt;&amp;&gt;</string>\n\
         \t</array>\n"
    ));
    assert!(!text.contains("A&B"), "an unescaped ampersand");
    assert!(text.contains("\t<key>RunAtLoad</key>\n\t<true/>\n"));
    assert!(text.contains("\t<key>KeepAlive</key>\n\t<true/>\n"));
    assert!(text.contains("\t<key>ProcessType</key>\n\t<string>Interactive</string>\n"));
    assert!(!text.contains("<key>Nice</key>"));
    assert!(text.contains(
        "\t<key>SoftResourceLimits</key>\n\t<dict>\n\t\t<key>NumberOfFiles</key>\n\t\t<integer>4096</integer>\n\t</dict>\n"
    ));
    assert!(text.contains(
        "\t<key>EnvironmentVariables</key>\n\t<dict>\n\
         \t\t<key>HOME</key>\n\t\t<string>/Users//a&amp;b</string>\n\
         \t\t<key>PATH</key>\n\t\t<string>/Users//a&amp;b/Library/Application Support/aterm/pkg/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>\n\
         \t</dict>\n"
    ));
    let log_line =
        "<string>/Users//a&amp;b/.config/aterm/secrets/systems.alab.aterm-secrets.log</string>";
    assert!(text.contains(&format!("\t<key>StandardOutPath</key>\n\t{log_line}\n")));
    assert!(text.contains(&format!("\t<key>StandardErrorPath</key>\n\t{log_line}\n")));
    assert_eq!(text.matches("<key>").count(), 12, "{text}");
    let server = plist_text(
        Job::Server.label(),
        Path::new("/fixture/aterm"),
        &words(&["secrets", "serve"]),
        &log_path(Path::new("/Users//fixture"), Job::Server),
    );
    assert!(server.contains("\t<key>ProcessType</key>\n\t<string>Background</string>\n"));
    assert!(server.contains("\t<key>Nice</key>\n\t<integer>5</integer>\n"));
}

/// A login job that runs until it says it is done starts at each login and again after any
/// end but its own exit 0, in the person's login (the gui domain, which needs no session
/// key), under its own HOME and attributed to its app.
#[test]
fn a_job_until_done_starts_at_login_and_again_after_any_end_but_its_own() {
    let text = plist_text_until_done(
        "systems.alab.aterm-setup",
        Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
        &words(&["__setup-resume"]),
        Path::new("/Users//a/Library/Application Support/aterm/doctor/setup-resume.log"),
        Path::new("/Users//a"),
        Some("systems.alab.aterm"),
    );
    assert!(text.contains("\t<key>RunAtLoad</key>\n\t<true/>\n"));
    assert!(text.contains(
        "\t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n"
    ));
    assert!(!text.contains("\t<key>KeepAlive</key>\n\t<true/>\n"));
    assert!(
        text.contains("\t\t<key>HOME</key>\n\t\t<string>/Users//a</string>\n\t\t<key>PATH</key>\n")
    );
    assert!(
        !text.contains("LimitLoadToSessionType"),
        "the gui domain is its session"
    );
    assert!(text.contains(
        "\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t<string>systems.alab.aterm</string>\n"
    ));
    assert!(text.contains("\t\t<string>__setup-resume</string>\n"));
    assert!(
        text.contains("\t<key>ProcessType</key>\n\t<string>Standard</string>\n"),
        "its clones and installs run at a window's pace"
    );
    assert!(!text.contains("<key>Nice</key>"), "{text}");
}

/// A job in the user domain names the session launchd loads it in, and one attributed to an
/// app names the app; the plist [`plist_text`] writes names neither.
#[test]
fn a_background_job_names_its_session_and_an_apps_job_its_app() {
    let log = Path::new("/l.log");
    let program = Path::new("/t/aterm");
    let text = plist_text_in(
        "l",
        program,
        &[],
        log,
        Domain::User,
        Some("com.aterm.aterm"),
    );
    assert!(text.contains("\t<key>LimitLoadToSessionType</key>\n\t<string>Background</string>\n"));
    assert!(text.contains(
        "\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t<string>com.aterm.aterm</string>\n\t</array>\n"
    ));
    let plain = plist_text("l", program, &[], log);
    assert_eq!(
        plain,
        plist_text_in("l", program, &[], log, Domain::Gui, None)
    );
    assert!(!plain.contains("LimitLoadToSessionType"), "{plain}");
    assert!(!plain.contains("AssociatedBundleIdentifiers"), "{plain}");
}

/// The app a program is the main executable of is read from its bundle's `Info.plist`;
/// a program outside a bundle, or a bundle whose identifier is not a plain one, names none.
#[test]
fn a_program_in_an_app_names_the_apps_identifier() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let app = root.path().join("aterm.app/Contents");
    std::fs::create_dir_all(app.join("MacOS")).expect("bundle");
    let info = |id: &str| {
        std::fs::write(
            app.join("Info.plist"),
            format!(
                "<plist><dict>\n\t<key>CFBundleName</key>\n\t<string>aterm</string>\n\
                 \t<key>CFBundleIdentifier</key>\n\t<string>{id}</string>\n</dict></plist>\n"
            ),
        )
        .expect("Info.plist");
    };
    info("com.aterm.aterm");
    let program = app.join("MacOS/aterm");
    assert_eq!(bundle_id_of(&program).as_deref(), Some("com.aterm.aterm"));
    info("a&amp;b");
    assert_eq!(bundle_id_of(&program), None);
    info("com.aterm.aterm");
    assert_eq!(bundle_id_of(&root.path().join("aterm")), None);
    assert_eq!(bundle_id_of(&app.join("Resources/aterm")), None);
}

#[test]
fn a_log_elsewhere_writes_no_environment() {
    let text = plist_text("l", Path::new("/t/aterm"), &[], Path::new("/var/log/x.log"));
    assert!(!text.contains("EnvironmentVariables"), "{text}");
    assert!(text.contains("\t<array>\n\t\t<string>/t/aterm</string>\n\t</array>\n"));
    assert_eq!(
        home_of_log(Path::new("/h/.config/aterm/secrets/l.log")),
        Some(Path::new("/h"))
    );
    assert_eq!(home_of_log(Path::new(".config/aterm/secrets/l.log")), None);
    assert_eq!(home_of_log(Path::new("/h/.config/aterm/other/l.log")), None);
}

/// `plutil -lint` reads the plist as launchd will.
#[cfg(target_os = "macos")]
#[test]
fn the_plist_is_well_formed_by_plutil() {
    let plutil = Path::new("/usr/bin/plutil");
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let lint = |name: &str, text: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, text).expect("write");
        let mut cmd = aterm_uds::spawnfd::command(plutil);
        cmd.arg("-lint").arg(&path);
        let out =
            aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(60))
                .expect("plutil");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    let full = plist_text(
        Job::Server.label(),
        Path::new("/A&B <\"q'>/x.app/Contents/MacOS/aterm"),
        &words(&["secrets", "serve", "--listen", "0.0.0.0:7484", "a & b"]),
        &log_path(Path::new("/Users//o'neil & co"), Job::Server),
    );
    let (ok, said) = lint("full.plist", &full);
    assert!(ok, "{said}\n{full}");
    let background = plist_text_in(
        Job::Server.label(),
        Path::new("/x.app/Contents/MacOS/aterm"),
        &words(&["secrets", "serve"]),
        &log_path(Path::new("/Users//o"), Job::Server),
        Domain::User,
        Some("com.aterm.aterm"),
    );
    let (ok, said) = lint("background.plist", &background);
    assert!(ok, "{said}\n{background}");
    let bare = plist_text("l", Path::new("/t/aterm"), &[], Path::new("/l.log"));
    let (ok, said) = lint("bare.plist", &bare);
    assert!(ok, "{said}\n{bare}");
    let broken = full.replace("A&amp;B", "A&B");
    let (ok, _) = lint("broken.plist", &broken);
    assert!(!ok, "the control lints");
}

/// A job set up over SSH or on a box with no display asks systemd to let the user linger,
/// so it outlives the session; a desktop session's, or a user who lingers already, does not.
#[test]
fn a_remote_or_headless_session_asks_to_linger() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        }
    };
    let ssh = env(&[
        ("SSH_CONNECTION", "10.0.0.2 50000 10.0.0.3 22"),
        ("DISPLAY", ":0"),
    ]);
    assert!(needs_linger(&ssh, false));
    assert!(needs_linger(&env(&[]), false), "no display: headless");
    assert!(needs_linger(&env(&[("SSH_TTY", "/dev/pts/1")]), false));
    // Controls: a desktop session, and a user who lingers already.
    assert!(!needs_linger(
        &env(&[("WAYLAND_DISPLAY", "wayland-0")]),
        false
    ));
    assert!(!needs_linger(
        &env(&[("DISPLAY", ":0"), ("SSH_TTY", "")]),
        false
    ));
    assert!(!needs_linger(&ssh, true));

    let enabled = linger_line("ay", Ok(()));
    assert!(
        enabled.starts_with("loginctl enable-linger ay:"),
        "{enabled}"
    );
    let refused = linger_line("ay", Err(Error::new("Access denied")));
    assert!(
        refused.contains("Access denied") && refused.contains("sudo loginctl enable-linger ay")
    );
    assert!(
        !refused.contains('\n') && !enabled.contains('\n'),
        "one line each"
    );
}

#[test]
fn a_systemd_word_reads_back_as_itself() {
    assert_eq!(systemd_word("/usr/bin/aterm"), "\"/usr/bin/aterm\"");
    assert_eq!(systemd_word("a b"), "\"a b\"");
    assert_eq!(systemd_word("say \"hi\""), "\"say \\\"hi\\\"\"");
    assert_eq!(systemd_word("back\\slash"), "\"back\\\\slash\"");
    assert_eq!(systemd_word("100%"), "\"100%%\"");
    assert_eq!(systemd_word("%h/x"), "\"%%h/x\"");
    assert_eq!(systemd_word("$HOME ${X}"), "\"$$HOME $${X}\"");
    assert_eq!(systemd_word("it's"), "\"it's\"");
    assert_eq!(systemd_word("a\nb\tc\rd"), "\"a\\nb\\tc\\rd\"");
    assert_eq!(systemd_word("\u{1}\u{7f}\u{85}"), "\"\\x01\\x7f\\u0085\"");
    assert_eq!(systemd_word(""), "\"\"");
    assert_eq!(systemd_word("π"), "\"π\"");
}

#[test]
fn the_unit_runs_the_quoted_argv_and_always_restarts() {
    let unit = systemd_unit(
        Job::Subscriber.description(),
        Path::new("/home/o neil/.local/bin/aterm"),
        &words(&["secrets", "watch", "50%", "$x"]),
    );
    assert_eq!(
        unit,
        "[Unit]\n\
         Description=aterm secrets subscriber (aterm secrets watch)\n\
         \n\
         [Service]\n\
         ExecStart=\"/home/o neil/.local/bin/aterm\" \"secrets\" \"watch\" \"50%%\" \"$$x\"\n\
         Restart=always\n\
         RestartSec=5\n\
         LimitNOFILE=4096\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    );
    let odd = systemd_unit("100% sure\nof it\\", Path::new("/a"), &[]);
    assert!(odd.contains("Description=100%% sure of it\n"), "{odd}");
}

#[test]
fn the_launchctl_and_systemctl_argv_are_plain() {
    let (gui, user) = (Domain::Gui, Domain::User);
    assert_eq!(
        launchctl_argv(501, Launchctl::Print(gui, "systems.alab.aterm-secrets")),
        ["print", "gui/501/systems.alab.aterm-secrets"]
    );
    assert_eq!(
        launchctl_argv(501, Launchctl::Print(user, "l.x")),
        ["print", "user/501/l.x"]
    );
    assert_eq!(
        launchctl_argv(501, Launchctl::PrintDomain(gui)),
        ["print", "gui/501"]
    );
    assert_eq!(
        launchctl_argv(501, Launchctl::Bootout(gui, "l.x")),
        ["bootout", "gui/501/l.x"]
    );
    assert_eq!(
        launchctl_argv(
            501,
            Launchctl::Bootstrap(gui, Path::new("/h/Library/LaunchAgents/l.x.plist"))
        ),
        ["bootstrap", "gui/501", "/h/Library/LaunchAgents/l.x.plist"]
    );
    assert_eq!(
        launchctl_argv(501, Launchctl::Bootstrap(user, Path::new("/p.plist"))),
        ["bootstrap", "user/501", "/p.plist"]
    );
    assert_eq!(
        launchctl_argv(502, Launchctl::Kickstart(gui, "l.x")),
        ["kickstart", "gui/502/l.x"]
    );
    assert_eq!(
        launchctl_argv(502, Launchctl::Restart(user, "l.x")),
        ["kickstart", "-k", "user/502/l.x"]
    );
    assert_eq!(
        systemctl_argv(&["daemon-reload"]),
        ["--user", "daemon-reload"]
    );
    assert_eq!(
        systemctl_argv(&["enable", "--now", "l.service"]),
        ["--user", "enable", "--now", "l.service"]
    );
    assert_eq!(
        systemctl_argv(&["show", "-p", "MainPID", "--value", "l.service"]),
        ["--user", "show", "-p", "MainPID", "--value", "l.service"]
    );
}

/// launchd as `install_launchd` sees it: one job, loaded in a domain or not, whether this
/// user is logged in at the screen (`gui/<uid>` there), and every call it got.
#[cfg(unix)]
#[derive(Default)]
struct FakeLaunchd {
    loaded: Option<Domain>,
    no_gui: bool,
    calls: Vec<Vec<String>>,
    cached_subscriber: Option<String>,
}

#[cfg(unix)]
impl FakeLaunchd {
    fn answer(&mut self, call: Launchctl<'_>) -> crate::Result<std::process::Output> {
        use std::os::unix::process::ExitStatusExt as _;
        self.calls.push(launchctl_argv(0, call));
        let there = |domain| domain == Domain::User || !self.no_gui;
        let (ok, stdout) = match call {
            Launchctl::PrintDomain(domain) => (there(domain), ""),
            Launchctl::Print(domain, _) if self.loaded == Some(domain) => (true, "\tpid = 42\n"),
            Launchctl::Print(..) => (false, ""),
            Launchctl::Bootout(domain, _) => {
                let held = self.loaded == Some(domain);
                if held {
                    self.loaded = None;
                }
                (held, "")
            }
            Launchctl::Bootstrap(domain, path) => {
                let has_background = std::fs::read_to_string(path).is_ok_and(|text| {
                    text.contains(
                        "\t<key>LimitLoadToSessionType</key>\n\t<string>Background</string>\n",
                    )
                });
                let domain_matches = match domain {
                    Domain::Gui => !has_background,
                    Domain::User => has_background,
                };
                let ok = there(domain) && self.loaded.is_none() && domain_matches;
                if ok {
                    self.loaded = Some(domain);
                }
                (ok, "")
            }
            Launchctl::Kickstart(domain, _) | Launchctl::Restart(domain, _) => {
                (self.loaded == Some(domain), "")
            }
        };
        let mut stdout = stdout.as_bytes().to_vec();
        if ok
            && matches!(call, Launchctl::Print(..))
            && let Some(cached) = &self.cached_subscriber
        {
            stdout.extend_from_slice(cached.as_bytes());
        }
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(if ok {
                0
            } else if matches!(call, Launchctl::Print(..)) {
                113 << 8
            } else {
                1 << 8
            }),
            stdout,
            stderr: Vec::new(),
        })
    }

    /// Install `args` (restarting when `restart`): its line, and the calls it made.
    fn install(
        &mut self,
        home: &Path,
        program: &Path,
        args: &[&str],
        restart: bool,
    ) -> (String, Vec<String>) {
        self.calls.clear();
        let line = install_launchd(
            home,
            Job::Subscriber,
            (program, &words(args)),
            restart,
            &mut |call| self.answer(call),
            aterm_types::update_boundary::Boundary::process(),
        )
        .expect("installs");
        let calls = self.calls.iter().map(|argv| argv.join(" ")).collect();
        (line, calls)
    }
}

/// A launchd bootstrap of the subscriber, with the bootout and kickstart around it, is not
/// repeated safely, so it holds the update boundary while it runs (init-on-by-default design
/// §6.7): an update's handoff waits for it, boundedly. Before, nothing held the guard.
#[cfg(target_os = "macos")]
#[test]
fn a_subscriber_bootstrap_holds_the_update_boundary() {
    use aterm_types::update_boundary::{Boundary, GuardedItem};
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let program = root.path().join("aterm");
    std::fs::write(&program, b"inert test program").unwrap();
    let boundary = Boundary::new();
    let mut launchd = FakeLaunchd::default();
    let mut unguarded = Vec::new();
    let mut answer = |call: Launchctl<'_>, launchd: &mut FakeLaunchd| {
        if matches!(
            call,
            Launchctl::Bootstrap(..) | Launchctl::Bootout(..) | Launchctl::Kickstart(..)
        ) && !boundary.is_held(GuardedItem::LaunchdBootstrap)
        {
            unguarded.push(launchctl_argv(0, call).join(" "));
        }
        launchd.answer(call)
    };
    let installed = install_absent_launchd(
        &home,
        &program,
        &mut || Ok(true),
        &mut |call| answer(call, &mut launchd),
        &boundary,
    )
    .unwrap();
    assert!(installed.is_some());
    install_launchd(
        &home,
        Job::Subscriber,
        (&program, &words(&["subscribe"])),
        true,
        &mut |call| answer(call, &mut launchd),
        &boundary,
    )
    .expect("reinstalls");
    assert!(
        launchd.calls.iter().flatten().any(|word| word == "bootout"),
        "the reinstall booted the job out: {:?}",
        launchd.calls
    );
    assert_eq!(
        unguarded,
        Vec::<String>::new(),
        "every bootstrap ran under the guard"
    );
}

/// A job's install reads what launchd has loaded and acts on it: leaves it running, boots it
/// out, bootstraps it. Its guard on the update boundary (design §6.7) comes before the first
/// read, so a wait there for an update's handoff, up to minutes, comes before anything is read
/// or written, and what it does rests on launchd as it is after the wait: here another install
/// loaded the job meanwhile, and this one boots it out before its bootstrap. Before, the guard
/// came after the reads and the unit's write, and the bootstrap acted on a reading from before
/// the wait.
#[cfg(unix)]
#[test]
fn a_job_install_takes_the_boundary_before_it_reads_what_launchd_has_loaded() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let program = root.path().join("aterm");
    std::fs::write(&program, b"inert test program").unwrap();
    let boundary = aterm_types::update_boundary::Boundary::new();
    let launchd = std::sync::Mutex::new(FakeLaunchd::default());
    let latch = boundary.latch_handoff(Duration::from_secs(60));
    let installed = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            install_launchd(
                &home,
                Job::Subscriber,
                (&program, &words(&["subscribe"])),
                false,
                &mut |call| launchd.lock().unwrap().answer(call),
                &boundary,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(60);
        while boundary.waiting() == 0 {
            assert!(Instant::now() < deadline, "the install never waited");
            aterm_uds::deadline::poll_yield();
        }
        assert_eq!(
            launchd.lock().unwrap().calls,
            Vec::<Vec<String>>::new(),
            "the install read launchd before its wait for the handoff"
        );
        assert!(
            !plist_path(&home, Job::Subscriber).exists(),
            "the install wrote its unit before its wait for the handoff"
        );
        // While the handoff is pending another install loads the job; then the handoff is
        // refused.
        launchd.lock().unwrap().loaded = Some(Domain::Gui);
        drop(latch);
        worker.join().unwrap()
    });
    let launchd = launchd.into_inner().unwrap();
    assert!(
        installed.is_ok() && launchd.calls.iter().flatten().any(|word| word == "bootout"),
        "the install acted on launchd as it was before its wait: {installed:?} {:?}",
        launchd.calls
    );
    assert_eq!(boundary.any_held(), None);
}

/// A new subscriber's install checks its policy, its unit and launchd at its local effect
/// boundary, right before the bootstrap. The bootstrap's guard waits for a pending update
/// handoff (design §6.7), up to minutes, after those checks; once the handoff is refused it
/// checks them again, and bootstraps nothing when one says no: the policy turned the
/// subscriber off, or a job of its label is loaded now. Before, the job was bootstrapped on
/// the checks made before the wait.
#[cfg(target_os = "macos")]
#[test]
fn a_subscriber_bootstrap_that_waited_for_a_handoff_checks_again_before_it_bootstraps() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for change in ["policy", "job", "nothing"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let program = root.path().join("aterm");
        std::fs::write(&program, b"inert test program").unwrap();
        let boundary = aterm_types::update_boundary::Boundary::new();
        let allowed = AtomicBool::new(true);
        let loaded = AtomicBool::new(false);
        let latch = boundary.latch_handoff(Duration::from_secs(60));
        let (installed, calls) = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let mut launchd = FakeLaunchd::default();
                let installed = install_absent_launchd(
                    &home,
                    &program,
                    &mut || Ok(allowed.load(Ordering::SeqCst)),
                    &mut |call| {
                        if loaded.load(Ordering::SeqCst) {
                            launchd.loaded = Some(Domain::Gui);
                        }
                        launchd.answer(call)
                    },
                    &boundary,
                );
                (installed, launchd.calls)
            });
            let deadline = Instant::now() + Duration::from_secs(60);
            while boundary.waiting() == 0 {
                assert!(Instant::now() < deadline, "the bootstrap never waited");
                aterm_uds::deadline::poll_yield();
            }
            // While the handoff is pending, the person turns the subscriber's policy off, or
            // another install loads its job; then the handoff is refused.
            match change {
                "policy" => allowed.store(false, Ordering::SeqCst),
                "job" => loaded.store(true, Ordering::SeqCst),
                _ => {}
            }
            drop(latch);
            worker.join().unwrap()
        });
        let bootstrapped = calls.iter().flatten().any(|word| word == "bootstrap");
        if change == "nothing" {
            // CONTROL: nothing changed while it waited, and it bootstraps once.
            assert!(
                bootstrapped && matches!(installed, Ok(Some(_))),
                "{installed:?} {calls:?}"
            );
        } else {
            assert!(
                !bootstrapped && matches!(installed, Ok(None)),
                "{change}: the subscriber was bootstrapped after its checks changed during \
                 its wait: {installed:?} {calls:?}"
            );
        }
        assert_eq!(boundary.any_held(), None);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_failed_new_subscriber_bootstrap_resumes_the_exact_unit_without_replacing_it() {
    use std::os::unix::fs::MetadataExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let program = root.path().join("aterm");
    std::fs::write(&program, b"inert test program").unwrap();
    let mut launchd = FakeLaunchd {
        no_gui: true,
        ..FakeLaunchd::default()
    };
    let first = install_absent_launchd(
        &home,
        &program,
        &mut || Ok(true),
        &mut |call| {
            if matches!(call, Launchctl::Bootstrap(..)) {
                return Err(Error::new("synthetic bootstrap failure"));
            }
            launchd.answer(call)
        },
        aterm_types::update_boundary::Boundary::process(),
    );
    assert!(first.is_err());
    let unit = plist_path(&home, Job::Subscriber);
    let before = std::fs::read(&unit).unwrap();
    let inode = std::fs::metadata(&unit).unwrap().ino();
    assert_eq!(
        recoverable_subscriber_unit(&home, &program).unwrap(),
        Some(true)
    );
    launchd.no_gui = false;
    let retry = install_absent_launchd(
        &home,
        &program,
        &mut || Ok(true),
        &mut |call| launchd.answer(call),
        aterm_types::update_boundary::Boundary::process(),
    )
    .unwrap();
    assert!(retry.is_some());
    assert_eq!(launchd.loaded, Some(Domain::User));
    assert_eq!(std::fs::metadata(&unit).unwrap().ino(), inode);
    assert_eq!(std::fs::read(&unit).unwrap(), before);
    assert!(
        !launchd
            .calls
            .iter()
            .flatten()
            .any(|word| word == "bootout" || word == "kickstart")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn new_subscriber_publication_refuses_late_foreign_units_jobs_and_policy() {
    use std::cell::Cell;
    for change in ["file", "job", "policy"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let program = root.path().join("aterm");
        std::fs::write(&program, b"inert test program").unwrap();
        let unit = plist_path(&home, Job::Subscriber);
        std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
        let allowed = Cell::new(true);
        let mut launchd = FakeLaunchd::default();
        if change == "job" {
            launchd.loaded = Some(Domain::Gui);
        }
        let result = install_absent_launchd(
            &home,
            &program,
            &mut || Ok(allowed.get()),
            &mut |call| {
                if matches!(call, Launchctl::PrintDomain(..)) {
                    if change == "file" {
                        std::fs::write(&unit, b"foreign unit").unwrap();
                    }
                    if change == "policy" {
                        allowed.set(false);
                    }
                }
                launchd.answer(call)
            },
            aterm_types::update_boundary::Boundary::process(),
        )
        .unwrap();
        assert!(result.is_none(), "{change}");
        assert!(
            !launchd
                .calls
                .iter()
                .flatten()
                .any(|word| word == "bootstrap" || word == "bootout" || word == "kickstart"),
            "{change}"
        );
        if change == "file" {
            assert_eq!(std::fs::read(&unit).unwrap(), b"foreign unit");
        }
        if change == "policy" {
            assert!(!unit.exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn a_job_launchd_runs_as_installed_is_left_running_and_no_install_kills_one() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path().join("home");
    let program = root.path().join("aterm");
    std::fs::write(&program, b"").expect("program");
    let label = Job::Subscriber.label();
    let plist = plist_path(&home, Job::Subscriber).display().to_string();
    let print = format!("print gui/0/{label}");
    let in_user = format!("print user/0/{label}");
    let bootout = format!("bootout gui/0/{label}");
    let bootstrap = format!("bootstrap gui/0 {plist}");
    let kickstart = format!("kickstart gui/0/{label}");
    let mut launchd = FakeLaunchd::default();

    // Fresh: bootstrapped, then started without killing anything.
    let (_, calls) = launchd.install(&home, &program, &["secrets", "watch"], false);
    assert_eq!(
        calls,
        [
            print.as_str(),
            "print gui/0",
            in_user.as_str(),
            bootstrap.as_str(),
            kickstart.as_str()
        ]
    );

    // `agent on` again, the job running from the same plist: one print, nothing else.
    let (line, calls) = launchd.install(&home, &program, &["secrets", "watch"], false);
    assert_eq!(calls, [print.as_str()]);
    assert!(line.ends_with("; already running"), "{line}");

    // CONTROL: a changed plist reloads the job, still never `kickstart -k`.
    let (line, calls) = launchd.install(&home, &program, &["secrets", "watch", "-v"], false);
    assert_eq!(
        calls,
        [
            print.as_str(),
            in_user.as_str(),
            bootout.as_str(),
            print.as_str(),
            bootstrap.as_str(),
            kickstart.as_str()
        ]
    );
    assert!(!line.ends_with("; already running"), "{line}");
    let written = std::fs::read_to_string(&plist).expect("plist");
    assert!(written.contains("<string>-v</string>"), "{written}");

    // `agent on --force` restarts the job it would otherwise leave running.
    let (_, calls) = launchd.install(&home, &program, &["secrets", "watch", "-v"], true);
    let restart = format!("kickstart -k gui/0/{label}");
    assert_eq!(
        calls,
        [
            print.as_str(),
            in_user.as_str(),
            bootout.as_str(),
            print.as_str(),
            bootstrap.as_str(),
            restart.as_str()
        ]
    );

    // A job launchd holds but does not run is started again.
    let (_, calls) = {
        let mut stopped = |call: Launchctl<'_>| match call {
            Launchctl::Print(..) if launchd.loaded.is_some() => {
                let mut out = launchd.answer(call)?;
                out.stdout = b"\tstate = not running\n".to_vec();
                Ok(out)
            }
            _ => launchd.answer(call),
        };
        launchd_calls(&home, &program, &mut stopped)
    };
    assert_eq!(
        calls,
        [
            print.as_str(),
            in_user.as_str(),
            bootout.as_str(),
            print.as_str(),
            bootstrap.as_str(),
            kickstart.as_str()
        ]
    );

    // A healthy same-path job otherwise bypasses held-program and absent-job repairs. Only
    // its exact owned old policy is refreshed, without selecting a different executable.
    let boundary = aterm_types::update_boundary::Boundary::new();
    let watch = words(&["secrets", "watch"]);
    let legacy = plist_text_in_with_subscriber_process_type(
        label,
        &program,
        &watch,
        &log_path(&home, Job::Subscriber),
        Domain::Gui,
        None,
        SubscriberProcessType::Background,
    );
    let unit = unit_file(&home, Job::Subscriber);
    write_unit_file(&unit, &legacy).unwrap();
    launchd.loaded = Some(Domain::Gui);
    launchd.calls.clear();
    let mut admitted = || Ok(true);
    assert!(
        refresh_subscriber_scheduling_with(
            &home,
            &program,
            &mut admitted,
            &mut |call| launchd.answer(call),
            &boundary,
            &mut |_| false,
        )
        .unwrap()
        .is_none(),
        "an unknown or old selected image stays capped"
    );
    assert!(launchd.calls.is_empty());
    assert_eq!(std::fs::read_to_string(&unit).unwrap(), legacy);

    record_pin(&home, Job::Subscriber, &program).unwrap();
    assert!(
        refresh_subscriber_scheduling_with(
            &home,
            &program,
            &mut admitted,
            &mut |call| launchd.answer(call),
            &boundary,
            &mut |_| panic!("a pin is not reconsidered for migration"),
        )
        .unwrap()
        .is_none()
    );
    clear_pin(&home, Job::Subscriber).unwrap();
    crate::fsx::write_atomic(&subscriber_off_path(&home), b"off\n", 0o600).unwrap();
    assert!(
        refresh_subscriber_scheduling_with(
            &home,
            &program,
            &mut admitted,
            &mut |call| launchd.answer(call),
            &boundary,
            &mut |_| panic!("Off is not reconsidered for migration"),
        )
        .unwrap()
        .is_none()
    );
    std::fs::remove_file(subscriber_off_path(&home)).unwrap();
    let foreign = legacy.replace(
        "</dict>\n</plist>",
        "<key>Disabled</key><true/></dict>\n</plist>",
    );
    write_unit_file(&unit, &foreign).unwrap();
    assert!(
        refresh_subscriber_scheduling_with(
            &home,
            &program,
            &mut admitted,
            &mut |call| launchd.answer(call),
            &boundary,
            &mut |_| panic!("a foreign definition is not migrated"),
        )
        .unwrap()
        .is_none()
    );
    write_unit_file(&unit, &legacy).unwrap();
    assert!(
        refresh_subscriber_scheduling_with(
            &home,
            &program,
            &mut || Ok(false),
            &mut |call| launchd.answer(call),
            &boundary,
            &mut |_| panic!("a withheld setup choice is not migrated"),
        )
        .unwrap()
        .is_none()
    );
    assert!(launchd.calls.is_empty());

    assert!(
        refresh_subscriber_scheduling_with(
            &home,
            &program,
            &mut admitted,
            &mut |call| launchd.answer(call),
            &boundary,
            &mut |_| true,
        )
        .unwrap()
        .is_none(),
        "owned disk bytes cannot authorize stopping a foreign cached job"
    );
    assert!(!launchd.calls.iter().flatten().any(|word| word == "bootout"));
    assert_eq!(std::fs::read_to_string(&unit).unwrap(), legacy);
    launchd.calls.clear();

    let cached = format!(
        "\tpath = {}\n\tprogram = {}\n\targuments = {{\n\t\t{}\n\t\tsecrets\n\t\twatch\n\t}}\n",
        unit.display(),
        program.display(),
        program.display()
    );
    for foreign in [
        cached.replace(unit.to_str().unwrap(), "/other/custom.plist"),
        cached.replace("\tpath = ", "\t\tpath = "),
        cached.replace(program.to_str().unwrap(), "/fixture/foreign-program"),
        cached.replace("\tprogram = ", "\t\tprogram = "),
        cached.replace("\t\twatch\n", "\t\tserve\n"),
        cached.replace("\t\twatch\n", "\t\twatch\n\t\textra\n"),
        format!("{cached}{cached}"),
    ] {
        assert!(!printed_subscriber_matches(&foreign, &program, &unit));
        launchd.cached_subscriber = Some(foreign);
        assert!(
            refresh_subscriber_scheduling_with(
                &home,
                &program,
                &mut admitted,
                &mut |call| launchd.answer(call),
                &boundary,
                &mut |_| true,
            )
            .unwrap()
            .is_none()
        );
        assert!(!launchd.calls.iter().flatten().any(|word| word == "bootout"));
        assert_eq!(std::fs::read_to_string(&unit).unwrap(), legacy);
        launchd.calls.clear();
    }
    assert!(printed_subscriber_matches(&cached, &program, &unit));
    launchd.cached_subscriber = Some(cached);

    let mut capable = |selected: &Path| {
        assert_eq!(selected, program.as_path());
        assert!(boundary.is_held(aterm_types::update_boundary::GuardedItem::LaunchdBootstrap));
        true
    };
    assert!(
        refresh_subscriber_scheduling_with(
            &home,
            &program,
            &mut admitted,
            &mut |call| launchd.answer(call),
            &boundary,
            &mut capable,
        )
        .unwrap()
        .is_some()
    );
    let canonical = plist_text_in_with_subscriber_process_type(
        label,
        &program,
        &watch,
        &log_path(&home, Job::Subscriber),
        Domain::Gui,
        None,
        SubscriberProcessType::Interactive,
    );
    assert_eq!(std::fs::read_to_string(&unit).unwrap(), canonical);
    assert_eq!(launchd.loaded, Some(Domain::Gui));
    assert!(launchd.calls.iter().flatten().any(|word| word == "bootout"));
    assert!(
        launchd
            .calls
            .iter()
            .flatten()
            .any(|word| word == "bootstrap")
    );
    launchd.calls.clear();
    assert!(
        refresh_subscriber_scheduling_with(
            &home,
            &program,
            &mut admitted,
            &mut |call| launchd.answer(call),
            &boundary,
            &mut |_| panic!("a current shape needs no migration"),
        )
        .unwrap()
        .is_none()
    );
    assert!(
        launchd.calls.is_empty(),
        "a canonical healthy job is left running"
    );

    // Capability comes from the loaded image and selected path together, not a version or
    // an arbitrary same-shaped executable. This test does not launch the inert old program.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    {
        let image =
            aterm_uds::process::LoadedImage::read(i32::try_from(std::process::id()).unwrap())
                .expect("the test's own loaded image");
        assert_eq!(
            subscriber_process_type_for(image.launch_path(), Some(&image)),
            SubscriberProcessType::Interactive
        );
        assert_eq!(
            subscriber_process_type_for(&program, Some(&image)),
            SubscriberProcessType::Background
        );
        assert_eq!(
            subscriber_process_type_for(image.launch_path(), None),
            SubscriberProcessType::Background
        );
    }
}

/// With no login at the Mac's screen (an SSH session's Mac) the job goes in `user/<uid>`,
/// the one domain there, loaded only as a background session's; once this user logs in at
/// the screen, setting it up again moves it there and never leaves a second copy running.
#[cfg(unix)]
#[test]
fn a_job_set_up_with_no_login_at_the_screen_runs_in_the_user_domain_until_one() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path().join("home");
    let program = root.path().join("aterm");
    std::fs::write(&program, b"").expect("program");
    let label = Job::Subscriber.label();
    let plist = plist_path(&home, Job::Subscriber);
    let shown = plist.display().to_string();
    let (print, in_user) = (
        format!("print gui/0/{label}"),
        format!("print user/0/{label}"),
    );
    let mut launchd = FakeLaunchd {
        no_gui: true,
        ..FakeLaunchd::default()
    };

    let (line, calls) = launchd.install(&home, &program, &["secrets", "watch"], false);
    let bootstrap_user = format!("bootstrap user/0 {shown}");
    let kickstart_user = format!("kickstart user/0/{label}");
    assert_eq!(
        calls,
        [
            print.as_str(),
            "print gui/0",
            in_user.as_str(),
            bootstrap_user.as_str(),
            kickstart_user.as_str()
        ]
    );
    assert_eq!(launchd.loaded, Some(Domain::User));
    assert!(line.contains("until the Mac restarts"), "{line}");
    let written = std::fs::read_to_string(&plist).expect("plist");
    assert!(
        written.contains("<string>Background</string>\n"),
        "{written}"
    );

    let (line, calls) = launchd.install(&home, &program, &["secrets", "watch"], false);
    assert_eq!(calls, [print.as_str(), "print gui/0", in_user.as_str()]);
    assert!(line.ends_with("; already running"), "{line}");

    // The person logs in at the screen and sets it up again: out of the user domain, into
    // theirs, with the plist a login there loads.
    launchd.no_gui = false;
    let (line, calls) = launchd.install(&home, &program, &["secrets", "watch"], false);
    let bootout_user = format!("bootout user/0/{label}");
    let bootstrap_gui = format!("bootstrap gui/0 {shown}");
    let kickstart_gui = format!("kickstart gui/0/{label}");
    assert_eq!(
        calls,
        [
            print.as_str(),
            "print gui/0",
            in_user.as_str(),
            bootout_user.as_str(),
            in_user.as_str(),
            bootstrap_gui.as_str(),
            kickstart_gui.as_str()
        ]
    );
    assert_eq!(launchd.loaded, Some(Domain::Gui));
    assert!(!line.contains("until the Mac restarts"), "{line}");
    let written = std::fs::read_to_string(&plist).expect("plist");
    assert!(!written.contains("LimitLoadToSessionType"), "{written}");
}

#[cfg(unix)]
fn launchd_calls(
    home: &Path,
    program: &Path,
    launchctl: &mut LaunchctlRun<'_>,
) -> (String, Vec<String>) {
    let mut calls = Vec::new();
    let line = install_launchd(
        home,
        Job::Subscriber,
        (program, &words(&["secrets", "watch", "-v"])),
        false,
        &mut |call| {
            calls.push(launchctl_argv(0, call).join(" "));
            launchctl(call)
        },
        aterm_types::update_boundary::Boundary::process(),
    )
    .expect("installs");
    (line, calls)
}

#[test]
fn the_pid_is_the_services_own_line() {
    let running = "gui/501/systems.alab.aterm-secrets = {\n\
                   \tactive count = 1\n\
                   \tstate = running\n\
                   \n\
                   \tenvironment = {\n\
                   \t\tHOME => /Users//x\n\
                   \t}\n\
                   \n\
                   \truns = 1\n\
                   \tpid = 1688\n\
                   }\n";
    assert_eq!(print_pid(running), Some(1688));
    assert_eq!(print_pid("gui/501/l = {\n\tstate = not running\n}\n"), None);
    assert_eq!(
        print_pid("l = {\n\tsection = {\n\t\tpid = 9\n\t}\n}\n"),
        None
    );
    assert_eq!(print_pid("\tpid = 0\n"), None);
    assert_eq!(print_pid("\tpid = 77  \n"), Some(77));
    assert_eq!(main_pid("4242\n"), Some(4242));
    assert_eq!(main_pid("MainPID=4242\n"), Some(4242));
    assert_eq!(main_pid("0\n"), None);
    assert_eq!(main_pid(""), None);
}

/// Only a successful launchd reading or its unloaded-job status establishes service state.
/// Permission failures and a signal remain unknown even when their output looks like a PID.
#[test]
fn a_system_job_read_distinguishes_stopped_from_unreadable() {
    let running = "system/job = {\n\tpid = 77\n}\n";
    assert_eq!(system_job_running_of(Some(0), running), Some(true));
    assert_eq!(
        system_job_running_of(Some(0), "\tstate = waiting\n"),
        Some(false)
    );
    assert_eq!(
        system_job_running_of(Some(113), "no such service"),
        Some(false)
    );
    assert_eq!(system_job_running_of(Some(1), running), None);
    assert_eq!(system_job_running_of(None, running), None);
}

#[test]
fn systemctl_is_found_by_absolute_path_or_refused_by_name() {
    let found = systemctl_path(&SYSTEMCTL_PATHS, &|p| p == Path::new("/bin/systemctl"));
    assert_eq!(found, Ok(PathBuf::from("/bin/systemctl")));
    let first = systemctl_path(&SYSTEMCTL_PATHS, &|_| true);
    assert_eq!(first, Ok(PathBuf::from("/usr/bin/systemctl")));
    let none = systemctl_path(&SYSTEMCTL_PATHS, &|_| false).unwrap_err();
    assert!(
        none.0.contains("no systemctl at /usr/bin/systemctl"),
        "{none}"
    );
    assert!(none.0.contains("supervisor"), "{none}");
}

#[test]
fn a_build_in_a_target_directory_is_never_a_services_program() {
    let cargo = |p: &Path| p == Path::new("/Users//x/aterm/Cargo.toml");
    let none = |_: &Path| false;
    assert!(in_build_dir(
        Path::new("/Users//x/aterm/target/debug/aterm"),
        &cargo
    ));
    assert!(in_build_dir(
        Path::new("/Users//x/aterm/target/x/y/aterm"),
        &cargo
    ));
    assert!(in_build_dir(Path::new("/w/target/release/aterm"), &none));
    assert!(in_build_dir(
        Path::new("/w/target/aarch64-apple-darwin/release/aterm"),
        &none
    ));
    assert!(in_build_dir(
        Path::new("/Users//x/.cache/aterm-secrets-init.noindex/target/debug/deps/aterm"),
        &none
    ));
    assert!(in_build_dir(
        Path::new("/Users//x/aterm/target.noindex/debug/aterm"),
        &none
    ));
    assert!(in_build_dir(
        Path::new("/Users//x/aterm/target.noindex/aterm (dev).app/Contents/MacOS/aterm"),
        &none
    ));
    for path in [
        "/Applications/aterm.app/Contents/MacOS/aterm",
        "/Users//x/.local/bin/aterm",
        "/Users//x/target",
        "/opt/target/bin/aterm",
        "/Users//x/aterm-secrets-init.noindex/aterm",
        "/Users//x/aterm-secrets-init.noindex",
    ] {
        assert!(!in_build_dir(Path::new(path), &none), "{path}");
    }
}

#[test]
fn a_bundled_executable_runs_from_its_bundle_and_a_build_is_refused() {
    let none = |_: &Path| false;
    let app = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    assert_eq!(service_program(app, &none), Ok(app.to_path_buf()));
    let plain = Path::new("/Users//x/.local/bin/aterm");
    assert_eq!(service_program(plain, &none), Ok(plain.to_path_buf()));
    let main = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let helper = Path::new("/Applications/aterm.app/Contents/Helpers/aterm");
    assert_eq!(
        service_program(helper, &|p: &Path| p == main),
        Ok(main.to_path_buf())
    );
    let nested = Path::new("/Applications/aterm.app/Contents/Library/X.app/Contents/MacOS/aterm");
    assert_eq!(
        service_program(nested, &|p: &Path| p == main),
        Ok(main.to_path_buf())
    );
    let lost = service_program(helper, &none).unwrap_err();
    assert!(lost.0.contains("--program"), "{lost}");
    let build = service_program(Path::new("/w/aterm/target/debug/aterm"), &none).unwrap_err();
    assert!(build.0.contains("cargo target directory"), "{build}");
    assert!(build.0.contains("--program"), "{build}");
    assert!(service_program(Path::new("aterm"), &none).is_err());
}

#[test]
fn a_copy_that_goes_away_is_never_a_services_program() {
    let none = |_: &Path| false;
    let temp = std::env::temp_dir().join("x").join("aterm");
    let resolved = std::env::temp_dir()
        .canonicalize()
        .expect("the temporary directory")
        .join("aterm");
    for path in [
        Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm"),
        Path::new("/private/var/folders/x/AppTranslocation/Y/d/aterm.app/Contents/MacOS/aterm"),
        Path::new("/tmp/aterm"),
        Path::new("/var/tmp/x/aterm"),
        &temp,
        &resolved,
    ] {
        let refused = service_program(path, &none).unwrap_err();
        assert!(refused.0.contains("goes away"), "{refused}");
        assert!(refused.0.contains("--program"), "{refused}");
    }
    for path in [
        "/Applications/aterm.app/Contents/MacOS/aterm",
        "/Users//x/.local/bin/aterm",
        "/Users//x/Volumes/aterm",
        "/tmpfoo/aterm",
    ] {
        assert!(!transient(Path::new(path)), "{path}");
    }
}

#[test]
fn the_test_binary_itself_is_refused_as_a_build() {
    let refused = program_for_service().unwrap_err();
    assert!(refused.0.contains("cargo target directory"), "{refused}");
}

#[test]
fn a_tagged_cache_directory_is_a_build_whatever_its_name() {
    let tag = |p: &Path| p == Path::new("/scratch/out/CACHEDIR.TAG");
    assert!(in_build_dir(
        Path::new("/scratch/out/debug/deps/aterm-0123"),
        &tag
    ));
    assert!(!in_build_dir(Path::new("/scratch/other/aterm"), &tag));
    let own = |p: &Path| p == Path::new("/scratch/aterm/CACHEDIR.TAG");
    assert!(!in_build_dir(Path::new("/scratch/aterm"), &own));
}

/// A build directory apart from the target directory (`build.build-dir`, atpkg's targo shim)
/// carries no tag and no `target` name: its `<profile>/deps` alone says it is cargo's.
#[test]
fn a_units_build_directory_is_a_build_whatever_holds_it() {
    let none = |_: &Path| false;
    assert!(in_build_dir(
        Path::new(
            "/Users//x/Library/Caches/aterm/.ubd/cargo-build.noindex/Users//x/aterm/\
             .cargo-build/debug/deps/aterm_secrets-0123"
        ),
        &none
    ));
    assert!(in_build_dir(
        Path::new("/b/.cargo-build/release/deps/aterm-0123"),
        &none
    ));
    assert!(in_build_dir(
        Path::new("/b/.cargo-build/debug/build/aterm-secrets/0123/out/aterm_secrets-0123"),
        &none
    ));
    assert!(!in_build_dir(Path::new("/Users//x/debug/aterm"), &none));
    assert!(!in_build_dir(Path::new("/Users//x/build/aterm"), &none));
    assert!(!in_build_dir(Path::new("/Users//x/deps/aterm"), &none));
}

#[test]
fn writing_the_plist_makes_a_0644_file_and_a_private_log_directory() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("home");
    let program = root.path().join("aterm");
    std::fs::write(&program, b"exe").expect("program");
    let args = words(&["secrets", "watch"]);
    let plist = write_plist(&home, Job::Subscriber, &program, &args).expect("written");
    assert_eq!(plist, plist_path(&home, Job::Subscriber));
    let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode(&plist), 0o644);
    assert_eq!(mode(&home.join(".config/aterm/secrets")), 0o700);
    let text = std::fs::read_to_string(&plist).expect("read");
    assert_eq!(
        text,
        plist_text_in_with_subscriber_process_type(
            Job::Subscriber.label(),
            &program,
            &args,
            &log_path(&home, Job::Subscriber),
            Domain::Gui,
            None,
            SubscriberProcessType::Background,
        )
    );
    assert!(text.contains(&format!(
        "<string>{}</string>",
        aterm_codec::xml::escape(&home.to_string_lossy())
    )));
    write_plist(&home, Job::Subscriber, &program, &args).expect("rewritten");
    let names: Vec<_> = std::fs::read_dir(plist.parent().expect("dir"))
        .expect("list")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");

    let unit = write_unit(&home, Job::Server, &program, &args).expect("unit");
    assert_eq!(mode(&unit), 0o644);
    assert_eq!(
        std::fs::read_to_string(&unit).expect("read"),
        systemd_unit(Job::Server.description(), &program, &args)
    );
}

#[test]
fn a_unit_file_is_refused_for_what_it_cannot_name() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path();
    let program = root.path().join("aterm");
    std::fs::write(&program, b"exe").expect("program");
    let refuse = |program: &Path, args: &[String]| {
        write_plist(home, Job::Subscriber, program, args).unwrap_err()
    };
    assert!(refuse(Path::new("aterm"), &[]).0.contains("absolute"));
    assert!(
        refuse(&root.path().join("missing"), &[])
            .0
            .contains("missing")
    );
    assert!(refuse(&program, &words(&["a\nb"])).0.contains("control"));
    assert!(refuse(home, &[]).0.contains("not a file"));
    assert!(!plist_path(home, Job::Subscriber).exists());

    if let Some(mkfifo) = ["/usr/bin/mkfifo", "/bin/mkfifo"]
        .into_iter()
        .find(|p| Path::new(p).is_file())
    {
        let fifo = root.path().join("fifo.plist");
        let made = aterm_uds::spawnfd::command(mkfifo)
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(made.success());
        assert_eq!(
            unit_program(&fifo),
            None,
            "a FIFO never blocks unit_program"
        );
    }
    let huge = root.path().join("huge.plist");
    std::fs::write(&huge, vec![b' '; UNIT_FILE_MOST + 1]).expect("huge");
    assert_eq!(unit_program(&huge), None, "oversized unit file refused");
}

/// A subscriber read installed but not running is read again until it runs, within
/// launchd's restart window from when it was first seen down, and is stopped past it.
#[test]
fn a_subscriber_down_for_launchds_restart_gap_is_not_stopped() {
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let clock = Cell::new(t0);
    let reads = Cell::new(0);
    let read = |back_after: Option<u32>| {
        let reads = &reads;
        move || -> Result<bool, String> {
            reads.set(reads.get() + 1);
            Ok(back_after.is_some_and(|after| reads.get() >= after))
        }
    };
    let mut now = || clock.get();
    let mut pause = |d: Duration| clock.set(clock.get() + d);
    // Back on the third read again: running, and nothing more is read.
    let mut since = None;
    assert_eq!(
        back_within_restart_window(&mut since, &mut now, &mut read(Some(3)), &mut pause),
        Ok(true)
    );
    assert_eq!(reads.get(), 3);
    assert!(clock.get() - t0 < RESTART_WINDOW);
    // Never back: stopped once the window has passed since first seen down.
    reads.set(0);
    clock.set(t0);
    let mut since = None;
    assert_eq!(
        back_within_restart_window(&mut since, &mut now, &mut read(None), &mut pause),
        Ok(false)
    );
    assert_eq!(since, Some(t0), "first seen down");
    assert_eq!(
        clock.get(),
        t0 + RESTART_WINDOW,
        "waited the window, no more"
    );
    let waited = reads.get();
    assert!(waited >= 2, "read again within it: {waited}");
    // A later read still down past the window: stopped at once.
    assert_eq!(
        back_within_restart_window(&mut since, &mut now, &mut read(None), &mut pause),
        Ok(false)
    );
    assert_eq!(reads.get(), waited, "no read, no wait");
    assert_eq!(clock.get(), t0 + RESTART_WINDOW);
    // A read within the window of the first waits only what is left of it.
    clock.set(t0 + RESTART_WINDOW / 2);
    let mut since = Some(t0);
    assert_eq!(
        back_within_restart_window(&mut since, &mut now, &mut read(None), &mut pause),
        Ok(false)
    );
    assert_eq!(clock.get(), t0 + RESTART_WINDOW);
}

#[test]
fn vault_daemon_plist_and_argv_target_system_domain_without_login_session() {
    let store = Path::new(DAEMON_STORE);
    let labelled = daemon_serve_args(store, Some("m19"));
    assert_eq!(
        labelled,
        vec![
            "secrets".to_string(),
            "serve".to_string(),
            "--store".to_string(),
            DAEMON_STORE.to_string(),
            "--pin".to_string(),
            format!("{}={}", crate::ALAB_VAULT, crate::ALAB_SIGNER),
            "--advertise".to_string(),
            "m19".to_string(),
        ]
    );
    // Negative control: an unlabelled daemon passes `--no-advertise` so it never contends for
    // `alab-secrets.local`.
    let unlabelled = daemon_serve_args(store, None);
    assert_eq!(
        unlabelled.last().map(String::as_str),
        Some("--no-advertise")
    );

    let program = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let text = daemon_plist_text(
        program,
        &labelled,
        Path::new(DAEMON_LOG),
        Some("com.aterm.aterm"),
    );
    assert!(text.contains(&format!("<string>{DAEMON_LABEL}</string>")));
    assert!(!text.contains("LimitLoadToSessionType"));
    assert!(!text.contains("<key>HOME</key>"));
    assert_eq!(
        plist_argv(&text).as_deref(),
        Some(
            std::iter::once(program.to_string_lossy().into_owned())
                .chain(labelled)
                .collect::<Vec<_>>()
                .as_slice()
        )
    );

    let plist = Path::new(DAEMON_PLIST);
    assert_eq!(
        system_launchctl_argv(SystemLaunchctl::Print(DAEMON_LABEL)),
        vec!["print", "system/systems.alab.aterm-vault-server"]
    );
    assert_eq!(
        system_launchctl_argv(SystemLaunchctl::Bootstrap(plist)),
        vec!["bootstrap", "system", DAEMON_PLIST]
    );
    assert_eq!(
        system_launchctl_argv(SystemLaunchctl::Bootout(DAEMON_LABEL)),
        vec!["bootout", "system/systems.alab.aterm-vault-server"]
    );
}

#[cfg(unix)]
#[test]
fn daemon_state_reads_system_domain_and_ignores_scratch_homes() {
    use std::os::unix::process::ExitStatusExt as _;
    let plist = Path::new(DAEMON_PLIST);
    let out = |raw: i32, stdout: &str| std::process::Output {
        status: std::process::ExitStatus::from_raw(raw << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    };
    assert_eq!(
        daemon_state_with(plist, true, &mut |_| {
            Ok(out(
                0,
                "system/systems.alab.aterm-vault-server = {\n\tpid = 4242\n}\n",
            ))
        })
        .unwrap(),
        State::Installed {
            pid: Some(4242),
            unit: plist.to_path_buf(),
        }
    );
    assert_eq!(
        daemon_state_with(plist, true, &mut |_| Ok(out(113, ""))).unwrap(),
        State::Installed {
            pid: None,
            unit: plist.to_path_buf(),
        }
    );
    assert_eq!(
        daemon_state_with(plist, false, &mut |_| Ok(out(113, ""))).unwrap(),
        State::Absent
    );
    assert_eq!(
        daemon_state_for_home(Path::new("/nonexistent/scratch-home-for-test")).unwrap(),
        State::Absent
    );
}
