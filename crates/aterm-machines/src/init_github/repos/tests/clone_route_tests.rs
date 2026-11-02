// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const URL: &str = "https://github.com/alice/new.git";
const SSH: &str = "git@github.com:alice/new.git";
const LEGACY: &str = "[url \"git@github.com:\"]\n\tinsteadOf = https://github.com/\n";

fn repo() -> Repo {
    Repo {
        owner: "alice".into(),
        name: "new".into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
    }
}

fn isolated<'s>(sys: &'s Live, config: &str) -> (Scratch<'s>, User, Cmd) {
    crate::items::github::need_clt_git();
    let scratch = Scratch::new(sys, "github-clone-route").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = scratch.path().to_string_lossy().into_owned();
    let path = scratch.path().join("gitconfig");
    sys.write_file(&path, config.as_bytes(), 0o600).unwrap();
    let command = git(sys, &who, None)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.to_str().unwrap());
    (scratch, who, command)
}

/// Real Git resolves both the broad legacy rewrite and the command-local exact override.
/// Every Git invocation here is --get-url, which performs no network or credential lookup.
#[test]
fn the_real_git_route_override_is_exact_and_preserves_configuration() {
    let sys = Live::new();
    let config = format!(
        "[user]\n\tname = Retained\n{LEGACY}\n[url \"ssh://git@github.com/alice/new.git/child\"]\n\tinsteadOf = {URL}/child\n"
    );
    let (mut scratch, _who, command) = isolated(&sys, &config);
    assert_eq!(
        admit_endpoint(&sys, &command, "alice", &repo(), URL).unwrap(),
        SSH
    );
    let current = || Ok(());
    for guard in [None, Some(CloneGuard::current_only(&current))] {
        let unpinned =
            clone_command(&sys, command.clone(), "alice", &repo(), URL, guard.as_ref()).unwrap();
        assert_eq!(
            admit_endpoint(&sys, &unpinned, "alice", &repo(), URL).unwrap(),
            SSH
        );
    }
    let guard = CloneGuard::pinned("alice", &current);
    let scoped = clone_command(&sys, command.clone(), "alice", &repo(), URL, Some(&guard)).unwrap();
    assert_eq!(
        admit_endpoint(&sys, &scoped, "alice", &repo(), URL).unwrap(),
        URL
    );
    let other = "https://github.com/alice/other.git";
    assert_eq!(
        text(
            &sys,
            &scoped.clone().args(["ls-remote", "--get-url", "--", other]),
            "fixture route"
        )
        .unwrap()
        .trim(),
        "git@github.com:alice/other.git"
    );
    let child = format!("{URL}/child");
    assert_eq!(
        text(
            &sys,
            &scoped
                .clone()
                .args(["ls-remote", "--get-url", "--", &child]),
            "fixture descendant route"
        )
        .unwrap()
        .trim(),
        "ssh://git@github.com/alice/new.git/child"
    );
    assert!(admit_endpoint(&sys, &scoped, "alice", &repo(), &child).is_err());
    assert_eq!(
        std::fs::read(scratch.path().join("gitconfig")).unwrap(),
        config.as_bytes()
    );
    scratch.close().unwrap();
}

#[test]
fn real_git_conflicting_or_foreign_routes_do_not_gain_a_credential_route() {
    for destination in [
        SSH,
        "https://github.com/bob/new.git",
        "https://token@github.com/alice/new.git",
        "https://elsewhere.invalid/alice/new.git",
    ] {
        let sys = Live::new();
        let config = format!("{LEGACY}\n[url \"{destination}\"]\n\tinsteadOf = {URL}\n");
        let (mut scratch, _who, command) = isolated(&sys, &config);
        let current = || Ok(());
        let guard = CloneGuard::pinned("alice", &current);
        assert!(clone_command(&sys, command, "alice", &repo(), URL, Some(&guard)).is_err());
        assert_eq!(
            std::fs::read(scratch.path().join("gitconfig")).unwrap(),
            config.as_bytes()
        );
        scratch.close().unwrap();
    }
}

#[test]
fn a_new_foreign_base_route_is_not_hidden_by_an_old_scoped_override() {
    let sys = Live::new();
    let (mut scratch, _who, command) = isolated(&sys, LEGACY);
    let current = || Ok(());
    let guard = CloneGuard::pinned("alice", &current);
    assert!(clone_command(&sys, command.clone(), "alice", &repo(), URL, Some(&guard)).is_ok());
    let changed = "[url \"https://elsewhere.invalid/\"]\n\tinsteadOf = https://github.com/\n";
    sys.write_file(&scratch.path().join("gitconfig"), changed.as_bytes(), 0o600)
        .unwrap();
    assert!(clone_command(&sys, command, "alice", &repo(), URL, Some(&guard)).is_err());
    assert_eq!(
        std::fs::read(scratch.path().join("gitconfig")).unwrap(),
        changed.as_bytes()
    );
    scratch.close().unwrap();
}

#[test]
fn pinned_ssh_clones_use_only_the_readmitted_https_commands() {
    let (sys, who, _) = fixture();
    let current = || Ok(());
    let guard = CloneGuard::pinned("alice", &current);
    let (repo, _, _) = script_clone_named_route(&sys, &who, "new", 0, false, Some(&guard), true);
    clone_one_guarded(&sys, &who, &repo, "alice", &current).unwrap();
    let calls = sys.calls();
    let network: Vec<_> = calls
        .iter()
        .filter(|call| call.contains("--symref") || call.contains(" clone "))
        .collect();
    assert_eq!(network.len(), 2);
    for call in network {
        assert!(
            call.contains(&format!("url.{URL}.insteadOf={URL}")),
            "{call}"
        );
        assert!(
            call.contains(".helper=osxkeychain") && call.contains(".username=alice"),
            "{call}"
        );
    }
    assert!(calls.iter().all(|call| !call.contains("--global")));
}

#[test]
fn the_current_check_precedes_each_final_route_admission() {
    for changed_before_first in [false, true] {
        let (sys, who, _) = fixture();
        let current = || {
            if changed_before_first || sys.calls().iter().any(|call| call.contains("--symref")) {
                Err("fixture approval withdrawn".into())
            } else {
                Ok(())
            }
        };
        let guard = CloneGuard::pinned("alice", &current);
        let (repo, _, _) =
            script_clone_named_route(&sys, &who, "new", 0, false, Some(&guard), true);
        assert!(clone_one_guarded(&sys, &who, &repo, "alice", &current).is_err());
        let calls = sys.calls();
        assert!(!calls.iter().any(|call| call.contains(" clone ")));
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.contains("--symref"))
                .count(),
            usize::from(!changed_before_first)
        );
        assert!(sys.stat(&Path::new(&who.home).join("new")).is_err());
    }
}

#[test]
fn a_route_changed_after_listing_is_refused_before_the_clone() {
    let (sys, who, _) = fixture();
    let base = git(&sys, &who, None)
        .args(["ls-remote", "--get-url", "--", URL])
        .display();
    sys.on(&base, Ran::ok(SSH));
    sys.on(&base, Ran::ok("https://elsewhere.invalid/alice/new.git"));
    let current = || Ok(());
    let guard = CloneGuard::pinned("alice", &current);
    let (repo, _, _) = script_clone_named_route(&sys, &who, "new", 0, false, Some(&guard), true);
    assert!(clone_one_guarded(&sys, &who, &repo, "alice", &current).is_err());
    let calls = sys.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.contains("--symref"))
            .count(),
        1
    );
    assert!(!calls.iter().any(|call| call.contains(" clone ")));
    assert!(sys.stat(&Path::new(&who.home).join("new")).is_err());
}
