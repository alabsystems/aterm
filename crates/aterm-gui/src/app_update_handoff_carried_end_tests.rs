// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for carrying supervised session ends and task work across a seamless update.
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;

use super::FreezeSeed;
use crate::native_updater_service::ApplyMode;
use crate::presence::ChipLevel;
use crate::session_store::{SessionHandoff, SessionRecord};
use crate::{App, HarnessFact, WindowId};

const SEC: u64 = 1_000;

fn idle_host() -> crate::harness_host::HostHandle {
    crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    )
}

fn desk(host: &crate::harness_host::HostHandle) -> App {
    let mut app = App::headless_for_test();
    app.harness = Some(host.clone());
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nautopilot = \"land\"\n"),
    );
    app
}

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

fn look(app: &mut App) {
    app.frontmost_window = Some(WindowId(0));
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .band_on_screen_for_test = true;
}

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

fn work(app: &mut App, session: u64, ms: u64) {
    assert!(publish(app, session, "busy"), "busy published");
    app.note_harness_phase(session);
    crate::turn_ledger::advance_for_test(ms);
    assert!(publish(app, session, "idle"), "idle published");
    app.note_harness_phase(session);
}

fn tell(app: &mut App, sid: &str, face: Option<TaskFace>) {
    app.apply_harness_fact(sid, HarnessFact::Task(face));
}

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

fn landings(app: &App) -> u32 {
    app.harness_screen
        .ab
        .week_at(crate::turn_ledger::now_ms(), &[])
        .ends
        .landed
}

fn sid(app: &App, session: u64) -> String {
    app.pool
        .get(session)
        .expect("pooled")
        .ctx
        .self_id
        .as_str()
        .to_string()
}

fn local_day(unix_ms: i64) -> i64 {
    let at = unix_ms.div_euclid(1000);
    let offset = crate::presence::local_offset_at(at).expect("the zone reads");
    (at + offset).div_euclid(86_400)
}

fn yesterday_1742() -> i64 {
    let day = local_day(crate::harness_screen::unix_ms_now()) - 1;
    let local = day * 86_400 + 17 * 3_600 + 42 * 60;
    let offset = crate::presence::local_offset_at(local).expect("the zone reads");
    (local - offset) * 1000 + 37_123
}

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

fn uncheered(app: &App, session: u64) -> Option<u32> {
    app.harness_glass
        .get(&session)
        .and_then(crate::harness_glass::SessionGlass::landed)
}

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

fn quiet(app: &mut App) -> Vec<i32> {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut fds = vec![slave, master];
    for pooled in app.pool.sessions.values_mut() {
        // SAFETY: `master` is the test's open PTY master; `F_DUPFD_CLOEXEC` duplicates it.
        let dup = unsafe { libc::fcntl(master, libc::F_DUPFD_CLOEXEC, 0) };
        assert!(dup >= 0, "dup the quiet master");
        pooled.session.master = dup;
        fds.push(dup);
    }
    fds
}

fn release(app: &mut App, fds: Vec<i32>) {
    for pooled in app.pool.sessions.values_mut() {
        pooled.session.master = -1;
    }
    for fd in fds {
        // SAFETY: Closes a test-owned file descriptor once.
        unsafe { libc::close(fd) };
    }
}

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

fn record_of(read: &SessionHandoff, sid: &str) -> SessionRecord {
    read.sessions
        .iter()
        .find(|rec| rec.sid == sid)
        .cloned()
        .expect("its record")
}

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

#[test]
fn an_unseen_end_and_the_tasks_work_cross_a_seamless_update() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let host = idle_host();
    let wid = WindowId(0);
    let at_1742 = yesterday_1742();

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

    let read = park(&mut parent);
    let record = |sid: &str| record_of(&read, sid);
    let records = [record(&finished_sid), record(&working_sid)];
    assert_eq!(
        records[0].autopilot_end.as_deref(),
        Some(format!("end=landed@{at_1742} seen=0 worked=0").as_str()),
        "the end, unseen, at its own time"
    );
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
    let carried = done_row(&next, finished).expect("the Done for you row crossed");
    assert_eq!(
        (carried.landed, carried.since_unix_ms),
        (row.landed, row.since_unix_ms),
        "the row the parent showed"
    );
    let line = next.status_item_chrome_line();
    assert!(line.contains(" done=1 "), "{line}");
    assert_eq!(uncheered(&next, finished), None, "a carried end is cheered");
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
    assert_eq!(
        bare.harness_glass.get(&working).and_then(|g| g.end()),
        None,
        "CONTROL: no end"
    );
    host.shutdown_and_join();
}

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

    let (behind, _) = successor(&[record(&fresh_sid(), &unseen)], &host);
    assert_eq!(marks(&behind), [ChipLevel::Off, ChipLevel::Done]);

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

    let (seen, _) = successor(
        &[record(
            &fresh_sid(),
            &format!("end=landed@{at} seen=1 worked=0"),
        )],
        &host,
    );
    assert_eq!(marks(&seen), [ChipLevel::Off, ChipLevel::Off]);

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

    let (bad, _) = successor(&[record(&fresh_sid(), "end=landed seen=0")], &host);
    assert_eq!(marks(&bad), [ChipLevel::Off, ChipLevel::Off]);
    host.shutdown_and_join();
}

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
