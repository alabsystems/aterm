// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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
