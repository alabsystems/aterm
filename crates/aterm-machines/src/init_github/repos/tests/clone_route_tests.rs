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

/// Real Git resolves the person's own rewrites, and a clone's command keeps them as they are:
/// the one thing it adds is aterm's credential route. Every Git invocation here is --get-url,
/// which performs no network or credential lookup.
#[test]
fn the_real_git_route_keeps_the_persons_rewrites_and_configuration() {
    let sys = Live::new();
    let config = format!(
        "[user]\n\tname = Retained\n{LEGACY}\n[url \"ssh://git@github.com/alice/new.git/child\"]\n\tinsteadOf = {URL}/child\n"
    );
    let (mut scratch, who, command) = isolated(&sys, &config);
    assert_eq!(
        admit_endpoint(&sys, &command, "alice", &repo(), URL).unwrap(),
        SSH
    );
    let admitted = clone_command(&sys, &who, command.clone(), "alice", &repo(), URL).unwrap();
    assert_eq!(
        admitted.display(),
        crate::git_credential::pinned(command.clone(), None).display()
    );
    let child = format!("{URL}/child");
    assert!(admit_endpoint(&sys, &command, "alice", &repo(), &child).is_err());
    assert_eq!(
        std::fs::read(scratch.path().join("gitconfig")).unwrap(),
        config.as_bytes()
    );
    scratch.close().unwrap();
}

/// A rewrite to the same repository over SSH is the person's route and is admitted; one to
/// another repository, a URL carrying a credential or another host is refused before any
/// network step (the controls).
#[test]
fn real_git_foreign_routes_are_refused_before_any_network_step() {
    for (destination, admitted) in [
        (SSH, true),
        ("https://github.com/bob/new.git", false),
        ("https://token@github.com/alice/new.git", false),
        ("https://elsewhere.invalid/alice/new.git", false),
    ] {
        let sys = Live::new();
        let config = format!("{LEGACY}\n[url \"{destination}\"]\n\tinsteadOf = {URL}\n");
        let (mut scratch, who, command) = isolated(&sys, &config);
        assert_eq!(
            clone_command(&sys, &who, command, "alice", &repo(), URL).is_ok(),
            admitted,
            "{destination}"
        );
        assert_eq!(
            std::fs::read(scratch.path().join("gitconfig")).unwrap(),
            config.as_bytes()
        );
        scratch.close().unwrap();
    }
}

/// A route admitted once is read again: a base rewrite to another host written since is
/// refused.
#[test]
fn a_new_foreign_base_route_is_refused_at_the_next_admission() {
    let sys = Live::new();
    let (mut scratch, who, command) = isolated(&sys, LEGACY);
    assert!(clone_command(&sys, &who, command.clone(), "alice", &repo(), URL).is_ok());
    let changed = "[url \"https://elsewhere.invalid/\"]\n\tinsteadOf = https://github.com/\n";
    sys.write_file(&scratch.path().join("gitconfig"), changed.as_bytes(), 0o600)
        .unwrap();
    assert!(clone_command(&sys, &who, command, "alice", &repo(), URL).is_err());
    assert_eq!(
        std::fs::read(scratch.path().join("gitconfig")).unwrap(),
        changed.as_bytes()
    );
    scratch.close().unwrap();
}

/// A clone whose URL is the person's SSH rewrite runs it unchanged: aterm's credential route is
/// the one thing on either network command, and no URL override and no keychain helper are.
#[test]
fn an_ssh_rewrite_clones_with_the_persons_own_url() {
    let (sys, who, _) = fixture();
    let current = || Ok(());
    let (repo, _, _) = script_clone_named_route(&sys, &who, "new", 0, false, true);
    clone_one_guarded(&sys, &who, &repo, &current).unwrap();
    let calls = sys.calls();
    let network: Vec<_> = calls
        .iter()
        .filter(|call| call.contains("--symref") || call.contains(" clone "))
        .collect();
    assert_eq!(network.len(), 2);
    for call in network {
        assert!(
            call.contains("-c credential.helper= ")
                && !call.contains("insteadOf")
                && !call.contains("osxkeychain"),
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
        let (repo, _, _) = script_clone_named_route(&sys, &who, "new", 0, false, true);
        assert!(clone_one_guarded(&sys, &who, &repo, &current).is_err());
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
    let (repo, _, _) = script_clone_named_route(&sys, &who, "new", 0, false, true);
    assert!(clone_one_guarded(&sys, &who, &repo, &current).is_err());
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
