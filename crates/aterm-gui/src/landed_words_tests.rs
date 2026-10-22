// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LANDED IS LANDED (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.4.1 and
//! §12.4.3 item 1): every word the window has for a landing says
//! `landed`, seen or not. The menu bar's worker and its manager read a
//! record's `why=landed` and keep an older build's `done`; This Mac counts a
//! landed tab that is no worker, and an exited worker apart; a Done for you
//! row stays `landed` once its end was looked at; rows that share a label
//! say where they are, and a unique one names no place; the tab's hover and
//! the Overview say who drives in the chip's own words, and the hover is
//! stamped again where only the face moves (no end told, under the work
//! floor) and where only the timer does.
//!
//! The menu is read as `chrome` prints it (`status_item::menu_lines` over
//! `compose_status_menu`), the hover as the strip was last stamped with it
//! (`work_counts::last_tooltips_pushed`). Each test names its NEGATIVE
//! CONTROL: the mutant it was run against, and what failed.

use std::sync::Arc;

use aterm_agent::harness::acts::TaskFace;

use crate::status_item::{EndRow, Glance, SessionRow, TaskRow, compose_status_menu, menu_lines};
use crate::task_record::TaskState;
use crate::{App, WindowId};

/// The menu's clock: every row's age is read from it.
const NOW: i64 = 1_800_000_000;

/// `secs` (unix seconds) in unix milliseconds.
fn ms(secs: i64) -> u64 {
    u64::try_from(secs).expect("a positive time") * 1000
}

/// A worker's row: its label, its task started by `from`, in `state`, moved
/// there by `why` four minutes ago, with a result of `result` bytes.
fn worker(
    id: u64,
    label: &str,
    from: &str,
    state: TaskState,
    why: Option<&'static str>,
    result: u64,
) -> SessionRow {
    SessionRow {
        id,
        sid: format!("s-{id:020}"),
        title: label.to_string(),
        agent: "idle",
        task: Some(TaskRow {
            state,
            why,
            from: from.to_string(),
            entered: 7,
            since_unix_ms: ms(NOW - 4 * 60),
            result_bytes: result,
            result_path: (result > 0).then(|| std::path::PathBuf::from("/tmp/r.md")),
            seen: false,
        }),
        ..SessionRow::default()
    }
}

/// The composed menu as `chrome` prints it.
fn menu(rows: &[SessionRow]) -> Vec<String> {
    let glance = Glance::build(rows, &[], None, ms(NOW), None);
    menu_lines(&compose_status_menu(&glance, NOW), &[])
}

/// The one printed menu line that carries `words`, without the row's path a
/// debug build ends it with (` at=…`, always last, §12.5.2: read apart by
/// `status_item`'s `every_row_path_chrome_prints_names_its_row`).
fn line_with(lines: &[String], words: &str) -> String {
    let found: Vec<&String> = lines.iter().filter(|l| l.contains(words)).collect();
    assert_eq!(found.len(), 1, "one line with {words:?}: {lines:#?}");
    found[0]
        .rsplit_once(" at=")
        .map_or_else(|| found[0].clone(), |(line, _)| line.to_string())
}

/// A LANDED WORKER READS `landed`, AN OLDER BUILD'S DONE READS `done`
/// (§12.4.1, the worker and its manager): `lint`, whose record reads `done
/// why=landed`, is `✓ lint — landed 4m · 2.1 KB`; `fmt`, an older build's
/// `done why=marker`, keeps `✓ fmt — done 4m`; the manager counts them apart,
/// `1 working, 1 landed, 1 done`. NEGATIVE CONTROL (as run): a `worker_line`
/// and a `manager_line` that ignore `why` (main's) print `✓ lint — done 4m ·
/// 2.1 KB` and `3 workers: 1 working, 2 done`, failing the first assertion.
#[test]
fn a_landed_worker_reads_landed_and_an_older_done_reads_done() {
    let manager = SessionRow {
        id: 1,
        sid: "s-manager".to_string(),
        title: "lab-sweep".to_string(),
        agent: "busy",
        ..SessionRow::default()
    };
    let rows = [
        manager,
        worker(2, "bench", "s-manager", TaskState::Working, None, 0),
        worker(
            3,
            "lint",
            "s-manager",
            TaskState::Done,
            Some(crate::task_record::WHY_LANDED),
            2150,
        ),
        worker(4, "fmt", "s-manager", TaskState::Done, Some("marker"), 0),
    ];
    let lines = menu(&rows);
    assert!(
        line_with(&lines, "\u{2713} lint")
            .contains("\u{2713} lint \u{2014} landed 4m \u{b7} 2.1 KB"),
        "{lines:#?}"
    );
    assert!(
        line_with(&lines, "\u{2713} fmt").contains("\u{2713} fmt \u{2014} done 4m"),
        "{lines:#?}"
    );
    assert!(
        line_with(&lines, "bench \u{2014}").contains("\u{25cf} bench \u{2014} working 4m"),
        "{lines:#?}"
    );
    assert!(
        line_with(&lines, "lab-sweep \u{2014}")
            .contains("lab-sweep \u{2014} 3 workers: 1 working, 1 landed, 1 done"),
        "{lines:#?}"
    );
}

/// THIS MAC COUNTS LANDED AND EXITED APART (§12.4.1): a plain Land tab whose
/// chip wears `✓ landed` (no worker), a worker whose record reads `done
/// why=landed` and a worker whose agent exited give `This Mac — 2 landed · 1
/// exited`. NEGATIVE CONTROL (as run): main's count, from workers' records
/// alone, prints `This Mac — 1 resting · 2 done`: the landed tab counted as
/// resting, the landing and the exit as `done`.
#[test]
fn this_mac_counts_landed_and_exited_apart() {
    let plain = SessionRow {
        id: 1,
        sid: "s-plain".to_string(),
        title: "docs pass".to_string(),
        agent: "idle",
        landed: true,
        ..SessionRow::default()
    };
    let rows = [
        plain,
        worker(
            2,
            "lint",
            crate::task_record::FROM_CTL,
            TaskState::Done,
            Some(crate::task_record::WHY_LANDED),
            0,
        ),
        worker(
            3,
            "bench",
            crate::task_record::FROM_CTL,
            TaskState::Exited,
            Some("exit"),
            0,
        ),
    ];
    let lines = menu(&rows);
    assert_eq!(
        line_with(&lines, "This Mac \u{2014}"),
        "statusmenu \"This Mac \u{2014} 2 landed \u{b7} 1 exited\" enabled=no"
    );
    // In-test: an older build's done is still `done`, beside them.
    let mut older = rows.to_vec();
    older.push(worker(
        4,
        "fmt",
        crate::task_record::FROM_CTL,
        TaskState::Done,
        Some("done-check"),
        0,
    ));
    assert_eq!(
        line_with(&menu(&older), "This Mac \u{2014}"),
        "statusmenu \"This Mac \u{2014} 2 landed \u{b7} 1 done \u{b7} 1 exited\" enabled=no"
    );
}

/// A SEEN LANDING STAYS LANDED IN DONE FOR YOU (§12.4.1): a `ctl` worker that
/// Land landed (`done why=landed`), whose end was looked at, or that landed
/// under the work floor and told no end at all, reads `✓ … — landed`; an
/// older build's `done why=marker` reads `done`. NEGATIVE CONTROL (as run):
/// main's `landed = end unseen` prints `✓ seen — done 4m` and `✓ quick — done
/// 4m`.
#[test]
fn a_seen_landing_stays_landed_in_done_for_you() {
    let landed = |id, label| {
        worker(
            id,
            label,
            crate::task_record::FROM_CTL,
            TaskState::Done,
            Some(crate::task_record::WHY_LANDED),
            0,
        )
    };
    let mut seen = landed(1, "seen");
    seen.end = Some(EndRow {
        serial: 1,
        since_unix_ms: ms(NOW - 4 * 60),
        seen: true,
    });
    let quick = landed(2, "quick");
    let older = worker(
        3,
        "older",
        crate::task_record::FROM_CTL,
        TaskState::Done,
        Some("marker"),
        0,
    );
    let lines = menu(&[seen, quick, older]);
    assert!(
        line_with(&lines, "\u{2713} seen").contains("\u{2713} seen \u{2014} landed 4m"),
        "{lines:#?}"
    );
    assert!(
        line_with(&lines, "\u{2713} quick").contains("\u{2713} quick \u{2014} landed 4m"),
        "{lines:#?}"
    );
    assert!(
        line_with(&lines, "\u{2713} older").contains("\u{2713} older \u{2014} done 4m"),
        "{lines:#?}"
    );
}

/// Session `local` of `app`: a `ctl` worker that Land landed, titled
/// `title` by its pin.
fn land_worker(app: &mut App, local: u64, title: &str) {
    let ctx = app.pool.get(local).expect("pooled").ctx.clone();
    ctx.meta
        .lock()
        .expect("meta")
        .set("title", Some(title.to_string()));
    let mut mark = crate::task_record::TaskMark::new(TaskState::Done);
    mark.why = Some(crate::task_record::WHY_LANDED);
    crate::task_record::mark_session_task(&ctx, local, &mark).expect("marked");
    app.on_task_changed(local);
}

/// An App whose session 0 is in window 0's first tab and a second session in
/// its second, each a `ctl` worker that Land landed, titled `first` and
/// `second` by their pins. The second's local id.
fn two_landed_workers(first: &str, second: &str) -> (App, u64) {
    let mut app = App::headless_for_test();
    let id = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(id));
    land_worker(&mut app, 0, first);
    land_worker(&mut app, id, second);
    (app, id)
}

/// The Done for you rows of `app`'s glance, as `chrome` prints them.
fn done_rows(app: &App) -> Vec<String> {
    let glance = app.status_glance();
    assert_eq!(glance.done.len(), 2, "both landings: {:?}", glance.done);
    let mut labels: Vec<String> = glance.done.iter().map(|d| d.label.clone()).collect();
    labels.sort();
    labels
}

/// TWO ROWS WITH ONE LABEL SAY WHERE THEY ARE (§12.4.1, §11.14's W4): two
/// landed workers both titled `Claude Code`, in tabs 1 and 2, are `Claude
/// Code in tab 1` and `Claude Code in tab 2`. NEGATIVE CONTROL (as run): the
/// label alone (`place_alike_rows` not run) gives two equal rows, `Claude
/// Code` twice.
#[test]
fn two_rows_with_one_label_say_where_they_are() {
    let (app, _) = two_landed_workers("Claude Code", "Claude Code");
    assert_eq!(
        done_rows(&app),
        ["Claude Code in tab 1", "Claude Code in tab 2"]
    );
}

/// A UNIQUE ROW NAMES NO PLACE (§12.4.1): `Claude Code` beside `lint` is
/// named by its label alone. NEGATIVE CONTROL (as run): a pass that names
/// every row's place prints `Claude Code in tab 1` and `lint in tab 2`.
#[test]
fn a_unique_row_names_no_place() {
    let (app, _) = two_landed_workers("Claude Code", "lint");
    assert_eq!(done_rows(&app), ["Claude Code", "lint"]);
}

/// TWO PANES OF ONE TAB WITH ONE LABEL SAY WHICH PANE (§12.4.9): two
/// landed workers both titled `Claude Code`, side by side in tab 1, where
/// `session_place` names no pane (no agent runs in either), are told apart by
/// the pane every pane counts in (`pane_place`): `Claude Code in tab 1,
/// <pane>`, two labels. NEGATIVE CONTROL (as run): `place_alike_rows` without
/// its pane pass gives `Claude Code in tab 1` twice.
#[test]
fn two_panes_of_one_tab_with_one_label_say_which_pane() {
    let mut app = App::headless_for_test();
    let right = app.split_active_stub_tab(WindowId(0));
    land_worker(&mut app, 0, "Claude Code");
    land_worker(&mut app, right, "Claude Code");
    let rows = done_rows(&app);
    assert_ne!(rows[0], rows[1], "told apart: {rows:?}");
    for row in &rows {
        assert!(row.starts_with("Claude Code in tab 1, "), "{rows:?}");
    }
}

/// A host that supervises nothing, shut down on drop.
struct TestHost(crate::harness_host::HostHandle);

impl Drop for TestHost {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

/// An App whose session 0 runs Claude Code under its own word `word`,
/// claimed by this host, in window 0's first tab, a second tab in front.
/// Its sid, and the host.
fn desk(word: &str) -> (App, String, TestHost) {
    let mut app = App::headless_for_test();
    let host = TestHost(crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    ));
    app.harness = Some(host.0.clone());
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    {
        let mut tl = ctx.timeline.lock().expect("timeline");
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    ctx.meta
        .lock()
        .expect("meta")
        .set("autopilot", Some(word.into()));
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claimed");
    let id = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(id));
    (app, ctx.self_id.as_str().to_string(), host)
}

/// Session `sid`'s loop tells `face` through the host's wake.
fn tell(app: &mut App, sid: &str, face: Option<TaskFace>) {
    app.apply_harness_fact(sid, crate::HarnessFact::Task(face));
}

/// The hover's who-drives words for session 0, as composed now.
fn driven(app: &App) -> Option<String> {
    app.presence_hover(0, None).and_then(|hover| {
        hover
            .split(crate::native_ui::FACT_SEPARATOR)
            .flat_map(|part| part.lines())
            .find(|part| part.starts_with("driven by aterm"))
            .map(str::to_string)
    })
}

/// Session 0's tab's hover (window 0's first tab) as the strip was last
/// stamped with it.
fn pushed(_app: &App) -> String {
    let tips = crate::work_counts::last_tooltips_pushed(WindowId(0)).expect("a stamp");
    tips.first().cloned().flatten().unwrap_or_default()
}

/// THE HOVER NAMES THE CHIP'S FACE (§12.4.1, "one helper; no clock"): under
/// Land, open and unknown read `driven by aterm (land)`, landed `(land,
/// landed)` with no clock, a person's own exchange `(land, quiet)`; under
/// Cruise `(cruise)`, and a timed Cruise names its time `(cruise, lands at
/// HH:MM)`. NEGATIVE CONTROL (as run): main's `view.wire()` alone prints
/// `driven by aterm (land)` for the landed face.
#[test]
fn the_hover_names_the_chips_face() {
    let (mut app, sid, _host) = desk("land");
    assert_eq!(driven(&app).as_deref(), Some("driven by aterm (land)"));
    tell(&mut app, &sid, Some(TaskFace::Open));
    assert_eq!(driven(&app).as_deref(), Some("driven by aterm (land)"));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(
        driven(&app).as_deref(),
        Some("driven by aterm (land, landed)"),
        "no clock: the hover is read long after its push"
    );
    tell(&mut app, &sid, Some(TaskFace::Theirs));
    assert_eq!(
        driven(&app).as_deref(),
        Some("driven by aterm (land, quiet)")
    );

    let (mut cruise, _, _host) = desk("cruise");
    assert_eq!(driven(&cruise).as_deref(), Some("driven by aterm (cruise)"));
    meta(&mut cruise, "set autopilot cruise until=+2h");
    let timed = driven(&cruise).expect("driven");
    assert!(
        timed.starts_with("driven by aterm (cruise, lands ") && timed.ends_with(')'),
        "{timed}"
    );
}

/// The wire's `meta` on session 0, its wake applied where it changed.
fn meta(app: &mut App, rest: &str) {
    let session = app.pool.get(0).expect("session 0");
    let (term, ctx) = (session.term.clone(), session.ctx.clone());
    let (reply, changed) = crate::control::cmd_meta(&term, &app.store, 0, &ctx, rest);
    assert!(reply.starts_with("OK"), "{reply}");
    if changed {
        app.on_meta_changed(0);
    }
}

/// THE HOVER FOLLOWS THE FACE (§12.3.6's `the_hover_follows_the_hold_and_the
/// face`, 4a's half; §12.3.3 item 3): only the face moves — land to landed
/// with no work, under the 30 s floor, so no end is told and no `•` marks
/// the tab — and the strip is stamped again: the hover pushed reads `(land,
/// landed)`; someone else's turn (theirs) stamps `(land, quiet)`. NEGATIVE
/// CONTROL (as run): `apply_task_face` without the restamp leaves the pushed
/// hover at `driven by aterm (land)`.
#[test]
fn the_hover_follows_the_face() {
    let (mut app, sid, _host) = desk("land");
    tell(&mut app, &sid, Some(TaskFace::Open));
    let _ = app.refresh_window_tabs(WindowId(0));
    assert!(
        pushed(&app).contains("driven by aterm (land)"),
        "{}",
        pushed(&app)
    );
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert!(!app.end_unseen(0), "PRECONDITION: under the floor, no end");
    assert!(
        pushed(&app).contains("driven by aterm (land, landed)"),
        "{}",
        pushed(&app)
    );
    tell(&mut app, &sid, Some(TaskFace::Theirs));
    assert!(
        pushed(&app).contains("driven by aterm (land, quiet)"),
        "{}",
        pushed(&app)
    );
}

/// THE HOVER FOLLOWS THE TIMER (§12.4.1's `(cruise, lands at 18:00)`): a
/// `meta set autopilot cruise until=…` stamps the strip with the timer's
/// words, and its cancel stamps `(cruise)` again. NEGATIVE CONTROL (as run):
/// without the restamp where the timer's index moves, the meta wake's own
/// stamp, made before the timer is indexed, leaves `(cruise)` on the hover.
#[test]
fn the_hover_follows_the_timer() {
    let (mut app, _, _host) = desk("cruise");
    let _ = app.refresh_window_tabs(WindowId(0));
    assert!(
        pushed(&app).contains("driven by aterm (cruise)"),
        "{}",
        pushed(&app)
    );
    meta(&mut app, "set autopilot cruise until=+2h");
    assert!(
        pushed(&app).contains("driven by aterm (cruise, lands "),
        "{}",
        pushed(&app)
    );
    meta(&mut app, "set autopilot cruise");
    assert!(
        pushed(&app).contains("driven by aterm (cruise)"),
        "{}",
        pushed(&app)
    );
}

/// THE MENU BAR FOLLOWS THE FACE (§12.4.3, item 1): a plain Land tab that
/// lands, under the floor (no end, no `•`), is counted `1 landed` on This Mac
/// at once — the face's move dirties the glance — and someone else's turn
/// takes it off again. NEGATIVE CONTROLS (as run): `status_session_row`
/// without `landed` counts no landing (`here.landed` 0, not 1);
/// `apply_task_face` without its restamp owes the glance no rebuild.
#[test]
fn this_mac_counts_a_landed_tab_that_is_no_worker() {
    let (mut app, sid, _host) = desk("land");
    tell(&mut app, &sid, Some(TaskFace::Open));
    let _ = app.take_status_rebuild();
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert!(app.take_status_rebuild(), "the glance owes a rebuild");
    assert_eq!(app.status_glance().here.landed, 1);
    tell(&mut app, &sid, Some(TaskFace::Theirs));
    assert!(app.take_status_rebuild(), "the glance owes a rebuild");
    assert_eq!(app.status_glance().here.landed, 0);
}

/// A LANDING TOLD UNDER CRUISE IS NO LANDED TAB (§12.4.9: `landed` is the
/// chip's own fold): a Land tab that landed counts `1 landed` and its hover
/// says `(land, landed)`; set to Cruise, its glass still holds the landed
/// face (`status task=landed`), but the chip wears `∞`, so This Mac counts no
/// landing and the hover says `(cruise)`. NEGATIVE CONTROL (as run):
/// `chip_face_landed` reading the glass's face alone keeps `here.landed` at 1.
#[test]
fn a_landing_under_cruise_is_not_counted_landed() {
    let (mut app, sid, _host) = desk("land");
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(app.status_glance().here.landed, 1);
    assert_eq!(
        driven(&app).as_deref(),
        Some("driven by aterm (land, landed)")
    );
    meta(&mut app, "set autopilot cruise");
    assert_eq!(
        app.chip_glass(0).task,
        Some(TaskFace::Landed),
        "PRECONDITION: the glass keeps the landed face"
    );
    assert_eq!(app.status_glance().here.landed, 0, "the chip wears cruise");
    assert_eq!(driven(&app).as_deref(), Some("driven by aterm (cruise)"));
    meta(&mut app, "set autopilot land");
    assert_eq!(app.status_glance().here.landed, 1, "back under Land");
}

/// A TIMER ON ANOTHER DAY NAMES ITS DAY (§12.4.1, the SESSIONS state line's
/// form): a timed Cruise that lands today reads `cruise, lands at 18:00`; one
/// that lands on another day `cruise, lands Sat 18:00`, never a bare
/// `18:00` that would read as today's. NEGATIVE CONTROL (as run): the
/// other-day arm worded as today's prints `cruise, lands at 18:00` for both.
#[test]
fn a_timer_on_another_day_names_its_day() {
    use crate::claude_lights::{ChipFace, ChipGlass, Clock, LandAtGlass, Weekday};
    let glass = |other_day| ChipGlass {
        land_at: Some(LandAtGlass {
            clock: Clock::of_minute(18 * 60),
            other_day,
        }),
        ..ChipGlass::default()
    };
    let words = |other_day| {
        crate::app_presence::driven_words(ChipFace::CruiseUntil, &glass(other_day), None)
    };
    assert_eq!(words(None).as_deref(), Some("cruise, lands at 18:00"));
    assert_eq!(
        words(Some(Weekday::Sat)).as_deref(),
        Some("cruise, lands Sat 18:00")
    );
}

/// 2026-10-16T19:00:00Z in unix milliseconds: the timer tests' wall.
const WALL_MS: i64 = 1_792_177_200_000;

/// THE HOVER FOLLOWS A COMMIT'S REBUILD (§9.3.6 item 3, §12.4.1): a
/// seamless update's successor drops the `MetaChanged` of a timer stored
/// before its Commit, so the hover reads `(cruise)` until the Commit's
/// rebuild indexes it, which stamps the strip with `(cruise, lands …)`; a
/// session that both had an entry and stores one is stamped once.
/// NEGATIVE CONTROLS (as run): `rebuild_autopilot_timers` without its
/// restamp leaves the pushed hover at `(cruise)`; without its dedupe it
/// stamps the strip twice.
#[test]
fn the_hover_follows_a_commits_rebuild() {
    crate::autopilot_timers::set_fake_clocks(Some((std::time::Instant::now(), WALL_MS)));
    let store = |app: &App| {
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        assert!(
            crate::session_timeline::apply_autopilot(
                &ctx,
                Some("cruise".into()),
                Some(WALL_MS + 3_600_000),
                aterm_slog::Actor::Ctl,
                None,
            )
            .moved
        );
    };
    // The words: stored while the wake is dropped, then the Commit.
    let (mut app, _, _host) = desk("cruise");
    app.handoff_successor = true;
    app.incoming_handoff_pending = true;
    store(&app);
    let _ = app.refresh_window_tabs(WindowId(0));
    assert!(
        pushed(&app).contains("driven by aterm (cruise)"),
        "{}",
        pushed(&app)
    );
    app.incoming_handoff_pending = false;
    app.settle_autopilot_commit();
    assert!(
        pushed(&app).contains("driven by aterm (cruise, lands "),
        "{}",
        pushed(&app)
    );
    // Once: an entry a graft indexed before the Commit, and the stored one.
    let (mut app, _, _host) = desk("cruise");
    app.handoff_successor = true;
    app.incoming_handoff_pending = true;
    store(&app);
    app.index_autopilot_timer(0, Some(crate::autopilot_timers::ClosedOver::Aterm));
    app.incoming_handoff_pending = false;
    let _ = crate::work_counts::take_tooltip_pushes();
    app.settle_autopilot_commit();
    assert_eq!(
        crate::work_counts::take_tooltip_pushes(),
        1,
        "window 0's strip, stamped once"
    );
    crate::autopilot_timers::set_fake_clocks(None);
}

/// THE HOVER FOLLOWS A FIRE THAT FOUND THE TIMER MOVED (§9.3.6, §12.4.1):
/// a timer re-armed over the wire, its wake not yet applied, is found moved
/// at the old instant's fire and indexed again at the new one; the strip is
/// stamped with the new time. NEGATIVE CONTROL (as run): the `Fired::Moved`
/// arm without its restamp leaves the old time on the pushed hover.
#[test]
fn the_hover_follows_a_fire_that_found_the_timer_moved() {
    let (mut app, _, _host) = desk("cruise");
    let mono = std::time::Instant::now();
    let land_at = WALL_MS + 120_000;
    crate::autopilot_timers::set_fake_clocks(Some((mono, WALL_MS)));
    assert!(app.set_session_autopilot_timed(
        0,
        Some(aterm_agent::supervise::Autopilot::Cruise),
        Some(land_at),
        crate::autopilot::Surface::Menu
    ));
    let before = pushed(&app);
    assert!(
        before.contains("driven by aterm (cruise, lands "),
        "{before}"
    );
    let later = land_at + 600_000;
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    assert!(
        crate::session_timeline::apply_autopilot(
            &ctx,
            Some("cruise".into()),
            Some(later),
            aterm_slog::Actor::Ctl,
            None,
        )
        .moved
    );
    crate::autopilot_timers::set_fake_clocks(Some((
        mono + std::time::Duration::from_secs(150),
        land_at,
    )));
    app.expire_autopilot_timers();
    assert_eq!(
        app.autopilot_timers.entry(0).map(|e| e.land_at_ms),
        Some(later),
        "PRECONDITION: indexed again at the new instant"
    );
    let now = driven(&app).expect("driven");
    assert_ne!(
        before.lines().find(|l| l.starts_with("driven by aterm")),
        Some(now.as_str()),
        "PRECONDITION: ten minutes later reads another time"
    );
    assert!(pushed(&app).contains(&now), "{}", pushed(&app));
    crate::autopilot_timers::set_fake_clocks(None);
}

/// A TIMED WRITE STAMPS EACH STRIP ONCE (§12.4.3's cost: the hover at the
/// strip's push): a `meta set autopilot cruise until=…` over the wire, and the
/// same write from the window's menu, each stamp the strip that shows the
/// session as many times as a write of no timer does (`cruise` to `land`),
/// with the timer's words in it. NEGATIVE CONTROL (as run): the index's own
/// restamp beside the meta move's (`on_meta_changed` stamping before it
/// indexes, `autopilot_pair_fanned_out` after) stamps it once more.
#[test]
fn a_timed_write_stamps_each_strip_once() {
    let (mut app, _, _host) = desk("cruise");
    let _ = crate::work_counts::take_tooltip_pushes();
    meta(&mut app, "set autopilot land");
    let plain = crate::work_counts::take_tooltip_pushes();
    assert!(plain >= 1, "PRECONDITION: a meta move stamps the strip");
    meta(&mut app, "set autopilot cruise until=+2h");
    assert_eq!(crate::work_counts::take_tooltip_pushes(), plain, "the wire");
    assert!(
        pushed(&app).contains("driven by aterm (cruise, lands "),
        "{}",
        pushed(&app)
    );
    // The window's own writer: a plain write with no timer before or after
    // it (`cruise` to `land`), then the timed one.
    assert!(app.set_session_autopilot(
        0,
        Some(aterm_agent::supervise::Autopilot::Cruise),
        crate::autopilot::Surface::Menu
    ));
    let _ = crate::work_counts::take_tooltip_pushes();
    assert!(app.set_session_autopilot(
        0,
        Some(aterm_agent::supervise::Autopilot::Land),
        crate::autopilot::Surface::Menu
    ));
    let plain = crate::work_counts::take_tooltip_pushes();
    assert!(plain >= 1, "PRECONDITION: a window write stamps the strip");
    assert!(app.set_session_autopilot_timed(
        0,
        Some(aterm_agent::supervise::Autopilot::Cruise),
        Some(crate::autopilot_timers::wall_ms() + 3_600_000),
        crate::autopilot::Surface::Menu
    ));
    assert_eq!(crate::work_counts::take_tooltip_pushes(), plain, "the menu");
    assert!(
        pushed(&app).contains("driven by aterm (cruise, lands "),
        "{}",
        pushed(&app)
    );
}

/// THE HOVER ASKS NO REDRAW WHERE THE STRIP DID NOT MOVE (§12.4.4: per frame,
/// nothing): a hover is the toolbar's tooltip, drawn by no frame, so a face
/// move that restamps it (land to landed under the floor) asks the window
/// for no frame while the strip's fingerprint is the one last presented.
/// NEGATIVE CONTROL (as run): `restamp_for_hover` asking every window it
/// stamps to redraw (as `refresh_tab_chrome_windows` does) asks one.
#[test]
fn the_hover_asks_no_redraw_where_the_strip_did_not_move() {
    let (mut app, sid, _host) = desk("land");
    // A surface that presents (`scripted_redraw_fixture`), so the frame
    // below stamps the strip's fingerprint it drew.
    app.windows.get_mut(&WindowId(0)).expect("window").present = Some(crate::PresentTarget::Cpu {
        surface: crate::present::CpuSurface::Scripted(Default::default()),
    });
    tell(&mut app, &sid, Some(TaskFace::Open));
    app.redraw_window(WindowId(0));
    assert!(
        app.windows
            .get(&WindowId(0))
            .is_some_and(|ws| ws.last_present.is_some()),
        "PRECONDITION: a frame was presented"
    );
    let _ = crate::work_counts::take_hover_redraws();
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert!(
        pushed(&app).contains("driven by aterm (land, landed)"),
        "PRECONDITION: the hover was stamped: {}",
        pushed(&app)
    );
    assert_eq!(crate::work_counts::take_hover_redraws(), 0);
}
