// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_ui::Control;

/// NO STEP SHOWS ONLY A SLIVER MORE: a stop a point taller than the room is read from its
/// own seat, its lower edge under the room's left unshown, with no step a point further
/// (which looked like the screen before, as if Page Down had stopped: Text & Fonts' 97pt
/// Font family in a 96pt list at 2x); one a line taller steps through to its end.
#[test]
fn a_stop_a_sliver_taller_than_the_room_takes_no_step_to_show_the_sliver() {
    let plan = |tall: f32| {
        let mut plan = ScrollPlan::new(10.0);
        plan.card(60.0, vec![UiKey::new("before")]);
        plan.card(tall, vec![UiKey::new("tall")]);
        plan.set_control_rows(vec![(0.0, 0.0, tall)]);
        plan.set_drawn_bands(vec![DrawnPart {
            top: 0.0,
            bottom: tall,
            whole: true,
        }]);
        plan.card(60.0, vec![UiKey::new("after")]);
        plan.set_room(96.0);
        plan
    };
    let sliver = plan(97.0);
    assert!(sliver.taller_than_room(1));
    assert_eq!(sliver.landings(1), vec![None], "its own seat alone");
    assert_eq!(
        sliver.page_down(1, None).0,
        2,
        "Page Down goes on to the next stop"
    );
    let taller = plan(140.0);
    assert!(
        taller.landings(1).len() > 1,
        "a stop a line taller steps on: {:?}",
        taller.landings(1)
    );
}

fn cards(heights: &[f32], room: f32, gap: f32) -> ScrollPlan {
    let mut plan = ScrollPlan::new(gap);
    for height in heights {
        plan.card(*height, Vec::new());
    }
    plan.set_room(room);
    plan
}

#[test]
fn a_told_scroll_is_applied_to_paint_only_at_its_exact_solver_inputs() {
    let state = SettingsViewState::new(&crate::app_config::Config::default());
    let seats = SeatsShared::open(&state);
    let input = cards(&[40.0, 60.0, 80.0], 90.0, 4.0);
    let _ = crate::work_counts::take_settings_page_work();
    let told = state.seated_under(input.clone(), None);
    assert_eq!(crate::work_counts::take_settings_page_work().seats, 1);
    assert!(state.scroll_record.borrow().is_none());
    {
        let _nested = SeatsShared::open(&state);
        assert!(state.scroll_seats.borrow().as_ref().unwrap().is_empty());
    }
    assert_eq!(state.scroll_seats.borrow().as_ref().unwrap().len(), 1);
    let painted = SeatedList::seated(&state, input.clone(), Vec::new(), input.room());
    assert_eq!(crate::work_counts::take_settings_page_work().seats, 0);
    assert_eq!(*state.scroll_record.borrow(), told);
    assert_eq!(painted.plan, told.as_ref().unwrap().plan);
    assert_eq!(
        painted.view,
        painted.plan.screen(told.as_ref().unwrap().top).view
    );
    assert_eq!(
        state.page_window.get(),
        Some(painted.plan.shown(0).len().max(1))
    );
    assert_eq!(state.result_page_limit.get(), Some(painted.plan.limit()));

    // A different prior record is itself a solver input, even for the same list.
    let _ = state.seat_shared(&mut input.clone());
    assert_eq!(crate::work_counts::take_settings_page_work().seats, 1);
    state.scroll_record.replace(None);
    let mut changed = input.clone();
    changed.pin_keyboard(Some((40.0, 20.0)));
    let _ = state.seat_shared(&mut changed);
    assert_eq!(crate::work_counts::take_settings_page_work().seats, 1);
    state.scroll_record.replace(None);
    drop(seats);
    state.start_let_go.set(true);
    let _seats = SeatsShared::open(&state);
    let _ = state.seat_shared(&mut input.clone());
    state.scroll_record.replace(None);
    state.start_let_go.set(false);
    let _ = state.seat_shared(&mut input.clone());
    assert_eq!(crate::work_counts::take_settings_page_work().seats, 2);
}

#[test]
fn independent_scroll_checks_and_held_presses_do_not_share_seats() {
    let state = SettingsViewState::new(&crate::app_config::Config::default());
    let seats = SeatsShared::open(&state);
    let input = cards(&[40.0, 60.0, 80.0], 90.0, 4.0);
    let _ = state.seated_under(input.clone(), None);
    // An impossible cached answer proves an uncounted check cannot read the memo.
    state.scroll_seats.borrow_mut().as_mut().unwrap()[0].view = usize::MAX..usize::MAX;
    let mut checked = input.clone();
    let view = crate::work_counts::page_work_uncounted(|| state.seat_shared(&mut checked));
    assert_eq!(view, checked.screen(0).view);
    assert_eq!(state.scroll_seats.borrow().as_ref().unwrap().len(), 1);
    state.scroll_record.replace(None);
    state.forget_shared_seats();
    let _ = crate::work_counts::page_work_uncounted(|| state.seat_shared(&mut input.clone()));
    assert!(state.scroll_seats.borrow().as_ref().unwrap().is_empty());
    state.scroll_record.replace(None);
    state.pressed_at.replace(Some(PressedAt {
        context: state.scroll_context(),
        key: UiKey::new("pressed"),
        bottom: 40.0,
        before: None,
        status: None,
        awaits: false,
        top: 0.0,
        side: None,
        band_gone: false,
        scrolled: false,
        list_top: None,
        segment: None,
    }));
    let _ = crate::work_counts::take_settings_page_work();
    let _ = state.seat_shared(&mut input.clone());
    assert_eq!(crate::work_counts::take_settings_page_work().seats, 1);
    assert!(state.scroll_seats.borrow().as_ref().unwrap().is_empty());
    drop(seats);
    assert!(state.scroll_seats.borrow().is_none());
}

#[test]
fn a_row_whose_second_line_the_strip_holds_is_shown_and_drawn_whole() {
    let plan = |overhang: f32, note: f32, control: f32| {
        let mut plan = ScrollPlan::new(10.0);
        plan.card(80.0, vec![UiKey::new("first")]);
        plan.card(4.0 + control + note, vec![UiKey::new("row")]);
        plan.set_control_rows(vec![(0.0, 4.0, control)]);
        plan.set_ink(4.0 + control + note);
        plan.card(60.0, vec![UiKey::new("after")]);
        plan.card(60.0, Vec::new());
        seat_room(&mut plan, 140.0, Overhang { now: overhang });
        plan
    };
    let shown = plan(44.0, 18.0, 36.0);
    assert_eq!(shown.view(0), 0..2, "the row is shown");
    let screen = shown.screen(0);
    assert_eq!(
        screen.live, 158.0,
        "drawn whole down to its note's end, and on over the gap to the next card"
    );
    assert_eq!(shown.page_after(0), 2, "Page Down turns past it");
    let barred = plan(0.0, 18.0, 36.0);
    assert_eq!(barred.view(0), 0..1);
    assert_eq!(barred.screen(0).live, 140.0);
    assert_eq!(plan(44.0, 18.0, 48.0).view(0), 0..1, "its control");
    assert_eq!(plan(44.0, 60.0, 36.0).view(0), 0..1, "past the strip");
    let mut words = ScrollPlan::new(10.0);
    words.card(80.0, Vec::new());
    words.card(58.0, Vec::new());
    words.card(60.0, Vec::new());
    seat_room(&mut words, 140.0, Overhang { now: 44.0 });
    assert_eq!(words.view(0), 0..1, "words alone are shown in the room");
    assert_eq!(words.screen(0).live, 140.0);
}

#[test]
fn the_first_screens_lead_is_its_first_stop_and_the_first_row_of_controls_after_it() {
    let button = |key: &str| {
        UiNode::new(
            key,
            UiContent::Button(Control::new(
                crate::native_ui::ButtonSpec::new(key),
                crate::native_ui::ActionId::new(key),
            )),
        )
        .layout(Layout::default().height(Length::Fixed(44.0)))
    };
    let words = |key: &str, height: f32| {
        UiNode::new(key, UiContent::Text(crate::native_ui::TextSpec::text(key)))
            .layout(Layout::default().height(Length::Fixed(height)))
    };
    let column = |key: &str, height: f32, gap: f32, children: Vec<UiNode>| {
        UiNode::new(
            key,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::column()
                .height(Length::Fixed(height))
                .padding(Insets::all(12.0))
                .gap(gap),
        )
        .children(children)
    };
    let card = column(
        "card",
        12.0 + 52.0 + 120.0 + 12.0,
        0.0,
        vec![
            words("words", 52.0),
            column(
                "actions",
                120.0,
                8.0,
                vec![button("first"), button("second")],
            ),
        ],
    );
    let mut plan = ScrollPlan::new(10.0);
    plan.card(68.0, vec![UiKey::new("lead")]);
    plan.set_control_rows(vec![(0.0, 12.0, 44.0)]);
    plan.card(196.0, focus_keys(&card));
    plan.measure(&card, 196.0);
    plan.set_room(400.0);
    assert_eq!(plan.first_screen_need(), 78.0 + 120.0);
    let mut quiet = ScrollPlan::new(10.0);
    quiet.card(68.0, vec![UiKey::new("lead")]);
    quiet.card(30.0, Vec::new());
    quiet.card(196.0, focus_keys(&card));
    quiet.measure(&card, 196.0);
    quiet.set_room(400.0);
    assert_eq!(quiet.first_screen_need(), 118.0 + 120.0);
    let mut alone = ScrollPlan::new(10.0);
    alone.card(90.0, Vec::new());
    alone.set_room(400.0);
    assert_eq!(alone.first_screen_need(), 90.0);
}

#[test]
fn page_up_ends_on_the_stop_before_the_screen_it_leaves() {
    let plan = cards(&[30.0, 30.0, 90.0, 30.0, 30.0, 30.0, 30.0], 100.0, 10.0);
    assert_eq!(plan.view(4), 4..6);
    assert_eq!(plan.page_before(4), 3);
    assert_eq!((plan.view(3), plan.view(2)), (3..5, 2..3));
    let (top, nudge) = plan.page_up(4);
    assert_eq!((top, nudge), (2, Some(30.0)), "3 ends at the room's edge");
    let mut landed = plan.clone();
    landed.seated = nudge.map(|nudge| Seat::nudged(top, nudge));
    assert_eq!(landed.shown(top), 3..4, "the tall card cut at the top");
    assert_eq!(landed.page_after(top), 4, "Page Down comes back");
    let mut padded = ScrollPlan::new(10.0);
    for _ in 0..6 {
        padded.card(50.0, Vec::new());
        padded.set_ink_top(10.0);
    }
    padded.set_room(105.0);
    let last = padded.limit();
    assert_eq!((last, padded.view(last)), (4, 4..5));
    assert_eq!(padded.screen(last).offset, 5.0, "scrolled into 4's padding");
    assert_eq!(padded.shown(last), 4..6, "4 whole under its cut padding");
    assert_eq!(padded.page_up(last), (3, None));
    assert_eq!(padded.shown(3), 3..4);
    let even = cards(&[30.0; 8], 100.0, 10.0);
    assert_eq!(even.page_up(3), (1, None));
    assert_eq!(even.view(1).end, 3);
}

#[test]
fn a_screen_holds_every_whole_stop_that_fits_and_never_none() {
    let plan = cards(&[40.0, 40.0, 40.0, 40.0], 100.0, 10.0);
    assert_eq!(plan.view(0), 0..2);
    assert_eq!(plan.view(3), 3..4);
    let tall = cards(&[300.0, 40.0], 100.0, 10.0);
    assert_eq!(
        tall.view(0),
        0..1,
        "a stop taller than the room is still shown"
    );
}

#[test]
fn the_last_screen_is_full() {
    let plan = cards(&[40.0, 40.0, 40.0, 40.0, 40.0], 100.0, 10.0);
    assert_eq!(plan.limit(), 3);
    assert_eq!(plan.view(plan.limit()), 3..5);
    assert_eq!(cards(&[40.0, 40.0], 100.0, 10.0).limit(), 0);
    assert_eq!(cards(&[], 100.0, 10.0).limit(), 0);
}

#[test]
fn page_up_shows_the_screen_that_ends_where_the_screen_began() {
    let plan = cards(&[40.0; 7], 100.0, 10.0);
    assert_eq!(plan.page_before(4), 2);
    assert_eq!(plan.view(plan.page_before(4)), 2..4);
    assert_eq!(plan.page_before(1), 0);
    assert_eq!(plan.page_before(0), 0);
    let tall = cards(&[40.0, 300.0, 40.0], 100.0, 10.0);
    assert_eq!(tall.page_before(2), 1, "a tall stop is a screen of its own");
}

#[test]
fn reveal_moves_the_least_that_shows_the_stop() {
    let plan = cards(&[40.0; 6], 100.0, 10.0);
    assert_eq!(plan.reveal(1, 0), 0);
    assert_eq!(plan.reveal(3, 0), 2);
    assert_eq!(plan.reveal(0, 3), 0);
    assert_eq!(plan.reveal(5, 0), 4);
}

#[test]
fn a_split_card_run_measures_as_the_card_it_draws() {
    let text = |key: &str| {
        UiNode::new(
            key,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
    };
    let card = SplitCard {
        key: "card".to_string(),
        spec: GroupSpec::new("Card").style(StyleRef::Secondary),
        padding: Insets::all(20.0),
        gap: 8.0,
        head: vec![(text("head"), 22.0)],
        repeat: Vec::new(),
        rows: (0..5)
            .map(|row| (text(&format!("row/{row}")), 30.0))
            .collect(),
    };
    let mut plan = ScrollPlan::new(10.0);
    let group = plan.group(card.height_of(0..0), card.height_of_lead());
    for (_, height) in &card.rows {
        plan.row(group, height + card.gap, Vec::new());
    }
    for rows in [0..1, 1..4, 2..5] {
        assert_eq!(plan.span(rows.clone()), card.height_of(rows));
    }
}

#[test]
fn a_runs_next_stop_starts_where_its_card_draws_it() {
    let text = |key: &str| {
        UiNode::new(
            key,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
    };
    for head in [false, true] {
        for bottom in [0.0, 2.0, 4.0, 20.0] {
            let card = SplitCard {
                key: "card".to_string(),
                spec: GroupSpec::new("Card"),
                padding: Insets {
                    top: 6.0,
                    bottom,
                    ..Insets::default()
                },
                gap: 4.0,
                head: if head {
                    vec![(text("head"), 24.0)]
                } else {
                    Vec::new()
                },
                repeat: Vec::new(),
                rows: (0..4)
                    .map(|row| (text(&format!("row/{row}")), 24.0))
                    .collect(),
            };
            let mut plan = ScrollPlan::new(6.0);
            let group = plan.group(card.height_of_head(), card.height_of_lead());
            plan.set_row_gap(group, card.gap);
            for (_, height) in &card.rows {
                plan.row(group, height + card.gap, Vec::new());
            }
            let last = plan.card(32.0, Vec::new());
            for from in 0..last {
                let at = format!("head {head}, padding {bottom}, from row {from}");
                let Length::Fixed(drawn) = card.node(from..card.rows.len()).layout.height else {
                    panic!("{at}: a split card is drawn at a fixed height");
                };
                assert_eq!(
                    plan.stop_top(from, last),
                    drawn + plan.gap(),
                    "{at}: where the card starts"
                );
                let tall = drawn + plan.gap() + 32.0;
                plan.set_room(tall);
                assert_eq!(plan.view(from), from..last + 1, "{at}: shown");
                plan.set_room(tall - 1.0);
                assert_eq!(plan.view(from), from..last, "{at}: the control");
            }
        }
    }
}

#[test]
fn a_card_never_leaves_its_words_on_a_screen_without_their_control() {
    let words = |key: &str| words(key);
    let control = |key: &str| {
        UiNode::new(
            key,
            UiContent::Custom(AuditedCustomNode {
                audit_id: MESSAGES_SCROLL_AUDIT,
                role: SemanticRole::Button,
                label: String::new(),
                paint: None,
                action: None,
                focusable: true,
                focused: false,
                focus_visible: false,
                progress: None,
                motion: None,
            }),
        )
    };
    let card = SplitCard {
        key: "card".to_string(),
        spec: GroupSpec::new("Card"),
        padding: Insets::all(10.0),
        gap: 5.0,
        head: Vec::new(),
        repeat: Vec::new(),
        rows: vec![
            (words("description"), 20.0),
            (control("first"), 40.0),
            (control("second"), 40.0),
            (words("caption"), 20.0),
        ],
    };
    assert_eq!(card.stops(1_000.0), vec![0..2, 2..3, 3..4]);
    assert_eq!(card.stops(60.0), vec![0..1, 1..2, 2..3, 3..4]);

    let status = UiNode::new(
        "status",
        UiContent::Group(GroupSpec {
            label: Some("Audio unavailable".to_string()),
            role: SemanticRole::Status,
            style: StyleRef::Quiet,
        }),
    )
    .children(vec![words("status/words")]);
    let card = SplitCard {
        rows: vec![
            (control("switch"), 40.0),
            (status, 10.0),
            (control("next"), 20.0),
        ],
        ..card
    };
    assert_eq!(card.stops(1_000.0), vec![0..2, 2..3]);
    assert_eq!(card.stops(60.0), vec![0..1, 1..2, 2..3]);
    let card = SplitCard {
        rows: vec![
            (control("switch"), 40.0),
            (words("next/words"), 10.0),
            (control("next"), 20.0),
        ],
        ..card
    };
    assert_eq!(card.stops(1_000.0), vec![0..1, 1..3]);
}

#[test]
fn a_note_on_the_rows_over_it_is_a_stop_of_its_own() {
    let control = |key: &str| {
        UiNode::new(
            key,
            UiContent::Custom(AuditedCustomNode {
                audit_id: MESSAGES_SCROLL_AUDIT,
                role: SemanticRole::Button,
                label: String::new(),
                paint: None,
                action: None,
                focusable: true,
                focused: false,
                focus_visible: false,
                progress: None,
                motion: None,
            }),
        )
    };
    let lines = |key: &str, role: SemanticRole| {
        UiNode::new(
            key,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .children(
            (0..2)
                .map(|line| {
                    UiNode::new(
                        format!("{key}/line-{line}"),
                        UiContent::Text(crate::native_ui::TextSpec {
                            text: "words".to_string(),
                            role,
                            style: StyleRef::Quiet,
                        }),
                    )
                    .paint_only()
                })
                .collect(),
        )
    };
    let card = |rows: Vec<(UiNode, f32)>| SplitCard {
        key: "card".to_string(),
        spec: GroupSpec::new("Card"),
        padding: Insets::all(10.0),
        gap: 5.0,
        head: Vec::new(),
        repeat: Vec::new(),
        rows,
    };
    let shared = card(vec![
        (control("color"), 40.0),
        (control("accent"), 40.0),
        (lines("shared-note", SemanticRole::Status), 40.0),
        (control("bloom"), 40.0),
    ]);
    assert_eq!(shared.stops(1_000.0), vec![0..1, 1..2, 2..3, 3..4]);
    let status_first = card(vec![
        (lines("status", SemanticRole::Status), 30.0),
        (control("install"), 40.0),
    ]);
    assert_eq!(status_first.stops(1_000.0), vec![0..2]);
    let description = card(vec![
        (control("switch"), 40.0),
        (lines("next/state", SemanticRole::Text), 30.0),
        (control("next"), 40.0),
    ]);
    assert_eq!(description.stops(1_000.0), vec![0..1, 1..3]);
}

/// A line of words, which a list draws (a row that draws nothing is no stop of its own).
fn words(key: &str) -> UiNode {
    UiNode::new(
        key,
        UiContent::Text(crate::native_ui::TextSpec {
            text: key.to_string(),
            role: SemanticRole::Text,
            style: StyleRef::Plain,
        }),
    )
}

#[test]
fn a_switch_drawn_whole_over_its_cards_caption_is_turned_past() {
    let control = UiNode::new(
        "switch",
        UiContent::Custom(AuditedCustomNode {
            audit_id: MESSAGES_SCROLL_AUDIT,
            role: SemanticRole::Button,
            label: String::new(),
            paint: None,
            action: None,
            focusable: true,
            focused: false,
            focus_visible: false,
            progress: None,
            motion: None,
        }),
    );
    let card = SplitCard {
        key: "card".to_string(),
        spec: GroupSpec::new("Card"),
        padding: Insets::all(10.0),
        gap: 5.0,
        head: Vec::new(),
        repeat: Vec::new(),
        rows: vec![
            (control, 40.0),
            (
                UiNode::new(
                    "caption",
                    UiContent::Text(crate::native_ui::TextSpec::text("caption")),
                ),
                20.0,
            ),
        ],
    };
    let lead = UiNode::new("lead", UiContent::Group(GroupSpec::new("Lead")));
    let after = UiNode::new("after", UiContent::Group(GroupSpec::new("After")));
    let items = [
        ListItem::Card(Box::new(lead), 100.0),
        ListItem::Split(card),
        ListItem::Card(Box::new(after), 160.0),
    ];
    let (mut plan, _) = plan_items(&items, 10.0, 170.0);
    seat_room(&mut plan, 170.0, Overhang::NONE);
    let switch = plan.stop_of(&UiKey::new("switch")).expect("listed");
    assert!(
        plan.shown(0).contains(&switch),
        "the first screen counts the switch: {:?}",
        plan.shown(0)
    );
    assert!(
        plan.page_after(0) > switch,
        "Page Down turns past it, to the caption"
    );
    assert_eq!(plan.len(), 4, "the switch and the caption are a stop each");
}

#[test]
fn a_card_of_words_is_a_stop_per_row_and_a_disabled_control_keeps_its_words() {
    let card = SplitCard {
        key: "facts".to_string(),
        spec: GroupSpec::new("Facts"),
        padding: Insets::all(10.0),
        gap: 5.0,
        head: Vec::new(),
        repeat: Vec::new(),
        rows: (0..4)
            .map(|row| (words(&format!("fact/{row}")), 20.0))
            .collect(),
    };
    assert_eq!(card.stops(1_000.0), vec![0..1, 1..2, 2..3, 3..4]);

    let mut slider = Control::new(
        crate::native_ui::SliderSpec {
            description: None,
            track: None,
            label: "Brightness".to_string(),
            step: 0.05,
            display_value: "50%".to_string(),
        },
        crate::native_ui::ActionId::new("brightness"),
    );
    slider.state.enabled = false;
    let card = SplitCard {
        key: "color".to_string(),
        spec: GroupSpec::new("Color"),
        padding: Insets::all(10.0),
        gap: 5.0,
        head: Vec::new(),
        repeat: Vec::new(),
        rows: vec![
            (words("wheel"), 100.0),
            (UiNode::new("brightness", UiContent::Slider(slider)), 40.0),
        ],
    };
    assert_eq!(
        card.stops(1_000.0),
        vec![0..2],
        "the wheel keeps with the unavailable slider it belongs to"
    );
}

fn chooser_plan(foot: ListFoot) -> ScrollPlan {
    let mut plan = ScrollPlan::new(0.0);
    for index in 0..2 {
        plan.card(32.0, vec![UiKey::new(format!("lead/{index}"))]);
    }
    let section = plan.group(32.0, 32.0);
    for index in 0..6 {
        plan.row(section, 32.0, vec![UiKey::new(format!("row/{index}"))]);
    }
    plan.card(40.0, vec![UiKey::new("disclosure")]);
    for index in 0..2 {
        plan.card(32.0, vec![UiKey::new(format!("footer/{index}"))]);
    }
    plan.set_foot(foot);
    plan.set_room(128.0);
    plan
}

#[test]
fn a_list_that_ends_on_whole_stops_draws_none_cut() {
    let plan = chooser_plan(ListFoot::Whole);
    for top in 0..=plan.limit() {
        let screen = plan.screen(top);
        assert_eq!(screen.drawn, screen.view, "screen {top}");
        assert!(screen.offset.abs() < 0.01, "screen {top}: {screen:?}");
        assert!(
            plan.span(screen.drawn.clone()) <= plan.room() + 0.01,
            "screen {top}: {screen:?}"
        );
        assert_eq!(plan.page_up(top).1, None, "Page Up from {top}");
        for index in screen.view.clone() {
            assert_eq!(plan.press_nudge(index, top), None, "{index} on {top}");
        }
        let node = scroll_list_screen(
            "list",
            "List",
            &plan,
            screen.view.clone(),
            screen.drawn.clone(),
            Vec::new(),
        );
        let thumb = node
            .children
            .iter()
            .find(|child| child.key.as_str() == "list/thumb")
            .expect("a scrolled list has its thumb");
        assert_eq!(thumb.layout.height, Length::Fixed(plan.room()), "{top}");
    }
    let last = plan.screen(plan.limit());
    assert_eq!(last.view.end, plan.len(), "the last screen ends the list");
    let peek = chooser_plan(ListFoot::Peek);
    assert!(
        (0..=peek.limit()).any(|top| {
            let screen = peek.screen(top);
            peek.span(screen.drawn.clone()) > peek.room() + 0.01
        }),
        "the control draws past the room"
    );
    assert!(
        peek.screen(peek.limit()).offset > 0.01,
        "the control's last screen is scrolled into its first stop"
    );
    assert!(
        (0..=peek.limit()).any(|top| {
            let screen = peek.screen(top);
            (screen.view.end..screen.drawn.end).any(|index| peek.press_nudge(index, top).is_some())
        }),
        "a press on the control's cut stop nudges it past a stop"
    );
}

#[test]
fn a_press_on_a_control_drawn_only_in_the_strip_nudges_it_to_the_rooms_edge() {
    let mut plan = ScrollPlan::new(2.0);
    for index in 0..5 {
        plan.card(40.0, vec![UiKey::new(format!("control/{index}"))]);
    }
    seat_room(&mut plan, 100.0, Overhang { now: 60.0 });
    let screen = plan.screen(0);
    assert_eq!(screen.view, 0..2, "two cards in the room");
    assert!(
        screen.drawn.contains(&3),
        "the fourth is drawn in the strip"
    );
    assert_eq!(
        plan.press_nudge(3, 0),
        Some(66.0),
        "126..166 becomes 60..100"
    );
    plan.seated = Some(Seat::nudged(0, 66.0));
    assert!(plan.shows_controls(3, 0), "whole in the room");
    assert_eq!(plan.press_nudge(3, 0), None, "whole moves nothing");
}

#[test]
fn a_screen_draws_on_to_the_first_stop_its_edge_cuts() {
    let plan = cards(&[40.0; 5], 100.0, 2.0);
    let view = plan.view(0);
    assert_eq!(view, 0..2, "82 of 100 holds two cards");
    assert_eq!(drawn_stops(&plan, &view), 0..3, "the third peeks 16pt");
    let shallow = cards(&[40.0; 5], 100.0, 9.0);
    assert_eq!(
        drawn_stops(&shallow, &shallow.view(0)),
        0..2,
        "a card that would show 2pt is not drawn"
    );
    let node = scroll_list_screen("list", "List", &plan, view, 0..3, Vec::new());
    assert_eq!(
        node.layout.height,
        Length::Fixed(plan.room()),
        "the peek is cut at the list's room"
    );

    let mut pressed = ScrollPlan::new(2.0);
    for index in 0..5 {
        pressed.card(40.0, vec![UiKey::new(format!("control/{index}"))]);
    }
    pressed.set_room(100.0);
    assert!(!pressed.shows_controls(2, 0));
    let nudge = pressed.press_nudge(2, 0).expect("the third control is cut");
    assert_eq!(nudge, 24.0); // 84..124 becomes 60..100.
    assert!(pressed.nudge_fits(0, nudge));
    pressed.seated = Some(Seat::nudged(0, nudge));
    assert!(pressed.shows_controls(2, 0));
    assert_eq!(pressed.press_nudge(2, 0), None, "whole moves nothing");
    let shown = pressed.view(0);
    let drawn = drawn_stops(&pressed, &shown);
    let node = scroll_list_screen("pressed", "List", &pressed, shown, drawn, Vec::new());
    let list = node
        .children
        .iter()
        .find(|node| node.key.as_str() == "pressed")
        .unwrap();
    assert_eq!(list.layout.offset, 24.0, "the list uses the same nudge");
    pressed.seated = None;
    assert!(!pressed.shows_controls(2, 0), "seating on stops drops it");
    pressed.set_room(300.0);
    assert!(!pressed.nudge_fits(0, nudge), "a whole list needs no nudge");
}

#[test]
fn a_screen_whose_edge_falls_between_cards_ends_on_the_card_before() {
    let mut plan = ScrollPlan::new(5.0);
    let group = plan.group(40.0, 20.0);
    plan.row(group, 60.0, Vec::new());
    plan.row(group, 40.0, Vec::new());
    plan.card(50.0, Vec::new());
    plan.set_room(145.0);
    let view = plan.view(0);
    assert_eq!(view, 0..2);
    let screen = plan.screen(0);
    assert_eq!(screen.drawn, 0..2);
    assert_eq!(screen.height, plan.room(), "drawn on to the edge");
    assert!(
        plan.card_bottom(0, 1) >= plan.room() - plan.gap() - 0.01,
        "blank under it only the gap"
    );
    assert_eq!(plan.shown(0), 0..2, "the next screen starts after it");
    let whole = cards(&[60.0, 60.0, 40.0], 130.0, 5.0);
    let screen = whole.screen(0);
    assert_eq!(screen.drawn, 0..2);
    assert_eq!(screen.height, whole.room());
    let mut fading = plan.clone();
    seat_room(&mut fading, 105.0, Overhang { now: 40.0 });
    let screen = fading.screen(0);
    assert_eq!((screen.view, screen.drawn), (0..1, 0..2));
    assert_eq!(screen.height, 145.0, "drawn on to the page's bottom");
}

#[test]
fn a_caption_is_kept_while_its_rows_scroll_under_it_and_a_nudge_never_brings_one_back() {
    let mut plan = ScrollPlan::new(10.0);
    let group = plan.group(50.0, 40.0);
    plan.set_row_gap(group, 4.0);
    plan.set_caption(group, 14.0, 36.0);
    plan.row(group, 66.0, vec![UiKey::new("a")]);
    plan.row(group, 66.0, vec![UiKey::new("b")]);
    plan.set_room(200.0);
    let tail = plan.top_caption(1..2, 64.0);
    assert_eq!(
        tail, None,
        "64pt into a 62pt run: let go (past its words, so none to hide)"
    );
    assert_eq!(
        plan.top_caption(1..2, 48.0).map(|caption| caption.kept),
        None
    );
    let mut gapless = plan.clone();
    gapless.set_row_gap(group, 0.0);
    assert_eq!(
        gapless.top_caption(1..2, 48.0).map(|caption| caption.kept),
        Some(true)
    );
    assert_eq!(
        gapless.top_caption(1..2, 64.0).map(|caption| caption.kept),
        None
    );
    assert_eq!(
        plan.top_caption(1..2, 44.0),
        Some(TopCaption {
            lead: 40.0,
            kept: true
        })
    );
    assert_eq!(
        plan.top_caption(0..2, 64.0).map(|caption| caption.kept),
        Some(true),
        "two rows under it: kept"
    );
    let let_go = Screen {
        view: 1..2,
        drawn: 1..2,
        offset: 64.0,
        height: 200.0,
        live: 200.0,
        fades: false,
        whole: false,
        caption: tail,
        edge_caption: None,
        bare: false,
    };
    let nudged = plan.nudged(let_go.clone(), -20.0, false);
    assert_eq!(nudged.offset, 44.0);
    assert!(
        nudged.caption.is_none_or(|caption| !caption.kept),
        "{:?}",
        nudged.caption
    );
    assert_eq!(plan.caption_floor(nudged.caption, &nudged.drawn, 1), 0.0);
    let kept = Screen {
        offset: 44.0,
        caption: plan.top_caption(1..2, 44.0),
        ..let_go
    };
    let nudged = plan.nudged(kept, -2.0, false);
    assert_eq!(nudged.caption.map(|caption| caption.kept), Some(true));
    assert_eq!(plan.caption_floor(nudged.caption, &nudged.drawn, 1), 40.0);
}

#[test]
fn a_hard_edge_cuts_no_control_live_and_no_caption_through() {
    let plan = |caption: bool| {
        let mut plan = ScrollPlan::new(10.0);
        plan.card(100.0, Vec::new());
        let group = plan.group(52.0, 40.0);
        if caption {
            plan.set_caption(group, 12.0, 34.0);
        }
        plan.row(group, 44.0, vec![UiKey::new("a")]);
        plan.row(group, 44.0, vec![UiKey::new("b")]);
        plan
    };
    let mut cut = plan(true);
    cut.set_room(130.0);
    let screen = cut.screen(0);
    assert_eq!((screen.view, screen.drawn), (0..1, 0..2));
    assert_eq!(screen.height, 130.0, "the card goes on to the edge");
    assert_eq!(
        screen.edge_caption,
        Some((20.0, 34.0)),
        "the caption the edge cuts paints none of its words"
    );
    let mut past = plan(true);
    past.set_room(170.0);
    let screen = past.screen(0);
    assert_eq!(
        (screen.height, screen.live, screen.edge_caption),
        (170.0, 150.0, None),
        "past the caption: the row the edge cuts fades from its top"
    );
    let mut fading = plan(true);
    seat_room(&mut fading, 100.0, Overhang { now: 30.0 });
    let screen = fading.screen(0);
    assert_eq!(
        (screen.height, screen.edge_caption),
        (130.0, None),
        "a fading strip draws on to the page's bottom"
    );
    let mut unknown = plan(false);
    unknown.set_room(130.0);
    let screen = unknown.screen(0);
    assert_eq!((screen.height, screen.edge_caption), (130.0, None));
}

#[test]
fn a_stop_taller_than_the_page_is_cut_by_the_lists_edge() {
    let tall = cards(&[150.0, 40.0], 100.0, 10.0);
    assert_eq!(tall.view(0), 0..1, "the first stop is shown");
    assert_eq!(tall.screen(0).height, 100.0, "cut at the list's room");
    let alone = cards(&[150.0], 100.0, 10.0);
    assert_eq!(alone.screen(0).height, 100.0, "a lone stop too");
    let fits = cards(&[90.0], 100.0, 10.0);
    assert_eq!(fits.screen(0).height, 90.0);
}

#[test]
fn the_last_screen_is_full_with_the_stop_before_it_cut_at_the_top() {
    let plan = cards(&[40.0, 40.0, 40.0, 40.0, 40.0], 100.0, 10.0);
    let last = plan.view(plan.limit());
    assert_eq!(last, 3..5);
    let drawn = drawn_stops(&plan, &last);
    assert_eq!(drawn, 2..5, "the stop before the last screen peeks over it");
    let node = scroll_list_screen("list", "List", &plan, last, drawn, Vec::new());
    let list = node
        .children
        .iter()
        .find(|child| child.key.as_str() == "list")
        .expect("the list");
    assert_eq!(list.layout.height, Length::Fixed(100.0));
    assert_eq!(list.layout.offset, plan.span(2..5) - 100.0);
    let exact = cards(&[50.0, 40.0, 50.0], 100.0, 10.0);
    let last = exact.view(exact.limit());
    assert_eq!(
        drawn_stops(&exact, &last),
        last,
        "a last screen its stops fill draws nothing over them"
    );

    let mut pressed = ScrollPlan::new(10.0);
    for index in 0..5 {
        pressed.card(40.0, vec![UiKey::new(format!("control/{index}"))]);
    }
    pressed.set_room(110.0);
    let top = pressed.limit();
    assert_eq!(pressed.view(top), 3..5);
    assert_eq!(pressed.screen(top).offset, 30.0);
    assert!(!pressed.shows_controls(2, top));
    let nudge = pressed.press_nudge(2, top).expect("the top control is cut");
    assert_eq!(nudge, -30.0); // -30..10 becomes 0..40.
    assert!(pressed.nudge_fits(top, nudge));
    assert!(!pressed.nudge_fits(top, -31.0));
    pressed.seated = Some(Seat::nudged(top, nudge));
    assert!(pressed.shows_controls(2, top));
    assert_eq!(pressed.press_nudge(2, top), None, "whole moves nothing");
    assert_eq!(pressed.screen(top).offset, 0.0);
    assert_eq!(
        pressed.press_nudge(pressed.len(), top),
        None,
        "absent stop refuses"
    );
}

#[test]
fn a_pages_own_list_draws_its_thumb_in_the_page_gutter() {
    use crate::native_ui::{LogicalRect, UiTree};
    use crate::pane_chrome::{SplitChrome, with_split_chrome};
    use crate::widget::DrawPrim;
    let (page_w, gutter) = (900.0_f32, 75.0_f32);
    let edge = page_w - gutter;
    let plan = cards(&[120.0; 6], 300.0, 12.0);
    let view = plan.view(1);
    let drawn = drawn_stops(&plan, &view);
    let compile = |seated: bool| {
        let children = drawn
            .clone()
            .map(|stop| {
                UiNode::new(
                    format!("card-{stop}"),
                    UiContent::Group(GroupSpec::new("Card").style(StyleRef::Secondary)),
                )
                .layout(Layout::column().height(Length::Fixed(120.0)))
            })
            .collect();
        let screen = scroll_list_screen(
            "settings/list",
            "List",
            &plan,
            view.clone(),
            drawn.clone(),
            children,
        );
        let mut page_children = vec![screen];
        if seated {
            seat_page_scroller(&mut page_children, gutter);
        }
        let page = UiNode::new("page", UiContent::Group(GroupSpec::new("Page")))
            .layout(
                Layout::column()
                    .padding(Insets {
                        top: 20.0,
                        right: gutter,
                        bottom: 20.0,
                        left: 30.0,
                    })
                    .clipped(),
            )
            .children(page_children);
        UiTree::new(page)
            .compile(LogicalRect::new(0.0, 0.0, page_w, 400.0))
            .expect("compiles")
    };
    let thumbs = |compiled: &crate::native_ui::CompiledUi| -> Vec<(f32, f32, [u8; 4])> {
        compiled
            .tray(aterm_render::Theme::default(), 13.0)
            .prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Panel { x, w, h, fill, .. } if *w <= 3.0 && *h > 0.0 => {
                    Some((*x, *w, *fill))
                }
                _ => None,
            })
            .collect()
    };
    let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
    for (chrome, width) in [(SplitChrome::Fluent, 2.0_f32), (SplitChrome::Classic, 3.0)] {
        let seated = compile(true);
        let thumb = seated
            .paint
            .iter()
            .find(|node| node.key.as_str() == "settings/list/thumb")
            .expect("a scrolled list has its thumb");
        assert!(
            (thumb.rect.right() - page_w).abs() < 0.01
                && (thumb.clip.right() - page_w).abs() < 0.01,
            "{chrome:?}: the thumb's node spans the gutter: {:?} {:?}",
            thumb.rect,
            thumb.clip
        );
        for node in seated
            .paint
            .iter()
            .filter(|node| node.key.as_str().starts_with("card-"))
        {
            assert!(
                node.rect.right() <= edge + 0.01,
                "{chrome:?}: {} keeps to the content edge: {:?}",
                node.key.as_str(),
                node.rect
            );
        }
        let painted = with_split_chrome(chrome, || thumbs(&seated));
        let alpha = crate::scrollbar::rest_alpha(crate::scrollbar::ALPHA_REST, true);
        let [r, g, b] =
            crate::scrollbar::over_ground(roles.text_primary, roles.surface, alpha, alpha).0;
        assert_eq!(
            painted,
            vec![(page_w - 2.0 - width, width, [r, g, b, 255])],
            "{chrome:?}: the terminal's thumb and resting tone, 2 pt from the page's edge"
        );
        let unseated = with_split_chrome(chrome, || thumbs(&compile(false)));
        assert!(
            unseated.len() == 1 && unseated[0].0 + unseated[0].1 <= edge,
            "{chrome:?}: CONTROL: unseated, inside the cards' column: {unseated:?}"
        );
    }
    let mut card_list = vec![
        UiNode::new(
            "settings/messages/scroll",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .children(vec![
            UiNode::new(
                "settings/messages",
                UiContent::Group(GroupSpec::new("Log").style(StyleRef::Secondary)),
            ),
            UiNode::new(
                "settings/messages/thumb",
                UiContent::Custom(AuditedCustomNode {
                    audit_id: MESSAGES_SCROLL_AUDIT,
                    role: SemanticRole::Group,
                    label: String::new(),
                    paint: Some("0,2,5".to_string()),
                    action: None,
                    focusable: false,
                    focused: false,
                    focus_visible: false,
                    progress: None,
                    motion: None,
                }),
            )
            .paint_only(),
        ]),
    ];
    seat_page_scroller(&mut card_list, gutter);
    assert_eq!(
        card_list[0].layout.outdent,
        Insets::default(),
        "a card's list"
    );
}

#[test]
fn a_group_run_pays_its_caption_on_every_screen() {
    let mut plan = ScrollPlan::new(10.0);
    let card = plan.card(30.0, Vec::new());
    let group = plan.group(50.0, 30.0);
    for _ in 0..4 {
        plan.row(group, 48.0, Vec::new());
    }
    plan.set_room(200.0);
    assert_eq!(plan.span(1..3), 50.0 + 96.0);
    assert_eq!(plan.span(0..2), 30.0 + 10.0 + 50.0 + 48.0);
    assert_eq!(plan.view(1), 1..4);
    assert_eq!(plan.view(plan.limit()), 2..5);
    assert_eq!(
        plan.runs(0..5),
        vec![
            ScrollRun::Card(card),
            ScrollRun::Group { group, rows: 1..5 }
        ]
    );
}

#[test]
fn a_screen_lets_its_caption_go_where_it_and_the_row_cannot_share_the_room() {
    let mut plan = ScrollPlan::new(4.0);
    let group = plan.group(60.0, 50.0);
    plan.set_row_gap(group, 4.0);
    plan.set_caption(group, 14.0, 46.0);
    for key in ["a", "b", "c"] {
        plan.row(group, 68.0, vec![UiKey::new(key)]);
        plan.set_control_rows(vec![(0.0, 0.0, 64.0)]);
        plan.set_ink(64.0);
    }
    plan.set_room(100.0);
    for stop in 0..3 {
        assert!(plan.starts_bare(stop), "row {stop}: 50pt + 64pt > 100pt");
        let screen = plan.screen(stop);
        assert!(
            screen.bare && (screen.offset >= 50.0 - 0.01 || stop == plan.limit()),
            "{screen:?}"
        );
        assert!(
            screen.caption.is_none_or(|caption| !caption.kept),
            "{screen:?}"
        );
        assert!(plan.shown(stop).contains(&stop), "row {stop} is whole");
        assert!(plan.shows_controls(stop, stop));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut top = 0;
    loop {
        seen.extend(plan.shown(top));
        let next = plan.page_after(top);
        if next == top {
            break;
        }
        top = next;
    }
    assert_eq!(seen.len(), 3, "Page Down shows every row whole: {seen:?}");
    let mut fits = ScrollPlan::new(4.0);
    let group = fits.group(60.0, 50.0);
    fits.row(group, 44.0, vec![UiKey::new("a")]);
    fits.row(group, 44.0, vec![UiKey::new("b")]);
    fits.set_room(100.0);
    assert!(!fits.starts_bare(0) && !fits.starts_bare(1));
    assert!(!fits.screen(0).bare);
}

#[test]
fn a_stop_a_bare_screen_shows_whole_is_not_cut() {
    let rows = |height: f32| {
        let mut plan = ScrollPlan::new(4.0);
        let group = plan.group(60.0, 50.0);
        plan.set_row_gap(group, 4.0);
        plan.set_caption(group, 14.0, 46.0);
        for key in ["a", "b", "c"] {
            plan.row(group, height + 6.0, vec![UiKey::new(key)]);
            plan.set_control_rows(vec![(0.0, 44.0, height - 44.0)]);
            plan.set_ink(height);
        }
        plan.set_room(156.0);
        plan
    };
    let fits = rows(108.0);
    assert!((0..3).all(|stop| fits.starts_bare(stop)), "50 + 108 > 156");
    assert_eq!(fits.cut_stops(), 0, "each row is whole on its bare screen");
    let tall = rows(160.0);
    assert_eq!(tall.cut_stops(), 3, "a row taller than the room is cut");
}

#[test]
fn the_last_screen_lets_its_caption_go_rather_than_cut_its_stop() {
    let mut plan = ScrollPlan::new(12.0);
    let first = plan.group(60.0, 50.0);
    plan.row(first, 84.0, vec![UiKey::new("a")]);
    plan.set_control_rows(vec![(0.0, 4.0, 72.0)]);
    plan.set_ink(76.0);
    plan.set_ink_top(4.0);
    let last = plan.group(60.0, 50.0);
    plan.set_caption(last, 14.0, 46.0);
    plan.row(last, 84.0, vec![UiKey::new("badge")]);
    plan.set_control_rows(vec![(0.0, 4.0, 72.0)]);
    plan.set_ink(76.0);
    plan.set_ink_top(4.0);
    plan.set_room(132.0);
    let top = plan.limit();
    assert_eq!(top, 1);
    let screen = plan.screen(top);
    assert!(screen.offset > 0.01, "scrolled to the card's end");
    assert!(
        screen.caption.is_none_or(|caption| !caption.kept),
        "{screen:?}"
    );
    assert!(plan.shown(top).contains(&1), "the badge row is whole");
}

#[test]
fn a_reveal_by_stops_keeps_the_stop_it_reveals_on_screen() {
    let mut plan = ScrollPlan::new(10.0);
    plan.card(150.0, vec![UiKey::new("tall")]);
    plan.set_ink(140.0);
    plan.card(40.0, vec![UiKey::new("next")]);
    plan.card(40.0, vec![UiKey::new("last")]);
    plan.set_room(120.0);
    assert!(!plan.shows_span(0, 0, (0.0, 150.0)));
    assert_eq!(plan.reveal_span(0, 0, (0.0, 150.0)), 0);
    assert_eq!(plan.land_selected(0, 0), (0, None));
}

fn adjust_card(now: f32) -> ScrollPlan {
    let mut plan = ScrollPlan::new(10.0);
    for key in ["a", "b", "c"] {
        plan.card(100.0, vec![UiKey::new(key)]);
    }
    let group = plan.group(52.0, 40.0);
    for (key, height) in [("r0", 50.0), ("r1", 50.0), ("r2", 50.0), ("r3", 68.0)] {
        plan.row(group, height, vec![UiKey::new(key)]);
        plan.set_control_rows(vec![(0.0, 4.0, height - 8.0)]);
        plan.set_ink(height - 4.0);
        plan.set_ink_top(4.0);
    }
    plan.row(group, 72.0, vec![UiKey::new("r4")]);
    plan.set_control_rows(vec![(0.0, 4.0, 36.0)]);
    plan.set_ink(70.0);
    plan.set_ink_top(4.0);
    seat_room(&mut plan, 326.0, Overhang { now });
    plan
}

#[test]
fn the_screen_that_draws_the_lists_end_is_its_last() {
    let plan = adjust_card(44.0);
    assert_eq!(plan.limit(), 3, "the screen seated at the card's first row");
    let screen = plan.screen(3);
    let end = plan.card_bottom(screen.drawn.start, plan.len() - 1) - screen.offset;
    assert!(
        (end - (plan.room() + plan.overhang())).abs() < 0.01 && screen.drawn.start < 3,
        "the list's end at the strip's foot, the card before peeking in: {screen:?} {end}"
    );
    assert_eq!(plan.shown(3), 3..8, "it shows every row, the last too");
    assert_eq!(plan.page_after(2), 3);
    let before = plan.shown(2);
    assert!(
        plan.shown(3).any(|stop| !before.contains(&stop)),
        "the turn shows what the screen before it did not"
    );
    let barred = adjust_card(0.0);
    let last = barred.limit();
    let screen = barred.screen(last);
    let end = barred.card_bottom(screen.drawn.start, barred.len() - 1) - screen.offset;
    assert!(
        (end - barred.room()).abs() < 0.01,
        "with a bar up the list ends at the room's edge: {end}"
    );
}

#[test]
fn with_a_bar_up_the_last_screen_shows_the_last_card_whole() {
    let mut plan = ScrollPlan::new(10.0);
    for key in ["a", "b", "c", "d"] {
        plan.card(80.0, vec![UiKey::new(key)]);
    }
    let group = plan.group(40.0, 24.0);
    plan.row(group, 60.0, vec![UiKey::new("last")]);
    plan.set_control_rows(vec![(0.0, 8.0, 36.0)]);
    plan.set_ink(52.0);
    plan.set_ink_top(8.0);
    let end = |now: f32| {
        let mut plan = plan.clone();
        seat_room(&mut plan, 200.0, Overhang { now });
        let screen = plan.screen(plan.limit());
        let at = plan.stop_top(screen.drawn.start, plan.len() - 1) - screen.offset;
        (
            plan.card_bottom(screen.drawn.start, plan.len() - 1) - screen.offset,
            at + plan.controls_of(plan.len() - 1).1,
            screen.height,
        )
    };
    let (bottom, controls, height) = end(0.0);
    assert!(
        bottom <= 200.0 + 0.01,
        "the last card whole in the room: {bottom}"
    );
    assert!(
        height >= bottom - 0.01,
        "drawn to its end: {height} {bottom}"
    );
    let (bottom, controls_free, _) = end(44.0);
    assert!(bottom <= 244.0 + 0.01 && controls_free <= 200.0 + 0.01);
    assert!(controls <= 200.0 + 0.01);
}

#[test]
fn a_control_over_the_rooms_edge_is_a_picture_from_its_top() {
    let mut plan = ScrollPlan::new(10.0);
    plan.card(200.0, vec![UiKey::new("first")]);
    plan.card(60.0, vec![UiKey::new("row")]);
    plan.set_control_spans(vec![(UiKey::new("row"), 12.0, 36.0)]);
    plan.set_control_rows(vec![(0.0, 12.0, 36.0)]);
    plan.card(60.0, vec![UiKey::new("after")]);
    seat_room(&mut plan, 255.0, Overhang { now: 44.0 });
    let screen = plan.screen(0);
    assert_eq!(plan.shown(0), 0..1);
    assert_eq!(
        screen.live, 222.0,
        "live and at full strength to the switch's top"
    );
    assert!(screen.fades);
    assert!(!plan.draws_controls_live(1, 0), "the switch is a picture");
    assert_eq!(plan.page_after(0), 1, "the next screen shows it");
    let mut card = ScrollPlan::new(10.0);
    card.card(150.0, vec![UiKey::new("lead")]);
    card.card(140.0, vec![UiKey::new("one"), UiKey::new("two")]);
    card.set_control_spans(vec![
        (UiKey::new("one"), 20.0, 44.0),
        (UiKey::new("two"), 72.0, 44.0),
    ]);
    card.set_control_rows(vec![(0.0, 20.0, 96.0)]);
    card.card(60.0, Vec::new());
    seat_room(&mut card, 250.0, Overhang { now: 44.0 });
    assert_eq!(card.screen(0).live, 232.0);
    assert!(card.draws_control_live(1, &UiKey::new("one"), 0));
    assert!(!card.draws_control_live(1, &UiKey::new("two"), 0));
    assert_eq!(
        card.live_control(1, &UiKey::new("one"), 0),
        Some(&UiKey::new("one"))
    );
    assert_eq!(
        card.live_control(1, &UiKey::new("two"), 0),
        Some(&UiKey::new("one"))
    );
    assert_eq!(card.live_control(2, &UiKey::new("two"), 0), None);
    let whole = cards(&[100.0, 100.0, 100.0], 230.0, 10.0);
    assert_eq!(whole.screen(0).live, 230.0);
}

/// THE LIST'S END LEAVES NO ROW CUT TO NOTHING UNDER ITS CAPTION: a card whose first row
/// puts its words over its switch (a compact row at 2x text), and whose end the last
/// screen anchors so that row's switch is cut away under the caption kept over the rows,
/// starts that screen at the next row, the room left under the list's end, where it drew
/// a blank band under the caption. CONTROL: where the switch shows under the caption, the
/// screen is anchored on the end as before.
#[test]
fn the_last_screen_draws_no_row_cut_to_nothing_under_its_caption() {
    let plan = |room: f32| {
        let mut plan = ScrollPlan::new(10.0);
        let group = plan.group(54.0, 40.0);
        plan.set_caption(group, 14.0, 36.0);
        plan.row(group, 108.0, vec![UiKey::new("gpu")]);
        plan.set_control_rows(vec![(0.0, 44.0, 64.0)]);
        plan.set_ink(108.0);
        plan.row(group, 144.0, vec![UiKey::new("rewind")]);
        plan.set_control_rows(vec![(0.0, 80.0, 64.0)]);
        plan.set_ink(144.0);
        seat_room(&mut plan, room, Overhang::NONE);
        plan
    };
    let cut = plan(230.0);
    assert_eq!(cut.limit(), 1);
    let screen = cut.screen(1);
    assert_eq!(
        (screen.drawn.clone(), screen.offset),
        (1..2, 0.0),
        "the cut row is not drawn: {screen:?}"
    );
    assert_eq!(cut.shown(1), 1..2, "the last row shown whole");
    assert!(
        cut.card_bottom(1, 1) < cut.room(),
        "the room left goes under the list's end"
    );
    let shows = plan(268.0);
    let screen = shows.screen(shows.limit());
    assert_eq!(
        screen.drawn.start, 0,
        "a row whose switch shows under the caption is drawn: {screen:?}"
    );
    assert!(screen.offset > 0.0);
}

#[test]
fn a_controls_name_line_and_control_must_fit_together() {
    let key = UiKey::new("settings/control/named");
    let row = UiKey::new("settings/row/named");
    let mut plan = ScrollPlan::new(4.0);
    let group = plan.group(0.0, 0.0);
    plan.row(group, 100.0, vec![key.clone()]);
    plan.set_control_rows(vec![(0.0, 52.0, 48.0)]);
    plan.set_control_spans(vec![(key.clone(), 52.0, 48.0)]);
    plan.set_names(RowNames {
        lines: vec![(row.clone(), 0.0, 24.0), (row.clone(), 24.0, 48.0)],
        controls: vec![(key.clone(), row)],
        self_named: Vec::new(),
    });
    plan.set_room(72.0);
    assert!(48.0 < plan.room(), "the control alone fits");
    assert!(
        !plan.named_control_fits(&key),
        "a live control still needs its name"
    );
    plan.set_room(76.0);
    assert!(
        plan.named_control_fits(&key),
        "one complete name line suffices"
    );
    assert!(100.0 > plan.room(), "the full two-line name need not fit");
    let mut names = plan.names.last().expect("the measured row").clone();
    names.self_named.push((key.clone(), 52.0));
    plan.set_names(names);
    plan.set_room(48.0);
    assert!(
        plan.named_control_fits(&key),
        "a self-naming button needs no extra row name"
    );
}

#[test]
fn a_name_only_cut_remains_visible_to_the_chrome_fit() {
    let key = UiKey::new("settings/control/copy_on_select");
    let row = UiKey::new("settings/row/copy_on_select");
    let mut plan = ScrollPlan::new(4.0);
    let group = plan.group(0.0, 0.0);
    // Two distinct stops: a 24pt name line and its 4pt gap, then a 48pt switch.
    // Each stop fits 72pt; their shortest identifying span needs 76pt.
    plan.row(
        group,
        28.0,
        vec![UiKey::new("settings/label/copy_on_select")],
    );
    plan.set_ink(24.0);
    plan.set_control_rows(Vec::new());
    plan.set_names(RowNames {
        lines: vec![(row.clone(), 0.0, 24.0)],
        ..RowNames::default()
    });
    plan.row(group, 48.0, vec![key.clone()]);
    plan.set_control_spans(vec![(key.clone(), 0.0, 48.0)]);
    plan.set_names(RowNames {
        controls: vec![(key.clone(), row)],
        ..RowNames::default()
    });
    let mut state = SettingsViewState::new(&crate::app_config::Config::default());
    state.common.last_focus = Some(key.clone());
    state.reveal_control(key);
    assert!(
        state.keyboard_name_reveal().is_some(),
        "the declared Bool owes its name"
    );

    plan.set_room(72.0);
    let short = super::super::PageFit::of(&state, &plan);
    assert_eq!(short.cuts, (0, 0), "physical stops are whole");
    assert!(
        !short.complete(),
        "zero physical cuts cannot hide the missing name"
    );
    assert!(!short.name_complete);

    plan.set_room(76.0);
    let named = super::super::PageFit::of(&state, &plan);
    assert!(named.complete());
    assert!(
        named.better_than(short, true),
        "a toolbar alternative can supply the name without changing physical cuts"
    );
    assert!(!short.better_than(named, true));

    state.reveal_as = Reveal::Hold;
    plan.set_room(72.0);
    assert!(
        super::super::PageFit::of(&state, &plan).complete(),
        "a held place does not acquire a new keyboard-name obligation"
    );
}

#[test]
fn the_last_screen_drops_a_hidden_control_with_words_left_beside_it() {
    let mut plan = ScrollPlan::new(5.0);
    let first = plan.group(50.0, 40.0);
    plan.set_caption(first, 14.0, 36.0);
    plan.row(first, 48.0, vec![UiKey::new("color")]);
    plan.set_control_rows(vec![(0.0, 4.0, 36.0)]);
    plan.set_control_spans(vec![(UiKey::new("color"), 4.0, 36.0)]);
    plan.set_names(RowNames {
        lines: vec![
            (UiKey::new("name"), 5.0, 21.0),
            (UiKey::new("name"), 23.0, 39.0),
        ],
        controls: vec![(UiKey::new("color"), UiKey::new("name"))],
        self_named: Vec::new(),
    });
    plan.set_ink(40.0);
    plan.set_ink_top(4.0);
    plan.set_drawn_bands(vec![
        DrawnPart {
            top: 5.0,
            bottom: 21.0,
            whole: true,
        },
        DrawnPart {
            top: 23.0,
            bottom: 39.0,
            whole: true,
        },
        DrawnPart {
            top: 4.0,
            bottom: 40.0,
            whole: false,
        },
    ]);
    let after = plan.group(50.0, 40.0);
    plan.set_caption(after, 14.0, 36.0);
    for (key, height) in [("next", 48.0), ("middle", 66.0), ("last", 66.0)] {
        plan.row(after, height, vec![UiKey::new(key)]);
        plan.set_control_rows(vec![(0.0, 4.0, 36.0)]);
        plan.set_ink(height - 4.0);
        plan.set_ink_top(4.0);
    }
    seat_room(&mut plan, 272.0, Overhang::NONE);
    // The first name line ends at 61, but its second line at 63..79 is whole
    // beside the control's remaining 19pt. No line starts below the control at 80.
    assert!(plan.veils_controls_from(0, 61.0));
    assert!(!plan.draws_nothing_from(0, 61.0));
    assert_eq!(plan.screen(plan.limit()).drawn.start, 1);
    assert!(
        !plan.veils_controls_from(0, 40.0),
        "the whole control stays"
    );
    assert!(!plan.veils_controls_from(0, 82.0), "no surviving words");
    assert!(plan.draws_nothing_from(0, 82.0));
    seat_room(&mut plan, 310.0, Overhang::NONE);
    assert_eq!(
        plan.screen(plan.limit()).drawn.start,
        0,
        "a whole row stays"
    );
}

#[test]
fn a_screen_that_ends_on_a_whole_card_scrolls_on_to_peek_the_next() {
    let plan = |padding: f32| {
        let mut plan = ScrollPlan::new(12.0);
        for key in ["a", "b", "c", "d", "e", "f"] {
            plan.card(136.0, vec![UiKey::new(key)]);
            plan.set_control_rows(vec![(0.0, padding, 136.0 - 2.0 * padding)]);
            plan.set_ink(136.0 - padding);
            plan.set_ink_top(padding);
        }
        seat_room(&mut plan, 300.0, Overhang::NONE);
        plan
    };
    let padded = plan(12.0);
    let screen = padded.screen(1);
    assert_eq!(screen.offset, 12.0, "scrolled on into b's padding");
    assert!(screen.drawn.contains(&3), "d peeks in");
    assert_eq!(
        padded.card_top(screen.drawn.start, 3) - screen.offset,
        300.0 - PEEK_MIN,
        "PEEK_MIN of d under the edge"
    );
    assert_eq!(padded.shown(1), 1..3, "b and c shown whole, as seated");
    assert_eq!(padded.page_after(1), 3);
    assert_eq!(padded.screen(0).offset, 0.0);
    let flush = plan(8.0);
    assert_eq!(flush.screen(1).offset, 0.0);
    assert!(!flush.screen(1).drawn.contains(&3));
}

#[test]
fn a_kept_caption_never_comes_down_over_a_row_the_scroll_left_whole() {
    let mut plan = ScrollPlan::new(10.0);
    plan.card(100.0, vec![UiKey::new("before")]);
    let group = plan.group(54.0, 40.0);
    plan.set_caption(group, 14.0, 36.0);
    for key in ["a", "b", "c", "d"] {
        plan.row(group, 44.0, vec![UiKey::new(key)]);
        plan.set_control_rows(vec![(0.0, 4.0, 36.0)]);
        plan.set_ink(40.0);
        plan.set_ink_top(4.0);
    }
    seat_room(&mut plan, 200.0, Overhang::NONE);
    let seated = plan.stop_screen(1);
    assert_eq!((seated.offset, seated.drawn.start), (0.0, 1));
    let nudged = plan.nudged(seated.clone(), 16.0, false);
    assert!(
        nudged.caption.is_none_or(|caption| !caption.kept),
        "the caption scrolls with its card: {nudged:?}"
    );
    assert_eq!(
        plan.shown_on(&nudged.drawn, nudged.offset, nudged.caption)
            .start,
        1,
        "the row the nudge left whole is shown"
    );
    let past = plan.nudged(seated, 48.0, false);
    assert!(past.caption.is_some_and(|caption| caption.kept), "{past:?}");
}
