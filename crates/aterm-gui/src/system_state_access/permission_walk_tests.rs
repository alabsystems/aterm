// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The permission walk through the App: one panel turned from page to page as each grant reads
//! on, a held grant skipped without its page, a Not now going on, the start on a cold first
//! launch, a control client's walk, and the Accessibility grant reaching the setup worker. The
//! pages, panels and grants are counted stand-ins; nothing reaches macOS.

use std::cell::Cell;

use super::*;
use crate::devtools_helper::{
    DONE_FOR, DevToolsHelperArm, Lane, Origin, PANEL_FOR, Page, PanelEvent, Phase, UNREAD_WORDS,
    test_arms,
};
use crate::permission_walk::{
    GRANT_POLL, REOPEN_MARKER, Started, Subject, UNREAD_WAIT, Walk, WalkLane, WalkReach,
    leave_reopen_in, take_reopen_in,
};
use crate::system_state_access::LOCAL_NETWORK_ALERT_BASELINE_WAIT;
use aterm_containment::DevToolGrant;
use aterm_containment::consent::FdaState;

thread_local! {
    static SHOWS: Cell<u32> = const { Cell::new(0) };
    static UPDATES: Cell<u32> = const { Cell::new(0) };
    static CLOSES: Cell<u32> = const { Cell::new(0) };
    static AX_TRUSTED: Cell<bool> = const { Cell::new(false) };
    static AUTOMATION_ASKS: Cell<u32> = const { Cell::new(0) };
    static AUTOMATION_OUT: Cell<bool> = const { Cell::new(false) };
    /// The key the Accessibility page's Not now was noted under, as `accessibility-declined`
    /// keeps it in `<state>`: it outlives each App a test makes, as a relaunch's `<state>` does.
    static AX_DECLINED: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    static AX_DECLINE_READS: Cell<u32> = const { Cell::new(0) };
    /// Whether System Settings shows a window of its own over its main one (macOS's Quit &
    /// Reopen sheet after a Full Disk Access switch), as the panel's arm reads the window list.
    static SETTINGS_ASKS: Cell<bool> = const { Cell::new(false) };
    /// Whether a quit nobody cancelled is asked of this process (Quit & Reopen's).
    static QUIT_ASKED: Cell<bool> = const { Cell::new(false) };
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
/// the panel reads no list of its own (the tests post each reading), shows a window over its
/// main one while [`SETTINGS_ASKS`] says so, and reads a quit asked of this process while
/// [`QUIT_ASKED`] does, as the walk's reach does.
fn counting_arm() -> DevToolsHelperArm {
    let mut arm = test_arms::asking(
        test_arms::counted(
            |_, _| {
                bump(&SHOWS);
                true
            },
            |_| bump(&UPDATES),
            || bump(&CLOSES),
            || false,
        ),
        || SETTINGS_ASKS.with(Cell::get),
    );
    arm.quit_asked = || QUIT_ASKED.with(Cell::get);
    arm
}

/// The walk's reach: Accessibility's grant as the test sets it, an Automation ask that is
/// counted and stays out until the test answers it, the Accessibility page's Not now kept in
/// [`AX_DECLINED`] (held for its span, its reads counted), a quit asked while [`QUIT_ASKED`]
/// says so, and the Quit & Reopen marker left in and taken from the real `<state>` folder
/// [`STATE`] names, as the live arms keep it, for [`BUNDLE`] on [`THIS_MAC`] at the clock's
/// second (none while no folder is named).
fn walk_reach() -> WalkReach {
    WalkReach {
        ax_trusted: || AX_TRUSTED.with(Cell::get),
        ask_automation: || {
            bump(&AUTOMATION_ASKS);
            AUTOMATION_OUT.with(|out| out.set(true));
        },
        automation_out: || AUTOMATION_OUT.with(Cell::get),
        ax_last_declined: || {
            bump(&AX_DECLINE_READS);
            AX_DECLINED.with(|key| key.borrow().clone().map(|key| (key, 1)))
        },
        ax_declined: || AX_DECLINED.with(|key| key.borrow().is_some()),
        note_ax_declined: |noted| {
            AX_DECLINED.with(|key| *key.borrow_mut() = Some(noted.to_owned()))
        },
        quit_asked: || QUIT_ASKED.with(Cell::get),
        leave_reopen: |pages| {
            if let (Some(state), Some(at)) = (state(), aterm_time::unix_secs()) {
                leave_reopen_in(
                    &state,
                    at,
                    pages,
                    std::path::Path::new(BUNDLE),
                    THIS_MAC.with(Cell::get),
                )
                .expect("the reopen marker");
            }
        },
        take_reopen: || {
            take_reopen_in(
                &state()?,
                aterm_time::unix_secs()?,
                Some(std::path::Path::new(BUNDLE)),
                Some(THIS_MAC.with(Cell::get)),
            )
        },
    }
}

/// [`counting_arm`] with System Settings the active app, so the person sees the panel beside it.
fn counting_arm_in_front() -> DevToolsHelperArm {
    test_arms::counted(
        |_, _| {
            bump(&SHOWS);
            true
        },
        |_| bump(&UPDATES),
        || bump(&CLOSES),
        || true,
    )
}

/// The `.app` the tests' aterm runs from.
const BUNDLE: &str = "/Applications/aterm.app";

/// The `<state>` folder [`STATE`] names.
fn state() -> Option<std::path::PathBuf> {
    STATE.with(|state| state.borrow().clone())
}

/// A new Mac at revision 1 with System State in front: Full Disk Access and Developer Tools
/// read off (aterm in a list of two, switched off), the pages and panel counted, the walk's
/// reach injected. `devtools` is the Developer Tools page's reach.
fn walk_app_with(devtools: crate::devtools_helper::Reach) -> App {
    walk_app_with_fda(devtools, test_arms::fda_opening())
}

/// [`walk_app_with`], with `fda` the Full Disk Access page's reach.
fn walk_app_with_fda(
    devtools: crate::devtools_helper::Reach,
    fda: crate::devtools_helper::FdaReach,
) -> App {
    walk_app_with_parts(devtools, fda, counting_arm())
}

/// [`walk_app_with`] with the panel's `arm`.
fn walk_app_with_arm(devtools: crate::devtools_helper::Reach, arm: DevToolsHelperArm) -> App {
    walk_app_with_parts(devtools, test_arms::fda_opening(), arm)
}

/// [`walk_app_with`], with `fda` the Full Disk Access page's reach and `arm` the panel's.
fn walk_app_with_parts(
    devtools: crate::devtools_helper::Reach,
    fda: crate::devtools_helper::FdaReach,
    arm: DevToolsHelperArm,
) -> App {
    use aterm_containment::consent::{DevToolProbe, DevToolProbeLabel, ProbeGate};
    fn no_probe(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        DevToolProbe::refused(DevToolProbeLabel::RefusedDisabled)
    }
    set_front(true);
    AX_TRUSTED.with(|trusted| trusted.set(false));
    AUTOMATION_OUT.with(|out| out.set(false));
    SETTINGS_ASKS.with(|asks| asks.set(false));
    QUIT_ASKED.with(|asked| asked.set(false));
    RECORDS.with(|records| records.borrow_mut().clear());
    let mut app = new_mac(devtools, fda);
    // A walk page is a person's stage: its time and its grant reads count while they are at the
    // Mac (`App::count_walk_page_time`). A test that models them away says so.
    app.user_input_recent = |_| true;
    app.devtools_helper = Lane::windowed_with_fda_for_test(devtools, fda, arm);
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
    use aterm_containment::consent::{DevToolProbe, DevToolProbeLabel, ProbeGate};
    fn no_probe(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        DevToolProbe::refused(DevToolProbeLabel::RefusedDisabled)
    }
    app.consent =
        crate::consent_state::ConsentState::with_cached_probe_for_test(probe(FdaState::Granted));
    app.consent
        .set_devtools_arms_for_test(no_probe, || Some(test_arms::SUBJECT));
    app.consent.probes_for_test().devtools_list = |_, _| test_arms::absent(2);
}

/// A walk's answer over `pages`, with no page held back by a Not now.
fn walking_over(pages: &[Subject]) -> Started {
    Started::Walking {
        pages: pages.to_vec(),
        not_now: Vec::new(),
    }
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
        walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
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

/// Whether the panel up names `next` as the walk's next page.
fn names_next(app: &App, next: &str) -> bool {
    app.devtools_helper
        .panel()
        .is_some_and(|panel| panel.content().texts().any(|(text, _)| text == next))
}

/// Full Disk Access reads on with Developer Tools still to come while macOS's Quit & Reopen sheet
/// stands over System Settings (`docs/measured/first-launch-permission-walk-0.145.0-2026-10-28.md`,
/// finding 3): the done words stay up and name the next page, past their second and for as long
/// as the sheet stands, and no page opens under the sheet. With the sheet gone but a quit asked
/// of this process (Quit & Reopen) they still wait; with the quit cancelled the same panel turns
/// to Developer Tools at the next look, and its page opens. The control: a walk whose Full Disk
/// Access page is its last names no next page, and its done words stay up past their second while
/// the sheet stands or a quit is asked of this process, as any Full Disk Access panel's do (the
/// lane's hold, `devtools_helper::Panel::hold_done`), and close done at the first look after,
/// the walk over. (That the
/// wait is not counted against the page's ten minutes is the lane's own test,
/// `a_walks_full_disk_access_done_words_wait_for_its_next_page`: a page the walk opens takes its
/// show time from the clock, not from the test's instants.)
#[test]
fn the_walk_holds_its_next_page_while_macos_asks_about_full_disk_access() {
    let t0 = Instant::now();
    let mut app = walk_app();
    let (opens, panels) = (test_arms::opens(), panel_counts());
    assert_eq!(
        app.start_permission_walk(&[], "a test", t0),
        walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
    );
    // The sheet comes with the grant.
    let granted = t0 + GRANT_POLL;
    grant_fda(&mut app);
    SETTINGS_ASKS.with(|asks| asks.set(true));
    app.tick_consent_lanes(granted);
    let sheet_stands = granted + Duration::from_secs(2 * 60);
    for look in [granted + DONE_FOR, sheet_stands] {
        app.tick_consent_lanes(look);
        assert_eq!(
            page_and_step(&app),
            Some((Page::FullDiskAccess, Some((1, 2)))),
            "the done words wait under the sheet"
        );
        assert_eq!(
            app.devtools_helper.panel().map(|panel| panel.phase()),
            Some(Phase::Done)
        );
        assert!(names_next(&app, "Next: Developer Tools."));
        assert_eq!(test_arms::opens(), opens, "no page under the sheet");
    }
    // Quit & Reopen asks this process to quit before its sheet goes.
    SETTINGS_ASKS.with(|asks| asks.set(false));
    QUIT_ASKED.with(|asked| asked.set(true));
    app.tick_consent_lanes(sheet_stands + GRANT_POLL);
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2)))),
        "a process asked to quit opens no page"
    );
    assert_eq!(test_arms::opens(), opens);
    QUIT_ASKED.with(|asked| asked.set(false));
    app.tick_consent_lanes(sheet_stands + GRANT_POLL * 2);
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((2, 2)))),
        "the next look once nothing asks"
    );
    assert_eq!(test_arms::opens(), opens + 1, "its page opened");
    let (shows, _, closes) = panel_counts();
    assert_eq!(
        (shows, closes),
        (panels.0 + 1, panels.2),
        "one panel, never closed"
    );

    // The control: Developer Tools reads on, so Full Disk Access is the walk's last page.
    let mut app = walk_app();
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::Granted, test_arms::on()),
        t0,
    );
    assert_eq!(
        app.start_permission_walk(&[], "a test", t0),
        walking_over(&[Subject::FullDiskAccess])
    );
    let closes = panel_counts().2;
    grant_fda(&mut app);
    SETTINGS_ASKS.with(|asks| asks.set(true));
    app.tick_consent_lanes(granted);
    assert!(
        !names_next(&app, "Next: Developer Tools."),
        "no next page to name"
    );
    let held = granted + DONE_FOR;
    app.tick_consent_lanes(held);
    assert_eq!(
        app.devtools_helper.panel().map(|panel| panel.phase()),
        Some(Phase::Done),
        "held under the sheet"
    );
    SETTINGS_ASKS.with(|asks| asks.set(false));
    QUIT_ASKED.with(|asked| asked.set(true));
    app.tick_consent_lanes(held + GRANT_POLL);
    assert_eq!(
        app.devtools_helper.panel().map(|panel| panel.phase()),
        Some(Phase::Done),
        "held while a quit is asked"
    );
    assert!(app.permission_walk.active());
    QUIT_ASKED.with(|asked| asked.set(false));
    app.tick_consent_lanes(held + GRANT_POLL * 2);
    assert!(app.devtools_helper.panel().is_none(), "closed done");
    assert!(!app.permission_walk.active(), "the walk is over");
    assert_eq!(panel_counts().2, closes + 1);
}

/// Done words that wait under macOS's sheet hold the walk no longer than the person or ten
/// minutes let them: their close box ends the walk there, and a sheet left standing lapses them
/// [`PANEL_FOR`] after the grant (a minute into the page here), the walk ending with them, not
/// ten minutes after the page's show. Neither notes a decline (the grant is in) or opens a page.
#[test]
fn the_close_box_or_ten_minutes_end_a_walk_held_under_macos_sheet() {
    for close_box in [true, false] {
        let t0 = Instant::now();
        let mut app = walk_app();
        assert_eq!(
            app.start_permission_walk(&[], "a test", t0),
            walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
        );
        let (opens, declines) = (test_arms::opens(), test_arms::fda_counts().2);
        grant_fda(&mut app);
        SETTINGS_ASKS.with(|asks| asks.set(true));
        let granted = t0 + Duration::from_secs(60);
        app.tick_consent_lanes(granted);
        app.tick_consent_lanes(granted + DONE_FOR);
        assert_eq!(
            page_and_step(&app),
            Some((Page::FullDiskAccess, Some((1, 2)))),
            "the done words wait"
        );
        if close_box {
            app.devtools_helper.press_for_test(PanelEvent::CloseBox);
            app.tick_consent_lanes(granted + DONE_FOR + GRANT_POLL);
        } else {
            app.tick_consent_lanes(granted + PANEL_FOR - GRANT_POLL);
            assert_eq!(
                page_and_step(&app),
                Some((Page::FullDiskAccess, Some((1, 2)))),
                "past the page's ten minutes, within the grant's"
            );
            app.tick_consent_lanes(granted + PANEL_FOR);
        }
        assert!(!app.permission_walk.active(), "close box: {close_box}");
        assert!(app.devtools_helper.panel().is_none());
        assert_eq!(test_arms::opens(), opens, "no page");
        assert_eq!(test_arms::fda_counts().2, declines, "nothing declined");
    }
}

/// The Full Disk Access probes the walk's reads ran (each a child aterm on a real Mac).
static FDA_PROBES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn probed_denied(
    _gate: aterm_containment::consent::ProbeGate,
) -> aterm_containment::consent::FdaProbe {
    FDA_PROBES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    probe(FdaState::Denied)
}

fn probed_granted(
    _gate: aterm_containment::consent::ProbeGate,
) -> aterm_containment::consent::FdaProbe {
    FDA_PROBES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    probe(FdaState::Granted)
}

/// A Full Disk Access reading of `state` a second old, past the walk's [`GRANT_POLL`] (the
/// window's own reads keep it: their interval is an hour), whose next probe is `arm`.
fn fda_read_a_second_ago(
    state: FdaState,
    arm: fn(aterm_containment::consent::ProbeGate) -> aterm_containment::consent::FdaProbe,
) -> crate::consent_state::ConsentState {
    use aterm_containment::consent::ProbeGate;
    let mut consent = crate::consent_state::ConsentState::inert();
    consent.set_fda_arm_for_test(arm);
    let read = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .expect("a second of uptime");
    let (_, ticket) = consent.read_at(ProbeGate::on(), Duration::from_secs(5), read);
    consent.complete_fda_for_test(ticket.expect("a fresh request"), probe(state), read);
    consent
}

/// Wait for the Full Disk Access probe that is out to come back: it runs on its worker thread,
/// so a minute is the hang detector.
fn fda_probe_back(app: &App) {
    let start = Instant::now();
    while app.consent.fda_in_flight_for_test().is_some() {
        assert!(start.elapsed() < Duration::from_secs(60), "hang");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The walk reads Full Disk Access every [`GRANT_POLL`] while its page asks for it, and no more
/// once the page shows its done words: they may wait minutes under macOS's Quit & Reopen sheet,
/// and the grant they show is in, so no probe (a child aterm each) runs under them. NEGATIVE
/// CONTROL: with the page up and the grant not yet on, each look past the poll probes.
#[test]
fn the_walk_reads_full_disk_access_no_more_once_its_page_shows_its_done_words() {
    use std::sync::atomic::Ordering;
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(&[], "a test", t0),
        walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
    );
    app.consent = fda_read_a_second_ago(FdaState::Denied, probed_denied);
    let probes = FDA_PROBES.load(Ordering::SeqCst);
    app.tick_consent_lanes(t0 + GRANT_POLL);
    fda_probe_back(&app);
    assert_eq!(
        FDA_PROBES.load(Ordering::SeqCst),
        probes + 1,
        "the page up reads its grant"
    );

    app.consent = fda_read_a_second_ago(FdaState::Granted, probed_granted);
    SETTINGS_ASKS.with(|asks| asks.set(true));
    let granted = t0 + GRANT_POLL * 2;
    app.tick_consent_lanes(granted);
    fda_probe_back(&app);
    assert_eq!(
        app.devtools_helper.panel().map(|panel| panel.phase()),
        Some(Phase::Done),
        "the done words"
    );
    app.consent = fda_read_a_second_ago(FdaState::Granted, probed_granted);
    let probes = FDA_PROBES.load(Ordering::SeqCst);
    for look in [granted + DONE_FOR, granted + Duration::from_secs(2 * 60)] {
        app.tick_consent_lanes(look);
        fda_probe_back(&app);
        assert_eq!(
            page_and_step(&app),
            Some((Page::FullDiskAccess, Some((1, 2)))),
            "the done words wait under the sheet"
        );
    }
    assert_eq!(
        FDA_PROBES.load(Ordering::SeqCst),
        probes,
        "no probe under the done words"
    );
}

/// The Developer Tools page that comes up after Full Disk Access read on in this process reads
/// its list itself, rather than show the reading from before the grant ("aterm can't see this
/// list without Full Disk Access") beside it and read no more
/// (`docs/measured/first-launch-permission-walk-0.145.0-2026-10-28.md`, finding 4). The control:
/// with Full Disk Access still off, the page says it can't see the list, and does not read it.
#[test]
fn the_developer_tools_page_after_full_disk_access_reads_its_list_itself() {
    let t0 = Instant::now();
    for fda_on in [true, false] {
        let mut app = walk_app();
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::NotGranted, test_arms::unread()),
            t0,
        );
        if fda_on {
            grant_fda(&mut app);
        }
        let reading = app.devtools_reading();
        assert!(app.show_devtools_helper(Origin::Settings, reading, None, t0));
        let panel = app.devtools_helper.panel().expect("its panel");
        let says_unread = panel
            .content()
            .texts()
            .any(|(text, _)| text == UNREAD_WORDS);
        assert_eq!(
            (
                panel.readable(),
                panel.wants_read(t0 + Duration::from_secs(60)),
                says_unread
            ),
            (fda_on, fda_on, !fda_on),
            "Full Disk Access on: {fda_on}"
        );
    }
}

/// A grant that reads held by the time the walk reaches it is skipped without its page: the
/// walk ends, its panel goes, and the Full Disk Access page never opens.
#[test]
fn a_grant_held_by_its_turn_is_skipped_without_its_page() {
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(&[Subject::DeveloperTools], "a test", t0),
        walking_over(&[Subject::DeveloperTools, Subject::FullDiskAccess])
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

/// The panel's "Step N of M" counts only the pages that come up: a grant that reads held while
/// an earlier page is up leaves its page out of the count, and once skipped it is not counted
/// among the pages before the next one. NEGATIVE CONTROL: before the grant reads held the page
/// counts the walk's every page.
#[test]
fn the_panels_count_leaves_out_a_page_whose_grant_reads_held() {
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(&[Subject::Accessibility], "a test", t0),
        walking_over(&[
            Subject::Accessibility,
            Subject::FullDiskAccess,
            Subject::DeveloperTools
        ])
    );
    assert_eq!(
        page_and_step(&app),
        Some((Page::Accessibility, Some((1, 3))))
    );
    let fda_opens = test_arms::fda_counts().0;
    grant_fda(&mut app);
    app.tick_consent_lanes(t0 + GRANT_POLL);
    assert_eq!(
        page_and_step(&app),
        Some((Page::Accessibility, Some((1, 2)))),
        "Full Disk Access reads held: its page will not come up"
    );
    AX_TRUSTED.with(|trusted| trusted.set(true));
    app.tick_consent_lanes(t0 + GRANT_POLL * 2);
    AUTOMATION_OUT.with(|out| out.set(false));
    app.tick_consent_lanes(t0 + GRANT_POLL * 2 + DONE_FOR);
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((2, 2)))),
        "the skipped page is not counted before it"
    );
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens,
        "no page for a held grant"
    );
}

/// A Not now on a page asked for by name ends the walk there; with both pages named (as
/// `aterm doctor` names them), Not now on the first goes on to the second.
#[test]
fn not_now_on_a_page_asked_for_ends_the_walk_there_or_goes_on_to_the_next_asked_for() {
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(&[Subject::DeveloperTools], "a person's press", t0),
        walking_over(&[Subject::DeveloperTools, Subject::FullDiskAccess])
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
        walking_over(&[Subject::DeveloperTools, Subject::FullDiskAccess])
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

thread_local! {
    static FDA_MARKER: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

thread_local! {
    /// Whether System State's quiet moment has come (`StepsReach::quiet`): the Local Network
    /// step runs only at it, while the walk's start needs only the keyboard's rest.
    static STEPS_QUIET: Cell<bool> = const { Cell::new(false) };
    /// The LAN re-probe's outcome, as its thread leaves it for the window.
    static LAN_PROBE: Cell<Option<(u64, crate::system_state_access::LocalNetworkProbe)>> =
        const { Cell::new(None) };
}

/// A first launch on a fresh Mac whose System State Local Network step is still to come and whose
/// quiet moment has not come ([`STEPS_QUIET`] false): the person at System State, present, the
/// keyboard at rest, so the walk is ready to start at every look.
fn walk_app_held_by_local_network(devtools: crate::devtools_helper::Reach) -> App {
    STEPS_QUIET.with(|quiet| quiet.set(false));
    let mut app = walk_app_with(devtools);
    app.access_steps.set_reach(StepsReach {
        recorded: |_, _| None,
        quiet: |_, _| STEPS_QUIET.with(Cell::get),
        names_holds: true,
        ..app.access_steps.reach
    });
    app.devtools_helper.note_cold_start();
    app.user_input_recent = |within| within >= Duration::from_secs(5);
    app
}

/// The look that finds the walk's wait for the Local Network step run out says in aterm.log that
/// the walk goes first only when a walk starts: on a Mac whose page markers hold the walk back (a
/// page came up here before), the wait runs out with no walk and nothing is said of one.
#[test]
fn the_walk_says_it_goes_before_local_network_only_when_it_starts() {
    use crate::permission_walk::LOCAL_NETWORK_WAIT;
    fn seen(_key: &str) -> Option<(String, u64)> {
        Some((atpkg::devtools::revision_key(1), 1))
    }
    let mut app = walk_app_held_by_local_network(test_arms::marked_as(seen));
    let t0 = Instant::now();
    let said = crate::test_log::logged_while(|| {
        app.tick_consent_lanes(t0);
        app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT);
        app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT + GRANT_POLL);
    });
    assert!(!app.permission_walk.active(), "the markers hold the walk");
    assert!(
        said.iter()
            .all(|line| !line.contains("the walk goes first")),
        "{said:?}"
    );
}

thread_local! {
    /// What the Local Network step did with its alert watch, in order: `start <revision>`, `arm`
    /// and `cancel` (`StepsReach::start_local_network_alert_watch`, `…arm…`, `…cancel…`), and its
    /// `trigger`.
    static ALERT_WATCH: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Whether the alert watch's first read is in
    /// (`StepsReach::local_network_alert_baseline_in`).
    static BASELINE_IN: Cell<bool> = const { Cell::new(true) };
    /// The answer the alert watch left for the window
    /// (`StepsReach::take_local_network_alert_answer`).
    static ALERT_ANSWER: Cell<Option<u64>> = const { Cell::new(None) };
    /// How many times the LAN readers were told Local Network's answer moved.
    static LAN_MOVED: Cell<u32> = const { Cell::new(0) };
}

fn watch_did(what: impl Into<String>) {
    ALERT_WATCH.with(|did| did.borrow_mut().push(what.into()));
}

fn watch_calls() -> Vec<String> {
    ALERT_WATCH.with(|did| did.borrow().clone())
}

fn lan_moved() -> u32 {
    LAN_MOVED.with(Cell::get)
}

/// The access record's Local Network lines this thread's steps wrote.
fn local_network_records() -> Vec<(u64, Access, AccessOutcome)> {
    records()
        .into_iter()
        .filter(|(_, access, _)| *access == Access::LocalNetwork)
        .collect()
}

/// A first launch on a fresh Mac at revision 1, which the person's own Turn On macOS Access…
/// press admitted (`App::person_asked_for_access`), so the first walk may start by itself, whose
/// Local Network step's quiet moment has come, the person at System State with the keyboard at
/// rest: the step's alert watch is a recorder ([`ALERT_WATCH`]) whose first read is in while
/// [`BASELINE_IN`] says so (at once, unless the test says otherwise) and whose answer the test
/// leaves in [`ALERT_ANSWER`], its trigger names a LAN when `lan` says so, and the LAN readers'
/// looks are counted. A touch reads within any span of 5 s or more, so the alert's hold counts
/// the person's time from the first look 5 s after its trigger. Nothing runs on another thread.
fn watched(lan: bool) -> App {
    STEPS_QUIET.with(|quiet| quiet.set(true));
    LAN_PROBE.with(|probe| probe.set(None));
    ALERT_ANSWER.with(|answer| answer.set(None));
    BASELINE_IN.with(|baseline| baseline.set(true));
    ALERT_WATCH.with(|did| did.borrow_mut().clear());
    LAN_MOVED.with(|moved| moved.set(0));
    let mut app = walk_app();
    app.access_steps.set_reach(StepsReach {
        recorded: |_, _| None,
        quiet: |_, _| STEPS_QUIET.with(Cell::get),
        take_local_network_probe: || LAN_PROBE.with(Cell::take),
        trigger_local_network: if lan {
            || {
                watch_did("trigger");
                true
            }
        } else {
            || {
                watch_did("trigger");
                false
            }
        },
        start_local_network_alert_watch: |revision| {
            watch_did(format!("start {revision}"));
            true
        },
        local_network_alert_baseline_in: || BASELINE_IN.with(Cell::get),
        arm_local_network_alert_watch: || watch_did("arm"),
        take_local_network_alert_answer: || ALERT_ANSWER.with(Cell::take),
        cancel_local_network_alert_watch: || watch_did("cancel"),
        lan_answer_moved: || LAN_MOVED.with(|moved| moved.set(moved.get() + 1)),
        ..app.access_steps.reach
    });
    app.person_asked_for_access();
    approve(&mut app, 1);
    assert!(
        app.person_permission_admission_stands(),
        "precondition: the person's press admits the walk"
    );
    app.devtools_helper.note_cold_start();
    app.user_input_recent = |within| within >= Duration::from_secs(5);
    app
}

/// The pages of the walk under way, if one is.
fn walk_pages(app: &App) -> Option<Vec<Subject>> {
    app.permission_walk
        .walk()
        .map(|walk| walk.subjects().to_vec())
}

/// The Local Network alert watch's answer ends the alert's hold in the turn that drains it, long
/// before the person's 30 s: the step starts the watch for its revision before its trigger, which
/// arms it; the hold stands with no look of the window list armed on the main thread (the hold's
/// own look, a `RECHECK_EVERY` while the person's time is not yet counted, is the steps' next
/// deadline), and once the watch leaves its answer the window's gestures are down, the watch is
/// stopped, the LAN readers look again, nothing more is recorded, and the first walk starts in
/// that turn, its page with it. NEGATIVE CONTROL: with `App::on_local_network_alert_answered`
/// draining the answer but leaving the hold (its `end_local_network_hold` taken out), the walk
/// waits on; with the consent lanes' turn not draining the answer before the walk's look
/// (`App::tick_consent_lanes`), the walk waits for a wake no test sends.
#[test]
fn the_alert_watchs_answer_ends_the_local_network_hold_in_the_turn_that_drains_it() {
    use crate::devtools_helper::RECHECK_EVERY;
    let walking = Some(vec![Subject::FullDiskAccess, Subject::DeveloperTools]);
    let t0 = Instant::now();
    let mut app = watched(true);
    let fda_opens = test_arms::fda_counts().0;
    app.tick_consent_lanes(t0);
    assert_eq!(
        watch_calls(),
        ["start 1", "trigger", "arm"],
        "the watch starts before the trigger, which arms it"
    );
    assert_eq!(
        local_network_records(),
        [(1, Access::LocalNetwork, AccessOutcome::Triggered)]
    );
    let moved = lan_moved();
    assert!(app.access_gesture_up(t0), "its alert's hold stands");
    assert_eq!(
        app.next_access_step_deadline(t0),
        Some(t0 + RECHECK_EVERY),
        "no look of the window list is armed on the main thread"
    );
    let waiting = t0 + Duration::from_secs(1);
    app.tick_consent_lanes(waiting);
    assert!(app.access_gesture_up(waiting), "no answer: the hold stands");
    assert_eq!(walk_pages(&app), None, "no page beside the alert");

    let answered = t0 + Duration::from_secs(2);
    ALERT_ANSWER.with(|answer| answer.set(Some(1)));
    app.tick_consent_lanes(answered);
    assert!(
        !app.access_gesture_up_beside_walk(answered),
        "the alert was answered: the hold is over"
    );
    assert_eq!(walk_pages(&app), walking, "the walk starts in that turn");
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens + 1,
        "its page comes up"
    );
    assert_eq!(lan_moved(), moved + 1, "the LAN readers look again");
    assert_eq!(
        watch_calls(),
        ["start 1", "trigger", "arm", "cancel"],
        "the watch is stopped"
    );
    assert_eq!(
        local_network_records(),
        [(1, Access::LocalNetwork, AccessOutcome::Triggered)],
        "nothing more is recorded: the answer is not known"
    );
}

/// With no answer from the alert watch (no alert seen after the trigger, the alert still up, or a
/// window list it could not read), the Local Network alert's hold runs as on main: it ends once
/// the person has been at the Mac 30 s counted from their first touch after the ask, and the walk
/// starts as it ends; the hold's end stops the watch, once. An answer drained after that ends
/// nothing more and moves no LAN reader. NEGATIVE CONTROL: with
/// `AccessSteps::end_local_network_hold` not stopping the watch, no stop reaches it; with
/// `App::on_local_network_alert_answered` not asking whether its watch's hold still stands (its
/// `local_network_watch` and `local_network_alert` checks taken out), the answer after the hold's
/// end moves the LAN readers again.
#[test]
fn with_no_answer_the_local_network_hold_runs_the_persons_30_s_and_stops_its_watch() {
    let walking = Some(vec![Subject::FullDiskAccess, Subject::DeveloperTools]);
    let t0 = Instant::now();
    let mut app = watched(true);
    app.tick_consent_lanes(t0);
    let moved = lan_moved();
    let touched = t0 + Duration::from_secs(5);
    app.tick_consent_lanes(touched);
    let last = touched + LOCAL_NETWORK_ALERT_HOLD - Duration::from_millis(1);
    app.tick_consent_lanes(last);
    assert!(
        app.access_gesture_up(last),
        "the hold stands until the person has had 30 s with the alert"
    );
    assert_eq!(walk_pages(&app), None, "no page beside the alert");
    assert_eq!(
        watch_calls(),
        ["start 1", "trigger", "arm"],
        "the watch goes on"
    );

    let over = touched + LOCAL_NETWORK_ALERT_HOLD;
    app.tick_consent_lanes(over);
    assert_eq!(
        walk_pages(&app),
        walking,
        "the person's 30 s are up: the walk starts"
    );
    assert_eq!(
        watch_calls(),
        ["start 1", "trigger", "arm", "cancel"],
        "the hold's end stops the watch"
    );
    ALERT_ANSWER.with(|answer| answer.set(Some(1)));
    app.tick_consent_lanes(over + Duration::from_secs(1));
    assert_eq!(
        lan_moved(),
        moved,
        "an answer once the hold is over moves nothing"
    );
    assert_eq!(
        watch_calls(),
        ["start 1", "trigger", "arm", "cancel"],
        "and stops nothing again"
    );
}

/// An answer the alert watch leaves for a watch other than the one the hold's trigger started
/// ends nothing: the hold stands. NEGATIVE CONTROL: with `App::on_local_network_alert_answered`
/// not matching the answer's revision to the hold's watch, it ends the hold at once.
#[test]
fn an_answer_for_another_watch_does_not_end_the_local_network_hold() {
    let t0 = Instant::now();
    let mut app = watched(true);
    app.tick_consent_lanes(t0);
    let moved = lan_moved();
    let later = t0 + Duration::from_secs(2);
    ALERT_ANSWER.with(|answer| answer.set(Some(2)));
    app.tick_consent_lanes(later);
    assert!(app.access_gesture_up(later), "the hold stands");
    assert_eq!(walk_pages(&app), None, "no page beside the alert");
    assert_eq!(lan_moved(), moved, "no LAN reader moved");
    assert_eq!(watch_calls(), ["start 1", "trigger", "arm"]);
}

/// A Local Network trigger that named no LAN asked nothing and takes no hold, so it stops the
/// alert watch it started just before it. The LAN re-probe's heard answer, which ends a hold
/// another way, stops that hold's watch too. NEGATIVE CONTROL: with the trigger's no-LAN arm not
/// stopping the watch, it reads on; with `AccessSteps::end_local_network_hold` not stopping it,
/// the re-probe's answer leaves it reading.
#[test]
fn a_hold_that_ends_without_the_alert_watch_stops_it() {
    use crate::system_state_access::LocalNetworkProbe;
    let t0 = Instant::now();
    let mut app = watched(false);
    app.tick_consent_lanes(t0);
    assert_eq!(
        watch_calls(),
        ["start 1", "trigger", "cancel"],
        "no LAN named: the watch it started is stopped, never armed"
    );
    assert!(
        !app.access_steps.local_network_alert_up(),
        "and no hold is taken"
    );

    let mut app = watched(true);
    app.tick_consent_lanes(t0);
    let heard = t0 + Duration::from_secs(3);
    LAN_PROBE.with(|probe| probe.set(Some((1, LocalNetworkProbe::Refused))));
    app.on_local_network_probe_ready(heard);
    assert!(
        !app.access_steps.local_network_alert_up(),
        "the re-probe's answer ends the hold"
    );
    assert_eq!(
        watch_calls(),
        ["start 1", "trigger", "arm", "cancel"],
        "and stops its watch"
    );
}

/// The Local Network trigger waits for its alert watch's first read, so the baseline is read
/// before the connect can raise the alert: at the step's moment the watch starts and the trigger
/// does not go; meanwhile the alert counts as up for the window's gestures (the dialog slot stays
/// held, the walk waits, nothing is recorded), the page's headline names no step, since no alert
/// is up yet, and the wait's end is the steps' next deadline. Once the first read is in, the
/// trigger goes in that turn, arms the watch and raises the hold from there, which the headline
/// then names. NEGATIVE CONTROL: a step that triggers just after starting the watch, without
/// waiting for its first read (`fire_local_network_trigger` in place of the wait), triggers at
/// `t0`; a headline that reads the wait as an alert (`local_network_alert_up()` in place of
/// `local_network_alert.is_some()`) names Local Network during the wait.
#[test]
fn the_local_network_trigger_waits_for_its_alert_watchs_first_read() {
    let t0 = Instant::now();
    let mut app = watched(true);
    BASELINE_IN.with(|baseline| baseline.set(false));
    app.tick_consent_lanes(t0);
    assert_eq!(
        watch_calls(),
        ["start 1"],
        "no trigger before the first read"
    );
    assert_eq!(local_network_records(), [], "nothing recorded yet");
    assert!(
        app.access_gesture_up(t0),
        "the alert counts as up meanwhile"
    );
    assert_eq!(
        app.system_state_access_step(),
        None,
        "no alert is up yet: the headline names no step"
    );
    assert_eq!(
        app.next_access_step_deadline(t0),
        Some(t0 + LOCAL_NETWORK_ALERT_BASELINE_WAIT),
        "the wait's end"
    );
    let still = t0 + Duration::from_millis(400);
    app.tick_consent_lanes(still);
    assert_eq!(watch_calls(), ["start 1"], "still waiting");
    assert_eq!(walk_pages(&app), None, "no page while the trigger waits");

    let read = t0 + Duration::from_millis(500);
    BASELINE_IN.with(|baseline| baseline.set(true));
    app.tick_consent_lanes(read);
    assert_eq!(
        watch_calls(),
        ["start 1", "trigger", "arm"],
        "the first read is in: the trigger goes and arms the watch"
    );
    assert_eq!(
        local_network_records(),
        [(1, Access::LocalNetwork, AccessOutcome::Triggered)]
    );
    assert_eq!(
        app.access_steps
            .local_network_alert
            .map(|alert| alert.asked),
        Some(read),
        "the hold runs from the trigger"
    );
    assert_eq!(
        app.system_state_access_step(),
        Some(Access::LocalNetwork),
        "the alert is up: the headline names it"
    );
}

/// A first read that does not come back within `LOCAL_NETWORK_ALERT_BASELINE_WAIT` (a
/// WindowServer that holds the worker's read) lets the trigger go without the watch: the watch is
/// stopped, never armed, and the hold ends only by the re-probe's answer or the person's time,
/// which an answer left later does not change. NEGATIVE CONTROL: a wait with no end of its own
/// (its `now < by` taken out) never triggers.
#[test]
fn a_first_read_that_does_not_come_back_lets_the_trigger_go_without_the_watch() {
    let t0 = Instant::now();
    let mut app = watched(true);
    BASELINE_IN.with(|baseline| baseline.set(false));
    app.tick_consent_lanes(t0);
    let gave_up = t0 + LOCAL_NETWORK_ALERT_BASELINE_WAIT;
    app.tick_consent_lanes(gave_up);
    assert_eq!(
        watch_calls(),
        ["start 1", "cancel", "trigger"],
        "the watch is stopped and the trigger goes, unarmed"
    );
    assert_eq!(
        local_network_records(),
        [(1, Access::LocalNetwork, AccessOutcome::Triggered)]
    );
    let later = gave_up + Duration::from_secs(2);
    ALERT_ANSWER.with(|answer| answer.set(Some(1)));
    app.tick_consent_lanes(later);
    assert!(app.access_gesture_up(later), "the hold stands");
}

/// A trigger waiting for its watch's first read is dropped when the swarm is switched off: the
/// watch is stopped, no alert counts as up, a network move told before it stays told, and the
/// step, which never asked, is not done, so it runs again at its next moment once the swarm is
/// back on. A wait whose approved revision gives way to another, or whose `[privacy]` switch
/// goes off, is dropped the same way, and the step asks for the revision that stands. NEGATIVE
/// CONTROL: with `AccessSteps::end_local_network_hold` leaving `local_network_done` as it was,
/// the step reads done and never starts again; with the step spending the network move as it
/// starts the watch, not as the trigger goes, the dropped wait spent it; with
/// `App::local_network_trigger_waits` not reading the `[privacy]` switches, the wait goes on
/// and triggers.
#[test]
fn a_trigger_waiting_for_its_first_read_is_dropped_when_the_swarm_goes_off() {
    use std::sync::atomic::Ordering;
    let t0 = Instant::now();
    let mut app = watched(true);
    BASELINE_IN.with(|baseline| baseline.set(false));
    app.access_steps
        .network_moved
        .store(true, Ordering::Release);
    app.tick_consent_lanes(t0);
    assert_eq!(watch_calls(), ["start 1"]);
    let swarm = app.config.swarm.take();
    app.config.swarm = Some(crate::app_config::SwarmConfig {
        enabled: Some(false),
    });
    let off = t0 + Duration::from_millis(200);
    app.tick_consent_lanes(off);
    assert_eq!(watch_calls(), ["start 1", "cancel"], "the watch is stopped");
    assert!(
        !app.access_steps.local_network_alert_up(),
        "no alert counts as up"
    );
    assert!(
        app.access_steps.network_moved.load(Ordering::Acquire),
        "the move told before the wait stays told"
    );
    assert_eq!(local_network_records(), [], "nothing was asked");
    app.config.swarm = swarm;
    let on = off + Duration::from_millis(200);
    app.tick_consent_lanes(on);
    assert_eq!(
        watch_calls(),
        ["start 1", "cancel", "start 1"],
        "the step runs again"
    );

    let other = on + Duration::from_millis(200);
    app.person_asked_for_access();
    approve(&mut app, 2);
    app.tick_consent_lanes(other);
    assert_eq!(
        watch_calls(),
        ["start 1", "cancel", "start 1", "cancel", "start 2"],
        "another revision: the wait is dropped and the step asks for that one"
    );

    let switched = other + Duration::from_millis(200);
    if let Some(privacy) = app.config.privacy.as_mut() {
        privacy.notice = Some(false);
    }
    BASELINE_IN.with(|baseline| baseline.set(true));
    app.tick_consent_lanes(switched);
    assert_eq!(
        watch_calls(),
        [
            "start 1", "cancel", "start 1", "cancel", "start 2", "cancel"
        ],
        "a [privacy] switch off: the wait is dropped, and nothing triggers"
    );
    assert_eq!(local_network_records(), [], "nothing was asked");
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
        walking_over(&[Subject::DeveloperTools, Subject::FullDiskAccess])
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
/// System Events is asked right after it, and once the person answers it the walk goes on a
/// second after the grant. Until the grant reads on, neither happens.
#[test]
fn an_accessibility_grant_is_recorded_for_the_setup_worker_and_asks_automation() {
    let t0 = Instant::now();
    let mut app = walk_app();
    let asks = AUTOMATION_ASKS.with(Cell::get);
    assert_eq!(
        app.start_permission_walk(&[Subject::Accessibility], "a test", t0),
        walking_over(&[
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
    AUTOMATION_OUT.with(|out| out.set(false));
    app.tick_consent_lanes(t0 + GRANT_POLL * 2 + DONE_FOR);
    assert_eq!(
        page_and_step(&app).map(|(page, _)| page),
        Some(Page::FullDiskAccess),
        "and the walk goes on"
    );
}

/// The Automation question the walk asks once Accessibility reads on holds its next page: the
/// panel stays held on its done words and no Full Disk Access page comes up beside macOS's
/// unanswered alert, however many looks pass; the page opens at the first look after the person
/// answers. A person's press on the row of the page the walk waits at opens it at once, as a
/// press opens its page beside any other question (`WalkLane::pressed`). NEGATIVE CONTROL: an
/// answer before the page's turn lets the walk go on a second after the grant
/// (`an_accessibility_grant_is_recorded_for_the_setup_worker_and_asks_automation`).
#[test]
fn the_walk_waits_out_the_automation_question_before_its_next_page() {
    for press in [false, true] {
        let t0 = Instant::now();
        let mut app = walk_app();
        let asks = AUTOMATION_ASKS.with(Cell::get);
        assert_eq!(
            app.start_permission_walk(&[Subject::Accessibility], "a test", t0),
            walking_over(&[
                Subject::Accessibility,
                Subject::FullDiskAccess,
                Subject::DeveloperTools
            ])
        );
        AX_TRUSTED.with(|trusted| trusted.set(true));
        app.tick_consent_lanes(t0 + GRANT_POLL);
        assert_eq!(
            AUTOMATION_ASKS.with(Cell::get),
            asks + 1,
            "Automation asked"
        );
        assert!(AUTOMATION_OUT.with(Cell::get), "macOS's question is up");
        let fda_opens = test_arms::fda_counts().0;
        for look in 1..=4 {
            app.tick_consent_lanes(t0 + GRANT_POLL * look + DONE_FOR);
        }
        assert_eq!(
            test_arms::fda_counts().0,
            fda_opens,
            "no page beside the unanswered Automation alert"
        );
        assert_ne!(
            page_and_step(&app).map(|(page, _)| page),
            Some(Page::FullDiskAccess)
        );
        assert!(app.permission_walk.active(), "the walk waits");
        if press {
            assert!(app.open_access_from_system_state(Access::FullDiskAccess, true));
            assert_eq!(
                test_arms::fda_counts().0,
                fda_opens + 1,
                "a person's press opens its page now"
            );
        } else {
            AUTOMATION_OUT.with(|out| out.set(false));
            app.tick_consent_lanes(t0 + GRANT_POLL * 5 + DONE_FOR);
            assert_eq!(
                test_arms::fda_counts().0,
                fda_opens + 1,
                "its page, at the first look after the answer"
            );
        }
        assert_eq!(
            page_and_step(&app),
            Some((Page::FullDiskAccess, Some((2, 3))))
        );
    }
}

/// The live Automation ask is out, and counts as a macOS dialog of aterm's up, from before its
/// worker starts until its Apple event returns: the walk's next page, the steps' one-gesture
/// rule and the setup worker's prompts all wait for macOS's answer, even once the walk is over.
/// The Apple event is a stand-in that returns when the test answers it.
#[cfg(target_os = "macos")]
#[test]
fn the_live_automation_ask_is_a_macos_dialog_up_until_its_event_returns() {
    use aterm_types::macos_setup::macos_dialog_up;
    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(!macos_dialog_up());
    let out = WalkReach::live().automation_out;
    assert!(!out());
    let (answer, answered) = std::sync::mpsc::channel::<()>();
    let worker = crate::permission_walk::ask_automation_with_for_test(move || {
        // A hang detector: the test answers at once.
        let _ = answered.recv_timeout(Duration::from_secs(60));
    })
    .expect("the ask's worker starts");
    assert!(out(), "out while its Apple event has not returned");
    assert!(macos_dialog_up(), "and a macOS dialog of aterm's up");
    answer.send(()).expect("the worker waits for the answer");
    worker.join().expect("the worker ends");
    assert!(!out(), "no longer out once it returned");
    assert!(!macos_dialog_up(), "nor a dialog up");
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
    let walking = walking_over(&[Subject::DeveloperTools, Subject::FullDiskAccess]);
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

/// An ask the window takes only after its control lane gave it up starts no walk: the lane
/// answers `ERR late` at its deadline (`ERR hangup` once its client hung up), and its client goes
/// on without the walk (`aterm doctor` opens the pages itself), so when the main thread comes to
/// the ask it opens no page and shows no panel, then or at the next turn. NEGATIVE CONTROL: an
/// ask the main thread takes before the lane gives it up, though its client hangs up just after,
/// starts the walk, the lane answers `OK walking`, and the page opens at the next turn.
#[test]
fn a_walk_ask_its_lane_gave_up_on_starts_no_walk() {
    use crate::Wake;
    use crate::control::control_media::{Hops, MainPost, take_hop};
    use crate::control::serve_ask_walk;
    use std::cell::RefCell;
    fn whole(left: Duration) -> Duration {
        left
    }
    let t0 = Instant::now();
    let asked = [Subject::FullDiskAccess, Subject::DeveloperTools];
    let hops: &'static Hops = Box::leak(Box::new(Hops::new()));
    for (client_gone, answer) in [(false, "ERR late"), (true, "ERR hangup")] {
        let mut app = walk_app();
        let posted = RefCell::new(None);
        let line = serve_ask_walk(
            asked.to_vec(),
            &mut MainPost::injected(
                |wake| {
                    posted.replace(Some(take_hop(wake)));
                    true
                },
                hops,
            ),
            Duration::ZERO,
            &mut || client_gone,
            whole,
        );
        assert!(line.starts_with(answer), "{line}");
        let Some(Wake::PermissionWalkAsked {
            subjects,
            reply,
            claim,
        }) = posted.take()
        else {
            panic!("{answer}: the ask was posted");
        };
        let opened = (test_arms::fda_counts().0, panel_counts().0);
        app.take_permission_walk_ask(&subjects, &claim, &reply, t0);
        assert!(!app.permission_walk.active(), "{answer}: no walk");
        app.tick_consent_lanes(t0 + GRANT_POLL);
        assert_eq!(
            (test_arms::fda_counts().0, panel_counts().0),
            opened,
            "{answer}: no page and no panel"
        );
    }

    let mut app = walk_app();
    let posted = RefCell::new(None);
    let fda_opens = test_arms::fda_counts().0;
    let line = serve_ask_walk(
        asked.to_vec(),
        &mut MainPost::injected(
            |wake| {
                posted.replace(Some(take_hop(wake)));
                true
            },
            hops,
        ),
        Duration::ZERO,
        &mut || {
            if let Some(Wake::PermissionWalkAsked {
                subjects,
                reply,
                claim,
            }) = posted.take()
            {
                app.take_permission_walk_ask(&subjects, &claim, &reply, t0);
            }
            true
        },
        whole,
    );
    assert_eq!(line, walking_over(&asked).reply(), "taken first");
    assert!(app.permission_walk.active());
    app.tick_consent_lanes(t0 + GRANT_POLL);
    assert_eq!(test_arms::fda_counts().0, fda_opens + 1, "its page");
}

/// A verb that takes back a page the walk under way passed counts as the verb starting a walk:
/// a program sending `permissions walk now` in a loop brings a page the person passed back at
/// most once a minute. In between that page stays passed, and when the verb asked for it the
/// answer is `OK recent`, so `aterm doctor` opens nothing beside the walk; a page the walk never
/// had joins all the same, even when the verb's walk would take a passed page back with it (a
/// sign-out's `walk now accessibility` reads Full Disk Access, passed with *I've switched it
/// on*, still missing). NEGATIVE CONTROL: a minute after the verb started the walk, it takes the
/// page back.
#[test]
fn the_verb_takes_back_a_page_the_walk_passed_at_most_once_a_minute() {
    use crate::permission_walk::VERB_AGAIN_AFTER;
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess], t0),
        walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
    );
    app.devtools_helper.press_for_test(PanelEvent::SwitchedOn);
    app.tick_devtools_hand_over(t0 + Duration::from_secs(1));
    app.drive_permission_walk(t0 + Duration::from_secs(1));
    assert_eq!(
        app.permission_walk.walk().and_then(Walk::current),
        Some(Subject::DeveloperTools),
        "on with no grant read"
    );
    assert_eq!(
        app.ask_permission_walk(&[Subject::Accessibility], t0 + Duration::from_secs(2))
            .reply(),
        "OK walking devtools accessibility\n",
        "a page the walk never had joins inside the minute"
    );
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((2, 3))))
    );

    let doctor = [Subject::FullDiskAccess, Subject::DeveloperTools];
    let inside = t0 + VERB_AGAIN_AFTER - Duration::from_secs(1);
    assert_eq!(app.ask_permission_walk(&doctor, inside), Started::Recent);
    assert_eq!(
        app.permission_walk.walk().map(Walk::subjects),
        Some(
            &[
                Subject::FullDiskAccess,
                Subject::DeveloperTools,
                Subject::Accessibility
            ][..]
        ),
        "nothing taken back"
    );
    assert_eq!(
        app.ask_permission_walk(&doctor, t0 + VERB_AGAIN_AFTER)
            .reply(),
        "OK walking devtools accessibility fda\n",
        "a minute on"
    );
    assert_eq!(
        app.ask_permission_walk(&doctor, t0 + VERB_AGAIN_AFTER + Duration::from_secs(1)),
        walking_over(&[
            Subject::DeveloperTools,
            Subject::Accessibility,
            Subject::FullDiskAccess
        ]),
        "a page ahead again is no taking back"
    );
}

/// The owner's Not now holds a page back from the control verb's walk, as it held the verb's
/// hand-overs: a declined Developer Tools page leaves the walk Full Disk Access alone (the answer
/// names it a Not now), and with both declined the verb answers declined and opens nothing.
/// NEGATIVE CONTROL: a person's press over the same declines walks both pages.
#[test]
fn the_owners_not_now_holds_a_page_back_from_the_control_verbs_walk() {
    let t0 = Instant::now();
    let mut app = walk_app_with(test_arms::declining());
    assert_eq!(
        app.ask_permission_walk(&[Subject::DeveloperTools, Subject::FullDiskAccess], t0),
        Started::Walking {
            pages: vec![Subject::FullDiskAccess],
            not_now: vec![Subject::DeveloperTools],
        }
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
        walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools]),
        "the owner's own press goes past a Not now"
    );
}

/// The verb's answer names each page asked for that the owner's Not now holds back from its
/// walk (`not-now:<subject>`), so `aterm doctor` opens nothing for it, as for `OK declined`;
/// a walk under way names it too. NEGATIVE CONTROL: a person's press, which goes past a Not
/// now, names none.
#[test]
fn the_verbs_answer_names_a_page_the_owners_not_now_held_back() {
    let t0 = Instant::now();
    let mut app = walk_app_with(test_arms::declining());
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess, Subject::DeveloperTools], t0)
            .reply(),
        "OK walking fda not-now:devtools\n"
    );
    assert_eq!(
        app.permission_walk.walk().map(Walk::subjects),
        Some(&[Subject::FullDiskAccess][..]),
        "the walk leaves the declined page out"
    );
    assert_eq!(
        app.ask_permission_walk(&[Subject::DeveloperTools], t0)
            .reply(),
        "OK walking fda not-now:devtools\n",
        "a walk under way"
    );

    let mut app = walk_app_with(test_arms::declining());
    assert_eq!(
        app.start_permission_walk(&[], "a person's press", t0)
            .reply(),
        "OK walking fda devtools\n"
    );
}

/// `OK declined` is decided over the pages the verb asked for: the owner's Not now on Developer
/// Tools does not turn an ask for Full Disk Access, which the window reads held, into declined,
/// so `aterm doctor` opens that page itself for the terminal that lacks it. NEGATIVE CONTROL:
/// the bare verb, which asks for every page, and an ask for Developer Tools answer declined.
/// Nothing opens.
#[test]
fn a_not_now_on_a_page_the_verb_did_not_ask_for_leaves_a_held_page_granted() {
    let t0 = Instant::now();
    let mut app = walk_app_with(test_arms::declining());
    grant_fda(&mut app);
    let opens = (test_arms::opens(), test_arms::fda_counts().0);
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess], t0),
        Started::Granted
    );
    assert_eq!(app.ask_permission_walk(&[], t0), Started::Declined);
    assert_eq!(
        app.ask_permission_walk(&[Subject::DeveloperTools], t0),
        Started::Declined
    );
    assert!(!app.permission_walk.active());
    assert_eq!((test_arms::opens(), test_arms::fda_counts().0), opens);
}

/// The control verb's walk asked for Accessibility alone, its page up first, and the owner's Not
/// now on it, the last page asked for, which ends that walk.
fn say_not_now_to_accessibility(app: &mut App, at: Instant) {
    assert_eq!(
        app.ask_permission_walk(&[Subject::Accessibility], at),
        walking_over(&[
            Subject::Accessibility,
            Subject::FullDiskAccess,
            Subject::DeveloperTools
        ])
    );
    assert_eq!(
        page_and_step(app).map(|(page, _)| page),
        Some(Page::Accessibility)
    );
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(at);
    app.tick_consent_lanes(at + GRANT_POLL);
    assert!(
        !app.permission_walk.active(),
        "a Not now on the last page asked for ends the walk"
    );
}

/// The walk's answer with Accessibility held back by the owner's Not now.
fn walking_without_accessibility() -> Started {
    Started::Walking {
        pages: vec![Subject::FullDiskAccess, Subject::DeveloperTools],
        not_now: vec![Subject::Accessibility],
    }
}

/// The owner's Not now on the walk's Accessibility page holds that page back from the control
/// verb in a later process too, as the other pages' do: the page notes `accessibility-declined`
/// under its TCC identity's key, so a relaunched window, which took no answer itself, walks Full
/// Disk Access and Developer Tools and names Accessibility a Not now (a sign-out then asks
/// nothing); `<state>` is read once for the key, however often the verb asks. A Not now an aterm
/// before that marker took is the revision's `declined` in the access record alone, and holds
/// the page the same way. NEGATIVE CONTROL: the owner's own press goes past it.
#[test]
fn the_owners_accessibility_not_now_holds_the_control_verb_in_a_later_process() {
    let t0 = Instant::now();
    let mut app = walk_app();
    say_not_now_to_accessibility(&mut app, t0);

    let mut app = walk_app();
    let reads = AX_DECLINE_READS.with(Cell::get);
    assert_eq!(
        app.ask_permission_walk(&[Subject::Accessibility], t0),
        walking_without_accessibility()
    );
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2))))
    );
    assert_eq!(
        app.ask_permission_walk(&[Subject::Accessibility], t0)
            .reply(),
        "OK walking fda devtools not-now:accessibility\n",
        "a walk under way"
    );
    assert_eq!(
        AX_DECLINE_READS.with(Cell::get),
        reads + 1,
        "read once for the key"
    );
    app.permission_walk.end("a test");
    assert_eq!(
        app.start_permission_walk(&[Subject::Accessibility], "a person's press", t0),
        walking_over(&[
            Subject::Accessibility,
            Subject::FullDiskAccess,
            Subject::DeveloperTools
        ]),
        "the owner's own press goes past a Not now"
    );

    AX_DECLINED.with(|key| *key.borrow_mut() = None);
    let mut app = walk_app();
    app.access_steps.set_reach(StepsReach {
        recorded: |_, access| {
            Some(if access == Access::Accessibility {
                AccessOutcome::Declined
            } else {
                AccessOutcome::Triggered
            })
        },
        ..app.access_steps.reach
    });
    assert_eq!(
        app.ask_permission_walk(&[Subject::Accessibility], t0),
        walking_without_accessibility(),
        "an older aterm's Not now, on record only"
    );
}

/// With no setup revision standing (before the Permissions approval, after Stop) a Not now is
/// recorded nowhere in the access record, yet the owner's Not now on the walk's Accessibility
/// page holds it back from the control verb a minute on, once the verb may walk again, and in a
/// later process, as the other pages' Not nows do. NEGATIVE CONTROL: the owner's own press goes
/// past it.
#[test]
fn an_accessibility_not_now_with_no_revision_holds_the_control_verb() {
    use crate::permission_walk::VERB_AGAIN_AFTER;
    let no_revision = || {
        let mut app = walk_app();
        app.observe_initial_setup_permissions(None);
        assert_eq!(app.access_revision(), None);
        app
    };
    let t0 = Instant::now();
    let mut app = no_revision();
    say_not_now_to_accessibility(&mut app, t0);
    assert!(
        !records()
            .iter()
            .any(|(_, access, _)| *access == Access::Accessibility),
        "nothing on record"
    );
    assert_eq!(
        app.ask_permission_walk(&[Subject::Accessibility], t0 + VERB_AGAIN_AFTER),
        walking_without_accessibility(),
        "a minute on"
    );

    let mut app = no_revision();
    assert_eq!(
        app.ask_permission_walk(&[Subject::Accessibility], t0),
        walking_without_accessibility(),
        "a later process"
    );
    app.permission_walk.end("a test");
    assert_eq!(
        app.start_permission_walk(&[Subject::Accessibility], "a person's press", t0),
        walking_over(&[
            Subject::Accessibility,
            Subject::FullDiskAccess,
            Subject::DeveloperTools
        ]),
        "the owner's own press goes past a Not now"
    );
}

/// The verb sent while a walk is under way joins it and answers with the pages it has still to
/// show: a page the walk passed with *I've switched it on* and no grant read on comes back
/// after its last page, in the same panel, so `aterm doctor` leaves it to the walk and opens no
/// second page beside it. The page up counts the page that went on at once ("Step 2 of 3"),
/// and the first launch's walk, which asks for no page by name, gives it a moment for its
/// grant's first reading as it gives its own. NEGATIVE CONTROL: a page passed with its grant
/// read on is left out (the window reads it held: doctor opens it for the terminal that lacks
/// it).
#[test]
fn the_verb_under_way_takes_back_a_page_the_walk_passed_and_names_only_the_pages_ahead() {
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(&[], "first launch", t0),
        walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
    );
    app.devtools_helper.press_for_test(PanelEvent::SwitchedOn);
    app.tick_devtools_hand_over(t0);
    app.drive_permission_walk(t0);
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((2, 2)))),
        "on with no grant read"
    );
    let shows = panel_counts().0;
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess, Subject::DeveloperTools], t0)
            .reply(),
        "OK walking devtools fda\n"
    );
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((2, 3)))),
        "the page up counts the page that went on"
    );
    let fda_opens = test_arms::fda_counts().0;
    app.devtools_helper.read_for_test(test_arms::on());
    app.tick_consent_lanes(t0 + GRANT_POLL);
    let turned = t0 + GRANT_POLL + DONE_FOR;
    app.tick_consent_lanes(turned);
    assert_eq!(
        app.permission_walk
            .walk()
            .map(|walk| walk.position(|subject| app.walk_reads_held(subject))),
        Some((3, 3)),
        "the walk comes back to it"
    );
    app.tick_consent_lanes(turned + UNREAD_WAIT);
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((3, 3)))),
        "its page, once its reading has had its moment"
    );
    assert_eq!(test_arms::fda_counts().0, fda_opens + 1, "its page opened");
    assert_eq!(panel_counts().0, shows, "in the same panel");

    let mut app = walk_app();
    let _ = app.start_permission_walk(&[], "first launch", t0);
    grant_fda(&mut app);
    app.tick_consent_lanes(t0 + GRANT_POLL);
    app.tick_consent_lanes(t0 + GRANT_POLL + DONE_FOR);
    assert_eq!(
        app.permission_walk.walk().and_then(Walk::current),
        Some(Subject::DeveloperTools)
    );
    assert_eq!(
        app.ask_permission_walk(
            &[Subject::FullDiskAccess, Subject::DeveloperTools],
            t0 + GRANT_POLL + DONE_FOR
        )
        .reply(),
        "OK walking devtools\n"
    );
}

/// A page the verb asks for while a walk is under way counts as asked for by name, and the verb
/// with no subject asks for every page: a Not now on the page a row's press named goes on to it
/// instead of ending the walk. NEGATIVE CONTROL: with no ask, the same Not now ends the walk and
/// opens nothing.
#[test]
fn a_not_now_on_the_pressed_page_goes_on_to_a_page_the_verb_asked_for() {
    let t0 = Instant::now();
    let doctor = [Subject::DeveloperTools, Subject::FullDiskAccess];
    for ask in [Some(&doctor[..]), Some(&[][..]), None] {
        let asked = ask.is_some();
        let mut app = walk_app();
        assert_eq!(
            app.start_permission_walk(&[Subject::DeveloperTools], "a person's press", t0),
            walking_over(&[Subject::DeveloperTools, Subject::FullDiskAccess])
        );
        if let Some(ask) = ask {
            assert_eq!(
                app.ask_permission_walk(ask, t0),
                walking_over(&[Subject::DeveloperTools, Subject::FullDiskAccess])
            );
        }
        let fda_opens = test_arms::fda_counts().0;
        app.devtools_helper.press_for_test(PanelEvent::NotNow);
        app.tick_devtools_hand_over(t0);
        app.tick_consent_lanes(t0 + GRANT_POLL);
        assert_eq!(app.permission_walk.active(), asked, "asked: {ask:?}");
        assert_eq!(
            test_arms::fda_counts().0,
            fda_opens + u32::from(asked),
            "asked: {ask:?}"
        );
    }
}

/// A verb that joins the first launch's walk, which asks for no page by name, names none of its
/// pages: a Not now on Full Disk Access goes on to Developer Tools, as on that walk's own pages,
/// though `aterm doctor` asked for Full Disk Access meanwhile, so the walk keeps the pages
/// nobody asked for by name. NEGATIVE CONTROL: on a walk a row's press named, the verb's pages
/// count as named (`a_not_now_on_the_pressed_page_goes_on_to_a_page_the_verb_asked_for`).
#[test]
fn a_verb_that_joins_the_first_launch_walk_leaves_its_not_now_going_on() {
    let t0 = Instant::now();
    let mut app = walk_app();
    app.permission_walk.start(
        Walk::new(vec![Subject::FullDiskAccess, Subject::DeveloperTools], t0).expect("two pages"),
        "first launch",
    );
    app.drive_permission_walk(t0);
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2))))
    );
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess], t0)
            .reply(),
        "OK walking fda devtools\n"
    );
    let opens = test_arms::opens();
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(t0);
    app.tick_consent_lanes(t0 + GRANT_POLL);
    assert_eq!(
        app.permission_walk.walk().and_then(Walk::current),
        Some(Subject::DeveloperTools),
        "the walk goes on"
    );
    assert_eq!(
        test_arms::opens(),
        opens + 1,
        "Developer Tools' page opened"
    );
}

/// The control verb's walk puts no System Settings page beside another macOS question, though
/// it answers at once: with the setup worker's prompt up (its own guard on the process's dialog
/// slot) or the Local Network alert's hold, `aterm doctor`'s `permissions walk now fda` answers
/// `OK walking` and brings no page or panel up until that question ends, and then its page comes
/// up at the next turn. The walk's own guard, which the window takes at each turn's end while it
/// waits, is no question it waits for. NEGATIVE CONTROL: a person's press over the same prompt
/// opens the page at once.
#[test]
fn the_control_verbs_walk_waits_out_another_macos_question_and_answers_at_once() {
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};
    fn guarded() -> App {
        let mut app = walk_app();
        app.access_steps.set_reach(StepsReach {
            dialog_up: macos_dialog_up,
            holds_dialog: true,
            ..app.access_steps.reach
        });
        app
    }
    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(!macos_dialog_up());
    let t0 = Instant::now();
    let walking = walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools]);

    let mut app = guarded();
    let worker = MacosDialog::when_none_up(Duration::ZERO).expect("the worker takes the slot");
    let (fda_opens, shows) = (test_arms::fda_counts().0, panel_counts().0);
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess], t0),
        walking
    );
    app.tick_consent_lanes(t0 + GRANT_POLL);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens,
        "no page beside the worker's prompt"
    );
    assert_eq!(panel_counts().0, shows, "and no panel");
    assert!(app.permission_walk.active(), "the walk waits");
    drop(worker);
    app.tick_consent_lanes(t0 + GRANT_POLL * 2);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens + 1,
        "its page, the turn after the prompt ended"
    );
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2))))
    );
    drop(app);
    assert!(!macos_dialog_up());

    let mut app = guarded();
    app.access_steps.alert_seen_at_for_test(t0);
    app.user_input_recent = |_| true;
    app.access_steps.dialog = Some(MacosDialog::up());
    let fda_opens = test_arms::fda_counts().0;
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess], t0),
        walking
    );
    app.tick_consent_lanes(t0 + GRANT_POLL);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens,
        "no page beside the Local Network alert"
    );
    app.tick_consent_lanes(t0 + LOCAL_NETWORK_ALERT_HOLD);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens + 1,
        "its page, once the alert's hold ends"
    );
    drop(app);
    assert!(!macos_dialog_up());

    let mut app = guarded();
    let worker = MacosDialog::when_none_up(Duration::ZERO).expect("the worker takes the slot");
    let fda_opens = test_arms::fda_counts().0;
    assert_eq!(
        app.start_permission_walk(&[Subject::FullDiskAccess], "a person's press", t0),
        walking
    );
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens + 1,
        "a person's press opens at once"
    );
    drop(worker);
    drop(app);
    assert!(!macos_dialog_up());
}

/// A person's Install… press that waits for the dialog slot goes before the control verb's walk
/// waiting with it: once the Local Network alert's hold that queued both ends, the walk opens no
/// page and holds no guard while the press waits or its tab is on its way, so the tab takes the
/// slot; the installer then holds it, and the walk's page comes up the turn after the installer
/// ended. NEGATIVE CONTROL: with no press waiting, the walk's page comes up as the hold ends
/// (`the_control_verbs_walk_waits_out_another_macos_question_and_answers_at_once`).
#[test]
fn a_persons_install_press_goes_before_the_control_verbs_walk_waiting_with_it() {
    use crate::clt_install::Install;
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};
    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(!macos_dialog_up());
    let t0 = Instant::now();
    let mut app = walk_app();
    app.access_steps.set_reach(StepsReach {
        dialog_up: macos_dialog_up,
        holds_dialog: true,
        ..app.access_steps.reach
    });
    let hold_ends = t0 + LOCAL_NETWORK_ALERT_HOLD;
    app.access_steps.alert_seen_at_for_test(t0);
    app.user_input_recent = |_| true;
    app.access_steps.dialog = Some(MacosDialog::up());
    // The person pressed Install… under the alert's hold: the press waits for the slot.
    app.workstation_setup.developer_tools = Install::PressedQueued { wanted: None };
    let (fda_opens, shows) = (test_arms::fda_counts().0, panel_counts().0);
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess], t0),
        walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
    );
    app.tick_consent_lanes(hold_ends);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens,
        "no page while the press waits"
    );
    assert!(!macos_dialog_up(), "the slot is free for the press");
    app.workstation_setup.developer_tools = Install::Pressed;
    app.tick_consent_lanes(hold_ends + GRANT_POLL);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens,
        "no page while its tab is on its way"
    );
    assert!(!macos_dialog_up(), "the slot is free for the tab");
    // The tab lands: the installer holds the slot through its run.
    assert!(app.reserve_access_dialog(), "the tab takes the slot");
    app.workstation_setup.developer_tools = Install::Opened;
    app.land_install_session_for_test(7);
    app.tick_consent_lanes(hold_ends + GRANT_POLL * 2);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens,
        "no page beside the installer"
    );
    assert_eq!(panel_counts().0, shows, "and no panel");
    assert!(app.permission_walk.active(), "the walk waits");
    assert!(app.finish_developer_tools_install(7));
    app.tick_consent_lanes(hold_ends + GRANT_POLL * 3);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens + 1,
        "its page, the turn after the installer ended"
    );
    drop(app);
    assert!(!macos_dialog_up());
}

/// A person's press on the row of the page the control verb's walk waits at makes the walk
/// theirs: the page opens at once, as their own walk's would, though the Local Network alert's
/// hold that the verb's walk waits out is still up. NEGATIVE CONTROL: with no press the page
/// waits for the hold to end
/// (`the_control_verbs_walk_waits_out_another_macos_question_and_answers_at_once`).
#[test]
fn a_persons_press_on_the_row_a_verbs_walk_waits_at_opens_its_page_now() {
    let t0 = Instant::now();
    let mut app = walk_app();
    app.access_steps.alert_seen_at_for_test(t0);
    let fda_opens = test_arms::fda_counts().0;
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess], t0),
        walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
    );
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens,
        "the verb's walk waits"
    );
    assert!(app.open_access_from_system_state(Access::FullDiskAccess, true));
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens + 1,
        "the press opens its page now"
    );
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2))))
    );
}

/// A Not now the owner says on the panel the control verb's walk waits behind holds that page
/// back from the walk, as it would hold it back from the verb sent then: with the window's own
/// Full Disk Access panel up, `aterm doctor` sends `permissions walk now fda devtools`; the owner
/// says Not now on that panel, and the walk goes on to Developer Tools without bringing Full
/// Disk Access back. NEGATIVE CONTROL: a Not now the page's markers do not keep holds nothing
/// back, and the walk opens Full Disk Access once the panel goes.
#[test]
fn a_not_now_on_the_panel_a_verbs_walk_waits_behind_holds_that_page_back() {
    for kept in [true, false] {
        test_arms::forget_fda_declines();
        let fda = if kept {
            test_arms::fda_keeping_declines()
        } else {
            test_arms::fda_opening()
        };
        let mut app = walk_app_with_fda(test_arms::opening(), fda);
        assert!(app.consent_card.begin_deciding(Instant::now()));
        app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
        let t0 = Instant::now();
        app.look_for_fda_moment(t0, |_, _| true);
        assert_eq!(
            app.devtools_helper.panel().map(|panel| panel.origin()),
            Some(Origin::Automatic)
        );
        assert_eq!(
            app.ask_permission_walk(&[Subject::FullDiskAccess, Subject::DeveloperTools], t0),
            walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
        );
        app.drive_permission_walk(t0 + GRANT_POLL);
        assert_eq!(
            page_and_step(&app),
            Some((Page::FullDiskAccess, None)),
            "the walk waits behind the window's own panel"
        );
        let fda_opens = test_arms::fda_counts().0;
        app.devtools_helper.press_for_test(PanelEvent::NotNow);
        app.tick_consent_lanes(t0 + GRANT_POLL * 2);
        if kept {
            assert_eq!(
                test_arms::fda_counts().0,
                fda_opens,
                "the page just declined is not brought back"
            );
            assert_eq!(
                page_and_step(&app),
                Some((Page::DeveloperTools, Some((1, 1)))),
                "the walk goes on to Developer Tools, its first page and its last"
            );
        } else {
            assert_eq!(
                test_arms::fda_counts().0,
                fda_opens + 1,
                "a Not now not kept holds nothing back"
            );
            assert_eq!(
                page_and_step(&app),
                Some((Page::FullDiskAccess, Some((1, 2))))
            );
        }
    }
    test_arms::forget_fda_declines();
}

thread_local! {
    static MARKER_READS: Cell<u32> = const { Cell::new(0) };
}

thread_local! {
    static HELD_READS: Cell<u32> = const { Cell::new(0) };
}

thread_local! {
    static STATE: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
    /// The Mac the tests' aterm runs on, by its hardware UUID.
    static THIS_MAC: Cell<&'static str> = const { Cell::new(MAC_A) };
}

/// The Mac a released aterm wrote [`STATE`] on, and another.
const MAC_A: &str = "1D683F62-5C4B-4E3A-9F21-7B8C6D5E4A30";

/// The pages of a first launch's walk.
const LEFT: [Subject; 2] = [Subject::FullDiskAccess, Subject::DeveloperTools];

/// A walk under way when System Settings' Quit & Reopen ends the process leaves its pages, from
/// the one it is at on, in `<state>`'s reopen marker for the aterm that opens again: at Full
/// Disk Access's done words, which wait under macOS's sheet
/// (`docs/measured/first-launch-permission-walk-0.145.0-2026-10-28.md`, finding 1), both;
/// once it turned to Developer Tools, that page alone. NEGATIVE CONTROL: a person's
/// quit (or a logout's) leaves nothing, and neither does a Quit & Reopen with no walk under way.
#[test]
fn a_quit_and_reopen_leaves_the_walks_pages_from_the_one_it_is_at() {
    let dir = aterm_tempfile::tempdir().expect("a scratch <state>");
    STATE.with(|cell| *cell.borrow_mut() = Some(dir.path().to_path_buf()));
    let marker = dir.path().join(REOPEN_MARKER);
    let taken = || {
        take_reopen_in(
            dir.path(),
            aterm_time::unix_secs().expect("the clock"),
            Some(std::path::Path::new(BUNDLE)),
            Some(MAC_A),
        )
    };
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(&[], "first launch", t0),
        walking_over(&LEFT)
    );
    let granted = t0 + GRANT_POLL;
    grant_fda(&mut app);
    SETTINGS_ASKS.with(|asks| asks.set(true));
    app.tick_consent_lanes(granted);
    app.tick_consent_lanes(granted + DONE_FOR);
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2)))),
        "the done words wait under the sheet"
    );

    app.leave_walk_at_quit(false);
    assert!(!marker.exists(), "a person's quit leaves nothing");
    app.leave_walk_at_quit(true);
    assert_eq!(taken(), Some(LEFT.to_vec()), "Quit & Reopen at the sheet");

    SETTINGS_ASKS.with(|asks| asks.set(false));
    app.tick_consent_lanes(granted + DONE_FOR + GRANT_POLL);
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((2, 2))))
    );
    app.leave_walk_at_quit(true);
    assert_eq!(
        taken(),
        Some(vec![Subject::DeveloperTools]),
        "Quit & Reopen at Developer Tools"
    );

    let idle = walk_app();
    idle.leave_walk_at_quit(true);
    assert!(!marker.exists(), "no walk under way");
    STATE.with(|cell| *cell.borrow_mut() = None);
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
        walking_over(&[
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

/// Tick the hand-overs until the `spctl` run that is out has ended and its end was taken; the
/// run is the lane's worker thread's, so a minute is the hang detector.
fn run_out(app: &mut App, at: Instant) {
    let start = Instant::now();
    while app.devtools_helper.listing().is_some() {
        assert!(start.elapsed() < Duration::from_secs(60), "hang");
        app.tick_devtools_hand_over(at);
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A walk at its Developer Tools page, alone (Full Disk Access reads on), whose list reads whole
/// and empty: the page waits for the `spctl` run that lists Terminal.
fn walk_at_listing_devtools(t0: Instant) -> App {
    use crate::devtools_helper::Listing;
    let mut app = walk_app();
    grant_fda(&mut app);
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(0)),
        t0,
    );
    assert_eq!(
        app.start_permission_walk(&[Subject::DeveloperTools], "a person's press", t0),
        walking_over(&[Subject::DeveloperTools])
    );
    assert_eq!(
        app.devtools_helper.listing(),
        Some(Listing::HandOver(Origin::Walk)),
        "the page waits for its run"
    );
    assert!(app.devtools_helper.panel().is_none());
    app
}

/// A press on another row while the walk's `spctl` run is out ends that walk and starts the
/// press's own: the run's end then brings no Developer Tools page over the page the person
/// pressed for, and the new walk's Developer Tools page opens in its turn with that run's
/// answer, needing no run of its own. A message's press that rode the run gets the page as that
/// press's. NEGATIVE CONTROL: with no press the run's end brings the walk's own page up, in its
/// place in the walk.
#[test]
fn a_walks_spctl_run_that_ends_after_a_press_on_another_row_leaves_that_rows_page() {
    use crate::devtools_helper::HandOver;
    let t0 = Instant::now();
    let mut app = walk_at_listing_devtools(t0);
    let opens = test_arms::opens();
    run_out(&mut app, t0);
    assert_eq!(test_arms::opens(), opens + 1, "the walk's page");
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((1, 1))))
    );

    let mut app = walk_at_listing_devtools(t0);
    assert!(app.open_access_from_system_state(Access::Accessibility, true));
    assert_eq!(
        page_and_step(&app),
        Some((Page::Accessibility, Some((1, 2))))
    );
    let opens = test_arms::opens();
    run_out(&mut app, t0);
    assert_eq!(test_arms::opens(), opens, "no Developer Tools page over it");
    assert_eq!(
        page_and_step(&app),
        Some((Page::Accessibility, Some((1, 2)))),
        "the pressed page stays"
    );
    assert!(app.permission_walk.active(), "and its walk goes on");
    AX_TRUSTED.with(|trusted| trusted.set(true));
    app.tick_consent_lanes(t0 + GRANT_POLL);
    AUTOMATION_OUT.with(|out| out.set(false));
    app.tick_consent_lanes(t0 + GRANT_POLL + DONE_FOR);
    assert_eq!(
        (page_and_step(&app), app.devtools_helper.listing()),
        (Some((Page::DeveloperTools, Some((2, 2)))), None),
        "the new walk's page, with the run's answer and no run of its own"
    );
    assert_eq!(test_arms::opens(), opens + 1);

    let mut app = walk_at_listing_devtools(t0);
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Digest),
        HandOver::Listing,
        "a message's press rides the run"
    );
    assert!(app.open_access_from_system_state(Access::Accessibility, true));
    let opens = test_arms::opens();
    run_out(&mut app, t0);
    assert_eq!(test_arms::opens(), opens + 1, "the message's page");
    assert_eq!(
        app.devtools_helper
            .panel()
            .map(|panel| (panel.page(), panel.origin(), panel.step())),
        Some((Page::DeveloperTools, Origin::Digest, None))
    );
}

/// The walk's `spctl` run that ends after the walk's Developer Tools page came up another way
/// (aterm put on the list by hand, a press on that row then opened its page at once) brings no
/// second page: the walk's panel stays in its place and the walk goes on. NEGATIVE CONTROL: the
/// run's end brings the page up while it waits for it
/// (`a_walks_spctl_run_that_ends_after_a_press_on_another_row_leaves_that_rows_page`).
#[test]
fn a_walks_spctl_run_that_ends_after_its_page_came_up_brings_no_second_page() {
    let t0 = Instant::now();
    let mut app = walk_at_listing_devtools(t0);
    assert!(app.open_access_from_system_state(Access::Accessibility, true));
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
        t0,
    );
    let opens = test_arms::opens();
    assert!(app.open_access_from_system_state(Access::DeveloperTools, true));
    assert_eq!(test_arms::opens(), opens + 1, "the row's page, at once");
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((1, 1))))
    );
    run_out(&mut app, t0);
    assert_eq!(test_arms::opens(), opens + 1, "no second page");
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((1, 1))))
    );
    assert!(app.permission_walk.active(), "the walk goes on");
}

/// While macOS's Quit & Reopen / Later sheet stands over System Settings (`settings_busy` and
/// `settings_asks`, which read it by one rule), the Full Disk Access done words wait for the
/// walk's next page past `DONE_FOR` (`Panel::waits`, let go by `App::turn_after_fda`) instead of
/// turning the panel to Developer Tools behind the sheet. Once Later dismisses the sheet, the panel
/// turns to Developer Tools as readable though the reading before the grant held
/// `NoFullDiskAccess` (`Lane::note_fda` retires that list at the grant).
#[test]
fn the_walk_holds_the_full_disk_access_done_card_while_settings_is_busy_and_clears_unread_after_grant()
 {
    thread_local! {
        static BUSY: Cell<bool> = const { Cell::new(false) };
    }
    let t0 = Instant::now();
    let mut app = walk_app();
    let mut arm = counting_arm();
    arm.settings_busy = || BUSY.with(Cell::get);
    arm.settings_asks = || BUSY.with(Cell::get);
    app.devtools_helper =
        Lane::windowed_with_fda_for_test(test_arms::opening(), test_arms::fda_opening(), arm);
    // Before Full Disk Access is granted, the cached reading has `NoFullDiskAccess`.
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::unread()),
        t0,
    );
    assert_eq!(
        app.start_permission_walk(&[], "a test", t0),
        Started::Walking {
            pages: vec![Subject::FullDiskAccess, Subject::DeveloperTools],
            not_now: Vec::new(),
        }
    );

    BUSY.with(|busy| busy.set(true));
    grant_fda(&mut app);
    let granted = t0 + GRANT_POLL;
    app.tick_consent_lanes(granted);
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2))))
    );
    assert_eq!(
        app.devtools_helper.panel().map(|panel| panel.phase()),
        Some(Phase::Done)
    );

    // While the Quit & Reopen / Later sheet stands, `DONE_FOR` does not close the Full Disk
    // Access done words (they wait for the walk's next page) or open Developer Tools.
    let sheet_up = granted + DONE_FOR;
    app.tick_consent_lanes(sheet_up);
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2)))),
        "held while System Settings has its Quit & Reopen sheet up"
    );

    // Later is chosen on the sheet: the next look lets the done words go and turns to Developer
    // Tools, with `NoFullDiskAccess` cleared so the panel is readable and can poll the list.
    BUSY.with(|busy| busy.set(false));
    app.tick_consent_lanes(sheet_up + GRANT_POLL);
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((2, 2)))),
        "turned to Developer Tools once the sheet dismissed"
    );
    assert_eq!(
        app.devtools_helper.panel().map(|panel| panel.readable()),
        Some(true),
        "the Developer Tools panel is readable after Full Disk Access was granted"
    );
}

/// When the 5 s Full Disk Access probe TTL expires and a refresh worker is in flight (`Pending`),
/// `walk_missing` uses the process's observed Full Disk Access grant so a walk started then does
/// not miscount `FullDiskAccess` as a second step to skip.
#[test]
fn an_expired_fda_probe_slot_keeps_the_observed_grant_when_counting_walk_steps() {
    let t0 = Instant::now();
    let mut app = walk_app();
    grant_fda(&mut app);
    // Observe the grant once, then expire the 5 s slot so the next read is `Pending`.
    assert!(!app.walk_missing(Subject::FullDiskAccess, false));
    let _ = app.consent.read_at(
        aterm_containment::consent::ProbeGate::on(),
        Duration::from_secs(5),
        t0 + Duration::from_secs(6),
    );
    assert_eq!(
        app.start_permission_walk(&[], "the Permissions row", t0 + Duration::from_secs(6)),
        Started::Walking {
            pages: vec![Subject::DeveloperTools],
            not_now: Vec::new(),
        },
        "Full Disk Access is not miscounted as a second step while its refresh probe is in flight"
    );
}

/// The Developer Tools page's panel offers Open Developer Tools beside Not now once its list
/// reads (here a list of two, aterm not on it), and the press brings System Settings up at the
/// page again through the opener the hand-over used, the panel staying up on its walk page. This
/// is the way back e2e145 lacked, its panel's Not now alone while System Settings stayed on
/// another page.
/// NEGATIVE CONTROL: a panel whose read list offers Not now alone (the build e2e145 ran) has no
/// press to post, and the page opens no second time.
#[test]
fn the_developer_tools_panels_open_brings_its_page_up_again() {
    use crate::devtools_helper::Button;
    let t0 = Instant::now();
    let mut app = walk_app();
    let opens = test_arms::opens();
    assert_eq!(
        app.start_permission_walk(&[Subject::DeveloperTools], "a test", t0),
        walking_over(&[Subject::DeveloperTools, Subject::FullDiskAccess])
    );
    assert_eq!(test_arms::opens(), opens + 1, "its page opened");
    let buttons = app
        .devtools_helper
        .panel()
        .map(|panel| panel.content().buttons().to_vec())
        .expect("the panel is up");
    assert_eq!(
        buttons,
        [Button::OpenPage(Page::DeveloperTools), Button::NotNow]
    );
    let closes = panel_counts().2;

    app.devtools_helper.press_for_test(PanelEvent::OpenPage);
    app.tick_devtools_hand_over(t0);
    assert_eq!(test_arms::opens(), opens + 2, "the page again");
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((1, 2)))),
        "the panel stays on its page"
    );
    assert_eq!(panel_counts().2, closes, "nothing closed");
    assert!(app.permission_walk.active());
}

/// A person's press on System State never waits behind a helper panel they cannot see (e2e145's
/// Accessibility panel, gone while aterm was in front, held every press until answered). With
/// the walk's Full Disk Access panel up and out of sight: the press on its own row brings the
/// page forward (System Settings at the page, the panel beside it) and leaves the walk as it
/// is; a press on another row has the panel give way, as no Not now, the walk stepped aside and
/// the window's gesture free, and then goes on (Local Network's pane opens). That pane's own
/// bounded hold is past at the next press made back in aterm.
/// NEGATIVE CONTROL: with System Settings in front the person sees the panel, and the same
/// presses leave it standing; before the fix the own row's press only "walked" (no page came)
/// and another row's waited behind a panel nobody saw.
#[test]
fn a_persons_press_never_waits_behind_a_panel_out_of_sight() {
    use crate::system_state_access::UnseenPanel;
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(&[], "a test", t0),
        walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
    );
    let fda_opens = test_arms::fda_counts().0;
    let (_, _, closes) = panel_counts();

    assert!(app.open_access_from_system_state(Access::FullDiskAccess, true));
    assert_eq!(test_arms::fda_counts().0, fda_opens + 1, "brought forward");
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2))))
    );
    assert_eq!(panel_counts().2, closes, "the panel stays");

    let opened = app.open_access_from_system_state(Access::LocalNetwork, true);
    assert!(app.devtools_helper.panel().is_none(), "it gave way");
    assert_eq!(panel_counts().2, closes + 1);
    assert!(!app.permission_walk.active(), "the walk is over");
    assert!(opened, "Local Network's pane opened");
    assert!(
        app.devtools_helper.take_declines().is_empty()
            && !RECORDS.with(|records| records
                .borrow()
                .iter()
                .any(|(_, _, outcome)| *outcome == AccessOutcome::Declined)),
        "no Not now"
    );
    assert_eq!(
        app.press_meets_unseen_panel(Some(Access::Debugger), t0),
        UnseenPanel::PaneLeft,
        "Local Network's pane, held with no panel beside it"
    );
    assert_eq!(
        app.press_meets_unseen_panel(Some(Access::Debugger), t0),
        UnseenPanel::Clear,
        "nothing left to wait behind"
    );

    // The control: the person can see the panel.
    let mut app = walk_app_with_arm(test_arms::opening(), counting_arm_in_front());
    let _ = app.start_permission_walk(&[], "a test", t0);
    let fda_opens = test_arms::fda_counts().0;
    let (_, _, closes) = panel_counts();
    for access in [Access::Debugger, Access::LocalNetwork] {
        assert_eq!(
            app.press_meets_unseen_panel(Some(access), t0),
            UnseenPanel::Clear,
            "{access:?}"
        );
    }
    assert_eq!(test_arms::fda_counts().0, fda_opens);
    assert_eq!(panel_counts().2, closes);
    assert!(app.permission_walk.active());
}

/// A control client's walk (`aterm doctor`, `aterm init`, `permissions walk now`) opens no page
/// while System Settings shows a window of its own over its main one (its password window, Quit &
/// Reopen's sheet): a page opened under that sheet is lost, its panel left beside the question.
/// Its page comes up once the question is gone.
#[test]
fn the_control_verbs_walk_opens_no_page_under_system_settings_sheet() {
    let t0 = Instant::now();
    let mut app = walk_app();
    SETTINGS_ASKS.with(|asks| asks.set(true));
    let fda_opens = test_arms::fda_counts().0;
    let started = app.ask_permission_walk(&[Subject::FullDiskAccess], t0);
    app.tick_consent_lanes(t0 + GRANT_POLL);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens,
        "no page under System Settings' own sheet ({started:?})"
    );
    SETTINGS_ASKS.with(|asks| asks.set(false));
    app.tick_consent_lanes(t0 + GRANT_POLL * 2);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens + 1,
        "the question gone, its page comes up"
    );
}

/// A walk page held off (here behind the walk's own unanswered Automation question) ends the
/// walk once it has waited as long as a panel stays up ([`PANEL_FOR`]) of the person's time at
/// the Mac, as the rest of the walk counts it: twenty minutes with nobody there count nothing,
/// and the walk is still there when they come back.
/// NEGATIVE CONTROL: count the wall clock since the page fell due (`Walk::count_held_off`
/// counting every gap): the walk ends while the person is away.
#[test]
fn a_held_off_walk_page_counts_only_the_persons_time() {
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(&[Subject::Accessibility], "a test", t0),
        walking_over(&[
            Subject::Accessibility,
            Subject::FullDiskAccess,
            Subject::DeveloperTools
        ])
    );
    AX_TRUSTED.with(|trusted| trusted.set(true));
    app.tick_consent_lanes(t0 + GRANT_POLL);
    assert!(AUTOMATION_OUT.with(Cell::get), "macOS's question is up");
    let step = Duration::from_secs(30);
    let mut now = t0 + GRANT_POLL + DONE_FOR;
    app.user_input_recent = |_| false;
    while now < t0 + PANEL_FOR * 2 {
        app.tick_consent_lanes(now);
        now += step;
    }
    assert!(
        app.permission_walk.active(),
        "twenty minutes away count nothing toward the held-off page's bound"
    );
    app.user_input_recent = |within| within >= Duration::from_secs(5);
    let back = now;
    while now <= back + PANEL_FOR + step && app.permission_walk.active() {
        app.tick_consent_lanes(now);
        now += step;
    }
    assert!(
        !app.permission_walk.active(),
        "ten minutes of the person's time with the page held off end the walk"
    );
    assert!(
        now > back + PANEL_FOR - step,
        "not before ten minutes of theirs"
    );
    AUTOMATION_OUT.with(|out| out.set(false));
}
