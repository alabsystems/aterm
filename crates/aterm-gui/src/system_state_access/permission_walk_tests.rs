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
    GRANT_POLL, REOPEN_MARKER, REOPEN_WITHIN, START_KEYS_QUIET, Started, Subject, UNREAD_WAIT,
    Walk, WalkLane, WalkReach, leave_reopen_in, take_reopen_in,
};
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
        assert_eq!(walk.position(|_| false), (2, 2));
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
        test_arms::marked_as(|_| Some((atpkg::devtools::revision_key(1), 1))),
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

thread_local! {
    static FDA_MARKER: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// A first launch whose person stepped away (the last touch two minutes ago: inside the steps'
/// five minutes, past the walk's one) keeps the window's own Full Disk Access step queued through
/// the real tick, however many parks pass, so its page and the marker it writes do not take this
/// Mac's first walk from it; once the person is back the walk starts and its panel takes the
/// page, step 1 of 2. NEGATIVE CONTROL: an update's successor, which never walks by itself, opens
/// the same step's page at the same moment and writes the marker.
#[test]
fn a_step_never_takes_the_first_page_from_a_walk_still_to_come() {
    use crate::devtools_helper::RECHECK_EVERY;
    fn opened_as(_key: &str) -> Option<(String, u64)> {
        FDA_MARKER.with(|marker| marker.borrow().clone().map(|word| (word, 1)))
    }
    fn note_opened(word: &str) {
        FDA_MARKER.with(|marker| *marker.borrow_mut() = Some(word.to_owned()));
    }
    fn marker() -> Option<String> {
        FDA_MARKER.with(|marker| marker.borrow().clone())
    }
    fn launched(cold: bool) -> App {
        FDA_MARKER.with(|marker| *marker.borrow_mut() = None);
        let mut app = walk_app_with_fda(
            test_arms::opening(),
            test_arms::fda_marking(opened_as, note_opened),
        );
        if cold {
            app.devtools_helper.note_cold_start();
        }
        app.user_input_recent = |within| within >= Duration::from_secs(120);
        assert!(app.consent_card.begin_deciding(Instant::now()));
        app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
        assert_eq!(app.devtools_helper.fda_queued(), Some(Origin::Automatic));
        app
    }
    let parks = |app: &mut App, t: Instant| {
        for park in 0..3 {
            app.tick_consent_lanes(t + RECHECK_EVERY * park);
        }
    };

    let mut successor = launched(false);
    let opens = test_arms::fda_counts().0;
    let t0 = Instant::now();
    parks(&mut successor, t0);
    assert_eq!(
        test_arms::fda_counts().0,
        opens + 1,
        "the successor's step opens its page"
    );
    assert!(marker().is_some(), "and writes the marker");

    let mut app = launched(true);
    let opens = test_arms::fda_counts().0;
    parks(&mut app, t0);
    assert_eq!(
        test_arms::fda_counts().0,
        opens,
        "nobody at the Mac: no page"
    );
    assert_eq!(marker(), None, "and no marker");
    assert_eq!(app.devtools_helper.fda_queued(), Some(Origin::Automatic));
    assert!(!app.permission_walk.active());

    app.user_input_recent = |within| within >= Duration::from_secs(1);
    app.tick_consent_lanes(t0 + RECHECK_EVERY * 3);
    assert_eq!(
        app.permission_walk
            .walk()
            .map(|walk| walk.subjects().to_vec()),
        Some(vec![Subject::FullDiskAccess, Subject::DeveloperTools]),
        "the person is back: the walk starts"
    );
    assert_eq!(test_arms::fda_counts().0, opens + 1, "its page comes up");
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2)))),
        "in the walk's panel"
    );
}

thread_local! {
    /// Whether System State's quiet moment has come (`StepsReach::quiet`): the Local Network
    /// step runs only at it, while the walk's start needs only the keyboard's rest.
    static STEPS_QUIET: Cell<bool> = const { Cell::new(false) };
    /// The LAN re-probe's outcome, as its thread leaves it for the window.
    static LAN_PROBE: Cell<Option<(u64, crate::system_state_access::LocalNetworkProbe)>> =
        const { Cell::new(None) };
}

/// A first launch on a fresh Mac, the person at System State and the keyboard at rest, starts no
/// walk while System State's Local Network step is still to run (not yet its quiet moment), nor
/// once it has run while its alert's hold stands, so System Settings does not come up beside
/// macOS's Local Network alert; the walk starts, its page with it, as the hold ends after 30 s.
/// The step runs while the walk is due, the person away or not, and the LAN re-probe's answer
/// ends the hold early: the walk starts at the park after it, with the person back. NEGATIVE
/// CONTROL: on a Mac whose access record holds Local Network the walk starts at the first look.
#[test]
fn the_first_walk_waits_for_the_local_network_question() {
    use crate::system_state_access::LocalNetworkProbe;
    fn fresh() -> App {
        STEPS_QUIET.with(|quiet| quiet.set(false));
        LAN_PROBE.with(|probe| probe.set(None));
        let mut app = walk_app();
        app.access_steps.set_reach(StepsReach {
            recorded: |_, _| None,
            quiet: |_, _| STEPS_QUIET.with(Cell::get),
            take_local_network_probe: || LAN_PROBE.with(Cell::take),
            ..app.access_steps.reach
        });
        app.devtools_helper.note_cold_start();
        app.user_input_recent = |within| within >= Duration::from_secs(5);
        app
    }
    let walking = Some(vec![Subject::FullDiskAccess, Subject::DeveloperTools]);
    let subjects = |app: &App| {
        app.permission_walk
            .walk()
            .map(|walk| walk.subjects().to_vec())
    };
    let t0 = Instant::now();
    let mut app = fresh();
    let (fired, fda_opens) = (triggers(), test_arms::fda_counts().0);
    app.tick_consent_lanes(t0);
    assert_eq!(triggers(), fired, "not yet the step's moment");
    assert_eq!(subjects(&app), None, "the Local Network step is to come");

    STEPS_QUIET.with(|quiet| quiet.set(true));
    let asked = t0 + Duration::from_secs(1);
    app.tick_consent_lanes(asked);
    assert_eq!(triggers(), fired + 1, "the step runs while the walk is due");
    app.tick_consent_lanes(asked + LOCAL_NETWORK_ALERT_HOLD - GRANT_POLL);
    assert_eq!(subjects(&app), None, "its alert's hold stands");
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens,
        "no page beside the alert"
    );
    app.tick_consent_lanes(asked + LOCAL_NETWORK_ALERT_HOLD);
    assert_eq!(subjects(&app), walking, "the hold is over: the walk starts");
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens + 1,
        "its page comes up"
    );

    let mut app = fresh();
    app.user_input_recent = |within| within >= Duration::from_secs(120);
    STEPS_QUIET.with(|quiet| quiet.set(true));
    app.tick_consent_lanes(t0);
    assert_eq!(
        triggers(),
        fired + 2,
        "the person away: the step still runs"
    );
    app.user_input_recent = |within| within >= Duration::from_secs(1);
    app.tick_consent_lanes(t0 + Duration::from_secs(2));
    assert_eq!(subjects(&app), None, "back, under the alert's hold");
    let answered = t0 + Duration::from_secs(4);
    LAN_PROBE.with(|probe| probe.set(Some((1, LocalNetworkProbe::Granted))));
    // The re-probe's wake drains it, then the park looks.
    app.on_local_network_probe_ready(answered);
    app.tick_consent_lanes(answered);
    assert_eq!(subjects(&app), walking, "the answer ended the hold");

    let mut asked_before = walk_app();
    asked_before.devtools_helper.note_cold_start();
    asked_before.user_input_recent = |within| within >= Duration::from_secs(5);
    asked_before.tick_consent_lanes(t0);
    assert_eq!(
        subjects(&asked_before),
        walking,
        "Local Network on record: the walk starts at once"
    );
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

/// The walk's wait for System State's Local Network step ends: while the step's moment does not
/// come (here its quiet moment, held for good), the walk starts [`LOCAL_NETWORK_WAIT`] after the
/// first look nothing else held, says so in aterm.log with what held the step (read before the
/// walk counted among it), and the step asks only once the walk is over, never beside it, nor
/// while System Settings, left up by the walk, still asks something: a window over its main one
/// (Quit & Reopen's sheet) or a quit of this process. The hold aterm.log names then says the walk
/// once, though both the window's gestures and the quiet moment name it.
/// NEGATIVE CONTROL: a look a park before that end starts no walk.
#[test]
fn the_walk_stops_waiting_for_a_local_network_step_that_does_not_come() {
    use crate::permission_walk::LOCAL_NETWORK_WAIT;
    let mut app = walk_app_held_by_local_network(test_arms::opening());
    let walking = Some(vec![Subject::FullDiskAccess, Subject::DeveloperTools]);
    let subjects = |app: &App| {
        app.permission_walk
            .walk()
            .map(|walk| walk.subjects().to_vec())
    };
    let t0 = Instant::now();
    let fired = triggers();
    app.tick_consent_lanes(t0);
    assert_eq!(subjects(&app), None, "the Local Network step is to come");
    app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT - GRANT_POLL);
    assert_eq!(subjects(&app), None, "a park before the wait's end");
    let said = crate::test_log::logged_while(|| app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT));
    assert_eq!(
        subjects(&app),
        walking,
        "the wait is over: the walk goes first"
    );
    let ahead: Vec<_> = said
        .iter()
        .filter(|line| line.contains("the walk goes first"))
        .collect();
    assert!(
        ahead.len() == 1
            && ahead[0].contains("it waits: ")
            && !ahead[0].contains("walk is under way"),
        "said once, with what held the step before the walk: {said:?}"
    );
    assert_eq!(triggers(), fired, "the step has not asked");
    let held = app
        .access_steps
        .local_network_held
        .clone()
        .expect("the step names its hold");
    assert_eq!(
        held.iter()
            .filter(|words| *words == "a permission walk is under way")
            .count(),
        1,
        "a part the gestures and the quiet moment both name is said once: {held:?}"
    );

    STEPS_QUIET.with(|quiet| quiet.set(true));
    let walked = t0 + LOCAL_NETWORK_WAIT + GRANT_POLL;
    app.tick_consent_lanes(walked);
    assert_eq!(
        triggers(),
        fired,
        "its moment came, but never beside the walk"
    );
    app.devtools_helper.press_for_test(PanelEvent::CloseBox);
    app.tick_consent_lanes(walked + GRANT_POLL);
    assert!(
        !app.permission_walk.active(),
        "the close box ended the walk"
    );
    SETTINGS_ASKS.with(|asks| asks.set(true));
    app.tick_consent_lanes(walked + GRANT_POLL * 2);
    assert_eq!(
        triggers(),
        fired,
        "System Settings' sheet stands: never beside it"
    );
    assert_eq!(
        app.access_steps.local_network_held.as_deref(),
        Some(
            &[
                "System Settings shows a window of its own over its main one (its password \
                 window, or Quit & Reopen's sheet), or, with its main window off screen, the one \
                 last seen there is still listed over it (for at most 10 min after the person \
                 left it)"
                    .to_owned()
            ][..]
        )
    );
    SETTINGS_ASKS.with(|asks| asks.set(false));
    QUIT_ASKED.with(|asked| asked.set(true));
    app.tick_consent_lanes(walked + GRANT_POLL * 3);
    assert_eq!(triggers(), fired, "a quit is asked of this process");
    assert_eq!(
        app.access_steps.local_network_held.as_deref(),
        Some(&["a quit is asked of this process (System Settings' Quit & Reopen)".to_owned()][..])
    );
    QUIT_ASKED.with(|asked| asked.set(false));
    app.tick_consent_lanes(walked + GRANT_POLL * 4);
    assert_eq!(triggers(), fired + 1, "the step asks once the walk is over");
}

/// The walk's wait for the Local Network step counts only an unbroken run of looks at which the
/// walk was ready and only that step held it, and never runs out at a look where the step's own
/// moment has come: a look away from System State starts the count again, and the step asks first
/// whenever its moment comes, the wait spent or not. NEGATIVE CONTROLS: the person back at
/// System State with the step's moment met after more than [`LOCAL_NETWORK_WAIT`] away gets the
/// step, not the walk; a run of ready looks as long as the wait, unbroken, still starts it.
#[test]
fn the_walks_wait_for_local_network_counts_only_unbroken_readiness() {
    use crate::permission_walk::LOCAL_NETWORK_WAIT;
    let subjects = |app: &App| {
        app.permission_walk
            .walk()
            .map(|walk| walk.subjects().to_vec())
    };
    let walking = Some(vec![Subject::FullDiskAccess, Subject::DeveloperTools]);
    let t0 = Instant::now();
    let fired = triggers();

    // Away for longer than the wait, then back with the step still held: the count starts again.
    let mut app = walk_app_held_by_local_network(test_arms::opening());
    app.tick_consent_lanes(t0);
    set_front(false);
    app.tick_consent_lanes(t0 + Duration::from_secs(1));
    app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT + Duration::from_secs(5));
    set_front(true);
    let back = t0 + LOCAL_NETWORK_WAIT * 2;
    app.tick_consent_lanes(back);
    assert_eq!(
        subjects(&app),
        None,
        "back at System State: the wait counts from the return"
    );
    app.tick_consent_lanes(back + LOCAL_NETWORK_WAIT - GRANT_POLL);
    assert_eq!(subjects(&app), None, "a park before the wait's end");
    app.tick_consent_lanes(back + LOCAL_NETWORK_WAIT);
    assert_eq!(subjects(&app), walking, "unbroken: the walk goes first");
    assert_eq!(triggers(), fired);

    // The step's moment comes at the very look the wait would run out: the step goes first.
    let mut app = walk_app_held_by_local_network(test_arms::opening());
    app.tick_consent_lanes(t0);
    app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT - GRANT_POLL);
    STEPS_QUIET.with(|quiet| quiet.set(true));
    app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT);
    assert_eq!(triggers(), fired + 1, "the step asks at its own moment");
    assert_eq!(subjects(&app), None, "and the walk waits for its alert");

    // Away for longer than the wait, back with the step's moment met: the step, not the walk.
    let mut app = walk_app_held_by_local_network(test_arms::opening());
    app.tick_consent_lanes(t0);
    set_front(false);
    app.tick_consent_lanes(t0 + Duration::from_secs(1));
    set_front(true);
    STEPS_QUIET.with(|quiet| quiet.set(true));
    app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT * 3);
    assert_eq!(triggers(), fired + 2, "back at its moment: the step asks");
    assert_eq!(subjects(&app), None, "no walk beside its alert");
}

/// Any walk that starts spends the walk's wait for the Local Network step: no look counts while
/// it is under way, so once a person's walk that lasted past [`LOCAL_NETWORK_WAIT`] is over, a
/// walk by itself over a subject missing since (Developer Tools, read on at the press, now off)
/// waits a fresh, unbroken
/// [`LOCAL_NETWORK_WAIT`] of ready looks before it goes ahead of the step.
/// NEGATIVE CONTROL: that fresh run, unbroken, starts it.
#[test]
fn a_walk_that_starts_spends_the_walks_wait_for_local_network() {
    use crate::permission_walk::LOCAL_NETWORK_WAIT;
    let subjects = |app: &App| {
        app.permission_walk
            .walk()
            .map(|walk| walk.subjects().to_vec())
    };
    let t0 = Instant::now();
    let fired = triggers();
    let mut app = walk_app_held_by_local_network(test_arms::opening());
    app.tick_consent_lanes(t0);
    assert_eq!(subjects(&app), None, "the Local Network step is to come");
    let pressed = t0 + Duration::from_secs(1);
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::Granted, test_arms::on()),
        pressed,
    );
    assert_eq!(
        app.start_permission_walk(&[Subject::FullDiskAccess], "a person's press", pressed),
        walking_over(&[Subject::FullDiskAccess])
    );
    app.tick_consent_lanes(pressed + GRANT_POLL);
    app.devtools_helper.press_for_test(PanelEvent::CloseBox);
    let ended = t0 + LOCAL_NETWORK_WAIT + Duration::from_secs(5);
    app.tick_consent_lanes(ended);
    assert!(!app.permission_walk.active(), "the close box ended it");
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
        ended,
    );
    let back = ended + GRANT_POLL;
    app.tick_consent_lanes(back);
    assert_eq!(
        subjects(&app),
        None,
        "the person's walk spent the wait: it counts again from here"
    );
    app.tick_consent_lanes(back + LOCAL_NETWORK_WAIT - GRANT_POLL);
    assert_eq!(subjects(&app), None, "a park before the fresh wait's end");
    app.tick_consent_lanes(back + LOCAL_NETWORK_WAIT);
    assert_eq!(
        subjects(&app),
        Some(vec![Subject::DeveloperTools]),
        "unbroken: the walk goes first"
    );
    assert_eq!(triggers(), fired, "the step has not asked");
}

/// System State's Local Network step and the walk that starts by itself read System Settings
/// asking something alike: while a window of its own stands over its main one (a sheet the
/// person left there) or a quit of this process is asked, the walk is not ready, so its wait for
/// the step never runs out on that question and System Settings' page never comes up under it.
/// NEGATIVE CONTROL: once the question goes, an unbroken [`LOCAL_NETWORK_WAIT`] of ready looks
/// starts the walk.
#[test]
fn the_walk_never_goes_first_while_system_settings_asks() {
    use crate::permission_walk::LOCAL_NETWORK_WAIT;
    let t0 = Instant::now();
    let mut app = walk_app_held_by_local_network(test_arms::opening());
    let fda_opens = test_arms::fda_counts().0;
    SETTINGS_ASKS.with(|asks| asks.set(true));
    app.tick_consent_lanes(t0);
    app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT);
    app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT * 2);
    assert!(!app.permission_walk.active(), "a sheet stands: no walk");
    SETTINGS_ASKS.with(|asks| asks.set(false));
    QUIT_ASKED.with(|asked| asked.set(true));
    app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT * 3);
    assert!(!app.permission_walk.active(), "a quit is asked: no walk");
    assert_eq!(test_arms::fda_counts().0, fda_opens, "no page under it");
    QUIT_ASKED.with(|asked| asked.set(false));
    let gone = t0 + LOCAL_NETWORK_WAIT * 3 + GRANT_POLL;
    app.tick_consent_lanes(gone);
    app.tick_consent_lanes(gone + LOCAL_NETWORK_WAIT - GRANT_POLL);
    assert!(
        !app.permission_walk.active(),
        "a park before the wait's end"
    );
    app.tick_consent_lanes(gone + LOCAL_NETWORK_WAIT);
    assert!(
        app.permission_walk.active(),
        "unbroken since it went: the walk"
    );
}

/// The walk's look reads System Settings asking (which may read the window list) last, once,
/// and only at a look that would start a walk, and a wait for the Local Network step reads the
/// page markers, the declines and System Settings once per wait, not once per park: the look
/// that begins it works out the walk (markers and declines read, System Settings not), the looks
/// between read memory alone however many parks pass with the person at System State, and the
/// look that finds it run out works the walk out again and reads System Settings once. A Mac
/// whose markers hold the walk for good reads System Settings at no park. A look at which the
/// wait ran out but the step's own moment has come lets the step ask first, reading nothing.
/// NEGATIVE CONTROL: the look that finds the wait run out reads them, and starts the walk.
#[test]
fn the_walks_look_reads_files_and_system_settings_once_per_wait() {
    use crate::permission_walk::LOCAL_NETWORK_WAIT;
    thread_local! {
        static READS: Cell<u32> = const { Cell::new(0) };
        static MARKS: Cell<u32> = const { Cell::new(0) };
        static DECLINES: Cell<u32> = const { Cell::new(0) };
    }
    fn every_mac(key: &str) -> Option<(String, u64)> {
        Some((key.to_owned(), 1))
    }
    let reads = || READS.with(Cell::get);
    let files = || {
        (
            MARKS.with(Cell::get),
            DECLINES.with(Cell::get) + AX_DECLINE_READS.with(Cell::get),
        )
    };
    let arm = || {
        test_arms::asking(counting_arm(), || {
            bump(&READS);
            false
        })
    };
    let mut fda = test_arms::fda_opening();
    fda.opened_as = every_mac;
    let mut held = walk_app_with_parts(test_arms::opening(), fda, arm());
    held.devtools_helper.note_cold_start();
    held.user_input_recent = |_| true;
    let t0 = Instant::now();
    let before = reads();
    for tick in 0..8 {
        held.tick_permission_walk(t0 + GRANT_POLL * tick);
    }
    assert!(!held.permission_walk.active(), "the markers hold it");
    assert_eq!(
        reads(),
        before,
        "no walk can start: System Settings is not read"
    );

    let mut fda = test_arms::fda_opening();
    fda.opened_as = |_| {
        bump(&MARKS);
        None
    };
    fda.last_declined = || {
        bump(&DECLINES);
        None
    };
    let mut waiting = walk_app_with_parts(test_arms::opening(), fda, arm());
    STEPS_QUIET.with(|quiet| quiet.set(false));
    waiting.access_steps.set_reach(StepsReach {
        recorded: |_, _| None,
        quiet: |_, _| STEPS_QUIET.with(Cell::get),
        names_holds: true,
        ..waiting.access_steps.reach
    });
    waiting.devtools_helper.note_cold_start();
    waiting.user_input_recent = |_| true;
    let (marks, declines) = files();
    let before = reads();
    waiting.tick_permission_walk(t0);
    assert!(!waiting.permission_walk.active(), "the wait begins");
    let begun = files();
    assert!(
        begun.0 > marks && begun.1 > declines,
        "the look that begins the wait works out the walk: {begun:?} after {:?}",
        (marks, declines)
    );
    assert_eq!(reads(), before, "the wait begins: System Settings not read");
    for tick in 1..40 {
        waiting.tick_permission_walk(t0 + LOCAL_NETWORK_WAIT * tick / 40);
    }
    assert!(!waiting.permission_walk.active(), "waiting");
    assert_eq!(
        files(),
        begun,
        "the looks between read no markers, declines or grants"
    );
    assert_eq!(reads(), before, "nor System Settings");
    // The wait ran out at this look, but the step's own moment has come: it asks first.
    STEPS_QUIET.with(|quiet| quiet.set(true));
    waiting.tick_permission_walk(t0 + LOCAL_NETWORK_WAIT);
    assert!(
        !waiting.permission_walk.active(),
        "the step's moment: it asks first"
    );
    assert_eq!(
        (files(), reads()),
        (begun, before),
        "the step's moment: nothing read"
    );
    STEPS_QUIET.with(|quiet| quiet.set(false));
    waiting.tick_permission_walk(t0 + LOCAL_NETWORK_WAIT + GRANT_POLL);
    assert!(waiting.permission_walk.active(), "the wait ran out");
    let ended = files();
    assert!(
        ended.0 > begun.0 && ended.1 > begun.1,
        "the look that starts it works out the walk again"
    );
    assert_eq!(reads(), before + 1, "and reads System Settings once");
}

/// The question System Settings asks is read while the person is with it, so a sheet left
/// standing holds the walk once System Settings is off screen, whatever order the person's leave
/// takes its windows away in: with System Settings in front nothing of the window's steps reads
/// it (System State is not in front), but the watch
/// (`devtools_helper::DevToolsHelperArm::watch_settings`) has the lane read it as System Settings
/// comes and every `devtools_helper::SETTINGS_FRONT_READ` while it stays the active app, so the
/// sheet that comes up there is seen on screen and remembered by its number
/// (`devtools_helper::settings_asking`). The person then swipes to aterm's full-screen Space (or
/// presses Cmd-H): its windows leave the screen before System Settings resigns the active app,
/// so the reading at the leave finds none on screen. Back at System State, the walk is not ready
/// while that sheet is still listed over that main window, and its wait runs out once the
/// sheet's number leaves the list. NEGATIVE CONTROL: the same person with no watch (the sheet
/// seen by no reading on screen) has the walk start beside the sheet.
#[test]
fn a_sheet_left_standing_is_read_while_the_person_is_with_system_settings() {
    use std::cell::RefCell;

    use crate::devtools_helper::{
        Asking, Rect, SETTINGS_FRONT_READ, SeenAsking, SettingsMoves, SettingsWindow,
        settings_asking,
    };
    use crate::permission_walk::LOCAL_NETWORK_WAIT;
    thread_local! {
        static ON_SCREEN: RefCell<Vec<SettingsWindow>> = const { RefCell::new(Vec::new()) };
        static LISTED: RefCell<Vec<SettingsWindow>> = const { RefCell::new(Vec::new()) };
        static SEEN: Cell<Option<SeenAsking>> = const { Cell::new(None) };
        static ACTIVE: Cell<bool> = const { Cell::new(false) };
        static MOVES: RefCell<Option<SettingsMoves>> = const { RefCell::new(None) };
    }
    // The live reading's rule over the windows the test lists.
    fn asks() -> bool {
        let on_screen = ON_SCREEN.with(|windows| windows.borrow().clone());
        let (asking, seen) = settings_asking(
            &on_screen,
            0,
            SEEN.with(Cell::get),
            Instant::now(),
            ACTIVE.with(Cell::get),
            || LISTED.with(|windows| windows.borrow().clone()),
        );
        SEEN.with(|cell| cell.set(seen));
        asking == Asking::Asks
    }
    let main = Rect {
        x: 142.0,
        y: 86.0,
        w: 740.0,
        h: 625.0,
    };
    let sheet = Rect {
        x: 382.0,
        y: 300.0,
        w: 260.0,
        h: 199.0,
    };
    let window = |number, bounds| SettingsWindow {
        number,
        layer: 0,
        alpha: 1.0,
        bounds,
    };
    let standing = vec![window(57, main), window(82, sheet)];
    let answered = vec![window(57, main)];
    let set = |cell: &'static std::thread::LocalKey<RefCell<Vec<SettingsWindow>>>,
               windows: &[SettingsWindow]| {
        cell.with(|cell| *cell.borrow_mut() = windows.to_vec());
    };
    // System Settings becomes or resigns the active app, as the live watch hears it.
    let moved = |active: bool| {
        ACTIVE.with(|cell| cell.set(active));
        if let Some(moves) = MOVES.with(|slot| slot.borrow().clone()) {
            if active { moves.came() } else { moves.left() }
        }
    };
    let run = |watch: bool| -> (bool, bool) {
        SEEN.with(|cell| cell.set(None));
        ACTIVE.with(|cell| cell.set(false));
        MOVES.with(|slot| *slot.borrow_mut() = None);
        let mut arm = test_arms::asking(counting_arm(), asks);
        arm.watch_settings = if watch {
            |moves| {
                MOVES.with(|slot| *slot.borrow_mut() = Some(moves));
                true
            }
        } else {
            |_| false
        };
        STEPS_QUIET.with(|quiet| quiet.set(false));
        let mut app = walk_app_with_arm(test_arms::opening(), arm);
        app.access_steps.set_reach(StepsReach {
            recorded: |_, _| None,
            quiet: |_, _| STEPS_QUIET.with(Cell::get),
            names_holds: true,
            ..app.access_steps.reach
        });
        app.devtools_helper.note_cold_start();
        app.user_input_recent = |within| within >= Duration::from_secs(5);
        let t0 = Instant::now();
        // The person goes to System Settings, at Full Disk Access, nothing asked yet.
        set_front(false);
        set(&ON_SCREEN, &answered);
        set(&LISTED, &answered);
        app.tick_consent_lanes(t0);
        assert!(app.devtools_helper.watch_tried(), "the window watches");
        moved(true);
        app.tick_consent_lanes(t0 + GRANT_POLL / 2);
        // They switch aterm on: macOS's Quit & Reopen sheet comes up over the page.
        set(&ON_SCREEN, &standing);
        set(&LISTED, &standing);
        app.tick_consent_lanes(t0 + GRANT_POLL / 2 + SETTINGS_FRONT_READ);
        // They swipe back to aterm's full-screen Space, the sheet unanswered: the windows leave
        // the screen before System Settings resigns the active app.
        set(&ON_SCREEN, &[]);
        moved(false);
        app.tick_consent_lanes(t0 + GRANT_POLL + SETTINGS_FRONT_READ);
        set_front(true);
        let back = t0 + GRANT_POLL * 2 + SETTINGS_FRONT_READ;
        for at in [
            back,
            back + LOCAL_NETWORK_WAIT,
            back + LOCAL_NETWORK_WAIT * 2,
        ] {
            app.tick_consent_lanes(at);
        }
        let held = !app.permission_walk.active();
        // Later, pressed on System Settings' Space: the sheet's number leaves the list.
        set(&LISTED, &answered);
        let gone = back + LOCAL_NETWORK_WAIT * 3;
        app.tick_consent_lanes(gone);
        app.tick_consent_lanes(gone + LOCAL_NETWORK_WAIT);
        (held, app.permission_walk.active())
    };
    assert_eq!(
        run(true),
        (true, true),
        "read while the person was with System Settings: the sheet holds the walk off screen \
         until it goes"
    );
    assert_eq!(
        run(false),
        (false, true),
        "control: never read on screen, the sheet is not known and the walk goes beside it"
    );
}

/// A question System Settings was seen asking that is still listed over its main window off
/// screen past `devtools_helper::ASKING_KEPT_FOR` after the person left it (lapsed: maybe still
/// standing, maybe an answered sheet's number kept) holds what opens System Settings' pages (the
/// step moment of the steps after Local Network, the walk's start), whose `open` under a standing
/// sheet is lost, until a reading on screen shows it gone; the Local Network alert's moment and
/// the debugger step's no longer wait for it. NEGATIVE CONTROL: with nothing lapsed the same
/// moments come and the walk starts.
#[test]
fn a_lapsed_question_holds_only_what_opens_system_settings() {
    thread_local! {
        static LAPSED: Cell<bool> = const { Cell::new(false) };
    }
    LAPSED.with(|lapsed| lapsed.set(true));
    let mut arm = counting_arm();
    arm.settings_lapsed = || LAPSED.with(Cell::get);
    let mut app = walk_app_with_arm(test_arms::opening(), arm);
    app.access_steps.set_reach(StepsReach {
        recorded: |_, _| Some(AccessOutcome::Triggered),
        quiet: |_, _| true,
        names_holds: true,
        ..app.access_steps.reach
    });
    let t0 = Instant::now();
    // The Local Network step is on record for this revision: done at this look.
    app.tick_local_network_step(t0);
    assert!(!app.local_network_pending(), "Local Network done");
    assert!(!app.system_settings_asks(), "lapsed: it does not ask");
    assert!(app.system_settings_may_ask(), "but it may");
    assert!(
        app.local_network_moment(t0),
        "the Local Network alert's moment does not wait for it"
    );
    assert!(
        !app.debugger_step_moment().settings_asks,
        "nor the debugger step's"
    );
    assert!(
        !app.access_step_moment(t0),
        "a step that opens a System Settings page waits for it"
    );
    LAPSED.with(|lapsed| lapsed.set(false));
    assert!(
        app.access_step_moment(t0),
        "control: nothing lapsed, the step's moment"
    );
    LAPSED.with(|lapsed| lapsed.set(true));
    app.devtools_helper.note_cold_start();
    app.user_input_recent = |_| true;
    app.tick_permission_walk(t0);
    app.tick_permission_walk(t0 + GRANT_POLL);
    assert!(!app.permission_walk.active(), "the walk waits for it");
    LAPSED.with(|lapsed| lapsed.set(false));
    app.tick_permission_walk(t0 + GRANT_POLL * 2);
    assert!(
        app.permission_walk.active(),
        "control: read on screen gone, the walk starts"
    );
}

/// Where macOS's idle time cannot be read, the walk that starts by itself reads the person as the
/// steps do ([`App::person_present_within`]): with no key, click or scroll in an aterm window it
/// never starts, its wait for the Local Network step never running out however long the look
/// lasts, and the later steps wait with it ([`App::walk_waits_for_the_person`]); one click in
/// aterm is the person there, the wait counts from it, and the person read present at the look
/// that began it stays present through it, so that one click lets it run out though no other
/// touch follows. NEGATIVE CONTROLS: a look a park before the wait's end starts no walk; the same
/// Mac with its idle time read starts the walk at [`LOCAL_NETWORK_WAIT`].
#[test]
fn an_unreadable_idle_time_holds_the_walk_until_the_person_touches_aterm() {
    use crate::permission_walk::LOCAL_NETWORK_WAIT;
    let t0 = Instant::now();
    let unreadable = || {
        let mut app = walk_app_held_by_local_network(test_arms::opening());
        // What the live reading answers when `HIDIdleTime` cannot be read.
        app.user_input_recent = |_| true;
        app.user_input_readable = || false;
        app
    };
    let mut app = unreadable();
    for at in [t0, t0 + LOCAL_NETWORK_WAIT, t0 + LOCAL_NETWORK_WAIT * 2] {
        app.tick_consent_lanes(at);
    }
    assert!(!app.permission_walk.active(), "nobody known there: no walk");
    assert!(
        app.walk_waits_for_the_person(t0 + LOCAL_NETWORK_WAIT * 2),
        "the later steps wait with it"
    );
    let clicked = t0 + LOCAL_NETWORK_WAIT * 2 + Duration::from_secs(1);
    app.last_person_input_at = Some(clicked);
    assert!(!app.walk_waits_for_the_person(clicked), "a click in aterm");
    app.tick_consent_lanes(clicked);
    app.tick_consent_lanes(clicked + LOCAL_NETWORK_WAIT - GRANT_POLL);
    assert!(
        !app.permission_walk.active(),
        "the wait counts from the click"
    );
    app.tick_consent_lanes(clicked + LOCAL_NETWORK_WAIT);
    assert!(
        app.permission_walk.active(),
        "one click, no touch since: the wait runs out, and the walk"
    );

    let mut read = unreadable();
    read.user_input_readable = || true;
    read.tick_consent_lanes(t0);
    read.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT);
    assert!(read.permission_walk.active(), "control: an idle time read");
}

/// A flick of keyboard focus shorter than `FOCUS_AWAY_FOR`, which holds no typing for the steps
/// (`App::note_keyboard_focus`), breaks no run of the walk's wait for the Local Network step: a
/// look while no aterm window has had focus for 5 ms neither starts the walk nor counts against
/// the wait, and the walk starts at [`LOCAL_NETWORK_WAIT`]. NEGATIVE CONTROLS: focus away
/// `FOCUS_AWAY_FOR` at a look, or taken back after that long away with no look between, starts
/// the count again.
#[test]
fn a_focus_flick_breaks_no_wait_for_local_network() {
    use crate::permission_walk::LOCAL_NETWORK_WAIT;
    /// A case: its name, what happens by `mid`, and whether the wait runs unbroken.
    type Case = (&'static str, fn(&mut App, Instant), bool);
    let t0 = Instant::now();
    let mid = t0 + Duration::from_secs(30);
    let cases: [Case; 3] = [
        (
            "a 5 ms flick",
            |app, mid| {
                set_front(false);
                app.keyboard_focus_lost_at = mid.checked_sub(Duration::from_millis(5));
            },
            true,
        ),
        (
            "away for FOCUS_AWAY_FOR at a look",
            |app, mid| {
                set_front(false);
                app.keyboard_focus_lost_at = mid.checked_sub(crate::FOCUS_AWAY_FOR);
            },
            false,
        ),
        (
            "back after FOCUS_AWAY_FOR away, no look between",
            |app, mid| {
                app.keyboard_focus_lost_at = mid.checked_sub(crate::FOCUS_AWAY_FOR * 2);
                app.keyboard_focus_back_at = Some(mid);
            },
            false,
        ),
    ];
    for (name, at_mid, unbroken) in cases {
        let mut app = walk_app_held_by_local_network(test_arms::opening());
        app.tick_consent_lanes(t0);
        at_mid(&mut app, mid);
        app.tick_consent_lanes(mid);
        assert!(!app.permission_walk.active(), "{name}: no walk mid-way");
        set_front(true);
        app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT);
        assert_eq!(app.permission_walk.active(), unbroken, "{name}");
        if !unbroken {
            app.tick_consent_lanes(t0 + LOCAL_NETWORK_WAIT * 2);
            assert!(app.permission_walk.active(), "{name}: counted again");
        }
    }
}

/// System State's debugger step waits on the run's order as the access steps do
/// (`App::debugger_step_moment`): while the Local Network step is still to run (after a walk that
/// went first handed it the debugger) and while System Settings asks something (a sheet over its
/// main window, a quit of this process), and its admission waits for each
/// (`lib_macos_access_refresh_tests`). NEGATIVE CONTROL: Local Network on record and System
/// Settings asking nothing hold it neither way.
#[test]
fn the_debugger_steps_moment_reads_local_network_and_system_settings() {
    let mut held = walk_app_held_by_local_network(test_arms::opening());
    assert!(held.local_network_step_to_come());
    let moment = held.debugger_step_moment();
    assert!(
        moment.local_network_pending,
        "the Local Network step is to come"
    );

    let mut app = walk_app();
    assert!(!app.local_network_step_to_come(), "on record");
    let moment = app.debugger_step_moment();
    assert!(
        !moment.local_network_pending && !moment.settings_asks,
        "control: nothing holds it"
    );
    SETTINGS_ASKS.with(|asks| asks.set(true));
    assert!(app.debugger_step_moment().settings_asks, "a sheet stands");
    SETTINGS_ASKS.with(|asks| asks.set(false));
    QUIT_ASKED.with(|asked| asked.set(true));
    assert!(app.debugger_step_moment().settings_asks, "a quit is asked");
    QUIT_ASKED.with(|asked| asked.set(false));
    assert!(!app.debugger_step_moment().settings_asks);
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

/// A page the verb takes back goes before the debugger still to come, so the debugger's step
/// stays last and the panel stays up through it: on the first launch's walk over Full Disk
/// Access, Developer Tools and the debugger, Full Disk Access passed with no grant read on comes
/// back after Developer Tools in the same panel, and the debugger goes to System State's step
/// only after it.
#[test]
fn a_page_the_verb_takes_back_comes_before_the_debugger_in_the_same_panel() {
    use crate::debugger_access::DebuggerStepQueue;
    let t0 = Instant::now();
    let later = t0 + Duration::from_secs(600);
    let mut app = walk_app();
    app.debugger_launch = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
    app.debugger_launch.look_later(later);
    assert_eq!(
        app.start_permission_walk(&[], "first launch", t0),
        walking_over(&[
            Subject::FullDiskAccess,
            Subject::DeveloperTools,
            Subject::Debugger
        ])
    );
    app.devtools_helper.press_for_test(PanelEvent::SwitchedOn);
    app.tick_devtools_hand_over(t0);
    app.drive_permission_walk(t0);
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((2, 3))))
    );
    let shows = panel_counts().0;
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess, Subject::DeveloperTools], t0)
            .reply(),
        "OK walking devtools fda debugger\n"
    );
    app.devtools_helper.read_for_test(test_arms::on());
    app.tick_consent_lanes(t0 + GRANT_POLL);
    let turned = t0 + GRANT_POLL + DONE_FOR;
    app.tick_consent_lanes(turned);
    let back = turned + UNREAD_WAIT;
    app.tick_consent_lanes(back);
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((3, 4)))),
        "Full Disk Access comes back before the debugger"
    );
    assert_eq!(panel_counts().0, shows, "in the same panel");
    assert_eq!(
        app.debugger_launch.next_deadline(),
        Some(later),
        "the debugger's step still waits for its moment"
    );

    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(back);
    let handed = back + UNREAD_WAIT;
    app.drive_permission_walk(handed);
    assert!(!app.permission_walk.active(), "the debugger was the last");
    assert_eq!(
        app.debugger_launch.next_deadline(),
        Some(handed),
        "System State's step looks once the page before it ended"
    );
}

/// A page the verb asks for while the walk waits at the debugger, its step not yet handed, goes
/// before it too: the debugger's page never comes up, so the panel held up for it turns to the
/// page the verb asked for, and System State's step still comes last. (A walk at a page that is
/// up keeps it: `a_page_the_verb_takes_back_comes_before_the_debugger_in_the_same_panel`.)
#[test]
fn a_page_the_verb_asks_for_at_the_debugger_comes_up_before_it_in_the_same_panel() {
    let t0 = Instant::now();
    let mut app = with_debugger_step(walk_app());
    let later = app.debugger_launch.next_deadline();
    app.permission_walk.start(
        Walk::new(vec![Subject::DeveloperTools, Subject::Debugger], t0).expect("two pages"),
        "first launch",
    );
    app.drive_permission_walk(t0);
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(t0);
    assert_eq!(
        app.permission_walk.walk().and_then(Walk::current),
        Some(Subject::Debugger),
        "the walk waits at the debugger"
    );
    let (shows, _, closes) = panel_counts();
    let fda_opens = test_arms::fda_counts().0;
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess], t0)
            .reply(),
        "OK walking fda devtools debugger\n",
        "the page passed comes back too, both before the debugger"
    );
    app.drive_permission_walk(t0 + UNREAD_WAIT);
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((2, 4)))),
        "Full Disk Access comes up before the debugger"
    );
    assert_eq!(test_arms::fda_counts().0, fda_opens + 1, "its page opened");
    assert_eq!(
        (panel_counts().0, panel_counts().2),
        (shows, closes),
        "in the same panel"
    );
    assert_eq!(
        app.debugger_launch.next_deadline(),
        later,
        "the debugger's step still waits for its moment"
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
    app.access_steps.local_network_until = Some(t0 + LOCAL_NETWORK_ALERT_HOLD);
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
    app.access_steps.local_network_until = Some(hold_ends);
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
    app.access_steps.local_network_until = Some(t0 + LOCAL_NETWORK_ALERT_HOLD);
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

/// The walk's start is looked at every park, so it reads the page markers only once every
/// in-memory reading says start: an update's successor, or a window whose System State is
/// away, reads none, and once a look finds one no later look reads them again. NEGATIVE
/// CONTROL: the look that would start the walk reads them.
#[test]
fn the_walks_start_reads_the_page_markers_last_and_never_again_once_one_is_found() {
    fn unseen(_key: &str) -> Option<(String, u64)> {
        bump(&MARKER_READS);
        None
    }
    fn seen(_key: &str) -> Option<(String, u64)> {
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

    let mut successor = present(test_arms::marked_as(unseen), false);
    let before = reads();
    ticks(&mut successor, 3);
    assert_eq!(reads(), before, "an update's successor reads no marker");

    let mut away = present(test_arms::marked_as(unseen), true);
    set_front(false);
    ticks(&mut away, 3);
    set_front(true);
    assert_eq!(reads(), before, "System State away: no marker read");

    let mut seen_before = present(test_arms::marked_as(seen), true);
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

/// A Mac whose `<state>` came from another Mac (Migration Assistant, a network home) holds that
/// Mac's openings of both pages: Full Disk Access's line names that Mac, and the plain Developer
/// Tools word has that Mac's identity written beside it. Neither is this Mac's, so the walk
/// starts by itself over both pages. NEGATIVE CONTROL: this bundle's opening on this Mac, of
/// either page, holds the walk back (the tests' key names no Mac, so this Mac's Developer Tools
/// line stands as the key's words with this Mac's after them).
#[test]
fn the_walk_starts_on_a_mac_whose_pages_came_up_only_on_another_mac() {
    fn elsewhere() -> Option<(String, u64)> {
        Some((
            "r1@com.aterm.aterm@5B1E0C2A-7F3D-4E8B-9A61-0D2C4F6E8A10".to_owned(),
            1,
        ))
    }
    fn plain() -> Option<(String, u64)> {
        Some((atpkg::devtools::revision_key(1), 1))
    }
    fn here(key: &str) -> Option<(String, u64)> {
        Some((key.to_owned(), 1))
    }
    fn devtools_here(key: &str) -> Option<(String, u64)> {
        Some((
            format!("{key}@com.aterm.aterm@0E1D2C3B-4A59-4867-8F70-9A8B7C6D5E4F"),
            1,
        ))
    }
    fn cold(
        devtools: crate::devtools_helper::Reach,
        fda_as: fn(&str) -> Option<(String, u64)>,
    ) -> App {
        let mut fda = test_arms::fda_opening();
        fda.last_opened = elsewhere;
        fda.opened_as = fda_as;
        let mut app = walk_app_with_fda(devtools, fda);
        app.devtools_helper.note_cold_start();
        app.user_input_recent = |_| true;
        app
    }
    let t0 = Instant::now();
    let mut moved = cold(test_arms::marked(plain), |_| None);
    moved.tick_permission_walk(t0);
    assert_eq!(
        moved
            .permission_walk
            .walk()
            .map(|walk| walk.subjects().to_vec()),
        Some(vec![Subject::FullDiskAccess, Subject::DeveloperTools]),
        "another Mac's pages leave this Mac's walk"
    );

    let mut fda_here = cold(test_arms::marked(plain), here);
    fda_here.tick_permission_walk(t0);
    assert!(
        !fda_here.permission_walk.active(),
        "this Mac's Full Disk Access page came up"
    );
    let mut devtools_here = cold(test_arms::marked_as(devtools_here), |_| None);
    devtools_here.tick_permission_walk(t0);
    assert!(
        !devtools_here.permission_walk.active(),
        "this Mac's Developer Tools page came up"
    );
}

thread_local! {
    static HELD_READS: Cell<u32> = const { Cell::new(0) };
}

/// Until the key names this Mac (the tests' key carries no bundle id or Mac, as before both are
/// read), an opening that names a Mac might be this one's or another's: it holds the start, and
/// a look under the same key finds the same, so however many parks pass the markers are read
/// once. Once the key changes they are read again, and an opening that reads as another Mac's
/// under the new key leaves the look to start the walk. The tests' key never gains the Mac, so a
/// new approved revision changes it here, and the stand-in answers the new key as it would a key
/// that names this Mac: the opening is another Mac's. NEGATIVE CONTROL: an opening that names no
/// Mac is every Mac's and holds the walk for good
/// (`the_walks_start_reads_the_page_markers_last_and_never_again_once_one_is_found`).
#[test]
fn an_opening_that_names_a_mac_holds_the_start_and_is_read_again_only_under_a_new_key() {
    fn opened_as(key: &str) -> Option<(String, u64)> {
        bump(&HELD_READS);
        (key == atpkg::devtools::revision_key(1)).then(|| {
            (
                format!("{key}@com.aterm.aterm@5B1E0C2A-7F3D-4E8B-9A61-0D2C4F6E8A10"),
                1,
            )
        })
    }
    let reads = || HELD_READS.with(Cell::get);
    let mut fda = test_arms::fda_opening();
    fda.opened_as = opened_as;
    let mut app = walk_app_with_fda(test_arms::opening(), fda);
    app.devtools_helper.note_cold_start();
    app.user_input_recent = |_| true;
    let t0 = Instant::now();
    let before = reads();
    for tick in 0..4 {
        app.tick_permission_walk(t0 + GRANT_POLL * tick);
    }
    assert!(
        !app.permission_walk.active(),
        "the opening might be this Mac's"
    );
    assert_eq!(
        reads(),
        before + 1,
        "the looks under one key read the markers once"
    );
    approve(&mut app, 2);
    app.tick_permission_walk(t0 + GRANT_POLL * 4);
    assert!(reads() > before + 1, "a new key reads them again");
    assert!(
        app.permission_walk.active(),
        "under a key that names this Mac, the opening is another Mac's"
    );
}

thread_local! {
    static STATE: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
    /// The Mac the tests' aterm runs on, by its hardware UUID.
    static THIS_MAC: Cell<&'static str> = const { Cell::new(MAC_A) };
}

/// The Mac a released aterm wrote [`STATE`] on, and another.
const MAC_A: &str = "1D683F62-5C4B-4E3A-9F21-7B8C6D5E4A30";
const MAC_B: &str = "8F2E6A41-0B3C-4D5E-A697-C8D9E0F1A2B3";

/// `key` with this bundle's and [`THIS_MAC`]'s words after it, as the key carries them once both
/// are read (the tests' key carries neither).
fn on_this_mac(key: &str) -> String {
    format!("{key}@com.aterm.aterm@{}", THIS_MAC.with(Cell::get))
}

/// A page marker read from [`STATE`], a real `<state>` folder.
fn from_state(
    read: impl FnOnce(&std::path::Path) -> Option<(String, u64)>,
) -> Option<(String, u64)> {
    STATE.with(|state| state.borrow().as_deref().and_then(read))
}

/// A Mac set up from another Mac's `<state>` (Migration Assistant, a network home) where a
/// released aterm walked both pages: `<state>` holds exactly what v0.145.0's window wrote there
/// (its Full Disk Access line, which names that Mac, and its Developer Tools marker's plain word,
/// with no file beside either). The plain word names no Mac, and the Full Disk Access marker
/// names another, so the walk starts by itself on this Mac over both pages. NEGATIVE CONTROL:
/// the Mac that wrote them gets no walk, and neither does this Mac when no Full Disk Access
/// opening tells whose the plain word is (the page came up there with no Full Disk Access page).
#[test]
fn a_mac_set_up_from_a_released_aterms_state_gets_its_first_walk() {
    use atpkg::devtools::{DEVELOPER_TOOLS, FULL_DISK_ACCESS};
    fn fda_opened_as(key: &str) -> Option<(String, u64)> {
        from_state(|state| FULL_DISK_ACCESS.opened_as_in(state, &on_this_mac(key)))
    }
    fn fda_last_opened() -> Option<(String, u64)> {
        from_state(|state| FULL_DISK_ACCESS.last_opened_in(state))
    }
    fn devtools_opened_as(key: &str) -> Option<(String, u64)> {
        from_state(|state| DEVELOPER_TOOLS.opened_as_in(state, &on_this_mac(key)))
    }
    fn devtools_last_opened() -> Option<(String, u64)> {
        from_state(|state| DEVELOPER_TOOLS.last_opened_in(state))
    }
    fn cold(mac: &'static str) -> App {
        THIS_MAC.with(|this| this.set(mac));
        let mut fda = test_arms::fda_opening();
        fda.opened_as = fda_opened_as;
        fda.last_opened = fda_last_opened;
        let mut devtools = test_arms::marked_as(devtools_opened_as);
        devtools.last_opened = devtools_last_opened;
        let mut app = walk_app_with_fda(devtools, fda);
        app.devtools_helper.note_cold_start();
        app.user_input_recent = |_| true;
        app
    }
    let dir = aterm_tempfile::tempdir().expect("a scratch <state>");
    let state = dir.path();
    // v0.145.0's `fda_page_brought_up` (`fda_opened_key`) and `mark_devtools_page_up`
    // (`revision_key`) under revision 1 on Mac A: one line in each marker.
    std::fs::write(
        state.join("fda-pane-opened"),
        format!("r1@com.aterm.aterm@{MAC_A} 1793163273\n"),
    )
    .expect("v0.145.0's Full Disk Access marker");
    std::fs::write(state.join("devtools-pane-opened"), "r1 1793163534\n")
        .expect("v0.145.0's Developer Tools marker");
    STATE.with(|cell| *cell.borrow_mut() = Some(state.to_path_buf()));
    let t0 = Instant::now();

    let mut moved = cold(MAC_B);
    moved.tick_permission_walk(t0);
    assert_eq!(
        moved
            .permission_walk
            .walk()
            .map(|walk| walk.subjects().to_vec()),
        Some(vec![Subject::FullDiskAccess, Subject::DeveloperTools]),
        "Mac A's pages are not Mac B's"
    );

    let mut home = cold(MAC_A);
    home.tick_permission_walk(t0);
    assert!(!home.permission_walk.active(), "Mac A's own pages came up");

    std::fs::remove_file(state.join("fda-pane-opened")).expect("no Full Disk Access page");
    let mut unknown = cold(MAC_B);
    unknown.tick_permission_walk(t0);
    assert!(
        !unknown.permission_walk.active(),
        "nothing says whose the plain word is, so it may be this Mac's"
    );
    STATE.with(|cell| *cell.borrow_mut() = None);
}

/// The pages of a walk with System State's debugger step still to come.
const LEFT: [Subject; 3] = [
    Subject::FullDiskAccess,
    Subject::DeveloperTools,
    Subject::Debugger,
];

/// [`walk_app`]'s window or [`walk_app_with_fda`]'s, with System State's once-per-Mac debugger
/// step queued and waiting for its moment, ten minutes out.
fn with_debugger_step(mut app: App) -> App {
    use crate::debugger_access::DebuggerStepQueue;
    app.debugger_launch = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
    app.debugger_launch
        .look_later(Instant::now() + Duration::from_secs(600));
    app
}

/// A walk under way when System Settings' Quit & Reopen ends the process leaves its pages, from
/// the one it is at on, in `<state>`'s reopen marker for the aterm that opens again: at Full
/// Disk Access's done words, which wait under macOS's sheet
/// (`docs/measured/first-launch-permission-walk-0.145.0-2026-10-28.md`, finding 1), all three;
/// once it turned to Developer Tools, that page and the debugger's. NEGATIVE CONTROL: a person's
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
    let mut app = with_debugger_step(walk_app());
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
        Some((Page::FullDiskAccess, Some((1, 3)))),
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
        Some((Page::DeveloperTools, Some((2, 3))))
    );
    app.leave_walk_at_quit(true);
    assert_eq!(
        taken(),
        Some(vec![Subject::DeveloperTools, Subject::Debugger]),
        "Quit & Reopen at Developer Tools"
    );

    let idle = walk_app();
    idle.leave_walk_at_quit(true);
    assert!(!marker.exists(), "no walk under way");
    STATE.with(|cell| *cell.borrow_mut() = None);
}

/// The aterm System Settings' Quit & Reopen opens, on [`STATE`]: a cold start with the person at
/// System State, Full Disk Access reading on, Developer Tools not, the debugger's step to come, and
/// the access record holding the Developer Tools page handed this revision. `<state>` holds the
/// Full Disk Access opening of the walk in the process that quit (the tests' key names no Mac),
/// so the look starts no first walk by itself (`App::fresh_mac`).
fn reopened_aterm() -> App {
    use atpkg::devtools::FULL_DISK_ACCESS;
    fn fda_opened_as(key: &str) -> Option<(String, u64)> {
        from_state(|state| FULL_DISK_ACCESS.opened_as_in(state, key))
    }
    fn fda_last_opened() -> Option<(String, u64)> {
        from_state(|state| FULL_DISK_ACCESS.last_opened_in(state))
    }
    let state = state().expect("a <state> named");
    std::fs::write(
        state.join("fda-pane-opened"),
        format!(
            "{} {}\n",
            atpkg::devtools::revision_key(1),
            aterm_time::unix_secs().expect("the clock")
        ),
    )
    .expect("the Full Disk Access marker");
    let mut fda = test_arms::fda_opening();
    fda.opened_as = fda_opened_as;
    fda.last_opened = fda_last_opened;
    let mut app = with_debugger_step(walk_app_with_fda(test_arms::opening(), fda));
    app.access_steps.set_reach(StepsReach {
        handed: |_, access| access == Access::DeveloperTools,
        ..app.access_steps.reach
    });
    app.devtools_helper.note_cold_start();
    app.user_input_recent = |_| true;
    grant_fda(&mut app);
    app
}

/// The pages the walk under way has still to show.
fn walk_ahead(app: &App) -> Option<Vec<Subject>> {
    app.permission_walk.walk().map(|walk| walk.ahead().to_vec())
}

/// The aterm System Settings' Quit & Reopen opens is a cold start whose Full Disk Access page
/// came up before, so its look starts no first walk (`App::fresh_mac`). With the pages the walk
/// in the process that quit left in `<state>`'s reopen marker, this bundle's on this Mac and
/// within [`REOPEN_WITHIN`], its first look takes the marker and walks on: Full Disk Access,
/// which reads held now, is left out, and Developer Tools' page opens at once, "Step 1 of 2"
/// before the debugger's step, though the access record holds that page handed this revision
/// (the walk's page is the owner's press). The marker is gone. A marker a dev bundle beside the
/// release left, or another Mac on the same network home, starts nothing here and stays in
/// place: the aterm on that Mac takes it and walks on. NEGATIVE CONTROL: with no marker, or one
/// older than [`REOPEN_WITHIN`], the same aterm starts nothing, and the marker is gone.
#[test]
fn the_aterm_quit_and_reopen_opens_walks_on_though_its_pages_came_up_before() {
    let dir = aterm_tempfile::tempdir().expect("a scratch <state>");
    let state = dir.path();
    STATE.with(|cell| *cell.borrow_mut() = Some(state.to_path_buf()));
    let marker = state.join(REOPEN_MARKER);
    let bundle = std::path::Path::new(BUNDLE);
    let now = aterm_time::unix_secs().expect("the clock");
    let t0 = Instant::now();

    leave_reopen_in(state, now, &LEFT, bundle, MAC_A).expect("the reopen marker");
    let mut app = reopened_aterm();
    assert!(app.devtools_shown_this_revision(), "handed before the quit");
    let opens = test_arms::opens();
    app.tick_permission_walk(t0);
    assert_eq!(
        walk_ahead(&app),
        Some(vec![Subject::DeveloperTools, Subject::Debugger]),
        "the walk goes on"
    );
    assert_eq!(test_arms::opens(), opens + 1, "Developer Tools' page opens");
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((1, 2))))
    );
    assert!(!marker.exists(), "the marker is taken");

    let dev = std::path::Path::new("/Applications/aterm (dev).app");
    for (left_by, on, why) in [
        (dev, MAC_A, "a dev bundle's"),
        (bundle, MAC_B, "another Mac's"),
    ] {
        leave_reopen_in(state, now, &LEFT, left_by, on).expect("the reopen marker");
        let mut app = reopened_aterm();
        let opens = test_arms::opens();
        app.tick_permission_walk(t0);
        assert!(!app.permission_walk.active(), "{why}: no walk");
        assert_eq!(test_arms::opens(), opens, "{why}: no page");
        assert!(marker.exists(), "{why}: left for the aterm it names");
    }
    THIS_MAC.with(|this| this.set(MAC_B));
    let mut there = reopened_aterm();
    there.tick_permission_walk(t0);
    assert_eq!(
        walk_ahead(&there),
        Some(vec![Subject::DeveloperTools, Subject::Debugger]),
        "the aterm on the Mac that quit walks on"
    );
    assert!(!marker.exists(), "and takes the marker");
    THIS_MAC.with(|this| this.set(MAC_A));

    let old = now - REOPEN_WITHIN.as_secs() - 1;
    for (left, why) in [(None, "no marker"), (Some(old), "an older one")] {
        if let Some(at) = left {
            leave_reopen_in(state, at, &LEFT, bundle, MAC_A).expect("the reopen marker");
        }
        let mut app = reopened_aterm();
        let opens = test_arms::opens();
        app.tick_permission_walk(t0);
        assert!(!app.permission_walk.active(), "{why}: no walk");
        assert_eq!(test_arms::opens(), opens, "{why}: no page");
        assert!(!marker.exists(), "{why}: no marker left");
    }
    STATE.with(|cell| *cell.borrow_mut() = None);
}

/// Pages a Quit & Reopen left wait in memory for the start's rules: the first look takes the
/// marker while System State is away and starts nothing, a later look still starts nothing while
/// the person types, and the first look with the keyboard at rest walks on. A walk that starts
/// first (a person's press on a row here; the control verb's starts the same way) ends that
/// wait: once it is over, no look walks on from the marker's pages.
#[test]
fn pages_quit_and_reopen_left_wait_for_the_start_and_give_way_to_a_walk_started_first() {
    let dir = aterm_tempfile::tempdir().expect("a scratch <state>");
    let state = dir.path();
    STATE.with(|cell| *cell.borrow_mut() = Some(state.to_path_buf()));
    let marker = state.join(REOPEN_MARKER);
    let bundle = std::path::Path::new(BUNDLE);
    let now = aterm_time::unix_secs().expect("the clock");
    let t0 = Instant::now();

    for press_first in [false, true] {
        leave_reopen_in(state, now, &LEFT, bundle, MAC_A).expect("the reopen marker");
        let mut app = reopened_aterm();
        set_front(false);
        app.tick_permission_walk(t0);
        set_front(true);
        assert!(!app.permission_walk.active(), "System State away");
        assert!(!marker.exists(), "taken at the first look");
        if press_first {
            assert_eq!(
                app.start_permission_walk(&[Subject::DeveloperTools], "a person's press", t0),
                walking_over(&[Subject::DeveloperTools, Subject::Debugger])
            );
            // The press's walk goes past Developer Tools to the debugger's step, which waits up
            // to UNREAD_WAIT for its rule's first reading, and is over.
            let opens = test_arms::opens();
            app.devtools_helper.press_for_test(PanelEvent::SwitchedOn);
            app.tick_devtools_hand_over(t0 + GRANT_POLL);
            let over = t0 + GRANT_POLL + UNREAD_WAIT;
            app.drive_permission_walk(over);
            assert!(!app.permission_walk.active());
            assert!(app.devtools_helper.panel().is_none(), "its panel is gone");
            app.tick_permission_walk(over + GRANT_POLL);
            assert_eq!(walk_ahead(&app), None, "the press came first");
            assert_eq!(test_arms::opens(), opens, "no page from the marker");
            continue;
        }
        app.last_keystroke_at = Some(t0);
        app.tick_permission_walk(t0 + GRANT_POLL);
        assert_eq!(walk_ahead(&app), None, "a key typed just now");
        app.tick_permission_walk(t0 + START_KEYS_QUIET);
        assert_eq!(
            walk_ahead(&app),
            Some(vec![Subject::DeveloperTools, Subject::Debugger]),
            "the keyboard at rest: the walk goes on"
        );
    }
    STATE.with(|cell| *cell.borrow_mut() = None);
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
        walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
    );

    let mut queued = walk_app();
    queued.debugger_launch = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
    assert_eq!(
        queued.start_permission_walk(&[Subject::Debugger], "a test", t0),
        walking_over(&[
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

/// A first launch's walk that gave way to a person's press elsewhere is not spent: once the
/// press's own gesture is over (here the Local Network pane's bounded hold) and a quiet moment
/// comes with System State in front, the walk starts again by itself over the pages it never got
/// past, its Full Disk Access page first, which comes up again. A press is no answer to a page
/// the person never saw.
/// NEGATIVE CONTROL: a give-way that spends the walk (every subject still read walked, as when it
/// only ended the walk) never starts it again: Full Disk Access and Developer Tools then wait
/// for a press on their rows.
#[test]
fn a_walk_that_gave_way_starts_again_at_the_next_quiet_moment() {
    let t0 = Instant::now();
    let mut app = walk_app();
    app.devtools_helper.note_cold_start();
    app.user_input_recent = |_| true;
    app.tick_permission_walk(t0);
    let walking = || Some(vec![Subject::FullDiskAccess, Subject::DeveloperTools]);
    assert_eq!(
        app.permission_walk
            .walk()
            .map(|walk| walk.subjects().to_vec()),
        walking()
    );
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2))))
    );
    let fda_opens = test_arms::fda_counts().0;

    assert!(app.open_access_from_system_state(Access::LocalNetwork, true));
    assert!(!app.permission_walk.active(), "it stepped aside");
    app.tick_permission_walk(t0 + Duration::from_secs(1));
    assert!(
        !app.permission_walk.active(),
        "not over the pane's own hold"
    );

    let quiet = Instant::now() + crate::system_state_access::PANE_HOLD + Duration::from_secs(1);
    app.tick_permission_walk(quiet);
    assert_eq!(
        app.permission_walk
            .walk()
            .map(|walk| walk.subjects().to_vec()),
        walking(),
        "the pages it never got past"
    );
    assert_eq!(test_arms::fda_counts().0, fda_opens + 1, "its page again");
    assert_eq!(
        page_and_step(&app),
        Some((Page::FullDiskAccess, Some((1, 2))))
    );
}
