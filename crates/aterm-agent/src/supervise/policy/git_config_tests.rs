// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A scratch directory at a canonical path, removed on drop.
struct Scratch(aterm_tempfile::TempDir);

impl Scratch {
    fn new(tag: &str) -> Self {
        let base = crate::supervise::test_canonical(&std::env::temp_dir());
        Self(
            aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-git-config-{tag}-"))
                .tempdir_in(base)
                .expect("scratch dir"),
        )
    }
}

/// `git <args>` in `dir`, hermetic (no system or global config), asserted ok.
fn git(dir: &Path, args: &[&str]) {
    let out = aterm_uds::deadline::output_until(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null"),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("git {args:?} within the hang detector: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A fresh repository with nothing configured beyond `git init`'s defaults.
fn repo(tag: &str) -> Scratch {
    let s = Scratch::new(tag);
    git(s.0.path(), &["init", "-q", "."]);
    s
}

/// A worker with nothing configured beyond the repository's own.
fn isolated() -> WorkerEnv {
    WorkerEnv::hermetic(None)
}

fn reads_in(dir: &Path) -> GitReads {
    GitReads {
        dirs: vec![dir.to_path_buf()],
        ..GitReads::default()
    }
}

// -- where a line's git reads run ------------------------------------------

#[test]
fn a_line_without_git_has_no_git_reads() {
    for line in ["ls -la", "grep -rn git src", "cat .gitignore"] {
        assert_eq!(
            git_reads(line, &[PathBuf::from("/w")], None),
            Ok(None),
            "{line}"
        );
    }
}

#[test]
fn a_git_read_runs_in_the_cwd_and_every_directory_the_line_names() {
    let home = Path::new("/Users//o");
    let dirs = |line: &str| {
        git_reads(line, &[PathBuf::from("/w/p")], Some(home))
            .expect("resolvable")
            .expect("a git read")
            .dirs
    };
    assert_eq!(dirs("git status --short"), [PathBuf::from("/w/p")]);
    assert_eq!(
        dirs("git -C sub log -1"),
        [PathBuf::from("/w/p/sub")],
        "-C replaces the directory for that invocation"
    );
    assert_eq!(
        dirs("git -C a -C ../b status"),
        [PathBuf::from("/w/p/b")],
        "-C chains"
    );
    let cd = dirs("cd /elsewhere/r && git status");
    assert!(cd.contains(&PathBuf::from("/w/p")), "{cd:?}");
    assert!(cd.contains(&PathBuf::from("/elsewhere/r")), "{cd:?}");
    let tilde = dirs("cd ~/r; git log -1");
    assert!(tilde.contains(&PathBuf::from("/Users//o/r")), "{tilde:?}");
    let bare = dirs("cd && git status");
    assert!(bare.contains(&PathBuf::from("/Users//o")), "{bare:?}");
    let quoted = dirs("git -C \"my dir\" status");
    assert_eq!(quoted, [PathBuf::from("/w/p/my dir")]);
}

/// Every directory the line may start from is a base: the session's and
/// wherever its Bash tool stands, each moved by the line's own `cd` and `-C`.
#[test]
fn a_git_read_runs_from_every_directory_the_shell_may_stand_in() {
    let from = [PathBuf::from("/w"), PathBuf::from("/w/sub")];
    let dirs = |line: &str| {
        git_reads(line, &from, None)
            .expect("placed")
            .expect("a git read")
            .dirs
    };
    assert_eq!(dirs("git status"), from);
    assert_eq!(
        dirs("git -C x log"),
        [PathBuf::from("/w/x"), PathBuf::from("/w/sub/x")]
    );
    assert_eq!(
        dirs("cd .. && git status"),
        [
            PathBuf::from("/w"),
            PathBuf::from("/w/sub"),
            PathBuf::from("/")
        ]
    );
}

#[test]
fn a_directory_this_check_cannot_resolve_is_refused() {
    for line in [
        "cd \"$X\" && git status",
        "cd $(mktemp -d) && git status",
        "git -C \"$D\" log",
        "cd - && git status",
        "cd ~bob/r && git status",
        "pushd +1 && git status",
    ] {
        assert!(
            git_reads(line, &[PathBuf::from("/w")], Some(Path::new("/h"))).is_err(),
            "{line}"
        );
    }
}

/// Every wrapper the classifier's head check sees through, this check sees
/// through too.
#[test]
fn a_git_behind_a_wrapper_is_still_placed() {
    let home = Path::new("/Users//o");
    let dirs = |line: &str| {
        git_reads(line, &[PathBuf::from("/w/p")], Some(home))
            .unwrap_or_else(|e| panic!("{line}: {e}"))
            .unwrap_or_else(|| panic!("{line}: a git read the check must see"))
            .dirs
    };
    let here = [PathBuf::from("/w/p")];
    assert_eq!(dirs("env -u FOO git status"), here);
    assert_eq!(dirs("env -i git status"), here);
    assert_eq!(dirs("env -P /usr/bin git status"), here);
    assert_eq!(dirs("echo x | xargs -d , git status"), here);
    assert_eq!(dirs("ls | xargs -n1 git log -1"), here);
    assert_eq!(dirs("2>/dev/null timeout 5 git status"), here);
    assert_eq!(dirs("perl -e 'alarm 30; exec @ARGV' git status"), here);
    // `env -C` moves the git, as `cd` does, and `git -C` chains on from it.
    assert_eq!(dirs("env -C /r git status"), [PathBuf::from("/r")]);
    assert_eq!(
        dirs("env --chdir=sub git status"),
        [PathBuf::from("/w/p/sub")]
    );
    assert_eq!(
        dirs("env --chdir sub git status"),
        [PathBuf::from("/w/p/sub")]
    );
    assert_eq!(dirs("env -iC.. git status"), [PathBuf::from("/w")]);
    assert_eq!(dirs("env -C /r git -C x log"), [PathBuf::from("/r/x")]);
    // A loop over an absolute `cd` lands in one place.
    assert!(dirs("for i in 1 2; do cd /r && git status; done").contains(&PathBuf::from("/r")));
    // A word that only names git runs nothing.
    assert_eq!(
        git_reads("grep -rn git src", &[PathBuf::from("/w")], None),
        Ok(None)
    );
}

#[test]
fn a_git_this_check_cannot_place_is_refused() {
    for line in [
        // xargs input supplies the directory.
        "echo . | xargs -I {} git -C {} status",
        "echo . | xargs -0I{} git -C {} status",
        "echo . | xargs -I {} env -C {} git status",
        // A directory the shell supplies.
        "env -C \"$D\" git status",
        "env -C $(mktemp -d) git status",
        // A wrapper that hides its command or picks the program itself.
        "env -S 'git status'",
        "env -P /tmp/evil git status",
        // A loop that repeats a relative `cd` climbs without a bound.
        "while true; do cd ..; git status; done",
        "for i in 1 2 3; do cd sub && git log -1; done",
    ] {
        assert!(
            git_reads(line, &[PathBuf::from("/w")], Some(Path::new("/h"))).is_err(),
            "{line}"
        );
    }
}

#[test]
fn what_a_git_read_does_beyond_reading_is_read_off_the_line() {
    let reads = |line: &str| {
        git_reads(line, &[PathBuf::from("/w")], None)
            .expect("resolvable")
            .expect("a git read")
    };
    assert!(reads("git log --show-signature -1").signatures);
    assert!(reads("git log --format='%h %G?' -3").signatures);
    assert!(reads("git tag -v v1").signatures);
    assert!(!reads("git log --oneline -3").signatures);
    assert!(reads("git log --oneline -3").log_like);
    assert!(!reads("git status").log_like);
    assert!(reads("git remote show origin").remote.is_some());
    assert!(reads("git remote show -n origin").remote.is_none());
    assert!(reads("git remote -v").remote.is_none());
}

// -- what a view says ---------------------------------------------------------

fn view(entries: &[(&str, &str, Option<&str>)]) -> GitView {
    GitView {
        config: entries
            .iter()
            .map(|(s, k, v)| (s.to_string(), k.to_string(), v.map(str::to_string)))
            .collect(),
        ..GitView::default()
    }
}

#[test]
fn each_key_that_runs_a_program_on_a_read_is_a_hazard() {
    let r = reads_in(Path::new("/w"));
    for key in [
        "core.fsmonitor",
        "diff.external",
        "diff.pdf.textconv",
        "diff.pdf.command",
        "filter.x.clean",
        "filter.x.smudge",
        "filter.x.process",
    ] {
        let why = hazard(&view(&[("local", key, Some("touch /tmp/p"))]), &r)
            .unwrap_or_else(|| panic!("{key} must be a hazard"));
        assert!(why.contains(key) && why.contains("local"), "{why}");
    }
}

/// A partial clone may fetch a missing object from its promisor remote on what
/// looks like a local read.
#[test]
fn a_partial_clone_is_a_hazard() {
    let r = reads_in(Path::new("/w"));
    for (key, value) in [
        ("extensions.partialclone", Some("origin")),
        ("remote.origin.promisor", Some("true")),
        ("remote.origin.promisor", None),
    ] {
        let why = hazard(&view(&[("local", key, value)]), &r)
            .unwrap_or_else(|| panic!("{key} must be a hazard"));
        assert!(why.contains("partial clone"), "{why}");
    }
    assert_eq!(
        hazard(
            &view(&[("local", "remote.origin.promisor", Some("false"))]),
            &r
        ),
        None
    );
}

#[test]
fn keys_that_cannot_run_on_this_read_are_not_hazards() {
    let r = reads_in(Path::new("/w"));
    // git's own fsmonitor daemon, a pager (never started without a terminal),
    // signing programs on a read that displays no signature, and the ordinary.
    for (key, value) in [
        ("core.fsmonitor", Some("true")),
        ("core.fsmonitor", Some("false")),
        ("core.fsmonitor", None),
        ("core.pager", Some("sh -c 'touch /tmp/p'")),
        ("pager.log", Some("sh -c 'touch /tmp/p'")),
        ("gpg.program", Some("/tmp/evil")),
        ("gpg.ssh.program", Some("/tmp/evil")),
        ("user.name", Some("x")),
        ("credential.helper", Some("osxkeychain")),
    ] {
        assert_eq!(hazard(&view(&[("global", key, value)]), &r), None, "{key}");
    }
}

#[test]
fn a_signing_program_is_a_hazard_only_on_a_read_that_displays_signatures() {
    let v = view(&[("local", "gpg.program", Some("/tmp/evil"))]);
    let mut r = reads_in(Path::new("/w"));
    assert_eq!(hazard(&v, &r), None);
    r.signatures = true;
    assert!(hazard(&v, &r).is_some_and(|w| w.contains("gpg.program")));
    // `log.showSignature` makes a plain `git log` display them.
    let v = view(&[
        ("local", "gpg.program", Some("/tmp/evil")),
        ("local", "log.showsignature", Some("true")),
    ]);
    let mut r = reads_in(Path::new("/w"));
    assert_eq!(hazard(&v, &r), None, "not a log");
    r.log_like = true;
    assert!(hazard(&v, &r).is_some_and(|w| w.contains("gpg.program")));
}

#[test]
fn an_index_hook_is_a_hazard() {
    let v = GitView {
        index_hook: Some(PathBuf::from("/w/.git/hooks/post-index-change")),
        ..GitView::default()
    };
    let why = hazard(&v, &reads_in(Path::new("/w"))).expect("a hazard");
    assert!(why.contains("post-index-change"), "{why}");
}

// -- the real git ---------------------------------------------------------------

#[test]
fn a_clean_repository_loads_nothing_that_runs() {
    let r = repo("clean");
    let v = isolated()
        .raw_git_view(r.0.path(), &|_| false)
        .expect("git config");
    assert!(
        v.config
            .iter()
            .any(|(scope, k, _)| scope == "local" && k == "core.bare"),
        "the repository's own config is read: {v:?}"
    );
    assert_eq!(hazard(&v, &reads_in(r.0.path())), None, "{v:?}");
    // Outside any repository: nothing either.
    let plain = Scratch::new("plain");
    let v = isolated()
        .raw_git_view(plain.0.path(), &|_| false)
        .expect("git config");
    assert_eq!(v.index_hook, None);
    assert_eq!(hazard(&v, &reads_in(plain.0.path())), None, "{v:?}");
}

#[test]
fn each_dangerous_key_in_the_repository_is_seen() {
    for key in [
        "core.fsmonitor",
        "diff.external",
        "diff.pdf.textconv",
        "filter.x.clean",
        "filter.x.process",
    ] {
        let r = repo("key");
        if key.starts_with("filter.") {
            // A filter runs only where the attributes select it.
            std::fs::write(r.0.path().join(".gitattributes"), "* filter=x\n").unwrap();
            git(r.0.path(), &["add", ".gitattributes"]);
        }
        git(
            r.0.path(),
            &["config", key, "touch /tmp/aterm-git-config-pwn"],
        );
        let v = isolated()
            .raw_git_view(r.0.path(), &|_| false)
            .expect("git config");
        let why = hazard(&v, &reads_in(r.0.path())).unwrap_or_else(|| panic!("{key}: {v:?}"));
        assert!(why.contains(key), "{why}");
        // A subdirectory of the worktree sees the same configuration.
        let sub = r.0.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(
            hazard(
                &isolated().raw_git_view(&sub, &|_| false).unwrap(),
                &reads_in(&sub)
            )
            .is_some()
        );
    }
}

#[test]
fn an_included_file_and_the_global_config_are_seen() {
    // include.path pulling a hazard in.
    let r = repo("include");
    let inc = r.0.path().join("inc.cfg");
    std::fs::write(&inc, "[core]\n\tfsmonitor = ./evil.sh\n").unwrap();
    git(
        r.0.path(),
        &["config", "include.path", inc.to_str().unwrap()],
    );
    let why = hazard(
        &isolated().raw_git_view(r.0.path(), &|_| false).unwrap(),
        &reads_in(r.0.path()),
    )
    .expect("included");
    assert!(why.contains("core.fsmonitor"), "{why}");

    // The global file — and includeIf in it.
    let clean = repo("global");
    let global = clean.0.path().join("global.cfg");
    std::fs::write(&global, "[diff]\n\texternal = /tmp/evil\n").unwrap();
    let probe = WorkerEnv::hermetic(Some(&global));
    let why = hazard(
        &probe.raw_git_view(clean.0.path(), &|_| false).unwrap(),
        &reads_in(clean.0.path()),
    )
    .expect("global");
    assert!(
        why.contains("diff.external") && why.contains("global"),
        "{why}"
    );
}

#[cfg(unix)]
#[test]
fn an_executable_post_index_change_hook_is_seen() {
    use std::os::unix::fs::PermissionsExt;
    let r = repo("hook");
    let hook = r.0.path().join(".git/hooks/post-index-change");
    std::fs::write(&hook, "#!/bin/sh\ntouch /tmp/aterm-git-config-pwn\n").unwrap();
    // Not executable: git would not run it.
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        isolated()
            .raw_git_view(r.0.path(), &|_| false)
            .unwrap()
            .index_hook,
        None
    );
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let v = isolated().raw_git_view(r.0.path(), &|_| false).unwrap();
    assert!(hazard(&v, &reads_in(r.0.path())).is_some_and(|w| w.contains("post-index-change")));
    // core.hooksPath moves it.
    let r = repo("hookspath");
    std::fs::create_dir_all(r.0.path().join("h")).unwrap();
    let moved = r.0.path().join("h/post-index-change");
    std::fs::write(&moved, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&moved, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(r.0.path(), &["config", "core.hooksPath", "h"]);
    assert!(
        isolated()
            .raw_git_view(r.0.path(), &|_| false)
            .unwrap()
            .index_hook
            .is_some()
    );
}

/// A `/`-rooted directory names no drive on Windows (Git Bash's `/c/x` is
/// `C:\x`): a git read placed there is refused, never read at this process's
/// `\c\x`. Negative control: a drive path is read.
#[cfg(windows)]
#[test]
fn a_slash_rooted_directory_is_refused_on_windows() {
    let why = isolated()
        .raw_git_view(Path::new("/c/Users"), &|_| false)
        .expect_err("no drive");
    assert!(why.contains("names no drive"), "{why}");
    let r = repo("drive");
    assert!(isolated().raw_git_view(r.0.path(), &|_| false).is_ok());
}

#[test]
fn a_directory_that_does_not_exist_is_read_at_its_nearest_ancestor() {
    let r = repo("ancestor");
    git(r.0.path(), &["config", "core.fsmonitor", "./evil.sh"]);
    let missing = r.0.path().join("not/yet");
    assert!(
        hazard(
            &isolated().raw_git_view(&missing, &|_| false).unwrap(),
            &reads_in(&missing)
        )
        .is_some()
    );
}

/// `git submodule add` of a scratch repository, hermetic, file transport
/// allowed (git refuses a local path otherwise).
fn add_submodule(sup: &Path, from: &Path, at: &str) {
    git(
        sup,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            from.to_str().unwrap(),
            at,
        ],
    );
}

/// A clean repository with one commit, to be a submodule's origin.
fn origin(tag: &str) -> Scratch {
    let r = repo(tag);
    std::fs::write(r.0.path().join("f"), "x\n").unwrap();
    git(r.0.path(), &["add", "f"]);
    git(
        r.0.path(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ],
    );
    r
}

/// A superproject's `git status` runs git in each populated submodule with its
/// own config, which the superproject's `git config --list` does not show; the
/// check views each, recursively. Negative control: a clean submodule.
#[test]
fn a_submodules_own_config_is_seen() {
    let from = origin("sub-origin");
    let sup = repo("super");
    add_submodule(sup.0.path(), from.0.path(), "sub");
    let v = isolated()
        .raw_git_view(sup.0.path(), &|_| false)
        .expect("git config");
    assert_eq!(v.submodules.len(), 1, "{v:?}");
    assert_eq!(hazard(&v, &reads_in(sup.0.path())), None, "clean: {v:?}");

    let sub = sup.0.path().join("sub");
    git(
        &sub,
        &[
            "config",
            "core.fsmonitor",
            "touch /tmp/aterm-git-config-pwn",
        ],
    );
    // The superproject's own config is clean.
    let own = isolated()
        .raw_git_view(sup.0.path(), &|_| false)
        .expect("git config");
    assert!(
        !own.config.iter().any(|(_, k, _)| k == "core.fsmonitor"),
        "{own:?}"
    );
    let why = hazard(&own, &reads_in(sup.0.path())).expect("the submodule's fsmonitor");
    assert!(
        why.contains("submodule") && why.contains("core.fsmonitor"),
        "{why}"
    );
    // Nested one level deeper: the submodule's own submodule.
    let inner_from = origin("inner-origin");
    let mid_from = origin("mid-origin");
    add_submodule(mid_from.0.path(), inner_from.0.path(), "inner");
    git(
        mid_from.0.path(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "sub",
        ],
    );
    let top = repo("top");
    add_submodule(top.0.path(), mid_from.0.path(), "mid");
    git(
        top.0.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "-q",
            "--init",
            "--recursive",
        ],
    );
    assert_eq!(
        hazard(
            &isolated().raw_git_view(top.0.path(), &|_| false).unwrap(),
            &reads_in(top.0.path())
        ),
        None
    );
    git(
        &top.0.path().join("mid/inner"),
        &["config", "diff.external", "/tmp/evil"],
    );
    let why = hazard(
        &isolated().raw_git_view(top.0.path(), &|_| false).unwrap(),
        &reads_in(top.0.path()),
    )
    .expect("nested");
    assert!(why.contains("diff.external"), "{why}");
}

/// A filter driver is a hazard only once an attribute selects it for a tracked
/// path (`git lfs install` writes `filter.lfs.*` into every global config).
#[test]
fn a_filter_driver_counts_only_where_the_attributes_select_it() {
    let r = repo("filter");
    std::fs::write(r.0.path().join("a.bin"), "x\n").unwrap();
    git(r.0.path(), &["add", "a.bin"]);
    let global = r.0.path().join("global.cfg");
    std::fs::write(
        &global,
        "[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\tsmudge = git-lfs smudge -- %f\n\tprocess = git-lfs filter-process\n",
    )
    .unwrap();
    let probe = WorkerEnv::hermetic(Some(&global));
    let v = probe
        .raw_git_view(r.0.path(), &|_| false)
        .expect("git config");
    assert_eq!(v.filters_selected, Some(Vec::new()), "{v:?}");
    assert_eq!(
        hazard(&v, &reads_in(r.0.path())),
        None,
        "no path selects lfs"
    );

    std::fs::write(r.0.path().join(".gitattributes"), "*.bin filter=lfs\n").unwrap();
    let v = probe
        .raw_git_view(r.0.path(), &|_| false)
        .expect("git config");
    assert_eq!(v.filters_selected, Some(vec!["lfs".to_string()]), "{v:?}");
    let why = hazard(&v, &reads_in(r.0.path())).expect("selected");
    assert!(why.contains("filter.lfs."), "{why}");

    // A driver the attributes name but no config defines runs nothing, and
    // a configured one no path selects stays quiet beside a selected one.
    let only_other = repo("filter-other");
    std::fs::write(only_other.0.path().join("a.bin"), "x\n").unwrap();
    std::fs::write(
        only_other.0.path().join(".gitattributes"),
        "*.bin filter=other\n",
    )
    .unwrap();
    git(only_other.0.path(), &["add", "."]);
    git(
        only_other.0.path(),
        &["config", "filter.x.clean", "touch /tmp/p"],
    );
    assert_eq!(
        hazard(
            &isolated()
                .raw_git_view(only_other.0.path(), &|_| false)
                .unwrap(),
            &reads_in(only_other.0.path())
        ),
        None
    );
    git(
        only_other.0.path(),
        &["config", "filter.other.clean", "touch /tmp/p"],
    );
    assert!(
        hazard(
            &isolated()
                .raw_git_view(only_other.0.path(), &|_| false)
                .unwrap(),
            &reads_in(only_other.0.path())
        )
        .is_some_and(|w| w.contains("filter.other.clean"))
    );
}

/// With an fsmonitor program configured, the check reads nothing that loads
/// the index — that would run the program it is looking for — and escalates.
#[cfg(unix)]
#[test]
fn the_check_never_runs_the_fsmonitor_it_finds() {
    use std::os::unix::fs::PermissionsExt;
    let r = repo("fsm-run");
    std::fs::write(r.0.path().join("f"), "x\n").unwrap();
    git(r.0.path(), &["add", "f"]);
    let ran = r.0.path().join("RAN");
    let script = r.0.path().join("fsm.sh");
    std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(
        r.0.path(),
        &["config", "core.fsmonitor", script.to_str().unwrap()],
    );
    // A configured filter too, so the attribute read would be wanted.
    git(r.0.path(), &["config", "filter.x.clean", "cat"]);
    let v = isolated()
        .raw_git_view(r.0.path(), &|_| false)
        .expect("git config");
    assert!(hazard(&v, &reads_in(r.0.path())).is_some_and(|w| w.contains("core.fsmonitor")));
    assert!(
        !ran.exists(),
        "the check ran the fsmonitor it was checking for"
    );
    // Negative control: the probe is live — a real status does run it.
    git(r.0.path(), &["status", "--short"]);
    assert!(
        ran.exists(),
        "the fixture's fsmonitor never runs, so the check proves nothing"
    );
}

// -- the worker's environment ---------------------------------------------------

/// The worker's own `GIT_CONFIG_*` is configuration its git reads load
/// (scope `command`): the view is read in the worker's environment.
/// Negative control: the same repository, the worker without it.
#[test]
fn the_workers_git_config_variables_are_seen() {
    let r = repo("worker-env");
    assert_eq!(
        hazard(
            &isolated().raw_git_view(r.0.path(), &|_| false).unwrap(),
            &reads_in(r.0.path())
        ),
        None
    );
    let worker = isolated()
        .with("GIT_CONFIG_VALUE_0=./evil.sh")
        .with("GIT_CONFIG_KEY_0=core.fsmonitor")
        .with("GIT_CONFIG_COUNT=1");
    let why = hazard(
        &worker.raw_git_view(r.0.path(), &|_| false).unwrap(),
        &reads_in(r.0.path()),
    )
    .expect("the worker's fsmonitor");
    assert!(
        why.contains("core.fsmonitor") && why.contains("command"),
        "{why}"
    );
}

/// `GIT_EXTERNAL_DIFF` in the worker's environment runs a program on a diff
/// as `diff.external` does. An empty one names none.
#[test]
fn the_workers_external_diff_is_a_hazard() {
    let r = repo("ext-diff");
    let worker = isolated().with("GIT_EXTERNAL_DIFF=/tmp/evil");
    let why = hazard(
        &worker.raw_git_view(r.0.path(), &|_| false).unwrap(),
        &reads_in(r.0.path()),
    )
    .expect("a hazard");
    assert!(why.contains("GIT_EXTERNAL_DIFF"), "{why}");
    let worker = isolated().with("GIT_EXTERNAL_DIFF=");
    assert_eq!(
        hazard(
            &worker.raw_git_view(r.0.path(), &|_| false).unwrap(),
            &reads_in(r.0.path())
        ),
        None
    );
}

/// Neither `GIT_CONFIG` (the probe's `git config --list` would list that one
/// file and miss what a read loads) nor `GIT_TRACE*` (the probe would write)
/// is passed on. Control: `GIT_CONFIG` really does hide the repository's
/// config from a plain `git config --list`.
#[test]
fn what_only_git_config_reads_does_not_hide_the_effective_config() {
    let r = repo("git-config-var");
    git(r.0.path(), &["config", "core.fsmonitor", "./evil.sh"]);
    let empty = r.0.path().join("empty.cfg");
    std::fs::write(&empty, "").unwrap();
    let hidden = aterm_uds::deadline::output_until(
        Command::new("git")
            .args(["config", "--list"])
            .current_dir(r.0.path())
            .env("GIT_CONFIG", &empty),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("git config within the hang detector: {e}"));
    assert!(
        !String::from_utf8_lossy(&hidden.stdout).contains("core.fsmonitor"),
        "the control: GIT_CONFIG hides the repository's config"
    );
    let trace = r.0.path().join("trace.log");
    let worker = isolated()
        .with(&format!("GIT_CONFIG={}", empty.display()))
        .with(&format!("GIT_TRACE={}", trace.display()));
    assert!(
        hazard(
            &worker.raw_git_view(r.0.path(), &|_| false).unwrap(),
            &reads_in(r.0.path())
        )
        .is_some_and(|w| w.contains("core.fsmonitor"))
    );
    assert!(!trace.exists(), "the probe wrote a trace");
}

/// The probe runs the git the worker's `PATH` finds (a wrapper that adds a
/// configuration stands in for a git built differently). Negative control:
/// the worker without it on its `PATH`.
#[cfg(unix)]
#[test]
fn the_git_on_the_workers_path_is_the_one_read() {
    use std::os::unix::fs::PermissionsExt;
    let r = repo("which-git");
    let real = isolated()
        .git_program(&|_| false)
        .expect("a git on this PATH");
    let bin = Scratch::new("bin");
    let wrapper = bin.0.path().join("git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec '{}' -c core.fsmonitor=./evil.sh \"$@\"\n",
            real.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The wrapper is a file this test just wrote: the system's assessment of
    // its first exec, which waits out every scan queued ahead of it on a
    // loaded host, is paid here, unclocked, under its own hang detector, not
    // inside the read below.
    aterm_spec::wait::first_exec(&wrapper, &["--version"]);
    let path = format!(
        "PATH={}:{}",
        bin.0.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let why = hazard(
        &isolated()
            .with(&path)
            .raw_git_view(r.0.path(), &|_| false)
            .unwrap(),
        &reads_in(r.0.path()),
    )
    .expect("the worker's git");
    assert!(why.contains("core.fsmonitor"), "{why}");
    assert_eq!(
        hazard(
            &isolated().raw_git_view(r.0.path(), &|_| false).unwrap(),
            &reads_in(r.0.path())
        ),
        None
    );
}

/// Which git runs is refused when the worker's `PATH` cannot say: a relative
/// (or empty) entry ahead of git, no `PATH` at all, and no git on it.
#[test]
fn a_worker_path_that_cannot_name_the_git_is_refused() {
    let r = repo("bad-path");
    for path in [
        "PATH=relative/bin:/usr/bin",
        "PATH=:/usr/bin",
        "PATH=/nonexistent-aterm-git-dir",
    ] {
        assert!(
            isolated()
                .with(path)
                .raw_git_view(r.0.path(), &|_| false)
                .is_err(),
            "{path}"
        );
    }
    assert!(
        WorkerEnv::default()
            .raw_git_view(r.0.path(), &|_| false)
            .is_err(),
        "no PATH"
    );
}

/// The first entry of a variable is the one `getenv` answers, and the one the
/// probe passes on.
#[test]
fn the_first_entry_of_a_variable_wins() {
    use crate::harness::accounts::HOMEDIR_ENV;
    let env = WorkerEnv::from_env(vec![
        "CLAUDE_CONFIG_DIR=/a".to_string(),
        "CLAUDE_CONFIG_DIR=/b".to_string(),
        format!("{HOMEDIR_ENV}=/h"),
    ]);
    assert_eq!(env.var("CLAUDE_CONFIG_DIR"), Some("/a"));
    let cwd = Path::new("/w");
    assert_eq!(env.claude_dir(cwd), Ok(PathBuf::from("/a")));
    let home_only = WorkerEnv::from_env(vec![format!("{HOMEDIR_ENV}=/h")]);
    assert_eq!(home_only.claude_dir(cwd), Ok(PathBuf::from("/h/.claude")));
}

/// A WORKER RESOLVES UNDER ITS OWN `os.homedir()`, the one variable it reads
/// ([`crate::harness::accounts::HOMEDIR_ENV`]): on Windows its
/// `USERPROFILE`, a scratch one included, EVEN BESIDE A `HOME` naming
/// another folder (measured, `claude.exe` 2.1.304: it wrote under
/// `USERPROFILE` and nothing under `HOME`); elsewhere its `HOME`. An empty
/// one falls to the account's home. NEGATIVE CONTROL: the other variable
/// alone names nothing (the account's home answers).
#[test]
fn a_worker_resolves_under_its_own_homedir() {
    use crate::harness::accounts::HOMEDIR_ENV;
    let cwd = Path::new("/w");
    let (scratch, own, other) = if cfg!(windows) {
        (r"C:\scratch\profile", "USERPROFILE", "HOME")
    } else {
        ("/scratch/profile", "HOME", "USERPROFILE")
    };
    assert_eq!(own, HOMEDIR_ENV);
    let under_scratch = Path::new(scratch).join(".claude");
    let account = atpkg::platform::account_home().map(|h| h.join(".claude"));
    let worker = |env: &[String]| WorkerEnv::from_env(env.to_vec()).claude_dir(cwd);
    assert_eq!(
        worker(&[format!("{own}={scratch}")]),
        Ok(under_scratch.clone())
    );
    assert_eq!(
        worker(&[format!("{other}=/h"), format!("{own}={scratch}")]),
        Ok(under_scratch.clone())
    );
    assert_eq!(worker(&[format!("{own}=")]).ok(), account);
    assert_eq!(worker(&[format!("{other}={scratch}")]).ok(), account);
}

/// A set `CLAUDE_CONFIG_DIR` wins, a relative (or empty) one taken against the
/// cwd; no `HOME`, or an empty one, is the account's home. Negative control:
/// with no account home either, it is an error, never a directory skipped.
#[test]
fn the_claude_code_directory_is_resolved_as_claude_code_resolves_it() {
    let cwd = Path::new("/w/proj");
    let account = || Some(PathBuf::from("/Users//acct"));
    for (config, home, want) in [
        (Some("/a"), Some("/h"), "/a"),
        (Some("cc"), Some("/h"), "/w/proj/cc"),
        (Some("../cc/./x"), None, "/w/cc/x"),
        (Some(""), Some("/h"), "/w/proj"),
        (None, Some("/h"), "/h/.claude"),
        (None, Some("h"), "/w/proj/h/.claude"),
        (None, Some(""), "/Users//acct/.claude"),
        (None, None, "/Users//acct/.claude"),
    ] {
        assert_eq!(
            claude_dir_at(config, home, cwd, account),
            Ok(PathBuf::from(want)),
            "{config:?} {home:?}"
        );
    }
    assert!(
        claude_dir_at(None, None, cwd, || None)
            .expect_err("no home anywhere")
            .contains("no home for its account")
    );
}

// -- the projection a harness tape keeps ----------------------------------------

/// Every combination of what a read does beyond reading that `hazard` reads:
/// a signature display of its own, and a `log`-like read that
/// `log.showSignature` turns into one.
fn every_read() -> Vec<GitReads> {
    let mut out = Vec::new();
    for signatures in [false, true] {
        for log_like in [false, true] {
            out.push(GitReads {
                dirs: vec![PathBuf::from("/w")],
                signatures,
                log_like,
                remote: None,
            });
        }
    }
    out
}

/// The keys a generated view draws from: every kind `hazard` reads, and keys
/// it never reads — a URL in a key's own name, a credential helper, an extra
/// header carrying a token, a pager, a name.
const KEY_POOL: &[&str] = &[
    "core.fsmonitor",
    "diff.external",
    "diff.pdf.textconv",
    "diff.pdf.command",
    "diff.textconv",
    "filter.lfs.clean",
    "filter.lfs.smudge",
    "filter.crypt.process",
    "filter.lfs.required",
    "extensions.partialclone",
    "remote.origin.promisor",
    "remote.origin.url",
    "gpg.program",
    "gpg.ssh.program",
    "gpg.format",
    "log.showsignature",
    "url.https://planted-user:planted-token@example.com/.insteadof",
    "http.https://example.com/.extraheader",
    "credential.helper",
    "core.pager",
    "pager.log",
    "user.name",
];

/// The values a generated entry draws from: bare, git's spellings of true and
/// false (the empty value among the false), and programs and secrets.
const VALUE_POOL: &[Option<&str>] = &[
    None,
    Some("true"),
    Some("TRUE"),
    Some("yes"),
    Some("On"),
    Some("1"),
    Some("false"),
    Some("no"),
    Some("OFF"),
    Some("0"),
    Some(""),
    Some("fsmonitor-watchman"),
    Some("/tmp/planted-program"),
    Some("Authorization: Bearer planted-token"),
    Some("origin"),
];

/// A generated view, `depth` submodule levels down at most.
fn generated(rng: &mut aterm_spec::rng::XorShift, depth: usize) -> GitView {
    let scopes = ["system", "global", "local", "worktree", "command"];
    let config = (0..rng.range(0, 7))
        .map(|_| {
            (
                rng.pick(&scopes).to_string(),
                rng.pick(KEY_POOL).to_string(),
                rng.pick(VALUE_POOL).map(str::to_string),
            )
        })
        .collect();
    let drivers = ["lfs", "crypt", "other"];
    GitView {
        config,
        index_hook: (rng.below(5) == 0).then(|| PathBuf::from("/w/.git/hooks/post-index-change")),
        filters_selected: match rng.below(3) {
            0 => None,
            1 => Some(Vec::new()),
            _ => Some(vec![rng.pick(&drivers).to_string()]),
        },
        submodules: if depth == 0 {
            Vec::new()
        } else {
            (0..rng.range(0, 2))
                .map(|i| {
                    (
                        PathBuf::from(format!("/w/sub{i}")),
                        generated(rng, depth - 1),
                    )
                })
                .collect()
        },
        external_diff: (rng.below(6) == 0).then(|| "/tmp/planted-difftool".to_string()),
    }
}

/// The fixtures of the hazard tests above, each a one-entry view.
fn fixture_views() -> Vec<GitView> {
    let mut out: Vec<GitView> = [
        ("local", "core.fsmonitor", Some("touch /tmp/p")),
        ("local", "diff.external", Some("touch /tmp/p")),
        ("local", "diff.pdf.textconv", Some("touch /tmp/p")),
        ("local", "diff.pdf.command", Some("touch /tmp/p")),
        ("local", "filter.x.clean", Some("touch /tmp/p")),
        ("local", "filter.x.smudge", Some("touch /tmp/p")),
        ("local", "filter.x.process", Some("touch /tmp/p")),
        ("local", "extensions.partialclone", Some("origin")),
        ("local", "remote.origin.promisor", Some("true")),
        ("local", "remote.origin.promisor", None),
        ("local", "remote.origin.promisor", Some("false")),
        ("global", "core.fsmonitor", Some("true")),
        ("global", "core.fsmonitor", Some("false")),
        ("global", "core.fsmonitor", None),
        ("global", "core.pager", Some("sh -c 'touch /tmp/p'")),
        ("global", "pager.log", Some("sh -c 'touch /tmp/p'")),
        ("global", "gpg.program", Some("/tmp/evil")),
        ("global", "gpg.ssh.program", Some("/tmp/evil")),
        ("global", "user.name", Some("x")),
        ("global", "credential.helper", Some("osxkeychain")),
    ]
    .iter()
    .map(|e| view(&[*e]))
    .collect();
    out.push(view(&[
        ("local", "gpg.program", Some("/tmp/evil")),
        ("local", "log.showsignature", Some("true")),
    ]));
    out.push(GitView {
        index_hook: Some(PathBuf::from("/w/.git/hooks/post-index-change")),
        ..GitView::default()
    });
    out
}

/// `project` judged the way `hazard` judges: `None` when, over the fixtures
/// and a thousand generated views (submodules two deep), every read judges
/// the projection exactly as it judges the view — the same reason, word for
/// word — else the first view and read that differ.
fn projection_holds(project: impl Fn(&GitView) -> GitView) -> Option<(GitView, GitReads)> {
    let mut rng = aterm_spec::rng::XorShift::new(0x7a9e_0005);
    let views = fixture_views()
        .into_iter()
        .chain((0..1000).map(|_| generated(&mut rng, 2)));
    for v in views {
        let p = project(&v);
        for r in every_read() {
            if hazard(&v, &r) != hazard(&p, &r) {
                return Some((v, r));
            }
        }
    }
    None
}

/// THE PROJECTION A TAPE KEEPS JUDGES AS THE VIEW DID: `hazard(v, r) ==
/// hazard(v.project(), r)` for every fixture and generated view and every
/// combination of what a read does — so the policy, which judges the
/// projection in every mode, judges as it did before the probe.
/// Negative control: a projection that also drops `log.showsignature` is
/// caught (a `git log` that the key turns into a signature display, over a
/// signing program, judges otherwise).
#[test]
fn hazard_judges_the_projection_as_it_judges_the_view() {
    assert_eq!(projection_holds(GitView::project), None);
    let dropping = |v: &GitView| {
        let mut p = v.project();
        p.config.retain(|(_, k, _)| k != "log.showsignature");
        p
    };
    let (v, r) = projection_holds(dropping).expect("the control is caught");
    assert!(r.log_like && !r.signatures, "{v:?} {r:?}");
}

/// THE PROJECTION KEEPS NOTHING `hazard` DOES NOT READ: no key it never reads
/// (a URL in a key's name, a credential helper, an extra header), no value
/// but a class, the worker's `GIT_EXTERNAL_DIFF` as its presence — over a
/// view that plants a secret in each, down a submodule too.
#[test]
fn the_projection_keeps_only_what_hazard_reads() {
    let secrets = [
        "planted-user",
        "planted-token",
        "osxkeychain",
        "/tmp/planted-program",
        "/tmp/planted-difftool",
        "example.com",
    ];
    let planted = GitView {
        config: vec![
            (
                "global".to_string(),
                "url.https://planted-user:planted-token@example.com/.insteadof".to_string(),
                Some("https://example.com/".to_string()),
            ),
            (
                "global".to_string(),
                "http.https://example.com/.extraheader".to_string(),
                Some("Authorization: Bearer planted-token".to_string()),
            ),
            (
                "global".to_string(),
                "credential.helper".to_string(),
                Some("osxkeychain".to_string()),
            ),
            (
                "local".to_string(),
                "core.fsmonitor".to_string(),
                Some("/tmp/planted-program".to_string()),
            ),
        ],
        external_diff: Some("/tmp/planted-difftool".to_string()),
        ..GitView::default()
    };
    let view = GitView {
        submodules: vec![(PathBuf::from("/w/sub"), planted.clone())],
        ..planted
    };
    let projected = format!("{:?}", view.project());
    for secret in secrets {
        assert!(!projected.contains(secret), "{secret} kept: {projected}");
    }
    // Negative control: the view itself holds every one of them.
    let whole = format!("{view:?}");
    for secret in secrets {
        assert!(whole.contains(secret), "{secret} was not planted");
    }
    // What `hazard` reads is kept: the fsmonitor key, a program by class.
    assert!(
        view.project()
            .config
            .iter()
            .any(|(_, k, v)| k == "core.fsmonitor" && v.as_deref() == Some(VALUE_SET))
    );
}

/// Each value class answers git's boolean readers as every value in it does.
#[test]
fn a_value_class_reads_as_its_values() {
    for v in VALUE_POOL {
        let class = value_class(*v);
        assert_eq!(is_bool(class.as_deref()), is_bool(*v), "{v:?}");
        assert_eq!(is_false(class.as_deref()), is_false(*v), "{v:?}");
        assert_eq!(is_true(class.as_deref()), is_true(*v), "{v:?}");
    }
}
