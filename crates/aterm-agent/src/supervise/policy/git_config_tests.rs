// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aterm-git-config-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(std::fs::canonicalize(&dir).expect("canonical scratch"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `git <args>` in `dir`, hermetic (no system or global config), asserted ok.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A fresh repository with nothing configured beyond `git init`'s defaults.
fn repo(tag: &str) -> Scratch {
    let s = Scratch::new(tag);
    git(&s.0, &["init", "-q", "."]);
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
/// through too — each of these the classifier approves as a read, and before
/// the two shared one reading of a segment each was `Ok(None)` here: no
/// config check at all (the 2026-09-26 review's scratch binary).
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
    // A word that only NAMES git runs nothing.
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

/// A partial clone fetches a missing object on demand, from its promisor
/// remote: credential helpers, ssh and the remote's programs run on what
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
    let v = isolated().view(&r.0, &|_| false).expect("git config");
    assert!(
        v.config
            .iter()
            .any(|(scope, k, _)| scope == "local" && k == "core.bare"),
        "the repository's own config is read: {v:?}"
    );
    assert_eq!(hazard(&v, &reads_in(&r.0)), None, "{v:?}");
    // Outside any repository: nothing either.
    let plain = Scratch::new("plain");
    let v = isolated().view(&plain.0, &|_| false).expect("git config");
    assert_eq!(v.index_hook, None);
    assert_eq!(hazard(&v, &reads_in(&plain.0)), None, "{v:?}");
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
            std::fs::write(r.0.join(".gitattributes"), "* filter=x\n").unwrap();
            git(&r.0, &["add", ".gitattributes"]);
        }
        git(&r.0, &["config", key, "touch /tmp/aterm-git-config-pwn"]);
        let v = isolated().view(&r.0, &|_| false).expect("git config");
        let why = hazard(&v, &reads_in(&r.0)).unwrap_or_else(|| panic!("{key}: {v:?}"));
        assert!(why.contains(key), "{why}");
        // A subdirectory of the worktree sees the same configuration.
        let sub = r.0.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(hazard(&isolated().view(&sub, &|_| false).unwrap(), &reads_in(&sub)).is_some());
    }
}

#[test]
fn an_included_file_and_the_global_config_are_seen() {
    // include.path pulling a hazard in.
    let r = repo("include");
    let inc = r.0.join("inc.cfg");
    std::fs::write(&inc, "[core]\n\tfsmonitor = ./evil.sh\n").unwrap();
    git(&r.0, &["config", "include.path", inc.to_str().unwrap()]);
    let why =
        hazard(&isolated().view(&r.0, &|_| false).unwrap(), &reads_in(&r.0)).expect("included");
    assert!(why.contains("core.fsmonitor"), "{why}");

    // The global file — and includeIf in it.
    let clean = repo("global");
    let global = clean.0.join("global.cfg");
    std::fs::write(&global, "[diff]\n\texternal = /tmp/evil\n").unwrap();
    let probe = WorkerEnv::hermetic(Some(&global));
    let why = hazard(
        &probe.view(&clean.0, &|_| false).unwrap(),
        &reads_in(&clean.0),
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
    let hook = r.0.join(".git/hooks/post-index-change");
    std::fs::write(&hook, "#!/bin/sh\ntouch /tmp/aterm-git-config-pwn\n").unwrap();
    // Not executable: git would not run it.
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(isolated().view(&r.0, &|_| false).unwrap().index_hook, None);
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let v = isolated().view(&r.0, &|_| false).unwrap();
    assert!(hazard(&v, &reads_in(&r.0)).is_some_and(|w| w.contains("post-index-change")));
    // core.hooksPath moves it.
    let r = repo("hookspath");
    std::fs::create_dir_all(r.0.join("h")).unwrap();
    let moved = r.0.join("h/post-index-change");
    std::fs::write(&moved, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&moved, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&r.0, &["config", "core.hooksPath", "h"]);
    assert!(
        isolated()
            .view(&r.0, &|_| false)
            .unwrap()
            .index_hook
            .is_some()
    );
}

#[test]
fn a_directory_that_does_not_exist_is_read_at_its_nearest_ancestor() {
    let r = repo("ancestor");
    git(&r.0, &["config", "core.fsmonitor", "./evil.sh"]);
    let missing = r.0.join("not/yet");
    assert!(
        hazard(
            &isolated().view(&missing, &|_| false).unwrap(),
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
    std::fs::write(r.0.join("f"), "x\n").unwrap();
    git(&r.0, &["add", "f"]);
    git(
        &r.0,
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

/// A superproject's `git status` runs git in each populated submodule, with
/// that submodule's config (`.git/modules/<name>/config`) — measured by the
/// 2026-09-26 review: an fsmonitor set there ran on a clean `git status` in
/// the superproject, whose own `git config --list` showed nothing. The check
/// views every populated submodule, recursively. NEGATIVE CONTROL: the same
/// superproject with a clean submodule loads nothing that runs.
#[test]
fn a_submodules_own_config_is_seen() {
    let from = origin("sub-origin");
    let sup = repo("super");
    add_submodule(&sup.0, &from.0, "sub");
    let v = isolated().view(&sup.0, &|_| false).expect("git config");
    assert_eq!(v.submodules.len(), 1, "{v:?}");
    assert_eq!(hazard(&v, &reads_in(&sup.0)), None, "clean: {v:?}");

    let sub = sup.0.join("sub");
    git(
        &sub,
        &[
            "config",
            "core.fsmonitor",
            "touch /tmp/aterm-git-config-pwn",
        ],
    );
    // What the review measured: the superproject's own config is clean.
    let own = isolated().view(&sup.0, &|_| false).expect("git config");
    assert!(
        !own.config.iter().any(|(_, k, _)| k == "core.fsmonitor"),
        "{own:?}"
    );
    let why = hazard(&own, &reads_in(&sup.0)).expect("the submodule's fsmonitor");
    assert!(
        why.contains("submodule") && why.contains("core.fsmonitor"),
        "{why}"
    );
    // Nested one level deeper: the submodule's own submodule.
    let inner_from = origin("inner-origin");
    let mid_from = origin("mid-origin");
    add_submodule(&mid_from.0, &inner_from.0, "inner");
    git(
        &mid_from.0,
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
    add_submodule(&top.0, &mid_from.0, "mid");
    git(
        &top.0,
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
            &isolated().view(&top.0, &|_| false).unwrap(),
            &reads_in(&top.0)
        ),
        None
    );
    git(
        &top.0.join("mid/inner"),
        &["config", "diff.external", "/tmp/evil"],
    );
    let why = hazard(
        &isolated().view(&top.0, &|_| false).unwrap(),
        &reads_in(&top.0),
    )
    .expect("nested");
    assert!(why.contains("diff.external"), "{why}");
}

/// A filter driver runs only on a path whose attributes select it: `git lfs
/// install` writes `filter.lfs.*` into every global config, and a repository
/// with no `filter=lfs` attribute never runs it — escalating on it would be the
/// blanket escalation the owner ruled out. The moment an attribute selects the
/// driver for a tracked path, it is a hazard.
#[test]
fn a_filter_driver_counts_only_where_the_attributes_select_it() {
    let r = repo("filter");
    std::fs::write(r.0.join("a.bin"), "x\n").unwrap();
    git(&r.0, &["add", "a.bin"]);
    let global = r.0.join("global.cfg");
    std::fs::write(
        &global,
        "[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\tsmudge = git-lfs smudge -- %f\n\tprocess = git-lfs filter-process\n",
    )
    .unwrap();
    let probe = WorkerEnv::hermetic(Some(&global));
    let v = probe.view(&r.0, &|_| false).expect("git config");
    assert_eq!(v.filters_selected, Some(Vec::new()), "{v:?}");
    assert_eq!(hazard(&v, &reads_in(&r.0)), None, "no path selects lfs");

    std::fs::write(r.0.join(".gitattributes"), "*.bin filter=lfs\n").unwrap();
    let v = probe.view(&r.0, &|_| false).expect("git config");
    assert_eq!(v.filters_selected, Some(vec!["lfs".to_string()]), "{v:?}");
    let why = hazard(&v, &reads_in(&r.0)).expect("selected");
    assert!(why.contains("filter.lfs."), "{why}");

    // A driver the attributes name but no config defines runs nothing, and
    // a configured one no path selects stays quiet beside a selected one.
    let only_other = repo("filter-other");
    std::fs::write(only_other.0.join("a.bin"), "x\n").unwrap();
    std::fs::write(only_other.0.join(".gitattributes"), "*.bin filter=other\n").unwrap();
    git(&only_other.0, &["add", "."]);
    git(&only_other.0, &["config", "filter.x.clean", "touch /tmp/p"]);
    assert_eq!(
        hazard(
            &isolated().view(&only_other.0, &|_| false).unwrap(),
            &reads_in(&only_other.0)
        ),
        None
    );
    git(
        &only_other.0,
        &["config", "filter.other.clean", "touch /tmp/p"],
    );
    assert!(
        hazard(
            &isolated().view(&only_other.0, &|_| false).unwrap(),
            &reads_in(&only_other.0)
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
    std::fs::write(r.0.join("f"), "x\n").unwrap();
    git(&r.0, &["add", "f"]);
    let ran = r.0.join("RAN");
    let script = r.0.join("fsm.sh");
    std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &r.0,
        &["config", "core.fsmonitor", script.to_str().unwrap()],
    );
    // A configured filter too, so the attribute read would be wanted.
    git(&r.0, &["config", "filter.x.clean", "cat"]);
    let v = isolated().view(&r.0, &|_| false).expect("git config");
    assert!(hazard(&v, &reads_in(&r.0)).is_some_and(|w| w.contains("core.fsmonitor")));
    assert!(
        !ran.exists(),
        "the check ran the fsmonitor it was checking for"
    );
    // NEGATIVE CONTROL: the probe is live — a real status does run it.
    git(&r.0, &["status", "--short"]);
    assert!(
        ran.exists(),
        "the fixture's fsmonitor never runs, so the check proves nothing"
    );
}

// -- the worker's environment ---------------------------------------------------

/// The worker's own `GIT_CONFIG_*` is configuration its git reads load
/// (scope `command`): the view is read in the worker's environment.
/// NEGATIVE CONTROL: the same repository, the worker without it.
#[test]
fn the_workers_git_config_variables_are_seen() {
    let r = repo("worker-env");
    assert_eq!(
        hazard(&isolated().view(&r.0, &|_| false).unwrap(), &reads_in(&r.0)),
        None
    );
    let worker = isolated()
        .with("GIT_CONFIG_VALUE_0=./evil.sh")
        .with("GIT_CONFIG_KEY_0=core.fsmonitor")
        .with("GIT_CONFIG_COUNT=1");
    let why = hazard(&worker.view(&r.0, &|_| false).unwrap(), &reads_in(&r.0))
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
    let why = hazard(&worker.view(&r.0, &|_| false).unwrap(), &reads_in(&r.0)).expect("a hazard");
    assert!(why.contains("GIT_EXTERNAL_DIFF"), "{why}");
    let worker = isolated().with("GIT_EXTERNAL_DIFF=");
    assert_eq!(
        hazard(&worker.view(&r.0, &|_| false).unwrap(), &reads_in(&r.0)),
        None
    );
}

/// `GIT_CONFIG` names the one file `git config` alone reads instead of the
/// effective set — passed on, the probe's own `git config --list` would list
/// that file and miss what a read loads — and `GIT_TRACE*` would have the
/// probe write; neither is passed on. CONTROL: `GIT_CONFIG` really does hide
/// the repository's config from a plain `git config --list`.
#[test]
fn what_only_git_config_reads_does_not_hide_the_effective_config() {
    let r = repo("git-config-var");
    git(&r.0, &["config", "core.fsmonitor", "./evil.sh"]);
    let empty = r.0.join("empty.cfg");
    std::fs::write(&empty, "").unwrap();
    let hidden = Command::new("git")
        .args(["config", "--list"])
        .current_dir(&r.0)
        .env("GIT_CONFIG", &empty)
        .output()
        .expect("git config");
    assert!(
        !String::from_utf8_lossy(&hidden.stdout).contains("core.fsmonitor"),
        "the control: GIT_CONFIG hides the repository's config"
    );
    let trace = r.0.join("trace.log");
    let worker = isolated()
        .with(&format!("GIT_CONFIG={}", empty.display()))
        .with(&format!("GIT_TRACE={}", trace.display()));
    assert!(
        hazard(&worker.view(&r.0, &|_| false).unwrap(), &reads_in(&r.0))
            .is_some_and(|w| w.contains("core.fsmonitor"))
    );
    assert!(!trace.exists(), "the probe wrote a trace");
}

/// The probe runs the git the WORKER's `PATH` finds — a git whose own
/// (compiled-in) configuration differs from this process's is read as it
/// is. Here a wrapper that adds a configuration stands in for such a git.
/// NEGATIVE CONTROL: the worker without it on its `PATH`.
#[cfg(unix)]
#[test]
fn the_git_on_the_workers_path_is_the_one_read() {
    use std::os::unix::fs::PermissionsExt;
    let r = repo("which-git");
    let real = isolated()
        .git_program(&|_| false)
        .expect("a git on this PATH");
    let bin = Scratch::new("bin");
    let wrapper = bin.0.join("git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec '{}' -c core.fsmonitor=./evil.sh \"$@\"\n",
            real.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "PATH={}:{}",
        bin.0.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let why = hazard(
        &isolated().with(&path).view(&r.0, &|_| false).unwrap(),
        &reads_in(&r.0),
    )
    .expect("the worker's git");
    assert!(why.contains("core.fsmonitor"), "{why}");
    assert_eq!(
        hazard(&isolated().view(&r.0, &|_| false).unwrap(), &reads_in(&r.0)),
        None
    );
}

/// Which git runs is refused when the worker's `PATH` cannot say: a relative
/// (or empty) entry ahead of git — the shell resolves it against the
/// directory the command runs in, a repository the worker may write — no
/// `PATH` at all, and no git on it.
#[test]
fn a_worker_path_that_cannot_name_the_git_is_refused() {
    let r = repo("bad-path");
    for path in [
        "PATH=relative/bin:/usr/bin",
        "PATH=:/usr/bin",
        "PATH=/nonexistent-aterm-git-dir",
    ] {
        assert!(
            isolated().with(path).view(&r.0, &|_| false).is_err(),
            "{path}"
        );
    }
    assert!(
        WorkerEnv::default().view(&r.0, &|_| false).is_err(),
        "no PATH"
    );
}

/// The first entry of a variable is the one `getenv` answers, and the one the
/// probe passes on.
#[test]
fn the_first_entry_of_a_variable_wins() {
    let env = WorkerEnv::from_env(vec![
        "CLAUDE_CONFIG_DIR=/a".to_string(),
        "CLAUDE_CONFIG_DIR=/b".to_string(),
        "HOME=/h".to_string(),
    ]);
    assert_eq!(env.var("CLAUDE_CONFIG_DIR"), Some("/a"));
    assert_eq!(env.claude_dir(), Some(PathBuf::from("/a")));
    let home_only = WorkerEnv::from_env(vec!["HOME=/h".to_string()]);
    assert_eq!(home_only.claude_dir(), Some(PathBuf::from("/h/.claude")));
    assert_eq!(WorkerEnv::default().claude_dir(), None);
}
