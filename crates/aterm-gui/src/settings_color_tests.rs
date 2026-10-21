// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The colour picker: one wheel, Brightness and hex row for every colour key,
//! on the picker's own hue, saturation and brightness; the automatic
//! selected-tab colour as a colour; and the Window page's way to Tab Color.

use super::modified::{Lab, rect_of};
use super::*;

/// Every key Settings edits as a colour, from the registry.
fn color_keys() -> Vec<&'static str> {
    SettingsViewState::new(&Config::default())
        .legacy
        .fields
        .iter()
        .filter(|field| matches!(field.kind, EditKind::Color))
        .map(|field| field.key)
        .collect()
}

/// `view_cx_at` with the window's automatic selected-tab colour.
fn cx_with_tab_color(width: f32, height: f32, automatic: [u8; 3]) -> ViewCx<'static> {
    let mut cx = view_cx_at(width, height);
    cx.motion.automatic_tab_color = automatic;
    cx
}

fn wheel_of(
    compiled: &crate::native_ui::CompiledUi,
    key: &str,
) -> crate::native_ui::ColorWheelSpec {
    let wheel = format!("settings/color/{key}/wheel");
    let node = compiled
        .paint
        .iter()
        .find(|node| node.key.as_str() == wheel)
        .unwrap_or_else(|| panic!("{wheel} is painted"));
    let UiContent::ColorWheel(control) = &node.content else {
        panic!("{wheel} is a colour wheel: {:?}", node.content);
    };
    control.spec.clone()
}

/// ONE PICKER FOR EVERY COLOUR KEY: each colour row offers Choose…, and
/// Choose… opens the wheel (its own pick action), Brightness and Done under
/// the row, with the row's hex field, all on one screen and fitted. On
/// Appearance's five colour rows at the shipped sizes, and every colour key
/// in Search, the selected tab colour and the trail colours among them.
#[test]
fn every_colour_row_opens_the_one_picker() {
    let mut cases: Vec<(&'static str, Option<SettingsRoute>)> = [
        prefs::EDIT_FOREGROUND,
        prefs::EDIT_BACKGROUND,
        prefs::EDIT_CURSOR_COLOR,
        prefs::EDIT_SELECTION_COLOR,
        prefs::EDIT_SELECTION_FOREGROUND,
    ]
    .into_iter()
    .map(|key| (key, Some(SettingsRoute::Appearance)))
    .collect();
    cases.extend(color_keys().into_iter().map(|key| (key, None)));
    assert!(cases.len() >= 13, "{cases:?}");
    for (key, route) in cases {
        for (width, height) in [(1_074.0_f32, 684.0_f32), (744.0, 420.0), (584.0, 348.0)] {
            let mut lab = Lab::new("");
            let label = lab.state().field_by_key(key).unwrap().label;
            match route {
                Some(route) => lab.state_mut().navigate(route),
                None => lab.state_mut().set_search(label.to_lowercase()),
            }
            let at = format!("{key} on {route:?} at {width}×{height}");
            let opener = format!("settings/color-open/{key}");
            lab.state_mut()
                .reveal_control(UiKey::new(format!("settings/control/{key}")));
            let compiled = lab.compile(width, height);
            assert!(
                compiled.semantic(&UiKey::new(opener.as_str())).is_some(),
                "{at}: the row offers Choose…"
            );
            lab.dispatch(action(&opener, None));
            let compiled = lab.compile(width, height);
            let wheel = compiled
                .semantic(&UiKey::new(format!("settings/color/{key}/wheel")))
                .unwrap_or_else(|| panic!("{at}: the wheel opens"));
            assert_eq!(
                wheel.action.as_ref().map(ActionId::as_str),
                Some(format!("settings/color-pick/{key}").as_str()),
                "{at}: the wheel picks for its own key"
            );
            assert!(
                compiled
                    .semantic(&UiKey::new(format!("settings/color/{key}/brightness")))
                    .is_some(),
                "{at}: Brightness is in the picker"
            );
            assert_eq!(
                compiled
                    .semantic(&UiKey::new(opener.as_str()))
                    .map(|node| node.label.clone()),
                Some(format!(
                    "Done choosing the {}",
                    color_picker_noun_for_test(label)
                )),
                "{at}: Choose… reads Done while the picker is open"
            );
            let wheel_rect = rect_of(&compiled, &format!("settings/color/{key}/wheel"));
            let control = rect_of(&compiled, &format!("settings/control/{key}"));
            assert!(
                wheel_rect.y >= control.bottom(),
                "{at}: the picker is under its row"
            );
            assert_eq!(
                lab.state().common.last_focus.as_ref().map(UiKey::as_str),
                Some(format!("settings/color/{key}/wheel").as_str()),
                "{at}: the keyboard goes into the picker"
            );
            let failures: Vec<_> = compiled
                .fit_failures()
                .into_iter()
                .filter(|line| line.contains(&format!("settings/color/{key}/")))
                .collect();
            assert!(failures.is_empty(), "{at}: {failures:?}");
            // Choose… again closes it.
            lab.dispatch(action(&opener, None));
            assert!(
                lab.compile(width, height)
                    .semantic(&UiKey::new(format!("settings/color/{key}/wheel")))
                    .is_none(),
                "{at}: Choose… closes the picker it opened"
            );
        }
    }
}

/// A pick on Foreground's wheel writes Foreground, at the brightness the
/// picker keeps; Brightness keeps the hue. The selected tab colour, whose
/// page the wheel was lifted from, is untouched.
#[test]
fn the_foreground_wheel_and_brightness_write_foreground() {
    let mut lab = Lab::new("foreground = \"#FFAA00\"\n");
    lab.state_mut().navigate(SettingsRoute::Appearance);
    lab.dispatch(action("settings/color-open/foreground", None));
    let pick = lab.commit(
        "settings/color-pick/foreground",
        Some(SemanticInput::ColorPick {
            hue: 0.25,
            saturation: 1.0,
        }),
    );
    assert_eq!(
        pick.edits
            .iter()
            .map(|edit| (edit.key.as_str(), edit.value.as_deref()))
            .collect::<Vec<_>>(),
        vec![("foreground", Some("#80FF00"))]
    );
    assert!(!lab.text().contains("active_tab_color"));
    let mut lab = Lab::new("foreground = \"#FFAA00\"\n");
    let darker = lab.commit(
        "settings/color-brightness/foreground",
        Some(SemanticInput::Number(0.5)),
    );
    assert_eq!(darker.edits[0].value.as_deref(), Some("#805500"));
    // Unset, Brightness starts from the theme's own foreground: half of
    // Default's #D0D0D0 is #686868.
    let mut lab = Lab::new("");
    let half = lab.commit(
        "settings/color-brightness/foreground",
        Some(SemanticInput::Number(0.4)),
    );
    let theme = color_picker::color_now(&SettingsViewState::new(&Config::default()), "foreground")
        .expect("an unset foreground is the theme's");
    let (hue, saturation, _) = crate::widget::rgb_to_hsv(theme);
    let expected = crate::widget::hsv_to_rgb(hue, saturation, 0.4);
    assert_eq!(
        half.edits[0].value.as_deref(),
        Some(format!("#{:02X}{:02X}{:02X}", expected[0], expected[1], expected[2]).as_str())
    );
}

fn color_picker_noun_for_test(label: &str) -> String {
    let lower = label.to_lowercase();
    if lower.ends_with("color") {
        lower
    } else {
        format!("{lower} color")
    }
}

/// Escape and Done (Choose… while open) close the picker, the keyboard
/// back on Choose…, and the page stays.
#[test]
fn escape_and_done_close_the_picker_back_on_choose() {
    for close in [
        AppEvent::TextInput(TextInputEvent::Cancel),
        action("settings/color-open/foreground", None),
    ] {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Appearance);
        lab.dispatch(action("settings/color-open/foreground", None));
        assert_eq!(lab.state().open_color_picker(), Some("foreground"));
        lab.dispatch(close);
        assert_eq!(lab.state().open_color_picker(), None);
        assert_eq!(
            lab.state().common.last_focus.as_ref().map(UiKey::as_str),
            Some("settings/color-open/foreground")
        );
        assert_eq!(
            lab.state().route,
            SettingsRoute::Appearance,
            "Escape closes the picker, not the page"
        );
    }
}

/// BLACK KEEPS ITS HUE: brightness 0 writes black, and raising it again
/// gives the hue it had, not a grey; the wheel marks that hue on its black
/// disk. A round trip through a dim brightness gives the colour back exactly,
/// and a colour the picker made keeps its hue when it comes back (as an Undo
/// brings it). Negative control: a black the picker never made raises to
/// grey.
#[test]
fn brightness_zero_keeps_the_picked_hue() {
    let set = |lab: &mut Lab, brightness: f64| {
        lab.commit(
            "settings/color-brightness/active_tab_color",
            Some(SemanticInput::Number(brightness)),
        )
        .edits[0]
            .value
            .clone()
            .unwrap_or_default()
    };
    let mut lab = Lab::new("active_tab_color = \"#FF8800\"\n");
    assert_eq!(set(&mut lab, 0.0), "#000000");
    lab.state_mut().navigate(SettingsRoute::TabColor);
    let wheel = wheel_of(&lab.compile(1_074.0, 684.0), prefs::EDIT_ACTIVE_TAB_COLOR);
    let [hue, saturation, value] = wheel.hsv.expect("the wheel has a colour");
    assert!((hue - 0.0889).abs() < 1e-3, "the kept hue: {hue}");
    assert_eq!((saturation, value), (1.0, 0.0));
    assert_eq!(set(&mut lab, 0.6), "#995200", "the hue comes back");
    // Exact round trip through 5%.
    let mut lab = Lab::new("active_tab_color = \"#FFCC99\"\n");
    let _ = set(&mut lab, 0.05);
    assert_eq!(set(&mut lab, 1.0), "#FFCC99");
    // Black the picker made, brought back another way, keeps its hue.
    let mut lab = Lab::new("active_tab_color = \"#FF8800\"\n");
    let _ = set(&mut lab, 0.0);
    let _ = set(&mut lab, 1.0);
    lab.commit(
        "settings/set/active_tab_color",
        Some(SemanticInput::Text("#000000".to_string())),
    );
    assert_eq!(set(&mut lab, 1.0), "#FF8800");
    // Negative control: a black the picker never made has no hue to keep.
    let mut lab = Lab::new("active_tab_color = \"#000000\"\n");
    assert_eq!(set(&mut lab, 0.6), "#999999");
    // A pick on a black wheel keeps its hue for the next brightness.
    let mut lab = Lab::new("active_tab_color = \"#000000\"\n");
    assert!(
        lab.invoke(
            "settings/color-pick/active_tab_color",
            Some(SemanticInput::ColorPick {
                hue: 0.25,
                saturation: 1.0,
            }),
        )
        .is_none(),
        "black stays black: the pixels showed black"
    );
    assert_eq!(set(&mut lab, 1.0), "#80FF00");
}

/// A PICK KEEPS THE WHEEL IN VIEW: on a short stacked Tab Color page the
/// wheel and the hex row are different stops, and a pick used to reveal the
/// hex row and scroll the wheel out from under the pointer. Negative
/// control: the hex row's own set still brings the hex row into view.
#[test]
fn a_wheel_pick_keeps_the_wheel_in_view_on_a_stacked_page() {
    let wheel_key = "settings/color/active_tab_color/wheel";
    for (width, height) in [(444.0_f32, 292.0_f32), (444.0, 320.0)] {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::TabColor);
        let compiled = lab.compile(width, height);
        let action = compiled
            .semantic(&UiKey::new(wheel_key))
            .and_then(|wheel| wheel.action.clone())
            .expect("the wheel takes a pick");
        assert_eq!(action.as_str(), "settings/color-pick/active_tab_color");
        lab.commit(
            action.as_str(),
            Some(SemanticInput::Text("#3366CC".to_string())),
        );
        assert!(lab.text().contains("active_tab_color = \"#3366CC\""));
        let compiled = lab.compile(width, height);
        let list = rect_of(&compiled, "settings/tab-color/list");
        let wheel = compiled
            .semantic(&UiKey::new(wheel_key))
            .unwrap_or_else(|| panic!("{width}×{height}: the wheel stays after a pick"))
            .rect;
        assert!(
            wheel.y < list.bottom() && wheel.bottom() > list.y,
            "{width}×{height}: the wheel is in the list's view: {wheel:?} in {list:?}"
        );
        assert_eq!(
            lab.state().common.last_focus.as_ref().map(UiKey::as_str),
            Some(wheel_key)
        );
        // Negative control: the hex row's own action reveals the hex row.
        lab.commit(
            "settings/set/active_tab_color",
            Some(SemanticInput::Text("#CC6633".to_string())),
        );
        assert_eq!(
            lab.state().page_reveal.as_ref().map(UiKey::as_str),
            Some("settings/control/active_tab_color")
        );
    }
}

/// THE AUTOMATIC SELECTED-TAB COLOUR IS A COLOUR: unset, Tab Color's hex
/// field, the wheel and the Window page's way to Tab Color all show the
/// colour the window paints, and follow it; a set colour shows itself.
#[test]
fn the_automatic_tab_color_is_shown_where_it_is_named() {
    let automatic = [0x3A, 0x3A, 0x3E];
    let other = [0x0A, 0x64, 0xD8];
    for theirs in [automatic, other] {
        let mut lab = Lab::new("");
        lab.state_mut().navigate(SettingsRoute::TabColor);
        let compiled = lab.compile_in(&cx_with_tab_color(1_074.0, 684.0, theirs));
        let field = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == "settings/control/active_tab_color")
            .expect("the hex field");
        let UiContent::TextField(control) = &field.content else {
            panic!("a text field")
        };
        assert_eq!(control.spec.swatch, Some(theirs), "the field's chip");
        let [h, s, v] = wheel_of(&compiled, prefs::EDIT_ACTIVE_TAB_COLOR)
            .hsv
            .expect("the wheel marks the automatic colour");
        assert_eq!(crate::widget::hsv_to_rgb(h, s, v), theirs);
        lab.state_mut().navigate(SettingsRoute::WindowTabs);
        lab.state_mut()
            .reveal_control(UiKey::new("settings/control/active_tab_color"));
        let compiled = lab.compile_in(&cx_with_tab_color(1_074.0, 684.0, theirs));
        let link = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == "settings/control/active_tab_color")
            .expect("the Window row");
        let UiContent::Button(button) = &link.content else {
            panic!("the Window row is a way to Tab Color")
        };
        assert_eq!(button.spec.chip, Some(theirs), "the link's chip");
        assert!(
            button.spec.label.contains(&format!(
                "#{:02X}{:02X}{:02X}",
                theirs[0], theirs[1], theirs[2]
            )),
            "the colour is in words too: {}",
            button.spec.label
        );
    }
    let mut lab = Lab::new("active_tab_color = \"#3366CC\"\n");
    lab.state_mut().navigate(SettingsRoute::WindowTabs);
    lab.state_mut()
        .reveal_control(UiKey::new("settings/control/active_tab_color"));
    let compiled = lab.compile_in(&cx_with_tab_color(1_074.0, 684.0, automatic));
    let link = compiled
        .paint
        .iter()
        .find(|node| node.key.as_str() == "settings/control/active_tab_color")
        .expect("the Window row");
    let UiContent::Button(button) = &link.content else {
        panic!("a button")
    };
    assert_eq!(button.spec.chip, Some([0x33, 0x66, 0xCC]));
}

/// A REFUSED DRAFT IS CORRECTABLE WHEREVER IT SHOWS: the Window page's row
/// for the selected tab colour keeps its field (with Choose… and Reset)
/// while a draft is held, so its tag, its reason and the bar's "Correct it"
/// are true there; a saved value turns it back into the way to Tab Color.
#[test]
fn the_window_tab_color_row_keeps_a_refused_draft_correctable() {
    let key = "settings/control/active_tab_color";
    for (width, height) in [(1_074.0_f32, 684.0_f32), (738.0, 420.0), (584.0, 348.0)] {
        let mut lab = Lab::new("active_tab_color = \"#3366CC\"\n");
        lab.state_mut().navigate(SettingsRoute::TabColor);
        assert!(
            lab.invoke(
                "settings/set/active_tab_color",
                Some(SemanticInput::Text("orange".to_string())),
            )
            .is_none(),
            "orange is refused"
        );
        lab.state_mut().navigate(SettingsRoute::WindowTabs);
        lab.state_mut().reveal_control(UiKey::new(key));
        let compiled = lab.compile(width, height);
        let at = format!("{width}×{height}");
        let field = compiled
            .semantic(&UiKey::new(key))
            .unwrap_or_else(|| panic!("{at}: the row"));
        assert_eq!(field.role, SemanticRole::TextField, "{at}: the field stays");
        assert_eq!(
            field.value,
            SemanticValue::Text("orange".to_string()),
            "{at}"
        );
        assert!(
            compiled
                .semantic(&UiKey::new("settings/error/active_tab_color"))
                .is_some(),
            "{at}: the reason"
        );
        let plan = lab.state().scroll_plan().expect("a list");
        let order: Vec<String> = plan
            .controls()
            .map(|(_, key)| key.as_str().to_string())
            .collect();
        let at_field = order.iter().position(|k| k == key).expect("in order");
        assert_eq!(
            order.get(at_field + 1).map(String::as_str),
            Some("settings/color-open/active_tab_color"),
            "{at}"
        );
        assert_eq!(
            order.get(at_field + 2).map(String::as_str),
            Some("settings/reset/active_tab_color"),
            "{at}"
        );
        lab.commit(
            "settings/set/active_tab_color",
            Some(SemanticInput::Text("#FFAA00".to_string())),
        );
        let compiled = lab.compile(width, height);
        assert_eq!(
            compiled.semantic(&UiKey::new(key)).map(|node| node.role),
            Some(SemanticRole::Button),
            "{at}: saved, the row is the way to Tab Color again"
        );
        assert!(
            compiled
                .semantic(&UiKey::new("settings/color-open/active_tab_color"))
                .is_none(),
            "{at}"
        );
    }
}

/// The arrow keys work the wheel: Left and Right turn its hue, Up and Down
/// change its saturation, the brightness kept.
#[test]
fn the_arrow_keys_turn_the_wheel() {
    let mut lab = Lab::new("foreground = \"#FF0000\"\n");
    lab.state_mut().navigate(SettingsRoute::Appearance);
    lab.dispatch(action("settings/color-open/foreground", None));
    let right = lab.commit_event(AppEvent::TextInput(TextInputEvent::Right { extend: true }));
    // A twelfth of a turn from red is orange.
    assert_eq!(right.edits[0].value.as_deref(), Some("#FF8000"));
    let down = lab.commit_event(AppEvent::StepFocus(1));
    assert_eq!(down.edits[0].value.as_deref(), Some("#FF860D"));
    assert_eq!(
        lab.state().common.last_focus.as_ref().map(UiKey::as_str),
        Some("settings/color/foreground/wheel"),
        "the keyboard stays on the wheel"
    );
}

/// `#RRGGBB` of what the picker makes from `hsv`.
fn hex_of_hsv([hue, saturation, value]: [f32; 3]) -> String {
    let [r, g, b] = crate::widget::hsv_to_rgb(hue, saturation, value);
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// THEMES' CUSTOMIZE SHEET OPENS THE SAME PICKER: each colour row wears the
/// colour in force on a well beside its field, and the well opens the wheel
/// and Brightness under the row (Done while open), fitted in the sheet's
/// column; a pick writes that key and no other. Negative control: before
/// the well is pressed no wheel is drawn.
#[test]
fn the_customize_sheet_opens_the_one_picker() {
    for (width, height) in [
        (1_284.0_f32, 934.0_f32),
        (1_074.0, 684.0),
        (738.0, 420.0),
        (584.0, 348.0),
    ] {
        let at = format!("{width}×{height}");
        let mut lab = Lab::new("theme = \"Dracula\"\nforeground = \"#FFAA00\"\n");
        lab.state_mut().navigate(SettingsRoute::Themes);
        lab.dispatch(action("settings/showcase/customize", None));
        let compiled = lab.compile(width, height);
        // Each field is wide enough for what it holds beside its well (an
        // upper-case hex, "Automatic"); a row the list's edge cuts is the
        // list's to scroll.
        let cut: Vec<_> = compiled
            .fit_failures()
            .into_iter()
            .filter(|line| {
                line.contains("paint-text key=\"settings/control/")
                    && line.contains("clip-truncated=false")
            })
            .collect();
        assert!(cut.is_empty(), "{at}: {cut:?}");
        for key in showcase_page::SHOWCASE_COLOR_KEYS {
            assert!(
                compiled
                    .semantic(&UiKey::new(format!("settings/color-open/{key}")))
                    .is_some(),
                "{at}: {key}'s row has its well"
            );
            // The well is the chip at the field's end, laid over it: it costs
            // the row's name no width.
            let field = rect_of(&compiled, &format!("settings/control/{key}"));
            let well = rect_of(&compiled, &format!("settings/color-open/{key}"));
            assert!(
                well.x >= field.x
                    && well.right() <= field.right()
                    && well.y >= field.y
                    && well.bottom() <= field.bottom(),
                "{at}: {key}'s well {well:?} lies over its field {field:?}"
            );
            // Wherever it is drawn it shows: the colour's chip, or, with no
            // colour to show, the colours mark.
            let blank = compiled.paint.iter().any(|node| {
                node.key.as_str() == format!("settings/color-open/{key}")
                    && matches!(&node.content, UiContent::Button(button)
                        if button.spec.chip.is_none() && button.spec.visual_icon.is_none())
            });
            assert!(!blank, "{at}: {key}'s well paints nothing");
        }
        // So the names keep their room: where the sheet's column holds a
        // name beside a hex-wide field and Reset, Background and Selection
        // paint whole, never "Select…" twice (as they did beside a well that
        // took a field's room of its own). Those widths were measured in
        // macOS's UI face; a wider one (Noto Sans) needs a wider sheet, and
        // short of it the one-line row elides its name as every one does
        // (`colour_rows_are_one_line_with_one_field_column`).
        if width >= 1_074.0 && cfg!(target_os = "macos") {
            for (key, name) in [
                (prefs::EDIT_BACKGROUND, "Background"),
                (prefs::EDIT_SELECTION_COLOR, "Selection"),
            ] {
                let painted = compiled
                    .paint
                    .iter()
                    .find_map(|node| match &node.content {
                        UiContent::Text(text)
                            if node.key.as_str() == format!("settings/label/{key}") =>
                        {
                            Some(text.text.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("{at}: {key}'s name"));
                assert_eq!(painted, name, "{at}");
            }
        }
        assert!(
            compiled
                .semantic(&UiKey::new("settings/color/background/wheel"))
                .is_none(),
            "{at}: no wheel until the well is pressed"
        );
        lab.dispatch(action("settings/color-open/background", None));
        let compiled = lab.compile(width, height);
        let wheel = compiled
            .semantic(&UiKey::new("settings/color/background/wheel"))
            .unwrap_or_else(|| panic!("{at}: the wheel opens"));
        assert_eq!(
            wheel.action.as_ref().map(ActionId::as_str),
            Some("settings/color-pick/background"),
            "{at}"
        );
        let well = rect_of(&compiled, "settings/color-open/background");
        let wheel_rect = rect_of(&compiled, "settings/color/background/wheel");
        assert!(wheel_rect.y >= well.bottom(), "{at}: under the row");
        assert!(
            compiled
                .semantic(&UiKey::new("settings/color/background/brightness"))
                .is_some(),
            "{at}: Brightness"
        );
        let failures: Vec<_> = compiled
            .fit_failures()
            .into_iter()
            .filter(|line| line.contains("settings/color"))
            .collect();
        assert!(failures.is_empty(), "{at}: {failures:?}");
        let pick = lab.commit(
            "settings/color-pick/background",
            Some(SemanticInput::ColorPick {
                hue: 0.5,
                saturation: 1.0,
            }),
        );
        let dracula = aterm_types::scheme::builtin("Dracula").expect("built-in");
        let bg = dracula.background;
        let (_, _, value) = crate::widget::rgb_to_hsv([bg.r, bg.g, bg.b]);
        assert_eq!(
            pick.edits
                .iter()
                .map(|edit| (edit.key.as_str(), edit.value.clone()))
                .collect::<Vec<_>>(),
            vec![("background", Some(hex_of_hsv([0.5, 1.0, value])))],
            "{at}"
        );
    }
    // A colour row with no colour to show (the default theme leaves the
    // selected text automatic): its well wears the colours mark and no chip,
    // and the field keeps no chip either. Negative control: a row with a
    // colour wears its chip and no mark.
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Themes);
    lab.dispatch(action("settings/showcase/customize", None));
    let compiled = lab.compile(1_284.0, 934.0);
    let well = |key: &str| {
        compiled
            .paint
            .iter()
            .find_map(|node| match &node.content {
                UiContent::Button(button)
                    if node.key.as_str() == format!("settings/color-open/{key}") =>
                {
                    Some((button.spec.chip, button.spec.visual_icon))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{key}'s well"))
    };
    assert_eq!(
        well(prefs::EDIT_SELECTION_FOREGROUND),
        (None, Some(crate::native_ui::ButtonIcon::Themes))
    );
    let (chip, icon) = well(prefs::EDIT_BACKGROUND);
    assert!(chip.is_some() && icon.is_none());
}

/// AN OPEN CUSTOMIZE PICKER IS WHOLE ON ITS SCREEN: opened from any colour
/// row of Themes' Customize sheet, its wheel and Brightness are drawn whole
/// (no edge of the list cuts them) on the screen the list shows, and the
/// wheel takes a pick, from 80x24 (where COLOURS repeated over the picker
/// would leave it a sliver the list's edge cuts) to a wide window. Negative
/// control: before the well is pressed neither is drawn.
#[test]
fn an_open_customize_picker_is_whole_on_its_screen() {
    let open_at = |key: &str, width: f32, height: f32| {
        let mut lab = Lab::new("theme = \"Dracula\"\n");
        lab.state_mut().navigate(SettingsRoute::Themes);
        lab.dispatch(action("settings/showcase/customize", None));
        let closed = lab.compile(width, height);
        lab.dispatch(action(&format!("settings/color-open/{key}"), None));
        (closed, lab.compile(width, height))
    };
    // Whether `key` paints, and every paint of it lies inside its clip.
    let whole = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        let mut painted = compiled
            .paint
            .iter()
            .filter(|node| node.key.as_str() == key)
            .peekable();
        painted.peek().is_some()
            && painted.all(|node| {
                node.rect.y >= node.clip.y - 0.5 && node.rect.bottom() <= node.clip.bottom() + 0.5
            })
    };
    for key in showcase_page::SHOWCASE_COLOR_KEYS {
        let wheel = format!("settings/color/{key}/wheel");
        let brightness = format!("settings/color/{key}/brightness");
        for (width, height) in [(584.0_f32, 348.0_f32), (738.0, 420.0), (1_074.0, 684.0)] {
            let at = format!("{width}×{height} {key}");
            let (closed, open) = open_at(key, width, height);
            assert!(
                closed
                    .paint
                    .iter()
                    .all(|node| node.key.as_str() != wheel && node.key.as_str() != brightness),
                "{at}: no picker until the well is pressed"
            );
            assert!(whole(&open, &wheel), "{at}: the wheel is drawn whole");
            assert!(whole(&open, &brightness), "{at}: Brightness is drawn whole");
            assert!(
                open.hits.iter().any(|hit| hit.key.as_str() == wheel),
                "{at}: the wheel takes a pick"
            );
        }
    }
}

/// THE CUSTOMIZE SHEET KEEPS ITS LEAST under the demo: in the chooser's
/// place, its list holds COLOURS over one whole colour row, on a window whose
/// compact toolbar names the page (no heading over the demo) as on any
/// other. The demo gives up whole rows for it as it does for the chooser.
/// (Sized by the chooser's least, the demo kept rows that left the sheet a
/// caption over a row the list's edge cut, 20 of its 32pt at 80x24.)
#[test]
fn the_customize_sheet_keeps_its_caption_over_a_whole_row() {
    // Every paint of `key` lies inside its clip, and it paints.
    let whole = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        let mut painted = compiled
            .paint
            .iter()
            .filter(|node| node.key.as_str() == key)
            .peekable();
        painted.peek().is_some()
            && painted.all(|node| {
                node.rect.y >= node.clip.y - 0.5 && node.rect.bottom() <= node.clip.bottom() + 0.5
            })
    };
    for (width, height) in [(584.0_f32, 348.0_f32), (738.0, 420.0), (1_074.0, 684.0)] {
        let at = format!("{width}×{height}");
        let mut lab = Lab::new("theme = \"Dracula\"\n");
        lab.state_mut().navigate(SettingsRoute::Themes);
        lab.dispatch(action("settings/showcase/customize", None));
        let compiled = lab.compile(width, height);
        assert!(
            whole(&compiled, "settings/showcase/colours/heading"),
            "{at}: COLOURS is drawn whole"
        );
        let row = compiled
            .paint
            .iter()
            .find(|node| {
                let key = node.key.as_str();
                key.starts_with("settings/row/") && key.ends_with("/foreground")
            })
            .map(|node| node.key.as_str().to_string())
            .unwrap_or_else(|| panic!("{at}: the first colour row paints"));
        assert!(
            whole(&compiled, &row),
            "{at}: {row} is drawn whole under COLOURS"
        );
    }
}

/// THE PALETTE SLOT'S EDITOR CARRIES THE PICKER: the open slot's wheel and
/// Brightness set its draft (the demo draws it), on the picker's own model
/// (brightness 0 and back keeps the hue), and closing the editor saves the
/// slot. Negative control: no slot open, no slot picker.
#[test]
fn the_palette_slot_editor_carries_the_picker() {
    let mut lab = Lab::new("theme = \"Dracula\"\n");
    lab.state_mut().navigate(SettingsRoute::Themes);
    lab.dispatch(action("settings/showcase/customize", None));
    let wheel = "settings/color/palette-slot/wheel";
    assert!(
        lab.compile(1_284.0, 934.0)
            .semantic(&UiKey::new(wheel))
            .is_none(),
        "no slot open, no slot picker"
    );
    lab.dispatch(action("settings/showcase/palette/1", None));
    lab.state_mut().reveal_control(UiKey::new(wheel));
    let compiled = lab.compile(1_284.0, 934.0);
    let node = compiled
        .semantic(&UiKey::new(wheel))
        .expect("the open slot's wheel");
    assert_eq!(
        node.action.as_ref().map(ActionId::as_str),
        Some("settings/color-pick/palette-slot")
    );
    assert!(
        compiled
            .semantic(&UiKey::new("settings/color/palette-slot/brightness"))
            .is_some()
    );
    let draft = |lab: &Lab| {
        lab.state()
            .showcase
            .slot
            .as_ref()
            .map(|slot| slot.input.value().to_string())
    };
    assert!(
        lab.invoke(
            "settings/color-pick/palette-slot",
            Some(SemanticInput::ColorPick {
                hue: 0.25,
                saturation: 1.0,
            }),
        )
        .is_none(),
        "a pick is the slot's draft, not a save"
    );
    let dracula = aterm_types::scheme::builtin("Dracula").expect("built-in");
    let red = dracula.ansi[1];
    let (_, _, value) = crate::widget::rgb_to_hsv([red.r, red.g, red.b]);
    let picked = hex_of_hsv([0.25, 1.0, value]);
    let draft = |lab: &Lab| draft(lab).map(|text| text.to_ascii_uppercase());
    assert_eq!(draft(&lab), Some(picked));
    let _ = lab.invoke(
        "settings/color-brightness/palette-slot",
        Some(SemanticInput::Number(0.0)),
    );
    assert_eq!(draft(&lab).as_deref(), Some("#000000"));
    let _ = lab.invoke(
        "settings/color-brightness/palette-slot",
        Some(SemanticInput::Number(0.6)),
    );
    let lifted = hex_of_hsv([0.25, 1.0, 0.6]);
    assert_eq!(draft(&lab), Some(lifted.clone()), "black kept its hue");
    let saved = lab.commit("settings/showcase/palette-slot-close", None);
    assert!(
        saved.edits.iter().any(|edit| edit
            .value
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(&lifted))
            && edit.key.contains("palette")),
        "closing the editor saves the slot: {:?}",
        saved.edits
    );
}

/// THE CURSOR STYLES PAGE'S TRAIL COLOURS OPEN THE PICKER: the saved style's
/// Adjust card lists Trail color with Choose…, which opens the wheel under
/// the row; a pick writes the trail colour.
#[test]
fn the_cursor_styles_trail_colour_opens_the_picker() {
    let key = prefs::EDIT_CURSOR_TRAIL_COLOR;
    let mut lab = Lab::new("cursor_trail_style = \"laser\"\n");
    lab.state_mut().navigate(SettingsRoute::CursorStyles);
    lab.state_mut()
        .reveal_control(UiKey::new(format!("settings/control/{key}")));
    let compiled = lab.compile(1_074.0, 684.0);
    let opener = format!("settings/color-open/{key}");
    assert!(
        compiled.semantic(&UiKey::new(opener.as_str())).is_some(),
        "Trail color offers Choose…"
    );
    lab.dispatch(action(&opener, None));
    let compiled = lab.compile(1_074.0, 684.0);
    let wheel = rect_of(&compiled, &format!("settings/color/{key}/wheel"));
    let field = rect_of(&compiled, &format!("settings/control/{key}"));
    assert!(wheel.y >= field.bottom(), "the wheel is under the row");
    let pick = lab.commit(
        &format!("settings/color-pick/{key}"),
        Some(SemanticInput::Text("#3366CC".to_string())),
    );
    assert_eq!(pick.edits[0].key, key);
    assert_eq!(pick.edits[0].value.as_deref(), Some("#3366CC"));
}

/// THE ADJUST CARD'S PICKER FITS ITS LIST: the trail colour's row and its
/// open picker are one stop of the Adjust card, under the card's caption and
/// padding, so the picker is sized against the list less that chrome. At the
/// shipped default (102x29) and at 80x24 nothing on the page is cut or
/// overflows. Negative control: with the picker closed the page fits too, and
/// with it open the wheel is on screen.
#[test]
fn the_adjust_cards_picker_fits_its_list() {
    let key = prefs::EDIT_CURSOR_TRAIL_COLOR;
    for (width, height) in [(738.0_f32, 418.0_f32), (584.0, 348.0)] {
        let mut lab = Lab::new("cursor_trail_style = \"laser\"\n");
        lab.state_mut().navigate(SettingsRoute::CursorStyles);
        let closed = lab.compile(width, height);
        assert!(
            closed.fit_failures().is_empty(),
            "{width}×{height} closed: {:?}",
            closed.fit_failures()
        );
        lab.dispatch(action(&format!("settings/color-open/{key}"), None));
        let open = lab.compile(width, height);
        assert!(
            open.semantic(&UiKey::new(format!("settings/color/{key}/wheel")))
                .is_some(),
            "{width}×{height}: the wheel is on screen"
        );
        assert!(
            open.fit_failures().is_empty(),
            "{width}×{height} open: {:?}",
            open.fit_failures()
        );
    }
}

/// THE OPEN PICKER FITS AT EVERY WIDTH AND TEXT SIZE: a colour row's open
/// picker (the wheel, Brightness and its label, Done) and the Tab Color page
/// are never cut and never overflow, from a phone's width to a window's, at
/// 1× and 2× text. Brightness beside its label takes only what the label
/// leaves it (it kept 375pt at 2× in a 268pt column), the picker's rows are
/// measured inside its card's padding, and Done on a line of its own is never
/// wider than the column (141pt in 136 at 2× on a phone).
#[test]
fn the_open_picker_fits_at_every_width_and_text_size() {
    at_each_text_scale(&[1.0, 2.0], "colour picker fit", |scale| {
        let key = prefs::EDIT_FOREGROUND;
        for (width, height) in [
            (192.0_f32, 432.0_f32),
            (240.0, 432.0),
            (336.0, 432.0),
            (432.0, 432.0),
            (480.0, 432.0),
            (584.0, 348.0),
            (738.0, 418.0),
        ] {
            let at = format!("{width}×{height} at {scale}×");
            let mut lab = Lab::new("");
            lab.state_mut().navigate(SettingsRoute::Appearance);
            lab.state_mut()
                .reveal_control(UiKey::new(format!("settings/control/{key}")));
            lab.compile(width, height);
            lab.dispatch(action(&format!("settings/color-open/{key}"), None));
            let open = lab.compile(width, height);
            assert!(
                open.semantic(&UiKey::new(format!("settings/color/{key}/wheel")))
                    .is_some(),
                "{at}: the wheel opens"
            );
            let failures: Vec<_> = open
                .fit_failures()
                .into_iter()
                .filter(|line| line.contains("settings/color"))
                .collect();
            assert!(failures.is_empty(), "{at}: {failures:?}");
            let mut lab = Lab::new("");
            lab.state_mut().navigate(SettingsRoute::TabColor);
            let page = lab.compile(width, height);
            assert!(
                page.fit_failures().is_empty(),
                "{at} on Tab Color: {:?}",
                page.fit_failures()
            );
        }
    });
}

/// MODIFIED NAMES A COLOUR ROW'S DEFAULT WITH ITS COLOUR: "Default:
/// Automatic" wears the automatic selected-tab colour the window paints,
/// right after the words, and the row's name says it in words; a theme
/// colour's default wears the theme's. Negative control: another automatic
/// colour moves the chip; a row that is not a colour wears none.
#[test]
fn modified_names_a_colour_default_with_its_colour() {
    let chip_of = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == format!("settings/label/{key}/chip"))
            .and_then(|node| match &node.content {
                UiContent::Custom(spec) => spec.value.clone(),
                _ => None,
            })
    };
    for automatic in [[0x3A, 0x3A, 0x3E], [0x0A, 0x64, 0xD8]] {
        let mut lab =
            Lab::new("active_tab_color = \"#3366CC\"\nforeground = \"#FFAA00\"\nfont_size = 15\n");
        lab.state_mut().navigate(SettingsRoute::Modified);
        let compiled = lab.compile_in(&cx_with_tab_color(1_200.0, 820.0, automatic));
        let hex = format!(
            "#{:02X}{:02X}{:02X}",
            automatic[0], automatic[1], automatic[2]
        );
        assert_eq!(
            chip_of(&compiled, prefs::EDIT_ACTIVE_TAB_COLOR),
            Some(hex.clone())
        );
        let label = compiled
            .semantic(&UiKey::new("settings/label/active_tab_color"))
            .expect("the row's name")
            .label
            .clone();
        assert!(
            label.contains("Default: Automatic") && label.contains(&hex),
            "{label}"
        );
        let painted = |key: &str| {
            compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == key)
                .map(|node| node.rect)
                .unwrap_or_else(|| panic!("{key} is painted"))
        };
        // The label's (last) painted line: the words the chip follows.
        let words = painted("settings/label/active_tab_color/visual/line-0");
        let chip = painted("settings/label/active_tab_color/chip");
        assert!(
            (chip.x - words.right()).abs() < 0.5,
            "the chip follows the words: {words:?} {chip:?}"
        );
        let theme =
            color_picker::color_now(&SettingsViewState::new(&Config::default()), "foreground")
                .expect("the theme's foreground");
        assert_eq!(
            chip_of(&compiled, prefs::EDIT_FOREGROUND),
            Some(format!("#{:02X}{:02X}{:02X}", theme[0], theme[1], theme[2]))
        );
        assert_eq!(chip_of(&compiled, "font_size"), None, "not a colour");
        // Compact, where the name cannot hold the default: the chip follows
        // the words on the row's second line.
        let mut lab = Lab::new("foreground = \"#FFAA00\"\n");
        lab.state_mut().navigate(SettingsRoute::Modified);
        let compiled = lab.compile_in(&cx_with_tab_color(584.0, 348.0, automatic));
        let chip = compiled
            .paint
            .iter()
            .find(|node| {
                node.key.as_str() == "settings/label/foreground/chip"
                    || node.key.as_str() == "settings/detail/foreground/chip"
            })
            .and_then(|node| match &node.content {
                UiContent::Custom(spec) => spec.value.clone(),
                _ => None,
            });
        assert_eq!(
            chip,
            Some(format!("#{:02X}{:02X}{:02X}", theme[0], theme[1], theme[2])),
            "compact: the default's chip is drawn where its words are"
        );
    }
}

/// MODIFIED'S COLOUR ROWS OPEN THE PICKER TOO: a changed colour, the
/// selected tab's among them, offers Choose… beside its field and ahead of
/// its Reset, and opens the wheel under the row, at the regular and compact
/// widths, fitted.
#[test]
fn modifieds_colour_rows_open_the_picker() {
    for key in [prefs::EDIT_FOREGROUND, prefs::EDIT_ACTIVE_TAB_COLOR] {
        for (width, height) in [(1_074.0_f32, 684.0_f32), (584.0, 348.0)] {
            let at = format!("{key} at {width}×{height}");
            let mut lab = Lab::new(&format!("{key} = \"#3366CC\"\n"));
            lab.state_mut().navigate(SettingsRoute::Modified);
            lab.state_mut()
                .reveal_control(UiKey::new(format!("settings/control/{key}")));
            let compiled = lab.compile(width, height);
            let opener = format!("settings/color-open/{key}");
            assert!(
                compiled.semantic(&UiKey::new(opener.as_str())).is_some(),
                "{at}: Choose…"
            );
            let order: Vec<String> = lab
                .state()
                .scroll_plan()
                .expect("a list")
                .controls()
                .map(|(_, key)| key.as_str().to_string())
                .collect();
            let field = format!("settings/control/{key}");
            let at_field = order.iter().position(|k| *k == field).expect("in order");
            assert_eq!(order.get(at_field + 1), Some(&opener), "{at}");
            assert_eq!(
                order.get(at_field + 2).map(String::as_str),
                Some(format!("settings/reset/{key}").as_str()),
                "{at}"
            );
            lab.dispatch(action(&opener, None));
            let compiled = lab.compile(width, height);
            let wheel = rect_of(&compiled, &format!("settings/color/{key}/wheel"));
            assert!(
                wheel.y >= rect_of(&compiled, &field).bottom(),
                "{at}: under the row"
            );
            let failures: Vec<_> = compiled
                .fit_failures()
                .into_iter()
                .filter(|line| line.contains(&format!("settings/color/{key}/")))
                .collect();
            assert!(failures.is_empty(), "{at}: {failures:?}");
        }
    }
}

/// THE TRAIL COLOUR'S PICKER CHOOSES THE CURSOR'S COLOUR. Trail color takes
/// one value beside a colour, `cursor` (what the old `classic mono` style
/// says, and what the writer keeps for it), so its picker holds Cursor color,
/// at the end of Brightness's row and after it in the keyboard order: chosen,
/// it writes the word; pressed again, the colour the cursor has now, so the
/// trail looks the same and the wheel starts from it; a pick on the wheel is
/// a colour of the trail's own. The picker is no taller for it, so it is
/// whole where Brightness alone was (80x24's Adjust card), at 1× and 2×.
/// Negative control: no other colour's picker holds it, and its action writes
/// nothing for one (the accent takes no word). Before it the word could only
/// be typed into the hex field.
#[test]
fn the_trail_colour_picker_chooses_the_cursor_colour() {
    let key = prefs::EDIT_CURSOR_TRAIL_COLOR;
    let button = format!("settings/color/{key}/cursor");
    let toggle = format!("settings/color-cursor/{key}");
    // The saved style's Adjust card on Cursor Styles lists Trail color.
    let open = |source: &str| {
        let mut lab = Lab::new(source);
        lab.state_mut().navigate(SettingsRoute::CursorStyles);
        lab.state_mut()
            .reveal_control(UiKey::new(format!("settings/control/{key}")));
        lab.dispatch(action(&format!("settings/color-open/{key}"), None));
        lab
    };
    let shown = |lab: &mut Lab| {
        lab.compile(1_074.0, 684.0)
            .semantic(&UiKey::new(button.as_str()))
            .map(|node| node.value.clone())
    };
    let chosen = |yes: bool| {
        Some(SemanticValue::Text(
            if yes { "Chosen" } else { "Not chosen" }.to_string(),
        ))
    };
    let written = |patch: &ConfigPatch| {
        patch
            .edits
            .iter()
            .map(|edit| (edit.key.clone(), edit.value.clone()))
            .collect::<Vec<_>>()
    };
    let mut lab = open("cursor_trail_style = \"classic\"\n");
    assert_eq!(shown(&mut lab), chosen(false));
    let order: Vec<String> = lab
        .state()
        .scroll_plan()
        .expect("a list")
        .controls()
        .map(|(_, key)| key.as_str().to_string())
        .collect();
    // After the wheel (Brightness has no colour to brighten while Classic
    // shifts through the spectrum, so it takes no keyboard).
    let wheel = format!("settings/color/{key}/wheel");
    let after = order
        .iter()
        .position(|k| *k == wheel)
        .unwrap_or_else(|| panic!("the wheel in the keyboard order: {order:?}"));
    assert_eq!(order.get(after + 1), Some(&button), "{order:?}");

    let on = lab.commit(&toggle, Some(SemanticInput::Bool(true)));
    assert_eq!(
        written(&on),
        vec![(key.to_string(), Some(prefs::TRAIL_COLOR_CURSOR.to_string()))]
    );
    assert!(lab.text().contains("cursor_trail_color = \"cursor\""));
    assert_eq!(shown(&mut lab), chosen(true));
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Trail color set to Cursor color"),
        "the change is named as the picker names it"
    );
    assert_eq!(
        lab.state().common.last_focus.as_ref().map(UiKey::as_str),
        Some(button.as_str()),
        "the keyboard stays on it"
    );
    assert!(
        lab.invoke(&toggle, Some(SemanticInput::Bool(true)))
            .is_none(),
        "chosen again writes nothing"
    );
    let cursor = color_picker::color_now(lab.state(), prefs::EDIT_CURSOR_COLOR)
        .expect("the cursor has a colour");
    let off = lab.commit(&toggle, Some(SemanticInput::Bool(false)));
    assert_eq!(
        written(&off),
        vec![(
            key.to_string(),
            Some(format!(
                "#{:02X}{:02X}{:02X}",
                cursor[0], cursor[1], cursor[2]
            ))
        )],
        "unchosen keeps the colour the cursor has now"
    );
    assert_eq!(shown(&mut lab), chosen(false));
    let pressed = lab.press(&toggle).expect("a press turns it over");
    assert_eq!(
        written(&pressed),
        vec![(key.to_string(), Some(prefs::TRAIL_COLOR_CURSOR.to_string()))]
    );
    lab.commit(
        &format!("settings/color-pick/{key}"),
        Some(SemanticInput::ColorPick {
            hue: 0.5,
            saturation: 1.0,
        }),
    );
    assert_eq!(
        shown(&mut lab),
        chosen(false),
        "a pick on the wheel is a colour of the trail's own"
    );

    // No other colour's picker holds it.
    for other in [prefs::EDIT_CURSOR_TRAIL_ACCENT, prefs::EDIT_FOREGROUND] {
        let mut lab = Lab::new("");
        let label = lab.state().field_by_key(other).unwrap().label;
        lab.state_mut().set_search(label.to_lowercase());
        lab.dispatch(action(&format!("settings/color-open/{other}"), None));
        let compiled = lab.compile(1_074.0, 684.0);
        assert!(
            compiled
                .semantic(&UiKey::new(format!("settings/color/{other}/wheel")))
                .is_some(),
            "{other}: its picker is open"
        );
        assert!(
            compiled
                .semantic(&UiKey::new(format!("settings/color/{other}/cursor")))
                .is_none(),
            "{other}"
        );
        assert!(
            lab.invoke(
                &format!("settings/color-cursor/{other}"),
                Some(SemanticInput::Bool(true))
            )
            .is_none(),
            "{other}"
        );
    }

    // Whole at every width and text size.
    at_each_text_scale(&[1.0, 2.0], "trail colour picker fit", |scale| {
        for (width, height) in [(584.0_f32, 348.0_f32), (738.0, 418.0), (1_074.0, 684.0)] {
            let at = format!("{width}×{height} at {scale}×");
            let mut lab =
                open("cursor_trail_style = \"classic\"\ncursor_trail_color = \"cursor\"\n");
            let fitted = |compiled: &crate::native_ui::CompiledUi| {
                let failures: Vec<_> = compiled
                    .fit_failures()
                    .into_iter()
                    .filter(|line| line.contains("settings/color"))
                    .collect();
                assert!(failures.is_empty(), "{at}: {failures:?}");
            };
            fitted(&lab.compile(width, height));
            // The keyboard on it brings it into view: at 1× whole, and at
            // every size wherever Brightness is (2× text at 80x24 leaves the
            // Adjust card no room for the picker under its row at all).
            lab.state_mut().reveal_control(UiKey::new(button.as_str()));
            let compiled = lab.compile(width, height);
            let shows = |key: &str| compiled.semantic(&UiKey::new(key)).is_some();
            let brightness = format!("settings/color/{key}/brightness");
            assert_eq!(
                shows(&button),
                shows(&brightness),
                "{at}: Cursor color shows wherever Brightness does"
            );
            if scale == 1.0 {
                assert!(shows(&button), "{at}: Cursor color is in the picker");
            }
            fitted(&compiled);
        }
    });
}
