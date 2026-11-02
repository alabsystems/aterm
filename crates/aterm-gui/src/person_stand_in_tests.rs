// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless `person` stand-in and `status` end-marker tests.

use std::sync::Arc;

use aterm_agent::harness::acts::TaskFace;

use crate::control::control_media::{PERSON_DENIED, PERSON_NEEDS_DEBUG, PersonAsked, parse_person};
use crate::presence::ChipLevel;
use crate::{App, WindowId};

const SEC: u64 = 1_000;

struct TestHost(crate::harness_host::HostHandle);

impl Drop for TestHost {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

fn host() -> TestHost {
    TestHost(crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    ))
}

fn claim(app: &App, session: u64) {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claimed");
}

fn desk() -> (App, String, TestHost) {
    let mut app = App::headless_for_test();
    let host = host();
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
        .set("autopilot", Some("land".into()));
    claim(&app, 0);
    let id = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(id));
    app.frontmost_window = Some(WindowId(0));
    assert!(
        !crate::messages_host::band_on_screen(&app.windows[&WindowId(0)]),
        "PRECONDITION: a headless window is on no screen"
    );
    (app, ctx.self_id.as_str().to_string(), host)
}

fn publish(app: &App, word: &'static str) -> bool {
    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .timeline
        .lock()
        .expect("timeline")
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

fn tell(app: &mut App, sid: &str, face: Option<TaskFace>) {
    app.apply_harness_fact(sid, crate::HarnessFact::Task(face));
}

fn end_after(app: &mut App, sid: &str, ms: u64) {
    tell(app, sid, Some(TaskFace::Open));
    assert!(publish(app, "busy"), "busy published");
    app.note_harness_phase(0);
    crate::turn_ledger::advance_for_test(ms);
    assert!(publish(app, "idle"), "idle published");
    app.note_harness_phase(0);
    tell(app, sid, Some(TaskFace::Landed));
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

fn field(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(prefix.as_str()))
        .map(str::to_owned)
}

fn look(app: &mut App, window: Option<u64>) -> Result<String, String> {
    app.person_stand_in(PersonAsked::Look(window))
}

#[test]
fn the_person_verb_reads_its_forms_and_refuses_unarmed_and_in_release() {
    let armed = |rest: &str| parse_person(rest, true, || true);
    assert_eq!(armed("look 0"), Ok(PersonAsked::Look(Some(0))));
    assert_eq!(armed(" look  12 "), Ok(PersonAsked::Look(Some(12))));
    assert_eq!(armed("look off"), Ok(PersonAsked::Look(None)));
    assert_eq!(
        armed("menubar press 3.0"),
        Ok(PersonAsked::Press(
            crate::status_item::MenuAt::parse("3.0").expect("a path")
        ))
    );
    for junk in [
        "",
        "look",
        "look -1",
        "look +1",
        "look 0 1",
        "look front",
        "menubar",
        "menubar press",
        "menubar press 1.",
        "menubar press x",
        "menubar click 1",
        "invoke TakeOver",
    ] {
        let refusal = armed(junk).expect_err(junk);
        assert!(refusal.starts_with("usage: person "), "{junk:?}: {refusal}");
    }
    for rest in ["look 0", "look off", "menubar press 0", "junk"] {
        assert_eq!(
            parse_person(rest, true, || false),
            Err(PERSON_DENIED.to_string()),
            "{rest}"
        );
        assert_eq!(
            parse_person(rest, false, || panic!("a release build reads no arming")),
            Err(PERSON_NEEDS_DEBUG.to_string()),
            "{rest}"
        );
    }
    assert!(PERSON_DENIED.starts_with("denied: person is a development build's stand-in"));
    assert!(PERSON_DENIED.ends_with("(ATERM_DEBUG_PERSON_INVOKE=1 at launch)"));
}

#[test]
fn a_stand_in_look_sees_the_front_tabs_end_by_the_one_rule() {
    let (mut app, sid, _host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    assert!(app.end_unseen(0), "PRECONDITION: a background end stands");
    assert_eq!(marks(&app)[0], ChipLevel::Done);
    assert_eq!(app.status_glance().done.len(), 1);
    assert_eq!(app.status_glance().button_title(), "\u{276f}");
    app.switch_tab_in(WindowId(0), 0);
    assert!(app.end_unseen(0), "a headless window: nobody looks");
    app.switch_tab_in(WindowId(0), 1);
    assert_eq!(look(&mut app, Some(9)), Err("no window 9".to_string()));
    assert!(app.end_unseen(0), "a refused look sees nothing");

    assert_eq!(
        look(&mut app, Some(0)),
        Ok("person look window=0".to_string())
    );
    assert!(app.end_unseen(0), "a look sees the front tab alone");
    assert_eq!(marks(&app)[0], ChipLevel::Done, "the `•` stands");
    assert_eq!(app.status_glance().done.len(), 1, "the row stands");
    assert_eq!(app.status_glance().button_title(), "\u{276f}");

    app.take_status_rebuild();
    app.switch_tab_in(WindowId(0), 0);
    assert!(!app.end_unseen(0), "seen by the one rule");
    assert!(app.take_status_rebuild(), "the look dirties the glance");
    let g = app.status_glance();
    assert!(g.done.is_empty(), "the row goes: {:?}", g.done);
    assert_eq!(g.button_title(), "\u{276f}", "and `❯•`");
    assert_eq!(marks(&app)[0], ChipLevel::Off);

    end_after(&mut app, &sid, 40 * SEC);
    assert!(!app.end_unseen(0), "seen as it lands where someone looks");

    assert_eq!(look(&mut app, None), Ok("person look off".to_string()));
    end_after(&mut app, &sid, 40 * SEC);
    assert!(app.end_unseen(0), "nobody looks: it stands");
    assert_eq!(app.status_glance().done.len(), 1, "a Done for you row");
}

#[test]
fn a_stand_in_look_sees_the_front_tab_at_once() {
    let (mut app, sid, _host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    app.switch_tab_in(WindowId(0), 0);
    assert!(app.end_unseen(0), "in front, nobody looks: it stands");
    assert_eq!(app.status_glance().done.len(), 1);
    assert_eq!(app.status_glance().button_title(), "\u{276f}");
    app.take_status_rebuild();

    assert_eq!(
        look(&mut app, Some(0)),
        Ok("person look window=0".to_string())
    );
    assert!(!app.end_unseen(0), "seen at once");
    assert!(app.take_status_rebuild(), "the glance told");
    let g = app.status_glance();
    assert!(g.done.is_empty(), "the row goes: {:?}", g.done);
    assert_eq!(g.button_title(), "\u{276f}", "and `❯•`");
}

#[test]
fn a_look_at_a_window_behind_sees_nothing() {
    let (mut app, sid, _host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    assert!(app.end_unseen(0), "PRECONDITION: a background end stands");
    let w1 = app
        .open_active_session_in_new_window_logical()
        .expect("a second window");
    assert_eq!(
        app.frontmost_window,
        Some(w1),
        "PRECONDITION: window 1 in front"
    );
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .band_on_screen_for_test = true;
    app.switch_tab_in(WindowId(0), 0);
    assert!(app.end_unseen(0), "a window behind is looked at by nobody");
    app.switch_tab_in(WindowId(0), 1);
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .band_on_screen_for_test = false;

    assert_eq!(
        look(&mut app, Some(0)),
        Ok("person look window=0".to_string())
    );
    app.switch_tab_in(WindowId(0), 0);
    assert!(app.end_unseen(0), "a look at a window behind sees nothing");
    assert_eq!(app.status_glance().done.len(), 1, "the row stands");

    let _ = app.close_window_logical(w1);
    assert_eq!(
        app.frontmost_window,
        Some(WindowId(0)),
        "PRECONDITION: window 0 in front again"
    );
    app.switch_tab_in(WindowId(0), 1);
    app.switch_tab_in(WindowId(0), 0);
    assert!(!app.end_unseen(0), "in front, looked at: seen");
    assert!(app.status_glance().done.is_empty(), "the row goes");
}

/// The `statusmenu` line `chrome` prints after the one starting `head`, and
/// its path (macOS: the menu-bar item's own platform).
#[cfg(target_os = "macos")]
fn row_after(app: &App, head: &str) -> (String, crate::status_item::MenuAt) {
    let lines = app.status_item_chrome_lines();
    let at = lines
        .iter()
        .position(|l| l.starts_with(head))
        .unwrap_or_else(|| panic!("no {head:?} line: {lines:#?}"));
    let line = lines[at + 1].clone();
    let path = line
        .rsplit_once(" at=")
        .and_then(|(_, at)| crate::status_item::MenuAt::parse(at))
        .unwrap_or_else(|| panic!("no path: {line}"));
    (line, path)
}

/// A STAND-IN PRESS GOES WHERE A CLICK GOES (§12.5.6): the Done for you row
/// `chrome` prints, pressed by its path, posts the row's action as the
/// AppKit relay posts a click; that wake's dispatch brings the landed tab
/// to the front and its end is seen, and the row and `❯•` go — a click's Go
/// is a look by its own act (§11.4.3, item 4), look or not.
#[cfg(target_os = "macos")]
#[test]
fn a_stand_in_press_goes_where_a_click_goes() {
    let (mut app, sid, _host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    assert!(app.end_unseen(0), "PRECONDITION: a background end stands");
    let (line, at) = row_after(&app, "statusmenu \"Done for you (1)\"");
    let label = line
        .strip_prefix("statusmenu ")
        .and_then(|l| l.split_once("\" enabled=yes"))
        .map(|(label, _)| format!("{label}\""))
        .unwrap_or_else(|| panic!("an action row: {line}"));
    // The row's age is said from the wall clock, so it may tick between the
    // read and the press ("landed 0s", then "landed 1s"): the row is the same.
    let unaged = |said: &str| {
        said.rsplit_once(" landed ")
            .map_or_else(|| said.to_string(), |(row, _)| row.to_string())
    };
    let mut posted = Vec::new();
    let pressed = app.person_menubar_press(&at, &mut |wake| {
        posted.push(wake);
        true
    });
    assert!(label.contains(" landed "), "the row says its age: {label}");
    assert_eq!(
        pressed.map(|said| unaged(&said)),
        Ok(unaged(&format!("pressed {label}")))
    );
    assert!(app.end_unseen(0), "nothing ran before the wake");
    assert_eq!(posted.len(), 1, "one click posted: {posted:?}");
    for wake in posted {
        let crate::Wake::StatusAction { action } = wake else {
            panic!("a status action: {wake:?}");
        };
        app.dispatch_status_action(action);
    }
    assert_eq!(
        app.focused_session_id(WindowId(0)),
        Some(0),
        "the landed tab in front"
    );
    assert!(!app.end_unseen(0), "its end seen");
    let g = app.status_glance();
    assert!(g.done.is_empty(), "the row goes: {:?}", g.done);
    assert_eq!(g.button_title(), "\u{276f}");

    let lines = app.status_item_chrome_lines();
    let info = lines
        .iter()
        .find(|l| l.contains(" enabled=no at="))
        .unwrap_or_else(|| panic!("an information line: {lines:#?}"));
    let info_at = info
        .rsplit_once(" at=")
        .and_then(|(_, at)| crate::status_item::MenuAt::parse(at))
        .expect("a path");
    let mut posted = Vec::new();
    let mut post = |wake| {
        posted.push(wake);
        true
    };
    assert_eq!(
        app.person_menubar_press(&info_at, &mut post),
        Err("not an action".to_string())
    );
    let past = crate::status_item::MenuAt::parse(&(lines.len() + 5).to_string()).expect("a path");
    assert_eq!(
        app.person_menubar_press(&past, &mut post),
        Err(format!("no row at {}", lines.len() + 5))
    );
    assert!(posted.is_empty(), "nothing posted: {posted:?}");

    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-person-press-")
        .tempdir()
        .expect("exclusive result dir");
    let path = dir.path().join("result.md");
    std::fs::write(&path, "# the worker's result\n").expect("result");
    for state in [
        crate::task_record::TaskState::Working,
        crate::task_record::TaskState::Done,
    ] {
        let mut mark = crate::task_record::TaskMark::new(state);
        if state == crate::task_record::TaskState::Done {
            mark.result_bytes = 22;
            mark.result_path = Some(path.clone());
        }
        crate::task_record::mark_session_task(&ctx, 0, &mark).expect("marked");
        app.on_task_changed(0);
    }
    app.switch_tab_in(WindowId(0), 1);
    let entered = app
        .status_glance()
        .reads
        .first()
        .map(|r| r.entered)
        .expect("PRECONDITION: a result to read");
    assert_ne!(
        app.done_seen.get(&0),
        Some(&entered),
        "PRECONDITION: unread"
    );
    let (line, at) = row_after(&app, "statusmenu \"Done for you (1)\"");
    let mut posted = Vec::new();
    let pressed = app.person_menubar_press(&at, &mut |wake| {
        posted.push(wake);
        true
    });
    assert!(pressed.is_ok(), "{line}: {pressed:?}");
    for wake in posted {
        let crate::Wake::StatusAction { action } = wake else {
            panic!("a status action: {wake:?}");
        };
        assert!(
            matches!(action, crate::status_item::StatusAction::ReadResult(_)),
            "{line}: {action:?}"
        );
        app.dispatch_status_action(action);
    }
    assert_eq!(
        app.done_seen.get(&0),
        Some(&entered),
        "the result opened, its done seen"
    );
}

#[test]
fn status_says_the_end_seen_and_cheered_apart() {
    let (mut app, sid, _host) = desk();
    let status = |app: &App| app.session_status_record(0).expect("status");
    let ends = |line: &str| {
        (
            field(line, "end"),
            field(line, "end_seen"),
            field(line, "end_cheered"),
        )
    };
    let dash = || Some("-".to_string());
    assert_eq!(ends(&status(&app)), (dash(), dash(), dash()), "no end");
    end_after(&mut app, &sid, 45 * SEC);
    let line = status(&app);
    let at_ms = app
        .pool
        .get(0)
        .expect("session 0")
        .ctx
        .timeline
        .lock()
        .expect("timeline")
        .task_face_at(TaskFace::Landed)
        .expect("the landing's instant");
    let at = Some((at_ms / 1000).to_string());
    let (zero, one) = (Some("0".to_string()), Some("1".to_string()));
    assert_eq!(
        ends(&line),
        (at.clone(), zero.clone(), zero.clone()),
        "{line}"
    );
    assert_eq!(field(&line, "task").as_deref(), Some("landed"), "{line}");
    assert_eq!(field(&line, "pause").as_deref(), Some("-"), "{line}");
    assert!(field(&line, "task_state").is_some(), "{line}");
    assert!(
        line.contains(" land_since=- held=- waiting=- land_asked=- landed_head=- landed_on=- landed_leg=- landed_repo=- end="),
        "right after `pause=`, Land's hold (slice 3c), the stop (3a) and the outage wait (12b): {line}"
    );
    let serial = app.harness_glass[&0].end().expect("an end").serial;
    assert!(
        app.harness_glass
            .get_mut(&0)
            .expect("the glass")
            .cheer_end(serial)
    );
    assert_eq!(
        ends(&status(&app)),
        (at.clone(), zero.clone(), one.clone()),
        "cheered apart from seen"
    );
    assert!(look(&mut app, Some(0)).is_ok());
    app.switch_tab_in(WindowId(0), 0);
    assert_eq!(ends(&status(&app)), (at, one.clone(), one), "seen");
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    assert!(
        crate::session_timeline::release_supervisor(
            &ctx,
            None,
            crate::metrics::now_us(),
            aterm_slog::Actor::Aterm,
        ),
        "PRECONDITION: this host's claim was live"
    );
    assert!(
        app.harness_glass[&0].end().is_some(),
        "PRECONDITION: the end still stands on the glass"
    );
    let line = status(&app);
    assert_eq!(field(&line, "task").as_deref(), Some("-"), "{line}");
    assert_eq!(
        ends(&line),
        (dash(), dash(), dash()),
        "no claim, no end told: {line}"
    );
    let other = app.next_session_id - 1;
    assert_eq!(
        ends(&app.session_status_record(other).expect("status")),
        (dash(), dash(), dash()),
        "no supervised agent, no face told"
    );
}

#[test]
fn status_says_a_carried_ends_cheer_is_spent() {
    let (mut app, _sid, _host) = desk();
    assert!(
        app.harness_glass.entry(0).or_default().carry_end(
            crate::harness_glass::EndFace::Landed,
            None,
            false
        ),
        "PRECONDITION: carried where no end stands"
    );
    let line = app.session_status_record(0).expect("status");
    assert_eq!(field(&line, "end_seen").as_deref(), Some("0"), "{line}");
    assert_eq!(
        field(&line, "end_cheered").as_deref(),
        Some("1"),
        "spent: {line}"
    );
}
