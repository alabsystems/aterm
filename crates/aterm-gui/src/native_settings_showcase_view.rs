// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Themes layout across Beside, Short, and Stacked modes ([`scroll::ListFoot::Whole`]).

use super::customize::{
    caption_height, colour_rows_least_column, colour_rows_on_one_line, sheet_blocks,
    sheet_list_least, sheet_sized_by_room,
};
use super::*;
use crate::settings_preview::theme_card::{
    SLAB_HEIGHT, SLAB_NARROW_WIDTH, SLAB_WIDTH, contrast_grade,
};

const BESIDE_FROM: f32 = 640.0;
const CHOOSER_WIDE_FROM: f32 = 900.0;
const CHOOSER_WIDE: f32 = 256.0;
const CHOOSER_MEDIUM: f32 = 232.0;
const CHOOSER_NARROW_BELOW: f32 = 240.0;
const COLUMN_GAP: f32 = 16.0;
const SHORT_ROWS: f32 = 4.0;
const DEMO_LEAST_WIDTH: f32 = 288.0;
const PANEL_INSET: f32 = 8.0;
const DEMO_STRIP_GAP: f32 = 8.0;
fn check_width() -> f32 {
    16.0_f32.max(text_width("\u{2713}", SemanticRole::Text, StyleRef::Accent).ceil())
}

fn aa_width() -> f32 {
    18.0_f32.max(text_width("Aa", SemanticRole::Text, StyleRef::Quiet).ceil())
}

fn low_width() -> f32 {
    14.0_f32.max(text_width("\u{26a0}", SemanticRole::Text, StyleRef::Quiet).ceil())
}

/// The widths of a row's tags at the live text size, measured once for the rows a list builds.
#[derive(Clone, Copy)]
struct TagWidths {
    aa: f32,
    low: f32,
    check: f32,
}

impl TagWidths {
    fn now() -> Self {
        Self {
            aa: aa_width(),
            low: low_width(),
            check: check_width(),
        }
    }
}

const ICON_BUTTON: f32 = 20.0;
const ROW_END: f32 = 4.0;

/// The least a theme's name keeps beside what shares its line.
pub(super) fn name_floor(state: &SettingsViewState, role: SemanticRole, style: StyleRef) -> f32 {
    let showcase = &state.showcase;
    let key = (
        std::sync::Arc::as_ptr(&showcase.cards).cast::<()>() as usize,
        settings_text_scale().to_bits(),
        role == SemanticRole::Heading,
    );
    let cached = showcase.name_floors.borrow().iter().find_map(|entry| {
        ((entry.cards, entry.scale, entry.heading) == key).then_some(entry.floor)
    });
    if let Some(floor) = cached {
        return floor;
    }
    let floor = showcase
        .cards
        .iter()
        .filter_map(|card| {
            let mut words = card.label.split_whitespace();
            let first = words.next()?;
            Some(if words.next().is_some() {
                format!("{first}\u{2026}")
            } else {
                first.to_string()
            })
        })
        .map(|lead| text_width(&lead, role, style).ceil())
        .fold(0.0_f32, f32::max)
        .min(COMPACT_INLINE_LABEL_MIN);
    let mut floors = showcase.name_floors.borrow_mut();
    floors.retain(|entry| entry.cards == key.0);
    floors.push(NameFloor {
        cards: key.0,
        scale: key.1,
        heading: key.2,
        floor,
    });
    floor
}

fn slab_column(state: &SettingsViewState, width: f32, narrow: bool) -> Option<f32> {
    let slab = if narrow {
        SLAB_NARROW_WIDTH
    } else {
        SLAB_WIDTH
    };
    let room = width - TEXT_INSET - ROW_END - (slab + 6.0) - (check_width() + 6.0);
    (room + 0.01 >= name_floor(state, SemanticRole::Text, StyleRef::Primary)).then_some(slab)
}

fn row_height() -> f32 {
    32.0_f32.max(28.0 * settings_text_scale())
}

/// The strip's words' height.
pub(super) fn strip_height() -> f32 {
    40.0_f32.max(36.0 * settings_text_scale())
}

fn field_height() -> f32 {
    32.0_f32.max(28.0 * settings_text_scale())
}

fn footer_height() -> f32 {
    1.0 + 2.0 * (PANEL_GAP + field_height())
}

const PANEL_GAP: f32 = 6.0;
const SECTION_ABOVE: f32 = 8.0;
const TEXT_INSET: f32 = crate::native_ui::BUTTON_LABEL_INSET;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    Beside { chooser: f32 },
    Short { chooser: f32 },
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

fn short_or_stacked(
    state: &SettingsViewState,
    page_width: f32,
    room: f32,
    frame: Option<&SettingsPreviewSpec>,
) -> Shape {
    let Some(chooser) = short_chooser(state, page_width) else {
        return Shape::Stacked;
    };
    let (_, strip) = strip_node(state, page_width);
    let demo = stacked_demo(state, room, strip, frame);
    let list = stacked_rest(room, demo, strip) - short_head(state);
    if list + 0.01 < SHORT_ROWS * row_height()
        && beside_fits(state, chooser, page_width, room, frame)
    {
        Shape::Short { chooser }
    } else {
        Shape::Stacked
    }
}

fn short_chooser(state: &SettingsViewState, page_width: f32) -> Option<f32> {
    let chooser = (CHOOSER_MEDIUM * settings_text_scale().clamp(1.0, 1.25))
        .min(page_width - COLUMN_GAP - DEMO_LEAST_WIDTH);
    let rows = chooser - 2.0 * PANEL_INSET - THUMB_GUTTER;
    slab_column(state, rows, chooser_is_narrow(chooser)).map(|_| chooser)
}

fn short_head(state: &SettingsViewState) -> f32 {
    let pins = ChooserPins {
        head: true,
        footer: false,
    };
    2.0 * PANEL_INSET + chooser_pinned_height(state, pins)
}

fn list_rows(state: &SettingsViewState, height: f32, pins: ChooserPins) -> f32 {
    let around = 2.0 * PANEL_INSET + chooser_pinned_height(state, pins);
    ((height - around + 0.01) / row_height()).floor().max(0.0)
}

fn whole_rows_height(state: &SettingsViewState, room: f32, pins: ChooserPins) -> f32 {
    let around = 2.0 * PANEL_INSET + chooser_pinned_height(state, pins);
    let rows = list_rows(state, room, pins).max(1.0);
    (around + rows * row_height()).min(room)
}

fn stacked_rest(room: f32, demo: f32, strip: f32) -> f32 {
    room - demo - 2.0 * DEMO_STRIP_GAP - strip
}

fn stacked_demo(
    state: &SettingsViewState,
    room: f32,
    strip: f32,
    frame: Option<&SettingsPreviewSpec>,
) -> f32 {
    let ten = frame.map_or(240.0, |frame| frame.demo_height(10));
    let six = frame.map_or(180.0, |frame| frame.demo_height(6));
    let demo = (room * 0.38).max(180.0).min(ten);
    if stacked_rest(room, demo, strip) - short_head(state) + 0.01 < SHORT_ROWS * row_height() {
        six.min(demo)
    } else {
        demo
    }
}

/// The page: the heading, then the chooser beside the demo and strip.
pub(crate) fn themes_page(
    state: &SettingsViewState,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> Vec<UiNode> {
    let viewport = cx.viewport;
    let gap = settings_page_gap(state, width, viewport);
    let maximum = PageLayout::Stage.maximum(width);
    let page_width = settings_page_content_width(viewport.width, width, maximum);
    let (mut out, used) = pinned_heading(state, width, viewport, gap, SHOWCASE_TITLE, "");
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
    let room = (body - used).max(0.0);
    let pinned = ChooserPins {
        head: true,
        footer: true,
    };
    let shape = match Shape::for_width(page_width) {
        Shape::Beside { chooser }
            if !beside_fits(state, chooser, page_width, room, frame.as_ref()) =>
        {
            Shape::Stacked
        }
        Shape::Beside { chooser } if list_rows(state, room, pinned) < SHORT_ROWS => {
            Shape::Short { chooser }
        }
        Shape::Stacked => short_or_stacked(state, page_width, room, frame.as_ref()),
        shape => shape,
    };
    match shape {
        Shape::Beside { chooser } | Shape::Short { chooser } => {
            let pins = ChooserPins {
                head: true,
                footer: matches!(shape, Shape::Beside { .. }),
            };
            let body = room;
            let room = whole_rows_height(state, room, pins);
            let chooser = panel_width(chooser, page_width);
            let demo_width = (page_width - chooser - COLUMN_GAP).max(0.0);
            let (strip_box, strip) = strip_node(state, demo_width);
            // THE STRIP STANDS OVER A STATUS'S BAR: the demo gives up what
            // the bar covers of the column, and the bar shortens for a
            // control of the strip it would still cover. Laid out under the
            // bar, the sheet's Done at 640x480 at 2× text was 8pt of 48.
            let cover = (state.status_extra.get() - (body - room)).max(0.0);
            let demo_height = (room - cover - DEMO_STRIP_GAP - strip).max(0.0);
            state.note_pinned_status_room(&strip_box, body - DEMO_STRIP_GAP - strip);
            let left = if state.showcase.sheet_open {
                sheet_panel(state, chooser, room, Vec::new())
            } else {
                chooser_panel(state, chooser, room, shape, Vec::new())
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
                    .chain([strip_box])
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
            let (strip_box, strip) = strip_node(state, page_width);
            let rest = |room: f32, demo: f32| stacked_rest(room, demo, strip);
            let mut demo_height = stacked_demo(state, room, strip, frame.as_ref());
            let rows = (!state.showcase.sheet_open).then(|| {
                chooser_rows_tallest(
                    state,
                    (page_width - 2.0 * PANEL_INSET).max(0.0),
                    chooser_is_narrow(page_width),
                )
            });
            let least = match rows {
                Some(rows) => chooser_least(state, rows),
                None => sheet_least(state, page_width),
            };
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
            let floor = match rows {
                Some(rows) => chooser_floor(state, rows),
                None => sheet_floor(state, page_width),
            };
            match (spec, frame.as_ref()) {
                (Some(spec), Some(frame)) if panel_height + 0.5 < floor => {
                    let inner = (page_width - 2.0 * PANEL_INSET - THUMB_GUTTER).max(0.0);
                    let list_room = (room - 2.0 * PANEL_INSET).max(0.0);
                    let (strip_box, strip) = strip_node(state, inner);
                    let demo = (1..=6)
                        .rev()
                        .map(|rows| frame.demo_height(rows))
                        .find(|&height| {
                            height + DEMO_STRIP_GAP + strip + DEMO_STRIP_GAP <= list_room + 0.01
                        })
                        .unwrap_or_else(|| frame.demo_height(1));
                    let lead = vec![(demo_node(spec, demo), demo), (strip_box, strip)];
                    out.push(if state.showcase.sheet_open {
                        sheet_panel(state, page_width, room, lead)
                    } else {
                        chooser_panel(state, page_width, room, Shape::Stacked, lead)
                    });
                }
                (spec, _) => {
                    if let Some(spec) = spec {
                        out.push(demo_node(spec, demo_height));
                    }
                    let pins = ChooserPins {
                        head: true,
                        footer: false,
                    };
                    let panel_height = whole_rows_height(state, panel_height, pins)
                        .max(least)
                        .min(panel_height);
                    state.note_pinned_status_room(&strip_box, panel_height);
                    out.push(strip_box);
                    out.push(if state.showcase.sheet_open {
                        sheet_panel(state, page_width, panel_height, Vec::new())
                    } else {
                        chooser_panel(state, page_width, panel_height, Shape::Stacked, Vec::new())
                    });
                }
            }
        }
    }
    out
}

fn panel_width(chooser: f32, page_width: f32) -> f32 {
    let sheet = colour_rows_least_column() + 2.0 * PANEL_INSET + THUMB_GUTTER;
    if chooser + 0.01 >= sheet || sheet > page_width - COLUMN_GAP - DEMO_LEAST_WIDTH + 0.01 {
        chooser
    } else {
        sheet
    }
}

fn beside_fits(
    state: &SettingsViewState,
    chooser: f32,
    page_width: f32,
    room: f32,
    frame: Option<&SettingsPreviewSpec>,
) -> bool {
    let demo = frame.map_or(0.0, |frame| frame.demo_height(1));
    let panel_width = panel_width(chooser, page_width);
    let (_, strip) = strip_node(state, (page_width - panel_width - COLUMN_GAP).max(0.0));
    let panel = if state.showcase.sheet_open {
        sheet_floor(state, panel_width)
    } else {
        let inner = (panel_width - 2.0 * PANEL_INSET).max(0.0);
        let rows = chooser_rows_tallest(state, inner, chooser_is_narrow(panel_width));
        chooser_floor(state, rows)
    };
    room + 0.5 >= demo + DEMO_STRIP_GAP + strip && room + 0.5 >= panel
}

fn chooser_least(state: &SettingsViewState, rows: f32) -> f32 {
    let pins = ChooserPins {
        head: true,
        footer: false,
    };
    2.0 * PANEL_INSET + chooser_pinned_height(state, pins) + chooser_tallest_stop(state, pins, rows)
}

fn chooser_floor(state: &SettingsViewState, rows: f32) -> f32 {
    2.0 * PANEL_INSET + chooser_tallest_stop(state, ChooserPins::NONE, rows)
}

fn chooser_is_narrow(panel_width: f32) -> bool {
    panel_width < CHOOSER_NARROW_BELOW * settings_text_scale().clamp(1.0, 1.25)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChooserPins {
    head: bool,
    footer: bool,
}

impl ChooserPins {
    const NONE: Self = Self {
        head: false,
        footer: false,
    };
}

fn chooser_pinned_height(state: &SettingsViewState, pins: ChooserPins) -> f32 {
    let head = if pins.head {
        chooser_head_heights(state)
            .iter()
            .map(|height| height + PANEL_GAP)
            .sum()
    } else {
        0.0
    };
    let footer = if pins.footer {
        PANEL_GAP + footer_height()
    } else {
        0.0
    };
    head + footer
}

fn chooser_rows_tallest(state: &SettingsViewState, inner: f32, narrow: bool) -> f32 {
    with_blocks(state, (inner, narrow, ""), |entry| {
        *entry.tallest.get_or_insert_with(|| {
            let items = section_items(entry.blocks.clone(), inner, false);
            scroll::tallest_span(&items, 0.0, f32::INFINITY)
        })
    })
}

fn chooser_tallest_stop(state: &SettingsViewState, pins: ChooserPins, rows: f32) -> f32 {
    let head = if pins.head {
        0.0
    } else {
        chooser_head_heights(state)
            .into_iter()
            .map(|height| height + PANEL_GAP)
            .fold(0.0, f32::max)
    };
    let footer = if pins.footer { 0.0 } else { field_height() };
    rows.max(head).max(footer)
}

fn chooser_head_heights(state: &SettingsViewState) -> Vec<f32> {
    if follows_system(state) {
        vec![field_height(), field_height()]
    } else {
        vec![field_height()]
    }
}

fn chooser_head(state: &SettingsViewState, width: f32) -> Vec<(UiNode, f32)> {
    let mut head = vec![(filter_node(state, width), field_height())];
    if follows_system(state) {
        head.push((side_segment(state), field_height()));
    }
    head
}

fn gapped(node: UiNode, height: f32, gap: f32) -> (UiNode, f32) {
    let key = node.key.as_str().to_string();
    let stop = UiNode::new(
        format!("{key}/stop"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fixed(height + gap))
            .padding(Insets {
                top: 0.0,
                right: 0.0,
                bottom: gap,
                left: 0.0,
            }),
    )
    .children(vec![node]);
    (stop, height + gap)
}

fn chooser_items(
    state: &SettingsViewState,
    mut blocks: Vec<(UiNode, f32, Option<String>)>,
    width: f32,
    narrow: bool,
    pins: ChooserPins,
    lead: Vec<(UiNode, f32)>,
    captions: bool,
) -> Vec<ListItem> {
    if !pins.footer {
        blocks.extend(
            footer_rows(state, width, narrow)
                .into_iter()
                .map(|(node, height)| (node, height, None)),
        );
    }
    let head = if pins.head {
        Vec::new()
    } else {
        chooser_head(state, width)
    }
    .into_iter()
    .map(|(node, height)| gapped(node, height, PANEL_GAP));
    lead.into_iter()
        .map(|(node, height)| lead_item(node, height, DEMO_STRIP_GAP, width))
        .chain(head.map(|(mut node, height)| {
            node.layout.width = Length::Fixed(width);
            ListItem::Card(Box::new(node), height)
        }))
        .chain(section_items(blocks, width, captions))
        .collect()
}

fn lead_item(node: UiNode, height: f32, after: f32, width: f32) -> ListItem {
    if node.key.as_str() == STRIP_KEY
        && node.layout.flow == crate::native_ui::Flow::Column
        && let UiContent::Group(spec) = &node.content
    {
        let rows = node
            .children
            .iter()
            .map(|child| {
                let mut row = child.clone();
                row.layout.width = Length::Fixed(width);
                let height = fixed_height(child);
                (row, height)
            })
            .collect();
        return ListItem::Split(scroll::SplitCard {
            key: STRIP_KEY.to_string(),
            spec: spec.clone(),
            padding: Insets {
                top: 0.0,
                right: 0.0,
                bottom: after,
                left: 0.0,
            },
            gap: node.layout.gap,
            head: Vec::new(),
            rows,
        });
    }
    let (mut stop, height) = gapped(node, height, after);
    stop.layout.width = Length::Fixed(width);
    ListItem::Card(Box::new(stop), height)
}

const STRIP_KEY: &str = "settings/showcase/status";

fn sheet_least(state: &SettingsViewState, panel_width: f32) -> f32 {
    let rows = (panel_width - 2.0 * PANEL_INSET - THUMB_GUTTER).max(0.0);
    2.0 * PANEL_INSET + field_height() + PANEL_GAP + sheet_list_least(state, rows)
}

fn follows_system(state: &SettingsViewState) -> bool {
    prefs::theme_mode(state.raw_value(prefs::EDIT_THEME).as_deref())
        == prefs::ThemeMode::FollowSystem
}

fn demo_node(spec: SettingsPreviewSpec, height: f32) -> UiNode {
    let mut node = crate::settings_preview::preview_node("settings/showcase/stage", spec, height);
    node.layout.height = Length::Fixed(height);
    node
}

/// The demo's spec: the Appearance projection of the candidate.
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
    spec.demo = base.demo;
    spec = spec.with_demo(resolved.authored_chrome(), typed, 0);
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

fn trial_type_value(state: &SettingsViewState, key: &str) -> Option<String> {
    let id = state.showcase.trial_id()?;
    state
        .trial_typography(id)
        .changes
        .into_iter()
        .find(|change| change.key == key)
        .map(|change| change.theme)
}

/// The face the demo asks the host to prepare.
pub(crate) fn demo_font_candidate(
    state: &SettingsViewState,
) -> crate::widget::SemanticFontCandidate {
    let mut candidate = preview_font_candidate(state);
    if let Some(family) = trial_type_value(state, prefs::EDIT_FONT_FAMILY) {
        let family = family.trim();
        match aterm_render::display_face_ids_for_family(family) {
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

fn strip_node(state: &SettingsViewState, width: f32) -> (UiNode, f32) {
    let showcase = &state.showcase;
    let configured = state.configured_theme_id();
    let in_use = showcase.label_of(&configured);
    let button_height = strip_button_height();
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
            format!("{shown} \u{b7} {}", custom_colors(overrides))
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
                let spoken = if overrides > 0 {
                    format!("Customize \u{b7} {}", custom_colors(overrides))
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
                    control.value = SemanticValue::Text(custom_colors(overrides));
                }
                buttons.push(customize);
                let live = showcase
                    .settled
                    .as_ref()
                    .filter(|settled| settled.id.eq_ignore_ascii_case(&configured))
                    .map_or_else(
                        || format!("Using {in_use}"),
                        |settled| settled.words.clone(),
                    );
                let detail = showcase.error.clone().unwrap_or(note);
                (in_use.clone(), detail, live)
            }
            ShowcaseTrial::Trying { id } | ShowcaseTrial::Keeping { id, .. } => {
                let keeping = matches!(showcase.trial, ShowcaseTrial::Keeping { .. })
                    || showcase.pending_keep.is_some();
                let label = showcase.label_of(id);
                let in_use = showcase.label_of(&state.theme_id_under_trial());
                let card = showcase.card(id);
                let typed = card.and_then(|card| card.typography.clone());
                let community = card.is_some_and(|card| card.section == Section::Community);
                let trial = state.trial_typography(id);
                let split = trial.keeps_colours_only();
                let mut was = format!("was {in_use}");
                if following {
                    was = format!("for {} \u{b7} {was}", appearance_label(side));
                }
                if community {
                    was = format!("Community \u{b7} {was}");
                }
                let note = override_note(&state.showcase_colors());
                if let Some(note) = &note {
                    was = format!("{was} \u{b7} {note}");
                }
                if showcase.keep_more && split {
                    buttons.push(button(
                        SHOWCASE_KEEP_COLOURS,
                        "Keep colors only",
                        "Keep colors only",
                        StyleRef::Secondary,
                        ControlState {
                            focused: focused(state, SHOWCASE_KEEP_COLOURS),
                            ..ControlState::default()
                        },
                        Some(hug_width("Keep colors only")),
                    ));
                }
                let mut revert = button(
                    SHOWCASE_REVERT,
                    "Revert",
                    "Revert",
                    StyleRef::Secondary,
                    ControlState {
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
                let live = match &note {
                    Some(note) if showcase.error.is_none() && !keeping => {
                        format!("{live}, {note}")
                    }
                    _ => live,
                };
                (title, detail, live)
            }
        }
    };
    let gap = 8.0;
    let span = |widths: &[f32]| {
        widths.iter().map(|width| width + gap).sum::<f32>()
            - if widths.is_empty() { 0.0 } else { gap }
    };
    let widths: Vec<f32> = buttons
        .iter()
        .map(|node| match node.layout.width {
            Length::Fixed(width) => width,
            _ => 0.0,
        })
        .collect();
    let trial = span(&[hug_width("Revert"), hug_width("Keep"), button_height]);
    let title_floor = name_floor(state, SemanticRole::Heading, StyleRef::Plain);
    let stacked =
        !buttons.is_empty() && width - span(&widths).max(trial) - gap + 0.01 < title_floor;
    let title_room = if stacked {
        width
    } else {
        (width - span(&widths) - gap).max(0.0)
    };
    let detail_line = width >= DETAIL_LINE_FROM;
    let two_lines = detail_line && !detail.is_empty();
    let line_one = button_height;
    let words_height = if detail_line {
        strip_height()
    } else {
        line_one
    };
    let mut lines = vec![
        UiNode::new(
            "settings/showcase/status/title",
            UiContent::Text(TextSpec {
                text: fit_text(&title, SemanticRole::Heading, StyleRef::Plain, title_room),
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
                    text: fit_text(&detail, SemanticRole::Status, StyleRef::Quiet, title_room),
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
    .layout(Layout::column().width(Length::Fill).height(if stacked {
        Length::Fixed(words_height)
    } else {
        Length::Fill
    }))
    .children(lines)
    .paint_only();
    let spoken = if two_lines || detail.is_empty() {
        live
    } else {
        format!("{live}; {detail}")
    };
    let status = |height: f32, layout: Layout, children: Vec<UiNode>| {
        UiNode::new(
            "settings/showcase/status",
            UiContent::Group(GroupSpec {
                label: Some(spoken.clone()),
                role: SemanticRole::Status,
                style: StyleRef::Plain,
            }),
        )
        .layout(
            layout
                .width(Length::Fill)
                .height(Length::Fixed(height))
                .gap(if stacked { STRIP_LINE_GAP } else { gap }),
        )
        .children(children)
    };
    if !stacked {
        let under = (strip_height() - button_height).max(0.0);
        let actions = UiNode::new(
            "settings/showcase/status/actions",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::row()
                .width(Length::Fixed(span(&widths).max(0.0)))
                .height(Length::Fill)
                .gap(gap)
                .padding(Insets {
                    top: 0.0,
                    right: 0.0,
                    bottom: under,
                    left: 0.0,
                }),
        )
        .children(buttons);
        let height = strip_height();
        return (status(height, Layout::row(), vec![texts, actions]), height);
    }
    let mut button_lines: Vec<(Vec<UiNode>, f32)> = Vec::new();
    let mut used = f32::INFINITY;
    for mut node in buttons {
        let own = match node.layout.width {
            Length::Fixed(own) => own.min(width),
            _ => width,
        };
        node.layout.width = Length::Fixed(own);
        let mut tall = button_height;
        if let UiContent::Button(control) = &mut node.content
            && let Some(words) = control
                .spec
                .visual_label
                .clone()
                .filter(|words| !words.is_empty())
        {
            let (words, lines) = button_words_in(&control.spec, &words, &[], own);
            control.spec.visual_label = Some(words);
            tall = crate::native_ui::native_button_height_for_lines(lines, button_height);
        }
        node.layout.height = Length::Fixed(tall);
        match button_lines.last_mut() {
            Some((line, height)) if used + gap + own <= width + 0.01 => {
                used += gap + own;
                *height = height.max(tall);
                line.push(node);
            }
            _ => {
                used = own;
                button_lines.push((vec![node], tall));
            }
        }
    }
    let drawn = button_lines
        .iter()
        .map(|(_, height)| STRIP_LINE_GAP + height)
        .sum::<f32>();
    let reserved = flow_lines(
        &[hug_width("Revert"), hug_width("Keep"), button_height],
        width,
        gap,
    ) as f32
        * (STRIP_LINE_GAP + button_height);
    let height = words_height + drawn.max(reserved);
    let mut children = vec![texts];
    children.extend(
        button_lines
            .into_iter()
            .enumerate()
            .map(|(index, (line, tall))| {
                UiNode::new(
                    format!("settings/showcase/status/actions/line-{index}"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(
                    Layout::row()
                        .width(Length::Fill)
                        .height(Length::Fixed(tall))
                        .gap(gap),
                )
                .children(line)
            }),
    );
    if reserved > drawn + 0.01 {
        children.push(
            UiNode::new(
                "settings/showcase/status/actions/reserve",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(reserved - drawn - STRIP_LINE_GAP)),
            )
            .paint_only(),
        );
    }
    (status(height, Layout::column(), children), height)
}

fn flow_lines(widths: &[f32], width: f32, gap: f32) -> usize {
    let mut lines = 0;
    let mut used = f32::INFINITY;
    for own in widths.iter().map(|own| own.min(width)) {
        if used + gap + own <= width + 0.01 {
            used += gap + own;
        } else {
            used = own;
            lines += 1;
        }
    }
    lines
}

const DETAIL_LINE_FROM: f32 = 400.0;

const STRIP_LINE_GAP: f32 = 4.0;

const fn appearance_label(side: aterm_types::Appearance) -> &'static str {
    match side {
        aterm_types::Appearance::Dark => "Dark",
        aterm_types::Appearance::Light => "Light",
    }
}

fn chooser_panel(
    state: &SettingsViewState,
    panel_width: f32,
    height: f32,
    shape: Shape,
    lead: Vec<(UiNode, f32)>,
) -> UiNode {
    let stacked = !matches!(shape, Shape::Beside { .. });
    let inner = (panel_width - 2.0 * PANEL_INSET).max(0.0);
    let narrow = chooser_is_narrow(panel_width);
    let forms: &[ChooserPins] = if !lead.is_empty() {
        &[ChooserPins::NONE]
    } else if stacked {
        &[
            ChooserPins {
                head: true,
                footer: false,
            },
            ChooserPins::NONE,
        ]
    } else {
        &[
            ChooserPins {
                head: true,
                footer: true,
            },
            ChooserPins {
                head: true,
                footer: false,
            },
            ChooserPins::NONE,
        ]
    };
    let room_for = |pins: ChooserPins| {
        (height - 2.0 * PANEL_INSET - chooser_pinned_height(state, pins)).max(0.0)
    };
    let footer_pins = footer_is_one_line(state, inner, narrow);
    let blocks = list_blocks(state, inner, narrow);
    *state.showcase.order.borrow_mut() = blocks
        .iter()
        .filter_map(|(_, _, slug)| slug.clone())
        .collect();
    let tallest_row = chooser_rows_tallest(state, inner, narrow);
    let rows = tallest_row.max(
        footer_rows(state, inner, narrow)
            .iter()
            .map(|(_, height)| *height)
            .fold(0.0, f32::max),
    );
    let lead_items: Vec<ListItem> = lead
        .iter()
        .cloned()
        .map(|(node, height)| lead_item(node, height, DEMO_STRIP_GAP, inner))
        .collect();
    let lead_tallest = |room: f32| scroll::tallest_span(&lead_items, 0.0, room);
    let pins = forms
        .iter()
        .copied()
        .filter(|pins| footer_pins || !pins.footer)
        .find(|&pins| {
            let room = room_for(pins);
            room + 0.5 >= chooser_tallest_stop(state, pins, rows).max(lead_tallest(room))
        })
        .unwrap_or(ChooserPins::NONE);
    let room = room_for(pins);
    state
        .showcase
        .page_rows
        .set(((room / row_height()).floor() as usize).max(1));
    let captions = room + 0.5 >= caption_height() + SECTION_ABOVE + tallest_row.max(row_height());
    let items_at = |blocks, width: f32| {
        chooser_items(state, blocks, width, narrow, pins, lead.clone(), captions)
    };
    let mut items = items_at(blocks, inner);
    let span = scroll::leading_span(&items, 0.0, f32::INFINITY, usize::MAX);
    if span > room + 0.01 {
        let width = (inner - THUMB_GUTTER).max(0.0);
        items = items_at(list_blocks(state, width, narrow), width);
    }
    let mut list = scroll::list_page_whole(state, SHOWCASE_LIST, "Themes", items, room, 0.0);
    set_role(&mut list, SHOWCASE_LIST, SemanticRole::ListBox);
    transparent_sections(&mut list);
    if pins.footer {
        list.layout.height = Length::Fixed(room);
    }
    let head = if pins.head {
        chooser_head(state, inner)
    } else {
        Vec::new()
    };
    let under_head = head
        .iter()
        .fold(height - PANEL_INSET + PANEL_GAP, |under, (_, tall)| {
            under - tall - PANEL_GAP
        });
    let mut children: Vec<UiNode> = head
        .into_iter()
        .map(|(node, _)| {
            state.note_pinned_status_room(&node, under_head);
            node
        })
        .collect();
    children.push(list);
    if pins.footer {
        children.push(rule_node("settings/showcase/footer/rule"));
        children.extend(
            footer_rows(state, inner, narrow)
                .into_iter()
                .map(|(node, _)| node),
        );
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

const FOLLOW_SYSTEM_LABEL: &str = "Follow system light/dark";

/// The Customize sheet's title, pinned over its list.
pub(crate) const SHEET_TITLE: &str = "Custom colors for every theme";

const SHEET_TITLE_SHORT: &str = "Colors for every theme";

fn sheet_title_words(width: f32) -> &'static str {
    let fits = |words: &str| {
        text_width(words, SemanticRole::Heading, StyleRef::Plain)
            <= width + crate::native_ui::TEXT_FIT_SLACK
    };
    if fits(SHEET_TITLE) {
        SHEET_TITLE
    } else {
        SHEET_TITLE_SHORT
    }
}

fn sheet_list_room(height: f32) -> f32 {
    (height - 2.0 * PANEL_INSET - field_height() - PANEL_GAP).max(0.0)
}

/// The chooser's blocks as list items.
pub(in crate::native_settings) fn section_items(
    blocks: Vec<(UiNode, f32, Option<String>)>,
    row_width: f32,
    captions: bool,
) -> Vec<ListItem> {
    const SECTION_PREFIX: &str = "settings/showcase/section/";
    let mut items = Vec::new();
    let mut blocks = blocks.into_iter().peekable();
    while let Some((mut node, height, _)) = blocks.next() {
        node.layout.width = Length::Fixed(row_width);
        let Some(word) = captions
            .then_some(&node)
            .and_then(|node| node.key.as_str().strip_prefix(SECTION_PREFIX))
            .and_then(|rest| rest.strip_suffix("/inset"))
            .map(str::to_string)
        else {
            items.push(ListItem::Card(Box::new(node), height));
            continue;
        };
        let mut rows = Vec::new();
        while let Some((next, _, _)) = blocks.peek() {
            if !next.key.as_str().starts_with(CARD_PREFIX) {
                break;
            }
            let (mut row, row_height, _) = blocks.next().expect("peeked");
            row.layout.width = Length::Fixed(row_width);
            rows.push((row, row_height));
        }
        if rows.is_empty() {
            items.push(ListItem::Card(Box::new(node), height));
            continue;
        }
        items.push(ListItem::Split(scroll::SplitCard {
            key: format!("settings/showcase/section-rows/{word}"),
            spec: GroupSpec::unlabeled(SemanticRole::Group),
            padding: Insets::all(0.0),
            gap: 0.0,
            head: vec![(node, height)],
            rows,
        }));
    }
    items
}

fn sheet_panel(
    state: &SettingsViewState,
    panel_width: f32,
    height: f32,
    lead: Vec<(UiNode, f32)>,
) -> UiNode {
    let inner = (panel_width - 2.0 * PANEL_INSET).max(0.0);
    let title_height = field_height();
    let pinned_room = sheet_list_room(height);
    let pin_title = lead.is_empty()
        && pinned_room + 0.5 >= sheet_list_least(state, (inner - THUMB_GUTTER).max(0.0));
    let room = if pin_title {
        pinned_room
    } else {
        (height - 2.0 * PANEL_INSET).max(0.0)
    };
    let gap = PANEL_GAP;
    let label = SHEET_TITLE.to_string();
    let title = |width: f32| {
        line(
            "settings/showcase/sheet-title",
            sheet_title_words(width),
            SemanticRole::Heading,
            StyleRef::Plain,
            width,
            title_height,
        )
        .paint_only()
    };
    let after = DEMO_STRIP_GAP - gap;
    let head = |width: f32| -> Vec<ListItem> {
        let mut head: Vec<ListItem> = lead
            .iter()
            .cloned()
            .map(|(node, height)| lead_item(node, height, after, width))
            .collect();
        if !pin_title {
            let mut title = title(width);
            title.layout.width = Length::Fixed(width);
            head.push(ListItem::Card(Box::new(title), title_height));
        }
        head
    };
    let head_span: f32 = lead
        .iter()
        .map(|(_, height)| height + after + gap)
        .sum::<f32>()
        + if pin_title { 0.0 } else { title_height + gap };
    let one_line = colour_rows_on_one_line((inner - THUMB_GUTTER).max(0.0));
    let probe = shared_sheet_blocks(state, inner, room, one_line);
    let span: f32 = head_span + probe.iter().map(|(_, height)| height + gap).sum::<f32>() - gap;
    let scrolls = span > room + 0.01;
    let (width, blocks) = if scrolls {
        let width = inner - THUMB_GUTTER;
        (width, shared_sheet_blocks(state, width, room, one_line))
    } else {
        (inner, probe)
    };
    let items: Vec<ListItem> = head(width)
        .into_iter()
        .chain(blocks.into_iter().map(|(mut node, height)| {
            if scrolls {
                node.layout.padding.right += THUMB_GUTTER;
            }
            ListItem::of(node, height)
        }))
        .collect();
    let list = scroll::list_page_whole(
        state,
        "settings/showcase/sheet-list",
        &label,
        items,
        room,
        gap,
    );
    let mut children = Vec::with_capacity(2);
    if pin_title {
        children.push(title(inner));
    }
    children.push(list);
    UiNode::new(
        "settings/showcase/sheet",
        UiContent::Group(GroupSpec::new(label.clone()).style(StyleRef::Secondary)),
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

fn sheet_floor(state: &SettingsViewState, panel_width: f32) -> f32 {
    let rows = (panel_width - 2.0 * PANEL_INSET - THUMB_GUTTER).max(0.0);
    2.0 * PANEL_INSET + sheet_list_least(state, rows).max(field_height())
}

fn transparent_sections(node: &mut UiNode) {
    if node
        .key
        .as_str()
        .starts_with("settings/showcase/section-rows/")
    {
        node.paint_only = true;
    }
    for child in &mut node.children {
        transparent_sections(child);
    }
}

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
        UiContent::Group(GroupSpec::new("Dark or light theme")),
    )
    .layout(Layout::row().height(Length::Fixed(field_height())).gap(4.0))
    .children(vec![
        segment(aterm_types::Appearance::Dark),
        segment(aterm_types::Appearance::Light),
    ])
}

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

fn footer_rows(state: &SettingsViewState, width: f32, narrow: bool) -> Vec<(UiNode, f32)> {
    let height = field_height();
    let following = prefs::theme_mode(state.raw_value(prefs::EDIT_THEME).as_deref())
        == prefs::ThemeMode::FollowSystem;
    let switch_width = 52.0 * settings_text_scale().min(1.4);
    let visible = if narrow {
        "Follow system"
    } else {
        FOLLOW_SYSTEM_LABEL
    };
    let switch = UiNode::new(
        FOLLOW_SYSTEM,
        UiContent::Switch(
            Control::new(
                SwitchSpec {
                    label: FOLLOW_SYSTEM_LABEL.to_string(),
                    description: Some(
                        "Use one theme in Dark mode and another in Light mode".to_string(),
                    ),
                },
                ActionId::new(FOLLOW_SYSTEM),
            )
            .value(SemanticValue::Bool(following))
            .state(ControlState {
                focused: focused(state, FOLLOW_SYSTEM),
                ..ControlState::default()
            })
            .style(StyleRef::Quiet),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(switch_width))
            .height(Length::Fixed(height)),
    );
    let beside = (width - TEXT_INSET - switch_width - 6.0).max(0.0);
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Plain);
    let measure = |text: &str| crate::tray_raster::ui_text_width_for(face, text, px);
    let over = !pieces_fit(visible, beside, &measure);
    let (label, label_height) = wrapped_words_node(
        "settings/showcase/footer/follow-label",
        visible,
        (SemanticRole::Text, StyleRef::Plain),
        if over {
            (width - TEXT_INSET).max(0.0)
        } else {
            beside
        },
        footer_line_height(),
    );
    let (follow_height, layout) = if over {
        (label_height + 4.0 + height, Layout::column().gap(4.0))
    } else {
        (height.max(label_height), Layout::row().gap(6.0))
    };
    let label = if !over && label_height < height {
        label.layout(Layout::default().width(Length::Fill).height(Length::Fill))
    } else {
        label
    };
    let follow = UiNode::new(
        "settings/showcase/footer/follow",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(layout.height(Length::Fixed(follow_height)).padding(Insets {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: TEXT_INSET,
    }))
    .children(vec![label.paint_only(), switch]);
    let folder_visual = if narrow { "Folder" } else { "Themes folder" };
    let import_visual = if narrow { "Import" } else { "Import\u{2026}" };
    let (links, links_height) = button_lines(
        "settings/showcase/footer/links",
        vec![
            button(
                IMPORT,
                "Import\u{2026}",
                import_visual,
                StyleRef::Quiet,
                ControlState {
                    enabled: crate::menu::local_file_picker_available(),
                    focused: focused(state, IMPORT),
                    ..ControlState::default()
                },
                None,
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
                None,
            ),
        ],
        width,
        ButtonLines {
            single: height,
            least: 0.0,
            slack: 1.0,
            gap: 4.0,
        },
    );
    vec![(follow, follow_height), (links, links_height)]
}

fn footer_line_height() -> f32 {
    20.0_f32.max(18.0 * settings_text_scale())
}

fn footer_is_one_line(state: &SettingsViewState, width: f32, narrow: bool) -> bool {
    footer_rows(state, width, narrow)
        .iter()
        .all(|(_, height)| *height <= field_height() + 0.01)
}

/// The list's blocks, in order, each with its height.
pub(in crate::native_settings) fn list_blocks(
    state: &SettingsViewState,
    width: f32,
    narrow: bool,
) -> Vec<(UiNode, f32, Option<String>)> {
    list_blocks_for(state, width, narrow, &state.showcase.query())
}

/// The chooser's rows at a width, narrow or not, under a query, and the tallest of them.
pub(in crate::native_settings) struct SharedBlocks {
    at: (u32, bool, String),
    blocks: Vec<(UiNode, f32, Option<String>)>,
    tallest: Option<f32>,
}

/// THE PAGE'S SEATS SHARE THE CHOOSER'S ROWS: while open, each width's rows are built once a render.
pub(in crate::native_settings) struct BlocksShared<'a>(&'a SettingsViewState);

impl<'a> BlocksShared<'a> {
    pub(in crate::native_settings) fn open(state: &'a SettingsViewState) -> Self {
        state.showcase_blocks.replace(Some(Vec::new()));
        state.showcase_sheet.replace(Some(Vec::new()));
        Self(state)
    }
}

impl Drop for BlocksShared<'_> {
    fn drop(&mut self) {
        self.0.showcase_blocks.replace(None);
        self.0.showcase_sheet.replace(None);
    }
}

/// The Customize sheet's rows at a width, in a room, colour rows on one line or not.
pub(in crate::native_settings) type SharedSheet = (SheetAt, Vec<(UiNode, f32)>);

/// What the sheet's rows are built at: the width, the room where an open picker reads it, colour
/// rows on one line or not, and what the seat lays setting rows out by.
type SheetAt = (u32, Option<u32>, bool, (Option<u32>, usize));

/// [`sheet_blocks`], built once a render for each width and room while [`BlocksShared`] is open.
fn shared_sheet_blocks(
    state: &SettingsViewState,
    width: f32,
    room: f32,
    one_line: bool,
) -> Vec<(UiNode, f32)> {
    let at = (
        width.to_bits(),
        sheet_sized_by_room(state).then_some(room.to_bits()),
        one_line,
        crate::native_settings::seat_row_rooms(),
    );
    let built = state
        .showcase_sheet
        .borrow()
        .as_ref()
        .and_then(|shared| shared.iter().find(|(seen, _)| *seen == at))
        .map(|(_, blocks)| blocks.clone());
    built.unwrap_or_else(|| {
        let blocks = sheet_blocks(state, width, room, one_line);
        if let Some(shared) = state.showcase_sheet.borrow_mut().as_mut() {
            shared.push((at, blocks.clone()));
        }
        blocks
    })
}

/// `read` of the rows [`list_blocks_for`] builds, shared while [`BlocksShared`] is open.
fn with_blocks<R>(
    state: &SettingsViewState,
    (width, narrow, query): (f32, bool, &str),
    read: impl FnOnce(&mut SharedBlocks) -> R,
) -> R {
    let at = (width.to_bits(), narrow, query.to_string());
    let build = |at| SharedBlocks {
        blocks: build_list_blocks(state, width, narrow, query),
        at,
        tallest: None,
    };
    let mut shared = state.showcase_blocks.borrow_mut();
    let Some(shared) = shared.as_mut() else {
        return read(&mut build(at));
    };
    let index = match shared.iter().position(|entry| entry.at == at) {
        Some(index) => index,
        None => {
            shared.push(build(at));
            shared.len() - 1
        }
    };
    read(&mut shared[index])
}

fn list_blocks_for(
    state: &SettingsViewState,
    width: f32,
    narrow: bool,
    query: &str,
) -> Vec<(UiNode, f32, Option<String>)> {
    with_blocks(state, (width, narrow, query), |entry| entry.blocks.clone())
}

fn build_list_blocks(
    state: &SettingsViewState,
    width: f32,
    narrow: bool,
    query: &str,
) -> Vec<(UiNode, f32, Option<String>)> {
    let showcase = &state.showcase;
    let slab = slab_column(state, width, narrow);
    let tags = TagWidths::now();
    let mut blocks: Vec<(UiNode, f32, Option<String>)> = Vec::new();
    let reach = showcase.reach_for(query);
    let attention: Vec<&AttentionRow> = showcase
        .attention
        .iter()
        .filter(|row| super::attention_row_matches(row, query, reach))
        .collect();
    if !attention.is_empty() {
        blocks.push(section_heading(
            Section::Attention,
            Section::Attention.caption().unwrap_or_default(),
            false,
        ));
        for row in attention {
            blocks.push((
                attention_row(state, row, width),
                attention_row_height(),
                None,
            ));
        }
    }
    let visible = showcase.visible_for(query);
    let community_total = showcase
        .cards
        .iter()
        .filter(|card| card.section == Section::Community)
        .count();
    let configured = state.configured_theme_id();
    let highlighted = state.highlighted_slug();
    let mut section = None;
    let mut rows_before = !blocks.is_empty();
    for index in &visible {
        let card = &showcase.cards[*index];
        if section != Some(card.section) {
            section = Some(card.section);
            if card.section == Section::Community && query.is_empty() {
                blocks.push(pack_row(state, community_total, rows_before, width));
            } else if let Some(caption) = card.section.caption_after_rows(rows_before) {
                blocks.push(section_heading(card.section, caption, rows_before));
            }
        }
        let in_use = configured.eq_ignore_ascii_case(&card.id);
        let selected = highlighted.as_deref() == Some(card.slug.as_str());
        let (row, height) = row_node(state, card, (width, narrow, slab, tags), in_use, selected);
        blocks.push((row, height, Some(card.slug.clone())));
        rows_before = true;
    }
    if section != Some(Section::Community) && community_total > 0 && query.is_empty() {
        blocks.push(pack_row(state, community_total, rows_before, width));
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

fn section_heading(
    section: Section,
    caption: &str,
    after_rows: bool,
) -> (UiNode, f32, Option<String>) {
    let above = if after_rows { SECTION_ABOVE } else { 0.0 };
    let height = caption_height() + above;
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

fn pack_row(
    state: &SettingsViewState,
    count: usize,
    after_rows: bool,
    width: f32,
) -> (UiNode, f32, Option<String>) {
    let above = if after_rows { SECTION_ABOVE } else { 0.0 };
    let open = state.showcase.pack_open;
    let mut spec = ButtonSpec::new("Community");
    spec.description = Some(format!(
        "{}: tried without a file, added to your themes folder on Keep",
        count_noun(
            count,
            "theme from the theme pack",
            "themes from the theme pack"
        )
    ));
    let control = Control::new(spec, ActionId::new(PACK))
        .state(ControlState {
            expanded: Some(open),
            focused: focused(state, PACK),
            ..ControlState::default()
        })
        .style(StyleRef::Plain);
    let lines = wrap_to_box(
        "Community",
        crate::native_ui::disclosure_label_room(width),
        &crate::native_ui::native_button_label_width,
    );
    let height = crate::native_ui::native_button_height_for_lines(lines.len(), row_height());
    let words = lines.join("\n");
    let mut control = control;
    control.spec.visual_label = Some(words);
    let mut node = UiNode::new(PACK, UiContent::Button(control)).layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(height)),
    );
    if above > 0.0 {
        node = UiNode::new(
            "settings/showcase/pack-row",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::column()
                .height(Length::Fixed(height + above))
                .padding(Insets {
                    top: above,
                    right: 0.0,
                    bottom: 0.0,
                    left: 0.0,
                }),
        )
        .children(vec![node]);
    }
    (node, height + above, None)
}

fn row_node(
    state: &SettingsViewState,
    card: &ShowcaseCard,
    (width, narrow, slab, tags): (f32, bool, Option<f32>, TagWidths),
    in_use: bool,
    selected: bool,
) -> (UiNode, f32) {
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
    let typed = card.typography.is_some();
    let low =
        contrast_grade(card.contrast) == crate::settings_preview::theme_card::ContrastGrade::Low;
    let tag = |suffix: &str, text: &str, style: StyleRef, tag_width: f32| {
        UiNode::new(
            format!("{key}/{suffix}"),
            UiContent::Text(TextSpec {
                text: text.to_string(),
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
    let TagWidths {
        aa: aa_width,
        low: low_width,
        check: check_width,
    } = tags;
    let others = [
        low.then_some(low_width),
        yours.then_some(2.0 * ICON_BUTTON + 4.0),
        slab,
        Some(check_width),
    ]
    .into_iter()
    .flatten()
    .map(|width| width + 6.0)
    .sum::<f32>();
    let name_room = width - TEXT_INSET - ROW_END - others;
    let name_fits_beside_tag = || {
        fit_text(
            &card.label,
            SemanticRole::Text,
            StyleRef::Primary,
            name_room - aa_width - 6.0,
        ) == card.label
    };
    if typed && (!yours || name_fits_beside_tag()) {
        trailing.push(tag("aa", "Aa", StyleRef::Quiet, aa_width));
    }
    if low {
        trailing.push(tag("low", "\u{26a0}", StyleRef::Quiet, low_width));
    }
    if yours {
        trailing.push(spacer(&format!("{key}/icons"), 2.0 * ICON_BUTTON + 4.0));
    }
    let slab_pad = ((height - SLAB_HEIGHT) / 2.0).max(0.0);
    trailing.extend(slab.map(|slab_width| {
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
        ])
    }));
    trailing.push(tag(
        "check",
        if in_use { "\u{2713}" } else { "" },
        StyleRef::Accent,
        check_width,
    ));
    let trailing_width: f32 = trailing
        .iter()
        .map(|node| match node.layout.width {
            Length::Fixed(width) => width + 6.0,
            _ => 0.0,
        })
        .sum();
    let mut name_width = (width - TEXT_INSET - ROW_END - trailing_width).max(0.0);
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Primary);
    let measure = |text: &str| crate::tray_raster::ui_text_width_for(face, text, px);
    if !in_use && !pieces_fit(&card.label, name_width, &measure) {
        trailing.pop();
        name_width += check_width + 6.0;
    }
    let (name, lines_height) = wrapped_words_node(
        &format!("{key}/name"),
        &card.label,
        (SemanticRole::Text, StyleRef::Primary),
        name_width,
        row_name_line_height(),
    );
    let one_line = matches!(name.content, UiContent::Text(_));
    let name = if one_line {
        name.layout(Layout::default().width(Length::Fill).height(Length::Fill))
    } else {
        UiNode::new(
            format!("{key}/name-frame"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::column()
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(Insets {
                    top: ROW_NAME_PAD,
                    ..Insets::default()
                }),
        )
        .children(vec![name])
    };
    let mut face_children = vec![name];
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
        children.push(file_actions(state, card, &key, slab));
    }
    let height = if one_line {
        height
    } else {
        height.max(lines_height + 2.0 * ROW_NAME_PAD)
    };
    (
        UiNode::new(key.clone(), option)
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(height)),
            )
            .children(children),
        height,
    )
}

fn row_name_line_height() -> f32 {
    22.0_f32.max(20.0 * settings_text_scale())
}

const ROW_NAME_PAD: f32 = 6.0;

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

fn file_actions(
    state: &SettingsViewState,
    card: &ShowcaseCard,
    key: &str,
    slab: Option<f32>,
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
    let after = slab.map_or(0.0, |slab| slab + 6.0) + check_width() + 6.0 + ROW_END;
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

fn attention_row_height() -> f32 {
    44.0_f32.max(40.0 * settings_text_scale())
}

fn attention_row(state: &SettingsViewState, row: &AttentionRow, width: f32) -> UiNode {
    let key = format!("{CARD_PREFIX}{}", row.slug);
    let open_key = format!("{key}/{CARD_OPEN}");
    let height = attention_row_height();
    let pad = ((height - ICON_BUTTON) / 2.0).max(0.0);
    let line_height = ((height - 8.0) / 2.0).max(0.0);
    let text_width = (width - TEXT_INSET - 6.0 - ICON_BUTTON - 8.0).max(0.0);
    let text_line = |suffix: &str, text: &str, role: SemanticRole, style: StyleRef| {
        UiNode::new(
            format!("{key}/{suffix}"),
            UiContent::Text(TextSpec {
                text: fit_text(text, role, style, text_width),
                role,
                style,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(line_height)),
        )
        .paint_only()
    };
    let lines = UiNode::new(
        format!("{key}/text"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .width(Length::Fixed(text_width))
            .height(Length::Fill)
            .padding(Insets {
                top: 4.0,
                right: 0.0,
                bottom: 4.0,
                left: 0.0,
            }),
    )
    .children(vec![
        text_line("name", &row.id, SemanticRole::Text, StyleRef::Primary),
        text_line(
            "reason",
            &row.summary,
            SemanticRole::Status,
            StyleRef::Danger,
        ),
    ])
    .paint_only();
    UiNode::new(
        key.clone(),
        UiContent::Group(GroupSpec::new(format!("{}, {}", row.id, row.reason))),
    )
    .layout(
        Layout::row()
            .height(Length::Fixed(height))
            .gap(6.0)
            .padding(Insets {
                top: 0.0,
                right: 8.0,
                bottom: 0.0,
                left: TEXT_INSET,
            }),
    )
    .children(vec![
        lines,
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
