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

/// A wide window whose Security list scrolls: the shipped 1074×684 where the
/// page holds This Mac's Universal Control and Secure Keyboard Entry (macOS's
/// rows, `native_advanced_effect`), a shorter one where its six other rows
/// fit 684 whole.
fn wide_scrolling_security() -> (f32, f32) {
    let macos_rows = native_advanced_key(prefs::EDIT_MACHINE_UNIVERSAL_CONTROL);
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
/// to its room's edge, its last stop's content ending there at the latest —
/// never a screen that ends on a band of blank, and never one scrolled past
/// its end to hide a cut. CONTROL: some last screen draws such a stop cut.
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
            lab.state_mut().page_scroll = plan.limit();
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
            let content = compiled
                .paint
                .iter()
                .filter(inside)
                .filter(|node| {
                    matches!(node.content, UiContent::Text(_)) || is_control(&node.content)
                })
                .map(|node| node.rect.bottom())
                .fold(list.y, f32::max);
            assert!(
                content <= list.y + room + 0.5,
                "{context}: the last stop's content ends past the room's edge: {content} vs {}",
                list.y + room
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
    // `defaults`, `native_advanced_effect`); elsewhere it lives in Modified,
    // which says the same timing (`settings_modified_tests`).
    if native_advanced_key(key) {
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

    // A refused draft's longer sentence does not fit beside Review in
    // Modified and Discard in a narrower window: the buttons go under it.
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Terminal);
    let _ = lab.invoke(
        &format!("settings/set/{}", prefs::EDIT_SEARCH_HISTORY_LINES),
        Some(SemanticInput::Text("lots".to_string())),
    );
    let compiled = lab.compile(600.0, height);
    let bar = rect_of(&compiled, "settings/feedback");
    let status = rect_of(&compiled, "settings/draft-recovery/status");
    let discard = rect_of(&compiled, "settings/drafts/discard-all");
    assert!(bar.height > scaled_control_height(), "{bar:?}");
    assert!(discard.y >= status.bottom() - 0.01, "under the words");
    let painted = painted_text(&compiled, "settings/draft-recovery/status/visual");
    assert!(painted.ends_with("or discard it."), "{painted}");
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
        lab.state_mut().page_scroll = 1_000;
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
/// shows is live: a control left whole under its cut label answers the
/// pointer, a theme card's frame cut in half answers over its half — as in
/// any native scroll view, and focusing either scrolls its stop whole.
#[test]
fn a_row_the_top_edge_cuts_is_drawn_and_live_where_it_shows() {
    let row = |key: &str, spec: ButtonSpec| {
        UiNode::new(key, UiContent::Group(GroupSpec::new(key)))
            .layout(Layout::column().height(Length::Fixed(60.0)))
            .children(vec![
                UiNode::new(
                    format!("{key}/label"),
                    UiContent::Text(TextSpec::heading(key)),
                )
                .layout(Layout::default().height(Length::Fixed(20.0))),
                UiNode::new(
                    format!("{key}/control"),
                    UiContent::Button(Control::new(spec, ActionId::new(format!("{key}/control")))),
                )
                .layout(Layout::default().height(Length::Fixed(40.0))),
            ])
    };
    let tree = UiTree::new(
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
                .scrolled(40.0)
                .clipped(),
        )
        .room(100.0, false)
        .children(vec![
            // -40..20: the top edge cuts its label and half its control.
            row("card", ButtonSpec::new("A theme").visual_label("")),
            // 20..80: whole.
            row("whole", ButtonSpec::new("whole")),
        ]),
    )
    .compile(LogicalRect::new(0.0, 0.0, 300.0, 200.0))
    .expect("compiles");
    let cut = UiKey::new("card/control");
    assert!(
        tree.paint.iter().any(|node| node.key == cut && node.peek),
        "the cut control is drawn, cut on purpose"
    );
    let shown = LogicalRect::new(0.0, 0.0, 300.0, 20.0);
    assert_eq!(tree.semantic(&cut).map(|node| node.rect), Some(shown));
    assert!(tree.focus_order.contains(&cut));
    assert_eq!(
        tree.hits
            .iter()
            .find(|hit| hit.key == cut)
            .map(|hit| hit.rect),
        Some(shown),
        "it answers over the half that shows"
    );
    let whole = UiKey::new("whole/control");
    assert!(tree.semantic(&whole).is_some() && tree.hits.iter().any(|hit| hit.key == whole));
    assert!(tree.fit_failures().is_empty(), "{:?}", tree.fit_failures());
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
        .find(|node| {
            matches!(
                node.key.as_str(),
                "settings/list" | "settings/top/list" | "updates/list" | "settings/showcase/list"
            )
        })?
        .rect;
    Some((list, plan.room().min(list.height)))
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
/// room), cut by either edge or whole; a control drawn only in the strip
/// under the room is a fading picture, never live; a screen with stops after
/// it reaches the page's bottom (its room and the strip), and the last screen
/// ends on its last stop at the room's edge — no screen ends on a band of
/// blank, and the list never stops short to hide a cut.
#[test]
fn every_screen_fills_its_list_and_every_control_its_room_shows_is_live() {
    for route in LIST_ROUTES {
        for (width, height) in LIST_SIZES {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            let _ = lab.compile(width, height);
            let limit = lab.state().scroll_plan().map_or(0, |plan| plan.limit());
            for top in 0..=limit {
                lab.state_mut().page_scroll = top;
                lab.state_mut().page_reveal = None;
                let compiled = lab.compile(width, height);
                let Some((plan, shown)) = lab.state().scroll_shown() else {
                    continue;
                };
                let Some((list, room)) = compiled_list(&compiled, &plan) else {
                    continue;
                };
                let context = format!("{route:?} {width}×{height} screen {top}");
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
            }
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
/// Shift-Right ten more, Home and End its domain's ends, and a size set
/// past the bar comes back up (59 → 60). (The bar's span had become the
/// value's limit: Right at 36 did nothing, and after one Left from 60 the
/// span closed behind the user at 59.)
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
    assert_eq!(
        press(14.0, TextInputEvent::Home { extend: false }).as_deref(),
        Some("6")
    );
    assert_eq!(
        press(14.0, TextInputEvent::End { extend: false }).as_deref(),
        Some("200")
    );
    // A size set by automation anywhere in the domain is taken.
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::TextFonts);
    let patch = lab.invoke(
        &format!("settings/set/{}", prefs::EDIT_FONT_PX),
        Some(SemanticInput::Number(60.0)),
    );
    assert_eq!(
        patch.and_then(|(patch, _)| patch.edits.first().and_then(|edit| edit.value.clone())),
        Some("60".to_string())
    );
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

/// A NOTE A RUN OF ROWS SHARES IS THE RUN'S ([`prefs::RowNote::Shared`]):
/// Modified listing both trail colors draws their one note once, as the stop
/// after the second (`settings/shared-note/<key>`), apart from that row's own
/// lines, never inside them as if that row's alone; each color's control
/// says it. Negative control: one trail color listed alone draws the note as
/// its own, inside its row, and no run note.
#[test]
fn a_note_a_run_shares_is_the_stop_after_the_run() {
    let note = prefs::footnote_of(prefs::EDIT_CURSOR_TRAIL_COLOR).expect("the trail colors' note");
    assert_eq!(
        prefs::footnote_of(prefs::EDIT_CURSOR_TRAIL_ACCENT),
        Some(note)
    );
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
/// stops before it).
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
                in_room(row.rect, row.clip)
                    && compiled.semantic(&row.key).is_some()
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
                if view.start >= plan.limit() {
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
/// which the bar takes, holds only a picture — and toggling the first
/// switch an edge of the room cuts brings it whole into the room, never
/// under the bar.
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
                let on = before.semantic(&control).and_then(|node| match node.value {
                    crate::native_ui::SemanticValue::Bool(on) => Some(on),
                    _ => None,
                });
                let Some(on) = on else {
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
                if cut {
                    cut_pressed += 1;
                    let (plan, _) = lab.state().scroll_shown().expect("a list");
                    let (list, room) = compiled_list(&after, &plan).expect("the list");
                    assert!(
                        now.y >= list.y - 0.5 && now.bottom() <= list.y + room + 0.5,
                        "{context}: {now:?} whole in the room after the press"
                    );
                } else {
                    assert_eq!(now, rect, "{context}: the press moved it");
                }
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
/// its list's place and room, and every control the list showed over the
/// changed row keep their rects (the changed row's own tags are a row's
/// anatomy, not the bar's). A docked
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
                let list = rect_of(&before, "settings/list");
                let fixed = |compiled: &crate::native_ui::CompiledUi| {
                    compiled
                        .semantics
                        .iter()
                        .filter(|node| {
                            let key = node.key.as_str();
                            key == "settings/compact-toolbar"
                                || key.starts_with("settings/page-heading/")
                                || key.starts_with("settings/preview/")
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
                assert_eq!(
                    lab.state().scroll_plan().map(|plan| plan.room()),
                    room,
                    "{context}: the list's room changed"
                );
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
