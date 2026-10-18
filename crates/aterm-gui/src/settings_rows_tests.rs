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
    assert!(control.label.contains("was not saved"), "{}", control.label);
    assert!(
        !control.label.contains("press Return to save"),
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

/// THE LAST SCREEN IS FULL: a page whose last stops leave room on the screen
/// that ends on them draws the stop before them over them, cut at the list's
/// top, and its last stop ends at the list's bottom edge — never a screen
/// that ends on a band of blank.
#[test]
fn the_end_of_a_scrolled_page_fills_its_screen() {
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Terminal);
    let _ = lab.compile(738.0, 418.0);
    dispatch(
        &mut lab,
        AppEvent::TextInput(TextInputEvent::End { extend: false }),
    );
    let compiled = lab.compile(738.0, 418.0);
    let (plan, shown) = lab.state().scroll_shown().expect("a list");
    assert_eq!(shown.end, plan.len());
    assert!(
        plan.span(shown.clone()) + 1.0 < plan.room(),
        "the last stops alone leave room on the screen"
    );
    let list = compiled
        .paint
        .iter()
        .find(|node| node.key.as_str() == "settings/list")
        .expect("the list")
        .rect;
    let inside = |node: &&crate::native_ui::PaintNode| {
        node.key.as_str() != "settings/list"
            && node.clip.y >= list.y - 0.01
            && node.clip.bottom() <= list.bottom() + 0.01
            && node.rect.x >= list.x - 0.01
            && node.rect.right() <= list.right() + 0.01
    };
    assert!(
        compiled
            .paint
            .iter()
            .filter(inside)
            .any(|node| node.peek && node.rect.y < list.y - 0.5),
        "the stop before the last screen peeks in over it"
    );
    let bottom = compiled
        .paint
        .iter()
        .filter(inside)
        .filter(|node| !node.peek)
        .map(|node| node.rect.bottom())
        .fold(0.0_f32, f32::max);
    assert!(
        (bottom - list.bottom()).abs() <= 0.5,
        "the last stop ends at the list's bottom edge: {bottom} vs {}",
        list.bottom()
    );
    assert!(
        compiled
            .semantics
            .iter()
            .all(|node| !node.key.as_str().starts_with("settings/control/")
                || node.rect.y >= list.y - 0.01),
        "nothing the top edge cuts is live"
    );
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
        Some("settings/tab-color/open")
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
                .all(|node| node.key.as_str() != "settings/tab-color/open"),
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

/// A peek is painted cut at its list's edge: a control of it that shows
/// whole is live (focusable, hit, in the accessibility tree), one the edge
/// cuts is a picture of what scrolling shows; its cut is not an audit
/// failure.
#[test]
fn a_peek_is_live_where_it_shows_whole_and_never_counted_clipped() {
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
                    .height(Length::Fixed(80.0))
                    .gap(4.0)
                    .clipped(),
            )
            .children(vec![
                button("shown"),
                // 34..64: whole inside the 80pt list.
                button("whole").peek(),
                // 68..98: the list's edge cuts it at 80.
                button("cut").peek(),
            ]),
        ]),
    )
    .compile(LogicalRect::new(0.0, 0.0, 300.0, 200.0))
    .expect("compiles");
    let whole = UiKey::new("whole");
    assert!(compiled_paints(&tree, &whole));
    assert!(tree.semantic(&whole).is_some(), "a whole peek is read out");
    assert!(tree.focus_order.contains(&whole), "and takes the keyboard");
    assert!(
        tree.hits.iter().any(|hit| hit.key == whole),
        "and answers the pointer"
    );
    let cut = UiKey::new("cut");
    assert!(compiled_paints(&tree, &cut));
    assert!(tree.semantic(&cut).is_none());
    assert!(!tree.focus_order.contains(&cut));
    assert!(tree.hits.iter().all(|hit| hit.key != cut));
    assert!(tree.fit_failures().is_empty(), "{:?}", tree.fit_failures());
}

/// A row over a list's top edge ([`UiNode::peek_above`]) is a picture to its
/// last node where the edge cuts it — its label goes first, so a control it
/// still shows whole under that is not live either, nor drawn, since a
/// control is whole and live or not shown — and live where it shows whole;
/// the list scrolled by its offset draws it there.
#[test]
fn a_row_over_the_top_is_live_only_where_it_shows_whole() {
    let row = |key: &str| {
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
                    UiContent::Button(Control::new(
                        ButtonSpec::new(key),
                        ActionId::new(format!("{key}/control")),
                    )),
                )
                .layout(Layout::default().height(Length::Fixed(40.0))),
            ])
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
                    .height(Length::Fixed(100.0))
                    .scrolled(20.0)
                    .clipped(),
            )
            .children(vec![
                // -20..40: the top edge cuts its label, not its control.
                row("cut").peek_above(),
                // 40..100: whole.
                row("whole").peek_above(),
            ]),
        ]),
    )
    .compile(LogicalRect::new(0.0, 0.0, 300.0, 200.0))
    .expect("compiles");
    let cut = UiKey::new("cut/control");
    assert!(
        compiled_paints(&tree, &UiKey::new("cut")),
        "the cut row is painted"
    );
    assert!(
        !compiled_paints(&tree, &cut),
        "its control, dead under the cut label, is not drawn"
    );
    assert!(
        tree.semantic(&cut).is_none(),
        "its whole control is not read out"
    );
    assert!(!tree.focus_order.contains(&cut));
    assert!(tree.hits.iter().all(|hit| hit.key != cut));
    let whole = UiKey::new("whole/control");
    assert!(
        tree.semantic(&whole).is_some() && compiled_paints(&tree, &whole),
        "a whole row over the top is live, and drawn"
    );
    assert!(tree.hits.iter().any(|hit| hit.key == whole));
    assert!(tree.fit_failures().is_empty(), "{:?}", tree.fit_failures());
}

/// A CARD CUT OVER THE TOP KEEPS ITS FRAME: a surface button (no words of
/// its own; a theme card's frame, its tile and name drawn over it by other
/// nodes) in a row the list's top edge cuts is dead like any control there,
/// but it is the card the peek shows, so it is drawn with the words on it.
/// CONTROL: a labelled button cut the same way is not drawn.
#[test]
fn a_surface_cut_over_the_top_is_drawn_dead() {
    let card = |key: &str, spec: ButtonSpec| {
        UiNode::new(
            key,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .children(vec![
            UiNode::new(
                format!("{key}/frame"),
                UiContent::Button(Control::new(spec, ActionId::new(format!("{key}/frame")))),
            ),
            UiNode::new(
                format!("{key}/name"),
                UiContent::Text(TextSpec::heading(key)),
            )
            .paint_only(),
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
                .scrolled(10.0)
                .clipped(),
        )
        // -10..50: the top edge cuts the row of cards.
        .children(vec![
            UiNode::new(
                "row",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::row().height(Length::Fixed(60.0)))
            .children(vec![
                card("surface", ButtonSpec::new("A theme").visual_label("")),
                card("labelled", ButtonSpec::new("Update Now")),
            ])
            .peek_above(),
        ]),
    )
    .compile(LogicalRect::new(0.0, 0.0, 300.0, 200.0))
    .expect("compiles");
    let surface = UiKey::new("surface/frame");
    assert!(compiled_paints(&tree, &surface), "the card keeps its frame");
    assert!(compiled_paints(&tree, &UiKey::new("surface/name")));
    assert!(tree.semantic(&surface).is_none(), "and is not live");
    assert!(tree.hits.iter().all(|hit| hit.key != surface));
    assert!(
        !compiled_paints(&tree, &UiKey::new("labelled/frame")),
        "a cut button with words of its own is not drawn"
    );
}

fn compiled_paints(compiled: &crate::native_ui::CompiledUi, key: &UiKey) -> bool {
    compiled
        .paint
        .iter()
        .any(|node| node.key == *key && node.peek)
}

/// A LIST'S EDGE NEVER LEAVES A CONTROL NEARLY WHOLE: on every screen of the
/// list pages (the Themes gallery's 190pt cards among them), at the shipped
/// and common window sizes, a control the list draws whole is live, and one
/// its top or bottom edge cuts is cut through its middle or more — a peek
/// that reads as cut, never a switch with a few points gone that ignores the
/// pointer, the keyboard and a screen reader.
#[test]
fn a_list_edge_never_leaves_a_control_nearly_whole() {
    let routes = [
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
    ];
    let sizes = [
        (584.0, 348.0),
        (738.0, 418.0),
        (1_074.0, 684.0),
        (1_193.0, 516.0),
        (1_564.0, 854.0),
    ];
    for route in routes {
        for (width, height) in sizes {
            let mut lab = Lab::new("");
            lab.state_mut().navigate(route);
            let _ = lab.compile(width, height);
            let limit = lab.state().scroll_plan().map_or(0, |plan| plan.limit());
            for top in 0..=limit {
                lab.state_mut().page_scroll = top;
                lab.state_mut().page_reveal = None;
                let compiled = lab.compile(width, height);
                let Some(list) = compiled
                    .paint
                    .iter()
                    .find(|node| matches!(node.key.as_str(), "settings/list" | "settings/top/list"))
                    .map(|node| node.clip)
                else {
                    continue;
                };
                for node in &compiled.paint {
                    let control = matches!(
                        node.content,
                        UiContent::Button(_)
                            | UiContent::Switch(_)
                            | UiContent::Slider(_)
                            | UiContent::TextField(_)
                    );
                    if !control
                        || node.rect.x < list.x - 0.5
                        || node.rect.right() > list.right() + 0.5
                        || node.rect.bottom() <= list.y + 0.5
                        || node.rect.y >= list.bottom() - 0.5
                    {
                        continue;
                    }
                    let context = format!(
                        "{route:?} {width}×{height} screen {top}: {} {:?} clip {:?}",
                        node.key.as_str(),
                        node.rect,
                        node.clip
                    );
                    let cut = node.clip.y > node.rect.y + 0.5
                        || node.clip.bottom() < node.rect.bottom() - 0.5;
                    if cut {
                        assert!(
                            node.clip.height <= node.rect.height / 2.0 + 1.0,
                            "{context}: cut, but nearly whole"
                        );
                    } else {
                        assert!(
                            compiled.semantic(&node.key).is_some(),
                            "{context}: drawn whole, but dead"
                        );
                    }
                }
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
                        let mut lab = Lab::new(&format!(
                            "theme = \"{theme}\"\ncursor_trail_bloom = {bloom}\ncursor_trail_style = \"{saved}\"\n"
                        ));
                        lab.state_mut().navigate(SettingsRoute::CursorStyles);
                        let compiled = lab.compile_in(&cx);
                        let context = format!("{context} {theme} bloom={bloom} {saved}");
                        let preview = rect(&compiled, "settings/preview/cursor-styles")
                            .unwrap_or_else(|| panic!("{context}: the preview"));
                        previews.insert((preview.height * 100.0).round() as i64);
                        let list = rect(&compiled, "settings/list").expect("the list");
                        let landed = if saved == "off" || saved == prefs::DEFAULT_CURSOR_TRAIL_STYLE
                        {
                            vec!["off", prefs::DEFAULT_CURSOR_TRAIL_STYLE]
                        } else {
                            vec![saved]
                        };
                        for value in landed {
                            let row = rect(&compiled, &cursor_style_row_key(value))
                                .unwrap_or_else(|| panic!("{context}: {value} is not on landing"));
                            assert!(
                                row.y >= list.y - 0.01 && row.bottom() <= list.bottom() + 0.01,
                                "{context}: {value} {row:?} whole in {list:?}"
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
                let mut lab = Lab::new(&format!("theme = \"{theme}\"\n"));
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
/// it ends above the bar and no style row is live under it (the list used to
/// keep its full room and run 45pt under the warning at 102×29, a row there
/// cut by the bar but still live).
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
        for node in &compiled.paint {
            let key = node.key.as_str();
            if key.starts_with("settings/cursor-style/") && compiled.semantic(&node.key).is_some() {
                assert!(
                    node.rect.bottom() <= warning.y + 0.01,
                    "{width}×{height}: {key} {:?} is live under the warning {warning:?}",
                    node.rect
                );
            }
        }
    }
}
