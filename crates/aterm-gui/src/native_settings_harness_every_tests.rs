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

/// A WORD'S PRESS MOVES NO ROW (design §11.7.7): on
/// one open page, session 7 taking its own word from Settings ▸ Harness,
/// handing it back to the default, and session 8 taking one change only the
/// words of their setting lines (`follows the default` ↔ `its own setting,
/// set 14:05 from Settings ▸ Harness`): every button of the rows under them
/// and of the `Every session` row stays where it was drawn — beside the
/// words and stacked. NEGATIVE CONTROL (`setting_line`'s height only beside
/// an own word, the slice's first build): the press moves every row under
/// it.
#[test]
fn a_words_press_moves_no_row_under_it() {
    use crate::autopilot::{SetByWords, Surface};
    let own = |state: &mut HarnessState, index: usize, word: Option<Autopilot>| {
        let session = &mut state.sessions[index];
        session.word = word;
        session.view = match word {
            Some(Autopilot::Land) => AutopilotView::Land,
            Some(Autopilot::Off) => AutopilotView::Off,
            _ => AutopilotView::Cruise,
        };
        session.set_by = Some(SetByWords {
            by: Surface::Settings.phrase().to_string(),
            at: Some(crate::harness_glass::Stamp {
                clock: crate::claude_lights::Clock::of_minute(14 * 60 + 5),
                day: 0,
            }),
        });
    };
    let buttons = |pages: &[crate::native_ui::CompiledUi], prefix: &str| {
        let mut frames: Vec<(String, crate::native_ui::LogicalRect)> = pages
            .iter()
            .flat_map(|page| page.semantics.iter())
            .filter(|n| {
                n.key.as_str().starts_with(prefix)
                    && n.role == crate::native_ui::SemanticRole::Button
            })
            .map(|n| (n.key.as_str().to_string(), n.rect))
            .collect();
        frames.sort_by(|a, b| a.0.cmp(&b.0));
        frames
    };
    let under = |pages: &[crate::native_ui::CompiledUi]| {
        [
            buttons(pages, "harness/session/8/"),
            buttons(pages, "harness/session/9/"),
            buttons(pages, "harness/session/all/"),
        ]
    };
    for (width, height, beside) in [(1_000.0, 1_400.0, true), (584.0, 1_400.0, false)] {
        let cx = view_cx_at(width, height);
        let mut state = fixture_of(3);
        own(&mut state, 0, None);
        let (mut runtime, instance, view) = harness_route(state.clone());
        let pages = every_page(&mut runtime, instance, view, &cx);
        assert_eq!(
            pages.iter().any(|page| page
                .semantic(&UiKey::new("harness/session/7/head"))
                .is_some()),
            beside,
            "{width}: the row beside its words"
        );
        assert_eq!(
            node(&pages, "harness/session/7/setting").label,
            "follows the default"
        );
        let before = under(&pages);
        assert!(before.iter().all(|frames| !frames.is_empty()), "{before:?}");

        let steps: [(&str, Option<Autopilot>, usize, &str); 3] = [
            (
                "session 7 takes its own word",
                Some(Autopilot::Land),
                0,
                "its own setting, set 14:05 from Settings \u{25b8} Harness",
            ),
            ("session 7 handed back", None, 0, "follows the default"),
            (
                "session 8 takes its own word",
                Some(Autopilot::Land),
                1,
                "its own setting, set 14:05 from Settings \u{25b8} Harness",
            ),
        ];
        for (revision, (step, word, index, says)) in (2..).zip(steps) {
            own(&mut state, index, word);
            republish(&mut runtime, instance, view, state.clone(), revision);
            let pages = every_page(&mut runtime, instance, view, &cx);
            let id = state.sessions[index].local_id;
            assert_eq!(
                node(&pages, &format!("harness/session/{id}/setting")).label,
                says,
                "{width}: {step}: its words"
            );
            assert_eq!(under(&pages), before, "{width}: {step}: no row moves");
        }
    }
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
            // The screen that shows the row: a screen whose edge cuts it shows
            // only what of it the room holds (the list's edge rule).
            let page = pages
                .iter()
                .find(|p| keys.iter().all(|k| p.semantic(&UiKey::new(*k)).is_some()))
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
/// `a_press_the_layout_moved_under_a_still_pointer_chooses_nothing`), and so
/// does every session row's button (the post-landing check's owner-eye-4: a
/// state line coming or going above moves them too). CONTROL: the frames
/// before the close are not the frames after it.
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
    assert!(rows.iter().all(|(_, look)| **look), "{rows:?}");
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

/// THE ROW'S TIMER CHOICES STAND IN COLUMNS WHERE THEY WRAP (the post-landing
/// check's owner-eye-3, and its review): `1 hour`, `2 hours`, `4 hours` and
/// `at 18:00` laid in more lines than one take one width, so each line's
/// first and second choice start where the line above's do; beside the
/// words that width is the row's slot, two and two, so the first column
/// starts under the Timer and the second under Follow the default — never
/// `at 18:00` a few points right of `2 hours`, nor the grid a few points
/// right of Follow. On one line (a stacked row's wide column) each keeps
/// its own words' width. On the wide page beside the words and on narrow
/// stacked ones, at 1× and 2× text. NEGATIVE CONTROLS: each choice as wide
/// as its own face (main's) puts the second line's second choice right of
/// the first line's on the wide page; each as wide as the widest (the
/// first fix) puts the second column 10.4 pt right of Follow the default;
/// the widest width on one line too makes the one line's widths one.
#[test]
fn the_every_rows_timer_choices_line_up_where_they_wrap() {
    let mut state = fixture_of(3);
    state.land_at_offer = Some(crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: None,
    });
    let wrapped = std::sync::Mutex::new(0_usize);
    let on_slots = std::sync::Mutex::new(0_usize);
    let one_line = std::sync::Mutex::new(0_usize);
    at_each_text_scale(&[1.0, 2.0], "the every row's timer grid", |scale| {
        for (width, height) in [
            (1_000.0, 1_400.0),
            (1_200.0, 1_400.0),
            (1_400.0, 1_400.0),
            (744.0, 1_400.0),
            (584.0, 1_400.0),
            (504.0, 1_400.0),
        ] {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let cx = view_cx_at(width, height);
            let (mut runtime, instance, view) = harness_route(state.clone());
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
            let rects: Vec<_> = [
                "harness/session/all/land-in/60",
                "harness/session/all/land-in/120",
                "harness/session/all/land-in/240",
                "harness/session/all/land-at",
            ]
            .iter()
            .map(|key| node(&pages, key).rect)
            .collect();
            let mut lines: Vec<Vec<crate::native_ui::LogicalRect>> = Vec::new();
            for rect in rects {
                match lines.last_mut() {
                    Some(line) if (line[0].y - rect.y).abs() < 0.5 => line.push(rect),
                    _ => lines.push(vec![rect]),
                }
            }
            if lines.len() < 2 {
                *one_line.lock().unwrap() += 1;
                let line = &lines[0];
                assert!(
                    line.iter()
                        .any(|rect| (rect.width - line[0].width).abs() > 0.5),
                    "{context}: on one line each its own width, {line:?}"
                );
                continue;
            }
            *wrapped.lock().unwrap() += 1;
            let first = &lines[0];
            for line in &lines {
                for (column, rect) in line.iter().enumerate() {
                    assert!(
                        (rect.width - first[0].width).abs() < 0.5,
                        "{context}: one width where they wrap, {lines:?}"
                    );
                    assert!(
                        (rect.x - first[column].x).abs() < 0.5,
                        "{context}: column {column} under the first line's, {lines:?}"
                    );
                }
            }
            // Beside the words (the Timer and Follow the default on one
            // line), where every face is whole in the row's slot (a choice
            // is then as wide as the Timer): the grid's columns under them,
            // two and two. A face wider than a slot (`at Sat 18:00` at 2×
            // in the Linux lane's Noto Sans) gives each choice the widest's
            // width instead, its columns still under one another (above).
            let timer = node(&pages, "harness/session/all/timer").rect;
            let follow = node(&pages, "harness/session/all/follow").rect;
            if (timer.y - follow.y).abs() < 0.5 && (first[0].width - timer.width).abs() < 0.5 {
                *on_slots.lock().unwrap() += 1;
                assert!(
                    lines.iter().all(|line| line.len() == 2),
                    "{context}: two and two, {lines:?}"
                );
                assert!(
                    (first[0].x - timer.x).abs() < 0.5 && (first[1].x - follow.x).abs() < 0.5,
                    "{context}: the columns under the Timer {timer:?} and Follow the default \
                     {follow:?}, {lines:?}"
                );
            }
        }
    });
    assert!(
        *wrapped.lock().unwrap() >= 2,
        "the choices wrapped on some page"
    );
    assert!(
        *on_slots.lock().unwrap() >= 2,
        "the choices wrapped beside the words on some page"
    );
    assert!(
        *one_line.lock().unwrap() >= 1,
        "the choices stood on one line on some page"
    );
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

/// Whether `key` is drawn whole and live on `page`: painted uncut by the
/// list's edges and unfaded, and on the page's semantics, so a person can
/// reach it (rows scrolled under a kept caption are marked cut on purpose,
/// `peek`, for the paint audit: that mark says nothing of what shows).
fn drawn_whole(page: &crate::native_ui::CompiledUi, key: &str) -> bool {
    page.semantic(&UiKey::new(key)).is_some()
        && page
            .paint
            .iter()
            .any(|n| n.key.as_str() == key && n.clip == n.rect && n.fade.is_none())
}

/// THE EVERY ROW'S OPEN TIMER SHOWS ITS LAST CHOICE TO A PRESS AND TO THE
/// KEYBOARD (the final check of 2026-10-20, F1): at 2× text on the lab's
/// narrow page (504 × 628 pt, and heights about it), three sessions and
/// `land_at` set, the list paged to the `Every session` row, whose block
/// with its Timer open is taller than the room under the SESSIONS caption
/// the list keeps over it. PRESSING TIMER opens it in view: `at 18:00`, its last choice,
/// drawn whole and live. THE KEYBOARD, the list back at the row's own seat
/// (no nudge, nothing revealed — as Home and Page Down leave it): focus on
/// `at 18:00` scrolls the list into the row until it is drawn whole — under
/// the caption kept where the room holds both, else past it. NOT THE WHEEL:
/// the wheel and a trackpad still move the list a whole stop at a time
/// (design ruling 264), so from the row's own seat they go past it (the
/// second review of 2026-10-20, R3; §11.14). NEGATIVE CONTROLS: head's
/// reveals by whole stops alone (no `seat_span`, no reveal at the Timer's
/// opening) draw `at 18:00` nowhere after the press and nowhere after the
/// focus, at 628 pt; and on a page tall enough (900 pt) that the row's own
/// seat draws it whole, the press moves nothing — the Timer stays where it
/// was pressed.
#[test]
fn the_every_rows_open_timer_shows_its_last_choice_to_a_press_and_to_the_keyboard() {
    let mut state = fixture_of(3);
    state.land_at_offer = Some(crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: None,
    });
    const AT: &str = "harness/session/all/land-at";
    let cut_at_the_seat = std::sync::Mutex::new(0_usize);
    let whole_at_the_seat = std::sync::Mutex::new(0_usize);
    at_each_text_scale(&[2.0], "the every row's open timer", |scale| {
        for height in [600.0, 628.0, 660.0, 900.0] {
            let control = height > 800.0;
            let context = format!("{scale}\u{d7} 504\u{d7}{height}");
            let cx = view_cx_at(504.0, height);
            let (mut runtime, instance, view) = harness_route(state.clone());
            let set_seat = |runtime: &mut NativeRuntime, page: usize| {
                let Some(AppViewState::Settings(settings)) = runtime.view_state_mut(view) else {
                    unreachable!()
                };
                settings.scroll_page_to(page);
                settings.page_reveal = None;
                settings.drop_list_nudge();
            };
            const TIMER: &str = "harness/session/all/timer";
            let timer_at = |page: &crate::native_ui::CompiledUi| {
                page.paint
                    .iter()
                    .find(|n| n.key.as_str() == TIMER)
                    .map(|n| n.rect)
            };
            // The list paged to the row: the first screen that lists its
            // Timer — for the control on the tall page, the last, where the
            // row is the first stop under the caption.
            let mut page = 0;
            let mut listed = loop {
                set_seat(&mut runtime, page);
                let shown = compile_settings_view(&runtime, instance, view, &cx);
                if shown.semantic(&UiKey::new(TIMER)).is_some() {
                    break timer_at(&shown);
                }
                page += 1;
                assert!(page < 40, "{context}: the row is on some screen");
            };
            while control && page < 40 {
                set_seat(&mut runtime, page + 1);
                let shown = compile_settings_view(&runtime, instance, view, &cx);
                if shown.semantic(&UiKey::new(TIMER)).is_none() || timer_at(&shown) == listed {
                    break;
                }
                page += 1;
                listed = timer_at(&shown);
            }
            set_seat(&mut runtime, page);
            compile_settings_view(&runtime, instance, view, &cx);
            // The press.
            press_for_requests(&mut runtime, instance, view, TIMER, None);
            let pressed = compile_settings_view(&runtime, instance, view, &cx);
            let by_press = drawn_whole(&pressed, AT);
            // The keyboard, from the row's own seat.
            set_seat(&mut runtime, page);
            let seat = compile_settings_view(&runtime, instance, view, &cx);
            if drawn_whole(&seat, AT) {
                // Whole at the seat: the press moved nothing.
                *whole_at_the_seat.lock().unwrap() += 1;
                assert_eq!(
                    timer_at(&pressed),
                    listed,
                    "{context}: the Timer stays where it was pressed"
                );
            } else {
                assert!(
                    !control,
                    "{context}: the tall page's seat draws `at 18:00` whole"
                );
                *cut_at_the_seat.lock().unwrap() += 1;
            }
            runtime
                .dispatch(instance, view, AppEvent::FocusChanged(Some(UiKey::new(AT))))
                .unwrap();
            let focused = compile_settings_view(&runtime, instance, view, &cx);
            let by_keyboard = drawn_whole(&focused, AT);
            assert_eq!(
                (by_press, by_keyboard),
                (true, true),
                "{context}: `at 18:00` whole (opened by the press, focused by the keyboard): \
                 {:?}",
                pressed
                    .paint
                    .iter()
                    .chain(&focused.paint)
                    .filter(|n| n.key.as_str() == AT)
                    .map(|n| (n.rect, n.clip, n.fade))
                    .collect::<Vec<_>>()
            );
        }
    });
    assert!(
        *cut_at_the_seat.lock().unwrap() >= 1,
        "PRECONDITION: the row's own seat cuts `at 18:00` on some page"
    );
    assert!(
        *whole_at_the_seat.lock().unwrap() >= 1,
        "PRECONDITION: the row's own seat draws `at 18:00` whole on some page"
    );
}

/// The block of row `row` (`all` for the `Every session` row) on `page`:
/// its rect, and how much of it the list's edges leave drawn.
fn block_of(
    page: &crate::native_ui::CompiledUi,
    row: &str,
) -> Option<(crate::native_ui::LogicalRect, crate::native_ui::LogicalRect)> {
    let key = format!("harness/session/{row}/block");
    page.paint
        .iter()
        .find(|n| n.key.as_str() == key)
        .map(|n| (n.rect, n.clip))
}

/// A TIMER CLOSED GIVES BACK THE PLACE ITS OPENING SCROLLED (the second
/// review of 2026-10-20, R2): at 2× text on the lab's narrow page (504 ×
/// 628 pt, and heights about it), three sessions and `land_at` set, the list
/// paged to the `Every session` row with its Timer closed and the row drawn
/// whole. Opening the Timer scrolls the list into the row so its last
/// choice shows (F1); closing it lets that scroll go, so the closed row —
/// its name, its state line, its buttons — is drawn whole again where it
/// was, never left under the SESSIONS caption the list keeps over it. A session row's
/// Timer the same. NEGATIVE CONTROL: the opening did scroll the Every row
/// (its block drawn higher with the Timer open than closed) on some page;
/// with the scroll kept after the close (the first fix of F1), the closed
/// block was drawn 300 pt of its 350 at 600 pt, its first 50 under the
/// caption.
#[test]
fn a_timer_closed_gives_back_the_place_its_opening_scrolled() {
    let mut state = fixture_of(3);
    state.land_at_offer = Some(crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: None,
    });
    let id = state.sessions[1].local_id.to_string();
    let scrolled = std::sync::Mutex::new(0_usize);
    at_each_text_scale(&[2.0], "a timer closed", |scale| {
        for height in [600.0, 628.0, 660.0] {
            for row in ["all", id.as_str()] {
                let context = format!("{scale}\u{d7} 504\u{d7}{height} {row}");
                let cx = view_cx_at(504.0, height);
                let (mut runtime, instance, view) = harness_route(state.clone());
                let timer = format!("harness/session/{row}/timer");
                // The list paged to the first screen that draws the row whole.
                let mut page = 0;
                let closed = loop {
                    let Some(AppViewState::Settings(settings)) = runtime.view_state_mut(view)
                    else {
                        unreachable!()
                    };
                    settings.scroll_page_to(page);
                    settings.page_reveal = None;
                    settings.drop_list_nudge();
                    let shown = compile_settings_view(&runtime, instance, view, &cx);
                    if let Some((rect, clip)) = block_of(&shown, row)
                        && rect == clip
                        && shown.semantic(&UiKey::new(timer.as_str())).is_some()
                    {
                        break rect;
                    }
                    page += 1;
                    assert!(page < 40, "{context}: the row is whole on some screen");
                };
                press_for_requests(&mut runtime, instance, view, &timer, None);
                let open = compile_settings_view(&runtime, instance, view, &cx);
                let (open_rect, _) = block_of(&open, row).expect("the row with its Timer open");
                if open_rect.y < closed.y - 0.5 {
                    *scrolled.lock().unwrap() += 1;
                }
                press_for_requests(&mut runtime, instance, view, &timer, None);
                let again = compile_settings_view(&runtime, instance, view, &cx);
                let (rect, clip) = block_of(&again, row).expect("the row closed again");
                assert_eq!(
                    (rect, clip),
                    (closed, closed),
                    "{context}: the closed row drawn whole where it was (open at {open_rect:?})"
                );
            }
        }
    });
    assert!(
        *scrolled.lock().unwrap() >= 1,
        "PRECONDITION: opening the Timer scrolled the list on some page"
    );
}

/// A SESSION ROW'S OPEN TIMER STANDS IN COLUMNS WHERE IT WRAPS (the final
/// check of 2026-10-20, F2; owner-eye-3, which §11.13 fixed for the Every
/// row only): `1 hour`, `2 hours`, `4 hours`, `at 18:00` and `Cancel` (a
/// timer armed) laid in more lines than one take one width, so each line's
/// choices start where the first line's do — `at 18:00` under `1 hour`,
/// `Cancel` under `2 hours`. On one line each keeps its own words' width.
/// At 1× and 2× text, on the wide pages and the narrow ones. NEGATIVE
/// CONTROL: each choice as wide as its own words (head's) puts `at 18:00`
/// 19.9 pt wider than `1 hour` and `Cancel` left of `2 hours` at 2× in 504 pt.
#[test]
fn a_session_rows_timer_choices_line_up_where_they_wrap() {
    let mut state = fixture_of(3);
    state.land_at_offer = Some(crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: None,
    });
    let id = state.sessions[1].local_id;
    state.sessions[1].armed_at_ms = Some(1_792_173_600_000);
    let wrapped = std::sync::Mutex::new(0_usize);
    let one_line = std::sync::Mutex::new(0_usize);
    at_each_text_scale(&[1.0, 2.0], "a session row's timer grid", |scale| {
        for width in [1_400.0, 1_000.0, 744.0, 584.0, 504.0] {
            let context = format!("{scale}\u{d7} {width}");
            let cx = view_cx_at(width, 1_400.0);
            let (mut runtime, instance, view) = harness_route(state.clone());
            press_for_requests(
                &mut runtime,
                instance,
                view,
                &format!("harness/session/{id}/timer"),
                None,
            );
            let pages = every_page(&mut runtime, instance, view, &cx);
            let rects: Vec<_> = [
                "land-in/60",
                "land-in/120",
                "land-in/240",
                "land-at",
                "land-cancel",
            ]
            .iter()
            .map(|choice| node(&pages, &format!("harness/session/{id}/{choice}")).rect)
            .collect();
            let mut lines: Vec<Vec<crate::native_ui::LogicalRect>> = Vec::new();
            for rect in &rects {
                match lines.last_mut() {
                    Some(line) if (line[0].y - rect.y).abs() < 0.5 => line.push(*rect),
                    _ => lines.push(vec![*rect]),
                }
            }
            if lines.len() < 2 {
                *one_line.lock().unwrap() += 1;
                let line = &lines[0];
                assert!(
                    line.iter()
                        .any(|rect| (rect.width - line[0].width).abs() > 0.5),
                    "{context}: on one line each its own width, {line:?}"
                );
                continue;
            }
            *wrapped.lock().unwrap() += 1;
            let first = &lines[0];
            for line in &lines {
                for (column, rect) in line.iter().enumerate() {
                    assert!(
                        (rect.width - first[0].width).abs() < 0.5,
                        "{context}: one width where they wrap, {lines:?}"
                    );
                    assert!(
                        (rect.x - first[column].x).abs() < 0.5,
                        "{context}: column {column} under the first line's, {lines:?}"
                    );
                }
            }
            if scale > 1.5 && (width - 504.0).abs() < 0.5 {
                // The finding's own page: `at 18:00` under `1 hour`, as wide;
                // `Cancel` under `2 hours`.
                let (hour, two, at, cancel) = (rects[0], rects[1], rects[3], rects[4]);
                assert!(
                    (at.x - hour.x).abs() < 0.5 && (at.width - hour.width).abs() < 0.5,
                    "{context}: {at:?} under {hour:?}"
                );
                assert!(
                    (cancel.x - two.x).abs() < 0.5,
                    "{context}: {cancel:?} under {two:?}"
                );
            }
        }
    });
    assert!(
        *wrapped.lock().unwrap() >= 2,
        "the choices wrapped on some page"
    );
    assert!(
        *one_line.lock().unwrap() >= 1,
        "the choices stood on one line on some page"
    );
}

/// AN OPEN TIMER'S CHOICES SHOW TOGETHER, AND WITH WHAT WAS PRESSED (the
/// tall-row parts of 2026-10-20 meeting F1): at 2× text on narrow pages of
/// every height from 560 to 720 pt, the `Every session` row's Timer pressed
/// from the first screen that lists it opens with `1 hour` and `at 18:00`,
/// its first and last choices, both drawn whole, and the Timer button it was
/// pressed by still drawn whole. NEGATIVE CONTROLS: with its choices' lines
/// listed one by one (no `timer-choices` column), the list split them into
/// different parts and `at 18:00` was not whole (590 pt); with the button's
/// part revealed (head's), neither choice was (560 pt); with the choices'
/// part revealed by whole stops, the Timer button went off the top (560 pt).
#[test]
fn an_open_timers_choices_show_together_and_with_its_button() {
    let mut state = fixture_of(3);
    state.land_at_offer = Some(crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: None,
    });
    const TIMER: &str = "harness/session/all/timer";
    const FIRST: &str = "harness/session/all/land-in/60";
    const LAST: &str = "harness/session/all/land-at";
    at_each_text_scale(&[2.0], "an open timer's choices", |scale| {
        for height in (560..=720).step_by(10).map(|h| h as f32) {
            let context = format!("{scale}\u{d7} 504\u{d7}{height}");
            let cx = view_cx_at(504.0, height);
            let (mut runtime, instance, view) = harness_route(state.clone());
            let mut page = 0;
            loop {
                let Some(AppViewState::Settings(settings)) = runtime.view_state_mut(view) else {
                    unreachable!()
                };
                settings.scroll_page_to(page);
                settings.page_reveal = None;
                settings.drop_list_nudge();
                let shown = compile_settings_view(&runtime, instance, view, &cx);
                if drawn_whole(&shown, TIMER) {
                    break;
                }
                page += 1;
                assert!(page < 40, "{context}: the Timer is whole on some screen");
            }
            press_for_requests(&mut runtime, instance, view, TIMER, None);
            let open = compile_settings_view(&runtime, instance, view, &cx);
            assert_eq!(
                (
                    drawn_whole(&open, FIRST),
                    drawn_whole(&open, LAST),
                    drawn_whole(&open, TIMER)
                ),
                (true, true, true),
                "{context}: `1 hour`, `at 18:00` and the Timer whole once it opened"
            );
        }
    });
}
