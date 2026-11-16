// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A parked Repositories stage counts the checkouts only while what it waits on names them
//! ([`aterm_machines::init_observe::Wait::fact`]): a clone or fetch another pass makes neither
//! ends nor restarts a park that waits on anything else.

use super::*;
use aterm_machines::init_observe::{Wait, WaitFact};
use aterm_machines::port::{Ran, Stat, System};

/// A run that selects the Repositories stage alone, after a pass of it over an inventory of one
/// repository whose destination (`~/arrived`) holds a personal file and no `.git`: work setup
/// preserves, so the stage needs a person and parks. With `lease`, another pass held this
/// account's repository lease through that pass, and its rows stood aside on it ([`HELD`]).
/// Returned with the run's state folder, revision and host, and the churn a clone completing at
/// that destination makes (its `.git` and index arriving), which moves the checkouts alone.
fn parked_beside_a_checkout(lease: bool) -> (aterm_tempfile::TempDir, u64, Fake, impl Fn(&Fake)) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(dir.path(), &selected(&[Setting::Repositories]))
        .unwrap()
        .unwrap();
    let mut host = native_repositories(true, true);
    let (sys, ctx) = host.native_repositories.as_ref().unwrap();
    let user = &ctx.users[0];
    sys.logged_in(user);
    let home = PathBuf::from(&user.home);
    let (uid, gid) = (user.uid, user.gid);
    let stat = move |dir, len| Stat {
        uid,
        gid,
        mode: if dir { 0o700 } else { 0o600 },
        dir,
        symlink: false,
        len,
    };
    let bytes = br#"{"schema":2,"account":"tester","repositories":[{"id":1,"name":"arrived","owner":{"login":"tester"},"full_name":"tester/arrived","private":true,"clone_url":"https://github.com/tester/arrived.git","default_branch":"main"}]}"#;
    sys.vault_set(&home, "github-repositories.json", bytes);
    sys.executable(home.join(".local/bin/aterm").to_str().unwrap(), b"launcher");
    let checkout = home.join("arrived");
    let git_dir = checkout.join(".git");
    sys.stat_of(checkout.to_str().unwrap(), stat(true, 0));
    sys.file(
        checkout.join("personal.txt").to_str().unwrap(),
        b"preserved local work",
    );
    let read_checkout = checkout.clone();
    let read_git = git_dir.clone();
    sys.simulate(move |sys, command| {
        let command = command.display();
        // The checkout's record of facts, once it is one: its `rev-parse` batch, its config
        // listing, HEAD on `main` at a commit, and no default branch of origin's recorded.
        Some(if command.ends_with(" credential fill") {
            Ran::ok(
                "protocol=https\nhost=github.com\nusername=tester\npassword=inert_repository_tools_token\n",
            )
        } else if command.contains(" config --null --show-origin --get-regexp ") {
            Ran::ok("")
        } else if command.ends_with(
            " rev-parse --is-inside-work-tree --show-prefix --show-toplevel \
             --is-shallow-repository --git-path index --git-path objects/info/alternates",
        ) {
            if sys.stat(&read_git).is_ok() {
                Ran::ok(&format!(
                    "true\n\n{}\nfalse\n.git/index\n.git/objects/info/alternates\n",
                    read_checkout.display()
                ))
            } else {
                Ran::exit(128, "not a Git repository")
            }
        } else if command.contains(" config --null --show-scope --show-origin --get-regexp ") {
            Ran::ok(
                "local\0file:.git/config\0remote.origin.url\nhttps://github.com/tester/arrived.git\0",
            )
        } else if command.ends_with(" symbolic-ref --quiet HEAD") {
            Ran::ok("refs/heads/main\n")
        } else if command.ends_with(" symbolic-ref --quiet refs/remotes/origin/HEAD") {
            Ran::exit(1, "")
        } else if command.contains(" for-each-ref ") && command.ends_with(" refs/heads/main") {
            Ran::ok("1234567890abcdef1234567890abcdef12345678 commit refs/heads/main\n")
        } else {
            panic!("unexpected repository effect: {command}")
        })
    });
    // Another pass's lease, as the converge pass holds align.lock across its rows.
    let held = lease.then(|| {
        System::repository_pass(sys)
            .unwrap()
            .pass
            .expect("the lease is free")
    });
    let (report, next) =
        execute_initial_for_revision(&mut host, dir.path(), revision, &mut None, SetupHooks::NONE)
            .unwrap()
            .unwrap();
    drop(held);
    assert_eq!(next, SetupNext::Park, "{}", report.to_text());
    assert_eq!(
        report.rows.iter().any(|row| row.status == HELD),
        lease,
        "{}",
        report.to_text()
    );
    let saved = progress(dir.path()).unwrap().unwrap();
    assert_eq!(saved.done & Stage::Repositories.bit(), 0);
    let churn = move |host: &Fake| {
        let (sys, _) = host.native_repositories.as_ref().unwrap();
        sys.stat_of(git_dir.to_str().unwrap(), stat(true, 0));
        sys.file(git_dir.join("index").to_str().unwrap(), b"DIRC");
    };
    (dir, revision, host, churn)
}

/// The mark a park of the run under `root` reads, with `host`'s repository inputs.
fn parked_mark(root: &Path, host: &Fake) -> InitialSetupParkMark {
    let (sys, _) = host.native_repositories.as_ref().unwrap();
    initial_setup_park_mark_with(Some(root), None, 1_800_000_600, || {
        Some("unchanged attached network".into())
    })
    .with_repository_inputs(|| aterm_machines::init_github::repository_input_mark(sys).ok())
}

/// Whether `churn` moved the park of the run under `root`: it ends the park (the mark it reads
/// differs) and, read against the mark before it, restarts the run's pace
/// ([`InitialSetupParkMark::facts_changed_since`]). The two always agree here. The lease
/// releases a lease watch of another test hears count in this process too, so they are held out.
fn moves_the_park(root: &Path, host: &Fake, churn: &impl Fn(&Fake)) -> bool {
    let before = parked_mark(root, host);
    churn(host);
    let mut after = parked_mark(root, host);
    after.lease = before.lease;
    let ends = after != before;
    let restarts = after.facts_changed_since(&before);
    assert_eq!(
        ends, restarts,
        "a park that ends restarts the pace, and only then"
    );
    restarts
}

/// A Repositories stage left on its own retry (a part that failed, or a refresh of the
/// checkouts here left for later), or waiting on any fact but a folder in a repository's way, is
/// neither woken nor has its retry pace started afresh by a clone or fetch at a checkout: its
/// park neither watches nor counts the checkouts, while it still counts the selected source.
/// CONTROL: the same churn under the same run, with the stage waiting on a folder in a
/// repository's way ([`WaitFact::Checkouts`]), moves the park, so the churn is a real change of
/// what the park could read.
#[test]
fn a_checkout_change_does_not_restart_an_unrelated_retry_pace() {
    for left in std::iter::once(None).chain(Wait::ALL.map(Some)) {
        let (dir, revision, host, churn) = parked_beside_a_checkout(false);
        record_repositories_left(dir.path(), revision, left);
        assert_eq!(repositories_left(dir.path(), revision), left);
        let counts = left.map(Wait::fact) == Some(WaitFact::Checkouts);
        let watches = parked_mark(dir.path(), &host).watches();
        assert_eq!(watches.checkouts, counts, "{left:?}: {watches:?}");
        assert_eq!(
            moves_the_park(dir.path(), &host, &churn),
            counts,
            "{left:?}: a checkout change moves only a park whose wait names the checkouts"
        );
    }
}

/// A Repositories stage another pass's repository lease held ([`HELD`]) waits on that lease's
/// release alone: a clone completing at a checkout meanwhile, as the lease's holder makes,
/// neither ends its park nor restarts its pace, and the park watches the lease in place of Git
/// and the checkouts. CONTROL: the same run and churn without the lease held parks on the folder
/// in the repository's way, which the churn moves.
#[test]
fn checkout_churn_is_no_fact_for_a_stage_waiting_on_the_lease() {
    for lease in [true, false] {
        let (dir, revision, host, churn) = parked_beside_a_checkout(lease);
        let left = repositories_left(dir.path(), revision);
        let watches = parked_mark(dir.path(), &host).watches();
        if lease {
            assert_eq!(left, Some(Wait::OtherPass));
            assert!(
                watches.lease && !watches.git && !watches.checkouts,
                "{watches:?}"
            );
        } else {
            assert_eq!(left.map(Wait::fact), Some(WaitFact::Checkouts));
            assert!(!watches.lease && watches.checkouts, "{watches:?}");
        }
        assert_eq!(
            moves_the_park(dir.path(), &host, &churn),
            !lease,
            "lease held: {lease}"
        );
    }
}

/// Work at a repository's destination holds the stage ([`repositories::left_wait`] reads it as
/// a folder in the way, [`Wait::InTheWay`]): only a change there moves it, so its park watches
/// the checkouts, and a clone completing there ends the park and restarts the pace. CONTROL: the
/// same run and churn, with the stage left on its own retry instead (no typed wait), neither
/// watches the checkouts nor moves.
#[test]
fn checkout_churn_still_wakes_a_stage_a_folder_blocks() {
    for blocked in [true, false] {
        let (dir, revision, host, churn) = parked_beside_a_checkout(false);
        assert_eq!(
            repositories_left(dir.path(), revision),
            Some(Wait::InTheWay)
        );
        if !blocked {
            record_repositories_left(dir.path(), revision, None);
        }
        assert_eq!(parked_mark(dir.path(), &host).watches().checkouts, blocked);
        assert_eq!(
            moves_the_park(dir.path(), &host, &churn),
            blocked,
            "folder in the way: {blocked}"
        );
    }
}
