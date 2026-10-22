// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings at phone width and large text — the system pages first,
//! then every page by the same rules: every word a page paints is
//! whole in the box it is drawn in, and every control stays inside its card
//! and is whole on some screen, from a 22-column window (178pt) up, at 1×,
//! 1.5× and 2× text.

use super::*;

/// The window widths a 22-, 24-, 30-, 36- and 42-column terminal window gives
/// Settings, and the heights of 24 and 36 rows.
pub(super) const NARROW_WIDTHS: [f32; 5] = [178.0, 192.0, 234.0, 276.0, 318.0];
pub(super) const NARROW_HEIGHTS: [f32; 2] = [348.0, 516.0];

/// The windows an 80×24, 102×29, 150×48, 167×36 and 220×60 terminal window
/// gives Settings.
pub(super) const WIDE_SIZES: [(f32, f32); 5] = [
    (584.0, 348.0),
    (744.0, 420.0),
    (1_095.0, 700.0),
    (1_219.0, 522.0),
    (1_606.0, 876.0),
];

/// The first screen of `route` in a `width`×`height` window, from a fresh
/// runtime.
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
    let cx = view_cx_at(width, height);
    let mut screens = Vec::new();
    each_screen(&mut runtime, instance, view, &cx, |compiled, _| {
        screens.push(compiled.clone());
    });
    screens
}

/// The painted words under any of `prefixes` that are not whole: elided or
/// clipped by the painter (the fit audit's `overflow=true`), or cut by their
/// author (an ellipsis in what it hands the painter).
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

/// The nodes under any of `prefixes` drawn past the box that holds them:
/// wider than what their cards and the list let show.
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

/// A CARD OF FACTS PAINTS EVERY NAME AND VALUE WHOLE AT PHONE WIDTH AND
/// LARGE TEXT (the narrow sweep): About's build information and font
/// credits at 22 to 42 columns, 1× and 2×, every screen — a name in its
/// column, a value in its own, or each name over its value where the card
/// cannot hold both side by side ([`FactColumns`]), nothing elided and
/// nothing past the card. Before, the name column was a fixed 104pt (124.8pt
/// at 2×, wider than a 22-column card's 110pt text) and the value column what
/// it left: "Architec…", "Running…", and at 2× every value a lone "…".
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

/// THE ONE RULE FOR A FACT'S WORDS: names over values only where the card
/// cannot hold them side by side, measured. A wide card keeps each name
/// beside its value, its name column as wide as the widest word of a name
/// (up to two fifths of the card); a 2× phone-width card stacks; a value's
/// words wrap at its pieces, a code value's characters with no hyphen to
/// read as one of them, and a word wider than its box breaks with one.
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

/// A STACKED SWITCH IS NEVER WIDER THAN ITS LINE (the narrow sweep):
/// on the system pages that draw switch rows — Keyboard & Input, Security,
/// Window, Software Update and Packages — at 22 and 24 columns, 1× and 2×,
/// every switch, and the Reset its row keeps, is inside its card. A stacked
/// row's switch was a fixed 104pt (145.6pt at 2×) whatever its line, which in
/// a 116pt column ran past the card and cut its own On/Off word; Packages'
/// switch rows kept their Reset beside the switch even where the line could
/// not hold both.
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

/// The setting names on `compiled` broken onto a next line whose first word
/// would have fit the line before it, in that line's own box.
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
            // A line a word broke inside, or one ending in a tag, is the
            // word's or the tag's.
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

/// A WRAPPED NAME FILLS THE BOX IT IS DRAWN IN: each line of a setting
/// row's name breaks only where its next word would not fit the line's own
/// box ([`early_breaks`]), on Harness, AI CLIs and Software Update at 22 to
/// 42 columns, 1× and 2×. Harness's and AI CLIs' cards laid their rows out
/// at the page's padding and drew them, on a short window, inside a tighter
/// one 20pt wider ("Supervise Muse" / "Code sessions" beside room for
/// "Code"); Software Update measured its switches 6pt inside their card.
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

/// A NARROW SLIDER SETS ITS READOUT OVER ITS TRACK (the narrow
/// sweep): a slider too narrow to keep its readout's room beside a track as
/// wide, and tall enough for both, paints the readout flush right on a line
/// of its own and the track under it across the whole control — at 2× text
/// Window's "5000 ms" (95.5pt) was painted into a 57pt slot beside a 67pt
/// track. Decided by the control's size, never the value: the track is the
/// same length at both ends of the range. A wide slider keeps them side by
/// side.
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

/// A SLIDER'S READOUT IS MEASURED IN THE FACE IT PAINTS IN, AND ENDS AT ITS
/// EDGE (the audit): the cursor trail's "24 cells" and "0.6 cells"
/// in the 126pt control a 22-column window gives a stacked slider at 2×
/// text, and every readout beside a wide track, stand flush right — the
/// painted text's right edge on the readout's end — and the fit audit
/// passes them. Measured in the mono face with a floor of 0.62 em a
/// character while painted in the UI face, "24 cells" was reported 109pt
/// in a 104pt slot (overflow=true) while its glyphs took 73pt, and it stood
/// flush left, 32pt short of the edge "260 ms" ends on.
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

/// A RESET'S PLACE NEVER COSTS A NAME MORE THAN A LINE OF ITS OWN (the
/// audit): on every page at 30, 38 and 42 columns, 1× and 2× text,
/// a stacked row's name — beside the Reset (or the place it keeps) at its
/// label line's end — takes at most one line more than the label line
/// alone sets it on: where it would take more, Reset has a line of its own,
/// or a switch row's Reset follows the switch, narrowed toward its least.
/// At 38 columns and 2× text Reset's empty 108pt place left "Supervise
/// Gemini CLI sessions" 124pt, a word to a line — four lines where the line
/// alone holds it on two — and "Allow programs to read the window title and
/// size" seven for three.
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
                        // Rows whose label line paints the name alone.
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

/// A STACKED CHOICE KEEPS ITS WORDS (the narrow sweep): a stacked
/// row's choice button paints its whole choice where it fits, else the
/// choice without its "(default)", else either with the arrow given up, else
/// — a colour row — the colour alone, its chip; never cut. At 22 columns the
/// Window page's "Automatic (default)" read "Auto…" beside room for
/// "Automatic", and AI CLIs' "Claude (default)" read "Claude (…" at 2×.
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

/// A BUTTON KEEPS ITS WORDS IN A NARROW COLUMN (the narrow sweep):
/// Harness's four actions and About's two at 22 to 42 columns, 1× and 2×,
/// every screen — whole on one line where the box holds them, else a short
/// form, else on as many lines as the box needs ([`button_words`]); never
/// cut. Harness fitted its labels 20pt short of the room the button paints
/// them in ("Tab Co…" in a box that held "Tab Control"), and About's hero
/// had no form for a column narrower than "Copy Build" ("Cop…", "Ope…").
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

/// A TEXT BUTTON PAINTS ITS LABEL'S LINES, each whole, one under another in
/// a button as tall as they need ([`native_ui::native_button_label_lines`],
/// [`native_ui::native_button_height_for_lines`]); a word wider than the room
/// has no whole-word lines.
#[test]
fn a_text_button_paints_its_labels_lines() {
    at_each_text_scale(&[2.0], "button lines", |_| {
        let _ = view_cx();
        let room = crate::native_ui::native_button_label_width("Open Project") + 1.0;
        let lines = crate::native_ui::native_button_label_lines("Open Project Site", room)
            .expect("each word fits");
        assert_eq!(lines, ["Open Project", "Site"]);
        assert!(crate::native_ui::native_button_label_lines("Information", 20.0).is_none());
        // A label with no space has its words at its CJK characters, kinsoku
        // kept, joined by nothing (before, one word wider than the room: no
        // lines at all).
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

/// ABOUT'S WORDMARK IS WHOLE AT EVERY SIZE AND TEXT SCALE (item A2 of the
/// narrow sweep): the hero's "aterm", in the Display step where its
/// column holds it and scaled down to the column where it does not
/// ([`native_ui::wordmark_typography`]) — from the 164pt window floor to a
/// wide window, short and tall, at 1×, 1.5× and 2×. An 80 × 24 window
/// (584 × 348) at 2× painted "ate…", a phone-width one "at…".
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

/// A WARNING STAYS A WARNING AS IT GIVES UP ITS SIZE (item A4 of the
/// narrow sweep): Software Update's failing verdict paints in the danger
/// colour at its own step, at the Body step in bold where that does not fit
/// ([`StyleRef::DangerCompact`]), and on as many lines as its column needs
/// where not even that does — from a phone-width page to a desktop one, at
/// 1×, 1.5× and 2×. The painter gave every heading but a quiet or accent one
/// the body's ink, so the verdict was never red; and where it stepped down
/// it took the plain style, keeping its words but not its warning.
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

/// SOFTWARE UPDATE KEEPS ITS WORDS (the narrow sweep): up to date,
/// with an update ready and failing, at 22 to 42 columns and on a wide
/// window, short and tall, 1× and 2× — the status card's caption, verdict,
/// version and detail, the update action and the Messages link each whole in
/// the column it is drawn in, nothing past its card. The caption, the verdict
/// and the version were one line cut to the card ("SOFTWARE UPD…", "aterm
/// 0.…"), the action was cut inside the card ("Upd…") and the Messages
/// link's arrow cut its word ("Messa…").
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

/// THE COMPACT TOOLBAR KEEPS ITS WORDS (item A3 of the narrow sweep):
/// at every compact width from the 164pt window floor and every text size,
/// the back button, the title and Search each paint whole words — Search's
/// field says "Search…" or "Search"; where the bar cannot hold even that
/// beside the back button, Search is a button (a magnifying glass named
/// Search Settings) that opens the field across the bar, the keyboard in it,
/// whole, with Cancel after it, and Cancel brings the button back. A
/// 24-column window at 2× painted "Se…".
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
            // "Search…" is the field's whole placeholder, not a cut.
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

/// A STACKED CLI ROW'S DETAIL IS WHOLE: on a compact page each CLI's state
/// and fact wrap to the column — a path at its folders — never cut ("signed-
/// ou…" at 2× text on a phone-width page).
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

/// Text that ends in an ellipsis because it says something is under way
/// ("Reading package status…"), not because it was cut: its painted line is
/// the end of one of these.
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

/// A PAGE'S NAME IS WHOLE IN ITS COLUMN (the narrow sweep): every
/// route's heading, at the window floor to 30 columns, 1× and 2× — in the
/// title type where it fits, else Body-bold, else Body-bold on lines. A
/// 22-column page painted "Keyboard…" and "Software U…" at 1×.
#[test]
fn a_page_name_is_whole_in_its_column() {
    at_each_text_scale(&[1.0, 2.0], "page names", |scale| {
        for width in [164.0, 178.0, 192.0, 234.0] {
            for route in SettingsRoute::ALL {
                let screens = screens_of(|_, _| {}, route, width, 900.0);
                let cut = cut_words(&screens[0], &["settings/page-heading/"]);
                assert!(
                    cut.is_empty(),
                    "{scale}\u{d7} {width} {}: {cut:#?}",
                    route.label()
                );
            }
        }
    });
}

/// THE PACKAGES PAGE KEEPS ITS WORDS (the narrow sweep): its
/// status, verbs, captions, service line, programs and activity at 22 to 42
/// columns and on a wide window, 1× and 2× — wrapped to the card's MEASURED
/// column, never cut, nothing past a card — before its tool reports and with
/// its tool live. Its lines were budgeted in average glyphs per width class
/// (56 a compact line, for 6 words at 2×), its three verbs held at a fixed
/// width whatever their words, its captions and its service line's columns
/// fixed; and the hero's "Signed updates from …" was one fixed line, cut to
/// "Signed updates fr…" at 22 columns once the tool was live.
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

/// MESSAGES' SMALL BUTTONS STAY ON THE PAGE (the narrow sweep): Copy
/// All and Open Log Folder, at a phone width and 2× text, keep their words
/// inside the column, on lines where it is narrower than one ("Open Log
/// Folder" ran 87pt past it).
#[test]
fn messages_small_buttons_stay_on_the_page() {
    at_each_text_scale(&[1.0, 2.0], "Messages buttons", |scale| {
        for width in NARROW_WIDTHS {
            let screens = screens_of(|_, _| {}, SettingsRoute::Messages, width, 516.0);
            let prefixes = [
                "settings/messages/copy-all",
                "settings/messages/open-folder",
            ];
            let cut = cut_words(&screens[0], &prefixes);
            assert!(cut.is_empty(), "{scale}\u{d7} {width}: {cut:#?}");
            let spilled = spilled_nodes(&screens[0], &prefixes);
            assert!(spilled.is_empty(), "{scale}\u{d7} {width}: {spilled:#?}");
        }
    });
}

/// HARNESS KEEPS ITS WORDS WITH SESSIONS OPEN (the narrow sweep):
/// the page with its sessions — and again with three under Land and a
/// reading of landings — at 22 to 42 columns, short and tall, 1×
/// and 2× — every row's title, detail, setting, state and day whole on as many
/// lines as its column needs, every face of its buttons whole, nothing past
/// its card; a row taller than one screen of its card listed a part at a time
/// ([`super::scroll::row_parts`]). A short window at 2× crammed the
/// buttons five to a line to keep the row on one screen ("Cru…", "…") and cut
/// the words to the column ("Tab 2 · Cla…").
#[test]
fn harness_keeps_its_words_with_sessions_open() {
    at_each_text_scale(&[1.0, 2.0], "Harness sessions", |scale| {
        let mut parts = 0;
        for width in NARROW_WIDTHS {
            for height in NARROW_HEIGHTS {
                // The page as published, and with landings counted (the Every row's `landed` and `quiet`, `Landed`, the
                // done claims checked, today's lines led by landings).
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

/// A ONE-LINE SESSIONS ROW KEEPS ITS WORDS (the sweep): on the
/// windows an 80×24 to a 220×60 terminal gives, at 1× to 2× text, a session
/// row whose words sit beside its five buttons wraps its title, detail and
/// setting line to the words' column, whole, its buttons a control tall. A
/// 167×36 window at 1.5× cut the detail at a joint, "Tab 2 · Claude Code ·…".
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

/// A PAGE'S SUBTITLE IS WHOLE IN ITS COLUMN (the sweep): every route
/// on the windows an 80×24 to a 220×60 terminal gives, 1× to 2× text, on as
/// many lines as the form's measure needs. Harness's at 2× on a 220×60
/// window painted "…what it did, and its settin…", 4pt over its 720pt
/// column.
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

/// A ROW TOO NARROW FOR ITS NAME BESIDE ITS CONTROLS STACKS THEM UNDER IT,
/// ON A WIDE PAGE TOO (the sweep; the rule a compact row keeps): at
/// 1.5× and 2× text on the windows a 150×48 and a 220×60 terminal give, no
/// setting row draws its name, control, segments or Reset past its row, and
/// Harness's Question priority — three segments and Reset's place wider than
/// the whole row at 2× — says its name whole over them. The name was given
/// no room at all (and not painted), and Reset's place ran 52pt past the row.
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

/// SEARCH'S RESULTS AND MODIFIED KEEP THEIR WORDS (the narrow sweep,
/// what the narrow toolbar's Search opens): a search that finds pages and
/// settings ("cursor"), one that finds nothing, and Modified with nothing
/// changed, at 22 to 42 columns, short and tall, 1× to 2× — the count, every
/// page result, the empty card and Reset All whole in the column, nothing past
/// it. A 24-column window at 2× painted "2 pages · 19 s…" and "✦ C… →", and
/// Modified "Nothin…", "Settings you c…" and a Reset All 12pt past the page.
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
                            // "Reset All…" asks before it acts: its words.
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

/// AN EMPTY FIELD DROPS NO WORD WHERE ITS ROW CAN GROW (the
/// audit): every empty text field on Appearance, Terminal and Cursor Styles
/// (the comet's trail colours too) at 22 and 30 columns, 1×, 1.5× and 2×
/// text, paints every word of its placeholder — less only the "(default)"
/// the quiet tint says and the aside it ends in — on one line or on lines of
/// the field, never "…" for words it leaves out — a colour field whose
/// words are not whole beside its chip sets the chip on a line of its own
/// under them, and still shows its colour. "Theme default" read "Theme…"
/// (and at 2× in 22 columns the field showed its chip alone), "platform
/// default" "platform…", "Pale blue (default)" "Pale…", and "Brighter pale
/// blue (default)" "Brighter" / "pale…", the row free to grow.
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
                        // A colour field still shows its colour: beside its
                        // words, or on a line of its own under them.
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

/// A FIELD'S VALUE IS WHOLE WHERE ITS ROW CAN GROW (the review):
/// a value wider than its field — a shell named by a hyphenated word, a
/// shell's path — paints every letter while the field is not being edited,
/// on the lines it takes (a path the longest of its forms two lines hold,
/// its program's name whole), the field as tall as they are; editing it
/// leaves the row as tall. A value one line holds stays on one line. It
/// was cut on one line: "nushell-nightly-build" read "nushel…" at 2× text
/// in a 22-column window, the path's program "…e-zsh".
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
                    // Its program's name whole: the path whole, or its
                    // folders given up for "…" only where two lines do not
                    // hold it, the name's folder kept where they hold it
                    // ([`path_placeholder_lines`]).
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
                // Editing it keeps the row's height.
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

/// A FIELD'S CHIP ON A LINE OF ITS OWN TAKES NONE OF ITS WORDS' WIDTH
/// ([`crate::native_ui::TextFieldSpec::swatch_line`]): the words have the
/// field's whole text box, over the chip's line, and the chip is drawn at
/// the field's end on that line, under them. NEGATIVE CONTROL: beside the
/// words the chip takes its 30pt off their room.
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
        // NEGATIVE CONTROL: beside the words, the chip takes its room.
        let beside = UiTree::new(UiNode::new("foreground", field(false)))
            .compile(rect)
            .expect("compiles");
        let (_, room) = available(&beside);
        assert!((room - (118.0 - 20.0 - 30.0)).abs() < 0.5, "{room}");
    });
}

/// A FIELD'S HINT STANDS AT THE END OF ITS TEXT BOX (the review):
/// the "Unsaved" a colour field with a refused draft says at its end ends
/// where the box its words have ends — at the field's end where the chip
/// stands on a line of its own, a chip's width before it where the chip
/// stands beside the words — so it never stands over them. The painter
/// took a chip's width off the hint's line for any field with a chip, and
/// the hint stood 30pt into the words of a field whose chip had a line of
/// its own.
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

/// A NARROW FIELD GIVES UP ITS HINT, NEVER ITS WORDS (the review):
/// at 2× text in a 22- or 24-column window a refused draft's field paints
/// its value whole and no "Unsaved" beside it — the page's draft bar says
/// the draft waits, and the field's description says it — where beside the
/// hint its words would keep less than the hint takes; a field with room
/// for both paints both. The hint left "abc" 8pt of its box, the value
/// painted nowhere (audit overflow=true).
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

/// THE FLEET FIELD'S WORDS KEEP IT A FIELD (the Linux lane): where
/// Settings does not know aterm.toml's folder, the field says "ai-fleet.toml
/// beside aterm.toml", which goes on five lines at 2× text in a 22-column
/// window; once that was a field 229pt tall in a 222pt list, whole on no
/// screen. Every word stands on the field's lines wherever a screen of the
/// list holds them ([`field_lines_most`]) — capped at two or three lines,
/// it read "ai-" / "fleet.toml" / "beside…" in a list with room for all —
/// and the field is whole on some screen, at every narrow width, 24 and 36
/// rows tall.
#[test]
fn the_fleet_fields_words_keep_it_a_field() {
    at_each_text_scale(&[1.0, 2.0], "fleet words", |scale| {
        for width in NARROW_WIDTHS {
            for height in NARROW_HEIGHTS {
                let context = format!("{scale}\u{d7} {width}x{height}");
                let (mut runtime, instance, view) = every_page_runtime();
                {
                    // Where aterm.toml is is not known: the field says it in
                    // words.
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
                        let read = painted_words(&painted).replace(['\n', ' '], "");
                        assert!(
                            !painted.contains('\u{2026}')
                                && read == "ai-fleet.tomlbesideaterm.toml",
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

/// THE MUSIC STATUS LINE SAYS WHY, WHOLE, IN THE ROOM IT HAS: whatever
/// silences the voices, at every width from the narrow host's 126-pt column
/// to a wide page and at 1x, 1.5x and 2x text, the painted status keeps its
/// words whole and names its cause in the setting's own words (Reduce
/// motion, System Reduce Motion, Heavy load, the cursor trail, Sound volume,
/// the audio output); where the full sentence fits on two lines, the full
/// sentence is what paints. One fixed "Some off" named neither the cause nor
/// the voices, under a switch that read On.
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

/// PACKAGES' SERVICE LINE IS LAID OUT AT THE GAP IT WAS MEASURED WITH (the
/// lab sweep): at every card text width from a phone's to a wide
/// page's, 1× and 2× text, "Unavailable in this copy of aterm" beside or
/// under "Background service" paints every line whole in its column. The row
/// set its columns 12pt apart where [`FactColumns`] had measured them 10pt
/// apart, so at 36 columns the value's first line was 2pt short of its words
/// ("Unavailabl…").
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

/// MESSAGES KEEPS ITS WORDS WITH A LOG (the lab sweep, which a log
/// in it made the page's real shape): the page with entries, collapsed and
/// with one open, at 22 to 42 columns, short and tall, 1× to 2× — scrolled
/// through every stop — every word whole and nothing past the page; what a
/// screen's edge cuts (a tall open entry read a part at a time) whole on
/// another screen; every control reachable whole. An entry's title is its
/// row's one line (ruling 262: the row's name says it whole, its body says
/// why), so it is the one text here an ellipsis may end. A 22-column window
/// gave the title 24pt ("C…") under a time column wider than the row at 2×,
/// cut the filter chips ("Proble…", "Tag:…") and the count ("40 messa…"),
/// seated a filters card taller than the list, so Open Log Folder was on no
/// screen, and cut an open entry's buttons by the list's edge on every one.
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
                        // Opened, then read from the list's top.
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
                                    // A head's title (ruling 262), said whole
                                    // where its entry is open (below).
                                    let head_title = line
                                        .starts_with("paint-text key=\"settings/messages/row/")
                                        && (line.contains("/label\"")
                                            || line.contains("/label/line-"));
                                    head_cut |= head_title && line.contains("/row/40/");
                                    !head_title
                                })
                                // The technical details' honest count of the
                                // lines the room leaves out (ruling 368; Copy
                                // keeps them).
                                .filter(|line| !line.contains(" more lines)\""))
                                .filter(|line| {
                                    // Cut by the room's edge: whole on another screen.
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
                    // An open entry whose head cut its title says it whole.
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
                    // An open entry's buttons too, on a tall entry's part.
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

/// A ONE-LINE HEAD'S TITLE HAS TWO FIFTHS OF ITS ROW (ruling 262's own
/// rule): at every row width from a phone's to a wide page's, 1× to 2×,
/// either the title's column — the row less the lead and the chevron's end —
/// is at least two fifths of the row, or the head stacks. The rule counted
/// the chevron's end as the title's, so at 30 columns a title had a quarter
/// of the row ("Claud…").
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

/// THE MANUAL PAGE'S BUTTONS KEEP THEIR WORDS: Open aterm.toml, Show in
/// Finder and Copy path stand three to a row only where each label is whole
/// in its third, measured; else one under another, each the card's width,
/// its words on lines where even that is too narrow — from a phone-width
/// window to a desktop one, at 1× and 2×. A guess from the text size (capped
/// at 1.5×) set them in a row on a 150 × 48 window at 2× and painted "Open
/// aterm.t…".
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

/// A PAGE SCROLLED DOWN KEEPS ITS CHROME ONLY WHILE ITS WINDOW AND ITS TEXT
/// SIZE STAY: a text size changed under a scrolled list chooses the page's
/// chrome again, as the page at that size chooses it, so a 1× subtitle and
/// insets are never kept for a 2× page whose short window gives them to its
/// list — every route, phone width to desktop. The kept chrome was keyed by
/// the window alone: Manual at 22 × 24 kept its 1× lean chrome at 2×.
/// CONTROL: some page here chooses a different chrome at 2× than at 1×, so
/// the case is real.
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
                    // At 2× from its first render, scrolled.
                    set_scale(2.0);
                    let (mut fresh, instance, view) = setup();
                    go_to_route(&mut fresh, instance, view, route);
                    scroll_to(&mut fresh, view, 2);
                    let _ = compile_settings_view(&fresh, instance, view, &cx);
                    let at_two = chrome_of(&fresh, view);
                    // At 1×, scrolled; then the text made 2×.
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

/// A PAGE'S FIRST FRAME IS ITS SECOND: every page opened, phone width to
/// desktop, at 1× and 2× text, drawn again with nothing between, draws the
/// same screen. The room a compact page lays its rows out for (whether a
/// row stacks, how many lines a field's words take) was measured under the
/// chrome the last render had left, the full chrome on a page's first
/// render, not under the chrome the page then chose: AI CLIs at 22 columns
/// at 2× text drew its fleet field a line taller on its second frame, and
/// its list a stop longer.
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

/// Where two compiles of one screen first differ, both sides whole: the
/// first painted node apart (or the one only one of them paints), else the
/// first hit region, semantic node, focus stop, segment group or list
/// picture apart, else the default action or the bounds.
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

/// THE RETIRED `auto_update` NOTE KEEPS ITS WORDS IN A NARROW COLUMN: on the
/// Packages page under Keep ALab tools up to date, at 22 to 42 columns and
/// on desktop widths, 1× and 2×, every line of atpkg's sentence is whole in
/// the card's column and every word of it paints, its remedy included. Its
/// lines were budgeted in characters per width class (56 on any compact
/// page), so a phone-width card at 2× cut each to an ellipsis.
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
            // Each break joined as the note writes its halves: `auto_update`
            // breaks before its `_` with no mark, an identifier's own joint.
            assert_eq!(
                crate::native_ui::joined_as_named(&said, &note),
                note,
                "{context}: every word paints"
            );
        }
    });
}

/// The Packages state of a copy of aterm with no package tool, its status
/// read (what a development build in a lab shows): Packages says it cannot
/// manage ALab tools, and Security's This Mac status why Universal Control
/// cannot change.
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
        // The same screen, every part of it: cheaper than comparing the
        // audits it prints, which it decides.
        if screens.last() == Some(&screen) {
            return screens;
        }
        screens.push(screen);
    }
    panic!("{route:?} {width}\u{d7}{height}: Page Down never reaches the list's end");
}

/// EVERY CONTROL AND EVERY WORD OF A SYSTEM PAGE IS ON SOME SCREEN WHOLE:
/// paging through Software Update, About, Messages, Packages, Security,
/// Harness, AI CLIs, Keyboard & Input and Window from top to end with Page
/// Down, at 22 to 42 columns short and tall and at 80 × 24 and 102 × 29, 1×
/// and 2×, with and without a package tool (and Harness as it opens and with
/// its sessions and landings counted), each control is drawn whole on
/// at least one screen, and each
/// block of wrapped words that starts with its own words paints all of them.
/// The list scrolls by whole rows, and a row taller than what a screen leaves
/// it under its card's caption was drawn from its top on every screen and its
/// rest on none: Packages' status detail lost "Install aterm from
/// alab.systems…", its consent card's rows their switches, Software Update's
/// automatic-update switch, Security's switches whose names took six lines
/// and the end of its This Mac status were on no screen
/// ([`super::scroll::row_parts`]).
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
                                    // Harness with its sessions and landings counted:
                                    // the Every row's parts, Landed, the done
                                    // claims checked, today's line led by landings.
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
                            // Drawn uncut and at full strength (a row
                            // scrolled under a kept caption is a peek of its
                            // card, its controls live and whole).
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
                        // A block cut honestly (an ellipsis), or painting a
                        // shorter form of its name, says so on its own.
                        if said.iter().any(|line| line.ends_with('\u{2026}')) {
                            continue;
                        }
                        // Each break joined as the label writes its
                        // halves: an identifier breaks at its own joints
                        // with no mark (`alab` / `.systems`).
                        let (want, have) = (
                            words(label),
                            words(&crate::native_ui::joined_as_named(&said.join("\n"), label)),
                        );
                        if have.first() != want.first() {
                            continue;
                        }
                        // A word's own hyphen may end a line, as copy
                        // breaks there (`painted_words` joins a line ending
                        // in a hyphen with the next, the hyphen dropped:
                        // `follow-` and `up` read `followup`).
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

/// A Settings runtime holding what every page draws at its fullest: a key
/// only aterm.toml holds (Modified's and Manual's own group), aterm.toml at a
/// known path (the fleet document's default beside it), Harness with a
/// session and Tab Control with this Mac, a LAN Mac and two tabs, their names
/// as long as a real fleet's, Packages with its tool (its three verbs, its
/// programs and its activity), and Security with the macOS host's posture
/// the most of its buttons stand in (every folder denied, other copies to
/// move to the Trash: the MACOS ACCESS block and the DEVELOPER TOOLS card
/// over it, which a macOS host always draws and a headless one never does).
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

/// What a painted node says in words: a text's, a button's painted label, an
/// empty field's placeholder.
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

/// `painted` cut inside a word of `whole` (what the same node says where
/// nothing cuts it): an ellipsis after part of a word, not after a whole one
/// (a one-line head or a placeholder may end after whole words, "Theme…").
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

/// The places on `screen` where words wrapped onto lines break inside a
/// word with no mark: a block's line (`<block>/line-<n>`, under the group
/// that says the block whole, else what its lines say where nothing cuts
/// them, `whole`) or a button's label line that ends where its next line
/// goes on in the same word — "Messages" then "." alone, where a word wider
/// than its box is broken with a hyphen ([`wrap_to_box`]). A line ending in
/// a hyphen, a path broken at a folder's `/` (before it, or after it where
/// its words are prose), a break inside an identifier (a path, a host, a
/// version, a hash: [`aterm_grapheme::TokenKind::Identifier`], which never
/// takes a mark, a hyphen would read as one of its characters) and East
/// Asian text, which breaks between its characters, are not breaks inside a
/// word.
fn broken_inside_a_word(
    screen: &crate::native_ui::CompiledUi,
    whole: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let mut wholes = std::collections::HashMap::<&str, &str>::new();
    let mut blocks =
        std::collections::BTreeMap::<String, std::collections::BTreeMap<usize, String>>::new();
    let mut broken = Vec::new();
    // Whether the line `before` ends where `after` goes on inside one of
    // `whole`'s words.
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
        // The block's words: what its group says, else its lines where
        // nothing cuts them (a setting's note says itself only in them).
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

/// What each node of `route` says where nothing cuts it: every screen of a
/// 4000×2400 window, from a runtime `prepare` sets up.
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

/// The nodes under `prefixes` on `screen` that cut a word inside itself
/// ([`cut_inside_a_word`]) of what they say whole (`whole`), or overflow.
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

/// EVERY PAGE KEEPS ITS WORDS AND ITS CONTROLS (the rules
/// on every page — a label wraps to its box, a word wider than its box
/// breaks with a hyphen, a button's words wrap, a tall row is listed a part
/// at a time): every route, every screen, at 22 to 42 columns short and tall
/// and the 80×24, 102×29 and 150×48 windows, 1×, 1.5× and 2× text, with a key
/// only aterm.toml holds, aterm.toml at a known path and a fleet's sessions
/// and computers, in the default theme and in GitHub Light with a light
/// window ([`EVERY_PAGE_SOURCES`]), where Modified, Appearance and Home draw
/// their theme rows set: at 22 to 30 columns there Modified's Color theme
/// button and its Reset, its window theme's third segment and Appearance's
/// theme Reset were whole on no screen, a row taller than any screen cut
/// whole. On every screen nothing overflows, nothing is clipped and
/// no two marks paint over each other ([`crate::native_ui::CompiledUi::fit_failures`]),
/// and no word is cut inside itself; and every control is whole on some
/// screen. Before, Cursor Styles cut every description ("Flames behind what
/// you type and a red-hot…") and its captions, Home its cursor link and its
/// cards' titles, Tab Control every computer and tab, Wallpaper its status
/// and ran Choose Image… past its card, Text & Fonts its display-face
/// switches, Modified's key only in aterm.toml drew its name in 4pt and its
/// buttons past the card, and Themes' folder button ran out of the panel.
/// Themes' own chooser list scrolls inside the page, and its rows are judged
/// by its own sweep (`every_part_of_the_themes_page_is_whole_on_some_screen`).
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
            // What each node says where nothing cuts it.
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
                            // A control only ever peeking past a screen's
                            // edge is one no screen shows whole: it counts.
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
                // PREMISE: the fixture draws what a page draws at its fullest
                // ([`every_page_runtime`]) — Packages' verbs, Security's
                // MACOS ACCESS block and DEVELOPER TOOLS card.
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

/// THE MACOS ACCESS BLOCK AND THE DEVELOPER TOOLS CARD KEEP THEIR WORDS AT
/// PHONE WIDTH, with the posture a macOS host always publishes (and a
/// headless lab never does, so no sweep of one saw them): at 22 and 24
/// columns, short and tall, 1.5× and 2× text, no screen of Security paints a
/// word of either card cut or past its box, and every button of theirs is
/// whole on some screen. The block's heading was one fixed line ("MACOS
/// A…"), its buttons one fixed-height row whatever their words ("Open…",
/// "Turn O…", "Ask fo…"), the card's floor a 306pt button in a 118pt card
/// ([`button_line_set`], [`access_card_heading`], [`devtools_floor`]), and
/// its facts broke a word wider than the card with no mark ("Download" /
/// "s", [`copy_paragraph_lines`]).
#[test]
fn the_macos_access_block_keeps_its_words_at_phone_width() {
    let ours = |key: &str| key.contains("macos-access") || key.contains("developer-tools");
    at_each_text_scale(&[1.5, 2.0], "macOS access", |scale| {
        let whole = whole_words_of(every_page_runtime, SettingsRoute::Security);
        for width in [178.0, 192.0] {
            for height in NARROW_HEIGHTS {
                let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
                let (mut runtime, instance, view) = every_page_runtime();
                go_to_route(&mut runtime, instance, view, SettingsRoute::Security);
                let mut buttons = std::collections::BTreeMap::<String, bool>::new();
                each_screen(
                    &mut runtime,
                    instance,
                    view,
                    &view_cx_at(width, height),
                    |screen, top| {
                        let failures = screen
                            .fit_failures()
                            .into_iter()
                            .filter(|line| line.split('"').nth(1).is_some_and(ours))
                            .collect::<Vec<_>>();
                        assert!(failures.is_empty(), "{context} screen {top}: {failures:#?}");
                        let broken = broken_inside_a_word(screen, &whole);
                        assert!(broken.is_empty(), "{context} screen {top}: {broken:#?}");
                        for node in &screen.paint {
                            if let UiContent::Button(_) = node.content
                                && ours(node.key.as_str())
                            {
                                let whole = !node.peek
                                    && node.fade.is_none()
                                    && node.clip.height >= node.rect.height - 0.5
                                    && node.clip.width >= node.rect.width - 0.5;
                                *buttons.entry(node.key.as_str().to_string()).or_default() |= whole;
                            }
                        }
                    },
                );
                assert!(
                    buttons.contains_key(MACOS_ACCESS_OPEN_FDA)
                        && buttons.contains_key(MACOS_ACCESS_OPEN_DEVTOOLS),
                    "{context}: premise, both cards drawn: {buttons:?}"
                );
                let unseen = buttons
                    .iter()
                    .filter(|(_, whole)| !**whole)
                    .collect::<Vec<_>>();
                assert!(unseen.is_empty(), "{context}: never whole: {unseen:?}");
            }
        }
    });
}

/// A WORD OF COPY WIDER THAN ITS BOX BREAKS AS A LABEL'S DOES
/// ([`copy_paragraph_lines`]): at its syllable, with a hyphen ("Mes-" /
/// "sages."), every line inside the box. The copy walk breaks only at white
/// space, and broke such a word wherever the box ended: a setting's note at
/// 2× in a 22-column card painted "Messages" and then "." alone, and the
/// label's rule then broke it "Messag-" / "es.".
/// CONTROLS: copy whose every word fits wraps at its spaces, as the walk wraps
/// it, with no hyphen; East Asian text breaks between its characters, with
/// none.
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
    // CONTROL: every word fits — the walk's lines.
    let wide = measure("Messages.") + 1.0;
    let walked: Vec<String> = crate::tray_raster::ui_text_wrap_ranges(text, px, wide)
        .into_iter()
        .map(|range| text[range].trim_end().to_string())
        .collect();
    assert_eq!(wrapped_copy_lines(text, px, wide), walked);
    // CONTROL: East Asian text, no hyphen.
    let cjk = "設定ファイルを開いて保存します";
    let lines = wrapped_copy_lines(cjk, px, measure("設定ファイ"));
    assert!(lines.len() > 1, "{lines:?}");
    assert!(lines.iter().all(|line| !line.ends_with('-')), "{lines:?}");
}

/// NO LINE OF COPY OR OF A LABEL IS ONE LETTER ALONE where a syllable of the
/// word after it fits beside it ([`copy_paragraph_lines`], [`wrap_to_box`]):
/// the Security note at 2× text in a 22-column card, in every box "A
/// program's" is too wide for and "A pro-" is not, reads "A pro-" /
/// "gram's …", every line inside the box, the words all there. CONTROL: an
/// identifier is never broken for it ("A aterm.toml" leaves "A" alone).
/// NEGATIVE CONTROL: the copy walk painted "A" alone, then "program's".
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
    // CONTROL: an identifier keeps its characters together.
    let code = "A aterm.toml key";
    let lines = wrap_to_box(code, measure("aterm.toml") + 1.0, &measure);
    assert_eq!(lines.first().map(String::as_str), Some("A"), "{lines:?}");
}

/// A SPACED SLASH ENDS ITS LINE, NEVER STARTS ONE (UAX #14 LB13,
/// [`aterm_grapheme::never_starts_a_line`]): Tabs' computers heading and a
/// window note, wrapped as a label ([`wrap_to_box`]) and as copy
/// ([`wrapped_copy_lines`]) in every box from the widest word with its
/// slash to the whole text, keep each " /" with the word before it, every
/// word whole and in order. NEGATIVE CONTROL: the heading painted "(CPU /
/// GPU" / "/ RAM /" at 2× text in a 22-column window.
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

/// A WORD WIDER THAN ITS BOX BREAKS AT A SYLLABLE, as a typesetter breaks
/// it ([`wrap_to_box`], [`aterm_grapheme::break_inside`]): the words the
/// 178pt window at 2× and 1.5× text broke, in a label's, a caption's and a
/// button's type, at every box from as narrow as the word's widest syllable
/// (and its hyphen) to as wide as the word — each line it breaks ends in a
/// hyphen at one of the word's syllable breaks, no line paints past the
/// box, the lines rejoined are the word, and a separator glued to a word
/// ("COMPUTER ·") never leaves a lone letter. NEGATIVE CONTROL: broken
/// wherever the box ended, Settings painted "Brightn-" / "ess", "AUTOMA-" /
/// "TIC", "NOTIFICATI-" / "ONS", "Foregro-" / "und", "COMPUTE-" / "R ·".
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
                // The widest run between two of its breaks, its hyphen on.
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
            // A separator glued to the word is no letter of it: the word's
            // last syllables go with it, never a lone letter, in every box
            // that holds a last syllable with its separator.
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

/// AN IDENTIFIER NO LINE HOLDS BREAKS WITH NO MARK IN IT — a path, a host, a
/// version, a hash or a file's name, which a hyphen would change
/// ([`aterm_grapheme::break_inside`]): About's values at the 22- and
/// 30-column value columns at 2× text, and Settings' file names, wrap so
/// that their lines rejoined are the value, character for character, each
/// line inside its column, and none ends in a hyphen; a host breaks before
/// its dot, a version before its build, and a path before its own hyphen
/// ("/aterm" / "-final"). CONTROL: a prose fact name still takes its hyphen at a
/// syllable ("Archi-" / "tecture"). NEGATIVE CONTROL: broken as a word,
/// About painted "0.11.0+g205-" / "65c325", "ec73ab1c-" / "007d90ca",
/// "alab.syst-" / "ems" and "Applicatio-" / "ns/", and Modified "ATERM.T-" /
/// "OML"; broken after a path's hyphen, "/aterm-" / "final" read as a word
/// broken.
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
                    // A path's own hyphen starts its next line, never ends
                    // one, where it reads as a hyphen the break put in.
                    assert!(
                        !line.ends_with('-'),
                        "{context}: {line:?} ends in a hyphen: {lines:?}"
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
        // CONTROL: a prose name still takes its hyphen, at a syllable.
        let name = wrap_to_box(
            "Architecture",
            fact_name_width("Architec"),
            &fact_name_width,
        );
        assert_eq!(name, ["Archi-", "tecture"]);
    });
}

/// A SHORT WORD IS NEVER BROKEN IN A CONTROL OR A TITLE that has room for
/// it, in a 22-column window at 2× text: a colour row's Choose… paints
/// "Choose" (its trailing "…" given up, [`button_words_in`]) where
/// "Choose…" is wider than the button and "Choose" is not; a colour field
/// whose words cannot be whole beside its chip sets the chip on a line of
/// its own and keeps its words whole (`add_color_chooser`); and a stacked
/// Messages head's title runs
/// past its chevron, which stands on the head's first row
/// ([`messages_stacked_width`]), so "Antigravity" is whole and the title is
/// not cut. NEGATIVE CONTROL: Appearance painted "Choo-" / "se…" and "The-"
/// / "me…", Messages "Antigrav-" / "ity CLI is" / "not…".
#[test]
fn a_short_word_is_never_broken_in_a_control_with_room_for_it() {
    at_each_text_scale(&[2.0], "short words", |_| {
        // Choose…, on its own line in a 122pt column.
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
        // The colour fields, on every screen of Appearance.
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
        // A stacked Messages head, in a window as tall as the report's.
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

/// A PATH IN A STATUS BREAKS BEFORE ITS FOLDERS' SLASHES, so no line of it
/// ends in a separator ([`status_word_lines`], [`aterm_grapheme::line_breaks`]):
/// the save failure of the Security page under a long lab path, at every
/// width an 80- to 150-column window at 1× and 2× text gives the status bar,
/// keeps the path's leading `/` with its first folder, starts each line the
/// path goes on with a `/` of it, ends no line in a `/` or in a hyphen of
/// the path, and keeps `lab-42` and `75d5bccd-9c38` whole. NEGATIVE
/// CONTROL: broken after each `/` and `-`, line 1 ended "was not saved: /"
/// and line 2 "lab-42/-Users-someone-aterm/75d5bccd-".
#[test]
fn a_path_in_a_status_breaks_before_its_folders_slashes() {
    let path = "/private/tmp/lab-42/-Users-someone-aterm/75d5bccd-9c38-431b-99e5-ef81935c6fbd/scratchpad/labs/fv-sk105-l80/cfg/aterm";
    let text = format!(
        "Allow programs to read the clipboard was not saved: {path} isn't writable \u{b7} check its permissions"
    );
    at_each_text_scale(&[1.0, 2.0], "status path", |scale| {
        // From the widest folder (no narrower line holds it whole) to a
        // 150-column window's bar.
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

/// A CURSOR STYLE SAYS ITS WHOLE NAME AND DESCRIPTION (#71): at 80×24,
/// 102×29 and 150×48 at 2× text and at 22 columns at 1×, every style row
/// paints its name and its description whole, wrapped to the room beside the
/// check mark ([`cursor_style_row`]) — or, where the row would be taller than
/// a screen (22 columns at 2×), its description in a block of its own under
/// it. They were cut to one line each: "Flames behind what you type and a
/// red-hot e…", "Rainbow k… (default)".
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
            // Every letter in order: a line a word broke at ends in a hyphen
            // the break added, or in the word's own ("water-" / "drop").
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

/// THE CURSOR LINK SAYS ITS WHOLE LABEL (#72): Home's "Cursor style:
/// Rainbow kitty (default)" beside More styles… where its words are whole in
/// two lines there, else over it, at 80×24 and 22 columns, 1× to 2×. At
/// 80×24 at 2× it read "Cursor style: Rainbow k…". Its button's words stay
/// whole too, its arrow given up before a word is broken ([`choice_words`]):
/// at 22 columns and 2× it painted "More" / "styl-" / "es…" beside an arrow
/// whose room held "styles…" whole.
#[test]
fn the_cursor_link_says_its_whole_label() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "cursor link", |scale| {
        for (width, height) in [(584.0, 348.0), (178.0, 348.0), (744.0, 420.0)] {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let screens = screens_of(|_, _| {}, SettingsRoute::Home, width, height);
            // Its lines, by key, across the screens that draw them whole.
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
            assert_eq!(label, "Cursor style: Rainbow kitty (default)", "{context}");
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

/// WALLPAPER KEEPS ITS STATUS AND ITS BUTTONS (#73): "No wallpaper. Dim and
/// text tint apply once you choose an image." wraps to its card, whole, at
/// 80×24, 102×29 and 150×48 at 2× text (it was cut on all three); and Choose
/// Image… and Remove go one under the other, each inside the card, where a
/// line cannot hold both (they ran past it at 22 columns, 1.5× and 2×).
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

/// TAB CONTROL KEEPS EVERY WORD OF ITS COMPUTERS AND TABS (#74): each
/// computer's name, state and address, its vitals and its system, each
/// tab's title and status, the session's line and the cards' headers wrap to
/// their card at 22 columns to 150×48, 1× and 2× — the tree's branch running
/// down beside a tab's lines — and its actions go to lines of their own. All
/// of them were cut to one line ("m110-MacBook-P…", "Mac16,5 · Darwin 26
/// ·…", "Coding Agent Har…").
#[test]
fn tab_control_keeps_every_word_of_its_computers_and_tabs() {
    at_each_text_scale(&[1.0, 2.0], "tab control", |scale| {
        for (width, height) in NARROW_WIDTHS
            .into_iter()
            .map(|width| (width, 516.0))
            .chain(WIDE_SIZES[..3].iter().copied())
        {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let whole = whole_words_of(every_page_runtime, SettingsRoute::Tabs);
            let (mut runtime, instance, view) = every_page_runtime();
            go_to_route(&mut runtime, instance, view, SettingsRoute::Tabs);
            each_screen(
                &mut runtime,
                instance,
                view,
                &view_cx_at(width, height),
                |screen, top| {
                    let cut = words_cut(screen, &whole, &["tabs/"]);
                    assert!(cut.is_empty(), "{context} screen {top}: {cut:#?}");
                    let spilled = spilled_nodes(screen, &["tabs/"]);
                    assert!(spilled.is_empty(), "{context} screen {top}: {spilled:#?}");
                },
            );
        }
    });
}

/// A PATH FIELD KEEPS ITS FILE'S NAME (#75): the fleet document's field,
/// unset, shows where aterm reads it — `ai-fleet.toml` beside aterm.toml, by
/// its path — and fits that path as a CLI's path is fitted, whole components
/// either side of an ellipsis ([`crate::native_ui::path_forms`]), its file's
/// name always, the whole path its accessible value; it read
/// "ai-fleet.toml…". Where no one line holds a form of it, its longest form
/// two lines hold goes on lines of the field, else "…/" and its name
/// ([`path_placeholder_lines`]) — so the field is whole on some screen
/// whatever the config folder's path: every folder of a long one on lines
/// made the field 691pt tall in a 268pt list at 2× in a 22-column window. A
/// path a field holds that is wider than the field keeps whole components
/// too ("/Users/…/dotfiles/ai-fleet.toml"), never a folder cut inside its
/// name.
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
                        // A form of the path ending in its file's name, whole
                        // or on lines (a path breaks at its folders and its
                        // own joints, never with a mark: its lines join with
                        // nothing between), on two lines at most where a
                        // form with a folder is shown.
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
                            Some(SemanticValue::Text(format!("{folder}/ai-fleet.toml"))),
                            "{context}"
                        );
                    },
                );
                assert!(seen, "{context}: the field is drawn");
                assert!(whole, "{context}: the field is whole on some screen");
            }
        }
    });
    // A set path wider than its field.
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

/// A PINNED SUMMARY NEVER STARVES THE LIST: Modified's count and Reset All
/// at 22 columns at 2× text took 178pt of a 268pt page and left its rows an
/// 86pt list, so the row of a key only aterm.toml holds was never whole and
/// its Reset and its way to Manual were on no screen; the summary is the
/// list's first stop there, and every control of the page is whole on some
/// screen. CONTROL: at 80×24 the summary stays pinned.
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

/// A CHOICE KEEPS ITS WORDS ON LINES: a stacked choice too narrow for its
/// words on one line, its arrow given up, sets them on as many lines as it
/// needs and its row grows ([`choice_words`]) — Home's font "Automatic (SF
/// Mono)" read "Auto…" and the theme "Def…" at 2× text in a 22-column
/// window, Sound's "Follow th…" at 1.5×; and a colour row's Choose… on a
/// line of its own wraps its word ("Choos…"), a control row's name over its
/// slider wraps ("Brightn…"), a Text & Fonts display face's switch stands
/// inside its card (145.6pt in 110) under its name.
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

/// A CARD'S CAPTION WRAPS ON EVERY PAGE: a group's caption ("ONLY IN
/// ATERM.TOML", "DISPLAY FACES"), Cursor Styles' ("ADJUST RAINBOW KITTY",
/// "COMPANIONS") and a Top card's title ("Colors & appearance") take the
/// lines their card needs at 22 columns, 1× to 2× — they were cut ("ONLY IN
/// ATE…", "ADJUST RAINBOW KI…", "Colors & appeara…") or, on a field page,
/// not drawn at all.
#[test]
fn a_cards_caption_wraps_on_every_page() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "captions", |scale| {
        for route in [
            SettingsRoute::Home,
            SettingsRoute::Modified,
            SettingsRoute::Manual,
            SettingsRoute::CursorStyles,
            SettingsRoute::TextFonts,
        ] {
            let context = format!("{scale}\u{d7} {route:?}");
            let whole = whole_words_of(every_page_runtime, route);
            let (mut runtime, instance, view) = every_page_runtime();
            go_to_route(&mut runtime, instance, view, route);
            each_screen(
                &mut runtime,
                instance,
                view,
                &view_cx_at(178.0, 516.0),
                |screen, top| {
                    let cut = words_cut(
                        screen,
                        &whole,
                        &[
                            "settings/group-heading/",
                            "settings/manual/only-in-aterm-toml/heading",
                            "settings/cursor-styles/",
                            "settings/top/",
                        ],
                    );
                    assert!(cut.is_empty(), "{context} screen {top}: {cut:#?}");
                },
            );
        }
    });
}

/// THE THEMES PAGE KEEPS ITS WORDS AND ITS BUTTONS IN ITS PANEL (#78, #61):
/// at 22 and 24 columns, 1× to 2×, a theme's name wraps in its row
/// ("Catppuccin L…"), Follow system's name wraps beside its switch
/// ("Follo…"), Import… and the folder's button go one under the other
/// inside the panel (the folder's ran out of it), the strip's Customize
/// wraps ("Custo…") and Community wraps beside its chevron ("Com…").
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

/// FOLLOW SYSTEM'S NAME GOES OVER ITS SWITCH BEFORE IT BREAKS A WORD: at 2×
/// text in a 22-column window (178x348) the 33pt the chooser's footer left
/// beside the switch set "Follow system" a few letters a line ("F-", "ol-",
/// "lo-", "w", "s-", ...), nine lines that made the footer 324pt, taller
/// than the 188pt list, so neither its name nor its switch was on any
/// screen whole. The name now takes the footer's width over the switch, its
/// words whole, and the two are whole on a screen. CONTROL: at 1× the name
/// keeps its place beside the switch, its words whole there.
#[test]
fn follow_systems_name_goes_over_its_switch_before_it_breaks_a_word() {
    // The name's painted lines on the last screen that draws them, whether
    // they sit over the switch (else beside it), and whether some screen
    // draws the name and the switch whole.
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
        assert_eq!(lines, ["Follow", "system"], "{scale}x: its words whole");
        assert_eq!(
            over,
            scale > 1.5,
            "{scale}x: over its switch only where beside breaks a word"
        );
        assert!(
            whole,
            "{scale}x: the name and the switch whole on a screen: {lines:?}"
        );
    });
}

/// A THEME NOT IN USE GIVES UP ITS EMPTY ✓ SLOT BEFORE ITS NAME BREAKS A
/// WORD: at 2× text in a 22-column window the chooser's "Dracula" broke
/// "Drac-" / "ula" beside the slot the ✓ of the theme in use stands in,
/// which paints nothing on any other row; with the slot given up it is
/// whole. CONTROL: the theme in use keeps its ✓ (and its name the lines it
/// takes beside it).
#[test]
fn a_theme_not_in_use_gives_its_name_the_check_slot() {
    // Dracula's row in the chooser filtered to it, at 2× in a 22-column
    // window, on every screen: its name's painted lines and its ✓ slot's
    // words, if it has one.
    let row = |source: &str| {
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
            &view_cx_at(178.0, 516.0),
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
        let (lines, check) = row("theme = \"Default\"\n");
        assert_eq!(lines, ["Dracula"], "the name whole");
        assert_eq!(check, None, "the empty slot is given up");
        // CONTROL: Dracula in use.
        let (lines, check) = row("theme = \"Dracula\"\n");
        assert_eq!(check.as_deref(), Some("\u{2713}"), "{lines:?}");
    });
}

/// A WRAPPED LINE OF WORDS PAINTS NO BOX (#76): words wrapped onto lines
/// are a plain group over lines that carry their style; the group took the
/// lines' style, and a group in a card's style is painted as a card, so at
/// 22 columns at 2× text Packages' "No package activity recorded yet." stood
/// in an accent-ringed box of its own, its words against the ring — and in a
/// box per piece where a screen took it a part at a time.
#[test]
fn a_wrapped_line_of_words_paints_no_box() {
    at_each_text_scale(&[2.0], "wrapped words box", |_| {
        let mut wrapped = 0;
        for screen in screens_of(|_, _| {}, SettingsRoute::Packages, 178.0, 348.0) {
            for node in &screen.paint {
                let key = node.key.as_str();
                // The card itself, and its runs off the screen, are boxes.
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

/// HARNESS LISTS A TALL SESSION ROW BY THE ONE SPLITTER (#77): a SESSIONS
/// row taller than its screen is listed a part at a time by the page's list
/// ([`super::scroll::row_parts`]), the splitter every Settings list keeps —
/// Harness kept one of its own from before it — and a landed row's parts
/// read as the one accent card it wears, open where they meet
/// ([`crate::native_ui::Joins`]); an open Timer's choices are one part.
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

/// The wraps of Latin Settings copy as `tests/fixtures/latin-wraps.txt`
/// records them: [`line_pieces`], [`wrap_to_box`] under a char measure at
/// five box widths and `ui_text_wrap_ranges` at three, as they broke before
/// CJK took the shared line breaks (taken from that code, 2026-10-21); the
/// walk's lines re-taken once a space past the box ended its line, the word
/// before it kept (a note wrapped a word early, #79), with no empty line
/// after a text's trailing space; the pieces and the box's lines re-taken
/// once a path broke before its slashes and an identifier nowhere else, and
/// a piece wider than its box where a typesetter breaks it
/// ([`aterm_grapheme::break_inside`]: "archi-" / "tec-" / "ture", not
/// "archi-" / "tectu-" / "re"; "/aterm" / ".toml", not "aterm-" / ".tom-" /
/// "l"); and once a path's own hyphen started the next line, never ended
/// one ("/aterm-lab" / "-instance-runner", not "/aterm-lab-" / "instance-").
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

/// LATIN SETTINGS COPY WRAPS AS RECORDED: breaking between CJK characters
/// (the shared line breaks, [`aterm_grapheme::line_breaks`]) moved no piece
/// and no line of a Latin name, note or path, and neither does anything
/// else unseen (the golden record: a change that moves a wrap re-takes it,
/// and the diff shows every line it moved).
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

/// Characters a line may not start with ([`aterm_grapheme::line_breaks`]'
/// kinsoku): the closers and small kana of the CJK samples below.
const NO_LINE_START: [char; 6] = [
    '\u{3002}', '\u{3001}', '\u{300d}', '\u{ff0c}', '\u{30fc}', '\u{30a1}',
];

/// CJK SETTINGS COPY WRAPS BETWEEN ITS CHARACTERS, at every box a 22- to
/// 42-column window gives a setting's name or a page's copy, 1× and 2×
/// text: each line of a name ([`row_name_lines`]) and of copy
/// ([`wrapped_copy_lines`]) fits its box and is as full as the box holds,
/// no line starts with a closer or a small kana or ends with an opener, no
/// hyphen is put inside a CJK word, and the lines rejoined are the text.
/// NEGATIVE CONTROL: broken only at spaces (else grapheme by grapheme, with
/// a hyphen in a name), a name read "設定ファイルを読み-" and copy started a
/// line with "。".
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
                // A name's lines are measured as they paint; copy's by the
                // advances its wrap sums, within half a point of that.
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
                // A name's line is as full as its box holds: the next place
                // the text may break would not fit it.
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

/// A CJK MESSAGE'S TITLE IS PAINTED ON THE MESSAGES PAGE at every window a
/// 22- to 42-column terminal gives Settings, short and tall, 1× and 2×,
/// scrolled through every stop: the head paints the title (whole, or ended
/// with `…` where its row cuts it, ruling 262), never nothing, never a line
/// starting with a closer, and each painted line inside its box.
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
