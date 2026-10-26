// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual Accounts pass may verify off screen, but must not open an account app there.

use super::*;
use aterm_machines::port::{Fake, Ran, Stat};
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
    let account = Arc::clone(account);
    sys.simulate(move |_, cmd| {
        let args: Vec<_> = cmd.args.iter().map(|arg| arg.to_str().unwrap()).collect();
        let answer = match cmd.program.to_str().unwrap() {
            "/usr/bin/id" => "alice\n",
            "/usr/bin/dscl" if args.contains(&"/Groups/admin") => {
                "GroupMembership: alice\n"
            }
            "/usr/bin/dscl" => {
                "UniqueID: 501\nPrimaryGroupID: 20\nNFSHomeDirectory: /Users//alice\nUserShell: /bin/zsh\n"
            }
            "/usr/bin/uname" => "arm64\n",
            "/usr/bin/sw_vers" => "27.0\n",
            "/usr/bin/defaults" => {
                assert!(matches!(args.as_slice(), ["export", "com.apple.imservice.ids.iMessage" | "com.apple.imservice.ids.FaceTime", "-"]));
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
            "/usr/bin/osascript" => "true",
            other => panic!("unexpected inert account tool: {other}"),
        };
        Some(Ran::ok(answer))
    });
    sys
}

fn run(preflight: Preflight, on_screen: bool, current: Current) -> (Report, Arc<Account>, bool) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut selection = Selection::none();
    selection.set(Setting::Messages, true);
    authorize_automatic_setup_at(root.path(), selection).unwrap();
    let account = Arc::new(Account {
        signed_in: AtomicBool::new(matches!(current, Current::SignedIn)),
        unreadable: matches!(current, Current::Unknown),
        signouts: AtomicUsize::new(0),
    });
    // Production creates distinct Live ports for the visibility preflight and its host.
    // Here the first port has the earlier reading, and the host sees the newer account.
    let preflight = system(Some(preflight), &account);
    let mut host = NativeHost::over(
        Box::new(system(None, &account)),
        Box::new(aterm_machines::init::prepare_workstation_selected),
    );
    let report = apply_part_with(
        root.path(),
        &preflight,
        &mut host,
        "inert-accounts-visibility",
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000),
        Part::Accounts { on_screen },
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
fn accounts_visibility_controls_really_apply_on_screen_and_verify_signed_out_off_screen() {
    let (report, account, journaled) = run(Preflight::Unknown, true, Current::SignedIn);
    assert!(!report.needs_attention(), "{report:?}");
    assert_eq!(account.signouts.load(Ordering::SeqCst), 1);
    assert!(!account.signed_in.load(Ordering::SeqCst));
    assert!(journaled);

    let (report, account, journaled) = run(Preflight::SignedOut, false, Current::SignedOut);
    assert!(!report.needs_attention(), "{report:?}");
    assert_eq!(account.signouts.load(Ordering::SeqCst), 0);
    assert!(!journaled);

    let (report, account, journaled) = run(Preflight::SignedOut, false, Current::Unknown);
    assert!(report.needs_attention());
    assert!(report.rows.iter().any(|row| row.status == initial::WAITING));
    assert_eq!(account.signouts.load(Ordering::SeqCst), 0);
    assert!(!journaled);
}

#[test]
fn accounts_visibility_unknown_preflight_cannot_authorize_an_off_screen_signout() {
    let (report, account, journaled) = run(Preflight::Unknown, false, Current::SignedIn);
    assert_eq!(account.signouts.load(Ordering::SeqCst), 0);
    assert!(account.signed_in.load(Ordering::SeqCst));
    assert!(!journaled);
    assert!(report.needs_attention());
}

#[test]
fn accounts_visibility_signed_out_preflight_cannot_authorize_a_later_off_screen_signout() {
    let (report, account, journaled) = run(Preflight::SignedOut, false, Current::SignedIn);
    assert_eq!(account.signouts.load(Ordering::SeqCst), 0);
    assert!(account.signed_in.load(Ordering::SeqCst));
    assert!(!journaled);
    assert!(report.needs_attention());
}
