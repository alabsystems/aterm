// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scroll positioning and focus reveal for the settings window.
use std::ops::Range;

use crate::native_ui::{
    AuditedCustomNode, Flow, GroupSpec, Insets, Layout, Length, MESSAGES_SCROLL_AUDIT,
    PAGE_SCROLL_AUDIT, SemanticRole, StyleRef, UiContent, UiKey, UiNode,
};

use super::{SettingsRoute, SettingsViewState};

/// The least of a card a list's edge leaves in view that reads as the card going on.
pub(super) const PEEK_MIN: f32 = crate::native_ui::LIST_PEEK_MIN;

/// The keyboard's control, and for each height a stop lays it out, the words it paints itself.
type WatchedWords = (UiKey, Vec<(f32, Option<crate::native_ui::WordBands>)>);

/// The words whose cut makes a list draw a control as a picture, as bands from its top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Words {
    /// Its own, which an edge crossing one of their lines or icons cuts.
    Own(crate::native_ui::WordBands),
    /// Its named row's, which a cut of the control leaves out of view.
    Row(f32, f32),
    /// None: a cut paints what is in view.
    Nothing,
}

thread_local! {
    /// The keyboard's control while a page is built ([`KeyboardWords`]).
    static KEYBOARD_WORDS: std::cell::RefCell<Option<WatchedWords>> =
        const { std::cell::RefCell::new(None) };
    /// The plans lists were laid out by this render, while [`PlansShared`] is open.
    static PLANS: std::cell::RefCell<Option<Vec<SharedPlan>>> =
        const { std::cell::RefCell::new(None) };
}

/// A list's items, its gap, room and what a bar takes of it, and the plan and stops they lay out.
type SharedPlan = (
    (std::rc::Rc<[ListItem]>, [u32; 3]),
    std::rc::Rc<(ScrollPlan, ItemStops)>,
);

/// A RENDER'S SEATS AND TELLINGS SHARE THEIR LISTS' PLANS: while open, a list's plan in a room
/// under a bar is laid out once ([`ListLaid::planned`], [`list_page_whole`]).
pub(super) struct PlansShared;

impl PlansShared {
    pub(super) fn open() -> Self {
        PLANS.with(|plans| plans.replace(Some(Vec::new())));
        Self
    }
}

impl Drop for PlansShared {
    fn drop(&mut self) {
        PLANS.with(|plans| plans.replace(None));
    }
}

/// The plan and stops `items`, `gap` apart, lay out in a list `room` tall under a bar that takes
/// `given` of it, as `plan` lays them out: once a render while [`PlansShared`] is open.
fn shared_plan(
    items: &std::rc::Rc<[ListItem]>,
    (gap, room, given): (f32, f32, f32),
    plan: impl FnOnce() -> (ScrollPlan, ItemStops),
) -> std::rc::Rc<(ScrollPlan, ItemStops)> {
    let at = [gap.to_bits(), room.to_bits(), given.to_bits()];
    let known = PLANS.with(|plans| {
        plans.borrow().as_ref().and_then(|plans| {
            plans
                .iter()
                .find(|((seen, was), _)| *was == at && std::rc::Rc::ptr_eq(seen, items))
                .map(|(_, planned)| std::rc::Rc::clone(planned))
        })
    });
    known.unwrap_or_else(|| {
        let planned = std::rc::Rc::new(plan());
        PLANS.with(|plans| {
            if let Some(plans) = plans.borrow_mut().as_mut() {
                plans.push((
                    (std::rc::Rc::clone(items), at),
                    std::rc::Rc::clone(&planned),
                ));
            }
        });
        planned
    })
}

/// While a page is built, what a screen shows of the keyboard's control is what it paints of
/// it: a list's edge that crosses its words draws it as a picture only
/// ([`ScrollPlan::painted_of_span_on`]).
pub(super) struct KeyboardWords;

impl KeyboardWords {
    pub(super) fn watch(key: Option<UiKey>) -> Self {
        KEYBOARD_WORDS.with(|words| words.replace(key.map(|key| (key, Vec::new()))));
        Self
    }

    /// The words naming the keyboard's `node`, `height` tall at `top`: its own, else its row's.
    fn of_node(
        node: &UiNode,
        top: f32,
        height: f32,
        rows: &[(&UiNode, f32, f32)],
    ) -> Option<(UiKey, Words)> {
        let own = KEYBOARD_WORDS.with(|words| {
            let mut words = words.borrow_mut();
            let (key, owns) = words.as_mut().filter(|(key, _)| node.key == *key)?;
            let known = owns
                .iter()
                .find(|(tall, _)| (tall - height).abs() < 0.01)
                .map(|(_, own)| *own);
            Some(known.unwrap_or_else(|| {
                let own = crate::native_ui::words_bands(key, &node.content, height);
                owns.push((height, own));
                own
            }))
        })?;
        let row = || {
            rows.iter().rev().find_map(|(row, at, tall)| {
                crate::native_ui::named_row_words(row, *tall)
                    .map(|(from, to)| Words::Row(at + from - top, at + to - top))
            })
        };
        let words = own.map_or_else(|| row().unwrap_or(Words::Nothing), Words::Own);
        Some((node.key.clone(), words))
    }
}

impl Drop for KeyboardWords {
    fn drop(&mut self) {
        KEYBOARD_WORDS.with(|words| words.replace(None));
    }
}

/// The strip under a list a page keeps for a status bar ([`seat_room`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Overhang {
    pub(crate) now: f32,
}

impl Overhang {
    /// No strip: a list that keeps no room for a bar.
    pub(crate) const NONE: Self = Self { now: 0.0 };
}

/// How a screen that does not show the whole list ends at the list's bottom edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ListFoot {
    #[default]
    Peek,
    Whole,
}

/// A page measured as stops ([`ScrollPlan::card`], [`ScrollPlan::row`]) and the room its list has.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ScrollPlan {
    heights: Shared<f32>,
    groups: Shared<Option<usize>>,
    heads: Shared<f32>,
    leads: Shared<f32>,
    captions: Shared<Option<(f32, f32)>>,
    card_captions: Shared<Option<CardCaption>>,
    notes: Shared<bool>,
    row_gaps: Shared<f32>,
    keys: Shared<Vec<UiKey>>,
    ids: Shared<Vec<UiKey>>,
    within: Shared<Vec<UiKey>>,
    previews: Shared<Vec<UiKey>>,
    control_rows: Shared<Vec<(f32, f32, f32)>>,
    drawn_bands: Shared<Vec<DrawnPart>>,
    spans: Shared<Vec<(UiKey, f32, f32)>>,
    words: Shared<Option<(UiKey, Words)>>,
    ink: Shared<f32>,
    ink_top: Shared<f32>,
    first_controls: Shared<Option<f32>>,
    room: f32,
    overhang: f32,
    given: f32,
    held_start: Option<HeldStart>,
    gap: f32,
    seated: Option<Seat>,
    segments: Shared<(UiKey, UiKey)>,
    /// Each segmented control's segments in order, the chosen one under the control's key
    /// ([`Self::segment_place`]).
    segment_sets: Shared<Vec<UiKey>>,
    foot: ListFoot,
    /// The pinned keyboard's control: how tall, and what a status's bar leaves ([`Left::pinned`]).
    pin: Option<(f32, f32)>,
    kept_limit: KeptLimit,
}

/// [`ScrollPlan::limit`] once worked out, asked many times a render: every change to the plan
/// through its methods forgets it. The seat the render chose ([`ScrollPlan::seated`], set in
/// place) is no part of it.
#[derive(Clone, Default)]
struct KeptLimit(std::cell::Cell<Option<usize>>);

impl KeptLimit {
    const fn forget(&mut self) {
        *self.0.get_mut() = None;
    }
}

/// A kept answer is no part of the plan: plans that differ by it alone are equal.
impl PartialEq for KeptLimit {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for KeptLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeptLimit")
    }
}

/// Per-stop and per-group lists, shared by a render's copies and copied only by one changing them.
type Shared<T> = std::sync::Arc<Vec<T>>;

/// Where the render seated a screen off its stops ([`ScrollPlan::seated`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Seat {
    pub(super) top: usize,
    pub(super) nudge: f32,
    pub(super) bare: bool,
}

impl Seat {
    /// A press's nudge of the screen whose first stop is `top`.
    pub(super) const fn nudged(top: usize, nudge: f32) -> Self {
        Self {
            top,
            nudge,
            bare: false,
        }
    }
}

/// Where a stop a screen draws sits on it ([`ScrollPlan::drawn_bands`]), from the list's top.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(super) struct DrawnBand {
    pub(super) stop: usize,
    pub(super) at: f32,
    pub(super) height: f32,
    pub(super) ink: (f32, f32),
    /// Where its card ends if nothing follows it: its closing padding and any
    /// ink past its height included, as the list's end is measured.
    pub(super) end: f32,
    pub(super) floor: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CardCaption {
    top: f32,
    bottom: f32,
    lead: f32,
    rows: f32,
}

/// One stretch of a screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ScrollRun {
    Card(usize),
    Group { group: usize, rows: Range<usize> },
}

/// How the last screen drawn at stop `top` sat, held while nothing scrolls the list
/// ([`ScrollPlan::held_seat`]): a stop that grows or shrinks in place (a Timer opened under the
/// pointer) re-decides neither, so it moves nothing on the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct HeldStart {
    top: usize,
    /// Whether it let its run's caption go ([`ScrollPlan::starts_bare`]).
    bare: bool,
    /// How far it was nudged so the next card peeks ([`ScrollPlan::peek_seat`]).
    peek: f32,
}

/// What one screen of a list draws ([`ScrollPlan::screen`]).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Screen {
    pub(super) view: Range<usize>,
    pub(super) drawn: Range<usize>,
    pub(super) offset: f32,
    pub(super) height: f32,
    pub(super) live: f32,
    pub(super) fades: bool,
    pub(super) whole: bool,
    pub(super) caption: Option<TopCaption>,
    pub(super) edge_caption: Option<(f32, f32)>,
    pub(super) bare: bool,
}

/// The caption of the run a screen starts in, at the list's top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TopCaption {
    pub(super) lead: f32,
    pub(super) kept: bool,
}

/// Where the list's last screen is seated ([`ScrollPlan::last_seat`]), as its [`Screen`] is.
struct LastSeat {
    top: usize,
    drawn: Range<usize>,
    offset: f32,
    caption: Option<TopCaption>,
    bare: bool,
}

impl ScrollPlan {
    pub(super) fn new(gap: f32) -> Self {
        Self {
            gap,
            ..Self::default()
        }
    }

    /// [`Self::new`] with room for `stops` stops, so a plan built stop by stop does not regrow.
    fn with_stops(gap: f32, stops: usize) -> Self {
        fn shared<T>(stops: usize) -> Shared<T> {
            std::sync::Arc::new(Vec::with_capacity(stops))
        }
        Self {
            heights: shared(stops),
            groups: shared(stops),
            card_captions: shared(stops),
            notes: shared(stops),
            keys: shared(stops),
            ids: shared(stops),
            within: shared(stops),
            previews: shared(stops),
            control_rows: shared(stops),
            spans: shared(stops),
            words: shared(stops),
            ink: shared(stops),
            ink_top: shared(stops),
            first_controls: shared(stops),
            ..Self::new(gap)
        }
    }

    /// The height the list has.
    pub(super) const fn set_room(&mut self, room: f32) {
        self.kept_limit.forget();
        self.room = room;
    }

    /// How the list's screens end at its bottom edge ([`ListFoot`]).
    pub(super) const fn set_foot(&mut self, foot: ListFoot) {
        self.kept_limit.forget();
        self.foot = foot;
    }

    /// Say the page pins the keyboard's control, `pin` its height and what a status's bar leaves.
    pub(super) const fn pin_keyboard(&mut self, pin: Option<(f32, f32)>) {
        self.kept_limit.forget();
        self.pin = pin;
    }

    /// Say that the list's room is `given` shorter than the page kept it.
    pub(super) const fn give_to_status(&mut self, given: f32) {
        self.kept_limit.forget();
        self.given = given.max(0.0);
    }

    /// The strip kept under the list for a status bar that is not up ([`seat_room`]).
    const fn set_overhang(&mut self, overhang: f32) {
        self.kept_limit.forget();
        self.overhang = overhang;
    }

    /// Hold the last screen at its first stop as it sat ([`HeldStart`]), or hold none.
    const fn hold_start(&mut self, held: Option<HeldStart>) {
        self.kept_limit.forget();
        self.held_start = held;
    }

    /// Whether the list's screens give room up to a plain status's taller bar.
    pub(super) fn gives_to_status(&self) -> bool {
        self.given > 0.01
    }

    /// This plan's stops and room as measured, with no screen seated on them.
    pub(super) fn unseated(&self) -> Self {
        let mut plan = Self {
            seated: None,
            ..self.clone()
        };
        // The limit reads the held start: the kept one goes with it.
        plan.hold_start(None);
        plan
    }

    #[cfg(test)]
    pub(super) const fn foot(&self) -> ListFoot {
        self.foot
    }

    pub(super) const fn room(&self) -> f32 {
        self.room
    }

    /// The room kept under the list for a status bar that is not up.
    #[cfg(test)]
    pub(super) const fn overhang(&self) -> f32 {
        self.overhang
    }

    pub(super) const fn gap(&self) -> f32 {
        self.gap
    }

    /// The seat the render chose off its stops ([`Self::seated`]).
    #[cfg(test)]
    pub(super) const fn seat(&self) -> Option<Seat> {
        self.seated
    }

    /// Whether stop `index` draws anything.
    #[cfg(test)]
    pub(super) fn paints(&self, index: usize) -> bool {
        self.ink[index] > self.ink_top[index] + 0.01
    }

    pub(super) fn len(&self) -> usize {
        self.heights.len()
    }

    /// Where each stop the screen whose first stop is `top` draws sits on it ([`Self::screen`]).
    #[cfg(test)]
    pub(super) fn drawn_bands(&self, top: usize) -> Vec<DrawnBand> {
        let screen = self.screen(top.min(self.limit()));
        screen
            .drawn
            .clone()
            .map(|stop| {
                let at = self.stop_top(screen.drawn.start, stop) - screen.offset;
                DrawnBand {
                    stop,
                    at,
                    height: self.heights[stop],
                    ink: (at + self.ink_top[stop], at + self.ink[stop]),
                    end: at + self.end_under(stop),
                    floor: self.caption_floor(screen.caption, &screen.drawn, stop),
                }
            })
            .collect()
    }

    /// A stop that is a whole card, `height` tall, holding `keys`.
    pub(super) fn card(&mut self, height: f32, keys: Vec<UiKey>) -> usize {
        self.kept_limit.forget();
        std::sync::Arc::make_mut(&mut self.heights).push(height);
        std::sync::Arc::make_mut(&mut self.ink).push(height);
        std::sync::Arc::make_mut(&mut self.ink_top).push(0.0);
        std::sync::Arc::make_mut(&mut self.first_controls)
            .push((!keys.is_empty()).then_some(height));
        std::sync::Arc::make_mut(&mut self.previews).push(Vec::new());
        std::sync::Arc::make_mut(&mut self.card_captions).push(None);
        std::sync::Arc::make_mut(&mut self.notes).push(false);
        std::sync::Arc::make_mut(&mut self.groups).push(None);
        std::sync::Arc::make_mut(&mut self.control_rows).push(if keys.is_empty() {
            Vec::new()
        } else {
            vec![(0.0, 0.0, height)]
        });
        std::sync::Arc::make_mut(&mut self.drawn_bands).push(Vec::new());
        std::sync::Arc::make_mut(&mut self.keys).push(keys);
        std::sync::Arc::make_mut(&mut self.ids).push(Vec::new());
        std::sync::Arc::make_mut(&mut self.within).push(Vec::new());
        std::sync::Arc::make_mut(&mut self.spans).push(Vec::new());
        std::sync::Arc::make_mut(&mut self.words).push(None);
        self.heights.len() - 1
    }

    /// [`Self::card`] put in at stop `index`, before the stops there.
    pub(super) fn insert_card(&mut self, index: usize, height: f32, keys: Vec<UiKey>) {
        self.kept_limit.forget();
        let index = index.min(self.heights.len());
        std::sync::Arc::make_mut(&mut self.heights).insert(index, height);
        std::sync::Arc::make_mut(&mut self.ink).insert(index, height);
        std::sync::Arc::make_mut(&mut self.ink_top).insert(index, 0.0);
        std::sync::Arc::make_mut(&mut self.first_controls)
            .insert(index, (!keys.is_empty()).then_some(height));
        std::sync::Arc::make_mut(&mut self.previews).insert(index, Vec::new());
        std::sync::Arc::make_mut(&mut self.card_captions).insert(index, None);
        std::sync::Arc::make_mut(&mut self.notes).insert(index, false);
        std::sync::Arc::make_mut(&mut self.groups).insert(index, None);
        std::sync::Arc::make_mut(&mut self.control_rows).insert(
            index,
            if keys.is_empty() {
                Vec::new()
            } else {
                vec![(0.0, 0.0, height)]
            },
        );
        std::sync::Arc::make_mut(&mut self.drawn_bands).insert(index, Vec::new());
        std::sync::Arc::make_mut(&mut self.keys).insert(index, keys);
        std::sync::Arc::make_mut(&mut self.ids).insert(index, Vec::new());
        std::sync::Arc::make_mut(&mut self.within).insert(index, Vec::new());
        std::sync::Arc::make_mut(&mut self.spans).insert(index, Vec::new());
        std::sync::Arc::make_mut(&mut self.words).insert(index, None);
    }

    /// A group card.
    pub(super) fn group(&mut self, head: f32, lead: f32) -> usize {
        self.kept_limit.forget();
        std::sync::Arc::make_mut(&mut self.heads).push(head);
        std::sync::Arc::make_mut(&mut self.leads).push(lead.max(0.0));
        std::sync::Arc::make_mut(&mut self.captions).push(None);
        std::sync::Arc::make_mut(&mut self.row_gaps).push(0.0);
        self.heads.len() - 1
    }

    /// Say that `group`'s card sets `gap` between its rows.
    pub(super) fn set_row_gap(&mut self, group: usize, gap: f32) {
        self.kept_limit.forget();
        if let Some(row_gap) = std::sync::Arc::make_mut(&mut self.row_gaps).get_mut(group) {
            *row_gap = gap.max(0.0);
        }
    }

    fn run_rows(&self, group: usize, rows: &Range<usize>) -> f32 {
        let stops = self.heights[rows.clone()].iter().sum::<f32>();
        (stops - self.row_gaps.get(group).copied().unwrap_or(0.0)).max(0.0)
    }

    /// Say that `group`'s card paints a caption from `top` under its top.
    pub(super) fn set_caption(&mut self, group: usize, top: f32, bottom: f32) {
        self.kept_limit.forget();
        if let Some(caption) = std::sync::Arc::make_mut(&mut self.captions).get_mut(group) {
            *caption = Some((top.max(0.0), bottom.max(top)));
        }
    }

    fn lead_of(&self, index: usize) -> f32 {
        self.groups[index].map_or(0.0, |group| self.leads[group])
    }

    fn closing(&self, group: usize) -> f32 {
        self.heads[group] - self.leads[group]
    }

    fn starts_run(&self, index: usize) -> bool {
        index == 0 || self.groups[index].is_none() || self.groups[index] != self.groups[index - 1]
    }

    /// Say where the controls of the last stop added sit.
    pub(super) fn set_control_rows(&mut self, rows: Vec<(f32, f32, f32)>) {
        self.kept_limit.forget();
        if let Some(first) = std::sync::Arc::make_mut(&mut self.first_controls).last_mut() {
            *first = rows.first().map(|(_, at, height)| at + height);
        }
        if let Some(last) = std::sync::Arc::make_mut(&mut self.control_rows).last_mut() {
            *last = rows;
        }
    }

    /// Say what the last stop added draws, band by band ([`drawn_bands_in`]).
    pub(super) fn set_drawn_bands(&mut self, bands: Vec<DrawnPart>) {
        self.kept_limit.forget();
        if let Some(last) = std::sync::Arc::make_mut(&mut self.drawn_bands).last_mut() {
            *last = bands;
        }
    }

    /// Say how far under its top the last stop added has its first row of controls end.
    pub(super) fn set_first_controls(&mut self, end: Option<f32>) {
        self.kept_limit.forget();
        if let (Some(first), Some(end)) = (
            std::sync::Arc::make_mut(&mut self.first_controls).last_mut(),
            end,
        ) {
            *first = Some(first.map_or(end, |was| was.min(end)));
        }
    }

    /// Say how far under its top what the last stop added draws ends.
    pub(super) fn set_ink(&mut self, ink: f32) {
        self.kept_limit.forget();
        if let (Some(last), Some(height)) = (
            std::sync::Arc::make_mut(&mut self.ink).last_mut(),
            self.heights.last(),
        ) {
            *last = ink.clamp(0.0, *height);
        }
    }

    /// Say how far under its top what the last stop added draws begins.
    pub(super) fn set_ink_top(&mut self, top: f32) {
        self.kept_limit.forget();
        if let (Some(last), Some(ink)) = (
            std::sync::Arc::make_mut(&mut self.ink_top).last_mut(),
            self.ink.last(),
        ) {
            *last = top.clamp(0.0, *ink);
        }
    }

    /// Say that the last stop added is a note on the rows over it.
    pub(super) fn set_note(&mut self, note: bool) {
        self.kept_limit.forget();
        if let Some(last) = std::sync::Arc::make_mut(&mut self.notes).last_mut() {
            *last = note;
        }
    }

    fn paired(&self, index: usize) -> bool {
        let next = index + 1;
        if next >= self.len()
            || !self.notes[next]
            || self.notes[index]
            || self.control_rows[index].is_empty()
            || self.starts_run(next)
        {
            return false;
        }
        let at = self.lead_of(index) - self.seat_drop(index);
        let at_note = self.next_top(at, next);
        self.shows_at(at, index, self.room)
            && if next + 1 == self.len() {
                self.shows_end_at(at_note, next)
            } else {
                self.shows_at(at_note, next, self.room)
            }
    }

    /// Whether stop `index` is no seat for a scroll by stops.
    pub(super) fn passes_over(&self, index: usize) -> bool {
        index > 0 && index < self.len() && self.paired(index - 1)
    }

    /// Say where each control the last stop added holds sits in it.
    pub(super) fn set_control_spans(&mut self, spans: Vec<(UiKey, f32, f32)>) {
        self.kept_limit.forget();
        if let Some(last) = std::sync::Arc::make_mut(&mut self.spans).last_mut() {
            *last = spans;
        }
    }

    /// Say which words name the keyboard's control where the last stop added lays it out.
    pub(super) fn set_keyboard_words(&mut self, words: Option<(UiKey, Words)>) {
        self.kept_limit.forget();
        if let Some(last) = std::sync::Arc::make_mut(&mut self.words).last_mut() {
            *last = words;
        }
    }

    /// Span of stop `index` revealed for control `key` ([`Self::revealed_span`]).
    pub(super) fn span_of(&self, index: usize, key: Option<&UiKey>, reveal: Reveal) -> (f32, f32) {
        let pressed = matches!(reveal, Reveal::Pressed | Reveal::Hold) && self.gives_to_status();
        key.filter(|_| pressed)
            .and_then(|key| self.control_span(index, key))
            .unwrap_or_else(|| self.revealed_span(index, reveal))
    }

    fn control_span(&self, index: usize, key: &UiKey) -> Option<(f32, f32)> {
        // The control's own span, else its segmented control's: a segment on a line of its own
        // took the span of the chosen one where that came first.
        let spans = self.spans.get(index)?;
        let control = self.resolve(key);
        spans
            .iter()
            .find(|(span, _, _)| span == key)
            .or_else(|| spans.iter().find(|(span, _, _)| span == control))
            .map(|(_, top, height)| (*top, top + height))
    }

    /// Measure the last stop added from `node`, laid out `height` tall.
    pub(super) fn measure(&mut self, node: &UiNode, height: f32) {
        self.kept_limit.forget();
        let mut within = Vec::new();
        keys_within(node, &mut within);
        self.set_within(within);
        let (spans, words) = control_spans_in(node, height);
        self.set_control_spans(spans);
        self.set_keyboard_words(words);
        self.set_control_rows(control_rows_in(node, height));
        self.set_drawn_bands(drawn_bands_in(node, height));
        self.set_first_controls(first_control_row(node, height).map(|(top, tall)| top + tall));
        let content = content_extent(node, height);
        self.set_ink(content.map_or(0.0, |(top, tall)| top + tall));
        self.set_ink_top(content.map_or(0.0, |(top, _)| top));
        if let Some(last) = std::sync::Arc::make_mut(&mut self.card_captions).last_mut() {
            *last = caption_extent(node);
        }
    }

    fn stop_top(&self, from: usize, index: usize) -> f32 {
        if index <= from {
            return self.lead_of(index);
        }
        let mut top = self.lead_of(from);
        for k in from + 1..=index {
            top = self.next_top(top, k);
        }
        top
    }

    fn next_top(&self, before: f32, k: usize) -> f32 {
        let after = before + self.heights[k - 1];
        if self.starts_run(k) {
            after
                + self.groups[k - 1].map_or(0.0, |group| self.closing(group))
                + self.gap
                + self.lead_of(k)
        } else {
            after
        }
    }

    fn card_top(&self, from: usize, index: usize) -> f32 {
        let top = self.stop_top(from, index);
        if index == from || self.starts_run(index) {
            top - self.lead_of(index)
        } else {
            top
        }
    }

    fn card_bottom(&self, from: usize, index: usize) -> f32 {
        self.stop_top(from, index)
            + self.heights[index]
            + self.groups[index].map_or(0.0, |group| self.closing(group))
    }

    fn end_under(&self, index: usize) -> f32 {
        (self.heights[index] + self.groups[index].map_or(0.0, |group| self.closing(group)))
            .max(self.ink[index])
    }

    fn shows_end_at(&self, at: f32, index: usize) -> bool {
        at + self.controls_of(index).1 <= self.room + 0.01
            && at + self.end_under(index) <= self.room + self.overhang + 0.01
    }

    /// A row of `group`, `height` tall with its share of the card's gaps.
    pub(super) fn row(&mut self, group: usize, height: f32, keys: Vec<UiKey>) -> usize {
        self.kept_limit.forget();
        std::sync::Arc::make_mut(&mut self.heights).push(height);
        std::sync::Arc::make_mut(&mut self.ink).push(height);
        std::sync::Arc::make_mut(&mut self.ink_top).push(0.0);
        std::sync::Arc::make_mut(&mut self.first_controls)
            .push((!keys.is_empty()).then_some(height));
        std::sync::Arc::make_mut(&mut self.previews).push(Vec::new());
        std::sync::Arc::make_mut(&mut self.card_captions).push(None);
        std::sync::Arc::make_mut(&mut self.notes).push(false);
        std::sync::Arc::make_mut(&mut self.groups).push(Some(group));
        std::sync::Arc::make_mut(&mut self.control_rows).push(if keys.is_empty() {
            Vec::new()
        } else {
            vec![(0.0, 0.0, height)]
        });
        std::sync::Arc::make_mut(&mut self.drawn_bands).push(Vec::new());
        std::sync::Arc::make_mut(&mut self.keys).push(keys);
        std::sync::Arc::make_mut(&mut self.ids).push(Vec::new());
        std::sync::Arc::make_mut(&mut self.within).push(Vec::new());
        std::sync::Arc::make_mut(&mut self.spans).push(Vec::new());
        std::sync::Arc::make_mut(&mut self.words).push(None);
        self.heights.len() - 1
    }

    /// The stretches `range` draws as, top to bottom.
    pub(super) fn runs(&self, range: Range<usize>) -> Vec<ScrollRun> {
        let mut runs = Vec::new();
        let mut k = range.start;
        while k < range.end {
            match self.groups[k] {
                None => {
                    runs.push(ScrollRun::Card(k));
                    k += 1;
                }
                Some(group) => {
                    let start = k;
                    while k < range.end && self.groups[k] == Some(group) {
                        k += 1;
                    }
                    runs.push(ScrollRun::Group {
                        group,
                        rows: start..k,
                    });
                }
            }
        }
        runs
    }

    /// The height one run lays out at.
    pub(super) fn run_height(&self, run: &ScrollRun) -> f32 {
        match run {
            ScrollRun::Card(k) => self.heights[*k],
            ScrollRun::Group { group, rows } => {
                self.heads[*group] + self.heights[rows.clone()].iter().sum::<f32>()
            }
        }
    }

    /// The height `range` takes on screen, the gaps between its runs included.
    pub(super) fn span(&self, range: Range<usize>) -> f32 {
        // [`Self::runs`] summed run by run as [`Self::run_height`] sums each, building no list.
        let (mut span, mut runs, mut k) = (0.0, 0usize, range.start);
        while k < range.end {
            let start = k;
            k += 1;
            if let Some(group) = self.groups[start] {
                while k < range.end && self.groups[k] == Some(group) {
                    k += 1;
                }
                span += self.heads[group] + self.heights[start..k].iter().sum::<f32>();
            } else {
                span += self.heights[start];
            }
            runs += 1;
        }
        span + runs.saturating_sub(1) as f32 * self.gap
    }

    /// The height `range` takes on screen down to what its last stop draws: the blank a stop
    /// ends on (the line a row keeps for its Reset, [`row_parts`]) needs no room.
    pub(super) fn drawn_span(&self, range: Range<usize>) -> f32 {
        let Some(last) = range.end.checked_sub(1).filter(|_| !range.is_empty()) else {
            return 0.0;
        };
        let blank = (self.heights[last] - self.content_of(last).1).max(0.0);
        (self.span(range) - blank).max(0.0)
    }

    /// The stops a screen seated at its first stop `top` shows.
    pub(super) fn view(&self, top: usize) -> Range<usize> {
        let n = self.len();
        if n == 0 {
            return 0..0;
        }
        let top = top.min(n - 1);
        let mut end = top + 1;
        let drop = self.seat_drop(top);
        let mut at = self.stop_top(top, top) - drop;
        while end < n {
            at = self.next_top(at, end);
            let shown = if end + 1 == n {
                self.shows_end_at(at, end)
            } else {
                self.shows_at(at, end, self.room)
            };
            if !shown {
                break;
            }
            end += 1;
        }
        top..end
    }

    fn shows_at(&self, at: f32, index: usize, room: f32) -> bool {
        at + self.controls_of(index).1 <= room + 0.01
            && at + self.ink[index] <= room + self.overhang + 0.01
    }

    /// The last first stop the list scrolls to, worked out once for the plan as it stands
    /// ([`KeptLimit`]).
    pub(super) fn limit(&self) -> usize {
        if let Some(limit) = self.kept_limit.0.get() {
            return limit;
        }
        let limit = self.limit_now();
        self.kept_limit.0.set(Some(limit));
        limit
    }

    fn limit_now(&self) -> usize {
        let n = self.len();
        if n == 0 {
            return 0;
        }
        // Where the last screen stands its end is the same whatever its first stop: weighed once.
        let anchor = (self.foot != ListFoot::Whole).then(|| self.last_anchor());
        let mut top = n - 1;
        while top > 0 && self.ends_on_at(top - 1, anchor) {
            top -= 1;
        }
        top
    }

    /// [`Self::ends_on_at`], the last screen's anchor weighed here.
    #[cfg(test)]
    pub(super) fn ends_on(&self, top: usize) -> bool {
        self.ends_on_at(
            top,
            (self.foot != ListFoot::Whole).then(|| self.last_anchor()),
        )
    }

    /// Whether the screen whose first stop is `top`, drawn as the list's
    /// last screen, shows `top` and every stop after it ([`Self::limit`]),
    /// the caption of the run it starts in kept as that stop's own seat
    /// keeps it ([`Self::starts_bare`]). The caller weighs where that screen's end stands,
    /// `anchor` ([`Self::last_anchor`], none where the list's foot is whole); this reads the
    /// screen's seat alone, no live band.
    fn ends_on_at(&self, top: usize, anchor: Option<f32>) -> bool {
        let n = self.len();
        let Some(anchor) = anchor else {
            return self.view(top).end >= n;
        };
        let seat = self.last_seat(top, anchor);
        if seat.bare && !self.starts_bare(top) {
            return false;
        }
        let shown = self.shown_on(&seat.drawn, seat.offset, seat.caption);
        !shown.is_empty() && shown.start <= top && shown.end >= n
    }

    fn last_screen(&self, top: usize) -> Screen {
        let LastSeat {
            top,
            drawn,
            offset,
            caption,
            bare,
        } = self.last_seat(top, self.last_anchor());
        let reach = self.room + self.overhang;
        let bottom = self.span(drawn.clone());
        let height = (bottom - offset).min(reach).max(self.room.min(bottom));
        Screen {
            view: top..self.len(),
            live: self.live_on(&drawn, offset, caption, self.room),
            drawn,
            offset,
            height,
            fades: false,
            whole: false,
            caption,
            edge_caption: None,
            bare,
        }
    }

    /// How far down the list its last screen stands the list's end ([`Self::last_screen`]).
    fn last_anchor(&self) -> f32 {
        let n = self.len();
        let reach = self.room + self.overhang;
        // WHAT THE LAST STOP DRAWS WINS OVER THE BLANK UNDER IT: where the room cannot hold the
        // last stop from its first words to its card's end, the blank under what it draws (its
        // Reset line kept empty, the card's padding) falls past the room's edge, rather than
        // the list's end standing on the edge with the stop's control cut away above it and
        // drawn not at all (Home's last switch at 164x98, the screen blank).
        let content = self.ink[n - 1].max(self.controls_of(n - 1).1);
        let tail = (self.end_under(n - 1) - content).max(0.0);
        // A last stop taller than the room is seated at its top, as any stop is: the screens
        // under it step down it ([`Self::tall_steps`]).
        if self.taller_than_room(n - 1) {
            self.span(n - 1..n)
        } else if self.end_under(n - 1) - self.ink_top[n - 1] > reach + 0.01 {
            self.room + tail
        } else {
            (self.room + self.end_under(n - 1) - self.controls_of(n - 1).1)
                .min(reach)
                .max(self.room.min(reach))
        }
    }

    /// Where the list's last screen whose first stop is `top` is seated, its end at `anchor`
    /// ([`Self::last_anchor`]).
    fn last_seat(&self, top: usize, anchor: f32) -> LastSeat {
        let n = self.len();
        let top = top.min(n - 1);
        let mut start = top;
        while start > 0 && self.span(start..n) + 0.5 < anchor {
            start -= 1;
        }
        let seat = |start: usize| {
            let flush = (self.span(start..n) - anchor).max(0.0);
            let bare =
                start == top && (self.starts_bare(start) || flush > self.ink_top[start] + 0.01);
            let caption = self.top_caption(start..n, flush).map(|caption| TopCaption {
                kept: caption.kept && !bare,
                ..caption
            });
            (flush, bare, caption)
        };
        let (flush, mut bare, caption) = seat(start);
        // NO BAND OF BLANK OVER THE LAST SCREEN'S ROWS: seated with its end on the room's edge,
        // its top can fall inside the caption of the run it starts in, which a hard edge never
        // cuts: hidden, it left a band of blank over the whole rows under it (Packages' ACTIVITY
        // and its Open Log at 102x45 and 150x47, 33pt and 40pt over the first row). Past it, the
        // caption kept over the rows scrolling under it cut the first of them, whose cut lines
        // then drew nothing (Harness' UPDATES at 1074x485 over 17pt of a row, Appearance's
        // EFFECTS at 2x text over 54pt of Matrix rain's). The stop before the screen's own first
        // that filled the seat gives it up: the stop after it starts the screen whole under the
        // caption its run repeats over every screen of its rows, and the room it cannot fill
        // lies under the list's end, where nothing follows. Under a cut caption the screen's
        // first stop may instead start whole at the list's top, its caption scrolled off (a whole
        // row beats a kept caption, and names itself), where that leaves less room unfilled:
        // ACTIVITY's 64pt head stays over its 32pt rows at 150x47, 8pt to spare rather than 40pt,
        // and the Sound page's bonks, rows of 66pt under a 40pt caption, start whole at the top
        // at 102x29, 5pt to spare rather than 31pt.
        // Past its caption, a filler its seat cuts through every word and control draws nothing
        // of them (a row cut through its words draws not at all): a band of blank card stood over
        // the screen's first stop (Cursor & Motion at 738x418 under a status's bar: 32pt of
        // Effect policy's card, Serious Mode's row cut away). It is given up as well.
        let blank = caption.is_none() && self.draws_nothing_from(start, flush);
        // A filler whose control its seat cuts, so the list draws that control as a picture at
        // its top or not at all, is given up too: a band of card over the screen's first stop
        // held the switch's sliver and the note under it (Security at 584x376: Secure Keyboard
        // Entry's switch, 4pt of it).
        let veiled = caption.is_none() && self.veils_controls_from(start, flush);
        let group = self.groups[start].is_some();
        let cut = group && matches!(caption, Some(caption) if !caption.kept);
        let kept = group && matches!(caption, Some(caption) if caption.kept);
        let headed = start < top
            && (kept
                || blank
                || veiled
                || cut
                    && self.span(start + 1..n) + 0.5 >= self.span(start..n) - self.lead_of(start));
        let (offset, caption) = if headed {
            start += 1;
            let (flush, after, caption) = seat(start);
            bare = after;
            (flush, caption)
        } else if cut {
            (self.lead_of(start), None)
        } else {
            (flush, caption)
        };
        LastSeat {
            top,
            drawn: start..n,
            offset,
            caption,
            bare,
        }
    }

    /// The first stop of the screen that shows the stops just before `top` on to it.
    pub(super) fn page_before(&self, top: usize) -> usize {
        let top = top.min(self.len());
        let mut first = top.saturating_sub(1);
        while first > 0 && self.view(first - 1).end >= top {
            first -= 1;
        }
        first
    }

    fn whole_by(&self, top: usize, index: usize) -> f32 {
        self.stop_top(top, index) + self.controls_of(index).1
    }

    fn controls_of(&self, index: usize) -> (f32, f32) {
        // A loop, not an iterator chain: every screen a plan views asks this of each stop.
        let mut span: Option<(f32, f32)> = None;
        for (_, at, height) in &self.control_rows[index] {
            let end = at + height;
            span = Some(span.map_or((*at, end), |(top, bottom)| (top.min(*at), bottom.max(end))));
        }
        span.unwrap_or((0.0, self.ink[index]))
    }

    /// The room the first screen needs to show its first stop and the first row of controls
    /// after it ([`super::PageChrome`] sheds chrome until the list has it).
    pub(super) fn first_screen_need(&self) -> f32 {
        let n = self.len();
        if n == 0 {
            return 0.0;
        }
        let lead = self.lead_of(0);
        let first = if n == 1 {
            (lead + self.controls_of(0).1).max(lead + self.end_under(0) - self.overhang)
        } else {
            (lead + self.controls_of(0).1).max(lead + self.ink[0] - self.overhang)
        };
        let second = (1..n)
            .find_map(|stop| self.first_controls[stop].map(|end| self.stop_top(0, stop) + end))
            .unwrap_or(0.0);
        first.max(second)
    }

    /// Whether a screen that starts `offset` into stop `index` draws nothing of it: every word
    /// and control of it cut by the screen's top, which draws them only whole, and every picture
    /// above it ([`Self::last_screen`]). A stop measured for none says no.
    pub(super) fn draws_nothing_from(&self, index: usize, offset: f32) -> bool {
        let parts = &self.drawn_bands[index];
        let top = self.stop_top(index, index);
        !parts.is_empty()
            && parts.iter().all(|part| {
                if part.whole {
                    top + part.top < offset - 0.5
                } else {
                    top + part.bottom <= offset + 0.5
                }
            })
    }

    /// Whether a screen that starts `offset` into stop `index` cuts a control of it at its top so
    /// the list does not draw it (a sliver of it, or its name cut through), while words of the
    /// stop under it still show: a band of card holding them without the control they belong to
    /// ([`Self::last_screen`]).
    fn veils_controls_from(&self, index: usize, offset: f32) -> bool {
        let top = self.stop_top(index, index);
        let parts = &self.drawn_bands[index];
        let controls = self.control_rows.get(index).map_or(&[][..], Vec::as_slice);
        // Words cut through: a whole band no control row of the stop draws.
        let name_cut = parts.iter().any(|part| {
            part.whole
                && top + part.top < offset - 0.5
                && top + part.bottom > offset + 0.5
                && !controls.iter().any(|(_, at, height)| {
                    (part.top - at).abs() < 0.5 && (part.bottom - (at + height)).abs() < 0.5
                })
        });
        let Some(veiled) = self.control_rows.get(index).and_then(|rows| {
            rows.iter()
                .filter(|(_, at, height)| {
                    top + at < offset - 0.5 && (name_cut || top + at + height - offset < PEEK_MIN)
                })
                .map(|(_, at, height)| top + at + height)
                .reduce(f32::max)
        }) else {
            return false;
        };
        parts
            .iter()
            .any(|part| part.whole && top + part.top >= veiled.max(offset) - 0.5)
    }

    /// How many of the list's stops the screen that starts with each cuts.
    pub(super) fn cut_stops(&self) -> usize {
        (0..self.len()).filter(|&stop| self.cuts(stop)).count()
    }

    /// Number of groups whose first stop draws without its caption.
    pub(super) fn bare_captions(&self) -> usize {
        (0..self.len())
            .filter(|&stop| self.starts_run(stop) && self.starts_bare(stop))
            .count()
    }

    fn cuts(&self, index: usize) -> bool {
        // The last stop is cut where what it draws is: the blank under it may fall past the
        // room's edge ([`Self::last_screen`]).
        if index + 1 == self.len() {
            let top = self.ink_top[index];
            return self.controls_of(index).1 - top > self.room + 0.01
                || self.ink[index] - top > self.room + self.overhang + 0.01;
        }
        let at = self.lead_of(index) - self.seat_drop(index);
        at + self.controls_of(index).1 > self.room + 0.01
            || at + self.ink[index] > self.room + self.overhang + 0.01
    }

    /// The least scroll from `top` whose screen shows stop `index` ([`Self::shows_revealed`]).
    pub(super) fn reveal(&self, index: usize, top: usize) -> usize {
        let limit = self.limit();
        let top = top.min(limit);
        if self.view(top).contains(&index) || self.shows_revealed(index, top, Reveal::Keep) {
            return top;
        }
        if index < top {
            return index.min(limit);
        }
        let mut first = top + 1;
        while first < index && !self.view(first).contains(&index) {
            first += 1;
        }
        first.min(limit)
    }

    /// Record the segmented controls in `node`, a stop just added ([`UiNode::segmented`]).
    pub(super) fn add_segments(&mut self, node: &UiNode) {
        fn walk(node: &UiNode, out: &mut Vec<(UiKey, UiKey)>, sets: &mut Vec<Vec<UiKey>>) {
            if node.paint_only {
                return;
            }
            if node.segmented {
                if let Some(control) = segmented_control_key(node) {
                    let segments: Vec<UiKey> = crate::native_ui::segments_of(&node.children)
                        .into_iter()
                        .map(|(segment, _)| segment)
                        .collect();
                    out.extend(
                        segments
                            .iter()
                            .filter(|segment| **segment != control)
                            .map(|segment| (segment.clone(), control.clone())),
                    );
                    sets.push(segments);
                }
                return;
            }
            for child in &node.children {
                walk(child, out, sets);
            }
        }
        self.kept_limit.forget();
        walk(
            node,
            std::sync::Arc::make_mut(&mut self.segments),
            std::sync::Arc::make_mut(&mut self.segment_sets),
        );
    }

    /// Where segment `key` stands among its segmented control's segments: the control's key
    /// ([`Self::resolve`]) and the segment's place from the first. A segment is drawn under the
    /// control's key once chosen, so the key a press found is gone from the list the press
    /// brings; its place is not ([`PressedAt::key_in`]).
    pub(super) fn segment_place(&self, key: &UiKey) -> Option<(UiKey, usize)> {
        let control = self.resolve(key);
        self.segment_sets
            .iter()
            .find(|set| set.contains(control))?
            .iter()
            .position(|segment| segment == key)
            .map(|at| (control.clone(), at))
    }

    /// The key of the segment at place `at` of the segmented control `control` names
    /// ([`Self::segment_place`]).
    pub(super) fn segment_at(&self, control: &UiKey, at: usize) -> Option<&UiKey> {
        self.segment_sets
            .iter()
            .find(|set| set.contains(control))?
            .get(at)
    }

    /// The control `key` is to the list.
    pub(super) fn resolve<'a>(&'a self, key: &'a UiKey) -> &'a UiKey {
        self.segments
            .iter()
            .find(|(segment, _)| segment == key)
            .map_or(key, |(_, control)| control)
    }

    /// [`Self::reveal_span`] for a landing that selects stop `index` ([`Reveal::LandWhole`]).
    pub(super) fn reveal_selected(&self, index: usize, top: usize) -> usize {
        self.reveal_span(index, top, (0.0, self.heights[index]))
    }

    /// Land on selected stop `index`, drawn whole where the room allows ([`Reveal::LandWhole`]).
    pub(super) fn land_selected(&self, index: usize, top: usize) -> (usize, Option<Seat>) {
        let top = self.reveal_selected(index, top);
        let span = (0.0, self.heights[index]);
        if self.heights[index] > self.room + 0.01 || self.foot == ListFoot::Whole {
            return (top, None);
        }
        let base = self.stop_screen(top.min(self.limit()));
        if base.whole || !base.drawn.contains(&index) || self.whole_in_room(&base, index, span) {
            return (top, None);
        }
        (top, self.bare_seat(base, index, span))
    }

    fn bare_seat(&self, base: Screen, index: usize, (from, to): (f32, f32)) -> Option<Seat> {
        let at = self.stop_top(base.drawn.start, index) - base.offset;
        let end = self.card_bottom(base.drawn.start, self.len() - 1) - base.offset;
        let seat = Seat {
            top: base.view.start,
            nudge: at
                .max(at + to - self.room)
                .min(at + from)
                .min(end - self.room)
                .max(-base.offset),
            bare: true,
        };
        let screen = self.nudged(base, seat.nudge, true);
        self.whole_in_room(&screen, index, (from, to))
            .then_some(seat)
    }

    /// Where the stop screen from `top` cuts span `span` of stop `index` and the room could
    /// hold it, the screen nudged off its stops to show it whole, else a bare seat.
    pub(super) fn seat_span(&self, index: usize, top: usize, span: (f32, f32)) -> Option<Seat> {
        if span.1 - span.0 > self.room + 0.01 || self.foot == ListFoot::Whole {
            return None;
        }
        let base = self.stop_screen(top.min(self.limit()));
        if base.whole || index < base.drawn.start || self.whole_in_room(&base, index, span) {
            return None;
        }
        let at = self.stop_top(base.drawn.start, index) - base.offset;
        let end = self.card_bottom(base.drawn.start, self.len() - 1) - base.offset;
        let floor = self.caption_floor(base.caption, &base.drawn, index);
        let nudge = if at + span.1 > self.room + 0.01 {
            at + span.1 - self.room
        } else {
            at + span.0 - floor
        };
        if nudge <= end - self.room + 0.01 && base.offset + nudge >= -0.01 {
            let kept = self.nudged(base.clone(), nudge, false);
            if self.whole_in_room(&kept, index, span) {
                return Some(Seat::nudged(base.view.start, nudge));
            }
        }
        self.bare_seat(base, index, span)
    }

    fn whole_in_room(&self, screen: &Screen, index: usize, span: (f32, f32)) -> bool {
        let room = Screen {
            live: screen.live.min(self.room),
            ..screen.clone()
        };
        self.shows_span_on(&room, index, span)
    }

    /// Least scroll from `top` whose screen shows span `from..to` of stop `index` whole.
    pub(super) fn reveal_span(&self, index: usize, top: usize, (from, to): (f32, f32)) -> usize {
        let limit = self.limit();
        let top = top.min(limit);
        if self.shows_span(index, top, (from, to)) {
            return top;
        }
        if index <= top {
            return index.min(limit);
        }
        let mut first = top + 1;
        let shows = |first: usize| match self.foot {
            ListFoot::Peek => {
                self.stop_top(first, index) - self.seat_drop(first) + to <= self.room + 0.01
            }
            ListFoot::Whole => self.shows_span(index, first, (from, to)),
        };
        while first < index && !shows(first) {
            first += 1;
        }
        first.min(limit)
    }

    /// Whether the screen whose first stop is `top`, as it draws ([`Self::screen`]), shows stop
    /// `index` from `span.0` to `span.1` under its top whole in its room.
    pub(super) fn shows_span(&self, index: usize, top: usize, span: (f32, f32)) -> bool {
        self.shows_span_on(&self.screen(top.min(self.limit())), index, span)
    }

    fn shows_span_on(&self, screen: &Screen, index: usize, (from, to): (f32, f32)) -> bool {
        self.live_band(screen, index)
            .is_some_and(|(at, floor, live)| at + from >= floor - 0.01 && at + to <= live + 0.01)
    }

    /// What `screen` paints of `key`'s `span` in stop `index`: none for a sliver or a cut word.
    fn painted_of_span_on(
        &self,
        screen: &Screen,
        index: usize,
        key: &UiKey,
        (from, to): (f32, f32),
    ) -> f32 {
        let Some((at, floor, _)) = self.live_band(screen, index) else {
            return 0.0;
        };
        let bottom = if self.gives_to_status() || self.foot == ListFoot::Whole {
            screen.height.min(self.room)
        } else {
            screen.height
        };
        let (start, end) = (at + from, at + to);
        let shown = (end.min(bottom) - start.max(floor)).max(0.0);
        if shown + 0.01 < PEEK_MIN.min(to - from) {
            return 0.0;
        }
        // Taller than the room, a control is painted as far as it shows ([`Self::tall_steps`]).
        if to - from > self.room + 0.01 {
            return shown;
        }
        let fades = screen.fades && screen.height > screen.live + 0.01;
        let edges = [Some(floor), (!fades).then_some(bottom)];
        let across = |(top, end): (f32, f32), edge: f32| top < edge - 0.01 && end > edge + 0.01;
        let cut = |words: (f32, f32)| {
            edges
                .into_iter()
                .flatten()
                .any(|edge| across((start, end), edge) && across(words, edge))
        };
        let words = self.words[index]
            .as_ref()
            .filter(|(watched, _)| watched == key)
            .map(|(_, words)| *words);
        let picture = match words {
            None => cut((start, end)),
            Some(Words::Own(bands)) => bands
                .iter()
                .any(|(top, under)| cut((start + top, start + under))),
            Some(Words::Row(top, under)) => {
                cut((start, end))
                    && (start + top < floor - 0.01 || (!fades && start + under > bottom + 0.01))
            }
            Some(Words::Nothing) => false,
        };
        if picture { 0.0 } else { shown }
    }

    fn live_band(&self, screen: &Screen, index: usize) -> Option<(f32, f32, f32)> {
        if !screen.drawn.contains(&index) {
            return None;
        }
        let at = self.stop_top(screen.drawn.start, index) - screen.offset;
        let floor = self.caption_floor(screen.caption, &screen.drawn, index);
        let live = if self.gives_to_status() || self.foot == ListFoot::Whole {
            screen.live.min(self.room)
        } else {
            screen.live
        };
        Some((at, floor, live))
    }

    fn caption_floor(
        &self,
        caption: Option<TopCaption>,
        drawn: &Range<usize>,
        index: usize,
    ) -> f32 {
        let Some(TopCaption { lead, kept: true }) = caption else {
            return 0.0;
        };
        if !drawn.contains(&index) {
            return 0.0;
        }
        let headed = match self.runs(drawn.start..index + 1).first() {
            Some(ScrollRun::Group { rows, .. }) => rows.contains(&index),
            Some(ScrollRun::Card(card)) => *card == index,
            None => false,
        };
        if headed { lead } else { 0.0 }
    }

    /// Whether the screen whose first stop is `top` shows stop `index` as the keyboard's landing
    /// reveals it.
    pub(super) fn shows_landing(&self, index: usize, top: usize) -> bool {
        self.shows_revealed(index, top, Reveal::Keep)
    }

    /// Least room a screen needs for `landing`'s row whole beside `part` of `key` (all where none).
    #[cfg(test)]
    pub(super) fn room_beside(
        &self,
        landing: &UiKey,
        key: &UiKey,
        part: Option<f32>,
    ) -> Option<f32> {
        let (row, control) = (self.stop_holding(landing)?, self.stop_of(key)?);
        if row == control {
            return None;
        }
        let (top, bottom) = self.controls_of(row);
        let (from, to) = self
            .control_span(control, key)
            .unwrap_or_else(|| self.controls_of(control));
        let part = part.map_or(to - from, |part| (part - 0.5).max(0.0));
        let between: f32 = self.heights[row.min(control) + 1..row.max(control)]
            .iter()
            .sum();
        Some(bottom - top + between + part)
    }

    /// Whether the screen whose first stop is `top` shows stop `index`'s controls whole.
    #[cfg(test)]
    pub(super) fn shows_controls(&self, index: usize, top: usize) -> bool {
        self.shows_span(index, top, self.controls_of(index))
    }

    /// Whether the screen whose first stop is `top` shows stop `index` as the keyboard reads it
    /// ([`Reveal::Keep`]).
    #[cfg(test)]
    pub(super) fn shows_for_keyboard(&self, index: usize, top: usize) -> bool {
        self.shows_revealed(index, top, Reveal::Keep)
    }

    fn shows_revealed(&self, index: usize, top: usize, reveal: Reveal) -> bool {
        self.shows_span(index, top, self.revealed_span(index, reveal))
    }

    fn revealed_span(&self, index: usize, reveal: Reveal) -> (f32, f32) {
        match reveal {
            Reveal::Keep => (self.controls_of(index).0, self.ink[index]),
            Reveal::Pressed | Reveal::Land | Reveal::Hold => self.controls_of(index),
            Reveal::LandWhole => (0.0, self.heights[index]),
        }
    }

    /// Whether the screen whose first stop is `top` draws some of stop `index`'s controls live.
    pub(super) fn draws_controls_live(&self, index: usize, top: usize) -> bool {
        let Some(rows) = self.control_rows.get(index) else {
            return false;
        };
        self.draws_live(
            index,
            top,
            rows.iter().map(|(_, from, height)| (*from, *height)),
        )
    }

    /// Whether the screen whose first stop is `top`, as it draws, draws some
    /// of control `key` of stop `index` live ([`Self::draws_controls_live`]
    /// for that one control, where the stop says where it sits).
    pub(super) fn draws_control_live(&self, index: usize, key: &UiKey, top: usize) -> bool {
        let span = self
            .control_span(index, key)
            .map(|(from, to)| (from, to - from));
        match span {
            Some(span) => self.draws_live(index, top, std::iter::once(span)),
            None => self.draws_controls_live(index, top),
        }
    }

    /// Return the keyboard-focusable control at stop `index` when interactive.
    pub(super) fn live_control<'a>(
        &'a self,
        index: usize,
        key: &'a UiKey,
        top: usize,
    ) -> Option<&'a UiKey> {
        if self.draws_control_live(index, key, top) {
            return Some(key);
        }
        self.spans
            .get(index)?
            .iter()
            .find(|(_, from, height)| {
                self.draws_live(index, top, std::iter::once((*from, *height)))
            })
            .map(|(span, _, _)| span)
    }

    fn draws_live(
        &self,
        index: usize,
        top: usize,
        mut spans: impl Iterator<Item = (f32, f32)>,
    ) -> bool {
        let screen = self.screen(top.min(self.limit()));
        if !screen.drawn.contains(&index) {
            return false;
        }
        let at = self.stop_top(screen.drawn.start, index) - screen.offset;
        let floor = self.caption_floor(screen.caption, &screen.drawn, index);
        let live = screen.live.min(self.room);
        // A control taller than the room is live wherever it shows, its top scrolled off.
        spans.any(|(from, height)| {
            at + from < live - 0.5
                && (at + from >= floor - 0.5 || height > self.room + 0.01)
                && at + from + height > floor + 0.5
        })
    }

    /// What the screen whose first stop is `top` draws, nudged where the render seated it: the
    /// one measure every other query reads.
    pub(super) fn screen(&self, top: usize) -> Screen {
        let screen = self.stop_screen(top);
        match self.seated {
            Some(seat)
                if self.foot == ListFoot::Peek
                    && seat.top == screen.view.start
                    && (seat.nudge.abs() > 0.01 || seat.bare) =>
            {
                self.nudged(screen, seat.nudge, seat.bare)
            }
            Some(seat) if seat.top == screen.view.start && self.seats_partway(seat) => Screen {
                offset: seat.nudge,
                ..screen
            },
            _ => screen,
        }
    }

    /// Whether `seat` starts a whole-stop list's screen partway into a stop taller than its room.
    fn seats_partway(&self, seat: Seat) -> bool {
        self.foot == ListFoot::Whole
            && !seat.bare
            && seat.top < self.len()
            && self.heights[seat.top] > self.room + 0.01
            && seat.nudge > 0.01
            && seat.nudge <= self.lead_of(seat.top) + self.heights[seat.top] - self.room + 0.01
    }

    /// The seat partway into `key`'s stop, taller than the room, that shows most of the control.
    pub(super) fn tall_stop_seat(&self, key: &UiKey) -> Option<Seat> {
        let index = self.stop_of(key)?;
        if self.foot != ListFoot::Whole
            || index > self.limit()
            || self.heights[index] <= self.room + 0.01
        {
            return None;
        }
        let (from, to) = self.control_span(index, key)?;
        let at = self.lead_of(index);
        let seat = if to - from > self.room + 0.01 {
            Seat::nudged(index, at + from)
        } else {
            Seat::nudged(index, at + to - self.room)
        };
        (self.seats_partway(seat) && at + from - seat.nudge >= -0.01).then_some(seat)
    }

    fn bare_at(&self, top: usize) -> bool {
        self.seated.is_some_and(|seat| seat.top == top && seat.bare)
    }

    fn stop_screen(&self, top: usize) -> Screen {
        let n = self.len();
        if n == 0 {
            return Screen {
                view: 0..0,
                drawn: 0..0,
                offset: 0.0,
                height: 0.0,
                live: 0.0,
                fades: false,
                whole: true,
                caption: None,
                edge_caption: None,
                bare: false,
            };
        }
        let view = self.view(top);
        let base = self
            .room
            .max(self.ink[view.start] + self.lead_of(view.start));
        let reach = self.room + self.overhang;
        if view.start == 0 && view.end >= n {
            return Screen {
                height: self.span(0..n).min(reach),
                view: view.clone(),
                drawn: view,
                offset: 0.0,
                live: base,
                fades: false,
                whole: true,
                caption: None,
                edge_caption: None,
                bare: false,
            };
        }
        if self.foot == ListFoot::Whole {
            let drop = self.seat_drop(view.start);
            return Screen {
                drawn: view.clone(),
                view,
                offset: drop,
                height: self.room,
                live: base,
                fades: false,
                whole: false,
                caption: None,
                edge_caption: None,
                bare: drop > 0.0,
            };
        }
        if top >= self.limit() {
            return self.last_screen(top);
        }
        let shows = |stop: usize| {
            if self.starts_run(stop) && self.gap > 0.01 {
                self.card_top(view.start, stop) <= reach - PEEK_MIN + 0.01
            } else {
                self.stop_top(view.start, stop) < reach - 0.01
            }
        };
        let mut end = view.end;
        while end < n && shows(end) {
            end += 1;
        }
        let drawn = view.start..end;
        let live = self.live_on(&drawn, 0.0, None, base);
        let screen = Screen {
            live,
            edge_caption: self.edge_caption(&drawn, 0.0, reach, live),
            drawn,
            view,
            offset: 0.0,
            height: reach,
            fades: true,
            whole: false,
            caption: None,
            bare: false,
        };
        let drop = self.seat_drop(screen.view.start);
        let screen = if drop > 0.0 {
            self.nudged(screen, drop, true)
        } else {
            screen
        };
        self.peek_seat(screen)
    }

    /// What the screen whose first stop is `top` lacks of the next card's least peek.
    #[cfg(test)]
    pub(super) fn peek_lack(&self, top: usize) -> Option<(f32, f32)> {
        self.peek_need(&self.screen(top.min(self.limit())))
    }

    /// What `screen` lacks of the next card's least peek and the padding over its first stop a
    /// seat may scroll into. THE PEEK IS MEASURED IN THE ROOM A BAR LEAVES: a status's bar
    /// covers the strip kept for it, so a peek in the strip alone left a screen ending on a
    /// whole card over a blank band while the bar was up (Cursor & Motion at 102x29: 18pt). The
    /// room the bar leaves is the room with or without it, so rows stay where they were when
    /// the bar comes or goes; a taller status's bar is measured as one the page kept room for.
    fn peek_need(&self, screen: &Screen) -> Option<(f32, f32)> {
        let first = screen.view.start;
        if self.gap <= 0.01 {
            return None;
        }
        let reach = self.room + self.given;
        let start = screen.drawn.start;
        // The first stop that room does not reach, as a screen drawn to it ends: what the
        // strip under the room draws besides with no bar up is no peek the room shows.
        let next = (first + 1..self.len()).find(|&stop| {
            if self.starts_run(stop) {
                self.card_top(start, stop) - screen.offset > reach - PEEK_MIN + 0.01
            } else {
                self.stop_top(start, stop) - screen.offset >= reach - 0.01
            }
        })?;
        if !self.starts_run(next) {
            return None;
        }
        let shows = reach - (self.card_top(start, next) - screen.offset);
        Some((PEEK_MIN - shows, self.peek_padding(first)))
    }

    /// How far a screen that starts at stop `first` may be nudged for the next card's peek: the
    /// stop's blank above its ink (its run's caption's, where it heads one).
    fn peek_padding(&self, first: usize) -> f32 {
        match self.groups[first] {
            Some(group) => self.captions[group]
                .map_or(self.ink_top[first], |(top, _)| top.min(self.lead_of(first))),
            None => self.ink_top[first],
        }
    }

    fn peek_seat(&self, screen: Screen) -> Screen {
        if screen.view.start == 0 || screen.offset > 0.01 || screen.bare {
            return screen;
        }
        // The last screen at this stop, unscrolled, took this nudge (or none) for the stops
        // as they stood then: a stop grown in place keeps it, rather than shifting the screen
        // to give the next card back its peek under the pointer.
        if let Some(held) = self.held_start
            && held.top == screen.view.start
        {
            return if held.peek > 0.01 && held.peek <= self.peek_padding(held.top) + 0.01 {
                self.nudged(screen, held.peek, false)
            } else {
                screen
            };
        }
        let Some((lacks, padding)) = self.peek_need(&screen) else {
            return screen;
        };
        if lacks <= 0.01 || lacks > padding + 0.01 {
            return screen;
        }
        let shown = self.shown_on(&screen.drawn, screen.offset, screen.caption);
        let peeked = self.nudged(screen.clone(), lacks, false);
        let now = self.shown_on(&peeked.drawn, peeked.offset, peeked.caption);
        if now.start == shown.start && now.end >= shown.end {
            peeked
        } else {
            screen
        }
    }

    /// Whether a screen that starts at stop `top` drops its group's caption: a whole stop beats
    /// its caption, so a row that fits the room only with the caption scrolled off starts bare.
    pub(super) fn starts_bare(&self, top: usize) -> bool {
        let Some(group) = self.groups.get(top).copied().flatten() else {
            return false;
        };
        if let Some(held) = self.held_start
            && held.top == top
        {
            return held.bare;
        }
        // A whole-stop list's rows start bare as a peeking list's do, each by itself: the open
        // Community section's rows lost COMMUNITY over every screen of them for one three-line
        // row that fitted only without it (178x516 at 2x), its card's head made a row.
        // A STOP TALLER THAN THE ROOM STARTS BARE TOO, its run's caption read on the landing
        // before it ([`Self::captioned_seat`]): seated under the caption, the keyboard's control
        // (Sound's 130pt Trail style at 2x in 164x98) showed 16pt under SOUND EFFECTS, not a
        // line of it.
        let lead = self.leads[group];
        let room = match self.foot {
            ListFoot::Peek => self.room + self.given,
            ListFoot::Whole => self.room,
        };
        lead > 0.01
            && !self.shows_at(lead, top, room)
            && (self.shows_at(0.0, top, room) || self.taller_than_room(top))
    }

    /// How the screen that starts at stop `top` sits ([`HeldStart`]).
    fn held_seat(&self, top: usize) -> HeldStart {
        let bare = self.starts_bare(top);
        let screen = self.stop_screen(top);
        HeldStart {
            top,
            bare,
            peek: if bare || screen.bare || screen.view.start != top {
                0.0
            } else {
                screen.offset
            },
        }
    }

    fn seat_drop(&self, top: usize) -> f32 {
        if self.starts_bare(top) {
            self.lead_of(top)
        } else {
            0.0
        }
    }

    fn shown_on(
        &self,
        drawn: &Range<usize>,
        offset: f32,
        caption: Option<TopCaption>,
    ) -> Range<usize> {
        let mut shown: Option<Range<usize>> = None;
        let mut at = 0.0;
        for stop in drawn.clone() {
            at = if stop == drawn.start {
                self.lead_of(stop) - offset
            } else {
                self.next_top(at, stop)
            };
            let whole = at + self.ink_top[stop] >= self.caption_floor(caption, drawn, stop) - 0.01
                && self.shows_at(at, stop, self.room);
            match (whole, shown.as_mut()) {
                (true, Some(run)) => run.end = stop + 1,
                (true, None) => shown = Some(stop..stop + 1),
                (false, Some(_)) => break,
                (false, None) => {}
            }
        }
        shown.unwrap_or(drawn.start..drawn.start)
    }

    fn control_lines(&self, index: usize) -> Vec<(f32, f32)> {
        let mut parts: Vec<(f32, f32)> = if self.spans[index].is_empty() {
            self.control_rows[index]
                .iter()
                .map(|(_, from, height)| (*from, from + height))
                .collect()
        } else {
            self.spans[index]
                .iter()
                .map(|(_, from, height)| (*from, from + height))
                .collect()
        };
        parts.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut lines: Vec<(f32, f32)> = Vec::new();
        for (from, to) in parts {
            match lines.last_mut() {
                Some(line) if from < line.1 - 0.5 => line.1 = line.1.max(to),
                _ => lines.push((from, to)),
            }
        }
        lines
    }

    fn live_on(
        &self,
        drawn: &Range<usize>,
        offset: f32,
        caption: Option<TopCaption>,
        base: f32,
    ) -> f32 {
        let shown = self.shown_on(drawn, offset, caption);
        if shown.is_empty() {
            // A screen that shows no stop whole (a run's caption over its first stop, cut) is
            // live down to the first control its room's edge cuts, as one that shows some is.
            let mut at = 0.0;
            for stop in drawn.clone() {
                at = if stop == drawn.start {
                    self.lead_of(stop) - offset
                } else {
                    self.next_top(at, stop)
                };
                if let Some((from, _)) = self
                    .control_lines(stop)
                    .into_iter()
                    .filter(|(from, to)| to - from <= self.room + 0.01)
                    .find(|(from, to)| at + to > self.room + 0.01 && at + from >= -0.01)
                {
                    return base.min(at + from);
                }
            }
            return base;
        }
        let reach = self.room + self.overhang;
        let (mut at, mut words, mut cut) = (0.0, 0.0_f32, f32::INFINITY);
        let mut next_card = 0.0_f32;
        for stop in drawn.clone() {
            at = if stop == drawn.start {
                self.lead_of(stop) - offset
            } else {
                self.next_top(at, stop)
            };
            if stop + 1 == shown.end {
                words = at + self.ink[stop];
            }
            if stop < shown.end {
                continue;
            }
            if stop == shown.end {
                next_card = at
                    - if self.starts_run(stop) {
                        self.lead_of(stop)
                    } else {
                        0.0
                    };
            }
            // A control taller than the room is live wherever it shows ([`Self::tall_steps`]).
            if let Some((from, _)) = self
                .control_lines(stop)
                .into_iter()
                .filter(|(from, to)| to - from <= self.room + 0.01)
                .find(|(from, to)| at + to > self.room + 0.01 || at + from < 0.0)
            {
                cut = at + from;
                break;
            }
        }
        base.max(words.min(reach))
            .max(next_card.min(reach))
            .min(cut)
    }

    /// The stops the screen whose first stop is `top` shows.
    pub(super) fn shown(&self, top: usize) -> Range<usize> {
        let n = self.len();
        if n == 0 {
            return 0..0;
        }
        let screen = self.screen(top.min(self.limit()));
        if screen.whole {
            return 0..n;
        }
        let shown = self.shown_on(&screen.drawn, screen.offset, screen.caption);
        if shown.is_empty() {
            screen.view.start..screen.view.start + 1
        } else {
            shown
        }
    }

    /// Page Down: the first stop after what the screen from `top` shows.
    pub(super) fn page_after(&self, top: usize) -> usize {
        self.shown(top).end.min(self.limit())
    }

    /// Page Up from the screen whose first stop is `top`: the new first stop and, where one fits,
    /// the nudge that ends its screen with the stop just before the ones `top`'s screen showed.
    pub(super) fn page_up(&self, top: usize) -> (usize, Option<f32>) {
        let first = self.shown(top).start;
        if first == 0 {
            return (0, None);
        }
        let seated = self.page_before(first);
        let seated_screen = self.stop_screen(seated.min(self.limit()));
        if seated == 0
            || self.foot == ListFoot::Whole
            || self
                .shown_on(
                    &seated_screen.drawn,
                    seated_screen.offset,
                    seated_screen.caption,
                )
                .end
                <= first
        {
            return (seated, None);
        }
        let end_of = |start: usize| self.stop_top(start, first - 1) + self.ink[first - 1];
        let Some(start) = (0..first)
            .rev()
            .find(|start| end_of(*start) >= self.room - 0.01)
        else {
            return (0, None);
        };
        let nudge = end_of(start) - self.room - self.stop_screen(start).offset;
        if nudge <= 0.01 {
            return (start, None);
        }
        let anchored = self.nudged(self.stop_screen(start), nudge, false);
        let shown = self.shown_on(&anchored.drawn, anchored.offset, anchored.caption);
        if self.nudge_fits(start, nudge) && !shown.is_empty() && shown.end == first {
            (start, Some(nudge))
        } else {
            (seated, None)
        }
    }

    /// What stop `index` draws, from its first words or control to its last, under its top.
    fn content_of(&self, index: usize) -> (f32, f32) {
        let (from, to) = self.controls_of(index);
        (self.ink_top[index].min(from), self.ink[index].max(to))
    }

    /// Whether what stop `index` draws is taller than the room: no screen holds it whole.
    pub(super) fn taller_than_room(&self, index: usize) -> bool {
        let (from, to) = self.content_of(index);
        to - from > self.room + 0.01
    }

    /// WHERE A SCREEN THAT STARTS WITH STOP `top` MAY STAND, in reading order: the stop's own
    /// seat (`None`); before it, for a stop that starts its run bare (whole only with its
    /// run's caption scrolled off, [`Self::starts_bare`]), the seat that draws the caption over
    /// it, the stop cut under it, so every heading is read whole on a screen the wheel and the
    /// page keys reach (Window & Tabs' Presence rim at 1x in 178x152, which Page Down skipped);
    /// after it, for a stop taller than the room, a seat each room further into it until its
    /// end shows ([`Self::tall_steps`]).
    pub(super) fn landings(&self, top: usize) -> Vec<Option<Seat>> {
        if self.len() == 0 {
            return vec![None];
        }
        let top = top.min(self.limit());
        let mut landings: Vec<Option<Seat>> =
            self.captioned_seat(top).into_iter().map(Some).collect();
        landings.push(None);
        landings.extend(self.tall_steps(top).into_iter().map(Some));
        landings
    }

    /// The seat over stop `top` that draws its run's caption whole above it, as high on the
    /// card as that leaves it ([`Self::landings`]).
    fn captioned_seat(&self, top: usize) -> Option<Seat> {
        let group = self.groups.get(top).copied().flatten()?;
        if self.foot != ListFoot::Peek || !self.starts_run(top) || self.seat_drop(top) <= 0.01 {
            return None;
        }
        let (caption_top, caption_bottom) = self.captions[group]?;
        let natural = self.stop_screen(top);
        if natural.whole || natural.view.start != top || caption_bottom - caption_top > self.room {
            return None;
        }
        let offset = (caption_bottom - self.room).max(0.0);
        let seat = Seat::nudged(top, offset - natural.offset);
        (offset <= caption_top + 0.01 && self.seat_fits(seat)).then_some(seat)
    }

    /// The seats a room apart down stop `top`, taller than the room, after its own until its
    /// end shows: each starts where the screen before cut its first words or control, or a
    /// room on where that is taller than the room ([`Self::landings`]).
    fn tall_steps(&self, top: usize) -> Vec<Seat> {
        if top >= self.len() || !self.taller_than_room(top) {
            return Vec::new();
        }
        let natural = self.stop_screen(top);
        if natural.whole || natural.view.start != top {
            return Vec::new();
        }
        let start = natural.drawn.start;
        let at = self.stop_top(start, top);
        let end = at + self.content_of(top).1 - self.room;
        let base = natural.offset;
        let floor_at = |offset: f32| {
            let screen = self.nudged(natural.clone(), offset - base, false);
            self.caption_floor(screen.caption, &screen.drawn, top)
        };
        let mut steps = Vec::new();
        let mut offset = base;
        // NO STEP SHOWS ONLY A SLIVER MORE: where what is left under the room's edge is less than
        // a peek (a button's lower edge), the stop is read; a step a point further looked the
        // same and Page Down seemed to stop (Text & Fonts' 97pt Font family in a 96pt list at 2x).
        while offset < end - PEEK_MIN && steps.len() < 256 {
            let floor = floor_at(offset);
            let (shown_top, shown_bottom) = (offset - at + floor, offset - at + self.room);
            // The first words or control this screen cut that the room holds whole: one taller
            // than the room (the stop's tall control itself) is stepped through a room at a time.
            let cut = self.drawn_bands[top]
                .iter()
                .filter(|band| {
                    band.whole
                        && band.bottom > shown_bottom + 0.5
                        && band.bottom - band.top <= self.room - floor + 0.01
                })
                .map(|band| band.top)
                .fold(f32::INFINITY, f32::min);
            // The next screen's top, where the caption over it leaves room, at the words or
            // control this one cut, else where this one's room ended.
            let want = if cut.is_finite() && cut > shown_top + 0.5 {
                cut
            } else {
                shown_bottom
            };
            let guess = offset + (want - shown_top);
            let next = want + at - floor_at(guess.min(end));
            offset = next.min(end).max(offset + 1.0);
            steps.push(Seat::nudged(top, offset - base));
        }
        steps
    }

    /// Where `seat` stands among stop `top`'s landings ([`Self::landings`]): the last one whose
    /// screen starts no further down than its screen.
    fn landing_at(&self, top: usize, seat: Option<Seat>, landings: &[Option<Seat>]) -> usize {
        let offset = |seat: Option<Seat>| {
            let screen = self.stop_screen(top);
            screen.offset
                + seat
                    .filter(|seat| seat.top == top)
                    .map_or(0.0, |seat| seat.nudge)
        };
        let now = offset(seat);
        landings
            .iter()
            .rposition(|landing| offset(*landing) <= now + 0.5)
            .unwrap_or_else(|| landings.iter().position(Option::is_none).unwrap_or(0))
    }

    /// The landing a wheel's line from stop `top` (seated at `seat`) brings: the next one of
    /// the stop's landings, else the first of the next stop's (the last of the stop before
    /// going up) ([`Self::landings`]). A stop no scroll by stops seats ([`Self::passes_over`])
    /// is passed over.
    pub(super) fn wheel_from(
        &self,
        top: usize,
        seat: Option<Seat>,
        forward: bool,
    ) -> (usize, Option<Seat>) {
        let limit = self.limit();
        let top = top.min(limit);
        let landings = self.landings(top);
        let at = self.landing_at(top, seat, &landings);
        if forward && at + 1 < landings.len() {
            return (top, landings[at + 1]);
        }
        if !forward && at > 0 {
            return (top, landings[at - 1]);
        }
        let mut next = top;
        loop {
            next = if forward {
                if next >= limit {
                    return (top, landings[at]);
                }
                next + 1
            } else {
                if next == 0 {
                    return (top, landings[at]);
                }
                next - 1
            };
            if !self.passes_over(next) {
                break;
            }
        }
        let landings = self.landings(next);
        let landing = if forward {
            landings.first()
        } else {
            landings.last()
        };
        (next, landing.copied().flatten())
    }

    /// Page Down from the screen whose first stop is `top`, seated at `seat`: within a stop
    /// taller than the room, a room further down it; from a screen that drew a stop's caption
    /// over it cut, that stop whole; else the first stop after what the screen showed whole,
    /// over its caption where the screen did not draw that whole ([`Self::landings`]).
    pub(super) fn page_down(&self, top: usize, seat: Option<Seat>) -> (usize, Option<Seat>) {
        let limit = self.limit();
        let top = top.min(limit);
        let landings = self.landings(top);
        let at = self.landing_at(top, seat, &landings);
        if at + 1 < landings.len() {
            return (top, landings[at + 1]);
        }
        let next = self.page_after(top);
        if next <= top {
            return (top, landings[at]);
        }
        let caption = self.captioned_seat(next).filter(|_| {
            let screen = self.screen(top);
            !self
                .caption_band(screen.drawn.start, next)
                .is_some_and(|(from, to)| {
                    from - screen.offset >= -0.01 && to - screen.offset <= self.room + 0.01
                })
        });
        (next, caption)
    }

    /// Page Up from the screen whose first stop is `top`, seated at `seat`: within a stop taller
    /// than the room, a room further up it; into one, its end; else as [`Self::page_up`].
    pub(super) fn page_up_from(&self, top: usize, seat: Option<Seat>) -> (usize, Option<Seat>) {
        let top = top.min(self.limit());
        let landings = self.landings(top);
        let at = self.landing_at(top, seat, &landings);
        let natural = landings.iter().position(Option::is_none).unwrap_or(0);
        if at > natural {
            return (top, landings[at - 1]);
        }
        let first = self.shown(top).start;
        if at == natural
            && first == top
            && top > 0
            && self.taller_than_room(top - 1)
            && !self.passes_over(top - 1)
        {
            return (top - 1, self.landings(top - 1).last().copied().flatten());
        }
        let (top, nudge) = self.page_up(top);
        (top, nudge.map(|nudge| Seat::nudged(top, nudge)))
    }

    fn top_caption(&self, drawn: Range<usize>, offset: f32) -> Option<TopCaption> {
        let first = drawn.start;
        match self.runs(drawn).first() {
            Some(ScrollRun::Group { group, rows }) => {
                let padding = self.captions[*group].map_or(0.0, |(top, _)| top);
                let at = self.lead_of(first) - offset;
                let row_whole = offset > self.ink_top[first] + 0.01
                    && at + self.ink_top[first] >= -0.01
                    && self.shows_at(at, first, self.room);
                let last = rows.end - 1;
                let ink_end = self.stop_top(first, last) - self.lead_of(first) + self.ink[last];
                let drawn_end = self.run_rows(*group, rows).min(ink_end);
                // Over a stop taller than the room the caption scrolls away with the card: kept,
                // it left the stop a sliver of the room (Sound's SOUND EFFECTS at 2x in 164x98,
                // 16pt of Typing sound's 130pt button a screen).
                let kept = offset > padding + 0.01
                    && offset < drawn_end - PEEK_MIN + 0.01
                    && !row_whole
                    && !self.taller_than_room(first);
                let cuts = self.captions[*group]
                    .is_some_and(|(top, bottom)| offset > top + 0.01 && offset < bottom - 0.01);
                (kept || cuts).then(|| TopCaption {
                    lead: self.leads[*group],
                    kept,
                })
            }
            Some(ScrollRun::Card(card)) => self.card_captions[*card]
                .filter(|caption| offset > caption.top + 0.01 && offset < self.heights[*card])
                .map(|caption| TopCaption {
                    lead: caption.lead,
                    kept: offset < caption.rows - 0.01,
                }),
            _ => None,
        }
    }

    fn edge_caption(
        &self,
        drawn: &Range<usize>,
        offset: f32,
        height: f32,
        live: f32,
    ) -> Option<(f32, f32)> {
        if live < height - 0.5 {
            return None;
        }
        let last = drawn.end.checked_sub(1)?;
        let (top, bottom) = self.caption_band(drawn.start, last)?;
        let edge = height + offset;
        let card = self.card_top(drawn.start, last);
        (edge > top + 0.5 && edge < bottom - 0.5).then_some((edge - card, bottom - card))
    }

    fn caption_band(&self, from: usize, index: usize) -> Option<(f32, f32)> {
        if index <= from || !self.starts_run(index) || self.gap <= 0.01 {
            return None;
        }
        let top = self.card_top(from, index);
        let (caption, under) = match self.groups[index] {
            Some(group) => self.captions[group]?,
            None => self.card_captions[index].map(|caption| (caption.top, caption.bottom))?,
        };
        Some((top + caption, top + under))
    }

    /// The seat a press on span `from..to` of stop `index` takes from `top`: the screen nudged
    /// so the cut control shows whole, bare where only that shows it. None on a whole-stop
    /// list, where nothing is cut, or where no nudge fits.
    pub(super) fn press_seat_span(
        &self,
        index: usize,
        top: usize,
        (from, to): (f32, f32),
    ) -> Option<Seat> {
        if self.foot == ListFoot::Whole {
            return None;
        }
        let top = top.min(self.limit());
        let seat = self.stop_screen(top).offset;
        let screen = self.screen(top);
        if screen.whole || !screen.drawn.contains(&index) {
            return None;
        }
        let start = screen.drawn.start;
        let at = self.stop_top(start, index) - screen.offset;
        let (control_top, control_bottom) = (at + from, at + to);
        let live = self.room;
        let floor_now = self.caption_floor(screen.caption, &screen.drawn, index);
        let nudge = if control_bottom > live + 0.01 && control_top < screen.height - 0.01 {
            control_bottom - live
        } else if control_top < floor_now - 0.01 && control_bottom > floor_now + 0.01 {
            control_top - floor_now
        } else {
            return None;
        };
        let offset = screen.offset + nudge;
        let end = self.card_bottom(start, self.len() - 1) - screen.offset;
        let shows = |bare: bool, (from, to): (f32, f32)| {
            let then = self.nudged(self.stop_screen(top), offset - seat, bare);
            let floor_then = self.caption_floor(then.caption, &then.drawn, index);
            offset >= -0.01
                && nudge <= end - live + 0.01
                && at + from - nudge >= floor_then - 0.01
                && at + to - nudge <= live + 0.01
        };
        let bare = self.bare_at(top);
        let stop = (self.ink_top[index], self.ink[index]);
        let choice = if !bare && !shows(false, stop) && shows(true, stop) {
            Some(true)
        } else {
            [bare, true]
                .into_iter()
                .find(|bare| shows(*bare, (from, to)))
        };
        choice.map(|bare| Seat {
            bare,
            ..Seat::nudged(top, offset - seat)
        })
    }

    /// THE SEAT THAT HOLDS A PRESSED CONTROL WHERE IT WAS PRESSED: the screen, from the stops
    /// at and above `top` (then below), that draws stop `index`'s control span `(from, to)`
    /// whole with its top `at` down the list. `None` where the list has no such seat (its end
    /// reached, or the control no longer fitting there whole).
    pub(super) fn seat_holding(
        &self,
        index: usize,
        top: usize,
        (from, to): (f32, f32),
        at: f32,
    ) -> Option<(usize, Option<Seat>)> {
        if self.foot == ListFoot::Whole
            || index >= self.len()
            || at + from.min(0.0) < -0.01
            || at + (to - from) > self.room + 0.01
        {
            return None;
        }
        let limit = self.limit();
        let near = top.min(limit).min(index);
        let candidates = (0..=near).rev().chain(near + 1..=limit.min(index));
        for start in candidates {
            let natural = self.stop_screen(start);
            if natural.whole || natural.drawn.start > index {
                continue;
            }
            let now = self.stop_top(natural.drawn.start, index) - natural.offset + from;
            let nudge = now - at;
            let holds = |screen: &Screen| {
                self.live_band(screen, index)
                    .is_some_and(|(then, floor, live)| {
                        (then + from - at).abs() < 0.5
                            && then + from >= floor - 0.01
                            && then + to <= live + 0.01
                    })
            };
            if nudge.abs() < 0.01 {
                if holds(&natural) {
                    return Some((start, None));
                }
                continue;
            }
            let seat = Seat::nudged(start, nudge);
            if self.hold_fits(seat) && holds(&self.nudged(natural, nudge, false)) {
                return Some((start, Some(seat)));
            }
        }
        None
    }

    /// Whether a press's hold may take `seat` ([`Self::seat_holding`]): where any screen may
    /// ([`Self::seat_fits`]), and on the list's last screen with room left unfilled under the
    /// list's end. A PRESS ON THE LAST SCREEN KEEPS IT AS IT WAS: that screen may end short of the
    /// room, where nothing follows ([`Self::last_screen`]), and a status's bar that took room from
    /// the list, or gave the room kept for it back, seated it anew, flush with the room's end:
    /// the switch pressed went down under the pointer by what the screen had left unfilled
    /// (Security at 206x420: Desktop notifications from aterm, 51pt).
    fn hold_fits(&self, seat: Seat) -> bool {
        if self.seat_fits(seat) {
            return true;
        }
        if self.foot == ListFoot::Whole || seat.bare || self.len() == 0 {
            return false;
        }
        let screen = self.stop_screen(seat.top.min(self.limit()));
        !screen.whole && screen.drawn.end >= self.len() && screen.offset + seat.nudge >= -0.01
    }

    /// Nudge from its seat taken by a press on stop `index`'s controls from `top`.
    #[cfg(test)]
    pub(super) fn press_nudge(&self, index: usize, top: usize) -> Option<f32> {
        (index < self.len())
            .then(|| self.press_seat_span(index, top, self.controls_of(index)))
            .flatten()
            .map(|seat| seat.nudge)
    }

    fn nudge_fits(&self, top: usize, nudge: f32) -> bool {
        let screen = self.stop_screen(top.min(self.limit()));
        if screen.whole || self.len() == 0 {
            return false;
        }
        let end = self.card_bottom(screen.drawn.start, self.len() - 1) - screen.offset;
        screen.offset + nudge >= -0.01 && nudge <= end - screen.live.max(self.room) + 0.01
    }

    fn seat_fits(&self, seat: Seat) -> bool {
        if self.foot == ListFoot::Whole {
            return self.seats_partway(seat);
        }
        if !seat.bare {
            return self.nudge_fits(seat.top, seat.nudge);
        }
        let screen = self.stop_screen(seat.top.min(self.limit()));
        if screen.whole || self.len() == 0 || screen.view.start != seat.top {
            return false;
        }
        let end = self.card_bottom(screen.drawn.start, self.len() - 1) - screen.offset;
        screen.offset + seat.nudge >= -0.01 && seat.nudge <= end - self.room + 0.01
    }

    fn nudged(&self, screen: Screen, nudge: f32, bare: bool) -> Screen {
        // Nudged back up off a bare seat, a screen draws its run's caption again
        // ([`Self::captioned_seat`]): it is not bare, and its room is the list's, never the
        // strip kept under it for a bar.
        let bare = (bare || screen.bare) && nudge >= -0.01;
        let n = self.len();
        let start = screen.drawn.start;
        let offset = (screen.offset + nudge).max(0.0);
        let reach = self.room + self.overhang;
        let shows = |stop: usize| {
            if self.starts_run(stop) && self.gap > 0.01 {
                self.card_top(start, stop) <= reach + offset - PEEK_MIN + 0.01
            } else {
                self.stop_top(start, stop) < reach + offset - 0.01
            }
        };
        let mut end = if nudge < -0.01 {
            start + 1
        } else {
            screen.drawn.end
        };
        while end < n && shows(end) {
            end += 1;
        }
        let drawn = start..end;
        let caption = self
            .top_caption(drawn.clone(), offset)
            .map(|caption| TopCaption {
                kept: !bare
                    && caption.kept
                    && (nudge >= 0.0 || screen.caption.is_some_and(|caption| caption.kept)),
                ..caption
            });
        let base = if bare {
            self.room.max(screen.live - nudge)
        } else {
            self.room
        };
        let live = self.live_on(&drawn, offset, caption, base);
        Screen {
            live,
            edge_caption: self.edge_caption(&drawn, offset, reach, live),
            drawn,
            offset,
            height: reach,
            fades: screen.fades || offset < screen.offset - 0.01,
            caption,
            bare,
            ..screen
        }
    }

    /// The stop holding control `key` (a segment: its control's).
    pub(super) fn stop_of(&self, key: &UiKey) -> Option<usize> {
        let key = self.resolve(key);
        self.keys.iter().position(|keys| keys.contains(key))
    }

    /// Say what the last stop added lists.
    pub(super) fn set_ids(&mut self, ids: Vec<UiKey>) {
        self.kept_limit.forget();
        if let Some(last) = self.ids.len().checked_sub(1) {
            self.set_ids_of(last, ids);
        }
    }

    /// [`Self::set_ids`] for stop `index`.
    pub(super) fn set_ids_of(&mut self, index: usize, ids: Vec<UiKey>) {
        self.kept_limit.forget();
        if let Some(stop) = std::sync::Arc::make_mut(&mut self.ids).get_mut(index) {
            *stop = ids;
        }
    }

    /// Say what else the last stop added draws ([`Self::within`]).
    pub(super) fn set_within(&mut self, within: Vec<UiKey>) {
        self.kept_limit.forget();
        if let Some(last) = std::sync::Arc::make_mut(&mut self.within).last_mut() {
            *last = within;
        }
    }

    /// What stop `index` is known by.
    pub(super) fn identities(&self, index: usize) -> impl Iterator<Item = &UiKey> {
        self.ids
            .get(index)
            .into_iter()
            .flatten()
            .chain(self.keys.get(index).into_iter().flatten())
            .chain(self.within.get(index).into_iter().flatten())
    }

    /// Whether stop `index` is `was`'s there, not one moved in by stops added or dropped above.
    fn holds_stop_of(&self, was: &Self, index: usize) -> bool {
        let mut known = was.identities(index).peekable();
        known.peek().is_none() || known.any(|id| self.identities(index).any(|now| now == id))
    }

    /// The stop that lists node `id` or holds control `id`, else the one that draws it.
    pub(super) fn stop_of_identity(&self, id: &UiKey) -> Option<usize> {
        self.ids
            .iter()
            .position(|ids| ids.contains(id))
            .or_else(|| self.stop_of(id))
            .or_else(|| self.within.iter().position(|within| within.contains(id)))
    }

    /// The stop that holds what a press found at `key`.
    pub(super) fn stop_holding(&self, key: &UiKey) -> Option<usize> {
        self.stop_of(key)
            .or_else(|| self.stop_of_preview(key))
            .or_else(|| self.stop_of_identity(key))
    }

    /// Say which live previews the last stop added draws.
    pub(super) fn note_previews<'a>(&mut self, nodes: impl IntoIterator<Item = &'a UiNode>) {
        fn collect(node: &UiNode, out: &mut Vec<UiKey>) {
            if matches!(node.content, UiContent::SettingsPreview(_)) {
                out.push(node.key.clone());
            }
            for child in &node.children {
                collect(child, out);
            }
        }
        self.kept_limit.forget();
        let mut keys = Vec::new();
        for node in nodes {
            collect(node, &mut keys);
        }
        if let Some(last) = std::sync::Arc::make_mut(&mut self.previews).last_mut() {
            *last = keys;
        }
    }

    /// The stop that draws the live preview keyed `key`.
    pub(super) fn stop_of_preview(&self, key: &UiKey) -> Option<usize> {
        self.previews.iter().position(|keys| keys.contains(key))
    }

    /// Where control `key` is among the page's controls, top to bottom.
    pub(super) fn control_index(&self, key: &UiKey) -> Option<usize> {
        let key = self.resolve(key);
        self.controls().position(|(_, control)| control == key)
    }

    /// The page's controls, top to bottom, with their stops.
    pub(super) fn controls(&self) -> impl Iterator<Item = (usize, &UiKey)> {
        self.keys
            .iter()
            .enumerate()
            .flat_map(|(stop, keys)| keys.iter().map(move |key| (stop, key)))
    }

    /// The list's scrolls, nearest `near` first.
    fn scrolls_from(&self, near: usize) -> Vec<usize> {
        let mut tops: Vec<usize> = (0..=self.limit()).collect();
        tops.sort_by_key(|top| (top.abs_diff(near), *top));
        tops
    }

    /// The screen nearest `near`, as the stops seat it, that shows the person no less of the
    /// keyboard's control `key` than `want` and stop `index`'s `span` whole with it.
    fn keeping_with(
        &self,
        key: &UiKey,
        want: &Left,
        (index, span): (usize, (f32, f32)),
        near: usize,
    ) -> Option<usize> {
        self.scrolls_from(near).into_iter().find(|&top| {
            want.kept_by(&Left::on(self, key, top)) && self.shows_span(index, top, span)
        })
    }

    /// The screen nearest `near`, as the stops seat it, that shows the person no less of the
    /// keyboard's control `key` than `want`; where none does, the one that shows the most of it.
    fn keeping_top(&self, key: &UiKey, want: &Left, near: usize) -> usize {
        let mut best: Option<(usize, f32)> = None;
        for top in self.scrolls_from(near) {
            let now = Left::on(self, key, top);
            if want.kept_by(&now) {
                return top;
            }
            if best.is_none_or(|(_, part)| now.part > part + 0.5) {
                best = Some((top, now.part));
            }
        }
        best.map_or(near.min(self.limit()), |(top, _)| top)
    }
}

/// Where a press found its control on the screen drawn ([`SettingsViewState::reveal_pressed`]):
/// the list it was on, the control, where it ended down from the list's top, the status shown
/// as it was pressed, and the status the press brought, once it came.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PressedAt {
    context: ScrollContext,
    key: UiKey,
    bottom: f32,
    before: Option<String>,
    status: Option<String>,
    /// Whether `status` came while a write was in flight ("Applying…"): the press's own write,
    /// whose outcome the press takes for its status when it lands.
    awaits: bool,
    /// Where the control began, down from the list's top: a status's bar over the list's top
    /// ends above it ([`SettingsViewState::press_band`]).
    top: f32,
    /// Where the bar of the status the press brought stands ([`BarSide`]), whether it stands
    /// there bare of its Undo, and the status it was weighed for, once a render weighed it
    /// ([`SettingsViewState::press_side`]).
    side: Option<(BarSide, bool, Option<String>)>,
    /// Whether the bar may no longer stand over the top of the list: the person scrolled the
    /// list, or the keyboard went to a control there ([`SettingsViewState::press_band`]).
    band_gone: bool,
    /// Whether the person scrolled the list since: the place the press found is then no
    /// screen's to hold ([`SettingsViewState::seat_scroll`]).
    scrolled: bool,
    /// How far down the window the list started on that screen
    /// ([`SettingsViewState::laid_list_top`]).
    list_top: Option<f32>,
    /// Where the control stood among its segmented control's segments, where it is one
    /// ([`ScrollPlan::segment_place`], [`Self::key_in`]).
    segment: Option<(UiKey, usize)>,
}

impl PressedAt {
    /// The key the control the press found is drawn under in `plan`: a segment the press chose
    /// takes its segmented control's key, so it is found by its place. Followed by the key it
    /// had, the segment the person pressed was lost to the list the press brought, which then
    /// moved it and stood a status's bar over it (Appearance's Window appearance, Dark, at
    /// 192x348 with 2x text: from 264pt to 139pt, under the bar).
    ///
    /// A setting's segment the list no longer draws is the one chosen, under its control's key
    /// ([`chosen_segment_key`]): in a segmented control split across stops (a segment to a line
    /// in a narrow window), the part without the chosen segment was known by a segment the
    /// choice renamed, and the press, lost, let the list move the segment pressed (Cursor &
    /// Motion at 164x292: Motion's Reduce motion, 49pt down).
    fn key_in(&self, plan: &ScrollPlan) -> UiKey {
        self.segment
            .as_ref()
            .and_then(|(control, at)| plan.segment_at(control, *at))
            .cloned()
            .or_else(|| {
                plan.stop_holding(&self.key)
                    .is_none()
                    .then(|| chosen_segment_key(&self.key))
                    .flatten()
                    .filter(|control| plan.stop_holding(control).is_some())
            })
            .unwrap_or_else(|| self.key.clone())
    }

    /// Whether a reveal of `key` follows this press: of the control pressed, or of the
    /// segmented control the segment pressed chose for ([`chosen_segment_key`]).
    fn followed_by(&self, key: &UiKey) -> bool {
        *key == self.key
            || self
                .segment
                .as_ref()
                .is_some_and(|(control, _)| key == control)
            || chosen_segment_key(&self.key).as_ref() == Some(key)
    }
}

/// The key a setting's segment `key` ("settings/segment/<setting>/<n>") is drawn under once it is
/// chosen: its segmented control's ([`super::segment_focus_key`]).
fn chosen_segment_key(key: &UiKey) -> Option<UiKey> {
    let (setting, _) = key
        .as_str()
        .strip_prefix("settings/segment/")?
        .rsplit_once('/')?;
    Some(UiKey::new(format!("settings/control/{setting}")))
}

/// WHERE THE BAR OF A STATUS A PRESS BROUGHT STANDS, so it never moves or covers the control
/// pressed ([`SettingsViewState::press_side`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BarSide {
    /// Under the list, where the list keeps the control where it was.
    Under,
    /// Over the top of the list, above the control.
    Over,
    /// In the compact toolbar, where neither side of the list holds it.
    Dock,
}

/// What a page's reveal ([`SettingsViewState::page_reveal`]) is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Reveal {
    #[default]
    Keep,
    Pressed,
    /// Reveals the landed stop's controls whole ([`SettingsViewState::settle_page_scroll`],
    /// [`SettingsViewState::seat_scroll`]).
    Land,
    LandWhole,
    Hold,
}

/// What a screen showed the person of the keyboard's control ([`ScrollRecord::left`]).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Left {
    key: UiKey,
    whole: bool,
    part: f32,
    /// Whether the screen painted all of the control's own span, though not all its stop reveals.
    own: bool,
}

impl Left {
    /// `key` whole, as the keyboard's control a landing gives way to is owed.
    fn whole(key: &UiKey) -> Self {
        Self {
            key: key.clone(),
            whole: true,
            part: 0.0,
            own: true,
        }
    }

    /// What the screen whose first stop is `top` shows of `key` on `plan`.
    fn on(plan: &ScrollPlan, key: &UiKey, top: usize) -> Self {
        let (whole, part, own) = plan.stop_of(key).map_or((true, 0.0, true), |stop| {
            let span = plan.span_of(stop, Some(key), Reveal::Pressed);
            let control = plan
                .control_span(stop, key)
                .unwrap_or_else(|| plan.controls_of(stop));
            // One screen, as it draws, for both.
            let screen = plan.screen(top.min(plan.limit()));
            let part = plan.painted_of_span_on(&screen, stop, key, control);
            (
                plan.shows_span_on(&screen, stop, span),
                part,
                part + 0.5 >= control.1 - control.0,
            )
        });
        Self {
            key: key.clone(),
            whole,
            part,
            own,
        }
    }

    /// `key` pinned around the list `tall` high, `shown` of it left by a status's bar.
    fn pinned(key: &UiKey, (tall, shown): (f32, f32)) -> Self {
        Self {
            key: key.clone(),
            whole: shown + 0.5 >= tall,
            part: shown,
            own: shown + 0.5 >= tall,
        }
    }

    /// Whether `now` shows the person no less of the control than this.
    pub(super) fn kept_by(&self, now: &Self) -> bool {
        // A control painted all of it, laid out taller since, is owed all of it again (R2).
        now.whole || (!self.whole && now.part + 0.5 >= self.part && (now.own || !self.own))
    }

    /// Whether `now` shows the person no less of the keyboard's control than `was`.
    pub(super) fn kept(was: Option<&Self>, now: Option<&Self>) -> bool {
        match (was, now) {
            (Some(was), Some(now)) => was.kept_by(now),
            (None, Some(now)) => now.whole,
            (_, None) => true,
        }
    }

    /// How much of the control this shows.
    pub(super) const fn part(&self) -> f32 {
        self.part
    }

    /// Whether this shows any of the control.
    pub(super) fn shows(&self) -> bool {
        self.whole || self.part > 0.0
    }

    /// Whether this painted all of the control, laid out `tall` since: it is owed all of it.
    fn grown(&self, tall: f32) -> bool {
        self.own && tall > self.part + 0.5
    }

    /// The least a screen shows of a control `span` tall to show no less ([`Self::kept_by`]).
    pub(super) fn owed_least(&self, span: f32) -> f32 {
        if self.whole || self.grown(span) {
            span
        } else {
            span.min((self.part - 0.5).max(0.0))
        }
    }
}

/// What a screen paints of the keyboard's listed control: how tall it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Peek {
    tall: f32,
}

impl Peek {
    /// Whether `room` paints more than `beyond` of the control: no sliver, more as far as it shows.
    pub(super) fn paints_in(self, room: f32, beyond: f32) -> bool {
        room + 0.01 >= PEEK_MIN.min(self.tall).max(beyond + 0.51)
    }

    /// Whether the control fits whole in `room`.
    pub(super) fn fits_in(self, room: f32) -> bool {
        self.tall <= room + 0.01
    }
}

/// Which list a record measured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ScrollContext {
    route: SettingsRoute,
    search: String,
    categories: bool,
    question: bool,
}

/// A list [`list_page_whole`] seated, kept with its rows, by which a bar's overrun is told.
pub(super) struct ListLaid {
    items: std::rc::Rc<[ListItem]>,
    gap: f32,
    rows: ListRows,
}

/// The rooms a list's caller builds the same rows for, and those it builds rows as wide for.
pub(super) struct ListRows {
    pub(super) same: SameRows,
    pub(super) as_wide: SameRows,
}

/// The rooms a list's caller builds the same rows for.
pub(super) type SameRows = Box<dyn Fn(f32) -> bool>;

impl ListLaid {
    /// The list `items` and `rows` lay out, `gap` apart, as [`list_page_whole`] would seat it.
    pub(super) const fn new(items: std::rc::Rc<[ListItem]>, gap: f32, rows: ListRows) -> Self {
        Self { items, gap, rows }
    }

    /// Whether the list's caller builds the same rows in `room`.
    pub(super) fn builds_alike_in(&self, room: f32) -> bool {
        (self.rows.same)(room)
    }

    /// Whether the list's caller builds its rows as wide in `room`, each laid out as here.
    pub(super) fn builds_as_wide_in(&self, room: f32) -> bool {
        (self.rows.as_wide)(room)
    }

    /// The plan the rows seated in a list `room` tall under a bar that takes `given` of it lay out,
    /// as [`list_page_whole`] plans them.
    pub(super) fn planned(&self, room: f32, given: f32) -> std::rc::Rc<(ScrollPlan, ItemStops)> {
        shared_plan(&self.items, (self.gap, room, given), || {
            let fitted = fit_rows(&self.items, (room - given).max(0.0));
            plan_under_status(
                &fitted,
                self.gap,
                room,
                given,
                Overhang::NONE,
                ListFoot::Whole,
            )
        })
    }
}

/// What the last render of a scrolling page measured and where it put the list.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ScrollRecord {
    context: ScrollContext,
    plan: ScrollPlan,
    top: usize,
    drawn: Option<usize>,
    asked: usize,
    landed: Option<UiKey>,
    /// Whether `landed` yielded to focus ([`Self::opening`], [`SettingsViewState::seat_scroll`]).
    yielded: bool,
    /// Whether no event has arrived since opening ([`SettingsViewState::seat_scroll`]).
    opening: bool,
    /// What the person was left with of focus ([`SettingsViewState::seat_scroll`]).
    left: Option<Left>,
    /// The keyboard's control as this screen found it ([`SettingsViewState::seat_scroll`]).
    keyboard: Option<UiKey>,
    /// Whether the list's top showed `first_beside`'s first result whole, so none reveals it.
    beside_shown: bool,
    /// Whether this screen revealed what that first result replaced, as each after it does.
    beside_spent: bool,
    /// Whether it held the start of a stop moved in at its top, which a let-go seats anew.
    moved_start: bool,
    hold: bool,
    /// The plan and scroll the screen held, kept across re-measurements
    /// ([`SettingsViewState::seat_scroll`], [`SettingsViewState::settle_page_scroll`],
    /// [`SettingsViewState::drop_list_nudge`]).
    place: Option<Box<(ScrollPlan, usize)>>,
}

impl ScrollRecord {
    /// The stop the list's screen started at.
    pub(super) const fn top(&self) -> usize {
        self.top
    }

    /// The keyboard's control as this screen found it.
    pub(super) const fn keyboard(&self) -> Option<&UiKey> {
        self.keyboard.as_ref()
    }

    /// Whether this screen held the start of a stop moved in at its top, which a let-go seats anew.
    pub(super) const fn holds_moved_start(&self) -> bool {
        self.moved_start
    }

    /// Forget that the list's top showed a pinned filter's first result whole.
    pub(super) const fn forget_beside(&mut self) {
        self.beside_shown = false;
        self.beside_spent = false;
    }

    /// Whether `state` shows the list this record was drawn for.
    pub(super) fn same_context(&self, state: &SettingsViewState) -> bool {
        self.context == state.scroll_context()
    }

    fn unscrolled(&self, state: &SettingsViewState) -> bool {
        self.context == state.scroll_context()
            && self.top == state.scroll_now().min(self.plan.limit())
    }

    /// What this screen showed of `key`: `listed` where its list holds it, else as pinned.
    fn shown_of(&self, key: &UiKey, listed: Left) -> Option<Left> {
        if self.plan.stop_of(key).is_some() {
            Some(listed)
        } else {
            self.plan.pin.map(|pin| Left::pinned(key, pin))
        }
    }

    /// What the person was left with of `key`, the keyboard's control
    /// ([`Self::left`]); for a control this screen was not drawn to keep (the
    /// keyboard moved since), or once the person scrolled it (a page turned,
    /// a nudge dropped), what it shows of it.
    fn left_of(&self, key: &UiKey) -> Left {
        self.left
            .clone()
            .filter(|left| left.key == *key)
            .unwrap_or_else(|| Left::on(&self.plan, key, self.top))
    }

    /// The list's place as this screen held it ([`Self::place`]).
    fn place(&self) -> (&ScrollPlan, usize) {
        self.place
            .as_deref()
            .map_or((&self.plan, self.top), |(plan, top)| (plan, *top))
    }
}

impl SettingsViewState {
    fn scroll_now(&self) -> usize {
        let context = self.scroll_context();
        self.scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.context == context && record.asked == self.page_scroll)
            .and_then(|record| record.drawn)
            .unwrap_or(self.page_scroll)
    }

    pub(super) fn scroll_context(&self) -> ScrollContext {
        ScrollContext {
            route: self.route,
            search: self.search.trim().to_string(),
            categories: self.compact_navigation,
            question: self.question_scroll.is_some(),
        }
    }

    /// In a compact window the Reset All question scrolls on its own: the page's scroll is
    /// kept aside until [`Self::settle_question_scroll`].
    pub(super) fn open_question_scroll(&mut self) {
        if self.question_scroll.is_none() {
            self.question_scroll = Some((self.scroll_context(), self.page_scroll));
            self.page_scroll = 0;
            self.page_reveal = None;
        }
    }

    /// The question closed.
    pub(super) fn settle_question_scroll(&mut self) {
        if self.reset_all_confirmation {
            return;
        }
        if let Some((context, top)) = self.question_scroll.take()
            && context == self.scroll_context()
        {
            self.page_scroll = top;
        }
    }

    /// Seat the list a render measured ([`Self::settle_page_scroll`]).
    ///
    /// The keeping rules, in precedence order (the showcase keeping tests assert each on every
    /// frame of Settings ▸ Themes):
    ///
    /// - R1: A pure re-render (same size, text scale and bars, no event between) draws the
    ///   same screen.
    /// - R2: When the last screen showed the keyboard's control whole, listed or pinned,
    ///   nothing the person did not do (a resize, a text-size change, a status or the
    ///   aterm.toml reload warning bar coming or going, a landing measured again) takes it out
    ///   of view. A control the last screen showed cut is shown no less: the screen that shows
    ///   the most of it, never one that shows less.
    /// - R3: Before the first event the theme in use (the landing) is shown whole wherever that
    ///   keeps R2; where both can be shown whole, both are, first frame included. Where both
    ///   cannot, the keyboard's control wins. Where it is pinned, landing costs it nothing, so
    ///   the list lands. It is judged over R2's rungs: a give-way may show both whole.
    /// - R4: A wheel's place holds until an event moves it, except as far as R2 needs.
    /// - R5: A key that leaves the keyboard on the same control keeps that control in view
    ///   (typing in the chooser's filter, Up or Down on it, keys on Customize). A filter change
    ///   shows the filter and then the first results, never the page's top while the filter is
    ///   listed. It is judged over R2's rungs as R3 is: a give-way may show both whole.
    ///
    /// A status never covers what the person is on: the bar shortens, down to its one line of
    /// words, for the keyboard's control, listed or pinned, and the pinned strip lays out above
    /// the bar.
    ///
    /// R4's place across a re-layout (a resize, a text size, a bar): the keyboard's control as
    /// many stops under the screen's top as the wheel's screen showed it, where that screen
    /// showed its stop; else that screen's first stop by what it is; the list's top where the
    /// wheel left it there; clamped at the list's end.
    ///
    /// Exceptions are geometry only, measured on unclipped heights: no list is drawn, or no
    /// scroll shows the control or row whole even with the bar at its least.
    ///
    /// An event is one the person sent (a press, a key, the wheel, a focus move); a
    /// projection's reply (a Messages tick, a refresh) is none.
    pub(super) fn seat_scroll(&self, plan: &mut ScrollPlan) -> Range<usize> {
        #[cfg(test)]
        crate::work_counts::list_seated();
        let limit = plan.limit();
        let mut top = self.scroll_now().min(limit);
        let focus = self.common.last_focus.as_ref();
        let last_shown = |key: &UiKey| {
            let record = self.scroll_record.borrow();
            let record = record.as_ref()?;
            let stop = record.plan.stop_of(key)?;
            (record.unscrolled(self) && record.plan.shown(record.top).contains(&stop))
                .then_some((record.top, stop))
        };
        let place_shown = |key: &UiKey| {
            let record = self.scroll_record.borrow();
            let (was, at) = record
                .as_ref()
                .filter(|record| record.unscrolled(self))?
                .place();
            let stop = was.stop_of(key)?;
            was.shown(at).contains(&stop).then_some((at, stop))
        };
        // A control pinned round the list showed whatever the scroll: a scroll leaves it owed (R5).
        let owed = |key: &UiKey| {
            let record = self.scroll_record.borrow();
            record
                .as_ref()
                .filter(|record| {
                    record.unscrolled(self)
                        || (record.context == self.scroll_context()
                            && record.plan.stop_of(key).is_none()
                            && record.plan.pin.is_some())
                })
                .map(|record| record.left_of(key))
        };
        let owes = |key: &UiKey, now: &ScrollPlan, top: usize| {
            owed(key).is_some_and(|left| !left.kept_by(&Left::on(now, key, top)))
        };
        let held_start = self
            .scroll_record
            .borrow()
            .as_ref()
            .filter(|record| {
                record.unscrolled(self)
                    && record.top == top
                    && (record.plan.room - plan.room).abs() < 0.5
            })
            .map(|record| {
                let moved = !plan.holds_stop_of(&record.plan, top);
                (record.plan.held_seat(record.top), moved)
            });
        // A stop moved in by stops added or dropped above holds the seat too, unless let go (R2).
        plan.hold_start(None);
        let moved_start = held_start
            .is_some_and(|(held, moved)| moved && plan.held_seat(top) != held)
            .then_some(!self.start_let_go.get());
        let held_start = held_start
            .filter(|_| moved_start != Some(false))
            .map(|(held, _)| held);
        plan.hold_start(held_start);
        let first_screen = self
            .scroll_record
            .borrow()
            .as_ref()
            .is_none_or(|record| record.context != self.scroll_context());
        let opening = self.seats_opening();
        let carried = self
            .scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.unscrolled(self))
            .and_then(|record| {
                let (was, at) = record.place();
                was.seated.filter(|seat| seat.top == at)
            });
        let shown_first = || {
            let record = self.scroll_record.borrow();
            let (was, at) = record
                .as_ref()
                .filter(|record| record.unscrolled(self))?
                .place();
            if at == 0 {
                return Some((0, 0, 0));
            }
            let first = was.shown(at).start;
            (first..was.len()).find_map(|stop| {
                was.identities(stop)
                    .find_map(|id| plan.stop_of_identity(id))
                    .map(|index| (at, stop, index))
            })
        };
        let land = matches!(self.reveal_as, Reveal::Land | Reveal::LandWhole);
        let drawn_landing = self
            .scroll_record
            .borrow()
            .as_ref()
            .filter(|record| {
                land && record.drawn.is_some()
                    && record.context == self.scroll_context()
                    && record.landed.is_some()
                    && record.landed == self.page_reveal
                    && ((record.yielded && focus.is_some_and(|key| plan.stop_of(key).is_some()))
                        || ((record.plan.room - plan.room).abs() < 0.5
                            && record.plan.heights == plan.heights))
            })
            .map(|record| (record.landed.clone(), record.yielded));
        let (drawn_landing, mut yielded) = drawn_landing.unwrap_or_default();
        let landing = land && drawn_landing.is_none();
        let given_back = self.beside_given_back(plan, self.scroll_record.borrow().as_ref());
        let beside_spent = given_back.is_some();
        let revealing = given_back
            .unwrap_or(self.page_reveal.as_ref())
            .filter(|_| self.reveal_as != Reveal::Hold && drawn_landing.is_none());
        let kept_in_view = revealing.or(focus);
        let held_in_view = kept_in_view.and_then(last_shown);
        let held = kept_in_view
            .and_then(place_shown)
            .zip(kept_in_view)
            .and_then(|((last_top, last), key)| {
                plan.stop_of(key).map(|index| (last_top, last, index))
            })
            .or_else(|| revealing.is_none().then(shown_first).flatten());
        let mut carried = carried;
        if let Some((last_top, last, index)) = held {
            let moved = (last_top + index).saturating_sub(last);
            let moved = if index == last {
                let kept = moved.min(limit);
                if index < kept && kept < limit {
                    index
                } else {
                    kept
                }
            } else {
                moved.min(index)
            }
            .min(limit);
            carried = carried.map(|seat| Seat { top: moved, ..seat });
            top = moved;
        }
        plan.seated = carried.filter(|seat| seat.top == top && plan.seat_fits(*seat));
        if let Some((seat_top, seat)) = self.held_place(plan, top, held) {
            top = seat_top;
            plan.seated = seat;
        }
        let mut landed = drawn_landing;
        let (relanding, moved) = self
            .scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.context == self.scroll_context())
            .map_or((false, false), |record| {
                (
                    record.drawn.is_some()
                        && record.landed.is_some()
                        && record.landed == self.page_reveal,
                    record.drawn.is_none() && record.keyboard.as_ref() != focus,
                )
            });
        let keyboard_stop = focus
            .filter(|key| {
                landing
                    && self.page_reveal.as_ref() != Some(*key)
                    && (opening
                        || moved
                        || (relanding && owed(key).is_some_and(|left| left.shows())))
            })
            .and_then(|key| plan.stop_of(key));
        if keyboard_stop.is_some() && !first_screen {
            top = self.page_scroll.min(limit);
            plan.seated = None;
            plan.hold_start(None);
        }
        // Where a screen shows both, the landing gives way to nothing (R3): the keyboard's
        // control whole with the landing, on the screen nearest the landing's.
        let keyboard_stop = keyboard_stop.filter(|_| {
            let both = focus.zip(revealing).and_then(|(key, landing)| {
                let index = plan.stop_holding(landing)?;
                let span = plan.span_of(index, Some(landing), self.reveal_as);
                let near = plan.reveal_span(index, top, span);
                let seat = plan.seated.take();
                let both = plan.keeping_with(key, &Left::whole(key), (index, span), near);
                plan.seated = if both.is_some() { None } else { seat };
                both
            });
            if let Some(both) = both {
                top = both;
            }
            both.is_none()
        });
        yielded |= keyboard_stop.is_some();
        let reveal = match keyboard_stop {
            Some(index) => Some((index, Reveal::Keep, None)),
            None => revealing.and_then(|key| {
                plan.stop_holding(key)
                    .map(|index| (index, self.reveal_as, Some(key)))
            }),
        };
        if let Some((index, reveal_as, key)) = reveal {
            let reveal_as =
                if reveal_as == Reveal::Keep && held_in_view.is_some() && plan.gives_to_status() {
                    Reveal::Pressed
                } else {
                    reveal_as
                };
            let span = plan.span_of(index, key, reveal_as);
            if landing {
                landed.clone_from(&self.page_reveal);
            }
            if plan.seated.is_none() || !plan.shows_span(index, top, span) {
                let pressed = if reveal_as == Reveal::Pressed {
                    plan.press_seat_span(index, top, span)
                } else {
                    None
                };
                let nudge = pressed.map(|seat| seat.nudge);
                plan.seated = pressed;
                let reached = (nudge.is_none() && reveal_as == Reveal::Pressed)
                    .then(|| plan.seat_span(index, top, span))
                    .flatten();
                if let Some(seat) = reached {
                    plan.seated = Some(seat);
                } else if nudge.is_none() {
                    top = match reveal_as {
                        Reveal::Keep => plan.reveal(index, top),
                        Reveal::Pressed | Reveal::Land | Reveal::Hold => {
                            plan.reveal_span(index, top, span)
                        }
                        Reveal::LandWhole => {
                            let (landed, seat) = plan.land_selected(index, top);
                            plan.seated = seat;
                            landed
                        }
                    };
                }
            }
            if plan.seated.is_none() && reveal_as != Reveal::LandWhole {
                plan.seated = plan.seat_span(index, top, span);
            }
            // A reveal of the keyboard's control that cannot show it whole shows the most of it
            // (R2, R5).
            if let Some(key) = key.filter(|key| focus == Some(*key) && plan.stop_of(key).is_some())
                && !Left::on(plan, key, top).whole
            {
                plan.seated = None;
                top = plan.keeping_top(key, &Left::whole(key), top);
            }
            // A reveal of another control never takes the keyboard's out of view (R2, R5): the
            // screen nearest the reveal's that keeps what the person was left with of it shows
            // the revealed stop too where one can.
            if let Some(focus) = focus.filter(|focus| {
                keyboard_stop.is_none()
                    && key.is_some_and(|key| plan.resolve(key) != plan.resolve(focus))
                    && plan.stop_of(focus).is_some()
            }) && let Some(want) = owed(focus)
                && !want.kept_by(&Left::on(plan, focus, top))
            {
                plan.seated = None;
                top = plan
                    .keeping_with(focus, &want, (index, span), top)
                    .unwrap_or_else(|| plan.keeping_top(focus, &want, top));
            }
        } else if let Some(key) = focus
            && let Some(index) = plan.stop_of(key)
            && (first_screen || owes(key, plan, top))
        {
            let reveal = if first_screen {
                Reveal::Keep
            } else {
                Reveal::Pressed
            };
            let span = plan.span_of(index, Some(key), reveal);
            if !plan.shows_span(index, top, span) {
                let (place, seat) = (top, plan.seated);
                let at_place = Left::on(plan, key, place);
                let pressed = (reveal == Reveal::Pressed)
                    .then(|| plan.press_seat_span(index, top, span))
                    .flatten();
                plan.seated = pressed;
                if pressed.is_none() {
                    top = match reveal {
                        Reveal::Keep => plan.reveal(index, top),
                        _ => plan.reveal_span(index, top, span),
                    };
                    plan.seated = plan.seat_span(index, top, span);
                }
                // Where that screen still shows less, the one that shows the most, nearest the
                // place (R4), and the place itself where no screen shows more.
                if !first_screen
                    && let Some(want) =
                        owed(key).filter(|want| !want.kept_by(&Left::on(plan, key, top)))
                {
                    plan.seated = None;
                    let most = plan.keeping_top(key, &want, place);
                    let bare = Left::on(plan, key, most);
                    plan.seated = plan.seat_span(index, most, span);
                    if !bare.kept_by(&Left::on(plan, key, most)) {
                        plan.seated = None;
                    }
                    let now = Left::on(plan, key, most);
                    if want.kept_by(&now) || now.part > at_place.part + 0.5 {
                        top = most;
                    } else {
                        (top, plan.seated) = (place, seat);
                    }
                }
            }
        }
        // Before the first event the landing shows wherever R2 lets it (R3), whatever kept the
        // keyboard's listed control.
        if opening && land {
            self.land_beside_keyboard(plan, &mut top, owed);
        }
        // A control in a stop taller than the room shows whole where it fits, else from its top.
        if let Some(key) = focus
            && let Some(index) = plan.stop_of(key)
            && let Some(span) = plan.control_span(index, key)
            && !plan.shows_span(index, top, span)
        {
            let before = Left::on(plan, key, top);
            let owing = owed(key).filter(|want| !want.kept_by(&before));
            if (first_screen || revealing == Some(key) || owing.is_some())
                && let Some(seat) = plan.tall_stop_seat(key)
            {
                let (was, seated) = (top, plan.seated.replace(seat));
                top = seat.top;
                let more = owing.is_some_and(|want| !want.whole)
                    && Left::on(plan, key, top).part > before.part + 0.5;
                if !plan.shows_span(index, top, span) && !more {
                    (top, plan.seated) = (was, seated);
                }
            }
        }
        // A PRESSED CONTROL STAYS WHERE IT WAS PRESSED: where what the press brought changed the
        // stops above it or around it (a row's note, a card's padding chosen by its tallest
        // row) and the seat left the control elsewhere, the screen that draws it whole where it
        // was pressed holds it there; what the press brought below comes into view as the list
        // stands. A press that turned Supervise coding agents off brought its rows' notes, the
        // card re-padded, and the switch went 10pt down under the pointer (Harness at 744x420
        // with 2x text).
        if let Some((held_top, seat)) = self.press_hold(plan, top) {
            top = held_top;
            plan.seated = seat;
        }
        let left = focus.map(|key| {
            let now = Left::on(plan, key, top);
            let record = self.scroll_record.borrow();
            record
                .as_ref()
                .filter(|record| record.drawn.is_some() && record.unscrolled(self))
                .and_then(|record| record.left.clone())
                .filter(|was| was.key == *key && !was.kept_by(&now))
                .unwrap_or(now)
        });
        let place = reveal
            .is_none()
            .then(|| {
                let record = self.scroll_record.borrow();
                // A screen scrolled since, the list no further, holds no place of its own (R4).
                record
                    .as_ref()
                    .filter(|record| record.unscrolled(self) && record.hold)
                    .map(|record| {
                        record
                            .place
                            .clone()
                            .unwrap_or_else(|| Box::new((record.plan.clone(), record.top)))
                    })
            })
            .flatten();
        let beside_shown = beside_spent
            || self.first_beside().is_some_and(|(key, _)| {
                plan.stop_holding(key).is_some_and(|index| {
                    plan.shows_span(index, 0, plan.span_of(index, Some(key), Reveal::Keep))
                })
            });
        let view = plan.screen(top).view;
        self.record_result_page_limit(limit);
        self.page_window.set(Some(plan.shown(top).len().max(1)));
        self.scroll_record.replace(Some(ScrollRecord {
            context: self.scroll_context(),
            plan: plan.clone(),
            top,
            drawn: Some(top),
            asked: self.page_scroll,
            yielded,
            opening,
            left,
            keyboard: focus.cloned(),
            beside_shown,
            beside_spent,
            moved_start: moved_start == Some(true) && plan.held_start.is_some(),
            landed,
            hold: true,
            place,
        }));
        view
    }

    /// Move `top` to the screen nearest the landing's that shows the landing whole and keeps what
    /// R2 owes the keyboard's listed control (`owed`, else whole, else the most any screen shows),
    /// where one does (R3).
    fn land_beside_keyboard(
        &self,
        plan: &mut ScrollPlan,
        top: &mut usize,
        owed: impl Fn(&UiKey) -> Option<Left>,
    ) {
        let (Some(landing), Some(key)) =
            (self.page_reveal.as_ref(), self.common.last_focus.as_ref())
        else {
            return;
        };
        let Some(index) = plan.stop_holding(landing) else {
            return;
        };
        if plan.stop_of(key).is_none() || plan.resolve(key) == plan.resolve(landing) {
            return;
        }
        let span = plan.span_of(index, Some(landing), self.reveal_as);
        if plan.shows_span(index, *top, span) {
            return;
        }
        let seat = plan.seated.take();
        let want = owed(key).unwrap_or_else(|| Left::whole(key));
        let best = Left::on(plan, key, plan.keeping_top(key, &want, *top));
        let want = if want.kept_by(&best) { want } else { best };
        let near = plan.reveal_span(index, *top, span);
        match plan.keeping_with(key, &want, (index, span), near) {
            Some(both) => *top = both,
            None => plan.seated = seat,
        }
    }

    /// Adopt where the last render put this page's list, once.
    pub(super) fn settle_page_scroll(&mut self) {
        let context = self.scroll_context();
        let mut record = self.scroll_record.borrow_mut();
        let Some(record) = record.as_mut().filter(|record| record.context == context) else {
            return;
        };
        let Some(top) = record.drawn.take() else {
            return;
        };
        self.page_events = true;
        self.page_scroll = top;
        record.place = None;
        let landed = record.landed.take();
        if matches!(self.reveal_as, Reveal::Land | Reveal::LandWhole)
            && landed.is_some()
            && landed == self.page_reveal
        {
            self.reveal_as = Reveal::Keep;
            self.page_reveal = None;
        }
    }

    /// Scroll the page's list to `top` as an event does.
    #[cfg(test)]
    pub(crate) fn scroll_page_to(&mut self, top: usize) {
        self.settle_page_scroll();
        self.page_scroll = top;
    }

    /// Reveals a newly focused control in the list ([`Self::page_reveal`]).
    pub(super) fn reveal_moved_focus(&mut self, before: Option<UiKey>, pressed: bool) {
        if self.page_reveal.is_some() && self.scroll_record.borrow().is_none() {
            return;
        }
        if self.reveal_as == Reveal::Hold && self.page_reveal == self.common.last_focus {
            return;
        }
        if self.common.last_focus != before
            && let Some(key) = self.common.last_focus.clone()
        {
            // THE KEYBOARD READS A PRESS'S STATUS WITHOUT MOVING ANYTHING: a stop of the status's
            // bar, or a control the screen shows whole clear of the bar, asks nothing of the list,
            // so the press holds it, the bar where it stands. Taken as moving on, Shift-Tab from
            // a switch pressed low on a narrow list to its status's Undo over the list sent the
            // bar back under the list, over the switch, and the list moved it up from under it.
            if !pressed && self.press_holds_place() && self.shown_clear_of_press(&key) {
                return;
            }
            let landing = self.page_reveal.is_some()
                && matches!(self.reveal_as, Reveal::Land | Reveal::LandWhole);
            if landing && !self.listed_on_page(&key) {
                return;
            }
            self.page_reveal = Some(key);
            self.reveal_as = if pressed {
                Reveal::Pressed
            } else {
                Reveal::Keep
            };
        }
    }

    /// Where on `plan` the list sits as the last render held it ([`ScrollRecord::place`]).
    fn held_place(
        &self,
        plan: &ScrollPlan,
        top: usize,
        held: Option<(usize, usize, usize)>,
    ) -> Option<(usize, Option<Seat>)> {
        let record = self.scroll_record.borrow();
        let record = record.as_ref()?;
        let (was, at) = record.place();
        if !record.hold
            || !record.unscrolled(self)
            || at < was.limit()
            || plan.foot == ListFoot::Whole
            || plan.len() == 0
        {
            return None;
        }
        if let Some(seat) = was.seated.filter(|seat| seat.top == at)
            && top == at
            && top >= plan.limit()
        {
            let (before, now) = (was.stop_screen(at), plan.stop_screen(top));
            if before.drawn.start == now.drawn.start {
                if (before.offset - now.offset).abs() < 0.01 {
                    return None;
                }
                let seat = Seat {
                    nudge: seat.nudge + before.offset - now.offset,
                    ..seat
                };
                return plan.seat_fits(seat).then_some((top, Some(seat)));
            }
        }
        let drawn = was.screen(at);
        if drawn.whole {
            return None;
        }
        let first = drawn.drawn.start;
        let (anchor_was, anchor) = match held {
            Some((_, last, index)) if last >= first => (last, index),
            _ => {
                let index = was
                    .identities(first)
                    .find_map(|id| plan.stop_of_identity(id))?;
                (first, index)
            }
        };
        let at_was = was.stop_top(first, anchor_was) - drawn.offset;
        let on = |screen: &Screen| plan.stop_top(screen.drawn.start, anchor) - screen.offset;
        let seated = plan.stop_screen(top.min(plan.limit()));
        if seated.drawn.start <= anchor && (on(&seated) - at_was).abs() < 0.5 {
            return None;
        }
        let start = was
            .identities(first)
            .find_map(|id| plan.stop_of_identity(id))
            .filter(|start| *start <= anchor && *start <= plan.limit())?;
        let base = plan.stop_screen(start);
        if base.view.start != start || base.whole {
            return None;
        }
        let over = |plan: &ScrollPlan, start: usize, stop: usize| {
            (plan.stop_top(start, stop), plan.heights[stop])
        };
        let (above_was, tall_was) = over(was, first, anchor_was);
        let (above, tall) = over(plan, start, anchor);
        if (above_was - above).abs() > 0.5 || (tall_was - tall).abs() > 0.5 {
            return None;
        }
        let seat = Seat {
            top: start,
            nudge: on(&base) - at_was,
            bare: drawn.bare,
        };
        plan.seat_fits(seat).then_some((start, Some(seat)))
    }

    /// Drop the list's nudge, held place and kept control after a scroll
    /// ([`ScrollRecord::left_of`]).
    pub(super) fn drop_list_nudge(&self) {
        if let Some(record) = self.scroll_record.borrow_mut().as_mut() {
            record.plan.seated = None;
            record.hold = false;
            record.place = None;
            record.left = None;
        }
        // The person scrolled: a status's bar over the list's top would cover what scrolls under
        // it, so it stands under the list from now on, and the press's place is no screen's.
        if let Some(at) = self.pressed_at.borrow_mut().as_mut() {
            at.band_gone = true;
            at.scrolled = true;
        }
        self.list_reseated.set(true);
    }

    fn listed_on_page(&self, key: &UiKey) -> bool {
        let context = self.scroll_context();
        self.scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.context == context)
            .is_some_and(|record| record.plan.stop_of(key).is_some())
    }

    /// The plan the last render of this page recorded.
    pub(super) fn scroll_plan(&self) -> Option<ScrollPlan> {
        let context = self.scroll_context();
        self.scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.context == context)
            .map(|record| record.plan.clone())
    }

    /// The stops the last render of this page showed, as it drew them.
    pub(super) fn scroll_shown(&self) -> Option<(ScrollPlan, Range<usize>)> {
        let context = self.scroll_context();
        self.scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.context == context)
            .map(|record| (record.plan.clone(), record.plan.shown(record.top)))
    }

    /// Page Down (`forward`) or Page Up on the page's list.
    pub(super) fn turn_list_page(&mut self, forward: bool) -> bool {
        let context = self.scroll_context();
        let turned = {
            let mut record = self.scroll_record.borrow_mut();
            record
                .as_mut()
                .filter(|record| record.context == context)
                .map(|record| {
                    let seat = record.plan.seated.filter(|seat| seat.top == record.top);
                    let (top, seat) = if forward {
                        record.plan.page_down(record.top, seat)
                    } else {
                        record.plan.page_up_from(record.top, seat)
                    };
                    record.plan.seated = seat;
                    record.top = top;
                    record.drawn = None;
                    record.left = None;
                    (top, record.plan.limit())
                })
        };
        let Some((top, limit)) = turned else {
            return false;
        };
        self.reduce_page_scroll(limit, super::SettingsPageScrollCommand::Absolute(top));
        true
    }

    /// Where the wheel's `lines` take the page's list, a landing a line from where the last
    /// render seated it ([`ScrollPlan::wheel_from`]).
    pub(super) fn wheel_landing(&self, lines: i32) -> Option<(usize, Option<Seat>)> {
        let context = self.scroll_context();
        let record = self.scroll_record.borrow();
        let record = record.as_ref().filter(|record| record.context == context)?;
        let plan = &record.plan;
        let mut place = (
            record.top,
            plan.seated.filter(|seat| seat.top == record.top),
        );
        for _ in 0..lines.unsigned_abs().min(plan.len() as u32 * 64 + 64) {
            let next = plan.wheel_from(place.0, place.1, lines > 0);
            if next == place {
                break;
            }
            place = next;
        }
        Some(place)
    }

    /// Seat the page's list at stop `top`, at `seat`, as a scroll does ([`Self::wheel_landing`]).
    pub(super) fn seat_list_at(&self, top: usize, seat: Option<Seat>) {
        let context = self.scroll_context();
        if let Some(record) = self
            .scroll_record
            .borrow_mut()
            .as_mut()
            .filter(|record| record.context == context)
        {
            record.plan.seated = seat;
            record.top = top;
            record.drawn = None;
        }
    }

    /// Bring `key` into view at the next render when the page's list, or the rail, holds it off
    /// screen.
    pub(crate) fn reveal_listed_control(&mut self, key: &UiKey) -> bool {
        if key.as_str().starts_with("settings/nav/") {
            *self.navigation_reveal.borrow_mut() = Some(key.clone());
            return true;
        }
        let listed = self
            .scroll_plan()
            .is_some_and(|plan| plan.stop_of(key).is_some())
            || self.open_disclosure_holding(key);
        if listed {
            self.reveal_control(key.clone());
        }
        listed
    }

    /// The keyboard's control, where the last screen showed it in the list, whole (`whole`) or
    /// any of it: a key that leaves the keyboard on it scrolls the list no further than keeps
    /// it so (R5 of [`Self::seat_scroll`]).
    pub(super) fn keyboard_shown(&self, whole: bool) -> Option<UiKey> {
        let key = self.common.last_focus.clone()?;
        let record = self.scroll_record.borrow();
        let record = record.as_ref().filter(|record| record.unscrolled(self))?;
        let left = record.left_of(&key);
        (record.plan.stop_of(&key).is_some() && if whole { left.whole } else { left.shows() })
            .then_some(key)
    }

    /// A pinned filter's first result revealed beside the filter, and the reveal it replaced.
    fn first_beside(&self) -> Option<&(UiKey, Option<UiKey>)> {
        self.first_beside.as_ref().filter(|(key, _)| {
            self.reveal_as == Reveal::Keep
                && self.page_reveal.as_ref() == Some(key)
                && self.common.last_focus.as_ref() != Some(key)
        })
    }

    /// The reveal a first result gives back once shown whole at the top and then re-laid (R5, R1).
    fn beside_given_back(
        &self,
        plan: &ScrollPlan,
        record: Option<&ScrollRecord>,
    ) -> Option<Option<&UiKey>> {
        let (_, before) = self.first_beside()?;
        let record = record.filter(|record| record.context == self.scroll_context())?;
        let relaid = !(record.drawn.is_some()
            && (record.plan.room - plan.room).abs() < 0.5
            && record.plan.heights == plan.heights);
        (record.beside_spent || (record.beside_shown && relaid)).then_some(before.as_ref())
    }

    /// Whether the reveal is the control `record`'s screen pinned, the seat's to keep (R5).
    pub(super) fn reveals_pinned(&self, record: Option<&ScrollRecord>) -> bool {
        self.reveal_as == Reveal::Keep
            && self.page_reveal.is_some()
            && self.page_reveal == self.common.last_focus
            && record.is_some_and(|record| {
                record.plan.pin.is_some()
                    && self
                        .page_reveal
                        .as_ref()
                        .is_some_and(|key| record.plan.stop_of(key).is_none())
            })
    }

    /// Whether the last list cuts the keyboard's control, or pins it where a bar covers some.
    pub(super) fn keyboard_cut(&self) -> bool {
        let Some(key) = self.common.last_focus.as_ref() else {
            return false;
        };
        let context = self.scroll_context();
        let record = self.scroll_record.borrow();
        record
            .as_ref()
            .filter(|record| record.context == context)
            .is_some_and(|record| {
                let plan = &record.plan;
                let Some(index) = plan.stop_of(key) else {
                    return plan.pin.is_some_and(|pin| !Left::pinned(key, pin).whole);
                };
                let (from, to) = plan
                    .control_span(index, key)
                    .unwrap_or_else(|| plan.controls_of(index));
                let whole = |plan: &ScrollPlan, top: usize| {
                    let left = Left::on(plan, key, top);
                    left.whole || left.part + 0.5 >= to - from
                };
                let partway = || {
                    let seat = plan.tall_stop_seat(key)?;
                    let mut seated = plan.clone();
                    seated.seated = Some(seat);
                    Some(whole(&seated, seat.top))
                };
                !whole(plan, record.top)
                    && !(0..=plan.limit()).any(|top| whole(plan, top))
                    && partway() != Some(true)
            })
    }

    /// Whether a screen seated now precedes the first event, as [`Self::landing_cut`] reads it.
    pub(super) fn seats_opening(&self) -> bool {
        let context = self.scroll_context();
        self.scroll_record.borrow().as_ref().is_none_or(|record| {
            record.context != context || (record.opening && record.drawn.is_some())
        })
    }

    /// Whether no event has arrived since the page opened, whatever its list's context since (R3).
    pub(super) const fn landing_untouched(&self) -> bool {
        !self.page_events
    }

    /// Whether the list lands on the theme in use, which [`Self::landing_cut`] reads.
    pub(super) const fn landing_owed(&self) -> bool {
        self.page_reveal.is_some() && matches!(self.reveal_as, Reveal::Land | Reveal::LandWhole)
    }

    /// Whether the screen last seated before the person's first event cuts the theme in use.
    pub(super) fn landing_cut(&self) -> bool {
        let Some(landing) = self.page_reveal.as_ref().filter(|_| self.landing_owed()) else {
            return false;
        };
        let context = self.scroll_context();
        let record = self.scroll_record.borrow();
        record
            .as_ref()
            .filter(|record| record.opening && record.context == context)
            .is_some_and(|record| {
                record
                    .plan
                    .stop_holding(landing)
                    .is_some_and(|index| !record.plan.shows_landing(index, record.top))
            })
    }

    /// What the list reveals: the landing (R3), or a first result or the reveal it gave back (R5).
    pub(super) fn reveal_owed(&self) -> Option<&UiKey> {
        let spent = || {
            self.scroll_record.borrow().as_ref().is_some_and(|record| {
                record.context == self.scroll_context() && record.beside_spent
            })
        };
        let reveal = match self.first_beside() {
            Some((_, before)) if spent() => before.as_ref(),
            _ => self.page_reveal.as_ref(),
        };
        reveal.filter(|key| {
            self.landing_owed()
                || (self.reveal_as == Reveal::Keep && self.common.last_focus.as_ref() != Some(*key))
        })
    }

    /// Whether the screen last seated cuts what the list reveals beside the keyboard's control.
    pub(super) fn reveal_cut(&self) -> bool {
        if self.landing_owed() {
            return self.landing_cut();
        }
        let Some(reveal) = self.reveal_owed() else {
            return false;
        };
        let context = self.scroll_context();
        let record = self.scroll_record.borrow();
        record
            .as_ref()
            .filter(|record| record.context == context)
            .is_some_and(|record| {
                record
                    .plan
                    .stop_holding(reveal)
                    .is_some_and(|index| !record.plan.shows_landing(index, record.top))
            })
    }

    /// Whether the screen last seated paints any of what the list reveals beside the keyboard.
    pub(super) fn reveal_painted(&self) -> bool {
        let Some(reveal) = self.reveal_owed() else {
            return false;
        };
        let context = self.scroll_context();
        let record = self.scroll_record.borrow();
        record
            .as_ref()
            .filter(|record| record.context == context)
            .is_some_and(|record| {
                let plan = &record.plan;
                plan.stop_holding(reveal).is_some_and(|index| {
                    let screen = plan.screen(record.top.min(plan.limit()));
                    let span = plan.revealed_span(index, Reveal::Keep);
                    plan.painted_of_span_on(&screen, index, reveal, span) > 0.0
                })
            })
    }

    /// Whether the theme in use fits a room `more` taller, with `keyboard` its control too (R3).
    pub(super) fn landing_fits(&self, more: f32, keyboard: bool) -> bool {
        let Some(landing) = self.page_reveal.as_ref() else {
            return false;
        };
        let context = self.scroll_context();
        let record = self.scroll_record.borrow();
        record
            .as_ref()
            .filter(|record| record.context == context)
            .and_then(|record| {
                let plan = &record.plan;
                let index = plan.stop_holding(landing)?;
                let at = plan.stop_top(0, index);
                let (from, to) = plan.revealed_span(index, Reveal::Keep);
                let (from, to) = keyboard
                    .then_some(self.common.last_focus.as_ref())
                    .flatten()
                    .and_then(|key| {
                        let stop = plan.stop_of(key)?;
                        Left::on(plan, key, record.top).whole.then(|| {
                            let at = plan.stop_top(0, stop);
                            let (top, bottom) = plan
                                .control_span(stop, key)
                                .unwrap_or_else(|| plan.controls_of(stop));
                            (at + top, at + bottom)
                        })
                    })
                    .map_or((at + from, at + to), |(top, bottom)| {
                        ((at + from).min(top), (at + to).max(bottom))
                    });
                Some(to - from <= plan.room + more + 0.01)
            })
            .unwrap_or_default()
    }

    /// What the screen last seated shows of the keyboard's listed control.
    pub(super) fn keyboard_left(&self) -> Option<Left> {
        let key = self.common.last_focus.as_ref()?;
        let context = self.scroll_context();
        let record = self.scroll_record.borrow();
        let record = record.as_ref().filter(|record| record.context == context)?;
        record
            .plan
            .stop_of(key)
            .map(|_| Left::on(&record.plan, key, record.top))
    }

    /// What the screen last seated shows of the keyboard's control, listed or else pinned.
    pub(super) fn keyboard_on_page(&self) -> Option<Left> {
        let key = self.common.last_focus.as_ref()?;
        let context = self.scroll_context();
        let record = self.scroll_record.borrow();
        let record = record.as_ref().filter(|record| record.context == context)?;
        record.shown_of(key, Left::on(&record.plan, key, record.top))
    }

    /// Whether the screen last seated shows the keyboard's control less than `record`'s (R2).
    pub(super) fn keyboard_short_of(&self, record: Option<&ScrollRecord>) -> bool {
        self.common.last_focus.is_some() && !self.keyboard_kept(self.keyboard_was(record).as_ref())
    }

    /// How much of the keyboard's listed control the list owes the person: what `record`, the
    /// screen before, showed of it where that was cut, else all of it (R2 of
    /// [`Self::seat_scroll`]).
    pub(super) fn keyboard_owed(&self, record: Option<&ScrollRecord>) -> Option<f32> {
        let tall = self.keyboard_height();
        match self.keyboard_was(record) {
            Some(left) if !left.whole && !tall.is_some_and(|tall| left.grown(tall)) => {
                Some(left.part)
            }
            _ => tall,
        }
    }

    /// What `record` left of the keyboard's control where it was drawn for it, listed or pinned.
    fn keyboard_was(&self, record: Option<&ScrollRecord>) -> Option<Left> {
        let key = self.common.last_focus.as_ref()?;
        record
            .filter(|record| record.same_context(self) && record.keyboard.as_ref() == Some(key))
            .and_then(|record| record.shown_of(key, record.left_of(key)))
    }

    /// What screens paint of the keyboard's listed control, as the list last seated lays it out.
    pub(super) fn keyboard_peek(&self) -> Option<Peek> {
        Some(Peek {
            tall: self.keyboard_height()?,
        })
    }

    /// How tall the list last seated lays the keyboard's listed control out.
    fn keyboard_height(&self) -> Option<f32> {
        let key = self.common.last_focus.as_ref()?;
        let context = self.scroll_context();
        let record = self.scroll_record.borrow();
        let plan = &record
            .as_ref()
            .filter(|record| record.context == context)?
            .plan;
        let index = plan.stop_of(key)?;
        let (from, to) = plan
            .control_span(index, key)
            .unwrap_or_else(|| plan.controls_of(index));
        Some(to - from)
    }

    /// Whether the last list lays the keyboard's listed control out unlike `record`'s screen.
    pub(super) fn keyboard_relaid(&self, record: Option<&ScrollRecord>) -> bool {
        let Some(key) = self.common.last_focus.as_ref() else {
            return false;
        };
        let was = record
            .filter(|record| record.same_context(self))
            .and_then(|record| {
                let plan = &record.plan;
                let index = plan.stop_of(key)?;
                let (from, to) = plan
                    .control_span(index, key)
                    .unwrap_or_else(|| plan.controls_of(index));
                Some(to - from)
            });
        was.zip(self.keyboard_height())
            .is_some_and(|(was, now)| (was - now).abs() > 0.5)
    }

    /// How tall the list last seated lays the keyboard's listed control out, where its screen
    /// shows it whole.
    pub(super) fn keyboard_whole(&self) -> Option<f32> {
        self.keyboard_left()
            .is_some_and(|left| left.whole)
            .then(|| self.keyboard_height())
            .flatten()
    }

    /// Whether the screen last seated shows the keyboard's control more than `was` did.
    pub(super) fn keyboard_shows_more(&self, was: &Left) -> bool {
        self.keyboard_left().is_some_and(|now| !now.kept_by(was))
    }

    /// Whether the last screen shows the keyboard's control, listed or pinned, no less than `was`.
    pub(super) fn keyboard_kept(&self, was: Option<&Left>) -> bool {
        Left::kept(was, self.keyboard_on_page().as_ref())
    }

    /// Keep `key` in view at the next render, its whole stop.
    pub(super) fn reveal_control(&mut self, key: UiKey) {
        self.page_reveal = Some(key);
        self.reveal_as = Reveal::Keep;
    }

    /// Keep control `key` in view at the next render, where the screen drawn showed it whole.
    pub(super) fn reveal_pressed(&mut self, key: UiKey) {
        let segment = self
            .scroll_record
            .borrow()
            .as_ref()
            .and_then(|record| record.place().0.segment_place(&key));
        let at = self.drawn_span(&key).map(|(top, bottom)| PressedAt {
            context: self.scroll_context(),
            key: key.clone(),
            bottom,
            before: self.bar_feedback().cloned(),
            status: None,
            awaits: false,
            top,
            side: None,
            band_gone: self.list_reseated.get(),
            scrolled: self.list_reseated.get(),
            list_top: self.laid_list_top.get(),
            segment,
        });
        self.pressed_at.replace(at);
        self.page_reveal = Some(key);
        self.reveal_as = Reveal::Pressed;
    }

    /// Whether the keyboard on `key` asks nothing of the list a press holds: a stop of the bar of
    /// the status it brought, or a control the screen last drawn shows whole, below that bar
    /// where it stands over the list's top ([`Self::press_holds_place`]).
    fn shown_clear_of_press(&self, key: &UiKey) -> bool {
        if super::status_bar_stop(key) {
            return true;
        }
        let Some((top, _)) = self.drawn_span(key) else {
            return false;
        };
        self.status_place.get() != super::StatusPlace::Over
            || self
                .status_band
                .get()
                .is_some_and(|band| top + 0.01 >= band)
    }

    /// Where control `key` begins and ends, down from the list's top, on the screen last drawn,
    /// where that screen showed it whole in the list's room.
    fn drawn_span(&self, key: &UiKey) -> Option<(f32, f32)> {
        let record = self.scroll_record.borrow();
        let record = record.as_ref().filter(|record| record.unscrolled(self))?;
        let (plan, top) = record.place();
        let index = plan.stop_holding(key).or_else(|| plan.stop_of(key))?;
        let (from, to) = plan.control_span(index, key)?;
        let screen = plan.screen(top.min(plan.limit()));
        if !screen.drawn.contains(&index) {
            return None;
        }
        let at = plan.stop_top(screen.drawn.start, index) - screen.offset;
        (at + from >= -0.01 && at + to <= plan.room + 0.01).then_some((at + from, at + to))
    }

    /// THE BAR OF A STATUS A PRESS BROUGHT TAKES THE SIDE OF THE LIST AWAY FROM THE PRESS: the
    /// room over the top of the press's list, down to the control, while the bar may stand
    /// there. That is while the press holds ([`Self::pressed_bottom`]) on the screen it was
    /// pressed on, and the keyboard is on no other control of the list that room covers.
    pub(super) fn press_band(&self) -> Option<f32> {
        self.pressed_bottom()?;
        let mut held = self.pressed_at.borrow_mut();
        let at = held
            .as_mut()
            .filter(|at| !at.band_gone && self.press_stands(at))?;
        let band = at.top;
        // What the bar drawn over the list covers, where one stands there: a control under it is
        // covered, one between it and the control pressed is not.
        let covers = self.status_band.get().map_or(band, |drawn| drawn.min(band));
        let record = self.scroll_record.borrow();
        let record = record.as_ref().filter(|record| record.unscrolled(self))?;
        let (plan, top) = record.place();
        let pressed = at.key_in(plan);
        let covered = self
            .common
            .last_focus
            .as_ref()
            .filter(|focus| **focus != at.key && **focus != pressed)
            .and_then(|focus| {
                let index = plan.stop_holding(focus).or_else(|| plan.stop_of(focus))?;
                let (from, _) = plan.control_span(index, focus)?;
                let screen = plan.screen(top.min(plan.limit()));
                screen.drawn.contains(&index).then(|| {
                    plan.stop_top(screen.drawn.start, index) - screen.offset + from < covers - 0.01
                })
            })
            .unwrap_or(false);
        if covered {
            at.band_gone = true;
            return None;
        }
        Some(band)
    }

    /// Whether a press holds the list on the screen it was pressed on, the status it brought
    /// standing ([`Self::pressed_bottom`]): its bar then takes the side that moves nothing
    /// ([`Self::press_side`]).
    pub(super) fn press_holds_place(&self) -> bool {
        self.pressed_bottom().is_some()
            && self
                .pressed_at
                .borrow()
                .as_ref()
                .is_some_and(|at| self.press_stands(at) && at.status.is_some())
    }

    /// THE PERSON MOVES ON FROM A PRESS: a press holds the list where it found its control only
    /// while the person has not scrolled the list since and the list still reveals what the
    /// press found, no page turned, no control the keyboard went on to, nothing else asked into
    /// view. Held past that, the list kept the switch pressed where it was and the controls Tab
    /// went on to, and the page Page Down turned to, never came into view (Security at 584x348:
    /// after Allow programs to read the clipboard, the next switches).
    fn press_stands(&self, at: &PressedAt) -> bool {
        !at.scrolled
            && self.reveal_as == Reveal::Pressed
            && self
                .page_reveal
                .as_ref()
                .is_some_and(|reveal| at.followed_by(reveal))
    }

    /// The key the control the press holding the list found is drawn under on the screen last
    /// seated ([`Self::reveal_pressed`], [`PressedAt::key_in`]).
    pub(super) fn pressed_key(&self) -> Option<UiKey> {
        let held = self.pressed_at.borrow();
        let at = held.as_ref()?;
        let record = self.scroll_record.borrow();
        Some(
            record
                .as_ref()
                .map_or_else(|| at.key.clone(), |record| at.key_in(record.place().0)),
        )
    }

    /// Where a render put the bar of the status a press brought ([`BarSide`]), whether bare of
    /// its Undo, and whether it weighed it for the status standing now.
    pub(super) fn press_side(&self) -> Option<(BarSide, bool, bool)> {
        let status = self.bar_feedback();
        self.pressed_at.borrow().as_ref().and_then(|at| {
            at.side
                .as_ref()
                .map(|(side, bare, weighed)| (*side, *bare, weighed.as_ref() == status))
        })
    }

    /// Put the bar of the status standing now, which a press brought, at `side`, `bare` of its
    /// Undo or not.
    pub(super) fn settle_press_side(&self, side: BarSide, bare: bool) {
        let status = self.bar_feedback().cloned();
        if let Some(at) = self.pressed_at.borrow_mut().as_mut() {
            at.side = Some((side, bare, status));
        }
    }

    /// The seat that holds the control a press found where the press found it, where `top` (with
    /// `plan`'s seat) draws it elsewhere ([`ScrollPlan::seat_holding`]): while the press holds and
    /// nothing has scrolled the list since.
    fn press_hold(&self, plan: &ScrollPlan, top: usize) -> Option<(usize, Option<Seat>)> {
        self.pressed_bottom()?;
        let (key, at) = self
            .pressed_at
            .borrow()
            .as_ref()
            .filter(|at| self.press_stands(at))
            .map(|at| (at.key_in(plan), at.top))?;
        let key = &key;
        let index = plan.stop_holding(key).or_else(|| plan.stop_of(key))?;
        let span = plan.control_span(index, key)?;
        let screen = plan.screen(top.min(plan.limit()));
        let now = plan
            .live_band(&screen, index)
            .map(|(then, _, _)| then + span.0);
        if now.is_some_and(|now| (now - at).abs() < 0.5) {
            return None;
        }
        plan.seat_holding(index, top, span, at)
    }

    /// Whether the screen last seated, its list starting `list_top` down the window, moved the
    /// control a press found, or does not show it whole in the room the list has
    /// ([`Self::press_band`]).
    pub(super) fn press_moved(&self, list_top: Option<f32>) -> bool {
        let held = self.pressed_at.borrow().clone();
        held.is_some_and(|at| {
            let key = self.pressed_key().unwrap_or_else(|| at.key.clone());
            at.list_top
                .zip(list_top)
                .is_some_and(|(was, now)| (was - now).abs() > 0.5)
                || self
                    .drawn_span(&key)
                    .is_none_or(|(_, bottom)| (bottom - at.bottom).abs() > 0.5)
        })
    }

    /// Handle a pointer press on a control preview picture in the list strip.
    pub(super) fn press_list_picture(&mut self, key: UiKey) -> bool {
        // A tile an open picker draws in its fading peek scrolls into the picker's window and
        // chooses nothing, as a control in the page list's peek does.
        if let Some(picker) = self.choice_picker.as_ref()
            && let Some(position) = picker.focus_position(&key)
        {
            picker.scroll_to(Some(position));
            return true;
        }
        let listed = self
            .scroll_plan()
            .is_some_and(|plan| plan.stop_holding(&key).is_some());
        if listed {
            self.reveal_pressed(key);
            return true;
        }
        if key.as_str().starts_with("settings/nav/") {
            self.common.last_focus = Some(key);
            return true;
        }
        false
    }

    /// A press however it came keeps its control in view where it left the keyboard (`kept`).
    pub(super) fn reveal_press(&mut self, key: &UiKey, before: Option<&UiKey>, kept: bool) {
        if self.page_reveal.as_ref() != before {
            return;
        }
        let context = self.scroll_context();
        let listed = self
            .scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.context == context)
            .is_some_and(|record| record.plan.stop_of(key).is_some());
        if listed && kept {
            self.reveal_pressed(key.clone());
        } else if listed && self.landing_owed() {
            // A press moving the keyboard ends the landing: R3 owes it only before the first event.
            self.page_reveal = None;
        }
    }

    /// Open a settings page scrolled and focused directly on control `key`.
    pub(super) fn land_on_control(&mut self, key: UiKey) {
        self.page_reveal = Some(key);
        self.reveal_as = Reveal::Land;
    }

    /// Open a settings page with control `key`'s stop selected.
    pub(super) fn land_on_stop(&mut self, key: UiKey) {
        self.page_reveal = Some(key);
        self.reveal_as = Reveal::LandWhole;
    }

    /// Opens a page at the top of its scroll list.
    pub(super) fn land_at_top(&mut self) {
        self.drop_list_nudge();
        self.page_scroll = 0;
        self.page_reveal = None;
        self.reveal_as = Reveal::Keep;
    }

    /// Forget the list's scroll and record, landing on the saved choice where the page has one.
    pub(super) fn reset_page_scroll(&mut self) {
        self.page_scroll = 0;
        let lands = self.search.trim().is_empty() && !self.compact_navigation;
        self.page_reveal = match self.route {
            SettingsRoute::CursorStyles if lands => Some(UiKey::new(super::cursor_style_row_key(
                &super::saved_cursor_style(self),
            ))),
            SettingsRoute::Themes if lands => self.showcase_landing(),
            _ => None,
        };
        self.reveal_as = if self.page_reveal.is_some() {
            Reveal::Land
        } else {
            Reveal::Keep
        };
        self.scroll_record.replace(None);
    }

    /// Steps keyboard focus up or down through the page.
    pub(super) fn step_list_focus(&mut self, lines: i32) -> bool {
        let Some(plan) = self.scroll_plan() else {
            return false;
        };
        let Some(current) = self.common.last_focus.clone() else {
            return false;
        };
        let controls: Vec<&UiKey> = plan.controls().map(|(_, key)| key).collect();
        let Some(index) = plan.control_index(&current) else {
            return false;
        };
        let target = if lines < 0 {
            index.checked_sub(lines.unsigned_abs() as usize)
        } else {
            index
                .checked_add(lines as usize)
                .filter(|target| *target < controls.len())
        };
        let Some(target) = target else {
            return false;
        };
        self.land_list_focus(controls[target].clone());
        true
    }

    /// Whether the stop holding the control the list keeps in view ([`Self::page_reveal`])
    /// fits the list's room.
    pub(super) fn revealed_stop_fits(&self) -> bool {
        self.scroll_plan()
            .is_none_or(|plan| self.revealed_stop_fits_in(&plan))
    }

    /// Whether that stop fits the room of `plan`, a list's plan told unseated.
    pub(super) fn revealed_stop_fits_in(&self, plan: &ScrollPlan) -> bool {
        self.page_reveal.as_ref().is_none_or(|key| {
            plan.stop_holding(key)
                .or_else(|| plan.stop_of(key))
                .is_none_or(|stop| plan.drawn_span(stop..stop + 1) <= plan.room() + 0.01)
        })
    }

    /// Moves keyboard focus to a control visible on the current page.
    pub(super) fn keep_list_focus_on_screen(&mut self) {
        let Some(plan) = self.scroll_plan() else {
            return;
        };
        let Some(stop) = self
            .common
            .last_focus
            .as_ref()
            .and_then(|focus| plan.stop_of(focus))
        else {
            return;
        };
        if plan.shows_landing(stop, self.page_scroll.min(plan.limit())) {
            return;
        }
        self.focus_list_screen();
    }

    /// Move the keyboard to the first control the list's screen shows, ringed.
    pub(super) fn focus_list_screen(&mut self) {
        let Some(plan) = self.scroll_plan() else {
            return;
        };
        let top = self.page_scroll.min(plan.limit());
        let landing = plan
            .controls()
            .find(|(stop, _)| plan.shows_landing(*stop, top))
            .map(|(_, key)| key.clone());
        if let Some(key) = landing {
            self.land_list_focus(key);
        } else if let Some(key) = plan
            .controls()
            .find_map(|(stop, key)| plan.live_control(stop, key, top))
            .cloned()
        {
            self.land_list_focus(key.clone());
            self.page_reveal = Some(key);
            self.reveal_as = Reveal::Hold;
        }
    }

    /// Records how far the status bar must shorten so it covers neither the active control nor,
    /// where both fit, the landing beside it ([`ListFoot::Whole`]), and a list seated under it.
    pub(super) fn note_status_room(&self, plan: &ScrollPlan, room: f32) {
        self.status_lists_seated
            .set(self.status_lists_seated.get() + 1);
        if self.measuring_calm.get() {
            return;
        }
        let record = self.scroll_record.borrow();
        let overrun = self.status_overrun_of(plan, room, self.status_slack.get(), record.as_ref());
        drop(record);
        if overrun > 0.01 {
            self.status_overrun
                .set(self.status_overrun.get().max(overrun));
        }
    }

    /// How far a status's bar must shorten over `plan`, seated in `room` with `slack` more the bar
    /// could give back ([`Self::note_status_room`]).
    fn status_overrun_of(
        &self,
        plan: &ScrollPlan,
        room: f32,
        slack: f32,
        record: Option<&ScrollRecord>,
    ) -> f32 {
        let (given, room) = (plan.given + slack, room + slack);
        if given <= 0.01 {
            return 0.0;
        }
        // A keyboard's control taller than the room takes the bar to its
        // least: Tokyo Night's 92pt row at 178x348 at 2× text under the
        // warning and a status showed 0pt, at the bar's least 64pt.
        let note = |need: f32, keyboard: bool| {
            if need <= room + 0.01 {
                given - (room - need)
            } else if keyboard {
                given
            } else {
                0.0
            }
        };
        let focus = self.common.last_focus.as_ref();
        let reveal = self
            .beside_given_back(plan, record)
            .unwrap_or(self.page_reveal.as_ref());
        let spans: Vec<(usize, (f32, f32))> = [reveal, focus]
            .into_iter()
            .flatten()
            .filter_map(|key| {
                let index = plan.stop_holding(key).or_else(|| plan.stop_of(key))?;
                Some((index, plan.span_of(index, Some(key), Reveal::Pressed)))
            })
            .collect();
        let keyboard = focus.and_then(|key| plan.stop_holding(key).or_else(|| plan.stop_of(key)));
        let mut overrun = 0.0_f32;
        for &(index, (top, bottom)) in &spans {
            let seated = (plan.foot == ListFoot::Whole)
                .then(|| plan.stop_top(index.min(plan.limit()), index) + bottom)
                .filter(|need| *need <= room + 0.01);
            overrun = overrun.max(note(
                seated.unwrap_or(bottom - top),
                keyboard == Some(index),
            ));
        }
        if let [first, second] = spans[..]
            && first.0 != second.0
        {
            let ((upper, (from, _)), (lower, (_, to))) = if first.0 < second.0 {
                (first, second)
            } else {
                (second, first)
            };
            overrun = overrun.max(note(plan.stop_top(upper, lower) + to - from, false));
        }
        // A CONTROL THE PERSON PRESSED STAYS WHERE IT WAS PRESSED: the bar shortens, down to
        // its least, until the room under it still holds the control where the screen last
        // drawn showed it; the rest of the status's words scroll in the bar. It grew over the
        // control instead and the list moved the control up from under the pointer by what it
        // covered (Software Update at 178x348: 73pt).
        if let Some(bottom) = self.pressed_bottom() {
            overrun = overrun.max(note(bottom, false));
        }
        overrun
    }

    /// Where the control a press revealed ended on the screen it was pressed on
    /// ([`Self::reveal_pressed`]): while the press holds, and after, for as long as the status
    /// that came with it stands on that list, so the bar keeps the room it took when it came.
    /// Let go when a scroll ended the press, the bar grew back over the list to show every word
    /// (Software Update at 178x348: from 98pt to 186pt of the page's 268pt).
    ///
    /// THE PRESS'S STATUS IS ITS WRITE'S, PENDING AND LANDED: a status that came while a write
    /// was in flight ("Applying…") gives way to the write's outcome as the same press's. Let go
    /// there, the outcome's bar grew back over the list in full and moved the switch the person
    /// had just pressed (AI at 164x348 with 2x text: 264pt, to 184pt under "Applying…", then to
    /// 120pt under "Look for AI CLI problems turned off").
    fn pressed_bottom(&self) -> Option<f32> {
        let mut held = self.pressed_at.borrow_mut();
        let at = held.as_mut()?;
        let status = self.bar_feedback();
        let landed = at.awaits && status.is_some();
        let gone = at.context != self.scroll_context()
            || match (&at.status, status) {
                (Some(came), now) => now != Some(came) && !landed,
                (None, _) => false,
            };
        if gone {
            *held = None;
            return None;
        }
        if (at.status.is_none() && status != at.before.as_ref())
            || (landed && status != at.status.as_ref())
        {
            at.status = status.cloned();
            at.awaits = !self.pending.is_empty();
        }
        let pressing =
            self.reveal_as == Reveal::Pressed && self.page_reveal.as_ref() == Some(&at.key);
        (pressing || at.status.is_some()).then_some(at.bottom)
    }

    /// The record of the screen `plan` ([`ListLaid::planned`]) would leave seated, `record` the
    /// screen before, told with no page built; the state is left as it was.
    pub(super) fn seated_under(
        &self,
        mut plan: ScrollPlan,
        record: Option<ScrollRecord>,
    ) -> Option<ScrollRecord> {
        let was = (
            self.scroll_record.replace(record),
            self.page_window.get(),
            self.result_page_limit.get(),
        );
        let _ = self.seat_scroll(&mut plan);
        let told = self.scroll_record.replace(was.0);
        self.page_window.set(was.1);
        self.result_page_limit.set(was.2);
        told
    }

    /// How tall the keyboard's listed control is among `laid`'s rows, seated in a list `room` tall.
    pub(super) fn keyboard_box(&self, laid: &ListLaid, room: f32) -> Option<f32> {
        let key = self.common.last_focus.as_ref()?;
        let fitted = fit_rows(&laid.items, room);
        let (plan, _) = plan_under_status(
            &fitted,
            laid.gap,
            room,
            0.0,
            Overhang::NONE,
            ListFoot::Whole,
        );
        let index = plan.stop_holding(key).or_else(|| plan.stop_of(key))?;
        plan.control_span(index, key)
            .map(|(top, bottom)| bottom - top)
    }

    /// What `read` says of the state with `record` the screen last seated; the state is left as
    /// it was.
    pub(super) fn reading<R>(
        &self,
        record: Option<ScrollRecord>,
        read: impl FnOnce(&Self) -> R,
    ) -> R {
        let was = self.scroll_record.replace(record);
        let said = read(self);
        self.scroll_record.replace(was);
        said
    }

    /// How far a status's bar over a list `room` tall, with `slack` more it could give back, must
    /// shorten: `plan` ([`ListLaid::planned`]) seated there, with no page built.
    pub(super) fn overrun_under(
        &self,
        plan: &ScrollPlan,
        room: f32,
        slack: f32,
        record: Option<&ScrollRecord>,
    ) -> f32 {
        self.status_overrun_of(plan, room, slack, record)
    }

    /// A status never covers a control pinned around the list either: the bar shortens for the
    /// keyboard's control, or the one a reveal waits to show, among `pinned`'s, where it would
    /// come more than `clear` over the foot of the page laid out under it
    /// ([`Self::status_extra`]).
    pub(super) fn note_pinned_status_room(&self, pinned: &UiNode, clear: f32) {
        let given = self.status_extra.get();
        if given <= 0.01 || self.measuring_calm.get() {
            return;
        }
        fn holds(node: &UiNode, key: &UiKey) -> bool {
            node.key == *key || node.children.iter().any(|child| holds(child, key))
        }
        let overrun = given - clear.max(0.0);
        if overrun > 0.01
            && [self.page_reveal.as_ref(), self.common.last_focus.as_ref()]
                .into_iter()
                .flatten()
                .any(|key| holds(pinned, key))
        {
            self.status_overrun
                .set(self.status_overrun.get().max(overrun));
        }
    }

    /// Home and End take the keyboard to the list's ends.
    pub(super) fn focus_list_edge(&mut self, first: bool) {
        let Some(plan) = self.scroll_plan() else {
            return;
        };
        if !self
            .common
            .last_focus
            .as_ref()
            .is_some_and(|focus| plan.stop_of(focus).is_some())
        {
            return;
        }
        let mut controls = plan.controls().map(|(_, key)| key);
        let target = if first {
            controls.next()
        } else {
            controls.last()
        };
        if let Some(key) = target.cloned() {
            self.land_list_focus(key);
        }
    }

    fn land_list_focus(&mut self, key: UiKey) {
        self.clear_field_edit();
        self.list_carried = Some(key.clone());
        self.common.last_focus = Some(key.clone());
        self.common.focus_visible = true;
        self.reveal_control(key);
    }

    /// The sidebar is one tab stop (the macOS sidebar convention).
    pub(crate) fn tab_past_navigation(
        &self,
        current: Option<&UiKey>,
        next: &UiKey,
        forward: bool,
        order: &[UiKey],
    ) -> Option<UiKey> {
        let in_rail = |key: &UiKey| key.as_str().starts_with("settings/nav/");
        if !in_rail(next) {
            return None;
        }
        if current.is_some_and(in_rail) {
            let target = if forward {
                let last = order.iter().rposition(in_rail)?;
                order.get(last + 1).or_else(|| order.first())
            } else {
                let first = order.iter().position(in_rail)?;
                first
                    .checked_sub(1)
                    .and_then(|index| order.get(index))
                    .or_else(|| order.last())
            }?;
            return (!in_rail(target)).then(|| target.clone());
        }
        let selected = UiKey::new(format!("settings/nav{}", self.route.path()));
        (order.contains(&selected) && selected != *next).then_some(selected)
    }

    /// Tab enters a list whose screen shows none of its controls
    /// ([`crate::native_ui::tab_into_unshown_list`] over this page's list).
    pub(crate) fn tab_into_unshown_list(&self, forward: bool, order: &[UiKey]) -> Option<UiKey> {
        let plan = self.scroll_plan()?;
        let controls: Vec<&UiKey> = plan.controls().map(|(_, key)| key).collect();
        crate::native_ui::tab_into_unshown_list(&controls, forward, order).cloned()
    }

    /// Tab walks the whole list, not just its screen ([`crate::native_ui::tab_past_screen`]
    /// over this page's list).
    pub(crate) fn tab_past_screen(
        &self,
        current: &UiKey,
        next: &UiKey,
        forward: bool,
        order: &[UiKey],
    ) -> Option<UiKey> {
        let plan = self.scroll_plan()?;
        let controls: Vec<&UiKey> = plan.controls().map(|(_, key)| key).collect();
        if let Some(step) =
            self.tab_by_bar_over_list(&plan, &controls, (current, next), forward, order)
        {
            return step;
        }
        crate::native_ui::tab_past_screen(
            &controls,
            |key| plan.control_index(key),
            current,
            next,
            forward,
            order,
        )
    }

    /// A STATUS'S BAR OVER THE TOP OF THE LIST TAKES ITS PLACE IN TAB ORDER WHERE IT STANDS ON THE
    /// SCREEN, between the rows it covers and the first under it: Shift-Tab from the first control
    /// under it reads its stops (its Undo, its words where they scroll), Tab from them goes on to
    /// that control, and Shift-Tab from its first stop to the row it covers before it. Taken as
    /// the list's edge, Shift-Tab from the switch just pressed went to the row hidden under the
    /// bar, which sent the bar back under the list and moved everything, and Tab from the bar
    /// went to the list's first row. `Some(None)` keeps `next`; `None` where no bar stands over
    /// the list or the step does not pass it.
    fn tab_by_bar_over_list(
        &self,
        plan: &ScrollPlan,
        controls: &[&UiKey],
        (current, next): (&UiKey, &UiKey),
        forward: bool,
        order: &[UiKey],
    ) -> Option<Option<UiKey>> {
        if self.status_place.get() != super::StatusPlace::Over {
            return None;
        }
        let listed = |key: &UiKey| plan.control_index(key).is_some();
        match (
            super::status_bar_stop(current),
            super::status_bar_stop(next),
        ) {
            (false, true) if !forward && listed(current) => Some(None),
            (true, false) if forward && listed(next) => Some(None),
            (true, false) if !forward => {
                let under = plan.control_index(order.iter().find(|key| listed(key))?)?;
                let covered = controls.get(under.checked_sub(1)?)?;
                Some(Some((*covered).clone()))
            }
            _ => None,
        }
    }
}

fn holds_control(node: &UiNode) -> bool {
    !node.paint_only && (is_control(node) || node.children.iter().any(holds_control))
}

fn notes_rows_over(node: &UiNode) -> bool {
    fn lines(node: &UiNode, status: &mut usize) -> bool {
        match &node.content {
            UiContent::Text(text) => {
                *status += 1;
                text.role == SemanticRole::Status
            }
            _ => node.children.iter().all(|child| lines(child, status)),
        }
    }
    let mut status = 0;
    !holds_control(node) && lines(node, &mut status) && status > 0
}

fn extent_where(
    node: &UiNode,
    height: f32,
    counts: &dyn Fn(&UiNode) -> Option<bool>,
) -> Option<(f32, f32)> {
    match counts(node) {
        Some(true) => Some((0.0, height)),
        Some(false) => None,
        None => children_extent(node, height, counts),
    }
}

fn children_extent(
    node: &UiNode,
    height: f32,
    counts: &dyn Fn(&UiNode) -> Option<bool>,
) -> Option<(f32, f32)> {
    let layout = &node.layout;
    let inner = (height - layout.padding.top - layout.padding.bottom).max(0.0);
    let length = |length: Length, available: f32| match length {
        Length::Fill => None,
        Length::Fixed(value) => Some(value),
        Length::Fraction(value) => Some(available * value),
    };
    let mut extent: Option<(f32, f32)> = None;
    let mut add = |top: f32, height: f32| {
        extent = Some(match extent {
            None => (top, height),
            Some((was, tall)) => {
                let start = was.min(top);
                (start, (was + tall).max(top + height) - start)
            }
        });
    };
    if layout.flow == Flow::Column {
        let gaps = layout.gap * node.children.len().saturating_sub(1) as f32;
        let available = (inner - gaps).max(0.0);
        let fixed: f32 = node
            .children
            .iter()
            .filter_map(|child| length(child.layout.height, available))
            .sum();
        let fills = node
            .children
            .iter()
            .filter(|child| child.layout.height == Length::Fill)
            .count();
        let fill = if fills == 0 {
            0.0
        } else {
            (available - fixed).max(0.0) / fills as f32
        };
        let mut y = layout.padding.top - layout.offset;
        for child in &node.children {
            let tall = length(child.layout.height, available)
                .unwrap_or(fill)
                .max(0.0);
            if let Some((top, height)) = extent_where(child, tall, counts) {
                add(y + top, height);
            }
            y += tall + layout.gap;
        }
    } else {
        for child in &node.children {
            let tall = length(child.layout.height, inner)
                .map_or(inner, |value| value.min(inner))
                .max(0.0);
            if let Some((top, height)) = extent_where(child, tall, counts) {
                add(layout.padding.top + top, height);
            }
        }
    }
    extent
}

const fn is_control(node: &UiNode) -> bool {
    match &node.content {
        UiContent::Button(_)
        | UiContent::Switch(_)
        | UiContent::Slider(_)
        | UiContent::TextField(_)
        | UiContent::ColorWheel(_) => true,
        UiContent::Custom(spec) => spec.focusable,
        _ => false,
    }
}

fn control_extent(node: &UiNode, height: f32) -> Option<(f32, f32)> {
    extent_where(node, height, &|node| {
        if node.paint_only {
            Some(false)
        } else if is_control(node) {
            Some(true)
        } else {
            None
        }
    })
}

fn keys_within(node: &UiNode, out: &mut Vec<UiKey>) {
    for child in &node.children {
        out.push(child.key.clone());
        keys_within(child, out);
    }
}

/// A band a stop draws, from its top ([`drawn_bands_in`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DrawnPart {
    pub(super) top: f32,
    pub(super) bottom: f32,
    /// Drawn only whole where an edge crosses it: words, and a control, which is drawn cut only
    /// with its name whole beside it.
    pub(super) whole: bool,
}

/// What `node`, laid out `height` tall, draws, band by band from its top: each line of words
/// and each control, drawn only whole where a list's edge crosses it, and each picture (a
/// preview, a swatch), drawn as far as it shows. A line reserved empty draws nothing.
pub(super) fn drawn_bands_in(node: &UiNode, height: f32) -> Vec<DrawnPart> {
    fn walk(node: &UiNode, top: f32, height: f32, out: &mut Vec<DrawnPart>) {
        if height <= 0.01 {
            return;
        }
        let part = |whole: bool| DrawnPart {
            top,
            bottom: top + height,
            whole,
        };
        match &node.content {
            UiContent::Group(_) => {}
            UiContent::Text(spec) => {
                // Its line, centred in its box, is what an edge cuts.
                if !spec.text.trim().is_empty() {
                    let (px, _) = crate::native_ui::text_paint_metrics(spec.role, spec.style);
                    let line = (px * 1.25).min(height);
                    let at = top + (height - line) / 2.0;
                    out.push(DrawnPart {
                        top: at,
                        bottom: at + line,
                        whole: true,
                    });
                }
                return;
            }
            UiContent::RichText(_) => {
                out.push(part(true));
                return;
            }
            _ => {
                out.push(part(is_control(node)));
                return;
            }
        }
        let layout = &node.layout;
        let inner = (height - layout.padding.top - layout.padding.bottom).max(0.0);
        let length = |length: Length, available: f32| match length {
            Length::Fill => None,
            Length::Fixed(value) => Some(value),
            Length::Fraction(value) => Some(available * value),
        };
        if layout.flow == Flow::Column {
            let gaps = layout.gap * node.children.len().saturating_sub(1) as f32;
            let available = (inner - gaps).max(0.0);
            let fixed: f32 = node
                .children
                .iter()
                .filter_map(|child| length(child.layout.height, available))
                .sum();
            let fills = node
                .children
                .iter()
                .filter(|child| child.layout.height == Length::Fill)
                .count();
            let fill = if fills == 0 {
                0.0
            } else {
                (available - fixed).max(0.0) / fills as f32
            };
            let mut y = top + layout.padding.top - layout.offset;
            for child in &node.children {
                let tall = length(child.layout.height, available)
                    .unwrap_or(fill)
                    .max(0.0);
                walk(child, y, tall, out);
                y += tall + layout.gap;
            }
        } else {
            for child in &node.children {
                let tall = length(child.layout.height, inner)
                    .map_or(inner, |value| value.min(inner))
                    .max(0.0);
                walk(child, top + layout.padding.top, tall, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(node, 0.0, height, &mut out);
    out
}

/// Each control under `node`, with `node` laid out `height` tall: its key, top and height; and
/// the words naming the keyboard's control there ([`KeyboardWords`]).
pub(super) fn control_spans_in(node: &UiNode, height: f32) -> ControlSpans {
    fn walk<'a>(
        node: &'a UiNode,
        top: f32,
        height: f32,
        out: &mut ControlSpans,
        rows: &mut Vec<(&'a UiNode, f32, f32)>,
    ) {
        if node.paint_only {
            return;
        }
        if is_control(node) {
            out.0.push((node.key.clone(), top, height));
            if out.1.is_none() {
                out.1 = KeyboardWords::of_node(node, top, height, rows);
            }
        }
        rows.push((node, top, height));
        let layout = &node.layout;
        let inner = (height - layout.padding.top - layout.padding.bottom).max(0.0);
        let length = |length: Length, available: f32| match length {
            Length::Fill => None,
            Length::Fixed(value) => Some(value),
            Length::Fraction(value) => Some(available * value),
        };
        if layout.flow == Flow::Column {
            let gaps = layout.gap * node.children.len().saturating_sub(1) as f32;
            let available = (inner - gaps).max(0.0);
            let fixed: f32 = node
                .children
                .iter()
                .filter_map(|child| length(child.layout.height, available))
                .sum();
            let fills = node
                .children
                .iter()
                .filter(|child| child.layout.height == Length::Fill)
                .count();
            let fill = if fills == 0 {
                0.0
            } else {
                (available - fixed).max(0.0) / fills as f32
            };
            let mut y = top + layout.padding.top - layout.offset;
            for child in &node.children {
                let tall = length(child.layout.height, available)
                    .unwrap_or(fill)
                    .max(0.0);
                walk(child, y, tall, out, rows);
                y += tall + layout.gap;
            }
        } else {
            for child in &node.children {
                let tall = length(child.layout.height, inner)
                    .map_or(inner, |value| value.min(inner))
                    .max(0.0);
                walk(child, top + layout.padding.top, tall, out, rows);
            }
        }
        rows.pop();
    }
    let mut out = (Vec::new(), None);
    walk(node, 0.0, height, &mut out, &mut Vec::new());
    out
}

/// Where each control a stop holds sits in it, and the words naming the keyboard's there.
pub(super) type ControlSpans = (Vec<(UiKey, f32, f32)>, Option<(UiKey, Words)>);

/// Positions of controls within stop `node` of `height` ([`ScrollPlan::set_control_rows`]).
pub(super) fn control_rows_in(node: &UiNode, height: f32) -> Vec<(f32, f32, f32)> {
    match column_rows(node, height) {
        Some(rows) => rows
            .into_iter()
            .filter_map(|(top, tall, row)| {
                control_extent(row, tall).map(|(at, controls)| (top, top + at, controls))
            })
            .collect(),
        None => control_extent(node, height)
            .map(|(top, tall)| (0.0, top, tall))
            .into_iter()
            .collect(),
    }
}

/// Where the first row of controls under `node`, laid out `height` tall, sits in it.
pub(super) fn first_control_row(node: &UiNode, height: f32) -> Option<(f32, f32)> {
    if node.paint_only {
        return None;
    }
    if is_control(node) {
        return Some((0.0, height));
    }
    match column_rows(node, height) {
        Some(rows) => rows.into_iter().find_map(|(top, tall, row)| {
            first_control_row(row, tall).map(|(at, controls)| (top + at, controls))
        }),
        None => control_extent(node, height),
    }
}

fn column_rows(node: &UiNode, height: f32) -> Option<Vec<(f32, f32, &UiNode)>> {
    if !matches!(node.content, UiContent::Group(_)) || node.layout.flow != Flow::Column {
        return None;
    }
    let layout = &node.layout;
    let inner = (height - layout.padding.top - layout.padding.bottom).max(0.0);
    let gaps = layout.gap * node.children.len().saturating_sub(1) as f32;
    let available = (inner - gaps).max(0.0);
    let length = |length: Length| match length {
        Length::Fill => None,
        Length::Fixed(value) => Some(value),
        Length::Fraction(value) => Some(available * value),
    };
    let fixed: f32 = node
        .children
        .iter()
        .filter_map(|child| length(child.layout.height))
        .sum();
    let fills = node
        .children
        .iter()
        .filter(|child| child.layout.height == Length::Fill)
        .count();
    let fill = if fills == 0 {
        0.0
    } else {
        (available - fixed).max(0.0) / fills as f32
    };
    let mut y = layout.padding.top - layout.offset;
    let mut rows = Vec::with_capacity(node.children.len());
    for child in &node.children {
        let tall = length(child.layout.height).unwrap_or(fill).max(0.0);
        rows.push((y, tall, child));
        y += tall + layout.gap;
    }
    Some(rows)
}

/// The key a segmented control is listed by ([`UiNode::segmented`]).
pub(super) fn same_control(drawn: &UiKey, key: &UiKey) -> bool {
    drawn == key
        || drawn
            .as_str()
            .strip_prefix("settings/segment/")
            .and_then(|segment| segment.rsplit_once('/'))
            .is_some_and(|(setting, _)| key.as_str() == format!("settings/control/{setting}"))
}

pub(super) fn segmented_control_key(node: &UiNode) -> Option<UiKey> {
    let segments = crate::native_ui::segments_of(&node.children);
    segments
        .iter()
        .find(|(_, state)| state.selected)
        .or_else(|| {
            segments
                .iter()
                .find(|(_, state)| state.enabled || state.busy)
        })
        .map(|(key, _)| key.clone())
}

fn paints_whole(node: &UiNode) -> Option<bool> {
    match &node.content {
        UiContent::Group(spec) if !spec.paints_backdrop() => None,
        _ => Some(true),
    }
}

/// Where what `node` draws sits in it, laid out `height` tall as the compiler lays it out.
pub(super) fn ink_extent(node: &UiNode, height: f32) -> Option<(f32, f32)> {
    extent_where(node, height, &paints_whole)
}

fn content_extent(node: &UiNode, height: f32) -> Option<(f32, f32)> {
    match &node.content {
        UiContent::Group(_) => children_extent(node, height, &paints_whole),
        _ => Some((0.0, height)),
    }
}

fn caption_extent(node: &UiNode) -> Option<CardCaption> {
    let UiContent::Group(_) = &node.content else {
        return None;
    };
    if node.layout.flow != Flow::Column || node.children.len() < 2 {
        return None;
    }
    let first = node.children.first()?;
    let Length::Fixed(height) = first.layout.height else {
        return None;
    };
    is_heading(first).then(|| {
        let top = node.layout.padding.top - node.layout.offset;
        let rest = &node.children[1..];
        CardCaption {
            top,
            bottom: top + height,
            lead: top + height + node.layout.gap,
            rows: rest
                .iter()
                .map(|child| match child.layout.height {
                    Length::Fixed(height) => height,
                    _ => 0.0,
                })
                .sum::<f32>()
                + rest.len().saturating_sub(1) as f32 * node.layout.gap,
        }
    })
}

/// Every control under `node` a person can focus (or will, once it is done saving), in authoring
/// order.
pub(super) fn focus_keys(node: &UiNode) -> Vec<UiKey> {
    fn walk(node: &UiNode, out: &mut Vec<UiKey>) {
        if node.paint_only {
            return;
        }
        if node.segmented {
            out.extend(segmented_control_key(node));
            return;
        }
        // What the page's Tab order takes ([`UiContent::focusable`]), and a control busy saving,
        // which takes the keyboard again once it is done.
        let busy = match &node.content {
            UiContent::Button(control) => control.state.busy,
            UiContent::Switch(control) => control.state.busy,
            UiContent::Slider(control) => control.state.busy,
            UiContent::TextField(control) => control.state.busy,
            UiContent::ColorWheel(control) => control.state.busy,
            _ => false,
        };
        let focusable = node.content.focusable() || busy;
        if focusable {
            out.push(node.key.clone());
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

/// Give `plan` its `room` and the strip under it a page keeps for a status bar ([`Overhang`]).
pub(super) fn seat_room(plan: &mut ScrollPlan, room: f32, overhang: Overhang) {
    let now = overhang.now.max(0.0);
    let n = plan.len();
    let fits = now > 0.0
        && plan.span(0..n) <= room + now + 0.01
        && (0..n).all(|stop| plan.whole_by(0, stop) <= room + 0.01);
    if fits {
        plan.set_room(room + now);
        plan.set_overhang(0.0);
    } else {
        plan.set_room(room);
        plan.set_overhang(now);
    }
}

/// The room a list `room` tall ending as `foot` groups stops for: under a status, the bar's rest.
pub(super) fn stops_room(state: &SettingsViewState, room: f32, foot: ListFoot) -> f32 {
    match foot {
        ListFoot::Whole => (room - state.status_extra.get()).max(0.0),
        ListFoot::Peek => room,
    }
}

/// `items` planned and given their `room` under a status's bar that takes `given` of it, as a
/// seat lays them out ([`seat_room_under_status`]).
fn plan_under_status(
    items: &[ListItem],
    gap: f32,
    room: f32,
    given: f32,
    overhang: Overhang,
    foot: ListFoot,
) -> (ScrollPlan, ItemStops) {
    let stops = match foot {
        ListFoot::Whole => (room - given).max(0.0),
        ListFoot::Peek => room,
    };
    let (mut plan, item_stops) = plan_items(items, gap, stops);
    plan.set_foot(foot);
    seat_room(&mut plan, (room - given).max(0.0), overhang);
    plan.give_to_status(given);
    (plan, item_stops)
}

/// [`seat_room`] for a list a plain status's bar may stand under.
pub(super) fn seat_room_under_status(
    state: &SettingsViewState,
    plan: &mut ScrollPlan,
    room: f32,
    overhang: Overhang,
) {
    let given = state.status_extra.get();
    seat_room(plan, (room - given).max(0.0), overhang);
    plan.give_to_status(given);
    state.note_status_room(plan, room);
}

/// The stops a screen whose shown stops are `view` draws.
pub(super) fn drawn_stops(plan: &ScrollPlan, view: &Range<usize>) -> Range<usize> {
    plan.screen(view.start).drawn
}

/// The list's frame for one screen ([`ScrollPlan::screen`]), its peek drawn.
pub(super) fn scroll_list_screen(
    key: &str,
    label: &str,
    plan: &ScrollPlan,
    view: Range<usize>,
    drawn: Range<usize>,
    children: Vec<UiNode>,
) -> UiNode {
    let screen = plan.screen(view.start);
    debug_assert_eq!(screen.view, view, "{key}: the screen it was seated on");
    debug_assert_eq!(screen.drawn, drawn, "{key}: the stops it draws");
    list_screen_node(key, label, plan, &screen, children)
}

fn list_screen_node(
    key: &str,
    label: &str,
    plan: &ScrollPlan,
    screen: &Screen,
    mut children: Vec<UiNode>,
) -> UiNode {
    let mut offset = screen.offset;
    if let (Some(caption), Some(first)) = (screen.caption, children.first_mut()) {
        if caption.kept && keep_caption(first, caption.lead, offset) {
            offset = 0.0;
        } else {
            hide_cut_caption(first, caption.lead, offset);
        }
    }
    if let (Some((edge, caption)), Some(last)) = (screen.edge_caption, children.last_mut()) {
        hide_cut_caption(last, caption, edge);
    }
    let list = UiNode::new(
        key,
        UiContent::Group(GroupSpec {
            label: Some(label.to_string()),
            role: SemanticRole::List,
            style: StyleRef::Plain,
        }),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fixed(screen.height))
            .gap(plan.gap())
            .scrolled(offset)
            .clipped(),
    )
    .room(screen.live.min(screen.height), screen.fades)
    .children(children);
    if screen.whole {
        return list;
    }
    let view = &screen.view;
    let filled = screen.fades
        || screen.drawn != screen.view
        || screen.offset > 0.0
        || plan.foot == ListFoot::Whole;
    let track = if filled {
        screen.height
    } else {
        plan.span(view.clone()).min(screen.height)
    };
    let thumb = UiNode::new(
        format!("{key}/thumb"),
        UiContent::Custom(AuditedCustomNode {
            audit_id: MESSAGES_SCROLL_AUDIT,
            role: SemanticRole::Group,
            label: String::new(),
            paint: Some(format!("{},{},{}", view.start, view.len(), plan.len())),
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
        }),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(track)),
    )
    .paint_only();
    UiNode::new(
        format!("{key}/scroll"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(screen.height)),
    )
    .children(vec![list, thumb])
}

fn keep_caption(card: &mut UiNode, caption: f32, offset: f32) -> bool {
    let gap = card.layout.gap;
    let mut y = card.layout.padding.top;
    let mut head = 0;
    for child in &card.children {
        let Length::Fixed(height) = child.layout.height else {
            break;
        };
        if y + height + gap > caption + 0.01 {
            break;
        }
        y += height + gap;
        head += 1;
    }
    let rows = card.children.split_off(head);
    let rows_height = rows
        .iter()
        .map(|row| match row.layout.height {
            Length::Fixed(height) => height,
            _ => 0.0,
        })
        .sum::<f32>()
        + rows.len().saturating_sub(1) as f32 * gap;
    if rows.is_empty() || offset >= rows_height {
        card.children.extend(rows);
        return false;
    }
    if let Length::Fixed(height) = card.layout.height {
        card.layout.height = Length::Fixed((height - offset).max(0.0));
    }
    let scrolled = UiNode::new(
        format!("{}/scrolled-rows", card.key.as_str()),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fixed(rows_height - offset))
            .gap(gap)
            .scrolled(offset)
            .clipped(),
    )
    .under_caption(rows_height - offset)
    .children(rows)
    .paint_only();
    card.children.push(scrolled);
    true
}

fn hide_cut_caption(card: &mut UiNode, caption: f32, offset: f32) {
    let gap = card.layout.gap;
    let mut y = card.layout.padding.top;
    for child in &mut card.children {
        let Length::Fixed(height) = child.layout.height else {
            break;
        };
        if y + height > caption + 0.01 {
            break;
        }
        if y < offset - 0.01 && offset < y + height - 0.01 {
            child.content = UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group));
            child.children.clear();
        }
        y += height + gap;
    }
}

/// Seat a page's own scrolling list in the page's right gutter.
pub(super) fn seat_page_scroller(children: &mut [UiNode], gutter: f32) {
    if gutter <= 0.0 {
        return;
    }
    for group in children {
        let Some(base) = group.key.as_str().strip_suffix("/scroll") else {
            continue;
        };
        let base = base.to_string();
        let [list, thumb] = group.children.as_mut_slice() else {
            continue;
        };
        let is_list = list.key.as_str() == base
            && matches!(&list.content, UiContent::Group(spec)
                if spec.role == SemanticRole::List && spec.style == StyleRef::Plain);
        let UiContent::Custom(spec) = &mut thumb.content else {
            continue;
        };
        if !is_list
            || thumb.key.as_str() != format!("{base}/thumb")
            || spec.audit_id != MESSAGES_SCROLL_AUDIT
        {
            continue;
        }
        spec.audit_id = PAGE_SCROLL_AUDIT;
        list.layout.padding.right += gutter;
        group.layout = group.layout.outdent(Insets {
            right: gutter,
            ..Insets::default()
        });
    }
}

/// THE TOOLBAR A PAGE'S LIST LEADS WITH SPANS THE WINDOW, as it spans it pinned over the page:
/// a page child whose first child, down to the toolbar, is the toolbar reaches the window's
/// edges past the page's `insets`, what it holds kept where it was by as much padding, and its
/// first child reaches through that padding to its edges, down to the toolbar. Listed in the
/// page's column, the toolbar stood inset by the page's margins, 14pt a side at 178pt.
pub(super) fn span_listed_toolbar(children: &mut [UiNode], insets: Insets) {
    for child in children
        .iter_mut()
        .filter(|child| leads_with_toolbar(child))
    {
        let left = (insets.left - child.layout.outdent.left).max(0.0);
        let right = (insets.right - child.layout.outdent.right).max(0.0);
        bleed(child, left, right);
    }
}

/// Whether `node` is the compact toolbar, as a list lists it, or leads with it by its first
/// children.
fn leads_with_toolbar(node: &UiNode) -> bool {
    is_listed_toolbar(node) || node.children.first().is_some_and(leads_with_toolbar)
}

fn is_listed_toolbar(node: &UiNode) -> bool {
    let key = node.key.as_str();
    key == super::COMPACT_TOOLBAR_KEY
        || key
            .strip_prefix(super::COMPACT_TOOLBAR_KEY)
            .is_some_and(|rest| rest == "/stop")
}

/// Reach `left` and `right` past `node`'s parent's content, keeping what it holds in place, and
/// its first child to its edges, down to the toolbar ([`span_listed_toolbar`]).
fn bleed(node: &mut UiNode, left: f32, right: f32) {
    node.layout.outdent.left += left;
    node.layout.outdent.right += right;
    if is_listed_toolbar(node) {
        // The whole of what holds it, whatever width its rows keep (the chooser's, short of the
        // gutter its thumb stands in).
        node.layout.width = Length::Fill;
        return;
    }
    node.layout.padding.left += left;
    node.layout.padding.right += right;
    let (left, right) = (node.layout.padding.left, node.layout.padding.right);
    if let Some(first) = node.children.first_mut() {
        bleed(first, left, right);
    }
}

/// A card the list lays out row by row.
#[derive(Clone)]
pub(super) struct SplitCard {
    pub(super) key: String,
    pub(super) spec: GroupSpec,
    pub(super) padding: Insets,
    pub(super) gap: f32,
    pub(super) head: Vec<(UiNode, f32)>,
    /// What leads a run of its rows that starts past its first in the head's place, as tall as
    /// the head (empty: the head itself): the caption of a head that is a control, as Themes'
    /// open Community disclosure, which stays one control, the keyboard's on the card's first
    /// stop.
    pub(super) repeat: Vec<(UiNode, f32)>,
    pub(super) rows: Vec<(UiNode, f32)>,
}

fn is_heading(node: &UiNode) -> bool {
    match &node.content {
        UiContent::Text(text) => text.role == SemanticRole::Heading,
        UiContent::Group(_) if node.paint_only => {
            matches!(node.children.as_slice(), [only] if is_heading(only))
        }
        // A caption with its card's action ending its line (the display faces' Reset, the
        // activity's Open Log): the line is the card's caption.
        UiContent::Group(_) => {
            node.layout.flow == Flow::Row && node.children.first().is_some_and(is_heading)
        }
        _ => false,
    }
}

impl SplitCard {
    /// A column card authored whole, its rows moved out; `Err` gives back a boxed non-card node.
    pub(super) fn from_column(node: UiNode) -> Result<Self, Box<UiNode>> {
        let UiContent::Group(spec) = &node.content else {
            return Err(Box::new(node));
        };
        if node.layout.flow != Flow::Column || node.children.is_empty() {
            return Err(Box::new(node));
        }
        let heights = node
            .children
            .iter()
            .map(|child| match child.layout.height {
                Length::Fixed(height) => Some(height),
                _ => None,
            })
            .collect::<Option<Vec<_>>>();
        let Some(heights) = heights else {
            return Err(Box::new(node));
        };
        let spec = spec.clone();
        let mut rows: Vec<(UiNode, f32)> = node.children.into_iter().zip(heights).collect();
        let leads = rows.len() > 1 && is_heading(&rows[0].0);
        let head = if leads {
            vec![rows.remove(0)]
        } else {
            Vec::new()
        };
        Ok(Self {
            key: node.key.as_str().to_string(),
            spec,
            padding: node.layout.padding,
            gap: node.layout.gap,
            head,
            repeat: Vec::new(),
            rows,
        })
    }

    fn height_of(&self, rows: Range<usize>) -> f32 {
        self.height_under(&self.head, rows)
    }

    /// How tall the card's `rows` stand under `head`.
    fn height_under(&self, head: &[(UiNode, f32)], rows: Range<usize>) -> f32 {
        let children = head.len() + rows.len();
        self.padding.top
            + self.padding.bottom
            + head.iter().map(|(_, height)| height).sum::<f32>()
            + self.rows[rows]
                .iter()
                .map(|(_, height)| height)
                .sum::<f32>()
            + children.saturating_sub(1) as f32 * self.gap
    }

    /// How tall the card's first stop is in a list `room` tall
    /// ([`Self::stops`]): what of it a screen that starts with it must hold.
    pub(super) fn first_stop_height(&self, room: f32) -> f32 {
        self.stops(room)
            .first()
            .map_or_else(|| self.height(), |stop| self.height_of(0..stop.end))
    }

    pub(super) fn height(&self) -> f32 {
        self.height_of(0..self.rows.len())
    }

    /// Number of control-holding card rows shown whole on the first screen of height `room`.
    pub(super) fn first_screen_controls(&self, room: f32) -> usize {
        let mut count = 0;
        for (index, stop) in self.stops(room).into_iter().enumerate() {
            if index > 0 && self.height_of(0..stop.end) > room + 0.01 {
                break;
            }
            count += self.rows[stop]
                .iter()
                .filter(|(node, _)| holds_control(node))
                .count();
        }
        count
    }

    /// What a run of the card's rows pays besides each row and its gap.
    pub(super) fn height_of_head(&self) -> f32 {
        self.height_of(0..0) - if self.head.is_empty() { self.gap } else { 0.0 }
    }

    /// Positions of controls across `rows` from the top of the first row.
    pub(super) fn control_rows(&self, rows: Range<usize>) -> Vec<(f32, f32, f32)> {
        let mut top = 0.0;
        let mut out = Vec::new();
        for (node, height) in &self.rows[rows] {
            if let Some((at, tall)) = control_extent(node, *height) {
                out.push((top, top + at, tall));
            }
            top += height + self.gap;
        }
        out
    }

    /// What `rows` draw, band by band, from the top of the first ([`drawn_bands_in`]).
    pub(super) fn drawn_bands(&self, rows: Range<usize>) -> Vec<DrawnPart> {
        let mut top = 0.0;
        let mut out = Vec::new();
        for (node, height) in &self.rows[rows] {
            out.extend(
                drawn_bands_in(node, *height)
                    .into_iter()
                    .map(|part| DrawnPart {
                        top: part.top + top,
                        bottom: part.bottom + top,
                        ..part
                    }),
            );
            top += height + self.gap;
        }
        out
    }

    /// Where each control `rows` hold sits, from the top of the first, and the words naming the
    /// keyboard's there.
    pub(super) fn control_spans(&self, rows: Range<usize>) -> ControlSpans {
        let mut top = 0.0;
        let mut out: ControlSpans = (Vec::new(), None);
        for (node, height) in &self.rows[rows] {
            let (spans, words) = control_spans_in(node, *height);
            out.0.extend(
                spans
                    .into_iter()
                    .map(|(key, at, tall)| (key, top + at, tall)),
            );
            out.1 = out.1.or(words);
            top += height + self.gap;
        }
        out
    }

    /// How far under the first of `rows` what they draw ends.
    pub(super) fn ink(&self, rows: Range<usize>) -> f32 {
        let mut top = 0.0;
        let mut ink = 0.0_f32;
        for (node, height) in &self.rows[rows] {
            if let Some((at, tall)) = ink_extent(node, *height) {
                ink = ink.max(top + at + tall);
            }
            top += height + self.gap;
        }
        ink
    }

    /// How far under the top of a stop of `rows` their first row of controls ends.
    pub(super) fn first_controls(&self, rows: Range<usize>) -> Option<f32> {
        let mut top = 0.0;
        for (node, height) in &self.rows[rows] {
            if let Some((at, tall)) = first_control_row(node, *height) {
                return Some(top + at + tall);
            }
            top += height + self.gap;
        }
        None
    }

    /// How far under the top of a stop of `rows` what they draw begins.
    pub(super) fn ink_top(&self, rows: Range<usize>) -> f32 {
        let mut top = 0.0;
        for (node, height) in &self.rows[rows] {
            if let Some((at, _)) = ink_extent(node, *height) {
                return top + at;
            }
            top += height + self.gap;
        }
        0.0
    }

    /// Where the card's head (its caption) begins and ends under its top.
    pub(super) fn caption(&self) -> Option<(f32, f32)> {
        (!self.head.is_empty()).then(|| {
            let head = self.head.iter().map(|(_, height)| height).sum::<f32>()
                + self.head.len().saturating_sub(1) as f32 * self.gap;
            (self.padding.top, self.padding.top + head)
        })
    }

    /// Where a run's first row starts in the card.
    pub(super) fn height_of_lead(&self) -> f32 {
        self.padding.top
            + self.head.iter().map(|(_, height)| height).sum::<f32>()
            + self.head.len() as f32 * self.gap
    }

    /// The card's rows grouped into list stops.
    pub(super) fn stops(&self, room: f32) -> Vec<Range<usize>> {
        let room = room - self.height_of_head();
        let height = |rows: Range<usize>| {
            self.rows[rows]
                .iter()
                .map(|(_, height)| height + self.gap)
                .sum::<f32>()
        };
        let controls: Vec<bool> = self
            .rows
            .iter()
            .map(|(node, _)| holds_control(node))
            .collect();
        let has_control = |rows: &Range<usize>| controls[rows.clone()].iter().any(|row| *row);
        let under_control = |row: usize| {
            row > 0
                && controls[row - 1]
                && !controls[row]
                && matches!(
                    &self.rows[row].0.content,
                    UiContent::Group(GroupSpec {
                        role: SemanticRole::Status,
                        ..
                    })
                )
        };
        // Read only for a stop's first row the rows after it could join.
        let notes = |row: usize| {
            controls[..row].iter().any(|control| *control) && notes_rows_over(&self.rows[row].0)
        };
        // A row that draws nothing (a note's lines kept empty) ends the stop before it, as a
        // blank part ends its row ([`row_parts`]): a stop of its own, it was the last screen of
        // Text & Fonts at 2x in 178x152, a band of empty card.
        let blank: Vec<bool> = self
            .rows
            .iter()
            .map(|(node, height)| draws_nothing(node, *height))
            .collect();
        let mut stops: Vec<Range<usize>> = Vec::new();
        for row in 0..self.rows.len() {
            match stops.last_mut() {
                Some(stop) if blank[row] => stop.end = row + 1,
                Some(stop) if under_control(row) && height(stop.start..row + 1) <= room + 0.01 => {
                    stop.end = row + 1;
                }
                Some(stop)
                    if !has_control(stop)
                        && !under_control(stop.start)
                        && !notes(stop.start)
                        && controls[row..].iter().any(|control| *control)
                        && height(stop.start..row + 1) <= room + 0.01 =>
                {
                    stop.end = row + 1;
                }
                _ => stops.push(row..row + 1),
            }
        }
        stops
    }

    /// Whether every one of `rows` is a note on the rows over it ([`notes_rows_over`]), with a
    /// control over it in the card.
    pub(super) fn notes_only(&self, rows: Range<usize>) -> bool {
        !rows.is_empty()
            && rows.clone().all(|row| {
                notes_rows_over(&self.rows[row].0)
                    && self.rows[..row].iter().any(|(node, _)| holds_control(node))
            })
    }

    /// The card holding `rows`, its head over them ([`Self::repeat`] past its first row).
    pub(super) fn node(&self, rows: Range<usize>) -> UiNode {
        let head = if rows.start > 0 && !self.repeat.is_empty() {
            &self.repeat
        } else {
            &self.head
        };
        self.node_under(head, rows)
    }

    /// The card holding `rows` under `head`.
    fn node_under(&self, head: &[(UiNode, f32)], rows: Range<usize>) -> UiNode {
        UiNode::new(self.key.clone(), UiContent::Group(self.spec.clone()))
            .layout(
                Layout::column()
                    .width(Length::Fill)
                    .height(Length::Fixed(self.height_under(head, rows.clone())))
                    .padding(self.padding)
                    .gap(self.gap)
                    .clipped(),
            )
            .children(
                head.iter()
                    .map(|(node, _)| node.clone())
                    .chain(self.rows[rows].iter().map(|(node, _)| node.clone()))
                    .collect(),
            )
    }
}

impl SplitCard {
    fn row_limit(&self, room: f32) -> f32 {
        room - self.height_of_lead() - self.padding.bottom
    }

    fn splits(&self, (node, height): &(UiNode, f32), room: f32) -> bool {
        let drawn = height - node.layout.padding.top - node.layout.padding.bottom;
        *height > self.row_limit(room) + 0.01
            && splittable(node)
            && !(drawn <= room + 0.01 && holds_open_disclosure(node))
    }

    /// Whether the head the card repeats over every screen of its rows starves them: the room
    /// under it holds less than the head. Then the head is the card's first row, and scrolls
    /// away. (A row that fits only without the head starts its screen bare instead,
    /// [`ScrollPlan::starts_bare`], the head kept over every other.)
    fn head_starves(&self, room: f32) -> bool {
        let head = self.head.iter().map(|(_, height)| height).sum::<f32>()
            + self.head.len().saturating_sub(1) as f32 * self.gap;
        !self.head.is_empty() && !self.rows.is_empty() && self.row_limit(room) < head - 0.01
    }

    /// Whether the card is laid out again for a screen `room` tall.
    pub(super) fn outgrows(&self, room: f32) -> bool {
        self.head_starves(room) || self.rows.iter().any(|row| self.splits(row, room))
    }

    /// The card fitted to a screen `room` tall.
    pub(super) fn fitted(&self, room: f32) -> Self {
        self.fitted_with(room, self.head_starves(room))
    }

    fn fitted_with(&self, room: f32, starved: bool) -> Self {
        let (head, rows) = if starved {
            (
                Vec::new(),
                self.head.iter().chain(&self.rows).cloned().collect(),
            )
        } else {
            (self.head.clone(), self.rows.clone())
        };
        let card = Self {
            key: self.key.clone(),
            spec: self.spec.clone(),
            padding: self.padding,
            gap: self.gap,
            repeat: if starved {
                Vec::new()
            } else {
                self.repeat.clone()
            },
            head,
            rows: Vec::new(),
        };
        let limit = card.row_limit(room).max(1.0);
        Self {
            rows: rows
                .into_iter()
                .flat_map(|row| {
                    let limit = if holds_open_disclosure(&row.0) {
                        room.max(1.0)
                    } else {
                        limit
                    };
                    if card.splits(&row, room) {
                        row_parts(row, limit, room, card.gap)
                    } else {
                        vec![row]
                    }
                })
                .collect(),
            ..card
        }
    }
}

/// Whether `node` holds a disclosure that is open.
pub(super) fn holds_open_disclosure(node: &UiNode) -> bool {
    let open = match &node.content {
        UiContent::Button(control) => control.state.expanded == Some(true),
        _ => false,
    };
    open || node.children.iter().any(holds_open_disclosure)
}

/// Whether a row part can be listed a child at a time: a column of children (a segmented
/// control's wrapped lines among them), or a group that holds one such column alone (a stacked
/// row's name over its lines, its controls' line holding one control), so a part taller than its
/// screen is listed a line at a time rather than cut under the list's edge with its last lines
/// never shown (a seven-line name at 2× in a 152pt window showed one; Dark, the third line of a
/// wrapped Auto/Light/Dark, none).
fn splittable(node: &UiNode) -> bool {
    matches!(node.content, UiContent::Group(_))
        && node.layout.offset == 0.0
        && match (node.layout.flow, node.children.as_slice()) {
            (Flow::Column, children) => !children.is_empty(),
            (Flow::Overlay | Flow::Row, [only]) => !node.segmented && splittable(only),
            _ => false,
        }
}

/// Whether `node` stays whole wherever a screen holds it, split only where it is taller than the
/// most a part may take: a group around one column (a name over its lines), a segmented
/// control's lines (one control the keyboard enters once while it is whole).
fn whole_while_it_fits(node: &UiNode) -> bool {
    node.segmented || node.layout.flow != Flow::Column
}

fn column_heights(node: &UiNode, height: f32) -> Vec<f32> {
    let gaps = node.layout.gap * node.children.len().saturating_sub(1) as f32;
    let available = (height - node.layout.padding.top - node.layout.padding.bottom - gaps).max(0.0);
    let lengths = node
        .children
        .iter()
        .map(|child| match child.layout.height {
            Length::Fixed(value) => Some(value),
            Length::Fraction(value) => Some(available * value),
            Length::Fill => None,
        })
        .collect::<Vec<_>>();
    let fills = lengths.iter().filter(|length| length.is_none()).count();
    let fixed = lengths.iter().flatten().sum::<f32>();
    let fill = if fills == 0 {
        0.0
    } else {
        (available - fixed).max(0.0) / fills as f32
    };
    lengths
        .into_iter()
        .map(|length| length.unwrap_or(fill).max(0.0))
        .collect()
}

/// Whether `node`, laid out `height` tall, draws nothing: no words, control, picture or box.
fn draws_nothing(node: &UiNode, height: f32) -> bool {
    drawn_bands_in(node, height).is_empty() && ink_extent(node, height).is_none()
}

/// Split a row taller than `limit`, where it is a column of children, into parts listed one at
/// a time.
pub(super) fn row_parts(
    (node, height): (UiNode, f32),
    limit: f32,
    most: f32,
    gap: f32,
) -> Vec<(UiNode, f32)> {
    let limit = if whole_while_it_fits(&node) {
        most.max(limit)
    } else {
        limit
    };
    if height <= limit + 0.01 || !splittable(&node) {
        return vec![(node, height)];
    }
    let heights = column_heights(&node, height);
    let parent_gap = gap;
    let mut template = node;
    let children = std::mem::take(&mut template.children);
    let (gap, padding) = (template.layout.gap, template.layout.padding);
    let inner = (limit - padding.top - padding.bottom).max(1.0);
    let inner_most = (most - padding.top - padding.bottom).max(inner);
    let pieces: Vec<(UiNode, f32)> = children
        .into_iter()
        .zip(heights)
        .flat_map(|(mut child, height)| {
            child.layout.height = Length::Fixed(height);
            row_parts((child, height), inner, inner_most, gap)
        })
        .collect();
    // A group around one column wraps each part of it as it wraps the column: its child's parts
    // are already as full as the room holds, and side by side or overlaid they would not stack.
    if template.layout.flow != Flow::Column {
        let count = pieces.len();
        return pieces
            .into_iter()
            .enumerate()
            .map(|(index, (piece, height))| {
                let mut part = template.clone();
                let padding = Insets {
                    top: if index == 0 { padding.top } else { 0.0 },
                    bottom: if index + 1 == count {
                        padding.bottom
                    } else {
                        0.0
                    },
                    ..padding
                };
                let height = height + padding.top + padding.bottom;
                if index > 0 {
                    part.key = UiKey::new(format!("{}/part-{index}", template.key.as_str()));
                    part.content = UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group));
                }
                part.layout.padding = padding;
                part.layout.height = Length::Fixed(height);
                part.children = vec![piece];
                (part, height)
            })
            .collect();
    }
    let mut parts: Vec<Vec<(UiNode, f32)>> = vec![Vec::new()];
    let mut used = 0.0_f32;
    for (piece, height) in pieces {
        let first = parts.len() == 1;
        let part = parts.last_mut().expect("one part");
        let cost = if part.is_empty() {
            height
        } else {
            gap + height
        };
        let names = first
            && !part.is_empty()
            && !part[1..].iter().any(|(node, _)| holds_control(node))
            && holds_control(&piece)
            && used + cost <= inner_most + 0.01;
        // NO PART DRAWS NOTHING: the line a row keeps empty for its Reset, listed a part of its
        // own, was a stop a screen started on, showing only a band of empty card (Home's last
        // screen, Harness and Software Update at 2x in 178x152). A piece that draws nothing ends
        // the part before it, as it ends the row, where nothing an edge cuts is drawn; one that
        // leads leads the piece after it.
        let blank = draws_nothing(&piece, height)
            || part
                .iter()
                .all(|(node, height)| draws_nothing(node, *height));
        if !part.is_empty() && used + cost > inner + 0.01 && !names && !blank {
            parts.push(vec![(piece, height)]);
            used = height;
        } else {
            part.push((piece, height));
            used += cost;
        }
    }
    let count = parts.len();
    let style = match &template.content {
        UiContent::Group(group) => group.style,
        _ => StyleRef::Plain,
    };
    let boxed = matches!(
        style,
        StyleRef::Primary | StyleRef::Secondary | StyleRef::Code
    );
    parts
        .into_iter()
        .enumerate()
        .map(|(index, pieces)| {
            let mut part = template.clone();
            let padding = Insets {
                top: if index == 0 { padding.top } else { 0.0 },
                bottom: if index + 1 == count {
                    padding.bottom
                } else {
                    0.0
                },
                ..padding
            };
            let height = pieces.iter().map(|(_, height)| height).sum::<f32>()
                + gap * pieces.len().saturating_sub(1) as f32
                + padding.top
                + padding.bottom;
            if index > 0 {
                part.key = UiKey::new(format!("{}/part-{index}", template.key.as_str()));
                part.content =
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group).style(style));
            }
            part.layout.padding = padding;
            part.layout.height = Length::Fixed(height);
            part.children = pieces.into_iter().map(|(node, _)| node).collect();
            if boxed {
                part.joins = crate::native_ui::Joins {
                    above: index > 0,
                    below: (index + 1 < count).then_some(parent_gap),
                };
            }
            (part, height)
        })
        .collect()
}

fn fit_rows(items: &[ListItem], room: f32) -> std::borrow::Cow<'_, [ListItem]> {
    let outgrows = items.iter().any(|item| match item {
        ListItem::Split(card) => card.outgrows(room),
        ListItem::Card(..) => false,
    });
    if !outgrows {
        return std::borrow::Cow::Borrowed(items);
    }
    std::borrow::Cow::Owned(
        items
            .iter()
            .map(|item| match item {
                ListItem::Split(card) if card.outgrows(room) => ListItem::Split(card.fitted(room)),
                item => item.clone(),
            })
            .collect(),
    )
}

/// One item of a [`list_page`].
#[derive(Clone)]
pub(super) enum ListItem {
    Card(Box<UiNode>, f32),
    Split(SplitCard),
}

impl ListItem {
    /// A card node listed row by row where it is a column of fixed rows, whole where it is not.
    pub(super) fn of(node: UiNode, height: f32) -> Self {
        match SplitCard::from_column(node) {
            Ok(card) => Self::Split(card),
            Err(node) => Self::Card(node, height),
        }
    }
}

pub(super) type ItemStops = Vec<(usize, Option<Range<usize>>)>;

fn plan_items(items: &[ListItem], gap: f32, room: f32) -> (ScrollPlan, ItemStops) {
    plan_items_measured(items, gap, room, true)
}

/// [`plan_items`] as far as [`ScrollPlan::span`] reads it (the stops' heights and groups and
/// the groups' heads), for a caller that asks only how tall stops are: it skips measuring each
/// stop's keys, controls and ink.
fn plan_spans(items: &[ListItem], gap: f32, room: f32) -> ScrollPlan {
    plan_items_measured(items, gap, room, false).0
}

fn plan_items_measured(
    items: &[ListItem],
    gap: f32,
    room: f32,
    measured: bool,
) -> (ScrollPlan, ItemStops) {
    let stops_at_most = items
        .iter()
        .map(|item| match item {
            ListItem::Split(card) => card.rows.len().max(1),
            _ => 1,
        })
        .sum();
    let mut plan = ScrollPlan::with_stops(gap, stops_at_most);
    let mut stops: ItemStops = Vec::with_capacity(stops_at_most);
    let keys = |node: &UiNode| {
        if measured {
            focus_keys(node)
        } else {
            Vec::new()
        }
    };
    for (index, item) in items.iter().enumerate() {
        match item {
            ListItem::Card(_, height) if !measured => {
                plan.card(*height, Vec::new());
                stops.push((index, None));
            }
            ListItem::Card(node, height) => {
                plan.card(*height, focus_keys(node));
                plan.set_ids(vec![node.key.clone()]);
                plan.measure(node, *height);
                plan.note_previews([&**node]);
                plan.add_segments(node);
                stops.push((index, None));
            }
            ListItem::Split(card) if card.rows.is_empty() && !measured => {
                plan.card(card.height(), Vec::new());
                stops.push((index, None));
            }
            ListItem::Split(card) if card.rows.is_empty() => {
                let node = card.node(0..card.rows.len());
                plan.card(card.height(), focus_keys(&node));
                plan.set_ids(vec![UiKey::new(card.key.as_str())]);
                plan.measure(&node, card.height());
                plan.note_previews(card.head.iter().map(|(node, _)| node));
                stops.push((index, None));
            }
            ListItem::Split(card) => {
                let group = plan.group(card.height_of_head(), card.height_of_lead());
                plan.set_row_gap(group, card.gap);
                if let Some((top, bottom)) = card.caption() {
                    plan.set_caption(group, top, bottom);
                }
                for (part, rows) in card.stops(room).into_iter().enumerate() {
                    // A head that is a control (Themes' open Community disclosure) is the
                    // keyboard's on the card's first stop, reached and revealed as every stop is.
                    plan.row(
                        group,
                        card.rows[rows.clone()]
                            .iter()
                            .map(|(_, height)| height + card.gap)
                            .sum(),
                        card.head
                            .iter()
                            .filter(|_| part == 0)
                            .chain(&card.rows[rows.clone()])
                            .flat_map(|(node, _)| keys(node))
                            .collect(),
                    );
                    if !measured {
                        stops.push((index, Some(rows)));
                        continue;
                    }
                    plan.set_ids(
                        card.rows[rows.clone()]
                            .iter()
                            .map(|(node, _)| node.key.clone())
                            .collect(),
                    );
                    let mut within = Vec::new();
                    for (node, _) in card.head.iter().filter(|_| part == 0) {
                        within.push(node.key.clone());
                        keys_within(node, &mut within);
                    }
                    for (node, _) in &card.rows[rows.clone()] {
                        keys_within(node, &mut within);
                    }
                    plan.set_within(within);
                    plan.set_control_rows(card.control_rows(rows.clone()));
                    plan.set_drawn_bands(card.drawn_bands(rows.clone()));
                    let (spans, words) = card.control_spans(rows.clone());
                    plan.set_control_spans(spans);
                    plan.set_keyboard_words(words);
                    plan.set_note(card.notes_only(rows.clone()));
                    plan.set_ink(card.ink(rows.clone()));
                    plan.set_ink_top(card.ink_top(rows.clone()));
                    plan.set_first_controls(card.first_controls(rows.clone()));
                    plan.note_previews(card.rows[rows.clone()].iter().map(|(node, _)| node));
                    for (node, _) in &card.rows[rows.clone()] {
                        plan.add_segments(node);
                    }
                    stops.push((index, Some(rows)));
                }
            }
        }
    }
    (plan, stops)
}

/// The height the tallest stop of `items` takes on screen in a list at most `room` tall.
pub(super) fn tallest_span(items: &[ListItem], gap: f32, room: f32) -> f32 {
    let plan = plan_spans(&fit_rows(items, room), gap, room);
    (0..plan.len())
        .map(|stop| plan.span(stop..stop + 1))
        .fold(0.0, f32::max)
}

/// The height the first `stops` stops of `items` take on screen together.
pub(super) fn leading_span(items: &[ListItem], gap: f32, room: f32, stops: usize) -> f32 {
    let plan = plan_spans(&fit_rows(items, room), gap, room);
    plan.span(0..stops.min(plan.len()))
}

/// What the first screen of `items` shows whole ([`list_page_peeking`]).
pub(super) fn first_screen_holds(
    items: &[ListItem],
    gap: f32,
    room: f32,
    overhang: Overhang,
    keys: &[UiKey],
) -> Option<usize> {
    let (mut plan, stops) = plan_items(&fit_rows(items, room), gap, room);
    seat_room(&mut plan, room, overhang);
    let whole =
        |stop: usize| plan.view(0).contains(&stop) && plan.span(0..stop + 1) <= plan.room() + 0.01;
    let first_item = stops.iter().take_while(|(item, _)| *item == 0).count();
    (first_item > 0 && (0..first_item).all(whole)).then(|| {
        keys.iter()
            .filter(|key| plan.stop_of(key).is_some_and(whole))
            .count()
    })
}

/// `items` as one scrolling list in `room`, `gap` apart, seated for this view.
pub(super) fn list_page(
    state: &SettingsViewState,
    key: &str,
    label: &str,
    items: Vec<ListItem>,
    room: f32,
    gap: f32,
) -> UiNode {
    list_page_peeking(state, key, label, items, room, gap, Overhang::NONE)
}

/// Render a settings list page with its preview strip peeking.
pub(super) fn list_page_peeking(
    state: &SettingsViewState,
    key: &str,
    label: &str,
    items: Vec<ListItem>,
    room: f32,
    gap: f32,
    overhang: Overhang,
) -> UiNode {
    let mut items = items;
    for (at, (node, height)) in state.take_list_head().into_iter().enumerate() {
        items.insert(at, ListItem::Card(Box::new(node), height));
    }
    let items = fit_rows(&items, room);
    let seated = SeatedList::new(state, &items, room, gap, overhang);
    let children = seated.screen_children(&items);
    seated.screen_node(key, label, children)
}

struct SeatedList {
    plan: ScrollPlan,
    stops: ItemStops,
    view: Range<usize>,
    drawn: Range<usize>,
}

impl SeatedList {
    fn new(
        state: &SettingsViewState,
        items: &[ListItem],
        room: f32,
        gap: f32,
        overhang: Overhang,
    ) -> Self {
        Self::with_foot(state, items, room, gap, overhang, ListFoot::Peek)
    }

    fn with_foot(
        state: &SettingsViewState,
        items: &[ListItem],
        room: f32,
        gap: f32,
        overhang: Overhang,
        foot: ListFoot,
    ) -> Self {
        let given = state.status_extra.get();
        let (plan, stops) = plan_under_status(items, gap, room, given, overhang, foot);
        Self::seated(state, plan, stops, room)
    }

    /// `plan` and its `stops` seated in a list `room` tall.
    fn seated(
        state: &SettingsViewState,
        mut plan: ScrollPlan,
        stops: ItemStops,
        room: f32,
    ) -> Self {
        state.note_status_room(&plan, room);
        let view = state.seat_scroll(&mut plan);
        let drawn = drawn_stops(&plan, &view);
        Self {
            plan,
            stops,
            view,
            drawn,
        }
    }

    fn screen_children(&self, items: &[ListItem]) -> Vec<UiNode> {
        let stops = &self.stops;
        self.plan
            .runs(self.drawn.clone())
            .into_iter()
            .filter_map(|run| match run {
                ScrollRun::Card(stop) => Some(match &items[stops[stop].0] {
                    ListItem::Card(node, _) => (**node).clone(),
                    ListItem::Split(card) => card.node(0..card.rows.len()),
                }),
                ScrollRun::Group { rows, .. } => {
                    let ListItem::Split(card) = &items[stops[rows.start].0] else {
                        return None;
                    };
                    let first = stops[rows.start].1.as_ref()?.start;
                    let end = stops[rows.end - 1].1.as_ref()?.end;
                    Some(card.node(first..end))
                }
            })
            .collect()
    }

    fn offscreen_children(
        &self,
        items: &[ListItem],
        range: Range<usize>,
        side: &str,
    ) -> Vec<UiNode> {
        let stops = &self.stops;
        self.plan
            .runs(range)
            .into_iter()
            .filter_map(|run| match run {
                ScrollRun::Card(stop) => match &items[stops.get(stop)?.0] {
                    ListItem::Card(node, _) => Some((**node).clone().offscreen()),
                    ListItem::Split(card) => Some(card.node(0..card.rows.len()).offscreen()),
                },
                ScrollRun::Group { rows, .. } => {
                    let (item, _) = stops.get(rows.start)?;
                    let ListItem::Split(card) = &items[*item] else {
                        return None;
                    };
                    let first = stops.get(rows.start)?.1.clone()?.start;
                    let last = stops.get(rows.end - 1)?.1.clone()?.end;
                    // Off the screen a part lists its rows alone, but for a head that is a control
                    // over its first row, which stays in the tree with it as every control does.
                    let head: &[(UiNode, f32)] = if first == 0 && !card.repeat.is_empty() {
                        &card.head
                    } else {
                        &[]
                    };
                    let mut node = card.node_under(head, first..last).offscreen();
                    node.key = UiKey::new(format!("{}/{side}", card.key));
                    Some(node)
                }
            })
            .collect()
    }

    fn screen_node(&self, key: &str, label: &str, children: Vec<UiNode>) -> UiNode {
        scroll_list_screen(
            key,
            label,
            &self.plan,
            self.view.clone(),
            self.drawn.clone(),
            children,
        )
    }
}

/// [`list_page`] with offscreen rows in the tree, the keyboard's control pinned as `pin` says.
pub(super) fn list_page_whole(
    state: &SettingsViewState,
    (key, label): (&str, &str),
    items: std::rc::Rc<[ListItem]>,
    (room, gap): (f32, f32),
    (rows, pin): (ListRows, Option<(f32, f32)>),
) -> UiNode {
    let given = state.status_extra.get();
    let fitted = fit_rows(&items, stops_room(state, room, ListFoot::Whole));
    let planned = shared_plan(&items, (gap, room, given), || {
        plan_under_status(&fitted, gap, room, given, Overhang::NONE, ListFoot::Whole)
    });
    let (mut plan, stops) = (*planned).clone();
    plan.pin_keyboard(pin);
    let seated = SeatedList::seated(state, plan, stops, room);
    let drawn = seated.drawn.clone();
    let above = seated.offscreen_children(&fitted, 0..drawn.start, "above");
    let below = seated.offscreen_children(&fitted, drawn.end..seated.plan.len(), "below");
    let lead = above
        .iter()
        .map(|node| match node.layout.height {
            Length::Fixed(height) => height + seated.plan.gap(),
            _ => seated.plan.gap(),
        })
        .sum::<f32>();
    let screen = seated.screen_children(&fitted);
    drop(fitted);
    let mut page = seated.screen_node(key, label, screen);
    if let Some(list) = find_node_mut(&mut page, key) {
        list.layout.offset += lead;
        if let Some(room) = list.room.as_mut() {
            room.lists_all = true;
        }
        let screen = std::mem::take(&mut list.children);
        list.children = above.into_iter().chain(screen).chain(below).collect();
    }
    if let Some(lists) = state.status_lists.borrow_mut().as_mut() {
        lists.push(ListLaid { items, gap, rows });
    }
    page
}

fn find_node_mut<'a>(node: &'a mut UiNode, key: &str) -> Option<&'a mut UiNode> {
    if node.key.as_str() == key {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_node_mut(child, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_ui::Control;

    /// NO STEP SHOWS ONLY A SLIVER MORE: a stop a point taller than the room is read from its
    /// own seat, its lower edge under the room's left unshown, with no step a point further
    /// (which looked like the screen before, as if Page Down had stopped: Text & Fonts' 97pt
    /// Font family in a 96pt list at 2x); one a line taller steps through to its end.
    #[test]
    fn a_stop_a_sliver_taller_than_the_room_takes_no_step_to_show_the_sliver() {
        let plan = |tall: f32| {
            let mut plan = ScrollPlan::new(10.0);
            plan.card(60.0, vec![UiKey::new("before")]);
            plan.card(tall, vec![UiKey::new("tall")]);
            plan.set_control_rows(vec![(0.0, 0.0, tall)]);
            plan.set_drawn_bands(vec![DrawnPart {
                top: 0.0,
                bottom: tall,
                whole: true,
            }]);
            plan.card(60.0, vec![UiKey::new("after")]);
            plan.set_room(96.0);
            plan
        };
        let sliver = plan(97.0);
        assert!(sliver.taller_than_room(1));
        assert_eq!(sliver.landings(1), vec![None], "its own seat alone");
        assert_eq!(
            sliver.page_down(1, None).0,
            2,
            "Page Down goes on to the next stop"
        );
        let taller = plan(140.0);
        assert!(
            taller.landings(1).len() > 1,
            "a stop a line taller steps on: {:?}",
            taller.landings(1)
        );
    }

    fn cards(heights: &[f32], room: f32, gap: f32) -> ScrollPlan {
        let mut plan = ScrollPlan::new(gap);
        for height in heights {
            plan.card(*height, Vec::new());
        }
        plan.set_room(room);
        plan
    }

    #[test]
    fn a_row_whose_second_line_the_strip_holds_is_shown_and_drawn_whole() {
        let plan = |overhang: f32, note: f32, control: f32| {
            let mut plan = ScrollPlan::new(10.0);
            plan.card(80.0, vec![UiKey::new("first")]);
            plan.card(4.0 + control + note, vec![UiKey::new("row")]);
            plan.set_control_rows(vec![(0.0, 4.0, control)]);
            plan.set_ink(4.0 + control + note);
            plan.card(60.0, vec![UiKey::new("after")]);
            plan.card(60.0, Vec::new());
            seat_room(&mut plan, 140.0, Overhang { now: overhang });
            plan
        };
        let shown = plan(44.0, 18.0, 36.0);
        assert_eq!(shown.view(0), 0..2, "the row is shown");
        let screen = shown.screen(0);
        assert_eq!(
            screen.live, 158.0,
            "drawn whole down to its note's end, and on over the gap to the next card"
        );
        assert_eq!(shown.page_after(0), 2, "Page Down turns past it");
        let barred = plan(0.0, 18.0, 36.0);
        assert_eq!(barred.view(0), 0..1);
        assert_eq!(barred.screen(0).live, 140.0);
        assert_eq!(plan(44.0, 18.0, 48.0).view(0), 0..1, "its control");
        assert_eq!(plan(44.0, 60.0, 36.0).view(0), 0..1, "past the strip");
        let mut words = ScrollPlan::new(10.0);
        words.card(80.0, Vec::new());
        words.card(58.0, Vec::new());
        words.card(60.0, Vec::new());
        seat_room(&mut words, 140.0, Overhang { now: 44.0 });
        assert_eq!(words.view(0), 0..1, "words alone are shown in the room");
        assert_eq!(words.screen(0).live, 140.0);
    }

    #[test]
    fn the_first_screens_lead_is_its_first_stop_and_the_first_row_of_controls_after_it() {
        let button = |key: &str| {
            UiNode::new(
                key,
                UiContent::Button(Control::new(
                    crate::native_ui::ButtonSpec::new(key),
                    crate::native_ui::ActionId::new(key),
                )),
            )
            .layout(Layout::default().height(Length::Fixed(44.0)))
        };
        let words = |key: &str, height: f32| {
            UiNode::new(key, UiContent::Text(crate::native_ui::TextSpec::text(key)))
                .layout(Layout::default().height(Length::Fixed(height)))
        };
        let column = |key: &str, height: f32, gap: f32, children: Vec<UiNode>| {
            UiNode::new(
                key,
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::column()
                    .height(Length::Fixed(height))
                    .padding(Insets::all(12.0))
                    .gap(gap),
            )
            .children(children)
        };
        let card = column(
            "card",
            12.0 + 52.0 + 120.0 + 12.0,
            0.0,
            vec![
                words("words", 52.0),
                column(
                    "actions",
                    120.0,
                    8.0,
                    vec![button("first"), button("second")],
                ),
            ],
        );
        let mut plan = ScrollPlan::new(10.0);
        plan.card(68.0, vec![UiKey::new("lead")]);
        plan.set_control_rows(vec![(0.0, 12.0, 44.0)]);
        plan.card(196.0, focus_keys(&card));
        plan.measure(&card, 196.0);
        plan.set_room(400.0);
        assert_eq!(plan.first_screen_need(), 78.0 + 120.0);
        let mut quiet = ScrollPlan::new(10.0);
        quiet.card(68.0, vec![UiKey::new("lead")]);
        quiet.card(30.0, Vec::new());
        quiet.card(196.0, focus_keys(&card));
        quiet.measure(&card, 196.0);
        quiet.set_room(400.0);
        assert_eq!(quiet.first_screen_need(), 118.0 + 120.0);
        let mut alone = ScrollPlan::new(10.0);
        alone.card(90.0, Vec::new());
        alone.set_room(400.0);
        assert_eq!(alone.first_screen_need(), 90.0);
    }

    #[test]
    fn page_up_ends_on_the_stop_before_the_screen_it_leaves() {
        let plan = cards(&[30.0, 30.0, 90.0, 30.0, 30.0, 30.0, 30.0], 100.0, 10.0);
        assert_eq!(plan.view(4), 4..6);
        assert_eq!(plan.page_before(4), 3);
        assert_eq!((plan.view(3), plan.view(2)), (3..5, 2..3));
        let (top, nudge) = plan.page_up(4);
        assert_eq!((top, nudge), (2, Some(30.0)), "3 ends at the room's edge");
        let mut landed = plan.clone();
        landed.seated = nudge.map(|nudge| Seat::nudged(top, nudge));
        assert_eq!(landed.shown(top), 3..4, "the tall card cut at the top");
        assert_eq!(landed.page_after(top), 4, "Page Down comes back");
        let mut padded = ScrollPlan::new(10.0);
        for _ in 0..6 {
            padded.card(50.0, Vec::new());
            padded.set_ink_top(10.0);
        }
        padded.set_room(105.0);
        let last = padded.limit();
        assert_eq!((last, padded.view(last)), (4, 4..5));
        assert_eq!(padded.screen(last).offset, 5.0, "scrolled into 4's padding");
        assert_eq!(padded.shown(last), 4..6, "4 whole under its cut padding");
        assert_eq!(padded.page_up(last), (3, None));
        assert_eq!(padded.shown(3), 3..4);
        let even = cards(&[30.0; 8], 100.0, 10.0);
        assert_eq!(even.page_up(3), (1, None));
        assert_eq!(even.view(1).end, 3);
    }

    #[test]
    fn a_screen_holds_every_whole_stop_that_fits_and_never_none() {
        let plan = cards(&[40.0, 40.0, 40.0, 40.0], 100.0, 10.0);
        assert_eq!(plan.view(0), 0..2);
        assert_eq!(plan.view(3), 3..4);
        let tall = cards(&[300.0, 40.0], 100.0, 10.0);
        assert_eq!(
            tall.view(0),
            0..1,
            "a stop taller than the room is still shown"
        );
    }

    #[test]
    fn the_last_screen_is_full() {
        let plan = cards(&[40.0, 40.0, 40.0, 40.0, 40.0], 100.0, 10.0);
        assert_eq!(plan.limit(), 3);
        assert_eq!(plan.view(plan.limit()), 3..5);
        assert_eq!(cards(&[40.0, 40.0], 100.0, 10.0).limit(), 0);
        assert_eq!(cards(&[], 100.0, 10.0).limit(), 0);
    }

    #[test]
    fn page_up_shows_the_screen_that_ends_where_the_screen_began() {
        let plan = cards(&[40.0; 7], 100.0, 10.0);
        assert_eq!(plan.page_before(4), 2);
        assert_eq!(plan.view(plan.page_before(4)), 2..4);
        assert_eq!(plan.page_before(1), 0);
        assert_eq!(plan.page_before(0), 0);
        let tall = cards(&[40.0, 300.0, 40.0], 100.0, 10.0);
        assert_eq!(tall.page_before(2), 1, "a tall stop is a screen of its own");
    }

    #[test]
    fn reveal_moves_the_least_that_shows_the_stop() {
        let plan = cards(&[40.0; 6], 100.0, 10.0);
        assert_eq!(plan.reveal(1, 0), 0);
        assert_eq!(plan.reveal(3, 0), 2);
        assert_eq!(plan.reveal(0, 3), 0);
        assert_eq!(plan.reveal(5, 0), 4);
    }

    #[test]
    fn a_split_card_run_measures_as_the_card_it_draws() {
        let text = |key: &str| {
            UiNode::new(
                key,
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
        };
        let card = SplitCard {
            key: "card".to_string(),
            spec: GroupSpec::new("Card").style(StyleRef::Secondary),
            padding: Insets::all(20.0),
            gap: 8.0,
            head: vec![(text("head"), 22.0)],
            repeat: Vec::new(),
            rows: (0..5)
                .map(|row| (text(&format!("row/{row}")), 30.0))
                .collect(),
        };
        let mut plan = ScrollPlan::new(10.0);
        let group = plan.group(card.height_of(0..0), card.height_of_lead());
        for (_, height) in &card.rows {
            plan.row(group, height + card.gap, Vec::new());
        }
        for rows in [0..1, 1..4, 2..5] {
            assert_eq!(plan.span(rows.clone()), card.height_of(rows));
        }
    }

    #[test]
    fn a_runs_next_stop_starts_where_its_card_draws_it() {
        let text = |key: &str| {
            UiNode::new(
                key,
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
        };
        for head in [false, true] {
            for bottom in [0.0, 2.0, 4.0, 20.0] {
                let card = SplitCard {
                    key: "card".to_string(),
                    spec: GroupSpec::new("Card"),
                    padding: Insets {
                        top: 6.0,
                        bottom,
                        ..Insets::default()
                    },
                    gap: 4.0,
                    head: if head {
                        vec![(text("head"), 24.0)]
                    } else {
                        Vec::new()
                    },
                    repeat: Vec::new(),
                    rows: (0..4)
                        .map(|row| (text(&format!("row/{row}")), 24.0))
                        .collect(),
                };
                let mut plan = ScrollPlan::new(6.0);
                let group = plan.group(card.height_of_head(), card.height_of_lead());
                plan.set_row_gap(group, card.gap);
                for (_, height) in &card.rows {
                    plan.row(group, height + card.gap, Vec::new());
                }
                let last = plan.card(32.0, Vec::new());
                for from in 0..last {
                    let at = format!("head {head}, padding {bottom}, from row {from}");
                    let Length::Fixed(drawn) = card.node(from..card.rows.len()).layout.height
                    else {
                        panic!("{at}: a split card is drawn at a fixed height");
                    };
                    assert_eq!(
                        plan.stop_top(from, last),
                        drawn + plan.gap(),
                        "{at}: where the card starts"
                    );
                    let tall = drawn + plan.gap() + 32.0;
                    plan.set_room(tall);
                    assert_eq!(plan.view(from), from..last + 1, "{at}: shown");
                    plan.set_room(tall - 1.0);
                    assert_eq!(plan.view(from), from..last, "{at}: the control");
                }
            }
        }
    }

    #[test]
    fn a_card_never_leaves_its_words_on_a_screen_without_their_control() {
        let words = |key: &str| words(key);
        let control = |key: &str| {
            UiNode::new(
                key,
                UiContent::Custom(AuditedCustomNode {
                    audit_id: MESSAGES_SCROLL_AUDIT,
                    role: SemanticRole::Button,
                    label: String::new(),
                    paint: None,
                    action: None,
                    focusable: true,
                    focused: false,
                    focus_visible: false,
                }),
            )
        };
        let card = SplitCard {
            key: "card".to_string(),
            spec: GroupSpec::new("Card"),
            padding: Insets::all(10.0),
            gap: 5.0,
            head: Vec::new(),
            repeat: Vec::new(),
            rows: vec![
                (words("description"), 20.0),
                (control("first"), 40.0),
                (control("second"), 40.0),
                (words("caption"), 20.0),
            ],
        };
        assert_eq!(card.stops(1_000.0), vec![0..2, 2..3, 3..4]);
        assert_eq!(card.stops(60.0), vec![0..1, 1..2, 2..3, 3..4]);

        let status = UiNode::new(
            "status",
            UiContent::Group(GroupSpec {
                label: Some("Audio unavailable".to_string()),
                role: SemanticRole::Status,
                style: StyleRef::Quiet,
            }),
        )
        .children(vec![words("status/words")]);
        let card = SplitCard {
            rows: vec![
                (control("switch"), 40.0),
                (status, 10.0),
                (control("next"), 20.0),
            ],
            ..card
        };
        assert_eq!(card.stops(1_000.0), vec![0..2, 2..3]);
        assert_eq!(card.stops(60.0), vec![0..1, 1..2, 2..3]);
        let card = SplitCard {
            rows: vec![
                (control("switch"), 40.0),
                (words("next/words"), 10.0),
                (control("next"), 20.0),
            ],
            ..card
        };
        assert_eq!(card.stops(1_000.0), vec![0..1, 1..3]);
    }

    #[test]
    fn a_note_on_the_rows_over_it_is_a_stop_of_its_own() {
        let control = |key: &str| {
            UiNode::new(
                key,
                UiContent::Custom(AuditedCustomNode {
                    audit_id: MESSAGES_SCROLL_AUDIT,
                    role: SemanticRole::Button,
                    label: String::new(),
                    paint: None,
                    action: None,
                    focusable: true,
                    focused: false,
                    focus_visible: false,
                }),
            )
        };
        let lines = |key: &str, role: SemanticRole| {
            UiNode::new(
                key,
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .children(
                (0..2)
                    .map(|line| {
                        UiNode::new(
                            format!("{key}/line-{line}"),
                            UiContent::Text(crate::native_ui::TextSpec {
                                text: "words".to_string(),
                                role,
                                style: StyleRef::Quiet,
                            }),
                        )
                        .paint_only()
                    })
                    .collect(),
            )
        };
        let card = |rows: Vec<(UiNode, f32)>| SplitCard {
            key: "card".to_string(),
            spec: GroupSpec::new("Card"),
            padding: Insets::all(10.0),
            gap: 5.0,
            head: Vec::new(),
            repeat: Vec::new(),
            rows,
        };
        let shared = card(vec![
            (control("color"), 40.0),
            (control("accent"), 40.0),
            (lines("shared-note", SemanticRole::Status), 40.0),
            (control("bloom"), 40.0),
        ]);
        assert_eq!(shared.stops(1_000.0), vec![0..1, 1..2, 2..3, 3..4]);
        let status_first = card(vec![
            (lines("status", SemanticRole::Status), 30.0),
            (control("install"), 40.0),
        ]);
        assert_eq!(status_first.stops(1_000.0), vec![0..2]);
        let description = card(vec![
            (control("switch"), 40.0),
            (lines("next/state", SemanticRole::Text), 30.0),
            (control("next"), 40.0),
        ]);
        assert_eq!(description.stops(1_000.0), vec![0..1, 1..3]);
    }

    /// A line of words, which a list draws (a row that draws nothing is no stop of its own).
    fn words(key: &str) -> UiNode {
        UiNode::new(
            key,
            UiContent::Text(crate::native_ui::TextSpec {
                text: key.to_string(),
                role: SemanticRole::Text,
                style: StyleRef::Plain,
            }),
        )
    }

    #[test]
    fn a_switch_drawn_whole_over_its_cards_caption_is_turned_past() {
        let control = UiNode::new(
            "switch",
            UiContent::Custom(AuditedCustomNode {
                audit_id: MESSAGES_SCROLL_AUDIT,
                role: SemanticRole::Button,
                label: String::new(),
                paint: None,
                action: None,
                focusable: true,
                focused: false,
                focus_visible: false,
            }),
        );
        let card = SplitCard {
            key: "card".to_string(),
            spec: GroupSpec::new("Card"),
            padding: Insets::all(10.0),
            gap: 5.0,
            head: Vec::new(),
            repeat: Vec::new(),
            rows: vec![
                (control, 40.0),
                (
                    UiNode::new(
                        "caption",
                        UiContent::Text(crate::native_ui::TextSpec::text("caption")),
                    ),
                    20.0,
                ),
            ],
        };
        let lead = UiNode::new("lead", UiContent::Group(GroupSpec::new("Lead")));
        let after = UiNode::new("after", UiContent::Group(GroupSpec::new("After")));
        let items = [
            ListItem::Card(Box::new(lead), 100.0),
            ListItem::Split(card),
            ListItem::Card(Box::new(after), 160.0),
        ];
        let (mut plan, _) = plan_items(&items, 10.0, 170.0);
        seat_room(&mut plan, 170.0, Overhang::NONE);
        let switch = plan.stop_of(&UiKey::new("switch")).expect("listed");
        assert!(
            plan.shown(0).contains(&switch),
            "the first screen counts the switch: {:?}",
            plan.shown(0)
        );
        assert!(
            plan.page_after(0) > switch,
            "Page Down turns past it, to the caption"
        );
        assert_eq!(plan.len(), 4, "the switch and the caption are a stop each");
    }

    #[test]
    fn a_card_of_words_is_a_stop_per_row_and_a_disabled_control_keeps_its_words() {
        let card = SplitCard {
            key: "facts".to_string(),
            spec: GroupSpec::new("Facts"),
            padding: Insets::all(10.0),
            gap: 5.0,
            head: Vec::new(),
            repeat: Vec::new(),
            rows: (0..4)
                .map(|row| (words(&format!("fact/{row}")), 20.0))
                .collect(),
        };
        assert_eq!(card.stops(1_000.0), vec![0..1, 1..2, 2..3, 3..4]);

        let mut slider = Control::new(
            crate::native_ui::SliderSpec {
                description: None,
                track: None,
                label: "Brightness".to_string(),
                step: 0.05,
                display_value: "50%".to_string(),
            },
            crate::native_ui::ActionId::new("brightness"),
        );
        slider.state.enabled = false;
        let card = SplitCard {
            key: "color".to_string(),
            spec: GroupSpec::new("Color"),
            padding: Insets::all(10.0),
            gap: 5.0,
            head: Vec::new(),
            repeat: Vec::new(),
            rows: vec![
                (words("wheel"), 100.0),
                (UiNode::new("brightness", UiContent::Slider(slider)), 40.0),
            ],
        };
        assert_eq!(
            card.stops(1_000.0),
            vec![0..2],
            "the wheel keeps with the unavailable slider it belongs to"
        );
    }

    fn chooser_plan(foot: ListFoot) -> ScrollPlan {
        let mut plan = ScrollPlan::new(0.0);
        for index in 0..2 {
            plan.card(32.0, vec![UiKey::new(format!("lead/{index}"))]);
        }
        let section = plan.group(32.0, 32.0);
        for index in 0..6 {
            plan.row(section, 32.0, vec![UiKey::new(format!("row/{index}"))]);
        }
        plan.card(40.0, vec![UiKey::new("disclosure")]);
        for index in 0..2 {
            plan.card(32.0, vec![UiKey::new(format!("footer/{index}"))]);
        }
        plan.set_foot(foot);
        plan.set_room(128.0);
        plan
    }

    #[test]
    fn a_list_that_ends_on_whole_stops_draws_none_cut() {
        let plan = chooser_plan(ListFoot::Whole);
        for top in 0..=plan.limit() {
            let screen = plan.screen(top);
            assert_eq!(screen.drawn, screen.view, "screen {top}");
            assert!(screen.offset.abs() < 0.01, "screen {top}: {screen:?}");
            assert!(
                plan.span(screen.drawn.clone()) <= plan.room() + 0.01,
                "screen {top}: {screen:?}"
            );
            assert_eq!(plan.page_up(top).1, None, "Page Up from {top}");
            for index in screen.view.clone() {
                assert_eq!(plan.press_nudge(index, top), None, "{index} on {top}");
            }
            let node = scroll_list_screen(
                "list",
                "List",
                &plan,
                screen.view.clone(),
                screen.drawn.clone(),
                Vec::new(),
            );
            let thumb = node
                .children
                .iter()
                .find(|child| child.key.as_str() == "list/thumb")
                .expect("a scrolled list has its thumb");
            assert_eq!(thumb.layout.height, Length::Fixed(plan.room()), "{top}");
        }
        let last = plan.screen(plan.limit());
        assert_eq!(last.view.end, plan.len(), "the last screen ends the list");
        let peek = chooser_plan(ListFoot::Peek);
        assert!(
            (0..=peek.limit()).any(|top| {
                let screen = peek.screen(top);
                peek.span(screen.drawn.clone()) > peek.room() + 0.01
            }),
            "the control draws past the room"
        );
        assert!(
            peek.screen(peek.limit()).offset > 0.01,
            "the control's last screen is scrolled into its first stop"
        );
        assert!(
            (0..=peek.limit()).any(|top| {
                let screen = peek.screen(top);
                (screen.view.end..screen.drawn.end)
                    .any(|index| peek.press_nudge(index, top).is_some())
            }),
            "a press on the control's cut stop nudges it past a stop"
        );
    }

    #[test]
    fn a_press_on_a_control_drawn_only_in_the_strip_nudges_it_to_the_rooms_edge() {
        let mut plan = ScrollPlan::new(2.0);
        for index in 0..5 {
            plan.card(40.0, vec![UiKey::new(format!("control/{index}"))]);
        }
        seat_room(&mut plan, 100.0, Overhang { now: 60.0 });
        let screen = plan.screen(0);
        assert_eq!(screen.view, 0..2, "two cards in the room");
        assert!(
            screen.drawn.contains(&3),
            "the fourth is drawn in the strip"
        );
        assert_eq!(
            plan.press_nudge(3, 0),
            Some(66.0),
            "126..166 becomes 60..100"
        );
        plan.seated = Some(Seat::nudged(0, 66.0));
        assert!(plan.shows_controls(3, 0), "whole in the room");
        assert_eq!(plan.press_nudge(3, 0), None, "whole moves nothing");
    }

    #[test]
    fn a_screen_draws_on_to_the_first_stop_its_edge_cuts() {
        let plan = cards(&[40.0; 5], 100.0, 2.0);
        let view = plan.view(0);
        assert_eq!(view, 0..2, "82 of 100 holds two cards");
        assert_eq!(drawn_stops(&plan, &view), 0..3, "the third peeks 16pt");
        let shallow = cards(&[40.0; 5], 100.0, 9.0);
        assert_eq!(
            drawn_stops(&shallow, &shallow.view(0)),
            0..2,
            "a card that would show 2pt is not drawn"
        );
        let node = scroll_list_screen("list", "List", &plan, view, 0..3, Vec::new());
        assert_eq!(
            node.layout.height,
            Length::Fixed(plan.room()),
            "the peek is cut at the list's room"
        );

        let mut pressed = ScrollPlan::new(2.0);
        for index in 0..5 {
            pressed.card(40.0, vec![UiKey::new(format!("control/{index}"))]);
        }
        pressed.set_room(100.0);
        assert!(!pressed.shows_controls(2, 0));
        let nudge = pressed.press_nudge(2, 0).expect("the third control is cut");
        assert_eq!(nudge, 24.0); // 84..124 becomes 60..100.
        assert!(pressed.nudge_fits(0, nudge));
        pressed.seated = Some(Seat::nudged(0, nudge));
        assert!(pressed.shows_controls(2, 0));
        assert_eq!(pressed.press_nudge(2, 0), None, "whole moves nothing");
        let shown = pressed.view(0);
        let drawn = drawn_stops(&pressed, &shown);
        let node = scroll_list_screen("pressed", "List", &pressed, shown, drawn, Vec::new());
        let list = node
            .children
            .iter()
            .find(|node| node.key.as_str() == "pressed")
            .unwrap();
        assert_eq!(list.layout.offset, 24.0, "the list uses the same nudge");
        pressed.seated = None;
        assert!(!pressed.shows_controls(2, 0), "seating on stops drops it");
        pressed.set_room(300.0);
        assert!(!pressed.nudge_fits(0, nudge), "a whole list needs no nudge");
    }

    #[test]
    fn a_screen_whose_edge_falls_between_cards_ends_on_the_card_before() {
        let mut plan = ScrollPlan::new(5.0);
        let group = plan.group(40.0, 20.0);
        plan.row(group, 60.0, Vec::new());
        plan.row(group, 40.0, Vec::new());
        plan.card(50.0, Vec::new());
        plan.set_room(145.0);
        let view = plan.view(0);
        assert_eq!(view, 0..2);
        let screen = plan.screen(0);
        assert_eq!(screen.drawn, 0..2);
        assert_eq!(screen.height, plan.room(), "drawn on to the edge");
        assert!(
            plan.card_bottom(0, 1) >= plan.room() - plan.gap() - 0.01,
            "blank under it only the gap"
        );
        assert_eq!(plan.shown(0), 0..2, "the next screen starts after it");
        let whole = cards(&[60.0, 60.0, 40.0], 130.0, 5.0);
        let screen = whole.screen(0);
        assert_eq!(screen.drawn, 0..2);
        assert_eq!(screen.height, whole.room());
        let mut fading = plan.clone();
        seat_room(&mut fading, 105.0, Overhang { now: 40.0 });
        let screen = fading.screen(0);
        assert_eq!((screen.view, screen.drawn), (0..1, 0..2));
        assert_eq!(screen.height, 145.0, "drawn on to the page's bottom");
    }

    #[test]
    fn a_caption_is_kept_while_its_rows_scroll_under_it_and_a_nudge_never_brings_one_back() {
        let mut plan = ScrollPlan::new(10.0);
        let group = plan.group(50.0, 40.0);
        plan.set_row_gap(group, 4.0);
        plan.set_caption(group, 14.0, 36.0);
        plan.row(group, 66.0, vec![UiKey::new("a")]);
        plan.row(group, 66.0, vec![UiKey::new("b")]);
        plan.set_room(200.0);
        let tail = plan.top_caption(1..2, 64.0);
        assert_eq!(
            tail, None,
            "64pt into a 62pt run: let go (past its words, so none to hide)"
        );
        assert_eq!(
            plan.top_caption(1..2, 48.0).map(|caption| caption.kept),
            None
        );
        let mut gapless = plan.clone();
        gapless.set_row_gap(group, 0.0);
        assert_eq!(
            gapless.top_caption(1..2, 48.0).map(|caption| caption.kept),
            Some(true)
        );
        assert_eq!(
            gapless.top_caption(1..2, 64.0).map(|caption| caption.kept),
            None
        );
        assert_eq!(
            plan.top_caption(1..2, 44.0),
            Some(TopCaption {
                lead: 40.0,
                kept: true
            })
        );
        assert_eq!(
            plan.top_caption(0..2, 64.0).map(|caption| caption.kept),
            Some(true),
            "two rows under it: kept"
        );
        let let_go = Screen {
            view: 1..2,
            drawn: 1..2,
            offset: 64.0,
            height: 200.0,
            live: 200.0,
            fades: false,
            whole: false,
            caption: tail,
            edge_caption: None,
            bare: false,
        };
        let nudged = plan.nudged(let_go.clone(), -20.0, false);
        assert_eq!(nudged.offset, 44.0);
        assert!(
            nudged.caption.is_none_or(|caption| !caption.kept),
            "{:?}",
            nudged.caption
        );
        assert_eq!(plan.caption_floor(nudged.caption, &nudged.drawn, 1), 0.0);
        let kept = Screen {
            offset: 44.0,
            caption: plan.top_caption(1..2, 44.0),
            ..let_go
        };
        let nudged = plan.nudged(kept, -2.0, false);
        assert_eq!(nudged.caption.map(|caption| caption.kept), Some(true));
        assert_eq!(plan.caption_floor(nudged.caption, &nudged.drawn, 1), 40.0);
    }

    #[test]
    fn a_hard_edge_cuts_no_control_live_and_no_caption_through() {
        let plan = |caption: bool| {
            let mut plan = ScrollPlan::new(10.0);
            plan.card(100.0, Vec::new());
            let group = plan.group(52.0, 40.0);
            if caption {
                plan.set_caption(group, 12.0, 34.0);
            }
            plan.row(group, 44.0, vec![UiKey::new("a")]);
            plan.row(group, 44.0, vec![UiKey::new("b")]);
            plan
        };
        let mut cut = plan(true);
        cut.set_room(130.0);
        let screen = cut.screen(0);
        assert_eq!((screen.view, screen.drawn), (0..1, 0..2));
        assert_eq!(screen.height, 130.0, "the card goes on to the edge");
        assert_eq!(
            screen.edge_caption,
            Some((20.0, 34.0)),
            "the caption the edge cuts paints none of its words"
        );
        let mut past = plan(true);
        past.set_room(170.0);
        let screen = past.screen(0);
        assert_eq!(
            (screen.height, screen.live, screen.edge_caption),
            (170.0, 150.0, None),
            "past the caption: the row the edge cuts fades from its top"
        );
        let mut fading = plan(true);
        seat_room(&mut fading, 100.0, Overhang { now: 30.0 });
        let screen = fading.screen(0);
        assert_eq!(
            (screen.height, screen.edge_caption),
            (130.0, None),
            "a fading strip draws on to the page's bottom"
        );
        let mut unknown = plan(false);
        unknown.set_room(130.0);
        let screen = unknown.screen(0);
        assert_eq!((screen.height, screen.edge_caption), (130.0, None));
    }

    #[test]
    fn a_stop_taller_than_the_page_is_cut_by_the_lists_edge() {
        let tall = cards(&[150.0, 40.0], 100.0, 10.0);
        assert_eq!(tall.view(0), 0..1, "the first stop is shown");
        assert_eq!(tall.screen(0).height, 100.0, "cut at the list's room");
        let alone = cards(&[150.0], 100.0, 10.0);
        assert_eq!(alone.screen(0).height, 100.0, "a lone stop too");
        let fits = cards(&[90.0], 100.0, 10.0);
        assert_eq!(fits.screen(0).height, 90.0);
    }

    #[test]
    fn the_last_screen_is_full_with_the_stop_before_it_cut_at_the_top() {
        let plan = cards(&[40.0, 40.0, 40.0, 40.0, 40.0], 100.0, 10.0);
        let last = plan.view(plan.limit());
        assert_eq!(last, 3..5);
        let drawn = drawn_stops(&plan, &last);
        assert_eq!(drawn, 2..5, "the stop before the last screen peeks over it");
        let node = scroll_list_screen("list", "List", &plan, last, drawn, Vec::new());
        let list = node
            .children
            .iter()
            .find(|child| child.key.as_str() == "list")
            .expect("the list");
        assert_eq!(list.layout.height, Length::Fixed(100.0));
        assert_eq!(list.layout.offset, plan.span(2..5) - 100.0);
        let exact = cards(&[50.0, 40.0, 50.0], 100.0, 10.0);
        let last = exact.view(exact.limit());
        assert_eq!(
            drawn_stops(&exact, &last),
            last,
            "a last screen its stops fill draws nothing over them"
        );

        let mut pressed = ScrollPlan::new(10.0);
        for index in 0..5 {
            pressed.card(40.0, vec![UiKey::new(format!("control/{index}"))]);
        }
        pressed.set_room(110.0);
        let top = pressed.limit();
        assert_eq!(pressed.view(top), 3..5);
        assert_eq!(pressed.screen(top).offset, 30.0);
        assert!(!pressed.shows_controls(2, top));
        let nudge = pressed.press_nudge(2, top).expect("the top control is cut");
        assert_eq!(nudge, -30.0); // -30..10 becomes 0..40.
        assert!(pressed.nudge_fits(top, nudge));
        assert!(!pressed.nudge_fits(top, -31.0));
        pressed.seated = Some(Seat::nudged(top, nudge));
        assert!(pressed.shows_controls(2, top));
        assert_eq!(pressed.press_nudge(2, top), None, "whole moves nothing");
        assert_eq!(pressed.screen(top).offset, 0.0);
        assert_eq!(
            pressed.press_nudge(pressed.len(), top),
            None,
            "absent stop refuses"
        );
    }

    #[test]
    fn a_pages_own_list_draws_its_thumb_in_the_page_gutter() {
        use crate::native_ui::{LogicalRect, UiTree};
        use crate::pane_chrome::{SplitChrome, with_split_chrome};
        use crate::widget::DrawPrim;
        let (page_w, gutter) = (900.0_f32, 75.0_f32);
        let edge = page_w - gutter;
        let plan = cards(&[120.0; 6], 300.0, 12.0);
        let view = plan.view(1);
        let drawn = drawn_stops(&plan, &view);
        let compile = |seated: bool| {
            let children = drawn
                .clone()
                .map(|stop| {
                    UiNode::new(
                        format!("card-{stop}"),
                        UiContent::Group(GroupSpec::new("Card").style(StyleRef::Secondary)),
                    )
                    .layout(Layout::column().height(Length::Fixed(120.0)))
                })
                .collect();
            let screen = scroll_list_screen(
                "settings/list",
                "List",
                &plan,
                view.clone(),
                drawn.clone(),
                children,
            );
            let mut page_children = vec![screen];
            if seated {
                seat_page_scroller(&mut page_children, gutter);
            }
            let page = UiNode::new("page", UiContent::Group(GroupSpec::new("Page")))
                .layout(
                    Layout::column()
                        .padding(Insets {
                            top: 20.0,
                            right: gutter,
                            bottom: 20.0,
                            left: 30.0,
                        })
                        .clipped(),
                )
                .children(page_children);
            UiTree::new(page)
                .compile(LogicalRect::new(0.0, 0.0, page_w, 400.0))
                .expect("compiles")
        };
        let thumbs = |compiled: &crate::native_ui::CompiledUi| -> Vec<(f32, f32, [u8; 4])> {
            compiled
                .tray(aterm_render::Theme::default(), 13.0)
                .prims
                .iter()
                .filter_map(|prim| match prim {
                    DrawPrim::Panel { x, w, h, fill, .. } if *w <= 3.0 && *h > 0.0 => {
                        Some((*x, *w, *fill))
                    }
                    _ => None,
                })
                .collect()
        };
        let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
        for (chrome, width) in [(SplitChrome::Fluent, 2.0_f32), (SplitChrome::Classic, 3.0)] {
            let seated = compile(true);
            let thumb = seated
                .paint
                .iter()
                .find(|node| node.key.as_str() == "settings/list/thumb")
                .expect("a scrolled list has its thumb");
            assert!(
                (thumb.rect.right() - page_w).abs() < 0.01
                    && (thumb.clip.right() - page_w).abs() < 0.01,
                "{chrome:?}: the thumb's node spans the gutter: {:?} {:?}",
                thumb.rect,
                thumb.clip
            );
            for node in seated
                .paint
                .iter()
                .filter(|node| node.key.as_str().starts_with("card-"))
            {
                assert!(
                    node.rect.right() <= edge + 0.01,
                    "{chrome:?}: {} keeps to the content edge: {:?}",
                    node.key.as_str(),
                    node.rect
                );
            }
            let painted = with_split_chrome(chrome, || thumbs(&seated));
            let alpha = crate::scrollbar::rest_alpha(crate::scrollbar::ALPHA_REST, true);
            let [r, g, b] =
                crate::scrollbar::over_ground(roles.text_primary, roles.surface, alpha, alpha).0;
            assert_eq!(
                painted,
                vec![(page_w - 2.0 - width, width, [r, g, b, 255])],
                "{chrome:?}: the terminal's thumb and resting tone, 2 pt from the page's edge"
            );
            let unseated = with_split_chrome(chrome, || thumbs(&compile(false)));
            assert!(
                unseated.len() == 1 && unseated[0].0 + unseated[0].1 <= edge,
                "{chrome:?}: CONTROL: unseated, inside the cards' column: {unseated:?}"
            );
        }
        let mut card_list = vec![
            UiNode::new(
                "settings/messages/scroll",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .children(vec![
                UiNode::new(
                    "settings/messages",
                    UiContent::Group(GroupSpec::new("Log").style(StyleRef::Secondary)),
                ),
                UiNode::new(
                    "settings/messages/thumb",
                    UiContent::Custom(AuditedCustomNode {
                        audit_id: MESSAGES_SCROLL_AUDIT,
                        role: SemanticRole::Group,
                        label: String::new(),
                        paint: Some("0,2,5".to_string()),
                        action: None,
                        focusable: false,
                        focused: false,
                        focus_visible: false,
                    }),
                )
                .paint_only(),
            ]),
        ];
        seat_page_scroller(&mut card_list, gutter);
        assert_eq!(
            card_list[0].layout.outdent,
            Insets::default(),
            "a card's list"
        );
    }

    #[test]
    fn a_group_run_pays_its_caption_on_every_screen() {
        let mut plan = ScrollPlan::new(10.0);
        let card = plan.card(30.0, Vec::new());
        let group = plan.group(50.0, 30.0);
        for _ in 0..4 {
            plan.row(group, 48.0, Vec::new());
        }
        plan.set_room(200.0);
        assert_eq!(plan.span(1..3), 50.0 + 96.0);
        assert_eq!(plan.span(0..2), 30.0 + 10.0 + 50.0 + 48.0);
        assert_eq!(plan.view(1), 1..4);
        assert_eq!(plan.view(plan.limit()), 2..5);
        assert_eq!(
            plan.runs(0..5),
            vec![
                ScrollRun::Card(card),
                ScrollRun::Group { group, rows: 1..5 }
            ]
        );
    }

    #[test]
    fn a_screen_lets_its_caption_go_where_it_and_the_row_cannot_share_the_room() {
        let mut plan = ScrollPlan::new(4.0);
        let group = plan.group(60.0, 50.0);
        plan.set_row_gap(group, 4.0);
        plan.set_caption(group, 14.0, 46.0);
        for key in ["a", "b", "c"] {
            plan.row(group, 68.0, vec![UiKey::new(key)]);
            plan.set_control_rows(vec![(0.0, 0.0, 64.0)]);
            plan.set_ink(64.0);
        }
        plan.set_room(100.0);
        for stop in 0..3 {
            assert!(plan.starts_bare(stop), "row {stop}: 50pt + 64pt > 100pt");
            let screen = plan.screen(stop);
            assert!(
                screen.bare && (screen.offset >= 50.0 - 0.01 || stop == plan.limit()),
                "{screen:?}"
            );
            assert!(
                screen.caption.is_none_or(|caption| !caption.kept),
                "{screen:?}"
            );
            assert!(plan.shown(stop).contains(&stop), "row {stop} is whole");
            assert!(plan.shows_controls(stop, stop));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut top = 0;
        loop {
            seen.extend(plan.shown(top));
            let next = plan.page_after(top);
            if next == top {
                break;
            }
            top = next;
        }
        assert_eq!(seen.len(), 3, "Page Down shows every row whole: {seen:?}");
        let mut fits = ScrollPlan::new(4.0);
        let group = fits.group(60.0, 50.0);
        fits.row(group, 44.0, vec![UiKey::new("a")]);
        fits.row(group, 44.0, vec![UiKey::new("b")]);
        fits.set_room(100.0);
        assert!(!fits.starts_bare(0) && !fits.starts_bare(1));
        assert!(!fits.screen(0).bare);
    }

    #[test]
    fn a_stop_a_bare_screen_shows_whole_is_not_cut() {
        let rows = |height: f32| {
            let mut plan = ScrollPlan::new(4.0);
            let group = plan.group(60.0, 50.0);
            plan.set_row_gap(group, 4.0);
            plan.set_caption(group, 14.0, 46.0);
            for key in ["a", "b", "c"] {
                plan.row(group, height + 6.0, vec![UiKey::new(key)]);
                plan.set_control_rows(vec![(0.0, 44.0, height - 44.0)]);
                plan.set_ink(height);
            }
            plan.set_room(156.0);
            plan
        };
        let fits = rows(108.0);
        assert!((0..3).all(|stop| fits.starts_bare(stop)), "50 + 108 > 156");
        assert_eq!(fits.cut_stops(), 0, "each row is whole on its bare screen");
        let tall = rows(160.0);
        assert_eq!(tall.cut_stops(), 3, "a row taller than the room is cut");
    }

    #[test]
    fn the_last_screen_lets_its_caption_go_rather_than_cut_its_stop() {
        let mut plan = ScrollPlan::new(12.0);
        let first = plan.group(60.0, 50.0);
        plan.row(first, 84.0, vec![UiKey::new("a")]);
        plan.set_control_rows(vec![(0.0, 4.0, 72.0)]);
        plan.set_ink(76.0);
        plan.set_ink_top(4.0);
        let last = plan.group(60.0, 50.0);
        plan.set_caption(last, 14.0, 46.0);
        plan.row(last, 84.0, vec![UiKey::new("badge")]);
        plan.set_control_rows(vec![(0.0, 4.0, 72.0)]);
        plan.set_ink(76.0);
        plan.set_ink_top(4.0);
        plan.set_room(132.0);
        let top = plan.limit();
        assert_eq!(top, 1);
        let screen = plan.screen(top);
        assert!(screen.offset > 0.01, "scrolled to the card's end");
        assert!(
            screen.caption.is_none_or(|caption| !caption.kept),
            "{screen:?}"
        );
        assert!(plan.shown(top).contains(&1), "the badge row is whole");
    }

    #[test]
    fn a_reveal_by_stops_keeps_the_stop_it_reveals_on_screen() {
        let mut plan = ScrollPlan::new(10.0);
        plan.card(150.0, vec![UiKey::new("tall")]);
        plan.set_ink(140.0);
        plan.card(40.0, vec![UiKey::new("next")]);
        plan.card(40.0, vec![UiKey::new("last")]);
        plan.set_room(120.0);
        assert!(!plan.shows_span(0, 0, (0.0, 150.0)));
        assert_eq!(plan.reveal_span(0, 0, (0.0, 150.0)), 0);
        assert_eq!(plan.land_selected(0, 0), (0, None));
    }

    fn adjust_card(now: f32) -> ScrollPlan {
        let mut plan = ScrollPlan::new(10.0);
        for key in ["a", "b", "c"] {
            plan.card(100.0, vec![UiKey::new(key)]);
        }
        let group = plan.group(52.0, 40.0);
        for (key, height) in [("r0", 50.0), ("r1", 50.0), ("r2", 50.0), ("r3", 68.0)] {
            plan.row(group, height, vec![UiKey::new(key)]);
            plan.set_control_rows(vec![(0.0, 4.0, height - 8.0)]);
            plan.set_ink(height - 4.0);
            plan.set_ink_top(4.0);
        }
        plan.row(group, 72.0, vec![UiKey::new("r4")]);
        plan.set_control_rows(vec![(0.0, 4.0, 36.0)]);
        plan.set_ink(70.0);
        plan.set_ink_top(4.0);
        seat_room(&mut plan, 326.0, Overhang { now });
        plan
    }

    #[test]
    fn the_screen_that_draws_the_lists_end_is_its_last() {
        let plan = adjust_card(44.0);
        assert_eq!(plan.limit(), 3, "the screen seated at the card's first row");
        let screen = plan.screen(3);
        let end = plan.card_bottom(screen.drawn.start, plan.len() - 1) - screen.offset;
        assert!(
            (end - (plan.room() + plan.overhang())).abs() < 0.01 && screen.drawn.start < 3,
            "the list's end at the strip's foot, the card before peeking in: {screen:?} {end}"
        );
        assert_eq!(plan.shown(3), 3..8, "it shows every row, the last too");
        assert_eq!(plan.page_after(2), 3);
        let before = plan.shown(2);
        assert!(
            plan.shown(3).any(|stop| !before.contains(&stop)),
            "the turn shows what the screen before it did not"
        );
        let barred = adjust_card(0.0);
        let last = barred.limit();
        let screen = barred.screen(last);
        let end = barred.card_bottom(screen.drawn.start, barred.len() - 1) - screen.offset;
        assert!(
            (end - barred.room()).abs() < 0.01,
            "with a bar up the list ends at the room's edge: {end}"
        );
    }

    #[test]
    fn with_a_bar_up_the_last_screen_shows_the_last_card_whole() {
        let mut plan = ScrollPlan::new(10.0);
        for key in ["a", "b", "c", "d"] {
            plan.card(80.0, vec![UiKey::new(key)]);
        }
        let group = plan.group(40.0, 24.0);
        plan.row(group, 60.0, vec![UiKey::new("last")]);
        plan.set_control_rows(vec![(0.0, 8.0, 36.0)]);
        plan.set_ink(52.0);
        plan.set_ink_top(8.0);
        let end = |now: f32| {
            let mut plan = plan.clone();
            seat_room(&mut plan, 200.0, Overhang { now });
            let screen = plan.screen(plan.limit());
            let at = plan.stop_top(screen.drawn.start, plan.len() - 1) - screen.offset;
            (
                plan.card_bottom(screen.drawn.start, plan.len() - 1) - screen.offset,
                at + plan.controls_of(plan.len() - 1).1,
                screen.height,
            )
        };
        let (bottom, controls, height) = end(0.0);
        assert!(
            bottom <= 200.0 + 0.01,
            "the last card whole in the room: {bottom}"
        );
        assert!(
            height >= bottom - 0.01,
            "drawn to its end: {height} {bottom}"
        );
        let (bottom, controls_free, _) = end(44.0);
        assert!(bottom <= 244.0 + 0.01 && controls_free <= 200.0 + 0.01);
        assert!(controls <= 200.0 + 0.01);
    }

    #[test]
    fn a_control_over_the_rooms_edge_is_a_picture_from_its_top() {
        let mut plan = ScrollPlan::new(10.0);
        plan.card(200.0, vec![UiKey::new("first")]);
        plan.card(60.0, vec![UiKey::new("row")]);
        plan.set_control_spans(vec![(UiKey::new("row"), 12.0, 36.0)]);
        plan.set_control_rows(vec![(0.0, 12.0, 36.0)]);
        plan.card(60.0, vec![UiKey::new("after")]);
        seat_room(&mut plan, 255.0, Overhang { now: 44.0 });
        let screen = plan.screen(0);
        assert_eq!(plan.shown(0), 0..1);
        assert_eq!(
            screen.live, 222.0,
            "live and at full strength to the switch's top"
        );
        assert!(screen.fades);
        assert!(!plan.draws_controls_live(1, 0), "the switch is a picture");
        assert_eq!(plan.page_after(0), 1, "the next screen shows it");
        let mut card = ScrollPlan::new(10.0);
        card.card(150.0, vec![UiKey::new("lead")]);
        card.card(140.0, vec![UiKey::new("one"), UiKey::new("two")]);
        card.set_control_spans(vec![
            (UiKey::new("one"), 20.0, 44.0),
            (UiKey::new("two"), 72.0, 44.0),
        ]);
        card.set_control_rows(vec![(0.0, 20.0, 96.0)]);
        card.card(60.0, Vec::new());
        seat_room(&mut card, 250.0, Overhang { now: 44.0 });
        assert_eq!(card.screen(0).live, 232.0);
        assert!(card.draws_control_live(1, &UiKey::new("one"), 0));
        assert!(!card.draws_control_live(1, &UiKey::new("two"), 0));
        assert_eq!(
            card.live_control(1, &UiKey::new("one"), 0),
            Some(&UiKey::new("one"))
        );
        assert_eq!(
            card.live_control(1, &UiKey::new("two"), 0),
            Some(&UiKey::new("one"))
        );
        assert_eq!(card.live_control(2, &UiKey::new("two"), 0), None);
        let whole = cards(&[100.0, 100.0, 100.0], 230.0, 10.0);
        assert_eq!(whole.screen(0).live, 230.0);
    }

    /// THE LIST'S END LEAVES NO ROW CUT TO NOTHING UNDER ITS CAPTION: a card whose first row
    /// puts its words over its switch (a compact row at 2x text), and whose end the last
    /// screen anchors so that row's switch is cut away under the caption kept over the rows,
    /// starts that screen at the next row, the room left under the list's end, where it drew
    /// a blank band under the caption. CONTROL: where the switch shows under the caption, the
    /// screen is anchored on the end as before.
    #[test]
    fn the_last_screen_draws_no_row_cut_to_nothing_under_its_caption() {
        let plan = |room: f32| {
            let mut plan = ScrollPlan::new(10.0);
            let group = plan.group(54.0, 40.0);
            plan.set_caption(group, 14.0, 36.0);
            plan.row(group, 108.0, vec![UiKey::new("gpu")]);
            plan.set_control_rows(vec![(0.0, 44.0, 64.0)]);
            plan.set_ink(108.0);
            plan.row(group, 144.0, vec![UiKey::new("rewind")]);
            plan.set_control_rows(vec![(0.0, 80.0, 64.0)]);
            plan.set_ink(144.0);
            seat_room(&mut plan, room, Overhang::NONE);
            plan
        };
        let cut = plan(230.0);
        assert_eq!(cut.limit(), 1);
        let screen = cut.screen(1);
        assert_eq!(
            (screen.drawn.clone(), screen.offset),
            (1..2, 0.0),
            "the cut row is not drawn: {screen:?}"
        );
        assert_eq!(cut.shown(1), 1..2, "the last row shown whole");
        assert!(
            cut.card_bottom(1, 1) < cut.room(),
            "the room left goes under the list's end"
        );
        let shows = plan(268.0);
        let screen = shows.screen(shows.limit());
        assert_eq!(
            screen.drawn.start, 0,
            "a row whose switch shows under the caption is drawn: {screen:?}"
        );
        assert!(screen.offset > 0.0);
    }

    #[test]
    fn a_screen_that_ends_on_a_whole_card_scrolls_on_to_peek_the_next() {
        let plan = |padding: f32| {
            let mut plan = ScrollPlan::new(12.0);
            for key in ["a", "b", "c", "d", "e", "f"] {
                plan.card(136.0, vec![UiKey::new(key)]);
                plan.set_control_rows(vec![(0.0, padding, 136.0 - 2.0 * padding)]);
                plan.set_ink(136.0 - padding);
                plan.set_ink_top(padding);
            }
            seat_room(&mut plan, 300.0, Overhang::NONE);
            plan
        };
        let padded = plan(12.0);
        let screen = padded.screen(1);
        assert_eq!(screen.offset, 12.0, "scrolled on into b's padding");
        assert!(screen.drawn.contains(&3), "d peeks in");
        assert_eq!(
            padded.card_top(screen.drawn.start, 3) - screen.offset,
            300.0 - PEEK_MIN,
            "PEEK_MIN of d under the edge"
        );
        assert_eq!(padded.shown(1), 1..3, "b and c shown whole, as seated");
        assert_eq!(padded.page_after(1), 3);
        assert_eq!(padded.screen(0).offset, 0.0);
        let flush = plan(8.0);
        assert_eq!(flush.screen(1).offset, 0.0);
        assert!(!flush.screen(1).drawn.contains(&3));
    }

    #[test]
    fn a_kept_caption_never_comes_down_over_a_row_the_scroll_left_whole() {
        let mut plan = ScrollPlan::new(10.0);
        plan.card(100.0, vec![UiKey::new("before")]);
        let group = plan.group(54.0, 40.0);
        plan.set_caption(group, 14.0, 36.0);
        for key in ["a", "b", "c", "d"] {
            plan.row(group, 44.0, vec![UiKey::new(key)]);
            plan.set_control_rows(vec![(0.0, 4.0, 36.0)]);
            plan.set_ink(40.0);
            plan.set_ink_top(4.0);
        }
        seat_room(&mut plan, 200.0, Overhang::NONE);
        let seated = plan.stop_screen(1);
        assert_eq!((seated.offset, seated.drawn.start), (0.0, 1));
        let nudged = plan.nudged(seated.clone(), 16.0, false);
        assert!(
            nudged.caption.is_none_or(|caption| !caption.kept),
            "the caption scrolls with its card: {nudged:?}"
        );
        assert_eq!(
            plan.shown_on(&nudged.drawn, nudged.offset, nudged.caption)
                .start,
            1,
            "the row the nudge left whole is shown"
        );
        let past = plan.nudged(seated, 48.0, false);
        assert!(past.caption.is_some_and(|caption| caption.kept), "{past:?}");
    }
}
