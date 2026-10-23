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

/// The strip under a list a page keeps for a status bar ([`seat_room`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Overhang {
    pub(crate) now: f32,
    pub(crate) kept: f32,
}

impl Overhang {
    /// No strip: a list that keeps no room for a bar.
    pub(crate) const NONE: Self = Self {
        now: 0.0,
        kept: 0.0,
    };
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
    heights: Vec<f32>,
    groups: Vec<Option<usize>>,
    heads: Vec<f32>,
    leads: Vec<f32>,
    captions: Vec<Option<(f32, f32)>>,
    card_captions: Vec<Option<CardCaption>>,
    notes: Vec<bool>,
    row_gaps: Vec<f32>,
    keys: Vec<Vec<UiKey>>,
    ids: Vec<Vec<UiKey>>,
    within: Vec<Vec<UiKey>>,
    previews: Vec<Vec<UiKey>>,
    control_rows: Vec<Vec<(f32, f32, f32)>>,
    spans: Vec<Vec<(UiKey, f32, f32)>>,
    ink: Vec<f32>,
    ink_top: Vec<f32>,
    first_controls: Vec<Option<f32>>,
    room: f32,
    overhang: f32,
    kept: f32,
    given: f32,
    held_start: Option<(usize, bool)>,
    gap: f32,
    seated: Option<Seat>,
    segments: Vec<(UiKey, UiKey)>,
    foot: ListFoot,
}

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

impl ScrollPlan {
    pub(super) fn new(gap: f32) -> Self {
        Self {
            gap,
            ..Self::default()
        }
    }

    /// The height the list has.
    pub(super) const fn set_room(&mut self, room: f32) {
        self.room = room;
    }

    /// How the list's screens end at its bottom edge ([`ListFoot`]).
    pub(super) const fn set_foot(&mut self, foot: ListFoot) {
        self.foot = foot;
    }

    /// Say that the list's room is `given` shorter than the page kept it.
    pub(super) const fn give_to_status(&mut self, given: f32) {
        self.given = given.max(0.0);
    }

    /// Whether the list's screens give room up to a plain status's taller bar.
    pub(super) fn gives_to_status(&self) -> bool {
        self.given > 0.01
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

    pub(super) const fn len(&self) -> usize {
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
                    floor: self.caption_floor(screen.caption, &screen.drawn, stop),
                }
            })
            .collect()
    }

    /// A stop that is a whole card, `height` tall, holding `keys`.
    pub(super) fn card(&mut self, height: f32, keys: Vec<UiKey>) -> usize {
        self.heights.push(height);
        self.ink.push(height);
        self.ink_top.push(0.0);
        self.first_controls
            .push((!keys.is_empty()).then_some(height));
        self.previews.push(Vec::new());
        self.card_captions.push(None);
        self.notes.push(false);
        self.groups.push(None);
        self.control_rows.push(if keys.is_empty() {
            Vec::new()
        } else {
            vec![(0.0, 0.0, height)]
        });
        self.keys.push(keys);
        self.ids.push(Vec::new());
        self.within.push(Vec::new());
        self.spans.push(Vec::new());
        self.heights.len() - 1
    }

    /// [`Self::card`] put in at stop `index`, before the stops there.
    pub(super) fn insert_card(&mut self, index: usize, height: f32, keys: Vec<UiKey>) {
        let index = index.min(self.heights.len());
        self.heights.insert(index, height);
        self.ink.insert(index, height);
        self.ink_top.insert(index, 0.0);
        self.first_controls
            .insert(index, (!keys.is_empty()).then_some(height));
        self.previews.insert(index, Vec::new());
        self.card_captions.insert(index, None);
        self.notes.insert(index, false);
        self.groups.insert(index, None);
        self.control_rows.insert(
            index,
            if keys.is_empty() {
                Vec::new()
            } else {
                vec![(0.0, 0.0, height)]
            },
        );
        self.keys.insert(index, keys);
        self.ids.insert(index, Vec::new());
        self.within.insert(index, Vec::new());
        self.spans.insert(index, Vec::new());
    }

    /// A group card.
    pub(super) fn group(&mut self, head: f32, lead: f32) -> usize {
        self.heads.push(head);
        self.leads.push(lead.max(0.0));
        self.captions.push(None);
        self.row_gaps.push(0.0);
        self.heads.len() - 1
    }

    /// Say that `group`'s card sets `gap` between its rows.
    pub(super) fn set_row_gap(&mut self, group: usize, gap: f32) {
        if let Some(row_gap) = self.row_gaps.get_mut(group) {
            *row_gap = gap.max(0.0);
        }
    }

    fn run_rows(&self, group: usize, rows: &Range<usize>) -> f32 {
        let stops = self.heights[rows.clone()].iter().sum::<f32>();
        (stops - self.row_gaps.get(group).copied().unwrap_or(0.0)).max(0.0)
    }

    /// Say that `group`'s card paints a caption from `top` under its top.
    pub(super) fn set_caption(&mut self, group: usize, top: f32, bottom: f32) {
        if let Some(caption) = self.captions.get_mut(group) {
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
        if let Some(first) = self.first_controls.last_mut() {
            *first = rows.first().map(|(_, at, height)| at + height);
        }
        if let Some(last) = self.control_rows.last_mut() {
            *last = rows;
        }
    }

    /// Say how far under its top the last stop added has its first row of controls end.
    pub(super) fn set_first_controls(&mut self, end: Option<f32>) {
        if let (Some(first), Some(end)) = (self.first_controls.last_mut(), end) {
            *first = Some(first.map_or(end, |was| was.min(end)));
        }
    }

    /// Say how far under its top what the last stop added draws ends.
    pub(super) fn set_ink(&mut self, ink: f32) {
        if let (Some(last), Some(height)) = (self.ink.last_mut(), self.heights.last()) {
            *last = ink.clamp(0.0, *height);
        }
    }

    /// Say how far under its top what the last stop added draws begins.
    pub(super) fn set_ink_top(&mut self, top: f32) {
        if let (Some(last), Some(ink)) = (self.ink_top.last_mut(), self.ink.last()) {
            *last = top.clamp(0.0, *ink);
        }
    }

    /// Say that the last stop added is a note on the rows over it.
    pub(super) fn set_note(&mut self, note: bool) {
        if let Some(last) = self.notes.last_mut() {
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
        if let Some(last) = self.spans.last_mut() {
            *last = spans;
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
        let control = self.resolve(key);
        self.spans
            .get(index)?
            .iter()
            .find(|(span, _, _)| span == key || span == control)
            .map(|(_, top, height)| (*top, top + height))
    }

    /// Measure the last stop added from `node`, laid out `height` tall.
    pub(super) fn measure(&mut self, node: &UiNode, height: f32) {
        let mut within = Vec::new();
        keys_within(node, &mut within);
        self.set_within(within);
        self.set_control_spans(control_spans_in(node, height));
        self.set_control_rows(control_rows_in(node, height));
        self.set_first_controls(first_control_row(node, height).map(|(top, tall)| top + tall));
        let content = content_extent(node, height);
        self.set_ink(content.map_or(0.0, |(top, tall)| top + tall));
        self.set_ink_top(content.map_or(0.0, |(top, _)| top));
        if let Some(last) = self.card_captions.last_mut() {
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
        self.heights.push(height);
        self.ink.push(height);
        self.ink_top.push(0.0);
        self.first_controls
            .push((!keys.is_empty()).then_some(height));
        self.previews.push(Vec::new());
        self.card_captions.push(None);
        self.notes.push(false);
        self.groups.push(Some(group));
        self.control_rows.push(if keys.is_empty() {
            Vec::new()
        } else {
            vec![(0.0, 0.0, height)]
        });
        self.keys.push(keys);
        self.ids.push(Vec::new());
        self.within.push(Vec::new());
        self.spans.push(Vec::new());
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
        let runs = self.runs(range);
        runs.iter().map(|run| self.run_height(run)).sum::<f32>()
            + runs.len().saturating_sub(1) as f32 * self.gap
    }

    /// The stops a screen seated at its first stop `top` SHOWS.
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

    /// The last first stop the list scrolls to.
    pub(super) fn limit(&self) -> usize {
        let n = self.len();
        if n == 0 {
            return 0;
        }
        let mut top = n - 1;
        while top > 0 && self.ends_on(top - 1) {
            top -= 1;
        }
        top
    }

    /// Whether the screen whose first stop is `top`.
    pub(super) fn ends_on(&self, top: usize) -> bool {
        let n = self.len();
        if self.foot == ListFoot::Whole {
            return self.view(top).end >= n;
        }
        let screen = self.last_screen(top);
        if screen.bare && !self.starts_bare(top) {
            return false;
        }
        let shown = self.shown_on(&screen.drawn, screen.offset, screen.caption);
        !shown.is_empty() && shown.start <= top && shown.end >= n
    }

    fn last_screen(&self, top: usize) -> Screen {
        let n = self.len();
        let top = top.min(n - 1);
        let reach = self.room + self.overhang;
        let anchor = (self.room + self.end_under(n - 1) - self.controls_of(n - 1).1)
            .min(reach)
            .max(self.room.min(reach));
        let mut start = top;
        while start > 0 && self.span(start..n) + 0.5 < anchor {
            start -= 1;
        }
        let bottom = self.span(start..n);
        let offset = (bottom - anchor).max(0.0);
        let height = (bottom - offset).min(reach).max(self.room.min(bottom));
        let bare = start == top && (self.starts_bare(start) || offset > self.ink_top[start] + 0.01);
        let caption = self
            .top_caption(start..n, offset)
            .map(|caption| TopCaption {
                kept: caption.kept && !bare,
                ..caption
            });
        let drawn = start..n;
        Screen {
            view: top..n,
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
        self.control_rows[index]
            .iter()
            .map(|(_, at, height)| (*at, at + height))
            .reduce(|(top, bottom), (at, end)| (top.min(at), bottom.max(end)))
            .unwrap_or((0.0, self.ink[index]))
    }

    /// THE FIRST SCREEN'S LEAD ([`super::PageChrome`]).
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
        if index + 1 == self.len() {
            let top = self.ink_top[index];
            return self.controls_of(index).1 - top > self.room + 0.01
                || self.end_under(index) - top > self.room + self.overhang + 0.01;
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
        fn walk(node: &UiNode, out: &mut Vec<(UiKey, UiKey)>) {
            if node.paint_only {
                return;
            }
            if node.segmented {
                if let Some(control) = segmented_control_key(node) {
                    out.extend(
                        crate::native_ui::segments_of(&node.children)
                            .into_iter()
                            .filter(|(segment, _)| *segment != control)
                            .map(|(segment, _)| (segment, control.clone())),
                    );
                }
                return;
            }
            for child in &node.children {
                walk(child, out);
            }
        }
        walk(node, &mut self.segments);
    }

    /// The control `key` is to the list.
    pub(super) fn resolve<'a>(&'a self, key: &'a UiKey) -> &'a UiKey {
        self.segments
            .iter()
            .find(|(segment, _)| segment == key)
            .map_or(key, |(_, control)| control)
    }

    /// [`Self::reveal_span`] for a landing that SELECTS stop `index` ([`Reveal::LandWhole`]).
    pub(super) fn reveal_selected(&self, index: usize, top: usize) -> usize {
        self.reveal_span(index, top, (0.0, self.heights[index]))
    }

    /// Lands on selected stop `index` and draws it whole where room permits ([`Reveal::LandWhole`]).
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

    /// THE SEAT THAT SHOWS WHAT THE STOPS CANNOT (the final check of 2026-10-20, F1).
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
        while first < index
            && self.stop_top(first, index) - self.seat_drop(first) + to > self.room + 0.01
        {
            first += 1;
        }
        first.min(limit)
    }

    /// Whether the screen whose first stop is `top`.
    pub(super) fn shows_span(&self, index: usize, top: usize, span: (f32, f32)) -> bool {
        self.shows_span_on(&self.screen(top.min(self.limit())), index, span)
    }

    fn shows_span_on(&self, screen: &Screen, index: usize, (from, to): (f32, f32)) -> bool {
        self.live_band(screen, index)
            .is_some_and(|(at, floor, live)| at + from >= floor - 0.01 && at + to <= live + 0.01)
    }

    fn shown_of_span(&self, index: usize, top: usize, (from, to): (f32, f32)) -> f32 {
        self.live_band(&self.screen(top.min(self.limit())), index)
            .map_or(0.0, |(at, floor, live)| {
                ((at + to).min(live) - (at + from).max(floor)).max(0.0)
            })
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

    /// Whether the screen whose first stop is `top`.
    pub(super) fn shows_landing(&self, index: usize, top: usize) -> bool {
        self.shows_revealed(index, top, Reveal::Keep)
    }

    /// Whether the screen whose first stop is `top`.
    #[cfg(test)]
    pub(super) fn shows_controls(&self, index: usize, top: usize) -> bool {
        self.shows_span(index, top, self.controls_of(index))
    }

    /// Whether the screen whose first stop is `top` ([`Reveal::Keep`]).
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

    /// Whether the screen whose first stop is `top`.
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

    /// Whether the screen whose first stop is `top`.
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
        spans.any(|(from, height)| {
            at + from < live - 0.5 && at + from >= floor - 0.5 && at + from + height > floor + 0.5
        })
    }

    /// What the screen whose first stop is `top` draws (THE ONE MEASURE).
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
            _ => screen,
        }
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
            return Screen {
                drawn: view.clone(),
                view,
                offset: 0.0,
                height: self.room,
                live: base,
                fades: false,
                whole: false,
                caption: None,
                edge_caption: None,
                bare: false,
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

    fn peek_need(&self, screen: &Screen) -> Option<(f32, f32)> {
        let next = screen.drawn.end;
        let first = screen.view.start;
        if next >= self.len() || self.gap <= 0.01 || !self.starts_run(next) {
            return None;
        }
        let reach = self.room + self.kept;
        let shows = reach - (self.card_top(screen.drawn.start, next) - screen.offset);
        let padding = match self.groups[first] {
            Some(group) => self.captions[group]
                .map_or(self.ink_top[first], |(top, _)| top.min(self.lead_of(first))),
            None => self.ink_top[first],
        };
        Some((PEEK_MIN - shows, padding))
    }

    fn peek_seat(&self, screen: Screen) -> Screen {
        if screen.view.start == 0 || screen.offset > 0.01 || screen.bare {
            return screen;
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

    /// A WHOLE STOP BEATS ITS CAPTION.
    pub(super) fn starts_bare(&self, top: usize) -> bool {
        let Some(group) = self.groups.get(top).copied().flatten() else {
            return false;
        };
        if let Some((at, bare)) = self.held_start
            && at == top
        {
            return bare;
        }
        let lead = self.leads[group];
        let room = self.room + self.given;
        self.foot == ListFoot::Peek
            && lead > 0.01
            && !self.shows_at(lead, top, room)
            && self.shows_at(0.0, top, room)
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
            if let Some((from, _)) = self
                .control_lines(stop)
                .into_iter()
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

    /// THE STOPS THE SCREEN WHOSE FIRST STOP IS `top` SHOWS.
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

    /// PAGE DOWN.
    pub(super) fn page_after(&self, top: usize) -> usize {
        self.shown(top).end.min(self.limit())
    }

    /// PAGE UP from the screen whose first stop is `top`.
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
                let kept =
                    offset > padding + 0.01 && offset < drawn_end - PEEK_MIN + 0.01 && !row_whole;
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

    /// THE PRESS'S NUDGE.
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
            return false;
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
        let bare = bare || screen.bare;
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
        let mut end = screen.drawn.end;
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
        if let Some(last) = self.ids.len().checked_sub(1) {
            self.set_ids_of(last, ids);
        }
    }

    /// [`Self::set_ids`] for stop `index`.
    pub(super) fn set_ids_of(&mut self, index: usize, ids: Vec<UiKey>) {
        if let Some(stop) = self.ids.get_mut(index) {
            *stop = ids;
        }
    }

    /// Say what else the last stop added draws ([`Self::within`]).
    pub(super) fn set_within(&mut self, within: Vec<UiKey>) {
        if let Some(last) = self.within.last_mut() {
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
        let mut keys = Vec::new();
        for node in nodes {
            collect(node, &mut keys);
        }
        if let Some(last) = self.previews.last_mut() {
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
}

impl Left {
    /// What the screen whose first stop is `top` shows of `key` on `plan`.
    fn on(plan: &ScrollPlan, key: &UiKey, top: usize) -> Self {
        let (whole, part) = plan.stop_of(key).map_or((true, 0.0), |stop| {
            let span = plan.span_of(stop, Some(key), Reveal::Pressed);
            let control = plan
                .control_span(stop, key)
                .unwrap_or_else(|| plan.controls_of(stop));
            (
                plan.shows_span(stop, top, span),
                plan.shown_of_span(stop, top, control),
            )
        });
        Self {
            key: key.clone(),
            whole,
            part,
        }
    }

    /// Whether `now` shows the person no less of the control than this.
    fn kept_by(&self, now: &Self) -> bool {
        now.whole || (!self.whole && now.part + 0.5 >= self.part)
    }

    /// Whether this shows any of the control.
    fn shows(&self) -> bool {
        self.whole || self.part > 0.0
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
    hold: bool,
    /// Prior screen plan and scroll held across re-measurements ([`SettingsViewState::seat_scroll`],
    /// [`SettingsViewState::settle_page_scroll`], [`SettingsViewState::drop_list_nudge`]).
    place: Option<Box<(ScrollPlan, usize)>>,
}

impl ScrollRecord {
    /// The stop the list's screen started at.
    pub(super) const fn top(&self) -> usize {
        self.top
    }

    /// Whether `state` shows the list this record was drawn for.
    pub(super) fn same_context(&self, state: &SettingsViewState) -> bool {
        self.context == state.scroll_context()
    }

    fn unscrolled(&self, state: &SettingsViewState) -> bool {
        self.context == state.scroll_context()
            && self.top == state.scroll_now().min(self.plan.limit())
    }

    /// What the person was left with of `key`, the keyboard's control ([`Self::left`]).
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

    /// THE RESET ALL QUESTION SCROLLS ON ITS OWN in a compact window.
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
    pub(super) fn seat_scroll(&self, plan: &mut ScrollPlan) -> Range<usize> {
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
        let owed = |key: &UiKey| {
            let record = self.scroll_record.borrow();
            record
                .as_ref()
                .filter(|record| record.unscrolled(self))
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
            .map(|record| (top, record.plan.starts_bare(record.top)));
        plan.held_start = held_start;
        let first_screen = self
            .scroll_record
            .borrow()
            .as_ref()
            .is_none_or(|record| record.context != self.scroll_context());
        let opening = first_screen
            || self.scroll_record.borrow().as_ref().is_some_and(|record| {
                record.opening && record.drawn.is_some() && record.context == self.scroll_context()
            });
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
        let revealing = self
            .page_reveal
            .as_ref()
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
            plan.held_start = None;
        }
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
                if !first_screen && owes(key, plan, top) {
                    (top, plan.seated) = (place, seat);
                }
            }
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
                record
                    .as_ref()
                    .filter(|record| record.unscrolled(self))
                    .map(|record| {
                        record
                            .place
                            .clone()
                            .unwrap_or_else(|| Box::new((record.plan.clone(), record.top)))
                    })
            })
            .flatten();
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
            landed,
            hold: true,
            place,
        }));
        view
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

    /// Where the screen that put the list where the last render held it ([`ScrollRecord::place`]).
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

    /// Clears any pending list nudge after a scroll ([`ScrollRecord::left_of`]).
    pub(super) fn drop_list_nudge(&self) {
        if let Some(record) = self.scroll_record.borrow_mut().as_mut() {
            record.plan.seated = None;
            record.hold = false;
            record.place = None;
            record.left = None;
        }
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

    /// PAGE DOWN (`forward`) OR PAGE UP ON THE PAGE'S LIST.
    pub(super) fn turn_list_page(&mut self, forward: bool) -> bool {
        let context = self.scroll_context();
        let turned = {
            let mut record = self.scroll_record.borrow_mut();
            record
                .as_mut()
                .filter(|record| record.context == context)
                .map(|record| {
                    let (top, nudge) = if forward {
                        (record.plan.page_after(record.top), None)
                    } else {
                        record.plan.page_up(record.top)
                    };
                    record.plan.seated = nudge.map(|nudge| Seat::nudged(top, nudge));
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

    /// Bring `key` into view at the next render when the page's list holds it off screen.
    pub(crate) fn reveal_listed_control(&mut self, key: &UiKey) -> bool {
        let listed = self
            .scroll_plan()
            .is_some_and(|plan| plan.stop_of(key).is_some())
            || self.open_disclosure_holding(key);
        if listed {
            self.reveal_control(key.clone());
        }
        listed
    }

    /// Keep `key` in view at the next render, its whole stop.
    pub(super) fn reveal_control(&mut self, key: UiKey) {
        self.page_reveal = Some(key);
        self.reveal_as = Reveal::Keep;
    }

    /// Keep control `key` in view at the next render.
    pub(super) fn reveal_pressed(&mut self, key: UiKey) {
        self.page_reveal = Some(key);
        self.reveal_as = Reveal::Pressed;
    }

    /// Handle a pointer press on a control preview picture in the list strip.
    pub(super) fn press_list_picture(&mut self, key: UiKey) -> bool {
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

    /// A PRESS KEEPS ITS CONTROL IN VIEW, however the press came.
    pub(super) fn reveal_press(&mut self, key: &UiKey, before: Option<&UiKey>) {
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
        if listed {
            self.reveal_pressed(key.clone());
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

    /// Forget the list.
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

    /// Whether the stop holding the control the list keeps in view ([`Self::page_reveal`]).
    pub(super) fn revealed_stop_fits(&self) -> bool {
        let Some(key) = self.page_reveal.as_ref() else {
            return true;
        };
        self.scroll_plan().is_none_or(|plan| {
            plan.stop_holding(key)
                .or_else(|| plan.stop_of(key))
                .is_none_or(|stop| plan.span(stop..stop + 1) <= plan.room() + 0.01)
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

    /// The keyboard to the first control the list's screen shows, ringed.
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

    /// Records status-bar slack so a status bar does not cover the active control ([`ListFoot::Whole`]).
    pub(super) fn note_status_room(&self, plan: &ScrollPlan, room: f32) {
        let slack = self.status_slack.get();
        let (given, room) = (plan.given + slack, room + slack);
        if given <= 0.01 || self.measuring_calm.get() {
            return;
        }
        for key in [self.page_reveal.as_ref(), self.common.last_focus.as_ref()]
            .into_iter()
            .flatten()
        {
            let Some(index) = plan.stop_holding(key).or_else(|| plan.stop_of(key)) else {
                continue;
            };
            let (top, bottom) = plan.span_of(index, Some(key), Reveal::Pressed);
            let seated = (plan.foot == ListFoot::Whole)
                .then(|| plan.stop_top(index.min(plan.limit()), index) + bottom)
                .filter(|need| *need <= room + 0.01);
            let need = seated.unwrap_or(bottom - top);
            let overrun = given - (room - need);
            if need <= room + 0.01 && overrun > 0.01 {
                self.status_overrun
                    .set(self.status_overrun.get().max(overrun));
            }
        }
    }

    /// HOME AND END TAKE THE KEYBOARD TO THE LIST'S ENDS.
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

    /// THE SIDEBAR IS ONE TAB STOP (the macOS sidebar convention).
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

    /// TAB ENTERS A LIST WHOSE SCREEN SHOWS NONE OF ITS CONTROLS.
    pub(crate) fn tab_into_unshown_list(&self, forward: bool, order: &[UiKey]) -> Option<UiKey> {
        let plan = self.scroll_plan()?;
        if plan.controls().any(|(_, key)| order.contains(key)) {
            return None;
        }
        let mut controls = plan.controls().map(|(_, key)| key.clone());
        if forward {
            controls.next()
        } else {
            controls.last()
        }
    }

    /// TAB WALKS THE WHOLE LIST, not just its screen.
    pub(crate) fn tab_past_screen(
        &self,
        current: &UiKey,
        next: &UiKey,
        forward: bool,
        order: &[UiKey],
    ) -> Option<UiKey> {
        let plan = self.scroll_plan()?;
        let controls: Vec<&UiKey> = plan.controls().map(|(_, key)| key).collect();
        let target = match (plan.control_index(current), plan.control_index(next)) {
            (Some(index), Some(after)) if index == after => {
                if forward {
                    controls.get(index + 1)
                } else {
                    controls.get(index.checked_sub(1)?)
                }
            }
            (Some(index), None) if forward => controls.get(index + 1),
            (Some(index), None) => controls.get(index.checked_sub(1)?),
            (None, Some(_)) if !order.contains(current) => None,
            (None, Some(_)) if forward => controls.first(),
            (None, Some(_)) => controls.last(),
            (Some(_), Some(_)) | (None, None) => None,
        }?;
        (*target != next).then(|| (*target).clone())
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

/// Each control `node`.
pub(super) fn control_spans_in(node: &UiNode, height: f32) -> Vec<(UiKey, f32, f32)> {
    fn walk(node: &UiNode, top: f32, height: f32, out: &mut Vec<(UiKey, f32, f32)>) {
        if node.paint_only {
            return;
        }
        if is_control(node) {
            out.push((node.key.clone(), top, height));
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

/// Every control under `node` a person can focus.
pub(super) fn focus_keys(node: &UiNode) -> Vec<UiKey> {
    fn walk(node: &UiNode, out: &mut Vec<UiKey>) {
        if node.paint_only {
            return;
        }
        if node.segmented {
            out.extend(segmented_control_key(node));
            return;
        }
        let focusable = match &node.content {
            UiContent::Button(control) => control.state.enabled || control.state.busy,
            UiContent::Switch(control) => control.state.enabled || control.state.busy,
            UiContent::Slider(control) => control.state.enabled || control.state.busy,
            UiContent::TextField(control) => control.state.enabled || control.state.busy,
            UiContent::ColorWheel(control) => control.state.enabled || control.state.busy,
            UiContent::Custom(spec) => spec.focusable,
            _ => false,
        };
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

/// The list itself.
pub(super) fn scroll_list_node(
    key: &str,
    label: &str,
    plan: &ScrollPlan,
    view: Range<usize>,
    children: Vec<UiNode>,
) -> UiNode {
    let whole = view.start == 0 && view.end >= plan.len();
    let height = if whole {
        plan.span(view.clone())
    } else {
        plan.room().max(plan.span(view.start..view.start + 1))
    };
    let screen = Screen {
        drawn: view.clone(),
        view,
        offset: 0.0,
        height,
        live: height,
        fades: false,
        whole,
        caption: None,
        edge_caption: None,
        bare: false,
    };
    list_screen_node(key, label, plan, &screen, children)
}

/// Give `plan` its `room` and the strip under it a page keeps for a status bar ([`Overhang`]).
pub(super) fn seat_room(plan: &mut ScrollPlan, room: f32, overhang: Overhang) {
    let now = overhang.now.max(0.0);
    let kept = overhang.kept.max(0.0);
    let n = plan.len();
    let fits = now > 0.0
        && plan.span(0..n) <= room + now + 0.01
        && (0..n).all(|stop| plan.whole_by(0, stop) <= room + 0.01);
    if fits {
        plan.set_room(room + now);
        plan.overhang = 0.0;
        plan.kept = 0.0;
    } else {
        plan.set_room(room);
        plan.overhang = now;
        plan.kept = kept;
    }
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

/// [`scroll_list_node`] for a screen that draws its PEEK ([`ScrollPlan::screen`]).
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
            value: Some(format!("{},{},{}", view.start, view.len(), plan.len())),
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

/// Seat a PAGE'S OWN scrolling list in the page's right gutter.
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

/// A card the list lays out row by row.
#[derive(Clone)]
pub(super) struct SplitCard {
    pub(super) key: String,
    pub(super) spec: GroupSpec,
    pub(super) padding: Insets,
    pub(super) gap: f32,
    pub(super) head: Vec<(UiNode, f32)>,
    pub(super) rows: Vec<(UiNode, f32)>,
}

fn is_heading(node: &UiNode) -> bool {
    match &node.content {
        UiContent::Text(text) => text.role == SemanticRole::Heading,
        UiContent::Group(_) => {
            node.paint_only && matches!(node.children.as_slice(), [only] if is_heading(only))
        }
        _ => false,
    }
}

impl SplitCard {
    /// A column card authored whole.
    pub(super) fn from_column(node: &UiNode) -> Option<Self> {
        let UiContent::Group(spec) = &node.content else {
            return None;
        };
        if node.layout.flow != Flow::Column || node.children.is_empty() {
            return None;
        }
        let mut rows = node
            .children
            .iter()
            .map(|child| match child.layout.height {
                Length::Fixed(height) => Some((child.clone(), height)),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let leads = rows.len() > 1 && is_heading(&rows[0].0);
        let head = if leads {
            vec![rows.remove(0)]
        } else {
            Vec::new()
        };
        Some(Self {
            key: node.key.as_str().to_string(),
            spec: spec.clone(),
            padding: node.layout.padding,
            gap: node.layout.gap,
            head,
            rows,
        })
    }

    fn height_of(&self, rows: Range<usize>) -> f32 {
        let children = self.head.len() + rows.len();
        self.padding.top
            + self.padding.bottom
            + self.head.iter().map(|(_, height)| height).sum::<f32>()
            + self.rows[rows]
                .iter()
                .map(|(_, height)| height)
                .sum::<f32>()
            + children.saturating_sub(1) as f32 * self.gap
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

    /// Where each control `rows` hold sits, from the top of the first.
    pub(super) fn control_spans(&self, rows: Range<usize>) -> Vec<(UiKey, f32, f32)> {
        let mut top = 0.0;
        let mut out = Vec::new();
        for (node, height) in &self.rows[rows] {
            out.extend(
                control_spans_in(node, *height)
                    .into_iter()
                    .map(|(key, at, tall)| (key, top + at, tall)),
            );
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
        let notes: Vec<bool> = self
            .rows
            .iter()
            .enumerate()
            .map(|(row, (node, _))| {
                controls[..row].iter().any(|control| *control) && notes_rows_over(node)
            })
            .collect();
        let mut stops: Vec<Range<usize>> = Vec::new();
        for row in 0..self.rows.len() {
            match stops.last_mut() {
                Some(stop) if under_control(row) && height(stop.start..row + 1) <= room + 0.01 => {
                    stop.end = row + 1;
                }
                Some(stop)
                    if !has_control(stop)
                        && !under_control(stop.start)
                        && !notes[stop.start]
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

    /// Whether every one of `rows` is A NOTE ON THE ROWS OVER IT ([`notes_rows_over`]).
    pub(super) fn notes_only(&self, rows: Range<usize>) -> bool {
        !rows.is_empty()
            && rows.clone().all(|row| {
                notes_rows_over(&self.rows[row].0)
                    && self.rows[..row].iter().any(|(node, _)| holds_control(node))
            })
    }

    /// The card holding `rows`, its head over them.
    pub(super) fn node(&self, rows: Range<usize>) -> UiNode {
        UiNode::new(self.key.clone(), UiContent::Group(self.spec.clone()))
            .layout(
                Layout::column()
                    .width(Length::Fill)
                    .height(Length::Fixed(self.height_of(rows.clone())))
                    .padding(self.padding)
                    .gap(self.gap)
                    .clipped(),
            )
            .children(
                self.head
                    .iter()
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
        let card = if self.head_starves(room) {
            Self {
                head: Vec::new(),
                rows: self.head.iter().chain(&self.rows).cloned().collect(),
                ..self.clone()
            }
        } else {
            self.clone()
        };
        let limit = card.row_limit(room).max(1.0);
        Self {
            rows: card
                .rows
                .iter()
                .cloned()
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

fn splittable(node: &UiNode) -> bool {
    matches!(node.content, UiContent::Group(_))
        && node.layout.flow == Flow::Column
        && node.layout.offset == 0.0
        && !node.segmented
        && !node.children.is_empty()
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

/// A ROW TALLER THAN ITS SCREEN, LISTED A PART AT A TIME.
pub(super) fn row_parts(
    (node, height): (UiNode, f32),
    limit: f32,
    most: f32,
    gap: f32,
) -> Vec<(UiNode, f32)> {
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
        if !part.is_empty() && used + cost > inner + 0.01 && !names {
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
        match SplitCard::from_column(&node) {
            Some(card) => Self::Split(card),
            None => Self::Card(Box::new(node), height),
        }
    }
}

type ItemStops = Vec<(usize, Option<Range<usize>>)>;

fn plan_items(items: &[ListItem], gap: f32, room: f32) -> (ScrollPlan, ItemStops) {
    let mut plan = ScrollPlan::new(gap);
    let mut stops: ItemStops = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            ListItem::Card(node, height) => {
                plan.card(*height, focus_keys(node));
                plan.set_ids(vec![node.key.clone()]);
                plan.measure(node, *height);
                plan.note_previews([&**node]);
                plan.add_segments(node);
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
                    plan.row(
                        group,
                        card.rows[rows.clone()]
                            .iter()
                            .map(|(_, height)| height + card.gap)
                            .sum(),
                        card.rows[rows.clone()]
                            .iter()
                            .flat_map(|(node, _)| focus_keys(node))
                            .collect(),
                    );
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
                    plan.set_control_spans(card.control_spans(rows.clone()));
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
    let (plan, _) = plan_items(&fit_rows(items, room), gap, room);
    (0..plan.len())
        .map(|stop| plan.span(stop..stop + 1))
        .fold(0.0, f32::max)
}

/// The height the first `stops` stops of `items` take on screen together.
pub(super) fn leading_span(items: &[ListItem], gap: f32, room: f32, stops: usize) -> f32 {
    let (plan, _) = plan_items(&fit_rows(items, room), gap, room);
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
        let (mut plan, stops) = plan_items(items, gap, room);
        plan.set_foot(foot);
        seat_room_under_status(state, &mut plan, room, overhang);
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
                    let part = SplitCard {
                        head: Vec::new(),
                        ..card.clone()
                    };
                    let mut node = part.node(first..last).offscreen();
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

/// [`list_page`] whose rows off the screen are in the tree too ([`UiNode::offscreen`]).
pub(super) fn list_page_whole(
    state: &SettingsViewState,
    key: &str,
    label: &str,
    items: Vec<ListItem>,
    room: f32,
    gap: f32,
) -> UiNode {
    let items = fit_rows(&items, room);
    let seated = SeatedList::with_foot(state, &items, room, gap, Overhang::NONE, ListFoot::Whole);
    let drawn = seated.drawn.clone();
    let above = seated.offscreen_children(&items, 0..drawn.start, "above");
    let below = seated.offscreen_children(&items, drawn.end..seated.plan.len(), "below");
    let lead = above
        .iter()
        .map(|node| match node.layout.height {
            Length::Fixed(height) => height + seated.plan.gap(),
            _ => seated.plan.gap(),
        })
        .sum::<f32>();
    let screen = seated.screen_children(&items);
    let mut page = seated.screen_node(key, label, screen);
    if let Some(list) = find_node_mut(&mut page, key) {
        list.layout.offset += lead;
        if let Some(room) = list.room.as_mut() {
            room.lists_all = true;
        }
        let screen = std::mem::take(&mut list.children);
        list.children = above.into_iter().chain(screen).chain(below).collect();
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
            seat_room(
                &mut plan,
                140.0,
                Overhang {
                    now: overhang,
                    kept: 44.0,
                },
            );
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
        seat_room(
            &mut words,
            140.0,
            Overhang {
                now: 44.0,
                kept: 44.0,
            },
        );
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
        let words = |key: &str| {
            UiNode::new(
                key,
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
        };
        let control = |key: &str| {
            UiNode::new(
                key,
                UiContent::Custom(AuditedCustomNode {
                    audit_id: MESSAGES_SCROLL_AUDIT,
                    role: SemanticRole::Button,
                    label: String::new(),
                    value: None,
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
        );
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
                    value: None,
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

    fn words(key: &str) -> UiNode {
        UiNode::new(
            key,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
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
                value: None,
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
            let node = scroll_list_node("list", "List", &plan, screen.view.clone(), Vec::new());
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
    fn the_thumb_never_runs_past_the_last_card() {
        let plan = cards(&[40.0, 40.0, 40.0, 40.0, 40.0], 100.0, 10.0);
        let view = plan.view(plan.limit());
        let node = scroll_list_node("list", "List", &plan, view.clone(), Vec::new());
        let thumb = node
            .children
            .iter()
            .find(|child| child.key.as_str() == "list/thumb")
            .expect("a scrolled list has its thumb");
        assert_eq!(thumb.layout.height, Length::Fixed(plan.span(view)));
        assert!(plan.span(plan.view(plan.limit())) < plan.room());
    }

    #[test]
    fn a_press_on_a_control_drawn_only_in_the_strip_nudges_it_to_the_rooms_edge() {
        let mut plan = ScrollPlan::new(2.0);
        for index in 0..5 {
            plan.card(40.0, vec![UiKey::new(format!("control/{index}"))]);
        }
        seat_room(
            &mut plan,
            100.0,
            Overhang {
                now: 60.0,
                kept: 60.0,
            },
        );
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
        seat_room(
            &mut fading,
            105.0,
            Overhang {
                now: 40.0,
                kept: 40.0,
            },
        );
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
        seat_room(
            &mut fading,
            100.0,
            Overhang {
                now: 30.0,
                kept: 30.0,
            },
        );
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
                        value: Some("0,2,5".to_string()),
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
        seat_room(&mut plan, 326.0, Overhang { now, kept: 44.0 });
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
            seat_room(&mut plan, 200.0, Overhang { now, kept: 44.0 });
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
        seat_room(
            &mut plan,
            255.0,
            Overhang {
                now: 44.0,
                kept: 44.0,
            },
        );
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
        seat_room(
            &mut card,
            250.0,
            Overhang {
                now: 44.0,
                kept: 44.0,
            },
        );
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
