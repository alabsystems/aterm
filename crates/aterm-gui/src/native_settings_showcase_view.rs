// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Themes layout across Beside, Short, and Stacked modes ([`scroll::ListFoot::Whole`]).

use super::customize::{
    caption_height, colour_rows_least_column, colour_rows_on_one_line, customize_row, sheet_blocks,
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
        (settings_text_scale().to_bits(), measured_in()),
        role == SemanticRole::Heading,
    );
    let cached =
        showcase.name_floors.borrow().iter().find_map(|entry| {
            ((entry.cards, entry.text, entry.heading) == key).then_some(entry.floor)
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
    // Faces once replaced are not measured in again.
    floors.retain(|entry| entry.cards == key.0 && entry.text.1 == key.1.1);
    floors.push(NameFloor {
        cards: key.0,
        text: key.1,
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
    let strip = strip_tall(state, page_width);
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
    /// The keyboard's control pinned round the list: how tall, what a bar leaves ([`pin`]).
    static KEYBOARD_PIN: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    /// The lists' items built this render, while [`BlocksShared`] is open ([`shared_items`]).
    static ITEMS: RefCell<Option<Vec<SharedItems>>> = const { RefCell::new(None) };
    /// The footer's rows built this render, while [`BlocksShared`] is open ([`footer_rows`]).
    static FOOTERS: RefCell<Option<Vec<SharedFooter>>> = const { RefCell::new(None) };
    /// The faces chrome text measures in as this render first asked, while [`BlocksShared`] is
    /// open ([`measured_in`]).
    static FACES: Cell<Option<Option<u64>>> = const { Cell::new(None) };
}

/// The faces chrome text measures in ([`crate::tray_raster::chrome_measure_epoch`]), weighed once
/// a render while [`BlocksShared`] is open: the theme names' floors and the chooser's rows are keyed
/// by them. A face that lands later in the render keys what it measures under the faces before it,
/// which the next render measures again.
fn measured_in() -> u64 {
    FACES.with(|faces| match faces.get() {
        Some(Some(epoch)) => epoch,
        open => {
            let epoch = crate::tray_raster::chrome_measure_epoch();
            if open.is_some() {
                faces.set(Some(Some(epoch)));
            }
            epoch
        }
    })
}

/// Rows' names as [`strip_names`] measured them by room, size and face, where every width was kept.
static NAMES_MEASURED: std::sync::Mutex<Option<aterm_hash::FxHashMap<NamesAt, NamesMeasured>>> =
    std::sync::Mutex::new(None);

/// Where [`strip_names`] measured rows' names: the title's rooms, and its size and face.
type NamesAt = ((u32, u32, bool), u32, crate::widget::TextFace);

/// The rows [`strip_names`] measured there, the font it measured them in, and the measure.
type NamesMeasured = (
    std::sync::Arc<[crate::settings_preview::theme_card::ShowcaseCard]>,
    Option<std::sync::Arc<aterm_render::font::Font>>,
    NamesMeasure,
);

/// The most places [`NAMES_MEASURED`] keeps a measure for.
const NAMES_MEASURED_MOST: usize = 1024;

/// Drop every name [`strip_names`] measured, in every view: for the macOS font-change tests.
#[cfg(all(test, target_os = "macos"))]
pub(in crate::native_settings) fn forget_strip_names() {
    NAMES_MEASURED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
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
    pin: Option<(f32, f32)>,
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

    /// What a status's bar takes of the list's room.
    pub(in crate::native_settings) const fn extra(self) -> f32 {
        self.extra
    }

    /// Whether the keyboard's control is pinned around the list, so the list never cuts it.
    pub(in crate::native_settings) const fn pins_keyboard(self) -> bool {
        self.keyboard_pinned
    }

    /// The keyboard's control where the page pins it round the list: how tall, what a bar leaves.
    pub(in crate::native_settings) const fn keyboard_pin(self) -> Option<(f32, f32)> {
        self.pin
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

/// Note `node` pinned round the list for the keyboard's control, `tall` high, `clear` of the foot.
fn pin(state: &SettingsViewState, node: &UiNode, (tall, clear): (f32, f32)) {
    if state
        .common
        .last_focus
        .as_ref()
        .is_some_and(|key| holds(node, key))
    {
        if measuring() {
            KEYBOARD_PINNED.with(|pinned| pinned.set(true));
        }
        let cover = (state.status_extra.get() - clear).clamp(0.0, tall);
        KEYBOARD_PIN.with(|pin| pin.set(Some((tall, tall - cover))));
    }
}

/// Whether the page is only measured ([`list_room`]), its list laid out as `room` says, which a
/// page built keeps for [`laid_last`].
fn measured(state: &SettingsViewState, room: ListRoom) -> bool {
    let room = ListRoom {
        extra: state.status_extra.get(),
        keyboard_pinned: measuring() && KEYBOARD_PINNED.with(Cell::get),
        pin: KEYBOARD_PIN.with(Cell::get),
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

/// How [`themes_page`] lays its list's panel out in a room under its heading.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PanelLaid {
    shape: Shape,
    width: f32,
    height: f32,
    demo: f32,
    led: bool,
}

impl PanelLaid {
    fn at(
        state: &SettingsViewState,
        page_width: f32,
        room: f32,
        frame: Option<&SettingsPreviewSpec>,
    ) -> Self {
        let pinned = ChooserPins {
            head: true,
            footer: true,
        };
        let shape = match Shape::for_width(page_width) {
            Shape::Beside { chooser } if !beside_fits(state, chooser, page_width, room, frame) => {
                Shape::Stacked
            }
            Shape::Beside { chooser } if list_rows(state, room, pinned) < SHORT_ROWS => {
                Shape::Short { chooser }
            }
            Shape::Stacked => short_or_stacked(state, page_width, room, frame),
            shape => shape,
        };
        if let Shape::Beside { chooser } | Shape::Short { chooser } = shape {
            let pins = ChooserPins {
                head: true,
                footer: matches!(shape, Shape::Beside { .. }),
            };
            return Self {
                shape,
                width: panel_width(chooser, page_width),
                height: whole_rows_height(state, room, pins),
                demo: 0.0,
                led: false,
            };
        }
        let strip = strip_tall(state, page_width);
        let rest = |room: f32, demo: f32| stacked_rest(room, demo, strip);
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
        let demo_at = |room: f32| {
            let demo = stacked_demo(state, room, strip, frame);
            match frame {
                Some(frame) if rest(room, demo) < least => (1..6)
                    .rev()
                    .map(|rows| frame.demo_height(rows))
                    .find(|&height| rest(room, height) >= least)
                    .unwrap_or_else(|| frame.demo_height(1))
                    .min(demo),
                _ => demo,
            }
        };
        let demo = demo_at(room);
        let panel_height = rest(room, demo).max(0.0);
        let floor = match rows {
            Some(rows) => chooser_floor(state, rows),
            None => sheet_floor(state, page_width),
        };
        let led = frame.is_some() && panel_height + 0.5 < floor;
        let pins = ChooserPins {
            head: true,
            footer: false,
        };
        // A panel fitting its pinned filter over the tallest row keeps it, else lists it, floored.
        let height = if led {
            room
        } else if rows.is_none() || panel_height + 0.5 >= least {
            whole_rows_height(state, panel_height, pins)
                .max(least)
                .min(panel_height)
        } else {
            whole_rows_height(state, panel_height, ChooserPins::NONE)
                .max(floor)
                .min(panel_height)
        };
        Self {
            shape,
            width: page_width,
            height,
            demo,
            led,
        }
    }
}

/// Heading, width, list body, demo frame, the chooser's lead name and a toolbar leading the list.
type PageRoom = (
    Vec<UiNode>,
    f32,
    f32,
    std::rc::Rc<DemoBuilt>,
    (Option<&'static str>, Option<(UiNode, f32)>),
);

/// The heading as a chrome heads it, toolbar listed or not, at a gap: its height and lead rows.
pub(in crate::native_settings) type SharedHeading = (
    ((bool, bool), bool, u32),
    (Vec<UiNode>, f32, Vec<(UiNode, f32)>),
);

/// [`pinned_heading`], once a render for each way the chrome heads it, in [`BlocksShared`].
fn heading_shared(
    state: &SettingsViewState,
    width: SettingsWidth,
    viewport: LogicalRect,
    gap: f32,
) -> (Vec<UiNode>, f32) {
    let at = (
        page_chrome(state, width, viewport),
        super::compact_toolbar_lists(state, viewport),
        gap.to_bits(),
    );
    let built = state
        .showcase_heading
        .borrow()
        .as_ref()
        .and_then(|shared| shared.iter().find(|(seen, _)| *seen == at))
        .map(|(_, heading)| heading.clone());
    if let Some((out, used, head)) = built {
        state.list_head.replace(head);
        return (out, used);
    }
    let (out, used) = pinned_heading(state, width, viewport, gap, SHOWCASE_TITLE, "");
    if let Some(shared) = state.showcase_heading.borrow_mut().as_mut() {
        let head = state.list_head.borrow().clone();
        shared.push((at, (out.clone(), used, head)));
    }
    (out, used)
}

/// Heading, width, list body, demo frame and chooser's lead name at `state`'s chrome and bars.
fn page_room(
    state: &SettingsViewState,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
    body: Option<f32>,
) -> PageRoom {
    let viewport = cx.viewport;
    let gap = settings_page_gap(state, width, viewport);
    let maximum = PageLayout::Stage.maximum(width);
    let page_width = settings_page_content_width(viewport.width, width, maximum);
    let (mut out, mut used) = heading_shared(state, width, viewport, gap);
    let body = body.unwrap_or_else(|| settings_list_body_height(state, width, viewport));
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
    let demo = demo_shared(
        state,
        cx.animation_phase_ms,
        cx.motion,
        cx.terminal_font_px,
        cx.terminal_scale,
        cx.semantic_font.as_ref(),
    );
    (
        out,
        page_width,
        (body - used).max(0.0),
        demo,
        (name, toolbar),
    )
}

/// How the page lays its list's panel out at `state`'s chrome and bars, as its shape alone tells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native_settings) struct PanelAt(PanelLaid, f32);

impl PanelAt {
    /// Its list's body `body` where given, else as laid out.
    pub(in crate::native_settings) fn of(
        state: &SettingsViewState,
        width: SettingsWidth,
        cx: &ViewCx<'_>,
        body: Option<f32>,
    ) -> Self {
        // Measured, the page leaves whether its list took the toolbar as the page last built it.
        let taken = state.toolbar_head_taken.get();
        let (_, page_width, room, demo, _) = page_room(state, width, cx, body);
        state.toolbar_head_taken.set(taken);
        let laid = PanelLaid::at(
            state,
            page_width,
            room,
            demo.as_ref().as_ref().map(|(_, frame)| frame),
        );
        Self(laid, room)
    }

    /// How wide its panel lays out.
    pub(in crate::native_settings) const fn width(self) -> f32 {
        self.0.width
    }

    /// The most room a list under its heading has; a chooser's frame gives way for its tallest row.
    pub(in crate::native_settings) fn room_most(self, state: &SettingsViewState) -> f32 {
        let framed = (self.1 - 2.0 * PANEL_INSET).max(0.0);
        if state.showcase.sheet_open {
            return framed;
        }
        let inner = (self.0.width - 2.0 * PANEL_INSET).max(0.0);
        let tallest = chooser_rows_tallest(state, inner, chooser_is_narrow(self.0.width));
        framed.max(self.0.height - 2.0 * chooser_inset(state, tallest, self.0.height))
    }

    /// Whether its chooser keeps its frame, which its rows lay out inside ([`ChooserFit::inset`]).
    pub(in crate::native_settings) fn framed(self, state: &SettingsViewState) -> bool {
        if state.showcase.sheet_open {
            return true;
        }
        let inner = (self.0.width - 2.0 * PANEL_INSET).max(0.0);
        let tallest = chooser_rows_tallest(state, inner, chooser_is_narrow(self.0.width));
        chooser_inset(state, tallest, self.0.height) > 0.0
    }

    /// How tall its rows lay out the block holding `key`, alike with the gutter and without.
    pub(in crate::native_settings) fn block_holding(
        self,
        state: &SettingsViewState,
        key: &UiKey,
    ) -> Option<u32> {
        if state.showcase.sheet_open {
            return None;
        }
        let inset = if self.framed(state) { PANEL_INSET } else { 0.0 };
        let inner = (self.0.width - 2.0 * inset).max(0.0);
        let narrow = chooser_is_narrow(self.0.width);
        let query = state.showcase.query();
        let [wide, thin] = [inner, (inner - THUMB_GUTTER).max(0.0)].map(|width| {
            with_blocks(state, (width, narrow, &query), false, |entry| {
                entry
                    .blocks
                    .iter()
                    .find(|(node, _, _)| holds(node, key))
                    .map(|(_, height, _)| height.to_bits())
            })
        });
        wide.filter(|_| wide == thin)
    }

    /// How far its panel stands over its list's body's foot.
    pub(in crate::native_settings) fn below(self) -> f32 {
        self.1 - self.0.height
    }

    /// The room its list lays out in and what stands apart from it, as the pins `fits` choose.
    pub(in crate::native_settings) fn list_room(
        self,
        state: &SettingsViewState,
        fits: &ChooserFits,
    ) -> (f32, Held) {
        let strip = !self.0.led;
        if state.showcase.sheet_open {
            let held = Held {
                head: false,
                footer: false,
                strip,
                captions: false,
            };
            return ((self.0.height - 2.0 * PANEL_INSET).max(0.0), held);
        }
        let at = (
            self.0.width.to_bits(),
            !matches!(self.0.shape, Shape::Beside { .. }),
            self.0.led,
        );
        let known = fits.borrow().iter().any(|(seen, _)| *seen == at);
        if !known {
            let inner = (self.0.width - 2.0 * PANEL_INSET).max(0.0);
            let footer = footer_rows(state, inner, chooser_is_narrow(self.0.width));
            let fit = ChooserFit::at(state, self.0.width, (at.1, at.2), &footer);
            fits.borrow_mut().push((at, fit));
        }
        let fits = fits.borrow();
        let (_, fit) = fits
            .iter()
            .find(|(seen, _)| *seen == at)
            .expect("kept above");
        // A led list pins nothing, whatever its lead.
        let pins = fit.pins(state, self.0.height, |_| 0.0);
        let room = fit.room(state, self.0.height, pins);
        let held = Held {
            head: pins.head,
            footer: pins.footer,
            strip,
            captions: room + 0.5 >= captions_from(fit.rows(state, self.0.height).tallest_row),
        };
        (room, held)
    }
}

/// What stands apart from a chooser's list ([`PanelAt::list_room`]); whether it captions rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::native_settings) struct Held {
    pub(in crate::native_settings) head: bool,
    pub(in crate::native_settings) footer: bool,
    pub(in crate::native_settings) strip: bool,
    pub(in crate::native_settings) captions: bool,
}

/// The chooser's pin measures by panel width, stacked or not, led or not ([`PanelAt::list_room`]).
pub(in crate::native_settings) type ChooserFits = RefCell<Vec<((u32, bool, bool), ChooserFit)>>;

/// The page: the heading, then the chooser beside the demo and strip.
pub(crate) fn themes_page(
    state: &SettingsViewState,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> Vec<UiNode> {
    KEYBOARD_PIN.with(|pin| pin.set(None));
    let gap = settings_page_gap(state, width, cx.viewport);
    let (mut out, page_width, room, demo, (name, toolbar)) = page_room(state, width, cx, None);
    let (spec, frame) = Option::clone(&demo).unzip();
    let laid = PanelLaid::at(state, page_width, room, frame.as_ref());
    let shape = laid.shape;
    match shape {
        Shape::Beside { .. } | Shape::Short { .. } => {
            let body = room;
            let room = laid.height;
            let chooser = laid.width;
            let demo_width = (page_width - chooser - COLUMN_GAP).max(0.0);
            let (strip_box, strip) = strip_node(state, demo_width);
            // The strip stands over a status's bar: the demo gives up what the bar
            // covers of the column, and the bar shortens for a strip control it would
            // still cover.
            let cover = (state.status_extra.get() - (body - room)).max(0.0);
            let demo_height = (room - cover - DEMO_STRIP_GAP - strip).max(0.0);
            state.note_pinned_status_room(&strip_box, body - DEMO_STRIP_GAP - strip);
            pin(state, &strip_box, (strip, body - DEMO_STRIP_GAP - strip));
            let panel = || {
                if state.showcase.sheet_open {
                    sheet_panel(state, chooser, room, (Vec::new(), None))
                } else {
                    chooser_panel(
                        state,
                        (chooser, room, body - room),
                        shape,
                        (Vec::new(), None),
                        name,
                    )
                }
            };
            if measuring() {
                let _ = panel();
                return out;
            }
            let left = panel();
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
            let demo_height = laid.demo;
            match (spec, frame.as_ref()) {
                (Some(spec), Some(frame)) if laid.led => {
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
                            (page_width, room, 0.0),
                            Shape::Stacked,
                            (lead, Some(sizing)),
                            name,
                        )
                    });
                }
                (spec, _) => {
                    let above = spec.as_ref().map_or(0.0, |_| demo_height + gap);
                    if let Some(spec) = spec {
                        out.push(demo_node(spec, demo_height));
                    }
                    let panel_height = laid.height;
                    // The panel's clearance over the page's foot, the column laid out top down.
                    let below = room - above - strip - gap - panel_height;
                    state.note_pinned_status_room(&strip_box, panel_height);
                    pin(state, &strip_box, (strip, panel_height + gap + below));
                    out.push(strip_box);
                    out.push(if state.showcase.sheet_open {
                        sheet_panel(state, page_width, panel_height, (Vec::new(), None))
                    } else {
                        chooser_panel(
                            state,
                            (page_width, panel_height, below.max(0.0)),
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
    let strip = strip_tall(state, (page_width - panel_width - COLUMN_GAP).max(0.0));
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
/// in it, never reshapes the page under the person. Measured once for what the rows are laid out
/// from ([`ShowcaseState::rows_tallest`]): every row built again at two widths, up to three times
/// a render, was a third of a Themes frame.
fn chooser_rows_tallest(state: &SettingsViewState, inner: f32, narrow: bool) -> f32 {
    // Built once while its inputs hold, not each render: every rung tried reads it at its width.
    let showcase = &state.showcase;
    let at = rows_tallest_at(state, inner, narrow);
    let known = showcase
        .rows_tallest
        .borrow()
        .iter()
        .find(|(was, _)| *was == at)
        .map(|(_, (tallest, _))| *tallest);
    if let Some(tallest) = known {
        return tallest;
    }
    let [(wide, wide_rows), (thin, thin_rows)] =
        [inner, (inner - THUMB_GUTTER).max(0.0)].map(|width| {
            with_blocks(state, (width, narrow, EVERY_ROW), false, |entry| {
                let tallest = entry
                    .blocks
                    .iter()
                    .map(|(_, height, _)| *height)
                    .fold(0.0, f32::max);
                // Every theme's row, in the cards' order: the blocks with a slug.
                let rows = entry
                    .blocks
                    .iter()
                    .filter(|(_, _, slug)| slug.is_some())
                    .map(|(_, height, _)| *height)
                    .collect::<Vec<_>>();
                (tallest, rows)
            })
        });
    let tallest = wide.max(thin);
    let rows = wide_rows
        .into_iter()
        .zip(thin_rows)
        .map(|(wide, thin)| [wide, thin])
        .collect();
    let mut kept = showcase.rows_tallest.borrow_mut();
    kept.insert(0, (at, (tallest, rows)));
    kept.truncate(ROWS_TALLEST_KEPT);
    tallest
}

/// The widths [`chooser_rows_tallest`] keeps its measures for: a resize's few, each window's rungs.
const ROWS_TALLEST_KEPT: usize = 16;

/// What [`chooser_rows_tallest`] reads at a panel `inner` wide.
fn rows_tallest_at(state: &SettingsViewState, inner: f32, narrow: bool) -> RowsTallestAt {
    let showcase = &state.showcase;
    RowsTallestAt {
        cards: std::sync::Arc::as_ptr(&showcase.cards).cast::<()>() as usize,
        in_use: state.configured_theme_id(),
        attention: std::sync::Arc::as_ptr(&showcase.attention).cast::<()>() as usize,
        inner: inner.to_bits(),
        narrow,
        scale: settings_text_scale().to_bits(),
        faces: crate::tray_raster::strip_band_font_epoch(),
        measured: measured_in(),
    }
}

/// Each theme row's height at `width` where [`chooser_rows_tallest`] kept them, as rows now read.
fn rows_kept_at(
    state: &SettingsViewState,
    width: f32,
    narrow: bool,
) -> Option<(std::sync::Arc<[[f32; 2]]>, usize)> {
    let now = rows_tallest_at(state, 0.0, narrow);
    state
        .showcase
        .rows_tallest
        .borrow()
        .iter()
        .find_map(|(at, (_, rows))| {
            let inner = f32::from_bits(at.inner);
            let measured = [inner, (inner - THUMB_GUTTER).max(0.0)];
            let side = measured
                .iter()
                .position(|at| at.to_bits() == width.to_bits())?;
            (RowsTallestAt {
                inner: 0,
                ..at.clone()
            } == now
                && rows.len() == state.showcase.cards.len())
            .then(|| (rows.clone(), side))
        })
}

/// A head field's height where the chooser lists it: a row less the gap under it.
fn listed_head_field_height() -> f32 {
    (row_height() - PANEL_GAP).max(0.0)
}

/// How tall the keyboard's chooser row lays out in a panel `inner` wide, or a gutter narrower.
fn keyboard_row_tallest(state: &SettingsViewState, inner: f32, narrow: bool) -> f32 {
    let Some(slug) = state
        .common
        .last_focus
        .as_ref()
        .and_then(|key| key.as_str().strip_prefix(CARD_PREFIX))
    else {
        return 0.0;
    };
    let showcase = &state.showcase;
    let Some(card) = showcase
        .visible_for("")
        .into_iter()
        .map(|index| &showcase.cards[index])
        .find(|card| card.slug == slug)
    else {
        return 0.0;
    };
    let width = (inner - THUMB_GUTTER).max(0.0);
    let slab = slab_column(state, width, narrow);
    let in_use = state.configured_theme_id().eq_ignore_ascii_case(&card.id);
    let selected = state.highlighted_slug().as_deref() == Some(card.slug.as_str());
    row_node(
        state,
        card,
        (width, narrow, slab, TagWidths::now()),
        (in_use, selected),
        false,
    )
    .1
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
    state.theme_follows_system()
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
pub(in crate::native_settings) type SharedDemo = (DemoAt, std::rc::Rc<DemoBuilt>);

/// The demo's spec and the frame it is laid out in, where the page shows one.
type DemoBuilt = Option<(SettingsPreviewSpec, SettingsPreviewSpec)>;

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
) -> DemoBuilt {
    Option::clone(&demo_shared(
        state,
        phase_ms,
        motion,
        terminal_font_px,
        terminal_scale,
        prepared_font,
    ))
}

/// [`demo_spec_and_frame`] as the render shares it, which a page's shape reads without a copy.
fn demo_shared(
    state: &SettingsViewState,
    phase_ms: u64,
    motion: crate::native_app::ViewMotionCx,
    terminal_font_px: f32,
    terminal_scale: f32,
    prepared_font: Option<&crate::tray_raster::PreparedSemanticFont>,
) -> std::rc::Rc<DemoBuilt> {
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
        .map(|(_, demo)| std::rc::Rc::clone(demo));
    built.unwrap_or_else(|| {
        let demo = std::rc::Rc::new(build_demo_spec_and_frame(
            state,
            phase_ms,
            motion,
            terminal_font_px,
            terminal_scale,
            prepared_font,
        ));
        if let Some(shared) = state.showcase_demo.borrow_mut().as_mut() {
            shared.push((at, std::rc::Rc::clone(&demo)));
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

/// How tall [`strip_node`] lays the strip out at a width, read where shared without a node copy.
fn strip_tall(state: &SettingsViewState, width: f32) -> f32 {
    let at = width.to_bits();
    let known = state
        .showcase_strip
        .borrow()
        .as_ref()
        .and_then(|shared| shared.iter().find(|(seen, _)| *seen == at))
        .map(|(_, (_, height))| *height);
    known.unwrap_or_else(|| strip_node(state, width).1)
}

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
    let following = state.theme_follows_system();
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

/// The most lines the strip's names take beside its buttons, and whether a word outgrows its room.
fn strip_names(
    state: &SettingsViewState,
    in_use: &str,
    (plain, typed): (f32, f32),
) -> NamesMeasure {
    let showcase = &state.showcase;
    let under_typed = showcase
        .card(&state.theme_id_under_trial())
        .is_some_and(|card| card.typography.is_some());
    let rooms = (plain.to_bits(), typed.to_bits(), under_typed);
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Heading, StyleRef::Plain);
    let font = crate::tray_raster::ui_font_of(face);
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
    let at = (rooms, px.to_bits(), face);
    // Rows naming the same themes, each with type or without, measure alike in every view.
    let alike = |(rows, held, _): &NamesMeasured| {
        let in_font = match (held, &font) {
            (Some(held), Some(font)) => std::sync::Arc::ptr_eq(held, font),
            (None, None) => true,
            _ => false,
        };
        in_font
            && (std::sync::Arc::ptr_eq(rows, &showcase.cards)
                || rows.len() == showcase.cards.len()
                    && rows.iter().zip(showcase.cards.iter()).all(|(was, card)| {
                        was.label == card.label
                            && was.typography.is_some() == card.typography.is_some()
                    }))
    };
    let known = NAMES_MEASURED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|kept| kept.get(&at).cloned());
    let cached = known
        .filter(|measured| alike(measured))
        .map(|(cards, _, rows)| {
            // A hit by label keeps the current cards, so the next frame's hit is by pointer.
            if !std::sync::Arc::ptr_eq(&cards, &showcase.cards)
                && let Some(kept) = NAMES_MEASURED
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_mut()
                    .and_then(|kept| kept.get_mut(&at))
            {
                kept.0 = std::sync::Arc::clone(&showcase.cards);
            }
            rows
        });
    let rows = cached.unwrap_or_else(|| {
        let (rows, whole) = crate::tray_raster::ui_measures_whole(|| {
            showcase
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
                )
        });
        if whole {
            let mut kept = NAMES_MEASURED
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let kept = kept.get_or_insert_with(aterm_hash::FxHashMap::default);
            // Two rooms a frame, and a hand resize passes many: past the most, measures start over.
            if kept.len() >= NAMES_MEASURED_MOST {
                kept.clear();
            }
            kept.insert(
                at,
                (std::sync::Arc::clone(&showcase.cards), font.clone(), rows),
            );
        }
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

/// What a chooser panel's pins are chosen by, bar its height ([`chooser_panel`]).
pub(in crate::native_settings) struct ChooserFit {
    forms: &'static [ChooserPins],
    panel_width: f32,
    framed: FitRows,
    /// Its rows laid out to the panel's edges, where its frame gives way ([`Self::inset`]).
    edge: std::cell::OnceCell<FitRows>,
}

/// A chooser's rows at a width: whether its footer pins, its tallest row, and its tallest stop.
#[derive(Clone, Copy)]
struct FitRows {
    footer_pins: bool,
    tallest_row: f32,
    rows: f32,
}

impl FitRows {
    fn at(state: &SettingsViewState, inner: f32, narrow: bool, footer: &[(UiNode, f32)]) -> Self {
        // The footer pins as two field-high rows ([`footer_height`]); one wrapping more is listed.
        let footer_pins = footer.len() == 2
            && footer
                .iter()
                .all(|(_, height)| *height <= field_height() + 0.01);
        let tallest_row = chooser_rows_tallest(state, inner, narrow);
        // A pinned head leaves room for the keyboard's row as the list lays it out (R2).
        let rows = tallest_row
            .max(keyboard_row_tallest(state, inner, narrow))
            .max(footer.iter().map(|(_, height)| *height).fold(0.0, f32::max));
        Self {
            footer_pins,
            tallest_row,
            rows,
        }
    }
}

impl ChooserFit {
    fn at(
        state: &SettingsViewState,
        panel_width: f32,
        (stacked, led): (bool, bool),
        footer: &[(UiNode, f32)],
    ) -> Self {
        let inner = (panel_width - 2.0 * PANEL_INSET).max(0.0);
        let narrow = chooser_is_narrow(panel_width);
        let forms: &[ChooserPins] = if led {
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
        Self {
            forms,
            panel_width,
            framed: FitRows::at(state, inner, narrow, footer),
            edge: std::cell::OnceCell::new(),
        }
    }

    /// Its rows in a panel `height` tall: in its frame, or to its edges where that gives way.
    fn rows(&self, state: &SettingsViewState, height: f32) -> FitRows {
        if self.inset(state, height) > 0.0 {
            return self.framed;
        }
        *self.edge.get_or_init(|| {
            let narrow = chooser_is_narrow(self.panel_width);
            let footer = footer_rows(state, self.panel_width, narrow);
            FitRows::at(state, self.panel_width, narrow, &footer)
        })
    }

    /// Pins a `height` panel takes: the first whose list holds its tallest stop and its lead's.
    fn pins(
        &self,
        state: &SettingsViewState,
        height: f32,
        lead_tallest: impl Fn(f32) -> f32,
    ) -> ChooserPins {
        let fit = self.rows(state, height);
        self.forms
            .iter()
            .copied()
            .filter(|pins| fit.footer_pins || !pins.footer)
            .find(|&pins| {
                let room = self.room(state, height, pins);
                room + 0.5 >= chooser_tallest_stop(state, pins, fit.rows).max(lead_tallest(room))
            })
            .unwrap_or(ChooserPins::NONE)
    }
}

impl ChooserFit {
    /// THE FRAME GIVES WAY BEFORE A ROW OUTGROWS ITS LIST: the list takes the panel to its edges.
    fn inset(&self, state: &SettingsViewState, height: f32) -> f32 {
        chooser_inset(state, self.framed.tallest_row, height)
    }

    /// The room a panel `height` tall leaves its list with `pins` pinned, in its frame.
    fn room(&self, state: &SettingsViewState, height: f32, pins: ChooserPins) -> f32 {
        (height - 2.0 * self.inset(state, height) - chooser_pinned_height(state, pins)).max(0.0)
    }
}

/// Chooser's frame in a `height` panel whose tallest row is `tallest_row` ([`ChooserFit::inset`]).
fn chooser_inset(state: &SettingsViewState, tallest_row: f32, height: f32) -> f32 {
    if tallest_row
        > height - 2.0 * PANEL_INSET - chooser_pinned_height(state, ChooserPins::NONE) + 0.5
    {
        0.0
    } else {
        PANEL_INSET
    }
}

/// The least room a list leads a section with its caption in, its tallest row `tallest_row` tall.
fn captions_from(tallest_row: f32) -> f32 {
    on_grid(caption_height() + SECTION_ABOVE) + tallest_row.max(row_height())
}

/// The theme chooser: its filter and footer pinned where its room holds them over whole rows,
/// else listed, its list led by `lead`, and by the page's name (`name`) where nothing over the
/// page names it and the list leads with its filter too.
fn chooser_panel(
    state: &SettingsViewState,
    (panel_width, height, below): (f32, f32, f32),
    shape: Shape,
    (lead, sizing): Lead,
    name: Option<&str>,
) -> UiNode {
    let stacked = !matches!(shape, Shape::Beside { .. });
    let narrow = chooser_is_narrow(panel_width);
    let framed = (panel_width - 2.0 * PANEL_INSET).max(0.0);
    let fit = ChooserFit::at(
        state,
        panel_width,
        (stacked, !lead.is_empty()),
        &footer_rows(state, framed, narrow),
    );
    let inset = fit.inset(state, height);
    let inner = (panel_width - 2.0 * inset).max(0.0);
    let footer = footer_rows(state, inner, narrow);
    let tallest_row = fit.rows(state, height).tallest_row;
    let lead_items: Vec<ListItem> = lead
        .iter()
        .cloned()
        .map(|(node, height)| lead_item(node, height, DEMO_STRIP_GAP, inner))
        .collect();
    let lead_tallest = |room: f32| scroll::tallest_span(&lead_items, 0.0, room);
    let pins = fit.pins(state, height, lead_tallest);
    let room = fit.room(state, height, pins);
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
        pin: None,
    };
    let head = if pins.head {
        chooser_head(state, inner)
    } else {
        Vec::new()
    };
    let under_head = head
        .iter()
        .fold(height - inset + PANEL_GAP, |under, (_, tall)| {
            under - tall - PANEL_GAP
        });
    let mut over = height - inset;
    for (node, tall) in &head {
        state.note_pinned_status_room(node, under_head);
        over -= tall;
        pin(state, node, (*tall, over + below));
        over -= PANEL_GAP;
    }
    let mut over = inset;
    for (node, tall) in footer.iter().filter(|_| pins.footer).rev() {
        pin(state, node, (*tall, over + below));
        over += tall + PANEL_GAP;
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
    let captions_from = captions_from(tallest_row);
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
    let mut list = scroll::list_page_whole(
        state,
        (SHOWCASE_LIST, "Themes"),
        items,
        (room, 0.0),
        (rows, KEYBOARD_PIN.with(Cell::get)),
    );
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
            .padding(Insets::all(inset))
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

/// The word naming the section a block heads: a section's heading or the open Community disclosure.
fn section_word(node: &UiNode) -> Option<&str> {
    if matches!(node.key.as_str(), PACK | "settings/showcase/pack-row") {
        return Some(Section::Community.word());
    }
    node.key
        .as_str()
        .strip_prefix("settings/showcase/section/")?
        .strip_suffix("/inset")
}

/// The caption over block `index` of a captioned list and its section's tallest row; none unheaded.
fn caption_over(blocks: &[(UiNode, f32, Option<String>)], index: usize) -> Option<(f32, f32)> {
    let row = |at: usize| blocks[at].0.key.as_str().starts_with(CARD_PREFIX);
    if !row(index) {
        return None;
    }
    let heading = (0..index).rev().find(|at| !row(*at))?;
    section_word(&blocks[heading].0)?;
    let tallest = blocks[heading + 1..]
        .iter()
        .take_while(|(node, _, _)| node.key.as_str().starts_with(CARD_PREFIX))
        .map(|(_, height, _)| *height)
        .fold(0.0, f32::max);
    Some((blocks[heading].1, tallest))
}

/// The caption over a row as the list lays it out at two widths: the heading and its tallest row.
fn caption_at_both(wide: Option<(f32, f32)>, thin: Option<(f32, f32)>) -> Option<(f32, f32)> {
    let ((lead, wide), (_, thin)) = (wide?, thin?);
    Some((lead, wide.max(thin)))
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
    let mut items = Vec::new();
    let mut blocks = blocks.into_iter().peekable();
    while let Some((mut node, height, _)) = blocks.next() {
        node.layout.width = Length::Fixed(row_width);
        let disclosure = matches!(node.key.as_str(), PACK | "settings/showcase/pack-row");
        let Some(word) = captions
            .then_some(&node)
            .and_then(section_word)
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
            let (mut caption, _, _) = section_band(Section::Community, caption, height, row_width);
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
        pin: None,
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
        (rows, KEYBOARD_PIN.with(Cell::get)),
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

/// The footer's rows at a width, narrow or not, built this render.
type SharedFooter = ((u32, bool), Vec<(UiNode, f32)>);

/// [`build_footer_rows`], built once a render for each width while [`BlocksShared`] is open.
fn footer_rows(state: &SettingsViewState, width: f32, narrow: bool) -> Vec<(UiNode, f32)> {
    let at = (width.to_bits(), narrow);
    let known = FOOTERS.with(|footers| {
        footers.borrow().as_ref().and_then(|footers| {
            footers
                .iter()
                .find(|(seen, _)| *seen == at)
                .map(|(_, rows)| rows.clone())
        })
    });
    known.unwrap_or_else(|| {
        let rows = build_footer_rows(state, width, narrow);
        FOOTERS.with(|footers| {
            if let Some(footers) = footers.borrow_mut().as_mut() {
                footers.push((at, rows.clone()));
            }
        });
        rows
    })
}

fn build_footer_rows(state: &SettingsViewState, width: f32, narrow: bool) -> Vec<(UiNode, f32)> {
    let height = field_height();
    let following = state.theme_follows_system();
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

/// Every block's key and height unpainted at `inner` and a gutter less: measured, or `kept` read.
#[cfg(test)]
pub(in crate::native_settings) fn list_heights(
    state: &SettingsViewState,
    (inner, narrow): (f32, bool),
    kept: Option<fn(f32) -> f32>,
) -> Vec<Vec<(String, f32)>> {
    state.showcase.rows_tallest.borrow_mut().clear();
    if let Some(read) = kept {
        let _ = chooser_rows_tallest(state, inner, narrow);
        for (_, (_, rows)) in state.showcase.rows_tallest.borrow_mut().iter_mut() {
            *rows = rows.iter().map(|row| row.map(read)).collect();
        }
    }
    let query = state.showcase.query();
    [inner, (inner - THUMB_GUTTER).max(0.0)]
        .into_iter()
        .map(|width| {
            with_blocks(state, (width, narrow, &query), false, |entry| {
                entry
                    .blocks
                    .iter()
                    .map(|(node, height, _)| (node.key.as_str().to_string(), *height))
                    .collect()
            })
        })
        .collect()
}

/// Where the page holds the keyboard's control: listed, or in the head, footer or strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::native_settings) enum Place {
    Listed,
    Head,
    Footer,
    Strip,
}

/// Rows between two of the chooser's at the inside and a gutter narrower, and the second's place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native_settings) struct Between {
    /// At each width: the rows strictly between, the first's row and the second.
    at: [(f32, f32, f32); 2],
    /// The list's rows at the inside: a list shorter scrolls, its rows a thumb's gutter narrower.
    rows: f32,
    /// Per width, the row a captioned list stops with the second heading a section under the first.
    joined: [f32; 2],
    pub(in crate::native_settings) place: Place,
    /// The caption over the first, and its section's tallest row.
    pub(in crate::native_settings) over: Option<(f32, f32)>,
    /// How far the second's row stands over its panel's foot where a footer pins it.
    pub(in crate::native_settings) foot: f32,
}

impl Between {
    /// The rows between, the first's and the second's in `room`: as a scroll lays them, else least.
    pub(in crate::native_settings) fn in_room(self, room: f32) -> (f32, f32, f32) {
        let [(between, from, to), thin] = self.at;
        if self.rows > room + 0.01 {
            thin
        } else {
            (between.min(thin.0), from.min(thin.1), to.min(thin.2))
        }
    }

    /// The row a captioned list `room` tall stops with the second heading a section past the first.
    pub(in crate::native_settings) fn joined_in(self, room: f32) -> f32 {
        let [wide, thin] = self.joined;
        if self.rows > room + 0.01 {
            thin
        } else {
            wide.min(thin)
        }
    }
}

/// The rows strictly between `from`'s and `to`'s, and theirs, in a panel so wide and framed.
pub(in crate::native_settings) fn blocks_between(
    state: &SettingsViewState,
    (page_width, panel_width, framed): (f32, f32, bool),
    from: &UiKey,
    to: &UiKey,
) -> Option<Between> {
    let inset = if framed { PANEL_INSET } else { 0.0 };
    let inner = (panel_width - 2.0 * inset).max(0.0);
    let query = state.showcase.query();
    let narrow = chooser_is_narrow(panel_width);
    let head = chooser_head(state, inner)
        .iter()
        .any(|(node, _)| holds(node, to));
    let pinned = footer_rows(state, inner, narrow);
    let footer_at_inner = pinned.iter().position(|(node, _)| holds(node, to));
    let footer = footer_at_inner.is_some();
    // A pinned footer stacks its rows up from its panel's inset, `to`'s over the rows after it.
    let foot = footer_at_inner.map_or(0.0, |at| {
        inset
            + pinned[at + 1..]
                .iter()
                .map(|(_, tall)| tall + PANEL_GAP)
                .sum::<f32>()
    });
    let strip = !head && !footer && holds(&strip_node(state, page_width).0, to);
    let between = |width: f32| {
        with_blocks(state, (width, narrow, &query), false, |entry| {
            let at = |key: &UiKey| {
                entry
                    .blocks
                    .iter()
                    .position(|(node, _, _)| holds(node, key))
            };
            let from = at(from)?;
            let (skip, take, first, to) = if head || strip {
                (0, from, None, None)
            } else if footer {
                (from + 1, entry.blocks.len(), Some(from), None)
            } else {
                let to = at(to)?;
                (from != to).then_some((
                    from.min(to) + 1,
                    from.max(to),
                    Some(from.min(to)),
                    Some(to),
                ))?
            };
            let height = |at: usize| entry.blocks[at].1;
            let joined = to
                .filter(|to| *to > from && section_word(&entry.blocks[*to].0).is_some())
                .and_then(|to| entry.blocks.get(to + 1))
                .filter(|(node, _, _)| node.key.as_str().starts_with(CARD_PREFIX))
                .map_or(0.0, |(_, height, _)| *height);
            let rows = entry
                .blocks
                .iter()
                .take(take)
                .skip(skip)
                .map(|(_, height, _)| *height)
                .sum::<f32>();
            let all = entry
                .blocks
                .iter()
                .map(|(_, height, _)| *height)
                .sum::<f32>();
            Some((
                (rows, height(from), to.map_or(0.0, height)),
                first.and_then(|first| caption_over(&entry.blocks, first)),
                all,
                joined,
            ))
        })
    };
    // The footer's rows a listed footer lays out before `to`'s, and `to`'s.
    let footer_at = |width: f32| {
        let rows = footer_rows(state, width, narrow);
        let at = rows.iter().position(|(node, _)| holds(node, to))?;
        Some((
            rows[..at].iter().map(|(_, height)| *height).sum::<f32>(),
            rows[at].1,
        ))
    };
    // A narrower list can lay its rows out shorter, where a row's slab gives way to its name.
    let narrower = (inner - THUMB_GUTTER).max(0.0);
    let ((wide, over, rows, joined), (thin, thin_over, _, thin_joined)) =
        (between(inner)?, between(narrower)?);
    let over = caption_at_both(over, thin_over);
    // Only a chooser beside the demo pins its footer; a leading strip leads its head too, unpinned.
    let place = if head {
        Place::Head
    } else if strip {
        Place::Strip
    } else if footer && matches!(Shape::for_width(page_width), Shape::Beside { .. }) {
        Place::Footer
    } else {
        Place::Listed
    };
    let at = |(rows, from, to): (f32, f32, f32), width: f32| {
        Some(if head {
            (rows, from, field_height())
        } else if strip {
            (
                rows + chooser_head_heights(state).iter().sum::<f32>(),
                from,
                strip_button_height(),
            )
        } else if footer {
            let (before, to) = footer_at(width)?;
            (rows + before, from, to)
        } else {
            (rows, from, to)
        })
    };
    Some(Between {
        at: [at(wide, inner)?, at(thin, narrower)?],
        rows,
        joined: [joined, thin_joined],
        place,
        over,
        foot,
    })
}

/// The caption over `key`'s row in a captioned list by panel width and frame, and its tallest row.
pub(in crate::native_settings) fn caption_over_row(
    state: &SettingsViewState,
    (panel_width, framed): (f32, bool),
    key: &UiKey,
) -> Option<(f32, f32)> {
    let inner = (panel_width - if framed { 2.0 * PANEL_INSET } else { 0.0 }).max(0.0);
    let query = state.showcase.query();
    let narrow = chooser_is_narrow(panel_width);
    let over = |width: f32| {
        with_blocks(state, (width, narrow, &query), false, |entry| {
            let at = entry
                .blocks
                .iter()
                .position(|(node, _, _)| holds(node, key))?;
            caption_over(&entry.blocks, at)
        })
    };
    caption_at_both(over(inner), over((inner - THUMB_GUTTER).max(0.0)))
}

/// Whether `node` is `key`'s or holds it.
fn holds(node: &UiNode, key: &UiKey) -> bool {
    node.key == *key || node.children.iter().any(|child| holds(child, key))
}

/// Whether the chooser's filter leaves theme row `key` out of a panel by width and frame.
pub(in crate::native_settings) fn row_filtered_out(
    state: &SettingsViewState,
    (panel_width, framed): (f32, bool),
    key: &UiKey,
) -> bool {
    let inner = (panel_width - if framed { 2.0 * PANEL_INSET } else { 0.0 }).max(0.0);
    let query = state.showcase.query();
    key.as_str().starts_with(CARD_PREFIX)
        && with_blocks(
            state,
            (inner, chooser_is_narrow(panel_width), &query),
            false,
            |entry| !entry.blocks.iter().any(|(node, _, _)| holds(node, key)),
        )
}

/// How tall `key`'s row lays out by panel width and frame, scrolling or not, captioned if `captions`.
pub(in crate::native_settings) fn row_under_caption(
    state: &SettingsViewState,
    (panel_width, framed): (f32, bool),
    key: &UiKey,
    captions: bool,
) -> Option<f32> {
    let inner = (panel_width - if framed { 2.0 * PANEL_INSET } else { 0.0 }).max(0.0);
    let query = state.showcase.query();
    let narrow = chooser_is_narrow(panel_width);
    let tall = |width: f32| {
        with_blocks(state, (width, narrow, &query), false, |entry| {
            let at = entry
                .blocks
                .iter()
                .position(|(node, _, _)| holds(node, key))?;
            let (node, row, _) = &entry.blocks[at];
            let over = captions
                .then(|| caption_over(&entry.blocks, at))
                .flatten()
                .map_or(0.0, |(lead, _)| lead);
            node.key
                .as_str()
                .starts_with(CARD_PREFIX)
                .then_some(row + over)
        })
    };
    Some(tall(inner)?.max(tall((inner - THUMB_GUTTER).max(0.0))?))
}

/// The least a demo-led list's first screen needs: the demo at one row, then the strip's buttons.
pub(in crate::native_settings) fn led_first_screen_least(
    state: &SettingsViewState,
    cx: &ViewCx<'_>,
) -> Option<f32> {
    let demo = demo_shared(
        state,
        cx.animation_phase_ms,
        cx.motion,
        cx.terminal_font_px,
        cx.terminal_scale,
        cx.semantic_font.as_ref(),
    );
    let (_, frame) = demo.as_ref().as_ref()?;
    Some(frame.demo_height(1) + strip_button_height())
}

/// The least listed `key` lays out to at any width: a theme row or the pack; else nothing.
pub(in crate::native_settings) fn listed_least(state: &SettingsViewState, key: &UiKey) -> f32 {
    let key = key.as_str();
    let row = key == PACK
        || key
            .strip_prefix(CARD_PREFIX)
            .is_some_and(|slug| state.showcase.cards.iter().any(|card| card.slug == slug));
    if row { row_height() } else { 0.0 }
}

/// The chooser's rows at a width, narrow or not, under a query, painted or not.
#[derive(Debug)]
pub(in crate::native_settings) struct SharedBlocks {
    at: (u32, bool, String),
    blocks: Vec<(UiNode, f32, Option<String>)>,
    painted: bool,
    /// What else unpainted rows were built of: kept across renders while it holds.
    from: Option<BlocksFrom>,
}

/// What unpainted rows read besides width and query: rows, faces, disclosure, focus, filter words.
#[derive(Debug, PartialEq)]
struct BlocksFrom {
    rows: RowsTallestAt,
    pack_open: bool,
    focus: Option<String>,
    filter: String,
}

/// The [`BlocksFrom`] unpainted rows `narrow` or not would be built of now.
fn blocks_from(state: &SettingsViewState, narrow: bool) -> BlocksFrom {
    BlocksFrom {
        rows: rows_tallest_at(state, 0.0, narrow),
        pack_open: state.showcase.pack_open,
        focus: state
            .common
            .last_focus
            .as_ref()
            .map(|focus| focus.as_str().to_string()),
        filter: state.showcase.filter.value().to_string(),
    }
}

/// The most unpainted lists [`ShowcaseState::blocks_kept`] keeps, the latest renders' first.
const BLOCKS_KEPT: usize = 16;

/// THE PAGE'S SEATS SHARE THE CHOOSER'S ROWS, demo, strip and colours: built once a render a width.
pub(in crate::native_settings) struct BlocksShared<'a>(&'a SettingsViewState);

impl<'a> BlocksShared<'a> {
    pub(in crate::native_settings) fn open(state: &'a SettingsViewState) -> Self {
        ITEMS.with(|items| items.replace(Some(Vec::new())));
        FOOTERS.with(|footers| footers.replace(Some(Vec::new())));
        FACES.with(|faces| faces.set(Some(None)));
        state.showcase_blocks.replace(Some(Vec::new()));
        state.showcase_sheet.replace(Some(Vec::new()));
        state.showcase_sheet_rows.replace(Some(Vec::new()));
        state.showcase_demo.replace(Some(Vec::new()));
        state.showcase_strip.replace(Some(Vec::new()));
        state.showcase_heading.replace(Some(Vec::new()));
        state.colors_shared.replace(Some(Vec::new()));
        state.follows_shared.set(Some(None));
        Self(state)
    }
}

impl Drop for BlocksShared<'_> {
    fn drop(&mut self) {
        ITEMS.with(|items| items.replace(None));
        FOOTERS.with(|footers| footers.replace(None));
        FACES.with(|faces| faces.set(None));
        let built = self.0.showcase_blocks.replace(None).unwrap_or_default();
        let mut kept = self.0.showcase.blocks_kept.borrow_mut();
        let mut latest: Vec<SharedBlocks> = built
            .into_iter()
            .filter(|entry| entry.from.is_some())
            .collect();
        latest.append(&mut kept);
        latest.truncate(BLOCKS_KEPT);
        *kept = latest;
        drop(kept);
        self.0.showcase_sheet.replace(None);
        self.0.showcase_sheet_rows.replace(None);
        self.0.showcase_demo.replace(None);
        self.0.showcase_strip.replace(None);
        self.0.showcase_heading.replace(None);
        self.0.colors_shared.replace(None);
        self.0.follows_shared.set(None);
    }
}

/// The Customize sheet's rows at a width, in a room, colour rows on one line or not, and the lines
/// a field was left and the most it wrapped to.
pub(in crate::native_settings) type SharedSheet = (SheetAt, (usize, usize), Vec<(UiNode, f32)>);

/// What the sheet's rows are built at: the width, the room where an open picker reads it, colour
/// rows on one line or not, and how tall rows stand too tall to stack.
type SheetAt = (u32, Option<u32>, bool, Option<u32>);

/// Sheet colour rows by width, one-line and too-tall flags, with the lines a field had and wanted.
pub(in crate::native_settings) type SharedSheetRows = (
    (u32, bool, Option<u32>),
    (usize, usize),
    std::rc::Rc<[Option<(UiNode, f32)>]>,
);

/// The sheet's colour rows ([`customize_row`]), once a render a width in [`BlocksShared`].
fn sheet_colour_rows(
    state: &SettingsViewState,
    width: f32,
    one_line: bool,
) -> std::rc::Rc<[Option<(UiNode, f32)>]> {
    let (tall, most) = crate::native_settings::seat_row_rooms();
    let at = (width.to_bits(), one_line, tall);
    // Rows whose fields all fit the lines left them are built alike under any as many.
    let built = state
        .showcase_sheet_rows
        .borrow()
        .as_ref()
        .and_then(|shared| {
            shared
                .iter()
                .find(|(seen, (left, wanted), _)| {
                    *seen == at && (*left == most || (*wanted <= *left && *wanted <= most))
                })
                .map(|(_, (_, wanted), rows)| (*wanted, rows.clone()))
        });
    if let Some((wanted, rows)) = built {
        crate::native_settings::field_lines_noted(wanted);
        return rows;
    }
    let (rows, wanted) = crate::native_settings::field_lines_wanted(|| {
        SHOWCASE_COLOR_KEYS
            .into_iter()
            .map(|key| customize_row(state, key, width, one_line))
            .collect::<std::rc::Rc<[_]>>()
    });
    if let Some(shared) = state.showcase_sheet_rows.borrow_mut().as_mut() {
        shared.push((at, (most, wanted), rows.clone()));
    }
    rows
}

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
    let (blocks, wanted) = crate::native_settings::field_lines_wanted(|| {
        let rows = sheet_colour_rows(state, width, one_line);
        sheet_blocks(state, (width, room), &rows)
    });
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
/// [`BlocksShared`] is open; unpainted, kept for later renders ([`ShowcaseState::blocks_kept`]).
fn with_blocks<R>(
    state: &SettingsViewState,
    (width, narrow, query): (f32, bool, &str),
    painted: bool,
    read: impl FnOnce(&SharedBlocks) -> R,
) -> R {
    let at = (width.to_bits(), narrow, query.to_string());
    let build = |at, from| SharedBlocks {
        blocks: build_list_blocks(state, width, narrow, query, painted),
        at,
        painted,
        from,
    };
    let mut shared = state.showcase_blocks.borrow_mut();
    let Some(shared) = shared.as_mut() else {
        return read(&build(at, None));
    };
    let index = match shared
        .iter()
        .position(|entry| entry.at == at && (entry.painted || !painted))
    {
        Some(index) => index,
        None => {
            let from = (!painted).then(|| blocks_from(state, narrow));
            let mut kept = state.showcase.blocks_kept.borrow_mut();
            let known = from.as_ref().and_then(|from| {
                kept.iter()
                    .position(|entry| entry.at == at && entry.from.as_ref() == Some(from))
            });
            shared.push(match known {
                Some(index) => kept.remove(index),
                None => {
                    drop(kept);
                    build(at, from)
                }
            });
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
    // The slab and tags a built row lays out by, measured once a row is built: kept rows need none.
    let measures = std::cell::OnceCell::new();
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
            width,
        ));
        for row in attention {
            blocks.push((
                attention_row(state, row, width),
                attention_row_height(),
                None,
            ));
        }
    }
    let visible: Vec<usize> = if every {
        (0..showcase.cards.len()).collect()
    } else {
        showcase.visible_for(query)
    };
    blocks.reserve(visible.len() + 2);
    let community_total = showcase
        .cards
        .iter()
        .filter(|card| card.section == Section::Community)
        .count();
    let configured = state.configured_theme_id();
    let highlighted = state.highlighted_slug();
    // Rows as tall without at a width kept ([`chooser_rows_tallest`]) are not measured again.
    let kept = (!painted)
        .then(|| rows_kept_at(state, width, narrow))
        .flatten();
    let mut section = None;
    let mut rows_before = !blocks.is_empty();
    for index in &visible {
        let card = &showcase.cards[*index];
        if section != Some(card.section) {
            section = Some(card.section);
            if card.section == Section::Community && query.is_empty() {
                blocks.push(pack_row(state, community_total, rows_before, width));
            } else if let Some(caption) = card.section.caption_after_rows(rows_before) {
                blocks.push(section_heading(card.section, caption, rows_before, width));
            }
        }
        let in_use = configured.eq_ignore_ascii_case(&card.id);
        let selected = highlighted.as_deref() == Some(card.slug.as_str());
        let (row, height) = match kept.as_ref().map(|(rows, side)| rows[*index][*side]) {
            Some(height) => (
                unbuilt_row(format!("{CARD_PREFIX}{}", card.slug), height),
                height,
            ),
            None => {
                let (slab, tags) =
                    *measures.get_or_init(|| (slab_column(state, width, narrow), TagWidths::now()));
                row_node(
                    state,
                    card,
                    (width, narrow, slab, tags),
                    (in_use, selected),
                    painted,
                )
            }
        };
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
    width: f32,
) -> (UiNode, f32, Option<String>) {
    // A band of whole rows ([`on_grid`]), its words at its foot, at least [`SECTION_ABOVE`]
    // over them where it follows a row.
    let (_, words) = caption_words(section, caption, width);
    let height = on_grid(words + if after_rows { SECTION_ABOVE } else { 0.0 });
    section_band(section, caption, height, width)
}

/// A section's caption on the lines a row `width` wide sets it on, and their height: never cut
/// ("COMMU…" over the open Community section's rows at 2x in a 178pt window).
fn caption_words(section: Section, caption: &str, width: f32) -> (UiNode, f32) {
    super::wrapped_words_node(
        &format!("settings/showcase/section/{}", section.word()),
        caption,
        (SemanticRole::Heading, StyleRef::Quiet),
        (width - TEXT_INSET).max(1.0),
        caption_height(),
    )
}

/// A section's caption in a band `height` tall, its words at its foot.
fn section_band(
    section: Section,
    caption: &str,
    height: f32,
    width: f32,
) -> (UiNode, f32, Option<String>) {
    let (words, tall) = caption_words(section, caption, width);
    let above = (height - tall).max(0.0);
    let key = format!("settings/showcase/section/{}", section.word());
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

/// A theme row `height` tall read for its key and height alone: no control is built.
fn unbuilt_row(key: String, height: f32) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::ListBoxOption)),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(height)),
    )
}

/// A theme's row and height; `painted`, its control with its paint, else its key alone, as tall.
fn row_node(
    state: &SettingsViewState,
    card: &ShowcaseCard,
    (width, narrow, slab, tags): (f32, bool, Option<f32>, TagWidths),
    (in_use, selected): (bool, bool),
    painted: bool,
) -> (UiNode, f32) {
    let key = format!("{CARD_PREFIX}{}", card.slug);
    let height = row_height();
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
    let metrics = crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Primary);
    let check = in_use || ui_pieces_fit(&card.label, (metrics.1, metrics.0), name_width);
    if !check {
        name_width += check_width + 6.0;
    }
    let words = (SemanticRole::Text, StyleRef::Primary);
    // Only a painted row sets its name's lines; one as tall without counts them.
    let lines = painted.then(|| wrapped_words_lines(&card.label, words, name_width));
    let line_count = lines.as_ref().map_or_else(
        || wrapped_words_line_count(&card.label, words, name_width),
        Vec::len,
    );
    let one_line = line_count <= 1;
    let row = |content: UiContent, height: f32| {
        UiNode::new(key.clone(), content).layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(height)),
        )
    };
    let lines_height = line_count.max(1) as f32 * row_name_line_height();
    let tall = if one_line {
        height
    } else {
        // A name on more lines than one makes the row as tall as they take, on the list's
        // grid ([`on_grid`]), its lines and its slab centred in it.
        on_grid(height.max(lines_height + 2.0 * ROW_NAME_PAD))
    };
    // Rows as tall without are read for their keys and heights alone: none builds their control.
    if !painted {
        return (unbuilt_row(key.clone(), tall), tall);
    }
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
        lines.unwrap_or_default(),
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
    (row(option, tall).children(children), tall)
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
