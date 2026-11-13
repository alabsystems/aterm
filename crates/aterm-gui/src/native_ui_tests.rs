// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// [`merge_icon_strokes`] merges what restarting the pair scan after every merge merged,
/// in the same order, so every union is the same float: random strokes, words and art
/// among them, and a prefix before `start` left alone.
#[test]
fn icon_strokes_merge_as_a_restarted_scan_merges_them() {
    fn restarted(marks: &mut Vec<InkMark>, start: usize) {
        let stroke = |m: &InkMark| m.words.is_none() && m.art.is_none();
        'again: loop {
            for a in start..marks.len() {
                for b in a + 1..marks.len() {
                    let one = marks[a].rect;
                    let grown = LogicalRect::new(
                        one.x - INK_OVERLAP_LEAST,
                        one.y - INK_OVERLAP_LEAST,
                        one.width + 2.0 * INK_OVERLAP_LEAST,
                        one.height + 2.0 * INK_OVERLAP_LEAST,
                    );
                    if stroke(&marks[a])
                        && stroke(&marks[b])
                        && grown.intersect(marks[b].rect).is_some()
                    {
                        marks[a].rect = one.union(marks[b].rect);
                        marks.remove(b);
                        continue 'again;
                    }
                }
            }
            return;
        }
    }
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    for round in 0..400 {
        let count = 1 + next(40) as usize;
        let spread = 20.0 + next(200) as f32;
        let specs: Vec<(LogicalRect, u64)> = (0..count)
            .map(|_| {
                let at = |n: &mut dyn FnMut(u64) -> u64| n(1000) as f32 / 1000.0 * spread;
                let rect = LogicalRect::new(
                    at(&mut next),
                    at(&mut next),
                    0.5 + next(30) as f32 / 3.0,
                    0.5 + next(30) as f32 / 3.0,
                );
                (rect, next(8))
            })
            .collect();
        let marks = || -> Vec<InkMark> {
            specs
                .iter()
                .enumerate()
                .map(|(node, &(rect, kind))| InkMark {
                    node,
                    words: (kind == 0).then(|| String::from("words")),
                    rect,
                    art: (kind == 1).then_some((
                        crate::widget::ImageArt::Kitty {
                            pose: crate::widget::ImageArt::BRAND_POSE,
                            dark: false,
                        },
                        rect,
                    )),
                })
                .collect()
        };
        let start = next(count as u64 + 1) as usize;
        let (mut want, mut got) = (marks(), marks());
        restarted(&mut want, start);
        merge_icon_strokes(&mut got, start);
        let shape = |m: &[InkMark]| {
            m.iter()
                .map(|m| (m.node, m.rect.x, m.rect.y, m.rect.width, m.rect.height))
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(&got), shape(&want), "round {round}, start {start}");
    }
}

#[test]
fn a_status_is_cut_after_a_whole_word() {
    let status = "Cursor blink was not saved: ~/.config/aterm isn\u{2019}t writable \u{b7} check its permissions";
    let whole = native_status_label_width(status);
    let mut mid_word = 0;
    let mut width = 40.0_f32;
    while width < whole + 4.0 {
        let shown = fit_native_status_label(status, width);
        if shown != status {
            let stem = shown.strip_suffix('…').expect("a cut ends in an ellipsis");
            assert!(
                !stem.contains(char::is_whitespace)
                    || status[stem.len()..].starts_with(|c: char| {
                        c.is_whitespace() || matches!(c, ':' | ',' | '·')
                    }),
                "{width}: {shown:?} cuts a word"
            );
            assert!(native_status_label_width(&shown) <= width + TEXT_FIT_SLACK);
        }
        let general = elide_text_label(
            status,
            width,
            native_type_px(TypeStep::Caption).get(),
            crate::widget::TextFace::Ui,
        );
        if let Some(stem) = general.strip_suffix('…')
            && !status[stem.len()..].starts_with(char::is_whitespace)
        {
            mid_word += 1;
        }
        width += 3.0;
    }
    assert_eq!(fit_native_status_label(status, whole + 1.0), status);
    assert!(
        mid_word > 0,
        "the control: the general elision cuts mid-word"
    );
}

#[test]
fn a_cut_one_line_label_ends_after_a_whole_word() {
    let label = "Claude Code is not installed";
    let words: Vec<&str> = label.split_whitespace().collect();
    for (fit, measure) in [
        (
            fit_native_text_label as fn(&str, f32) -> String,
            native_type_px(TypeStep::Body).get(),
        ),
        (
            fit_native_button_label as fn(&str, f32) -> String,
            native_type_px(TypeStep::Secondary).get(),
        ),
    ] {
        let whole = crate::tray_raster::ui_text_width(label, measure);
        let first = crate::tray_raster::ui_text_width("Claude…", measure);
        let mut width = first + 1.0;
        while width < whole {
            let cut = fit(label, width);
            let stem = cut.strip_suffix('…').unwrap_or(&cut).trim_end();
            let kept = stem.split_whitespace().count();
            assert_eq!(
                stem,
                words[..kept].join(" "),
                "{width}: {cut:?} ends after a whole word"
            );
            width += 3.0;
        }
        assert_eq!(fit(label, whole + 1.0), label);
    }
}

#[test]
fn every_aurora_tile_holds_the_page_chrome_ink() {
    use crate::native_appearance::{contrast_ratio, forced_chrome_theme};
    use crate::settings::{AURORA_INK_FLOOR, aurora_tile_unheld};
    let host = aterm_render::Theme::default();
    let themes = [
        ("default", host),
        ("forced light", forced_chrome_theme(host, false)),
        ("forced dark", forced_chrome_theme(host, true)),
    ];
    for (name, theme) in themes {
        let roles = crate::settings::Roles::from_theme(theme);
        let ink = quiet_text(&roles);
        let mut prims = Vec::new();
        crate::settings::paint_settings_aurora(
            &mut prims,
            0.0,
            0.0,
            1_120.0,
            640.0,
            roles.surface,
            ink,
        );
        assert!(!prims.is_empty(), "{name}: the aurora paints");
        let mut darkest = f32::INFINITY;
        for prim in &prims {
            let crate::widget::DrawPrim::Panel { fill, .. } = prim else {
                panic!("{name}: the aurora is tiles");
            };
            let tile = [fill[0], fill[1], fill[2]];
            for (what, text) in [("quiet", ink), ("heading", roles.text_primary)] {
                let ratio = contrast_ratio(text, tile);
                darkest = darkest.min(ratio);
                assert!(
                    ratio >= AURORA_INK_FLOOR,
                    "{name}: {what} ink on tile {tile:?} is {ratio:.2}:1"
                );
            }
        }
        assert!(darkest.is_finite());
    }
    let roles = crate::settings::Roles::from_theme(host);
    let ink = quiet_text(&roles);
    let failing = (0..64)
        .flat_map(|j| (0..112).map(move |i| (i, j)))
        .filter(|&(i, j)| {
            contrast_ratio(ink, aurora_tile_unheld(roles.surface, i, j)) < AURORA_INK_FLOOR
        })
        .count();
    assert!(
        failing > 0,
        "control: the unheld pour fails the quiet ink somewhere on the default palette"
    );
}

#[test]
fn a_disabled_control_is_never_painted_focused() {
    let button = |enabled: bool| {
        UiNode::new(
            "test/older",
            UiContent::Button(
                Control::new(ButtonSpec::new("Older"), ActionId::new("test/older")).state(
                    ControlState {
                        enabled,
                        ..ControlState::default()
                    },
                ),
            ),
        )
        .layout(Layout::default().height(Length::Fixed(32.0)))
    };
    for enabled in [false, true] {
        let tree = UiTree::new(
            UiNode::new("test", UiContent::Group(GroupSpec::new("test")))
                .layout(Layout::column())
                .children(vec![button(enabled)]),
        )
        .apply_focus(Some(&UiKey::new("test/older")));
        let UiContent::Button(control) = &tree.root.children[0].content else {
            unreachable!();
        };
        assert_eq!(control.state.focused, enabled, "enabled={enabled}");
        assert_eq!(control.state.focus_visible, enabled, "enabled={enabled}");
    }
}

#[test]
fn the_hero_banner_sky_survives_a_mid_tone_terminal_theme() {
    use crate::native_appearance::{contrast_ratio, forced_chrome_theme};
    use aterm_render::Theme;

    let openness = |c: [u8; 3]| contrast_ratio(c, [0, 0, 0]);
    let green_lean = |c: [u8; 3]| i32::from(c[1]) * 2 - i32::from(c[0]) - i32::from(c[2]);
    const VISIBLE_LEAN: i32 = 6;

    let mut band = Vec::new();
    let mut bg = 0x60_u32;
    while bg <= 0xB0 {
        let neutral = (bg << 16) | (bg << 8) | bg;
        let name = format!("#{bg:02X}{bg:02X}{bg:02X}");
        band.push((format!("{name} on light ink"), neutral, 0x00EE_EEEE_u32));
        band.push((format!("{name} on dark ink"), neutral, 0x0011_1111));
        bg += 4;
    }
    band.push(("#797676 on light ink".to_string(), 0x0079_7676, 0x00EE_EEEE));
    band.push(("#999955 on dark ink".to_string(), 0x0099_9955, 0x0022_2211));
    band.push(("#999955 on light ink".to_string(), 0x0099_9955, 0x00EE_EEEE));
    let mut themes: Vec<(String, Theme)> = band
        .into_iter()
        .map(|(label, bg, fg)| {
            let theme = Theme {
                fg,
                bg,
                cursor: 0x0085_9900,
                selection: 0x00EE_E8D5,
            };
            (label, theme)
        })
        .collect();
    themes.push((
        "forced-light".to_string(),
        forced_chrome_theme(Theme::default(), false),
    ));
    themes.push((
        "forced-dark".to_string(),
        forced_chrome_theme(FORCED_DARK_TERMINAL, true),
    ));

    for (label, theme) in themes {
        let roles = crate::settings::Roles::from_theme(theme);
        let (sky, rim) = rainbow_banner_sky(&roles);

        assert!(
            openness(sky) >= openness(roles.elevated),
            "{label}: the banner sky {sky:?} ({:.2}) sits heavier than the cards {:?} ({:.2})",
            openness(sky),
            roles.elevated,
            openness(roles.elevated),
        );
        let base = if openness(roles.elevated) >= openness(roles.surface) {
            roles.elevated
        } else {
            roles.surface
        };
        assert!(
            green_lean(sky) - green_lean(base) >= VISIBLE_LEAN,
            "{label}: the banner sky {sky:?} carries no visible green over its base {base:?}",
        );
        let rim_vs_sky = contrast_ratio(rim, sky);
        assert!(
            rim_vs_sky > 1.08,
            "{label}: the banner rim {rim:?} vanishes into its own sky ({rim_vs_sky:.2}:1)"
        );
        assert!(
            rim_vs_sky <= contrast_ratio(roles.text_primary, sky),
            "{label}: the banner rim outshouts the page's own ink"
        );
    }
}

#[test]
fn a_badged_rail_row_paints_one_danger_dot_clear_of_its_label_and_chevron() {
    use crate::native_appearance::forced_chrome_theme;
    use crate::widget::DrawPrim;
    use aterm_render::Theme;
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let theme = forced_chrome_theme(Theme::default(), false);
    let roles = crate::settings::Roles::from_theme(theme);
    let node = |spec: ButtonSpec, rect: LogicalRect| PaintNode {
        cut_edges: CutEdges::NONE,
        peek: false,
        fade: None,
        key: UiKey::new("settings/nav/packages"),
        rect,
        clip: rect,
        content: UiContent::Button(
            Control::new(spec, ActionId::new("settings/route/packages"))
                .style(StyleRef::Navigation),
        ),
        joins: Joins::default(),
    };
    let dots = |node: &PaintNode| -> Vec<(f32, f32, f32)> {
        let mut prims = Vec::new();
        paint_compiled_node(&mut prims, node, roles, theme, 14.0);
        prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Dot {
                    cx, cy, r, color, ..
                } if color[..3] == roles.danger => Some((*cx, *cy, *r)),
                _ => None,
            })
            .collect()
    };
    let icon_row = || ButtonSpec::new("Packages").visual_icon(ButtonIcon::Packages);
    let chevron_row = || ButtonSpec::new("Packages").trailing_icon(ButtonIcon::Forward);
    for (shape, spec, rect, labelled) in [
        (
            "icon row",
            icon_row as fn() -> ButtonSpec,
            LogicalRect::new(8.0, 40.0, 180.0, 34.0),
            true,
        ),
        (
            "icon-only row",
            icon_row,
            LogicalRect::new(8.0, 40.0, 48.0, 34.0),
            false,
        ),
        (
            "chevron row",
            chevron_row,
            LogicalRect::new(0.0, 0.0, 320.0, 44.0),
            true,
        ),
    ] {
        assert!(
            dots(&node(spec(), rect)).is_empty(),
            "{shape}: no badge, no dot"
        );
        let badged = node(spec().badge(true), rect);
        let found = dots(&badged);
        assert_eq!(found.len(), 1, "{shape}: one dot: {found:?}");
        let (cx, cy, r) = found[0];
        assert!(rect.contains(cx, cy), "{shape}: {cx},{cy} inside {rect:?}");
        if shape == "chevron row" {
            assert!(
                cx + r < rect.right() - 34.0,
                "the dot ({cx}±{r}) stays left of the chevron's slot"
            );
        }
        if labelled {
            let audit = text_fit_audit(&badged).expect("a rail label is audited");
            let plain = text_fit_audit(&node(spec(), rect)).expect("audited");
            assert!(
                audit.authored_available < plain.authored_available,
                "{shape}: the badge takes its room from the label's slot: {} vs {}",
                audit.authored_available,
                plain.authored_available
            );
        }
    }
}

#[test]
fn button_color_chip_paints_beside_the_trailing_icon() {
    use crate::native_appearance::forced_chrome_theme;
    use crate::widget::DrawPrim;
    use aterm_render::Theme;
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let theme = forced_chrome_theme(Theme::default(), false);
    let roles = crate::settings::Roles::from_theme(theme);
    let orange = [0xE6, 0x7A, 0x00];
    let rect = LogicalRect::new(20.0, 30.0, 140.0, 36.0);
    let node = |chip: bool| {
        let spec = ButtonSpec::new("Choose the selected tab color; now #E67A00")
            .visual_label("#E67A00 and a label long enough to be cut")
            .trailing_icon(ButtonIcon::Forward);
        PaintNode {
            cut_edges: CutEdges::NONE,
            peek: false,
            fade: None,
            key: UiKey::new("settings/control/active_tab_color"),
            rect,
            clip: rect,
            content: UiContent::Button(
                Control::new(
                    ButtonSpec {
                        chip: chip.then_some(orange),
                        ..spec
                    },
                    ActionId::new("settings/route/tab-color"),
                )
                .style(StyleRef::Setting),
            ),
            joins: Joins::default(),
        }
    };
    let paint = |chip: bool| {
        let mut prims = Vec::new();
        paint_compiled_node(&mut prims, &node(chip), roles, theme, 14.0);
        prims
    };
    let chips = |prims: &[DrawPrim]| -> Vec<(f32, f32, f32, f32)> {
        prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Panel {
                    x, y, w, h, fill, ..
                } if fill[..3] == orange => Some((*x, *y, *w, *h)),
                _ => None,
            })
            .collect()
    };
    assert!(chips(&paint(false)).is_empty(), "no chip, no colour");
    let prims = paint(true);
    let found = chips(&prims);
    assert_eq!(found.len(), 1, "one chip: {found:?}");
    let (x, y, w, h) = found[0];
    assert_eq!((w, h), (TEXT_FIELD_SWATCH_SIZE, TEXT_FIELD_SWATCH_SIZE));
    assert!(
        x >= rect.x && y >= rect.y && y + h <= rect.bottom(),
        "inside the button: {found:?} in {rect:?}"
    );
    assert!(
        x + w <= rect.right() - 34.0,
        "the chip ends before the arrow's slot: {found:?}"
    );
    assert!(
        (y + h / 2.0 - (rect.y + rect.height / 2.0)).abs() < 0.01,
        "centred on the row"
    );
    let text_right = prims
        .iter()
        .find_map(|prim| match prim {
            DrawPrim::Text { x, s, px, face, .. } => {
                Some(x + crate::tray_raster::ui_text_width_for(*face, s, *px))
            }
            _ => None,
        })
        .expect("the label is painted");
    assert!(
        text_right <= x - TEXT_FIELD_SWATCH_GAP + 0.5,
        "the label ends at {text_right}, before the chip at {x}"
    );
    let with_chip = text_fit_audit(&node(true)).expect("audited");
    let plain = text_fit_audit(&node(false)).expect("audited");
    assert!(
        (plain.authored_available
            - with_chip.authored_available
            - (TEXT_FIELD_SWATCH_SIZE + TEXT_FIELD_SWATCH_GAP))
            .abs()
            < 0.01,
        "the chip takes its room from the label: {} vs {}",
        plain.authored_available,
        with_chip.authored_available
    );
    assert!(
        (text_button_label_room(
            &ButtonSpec {
                chip: Some(orange),
                ..ButtonSpec::new("x").trailing_icon(ButtonIcon::Forward)
            },
            rect.width
        ) - with_chip.authored_available)
            .abs()
            < 0.01,
        "a caller that fits a label first measures the painter's slot"
    );
}

#[test]
fn semibold_goes_only_where_a_slot_was_sized_with_room_to_grow_ink() {
    use crate::native_appearance::forced_chrome_theme;
    use crate::widget::{DrawPrim, TextFace};
    use aterm_render::Theme;

    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();

    let rect = LogicalRect::new(8.0, 40.0, 180.0, 34.0);
    let node = |spec: ButtonSpec, selected: bool| {
        let mut control =
            Control::new(spec, ActionId::new("chrome/nav")).style(StyleRef::Navigation);
        control.state.selected = selected;
        PaintNode {
            cut_edges: CutEdges::NONE,
            peek: false,
            fade: None,
            key: UiKey::new("chrome/nav"),
            rect,
            clip: rect,
            content: UiContent::Button(control),
            joins: Joins::default(),
        }
    };
    let rail_row = || ButtonSpec::new("Appearance").visual_icon(ButtonIcon::Home);
    let outline_row = || ButtonSpec::new("A document heading");
    let mode_chip = || ButtonSpec::new("Preview mode").visual_label("Preview");

    let theme = forced_chrome_theme(Theme::default(), false);
    let roles = crate::settings::Roles::from_theme(theme);
    let paint = |spec: ButtonSpec, selected: bool| {
        let mut prims = Vec::new();
        paint_compiled_node(&mut prims, &node(spec, selected), roles, theme, 14.0);
        prims
    };
    let semibold = |prims: &[DrawPrim]| {
        prims
            .iter()
            .any(|p| matches!(p, DrawPrim::Text { face, .. } if *face == TextFace::UiBold))
    };
    let accent_rim = |prims: &[DrawPrim]| {
        prims.iter().any(|p| {
            matches!(
                p,
                DrawPrim::Stroke { w, color, .. }
                    if *w >= rect.width - 2.0 && color[..3] == roles.accent
            )
        })
    };

    assert!(
        semibold(&paint(rail_row(), true)),
        "the selected rail destination is not semibold — selection reads as nothing"
    );
    assert!(
        !semibold(&paint(rail_row(), false)),
        "a resting rail row is already semibold"
    );
    assert!(
        !semibold(&paint(outline_row(), true)),
        "a selected outline row grew semibold ink in a slot its heading already fills"
    );
    let chip = paint(mode_chip(), true);
    assert!(
        !semibold(&chip),
        "a selected toolbar chip grew semibold ink in a slot measured for regular"
    );
    assert!(
        accent_rim(&chip),
        "a selected toolbar chip lost its accent rim"
    );

    let px = native_type_px(TypeStep::Secondary).get();
    let slot = (crate::tray_raster::ui_text_width("Preview", 13.0) + 20.0)
        .ceil()
        .max(62.0);
    let chip_rect = LogicalRect::new(8.0, 40.0, slot, 34.0);
    let mut control = Control::new(mode_chip(), ActionId::new("markdown/mode/preview"))
        .style(StyleRef::Navigation);
    control.state.selected = true;
    let chip_node = PaintNode {
        cut_edges: CutEdges::NONE,
        peek: false,
        fade: None,
        key: UiKey::new("markdown/preview-mode"),
        rect: chip_rect,
        clip: chip_rect,
        content: UiContent::Button(control),
        joins: Joins::default(),
    };
    let mut prims = Vec::new();
    paint_compiled_node(&mut prims, &chip_node, roles, theme, 14.0);
    let painted = prims.iter().find_map(|p| match p {
        DrawPrim::Text { s, .. } => Some(s.clone()),
        _ => None,
    });
    assert_eq!(
        painted.as_deref(),
        Some("Preview"),
        "the selected mode chip elided its own word inside its authored {slot}pt slot \
         (semibold in a space measured for regular at {px}px)"
    );
}

#[test]
fn a_selected_rail_row_wears_its_platforms_selection() {
    use crate::native_appearance::forced_chrome_theme;
    use crate::widget::DrawPrim;
    use aterm_render::Theme;

    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let theme = forced_chrome_theme(Theme::default(), false);
    let roles = crate::settings::Roles::from_theme(theme);
    let rect = LogicalRect::new(8.0, 40.0, 180.0, 34.0);
    let paint = |spec: ButtonSpec, selected: bool| {
        let mut control =
            Control::new(spec, ActionId::new("chrome/nav")).style(StyleRef::Navigation);
        control.state.selected = selected;
        let node = PaintNode {
            cut_edges: CutEdges::NONE,
            peek: false,
            fade: None,
            key: UiKey::new("chrome/nav"),
            rect,
            clip: rect,
            content: UiContent::Button(control),
            joins: Joins::default(),
        };
        let mut prims = Vec::new();
        paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
        prims
    };
    let mark = |prims: &[DrawPrim]| {
        prims.iter().find_map(|p| match p {
            DrawPrim::Panel { x, w, h, fill, .. }
                if *x == rect.x && *w <= 4.0 && fill[..3] == roles.accent =>
            {
                Some((*w, *h))
            }
            _ => None,
        })
    };
    let accent_rim = |prims: &[DrawPrim]| {
        prims.iter().any(|p| {
            matches!(
                p,
                DrawPrim::Stroke { w, color, .. }
                    if *w >= rect.width - 8.0 && color[..3] == roles.accent
            )
        })
    };
    let ink = |prims: &[DrawPrim]| {
        prims.iter().find_map(|p| match p {
            DrawPrim::Text { color, .. } => Some([color[0], color[1], color[2]]),
            _ => None,
        })
    };
    let rail_row = || ButtonSpec::new("Appearance").visual_icon(ButtonIcon::Home);
    let selected = paint(rail_row(), true);
    assert_eq!(
        mark(&paint(rail_row(), false)),
        None,
        "a resting row is unmarked"
    );
    if FLUENT_RAIL_SELECTION {
        assert_eq!(mark(&selected), Some((3.0, 16.0)), "WinUI's pill");
        assert!(!accent_rim(&selected), "no accent rim around the row");
        assert_eq!(
            ink(&selected),
            Some(roles.text_primary),
            "the ink is unchanged"
        );
        let chip = paint(
            ButtonSpec::new("Preview mode").visual_label("Preview"),
            true,
        );
        assert!(accent_rim(&chip), "a chip keeps its accent cell");
    } else {
        assert_eq!(
            mark(&selected),
            Some((4.0, rect.height - 8.0)),
            "the rail bar"
        );
        assert!(!accent_rim(&selected), "no rim: the outline is focus's");
        assert_eq!(ink(&selected), Some(roles.accent_ink), "the accent as text");
    }
}

#[test]
fn the_paint_audit_elides_the_same_label_the_painter_paints() {
    use crate::native_appearance::forced_chrome_theme;
    use crate::widget::DrawPrim;
    use aterm_render::Theme;

    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();

    let theme = forced_chrome_theme(Theme::default(), true);
    let roles = crate::settings::Roles::from_theme(theme);
    let rect = LogicalRect::new(8.0, 40.0, 132.0, 34.0);
    let specs = [
        ButtonSpec::new("Appearance and window theme").visual_icon(ButtonIcon::Home),
        ButtonSpec::new("A document heading long enough to elide"),
        ButtonSpec::new("Preview mode").visual_label("Preview"),
        ButtonSpec::new("Software Update").trailing_icon(ButtonIcon::Forward),
        ButtonSpec::new("Appearance and window theme")
            .visual_icon(ButtonIcon::Home)
            .badge(true),
        ButtonSpec::new("Software Update")
            .trailing_icon(ButtonIcon::Forward)
            .badge(true),
    ];
    for spec in specs {
        for selected in [false, true] {
            let mut control =
                Control::new(spec.clone(), ActionId::new("chrome/nav")).style(StyleRef::Navigation);
            control.state.selected = selected;
            let node = PaintNode {
                cut_edges: CutEdges::NONE,
                peek: false,
                fade: None,
                key: UiKey::new("chrome/nav"),
                rect,
                clip: rect,
                content: UiContent::Button(control),
                joins: Joins::default(),
            };
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
            let painted = prims.iter().find_map(|p| match p {
                DrawPrim::Text { s, .. } => Some(s.clone()),
                _ => None,
            });
            let audited = text_fit_audit(&node).map(|audit| audit.painted);
            assert_eq!(
                painted, audited,
                "{:?} (selected={selected}): the paint-truth audit and the painter \
                 disagree about the label that reaches the glass",
                spec.label
            );
        }
    }
}

const FORCED_DARK_TERMINAL: aterm_render::Theme = aterm_render::Theme {
    fg: 0x0065_7B83,
    bg: 0x00FD_F6E3,
    cursor: 0x0085_9900,
    selection: 0x00EE_E8D5,
};

#[test]
fn the_hero_banner_sky_follows_the_resolved_chrome_palette() {
    use crate::native_appearance::{forced_chrome_theme, surface_is_dark};
    use aterm_render::Theme;

    let sky_for = |theme: Theme| rainbow_banner_sky(&crate::settings::Roles::from_theme(theme)).0;
    let light_sky = sky_for(forced_chrome_theme(Theme::default(), false));
    let dark_sky = sky_for(forced_chrome_theme(FORCED_DARK_TERMINAL, true));
    assert_ne!(
        light_sky, dark_sky,
        "the two forced chrome sides paint the same sky — window_theme is a no-op here again"
    );
    assert!(
        !surface_is_dark(light_sky),
        "the forced-LIGHT banner sky {light_sky:?} classifies dark"
    );
    assert!(
        surface_is_dark(dark_sky),
        "the forced-DARK banner sky {dark_sky:?} classifies light"
    );
}

#[test]
fn a_rail_rows_outline_is_focus_alone_never_selection() {
    use crate::native_appearance::forced_chrome_theme;
    use crate::widget::DrawPrim;
    use aterm_render::Theme;

    let terminal_light = Theme {
        fg: 0x0065_7B83,
        bg: 0x00FD_F6E3,
        cursor: 0x0085_9900,
        selection: 0x00EE_E8D5,
    };
    let rect = LogicalRect::new(8.0, 40.0, 180.0, 34.0);
    let row = |selected: bool, focus_visible: bool| {
        let mut control = Control::new(
            ButtonSpec::new("Top Settings").visual_icon(ButtonIcon::Home),
            ActionId::new("settings/nav/top"),
        )
        .style(StyleRef::Navigation);
        control.state.selected = selected;
        control.state.focus_visible = focus_visible;
        PaintNode {
            cut_edges: CutEdges::NONE,
            peek: false,
            fade: None,
            key: UiKey::new("settings/nav/top"),
            rect,
            clip: rect,
            content: UiContent::Button(control),
            joins: Joins::default(),
        }
    };
    for (label, theme) in [
        ("forced-light", forced_chrome_theme(Theme::default(), false)),
        ("forced-dark", forced_chrome_theme(terminal_light, true)),
    ] {
        let roles = crate::settings::Roles::from_theme(theme);
        for selected in [false, true] {
            for focus_visible in [false, true] {
                let mut prims = Vec::new();
                paint_compiled_node(
                    &mut prims,
                    &row(selected, focus_visible),
                    roles,
                    theme,
                    14.0,
                );
                let ring = prims.iter().find_map(|p| match p {
                    DrawPrim::Stroke {
                        w, width, color, ..
                    } if *w >= rect.width - 2.0 && color[..3] == roles.accent => Some(*width),
                    _ => None,
                });
                assert_eq!(
                    ring,
                    focus_visible.then_some(2.0),
                    "{label}: selected={selected} focus={focus_visible}"
                );
                if selected {
                    let bar = prims.iter().any(|p| {
                        matches!(
                            p,
                            DrawPrim::Panel { x, w, fill, .. }
                                if (*x - rect.x).abs() < 0.01
                                    && *w <= 6.0
                                    && fill[..3] == roles.accent
                        )
                    });
                    let wash = prims
                        .iter()
                        .any(|p| matches!(p, DrawPrim::Panel { w, .. } if *w >= rect.width - 0.01));
                    assert!(bar && wash, "{label}: focus={focus_visible}");
                }
            }
        }
    }
}

#[test]
fn responsive_button_and_status_labels_preserve_complete_graphemes() {
    for (source, expected) in [("A👩‍💻WWWW", "A👩‍💻…"), ("AéWWWW", "Aé…"), ("A🇺🇳WWWW", "A🇺🇳…")]
    {
        for step in [TypeStep::Secondary, TypeStep::Caption] {
            let px = native_type_px(step).get();
            let max_width = visual_text_width(expected, px, crate::widget::TextFace::Ui) + 0.01;
            let fitted = if step == TypeStep::Secondary {
                fit_native_button_label(source, max_width)
            } else {
                fit_native_status_label(source, max_width)
            };
            assert_eq!(fitted, expected, "{source:?} at {step:?}");
        }
    }

    let capped_source = format!("{}👩‍💻W", "a".repeat(255));
    let capped_expected = format!("{}👩‍💻…", "a".repeat(255));
    assert_eq!(
        elide_text_label(
            &capped_source,
            100_000.0,
            native_type_px(TypeStep::Secondary).get(),
            crate::widget::TextFace::Ui,
        ),
        capped_expected
    );
}

#[test]
fn a_label_fitted_to_its_own_measure_through_a_padded_width_is_whole() {
    let face = crate::widget::TextFace::Ui;
    for px in [11.0_f32, 12.0, 13.0, 14.0, 15.0, 16.5, 18.0, 21.0] {
        for label in ["Refresh", "Open aterm.toml", "Tab Control", "Show tab"] {
            for pad in [24.0_f32, 28.0, 40.0, 56.0] {
                let width = visual_text_width(label, px, face) + pad;
                assert_eq!(
                    elide_text_label(label, width - pad, px, face),
                    label,
                    "{label:?} at {px}px padded by {pad}"
                );
            }
        }
    }
    let px = native_type_px(TypeStep::Secondary).get();
    for label in [
        "Refresh",
        "Open aterm.toml",
        "AI CLIs",
        "Tab Control",
        "Show tab",
        "Cancel",
        "Reset All…",
        "Open Developer Tools…",
    ] {
        for pad in [21.0_f32, 24.0, 26.0, 28.0, 40.0, 50.0, 56.0] {
            let width = native_button_label_width(label) + pad;
            assert_eq!(
                fit_native_button_label(label, width - pad),
                label,
                "{label:?} padded by {pad}"
            );
        }
        let short = native_button_label_width(label) - 1.0;
        assert!(
            elide_text_label(label, short, px, face).ends_with('…'),
            "{label:?} one pixel short"
        );
    }
}

#[test]
fn an_elided_label_never_leaves_a_space_before_its_ellipsis() {
    let px = native_type_px(TypeStep::Body).get();
    let face = crate::widget::TextFace::Ui;
    let width = visual_text_width("Typeset ", px, face) + visual_text_width("…", px, face) + 0.01;
    assert!(
        visual_text_width("Typeset M", px, face) + visual_text_width("…", px, face) > width,
        "the cut lands on the space"
    );
    assert_eq!(
        elide_text_label("Typeset Mirage", width, px, face),
        "Typeset…"
    );
}

#[test]
fn a_cut_block_whose_span_straddles_the_cut_paints_its_last_row() {
    let source = format!("{}**bbbb** tail", "a".repeat(131_066));
    let document = crate::native_markdown::parse(&source);
    let block = &document.blocks[0];
    assert!(
        matches!(
            block,
            crate::native_markdown::MarkdownBlock::Paragraph { .. }
        ),
        "one paragraph: {block:?}"
    );
    let (text, inline) = crate::native_app::bounded_markdown_block_paint(&document, 0, block);
    assert!(
        text.ends_with('…') && text.len() <= 128 * 1024,
        "the block was cut"
    );
    let own = text.len() - '…'.len_utf8();
    assert!(
        inline
            .iter()
            .any(|(span, style)| style.strong && span.start < own),
        "the straddling bold run survives the cut: {inline:?}"
    );
    for (span, _) in &inline {
        assert!(
            span.end <= own && text.is_char_boundary(span.end),
            "a span ends in the block's own text, not the ellipsis: {span:?}"
        );
    }
    let max_columns = 80;
    let rows = crate::native_markdown::wrap_reader_text(&text, max_columns, 0, 131_072, false);
    let spec = MarkdownBlockSpec {
        text: text.clone(),
        kind: MarkdownBlockKind::Paragraph,
        dense: false,
        selectable: true,
        action: None,
        selected: false,
        source: 0..source.len(),
        visual_row: rows.len() - 1,
        total_visual_rows: rows.len(),
        estimated_height: 48.0,
        inline,
    };
    let base = StyledBase {
        x: 0.0,
        y: 0.0,
        bottom: 1_000.0,
        line_h: 18.0,
        size: native_type_px(TypeStep::Body),
        face: crate::widget::TextFace::Ui,
        weight: crate::widget::TextWeight::Regular,
        color: [0, 0, 0],
    };
    let mut prims = Vec::new();
    paint_styled_markdown_rows(
        &mut prims,
        &spec,
        &base,
        max_columns,
        1,
        crate::settings::Roles::from_theme(aterm_render::Theme::default()),
    );
    let painted: String = prims
        .iter()
        .filter_map(|p| match p {
            crate::widget::DrawPrim::Text { s, .. } => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        painted.ends_with('…'),
        "the last row ends in the cut's ellipsis: {painted:?}"
    );
    assert!(
        prims.iter().any(|p| matches!(
            p,
            crate::widget::DrawPrim::Text { s, face: crate::widget::TextFace::UiBold, .. }
                if s.starts_with('b')
        )),
        "the bold run's kept bytes paint bold on the last row: {painted:?}"
    );
}

#[test]
fn a_markdown_paragraph_paints_its_inline_spans() {
    let source = "plain **bold** `code` ~~gone~~ [site](https://x.example)";
    let document = crate::native_markdown::parse(source);
    let crate::native_markdown::MarkdownBlock::Paragraph { text, .. } = &document.blocks[0] else {
        panic!("one paragraph");
    };
    let block = |inline| {
        UiTree::new(UiNode::new(
            "markdown/block/0",
            UiContent::MarkdownBlock(MarkdownBlockSpec {
                text: text.clone(),
                kind: MarkdownBlockKind::Paragraph,
                dense: false,
                selectable: true,
                action: None,
                selected: false,
                source: 0..source.len(),
                visual_row: 0,
                total_visual_rows: 1,
                estimated_height: 48.0,
                inline,
            }),
        ))
        .compile(LogicalRect::new(0.0, 0.0, 900.0, 48.0))
        .unwrap()
    };
    let styled = block(document.block_styles(0, text.len()));
    let prims = styled.tray(aterm_render::Theme::default(), 13.0).prims;
    let texts: Vec<(&str, crate::widget::TextFace, f32)> = prims
        .iter()
        .filter_map(|p| match p {
            crate::widget::DrawPrim::Text { s, face, x, .. } => Some((s.as_str(), *face, *x)),
            _ => None,
        })
        .collect();
    let face_of = |word: &str| {
        texts
            .iter()
            .find(|(s, _, _)| *s == word)
            .map(|(_, face, _)| *face)
            .unwrap_or_else(|| panic!("{word} painted: {texts:?}"))
    };
    assert_eq!(face_of("bold"), crate::widget::TextFace::UiBold);
    assert_eq!(face_of("code"), crate::widget::TextFace::Mono);
    assert_eq!(face_of("plain"), crate::widget::TextFace::Ui);
    assert!(
        texts.windows(2).all(|pair| pair[0].2 < pair[1].2),
        "segments advance left to right: {texts:?}"
    );
    let strokes = prims
        .iter()
        .filter(|p| matches!(p, crate::widget::DrawPrim::Stroke { h, .. } if *h <= 1.0))
        .count();
    assert!(strokes >= 2, "a strike line and a link underline");
}

#[test]
fn clip_introspection_ignores_subpixel_intersection_noise_only() {
    let rect = LogicalRect::new(12.0, 76.0, 172.0, 28.3);
    let rounding_noise = LogicalRect::new(12.001, 76.0, 171.999, 28.299);
    let visible_clip = LogicalRect::new(13.0, 76.0, 171.0, 28.3);

    assert!(!materially_clipped(rect, rect));
    assert!(!materially_clipped(rect, rounding_noise));
    assert!(materially_clipped(rect, visible_clip));
}

#[test]
fn disabled_switch_dims_its_track_and_thumb() {
    let switch_tree = |enabled: bool| {
        UiTree::new(
            UiNode::new(
                "app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column().padding(Insets::all(8.0)))
            .children(vec![
                UiNode::new(
                    "switch",
                    UiContent::Switch(
                        crate::native_ui::Control::new(
                            SwitchSpec {
                                label: "Automatic updates".to_string(),
                                description: None,
                            },
                            ActionId::new("test/switch"),
                        )
                        .value(SemanticValue::Bool(true))
                        .state(ControlState {
                            enabled,
                            ..ControlState::default()
                        }),
                    ),
                )
                .layout(Layout::default().height(Length::Fixed(36.0))),
            ]),
        )
        .compile(LogicalRect::new(0.0, 0.0, 220.0, 52.0))
        .expect("switch fixture compiles")
        .tray(aterm_render::Theme::default(), 13.0)
        .prims
    };
    let feature = |prims: &[crate::widget::DrawPrim]| {
        prims
            .iter()
            .filter_map(|prim| match prim {
                crate::widget::DrawPrim::Capsule { fill, track, .. } => {
                    Some(format!("capsule fill={fill:?} track={track:?}"))
                }
                crate::widget::DrawPrim::Dot { color, cx, cy, .. } => {
                    Some(format!("dot color={color:?} at {cx},{cy}"))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let live = switch_tree(true);
    let inert = switch_tree(false);
    let live_features = feature(&live);
    let inert_features = feature(&inert);
    assert_eq!(live_features.len(), inert_features.len());
    assert_ne!(
        live_features, inert_features,
        "a disabled switch must be visibly dimmed, not a pixel twin of the live control"
    );
    let position = |features: &[String]| {
        features
            .iter()
            .find(|feature| feature.starts_with("dot"))
            .and_then(|feature| feature.split(" at ").nth(1))
            .map(str::to_string)
    };
    assert_eq!(position(&live_features), position(&inert_features));
}

/// A SWITCH IS DRAWN AS macOS DRAWS ONE (init-on-by-default design §4.4, D1): its on track
/// in the accent macOS paints controls in, its off track `#48484A` dark and `#E5E5EA`
/// light, its knob white whether on or off. NEGATIVE CONTROL: another system accent moves
/// the on track, and the chrome's own accent is not what it wears.
#[test]
fn a_switch_wears_the_system_accent_and_a_white_knob() {
    use crate::native_appearance::{SystemAccent, install_control_accent};
    let light = aterm_types::scheme::builtin_names()
        .into_iter()
        .map(|name| {
            aterm_render::Theme::from(&aterm_types::scheme::builtin(name).expect("a builtin"))
        })
        .find(|theme| {
            !crate::native_appearance::surface_is_dark(
                crate::settings::Roles::from_theme(*theme).surface,
            )
        })
        .expect("a light builtin theme");
    let purple = SystemAccent {
        on_dark: [0xA5, 0x50, 0xA7],
        on_light: [0x95, 0x3D, 0x96],
    };
    let live = ControlState::default();
    for (accent, worn) in [
        (purple, (purple.on_dark, purple.on_light)),
        (
            crate::native_appearance::MACOS_SYSTEM_BLUE,
            ([0x0A, 0x84, 0xFF], [0x00, 0x7A, 0xFF]),
        ),
    ] {
        let _ = install_control_accent(Some(accent));
        for (theme, on, off) in [
            (aterm_render::Theme::default(), worn.0, SWITCH_OFF_DARK),
            (light, worn.1, SWITCH_OFF_LIGHT),
        ] {
            let roles = crate::settings::Roles::from_theme(theme);
            let inks = mac_switch_inks(live, &roles);
            assert_eq!(inks.on, crate::widget::rgba(on, 255));
            assert_eq!(inks.off, crate::widget::rgba(off, 255));
            assert_eq!(inks.knob, [0xFF, 0xFF, 0xFF, 0xFF], "the knob is white");
            if roles.accent != on {
                assert_ne!(inks.on[..3], roles.accent, "not the chrome's accent");
            }
            let mut prims = Vec::new();
            for value in [true, false] {
                paint_mac_switch(
                    &mut prims,
                    LogicalRect::new(0.0, 0.0, 58.0, 28.0),
                    live,
                    value,
                    &roles,
                );
            }
            let knobs: Vec<_> = prims
                .iter()
                .filter_map(|prim| match prim {
                    crate::widget::DrawPrim::Dot { color, .. } if color[3] == 255 => Some(*color),
                    _ => None,
                })
                .collect();
            assert_eq!(knobs, vec![[0xFF; 4]; 2], "on and off, the knob is white");
        }
    }
    let _ = install_control_accent(None);
}

/// A DISCLOSURE CHEVRON TURNS ON ITS CENTRE (D1's LOW item): closed it points right, open it
/// points down, and between them it is the same shape turned part of the way, so a motion
/// can swing it. NEGATIVE CONTROL: half a turn points neither right nor down.
#[test]
fn a_disclosure_chevron_points_right_closed_and_down_open() {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a test's tenths of a point"
    )]
    let tip = |turn: f32| {
        let mut prims = Vec::new();
        paint_disclosure_chevron(&mut prims, (100.0, 50.0), turn, [0, 0, 0, 255]);
        assert_eq!(prims.len(), 2, "two arms");
        match (&prims[0], &prims[1]) {
            (
                crate::widget::DrawPrim::Line { x2, y2, .. },
                crate::widget::DrawPrim::Line { x1, y1, .. },
            ) => {
                assert!(
                    (x2 - x1).abs() < 1e-4 && (y2 - y1).abs() < 1e-4,
                    "one point"
                );
                (
                    ((x2 - 100.0) * 10.0).round() as i32,
                    ((y2 - 50.0) * 10.0).round() as i32,
                )
            }
            _ => panic!("a chevron is two lines"),
        }
    };
    assert_eq!(tip(0.0), (20, 0), "closed: its point to the right");
    assert_eq!(tip(1.0), (0, 20), "open: its point below");
    let half = tip(0.5);
    assert!(
        half != tip(0.0) && half != tip(1.0),
        "half a turn is between"
    );
    assert_eq!(tip(-1.0), tip(0.0), "a turn below closed is closed");
    assert_eq!(tip(2.0), tip(1.0), "a turn past open is open");
}

/// AN OPEN LINE STAYS CLEAR, AND A HOVERED ONE IS WASHED IN INK OVER ITS OWN GROUND (init-on-
/// by-default design §4.4): the chevron turned down says open, so an open disclosure lays no
/// band (macOS keeps an expanded row clear); hovered or pressed, it lays the text ink at a
/// fixed alpha, never one surface's colour, so it is as faint on a white card as on an
/// elevated one. NEGATIVE CONTROL: hovered, the same disclosure does lay its wash.
#[test]
fn an_open_disclosure_lays_no_band_and_a_hover_lays_ink_over_its_ground() {
    let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
    let rect = LogicalRect::new(0.0, 0.0, 300.0, 44.0);
    let washes = |state: ControlState| -> Vec<[u8; 4]> {
        let mut spec = ButtonSpec::new("Details");
        spec.visual_label = Some(String::new());
        let control = Control::new(spec, ActionId::new("line/disclose")).state(state);
        let mut prims = Vec::new();
        paint_disclosure_button(&mut prims, rect, &control, &roles);
        prims
            .iter()
            .filter_map(|prim| match prim {
                crate::widget::DrawPrim::Panel { fill, .. } => Some(*fill),
                _ => None,
            })
            .collect()
    };
    let open = ControlState {
        expanded: Some(true),
        ..ControlState::default()
    };
    assert!(washes(open).is_empty(), "an open line lays no band");
    let hover = washes(ControlState {
        hovered: true,
        ..open
    });
    let pressed = washes(ControlState {
        pressed: true,
        ..open
    });
    assert_eq!(hover.len(), 1, "a hovered line lays one wash");
    assert_eq!(hover[0][..3], roles.text_primary, "of the text ink");
    assert_eq!(
        f32::from(hover[0][3]),
        (DISCLOSURE_HOVER_WASH * 255.0).round(),
        "at the hover strength marks are held to"
    );
    assert!(pressed[0][3] > hover[0][3], "a press is stronger");
}

fn assert_fit_failures_match_full_audit(compiled: &CompiledUi) -> Vec<String> {
    let audit = compiled.paint_audit_lines();
    let summary = &audit[0];
    let selected = audit[1..]
        .iter()
        .filter(|line| {
            line.contains("overflow=true")
                || line.contains("clip-truncated=true")
                || (line.starts_with("paint-node ") && line.contains(" clipped=true "))
                || line.starts_with("paint-overlap ")
        })
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let failures = compiled.fit_failures();
    assert_eq!(
        failures
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        selected,
        "fit_failures must be the full audit's failure records"
    );
    assert_eq!(
        failures.len(),
        selected.len(),
        "fit_failures reports each failure record once"
    );
    let clean_summary = summary.contains(" overflow=0 ")
        && summary.contains(" clipped=0 ")
        && summary.contains(" overlaps=0 ");
    assert_eq!(
        failures.is_empty(),
        clean_summary && selected.is_empty(),
        "fit_failures passes exactly when the full audit passes: {summary}"
    );
    failures
}

/// A SWITCH TOO NARROW FOR ITS WORD DRAWS ITS TRACK ALONE ([`switch_word_shown`]): at
/// [`switch_least_width`] it paints its On or Off word whole; narrower than its word (a 96pt
/// column at 1.5× text in Noto Sans), it paints no word to run into its track and audits no
/// text, its state still its value. NEGATIVE CONTROL: the least width paints the word.
#[test]
fn a_switch_too_narrow_for_its_word_draws_its_track_alone() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let compile = |width: f32, on: bool| {
        UiTree::new(
            UiNode::new(
                "app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column())
            .children(vec![
                UiNode::new(
                    "switch",
                    UiContent::Switch(
                        Control::new(
                            SwitchSpec {
                                label: "Automatic updates".to_string(),
                                description: None,
                            },
                            ActionId::new("switch"),
                        )
                        .value(SemanticValue::Bool(on)),
                    ),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fixed(width))
                        .height(Length::Fixed(32.0)),
                ),
            ]),
        )
        .compile(LogicalRect::new(0.0, 0.0, 240.0, 64.0))
        .expect("compiles")
    };
    let least = switch_least_width();
    for on in [false, true] {
        let word = if on { "On" } else { "Off" };
        let painted = |compiled: &CompiledUi| {
            compiled
                .tray(aterm_render::Theme::default(), 14.0)
                .prims
                .into_iter()
                .any(|prim| matches!(prim, crate::widget::DrawPrim::Text { s, .. } if s == word))
        };
        let key = UiKey::new("switch");
        let roomy = compile(least, on);
        assert!(painted(&roomy), "{word} at the least width {least}");
        assert!(
            roomy.fit_failures().is_empty(),
            "{:?}",
            roomy.fit_failures()
        );
        let word_width =
            crate::tray_raster::ui_text_width(word, native_type_px(TypeStep::Secondary).get());
        let narrow = compile(
            SWITCH_WORD_LEAD + SWITCH_WORD_GAP + SWITCH_TRACK_WIDTH + word_width / 2.0,
            on,
        );
        assert!(!painted(&narrow), "no {word} run into the track");
        assert!(narrow.text_fit_audit_line(&key).is_none());
        assert!(
            narrow.fit_failures().is_empty(),
            "{:?}",
            narrow.fit_failures()
        );
        assert_eq!(
            narrow.semantic(&key).map(|node| node.value.clone()),
            Some(SemanticValue::Bool(on))
        );
    }
}

#[test]
fn paint_audit_reports_marks_painted_over_each_other() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let text = |key: &str, words: &str| {
        UiNode::new(key, UiContent::Text(TextSpec::text(words)))
            .layout(Layout::default().height(Length::Fixed(24.0)))
    };
    let switch = |key: &str| {
        UiNode::new(
            key,
            UiContent::Switch(
                Control::new(
                    SwitchSpec {
                        label: key.to_string(),
                        description: None,
                    },
                    ActionId::new(key),
                )
                .value(SemanticValue::Bool(false))
                .style(StyleRef::Quiet),
            ),
        )
        .layout(Layout::default().height(Length::Fixed(24.0)))
    };
    let pictogram = |key: &str| {
        UiNode::new(
            key,
            UiContent::Button(
                Control::new(
                    ButtonSpec::new("Window").visual_icon(ButtonIcon::Window),
                    ActionId::new(key),
                )
                .style(StyleRef::Quiet),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(32.0))
                .height(Length::Fixed(24.0)),
        )
    };
    let row = |key: &str, layout: Layout, children: Vec<UiNode>| {
        UiNode::new(
            key,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(layout.height(Length::Fixed(24.0)))
        .children(children)
    };
    let compile = |children: Vec<UiNode>| {
        UiTree::new(
            UiNode::new(
                "app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column().padding(Insets::all(8.0)).gap(8.0))
            .children(children),
        )
        .compile(LogicalRect::new(0.0, 0.0, 240.0, 120.0))
        .expect("compiles")
    };
    let stacked = compile(vec![
        row(
            "labels",
            Layout::default(),
            vec![text("state", "Live trail"), text("badge", "Shape: Block")],
        ),
        row(
            "thumb",
            Layout::default().width(Length::Fixed(100.0)),
            vec![text("over", "Typing glow"), switch("glow")],
        ),
        row(
            "glyph",
            Layout::default(),
            vec![text("under", "Windows"), pictogram("window")],
        ),
    ]);
    let overlaps = stacked.ink_overlaps();
    assert!(
        overlaps.iter().any(|line| line.contains("a=\"state\"")
            && line.contains("b=\"badge\"")
            && line.contains("a-ink=words:\"Live trail\"")),
        "{overlaps:#?}"
    );
    assert!(
        overlaps.iter().any(|line| line.contains("a=\"over\"")
            && line.contains("b=\"glow\"")
            && line.contains("b-ink=icon")),
        "{overlaps:#?}"
    );
    assert!(
        overlaps.iter().any(|line| line.contains("a=\"under\"")
            && line.contains("b=\"window\"")
            && line.contains("b-ink=icon")),
        "{overlaps:#?}"
    );
    assert_eq!(overlaps.len(), 3, "{overlaps:#?}");
    let audit = stacked.paint_audit_lines();
    assert!(audit[0].contains(" overlaps=3 "), "{}", audit[0]);
    assert_eq!(assert_fit_failures_match_full_audit(&stacked).len(), 3);
    let apart = compile(vec![
        row(
            "labels",
            Layout::row().gap(12.0),
            vec![text("state", "Live trail"), text("badge", "Shape: Block")],
        ),
        row(
            "thumb",
            Layout::row().gap(12.0),
            vec![text("over", "Typing glow"), switch("glow")],
        ),
        row(
            "glyph",
            Layout::row().gap(12.0),
            vec![text("under", "Windows"), pictogram("window")],
        ),
    ]);
    assert!(
        apart.ink_overlaps().is_empty(),
        "{:#?}",
        apart.ink_overlaps()
    );
    assert!(assert_fit_failures_match_full_audit(&apart).is_empty());
}

#[test]
fn paint_audit_reports_exact_text_and_button_overflow() {
    let tree = UiTree::new(
        UiNode::new(
            "app",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column().padding(Insets::all(8.0)).gap(4.0))
        .children(vec![
            UiNode::new(
                "long-text",
                UiContent::Text(TextSpec::text(
                    "A renderer-aware sentence that cannot fit this phone row",
                )),
            )
            .layout(Layout::default().height(Length::Fixed(24.0))),
            button("long-button", "An impossibly verbose toolbar action"),
        ]),
    );
    let compiled = tree
        .clone()
        .compile(LogicalRect::new(0.0, 0.0, 140.0, 100.0))
        .expect("audit fixture compiles");
    let audit = compiled.paint_audit_lines();
    assert!(audit[0].contains("text-nodes=2 overflow=2"));
    let failures = assert_fit_failures_match_full_audit(&compiled);
    assert_eq!(
        failures.len(),
        2,
        "both overflowing nodes fail the cheap verdict"
    );
    let roomy = tree
        .compile(LogicalRect::new(0.0, 0.0, 1200.0, 100.0))
        .expect("roomy audit fixture compiles");
    assert!(
        assert_fit_failures_match_full_audit(&roomy).is_empty(),
        "the same tree with room to fit passes both verdicts"
    );
    assert!(audit.iter().any(|line| {
        line.contains("key=\"long-text\"")
            && line.contains("kind=text")
            && line.contains("overflow=true")
            && line.contains("painted=\"")
            && line.contains('…')
    }));
    assert!(audit.iter().any(|line| {
        line.contains("key=\"long-button\"")
            && line.contains("kind=button")
            && line.contains("overflow=true")
            && line.contains('…')
    }));

    let painted = compiled
        .tray(aterm_render::Theme::default(), 13.0)
        .prims
        .into_iter()
        .filter_map(|primitive| match primitive {
            crate::widget::DrawPrim::Text { s, .. } => Some(s),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(painted.len(), 2);
    assert!(painted.iter().all(|text| text.ends_with('…')));
}

#[test]
fn paint_audit_measures_text_against_the_effective_ancestor_clip() {
    let text = "This fits its authored width but not the clipped viewport";
    let tree = UiTree::new(
        UiNode::new(
            "app",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::row().clipped())
        .children(vec![
            UiNode::new("clipped-text", UiContent::Text(TextSpec::text(text))).layout(
                Layout::default()
                    .width(Length::Fixed(360.0))
                    .height(Length::Fixed(28.0)),
            ),
        ]),
    );
    let compiled = tree
        .compile(LogicalRect::new(0.0, 0.0, 132.0, 60.0))
        .expect("clipped audit fixture compiles");
    let paint = compiled
        .paint
        .iter()
        .find(|node| node.key.as_str() == "clipped-text")
        .unwrap();
    assert_eq!(paint.rect.width, 360.0);
    assert_eq!(paint.clip.width, 132.0);

    let audit = compiled.paint_audit_lines();
    assert!(audit[0].contains("clipped=1"));
    let failures = assert_fit_failures_match_full_audit(&compiled);
    assert!(
        failures
            .iter()
            .any(|line| line.starts_with("paint-node key=\"clipped-text\"")),
        "the ancestor-clipped node fails the cheap verdict: {failures:?}"
    );
    assert!(
        failures
            .iter()
            .any(|line| line.starts_with("paint-text key=\"clipped-text\"")),
        "the clip-truncated text fails the cheap verdict: {failures:?}"
    );
    assert!(audit.iter().any(|line| {
        line.starts_with("paint-node key=\"clipped-text\"")
            && line.contains("rect=0.0,0.0,360.0,28.0")
            && line.contains("clip=0.0,0.0,132.0,28.0")
            && line.contains("visible=true")
            && line.contains("clipped=true")
    }));
    assert!(audit.iter().any(|line| {
        line.starts_with("paint-text key=\"clipped-text\"")
            && line.contains("overflow=true")
            && line.contains("clip-truncated=true")
            && line.contains("available=132.0")
            && line.contains("authored-available=360.0")
    }));
}

pub(crate) fn button(key: &str, label: &str) -> UiNode {
    UiNode::new(
        key,
        UiContent::Button(Control::new(
            ButtonSpec::new(label),
            ActionId::new(format!("activate/{key}")),
        )),
    )
    .layout(Layout::default().height(Length::Fixed(40.0)))
}

fn editor_viewport() -> UiTree {
    use crate::native_editor::{EditorSelectionSpan, EditorViewportLine, EditorViewportProjection};

    UiTree::new(UiNode::new(
        "editor/buffer",
        UiContent::TextViewport(TextViewportSpec {
            label: "notes.md".to_string(),
            document_key: "document:7@12".to_string(),
            selectable: true,
            projection: Some(EditorViewportProjection {
                first_line: 40,
                total_lines: 2_000_000,
                lines: vec![
                    EditorViewportLine {
                        number: 40,
                        source: 9_000..9_016,
                        column_start: 0,
                        text: "let answer = 42;".to_string(),
                        selections: vec![EditorSelectionSpan {
                            bytes: 4..10,
                            continues: false,
                            primary: true,
                        }],
                        carets: vec![(10, true)],
                        syntax: Vec::new(),
                        diagnostics: Vec::new(),
                    },
                    EditorViewportLine {
                        number: 41,
                        source: 9_017..9_017,
                        column_start: 0,
                        text: String::new(),
                        selections: Vec::new(),
                        carets: Vec::new(),
                        syntax: Vec::new(),
                        diagnostics: Vec::new(),
                    },
                ],
            }),
            grid: None,
            grid_reading: None,
            concealed: false,
            preedit: "λ".to_string(),
            preedit_caret: None,
            status: Some("Saved".to_string()),
            semantic_status: Some("Saved".to_string()),
            minibuffer: None,
            minibuffer_caret: None,
            cursor_label: Some("Ln 41, Col 11".to_string()),
            dirty: true,
            saving: false,
            focused: true,
            focus_visible: true,
            action: Some(ActionId::new("editor/focus-buffer")),
        }),
    ))
}

fn one_line_viewport(
    source: std::ops::Range<usize>,
    column_start: usize,
    text: &str,
) -> TextViewportSpec {
    use crate::native_editor::{EditorViewportLine, EditorViewportProjection};
    TextViewportSpec {
        label: "one-line.txt".to_string(),
        document_key: "document:1@1".to_string(),
        selectable: true,
        projection: Some(EditorViewportProjection {
            first_line: 0,
            total_lines: 1,
            lines: vec![EditorViewportLine {
                number: 0,
                source,
                column_start,
                text: text.to_string(),
                selections: Vec::new(),
                carets: vec![(0, true)],
                syntax: Vec::new(),
                diagnostics: Vec::new(),
            }],
        }),
        grid: None,
        grid_reading: None,
        concealed: false,
        preedit: String::new(),
        preedit_caret: None,
        status: None,
        semantic_status: None,
        minibuffer: None,
        minibuffer_caret: None,
        cursor_label: None,
        dirty: false,
        saving: false,
        focused: true,
        focus_visible: true,
        action: Some(ActionId::new("editor/focus-buffer")),
    }
}

/// A grid viewport of `text` (a `.csv`) laid out for `rect`, its cursor moved by `moves`
/// arrow keys (right, then down).
fn grid_viewport(text: &str, rect: LogicalRect, moves: (usize, usize)) -> TextViewportSpec {
    use crate::native_editor::{EditorWorkspace, project_grid};
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let mut store = crate::test_document_store();
    let document = store.open("file:///tmp/grid.csv".to_string(), text.to_string());
    let mut workspace = EditorWorkspace::new();
    let mut view = workspace
        .attach(
            &mut store,
            document,
            crate::document_store::DocumentViewId(1),
        )
        .unwrap();
    workspace
        .set_face(&store, &mut view, Some(amacs::table::Delimiter::Comma))
        .unwrap();
    let arrows = std::iter::repeat_n(NamedKey::ArrowRight, moves.0)
        .chain(std::iter::repeat_n(NamedKey::ArrowDown, moves.1));
    for arrow in arrows {
        workspace
            .handle_input(
                &mut store,
                &mut view,
                amacs::input::EditorInput::Key {
                    key: Key::Named(arrow),
                    mods: Modifiers::empty(),
                    text: None,
                },
            )
            .unwrap();
    }
    let mut spec = one_line_viewport(0..0, 0, "");
    spec.projection = None;
    let geometry = spec.geometry(rect);
    let rows = whole_editor_rows(&geometry);
    let cells = ((rect.right() - geometry.text_x) / geometry.cell_w).floor() as usize;
    let table = amacs::table::Table::scan(text, amacs::table::Delimiter::Comma).unwrap();
    spec.grid = Some(Box::new(project_grid(text, &table, &view, rows, cells)));
    spec
}

/// A press on a grid's `◂` or `▸` mark scrolls its columns that way; a press on a drawn header
/// cell away from them still lands on the cell (the review found the marks did nothing).
#[test]
fn a_press_on_a_grid_mark_scrolls_its_columns() {
    let header = (1..=12)
        .map(|n| format!("header {n:02}"))
        .collect::<Vec<_>>()
        .join(",");
    let row = (1..=12)
        .map(|n| format!("value {n:02}"))
        .collect::<Vec<_>>()
        .join(",");
    let text = format!("{header}\n{row}\n");
    let rect = LogicalRect::new(0.0, 0.0, 520.0, 360.0);
    let spec = grid_viewport(&text, rect, (6, 1));
    let grid = spec.grid.clone().unwrap();
    assert!(
        grid.pinned && grid.hidden_left && grid.hidden_right,
        "{grid:?}"
    );
    let geometry = spec.geometry(rect);
    let (left, right) = editor_grid_marks(rect, &geometry, &grid);
    let header_y = geometry.body_y + geometry.line_h * 0.5;
    let half = geometry.cell_w * 0.5;
    assert_eq!(
        editor_grid_hit(&spec, rect, left.unwrap() + half, header_y),
        Some(GridHit::Scroll(-1))
    );
    assert_eq!(
        editor_grid_hit(&spec, rect, right.unwrap() + half, header_y),
        Some(GridHit::Scroll(1))
    );
    // The pinned column's header, and a cell on a data row under a mark, are cells.
    let pinned_x = geometry.text_x + geometry.cell_w * 2.0;
    assert_eq!(
        editor_grid_hit(&spec, rect, pinned_x, header_y),
        Some(GridHit::Cell(0, 0))
    );
    let first = &grid.shown[1];
    let row_y = header_y + geometry.line_h;
    assert_eq!(
        editor_grid_hit(&spec, rect, left.unwrap() + half, row_y),
        Some(GridHit::Cell(1, first.index))
    );
    // NEGATIVE CONTROL: a grid with nothing hidden has no marks to press.
    let narrow = grid_viewport("a,b\n1,2\n", rect, (0, 0));
    let narrow_grid = narrow.grid.clone().unwrap();
    assert_eq!(
        editor_grid_marks(rect, &narrow.geometry(rect), &narrow_grid),
        (None, None)
    );
}

/// G2: a press on a line of the cell panel lands on the value's boundary nearest it (the
/// second line's, for a press on the second line), and a press on the panel's label or the
/// grid's rows is not the panel's.
#[test]
fn a_press_in_the_grid_panel_lands_on_a_boundary_of_its_value() {
    let text = "name,note\nm1,\"Two reasons: first\nthe phone prompt\"\n";
    let rect = LogicalRect::new(0.0, 0.0, 520.0, 360.0);
    let spec = grid_viewport(text, rect, (1, 1));
    let grid = spec.grid.clone().unwrap();
    assert_eq!(grid.panel_value(), "Two reasons: first\nthe phone prompt");
    let geometry = spec.geometry(rect);
    let (_, panel_slot) = editor_grid_slots(&geometry, &grid);
    let line_y = |line: usize| {
        geometry.body_y + (panel_slot + 1 + line) as f32 * geometry.line_h + geometry.line_h * 0.5
    };
    // Four cells and a little in: the boundary after "the ".
    let x = geometry.text_x + geometry.cell_w * 4.2;
    assert_eq!(
        editor_grid_hit(&spec, rect, x, line_y(1)),
        Some(GridHit::Panel(23))
    );
    assert_eq!(
        editor_grid_hit(&spec, rect, x, line_y(0)),
        Some(GridHit::Panel(4))
    );
    // Past the right half of a cell, the boundary after it.
    let x = geometry.text_x + geometry.cell_w * 4.6;
    assert_eq!(
        editor_grid_hit(&spec, rect, x, line_y(0)),
        Some(GridHit::Panel(5))
    );
    // NEGATIVE CONTROL: the panel's label row and the cursor's row are not the panel.
    let label_y = geometry.body_y + panel_slot as f32 * geometry.line_h + 1.0;
    assert!(!matches!(
        editor_grid_hit(&spec, rect, x, label_y),
        Some(GridHit::Panel(_))
    ));
    let row_y = geometry.body_y + geometry.line_h * 1.5;
    assert!(matches!(
        editor_grid_hit(&spec, rect, x, row_y),
        Some(GridHit::Cell(1, _))
    ));
}

/// G2 review: a press in the near half of a double-width character in the cell panel lands
/// before it, and one in its far half after it, the offset rounded once. *Control:* rounding
/// to a whole cell first put a press 0.7 cells in after `日` (byte 3) and one 2.6 cells in
/// after `本` (byte 6).
#[test]
fn a_press_in_the_grid_panel_rounds_a_wide_character_once() {
    let text = "name,note\nm1,日本\n";
    let rect = LogicalRect::new(0.0, 0.0, 520.0, 360.0);
    let spec = grid_viewport(text, rect, (1, 1));
    let grid = spec.grid.clone().unwrap();
    assert_eq!(grid.panel_value(), "日本");
    let geometry = spec.geometry(rect);
    let (_, panel_slot) = editor_grid_slots(&geometry, &grid);
    let y = geometry.body_y + (panel_slot + 1) as f32 * geometry.line_h + geometry.line_h * 0.5;
    let at =
        |cells: f32| editor_grid_hit(&spec, rect, geometry.text_x + geometry.cell_w * cells, y);
    assert_eq!(at(0.2), Some(GridHit::Panel(0)));
    assert_eq!(at(0.7), Some(GridHit::Panel(0)), "the near half of 日");
    assert_eq!(at(1.3), Some(GridHit::Panel(3)), "the far half of 日");
    assert_eq!(at(2.6), Some(GridHit::Panel(3)), "the near half of 本");
    assert_eq!(at(3.4), Some(GridHit::Panel(6)), "the far half of 本");
}

const GRID_TEXT: &str = concat!(
    "computer,AI login email,services,your correction\n",
    "m1,alice@example.test,\"Claude, GitHub\",\n",
    "m2,bob@example.test,Claude\n",
    "m3,carol@example.test,Claude,\"say \"\"hi\"\"\"\n",
);

/// T9: a grid paints its header band, its cells, its row numbers and its badge; a press on a
/// drawn cell lands on that cell, and a press in the panel or the gutter on none; and a cursor
/// move alone changes the viewport's fingerprint, so the frame repaints.
#[test]
fn t9_the_grid_paints_its_cells_and_a_press_lands_on_the_cell_drawn() {
    let rect = LogicalRect::new(0.0, 0.0, 900.0, 420.0);
    let spec = grid_viewport(GRID_TEXT, rect, (0, 0));
    let grid = spec.grid.clone().unwrap();
    let geometry = spec.geometry(rect);
    assert_eq!(grid.shown.len(), 4, "{grid:?}");

    let texts: Vec<String> = UiTree::new(UiNode::new(
        "editor/buffer",
        UiContent::TextViewport(spec.clone()),
    ))
    .compile(rect)
    .unwrap()
    .tray(aterm_render::Theme::default(), 13.0)
    .prims
    .into_iter()
    .filter_map(|prim| match prim {
        crate::widget::DrawPrim::Text { s, .. } => Some(s),
        _ => None,
    })
    .collect();
    for drawn in [
        "GRID",
        "computer",
        "alice@example.test",
        "Claude, GitHub",
        "say \"hi\"",
        "1",
        "4",
    ] {
        assert!(texts.iter().any(|text| text == drawn), "{drawn}: {texts:?}");
    }
    assert!(!texts.iter().any(|text| text == "EDIT"), "{texts:?}");

    let rows = std::iter::once(&grid.header).chain(&grid.rows);
    for (slot, row) in rows.enumerate() {
        let y = geometry.body_y + (slot as f32 + 0.5) * geometry.line_h;
        for shown in &grid.shown {
            for cells in [shown.x, shown.x + shown.width + 1] {
                let x = geometry.text_x + (cells as f32 + 0.5) * geometry.cell_w;
                assert_eq!(
                    editor_grid_cell_at(&spec, rect, x, y),
                    Some((row.record, shown.index)),
                    "slot {slot} column {} at cell {cells}",
                    shown.index
                );
            }
        }
    }
    let panel_y = geometry.body_y + (grid.rows_layout.data as f32 + 1.5) * geometry.line_h;
    assert_eq!(
        editor_grid_cell_at(&spec, rect, geometry.text_x + 4.0, panel_y),
        None
    );
    assert_eq!(
        editor_grid_cell_at(
            &spec,
            rect,
            geometry.text_x - 4.0,
            geometry.body_y + geometry.line_h * 1.5
        ),
        None
    );

    let moved = grid_viewport(GRID_TEXT, rect, (3, 1));
    assert_eq!(
        moved
            .grid
            .as_ref()
            .map(|grid| (grid.cursor.record, grid.cursor.column)),
        Some((1, 3))
    );
    assert_eq!(
        TextViewportSpec {
            grid: None,
            grid_reading: None,
            concealed: false,
            ..moved.clone()
        },
        TextViewportSpec {
            grid: None,
            grid_reading: None,
            concealed: false,
            ..spec.clone()
        },
        "premise: only the grid differs"
    );
    assert_ne!(
        text_viewport_fingerprint(&moved),
        text_viewport_fingerprint(&spec),
        "a cursor move repaints"
    );
}

/// THE EDITOR PAINTS, COUNTS AND EXPOSES ONLY THE ROWS IT SHOWS WHOLE (audit r20 ED2, ED3): a
/// body four and a half rows tall drew a fifth row the footer cut (a sliver of its glyph tops
/// under the footer at 2x), and the paint audit said six rows were visible, counting the cut
/// row and one wholly under the footer, so its clip check ran on rows no one sees. The body
/// draws its whole rows alone, the audit counts the rows that show whole inside the body and
/// the node's clip, and a clip that cuts one of them is still caught.
#[test]
fn the_editor_paints_and_counts_only_the_rows_it_shows_whole() {
    use crate::native_editor::{EditorViewportLine, EditorViewportProjection};
    let mut spec = one_line_viewport(0..4, 0, "line");
    spec.projection = Some(EditorViewportProjection {
        first_line: 0,
        total_lines: 9,
        lines: (0..9)
            .map(|number| EditorViewportLine {
                number,
                source: number * 5..number * 5 + 4,
                column_start: 0,
                text: "line".to_string(),
                selections: Vec::new(),
                carets: if number == 0 {
                    vec![(0, true)]
                } else {
                    Vec::new()
                },
                syntax: Vec::new(),
                diagnostics: Vec::new(),
            })
            .collect(),
    });
    let probe = spec.geometry(LogicalRect::new(0.0, 0.0, 400.0, 400.0));
    let height = probe.header_h + probe.footer_h + 4.5 * probe.line_h;
    let rect = LogicalRect::new(0.0, 0.0, 400.0, height);
    let geometry = spec.geometry(rect);
    assert_eq!(
        whole_editor_rows(&geometry),
        4,
        "premise: four whole rows and a half"
    );
    let audit = |compiled: &CompiledUi| {
        compiled
            .paint_audit_lines()
            .into_iter()
            .find(|line| line.starts_with("paint-editor "))
            .expect("the editor's audit")
    };

    let compiled = UiTree::new(UiNode::new(
        "editor/buffer",
        UiContent::TextViewport(spec.clone()),
    ))
    .compile(rect)
    .unwrap();
    let line = audit(&compiled);
    assert!(
        line.contains(" painted-rows=4 visible-rows=4 ") && line.contains(" clip-truncated=false "),
        "{line}"
    );
    assert!(
        compiled.sliced_marks().is_empty(),
        "{:#?}",
        compiled.sliced_marks()
    );
    let numbers: Vec<String> = compiled
        .tray(aterm_render::Theme::default(), 13.0)
        .prims
        .into_iter()
        .filter_map(|prim| match prim {
            crate::widget::DrawPrim::Text { s, .. } if s.parse::<usize>().is_ok() => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(
        numbers,
        ["1", "2", "3", "4"],
        "the gutter numbers the rows drawn"
    );

    // Scrolled up under a clip, the first row's top is cut: three of the four show whole.
    let offset = probe.header_h + probe.line_h * 0.5;
    let clipped = UiTree::new(
        UiNode::new(
            "editor/scroll",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::column()
                .height(Length::Fixed(height))
                .scrolled(offset)
                .clipped(),
        )
        .children(vec![
            UiNode::new("editor/buffer", UiContent::TextViewport(spec))
                .layout(Layout::default().height(Length::Fixed(height))),
        ]),
    )
    .compile(rect)
    .unwrap();
    let line = audit(&clipped);
    assert!(
        line.contains(" painted-rows=4 visible-rows=3 ") && line.contains(" clip-truncated=true "),
        "{line}"
    );
}

#[test]
fn editor_fingerprint_tracks_every_paint_only_viewport_state() {
    use crate::native_editor::{EditorDiagnosticSpan, EditorSyntaxClass, EditorSyntaxSpan};

    let fingerprint = |edit: &dyn Fn(&mut TextViewportSpec)| {
        let mut tree = editor_viewport();
        let UiContent::TextViewport(spec) = &mut tree.root.content else {
            panic!("editor viewport fixture")
        };
        edit(spec);
        tree.compile(LogicalRect::new(0.0, 0.0, 640.0, 360.0))
            .unwrap()
            .fingerprint()
    };
    let base = fingerprint(&|_| {});
    type Edit<'a> = &'a dyn Fn(&mut TextViewportSpec);
    let edits: [(&str, Edit); 5] = [
        ("minibuffer", &|spec| {
            spec.minibuffer = Some("Find: semantic".to_string());
        }),
        ("cursor label", &|spec| {
            spec.cursor_label = Some("Ln 99, Col 3".to_string());
        }),
        ("selection", &|spec| {
            spec.projection.as_mut().unwrap().lines[0].selections[0].bytes = 0..3;
        }),
        ("syntax", &|spec| {
            spec.projection.as_mut().unwrap().lines[0]
                .syntax
                .push(EditorSyntaxSpan {
                    bytes: 0..3,
                    class: EditorSyntaxClass::Key,
                });
        }),
        ("diagnostic", &|spec| {
            spec.projection.as_mut().unwrap().lines[0]
                .diagnostics
                .push(EditorDiagnosticSpan {
                    bytes: 4..10,
                    error: true,
                });
        }),
    ];
    for (what, edit) in edits {
        assert_ne!(base, fingerprint(edit), "{what} must move the fingerprint");
    }
}

fn form() -> UiTree {
    UiTree::new(
        UiNode::new(
            "app",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(
            Layout::column()
                .padding(Insets::all(8.0))
                .gap(4.0)
                .clipped(),
        )
        .children(vec![
            UiNode::new("title", UiContent::Text(TextSpec::heading("Settings")))
                .layout(Layout::default().height(Length::Fixed(32.0))),
            button("save", "Save"),
            button("reset", "Reset"),
        ]),
    )
}

#[test]
fn one_control_description_drives_every_observer() {
    let compiled = form()
        .compile(LogicalRect::new(0.0, 0.0, 320.0, 200.0))
        .unwrap();
    compiled.validate_parity().unwrap();

    let key = UiKey::new("save");
    let semantic = compiled.semantic(&key).unwrap();
    assert_eq!(semantic.role, SemanticRole::Button);
    assert_eq!(semantic.label, "Save");
    assert_eq!(semantic.action.as_ref().unwrap().as_str(), "activate/save");
    assert_eq!(compiled.focus_order, [key.clone(), UiKey::new("reset")]);
    assert_eq!(
        compiled.hit_test(10.0, 50.0).map(|hit| hit.action.as_str()),
        Some("activate/save")
    );
    assert!(compiled.controls_lines().iter().any(|line| {
        line.contains("key=\"save\"")
            && line.contains("role=Button")
            && line.contains("action=activate/save")
    }));
}

#[test]
fn editor_ime_caret_tracks_document_composition_and_typed_minibuffer() {
    use crate::widget::DrawPrim;
    let bounds = LogicalRect::new(0.0, 0.0, 760.0, 420.0);
    let compile = |preedit_caret, minibuffer, minibuffer_caret| {
        let mut tree = editor_viewport();
        let UiContent::TextViewport(spec) = &mut tree.root.content else {
            panic!("editor")
        };
        spec.preedit = "e\u{301}日A本B".to_string();
        spec.preedit_caret = preedit_caret;
        spec.minibuffer = minibuffer;
        spec.minibuffer_caret = minibuffer_caret;
        tree.compile(bounds).unwrap()
    };
    let mut previous = None;
    for byte in [0, 3, 7, 11] {
        let compiled = compile(Some(byte), None, None);
        let caret = compiled.ime_caret_rect().unwrap();
        let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
        assert!(
            prims.iter().any(|prim| matches!(prim,
                DrawPrim::Stroke { x, y, w, h, .. }
                    if (*x, *y, *w, *h) == (caret.x, caret.y, caret.width, caret.height)
            )),
            "candidate caret must be painted at preedit byte {byte}"
        );
        if let Some(previous) = previous {
            assert!(caret.x > previous);
        }
        previous = Some(caret.x);
    }
    assert_eq!(
        compile(None, None, None).ime_caret_rect(),
        compile(Some(11), None, None).ime_caret_rect()
    );
    let mut tree = editor_viewport();
    let UiContent::TextViewport(spec) = &mut tree.root.content else {
        panic!("editor")
    };
    spec.preedit = "e\u{301}日👩‍💻\n本".to_string();
    spec.preedit_caret = Some("e\u{301}日👩‍💻\n".len());
    let compiled = tree.compile(bounds).unwrap();
    let caret = compiled.ime_caret_rect().unwrap();
    let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
    let preedit_x = prims
        .iter()
        .find_map(|prim| match prim {
            DrawPrim::Text { x, s, .. } if s == "e\u{301}日👩‍💻↵本" => Some(*x),
            _ => None,
        })
        .unwrap();
    let advance = crate::tray_raster::measure_text(
        "e\u{301}日👩‍💻↵",
        native_type_px(crate::type_scale::TypeStep::Secondary).get(),
        crate::widget::TextWeight::Regular,
    );
    assert!(
        (caret.x - preedit_x - advance).abs() < 0.01,
        "marked caret follows actual fallback-font advances"
    );
    let label = "I-search: λ日本Z".to_string();
    let start = "I-search: λ".len();
    let typed = compile(Some(3), Some(label.clone()), Some(start));
    let footer = typed.ime_caret_rect().unwrap();
    let end = compile(Some(3), Some(label.clone()), Some(label.len()))
        .ime_caret_rect()
        .unwrap();
    assert!(end.x > footer.x);
    let prims = typed.tray(aterm_render::Theme::default(), 13.0).prims;
    assert!(prims.iter().any(|prim| matches!(prim,
        DrawPrim::Stroke { x, y, w, h, .. }
            if (*x, *y, *w, *h) == (footer.x, footer.y, footer.width, footer.height)
    )));
    let hud = compile(Some(3), Some("C-x …".to_string()), None);
    assert_eq!(
        hud.ime_caret_rect(),
        compile(Some(3), None, None).ime_caret_rect()
    );
    assert!(
        hud.ime_caret_rect().unwrap().y < footer.y,
        "shortcut HUD is not a text input"
    );
    let mut focus_failures = Vec::new();
    for prompt in [false, true] {
        let compile_focus = |focused: bool, focus_visible: bool| {
            let mut tree = editor_viewport();
            let UiContent::TextViewport(spec) = &mut tree.root.content else {
                panic!("editor")
            };
            spec.preedit = "e\u{301}日A本B".to_string();
            spec.preedit_caret = Some(3);
            spec.minibuffer = prompt.then(|| label.clone());
            spec.minibuffer_caret = prompt.then_some(start);
            let key = UiKey::new("editor/buffer");
            tree.apply_interaction(focused.then_some(&key), None, None, focus_visible);
            tree.compile(bounds).unwrap()
        };
        let pointer = compile_focus(true, false);
        let keyboard = compile_focus(true, true);
        let expected = if prompt {
            Some(footer)
        } else {
            compile(Some(3), None, None).ime_caret_rect()
        };
        assert_eq!(
            pointer.ime_caret_rect(),
            expected,
            "pointer style preserves exact caret geometry, prompt={prompt}"
        );
        assert_eq!(
            keyboard.ime_caret_rect(),
            expected,
            "keyboard style preserves exact caret geometry, prompt={prompt}"
        );
        let has_ring = |compiled: &CompiledUi| {
            let rect = compiled.paint[0].rect;
            compiled.tray(aterm_render::Theme::default(), 13.0).prims.iter().any(|prim| matches!(prim,
                DrawPrim::Stroke { x, y, w, h, radius, .. }
                    if (*x, *y, *w, *h, *radius) == (rect.x + 0.5, rect.y + 0.5, rect.width - 1.0, rect.height - 1.0, 10.0)
            ))
        };
        if has_ring(&pointer) {
            focus_failures.push(format!(
                "pointer style drew a keyboard focus ring, prompt={prompt}"
            ));
        }
        assert!(
            has_ring(&keyboard),
            "keyboard focus retains its ring, prompt={prompt}"
        );
        if pointer.fingerprint() == keyboard.fingerprint() {
            focus_failures.push(format!(
                "focus-ring visibility did not change the paint fingerprint, prompt={prompt}"
            ));
        }
        let toolbar = compile_focus(false, false);
        if toolbar.ime_caret_rect() != expected {
            focus_failures.push(format!(
                "toolbar navigation lost the text target's exact IME caret, prompt={prompt}"
            ));
        }
        assert!(
            !has_ring(&toolbar),
            "toolbar focus does not outline the viewport"
        );
        if prompt
            && !toolbar
                .tray(aterm_render::Theme::default(), 13.0)
                .prims
                .iter()
                .any(|prim| {
                    matches!(prim,
                        DrawPrim::Stroke { x, y, w, h, .. }
                            if (*x, *y, *w, *h) == (footer.x, footer.y, footer.width, footer.height)
                    )
                })
        {
            focus_failures.push("toolbar navigation lost the painted minibuffer caret".to_string());
        }
    }
    let mut clipped = typed;
    clipped.paint[0].clip.height = footer.y;
    assert!(
        clipped.ime_caret_rect().is_none(),
        "a hidden minibuffer cannot fall back to the document caret"
    );
    let mut unfocused = compile(Some(3), None, None);
    let UiContent::TextViewport(spec) = &mut unfocused.paint[0].content else {
        panic!("editor")
    };
    spec.focused = false;
    if unfocused.ime_caret_rect() != compile(Some(3), None, None).ime_caret_rect() {
        focus_failures
            .push("document text remains the input target during toolbar navigation".to_string());
    }
    let mut unavailable = unfocused.clone();
    let UiContent::TextViewport(spec) = &mut unavailable.paint[0].content else {
        panic!("editor")
    };
    spec.projection = None;
    assert!(
        unavailable.ime_caret_rect().is_none(),
        "no loaded document has no body caret"
    );
    let UiContent::TextViewport(spec) = &mut unfocused.paint[0].content else {
        panic!("editor")
    };
    spec.selectable = false;
    assert!(
        unfocused.ime_caret_rect().is_none(),
        "a nonselectable viewport is not a text target"
    );

    let input = crate::native_text_input::TextInputState::new("query".to_string()).projection();
    let field = UiNode::new(
        "query",
        UiContent::TextField(Control::new(
            TextFieldSpec {
                label: "Query".to_string(),
                placeholder: None,
                secret: false,
                visual_value: None,
                input: Some(input),
                swatch: None,
                swatch_line: false,
                trailing_hint: None,
                description: None,
                end_button: false,
            },
            ActionId::new("set/query"),
        )),
    )
    .layout(Layout::default().height(Length::Fixed(40.0)));
    let mut tree = UiTree::new(
        UiNode::new(
            "root",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column())
        .children(vec![
            field,
            editor_viewport()
                .root
                .layout(Layout::default().height(Length::Fill)),
        ]),
    );
    tree.apply_interaction(Some(&UiKey::new("query")), None, None, true);
    let mut both = tree.compile(bounds).unwrap();
    let mut field_only = both.clone();
    field_only
        .paint
        .retain(|node| matches!(node.content, UiContent::TextField(_)));
    let field_caret = field_only
        .ime_caret_rect()
        .expect("the focused text field has a caret");
    let mut viewport_only = both.clone();
    viewport_only
        .paint
        .retain(|node| matches!(node.content, UiContent::TextViewport(_)));
    if viewport_only.ime_caret_rect().is_none() {
        focus_failures
            .push("the priority control must retain a usable viewport fallback".to_string());
    }
    if both.ime_caret_rect() != Some(field_caret) {
        focus_failures
            .push("the focused text field must precede a viewport text target".to_string());
    }
    both.paint
        .iter_mut()
        .find(|node| node.key.as_str() == "query")
        .unwrap()
        .clip
        .width = 0.0;
    assert!(
        both.ime_caret_rect().is_none(),
        "a hidden focused field cannot lend IME to the viewport"
    );
    assert!(focus_failures.is_empty(), "{}", focus_failures.join("\n"));
}

#[test]
fn editor_selection_caret_and_diagnostic_share_grapheme_cell_geometry() {
    use crate::native_editor::{EditorDiagnosticSpan, EditorSelectionSpan};
    use crate::type_scale::TypeStep;
    use crate::widget::DrawPrim;

    let text = "e\u{301}中👩‍💻".to_string();
    let after_accent = "e\u{301}".len();
    let after_cjk = after_accent + "中".len();
    let after_woman = after_cjk + "👩".len();
    let text_len = text.len();
    let mut spec = one_line_viewport(0..text_len, 0, &text);
    let line = &mut spec.projection.as_mut().expect("one line").lines[0];
    line.selections = vec![
        EditorSelectionSpan {
            bytes: 0..after_accent,
            continues: false,
            primary: true,
        },
        EditorSelectionSpan {
            bytes: after_cjk..after_woman,
            continues: false,
            primary: false,
        },
    ];
    line.carets = vec![(text_len, true)];
    line.diagnostics = vec![
        EditorDiagnosticSpan {
            bytes: after_accent..after_cjk,
            error: true,
        },
        EditorDiagnosticSpan {
            bytes: after_cjk..after_woman,
            error: false,
        },
    ];
    let rect = LogicalRect::new(0.0, 0.0, 760.0, 420.0);
    let geometry = spec.geometry(rect);
    let prims = UiTree::new(UiNode::new("editor/unicode", UiContent::TextViewport(spec)))
        .compile(rect)
        .unwrap()
        .tray(aterm_render::Theme::default(), 13.0)
        .prims;
    let close = |left: f32, right: f32| (left - right).abs() < 0.01;

    assert!(
        prims
            .iter()
            .any(|primitive| matches!(primitive, DrawPrim::Text { s, .. } if s == "e\u{301}中👩‍💻"))
    );
    assert!(
        prims.iter().any(|primitive| {
            matches!(
                primitive,
                DrawPrim::Panel { x, y, w, h, fill: [_, _, _, 86], .. }
                    if close(*x, geometry.text_x)
                        && close(*y, geometry.body_y + 1.0)
                        && close(*w, geometry.cell_w)
                        && close(*h, geometry.line_h - 2.0)
            )
        }),
        "the composed selection occupies exactly one cell"
    );
    assert!(
        prims.iter().any(|primitive| {
            matches!(
                primitive,
                DrawPrim::Panel { x, w, fill: [_, _, _, 54], .. }
                    if close(*x, geometry.text_x + geometry.cell_w * 3.0)
                        && close(*w, geometry.cell_w * 2.0)
            )
        }),
        "a selection ending inside a ZWJ sequence expands to its two-cell grapheme"
    );

    let diagnostic_y = crate::tray_raster::row_baseline(
        geometry.body_y,
        geometry.line_h,
        native_type_px(TypeStep::Secondary).get(),
    ) + 2.0;
    assert!(
        prims.iter().any(|primitive| {
            matches!(
                primitive,
                DrawPrim::Stroke { x, y, w, h, .. }
                    if close(*x, geometry.text_x + geometry.cell_w)
                        && close(*y, diagnostic_y)
                        && close(*w, geometry.cell_w * 2.0)
                        && close(*h, 1.0)
            )
        }),
        "the CJK diagnostic occupies exactly two cells"
    );
    assert!(
        prims.iter().any(|primitive| {
            matches!(
                primitive,
                DrawPrim::Stroke { x, y, w, h, .. }
                    if close(*x, geometry.text_x + geometry.cell_w * 3.0)
                        && close(*y, diagnostic_y)
                        && close(*w, geometry.cell_w * 2.0)
                        && close(*h, 1.0)
            )
        }),
        "a diagnostic ending inside a ZWJ sequence expands to its two-cell grapheme"
    );
    assert!(
        prims.iter().any(|primitive| {
            matches!(
                primitive,
                DrawPrim::Stroke { x, y, w, h, .. }
                    if close(*x, geometry.text_x + geometry.cell_w * 5.0)
                        && close(*y, geometry.body_y + 2.0)
                        && close(*w, 1.0)
                        && close(*h, geometry.line_h - 4.0)
            )
        }),
        "the caret follows the one-cell accent, two-cell CJK, and two-cell ZWJ emoji"
    );
}

#[test]
fn editor_preedit_underlines_use_complete_grapheme_cell_widths() {
    use crate::widget::DrawPrim;

    let rect = LogicalRect::new(0.0, 0.0, 760.0, 420.0);
    let geometry = text_viewport_geometry(rect, 1);
    let close = |left: f32, right: f32| (left - right).abs() < 0.01;
    for (preedit, cells) in [("e\u{301}", 1usize), ("中", 2), ("👩‍💻", 2)] {
        let mut spec = one_line_viewport(0..0, 0, "");
        spec.preedit = preedit.to_string();
        let prims = UiTree::new(UiNode::new("editor/ime", UiContent::TextViewport(spec)))
            .compile(rect)
            .unwrap()
            .tray(aterm_render::Theme::default(), 13.0)
            .prims;
        assert!(
            prims
                .iter()
                .any(|primitive| { matches!(primitive, DrawPrim::Text { s, .. } if s == preedit) }),
            "the complete preedit cluster is painted for {preedit:?}"
        );
        assert!(
            prims.iter().any(|primitive| {
                matches!(
                    primitive,
                    DrawPrim::Stroke { x, y, w, h, .. }
                        if close(*x, geometry.text_x + 2.0)
                            && close(*y, geometry.body_y + geometry.line_h - 2.0)
                            && close(*w, cells as f32 * geometry.cell_w)
                            && close(*h, 1.0)
                )
            }),
            "preedit underline width drifted for {preedit:?}"
        );
    }
}

#[test]
fn editor_display_columns_are_utf8_safe_and_tab_aligned() {
    let column = crate::native_editor::editor_display_column;
    assert_eq!(column("α\tβ", 0, 0), 0);
    assert_eq!(column("α\tβ", "α".len(), 0), 1);
    assert_eq!(column("α\tβ", "α\t".len(), 0), 4);
    assert_eq!(column("α\tβ", "α\t".len() + 1, 0), 4);
    assert_eq!(column("α\tβ", "α\tβ".len(), 0), 5);
    assert_eq!(
        column("\tβ", "\t".len(), 3),
        1,
        "a shifted line keeps the canonical tab-stop phase"
    );

    let composed = "e\u{301}中👩‍💻";
    let after_accent = "e\u{301}".len();
    let after_cjk = after_accent + "中".len();
    assert_eq!(column(composed, 1, 0), 0, "never enter a combining cluster");
    assert_eq!(column(composed, after_accent, 0), 1);
    assert_eq!(column(composed, after_cjk, 0), 3);
    assert_eq!(column(composed, composed.len(), 0), 5);
}

#[test]
fn button_swatch_strip_paints_inside_the_row() {
    use crate::widget::DrawPrim;

    let background = [1, 2, 3];
    let cells: Vec<[u8; 3]> = (0..16_u8).map(|i| [10 + i, 100, 200]).collect();
    let rect = LogicalRect::new(0.0, 0.0, 240.0, 44.0);
    let paint = |spec: ButtonSpec| {
        let compiled = UiTree::new(UiNode::new(
            "theme",
            UiContent::Button(Control::new(spec, ActionId::new("choose"))),
        ))
        .compile(rect)
        .unwrap();
        compiled.validate_parity().unwrap();
        compiled.tray(aterm_render::Theme::default(), 13.0).prims
    };
    let fills = |prims: &[DrawPrim]| {
        prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Panel {
                    x, y, w, h, fill, ..
                } => Some(((*x, *y, *w, *h), *fill)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let prims = paint(ButtonSpec::new("Nord").swatches(background, &cells));
    let panels = fills(&prims);
    for color in std::iter::once(background).chain(cells.iter().copied()) {
        let (bounds, _) = panels
            .iter()
            .find(|(_, fill)| *fill == crate::widget::rgba(color, 255))
            .unwrap_or_else(|| panic!("{color:?} is painted"));
        let (x, y, w, h) = *bounds;
        assert!(
            x >= rect.x && y >= rect.y && x + w <= rect.right() + 0.01 && y + h <= rect.bottom(),
            "{color:?} at {bounds:?} stays inside {rect:?}"
        );
    }
    let plain = fills(&paint(ButtonSpec::new("Nord")));
    assert!(
        plain
            .iter()
            .all(|(_, fill)| *fill != crate::widget::rgba(background, 255))
    );
}

/// A WORD BROKEN TWICE JOINS AS ITS NAME WRITES IT ([`joined_as_named`]): "BUILD
/// INFORMATION" set in Noto Sans (Linux's face) at 2× text in a 164pt About paints "BUILD IN-" /
/// "FORMA-" / "TION", and joins to its name's words as a word broken once does. NEGATIVE
/// CONTROL: a word's own hyphen at a break stays ("a follow-" / "up").
#[test]
fn a_word_broken_twice_joins_as_its_name_writes_it() {
    assert_eq!(
        joined_as_named("BUILD IN-\nFORMA-\nTION", "Build information"),
        "BUILD INFORMATION"
    );
    assert_eq!(
        joined_as_named("Infor-\nmation", "Information"),
        "Information"
    );
    assert_eq!(
        joined_as_named("a follow-\nup", "a follow-up"),
        "a follow-up"
    );
}

#[test]
fn a_buttons_painted_words_are_in_its_name() {
    for (visual, name) in [
        ("Clear", "Clear search"),
        ("More sty\u{2026}", "More styles"),
        (
            "bidi \u{2014} Bi\u{2026}onal text",
            "Insert bidi \u{2014} Bidirectional text",
        ),
        (
            "Show  in finder",
            "Show in Finder: the folder that holds aterm.toml",
        ),
        ("\u{2039}", "Previous config completions"),
        (
            "View",
            "Review in Modified: view the unsaved Settings drafts",
        ),
        ("Open\u{2026} site", "Open Project Site"),
        ("", "A card's surface"),
        // A wrap's break that took the fact separator.
        ("Problems\n5", "Problems \u{b7} 5"),
    ] {
        assert!(
            visible_label_in_name(visual, name),
            "{visual:?} in {name:?}"
        );
    }
    for (visual, name) in [
        ("Open Log", "Open the package log"),
        ("Colors", "Appearance"),
        ("Updates", "Software Update"),
        ("Card\u{2026}dis", "Discard"),
        ("View", "Review in Modified: the unsaved Settings drafts"),
        ("Art", "Restart now"),
        ("Problems\n6", "Problems \u{b7} 5"),
    ] {
        assert!(
            !visible_label_in_name(visual, name),
            "{visual:?} in {name:?}"
        );
    }
    let compile = |visual: &str, name: &str| {
        UiTree::new(UiNode::new(
            "button",
            UiContent::Button(Control::new(
                ButtonSpec::new(name).visual_label(visual),
                ActionId::new("button"),
            )),
        ))
        .compile(LogicalRect::new(0.0, 0.0, 200.0, 36.0))
    };
    assert!(compile("Open Log", "Open Log").is_ok());
    assert_eq!(
        compile("Open Log", "Open the package log").err(),
        Some(CompileError::VisibleLabelNotInName(UiKey::new("button")))
    );
}

#[test]
fn a_segmented_control_is_one_tab_stop() {
    let segment = |key: &str, selected: bool, focused: bool| {
        UiNode::new(
            key,
            UiContent::Button(
                Control::new(ButtonSpec::new(key), ActionId::new(key)).state(ControlState {
                    selected,
                    focused,
                    ..ControlState::default()
                }),
            ),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill))
    };
    let compile = |segmented: bool, focused: Option<&str>| {
        let mut group = UiNode::new("choice", UiContent::Group(GroupSpec::new("Choice")))
            .layout(
                Layout::row()
                    .width(Length::Fill)
                    .height(Length::Fixed(36.0)),
            )
            .children(
                ["a", "b", "c"]
                    .into_iter()
                    .map(|key| segment(key, key == "a", focused == Some(key)))
                    .collect(),
            );
        if segmented {
            group = group.segmented();
        }
        UiTree::new(group)
            .compile(LogicalRect::new(0.0, 0.0, 300.0, 36.0))
            .unwrap()
    };
    let keys = |compiled: &CompiledUi| {
        compiled
            .focus_order
            .iter()
            .map(|key| key.as_str().to_string())
            .collect::<Vec<_>>()
    };
    let rest = compile(true, None);
    assert_eq!(keys(&rest), ["a"], "the selected segment");
    assert_eq!(rest.hits.len(), 3, "every segment is hit");
    assert_eq!(keys(&compile(true, Some("b"))), ["b"], "the focused one");
    assert_eq!(
        rest.segment_step(&UiKey::new("a"), 1),
        Some(Some(UiKey::new("b")))
    );
    assert_eq!(
        rest.segment_step(&UiKey::new("c"), 1),
        Some(None),
        "its end"
    );
    assert_eq!(
        rest.segment_step(&UiKey::new("choice"), 1),
        None,
        "no segment"
    );
    assert_eq!(
        keys(&compile(false, None)),
        ["a", "b", "c"],
        "a plain group"
    );
}

#[test]
fn a_joined_box_draws_no_edge_where_it_runs_on() {
    use crate::widget::DrawPrim;
    let theme = aterm_render::Theme::default();
    let roles = crate::settings::Roles::from_theme(theme);
    let rect = LogicalRect::new(10.0, 100.0, 200.0, 80.0);
    let piece = |joins: Joins| PaintNode {
        key: UiKey::new("card/part-1"),
        rect,
        clip: rect,
        content: UiContent::Group(
            GroupSpec::unlabeled(SemanticRole::Group).style(StyleRef::Secondary),
        ),
        peek: false,
        fade: None,
        cut_edges: CutEdges::NONE,
        joins,
    };
    let drawn = |joins: Joins| {
        let mut prims = Vec::new();
        paint_compiled_node(&mut prims, &piece(joins), roles, theme, 14.0);
        prims
    };
    let joined = drawn(Joins {
        above: true,
        below: Some(6.0),
    });
    assert!(
        matches!(
            joined.first(),
            Some(DrawPrim::ClipPush { y, h, .. })
                if (*y - rect.y).abs() < 0.01 && (*h - (rect.height + 6.0)).abs() < 0.01
        ),
        "{joined:?}"
    );
    let face = joined
        .iter()
        .find_map(|prim| match prim {
            DrawPrim::Panel { y, h, .. } if *h > 40.0 => Some((*y, *y + *h)),
            _ => None,
        })
        .expect("the card's face");
    assert!(face.0 < rect.y - 1.0, "open over it: {face:?}");
    assert!(
        face.1 > rect.bottom() + 6.0 + 1.0,
        "open under it, over the gap: {face:?}"
    );
    assert!(
        matches!(joined.last(), Some(DrawPrim::ClipPop)),
        "{joined:?}"
    );
    let closed = drawn(Joins::default());
    assert!(
        !closed
            .iter()
            .any(|prim| matches!(prim, DrawPrim::ClipPush { .. } | DrawPrim::ClipPop))
    );
    let face = closed
        .iter()
        .find_map(|prim| match prim {
            DrawPrim::Panel { y, h, .. } if *h > 40.0 => Some((*y, *y + *h)),
            _ => None,
        })
        .expect("the card's face");
    assert_eq!(face, (rect.y, rect.bottom()));
}

#[test]
fn a_field_placeholder_fits_its_box() {
    let placeholder = "Automatic (default)";
    assert_eq!(
        fit_native_field_placeholder(placeholder, 200.0, false),
        placeholder
    );
    assert_eq!(
        fit_native_field_placeholder(placeholder, 118.0, false),
        "Automatic"
    );
    let narrow = fit_native_field_placeholder(placeholder, 40.0, false);
    assert!(narrow.ends_with('\u{2026}'), "{narrow:?}");
    let px = native_type_px(TypeStep::Secondary).get();
    let box_of = |field: f32| field - 2.0 * TEXT_FIELD_HORIZONTAL_PADDING;
    assert!(visual_text_width(&narrow, px, crate::widget::TextFace::Ui) <= box_of(40.0) + 0.01);
    let whole = visual_text_width(placeholder, px, crate::widget::TextFace::Ui)
        + 2.0 * TEXT_FIELD_HORIZONTAL_PADDING;
    assert_eq!(
        fit_native_field_placeholder(placeholder, whole, false),
        placeholder
    );
    assert_ne!(
        fit_native_field_placeholder(placeholder, whole, true),
        placeholder
    );
    let field_for = |text: &str| {
        visual_text_width(text, px, crate::widget::TextFace::Ui)
            + 2.0 * TEXT_FIELD_HORIZONTAL_PADDING
    };
    assert_eq!(
        fit_native_field_placeholder("none (HH:MM)", field_for("none") + 1.0, false),
        "none"
    );
    let fleet = "ai-fleet.toml beside aterm.toml";
    assert_eq!(
        fit_native_field_placeholder(fleet, field_for("ai-fleet.toml beside\u{2026}"), false),
        "ai-fleet.toml beside\u{2026}"
    );
    assert_eq!(
        fit_native_field_placeholder(fleet, field_for("ai-fleet.toml beside a"), false),
        "ai-fleet.toml beside\u{2026}"
    );
}

#[test]
fn a_windows_path_gives_way_at_its_folders() {
    let path = r"C:\Users\someone\AppData\Roaming\aterm\ai-fleet.toml";
    let forms: Vec<String> = path_forms(path).into_iter().map(|form| form.0).collect();
    assert!(
        forms.contains(&r"C:\…\aterm\ai-fleet.toml".to_string()),
        "{forms:?}"
    );
    assert_eq!(
        forms.last().map(String::as_str),
        Some(r"…\aterm\ai-fleet.toml")
    );
    assert_eq!(&path[path_name_start(path)..], "ai-fleet.toml");
    let px = native_type_px(TypeStep::Secondary).get();
    let field_for = |text: &str| {
        visual_text_width(text, px, crate::widget::TextFace::Ui)
            + 2.0 * TEXT_FIELD_HORIZONTAL_PADDING
    };
    assert_eq!(
        fit_native_field_placeholder(path, field_for(r"…\ai-fleet.toml") + 1.0, false),
        r"…\ai-fleet.toml"
    );
    let unix = r"/Users//someone/odd\name.toml";
    assert_eq!(&unix[path_name_start(unix)..], r"odd\name.toml");
    assert_eq!(
        path_forms(unix).last().map(|form| form.0.as_str()),
        Some(r"…/someone/odd\name.toml")
    );
}

#[test]
fn an_unfitted_placeholder_paints_elided_and_is_reported() {
    use crate::widget::DrawPrim;
    let control = Control::new(
        TextFieldSpec {
            label: "Selected tab color".to_string(),
            placeholder: Some("Automatic (default)".to_string()),
            secret: false,
            visual_value: None,
            input: None,
            swatch: None,
            swatch_line: false,
            trailing_hint: None,
            description: None,
            end_button: false,
        },
        ActionId::new("set/tab"),
    )
    .value(SemanticValue::Text(String::new()));
    let compiled = UiTree::new(UiNode::new("tab", UiContent::TextField(control)))
        .compile(LogicalRect::new(0.0, 0.0, 70.0, 36.0))
        .unwrap();
    let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
    let painted = prims
        .iter()
        .find_map(|prim| match prim {
            DrawPrim::Text { s, .. } if !s.is_empty() => Some(s.clone()),
            _ => None,
        })
        .expect("the placeholder paints");
    assert!(painted.ends_with('\u{2026}'), "{painted:?}");
    let px = native_type_px(TypeStep::Secondary).get();
    assert!(
        visual_text_width(&painted, px, crate::widget::TextFace::Ui)
            <= 70.0 - 2.0 * TEXT_FIELD_HORIZONTAL_PADDING + 0.01
    );
    let audit = compiled
        .paint_audit_lines()
        .into_iter()
        .find(|line| line.starts_with("paint-text") && line.contains("kind=text-field"))
        .expect("the field is audited");
    assert!(audit.contains("overflow=true"), "{audit}");
    assert!(audit.contains(&format!("painted={painted:?}")), "{audit}");
}

/// A FIELD TOO NARROW FOR ITS HINT'S WORDS MARKS ITSELF WITH A DOT: at 22 columns (a 118pt
/// field) the field holding an unsaved draft dropped "Unsaved" and drew nothing in its
/// place, while the dock counted the draft. A wide field keeps the words; the dot stands
/// inside the field's end, vertically centred, where the words would end.
#[test]
fn a_field_too_narrow_for_its_hint_words_marks_itself() {
    use crate::widget::DrawPrim;
    let paint = |width: f32| {
        let control = Control::new(
            TextFieldSpec {
                label: "Scrollback limit".to_string(),
                placeholder: Some("100000 (default)".to_string()),
                secret: false,
                visual_value: Some("5000".to_string()),
                input: None,
                swatch: None,
                swatch_line: false,
                trailing_hint: Some("Unsaved".to_string()),
                description: None,
                end_button: false,
            },
            ActionId::new("set/scrollback"),
        )
        .value(SemanticValue::Text("5000".to_string()));
        let prims = UiTree::new(UiNode::new("field", UiContent::TextField(control)))
            .compile(LogicalRect::new(0.0, 0.0, width, 46.0))
            .unwrap()
            .tray(aterm_render::Theme::default(), 13.0)
            .prims;
        let words = prims
            .iter()
            .any(|prim| matches!(prim, DrawPrim::Text { s, .. } if s == "Unsaved"));
        let dots = prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Dot { cx, cy, r, .. } => Some((*cx, *cy, *r)),
                _ => None,
            })
            .collect::<Vec<_>>();
        (words, dots)
    };
    let (words, dots) = paint(300.0);
    assert!(
        words && dots.is_empty(),
        "a wide field says Unsaved: {dots:?}"
    );
    let (words, dots) = paint(118.0);
    assert!(!words, "the narrow field has no room for the words");
    assert!(
        matches!(dots[..], [(cx, cy, r)]
            if cx + r <= 118.0 - TEXT_FIELD_HORIZONTAL_PADDING + 0.01
                && cx - r > 118.0 / 2.0
                && (cy - 23.0).abs() < 0.01),
        "one dot at the field's end: {dots:?}"
    );
}

#[test]
fn an_icon_row_s_label_is_audited_in_the_room_its_arrow_leaves() {
    let audit = |arrow: bool| {
        let mut spec = ButtonSpec::new("Cursor Motion")
            .visual_icon(ButtonIcon::Search)
            .visual_label("Cursor Motion");
        if arrow {
            spec = spec.trailing_icon(ButtonIcon::Forward);
        }
        let control = Control::new(spec, ActionId::new("go")).style(StyleRef::Navigation);
        let width = NAVIGATION_LABEL_LEAD
            + NAVIGATION_LABEL_END
            + native_button_label_width("Cursor Motion")
            + 8.0;
        UiTree::new(UiNode::new("go", UiContent::Button(control)))
            .compile(LogicalRect::new(0.0, 0.0, width, 44.0))
            .unwrap()
            .paint_audit_lines()
            .into_iter()
            .find(|line| line.starts_with("paint-text key=\"go\""))
            .expect("the row is audited")
    };
    let with_arrow = audit(true);
    assert!(with_arrow.contains("overflow=true"), "{with_arrow}");
    let without = audit(false);
    assert!(without.contains("overflow=false"), "{without}");
}

#[test]
fn text_field_paints_the_canonical_selection_caret_preedit_and_swatch() {
    use crate::widget::DrawPrim;

    let mut input = crate::native_text_input::TextInputState::new("aβz".to_string());
    input.set_selection(1, 3);
    input.set_preedit("に".to_string(), Some(0..3));
    let projection = input.projection();
    let control = Control::new(
        TextFieldSpec {
            label: "Accent".to_string(),
            placeholder: Some("effective default".to_string()),
            secret: false,
            visual_value: None,
            input: Some(projection.clone()),
            swatch: Some([12, 34, 56]),
            swatch_line: false,
            trailing_hint: None,
            description: None,
            end_button: false,
        },
        ActionId::new("set/accent"),
    )
    .value(SemanticValue::Text(projection.text.clone()))
    .state(ControlState {
        focused: true,
        focus_visible: true,
        ..ControlState::default()
    });
    let compiled = UiTree::new(UiNode::new("accent", UiContent::TextField(control)))
        .compile(LogicalRect::new(0.0, 0.0, 280.0, 40.0))
        .unwrap();
    compiled.validate_parity().unwrap();
    assert_eq!(
        compiled.semantic(&UiKey::new("accent")).unwrap().value,
        SemanticValue::Text("aにz".to_string())
    );
    let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
    let caret = compiled.ime_caret_rect().expect("focused field caret");
    assert!(
        prims.iter().any(|prim| matches!(
            prim,
            DrawPrim::Stroke { x, y, w, h, .. }
                if (*x, *y, *w, *h) == (caret.x, caret.y, caret.width, caret.height)
        )),
        "IME anchor must be the painted composition caret"
    );
    let mut clipped = compiled.clone();
    clipped.paint[0].clip = LogicalRect::new(0.0, 0.0, caret.x, 40.0);
    assert!(
        clipped.ime_caret_rect().is_none(),
        "clipped caret cannot anchor IME"
    );
    assert!(
        prims
            .iter()
            .any(|prim| matches!(prim, DrawPrim::Text { s, .. } if s == "aにz"))
    );
    assert!(prims.iter().any(|prim| matches!(
        prim,
        DrawPrim::Panel {
            fill: [12, 34, 56, 255],
            ..
        }
    )));
}

#[test]
fn text_field_pointer_uses_painted_proportional_grapheme_geometry() {
    let text = "Aé👩‍💻Z".to_string();
    let mut input = crate::native_text_input::TextInputState::new(text.clone());
    input.set_selection(3, 3);
    let control = Control::new(
        TextFieldSpec {
            label: "Color name".to_string(),
            placeholder: None,
            secret: false,
            visual_value: None,
            input: Some(input.projection()),
            swatch: Some([30, 40, 50]),
            swatch_line: false,
            trailing_hint: None,
            description: None,
            end_button: false,
        },
        ActionId::new("set/color"),
    )
    .value(SemanticValue::Text(text.clone()))
    .state(ControlState {
        focused: true,
        ..ControlState::default()
    });
    let compiled = UiTree::new(UiNode::new("color", UiContent::TextField(control)))
        .compile(LogicalRect::new(20.0, 10.0, 300.0, 40.0))
        .unwrap();
    let key = UiKey::new("color");
    let paint = compiled.paint.iter().find(|node| node.key == key).unwrap();
    let UiContent::TextField(control) = &paint.content else {
        unreachable!();
    };
    let px = native_type_px(crate::type_scale::TypeStep::Secondary).get();
    let geometry = text_field_geometry(control, paint.rect, px);
    let cluster_start = text_field_x_for_byte(&text, &geometry, 3, px);
    let cluster_end = text_field_x_for_byte(&text, &geometry, 14, px);

    assert_eq!(
        compiled.text_field_byte_at(&key, cluster_start + (cluster_end - cluster_start) * 0.49),
        Some(3)
    );
    assert_eq!(
        compiled.text_field_byte_at(&key, cluster_start + (cluster_end - cluster_start) * 0.51),
        Some(14)
    );
    assert_eq!(
        compiled.text_field_byte_at(&key, paint.rect.x + 1.0),
        Some(geometry.visible.start),
        "leading padding clamps to the painted projection start"
    );
    assert_eq!(
        compiled.text_field_byte_at(&key, paint.rect.right() - 1.0),
        Some(geometry.visible.end),
        "the trailing swatch is still an end-caret target"
    );
    assert_eq!(compiled.text_field_byte_at(&key, f32::NAN), None);
    assert_eq!(
        compiled.text_field_byte_at(&UiKey::new("stale-field"), geometry.text_x),
        None
    );
}

#[test]
fn text_field_pointer_is_bounded_to_the_clipped_visible_projection() {
    let text = format!("{}e\u{301}👩‍💻末", "long-prefix-".repeat(2_000));
    let input = crate::native_text_input::TextInputState::new(text.clone());
    let control = Control::new(
        TextFieldSpec {
            label: "Fallback fonts".to_string(),
            placeholder: None,
            secret: false,
            visual_value: None,
            input: Some(input.projection()),
            swatch: None,
            swatch_line: false,
            trailing_hint: None,
            description: None,
            end_button: false,
        },
        ActionId::new("set/fonts"),
    )
    .value(SemanticValue::Text(text.clone()))
    .state(ControlState {
        focused: true,
        ..ControlState::default()
    });
    let compiled = UiTree::new(UiNode::new("fonts", UiContent::TextField(control)))
        .compile(LogicalRect::new(0.0, 0.0, 170.0, 40.0))
        .unwrap();
    let key = UiKey::new("fonts");
    let paint = compiled.paint.iter().find(|node| node.key == key).unwrap();
    let UiContent::TextField(control) = &paint.content else {
        unreachable!();
    };
    let geometry = text_field_geometry(
        control,
        paint.rect,
        native_type_px(crate::type_scale::TypeStep::Secondary).get(),
    );
    assert!(
        geometry.visible.start > 0,
        "long prefix is horizontally clipped"
    );
    assert!(geometry.visible.len() <= TEXT_FIELD_GEOMETRY_BYTES);
    let left = compiled.text_field_byte_at(&key, geometry.text_x).unwrap();
    let right = compiled
        .text_field_byte_at(&key, geometry.text_right)
        .unwrap();
    assert_eq!(left, geometry.visible.start);
    assert_eq!(right, geometry.visible.end);
    assert!(text.is_char_boundary(left) && text.is_char_boundary(right));
    assert!(text.grapheme_indices().any(|(byte, _)| byte == left));
    assert!(right == text.len() || text.grapheme_indices().any(|(byte, _)| byte == right));
}

#[test]
fn slider_pointer_value_uses_the_painted_track_and_authored_step() {
    let slider = Control::new(
        SliderSpec {
            description: None,
            track: None,
            label: "Font size".to_string(),
            step: 2.0,
            display_value: "14".to_string(),
        },
        ActionId::new("set/font-size"),
    )
    .value(SemanticValue::Number {
        value: 14.0,
        minimum: 6.0,
        maximum: 32.0,
    });
    let compiled = UiTree::new(UiNode::new("font-size", UiContent::Slider(slider)))
        .compile(LogicalRect::new(20.0, 10.0, 280.0, 40.0))
        .unwrap();
    let key = UiKey::new("font-size");
    let paint = compiled
        .paint
        .iter()
        .find(|paint| paint.key == key)
        .unwrap();
    let geometry = slider_geometry(paint.rect);
    let x = geometry.track_x + (geometry.track_right - geometry.track_x) * 0.5;
    assert_eq!(compiled.slider_value_at(&key, x), Some(20.0));
    assert_eq!(
        compiled.slider_value_at(&key, geometry.track_x - 1.0),
        None,
        "the numeric readout focuses the slider without inventing a value"
    );
    let (readout_x, _) = slider_readout_place("14", &geometry);
    assert!(compiled.slider_readout_at(&key, readout_x + 2.0, paint.rect.y + 20.0));
    assert!(!compiled.slider_readout_at(&key, x, paint.rect.y + 20.0));

    // A narrow slider stacks its number over the track: a click on the number is the
    // number's, though the track runs under it.
    let narrow = Control::new(
        SliderSpec {
            description: None,
            track: None,
            label: "Font size".to_string(),
            step: 2.0,
            display_value: "14".to_string(),
        },
        ActionId::new("set/font-size"),
    )
    .value(SemanticValue::Number {
        value: 14.0,
        minimum: 6.0,
        maximum: 32.0,
    });
    let compiled = UiTree::new(UiNode::new("font-size", UiContent::Slider(narrow)))
        .compile(LogicalRect::new(0.0, 0.0, 90.0, 60.0))
        .unwrap();
    let rect = compiled
        .paint
        .iter()
        .find(|paint| paint.key == key)
        .unwrap()
        .rect;
    let geometry = slider_geometry(rect);
    let (band_top, band_height) = geometry.value_band;
    assert!(band_top + band_height <= geometry.track_y, "stacked");
    let (readout_x, width) = slider_readout_place("14", &geometry);
    let on_number = (readout_x + width / 2.0, band_top + band_height / 2.0);
    assert!(compiled.slider_readout_at(&key, on_number.0, on_number.1));
    assert!(!compiled.slider_readout_at(&key, on_number.0, geometry.track_y));
    let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
    let readout = prims.iter().find_map(|prim| match prim {
        crate::widget::DrawPrim::Text { s, face, .. } if s == "14" => Some(*face),
        _ => None,
    });
    assert_eq!(readout, Some(crate::widget::TextFace::Ui));
}

#[test]
fn fractional_slider_steps_snap_to_the_decimal_the_slider_shows() {
    assert_eq!(snap_slider_value(0.3, 0.0, 1.0, 0.05), 0.3);
    assert_eq!(snap_slider_value(0.7, 0.0, 1.0, 0.05), 0.7);
    assert_eq!(snap_slider_value(0.86, 0.0, 1.0, 0.05), 0.85);
    assert_eq!(snap_slider_value(1.3, 0.1, 3.0, 0.05), 1.3);
    assert_eq!(snap_slider_value(7.0, 0.0, 64.0, 1.0), 7.0);
    assert_eq!(snap_slider_value(2.0, 0.0, 1.0, 0.05), 1.0);
}

#[test]
fn renderer_native_button_icons_emit_no_font_dependent_text() {
    use crate::widget::DrawPrim;

    for icon in [
        ButtonIcon::Back,
        ButtonIcon::Forward,
        ButtonIcon::Copy,
        ButtonIcon::ChevronDown,
        ButtonIcon::Home,
        ButtonIcon::Modified,
        ButtonIcon::Appearance,
        ButtonIcon::Text,
        ButtonIcon::Cursor,
        ButtonIcon::Window,
        ButtonIcon::Keyboard,
        ButtonIcon::Terminal,
        ButtonIcon::Security,
        ButtonIcon::Diagnostics,
        ButtonIcon::Update,
        ButtonIcon::Packages,
        ButtonIcon::Info,
        ButtonIcon::Check,
        ButtonIcon::Image,
        ButtonIcon::Swatch,
        ButtonIcon::Document,
        ButtonIcon::Agents,
        ButtonIcon::Trail,
        ButtonIcon::Sound,
        ButtonIcon::Themes,
        ButtonIcon::Spark,
        ButtonIcon::Tabs,
        ButtonIcon::Trash,
        ButtonIcon::Reset,
        ButtonIcon::Clear,
    ] {
        let mut prims = Vec::new();
        paint_button_icon(
            &mut prims,
            LogicalRect::new(0.0, 0.0, 44.0, 32.0),
            icon,
            [180, 210, 255],
        );
        assert!(!prims.is_empty(), "{icon:?}");
        assert!(
            prims
                .iter()
                .all(|prim| !matches!(prim, DrawPrim::Text { .. })),
            "{icon:?} must remain code-native"
        );
    }
}

#[test]
fn preserved_markdown_source_soft_wraps_at_words_before_hard_chunks() {
    assert_eq!(
        crate::native_markdown::wrap_reader_text("alpha beta gamma", 10, 0, 4, true),
        ["alpha beta", "gamma"]
    );
    assert_eq!(
        crate::native_markdown::wrap_reader_text("supercalifragilistic", 5, 0, 8, true),
        ["super", "calif", "ragil", "istic"]
    );
    assert_eq!(
        crate::native_markdown::wrap_reader_text("  let value = exact;", 14, 0, 4, true),
        ["  let value =", "exact;"]
    );
}

fn latin_markdown_wraps() -> String {
    let corpus = [
        "alpha beta gamma delta epsilon zeta",
        "supercalifragilisticexpialidocious and more",
        "  let value = exact;  // two  spaces\tand a tab",
        "one\ntwo\nthree\n\nfive six",
        "aarch64-apple-darwin ~/.config/aterm/aterm.toml --columns",
        "café naïve “quoted” — dash … end, (paren) [bracket] 50%",
        "a\u{0301}b c\u{0327}d e",
        "abcde fghij",
        "",
        " ",
    ];
    let mut out = String::new();
    for text in corpus {
        for columns in (1..=24).chain([32, 40, 80]) {
            for preserve in [false, true] {
                for lines in [2usize, 131_072] {
                    let rows =
                        crate::native_markdown::wrap_reader_text(text, columns, 0, lines, preserve);
                    out.push_str(&format!(
                        "W {text:?} {columns} {preserve} {lines} => {rows:?}\n"
                    ));
                    if !preserve {
                        out.push_str(&format!(
                            "R {text:?} {columns} {lines} => {:?}\n",
                            wrapped_piece_ranges(text, &rows)
                        ));
                    }
                }
            }
        }
    }
    out
}

#[test]
fn latin_text_wraps_in_the_reader_where_it_always_did() {
    let golden = include_str!("../tests/fixtures/latin-markdown-wraps.txt");
    let now = latin_markdown_wraps();
    let moved: Vec<(&str, &str)> = golden
        .lines()
        .zip(now.lines())
        .filter(|(was, is)| was != is)
        .take(8)
        .collect();
    assert!(moved.is_empty(), "moved wraps (was, is): {moved:#?}");
    assert_eq!(golden.lines().count(), now.lines().count());
}

/// A BLOCK THE READER SHOWS ONLY PART OF KEEPS ITS PANEL OPEN AT THAT
/// EDGE: a code block, table or quote ended at the preview's last whole
/// row (or scrolled into) runs its panel past its rectangle there, cut
/// by it, so its border never closes under the rows shown as if the
/// block ended with them. A block shown whole keeps its panel closed.
#[test]
fn a_block_the_reader_cuts_keeps_its_panel_open_at_the_cut() {
    use crate::widget::DrawPrim;

    let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
    let text = (0..12)
        .map(|line| format!("let line_{line} = {line};"))
        .collect::<Vec<_>>()
        .join("\n");
    for kind in [
        MarkdownBlockKind::Code {
            language: Some("rust".to_string()),
        },
        MarkdownBlockKind::Table,
        MarkdownBlockKind::Quote,
    ] {
        let spec = |visual_row: usize| MarkdownBlockSpec {
            text: text.clone(),
            kind: kind.clone(),
            dense: false,
            selectable: true,
            action: None,
            selected: false,
            source: 0..text.len(),
            estimated_height: 0.0,
            visual_row,
            total_visual_rows: 12,
            inline: Vec::new(),
        };
        let frame = markdown_frame(&spec(0)).expect("a block with text");
        // (first row shown, rows shown, the block's rectangle): whole, ended at the
        // preview's last whole row, scrolled into and shown to its end.
        for (row, rows, open_top, open_bottom) in [
            (0, 12, false, false),
            (0, 4, false, true),
            (5, 7, true, false),
            (5, 3, true, true),
        ] {
            let height = frame.inset_top
                + rows as f32 * frame.line_h
                + if row + rows == 12 {
                    frame.inset_bottom
                } else {
                    0.0
                };
            let rect = LogicalRect::new(10.0, 40.0, 320.0, height);
            let mut prims = Vec::new();
            paint_markdown_block(&mut prims, rect, &spec(row), roles);
            let context = format!("{kind:?} rows {row}+{rows}");
            // The panel: the first fill as wide as the block.
            let (top, bottom) = prims
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Panel { y, w, h, .. } if (*w - rect.width).abs() < 0.01 => {
                        Some((*y, *y + *h))
                    }
                    _ => None,
                })
                .expect("the block's panel");
            assert_eq!(top < rect.y - 0.5, open_top, "{context}: its top edge");
            assert_eq!(
                bottom > rect.bottom() + 0.5,
                open_bottom,
                "{context}: its bottom edge"
            );
            if open_top || open_bottom {
                assert!(
                    matches!(
                        prims.first(),
                        Some(DrawPrim::ClipPush { y, h, .. })
                            if (*y - rect.y).abs() < 0.01 && (*h - rect.height).abs() < 0.01
                    ),
                    "{context}: the panel is cut by the block's rectangle: {prims:?}"
                );
            }
            let pushes = prims
                .iter()
                .filter(|prim| matches!(prim, DrawPrim::ClipPush { .. }))
                .count();
            let pops = prims
                .iter()
                .filter(|prim| matches!(prim, DrawPrim::ClipPop))
                .count();
            assert_eq!(pushes, pops, "{context}");
        }
    }
}

#[test]
fn a_cjk_block_wraps_inside_the_readers_column() {
    const NO_ROW_START: [char; 6] = ['。', '、', '」', 'ー', 'ァ', 'ょ'];
    let text = "設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません。\
                コーヒーブレイク中にアップデートをダウンロードしています、ちょっと待ってください。";
    let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
    for scale in [1.0_f32, 2.0] {
        std::thread::Builder::new()
            .name(format!("CJK reader at {scale}\u{d7}"))
            .stack_size(8 << 20)
            .spawn(move || {
                crate::native_appearance::install_preferences(
                    crate::native_appearance::AppearancePreferences {
                        text_scale: scale,
                        ..crate::native_appearance::current_preferences()
                    },
                );
                for width in (160..=480).step_by(16).map(|width| width as f32) {
                    for (kind, column) in [
                        (MarkdownBlockKind::Paragraph, width),
                        (MarkdownBlockKind::Code { language: None }, width - 24.0),
                    ] {
                        let context = format!("{scale}\u{d7} {width}pt {kind:?}");
                        let spec = MarkdownBlockSpec {
                            text: text.to_string(),
                            kind: kind.clone(),
                            dense: false,
                            selectable: true,
                            action: None,
                            selected: false,
                            source: 0..text.len(),
                            estimated_height: 4_000.0,
                            visual_row: 0,
                            total_visual_rows: 64,
                            inline: Vec::new(),
                        };
                        let mut prims = Vec::new();
                        paint_markdown_block(
                            &mut prims,
                            LogicalRect::new(0.0, 0.0, width, 4_000.0),
                            &spec,
                            roles,
                        );
                        let rows: Vec<String> = prims
                            .iter()
                            .filter_map(|prim| match prim {
                                crate::widget::DrawPrim::Text { s, px, face, .. } => {
                                    let painted = visual_text_width(s, *px, *face);
                                    assert!(
                                        painted <= column + 0.5,
                                        "{context}: {s:?} paints {painted:.1}pt in a \
                                         {column}pt column"
                                    );
                                    Some(s.clone())
                                }
                                _ => None,
                            })
                            .collect();
                        assert!(rows.len() > 1, "{context}: {rows:?}");
                        for row in rows.iter().skip(1) {
                            let first = row.chars().next().unwrap_or(' ');
                            assert!(
                                !NO_ROW_START.contains(&first),
                                "{context}: a row starts with {first:?}: {rows:?}"
                            );
                        }
                        assert_eq!(rows.concat(), text, "{context}: rejoined");
                    }
                }
            })
            .expect("a thread for the scale")
            .join()
            .unwrap_or_else(|_| panic!("at {scale}\u{d7}"));
    }
}

#[test]
fn wrap_skip_returns_exactly_the_unskipped_suffix() {
    const CASES: [(&str, usize, bool); 6] = [
        ("alpha beta gamma delta epsilon zeta", 10, false),
        ("alpha beta gamma delta epsilon zeta", 10, true),
        ("supercalifragilisticexpialidocious", 5, true),
        ("one\ntwo\nthree\nfour\nfive\nsix\nseven", 6, true),
        ("one\ntwo\nthree\nfour\nfive\nsix\nseven", 6, false),
        ("", 8, true),
    ];
    for (text, columns, preserve) in CASES {
        for total in 1..=8usize {
            let full = crate::native_markdown::wrap_reader_text(text, columns, 0, total, preserve);
            for skip in 0..=total + 1 {
                let windowed =
                    crate::native_markdown::wrap_reader_text(text, columns, skip, total, preserve);
                let expected = full.get(skip.min(full.len())..).unwrap_or_default();
                assert_eq!(
                    windowed, expected,
                    "text={text:?} columns={columns} preserve={preserve} total={total} skip={skip}"
                );
            }
        }
    }
}

#[test]
fn code_text_uses_the_terminal_monospace_face() {
    use crate::widget::{DrawPrim as Prim, TextFace};

    let compiled = UiTree::new(UiNode::new(
        "code",
        UiContent::Text(TextSpec {
            text: "$ cargo test".to_string(),
            role: SemanticRole::Text,
            style: StyleRef::Code,
        }),
    ))
    .compile(LogicalRect::new(0.0, 0.0, 240.0, 40.0))
    .unwrap();
    let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
    assert!(prims.iter().any(|primitive| matches!(
        primitive,
        Prim::Text { s, face, .. } if s == "$ cargo test" && *face == TextFace::Mono
    )));
}

#[test]
fn text_viewport_geometry_uses_the_active_mono_raster_advance() {
    use crate::widget::TextWeight;

    let measured = crate::tray_raster::measure_text("M", 13.0, TextWeight::Regular);
    assert!(measured.is_finite() && measured > 0.0);
    let geometry = text_viewport_geometry(LogicalRect::new(0.0, 0.0, 760.0, 420.0), 1);
    assert_eq!(geometry.cell_w, measured);
}

#[test]
fn the_editor_toolbar_plan_wraps_shortens_and_yields_in_order() {
    let toolbar = EditorToolbar {
        commands_hidden: false,
        problem_navigation: true,
        reload: false,
        edit_header: false,
        grid: None,
    };
    let items = editor_toolbar_items(toolbar);
    let gap = 4.0;
    let width = |label: &str| editor_toolbar_button_width(label);
    let long_total: f32 =
        items.iter().map(|item| width(item.long)).sum::<f32>() + gap * (items.len() - 1) as f32;
    let words = |plan: &EditorToolbarPlan| {
        plan.rows
            .iter()
            .map(|row| row.iter().map(|(_, label, _)| *label).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    };
    let plan = plan_editor_toolbar(&items, long_total, gap, EDITOR_TOOLBAR_MAX_ROWS);
    assert_eq!(plan.rows.len(), 1);
    assert_eq!(words(&plan)[0].last(), Some(&"Next Issue"));
    let less = long_total - (width("Next Issue") - width("Next")) + 0.5;
    let plan = plan_editor_toolbar(&items, less, gap, EDITOR_TOOLBAR_MAX_ROWS);
    assert_eq!(plan.rows.len(), 1, "{:?}", words(&plan));
    assert_eq!(words(&plan)[0].last(), Some(&"Next"));
    assert_eq!(words(&plan)[0][6], "Prev Issue", "the last first");
    let plan = plan_editor_toolbar(&items, long_total * 0.55, gap, EDITOR_TOOLBAR_MAX_ROWS);
    assert_eq!(plan.rows.len(), 2);
    let flat: Vec<&str> = words(&plan).concat();
    assert_eq!(
        flat,
        items.iter().map(|item| item.long).collect::<Vec<_>>(),
        "order kept, long words kept"
    );
    let narrow = width("Next Issue") + 1.0;
    let plan = plan_editor_toolbar(&items, narrow, gap, EDITOR_TOOLBAR_MAX_ROWS);
    let flat: Vec<&str> = words(&plan).concat();
    for kept in ["Save", "Undo", "Commands"] {
        assert!(flat.contains(&kept), "{kept}: {flat:?}");
    }
    assert!(flat.contains(&"Prev") || flat.contains(&"Prev Issue"));
    assert!(!flat.contains(&"Redo"), "Redo gives way first: {flat:?}");
    for row in &plan.rows {
        let used: f32 =
            row.iter().map(|(_, _, w)| w).sum::<f32>() + gap * (row.len().saturating_sub(1)) as f32;
        assert!(used <= narrow + 0.01 || row.len() == 1, "{row:?}");
        for (_, label, w) in row {
            assert_eq!(*w, width(label), "never squeezed");
        }
    }
    let three = plan_editor_toolbar(&items, long_total * 0.36, gap, EDITOR_TOOLBAR_MAX_ROWS);
    assert!(three.rows.len() <= EDITOR_TOOLBAR_MAX_ROWS);
    assert_eq!(
        three.rows.concat().len(),
        items.len(),
        "{:?}",
        words(&three)
    );
}

/// THE FILE BEING EDITED SHOWS SEVERAL ROWS AT EVERY STANDARD SIZE AND TEXT SCALE (audit r17,
/// r20 MC4): an 80x24 window at 2x text showed aterm.toml one row at a time under two rows of
/// toolbar and the help line (80x24 at 1.25x, two). The chrome now sheds by measure: the
/// toolbar's rows past its first for the file's minimum rows, the resting help line where the
/// file would show fewer than four beside it. So the file keeps its minimum rows at 1x and
/// four or more at every larger text size (four is all a header, a footer and one toolbar row
/// leave at 80x24 and 2x); the help and the toolbar's rows come back where they fit. An
/// `edit` ticket's header row (Done, Abort) stands for the toolbar there, which gives way
/// whole under it.
#[test]
fn the_file_shows_several_rows_at_every_standard_size_and_text_scale() {
    let windows = [
        (584.0, 348.0, "80x24"),
        (738.0, 418.0, "102x29"),
        (900.0, 600.0, "120x36"),
        (1_200.0, 800.0, "160x48"),
    ];
    // The config editor's toolbars, and an `edit` ticket's with its header row.
    let toolbars = [false, true].into_iter().flat_map(|edit_header| {
        [false, true]
            .into_iter()
            .flat_map(move |problem_navigation| {
                [false, true].map(|reload| EditorToolbar {
                    commands_hidden: false,
                    problem_navigation,
                    reload,
                    edit_header,
                    grid: None,
                })
            })
    });
    for toolbar in toolbars {
        for (width, height, name) in windows {
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            for scale in [0.85, 1.0, 1.25, 1.5, 1.75, 2.0] {
                for palette in [
                    EditorPalette::None,
                    EditorPalette::Help { help_rows: 1 },
                    EditorPalette::Help { help_rows: 2 },
                ] {
                    let editor =
                        editor_text_viewport_rect_at_scale(viewport, palette, toolbar, scale);
                    let rows =
                        whole_editor_rows(&text_viewport_geometry_at_scale(editor, scale, 1));
                    let floor = if scale <= 1.0 {
                        MINIMUM_EDITOR_DOCUMENT_ROWS
                    } else {
                        4
                    };
                    assert!(
                        rows >= floor,
                        "{name} at {scale}x, {palette:?}, {toolbar:?}: {rows} rows of the file"
                    );
                }
            }
        }
    }
    let small = LogicalRect::new(0.0, 0.0, 584.0, 348.0);
    let shell = editor_shell_metrics_at_scale(
        small,
        EditorPalette::Help { help_rows: 1 },
        EditorToolbar::default(),
        2.0,
    );
    assert_eq!(shell.command_rows, 1, "one toolbar row at 80x24 and 2x");
    assert_eq!(
        shell.palette_height, 0.0,
        "the resting help gives way there"
    );
    let ticket = EditorToolbar {
        edit_header: true,
        ..EditorToolbar::default()
    };
    let shell = editor_shell_metrics_at_scale(small, EditorPalette::None, ticket, 2.0);
    assert_eq!(
        (shell.command_rows, shell.command_row_cap),
        (1, 0),
        "an edit ticket's header row alone: its Done saves"
    );
    let roomy = LogicalRect::new(0.0, 0.0, 738.0, 760.0);
    let shell = editor_shell_metrics_at_scale(
        roomy,
        EditorPalette::Help { help_rows: 1 },
        EditorToolbar::default(),
        2.0,
    );
    assert!(shell.palette_height > 0.0, "the help line where it fits");
    assert_eq!(
        shell.command_rows,
        plan_editor_toolbar(
            &editor_toolbar_items(EditorToolbar::default()),
            shell.command_bar_width,
            shell.command_gap,
            EDITOR_TOOLBAR_MAX_ROWS,
        )
        .rows
        .len(),
        "and the whole toolbar"
    );
}

/// AN OPEN COMPLETION LIST PACKS ONLY AS TIGHT AS THE FILE NEEDS (audit r22 SM2): once the
/// toolbar had given way to an open list, the list kept its closed-up rows (the help line's
/// height, 40pt at 2x instead of 48) even where its own rows left the file its floor: at
/// 80x24 and 1.25x, rows of 35pt where 42.5 left aterm.toml the same rows. Now under each
/// chrome the list tightens its padding, then closes its rows up, only where that buys the
/// file a row short of its floor, and the toolbar and a ticket's header row give way only
/// where every packing under them leaves the file short. Over the standard windows, text
/// scales, toolbars, list lengths and help heights, and every height of an 80- and a
/// 102-column window.
#[test]
fn an_open_completion_list_packs_only_as_tight_as_the_file_needs() {
    let mut windows = vec![
        (584.0, 348.0),
        (640.0, 400.0),
        (738.0, 418.0),
        (900.0, 600.0),
        (1_200.0, 800.0),
    ];
    for height in (300..=620).step_by(3) {
        windows.extend([(584.0, height as f32), (738.0, height as f32)]);
    }
    let toolbars = [
        EditorToolbar::default(),
        EditorToolbar {
            commands_hidden: false,
            problem_navigation: true,
            reload: true,
            edit_header: false,
            grid: Some(true),
        },
        EditorToolbar {
            edit_header: true,
            ..EditorToolbar::default()
        },
    ];
    let (mut own_rows_after_the_toolbar, mut closed_up_after_the_toolbar) = (0, 0);
    for scale in [0.85_f32, 1.0, 1.25, 1.5, 1.75, 2.0] {
        let text_scale = editor_text_scale(scale);
        for (width, height) in windows.iter().copied() {
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            let rows = |m: &EditorShellMetrics| {
                whole_editor_rows(&text_viewport_geometry_at_scale(
                    editor_rect_in(viewport, m),
                    text_scale,
                    1,
                ))
            };
            for toolbar in toolbars {
                for candidates in [1, 8] {
                    for help_rows in 1..=3 {
                        let palette = EditorPalette::Rows {
                            candidates,
                            help_rows,
                        };
                        let chosen =
                            editor_shell_metrics_at_scale(viewport, palette, toolbar, scale);
                        let at = |toolbar: EditorToolbar, cap: usize, packing: ListPacking| {
                            editor_shell_layout(
                                viewport, palette, toolbar, text_scale, cap, packing,
                            )
                        };
                        let file = rows(&chosen);
                        let wanted = file.min(EDITOR_DOCUMENT_FLOOR_ROWS);
                        let context = format!(
                            "{width}x{height} at {scale}x, {toolbar:?}, {candidates} listed, \
                             {help_rows} help rows: {file} rows of the file under {chosen:?}"
                        );
                        let header = toolbar.edit_header && chosen.command_rows > 0;
                        let standing = EditorToolbar {
                            edit_header: header,
                            ..toolbar
                        };
                        let looser = |packing: ListPacking| {
                            rows(&at(standing, chosen.command_row_cap, packing))
                        };
                        let own = at(standing, chosen.command_row_cap, ListPacking::Own);
                        if chosen.palette_row_height < own.palette_row_height {
                            let n = looser(ListPacking::TightPadding);
                            assert!(n < wanted, "{context}: its own rows leave it {n}");
                        } else if chosen.palette_padding < own.palette_padding {
                            let n = looser(ListPacking::Own);
                            assert!(n < wanted, "{context}: its own padding leaves it {n}");
                        }
                        let buttons_gone = chosen.command_row_cap == 0 && !toolbar.edit_header;
                        if buttons_gone {
                            let n = rows(&at(toolbar, 1, ListPacking::ClosedUp));
                            assert!(n < wanted, "{context}: one toolbar row leaves it {n}");
                            if chosen.palette_row_height < own.palette_row_height {
                                closed_up_after_the_toolbar += 1;
                            } else {
                                own_rows_after_the_toolbar += 1;
                            }
                        }
                        if toolbar.edit_header && !header {
                            let n = rows(&at(toolbar, 0, ListPacking::ClosedUp));
                            assert!(n < wanted, "{context}: the header row leaves it {n}");
                        }
                    }
                }
            }
        }
    }
    assert!(
        own_rows_after_the_toolbar > 0 && closed_up_after_the_toolbar > 0,
        "the sweep reaches lists at their own height and closed up where the toolbar gave \
         way: {own_rows_after_the_toolbar}, {closed_up_after_the_toolbar}"
    );
    let small = LogicalRect::new(0.0, 0.0, 584.0, 348.0);
    let listed = EditorPalette::Rows {
        candidates: 8,
        help_rows: 1,
    };
    let shell = editor_shell_metrics_at_scale(small, listed, EditorToolbar::default(), 1.25);
    assert_eq!(
        (shell.command_rows, shell.palette_row_height),
        (0, 34.0 * 1.25),
        "80x24 at 1.25x: the toolbar gives way, the rows stand at their own height"
    );
    let shell = editor_shell_metrics_at_scale(small, listed, EditorToolbar::default(), 2.0);
    assert_eq!(
        (shell.command_rows, shell.palette_row_height),
        (0, 40.0),
        "80x24 at 2x: closed up, for the file's rows"
    );
}

#[test]
fn a_full_completion_page_never_costs_the_editor_its_document_rows() {
    for width in [480, 744, 900, 1_440] {
        for height in 335..=1_200 {
            for candidates in [1_usize, 3, 8] {
                let viewport = LogicalRect::new(0.0, 0.0, width as f32, height as f32);
                let editor = editor_text_viewport_rect_at_scale(
                    viewport,
                    candidates,
                    EditorToolbar::default(),
                    1.0,
                );
                let geometry = text_viewport_geometry_at_scale(editor, 1.0, 1);
                let rows = (geometry.body_h / geometry.line_h).floor() as usize;
                assert!(
                    rows >= MINIMUM_EDITOR_DOCUMENT_ROWS,
                    "{width}x{height} with {candidates} candidates paints {rows} rows"
                );
            }
        }
    }

    let large_type = LogicalRect::new(0.0, 0.0, 744.0, 760.0);
    let editor =
        editor_text_viewport_rect_at_scale(large_type, 8_usize, EditorToolbar::default(), 2.0);
    let geometry = text_viewport_geometry_at_scale(editor, 2.0, 1);
    assert!(
        (geometry.body_h / geometry.line_h).floor() as usize >= MINIMUM_EDITOR_DOCUMENT_ROWS,
        "2× landing paints {} px of body over {} px lines",
        geometry.body_h,
        geometry.line_h
    );
}

#[test]
fn text_viewport_pointer_mapping_shares_line_gutter_tab_and_utf8_geometry() {
    use crate::widget::DrawPrim as Prim;

    let spec = one_line_viewport(100..105, 0, "α\tβ");
    let rect = LogicalRect::new(0.0, 0.0, 760.0, 420.0);
    let geometry = spec.geometry(rect);
    let y = geometry.body_y + geometry.line_h * 0.5;

    assert_eq!(text_viewport_byte_at(&spec, rect, 10.0, y), None);
    assert_eq!(
        text_viewport_byte_at(&spec, rect, geometry.text_x, 10.0),
        None
    );
    assert_eq!(text_viewport_byte_at(&spec, rect, rect.right(), y), None);
    assert_eq!(
        text_viewport_byte_at(&spec, rect, geometry.text_x, y),
        Some(100)
    );
    assert_eq!(
        text_viewport_byte_at(&spec, rect, geometry.text_x + 0.6 * geometry.cell_w, y),
        Some(102),
        "right half of α maps after its two-byte UTF-8 scalar"
    );
    assert_eq!(
        text_viewport_byte_at(&spec, rect, geometry.text_x + 1.2 * geometry.cell_w, y),
        Some(102),
        "left side of the tab maps before it"
    );
    assert_eq!(
        text_viewport_byte_at(&spec, rect, geometry.text_x + 3.0 * geometry.cell_w, y),
        Some(103),
        "right side of the four-column tab maps after it"
    );
    assert_eq!(
        text_viewport_byte_at(&spec, rect, geometry.text_x + 4.1 * geometry.cell_w, y),
        Some(103)
    );
    assert_eq!(
        text_viewport_byte_at(&spec, rect, geometry.text_x + 4.8 * geometry.cell_w, y),
        Some(105)
    );
    assert_eq!(
        text_viewport_byte_at(
            &spec,
            rect,
            geometry.text_x,
            geometry.body_y + geometry.line_h + 1.0,
        ),
        None,
        "an unmaterialized row is not addressable"
    );

    let unicode_text = "e\u{301}中👩‍💻".to_string();
    let unicode_start = 300usize;
    let after_accent = unicode_start + "e\u{301}".len();
    let after_cjk = after_accent + "中".len();
    let unicode_end = unicode_start + unicode_text.len();
    let unicode = one_line_viewport(unicode_start..unicode_end, 0, &unicode_text);
    for tenth_cell in 0..=50 {
        let mapped = text_viewport_byte_at(
            &unicode,
            rect,
            geometry.text_x + tenth_cell as f32 * 0.1 * geometry.cell_w,
            y,
        )
        .unwrap();
        assert!(
            [unicode_start, after_accent, after_cjk, unicode_end].contains(&mapped),
            "pointer mapping returned a byte inside a grapheme: {mapped}"
        );
    }
    assert_eq!(
        text_viewport_byte_at(&unicode, rect, geometry.text_x + 0.4 * geometry.cell_w, y,),
        Some(unicode_start)
    );
    assert_eq!(
        text_viewport_byte_at(&unicode, rect, geometry.text_x + 0.6 * geometry.cell_w, y,),
        Some(after_accent)
    );
    assert_eq!(
        text_viewport_byte_at(&unicode, rect, geometry.text_x + 1.9 * geometry.cell_w, y,),
        Some(after_accent)
    );
    assert_eq!(
        text_viewport_byte_at(&unicode, rect, geometry.text_x + 2.1 * geometry.cell_w, y,),
        Some(after_cjk)
    );
    assert_eq!(
        text_viewport_byte_at(&unicode, rect, geometry.text_x + 3.9 * geometry.cell_w, y,),
        Some(after_cjk)
    );
    assert_eq!(
        text_viewport_byte_at(&unicode, rect, geometry.text_x + 4.1 * geometry.cell_w, y,),
        Some(unicode_end)
    );

    let mut shifted = one_line_viewport(200..202, 3, "\tX");
    shifted.projection.as_mut().expect("one line").lines[0].carets = vec![(1, true)];
    assert_eq!(
        text_viewport_byte_at(&shifted, rect, geometry.text_x + 0.4 * geometry.cell_w, y,),
        Some(200)
    );
    assert_eq!(
        text_viewport_byte_at(&shifted, rect, geometry.text_x + 0.6 * geometry.cell_w, y,),
        Some(201),
        "a tab beginning at global column three occupies exactly one painted cell"
    );
    let shifted_tree = UiTree::new(UiNode::new(
        "editor/shifted",
        UiContent::TextViewport(shifted),
    ));
    let shifted_prims = shifted_tree
        .compile(rect)
        .unwrap()
        .tray(aterm_render::Theme::default(), 13.0)
        .prims;
    let expected_caret_x = geometry.text_x + geometry.cell_w;
    assert!(
        shifted_prims.iter().all(|primitive| {
            !matches!(
                primitive,
                Prim::Text { s, .. } if s.contains('\t')
            )
        }),
        "the paint IR must expand tabs before they reach the font rasterizer"
    );
    assert!(
        shifted_prims.iter().any(|primitive| {
            matches!(
                primitive,
                Prim::Text { x, s, .. }
                    if s == "X" && (*x - expected_caret_x).abs() < 0.01
            )
        }),
        "text after a shifted tab starts at the exact canonical cell"
    );
    assert!(
        shifted_prims.iter().any(|primitive| {
            matches!(
                primitive,
                Prim::Stroke { x, y: stroke_y, w, h, .. }
                    if (*x - expected_caret_x).abs() < 0.01
                        && (*stroke_y - (geometry.body_y + 2.0)).abs() < 0.01
                        && (*w - 1.0).abs() < 0.01
                        && (*h - (geometry.line_h - 4.0)).abs() < 0.01
            )
        }),
        "caret paint uses the same shifted tab phase as pointer mapping"
    );
}

#[test]
fn host_focus_pass_updates_every_standard_control_before_compile() {
    let compiled = form()
        .apply_focus(Some(&UiKey::new("reset")))
        .compile(LogicalRect::new(0.0, 0.0, 320.0, 200.0))
        .unwrap();
    assert!(
        compiled
            .semantic(&UiKey::new("reset"))
            .and_then(|node| node.state)
            .is_some_and(|state| state.focused)
    );
    assert!(
        compiled
            .semantic(&UiKey::new("save"))
            .and_then(|node| node.state)
            .is_some_and(|state| !state.focused)
    );
}

#[test]
fn disabled_control_is_semantic_and_painted_but_not_focusable_or_hittable() {
    let disabled =
        Control::new(ButtonSpec::new("Install"), ActionId::new("install")).state(ControlState {
            enabled: false,
            ..ControlState::default()
        });
    let tree = UiTree::new(UiNode::new("install", UiContent::Button(disabled)));
    let compiled = tree
        .compile(LogicalRect::new(0.0, 0.0, 100.0, 40.0))
        .unwrap();
    assert_eq!(compiled.paint.len(), 1);
    assert_eq!(compiled.semantics.len(), 1);
    assert!(compiled.hits.is_empty());
    assert!(compiled.focus_order.is_empty());
}

#[test]
fn default_action_is_the_first_enabled_primary_button_in_authoring_order() {
    let disabled_primary = Control::new(ButtonSpec::new("Stale"), ActionId::new("stale"))
        .state(ControlState {
            enabled: false,
            ..ControlState::default()
        })
        .style(StyleRef::Primary);
    let secondary =
        Control::new(ButtonSpec::new("Check"), ActionId::new("check")).style(StyleRef::Secondary);
    let primary =
        Control::new(ButtonSpec::new("Install"), ActionId::new("install")).style(StyleRef::Primary);
    let row = |key: &str, control| {
        UiNode::new(key, UiContent::Button(control))
            .layout(Layout::default().height(Length::Fixed(40.0)))
    };
    let tree = UiTree::new(
        UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
            .layout(Layout::column())
            .children(vec![
                row("stale", disabled_primary),
                row("check", secondary),
                row("install", primary),
            ]),
    );
    let compiled = tree
        .clone()
        .compile(LogicalRect::new(0.0, 0.0, 200.0, 120.0))
        .unwrap();
    let (key, action) = compiled.default_action.clone().expect("enabled Primary");
    assert_eq!(key, UiKey::new("install"));
    assert_eq!(action, ActionId::new("install"));

    let clipped = tree
        .compile(LogicalRect::new(0.0, 0.0, 200.0, 1.0))
        .unwrap();
    assert!(
        clipped
            .paint
            .iter()
            .all(|node| node.key != UiKey::new("install"))
    );
    assert_eq!(
        clipped.default_action,
        Some((UiKey::new("install"), ActionId::new("install")))
    );

    let no_primary = UiTree::new(
        UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
            .layout(Layout::column())
            .children(vec![button("check", "Check")]),
    );
    let compiled = no_primary
        .compile(LogicalRect::new(0.0, 0.0, 200.0, 120.0))
        .unwrap();
    assert_eq!(compiled.default_action, None);
}

#[test]
fn clipped_geometry_is_shared_by_hit_and_semantics() {
    let tree = UiTree::new(
        UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
            .layout(Layout::column().clipped())
            .children(vec![
                button("wide", "Wide").layout(
                    Layout::default()
                        .width(Length::Fixed(400.0))
                        .height(Length::Fixed(40.0)),
                ),
            ]),
    );
    let compiled = tree
        .compile(LogicalRect::new(10.0, 20.0, 100.0, 60.0))
        .unwrap();
    let semantic = compiled.semantic(&UiKey::new("wide")).unwrap();
    let hit = &compiled.hits[0];
    assert_eq!(semantic.rect, hit.rect);
    assert_eq!(semantic.rect, LogicalRect::new(10.0, 20.0, 100.0, 40.0));
    assert!(compiled.hit_test(109.9, 30.0).is_some());
    assert!(compiled.hit_test(110.0, 30.0).is_none());
}

#[test]
fn ambiguous_trees_fail_closed_before_dispatch() {
    let custom = UiNode::new(
        "custom",
        UiContent::Custom(AuditedCustomNode {
            audit_id: "",
            role: SemanticRole::Button,
            label: "Novel".to_string(),
            paint: None,
            action: Some(ActionId::new("novel")),
            focusable: true,
            focused: false,
            focus_visible: false,
            progress: None,
            motion: None,
        }),
    );
    for (tree, refused) in [
        (
            UiTree::new(
                UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
                    .layout(Layout::column())
                    .children(vec![button("same", "One"), button("same", "Two")]),
            ),
            CompileError::DuplicateKey(UiKey::new("same")),
        ),
        (
            UiTree::new(custom),
            CompileError::CustomWithoutAudit(UiKey::new("custom")),
        ),
        (
            UiTree::new(button("hidden-action", "Hidden action").paint_only()),
            CompileError::PaintOnlyAction(UiKey::new("hidden-action")),
        ),
    ] {
        assert_eq!(
            tree.compile(LogicalRect::new(0.0, 0.0, 200.0, 100.0)),
            Err(refused.clone()),
            "{refused:?}"
        );
    }
}

#[test]
fn paint_only_copy_renders_without_duplicate_semantics() {
    let complete =
        "On. Typing notes are silent while the cursor trail is off; other sounds still play.";
    let compile = |visual: &str| {
        UiTree::new(
            UiNode::new(
                "status",
                UiContent::Group(GroupSpec {
                    label: Some(complete.to_string()),
                    role: SemanticRole::Status,
                    style: StyleRef::Quiet,
                }),
            )
            .children(vec![
                UiNode::new(
                    "status/visual",
                    UiContent::Text(TextSpec {
                        text: visual.to_string(),
                        role: SemanticRole::Status,
                        style: StyleRef::Quiet,
                    }),
                )
                .paint_only(),
            ]),
        )
        .compile(LogicalRect::new(0.0, 0.0, 240.0, 40.0))
        .unwrap()
    };
    let compiled = compile("Typing notes off");
    assert_eq!(compiled.paint.len(), 2);
    assert_eq!(compiled.semantics.len(), 1);
    assert_eq!(
        compiled.semantic(&UiKey::new("status")).unwrap().label,
        complete
    );
    assert!(compiled.semantic(&UiKey::new("status/visual")).is_none());
    compiled.validate_parity().unwrap();

    let other = compile("Visual B");
    assert_eq!(compiled.semantics, other.semantics);
    assert_ne!(compiled.fingerprint(), other.fingerprint());
}

#[test]
fn paint_only_tree_hides_the_whole_face() {
    let compile = |face: UiNode| {
        UiTree::new(
            UiNode::new("row", UiContent::Group(GroupSpec::new("Row"))).children(vec![face]),
        )
        .compile(LogicalRect::new(0.0, 0.0, 240.0, 40.0))
        .unwrap()
    };
    let face = || {
        UiNode::new(
            "row/face",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .children(vec![UiNode::new(
            "row/face/name",
            UiContent::Text(TextSpec {
                text: "Row".to_string(),
                role: SemanticRole::Text,
                style: StyleRef::Primary,
            }),
        )])
    };
    let whole = compile(face().paint_only_tree());
    assert_eq!(whole.paint.len(), 3);
    let keys: Vec<&str> = whole
        .semantics
        .iter()
        .map(|node| node.key.as_str())
        .collect();
    assert_eq!(keys, vec!["row"]);
    let shallow = compile(face().paint_only());
    assert!(shallow.semantic(&UiKey::new("row/face/name")).is_some());
}

#[test]
fn fixed_and_fill_layout_is_deterministic() {
    let tree = UiTree::new(
        UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
            .layout(Layout::row().gap(4.0))
            .children(vec![
                button("nav", "Nav").layout(
                    Layout::default()
                        .width(Length::Fixed(80.0))
                        .height(Length::Fill),
                ),
                button("content", "Content")
                    .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
            ]),
    );
    let compiled = tree
        .compile(LogicalRect::new(0.0, 0.0, 200.0, 100.0))
        .unwrap();
    assert_eq!(
        compiled.semantic(&UiKey::new("nav")).unwrap().rect,
        LogicalRect::new(0.0, 0.0, 80.0, 100.0)
    );
    assert_eq!(
        compiled.semantic(&UiKey::new("content")).unwrap().rect,
        LogicalRect::new(84.0, 0.0, 116.0, 100.0)
    );
}

#[test]
fn color_wheel_pick_and_marker_round_trip() {
    let rect = LogicalRect::new(10.0, 20.0, 240.0, 240.0);
    let geometry = color_wheel_geometry(rect);
    let (_, centre) = color_wheel_pick_at(rect, geometry.cx, geometry.cy).unwrap();
    assert_eq!(centre, 0.0, "disk centre is saturation 0");
    assert_eq!(
        color_wheel_pick_at(rect, geometry.cx + geometry.r + 2.0, geometry.cy),
        None,
        "outside the disk is a miss, not a pick"
    );
    let (top, full) =
        color_wheel_pick_at(rect, geometry.cx, geometry.cy - geometry.r + 0.01).unwrap();
    assert!(top.min(1.0 - top) < 1e-3, "top is hue 0: {top}");
    assert!(full > 0.99, "full radius is saturation 1: {full}");
    let (right, _) =
        color_wheel_pick_at(rect, geometry.cx + geometry.r - 0.01, geometry.cy).unwrap();
    assert!(
        (right - 0.25).abs() < 1e-3,
        "3 o'clock is hue 0.25: {right}"
    );
    for (hue, saturation) in [(0.0889, 1.0), (0.5, 0.4), (0.91, 0.75), (0.33, 0.05)] {
        let (mx, my) = color_wheel_marker_at(rect, hue, saturation);
        let (h, s) = color_wheel_pick_at(rect, mx, my).expect("marker is on the disk");
        assert!(
            (h - hue).abs() < 1e-3 && (s - saturation).abs() < 1e-3,
            "marker round trip drifted for {hue}/{saturation}: {h}/{s}"
        );
    }
}

#[test]
fn an_unfocused_field_marks_what_it_cuts() {
    let theme = aterm_render::Theme::default();
    let roles = crate::settings::Roles::from_theme(theme);
    let rect = LogicalRect::new(0.0, 0.0, 300.0, 36.0);
    let field = |value: &str, focused: bool| {
        let mut control = Control::new(
            TextFieldSpec {
                label: "Wallpaper image".to_string(),
                description: None,
                swatch_line: false,
                trailing_hint: None,
                placeholder: None,
                secret: false,
                visual_value: None,
                input: focused.then(|| {
                    crate::native_text_input::TextInputState::new(value.to_string()).projection()
                }),
                swatch: None,
                end_button: false,
            },
            ActionId::new("settings/set/wallpaper"),
        )
        .value(SemanticValue::Text(value.to_string()));
        control.state.enabled = true;
        control.state.focused = focused;
        PaintNode {
            cut_edges: CutEdges::NONE,
            key: UiKey::new("settings/control/wallpaper"),
            rect,
            clip: rect,
            content: UiContent::TextField(control),
            peek: false,
            fade: None,
            joins: Joins::default(),
        }
    };
    let painted = |node: &PaintNode| {
        let mut prims = Vec::new();
        paint_compiled_node(&mut prims, node, roles, theme, 14.0);
        prims
            .into_iter()
            .find_map(|prim| match prim {
                crate::widget::DrawPrim::Text { s, .. } => Some(s),
                _ => None,
            })
            .unwrap_or_default()
    };
    let path = "/Users//someone/Library/Mobile Documents/Pictures/Backdrops/sunset-over-the-bay.png";
    let node = field(path, false);
    let shown = painted(&node);
    assert!(
        shown.starts_with(TEXT_FIELD_ELLIPSIS) && shown.ends_with("sunset-over-the-bay.png"),
        "a path keeps its end: {shown:?}"
    );
    let UiContent::TextField(control) = &node.content else {
        unreachable!()
    };
    let size = native_type_px(TypeStep::Secondary).get();
    let geometry = text_field_geometry(control, rect, size);
    assert_eq!(geometry.visible.end, path.len());
    let audit = text_fit_audit(&node).expect("audited");
    assert!(!audit.overflow, "deliberate fitting: {audit:?}");
    let words = "an ordinary value far too long to show whole in this field";
    let shown = painted(&field(words, false));
    assert!(
        shown.starts_with("an ordinary") && shown.ends_with(TEXT_FIELD_ELLIPSIS),
        "anything else keeps its start: {shown:?}"
    );
    assert_eq!(painted(&field("/tmp/wall.png", false)), "/tmp/wall.png");
    assert!(!painted(&field(path, true)).contains(TEXT_FIELD_ELLIPSIS));
}

#[test]
fn a_black_wheel_marks_the_kept_hue() {
    use crate::widget::DrawPrim;
    let theme = aterm_render::Theme::default();
    let roles = crate::settings::Roles::from_theme(theme);
    let rect = LogicalRect::new(0.0, 0.0, 200.0, 200.0);
    let paint = |hsv: Option<[f32; 3]>| {
        let node = PaintNode {
            cut_edges: CutEdges::NONE,
            key: UiKey::new("wheel"),
            rect,
            clip: rect,
            content: UiContent::ColorWheel(Control::new(
                ColorWheelSpec {
                    label: "Foreground color wheel".to_string(),
                    hsv,
                },
                ActionId::new("settings/color-pick/foreground"),
            )),
            peek: false,
            fade: None,
            joins: Joins::default(),
        };
        let mut prims = Vec::new();
        paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
        prims
    };
    let prims = paint(Some([0.25, 1.0, 0.0]));
    let disk = prims.iter().find_map(|prim| match prim {
        DrawPrim::HsvDisk { value, .. } => Some(*value),
        _ => None,
    });
    assert_eq!(disk, Some(0.0), "the disk is painted black");
    let (mx, my) = color_wheel_marker_at(rect, 0.25, 1.0);
    let dots: Vec<(f32, f32)> = prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Dot { cx, cy, .. } => Some((*cx, *cy)),
            _ => None,
        })
        .collect();
    assert!(
        !dots.is_empty() && dots.iter().all(|&(x, y)| x == mx && y == my),
        "the marker sits at 3 o'clock on the rim: {dots:?} vs {mx},{my}"
    );
    let geometry = color_wheel_geometry(rect);
    assert!(mx > geometry.cx + geometry.r * 0.9, "off centre: {mx}");
    assert!(
        paint(None)
            .iter()
            .all(|prim| !matches!(prim, DrawPrim::Dot { .. })),
        "no colour, no marker"
    );
}

#[test]
fn focus_on_a_selected_segment_wears_a_ring_and_danger_reads_as_danger() {
    use crate::widget::DrawPrim;
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let theme = aterm_render::Theme::default();
    let roles = crate::settings::Roles::from_theme(theme);
    let rect = LogicalRect::new(10.0, 10.0, 120.0, 44.0);
    let paint = |style: StyleRef, selected: bool, focus_visible: bool| {
        let mut control = Control::new(
            ButtonSpec::new("Auto").visual_label("Auto"),
            ActionId::new("settings/set/bidi"),
        )
        .style(style);
        control.state.selected = selected;
        control.state.focus_visible = focus_visible;
        let node = PaintNode {
            cut_edges: CutEdges::NONE,
            key: UiKey::new("settings/control/bidi"),
            rect,
            clip: rect,
            content: UiContent::Button(control),
            peek: false,
            fade: None,
            joins: Joins::default(),
        };
        let mut prims = Vec::new();
        paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
        prims
    };
    let inner_ring = |prims: &[DrawPrim]| {
        prims.iter().any(|prim| {
            matches!(prim, DrawPrim::Stroke { w, color, .. }
                if *w < rect.width - 2.0 && color[..3] == roles.text_primary)
        })
    };
    assert!(inner_ring(&paint(StyleRef::Navigation, true, true)));
    assert!(!inner_ring(&paint(StyleRef::Navigation, true, false)));
    let danger = paint(StyleRef::Danger, false, false);
    assert!(danger.iter().any(|prim| matches!(prim,
        DrawPrim::Stroke { color, .. } if color[..3] == roles.danger)));
}

#[test]
fn a_muted_selected_segment_paints_no_accent() {
    use crate::widget::DrawPrim;
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let terminal = aterm_render::Theme::default();
    for theme in [
        terminal,
        crate::native_appearance::forced_chrome_theme(terminal, false),
    ] {
        let roles = crate::settings::Roles::from_theme(theme);
        let rect = LogicalRect::new(10.0, 10.0, 88.0, 36.0);
        let paint = |style: StyleRef, selected: bool, muted: bool, focus_visible: bool| {
            let mut control = Control::new(
                ButtonSpec::new("Default autopilot: Cruise").visual_label("Cruise"),
                ActionId::new("settings/set/harness.autopilot"),
            )
            .style(style);
            control.state.selected = selected;
            control.state.muted = muted;
            control.state.focus_visible = focus_visible;
            let node = PaintNode {
                cut_edges: CutEdges::NONE,
                key: UiKey::new("settings/control/harness.autopilot"),
                rect,
                clip: rect,
                content: UiContent::Button(control),
                peek: false,
                fade: None,
                joins: Joins::default(),
            };
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
            prims
        };
        let wash = mix_rgb(
            roles.elevated,
            roles.accent,
            crate::native_appearance::ACCENT_WASH_SELECTED,
        );
        let accented =
            |rgb: &[u8]| rgb == roles.accent || rgb == roles.accent_ink || rgb == wash.as_slice();
        let panels = |prims: &[DrawPrim]| -> Vec<[u8; 3]> {
            prims
                .iter()
                .filter_map(|prim| match prim {
                    DrawPrim::Panel { fill, .. } => Some([fill[0], fill[1], fill[2]]),
                    _ => None,
                })
                .collect()
        };
        let strokes = |prims: &[DrawPrim]| -> Vec<[u8; 3]> {
            prims
                .iter()
                .filter_map(|prim| match prim {
                    DrawPrim::Stroke { color, .. } => Some([color[0], color[1], color[2]]),
                    _ => None,
                })
                .collect()
        };
        let ink = |prims: &[DrawPrim]| -> Vec<[u8; 3]> {
            prims
                .iter()
                .filter_map(|prim| match prim {
                    DrawPrim::Text { color, .. } => Some([color[0], color[1], color[2]]),
                    _ => None,
                })
                .collect()
        };
        let live = paint(StyleRef::Navigation, true, false, false);
        assert!(
            panels(&live).iter().any(|rgb| accented(rgb)),
            "{theme:?}: the live choice washes in the accent"
        );
        assert!(strokes(&live).iter().any(|rgb| accented(rgb)));
        assert_eq!(ink(&live), [roles.accent_ink]);
        let idle = paint(StyleRef::Navigation, true, true, false);
        assert!(
            panels(&idle)
                .iter()
                .chain(&strokes(&idle))
                .chain(&ink(&idle))
                .all(|rgb| !accented(rgb)),
            "{theme:?}: no accent on an idle choice: {idle:?}"
        );
        assert!(
            !panels(&idle).is_empty() && !strokes(&idle).is_empty(),
            "{theme:?}: the idle choice still has its cell and rim"
        );
        let unchosen = paint(StyleRef::Navigation, false, true, false);
        assert!(
            panels(&unchosen).is_empty() && strokes(&unchosen).is_empty(),
            "{theme:?}: and still reads chosen beside an idle unchosen one, flush"
        );
        assert_eq!(ink(&idle), [quiet_text(&roles)], "{theme:?}");
        assert!(
            strokes(&paint(StyleRef::Navigation, true, true, true)).contains(&roles.accent),
            "{theme:?}: an idle choice under focus wears the accent ring"
        );
        assert_eq!(
            ink(&paint(StyleRef::Setting, false, false, false)),
            [roles.text_primary]
        );
        assert_eq!(
            ink(&paint(StyleRef::Setting, false, true, false)),
            [quiet_text(&roles)]
        );
    }
}

#[test]
fn resting_controls_rise_above_their_card_on_windows_light_themes() {
    use crate::native_appearance::{contrast_ratio, surface_is_dark};
    use crate::widget::DrawPrim;
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let openness = |c: [u8; 3]| contrast_ratio(c, [0, 0, 0]);
    let mut light = 0;
    for name in aterm_types::scheme::builtin_names() {
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in scheme");
        let theme = aterm_render::Theme::from(&scheme);
        let roles = crate::settings::Roles::from_theme(theme);
        let rect = LogicalRect::new(10.0, 10.0, 160.0, 36.0);
        for style in [StyleRef::Secondary, StyleRef::Setting] {
            let control = Control::new(
                ButtonSpec::new("More styles").visual_label("More styles"),
                ActionId::new("settings/open/cursor.styles"),
            )
            .style(style);
            let node = PaintNode {
                cut_edges: CutEdges::NONE,
                key: UiKey::new("settings/control/cursor.styles"),
                rect,
                clip: rect,
                content: UiContent::Button(control),
                peek: false,
                fade: None,
                joins: Joins::default(),
            };
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
            let fill = prims
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Panel { fill, .. } => Some([fill[0], fill[1], fill[2]]),
                    _ => None,
                })
                .expect("a resting control paints its fill");
            assert_eq!(fill, control_rest_fill(&roles), "{name} {style:?}");
            let card = roles.elevated;
            if surface_is_dark(roles.surface) {
                assert!(openness(fill) > openness(card), "{name}: dark rest");
            } else {
                light += 1;
                assert_eq!(
                    openness(fill) > openness(card),
                    cfg!(windows),
                    "{name} {style:?}: rest fill {fill:?} on card {card:?}"
                );
            }
        }
    }
    assert!(light >= 8, "light built-ins measured: {light}");
}

#[test]
fn quiet_text_buttons_are_standard_buttons_on_windows() {
    use crate::widget::DrawPrim;
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let rimmed = |prims: &[DrawPrim]| {
        prims
            .iter()
            .any(|prim| matches!(prim, DrawPrim::Stroke { width, .. } if *width == 1.0))
    };
    for name in ["Default Light", "Default"] {
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in scheme");
        let theme = aterm_render::Theme::from(&scheme);
        let roles = crate::settings::Roles::from_theme(theme);
        let rect = LogicalRect::new(10.0, 10.0, 120.0, 36.0);
        let paint = |spec: ButtonSpec, style: StyleRef, state: ControlState| {
            let control = Control::new(spec, ActionId::new("settings/route/themes"))
                .state(state)
                .style(style);
            let node = PaintNode {
                cut_edges: CutEdges::NONE,
                key: UiKey::new("settings/theme/browse"),
                rect,
                clip: rect,
                content: UiContent::Button(control),
                peek: false,
                fade: None,
                joins: Joins::default(),
            };
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
            prims
        };
        let words = || ButtonSpec::new("Browse themes").visual_label("Browse themes\u{2026}");
        for state in [
            ControlState::default(),
            ControlState {
                hovered: true,
                ..ControlState::default()
            },
            ControlState {
                enabled: false,
                ..ControlState::default()
            },
        ] {
            let quiet = paint(words(), StyleRef::Quiet, state);
            let secondary = paint(words(), StyleRef::Secondary, state);
            assert_eq!(rimmed(&quiet), cfg!(windows), "{name} {state:?}");
            assert_eq!(
                format!("{quiet:?}") == format!("{secondary:?}"),
                cfg!(windows),
                "{name} {state:?}: a quiet text button is a Secondary one on Windows only"
            );
        }
        let rest = paint(words(), StyleRef::Quiet, ControlState::default());
        let fill = rest
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Panel { fill, .. } => Some(*fill),
                _ => None,
            })
            .expect("a resting quiet button paints its fill");
        let rest_fill = control_rest_fill(&roles);
        if cfg!(windows) {
            assert_eq!(
                fill,
                [rest_fill[0], rest_fill[1], rest_fill[2], 255],
                "{name}"
            );
        } else {
            assert_eq!(fill[3], 72, "{name}: the quiet wash elsewhere");
        }
        let icon = paint(
            ButtonSpec::new("Browse themes").visual_icon(ButtonIcon::Forward),
            StyleRef::Quiet,
            ControlState::default(),
        );
        assert!(!rimmed(&icon), "{name}: a quiet icon button has no rim");
    }
}

#[test]
fn a_lists_edges_cut_no_line_of_words_or_pictogram_through() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let list = |fades: bool| {
        let icon = UiNode::new(
            "list/top",
            UiContent::Button(
                Control::new(
                    ButtonSpec::new("Window").visual_icon(ButtonIcon::Window),
                    ActionId::new("list/top"),
                )
                .style(StyleRef::Quiet),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(32.0))
                .height(Length::Fixed(24.0)),
        );
        let filler = UiNode::new(
            "list/filler",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::default().height(Length::Fixed(52.0)));
        let words = UiNode::new(
            "list/bottom",
            UiContent::Text(TextSpec::text("COLOR PREVIEW")),
        )
        .layout(Layout::default().height(Length::Fixed(24.0)));
        let room = if fades { 60.0 } else { 72.0 };
        UiTree::new(
            UiNode::new(
                "app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column().padding(Insets::all(8.0)))
            .children(vec![
                UiNode::new(
                    "list",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::List)),
                )
                .layout(
                    Layout::column()
                        .height(Length::Fixed(72.0))
                        .scrolled(12.0)
                        .clipped(),
                )
                .room(room, fades)
                .children(vec![icon, filler, words]),
            ]),
        )
        .compile(LogicalRect::new(0.0, 0.0, 240.0, 120.0))
        .expect("compiles")
    };
    let words_painted = |compiled: &CompiledUi| {
        compiled.tray(aterm_render::Theme::default(), 14.0).prims.iter().any(|prim| {
            matches!(prim, crate::widget::DrawPrim::Text { s, .. } if s.contains("PREVIEW"))
        })
    };
    let hard = list(false);
    let node = |key: &str| {
        hard.paint
            .iter()
            .find(|node| node.key.as_str() == key)
            .unwrap_or_else(|| panic!("{key} is painted"))
            .cut_edges
    };
    assert!(
        node("list/bottom").bottom.is_some(),
        "{:?}",
        node("list/bottom")
    );
    assert!(!words_painted(&hard), "the bottom edge slices no words");
    assert!(hard.sliced_marks().is_empty(), "{:#?}", hard.sliced_marks());
    let top = UiKey::new("list/top");
    assert!(
        hard.paint.iter().all(|node| node.key != top),
        "the cut button paints nothing"
    );
    assert!(hard.semantic(&top).is_none() && hard.hits.iter().all(|hit| hit.key != top));
    assert_eq!(hard.list_picture_at(24.0, 12.0), Some(&top));
    let fading = list(true);
    assert!(words_painted(&fading), "the strip fades the line out");
}

/// A RESTING CONTROL'S RIM, WHERE IT IS THE CONTROL'S ONLY BOUNDARY, HOLDS 3:1 (WCAG
/// 1.4.11's non-text floor) AGAINST THE GROUND IT STANDS ON, IN THE RASTERIZED PIXELS: every
/// built-in theme, on a card and on the page, every button style and every boxed control (a
/// field, a slider, a switch). A rest fill a few percent off the ground draws no boundary, so
/// the rim is all a person sees of the control's extent. A disabled control is exempt, as
/// WCAG exempts it, and stays soft.
#[test]
fn a_resting_controls_rim_holds_3_to_1_against_its_ground() {
    use crate::native_appearance::contrast_ratio;
    use crate::widget::DrawPrim;
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let rect = LogicalRect::new(12.0, 10.0, 160.0, 36.0);
    let styles = [
        StyleRef::Plain,
        StyleRef::Hero,
        StyleRef::Primary,
        StyleRef::Secondary,
        StyleRef::Quiet,
        StyleRef::Accent,
        StyleRef::Success,
        StyleRef::Danger,
        StyleRef::DangerCompact,
        StyleRef::Navigation,
        StyleRef::Setting,
        StyleRef::Code,
        StyleRef::Chip,
        StyleRef::Note,
    ];
    let action = || ActionId::new("test/control");
    let mut controls: Vec<(String, UiContent)> = Vec::new();
    for style in styles {
        for (selected, muted) in [(false, false), (true, false), (false, true), (true, true)] {
            controls.push((
                format!(
                    "{style:?} button{}{}",
                    if selected { " (selected)" } else { "" },
                    if muted { " (idle)" } else { "" }
                ),
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new("Copy All").visual_label("Copy All"),
                        action(),
                    )
                    .state(ControlState {
                        selected,
                        muted,
                        ..ControlState::default()
                    })
                    .style(style),
                ),
            ));
        }
    }
    controls.push((
        "text field".to_string(),
        UiContent::TextField(Control::new(
            TextFieldSpec {
                label: "Query".to_string(),
                placeholder: Some("Search".to_string()),
                secret: false,
                visual_value: None,
                input: None,
                swatch: None,
                swatch_line: false,
                trailing_hint: None,
                description: None,
                end_button: false,
            },
            action(),
        )),
    ));
    controls.push((
        "slider".to_string(),
        UiContent::Slider(
            Control::new(
                SliderSpec {
                    description: None,
                    track: None,
                    label: "Font size".to_string(),
                    step: 1.0,
                    display_value: "14".to_string(),
                },
                action(),
            )
            .value(SemanticValue::Number {
                value: 14.0,
                minimum: 6.0,
                maximum: 32.0,
            }),
        ),
    ));
    controls.push((
        "switch".to_string(),
        UiContent::Switch(
            Control::new(
                SwitchSpec {
                    label: "Automatic updates".to_string(),
                    description: None,
                },
                action(),
            )
            .value(SemanticValue::Bool(false)),
        ),
    ));
    let mut rims = 0;
    let mut short = Vec::new();
    for name in aterm_types::scheme::builtin_names() {
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in scheme");
        let theme = aterm_render::Theme::from(&scheme);
        let roles = crate::settings::Roles::from_theme(theme);
        for (what, content) in &controls {
            let node = PaintNode {
                cut_edges: CutEdges::NONE,
                key: UiKey::new("test/control"),
                rect,
                clip: rect,
                content: content.clone(),
                peek: false,
                fade: None,
                joins: Joins::default(),
            };
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
            let rimmed = prims.iter().any(|prim| {
                matches!(prim, DrawPrim::Stroke { x, y, .. }
                    if (*x - rect.x - 0.5).abs() < 0.01 && (*y - rect.y - 0.5).abs() < 0.01)
            });
            if !rimmed {
                continue;
            }
            for (on, ground) in [("card", roles.elevated), ("page", roles.surface)] {
                let mut drawn = vec![DrawPrim::Panel {
                    x: 0.0,
                    y: 0.0,
                    w: 200.0,
                    h: 60.0,
                    radius: 0.0,
                    fill: [ground[0], ground[1], ground[2], 255],
                }];
                drawn.extend(prims.iter().cloned());
                let (pixels, width, _) =
                    crate::tray_raster::rasterize_tray(&drawn, 200, 60, 1.0, [0, 0, 0, 255]);
                let at = |x: f32, y: f32| -> [u8; 3] {
                    let index = ((y as u32 * width + x as u32) * 4) as usize;
                    [pixels[index], pixels[index + 1], pixels[index + 2]]
                };
                let mid = rect.y + rect.height / 2.0;
                let beside = at(rect.x - 2.0, mid);
                assert_eq!(beside, ground, "{name} {what} on the {on}: the ground");
                let edge = [
                    at(rect.x, mid),
                    at(rect.right() - 1.0, mid),
                    at(rect.x + rect.width / 2.0, rect.y),
                    at(rect.x + rect.width / 2.0, rect.bottom() - 1.0),
                ]
                .into_iter()
                .map(|rim| contrast_ratio(rim, ground))
                .fold(f32::INFINITY, f32::min);
                rims += 1;
                if edge < 3.0 {
                    short.push(format!("{name} {what} on the {on}: {edge:.2}:1"));
                }
            }
        }
    }
    assert!(rims > 100, "rims measured: {rims}");
    assert!(
        short.is_empty(),
        "{} rims under 3:1:\n{}",
        short.len(),
        short.join("\n")
    );
}
