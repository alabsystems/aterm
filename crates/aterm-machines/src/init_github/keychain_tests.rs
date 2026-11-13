// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::git_credential::script;
use crate::items::defaults::tests::{ctx, user};
use crate::model::{Mode, Outcome};
use crate::port::{Fake, Ran, Stat};

#[test]
fn keychain_stops_dispatch_when_admission_changes_during_the_read_or_replacement() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    let mut observations = Vec::new();
    for after_write in [false, true] {
        for fault in ["off", "unread", "nonexec"] {
            let (sys, who, context) = mac();
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
                    if fault == "nonexec" {
                        sys.stat_of(
                            crate::items::github::CLT_GIT,
                            Stat {
                                uid: 0,
                                gid: 0,
                                mode: 0o644,
                                dir: false,
                                symlink: false,
                                len: 3,
                            },
                        );
                    } else {
                        let profile = format!("{}/.config/aterm/init.toml", owner.home);
                        let bytes = b"[github]\nenabled=false\n";
                        sys.file(&profile, bytes).stat_of(
                            &profile,
                            Stat {
                                uid: owner.uid,
                                gid: owner.gid,
                                mode: 0o600,
                                dir: false,
                                symlink: false,
                                len: bytes.len() as u64,
                            },
                        );
                        if fault == "unread" {
                            sys.stat_fails(&profile, std::io::ErrorKind::PermissionDenied);
                        }
                    }
                    boundary.store(serial, Ordering::SeqCst);
                }
                None
            });
            let item = Keychain::new(who);
            assert!(matches!(item.probe(&sys, &context), Probe::Change { .. }));
            let result = item.apply(&sys, &context);
            assert!(
                stopped.load(Ordering::SeqCst),
                "the mutation boundary was reached"
            );
            observations.push((
                after_write,
                fault,
                result.is_err(),
                all_commands.load(Ordering::SeqCst) - cutoff.load(Ordering::SeqCst),
            ));
        }
    }
    assert!(
        observations
            .iter()
            .all(|(_, _, refused, later_commands)| *refused && *later_commands == 0),
        "{observations:#?}"
    );
}

#[test]
fn keychain_reads_no_settings_through_an_unadmitted_git() {
    let mut observations = Vec::new();
    for fault in ["missing", "nonexec", "empty", "loop", "install-shim"] {
        let (sys, who, context) = mac();
        let path = crate::items::github::CLT_GIT;
        match fault {
            "missing" => {
                sys.stat_fails(path, std::io::ErrorKind::NotFound);
            }
            "nonexec" | "empty" => {
                sys.stat_of(
                    path,
                    Stat {
                        uid: 0,
                        gid: 0,
                        mode: if fault == "nonexec" { 0o644 } else { 0o755 },
                        dir: false,
                        symlink: false,
                        len: if fault == "empty" { 0 } else { 3 },
                    },
                );
            }
            "loop" => {
                sys.link(path, path);
            }
            "install-shim" => {
                sys.link(path, crate::items::github::SYSTEM_GIT);
                sys.executable(crate::items::github::SYSTEM_GIT, b"shim");
            }
            _ => unreachable!(),
        }
        let result = Keychain::new(who).probe(&sys, &context);
        observations.push((
            fault,
            matches!(result, Probe::NotApplicable { .. }),
            sys.calls(),
        ));
    }
    let (sys, who, context) = mac();
    script::unconfigured(&sys, &who);
    assert!(matches!(
        Keychain::new(who).probe(&sys, &context),
        Probe::NotApplicable { .. }
    ));
    assert!(
        !sys.calls().is_empty(),
        "the executable control reads settings"
    );
    assert!(
        observations
            .iter()
            .all(|(_, skipped, calls)| *skipped && calls.is_empty()),
        "{observations:#?}"
    );
}

#[test]
fn keychain_apply_rechecks_git_and_the_persons_switch_before_any_command() {
    let mut observations = Vec::new();
    for fault in ["nonexec", "off", "unread"] {
        let (sys, who, context) = mac();
        script::keychain_block(&sys, &who, "alice");
        script::keychain_block(&sys, &who, "alice");
        let item = Keychain::new(who.clone());
        assert!(matches!(item.probe(&sys, &context), Probe::Change { .. }));
        let before = sys.calls().len();
        if fault == "nonexec" {
            sys.stat_of(
                crate::items::github::CLT_GIT,
                Stat {
                    uid: 0,
                    gid: 0,
                    mode: 0o644,
                    dir: false,
                    symlink: false,
                    len: 3,
                },
            );
        } else {
            let path = format!("{}/.config/aterm/init.toml", who.home);
            let bytes = b"[github]\nenabled=false\n";
            sys.file(&path, bytes).stat_of(
                &path,
                Stat {
                    uid: who.uid,
                    gid: who.gid,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: bytes.len() as u64,
                },
            );
            if fault == "unread" {
                sys.stat_fails(&path, std::io::ErrorKind::PermissionDenied);
            }
        }
        let result = item.apply(&sys, &context);
        observations.push((fault, result.is_err(), sys.calls()[before..].to_vec()));
    }
    assert!(
        observations
            .iter()
            .all(|(_, refused, calls)| *refused && calls.is_empty()),
        "{observations:#?}"
    );
}

/// A Mac with git, a launcher and no token file: the item needs nothing more.
fn mac() -> (Fake, User, Ctx) {
    let mut sys = Fake::root();
    sys.euid = 501;
    sys.executable(crate::items::github::CLT_GIT, b"git");
    let who = user("alice", 501);
    script::launcher(&sys, &who);
    let context = ctx(std::slice::from_ref(&who));
    (sys, who, context)
}

/// No helper ran, and neither did anything that reaches a token or GitHub.
fn settings_alone(sys: &Fake) {
    let calls = sys.calls();
    assert!(
        calls.iter().all(|call| !call.contains("credential fill")
            && !call.contains("agh ")
            && !call.contains("osxkeychain get")),
        "{calls:?}"
    );
}

/// The block an earlier init wrote gives way with no token on the machine and no GitHub to
/// ask: the plan reads the settings alone and names its revert; the apply replaces the block
/// with aterm's helper for both hosts, and the read-back holds.
#[test]
fn the_block_gives_way_from_the_settings_alone() {
    let (sys, who, context) = mac();
    script::keychain_block(&sys, &who, "alice");
    let items: [Box<dyn Item>; 1] = [item(&who)];
    let rows = engine::run(&items, Mode::Plan, &sys, &context, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Planned, "{rows:?}");
    assert_eq!(rows[0].from.as_deref(), Some(KEYCHAIN_BLOCK));
    let revert = rows[0].revert.as_deref().unwrap_or_default();
    assert!(
        revert.contains("--add credential.https://github.com.helper osxkeychain")
            && revert.contains("--add credential.https://github.com.username alice"),
        "{revert}"
    );
    assert_eq!(sys.calls(), [script::helpers(&who)]);

    let (sys, who, context) = mac();
    script::keychain_block(&sys, &who, "alice");
    script::keychain_block(&sys, &who, "alice");
    script::alike(&sys, &who);
    let writes = script::replaces_keychain_block(&sys, &who);
    let items: [Box<dyn Item>; 1] = [item(&who)];
    let rows = engine::run(&items, Mode::Apply, &sys, &context, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    let calls = sys.calls();
    assert!(
        writes.iter().all(|write| calls.contains(write)),
        "{calls:?}"
    );
    settings_alone(&sys);
}

/// `[github] enabled = false` leaves git's settings to the person, whichever entry point runs
/// the row, and it reads them no further. An init.toml that cannot be admitted says nothing
/// about the switch, so the row cannot read and writes nothing: never the vault's route in
/// its place.
#[test]
fn the_off_switch_and_an_init_toml_it_cannot_admit_write_nothing() {
    for (text, want) in [
        ("[github]\nenabled = false\n", Outcome::NotApplicable),
        ("[github]\nowner = \"alice\"\n", Outcome::Unknown),
        ("not toml at all", Outcome::Unknown),
    ] {
        let (sys, who, context) = mac();
        script::keychain_block(&sys, &who, "alice");
        let file = format!("{}/.config/aterm/init.toml", who.home);
        sys.file(&file, text.as_bytes()).stat_of(
            &file,
            crate::port::Stat {
                uid: who.uid,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: text.len() as u64,
            },
        );
        let items: [Box<dyn Item>; 1] = [item(&who)];
        let rows = engine::run(&items, Mode::Apply, &sys, &context, &mut NoJournal);
        assert_eq!(rows[0].outcome, want, "{text}: {rows:?}");
        let calls = sys.calls();
        assert!(!calls.contains(&script::helpers(&who)), "{calls:?}");
        assert!(
            calls
                .iter()
                .all(|call| !call.contains("--unset-all") && !call.contains(" --add ")),
            "{calls:?}"
        );
    }
}

/// The vault's token, as the helper would read it.
fn token(sys: &Fake, who: &User) {
    let file = format!("{}/.secrets/gh_access_token", who.home);
    sys.file(&file, b"fixture_token_only").stat_of(
        &file,
        crate::port::Stat {
            uid: who.uid,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 18,
        },
    );
}

/// `gh auth setup-git` writes this section for anyone, so with no token for aterm's helper to
/// hand back it stays: a helper with none would end the person's `git push`. The row writes
/// nothing and names it.
#[test]
fn the_setup_git_block_stays_where_atermss_helper_has_no_token() {
    let (sys, who, context) = mac();
    script::gh_setup_git_block(&sys, &who);
    let items: [Box<dyn Item>; 1] = [item(&who)];
    let rows = engine::run(&items, Mode::Apply, &sys, &context, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
    let why = rows[0].why.as_deref().unwrap_or_default();
    assert!(
        why.contains("may still reach the login keychain")
            && why.contains("credential.https://github.com.helper"),
        "{why}"
    );
    let calls = sys.calls();
    assert!(
        calls
            .iter()
            .all(|call| !call.contains("--unset-all") && !call.contains(" --add ")),
        "{calls:?}"
    );
    settings_alone(&sys);
}

/// A conditional include git refuses to read only colours the row's note: the row stands, so
/// no retry and no stage waits on a file git reads in some other repository.
#[test]
fn an_include_git_refuses_leaves_the_row_standing() {
    let (sys, who, context) = mac();
    let global = format!("{}/.gitconfig", who.home);
    let work = format!("{}/work.inc", who.home);
    sys.on(
        &script::helpers(&who),
        Ran::ok(&script::printed(&[(
            global.as_str(),
            "includeif.gitdir:~/work/.path",
            "~/work.inc",
        )])),
    );
    sys.on(
        &script::included(&who, &work),
        Ran::exit(128, "fatal: bad config line 1"),
    );
    let items: [Box<dyn Item>; 1] = [item(&who)];
    let rows = engine::run(&items, Mode::Apply, &sys, &context, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
    assert!(
        rows[0].why.as_deref().unwrap_or_default().contains(&work),
        "{rows:?}"
    );
}

/// What `gh auth setup-git` wrote goes the same way, where the token is here.
#[test]
fn the_setup_git_block_gives_way_too() {
    let (sys, who, context) = mac();
    token(&sys, &who);
    script::gh_setup_git_block(&sys, &who);
    script::gh_setup_git_block(&sys, &who);
    script::alike(&sys, &who);
    let writes = script::replaces_gh_setup_git_block(&sys, &who);
    let items: [Box<dyn Item>; 1] = [item(&who)];
    let rows = engine::run(&items, Mode::Apply, &sys, &context, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    let calls = sys.calls();
    assert!(
        writes.iter().all(|write| calls.contains(write)),
        "{calls:?}"
    );
    settings_alone(&sys);
}

/// Removing the block with no helper to put in its place would turn the system's keychain
/// helper back on: with no launcher the row waits for one and writes nothing.
#[test]
fn no_launcher_leaves_the_block_where_it_is() {
    let mut sys = Fake::root();
    sys.euid = 501;
    sys.executable(crate::items::github::CLT_GIT, b"git");
    let who = user("alice", 501);
    let context = ctx(std::slice::from_ref(&who));
    script::keychain_block(&sys, &who, "alice");
    let keychain = Keychain::new(who.clone());
    let Probe::Blocked { why } = keychain.probe(&sys, &context) else {
        panic!("no launcher, no replacement");
    };
    assert!(why.contains("/Users//alice/.local/bin/aterm"), "{why}");
    assert_eq!(keychain.revert(), None);
    assert_eq!(sys.calls(), [script::helpers(&who)]);
}

/// A block that changed after the probe journaled its revert is left: that revert would not
/// put back what is there.
#[test]
fn a_block_that_changed_after_the_probe_is_left() {
    let (sys, who, context) = mac();
    script::keychain_block(&sys, &who, "alice");
    script::keychain_block(&sys, &who, "someone-else");
    let keychain = Keychain::new(who.clone());
    assert!(matches!(
        keychain.probe(&sys, &context),
        Probe::Change { .. }
    ));
    assert!(keychain.apply(&sys, &context).is_err());
    assert!(
        sys.calls().iter().all(|call| !call.contains("--unset-all")),
        "{:?}",
        sys.calls()
    );
}

/// Nothing of an earlier init's is no step of this run, and a machine with no git has
/// nothing to read: neither row says the stage is done or asks for anything. A keychain
/// helper of someone else's stays and is named; aterm's own git never asks it.
#[test]
fn settings_of_someone_elses_are_named_and_left() {
    let (sys, who, context) = mac();
    script::alike(&sys, &who);
    let keychain = Keychain::new(who.clone());
    assert!(matches!(
        keychain.probe(&sys, &context),
        Probe::NotApplicable { .. }
    ));
    settings_alone(&sys);

    let bare = Fake::root();
    assert!(matches!(
        Keychain::new(who.clone()).probe(&bare, &context),
        Probe::NotApplicable { .. }
    ));
    assert!(bare.calls().is_empty(), "{:?}", bare.calls());

    let (sys, who, context) = mac();
    let global = "/Users//alice/.gitconfig";
    let helper = "credential.https://github.com.helper";
    sys.on(
        &script::helpers(&who),
        Ran::ok(&script::printed(&[
            (global, helper, ""),
            (global, helper, "osxkeychain"),
            (global, helper, "!other-stand-in"),
        ])),
    );
    let keychain = Keychain::new(who.clone());
    let Probe::NotApplicable { why } = keychain.probe(&sys, &context) else {
        panic!("someone else's section is no block");
    };
    assert!(why.contains(helper) && why.contains(global), "{why}");
    assert_eq!(keychain.revert(), None);
    settings_alone(&sys);
    assert!(
        sys.calls().iter().all(|call| !call.contains("--add")),
        "{:?}",
        sys.calls()
    );
}
