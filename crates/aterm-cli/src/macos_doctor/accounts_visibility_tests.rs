// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual Accounts pass reads the accounts whatever this login's screen shows, but clicks
//! into no account app behind a locked screen, another login or the person's own keys.

use super::*;
use aterm_machines::port::{Fake, Ran, SessionReach, Stat};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Copy)]
pub(super) enum Preflight {
    Unknown,
    SignedOut,
}

#[derive(Clone, Copy)]
enum Current {
    SignedIn,
    SignedOut,
    Unknown,
}

pub(super) struct Account {
    pub(super) signed_in: AtomicBool,
    pub(super) unreadable: bool,
    pub(super) signouts: AtomicUsize,
}

pub(super) fn system(preflight: Option<Preflight>, account: &Arc<Account>) -> Fake {
    system_after_signout(preflight, account, || {})
}

pub(super) fn system_after_signout(
    preflight: Option<Preflight>,
    account: &Arc<Account>,
    after_signout: impl Fn() + Send + Sync + 'static,
) -> Fake {
    let mut sys = Fake::root();
    sys.euid = 501;
    sys.stat_of(
        "/dev/console",
        Stat {
            uid: 501,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    sys.logged_in(&aterm_machines::model::User {
        name: "alice".into(),
        uid: 501,
        gid: 20,
        home: "/Users//alice".into(),
        shell: "/bin/zsh".into(),
        admin: true,
    });
    // Accessibility is on: the sign-out's clicks run.
    sys.trusts(&[true]);
    let account = Arc::clone(account);
    sys.simulate(move |_, cmd| {
        let args: Vec<_> = cmd.args.iter().map(|arg| arg.to_str().unwrap()).collect();
        let answer = match cmd.program.to_str().unwrap() {
            "/usr/bin/hidutil" => "Services:\n\nDevices:\n",
            "/usr/bin/defaults" => {
                assert!(matches!(
                    args.as_slice(),
                    [
                        "export",
                        "com.apple.imservice.ids.iMessage" | "com.apple.imservice.ids.FaceTime",
                        "-"
                    ]
                ));
                if matches!(preflight, Some(Preflight::Unknown))
                    || (preflight.is_none() && account.unreadable)
                {
                    return Some(Ran::exit(1, "injected account reading failure"));
                }
                if preflight.is_none() && account.signed_in.load(Ordering::SeqCst) {
                    "<dict><key>ActiveAccounts</key><array><string>fixture</string></array></dict>"
                } else {
                    "<dict><key>ActiveAccounts</key><array/></dict>"
                }
            }
            // The Automation question for System Events, asked before any app opens.
            "/usr/bin/osascript" if args.len() == 2 => "System Events",
            "/usr/bin/osascript" if args.first() == Some(&"-e") => {
                assert!(["Messages", "FaceTime"].contains(&args[3]));
                account.signouts.fetch_add(1, Ordering::SeqCst);
                account.signed_in.store(false, Ordering::SeqCst);
                after_signout();
                "sign-out-requested"
            }
            "/usr/bin/osascript" if args.get(4) == Some(&"--") => {
                r#"{"panes":["iMessage"],"sign_out":["Sign Out"]}"#
            }
            other => panic!("unexpected inert account tool: {other}"),
        };
        Some(Ran::ok(answer))
    });
    sys
}

/// The Accounts pass over `current`, with this login's session reading `reach` (`None`: open).
fn run(
    preflight: Preflight,
    reach: Option<SessionReach>,
    current: Current,
) -> (Report, Arc<Account>, bool) {
    let root = aterm_tempfile::tempdir().unwrap();
    run_in(root.path(), preflight, reach, current)
}

/// When [`run_in`]'s pass runs, in unix seconds.
const RUN_AT: u64 = 1_800_000_000;

/// The build [`run_in`]'s pass runs as.
const RUN_BUILD: &str = "inert-accounts-visibility";

/// [`run`] in the state root `root`, whose records the caller reads after.
fn run_in(
    root: &Path,
    preflight: Preflight,
    reach: Option<SessionReach>,
    current: Current,
) -> (Report, Arc<Account>, bool) {
    let mut selection = Selection::none();
    selection.set(Setting::Messages, true);
    authorize_automatic_setup_at(root, selection).unwrap();
    let account = Arc::new(Account {
        signed_in: AtomicBool::new(matches!(current, Current::SignedIn)),
        unreadable: matches!(current, Current::Unknown),
        signouts: AtomicUsize::new(0),
    });
    // Production creates distinct Live ports for the visibility preflight and its host.
    // Here the first port has the earlier reading, and the host sees the newer account.
    let preflight = system(Some(preflight), &account);
    let session = system(None, &account);
    *session.session.lock().unwrap() = reach;
    let mut host = NativeHost::over(
        Box::new(session),
        Box::new(aterm_machines::init::prepare_workstation_selected),
        initial::complete_explicit_workstation_plan_for_test,
    );
    let report = apply_part_with(
        root,
        &preflight,
        &mut host,
        RUN_BUILD,
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(RUN_AT),
        Part::Accounts,
    )
    .unwrap()
    .unwrap();
    let journaled = host
        .sys
        .read_file(Path::new(Fake::JOURNAL))
        .is_ok_and(|bytes| !bytes.is_empty());
    (report, account, journaled)
}

#[test]
fn accounts_visibility_controls_really_apply_into_an_open_session_and_verify_behind_a_lock() {
    let (report, account, journaled) = run(Preflight::Unknown, None, Current::SignedIn);
    assert!(!report.needs_attention(), "{report:?}");
    assert_eq!(account.signouts.load(Ordering::SeqCst), 1);
    assert!(!account.signed_in.load(Ordering::SeqCst));
    assert!(journaled);

    // Behind a lock an account already signed out reads done: nothing needs clicking.
    let locked = Some(SessionReach::Locked);
    let (report, account, journaled) = run(Preflight::SignedOut, locked, Current::SignedOut);
    assert!(!report.needs_attention(), "{report:?}");
    assert_eq!(account.signouts.load(Ordering::SeqCst), 0);
    assert!(!journaled);

    // An account nothing can read is not done, behind a lock as anywhere: the read is no
    // click, so the lock does not excuse it, and its row reads unconfirmed. Nothing clicks.
    let (report, account, journaled) = run(Preflight::SignedOut, locked, Current::Unknown);
    assert!(
        !report.rows.is_empty() && report.rows.iter().all(Row::failed),
        "{report:?}"
    );
    assert_eq!(account.signouts.load(Ordering::SeqCst), 0);
    assert!(!journaled);
}

/// No earlier reading authorizes clicks behind a lock: whatever the preflight read, a signed-in
/// account behind a locked screen gets no click, no journal entry and no failed try; its row
/// waits, and the pass looks again by itself ([`LOOK_AGAIN`]).
#[test]
fn accounts_visibility_no_preflight_authorizes_a_signout_behind_a_lock() {
    for preflight in [Preflight::Unknown, Preflight::SignedOut] {
        let root = aterm_tempfile::tempdir().unwrap();
        let (report, account, journaled) = run_in(
            root.path(),
            preflight,
            Some(SessionReach::Locked),
            Current::SignedIn,
        );
        assert_eq!(account.signouts.load(Ordering::SeqCst), 0);
        assert!(account.signed_in.load(Ordering::SeqCst));
        assert!(!journaled);
        assert!(
            report.rows.iter().any(|row| row.status == initial::WAITING),
            "{report:?}"
        );
        // The production host records the wait it stood aside on, durably, with no failed try,
        // and the park's timer comes a minute on ([`LOOK_AGAIN`]): deleting `NativeHost`'s
        // extraction of the wait from its rows fails here.
        let revision = initial::revision_at(root.path()).unwrap();
        let record = Automatic::load(root.path(), revision).expect("the Automatic record");
        assert!(record.failed.is_empty(), "{record:?}");
        assert_eq!(record.waits.len(), 1, "{record:?}");
        assert_eq!(
            (
                record.waits[0].build.as_str(),
                record.waits[0].tries,
                record.waits[0].at
            ),
            (RUN_BUILD, 1, RUN_AT),
            "{record:?}"
        );
        assert_eq!(
            mac_settings_retry_ahead(root.path(), revision, RUN_BUILD, RUN_AT).unwrap(),
            Some(RUN_AT + 60)
        );
    }
}
