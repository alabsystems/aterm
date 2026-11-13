// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{checkout_event_roots, file_events_duration};
#[test]
fn explicit_census_is_bounded_and_other_doctor_forms_are_untouched() {
    let parse = |args: &[&str]| {
        file_events_duration(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    };
    assert_eq!(
        parse(&["--file-events", "60"]),
        Some(Ok(std::time::Duration::from_secs(60)))
    );
    assert_eq!(parse(&["--read-only"]), None);
    for args in [
        vec!["--file-events"],
        vec!["--file-events", "0"],
        vec!["--file-events", "3601"],
        vec!["--file-events", "1", "--apply"],
    ] {
        assert!(parse(&args).unwrap().is_err());
    }
}

#[test]
fn checkout_sources_include_real_clone_and_linked_worktree_caches_but_not_foreign_roots() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let source = scratch.path().join("source");
    std::fs::create_dir_all(source.join("crates/aterm-verify")).unwrap();
    std::fs::write(source.join("crates/aterm-verify/Cargo.toml"), b"").unwrap();
    let source = source.canonicalize().unwrap();
    let git = |args: &[&str]| {
        let mut command = aterm_uds::spawnfd::command("git");
        command.current_dir(&source).args(args);
        let output =
            aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "fixture",
    ]);
    let linked = scratch.path().join("linked");
    git(&[
        "worktree",
        "add",
        "--detach",
        linked.to_str().unwrap(),
        "HEAD",
    ]);
    let linked = linked.canonicalize().unwrap();
    let expected = |root: &std::path::Path| {
        vec![
            root.join("target"),
            source.join(".git/aterm-release"),
            source.join(".git/refs/agh/aterm-release"),
        ]
    };
    assert_eq!(checkout_event_roots(&source), expected(&source));
    assert_eq!(checkout_event_roots(&linked), expected(&linked));
    assert!(checkout_event_roots(scratch.path()).is_empty());
    // Negative control: a malformed Git pointer must not select arbitrary cache paths.
    std::fs::write(linked.join(".git"), b"not a Git pointer\n").unwrap();
    assert_eq!(checkout_event_roots(&linked), vec![linked.join("target")]);
}
