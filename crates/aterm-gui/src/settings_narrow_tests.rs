// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests Settings layout and text fitting at narrow window widths (from 22 columns up) across
//! 1×, 1.5×, and 2× text scales.

use super::*;

/// The window widths a 22-, 24-, 30-, 36- and 42-column terminal window gives Settings.
pub(super) const NARROW_WIDTHS: [f32; 5] = [178.0, 192.0, 234.0, 276.0, 318.0];
pub(super) const NARROW_HEIGHTS: [f32; 2] = [348.0, 516.0];

/// The windows an 80×24, 102×29, 150×48, 167×36 and 220×60 terminal window gives Settings.
pub(super) const WIDE_SIZES: [(f32, f32); 5] = [
    (584.0, 348.0),
    (744.0, 420.0),
    (1_095.0, 700.0),
    (1_219.0, 522.0),
    (1_606.0, 876.0),
];

/// The first screen of `route` in a `width`×`height` window, from a fresh runtime.
pub(super) fn first_screen_of(
    route: SettingsRoute,
    width: f32,
    height: f32,
) -> crate::native_ui::CompiledUi {
    let (mut runtime, instance, view) = setup();
    go_to_route(&mut runtime, instance, view, route);
    compile_settings_view(&runtime, instance, view, &view_cx_at(width, height))
}

/// Every screen of `route` in a `width`×`height` window, top to the last
/// one its list scrolls to, from a fresh runtime `prepare` has set up.
pub(super) fn screens_of(
    prepare: impl FnOnce(&mut NativeRuntime, crate::native_app::ViewId),
    route: SettingsRoute,
    width: f32,
    height: f32,
) -> Vec<crate::native_ui::CompiledUi> {
    let (mut runtime, instance, view) = routed(prepare, route);
    let cx = view_cx_at(width, height);
    let mut screens = Vec::new();
    each_screen(&mut runtime, instance, view, &cx, |compiled, _| {
        screens.push(compiled.clone());
    });
    screens
}

/// The first of [`screens_of`], without rendering the rest.
fn top_screen_of(route: SettingsRoute, width: f32, height: f32) -> crate::native_ui::CompiledUi {
    let (mut runtime, instance, view) = routed(|_, _| {}, route);
    walk_start(&mut runtime, instance, view, &view_cx_at(width, height)).0
}

fn routed(
    prepare: impl FnOnce(&mut NativeRuntime, crate::native_app::ViewId),
    route: SettingsRoute,
) -> (
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    let (mut runtime, instance, view) = setup();
    prepare(&mut runtime, view);
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
    (runtime, instance, view)
}

/// The painted words under any of `prefixes` that are not whole.
pub(super) fn cut_words(compiled: &crate::native_ui::CompiledUi, prefixes: &[&str]) -> Vec<String> {
    compiled
        .paint_audit_lines()
        .into_iter()
        .filter(|line| {
            line.strip_prefix("paint-text key=\"").is_some_and(|rest| {
                prefixes.iter().any(|prefix| rest.starts_with(prefix))
                    && (line.contains(" overflow=true ")
                        || line
                            .rsplit_once(" painted=")
                            .is_some_and(|(_, painted)| painted.contains('\u{2026}')))
            })
        })
        .collect()
}

/// The nodes under any of `prefixes` drawn past the box that holds them.
pub(super) fn spilled_nodes(
    compiled: &crate::native_ui::CompiledUi,
    prefixes: &[&str],
) -> Vec<String> {
    compiled
        .paint
        .iter()
        .filter(|node| {
            !node.peek
                && prefixes
                    .iter()
                    .any(|prefix| node.key.as_str().starts_with(prefix))
                && node.rect.width > 0.0
                && (node.rect.x < node.clip.x - 0.5 || node.rect.right() > node.clip.right() + 0.5)
        })
        .map(|node| format!("{} {:?} in {:?}", node.key.as_str(), node.rect, node.clip))
        .collect()
}

#[test]
fn a_card_of_facts_paints_every_name_and_value_whole_at_phone_width() {
    at_each_text_scale(&[1.0, 2.0], "facts at phone width", |scale| {
        for width in NARROW_WIDTHS {
            for height in NARROW_HEIGHTS {
                let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
                for (index, screen) in screens_of(|_, _| {}, SettingsRoute::About, width, height)
                    .iter()
                    .enumerate()
                {
                    let prefixes = ["about/provenance/", "about/font-credits/"];
                    let cut = cut_words(screen, &prefixes);
                    assert!(cut.is_empty(), "{context} screen {index}: {cut:#?}");
                    let spilled = spilled_nodes(screen, &prefixes);
                    assert!(spilled.is_empty(), "{context} screen {index}: {spilled:#?}");
                }
            }
        }
    });
}

#[test]
fn facts_sit_beside_their_names_until_a_word_cannot() {
    at_each_text_scale(&[2.0], "fact columns", |_| {
        let _ = view_cx();
        let rows = vec![
            (
                "Unsupervised".to_string(),
                "at its prompt 50% of 1m".to_string(),
            ),
            ("commit".to_string(), "6e83e41d17".to_string()),
        ];
        let wide = FactColumns::measure(&rows, 684.0, about_metadata_label_width());
        assert!(!wide.stacked, "a wide card keeps them beside: {wide:?}");
        assert!(
            fact_name_width("Unsupervised") <= wide.label + 0.01,
            "its name column holds its widest name word: {wide:?}"
        );
        let narrow = FactColumns::measure(&rows, 130.0, about_metadata_label_width());
        assert!(
            narrow.stacked,
            "a phone-width card at 2x stacks: {narrow:?}"
        );
        assert_eq!((narrow.label, narrow.value), (130.0, 130.0));
        let commit = metadata_value_lines("commit", "6e83e41d17", 100.0);
        assert!(
            commit.len() > 1 && commit.concat() == "6e83e41d17",
            "{commit:?}"
        );
        let name = wrap_to_box("Architecture", 100.0, &fact_name_width);
        assert!(name.len() > 1, "{name:?}");
        assert!(
            name[..name.len() - 1]
                .iter()
                .all(|line| line.ends_with('-'))
        );
        assert!(
            name.last()
                .is_some_and(|tail| tail.graphemes().count() >= 3),
            "no lone letter: {name:?}"
        );
        assert_eq!(name.concat().replace('-', ""), "Architecture");
    });
}

/// A FACT THAT WRAPS TALLER BESIDE ITS NAME THAN UNDER IT STACKS: a value
/// column every word of the fact fits, but only a word or two to a line,
/// gives way to the name over the fact at the card's full width.
#[test]
fn a_fact_that_wraps_taller_beside_its_name_stacks() {
    at_each_text_scale(&[2.0], "a fact taller beside", |scale| {
        let _ = view_cx();
        let (name, fact) = (
            "Background service",
            "On \u{2014} updates arrive in the background, without interrupting",
        );
        let rows = vec![(name.to_string(), fact.to_string())];
        let px = 13.0 * scale;
        let widest = line_pieces(fact)
            .iter()
            .map(|(_, piece)| metadata_value_text_width(piece, px, false))
            .fold(0.0_f32, f32::max);
        let label = fact_name_width(name).ceil();
        let inner = label + ABOUT_METADATA_ROW_GAP + widest.ceil() + 1.0;
        let beside = FactColumns {
            label,
            value: inner - label - ABOUT_METADATA_ROW_GAP,
            stacked: false,
        };
        let stacked = FactColumns {
            label: inner,
            value: inner,
            stacked: true,
        };
        assert!(
            beside.lines(&rows) > stacked.lines(&rows),
            "every word fits beside, on more lines: {} beside, {} stacked in {inner}pt",
            beside.lines(&rows),
            stacked.lines(&rows)
        );
        assert_eq!(FactColumns::measure(&rows, inner, label), stacked);
        let wide = inner + 400.0;
        assert!(
            !FactColumns::measure(&rows, wide, label).stacked,
            "a card wide enough keeps the fact beside on a line"
        );
    });
}

#[test]
fn a_stacked_switch_is_never_wider_than_its_line() {
    at_each_text_scale(&[1.0, 2.0], "switches at phone width", |scale| {
        for width in [178.0, 192.0, 234.0, 276.0] {
            for route in [
                SettingsRoute::KeyboardInput,
                SettingsRoute::Security,
                SettingsRoute::WindowTabs,
                SettingsRoute::SoftwareUpdate,
                SettingsRoute::Packages,
            ] {
                let context = format!("{scale}\u{d7} {width} {}", route.label());
                let mut switches = 0;
                for (index, screen) in screens_of(|_, _| {}, route, width, 516.0)
                    .iter()
                    .enumerate()
                {
                    let prefixes = ["settings/control/", "settings/reset", "settings/controls/"];
                    switches += screen
                        .paint
                        .iter()
                        .filter(|node| matches!(node.content, UiContent::Switch(_)))
                        .count();
                    let spilled = spilled_nodes(screen, &prefixes);
                    assert!(spilled.is_empty(), "{context} screen {index}: {spilled:#?}");
                    let cut = cut_words(screen, &["settings/control/"]);
                    assert!(cut.is_empty(), "{context} screen {index}: {cut:#?}");
                }
                assert!(switches > 0, "{context}: the page draws switches");
            }
        }
    });
}

/// The setting names on `compiled` broken onto a next line whose first word would have fit.
pub(super) fn early_breaks(compiled: &crate::native_ui::CompiledUi) -> Vec<String> {
    let mut lines: BTreeMap<String, Vec<(usize, String, f32)>> = BTreeMap::new();
    for node in compiled.paint.iter().filter(|node| !node.peek) {
        let Some((base, index)) = node.key.as_str().rsplit_once("/visual/line-") else {
            continue;
        };
        let (Ok(index), UiContent::Text(text)) = (index.parse::<usize>(), &node.content) else {
            continue;
        };
        lines.entry(base.to_string()).or_default().push((
            index,
            text.text.clone(),
            node.rect.width,
        ));
    }
    let mut early = Vec::new();
    for (base, mut name) in lines {
        name.sort_by_key(|(index, _, _)| *index);
        for pair in name.windows(2) {
            let ((_, line, width), (_, next, _)) = (&pair[0], &pair[1]);
            if line.ends_with('-') || line.contains(FACT_SEPARATOR) {
                continue;
            }
            let Some((_, piece)) = line_pieces(next).into_iter().next() else {
                continue;
            };
            if row_label_width(&format!("{line} {piece}")) <= width + 0.01 {
                early.push(format!("{base}: {line:?} / {next:?} in {width}pt"));
            }
        }
    }
    early
}

#[test]
fn a_wrapped_name_fills_the_box_it_is_drawn_in() {
    at_each_text_scale(&[1.0, 2.0], "names fill their boxes", |scale| {
        for width in NARROW_WIDTHS {
            for height in NARROW_HEIGHTS {
                for route in [
                    SettingsRoute::Harness,
                    SettingsRoute::Ai,
                    SettingsRoute::SoftwareUpdate,
                ] {
                    let context = format!("{scale}\u{d7} {width}\u{d7}{height} {}", route.label());
                    for (index, screen) in screens_of(|_, _| {}, route, width, height)
                        .iter()
                        .enumerate()
                    {
                        let early = early_breaks(screen);
                        assert!(early.is_empty(), "{context} screen {index}: {early:#?}");
                    }
                }
            }
        }
    });
}

#[test]
fn a_narrow_slider_sets_its_readout_over_its_track() {
    at_each_text_scale(&[1.0, 2.0], "narrow slider", |scale| {
        let _ = view_cx();
        let slider = |value: f64, display: &str| {
            UiContent::Slider(
                Control::new(
                    SliderSpec {
                        label: "Quiet after".to_string(),
                        step: 50.0,
                        display_value: display.to_string(),
                        description: None,
                        track: None,
                    },
                    ActionId::new("quiet"),
                )
                .value(SemanticValue::Number {
                    value,
                    minimum: 0.0,
                    maximum: 5_000.0,
                }),
            )
        };
        let height = 46.0 * scale.min(1.4);
        for (width, stacked) in [(118.0 * scale.min(1.2), true), (600.0, false)] {
            let mut tracks = Vec::new();
            for (value, display) in [(50.0, "50 ms"), (5_000.0, "5000 ms")] {
                let rect = LogicalRect::new(0.0, 0.0, width, height);
                let compiled = UiTree::new(UiNode::new("quiet", slider(value, display)))
                    .compile(rect)
                    .expect("the slider compiles");
                let audit = compiled
                    .text_fit_audit_line(&UiKey::new("quiet"))
                    .expect("the readout is audited");
                assert!(
                    audit.contains(" overflow=false "),
                    "{scale}\u{d7} {width}pt {display}: {audit}"
                );
                let geometry = crate::native_ui::slider_geometry(rect);
                assert_eq!(
                    geometry.value_left < geometry.track_right,
                    stacked,
                    "{scale}\u{d7} {width}pt: the readout over the track only where narrow"
                );
                tracks.push((geometry.track_x, geometry.track_right));
            }
            assert_eq!(
                tracks[0], tracks[1],
                "{scale}\u{d7} {width}pt: one track length"
            );
        }
    });
}

#[test]
fn a_slider_readout_is_measured_in_its_face_and_ends_at_its_edge() {
    at_each_text_scale(&[1.0, 2.0], "slider readout", |scale| {
        let _ = view_cx();
        let theme = aterm_render::Theme::default();
        let height = 46.0 * scale.min(1.4);
        for width in [126.0, 600.0] {
            for display in ["24 cells", "0.6 cells", "512 cells", "260 ms", "100%"] {
                let rect = LogicalRect::new(0.0, 0.0, width, height);
                let slider = UiContent::Slider(
                    Control::new(
                        SliderSpec {
                            label: "Trail length".to_string(),
                            step: 1.0,
                            display_value: display.to_string(),
                            description: None,
                            track: None,
                        },
                        ActionId::new("trail"),
                    )
                    .value(SemanticValue::Number {
                        value: 24.0,
                        minimum: 1.0,
                        maximum: 512.0,
                    }),
                );
                let compiled = UiTree::new(UiNode::new("trail", slider))
                    .compile(rect)
                    .expect("the slider compiles");
                let context = format!("{scale}\u{d7} {width}pt {display}");
                let audit = compiled
                    .text_fit_audit_line(&UiKey::new("trail"))
                    .expect("the readout is audited");
                assert!(audit.contains(" overflow=false "), "{context}: {audit}");
                let geometry = crate::native_ui::slider_geometry(rect);
                let prims = compiled.tray(theme, 14.0).prims;
                let (x, size, face) = prims
                    .iter()
                    .find_map(|prim| match prim {
                        crate::widget::DrawPrim::Text { s, x, px, face, .. } if s == display => {
                            Some((*x, *px, *face))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("{context}: the readout is painted"));
                let right = x + crate::tray_raster::ui_text_width_for(face, display, size);
                assert!(
                    (right - geometry.value_right).abs() <= 0.5,
                    "{context}: ends at {right:.1}, the readout's end is {:.1}",
                    geometry.value_right
                );
                assert!(
                    x >= geometry.value_left - 0.01,
                    "{context}: starts in its slot"
                );
            }
        }
    });
}

#[test]
fn a_reset_place_never_costs_a_name_more_than_a_line_of_its_own() {
    at_each_text_scale(&[1.0, 2.0], "reset seat", |scale| {
        let mut faults = Vec::new();
        let mut rows = 0;
        for width in [234.0, 290.0, 318.0] {
            for route in SettingsRoute::ALL {
                for screen in screens_of(|_, _| {}, route, width, 600.0) {
                    for row in screen
                        .paint
                        .iter()
                        .filter(|node| !node.peek && node.key.as_str().starts_with("settings/row/"))
                    {
                        let Some(key) = row.key.as_str().splitn(4, '/').nth(3) else {
                            continue;
                        };
                        let stacked = screen.paint.iter().any(|node| {
                            node.key.as_str() == format!("settings/label-line/{key}")
                                || node.key.as_str() == format!("settings/reset-line/{key}")
                                || node.key.as_str() == format!("settings/controls/{key}")
                        });
                        let Some(name) = screen
                            .semantic(&row.key)
                            .map(|node| node.label.clone())
                            .filter(|_| stacked)
                        else {
                            continue;
                        };
                        let prefix = format!("settings/label/{key}/visual/line-");
                        let lines: Vec<String> = screen
                            .paint
                            .iter()
                            .filter(|node| node.key.as_str().starts_with(&prefix))
                            .filter_map(|node| match &node.content {
                                UiContent::Text(text) => Some(text.text.clone()),
                                _ => None,
                            })
                            .collect();
                        let squeeze = |text: &str| text.split_whitespace().collect::<String>();
                        if lines.is_empty() || squeeze(&lines.concat()) != squeeze(&name) {
                            continue;
                        }
                        rows += 1;
                        let alone = row_name_lines(&name, row.rect.width).len();
                        if lines.len() > alone + 1 {
                            faults.push(format!(
                                "{scale}\u{d7} {width} {}: {name:?} on {} lines, {alone} alone: \
                                 {lines:?}",
                                route.label(),
                                lines.len()
                            ));
                        }
                    }
                }
            }
        }
        assert!(rows > 0, "{scale}\u{d7}: stacked rows are drawn");
        faults.sort();
        faults.dedup();
        assert!(
            faults.is_empty(),
            "{} rows:\n{}",
            faults.len(),
            faults.join("\n")
        );
    });
}

#[test]
fn a_stacked_choice_keeps_its_words() {
    at_each_text_scale(&[1.0, 2.0], "stacked choices", |scale| {
        for width in [178.0, 192.0, 234.0] {
            for (route, key) in [
                (SettingsRoute::WindowTabs, prefs::EDIT_ACTIVE_TAB_COLOR),
                (SettingsRoute::Ai, prefs::EDIT_AI_PREFERRED),
            ] {
                let context = format!("{scale}\u{d7} {width} {key}");
                let control = format!("settings/control/{key}");
                let mut seen = false;
                for screen in screens_of(|_, _| {}, route, width, 516.0) {
                    let Some(node) = screen
                        .paint
                        .iter()
                        .find(|node| !node.peek && node.key.as_str() == control)
                    else {
                        continue;
                    };
                    seen = true;
                    let UiContent::Button(button) = &node.content else {
                        panic!("{context}: a button");
                    };
                    let painted = button.spec.visual_label.clone().unwrap_or_default();
                    assert!(
                        !painted.contains('\u{2026}')
                            && (!painted.is_empty() || button.spec.chip.is_some()),
                        "{context}: {painted:?}"
                    );
                    let cut = cut_words(&screen, &[control.as_str()]);
                    assert!(cut.is_empty(), "{context}: {cut:#?}");
                }
                assert!(seen, "{context}: the row is listed");
            }
        }
    });
}

#[test]
fn a_button_keeps_its_words_in_a_narrow_column() {
    at_each_text_scale(&[1.0, 2.0], "buttons at phone width", |scale| {
        for width in NARROW_WIDTHS {
            for (route, prefixes) in [
                (
                    SettingsRoute::Harness,
                    &[
                        harness_page::HARNESS_REFRESH,
                        harness_page::HARNESS_OPEN_CONFIG,
                        harness_page::HARNESS_AI_CLIS,
                        harness_page::HARNESS_TAB_CONTROL,
                    ][..],
                ),
                (
                    SettingsRoute::About,
                    &["about/copy-build-info", "about/open-site"][..],
                ),
            ] {
                let context = format!("{scale}\u{d7} {width} {}", route.label());
                let mut buttons = 0;
                for (index, screen) in screens_of(|_, _| {}, route, width, 516.0)
                    .iter()
                    .enumerate()
                {
                    buttons += screen
                        .paint
                        .iter()
                        .filter(|node| {
                            !node.peek && prefixes.iter().any(|key| node.key.as_str() == *key)
                        })
                        .count();
                    let cut = cut_words(screen, prefixes);
                    assert!(cut.is_empty(), "{context} screen {index}: {cut:#?}");
                    let spilled = spilled_nodes(screen, prefixes);
                    assert!(spilled.is_empty(), "{context} screen {index}: {spilled:#?}");
                }
                assert!(buttons > 0, "{context}: the buttons are drawn");
            }
        }
    });
}

#[test]
fn a_text_button_paints_its_labels_lines() {
    at_each_text_scale(&[2.0], "button lines", |_| {
        let _ = view_cx();
        let room = crate::native_ui::native_button_label_width("Open Project") + 1.0;
        let lines = crate::native_ui::native_button_label_lines("Open Project Site", room)
            .expect("each word fits");
        assert_eq!(lines, ["Open Project", "Site"]);
        assert!(crate::native_ui::native_button_label_lines("Information", 20.0).is_none());
        let cjk_room = crate::native_ui::native_button_label_width("設定を") + 1.0;
        assert_eq!(
            crate::native_ui::native_button_label_lines("設定を開く。", cjk_room).as_deref(),
            Some(&["設定を".to_string(), "開く。".to_string()][..])
        );
        let single = scaled_control_height();
        let height = crate::native_ui::native_button_height_for_lines(2, single);
        assert!(height > single);
        let width = room + 20.0;
        let compiled = UiTree::new(UiNode::new(
            "site",
            UiContent::Button(
                Control::new(
                    ButtonSpec::new("Open Project Site").visual_label(lines.join("\n")),
                    ActionId::new("site"),
                )
                .style(StyleRef::Secondary),
            ),
        ))
        .compile(LogicalRect::new(0.0, 0.0, width, height))
        .expect("the button compiles");
        let audit = compiled
            .text_fit_audit_line(&UiKey::new("site"))
            .expect("the label is audited");
        assert!(audit.contains(" overflow=false "), "{audit}");
    });
}

#[test]
fn about_s_wordmark_is_whole_at_every_size() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "the wordmark", |scale| {
        for width in [
            164.0, 178.0, 192.0, 234.0, 276.0, 318.0, 584.0, 744.0, 1_200.0,
        ] {
            for height in [348.0, 420.0, 516.0, 900.0] {
                let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
                let screens = screens_of(|_, _| {}, SettingsRoute::About, width, height);
                let screen = &screens[0];
                let audit = screen
                    .text_fit_audit_line(&UiKey::new("about/wordmark"))
                    .unwrap_or_else(|| panic!("{context}: the wordmark is drawn"));
                assert!(audit.contains(" overflow=false "), "{context}: {audit}");
                let node = screen
                    .paint
                    .iter()
                    .find(|node| node.key.as_str() == "about/wordmark")
                    .expect("painted");
                let (px, face) = crate::native_ui::wordmark_typography("aterm", node.rect.width);
                assert!(
                    crate::tray_raster::ui_text_width_for(face, "aterm", px.get())
                        <= node.rect.width + 0.01,
                    "{context}: the wordmark's type fits its {}pt box",
                    node.rect.width
                );
            }
        }
    });
}

#[test]
fn a_warning_headline_keeps_its_colour_at_every_size() {
    use crate::widget::DrawPrim;
    let mut failing = update_status(false);
    failing.failing_checks = aterm_update::PERSISTENT_AFTER;
    failing.failing_persistent = true;
    failing.failing_kind = "network".into();
    at_each_text_scale(&[1.0, 1.5, 2.0], "warning headline", |scale| {
        let theme = aterm_render::Theme::default();
        let roles = crate::settings::Roles::from_theme(theme);
        let mut stepped = 0;
        for width in [178.0_f32, 234.0, 320.0, 584.0, 1_200.0] {
            let context = format!("{scale}\u{d7} {width}");
            let update = UpdateState::from_status(1, "0.1.0", Some(&failing), false);
            let pages = update_route_pages(update, width, 900.0);
            let first = &pages[0];
            let lines = first
                .paint
                .iter()
                .filter(|node| node.key.as_str().starts_with("updates/headline"))
                .filter_map(|node| match &node.content {
                    UiContent::Text(text) => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(!lines.is_empty(), "{context}: the verdict is painted");
            let prims = first.tray(theme, 14.0).prims;
            for line in &lines {
                assert!(
                    matches!(line.style, StyleRef::Danger | StyleRef::DangerCompact),
                    "{context}: {:?} in {:?}",
                    line.text,
                    line.style
                );
                stepped += usize::from(line.style == StyleRef::DangerCompact);
                let ink = prims.iter().find_map(|prim| match prim {
                    DrawPrim::Text { s, color, .. } if *s == line.text => {
                        Some([color[0], color[1], color[2]])
                    }
                    _ => None,
                });
                assert_eq!(ink, Some(roles.danger), "{context}: {:?}", line.text);
            }
            let cut = cut_words(first, &["updates/headline"]);
            assert!(cut.is_empty(), "{context}: {cut:#?}");
        }
        if scale > 1.5 {
            assert!(
                stepped > 0,
                "{scale}\u{d7}: a narrow page steps the verdict down"
            );
        }
    });
}

#[test]
fn software_update_keeps_its_words() {
    let mut failing = update_status(false);
    failing.failing_checks = aterm_update::PERSISTENT_AFTER;
    failing.failing_persistent = true;
    failing.failing_kind = "network".into();
    let states = [
        ("up to date", update_status(false)),
        ("ready", update_status(true)),
        ("failing", failing),
    ];
    at_each_text_scale(&[1.0, 2.0], "Software Update", |scale| {
        for (name, status) in &states {
            for width in NARROW_WIDTHS.into_iter().chain([744.0]) {
                for height in NARROW_HEIGHTS {
                    let context = format!("{scale}\u{d7} {name} {width}\u{d7}{height}");
                    let update = UpdateState::from_status(1, "0.1.0", Some(status), false);
                    for (index, screen) in
                        update_route_pages(update, width, height).iter().enumerate()
                    {
                        let cut = cut_words(screen, &["updates/"])
                            .into_iter()
                            .filter(|line| !says_it_is_under_way(line))
                            .collect::<Vec<_>>();
                        assert!(cut.is_empty(), "{context} screen {index}: {cut:#?}");
                        let spilled = spilled_nodes(screen, &["updates/"]);
                        assert!(spilled.is_empty(), "{context} screen {index}: {spilled:#?}");
                    }
                }
            }
        }
    });
}

#[test]
fn the_compact_toolbar_keeps_its_words() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "the compact toolbar", |scale| {
        let mut buttons = 0;
        for width in [164.0_f32, 178.0, 192.0, 234.0, 276.0, 318.0, 420.0, 584.0] {
            let context = format!("{scale}\u{d7} {width}");
            let cx = view_cx_at(width, 516.0);
            let (mut runtime, instance, view) = setup();
            go_to_route(&mut runtime, instance, view, SettingsRoute::KeyboardInput);
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            let toolbar = [
                "settings/compact-navigation",
                "settings/page-heading/",
                "settings/search",
            ];
            let whole = |lines: Vec<String>| {
                lines
                    .into_iter()
                    .filter(|line| !line.ends_with("painted=\"Search\u{2026}\""))
                    .collect::<Vec<_>>()
            };
            let cut = whole(cut_words(&compiled, &toolbar));
            assert!(cut.is_empty(), "{context}: {cut:#?}");
            let placeholder = |compiled: &crate::native_ui::CompiledUi| {
                compiled.paint.iter().find_map(|node| match &node.content {
                    UiContent::TextField(field) if node.key.as_str() == "settings/search" => {
                        Some(field.spec.placeholder.clone().unwrap_or_default())
                    }
                    _ => None,
                })
            };
            let opener = compiled.semantic(&UiKey::new(SEARCH_OPEN)).is_some();
            if !opener {
                let words = placeholder(&compiled).expect("the field is drawn");
                assert!(
                    words == "Search\u{2026}" || words == "Search",
                    "{context}: {words:?}"
                );
                continue;
            }
            buttons += 1;
            assert!(
                placeholder(&compiled).is_none(),
                "{context}: no cramped field"
            );
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(SEARCH_OPEN),
                        value: None,
                    }),
                )
                .unwrap();
            let open = compile_settings_view(&runtime, instance, view, &cx);
            let words = placeholder(&open).expect("the open field is drawn");
            assert!(
                words == "Search\u{2026}" || words == "Search",
                "{context}: the open field says Search: {words:?}"
            );
            assert!(
                open.semantic(&UiKey::new("settings/search"))
                    .is_some_and(|field| field.state.is_some_and(|state| state.focused)),
                "{context}: the keyboard is in the field"
            );
            assert!(open.semantic(&UiKey::new(SEARCH_CANCEL)).is_some());
            let cut = whole(cut_words(&open, &["settings/search", SEARCH_CANCEL]));
            assert!(cut.is_empty(), "{context}: {cut:#?}");
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(SEARCH_CANCEL),
                        value: None,
                    }),
                )
                .unwrap();
            let closed = compile_settings_view(&runtime, instance, view, &cx);
            assert!(
                closed.semantic(&UiKey::new(SEARCH_OPEN)).is_some(),
                "{context}"
            );
            assert!(placeholder(&closed).is_none(), "{context}: closed again");
        }
        if scale > 1.5 {
            assert!(
                buttons > 0,
                "{scale}\u{d7}: a narrow bar draws Search as a button"
            );
        }
    });
}

#[test]
fn a_stacked_cli_row_keeps_its_whole_detail() {
    at_each_text_scale(&[1.0, 2.0], "stacked CLI detail", |scale| {
        for width in NARROW_WIDTHS {
            let prepare = |runtime: &mut NativeRuntime, _: crate::native_app::ViewId| {
                assert!(runtime.replace_settings_harness(super::ai::fixture(), 1));
            };
            let mut details = 0;
            for screen in screens_of(prepare, SettingsRoute::Ai, width, 516.0) {
                details += screen
                    .paint
                    .iter()
                    .filter(|node| !node.peek && node.key.as_str().starts_with("ai/cli/"))
                    .filter(|node| node.key.as_str().contains("/detail"))
                    .count();
                let cut = cut_words(&screen, &["ai/cli/"]);
                assert!(cut.is_empty(), "{scale}\u{d7} {width}: {cut:#?}");
            }
            assert!(details > 0, "{scale}\u{d7} {width}: the details are drawn");
        }
    });
}

/// Text that ends in an ellipsis because it says something is under way.
fn says_it_is_under_way(line: &str) -> bool {
    [
        "Reading package status\u{2026}",
        "Reading the package log\u{2026}",
        "Reading this copy\u{2026}",
        "Checking this copy\u{2026}",
        "Reading the sessions\u{2026}",
        "Reading\u{2026}",
        "reading\u{2026}",
    ]
    .iter()
    .any(|words| {
        line.rsplit_once(" painted=\"").is_some_and(|(_, painted)| {
            let painted = painted.trim_end_matches('"');
            !painted.is_empty() && words.ends_with(painted)
        })
    })
}

#[test]
fn a_page_name_is_whole_in_its_column() {
    at_each_text_scale(&[1.0, 2.0], "page names", |scale| {
        for width in [164.0, 178.0, 192.0, 234.0] {
            for route in SettingsRoute::ALL {
                let screen = top_screen_of(route, width, 900.0);
                let cut = cut_words(&screen, &["settings/page-heading/"]);
                assert!(
                    cut.is_empty(),
                    "{scale}\u{d7} {width} {}: {cut:#?}",
                    route.label()
                );
            }
        }
    });
}

#[test]
fn the_packages_page_keeps_its_words() {
    at_each_text_scale(&[1.0, 2.0], "Packages", |scale| {
        for width in NARROW_WIDTHS.into_iter().chain([744.0]) {
            for (height, live) in [348.0, 900.0]
                .into_iter()
                .flat_map(|height| [(height, false), (height, true)])
            {
                let context = format!("{scale}\u{d7} {width}\u{d7}{height} live={live}");
                let prepare = |runtime: &mut NativeRuntime, _: crate::native_app::ViewId| {
                    if live {
                        assert!(
                            runtime.replace_settings_packages(super::live_packages_state(None), 2)
                        );
                    }
                };
                for (index, screen) in screens_of(prepare, SettingsRoute::Packages, width, height)
                    .iter()
                    .enumerate()
                {
                    let cut = cut_words(screen, &["packages/"])
                        .into_iter()
                        .filter(|line| !says_it_is_under_way(line))
                        .collect::<Vec<_>>();
                    assert!(cut.is_empty(), "{context} screen {index}: {cut:#?}");
                    let spilled = spilled_nodes(screen, &["packages/", "settings/control/"]);
                    assert!(spilled.is_empty(), "{context} screen {index}: {spilled:#?}");
                }
            }
        }
    });
}

#[test]
fn messages_small_buttons_stay_on_the_page() {
    at_each_text_scale(&[1.0, 2.0], "Messages buttons", |scale| {
        for width in NARROW_WIDTHS {
            let screen = top_screen_of(SettingsRoute::Messages, width, 516.0);
            let prefixes = [
                "settings/messages/copy-all",
                "settings/messages/open-folder",
            ];
            let cut = cut_words(&screen, &prefixes);
            assert!(cut.is_empty(), "{scale}\u{d7} {width}: {cut:#?}");
            let spilled = spilled_nodes(&screen, &prefixes);
            assert!(spilled.is_empty(), "{scale}\u{d7} {width}: {spilled:#?}");
        }
    });
}

#[test]
fn harness_keeps_its_words_with_sessions_open() {
    at_each_text_scale(&[1.0, 2.0], "Harness sessions", |scale| {
        let mut parts = 0;
        for width in NARROW_WIDTHS {
            for height in NARROW_HEIGHTS {
                for (fixture, state) in [
                    ("", super::harness::fixture()),
                    (" landings", super::harness::fixture_landings()),
                ] {
                    let context = format!("{scale}\u{d7} {width}\u{d7}{height}{fixture}");
                    let prepare = |runtime: &mut NativeRuntime, _: crate::native_app::ViewId| {
                        assert!(runtime.replace_settings_harness(state.clone(), 1));
                    };
                    for (index, screen) in
                        screens_of(prepare, SettingsRoute::Harness, width, height)
                            .iter()
                            .enumerate()
                    {
                        parts += screen
                            .paint
                            .iter()
                            .filter(|node| node.key.as_str().contains("/block/part-"))
                            .count();
                        let cut = cut_words(screen, &["harness/"])
                            .into_iter()
                            .filter(|line| !says_it_is_under_way(line))
                            .collect::<Vec<_>>();
                        assert!(cut.is_empty(), "{context} screen {index}: {cut:#?}");
                        let spilled = spilled_nodes(screen, &["harness/", "settings/control/"]);
                        assert!(spilled.is_empty(), "{context} screen {index}: {spilled:#?}");
                    }
                }
            }
        }
        if scale > 1.5 {
            assert!(
                parts > 0,
                "{scale}\u{d7}: a short window lists a row a part at a time"
            );
        }
    });
}

#[test]
fn a_one_line_session_row_keeps_its_words() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "one-line sessions", |scale| {
        let mut wrapped_beside = 0;
        for (width, height) in WIDE_SIZES {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let prepare = |runtime: &mut NativeRuntime, _: crate::native_app::ViewId| {
                assert!(runtime.replace_settings_harness(super::harness::fixture(), 1));
            };
            for (index, screen) in screens_of(prepare, SettingsRoute::Harness, width, height)
                .iter()
                .enumerate()
            {
                let cut = cut_words(screen, &["harness/"])
                    .into_iter()
                    .filter(|line| !says_it_is_under_way(line))
                    .collect::<Vec<_>>();
                assert!(cut.is_empty(), "{context} screen {index}: {cut:#?}");
                let spilled = spilled_nodes(screen, &["harness/"]);
                assert!(spilled.is_empty(), "{context} screen {index}: {spilled:#?}");
                wrapped_beside += screen
                    .paint
                    .iter()
                    .filter_map(|node| node.key.as_str().strip_suffix("/detail/line-1"))
                    .filter(|row| {
                        screen
                            .paint
                            .iter()
                            .any(|node| node.key.as_str() == format!("{row}/head"))
                    })
                    .count();
            }
        }
        if (scale - 1.5).abs() < 0.01 {
            assert!(
                wrapped_beside > 0,
                "1.5\u{d7}: a detail wraps beside the buttons somewhere"
            );
        }
    });
}

#[test]
fn a_page_subtitle_is_whole_in_its_column() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "page subtitles", |scale| {
        for (width, height) in WIDE_SIZES {
            for route in SettingsRoute::ALL {
                let screen = first_screen_of(route, width, height);
                let cut = cut_words(&screen, &["settings/page-subtitle/"]);
                assert!(
                    cut.is_empty(),
                    "{scale}\u{d7} {width}\u{d7}{height} {}: {cut:#?}",
                    route.label()
                );
            }
        }
    });
}

#[test]
fn a_row_too_narrow_for_its_name_beside_its_controls_stacks_them() {
    let prefixes = [
        "settings/label/",
        "settings/control/",
        "settings/segments/",
        "settings/segment/",
        "settings/reset",
    ];
    at_each_text_scale(&[1.5, 2.0], "wide rows", |scale| {
        for (width, height) in [WIDE_SIZES[2], WIDE_SIZES[4]] {
            for route in SettingsRoute::ALL {
                for (index, screen) in screens_of(|_, _| {}, route, width, height)
                    .iter()
                    .enumerate()
                {
                    let spilled = spilled_nodes(screen, &prefixes);
                    assert!(
                        spilled.is_empty(),
                        "{scale}\u{d7} {width}\u{d7}{height} {} screen {index}: {spilled:#?}",
                        route.label()
                    );
                }
            }
        }
        if scale > 1.5 {
            let named = screens_of(|_, _| {}, SettingsRoute::Harness, 1_606.0, 876.0)
                .iter()
                .any(|screen| {
                    screen.paint_audit_lines().iter().any(|line| {
                        line.starts_with(
                            "paint-text key=\"settings/label/harness.question_priority/",
                        ) && line.ends_with("painted=\"Question priority\"")
                    })
                });
            assert!(named, "2\u{d7}: Question priority says its name");
        }
    });
}

#[test]
fn search_results_and_modified_keep_their_words() {
    let prefixes = [
        "settings/results-range",
        "settings/search/",
        "settings/empty",
        "settings/modified/",
    ];
    at_each_text_scale(&[1.0, 1.5, 2.0], "search results", |scale| {
        let mut wrapped = 0;
        for width in NARROW_WIDTHS {
            for height in NARROW_HEIGHTS {
                for query in ["cursor", "zzqx", ""] {
                    let context = format!("{scale}\u{d7} {width}\u{d7}{height} {query:?}");
                    let (mut runtime, instance, view) = setup();
                    if query.is_empty() {
                        go_to_route(&mut runtime, instance, view, SettingsRoute::Modified);
                    } else {
                        runtime
                            .dispatch(
                                instance,
                                view,
                                AppEvent::Action(ActionInvocation {
                                    id: ActionId::new("settings/search"),
                                    value: Some(SemanticInput::Text(query.to_string())),
                                }),
                            )
                            .unwrap();
                    }
                    let cx = view_cx_at(width, height);
                    each_screen(&mut runtime, instance, view, &cx, |screen, top| {
                        let cut = cut_words(screen, &prefixes)
                            .into_iter()
                            .filter(|line| !line.ends_with("painted=\"Reset All\u{2026}\""))
                            .collect::<Vec<_>>();
                        assert!(cut.is_empty(), "{context} screen {top}: {cut:#?}");
                        let spilled = spilled_nodes(screen, &prefixes);
                        assert!(spilled.is_empty(), "{context} screen {top}: {spilled:#?}");
                        wrapped += screen
                            .paint_audit_lines()
                            .iter()
                            .filter(|line| {
                                line.starts_with("paint-text key=\"settings/search/page/")
                                    && line.contains("\\n")
                            })
                            .count();
                    });
                }
            }
        }
        if scale > 1.5 {
            assert!(wrapped > 0, "2\u{d7}: a page result's words go on lines");
        }
    });
}

#[test]
fn an_empty_field_drops_no_word_where_its_row_can_grow() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "placeholder words", |scale| {
        let mut faults = Vec::new();
        let mut fields = 0;
        for (route, source) in [
            (SettingsRoute::Appearance, ""),
            (SettingsRoute::Terminal, ""),
            (
                SettingsRoute::CursorStyles,
                "cursor_trail_style = \"comet\"\n",
            ),
        ] {
            for width in [178.0, 234.0] {
                let mut lab = super::modified::Lab::new(source);
                lab.state_mut().navigate(route);
                let mut page = 0;
                loop {
                    lab.state_mut().scroll_page_to(page);
                    lab.state_mut().page_reveal = None;
                    let screen = lab.compile(width, 348.0);
                    for node in &screen.paint {
                        let UiContent::TextField(field) = &node.content else {
                            continue;
                        };
                        if !node.key.as_str().starts_with("settings/control/")
                            || field.spec.input.is_some()
                            || !field
                                .spec
                                .visual_value
                                .as_deref()
                                .unwrap_or_default()
                                .is_empty()
                        {
                            continue;
                        }
                        let SemanticValue::Text(value) = &field.value else {
                            continue;
                        };
                        let painted = field.spec.placeholder.clone().unwrap_or_default();
                        fields += 1;
                        let context = format!(
                            "{scale}\u{d7} {width} {} {}",
                            route.label(),
                            node.key.as_str()
                        );
                        let themed = [
                            prefs::EDIT_FOREGROUND,
                            prefs::EDIT_BACKGROUND,
                            prefs::EDIT_CURSOR_COLOR,
                            prefs::EDIT_SELECTION_COLOR,
                        ]
                        .iter()
                        .any(|key| node.key.as_str() == format!("settings/control/{key}"));
                        if themed && field.spec.swatch.is_none() {
                            faults.push(format!("{context}: its colour is not shown"));
                        }
                        let read = painted_words(&painted);
                        let plain = crate::native_ui::plain_placeholder(value);
                        if painted.contains('\u{2026}')
                            || plain.split_whitespace().any(|word| !read.contains(word))
                        {
                            faults.push(format!("{context}: {painted:?} of {value:?}"));
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
        assert!(fields > 0, "{scale}\u{d7}: empty fields are drawn");
        faults.sort();
        faults.dedup();
        assert!(
            faults.is_empty(),
            "{} fields:\n{}",
            faults.len(),
            faults.join("\n")
        );
    });
}

#[test]
fn a_fields_value_is_whole_where_its_row_can_grow() {
    let bare = |text: &str| {
        text.chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect::<String>()
    };
    at_each_text_scale(&[1.0, 1.5, 2.0], "field values", |scale| {
        let mut wrapped = 0;
        for (value, path) in [
            ("nushell-nightly-build", false),
            ("/opt/homebrew/Cellar/zsh/5.9/bin/login-zsh", true),
        ] {
            for width in [178.0, 234.0, 744.0] {
                let mut lab = super::modified::Lab::new(&format!("shell = \"{value}\"\n"));
                lab.state_mut().navigate(SettingsRoute::Modified);
                let key = UiKey::new("settings/control/shell");
                let context = format!("{scale}\u{d7} {width}pt {value}");
                let rest = lab.compile(width, 2_000.0);
                let painted = rest
                    .text_fit_audit_line(&key)
                    .unwrap_or_else(|| panic!("{context}: the field is drawn"));
                assert!(painted.contains(" overflow=false "), "{context}: {painted}");
                let shown = painted
                    .split(" painted=\"")
                    .nth(1)
                    .map(|rest| rest.trim_end().trim_end_matches('"').replace("\\n", "\n"))
                    .expect("painted words");
                let lines = shown.split('\n').count();
                let row = rest
                    .paint
                    .iter()
                    .find(|node| node.key == key)
                    .map(|node| node.rect)
                    .expect("the field's rect");
                if path {
                    assert!(
                        bare(&shown).ends_with(&bare("/login-zsh"))
                            && (lines <= 2 || shown.starts_with('\u{2026}')),
                        "{context}: {shown:?}"
                    );
                } else {
                    assert_eq!(bare(&shown), bare(value), "{context}: {shown:?}");
                    assert!(!shown.contains('\u{2026}'), "{context}: {shown:?}");
                }
                if width >= 744.0 && !path {
                    assert_eq!(lines, 1, "{context}: one line holds it: {shown:?}");
                }
                assert!(
                    row.height + 0.5
                        >= lines as f32 * crate::native_ui::native_button_line_height(),
                    "{context}: the field holds its {lines} lines: {row:?}"
                );
                wrapped += usize::from(lines > 1);
                let _ = lab.invoke(
                    "settings/set/shell",
                    Some(SemanticInput::TextPosition {
                        byte: 0,
                        extend: false,
                    }),
                );
                let editing = lab.compile(width, 2_000.0);
                let field = editing
                    .paint
                    .iter()
                    .find(|node| node.key == key)
                    .expect("the field is drawn");
                assert!(
                    matches!(&field.content, UiContent::TextField(control) if control.spec.input.is_some()),
                    "{context}: the field is being edited"
                );
                assert!(
                    (field.rect.height - row.height).abs() < 0.5,
                    "{context}: editing keeps its height: {:?} vs {row:?}",
                    field.rect
                );
            }
        }
        assert!(
            wrapped > 0,
            "{scale}\u{d7}: a narrow field sets its value on lines"
        );
    });
}

#[test]
fn a_fields_chip_on_its_own_line_takes_none_of_its_words_width() {
    at_each_text_scale(&[2.0], "chip line", |_| {
        let _ = view_cx();
        let theme = aterm_render::Theme::default();
        let line = crate::native_ui::native_button_line_height();
        let field = |swatch_line: bool| {
            UiContent::TextField(Control::new(
                TextFieldSpec {
                    label: "Foreground".to_string(),
                    placeholder: Some("Theme".to_string()),
                    secret: false,
                    visual_value: Some(String::new()),
                    input: None,
                    swatch: Some([0x20, 0x40, 0x80]),
                    swatch_line,
                    trailing_hint: None,
                    description: None,
                    end_button: false,
                },
                ActionId::new("foreground"),
            ))
        };
        let height = scaled_control_height() + line;
        let rect = LogicalRect::new(0.0, 0.0, 118.0, height);
        let available = |compiled: &crate::native_ui::CompiledUi| {
            let audit = compiled
                .text_fit_audit_line(&UiKey::new("foreground"))
                .expect("the field is audited");
            let value = audit
                .split(" available=")
                .nth(1)
                .and_then(|rest| rest.split(' ').next())
                .and_then(|number| number.parse::<f32>().ok())
                .expect("its room");
            (audit, value)
        };
        let own = UiTree::new(UiNode::new("foreground", field(true)))
            .compile(rect)
            .expect("compiles");
        let (audit, room) = available(&own);
        assert!(audit.contains(" overflow=false "), "{audit}");
        assert!(
            (room - (118.0 - 20.0)).abs() < 0.5,
            "the whole text box: {audit}"
        );
        let chip = own
            .tray(theme, 14.0)
            .prims
            .into_iter()
            .find_map(|prim| match prim {
                crate::widget::DrawPrim::Panel { x, y, w, .. }
                    if (w - crate::native_ui::TEXT_FIELD_SWATCH_SIZE).abs() < 0.01 =>
                {
                    Some((x, y))
                }
                _ => None,
            })
            .expect("the chip is drawn");
        assert!(
            chip.1 >= height - line - 0.5,
            "on the line under the words: {chip:?} in {height}"
        );
        assert!(chip.0 > 118.0 - 40.0, "at the field's end: {chip:?}");
        let beside = UiTree::new(UiNode::new("foreground", field(false)))
            .compile(rect)
            .expect("compiles");
        let (_, room) = available(&beside);
        assert!((room - (118.0 - 20.0 - 30.0)).abs() < 0.5, "{room}");
    });
}

#[test]
fn a_fields_hint_stands_at_the_end_of_its_text_box() {
    at_each_text_scale(&[1.0, 2.0], "hint at the box's end", |scale| {
        let _ = view_cx();
        let theme = aterm_render::Theme::default();
        let line = crate::native_ui::native_button_line_height();
        for swatch_line in [true, false] {
            let control = Control::new(
                TextFieldSpec {
                    label: "Background".to_string(),
                    placeholder: Some("Theme".to_string()),
                    secret: false,
                    visual_value: Some("zz".to_string()),
                    input: None,
                    swatch: Some([0x20, 0x40, 0x80]),
                    swatch_line,
                    trailing_hint: Some("Unsaved".to_string()),
                    description: None,
                    end_button: false,
                },
                ActionId::new("background"),
            );
            let height = scaled_control_height() + line;
            let rect = LogicalRect::new(0.0, 0.0, 260.0, height);
            let geometry = crate::native_ui::text_field_geometry(
                &control,
                rect,
                crate::native_ui::native_type_px(crate::type_scale::TypeStep::Secondary).get(),
            );
            let compiled = UiTree::new(UiNode::new("background", UiContent::TextField(control)))
                .compile(rect)
                .expect("compiles");
            let (x, px) = compiled
                .tray(theme, 14.0)
                .prims
                .into_iter()
                .find_map(|prim| match prim {
                    crate::widget::DrawPrim::Text { s, x, px, .. } if s == "Unsaved" => {
                        Some((x, px))
                    }
                    _ => None,
                })
                .expect("the hint is drawn");
            let context = format!("{scale}\u{d7} chip line {swatch_line}");
            assert!(
                x >= geometry.text_right + crate::native_ui::TEXT_FIELD_SWATCH_GAP - 0.5,
                "{context}: the hint at {x:.1} stands after the words' box, which ends at {:.1}",
                geometry.text_right
            );
            let end = x + crate::tray_raster::ui_text_width("Unsaved", px);
            let box_end = if swatch_line {
                260.0 - crate::native_ui::TEXT_FIELD_HORIZONTAL_PADDING
            } else {
                260.0
                    - crate::native_ui::TEXT_FIELD_HORIZONTAL_PADDING
                    - crate::native_ui::TEXT_FIELD_SWATCH_SIZE
                    - crate::native_ui::TEXT_FIELD_SWATCH_GAP
            };
            assert!(
                (end - box_end).abs() < 0.5,
                "{context}: the hint ends at {end:.1}, its box at {box_end:.1}"
            );
        }
    });
}

#[test]
fn a_narrow_field_gives_up_its_hint_never_its_words() {
    at_each_text_scale(&[2.0], "hint or words", |_| {
        for (width, hinted) in [(178.0, false), (192.0, false), (744.0, true)] {
            let mut lab = super::modified::Lab::new("");
            lab.state_mut().navigate(SettingsRoute::Terminal);
            let _ = lab.compile(width, 432.0);
            let _ = lab.invoke(
                &format!("settings/set/{}", prefs::EDIT_SCROLLBACK),
                Some(SemanticInput::Text("abc".to_string())),
            );
            let compiled = lab.compile(width, 432.0);
            let key = UiKey::new(format!("settings/control/{}", prefs::EDIT_SCROLLBACK));
            let context = format!("2\u{d7} {width}pt");
            let audit = compiled
                .text_fit_audit_line(&key)
                .unwrap_or_else(|| panic!("{context}: the field is drawn"));
            assert!(
                audit.contains(" overflow=false ") && audit.contains("abc"),
                "{context}: its words whole: {audit}"
            );
            let painted = compiled
                .tray(aterm_render::Theme::default(), 14.0)
                .prims
                .into_iter()
                .any(|prim| matches!(prim, crate::widget::DrawPrim::Text { s, .. } if s == "Unsaved"));
            assert_eq!(painted, hinted, "{context}: the hint");
        }
    });
}

#[test]
fn the_fleet_fields_words_keep_it_a_field() {
    at_each_text_scale(&[1.0, 2.0], "fleet words", |scale| {
        for width in NARROW_WIDTHS {
            for height in NARROW_HEIGHTS {
                let context = format!("{scale}\u{d7} {width}x{height}");
                let (mut runtime, instance, view) = every_page_runtime();
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!()
                    };
                    state.arm_config_file(None, None, ConfigFileArm::inert());
                }
                go_to_route(&mut runtime, instance, view, SettingsRoute::Ai);
                let key = UiKey::new(format!("settings/control/{}", prefs::EDIT_AI_FLEET));
                let (mut seen, mut whole) = (false, false);
                each_screen(
                    &mut runtime,
                    instance,
                    view,
                    &view_cx_at(width, height),
                    |screen, _| {
                        let Some(node) = screen
                            .paint
                            .iter()
                            .find(|node| node.key == key && !node.peek)
                        else {
                            return;
                        };
                        seen = true;
                        whole |= node.clip.height >= node.rect.height - 0.5;
                        let painted = painted_text(node).unwrap_or_default();
                        assert!(
                            !painted.contains('\u{2026}')
                                && reads_as(&painted, "ai-fleet.toml beside aterm.toml"),
                            "{context}: the fleet field paints {painted:?}"
                        );
                    },
                );
                assert!(seen, "{context}: the fleet field is on the page");
                assert!(whole, "{context}: the field is whole on some screen");
            }
        }
    });
}

#[test]
fn every_music_status_line_is_whole_and_names_its_cause() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "music status", |scale| {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let (px, _) = crate::native_ui::text_paint_metrics(SemanticRole::Status, StyleRef::Quiet);
        for status in [
            MusicStatus::NoOutput { saved_on: true },
            MusicStatus::NoOutput { saved_on: false },
            MusicStatus::VolumeZero,
            MusicStatus::TypingNotesOff,
            MusicStatus::MotionPaused(MotionReduction::Setting),
            MusicStatus::MotionPaused(MotionReduction::System),
            MusicStatus::MotionPaused(MotionReduction::HeavyLoad),
        ] {
            let cause = match status {
                MusicStatus::NoOutput { .. } => "audio output".to_string(),
                MusicStatus::VolumeZero => "sound volume".to_string(),
                MusicStatus::TypingNotesOff => "trail".to_string(),
                MusicStatus::MotionPaused(reduction) => reduction.cause().to_lowercase(),
            };
            for width in [126.0, 160.0, 220.0, 320.0, 528.0, 682.0] {
                let (node, height) = top_music_suppression_node(status, width);
                let compiled = crate::native_ui::UiTree::new(node)
                    .compile(LogicalRect::new(0.0, 0.0, width, height))
                    .expect("the status compiles");
                let context = format!("{scale}\u{d7} {width}pt {status:?}");
                let cut = cut_words(&compiled, &["settings/top/music-effects/suppression"]);
                assert!(cut.is_empty(), "{context}: {cut:#?}");
                let painted = compiled
                    .paint
                    .iter()
                    .filter(|node| {
                        node.key
                            .as_str()
                            .starts_with("settings/top/music-effects/suppression/visual")
                    })
                    .filter_map(|node| match &node.content {
                        UiContent::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(
                    painted.to_lowercase().contains(&cause),
                    "{context}: {painted:?} names no {cause}"
                );
                let full = &status.forms()[0];
                if wrapped_copy_lines(full, px, width).len() <= 2 {
                    assert_eq!(&painted, full, "{context}: the whole sentence fits");
                }
            }
        }
    });
}

#[test]
fn the_packages_service_line_paints_its_value_in_the_column_it_was_measured_for() {
    at_each_text_scale(&[1.0, 2.0], "service line", |scale| {
        let mut packages = packages_state_with_activity(0).projection();
        packages.loop_status = "Unavailable in this copy of aterm".to_string();
        for width in (100..=420).map(|width| width as f32) {
            let (row, height) = packages_service_status_row(&packages, width);
            let compiled = crate::native_ui::UiTree::new(row)
                .compile(LogicalRect::new(0.0, 0.0, width, height))
                .expect("the row compiles");
            let overflow = compiled
                .paint_audit_lines()
                .into_iter()
                .filter(|line| line.starts_with("paint-text") && line.contains("overflow=true"))
                .collect::<Vec<_>>();
            assert!(overflow.is_empty(), "{scale}\u{d7} {width}: {overflow:#?}");
        }
    });
}

/// A MESSAGES FILTER CHIP BREAKS A WORD ONLY WHERE ITS ROOM CANNOT HOLD IT (item 101),
/// from 22 to 42 columns at 1×, 1.5× and 2×, and no line starts or ends in ` · `.
#[test]
fn a_messages_chip_breaks_a_word_only_where_its_room_cannot_hold_it() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "Messages chips", |scale| {
        let px = crate::native_ui::native_type_px(crate::type_scale::TypeStep::Secondary).get();
        let measure = |line: &str| {
            crate::tray_raster::ui_text_width_for(crate::widget::TextFace::UiBold, line, px).max(
                crate::tray_raster::ui_text_width_for(crate::widget::TextFace::Ui, line, px),
            )
        };
        for cols in 22..=42_u16 {
            let width = 178.0 + 7.0 * f32::from(cols - 22);
            let (runtime, instance, view) = super::setup_with_messages();
            let compiled =
                compile_settings_view(&runtime, instance, view, &view_cx_at(width, 516.0));
            let mut chips = 0;
            for node in &compiled.paint {
                if !node.key.as_str().starts_with("settings/messages/filter/") {
                    continue;
                }
                let UiContent::Button(control) = &node.content else {
                    continue;
                };
                let Some(painted) = control.spec.visual_label.as_deref() else {
                    continue;
                };
                chips += 1;
                let at = format!("{scale}\u{d7} {cols} cols {}", node.key.as_str());
                let room = crate::native_ui::centred_label_room(&control.spec, node.rect.width);
                let lines: Vec<&str> = painted.split('\n').collect();
                for line in &lines {
                    assert!(
                        !line.starts_with('\u{b7}') && !line.ends_with('\u{b7}'),
                        "{at}: {painted:?}"
                    );
                }
                for word in control
                    .spec
                    .label
                    .split([' ', '\u{b7}'])
                    .filter(|w| !w.is_empty())
                {
                    if measure(word) <= room {
                        assert!(
                            lines.iter().any(|line| line.split(' ').any(|w| w == word)),
                            "{at}: {word:?} fits the {room}pt room but {painted:?}"
                        );
                    }
                }
            }
            assert!(chips >= 2, "{scale}\u{d7} {cols} cols: chips");
        }
    });
}

#[test]
fn messages_keeps_its_words_with_a_log() {
    let reach = [
        "settings/messages/copy-all",
        "settings/messages/open-folder",
        "settings/messages/filter/all",
        "settings/messages/filter/warn",
    ];
    at_each_text_scale(&[1.0, 1.5, 2.0], "Messages with a log", |scale| {
        for width in NARROW_WIDTHS {
            for height in NARROW_HEIGHTS {
                for open in [false, true] {
                    let context = format!("{scale}\u{d7} {width}\u{d7}{height} open={open}");
                    let (mut runtime, instance, view) = super::setup_with_messages();
                    let cx = view_cx_at(width, height);
                    if open {
                        super::select_message(&mut runtime, instance, view, 40);
                        let _ = compile_settings_view(&runtime, instance, view, &cx);
                        runtime
                            .dispatch(instance, view, AppEvent::ScrollLines(-10_000))
                            .unwrap();
                    }
                    let mut whole = std::collections::BTreeSet::new();
                    let mut edged = std::collections::BTreeSet::new();
                    let mut head_cut = false;
                    let mut name = std::collections::BTreeMap::new();
                    let key_of =
                        |line: &str| line.split('"').nth(1).unwrap_or_default().to_string();
                    super::scroll_messages_through(
                        &mut runtime,
                        instance,
                        view,
                        &cx,
                        1,
                        |screen, k| {
                            let cut = cut_words(screen, &["settings/messages/"])
                                .into_iter()
                                .filter(|line| {
                                    let head_title = line
                                        .starts_with("paint-text key=\"settings/messages/row/")
                                        && (line.contains("/label\"")
                                            || line.contains("/label/line-"));
                                    head_cut |= head_title && line.contains("/row/40/");
                                    !head_title
                                })
                                .filter(|line| !line.contains(" more lines)\""))
                                .filter(|line| {
                                    let edge = line.contains("clip-truncated=true");
                                    if edge {
                                        edged.insert(key_of(line));
                                    }
                                    !edge
                                })
                                .collect::<Vec<_>>();
                            assert!(cut.is_empty(), "{context} screen {k}: {cut:#?}");
                            let spilled = spilled_nodes(screen, &["settings/messages/"])
                                .into_iter()
                                .filter(|line| !line.starts_with("settings/messages/scroll"))
                                .collect::<Vec<_>>();
                            assert!(spilled.is_empty(), "{context} screen {k}: {spilled:#?}");
                            for line in screen.paint_audit_lines() {
                                if line.starts_with("paint-text")
                                    && line.contains("overflow=false")
                                    && line.contains("clip-truncated=false")
                                {
                                    whole.insert(key_of(&line));
                                }
                            }
                            for node in &screen.paint {
                                if let Some(index) = node
                                    .key
                                    .as_str()
                                    .strip_prefix("settings/messages/row/40/name/")
                                    .and_then(|index| index.parse::<usize>().ok())
                                    && let UiContent::Text(text) = &node.content
                                {
                                    name.insert(index, text.text.clone());
                                }
                            }
                        },
                    );
                    if open && head_cut {
                        let title = super::sample_messages()
                            .entries
                            .iter()
                            .find(|entry| entry.id == 40)
                            .map(|entry| entry.title.clone())
                            .unwrap_or_default();
                        let said = name.values().cloned().collect::<Vec<_>>().join("\n");
                        assert_eq!(painted_words(&said), title, "{context}: the open title");
                    }
                    let entry = open.then(|| "settings/messages/row/40/copy".to_string());
                    for key in reach
                        .iter()
                        .map(|key| key.to_string())
                        .chain(entry)
                        .chain(edged)
                    {
                        if key.starts_with("settings/messages/scroll") {
                            continue;
                        }
                        assert!(
                            whole.contains(&key),
                            "{context}: {key} is whole on some screen"
                        );
                    }
                }
            }
        }
    });
}

#[test]
fn a_one_line_head_leaves_its_title_two_fifths_of_the_row() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "two fifths", |scale| {
        let messages = super::sample_messages();
        let all: Vec<&MessageView> = messages.entries.iter().collect();
        let clock = MessagesClock::of(&all, messages.now_unix_ms, messages.utc_offset_s);
        for width in (120..=720).step_by(2).map(|width| width as f32) {
            for compact in [true, false] {
                let lead = MessagesLead::measure(&all, clock, compact, width);
                let title = width - lead.title_x() - MESSAGES_CHEVRON_END;
                assert!(
                    lead.stacked || title >= width * 0.4 - 0.01,
                    "{scale}\u{d7} {width} compact={compact}: the title has {title:.1}pt"
                );
            }
        }
    });
}

#[test]
fn the_manual_page_s_buttons_keep_their_words() {
    let buttons = [
        "settings/manual/open",
        "settings/manual/show-file",
        "settings/manual/copy-path",
    ];
    at_each_text_scale(&[1.0, 2.0], "the Manual page's buttons", |scale| {
        let sizes = NARROW_WIDTHS
            .into_iter()
            .flat_map(|width| NARROW_HEIGHTS.map(|height| (width, height)))
            .chain(WIDE_SIZES);
        let mut drawn = 0;
        for (width, height) in sizes {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            for (index, screen) in screens_of(|_, _| {}, SettingsRoute::Manual, width, height)
                .iter()
                .enumerate()
            {
                drawn += screen
                    .paint
                    .iter()
                    .filter(|node| !node.peek && buttons.contains(&node.key.as_str()))
                    .count();
                let cut = cut_words(screen, &buttons);
                assert!(cut.is_empty(), "{context} screen {index}: {cut:#?}");
                let spilled = spilled_nodes(screen, &buttons);
                assert!(spilled.is_empty(), "{context} screen {index}: {spilled:#?}");
            }
        }
        assert!(drawn > 0, "{scale}\u{d7}: the buttons are drawn");
    });
}

#[test]
fn a_scrolled_page_chooses_its_chrome_again_when_the_text_size_changes() {
    let chrome_of = |runtime: &NativeRuntime, view: crate::native_app::ViewId| {
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!()
        };
        state.page_chrome.get()
    };
    let scroll_to = |runtime: &mut NativeRuntime, view: crate::native_app::ViewId, stop| {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.scroll_page_to(stop);
    };
    let set_scale = |scale: f32| {
        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: scale,
                ..crate::native_appearance::current_preferences()
            },
        );
    };
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            let mut differs = 0;
            for route in SettingsRoute::ALL {
                for (width, height) in NARROW_WIDTHS
                    .into_iter()
                    .flat_map(|width| NARROW_HEIGHTS.map(|height| (width, height)))
                    .chain(WIDE_SIZES)
                {
                    let context = format!("{route:?} {width}\u{d7}{height}");
                    let cx = view_cx_at(width, height);
                    set_scale(2.0);
                    let (mut fresh, instance, view) = setup();
                    go_to_route(&mut fresh, instance, view, route);
                    scroll_to(&mut fresh, view, 2);
                    let _ = compile_settings_view(&fresh, instance, view, &cx);
                    let at_two = chrome_of(&fresh, view);
                    set_scale(1.0);
                    let (mut runtime, instance, view) = setup();
                    go_to_route(&mut runtime, instance, view, route);
                    let _ = compile_settings_view(&runtime, instance, view, &cx);
                    scroll_to(&mut runtime, view, 2);
                    let _ = compile_settings_view(&runtime, instance, view, &cx);
                    let at_one = chrome_of(&runtime, view);
                    differs += usize::from(at_one != at_two);
                    set_scale(2.0);
                    let _ = compile_settings_view(&runtime, instance, view, &cx);
                    assert_eq!(chrome_of(&runtime, view), at_two, "{context}");
                }
            }
            assert!(differs > 0, "some page's chrome differs at 2\u{d7}");
        })
        .expect("a thread")
        .join()
        .expect("the chrome is chosen again");
}

#[test]
fn every_pages_first_frame_is_its_second() {
    at_each_text_scale(&[1.0, 2.0], "first frames", |scale| {
        let windows = SettingsRoute::ALL
            .into_iter()
            .flat_map(|route| {
                NARROW_WIDTHS
                    .into_iter()
                    .flat_map(|width| NARROW_HEIGHTS.map(|height| (width, height)))
                    .chain(WIDE_SIZES)
                    .map(move |(width, height)| (route, width, height))
            })
            .collect::<Vec<_>>();
        on_workers(&windows, "first frames", |&(route, width, height)| {
            let cx = view_cx_at(width, height);
            let (mut runtime, instance, view) = setup();
            go_to_route(&mut runtime, instance, view, route);
            let first = compile_settings_view(&runtime, instance, view, &cx);
            let second = compile_settings_view(&runtime, instance, view, &cx);
            assert!(
                first == second,
                "{route:?} {width}\u{d7}{height} at {scale}\u{d7}: the second frame draws \
                 another screen: {}",
                first_apart(&first, &second)
            );
        });
    });
}

/// Customize's first frame is its second: opened before the page drew or after, drawn again with nothing between, the sheet draws the same screen.
#[test]
fn the_customize_sheets_first_frame_is_its_second() {
    at_each_text_scale(&[1.0, 2.0], "Customize's first frames", |scale| {
        let mut windows = Vec::new();
        for theme in ["Dracula", "GitHub Light"] {
            for (width, height) in [
                (178.0, 348.0),
                (444.0, 180.0),
                (444.0, 292.0),
                (584.0, 348.0),
                (1_095.0, 700.0),
            ] {
                for drawn_first in [false, true] {
                    windows.push((theme, width, height, drawn_first));
                }
            }
        }
        on_workers(
            &windows,
            "Customize's first frames",
            |&(theme, width, height, drawn_first)| {
                let cx = view_cx_at(width, height);
                let (mut runtime, instance, view) = setup();
                replace_settings_source(&mut runtime, view, format!("theme = \"{theme}\"\n"));
                go_to_route(&mut runtime, instance, view, SettingsRoute::Themes);
                if drawn_first {
                    let _ = compile_settings_view(&runtime, instance, view, &cx);
                }
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: ActionId::new("settings/showcase/customize"),
                            value: None,
                        }),
                    )
                    .unwrap();
                let first = compile_settings_view(&runtime, instance, view, &cx);
                assert!(
                    first
                        .semantic(&UiKey::new("settings/showcase/customize-done"))
                        .is_some(),
                    "{theme} {width}\u{d7}{height} at {scale}\u{d7}: the sheet is open"
                );
                let second = compile_settings_view(&runtime, instance, view, &cx);
                assert!(
                    first == second,
                    "{theme} {width}\u{d7}{height} at {scale}\u{d7}, Customize opened \
                     {}: the second frame draws another screen: {}",
                    if drawn_first {
                        "on a drawn page"
                    } else {
                        "before the page drew"
                    },
                    first_apart(&first, &second)
                );
            },
        );
    });
}
/// Where two compiles of one screen first differ, both sides whole.
fn first_apart(was: &crate::native_ui::CompiledUi, now: &crate::native_ui::CompiledUi) -> String {
    fn apart<T: PartialEq + std::fmt::Debug>(what: &str, was: &[T], now: &[T]) -> Option<String> {
        (0..was.len().max(now.len())).find_map(|at| {
            let (one, two) = (was.get(at), now.get(at));
            (one != two).then(|| {
                format!(
                    "{what} {at} (of {}, then {}) was {one:?}, then {two:?}",
                    was.len(),
                    now.len()
                )
            })
        })
    }
    apart("painted node", &was.paint, &now.paint)
        .or_else(|| apart("hit region", &was.hits, &now.hits))
        .or_else(|| apart("semantic node", &was.semantics, &now.semantics))
        .or_else(|| apart("focus stop", &was.focus_order, &now.focus_order))
        .or_else(|| apart("segment group", &was.segment_groups, &now.segment_groups))
        .or_else(|| apart("list picture", &was.list_pictures, &now.list_pictures))
        .or_else(|| {
            apart(
                "default action",
                std::slice::from_ref(&was.default_action),
                std::slice::from_ref(&now.default_action),
            )
        })
        .or_else(|| {
            apart(
                "bounds",
                std::slice::from_ref(&was.bounds),
                std::slice::from_ref(&now.bounds),
            )
        })
        .unwrap_or_else(|| "nothing apart".to_string())
}

#[test]
fn the_retired_packages_note_keeps_its_words_in_a_narrow_column() {
    let note = atpkg::config::auto_update_note(false, Some(true));
    at_each_text_scale(&[1.0, 2.0], "the retired note", |scale| {
        for width in NARROW_WIDTHS.into_iter().chain([474.0, 744.0]) {
            let context = format!("{scale}\u{d7} {width}");
            let prepare = |runtime: &mut NativeRuntime, _: crate::native_app::ViewId| {
                let packages = packages_state_with_activity_policy(2, false)
                    .with_retired_switch_note(Some(note.clone()));
                assert!(runtime.replace_settings_packages(packages, 2));
            };
            let mut lines = std::collections::BTreeMap::new();
            for (index, screen) in screens_of(prepare, SettingsRoute::Packages, width, 516.0)
                .iter()
                .enumerate()
            {
                let cut = cut_words(screen, &["packages/retired-note"]);
                assert!(cut.is_empty(), "{context} screen {index}: {cut:#?}");
                for node in screen.paint.iter().filter(|node| !node.peek) {
                    if let (Some(line), UiContent::Text(text)) = (
                        node.key
                            .as_str()
                            .strip_prefix("packages/retired-note/line/")
                            .and_then(|n| n.parse::<usize>().ok()),
                        &node.content,
                    ) {
                        lines.insert(line, text.text.clone());
                    }
                }
            }
            assert!(!lines.is_empty(), "{context}: the note is drawn");
            let said = lines.into_values().collect::<Vec<_>>().join("\n");
            assert_eq!(
                crate::native_ui::joined_as_named(&said, &note),
                note,
                "{context}: every word paints"
            );
        }
    });
}

/// The Packages state of a copy of aterm with no package tool, its status read.
fn packages_without_its_tool() -> PackagesState {
    let mut service = crate::packages_screen::PackagesService::new();
    assert!(service.note_tool(false));
    let sequence = service.begin(None).expect("a status read starts");
    assert!(service.finish(
        sequence,
        crate::packages_screen::PackagesWorkerCompletion::refresh(
            crate::packages_screen::PackagesStatusReport::from_parts(
                false,
                atpkg::manager_enabled(),
                "fp".to_string(),
                None,
                &[],
            ),
        ),
    ));
    service.state(false, true, false)
}

/// Every screen of `route` in a `width`×`height` window as Page Down turns
/// them over from the list's top, from a fresh runtime `prepare` has set up,
/// to the screen a turn no longer changes.
fn paged_screens_of(
    prepare: impl FnOnce(&mut NativeRuntime),
    route: SettingsRoute,
    width: f32,
    height: f32,
) -> Vec<crate::native_ui::CompiledUi> {
    let (mut runtime, instance, view) = setup();
    prepare(&mut runtime);
    go_to_route(&mut runtime, instance, view, route);
    let cx = view_cx_at(width, height);
    let mut screens = vec![compile_settings_view(&runtime, instance, view, &cx)];
    for _ in 0..400 {
        runtime
            .dispatch(instance, view, AppEvent::ScrollPage(1))
            .unwrap();
        let screen = compile_settings_view(&runtime, instance, view, &cx);
        if screens.last() == Some(&screen) {
            return screens;
        }
        screens.push(screen);
    }
    panic!("{route:?} {width}\u{d7}{height}: Page Down never reaches the list's end");
}

#[test]
fn every_control_and_word_of_a_system_page_is_on_some_screen() {
    let routes = [
        SettingsRoute::SoftwareUpdate,
        SettingsRoute::About,
        SettingsRoute::Messages,
        SettingsRoute::Packages,
        SettingsRoute::Security,
        SettingsRoute::Harness,
        SettingsRoute::Ai,
        SettingsRoute::KeyboardInput,
        SettingsRoute::WindowTabs,
    ];
    let words = |text: &str| -> Vec<String> {
        painted_words(text)
            .split(|c: char| c.is_whitespace() || c == '\u{b7}')
            .filter(|word| !word.is_empty())
            .map(str::to_string)
            .collect()
    };
    at_each_text_scale(&[1.0, 2.0], "every control and word", |scale| {
        for route in routes {
            let sizes = NARROW_WIDTHS
                .into_iter()
                .flat_map(|width| NARROW_HEIGHTS.map(|height| (width, height)))
                .chain(WIDE_SIZES[..2].iter().copied());
            for (width, height) in sizes {
                {
                    let context = format!("{scale}\u{d7} {route:?} {width}\u{d7}{height}");
                    let mut controls = std::collections::BTreeMap::<String, bool>::new();
                    let mut labels = std::collections::BTreeMap::<String, String>::new();
                    let mut lines = std::collections::BTreeMap::<
                        String,
                        std::collections::BTreeMap<usize, String>,
                    >::new();
                    for screen in
                        [false, true].into_iter().flat_map(|toolless| {
                            paged_screens_of(
                                |runtime| {
                                    if toolless {
                                        assert!(runtime.replace_settings_packages(
                                            packages_without_its_tool(),
                                            3
                                        ));
                                    }
                                    if toolless && route == SettingsRoute::Harness {
                                        assert!(runtime.replace_settings_harness(
                                            super::harness::fixture_landings(),
                                            1
                                        ));
                                    }
                                },
                                route,
                                width,
                                height,
                            )
                        })
                    {
                        for node in &screen.paint {
                            let key = node.key.as_str().to_string();
                            let whole = node.fade.is_none()
                                && node.clip.height >= node.rect.height - 0.5
                                && node.clip.width >= node.rect.width - 0.5;
                            match &node.content {
                                UiContent::Group(GroupSpec {
                                    label: Some(label), ..
                                }) => {
                                    labels.insert(key, label.clone());
                                }
                                UiContent::Text(text) => {
                                    if let Some((block, line)) = key
                                        .rsplit_once("/line-")
                                        .and_then(|(block, n)| Some((block, n.parse().ok()?)))
                                        && whole
                                    {
                                        lines
                                            .entry(block.to_string())
                                            .or_default()
                                            .insert(line, text.text.clone());
                                    }
                                }
                                UiContent::Button(_)
                                | UiContent::Switch(_)
                                | UiContent::Slider(_)
                                | UiContent::TextField(_) => {
                                    *controls.entry(key).or_default() |= whole;
                                }
                                _ => {}
                            }
                        }
                    }
                    let unseen = controls
                        .iter()
                        .filter(|(key, whole)| !**whole && !key.starts_with("settings/nav/"))
                        .map(|(key, _)| key.as_str())
                        .collect::<Vec<_>>();
                    assert!(unseen.is_empty(), "{context}: never whole: {unseen:?}");
                    for (block, painted) in &lines {
                        let Some(label) = labels.get(block) else {
                            continue;
                        };
                        let said = painted.values().cloned().collect::<Vec<_>>();
                        if said.iter().any(|line| line.ends_with('\u{2026}')) {
                            continue;
                        }
                        let (want, have) = (
                            words(label),
                            words(&crate::native_ui::joined_as_named(&said.join("\n"), label)),
                        );
                        if have.first() != want.first() {
                            continue;
                        }
                        let missing = want
                            .iter()
                            .filter(|word| {
                                !have.contains(word) && !have.contains(&word.replace('-', ""))
                            })
                            .collect::<Vec<_>>();
                        assert!(
                            missing.is_empty(),
                            "{context}: {block} never paints {missing:?} of {label:?}"
                        );
                    }
                }
            }
        }
    });
}

/// A Settings runtime holding what every page draws at its fullest.
fn every_page_runtime() -> (
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    every_page_runtime_over(EVERY_PAGE_SOURCES[0])
}

/// The aterm.toml sources the every-page sweep runs over: the default
/// theme, and GitHub Light with a light window (the lab sweep's two), whose
/// rows Modified, Appearance and Home then draw set.
const EVERY_PAGE_SOURCES: [&str; 2] = [
    "agents_auto_prime = false\n",
    "agents_auto_prime = false\ntheme = \"GitHub Light\"\nwindow_theme = \"light\"\n",
];

/// [`every_page_runtime`] over aterm.toml `source`.
pub(super) fn every_page_runtime_over(
    source: &str,
) -> (
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    use crate::harness_screen::{TabComputer, TabControlState, TabSpreadsheetRow};
    let (mut runtime, instance, view) = setup();
    replace_settings_source(&mut runtime, view, source.to_string());
    assert!(runtime.replace_settings_packages(super::live_packages_state(None), 2));
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.arm_config_file(
            Some(std::path::PathBuf::from(
                "/Users//someone/.config/aterm/aterm.toml",
            )),
            Some(std::path::Path::new("/Users//someone")),
            ConfigFileArm::inert(),
        );
        let posture = crate::security_fixtures::named("denied-folders").expect("a posture");
        state.replace_macos_access(crate::native_settings::MacosAccess {
            claimants: Some(crate::security_fixtures::census(true)),
            ..posture
        });
    }
    let mut harness = super::harness::fixture();
    let computer = |id: &str, name: &str, lan: bool| TabComputer {
        id: id.to_string(),
        name: name.to_string(),
        address: if lan {
            "192.168.1.110 \u{b7} m110-MacBook-Pro-M4-Max-48GB-1TB.local".to_string()
        } else {
            "local".to_string()
        },
        os_version: "Mac16,5 \u{b7} Darwin 26 \u{b7} macOS 26.1 \u{b7} OpenSSH_9.9p2".to_string(),
        status: if lan { "advertised" } else { "online" }.to_string(),
        vitals: if lan {
            String::new()
        } else {
            "CPU 14% \u{b7} GPU 6% \u{b7} RAM 38.2/128.0 GiB (30%) \u{b7} up 5d 11h".to_string()
        },
        instances: 1,
        tabs: 2,
        ai_clis: 1,
        active_ai: 1,
    };
    let tab = |key: &str, title: &str, ai: bool, local_id: u64| TabSpreadsheetRow {
        key: key.to_string(),
        computer: "m111-MacBook-Pro-M4-Max-128GB-8TB (this Mac)".to_string(),
        instance: "this aterm (pid 1000)".to_string(),
        place: "Tab #0*! (Window 1) \u{b7} 102\u{d7}29".to_string(),
        sid: "s-7".to_string(),
        title: title.to_string(),
        state: "alive".to_string(),
        program: "Claude Code".to_string(),
        is_ai_cli: ai,
        ai_status: "working: Bash \u{b7} targo test (14s)".to_string(),
        autopilot: "land (supervised)".to_string(),
        human: "12s ago".to_string(),
        vitals: "CPU 8.4% \u{b7} RSS 142 MiB".to_string(),
        show_local_id: Some(local_id),
    };
    harness.tabs = TabControlState {
        computers: vec![
            computer(
                "local",
                "m111-MacBook-Pro-M4-Max-128GB-8TB (this Mac)",
                false,
            ),
            computer(
                "lan/m110-MacBook-Pro-M4-Max-48GB-1TB",
                "m110-MacBook-Pro-M4-Max-48GB-1TB",
                true,
            ),
        ],
        tabs: vec![
            tab("local/7", "fix the build of the settings page", true, 7),
            tab("local/8", "zsh", false, 8),
        ],
    };
    assert!(runtime.replace_settings_harness(harness, 1));
    (runtime, instance, view)
}

/// What a painted node says in words.
fn painted_text(node: &crate::native_ui::PaintNode) -> Option<String> {
    match &node.content {
        UiContent::Text(text) => Some(text.text.clone()),
        UiContent::Button(button) => Some(
            button
                .spec
                .visual_label
                .clone()
                .unwrap_or_else(|| button.spec.label.clone()),
        ),
        UiContent::TextField(field)
            if field.spec.input.is_none()
                && field
                    .spec
                    .visual_value
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty() =>
        {
            field.spec.placeholder.clone()
        }
        _ => None,
    }
}

/// Whether lines `painted` say `words`: a line ending in a hyphen is either a word's own
/// hyphen (a name breaks after its hyphens) or the mark a break inside a word puts there.
fn reads_as(painted: &str, words: &str) -> bool {
    fn says(painted: &[char], words: &[char]) -> bool {
        match (painted, words) {
            ([], []) => true,
            (['-', '\n', rest @ ..], _) => {
                says(rest, words) || words.first() == Some(&'-') && says(rest, &words[1..])
            }
            ([c, rest @ ..], _) if c.is_whitespace() => says(rest, words),
            (_, [c, rest @ ..]) if c.is_whitespace() => says(painted, rest),
            ([p, painted @ ..], [w, words @ ..]) => p == w && says(painted, words),
            _ => false,
        }
    }
    let painted: Vec<char> = painted.chars().collect();
    let words: Vec<char> = words.chars().collect();
    says(&painted, &words)
}

/// `painted` cut inside a word of `whole` (what the same node says where nothing cuts it).
fn cut_inside_a_word(painted: &str, whole: &str) -> bool {
    let line = painted.rsplit('\n').next().unwrap_or(painted);
    let Some(stem) = line.strip_suffix('\u{2026}') else {
        return false;
    };
    if painted_words(painted) == painted_words(whole) {
        return false;
    }
    let stem = stem.trim_start();
    whole.match_indices(stem).next().is_some_and(|(at, _)| {
        whole[at + stem.len()..]
            .chars()
            .next()
            .is_some_and(|next| !next.is_whitespace() && next != '\u{2026}')
    })
}

/// The places on `screen` where words wrapped onto lines break inside a word with no mark.
fn broken_inside_a_word(
    screen: &crate::native_ui::CompiledUi,
    whole: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let mut wholes = std::collections::HashMap::<&str, &str>::new();
    let mut blocks =
        std::collections::BTreeMap::<String, std::collections::BTreeMap<usize, String>>::new();
    let mut broken = Vec::new();
    let inside = |before: &str, after: &str, whole: &str| {
        let before = before.trim_end();
        let after = after.trim_start();
        let (Some(end), Some(start)) = (before.chars().next_back(), after.chars().next()) else {
            return false;
        };
        if matches!(end, '-' | '/' | '\\' | '\u{2026}')
            || matches!(start, '/' | '\\')
            || crate::tray_raster::is_east_asian(end)
            || crate::tray_raster::is_east_asian(start)
        {
            return false;
        }
        let last = before.rsplit(char::is_whitespace).next().unwrap_or(before);
        let first = after.split(char::is_whitespace).next().unwrap_or(after);
        let joined = format!("{last}{first}");
        let spaced = format!("{last} {first}");
        whole.contains(&joined)
            && !whole.contains(&spaced)
            && aterm_grapheme::token_kind(&joined) == aterm_grapheme::TokenKind::Word
    };
    for node in screen.paint.iter().filter(|node| !node.peek) {
        let key = node.key.as_str();
        match &node.content {
            UiContent::Group(GroupSpec {
                label: Some(label), ..
            }) => {
                wholes.insert(key, label.as_str());
            }
            UiContent::Text(text) => {
                if let Some((block, line)) = key
                    .rsplit_once("/line-")
                    .and_then(|(block, n)| Some((block, n.parse::<usize>().ok()?)))
                {
                    blocks
                        .entry(block.to_string())
                        .or_default()
                        .insert(line, text.text.clone());
                }
            }
            UiContent::Button(button) => {
                let painted = button
                    .spec
                    .visual_label
                    .as_deref()
                    .unwrap_or(&button.spec.label);
                let lines = painted.split('\n').collect::<Vec<_>>();
                for pair in lines.windows(2) {
                    if inside(pair[0], pair[1], &button.spec.label) {
                        broken.push(format!("{key}: {painted:?} of {:?}", button.spec.label));
                    }
                }
            }
            _ => {}
        }
    }
    for (block, lines) in &blocks {
        let said = wholes
            .get(block.as_str())
            .filter(|label| !label.is_empty())
            .map(|label| (*label).to_string())
            .unwrap_or_else(|| {
                (0..)
                    .map_while(|line| whole.get(&format!("{block}/line-{line}")))
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            });
        for (line, text) in lines {
            if let Some(next) = lines.get(&(line + 1))
                && inside(text, next, &said)
            {
                broken.push(format!("{block}: {text:?} / {next:?} of {said:?}"));
            }
        }
    }
    broken
}

/// What each node of `route` says where nothing cuts it.
fn whole_words_of(
    prepare: impl FnOnce() -> (
        NativeRuntime,
        crate::native_app::AppInstanceId,
        crate::native_app::ViewId,
    ),
    route: SettingsRoute,
) -> std::collections::HashMap<String, String> {
    let (mut runtime, instance, view) = prepare();
    go_to_route(&mut runtime, instance, view, route);
    let mut whole = std::collections::HashMap::new();
    each_screen(
        &mut runtime,
        instance,
        view,
        &view_cx_at(4_000.0, 2_400.0),
        |compiled, _| {
            for node in &compiled.paint {
                if let Some(text) = painted_text(node) {
                    whole.insert(node.key.as_str().to_string(), text);
                }
            }
        },
    );
    whole
}

/// The nodes under `prefixes` on `screen` that cut a word inside itself ([`cut_inside_a_word`]).
fn words_cut(
    screen: &crate::native_ui::CompiledUi,
    whole: &std::collections::HashMap<String, String>,
    prefixes: &[&str],
) -> Vec<String> {
    let mut cut: Vec<String> = screen
        .fit_failures()
        .into_iter()
        .filter(|line| {
            prefixes
                .iter()
                .any(|prefix| line.contains(&format!("key=\"{prefix}")))
        })
        .collect();
    for node in screen.paint.iter().filter(|node| !node.peek) {
        let key = node.key.as_str();
        if !prefixes.iter().any(|prefix| key.starts_with(prefix)) {
            continue;
        }
        if let (Some(painted), Some(whole)) = (painted_text(node), whole.get(key))
            && cut_inside_a_word(&painted, whole)
        {
            cut.push(format!("{key}: {painted:?} of {whole:?}"));
        }
    }
    cut
}

#[test]
fn every_page_keeps_its_words_and_every_control_whole_on_some_screen() {
    let sizes: Vec<(f32, f32)> = NARROW_WIDTHS
        .into_iter()
        .flat_map(|width| NARROW_HEIGHTS.map(|height| (width, height)))
        .chain(WIDE_SIZES[..3].iter().copied())
        .collect();
    at_each_text_scale(&[1.0, 1.5, 2.0], "every page", |scale| {
        let pages: Vec<(SettingsRoute, &str)> = SettingsRoute::ALL
            .into_iter()
            .flat_map(|route| EVERY_PAGE_SOURCES.map(|source| (route, source)))
            .collect();
        on_workers(&pages, "every page", |&(route, source)| {
            let whole = whole_words_of(|| every_page_runtime_over(source), route);
            for &(width, height) in &sizes {
                let context = format!(
                    "{scale}\u{d7} {route:?} {width}\u{d7}{height} {}",
                    source.trim().replace('\n', "; ")
                );
                let (mut runtime, instance, view) = every_page_runtime_over(source);
                go_to_route(&mut runtime, instance, view, route);
                let mut controls = std::collections::BTreeMap::<String, bool>::new();
                each_screen(
                    &mut runtime,
                    instance,
                    view,
                    &view_cx_at(width, height),
                    |compiled, top| {
                        let failures = compiled.fit_failures();
                        assert!(failures.is_empty(), "{context} screen {top}: {failures:#?}");
                        let broken = broken_inside_a_word(compiled, &whole);
                        assert!(broken.is_empty(), "{context} screen {top}: {broken:#?}");
                        for node in &compiled.paint {
                            let key = node.key.as_str();
                            if !node.peek
                                && let Some(painted) = painted_text(node)
                                && let Some(whole) = whole.get(key)
                            {
                                assert!(
                                    !cut_inside_a_word(&painted, whole),
                                    "{context} screen {top}: {key} paints {painted:?} of {whole:?}"
                                );
                            }
                            if matches!(
                                node.content,
                                UiContent::Button(_)
                                    | UiContent::Switch(_)
                                    | UiContent::Slider(_)
                                    | UiContent::TextField(_)
                            ) {
                                let shown = !node.peek
                                    && node.fade.is_none()
                                    && node.clip.height >= node.rect.height - 0.5
                                    && node.clip.width >= node.rect.width - 0.5;
                                *controls.entry(key.to_string()).or_default() |= shown;
                            }
                        }
                    },
                );
                let unseen = controls
                    .iter()
                    .filter(|(key, shown)| !**shown && !key.starts_with("settings/nav/"))
                    .map(|(key, _)| key.as_str())
                    .collect::<Vec<_>>();
                assert!(unseen.is_empty(), "{context}: never whole: {unseen:?}");
                let drawn: &[&str] = match route {
                    SettingsRoute::Packages => &[
                        "packages/check",
                        "packages/install-default",
                        "packages/uninstall-all",
                    ],
                    SettingsRoute::Security => &[
                        MACOS_ACCESS_OPEN_FDA,
                        MACOS_ACCESS_OPEN_FILES,
                        MACOS_ACCESS_TRASH,
                        MACOS_ACCESS_OPEN_DEVTOOLS,
                    ],
                    _ => &[],
                };
                for key in drawn {
                    assert!(controls.contains_key(*key), "{context}: no {key}");
                }
            }
        });
    });
}

#[test]
fn a_word_of_copy_wider_than_its_box_breaks_with_a_hyphen() {
    let px = 26.0;
    let measure = |text: &str| crate::tray_raster::ui_text_width(text, px);
    let text = "the menu bar and Messages.";
    let width = measure("Messages") + 1.0;
    assert!(
        measure("Messages.") > width + crate::native_ui::TEXT_FIT_SLACK,
        "premise: the word is wider than the box"
    );
    let lines = wrapped_copy_lines(text, px, width);
    assert!(
        lines.iter().all(|line| measure(line) <= width + 0.5),
        "{lines:?}"
    );
    assert!(
        lines.iter().all(|line| line.chars().count() > 1),
        "{lines:?}"
    );
    let rejoined = lines.iter().fold(String::new(), |mut text, line| {
        if text.ends_with('-') {
            text.pop();
        } else if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line);
        text
    });
    assert_eq!(rejoined, text, "{lines:?}");
    assert!(
        lines
            .windows(2)
            .any(|pair| pair[0].ends_with("Mes-") && pair[1] == "sages."),
        "broken at its syllable: {lines:?}"
    );
    let wide = measure("Messages.") + 1.0;
    let walked: Vec<String> = crate::tray_raster::ui_text_wrap_ranges(text, px, wide)
        .into_iter()
        .map(|range| text[range].trim_end().to_string())
        .collect();
    assert_eq!(wrapped_copy_lines(text, px, wide), walked);
    let cjk = "設定ファイルを開いて保存します";
    let lines = wrapped_copy_lines(cjk, px, measure("設定ファイ"));
    assert!(lines.len() > 1, "{lines:?}");
    assert!(lines.iter().all(|line| !line.ends_with('-')), "{lines:?}");
}

#[test]
fn no_line_is_a_lone_letter_a_syllable_after_it_could_fill() {
    let px = 26.0;
    let measure = |text: &str| crate::tray_raster::ui_text_width(text, px);
    let text = "A program's colors last for its session and are never saved; off refuses them.";
    let least = measure("A pro-").max(measure("program's")).ceil();
    let most = measure("A program's");
    assert!(least < most, "premise: a box leaves \"A\" alone");
    let mut width = least;
    while width < most {
        let context = format!("{width}pt");
        for lines in [
            wrapped_copy_lines(text, px, width),
            wrap_to_box(text, width, &measure),
        ] {
            assert!(
                lines.iter().all(|line| line.trim() != "A"),
                "{context}: {lines:?}"
            );
            assert_eq!(
                lines.first().map(String::as_str),
                Some("A pro-"),
                "{context}"
            );
            assert!(
                lines
                    .iter()
                    .all(|line| measure(line) <= width + crate::native_ui::TEXT_FIT_SLACK),
                "{context}: {lines:?}"
            );
            let rejoined = lines.join(" ").replace("- ", "");
            assert_eq!(rejoined, text, "{context}: {lines:?}");
        }
        width += 2.0;
    }
    let code = "A aterm.toml key";
    let lines = wrap_to_box(code, measure("aterm.toml") + 1.0, &measure);
    assert_eq!(lines.first().map(String::as_str), Some("A"), "{lines:?}");
}

#[test]
fn a_spaced_slash_never_starts_a_line() {
    let px = 26.0;
    let measure = |text: &str| crate::tray_raster::ui_text_width(text, px);
    for text in [
        "LIVE SYSTEM VITALS (CPU / GPU / RAM / DISK / UPTIME / RTT)",
        "Removes eligible inactive files toward a 64 MiB / 2,048-file cap.",
    ] {
        let least = text
            .split_whitespace()
            .map(|word| measure(&format!("{word} /")))
            .fold(0.0_f32, f32::max)
            .ceil();
        let mut width = least;
        while width < measure(text) {
            let context = format!("{width}pt {text:?}");
            for lines in [
                wrap_to_box(text, width, &measure),
                wrapped_copy_lines(text, px, width),
            ] {
                assert!(
                    lines.iter().all(|line| !line.starts_with('/')),
                    "{context}: {lines:?}"
                );
                assert_eq!(lines.join(" "), text, "{context}");
            }
            width += 3.0;
        }
    }
}

#[test]
fn a_word_wider_than_its_box_breaks_at_a_syllable() {
    let words = [
        "Brightness",
        "AUTOMATIC",
        "NOTIFICATIONS",
        "MAINTENANCE",
        "Foreground",
        "Background",
        "SUPERVISION",
        "COMPUTERS",
        "CONTINUED",
        "ACCESSIBLE",
        "PERMISSIONS",
        "SPREADSHEET",
        "Messages.",
        "appearance",
        "translucent",
        "Lightning",
        "underline",
        "Problems",
        "Antigravity",
        "introspectable.",
        "automatically",
        "registration",
    ];
    at_each_text_scale(&[1.5, 2.0], "syllable breaks", |scale| {
        let faces = [
            crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Primary),
            crate::native_ui::text_paint_metrics(SemanticRole::Heading, StyleRef::Quiet),
            crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Quiet),
        ];
        for (px, face) in faces {
            let measure = |line: &str| crate::tray_raster::ui_text_width_for(face, line, px);
            for word in words {
                let points = aterm_grapheme::hyphen_points(word);
                assert!(!points.is_empty(), "premise: {word:?} has syllables");
                let narrowest = std::iter::once(0)
                    .chain(points.iter().copied())
                    .zip(points.iter().copied().chain(std::iter::once(word.len())))
                    .map(|(from, to)| measure(&format!("{}-", &word[from..to])))
                    .fold(0.0_f32, f32::max);
                let mut width = narrowest.ceil();
                while width < measure(word) {
                    let context = format!("{scale}\u{d7} {px}px {width}pt {word:?}");
                    let lines = wrap_to_box(word, width, &measure);
                    let mut at = 0;
                    for line in &lines[..lines.len() - 1] {
                        let stem = line.strip_suffix('-').unwrap_or_else(|| {
                            panic!("{context}: a broken line ends in a hyphen: {lines:?}")
                        });
                        at += stem.len();
                        assert!(
                            points.contains(&at),
                            "{context}: broken off a syllable: {lines:?}"
                        );
                    }
                    for line in &lines {
                        assert!(
                            measure(line) <= width + crate::native_ui::TEXT_FIT_SLACK,
                            "{context}: {line:?} is wider than its box"
                        );
                    }
                    let rejoined: String = lines
                        .iter()
                        .map(|line| line.strip_suffix('-').unwrap_or(line))
                        .collect();
                    assert_eq!(rejoined, word, "{context}");
                    width += 3.0;
                }
            }
            let heading = "HARDWARE \u{b7} OS & COMPUTER \u{b7} STATUS";
            let least = measure("PUTER \u{b7}").max(measure("WARE \u{b7}")).ceil();
            for width in (0..8).map(|step| least + 6.0 * step as f32) {
                for line in wrap_to_box(heading, width, &measure) {
                    let letters = line
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .trim_end_matches('-');
                    assert!(
                        letters.chars().count() != 1 || letters == "&",
                        "{width}pt: a lone letter {line:?}"
                    );
                }
            }
        }
    });
}

#[test]
fn an_identifier_no_line_holds_breaks_with_no_mark_in_it() {
    at_each_text_scale(&[2.0], "identifiers", |_| {
        let facts = [
            (
                "compiler",
                "Trust 0.11.0+g20565c325 \u{b7} release \u{b7} trust_verify",
            ),
            ("signature", "ec73ab1c007d90ca"),
            ("site", "alab.systems"),
            ("running", "/Users//someone/aterm-final/target/debug/aterm"),
            ("another copy", "/Applications/aterm.app (0.109.0)"),
        ];
        let px = 13.0 * crate::native_appearance::text_scale();
        for width in [126.0_f32, 182.0] {
            for (label, value) in facts {
                let context = format!("{width}pt {label}");
                let lines = metadata_value_lines(label, value, width);
                let bare = |text: &str| text.replace(FACT_SEPARATOR, "").replace(' ', "");
                assert_eq!(
                    bare(&lines.concat()),
                    bare(value),
                    "{context}: nothing is put in it: {lines:?}"
                );
                for line in &lines {
                    assert!(
                        metadata_value_text_width(line, px, false)
                            <= width + crate::native_ui::TEXT_FIT_SLACK,
                        "{context}: {line:?} is wider than its column"
                    );
                    assert!(
                        !line.starts_with('-'),
                        "{context}: {line:?} starts with a hyphen, which ends its line: {lines:?}"
                    );
                }
            }
        }
        let measure = |line: &str| metadata_value_text_width(line, px, false);
        let narrow = measure("alab.syst");
        assert_eq!(
            wrap_to_box("alab.systems", narrow, &measure),
            ["alab", ".systems"]
        );
        let version = wrap_to_box("0.11.0+g20565c325", measure("0.11.0+g2056"), &measure);
        assert_eq!(version, ["0.11.0", "+g20565c325"]);
        let (heading_px, heading_face) =
            crate::native_ui::text_paint_metrics(SemanticRole::Heading, StyleRef::Quiet);
        let heading =
            |line: &str| crate::tray_raster::ui_text_width_for(heading_face, line, heading_px);
        let caps = wrap_to_box("ONLY IN ATERM.TOML", heading("ATERM.TO"), &heading);
        assert!(
            caps.iter().all(|line| !line.ends_with('-')) && caps.contains(&".TOML".to_string()),
            "{caps:?}"
        );
        let name = wrap_to_box(
            "Architecture",
            fact_name_width("Architec"),
            &fact_name_width,
        );
        assert_eq!(name, ["Archi-", "tecture"]);
    });
}

#[test]
fn a_short_word_is_never_broken_in_a_control_with_room_for_it() {
    at_each_text_scale(&[2.0], "short words", |_| {
        let spec = ButtonSpec::new("Choose the foreground color").visual_label("Choose\u{2026}");
        let room = crate::native_ui::text_button_label_room(&spec, 122.0);
        assert!(
            crate::native_ui::native_button_label_width("Choose\u{2026}") > room
                && crate::native_ui::native_button_label_width("Choose") <= room,
            "premise: only the ellipsis does not fit"
        );
        assert_eq!(
            button_words_in(&spec, "Choose\u{2026}", &[], 122.0),
            ("Choose".to_string(), 1)
        );
        let (mut runtime, instance, view) = setup();
        go_to_route(&mut runtime, instance, view, SettingsRoute::Appearance);
        let mut fields = 0;
        each_screen(
            &mut runtime,
            instance,
            view,
            &view_cx_at(178.0, 488.0),
            |screen, top| {
                for node in screen.paint.iter().filter(|node| !node.peek) {
                    let UiContent::TextField(_) = &node.content else {
                        continue;
                    };
                    let colour = [
                        prefs::EDIT_FOREGROUND,
                        prefs::EDIT_BACKGROUND,
                        prefs::EDIT_CURSOR_COLOR,
                        prefs::EDIT_SELECTION_COLOR,
                    ]
                    .iter()
                    .any(|key| node.key.as_str() == format!("settings/control/{key}"));
                    if !colour {
                        continue;
                    }
                    let Some(painted) = painted_text(node) else {
                        continue;
                    };
                    fields += 1;
                    assert!(
                        !painted.is_empty()
                            && painted
                                .lines()
                                .all(|line| !line.ends_with('-') && !line.ends_with('\u{2026}')),
                        "screen {top}: {} paints {painted:?}",
                        node.key.as_str()
                    );
                }
            },
        );
        assert!(fields > 0, "premise: a colour field is drawn");
        let title = "Antigravity CLI is not installed";
        let (mut runtime, instance, view) = super::setup_with_messages();
        let mut messages = super::sample_messages();
        for entry in &mut messages.entries {
            if entry.id == 40 {
                entry.title = title.to_string();
            }
        }
        assert!(runtime.replace_settings_messages(messages, 8));
        let mut seen = false;
        super::scroll_messages_through(
            &mut runtime,
            instance,
            view,
            &view_cx_at(178.0, 488.0),
            1,
            |screen, k| {
                let mut lines: Vec<(String, String)> = screen
                    .paint
                    .iter()
                    .filter(|node| {
                        !node.peek
                            && node
                                .key
                                .as_str()
                                .starts_with("settings/messages/row/40/label")
                    })
                    .filter_map(|node| match &node.content {
                        UiContent::Text(text) => {
                            Some((node.key.as_str().to_string(), text.text.clone()))
                        }
                        _ => None,
                    })
                    .collect();
                if lines.is_empty() {
                    return;
                }
                seen = true;
                lines.sort();
                let lines: Vec<String> = lines.into_iter().map(|(_, line)| line).collect();
                assert_eq!(lines.join(" "), title, "screen {k}: {lines:?}");
            },
        );
        assert!(seen, "premise: the head is drawn");
    });
}

#[test]
fn a_path_in_a_status_breaks_before_its_folders_slashes() {
    let path = "/private/tmp/lab-42/-Users-someone-aterm/75d5bccd-9c38-431b-99e5-ef81935c6fbd/scratchpad/labs/fv-sk105-l80/cfg/aterm";
    let text = format!(
        "Allow programs to read the clipboard was not saved: {path} isn't writable \u{b7} check its permissions"
    );
    at_each_text_scale(&[1.0, 2.0], "status path", |scale| {
        let widest = path
            .split('/')
            .map(|folder| status_words_width(&format!("/{folder}")))
            .fold(0.0_f32, f32::max);
        let from = widest.ceil() as usize;
        for width in (from..=1100.max(from))
            .step_by(17)
            .map(|width| width as f32)
        {
            let context = format!("{scale}\u{d7} {width}pt");
            let lines = status_word_lines(&text, width);
            let mut in_path = false;
            for line in &lines {
                if in_path {
                    assert!(line.starts_with('/'), "{context}: {lines:#?}");
                }
                assert!(
                    !line.ends_with('/') && !line.ends_with('-'),
                    "{context}: a line ends in a separator: {lines:#?}"
                );
                let last = line.split_whitespace().last().unwrap_or_default();
                in_path = path.contains(last) && !path.ends_with(last);
            }
            for whole in ["lab-42", "75d5bccd-9c38"] {
                assert!(
                    lines.iter().any(|line| line.contains(whole)),
                    "{context}: {whole} broken: {lines:#?}"
                );
            }
        }
    });
}

#[test]
fn a_cursor_style_says_its_whole_name_and_description() {
    at_each_text_scale(&[1.0, 2.0], "cursor style words", |scale| {
        let sizes: &[(f32, f32)] = if scale > 1.5 {
            &[
                (584.0, 348.0),
                (744.0, 420.0),
                (1_095.0, 700.0),
                (178.0, 348.0),
            ]
        } else {
            &[(178.0, 348.0), (178.0, 516.0)]
        };
        for &(width, height) in sizes {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let (mut runtime, instance, view) = setup();
            go_to_route(&mut runtime, instance, view, SettingsRoute::CursorStyles);
            let styles = match runtime.view_state(view) {
                Some(AppViewState::Settings(state)) => listed_cursor_styles(state),
                _ => unreachable!(),
            };
            let mut said: std::collections::BTreeMap<String, Vec<String>> = Default::default();
            each_screen(
                &mut runtime,
                instance,
                view,
                &view_cx_at(width, height),
                |compiled, _| {
                    for node in compiled.paint.iter().filter(|node| !node.peek) {
                        if let UiContent::Text(text) = &node.content {
                            said.entry(node.key.as_str().to_string())
                                .or_default()
                                .push(text.text.clone());
                        }
                    }
                },
            );
            let block = |key: &str| -> String {
                let mut lines: Vec<(usize, String)> = said
                    .iter()
                    .filter_map(|(drawn, texts)| {
                        let index = if drawn == key {
                            0
                        } else {
                            drawn
                                .strip_prefix(key)?
                                .strip_prefix("/line-")?
                                .parse()
                                .ok()?
                        };
                        Some((index, texts.last()?.clone()))
                    })
                    .collect();
                lines.sort();
                painted_words(
                    &lines
                        .into_iter()
                        .map(|(_, line)| line)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            };
            let letters = |text: &str| -> String {
                text.chars()
                    .filter(|c| !c.is_whitespace() && *c != '-')
                    .collect()
            };
            for style in styles {
                let key = cursor_style_row_key(&style.value);
                assert_eq!(
                    letters(&block(&format!("{key}/name"))),
                    letters(&style.label()),
                    "{context}"
                );
                assert_eq!(
                    letters(&block(&format!("{key}/description"))),
                    letters(&style.description),
                    "{context}: {key}"
                );
            }
        }
    });
}

#[test]
fn the_cursor_link_says_its_whole_label() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "cursor link", |scale| {
        for (width, height) in [(584.0, 348.0), (178.0, 348.0), (744.0, 420.0)] {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let screens = screens_of(|_, _| {}, SettingsRoute::Home, width, height);
            let mut lines = std::collections::BTreeMap::<String, String>::new();
            for screen in &screens {
                for node in screen.paint.iter().filter(|node| !node.peek) {
                    if node
                        .key
                        .as_str()
                        .starts_with("settings/cursor-style-link/label")
                        && let UiContent::Text(text) = &node.content
                    {
                        lines.insert(node.key.as_str().to_string(), text.text.clone());
                    }
                }
            }
            let mut ordered: Vec<(usize, String)> = lines
                .into_iter()
                .map(|(key, text)| {
                    let index = key
                        .rsplit_once("/line-")
                        .and_then(|(_, n)| n.parse().ok())
                        .unwrap_or(0);
                    (index, text)
                })
                .collect();
            ordered.sort();
            let label = painted_words(
                &ordered
                    .into_iter()
                    .map(|(_, text)| text)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            assert_eq!(
                label,
                format!(
                    "Cursor style: {} (default)",
                    cursor_style_name(crate::app_config::default_cursor_trail_style())
                ),
                "{context}"
            );
            let button = screens
                .iter()
                .flat_map(|screen| &screen.paint)
                .find_map(|node| match &node.content {
                    UiContent::Button(button)
                        if node.key.as_str() == "settings/control/cursor_trail_style" =>
                    {
                        button.spec.visual_label.clone()
                    }
                    _ => None,
                })
                .unwrap_or_default();
            assert_eq!(
                button.split_whitespace().collect::<Vec<_>>(),
                ["More", "styles\u{2026}"],
                "{context}: {button:?}"
            );
            for screen in &screens {
                let spilled = spilled_nodes(
                    screen,
                    &[
                        "settings/cursor-style-link",
                        "settings/control/cursor_trail_style",
                    ],
                );
                assert!(spilled.is_empty(), "{context}: {spilled:#?}");
            }
        }
    });
}

#[test]
fn wallpaper_keeps_its_status_and_its_buttons() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "wallpaper", |scale| {
        for (width, height) in NARROW_WIDTHS
            .into_iter()
            .map(|width| (width, 348.0))
            .chain(WIDE_SIZES[..3].iter().copied())
        {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let whole = whole_words_of(setup, SettingsRoute::Wallpaper);
            for screen in screens_of(|_, _| {}, SettingsRoute::Wallpaper, width, height) {
                let cut = words_cut(&screen, &whole, &["settings/wallpaper/"]);
                assert!(cut.is_empty(), "{context}: {cut:#?}");
                let spilled = spilled_nodes(&screen, &["settings/wallpaper/"]);
                assert!(spilled.is_empty(), "{context}: {spilled:#?}");
            }
        }
    });
}

#[test]
fn a_path_field_keeps_its_file_name() {
    let long = "/Volumes/Work Disk/Shared Configuration/profiles/someone/settings/aterm";
    at_each_text_scale(&[1.0, 2.0], "fleet path", |scale| {
        for (folder, home) in [
            ("/Users//someone/.config/aterm", "/Users//someone"),
            (long, "/Users//someone"),
        ] {
            for width in NARROW_WIDTHS {
                let context = format!("{scale}\u{d7} {width} {folder}");
                let (mut runtime, instance, view) = every_page_runtime();
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!()
                    };
                    state.arm_config_file(
                        Some(std::path::PathBuf::from(format!("{folder}/aterm.toml"))),
                        Some(std::path::Path::new(home)),
                        ConfigFileArm::inert(),
                    );
                }
                go_to_route(&mut runtime, instance, view, SettingsRoute::Ai);
                let key = UiKey::new(format!("settings/control/{}", prefs::EDIT_AI_FLEET));
                let (mut seen, mut whole) = (false, false);
                each_screen(
                    &mut runtime,
                    instance,
                    view,
                    &view_cx_at(width, 516.0),
                    |screen, _| {
                        let Some(node) = screen
                            .paint
                            .iter()
                            .find(|node| node.key == key && !node.peek)
                        else {
                            return;
                        };
                        seen = true;
                        whole |= node.clip.height >= node.rect.height - 0.5;
                        let painted = painted_text(node).unwrap_or_default();
                        assert!(
                            painted.replace('\n', "").ends_with("ai-fleet.toml"),
                            "{context}: {painted:?}"
                        );
                        assert!(
                            painted.lines().count() <= 2 || painted.starts_with("\u{2026}/ai"),
                            "{context}: {painted:?}"
                        );
                        let value = screen.semantic(&key).map(|node| node.value.clone());
                        assert_eq!(
                            value,
                            Some(SemanticValue::Text(
                                std::path::Path::new(folder)
                                    .join("ai-fleet.toml")
                                    .display()
                                    .to_string()
                            )),
                            "{context}"
                        );
                    },
                );
                assert!(seen, "{context}: the field is drawn");
                assert!(whole, "{context}: the field is whole on some screen");
            }
        }
    });
    let path = "/Users//someone/Library/Application Support/dotfiles/ai-fleet.toml";
    let field = Control::new(
        TextFieldSpec {
            label: "Fleet document".to_string(),
            placeholder: None,
            secret: false,
            visual_value: Some(path.to_string()),
            input: None,
            swatch: None,
            swatch_line: false,
            trailing_hint: None,
            description: None,
            end_button: false,
        },
        ActionId::new("fleet"),
    )
    .value(SemanticValue::Text(path.to_string()));
    let width =
        crate::native_ui::text_field_width_for_text("/Users/…/dotfiles/ai-fleet.toml") + 2.0;
    let compiled = crate::native_ui::UiTree::new(
        UiNode::new("fleet", UiContent::TextField(field)).layout(
            Layout::default()
                .width(Length::Fixed(width))
                .height(Length::Fixed(32.0)),
        ),
    )
    .compile(LogicalRect::new(0.0, 0.0, width, 32.0))
    .unwrap();
    let line = compiled
        .text_fit_audit_line(&UiKey::new("fleet"))
        .unwrap_or_default();
    assert!(
        line.contains("painted=\"/Users/…/dotfiles/ai-fleet.toml\""),
        "{line}"
    );
}

#[test]
fn a_pinned_summary_never_starves_the_list() {
    at_each_text_scale(&[1.5, 2.0], "Modified summary", |scale| {
        for width in [178.0, 192.0, 234.0, 276.0] {
            let context = format!("{scale}\u{d7} {width}");
            let (mut runtime, instance, view) = every_page_runtime();
            go_to_route(&mut runtime, instance, view, SettingsRoute::Modified);
            let mut whole = std::collections::BTreeSet::new();
            each_screen(
                &mut runtime,
                instance,
                view,
                &view_cx_at(width, 348.0),
                |screen, _| {
                    for node in &screen.paint {
                        if node.key.as_str().starts_with("settings/modified/")
                            && matches!(node.content, UiContent::Button(_))
                            && node.clip == node.rect
                            && node.fade.is_none()
                        {
                            whole.insert(node.key.as_str().to_string());
                        }
                    }
                },
            );
            for key in [
                "settings/modified/reset-all",
                "settings/modified/manual/reset/agents-auto-prime",
                "settings/modified/manual/open/agents-auto-prime",
            ] {
                assert!(
                    whole.contains(key),
                    "{context}: {key} on no screen: {whole:?}"
                );
            }
        }
    });
    at_each_text_scale(&[1.0], "Modified summary control", |_| {
        let (runtime, instance, view) = every_page_runtime();
        let mut runtime = runtime;
        go_to_route(&mut runtime, instance, view, SettingsRoute::Modified);
        let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(584.0, 348.0));
        let list = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == "settings/list")
            .map(|node| node.rect)
            .expect("the list");
        let summary = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == "settings/modified/summary")
            .map(|node| node.rect)
            .expect("the summary");
        assert!(summary.bottom() <= list.y + 0.5, "pinned over the list");
    });
}

#[test]
fn a_narrow_row_keeps_its_words_and_its_controls_in_its_card() {
    at_each_text_scale(&[1.5, 2.0], "narrow rows", |scale| {
        for width in [178.0, 192.0] {
            for route in [
                SettingsRoute::Home,
                SettingsRoute::Sound,
                SettingsRoute::Appearance,
                SettingsRoute::Terminal,
                SettingsRoute::TabColor,
                SettingsRoute::TextFonts,
            ] {
                let context = format!("{scale}\u{d7} {route:?} {width}");
                let whole = whole_words_of(setup, route);
                for screen in screens_of(|_, _| {}, route, width, 348.0) {
                    let failures = screen.fit_failures();
                    assert!(failures.is_empty(), "{context}: {failures:#?}");
                    let cut = words_cut(
                        &screen,
                        &whole,
                        &[
                            "settings/control/",
                            "settings/color-open/",
                            "settings/color/",
                            "settings/display-face/",
                        ],
                    );
                    assert!(cut.is_empty(), "{context}: {cut:#?}");
                    let spilled = spilled_nodes(&screen, &["settings/display-face/"]);
                    assert!(spilled.is_empty(), "{context}: {spilled:#?}");
                }
            }
        }
    });
}

#[test]
fn the_themes_page_keeps_its_words_and_buttons_in_its_panel() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "themes narrow", |scale| {
        for width in [178.0, 192.0] {
            for height in NARROW_HEIGHTS {
                let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
                let whole = whole_words_of(setup, SettingsRoute::Themes);
                for screen in screens_of(|_, _| {}, SettingsRoute::Themes, width, height) {
                    let failures = screen.fit_failures();
                    assert!(failures.is_empty(), "{context}: {failures:#?}");
                    let cut = words_cut(
                        &screen,
                        &whole,
                        &[
                            "settings/showcase/card/",
                            "settings/showcase/footer/",
                            "settings/showcase/customize",
                            "settings/showcase/pack",
                            "settings/showcase/open-folder",
                        ],
                    );
                    assert!(cut.is_empty(), "{context}: {cut:#?}");
                    let spilled = spilled_nodes(&screen, &["settings/showcase/"]);
                    assert!(spilled.is_empty(), "{context}: {spilled:#?}");
                }
            }
        }
    });
}

/// Follow system's name stands beside its switch where it fits one line there, else over it in a
/// row of its own, never a word broken: the chooser's list stands on whole rows, and a name wrapped
/// beside the switch made a row of a line and a half that left the list's end short of its edge.
#[test]
fn follow_systems_name_goes_over_its_switch_before_it_breaks_a_word() {
    let footer = || {
        let (mut runtime, instance, view) = setup();
        replace_settings_source(&mut runtime, view, "theme = \"Default\"\n".to_string());
        go_to_route(&mut runtime, instance, view, SettingsRoute::Themes);
        let label = "settings/showcase/footer/follow-label";
        let (mut lines, mut over, mut whole) = (Vec::new(), false, false);
        each_screen(
            &mut runtime,
            instance,
            view,
            &view_cx_at(178.0, 348.0),
            |screen, _| {
                let drawn = screen
                    .paint
                    .iter()
                    .filter(|node| {
                        node.key.as_str() == label
                            || node.key.as_str().starts_with(&format!("{label}/"))
                    })
                    .filter_map(|node| match &node.content {
                        UiContent::Text(text) => Some((text.text.clone(), node.rect, node.clip)),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let Some(switch) = screen
                    .paint
                    .iter()
                    .find(|node| node.key.as_str() == "settings/showcase/follow-system")
                else {
                    return;
                };
                if drawn.is_empty() {
                    return;
                }
                lines = drawn.iter().map(|(text, ..)| text.clone()).collect();
                over = drawn
                    .iter()
                    .all(|(_, rect, _)| rect.bottom() <= switch.rect.y + 0.5);
                whole |=
                    switch.clip == switch.rect && drawn.iter().all(|(_, rect, clip)| clip == rect);
            },
        );
        (lines, over, whole)
    };
    at_each_text_scale(&[1.0, 2.0], "follow system", |scale| {
        let (lines, over, whole) = footer();
        assert_eq!(
            lines.join(" "),
            "Follow system",
            "{scale}x: its words whole: {lines:?}"
        );
        assert!(
            over,
            "{scale}x: over its switch, which leaves it no line beside"
        );
        assert!(
            whole,
            "{scale}x: the name and the switch whole on a screen: {lines:?}"
        );
    });
}

/// The chooser's Dracula row with 2× text, walked from a 30-column window narrower by 4pt steps
/// (less than the check's room, its 16pt and gap) to the first width whose name in use wraps
/// beside its check: there the name not in use gives up the empty slot and is whole in its room.
/// The face decides where that width falls (Helvetica Neue's at 178pt, Segoe UI's narrower), so
/// the walk finds it on every face, and a face where the name never wraps fails the walk.
#[test]
fn a_theme_not_in_use_gives_its_name_the_check_slot() {
    let row = |source: &str, width: f32| {
        let (mut runtime, instance, view) = setup();
        replace_settings_source(&mut runtime, view, source.to_string());
        go_to_route(&mut runtime, instance, view, SettingsRoute::Themes);
        {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!()
            };
            state.showcase.filter =
                crate::native_text_input::TextInputState::new("Dracula".to_string());
        }
        let name = "settings/showcase/card/dracula/name";
        let text = |node: &crate::native_ui::PaintNode| match &node.content {
            UiContent::Text(text) => Some(text.text.clone()),
            _ => None,
        };
        let (mut lines, mut check) = (Vec::new(), None);
        each_screen(
            &mut runtime,
            instance,
            view,
            &view_cx_at(width, 516.0),
            |screen, _| {
                let drawn = screen
                    .paint
                    .iter()
                    .filter(|node| {
                        node.key.as_str() == name
                            || node.key.as_str().starts_with(&format!("{name}/"))
                    })
                    .filter_map(text)
                    .collect::<Vec<_>>();
                if !drawn.is_empty() {
                    lines = drawn;
                    check = screen
                        .paint
                        .iter()
                        .find(|node| node.key.as_str() == "settings/showcase/card/dracula/check")
                        .and_then(text);
                }
            },
        );
        (lines, check)
    };
    at_each_text_scale(&[2.0], "chooser check slot", |_| {
        const FROM: f32 = 234.0;
        let mut width = FROM;
        loop {
            assert!(
                width >= 100.0,
                "the name in use never wraps beside its check (whole from {FROM}pt down to {}pt)",
                width + 4.0
            );
            let (used, check) = row("theme = \"Dracula\"\n", width);
            assert_eq!(check.as_deref(), Some("\u{2713}"), "{width}pt: {used:?}");
            let (lines, check) = row("theme = \"Default\"\n", width);
            if used == ["Dracula"] {
                assert_eq!(
                    lines,
                    ["Dracula"],
                    "{width}pt: the name whole beside its slot"
                );
                assert_eq!(check.as_deref(), Some(""), "{width}pt: the slot kept");
            } else {
                assert_eq!(
                    check, None,
                    "{width}pt: the empty slot is given up where the name in use wraps: {used:?}"
                );
                assert_eq!(
                    lines,
                    ["Dracula"],
                    "{width}pt: the slot's room makes the name whole"
                );
                break;
            }
            width -= 4.0;
        }
        assert!(
            width < FROM,
            "the walk begins where the name is whole beside its check: it wraps at {width}pt"
        );
    });
}

#[test]
fn a_wrapped_line_of_words_paints_no_box() {
    at_each_text_scale(&[2.0], "wrapped words box", |_| {
        let mut wrapped = 0;
        for screen in screens_of(|_, _| {}, SettingsRoute::Packages, 178.0, 348.0) {
            for node in &screen.paint {
                let key = node.key.as_str();
                if !key.starts_with("packages/activity/")
                    || key.ends_with("/above")
                    || key.ends_with("/below")
                {
                    continue;
                }
                if key.ends_with("/line-1") {
                    wrapped += 1;
                }
                if let UiContent::Group(group) = &node.content {
                    assert!(
                        !matches!(
                            group.style,
                            StyleRef::Primary | StyleRef::Secondary | StyleRef::Code
                        ),
                        "{} paints a box",
                        node.key.as_str()
                    );
                }
            }
        }
        assert!(wrapped > 0, "premise: the activity line wraps");
    });
}

#[test]
fn harness_lists_a_tall_session_row_by_the_one_splitter() {
    use crate::native_app::SemanticInput;
    at_each_text_scale(&[2.0], "session parts", |_| {
        let (mut runtime, instance, view) = setup();
        assert!(runtime.replace_settings_harness(super::harness::fixture(), 1));
        go_to_route(&mut runtime, instance, view, SettingsRoute::Harness);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(crate::harness_screen::HARNESS_LAND),
                    value: Some(SemanticInput::Text("session/7".to_string())),
                }),
            )
            .unwrap();
        let cx = view_cx_at(178.0, 348.0);
        let mut pieces = std::collections::BTreeMap::new();
        each_screen(&mut runtime, instance, view, &cx, |screen, _| {
            for node in &screen.paint {
                let key = node.key.as_str();
                if key == "harness/session/7/block"
                    || key.starts_with("harness/session/7/block/part-")
                {
                    pieces.insert(key.to_string(), node.joins);
                }
            }
        });
        assert!(
            pieces.len() > 1,
            "premise: the row is listed in parts: {pieces:?}"
        );
        let last = pieces.len() - 1;
        for (index, (key, joins)) in pieces.iter().enumerate() {
            assert_eq!(joins.above, index > 0, "{key}: {joins:?}");
            assert_eq!(joins.below.is_some(), index < last, "{key}: {joins:?}");
        }
    });
}

/// The wraps of Latin Settings copy as `tests/fixtures/latin-wraps.txt` records them.
fn latin_wraps() -> String {
    let corpus = [
        "Cursor blink",
        "Show the tab bar when only one tab is open",
        "aarch64-apple-darwin",
        "~/.config/aterm/aterm.toml \u{b7} edited 3 minutes ago",
        "https://github.com/alabsystems/aterm/releases --columns 120",
        "Supervisor architecture \u{b7} Off \u{b7} from theme Dracula",
        "Pneumonoultramicroscopicsilicovolcanoconiosis is long",
        "a  b\tc\nd",
        "",
        "trailing space ",
        "/usr/local/bin/aterm-lab-instance-runner",
        "x-y--z//w/-v",
        "Couldn't use your keybindings file \u{b7} line 3",
    ];
    let mut out = String::new();
    for text in corpus {
        out.push_str(&format!("P {text:?} => {:?}\n", line_pieces(text)));
        for width in [6.0_f32, 10.0, 16.0, 24.0, 40.0] {
            let lines = wrap_to_box(text, width, &|s: &str| s.chars().count() as f32);
            out.push_str(&format!("W {text:?} {width} => {lines:?}\n"));
        }
        for width in [60.0_f32, 100.0, 160.0] {
            let ranges = crate::tray_raster::ui_text_wrap_ranges(text, 13.0, width);
            out.push_str(&format!("R {text:?} {width} => {ranges:?}\n"));
        }
    }
    out
}

#[test]
fn latin_settings_copy_wraps_as_recorded() {
    let golden = include_str!("../tests/fixtures/latin-wraps.txt");
    let now = latin_wraps();
    let moved: Vec<(&str, &str)> = golden
        .lines()
        .zip(now.lines())
        .filter(|(was, is)| was != is)
        .take(8)
        .collect();
    assert!(moved.is_empty(), "moved wraps (was, is): {moved:#?}");
    assert_eq!(golden.lines().count(), now.lines().count());
}

/// Characters a line may not start with ([`aterm_grapheme::line_breaks`]' kinsoku).
const NO_LINE_START: [char; 6] = [
    '\u{3002}', '\u{3001}', '\u{300d}', '\u{ff0c}', '\u{30fc}', '\u{30a1}',
];

#[test]
fn cjk_settings_copy_wraps_between_its_characters() {
    let samples = [
        "設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません。",
        "无法保存设置文件，请检查文件夹的权限，然后再试一次。",
        "コーヒーブレイク中にアップデートをダウンロードしています",
    ];
    at_each_text_scale(&[1.0, 2.0], "CJK copy", |scale| {
        let (px, face) =
            crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Primary);
        let measure = |line: &str| crate::tray_raster::ui_text_width_for(face, line, px);
        for text in samples {
            for width in (48..=318).step_by(6).map(|width| width as f32) {
                let context = format!("{scale}\u{d7} {width}pt {text:?}");
                let names = row_name_lines(text, width);
                let copy = wrapped_copy_lines(text, px, width);
                for (what, lines, slack) in [
                    ("name", &names, crate::native_ui::TEXT_FIT_SLACK),
                    ("copy", &copy, 0.5),
                ] {
                    assert!(!lines.is_empty(), "{context}: no {what} lines");
                    for line in lines.iter() {
                        assert!(
                            !line.is_empty(),
                            "{context}: an empty {what} line {lines:?}"
                        );
                        assert!(
                            !line.ends_with('-'),
                            "{context}: a hyphen in a CJK {what} {lines:?}"
                        );
                        assert!(
                            measure(line) <= width.max(48.0) + slack || line.chars().count() == 1,
                            "{context}: {what} line {line:?} is wider than its box"
                        );
                    }
                    for pair in lines.windows(2) {
                        let (line, next) = (&pair[0], &pair[1]);
                        let first = next.chars().next().expect("a line has a character");
                        assert!(
                            !NO_LINE_START.contains(&first),
                            "{context}: {what} line {next:?} starts with {first:?}"
                        );
                        assert!(
                            !line.ends_with(['\u{300c}', '\u{ff08}']),
                            "{context}: {what} line {line:?} ends with an opener"
                        );
                    }
                    assert_eq!(lines.concat(), text, "{context}: the {what} rejoined");
                }
                for pair in names.windows(2) {
                    let (line, next) = (&pair[0], &pair[1]);
                    let Some((_, piece)) = line_pieces(next).into_iter().next() else {
                        continue;
                    };
                    assert!(
                        measure(&format!("{line}{piece}")) > width + 0.01,
                        "{context}: {line:?} / {next:?} broke early"
                    );
                }
            }
        }
    });
}

#[test]
fn a_cjk_message_title_is_painted_at_every_narrow_width() {
    let title = "設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません";
    at_each_text_scale(&[1.0, 2.0], "CJK message", |scale| {
        for width in NARROW_WIDTHS {
            for height in NARROW_HEIGHTS {
                let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
                let (mut runtime, instance, view) = super::setup_with_messages();
                let mut messages = super::sample_messages();
                for entry in &mut messages.entries {
                    if entry.id == 40 {
                        entry.title = title.to_string();
                    }
                }
                assert!(runtime.replace_settings_messages(messages, 8));
                let cx = view_cx_at(width, height);
                let mut seen = 0;
                super::scroll_messages_through(
                    &mut runtime,
                    instance,
                    view,
                    &cx,
                    1,
                    |screen, k| {
                        let mut painted: Vec<(String, String)> = screen
                            .paint
                            .iter()
                            .filter(|node| {
                                !node.peek
                                    && node
                                        .key
                                        .as_str()
                                        .starts_with("settings/messages/row/40/label")
                            })
                            .filter_map(|node| match &node.content {
                                UiContent::Text(text) => {
                                    Some((node.key.as_str().to_string(), text.text.clone()))
                                }
                                _ => None,
                            })
                            .collect();
                        if painted.is_empty() {
                            return;
                        }
                        seen += 1;
                        painted.sort();
                        let shown: String = painted.iter().map(|(_, line)| line.as_str()).collect();
                        let stem = shown.trim_end_matches('\u{2026}');
                        assert!(
                            title.starts_with(stem) && !stem.is_empty(),
                            "{context} screen {k}: {shown:?} is not the title"
                        );
                        for (_, line) in painted.iter().skip(1) {
                            let first = line.chars().next().unwrap_or(' ');
                            assert!(
                                !NO_LINE_START.contains(&first),
                                "{context} screen {k}: a title line starts with {first:?}: {painted:?}"
                            );
                        }
                        let spilled = spilled_nodes(screen, &["settings/messages/row/40/"]);
                        assert!(spilled.is_empty(), "{context} screen {k}: {spilled:#?}");
                    },
                );
                assert!(seen > 0, "{context}: the title is painted on some screen");
            }
        }
    });
}

/// What a paint-text audit line says a node paints, its lines joined by `\n`.
fn audit_painted(line: &str) -> Option<String> {
    let (_, painted) = line.rsplit_once(" painted=")?;
    Some(
        painted
            .trim_matches('"')
            .replace("\\n", "\n")
            .replace("\\\"", "\""),
    )
}

/// Every screen of the compact category sheet, opened over `route` in a `width`×`height`
/// window from a fresh runtime.
fn sheet_screens(
    route: SettingsRoute,
    width: f32,
    height: f32,
) -> Vec<crate::native_ui::CompiledUi> {
    let (mut runtime, instance, view) = setup();
    go_to_route(&mut runtime, instance, view, route);
    let cx = view_cx_at(width, height);
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("settings/compact-navigation"),
                value: None,
            }),
        )
        .unwrap();
    let mut screens = Vec::new();
    each_screen(&mut runtime, instance, view, &cx, |compiled, _| {
        screens.push(compiled.clone());
    });
    screens
}

/// Every category's name is whole on the compact category sheet, and no two paint alike: a cell
/// gives up its arrow, then its pictogram, before its name, and the name takes the lines it
/// needs, a word broken only where it is wider than a line. At every compact width from the
/// 164pt window floor, at 1×, 1.5× and 2× text, every category is on some screen.
#[test]
fn every_category_name_is_whole_and_its_own() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "category names", |scale| {
        let mut faults = Vec::new();
        for width in [164.0_f32, 178.0, 192.0, 234.0, 276.0, 318.0, 420.0, 584.0] {
            let context = format!("{scale}\u{d7} {width}");
            let mut painted = BTreeMap::<String, String>::new();
            for compiled in sheet_screens(SettingsRoute::Home, width, 348.0) {
                let cut = cut_words(&compiled, &["settings/categories/"]);
                if !cut.is_empty() {
                    faults.push(format!("{context}: {cut:#?}"));
                }
                for line in compiled.paint_audit_lines() {
                    let Some(route) = line
                        .strip_prefix("paint-text key=\"settings/categories")
                        .and_then(|rest| rest.split_once('"'))
                        .and_then(|(path, _)| SettingsRoute::from_path(path))
                    else {
                        continue;
                    };
                    let shown = audit_painted(&line).unwrap_or_default();
                    let words = shown.replace("-\n", "").replace('\n', " ");
                    if words != route.label() {
                        faults.push(format!("{context}: {:?} paints {shown:?}", route.label()));
                    }
                    painted.insert(route.label().to_string(), shown);
                }
            }
            if painted.len() != SettingsRoute::rail_len() {
                faults.push(format!(
                    "{context}: {} of {} categories on some screen: {painted:?}",
                    painted.len(),
                    SettingsRoute::rail_len()
                ));
            }
            let distinct: BTreeSet<&String> = painted.values().collect();
            if distinct.len() != painted.len() {
                faults.push(format!("{context}: two names paint alike: {painted:?}"));
            }
        }
        assert!(
            faults.is_empty(),
            "{} faults:\n{}",
            faults.len(),
            faults.join("\n")
        );
    });
}

/// The compact toolbar holds every control whole: at every compact width from the 164pt floor
/// to 60 columns, at every text size, on a page and on the category sheet, the leading button,
/// the title and Search are each whole inside the bar (the toolbar less its insets), each
/// button a touch target, their words whole.
#[test]
fn the_compact_toolbar_holds_every_control_whole() {
    at_each_text_scale(
        &[0.85, 1.0, 1.2, 1.4, 1.7, 2.0],
        "the toolbar's controls",
        |scale| {
            let mut faults = Vec::new();
            for columns in (20..=60).step_by(2) {
                let width = 164.0 + (columns - 20) as f32 * 7.0;
                for sheet in [false, true] {
                    let context = format!("{scale}\u{d7} {width} sheet:{sheet}");
                    let (mut runtime, instance, view) = setup();
                    go_to_route(&mut runtime, instance, view, SettingsRoute::KeyboardInput);
                    if sheet {
                        runtime
                            .dispatch(
                                instance,
                                view,
                                AppEvent::Action(ActionInvocation {
                                    id: ActionId::new("settings/compact-navigation"),
                                    value: None,
                                }),
                            )
                            .unwrap();
                    }
                    let compiled =
                        compile_settings_view(&runtime, instance, view, &view_cx_at(width, 348.0));
                    let Some(toolbar) = compiled.semantic(&UiKey::new("settings/compact-toolbar"))
                    else {
                        continue;
                    };
                    let bar = (toolbar.rect.x + 10.0, toolbar.rect.right() - 10.0);
                    let mut controls = 0;
                    for node in &compiled.paint {
                        let key = node.key.as_str();
                        let control = key == "settings/compact-navigation"
                            || key == "settings/compact-navigation-close"
                            || key == SEARCH_OPEN
                            || key == "settings/search-bar"
                            || key.starts_with("settings/page-heading/");
                        if !control || node.rect.bottom() > toolbar.rect.bottom() + 0.5 {
                            continue;
                        }
                        controls += 1;
                        if node.rect.x < bar.0 - 0.5 || node.rect.right() > bar.1 + 0.5 {
                            faults.push(format!("{context}: {key} {:?} past {bar:?}", node.rect));
                        }
                        if node.clip != node.rect {
                            faults.push(format!("{context}: {key} cut to {:?}", node.clip));
                        }
                        if matches!(node.content, UiContent::Button(_)) && node.rect.width < 43.5 {
                            faults.push(format!("{context}: {key} {:?} narrow", node.rect));
                        }
                    }
                    if controls < 2 {
                        faults.push(format!("{context}: {controls} controls in the toolbar"));
                    }
                    let cut: Vec<String> = cut_words(
                        &compiled,
                        &[
                            "settings/compact-navigation",
                            "settings/page-heading/",
                            "settings/search",
                        ],
                    )
                    .into_iter()
                    .filter(|line| !line.ends_with("painted=\"Search\u{2026}\""))
                    .collect();
                    if !cut.is_empty() {
                        faults.push(format!("{context}: {cut:#?}"));
                    }
                }
            }
            assert!(
                faults.is_empty(),
                "{} faults:\n{}",
                faults.len(),
                faults.join("\n")
            );
        },
    );
}

/// Every category is whole and live on some screen of a short category sheet at 2× text: a
/// group's heading is let go where it and the row under it cannot both be whole in the room,
/// as a field page's caption is, so no row is cut on every screen.
#[test]
fn every_category_is_whole_on_some_screen_of_a_short_sheet() {
    at_each_text_scale(&[2.0], "a short sheet", |_| {
        let mut faults = Vec::new();
        for (width, height) in [
            (738.0, 180.0),
            (444.0, 180.0),
            (584.0, 210.0),
            (178.0, 210.0),
        ] {
            let screens = sheet_screens(SettingsRoute::Home, width, height);
            let listed: BTreeSet<String> = screens
                .iter()
                .flat_map(|compiled| compiled.semantics.iter())
                .filter_map(|node| {
                    let path = node.key.as_str().strip_prefix("settings/categories")?;
                    SettingsRoute::from_path(path).map(|_| node.key.as_str().to_string())
                })
                .collect();
            if listed.len() != SettingsRoute::rail_len() {
                faults.push(format!(
                    "{width}x{height}: {} categories listed",
                    listed.len()
                ));
            }
            for key in listed {
                let key = UiKey::new(key);
                let whole = screens.iter().any(|compiled| {
                    compiled.paint.iter().any(|node| {
                        node.key == key
                            && node.clip == node.rect
                            && node.fade.is_none()
                            && compiled.hits.iter().any(|hit| {
                                hit.key == key && (hit.rect.height - node.rect.height).abs() < 0.5
                            })
                    })
                });
                if !whole {
                    faults.push(format!(
                        "{width}x{height}: {} is whole on no screen",
                        key.as_str()
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
    });
}

/// Every page is named on screen, once: the compact toolbar titles the page beside the Search
/// button as well as beside a field, and where it cannot, the page's list leads with its name.
/// On every route and the category sheet, at short and narrow compact windows and a regular
/// one, the page's name is one Heading on its top screen, whole.
#[test]
fn every_page_is_named_once_on_screen() {
    let windows = [
        (1.0, 318.0, 208.0),
        (1.0, 178.0, 210.0),
        (1.0, 584.0, 210.0),
        (1.0, 849.0, 513.0),
        (2.0, 584.0, 348.0),
        (2.0, 444.0, 292.0),
        (2.0, 318.0, 348.0),
        (2.0, 234.0, 348.0),
        (2.0, 178.0, 348.0),
        (2.0, 234.0, 180.0),
        (2.0, 178.0, 180.0),
    ];
    at_each_text_scale(&[1.0, 2.0], "page names", |scale| {
        let mut faults = Vec::new();
        for (_, width, height) in windows.iter().filter(|(at, ..)| *at == scale) {
            let cx = view_cx_at(*width, *height);
            for (route, sheet) in SettingsRoute::ALL
                .into_iter()
                .map(|route| (route, false))
                .chain([(SettingsRoute::Home, true)])
            {
                let context = format!("{scale}\u{d7} {width}x{height} {route:?} sheet:{sheet}");
                let (mut runtime, instance, view) = setup();
                go_to_route(&mut runtime, instance, view, route);
                if sheet {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    state.compact_navigation = true;
                }
                // The page's top screen: a page that opens on a row further down (Cursor
                // Styles, on the saved style) is named where its list starts.
                let compiled = walk_start(&mut runtime, instance, view, &cx).0;
                // The category sheet is the compact window's; beside the rail it is not drawn.
                let compact = compiled
                    .semantic(&UiKey::new("settings/compact-toolbar"))
                    .is_some();
                if sheet && !compact {
                    continue;
                }
                let names: Vec<&crate::native_ui::SemanticNode> = compiled
                    .semantics
                    .iter()
                    .filter(|node| {
                        node.role == SemanticRole::Heading
                            && node.key.as_str().starts_with("settings/page-heading/")
                            && !node.offscreen
                            && node.rect.bottom() <= height + 0.5
                    })
                    .collect();
                let want: Vec<&str> = if sheet {
                    vec![ALL_SETTINGS]
                } else if route == SettingsRoute::Harness {
                    vec![route.label(), harness_page::HARNESS_PAGE_TITLE]
                } else {
                    vec![route.label()]
                };
                let [name] = names.as_slice() else {
                    faults.push(format!(
                        "{context}: {} names on screen: {:?}",
                        names.len(),
                        names.iter().map(|node| &node.label).collect::<Vec<_>>()
                    ));
                    continue;
                };
                if !want.contains(&name.label.as_str()) {
                    faults.push(format!("{context}: named {:?}", name.label));
                }
                let cut = cut_words(&compiled, &["settings/page-heading/"]);
                if !cut.is_empty() {
                    faults.push(format!("{context}: {cut:#?}"));
                }
            }
        }
        assert!(
            faults.is_empty(),
            "{} faults:\n{}",
            faults.len(),
            faults.join("\n")
        );
    });
}

/// A summary taller than the page is on some screen: Modified's count and its Reset All are
/// each whole on some screen of a short window at 2× text, the list taking the summary a part
/// at a time where it is taller than a screen. CONTROL: at 444×194 the summary is pinned whole
/// over the list.
#[test]
fn a_summary_taller_than_the_page_is_on_some_screen() {
    let source = "agents_auto_prime = false\ncursor_blink = false\ntheme = \"Nord\"\n\
                  cursor_trail_ms = 300\ncursor_trail_length = 30\nfuture_thing = 5\n";
    let seen = |width: f32, height: f32| {
        let (mut runtime, instance, view) = every_page_runtime_over(source);
        go_to_route(&mut runtime, instance, view, SettingsRoute::Modified);
        let (mut count, mut reset, mut cut) = (false, false, Vec::new());
        each_screen(
            &mut runtime,
            instance,
            view,
            &view_cx_at(width, height),
            |screen, _| {
                for node in &screen.paint {
                    let whole = node.clip == node.rect && node.fade.is_none();
                    match node.key.as_str() {
                        "settings/results-range" => count |= whole,
                        "settings/modified/reset-all" => {
                            reset |= whole
                                && screen.hits.iter().any(|hit| {
                                    hit.key == node.key
                                        && (hit.rect.height - node.rect.height).abs() < 0.5
                                });
                        }
                        _ => {}
                    }
                }
                cut.extend(cut_words(screen, &["settings/results-range"]));
            },
        );
        (count, reset, cut)
    };
    at_each_text_scale(&[2.0], "a tall summary", |_| {
        assert_eq!(seen(444.0, 194.0), (true, true, Vec::new()), "CONTROL");
        for (width, height) in [(444.0, 180.0), (584.0, 180.0), (178.0, 180.0)] {
            let (count, reset, cut) = seen(width, height);
            assert!(count, "{width}x{height}: the count is whole on some screen");
            assert!(
                reset,
                "{width}x{height}: Reset All is whole and live on some screen"
            );
            assert!(cut.is_empty(), "{width}x{height}: {cut:#?}");
        }
    });
}

/// The Settings search field keeps a typed query readable: it shows the query's caret end,
/// never an ellipsis over typed words, with the keyboard in it or not, and the toolbar gives it
/// the room the query needs before the leading button's name. At every compact width, at 1×,
/// 1.5× and 2× text, "board" is whole, and a query longer than the field shows its end.
#[test]
fn the_search_field_keeps_a_typed_query_readable() {
    let long = "cursor trail length and colour";
    at_each_text_scale(&[1.0, 1.5, 2.0], "the search field", |scale| {
        let mut faults = Vec::new();
        for columns in (20..=80).step_by(6) {
            let width = 164.0 + (columns - 20) as f32 * 7.0;
            for query in ["board", long] {
                for focused in [true, false] {
                    let context = format!("{scale}\u{d7} {width} {query:?} focused:{focused}");
                    let cx = view_cx_at(width, 348.0);
                    let (mut runtime, instance, view, _) = compile_search(query, &cx);
                    {
                        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view)
                        else {
                            unreachable!();
                        };
                        state.common.last_focus = focused.then(|| UiKey::new("settings/search"));
                    }
                    let compiled = compile_settings_view(&runtime, instance, view, &cx);
                    let Some(shown) = compiled
                        .paint_audit_lines()
                        .into_iter()
                        .find(|line| line.starts_with("paint-text key=\"settings/search\" "))
                        .and_then(|line| audit_painted(&line))
                    else {
                        faults.push(format!("{context}: no field"));
                        continue;
                    };
                    if shown.contains('\u{2026}') || (query == "board" && shown != query) {
                        faults.push(format!("{context}: paints {shown:?}"));
                    } else if !query.ends_with(shown.trim_start()) || shown.trim().is_empty() {
                        faults.push(format!("{context}: paints {shown:?}, not its end"));
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
    });
}
