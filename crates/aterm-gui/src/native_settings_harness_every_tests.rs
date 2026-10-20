// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SESSIONS' `Every session` row on Settings ▸ Harness (design §11.5): where
//! it stands, what it says, and what its presses ask of the host.

use super::*;
use crate::autopilot_timers::TimerChoice;
use crate::harness_screen::EveryPress;
use crate::native_app::HarnessRequest;

/// The fixture with `n` sessions, each its own tab: the fixture's own (on
/// Land by its word) first, then sessions following a default of Cruise,
/// every third pinned Off.
fn fixture_of(n: usize) -> HarnessState {
    let mut state = fixture();
    let first = state.sessions[0].clone();
    state.sessions = (0..n)
        .map(|i| {
            let mut session = first.clone();
            if i > 0 {
                let id = 7 + u64::try_from(i).expect("small");
                session.local_id = id;
                session.sid = format!("s-{id}");
                session.title = format!("task {i}");
                session.place = format!("in tab {}", i + 2);
                let off = i % 3 == 2;
                session.word = off.then_some(Autopilot::Off);
                session.view = if off {
                    AutopilotView::Off
                } else {
                    AutopilotView::Cruise
                };
            }
            session
        })
        .collect();
    state
}

/// The state published to the open page again (a session opened or closed),
/// as the App's publish delivers it.
fn republish(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    state: HarnessState,
    revision: u64,
) {
    assert!(runtime.replace_settings_harness(state, revision));
    runtime
        .dispatch(instance, view, AppEvent::HarnessChanged { revision })
        .expect("dispatched");
}

/// Every button of session `id`'s row, by key, and where it is drawn.
fn row_frames(
    pages: &[crate::native_ui::CompiledUi],
    id: u64,
) -> Vec<(String, crate::native_ui::LogicalRect)> {
    let prefix = format!("harness/session/{id}/");
    let mut frames: Vec<_> = pages[0]
        .semantics
        .iter()
        .filter(|n| {
            n.key.as_str().starts_with(&prefix) && n.role == crate::native_ui::SemanticRole::Button
        })
        .map(|n| (n.key.as_str().to_string(), n.rect))
        .collect();
    frames.sort_by(|a, b| a.0.cmp(&b.0));
    frames
}

/// NOTHING MOVES UNDER THE POINTER (design §11.5.5): on one open page, a
/// second session opening brings the `Every session` row in AFTER every
/// per-session row, and the first row's five buttons stay where they were
/// drawn; a third opening, and then one closing, move neither of the first
/// two rows' buttons either. One session: no row. NEGATIVE CONTROL (run by
/// hand, recorded in the landing commit): the row placed first moves the
/// first row's buttons down when it appears.
#[test]
fn the_every_row_comes_after_the_rows_and_moves_none_of_them() {
    let cx = view_cx_at(1_000.0, 1_400.0);
    let (mut runtime, instance, view) = harness_route(fixture_of(1));
    let pages = every_page(&mut runtime, instance, view, &cx);
    assert!(
        pages[0]
            .semantic(&UiKey::new("harness/session/all"))
            .is_none(),
        "one session: no row"
    );
    let one = row_frames(&pages, 7);
    assert_eq!(one.len(), 5, "{one:?}");

    republish(&mut runtime, instance, view, fixture_of(2), 2);
    let pages = every_page(&mut runtime, instance, view, &cx);
    assert_eq!(row_frames(&pages, 7), one, "the row's appearing moves none");
    let two = row_frames(&pages, 8);
    let every = node(&pages, "harness/session/all/block").rect;
    let last = node(&pages, "harness/session/8/block").rect;
    assert!(
        every.y >= last.y + last.height,
        "after every per-session row: {every:?} under {last:?}"
    );

    republish(&mut runtime, instance, view, fixture_of(3), 3);
    let pages = every_page(&mut runtime, instance, view, &cx);
    assert_eq!(row_frames(&pages, 7), one, "a third session opening");
    assert_eq!(row_frames(&pages, 8), two);

    republish(&mut runtime, instance, view, fixture_of(2), 4);
    let pages = every_page(&mut runtime, instance, view, &cx);
    assert_eq!(row_frames(&pages, 7), one, "one closing");
    assert_eq!(row_frames(&pages, 8), two);
}

/// THE ROW SAYS WHAT IS THERE AND WHAT EACH BUTTON MOVES (design §11.5.1):
/// `Every session` and `4 sessions: 2 cruising, 1 landing, 1 off`; Cruise,
/// Land, Timer and Follow the default, each spoken with its count (`Land
/// every session, 2 sessions`) and described with what stays; no Off. Its
/// Timer, opened, holds `1 hour`, `2 hours`, `4 hours` and `at 18:00`, and no cancel.
/// CONTROL: under the master switch off the row says why and every button
/// is inert.
#[test]
fn the_every_row_says_what_is_there_and_what_each_button_moves() {
    let mut state = fixture_of(4);
    let mut policy = SupervisorConfig::default();
    policy.set("land_at", "18:00").expect("a known key");
    state.config = config_rows(&policy);
    state.land_at_offer = Some(crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: None,
    });
    let (mut runtime, instance, view) = harness_route(state.clone());
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/session/all").label,
        "Every session, 4 sessions: 2 cruising, 1 landing, 1 off"
    );
    let button = |pages: &[crate::native_ui::CompiledUi], key: &str| {
        let n = node(pages, key);
        (
            n.label.clone(),
            n.description.clone().unwrap_or_default(),
            n.state.is_some_and(|s| s.enabled),
        )
    };
    assert_eq!(
        button(&pages, "harness/session/all/autopilot/land"),
        (
            "Land every session, 2 sessions".to_string(),
            "Land 2 sessions; the 1 off stays off".to_string(),
            true
        )
    );
    assert_eq!(
        button(&pages, "harness/session/all/autopilot/cruise"),
        (
            "Cruise every session, 1 session".to_string(),
            "Cruise 1 session: 1 landing; the 1 off stays off".to_string(),
            true
        )
    );
    assert_eq!(
        button(&pages, "harness/session/all/follow").0,
        "Follow the default for every session, 1 session"
    );
    assert_eq!(
        button(&pages, "harness/session/all/timer").0,
        "Timer for every session, 2 sessions"
    );
    for key in [
        "harness/session/all/autopilot/off",
        "harness/session/all/land-in/60",
        "harness/session/all/land-cancel",
    ] {
        assert!(
            pages.iter().all(|p| p.semantic(&UiKey::new(key)).is_none()),
            "{key}: not drawn"
        );
    }
    // The Timer, opened.
    let requests = press_for_requests(
        &mut runtime,
        instance,
        view,
        "harness/session/all/timer",
        None,
    );
    assert!(
        requests.is_empty(),
        "a disclosure asks the host for nothing"
    );
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/session/all/timer")
            .state
            .and_then(|s| s.expanded),
        Some(true)
    );
    for (key, label) in [
        (
            "harness/session/all/land-in/60",
            "Cruise every session for 1 hour, then land, 2 sessions",
        ),
        (
            "harness/session/all/land-in/240",
            "Cruise every session for 4 hours, then land, 2 sessions",
        ),
        (
            "harness/session/all/land-at",
            "Cruise every session, then land at 18:00, 2 sessions",
        ),
    ] {
        assert_eq!(node(&pages, key).label, label);
    }
    assert!(
        pages.iter().all(|p| p
            .semantic(&UiKey::new("harness/session/all/land-cancel"))
            .is_none()),
        "no bulk cancel"
    );

    // CONTROL: the master switch off.
    let mut off = state;
    off.posture = HarnessPosture::MasterOff;
    for session in &mut off.sessions {
        session.view = AutopilotView::MasterOff;
        session.default = None;
    }
    let (mut runtime, instance, view) = harness_route(off);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/session/all").label,
        "Every session, 4 sessions \u{b7} harness off in Settings"
    );
    for key in [
        "harness/session/all/autopilot/cruise",
        "harness/session/all/autopilot/land",
        "harness/session/all/timer",
        "harness/session/all/follow",
    ] {
        assert!(!button(&pages, key).2, "{key}: inert");
    }
}

/// THE ROW'S PRESSES ARE TYPED HOST REQUESTS (design §11.5.3, item 1): each
/// press is one `HarnessRequest::Every`, the timer's choices included; an
/// Off or a cancel is no press of the row. CONTROLS: under the master
/// switch off a press asks for nothing and says why; with one session (no
/// row) a press asks for nothing.
#[test]
fn the_every_rows_presses_are_typed_host_requests() {
    let (mut runtime, instance, view) = harness_route(fixture_of(3));
    for press in [
        EveryPress::Mode(Autopilot::Cruise),
        EveryPress::Mode(Autopilot::Land),
        EveryPress::Timer(TimerChoice::In(120)),
        EveryPress::Timer(TimerChoice::At),
        EveryPress::Follow,
    ] {
        let key = crate::harness_screen::every_action(press);
        assert_eq!(
            press_for_requests(&mut runtime, instance, view, &key, None),
            [HarnessRequest::Every(press)],
            "{key}"
        );
    }
    for key in [
        "harness/session/all/autopilot/off",
        "harness/session/all/land-cancel",
    ] {
        assert!(
            press_for_requests(&mut runtime, instance, view, key, None).is_empty(),
            "{key}"
        );
    }
    let mut off = fixture_of(3);
    off.posture = HarnessPosture::MasterOff;
    for session in &mut off.sessions {
        session.view = AutopilotView::MasterOff;
        session.default = None;
    }
    let (mut runtime, instance, view) = harness_route(off);
    assert!(
        press_for_requests(
            &mut runtime,
            instance,
            view,
            "harness/session/all/autopilot/land",
            None
        )
        .is_empty()
    );
    let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert_eq!(
        state.feedback.as_deref(),
        Some("The harness is off here, so Autopilot has nothing to steer.")
    );
    let (mut runtime, instance, view) = harness_route(fixture_of(1));
    assert!(
        press_for_requests(
            &mut runtime,
            instance,
            view,
            "harness/session/all/autopilot/land",
            None
        )
        .is_empty(),
        "one session: no row to press"
    );
}

/// THE ROW IS WHOLE ON EVERY HOST (design §11.5.1): on the 80 × 24 window's
/// page, the shipped page and a compact one, at 1× and 2× text, the row's
/// four buttons are drawn with their faces whole, none of them overlapping
/// another or the row's words, and inside the card.
#[test]
fn the_every_row_is_whole_on_every_host() {
    for (width, height) in [
        (584.0, 348.0),
        (744.0, 420.0),
        (1_000.0, 900.0),
        (340.0, 700.0),
    ] {
        at_each_text_scale(&[1.0, 2.0], "every session row", |scale| {
            let (mut runtime, instance, view) = harness_route(fixture_of(3));
            let pages = every_page(&mut runtime, instance, view, &view_cx_at(width, height));
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let keys = [
                "harness/session/all/autopilot/cruise",
                "harness/session/all/autopilot/land",
                "harness/session/all/timer",
                "harness/session/all/follow",
            ];
            let page = pages
                .iter()
                .find(|p| p.semantic(&UiKey::new(keys[0])).is_some())
                .unwrap_or_else(|| panic!("{context}: the row is drawn"));
            let rects: Vec<_> = keys
                .iter()
                .map(|k| page.semantic(&UiKey::new(*k)).expect("drawn").rect)
                .collect();
            let card = page
                .semantics
                .iter()
                .filter(|n| n.key.as_str().starts_with("harness/sessions"))
                .map(|n| n.rect)
                .find(|r| {
                    r.x <= rects[0].x
                        && r.y <= rects[0].y
                        && r.y + r.height >= rects[0].y + rects[0].height
                })
                .unwrap_or_else(|| panic!("{context}: in a SESSIONS card"));
            for (k, r) in keys.iter().zip(&rects) {
                assert!(r.width > 0.0 && r.height > 0.0, "{context}: {k}");
                assert!(
                    r.x >= card.x - 0.5 && r.x + r.width <= card.x + card.width + 0.5,
                    "{context}: {k} {r:?} in {card:?}"
                );
            }
            for (i, a) in rects.iter().enumerate() {
                for b in &rects[i + 1..] {
                    let apart = a.x + a.width <= b.x + 0.5
                        || b.x + b.width <= a.x + 0.5
                        || a.y + a.height <= b.y + 0.5
                        || b.y + b.height <= a.y + 0.5;
                    assert!(apart, "{context}: {a:?} and {b:?} overlap");
                }
            }
            let face = button_face(&pages, "harness/session/all/follow");
            assert_eq!(face, "Follow the default", "{context}: its face whole");
        });
    }
}

/// The paint node `key` draws, on whichever page draws it.
fn painted<'a>(
    pages: &'a [crate::native_ui::CompiledUi],
    key: &str,
) -> &'a crate::native_ui::PaintNode {
    pages
        .iter()
        .find_map(|page| page.paint.iter().find(|n| n.key.as_str() == key))
        .unwrap_or_else(|| panic!("{key} is painted on no page"))
}

/// THE ROW MOVES WITH THE ROWS ABOVE IT, SO ITS PRESSES WAIT FOR A LOOK
/// (design §11.5.7): with sessions 7, 8 and 9 listed, the middle one
/// closing lifts session 9's row into 8's place and the `Every session` row
/// under it, so the bulk Land then stands exactly where session 9's Land
/// was drawn: a still pointer there would rest on it. Every control of the
/// row (its buttons, its Timer and the Timer's choices) asks for a look
/// before a pointer press (`Control::look_first`, which the App's press
/// path holds to: `harness_every_tests`'
/// `a_press_the_layout_moved_under_a_still_pointer_chooses_nothing`); no
/// session row's button does. CONTROL: the frames before the close are not
/// the frames after it.
#[test]
fn the_every_row_moves_with_the_rows_above_it_and_its_presses_wait_for_a_look() {
    let cx = view_cx_at(1_000.0, 1_400.0);
    let (mut runtime, instance, view) = harness_route(fixture_of(3));
    let pages = every_page(&mut runtime, instance, view, &cx);
    let nine_land = node(&pages, "harness/session/9/autopilot/land").rect;
    let bulk_land = node(&pages, "harness/session/all/autopilot/land").rect;
    assert_ne!(nine_land, bulk_land, "control: apart before the close");
    let mut closed = fixture_of(3);
    closed.sessions.retain(|s| s.local_id != 8);
    republish(&mut runtime, instance, view, closed, 2);
    let pages = every_page(&mut runtime, instance, view, &cx);
    let moved = node(&pages, "harness/session/all/autopilot/land").rect;
    let (x, y) = (
        nine_land.x + nine_land.width / 2.0,
        nine_land.y + nine_land.height / 2.0,
    );
    assert!(
        moved.contains(x, y),
        "the bulk Land {moved:?} stands where session 9's Land {nine_land:?} was"
    );
    assert!(
        press_for_requests(
            &mut runtime,
            instance,
            view,
            "harness/session/all/timer",
            None
        )
        .is_empty()
    );
    let pages = every_page(&mut runtime, instance, view, &cx);
    let hits: std::collections::BTreeMap<&str, bool> = pages
        .iter()
        .flat_map(|page| page.hits.iter())
        .map(|hit| (hit.key.as_str(), hit.look_first))
        .collect();
    let (every, rows): (Vec<_>, Vec<_>) = hits
        .iter()
        .filter(|(key, _)| key.starts_with("harness/session/"))
        .partition(|(key, _)| key.starts_with("harness/session/all/"));
    assert_eq!(
        every.len(),
        4 + crate::autopilot_menu::LAND_IN_MINUTES.len(),
        "{every:?}"
    );
    assert!(every.iter().all(|(_, look)| **look), "{every:?}");
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|(_, look)| !**look), "{rows:?}");
}

/// THE ROW'S TIMER OPENS UNDER ITS TIMER AND FOLLOW THE DEFAULT (design
/// §11.5.7): beside the words, its choices stand under those two columns
/// only, so a row above shrinking (Cruise dropping the timers' `lands at`
/// lines) never lifts one into the place of the row's Cruise or Land.
/// CONTROL: the row's Land is left of every choice.
#[test]
fn the_every_rows_timer_opens_under_its_timer_and_follow() {
    let mut state = fixture_of(3);
    state.land_at_offer = Some(crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: None,
    });
    let cx = view_cx_at(1_000.0, 1_400.0);
    let (mut runtime, instance, view) = harness_route(state);
    assert!(
        press_for_requests(
            &mut runtime,
            instance,
            view,
            "harness/session/all/timer",
            None
        )
        .is_empty()
    );
    let pages = every_page(&mut runtime, instance, view, &cx);
    let timer = node(&pages, "harness/session/all/timer").rect;
    let follow = node(&pages, "harness/session/all/follow").rect;
    let land = node(&pages, "harness/session/all/autopilot/land").rect;
    for key in [
        "harness/session/all/land-in/60",
        "harness/session/all/land-in/120",
        "harness/session/all/land-in/240",
        "harness/session/all/land-at",
    ] {
        let choice = node(&pages, key).rect;
        assert!(
            choice.x >= timer.x - 0.5 && choice.x + choice.width <= follow.x + follow.width + 0.5,
            "{key} {choice:?} under {timer:?} and {follow:?}"
        );
        assert!(choice.x >= land.x + land.width, "{key}: right of Land");
    }
}

/// WHERE IT STEERS NONE, THE ROW'S LINE IS WHOLE (design §11.5.4): under
/// the master switch off, on the 80 × 24 window's page, the shipped page
/// and a wide one, at 1× and 2× text, the row reads `3 sessions · harness
/// off in Settings` whole, its reason never cut. CONTROL: main's long words
/// for an inactive control (`currently inactive: Supervise coding agents is
/// Off`) are cut in the row's words column somewhere among them.
#[test]
fn the_inert_rows_line_is_whole() {
    let mut off = fixture_of(3);
    off.posture = HarnessPosture::MasterOff;
    for session in &mut off.sessions {
        session.view = AutopilotView::MasterOff;
        session.default = None;
    }
    let long = "3 sessions \u{b7} currently inactive: Supervise coding agents is Off";
    let cut = std::sync::atomic::AtomicBool::new(false);
    for (width, height) in [(584.0, 348.0), (744.0, 420.0), (1_000.0, 900.0)] {
        at_each_text_scale(&[1.0, 2.0], "the inert row's line", |scale| {
            let (mut runtime, instance, view) = harness_route(off.clone());
            let pages = every_page(&mut runtime, instance, view, &view_cx_at(width, height));
            let detail = painted(&pages, "harness/session/all/detail");
            let crate::native_ui::UiContent::Text(text) = &detail.content else {
                panic!("the row's line is text")
            };
            assert_eq!(
                text.text, "3 sessions \u{b7} harness off in Settings",
                "{scale}\u{d7} {width}: whole"
            );
            if crate::native_ui::fit_native_status_label(long, detail.rect.width) != long {
                cut.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
    }
    assert!(
        cut.load(std::sync::atomic::Ordering::Relaxed),
        "control: the long words are cut somewhere"
    );
}
