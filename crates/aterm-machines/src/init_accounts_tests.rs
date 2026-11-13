// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::items::defaults::tests::{ctx, user};
use crate::model::{Mode, Outcome};
use crate::port::{Fake, Ran};

fn fixture() -> (Fake, Ctx, SignOut) {
    let user = user("alice", 501);
    let mut ctx = ctx(std::slice::from_ref(&user));
    ctx.console = Some(user.clone());
    ctx.disrupt = true;
    let item = SignOut {
        service: Service::Messages,
        user,
        blocked: Mutex::new(None),
        reading: crate::init_observe::LastReading::default(),
        consent: std::sync::Arc::default(),
    };
    let mut sys = Fake::root();
    sys.responsible = Some("aterm".into());
    (sys, ctx, item)
}

/// The runs a sign-out makes once Accessibility and Automation are on: the labels, and the
/// clicks answering `clicked`.
fn clicks(sys: &Fake, item: &SignOut, clicked: &str) {
    let labels = Labels {
        panes: vec!["iMessage".into()],
        sign_out: vec!["Sign Out".into()],
    };
    sys.on(
        &labels_cmd(&item.user, item.service).display(),
        Ran::ok(r#"{"panes":["iMessage"],"sign_out":["Sign Out"]}"#),
    );
    sys.on(
        &signout_cmd(&item.user, item.service, &labels).display(),
        Ran::ok(clicked),
    );
}

fn automation_ask(item: &SignOut) -> Cmd {
    Cmd::new(OSASCRIPT)
        .args(["-e", AUTOMATION_ASK])
        .as_user(&item.user)
        .in_session()
        .budget(PATIENCE)
}

/// Signed in for the first `reads` readings of the account, signed out after.
fn signed_in_for(sys: &Fake, item: &SignOut, reads: usize) {
    for _ in 0..reads {
        store(sys, item, DISABLED);
    }
    store(sys, item, "<dict/>");
    prefs(sys, item, EMPTY_PREFS);
}

fn count(sys: &Fake, call: &str) -> usize {
    sys.calls().iter().filter(|c| c.as_str() == call).count()
}

fn prefs(sys: &Fake, item: &SignOut, body: &str) {
    let cmd = Cmd::new("/usr/bin/defaults")
        .args(["export", item.service.domain(), "-"])
        .as_user(&item.user);
    sys.on(&cmd.display(), Ran::ok(body));
}

fn store(sys: &Fake, item: &SignOut, xml: &str) {
    let path = store_path(&item.user);
    sys.file(
        path.to_str().unwrap(),
        b"\x1f\x8bcompressed account fixture",
    );
    let gzip = Cmd::new(GZIP)
        .args(["-dc", "--"])
        .arg(path)
        .as_user(&item.user);
    sys.on(&gzip.display(), Ran::ok("binary account fixture"));
    let convert = Cmd::new(PLUTIL)
        .args(["-convert", "xml1", "-o", "-", "--", "-"])
        .as_user(&item.user);
    sys.on(&convert.display(), Ran::ok(xml));
}

fn uncompressed_store(sys: &Fake, item: &SignOut, xml: &str) {
    let path = store_path(&item.user);
    sys.file(path.to_str().unwrap(), b"bplist00account fixture");
    let convert = Cmd::new(PLUTIL)
        .args(["-convert", "xml1", "-o", "-", "--"])
        .arg(path)
        .as_user(&item.user);
    sys.on(&convert.display(), Ran::ok(xml));
}

const EMPTY_PREFS: &str =
    "<dict><key>ActiveAccounts</key><array/><key>OnlineAccounts</key><array/></dict>";
const DISABLED: &str = "<dict><key>com.apple.ids.service.com.apple.madrid</key><dict><key>Accounts</key><dict><key>private-account-id</key><dict><key>AuthToken</key><string>private-token</string></dict></dict><key>EnabledAccounts</key><array/></dict></dict>";

#[test]
fn disabled_and_disconnected_still_requires_durable_signout() {
    let (sys, ctx, item) = fixture();
    store(&sys, &item, DISABLED);
    prefs(&sys, &item, EMPTY_PREFS);
    assert!(matches!(item.probe(&sys, &ctx), Probe::Change { .. }));
    let printed = format!("{:?}", item.probe(&sys, &ctx));
    assert!(!printed.contains("private-account-id"));
    assert!(!printed.contains("private-token"));
    // System State reads it as signed in, from the probe, not the printed line.
    assert_eq!(
        item.detail(&sys, &ctx),
        Some(crate::init_observe::Reading::SignedIn.token())
    );
}

#[test]
fn absent_registrations_do_not_open_apps_or_request_permissions() {
    let (sys, ctx, item) = fixture();
    store(&sys, &item, "<dict/>");
    prefs(&sys, &item, EMPTY_PREFS);
    assert!(matches!(item.probe(&sys, &ctx), Probe::Ok { .. }));
    assert!(sys.calls().iter().all(|c| !c.contains("osascript")));
    assert_eq!(item.detail(&sys, &ctx), None);
}

#[test]
fn an_uncompressed_account_store_reads_signed_out_without_decompression() {
    let (sys, ctx, item) = fixture();
    uncompressed_store(&sys, &item, "<dict/>");
    prefs(&sys, &item, EMPTY_PREFS);
    assert!(matches!(item.probe(&sys, &ctx), Probe::Ok { .. }));
    assert!(sys.calls().iter().all(|call| !call.contains(GZIP)));
    assert_eq!(item.detail(&sys, &ctx), None);
}

#[test]
fn an_uncompressed_account_store_still_detects_a_registered_account() {
    let (sys, ctx, item) = fixture();
    uncompressed_store(&sys, &item, DISABLED);
    prefs(&sys, &item, EMPTY_PREFS);
    assert!(matches!(item.probe(&sys, &ctx), Probe::Change { .. }));
    assert_eq!(
        item.detail(&sys, &ctx),
        Some(crate::init_observe::Reading::SignedIn.token())
    );
}

#[test]
fn a_malformed_uncompressed_account_store_is_never_reported_signedout() {
    let (sys, ctx, item) = fixture();
    let path = store_path(&item.user);
    sys.file(path.to_str().unwrap(), b"bplist00malformed account fixture");
    let convert = Cmd::new(PLUTIL)
        .args(["-convert", "xml1", "-o", "-", "--"])
        .arg(path)
        .as_user(&item.user);
    sys.on(&convert.display(), Ran::exit(1, "invalid property list"));
    assert!(matches!(item.probe(&sys, &ctx), Probe::Unusable { .. }));
}

#[test]
fn unreadable_or_truncated_account_store_is_never_reported_signedout() {
    let (sys, ctx, item) = fixture();
    let path = store_path(&item.user);
    sys.stat_fails(path.to_str().unwrap(), io::ErrorKind::PermissionDenied);
    assert!(matches!(item.probe(&sys, &ctx), Probe::Unusable { .. }));
    let (sys, ctx, item) = fixture();
    store(&sys, &item, "<dict/>");
    prefs(&sys, &item, EMPTY_PREFS);
    let gzip = Cmd::new(GZIP)
        .args(["-dc", "--"])
        .arg(store_path(&item.user))
        .as_user(&item.user);
    sys.refuse_complete_stdout(&gzip.display(), "record exceeded limit");
    assert!(matches!(item.probe(&sys, &ctx), Probe::Unusable { .. }));
}

/// The sign-out clicks are not repeated safely, so they hold the update boundary while
/// they run (init-on-by-default design §6.7): an update's handoff waits for them, boundedly.
/// Before, nothing held the guard, and a handoff could cut the clicks in half.
#[test]
fn the_sign_out_clicks_hold_the_update_boundary() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (sys, ctx, item) = fixture();
    sys.trusts(&[true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 1);
    let labels = Labels {
        panes: vec!["iMessage".into()],
        sign_out: vec!["Sign Out".into()],
    };
    let clicking = signout_cmd(&item.user, item.service, &labels).display();
    let held = std::sync::Arc::new(AtomicBool::new(false));
    let seen = std::sync::Arc::clone(&held);
    sys.simulate(move |sys, cmd| {
        if cmd.display() == clicking {
            seen.store(
                sys.boundary
                    .is_held(aterm_types::update_boundary::GuardedItem::SignOutSequence),
                Ordering::SeqCst,
            );
        }
        None
    });
    item.apply(&sys, &ctx).expect("signed out");
    assert!(
        held.load(Ordering::SeqCst),
        "the clicks ran under the guard: {:?}",
        sys.calls()
    );
}

/// The clicks of a sign-out ready to click (Accessibility on, Automation answered, the
/// labels read), as [`signout_cmd`] runs them.
fn clicking(item: &SignOut) -> String {
    let labels = Labels {
        panes: vec!["iMessage".into()],
        sign_out: vec!["Sign Out".into()],
    };
    signout_cmd(&item.user, item.service, &labels).display()
}

/// Run `item`'s apply on a worker while an update's handoff is pending on this thread, and
/// once the clicks wait for it, call `meanwhile` (minutes passing) and refuse the handoff.
fn apply_behind_a_handoff(
    sys: &Fake,
    ctx: &Ctx,
    item: &SignOut,
    meanwhile: impl FnOnce(),
) -> Result<(), String> {
    let latch = sys.boundary.latch_handoff(Duration::from_secs(60));
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| item.apply(sys, ctx));
        let deadline = Instant::now() + Duration::from_secs(60);
        while sys.boundary.waiting() == 0 {
            assert!(Instant::now() < deadline, "the sign-out never waited");
            aterm_uds::deadline::poll_yield();
        }
        assert_eq!(
            count(sys, &clicking(item)),
            0,
            "nothing clicks while the handoff is pending"
        );
        meanwhile();
        drop(latch);
        worker.join().unwrap()
    })
}

/// A sign-out's clicks wait for a pending update handoff (init-on-by-default design §6.7),
/// up to minutes, and once that handoff is refused it reads this login's session again
/// before it clicks: the screen locked meanwhile, so nothing is clicked, and the sign-out
/// stands aside, calmly, waiting to look again by itself. Before, it clicked at once.
#[test]
fn a_lock_during_a_handoff_wait_stands_the_sign_out_aside() {
    let (sys, ctx, item) = fixture();
    sys.trusts(&[true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 1);
    apply_behind_a_handoff(&sys, &ctx, &item, || {
        *sys.session.lock().unwrap() = Some(SessionReach::Locked);
    })
    .expect("standing aside is no failure");
    assert_eq!(
        count(&sys, &clicking(&item)),
        0,
        "nothing clicks behind the lock: {:?}",
        sys.calls()
    );
    let Some(aterm_converge::StoodAside::Waiting(why)) = item.stood_aside() else {
        panic!("{:?}", item.stood_aside());
    };
    assert!(why.starts_with("this Mac's screen is locked"), "{why}");
    assert_eq!(sys.boundary.any_held(), None, "it took no guard");
    // CONTROL: the screen stays open, and the clicks go once the handoff is refused.
    let (sys, ctx, item) = fixture();
    sys.trusts(&[true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 1);
    apply_behind_a_handoff(&sys, &ctx, &item, || {}).expect("signed out");
    assert_eq!(count(&sys, &clicking(&item)), 1, "{:?}", sys.calls());
    assert_eq!(item.stood_aside(), None);
}

/// A sign-out clicks only into this login's own screen, unlocked, with the keyboard and
/// pointer at rest: locked, switched to another login, in the person's use or unread, it
/// clicks nothing, takes no front, and stands aside waiting, a wait its next pass looks at
/// again by itself ([`looks_again`]), never a failed try. Before, it clicked whatever the
/// session's state.
#[test]
fn a_sign_out_into_an_unreachable_session_clicks_nothing() {
    for reach in [
        SessionReach::Locked,
        SessionReach::SwitchedAway,
        SessionReach::KeysInUse,
        SessionReach::Unread,
    ] {
        let (sys, ctx, item) = fixture();
        sys.trusts(&[true]);
        clicks(&sys, &item, "sign-out-requested");
        signed_in_for(&sys, &item, 1);
        *sys.session.lock().unwrap() = Some(reach);
        item.apply(&sys, &ctx)
            .expect("standing aside is no failure");
        assert_eq!(
            count(&sys, &clicking(&item)),
            0,
            "{reach:?}: {:?}",
            sys.calls()
        );
        let Some(aterm_converge::StoodAside::Waiting(why)) = item.stood_aside() else {
            panic!("{reach:?}: {:?}", item.stood_aside());
        };
        assert!(why.starts_with(reach.waits().unwrap()), "{why}");
        assert!(looks_again(&report_of(&item)), "{why}");
    }
    // CONTROL: open, it clicks once and stands aside for nothing.
    let (sys, ctx, item) = fixture();
    sys.trusts(&[true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 1);
    item.apply(&sys, &ctx).expect("signed out");
    assert_eq!(count(&sys, &clicking(&item)), 1, "{:?}", sys.calls());
    assert_eq!(item.stood_aside(), None);
    // At a person's own terminal the person who ran it is there and asked for the clicks:
    // their own keys hold nothing, a lock still does. A coding agent's terminal is no
    // person's: the keys in use are the person's, typing elsewhere, and nothing is clicked
    // into their work (before, Messages took the front mid-typing there).
    for (reach, agent, clicked) in [
        (SessionReach::KeysInUse, None, 1),
        (SessionReach::Locked, None, 0),
        (SessionReach::KeysInUse, Some("Claude Code"), 0),
        // An agent's run clicks nothing even into an open session: the sign-out is the
        // person's to start (owner ruling 2026-11-09,
        // `a_sign_out_in_an_agents_run_asks_and_clicks_nothing`).
        (SessionReach::Open, Some("Claude Code"), 0),
    ] {
        let (mut sys, ctx, item) = fixture();
        sys.tty = true;
        sys.agent = agent;
        sys.trusts(&[true]);
        clicks(&sys, &item, "sign-out-requested");
        signed_in_for(&sys, &item, 1);
        *sys.session.lock().unwrap() = Some(reach);
        item.apply(&sys, &ctx).expect("no failure");
        assert_eq!(
            count(&sys, &clicking(&item)),
            clicked,
            "{reach:?} at a terminal under {agent:?}"
        );
        if agent.is_some() {
            assert_eq!(
                item.stood_aside(),
                Some(aterm_converge::StoodAside::Waiting(agents_run()))
            );
        } else if clicked == 0 {
            assert!(looks_again(&report_of(&item)), "{:?}", item.stood_aside());
        }
    }
    // Through the engine, behind a lock, the sign-out stands aside before it acts: nothing is
    // journaled, read of macOS's Automation answer or clicked.
    let (sys, ctx, item) = fixture();
    sys.trusts(&[true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 1);
    *sys.session.lock().unwrap() = Some(SessionReach::Locked);
    let script = clicking(&item);
    let rows = engine::run(&[Box::new(item)], Mode::Apply, &sys, &ctx, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{:?}", rows[0]);
    assert!(looks_again(&rows[0]), "{:?}", rows[0]);
    assert_eq!(count(&sys, &script), 0);
    assert_eq!(count(&sys, "automation-consent"), 0, "{:?}", sys.calls());
}

/// The row `item`'s last apply leaves, as the engine reports a stand-aside.
fn report_of(item: &SignOut) -> crate::model::ItemReport {
    crate::model::ItemReport {
        id: item.id(),
        family: item.family().to_owned(),
        title: item.title(),
        outcome: Outcome::Blocked,
        from: None,
        to: None,
        why: None,
        revert: None,
        effect: item.effect(),
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: item.stood_aside(),
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    }
}

/// Away from a terminal (System State, which asks for Accessibility itself) a sign-out
/// without it asks nothing, opens nothing and waits for nothing: it stands aside naming the
/// app the grant goes to.
#[test]
fn permission_refusal_does_not_launch_an_account_app() {
    let (sys, ctx, item) = fixture();
    sys.trusts(&[false]);
    item.apply(&sys, &ctx)
        .expect("permission block is a structured outcome");
    let Some(aterm_converge::StoodAside::Permission(why)) = item.stood_aside() else {
        panic!("{:?}", item.stood_aside());
    };
    assert!(why.starts_with("aterm has no Accessibility"), "{why}");
    assert_eq!(sys.calls(), ["session-reach", "ax-trusted"]);
}

/// At a terminal, macOS is asked once and the trust read every 250 ms in this process: the
/// moment it flips the sign-out goes on in the same run, with no exit 77, no `sleep` child
/// and no keystroke; the deregistration that follows is read in this process too.
#[test]
fn trust_that_flips_while_init_waits_signs_out_in_the_same_run() {
    let (mut sys, ctx, item) = fixture();
    sys.tty = true;
    sys.trusts(&[false, false, false, false, true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 2);
    let rows = engine::run(&[Box::new(item)], Mode::Apply, &sys, &ctx, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0]);
    assert_eq!(rows[0].stood_aside, None);
    assert_eq!(count(&sys, "ax-request"), 1);
    assert_eq!(count(&sys, "sleep 250ms"), 3 + 1, "{:?}", sys.calls());
    let calls = sys.calls();
    assert!(
        calls
            .iter()
            .all(|c| !c.contains("/bin/sleep") && !c.contains("keystroke")),
        "{calls:?}"
    );
    let at = |needle: &str| calls.iter().position(|c| c.contains(needle)).unwrap();
    // The Automation answer is read once the grant is on, before any click.
    assert!(
        at("ax-request") < at("automation-consent") && at("automation-consent") < at("Sign Out")
    );
}

/// In the aterm window that runs it, the run's Accessibility ask goes to the window's
/// permission walk: macOS's own dialog is not asked for, and the grant is read every 250 ms
/// until it flips. NEGATIVE CONTROL: with no window to walk it, macOS is asked.
#[test]
fn the_aterm_window_walks_the_runs_accessibility_ask() {
    let (mut sys, ctx, item) = fixture();
    sys.tty = true;
    sys.walks = AccessibilityWalk::Taken;
    sys.trusts(&[false, false, false, true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 2);
    let rows = engine::run(&[Box::new(item)], Mode::Apply, &sys, &ctx, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0]);
    assert_eq!(count(&sys, "ax-walk"), 1);
    assert_eq!(count(&sys, "ax-request"), 0, "{:?}", sys.calls());
    let calls = sys.calls();
    let at = |needle: &str| calls.iter().position(|c| c.contains(needle)).unwrap();
    assert!(at("ax-walk") < at("automation-consent") && at("automation-consent") < at("Sign Out"));

    let (mut sys, ctx, item) = fixture();
    sys.tty = true;
    sys.trusts(&[false, true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 2);
    let _ = engine::run(&[Box::new(item)], Mode::Apply, &sys, &ctx, &mut NoJournal);
    assert_eq!((count(&sys, "ax-walk"), count(&sys, "ax-request")), (1, 1));
}

/// A RUN IN A TAB BEHIND BRINGS NOTHING UP: the window's desk door refused the walk, so the
/// sign-out asks macOS for no dialog in its place, brings no app forward and stands aside
/// for the person. NEGATIVE CONTROL: with no window to walk it, macOS is asked
/// (`the_aterm_window_walks_the_runs_accessibility_ask`).
#[test]
fn a_walk_the_desk_door_refused_raises_no_macos_dialog_and_no_app() {
    let (mut sys, ctx, item) = fixture();
    sys.tty = true;
    sys.walks = AccessibilityWalk::Behind;
    sys.trusts(&[false]);
    item.apply(&sys, &ctx)
        .expect("standing aside is no failure");
    assert_eq!(
        item.stood_aside(),
        Some(aterm_converge::StoodAside::Waiting(tab_behind()))
    );
    assert_eq!(count(&sys, "ax-walk"), 1);
    assert_eq!(count(&sys, "ax-request"), 0, "{:?}", sys.calls());
    assert!(
        sys.calls()
            .iter()
            .all(|c| !c.contains(OPEN) && !c.contains(AUTOMATION_ASK)),
        "{:?}",
        sys.calls()
    );
}

/// The window's answer holds macOS's own dialog back. The owner's Not now on the walk's
/// Accessibility page asks nothing: the app comes forward for the person's own Sign Out
/// click, which the run reads. A walk showing other pages gets no dialog beside them: the
/// run reads the grant until it flips and signs out. NEGATIVE CONTROL: with no window to
/// walk it, macOS is asked (`the_aterm_window_walks_the_runs_accessibility_ask`).
#[test]
fn a_not_now_on_the_walks_page_or_a_walk_of_other_pages_raises_no_macos_dialog() {
    let (mut sys, ctx, item) = fixture();
    sys.tty = true;
    sys.walks = AccessibilityWalk::NotNow;
    sys.trusts(&[false]);
    let open = Cmd::new(OPEN)
        .args(["-b", "com.apple.MobileSMS"])
        .as_user(&item.user)
        .in_session();
    sys.on(&open.display(), Ran::ok(""));
    signed_in_for(&sys, &item, 3);
    item.apply(&sys, &ctx).unwrap();
    assert_eq!(item.stood_aside(), None, "signed out by the person's click");
    assert_eq!(count(&sys, "ax-request"), 0, "{:?}", sys.calls());
    assert_eq!(count(&sys, &open.display()), 1, "the app came forward");
    assert!(sys.calls().iter().all(|c| !c.contains(AUTOMATION_ASK)));

    let (mut sys, ctx, item) = fixture();
    sys.tty = true;
    sys.walks = AccessibilityWalk::OtherPages;
    sys.trusts(&[false, false, false, true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 2);
    let rows = engine::run(&[Box::new(item)], Mode::Apply, &sys, &ctx, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0]);
    assert_eq!(count(&sys, "ax-request"), 0, "{:?}", sys.calls());
}

/// Trust that never flips: after two minutes of reads (480, every 250 ms), the app comes
/// forward for the person's own Sign Out click, read for two minutes more; still signed in,
/// the row stands aside naming the app. The run's other sign-out neither asks nor waits for
/// Accessibility again.
#[test]
fn trust_that_never_flips_asks_for_the_click_then_names_the_app() {
    let (mut sys, ctx, item) = fixture();
    sys.tty = true;
    sys.responsible = Some("Terminal".into());
    sys.trusts(&[false]);
    let open = Cmd::new(OPEN)
        .args(["-b", "com.apple.MobileSMS"])
        .as_user(&item.user)
        .in_session();
    sys.on(&open.display(), Ran::ok(""));
    store(&sys, &item, DISABLED);
    prefs(&sys, &item, EMPTY_PREFS);
    item.apply(&sys, &ctx).unwrap();
    let Some(aterm_converge::StoodAside::Permission(why)) = item.stood_aside() else {
        panic!("{:?}", item.stood_aside());
    };
    assert!(why.starts_with("Terminal has no Accessibility"), "{why}");
    assert!(why.contains("`aterm init --sign-out`"), "{why}");
    assert_eq!(count(&sys, "ax-request"), 1);
    assert_eq!(count(&sys, &open.display()), 1);
    assert_eq!(count(&sys, "sleep 250ms"), 480 + 480);
    assert!(sys.calls().iter().all(|c| !c.contains(AUTOMATION_ASK)));

    let other = SignOut {
        service: Service::FaceTime,
        user: item.user.clone(),
        blocked: Mutex::new(None),
        reading: crate::init_observe::LastReading::default(),
        consent: std::sync::Arc::clone(&item.consent),
    };
    let facetime = Cmd::new(OPEN)
        .args(["-b", "com.apple.FaceTime"])
        .as_user(&item.user)
        .in_session();
    sys.on(&facetime.display(), Ran::ok(""));
    sys.on(
        &Cmd::new("/usr/bin/defaults")
            .args(["export", Service::FaceTime.domain(), "-"])
            .as_user(&item.user)
            .display(),
        Ran::ok("<dict><key>ActiveAccounts</key><array><string>a</string></array></dict>"),
    );
    let before = count(&sys, "sleep 250ms");
    other.apply(&sys, &ctx).unwrap();
    assert_eq!(count(&sys, "ax-request"), 1, "asked once for the run");
    assert_eq!(
        count(&sys, "sleep 250ms") - before,
        480,
        "the click's wait alone"
    );

    // The person clicks Sign Out by hand: the run goes on, signed out.
    let (mut sys, ctx, item) = fixture();
    sys.tty = true;
    sys.trusts(&[false]);
    sys.on(&open.display(), Ran::ok(""));
    signed_in_for(&sys, &item, 3);
    item.apply(&sys, &ctx).unwrap();
    assert_eq!(item.stood_aside(), None);
}

/// The app left the front before a click: the script clicks nothing more, and nothing is
/// read for a deregistration it did not ask for. Whose doing that was the session says,
/// read again at once: the person's own keys or pointer took the front (or the screen locked),
/// so the sign-out waits for them to rest and is no failed try; with the session still open,
/// the try failed. Before, either was a failed try.
#[test]
fn an_app_that_leaves_the_front_ends_the_clicks() {
    for (taken, failed) in [(Some(SessionReach::KeysInUse), false), (None, true)] {
        let (sys, ctx, item) = fixture();
        sys.trusts(&[true]);
        clicks(&sys, &item, "not-frontmost");
        let script = clicking(&item);
        sys.simulate(move |fake, cmd| {
            (cmd.display() == script).then(|| {
                fake.session.lock().unwrap().clone_from(&taken);
                Ran::ok("not-frontmost")
            })
        });
        let applied = item.apply(&sys, &ctx);
        assert_eq!(count(&sys, &clicking(&item)), 1, "{:?}", sys.calls());
        if failed {
            let why = applied.unwrap_err();
            assert!(why.starts_with("Messages left the front"), "{why}");
            assert_eq!(item.stood_aside(), None);
        } else {
            applied.expect("the person's keys are no failure");
            let Some(aterm_converge::StoodAside::Waiting(why)) = item.stood_aside() else {
                panic!("{:?}", item.stood_aside());
            };
            assert!(
                why.starts_with("the keyboard or pointer is in use"),
                "{why}"
            );
            assert!(looks_again(&report_of(&item)));
        }
        assert!(
            sys.calls().iter().all(|c| !c.contains("defaults")),
            "{:?}",
            sys.calls()
        );
    }
}

/// Through the workstation pass's snapshot guard (`init::Guard`), which records an attempt
/// before each apply: a sign-out whose app lost the front to the person's own keys stood
/// aside, so the pass keeps no attempt of it, and no failed try is counted toward the
/// build's two. CONTROL: the same lost front with the session still open is this try's own
/// failure, an attempt that did not verify.
#[test]
fn a_front_lost_to_the_persons_keys_is_no_attempt_of_the_workstation_pass() {
    for (taken, attempted) in [(Some(SessionReach::KeysInUse), false), (None, true)] {
        let (sys, ctx, item) = fixture();
        sys.trusts(&[true]);
        clicks(&sys, &item, "not-frontmost");
        signed_in_for(&sys, &item, 1);
        let script = clicking(&item);
        sys.simulate(move |fake, cmd| {
            (cmd.display() == script).then(|| {
                fake.session.lock().unwrap().clone_from(&taken);
                Ran::ok("not-frontmost")
            })
        });
        let run = crate::init::Workstation::of_in(ctx, vec![Box::new(item)])
            .run_recording(&sys, Mode::Apply)
            .expect("the pass runs");
        assert_eq!(run.rows.len(), 1, "{:?}", run.rows);
        if attempted {
            assert_eq!(run.attempts.len(), 1, "{:?}", run.attempts);
            assert!(!run.attempts[0].verified);
            assert_eq!(run.rows[0].stood_aside, None, "{:?}", run.rows[0]);
        } else {
            assert!(run.attempts.is_empty(), "{:?}", run.attempts);
            assert!(looks_again(&run.rows[0]), "{:?}", run.rows[0]);
        }
    }
}

/// A refused Automation question stands aside before any app opens, naming the switch
/// System Settings lists for it, and asks nothing again.
#[test]
fn a_refused_automation_question_opens_no_app() {
    let (sys, ctx, item) = fixture();
    sys.trusts(&[true]);
    *sys.automation.lock().unwrap() = Some(AutomationConsent::Refused);
    item.apply(&sys, &ctx).unwrap();
    assert_eq!(
        item.stood_aside(),
        Some(aterm_converge::StoodAside::Permission(
            AUTOMATION_OFF.into()
        ))
    );
    assert!(
        sys.calls()
            .iter()
            .all(|c| !c.contains("Sign Out") && !c.contains(OPEN))
    );
}

/// System Events refusing the clicks for want of assistive access (-25211) is Accessibility,
/// not Automation: the sign-out stands aside on the Accessibility switch, whose grant's
/// access-record line wakes the run, and names no Automation switch it cannot use. A refusal
/// of the Apple events themselves (-1743) still names Automation. Before, both read as
/// Automation off.
#[test]
fn a_click_refused_for_assistive_access_stands_aside_on_accessibility() {
    for (outcome, names, not) in [
        ("accessibility-blocked", "Accessibility", "Automation"),
        ("automation-blocked", "Automation", "Accessibility"),
    ] {
        let (sys, ctx, item) = fixture();
        sys.trusts(&[true]);
        clicks(&sys, &item, outcome);
        item.apply(&sys, &ctx).expect("a refusal stands aside");
        let Some(aterm_converge::StoodAside::Permission(why)) = item.stood_aside() else {
            panic!("{outcome}: {:?}", item.stood_aside());
        };
        assert!(
            why.contains(&format!("Privacy & Security \u{25b8} {names}")),
            "{why}"
        );
        assert!(!why.contains(not), "{outcome}: {why}");
    }
    // The script names the two refusals apart.
    assert!(SIGN_OUT.contains("errorNumber is -25211 then return \"accessibility-blocked\""));
    assert!(SIGN_OUT.contains("errorNumber is -1743 then return \"automation-blocked\""));
}

/// With no terminal nothing the sign-out reads waits on another dialog of aterm's: the
/// session, the grant and macOS's Automation answer are read first, and the dialog slot is
/// taken, without waiting, only for the clicks. macOS's Automation question still out (the
/// window asking it, the slot taken) reads unanswered, so the sign-out waits on that answer
/// and takes no slot; with every answer in, a dialog up is a wait that passes by itself
/// ([`DIALOG_UP`], [`looks_again`]), clicking nothing. At a terminal the slot is waited for, as
/// the person there waits, before anything is asked. Before, the unattended sign-out took the
/// slot first, waiting a minute behind the window's own question, and its words handed the
/// person a remedy.
#[test]
fn an_unattended_sign_out_reads_everything_before_it_takes_the_dialog_slot() {
    use std::sync::atomic::Ordering;
    let (sys, ctx, item) = fixture();
    sys.trusts(&[true]);
    sys.dialog_up.store(true, Ordering::SeqCst);
    *sys.automation.lock().unwrap() = Some(AutomationConsent::Unanswered);
    item.apply(&sys, &ctx).unwrap();
    let Some(aterm_converge::StoodAside::Waiting(why)) = item.stood_aside() else {
        panic!("{:?}", item.stood_aside());
    };
    assert_eq!(why, automation_unanswered(AutomationConsent::Unanswered));
    assert_eq!(count(&sys, "dialog-slot"), 0, "{:?}", sys.calls());

    let (sys, ctx, item) = fixture();
    sys.trusts(&[true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 1);
    sys.dialog_up.store(true, Ordering::SeqCst);
    item.apply(&sys, &ctx).unwrap();
    assert_eq!(
        item.stood_aside(),
        Some(aterm_converge::StoodAside::Waiting(DIALOG_UP.into()))
    );
    assert!(
        looks_again(&report_of(&item)),
        "a wait with its own schedule"
    );
    assert_eq!(count(&sys, &clicking(&item)), 0);
    let calls = sys.calls();
    let at = |needle: &str| calls.iter().position(|c| c == needle).unwrap();
    assert!(at("session-reach") < at("automation-consent"), "{calls:?}");
    assert!(at("automation-consent") < at("dialog-slot"), "{calls:?}");
    // CONTROL: the slot free, the clicks go.
    sys.dialog_up.store(false, Ordering::SeqCst);
    item.apply(&sys, &ctx).unwrap();
    assert_eq!(count(&sys, &clicking(&item)), 1, "{:?}", sys.calls());

    // At a terminal the slot comes first and is waited for.
    let (mut sys, ctx, item) = fixture();
    sys.tty = true;
    sys.trusts(&[true]);
    sys.dialog_up.store(true, Ordering::SeqCst);
    *sys.automation.lock().unwrap() = Some(AutomationConsent::Unanswered);
    item.apply(&sys, &ctx).unwrap();
    assert_eq!(count(&sys, "dialog-slot wait"), 1, "{:?}", sys.calls());
    assert_eq!(count(&sys, &automation_ask(&item).display()), 0);
}

/// A FaceTime sign-out sharing `item`'s run.
fn facetime_beside(item: &SignOut) -> SignOut {
    SignOut {
        service: Service::FaceTime,
        user: item.user.clone(),
        blocked: Mutex::new(None),
        reading: crate::init_observe::LastReading::default(),
        consent: std::sync::Arc::clone(&item.consent),
    }
}

/// With no terminal (System State's run) the sign-outs read macOS's Automation answer and
/// never ask its question: unanswered, they wait, calmly, with no System Settings switch to
/// name (none exists before macOS asked) and no command, open no app, and look again by
/// themselves; both sign-outs share the one reading. Before, the run asked the question
/// itself, waited two minutes on it, and called no answer a refusal.
#[test]
fn an_unanswered_automation_question_is_no_refusal_and_asks_nothing() {
    let (sys, ctx, item) = fixture();
    sys.trusts(&[true, true]);
    *sys.automation.lock().unwrap() = Some(AutomationConsent::Unanswered);
    item.apply(&sys, &ctx).unwrap();
    let other = facetime_beside(&item);
    other.apply(&sys, &ctx).unwrap();
    for item in [&item, &other] {
        let Some(aterm_converge::StoodAside::Waiting(why)) = item.stood_aside() else {
            panic!("{:?}", item.stood_aside());
        };
        assert!(
            !why.contains("System Settings") && !why.contains("is off"),
            "{why}"
        );
        assert!(!why.contains("aterm init"), "{why}");
        assert!(
            why.contains(aterm_types::macos_setup::StageRow::AUTOMATION_UNANSWERED),
            "the window reads these words to ask the question: {why}"
        );
        assert!(looks_again(&report_of(item)), "{why}");
    }
    assert_eq!(count(&sys, "automation-consent"), 1, "{:?}", sys.calls());
    assert!(
        sys.calls()
            .iter()
            .all(|c| !c.contains(AUTOMATION_ASK) && !c.contains("Sign Out") && !c.contains(OPEN)),
        "{:?}",
        sys.calls()
    );
    // CONTROL: at a terminal the person is there, and macOS is asked once; no answer in its
    // two minutes is still unanswered, never off.
    let (mut sys, ctx, item) = fixture();
    sys.tty = true;
    sys.trusts(&[true]);
    *sys.automation.lock().unwrap() = Some(AutomationConsent::Unanswered);
    sys.time_out(&automation_ask(&item).display());
    item.apply(&sys, &ctx).unwrap();
    assert_eq!(count(&sys, &automation_ask(&item).display()), 1);
    assert!(
        matches!(item.stood_aside(), Some(aterm_converge::StoodAside::Waiting(ref why)) if why.starts_with("macOS has had no answer")),
        "{:?}",
        item.stood_aside()
    );
}

/// macOS reads nothing of a target that is not running: System Events, a background app, is
/// started, and its answer read again, before the sign-out goes on.
#[test]
fn system_events_is_started_for_its_automation_answer() {
    let (sys, ctx, item) = fixture();
    sys.trusts(&[true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 1);
    *sys.automation.lock().unwrap() = Some(AutomationConsent::Unread);
    let start = Cmd::new(OPEN)
        .args(["-g", "-j", "-b", SYSTEM_EVENTS])
        .as_user(&item.user)
        .in_session()
        .display();
    let started = start.clone();
    sys.simulate(move |fake, cmd| {
        (cmd.display() == started).then(|| {
            *fake.automation.lock().unwrap() = Some(AutomationConsent::Allowed);
            Ran::ok("")
        })
    });
    item.apply(&sys, &ctx).expect("signed out");
    assert_eq!(count(&sys, &start), 1);
    assert_eq!(count(&sys, &clicking(&item)), 1, "{:?}", sys.calls());
    assert_eq!(item.stood_aside(), None);
}

#[test]
fn a_consent_request_is_answered_in_this_process() {
    for (readings, expected) in [
        (Some(false), Some(false)),
        (Some(true), Some(true)),
        (None, None),
    ] {
        let sys = Fake::root();
        if let Some(trusted) = readings {
            sys.trusts(&[trusted]);
        }
        assert_eq!(request_accessibility(&sys).ok(), expected);
        assert_eq!(sys.calls(), ["ax-request"]);
    }
}

#[test]
fn unsupported_localized_labels_refuse_before_launching_the_app() {
    let (sys, ctx, item) = fixture();
    sys.trusts(&[true]);
    sys.on(
        &labels_cmd(&item.user, item.service).display(),
        Ran::ok(r#"{"panes":[],"sign_out":[]}"#),
    );
    assert!(item.apply(&sys, &ctx).is_err());
    // The labels' read is the last thing run: nothing launched after it.
    let calls = sys.calls();
    assert_eq!(
        calls.last(),
        Some(&labels_cmd(&item.user, item.service).display()),
        "{calls:?}"
    );
    assert!(calls.iter().all(|c| !c.contains("keystroke")));
}

#[test]
fn an_expired_shared_deadline_refuses_before_running_another_child() {
    let (sys, _ctx, item) = fixture();
    let expired = Instant::now() - Duration::from_secs(60);
    assert!(localized_labels(&sys, &item.user, item.service, Some(expired)).is_err());
    assert!(sys.calls().is_empty());
}

#[test]
fn ui_exit_success_requires_removal_and_readback() {
    for removed in [false, true] {
        let (sys, ctx, item) = fixture();
        sys.trusts(&[true]);
        clicks(&sys, &item, "sign-out-requested");
        if removed {
            signed_in_for(&sys, &item, 2);
        } else {
            store(&sys, &item, DISABLED);
            prefs(&sys, &item, EMPTY_PREFS);
        }
        let rows = engine::run(&[Box::new(item)], Mode::Apply, &sys, &ctx, &mut NoJournal);
        assert_eq!(
            rows[0].outcome,
            if removed {
                Outcome::Changed
            } else {
                Outcome::Failed
            }
        );
        assert!(sys.calls().iter().all(|c| !c.contains("private-token")));
        assert!(sys.calls().iter().all(|c| !c.contains("/bin/sleep")));
    }
}

#[test]
fn sign_out_applescript_walks_ui_elements_and_matches_catalyst_descriptions() {
    assert!(
        !SIGN_OUT.contains("entire contents"),
        "macOS 27 Tahoe System Events returns 0 elements for `entire contents` on Messages/FaceTime windows"
    );
    assert!(SIGN_OUT.contains("UI elements of containerElement"));
    assert!(SIGN_OUT.contains("description of elementRef as string"));
    assert!(SIGN_OUT.contains("AXMenuItemCmdChar"));
    assert!(SIGN_OUT.contains("with timeout of 45 seconds"));
    assert!(SIGN_OUT.contains("error errorText"));
}

/// The script never types a key, and every click it makes for the sign-out comes right after
/// a look that the app is still in front.
#[test]
fn every_sign_out_click_follows_a_frontmost_check_and_nothing_is_typed() {
    assert!(!SIGN_OUT.contains("keystroke"));
    let lines: Vec<&str> = SIGN_OUT.lines().map(str::trim).collect();
    let clicks: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            !line.starts_with("--") && line.contains("click ") && !line.contains("closeButton")
        })
        .map(|(at, _)| at)
        .collect();
    assert_eq!(clicks.len(), 4, "{clicks:?}");
    for at in clicks {
        assert_eq!(lines[at - 1], "my mustBeFront(appName)", "{}", lines[at]);
    }
}

/// AN AGENT'S RUN PUTS NOTHING ON THE SCREEN (owner ruling 2026-11-09): a sign-out reached
/// by a run a coding agent started asks macOS nothing (no Accessibility read or walk, no
/// Automation question), brings no app forward and clicks nothing, with a person at the
/// screen or not; it stands aside in words that say whose run it was. NEGATIVE CONTROL: the
/// same Mac with no agent above the run signs out.
#[test]
fn a_sign_out_in_an_agents_run_asks_and_clicks_nothing() {
    let (mut sys, ctx, item) = fixture();
    sys.agent = Some("claude");
    sys.trusts(&[true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 1);
    item.apply(&sys, &ctx)
        .expect("standing aside is no failure");
    assert_eq!(
        sys.calls(),
        Vec::<String>::new(),
        "an agent's run starts nothing for a sign-out"
    );
    assert_eq!(
        item.stood_aside(),
        Some(aterm_converge::StoodAside::Waiting(agents_run()))
    );
    // CONTROL: the person's own run clicks.
    let (sys, ctx, item) = fixture();
    sys.trusts(&[true]);
    clicks(&sys, &item, "sign-out-requested");
    signed_in_for(&sys, &item, 1);
    item.apply(&sys, &ctx).expect("signed out");
    assert_eq!(count(&sys, &clicking(&item)), 1, "{:?}", sys.calls());
    assert_eq!(item.stood_aside(), None);
}
