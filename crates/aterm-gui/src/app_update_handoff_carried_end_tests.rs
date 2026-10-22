// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CARRIED END ACROSS A SEAMLESS UPDATE (`docs/DESIGN-harness-autopilot-
//! 2026-10-15.md` §11.2.3, item 8; §11.2.5 "The carry"; slice 2b): the park
//! stamps each supervised session's end and its task's work on its manifest
//! record (`SessionRecord::autopilot_end`, `App::end_carry_rows`); the
//! successor seeds the end's face and time onto the timeline before the
//! session is registered (`spawn::seed_adopted_end`), and spends the rest at
//! the session's first publication (`App::adopt_end_carry`). The carried end
//! is the menu bar's Done for you row it was before the update (§11.4: its
//! word and its instant, `statusitem … done=`), and it comes in cheered: the
//! successor's kitty never cheers it (§11.3.4).

use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;

use super::FreezeSeed;
use crate::native_updater_service::ApplyMode;
use crate::presence::ChipLevel;
use crate::session_store::{SessionHandoff, SessionRecord};
use crate::{App, HarnessFact, WindowId};

const SEC: u64 = 1_000;

/// A host that supervises nothing: what the App's posture needs to read
/// `On`, shut down by the caller.
fn idle_host() -> crate::harness_host::HostHandle {
    crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    )
}

/// An App whose host supervises nothing and whose table's default is Land
/// (`[harness] autopilot = "land"`): under FOREVER LAND only Land ends a
/// task, and a session that follows the default keeps no user field (the
/// review's R6).
fn desk(host: &crate::harness_host::HostHandle) -> App {
    let mut app = App::headless_for_test();
    app.harness = Some(host.clone());
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nautopilot = \"land\"\n"),
    );
    app
}

/// Session `session` runs Claude Code under NO WORD OF ITS OWN — the
/// desk's default, Land — claimed by this host.
fn claude(app: &App, session: u64) {
    let ctx = app.pool.get(session).expect("pooled").ctx.clone();
    {
        let mut tl = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claimed");
}

/// Window 0 looked at: the frontmost, on screen.
fn look(app: &mut App) {
    app.frontmost_window = Some(WindowId(0));
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .band_on_screen_for_test = true;
}

/// The agent word the status sweep publishes for session `session`.
fn publish(app: &App, session: u64, word: &'static str) -> bool {
    app.pool
        .get(session)
        .expect("pooled")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        )
}

/// Session `session`'s agent goes busy, works `ms` on the ledger clock
/// (moved on, not waited), and sits at its prompt: both words fed to the A/B
/// book as the sweep feeds them.
fn work(app: &mut App, session: u64, ms: u64) {
    assert!(publish(app, session, "busy"), "busy published");
    app.note_harness_phase(session);
    crate::turn_ledger::advance_for_test(ms);
    assert!(publish(app, session, "idle"), "idle published");
    app.note_harness_phase(session);
}

/// Session `sid`'s loop tells `face` through the host's wake.
fn tell(app: &mut App, sid: &str, face: Option<TaskFace>) {
    app.apply_harness_fact(sid, HarnessFact::Task(face));
}

/// Every tab's mark in window 0, as its strip is stamped.
fn marks(app: &App) -> Vec<ChipLevel> {
    let ws = &app.windows[&WindowId(0)];
    let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
        .tab_set
        .tabs()
        .iter()
        .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
        .collect();
    app.stamp_presence_chips(WindowId(0), &mut metadata);
    metadata.iter().map(|m| m.attention).collect()
}

/// Land's landings past the floor the book counted this week.
fn landings(app: &App) -> u32 {
    app.harness_screen
        .ab
        .week_at(crate::turn_ledger::now_ms(), &[])
        .ends
        .landed
}

/// Session `session`'s sid.
fn sid(app: &App, session: u64) -> String {
    app.pool
        .get(session)
        .expect("pooled")
        .ctx
        .self_id
        .as_str()
        .to_string()
}

/// The local day `unix_ms` falls on.
fn local_day(unix_ms: i64) -> i64 {
    let at = unix_ms.div_euclid(1000);
    let offset = crate::presence::local_offset_at(at).expect("the zone reads");
    (at + offset).div_euclid(86_400)
}

/// 17:42:37.123 local, yesterday: an end's time that no restate today can
/// read, and off the minute, so the end's instant (the timeline's, which
/// the park carries and the row reads) is told apart from its own minute
/// (the glass's `Stamp`, whose `Stamp::unix_ms` is 17:42:00).
fn yesterday_1742() -> i64 {
    let day = local_day(crate::harness_screen::unix_ms_now()) - 1;
    let local = day * 86_400 + 17 * 3_600 + 42 * 60;
    let offset = crate::presence::local_offset_at(local).expect("the zone reads");
    (local - offset) * 1000 + 37_123
}

/// The Autopilot chip's title of session `session`, as the footer builds it.
fn chip_title(app: &App, session: u64) -> String {
    let today = Some(local_day(crate::harness_screen::unix_ms_now()));
    crate::claude_lights::autopilot_title(
        app.session_autopilot(session).1,
        &app.chip_glass(session),
        "",
        today,
        crate::claude_lights::TitleWords::default(),
    )
    .0
}

/// Session `session`'s state line on Settings ▸ Harness's SESSIONS card, as
/// the page publishes it (the page is opened in window 0 to read it).
fn page_state_line(app: &mut App, session: u64) -> Option<String> {
    assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
    app.publish_native_harness_state();
    let instance = app
        .native_runtime
        .instance_by_kind(crate::native_app::AppKind::Settings)
        .expect("the Settings controller");
    let Some(crate::native_app::NativeApp::Settings(settings)) = app.native_runtime.app(instance)
    else {
        unreachable!("a Settings instance")
    };
    let projection = settings.harness_snapshot().projection();
    projection
        .sessions
        .iter()
        .find(|row| row.local_id == session)
        .expect("the session's row")
        .state_line()
}

/// Session `session`'s Done for you row (§11.4.3), as the menu bar's glance
/// is built off the real App (`App::status_glance`): the one row whose click
/// goes to its tab.
fn done_row(app: &App, session: u64) -> Option<crate::status_item::DoneRow> {
    let glance = app.status_glance();
    let mut rows = glance.done.iter().filter(|row| {
        matches!(row.at, crate::status_item::DoneAt::Go(at)
            if glance.go[usize::from(at)] == session)
    });
    let row = rows.next().cloned();
    assert!(rows.next().is_none(), "one row per session");
    row
}

/// Session `session`'s end no pet has cheered yet: the room's `landed`
/// (§11.3.3, item 1).
fn uncheered(app: &App, session: u64) -> Option<u32> {
    app.harness_glass
        .get(&session)
        .and_then(crate::harness_glass::SessionGlass::landed)
}

/// The kitty as slice 3's cheer tests keep it (§11.3): the resident pet the
/// trail, Full motion, window 0 presenting through a scripted CPU surface
/// (the real redraw path) and focused; drawn for a moment on its front tab.
fn with_the_pet(app: &mut App, t: &mut Instant) {
    app.config.serious_mode = Some(false);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty pet".into());
    app.config.motion = Some("full".into());
    app.kitty_cursor_enabled_cache = None;
    app.recompute_sparkle();
    assert!(app.trail_is_kitty_pet(), "fixture: the pet is the trail");
    let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
    ws.present = Some(crate::PresentTarget::Cpu {
        surface: crate::present::CpuSurface::Scripted(Default::default()),
    });
    ws.focused = true;
    for _ in 0..10 {
        *t += Duration::from_millis(16);
        app.redraw_window_at(WindowId(0), *t);
    }
}

/// Window 0 switched to tab `tab` (the sighting, §11.3.3 item 3) and drawn
/// for a second: whether its pet owed a cheer on any of those frames.
fn cheers_at(app: &mut App, tab: usize, t: &mut Instant) -> bool {
    app.switch_tab_in(WindowId(0), tab);
    let mut cheered = false;
    for _ in 0..60 {
        *t += Duration::from_millis(16);
        app.redraw_window_at(WindowId(0), *t);
        cheered |= app.windows[&WindowId(0)]
            .fx
            .companion()
            .brain()
            .cheer_owed();
    }
    cheered
}

/// A quiet, close-on-exec PTY master for every session of `app`, as the
/// park needs them (a stub's `-1` master reads as activity): each session
/// owns a duplicate of one pair. Returns the descriptors [`release`] closes.
fn quiet(app: &mut App) -> Vec<i32> {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut fds = vec![slave, master];
    for pooled in app.pool.sessions.values_mut() {
        // SAFETY: `master` is the test's open PTY master; `F_DUPFD_CLOEXEC`
        // returns a new descriptor, close-on-exec from birth.
        let dup = unsafe { libc::fcntl(master, libc::F_DUPFD_CLOEXEC, 0) };
        assert!(dup >= 0, "dup the quiet master");
        pooled.session.master = dup;
        fds.push(dup);
    }
    fds
}

/// Hand every session's master back and close the test's descriptors.
fn release(app: &mut App, fds: Vec<i32>) {
    for pooled in app.pool.sessions.values_mut() {
        pooled.session.master = -1;
    }
    for fd in fds {
        // SAFETY: closing a test-owned descriptor exactly once.
        unsafe { libc::close(fd) };
    }
}

/// THE PARK, the prelaunched lane's own
/// (`App::park_and_transfer_to_prelaunched_successor`), of `parent`'s desk:
/// the manifest it hands over, as the successor reads it back off the wire.
fn park(parent: &mut App) -> SessionHandoff {
    let fds = quiet(parent);
    let (attempt, transferred) =
        super::dry_run_capture_tests::prelaunched(31, ApplyMode::Immediate, FreezeSeed::Default);
    parent.update_handoff_prelaunch = Some(attempt);
    let parked = parent.park_and_transfer_to_prelaunched_successor(Instant::now());
    assert!(
        matches!(parked, super::ParkAttempt::Parked),
        "PRECONDITION: the park landed ({parked:?})"
    );
    let transfer = transferred.try_recv().expect("the park's capture");
    let wire = transfer.capture.manifest.to_toml().expect("serializes");
    drop(transfer);
    parent.pending_update_handoff = None;
    parent.update_handoff_prelaunch = None;
    release(parent, fds);
    SessionHandoff::from_toml(&wire).expect("the successor reads it")
}

/// Record `sid`'s of manifest `read`.
fn record_of(read: &SessionHandoff, sid: &str) -> SessionRecord {
    read.sessions
        .iter()
        .find(|rec| rec.sid == sid)
        .cloned()
        .expect("its record")
}

/// A successor's adopted shell answering to `sid`, as `spawn_session`
/// builds one before registration: the stub of a handed shell, its timeline
/// seeded from the record's Autopilot end row through the spawn's own seam
/// (`spawn::seed_adopted_end`).
fn adopted(id: u64, rec: &SessionRecord) -> crate::Session {
    crate::prime_test_timing();
    let session = crate::ptyless_session(
        id,
        aterm_session::SessionId::new(&rec.sid),
        aterm_core::terminal::Terminal::new(24, 80),
        Arc::new(aterm_session::sink::SinkWriter::new(-1)),
    );
    *session
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .slog_mut() = crate::session_log::SessionLog::for_test();
    crate::spawn::seed_adopted_end(&session.ctx.timeline, rec.autopilot_end.as_deref());
    session
}

/// THE SUCCESSOR of a park whose manifest held `records` (as read back off
/// the wire): its window's own first shell in front of window 0 (looked
/// at), then each record's shell adopted under its sid in a tab of its own
/// behind it, under no word of its own (the desk's default, Land) and claimed by
/// this host's own supervisor — the intake mapping each record's row by sid
/// (`seamless::end_carry_of`, as the real intake does) — and each
/// adopted session's first publication (`App::refresh_presence_session`).
/// The App and the adopted sessions' local ids, in record order.
fn successor(records: &[SessionRecord], host: &crate::harness_host::HostHandle) -> (App, Vec<u64>) {
    let mut app = desk(host);
    app.end_carry = crate::seamless::end_carry_of(records);
    let mut ids = Vec::new();
    for rec in records {
        let id = app.next_session_id;
        app.push_stub_tab(WindowId(0), adopted(id, rec));
        claude(&app, id);
        ids.push(id);
    }
    app.switch_tab_in(WindowId(0), 0);
    look(&mut app);
    for id in &ids {
        app.refresh_presence_session(*id);
    }
    (app, ids)
}

/// AN UNSEEN END AND THE TASK'S WORK CROSS A SEAMLESS UPDATE (§11.2.5 "The
/// carry"). The parent: two Claude Code sessions under the desk's default
/// Land with no user field of their own, behind a shell in front of a
/// window someone looks at. The FINISHED one opened its task, worked 45 s,
/// and its landing at 17:42 (yesterday) is an end nobody looked at: its tab
/// wears the `•`. The WORKING one is 25 s into its task. The real park (the
/// prelaunched lane's) stamps both rows, and the successor reads them back
/// off the wire:
///
/// * the finished tab wears the `•` before its loop is back, and keeps it
///   after the loop restates its landing from "unknown" — which is no end
///   and counts nothing — and the restated landing keeps 17:42: the chip's
///   face clock, its title and the page's state line;
/// * the working session's loop restates open, its agent works 10 s more,
///   and its landing is an END: 35 s since the task opened, not 10;
/// * the finished session's end is the Done for you row it was before the
///   update (§11.4): `landed`, aged from 17:42:37.123 — the end's instant,
///   neither its own minute nor the successor's restate — before and after
///   the loop restates the landing, and `chrome`'s `statusitem` counts it
///   (`done=1`);
/// * it came in cheered (§11.3.4): the successor's pet, shown its tab, owes
///   no cheer for it.
///
/// NEGATIVE CONTROLS, in-test: the same successor fed the same manifest
/// unstamped (as a build before the carry parks it) — no `•`, no Done for
/// you row, the restated landing stamped at the restate, and 10 s then
/// landed is no end; and the working session's end, told in the successor, which its
/// pet cheers on sight. As run (each
/// fails this test): the spend gated by the restore's user-meta seed's
/// early return (`App::seed_restored_user_meta`: every user field `None`, so
/// nothing is spent for a session that follows the default, the review's
/// R6); the work not carried (`AbBook::carry_work` adding nothing, as a
/// zeroing at "unknown" would); the timeline's seed dropped; the carried
/// end put in uncheered (`SessionGlass::carry_end` with `cheered: 0`: the
/// room's `landed` hands its serial to the pet); the park carrying the
/// end's own minute, not the timeline's instant (`App::end_carry_rows`);
/// the row's instant read off the end's minute (`end_wall_ms(end, None)`).
#[test]
fn an_unseen_end_and_the_tasks_work_cross_a_seamless_update() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let host = idle_host();
    let wid = WindowId(0);
    let at_1742 = yesterday_1742();

    // THE PARENT.
    let mut parent = desk(&host);
    let working = parent.next_session_id;
    parent.push_stub_tab(wid, crate::stub_session(working));
    let shell = parent.next_session_id;
    parent.push_stub_tab(wid, crate::stub_session(shell));
    claude(&parent, 0);
    claude(&parent, working);
    look(&mut parent);
    let (finished_sid, working_sid) = (sid(&parent, 0), sid(&parent, working));
    tell(&mut parent, &finished_sid, Some(TaskFace::Open));
    work(&mut parent, 0, 45 * SEC);
    // The landing, at 17:42:37.123: told with that time (a noted face's
    // stamp, the test's way to place the move), from open — an end.
    parent.apply_harness_fact(
        &finished_sid,
        HarnessFact::TaskNoted(TaskFace::Landed, at_1742),
    );
    assert!(parent.end_unseen(0), "PRECONDITION: an end nobody saw");
    assert_eq!(
        marks(&parent),
        [ChipLevel::Done, ChipLevel::Off, ChipLevel::Off],
        "PRECONDITION: its `•`"
    );
    let row = done_row(&parent, 0).expect("PRECONDITION: its Done for you row");
    assert_eq!(
        (row.landed, row.since_unix_ms),
        (true, u64::try_from(at_1742).expect("after the epoch")),
        "PRECONDITION: landed, at its instant (17:42:37.123, not its minute)"
    );
    tell(&mut parent, &working_sid, Some(TaskFace::Open));
    work(&mut parent, working, 25 * SEC);

    // THE PARK, the prelaunched lane's own.
    let read = park(&mut parent);
    let record = |sid: &str| record_of(&read, sid);
    let records = [record(&finished_sid), record(&working_sid)];
    assert_eq!(
        records[0].autopilot_end.as_deref(),
        Some(format!("end=landed@{at_1742} seen=0 worked=0").as_str()),
        "the end, unseen, at its own time"
    );
    // The ledger clock is the wall clock plus the test's advance, so the
    // busy stretch is the 25 s advanced and the real milliseconds between
    // its two publishes; under 30 s, the end after the update still needs
    // its 10 s.
    let worked: u64 = records[1]
        .autopilot_end
        .as_deref()
        .and_then(|row| row.strip_prefix("end=- worked="))
        .and_then(|ms| ms.parse().ok())
        .expect("the work, and no end");
    assert!((25_000..30_000).contains(&worked), "the work: {worked} ms");
    assert_eq!(
        record(&sid(&parent, shell)).autopilot_end,
        None,
        "a shell carries no key"
    );

    // THE SUCCESSOR.
    let (mut next, ids) = successor(&records, &host);
    let (finished, working) = (ids[0], ids[1]);
    assert_eq!(
        marks(&next),
        [ChipLevel::Off, ChipLevel::Done, ChipLevel::Off],
        "the `•` crossed, before the loop is back"
    );
    let line = next.stripless_tab_chrome().expect("the tab line");
    assert!(
        line.contains(r#"states=[["selected"], ["done"], []]"#),
        "{line}"
    );
    // Its Done for you row, as it stood: landed, the end's own instant.
    let carried = done_row(&next, finished).expect("the Done for you row crossed");
    assert_eq!(
        (carried.landed, carried.since_unix_ms),
        (row.landed, row.since_unix_ms),
        "the row the parent showed"
    );
    let line = next.status_item_chrome_line();
    assert!(line.contains(" done=1 "), "{line}");
    // It came in cheered: no pet is handed it.
    assert_eq!(uncheered(&next, finished), None, "a carried end is cheered");
    // The loop's start restates the landing from "unknown": no end, no
    // count, and its clock is the end's own.
    tell(&mut next, &finished_sid, Some(TaskFace::Landed));
    let stamp = crate::harness_glass::Stamp::at(at_1742.div_euclid(1000));
    assert_eq!(next.chip_glass(finished).task, Some(TaskFace::Landed));
    assert_eq!(next.chip_glass(finished).task_at, stamp, "the face's clock");
    let title = chip_title(&next, finished);
    assert!(
        title.starts_with("autopilot: land, landed ") && title.contains("17:42"),
        "{title}"
    );
    assert_eq!(marks(&next)[1], ChipLevel::Done, "still unseen");
    assert_eq!(landings(&next), 0, "a carried end is no new end");
    assert_eq!(
        next.harness_glass[&finished].end().map(|end| end.serial),
        Some(1)
    );
    assert_eq!(
        done_row(&next, finished).map(|row| row.since_unix_ms),
        Some(row.since_unix_ms),
        "the restated landing keeps the row's instant"
    );
    // The working session: 10 s more after the update, then landed — an
    // end.
    tell(&mut next, &working_sid, Some(TaskFace::Open));
    work(&mut next, working, 10 * SEC);
    tell(&mut next, &working_sid, Some(TaskFace::Landed));
    assert_eq!(
        marks(&next),
        [ChipLevel::Off, ChipLevel::Done, ChipLevel::Done],
        "25 s + 10 s: a new end"
    );
    assert_eq!(landings(&next), 1, "counted under Land");
    let line = next.status_item_chrome_line();
    assert!(line.contains(" done=2 "), "{line}");
    // THE KITTY: shown the carried end's tab, it cheers nothing; shown the
    // end told here (CONTROL), it cheers.
    let mut t = Instant::now();
    with_the_pet(&mut next, &mut t);
    assert!(
        !cheers_at(&mut next, 1, &mut t),
        "no cheer for a carried end"
    );
    assert_eq!(uncheered(&next, finished), None);
    assert_eq!(
        uncheered(&next, working),
        Some(1),
        "PRECONDITION: the end told here waits for its cheer"
    );
    assert!(
        cheers_at(&mut next, 2, &mut t),
        "CONTROL: an end told here cheers"
    );
    let line = page_state_line(&mut next, finished).expect("a state line");
    assert!(
        line.starts_with("landed ") && line.contains("17:42"),
        "{line}"
    );

    // NEGATIVE CONTROL: the same manifest, unstamped.
    let unstamped: Vec<SessionRecord> = records
        .iter()
        .cloned()
        .map(|rec| SessionRecord {
            autopilot_end: None,
            ..rec
        })
        .collect();
    let before = crate::harness_screen::unix_ms_now();
    let (mut bare, ids) = successor(&unstamped, &host);
    let (finished, working) = (ids[0], ids[1]);
    assert_eq!(marks(&bare), [ChipLevel::Off; 3], "CONTROL: no `•`");
    assert_eq!(done_row(&bare, finished), None, "CONTROL: no row");
    let line = bare.status_item_chrome_line();
    assert!(line.contains(" done=0 "), "CONTROL: {line}");
    tell(&mut bare, &finished_sid, Some(TaskFace::Landed));
    let (_, at) = bare
        .pool
        .get(finished)
        .expect("pooled")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task_face()
        .expect("the restated face");
    assert!(
        at >= before,
        "CONTROL: the restate's clock ({at} < {before})"
    );
    assert_ne!(bare.chip_glass(finished).task_at, stamp);
    tell(&mut bare, &working_sid, Some(TaskFace::Open));
    work(&mut bare, working, 10 * SEC);
    tell(&mut bare, &working_sid, Some(TaskFace::Landed));
    assert_eq!(marks(&bare), [ChipLevel::Off; 3], "CONTROL: 10 s, no end");
    // The book counts the landing (`harness_task::is_an_end` has no work
    // floor); the end, which has (`is_a_task_end`), is not told.
    assert_eq!(
        bare.harness_glass.get(&working).and_then(|g| g.end()),
        None,
        "CONTROL: no end"
    );
    host.shutdown_and_join();
}

/// A CARRIED END IS JUDGED AT ONCE, AND NEVER OUTLIVES ITS WORK (§11.2.3,
/// item 8; §11.2.4): spent where its session is in the front tab of a window
/// someone looks at, the end is seen as it lands and marks nothing, even
/// once another tab comes in front; one the outgoing process had seen
/// crosses seen; one whose loop has already restated another face (open:
/// it moved on) is dropped, fail-closed, and its work kept; one the loop
/// moves on from after the spend is cleared with its `•` (one re-stamp).
/// A row that does not read carries nothing. NEGATIVE CONTROL: the same
/// unseen end behind the front tab wears the `•`; as run, a spend with no
/// judgement at once marks the looked-at tab once it goes behind.
#[test]
fn a_carried_end_is_judged_at_once_and_never_outlives_its_work() {
    let host = idle_host();
    let at = yesterday_1742();
    const SHELL: &str = "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-0\"\n\
                         state = \"alive\"\ntitle = \"zsh\"\n";
    let base = SessionHandoff::from_toml(SHELL)
        .expect("a manifest")
        .sessions[0]
        .clone();
    let record = |sid: &str, row: &str| SessionRecord {
        sid: sid.to_string(),
        autopilot_end: Some(row.to_string()),
        ..base.clone()
    };
    let unseen = format!("end=landed@{at} seen=0 worked=0");
    let fresh_sid = || aterm_session::SessionId::generate().as_str().to_string();

    // CONTROL: behind the front tab, it marks.
    let (behind, _) = successor(&[record(&fresh_sid(), &unseen)], &host);
    assert_eq!(marks(&behind), [ChipLevel::Off, ChipLevel::Done]);

    // In the front tab of a window someone looks at: seen at once.
    let mut front = desk(&host);
    let rec = record(&fresh_sid(), &unseen);
    front.end_carry = crate::seamless::end_carry_of(std::slice::from_ref(&rec));
    let id = front.next_session_id;
    front.push_stub_tab(WindowId(0), adopted(id, &rec));
    claude(&front, id);
    look(&mut front);
    crate::work_counts::take_end_restamps();
    front.refresh_presence_session(id);
    assert!(!front.end_unseen(id), "seen as it lands");
    assert_eq!(crate::work_counts::take_end_restamps(), 0, "nothing drawn");
    front.switch_tab_in(WindowId(0), 0);
    assert_eq!(marks(&front), [ChipLevel::Off, ChipLevel::Off]);

    // Seen before the update: crosses seen.
    let (seen, _) = successor(
        &[record(
            &fresh_sid(),
            &format!("end=landed@{at} seen=1 worked=0"),
        )],
        &host,
    );
    assert_eq!(marks(&seen), [ChipLevel::Off, ChipLevel::Off]);

    // The loop moved on before the first publication: dropped, work kept.
    let mut moved = desk(&host);
    let rec = record(
        &fresh_sid(),
        &format!("end=landed@{at} seen=0 worked=25000"),
    );
    moved.end_carry = crate::seamless::end_carry_of(std::slice::from_ref(&rec));
    let id = moved.next_session_id;
    moved.push_stub_tab(WindowId(0), adopted(id, &rec));
    claude(&moved, id);
    moved.switch_tab_in(WindowId(0), 0);
    look(&mut moved);
    tell(&mut moved, &rec.sid, Some(TaskFace::Open));
    moved.refresh_presence_session(id);
    assert_eq!(marks(&moved), [ChipLevel::Off, ChipLevel::Off], "dropped");
    assert_eq!(moved.harness_glass.get(&id).and_then(|g| g.end()), None);
    assert_eq!(
        moved
            .harness_screen
            .ab
            .worked(id, crate::turn_ledger::now_ms()),
        25_000,
        "the work is the task's, kept"
    );

    // Spent, then the loop moves on: cleared with its `•`, once.
    let (mut after, ids) = successor(&[record(&fresh_sid(), &unseen)], &host);
    let sid_after = sid(&after, ids[0]);
    assert_eq!(marks(&after), [ChipLevel::Off, ChipLevel::Done]);
    crate::work_counts::take_end_restamps();
    tell(&mut after, &sid_after, Some(TaskFace::Open));
    assert_eq!(marks(&after), [ChipLevel::Off, ChipLevel::Off]);
    assert_eq!(
        crate::work_counts::take_end_restamps(),
        1,
        "taken down once"
    );

    // A row that does not read carries nothing.
    let (bad, _) = successor(&[record(&fresh_sid(), "end=landed seen=0")], &host);
    assert_eq!(marks(&bad), [ChipLevel::Off, ChipLevel::Off]);
    host.shutdown_and_join();
}

/// A LANDING WITHDRAWN BEFORE THE SPEND BRINGS NO END BACK (the review of
/// `c5af3a7507`): the loop's start restates the seeded landing (it takes
/// the carried 17:42 back), then WITHDRAWS it — `open`, then a new landing,
/// both before the session's first publication. The landing the timeline
/// now holds is another, at its own time, and no end (from `open` with no
/// work); the spend holds the restated face to the seeded face AND time
/// (`CarriedEnd::told`), so the carried end is dropped. NEGATIVE CONTROL,
/// in-test: the same row restated once, unwithdrawn, is spent (`•`); as
/// run, the spend comparing the face's kind alone brings the 17:42 end
/// back.
#[test]
fn a_landing_withdrawn_before_the_spend_brings_no_end_back() {
    let host = idle_host();
    let at = yesterday_1742();
    const SHELL: &str = "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-0\"\n\
                         state = \"alive\"\ntitle = \"zsh\"\n";
    let base = SessionHandoff::from_toml(SHELL)
        .expect("a manifest")
        .sessions[0]
        .clone();
    let row = format!("end=landed@{at} seen=0 worked=0");
    let restated = |withdrawn: bool| {
        let rec = SessionRecord {
            sid: aterm_session::SessionId::generate().as_str().to_string(),
            autopilot_end: Some(row.clone()),
            ..base.clone()
        };
        let mut app = desk(&host);
        app.end_carry = crate::seamless::end_carry_of(std::slice::from_ref(&rec));
        let id = app.next_session_id;
        app.push_stub_tab(WindowId(0), adopted(id, &rec));
        claude(&app, id);
        app.switch_tab_in(WindowId(0), 0);
        look(&mut app);
        tell(&mut app, &rec.sid, Some(TaskFace::Landed));
        if withdrawn {
            tell(&mut app, &rec.sid, Some(TaskFace::Open));
            tell(&mut app, &rec.sid, Some(TaskFace::Landed));
        }
        let (face, told_at) = app
            .pool
            .get(id)
            .expect("pooled")
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .task_face()
            .expect("a face");
        assert_eq!(face, TaskFace::Landed);
        assert_eq!(
            told_at == at,
            !withdrawn,
            "PRECONDITION: whose landing the timeline holds"
        );
        assert!(!app.end_unseen(id), "PRECONDITION: no end of its own");
        // The first publication spends the carry.
        app.refresh_presence_session(id);
        (app, id)
    };
    let (kept, _) = restated(false);
    assert_eq!(
        marks(&kept),
        [ChipLevel::Off, ChipLevel::Done],
        "CONTROL: the seeded landing, restated, is the carried end's"
    );
    let (app, id) = restated(true);
    assert_eq!(
        marks(&app),
        [ChipLevel::Off, ChipLevel::Off],
        "a withdrawn landing's end must not come back"
    );
    assert_eq!(app.harness_glass.get(&id).and_then(|g| g.end()), None);
    host.shutdown_and_join();
}
