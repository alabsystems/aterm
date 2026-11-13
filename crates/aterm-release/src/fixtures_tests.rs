// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// Both prebuilt reuse (restricted to fixture inputs) and disjoint composition
/// (all paths) must see an octopus's parent-equal fixture resolution.
#[test]
fn octopus_fixture_history_is_not_a_combined_diff() {
    use super::*;
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let git = crate::ledger::GitCli::new(root);
    let read = |args: &[&str]| git_ok(&git, args).unwrap().stdout_utf8().trim().to_owned();
    read(&["init", "-q"]);
    read(&["config", "user.name", "Fixture"]);
    read(&["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(root.join("guard"), "old\n").unwrap();
    read(&["add", "."]);
    read(&["commit", "-qm", "a"]);
    let a = read(&["rev-parse", "HEAD"]);
    let old_tree = read(&["rev-parse", "HEAD^{tree}"]);
    let b = read(&["commit-tree", &old_tree, "-p", &a, "-m", "b"]);
    std::fs::write(root.join("guard"), "current\n").unwrap();
    read(&["add", "."]);
    let tree = read(&["write-tree"]);
    let base = read(&["commit-tree", &tree, "-p", &a, "-p", &b, "-m", "released"]);
    std::fs::write(root.join("manifest"), "kept\n").unwrap();
    read(&["add", "."]);
    let tree = read(&["write-tree"]);
    let tip = read(&[
        "commit-tree",
        &tree,
        "-p",
        &base,
        "-p",
        &a,
        "-p",
        &b,
        "-m",
        "archive",
    ]);
    let restricted = touched(&git, &base, &tip, &["guard"]).unwrap();
    let all = touched(&git, &base, &tip, &[]).unwrap();
    assert_eq!(
        restricted,
        std::collections::BTreeSet::from([b"guard".to_vec()])
    );
    assert_eq!(
        all,
        std::collections::BTreeSet::from([b"guard".to_vec(), b"manifest".to_vec()])
    );
    assert!(touched(&git, &base, &base, &["guard"]).unwrap().is_empty());
    // NEGATIVE CONTROL: the old range query hides guard, unsafely reusing or composing.
    assert_eq!(
        read(&[
            "log",
            "--format=",
            "--name-only",
            "--diff-merges=combined",
            &format!("{base}..{tip}")
        ]),
        "manifest"
    );
}

use super::files_under;

#[test]
fn explicit_native_layout_binds_index_reuse_and_fallback_to_the_measured_target() {
    use super::*;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = aterm_verify::physical(scratch.path());
    let repo = root.join("repo");
    let manifest = repo.join("crates/aterm-gui/Cargo.toml");
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::write(&manifest, b"fixture manifest").unwrap();
    fs::write(repo.join(".gitignore"), b"/target/\n").unwrap();
    let git = crate::ledger::GitCli::new(&repo);
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["add", "."],
        vec!["commit", "-qm", "fixture"],
    ] {
        git_ok(&git, &args).unwrap();
    }
    let tools = root.join("tools");
    fs::create_dir(&tools).unwrap();
    // The identity query runs a signed system image and its output proves it completed.
    std::os::unix::fs::symlink("/bin/echo", tools.join("trustc")).unwrap();
    let targo = tools.join("targo");
    let (tree, toolchain) = aterm_verify::test_index::identities(&repo, &targo).unwrap();
    assert_eq!(toolchain, "-vV");
    let target = root.join("measured.noindex");
    let build_dir = root.join("build.noindex");
    let env = aterm_verify::EnvSnapshot {
        home: root.clone(),
        cargo_target_dir: Some(target.clone()),
        cargo_layout: vec![("CARGO_BUILD_BUILD_DIR", build_dir.clone().into_os_string())],
        ..Default::default()
    };
    let layout = aterm_verify::NativeBuildPlan::capture(&env, &repo).unwrap();
    fs::create_dir_all(layout.target()).unwrap();
    let binary = layout.target().join("aterm_gui-0000000000000000");
    fs::write(&binary, b"unexecuted fixture-index bytes").unwrap();
    let index = aterm_verify::test_index::Index {
        tree,
        toolchain,
        binaries: vec![aterm_verify::test_index::Entry {
            package: "aterm-gui".into(),
            kind: "lib".into(),
            name: "aterm_gui".into(),
            manifest_path: manifest,
            sha256: aterm_verify::test_index::digest(&binary).unwrap(),
            path: binary,
        }],
    };
    let at = layout.target().join("verify/test-executables.json");
    index.write(&at).unwrap();
    let explicit = GeneratorIndex::from_plan(&targo, &repo, layout.clone()).unwrap();
    assert!(explicit.ready());
    assert!(!GeneratorIndex::new(&targo, &repo).unwrap().ready());

    // A valid default index must not hide a missing index in the measured target.
    fs::remove_file(&at).unwrap();
    let default = crate::release_steps::verify_plan(&repo)
        .unwrap()
        .target()
        .to_owned();
    index
        .write(&default.join("verify/test-executables.json"))
        .unwrap();
    assert!(GeneratorIndex::new(&targo, &repo).unwrap().ready());
    assert!(!explicit.ready());
    let pins = Pins {
        version: "0.0.0".into(),
        build: 1,
        commit: "11".repeat(20),
    };
    let mut compiled = false;
    let outcome = stage_with_layout(
        &targo,
        &repo,
        &root.join("stage"),
        &pins,
        Some(&layout),
        &mut |cmd| {
            compiled = true;
            assert_eq!(cmd.get_program(), targo.as_os_str());
            let args = cmd.get_args().collect::<Vec<_>>();
            let target = args.iter().position(|arg| *arg == "--target-dir").unwrap();
            assert_eq!(args[target + 1], layout.target().as_os_str());
            assert!(!args.iter().any(|arg| *arg == "--config"));
            assert!(
                cmd.get_envs()
                    .any(|(key, value)| key == "CARGO_BUILD_BUILD_DIR"
                        && value == Some(build_dir.as_os_str()))
            );
            Err(std::io::Error::other(
                "stop before compiling or executing a fixture",
            ))
        },
    );
    assert!(outcome.is_err());
    assert!(
        compiled,
        "the default target cannot supply the measured fixture"
    );
}

/// Historical forward windows, not a timing assertion or a release operation.
#[test]
#[ignore = "models seven-minute reader churn from local origin/main history"]
fn model_reader_refresh_windows() {
    use super::*;
    let root = aterm_tempfile::manifest_dir()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let git = crate::ledger::GitCli::new(&root);
    let mut metadata = aterm_uds::spawnfd::command(crate::gates::resolve_targo().unwrap());
    let metadata = metadata
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(metadata.status.success());
    let inputs = reader_inputs::Inputs::from_metadata(&metadata.stdout, &root).unwrap();
    let head = std::env::var("ATERM_MEASURE_MAIN_BASE").unwrap_or_else(|_| "origin/main".into());
    let history = git_ok(
        &git,
        &[
            "log",
            "--first-parent",
            "--since=24 hours ago",
            "--format=%ct %H",
            &head,
        ],
    )
    .unwrap()
    .stdout_utf8();
    let commits: Vec<_> = history
        .lines()
        .map(|line| {
            let (time, sha) = line.split_once(' ').unwrap();
            (time.parse::<i64>().unwrap(), sha)
        })
        .collect();
    let mut samples = 0;
    let mut cone = 0;
    let mut fixtures = 0;
    for &(time, base) in &commits {
        if commits[0].0 < time + 420 {
            continue;
        }
        let tip = commits.iter().find(|(t, _)| *t <= time + 420).unwrap().1;
        let paths = git_ok(&git, &["diff", "--name-only", base, tip])
            .unwrap()
            .stdout_utf8();
        samples += 1;
        cone += usize::from(paths.lines().any(|p| !inputs.ignores(Path::new(p))));
        let touched = touched(&git, base, tip, &[]).unwrap();
        fixtures += usize::from(
            touched
                .iter()
                .filter_map(|p| std::str::from_utf8(p).ok())
                .any(|p| {
                    p.starts_with(HANDOFF_FIXTURE_ROOT)
                        || p == HANDOFF_FIXTURE_GUARD
                        || p == SHIPPED_STATE_CORPUS
                }),
        );
    }
    println!(
        "head {head}; 7-minute windows: {samples}; reader cone changed: {cone}; any fixture edit: {fixtures}"
    );
    assert!(samples > 0);
}

#[test]
fn files_under_ignores_symlinks() {
    let dir = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/a.txt"), b"ok").unwrap();
    std::os::unix::fs::symlink(dir.path(), dir.path().join("sub/cycle")).unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("sub/a.txt"),
        dir.path().join("sub/link.txt"),
    )
    .unwrap();
    let found = files_under(dir.path()).unwrap();
    assert_eq!(found, vec![("sub/a.txt".to_string(), b"ok".to_vec())]);
}
