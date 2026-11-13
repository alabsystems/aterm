// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn git(root: &Path, args: &[&str]) -> String {
    let out = crate::ledger::GitCli::new(root).git(args).unwrap();
    assert!(out.success(), "{}", out.stderr_utf8());
    out.stdout_utf8().trim().to_owned()
}

fn selected_engine(root: &Path) -> EngineInput {
    EngineInput::selected(
        root,
        &agh::Slug::parse("fixture/policy").unwrap(),
        PathBuf::from(git(root, &["rev-parse", "--absolute-git-dir"])),
        git(root, &["rev-parse", "HEAD"]),
    )
    .unwrap()
}

#[test]
fn reader_context_ignores_product_version_but_binds_configuration_and_environment() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Fixture"]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    git(root, &["commit", "--allow-empty", "-qm", "policy"]);
    let engine = selected_engine(root);
    let mut command = Command::new("fixed-verifier");
    engine.apply(&mut command);
    command
        .current_dir(root)
        .env("INPUT_CONTEXT_CONTROL", "one");
    let before = context(root, &engine, "fixed-tools", &command).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace.package]\nversion='1.0.0'\n",
    )
    .unwrap();
    assert_eq!(
        before,
        context(root, &engine, "fixed-tools", &command).unwrap()
    );
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace.package]\nversion='1.1.0'\n",
    )
    .unwrap();
    assert_eq!(
        before,
        context(root, &engine, "fixed-tools", &command).unwrap()
    );
    std::fs::create_dir(root.join(".cargo")).unwrap();
    std::fs::write(root.join(".cargo/config.toml"), "[build]\njobs=2\n").unwrap();
    assert_ne!(
        before,
        context(root, &engine, "fixed-tools", &command).unwrap()
    );
    std::fs::remove_file(root.join(".cargo/config.toml")).unwrap();
    for name in aterm_verify::fmt::CONFIG_FILES {
        std::fs::write(root.join(name), "edition='2024'\n").unwrap();
        assert_ne!(
            before,
            context(root, &engine, "fixed-tools", &command).unwrap(),
            "the formatter reads {name}"
        );
        std::fs::remove_file(root.join(name)).unwrap();
    }
    command.env("INPUT_CONTEXT_CONTROL", "two");
    assert_ne!(
        before,
        context(root, &engine, "fixed-tools", &command).unwrap()
    );
    command.env("INPUT_CONTEXT_CONTROL", "one");
    assert_ne!(
        before,
        context(root, &engine, "changed-tools", &command).unwrap()
    );
    for name in [
        "PUBLICATION_ENGINE",
        aterm_verify::engine::GIT_DIR_ENV,
        aterm_verify::engine::COMMIT_ENV,
    ] {
        command.env(name, "forged");
        assert!(
            context(root, &engine, "fixed-tools", &command).is_err(),
            "{name}"
        );
        engine.apply(&mut command);
    }
    let ordinary = |command: &Command| {
        crate::check_content::context(
            "fixed-driver",
            "fixed-tools",
            command,
            crate::check_content::ReleaseHalf::Tree,
            crate::check_content::Admission::WaitDefault,
        )
    };
    let ordinary_before = ordinary(&command);
    let relocated = EngineInput::selected(
        &root.join("owned-copy-not-yet-created"),
        &agh::Slug::parse("fixture/policy").unwrap(),
        engine.git_dir.clone(),
        engine.commit.clone(),
    )
    .unwrap();
    relocated.apply(&mut command);
    assert_eq!(
        before,
        context(root, &relocated, "fixed-tools", &command).unwrap()
    );
    assert_ne!(
        ordinary_before,
        ordinary(&command),
        "ordinary qualification keeps raw locator inputs"
    );
    let other_authority = EngineInput::selected(
        root,
        &agh::Slug::parse("fixture/other-policy").unwrap(),
        engine.git_dir.clone(),
        engine.commit.clone(),
    )
    .unwrap();
    other_authority.apply(&mut command);
    assert_ne!(
        before,
        context(root, &other_authority, "fixed-tools", &command).unwrap()
    );
    let local = EngineInput::local_fixture(root).unwrap();
    local.apply(&mut command);
    let local_context = context(root, &local, "fixed-tools", &command).unwrap();
    assert_ne!(
        before, local_context,
        "a local fixture is not a repository policy receipt"
    );
    let relocated_local = EngineInput::new(
        &root.join("fixture-copy"),
        EnginePolicySource::LocalFixture,
        local.git_dir.clone(),
        local.commit.clone(),
    )
    .unwrap();
    relocated_local.apply(&mut command);
    assert_eq!(
        local_context,
        context(root, &relocated_local, "fixed-tools", &command).unwrap()
    );
    assert!(
        EngineInput::new(
            root,
            EnginePolicySource::LocalFixture,
            local.git_dir.clone(),
            "f".repeat(40)
        )
        .is_err()
    );
    std::fs::write(root.join("policy-data"), "new policy\n").unwrap();
    git(root, &["add", "policy-data"]);
    git(root, &["commit", "-qm", "changed fixture policy"]);
    let changed_local = EngineInput::local_fixture(root).unwrap();
    changed_local.apply(&mut command);
    assert_ne!(
        local_context,
        context(root, &changed_local, "fixed-tools", &command).unwrap()
    );
    local.apply(&mut command);
    assert_eq!(
        local_context,
        context(root, &local, "fixed-tools", &command).unwrap()
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
    let library = dir.path().join("library");
    std::fs::create_dir(&library).unwrap();
    git(&library, &["init", "-q"]);
    git(&library, &["config", "user.name", "Fixture"]);
    git(
        &library,
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::write(library.join("lib"), "first").unwrap();
    git(&library, &["add", "."]);
    git(&library, &["commit", "-qm", "first library"]);
    let first_pin = git(&library, &["rev-parse", "HEAD"]);
    let old_remote = dir.path().join("old-remote");
    git(
        &source,
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--bare",
            library.to_str().unwrap(),
            old_remote.to_str().unwrap(),
        ],
    );
    git(
        &source,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            library.to_str().unwrap(),
            "vendor/astream",
        ],
    );
    git(&source, &["config", "submodule.recurse", "true"]);
    std::fs::write(source.join("input"), "first").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "first"]);
    let first = git(&source, &["rev-parse", "HEAD"]);
    let owned = directory(dir.path());
    let runner = crate::ledger::GitCli::new(&source);
    let mut hydrate = |own: &crate::ledger::GitCli| {
        git_run(
            own,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "--init",
                "--checkout",
                "vendor/astream",
            ],
        )
    };
    checkout_with(&runner, &owned, &first, |own| {
        hydrate(own)?;
        let module = owned.join(INPUT_SUBMODULE);
        // An interrupted initial clone can already name the requested commit while
        // its index and files are still absent. Git's next update reports success.
        git(&module, &["read-tree", "--empty"]);
        std::fs::remove_file(module.join("lib")).unwrap();
        hydrate(own)?;
        assert!(git(&module, &["ls-files", "--stage"]).is_empty());
        assert!(empty_directory(&module, true).unwrap());
        Ok(())
    })
    .unwrap();
    assert!(aterm_tempfile::is_quiet(&owned));
    let owned_library = owned.join("vendor/astream");
    assert_eq!(git(&owned_library, &["rev-parse", "HEAD"]), first_pin);
    assert_eq!(
        git(&owned, &["config", "--get", "submodule.recurse"]),
        "true"
    );
    // The kept submodule predates B. A real failed fetch leaves the new parent and old child.
    std::fs::write(library.join("lib"), "second").unwrap();
    git(&library, &["commit", "-qam", "second library"]);
    let second_pin = git(&library, &["rev-parse", "HEAD"]);
    let source_library = source.join("vendor/astream");
    git(
        &source_library,
        &["-c", "protocol.file.allow=always", "fetch", "-q", "origin"],
    );
    git(&source_library, &["checkout", "-q", &second_pin]);
    std::fs::write(source.join("input"), "second").unwrap();
    git(&source, &["commit", "-qam", "second"]);
    let second = git(&source, &["rev-parse", "HEAD"]);
    assert!(
        !crate::ledger::GitCli::new(&owned_library)
            .git(&["cat-file", "-e", &second_pin])
            .unwrap()
            .success(),
        "the kept worktree has not fetched the next submodule pin"
    );
    let outer_note = owned.join("untracked-note");
    std::fs::write(&outer_note, "keep me").unwrap();
    assert!(
        checkout_with(&runner, &owned, &second, |own| {
            git_run(
                own,
                &[
                    "-c",
                    "protocol.file.allow=never",
                    "submodule",
                    "update",
                    "--init",
                    "--checkout",
                    "vendor/astream",
                ],
            )
        })
        .is_err()
    );
    assert_eq!(git(&owned, &["rev-parse", "HEAD"]), second);
    assert_eq!(
        std::fs::read_to_string(owned.join("input")).unwrap(),
        "second"
    );
    assert_eq!(git(&owned_library, &["rev-parse", "HEAD"]), first_pin);
    assert!(pending_path(&owned).is_file());
    std::fs::write(owned_library.join("lib"), "keep nested edit").unwrap();
    assert!(checkout_with(&runner, &owned, &second, &mut hydrate).is_err());
    assert_eq!(
        std::fs::read_to_string(owned_library.join("lib")).unwrap(),
        "keep nested edit"
    );
    std::fs::write(owned_library.join("lib"), "first").unwrap();
    std::fs::write(owned_library.join("untracked"), "keep nested file").unwrap();
    assert!(checkout_with(&runner, &owned, &second, &mut hydrate).is_err());
    assert_eq!(
        std::fs::read_to_string(owned_library.join("untracked")).unwrap(),
        "keep nested file"
    );
    std::fs::remove_file(owned_library.join("untracked")).unwrap();
    git(&owned_library, &["config", "user.name", "Fixture"]);
    git(
        &owned_library,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(
        &owned_library,
        &["commit", "--allow-empty", "-qm", "unexpected child HEAD"],
    );
    let unexpected = git(&owned_library, &["rev-parse", "HEAD"]);
    assert!(checkout_with(&runner, &owned, &second, &mut hydrate).is_err());
    assert_eq!(git(&owned_library, &["rev-parse", "HEAD"]), unexpected);
    git(&owned_library, &["checkout", "--detach", &first_pin]);
    git(&owned, &["add", "vendor/astream"]);
    assert!(checkout_with(&runner, &owned, &second, &mut hydrate).is_err());
    git(&owned, &["restore", "--staged", "vendor/astream"]);
    checkout_with(&runner, &owned, &second, &mut hydrate).unwrap();
    assert!(!pending_path(&owned).exists());
    assert_eq!(git(&owned_library, &["rev-parse", "HEAD"]), second_pin);
    assert_eq!(
        std::fs::read_to_string(owned_library.join("lib")).unwrap(),
        "second"
    );
    assert_eq!(std::fs::read_to_string(&outer_note).unwrap(), "keep me");
    // Settle an already admitted transition before following a newer selection.
    assert!(
        checkout_with(&runner, &owned, &first, |_| Err(
            "interrupted hydration".into()
        ))
        .is_err()
    );
    let mut settled = Vec::new();
    checkout_with(&runner, &owned, &second, |own| {
        settled.push(
            git_read(own, &["rev-parse", "HEAD"])
                .unwrap()
                .trim()
                .to_owned(),
        );
        hydrate(own)
    })
    .unwrap();
    assert_eq!(settled, [first.clone(), second.clone()]);

    // Initial cloning can succeed before fetching the requested pin fails. Only its empty
    // --no-checkout skeleton is resumable, not a materialized default-branch checkout.
    git(
        &source,
        &[
            "config",
            "submodule.vendor/astream.url",
            old_remote.to_str().unwrap(),
        ],
    );
    let fresh = directory(&dir.path().join("fresh"));
    assert!(checkout_with(&runner, &fresh, &second, &mut hydrate).is_err());
    let fresh_library = fresh.join("vendor/astream");
    assert_eq!(git(&fresh_library, &["rev-parse", "HEAD"]), first_pin);
    let default_branch = git(&fresh_library, &["symbolic-ref", "HEAD"]);
    assert!(git(&fresh_library, &["ls-files", "--stage"]).is_empty());
    assert!(empty_directory(&fresh_library, true).unwrap());
    let module = child_gitdir(&crate::ledger::GitCli::new(&fresh)).unwrap();
    let held_clone = fresh.with_file_name("held-clone");
    std::fs::rename(&fresh_library, &held_clone).unwrap();
    git(
        &fresh,
        &[
            "--git-dir",
            module.to_str().unwrap(),
            "--work-tree",
            fresh.to_str().unwrap(),
            "config",
            "remote.origin.url",
            "different-origin",
        ],
    );
    assert!(checkout_with(&runner, &fresh, &second, &mut hydrate).is_err());
    git(
        &fresh,
        &[
            "--git-dir",
            module.to_str().unwrap(),
            "--work-tree",
            fresh.to_str().unwrap(),
            "config",
            "remote.origin.url",
            old_remote.to_str().unwrap(),
        ],
    );
    std::fs::rename(&held_clone, &fresh_library).unwrap();
    git(&fresh_library, &["checkout", "--detach", &first_pin]);
    assert!(checkout_with(&runner, &fresh, &second, &mut hydrate).is_err());
    assert_eq!(
        std::fs::read_to_string(fresh_library.join("lib")).unwrap(),
        "first"
    );
    git(&fresh_library, &["read-tree", "--empty"]);
    std::fs::remove_file(fresh_library.join("lib")).unwrap();
    git(&fresh_library, &["symbolic-ref", "HEAD", &default_branch]);
    git(
        &old_remote,
        &[
            "-c",
            "protocol.file.allow=always",
            "fetch",
            library.to_str().unwrap(),
            &format!("{second_pin}:refs/heads/ready"),
        ],
    );
    checkout_with(&runner, &fresh, &second, &mut hydrate).unwrap();
    assert_eq!(git(&fresh_library, &["rev-parse", "HEAD"]), second_pin);
    assert_eq!(
        git(&fresh_library, &["rev-parse", &default_branch]),
        first_pin
    );
    assert!(!pending_path(&fresh).exists());
    git(
        &source,
        &[
            "config",
            "submodule.vendor/astream.url",
            library.to_str().unwrap(),
        ],
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
    let ordinary = dir.path().join("ordinary");
    std::fs::create_dir(&ordinary).unwrap();
    git(&ordinary, &["init", "-q"]);
    git(&ordinary, &["config", "user.name", "Fixture"]);
    git(
        &ordinary,
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::create_dir_all(ordinary.join(INPUT_SUBMODULE)).unwrap();
    std::fs::write(
        ordinary.join(INPUT_SUBMODULE).join("file"),
        "tracked directory",
    )
    .unwrap();
    git(&ordinary, &["add", "."]);
    git(&ordinary, &["commit", "-qm", "ordinary tracked directory"]);
    let ordinary_tip = git(&ordinary, &["rev-parse", "HEAD"]);
    let ordinary_input = directory(&dir.path().join("ordinary-input"));
    let ordinary_git = crate::ledger::GitCli::new(&ordinary);
    checkout(&ordinary_git, &ordinary_input, &ordinary_tip).unwrap();
    std::fs::write(
        ordinary_input.join(INPUT_SUBMODULE).join("file"),
        "keep tracked edit",
    )
    .unwrap();
    assert!(checkout(&ordinary_git, &ordinary_input, &ordinary_tip).is_err());
    assert_eq!(
        std::fs::read_to_string(ordinary_input.join(INPUT_SUBMODULE).join("file")).unwrap(),
        "keep tracked edit"
    );
    git(
        &source,
        &["worktree", "remove", "--force", owned.to_str().unwrap()],
    );
    git(
        &source,
        &["worktree", "remove", "--force", fresh.to_str().unwrap()],
    );
    git(
        &ordinary,
        &[
            "worktree",
            "remove",
            "--force",
            ordinary_input.to_str().unwrap(),
        ],
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
    let engine = selected_engine(root);
    let mut command = aterm_uds::spawnfd::command("fixture-verifier");
    engine.apply(&mut command);
    command.current_dir(root);
    detach_launcher(&mut command);
    let key = |command: &Command, seal: &str| {
        identity(&runner, root, &engine, &tip, &base, seal, command).unwrap()
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
    let explanation = describe(&command, "tools-one", &engine).unwrap();
    assert!(explanation.contains("env:INPUT_FIXTURE_FLAG"));
    assert!(!explanation.contains("changed"));
    assert!(!explanation.contains("CODEX_TOOL_CALL_ID"));
    let before_source = key(&command, "tools-one");
    std::fs::write(root.join("source-0.txt"), "changed\n").unwrap();
    assert!(identity(&runner, root, &engine, &tip, &base, "tools-one", &command).is_err());
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "changed source"]);
    let changed = git(root, &["rev-parse", "HEAD"]);
    assert_ne!(
        before_source.1,
        identity(
            &runner,
            root,
            &engine,
            &changed,
            &base,
            "tools-one",
            &command
        )
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
    assert!(
        identity(
            &runner,
            root,
            &engine,
            &changed,
            &base,
            "tools-one",
            &command
        )
        .is_err()
    );
    println!(
        "release-input key fixture: 68 commits, 377 paths; repeated_and_invalidated_s={:.3}",
        start.elapsed().as_secs_f64()
    );
}
