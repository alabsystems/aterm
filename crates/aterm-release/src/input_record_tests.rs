// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn git(root: &Path, args: &[&str]) -> String {
    let out = crate::ledger::GitCli::new(root).git(args).unwrap();
    assert!(out.success(), "{}", out.stderr_utf8());
    out.stdout_utf8().trim().to_owned()
}

#[test]
fn reader_context_ignores_product_version_but_binds_configuration_and_environment() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Fixture"]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    git(root, &["commit", "--allow-empty", "-qm", "policy"]);
    let mut command = Command::new("fixed-verifier");
    command
        .current_dir(root)
        .env("INPUT_CONTEXT_CONTROL", "one");
    let before = context(root, root, "fixed-tools", &command).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace.package]\nversion='1.0.0'\n",
    )
    .unwrap();
    assert_eq!(
        before,
        context(root, root, "fixed-tools", &command).unwrap()
    );
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace.package]\nversion='1.1.0'\n",
    )
    .unwrap();
    assert_eq!(
        before,
        context(root, root, "fixed-tools", &command).unwrap()
    );
    std::fs::create_dir(root.join(".cargo")).unwrap();
    std::fs::write(root.join(".cargo/config.toml"), "[build]\njobs=2\n").unwrap();
    assert_ne!(
        before,
        context(root, root, "fixed-tools", &command).unwrap()
    );
    std::fs::remove_file(root.join(".cargo/config.toml")).unwrap();
    for name in aterm_verify::fmt::CONFIG_FILES {
        std::fs::write(root.join(name), "edition='2024'\n").unwrap();
        assert_ne!(
            before,
            context(root, root, "fixed-tools", &command).unwrap(),
            "the formatter reads {name}"
        );
        std::fs::remove_file(root.join(name)).unwrap();
    }
    command.env("INPUT_CONTEXT_CONTROL", "two");
    assert_ne!(
        before,
        context(root, root, "fixed-tools", &command).unwrap()
    );
    command.env("INPUT_CONTEXT_CONTROL", "one");
    assert_ne!(
        before,
        context(root, root, "changed-tools", &command).unwrap()
    );
}

#[test]
fn one_writer_per_input_key_without_blocking_other_keys() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let other = dir.path().join("other-key");
    let held = claim(dir.path()).unwrap();
    assert!(claim(dir.path()).is_err());
    let independent = claim(&other).unwrap();
    assert!(other.join("input.lock").is_file());
    assert!(claim(&other).is_err());
    drop(held);
    let resumed = claim(dir.path()).unwrap();
    drop((independent, resumed));
}

#[test]
fn stable_checkout_moves_clean_tips_but_preserves_unexpected_work() {
    let dir = aterm_tempfile::Builder::new().heard().tempdir().unwrap();
    assert!(!aterm_tempfile::is_quiet(dir.path()));
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q"]);
    git(&source, &["config", "user.name", "Fixture"]);
    git(
        &source,
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::write(source.join("input"), "first").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "first"]);
    let first = git(&source, &["rev-parse", "HEAD"]);
    let owned = directory(dir.path());
    let runner = crate::ledger::GitCli::new(&source);
    checkout(&runner, &owned, &first).unwrap();
    assert!(aterm_tempfile::is_quiet(&owned));
    std::fs::write(source.join("input"), "second").unwrap();
    git(&source, &["commit", "-qam", "second"]);
    let second = git(&source, &["rev-parse", "HEAD"]);
    checkout(&runner, &owned, &second).unwrap();
    assert_eq!(
        std::fs::read_to_string(owned.join("input")).unwrap(),
        "second"
    );
    std::fs::write(owned.join("input"), "unexpected work").unwrap();
    assert!(checkout(&runner, &owned, &first).is_err());
    assert_eq!(
        std::fs::read_to_string(owned.join("input")).unwrap(),
        "unexpected work"
    );
    let foreign = dir.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    assert!(checkout(&runner, &foreign, &first).is_err());
    git(
        &source,
        &["worktree", "remove", "--force", owned.to_str().unwrap()],
    );
}

#[test]
fn sixty_eight_commits_and_377_paths_bind_tree_cone_tools_and_environment() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Fixture"]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    git(root, &["commit", "--allow-empty", "-qm", "base"]);
    let base = git(root, &["rev-parse", "HEAD"]);
    for commit in 0..68 {
        for index in (commit..377).step_by(68) {
            std::fs::write(root.join(format!("source-{index}.txt")), "source\n").unwrap();
        }
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "unreceipted"]);
    }
    let tip = git(root, &["rev-parse", "HEAD"]);
    let input = aterm_verify::release_input::discover(
        root,
        &std::env::var_os("PATH").unwrap_or_default(),
        &base,
        &tip,
    )
    .unwrap();
    assert_eq!((input.commits.len(), input.paths.len()), (68, 377));
    let runner = crate::ledger::GitCli::new(root);
    let mut command = aterm_uds::spawnfd::command("fixture-verifier");
    command.current_dir(root);
    detach_launcher(&mut command);
    let key = |command: &Command, seal: &str| {
        identity(&runner, root, root, &tip, &base, seal, command).unwrap()
    };
    let start = std::time::Instant::now();
    let first = key(&command, "tools-one");
    assert_eq!(first, key(&command, "tools-one"));
    command.env("TRUST_TARGO_NESTED_UNVERIFIED_ENDPOINT", "first-invocation");
    command.env("CODEX_TOOL_CALL_ID", "first-call");
    detach_launcher(&mut command);
    let detached = key(&command, "tools-one");
    assert_eq!(first, detached);
    command.env("TRUST_TARGO_NESTED_UNVERIFIED_ENDPOINT", "next-invocation");
    command.env("CODEX_TOOL_CALL_ID", "next-call");
    detach_launcher(&mut command);
    assert_eq!(detached, key(&command, "tools-one"));
    assert_ne!(first, key(&command, "tools-two"));
    command.env("INPUT_FIXTURE_FLAG", "changed");
    assert_ne!(first, key(&command, "tools-one"));
    let explanation = describe(&command, "tools-one").unwrap();
    assert!(explanation.contains("env:INPUT_FIXTURE_FLAG"));
    assert!(!explanation.contains("changed"));
    assert!(!explanation.contains("CODEX_TOOL_CALL_ID"));
    let before_source = key(&command, "tools-one");
    std::fs::write(root.join("source-0.txt"), "changed\n").unwrap();
    assert!(identity(&runner, root, root, &tip, &base, "tools-one", &command).is_err());
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "changed source"]);
    let changed = git(root, &["rev-parse", "HEAD"]);
    assert_ne!(
        before_source.1,
        identity(&runner, root, root, &changed, &base, "tools-one", &command)
            .unwrap()
            .1
    );
    git(
        root,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "same tree, different history",
        ],
    );
    assert!(identity(&runner, root, root, &changed, &base, "tools-one", &command).is_err());
    println!(
        "release-input key fixture: 68 commits, 377 paths; repeated_and_invalidated_s={:.3}",
        start.elapsed().as_secs_f64()
    );
}
