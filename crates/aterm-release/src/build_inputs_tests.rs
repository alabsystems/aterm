// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(target_os = "macos")]
#[test]
fn a_captured_sdk_binds_command_and_seal_and_refuses_selection_or_content_drift() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let a = dir.path().join("sdk-a");
    let b = dir.path().join("sdk-b");
    for root in [&a, &b] {
        std::fs::create_dir(root).unwrap();
        std::fs::write(root.join("SDKSettings.plist"), "original SDK").unwrap();
    }
    let sdk = AppleSdk {
        root: a.canonicalize().unwrap(),
    };
    let selected = std::cell::RefCell::new(sdk.root.clone());
    let mut command = aterm_uds::spawnfd::command("unused");
    command.env_clear().env("SDKROOT", &b);
    sdk.apply(&mut command);
    let command_root = PathBuf::from(
        command
            .get_envs()
            .find(|(name, _)| *name == "SDKROOT")
            .unwrap()
            .1
            .unwrap(),
    );
    assert_eq!(command_root, sdk.root);
    let mut roots = || {
        sdk.verify_selected(selected.borrow().clone())?;
        Ok(vec![(input_seal::SDK.to_owned(), command_root.clone())])
    };
    let mut process = input_seal::Process::unstarted(SCHEMAS);
    let mut sources = input_seal::Sources {
        roots: &mut roots,
        facts: &mut |_| read_facts(),
        open: &mut crate::durable::open_regular_file,
        now: &mut || input_seal::now_ns() + 3_600_000_000_000,
        stopped: &|| false,
        report: &mut |_| {},
    };
    let before = process.identity(&mut sources, false).unwrap();
    assert!(process.holds(&mut sources, &before));
    *selected.borrow_mut() = b.canonicalize().unwrap();
    assert!(process.identity(&mut sources, false).is_err());
    assert!(!process.holds(&mut sources, &before));
    *selected.borrow_mut() = sdk.root.clone();
    std::fs::write(a.join("SDKSettings.plist"), "changed SDK").unwrap();
    assert!(!process.holds(&mut sources, &before));
    assert_ne!(process.identity(&mut sources, false).unwrap(), before);
}

#[cfg(target_os = "macos")]
#[test]
fn a_native_seal_needs_neither_stock_proxies_nor_foreign_standard_libraries() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let bin = root.join("bin");
    std::fs::create_dir_all(root.join("lib/rustlib/aarch64-apple-darwin/lib")).unwrap();
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(
        root.join("lib/rustlib/aarch64-apple-darwin/lib/native.rlib"),
        b"native",
    )
    .unwrap();
    let trustc = bin.join("trustc");
    let quoted_root = crate::release::shell_word(root.to_str().unwrap());
    std::fs::write(&trustc, format!("#!/bin/sh\ncase \"$1\" in --print) printf '%s\\n' {quoted_root} ;; -vV) printf 'host: aarch64-apple-darwin\\n' ;; *) exit 1 ;; esac\n")).unwrap();
    std::fs::set_permissions(&trustc, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut compiler = candidate(&root);
    compiler.stock = Ok(crate::buildplan::Stock::unavailable_for_test(
        &root,
        &compiler.pin.stock_toolchain,
    ));
    let sdk = AppleSdk::capture().unwrap();
    let before = crate::buildplan::TakePurpose::Native
        .identity(Some(&compiler), &sdk)
        .unwrap()
        .unwrap();
    assert!(
        crate::buildplan::TakePurpose::Universal
            .identity(Some(&compiler), &sdk)
            .is_err(),
        "the same inputs cannot seal a universal product without its stock role"
    );
    assert_eq!(
        crate::buildplan::TakePurpose::Native
            .identity(Some(&compiler), &sdk)
            .unwrap()
            .unwrap(),
        before
    );
    let captured_error = "captured unusable RUSTUP_HOME".to_owned();
    compiler.stock = Err(captured_error.clone());
    assert_eq!(
        crate::buildplan::TakePurpose::Native
            .identity(Some(&compiler), &sdk)
            .unwrap(),
        Some(before)
    );
    assert_eq!(
        crate::buildplan::TakePurpose::Universal
            .identity(Some(&compiler), &sdk)
            .unwrap_err(),
        captured_error,
    );
    assert!(
        crate::buildplan::TakePurpose::Native
            .identity(None, &sdk)
            .unwrap()
            .is_some()
    );
}

/// A call's report reaches the log [`arm`] named (the cutter's transcript, which `release
/// status` counts its `inputs` events from) line by line, and no file where none was named.
/// NEGATIVE CONTROL: without the append the transcript holds none of it.
#[test]
fn a_report_reaches_the_log_the_release_armed() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let log = dir.path().join(crate::release_timeline::TRANSCRIPT);
    let event = "release-event: {\"schema\":1,\"event\":\"inputs\",\"mode\":\"adopted\",\
                 \"entries\":9,\"secs\":0.5}";
    say_to(Some(&log), "inputs: rechecked by stat 9 entries in 500 ms");
    say_to(Some(&log), event);
    say_to(None, "inputs: hashed 9 entries in 1.0 s");
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        format!("inputs: rechecked by stat 9 entries in 500 ms\n{event}\n")
    );
    let mut timeline = crate::release_timeline::Timeline::default();
    timeline
        .read(
            crate::release_timeline::TRANSCRIPT,
            std::fs::read(&log).unwrap().as_slice(),
        )
        .unwrap();
    // The cutter's own call in its transcript makes the hash count known: none.
    let value = timeline.value();
    assert_eq!(
        (
            value["full_hashes"].as_u64(),
            value["seal_rechecks"].as_u64()
        ),
        (Some(0), Some(1)),
        "{value}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

/// The tree the release's vectors are of: three files of three modes, a link to a sibling, a
/// dangling link, and a link back to its own directory.
#[cfg(unix)]
fn fixed_tree(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let root = dir.join("inputs");
    for (name, bytes, mode) in [
        ("bin/trustc", "trustc", 0o755),
        ("lib/std.rlib", "std", 0o644),
        ("sdk/usr/include/stdio.h", "header", 0o444),
    ] {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    std::os::unix::fs::symlink("trustc", root.join("bin/rustc")).unwrap();
    std::os::unix::fs::symlink("missing", root.join("bin/crashlog")).unwrap();
    std::os::unix::fs::symlink(".", root.join("sdk/usr/include/self")).unwrap();
    for sub in ["", "bin", "lib", "sdk", "sdk/usr", "sdk/usr/include"] {
        std::fs::set_permissions(root.join(sub), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    root
}

/// Facts with every field read.
#[cfg(unix)]
fn read_facts() -> input_seal::Facts {
    input_seal::Facts {
        os_build: Some("25B78".into()),
        model: Some("Mac16,1".into()),
        sdk_settings: Some("5".repeat(64)),
        trust_build: Some(format!("16387 {}", "4".repeat(64))),
    }
}

#[cfg(unix)]
fn candidate(root: &Path) -> CompilerInputs {
    CompilerInputs {
        bin: root.join("bin").canonicalize().unwrap(),
        stock: crate::buildplan::Stock::under(root.as_os_str(), "1.91.1"),
        pin: crate::trust_lock::CompilerPin {
            build: 27009,
            tree_root: "4".repeat(64),
            compiler_commit: "5".repeat(40),
            receipt: "SEED-MINT sha256:fixture".into(),
            stock_toolchain: "1.91.1".into(),
        },
    }
}

#[cfg(unix)]
fn role_roots(root: &Path, compiler: &CompilerInputs) -> Vec<input_seal::Root> {
    candidate_roots_with(compiler, |bin, stock| {
        assert_eq!(bin, compiler.bin);
        assert_eq!(stock, compiler.pin.stock_toolchain);
        Ok(vec![
            (input_seal::TRUST_SELECTED.into(), bin.to_owned()),
            ("stock/bin".into(), root.join("lib")),
            (input_seal::SDK.into(), root.join("sdk")),
        ])
    })
    .unwrap()
}

#[cfg(unix)]
#[derive(Default)]
struct RoleReads {
    opens: std::cell::Cell<usize>,
    hashes: std::cell::Cell<usize>,
    adoptions: std::cell::Cell<usize>,
}

#[cfg(unix)]
fn role_identity(
    process: &mut input_seal::Process,
    compiler: &CompilerInputs,
    root: &Path,
    reads: &RoleReads,
) -> String {
    process
        .identity(
            &mut input_seal::Sources {
                roots: &mut || Ok(role_roots(root, compiler)),
                facts: &mut |_| candidate_facts(compiler, read_facts()),
                open: &mut |path| {
                    reads.opens.set(reads.opens.get() + 1);
                    crate::durable::open_regular_file(path)
                },
                now: &mut || input_seal::now_ns() + 3_600_000_000_000,
                stopped: &|| false,
                report: &mut |report| match report {
                    Report::Hashed { .. } => reads.hashes.set(reads.hashes.get() + 1),
                    Report::Adopted { .. } => reads.adoptions.set(reads.adoptions.get() + 1),
                    _ => {}
                },
            },
            false,
        )
        .unwrap()
}

/// Identical physical inputs under a different complete source declaration are another
/// candidate identity and another kept-file key, including a stock-only change.
#[cfg(unix)]
#[test]
fn candidate_seals_bind_every_compiler_pin_field() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = fixed_tree(dir.path());
    let compiler = candidate(&root);
    let state = dir.path().join("state/inputs-seal");
    let mut process = input_seal::Process::unstarted(SCHEMAS);
    process.arm(state.clone());
    let first = role_identity(&mut process, &compiler, &root, &RoleReads::default());
    let mut changed = vec![compiler.clone(); 5];
    changed[0].pin.build += 1;
    changed[1].pin.tree_root = "6".repeat(64);
    changed[2].pin.compiler_commit = "7".repeat(40);
    changed[3].pin.receipt.push_str("-other");
    changed[4].pin.stock_toolchain = "1.92.0".into();
    for other in changed {
        assert_ne!(
            candidate_facts(&compiler, read_facts()),
            candidate_facts(&other, read_facts())
        );
        assert!(
            input_seal::matching_kept(SCHEMAS, &state, &first, &role_roots(&root, &other))
                .is_none()
        );
        let actual = role_identity(&mut process, &other, &root, &RoleReads::default());
        assert_ne!(actual, first, "{:?}", other.pin);
    }
    assert_eq!(
        role_identity(&mut process, &compiler, &root, &RoleReads::default()),
        first,
        "an immutable original binding still reads its own retained seal"
    );
}

/// A warm call and the receiving process reuse only the same roles. Handing A's valid
/// seal to B cannot authorize it, even where both roles resolve the same physical files.
#[cfg(unix)]
#[test]
fn candidate_roles_recheck_and_handoff_without_ambient_pin_reuse() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = fixed_tree(dir.path());
    let compiler = candidate(&root);
    let state = dir.path().join("state/inputs-seal");
    let mut process = input_seal::Process::unstarted(SCHEMAS);
    process.arm(state);
    let first = role_identity(&mut process, &compiler, &root, &RoleReads::default());
    let handed = process.saved().unwrap().to_owned();
    let warm = RoleReads::default();
    assert_eq!(role_identity(&mut process, &compiler, &root, &warm), first);
    assert_eq!(warm.opens.get(), 0);
    assert_eq!(warm.hashes.get(), 0);

    let mut receiver = input_seal::Process::unstarted(SCHEMAS);
    receiver.begin(Some(handed.clone()), None);
    let adopted = RoleReads::default();
    assert_eq!(
        role_identity(&mut receiver, &compiler, &root, &adopted),
        first
    );
    assert_eq!(adopted.opens.get(), 0);
    assert_eq!(adopted.adoptions.get(), 1);

    let mut other = compiler.clone();
    other.pin.stock_toolchain = "1.92.0".into();
    assert!(!process.holds(
        &mut input_seal::Sources {
            roots: &mut || Ok(role_roots(&root, &other)),
            facts: &mut |_| candidate_facts(&other, read_facts()),
            open: &mut |_| panic!("a held check must hash no file"),
            now: &mut || input_seal::now_ns() + 3_600_000_000_000,
            stopped: &|| false,
            report: &mut |_| {},
        },
        &first,
    ));
    let mut receiver = input_seal::Process::unstarted(SCHEMAS);
    receiver.begin(Some(handed), None);
    let refused = RoleReads::default();
    assert_ne!(role_identity(&mut receiver, &other, &root, &refused), first);
    assert_eq!(refused.adoptions.get(), 0);
    assert_eq!(refused.hashes.get(), 1);
    assert!(refused.opens.get() > 0);
}

#[cfg(unix)]
#[test]
fn candidate_root_resolution_never_falls_back_or_accepts_an_alias() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = fixed_tree(dir.path());
    let mut compiler = candidate(&root);
    let why = candidate_roots_with(&compiler, |bin, stock| {
        assert_eq!(bin, compiler.bin);
        assert_eq!(stock, compiler.pin.stock_toolchain);
        Err("selected role unavailable".into())
    })
    .unwrap_err();
    assert_eq!(why, "selected role unavailable");
    let alias = dir.path().join("alias-bin");
    std::os::unix::fs::symlink(&compiler.bin, &alias).unwrap();
    compiler.bin = alias;
    assert!(candidate_roots_with(&compiler, |_, _| panic!("noncanonical role executed")).is_err());
    compiler.bin = "relative/bin".into();
    assert!(candidate_roots_with(&compiler, |_, _| panic!("relative role executed")).is_err());
}

/// The release's identity of [`fixed_tree`], taken through `atpkg::input_seal` under
/// [`SCHEMAS`], with every fact read and with none: the bytes the release's seal stood for
/// before its protocol moved to atpkg (`H(SEAL_SCHEMA, SCHEMA, digest, facts)`, each field
/// length-prefixed, each fact `some`/`none`). NEGATIVE CONTROL: swap [`SCHEMA`] and
/// [`SEAL_SCHEMA`]'s places in the identity and both vectors change.
#[cfg(unix)]
#[test]
fn the_release_identity_of_a_fixed_tree_is_its_vector() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = fixed_tree(dir.path());
    let roots = vec![
        ("inputs".to_string(), root.clone()),
        ("again".to_string(), root.join("bin")),
    ];
    let identity = |facts: input_seal::Facts| {
        input_seal::Process::unstarted(SCHEMAS)
            .identity(
                &mut input_seal::Sources {
                    roots: &mut || Ok(roots.clone()),
                    facts: &mut |_| facts.clone(),
                    open: &mut crate::durable::open_regular_file,
                    now: &mut input_seal::now_ns,
                    stopped: &|| false,
                    report: &mut |_| {},
                },
                false,
            )
            .unwrap()
    };
    assert_eq!(
        (
            identity(read_facts()).as_str(),
            identity(input_seal::Facts::default()).as_str()
        ),
        (
            "f222d4607655f614ccfeb34adb607a1f8ffb48aaa4c2d32f106dc828090da518",
            "99676f42287dee09e213848b182ff683a4139cf2ad3b4d8bbe8aaa0a22d200f4"
        )
    );
}

/// Each seal call the release makes reaches its log as the release's timeline counts it
/// ([`report`]): a take as a full hash, a held seal's recheck and another process's adoption of
/// the kept seal as seal rechecks, each by one `release-event:` line of its mode. NEGATIVE
/// CONTROL: report an adoption's mode as `hashed`, and the log's events name two takes.
#[cfg(unix)]
#[test]
fn the_release_timeline_counts_each_seal_call_as_it_was_reported() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = fixed_tree(dir.path());
    let state = dir.path().join("state/inputs-seal");
    let log = dir.path().join(crate::release_timeline::TRANSCRIPT);
    let roots = vec![("inputs".to_string(), root)];
    let ask = |process: &mut input_seal::Process| {
        process
            .identity(
                &mut input_seal::Sources {
                    roots: &mut || Ok(roots.clone()),
                    facts: &mut |_| read_facts(),
                    open: &mut crate::durable::open_regular_file,
                    // An hour on: the scratch tree has settled.
                    now: &mut || input_seal::now_ns() + 3_600_000_000_000,
                    stopped: &|| false,
                    report: &mut |said| report(Some(&log), said),
                },
                false,
            )
            .unwrap()
    };
    let start = || {
        let mut process = input_seal::Process::unstarted(SCHEMAS);
        process.arm(state.clone());
        process.begin(None, None);
        process
    };
    let mut first = start();
    let identity = ask(&mut first);
    assert_eq!(ask(&mut first), identity);
    assert_eq!(ask(&mut start()), identity);
    let text = std::fs::read_to_string(&log).unwrap();
    let modes: Vec<String> = text
        .lines()
        .filter_map(|line| line.strip_prefix("release-event: "))
        .map(|json| {
            let event: aterm_json::Value = aterm_json::from_str(json).unwrap();
            event["mode"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(modes, ["hashed", "rechecked", "adopted"], "{text}");
    let mut timeline = crate::release_timeline::Timeline::default();
    timeline
        .read(crate::release_timeline::TRANSCRIPT, text.as_bytes())
        .unwrap();
    let value = timeline.value();
    assert_eq!(
        (
            value["full_hashes"].as_u64(),
            value["seal_rechecks"].as_u64()
        ),
        (Some(1), Some(2)),
        "{value}"
    );
}

/// A seal call's notes reach the release's log ([`report`]) as `inputs:` lines in their own
/// words and never as events: a take begun as its inputs were written (unsettled), a held
/// seal's recheck that finds the OS build and then the model changed, and a seal of facts unread
/// that is not kept. The timeline counts only the four takes. NEGATIVE CONTROL: let a note also
/// say a `release-event:` of mode `hashed`, and the log names, and the timeline counts, a take
/// more than were made.
#[cfg(unix)]
#[test]
fn a_seal_calls_notes_are_said_but_never_counted() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let written = input_seal::now_ns();
    let root = fixed_tree(dir.path());
    let log = dir.path().join(crate::release_timeline::TRANSCRIPT);
    let roots = vec![("inputs".to_string(), root)];
    let mut process = input_seal::Process::unstarted(SCHEMAS);
    process.arm(dir.path().join("state/inputs-seal"));
    process.begin(None, None);
    let mut ask = |facts: input_seal::Facts, began: i128| {
        process
            .identity(
                &mut input_seal::Sources {
                    roots: &mut || Ok(roots.clone()),
                    facts: &mut |_| facts.clone(),
                    open: &mut crate::durable::open_regular_file,
                    now: &mut || began,
                    stopped: &|| false,
                    report: &mut |said| report(Some(&log), said),
                },
                false,
            )
            .unwrap()
    };
    let settled = written + 3_600_000_000_000;
    let moved = input_seal::Facts {
        os_build: Some("25C56".into()),
        ..read_facts()
    };
    ask(read_facts(), written);
    ask(read_facts(), settled);
    ask(moved.clone(), settled);
    ask(
        input_seal::Facts {
            model: None,
            ..moved
        },
        settled,
    );
    let text = std::fs::read_to_string(&log).unwrap();
    let said: Vec<String> = text
        .lines()
        .map(|line| {
            if let Some(json) = line.strip_prefix("release-event: ") {
                let event: aterm_json::Value = aterm_json::from_str(json).unwrap();
                format!("event {}", event["mode"].as_str().unwrap())
            } else if line.starts_with("inputs: hashed ") {
                "take".into()
            } else {
                line.into()
            }
        })
        .collect();
    let changed = |fact: &str| {
        format!(
            "inputs: this machine's {fact} changed since the inputs were hashed; hashing them again"
        )
    };
    assert_eq!(
        said,
        [
            "take".to_string(),
            "event hashed".into(),
            "inputs: an input's status changed too recently to tell a later write by it, so this \
             seal is neither held nor kept and the next call hashes again"
                .into(),
            "take".into(),
            "event hashed".into(),
            changed("OS build (25B78 → 25C56)"),
            "take".into(),
            "event hashed".into(),
            changed("model (Mac16,1 → unread)"),
            "take".into(),
            "event hashed".into(),
            "inputs: the seal was not kept, so each child hashes for itself: this machine's OS \
             build or model could not be read"
                .into(),
        ],
        "{text}"
    );
    let mut timeline = crate::release_timeline::Timeline::default();
    timeline
        .read(crate::release_timeline::TRANSCRIPT, text.as_bytes())
        .unwrap();
    let value = timeline.value();
    assert_eq!(
        (
            value["full_hashes"].as_u64(),
            value["seal_rechecks"].as_u64()
        ),
        (Some(4), Some(0)),
        "{value}"
    );
}

/// The content walk's test vector: a fixed tree's digest under [`SCHEMA`]. A change to what
/// `atpkg::input_seal`'s walk reads into the digest or how it encodes it changes this vector,
/// and ships with a new schema and a new vector, never a new vector alone: a kept seal binds
/// the schema, not the walk's code, so under the old schema the old walk's seals would still be
/// adopted.
#[cfg(unix)]
#[test]
fn the_content_digest_of_a_fixed_tree_is_schema_v2s() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("inputs");
    for (name, bytes, mode) in [
        ("bin/trustc", "trustc", 0o755),
        ("lib/std.rlib", "std", 0o644),
        ("sdk/usr/include/stdio.h", "header", 0o444),
    ] {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    std::os::unix::fs::symlink("trustc", root.join("bin/rustc")).unwrap();
    std::os::unix::fs::symlink("missing", root.join("bin/crashlog")).unwrap();
    std::os::unix::fs::symlink(".", root.join("sdk/usr/include/self")).unwrap();
    for sub in ["", "bin", "lib", "sdk", "sdk/usr", "sdk/usr/include"] {
        std::fs::set_permissions(root.join(sub), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(
        (
            std::str::from_utf8(SCHEMA).unwrap(),
            digest_roots(&[("inputs", &root), ("again", &root.join("bin"))])
                .unwrap()
                .as_str()
        ),
        (
            "aterm-release-build-inputs-v2",
            "be778f1603d620e4117be30eca49205ee1cedfb5ed39f45a194420b35ceeedf5"
        )
    );
}
