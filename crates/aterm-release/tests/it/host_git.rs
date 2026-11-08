// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This test binary's git reads no configuration of the machine it runs on.

use std::path::Path;

use crate::ledger::{GitCli, GitRunner as _};

const FIXTURE_URL: &str = "https://github.com/fixture/app.git";

/// THE CUTTER'S OWN GIT, IN THIS TEST BINARY, READS NO CONFIGURATION OF THE MACHINE IT RUNS ON
/// (`hermetic_git`, run when it loads): run in a home whose `.gitconfig` signs
/// each commit with a signer that fails, names a hooks path whose pre-commit refuses, and
/// rewrites `https://github.com/` to a mirror, and whose global ignore file ignores everything and
/// global attributes file marks everything `eol=crlf`, [`GitCli`] commits in a fixture, adds a
/// file, reads no attribute and reads the fixture's origin as the fixture's config records it, so
/// an engine fixture sees what it set up; an attributes file the fixture's own config names is
/// still read. NEGATIVE CONTROL: with that `.gitconfig` named as the global one, the commit fails
/// and the origin is rewritten; with no global configuration, the add is refused and the
/// attribute is read.
#[test]
fn the_cutters_git_reads_no_configuration_of_the_machine_it_runs_on() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (home, hooks, repo) = (root.join("home"), root.join("hooks"), root.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&hooks).unwrap();
    let refuse = hooks.join("pre-commit");
    std::fs::write(
        &refuse,
        "#!/bin/sh\necho 'the host hook refused' >&2\nexit 1\n",
    )
    .unwrap();
    let mut mode = std::fs::metadata(&refuse).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&refuse, mode).unwrap();
    let gitconfig = home.join(".gitconfig");
    std::fs::write(
        &gitconfig,
        format!(
            "[commit]\n\tgpgsign = true\n[gpg]\n\tprogram = /usr/bin/false\n[core]\n\thooksPath = \
             {}\n[url \"https://mirror.example.invalid/\"]\n\tinsteadOf = https://github.com/\n",
            hooks.display()
        ),
    )
    .unwrap();
    crate::ledger::testing::init(
        &repo,
        &[
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
        ],
    );
    let out = crate::ledger::testing::git()
        .arg("-C")
        .arg(&repo)
        .args(["remote", "add", "origin", FIXTURE_URL])
        .output()
        .unwrap();
    assert!(out.status.success());
    let xdg_git = root.join("xdg/git");
    std::fs::create_dir_all(&xdg_git).unwrap();
    std::fs::write(xdg_git.join("ignore"), "*\n").unwrap();
    std::fs::write(xdg_git.join("attributes"), "* eol=crlf\n").unwrap();
    for leaf in ["kept.txt", "ignored.txt"] {
        std::fs::write(repo.join(leaf), "fixture\n").unwrap();
    }
    let git = GitCli::new(&repo);
    let text = |path: &Path| path.to_str().unwrap().to_owned();
    let (home, xdg, global) = (text(&home), text(&root.join("xdg")), text(&gitconfig));
    let at_home = [("HOME", home.as_str()), ("XDG_CONFIG_HOME", xdg.as_str())];
    let commit = ["commit", "-q", "--allow-empty", "-m", "fixture"];
    let origin = ["remote", "get-url", "origin"];

    let eol = |leaf: &'static str| ["check-attr", "eol", "--", leaf];

    let made = git.git_env(&commit, &at_home).unwrap();
    assert!(made.success(), "{}", made.stderr_utf8());
    assert_eq!(
        git.git_env(&origin, &at_home).unwrap().stdout_utf8().trim(),
        FIXTURE_URL
    );
    let added = git.git_env(&["add", "kept.txt"], &at_home).unwrap();
    assert!(added.success(), "{}", added.stderr_utf8());
    assert_eq!(
        git.git_env(&eol("kept.txt"), &at_home)
            .unwrap()
            .stdout_utf8()
            .trim(),
        "kept.txt: eol: unspecified"
    );

    let named = [
        at_home[0],
        at_home[1],
        ("GIT_CONFIG_GLOBAL", global.as_str()),
    ];
    assert!(
        !git.git_env(&commit, &named).unwrap().success(),
        "NEGATIVE CONTROL: the home's configuration refuses the commit"
    );
    assert_eq!(
        git.git_env(&origin, &named).unwrap().stdout_utf8().trim(),
        "https://mirror.example.invalid/fixture/app.git",
        "NEGATIVE CONTROL: the home's configuration rewrites the origin"
    );
    let bare = [at_home[0], at_home[1], ("GIT_CONFIG_GLOBAL", "/dev/null")];
    assert!(
        !git.git_env(&["add", "ignored.txt"], &bare)
            .unwrap()
            .success(),
        "NEGATIVE CONTROL: the home's ignore file refuses the add"
    );
    assert_eq!(
        git.git_env(&eol("ignored.txt"), &bare)
            .unwrap()
            .stdout_utf8()
            .trim(),
        "ignored.txt: eol: crlf",
        "NEGATIVE CONTROL: the home's attributes file is read"
    );

    // The hermetic configuration is global scope: a test's own repository setting outranks it.
    let attributes = text(&xdg_git.join("attributes"));
    let set = ["config", "core.attributesFile", attributes.as_str()];
    assert!(git.git_env(&set, &at_home).unwrap().success());
    assert_eq!(
        git.git_env(&eol("kept.txt"), &at_home)
            .unwrap()
            .stdout_utf8()
            .trim(),
        "kept.txt: eol: crlf",
        "a repository's attributes file is read"
    );
}

/// Where [`a_test_binary_loads_without_the_git_environment_it_was_started_with`]'s re-run writes
/// the Git environment it loaded with.
const LOAD_PROBE: &str = "ATERM_RELEASE_TEST_GIT_LOAD_PROBE";

/// THIS TEST BINARY LOADS WITHOUT THE GIT ENVIRONMENT IT WAS STARTED WITH: re-run with a
/// `GIT_DIR`, a global config, an empty command configuration and a `GIT_EDITOR` (as a hook or a
/// shell may export them), the image's code finds none of them, only the hermetic global
/// configuration (`crate::hermetic_git::HERMETIC_GLOBAL_CONFIG`) and a `GIT_TRACE*` kept.
/// NEGATIVE CONTROL: re-run with `crate::ledger::testing::CHOSEN_GIT_ENV`, as a test that chose
/// them starts it, it keeps them.
#[test]
fn a_test_binary_loads_without_the_git_environment_it_was_started_with() {
    const NAMES: [&str; 5] = [
        "GIT_DIR",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_COUNT",
        "GIT_EDITOR",
        "GIT_TRACE_SETUP",
    ];
    if let Some(out) = std::env::var_os(LOAD_PROBE) {
        let seen: Vec<String> = NAMES
            .iter()
            .map(|name| format!("{name}={}", std::env::var(name).unwrap_or_default()))
            .collect();
        std::fs::write(out, seen.join("\n")).unwrap();
        return;
    }
    let hermetic = std::env::var("GIT_CONFIG_GLOBAL").unwrap();
    assert_eq!(
        std::fs::read_to_string(&hermetic).unwrap(),
        crate::hermetic_git::HERMETIC_GLOBAL_CONFIG
    );
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let rerun = |chosen: bool| {
        let out = root.join(format!("seen-{chosen}"));
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "host_git::a_test_binary_loads_without_the_git_environment_it_was_started_with",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(LOAD_PROBE, &out)
            .env("GIT_DIR", "/hostile/.git")
            .env("GIT_CONFIG_GLOBAL", "/hostile/gitconfig")
            .env("GIT_CONFIG_COUNT", "0")
            .env("GIT_EDITOR", "/hostile/editor")
            .env("GIT_TRACE_SETUP", "0")
            .stdin(aterm_uds::spawnfd::Stdio::null());
        if chosen {
            child.env(crate::ledger::testing::CHOSEN_GIT_ENV, "1");
        } else {
            child.env_remove(crate::ledger::testing::CHOSEN_GIT_ENV);
        }
        let ran =
            aterm_uds::deadline::output_within(&mut child, aterm_spec::wait::MUST_HAPPEN).unwrap();
        assert!(
            ran.status.success(),
            "{}",
            String::from_utf8_lossy(&ran.stderr)
        );
        std::fs::read_to_string(out).unwrap()
    };
    assert_eq!(
        rerun(false),
        format!(
            "GIT_DIR=\nGIT_CONFIG_GLOBAL={hermetic}\nGIT_CONFIG_COUNT=\nGIT_EDITOR=\n\
             GIT_TRACE_SETUP=0"
        )
    );
    assert_eq!(
        rerun(true),
        "GIT_DIR=/hostile/.git\nGIT_CONFIG_GLOBAL=/hostile/gitconfig\nGIT_CONFIG_COUNT=0\n\
         GIT_EDITOR=/hostile/editor\nGIT_TRACE_SETUP=0",
        "NEGATIVE CONTROL: a chosen Git environment is kept"
    );
}
