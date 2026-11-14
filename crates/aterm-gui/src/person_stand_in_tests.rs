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
        !crate::messages_host::window_on_screen(&app.windows[&WindowId(0)]),
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
        armed("menubar press 3"),
        Ok(PersonAsked::Press(
            crate::status_item::MenuAt::parse("3").expect("a place")
        ))
    );
    assert_eq!(armed("menubar click"), Ok(PersonAsked::Click));
    assert_eq!(armed("menubar open"), Ok(PersonAsked::Open));
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
        "menubar press 3.0",
        "menubar press x",
        "menubar click 1",
        "menubar open 1",
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

    app.take_status_rebuild();
    app.switch_tab_in(WindowId(0), 0);
    assert!(!app.end_unseen(0), "seen by the one rule");
    assert!(
        app.take_status_rebuild(),
        "the look owes the Agents surfaces a rebuild"
    );
    assert_eq!(marks(&app)[0], ChipLevel::Off);

    end_after(&mut app, &sid, 40 * SEC);
    assert!(!app.end_unseen(0), "seen as it lands where someone looks");

    assert_eq!(look(&mut app, None), Ok("person look off".to_string()));
    end_after(&mut app, &sid, 40 * SEC);
    assert!(app.end_unseen(0), "nobody looks: it stands");
}

#[test]
fn a_stand_in_look_sees_the_front_tab_at_once() {
    let (mut app, sid, _host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    app.switch_tab_in(WindowId(0), 0);
    assert!(app.end_unseen(0), "in front, nobody looks: it stands");
    app.take_status_rebuild();

    assert_eq!(
        look(&mut app, Some(0)),
        Ok("person look window=0".to_string())
    );
    assert!(!app.end_unseen(0), "seen at once");
    assert!(app.take_status_rebuild(), "the Agents surfaces told");
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
        .window_on_screen_for_test = true;
    app.switch_tab_in(WindowId(0), 0);
    assert!(app.end_unseen(0), "a window behind is looked at by nobody");
    app.switch_tab_in(WindowId(0), 1);
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .window_on_screen_for_test = false;

    assert_eq!(
        look(&mut app, Some(0)),
        Ok("person look window=0".to_string())
    );
    app.switch_tab_in(WindowId(0), 0);
    assert!(app.end_unseen(0), "a look at a window behind sees nothing");

    let _ = app.close_window_logical(w1);
    assert_eq!(
        app.frontmost_window,
        Some(WindowId(0)),
        "PRECONDITION: window 0 in front again"
    );
    app.switch_tab_in(WindowId(0), 1);
    app.switch_tab_in(WindowId(0), 0);
    assert!(!app.end_unseen(0), "in front, looked at: seen");
}

/// The place of the `statusmenu` line `chrome` prints for the row titled `title`.
#[cfg(target_os = "macos")]
fn row_titled(app: &App, title: &str) -> crate::status_item::MenuAt {
    let lines = app.status_item_chrome_lines();
    let line = lines
        .iter()
        .find(|l| l.starts_with(&format!("statusmenu {title:?} ")))
        .unwrap_or_else(|| panic!("no {title:?} line: {lines:#?}"));
    line.rsplit_once(" at=")
        .and_then(|(_, at)| crate::status_item::MenuAt::parse(at))
        .unwrap_or_else(|| panic!("no place: {line}"))
}

/// A STAND-IN PRESS GOES WHERE A CLICK GOES: a level's row `chrome` prints, pressed by its
/// place, posts the row's action as the AppKit relay posts a click, and that wake's dispatch
/// stores the level at once; a grey line and a place past the menu post nothing. The store is
/// this test thread's (`control::store_fleet_access`).
#[cfg(target_os = "macos")]
#[test]
fn a_stand_in_press_goes_where_a_click_goes() {
    use aterm_types::fleet_access::FleetAccess::{Full, ReadOnly};
    let (mut app, _sid, _host) = desk();
    let mut slot = crate::fleet_glance::FleetSlot::default();
    slot.book.in_fleet = true;
    app.fleet_glance = Some(crate::fleet_glance::FleetGlance::over(slot));
    let now = aterm_time::unix_millis().unwrap_or(0);
    let _ = app
        .fleet_traffic
        .set(Some(crate::fleet_traffic::FleetTraffic::over(
            Some(crate::status_item::a_blocked_hour(now)),
            0,
        )));
    let stored = crate::control::fleet_access_now;
    assert_eq!(stored(), ReadOnly, "PRECONDITION: the default");

    let at = row_titled(&app, Full.label());
    let mut posted = Vec::new();
    let pressed = app.person_menubar_press(&at, &mut |wake| {
        posted.push(wake);
        true
    });
    assert_eq!(pressed, Ok(format!("pressed {:?}", Full.label())));
    assert_eq!(stored(), ReadOnly, "nothing ran before the wake");
    assert_eq!(posted.len(), 1, "one click posted: {posted:?}");
    for wake in posted {
        let crate::Wake::StatusAction { action } = wake else {
            panic!("a status action: {wake:?}");
        };
        assert_eq!(
            action,
            crate::status_item::StatusAction::SetFleetAccess(Full)
        );
        app.dispatch_status_action(action);
    }
    assert_eq!(stored(), Full, "a pick in the menu is stored at once");
    let checked = app
        .status_item_chrome_lines()
        .into_iter()
        .filter(|l| l.contains(" checked=yes"))
        .collect::<Vec<_>>();
    assert_eq!(checked.len(), 1, "{checked:#?}");
    assert!(
        checked[0].starts_with(&format!("statusmenu {:?} ", Full.label())),
        "{checked:#?}"
    );

    // The whole record is one press away, and posts its own action.
    let at = row_titled(&app, "All activity\u{2026}");
    let mut posted = Vec::new();
    assert!(
        app.person_menubar_press(&at, &mut |wake| {
            posted.push(wake);
            true
        })
        .is_ok()
    );
    assert!(
        matches!(
            posted[..],
            [crate::Wake::StatusAction {
                action: crate::status_item::StatusAction::AllActivity
            }]
        ),
        "{posted:?}"
    );

    let lines = app.status_item_chrome_lines();
    let info = lines
        .iter()
        .find(|l| l.contains(" enabled=no"))
        .unwrap_or_else(|| panic!("a grey line: {lines:#?}"));
    let info_at = info
        .rsplit_once(" at=")
        .and_then(|(_, at)| crate::status_item::MenuAt::parse(at))
        .expect("a place");
    let mut posted = Vec::new();
    let mut post = |wake| {
        posted.push(wake);
        true
    };
    assert_eq!(
        app.person_menubar_press(&info_at, &mut post),
        Err("not an action".to_string())
    );
    let past = crate::status_item::MenuAt::parse(&(lines.len() + 5).to_string()).expect("a place");
    assert_eq!(
        app.person_menubar_press(&past, &mut post),
        Err(format!("no row at {}", lines.len() + 5))
    );
    assert!(posted.is_empty(), "nothing posted: {posted:?}");
}

/// A STAND-IN OPEN IS THE MENU'S OPEN: `person menubar open` states that the person saw what
/// this Mac's lock refused, so the bar's blocked mark clears, with no menu on the screen.
#[test]
fn a_stand_in_open_clears_the_bars_blocked_mark() {
    let (mut app, _sid, _host) = desk();
    let mut slot = crate::fleet_glance::FleetSlot::default();
    slot.book.in_fleet = true;
    app.fleet_glance = Some(crate::fleet_glance::FleetGlance::over(slot));
    let now = aterm_time::unix_millis().unwrap_or(0);
    let _ = app
        .fleet_traffic
        .set(Some(crate::fleet_traffic::FleetTraffic::over(
            Some(crate::status_item::a_blocked_hour(now)),
            0,
        )));
    let blocked = |app: &App| field(&app.status_item_chrome_lines()[0], "blocked");
    assert_eq!(blocked(&app).as_deref(), Some("yes"));
    assert_eq!(
        app.person_stand_in(PersonAsked::Open),
        Ok("opened the menu".to_string())
    );
    assert_eq!(blocked(&app).as_deref(), Some("no"));
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

/// The `access=` and `shown=` words of the `statusitem` chrome line: the level in force and the
/// level the lock shows.
fn lock_words(app: &App) -> (Option<String>, Option<String>) {
    let line = app.status_item_chrome_lines().swap_remove(0);
    (field(&line, "access"), field(&line, "shown"))
}

fn words(access: &str, shown: &str) -> (Option<String>, Option<String>) {
    (Some(access.to_string()), Some(shown.to_string()))
}

/// The lock on a real App (`docs/DESIGN-fleet-access-lock-2026-11-10.md` §6.3): a click's raise
/// is stored only once its settle has passed, a lower level at once, and the chrome line says
/// both levels throughout. The store is this test thread's (`control::store_fleet_access`).
#[test]
fn the_lock_stores_a_raise_after_its_settle_and_a_lower_level_at_once() {
    use aterm_types::fleet_access::FleetAccess::{Full, ReadOnly, ReadWrite};
    let settle = crate::status_item::SETTLE;
    let (mut app, _sid, _host) = desk();
    let stored = crate::control::fleet_access_now;
    assert_eq!(stored(), ReadOnly, "PRECONDITION: the default");
    assert_eq!(lock_words(&app), words("read", "read"));
    let _ = app.take_status_rebuild();

    let t0 = std::time::Instant::now();
    app.rotate_fleet_access(t0, false);
    assert_eq!(stored(), ReadOnly, "a raise is not stored by its click");
    assert_eq!(lock_words(&app), words("read", "write"));
    assert!(app.take_status_rebuild(), "the lock repaints");
    app.tick_lock_face(t0 + settle / 2);
    assert_eq!(stored(), ReadOnly, "nor before its settle");
    assert!(!app.take_status_rebuild(), "nothing moved");
    app.tick_lock_face(t0 + settle);
    assert_eq!(stored(), ReadWrite);
    assert_eq!(lock_words(&app), words("write", "write"));
    assert!(app.take_status_rebuild());

    // On over Full Access and round to Read Only inside one settle: Full Access is never stored.
    let t1 = t0 + 10 * settle;
    app.rotate_fleet_access(t1, false);
    assert_eq!(lock_words(&app), words("write", "full"));
    app.rotate_fleet_access(t1 + settle / 4, false);
    assert_eq!(stored(), ReadOnly, "a lower level is stored at once");
    assert_eq!(lock_words(&app), words("read", "read"));
    app.tick_lock_face(t1 + 10 * settle);
    assert_eq!(stored(), ReadOnly);

    // A pick in the menu is stored at once, a raise included.
    app.pick_fleet_access(Full);
    assert_eq!(stored(), Full);
    assert_eq!(lock_words(&app), words("full", "full"));
}

/// THE BUTTON'S OWN CLICKS, as AppKit counts them (`status_item::click_act`), on a real App: a
/// quick second click is a multi-click's second release, and it is honoured where it lowers
/// the level or cancels a raise and never where it would raise. From Read and Write, click and
/// click again at once ends at Read Only, stored at that second click: the first click's raise
/// to Full Access is dropped, never left to fall due. CONTROL: the same double-click from Read
/// Only moves one step, to Read and Write, and from Full Access ends at Read Only.
#[test]
fn a_quick_second_click_lowers_at_once_and_never_raises_a_second_step() {
    use aterm_types::fleet_access::FleetAccess::{self, Full, ReadOnly, ReadWrite};
    use aterm_types::status_click::{EventKey, LEFT_MOUSE_UP};
    let settle = crate::status_item::SETTLE;
    let stored = crate::control::fleet_access_now;
    let t0 = std::time::Instant::now();
    // The two releases of a double-click 0.2 s apart, read as the button's handler reads them.
    let double_click = |app: &mut App, at: std::time::Instant| {
        let release = |seconds, clicks| EventKey {
            kind: LEFT_MOUSE_UP,
            flags: 0,
            at: seconds,
            clicks,
        };
        let (first, second) = (release(1.0, 1), release(1.2, 2));
        for (event, stale, at) in [
            (first, None, at),
            (
                second,
                Some(first),
                at + std::time::Duration::from_millis(200),
            ),
        ] {
            match crate::status_item::click_act(Some(event), stale, true) {
                crate::status_item::ClickAct::Rotate { repeated } => {
                    app.rotate_fleet_access(at, repeated);
                }
                crate::status_item::ClickAct::Menu => panic!("a left release in a fleet rotates"),
            }
        }
    };
    let ends_at = |from: FleetAccess| {
        let (mut app, _sid, _host) = desk();
        app.pick_fleet_access(from);
        assert_eq!(stored(), from, "PRECONDITION");
        double_click(&mut app, t0);
        let at_once = stored();
        app.tick_lock_face(t0 + 10 * settle);
        (at_once, stored(), lock_words(&app))
    };
    assert_eq!(
        ends_at(ReadWrite),
        (ReadOnly, ReadOnly, words("read", "read")),
        "the second click closed it at once, and Full Access was never stored"
    );
    assert_eq!(
        ends_at(ReadOnly),
        (ReadOnly, ReadWrite, words("write", "write")),
        "CONTROL: one step, after its settle"
    );
    assert_eq!(
        ends_at(Full),
        (ReadOnly, ReadOnly, words("read", "read")),
        "CONTROL: closed by the first click, and the second raises nothing"
    );
}

/// A level another writer stored (`aterm fleet access`, another window's lock) replaces the one
/// this App holds and cancels its pending raise, at the App's next read of it.
#[test]
fn a_level_stored_elsewhere_cancels_the_locks_pending_raise() {
    use aterm_types::fleet_access::FleetAccess::{Full, ReadOnly};
    let settle = crate::status_item::SETTLE;
    let (mut app, _sid, _host) = desk();
    let t0 = std::time::Instant::now();
    app.pick_fleet_access(Full);
    app.rotate_fleet_access(t0, false);
    assert_eq!(
        lock_words(&app),
        words("read", "read"),
        "PRECONDITION: closed"
    );
    app.rotate_fleet_access(t0 + 10 * settle, false);
    assert_eq!(
        lock_words(&app),
        words("read", "write"),
        "PRECONDITION: a raise is pending"
    );
    // The other writer.
    assert_eq!(crate::control::store_fleet_access(Full, None), Ok(true));
    let _ = app.take_status_rebuild();
    app.reread_fleet_access();
    assert_eq!(lock_words(&app), words("full", "full"));
    assert!(app.take_status_rebuild(), "the lock repaints");
    assert_eq!(crate::control::store_fleet_access(ReadOnly, None), Ok(true));
    app.tick_lock_face(t0 + 20 * settle);
    assert_eq!(
        crate::control::fleet_access_now(),
        ReadOnly,
        "the cancelled raise was never stored"
    );
}

/// A PENDING RAISE IS WRITTEN ONLY OVER THE STATE ITS CLICK READ. Inside the settle the state
/// moves with the level as it was: `aterm fleet access read` names the level in force, or this
/// Mac leaves its fleet (which removes no level at the default), or joins another. No
/// notification need arrive: the lock's own clock reads the state again before it writes, and
/// the raise is dropped. CONTROL: the same click with nothing moved stores its raise.
#[test]
fn a_raise_is_dropped_where_the_state_moved_under_it_with_the_level_as_it_was() {
    use aterm_types::fleet_access::FleetAccess::{ReadOnly, ReadWrite};
    let settle = crate::status_item::SETTLE;
    let stored = crate::control::fleet_access_now;
    let t0 = std::time::Instant::now();
    let clicked = || {
        let (mut app, _sid, _host) = desk();
        app.reread_fleet_access();
        app.rotate_fleet_access(t0, false);
        assert_eq!(
            (stored(), lock_words(&app)),
            (ReadOnly, words("read", "write")),
            "PRECONDITION: a raise is pending at the default"
        );
        app
    };

    let mut app = clicked();
    app.tick_lock_face(t0 + settle);
    assert_eq!(stored(), ReadWrite, "CONTROL: nothing moved");
    assert_eq!(crate::control::store_fleet_access(ReadOnly, None), Ok(true));

    let mut app = clicked();
    crate::control::test_state_moved();
    let _ = app.take_status_rebuild();
    app.tick_lock_face(t0 + settle);
    assert_eq!(stored(), ReadOnly, "the raise was not written");
    assert_eq!(lock_words(&app), words("read", "read"));
    assert!(app.take_status_rebuild(), "the lock repaints closed");
    app.tick_lock_face(t0 + 10 * settle);
    assert_eq!(stored(), ReadOnly, "nor later");

    // The notification, where it does arrive first, drops the raise at once.
    let mut app = clicked();
    crate::control::test_state_moved();
    app.reread_fleet_access();
    assert_eq!(lock_words(&app), words("read", "read"));
    app.tick_lock_face(t0 + settle);
    assert_eq!(stored(), ReadOnly);

    // The store itself refuses a raise asked over a state that is gone, whatever the face
    // believed (a state that moved between the clock's read and its write).
    let over = crate::control::fleet_access_basis();
    crate::control::test_state_moved();
    assert_eq!(
        crate::control::store_fleet_access(ReadWrite, Some(over)),
        Ok(false)
    );
    assert_eq!(stored(), ReadOnly);
}

#[cfg(target_os = "macos")]
#[test]
fn a_stand_in_click_rotates_the_lock_and_the_menus_open_cancels_its_raise() {
    use aterm_types::fleet_access::FleetAccess::ReadOnly;
    let (mut app, _sid, _host) = desk();
    let mut posted = Vec::new();
    let clicked = app.person_menubar_click(&mut |wake| {
        posted.push(wake);
        true
    });
    assert!(
        clicked
            .as_deref()
            .is_ok_and(|said| said.starts_with("clicked ")),
        "{clicked:?}"
    );
    assert_eq!(
        lock_words(&app),
        words("read", "read"),
        "nothing ran before the wake"
    );
    assert_eq!(posted.len(), 1, "one click posted: {posted:?}");
    for wake in posted {
        let crate::Wake::StatusAction { action } = wake else {
            panic!("a status action: {wake:?}");
        };
        assert_eq!(
            action,
            crate::status_item::StatusAction::RotateFleetAccess { repeated: false },
            "the button's own action"
        );
        app.dispatch_status_action(action);
    }
    assert_eq!(lock_words(&app), words("read", "write"));
    app.on_status_menu_opening(None);
    assert_eq!(lock_words(&app), words("read", "read"));
    app.tick_lock_face(std::time::Instant::now() + 10 * crate::status_item::SETTLE);
    assert_eq!(crate::control::fleet_access_now(), ReadOnly);
    assert_eq!(
        app.person_menubar_click(&mut |_| false),
        Err("event loop gone".to_string())
    );
}
