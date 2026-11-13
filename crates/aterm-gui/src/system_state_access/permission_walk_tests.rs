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
    AutomationAsked, GRANT_POLL, KeptPage, START_KEYS_QUIET, START_PRESENT_WITHIN, Started,
    Subject, UNREAD_WAIT, WALK_AGAIN, Walk, WalkLane, WalkReach, automation_asked_in,
    keep_automation_ask_in, keep_pages_in, left_pages_in,
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
    /// Whether the person quit System Settings, as the panel's arm reads it running.
    static SETTINGS_QUIT: Cell<bool> = const { Cell::new(false) };
    /// The walk record's clock, in unix seconds: it outlives each App a test makes, as the
    /// Mac's does.
    static CLOCK: Cell<u64> = const { Cell::new(CLOCK_AT) };
}

/// The unix second [`CLOCK`] starts at.
const CLOCK_AT: u64 = 1_800_000_000;

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
/// main one while [`SETTINGS_ASKS`] says so, runs until [`SETTINGS_QUIT`] says it quit, and
/// reads a quit asked of this process while [`QUIT_ASKED`] does, as the walk's reach does.
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
    arm.settings_running = || !SETTINGS_QUIT.with(Cell::get);
    arm
}

/// The walk's reach: Accessibility's grant as the test sets it, an Automation ask that is
/// counted and stays out until the test answers it, the Accessibility page's Not now kept in
/// [`AX_DECLINED`] (held for its span, its reads counted), a quit asked while [`QUIT_ASKED`]
/// says so, and the walk record kept in and read from the real `<state>` folder [`STATE`]
/// names, as the live arms keep it, for [`BUNDLE`] on [`THIS_MAC`] (none while no folder is
/// named), by [`CLOCK`].
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
        keep_pages: |revision, pages| {
            if let Some(state) = state() {
                keep_pages_in(
                    &state,
                    std::path::Path::new(BUNDLE),
                    THIS_MAC.with(Cell::get),
                    revision,
                    pages,
                )
                .expect("the walk record");
            }
        },
        left_pages: |revision| {
            state().map_or_else(Vec::new, |state| {
                left_pages_in(
                    &state,
                    std::path::Path::new(BUNDLE),
                    THIS_MAC.with(Cell::get),
                    revision,
                )
            })
        },
        keep_automation_ask: |revision, asked| {
            if let Some(state) = state() {
                keep_automation_ask_in(
                    &state,
                    std::path::Path::new(BUNDLE),
                    THIS_MAC.with(Cell::get),
                    revision,
                    asked,
                )
                .expect("the walk record");
            }
        },
        automation_asked: |revision| {
            automation_asked_in(
                &state()?,
                std::path::Path::new(BUNDLE),
                THIS_MAC.with(Cell::get),
                revision,
            )
        },
        unix_now: || Some(CLOCK.with(Cell::get)),
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
    SETTINGS_QUIT.with(|quit| quit.set(false));
    RECORDS.with(|records| records.borrow_mut().clear());
    let mut app = new_mac(devtools, fda);
    // The person is looking at aterm: a walk that starts by itself, and macOS's Automation
    // question, come up only over an aterm window holding keyboard focus (`StartFacts::in_front`).
    focus(&mut app, true);
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
                AccessOutcome::Handed
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
        app.access_revision(),
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
/// slot), `aterm doctor`'s `permissions walk now fda` answers
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
/// waiting with it: once the setup worker's prompt that queued both ends, the walk opens no page
/// and holds no guard while the press waits or its tab is on its way, so the tab takes the slot;
/// the installer then holds it, and the walk's page comes up the turn after the installer ended.
/// NEGATIVE CONTROL: with no press waiting, the walk's page comes up as the prompt ends
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
    let hold_ends = t0 + GRANT_POLL;
    let worker = MacosDialog::when_none_up(Duration::ZERO).expect("the worker takes the slot");
    // The person pressed Install… under the worker's prompt: the press waits for the slot.
    app.workstation_setup.developer_tools = Install::PressedQueued { wanted: None };
    let (fda_opens, shows) = (test_arms::fda_counts().0, panel_counts().0);
    assert_eq!(
        app.ask_permission_walk(&[Subject::FullDiskAccess], t0),
        walking_over(&[Subject::FullDiskAccess, Subject::DeveloperTools])
    );
    app.tick_consent_lanes(t0);
    assert_eq!(
        test_arms::fda_counts().0,
        fda_opens,
        "no page beside the prompt"
    );
    drop(worker);
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
/// theirs: the page opens at once, as their own walk's would, though the setup worker's prompt
/// that the verb's walk waits out is still up. NEGATIVE CONTROL: with no press the page waits
/// for the prompt to end
/// (`the_control_verbs_walk_waits_out_another_macos_question_and_answers_at_once`).
#[test]
fn a_persons_press_on_the_row_a_verbs_walk_waits_at_opens_its_page_now() {
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};
    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let t0 = Instant::now();
    let mut app = walk_app();
    app.access_steps.set_reach(StepsReach {
        dialog_up: macos_dialog_up,
        holds_dialog: true,
        ..app.access_steps.reach
    });
    let worker = MacosDialog::when_none_up(Duration::ZERO).expect("the worker takes the slot");
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
    drop(worker);
    drop(app);
    assert!(!macos_dialog_up());
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

/// Point [`STATE`] at a scratch `<state>`: the walk record lives there, as the live arms keep
/// it, and outlives each App a test makes, as a relaunch's `<state>` does.
fn scratch_state() -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::tempdir().expect("a scratch <state>");
    STATE.with(|cell| *cell.borrow_mut() = Some(dir.path().to_path_buf()));
    dir
}

/// The pages the walk record in `state` keeps for the tests' aterm under revision 1, with the
/// walks in a row that left each unanswered.
fn kept_tries(state: &std::path::Path) -> Vec<(Subject, u32)> {
    left_pages_in(state, std::path::Path::new(BUNDLE), MAC_A, 1)
        .into_iter()
        .map(|page| (page.subject, page.tries))
        .collect()
}

/// [`kept_tries`]' pages alone.
fn kept_pages(state: &std::path::Path) -> Vec<Subject> {
    kept_tries(state)
        .into_iter()
        .map(|(subject, _)| subject)
        .collect()
}

/// Keep `pages`, none left unanswered, in `state`'s walk record for the tests' aterm under
/// revision 1.
fn keep_fresh(state: &std::path::Path, pages: &[Subject]) {
    let pages: Vec<KeptPage> = pages.iter().copied().map(KeptPage::new).collect();
    keep_pages_in(state, std::path::Path::new(BUNDLE), MAC_A, 1, &pages).expect("a line");
}

/// The subjects of the walk under way.
fn walking(app: &App) -> Option<Vec<Subject>> {
    app.permission_walk
        .walk()
        .map(|walk| walk.subjects().to_vec())
}

/// A later process on a Mac whose Full Disk Access page came up before (`App::fresh_mac` reads
/// it), started cold or as an update's successor.
fn later_process(cold: bool) -> App {
    let mut app = walk_app_with_fda(
        test_arms::opening(),
        test_arms::fda_marking(|key| Some((key.to_owned(), 1)), |_| {}),
    );
    if cold {
        app.devtools_helper.note_cold_start();
    }
    app
}

/// The walk record follows the walk under way from page to page, so whatever ends the process
/// leaves the pages it has still to show, the one it is at first: both at Full Disk Access's
/// done words, which wait under macOS's Quit & Reopen sheet
/// (`docs/measured/first-launch-permission-walk-0.145.0-2026-10-28.md`, finding 1), Developer
/// Tools alone once the panel turned to it; the close box, the person's answer, takes its line
/// away. NEGATIVE CONTROL: a walk while no revision stands keeps nothing, and leaves the line
/// an earlier walk kept.
#[test]
fn the_walk_record_follows_the_walk_from_page_to_page() {
    let dir = scratch_state();
    let t0 = Instant::now();
    let mut app = walk_app();
    assert_eq!(
        app.start_permission_walk(&[], "first launch", t0),
        walking_over(&LEFT)
    );
    assert_eq!(kept_pages(dir.path()), LEFT, "as it starts");
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
    assert_eq!(kept_pages(dir.path()), LEFT, "at the sheet");

    SETTINGS_ASKS.with(|asks| asks.set(false));
    app.tick_consent_lanes(granted + DONE_FOR + GRANT_POLL);
    assert_eq!(
        page_and_step(&app),
        Some((Page::DeveloperTools, Some((2, 2))))
    );
    assert_eq!(
        kept_pages(dir.path()),
        [Subject::DeveloperTools],
        "at Developer Tools"
    );
    app.devtools_helper.press_for_test(PanelEvent::CloseBox);
    app.tick_consent_lanes(granted + DONE_FOR + GRANT_POLL * 2);
    assert!(!app.permission_walk.active(), "the close box ends it");
    assert!(kept_pages(dir.path()).is_empty(), "and its line goes");

    keep_fresh(dir.path(), &LEFT);
    let mut unapproved = walk_app();
    unapproved.observe_initial_setup_permissions(None);
    assert_eq!(unapproved.access_revision(), None);
    assert_eq!(
        unapproved.start_permission_walk(&[], "a test", t0),
        walking_over(&LEFT)
    );
    unapproved
        .devtools_helper
        .press_for_test(PanelEvent::CloseBox);
    unapproved.tick_consent_lanes(t0 + GRANT_POLL);
    assert!(!unapproved.permission_walk.active());
    assert_eq!(kept_pages(dir.path()), LEFT, "no revision: nothing kept");
    STATE.with(|cell| *cell.borrow_mut() = None);
}

/// An update's Commit ends the predecessor mid-walk with nothing of the quit path run
/// (`seamless::commit_and_exit`, `clean_exit_now`): its first walk, which started by itself,
/// kept the pages it had still to show as it started. The successor, which the person did not
/// start, brings none of them up by itself (owner ruling 2026-11-09): the line stays for the
/// next launch of theirs, which, on a Mac whose Full Disk Access page came up before
/// (`App::fresh_mac` false), walks on from them, its Full Disk Access page step 1 of 2 again. A
/// crash, a logout or a person's quit and the cold launch after it do the same. NEGATIVE
/// CONTROLS: a predecessor whose walk ended by the close box, or that walked nothing, leaves
/// the next launch nothing to walk; one that reads each kept page's grant on starts none, and
/// the line goes.
#[test]
fn an_update_commit_mid_walk_leaves_its_pages_for_the_persons_next_launch() {
    fn predecessor(t0: Instant) -> App {
        let mut app = walk_app();
        app.devtools_helper.note_cold_start();
        app.tick_permission_walk(t0);
        assert_eq!(
            walking(&app),
            Some(LEFT.to_vec()),
            "the first walk starts by itself"
        );
        assert_eq!(
            page_and_step(&app),
            Some((Page::FullDiskAccess, Some((1, 2))))
        );
        app
    }
    let dir = scratch_state();
    let t0 = Instant::now();
    let t1 = t0 + START_KEYS_QUIET;
    for cold in [false, true] {
        drop(predecessor(t0));
        assert_eq!(kept_pages(dir.path()), LEFT, "kept as it started");
        let fda_opens = test_arms::fda_counts().0;
        if !cold {
            let mut successor = later_process(false);
            successor.tick_permission_walk(t1);
            assert!(
                !successor.permission_walk.active(),
                "an update's successor walks on from nothing by itself"
            );
            assert_eq!(test_arms::fda_counts().0, fda_opens, "no page comes up");
            assert_eq!(
                kept_pages(dir.path()),
                LEFT,
                "the line stays for the person"
            );
            continue;
        }
        let mut app = later_process(cold);
        app.tick_permission_walk(t1);
        assert_eq!(walking(&app), Some(LEFT.to_vec()), "cold {cold}: walks on");
        assert_eq!(
            test_arms::fda_counts().0,
            fda_opens + 1,
            "its page comes up again"
        );
        assert_eq!(
            page_and_step(&app),
            Some((Page::FullDiskAccess, Some((1, 2))))
        );
        app.devtools_helper.press_for_test(PanelEvent::CloseBox);
        app.tick_consent_lanes(t1 + GRANT_POLL);
        assert!(!app.permission_walk.active(), "the close box ends it");
        assert!(kept_pages(dir.path()).is_empty(), "the line goes");
        let mut after = later_process(cold);
        after.tick_permission_walk(t1);
        assert!(
            !after.permission_walk.active(),
            "cold {cold}: nothing to walk on from after the close box"
        );
    }

    let mut idle = walk_app();
    idle.tick_permission_walk(t0);
    assert!(!idle.permission_walk.active(), "a lane that starts none");
    drop(idle);
    let mut app = later_process(true);
    app.tick_permission_walk(t1);
    assert!(!app.permission_walk.active(), "nothing was walked");

    drop(predecessor(t0));
    let mut app = later_process(true);
    grant_fda(&mut app);
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::Granted, test_arms::on()),
        t1,
    );
    app.tick_permission_walk(t1);
    assert!(!app.permission_walk.active(), "every kept page reads held");
    assert!(kept_pages(dir.path()).is_empty(), "so the line goes");
    STATE.with(|cell| *cell.borrow_mut() = None);
}

/// An install of Apple's tools under way outside this launch's own tab (Apple's installer
/// window, aterm's root half, another installer; or an earlier aterm's tab adopted) holds the
/// walk that would start by itself: Full Disk Access switched on brings macOS's Quit & Reopen,
/// whose quit hangs an adopted tab up mid-install, so its next try would ask for the password
/// again, and System Settings in front takes the keyboard from the installer (the review found
/// the first walk opened its Full Disk Access page beside it). NEGATIVE CONTROL: once the window
/// reads no installer up, the first walk starts, its Full Disk Access page first.
#[test]
fn no_walk_starts_by_itself_while_an_install_of_the_tools_runs() {
    use crate::clt_install::Reading;
    let t0 = Instant::now();
    for (n, up) in [
        Reading {
            apple_window: true,
            ..Reading::default()
        },
        Reading {
            placeholder: true,
            install_process: true,
            ..Reading::default()
        },
        Reading {
            placeholder: true,
            live_run: true,
            ..Reading::default()
        },
    ]
    .into_iter()
    .enumerate()
    {
        let _dir = scratch_state();
        let mut app = walk_app();
        app.devtools_helper.note_cold_start();
        app.workstation_setup.developer_tools_seen = Some(up);
        let fda_opens = test_arms::fda_counts().0;
        app.tick_permission_walk(t0);
        assert!(
            !app.permission_walk.active(),
            "{n}: no walk beside the install"
        );
        assert_eq!(
            test_arms::fda_counts().0,
            fda_opens,
            "{n}: no Full Disk Access page beside it"
        );
        app.workstation_setup.developer_tools_seen = Some(Reading::default());
        app.tick_permission_walk(t0 + START_KEYS_QUIET);
        assert_eq!(
            walking(&app),
            Some(LEFT.to_vec()),
            "{n}: its end lets the walk go"
        );
        assert_eq!(test_arms::fda_counts().0, fda_opens + 1, "{n}");
        STATE.with(|cell| *cell.borrow_mut() = None);
    }
}

/// While a quit is asked of this process (its barriers still running) the walk page's ten
/// minutes stand still, as while the person is away: a going process lapses no page, so the
/// walk record keeps it for the process after this one. NEGATIVE CONTROL: with no quit asked
/// the same twenty minutes lapse the page: the record keeps the page after it, for the next walk
/// to take up, and the page that lapsed leaves it, the person's to give from System State.
#[test]
fn a_quit_asked_of_this_process_stands_the_walks_page_time() {
    for quit in [true, false] {
        let dir = scratch_state();
        let t0 = Instant::now();
        let mut app = walk_app();
        assert_eq!(
            app.start_permission_walk(&[], "a test", t0),
            walking_over(&LEFT)
        );
        QUIT_ASKED.with(|asked| asked.set(quit));
        let step = Duration::from_secs(30);
        let mut now = t0 + step;
        while now < t0 + PANEL_FOR * 2 {
            app.tick_consent_lanes(now);
            now += step;
        }
        if quit {
            assert!(
                app.permission_walk.active(),
                "a going process lapses no page"
            );
            assert_eq!(
                page_and_step(&app),
                Some((Page::FullDiskAccess, Some((1, 2))))
            );
            assert_eq!(kept_pages(dir.path()), LEFT);
        } else {
            assert!(!app.permission_walk.active(), "the page lapsed");
            assert_eq!(
                kept_tries(dir.path()),
                [(Subject::DeveloperTools, 0)],
                "the page after it stays kept as it was, the one the person left goes"
            );
        }
        QUIT_ASKED.with(|asked| asked.set(false));
        STATE.with(|cell| *cell.borrow_mut() = None);
    }
}

/// The person quits System Settings under the walk's page: once its grace is past the panel reads
/// it gone (`Closed::SettingsGone`) and the walk hears it, by `shown` plus a minute at most.
fn quit_settings(app: &mut App, shown: Instant) {
    SETTINGS_QUIT.with(|quit| quit.set(true));
    let mut now = shown;
    while app.permission_walk.active() && now < shown + Duration::from_secs(60) {
        now += crate::devtools_helper::PANEL_TICK;
        app.tick_consent_lanes(now);
    }
    SETTINGS_QUIT.with(|quit| quit.set(false));
}

/// A day, in seconds: [`WALK_AGAIN`]'s first pause.
const DAY: u64 = 86_400;

/// System Settings quit under the walk's page is the person leaving it: the walk ends, as by the
/// close box, its pages leave the walk record, and no later process brings them up by itself,
/// whatever time passes (the review found such pages came back a day on, then every two, four,
/// up to thirty days, for good: a nag). They stay the person's to give from System State.
/// NEGATIVE CONTROL: a walk that stops because aterm could not show its page (its panel gone)
/// keeps its pages, each counting that walk, so they come back by themselves on
/// [`WALK_AGAIN`]'s schedule.
#[test]
fn a_walk_the_person_left_by_quitting_system_settings_never_comes_back_by_itself() {
    use Subject::{DeveloperTools, FullDiskAccess};
    assert_eq!(WALK_AGAIN.after(1), Duration::from_secs(DAY));
    let dir = scratch_state();
    CLOCK.with(|clock| clock.set(CLOCK_AT));
    let t0 = Instant::now();
    let mut first = walk_app();
    first.devtools_helper.note_cold_start();
    first.tick_permission_walk(t0);
    assert_eq!(walking(&first), Some(LEFT.to_vec()), "the first walk");
    quit_settings(&mut first, t0);
    assert!(!first.permission_walk.active(), "it ends");
    assert!(kept_pages(dir.path()).is_empty(), "its line goes");
    first.tick_permission_walk(t0 + Duration::from_secs(120));
    assert!(!first.permission_walk.active(), "not again in its process");
    drop(first);
    for days in [1, 3, 400] {
        CLOCK.with(|clock| clock.set(CLOCK_AT + days * DAY));
        let mut later = later_process(true);
        later.tick_permission_walk(t0);
        assert!(
            !later.permission_walk.active(),
            "{days} days on, nothing comes back by itself"
        );
    }

    CLOCK.with(|clock| clock.set(CLOCK_AT));
    let mut failed = walk_app();
    failed.devtools_helper.note_cold_start();
    failed.tick_permission_walk(t0);
    assert_eq!(walking(&failed), Some(LEFT.to_vec()));
    failed.permission_walk.stop("its panel is gone");
    assert_eq!(
        kept_tries(dir.path()),
        [(FullDiskAccess, 1), (DeveloperTools, 1)],
        "aterm could not show it: kept for its time"
    );
    drop(failed);
    CLOCK.with(|clock| clock.set(CLOCK_AT + DAY));
    let mut due = later_process(true);
    due.tick_permission_walk(t0);
    assert_eq!(walking(&due), Some(LEFT.to_vec()), "a day on, it walks on");
    STATE.with(|cell| *cell.borrow_mut() = None);
}

/// The setup run's last status, its plan signing out of Messages (`messages`) or out of nothing,
/// that reads Accessibility off and Messages signed in.
fn sign_outs(messages: bool) -> SetupStatus {
    use aterm_types::macos_setup::Subject as Row;
    let mut selection = Selection::none();
    selection.set(Setting::Messages, messages);
    let mut grant = observed(Row::Access(Access::Accessibility), Standing::NeedsYou, None);
    grant.now = Some("Off".to_owned());
    let mut signed_in = observed(Row::Setting(Setting::Messages), Standing::Waiting, None);
    signed_in.now = Some("Signed in".to_owned());
    status(selection, vec![grant, signed_in])
}

/// An Accessibility page the walk record keeps comes back only while the setup run still offers
/// it: before the run's status is in, the look waits, keeping every page (unread is no answer);
/// once the run reads the sign-outs needing it, the walk walks on over it, and a kept page whose
/// grant reads held leaves the record as the walk starts. NEGATIVE CONTROL: a run whose plan
/// signs out of nothing does not want it, so no walk brings it up and it leaves the record.
#[test]
fn a_kept_accessibility_page_waits_for_the_runs_reading_and_leaves_once_unwanted() {
    use Subject::{Accessibility, FullDiskAccess};
    /// A later process whose Full Disk Access and Developer Tools read on.
    fn only_accessibility_missing() -> App {
        let mut app = later_process(true);
        grant_fda(&mut app);
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::Granted, test_arms::on()),
            Instant::now(),
        );
        app
    }
    let dir = scratch_state();
    CLOCK.with(|clock| clock.set(CLOCK_AT));
    let t0 = Instant::now();
    keep_fresh(dir.path(), &[FullDiskAccess, Accessibility]);
    let mut app = only_accessibility_missing();
    app.tick_permission_walk(t0);
    assert!(!app.permission_walk.active(), "the run's reading is not in");
    assert_eq!(
        kept_pages(dir.path()),
        [FullDiskAccess, Accessibility],
        "kept, unread"
    );
    app.publish_system_state(sign_outs(true));
    app.tick_permission_walk(t0 + GRANT_POLL);
    assert_eq!(
        walking(&app),
        Some(vec![Accessibility]),
        "the run offers it: walks on"
    );
    assert_eq!(
        kept_pages(dir.path()),
        [Accessibility],
        "Full Disk Access reads held: it leaves"
    );
    drop(app);

    keep_fresh(dir.path(), &[Accessibility]);
    let mut unwanted = only_accessibility_missing();
    unwanted.publish_system_state(sign_outs(false));
    unwanted.tick_permission_walk(t0);
    assert!(!unwanted.permission_walk.active(), "no sign-out needs it");
    assert!(kept_pages(dir.path()).is_empty(), "so it leaves the record");
    STATE.with(|cell| *cell.borrow_mut() = None);
}

/// A person's press on another row, its grant read on so it starts no walk of its own, gives the
/// walk way: the walk's unanswered pages stay in the walk record with no walk counted against
/// them, for the walk that starts by itself at the next quiet moment. A press is no answer to a
/// page it never saw. NEGATIVE CONTROL: the close box, the person's answer, takes them away.
#[test]
fn a_press_on_another_row_leaves_the_walks_pages_kept() {
    let dir = scratch_state();
    let t0 = Instant::now();
    for answer in [false, true] {
        let mut app = walk_app_with_arm(test_arms::opening(), counting_arm_in_front());
        assert_eq!(
            app.start_permission_walk(&[], "first launch", t0),
            walking_over(&LEFT)
        );
        if answer {
            app.devtools_helper.press_for_test(PanelEvent::CloseBox);
            app.tick_consent_lanes(t0 + GRANT_POLL);
            assert!(kept_pages(dir.path()).is_empty(), "the close box");
            continue;
        }
        AX_TRUSTED.with(|trusted| trusted.set(true));
        let _ = app.open_access_from_system_state(Access::Accessibility, true);
        assert!(!app.permission_walk.active(), "the walk gave way");
        assert_eq!(
            kept_tries(dir.path()),
            [(Subject::FullDiskAccess, 0), (Subject::DeveloperTools, 0)],
            "its pages stay, none counted"
        );
    }
    STATE.with(|cell| *cell.borrow_mut() = None);
}

/// The setup run's reading that the sign-outs need Accessibility (a plan that signs out of
/// Messages, the grant off) starts a walk over Accessibility alone by itself in a later process
/// on a Mac whose first walk is past (`App::fresh_mac` reads a page brought up here before),
/// with no System State tab open: the run's last status says it (`App::system_state_offers`).
/// Once the access record holds its page handed under the revision, no later process brings it
/// up by itself again under it (no walk record is kept here: no `<state>` is named, so the
/// handed line alone decides), and a new revision offers it again. NEGATIVE CONTROLS: a plan
/// that signs out of nothing, or the grant read on, starts no walk.
#[test]
fn accessibility_owed_by_the_run_is_offered_once_in_a_later_process() {
    /// A later process whose Full Disk Access and Developer Tools read on, whose access record
    /// is [`RECORDS`] (kept across Apps, as `<state>` is), and whose setup run published
    /// `sign_outs(messages)`.
    fn signing_out(messages: bool) -> App {
        let records = records();
        let mut app = later_process(true);
        RECORDS.with(|kept| *kept.borrow_mut() = records);
        app.access_steps.set_reach(StepsReach {
            handed: |revision, access| {
                RECORDS.with(|records| {
                    records
                        .borrow()
                        .contains(&(revision, access, AccessOutcome::Handed))
                })
            },
            recorded: |_, _| Some(AccessOutcome::Handed),
            frontmost: front,
            open_pane: crate::system_state_access::live_open_access_pane,
            ..stepping()
        });
        grant_fda(&mut app);
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::Granted, test_arms::on()),
            Instant::now(),
        );
        app.publish_system_state(sign_outs(messages));
        app
    }
    let t0 = Instant::now();
    RECORDS.with(|records| records.borrow_mut().clear());
    let mut app = signing_out(true);
    app.tick_permission_walk(t0);
    assert_eq!(
        walking(&app),
        Some(vec![Subject::Accessibility]),
        "the sign-outs' page, by itself"
    );
    assert_eq!(
        page_and_step(&app).map(|(page, _)| page),
        Some(Page::Accessibility)
    );
    assert!(
        records().contains(&(1, Access::Accessibility, AccessOutcome::Handed)),
        "{:?}",
        records()
    );
    drop(app);

    let mut again = signing_out(true);
    again.tick_permission_walk(t0);
    assert!(
        !again.permission_walk.active(),
        "handed under this revision: no later process brings it up by itself"
    );
    approve(&mut again, 2);
    again.tick_permission_walk(t0 + GRANT_POLL);
    assert_eq!(
        walking(&again),
        Some(vec![Subject::Accessibility]),
        "a new revision offers it again"
    );
    drop(again);

    RECORDS.with(|records| records.borrow_mut().clear());
    let mut nothing = signing_out(false);
    nothing.tick_permission_walk(t0);
    assert!(!nothing.permission_walk.active(), "no sign-out needs it");
    let mut on = signing_out(true);
    AX_TRUSTED.with(|trusted| trusted.set(true));
    on.tick_permission_walk(t0);
    assert!(!on.permission_walk.active(), "the grant reads on");
}

/// A plan that signs out of an account, on a Mac whose Accessibility already reads on, has
/// macOS's Automation question asked by the window at a cold process's first look, with no
/// sign-out row yet (the sign-outs come last) and ahead of any walk's pages
/// (`App::look_for_automation_ask`): early and batched. The walk record keeps the ask, so a
/// relaunch under the same revision asks nothing, and it is asked again only on `WALK_AGAIN`'s
/// pause, and only while a sign-out still reads the answer missing. The question never goes up
/// with nobody at the Mac: screen away, or nothing touched within `START_PRESENT_WITHIN`, asks
/// nothing. Before, the window asked only once a sign-out row waited, late and alone, with no
/// presence check, and its mark lived in the process alone. NEGATIVE CONTROLS: a plan that signs
/// out of nothing asks nothing, and a new revision asks again.
#[test]
fn macos_automation_question_is_asked_early_with_a_person_there_and_kept_in_the_walk_record() {
    use aterm_types::macos_setup::StageRow;
    /// A later process whose Full Disk Access, Developer Tools and Accessibility read on, whose
    /// plan signs out of Messages (`messages`), and whose setup run's rows say `words`, if any.
    fn ready(messages: bool, words: Option<&str>) -> App {
        let mut app = later_process(true);
        grant_fda(&mut app);
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::Granted, test_arms::on()),
            Instant::now(),
        );
        AX_TRUSTED.with(|trusted| trusted.set(true));
        let mut status = sign_outs(messages);
        status.rows = words
            .map(|words| {
                vec![crate::SetupStatusRow {
                    name: "Messages signed out (alice)".into(),
                    status: "waiting".into(),
                    detail: words.into(),
                }]
            })
            .unwrap_or_default();
        app.publish_system_state(status);
        app
    }
    let dir = scratch_state();
    let unanswered = format!(
        "macOS has had no answer yet to whether aterm may control System Events, which the \
         sign-out's clicks go through; {}, and aterm looks again by itself",
        StageRow::AUTOMATION_UNANSWERED
    );
    let t0 = Instant::now();
    let kept = || {
        automation_asked_in(
            dir.path(),
            std::path::Path::new(BUNDLE),
            THIS_MAC.with(Cell::get),
            1,
        )
    };

    // Nobody there: the screen away, or nothing touched within the walk's own bound (the last
    // key longer ago than it, though within a longer one), asks nothing.
    let asks = AUTOMATION_ASKS.with(Cell::get);
    let mut away = ready(true, None);
    away.screen_away = || true;
    away.tick_permission_walk(t0);
    let mut idle = ready(true, None);
    idle.user_input_recent = |within| within > START_PRESENT_WITHIN;
    idle.user_input_readable = || true;
    idle.tick_permission_walk(t0);
    assert_eq!(
        AUTOMATION_ASKS.with(Cell::get),
        asks,
        "behind a lock, or with nobody touching the Mac, nothing is asked"
    );
    assert_eq!(kept(), None, "and nothing is kept");
    drop((away, idle));

    // An install of Apple's tools under way outside this launch's tab (Apple's installer window,
    // aterm's root half run elsewhere) holds the question as it holds a walk that would start by
    // itself: macOS's alert in front would take the keyboard from that install's prompt or
    // window. With none up, the next look asks (below).
    let mut installing = ready(true, None);
    installing.workstation_setup.developer_tools_seen = Some(crate::clt_install::Reading {
        apple_window: true,
        ..crate::clt_install::Reading::default()
    });
    installing.tick_permission_walk(t0);
    assert_eq!(
        AUTOMATION_ASKS.with(Cell::get),
        asks,
        "nothing asked beside an install of the tools"
    );
    assert_eq!(kept(), None, "and nothing kept");
    drop(installing);

    // A person there, no sign-out row yet: the first look asks, and the record keeps it.
    let mut app = ready(true, None);
    app.tick_permission_walk(t0);
    assert!(
        !app.permission_walk.active(),
        "no walk: every grant reads on"
    );
    assert_eq!(
        AUTOMATION_ASKS.with(Cell::get),
        asks + 1,
        "the window asks first"
    );
    let first = AutomationAsked {
        tries: 1,
        at: CLOCK_AT,
    };
    assert_eq!(kept(), Some(first), "the walk record keeps the ask");
    AUTOMATION_OUT.with(|out| out.set(false));
    app.tick_permission_walk(t0 + GRANT_POLL);
    drop(app);

    // A relaunch under the same revision, the sign-out still unanswered: nothing before the pause.
    let mut relaunch = ready(true, Some(&unanswered));
    relaunch.tick_permission_walk(t0);
    assert_eq!(
        AUTOMATION_ASKS.with(Cell::get),
        asks + 1,
        "a relaunch asks no sooner than the process that asked"
    );
    // The pause over, and the answer still missing: asked again, once.
    CLOCK.with(|clock| clock.set(first.due_by(CLOCK_AT)));
    relaunch.tick_permission_walk(t0 + GRANT_POLL);
    assert_eq!(
        AUTOMATION_ASKS.with(Cell::get),
        asks + 2,
        "asked again on the walk's pause"
    );
    assert_eq!(kept().map(|asked| asked.tries), Some(2));
    AUTOMATION_OUT.with(|out| out.set(false));
    relaunch.tick_permission_walk(t0 + 2 * GRANT_POLL);
    assert_eq!(
        AUTOMATION_ASKS.with(Cell::get),
        asks + 2,
        "and not again before its next pause"
    );
    drop(relaunch);

    // The pause over but the sign-out answered (its row no longer waits on it): nothing.
    let later = kept().unwrap();
    CLOCK.with(|clock| clock.set(later.due_by(later.at)));
    let mut answered = ready(true, None);
    answered.tick_permission_walk(t0);
    assert_eq!(
        AUTOMATION_ASKS.with(Cell::get),
        asks + 2,
        "an answer is not asked again"
    );
    // A new revision asks again by itself.
    approve(&mut answered, 2);
    answered.tick_permission_walk(t0 + GRANT_POLL);
    assert_eq!(
        AUTOMATION_ASKS.with(Cell::get),
        asks + 3,
        "a new revision asks again"
    );
    drop(answered);

    // NEGATIVE CONTROL: a plan that signs out of nothing asks nothing.
    AUTOMATION_OUT.with(|out| out.set(false));
    let asks = AUTOMATION_ASKS.with(Cell::get);
    let mut nothing = ready(false, Some(&unanswered));
    nothing.tick_permission_walk(t0);
    assert_eq!(
        AUTOMATION_ASKS.with(Cell::get),
        asks,
        "none planned, nothing asked"
    );
    CLOCK.with(|clock| clock.set(CLOCK_AT));
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
/// the window's gesture free, and then goes on (Accessibility's pane opens, aterm trusted already
/// so its row starts no walk). That pane's own
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

    // Trusted already, Accessibility's row takes no walk of its own: its press opens the pane.
    AX_TRUSTED.with(|trusted| trusted.set(true));
    let opened = app.open_access_from_system_state(Access::Accessibility, true);
    assert!(app.devtools_helper.panel().is_none(), "it gave way");
    assert_eq!(panel_counts().2, closes + 1);
    assert!(!app.permission_walk.active(), "the walk is over");
    assert!(opened, "Accessibility's pane opened");
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
        "Accessibility's pane, held with no panel beside it"
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
    for access in [Access::Debugger, Access::Accessibility] {
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
