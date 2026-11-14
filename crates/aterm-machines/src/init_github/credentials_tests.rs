// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::git_credential::{GITHUB, script};
use crate::items::defaults::tests::user;
use crate::port::{Fake, Live, Scratch, Stat};

const DEV: &str = "fixture_dev_token";

#[test]
fn vault_replacement_stops_commands_when_the_person_turns_settings_off() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for after_write in [false, true] {
        let who = user("alice", 501);
        let sys = fake(&who);
        script::keychain_block(&sys, &who, "alice");
        script::keychain_block(&sys, &who, "alice");
        script::replaces_keychain_block(&sys, &who);
        let queries = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let cutoff = Arc::new(AtomicUsize::new(0));
        let all_commands = Arc::new(AtomicUsize::new(0));
        let commands = all_commands.clone();
        let (reads, stop, boundary) = (queries.clone(), stopped.clone(), cutoff.clone());
        let owner = who.clone();
        sys.simulate(move |sys, command| {
            let serial = commands.fetch_add(1, Ordering::SeqCst) + 1;
            let query = command.args.iter().any(|arg| arg == "--get-regexp");
            let write = command
                .args
                .iter()
                .any(|arg| arg == "--unset-all" || arg == "--add");
            let due = if after_write {
                write
            } else {
                query && reads.fetch_add(1, Ordering::SeqCst) == 1
            };
            if due && !stop.swap(true, Ordering::SeqCst) {
                profiled(sys, &owner, "[github]\nenabled=false\n");
                boundary.store(serial, Ordering::SeqCst);
            }
            None
        });
        let GithubRead::Due { revert, .. } = vault_git_credentials(&sys).unwrap() else {
            panic!("the valid control plans its replacement")
        };
        let result = sync_vault_git_credentials(&sys, &revert);
        assert!(stopped.load(Ordering::SeqCst));
        assert!(result.is_err());
        assert_eq!(
            all_commands.load(Ordering::SeqCst),
            cutoff.load(Ordering::SeqCst),
            "no command starts after Off: after_write={after_write}"
        );
    }
}

fn stat(who: &User, len: usize) -> Stat {
    Stat {
        uid: who.uid,
        gid: 20,
        mode: 0o600,
        dir: false,
        symlink: false,
        len: len as u64,
    }
}

/// `who`'s machine, with git and the launcher.
fn bare(who: &User) -> Fake {
    let mut sys = Fake::root();
    sys.euid = who.uid;
    sys.logged_in(who);
    sys.executable(crate::items::github::CLT_GIT, b"git");
    script::launcher(&sys, who);
    sys
}

/// [`bare`], with the vault's token.
fn fake(who: &User) -> Fake {
    let sys = bare(who);
    let path = token_file(Path::new(&who.home));
    sys.file(path.to_str().unwrap(), DEV.as_bytes())
        .stat_of(path.to_str().unwrap(), stat(who, DEV.len()));
    sys
}

fn profiled(sys: &Fake, who: &User, text: &str) {
    let path = Path::new(&who.home).join(".config/aterm/init.toml");
    sys.file(path.to_str().unwrap(), text.as_bytes())
        .stat_of(path.to_str().unwrap(), stat(who, text.len()));
}

/// The revert the read gave, the read being [`GithubRead::Due`].
fn due(sys: &Fake) -> String {
    match vault_git_credentials(sys).unwrap() {
        GithubRead::Due { from, revert } => {
            assert_eq!(from, KEYCHAIN_BLOCK);
            revert
        }
        other => panic!("{other:?}"),
    }
}

/// The commands `sys` ran, without the identity reads the fake also records.
fn commands(sys: &Fake) -> Vec<String> {
    let mut calls = sys.calls();
    calls.retain(|call| call != "identity");
    calls
}

/// No helper, GitHub request or token read reaches the calls.
fn settings_alone(sys: &Fake) {
    let calls = sys.calls();
    assert!(
        calls
            .iter()
            .all(|call| !call.contains("credential fill") && !call.contains("agh GET")),
        "{calls:?}"
    );
}

/// `[github] enabled = false` leaves git and the credentials alone.
#[test]
fn a_disabled_stage_reads_neither_git_nor_credentials() {
    let who = user("alice", 501);
    let sys = fake(&who);
    profiled(&sys, &who, "[github]\nenabled = false\n");
    assert!(matches!(
        vault_git_credentials(&sys).unwrap(),
        GithubRead::NotApplicable(_)
    ));
    assert!(
        sys.calls().iter().all(|call| !call.contains("git ")),
        "{:?}",
        sys.calls()
    );
}

/// The block goes in every mode but off, whichever token source init.toml names and whether
/// or not its account is the vault's: the read needs neither the token nor GitHub. With no
/// block, a custom source's lookups are its own item's to read.
#[test]
fn the_block_is_due_from_the_settings_alone_in_every_mode() {
    for source in [
        "",
        "[github]\naccount='alice'\nowner='team'\n",
        "[github]\naccount='someone-else'\ntoken_file='~/.secrets/gh_access_token'\n",
        "[github]\naccount='alice'\ntoken_file='~/.secrets/custom'\n",
        "[github]\naccount='alice'\ntoken_file='~/.secrets/custom'\ntoken_key='TOKEN'\n",
    ] {
        let who = user("alice", 501);
        let sys = bare(&who);
        if !source.is_empty() {
            profiled(&sys, &who, source);
        }
        script::keychain_block(&sys, &who, "alice");
        assert!(due(&sys).contains("osxkeychain"), "{source}");
        settings_alone(&sys);
    }
    let who = user("alice", 501);
    let sys = fake(&who);
    profiled(
        &sys,
        &who,
        "[github]\naccount='alice'\ntoken_file='~/.secrets/custom'\n",
    );
    script::alike(&sys, &who);
    assert!(matches!(
        vault_git_credentials(&sys).unwrap(),
        GithubRead::NotApplicable(_)
    ));
    settings_alone(&sys);
}

/// A section of GitHub CLI's helper can be a person's own, so the pass replaces it only where
/// the vault's token is here for aterm's helper to hand back: otherwise it asks nothing of
/// the section and writes nothing.
#[test]
fn a_gh_section_is_due_only_where_the_token_is_here() {
    let who = user("alice", 501);
    let sys = fake(&who);
    script::gh_setup_git_block(&sys, &who);
    assert!(matches!(
        vault_git_credentials(&sys).unwrap(),
        GithubRead::Due { .. }
    ));

    let sys = bare(&who);
    script::gh_setup_git_block(&sys, &who);
    assert!(matches!(
        vault_git_credentials(&sys).unwrap(),
        GithubRead::Deferred(_)
    ));
    settings_alone(&sys);
    assert!(
        sys.calls().iter().all(|call| !call.contains("--unset-all")),
        "{:?}",
        sys.calls()
    );
}

/// With no launcher there is no helper to put in the block's place, so the pass waits and
/// writes nothing: the block alone gone would turn the system's keychain helper back on.
#[test]
fn no_launcher_defers_the_block() {
    let who = user("alice", 501);
    let mut sys = Fake::root();
    sys.euid = who.uid;
    sys.logged_in(&who);
    sys.executable(crate::items::github::CLT_GIT, b"git");
    script::keychain_block(&sys, &who, "alice");
    let GithubRead::Deferred(why) = vault_git_credentials(&sys).unwrap() else {
        panic!("no launcher, no replacement");
    };
    assert!(why.contains("/Users//alice/.local/bin/aterm"), "{why}");
    assert_eq!(commands(&sys), [script::helpers(&who)]);
}

/// CLT's own keychain helper is not aterm's block: it is left as it is, and aterm's route
/// answers past it. Negative controls: no token defers, and a route that hands back another
/// token is no current one.
#[test]
fn with_no_block_the_read_asks_atermss_route_alone() {
    let who = user("alice", 501);
    let sys = fake(&who);
    script::unconfigured(&sys, &who);
    script::lookups_hold(&sys, &who, GITHUB, "alice", DEV);
    assert!(matches!(
        vault_git_credentials(&sys).unwrap(),
        GithubRead::Current(_)
    ));
    let calls = commands(&sys);
    assert_eq!(calls, [script::helpers(&who), script::fill(&who)]);
    assert!(
        !calls.iter().any(|call| call.contains("agh GET")),
        "{calls:?}"
    );

    let sys = bare(&who);
    script::alike(&sys, &who);
    assert!(matches!(
        vault_git_credentials(&sys).unwrap(),
        GithubRead::Deferred(_)
    ));
    settings_alone(&sys);

    let sys = fake(&who);
    script::alike(&sys, &who);
    script::lookups_hold(&sys, &who, GITHUB, "alice", "fixture_another_token");
    let GithubRead::NotApplicable(why) = vault_git_credentials(&sys).unwrap() else {
        panic!("another token is no current route");
    };
    assert!(!why.contains("fixture"), "{why}");
}

/// The pass replaces the block whose revert the engine journaled, from the settings alone.
/// Negative controls: a block that changed since, a stage turned off since and a block
/// already gone write nothing.
#[test]
fn the_pass_replaces_the_block_it_journaled_and_no_other() {
    let who = user("alice", 501);
    let sys = bare(&who);
    script::keychain_block(&sys, &who, "alice");
    let journaled = due(&sys);
    let writes = script::replaces_keychain_block(&sys, &who);
    let done = sync_vault_git_credentials(&sys, &journaled).unwrap();
    assert!(done.contains("no repositories changed"), "{done}");
    let calls = sys.calls();
    assert!(
        writes.iter().all(|write| calls.contains(write)),
        "{calls:?}"
    );
    settings_alone(&sys);

    let sys = bare(&who);
    script::keychain_block(&sys, &who, "someone-else");
    assert!(sync_vault_git_credentials(&sys, &journaled).is_err());
    assert!(
        sys.calls().iter().all(|call| !call.contains("--unset-all")),
        "{:?}",
        sys.calls()
    );

    let sys = bare(&who);
    profiled(&sys, &who, "[github]\nenabled = false\n");
    script::keychain_block(&sys, &who, "alice");
    assert!(sync_vault_git_credentials(&sys, &journaled).is_err());
    assert!(commands(&sys).is_empty(), "{:?}", sys.calls());

    let sys = bare(&who);
    script::alike(&sys, &who);
    assert!(sync_vault_git_credentials(&sys, &journaled).is_ok());
    assert!(
        sys.calls().iter().all(|call| !call.contains("--unset-all")),
        "{:?}",
        sys.calls()
    );
}

/// Against this host's real native git in a scratch HOME (no system file read), with the
/// block an earlier `aterm init` wrote here ([`script::written_helper`]): the replacement
/// leaves the file's other settings as they were, and the revert the read gave puts the block
/// back. No lookup runs while the file names the block's helper.
#[test]
fn the_pass_keeps_the_persons_settings_and_its_revert_puts_the_block_back() {
    crate::items::github::need_native_git();
    let live = Live::new();
    let mut scratch = Scratch::new(&live, "github-auto-sync").unwrap();
    let home = scratch.path().to_owned();
    let mut who = user("fixture", live.euid());
    who.home = home.to_string_lossy().into_owned();
    let git = Git::isolated();
    let gitconfig = home.join(".gitconfig");
    let written = format!("helper = {}\n", script::written_helper(&live));
    let block = format!(
        "[credential \"https://github.com\"]\n\thelper =\n\t{written}\tusername = alice\n\tuseHttpPath = false\n[user]\n\tname = Preserved\n"
    );
    live.write_file(&gitconfig, block.as_bytes(), 0o600)
        .unwrap();
    let found = git
        .keychain_block(&live, &who, &|| true)
        .unwrap()
        .expect("the block init wrote");
    let helper = crate::git_credential::Helper::stand_in("!true");
    git.replace_keychain_block(&live, &who, &found, &helper, &|| Ok(()))
        .unwrap();
    let after = std::fs::read_to_string(&gitconfig).unwrap();
    assert!(!after.contains(&written), "{after}");
    assert!(after.contains("name = Preserved"), "{after}");
    assert_eq!(git.keychain_block(&live, &who, &|| true).unwrap(), None);
    let commands = found.revert().replace(
        "git config",
        &format!("{} config", crate::items::github::need_native_git()),
    );
    let ran = live
        .run_complete_stdout(
            &crate::port::Cmd::new("/bin/sh")
                .as_user(&who)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(["-c", &commands]),
        )
        .unwrap();
    assert!(ran.success(), "{ran:?}");
    assert_eq!(
        git.keychain_block(&live, &who, &|| true).unwrap().as_ref(),
        Some(&found)
    );
    scratch.close().unwrap();
}
