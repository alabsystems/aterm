// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Themes' LAYOUT: the chooser, the demo and the strip, in one of
//! two shapes.
//!
//! * **Beside** (a content column of 640pt or more): the chooser panel on
//!   the left (256pt wide from 900pt, 232pt below), the demo filling the
//!   rest, and the strip under the demo. The chooser's filter and footer are
//!   pinned inside the panel; only its list scrolls.
//! * **Stacked** (narrower): the demo pinned at the top (38% of the body,
//!   at least 180pt and at most ten rows, six on a short window, fewer where
//!   six would leave the chooser no whole stop), the strip under it, and the
//!   chooser panel below, whose list — footer included — is the one thing
//!   that scrolls.
//!
//! The strip is always 40pt and always reserved, so nothing moves when a
//! trial starts or ends. Every right edge of a scrolling column stops
//! [`THUMB_GUTTER`] short of the column's edge.

use super::customize::{caption_height, sheet_blocks};
use super::*;
use crate::settings_preview::theme_card::{
    SLAB_HEIGHT, SLAB_NARROW_WIDTH, SLAB_WIDTH, contrast_grade,
};

/// The chooser stands beside the demo from this content width, at its wide
/// width from [`CHOOSER_WIDE_FROM`].
const BESIDE_FROM: f32 = 640.0;
const CHOOSER_WIDE_FROM: f32 = 900.0;
const CHOOSER_WIDE: f32 = 256.0;
const CHOOSER_MEDIUM: f32 = 232.0;
/// A chooser narrower than this draws the narrow slab and the short footer.
const CHOOSER_NARROW_BELOW: f32 = 240.0;
/// The gap between the chooser and the demo, and the panel's inset.
const COLUMN_GAP: f32 = 16.0;
const PANEL_INSET: f32 = 8.0;
/// The gap between the demo and the strip.
const DEMO_STRIP_GAP: f32 = 8.0;
/// The ✓ column, so the names line up whether or not a row wears it.
const CHECK_WIDTH: f32 = 16.0;
/// A Yours row's Open and Trash.
const ICON_BUTTON: f32 = 20.0;
/// The room after a row's ✓ column: the ✓ is narrower than its column, so
/// the column's own slack is the rest of the row's end margin.
const ROW_END: f32 = 4.0;

/// A chooser row's height.
fn row_height() -> f32 {
    32.0_f32.max(28.0 * settings_text_scale())
}

/// The strip's height: two lines, or one line beside its buttons.
pub(super) fn strip_height() -> f32 {
    40.0_f32.max(36.0 * settings_text_scale())
}

/// The filter field's (and the Dark | Light segment's) height.
fn field_height() -> f32 {
    32.0_f32.max(28.0 * settings_text_scale())
}

/// The footer: the hairline, then its two rows, each after the panel's
/// 6pt gap.
fn footer_height() -> f32 {
    1.0 + 2.0 * (PANEL_GAP + field_height())
}

/// The gap between the panel's stacked parts.
const PANEL_GAP: f32 = 6.0;
/// The room over a section caption or the Community disclosure where it
/// follows rows.
const SECTION_ABOVE: f32 = 8.0;
/// Where a row's words start, from the row's own edge: names, captions and
/// the footer's label share it with the labels of the buttons in the same
/// column (the Community disclosure, Import…, Themes folder).
const TEXT_INSET: f32 = crate::native_ui::BUTTON_LABEL_INSET;

/// The page's two shapes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    /// The chooser `chooser` wide beside the demo.
    Beside { chooser: f32 },
    /// The demo and strip over the chooser.
    Stacked,
}

impl Shape {
    fn for_width(page_width: f32) -> Self {
        let scale = settings_text_scale().clamp(1.0, 1.25);
        if page_width >= CHOOSER_WIDE_FROM * scale {
            Self::Beside {
                chooser: CHOOSER_WIDE * scale,
            }
        } else if page_width >= BESIDE_FROM * scale {
            Self::Beside {
                chooser: CHOOSER_MEDIUM * scale,
            }
        } else {
            Self::Stacked
        }
    }
}

/// THE PAGE: the heading, then the chooser beside the demo and strip, or
/// the demo and strip over the chooser.
pub(crate) fn themes_page(
    state: &SettingsViewState,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> Vec<UiNode> {
    let viewport = cx.viewport;
    let gap = settings_page_gap(state, width, viewport);
    let maximum = PageLayout::Stage.maximum(width);
    let page_width = settings_page_content_width(viewport.width, width, maximum);
    let (mut out, mut used) = pinned_heading(width, viewport, gap, SHOWCASE_TITLE, "");
    let body = settings_list_body_height(state, width, viewport);
    let (spec, frame) = demo_spec_and_frame(
        state,
        cx.animation_phase_ms,
        cx.motion,
        cx.terminal_font_px,
        cx.terminal_scale,
        cx.semantic_font.as_ref(),
    )
    .unzip();
    let strip = strip_height();
    let shape = Shape::for_width(page_width);
    match shape {
        Shape::Beside { chooser } => {
            let demo_floor = frame.as_ref().map_or(180.0, |frame| frame.demo_height(6));
            // The title gives way where it alone keeps the demo from its
            // six rows.
            let needs = demo_floor + DEMO_STRIP_GAP + strip;
            if body - used < needs && body >= needs {
                out.clear();
                used = 0.0;
            }
            let room = (body - used).max(0.0);
            let demo_width = (page_width - chooser - COLUMN_GAP).max(0.0);
            let demo_height = (room - DEMO_STRIP_GAP - strip).max(0.0);
            let left = if state.showcase.sheet_open {
                sheet_panel(state, chooser, room)
            } else {
                chooser_panel(state, chooser, room, false)
            };
            let right = UiNode::new(
                "settings/showcase/stage-column",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::column()
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .gap(DEMO_STRIP_GAP),
            )
            .children(
                spec.map(|spec| demo_node(spec, demo_height))
                    .into_iter()
                    .chain([strip_node(state, demo_width)])
                    .collect(),
            );
            out.push(
                UiNode::new(
                    "settings/showcase/layout",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::row().height(Length::Fixed(room)).gap(COLUMN_GAP))
                .children(vec![left, right]),
            );
        }
        Shape::Stacked => {
            let list_floor = panel_floor();
            let demo_for = |room: f32| {
                let ten = frame.as_ref().map_or(240.0, |frame| frame.demo_height(10));
                (room * 0.38).max(180.0).min(ten)
            };
            let six = frame.as_ref().map_or(180.0, |frame| frame.demo_height(6));
            let mut room = (body - used).max(0.0);
            let rest = |room: f32, demo: f32| room - demo - 2.0 * DEMO_STRIP_GAP - strip;
            let mut demo_height = demo_for(room);
            // Fewer than three rows would show: the demo drops to six rows,
            // then the title gives way.
            if rest(room, demo_height) < list_floor {
                demo_height = six.min(demo_height);
            }
            if rest(room, demo_height) < list_floor && used > 0.0 {
                out.clear();
                room = body;
            }
            // Where six rows still leave the chooser short of its filter and
            // one whole stop of its list, the demo gives up whole rows, down
            // to one: a stop taller than the list's room is drawn cut. (At
            // 584×348 the six rows of Windows' 1.1 line left the list 19pt,
            // and the Community disclosure showed 25 of its 32pt on the one
            // screen that showed it.)
            let least = chooser_least(state);
            if rest(room, demo_height) < least
                && let Some(frame) = frame.as_ref()
            {
                demo_height = (1..6)
                    .rev()
                    .map(|rows| frame.demo_height(rows))
                    .find(|&height| rest(room, height) >= least)
                    .unwrap_or_else(|| frame.demo_height(1))
                    .min(demo_height);
            }
            let panel_height = rest(room, demo_height).max(0.0);
            if let Some(spec) = spec {
                out.push(demo_node(spec, demo_height));
            }
            out.push(strip_node(state, page_width));
            out.push(if state.showcase.sheet_open {
                sheet_panel(state, page_width, panel_height)
            } else {
                chooser_panel(state, page_width, panel_height, true)
            });
        }
    }
    out
}

/// The least panel that shows its filter and three rows.
fn panel_floor() -> f32 {
    2.0 * PANEL_INSET + field_height() + 6.0 + 3.0 * row_height()
}

/// The least chooser that shows its filter (and the Dark | Light segment
/// while the theme follows the system) over one whole stop of its list. The
/// tallest stop is a row with [`SECTION_ABOVE`] over it: the Community
/// disclosure after rows ([`pack_row`]); a section caption
/// ([`section_heading`]) is shorter, and a footer row is a row.
fn chooser_least(state: &SettingsViewState) -> f32 {
    let segment = if follows_system(state) {
        PANEL_GAP + field_height()
    } else {
        0.0
    };
    2.0 * PANEL_INSET + field_height() + segment + PANEL_GAP + row_height() + SECTION_ABOVE
}

/// Whether the theme follows the system's appearance, which shows the
/// Dark | Light segment under the filter.
fn follows_system(state: &SettingsViewState) -> bool {
    prefs::theme_mode(state.raw_value(prefs::EDIT_THEME).as_deref())
        == prefs::ThemeMode::FollowSystem
}

/// The demo: the terminal session in the candidate theme, `height` tall.
fn demo_node(spec: SettingsPreviewSpec, height: f32) -> UiNode {
    crate::settings_preview::preview_node("settings/showcase/stage", spec, height)
}

/// The demo's spec: the Appearance projection of the candidate (the trial,
/// else the configured theme, under every override and draft) as the
/// [`PreviewScene::ThemeDemo`] scene, its tape `phase_ms` after the page
/// opened. Three tape loops after the last input on the page it rests on its
/// settled frame. The same spec answers the host's cadence
/// ([`SettingsViewState::preview_animation`]) and paints.
pub(crate) fn demo_spec(
    state: &SettingsViewState,
    phase_ms: u64,
    motion: crate::native_app::ViewMotionCx,
    terminal_font_px: f32,
    terminal_scale: f32,
    prepared_font: Option<&crate::tray_raster::PreparedSemanticFont>,
) -> Option<SettingsPreviewSpec> {
    demo_spec_and_frame(
        state,
        phase_ms,
        motion,
        terminal_font_px,
        terminal_scale,
        prepared_font,
    )
    .map(|(spec, _)| spec)
}

/// [`demo_spec`], and the spec the page sizes the demo's frame by: the same
/// before the tried theme's line height, so a typography trial redraws the
/// demo's lines but never moves the strip or the list.
fn demo_spec_and_frame(
    state: &SettingsViewState,
    phase_ms: u64,
    motion: crate::native_app::ViewMotionCx,
    terminal_font_px: f32,
    terminal_scale: f32,
    prepared_font: Option<&crate::tray_raster::PreparedSemanticFont>,
) -> Option<(SettingsPreviewSpec, SettingsPreviewSpec)> {
    let showcase = &state.showcase;
    let epoch = showcase.demo_epoch_ms.get().unwrap_or_else(|| {
        showcase.demo_epoch_ms.set(Some(phase_ms));
        phase_ms
    });
    if showcase.input_pending.replace(false) {
        showcase.last_input_phase_ms.set(phase_ms);
    }
    let idle = phase_ms.saturating_sub(showcase.last_input_phase_ms.get());
    let mut spec = renderer_preview_spec_for_key_with_font(
        state,
        prefs::EDIT_THEME,
        phase_ms,
        motion,
        terminal_font_px,
        terminal_scale,
        prepared_font,
    )?;
    let resolved = preview_resolved_colors(state, motion.system_dark);
    let typed = showcase
        .trial_id()
        .and_then(|id| showcase.card(id))
        .and_then(|card| card.typography.clone());
    let base = crate::settings_preview::SettingsPreviewSpec::theme_demo(
        spec.font_px,
        phase_ms.saturating_sub(epoch) + crate::settings_theme_demo::DEMO_OPEN_PHASE_MS,
    );
    spec.scene = base.scene;
    spec.cursor = base.cursor;
    spec.window_follows_theme = true;
    spec.demo = base.demo;
    spec = spec.with_demo(resolved.authored_chrome(), typed, 0);
    // The family and line height Keep would leave in force, where the trial
    // changes them: the demo draws in them (the app-wide trial stays colours
    // only).
    let candidate = demo_font_candidate(state);
    if Some(&candidate) != Some(&spec.font_candidate) {
        let prepared = prepared_font
            .filter(|prepared| prepared.matches(&candidate))
            .cloned()
            .unwrap_or_else(|| {
                crate::tray_raster::PreparedSemanticFont::unavailable(candidate.clone())
            });
        spec = spec
            .with_font_candidate(candidate)
            .with_prepared_font(prepared);
    }
    let frame = spec.clone();
    if let Some(line_height) = trial_type_value(state, prefs::EDIT_LINE_HEIGHT)
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        spec.line_height = line_height;
    }
    if !spec.reduced_motion && crate::settings_theme_demo::demo_rests(idle) {
        spec.reduced_motion = true;
        spec.pause = PreviewPause::Resting;
    }
    Some((spec, frame))
}

/// The value the type key `key` takes once the trial is kept, where that
/// changes what is in force (empty: aterm's own).
fn trial_type_value(state: &SettingsViewState, key: &str) -> Option<String> {
    let id = state.showcase.trial_id()?;
    state
        .trial_typography(id)
        .changes
        .into_iter()
        .find(|change| change.key == key)
        .map(|change| change.theme)
}

/// The face the demo asks the host to prepare: the page's own, with the
/// family Keep would leave in force where the trial changes it (the tried
/// theme's, or aterm's own where only the configured theme names one).
pub(crate) fn demo_font_candidate(
    state: &SettingsViewState,
) -> crate::widget::SemanticFontCandidate {
    let mut candidate = preview_font_candidate(state);
    if let Some(family) = trial_type_value(state, prefs::EDIT_FONT_FAMILY) {
        let family = family.trim();
        match aterm_render::display_face_ids_for_family(family) {
            // A tried theme's `display:` family is display faces drawn over
            // the automatic font (`Config::display_faces_request`), never the
            // font: the candidate's font is never a display face. The
            // config's own `display_font` still wins over the theme's.
            Some(ids) => {
                candidate.regular = None;
                if state.raw_value(prefs::EDIT_DISPLAY_FONT).is_none() {
                    candidate.display = aterm_render::display_layer_ids(ids);
                }
            }
            None => candidate.regular = (!family.is_empty()).then(|| family.to_string()),
        }
    }
    candidate
}

/// THE STRIP: what the demo draws and the one action that moment offers.
/// Idle: the theme in use (and how many colours of the person's own draw on
/// top) with Customize. Trying: the theme tried and what it replaces, with
/// Revert and Keep (Keep ▾ for a theme that sets type). The sheet open: Done.
/// A polite live region: its label changes once per settled trial.
fn strip_node(state: &SettingsViewState, width: f32) -> UiNode {
    let showcase = &state.showcase;
    let configured = state.configured_theme_id();
    let in_use = showcase.label_of(&configured);
    let pending = state.config_key_pending(prefs::EDIT_THEME);
    let button_height = strip_button_height();
    // Each button hugs its own words: a button wider than its label reads
    // as a field, its words pushed to one side.
    let sized = hug_width;
    let following = prefs::theme_mode(state.raw_value(prefs::EDIT_THEME).as_deref())
        == prefs::ThemeMode::FollowSystem;
    let side = theme_visible_side(state, state.presented_motion.get().system_dark);
    let overrides = override_count(&state.showcase_colors());
    let mut buttons: Vec<UiNode> = Vec::new();
    let (title, detail, live) = if showcase.sheet_open {
        buttons.push(button(
            CUSTOMIZE_DONE,
            "Done",
            "Done",
            StyleRef::Secondary,
            ControlState {
                focused: focused(state, CUSTOMIZE_DONE),
                ..ControlState::default()
            },
            Some(sized("Done")),
        ));
        let shown = showcase
            .trial_id()
            .map_or_else(|| in_use.clone(), |id| showcase.label_of(id));
        let title = if overrides > 0 {
            format!(
                "{shown} \u{b7} {}",
                count_noun(overrides, "tweak", "tweaks")
            )
        } else {
            shown.clone()
        };
        (title.clone(), String::new(), format!("Customize {title}"))
    } else {
        match &showcase.trial {
            ShowcaseTrial::Idle => {
                let note = override_note(&state.showcase_colors()).unwrap_or_default();
                let visual = if overrides > 0 {
                    format!("Customize \u{b7} {overrides}")
                } else {
                    "Customize".to_string()
                };
                // Named by the words it paints, the count's noun after them.
                let spoken = if overrides > 0 {
                    format!(
                        "Customize \u{b7} {}",
                        count_noun(overrides, "override", "overrides")
                    )
                } else {
                    "Customize".to_string()
                };
                let mut customize = button(
                    CUSTOMIZE,
                    &spoken,
                    &visual,
                    StyleRef::Secondary,
                    ControlState {
                        focused: focused(state, CUSTOMIZE),
                        ..ControlState::default()
                    },
                    Some(sized(&visual)),
                );
                if let UiContent::Button(control) = &mut customize.content {
                    control.value =
                        SemanticValue::Text(count_noun(overrides, "override", "overrides"));
                }
                buttons.push(customize);
                let live = showcase
                    .settled
                    .clone()
                    .unwrap_or_else(|| format!("Using {in_use}"));
                let detail = showcase.error.clone().unwrap_or(note);
                (in_use.clone(), detail, live)
            }
            ShowcaseTrial::Trying { id } | ShowcaseTrial::Keeping { id, .. } => {
                let keeping = matches!(showcase.trial, ShowcaseTrial::Keeping { .. })
                    || showcase.pending_keep.is_some();
                let label = showcase.label_of(id);
                let card = showcase.card(id);
                let typed = card.and_then(|card| card.typography.clone());
                let community = card.is_some_and(|card| card.section == Section::Community);
                let trial = state.trial_typography(id);
                // Keep ▾ only where the colours alone can be kept.
                let split = !keeping && trial.keeps_colours_only();
                let mut was = format!("was {in_use}");
                if following {
                    was = format!("for {} \u{b7} {was}", appearance_label(side));
                }
                if community {
                    was = format!("Community \u{b7} {was}");
                }
                if showcase.keep_more && split {
                    buttons.push(button(
                        SHOWCASE_KEEP_COLOURS,
                        "Keep colours only",
                        "Keep colours only",
                        StyleRef::Secondary,
                        ControlState {
                            enabled: !pending,
                            focused: focused(state, SHOWCASE_KEEP_COLOURS),
                            ..ControlState::default()
                        },
                        Some(hug_width("Keep colours only")),
                    ));
                }
                let mut revert = button(
                    SHOWCASE_REVERT,
                    "Revert",
                    "Revert",
                    StyleRef::Secondary,
                    ControlState {
                        enabled: !keeping,
                        focused: focused(state, SHOWCASE_REVERT),
                        ..ControlState::default()
                    },
                    Some(sized("Revert")),
                );
                if let UiContent::Button(control) = &mut revert.content {
                    control.spec.description = Some("Escape".to_string());
                }
                buttons.push(revert);
                let mut keep = button(
                    SHOWCASE_KEEP,
                    "Keep",
                    "Keep",
                    StyleRef::Primary,
                    ControlState {
                        enabled: !keeping && !pending,
                        busy: keeping,
                        focused: focused(state, SHOWCASE_KEEP),
                        ..ControlState::default()
                    },
                    Some(sized("Keep")),
                );
                if let UiContent::Button(control) = &mut keep.content {
                    control.spec.description = Some("Return".to_string());
                }
                buttons.push(keep);
                if split {
                    let more = UiNode::new(
                        KEEP_MORE,
                        UiContent::Button(
                            Control::new(
                                ButtonSpec::new("More keep options")
                                    .visual_label("")
                                    .visual_icon(ButtonIcon::ChevronDown),
                                ActionId::new(KEEP_MORE),
                            )
                            .state(ControlState {
                                expanded: Some(showcase.keep_more),
                                focused: focused(state, KEEP_MORE),
                                ..ControlState::default()
                            })
                            .style(StyleRef::Primary),
                        ),
                    )
                    .layout(
                        Layout::default()
                            .width(Length::Fixed(button_height))
                            .height(Length::Fill),
                    );
                    buttons.push(more);
                }
                let title = match &typed {
                    Some(typography) => format!("{label}  Aa {typography}"),
                    None => label.clone(),
                };
                let detail = showcase.error.clone().unwrap_or(was);
                let live = match &showcase.error {
                    Some(error) => format!("{label}: {error}"),
                    None if keeping => format!("Keeping {label}"),
                    None if following => format!(
                        "Trying {label} for {}, was {in_use}",
                        appearance_label(side)
                    ),
                    None => format!("Trying {label}, was {in_use}"),
                };
                (title, detail, live)
            }
        }
    };
    let buttons_width: f32 = buttons
        .iter()
        .map(|node| match node.layout.width {
            Length::Fixed(width) => width + 8.0,
            _ => 0.0,
        })
        .sum();
    let text_width = (width - buttons_width - 8.0).max(0.0);
    // Line 2 shows where the strip has room for it; else its words are in
    // the strip's spoken label only.
    let two_lines = width >= 400.0 && !detail.is_empty();
    // Line 1 sits on the buttons' band in every state, so the name never
    // jumps when a trial starts or ends; line 2 takes the band under it.
    let line_one = button_height;
    let mut lines = vec![
        UiNode::new(
            "settings/showcase/status/title",
            UiContent::Text(TextSpec {
                text: fit_text(&title, SemanticRole::Heading, StyleRef::Plain, text_width),
                role: SemanticRole::Heading,
                style: StyleRef::Plain,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(line_one)),
        )
        .paint_only(),
    ];
    if two_lines {
        let danger = showcase.error.is_some();
        lines.push(
            UiNode::new(
                "settings/showcase/status/detail",
                UiContent::Text(TextSpec {
                    text: fit_text(&detail, SemanticRole::Status, StyleRef::Quiet, text_width),
                    role: SemanticRole::Status,
                    style: if danger {
                        StyleRef::Danger
                    } else {
                        StyleRef::Quiet
                    },
                }),
            )
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(strip_height() - line_one)),
            )
            .paint_only(),
        );
    }
    let texts = UiNode::new(
        "settings/showcase/status/text",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::column().width(Length::Fill).height(Length::Fill))
    .children(lines)
    .paint_only();
    let under = (strip_height() - button_height).max(0.0);
    let actions = UiNode::new(
        "settings/showcase/status/actions",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::row()
            .width(Length::Fixed((buttons_width - 8.0).max(0.0)))
            .height(Length::Fill)
            .gap(8.0)
            .padding(Insets {
                top: 0.0,
                right: 0.0,
                bottom: under,
                left: 0.0,
            }),
    )
    .children(buttons);
    let spoken = if two_lines || detail.is_empty() {
        live
    } else {
        format!("{live}; {detail}")
    };
    UiNode::new(
        "settings/showcase/status",
        UiContent::Group(GroupSpec {
            label: Some(spoken),
            role: SemanticRole::Status,
            style: StyleRef::Plain,
        }),
    )
    .layout(
        Layout::row()
            .width(Length::Fill)
            .height(Length::Fixed(strip_height()))
            .gap(8.0),
    )
    .children(vec![texts, actions])
}

/// `Dark` or `Light`.
const fn appearance_label(side: aterm_types::Appearance) -> &'static str {
    match side {
        aterm_types::Appearance::Dark => "Dark",
        aterm_types::Appearance::Light => "Light",
    }
}

/// THE CHOOSER: the filter, the Dark | Light segment while the theme follows
/// the system, the list of rows, and the footer — pinned under the list
/// beside the demo, the list's last rows when `stacked`.
fn chooser_panel(
    state: &SettingsViewState,
    panel_width: f32,
    height: f32,
    stacked: bool,
) -> UiNode {
    let inner = (panel_width - 2.0 * PANEL_INSET).max(0.0);
    let narrow = panel_width < CHOOSER_NARROW_BELOW * settings_text_scale().clamp(1.0, 1.25);
    let mut children = vec![filter_node(state, inner)];
    let mut used = field_height();
    if follows_system(state) {
        children.push(side_segment(state));
        used += PANEL_GAP + field_height();
    }
    let footer = (!stacked).then(|| footer_rows(state, inner, narrow));
    if footer.is_some() {
        used += PANEL_GAP + footer_height();
    }
    let room = (height - 2.0 * PANEL_INSET - used - PANEL_GAP).max(0.0);
    let blocks_at = |width: f32| {
        let mut blocks = list_blocks(state, width, narrow);
        if stacked {
            blocks.extend(
                footer_rows(state, width, narrow)
                    .into_iter()
                    .map(|(node, height)| (node, height, None)),
            );
        }
        blocks
    };
    let mut blocks = blocks_at(inner);
    // The rows a screen holds, for Page Up and Page Down.
    state
        .showcase
        .page_rows
        .set(((room / row_height()).floor() as usize).max(1));
    *state.showcase.order.borrow_mut() = blocks
        .iter()
        .filter_map(|(_, _, slug)| slug.clone())
        .collect();
    let span: f32 = blocks.iter().map(|(_, height, _)| *height).sum();
    // While the list scrolls its rows stop short of the thumb's gutter
    // (their heights do not depend on their width). The panel is a bounded
    // box: its list never runs on past its edge into the page's overhang.
    let mut row_width = inner;
    if span > room + 0.01 {
        row_width = (inner - THUMB_GUTTER).max(0.0);
        blocks = blocks_at(row_width);
    }
    let items: Vec<ListItem> = blocks
        .into_iter()
        .map(|(mut node, height, _)| {
            node.layout.width = Length::Fixed(row_width);
            ListItem::Card(Box::new(node), height)
        })
        .collect();
    let mut list = scroll::list_page_whole(
        state,
        SHOWCASE_LIST,
        "Themes",
        items,
        room,
        0.0,
        scroll::Overhang::NONE,
    );
    set_role(&mut list, SHOWCASE_LIST, SemanticRole::ListBox);
    // The list keeps its whole room however few rows it holds, so the footer
    // stays on the panel's bottom edge while a filter narrows the list.
    if !stacked {
        list.layout.height = Length::Fixed(room);
    }
    children.push(list);
    if let Some(rows) = footer {
        children.push(rule_node("settings/showcase/footer/rule"));
        children.extend(rows.into_iter().map(|(node, _)| node));
    }
    UiNode::new(
        "settings/showcase/chooser",
        UiContent::Group(GroupSpec::new("Theme chooser").style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .width(Length::Fixed(panel_width))
            .height(Length::Fixed(height))
            .padding(Insets::all(PANEL_INSET))
            .gap(PANEL_GAP),
    )
    .children(children)
}

/// The Customize sheet in the chooser's place: its sections as one list.
fn sheet_panel(state: &SettingsViewState, panel_width: f32, height: f32) -> UiNode {
    let inner = (panel_width - 2.0 * PANEL_INSET).max(0.0);
    // The sheet's title, pinned over its list: what is being customized.
    let title_height = field_height();
    let room = (height - 2.0 * PANEL_INSET - title_height - PANEL_GAP).max(0.0);
    let gap = PANEL_GAP;
    let probe = sheet_blocks(state, inner, room);
    let span: f32 = probe.iter().map(|(_, height)| height + gap).sum::<f32>() - gap;
    let scrolls = span > room + 0.01;
    let blocks = if scrolls {
        sheet_blocks(state, inner - THUMB_GUTTER, room)
    } else {
        probe
    };
    let items: Vec<ListItem> = blocks
        .into_iter()
        .map(|(mut node, height)| {
            if scrolls {
                node.layout.padding.right += THUMB_GUTTER;
            }
            ListItem::of(node, height)
        })
        .collect();
    let shown = state
        .showcase
        .trial_id()
        .map(str::to_string)
        .unwrap_or_else(|| state.configured_theme_id());
    let label = format!("Customize {}", state.showcase.label_of(&shown));
    let list = scroll::list_page_whole(
        state,
        "settings/showcase/sheet-list",
        &label,
        items,
        room,
        gap,
        scroll::Overhang::NONE,
    );
    let title = line(
        "settings/showcase/sheet-title",
        &label,
        SemanticRole::Heading,
        StyleRef::Plain,
        inner,
        title_height,
    )
    .paint_only();
    UiNode::new(
        "settings/showcase/sheet",
        UiContent::Group(GroupSpec::new(label).style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .width(Length::Fixed(panel_width))
            .height(Length::Fixed(height))
            .padding(Insets::all(PANEL_INSET))
            .gap(PANEL_GAP),
    )
    .children(vec![title, list])
}

/// Give the node keyed `key` (the list, wherever its scroll frame put it)
/// the role `role`.
fn set_role(node: &mut UiNode, key: &str, role: SemanticRole) {
    if node.key.as_str() == key {
        if let UiContent::Group(spec) = &mut node.content {
            spec.role = role;
        }
        return;
    }
    for child in &mut node.children {
        set_role(child, key, role);
    }
}

/// The filter: always present, so ⌘F always has somewhere to go. Its
/// placeholder is fitted to the `width` it takes
/// ([`crate::native_ui::fit_native_field_placeholder`]).
fn filter_node(state: &SettingsViewState, width: f32) -> UiNode {
    let placeholder = crate::native_ui::fit_native_field_placeholder("Filter themes", width, false);
    let mut node = text_field(
        state,
        SHOWCASE_FILTER,
        "Filter themes",
        &placeholder,
        &state.showcase.filter,
        None,
        false,
    );
    node.layout.height = Length::Fixed(field_height());
    if state.showcase.filter.value().is_empty() {
        return node;
    }
    // While it holds a query, a Clear button sits over the field's end and
    // the field's text stops short of it.
    if let UiContent::TextField(field) = &mut node.content {
        field.spec.end_button = true;
    }
    node.layout.width = Length::Fill;
    let end = crate::native_ui::TEXT_FIELD_END_BUTTON;
    let pad = ((field_height() - end) / 2.0).max(0.0);
    let clear = UiNode::new(
        FILTER_CLEAR,
        UiContent::Button(
            Control::new(
                ButtonSpec::new("Clear filter")
                    .visual_label("")
                    .visual_icon(ButtonIcon::Clear),
                ActionId::new(FILTER_CLEAR),
            )
            .state(ControlState {
                focused: focused(state, FILTER_CLEAR),
                ..ControlState::default()
            })
            .style(StyleRef::Quiet),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(end))
            .height(Length::Fill),
    );
    let lead = UiNode::new(
        "settings/showcase/filter-row/lead",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::default().width(Length::Fill).height(Length::Fill));
    let end_row = UiNode::new(
        "settings/showcase/filter-row/end",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::row()
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Insets {
                top: pad,
                right: pad,
                bottom: pad,
                left: 0.0,
            }),
    )
    .children(vec![lead, clear]);
    UiNode::new(
        "settings/showcase/filter-row",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(field_height())),
    )
    .children(vec![node, end_row])
}

/// The Dark | Light segment: the side Keep writes while the theme follows
/// the system.
fn side_segment(state: &SettingsViewState) -> UiNode {
    let side = theme_visible_side(state, state.presented_motion.get().system_dark);
    let segment = |appearance: aterm_types::Appearance| {
        let word = appearance_word(appearance);
        let key = format!("{SIDE_PREFIX}{word}");
        let label = appearance_label(appearance);
        UiNode::new(
            key.clone(),
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(format!("{label} side")).visual_label(label),
                    ActionId::new(key.clone()),
                )
                .state(ControlState {
                    selected: side == appearance,
                    focused: focused(state, &key),
                    ..ControlState::default()
                })
                .style(StyleRef::Navigation),
            ),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill))
    };
    UiNode::new(
        "settings/showcase/side",
        UiContent::Group(GroupSpec::new("Side Keep writes")),
    )
    .layout(Layout::row().height(Length::Fixed(field_height())).gap(4.0))
    .children(vec![
        segment(aterm_types::Appearance::Dark),
        segment(aterm_types::Appearance::Light),
    ])
}

/// A hairline across the panel.
fn rule_node(key: &str) -> UiNode {
    UiNode::new(
        key,
        UiContent::Custom(AuditedCustomNode {
            audit_id: crate::native_ui::MESSAGES_RULE_AUDIT,
            role: SemanticRole::Group,
            label: String::new(),
            value: None,
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
        }),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(1.0)),
    )
    .paint_only()
}

/// The footer's two rows: Match system appearance, then Import… and the
/// themes folder.
fn footer_rows(state: &SettingsViewState, width: f32, narrow: bool) -> Vec<(UiNode, f32)> {
    let height = field_height();
    let following = prefs::theme_mode(state.raw_value(prefs::EDIT_THEME).as_deref())
        == prefs::ThemeMode::FollowSystem;
    let switch_width = 52.0 * settings_text_scale().min(1.4);
    let visible = if narrow {
        "Match system"
    } else {
        "Match system appearance"
    };
    let switch = UiNode::new(
        FOLLOW_SYSTEM,
        UiContent::Switch(
            Control::new(
                SwitchSpec {
                    label: "Match system appearance".to_string(),
                    description: Some(
                        "Use one theme in Dark mode and another in Light mode".to_string(),
                    ),
                },
                ActionId::new(FOLLOW_SYSTEM),
            )
            .value(SemanticValue::Bool(following))
            .state(ControlState {
                enabled: !state.config_key_pending(prefs::EDIT_THEME),
                focused: focused(state, FOLLOW_SYSTEM),
                ..ControlState::default()
            })
            // A plain switch: its words are the label beside it.
            .style(StyleRef::Quiet),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(switch_width))
            .height(Length::Fill),
    );
    let follow = UiNode::new(
        "settings/showcase/footer/follow",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::row()
            .height(Length::Fixed(height))
            .gap(6.0)
            .padding(Insets {
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
                left: TEXT_INSET,
            }),
    )
    .children(vec![
        line(
            "settings/showcase/footer/follow-label",
            visible,
            SemanticRole::Text,
            StyleRef::Plain,
            (width - TEXT_INSET - switch_width - 6.0).max(0.0),
            height,
        )
        .paint_only(),
        switch,
    ]);
    let folder_visual = if narrow { "Folder" } else { "Themes folder" };
    let links = UiNode::new(
        "settings/showcase/footer/links",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::row().height(Length::Fixed(height)).gap(4.0))
    .children(vec![
        button(
            IMPORT,
            "Import\u{2026}",
            "Import\u{2026}",
            StyleRef::Quiet,
            ControlState {
                enabled: crate::menu::local_file_picker_available(),
                focused: focused(state, IMPORT),
                ..ControlState::default()
            },
            Some(hug_width("Import\u{2026}")),
        ),
        button(
            OPEN_FOLDER,
            if narrow {
                "Folder: open themes folder"
            } else {
                "Themes folder: open it"
            },
            folder_visual,
            StyleRef::Quiet,
            ControlState {
                focused: focused(state, OPEN_FOLDER),
                ..ControlState::default()
            },
            Some(hug_width(folder_visual)),
        ),
        UiNode::new(
            "settings/showcase/footer/spacer",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
    ]);
    vec![(follow, height), (links, height)]
}

/// The list's blocks, in order, each with its height and — for a row a
/// trial can land on — its slug: NEEDS ATTENTION, the signature themes,
/// BUILT IN, YOURS, and the Community disclosure with its rows.
fn list_blocks(
    state: &SettingsViewState,
    width: f32,
    narrow: bool,
) -> Vec<(UiNode, f32, Option<String>)> {
    let showcase = &state.showcase;
    let query = showcase.query();
    let mut blocks: Vec<(UiNode, f32, Option<String>)> = Vec::new();
    let attention: Vec<&AttentionRow> = showcase
        .attention
        .iter()
        .filter(|row| {
            query.is_empty()
                || crate::native_config_language::candidate_match_score(
                    &row.id.to_lowercase(),
                    &query,
                )
                .is_some()
        })
        .collect();
    if !attention.is_empty() {
        blocks.push(section_heading(Section::Attention, false));
        for row in attention {
            blocks.push((attention_row(state, row, width), row_height(), None));
        }
    }
    let visible = showcase.visible();
    let community_total = showcase
        .cards
        .iter()
        .filter(|card| card.section == Section::Community)
        .count();
    let community_matches = showcase
        .cards
        .iter()
        .filter(|card| card.section == Section::Community && card.matches(&query))
        .count();
    // The ✓ marks the theme the side the segment shows draws: with the
    // theme following the system, the other side's theme is that side's
    // business, and two ✓ would not say which one Keep replaces.
    let configured = state.configured_theme_id();
    let highlighted = state.highlighted_slug();
    let mut section = None;
    let mut rows_before = !blocks.is_empty();
    for index in &visible {
        let card = &showcase.cards[*index];
        if section != Some(card.section) {
            section = Some(card.section);
            if card.section == Section::Community {
                blocks.push(pack_row(state, community_total, rows_before));
            } else if card.section.caption().is_some() {
                blocks.push(section_heading(card.section, rows_before));
            }
        }
        let in_use = configured.eq_ignore_ascii_case(&card.id);
        let selected = highlighted.as_deref() == Some(card.slug.as_str());
        blocks.push((
            row_node(state, card, width, narrow, in_use, selected),
            row_height(),
            Some(card.slug.clone()),
        ));
        rows_before = true;
    }
    // The disclosure stays while the section is closed, so it can open.
    if section != Some(Section::Community)
        && community_total > 0
        && (query.is_empty() || community_matches > 0)
    {
        blocks.push(pack_row(state, community_total, rows_before));
    }
    if visible.is_empty() && !query.is_empty() {
        let height = caption_height();
        let key = "settings/showcase/empty";
        let words = line(
            key,
            &format!(
                "No themes match \u{2018}{}\u{2019}.",
                showcase.filter.value().trim()
            ),
            SemanticRole::Status,
            StyleRef::Quiet,
            width - TEXT_INSET,
            height,
        );
        blocks.push((inset(key, words, 0.0, height), height, None));
    }
    blocks
}

/// A section's caption (8pt more over it where it follows a row), its words
/// on the rows' name column.
fn section_heading(section: Section, after_rows: bool) -> (UiNode, f32, Option<String>) {
    let above = if after_rows { SECTION_ABOVE } else { 0.0 };
    let height = caption_height() + above;
    let caption = section.caption().unwrap_or_default();
    let key = format!("settings/showcase/section/{}", section.word());
    let words = line(
        key.clone(),
        caption,
        SemanticRole::Heading,
        StyleRef::Quiet,
        f32::MAX,
        caption_height(),
    );
    (inset(&key, words, above, height), height, None)
}

/// `node` set in a frame `height` tall, `above` down and [`TEXT_INSET`] in:
/// a text node paints in its whole rect, so the frame places it.
fn inset(key: &str, node: UiNode, above: f32, height: f32) -> UiNode {
    UiNode::new(
        format!("{key}/inset"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fixed(height))
            .padding(Insets {
                top: above,
                right: 0.0,
                bottom: 0.0,
                left: TEXT_INSET,
            }),
    )
    .children(vec![node])
}

/// The Community disclosure.
fn pack_row(
    state: &SettingsViewState,
    count: usize,
    after_rows: bool,
) -> (UiNode, f32, Option<String>) {
    let above = if after_rows { SECTION_ABOVE } else { 0.0 };
    let open = state.showcase.pack_open || !state.showcase.query().is_empty();
    let mut spec = ButtonSpec::new("Community").visual_label("Community");
    spec.description = Some(format!(
        "{}: tried without a file, added to your themes folder on Keep",
        count_noun(
            count,
            "theme from the theme pack",
            "themes from the theme pack"
        )
    ));
    let mut node = UiNode::new(
        PACK,
        UiContent::Button(
            Control::new(spec, ActionId::new(PACK))
                .state(ControlState {
                    expanded: Some(open),
                    focused: focused(state, PACK),
                    ..ControlState::default()
                })
                .style(StyleRef::Plain),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(row_height())),
    );
    if above > 0.0 {
        node = UiNode::new(
            "settings/showcase/pack-row",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::column()
                .height(Length::Fixed(row_height() + above))
                .padding(Insets {
                    top: above,
                    right: 0.0,
                    bottom: 0.0,
                    left: 0.0,
                }),
        )
        .children(vec![node]);
    }
    (node, row_height() + above, None)
}

/// One theme's row: the option that tries it (its whole area), its name,
/// "Aa" where it sets type, ⚠ where its text contrast is under 4.5:1, a Yours
/// row's Open and Trash, the slab and the ✓ on the theme in use.
fn row_node(
    state: &SettingsViewState,
    card: &ShowcaseCard,
    width: f32,
    narrow: bool,
    in_use: bool,
    selected: bool,
) -> UiNode {
    let key = format!("{CARD_PREFIX}{}", card.slug);
    let height = row_height();
    let trying = state
        .showcase
        .trial_id()
        .is_some_and(|id| id.eq_ignore_ascii_case(&card.id));
    let mut spec = ButtonSpec::new(card.label.clone()).visual_label("");
    spec.description = (!card.description.is_empty()).then(|| card.description.clone());
    let option = UiContent::Button(
        Control::new(spec, ActionId::new(key.clone()))
            .value(SemanticValue::Text(card.row_value(in_use, trying)))
            .state(ControlState {
                selected,
                focused: focused(state, &key),
                ..ControlState::default()
            })
            .style(StyleRef::Navigation)
            .role(SemanticRole::ListBoxOption),
    );
    let yours = card.section == Section::Yours;
    let slab_width = if narrow {
        SLAB_NARROW_WIDTH
    } else {
        SLAB_WIDTH
    };
    let typed = card.typography.is_some();
    let low =
        contrast_grade(card.contrast) == crate::settings_preview::theme_card::ContrastGrade::Low;
    let tag = |suffix: &str, text: &str, style: StyleRef, tag_width: f32| {
        UiNode::new(
            format!("{key}/{suffix}"),
            UiContent::Text(TextSpec {
                text: text.to_string(),
                // Plain text, so the painter honours `style`: a status line
                // paints every style but its own in the secondary ink, which
                // greyed the ✓ that is meant to wear the accent.
                role: SemanticRole::Text,
                style,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(tag_width))
                .height(Length::Fill),
        )
    };
    let mut trailing: Vec<UiNode> = Vec::new();
    // A Yours row whose name the tag would cut gives "Aa" up to its file
    // actions, so the name keeps its room; the row's value and the strip
    // still say the type.
    let aa_width = 18.0;
    let others = [
        low.then_some(14.0),
        yours.then_some(2.0 * ICON_BUTTON + 4.0),
        Some(slab_width),
        Some(CHECK_WIDTH),
    ]
    .into_iter()
    .flatten()
    .map(|width| width + 6.0)
    .sum::<f32>();
    let name_room = width - TEXT_INSET - ROW_END - others;
    let name_fits_beside_tag = fit_text(
        &card.label,
        SemanticRole::Text,
        StyleRef::Primary,
        name_room - aa_width - 6.0,
    ) == card.label;
    if typed && (!yours || name_fits_beside_tag) {
        trailing.push(tag("aa", "Aa", StyleRef::Quiet, aa_width));
    }
    if low {
        trailing.push(tag("low", "\u{26a0}", StyleRef::Quiet, 14.0));
    }
    if yours {
        trailing.push(spacer(&format!("{key}/icons"), 2.0 * ICON_BUTTON + 4.0));
    }
    let slab_pad = ((height - SLAB_HEIGHT) / 2.0).max(0.0);
    trailing.push(
        UiNode::new(
            format!("{key}/slab-frame"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(slab_width))
                .height(Length::Fill)
                .padding(Insets {
                    top: slab_pad,
                    right: 0.0,
                    bottom: slab_pad,
                    left: 0.0,
                }),
        )
        .children(vec![
            UiNode::new(
                format!("{key}/slab"),
                UiContent::Custom(AuditedCustomNode {
                    audit_id: crate::native_ui::THEME_SLAB_AUDIT,
                    role: SemanticRole::Group,
                    label: String::new(),
                    value: Some(card.slab_value(narrow)),
                    action: None,
                    focusable: false,
                    focused: false,
                    focus_visible: false,
                }),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
        ]),
    );
    trailing.push(tag(
        "check",
        if in_use { "\u{2713}" } else { "" },
        StyleRef::Accent,
        CHECK_WIDTH,
    ));
    let trailing_width: f32 = trailing
        .iter()
        .map(|node| match node.layout.width {
            Length::Fixed(width) => width + 6.0,
            _ => 0.0,
        })
        .sum();
    let name_width = (width - TEXT_INSET - ROW_END - trailing_width).max(0.0);
    // The name is the row's words, in the primary ink: the captions over
    // the rows and the footer's labels are the quieter voice.
    let mut face_children = vec![
        UiNode::new(
            format!("{key}/name"),
            UiContent::Text(TextSpec {
                text: fit_text(
                    &card.label,
                    SemanticRole::Text,
                    StyleRef::Primary,
                    name_width,
                ),
                role: SemanticRole::Text,
                style: StyleRef::Primary,
            }),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
    ];
    face_children.extend(trailing);
    let face = UiNode::new(
        format!("{key}/face"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::row()
            .width(Length::Fill)
            .height(Length::Fill)
            .gap(6.0)
            .padding(Insets {
                top: 0.0,
                right: ROW_END,
                bottom: 0.0,
                left: TEXT_INSET,
            }),
    )
    .children(face_children)
    .paint_only_tree();
    let mut children = vec![face];
    if yours {
        children.push(file_actions(state, card, &key, slab_width));
    }
    // The option IS the row, a direct child of the list box: a list's
    // selection (AT-SPI's child index) counts options, never a wrapper.
    UiNode::new(key.clone(), option)
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(height)),
        )
        .children(children)
}

/// A fixed-width gap in a row.
fn spacer(key: &str, width: f32) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(width))
            .height(Length::Fill),
    )
}

/// A Yours row's Open and Trash, over the gap the row's face keeps for them
/// before the slab. Trash reads Confirm while one more press moves the file,
/// and is absent on the theme in use.
fn file_actions(
    state: &SettingsViewState,
    card: &ShowcaseCard,
    key: &str,
    slab_width: f32,
) -> UiNode {
    let armed = state.showcase.trash_armed.as_deref() == Some(card.slug.as_str());
    let in_use = state
        .configured_theme_ids()
        .iter()
        .any(|id| id.eq_ignore_ascii_case(&card.id));
    let icon = |action: String, label: String, icon: ButtonIcon| {
        UiNode::new(
            action.clone(),
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(label).visual_label("").visual_icon(icon),
                    ActionId::new(action.clone()),
                )
                .state(ControlState {
                    focused: focused(state, &action),
                    selected: icon == ButtonIcon::Trash && armed,
                    ..ControlState::default()
                })
                .style(StyleRef::Quiet),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(ICON_BUTTON))
                .height(Length::Fill),
        )
    };
    let mut buttons = vec![icon(
        format!("{key}/{CARD_OPEN}"),
        "Open file".to_string(),
        ButtonIcon::Document,
    )];
    if in_use {
        buttons.push(spacer(&format!("{key}/no-trash"), ICON_BUTTON).paint_only());
    } else {
        buttons.push(icon(
            format!("{key}/{CARD_TRASH}"),
            if armed {
                "Confirm Move to Trash".to_string()
            } else {
                "Move to Trash".to_string()
            },
            ButtonIcon::Trash,
        ));
    }
    // What follows the icons in the face: the slab and the ✓ column.
    let after = slab_width + 6.0 + CHECK_WIDTH + 6.0 + ROW_END;
    let pad = ((row_height() - ICON_BUTTON) / 2.0).max(0.0);
    let mut children = vec![
        UiNode::new(
            format!("{key}/actions/lead"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill))
        .paint_only(),
    ];
    children.extend(buttons);
    children.push(spacer(&format!("{key}/actions/tail"), after - 4.0).paint_only());
    UiNode::new(
        format!("{key}/actions"),
        UiContent::Group(GroupSpec::new(format!("{} file", card.label))),
    )
    .layout(
        Layout::row()
            .width(Length::Fill)
            .height(Length::Fill)
            .gap(4.0)
            .padding(Insets {
                top: pad,
                right: 0.0,
                bottom: pad,
                left: 0.0,
            }),
    )
    .children(children)
}

/// A theme file that did not load: its name, why (in the danger ink), and
/// Open. Not a row a trial can land on.
fn attention_row(state: &SettingsViewState, row: &AttentionRow, width: f32) -> UiNode {
    let key = format!("{CARD_PREFIX}{}", row.slug);
    let open_key = format!("{key}/{CARD_OPEN}");
    let pad = ((row_height() - ICON_BUTTON) / 2.0).max(0.0);
    let text_width = (width - 12.0 - ICON_BUTTON - 8.0 - 12.0).max(0.0);
    let name_width = (text_width * 0.45).max(0.0);
    UiNode::new(
        key.clone(),
        UiContent::Group(GroupSpec::new(format!("{}, {}", row.id, row.reason))),
    )
    .layout(
        Layout::row()
            .height(Length::Fixed(row_height()))
            .gap(6.0)
            .padding(Insets {
                top: 0.0,
                right: 8.0,
                bottom: 0.0,
                left: 12.0,
            }),
    )
    .children(vec![
        UiNode::new(
            format!("{key}/name"),
            UiContent::Text(TextSpec {
                text: fit_text(&row.id, SemanticRole::Text, StyleRef::Primary, name_width),
                role: SemanticRole::Text,
                style: StyleRef::Primary,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(name_width))
                .height(Length::Fill),
        )
        .paint_only(),
        UiNode::new(
            format!("{key}/reason"),
            UiContent::Text(TextSpec {
                text: fit_text(
                    &row.reason,
                    SemanticRole::Status,
                    StyleRef::Danger,
                    text_width - name_width - 6.0,
                ),
                role: SemanticRole::Status,
                style: StyleRef::Danger,
            }),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill))
        .paint_only(),
        UiNode::new(
            open_key.clone(),
            UiContent::Button(
                Control::new(
                    ButtonSpec::new("Open file")
                        .visual_label("")
                        .visual_icon(ButtonIcon::Document),
                    ActionId::new(open_key.clone()),
                )
                .state(ControlState {
                    focused: focused(state, &open_key),
                    ..ControlState::default()
                })
                .style(StyleRef::Quiet),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(ICON_BUTTON))
                .height(Length::Fill)
                .padding(Insets {
                    top: pad,
                    right: 0.0,
                    bottom: pad,
                    left: 0.0,
                }),
        ),
    ])
}
