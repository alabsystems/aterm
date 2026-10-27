// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The permission walk through the App: one panel turned from page to page as each grant reads
//! on, a held grant skipped without its page, a Not now going on, the start on a cold first
//! launch, a control client's walk, and the Accessibility grant reaching the setup worker. The
//! pages, panels and grants are counted stand-ins; nothing reaches macOS.

use std::cell::Cell;

use super::*;
use crate::devtools_helper::{
    DONE_FOR, DevToolsHelperArm, Lane, Origin, Page, PanelEvent, Phase, test_arms,
};
use crate::permission_walk::{
    GRANT_POLL, START_KEYS_QUIET, Started, Subject, UNREAD_WAIT, Walk, WalkLane, WalkReach,
};
use aterm_containment::DevToolGrant;
use aterm_containment::consent::FdaState;

thread_local! {
    static SHOWS: Cell<u32> = const { Cell::new(0) };
    static UPDATES: Cell<u32> = const { Cell::new(0) };
    static CLOSES: Cell<u32> = const { Cell::new(0) };
    static AX_TRUSTED: Cell<bool> = const { Cell::new(false) };
    static AUTOMATION_ASKS: Cell<u32> = const { Cell::new(0) };
}

fn bump(cell: &'static std::thread::LocalKey<Cell<u32>>) {
    cell.with(|n| n.set(n.get() + 1));
}

/// The panel's shows, in-place turns and closes so far.
fn panel_counts() -> (u32, u32, u32) {
    (
        SHOWS.with(Cell::get),
        UPDATES.with(Cell::get),
        CLOSES.with(Cell::get),
    )
}

/// A panel whose show, turn and close are counted; System Settings runs but is not in front, so
/// the panel reads no list of its own (the tests post each reading).
fn counting_arm() -> DevToolsHelperArm {
    test_arms::counted(
        |_, _| {
            bump(&SHOWS);
            true
        },
        |_| bump(&UPDATES),
        || bump(&CLOSES),
        || false,
    )
}

fn walk_reach() -> WalkReach {
    WalkReach {
        ax_trusted: || AX_TRUSTED.with(Cell::get),
        ask_automation: || bump(&AUTOMATION_ASKS),
    }
}

/// A new Mac at revision 1 with System State in front: Full Disk Access and Developer Tools
/// read off (aterm in a list of two, switched off), the pages and panel counted, the walk's
/// reach injected. `devtools` is the Developer Tools page's reach.
fn walk_app_with(devtools: crate::devtools_helper::Reach) -> App {
    use aterm_containment::consent::{DevToolProbe, DevToolProbeLabel, ProbeGate};
    fn no_probe(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        DevToolProbe::refused(DevToolProbeLabel::RefusedDisabled)
    }
    set_front(true);
    AX_TRUSTED.with(|trusted| trusted.set(false));
    RECORDS.with(|records| records.borrow_mut().clear());
    let mut app = new_mac(devtools, test_arms::fda_opening());
    app.devtools_helper =
        Lane::windowed_with_fda_for_test(devtools, test_arms::fda_opening(), counting_arm());
    app.consent
        .set_devtools_arms_for_test(no_probe, || Some(test_arms::SUBJECT));
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
        Instant::now(),
    );
    let _ = app.devtools_watch.observe(DevToolGrant::NotGranted, true);
    app.permission_walk = WalkLane::with_reach(walk_reach());
    app
}

fn walk_app() -> App {
    walk_app_with(test_arms::opening())
}

fn grant_fda(app: &mut App) {
    app.consent =
        crate::consent_state::ConsentState::with_cached_probe_for_test(probe(FdaState::Granted));
}

fn page_and_step(app: &App) -> Option<(Page, Option<(usize, usize)>)> {
    app.devtools_helper
        .panel()
        .map(|panel| (panel.page(), panel.step()))
}

/// Full Disk Access reads on while the walk's page is up: the next quarter-second look shows
/// the tick, and a second later the same panel turns to Developer Tools, whose page opens. The
/// control: the same grant on a page a press opened (no walk) closes the panel and opens nothing.
#[test]
fn the_walk_turns_from_full_disk_access_to_developer_tools_in_the_same_panel() {
    let t0 = Instant::now();
    let mut app = walk_app();
    let (fda_opens, opens, panels) = (
        test_arms::fda_counts().0,
        test_arms::opens(),
        panel_counts(),
    );
    assert_eq!(
        app.start_permission_walk(&[], "a test", t0),
        Started::Walking(vec![Subject::FullDiskAccess, Subject::DeveloperTools])
    );
    assert_eq!(test_arms::fda_counts().0, fda_opens + 1, "its page opened");
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2))))
    );

    grant_fda(&mut app);
    let granted = t0 + GRANT_POLL;
    app.tick_consent_lanes(granted);
    assert_eq!(
        app.devtools_helper.panel().map(|panel| panel.phase()),
        Some(Phase::Done),
        "the tick, at the first look after the grant"
    );
    app.tick_consent_lanes(granted + DONE_FOR);
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((2, 2)))),
        "a second after the tick, the next page"
    );
    assert_eq!(
        test_arms::opens(),
        opens + 1,
        "Developer Tools' page opened"
    );
    let (shows, turns, closes) = panel_counts();
    assert_eq!(
        (shows, closes),
        (panels.0 + 1, panels.2),
        "one panel, never closed"
    );
    assert!(turns > panels.1, "turned in place");

    // The control: a press's page, no walk.
    let mut app = walk_app();
    let opens = test_arms::opens();
    #[expect(
        clippy::disallowed_methods,
        reason = "the control opens the press's page through its own door"
    )]
    let opened = app.open_fda_page(Origin::Settings, true, t0);
    assert_eq!(opened, crate::devtools_helper::HandOver::Opened);
    grant_fda(&mut app);
    app.tick_consent_lanes(granted);
    app.tick_consent_lanes(granted + DONE_FOR);
    assert!(app.devtools_helper.panel().is_none(), "the panel closed");
    assert_eq!(test_arms::opens(), opens, "and nothing came next");
}

/// A grant that reads held by the time the walk reaches it is skipped without its page: the
/// walk ends, its panel goes, and the Full Disk Access page never opens.
#[test]
fn a_grant_held_by_its_turn_is_skipped_without_its_page() {
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(&[Subject::DeveloperTools], "a test", t0),
        Started::Walking(vec![Subject::DeveloperTools, Subject::FullDiskAccess])
    );
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((1, 2))))
    );
    let fda_opens = test_arms::fda_counts().0;
    let closes = panel_counts().2;
    // Full Disk Access is switched on meanwhile; then Developer Tools reads on.
    grant_fda(&mut app);
    app.devtools_helper.read_for_test(test_arms::on());
    app.tick_consent_lanes(t0 + GRANT_POLL);
    app.tick_consent_lanes(t0 + GRANT_POLL + DONE_FOR);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens,
        "no page for a held grant"
    );
    assert!(!app.permission_walk.active(), "the walk is over");
    assert!(app.devtools_helper.panel().is_none());
    assert_eq!(panel_counts().2, closes + 1, "its panel went");
}

/// Not now on the walk's own Developer Tools page goes on to the debugger, which the walk hands
/// to System State's once-per-Mac step: the panel goes, the step looks at once instead of at its
/// next look, and the walk itself starts no change (it is no person's press on the row's
/// `Turn On…`; the census's `devtools-door` fence holds it off that door). Control: with no
/// step queued the walk skips the debugger and no look is brought forward. A Not now on a page
/// asked for by name ends the walk there.
#[test]
fn not_now_on_developer_tools_goes_on_to_the_debugger_step() {
    use crate::debugger_access::DebuggerStepQueue;
    let t0 = Instant::now();
    let later = t0 + Duration::from_secs(600);
    for queued in [true, false] {
        let mut app = walk_app();
        app.debugger_launch = if queued {
            DebuggerStepQueue::queued_for_test(|| Some("MAC-A"))
        } else {
            DebuggerStepQueue::new(false)
        };
        // The step waits for its moment, ten minutes out.
        app.debugger_launch.look_later(later);
        app.permission_walk.start(
            Walk::new(vec![Subject::DeveloperTools, Subject::Debugger], t0).expect("two pages"),
            "first launch",
        );
        app.drive_permission_walk(t0);
        assert_eq!(
            page_and_step(&app),
            Some((Page::DeveloperTools, Some((1, 2))))
        );
        app.devtools_helper.press_for_test(PanelEvent::NotNow);
        app.tick_devtools_hand_over(t0);
        let walk = app.permission_walk.walk().expect("the walk goes on");
        assert_eq!(walk.current(), Some(Subject::Debugger));
        assert_eq!(walk.position(), (2, 2));
        let closes = panel_counts().2;
        let at = t0 + UNREAD_WAIT;
        assert_eq!(
            app.debugger_launch.next_deadline(),
            queued.then_some(later),
            "the step still waits for its moment"
        );
        app.drive_permission_walk(at);
        assert_eq!(panel_counts().2, closes + 1, "the panel goes");
        assert!(!app.permission_walk.active(), "the debugger was the last");
        assert!(
            !app.debugger_change.under_way(),
            "the walk starts no change"
        );
        if queued {
            assert_eq!(
                app.debugger_launch.next_deadline(),
                Some(at),
                "System State's step looks at once"
            );
        } else {
            assert_eq!(
                app.debugger_launch.next_deadline(),
                None,
                "no step to bring forward"
            );
        }
    }

    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(&[Subject::DeveloperTools], "a person's press", t0),
        Started::Walking(vec![Subject::DeveloperTools, Subject::FullDiskAccess])
    );
    let fda_opens = test_arms::fda_counts().0;
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(t0);
    assert!(
        !app.permission_walk.active(),
        "Not now on the page asked for"
    );
    app.tick_consent_lanes(t0 + GRANT_POLL);
    assert_eq!(test_arms::fda_counts().0, fda_opens);
    assert!(app.devtools_helper.panel().is_none());

    // `aterm doctor` names both: Not now on the first goes on to the second, also asked for.
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(
            &[Subject::DeveloperTools, Subject::FullDiskAccess],
            "a control client",
            t0
        ),
        Started::Walking(vec![Subject::DeveloperTools, Subject::FullDiskAccess])
    );
    let fda_opens = test_arms::fda_counts().0;
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(t0);
    assert_eq!(
        app.permission_walk.walk().and_then(Walk::current),
        Some(Subject::FullDiskAccess),
        "the next page asked for"
    );
    app.tick_consent_lanes(t0 + GRANT_POLL);
    assert_eq!(test_arms::fda_counts().0, fda_opens + 1, "its page opened");
}

/// The walk starts by itself on a cold first launch with System State in front and the person
/// there (input 30 s ago) and not typing: a key 1 s ago holds it until the keyboard has rested
/// [`START_KEYS_QUIET`]. An update's successor, a Mac whose pages came up before, nobody at the
/// Mac for a minute, or System State away never starts it.
#[test]
fn the_walk_starts_on_a_cold_first_launch_once_the_keyboard_rests() {
    fn starts(app: &mut App, now: Instant) -> bool {
        app.tick_permission_walk(now);
        app.permission_walk.active()
    }
    fn cold() -> App {
        let mut app = walk_app();
        app.devtools_helper.note_cold_start();
        app.user_input_recent = |within| within >= Duration::from_secs(30);
        app
    }
    let t0 = Instant::now();
    let mut app = cold();
    app.last_keystroke_at = Some(t0 - Duration::from_secs(1));
    assert!(!starts(&mut app, t0), "a key typed 1 s ago");
    assert!(
        starts(&mut app, t0 - Duration::from_secs(1) + START_KEYS_QUIET),
        "the keyboard rested 2 s"
    );
    assert_eq!(
        app.permission_walk
            .walk()
            .map(|walk| walk.subjects().to_vec()),
        Some(vec![Subject::FullDiskAccess, Subject::DeveloperTools])
    );

    let later = t0 + Duration::from_secs(10);
    let mut successor = walk_app();
    successor.user_input_recent = |_| true;
    assert!(!starts(&mut successor, later), "an update's successor");

    let mut seen = cold();
    seen.devtools_helper = Lane::windowed_with_fda_for_test(
        test_arms::marked(|| Some((atpkg::devtools::revision_key(1), 1))),
        test_arms::fda_opening(),
        counting_arm(),
    );
    seen.devtools_helper.note_cold_start();
    assert!(!starts(&mut seen, later), "a page came up before");

    let mut away = cold();
    away.user_input_recent = |_| false;
    assert!(!starts(&mut away, later), "nobody at the Mac for a minute");

    let mut elsewhere = cold();
    set_front(false);
    assert!(!starts(&mut elsewhere, later), "System State not in front");
    set_front(true);
    assert!(starts(&mut elsewhere, later), "and once it is");
}

/// `aterm doctor`'s Developer Tools hand-over is `permissions walk now devtools`: once
/// Developer Tools reads on, Full Disk Access's page comes up in the same panel, though the
/// Developer Tools page just opened. The control: today's automatic Full Disk Access hand-over
/// after that page waits out its ten minutes.
#[test]
fn a_walk_doctor_starts_admits_full_disk_access_right_after_developer_tools() {
    let t0 = Instant::now();
    let just_opened = test_arms::opened(|_| Some(0));
    let mut app = walk_app_with(just_opened);
    assert_eq!(
        app.start_permission_walk(&[Subject::DeveloperTools], "a control client", t0),
        Started::Walking(vec![Subject::DeveloperTools, Subject::FullDiskAccess])
    );
    let fda_opens = test_arms::fda_counts().0;
    app.devtools_helper.read_for_test(test_arms::on());
    app.tick_consent_lanes(t0 + GRANT_POLL);
    app.tick_consent_lanes(t0 + GRANT_POLL + DONE_FOR);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens + 1,
        "Full Disk Access' page"
    );
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((2, 2))))
    );

    let mut app = walk_app_with(just_opened);
    let fda_opens = test_arms::fda_counts().0;
    assert_eq!(
        app.fda_hand_over(
            Origin::Automatic,
            crate::devtools_helper::Moment::Asked,
            FdaState::Denied
        ),
        crate::devtools_helper::HandOver::Queued,
        "ten minutes after the Developer Tools page"
    );
    app.tick_consent_lanes(t0 + GRANT_POLL + DONE_FOR);
    assert_eq!(test_arms::fda_counts().0, fda_opens);
}

/// The Accessibility page's grant, read in-process, is recorded at the next quarter-second look:
/// that record line is what wakes the setup worker (`access_record::on_append`). Automation for
/// System Events is asked right after it. Until the grant reads on, neither happens.
#[test]
fn an_accessibility_grant_is_recorded_for_the_setup_worker_and_asks_automation() {
    let t0 = Instant::now();
    let mut app = walk_app();
    let asks = AUTOMATION_ASKS.with(Cell::get);
    assert_eq!(
        app.start_permission_walk(&[Subject::Accessibility], "a test", t0),
        Started::Walking(vec![
            Subject::Accessibility,
            Subject::FullDiskAccess,
            Subject::DeveloperTools
        ])
    );
    assert_eq!(
        page_and_step(&app),
        Some((Page::Accessibility, Some((1, 3))))
    );
    let granted = |revision: u64| {
        RECORDS.with(|records| {
            records
                .borrow()
                .contains(&(revision, Access::Accessibility, AccessOutcome::Granted))
        })
    };
    app.tick_consent_lanes(t0 + GRANT_POLL);
    assert!(!granted(1), "not yet trusted");
    assert_eq!(AUTOMATION_ASKS.with(Cell::get), asks);

    AX_TRUSTED.with(|trusted| trusted.set(true));
    app.tick_consent_lanes(t0 + GRANT_POLL * 2);
    assert!(granted(1), "the grant's record line");
    assert_eq!(AUTOMATION_ASKS.with(Cell::get), asks + 1, "Automation next");
    app.tick_consent_lanes(t0 + GRANT_POLL * 2 + DONE_FOR);
    assert_eq!(
        page_and_step(&app).map(|(page, _)| page),
        Some(Page::FullDiskAccess),
        "and the walk goes on"
    );
}

/// The control verb brings System State's walk up at most once a minute: any program in a
/// session can send it. A second ask while the walk is under way names that walk; one after the
/// walk ended, inside the minute, answers recent and opens nothing; a minute on it starts again.
/// NEGATIVE CONTROL: a person's start inside that minute is not held.
#[test]
fn the_control_verb_starts_a_walk_at_most_once_a_minute() {
    use crate::permission_walk::VERB_AGAIN_AFTER;
    let t0 = Instant::now();
    let mut app = walk_app();
    let walking = Started::Walking(vec![Subject::DeveloperTools, Subject::FullDiskAccess]);
    assert_eq!(
        app.ask_permission_walk(&[Subject::DeveloperTools], t0),
        walking
    );
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess], t0),
        walking,
        "one under way keeps going and names its own"
    );
    app.permission_walk.end("a test");
    let opens = (test_arms::opens(), test_arms::fda_counts().0);
    let inside = t0 + VERB_AGAIN_AFTER - Duration::from_secs(1);
    assert_eq!(
        app.ask_permission_walk(&[Subject::DeveloperTools], inside),
        Started::Recent
    );
    assert!(!app.permission_walk.active());
    assert_eq!(
        (test_arms::opens(), test_arms::fda_counts().0),
        opens,
        "nothing opened"
    );
    assert_eq!(
        app.start_permission_walk(&[Subject::DeveloperTools], "a person's press", inside),
        walking,
        "a person's press is not the verb"
    );
    app.permission_walk.end("a test");
    assert_eq!(
        app.ask_permission_walk(&[Subject::DeveloperTools], t0 + VERB_AGAIN_AFTER),
        walking,
        "a minute on"
    );
}

/// The owner's Not now holds a page back from the control verb's walk, as it held the verb's
/// hand-overs: a declined Developer Tools page leaves the walk Full Disk Access alone, and with
/// both declined the verb answers declined and opens nothing. NEGATIVE CONTROL: a person's press
/// over the same declines walks both pages.
#[test]
fn the_owners_not_now_holds_a_page_back_from_the_control_verbs_walk() {
    let t0 = Instant::now();
    let mut app = walk_app_with(test_arms::declining());
    assert_eq!(
        app.ask_permission_walk(&[Subject::DeveloperTools, Subject::FullDiskAccess], t0),
        Started::Walking(vec![Subject::FullDiskAccess])
    );

    let mut app = walk_app_with(test_arms::declining());
    app.devtools_helper = Lane::windowed_with_fda_for_test(
        test_arms::declining(),
        test_arms::fda_declined(),
        counting_arm(),
    );
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
        t0,
    );
    let opens = (test_arms::opens(), test_arms::fda_counts().0);
    assert_eq!(app.ask_permission_walk(&[], t0), Started::Declined);
    assert!(!app.permission_walk.active());
    assert_eq!((test_arms::opens(), test_arms::fda_counts().0), opens);
    assert_eq!(
        app.start_permission_walk(&[], "a person's press", t0),
        Started::Walking(vec![Subject::FullDiskAccess, Subject::DeveloperTools]),
        "the owner's own press goes past a Not now"
    );
}

thread_local! {
    static MARKER_READS: Cell<u32> = const { Cell::new(0) };
}

/// The walk's start is looked at every park, so it reads the page markers only once every
/// in-memory reading says start: an update's successor, or a window whose System State is
/// away, reads none, and once a look finds one no later look reads them again. NEGATIVE
/// CONTROL: the look that would start the walk reads them.
#[test]
fn the_walks_start_reads_the_page_markers_last_and_never_again_once_one_is_found() {
    fn unseen() -> Option<(String, u64)> {
        bump(&MARKER_READS);
        None
    }
    fn seen() -> Option<(String, u64)> {
        bump(&MARKER_READS);
        Some((atpkg::devtools::revision_key(1), 1))
    }
    fn present(devtools: crate::devtools_helper::Reach, cold: bool) -> App {
        let mut app = walk_app_with(devtools);
        if cold {
            app.devtools_helper.note_cold_start();
        }
        app.user_input_recent = |_| true;
        app
    }
    let reads = || MARKER_READS.with(Cell::get);
    let t0 = Instant::now();
    let ticks = |app: &mut App, n: u32| {
        for tick in 0..n {
            app.tick_permission_walk(t0 + GRANT_POLL * tick);
        }
    };

    let mut successor = present(test_arms::marked(unseen), false);
    let before = reads();
    ticks(&mut successor, 3);
    assert_eq!(reads(), before, "an update's successor reads no marker");

    let mut away = present(test_arms::marked(unseen), true);
    set_front(false);
    ticks(&mut away, 3);
    set_front(true);
    assert_eq!(reads(), before, "System State away: no marker read");

    let mut seen_before = present(test_arms::marked(seen), true);
    ticks(&mut seen_before, 1);
    let first = reads();
    assert!(
        first > before,
        "the look that would start the walk reads them"
    );
    ticks(&mut seen_before, 4);
    assert_eq!(reads(), first, "a marker found is never read again");
    assert!(!seen_before.permission_walk.active());
}

/// A walk that starts by itself leaves out a page the owner said Not now to (a re-ask after a
/// decline is the automatic hand-over's, at a quiet moment), and does not start over another
/// of the window's gestures. NEGATIVE CONTROL: a person's press over the same decline walks
/// that page (`the_owners_not_now_holds_a_page_back_from_the_control_verbs_walk`).
#[test]
fn a_walk_by_itself_leaves_a_declined_page_and_never_starts_over_another_gesture() {
    fn cold(devtools: crate::devtools_helper::Reach) -> App {
        let mut app = walk_app_with(devtools);
        app.devtools_helper.note_cold_start();
        app.user_input_recent = |_| true;
        app
    }
    let t0 = Instant::now();
    let mut declined = cold(test_arms::declining());
    declined.tick_permission_walk(t0);
    assert_eq!(
        declined
            .permission_walk
            .walk()
            .map(|walk| walk.subjects().to_vec()),
        Some(vec![Subject::FullDiskAccess]),
        "Developer Tools was declined"
    );

    let mut busy = cold(test_arms::opening());
    #[expect(
        clippy::disallowed_methods,
        reason = "a press's page, through its own door, is the gesture up"
    )]
    let opened = busy.open_fda_page(Origin::Settings, true, t0);
    assert_eq!(opened, crate::devtools_helper::HandOver::Opened);
    busy.tick_permission_walk(t0);
    assert!(
        !busy.permission_walk.active(),
        "a press's panel is up: no walk over it"
    );
}

/// The debugger is a page of the walk only while System State's once-per-Mac step is still to
/// come, named or not: that step is the walk's one way to it, so with none queued a walk that
/// names it leaves it out (and `aterm init` never says the window walks it). NEGATIVE CONTROL:
/// with the step queued the same name leads the walk.
#[test]
fn the_walk_takes_the_debugger_only_while_its_step_is_to_come() {
    use crate::debugger_access::DebuggerStepQueue;
    let t0 = Instant::now();
    let mut idle = walk_app();
    assert_eq!(
        idle.start_permission_walk(&[Subject::Debugger], "a test", t0),
        Started::Walking(vec![Subject::FullDiskAccess, Subject::DeveloperTools])
    );

    let mut queued = walk_app();
    queued.debugger_launch = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
    assert_eq!(
        queued.start_permission_walk(&[Subject::Debugger], "a test", t0),
        Started::Walking(vec![
            Subject::Debugger,
            Subject::FullDiskAccess,
            Subject::DeveloperTools
        ])
    );
}

/// The walk turns one panel from page to page under one generation, so a Developer Tools list
/// read still out when that page closed comes back to the page after it. It is not that page's
/// grant: the Accessibility page stays up, unticked. NEGATIVE CONTROL: the same reading ticks
/// the Developer Tools page (`a_walk_doctor_starts_admits_full_disk_access_right_after_developer_tools`).
#[test]
fn a_list_reading_that_comes_back_after_its_page_turned_ticks_no_other_page() {
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(
            &[Subject::DeveloperTools, Subject::Accessibility],
            "a test",
            t0
        ),
        Started::Walking(vec![
            Subject::DeveloperTools,
            Subject::Accessibility,
            Subject::FullDiskAccess
        ])
    );
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(t0);
    app.drive_permission_walk(t0);
    assert_eq!(
        page_and_step(&app),
        Some((Page::Accessibility, Some((2, 3)))),
        "turned to Accessibility"
    );
    // The Developer Tools page's read, still out as it closed, comes back now.
    app.devtools_helper.read_for_test(test_arms::on());
    app.tick_consent_lanes(t0 + GRANT_POLL);
    assert_eq!(
        app.devtools_helper
            .panel()
            .map(|panel| (panel.page(), panel.phase() == Phase::Done)),
        Some((Page::Accessibility, false)),
        "the Accessibility page is not ticked by a list"
    );
}
