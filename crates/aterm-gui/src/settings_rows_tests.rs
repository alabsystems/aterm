// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Settings row's anatomy (its label line, second line, Reset's place),
//! the list's peek, drafts and refusals, the bars, and the keyboard.

use super::modified::{Lab, rect_of};
use super::*;

fn painted_text(compiled: &crate::native_ui::CompiledUi, prefix: &str) -> String {
    compiled
        .paint
        .iter()
        .filter(|node| !node.peek && node.key.as_str().starts_with(prefix))
        .filter_map(|node| match &node.content {
            UiContent::Text(spec) => Some(spec.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn dispatch(lab: &mut Lab, event: AppEvent) {
    lab.dispatch(event);
}

/// Page Down, each press drawn at `width`×`height` as the window draws
/// between presses, until a press moves the list no further: its last
/// screen, as the 40 presses these tests made before reached it after its
/// few screens (test-run speed: the presses past the end were most of their
/// layouts). That frame.
fn to_the_last_screen(lab: &mut Lab, width: f32, height: f32) -> crate::native_ui::CompiledUi {
    let shown = |lab: &Lab| {
        (
            lab.state().page_scroll,
            lab.state().scroll_shown().map(|(_, view)| view),
        )
    };
    // The first screen, drawn before the first press.
    let _ = lab.compile(width, height);
    for _ in 0..40 {
        let at = shown(lab);
        dispatch(lab, AppEvent::ScrollPage(1));
        let compiled = lab.compile(width, height);
        if shown(lab) == at {
            return compiled;
        }
    }
    panic!("forty presses of Page Down never reached the list's end")
}

/// A wide window whose Security list scrolls: the shipped 1074×684 where the
/// page holds This Mac's Universal Control and Secure Keyboard Entry (macOS's
/// rows: `row_available_here`, the platform half of a row's admission), a
/// shorter one where its six other rows fit 684 whole.
fn wide_scrolling_security() -> (f32, f32) {
    let macos_rows = row_available_here(
        prefs::EDIT_MACHINE_UNIVERSAL_CONTROL,
        SettingsAvailability::runtime(true),
    );
    (1_074.0, if macos_rows { 684.0 } else { 560.0 })
}

/// A switch keeps its place when its Reset comes: every row keeps Reset's
/// place, on a wide page and a compact one.
#[test]
fn a_switch_stays_where_it_is_when_its_reset_comes() {
    for (width, height) in [(1_074.0, 684.0), (584.0, 348.0)] {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Security);
        let key = format!("settings/control/{}", prefs::EDIT_ALLOW_OSC52_QUERY);
        let before = rect_of(&lab.compile(width, height), &key);
        lab.commit(
            &format!("settings/set/{}", prefs::EDIT_ALLOW_OSC52_QUERY),
            Some(SemanticInput::Bool(true)),
        );
        let compiled = lab.compile(width, height);
        let after = rect_of(&compiled, &key);
        assert!(
            compiled
                .semantic(&UiKey::new(format!(
                    "settings/reset/{}",
                    prefs::EDIT_ALLOW_OSC52_QUERY
                )))
                .is_some(),
            "{width}×{height}: the changed row offers Reset"
        );
        assert_eq!(
            (before.x, before.width),
            (after.x, after.width),
            "{width}×{height}: the switch never moves or shrinks"
        );
    }
}

/// A value set on a color field's control over a refused draft saves, and
/// the stale draft goes with it; Reset drops a draft too.
#[test]
fn a_valid_color_over_a_refused_draft_clears_the_draft() {
    let mut lab = Lab::new("");
    let set = format!("settings/set/{}", prefs::EDIT_FOREGROUND);
    assert!(
        lab.invoke(&set, Some(SemanticInput::Text("orange".to_string())))
            .is_none(),
        "orange is refused"
    );
    assert!(lab.state().field_has_unsaved_draft(prefs::EDIT_FOREGROUND));
    lab.commit(&set, Some(SemanticInput::Text("#FFAA00".to_string())));
    assert!(lab.text().contains("foreground = \"#FFAA00\""));
    assert!(
        !lab.state()
            .field_inputs
            .contains_key(prefs::EDIT_FOREGROUND)
    );
    assert_eq!(lab.state().unsaved_field_draft_count(), 0);

    let mut lab = Lab::new("background = \"#202040\"\n");
    let _ = lab.invoke(
        &format!("settings/set/{}", prefs::EDIT_BACKGROUND),
        Some(SemanticInput::Text("nope".to_string())),
    );
    assert!(lab.state().field_has_unsaved_draft(prefs::EDIT_BACKGROUND));
    lab.commit(&format!("settings/reset/{}", prefs::EDIT_BACKGROUND), None);
    assert!(
        !lab.state()
            .field_inputs
            .contains_key(prefs::EDIT_BACKGROUND)
    );
    assert!(!lab.text().contains("background"));
}

/// A refused value's reason lives as long as its draft: on Modified too,
/// spoken by a Status and by the field's name, never telling Return will
/// save it; discarding the draft takes the reason.
#[test]
fn a_refused_value_says_why_wherever_its_draft_is() {
    let mut lab = Lab::new("");
    let key = prefs::EDIT_SEARCH_HISTORY_LINES;
    let _ = lab.invoke(
        &format!("settings/set/{key}"),
        Some(SemanticInput::Text("lots".to_string())),
    );
    assert!(lab.state().field_errors.contains_key(key));
    lab.state_mut().navigate(SettingsRoute::Modified);
    assert!(
        lab.state().field_errors.contains_key(key),
        "the reason outlives the page change while the draft lives"
    );
    let compiled = lab.compile(900.0, 820.0);
    let error = compiled
        .semantic(&UiKey::new(format!("settings/error/{key}")))
        .expect("Modified says why the draft was not saved");
    assert_eq!(error.role, SemanticRole::Status);
    assert!(error.label.contains("was not saved"), "{}", error.label);
    let control = compiled
        .semantic(&UiKey::new(format!("settings/control/{key}")))
        .expect("the field");
    assert!(control.state.is_some_and(|state| state.invalid));
    // The field's description says why, and how to put it right; its name
    // is the setting's.
    let description = control.description.clone().unwrap_or_default();
    assert!(description.contains("was not saved"), "{description}");
    assert!(description.contains("correct it"), "{description}");
    assert!(
        !description.contains("press Return to save"),
        "{description}"
    );
    assert!(
        !control.label.contains("was not saved"),
        "{}",
        control.label
    );

    let _ = lab.invoke("settings/drafts/discard-all", None);
    let _ = lab.invoke("settings/drafts/discard-all", None);
    assert!(lab.state().field_inputs.is_empty());
    assert!(
        lab.state().field_errors.is_empty(),
        "the reason goes with its draft"
    );
}

/// Reset All asks with the keyboard on Cancel, and answering No puts it
/// back on Reset All…, never on a button the question turned off.
#[test]
fn reset_all_puts_the_keyboard_on_cancel_and_back() {
    let mut lab = Lab::new("cursor_blink = false\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    let _ = lab.compile(1_074.0, 684.0);
    let _ = lab.invoke("settings/reset-all", None);
    assert_eq!(
        lab.state().common.last_focus.as_ref().map(UiKey::as_str),
        Some("settings/reset-all-cancel")
    );
    let compiled = lab.compile(1_074.0, 684.0);
    let cancel = rect_of(&compiled, "settings/reset-all-cancel");
    let confirm = rect_of(&compiled, "settings/reset-all-confirm");
    assert!(
        cancel.width < 160.0 && confirm.width < 160.0 && cancel.right() <= confirm.x,
        "Cancel, then Reset All, each as wide as its words: {cancel:?} {confirm:?}"
    );
    dispatch(&mut lab, AppEvent::TextInput(TextInputEvent::Cancel));
    assert_eq!(
        lab.state().common.last_focus.as_ref().map(UiKey::as_str),
        Some("settings/modified/reset-all")
    );
    let compiled = lab.compile(1_074.0, 684.0);
    assert!(
        compiled
            .semantic(&UiKey::new("settings/modified/reset-all"))
            .and_then(|node| node.state)
            .is_some_and(|state| state.enabled)
    );
}

/// Home and End move the list to its top and bottom, and take the keyboard
/// with it as Page Up and Page Down do: End to the last control of the
/// bottom screen, Home to the first of the top one, ringed, so the next
/// arrow steps on from there instead of jumping the list back.
#[test]
fn home_and_end_move_the_list_to_its_ends() {
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Appearance);
    let _ = lab.compile(1_074.0, 684.0);
    let first = lab
        .state()
        .scroll_plan()
        .and_then(|plan| plan.controls().next().map(|(_, key)| key.clone()))
        .expect("the list's first control");
    dispatch(&mut lab, AppEvent::FocusChanged(Some(first.clone())));
    let _ = lab.compile(1_074.0, 684.0);
    dispatch(
        &mut lab,
        AppEvent::TextInput(TextInputEvent::End { extend: false }),
    );
    let _ = lab.compile(1_074.0, 684.0);
    let (plan, shown) = lab.state().scroll_shown().expect("a list");
    assert_eq!(shown.end, plan.len(), "End shows the last stop");
    assert!(shown.start > 0);
    let last_on_screen = plan
        .controls()
        .filter(|(stop, _)| shown.contains(stop))
        .last()
        .map(|(_, key)| key.clone());
    assert_eq!(
        lab.state().common.last_focus,
        last_on_screen,
        "End takes the keyboard to the bottom screen's last control"
    );
    assert!(lab.state().common.focus_visible, "and rings it");
    dispatch(&mut lab, AppEvent::StepFocus(-1));
    let _ = lab.compile(1_074.0, 684.0);
    let (_, after) = lab.state().scroll_shown().expect("a list");
    assert_eq!(
        after.end,
        plan.len(),
        "Up steps back; the list stays at its end"
    );
    dispatch(
        &mut lab,
        AppEvent::TextInput(TextInputEvent::Home { extend: false }),
    );
    let _ = lab.compile(1_074.0, 684.0);
    let (_, shown) = lab.state().scroll_shown().expect("a list");
    assert_eq!(shown.start, 0, "Home shows the first stop");
    assert_eq!(
        lab.state().common.last_focus.as_ref(),
        Some(&first),
        "Home takes the keyboard to the list's first control"
    );
    assert!(lab.state().common.focus_visible);
}

/// Home and End on a sidebar row move the keyboard to the sidebar's first or
/// last row and never open it, and the page's list stays where it is. Page
/// Down from a sidebar row turns the list and takes the keyboard into it, so
/// the Home after it goes back to the list's top, never to the sidebar; Page
/// Up does the same, to the screen's first control too.
#[test]
fn the_sidebar_keys_act_on_what_the_keyboard_is_in() {
    let (width, height) = wide_scrolling_security();
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Security);
    for (event, row) in [
        (TextInputEvent::Home { extend: false }, "settings/nav/home"),
        (TextInputEvent::End { extend: false }, "settings/nav/about"),
    ] {
        lab.state_mut().common.last_focus = Some(UiKey::new("settings/nav/security"));
        let _ = lab.compile(width, height);
        dispatch(&mut lab, AppEvent::TextInput(event));
        let _ = lab.compile(width, height);
        let state = lab.state();
        assert_eq!(
            state.route,
            SettingsRoute::Security,
            "{row}: the page stays"
        );
        assert_eq!(state.page_scroll, 0, "{row}: and so does its list");
        assert_eq!(
            state.common.last_focus.as_ref().map(UiKey::as_str),
            Some(row)
        );
        assert!(state.common.focus_visible);
    }

    lab.state_mut().common.last_focus = Some(UiKey::new("settings/nav/security"));
    let _ = lab.compile(width, height);
    dispatch(&mut lab, AppEvent::ScrollPage(1));
    let _ = lab.compile(width, height);
    let (plan, shown) = lab.state().scroll_shown().expect("a list");
    assert!(shown.start > 0, "Page Down turns the list from the sidebar");
    let first_on_screen = plan
        .controls()
        .find(|(stop, _)| shown.contains(stop))
        .map(|(_, key)| key.clone());
    assert_eq!(
        lab.state().common.last_focus,
        first_on_screen,
        "and takes the keyboard to the first control it shows"
    );
    assert!(lab.state().common.focus_visible);
    dispatch(
        &mut lab,
        AppEvent::TextInput(TextInputEvent::Home { extend: false }),
    );
    let _ = lab.compile(width, height);
    let (plan, shown) = lab.state().scroll_shown().expect("a list");
    assert_eq!(shown.start, 0, "Home then takes the list to its top");
    assert_eq!(
        lab.state().common.last_focus.as_ref(),
        plan.controls().next().map(|(_, key)| key),
        "and the keyboard to its first control"
    );
    assert_eq!(lab.state().route, SettingsRoute::Security);

    // Page Up from the sidebar at the list's top: the list cannot turn back,
    // and the keyboard goes to the screen's first control, never down to its
    // last.
    lab.state_mut().common.last_focus = Some(UiKey::new("settings/nav/security"));
    let _ = lab.compile(width, height);
    dispatch(&mut lab, AppEvent::ScrollPage(-1));
    let _ = lab.compile(width, height);
    let (plan, shown) = lab.state().scroll_shown().expect("a list");
    assert_eq!(shown.start, 0, "the list stays at its top");
    assert_eq!(
        lab.state().common.last_focus.as_ref(),
        plan.controls().next().map(|(_, key)| key),
        "Page Up takes the keyboard to the screen's first control"
    );
}

/// Return and Escape in a text field end the edit, never the keyboard: it
/// stays on the field, ringed, and Down goes on to the next control, where
/// it used to land on nothing and the next Tab started over at the toolbar.
#[test]
fn return_and_escape_in_a_field_keep_the_keyboard_on_it() {
    let field = UiKey::new(format!("settings/control/{}", prefs::EDIT_SCROLLBACK));
    for submit in [true, false] {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Terminal);
        let _ = lab.compile(738.0, 420.0);
        dispatch(&mut lab, AppEvent::FocusChanged(Some(field.clone())));
        dispatch(
            &mut lab,
            AppEvent::TextInput(TextInputEvent::Commit("5000".to_string())),
        );
        if submit {
            lab.commit_event(AppEvent::TextInput(TextInputEvent::Submit));
            assert!(lab.text().contains("scrollback_lines = 5000"));
        } else {
            dispatch(&mut lab, AppEvent::TextInput(TextInputEvent::Cancel));
            assert!(!lab.text().contains("scrollback_lines"));
        }
        let compiled = lab.compile(738.0, 420.0);
        let state = lab.state();
        assert_eq!(state.editing_field, None, "submit={submit}: the edit ends");
        assert_eq!(
            state.common.last_focus.as_ref(),
            Some(&field),
            "submit={submit}: the keyboard stays on the field"
        );
        assert!(state.common.focus_visible, "submit={submit}: ringed");
        assert!(
            compiled
                .semantic(&field)
                .and_then(|node| node.state)
                .is_some_and(|state| state.focused),
            "submit={submit}: the field draws as focused"
        );
        dispatch(&mut lab, AppEvent::StepFocus(1));
        let _ = lab.compile(738.0, 420.0);
        let next = lab.state().common.last_focus.clone();
        let plan = lab.state().scroll_plan().expect("a list");
        let order: Vec<&UiKey> = plan.controls().map(|(_, key)| key).collect();
        let after = order.iter().position(|key| **key == field).map(|at| at + 1);
        assert_eq!(
            next.as_ref(),
            after.and_then(|at| order.get(at).copied()),
            "submit={submit}: Down goes on to the control after the field"
        );
    }
}

/// Home and End on the compact category sheet reach its first and last
/// category, ringed and on screen.
#[test]
fn home_and_end_reach_the_first_and_last_category() {
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Security);
    lab.state_mut().compact_navigation = true;
    lab.state_mut().common.last_focus = Some(UiKey::new("settings/categories/security"));
    let _ = lab.compile(584.0, 348.0);
    for (event, category) in [
        (
            TextInputEvent::Home { extend: false },
            "settings/categories/home",
        ),
        (
            TextInputEvent::End { extend: false },
            "settings/categories/about",
        ),
    ] {
        dispatch(&mut lab, AppEvent::TextInput(event));
        let compiled = lab.compile(584.0, 348.0);
        let state = lab.state();
        assert_eq!(
            state.common.last_focus.as_ref().map(UiKey::as_str),
            Some(category)
        );
        assert!(state.common.focus_visible);
        assert!(
            compiled.semantic(&UiKey::new(category)).is_some(),
            "{category} is on screen"
        );
    }
}

/// THE ROWS A SCREEN SHOWS NEVER CHANGE FOR A STATUS BAR: the room a page
/// keeps under its list for the bar a change brings up holds only the list's
/// peek, so while no bar is up the list reaches the page's bottom (no blank
/// band), and when the bar comes every control the screen shows is still
/// whole and live where it was, and only the peek gives the bar its room —
/// on a compact page and a wide one.
#[test]
fn a_status_bar_never_takes_a_row_from_the_screen() {
    let key = prefs::EDIT_ALLOW_OSC52_QUERY;
    for (width, height) in [(738.0, 418.0), wide_scrolling_security()] {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Security);
        let control = format!("settings/control/{key}");
        lab.state_mut().common.last_focus = Some(UiKey::new(control.as_str()));
        let before = lab.compile(width, height);
        let (plan, shown) = lab.state().scroll_shown().expect("a list");
        assert!(shown.end < plan.len(), "{width}×{height}: the list scrolls");
        assert!(
            plan.overhang() > 0.0,
            "{width}×{height}: it keeps a bar's room"
        );
        let shown_controls: Vec<&UiKey> = plan
            .controls()
            .filter(|(stop, _)| shown.contains(stop))
            .map(|(_, key)| key)
            .collect();
        let at = |compiled: &crate::native_ui::CompiledUi| {
            shown_controls
                .iter()
                .map(|key| compiled.semantic(key).map(|node| node.rect))
                .collect::<Vec<_>>()
        };
        assert!(at(&before).iter().all(Option::is_some), "{width}×{height}");
        let list = rect_of(&before, "settings/list");
        let page = rect_of(&before, "settings/page/security");
        assert!(
            list.height > plan.room() + 0.5 && list.height <= plan.room() + plan.overhang() + 0.5,
            "{width}×{height}: with no bar up the list draws its peek in the bar's room"
        );
        assert!(
            shown_controls.iter().all(|key| {
                rect_of(&before, key.as_str()).bottom() <= list.y + plan.room() + 0.01
            }),
            "{width}×{height}: no control the screen shows is in the bar's room"
        );
        assert!(list.bottom() <= page.bottom() + 0.01, "{width}×{height}");
        lab.commit(
            &format!("settings/set/{key}"),
            Some(SemanticInput::Bool(true)),
        );
        let after = lab.compile(width, height);
        assert!(
            after.semantic(&UiKey::new("settings/undo")).is_some(),
            "{width}×{height}: the change's bar is up"
        );
        assert_eq!(at(&before), at(&after), "{width}×{height}");
        assert_eq!(
            lab.state().scroll_plan().map(|plan| plan.room()),
            Some(plan.room()),
            "{width}×{height}"
        );
        let list_after = rect_of(&after, "settings/list");
        assert_eq!(list_after.y, list.y, "{width}×{height}");
        assert!(
            list_after.height <= plan.room() + 0.5,
            "{width}×{height}: the bar takes only the peek"
        );
    }
}

/// THE LAST SCREEN IS FULL: on every list page at the common sizes, a last
/// screen whose last stops leave room in the list draws the stop before them
/// over them, cut at the list's top and live over what shows, and draws on
/// to its room's edge, its last stop's controls ending there at the latest
/// and its words no further than the page's bottom (the strip kept for a
/// status bar that is not up) — never a screen that ends on a band of
/// blank, and never one scrolled past its end to hide a cut. CONTROL: some
/// last screen draws such a stop cut.
#[test]
fn the_end_of_a_scrolled_page_fills_its_screen() {
    let mut peeked = 0;
    for route in LIST_ROUTES {
        for (width, height) in LIST_SIZES {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            let _ = lab.compile(width, height);
            let Some(plan) = lab.state().scroll_plan() else {
                continue;
            };
            lab.state_mut().scroll_page_to(plan.limit());
            lab.state_mut().page_reveal = None;
            let compiled = lab.compile(width, height);
            let (plan, shown) = lab.state().scroll_shown().expect("a list");
            let screen = plan.screen(shown.start);
            if shown.end < plan.len() || screen.drawn.start >= shown.start {
                continue;
            }
            let context = format!("{route:?} {width}×{height}");
            let Some((list, room)) = compiled_list(&compiled, &plan) else {
                panic!("{context}: the list");
            };
            let inside = |node: &&crate::native_ui::PaintNode| {
                !matches!(
                    node.key.as_str(),
                    "settings/list"
                        | "settings/top/list"
                        | "updates/list"
                        | "settings/showcase/list"
                ) && !node.clip.is_empty()
                    && node.clip.y >= list.y - 0.01
                    && node.clip.bottom() <= list.bottom() + 0.01
                    && node.rect.x >= list.x - 0.01
                    && node.rect.right() <= list.right() + 0.01
            };
            // Cut at the list's top, or under the caption its group keeps
            // whole there.
            if compiled
                .paint
                .iter()
                .filter(inside)
                .any(|node| node.peek && node.clip.y > node.rect.y + 0.5)
            {
                peeked += 1;
            }
            let drawn_to = compiled
                .paint
                .iter()
                .filter(inside)
                .map(|node| node.clip.bottom())
                .fold(list.y, f32::max);
            assert!(
                drawn_to >= list.y + room - 0.5,
                "{context}: the last screen ends on a band of blank: {drawn_to} vs {}",
                list.y + room
            );
            let ends = |words: bool| {
                compiled
                    .paint
                    .iter()
                    .filter(inside)
                    .filter(|node| {
                        if words {
                            matches!(node.content, UiContent::Text(_))
                        } else {
                            is_control(&node.content)
                        }
                    })
                    .map(|node| node.rect.bottom())
                    .fold(list.y, f32::max)
            };
            let controls = ends(false);
            assert!(
                controls <= list.y + room + 0.5,
                "{context}: the last stop's controls end past the room's edge: {controls} vs {}",
                list.y + room
            );
            let words = ends(true);
            assert!(
                words <= list.y + room + plan.overhang() + 0.5,
                "{context}: the last stop's words end past the page's bottom: {words} vs {}",
                list.y + room + plan.overhang()
            );
            for node in compiled
                .paint
                .iter()
                .filter(inside)
                .filter(|node| is_control(&node.content) && node.clip.y > node.rect.y + 0.5)
            {
                let live = compiled.semantic(&node.key).unwrap_or_else(|| {
                    panic!("{context}: {:?} cut by the top edge, dead", node.key)
                });
                assert!(
                    (live.rect.y - node.clip.y).abs() <= 0.5
                        && (live.rect.bottom() - node.clip.bottom()).abs() <= 0.5,
                    "{context}: {:?} live over {:?}, not what shows {:?}",
                    node.key,
                    live.rect,
                    node.clip
                );
            }
        }
    }
    assert!(peeked > 0, "some last screen draws the stop before it cut");
}

/// The docked compact toolbar's Reset All question and draft bar keep their
/// buttons inside their group and the status box: the last one ends at the
/// box's edge, never past it toward the window's.
#[test]
fn the_docked_bar_keeps_its_buttons_inside_its_box() {
    for variant in ["reset", "draft"] {
        let mut lab = Lab::new("font_px = 14.0\n");
        {
            let state = lab.state_mut();
            state.navigate(SettingsRoute::Terminal);
            if variant == "reset" {
                state.feedback = Some("Reset all settings?".to_string());
                state.reset_all_confirmation = true;
            } else {
                state.field_inputs.insert(
                    prefs::EDIT_SCROLLBACK.to_string(),
                    crate::native_text_input::TextInputState::new("abc".to_string()),
                );
            }
        }
        let compiled = lab.compile(584.0, 348.0);
        let (group, buttons): (&str, &[&str]) = if variant == "reset" {
            (
                "settings/reset-all/actions",
                &["settings/reset-all-cancel", "settings/reset-all-confirm"],
            )
        } else {
            (
                "settings/draft-recovery/actions",
                &["settings/drafts/review", "settings/drafts/discard-all"],
            )
        };
        let feedback = rect_of(&compiled, "settings/feedback");
        let actions = rect_of(&compiled, group);
        for key in buttons {
            let button = rect_of(&compiled, key);
            assert!(
                button.x >= actions.x - 0.01 && button.right() <= actions.right() + 0.01,
                "{variant}: {key} {button:?} runs out of its group {actions:?}"
            );
            assert!(
                button.right() <= feedback.right() + 0.01,
                "{variant}: {key} {button:?} runs past the box {feedback:?}"
            );
        }
        let last = rect_of(&compiled, buttons[buttons.len() - 1]);
        assert!(
            (last.right() - actions.right()).abs() <= 0.5,
            "{variant}: the last button ends at its group's edge"
        );
    }
}

/// The sidebar is one Tab stop: Tab leaves it for the page, entering it
/// lands on the selected row, and its arrows open the next page.
#[test]
fn the_sidebar_is_one_tab_stop_and_its_arrows_open_pages() {
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Terminal);
    let compiled = lab.compile(1_074.0, 684.0);
    let order = compiled.focus_order.clone();
    let key = |key: &str| UiKey::new(key);
    let state = lab.state();
    assert_eq!(
        state.tab_past_navigation(
            Some(&key("settings/search")),
            &key("settings/nav/home"),
            true,
            &order
        ),
        Some(key("settings/nav/terminal")),
        "Tab into the sidebar lands on the selected row"
    );
    let first_page_control = order
        .iter()
        .skip_while(|key| !key.as_str().starts_with("settings/nav/"))
        .find(|key| !key.as_str().starts_with("settings/nav/"))
        .cloned()
        .expect("a page control after the sidebar");
    assert_eq!(
        state.tab_past_navigation(
            Some(&key("settings/nav/terminal")),
            &key("settings/nav/harness"),
            true,
            &order
        ),
        Some(first_page_control),
        "Tab leaves the sidebar in one step"
    );
    assert_eq!(
        state.tab_past_navigation(
            Some(&key("settings/nav/terminal")),
            &key("settings/nav/keyboard-input"),
            false,
            &order
        ),
        Some(key("settings/search")),
        "Shift-Tab leaves it backwards in one step"
    );
    dispatch(
        &mut lab,
        AppEvent::FocusChanged(Some(key("settings/nav/terminal"))),
    );
    dispatch(&mut lab, AppEvent::StepFocus(1));
    assert_eq!(lab.state().route, SettingsRoute::Harness);
    assert_eq!(
        lab.state().common.last_focus.as_ref().map(UiKey::as_str),
        Some("settings/nav/harness"),
        "the keyboard stays in the sidebar"
    );
}

/// SETTINGS ▸ THEMES HAS A RAIL ROW THE KEYBOARD REACHES. Opening it from
/// the rail leaves the keyboard on its own row, a node the tree has (it was
/// a key with no node when the rail listed no Themes row, so the keyboard
/// was lost); Up from it opens Appearance and the row goes, Down opens
/// Wallpaper, and Tab into the sidebar lands on it.
#[test]
fn arrows_and_tab_reach_the_themes_row() {
    let key = |key: &str| UiKey::new(key);
    let focus = |lab: &Lab| {
        lab.state()
            .common
            .last_focus
            .as_ref()
            .map(|key| key.as_str().to_string())
    };
    let open_themes = |lab: &mut Lab| {
        dispatch(
            lab,
            AppEvent::FocusChanged(Some(key("settings/nav/appearance"))),
        );
        let _ = lab.invoke(route_action(SettingsRoute::Themes).as_str(), None);
        assert_eq!(lab.state().route, SettingsRoute::Themes);
    };
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Appearance);
    let _ = lab.compile(1_200.0, 820.0);
    open_themes(&mut lab);
    assert_eq!(focus(&lab).as_deref(), Some("settings/nav/themes"));
    let compiled = lab.compile(1_200.0, 820.0);
    assert!(
        compiled.semantic(&key("settings/nav/themes")).is_some(),
        "the keyboard is on a row the tree has"
    );
    assert!(compiled.focus_order.contains(&key("settings/nav/themes")));
    let order = compiled.focus_order.clone();
    assert_eq!(
        lab.state().tab_past_navigation(
            Some(&key("settings/search")),
            &key("settings/nav/home"),
            true,
            &order
        ),
        Some(key("settings/nav/themes")),
        "Tab into the sidebar lands on the selected Themes row"
    );

    dispatch(&mut lab, AppEvent::StepFocus(-1));
    assert_eq!(lab.state().route, SettingsRoute::Appearance);
    assert_eq!(focus(&lab).as_deref(), Some("settings/nav/appearance"));
    let compiled = lab.compile(1_200.0, 820.0);
    assert!(
        compiled.semantic(&key("settings/nav/themes")).is_none(),
        "the row goes with its page"
    );

    open_themes(&mut lab);
    let _ = lab.compile(1_200.0, 820.0);
    dispatch(&mut lab, AppEvent::StepFocus(1));
    assert_eq!(lab.state().route, SettingsRoute::Wallpaper);
    assert_eq!(focus(&lab).as_deref(), Some("settings/nav/wallpaper"));

    // From Appearance's row while Themes shows, Down is the Themes row: the
    // page it already shows.
    open_themes(&mut lab);
    let _ = lab.compile(1_200.0, 820.0);
    dispatch(
        &mut lab,
        AppEvent::FocusChanged(Some(key("settings/nav/appearance"))),
    );
    dispatch(&mut lab, AppEvent::StepFocus(1));
    assert_eq!(lab.state().route, SettingsRoute::Themes);
    assert_eq!(focus(&lab).as_deref(), Some("settings/nav/themes"));
}

/// The compact category sheet opens on the page it came from, and its arrow
/// keys move down a column and along a row.
#[test]
fn the_category_grid_moves_by_column_and_row() {
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Modified);
    let _ = lab.compile(738.0, 418.0);
    let _ = lab.invoke("settings/compact-navigation", None);
    assert_eq!(
        lab.state().common.last_focus.as_ref().map(UiKey::as_str),
        Some("settings/categories/modified")
    );
    let _ = lab.compile(738.0, 418.0);
    let focus = |lab: &Lab| {
        lab.state()
            .common
            .last_focus
            .as_ref()
            .map(|key| key.as_str().to_string())
    };
    // Top Settings | Modified, then Manual alone: Down lands on Manual.
    dispatch(&mut lab, AppEvent::StepFocus(1));
    assert_eq!(focus(&lab).as_deref(), Some("settings/categories/manual"));
    dispatch(&mut lab, AppEvent::StepFocus(1));
    assert_eq!(
        focus(&lab).as_deref(),
        Some("settings/categories/appearance")
    );
    dispatch(
        &mut lab,
        AppEvent::TextInput(TextInputEvent::Right { extend: false }),
    );
    assert_eq!(
        focus(&lab).as_deref(),
        Some("settings/categories/wallpaper")
    );
    dispatch(&mut lab, AppEvent::StepFocus(-1));
    assert_eq!(focus(&lab).as_deref(), Some("settings/categories/manual"));
}

/// A row's note is drawn inside its own row: Minimum contrast's under it,
/// none under Faint text opacity.
#[test]
fn a_note_is_drawn_in_its_own_row() {
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Appearance);
    let compiled = lab.compile(1_564.0, 2_400.0);
    let row = |key: &str| {
        compiled
            .paint
            .iter()
            .find(|node| {
                node.key.as_str().starts_with("settings/row/")
                    && node.key.as_str().ends_with(&format!("/{key}"))
            })
            .map(|node| node.rect)
            .unwrap_or_else(|| panic!("{key}'s row"))
    };
    let note = compiled
        .paint
        .iter()
        .find(|node| {
            node.key.as_str() == format!("settings/detail/{}/line-0", prefs::EDIT_MINIMUM_CONTRAST)
        })
        .expect("Minimum contrast draws its note");
    let minimum = row(prefs::EDIT_MINIMUM_CONTRAST);
    let next = row(prefs::EDIT_SELECTION_INACTIVE);
    assert!(note.rect.y >= minimum.bottom() - 0.01 && note.rect.bottom() <= next.y + 0.01);
    assert!(
        !painted_text(
            &compiled,
            &format!("settings/detail/{}/", prefs::EDIT_FAINT_OPACITY)
        )
        .contains("leaves colors"),
        "Faint text opacity carries no other row's note"
    );
}

/// When a change takes effect is plain words on the row's second line; a
/// change that applies at once says nothing.
#[test]
fn timing_is_plain_words_and_says_nothing_when_live() {
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Security);
    let compiled = lab.compile(1_074.0, 900.0);
    let key = prefs::EDIT_MACHINE_UNIVERSAL_CONTROL;
    // Universal Control is a Security row only where it actuates (macOS's
    // `defaults`, `row_available_here`); elsewhere it is listed only once
    // written, saying it has no effect (`settings_modified_tests`).
    if row_available_here(key, SettingsAvailability::for_state(lab.state())) {
        assert!(
            painted_text(&compiled, &format!("settings/detail/{key}/"))
                .contains("Applies at the next ALab tools check"),
            "Universal Control says when it applies"
        );
    } else {
        assert!(
            compiled
                .semantic(&UiKey::new(format!("settings/control/{key}")))
                .is_none(),
            "Security offers no Universal Control where it has no effect"
        );
    }
    lab.state_mut().navigate(SettingsRoute::Harness);
    let compiled = lab.compile(584.0, 900.0);
    for key in [
        prefs::EDIT_HARNESS_ENABLED,
        prefs::EDIT_HARNESS_APPROVE,
        prefs::EDIT_HARNESS_ANSWER_QUESTIONS,
        prefs::EDIT_HARNESS_QUESTION_PRIORITY,
    ] {
        let words = format!(
            "{} {}",
            painted_text(&compiled, &format!("settings/label/{key}/visual")),
            painted_text(&compiled, &format!("settings/detail/{key}/"))
        );
        assert!(!words.contains("Now"), "{key}: {words}");
        assert!(!words.contains('…'), "{key}: {words}");
        assert!(!words.contains("launcher"), "{key}: {words}");
    }
}

/// AN UNSAVED DRAFT'S BAR IS AS TALL AS ITS WORDS NEED: where its sentence
/// fits whole beside its buttons (the shipped 102×29 window, on a page and
/// on Modified) it is one row, as tall as the status bar the page keeps room
/// for, so the list keeps its room; a sentence too long for that puts the
/// buttons on a row under it.
#[test]
fn the_draft_bar_is_one_row_where_its_words_fit_beside_its_buttons() {
    let (width, height) = (738.0, 418.0);
    for route in [SettingsRoute::Terminal, SettingsRoute::Modified] {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(route);
        let _ = lab.compile(width, height);
        let room = lab.state().scroll_plan().map(|plan| plan.room());
        lab.state_mut().field_inputs.insert(
            prefs::EDIT_SCROLLBACK.to_string(),
            crate::native_text_input::TextInputState::new("50".to_string()),
        );
        let compiled = lab.compile(width, height);
        let bar = rect_of(&compiled, "settings/feedback");
        assert_eq!(bar.height, scaled_control_height(), "{route:?}: one row");
        let status = rect_of(&compiled, "settings/draft-recovery/status");
        let discard = rect_of(&compiled, "settings/drafts/discard-all");
        assert!(
            discard.y >= bar.y && discard.bottom() <= bar.bottom() + 0.01,
            "{route:?}: Discard sits in the bar's one row: {discard:?} in {bar:?}"
        );
        assert!(status.right() <= discard.x + 0.01, "{route:?}");
        let painted = painted_text(&compiled, "settings/draft-recovery/status/visual");
        assert!(
            painted.ends_with("discard before closing.") || painted.ends_with("or discard."),
            "{route:?}: the words are whole: {painted}"
        );
        if route == SettingsRoute::Terminal {
            assert_eq!(
                lab.state().scroll_plan().map(|plan| plan.room()),
                room,
                "the bar takes only the room the page kept for it"
            );
        }
    }

    // A refused draft's longer sentence does not fit beside its buttons in a
    // window too narrow for it: the buttons go under it. Too narrow is
    // measured in this host's UI face, so the check holds in any face: a bar
    // whose row could not hold the sentence even with no button beside it
    // (its 16pt edges and the 8pt gap before the buttons), while the column
    // under it (8pt edges) holds the sentence whole.
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Terminal);
    let _ = lab.invoke(
        &format!("settings/set/{}", prefs::EDIT_SEARCH_HISTORY_LINES),
        Some(SemanticInput::Text("lots".to_string())),
    );
    let sentence = painted_text(
        &lab.compile(1_074.0, 684.0),
        "settings/draft-recovery/status/visual",
    );
    assert!(sentence.ends_with("or discard it."), "{sentence}");
    let fits = |room: f32| crate::native_ui::fit_native_status_label(&sentence, room) == sentence;
    let need = crate::tray_raster::ui_text_width_for(
        crate::widget::TextFace::Ui,
        &sentence,
        crate::native_ui::native_type_px(crate::type_scale::TypeStep::Caption).get(),
    );
    // The bar spans the page beside the sidebar: a first compile says how
    // much of the window that is at this narrow a width.
    let mut narrow = (need + 28.0).ceil();
    let beside = narrow - rect_of(&lab.compile(narrow, height), "settings/feedback").width;
    narrow += beside;
    let compiled = lab.compile(narrow, height);
    let bar = rect_of(&compiled, "settings/feedback");
    assert!(
        !fits(bar.width - 16.0 - 16.0 - 8.0) && fits(bar.width - 16.0),
        "{narrow}pt: a {}pt bar fits the {need}pt sentence only under its buttons",
        bar.width
    );
    let status = rect_of(&compiled, "settings/draft-recovery/status");
    let discard = rect_of(&compiled, "settings/drafts/discard-all");
    assert!(bar.height > scaled_control_height(), "{bar:?}");
    assert!(discard.y >= status.bottom() - 0.01, "under the words");
    let painted = painted_text(&compiled, "settings/draft-recovery/status/visual");
    assert!(painted.ends_with("or discard it."), "{painted}");
}

/// A FONT ROW'S VALUE IS WHOLE ON EVERY FORM OF ITS ROW (the
/// audit): with the terminal's report of the faces it draws in ("SF Mono
/// Light Italic", "Apple Color Emoji") and a long saved family, Text &
/// Fonts at a short compact window (738x180), a tall one (738x420), a
/// medium (1095x700) and a wide one (1564x712), at 1×, 1.5× and 2× text,
/// paints every control's words whole on every screen — its button takes
/// from the name's column the room its words need, or sets them on lines,
/// or the row stacks. A one-line row kept its 375pt control whatever its
/// value: at 2× text "Automatic (SF Mono Light Italic)" read "Automatic
/// (SF Mono Light…" and "Automatic (Apple Color Emoji)" read "Automatic
/// (Apple Color E…" at every window width, and at 1× a saved "Iosevka Term
/// Curly Slab Extended ExtraLight Italic" lost "ght Italic". The sweeps
/// missed it: with no report the rows say only "Automatic".
#[test]
fn a_font_rows_value_is_whole_on_every_form_of_its_row() {
    let drawn = aterm_render::DrawnFaces {
        regular: Some("SF Mono Light".to_string()),
        regular_has_ligatures: false,
        styles: [
            aterm_render::StyleDraw {
                face: Some("SF Mono Light Bold".to_string()),
                synthetic: aterm_render::StyleBits::REGULAR,
            },
            aterm_render::StyleDraw {
                face: Some("SF Mono Light Italic".to_string()),
                synthetic: aterm_render::StyleBits::REGULAR,
            },
            aterm_render::StyleDraw {
                face: Some("SF Mono Light Italic".to_string()),
                synthetic: aterm_render::StyleBits::BOLD,
            },
        ],
        symbol: Some("Noto Sans Symbols 2".to_string()),
        emoji: Some("Apple Color Emoji".to_string()),
    };
    at_each_text_scale(&[1.0, 1.5, 2.0], "font values", |scale| {
        let mut faults = Vec::new();
        let mut rows = 0;
        for source in [
            "",
            "font_family_bold = \"Iosevka Term Curly Slab Extended ExtraLight Italic\"\n",
        ] {
            for (width, height) in [
                (738.0, 180.0),
                (738.0, 420.0),
                (1_095.0, 700.0),
                (1_564.0, 712.0),
            ] {
                let mut lab = Lab::new(source);
                lab.state_mut().navigate(SettingsRoute::TextFonts);
                assert!(
                    lab.state_mut()
                        .replace_font_families(injected_font_families())
                );
                assert!(lab.state_mut().replace_drawn_faces(Some(drawn.clone())));
                let mut page = 0;
                loop {
                    lab.state_mut().scroll_page_to(page);
                    lab.state_mut().page_reveal = None;
                    let screen = lab.compile(width, height);
                    let context = format!(
                        "{scale}\u{d7} {width}x{height}{} screen {page}",
                        if source.is_empty() { "" } else { " saved" }
                    );
                    for line in screen.paint_audit_lines() {
                        let font = [
                            prefs::EDIT_FONT_FAMILY,
                            prefs::EDIT_FONT_FAMILY_BOLD,
                            prefs::EDIT_FONT_FAMILY_ITALIC,
                            prefs::EDIT_FONT_FAMILY_BOLD_ITALIC,
                            prefs::EDIT_SYMBOL_FONT,
                            prefs::EDIT_EMOJI_FONT,
                        ]
                        .iter()
                        .any(|key| {
                            line.starts_with(&format!("paint-text key=\"settings/control/{key}\""))
                        });
                        if !font {
                            continue;
                        }
                        rows += 1;
                        if line.contains(" overflow=true ")
                            || line.contains("clip-truncated=true")
                            || line
                                .rsplit_once(" painted=")
                                .is_some_and(|(_, painted)| painted.contains('\u{2026}'))
                        {
                            faults.push(format!("{context}: {line}"));
                        }
                    }
                    let limit = lab.state().result_page_limit.get().unwrap_or(0);
                    if page >= limit {
                        break;
                    }
                    page += 1;
                }
            }
        }
        assert!(rows > 0, "{scale}\u{d7}: the font rows are drawn");
        assert!(
            faults.is_empty(),
            "{} cut:\n{}",
            faults.len(),
            faults.join("\n")
        );
    });
}

/// A REFUSED DRAFT'S SENTENCE IS READ WHOLE UNDER ITS BAR'S BUTTONS (the
/// audit): from the 164pt window floor up to where the sentence
/// fits beside its buttons, 1× and 2× text, the unsaved-draft bar sets its
/// words over its buttons on the lines they take — every word on a line,
/// none cut — its words' box holding its lines and its buttons under them.
/// It was cut to one line: a refused draft in a 192pt window read
/// "Scrollback limit was not saved.…", "Correct it and press Return, or
/// discard it." gone.
#[test]
fn a_refused_drafts_sentence_is_read_whole_under_its_buttons() {
    at_each_text_scale(&[1.0, 2.0], "draft words", |scale| {
        let mut checked = 0;
        for width in [164.0, 178.0, 192.0, 234.0, 304.0, 374.0] {
            let height = 432.0;
            let mut lab = Lab::new("");
            lab.state_mut().navigate(SettingsRoute::Terminal);
            let _ = lab.compile(width, height);
            let _ = lab.invoke(
                &format!("settings/set/{}", prefs::EDIT_SCROLLBACK),
                Some(SemanticInput::Text("abc".to_string())),
            );
            let compiled = lab.compile(width, height);
            let context = format!("{scale}\u{d7} {width}pt");
            let status = compiled
                .semantic(&UiKey::new("settings/draft-recovery/status"))
                .unwrap_or_else(|| panic!("{context}: the draft's status is up"));
            assert!(
                status.label.ends_with("or discard it."),
                "{context}: {}",
                status.label
            );
            // Under the page the bar paints its sentence; docked in the
            // compact toolbar, its authored short words.
            let docked = compiled
                .semantic(&UiKey::new("settings/status-dock"))
                .is_some();
            let bar = DraftBar::of(lab.state());
            let sentence = if docked { bar.short } else { bar.visual };
            let every = every_status_line(
                &mut lab,
                width,
                height,
                "settings/draft-recovery/status/visual",
            );
            // A word wider than the box goes on with a hyphen.
            let bare = |text: &str| text.split_whitespace().collect::<String>().replace('-', "");
            assert_eq!(
                bare(&every.concat()),
                bare(&sentence),
                "{context}: {every:#?}"
            );
            assert!(
                every.iter().all(|line| !line.contains('\u{2026}')),
                "{context}: {every:#?}"
            );
            let compiled = lab.compile(width, height);
            let words = rect_of(&compiled, "settings/draft-recovery/status");
            let discard = rect_of(&compiled, "settings/drafts/discard-all");
            if every.len() > 1 && !docked {
                assert!(
                    discard.y >= words.bottom() - 0.01,
                    "{context}: under the words"
                );
                let shown = compiled
                    .paint
                    .iter()
                    .filter(|node| {
                        node.key
                            .as_str()
                            .starts_with("settings/draft-recovery/status/visual/line-")
                            && node.clip.height >= node.rect.height - 0.5
                    })
                    .count();
                assert!(
                    words.height + 0.5 >= shown as f32 * status_line_height(),
                    "{context}: the box holds its {shown} lines: {words:?}"
                );
            }
            checked += usize::from(every.len() > 1);
            for fault in compiled.fit_failures() {
                assert!(
                    !fault.contains("settings/draft-recovery/status"),
                    "{context}: {fault}"
                );
            }
        }
        assert!(
            checked > 0,
            "{scale}\u{d7}: a narrow bar sets its words on lines"
        );
    });
}

/// A STATUS BAR'S STACKED BUTTONS KEEP THEIR WORDS (the review):
/// where a bar's buttons go one under another — the unsaved-draft dock
/// beside the compact toolbar's back button at 2× text in a 22- or
/// 24-column window — each paints its whole words, on lines where one line
/// cannot hold them, the bar as tall as they are: nothing it offers is cut.
/// Held to one touch row, the dock painted "Rev…" and "Dis…".
#[test]
fn a_status_bars_stacked_buttons_keep_their_words() {
    at_each_text_scale(&[1.0, 2.0], "stacked bar buttons", |scale| {
        let mut stacked = 0;
        for width in [164.0, 178.0, 192.0, 234.0] {
            for height in [348.0, 432.0] {
                let mut lab = Lab::new("");
                lab.state_mut().navigate(SettingsRoute::Terminal);
                let _ = lab.compile(width, height);
                let _ = lab.invoke(
                    &format!("settings/set/{}", prefs::EDIT_SCROLLBACK),
                    Some(SemanticInput::Text("abc".to_string())),
                );
                let compiled = lab.compile(width, height);
                let context = format!("{scale}\u{d7} {width}x{height}");
                let review = rect_of(&compiled, "settings/drafts/review");
                let discard = rect_of(&compiled, "settings/drafts/discard-all");
                stacked += usize::from(discard.y >= review.bottom() - 0.5);
                for fault in compiled.fit_failures() {
                    assert!(!fault.contains("settings/drafts/"), "{context}: {fault}");
                }
                for key in ["settings/drafts/review", "settings/drafts/discard-all"] {
                    let audit = compiled
                        .text_fit_audit_line(&UiKey::new(key))
                        .unwrap_or_else(|| panic!("{context}: {key} is drawn"));
                    assert!(
                        audit.contains(" overflow=false ") && !audit.contains('\u{2026}'),
                        "{context}: {audit}"
                    );
                }
                let actions = rect_of(&compiled, "settings/draft-recovery/actions");
                assert!(
                    actions.bottom() + 0.5 >= discard.bottom(),
                    "{context}: the bar holds its buttons: {actions:?} {discard:?}"
                );
            }
        }
        assert!(
            stacked > 0,
            "{scale}\u{d7}: a narrow bar stacks its buttons"
        );
    });
}

/// A DOCKED STATUS'S ACTIONS UNDER ITS WORDS STAND ACROSS THE TOOLBAR (the
/// review): where the compact toolbar's dock lays a refused
/// draft's Review and Discard under its words (2× text in a 22-column
/// window), they take the toolbar's whole width, under its leading button
/// too, each word whole on one line, and the page keeps the room the
/// toolbar leaves it. Held to the dock's column beside the back button they
/// broke "Re-" / "view" and "Dis-" / "card" and grew the toolbar to 322pt of
/// the window's 348.
#[test]
fn a_docked_statuss_actions_stand_across_the_toolbar() {
    at_each_text_scale(&[2.0], "docked actions", |_| {
        for height in [348.0, 432.0] {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(SettingsRoute::Terminal);
            let _ = lab.compile(178.0, height);
            let _ = lab.invoke(
                &format!("settings/set/{}", prefs::EDIT_SCROLLBACK),
                Some(SemanticInput::Text("abc".to_string())),
            );
            let compiled = lab.compile(178.0, height);
            let context = format!("2\u{d7} 178x{height}");
            assert!(
                compiled
                    .semantic(&UiKey::new("settings/status-dock"))
                    .is_some(),
                "{context}: the draft docks"
            );
            let toolbar = rect_of(&compiled, "settings/compact-toolbar");
            let back = rect_of(&compiled, "settings/compact-navigation");
            for key in ["settings/drafts/review", "settings/drafts/discard-all"] {
                let button = rect_of(&compiled, key);
                assert!(
                    (button.x - back.x).abs() < 0.5 && button.y >= back.bottom() - 0.5,
                    "{context}: {key} stands under the back button: {button:?} {back:?}"
                );
                let audit = compiled
                    .text_fit_audit_line(&UiKey::new(key))
                    .unwrap_or_else(|| panic!("{context}: {key} is drawn"));
                assert!(
                    audit.contains(" overflow=false ") && !audit.contains("\\n"),
                    "{context}: its words on one line: {audit}"
                );
            }
            assert!(
                toolbar.height <= height - 90.0,
                "{context}: the page keeps its room: {toolbar:?}"
            );
        }
    });
}

/// The draft bar names a refused draft's reason and offers buttons as wide
/// as their words; asked to confirm, it names the one draft and offers
/// Cancel; on Modified it does not offer to go to Modified.
#[test]
fn the_draft_bar_says_why_names_the_draft_and_offers_cancel() {
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Terminal);
    let key = prefs::EDIT_SEARCH_HISTORY_LINES;
    let _ = lab.invoke(
        &format!("settings/set/{key}"),
        Some(SemanticInput::Text("lots".to_string())),
    );
    let compiled = lab.compile(1_074.0, 684.0);
    let status = compiled
        .semantic(&UiKey::new("settings/draft-recovery/status"))
        .expect("the draft bar");
    assert!(status.label.contains("was not saved"), "{}", status.label);
    // The red line under the field paints the reason; the bar names the
    // setting and what to do, so one screen never paints it twice.
    let painted = painted_text(&compiled, "settings/draft-recovery/status/visual");
    assert!(
        painted.starts_with("Searchable scrollback lines was not saved.")
            && !painted.contains("whole number"),
        "{painted}"
    );
    let review = rect_of(&compiled, "settings/drafts/review");
    let discard = rect_of(&compiled, "settings/drafts/discard-all");
    assert!(
        review.width < 200.0 && discard.width < 200.0,
        "buttons, not text fields: {review:?} {discard:?}"
    );

    let _ = lab.invoke("settings/drafts/discard-all", None);
    let compiled = lab.compile(1_074.0, 684.0);
    let status = compiled
        .semantic(&UiKey::new("settings/draft-recovery/status"))
        .expect("the question");
    assert!(
        status
            .label
            .contains("Discard your unsaved Searchable scrollback lines draft?"),
        "{}",
        status.label
    );
    assert!(
        compiled
            .semantic(&UiKey::new("settings/drafts/discard-cancel"))
            .is_some()
    );
    let _ = lab.invoke("settings/drafts/discard-cancel", None);
    assert!(!lab.state().draft_discard_confirmation);
    assert!(
        lab.state().field_has_unsaved_draft(key),
        "Cancel keeps the draft"
    );

    lab.state_mut().navigate(SettingsRoute::Modified);
    let compiled = lab.compile(1_074.0, 684.0);
    assert!(
        compiled
            .semantic(&UiKey::new("settings/drafts/review"))
            .is_none(),
        "Modified does not offer to go to Modified"
    );
}

/// A number's status reads as its slider does.
#[test]
fn a_saved_number_reads_as_its_slider() {
    let mut lab = Lab::new("");
    lab.commit(
        &format!("settings/set/{}", prefs::EDIT_BACKGROUND_OPACITY),
        Some(SemanticInput::Number(0.7)),
    );
    let feedback = lab.state().feedback.clone().unwrap_or_default();
    assert!(
        feedback.starts_with("Background opacity set to 70%"),
        "{feedback}"
    );
}

/// A choice set in the words it shows saves its option; a token saves as it
/// is.
#[test]
fn a_choice_set_in_its_words_saves_its_option() {
    let state = SettingsViewState::new(&Config::default());
    let field = state.field_by_key(prefs::EDIT_BIDI).expect("bidi");
    let set = |text: &str| {
        semantic_value_for_field(
            field,
            Some(SemanticInput::Text(text.to_string())),
            &crate::app_config::ThemeCatalog::default(),
        )
    };
    assert_eq!(set("If asked").as_deref(), Some("explicit"));
    assert_eq!(set("Auto (default)").as_deref(), Some("implicit"));
    assert_eq!(set("explicit").as_deref(), Some("explicit"));
}

/// Choose… beside the selected tab color is in the list's keyboard order,
/// between its field and its Reset.
#[test]
fn choose_beside_the_tab_color_is_in_the_keyboard_order() {
    // On the Window page the row is itself the way to Tab Color, then its
    // Reset.
    let mut lab = Lab::new("active_tab_color = \"#336699\"\n");
    lab.state_mut().navigate(SettingsRoute::WindowTabs);
    let _ = lab.compile(1_074.0, 684.0);
    let field = format!("settings/control/{}", prefs::EDIT_ACTIVE_TAB_COLOR);
    let reset = format!("settings/reset/{}", prefs::EDIT_ACTIVE_TAB_COLOR);
    let order = |lab: &Lab| -> Vec<String> {
        let plan = lab.state().scroll_plan().expect("a list");
        plan.controls()
            .map(|(_, key)| key.as_str().to_string())
            .collect()
    };
    let controls = order(&lab);
    let at = controls
        .iter()
        .position(|key| *key == field)
        .expect("the row");
    assert_eq!(controls.get(at + 1), Some(&reset));
    // In Search the field keeps its Choose… after it, ahead of its Reset.
    lab.state_mut().set_search("tab color".to_string());
    let _ = lab.compile(1_074.0, 684.0);
    let controls = order(&lab);
    let at = controls
        .iter()
        .position(|key| *key == field)
        .expect("the field");
    assert_eq!(
        controls.get(at + 1).map(String::as_str),
        Some("settings/color-open/active_tab_color")
    );
    assert_eq!(controls.get(at + 2), Some(&reset));
}

/// The Tab Color page's own hex row offers no Choose…: the wheel it would
/// open is the page it is on.
#[test]
fn the_tab_color_page_does_not_offer_to_open_itself() {
    for (width, height) in [(1_074.0, 684.0), (738.0, 420.0), (584.0, 348.0)] {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::TabColor);
        // The list's last screen, where the hex row is.
        lab.state_mut().scroll_page_to(1_000);
        let compiled = lab.compile(width, height);
        assert!(
            compiled.paint.iter().any(|node| node.key.as_str()
                == format!("settings/control/{}", prefs::EDIT_ACTIVE_TAB_COLOR)),
            "{width}×{height}: the page shows its hex field"
        );
        assert!(
            compiled
                .paint
                .iter()
                .all(|node| !node.key.as_str().starts_with("settings/color-open/")),
            "{width}×{height}: no Choose… that opens the page it is on"
        );
    }
}

/// ONE LAW FOR THE SCROLLBACK: whatever the file can hold and the engine
/// keeps, Settings saves. A hand-written 50,000,000 is in effect, so
/// pressing Return on it (or typing a larger one) saves rather than being
/// refused by a writer-only cap the load never applied; the memory budget,
/// not a line count, bounds what history costs.
#[test]
fn a_hand_authored_scrollback_saves_again() {
    let key = prefs::EDIT_SCROLLBACK;
    let mut lab = Lab::new("scrollback_lines = 50000000\n");
    lab.state_mut().navigate(SettingsRoute::Terminal);
    let set = format!("settings/set/{key}");
    lab.commit(&set, Some(SemanticInput::Text("60000000".to_string())));
    assert_eq!(lab.text(), "scrollback_lines = 60000000\n");
    assert!(!lab.state().field_errors.contains_key(key));
    // Return on the value in effect is not refused.
    let _ = lab.invoke(&set, Some(SemanticInput::Text("60000000".to_string())));
    assert!(
        !lab.state().field_errors.contains_key(key),
        "the value in effect is not refused: {:?}",
        lab.state().field_errors.get(key)
    );
    for whole in ["50000000", "4294967296", &i64::MAX.to_string(), "0"] {
        assert!(prefs::check_edit_value(key, whole).is_ok(), "{whole}");
    }
    for refused in ["-1", "1.5", "9223372036854775808"] {
        assert!(prefs::check_edit_value(key, refused).is_err(), "{refused}");
    }
    // What Settings saves, the engine keeps.
    let config: Config = aterm_toml::from_str("scrollback_lines = 50000000").unwrap();
    assert_eq!(
        config.engine_behaviour_config().scrollback_limit,
        Some(50_000_000)
    );
}

/// The first Top Settings card keeps its title at 80×24.
#[test]
fn the_first_top_card_keeps_its_title_at_80_by_24() {
    let lab = Lab::new("");
    let compiled = lab.compile(584.0, 348.0);
    assert!(
        compiled
            .paint
            .iter()
            .any(|node| !node.peek && node.key.as_str() == "settings/top/theme/heading"),
        "the card says what it is"
    );
}

/// A TOP CARD HAS ONE NAME AT EVERY WIDTH: the colors card's semantics say
/// "Terminal colors & window appearance" however narrow the window, and
/// only what is painted is fitted — measured in the face it is painted in,
/// so it never overflows its rect. NEGATIVE CONTROLS: at 374pt the painted
/// copy is the short title (the fitting runs), and at 584pt at 1.35× the
/// whole title is painted (the Caption face it is painted in holds it).
#[test]
fn the_top_colors_card_has_one_name_at_every_width() {
    const TITLE: &str = "Terminal colors & window appearance";
    super::at_each_text_scale(&[1.0, 1.35, 2.0], "top card name", |scale| {
        let lab = Lab::new("");
        for width in [320.0, 374.0, 406.0, 584.0, 760.0, 1_100.0] {
            let compiled = lab.compile(width, 348.0);
            let context = format!("{width}pt at {scale}x");
            let card = compiled
                .semantic(&UiKey::new("settings/top/theme"))
                .unwrap_or_else(|| panic!("{context}: the colors card"));
            assert_eq!(card.label, TITLE, "{context}");
            let heading = compiled
                .semantic(&UiKey::new("settings/top/theme/heading"))
                .unwrap_or_else(|| panic!("{context}: its heading"));
            assert_eq!(heading.role, SemanticRole::Heading, "{context}");
            assert_eq!(heading.label, TITLE, "{context}");
            let visual_key = UiKey::new("settings/top/theme/heading/visual");
            assert!(compiled.semantic(&visual_key).is_none(), "{context}");
            let visual = compiled
                .paint
                .iter()
                .find(|node| node.key == visual_key)
                .unwrap_or_else(|| panic!("{context}: the painted title"));
            let UiContent::Text(spec) = &visual.content else {
                panic!("{context}: text")
            };
            let (px, face) = crate::native_ui::text_paint_metrics(spec.role, spec.style);
            assert!(
                crate::tray_raster::ui_text_width_for(face, &spec.text, px)
                    <= visual.rect.width + 0.5,
                "{context}: {:?} overflows {:?}",
                spec.text,
                visual.rect
            );
            if (width, scale) == (374.0, 1.0) {
                assert_eq!(spec.text, "Colors & appearance", "{context}");
            }
            if (width, scale) == (584.0, 1.35) {
                assert_eq!(spec.text, TITLE, "{context}");
            }
        }
    });
}

/// The Text card's size is Text & Fonts' `font_px`: setting it from Top
/// Settings writes the key to aterm.toml, and the card's specimen reports
/// the new size.
#[test]
fn the_top_text_card_writes_font_px() {
    let mut lab = Lab::new("");
    let patch = lab.commit(
        &format!("settings/set/{}", prefs::EDIT_FONT_PX),
        Some(SemanticInput::Text("15".to_string())),
    );
    assert!(
        patch
            .edits
            .iter()
            .any(|edit| edit.key == prefs::EDIT_FONT_PX && edit.value.as_deref() == Some("15")),
        "{patch:?}"
    );
    assert!(lab.text().contains("font_px = 15"), "{}", lab.text());
    let compiled = lab.compile(1_200.0, 820.0);
    let specimen = compiled
        .semantic(&UiKey::new("settings/top/text/preview"))
        .expect("the type specimen");
    assert!(
        matches!(&specimen.value, SemanticValue::Text(value) if value.contains("15 pixels")),
        "{:?}",
        specimen.value
    );
}

/// The accent as text reads at 4.5:1 on every backdrop it is inked on, in a
/// light theme too, while the accent itself keeps its own floor.
#[test]
fn accent_text_reads_at_four_and_a_half_to_one() {
    let light = aterm_render::Theme {
        fg: 0x001F_2328,
        bg: 0x00FF_FFFF,
        cursor: 0x0009_69DA,
        selection: 0x00B6_E3FF,
    };
    for theme in [light, aterm_render::Theme::default()] {
        let roles = crate::native_appearance::roles(
            theme,
            crate::native_appearance::AppearancePreferences::default(),
        );
        let wash = crate::native_appearance::mix_srgb(
            roles.elevated,
            roles.accent,
            crate::native_appearance::ACCENT_WASH_PRESSED,
        );
        for backdrop in [roles.surface, roles.elevated, wash] {
            assert!(crate::native_appearance::contrast_ratio(roles.accent_ink, backdrop) >= 4.5);
        }
        for backdrop in [roles.surface, roles.elevated] {
            assert!(crate::native_appearance::contrast_ratio(roles.success, backdrop) >= 4.5);
        }
    }
}

/// THE LIST'S EDGE RULE ([`UiNode::room`]): inside a list's room a control
/// is live over exactly what of it shows — whole, or cut by the room's edge
/// — read out, in the Tab order, its hit its visible part; what the list
/// draws in the strip under its room is a picture that fades, never live;
/// and the list's cuts are on purpose, so the paint audit counts none of them.
/// CONTROL: a node an inner card clips (not the list) is still counted.
#[test]
fn a_list_is_live_where_its_room_shows_a_control_and_its_strip_is_a_picture() {
    let button = |key: &str| {
        UiNode::new(
            key,
            UiContent::Button(Control::new(ButtonSpec::new(key), ActionId::new(key))),
        )
        .layout(Layout::default().height(Length::Fixed(30.0)))
    };
    let tree = UiTree::new(
        UiNode::new(
            "root",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column())
        .children(vec![
            UiNode::new(
                "list",
                UiContent::Group(GroupSpec {
                    label: Some("List".to_string()),
                    role: SemanticRole::List,
                    style: StyleRef::Plain,
                }),
            )
            .layout(
                Layout::column()
                    .height(Length::Fixed(120.0))
                    .gap(4.0)
                    .clipped(),
            )
            .room(80.0, true)
            .children(vec![
                // 0..30: whole in the room.
                button("shown"),
                // 34..64: whole in the room.
                button("whole"),
                // 68..98: the room's edge at 80 cuts it.
                button("cut"),
                // 102..132: in the strip, cut at the list's edge.
                button("strip"),
            ]),
            UiNode::new(
                "card",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::column().height(Length::Fixed(20.0)).clipped())
            .children(vec![button("overflow")]),
        ]),
    )
    .compile(LogicalRect::new(0.0, 0.0, 300.0, 200.0))
    .expect("compiles");
    for key in ["shown", "whole"] {
        let key = UiKey::new(key);
        assert!(tree.semantic(&key).is_some(), "{key:?} is read out");
        assert!(
            tree.focus_order.contains(&key),
            "{key:?} takes the keyboard"
        );
        assert!(tree.hits.iter().any(|hit| hit.key == key));
    }
    let cut = UiKey::new("cut");
    let shown = LogicalRect::new(0.0, 68.0, 300.0, 12.0);
    assert_eq!(
        tree.semantic(&cut).map(|node| node.rect),
        Some(shown),
        "a control the room's edge cuts is live over what shows"
    );
    assert!(tree.focus_order.contains(&cut));
    assert_eq!(
        tree.hits
            .iter()
            .find(|hit| hit.key == cut)
            .map(|hit| hit.rect),
        Some(shown)
    );
    let strip = UiKey::new("strip");
    let painted = tree
        .paint
        .iter()
        .find(|node| node.key == strip)
        .expect("the strip's control is drawn");
    assert_eq!(painted.fade, Some((80.0, 120.0)), "and fades there");
    assert!(tree.semantic(&strip).is_none(), "but it is a picture");
    assert!(!tree.focus_order.contains(&strip));
    assert!(tree.hits.iter().all(|hit| hit.key != strip));
    assert!(
        tree.paint
            .iter()
            .filter(|node| node.key.as_str() != "overflow")
            .all(|node| !node.clipped()),
        "a list's cuts are on purpose"
    );
    assert!(
        tree.paint
            .iter()
            .any(|node| node.key.as_str() == "overflow" && node.clipped()),
        "an inner card's clip still counts"
    );
}

/// A row the list's top edge cuts is drawn as it shows, and what of it
/// shows is live, WHERE IT SHOWS ITS NAME: a button the edge cuts only
/// through its padding, its words whole under the edge, answers over the
/// part that shows, as in any native scroll view, and focusing it scrolls its
/// stop whole. One the edge cuts through its words, and one it leaves a
/// sliver of, paints nothing and answers no press but the list's — a press
/// there scrolls it whole into view — so no nameless half control, and no
/// stray line, is pressed blind (a 4pt sliver of Cursor & Motion's Serious
/// Mode switch under its preview at 80x24 turned it on).
#[test]
fn a_row_the_top_edge_cuts_is_drawn_and_live_where_it_shows() {
    let row = |key: &str, label: f32, control: f32| {
        UiNode::new(key, UiContent::Group(GroupSpec::new(key)))
            .layout(Layout::column().height(Length::Fixed(label + control)))
            .children(vec![
                UiNode::new(
                    format!("{key}/label"),
                    UiContent::Text(TextSpec::heading(key)),
                )
                .layout(Layout::default().height(Length::Fixed(label))),
                UiNode::new(
                    format!("{key}/control"),
                    UiContent::Button(Control::new(
                        ButtonSpec::new(key),
                        ActionId::new(format!("{key}/control")),
                    )),
                )
                .layout(Layout::default().height(Length::Fixed(control))),
            ])
    };
    let list = |scrolled: f32, rows: Vec<UiNode>| {
        UiTree::new(
            UiNode::new(
                "list",
                UiContent::Group(GroupSpec {
                    label: Some("List".to_string()),
                    role: SemanticRole::List,
                    style: StyleRef::Plain,
                }),
            )
            .layout(
                Layout::column()
                    .height(Length::Fixed(100.0))
                    .scrolled(scrolled)
                    .clipped(),
            )
            .room(100.0, false)
            .children(rows),
        )
        .compile(LogicalRect::new(0.0, 0.0, 300.0, 200.0))
        .expect("compiles")
    };
    // -24..36: the top edge cuts its label away and 4pt of its 40pt
    // button's padding, its words whole under it.
    let tree = list(
        24.0,
        vec![row("padded", 20.0, 40.0), row("whole", 20.0, 40.0)],
    );
    let cut = UiKey::new("padded/control");
    assert!(
        tree.paint.iter().any(|node| node.key == cut && node.peek),
        "the cut control is drawn, cut on purpose"
    );
    let shown = LogicalRect::new(0.0, 0.0, 300.0, 36.0);
    assert_eq!(tree.semantic(&cut).map(|node| node.rect), Some(shown));
    assert!(tree.focus_order.contains(&cut));
    assert_eq!(
        tree.hits
            .iter()
            .find(|hit| hit.key == cut)
            .map(|hit| hit.rect),
        Some(shown),
        "it answers over the part that shows"
    );
    let whole = UiKey::new("whole/control");
    assert!(tree.semantic(&whole).is_some() && tree.hits.iter().any(|hit| hit.key == whole));
    assert!(tree.fit_failures().is_empty(), "{:?}", tree.fit_failures());
    // -40..20: the edge cuts through its words; -56..4: a 4pt sliver of it.
    for (scrolled, what) in [(40.0, "its words"), (56.0, "all but a sliver")] {
        let tree = list(
            scrolled,
            vec![row("named", 20.0, 40.0), row("after", 20.0, 40.0)],
        );
        let cut = UiKey::new("named/control");
        assert!(
            tree.paint.iter().all(|node| node.key != cut),
            "the edge cuts {what}: the control paints nothing"
        );
        assert!(tree.semantic(&cut).is_none() && tree.hits.iter().all(|hit| hit.key != cut));
        assert!(!tree.focus_order.contains(&cut));
        let shows = 60.0 - scrolled;
        assert_eq!(
            tree.list_picture_at(150.0, shows / 2.0),
            Some(&cut),
            "a press there scrolls it into view"
        );
    }
}

/// Each paint node of `compiled` and the draw primitives it lays down, in
/// paint order, as [`crate::native_ui::CompiledUi::tray`] paints them: its
/// fade and clip around what the node itself draws.
fn prims_by_node(
    compiled: &crate::native_ui::CompiledUi,
) -> Vec<(&crate::native_ui::PaintNode, Vec<crate::widget::DrawPrim>)> {
    use crate::widget::DrawPrim;
    let prims = compiled.tray(aterm_render::Theme::default(), 14.0).prims;
    let mut at = 0;
    let mut out = Vec::with_capacity(compiled.paint.len());
    for node in &compiled.paint {
        assert!(matches!(prims[at], DrawPrim::ClipPush { .. }));
        at += 1;
        if node.fade.is_some() {
            assert!(matches!(prims[at], DrawPrim::FadePush { .. }));
            at += 1;
        }
        let mut depth = 0usize;
        let mut own = Vec::new();
        loop {
            match &prims[at] {
                DrawPrim::FadePop if depth == 0 && node.fade.is_some() => {
                    at += 1;
                    assert!(matches!(prims[at], DrawPrim::ClipPop));
                    break;
                }
                DrawPrim::ClipPop if depth == 0 => break,
                DrawPrim::ClipPop => depth -= 1,
                DrawPrim::ClipPush { .. } => depth += 1,
                _ => {}
            }
            own.push(prims[at].clone());
            at += 1;
        }
        at += 1;
        out.push((node, own));
    }
    out
}

/// What the tops of the lists on `compiled` paint — the page's list under
/// its pinned heading or preview, the rows a list scrolls under a caption it
/// keeps whole over them (`…/scrolled-rows`), the sidebar's list under its
/// search field: no line of words cut through by the list's top edge — a
/// line reaching an em over its baseline and a quarter of one under it —
/// and, under a kept caption, every node that edge leaves whole in the
/// list's room (over `room_bottom`; the strip under it holds a picture)
/// judged by the fit audit as any other, never flagged a cut on purpose. The
/// number of kept captions it read.
fn assert_list_tops(
    compiled: &crate::native_ui::CompiledUi,
    room_bottom: f32,
    context: &str,
) -> usize {
    let painted = prims_by_node(compiled);
    let mut kept = 0;
    let (mut sliced, mut excused) = (Vec::new(), Vec::new());
    for rows in compiled
        .paint
        .iter()
        .filter(|node| is_list_node(node) || node.key.as_str().ends_with("/scrolled-rows"))
    {
        let caption = rows.key.as_str().ends_with("/scrolled-rows");
        kept += usize::from(caption);
        let edge = rows.clip.y;
        let within = |clip: LogicalRect| {
            !clip.is_empty()
                && clip.x >= rows.clip.x - 0.01
                && clip.right() <= rows.clip.right() + 0.01
                && clip.y >= rows.clip.y - 0.01
                && clip.bottom() <= rows.clip.bottom() + 0.01
        };
        for (node, prims) in painted.iter().filter(|(node, _)| within(node.clip)) {
            // A line the edge cuts shows below it, inside the node's clip,
            // which the edge sets (an edge an inner list's clip hides it
            // under shows nothing of it).
            let at_edge = (node.clip.y - edge).abs() <= 0.5;
            for prim in prims {
                if let crate::widget::DrawPrim::Text {
                    baseline, px, s, ..
                } = prim
                    && at_edge
                    && baseline + 0.25 * px > node.clip.y + 0.01
                    && baseline - 0.95 * px < node.clip.y - 0.01
                {
                    sliced.push(format!("{:?} paints {s:?}", node.key.as_str()));
                }
            }
            // Whole under the caption and in the list's room: a node drawn
            // in the strip under the room is a picture, cut on purpose.
            let whole = node.rect.y >= edge - 0.01
                && node.rect.bottom() <= rows.clip.bottom().min(room_bottom) + 0.01;
            if caption && whole && node.peek {
                excused.push(format!("{:?} {:?}", node.key.as_str(), node.rect));
            }
        }
    }
    assert!(
        sliced.is_empty() && excused.is_empty(),
        "{context}: words cut through by a list's top edge: {sliced:#?}; whole under a kept \
         caption and excused from the audit: {excused:#?}"
    );
    kept
}

/// ROWS UNDER A KEPT CAPTION ARE PAINTED WHOLE OR NOT AT ALL, AND AUDITED
/// (#45, #63): on the last screen of every list page at 80×24, 102×29 and
/// 150×48, where a caption is kept whole over the rows that scroll under it,
/// no row cut there paints a line of words sliced just under the caption
/// (Sound's last screen at 102×29 painted its note's line so under SOUND
/// EFFECTS), and the fit audit excuses none of the rows under it, whole
/// ones too (every row under the caption was flagged a cut on purpose).
#[test]
fn rows_under_a_kept_caption_paint_no_cut_line_and_are_audited() {
    let mut kept = 0;
    for route in LIST_ROUTES
        .into_iter()
        .chain([SettingsRoute::TextFonts, SettingsRoute::Ai])
    {
        for (width, height) in [(584.0, 348.0), (738.0, 418.0), (1_074.0, 684.0)] {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            let compiled = to_the_last_screen(&mut lab, width, height);
            let Some(plan) = lab.state().scroll_plan() else {
                continue;
            };
            let Some((list, room)) = compiled_list(&compiled, &plan) else {
                continue;
            };
            let context = format!("{route:?} at {width}×{height}, its last screen");
            kept += assert_list_tops(&compiled, list.y + room, &context);
            let failures = compiled.fit_failures();
            assert!(failures.is_empty(), "{context}: {failures:#?}");
        }
    }
    assert!(kept > 0, "some last screen keeps a caption over its rows");
}

/// NO LIST'S TOP EDGE SLICES A LINE OF WORDS IN A SHORT WINDOW: at 220×12
/// and 220×16 with 2× text, on every page, first screen and last, the page's
/// list under its pinned heading and the sidebar's under its search field —
/// that one scrolled part of a row past its top, as the wheel leaves it —
/// paint no line of words cut through by the edge. The sidebar painted the
/// foot of a row's name under the search field.
#[test]
fn no_list_top_slices_a_line_in_a_short_window() {
    at_each_text_scale(&[2.0], "short list tops", |_| {
        for route in SettingsRoute::ALL {
            for (width, height) in [(1_564.0, 180.0), (1_564.0, 240.0)] {
                let mut lab = Lab::new("");
                lab.state_mut().navigate(route);
                let _ = lab.compile(width, height);
                let limit = lab.state().scroll_plan().map_or(0, |plan| plan.limit());
                for top in [0, limit] {
                    lab.state_mut().scroll_page_to(top);
                    lab.state_mut().page_reveal = None;
                    let _ = lab.compile(width, height);
                    // The rail's place, a part of a row past its top.
                    let rail = lab.state().navigation_scroll.get();
                    let rail_limit = lab.state().navigation_scroll_limit.get();
                    lab.state().navigation_scroll.set(if rail >= 20.0 {
                        rail - 20.0
                    } else {
                        (rail + 20.0).min(rail_limit)
                    });
                    let compiled = lab.compile(width, height);
                    let room_bottom = lab
                        .state()
                        .scroll_plan()
                        .and_then(|plan| compiled_list(&compiled, &plan))
                        .map_or(height, |(list, room)| list.y + room);
                    assert_list_tops(
                        &compiled,
                        room_bottom,
                        &format!("{route:?} {width}×{height} at 2× screen {top}"),
                    );
                }
            }
        }
    });
}

/// The list pages the edge sweeps cover, the Themes gallery's 190pt cards
/// and Software Update's status card among them.
const LIST_ROUTES: [SettingsRoute; 12] = [
    SettingsRoute::Home,
    SettingsRoute::Appearance,
    SettingsRoute::KeyboardInput,
    SettingsRoute::Terminal,
    SettingsRoute::Security,
    SettingsRoute::Sound,
    SettingsRoute::CursorStyles,
    SettingsRoute::CursorMotion,
    SettingsRoute::Harness,
    SettingsRoute::WindowTabs,
    SettingsRoute::Themes,
    SettingsRoute::SoftwareUpdate,
];

/// The shipped and common window sizes the edge sweeps cover: 80×24, 100×34
/// (where Software Update's compact forms trade places), the shipped 102×29,
/// 150×48, 167×36 and 220×60.
const LIST_SIZES: [(f32, f32); 6] = [
    (584.0, 348.0),
    (724.0, 488.0),
    (738.0, 418.0),
    (1_074.0, 684.0),
    (1_193.0, 516.0),
    (1_564.0, 854.0),
];

/// The list a compiled Settings page draws, its room and whether a peek
/// follows the screen: the list's rect, how far from its top it is live.
fn compiled_list(
    compiled: &crate::native_ui::CompiledUi,
    plan: &scroll::ScrollPlan,
) -> Option<(LogicalRect, f32)> {
    let list = compiled
        .paint
        .iter()
        .find(|node| is_list_node(node) && node.key.as_str() != "settings/navigation/list")?
        .rect;
    Some((list, plan.room().min(list.height)))
}

/// Whether `node` is a scrolling list: the page's (whatever its page calls
/// it — `settings/list`, Tab Color's, About's, Packages'…) or the sidebar's.
fn is_list_node(node: &crate::native_ui::PaintNode) -> bool {
    matches!(&node.content, UiContent::Group(spec) if spec.role == SemanticRole::List)
}

fn is_control(content: &UiContent) -> bool {
    matches!(
        content,
        UiContent::Button(_)
            | UiContent::Switch(_)
            | UiContent::Slider(_)
            | UiContent::TextField(_)
    )
}

/// EVERY SCREEN FILLS ITS LIST AND EVERY CONTROL IT DRAWS IN ITS ROOM IS
/// LIVE: on every screen of the list pages, at the shipped and common
/// window sizes, a control any part of which the list's room shows is live
/// over exactly that part (its semantic and hit rects are its clip inside the
/// room), cut by the top edge or whole; one the screen draws as its peek —
/// from the first control after the stops it shows that the room does not
/// hold whole, and any drawn only in the strip under the room — is a fading
/// picture over all it shows, never live, and a control the list lists,
/// which a press there scrolls in; a screen with stops after
/// it reaches the page's bottom (its room and the strip), and the last screen
/// ends on its last stop at the room's edge — no screen ends on a band of
/// blank, and the list never stops short to hide a cut. And THE AUDIT JUDGES
/// EVERY ROW IT PAINTS: at 1× and 2× text, no screen paints a word cut off,
/// overflowing or clipped by accident — the rows scrolled under a caption a
/// screen keeps whole included — and no list's top edge slices a line of
/// words ([`assert_list_tops`]): the last screen's first stop, peeking in
/// over the list's top, painted Cursor & Motion's "Typing momentum glow"
/// half under its pinned preview at 150×48.
#[test]
fn every_screen_fills_its_list_and_every_control_its_room_shows_is_live() {
    at_each_text_scale(&[1.0, 2.0], "the list edge sweep", |scale| {
        list_edge_sweep(scale);
    });
}

/// One text scale of [`every_screen_fills_its_list_and_every_control_its_room_shows_is_live`].
fn list_edge_sweep(scale: f32) {
    for route in LIST_ROUTES {
        for (width, height) in LIST_SIZES {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            lab.each_screen(width, height, |state, compiled, top| {
                let Some((plan, shown)) = state.scroll_shown() else {
                    return;
                };
                let Some((list, room)) = compiled_list(compiled, &plan) else {
                    return;
                };
                let context = format!("{route:?} {width}×{height} at {scale}× screen {top}");
                assert_list_tops(compiled, list.y + room, &context);
                let failures = compiled.fit_failures();
                assert!(failures.is_empty(), "{context}: {failures:#?}");
                // A press on the picture of a control in the strip scrolls
                // that control in: every one is a control the list lists.
                // (A sidebar row its edge draws without its name is the
                // sidebar's: the press brings it into view there.)
                for picture in &compiled.list_pictures {
                    assert!(
                        plan.stop_holding(&picture.key).is_some()
                            || picture.key.as_str().starts_with("settings/nav/"),
                        "{context}: a press on {:?} in the strip finds no stop",
                        picture.key.as_str()
                    );
                }
                let room_rect = LogicalRect::new(list.x, list.y, list.width, room);
                for node in &compiled.paint {
                    if !is_control(&node.content)
                        || node.clip.is_empty()
                        || node.rect.x < list.x - 0.5
                        || node.rect.right() > list.right() + 0.5
                        || node.clip.y < list.y - 0.5
                        || node.clip.bottom() > list.bottom() + 0.5
                    {
                        continue;
                    }
                    let context = format!(
                        "{context}: {} {:?} clip {:?}",
                        node.key.as_str(),
                        node.rect,
                        node.clip
                    );
                    // ONE BOUNDARY: a control the screen draws past the
                    // stops it shows, from the first the room does not hold
                    // whole, is its peek — fading from its top, a picture
                    // over all it shows, never live in part.
                    if node.fade.is_some_and(|(top, _)| top <= node.rect.y + 0.5) {
                        assert!(
                            compiled.semantic(&node.key).is_none(),
                            "{context}: fading, live"
                        );
                        let enabled = match &node.content {
                            UiContent::Button(control) => control.state.enabled,
                            UiContent::Switch(control) => control.state.enabled,
                            UiContent::Slider(control) => control.state.enabled,
                            UiContent::TextField(control) => control.state.enabled,
                            _ => true,
                        };
                        assert!(
                            node.clip.height < 0.5
                                || !enabled
                                || compiled.list_pictures.iter().any(|picture| {
                                    picture.key == node.key
                                        && (picture.rect.y - node.clip.y).abs() <= 0.5
                                        && (picture.rect.bottom() - node.clip.bottom()).abs() <= 0.5
                                }),
                            "{context}: fading, but a press on it is lost"
                        );
                        continue;
                    }
                    match node.clip.intersect(room_rect) {
                        Some(live) if live.height > 0.5 => {
                            let semantic = compiled
                                .semantic(&node.key)
                                .unwrap_or_else(|| panic!("{context}: shown in the room, dead"));
                            assert!(
                                (semantic.rect.y - live.y).abs() <= 0.5
                                    && (semantic.rect.bottom() - live.bottom()).abs() <= 0.5,
                                "{context}: live over {:?}, not what shows {live:?}",
                                semantic.rect
                            );
                        }
                        _ => {
                            assert!(
                                compiled.semantic(&node.key).is_none(),
                                "{context}: in the strip, live"
                            );
                            assert!(
                                shown.end >= plan.len() || node.fade.is_some(),
                                "{context}: in the strip, not fading"
                            );
                        }
                    }
                }
                // A screen with stops after it draws on to the page's bottom.
                // Only on a hard edge — no strip fading under the room — and
                // where no card after it would show, does it end on a cut of
                // the last card it draws instead, never on one that looks
                // complete; where the strip fades, the fade says the list goes
                // on, and a cut short of the page's bottom would leave a band
                // of blank under it (Cursor Styles at 150×48 cut its fourth
                // style 34pt short, Appearance at 102×29 a note 42pt short).
                let ends_on_cut = compiled.paint.iter().any(|node| {
                    node.key.as_str() != "settings/list"
                        && node.rect.x >= list.x - 0.5
                        && node.rect.right() <= list.right() + 0.5
                        && node.rect.y < list.bottom() - 0.5
                        && node.rect.bottom() > list.bottom() + 0.5
                });
                if shown.end < plan.len() && shown.start < plan.limit() {
                    assert!(
                        list.height >= plan.room() + plan.overhang() - 0.5
                            || (ends_on_cut && plan.overhang() <= 0.01),
                        "{context}: the list {list:?} stops short of its room {} and strip {}",
                        plan.room(),
                        plan.overhang()
                    );
                }
                let drawn_to = compiled
                    .paint
                    .iter()
                    .filter(|node| {
                        node.key != UiKey::new("settings/list")
                            && node.rect.x >= list.x - 0.5
                            && node.rect.right() <= list.right() + 0.5
                            && !node.clip.is_empty()
                            && node.clip.y >= list.y - 0.5
                            && node.clip.bottom() <= list.bottom() + 0.5
                    })
                    .map(|node| node.clip.bottom())
                    .fold(list.y, f32::max);
                assert!(
                    drawn_to >= (list.y + room).min(list.bottom()) - 0.5,
                    "{context}: the screen ends on a band of blank: drawn to {drawn_to}, room to {}",
                    list.y + room
                );
            });
        }
    }
}

/// CHOOSING A CURSOR STYLE NEVER RESIZES THE PREVIEW OR MOVES ITS ROW: the
/// preview keeps room for the tallest stop any style's Adjust card can take,
/// so whichever style is saved (Off has no card), in a dark theme or a light
/// one (whose Bloom rows say why they are idle), with Bloom on or off, the
/// 80×24 and shipped 102×29 windows give one preview height; the page lands
/// on Off and the saved style whole; and a style chosen from the keyboard
/// keeps its place, and the preview its height, when the status bar comes.
#[test]
fn choosing_a_cursor_style_never_resizes_the_preview_or_moves_its_row() {
    let rect = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        compiled.semantic(&UiKey::new(key)).map(|node| node.rect)
    };
    for (width, height) in [(584.0, 348.0), (738.0, 418.0)] {
        for font_px in [12.0, 16.0] {
            let mut cx = view_cx_at(width, height);
            cx.terminal_font_px = font_px;
            let context = format!("{width}×{height} at {font_px}px");
            let mut previews = std::collections::BTreeSet::new();
            for theme in ["Dracula", "GitHub Light"] {
                for bloom in [true, false] {
                    for saved in [
                        "off",
                        prefs::DEFAULT_CURSOR_TRAIL_STYLE,
                        "comet",
                        "rainbow kitty flying",
                    ] {
                        // The saved style drawn, on every host (`trail_drawn`).
                        let mut lab = Lab::new(&trail_drawn(&format!(
                            "theme = \"{theme}\"\ncursor_trail_bloom = {bloom}\ncursor_trail_style = \"{saved}\"\n"
                        )));
                        lab.state_mut().navigate(SettingsRoute::CursorStyles);
                        let compiled = lab.compile_in(&cx);
                        let context = format!("{context} {theme} bloom={bloom} {saved}");
                        let preview = rect(&compiled, "settings/preview/cursor-styles")
                            .unwrap_or_else(|| panic!("{context}: the preview"));
                        previews.insert((preview.height * 100.0).round() as i64);
                        let list = rect(&compiled, "settings/list").expect("the list");
                        // The saved style lands whole in the room the list
                        // keeps with a bar up; Off too where the list's first
                        // screen shows them both.
                        let (plan, _) = lab.state().scroll_shown().expect("a list");
                        let saved_row = cursor_style_row_key(saved);
                        let both = plan
                            .stop_of(&UiKey::new(saved_row.as_str()))
                            .is_some_and(|stop| plan.view(0).contains(&stop));
                        let landed = if both && saved != "off" {
                            vec![saved, "off"]
                        } else {
                            vec![saved]
                        };
                        for value in landed {
                            let row = rect(&compiled, &cursor_style_row_key(value))
                                .unwrap_or_else(|| panic!("{context}: {value} is not on landing"));
                            assert!(
                                row.y >= list.y - 0.01
                                    && row.bottom() <= list.y + plan.room() + 0.01,
                                "{context}: {value} {row:?} whole in the room of {list:?}"
                            );
                        }
                    }
                }
            }
            assert_eq!(
                previews.len(),
                1,
                "{context}: one preview height whichever style, theme and Bloom: {previews:?}"
            );

            // A style reached from the keyboard and chosen keeps its place.
            for theme in ["Dracula", "GitHub Light"] {
                let mut lab = Lab::new(&trail_drawn(&format!("theme = \"{theme}\"\n")));
                lab.state_mut().navigate(SettingsRoute::CursorStyles);
                let _ = lab.compile_in(&cx);
                let comet = cursor_style_row_key("comet");
                let mut steps = 0;
                while lab.state().common.last_focus.as_ref().map(UiKey::as_str)
                    != Some(comet.as_str())
                {
                    lab.dispatch(AppEvent::StepFocus(1));
                    let _ = lab.compile_in(&cx);
                    steps += 1;
                    assert!(steps < 20, "{context} {theme}: Down reaches Comet");
                }
                let before = lab.compile_in(&cx);
                let row = rect(&before, &comet).expect("Comet on screen");
                let preview = rect(&before, "settings/preview/cursor-styles");
                lab.commit(&comet, None);
                let after = lab.compile_in(&cx);
                assert!(
                    after.semantic(&UiKey::new("settings/undo")).is_some(),
                    "{context} {theme}: the bar is up"
                );
                assert_eq!(
                    rect(&after, &comet),
                    Some(row),
                    "{context} {theme}: Comet stays put"
                );
                assert_eq!(
                    rect(&after, "settings/preview/cursor-styles"),
                    preview,
                    "{context} {theme}"
                );
            }
        }
    }
}

/// A BAR TALLER THAN THE ROOM KEPT FOR ONE TAKES THE REST FROM THE CURSOR
/// STYLES LIST: a reload warning is taller than the room the page keeps
/// under its list for a status bar, and the list gives up the difference, so
/// it ends above the bar and no part of a style row under it is live (the
/// list used to keep its full room and run 45pt under the warning at 102×29,
/// a row there cut by the bar but still live).
#[test]
fn a_tall_bar_takes_its_room_from_the_cursor_styles_list() {
    for (width, height) in [(738.0, 418.0), (1_193.0, 516.0)] {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::CursorStyles);
        lab.state_mut().config_watch_status = Some(
            "aterm.toml reload rejected: repair the invalid authored value before automatic \
             reload can resume."
                .to_string(),
        );
        let compiled = lab.compile(width, height);
        let warning = rect_of(&compiled, "settings/config-watch-status");
        let drawn = |key: &str| {
            compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == key)
                .map(|node| node.rect)
                .unwrap_or_else(|| panic!("{key} is drawn"))
        };
        let list = drawn("settings/list");
        assert!(
            list.bottom() <= warning.y + 0.01,
            "{width}×{height}: the list {list:?} ends above the warning {warning:?}"
        );
        for node in &compiled.semantics {
            let key = node.key.as_str();
            if key.starts_with("settings/cursor-style/") {
                assert!(
                    node.rect.bottom() <= warning.y + 0.01,
                    "{width}×{height}: {key} {:?} is live under the warning {warning:?}",
                    node.rect
                );
            }
        }
    }
}

/// ONE SEPARATOR, WHEREVER A ROW'S FACTS GO: the facts a row paints beside
/// its name where they fit read exactly as the line they move to under it
/// where they do not, both joined by [`FACT_SEPARATOR`] (the label line once
/// used a wider joiner than its second line, so the same tags changed spacing
/// with the window's width).
#[test]
fn a_rows_facts_read_the_same_beside_its_name_and_under_it() {
    let mut lab = Lab::new("allow_window_ops = true\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    let key = prefs::EDIT_ALLOW_WINDOW_OPS;
    let name = lab.state().field_by_key(key).unwrap().label;
    let (mut beside, mut under) = (None, None);
    for width in (320..=1_400).step_by(10) {
        let compiled = lab.compile(width as f32, 640.0);
        let label = painted_text(&compiled, &format!("settings/label/{key}/visual"));
        let detail = painted_text(&compiled, &format!("settings/detail/{key}/line-0"));
        if let Some(facts) = label
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(FACT_SEPARATOR))
        {
            beside.get_or_insert_with(|| facts.to_string());
        } else if label == name && !detail.is_empty() {
            under.get_or_insert(detail);
        }
    }
    let beside = beside.expect("a wide window paints the facts beside the name");
    let under = under.expect("a narrow window moves them under it");
    assert!(beside.contains("Default: Off"), "{beside}");
    assert!(
        under.starts_with(&beside),
        "the facts read alike beside the name ({beside:?}) and under it ({under:?})"
    );
}

/// A SETTING'S NAME WRAPS, NEVER CUT: on a narrow window, and at large text,
/// every row's painted name is whole — its lines joined start with the
/// setting's name and carry no ellipsis, and the renderer's fit audit finds
/// none of them overflowing — so two settings never read alike for the words
/// a narrow column would cut (Security's "Allow programs to read the c…").
#[test]
fn a_setting_name_wraps_and_is_never_cut_off() {
    at_each_text_scale(&[1.0, 1.5], "name wrap", |scale| {
        for route in [
            SettingsRoute::Security,
            SettingsRoute::Sound,
            SettingsRoute::CursorMotion,
            SettingsRoute::Harness,
            SettingsRoute::SoftwareUpdate,
            SettingsRoute::Packages,
            SettingsRoute::Terminal,
        ] {
            for (width, height) in [(444.0, 350.0), (318.0, 294.0), (584.0, 348.0)] {
                let (mut runtime, instance, view) = setup();
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: route_action(route),
                            value: None,
                        }),
                    )
                    .unwrap();
                let labels: Vec<(String, &'static str)> = {
                    let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
                        unreachable!();
                    };
                    state
                        .legacy
                        .fields
                        .iter()
                        .map(|field| (field.key.to_string(), field.label))
                        .collect()
                };
                let mut painted_at_318 = BTreeMap::new();
                each_screen(
                    &mut runtime,
                    instance,
                    view,
                    &view_cx_at(width, height),
                    |compiled, top| {
                        let context = format!(
                            "{} {width}×{height} at {scale}× screen {top}",
                            route.label()
                        );
                        for (key, label) in &labels {
                            let Some(painted) = painted_row_label(compiled, key) else {
                                continue;
                            };
                            // A peek or a list's edge cuts a label on purpose;
                            // only labels whole on screen.
                            if !row_label_shown_whole(compiled, key) {
                                continue;
                            }
                            assert!(
                                !painted.contains('\u{2026}'),
                                "{context}: {key} is cut: {painted:?}"
                            );
                            let words =
                                |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
                            assert!(
                                words(&painted).starts_with(&words(label)),
                                "{context}: {key} paints its whole name {label:?}: {painted:?}"
                            );
                            if width == 318.0 {
                                painted_at_318.insert(key.clone(), painted);
                            }
                        }
                        let cut: Vec<String> = compiled
                            .fit_failures()
                            .into_iter()
                            .filter(|line| line.contains("key=\"settings/label/"))
                            .collect();
                        assert!(cut.is_empty(), "{context}:\n{}", cut.join("\n"));
                    },
                );
                if route == SettingsRoute::Security && width == 318.0 && scale == 1.0 {
                    let read = painted_at_318
                        .get(prefs::EDIT_ALLOW_OSC52_QUERY)
                        .expect("the clipboard switch is painted");
                    let window = painted_at_318
                        .get(prefs::EDIT_ALLOW_WINDOW_OPS)
                        .expect("the window switch is painted");
                    assert_ne!(read, window, "two safety switches never read alike");
                }
            }
        }
    });
}

/// THE THEME CARD'S ROWS KEEP THE ROW RULE, at every width and text scale:
/// each row's name is painted whole, wrapped between its words, never cut —
/// a row stacks its control under the name where both cannot share a line,
/// and seats its Reset (or the place it keeps) as every stacked row does —
/// and no control, nor the words drawn in it, runs out of its box. On
/// Appearance, Top Settings and Modified, over one theme, a pair, and a pair
/// of aterm's own (the Follow system row's Reset). (The Follow system row
/// kept its name beside the switch and Reset's place whatever the column: at
/// 22 columns its name got 40pt, broken inside "system", and 8pt at 2×; and
/// Top's Window appearance kept its three segments on one line, "A…", "Li…",
/// "D…".)
#[test]
fn the_theme_cards_rows_keep_the_row_rule_at_every_size() {
    const ROWS: [(&str, &str); 6] = [
        ("theme-mode", "Follow system"),
        ("theme-slot/dark", "Dark theme"),
        ("theme-slot/light", "Light theme"),
        ("theme-preview-side", "Preview"),
        ("theme-browse", "More themes"),
        // The card's other row: Top's short name at large text is "Window".
        ("window_theme", "Window"),
    ];
    const OWN: [&str; 11] = [
        "key=\"settings/control/window_theme\"",
        "key=\"settings/segment/window_theme/",
        "key=\"settings/label/theme-",
        "key=\"settings/label-line/theme-",
        "key=\"settings/theme-mode\"",
        "key=\"settings/theme-slot/",
        "key=\"settings/theme-slot-reset/",
        "key=\"settings/theme-preview-side\"",
        "key=\"settings/reset/theme\"",
        "key=\"settings/reset-slot/theme-",
        "key=\"settings/themes/browse-row\"",
    ];
    at_each_text_scale(&[1.0, 1.25, 1.5, 2.0], "theme rows", |scale| {
        for source in [
            "theme = \"GitHub Light\"\n",
            "theme = \"dark:Nord,light:GitHub Light\"\n",
            "theme = \"dark:Default,light:Default Light\"\n",
        ] {
            for route in [
                SettingsRoute::Appearance,
                SettingsRoute::Home,
                SettingsRoute::Modified,
            ] {
                // 22, 26 and 34 columns of the 12px cell, then wider.
                for (width, height) in [
                    (178.0, 420.0),
                    (206.0, 420.0),
                    (262.0, 420.0),
                    (318.0, 294.0),
                    (444.0, 350.0),
                    (584.0, 348.0),
                    (760.0, 446.0),
                ] {
                    let service = VersionedConfigService::new(source.to_string()).expect("fixture");
                    let (mut runtime, instance, view) = setup();
                    {
                        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view)
                        else {
                            unreachable!();
                        };
                        **state =
                            SettingsViewState::from_snapshot(&service.snapshot()).expect("state");
                        state.navigate(route);
                    }
                    each_screen(
                        &mut runtime,
                        instance,
                        view,
                        &view_cx_at(width, height),
                        |compiled, top| {
                            let context = format!(
                                "{} {width}×{height} at {scale}× screen {top} over {source:?}",
                                route.label()
                            );
                            for (key, name) in ROWS {
                                let Some(painted) = painted_row_label(compiled, key) else {
                                    continue;
                                };
                                if !row_label_shown_whole(compiled, key) {
                                    continue;
                                }
                                let words = |text: &str| {
                                    text.split_whitespace().collect::<Vec<_>>().join(" ")
                                };
                                assert!(
                                    !painted.contains('\u{2026}')
                                        && words(&painted).starts_with(name),
                                    "{context}: {key} paints its whole name {name:?}: {painted:?}"
                                );
                            }
                            let cut: Vec<String> = compiled
                                .fit_failures()
                                .into_iter()
                                .filter(|line| OWN.iter().any(|key| line.contains(key)))
                                .collect();
                            assert!(cut.is_empty(), "{context}:\n{}", cut.join("\n"));
                        },
                    );
                }
            }
        }
    });
}

/// A ROW'S LAYOUT NEVER DEPENDS ON ITS STATE: writing a row (Changed comes)
/// or typing into it (a draft) neither makes it taller nor moves its control
/// or anything under it, at every width a row is laid out in — the tags are
/// placed for the row's fullest state ([`place_row_tags`]), and a draft says
/// so in its field. (Typing one digit into Scrollback limit used to add an
/// "Unsaved draft" line and push the next row down; turning a permission on
/// put "Changed" on a line of its own.)
#[test]
fn a_rows_layout_never_depends_on_its_state() {
    let clean = SettingsViewState::new(&Config::default());
    let mut reserved = 0usize;
    for index in 0..clean.legacy.fields.len() {
        let key = clean.legacy.fields[index].key;
        if !native_advanced_key(key) {
            continue;
        }
        // Written with its default's own value, so writing changes nothing
        // but that it is written. A row with no default value to write (an
        // automatic color) is written with a plain value of its kind; the
        // theme and the fonts, whose every name is a lookup this
        // catalog-less view may fail (a real fact the row states), are
        // left to the tests of their own cards.
        let kind = clean.legacy.fields[index].kind;
        let raw = match crate::prefs::unset_literal(&clean.legacy.fields[index]) {
            _ if key == prefs::EDIT_THEME || font_picker_key(key) => continue,
            Some(raw) if prefs::check_edit_value(key, &raw).is_ok() => raw,
            _ if matches!(kind, EditKind::Color) => "#808080".to_string(),
            _ => continue,
        };
        // As a hand-written aterm.toml holds it: a Settings write of a
        // default removes the key, so only the file can write one.
        let item = crate::prefs::typed_toml_item(key, &raw)
            .unwrap_or_else(|error| panic!("{key} = {raw}: {error:?}"));
        let source = crate::prefs::with_toml_item("", key, item)
            .unwrap_or_else(|error| panic!("{key} = {raw} is written: {error:?}"));
        let service = VersionedConfigService::new(source).expect("the written config");
        let mut written = SettingsViewState::from_snapshot(&service.snapshot()).expect("projects");
        assert!(written.is_explicit(key), "{key} is written");
        for width in [SettingsWidth::Compact, SettingsWidth::Medium] {
            for text_width in (300..=900).step_by(40).map(|width| width as f32) {
                let measure = |state: &SettingsViewState| {
                    let field = &state.legacy.fields[index];
                    let finished = finished_setting_row(
                        state,
                        index,
                        field,
                        false,
                        width,
                        text_width,
                        prefs::footnote_of(key).map(prefs::RowNote::Own),
                        None,
                        None,
                    );
                    let whole = finished.whole();
                    let height = fixed_height(&whole);
                    let compiled = UiTree::new(whole)
                        .compile(LogicalRect::new(0.0, 0.0, text_width, height.max(1.0)))
                        .unwrap();
                    let control = compiled
                        .semantics
                        .iter()
                        .find(|node| node.key.as_str() == format!("settings/control/{key}"))
                        .map(|node| node.rect);
                    let fit = row_tag_fit(&finished.name, &finished.worst, finished.label_width);
                    (height, control, fit.state)
                };
                let before = measure(&clean);
                let after = measure(&written);
                // A draft its row would take (an invalid one says why under
                // the row, as a refused value does).
                let draft = match kind {
                    EditKind::Color => "#123456",
                    EditKind::Integer => "7",
                    EditKind::Float => "1.5",
                    _ => "a draft",
                };
                // (The selected tab color off its own page is a way to Tab
                // Color while it holds no draft: a draft comes from a field
                // that shows it — search, Modified, Tab Color — and the row
                // keeps that field while the draft lasts.)
                let links = color_picker::tab_color_row_links(
                    &written,
                    &written.legacy.fields[index],
                    false,
                );
                let drafted = (field_accepts_text_input(&written.legacy.fields[index]) && !links)
                    .then(|| {
                        written.field_inputs.insert(
                            key.to_string(),
                            crate::native_text_input::TextInputState::new(draft.to_string()),
                        );
                        let drafted = measure(&written);
                        written.field_inputs.remove(key);
                        drafted
                    });
                let context = format!("{key} {width:?} at {text_width}");
                assert_eq!(before.0, after.0, "{context}: writing it keeps its height");
                assert_eq!(before.1, after.1, "{context}: writing it keeps its control");
                if let Some(drafted) = drafted {
                    assert_eq!(before.0, drafted.0, "{context}: a draft keeps its height");
                    assert_eq!(before.1, drafted.1, "{context}: a draft keeps its control");
                }
                if !before.2 {
                    reserved += 1;
                }
            }
        }
    }
    // The sweep reaches rows whose Changed does not fit beside the name, so
    // the room kept for it under the name is exercised.
    assert!(reserved > 0, "a row keeps room for Changed under its name");
}

/// A BOX LAYS OUT ITS ROWS' FACTS ALIKE: Modified's two Size rows, one name
/// a little longer than the other, put their facts beside both names or
/// under both, at every width — never one beside and one under (the facts
/// were fitted per row, so "Initial columns" moved them below while "Initial
/// lines" kept them inline).
#[test]
fn sibling_rows_place_their_facts_alike() {
    let mut lab = Lab::new("columns = 100\nlines = 30\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    let fits = |text_width: f32, key: &str| {
        let state = lab.state();
        let index = state.field_index_of(key).unwrap();
        finished_setting_row(
            state,
            index,
            &state.legacy.fields[index],
            true,
            SettingsWidth::Medium,
            text_width,
            None,
            None,
            None,
        )
        .facts_fit_inline()
    };
    // The rows disagree on their own somewhere in the sweep: the box rule is
    // what keeps them alike there.
    assert!(
        (200..=900)
            .step_by(2)
            .map(|width| width as f32)
            .any(|width| fits(width, prefs::EDIT_COLUMNS) != fits(width, prefs::EDIT_LINES)),
        "the sweep reaches a width where each row alone would decide otherwise"
    );
    for width in (560..=1_600).step_by(16) {
        let compiled = lab.compile(width as f32, 900.0);
        let inline =
            |key: &str| painted_row_label(&compiled, key).map(|label| label.contains("Default:"));
        let (columns, lines) = (inline(prefs::EDIT_COLUMNS), inline(prefs::EDIT_LINES));
        if let (Some(columns), Some(lines)) = (columns, lines) {
            assert_eq!(
                columns, lines,
                "{width}: the Size rows place their facts alike"
            );
        }
    }
}

/// A REFUSED VALUE SAYS WHY UNDER ITS ROW ON EVERY PAGE: the reason is the
/// row's own, so the Tab Color page and the Cursor Styles Adjust card paint
/// it under their fields as the registry pages do (they painted only a red
/// rim, the reason reaching the accessible name alone).
#[test]
fn a_refused_value_is_painted_under_its_row_on_every_page() {
    for (source, route, key, typed) in [
        (
            "",
            SettingsRoute::TabColor,
            prefs::EDIT_ACTIVE_TAB_COLOR,
            "orange",
        ),
        (
            "cursor_trail_style = \"comet\"\n",
            SettingsRoute::CursorStyles,
            prefs::EDIT_CURSOR_TRAIL_COLOR,
            "orange",
        ),
        (
            "",
            SettingsRoute::Appearance,
            prefs::EDIT_FOREGROUND,
            "orange",
        ),
    ] {
        let mut lab = Lab::new(source);
        lab.state_mut().navigate(route);
        let wheel = |lab: &Lab| {
            lab.compile(744.0, 420.0)
                .semantic(&UiKey::new("settings/tab-color/wheel"))
                .map(|node| node.rect)
        };
        let wheel_before = wheel(&lab);
        lab.invoke(
            &format!("settings/set/{key}"),
            Some(SemanticInput::Text(typed.to_string())),
        );
        assert!(lab.state().field_errors.contains_key(key), "{key}: refused");
        // At the shipped size the picker kept room for the reason: the
        // refusal moves nothing of it.
        assert_eq!(wheel(&lab), wheel_before, "{key}: the wheel stays put");
        let mut seen = false;
        for (width, height) in [(738.0, 418.0), (1_564.0, 854.0), (640.0, 418.0)] {
            let mut lab_view = |compiled: &crate::native_ui::CompiledUi| {
                let Some(control) = compiled
                    .semantic(&UiKey::new(format!("settings/control/{key}")))
                    .map(|node| node.rect)
                else {
                    return;
                };
                let line = compiled
                    .paint
                    .iter()
                    .find(|node| node.key.as_str() == format!("settings/error/{key}/line-0"))
                    .unwrap_or_else(|| panic!("{key} at {width}×{height}: the reason is painted"));
                assert!(
                    line.rect.y >= control.bottom() - 0.01,
                    "{key} at {width}×{height}: under its row"
                );
                seen = true;
            };
            lab.state_mut()
                .reveal_control(UiKey::new(format!("settings/control/{key}")));
            lab_view(&lab.compile(width, height));
        }
        assert!(seen, "{key}: its row is drawn");
    }
}

/// TOP SETTINGS IS DIRECT AT EVERY SIZE: its rows' labels are the bare name,
/// painted and never a second accessibility stop — no "Changed" spoken that
/// is never painted, at any width or text size — while the Sound page's Music
/// effects row, an ordinary row, still says Changed both ways.
#[test]
fn top_rows_speak_and_paint_their_bare_name_at_every_size() {
    at_each_text_scale(&[1.0, 1.25, 2.0], "top direct rows", |scale| {
        for source in [
            "window_theme = \"dark\"\ntrail_sounds = false\ntheme = \"Dracula\"\n\
             [sparkle_words.profanity]\nenabled = false\n[sparkle_words.feline]\nenabled = false\n",
            "theme = \"dark:Dracula,light:Default Light\"\n",
        ] {
            for (width, height) in [(1_074.0, 900.0), (390.0, 760.0), (738.0, 418.0)] {
                let mut lab = Lab::new(source);
                lab.state_mut().navigate(SettingsRoute::Home);
                let compiled = lab.compile(width, height);
                let context = format!("{width}×{height} at {scale}×: {source}");
                for node in &compiled.semantics {
                    assert!(
                        !node.key.as_str().starts_with("settings/label/"),
                        "{context}: {} is a second stop for its row's name",
                        node.key.as_str()
                    );
                }
                for key in [prefs::EDIT_WINDOW_THEME, KEYWORD_KITTIES_KEY] {
                    if let Some(painted) = painted_row_label(&compiled, key) {
                        assert!(
                            !painted.contains(CHANGED_WORD),
                            "{context}: {key} {painted:?}"
                        );
                    }
                }
            }
        }
    });
    // The Sound page's Music effects row is an ordinary row: it says Changed.
    let mut lab = Lab::new("trail_sounds = false\n");
    lab.state_mut().navigate(SettingsRoute::Sound);
    let compiled = lab.compile(1_074.0, 900.0);
    let label = compiled
        .semantic(&UiKey::new(format!(
            "settings/label/{}",
            prefs::EDIT_TRAIL_SOUNDS
        )))
        .expect("the Sound page's Music effects label")
        .label
        .clone();
    assert!(label.contains(CHANGED_WORD), "{label}");
    assert!(
        painted_row_label(&compiled, prefs::EDIT_TRAIL_SOUNDS)
            .is_some_and(|painted| painted.contains(CHANGED_WORD))
    );
}

/// A LANDING OPENS NO EDIT: the list's own keys (Page Down from the sidebar,
/// Home, End, the arrows) put the keyboard on a text field as on any control,
/// ringed and with no caret, so Home and End go on moving through the list;
/// the field takes them for its caret only once it is typed into. (A Page
/// Down that landed on Searchable scrollback lines used to open an edit
/// there, and End and Home then moved a caret in an empty draft.)
#[test]
fn a_landing_on_a_text_field_leaves_home_and_end_to_the_list() {
    let (width, height) = (1_074.0, 684.0);
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Terminal);
    let compile = |lab: &mut Lab| {
        let _ = lab.compile(width, height);
    };
    let edge = |lab: &mut Lab, home: bool| {
        let event = if home {
            TextInputEvent::Home { extend: false }
        } else {
            TextInputEvent::End { extend: false }
        };
        dispatch(lab, AppEvent::TextInput(event));
        let _ = lab.compile(width, height);
        let (plan, _) = lab.state().scroll_shown().expect("a list");
        let want = if home {
            plan.controls().next()
        } else {
            plan.controls().last()
        }
        .map(|(_, key)| key.clone());
        assert_eq!(lab.state().common.last_focus, want, "home={home}");
        assert!(lab.state().editing_field.is_none(), "home={home}: no edit");
    };
    // Every landing on a text field (Home and End too: Scrollback limit is
    // the list's first control) leaves the list's keys to the list.
    lab.state_mut().common.last_focus = Some(UiKey::new("settings/nav/terminal"));
    compile(&mut lab);
    dispatch(&mut lab, AppEvent::ScrollPage(1));
    compile(&mut lab);
    assert!(
        lab.state()
            .common
            .last_focus
            .as_ref()
            .is_some_and(|key| key.as_str().starts_with("settings/control/")),
        "Page Down takes the keyboard into the list"
    );
    assert!(lab.state().editing_field.is_none(), "and opens no edit");
    assert!(lab.state().common.focus_visible);
    edge(&mut lab, false);
    edge(&mut lab, true);
    let first = lab
        .state()
        .common
        .last_focus
        .clone()
        .expect("the first control");
    assert_eq!(
        first.as_str(),
        format!("settings/control/{}", prefs::EDIT_SCROLLBACK),
        "Scrollback limit, a text field, is the list's first control"
    );
    edge(&mut lab, false);
    edge(&mut lab, true);
    // Typed into, the field takes Home and End for its caret.
    dispatch(
        &mut lab,
        AppEvent::TextInput(TextInputEvent::Commit("5".to_string())),
    );
    compile(&mut lab);
    assert_eq!(
        lab.state().editing_field.as_deref(),
        Some(prefs::EDIT_SCROLLBACK)
    );
    let scroll = lab.state().page_scroll;
    dispatch(
        &mut lab,
        AppEvent::TextInput(TextInputEvent::End { extend: false }),
    );
    compile(&mut lab);
    assert_eq!(lab.state().common.last_focus.as_ref(), Some(&first));
    assert_eq!(
        lab.state().page_scroll,
        scroll,
        "the caret moves, not the list"
    );
    // Escape ends the edit, and End is the list's again.
    dispatch(&mut lab, AppEvent::TextInput(TextInputEvent::Cancel));
    compile(&mut lab);
    edge(&mut lab, false);
}

/// A closed font row is a list button for every key: Home and End move the
/// list from it (they used to count as caret keys for a caret it does not
/// draw).
#[test]
fn home_and_end_on_a_closed_font_row_move_the_list() {
    let (width, height) = (1_074.0, 684.0);
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::TextFonts);
    let _ = lab.compile(width, height);
    dispatch(
        &mut lab,
        AppEvent::FocusChanged(Some(UiKey::new(format!(
            "settings/control/{}",
            prefs::EDIT_FONT_FAMILY
        )))),
    );
    assert!(lab.state().editing_field.is_none(), "focus opens no edit");
    for home in [false, true, false] {
        let event = if home {
            TextInputEvent::Home { extend: false }
        } else {
            TextInputEvent::End { extend: false }
        };
        dispatch(&mut lab, AppEvent::TextInput(event));
        let _ = lab.compile(width, height);
        let (plan, _) = lab.state().scroll_shown().expect("a list");
        let want = if home {
            plan.controls().next()
        } else {
            plan.controls().last()
        }
        .map(|(_, key)| key.clone());
        assert_eq!(lab.state().common.last_focus, want, "home={home}");
    }
}

/// A SLIDER'S KEYS REACH ITS WHOLE DOMAIN, whatever span its bar draws: Font
/// size's bar shows 8–36 px, but Right at 36 writes 37, Left at 8 writes 7,
/// Shift-Right ten more, and a size set past the bar comes back up (59 →
/// 60); automation reaches the domain's ends. (The bar's span had become the
/// value's limit: Right at 36 did nothing, and after one Left from 60 the
/// span closed behind the user at 59.) Its value keys are the arrows alone:
/// Home and End are the list's, and write nothing.
#[test]
fn font_size_keys_reach_the_whole_runtime_range() {
    let key = UiKey::new(format!("settings/control/{}", prefs::EDIT_FONT_PX));
    let press = |from: f64, event: TextInputEvent| -> Option<String> {
        let mut lab = Lab::new(&format!("font_px = {from:.1}\n"));
        lab.state_mut().navigate(SettingsRoute::TextFonts);
        let _ = lab.compile(1_074.0, 684.0);
        lab.dispatch(AppEvent::FocusChanged(Some(key.clone())));
        let outcome = lab.dispatch_outcome(AppEvent::TextInput(event));
        assert_eq!(
            lab.state().common.last_focus.as_ref(),
            Some(&key),
            "the keyboard stays on the slider"
        );
        outcome.effects.iter().find_map(|effect| match effect {
            AppEffect::ConfigPatch { patch, .. } => {
                patch.edits.first().and_then(|edit| edit.value.clone())
            }
            _ => None,
        })
    };
    let right = TextInputEvent::Right { extend: false };
    let left = TextInputEvent::Left { extend: false };
    assert_eq!(press(36.0, right.clone()).as_deref(), Some("37"));
    assert_eq!(press(8.0, left.clone()).as_deref(), Some("7"));
    assert_eq!(press(7.0, left.clone()).as_deref(), Some("6"));
    assert_eq!(press(59.0, right.clone()).as_deref(), Some("60"));
    assert_eq!(
        press(36.0, TextInputEvent::Right { extend: true }).as_deref(),
        Some("46")
    );
    assert_eq!(press(6.0, left), None, "the domain's first end");
    assert_eq!(press(200.0, right), None, "and its last");
    for edge in [
        TextInputEvent::Home { extend: false },
        TextInputEvent::End { extend: false },
    ] {
        let mut lab = Lab::new("font_px = 14.0\n");
        lab.state_mut().navigate(SettingsRoute::TextFonts);
        let _ = lab.compile(1_074.0, 684.0);
        lab.dispatch(AppEvent::FocusChanged(Some(key.clone())));
        let outcome = lab.dispatch_outcome(AppEvent::TextInput(edge.clone()));
        assert!(
            !outcome
                .effects
                .iter()
                .any(|effect| matches!(effect, AppEffect::ConfigPatch { .. })),
            "{edge:?} on a slider is the list's key, never a value"
        );
    }
    // A size set by automation anywhere in the domain, its ends included,
    // is taken.
    for size in [6.0, 60.0, 200.0] {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::TextFonts);
        let patch = lab.invoke(
            &format!("settings/set/{}", prefs::EDIT_FONT_PX),
            Some(SemanticInput::Number(size)),
        );
        assert_eq!(
            patch.and_then(|(patch, _)| patch.edits.first().and_then(|edit| edit.value.clone())),
            Some(format!("{size}"))
        );
    }
}

/// EVERY CONTROL SAYS ITS ROW'S NOTE: a slider and a text field carry it as
/// their description, as a switch and a button do (Top padding's "The top
/// cannot be more than Sides and bottom." and Scrollback limit's "0 keeps
/// every line." reached no assistive technology: their painted lines are
/// paint only, and their specs had no description).
#[test]
fn slider_and_text_field_rows_say_their_notes() {
    let mut lab = Lab::new("");
    let mut reached = std::collections::HashSet::new();
    for route in [SettingsRoute::WindowTabs, SettingsRoute::Terminal] {
        lab.state_mut().navigate(route);
        let compiled = lab.compile(1_564.0, 2_000.0);
        for node in &compiled.semantics {
            let Some(key) = node.key.as_str().strip_prefix("settings/control/") else {
                continue;
            };
            if !matches!(node.role, SemanticRole::Slider | SemanticRole::TextField) {
                continue;
            }
            let Some(note) = prefs::footnote_of(key) else {
                continue;
            };
            assert!(
                node.description
                    .as_deref()
                    .is_some_and(|description| description.contains(note)),
                "{key} says its note: {:?}",
                node.description
            );
            reached.insert((key.to_string(), node.role));
        }
    }
    assert!(
        reached.contains(&(
            prefs::EDIT_WINDOW_PADDING_TOP.to_string(),
            SemanticRole::Slider
        )),
        "{reached:?}"
    );
    assert!(
        reached.contains(&(prefs::EDIT_SCROLLBACK.to_string(), SemanticRole::TextField)),
        "{reached:?}"
    );
}

/// A CARD'S UNSET FIELDS MARK THEIR DEFAULTS ALIKE: on Cursor Styles'
/// Adjust card the trail colour and its accent, both unset, either both
/// paint "(default)" or neither does, at every text size and width, whatever
/// each colour's name is long ("Brighter cursor color" lost its mark beside
/// "Shifting colors (default)", "Pale blue (default)" and "Cursor color
/// (default)"). Each field's value keeps the whole words.
#[test]
fn a_cards_unset_fields_mark_their_defaults_alike() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "default marks", |scale| {
        let mut both = 0;
        for style in ["classic", "comet", "lumen", "sparkle", "laser"] {
            for (width, height) in [
                (1_606.0, 5_000.0),
                (1_219.0, 5_000.0),
                (1_095.0, 5_000.0),
                (744.0, 5_000.0),
                (584.0, 6_000.0),
                (318.0, 9_000.0),
            ] {
                let mut lab = Lab::new(&format!("cursor_trail_style = \"{style}\"\n"));
                lab.state_mut().navigate(SettingsRoute::CursorStyles);
                let compiled = lab.compile(width, height);
                let painted: Vec<String> = [
                    prefs::EDIT_CURSOR_TRAIL_COLOR,
                    prefs::EDIT_CURSOR_TRAIL_ACCENT,
                ]
                .iter()
                .filter_map(|key| {
                    let control = format!("settings/control/{key}");
                    compiled.paint.iter().find_map(|node| match &node.content {
                        UiContent::TextField(field) if node.key.as_str() == control => {
                            field.spec.placeholder.clone()
                        }
                        _ => None,
                    })
                })
                .collect();
                let [color, accent] = painted.as_slice() else {
                    continue;
                };
                both += 1;
                assert_eq!(
                    color.ends_with(" (default)"),
                    accent.ends_with(" (default)"),
                    "{style} at {width} and {scale}x: {color:?} beside {accent:?}"
                );
                for key in [
                    prefs::EDIT_CURSOR_TRAIL_COLOR,
                    prefs::EDIT_CURSOR_TRAIL_ACCENT,
                ] {
                    let value = compiled
                        .semantic(&UiKey::new(format!("settings/control/{key}")))
                        .and_then(|node| match &node.value {
                            SemanticValue::Text(value) => Some(value.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    assert!(
                        value.ends_with(" (default)"),
                        "{style} at {width} and {scale}x: {key} says {value:?}"
                    );
                }
            }
        }
        assert!(
            both > 10,
            "the Adjust card's two colours drawn in {both} cases"
        );
    });
}

/// THE TRAIL COLOURS' NOTE SAYS WHAT THEY DO FOR THE SAVED STYLE: blank
/// Classic shifts through colours (never "follows the cursor color"), Fire
/// on a block does not use them, Laser keeps its violet, Comet its pale blue
/// until a cursor colour is set; the note Modified paints and each control
/// says is that style's, the one [`prefs::trail_color_note`] composes.
#[test]
fn the_trail_colors_note_names_the_saved_styles_law() {
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    for (style, says, never) in [
        (
            "classic",
            "Classic shifts through colors",
            "follows the cursor color",
        ),
        (
            "fire",
            "Fire draws its own colors on a block cursor",
            "Blank:",
        ),
        (
            "laser",
            "Lightning keeps its violet",
            "until you set a cursor color",
        ),
        (
            "comet",
            "Comet keeps its pale blue until you set a cursor color",
            "Lightning",
        ),
        ("lumen", "Lumen follows the cursor color", "keeps its"),
    ] {
        let mut lab = Lab::new(&format!(
            "cursor_trail_style = \"{style}\"\ncursor_trail_color = \"#FF0000\"\n\
             cursor_trail_accent = \"#00FF00\"\n"
        ));
        lab.state_mut().navigate(SettingsRoute::Modified);
        let compiled = lab.compile(1_074.0, 1_400.0);
        let note = prefs::trail_color_note(style, true, false);
        assert!(note.contains(says), "{style}: {note}");
        assert!(!note.contains(never), "{style}: {note}");
        let painted = compiled
            .paint
            .iter()
            .filter_map(|node| {
                let key = node.key.as_str();
                (key.starts_with("settings/shared-note/") && !key.contains("/line-"))
                    .then(|| key.trim_start_matches("settings/shared-note/").to_string())
            })
            .next()
            .expect("the run note");
        assert_eq!(
            words(&painted_text(
                &compiled,
                &format!("settings/shared-note/{painted}/line-")
            )),
            words(&note),
            "{style}"
        );
        assert!(
            compiled
                .semantic(&UiKey::new(format!(
                    "settings/control/{}",
                    prefs::EDIT_CURSOR_TRAIL_COLOR
                )))
                .and_then(|control| control.description.as_deref())
                .is_some_and(|description| description.contains(&note)),
            "{style}: the control says it"
        );
    }
}

/// A NOTE A RUN OF ROWS SHARES IS THE RUN'S ([`prefs::RowNote::Shared`]):
/// Modified listing both trail colors draws their one note once, as the stop
/// after the second (`settings/shared-note/<key>`), apart from that row's own
/// lines, never inside them as if that row's alone; each color's control
/// says it. Negative control: one trail color listed alone draws the note as
/// its own, inside its row, and no run note.
#[test]
fn a_note_a_run_shares_is_the_stop_after_the_run() {
    // The two trail colours' one note, for the default style on a block.
    let note = &prefs::trail_color_note(prefs::DEFAULT_CURSOR_TRAIL_STYLE, true, false);
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    for (width, height) in [(1_074.0, 900.0), (584.0, 900.0), (390.0, 900.0)] {
        let mut lab =
            Lab::new("cursor_trail_color = \"#FF0000\"\ncursor_trail_accent = \"#00FF00\"\n");
        lab.state_mut().navigate(SettingsRoute::Modified);
        let compiled = lab.compile(width, height);
        let shared: Vec<_> = compiled
            .paint
            .iter()
            .filter(|node| {
                let key = node.key.as_str();
                key.starts_with("settings/shared-note/") && !key.contains("/line-")
            })
            .collect();
        assert_eq!(shared.len(), 1, "{width}: one run note: {shared:?}");
        let closer = shared[0]
            .key
            .as_str()
            .strip_prefix("settings/shared-note/")
            .unwrap()
            .to_string();
        assert!(
            [
                prefs::EDIT_CURSOR_TRAIL_COLOR,
                prefs::EDIT_CURSOR_TRAIL_ACCENT
            ]
            .contains(&closer.as_str()),
            "{width}: {closer}"
        );
        assert_eq!(
            words(&painted_text(
                &compiled,
                &format!("settings/shared-note/{closer}/line-")
            )),
            words(note),
            "{width}: the run note paints whole"
        );
        for key in [
            prefs::EDIT_CURSOR_TRAIL_COLOR,
            prefs::EDIT_CURSOR_TRAIL_ACCENT,
        ] {
            assert!(
                !words(&painted_text(&compiled, &format!("settings/detail/{key}/")))
                    .contains(&words(note)),
                "{width}: the note is not {key}'s own lines"
            );
            assert!(
                compiled
                    .semantic(&UiKey::new(format!("settings/control/{key}")))
                    .and_then(|control| control.description.as_deref())
                    .is_some_and(|description| description.contains(note)),
                "{width}: {key}'s control says the note"
            );
        }
        // After the closing row, whole, set apart.
        let row_bottom = compiled
            .paint
            .iter()
            .filter(|node| {
                let key = node.key.as_str();
                (key.starts_with("settings/row/") && key.ends_with(&format!("/{closer}")))
                    || key == format!("settings/row-detail/{closer}")
            })
            .map(|node| node.rect.bottom())
            .reduce(f32::max)
            .expect("the closing row");
        assert!(
            shared[0].rect.y >= row_bottom - 0.01,
            "{width}: the run note {:?} follows its run's last row (bottom {row_bottom})",
            shared[0].rect
        );
        // Read by no one twice: no words of its own for assistive
        // technology (the controls say it), its lines paint only.
        assert!(
            compiled
                .semantic(&shared[0].key)
                .is_none_or(|node| node.label.is_empty()),
            "{width}: the run note speaks no words of its own"
        );
        assert!(
            compiled
                .paint
                .iter()
                .filter(|node| node
                    .key
                    .as_str()
                    .starts_with(&format!("settings/shared-note/{closer}/line-")))
                .all(|line| compiled.semantic(&line.key).is_none()),
            "{width}: the run note's lines paint only"
        );
    }
    // One trail color alone: the note is its row's own.
    let mut lab = Lab::new("cursor_trail_color = \"#FF0000\"\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    let compiled = lab.compile(1_074.0, 900.0);
    assert!(
        !compiled
            .paint
            .iter()
            .any(|node| node.key.as_str().starts_with("settings/shared-note/")),
        "a lone row has no run note"
    );
    assert!(
        words(&format!(
            "{} {}",
            painted_text(
                &compiled,
                &format!("settings/detail/{}/", prefs::EDIT_CURSOR_TRAIL_COLOR)
            ),
            painted_text(
                &compiled,
                &format!("settings/note/{}/", prefs::EDIT_CURSOR_TRAIL_COLOR)
            )
        ))
        .contains(&words(note)),
        "the lone trail color draws the note inside its row"
    );
}

/// A ONE-LINE ROW LEAVES ITS NAME ITS MEASURE BESIDE EVERYTHING ITS LINE
/// HOLDS: what a row seats beside its control (the Theme row's Browse
/// themes…, a search hit's Choose… for the tab color) comes out of the
/// control's room, and the row stacks before its name would get less than
/// its floor ([`one_line_name_floor`]). (At 80x24 Appearance's Color theme row went
/// one line with its name cut to "Color" / "the…" in 32pt, beside its picker
/// and Browse themes….)
#[test]
fn a_one_line_row_leaves_its_name_room_beside_everything_on_its_line() {
    for search in ["", "tab color"] {
        let mut state = SettingsViewState::new(&Config::default());
        state.search = search.to_string();
        let mut seated = 0usize;
        for index in 0..state.legacy.fields.len() {
            let field = &state.legacy.fields[index];
            let mut text_width = 300.0_f32;
            while text_width <= 760.0 {
                let finished = finished_setting_row(
                    &state,
                    index,
                    field,
                    false,
                    SettingsWidth::Compact,
                    text_width,
                    None,
                    None,
                    None,
                );
                if !finished.stacked {
                    let room = one_line_label_room(&finished.row, text_width);
                    assert!(
                        room + 0.01 >= one_line_name_floor(&finished.name),
                        "{search:?}: {} on one line in a {text_width}pt column leaves \
                         its name {room}pt",
                        field.key
                    );
                    if one_line_row_extras(&finished.row) > 0.0 {
                        seated += 1;
                    }
                }
                text_width += 4.0;
            }
        }
        // The sweep reaches one-line rows that seat something beside their
        // control, so the measure above is the one under test.
        assert!(
            seated > 0,
            "{search:?}: a one-line row with something beside its control"
        );
    }
}

/// PAGE DOWN NEVER SHOWS AGAIN A ROW IT SHOWED WHOLE (ONE MEASURE): a row a
/// screen draws whole and live in its room — the one a status bar leaves it
/// — with the words under it (its note) whole too counts as shown, its
/// card's padding cut or not, so the next screen starts after it; and a row
/// drawn in the strip under the room is a fading picture, never one the
/// screen showed. Judged from what is drawn, on the field pages at 80×24 and
/// the shipped 102×29, until the last screen (which fills itself from the
/// stops before it). A line of the note that the room's edge cuts away
/// entirely is not drawn at all, so the note's block is judged whole too: in
/// Noto Sans, Security's notifications row keeps a blank line for its tags
/// and has its words on the line under it, which the 80×24 room's edge cuts
/// away whole.
#[test]
fn page_down_never_repeats_a_row_it_showed_whole() {
    let whole_rows = |compiled: &crate::native_ui::CompiledUi, list: LogicalRect, room: f32| {
        let in_room = |rect: LogicalRect, clip: LogicalRect| {
            clip == rect && rect.y >= list.y - 0.5 && rect.bottom() <= list.y + room + 0.5
        };
        let rows: Vec<&crate::native_ui::PaintNode> = compiled
            .paint
            .iter()
            .filter(|node| node.key.as_str().starts_with("settings/row/"))
            .collect();
        rows.iter()
            .enumerate()
            .filter(|(index, row)| {
                // The words under the row, down to the next row: its note.
                let below = rows
                    .get(index + 1)
                    .map_or(f32::INFINITY, |next| next.rect.y);
                let key = row.key.as_str().rsplit('/').next().unwrap_or_default();
                let note = format!("settings/detail/{key}");
                in_room(row.rect, row.clip)
                    && compiled.semantic(&row.key).is_some()
                    && compiled
                        .paint
                        .iter()
                        .filter(|node| node.key.as_str() == note)
                        .all(|block| in_room(block.rect, block.clip))
                    && compiled.paint.iter().all(|node| {
                        !matches!(node.content, UiContent::Text(_))
                            || node.rect.x < row.rect.x - 0.5
                            || node.rect.right() > row.rect.right() + 0.5
                            || node.rect.y < row.rect.bottom() - 0.5
                            || node.rect.y >= below - 0.5
                            || in_room(node.rect, node.clip)
                    })
            })
            .map(|(_, row)| row.key.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let mut turned = 0;
    for route in [
        SettingsRoute::Terminal,
        SettingsRoute::Appearance,
        SettingsRoute::Security,
        SettingsRoute::Sound,
        SettingsRoute::WindowTabs,
        SettingsRoute::KeyboardInput,
        SettingsRoute::CursorMotion,
    ] {
        for (width, height) in [(584.0, 348.0), (738.0, 418.0)] {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            let compiled = lab.compile(width, height);
            let (plan, _) = lab.state().scroll_shown().expect("a list");
            let (list, room) = compiled_list(&compiled, &plan).expect("the list");
            let mut shown = whole_rows(&compiled, list, room);
            loop {
                dispatch(&mut lab, AppEvent::ScrollPage(1));
                let compiled = lab.compile(width, height);
                let (plan, view) = lab.state().scroll_shown().expect("a list");
                let (list, room) = compiled_list(&compiled, &plan).expect("the list");
                let next = whole_rows(&compiled, list, room);
                // The last screen, seated where the list's end puts it (it
                // may show whole a stop over its seat, drawn over its top).
                let top = lab
                    .state()
                    .scroll_record
                    .borrow()
                    .as_ref()
                    .map_or(0, |record| record.top());
                if top >= plan.limit() {
                    break;
                }
                turned += 1;
                let again: Vec<_> = shown.intersection(&next).collect();
                assert!(
                    again.is_empty(),
                    "{route:?} {width}×{height} screen {}: Page Down shows again {again:?}",
                    view.start
                );
                shown = next;
            }
        }
    }
    assert!(turned > 0, "some screen turns to one that is not the last");
}

/// A PRESS NEVER MOVES A CONTROL THE SCREEN SHOWS WHOLE, AND THE BAR IT
/// BRINGS NEVER COVERS IT: on the field pages at the shipped 102×29 and
/// 150×48, toggling the last switch the list's room shows whole leaves
/// it where it was, under the status bar's edge — the strip under the room,
/// which the bar takes, holds only a picture — and the first switch the
/// room's edge cuts, the screen's fading peek, comes whole into the room
/// for a press on its picture and then toggles where it is, never under
/// the bar.
#[test]
fn a_press_never_moves_a_whole_control_and_brings_a_cut_one_whole() {
    let mut cut_pressed = 0;
    for route in [
        SettingsRoute::Terminal,
        SettingsRoute::Security,
        SettingsRoute::KeyboardInput,
        SettingsRoute::WindowTabs,
        SettingsRoute::Sound,
    ] {
        for (width, height) in [(738.0, 418.0), (1_074.0, 684.0)] {
            let switches = |compiled: &crate::native_ui::CompiledUi| {
                compiled
                    .paint
                    .iter()
                    .filter(|node| matches!(node.content, UiContent::Switch(_)))
                    .filter_map(|node| {
                        let key = node.key.as_str().strip_prefix("settings/control/")?;
                        Some((key.to_string(), node.rect, node.clip))
                    })
                    .collect::<Vec<_>>()
            };
            for cut in [false, true] {
                let mut lab = Lab::new("");
                lab.state_mut().navigate(route);
                let before = lab.compile(width, height);
                let (plan, _) = lab.state().scroll_shown().expect("a list");
                let (list, room) = compiled_list(&before, &plan).expect("the list");
                let edge = list.y + room;
                let candidates = switches(&before);
                let pick = if cut {
                    candidates
                        .iter()
                        .find(|(_, rect, clip)| rect.bottom() > edge + 0.5 && clip.y < edge - 0.5)
                } else {
                    candidates
                        .iter()
                        .rev()
                        .find(|(_, rect, clip)| rect == clip && rect.bottom() <= edge + 0.5)
                };
                let Some((key, rect, _)) = pick.cloned() else {
                    continue;
                };
                let control = UiKey::new(format!("settings/control/{key}"));
                let value = |compiled: &crate::native_ui::CompiledUi| {
                    compiled
                        .semantic(&control)
                        .and_then(|node| match node.value {
                            crate::native_ui::SemanticValue::Bool(on) => Some(on),
                            _ => None,
                        })
                };
                // A switch over the room's edge is the screen's peek, a
                // picture: a press on it scrolls it whole into the room and
                // works nothing, and the next one works it.
                let (rect, before) = if cut {
                    assert_eq!(
                        value(&before),
                        None,
                        "{route:?} {width}×{height} {key}: live"
                    );
                    lab.dispatch(AppEvent::PressListPicture(control.clone()));
                    let scrolled = lab.compile(width, height);
                    let (plan, _) = lab.state().scroll_shown().expect("a list");
                    let (list, room) = compiled_list(&scrolled, &plan).expect("the list");
                    let now = rect_of(&scrolled, control.as_str());
                    assert!(
                        now.y >= list.y - 0.5 && now.bottom() <= list.y + room + 0.5,
                        "{route:?} {width}×{height} {key}: {now:?} scrolled whole into the room"
                    );
                    (now, scrolled)
                } else {
                    (rect, before)
                };
                let Some(on) = value(&before) else {
                    continue;
                };
                // A pointer press moves the keyboard to the control first.
                lab.state_mut().common.last_focus = Some(control.clone());
                lab.commit(
                    &format!("settings/set/{key}"),
                    Some(SemanticInput::Bool(!on)),
                );
                let after = lab.compile(width, height);
                let context = format!("{route:?} {width}×{height} {key} cut={cut}");
                assert!(
                    after.semantic(&UiKey::new("settings/undo")).is_some(),
                    "{context}: the bar is up"
                );
                let now = rect_of(&after, control.as_str());
                cut_pressed += usize::from(cut);
                assert_eq!(now, rect, "{context}: the press moved it");
                if let Some(bar) = after
                    .paint
                    .iter()
                    .find(|node| node.key.as_str() == "settings/feedback")
                {
                    assert!(
                        now.bottom() <= bar.rect.y + 0.5,
                        "{context}: {now:?} under the bar {:?}",
                        bar.rect
                    );
                }
            }
        }
    }
    assert!(
        cut_pressed > 0,
        "some screen cuts a switch at its room's edge"
    );
}

/// A PRESS ON A CONTROL AN EDGE OF THE ROOM CUTS KEEPS IT UNDER THE POINTER
/// (lead-known-open #31), as a platform list scrolls a clicked row into view
/// by its hidden part: pressed anywhere it shows in the room, the switch
/// lands whole in the room and the point it was pressed at is on it still —
/// one cut at the top of a last screen, live where it shows, turns once; one
/// the room's bottom edge cuts on a first screen is the screen's peek,
/// fading from its top, a picture a press scrolls in and works nothing on
/// (ONE BOUNDARY, [`scroll::ScrollPlan::screen`]): drawn whole and at full
/// strength over the edge, it answered a press over its upper part only, and
/// Page Down turned onto it again. A stop's worth of scroll (the least whole
/// stop, 48pt or more) carried a bottom one out from under the pointer: at
/// 150×48 Window's last switch went from 592 to 526 under a pointer at 597,
/// its hover left behind; a top one came down past a pointer near its cut.
#[test]
fn a_press_on_a_cut_control_keeps_it_under_the_pointer() {
    let mut pressed = [0, 0];
    for route in [
        SettingsRoute::Terminal,
        SettingsRoute::Security,
        SettingsRoute::KeyboardInput,
        SettingsRoute::WindowTabs,
        SettingsRoute::Sound,
        SettingsRoute::Harness,
        SettingsRoute::TextFonts,
        SettingsRoute::CursorMotion,
    ] {
        for (width, height) in [
            (584.0, 348.0),
            (584.0, 376.0),
            (738.0, 418.0),
            (1_074.0, 684.0),
            (1_193.0, 516.0),
            (1_564.0, 854.0),
        ] {
            // The first screen (its bottom edge) and the last (its top).
            for last in [false, true] {
                let mut lab = Lab::new("");
                lab.state_mut().navigate(route);
                let before = if last {
                    to_the_last_screen(&mut lab, width, height)
                } else {
                    lab.compile(width, height)
                };
                let (plan, _) = lab.state().scroll_shown().expect("a list");
                let (list, room) = compiled_list(&before, &plan).expect("the list");
                let edge = list.y + room;
                let cut = before.paint.iter().find_map(|node| {
                    let UiContent::Switch(_) = node.content else {
                        return None;
                    };
                    let key = node.key.as_str().strip_prefix("settings/control/")?;
                    let shows = if last {
                        node.clip.y > node.rect.y + 0.5 && node.clip.height > 4.0
                    } else {
                        node.rect.y < edge - 4.0 && node.rect.bottom() > edge + 0.5
                    };
                    shows.then(|| {
                        let (top, bottom) = if last {
                            (node.clip.y, node.rect.bottom())
                        } else {
                            (node.rect.y, edge)
                        };
                        (key.to_string(), top, bottom, node.fade)
                    })
                });
                // At the last screen's top, a switch the edge drew without its
                // name, or a sliver of: not drawn, its picture the list's.
                let veiled = || {
                    before.list_pictures.iter().find_map(|picture| {
                        let key = picture.key.as_str().strip_prefix("settings/control/")?;
                        (picture.rect.y <= list.y + 60.0
                            && before.paint.iter().all(|node| node.key != picture.key))
                        .then(|| (key.to_string(), picture.rect.y, picture.rect.bottom()))
                    })
                };
                let (key, shown_top, shown_bottom, fade, live) = match cut {
                    Some((key, top, bottom, fade)) => {
                        let live = before
                            .semantic(&UiKey::new(format!("settings/control/{key}")))
                            .map(|node| node.value.clone());
                        (key, top, bottom, fade, live)
                    }
                    None if last => match veiled() {
                        Some((key, top, bottom)) => (key, top, bottom, None, None),
                        None => continue,
                    },
                    None => continue,
                };
                let control = UiKey::new(format!("settings/control/{key}"));
                let was_picture = live.is_none();
                let on = if last && live.is_none() {
                    // Drawn without its name, or a sliver of it: a press on
                    // its picture scrolls it whole into view and works
                    // nothing.
                    lab.dispatch(AppEvent::PressListPicture(control.clone()));
                    match lab
                        .compile(width, height)
                        .semantic(&control)
                        .map(|node| node.value.clone())
                    {
                        Some(crate::native_ui::SemanticValue::Bool(on)) => on,
                        Some(_) => continue,
                        None => panic!("{route:?} {width}×{height}: {key} scrolled in"),
                    }
                } else if last {
                    // The press names its control, not a point (a commit
                    // with the switch focused), so one press answers for a
                    // pointer anywhere on what shows.
                    let Some(crate::native_ui::SemanticValue::Bool(on)) = live else {
                        panic!("{route:?} {width}×{height}: {key} cut at the top is live");
                    };
                    lab.state_mut().common.last_focus = Some(control.clone());
                    lab.commit(
                        &format!("settings/set/{key}"),
                        Some(SemanticInput::Bool(!on)),
                    );
                    !on
                } else {
                    // The peek: fading from its top, a picture.
                    assert!(
                        live.is_none() && fade.is_some_and(|(top, _)| top <= shown_top + 0.5),
                        "{route:?} {width}×{height}: {key} over the room's edge is a fading \
                         picture ({fade:?})"
                    );
                    lab.dispatch(AppEvent::PressListPicture(control.clone()));
                    match lab
                        .compile(width, height)
                        .semantic(&control)
                        .map(|node| node.value.clone())
                    {
                        Some(crate::native_ui::SemanticValue::Bool(on)) => on,
                        other => panic!("{route:?} {width}×{height}: {key} scrolled in: {other:?}"),
                    }
                };
                let after = lab.compile(width, height);
                let now = rect_of(&after, control.as_str());
                let (plan, _) = lab.state().scroll_shown().expect("a list");
                let (list, room) = compiled_list(&after, &plan).expect("the list");
                // Pressed at the top, the middle and the bottom of what shows.
                for at in [
                    shown_top + 1.0,
                    (shown_top + shown_bottom) / 2.0,
                    shown_bottom - 1.0,
                ] {
                    let context =
                        format!("{route:?} {width}×{height} last={last} {key} pressed at {at}");
                    assert!(
                        now.y >= list.y - 0.5 && now.bottom() <= list.y + room + 0.5,
                        "{context}: {now:?} whole in the room"
                    );
                    assert!(
                        now.y <= at && at <= now.bottom(),
                        "{context}: {now:?} still under the pointer"
                    );
                    assert_eq!(
                        after.semantic(&control).map(|node| node.value.clone()),
                        Some(crate::native_ui::SemanticValue::Bool(on)),
                        "{context}: turned once where it was live, not at all where it was a \
                         picture"
                    );
                }
                if was_picture {
                    // Whole now, the next press works it and moves nothing.
                    lab.state_mut().common.last_focus = Some(control.clone());
                    lab.commit(
                        &format!("settings/set/{key}"),
                        Some(SemanticInput::Bool(!on)),
                    );
                    let worked = lab.compile(width, height);
                    assert_eq!(
                        rect_of(&worked, control.as_str()),
                        now,
                        "{route:?} {width}×{height}: {key} pressed whole moved"
                    );
                }
                pressed[usize::from(last)] += 1;
            }
        }
    }
    assert!(
        pressed.iter().all(|count| *count > 0),
        "some screens cut a switch at each edge: {pressed:?}"
    );
}

/// THE NUDGE A PRESS GAVE THE LIST STANDS UNTIL THE LIST SCROLLS: after a
/// press scrolled a cut switch whole by its hidden part — up at a first
/// screen's bottom edge (a press on its picture there), down at a last
/// screen's top — a press on another
/// switch the screen shows whole, and the keyboard moving to a control it
/// shows whole, move nothing, as a platform list holds the place a click
/// scrolled it to; and a scroll puts the list back on its stops, at an end of
/// the list too (the wheel up at its top shows the top again, down at its end
/// the end). The nudge was the pressed control's alone: at 150×48 Window, a
/// press on Tab status badge after Mark connected sessions on tabs had nudged
/// the list sent the list back down 20pt, the switch out from under a
/// pointer at its top and the first one cut again.
#[test]
fn a_press_nudge_stands_until_the_list_scrolls() {
    let mut checked = [0, 0];
    // The keyboard's moves that held the nudge, at each edge, and those that
    // showed a stop the nudged screen cut.
    let mut held = [0, 0];
    let mut revealed_whole = 0;
    for route in [
        SettingsRoute::Terminal,
        SettingsRoute::Security,
        SettingsRoute::KeyboardInput,
        SettingsRoute::WindowTabs,
        SettingsRoute::Sound,
        SettingsRoute::Harness,
        SettingsRoute::TextFonts,
        SettingsRoute::CursorMotion,
    ] {
        for (width, height) in [
            (584.0, 348.0),
            (584.0, 376.0),
            (738.0, 418.0),
            (1_074.0, 684.0),
            (1_193.0, 516.0),
            (1_564.0, 854.0),
        ] {
            for last in [false, true] {
                let switches = |compiled: &crate::native_ui::CompiledUi| {
                    compiled
                        .paint
                        .iter()
                        .filter(|node| matches!(node.content, UiContent::Switch(_)))
                        .filter_map(|node| {
                            let key = node.key.as_str().strip_prefix("settings/control/")?;
                            let control = UiKey::new(node.key.as_str());
                            let on =
                                compiled
                                    .semantic(&control)
                                    .and_then(|semantic| match semantic.value {
                                        crate::native_ui::SemanticValue::Bool(on) => Some(on),
                                        _ => None,
                                    })?;
                            Some((key.to_string(), node.rect, node.clip, on))
                        })
                        .collect::<Vec<_>>()
                };
                // The first press: a switch an edge of the room cuts.
                let nudged = |lab: &mut Lab| {
                    lab.state_mut().navigate(route);
                    let before = if last {
                        to_the_last_screen(lab, width, height)
                    } else {
                        lab.compile(width, height)
                    };
                    let (plan, _) = lab.state().scroll_shown().expect("a list");
                    let (list, room) = compiled_list(&before, &plan).expect("the list");
                    let edge = list.y + room;
                    // At the last screen's top: a switch the edge cuts, live
                    // under its name — or, drawn without its name or a sliver
                    // of, its picture (`None` for its rect until it shows).
                    let (key, rect) = if last {
                        match switches(&before)
                            .into_iter()
                            .find(|(_, rect, clip, _)| clip.y > rect.y + 0.5 && clip.height > 4.0)
                        {
                            Some((key, rect, _, _)) => (key, Some(rect)),
                            None => before.list_pictures.iter().find_map(|picture| {
                                let key = picture.key.as_str().strip_prefix("settings/control/")?;
                                (picture.rect.y <= list.y + 60.0
                                    && before.paint.iter().all(|node| node.key != picture.key))
                                .then(|| (key.to_string(), None))
                            })?,
                        }
                    } else {
                        // Over the room's edge, the screen's peek: a picture.
                        before
                            .paint
                            .iter()
                            .filter(|node| matches!(node.content, UiContent::Switch(_)))
                            .find(|node| {
                                node.rect.y < edge - 4.0 && node.rect.bottom() > edge + 0.5
                            })
                            .and_then(|node| {
                                let key = node.key.as_str().strip_prefix("settings/control/")?;
                                Some((key.to_string(), Some(node.rect)))
                            })?
                    };
                    let control = format!("settings/control/{key}");
                    let picture = before
                        .list_pictures
                        .iter()
                        .find(|picture| picture.key.as_str() == control)
                        .map(|picture| picture.rect);
                    if last && rect.is_some() {
                        let on = switches(&before)
                            .into_iter()
                            .find(|(at, ..)| *at == key)
                            .map(|(.., on)| on)?;
                        lab.state_mut().common.last_focus = Some(UiKey::new(control.as_str()));
                        lab.commit(
                            &format!("settings/set/{key}"),
                            Some(SemanticInput::Bool(!on)),
                        );
                    } else {
                        dispatch(
                            lab,
                            AppEvent::PressListPicture(UiKey::new(control.as_str())),
                        );
                    }
                    let after = lab.compile(width, height);
                    let now = rect_of(&after, &control);
                    // A picture cut at the top showed the switch's foot.
                    let rect = rect.or_else(|| {
                        picture.map(|shown| {
                            LogicalRect::new(
                                now.x,
                                shown.bottom() - now.height,
                                now.width,
                                now.height,
                            )
                        })
                    })?;
                    let moved = now.y - rect.y;
                    // Nudged by its hidden part, not seated a stop further on.
                    let nudged = if last {
                        moved > 0.5 && moved < rect.height
                    } else {
                        moved < -0.5 && moved > -rect.height
                    };
                    nudged.then_some((before, after, control, list, edge))
                };
                let mut lab = Lab::new("");
                let Some((_, after, first, list, edge)) = nudged(&mut lab) else {
                    continue;
                };
                // Shown whole as the keyboard reads it — the switch and its
                // stop's words ([`Reveal::Keep`]) — so its move to it has
                // nothing to bring into view: a nudge back at a last screen's
                // top cuts what ended flush at its bottom.
                let (shown, view) = lab.state().scroll_shown().expect("a list");
                let whole = switches(&after)
                    .into_iter()
                    .filter(|(key, rect, clip, _)| {
                        let control = format!("settings/control/{key}");
                        control != first
                            && rect == clip
                            && rect.y >= list.y - 0.5
                            && rect.bottom() <= edge + 0.5
                            && shown
                                .stop_of(&UiKey::new(control.as_str()))
                                .is_some_and(|index| shown.shows_for_keyboard(index, view.start))
                    })
                    .collect::<Vec<_>>();
                let Some((second, _, _, on)) = whole.last().cloned() else {
                    continue;
                };
                // The keyboard shows a control's whole stop, its words too
                // (`Reveal::Keep`), so the control it moves to here is the
                // last the nudged screen shows whole with its row and the
                // lines under it — whole as the keyboard reads it: all it
                // draws, its name, control and lines, its row's padding
                // over them cut or not. (A press's 3pt nudge cut that much
                // of Music effects' row over its name and switch at Sound's
                // 584×376 in Cantarell, which the keyboard's move to it
                // rightly left as it was.)
                let stop_whole = |key: &str| {
                    let drawn_top = after
                        .paint
                        .iter()
                        .filter(|node| {
                            let at = node.key.as_str();
                            at == format!("settings/label/{key}")
                                || at == format!("settings/control/{key}")
                                || at == format!("settings/detail/{key}")
                        })
                        .map(|node| node.rect.y)
                        .fold(f32::INFINITY, f32::min);
                    after
                        .paint
                        .iter()
                        .filter(|node| {
                            let at = node.key.as_str();
                            at == format!("settings/detail/{key}")
                                || at == format!("settings/row-detail/{key}")
                                || (at.starts_with("settings/row/")
                                    && at.ends_with(&format!("/{key}")))
                        })
                        .all(|node| {
                            node.rect == node.clip
                                || ((node.clip.bottom() - node.rect.bottom()).abs() < 0.5
                                    && node.clip.y <= drawn_top + 0.5)
                        })
                };
                let keyboard = whole
                    .iter()
                    .rev()
                    .find(|(key, ..)| stop_whole(key))
                    .map(|(key, ..)| format!("settings/control/{key}"));
                let context =
                    format!("{route:?} {width}×{height} last={last} {first} then {second}");
                let second = format!("settings/control/{second}");
                // Where each is drawn: before the first press, the switch the
                // room's edge cut was the screen's peek, a picture.
                let place = |compiled: &crate::native_ui::CompiledUi| {
                    let drawn = |key: &str| {
                        compiled
                            .paint
                            .iter()
                            .find(|node| node.key.as_str() == key)
                            .map(|node| node.rect)
                            .unwrap_or_else(|| panic!("{context}: {key} drawn"))
                    };
                    (drawn(&first), drawn(&second))
                };
                // A press on another switch the nudged screen shows whole, the
                // keyboard moved to it by the pointer's press first.
                lab.state_mut().common.last_focus = Some(UiKey::new(second.as_str()));
                lab.commit(
                    &format!(
                        "settings/set/{}",
                        second.trim_start_matches("settings/control/")
                    ),
                    Some(SemanticInput::Bool(!on)),
                );
                // (What it turned may retell the rows around it, so only the
                // pressed switch is held to its place.)
                let pressed = lab.compile(width, height);
                assert_eq!(
                    rect_of(&pressed, &second),
                    rect_of(&after, &second),
                    "{context}: the second press moved the list"
                );
                assert_eq!(
                    pressed
                        .semantic(&UiKey::new(second.as_str()))
                        .map(|node| node.value.clone()),
                    Some(crate::native_ui::SemanticValue::Bool(!on)),
                    "{context}: the second switch turned"
                );
                // The keyboard moving to a control it shows whole, words too,
                // moves nothing. Where the nudged screen shows no control's
                // whole stop (the edge cuts the words under the last whole
                // switch), the keyboard's move to that switch shows its whole
                // stop, as the keyboard always does.
                let mut lab = Lab::new("");
                let (before, after, ..) = nudged(&mut lab).expect("nudged as before");
                if let Some(keyboard) = &keyboard {
                    dispatch(
                        &mut lab,
                        AppEvent::FocusChanged(Some(UiKey::new(keyboard.as_str()))),
                    );
                    assert_eq!(
                        place(&lab.compile(width, height)),
                        place(&after),
                        "{context}: the keyboard's move to {keyboard} moved the list"
                    );
                    held[usize::from(last)] += 1;
                } else {
                    dispatch(
                        &mut lab,
                        AppEvent::FocusChanged(Some(UiKey::new(second.as_str()))),
                    );
                    let revealed = lab.compile(width, height);
                    let stop = revealed
                        .paint
                        .iter()
                        .filter(|node| {
                            let at = node.key.as_str();
                            let key = second.trim_start_matches("settings/control/");
                            at == second
                                || at == format!("settings/detail/{key}")
                                || at == format!("settings/row-detail/{key}")
                                || (at.starts_with("settings/row/")
                                    && at.ends_with(&format!("/{key}")))
                        })
                        .collect::<Vec<_>>();
                    assert!(
                        stop.len() > 1 && stop.iter().all(|node| node.rect == node.clip),
                        "{context}: the keyboard's move to {second} shows its whole stop: {:?}",
                        stop.iter()
                            .map(|node| (node.key.as_str(), node.rect, node.clip))
                            .collect::<Vec<_>>()
                    );
                    revealed_whole += 1;
                }
                // A scroll past the end it is at puts the list back on its
                // stops: the wheel up at its top shows the top; down at its
                // end, the end — as it sits with the bar the press brought up,
                // its last card whole in the room the bar leaves.
                dispatch(&mut lab, AppEvent::ScrollLines(if last { 1 } else { -1 }));
                let wheeled = lab.compile(width, height);
                if last {
                    let (plan, shown) = lab.state().scroll_shown().expect("a list");
                    assert!(
                        plan.seat().is_none() && shown.end == plan.len(),
                        "{context}: the wheel kept the nudge ({:?}, shows {shown:?})",
                        plan.seat()
                    );
                } else {
                    assert_eq!(
                        place(&wheeled),
                        place(&before),
                        "{context}: the wheel kept the nudge"
                    );
                }
                checked[usize::from(last)] += 1;
            }
        }
    }
    assert!(
        checked.iter().all(|count| *count > 0),
        "some screens nudge a cut switch at each edge: {checked:?}"
    );
    assert!(
        held.iter().all(|count| *count > 0),
        "the keyboard held some nudge at each edge: {held:?} ({revealed_whole} showed a cut stop)"
    );
}

/// A STYLE CHOSEN STAYS UNDER THE POINTER: on the Cursor Styles landing and
/// the screen after it, every style row a person can press, saved or not,
/// whole in the list's room keeps its place when pressed — the bar the
/// choice brings up takes only the strip under that room, which holds a
/// picture, never a row to press — and one the room's edge cuts comes whole
/// into the room; the bar never covers either (cs-32, cs-35: a row drawn whole in that strip
/// was live, and pressing it scrolled the list a stop under the pointer).
#[test]
fn a_style_chosen_from_the_list_stays_under_the_pointer_when_the_bar_comes() {
    for (width, height) in [
        (738.0, 418.0),
        (1_074.0, 684.0),
        (1_193.0, 516.0),
        (1_564.0, 854.0),
    ] {
        for saved in ["off", prefs::DEFAULT_CURSOR_TRAIL_STYLE] {
            for screen in [0, 1] {
                let config = format!("cursor_trail_style = \"{saved}\"\n");
                let landing = |lab: &mut Lab| {
                    lab.state_mut().navigate(SettingsRoute::CursorStyles);
                    let compiled = lab.compile(width, height);
                    if screen == 0 {
                        return compiled;
                    }
                    dispatch(lab, AppEvent::ScrollPage(1));
                    lab.compile(width, height)
                };
                let mut lab = Lab::new(&config);
                let before = landing(&mut lab);
                let (plan, _) = lab.state().scroll_shown().expect("a list");
                let (list, room) = compiled_list(&before, &plan).expect("the list");
                let whole = |rect: LogicalRect| {
                    rect.y >= list.y - 0.5 && rect.bottom() <= list.y + room + 0.5
                };
                let rows: Vec<(String, LogicalRect, LogicalRect)> = before
                    .semantics
                    .iter()
                    .filter(|node| {
                        node.key.as_str().starts_with("settings/cursor-style/")
                            && node.action.is_some()
                    })
                    .filter_map(|node| {
                        let paint = before.paint.iter().find(|paint| paint.key == node.key)?;
                        Some((node.key.as_str().to_string(), paint.rect, paint.clip))
                    })
                    .collect();
                assert!(!rows.is_empty(), "{width}×{height}: a style row to press");
                for (key, rect, _) in rows {
                    let mut lab = Lab::new(&config);
                    let _ = landing(&mut lab);
                    let context = format!("{width}×{height} {saved} screen {screen}: {key}");
                    // A pointer press moves the keyboard to the row first.
                    lab.state_mut().common.last_focus = Some(UiKey::new(key.as_str()));
                    let _ = lab.press(&key);
                    let after = lab.compile(width, height);
                    let now = after
                        .paint
                        .iter()
                        .find(|node| node.key.as_str() == key)
                        .unwrap_or_else(|| panic!("{context}: the row stays drawn"));
                    if whole(rect) {
                        assert_eq!(now.rect, rect, "{context}: the press moved it");
                    } else {
                        let (plan, _) = lab.state().scroll_shown().expect("a list");
                        let (list, room) = compiled_list(&after, &plan).expect("the list");
                        assert!(
                            now.rect.y >= list.y - 0.5 && now.rect.bottom() <= list.y + room + 0.5,
                            "{context}: the press brings {:?} whole into the room",
                            now.rect
                        );
                    }
                    if let Some(bar) = after
                        .paint
                        .iter()
                        .find(|node| node.key.as_str() == "settings/feedback")
                    {
                        assert!(
                            now.rect.bottom() <= bar.rect.y + 0.5,
                            "{context}: {:?} under the bar {:?}",
                            now.rect,
                            bar.rect
                        );
                    }
                }
            }
        }
    }
}

/// A CHANGE MOVES NOTHING ON A COMPACT PAGE (THE COMPACT STATUS RULE): at
/// 80×24, 60×20 and the shipped 102×29 at 1× text, and at 568×320 and
/// 102×29 at 1.5×, a change's status and Undo come where the page kept room
/// for them — in the compact toolbar at its own height where stacking them
/// would leave the page under its floor, else in the bar under the list, in
/// the room the list kept — so the toolbar, the page's heading, its preview,
/// its list's place, and every control the list showed over the changed row
/// keep their rects (the changed row's own tags are a row's anatomy, not the
/// bar's); its list's room too, but for what a status whose words take more
/// than that room (read whole, on their lines) takes from its bottom. A docked
/// status gives Search back, and the change stays undoable. NEGATIVE
/// CONTROL: under the fixed 480pt cutoff the 60×20 dock stacked Undo under
/// the words and the toolbar grew by 60pt; under the 220pt dock floor 80×24
/// neither docked the bar nor kept its room, and the list lost 44pt.
#[test]
fn a_change_moves_nothing_at_any_compact_size() {
    at_each_text_scale(&[1.0, 1.5], "a change moves nothing", |scale| {
        let sizes: &[(f32, f32)] = if scale > 1.0 {
            &[(568.0, 320.0), (738.0, 418.0)]
        } else {
            &[(584.0, 348.0), (444.0, 292.0), (738.0, 418.0)]
        };
        let cases = [
            (
                SettingsRoute::KeyboardInput,
                prefs::EDIT_CONFIRM_MULTILINE_PASTE,
                SemanticInput::Bool(false),
            ),
            (
                SettingsRoute::Security,
                prefs::EDIT_ALLOW_WINDOW_OPS,
                SemanticInput::Bool(true),
            ),
            (
                SettingsRoute::TextFonts,
                prefs::EDIT_FONT_WEIGHT,
                SemanticInput::Number(500.0),
            ),
        ];
        for &(width, height) in sizes {
            for (route, key, value) in &cases {
                let context = format!("{route:?} {width}×{height} at {scale}×");
                let mut lab = Lab::new("");
                lab.state_mut().navigate(*route);
                let control = UiKey::new(format!("settings/control/{key}"));
                lab.state_mut().common.last_focus = Some(control.clone());
                let before = lab.compile(width, height);
                let room = lab.state().scroll_plan().map(|plan| plan.room());
                let strip = lab
                    .state()
                    .scroll_plan()
                    .map_or(0.0, |plan| plan.overhang());
                // Whether the list fitted whole in its room and the strip the
                // page keeps for a bar (`scroll::seat_room`: only words and
                // padding in the strip), and so took both.
                let whole_before = lab
                    .state()
                    .scroll_plan()
                    .is_some_and(|plan| plan.limit() == 0 && plan.view(0).end == plan.len());
                let list = rect_of(&before, "settings/list");
                // The page's own anchors. A title the compact toolbar carries gives
                // its place to a status the toolbar docks, as Search does; the
                // page under the toolbar keeps its place.
                let fixed = |compiled: &crate::native_ui::CompiledUi| {
                    let toolbar = compiled
                        .semantic(&UiKey::new("settings/compact-toolbar"))
                        .map(|node| node.rect);
                    compiled
                        .semantics
                        .iter()
                        .filter(|node| {
                            let key = node.key.as_str();
                            let in_toolbar = toolbar.is_some_and(|toolbar| {
                                key != "settings/compact-toolbar"
                                    && node.rect.bottom() <= toolbar.bottom() + 0.5
                            });
                            !in_toolbar
                                && (key == "settings/compact-toolbar"
                                    || key.starts_with("settings/page-heading/")
                                    || key.starts_with("settings/preview/"))
                        })
                        .map(|node| (node.key.clone(), node.rect))
                        .collect::<Vec<_>>()
                };
                let shown = |compiled: &crate::native_ui::CompiledUi| {
                    compiled
                        .semantics
                        .iter()
                        .filter(|node| {
                            !node.offscreen
                                && node.action.is_some()
                                && node.rect.x >= list.x - 0.5
                                && node.rect.right() <= list.right() + 0.5
                                && node.rect.y >= list.y - 0.5
                                && node.rect.bottom() <= list.bottom() + 0.5
                        })
                        .map(|node| (node.key.clone(), node.rect))
                        .collect::<Vec<_>>()
                };
                let (fixed_before, shown_before) = (fixed(&before), shown(&before));
                assert!(
                    shown_before.iter().any(|(key, _)| *key == control),
                    "{context}: the control is on the screen"
                );
                lab.commit(&format!("settings/set/{key}"), Some(value.clone()));
                let after = lab.compile(width, height);
                assert!(
                    after.semantic(&UiKey::new("settings/undo")).is_some(),
                    "{context}: the change's status offers Undo"
                );
                assert_eq!(fixed_before, fixed(&after), "{context}: the page moved");
                let room_after = lab.state().scroll_plan().map(|plan| plan.room());
                if room_after != room {
                    // A list that took the strip as well gives the bar that
                    // comes exactly the strip, and nothing it showed moves.
                    // A status whose words take more than the room the page
                    // kept for one (two lines in the dock's row at 60x20, so
                    // it stands under the page) takes the rest from the
                    // page's bottom inset, down to the leanest, then from the
                    // list's bottom, and nothing over it moves.
                    let bar = rect_of(&after, "settings/feedback").height;
                    let lost = room.zip(room_after).map(|(before, after)| before - after);
                    let inset = page_chrome_insets(
                        lab.state(),
                        LogicalRect::new(0.0, 0.0, width, height),
                        SettingsWidth::Compact,
                        FORM_MEASURE,
                    )
                    .bottom;
                    let inset_taken = (bar - strip)
                        .max(0.0)
                        .min((inset - LEANEST_PAGE_INSET).max(0.0));
                    assert!(
                        lost.is_some_and(|lost| {
                            (whole_before && (lost - bar).abs() < 0.5)
                                || (bar > strip + 0.5
                                    && (lost - (bar - strip - inset_taken)).abs() < 0.5)
                        }),
                        "{context}: the list's room changed: {room:?} to {room_after:?} \
                         (bar {bar}, strip {strip})"
                    );
                }
                assert_eq!(rect_of(&after, "settings/list").y, list.y, "{context}");
                // The rows over the changed one: its own row may take a line
                // for the tag the change gives it, and move what is under it.
                let changed_top = shown_before
                    .iter()
                    .find(|(key, _)| *key == control)
                    .map_or(f32::INFINITY, |(_, rect)| rect.y);
                let shown_after = shown(&after);
                for (key, rect) in shown_before
                    .iter()
                    .filter(|(_, rect)| rect.bottom() <= changed_top)
                {
                    assert!(
                        shown_after.contains(&(key.clone(), *rect)),
                        "{context}: {key:?} {rect:?} moved or went"
                    );
                }
                if after
                    .semantic(&UiKey::new("settings/status-dock"))
                    .is_some()
                {
                    assert!(
                        after
                            .semantic(&UiKey::new("settings/status-dock/search"))
                            .is_some(),
                        "{context}: the dock gives Search back"
                    );
                    let _ = lab.invoke("settings/status-dock/search", None);
                    let searching = lab.compile(width, height);
                    assert!(
                        searching.semantic(&UiKey::new("settings/search")).is_some()
                            && lab.state().common.last_focus.as_ref().map(UiKey::as_str)
                                == Some("settings/search"),
                        "{context}: Search is back and has the keyboard"
                    );
                    assert!(
                        lab.state().undo_offer().is_some(),
                        "{context}: the change stays undoable (⌘Z)"
                    );
                    assert!(
                        searching
                            .semantic(&UiKey::new("settings/status-dock"))
                            .is_none()
                            && searching
                                .semantic(&UiKey::new("settings/feedback"))
                                .is_none(),
                        "{context}: the status is set aside, docked nowhere and stacked nowhere"
                    );
                    assert_eq!(
                        fixed_before,
                        fixed(&searching),
                        "{context}: Search coming back moved the page"
                    );
                }
            }
        }
    });
}

/// A CHANGE FROM THE KEYBOARD MOVES NOTHING ON THE FIRST SCREEN EITHER: at
/// 80x16, Follow system turned on with Space grew the theme card's rows; the
/// page, choosing its chrome again, gave the bar's room to its list, the
/// preview went into the list, and the switch the keyboard was on jumped
/// 78pt up. The chrome, and where the preview stands, are kept until the
/// list scrolls, as for a press.
#[test]
fn a_change_from_the_keyboard_moves_nothing_on_the_first_screen() {
    at_each_text_scale(&[1.0], "keyboard change", |_| {
        let (width, height) = (584.0, 238.0);
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Appearance);
        let key = UiKey::new("settings/theme-mode");
        let before = lab.compile(width, height);
        lab.dispatch(AppEvent::FocusChanged(Some(key.clone())));
        let _ = lab.compile(width, height);
        lab.commit("settings/theme-mode", None);
        let after = lab.compile(width, height);
        for key in [key.as_str(), "settings/preview/appearance"] {
            assert_eq!(
                after.semantic(&UiKey::new(key)).map(|node| node.rect),
                before.semantic(&UiKey::new(key)).map(|node| node.rect),
                "{key} stays where it was"
            );
        }
    });
}

/// A STATUS SEARCH SET ASIDE NEVER HIDES A LATER ONE: at 80×24, where a
/// change's status docks in Search's place, pressing the dock's Search sets
/// that status aside; the next change's status shows, and so does the one
/// after it, though its words are those of the status set aside (a switch
/// turned off, on, and off again), with its Undo. NEGATIVE CONTROL: the
/// status set aside was matched by its words alone, so the third change
/// showed no status and no Undo at all.
#[test]
fn a_status_search_set_aside_never_hides_a_later_one() {
    let (width, height) = (584.0, 348.0);
    let key = prefs::EDIT_CONFIRM_MULTILINE_PASTE;
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::KeyboardInput);
    let _ = lab.compile(width, height);
    let status = |lab: &mut Lab| {
        let compiled = lab.compile(width, height);
        let words = compiled
            .semantic(&UiKey::new("settings/status"))
            .map(|node| node.label.clone());
        let undo = compiled.semantic(&UiKey::new("settings/undo")).is_some();
        (words, undo)
    };
    lab.commit(
        &format!("settings/set/{key}"),
        Some(SemanticInput::Bool(false)),
    );
    let (first, undo) = status(&mut lab);
    assert!(
        first.is_some() && undo,
        "the change's status docks: {first:?}"
    );
    assert!(
        lab.compile(width, height)
            .semantic(&UiKey::new("settings/status-dock/search"))
            .is_some(),
        "the dock offers Search"
    );
    let _ = lab.invoke("settings/status-dock/search", None);
    assert_eq!(status(&mut lab), (None, false), "the status is set aside");
    for (step, on) in [(2, true), (3, false)] {
        lab.commit(
            &format!("settings/set/{key}"),
            Some(SemanticInput::Bool(on)),
        );
        let (words, undo) = status(&mut lab);
        assert!(
            words.is_some() && undo,
            "change {step}: its status and Undo show: {words:?}"
        );
        if step == 3 {
            assert_eq!(words, first, "the words of the status set aside");
        }
    }
}

/// A BAR BUTTON KEEPS ITS WORDS AND SAYS THEM: in every bar that offers
/// buttons — one draft and two, on a page and on Modified, the discard
/// question for one and for all, the Reset All question — at 1×, 1.5× and
/// 2× text, from a phone's 286.5pt to a wide window, each button paints its
/// own words whole (the layout gives — side by side, equal shares, one under
/// another — never the words), its accessible name holds those words, and
/// it is drawn whole inside the window, clear of the status and the other
/// buttons. NEGATIVE CONTROL: the short-words fallback painted "View",
/// "Drop", "Keep" and "Reset", none of them in their buttons' names.
#[test]
fn a_bar_button_keeps_its_words_and_says_them() {
    const BUTTONS: [&str; 5] = [
        "settings/drafts/review",
        "settings/drafts/discard-all",
        "settings/drafts/discard-cancel",
        "settings/reset-all-cancel",
        "settings/reset-all-confirm",
    ];
    at_each_text_scale(&[1.0, 1.5, 2.0], "bar buttons keep their words", |scale| {
        for variant in [
            "one",
            "two",
            "modified",
            "confirm-one",
            "confirm-two",
            "reset",
        ] {
            for (width, height) in [
                (286.5, 558.0),
                (320.0, 568.0),
                (444.0, 292.0),
                (584.0, 348.0),
                (738.0, 418.0),
                (1_074.0, 684.0),
            ] {
                let context = format!("{variant} at {width}×{height} {scale}×");
                let mut lab = Lab::new("theme = \"Nord\"\ncursor_blink = false\n");
                lab.state_mut().navigate(if variant == "modified" {
                    SettingsRoute::Modified
                } else {
                    SettingsRoute::Terminal
                });
                let mut drafts = vec![prefs::EDIT_SCROLLBACK];
                if variant.ends_with("two") {
                    drafts.push(prefs::EDIT_SEARCH_HISTORY_LINES);
                }
                if variant == "reset" {
                    let _ = lab.invoke("settings/reset-all", None);
                    assert!(lab.state().reset_all_confirmation, "{context}");
                } else {
                    for key in drafts {
                        lab.state_mut().field_inputs.insert(
                            key.to_string(),
                            crate::native_text_input::TextInputState::new("50".to_string()),
                        );
                    }
                    lab.state_mut().draft_discard_confirmation = variant.starts_with("confirm");
                }
                let compiled = lab.compile(width, height);
                let viewport = LogicalRect::new(0.0, 0.0, width, height);
                let mut painted = Vec::new();
                for node in &compiled.paint {
                    let UiContent::Button(control) = &node.content else {
                        continue;
                    };
                    if !BUTTONS.contains(&node.key.as_str()) {
                        continue;
                    }
                    let words = control
                        .spec
                        .visual_label
                        .clone()
                        .unwrap_or_else(|| control.spec.label.clone());
                    assert!(
                        !words.contains('\u{2026}') && !words.is_empty(),
                        "{context}: {:?} paints {words:?}",
                        node.key
                    );
                    assert!(
                        control
                            .spec
                            .label
                            .to_lowercase()
                            .contains(&words.to_lowercase()),
                        "{context}: {:?} paints {words:?}, not in its name {:?}",
                        node.key,
                        control.spec.label
                    );
                    assert_eq!(node.clip, node.rect, "{context}: {:?} is cut", node.key);
                    assert!(
                        node.rect.x >= viewport.x - 0.01
                            && node.rect.y >= viewport.y - 0.01
                            && node.rect.right() <= viewport.right() + 0.01
                            && node.rect.bottom() <= viewport.bottom() + 0.01,
                        "{context}: {:?} {:?} leaves the window",
                        node.key,
                        node.rect
                    );
                    let audit = compiled.text_fit_audit_line(&node.key).unwrap_or_default();
                    assert!(
                        !audit.contains("overflow=true"),
                        "{context}: {:?}'s words overflow it: {audit}",
                        node.key
                    );
                    painted.push(node.rect);
                }
                // Modified offers Discard alone: it is where Review leads.
                let offered = if variant == "modified" { 1 } else { 2 };
                assert_eq!(
                    painted.len(),
                    offered,
                    "{context}: the bar's buttons: {painted:?}"
                );
                for (index, a) in painted.iter().enumerate() {
                    for b in &painted[index + 1..] {
                        assert!(
                            a.intersect(*b)
                                .is_none_or(|both| both.width < 0.5 || both.height < 0.5),
                            "{context}: {a:?} and {b:?} overlap"
                        );
                    }
                }
            }
        }
    });
}

/// THE RESET ALL QUESTION IS NEVER CUT: in compact windows (80x24, 60x12
/// and a 22-column window, at 1.5x and 2x text) it stands whole over the
/// page where it can, and where the window is too short for that it takes
/// the page's place, Cancel and Reset All whole in the window under it;
/// either way the keyboard is on Cancel, nothing docks it in the toolbar's
/// one line, and every line of its words is painted whole on some screen
/// (Page Down walks them), together the whole question. Escape gives the
/// page's list back where it was. NEGATIVE CONTROL: where it takes the page,
/// the dock it took before cut its words ("Reset 9 settings? It a…").
#[test]
fn the_reset_all_question_is_never_cut() {
    let source = "theme = \"Nord\"\ncursor_blink = false\nfont_px = 18\n\
                  cursor_style = \"bar\"\ncopy_on_select = true\nbold_is_bright = true\n\
                  [update]\nenabled = false\nauto_apply = false\n[packages]\nenabled = false\n";
    let taken = std::sync::atomic::AtomicUsize::new(0);
    at_each_text_scale(&[1.5, 2.0], "the Reset All question", |scale| {
        for (width, height) in [(584.0, 348.0), (684.0, 264.0), (266.0, 516.0)] {
            let context = format!("{width}x{height} at {scale}x");
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            let mut lab = Lab::new(source);
            lab.state_mut().navigate(SettingsRoute::Modified);
            let _ = lab.compile(width, height);
            // The page's list, scrolled on, to come back to.
            lab.dispatch(AppEvent::ScrollPage(1));
            let _ = lab.compile(width, height);
            let scrolled = lab.state().page_scroll;
            assert!(lab.invoke("settings/reset-all", None).is_none());
            let words = format!(
                "{} Keys only in aterm.toml stay.",
                reset_all_question_line(lab.state())
            );
            let takes_page = reset_all_question_takes_page(lab.state(), viewport);
            if takes_page {
                taken.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let docked = crate::native_ui::fit_native_status_label(
                &reset_all_question_line(lab.state()),
                (compact_dock_layout(
                    lab.state(),
                    viewport,
                    &DockActions::of(lab.state()),
                    compact_dock_watcher(lab.state()),
                    lab.state().plain_status(),
                    dock_plain_words(lab.state()),
                )
                .words_width
                    - 10.0)
                    .max(0.0),
            );
            assert!(
                !takes_page || docked.ends_with('\u{2026}'),
                "{context}: where it takes the page, the dock cut it: {docked:?}"
            );

            let mut painted: Vec<(String, String)> = Vec::new();
            for screen in 0..12 {
                let compiled = lab.compile(width, height);
                let screen_context = format!("{context}, screen {screen}");
                assert!(
                    compiled
                        .semantic(&UiKey::new("settings/status-dock"))
                        .is_none(),
                    "{screen_context}: the question is docked"
                );
                assert_eq!(
                    compiled
                        .semantic(&UiKey::new("settings/page/reset-all"))
                        .is_some(),
                    takes_page,
                    "{screen_context}: the question takes the page where it cannot stand over it"
                );
                assert_eq!(
                    compiled
                        .semantic(&UiKey::new("settings/status"))
                        .map(|node| node.label.clone()),
                    Some(reset_all_scope(lab.state())),
                    "{screen_context}: the question says all of itself"
                );
                assert_eq!(
                    lab.state().common.last_focus.as_ref().map(UiKey::as_str),
                    Some("settings/reset-all-cancel"),
                    "{screen_context}: the keyboard is on Cancel"
                );
                let words_bottom = if takes_page {
                    rect_of(&compiled, "settings/reset-all/question").bottom()
                } else {
                    rect_of(&compiled, "settings/status").bottom()
                };
                for key in ["settings/reset-all-cancel", "settings/reset-all-confirm"] {
                    let button = rect_of(&compiled, key);
                    assert!(
                        button.y >= words_bottom - 0.01 && button.bottom() <= height + 0.01,
                        "{screen_context}: {key} {button:?} is whole, under the words ({words_bottom})"
                    );
                }
                for node in compiled
                    .paint
                    .iter()
                    .filter(|node| node.key.as_str().starts_with("settings/status/"))
                {
                    let UiContent::Text(spec) = &node.content else {
                        continue;
                    };
                    assert!(
                        !spec.text.contains('\u{2026}'),
                        "{screen_context}: {:?} is cut: {:?}",
                        node.key,
                        spec.text
                    );
                    if node.clip == node.rect
                        && !painted.iter().any(|(key, _)| key == node.key.as_str())
                    {
                        painted.push((node.key.as_str().to_string(), spec.text.clone()));
                    }
                }
                let audit = compiled.paint_audit_lines().remove(0);
                assert!(
                    audit.contains(" overflow=0 ") && audit.contains(" clipped=0 "),
                    "{screen_context}: {audit}"
                );
                lab.dispatch(AppEvent::ScrollPage(1));
            }
            let order = |key: &str| {
                let part = if key.starts_with("settings/status/visual") {
                    0
                } else if key.starts_with("settings/status/reach") {
                    1
                } else {
                    2
                };
                let line = key
                    .rsplit_once("/line-")
                    .and_then(|(_, line)| line.parse::<usize>().ok())
                    .unwrap_or(0);
                (part, line)
            };
            painted.sort_by_key(|(key, _)| order(key));
            let read = painted
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(read, words, "{context}: every line is read whole");

            lab.dispatch(AppEvent::TextInput(TextInputEvent::Cancel));
            assert!(!lab.state().reset_all_confirmation, "{context}");
            assert_eq!(
                lab.state().page_scroll,
                scrolled,
                "{context}: the page's list comes back where it was"
            );
        }
    });
    assert!(
        taken.into_inner() >= 3,
        "the question took the page in the windows too short for it"
    );
}

/// THE RESET ALL QUESTION TAKES THE PAGE WHERE ITS BAR IS TALLER THAN THE
/// WINDOW, AT ANY WIDTH (#92): at 220x12 at 2× text its bar under the page
/// was 192pt in a 180pt window, Cancel and Reset All cut by the window's
/// bottom (clipped, their words overflowing). It takes the page's place
/// beside the sidebar instead: its words on lines of a list of their own,
/// each whole on some screen, Cancel and Reset All whole under it, the
/// page audit clean on every screen.
#[test]
fn the_reset_all_question_takes_the_page_where_its_bar_is_taller_than_the_window() {
    at_each_text_scale(&[2.0], "wide question", |_| {
        let (width, height) = (1_564.0, 180.0);
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Security);
        let _ = lab.compile(width, height);
        // A change Reset All turns back, and says it does.
        lab.commit("settings/set/allow_osc52_query", None);
        let _ = lab.compile(width, height);
        assert!(lab.invoke("settings/reset-all", None).is_none());
        assert!(reset_all_question_takes_page(
            lab.state(),
            LogicalRect::new(0.0, 0.0, width, height)
        ));
        let mut lines = std::collections::BTreeSet::new();
        let mut before = None;
        for _ in 0..20 {
            let compiled = lab.compile(width, height);
            let failures = compiled.fit_failures();
            assert!(failures.is_empty(), "{failures:#?}");
            for key in ["settings/reset-all-cancel", "settings/reset-all-confirm"] {
                let node = compiled
                    .paint
                    .iter()
                    .find(|node| node.key.as_str() == key)
                    .unwrap_or_else(|| panic!("{key} is painted"));
                assert_eq!(node.clip, node.rect, "{key} is whole");
            }
            lines.extend(
                compiled
                    .paint
                    .iter()
                    .filter(|node| {
                        node.key.as_str().starts_with("settings/status/")
                            && matches!(node.content, UiContent::Text(_))
                            && node.clip == node.rect
                    })
                    .map(|node| node.key.as_str().to_string()),
            );
            let top = lab.state().page_scroll;
            if before == Some(top) {
                break;
            }
            before = Some(top);
            lab.dispatch(AppEvent::ScrollPage(1));
        }
        // Its question, what else it reaches, and what it keeps.
        for part in [
            "settings/status/visual",
            "settings/status/reach",
            "settings/status/kept",
        ] {
            assert!(
                lines.iter().any(|line| line.starts_with(part)),
                "{part} is painted whole: {lines:?}"
            );
        }
    });
}

/// RESET ALL NAMES WHAT IT TURNS BACK ON: where the reset clears keys that
/// act beyond the window — aterm's updates, its tool downloads, the
/// harness's approvals — set away from their defaults, its question names
/// each with the default it goes back to and says the downloads may follow,
/// on its bar (wrapped whole, standing over the waiting page even at 80×24,
/// never cut to the dock's one line) and in its accessible words, and the
/// confirm button's name says it too; nothing is written before the
/// confirm. A reset of a theme and a cursor blink says no
/// more than its count. NEGATIVE CONTROL: the question was built from the
/// count alone ("Reset 6 settings? Keys only in aterm.toml stay.").
#[test]
fn reset_all_names_what_it_turns_back_on() {
    let source = "theme = \"Nord\"\n[update]\nenabled = false\nauto_apply = false\n\
                  [packages]\nenabled = false\n[harness]\napprove = \"none\"\n";
    let mut lab = Lab::new(source);
    lab.state_mut().navigate(SettingsRoute::Modified);
    let written = lab.invoke("settings/reset-all", None);
    assert!(written.is_none(), "the question writes nothing");
    assert!(lab.state().reset_all_confirmation);
    for (width, height) in [(1_080.0, 720.0), (738.0, 418.0), (584.0, 348.0)] {
        let compiled = lab.compile(width, height);
        let context = format!("{width}×{height}");
        let status = compiled
            .semantic(&UiKey::new("settings/status"))
            .unwrap_or_else(|| panic!("{context}: the question"));
        let confirm = compiled
            .semantic(&UiKey::new("settings/reset-all-confirm"))
            .unwrap_or_else(|| panic!("{context}: Reset All"));
        for named in [
            "Check for updates automatically to On",
            "Install updates automatically to On",
            "Keep ALab tools up to date to On",
            "Approve permission boxes to All",
            "several GB",
        ] {
            assert!(
                status.label.contains(named),
                "{context}: the question names {named:?}: {}",
                status.label
            );
            assert!(
                confirm.label.contains(named),
                "{context}: Reset All's name says {named:?}: {}",
                confirm.label
            );
        }
        // The question stands whole over the waiting page, never cut to the
        // dock's one line: the reach wrapped in lines of its own.
        assert!(
            compiled
                .semantic(&UiKey::new("settings/status-dock"))
                .is_none(),
            "{context}: the question is docked, its reach cut"
        );
        let reach: String = compiled
            .paint
            .iter()
            .filter(|node| node.key.as_str().starts_with("settings/status/reach"))
            .filter_map(|node| match &node.content {
                UiContent::Text(spec) => Some(spec.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            reach.starts_with("It also sets ")
                && reach.contains("Check for updates automatically to On")
                && reach.ends_with("several GB of tools at the next check.")
                && !reach.contains('\u{2026}'),
            "{context}: the reach is painted whole: {reach:?}"
        );
        for line in compiled
            .paint
            .iter()
            .filter(|node| node.key.as_str().starts_with("settings/status"))
        {
            let audit = compiled.text_fit_audit_line(&line.key).unwrap_or_default();
            assert!(!audit.contains("overflow=true"), "{context}: {audit}");
            assert_eq!(line.clip, line.rect, "{context}: {:?} is cut", line.key);
        }
        for key in ["settings/reset-all-cancel", "settings/reset-all-confirm"] {
            let button = rect_of(&compiled, key);
            assert!(
                button.bottom() <= height + 0.01,
                "{context}: {key} {button:?} is in the window"
            );
        }
        // The waiting page gives way to the question without being cut by its
        // own edge: what it has no room for is not drawn (at 80×24 the
        // summary under the heading used to be cut 5pt by the page's edge),
        // and its list never runs past the room it has
        // ([`scroll::ScrollPlan::screen`]).
        let audit = compiled.paint_audit_lines().remove(0);
        assert!(
            audit.contains(" overflow=0 ") && audit.contains(" clipped=0 "),
            "{context}: {audit}\n{}",
            compiled
                .paint
                .iter()
                .filter(|node| node.clipped())
                .map(|node| format!("{:?} {:?} clip {:?}", node.key, node.rect, node.clip))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let mut plain = Lab::new("theme = \"Nord\"\ncursor_blink = false\n");
    plain.state_mut().navigate(SettingsRoute::Modified);
    let _ = plain.invoke("settings/reset-all", None);
    let compiled = plain.compile(1_080.0, 720.0);
    let status = compiled
        .semantic(&UiKey::new("settings/status"))
        .expect("the question");
    assert_eq!(
        status.label,
        "Reset 2 settings to their defaults? Lists, maps, and other keys only in aterm.toml stay."
    );
    assert!(
        !compiled
            .paint
            .iter()
            .any(|node| node.key.as_str().starts_with("settings/status/reach")),
        "a reset of the window's own looks reaches nothing beyond it"
    );
}

/// THE COLOR THEME ROW HOLDS EVERYTHING ON ITS LINE, at every window width
/// and text scale: on the screen that shows it whole (its picker live over
/// all of it), its name is whole and its picker and Reset lie inside the
/// row, none cut by an edge. (With Browse themes… beside them, the row
/// overran its 488pt at 80 columns of a 15px font: Reset cut to 56 of its
/// 72pt.)
#[test]
fn the_color_theme_row_holds_its_picker_and_reset_at_every_width() {
    const OWN: [&str; 4] = [
        "key=\"settings/control/theme\"",
        "key=\"settings/reset/theme\"",
        "key=\"settings/label/theme\"",
        "key=\"settings/reset-slot/theme\"",
    ];
    let picker = UiKey::new("settings/control/theme");
    at_each_text_scale(&[1.0, 1.25, 1.5, 2.0], "color theme row", |scale| {
        for route in [SettingsRoute::Appearance, SettingsRoute::Modified] {
            // 22 columns of the 12px cell up to a wide window, in steps
            // finer than any control.
            let mut width = 178.0;
            while width <= 1_480.0 {
                let height = 760.0;
                let service = VersionedConfigService::new("theme = \"GitHub Light\"\n".to_string())
                    .expect("fixture");
                let (mut runtime, instance, view) = setup();
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    **state = SettingsViewState::from_snapshot(&service.snapshot()).expect("state");
                    state.navigate(route);
                }
                let mut seen = false;
                each_screen(
                    &mut runtime,
                    instance,
                    view,
                    &view_cx_at(width, height),
                    |compiled, top| {
                        let context = format!(
                            "{} {width}×{height} at {scale}× screen {top}",
                            route.label()
                        );
                        let Some(row) = compiled.paint.iter().find(|node| {
                            let key = node.key.as_str();
                            key.starts_with("settings/row/") && key.ends_with("/theme")
                        }) else {
                            return;
                        };
                        let control = compiled.paint.iter().find(|node| node.key == picker);
                        let live = compiled.hits.iter().find(|hit| hit.key == picker);
                        let whole =
                            |rect: crate::native_ui::LogicalRect,
                             shown: crate::native_ui::LogicalRect| {
                                shown.y <= rect.y + 0.5 && shown.bottom() + 0.5 >= rect.bottom()
                            };
                        let (Some(control), Some(live)) = (control, live) else {
                            return;
                        };
                        if !whole(row.rect, row.clip) || !whole(control.rect, live.rect) {
                            return;
                        }
                        seen = true;
                        if let Some(painted) = painted_row_label(compiled, "theme") {
                            assert!(
                                !painted.contains('\u{2026}')
                                    && painted
                                        .split_whitespace()
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                        .starts_with("Color theme"),
                                "{context}: the row paints its whole name: {painted:?}"
                            );
                        }
                        // Beside the picker, the name keeps its floor on its
                        // line; where it cannot, the row stacks the picker
                        // under it.
                        let label = compiled
                            .paint
                            .iter()
                            .find(|node| node.key.as_str() == "settings/label/theme")
                            .expect("the row's name");
                        if control.rect.y + 0.5 < label.rect.bottom() {
                            assert!(
                                label.rect.width + 0.5 >= one_line_name_floor("Color theme"),
                                "{context}: the name beside the picker has {:?}",
                                label.rect
                            );
                        }
                        for key in ["settings/control/theme", "settings/reset/theme"] {
                            if let Some(node) =
                                compiled.paint.iter().find(|node| node.key.as_str() == key)
                            {
                                assert!(
                                    node.rect.x + 0.5 >= row.rect.x
                                        && node.rect.right() <= row.rect.right() + 0.5
                                        && node.clip.width + 0.5 >= node.rect.width,
                                    "{context}: {key} {:?} (shown {:?}) inside the row {:?}",
                                    node.rect,
                                    node.clip,
                                    row.rect
                                );
                            }
                        }
                        let cut: Vec<String> = compiled
                            .fit_failures()
                            .into_iter()
                            .filter(|line| OWN.iter().any(|key| line.contains(key)))
                            .collect();
                        assert!(cut.is_empty(), "{context}:\n{}", cut.join("\n"));
                    },
                );
                assert!(
                    seen,
                    "{} {width} at {scale}×: some screen shows the row whole",
                    route.label()
                );
                width += 26.0;
            }
        }
    });
}

/// EVERY ONE-LINE ROW LEAVES ITS NAME ITS FLOOR beside everything on its
/// line ([`one_line_name_floor`]), on every page, at every window width and
/// text scale: where the control cannot give up enough width, the row
/// stacks it under the name. (Harness' Question priority kept its three
/// segments and Reset beside a 44pt name at 760pt, broken inside the word
/// "Question"; Music effects on a phone-width Sound page at 1.25× was
/// measured without the place Reset keeps, its name 80 of its 96pt.)
#[test]
fn a_one_line_rows_name_keeps_its_floor_on_every_page() {
    at_each_text_scale(&[1.0, 1.25, 1.5, 2.0], "one-line rows", |scale| {
        let mut short = std::collections::BTreeSet::new();
        for route in SettingsRoute::ALL {
            let mut width = 300.0;
            while width <= 1_480.0 {
                let (mut runtime, instance, view) = setup();
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    state.navigate(route);
                }
                each_screen(
                    &mut runtime,
                    instance,
                    view,
                    &view_cx_at(width, 760.0),
                    |compiled, _| {
                        for label in &compiled.paint {
                            let Some(key) = label.key.as_str().strip_prefix("settings/label/")
                            else {
                                continue;
                            };
                            if key.contains('/') {
                                continue;
                            }
                            let Some(semantic) = compiled.semantic(&label.key) else {
                                continue;
                            };
                            // The name, without the tags after it.
                            let name = semantic.label.split(" \u{b7} ").next().unwrap_or_default();
                            let control = format!("settings/control/{key}");
                            let segments = format!("settings/segments/{key}");
                            let Some(control) = compiled.paint.iter().find(|node| {
                                node.key.as_str() == control || node.key.as_str() == segments
                            }) else {
                                continue;
                            };
                            let beside = control.rect.y + 0.5 < label.rect.bottom()
                                && control.rect.x > label.rect.x;
                            let floor = one_line_name_floor(name);
                            if beside && label.rect.width + 0.5 < floor {
                                short.insert(format!(
                                    "{} {width}: {key} {name:?} has {:.0} of its {floor:.0}pt",
                                    route.label(),
                                    label.rect.width
                                ));
                            }
                        }
                    },
                );
                width += 40.0;
            }
        }
        assert!(
            short.is_empty(),
            "at {scale}×:\n{}",
            short.into_iter().collect::<Vec<_>>().join("\n")
        );
    });
}

/// THE LIST KEEPS ITS PLACE BY ITS STOPS, NOT THEIR NUMBER (#62): a text
/// size change measures the list into other stops — Themes at 80×24 lists
/// its demo and strip as its first stops at 2× text, where they were pinned
/// over it at 1×; Harness splits rows into parts and lists notes that hold
/// no control — and the list kept the NUMBER of the stop it was at, so it
/// showed another part of the list. It keeps the stop itself, by what it
/// lists: the screen starts with the stop that lists what the last one
/// started with — or, where no stop lists that any more, the first stop
/// after it that is listed — at 2× and back at 1×, with the keyboard off the
/// list (on the search field, on the sidebar row that opened the page) as
/// with none, whether or not the scroll settled on the 2× screen before the
/// change back (a 2× screen past the 1× list's last kept its place too), and
/// the next frame at either size, nothing between, draws the same screen:
/// Home at 1564x180 drew its 2× screen from the stop it was at, and the next
/// frame from the stop after, where the scroll it was asked for stood. On
/// a list's last screen, which starts where the list's end puts it, the
/// stop is on the screen whole. At 80×24 and 102×29, and at the short
/// windows 60×20, 102×12, 80×16 and 220×12, where what a screen starts with
/// is often listed otherwise at the other size
/// ([`a_text_size_change_keeps_a_screen_that_starts_with_what_it_lists_otherwise`]).
#[test]
fn a_text_size_change_keeps_the_list_at_the_stop_it_was_at() {
    let set_scale = |scale: f32| {
        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: scale,
                ..crate::native_appearance::current_preferences()
            },
        );
    };
    // The list as it is, where its screen starts, and whether that screen
    // is the list's last.
    let place = |lab: &Lab| {
        let (plan, shown) = lab.state().scroll_shown().expect("a list");
        let top = lab
            .state()
            .scroll_record
            .borrow()
            .as_ref()
            .map_or(0, |record| record.top());
        let last = top >= plan.limit();
        (plan, shown, last, top)
    };
    // The stop of `to` the screen of `from` starting at `start` is kept at:
    // the one that lists what `start` did, else as far over the first stop
    // after it `to` lists as that stop was.
    let kept = |from: &scroll::ScrollPlan, start: usize, to: &scroll::ScrollPlan| {
        (start..from.len()).find_map(|stop| {
            from.identities(stop)
                .find_map(|id| to.stop_of_identity(id))
                .map(|index| (start + index).saturating_sub(stop).min(to.limit()))
        })
    };
    // Whether a screen showing `shown` keeps stop `want`: it starts with
    // it, or, the list's last screen, shows it.
    let keeps = |shown: &std::ops::Range<usize>, last: bool, want: Option<usize>| {
        want.is_none_or(|want| shown.start == want || (last && shown.contains(&want)))
    };
    let moved = std::sync::atomic::AtomicUsize::new(0);
    let checked = std::sync::atomic::AtomicUsize::new(0);
    let faults = std::sync::Mutex::new(Vec::new());
    // Each window of each page under each keyboard place is a fresh lab of
    // its own: spread over a few threads, each at its own text size.
    let places = [None, Some("settings/search"), Some("settings/nav/themes")]
        .into_iter()
        .flat_map(|focus| {
            [
                SettingsRoute::Themes,
                SettingsRoute::Appearance,
                SettingsRoute::Sound,
                SettingsRoute::Home,
                SettingsRoute::Harness,
            ]
            .into_iter()
            .flat_map(move |route| {
                [
                    (584.0, 348.0),
                    (738.0, 418.0),
                    (444.0, 292.0),
                    (738.0, 180.0),
                    (584.0, 236.0),
                    (1_564.0, 180.0),
                ]
                .map(|(width, height)| (focus, route, width, height))
            })
        })
        .collect::<Vec<_>>();
    on_workers(
        &places,
        "text size places",
        |&(focus, route, width, height)| {
            use std::sync::atomic::Ordering::Relaxed;
            set_scale(1.0);
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            let _ = lab.compile(width, height);
            let limit = lab.state().scroll_plan().map_or(0, |plan| plan.limit());
            for top in 1..=limit {
                for settled in [false, true] {
                    set_scale(1.0);
                    let mut lab = Lab::new("");
                    lab.state_mut().navigate(route);
                    lab.state_mut().common.last_focus = focus.map(UiKey::new);
                    let _ = lab.compile(width, height);
                    lab.state_mut().scroll_page_to(top);
                    lab.state_mut().page_reveal = None;
                    let _ = lab.compile(width, height);
                    let (one, shown, _, _) = place(&lab);
                    let at = shown.start;
                    let context = format!(
                        "{route:?} {width}×{height} from stop {at}, keyboard {focus:?}, \
                         settled {settled}"
                    );
                    set_scale(2.0);
                    let at_two = lab.compile(width, height);
                    if lab.compile(width, height) != at_two {
                        faults
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!(
                                "{context}: at 2× the next frame draws another screen"
                            ));
                    }
                    if settled {
                        lab.state_mut().settle_page_scroll();
                    }
                    let (two, shown_two, last, top_two) = place(&lab);
                    let want = kept(&one, at, &two);
                    if !keeps(&shown_two, last, want) {
                        faults
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!(
                                "{context}: at 2× the screen shows {shown_two:?}, not from stop \
                                 {want:?}, which lists what stop {at} at 1× did"
                            ));
                    }
                    moved.fetch_add(usize::from(shown_two.start != at), Relaxed);
                    set_scale(1.0);
                    let back = lab.compile(width, height);
                    if lab.compile(width, height) != back {
                        faults
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!(
                                "{context}: back at 1× the next frame draws another screen"
                            ));
                    }
                    let (_, shown_back, last, _) = place(&lab);
                    // A list at its top stays there.
                    let want = if top_two == 0 {
                        Some(0)
                    } else {
                        kept(&two, shown_two.start, &one)
                    };
                    if !keeps(&shown_back, last, want) {
                        faults
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!(
                                "{context}: back at 1× the screen shows {shown_back:?}, not from \
                                 stop {want:?}, which lists what stop {} at 2× did",
                                shown_two.start
                            ));
                    }
                    checked.fetch_add(1, Relaxed);
                }
            }
        },
    );
    let (checked, moved) = (checked.into_inner(), moved.into_inner());
    let faults = faults
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(checked > 100, "{checked} places");
    assert!(moved > 0, "a text size change renumbers some list's stops");
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
}

/// A SCREEN THAT STARTS WITH WHAT THE OTHER TEXT SIZE LISTS OTHERWISE KEEPS
/// ITS PLACE (the narrow-width check of #88): from a 1× screen whose first
/// stop 2× text lists otherwise, the 2× screen starts with what it started
/// with. Themes at 60x20: the Built-in caption, a stop at 1×, heads the
/// Default card at 2×, and the screen started a card up, with Linen. Home at
/// 220x12: the Playful effects title row, a stop at 1×, is its heading alone
/// at 2×, over the Trail sounds row, and the screen started a row up. Home at
/// 102x12: the cursor-trail specimen, which 2× lists no more under its
/// card's title, kept the stop's number, and the Text card showed.
/// Appearance at 220x12: the list's last screen, whose first stop the Output
/// streak row is, started a stop past it at 2×, on the row's note.
#[test]
fn a_text_size_change_keeps_a_screen_that_starts_with_what_it_lists_otherwise() {
    let set_scale = |scale: f32| {
        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: scale,
                ..crate::native_appearance::current_preferences()
            },
        );
    };
    let mut faults = Vec::new();
    for (route, (width, height), from, starts) in [
        (
            SettingsRoute::Themes,
            (444.0, 292.0),
            Some("settings/showcase/section/builtin/inset"),
            "settings/showcase/card/default",
        ),
        (
            SettingsRoute::Home,
            (1_564.0, 180.0),
            Some("settings/top/playful-effects/title-row"),
            "settings/row/80/trail_sounds",
        ),
        (
            SettingsRoute::Home,
            (738.0, 180.0),
            Some("settings/top/cursor-trail/preview"),
            "settings/top/cursor-trail/title-row",
        ),
        // The list's last screen.
        (
            SettingsRoute::Appearance,
            (1_564.0, 180.0),
            None,
            "settings/row/58/output_streak.enabled",
        ),
    ] {
        set_scale(1.0);
        let mut lab = Lab::new("");
        lab.state_mut().navigate(route);
        let _ = lab.compile(width, height);
        let plan = lab.state().scroll_plan().expect("a list");
        let top = match from {
            Some(key) => plan
                .stop_of_identity(&UiKey::new(key))
                .expect("1× lists it"),
            None => plan.len(),
        };
        lab.state_mut().scroll_page_to(top);
        lab.state_mut().page_reveal = None;
        let _ = lab.compile(width, height);
        let (one, shown) = lab.state().scroll_shown().expect("a list");
        let first = one.identities(shown.start).next().cloned();
        set_scale(2.0);
        let _ = lab.compile(width, height);
        let (two, shown_two) = lab.state().scroll_shown().expect("a list");
        let now = two.identities(shown_two.start).next().cloned();
        if now.as_ref().map(UiKey::as_str) != Some(starts) {
            faults.push(format!(
                "{route:?} {width}x{height}: from the 1× screen starting with {first:?}, the 2× \
                 screen starts with {now:?}, not {starts}"
            ));
        }
    }
    set_scale(1.0);
    assert!(faults.is_empty(), "{faults:#?}");
}

/// THE AUDIT JUDGES EVERY SCREEN OF EVERY PAGE, OVERLAPS INCLUDED (#50): on
/// every page, at the 80×24, 102×29 and 150×48 windows, at 1× and 2× text,
/// in a dark theme and a light one, every screen of its list paints no word
/// cut off, overflowing or clipped by accident, no two marks over each
/// other ([`crate::native_ui::CompiledUi::ink_overlaps`]) and no line sliced
/// by a list's top edge ([`assert_list_tops`]): Cursor & Motion's and Cursor
/// Styles' preview header once ran its state, "… · Live trail", under its
/// Pause glyph, and no audit said so.
#[test]
fn every_screen_of_every_page_paints_no_cut_or_overlapping_mark() {
    at_each_text_scale(&[1.0, 2.0], "the page audit", |scale| {
        let mut failures = Vec::new();
        let mut screens = 0;
        for (theme, source) in [
            ("Default", ""),
            (
                "GitHub Light",
                "theme = \"GitHub Light\"\nwindow_theme = \"light\"\n",
            ),
        ] {
            for route in SettingsRoute::ALL {
                for (width, height) in [(584.0, 348.0), (738.0, 418.0), (1_074.0, 684.0)] {
                    let mut lab = Lab::new(source);
                    lab.state_mut().navigate(route);
                    screens += lab.each_screen(width, height, |state, compiled, top| {
                        let room_bottom = state
                            .scroll_plan()
                            .and_then(|plan| compiled_list(compiled, &plan))
                            .map_or(height, |(list, room)| list.y + room);
                        assert_list_tops(
                            compiled,
                            room_bottom,
                            &format!("{route:?} {width}×{height} {theme} at {scale}× screen {top}"),
                        );
                        let found = compiled.fit_failures();
                        if !found.is_empty() {
                            failures.push(format!(
                                "{route:?} {width}×{height} {theme} screen {top}: {found:#?}"
                            ));
                        }
                    });
                }
            }
        }
        assert!(screens > 100, "{screens} screens");
        assert!(
            failures.is_empty(),
            "at {scale}×, {} of {screens} screens:\n{}",
            failures.len(),
            failures.join("\n")
        );
    });
}

/// Every line a status paints under `prefix` (`settings/status/visual`), in
/// order: what the status reads as.
fn status_words(compiled: &crate::native_ui::CompiledUi, prefix: &str) -> String {
    compiled
        .paint
        .iter()
        .filter(|node| {
            let key = node.key.as_str();
            key == prefix
                || key
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with("/line-"))
        })
        .filter_map(|node| match &node.content {
            UiContent::Text(spec) if node.clip.height >= node.rect.height - 0.5 => {
                Some(spec.text.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every line of a status's words under `prefix` in `lab`'s `width`x`height`
/// window, those its bar shows and those the wheel over the bar scrolls to
/// ([`status_words_node`]), in order; the words are scrolled back to their
/// top after.
fn every_status_line(lab: &mut Lab, width: f32, height: f32, prefix: &str) -> Vec<String> {
    let mut lines: std::collections::BTreeMap<usize, String> = std::collections::BTreeMap::new();
    let mut read = |compiled: &crate::native_ui::CompiledUi| {
        let before = lines.len();
        for node in &compiled.paint {
            let key = node.key.as_str();
            let index = if key == prefix {
                Some(0)
            } else {
                key.strip_prefix(prefix)
                    .and_then(|rest| rest.strip_prefix("/line-"))
                    .and_then(|index| index.parse().ok())
            };
            if let (Some(index), UiContent::Text(spec)) = (index, &node.content) {
                lines.insert(index, spec.text.clone());
            }
        }
        lines.len() > before
    };
    let mut scrolled = 0;
    read(&lab.compile(width, height));
    while scrolled < 64 {
        lab.dispatch(AppEvent::ScrollAt {
            lines: 1,
            key: UiKey::new("settings/feedback"),
        });
        scrolled += 1;
        if !read(&lab.compile(width, height)) && lab.state().status_scroll.borrow().limit <= 0.01 {
            break;
        }
        let scroll = lab.state().status_scroll.borrow();
        if scroll.offset >= scroll.limit - 0.01 {
            break;
        }
    }
    lab.dispatch(AppEvent::ScrollAt {
        lines: -scrolled,
        key: UiKey::new("settings/feedback"),
    });
    let _ = lab.compile(width, height);
    lines.into_values().collect()
}

/// A STATUS IS READ WHOLE AND NOTHING THE PERSON PRESSED MOVES (#92): on
/// every page with a switch on its first screen (Messages and Themes keep
/// statuses of their own), at 80x24, 60x20 and 22x24 and the short windows
/// 220x12, 102x12 and 80x16, 1× and 2× text, both themes, every status a
/// change brings up — a saved change and its Undo, the Redo an Undo
/// offers, a refused write, a save failure, one with a long path — paints
/// all its words, on as many lines as they take, its bar (or the toolbar it
/// docks in) as tall as they are — but where that would leave the list
/// less than the switch the person pressed, or the window fewer lines,
/// when its words scroll in the bar from their first line, every one of
/// them on a line, none cut; the page audits clean; the
/// switch the person pressed stays whole, and where it was wherever the bar
/// leaves it there (the bar takes its room from the page's bottom inset,
/// then the list's bottom), else moved by its hidden part; and the Reset
/// All question is read whole. The status line at 2× text read "Color
/// theme was not saved…", hiding why; a long one covered the switch just
/// pressed (22x24 at 2×: a saved status left the list 40pt), and a
/// one-line bar at 220x12 at 2× cut Sound's 72pt switch to 52.
#[test]
fn a_status_is_read_whole_and_nothing_pressed_moves() {
    const FAILURE: &str = "~/.config/aterm isn\u{2019}t writable \u{b7} check its permissions";
    const LONG_FAILURE: &str = "/Volumes/Shared Drive/Users//someone/dotfiles/config/aterm \
                                isn\u{2019}t writable \u{b7} check its permissions";
    at_each_text_scale(&[1.0, 2.0], "status kinds", |scale| {
        let faults = std::sync::Mutex::new(Vec::new());
        let kinds = std::sync::atomic::AtomicUsize::new(0);
        // Each window of each page in each theme, every status kind on it,
        // spread over a few threads at the rung's text size.
        let windows = ["", "theme = \"GitHub Light\"\n"]
            .into_iter()
            .flat_map(|source| {
                SettingsRoute::ALL
                    .into_iter()
                    .filter(|route| {
                        !matches!(route, SettingsRoute::Messages | SettingsRoute::Themes)
                    })
                    .flat_map(move |route| {
                        [
                            (584.0, 348.0),
                            (444.0, 292.0),
                            (178.0, 348.0),
                            (1_564.0, 180.0),
                            (738.0, 180.0),
                            (584.0, 236.0),
                        ]
                        .map(|(width, height)| (source, route, width, height))
                    })
            })
            .collect::<Vec<_>>();
        on_workers(
            &windows,
            "status kinds",
            |&(source, route, width, height)| {
                for kind in [
                    "saved",
                    "pointer",
                    "redo",
                    "refused",
                    "failure",
                    "long-failure",
                    "reset-all",
                ] {
                    let context = format!(
                        "{route:?} {kind} {width}x{height} {scale}x{}",
                        if source.is_empty() { "" } else { " light" }
                    );
                    let mut lab = Lab::new(source);
                    lab.state_mut().navigate(route);
                    let before = lab.compile(width, height);
                    // The first switch the screen shows whole — its stop too,
                    // as the keyboard that comes to it reads it (the words
                    // under it, which a keyboard landing brings into view) —
                    // as a person presses it.
                    let keyboard_whole = |key: &UiKey| {
                        lab.state().scroll_plan().is_none_or(|plan| {
                            let top = lab.state().page_scroll.min(plan.limit());
                            plan.stop_of(key)
                                .is_none_or(|stop| plan.shows_for_keyboard(stop, top))
                        })
                    };
                    let Some((pressed, action)) =
                        before.paint.iter().find_map(|node| match &node.content {
                            UiContent::Switch(control)
                                if node.clip == node.rect
                                    && before.semantic(&node.key).is_some()
                                    && (kind == "pointer" || keyboard_whole(&node.key)) =>
                            {
                                Some((node.key.clone(), control.action.as_str().to_string()))
                            }
                            _ => None,
                        })
                    else {
                        continue;
                    };
                    let was = before.semantic(&pressed).map(|node| node.rect);
                    if kind == "pointer" {
                        // As the host's pointer does: the press puts the
                        // keyboard on the control itself and draws, the
                        // release sends its action, the page draws while the
                        // write is on its way, then the result comes.
                        lab.state_mut().common.last_focus = Some(pressed.clone());
                        let _ = lab.compile(width, height);
                    } else {
                        // A click puts the keyboard on what it presses.
                        lab.dispatch(AppEvent::FocusChanged(Some(pressed.clone())));
                        let _ = lab.compile(width, height);
                    }
                    match kind {
                        "saved" | "pointer" => {
                            // As the host does: the page renders while the
                            // write is on its way ("Applying…"), then the
                            // result comes.
                            let (patch, operation) = lab
                                .invoke(&action, None)
                                .unwrap_or_else(|| panic!("{context}: {action} writes"));
                            let _ = lab.compile(width, height);
                            lab.carry(patch, operation);
                        }
                        "redo" => {
                            lab.commit(&action, None);
                            lab.undo();
                        }
                        "refused" | "failure" | "long-failure" => {
                            let Some((_, operation)) = lab.invoke(&action, None) else {
                                continue;
                            };
                            let message = match kind {
                                "refused" => "out of range",
                                "failure" => FAILURE,
                                _ => LONG_FAILURE,
                            }
                            .to_string();
                            lab.dispatch(AppEvent::ConfigPatchFinished {
                                operation,
                                outcome: ConfigPatchOutcome::Rejected { message },
                            });
                        }
                        _ => {
                            lab.commit(&action, None);
                            lab.invoke("settings/reset-all", None);
                        }
                    }
                    let after = lab.compile(width, height);
                    kinds.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let feedback = lab.state().feedback.clone().unwrap_or_default();
                    if kind != "reset-all" {
                        // Every word, in order; a word wider than the bar
                        // (a path at 22 columns) is set across lines whole.
                        let bare = |text: &str| text.split_whitespace().collect::<String>();
                        let read = status_words(&after, "settings/status/visual");
                        // No word set across two lines that a line's box
                        // holds whole (the dock beside the back button at
                        // 22 columns broke "Restore" / "d Color").
                        let (px, face) = crate::native_ui::text_paint_metrics(
                            SemanticRole::Status,
                            StyleRef::Plain,
                        );
                        let lines: Vec<(String, f32)> = after
                            .paint
                            .iter()
                            .filter(|node| {
                                node.key
                                    .as_str()
                                    .starts_with("settings/status/visual/line-")
                            })
                            .filter_map(|node| match &node.content {
                                UiContent::Text(spec) => Some((spec.text.clone(), node.rect.width)),
                                _ => None,
                            })
                            .collect();
                        // Every word on a line of the bar: those it shows,
                        // and those it scrolls to.
                        let every =
                            every_status_line(&mut lab, width, height, "settings/status/visual");
                        let all = every.join(" ");
                        if bare(&all) != bare(&feedback) || all.contains('\u{2026}') {
                            faults
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push(format!("{context}: sets {every:?} of {feedback:?}"));
                        }
                        for word in feedback.split_whitespace() {
                            // Each piece a line may break between (a path's
                            // folders, a word's hyphen) whole on a line where
                            // a line's box holds it.
                            for (_, piece) in line_pieces(word) {
                                let whole = every.len() <= 1
                                    || every.iter().any(|line| {
                                        line.split_whitespace()
                                            .any(|painted| painted.contains(piece))
                                    });
                                let fits = lines.iter().any(|(_, box_width)| {
                                    crate::tray_raster::ui_text_width_for(face, piece, px)
                                        <= *box_width - 10.0
                                });
                                if !whole && fits {
                                    faults
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                                        .push(format!(
                                            "{context}: {piece:?} of {word:?} is set across lines"
                                        ));
                                }
                            }
                        }
                        // Only where the window under the toolbar holds
                        // fewer lines than the words take — the bar filling
                        // it — or where one more line would leave the list
                        // less than the switch pressed, does the bar show
                        // fewer lines than its words take: they scroll in
                        // it, a thumb saying so, from their first line.
                        let scrolls = after
                            .paint
                            .iter()
                            .any(|node| node.key.as_str() == "settings/status/visual/thumb");
                        let fills = after
                            .semantic(&UiKey::new("settings/feedback"))
                            .is_some_and(|bar| {
                                bar.rect.bottom() >= height - 0.5
                                    && bar.rect.y <= compact_toolbar_height() + 32.0 * scale
                            });
                        let bounded =
                            lab.state()
                                .scroll_plan()
                                .zip(was)
                                .is_some_and(|(plan, was)| {
                                    plan.room() < was.height + status_line_height() + 0.5
                                });
                        let from_the_top =
                            bare(&feedback).starts_with(&bare(&read)) && !read.is_empty();
                        if bare(&read) != bare(&feedback)
                            && !(scrolls && (fills || bounded) && from_the_top)
                        {
                            faults
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push(format!("{context}: reads {read:?} of {feedback:?}"));
                        }
                    }
                    for fault in after.fit_failures().into_iter().chain(after.sliced_marks()) {
                        faults
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!("{context}: {fault}"));
                    }
                    // The switch stays whole, the bar never over it: but
                    // where the bar is as short as it gets (one line) and
                    // the window still holds no more (102x12 at 2× text: an
                    // 80pt toolbar, a 64pt bar and a 36pt list).
                    let least_bar = after
                        .paint
                        .iter()
                        .filter(|node| {
                            node.key
                                .as_str()
                                .starts_with("settings/status/visual/line-")
                                && node.clip.height >= node.rect.height - 0.5
                        })
                        .count()
                        <= 1;
                    let whole_after = after.paint.iter().any(|node| {
                        node.key == pressed
                            && node.fade.is_none()
                            && (node.clip.y - node.rect.y).abs() < 0.5
                            && (node.clip.height - node.rect.height).abs() < 0.5
                    }) && after.semantic(&pressed).is_some();
                    if kind != "reset-all" && !whole_after && !least_bar {
                        faults
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!(
                                "{context}: the pressed switch is not whole under the status"
                            ));
                    }
                    // The switch stays under the pointer wherever the list's
                    // room, the taller bar's share given up from its bottom,
                    // still holds it whole; where that cuts it, it moves the
                    // least that shows it whole: its hidden part.
                    let now = after.semantic(&pressed).map(|node| node.rect);
                    let room_bottom = lab.state().scroll_plan().and_then(|plan| {
                        after
                            .paint
                            .iter()
                            .find(|node| {
                                matches!(&node.content, UiContent::Group(spec)
                                if spec.role == SemanticRole::List)
                                    && node.key.as_str() != "settings/navigation/list"
                                    && was.is_some_and(|was| {
                                        node.rect.contains(was.x + 1.0, was.y + 1.0)
                                    })
                            })
                            .map(|list| list.rect.y + plan.room())
                    });
                    if kind != "reset-all"
                        && let (Some(was), Some(now), Some(bottom)) = (was, now, room_bottom)
                        && (now.height - was.height).abs() < 0.5
                        && (now.y - was.y).abs() > 0.5
                    {
                        let least = (was.bottom() - bottom).max(0.0);
                        if (was.y - now.y - least).abs() > 0.5 {
                            faults
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push(format!(
                                    "{context}: the pressed switch moved from {was:?} to {now:?} \
                                     (the list's room ends at {bottom})"
                                ));
                        }
                    }
                }
            },
        );
        let kinds = kinds.into_inner();
        let faults = faults
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(kinds > 0);
        assert!(
            faults.is_empty(),
            "{} faults:\n{}",
            faults.len(),
            faults.join("\n")
        );
    });
}

/// A CONTROL THE WHEEL LEFT CUT STAYS WHERE IT WAS DRAWN: the control the
/// keyboard is on is kept in view where the last screen showed it whole and
/// something since cut it (a status that came), but one the last screen drew
/// cut is drawn so again until an event moves the list. Home at 444x292 at
/// 2× text, the keyboard on Follow system light/dark under a refused write's
/// status: the wheel down the list and back drew the switch cut by the
/// room's edge, 8pt of it under a short status and all of it under a long
/// path's, and the next frame, with nothing between, drew it whole, 8pt
/// higher or back in view. CONTROL: the status that came kept the same
/// switch whole.
#[test]
fn a_control_the_wheel_left_cut_stays_where_it_was_drawn() {
    at_each_text_scale(&[2.0], "a control the wheel left cut", |_| {
        let (width, height) = (444.0, 292.0);
        let switch = UiKey::new(THEME_MODE_ACTION);
        let drawn = |compiled: &crate::native_ui::CompiledUi| {
            compiled
                .paint
                .iter()
                .find(|node| node.key == switch)
                .map(|node| {
                    (node.clip.y - node.rect.y).abs() < 0.5
                        && (node.clip.height - node.rect.height).abs() < 0.5
                })
        };
        for (status, message, cut) in [
            (
                "a short status",
                "~/.config/aterm isn\u{2019}t writable \u{b7} check its permissions",
                Some(false),
            ),
            (
                "a long path's status",
                "/Volumes/Shared Drive/Users//someone/dotfiles/config/aterm \
                 isn\u{2019}t writable \u{b7} check its permissions",
                None,
            ),
        ] {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(SettingsRoute::Home);
            let _ = lab.compile(width, height);
            lab.dispatch(AppEvent::FocusChanged(Some(switch.clone())));
            let _ = lab.compile(width, height);
            let (_, operation) = lab
                .invoke(THEME_MODE_ACTION, None)
                .expect("the switch writes");
            lab.dispatch(AppEvent::ConfigPatchFinished {
                operation,
                outcome: ConfigPatchOutcome::Rejected {
                    message: message.to_string(),
                },
            });
            assert_eq!(
                drawn(&lab.compile(width, height)),
                Some(true),
                "the control under {status}: the status that came keeps the switch whole"
            );
            lab.dispatch(AppEvent::ScrollLines(3));
            let _ = lab.compile(width, height);
            lab.dispatch(AppEvent::ScrollLines(-3));
            let back = lab.compile(width, height);
            assert_eq!(
                drawn(&back),
                cut,
                "under {status} the wheel back up draws the switch cut by the room's edge"
            );
            assert!(
                lab.compile(width, height) == back,
                "under {status} the next frame draws the screen the wheel left"
            );
        }
    });
}

/// A STATUS NEVER COVERS WHAT THE PERSON IS ON, THOUGH THE WHEEL LEFT IT CUT
/// (#92): the switch the wheel left cut stays so only while nothing comes;
/// a bar that comes over it keeps it in view. Home at 300x292 at 2× text,
/// the keyboard on Follow system light/dark, the switch pressed (its write
/// out, the switch whole at y 164), the wheel down the list and back (the
/// switch cut at y 174, 54 of its 64pt showing), then the write refused
/// under a long path's status: the switch was not painted at all, the
/// refusal over the control just pressed. It is drawn whole, and the next
/// frame draws the same screen.
#[test]
fn a_status_that_comes_over_a_control_the_wheel_left_cut_keeps_it_in_view() {
    at_each_text_scale(&[2.0], "a status over a cut control", |_| {
        let (width, height) = (300.0, 292.0);
        let switch = UiKey::new(THEME_MODE_ACTION);
        let painted = |compiled: &crate::native_ui::CompiledUi| {
            compiled
                .paint
                .iter()
                .find(|node| node.key == switch)
                .map(|node| (node.rect, node.clip))
        };
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Home);
        let _ = lab.compile(width, height);
        lab.dispatch(AppEvent::FocusChanged(Some(switch.clone())));
        let _ = lab.compile(width, height);
        let (_, operation) = lab
            .invoke(THEME_MODE_ACTION, None)
            .expect("the switch writes");
        let _ = lab.compile(width, height);
        lab.dispatch(AppEvent::ScrollLines(1));
        let _ = lab.compile(width, height);
        lab.dispatch(AppEvent::ScrollLines(-1));
        let cut = painted(&lab.compile(width, height));
        assert!(
            cut.is_some_and(|(rect, clip)| clip.height < rect.height - 0.5),
            "the wheel leaves the switch cut: {cut:?}"
        );
        lab.dispatch(AppEvent::ConfigPatchFinished {
            operation,
            outcome: ConfigPatchOutcome::Rejected {
                message: "/Volumes/Shared Drive/Users//someone/dotfiles/config/aterm \
                          isn\u{2019}t writable \u{b7} check its permissions"
                    .to_string(),
            },
        });
        let after = lab.compile(width, height);
        let now = painted(&after);
        assert!(
            now.is_some_and(|(rect, clip)| (clip.y - rect.y).abs() < 0.5
                && (clip.height - rect.height).abs() < 0.5),
            "the refusal's status keeps the switch whole: {cut:?} to {now:?}"
        );
        assert!(
            lab.compile(width, height) == after,
            "the next frame draws the screen the status left"
        );
    });
}

/// A STATUS'S BAR TAKES THE PAGE'S BOTTOM INSET BEFORE ITS LIST (#92): at
/// 220x12 at 2× text, Sound's Music effects switch pressed, its one-line
/// saved status (64pt, Undo beside it) took all it needs from the list's
/// room, which the page had given the bar's, and cut the 72pt switch to 52
/// while the page kept 24pt of blank under the list. The bar takes that
/// inset first: the switch stays whole where it was pressed.
#[test]
fn a_status_bar_takes_the_pages_bottom_inset_before_its_list() {
    at_each_text_scale(&[2.0], "bottom inset", |_| {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Sound);
        let key = UiKey::new("settings/control/trail_sounds");
        let before = lab.compile(1_564.0, 180.0);
        let was = rect_of(&before, key.as_str());
        lab.dispatch(AppEvent::FocusChanged(Some(key.clone())));
        let _ = lab.compile(1_564.0, 180.0);
        lab.commit("settings/set/trail_sounds", None);
        let after = lab.compile(1_564.0, 180.0);
        assert!(lab.state().feedback.is_some(), "a status is up");
        let now = after
            .paint
            .iter()
            .find(|node| node.key == key)
            .map(|node| (node.rect, node.clip));
        assert!(
            now.is_some_and(|(rect, clip)| rect == was && clip == rect),
            "the switch stays whole where it was pressed: {was:?} to {now:?}"
        );
    });
}

/// A STATUS NEVER COVERS THE SWITCH THE PERSON PRESSED (#92): at 22x24 at
/// 2× text "Allow programs to read the clipboard turned on", on five lines
/// with Undo under them, took 228pt of the 348pt window and left the list
/// 40pt: the switch just pressed was under the bar, where a second click
/// landed on Undo. The bar is as tall as leaves the switch whole, its words
/// scrolling in it from their first line — every one of them reached by
/// the wheel over the bar, none cut: the middle cut that took "read" out
/// of them is gone. NEGATIVE CONTROL: the wheel over the page's list
/// scrolls the list, not the status.
#[test]
fn a_status_never_covers_the_switch_the_person_pressed() {
    at_each_text_scale(&[2.0], "covered switch", |_| {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Security);
        let key = UiKey::new("settings/control/allow_osc52_query");
        let _ = lab.compile(178.0, 348.0);
        lab.dispatch(AppEvent::FocusChanged(Some(key.clone())));
        let _ = lab.compile(178.0, 348.0);
        lab.commit("settings/set/allow_osc52_query", None);
        let after = lab.compile(178.0, 348.0);
        let feedback = lab.state().feedback.clone().unwrap_or_default();
        assert!(feedback.ends_with("turned on"), "{feedback:?}");
        let switch = after
            .paint
            .iter()
            .find(|node| node.key == key)
            .map(|node| (node.rect, node.clip));
        let bar = rect_of(&after, "settings/feedback");
        assert!(
            switch.is_some_and(|(rect, clip)| clip == rect && rect.bottom() <= bar.y + 0.5),
            "the switch is whole over the bar: {switch:?}, bar {bar:?}"
        );
        let read = status_words(&after, "settings/status/visual");
        assert!(
            read.starts_with("Allow programs") && !read.contains('\u{2026}'),
            "the status starts at its first words, none cut: {read:?}"
        );
        assert_eq!(
            every_status_line(&mut lab, 178.0, 348.0, "settings/status/visual").join(" "),
            feedback,
            "every word is on a line"
        );
        assert!(
            !read.ends_with("turned on"),
            "the bar shows fewer lines than the words take: {read:?}"
        );
        assert!(
            after
                .paint
                .iter()
                .any(|node| node.key.as_str() == "settings/status/visual/thumb"),
            "a thumb says the words scroll"
        );
        // The wheel over the page's list scrolls the list, not the words.
        lab.dispatch(AppEvent::ScrollAt {
            lines: 1,
            key: UiKey::new("settings/list"),
        });
        assert!(
            lab.state().status_scroll.borrow().offset.abs() < 0.01,
            "the wheel over the list leaves the status where it was"
        );
        let _ = lab.compile(178.0, 348.0);
        // The wheel over the bar scrolls its words to their end, a line at
        // a time.
        let mut end = read.clone();
        for _ in 0..8 {
            lab.dispatch(AppEvent::ScrollAt {
                lines: 1,
                key: UiKey::new("settings/status"),
            });
            end = status_words(&lab.compile(178.0, 348.0), "settings/status/visual");
        }
        assert!(
            end.ends_with("turned on"),
            "the wheel reaches its end: {end:?}"
        );
        assert!(!end.contains('\u{2026}'), "{end:?}");
    });
}

/// A STATUS AFTER A WRITE IN FLIGHT NEVER COVERS THE SWITCH EITHER (#92):
/// the host renders while a write is on its way ("Applying…", its own bar)
/// and the switch pressed is nudged whole above that bar; the result's
/// taller bar then came over it, and the list read the switch as whole
/// still, the first stop being "live" past the room's edge under the bar:
/// at 22x24 at 2× text Keyboard & Input's Copy on select switch was cut to
/// 32 of 64pt, Security's clipboard switch went under the bar altogether.
#[test]
fn a_status_after_a_write_in_flight_never_covers_the_switch() {
    at_each_text_scale(&[2.0], "write in flight", |_| {
        for (route, key) in [
            (SettingsRoute::KeyboardInput, "copy_on_select"),
            (SettingsRoute::Security, "allow_osc52_query"),
        ] {
            let (width, height) = (178.0, 348.0);
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            let _ = lab.compile(width, height);
            let control = UiKey::new(format!("settings/control/{key}"));
            lab.dispatch(AppEvent::FocusChanged(Some(control.clone())));
            let (patch, operation) = lab
                .invoke(&format!("settings/set/{key}"), None)
                .expect("writes");
            let _ = lab.compile(width, height);
            lab.carry(patch, operation);
            let after = lab.compile(width, height);
            let switch = after
                .paint
                .iter()
                .find(|node| node.key == control)
                .map(|node| (node.rect, node.clip));
            let bar = rect_of(&after, "settings/feedback");
            assert!(
                switch.is_some_and(|(rect, clip)| clip == rect && rect.bottom() <= bar.y + 0.5),
                "{route:?}: the switch is whole over the bar: {switch:?}, bar {bar:?}"
            );
        }
    });
}

/// A POINTER PRESS KEEPS ITS CONTROL IN VIEW (#92): the host's pointer
/// puts the keyboard on the control itself, with no focus change the
/// reducer sees, then sends the action, and nothing kept the control in
/// view: at 80x16 Follow system pressed on added the Dark and Light theme
/// rows, the page chose its chrome again, the preview went into the list,
/// and the switch jumped 78pt up from under the pointer; at 80x24 at 2×
/// text on Modified, pressed off, the Color theme row came back, the
/// summary went into the list, and the switch jumped 68pt. Keyboard and
/// pointer presses keep it alike: the switch and what the page pins stay.
#[test]
fn a_pointer_press_keeps_its_control_in_view() {
    for (scale, route, source, (width, height), pinned) in [
        (
            1.0,
            SettingsRoute::Appearance,
            "",
            (584.0, 236.0),
            "settings/preview/appearance",
        ),
        (
            2.0,
            SettingsRoute::Modified,
            "theme = \"dark:Default,light:Default Light\"\n",
            (584.0, 348.0),
            "settings/modified/reset-all",
        ),
    ] {
        at_each_text_scale(&[scale], "pointer press", |_| {
            let mut lab = Lab::new(source);
            lab.state_mut().navigate(route);
            let key = UiKey::new("settings/theme-mode");
            let before = lab.compile(width, height);
            lab.state_mut().common.last_focus = Some(key.clone());
            let _ = lab.compile(width, height);
            let (patch, operation) = lab.invoke("settings/theme-mode", None).expect("writes");
            let _ = lab.compile(width, height);
            lab.carry(patch, operation);
            let after = lab.compile(width, height);
            let rect = |compiled: &crate::native_ui::CompiledUi, key: &str| {
                compiled.semantic(&UiKey::new(key)).map(|node| node.rect)
            };
            assert_eq!(
                rect(&after, pinned),
                rect(&before, pinned),
                "{route:?} at {scale}x: {pinned} stays where it was"
            );
            // The switch stays, but for the hidden part a bar under the
            // page took from it (a docked status takes none).
            let (was, now) = (rect(&before, key.as_str()), rect(&after, key.as_str()));
            let hidden = rect(&after, "settings/feedback")
                .filter(|bar| bar.y > 100.0)
                .zip(was)
                .map_or(0.0, |(bar, was)| (was.bottom() - bar.y).max(0.0));
            assert!(
                was.zip(now)
                    .is_some_and(|(was, now)| (was.y - now.y - hidden).abs() < 0.5),
                "{route:?} at {scale}x: the switch moved from {was:?} to {now:?}, its hidden \
                 part {hidden}"
            );
        });
    }
}

/// A STATUS'S PATH WRAPS AT ITS FOLDERS: a status is broken where every line
/// of aterm's chrome breaks ([`wrap_to_box`]) — between words, after a
/// path's `/` or a `-` — never inside a folder's name where the box holds
/// it: wrapped by the terminal's own rule, a save failure read
/// "…/scratchpad/labs/r1" / "3d-p4/cfg/aterm isn't writable".
#[test]
fn a_status_path_wraps_at_its_folders() {
    at_each_text_scale(&[1.0, 2.0], "status path", |_| {
        let text = "Color theme was not saved: /private/tmp/claude-502/-Users-person-aterm/\
                    75d5bccd-9c38-431b-99e5-ef81935c6fbd/scratchpad/labs/r13d-p4/cfg/aterm \
                    isn\u{2019}t writable \u{b7} check its permissions";
        let mut checked = 0;
        for width in (120..640).step_by(3) {
            let width = width as f32;
            if !pieces_fit(text, width, &status_words_width) {
                continue;
            }
            checked += 1;
            assert_eq!(
                broken_words(text, width),
                0,
                "{width}pt: {:#?}",
                status_word_lines(text, width)
            );
        }
        assert!(checked > 20, "{checked} widths");
    });
}

/// THE KEYBOARD ON A STATUS BAR READS ITS WORDS (the review): at
/// 22x24 at 2× text, where a status takes more lines than its bar shows
/// over the switch just pressed — the switch's own, and a save that failed,
/// which offers no Undo — the words' box is the keyboard's next stop after
/// the switch, and Down on it scrolls the words a line at a time to their
/// end, the keyboard staying on it, every word on a line reached; Up
/// scrolls them back to their first line. Only the wheel over the bar
/// reached them: a keyboard saw the thumb, never the words under it.
/// NEGATIVE CONTROL: Down on the switch moves the keyboard, not the words.
#[test]
fn the_keyboard_on_a_status_bar_reads_its_words() {
    at_each_text_scale(&[2.0], "status keys", |_| {
        for failed in [false, true] {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(SettingsRoute::Security);
            let key = UiKey::new("settings/control/allow_osc52_query");
            let _ = lab.compile(178.0, 348.0);
            lab.dispatch(AppEvent::FocusChanged(Some(key.clone())));
            let _ = lab.compile(178.0, 348.0);
            if failed {
                let (_, operation) = lab
                    .invoke("settings/set/allow_osc52_query", None)
                    .expect("writes");
                lab.dispatch(AppEvent::ConfigPatchFinished {
                    operation,
                    outcome: ConfigPatchOutcome::Rejected {
                        message: "/private/tmp/labs/r14fit/cfg/aterm isn\u{2019}t writable \u{b7} \
                                  check its permissions"
                            .to_string(),
                    },
                });
            } else {
                lab.commit("settings/set/allow_osc52_query", None);
            }
            let context = if failed { "a failed save" } else { "turned on" };
            let feedback = lab.state().feedback.clone().unwrap_or_default();
            let pressed = lab.compile(178.0, 348.0);
            let words = UiKey::new("settings/status/visual/words");
            let order = &pressed.focus_order;
            let at = |key: &UiKey| order.iter().position(|focus| focus == key);
            assert_eq!(
                at(&words),
                at(&key).map(|switch| switch + 1),
                "{context}: the words are the keyboard's next stop: {order:?}"
            );
            // NEGATIVE CONTROL: on the switch, Down is the list's.
            lab.dispatch(AppEvent::StepFocus(1));
            let _ = lab.compile(178.0, 348.0);
            assert!(
                lab.state().status_scroll.borrow().offset.abs() < 0.01,
                "{context}: Down off the bar leaves the words where they were"
            );
            lab.dispatch(AppEvent::FocusChanged(Some(words.clone())));
            // Each line the bar shows whole, by its place in the words.
            let mut seen = std::collections::BTreeMap::<usize, String>::new();
            let read = |lab: &Lab, seen: &mut std::collections::BTreeMap<usize, String>| {
                let compiled = lab.compile(178.0, 348.0);
                for node in &compiled.paint {
                    if let (Some(index), UiContent::Text(text)) = (
                        node.key
                            .as_str()
                            .strip_prefix("settings/status/visual/line-")
                            .and_then(|index| index.parse().ok()),
                        &node.content,
                    ) && node.clip.height >= node.rect.height - 0.5
                    {
                        seen.insert(index, text.text.clone());
                    }
                }
                status_words(&compiled, "settings/status/visual")
            };
            let first = read(&lab, &mut seen);
            for _ in 0..16 {
                lab.dispatch(AppEvent::StepFocus(1));
                let _ = read(&lab, &mut seen);
                assert_eq!(
                    lab.state().common.last_focus.as_ref(),
                    Some(&words),
                    "{context}: the keyboard stays on the words"
                );
            }
            let whole = seen.values().cloned().collect::<Vec<_>>().join(" ");
            // A path goes on at its folders, with no space between.
            let bare = |text: &str| text.split_whitespace().collect::<String>();
            assert_eq!(
                bare(&whole),
                bare(&feedback),
                "{context}: every word is reached: {seen:?}"
            );
            assert!(!whole.contains('\u{2026}'), "{context}: {whole:?}");
            for _ in 0..16 {
                lab.dispatch(AppEvent::StepFocus(-1));
            }
            assert_eq!(
                read(&lab, &mut seen),
                first,
                "{context}: Up scrolls them back to their first line"
            );
            assert!(first.starts_with("Allow"), "{context}: {first:?}");
        }
    });
}

/// A SAVE THAT FAILED IS READ WHOLE IN A SHORT BAR (the audit):
/// at 22x24 at 2× text a save failure that names the folder to fix, set on
/// more lines than the bar holds over the switch just pressed, keeps every
/// word — which change was not saved, the folder, and what to do — on a
/// line the wheel over the bar scrolls to. The bar took its middle out:
/// "Allow / programs to / read the … / isn’t writable · / check its /
/// permissions", "clipboard was not saved:" and the folder gone.
#[test]
fn a_save_that_failed_is_read_whole_in_a_short_bar() {
    at_each_text_scale(&[2.0], "failure in a short bar", |_| {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Security);
        let key = UiKey::new("settings/control/allow_osc52_query");
        let _ = lab.compile(178.0, 348.0);
        lab.dispatch(AppEvent::FocusChanged(Some(key.clone())));
        let _ = lab.compile(178.0, 348.0);
        let (_, operation) = lab
            .invoke("settings/set/allow_osc52_query", None)
            .expect("writes");
        lab.dispatch(AppEvent::ConfigPatchFinished {
            operation,
            outcome: ConfigPatchOutcome::Rejected {
                message: "/private/tmp/labs/r14fit/cfg/aterm isn\u{2019}t writable \u{b7} \
                          check its permissions"
                    .to_string(),
            },
        });
        let after = lab.compile(178.0, 348.0);
        let feedback = lab.state().feedback.clone().unwrap_or_default();
        assert!(
            feedback.contains("was not saved: /private/tmp/"),
            "{feedback:?}"
        );
        let every = every_status_line(&mut lab, 178.0, 348.0, "settings/status/visual");
        let bare = |text: &str| text.split_whitespace().collect::<String>();
        assert_eq!(bare(&every.concat()), bare(&feedback), "{every:#?}");
        assert!(
            every.iter().all(|line| !line.contains('\u{2026}')),
            "{every:#?}"
        );
        let mut seen = status_words(&after, "settings/status/visual");
        let mut read = seen.clone();
        for _ in 0..every.len() {
            lab.dispatch(AppEvent::ScrollAt {
                lines: 1,
                key: UiKey::new("settings/status/visual/line-0"),
            });
            let now = status_words(&lab.compile(178.0, 348.0), "settings/status/visual");
            if now != seen {
                read.push(' ');
                read.push_str(&now);
            }
            seen = now;
        }
        for word in ["clipboard", "was", "not", "saved:", "permissions"] {
            assert!(read.contains(word), "{word:?} is reached: {read:?}");
        }
        // CONTROL: a status its bar holds shows whole, and no thumb.
        let mut wide = Lab::new("");
        wide.state_mut().navigate(SettingsRoute::Security);
        let _ = wide.compile(1_095.0, 700.0);
        wide.commit("settings/set/allow_osc52_query", None);
        let shown = wide.compile(1_095.0, 700.0);
        assert!(
            !shown
                .paint
                .iter()
                .any(|node| node.key.as_str() == "settings/status/visual/thumb"),
            "a status its bar holds does not scroll"
        );
    });
}

/// A SWITCH A TALLER STATUS MOVES KEEPS ITS NAME (#92): Appearance's Follow
/// system switch at 80x24 at 2× text, pressed, brings up a two-line status
/// over it with Undo; the list scrolls it 40pt up, by its hidden part, and
/// the THEME caption the screen kept came down over the row's name, so the
/// switch stood alone. A whole stop beats its caption: the caption goes and
/// the row shows whole, its name over its switch.
#[test]
fn a_switch_a_taller_status_moves_keeps_its_name() {
    at_each_text_scale(&[2.0], "status press", |_| {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Appearance);
        let before = lab.compile(584.0, 348.0);
        let name = |compiled: &crate::native_ui::CompiledUi| {
            compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == "settings/label/theme-mode")
                .map(|node| (node.rect, node.clip))
        };
        let was = rect_of(&before, "settings/theme-mode");
        lab.dispatch(AppEvent::FocusChanged(Some(UiKey::new(
            "settings/theme-mode",
        ))));
        let _ = lab.compile(584.0, 348.0);
        lab.commit("settings/theme-mode", None);
        let after = lab.compile(584.0, 348.0);
        let now = rect_of(&after, "settings/theme-mode");
        assert!(
            lab.state().feedback.is_some() && now.y < was.y - 0.5,
            "the status's bar moves the switch: {was:?} to {now:?}"
        );
        let name = name(&after);
        assert!(
            name.is_some_and(
                |(rect, clip)| clip.height >= rect.height - 0.5 && clip.width >= rect.width - 0.5
            ),
            "the switch's name is drawn whole over it: {name:?}"
        );
    });
}

/// A CJK STATUS BREAKS NO WORD WHERE IT BREAKS BETWEEN CHARACTERS: the
/// shared wrap sets CJK lines between characters, a break as good as a
/// space, so the bar that weighs where its Undo goes by the words it breaks
/// ([`broken_words`]) counts none there, as for a Latin status that breaks
/// only at its spaces, and keeps its Undo beside the words. Counted as
/// broken words, a narrower box broke more of them, and every CJK status
/// that took two lines put its Undo under them.
#[test]
fn a_cjk_status_breaks_no_word_between_its_characters() {
    let cjk = "設定を保存できませんでした。フォルダーの権限を確認してください";
    assert!(status_word_lines(cjk, 120.0).len() > 1, "two lines or more");
    assert_eq!(broken_words(cjk, 120.0), 0);
    // A Latin word wider than its box is still broken inside.
    assert!(broken_words("Restored", 20.0) > 0);
    assert_eq!(broken_words("Color theme set to Default", 60.0), 0);
}

/// The group cards a compiled list draws (`settings/group/…`), each painted
/// rect and clip.
fn group_cards(
    compiled: &crate::native_ui::CompiledUi,
    list: LogicalRect,
) -> Vec<(String, LogicalRect, LogicalRect)> {
    compiled
        .paint
        .iter()
        .filter(|node| {
            node.key.as_str().starts_with("settings/group/")
                && !node.key.as_str().contains("/scrolled-rows")
                && node.rect.x >= list.x - 0.5
                && node.rect.right() <= list.right() + 0.5
                && !node.clip.is_empty()
        })
        .map(|node| (node.key.as_str().to_string(), node.rect, node.clip))
        .collect()
}

/// WITH A STATUS BAR UP, END AND THE WHEEL SHOW THE LAST CARD WHOLE: on every
/// list page at 80x24, 102x29 and 150x48, a plain status up first, End — and
/// a wheel run past the end — draws the list's last card whole inside the
/// list, its closing padding and bottom border on screen. The last screen let
/// that padding fall in the strip the page keeps for a bar, which the bar
/// takes, and cut the card there on every screen (Terminal at 102x29: the
/// SYSTEM card 14pt short; Modified at 80x24: 18pt; Appearance at 150x48).
#[test]
fn with_a_status_bar_up_end_shows_the_last_card_whole() {
    let mut ended = 0;
    let mut faults = Vec::new();
    for route in LIST_ROUTES
        .into_iter()
        .chain([SettingsRoute::Modified, SettingsRoute::TextFonts])
    {
        for (width, height) in [(584.0, 348.0), (738.0, 418.0), (1_074.0, 684.0)] {
            for by in ["End", "the wheel"] {
                let mut lab = Lab::new("cursor_blink = false\nfont_px = 15.0\n");
                lab.state_mut().navigate(route);
                let _ = lab.compile(width, height);
                lab.state_mut()
                    .show_transient_status_for_test("Cursor blink turned off");
                let _ = lab.compile(width, height);
                dispatch(
                    &mut lab,
                    if by == "End" {
                        AppEvent::TextInput(TextInputEvent::End { extend: false })
                    } else {
                        AppEvent::ScrollLines(200)
                    },
                );
                let compiled = lab.compile(width, height);
                let Some((plan, shown)) = lab.state().scroll_shown() else {
                    continue;
                };
                if plan.limit() == 0 || shown.end < plan.len() {
                    continue;
                }
                let Some((list, _)) = compiled_list(&compiled, &plan) else {
                    continue;
                };
                let Some((key, rect, clip)) = group_cards(&compiled, list)
                    .into_iter()
                    .max_by(|a, b| a.1.bottom().total_cmp(&b.1.bottom()))
                else {
                    continue;
                };
                ended += 1;
                if rect.bottom() > clip.bottom() + 0.5 {
                    faults.push(format!(
                        "{route:?} {width}×{height} by {by}: {key} {rect:?} cut at {}",
                        clip.bottom()
                    ));
                }
            }
        }
    }
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
    assert!(ended > 0, "some page scrolls to its end");
}

/// NOTHING SCROLLED, NO ROW MOVES: a list on its last screen holds the place
/// it drew when a press adds a stop after its end. Cursor Styles at 220x60
/// with Off saved, wheeled to its end: pressing Lightning there saves it and
/// brings its Adjust card after the list's end, and the screen at the same
/// first stop was no longer the last one — every row went 40pt up from under
/// the pointer, Comet in Lightning's place (SC12). Lightning, and Classic,
/// stay where they were.
#[test]
fn a_stop_added_after_the_end_moves_no_row_on_the_last_screen() {
    for style in ["laser", "classic"] {
        let mut lab = Lab::new("cursor_trail_style = \"off\"\n");
        lab.state_mut().navigate(SettingsRoute::CursorStyles);
        let (width, height) = (1_564.0, 854.0);
        let _ = lab.compile(width, height);
        dispatch(&mut lab, AppEvent::ScrollLines(10));
        let before = lab.compile(width, height);
        let key = format!("settings/cursor-style/{style}");
        let at = |compiled: &crate::native_ui::CompiledUi| {
            compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == key)
                .map(|node| node.rect)
        };
        let Some(rect) = at(&before) else {
            panic!("{style} is drawn on the last screen");
        };
        let (plan, _) = lab.state().scroll_shown().expect("a list");
        assert!(
            lab.state().page_scroll >= plan.limit(),
            "the wheel reached the last screen"
        );
        // A pointer press moves the keyboard to the row first.
        lab.state_mut().common.last_focus = Some(UiKey::new(key.as_str()));
        let _ = lab.press(&key);
        let after = lab.compile(width, height);
        assert_eq!(saved_cursor_style(lab.state()), style);
        assert_eq!(at(&after), Some(rect), "{style} stays under the pointer");
    }
}

/// ONE BOUNDARY FOR WHAT A SCREEN SHOWS AND WHAT IT DRAWS AS SHOWN: on every
/// list page's screens before the last, at 80x24 and the shipped 102x29, a
/// control the room's bottom edge cuts — part of it in the room, the rest
/// past the edge — is the screen's peek: fading from its top, live nowhere,
/// a press on any of it the list's. It was painted at full strength over
/// the edge (0.87 of it and more where it ran a few points past), live above
/// the edge only, and Page Down turned onto it again (Terminal's Accent color
/// at 102x29, Sound's Ambient sound bed, Appearance's Selection color); at
/// 80x24, where no strip fades, its words were dropped and its face left live
/// with no name (Software Update's Install updates automatically, Terminal's
/// Bidirectional text).
#[test]
fn a_control_the_rooms_edge_cuts_is_a_picture_from_its_top() {
    let mut cut = 0;
    let mut faults = Vec::new();
    for route in LIST_ROUTES {
        for (width, height) in [(584.0, 348.0), (738.0, 418.0)] {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            lab.each_screen(width, height, |state, compiled, top| {
                // The screens before the last.
                let Some((plan, _)) = state.scroll_shown().filter(|(plan, _)| top < plan.limit())
                else {
                    return;
                };
                if plan.foot() == scroll::ListFoot::Whole {
                    return;
                }
                let Some((list, room)) = compiled_list(compiled, &plan) else {
                    return;
                };
                let edge = list.y + room;
                for node in compiled.paint.iter().filter(|node| {
                    is_control(&node.content)
                        && node.rect.x >= list.x - 0.5
                        && node.rect.right() <= list.right() + 0.5
                        && node.rect.y < edge - 0.5
                        && node.rect.bottom() > edge + 0.5
                        && node.clip.y < edge - 0.5
                }) {
                    cut += 1;
                    let picture = node.fade.is_some_and(|(top, _)| top <= node.rect.y + 0.5)
                        && compiled.semantic(&node.key).is_none()
                        && compiled.hits.iter().all(|hit| hit.key != node.key);
                    if !picture {
                        faults.push(format!(
                            "{route:?} {width}×{height} screen {top}: {} {:?} over the edge at \
                             {edge}: fade {:?}, live {:?}",
                            node.key.as_str(),
                            node.rect,
                            node.fade,
                            compiled.semantic(&node.key).map(|node| node.rect)
                        ));
                    }
                }
            });
        }
    }
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
    assert!(cut > 0, "some screen draws a control over its room's edge");
}

/// A CUT CONTROL IS DRAWN WITH ITS NAME OR NOT AT ALL: on every screen of
/// the list pages at 80x24, 102x29 and 150x48, no list draws a sliver of a
/// control — less than [`scroll::PEEK_MIN`] of it — and every control an edge
/// cuts that is drawn and live shows its row's name. A 4pt sliver of Cursor
/// & Motion's Serious Mode switch under the preview at 80x24 read as a stray
/// line and a click on it turned Serious Mode on; Terminal's search match
/// color field and Sound's detonation switch showed 9pt under a kept
/// caption; at 102x29 Keyboard & Input's multi-line paste switch was 64%
/// drawn under its caption with neither its name nor its On.
#[test]
fn a_cut_control_is_drawn_with_its_name_or_not_at_all() {
    let mut faults = Vec::new();
    let mut cut_live = 0;
    for route in LIST_ROUTES {
        for (width, height) in [(584.0, 348.0), (738.0, 418.0), (1_074.0, 684.0)] {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            lab.each_screen(width, height, |_, compiled, top| {
                let context = format!("{route:?} {width}×{height} screen {top}");
                for node in compiled
                    .paint
                    .iter()
                    .filter(|node| is_control(&node.content) && node.peek)
                {
                    let shows = node.clip.height;
                    if shows + 0.5 < node.rect.height.min(scroll::PEEK_MIN) && shows > 0.5 {
                        faults.push(format!(
                            "{context}: {} {:?} drawn {shows}pt",
                            node.key.as_str(),
                            node.rect
                        ));
                    }
                    let live = compiled.semantic(&node.key).is_some();
                    let cut = (node.clip.height - node.rect.height).abs() > 0.5;
                    let Some(setting) = node
                        .key
                        .as_str()
                        .strip_prefix("settings/control/")
                        .or_else(|| {
                            node.key
                                .as_str()
                                .strip_prefix("settings/segment/")
                                .and_then(|rest| rest.rsplit_once('/').map(|(key, _)| key))
                        })
                    else {
                        continue;
                    };
                    if !(live && cut) {
                        continue;
                    }
                    cut_live += 1;
                    let line = UiKey::new(format!("settings/label/{setting}/visual/line-0"));
                    if compiled.paint.iter().any(|paint| paint.key == line)
                        && compiled.text_fit_audit_line(&line).is_none()
                    {
                        faults.push(format!(
                            "{context}: {} cut {:?} of {:?}, live, its name not drawn",
                            node.key.as_str(),
                            node.clip,
                            node.rect
                        ));
                    }
                }
            });
        }
    }
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
    assert!(cut_live > 0, "some screen draws a cut control live, named");
}

/// A KEPT CAPTION HEADS WHAT IS UNDER IT: on the last screen of every list
/// page at 80x24, 102x29 and 150x48, a group caption the list keeps at its
/// top has a row of the group drawn under it — a control or a line of
/// words in the room under the caption. Kept over a row the end's scroll
/// had left whole, the caption cut that row, the edge rule took its cut
/// control with its name, and the screen drew a caption over an empty card
/// (Cursor & Motion at 102x29: EFFECT POLICY, Serious Mode gone under it).
#[test]
fn a_kept_caption_heads_a_row_on_every_last_screen() {
    let mut faults = Vec::new();
    let mut kept = 0;
    for route in LIST_ROUTES
        .into_iter()
        .chain([SettingsRoute::TextFonts, SettingsRoute::Ai])
    {
        for (width, height) in [(584.0, 348.0), (738.0, 418.0), (1_074.0, 684.0)] {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            let _ = lab.compile(width, height);
            let limit = lab.state().scroll_plan().map_or(0, |plan| plan.limit());
            lab.state_mut().scroll_page_to(limit);
            lab.state_mut().page_reveal = None;
            let compiled = lab.compile(width, height);
            for under in compiled
                .paint
                .iter()
                .filter(|node| node.key.as_str().ends_with("/scrolled-rows"))
            {
                kept += 1;
                let room = under.clip;
                let heads = compiled.paint.iter().any(|node| {
                    let inside = node.clip.y >= room.y - 0.5
                        && node.clip.bottom() <= room.bottom() + 0.5
                        && node.clip.height > 0.5;
                    inside
                        && (is_control(&node.content)
                            || (matches!(node.content, UiContent::Text(_))
                                && compiled.text_fit_audit_line(&node.key).is_some()))
                });
                if !heads {
                    faults.push(format!(
                        "{route:?} {width}×{height} last screen {limit}: {} heads nothing \
                         under its kept caption ({room:?})",
                        under.key.as_str()
                    ));
                }
            }
        }
    }
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
    assert!(kept > 0, "some last screen keeps a caption");
}

/// A NAMELESS HALF SWITCH IS NO SWITCH TO PRESS: Software Update at 80x24,
/// whose first screen ends on Install updates automatically cut by the list's
/// edge, draws that row as the screen's peek, fading from its top — no hit
/// where its half switch and its Reset show, so a press there works nothing
/// and only scrolls the row whole into the list; the next press works it
/// (SC9: the cut switch, its name dropped, turned on for a press on its half,
/// and its Reset reset it for a press on blank backdrop).
#[test]
fn a_press_on_softwares_cut_auto_update_row_only_scrolls_it_in() {
    let (width, height) = (584.0, 348.0);
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::SoftwareUpdate);
    let first = lab.compile(width, height);
    let switch = UiKey::new("settings/control/update.auto_apply");
    let (plan, _) = lab.state().scroll_shown().expect("a list");
    let (list, room) = compiled_list(&first, &plan).expect("the list");
    let node = first
        .paint
        .iter()
        .find(|node| node.key == switch)
        .expect("the switch is drawn on the first screen");
    let edge = list.y + room;
    assert!(
        node.rect.y < edge && node.rect.bottom() > edge,
        "the list's edge cuts it at 80x24: {:?} at {edge}",
        node.rect
    );
    assert!(
        first.semantic(&switch).is_none(),
        "the cut switch is not live"
    );
    for x in [node.clip.x + 4.0, list.right() - 30.0] {
        let y = node.clip.y + node.clip.height / 2.0;
        assert!(
            first
                .hits
                .iter()
                .all(|hit| !hit.rect.contains(x, y) || !hit.key.as_str().contains("update.")),
            "no press at {x},{y} works the row"
        );
    }
    let text = lab.text();
    dispatch(&mut lab, AppEvent::PressListPicture(switch.clone()));
    let scrolled = lab.compile(width, height);
    assert_eq!(lab.text(), text, "the press wrote nothing");
    let now = rect_of(&scrolled, switch.as_str());
    let (plan, _) = lab.state().scroll_shown().expect("a list");
    let (list, room) = compiled_list(&scrolled, &plan).expect("the list");
    assert!(
        now.y >= list.y - 0.5 && now.bottom() <= list.y + room + 0.5,
        "scrolled whole into the list: {now:?}"
    );
}

/// A STATUS NEVER MOVES THE SWITCH A PRESS FOUND WHOLE ON A COMPACT PAGE
/// THAT KEEPS NO ROOM FOR IT: at 80x24 and 80x26, where the page keeps no
/// room under its list for the status bar a change brings up (its toolbar
/// docks that bar), every page's last switch drawn whole is pressed, and the
/// status the press brings up takes the dock — its words on the dock's one
/// row, whole in its label — so the switch stays where it was, under the
/// pointer, and no bar stands over it. Software Update's "Install updates
/// automatically turned on · This copy can't update; the setting is saved"
/// took two lines of the dock, stood under the page instead, and took its
/// room from the list's bottom: the switch went 23pt up at 80x26 (SC7).
#[test]
fn a_status_never_moves_a_switch_pressed_whole_on_a_compact_page() {
    let mut pressed = 0;
    let mut faults = Vec::new();
    for route in SettingsRoute::ALL {
        if matches!(route, SettingsRoute::Messages | SettingsRoute::Themes) {
            continue;
        }
        for (width, height) in [(584.0, 348.0), (584.0, 376.0)] {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            let before = lab.compile(width, height);
            let Some((key, rect)) = before
                .paint
                .iter()
                .rfind(|node| {
                    matches!(node.content, UiContent::Switch(_))
                        && node.clip == node.rect
                        && before.semantic(&node.key).is_some()
                })
                .map(|node| (node.key.clone(), node.rect))
            else {
                continue;
            };
            let Some(crate::native_ui::SemanticValue::Bool(on)) =
                before.semantic(&key).map(|node| node.value.clone())
            else {
                continue;
            };
            let setting = key
                .as_str()
                .trim_start_matches("settings/control/")
                .to_string();
            // A pointer press puts the keyboard on the control first.
            lab.state_mut().common.last_focus = Some(key.clone());
            let Some((patch, operation)) = lab.invoke(
                &format!("settings/set/{setting}"),
                Some(SemanticInput::Bool(!on)),
            ) else {
                continue;
            };
            lab.carry(patch, operation);
            let after = lab.compile(width, height);
            pressed += 1;
            let context = format!("{route:?} {width}×{height} {setting}");
            let now = after
                .paint
                .iter()
                .find(|node| node.key == key)
                .map(|node| node.rect);
            if now != Some(rect) {
                faults.push(format!("{context}: moved from {rect:?} to {now:?}"));
            }
            if let Some(bar) = after
                .paint
                .iter()
                .find(|node| node.key.as_str() == "settings/feedback")
                .filter(|bar| {
                    bar.rect
                        .intersect(rect)
                        .is_some_and(|over| over.height > 0.5 && over.width > 0.5)
                })
            {
                faults.push(format!(
                    "{context}: {:?} under the bar {:?}",
                    rect, bar.rect
                ));
            }
        }
    }
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
    assert!(pressed > 0, "some page presses a switch");
}
