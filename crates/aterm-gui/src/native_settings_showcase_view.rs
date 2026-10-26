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

pub(in crate::native_settings) fn row_height() -> f32 {
    32.0_f32.max(28.0 * settings_text_scale())
}

/// THE CHOOSER'S LIST IS ONE GRID: a stop `height` tall stands on the whole rows
/// ([`row_height`]) that hold it (a theme row one, a section's caption band one, the Community
/// disclosure one, a NEEDS ATTENTION row or a name on two lines as many as they take; a footer
/// row is a field's height, one row), so the room, cut to whole rows ([`whole_rows_height`]),
/// holds whole stops to its edge on the last screen too. (A 40pt Community disclosure under
/// 32pt rows left the list's end 24pt short of the room's edge at every size at 1×, and a 44pt
/// caption band against 56pt rows 32pt to 48pt at 2×.)
fn on_grid(height: f32) -> f32 {
    let row = row_height();
    ((height - 0.5) / row).ceil().max(1.0) * row
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

/// The panel `room` holds cut to whole rows under what it pins (`pins`); one too short for its
/// pinned head over a row lists the head too ([`chooser_panel`]), its rows still whole, so no
/// screen of the list ends short (98pt of 56pt rows at 2× in an 80x24 window: every screen 42pt
/// short).
fn whole_rows_height(state: &SettingsViewState, room: f32, pins: ChooserPins) -> f32 {
    let pins = if list_rows(state, room, pins) < 1.0 {
        ChooserPins::NONE
    } else {
        pins
    };
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

thread_local! {
    /// What [`list_room`] measured of the page laid out, while it measures one.
    static MEASURING: Cell<Option<Option<ListRoom>>> = const { Cell::new(None) };
    /// How the page built last laid its list out ([`laid_last`]).
    static LAID: Cell<Option<ListRoom>> = const { Cell::new(None) };
    /// The rows [`list_laid`] laid its list out with, while it lays one out.
    static LAYING: RefCell<Option<Option<scroll::ListLaid>>> = const { RefCell::new(None) };
    /// Whether the page measured pins the keyboard's control around its list ([`pin`]).
    static KEYBOARD_PINNED: Cell<bool> = const { Cell::new(false) };
    /// The lists' items built this render, while [`BlocksShared`] is open ([`shared_items`]).
    static ITEMS: RefCell<Option<Vec<SharedItems>>> = const { RefCell::new(None) };
}

/// A list's items built this render and what they were built of.
type SharedItems = (ItemsAt, std::rc::Rc<[ListItem]>);

/// What a list's items are built of: the chooser's or the sheet's rows, and how they are held.
#[derive(PartialEq)]
enum ItemsAt {
    Chooser(
        (u32, bool, String, bool),
        (bool, bool, bool),
        Vec<(UiKey, u32)>,
    ),
    Sheet((usize, u32), (bool, bool), Vec<(UiKey, u32)>),
}

/// The lead's nodes as [`ItemsAt`] tells them apart.
fn lead_at(lead: &[(UiNode, f32)]) -> Vec<(UiKey, u32)> {
    lead.iter()
        .map(|(node, height)| (node.key.clone(), height.to_bits()))
        .collect()
}

/// The items `build` builds at `at`, built once a render while [`BlocksShared`] is open.
fn shared_items(at: ItemsAt, build: impl FnOnce() -> Vec<ListItem>) -> std::rc::Rc<[ListItem]> {
    let known = ITEMS.with(|items| {
        items.borrow().as_ref().and_then(|items| {
            items
                .iter()
                .find(|(seen, _)| *seen == at)
                .map(|(_, built)| std::rc::Rc::clone(built))
        })
    });
    known.unwrap_or_else(|| {
        let built: std::rc::Rc<[ListItem]> = build().into();
        ITEMS.with(|items| {
            if let Some(items) = items.borrow_mut().as_mut() {
                items.push((at, std::rc::Rc::clone(&built)));
            }
        });
        built
    })
}

/// How the page built since this was last asked laid its list out, as [`list_room`] measures it.
pub(in crate::native_settings) fn laid_last() -> Option<ListRoom> {
    LAID.with(Cell::take)
}

/// WHAT A PAGE LAYS ITS LIST OUT IN, measured unseated: two seats alike build the same list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native_settings) struct ListRoom {
    panel: (f32, f32),
    held: (bool, bool, usize),
    room: f32,
    extra: f32,
    keyboard_pinned: bool,
}

impl ListRoom {
    /// The room the list's stops are seated in ([`scroll::seat_room_under_status`]).
    pub(in crate::native_settings) fn stops(self) -> f32 {
        (self.room - self.extra).max(0.0)
    }

    /// The room the list is seated in, before a status's bar takes any of it.
    pub(in crate::native_settings) const fn room(self) -> f32 {
        self.room
    }

    /// Whether the keyboard's control is pinned around the list, so the list never cuts it.
    pub(in crate::native_settings) const fn pins_keyboard(self) -> bool {
        self.keyboard_pinned
    }

    /// Whether the list lays its stops out as `other` does: as wide, pinned and led the same.
    pub(in crate::native_settings) fn stops_as(self, other: Self) -> bool {
        self.panel.0 == other.panel.0 && self.held == other.held
    }
}

/// How [`themes_page`] lays its list out at the chrome and bars `state` holds: its sizes, measured,
/// with no row built.
pub(in crate::native_settings) fn list_room(
    state: &SettingsViewState,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> Option<ListRoom> {
    #[cfg(test)]
    crate::work_counts::settings_list_measured();
    KEYBOARD_PINNED.with(|pinned| pinned.set(false));
    MEASURING.with(|measuring| measuring.set(Some(None)));
    let _ = themes_page(state, width, cx);
    MEASURING.with(Cell::take).flatten()
}

/// [`list_room`] with the rows a seat there seats its list with, laid out with no page built.
pub(in crate::native_settings) fn list_laid(
    state: &SettingsViewState,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> (Option<ListRoom>, Option<scroll::ListLaid>) {
    LAYING.with(|laying| laying.replace(Some(None)));
    let room = list_room(state, width, cx);
    (room, LAYING.with(RefCell::take).flatten())
}

fn measuring() -> bool {
    MEASURING.with(Cell::get).is_some()
}

/// Whether the list measured is laid out too ([`list_laid`]).
fn laying() -> bool {
    LAYING.with(|laying| laying.borrow().is_some())
}

/// Keep `list`, the rows [`list_laid`] lays its list out with.
fn lay(list: scroll::ListLaid) {
    LAYING.with(|laying| {
        if let Some(laid) = laying.borrow_mut().as_mut() {
            *laid = Some(list);
        }
    });
}

/// Note `node`, pinned around the list, and whether it holds the keyboard's control.
fn pin(state: &SettingsViewState, node: &UiNode) {
    fn holds(node: &UiNode, key: &UiKey) -> bool {
        node.key == *key || node.children.iter().any(|child| holds(child, key))
    }
    if measuring()
        && state
            .common
            .last_focus
            .as_ref()
            .is_some_and(|key| holds(node, key))
    {
        KEYBOARD_PINNED.with(|pinned| pinned.set(true));
    }
}

/// Whether the page is only measured ([`list_room`]), its list laid out as `room` says, which a
/// page built keeps for [`laid_last`].
fn measured(state: &SettingsViewState, room: ListRoom) -> bool {
    let room = ListRoom {
        extra: state.status_extra.get(),
        keyboard_pinned: measuring() && KEYBOARD_PINNED.with(Cell::get),
        ..room
    };
    MEASURING.with(|measuring| {
        if measuring.get().is_some() {
            measuring.set(Some(Some(room)));
            return true;
        }
        LAID.with(|laid| laid.set(Some(room)));
        false
    })
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
    let (mut out, mut used) = pinned_heading(state, width, viewport, gap, SHOWCASE_TITLE, "");
    let body = settings_list_body_height(state, width, viewport);
    // Its chooser lists in a panel beside or under the demo, so the page's name, where nothing
    // else names it, is pinned over the page where the body still holds the chooser's filter
    // under it, as short windows shed chrome before content; else the chooser's list leads with
    // it wherever it lists its filter too ([`chooser_panel`]).
    // The toolbar, where it leads the page's list ([`super::compact_toolbar_lists`]), leads the
    // chooser's list only where that list leads with the demo and strip too, nothing pinned over
    // it: anywhere else it stays over the page, and the page's name with it.
    let mut head = state.take_list_head();
    let toolbar = head
        .iter()
        .position(|(node, _)| super::is_compact_toolbar(node))
        .map(|at| head.remove(at));
    state.toolbar_head_taken.set(false);
    let mut name = head.pop();
    // Pinned, the name leaves the chooser its filter and its tallest row under it
    // ([`chooser_floor`]): at 1× text in a 178x152 window a name pinned over a 58pt list left a
    // two-line row (64pt) no screen showed whole.
    let floor = || {
        let rows = chooser_rows_tallest(
            state,
            (page_width - 2.0 * PANEL_INSET).max(0.0),
            chooser_is_narrow(page_width),
        );
        chooser_floor(state, rows)
    };
    if let Some((node, height)) = name.take_if(|(_, height)| {
        let filter = 2.0 * PANEL_INSET
            + chooser_pinned_height(
                state,
                ChooserPins {
                    head: true,
                    footer: false,
                },
            );
        toolbar.is_none() && body - used - *height - gap >= filter.max(floor()) - 0.01
    }) {
        out.push(node);
        used += height + gap;
    }
    let name = name.is_some().then_some(SHOWCASE_TITLE);
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
            // The strip stands over a status's bar: the demo gives up what the bar
            // covers of the column, and the bar shortens for a strip control it would
            // still cover.
            let cover = (state.status_extra.get() - (body - room)).max(0.0);
            let demo_height = (room - cover - DEMO_STRIP_GAP - strip).max(0.0);
            state.note_pinned_status_room(&strip_box, body - DEMO_STRIP_GAP - strip);
            pin(state, &strip_box);
            if measuring() {
                let _ = if state.showcase.sheet_open {
                    sheet_panel(state, chooser, room, (Vec::new(), None))
                } else {
                    chooser_panel(state, chooser, room, shape, (Vec::new(), None), name)
                };
                return out;
            }
            let left = if state.showcase.sheet_open {
                sheet_panel(state, chooser, room, (Vec::new(), None))
            } else {
                chooser_panel(state, chooser, room, shape, (Vec::new(), None), name)
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
                    let first = match lead_item(strip_box.clone(), strip, 0.0, inner) {
                        ListItem::Split(card) => card.first_stop_height(list_room),
                        ListItem::Card(_, height) => height,
                    };
                    // The demo is sized for the screen the toolbar leaves it under it.
                    let over = toolbar
                        .as_ref()
                        .map_or(0.0, |(_, height)| height + DEMO_STRIP_GAP);
                    let sizing = LeadSizing::new(frame, first, over, list_room);
                    state.toolbar_head_taken.set(toolbar.is_some());
                    let lead = toolbar
                        .into_iter()
                        .chain([
                            (demo_node(spec, sizing.chosen), sizing.chosen),
                            (strip_box, strip),
                        ])
                        .collect();
                    out.push(if state.showcase.sheet_open {
                        sheet_panel(state, page_width, room, (lead, Some(sizing)))
                    } else {
                        chooser_panel(
                            state,
                            page_width,
                            room,
                            Shape::Stacked,
                            (lead, Some(sizing)),
                            name,
                        )
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
                    // A panel that holds its pinned filter over its tallest row keeps that room;
                    // one that does not lists the filter, its rows whole, at least its floor.
                    let panel_height = if rows.is_none() || panel_height + 0.5 >= least {
                        whole_rows_height(state, panel_height, pins)
                            .max(least)
                            .min(panel_height)
                    } else {
                        whole_rows_height(state, panel_height, ChooserPins::NONE)
                            .max(floor)
                            .min(panel_height)
                    };
                    state.note_pinned_status_room(&strip_box, panel_height);
                    pin(state, &strip_box);
                    out.push(strip_box);
                    out.push(if state.showcase.sheet_open {
                        sheet_panel(state, page_width, panel_height, (Vec::new(), None))
                    } else {
                        chooser_panel(
                            state,
                            page_width,
                            panel_height,
                            Shape::Stacked,
                            (Vec::new(), None),
                            name,
                        )
                    });
                }
            }
        }
    }
    out
}

/// How the demo that leads a stacked list over the strip is sized by the list's room: the demo's
/// heights by its rows, the height of the strip's first stop (its words over its first line of
/// buttons, as the list lists it), what leads the list over the demo (the toolbar where it
/// leads the list, and its gap), and the height chosen.
#[derive(Clone, Copy)]
struct LeadSizing {
    heights: [f32; 6],
    strip: f32,
    over: f32,
    chosen: f32,
}

impl LeadSizing {
    fn new(frame: &SettingsPreviewSpec, strip: f32, over: f32, list_room: f32) -> Self {
        let mut sizing = Self {
            heights: std::array::from_fn(|rows| frame.demo_height(rows + 1)),
            strip,
            over,
            chosen: 0.0,
        };
        sizing.chosen = sizing.demo_in(list_room);
        sizing
    }

    /// The tallest demo that leaves the strip's first stop a place beside it in a list
    /// `list_room` tall under what leads it; where even one row leaves it none, the tallest that
    /// list's screen holds, so the first screen is never one row of demo over a blank; else one
    /// row's.
    fn demo_in(self, list_room: f32) -> f32 {
        let list_room = (list_room - self.over).max(0.0);
        let tallest = |under: f32| {
            self.heights
                .iter()
                .rev()
                .copied()
                .find(|&height| height + under <= list_room + 0.01)
        };
        tallest(DEMO_STRIP_GAP + self.strip + DEMO_STRIP_GAP)
            .or_else(|| tallest(DEMO_STRIP_GAP))
            .unwrap_or(self.heights[0])
    }

    /// Whether a list `list_room` tall is led by the same demo.
    fn alike_in(sizing: Option<Self>, list_room: f32) -> bool {
        sizing.is_none_or(|sizing| sizing.demo_in(list_room) == sizing.chosen)
    }
}

/// Rows built over the page that lead a panel's list, and how they are sized by its room.
type Lead = (Vec<(UiNode, f32)>, Option<LeadSizing>);

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

/// What [`build_list_blocks`] takes for a query to list every row the list can list.
const EVERY_ROW: &str = "\u{0}every row";

/// The tallest stop of the chooser's rows in a panel `inner` wide, listed with no captions, each a
/// stop of its own: at `inner`, and where the list scrolls at the width its thumb's gutter leaves
/// it, which wraps a name a line taller (Catppuccin Mocha in a 244pt window), so a pinned filter
/// never leaves a room one row tall over a row two rows tall, drawn as no row at all; and of every
/// row the list can list, the closed Community section's too, so opening it, or trying a theme
/// in it, never reshapes the page under the person.
fn chooser_rows_tallest(state: &SettingsViewState, inner: f32, narrow: bool) -> f32 {
    [inner, (inner - THUMB_GUTTER).max(0.0)]
        .into_iter()
        .map(|width| {
            with_blocks(state, (width, narrow, EVERY_ROW), false, |entry| {
                entry
                    .blocks
                    .iter()
                    .map(|(_, height, _)| *height)
                    .fold(0.0, f32::max)
            })
        })
        .fold(0.0, f32::max)
}

/// A head field's height where the chooser lists it: a row less the gap under it.
fn listed_head_field_height() -> f32 {
    (row_height() - PANEL_GAP).max(0.0)
}

fn chooser_tallest_stop(state: &SettingsViewState, pins: ChooserPins, rows: f32) -> f32 {
    let head = if pins.head || chooser_head_heights(state).is_empty() {
        0.0
    } else {
        listed_head_field_height() + PANEL_GAP
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
    blocks: Vec<(UiNode, f32, Option<String>)>,
    width: f32,
    pins: ChooserPins,
    lead: Vec<(UiNode, f32)>,
    captions: bool,
    footer: Option<Vec<(UiNode, f32)>>,
) -> Vec<ListItem> {
    let footer = footer.map(|rows| footer_item(rows, width));
    let head = if pins.head {
        Vec::new()
    } else {
        chooser_head(state, width)
    }
    .into_iter()
    .map(|(mut node, _)| {
        // Listed, the head's fields stand on the list's grid as every stop does ([`on_grid`]):
        // a row each, its gap under it, so a room cut to whole rows holds them.
        let height = listed_head_field_height();
        node.layout.height = Length::Fixed(height);
        gapped(node, height, PANEL_GAP)
    });
    lead.into_iter()
        .map(|(node, height)| lead_item(node, height, DEMO_STRIP_GAP, width))
        .chain(head.map(|(mut node, height)| {
            node.layout.width = Length::Fixed(width);
            ListItem::Card(Box::new(node), height)
        }))
        .chain(section_items(blocks, width, captions))
        .chain(footer)
        .collect()
}

/// The footer's rows as one card, so a row taller than its screen is listed a part at a time.
fn footer_item(rows: Vec<(UiNode, f32)>, width: f32) -> ListItem {
    let rows = rows
        .into_iter()
        .map(|(mut node, height)| {
            node.layout.width = Length::Fixed(width);
            (node, height)
        })
        .collect();
    ListItem::Split(scroll::SplitCard {
        key: "settings/showcase/footer".to_string(),
        spec: GroupSpec::unlabeled(SemanticRole::Group),
        padding: Insets::all(0.0),
        gap: 0.0,
        head: Vec::new(),
        repeat: Vec::new(),
        rows,
    })
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
            repeat: Vec::new(),
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

/// The demo's spec and frame at what they are built at, shared while [`BlocksShared`] is open.
pub(in crate::native_settings) type SharedDemo =
    (DemoAt, Option<(SettingsPreviewSpec, SettingsPreviewSpec)>);

/// What the demo is built at: the phase, motion, type size and scale, and the prepared font.
type DemoAt = (
    u64,
    crate::native_app::ViewMotionCx,
    u32,
    u32,
    Option<usize>,
);

/// [`build_demo_spec_and_frame`], built once a render while [`BlocksShared`] is open.
fn demo_spec_and_frame(
    state: &SettingsViewState,
    phase_ms: u64,
    motion: crate::native_app::ViewMotionCx,
    terminal_font_px: f32,
    terminal_scale: f32,
    prepared_font: Option<&crate::tray_raster::PreparedSemanticFont>,
) -> Option<(SettingsPreviewSpec, SettingsPreviewSpec)> {
    let at = (
        phase_ms,
        motion,
        terminal_font_px.to_bits(),
        terminal_scale.to_bits(),
        prepared_font.map(|font| std::ptr::from_ref(font) as usize),
    );
    let built = state
        .showcase_demo
        .borrow()
        .as_ref()
        .and_then(|shared| shared.iter().find(|(seen, _)| *seen == at))
        .map(|(_, demo)| demo.clone());
    built.unwrap_or_else(|| {
        let demo = build_demo_spec_and_frame(
            state,
            phase_ms,
            motion,
            terminal_font_px,
            terminal_scale,
            prepared_font,
        );
        if let Some(shared) = state.showcase_demo.borrow_mut().as_mut() {
            shared.push((at, demo.clone()));
        }
        demo
    })
}

fn build_demo_spec_and_frame(
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

/// One thing the strip says about what the demo draws, beyond the theme's
/// name ([`StripWords`]).
enum StripFact {
    /// The side of the Follow system pair a trial is for ("for Light").
    Side(&'static str),
    /// The tried theme comes from the community pack.
    Community,
    /// The theme the trial replaces ("was Default").
    Was(String),
    /// The person's colors, drawn on every theme
    /// ("1 custom color on every theme").
    Note(String),
    /// Why the theme cannot be drawn, or why the trial ended.
    Error(String),
}

/// THE STRIP'S WORDS, FROM ONE SET OF FACTS: line 1 paints the theme's name
/// and what follows it there; line 2 paints the facts, where the strip has
/// room for a second line; the strip's Status says one sentence holding the
/// name and every fact once, at every width. (Line 2's words were added to a
/// sentence that already said them wherever line 2 was hidden: "Trying
/// Linen, was Default; was Default" at 80x24.)
struct StripWords {
    /// The theme's name, painted whole ([`strip_title`]).
    name: String,
    /// What line 1 paints after the name: the type the theme sets, or the
    /// custom colors the sheet edits.
    after_name: String,
    /// What line 2 paints, in order ([`painted_facts`]).
    facts: Vec<StripFact>,
    /// The Status's sentence.
    spoken: String,
}

/// The strip's sentence: `headline` and the side a trial is for ("Trying
/// Linen for Light"), the type the theme sets, then every other fact once.
fn spoken_facts(headline: String, type_words: Option<&str>, facts: &[StripFact]) -> String {
    let mut sentence = headline;
    for fact in facts {
        if let StripFact::Side(side) = fact {
            sentence.push_str(&format!(" for {side}"));
        }
    }
    if let Some(type_words) = type_words {
        sentence.push_str(&format!(", {type_words}"));
    }
    for fact in facts {
        match fact {
            StripFact::Side(_) => {}
            StripFact::Community => sentence.push_str(", a Community theme"),
            StripFact::Was(theme) => sentence.push_str(&format!(", was {theme}")),
            StripFact::Note(note) => sentence.push_str(&format!(", {note}")),
            StripFact::Error(error) => sentence.push_str(&format!("; {error}")),
        }
    }
    sentence
}

/// Line 2: the facts in their short forms, ` · ` between them.
fn painted_facts(facts: &[StripFact]) -> String {
    facts
        .iter()
        .map(|fact| match fact {
            StripFact::Side(side) => format!("for {side}"),
            StripFact::Community => "Community".to_string(),
            StripFact::Was(theme) => format!("was {theme}"),
            StripFact::Note(words) | StripFact::Error(words) => words.clone(),
        })
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// The strip and its height at a width, shared while [`BlocksShared`] is open.
pub(in crate::native_settings) type SharedStrip = (u32, (UiNode, f32));

/// [`build_strip_node`], built once a render for each width while [`BlocksShared`] is open.
fn strip_node(state: &SettingsViewState, width: f32) -> (UiNode, f32) {
    let at = width.to_bits();
    let built = state
        .showcase_strip
        .borrow()
        .as_ref()
        .and_then(|shared| shared.iter().find(|(seen, _)| *seen == at))
        .map(|(_, strip)| strip.clone());
    built.unwrap_or_else(|| {
        let strip = build_strip_node(state, width);
        if let Some(shared) = state.showcase_strip.borrow_mut().as_mut() {
            shared.push((at, strip.clone()));
        }
        strip
    })
}

fn build_strip_node(state: &SettingsViewState, width: f32) -> (UiNode, f32) {
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
    let words = if showcase.sheet_open {
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
        let after_name = if overrides > 0 {
            format!(" \u{b7} {}", custom_colors(overrides))
        } else {
            String::new()
        };
        StripWords {
            spoken: format!("Customize {shown}{after_name}"),
            name: shown,
            after_name,
            facts: Vec::new(),
        }
    } else {
        match &showcase.trial {
            ShowcaseTrial::Idle => {
                let note = override_note(&state.showcase_colors());
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
                let headline = showcase
                    .settled
                    .as_ref()
                    .filter(|settled| settled.id.eq_ignore_ascii_case(&configured))
                    .map_or_else(
                        || format!("Using {in_use}"),
                        |settled| settled.words.clone(),
                    );
                let facts: Vec<StripFact> = match &showcase.error {
                    Some(error) => vec![StripFact::Error(error.clone())],
                    None => note.into_iter().map(StripFact::Note).collect(),
                };
                StripWords {
                    spoken: spoken_facts(headline, None, &facts),
                    name: in_use.clone(),
                    after_name: String::new(),
                    facts,
                }
            }
            ShowcaseTrial::Trying { id } | ShowcaseTrial::Keeping { id, .. } => {
                let keeping = matches!(showcase.trial, ShowcaseTrial::Keeping { .. })
                    || showcase.pending_keep.is_some();
                let label = showcase.label_of(id);
                let in_use = showcase.label_of(&state.theme_id_under_trial());
                let card = showcase.card(id);
                let typed = card.and_then(|card| {
                    Some((card.typography.clone()?, card.typography_spoken.clone()))
                });
                let community = card.is_some_and(|card| card.section == Section::Community);
                let trial = state.trial_typography(id);
                let split = trial.keeps_colours_only();
                // What the strip says of the trial, each fact once: the side
                // it is tried for, where it comes from, what it replaces, and
                // the person's colors it is drawn under too (said, so a
                // washed-out demo has its reason in view) — or why it cannot
                // be drawn, in their place.
                let facts: Vec<StripFact> = match &showcase.error {
                    Some(error) => vec![StripFact::Error(error.clone())],
                    None => community
                        .then_some(StripFact::Community)
                        .into_iter()
                        .chain(following.then_some(StripFact::Side(appearance_label(side))))
                        .chain([StripFact::Was(in_use.clone())])
                        .chain(override_note(&state.showcase_colors()).map(StripFact::Note))
                        .collect(),
                };
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
                let after_name = typed
                    .as_ref()
                    .map(|(typography, _)| format!("  Aa {typography}"))
                    .unwrap_or_default();
                let spoken = match &showcase.error {
                    Some(error) => format!("{label}: {error}"),
                    None => {
                        let verb = if keeping { "Keeping" } else { "Trying" };
                        let type_words = typed.as_ref().map(|(visual, spoken)| {
                            spoken.clone().unwrap_or_else(|| format!("type {visual}"))
                        });
                        spoken_facts(format!("{verb} {label}"), type_words.as_deref(), &facts)
                    }
                };
                StripWords {
                    name: label,
                    after_name,
                    facts,
                    spoken,
                }
            }
        }
    };
    let detail = painted_facts(&words.facts);
    let danger = words
        .facts
        .iter()
        .any(|fact| matches!(fact, StripFact::Error(_)));
    // THE STRIP MEASURES ITS LINE, AND NEVER CUTS A NAME: the title sits
    // beside the buttons where every theme's whole name takes at most two
    // lines there, between its words ([`strip_names`], so moving the
    // highlight from theme to theme never changes the strip's form),
    // measured against what every trial shows (Revert, Keep, Keep ▾), so a
    // trial starting never changes it either; where it cannot, the title
    // takes the strip's whole width on its own lines, the name wrapped
    // between its words, and the buttons the lines under it, each no wider
    // than the strip. Either way it keeps as many lines as the longest name
    // takes there. (At 22 columns at 2× Customize took the whole strip and
    // more, so the name was not drawn and the button was cut; Keep ▾ open cut
    // Revert to nothing at 444pt; the name kept only its first word,
    // "GitHub…", the word GitHub Light shares with every GitHub theme, and
    // beside the buttons at 80x24 "GitHub Dark…" stood for GitHub Dark High
    // Contrast, Dimmed and Default alike.)
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
    // Beside a trial's buttons — Keep ▾ only for a theme whose type a Keep
    // can change — or this state's, where they are wider than any trial's
    // (Keep colors only).
    let beside_room = |buttons: f32| (width - buttons - gap).max(0.0);
    let current = span(&widths);
    let beside = strip_names(
        state,
        &in_use,
        if current > trial + 0.01 {
            (beside_room(current), beside_room(current))
        } else {
            (
                beside_room(span(&[hug_width("Revert"), hug_width("Keep")])),
                beside_room(trial),
            )
        },
    );
    let stacked = !buttons.is_empty() && (beside.broken || beside.lines > NAME_LINES_BESIDE);
    let (title_room, name_lines) = if stacked {
        (
            width,
            strip_names(state, &in_use, (width, width))
                .lines
                .min(NAME_LINES_MOST),
        )
    } else {
        ((width - span(&widths) - gap).max(0.0), beside.lines)
    };
    let detail_line = width >= DETAIL_LINE_FROM;
    let two_lines = detail_line && !detail.is_empty();
    // Line 1 sits on the buttons' band in every state, so the name never
    // jumps when a trial starts or ends; line 2 takes the band under it. A
    // name some theme takes two lines for keeps their band in every state.
    let line_one = button_height;
    let title_height = if name_lines > 1 {
        line_one.max(name_lines as f32 * strip_name_line_height())
    } else {
        line_one
    };
    // The words keep line 2's band only where a line 2 can show, in every
    // state alike.
    let words_height = title_height
        + if detail_line {
            strip_height() - line_one
        } else {
            0.0
        };
    let mut lines = vec![strip_title(
        &words.name,
        &words.after_name,
        title_room,
        title_height,
        name_lines,
    )];
    if two_lines {
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
    // ONE SENTENCE AT EVERY WIDTH, each fact in it once: line 2 is a painted
    // subset of it, shown where the strip has room, so a hidden line 2 loses
    // nothing and a shown one says nothing twice.
    let spoken = words.spoken;
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
        let height = strip_height().max(words_height);
        let under = (height - button_height).max(0.0);
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

/// What the strip lays its names out for in a given room ([`strip_names`]).
#[derive(Clone, Copy, Debug, PartialEq)]
struct NamesMeasure {
    /// The most lines a name takes there.
    lines: usize,
    /// Whether some name has a word wider than the room, which only a break
    /// inside it would fit.
    broken: bool,
}

/// The most lines a name takes beside the strip's buttons: more, and the
/// title goes over them, where it has the strip's whole width.
const NAME_LINES_BESIDE: usize = 2;

/// The most lines the strip keeps for a name over its buttons: every theme
/// aterm ships is whole in them down to a 22-column window at 2× text
/// ("Catppuc-" / "cin" / "Mocha"), and every pack theme from 30 columns; a
/// name that takes more there keeps its first lines whole and its last cut
/// between words. A taller strip pushes the chooser's own rows screens
/// down, the strip's lines blank for the shorter names.
const NAME_LINES_MOST: usize = 3;

/// A name's lines wrapped to `width` in the strip's title face: between its
/// words, inside one only where that word alone is wider than `width`.
fn strip_name_lines(name: &str, width: f32) -> Vec<String> {
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Heading, StyleRef::Plain);
    wrap_to_box(name, width, &|line| {
        crate::tray_raster::ui_text_width_for(face, line, px)
    })
}

/// THE STRIP'S NAMES, every one it can show — each row's (a trial's title)
/// and `in_use`'s (the theme the configuration draws, which a row may not
/// name) — in the room its title has: `plain` beside a trial's Revert and
/// Keep, `typed` beside those and Keep ▾ too, which only a trial that can
/// change the type in force shows (the tried theme sets type, or the theme
/// it replaces does: Keep colors only needs a type change). The most
/// lines a name takes there, and whether a word of one is wider than its
/// room. The strip sits its title beside its buttons only where every name
/// takes a line or two there, whole words each, and keeps as many lines as
/// the longest takes, so neither a trial nor the highlight moving changes
/// its form or its height, and no name is cut ([`strip_node`]). The rows'
/// share is measured once a frame for its rows, text scale and rooms.
fn strip_names(
    state: &SettingsViewState,
    in_use: &str,
    (plain, typed): (f32, f32),
) -> NamesMeasure {
    let showcase = &state.showcase;
    let under_typed = showcase
        .card(&state.theme_id_under_trial())
        .is_some_and(|card| card.typography.is_some());
    let key = (
        std::sync::Arc::as_ptr(&showcase.cards).cast::<()>() as usize,
        settings_text_scale().to_bits(),
        (plain.to_bits(), typed.to_bits(), under_typed),
    );
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Heading, StyleRef::Plain);
    let measure = |name: &str, room: f32| NamesMeasure {
        lines: strip_name_lines(name, room).len().max(1),
        broken: name.split_whitespace().any(|word| {
            crate::tray_raster::ui_text_width_for(face, word, px)
                > room + crate::native_ui::TEXT_FIT_SLACK
        }),
    };
    let most = |a: NamesMeasure, b: NamesMeasure| NamesMeasure {
        lines: a.lines.max(b.lines),
        broken: a.broken || b.broken,
    };
    let cached = showcase.strip_names.borrow().iter().find_map(|entry| {
        ((entry.cards, entry.scale, entry.rooms) == key).then_some(NamesMeasure {
            lines: entry.lines,
            broken: entry.broken,
        })
    });
    let rows = cached.unwrap_or_else(|| {
        let rows = showcase
            .cards
            .iter()
            .map(|card| {
                let room = if under_typed || card.typography.is_some() {
                    typed
                } else {
                    plain
                };
                measure(&card.label, room)
            })
            .fold(
                NamesMeasure {
                    lines: 1,
                    broken: false,
                },
                most,
            );
        let mut entries = showcase.strip_names.borrow_mut();
        entries.retain(|entry| entry.cards == key.0 && entry.scale == key.1);
        // Two rooms a frame (beside the buttons, the strip's width), and a
        // window resized by hand passes through many.
        if entries.len() >= 8 {
            entries.remove(0);
        }
        entries.push(StripNames {
            cards: key.0,
            scale: key.1,
            rooms: key.2,
            lines: rows.lines,
            broken: rows.broken,
        });
        rows
    });
    most(rows, measure(in_use, plain))
}

/// One line of the strip's name where it wraps: two of them are the
/// strip's own height ([`strip_height`]), so two lines beside the buttons
/// cost it nothing where no line 2 shows.
fn strip_name_line_height() -> f32 {
    strip_height() / 2.0
}

/// THE STRIP'S LINE 1, `height` tall: the theme's `name` on `band` lines
/// `room` wide (beside the buttons, or over them), wrapped between its words
/// from the band's top, or on one line where `band` is 1 — whole wherever
/// the band holds it ([`strip_names`] sizes it so), else its last line cut
/// between words — and `after_name` (the type it sets, the custom colors the
/// sheet edits) after it on its last line, fitted to what the name leaves
/// there: cut between its words, and left off where none of it fits, never
/// leaving an ellipsis after a whole name that reads as one cut.
fn strip_title(name: &str, after_name: &str, room: f32, height: f32, band: usize) -> UiNode {
    let key = "settings/showcase/status/title";
    let text = |text: String| {
        UiContent::Text(TextSpec {
            text,
            role: SemanticRole::Heading,
            style: StyleRef::Plain,
        })
    };
    let fit = |words: &str| fit_text(words, SemanticRole::Heading, StyleRef::Plain, room);
    // The name's last line, with what follows the name after it.
    let finish = |last: &str| {
        if after_name.is_empty() {
            return fit(last);
        }
        let whole = format!("{last}{after_name}");
        let fitted = fit(&whole);
        let kept = fitted.trim_end_matches('\u{2026}').trim_end();
        if fitted != whole && kept.len() <= last.trim_end().len() {
            fit(last)
        } else {
            fitted
        }
    };
    if band <= 1 {
        return UiNode::new(key, text(finish(name)))
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(height)),
            )
            .paint_only();
    }
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Heading, StyleRef::Plain);
    let mut lines = wrap_to_box_from(
        name,
        room,
        &|line| crate::tray_raster::ui_text_width_for(face, line, px),
        "-",
    );
    if lines.len() > band {
        // The band's last line takes the rest of the name, cut to it.
        let (at, _) = lines[band - 1];
        lines.truncate(band - 1);
        lines.push((at, name[at..].to_string()));
    }
    let count = lines.len();
    let line_height = strip_name_line_height();
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fixed(height)),
    )
    .children(
        lines
            .into_iter()
            .enumerate()
            .map(|(index, (_, line))| {
                let line = if index + 1 == count {
                    finish(&line)
                } else {
                    line
                };
                UiNode::new(format!("{key}/line-{index}"), text(line))
                    .layout(
                        Layout::default()
                            .width(Length::Fill)
                            .height(Length::Fixed(line_height)),
                    )
                    .paint_only()
            })
            .collect(),
    )
    .paint_only()
}

/// The strip's least width for a second line of words (what the theme
/// replaces, the side it is tried for).
const DETAIL_LINE_FROM: f32 = 400.0;

const STRIP_LINE_GAP: f32 = 4.0;

const fn appearance_label(side: aterm_types::Appearance) -> &'static str {
    match side {
        aterm_types::Appearance::Dark => "Dark",
        aterm_types::Appearance::Light => "Light",
    }
}

/// The theme chooser: its filter and footer pinned where its room holds them over whole rows,
/// else listed, its list led by `lead`, and by the page's name (`name`) where nothing over the
/// page names it and the list leads with its filter too.
fn chooser_panel(
    state: &SettingsViewState,
    panel_width: f32,
    height: f32,
    shape: Shape,
    (lead, sizing): Lead,
    name: Option<&str>,
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
    let footer = footer_rows(state, inner, narrow);
    // The pinned footer is its two rows of a field each ([`footer_height`]): one whose words or
    // links wrap to more rows is listed.
    let footer_pins = footer.len() == 2
        && footer
            .iter()
            .all(|(_, height)| *height <= field_height() + 0.01);
    let tallest_row = chooser_rows_tallest(state, inner, narrow);
    let rows = tallest_row.max(footer.iter().map(|(_, height)| *height).fold(0.0, f32::max));
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
    let lead: Vec<(UiNode, f32)> = match name.filter(|_| !pins.head) {
        // Under the toolbar where it leads the list.
        Some(title) => {
            let mut lead = lead;
            let at = usize::from(
                lead.first()
                    .is_some_and(|(node, _)| super::is_compact_toolbar(node)),
            );
            lead.insert(
                at,
                super::page_name_node(title, (inner - THUMB_GUTTER).max(0.0)),
            );
            lead
        }
        None => lead,
    };
    let laid = ListRoom {
        panel: (panel_width, height),
        held: (pins.head, pins.footer, lead.len()),
        room,
        extra: 0.0,
        keyboard_pinned: false,
    };
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
    for (node, _) in &head {
        state.note_pinned_status_room(node, under_head);
        pin(state, node);
    }
    for (node, _) in footer.iter().filter(|_| pins.footer) {
        pin(state, node);
    }
    let unbuilt = || {
        UiNode::new(
            "settings/showcase/chooser",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
    };
    if measured(state, laid) && !laying() {
        return unbuilt();
    }
    let query = state.showcase.query();
    if !measuring() {
        *state.showcase.order.borrow_mut() =
            with_blocks(state, (inner, narrow, &query), false, |entry| {
                entry
                    .blocks
                    .iter()
                    .filter_map(|(_, _, slug)| slug.clone())
                    .collect()
            });
        state
            .showcase
            .page_rows
            .set(((room / row_height()).floor() as usize).max(1));
    }
    let captions_from = on_grid(caption_height() + SECTION_ABOVE) + tallest_row.max(row_height());
    let captions = room + 0.5 >= captions_from;
    let led = lead_at(&lead);
    let items_at = |width: f32, painted: bool| {
        let at = (width.to_bits(), narrow, query.clone(), painted);
        let held = (pins.head, pins.footer, captions);
        shared_items(ItemsAt::Chooser(at, held, led.clone()), || {
            let blocks = with_blocks(state, (width, narrow, &query), painted, |entry| {
                entry.blocks.clone()
            });
            let footer = (!pins.footer).then(|| {
                if width == inner {
                    footer.clone()
                } else {
                    footer_rows(state, width, narrow)
                }
            });
            chooser_items(state, blocks, width, pins, lead.clone(), captions, footer)
        })
    };
    // The list at `inner` as tall as it lays out, whether it scrolls there.
    let span = scroll::leading_span(&items_at(inner, false), 0.0, f32::INFINITY, usize::MAX);
    let scrolls = span > room + 0.01;
    let items = items_at(
        if scrolls {
            (inner - THUMB_GUTTER).max(0.0)
        } else {
            inner
        },
        true,
    );
    // The rooms these rows are built for: captioned, scrolling and led as in `room`; and as wide
    // for, scrolling as there.
    let as_wide = move |room: f32| (span > room + 0.01) == scrolls;
    let same = move |room: f32| {
        as_wide(room)
            && (room + 0.5 >= captions_from) == captions
            && LeadSizing::alike_in(sizing, room)
    };
    let rows = scroll::ListRows {
        same: Box::new(same),
        as_wide: Box::new(as_wide),
    };
    if measuring() {
        lay(scroll::ListLaid::new(items, 0.0, rows));
        return unbuilt();
    }
    let mut list =
        scroll::list_page_whole(state, (SHOWCASE_LIST, "Themes"), items, (room, 0.0), rows);
    set_role(&mut list, SHOWCASE_LIST, SemanticRole::ListBox);
    transparent_sections(&mut list);
    if pins.footer {
        list.layout.height = Length::Fixed(room);
    }
    let mut children: Vec<UiNode> = head.into_iter().map(|(node, _)| node).collect();
    children.push(list);
    if pins.footer {
        children.push(rule_node("settings/showcase/footer/rule"));
        children.extend(footer.into_iter().map(|(node, _)| node));
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

/// The chooser's blocks as list items: each captioned section (NEEDS ATTENTION, ATERM, BUILT IN,
/// YOURS, COMMUNITY) one card listed row by row under its caption ([`scroll::SplitCard`]), so a
/// screen that starts inside a section repeats its caption over its first row there (with
/// `captions`: where the list's room holds a caption over a row). The open Community section's
/// head is its disclosure, still one control, and a screen that starts past its first row repeats
/// COMMUNITY in its place, so its rows are never under no name (Tokyo Night Day read as a built-in
/// beside Tokyo Night) nor under the disclosure and a caption saying the same word. Every other
/// block (the signature rows leading the list, the closed disclosure, the footer, an empty
/// filter's line) is a stop of its own.
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
        let disclosure = matches!(node.key.as_str(), PACK | "settings/showcase/pack-row");
        let Some(word) = captions
            .then_some(&node)
            .and_then(|node| {
                if disclosure {
                    return Some(Section::Community.word());
                }
                node.key
                    .as_str()
                    .strip_prefix(SECTION_PREFIX)?
                    .strip_suffix("/inset")
            })
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
        let repeat = if disclosure {
            let caption = Section::Community.caption().unwrap_or_default();
            let (mut caption, _, _) = section_band(Section::Community, caption, height);
            caption.layout.width = Length::Fixed(row_width);
            vec![(caption, height)]
        } else {
            Vec::new()
        };
        items.push(ListItem::Split(scroll::SplitCard {
            key: format!("settings/showcase/section-rows/{word}"),
            spec: GroupSpec::unlabeled(SemanticRole::Group),
            padding: Insets::all(0.0),
            gap: 0.0,
            head: vec![(node, height)],
            repeat,
            rows,
        }));
    }
    items
}

fn sheet_panel(
    state: &SettingsViewState,
    panel_width: f32,
    height: f32,
    (lead, sizing): Lead,
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
    let laid = ListRoom {
        panel: (panel_width, height),
        held: (pin_title, false, lead.len()),
        room,
        extra: 0.0,
        keyboard_pinned: false,
    };
    let unbuilt = || {
        UiNode::new(
            "settings/showcase/sheet",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
    };
    if measured(state, laid) && !laying() {
        return unbuilt();
    }
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
    let rows_span = with_sheet_blocks(state, (inner, room, one_line), |_, blocks| {
        blocks.iter().map(|(_, height)| height + gap).sum::<f32>()
    });
    let span = head_span + rows_span - gap;
    let scrolls = span > room + 0.01;
    let width = if scrolls { inner - THUMB_GUTTER } else { inner };
    let led = lead_at(&lead);
    let items = with_sheet_blocks(state, (width, room, one_line), |built, blocks| {
        let at = (built, width.to_bits());
        shared_items(ItemsAt::Sheet(at, (pin_title, scrolls), led), || {
            head(width)
                .into_iter()
                .chain(blocks.iter().cloned().map(|(mut node, height)| {
                    if scrolls {
                        node.layout.padding.right += THUMB_GUTTER;
                    }
                    ListItem::of(node, height)
                }))
                .collect()
        })
    });
    // The rooms these rows are built for: scrolling as in `room`, and that room where an open
    // picker is sized by it.
    let sized = sheet_sized_by_room(state);
    let built_in = room;
    let as_wide = move |room: f32| (!sized || room == built_in) && (span > room + 0.01) == scrolls;
    let same = move |room: f32| as_wide(room) && LeadSizing::alike_in(sizing, room);
    let rows = scroll::ListRows {
        same: Box::new(same),
        as_wide: Box::new(as_wide),
    };
    if measuring() {
        lay(scroll::ListLaid::new(items, gap, rows));
        return unbuilt();
    }
    let list = scroll::list_page_whole(
        state,
        ("settings/showcase/sheet-list", &label),
        items,
        (room, gap),
        rows,
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
            paint: None,
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
    // On the list's grid ([`on_grid`]): the words beside the switch in one row where they fit
    // one line there, else over it in a row of their own (wrapped beside it, a two-line label
    // made a 72pt row under 56pt rows at 2× text, and the list's end fell 40pt short of its
    // room's edge).
    let over = measure(visible) > beside + crate::native_ui::TEXT_FIT_SLACK;
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
    let inset = Insets {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: TEXT_INSET,
    };
    let group = || UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group));
    let mut rows = Vec::new();
    if over {
        let band = on_grid(label_height);
        rows.push((
            UiNode::new("settings/showcase/footer/follow-words", group())
                .layout(
                    Layout::column()
                        .height(Length::Fixed(band))
                        .padding(Insets {
                            top: band - label_height,
                            ..inset
                        }),
                )
                .children(vec![label.paint_only()]),
            band,
        ));
        rows.push((
            UiNode::new("settings/showcase/footer/follow", group())
                .layout(Layout::row().height(Length::Fixed(height)).padding(inset))
                .children(vec![switch]),
            height,
        ));
    } else {
        let label = label.layout(Layout::default().width(Length::Fill).height(Length::Fill));
        rows.push((
            UiNode::new("settings/showcase/footer/follow", group())
                .layout(
                    Layout::row()
                        .height(Length::Fixed(height))
                        .gap(6.0)
                        .padding(inset),
                )
                .children(vec![label.paint_only(), switch]),
            height,
        ));
    }
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
    // Links that wrap stand a row a line, each on the grid as one line of them does.
    if links_height > height + 0.01 && !links.children.is_empty() {
        rows.extend(links.children.into_iter().map(|line| {
            let tall = match line.layout.height {
                Length::Fixed(tall) => tall,
                _ => height,
            };
            (
                line.layout(Layout::row().height(Length::Fixed(tall)).gap(4.0)),
                tall,
            )
        }));
    } else {
        rows.push((links, links_height));
    }
    rows
}

fn footer_line_height() -> f32 {
    20.0_f32.max(18.0 * settings_text_scale())
}

/// The list's blocks, in order, each with its height.
#[cfg(test)]
pub(in crate::native_settings) fn list_blocks(
    state: &SettingsViewState,
    width: f32,
    narrow: bool,
) -> Vec<(UiNode, f32, Option<String>)> {
    with_blocks(
        state,
        (width, narrow, &state.showcase.query()),
        true,
        |entry| entry.blocks.clone(),
    )
}

/// The chooser's rows at a width, narrow or not, under a query, painted or not.
pub(in crate::native_settings) struct SharedBlocks {
    at: (u32, bool, String),
    blocks: Vec<(UiNode, f32, Option<String>)>,
    painted: bool,
}

/// THE PAGE'S SEATS SHARE THE CHOOSER'S ROWS, the demo and the strip: while open, each width's are
/// built once a render.
pub(in crate::native_settings) struct BlocksShared<'a>(&'a SettingsViewState);

impl<'a> BlocksShared<'a> {
    pub(in crate::native_settings) fn open(state: &'a SettingsViewState) -> Self {
        ITEMS.with(|items| items.replace(Some(Vec::new())));
        state.showcase_blocks.replace(Some(Vec::new()));
        state.showcase_sheet.replace(Some(Vec::new()));
        state.showcase_demo.replace(Some(Vec::new()));
        state.showcase_strip.replace(Some(Vec::new()));
        Self(state)
    }
}

impl Drop for BlocksShared<'_> {
    fn drop(&mut self) {
        ITEMS.with(|items| items.replace(None));
        self.0.showcase_blocks.replace(None);
        self.0.showcase_sheet.replace(None);
        self.0.showcase_demo.replace(None);
        self.0.showcase_strip.replace(None);
    }
}

/// The Customize sheet's rows at a width, in a room, colour rows on one line or not, and the lines
/// a field was left and the most it wrapped to.
pub(in crate::native_settings) type SharedSheet = (SheetAt, (usize, usize), Vec<(UiNode, f32)>);

/// What the sheet's rows are built at: the width, the room where an open picker reads it, colour
/// rows on one line or not, and how tall rows stand too tall to stack.
type SheetAt = (u32, Option<u32>, bool, Option<u32>);

/// `read` of [`sheet_blocks`], built once a render for each width and room while [`BlocksShared`]
/// is open, and which of the render's they are.
fn with_sheet_blocks<R>(
    state: &SettingsViewState,
    (width, room, one_line): (f32, f32, bool),
    read: impl FnOnce(usize, &[(UiNode, f32)]) -> R,
) -> R {
    let (tall, most) = crate::native_settings::seat_row_rooms();
    let at = (
        width.to_bits(),
        sheet_sized_by_room(state).then_some(room.to_bits()),
        one_line,
        tall,
    );
    // Rows whose fields all fit the lines left them are built alike under any as many.
    let shared = state.showcase_sheet.borrow();
    let built = shared.as_ref().and_then(|shared| {
        shared.iter().position(|(seen, (left, wanted), _)| {
            *seen == at && (*left == most || (*wanted <= *left && *wanted <= most))
        })
    });
    if let Some((index, shared)) = built.zip(shared.as_ref()) {
        return read(index, &shared[index].2);
    }
    drop(shared);
    let (blocks, wanted) =
        crate::native_settings::field_lines_wanted(|| sheet_blocks(state, width, room, one_line));
    if let Some(shared) = state.showcase_sheet.borrow_mut().as_mut() {
        shared.push((at, (most, wanted), blocks));
    } else {
        return read(usize::MAX, &blocks);
    }
    let shared = state.showcase_sheet.borrow();
    let shared = shared.as_deref().unwrap_or_default();
    let index = shared.len().saturating_sub(1);
    read(
        index,
        shared.get(index).map_or(&[], |(_, _, blocks)| blocks),
    )
}

/// `read` of the rows [`build_list_blocks`] builds, `painted` or any, shared while
/// [`BlocksShared`] is open.
fn with_blocks<R>(
    state: &SettingsViewState,
    (width, narrow, query): (f32, bool, &str),
    painted: bool,
    read: impl FnOnce(&SharedBlocks) -> R,
) -> R {
    let at = (width.to_bits(), narrow, query.to_string());
    let build = |at| SharedBlocks {
        blocks: build_list_blocks(state, width, narrow, query, painted),
        at,
        painted,
    };
    let mut shared = state.showcase_blocks.borrow_mut();
    let Some(shared) = shared.as_mut() else {
        return read(&build(at));
    };
    let index = match shared
        .iter()
        .position(|entry| entry.at == at && (entry.painted || !painted))
    {
        Some(index) => index,
        None => {
            shared.push(build(at));
            shared.len() - 1
        }
    };
    read(&shared[index])
}

/// The chooser's list as blocks, each with its height and its theme's slug; `painted`, each
/// theme's row with what it paints, else as tall without ([`row_node`]).
fn build_list_blocks(
    state: &SettingsViewState,
    width: f32,
    narrow: bool,
    query: &str,
    painted: bool,
) -> Vec<(UiNode, f32, Option<String>)> {
    let showcase = &state.showcase;
    // Every row the list can list, the Community section's too, as an empty filter lists them.
    let every = query == EVERY_ROW;
    let query = if every { "" } else { query };
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
    let visible = if every {
        (0..showcase.cards.len()).collect()
    } else {
        showcase.visible_for(query)
    };
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
        let (row, height) = row_node(
            state,
            card,
            (width, narrow, slab, tags),
            (in_use, selected),
            painted,
        );
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
        let band = on_grid(height);
        blocks.push((inset(key, words, 0.0, band), band, None));
    }
    blocks
}

fn section_heading(
    section: Section,
    caption: &str,
    after_rows: bool,
) -> (UiNode, f32, Option<String>) {
    // A band of whole rows ([`on_grid`]), its words at its foot, at least [`SECTION_ABOVE`]
    // over them where it follows a row.
    let height = on_grid(caption_height() + if after_rows { SECTION_ABOVE } else { 0.0 });
    section_band(section, caption, height)
}

/// A section's caption in a band `height` tall, its words at its foot.
fn section_band(section: Section, caption: &str, height: f32) -> (UiNode, f32, Option<String>) {
    let above = (height - caption_height()).max(0.0);
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
    // It heads its section as a caption does: a band of whole rows ([`on_grid`]), the control
    // all of it but the [`SECTION_ABOVE`] over it where it follows a row, as tall as its words
    // take at least (they wrap to the room its chevron leaves, never cut: "Com…" at 2× text in
    // a 22-column window).
    let least = crate::native_ui::native_button_height_for_lines(lines.len(), caption_height());
    let band = on_grid(least + above);
    let height = band - above;
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
                .height(Length::Fixed(band))
                .padding(Insets {
                    top: above,
                    right: 0.0,
                    bottom: 0.0,
                    left: 0.0,
                }),
        )
        .children(vec![node]);
    }
    (node, band, None)
}

/// A theme's row and its height; `painted`, with what it paints (its name, tags, slab and file
/// buttons), else its control alone at that height, all a list measures its stops by.
fn row_node(
    state: &SettingsViewState,
    card: &ShowcaseCard,
    (width, narrow, slab, tags): (f32, bool, Option<f32>, TagWidths),
    (in_use, selected): (bool, bool),
    painted: bool,
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
    let TagWidths {
        aa: aa_width,
        low: low_width,
        check: check_width,
    } = tags;
    let icons_width = 2.0 * ICON_BUTTON + 4.0;
    let others = [
        low.then_some(low_width),
        yours.then_some(icons_width),
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
    let aa = typed && (!yours || name_fits_beside_tag());
    // The tags beside the name, in the order they are laid out.
    let trailing_width: f32 = [
        aa.then_some(aa_width),
        low.then_some(low_width),
        yours.then_some(icons_width),
        slab,
        Some(check_width),
    ]
    .into_iter()
    .flatten()
    .map(|width| width + 6.0)
    .sum();
    let mut name_width = (width - TEXT_INSET - ROW_END - trailing_width).max(0.0);
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Primary);
    let measure = |text: &str| crate::tray_raster::ui_text_width_for(face, text, px);
    let check = in_use || pieces_fit(&card.label, name_width, &measure);
    if !check {
        name_width += check_width + 6.0;
    }
    let words = (SemanticRole::Text, StyleRef::Primary);
    let lines = wrapped_words_lines(&card.label, words, name_width);
    let one_line = lines.len() <= 1;
    let row = |height: f32| {
        UiNode::new(key.clone(), option).layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(height)),
        )
    };
    let lines_height = lines.len().max(1) as f32 * row_name_line_height();
    let tall = if one_line {
        height
    } else {
        // A name on more lines than one makes the row as tall as they take, on the list's
        // grid ([`on_grid`]), its lines and its slab centred in it.
        on_grid(height.max(lines_height + 2.0 * ROW_NAME_PAD))
    };
    if !painted {
        return (row(tall), tall);
    }
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
    if aa {
        trailing.push(tag("aa", "Aa", StyleRef::Quiet, aa_width));
    }
    if low {
        trailing.push(tag("low", "\u{26a0}", StyleRef::Quiet, low_width));
    }
    if yours {
        trailing.push(spacer(&format!("{key}/icons"), icons_width));
    }
    let slab_pad = ((tall - SLAB_HEIGHT) / 2.0).max(0.0);
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
                    paint: Some(card.slab_value(narrow)),
                    action: None,
                    focusable: false,
                    focused: false,
                    focus_visible: false,
                }),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
        ])
    }));
    if check {
        trailing.push(tag(
            "check",
            if in_use { "\u{2713}" } else { "" },
            StyleRef::Accent,
            check_width,
        ));
    }
    let (name, _) = words_node_of_lines(
        &format!("{key}/name"),
        &card.label,
        words,
        lines,
        row_name_line_height(),
    );
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
                    top: ((tall - lines_height) / 2.0).max(ROW_NAME_PAD),
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
    (row(tall).children(children), tall)
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

/// A NEEDS ATTENTION row's least height: two lines inside a 4pt margin over and under.
fn attention_row_least() -> f32 {
    44.0_f32.max(40.0 * settings_text_scale())
}

/// A NEEDS ATTENTION row's height: the whole rows that hold its least ([`on_grid`]).
fn attention_row_height() -> f32 {
    on_grid(attention_row_least())
}

fn attention_row(state: &SettingsViewState, row: &AttentionRow, width: f32) -> UiNode {
    let key = format!("{CARD_PREFIX}{}", row.slug);
    let open_key = format!("{key}/{CARD_OPEN}");
    let height = attention_row_height();
    let pad = ((height - ICON_BUTTON) / 2.0).max(0.0);
    // Its two lines centred in the whole rows that hold them.
    let line_height = ((attention_row_least() - 8.0) / 2.0).max(0.0);
    let margin = ((height - 2.0 * line_height) / 2.0).max(0.0);
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
                top: margin,
                right: 0.0,
                bottom: margin,
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
