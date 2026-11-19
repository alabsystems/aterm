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
fn source_reader_items_follow_pinned_policy_bytes_and_full_records_follow_the_commit() {
    let source = aterm_tempfile::tempdir().unwrap();
    let policy = aterm_tempfile::tempdir().unwrap();
    let root = source.path();
    let repo = policy.path();
    for dir in [root, repo] {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.name", "Fixture"]);
        git(dir, &["config", "user.email", "fixture@example.invalid"]);
    }
    git(root, &["commit", "--allow-empty", "-qm", "source base"]);
    let base = git(root, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("source.txt"), "source\n").unwrap();
    git(root, &["add", "source.txt"]);
    git(root, &["commit", "-qm", "unreceipted source"]);
    let tip = git(root, &["rev-parse", "HEAD"]);
    let command = |engine: &EngineInput| {
        let mut command = Command::new("fixture-verifier");
        engine.apply(&mut command);
        command.current_dir(root);
        command
    };
    let reader = |engine: &EngineInput| context(root, engine, "tools", &command(engine)).unwrap();
    let judgment = |engine: &EngineInput| {
        identity(
            &crate::ledger::GitCli::new(root),
            root,
            engine,
            &tip,
            &base,
            "tools",
            &command(engine),
        )
        .unwrap()
    };
    let baseline = repo.join(aterm_census::export_scan::rows::BASELINE);
    let registry = repo.join(aterm_verify::export_scan::REGISTRY);
    for path in [&baseline, &registry] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "original\n").unwrap();
    }
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "pinned policy"]);
    let first = selected_engine(repo);
    let items = reader(&first);
    let mut ctx = aterm_verify::Ctx::new(
        root.to_owned(),
        aterm_verify::Scope::workspace(),
        aterm_verify::EnvSnapshot::default(),
        root.join("scratch"),
    );
    ctx.carry_context = Some(items.clone());
    let receipt = aterm_verify::input_content::Proof::new(
        &ctx,
        "export-scan",
        "source selection",
        vec![aterm_census::inputs::file(root, "source.txt").unwrap()],
    )
    .unwrap();
    ctx.input_items.push(receipt.clone());
    std::fs::write(repo.join("plan.md"), "new plan\n").unwrap();
    git(repo, &["add", "plan.md"]);
    git(repo, &["commit", "-qm", "plan update"]);
    let after = selected_engine(repo);
    let after_plan = reader(&after);
    assert_eq!(items, after_plan);
    assert_ne!(judgment(&first), judgment(&after));
    ctx.carry_context = Some(after_plan);
    assert!(aterm_verify::input_content::exact(&ctx, &receipt));
    for path in [&baseline, &registry] {
        std::fs::write(path, "revised\n").unwrap();
        git(repo, &["commit", "-qam", "revise policy input"]);
        ctx.carry_context = Some(reader(&selected_engine(repo)));
        assert!(!aterm_verify::input_content::exact(&ctx, &receipt));
        std::fs::write(path, "original\n").unwrap();
        git(repo, &["commit", "-qam", "restore policy input"]);
    }
    std::fs::remove_file(&registry).unwrap();
    git(repo, &["commit", "-qam", "incomplete policy"]);
    let missing = reader(&selected_engine(repo));
    assert_ne!(items, missing);
    git(
        repo,
        &["commit", "--allow-empty", "-qm", "still incomplete"],
    );
    assert_ne!(missing, reader(&selected_engine(repo)));
    assert_eq!(reader(&first), items);
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

/// A repository with a committer, as these fixtures make them.
fn repository(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q"]);
    git(path, &["config", "user.name", "Fixture"]);
    git(path, &["config", "user.email", "fixture@example.invalid"]);
}

/// Write each `(path, content)` (`None` removes it) in `repo`, commit everything, and name the
/// commit.
fn commit(repo: &Path, files: &[(&str, Option<&str>)]) -> String {
    for (path, content) in files {
        let at = repo.join(path);
        match content {
            Some(content) => {
                std::fs::create_dir_all(at.parent().unwrap()).unwrap();
                std::fs::write(at, content).unwrap();
            }
            None => std::fs::remove_file(at).unwrap(),
        }
    }
    git(repo, &["add", "-A", "."]);
    git(repo, &["commit", "-qm", "fixture"]);
    git(repo, &["rev-parse", "HEAD"])
}

/// Git's configuration that cuts a checkout short: when it writes `path`, its smudge filter
/// leaves `dir`'s `cut` marker and sends that git `signal`, and git dies having written the files
/// it sorts before `path`, removed `path`'s old file and written neither its index nor HEAD.
/// `INT` is a person's ^C, on which Git removes its index lock; `KILL` is a Git killed outright,
/// as by a power loss, which leaves that lock.
fn cut_at(dir: &Path, path: &str, signal: &str) -> [(&'static str, String); 2] {
    let attributes = dir.join("attributes");
    std::fs::write(&attributes, format!("{path} filter=cut\n")).unwrap();
    [
        ("core.attributesFile", attributes.display().to_string()),
        (
            "filter.cut.smudge",
            format!(
                "touch '{}'; kill -{signal} $PPID; cat",
                dir.join("cut").display()
            ),
        ),
    ]
}

/// The absolute path of Git's lock on the index of the checkout at `top`.
fn index_lock(top: &Path) -> PathBuf {
    PathBuf::from(git(
        top,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "index.lock",
        ],
    ))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// AN INTERRUPTED CHECKOUT OF THE INPUT CHECKOUT IS ITS OWN RESIDUE, NOT A PERSON'S EDITS. A
/// ^C cuts the move from B to T short: HEAD and the index stay at B over some of T's files, and
/// the file Git was writing is gone. The retry recognises that from its receipt and carries it
/// to T, clean, with nothing forced.
/// NEGATIVE CONTROLS: a byte beside that residue that is neither B's nor T's, on a path the two
/// trees disagree on or on one they share, is refused and kept, with HEAD still at B, and so is
/// a staged edit of a path they share.
#[test]
fn an_interrupted_checkout_resumes_to_its_target_and_edits_beside_it_are_kept() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    repository(&source);
    let before = commit(
        &source,
        &[
            ("a", Some("a1")),
            ("b", Some("b1")),
            ("d/x", Some("d1")),
            ("gone", Some("g")),
            ("z", Some("z1")),
        ],
    );
    let target = commit(
        &source,
        &[
            ("a", Some("a2")),
            ("d/x", Some("d2")),
            ("gone", None),
            ("new", Some("n")),
            ("z", Some("z2")),
            ("zz", Some("zz2")),
        ],
    );
    let owned = directory(dir.path());
    let runner = crate::ledger::GitCli::new(&source);
    let no_child = |_: &crate::ledger::GitCli| -> Result<(), String> {
        unreachable!("the fixture has no input submodule")
    };
    checkout_with(&runner, &owned, &before, no_child).unwrap();
    for (key, value) in cut_at(dir.path(), "z", "INT") {
        git(&source, &["config", key, &value]);
    }
    let cut = checkout_with(&runner, &owned, &target, no_child);
    assert!(cut.is_err() && dir.path().join("cut").exists(), "{cut:?}");
    git(&source, &["config", "--unset", "core.attributesFile"]);
    git(&source, &["config", "--unset", "filter.cut.smudge"]);
    assert_eq!(git(&owned, &["rev-parse", "HEAD"]), before);
    assert!(pending_path(&owned).is_file());
    assert_eq!(read(&owned.join("a")), "a2", "the checkout wrote some of T");
    assert!(
        !owned.join("z").exists() && !owned.join("zz").exists(),
        "and was cut short writing z, which it removed first"
    );

    for (path, edit, kept) in [("a", "a person's a", "a2"), ("b", "a person's b", "b1")] {
        std::fs::write(owned.join(path), edit).unwrap();
        let refused = checkout_with(&runner, &owned, &target, no_child).unwrap_err();
        assert!(
            refused.contains("tracked edits; no work was discarded"),
            "{refused}"
        );
        assert_eq!(read(&owned.join(path)), edit);
        assert_eq!(git(&owned, &["rev-parse", "HEAD"]), before);
        std::fs::write(owned.join(path), kept).unwrap();
    }
    // A person's staged edit of a path both trees hold alike: Git's checkout would carry it to
    // T untouched, so only the index shows it. It is refused at B, staged as it was.
    std::fs::write(owned.join("b"), "a person's staged b").unwrap();
    git(&owned, &["add", "b"]);
    let refused = checkout_with(&runner, &owned, &target, no_child).unwrap_err();
    assert!(
        refused.contains("tracked edits; no work was discarded"),
        "{refused}"
    );
    assert_eq!(git(&owned, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&owned, &["show", ":b"]), "a person's staged b");
    std::fs::write(owned.join("b"), "b1").unwrap();
    git(&owned, &["add", "b"]);

    checkout_with(&runner, &owned, &target, no_child).unwrap();
    assert_eq!(git(&owned, &["rev-parse", "HEAD"]), target);
    assert!(!pending_path(&owned).exists());
    assert_eq!(git(&owned, &["status", "--porcelain", "--ignored"]), "");
    for (path, content) in [
        ("a", "a2"),
        ("b", "b1"),
        ("d/x", "d2"),
        ("new", "n"),
        ("z", "z2"),
        ("zz", "zz2"),
    ] {
        assert_eq!(read(&owned.join(path)), content, "{path}");
    }
    assert!(!owned.join("gone").exists());
    git(
        &source,
        &["worktree", "remove", "--force", owned.to_str().unwrap()],
    );
}

/// A CHECKOUT CUT SHORT AMONG FILES NEW TO ITS TARGET RESUMES TOO. The move from B to T wrote
/// `n1`, which B does not hold, and was cut writing `n2`: HEAD and the index are B's, nothing
/// tracked differs, and `n1` is a file Git's checkout of T would refuse to overwrite. The retry
/// recognises it from its receipt as that checkout's own and carries it to T.
/// NEGATIVE CONTROL: bytes at `n1` that are not T's are refused by Git's checkout, which names
/// the file, and kept, with HEAD still at B.
#[test]
fn a_checkout_cut_short_among_files_new_to_its_target_resumes() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    repository(&source);
    let before = commit(&source, &[("a", Some("a1"))]);
    let target = commit(&source, &[("n1", Some("n1")), ("n2", Some("n2"))]);
    let owned = directory(dir.path());
    let runner = crate::ledger::GitCli::new(&source);
    let no_child = |_: &crate::ledger::GitCli| -> Result<(), String> {
        unreachable!("the fixture has no input submodule")
    };
    checkout_with(&runner, &owned, &before, no_child).unwrap();
    for (key, value) in cut_at(dir.path(), "n2", "INT") {
        git(&source, &["config", key, &value]);
    }
    let cut = checkout_with(&runner, &owned, &target, no_child);
    assert!(cut.is_err() && dir.path().join("cut").exists(), "{cut:?}");
    git(&source, &["config", "--unset", "core.attributesFile"]);
    git(&source, &["config", "--unset", "filter.cut.smudge"]);
    assert_eq!(git(&owned, &["rev-parse", "HEAD"]), before);
    assert!(pending_path(&owned).is_file());
    assert_eq!(read(&owned.join("n1")), "n1", "the checkout wrote n1");
    assert!(!owned.join("n2").exists(), "and was cut short writing n2");
    assert_eq!(git(&owned, &["status", "--porcelain"]), "?? n1");

    std::fs::write(owned.join("n1"), "a person's n1").unwrap();
    let refused = checkout_with(&runner, &owned, &target, no_child).unwrap_err();
    assert!(
        refused.contains("untracked working tree files would be overwritten")
            && refused.contains("n1"),
        "{refused}"
    );
    assert_eq!(read(&owned.join("n1")), "a person's n1");
    assert_eq!(git(&owned, &["rev-parse", "HEAD"]), before);
    std::fs::write(owned.join("n1"), "n1").unwrap();

    checkout_with(&runner, &owned, &target, no_child).unwrap();
    assert_eq!(git(&owned, &["rev-parse", "HEAD"]), target);
    assert!(!pending_path(&owned).exists());
    assert_eq!(git(&owned, &["status", "--porcelain", "--ignored"]), "");
    for (path, content) in [("a", "a1"), ("n1", "n1"), ("n2", "n2")] {
        assert_eq!(read(&owned.join(path)), content, "{path}");
    }
    git(
        &source,
        &["worktree", "remove", "--force", owned.to_str().unwrap()],
    );
}

/// A CHECKOUT KILLED OUTRIGHT NAMES THE LOCK IT LEFT, AND RESUMES ONCE A PERSON REMOVES IT. Git
/// killed (`SIGKILL`, as by a power loss) while it moved B to T leaves its index lock as well as
/// the residue a ^C leaves. Every retry refuses naming that lock and the act that clears it, with
/// HEAD, the index and the files as they were; once the lock is gone the retry carries the
/// residue to T, clean.
/// NEGATIVE CONTROL: the retry before the lock's removal changes nothing a later retry relies on,
/// so the same receipt then completes.
#[test]
fn a_checkout_killed_outright_names_its_lock_and_resumes_once_it_is_removed() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    repository(&source);
    let before = commit(&source, &[("a", Some("a1")), ("z", Some("z1"))]);
    let target = commit(
        &source,
        &[("a", Some("a2")), ("n", Some("n")), ("z", Some("z2"))],
    );
    let owned = directory(dir.path());
    let runner = crate::ledger::GitCli::new(&source);
    let no_child = |_: &crate::ledger::GitCli| -> Result<(), String> {
        unreachable!("the fixture has no input submodule")
    };
    checkout_with(&runner, &owned, &before, no_child).unwrap();
    for (key, value) in cut_at(dir.path(), "z", "KILL") {
        git(&source, &["config", key, &value]);
    }
    let cut = checkout_with(&runner, &owned, &target, no_child);
    assert!(cut.is_err() && dir.path().join("cut").exists(), "{cut:?}");
    git(&source, &["config", "--unset", "core.attributesFile"]);
    git(&source, &["config", "--unset", "filter.cut.smudge"]);
    let lock = index_lock(&owned);
    assert!(lock.is_file(), "the killed Git left its lock");
    assert_eq!(read(&owned.join("a")), "a2", "the checkout wrote some of T");
    assert!(!owned.join("z").exists());

    for _ in 0..2 {
        let refused = checkout_with(&runner, &owned, &target, no_child).unwrap_err();
        assert!(
            refused.contains(&format!("remove {}", lock.display()))
                && refused.contains("still running, or one was killed outright"),
            "{refused}"
        );
        assert_eq!(git(&owned, &["rev-parse", "HEAD"]), before);
        assert_eq!(git(&owned, &["show", ":a"]), "a1", "the index is B's");
        assert_eq!(read(&owned.join("a")), "a2");
        assert!(lock.is_file() && pending_path(&owned).is_file());
    }

    std::fs::remove_file(&lock).unwrap();
    checkout_with(&runner, &owned, &target, no_child).unwrap();
    assert_eq!(git(&owned, &["rev-parse", "HEAD"]), target);
    assert!(!pending_path(&owned).exists());
    assert_eq!(git(&owned, &["status", "--porcelain", "--ignored"]), "");
    for (path, content) in [("a", "a2"), ("n", "n"), ("z", "z2")] {
        assert_eq!(read(&owned.join(path)), content, "{path}");
    }
    git(
        &source,
        &["worktree", "remove", "--force", owned.to_str().unwrap()],
    );
}

/// A FIRST CHECKOUT CUT SHORT WITH ITS GIT COMPLETES FROM ITS RECEIPT. `git worktree add` removes
/// what it made when it fails or is signalled. Killed outright (`SIGKILL`, a power loss) before
/// it pointed the new checkout's HEAD at the target, it leaves that checkout's `.git` file over
/// nothing, its HEAD missing (or, from some Git versions, a placeholder of zeros); after, the
/// target's HEAD over no index, some of the target's files, and the lock of the index its
/// checkout was writing. The retry recognises each from its receipt: it points the HEAD at the
/// target, refuses naming the lock until a person removes it, and completes the checkout with
/// nothing forced.
/// NEGATIVE CONTROLS: bytes at a path of the target that are not the target's are refused as
/// edits and kept; a first checkout that completed (its index written) and kept its receipt, as
/// one whose hydration failed does, holds a person's deletion, which is refused and kept.
#[test]
fn a_first_checkout_cut_short_with_its_git_completes_from_its_receipt() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    repository(&source);
    let target = commit(
        &source,
        &[("a", Some("a1")), ("b", Some("b1")), ("d/c", Some("c1"))],
    );
    let owned = directory(dir.path());
    let runner = crate::ledger::GitCli::new(&source);
    let no_child = |_: &crate::ledger::GitCli| -> Result<(), String> {
        unreachable!("the fixture has no input submodule")
    };
    // The receipt of a first checkout at `root` whose `worktree add` failed and removed what it
    // made, then the worktree `add` (with its own arguments) leaves there.
    let attributes = dir.path().join("attributes");
    std::fs::write(&attributes, "b filter=fail\n").unwrap();
    let first = |root: &Path, add: &[&str]| {
        for (key, value) in [
            ("core.attributesFile", attributes.to_str().unwrap()),
            ("filter.fail.smudge", "false"),
            ("filter.fail.required", "true"),
        ] {
            git(&source, &["config", key, value]);
        }
        assert!(checkout_with(&runner, root, &target, no_child).is_err());
        git(&source, &["config", "--unset", "core.attributesFile"]);
        git(&source, &["config", "--remove-section", "filter.fail"]);
        assert!(pending_path(root).is_file() && !root.exists());
        let mut args = vec!["worktree", "add", "-q"];
        args.extend(add);
        args.extend(["--detach", root.to_str().unwrap(), target.as_str()]);
        git(&source, &args);
    };

    let completed = directory(&dir.path().join("completed"));
    first(&completed, &[]);
    std::fs::remove_file(completed.join("b")).unwrap();
    let refused = checkout_with(&runner, &completed, &target, no_child).unwrap_err();
    assert!(
        refused.contains("tracked edits; no work was discarded"),
        "{refused}"
    );
    assert!(!completed.join("b").exists(), "the deletion is kept");
    assert!(pending_path(&completed).is_file());

    // What a `worktree add` killed with its checkout leaves: the target's HEAD, no index, and
    // one of the target's files.
    first(&owned, &["--no-checkout"]);
    std::fs::write(owned.join("a"), "a1").unwrap();
    assert_eq!(git(&owned, &["ls-files", "--cached"]), "");

    std::fs::create_dir(owned.join("d")).unwrap();
    std::fs::write(owned.join("d/c"), "a person's c").unwrap();
    let refused = checkout_with(&runner, &owned, &target, no_child).unwrap_err();
    assert!(
        refused.contains("tracked edits; no work was discarded"),
        "{refused}"
    );
    assert_eq!(read(&owned.join("d/c")), "a person's c");
    assert_eq!(git(&owned, &["ls-files", "--cached"]), "");
    std::fs::remove_file(owned.join("d/c")).unwrap();

    // The killed Git's checkout held its index lock, which it left.
    let lock = index_lock(&owned);
    std::fs::write(&lock, "").unwrap();
    let refused = checkout_with(&runner, &owned, &target, no_child).unwrap_err();
    assert!(
        refused.contains(&format!("remove {}", lock.display())),
        "{refused}"
    );
    assert_eq!(git(&owned, &["ls-files", "--cached"]), "");
    std::fs::remove_file(&lock).unwrap();

    // Killed before it pointed HEAD at the target: Git cannot open the checkout it registered.
    let unborn = |name: &str, head: Option<&str>| {
        let root = directory(&dir.path().join(name));
        first(&root, &["--no-checkout"]);
        let pointer = read(&root.join(".git"));
        let admin = root.join(pointer.trim_end().strip_prefix("gitdir: ").unwrap());
        match head {
            Some(head) => std::fs::write(admin.join("HEAD"), head).unwrap(),
            None => std::fs::remove_file(admin.join("HEAD")).unwrap(),
        }
        let _ = std::fs::remove_file(admin.join("index"));
        std::fs::write(admin.join("locked"), "initializing").unwrap();
        root
    };
    let missing = unborn("missing", None);
    let placeholder = unborn(
        "placeholder",
        Some(&format!("{}\n", "0".repeat(target.len()))),
    );

    for worktree in [&owned, &missing, &placeholder] {
        checkout_with(&runner, worktree, &target, no_child).unwrap();
        assert_eq!(git(worktree, &["rev-parse", "HEAD"]), target);
        assert!(!pending_path(worktree).exists());
        assert_eq!(git(worktree, &["status", "--porcelain", "--ignored"]), "");
        for (path, content) in [("a", "a1"), ("b", "b1"), ("d/c", "c1")] {
            assert_eq!(read(&worktree.join(path)), content, "{path}");
        }
    }
    for worktree in [&owned, &completed, &missing, &placeholder] {
        git(
            &source,
            &[
                "worktree",
                "remove",
                "--force",
                "--force",
                worktree.to_str().unwrap(),
            ],
        );
    }
}

/// THE INPUT SUBMODULE'S INTERRUPTED CHECKOUT IS RECOGNISED THE SAME WAY, from L1 to L2 and on
/// its first checkout of L2 in a clone whose default HEAD, L3, is past that pin. Each retry
/// finishes at L2, clean. A submodule checkout killed outright refuses naming the lock it left,
/// with its HEAD unmoved, until a person removes that lock.
/// NEGATIVE CONTROLS: an untracked file beside the residue is work, named as work: the retry
/// refuses before it moves anything and keeps it, from L1 and on a first checkout alike.
#[test]
fn an_interrupted_submodule_checkout_resumes_to_its_pin() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    repository(&library);
    let first_pin = commit(&library, &[("lib", Some("l1")), ("libz", Some("z1"))]);
    let source = dir.path().join("source");
    repository(&source);
    git(
        &source,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            library.to_str().unwrap(),
            INPUT_SUBMODULE,
        ],
    );
    let first = commit(&source, &[("input", Some("first"))]);
    let second_pin = commit(&library, &[("lib", Some("l2")), ("libz", Some("z2"))]);
    let source_library = source.join(INPUT_SUBMODULE);
    git(
        &source_library,
        &["-c", "protocol.file.allow=always", "fetch", "-q", "origin"],
    );
    git(&source_library, &["checkout", "-q", &second_pin]);
    let second = commit(&source, &[("input", Some("second"))]);
    // The library moves on past the pin: a fresh clone's default HEAD is L3.
    let third = commit(&library, &[("lib3", Some("l3"))]);
    let runner = crate::ledger::GitCli::new(&source);
    // Git's update of the submodule, cut short writing `libz` by `cut`'s signal; `note`, written
    // once that update ends, is a person's file beside what it left.
    let hydrate = |cut: Option<&'static str>, note: Option<PathBuf>| {
        let dir = dir.path().to_path_buf();
        move |own: &crate::ledger::GitCli| {
            let settings: Vec<String> = cut.map_or_else(Vec::new, |signal| {
                cut_at(&dir, "libz", signal)
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect()
            });
            let mut args = vec!["-c", "protocol.file.allow=always"];
            for setting in &settings {
                args.extend(["-c", setting.as_str()]);
            }
            args.extend([
                "submodule",
                "update",
                "--init",
                "--checkout",
                INPUT_SUBMODULE,
            ]);
            let updated = git_run(own, &args);
            if let Some(note) = &note {
                std::fs::write(note, "keep me").unwrap();
            }
            updated
        }
    };
    let cut_fired = || std::fs::remove_file(dir.path().join("cut")).is_ok();
    let finished = |worktree: &Path| {
        let library = worktree.join(INPUT_SUBMODULE);
        assert_eq!(git(&library, &["rev-parse", "HEAD"]), second_pin);
        assert_eq!(read(&library.join("libz")), "z2");
        assert!(!library.join("lib3").exists());
        assert_eq!(git(&library, &["status", "--porcelain", "--ignored"]), "");
        assert!(!pending_path(worktree).exists());
    };

    // From L1 to L2.
    let owned = directory(dir.path());
    checkout_with(&runner, &owned, &first, hydrate(None, None)).unwrap();
    let owned_library = owned.join(INPUT_SUBMODULE);
    assert_eq!(git(&owned_library, &["rev-parse", "HEAD"]), first_pin);
    assert!(checkout_with(&runner, &owned, &second, hydrate(Some("INT"), None)).is_err());
    assert!(cut_fired(), "the cut fired");
    assert_eq!(git(&owned_library, &["rev-parse", "HEAD"]), first_pin);
    assert_eq!(read(&owned_library.join("lib")), "l2");
    assert!(
        !owned_library.join("libz").exists(),
        "Git removed the file it was writing"
    );
    std::fs::write(owned_library.join("note"), "keep me").unwrap();
    let refused = checkout_with(&runner, &owned, &second, hydrate(None, None)).unwrap_err();
    assert!(
        refused.contains("input submodule contains work"),
        "{refused}"
    );
    assert_eq!(read(&owned_library.join("note")), "keep me");
    assert_eq!(
        git(&owned_library, &["rev-parse", "HEAD"]),
        first_pin,
        "refused before hydration moved it"
    );
    std::fs::remove_file(owned_library.join("note")).unwrap();
    checkout_with(&runner, &owned, &second, hydrate(None, None)).unwrap();
    finished(&owned);

    // From L1 to L2, its Git killed outright: the lock it left is named until it is removed.
    let killed = directory(&dir.path().join("killed"));
    checkout_with(&runner, &killed, &first, hydrate(None, None)).unwrap();
    let killed_library = killed.join(INPUT_SUBMODULE);
    assert!(checkout_with(&runner, &killed, &second, hydrate(Some("KILL"), None)).is_err());
    assert!(cut_fired(), "the cut fired");
    let lock = index_lock(&killed_library);
    assert!(lock.is_file(), "the killed Git left its lock");
    let refused = checkout_with(&runner, &killed, &second, hydrate(None, None)).unwrap_err();
    assert!(
        refused.contains(&format!("remove {}", lock.display())),
        "{refused}"
    );
    assert_eq!(git(&killed_library, &["rev-parse", "HEAD"]), first_pin);
    assert_eq!(read(&killed_library.join("lib")), "l2");
    std::fs::remove_file(&lock).unwrap();
    checkout_with(&runner, &killed, &second, hydrate(None, None)).unwrap();
    finished(&killed);

    // The first checkout of L2, cut short in a fresh clone at L3, completes from its unborn
    // HEAD at once: Git's update failed, and what it left is that checkout's alone.
    let fresh = directory(&dir.path().join("fresh"));
    checkout_with(&runner, &fresh, &second, hydrate(Some("INT"), None)).unwrap();
    assert!(cut_fired(), "the cut fired");
    finished(&fresh);

    // The same first checkout with a person's file beside what it left: that is work, named as
    // work, and the clone stays at its default HEAD with the file kept.
    let noted = directory(&dir.path().join("noted"));
    let noted_library = noted.join(INPUT_SUBMODULE);
    let refused = checkout_with(
        &runner,
        &noted,
        &second,
        hydrate(Some("INT"), Some(noted_library.join("note"))),
    )
    .unwrap_err();
    assert!(cut_fired(), "the cut fired");
    assert!(
        refused.contains("input submodule contains work; no work was discarded"),
        "{refused}"
    );
    assert_eq!(read(&noted_library.join("note")), "keep me");
    assert_eq!(git(&noted_library, &["rev-parse", "HEAD"]), third);
    assert!(pending_path(&noted).is_file());
    std::fs::remove_file(noted_library.join("note")).unwrap();
    checkout_with(&runner, &noted, &second, hydrate(None, None)).unwrap();
    finished(&noted);

    for worktree in [&owned, &killed, &fresh, &noted] {
        git(
            &source,
            &["worktree", "remove", "--force", worktree.to_str().unwrap()],
        );
    }
}
