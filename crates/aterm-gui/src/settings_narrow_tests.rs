// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings' system pages at phone width and large text: every word a page
//! paints is whole in the box it is drawn in, and every control stays inside
//! its card, from a 22-column window (178pt) up, at 1× and 2× text.

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
/// LARGE TEXT (the round-9 narrow sweep): About's build information and font
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

/// A STACKED SWITCH IS NEVER WIDER THAN ITS LINE (the round-9 narrow sweep):
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

/// A NARROW SLIDER SETS ITS READOUT OVER ITS TRACK (the round-9 narrow
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

/// A STACKED CHOICE KEEPS ITS WORDS (the round-9 narrow sweep): a stacked
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

/// A BUTTON KEEPS ITS WORDS IN A NARROW COLUMN (the round-9 narrow sweep):
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
/// round-9 narrow sweep): the hero's "aterm", in the Display step where its
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

/// A WARNING STAYS A WARNING AS IT GIVES UP ITS SIZE (item A4 of the round-9
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

/// SOFTWARE UPDATE KEEPS ITS WORDS (the round-9 narrow sweep): up to date,
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

/// THE COMPACT TOOLBAR KEEPS ITS WORDS (item A3 of the round-9 narrow sweep):
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

/// A CLI'S DETAIL LINE CUTS A PATH HONESTLY (item A5 of the round-9 narrow
/// sweep): on a one-line row, a detail that names a path (a copy aterm does
/// not manage) keeps the path's tail — the binary and its folder — and cuts
/// its middle, the words around it whole where they fit; the words before it
/// cut at a word where they do not; never the path's last component. A line
/// with no path is cut back to a joint or a word. The end of the line used to
/// go, the path's tail first; the row's accessible name says it whole. A
/// PATH WITH A SPACE IN IT (`~/Library/Application Support/…`, where aterm's
/// own copies live) is the look's whole path: found by the next space, it
/// was "…/Library/Application", its binary and the words after it gone.
#[test]
fn a_cli_detail_cuts_a_path_honestly() {
    at_each_text_scale(&[1.0, 2.0], "path tail", |scale| {
        let _ = view_cx();
        let (px, face) =
            crate::native_ui::text_paint_metrics(SemanticRole::Status, StyleRef::Quiet);
        let width_of = |text: &str| crate::tray_raster::ui_text_width_for(face, text, px);
        let cases = [
            (
                "/Users//someone/.local/share/tools/bin/claude",
                "not installed by aterm (a copy it does not manage is at \
                 /Users//someone/.local/share/tools/bin/claude)",
                "/bin/claude",
            ),
            (
                "/Users//someone/Library/Application Support/aterm/pkg/reroute/agy",
                "not installed by aterm (a copy it does not manage is at \
                 /Users//someone/Library/Application Support/aterm/pkg/reroute/agy) \
                 (not measured in this build)",
                "/reroute/agy",
            ),
        ];
        for (path, detail, tail) in cases {
            let binary = tail.rsplit('/').next().expect("a binary");
            let whole = width_of(detail);
            for fraction in [0.95_f32, 0.8, 0.6, 0.45, 0.3, 0.2] {
                let width = whole * fraction;
                let context = format!("{scale}\u{d7} {binary} {width:.0}pt");
                let line = ai_page::fit_keeping_path_tail(detail, Some(path), width);
                assert!(width_of(&line) <= width + 0.01, "{context}: {line:?} fits");
                assert!(
                    line.contains(&format!("/{binary}")) || line == binary,
                    "{context}: the binary is never cut: {line:?}"
                );
                assert!(
                    !line.contains("Application\u{2026}") && !line.ends_with("Application"),
                    "{context}: a folder with a space is never the path's end: {line:?}"
                );
                if line.contains(tail) {
                    assert!(
                        line.contains('\u{2026}'),
                        "{context}: an honest cut: {line:?}"
                    );
                }
            }
            // Where the line holds the words and the path's tail, it keeps them.
            let roomy = ai_page::fit_keeping_path_tail(detail, Some(path), whole * 0.8);
            assert!(
                roomy.starts_with("not installed by aterm (a copy it does not manage is at ")
                    && roomy.contains(&format!("{tail})")),
                "{scale}\u{d7}: {roomy:?}"
            );
            // What follows the path stays where it fits.
            if detail.ends_with("(not measured in this build)") {
                assert!(
                    roomy.ends_with(") (not measured in this build)"),
                    "{scale}\u{d7}: {roomy:?}"
                );
            }
        }
        // No path: back to a joint, never inside a word where a word fits.
        let facts = "ok \u{b7} signed in (Max) \u{b7} served 2 h ago";
        let cut = ai_page::fit_keeping_path_tail(
            facts,
            None,
            width_of("ok \u{b7} signed in (Max) \u{b7} \u{2026}") + 1.0,
        );
        assert_eq!(cut, "ok \u{b7} signed in (Max) \u{b7} \u{2026}");
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

/// A PAGE'S NAME IS WHOLE IN ITS COLUMN (the round-9 narrow sweep): every
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

/// THE PACKAGES PAGE KEEPS ITS WORDS (the round-9 narrow sweep): its
/// status, verbs, captions, service line, programs and activity at 22 to 42
/// columns and on a wide window, 1× and 2× — wrapped to the card's MEASURED
/// column, never cut, nothing past a card. Its lines were budgeted in
/// average glyphs per width class (56 a compact line, for 6 words at 2×), its
/// three verbs held at a fixed width whatever their words, its captions and
/// its service line's columns fixed.
#[test]
fn the_packages_page_keeps_its_words() {
    at_each_text_scale(&[1.0, 2.0], "Packages", |scale| {
        for width in NARROW_WIDTHS.into_iter().chain([744.0]) {
            for height in [348.0, 900.0] {
                let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
                for (index, screen) in screens_of(|_, _| {}, SettingsRoute::Packages, width, height)
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

/// MESSAGES' SMALL BUTTONS STAY ON THE PAGE (the round-9 narrow sweep): Copy
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

/// HARNESS KEEPS ITS WORDS WITH SESSIONS OPEN (the round-9 narrow sweep):
/// the page with two sessions, at 22 to 42 columns, short and tall, 1× and
/// 2× — every row's title, detail, setting, state and day whole on as many
/// lines as its column needs, every face of its buttons whole, nothing past
/// its card; a row taller than one screen of its card listed a part at a time
/// ([`harness_page::split_tall_block`]). A short window at 2× crammed the
/// buttons five to a line to keep the row on one screen ("Cru…", "…") and cut
/// the words to the column ("Tab 2 · Cla…").
#[test]
fn harness_keeps_its_words_with_sessions_open() {
    at_each_text_scale(&[1.0, 2.0], "Harness sessions", |scale| {
        let mut parts = 0;
        for width in NARROW_WIDTHS {
            for height in NARROW_HEIGHTS {
                let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
                let prepare = |runtime: &mut NativeRuntime, _: crate::native_app::ViewId| {
                    assert!(runtime.replace_settings_harness(super::harness::fixture(), 1));
                };
                for (index, screen) in screens_of(prepare, SettingsRoute::Harness, width, height)
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
        if scale > 1.5 {
            assert!(
                parts > 0,
                "{scale}\u{d7}: a short window lists a row a part at a time"
            );
        }
    });
}

/// A ONE-LINE SESSIONS ROW KEEPS ITS WORDS (the round-9 sweep): on the
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

/// A PAGE'S SUBTITLE IS WHOLE IN ITS COLUMN (the round-9 sweep): every route
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
/// ON A WIDE PAGE TOO (the round-9 sweep; the rule a compact row keeps): at
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

/// SEARCH'S RESULTS AND MODIFIED KEEP THEIR WORDS (the round-9 narrow sweep,
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

/// PACKAGES' SERVICE LINE IS LAID OUT AT THE GAP IT WAS MEASURED WITH (the
/// round-9 lab sweep): at every card text width from a phone's to a wide
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

/// MESSAGES KEEPS ITS WORDS WITH A LOG (the round-9 lab sweep, which a log
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
        state.page_scroll = stop;
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
            assert_eq!(painted_words(&said), note, "{context}: every word paints");
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
        if screens
            .last()
            .is_some_and(|last| last.paint_audit_lines() == screen.paint_audit_lines())
        {
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
/// and 2×, with and without a package tool, each control is drawn whole on
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
                        let (want, have) = (words(label), words(&said.join("\n")));
                        if have.first() != want.first() {
                            continue;
                        }
                        let missing = want
                            .iter()
                            .filter(|word| !have.contains(word))
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
