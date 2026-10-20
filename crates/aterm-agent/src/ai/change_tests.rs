// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::facts::LiveRunner;

/// Git for the fixtures, kept off the developer's own configuration: no global or system
/// config, no signing, a scratch HOME.
fn git(dir: &Path, args: &[&str]) {
    git_home(dir, args, dir);
}

fn git_home(dir: &Path, args: &[&str], home: &Path) {
    let mut cmd = aterm_uds::spawnfd::command("git");
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir);
    let out = aterm_uds::deadline::output_until(
        &mut cmd,
        std::time::Instant::now() + Duration::from_secs(60),
    )
    .expect("git ran");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Repo {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
}

fn repo() -> Repo {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("root");
    git(&root, &["init", "-q"]);
    std::fs::write(root.join("a.rs"), "fn a() {}\n").expect("a");
    git(&root, &["add", "a.rs"]);
    git(&root, &["commit", "-q", "-m", "a"]);
    Repo { _dir: dir, root }
}

fn take(r: &Repo, base: &Base) -> Result<Change, Refused> {
    take_home(r, base, None)
}

/// The change, with `home` as HOME (whose `.gitconfig` the review's git reads, as it would
/// the person's).
fn take_home(r: &Repo, base: &Base, home: Option<&Path>) -> Result<Change, Refused> {
    let path = std::env::var("PATH").ok();
    let home = home.map(|h| h.display().to_string());
    let var = move |k: &str| match k {
        "PATH" => path.clone(),
        "HOME" => home.clone(),
        _ => None,
    };
    collect(&LiveRunner, &var, &r.root, base, None)
}

#[test]
fn the_uncommitted_change_and_untracked_files_are_sent_and_secrets_held_back() {
    let r = repo();
    std::fs::write(r.root.join("a.rs"), "fn a() { b() }\n").expect("edit");
    std::fs::write(r.root.join("new.rs"), "fn b() {}\n").expect("new");
    std::fs::write(r.root.join(".env"), "TOKEN=hunter2\n").expect("env");
    std::fs::create_dir_all(r.root.join(".ssh")).expect("ssh");
    std::fs::write(r.root.join(".ssh/id_ed25519"), "key\n").expect("key");
    let c = take(&r, &Base::Branch).expect("change");
    assert!(c.diff.contains("+fn a() { b() }"), "{}", c.diff);
    assert!(
        c.diff.contains("+++ b/new.rs") && c.diff.contains("+fn b() {}"),
        "{}",
        c.diff
    );
    assert!(
        !c.diff.contains("hunter2") && !c.diff.contains("key\n+"),
        "{}",
        c.diff
    );
    let mut held = c.held_back.clone();
    held.sort();
    assert_eq!(held, [".env", ".ssh/id_ed25519"]);
    assert_eq!(c.files, 2);
}

#[test]
fn a_base_ref_sends_the_change_since_the_merge_base() {
    let r = repo();
    git(&r.root, &["checkout", "-q", "-b", "work"]);
    std::fs::write(r.root.join("b.rs"), "fn b() {}\n").expect("b");
    git(&r.root, &["add", "b.rs"]);
    git(&r.root, &["commit", "-q", "-m", "b"]);
    let c = take(&r, &Base::Ref("main".into())).expect("change");
    assert!(c.diff.contains("+++ b/b.rs"), "{}", c.diff);
    assert!(!c.diff.contains("a.rs"), "{}", c.diff);
    assert!(
        matches!(take(&r, &Base::Uncommitted), Err(Refused(why)) if why.starts_with("no change"))
    );
    assert!(matches!(
        take(&r, &Base::Ref("--output=/tmp/x".into())),
        Err(Refused(_))
    ));
}

#[test]
fn a_repository_that_says_no_or_configures_a_filter_is_refused() {
    let r = repo();
    std::fs::write(r.root.join("a.rs"), "fn a() { 1 }\n").expect("edit");
    git(&r.root, &["config", "aterm.aiReview", "false"]);
    assert!(
        matches!(take(&r, &Base::Uncommitted), Err(Refused(why)) if why.contains("aterm.aiReview"))
    );
    git(&r.root, &["config", "--unset", "aterm.aiReview"]);
    git(&r.root, &["config", "filter.x.clean", "touch /tmp/never"]);
    assert!(
        matches!(take(&r, &Base::Uncommitted), Err(Refused(why)) if why.contains("filter driver"))
    );
    git(&r.root, &["config", "--remove-section", "filter.x"]);
    assert!(
        take(&r, &Base::Uncommitted).is_ok(),
        "the control: with neither, it is sent"
    );
}

#[test]
fn outside_a_repository_nothing_is_sent() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = std::env::var("PATH").ok();
    let var = move |k: &str| if k == "PATH" { path.clone() } else { None };
    let got = collect(&LiveRunner, &var, dir.path(), &Base::Branch, None);
    assert!(matches!(got, Err(Refused(why)) if why.contains("not inside a git repository")));
}

#[test]
fn the_secret_names_are_matched_by_component_and_basename() {
    let rules = crate::supervise::policy::approval::default_secrets(None);
    for name in [
        ".env",
        "config/.env.local",
        "deploy/id_rsa",
        ".aws/credentials",
        "x/server.pem",
    ] {
        assert!(looks_secret(Path::new("/r"), name, &rules), "{name}");
    }
    for name in ["src/env.rs", "README.md", "keys.rs"] {
        assert!(!looks_secret(Path::new("/r"), name, &rules), "{name}");
    }
}

#[test]
fn an_untracked_link_is_held_back_and_never_read_through() {
    let r = repo();
    let outside = r._dir.path().join("private.md");
    std::fs::write(&outside, "PRIVATE-MARKER\n").expect("outside");
    std::fs::write(r.root.join("new.rs"), "fn n() {}\n").expect("new");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, r.root.join("notes.md")).expect("link");
    let c = take(&r, &Base::Uncommitted).expect("change");
    assert!(!c.diff.contains("PRIVATE-MARKER"), "{}", c.diff);
    // Making a link takes a privilege on Windows: there the plain-file control alone runs.
    #[cfg(unix)]
    assert_eq!(
        c.links,
        ["notes.md"],
        "the link is named, apart from the secrets"
    );
    assert!(
        c.diff.contains("+fn n() {}"),
        "the control: a plain file is sent"
    );
}

#[test]
fn the_opt_out_is_read_as_git_reads_a_boolean_and_fails_closed() {
    let r = repo();
    std::fs::write(r.root.join("a.rs"), "fn a() { 2 }\n").expect("edit");
    for word in ["no", "off", "0", "FALSE"] {
        git(&r.root, &["config", "aterm.aiReview", word]);
        assert!(
            matches!(take(&r, &Base::Uncommitted), Err(Refused(why)) if why.contains("says no")),
            "{word}"
        );
    }
    git(&r.root, &["config", "aterm.aiReview", "maybe"]);
    assert!(
        matches!(take(&r, &Base::Uncommitted), Err(Refused(why)) if why.contains("could not be read"))
    );
    git(&r.root, &["config", "aterm.aiReview", "yes"]);
    assert!(
        take(&r, &Base::Uncommitted).is_ok(),
        "the control: yes sends"
    );
}

#[test]
fn only_the_repositorys_own_filter_driver_refuses() {
    let r = repo();
    std::fs::write(r.root.join("a.rs"), "fn a() { 3 }\n").expect("edit");
    let home = r._dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join(".gitconfig"),
        "[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\trequired = true\n",
    )
    .expect("gitconfig");
    assert!(
        take_home(&r, &Base::Uncommitted, Some(&home)).is_ok(),
        "the person's own git-lfs"
    );
    git(&r.root, &["config", "filter.x.clean", "touch /tmp/never"]);
    assert!(
        matches!(take_home(&r, &Base::Uncommitted, Some(&home)), Err(Refused(why)) if why.contains("filter.x.clean"))
    );
}

/// The person's own (global) `aterm.aiReview = false` refuses too, read from their home as
/// git finds it on this host: `HOME`, or on Windows `USERPROFILE` (where no `HOME` is set).
/// Negative control: the same home without the key sends.
#[test]
fn a_global_opt_out_in_the_persons_home_refuses() {
    let r = repo();
    std::fs::write(r.root.join("a.rs"), "fn a() { 6 }\n").expect("edit");
    let home = r._dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let take_in = |home: &Path| {
        let path = std::env::var("PATH").ok();
        let home = home.display().to_string();
        let var = move |k: &str| match k {
            "PATH" => path.clone(),
            k if k == key => Some(home.clone()),
            _ => None,
        };
        collect(&LiveRunner, &var, &r.root, &Base::Uncommitted, None)
    };
    assert!(take_in(&home).is_ok(), "the control: no opt-out sends");
    std::fs::write(home.join(".gitconfig"), "[aterm]\n\taiReview = false\n").expect("gitconfig");
    assert!(
        matches!(take_in(&home), Err(Refused(why)) if why.contains("says no")),
        "the person's global opt-out"
    );
}

#[test]
fn a_git_the_repository_carries_is_never_run() {
    let r = repo();
    let bin = r.root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    for name in ["git", "git.exe"] {
        std::fs::write(bin.join(name), "#!/bin/sh\n").expect("git");
    }
    let sub = r.root.join("src");
    std::fs::create_dir_all(&sub).expect("src");
    // The carried bin first, then the host's own PATH (where the fixtures' git is).
    let host = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&host)))
            .expect("PATH");
    let got = git_program(path.to_str(), &sub).expect("a git");
    assert!(!got.starts_with(&r.root), "{}", got.display());
    assert_eq!(repo_roots_of(&sub).first(), Some(&real(&r.root)));
}

#[cfg(unix)]
#[test]
fn a_git_link_cannot_hide_its_repository_target() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let r = repo();
    let trusted_bin = r._dir.path().join("trusted-bin");
    let linked_bin = r._dir.path().join("linked-bin");
    std::fs::create_dir(&trusted_bin).expect("trusted bin");
    std::fs::create_dir(&linked_bin).expect("linked bin");
    let trusted = trusted_bin.join("git");
    let carried = r.root.join("carried-git");
    for program in [&trusted, &carried] {
        std::fs::write(program, "#!/bin/sh\nexit 0\n").expect("stand-in");
        std::fs::set_permissions(program, std::fs::Permissions::from_mode(0o700))
            .expect("executable stand-in");
    }
    let path = std::env::join_paths([&trusted_bin]).expect("trusted PATH");
    let got = git_program(path.to_str(), &r.root).expect("trusted git");
    assert_eq!(
        real(&got),
        real(&trusted),
        "an ordinary outside file is allowed"
    );

    let link = linked_bin.join("git");
    symlink(&trusted, &link).expect("outside target");
    let path = std::env::join_paths([&linked_bin, &trusted_bin]).expect("linked PATH");
    let got = git_program(path.to_str(), &r.root).expect("outside linked git");
    assert_eq!(real(&got), real(&trusted), "an outside target is allowed");

    std::fs::remove_file(&link).expect("replace owned link");
    symlink(&carried, &link).expect("repository target");
    let got = git_program(path.to_str(), &r.root).expect("trusted fallback");
    assert_eq!(
        real(&got),
        real(&trusted),
        "an outside PATH directory cannot authorize a git inside the repository"
    );
    assert_eq!(
        git_program(linked_bin.to_str(), &r.root),
        None,
        "a carried target alone offers no trusted fallback"
    );

    // An ordinary but non-executable shadow must also leave the trusted
    // executable reachable through the next PATH entry.
    std::fs::remove_file(&link).expect("remove owned link");
    std::fs::write(&link, "not executable").expect("non-executable shadow");
    std::fs::set_permissions(&link, std::fs::Permissions::from_mode(0o600))
        .expect("non-executable mode");
    let got = git_program(path.to_str(), &r.root).expect("trusted executable fallback");
    assert_eq!(real(&got), real(&trusted));
}

#[test]
fn incomplete_git_observations_cannot_admit_a_change() {
    use crate::ai::testing::{FakeRunner, ran};

    let r = repo();
    let path = std::env::var("PATH").ok();
    let var = move |k: &str| if k == "PATH" { path.clone() } else { None };
    for failure in ["capture limit", "link listing"] {
        let runner = FakeRunner::with(move |call| {
            Ok(if call.args.iter().any(|a| a == "--show-toplevel") {
                ran(0, "/r\n", "")
            } else if call.args.iter().any(|a| a == "--get-regexp") {
                ran(1, "", "")
            } else if call.args.iter().any(|a| a == "ls-files") {
                if !call.args.iter().any(|a| a == "-s") {
                    ran(0, "", "")
                } else if failure == "link listing" {
                    ran(128, "", "index could not be read")
                } else {
                    ran(0, &"x".repeat(super::super::facts::OUTPUT_KEEP), "")
                }
            } else if call.args.iter().any(|a| a == "--name-only") {
                ran(0, "a.rs\0", "")
            } else if call.args.iter().any(|a| a == "diff") {
                ran(0, "diff --git a/a.rs b/a.rs\n+changed\n", "")
            } else if call.args.iter().any(|a| a == "HEAD") {
                ran(0, "head\n", "")
            } else {
                ran(1, "", "")
            })
        });
        let got = collect(&runner, &var, &r.root, &Base::Uncommitted, None);
        assert!(matches!(got, Err(Refused(_))), "{failure}: {got:?}");
    }
}

#[test]
fn a_repository_cannot_pass_its_driver_off_as_the_persons() {
    let r = repo();
    std::fs::write(r.root.join("a.rs"), "fn a() { 4 }\n").expect("edit");
    let home = r._dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join(".gitconfig"),
        "[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n",
    )
    .expect("gitconfig");
    // The same key in the repository's own config is the repository's.
    git(&r.root, &["config", "filter.lfs.clean", "touch /tmp/never"]);
    assert!(
        matches!(take_home(&r, &Base::Uncommitted, Some(&home)), Err(Refused(why)) if why.contains("filter.lfs.clean"))
    );
}

#[test]
fn a_filter_guard_that_cannot_read_its_answer_refuses() {
    use crate::ai::testing::{FakeRunner, ran};
    let r = repo();
    // An old git that knows no `--show-origin` (or any failure): exit 129.
    let runner = FakeRunner::with(|call| {
        Ok(if call.args.iter().any(|a| a == "--show-toplevel") {
            ran(0, "/r\n", "")
        } else if call.args.iter().any(|a| a == "--show-origin") {
            ran(129, "", "error: unknown option")
        } else {
            ran(1, "", "")
        })
    });
    let path = std::env::var("PATH").ok();
    let var = move |k: &str| if k == "PATH" { path.clone() } else { None };
    let got = collect(&runner, &var, &r.root, &Base::Uncommitted, None);
    assert!(
        matches!(&got, Err(Refused(why)) if why.contains("could not be read")),
        "{got:?}"
    );
}

#[test]
fn a_secret_place_counts_inside_a_repository_never_around_it() {
    use crate::supervise::policy::approval::SecretRule;
    let rules = [
        SecretRule::Under(PathBuf::from("/home/me/Library")),
        SecretRule::Under(PathBuf::from("/home/me/.config/gh")),
    ];
    // A dotfiles repository at $HOME holds the places.
    assert!(looks_secret(
        Path::new("/home/me"),
        ".config/gh/hosts.yml",
        &rules
    ));
    // A repository inside a cloud folder under ~/Library is not secret for that.
    assert!(!looks_secret(
        Path::new("/home/me/Library/CloudStorage/Box/proj"),
        "src/main.rs",
        &rules
    ));
}

#[cfg(unix)]
#[test]
fn a_tracked_link_is_named_too() {
    let r = repo();
    std::os::unix::fs::symlink("a.rs", r.root.join("alias.rs")).expect("link");
    git(&r.root, &["add", "alias.rs"]);
    git(&r.root, &["commit", "-q", "-m", "link"]);
    std::fs::write(r.root.join("a.rs"), "fn a() { 5 }\n").expect("edit");
    let c = take(&r, &Base::Uncommitted).expect("change");
    assert_eq!(c.links, ["alias.rs"]);

    // A working-tree link matters before it is staged into the index as a link.
    let outside = r._dir.path().join("private.md");
    std::fs::write(&outside, "PRIVATE-MARKER\n").expect("outside");
    std::fs::remove_file(r.root.join("a.rs")).expect("remove tracked file");
    std::os::unix::fs::symlink(&outside, r.root.join("a.rs")).expect("unstaged link");
    let c = take(&r, &Base::Uncommitted).expect("link change");
    assert_eq!(c.links, ["a.rs", "alias.rs"]);
    assert!(!c.diff.contains("PRIVATE-MARKER"));
}

#[test]
fn an_unborn_head_repository_includes_staged_files_and_counts_emitted_hunks() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("unborn");
    std::fs::create_dir_all(&root).expect("root");
    git(&root, &["init", "-q"]);
    std::fs::write(root.join("staged.rs"), "fn staged() {}\n").expect("staged");
    git(&root, &["add", "staged.rs"]);
    std::fs::write(root.join("untracked.rs"), "fn untracked() {}\n").expect("untracked");
    std::fs::write(root.join("blob.bin"), b"\x00\x01\x02").expect("binary");
    let r = Repo { _dir: dir, root };
    let c = take(&r, &Base::Uncommitted).expect("unborn change");
    assert!(
        c.diff.contains("+++ b/staged.rs") && c.diff.contains("+fn staged() {}"),
        "{}",
        c.diff
    );
    assert!(
        c.diff.contains("+++ b/untracked.rs") && c.diff.contains("+fn untracked() {}"),
        "{}",
        c.diff
    );
    assert!(!c.diff.contains("blob.bin"), "{}", c.diff);
    assert_eq!(c.files, 2);
}

#[cfg(unix)]
fn root_whitespace_keeps_the_selected_repository(suffix: &str) {
    let mut r = repo();
    std::fs::write(r.root.join("a.rs"), "fn neighbor_change() {}\n").expect("neighbor edit");
    let control = take(&r, &Base::Uncommitted).expect("plain repository");
    assert_eq!(control.root, real(&r.root));
    assert!(control.diff.contains("+fn neighbor_change() {}"));

    let selected = r.root.with_file_name(format!("repo{suffix}"));
    std::fs::create_dir(&selected).expect("selected repository");
    git(&selected, &["init", "-q"]);
    std::fs::write(selected.join("a.rs"), "fn a() {}\n").expect("initial file");
    git(&selected, &["add", "a.rs"]);
    git(&selected, &["commit", "-q", "-m", "a"]);
    std::fs::write(selected.join("a.rs"), "fn selected_change() {}\n").expect("selected edit");
    r.root = selected.clone();
    let change = take(&r, &Base::Uncommitted).expect("selected repository change");
    assert_eq!(
        (
            change.root,
            change.diff.contains("+fn selected_change() {}"),
            change.diff.contains("+fn neighbor_change() {}"),
        ),
        (real(&selected), true, false),
        "the selected repository's path and change must remain together"
    );
}

#[cfg(unix)]
#[test]
fn a_repository_root_ending_in_space_keeps_its_change() {
    root_whitespace_keeps_the_selected_repository(" ");
}

#[cfg(unix)]
#[test]
fn a_repository_root_ending_in_newlines_keeps_its_change() {
    root_whitespace_keeps_the_selected_repository("\n\n");
}
