// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every Settings page is ONE SCROLLING LIST (design ruling 264, the model
//! Settings ▸ Messages set): its content is a run of STOPS, each a whole row
//! (a card's rows are each a stop of their own) or a whole card, under a
//! pinned heading and, where the page has one, its live preview. The wheel,
//! the arrow keys and a screen reader's scroll move the list a stop at a time
//! and Page Up/Down a whole screen. The list never scrolls past the screen
//! that shows its last stop, and a screen always shows at least one stop.
//! There is no pager: a thin paint-only thumb says the list scrolls and where
//! it is.
//!
//! ONE MEASURE: a screen SHOWS the stops whose controls are whole inside the
//! list's ROOM — the room a status bar leaves it — and whose drawn content
//! (rows, words, controls; not the card padding under them) ends on the
//! screen: in the room, or, for the words under a stop's controls (a row's
//! facts, its note), in the strip under the room while no bar takes it
//! ([`ScrollPlan::view`]). The screen draws what it shows whole and at full
//! strength, live where it holds words, its peek fading only under that
//! ([`ScrollPlan::screen`]): a row's second line drawn in the strip was a
//! row shown whole that Page Down turned onto again. The scroll's bound,
//! Page Up and Down, the keyboard's reveal and what the screen draws all
//! read that one measure, so a stop a screen shows is never shown again by
//! the next, and none it draws whole goes uncounted. Page Down turns to the
//! stop after the last the screen shows ([`ScrollPlan::page_after`]); Page
//! Up to the screen that ends on the stop before its first, seated at a stop
//! where one does and else scrolled so that stop ends at the room's edge, as
//! the last screen is ([`ScrollPlan::page_up`]). Only a turn that reaches an
//! end of the list — its full last screen, its first — shows again what the
//! screen before showed, as a platform list's last page does.
//!
//! THE EDGE RULE ([`UiNode::room`]): whatever part of a control shows inside
//! the room is live, at both of the list's edges; a press on one an edge cuts
//! scrolls it whole by its hidden part and no more, so it stays under the
//! pointer that pressed it, as a platform list scrolls a clicked row into
//! view: one the room's bottom edge cuts is nudged up to end at that edge
//! ([`ScrollPlan::press_nudge`]), and one cut at the last screen's top comes
//! down to the screen that starts with its stop. The list holds the place the
//! nudge gave it, as a platform list holds where a click scrolled it, until
//! something scrolls it ([`SettingsViewState::drop_list_nudge`]): the next
//! press, or the keyboard, on a control it shows whole moves nothing. The
//! strip under the room, which the page keeps for a status bar that is not
//! up, holds no control: the words under the controls of the last stop the
//! screen shows, drawn whole, and under them only a picture — the peek of
//! the stops after the screen, drawn on to the page's bottom so no screen
//! ends on a band of blank, fading out and never live — so the bar that
//! comes takes nothing a person can press and no row moves for it (a bar
//! over a shown row's facts covers those words, and the keyboard on that
//! row keeps its place). On the last screen the stop before the screen's first
//! peeks in over the top, cut at the list's top edge and live where it shows,
//! so the last stop ends at the room's edge and that screen is as full as
//! every other.
//!
//! A HARD EDGE NEVER CUTS THROUGH A CAPTION, AND NO SCREEN ENDS ON BLANK
//! UNDER A CARD IT CUTS: where no strip fades under the room, the screen
//! draws on to the page's bottom and its edge cuts the card that crosses it
//! there, live where it shows; a caption the edge would cut through paints
//! none of its words, the card's border, padding and backdrop going on to
//! the edge ([`ScrollPlan::screen`]). Where no card after the screen shows
//! [`PEEK_MIN`] of itself, the screen ends on its last card's boundary, and
//! the next screen starts at the card after it. Where the strip fades, the
//! peek is a picture drawn on to the page's bottom.
//!
//! A group's rows draw inside the group's card. A screen that starts in the
//! middle of a group repeats the group's caption over its first row there,
//! the last screen too ([`scroll_list_screen`]).
//!
//! The render measures the page into a [`ScrollPlan`] and records it
//! ([`SettingsViewState::seat_scroll`]); the reducer reads that record to
//! bound the scroll, to move the keyboard between the page's controls, and
//! to adopt where the render put the list ([`SettingsViewState::settle_page_scroll`]).

use std::ops::Range;

use crate::native_ui::{
    AuditedCustomNode, Flow, GroupSpec, Insets, Layout, Length, MESSAGES_SCROLL_AUDIT,
    PAGE_SCROLL_AUDIT, SemanticRole, StyleRef, UiContent, UiKey, UiNode,
};

use super::{SettingsRoute, SettingsViewState};

/// The least of a card a list's edge leaves in view that reads as the card
/// going on — its border, its corners and some of its padding
/// ([`ScrollPlan::screen`]).
pub(super) const PEEK_MIN: f32 = 16.0;

/// The strip under a list a page keeps for a status bar ([`seat_room`]): how
/// tall it is now — none while a bar is up and takes it — and how tall the
/// page keeps it, bar or no bar.
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

/// A page measured as stops ([`ScrollPlan::card`], [`ScrollPlan::row`]) and
/// the room its list has. Pure, so the renderer and the reducer share it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ScrollPlan {
    heights: Vec<f32>,
    groups: Vec<Option<usize>>,
    /// What each group's card adds around any run of its rows: its padding
    /// and caption.
    heads: Vec<f32>,
    /// The part of each group's head drawn above a run's first row.
    leads: Vec<f32>,
    /// Where each group's caption begins and ends under its card's top (its
    /// words, under the card's top padding), where one is known: a hard edge
    /// never cuts through it.
    captions: Vec<Option<(f32, f32)>>,
    /// Each stop's own caption, a whole card's leading heading.
    card_captions: Vec<Option<CardCaption>>,
    /// The gap each group's card sets between its rows, which each of its
    /// stops carries as its share ([`Self::set_row_gap`]): a run's rows end
    /// that much above the sum of its stops.
    row_gaps: Vec<f32>,
    keys: Vec<Vec<UiKey>>,
    /// The live previews each stop draws (their node keys): what a page's
    /// preview cadence asks whether a screen draws ([`Self::stop_of_preview`]).
    previews: Vec<Vec<UiKey>>,
    /// Each stop's rows that hold a control: where the row starts in the
    /// stop, and where its controls start and how tall they are.
    control_rows: Vec<Vec<(f32, f32, f32)>>,
    /// How far under each stop's top what it draws ends: its rows, words and
    /// controls, not the padding under them ([`Self::measure`]).
    ink: Vec<f32>,
    /// How far under each stop's top what it draws begins, the padding over
    /// it aside: a screen whose top edge cuts only that padding shows the
    /// stop whole.
    ink_top: Vec<f32>,
    /// How far under each stop's top its first row of controls ends, where
    /// it holds one: a card's first row of buttons, not the rows under it
    /// ([`Self::first_screen_need`]).
    first_controls: Vec<Option<f32>>,
    room: f32,
    /// The strip under the list kept for a status bar that is not up: the
    /// screen draws its peek on into it ([`ScrollPlan::screen`]).
    overhang: f32,
    /// The strip the page keeps for a bar whether or not one is up: the last
    /// card's padding may fall in it, so where the list's end sits is the same
    /// with a bar and without.
    kept: f32,
    gap: f32,
    /// The screen the render seated ([`SettingsViewState::seat_scroll`]) and
    /// how far past its stops' seat it is scrolled, where a press found a
    /// control an edge of the room cuts: that control's hidden part
    /// ([`Self::press_nudge`]). Every [`Self::screen`] of that first stop
    /// draws so scrolled — the render's, and the record's that the reducer
    /// reads. It is part of the list's place: the next render keeps it until
    /// something scrolls the list ([`SettingsViewState::drop_list_nudge`]) or
    /// a reveal needs what the nudged screen does not show whole.
    seated: Option<Seat>,
    /// Each segment of a segmented control the list holds, and the key the
    /// control is listed by ([`Self::add_segments`]): a segment is that one
    /// control to the list's keyboard, never a control of its own.
    segments: Vec<(UiKey, UiKey)>,
}

/// Where the render seated a screen off its stops ([`ScrollPlan::seated`]):
/// its first stop, how far past that stop's seat it is scrolled, and whether
/// it lets go of the caption of the run it starts in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Seat {
    pub(super) top: usize,
    pub(super) nudge: f32,
    /// THE LANDING SEATED PAST ITS CAPTION ([`ScrollPlan::land_selected`]):
    /// the screen draws the run it starts in without its caption kept over
    /// it — scrolled off whole by its lead — so a selected stop that fits
    /// the room but not under that caption is drawn whole. Anything after
    /// it that nudges this screen (a press) keeps it let go, as a press's
    /// nudge never brings back a caption the screen let go.
    pub(super) bare: bool,
}

impl Seat {
    /// A press's nudge of the screen whose first stop is `top`
    /// ([`ScrollPlan::press_nudge`]).
    pub(super) const fn nudged(top: usize, nudge: f32) -> Self {
        Self {
            top,
            nudge,
            bare: false,
        }
    }
}

/// Where a stop a screen draws sits on it ([`ScrollPlan::drawn_bands`]), from
/// the list's top: where the stop starts and how tall it is, where what it
/// draws begins and ends, and where the room starts over it (under a caption
/// the screen keeps whole at its top).
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(super) struct DrawnBand {
    pub(super) stop: usize,
    pub(super) at: f32,
    pub(super) height: f32,
    pub(super) ink: (f32, f32),
    pub(super) floor: f32,
}

/// A whole card's caption, under the card's top: where its words begin
/// (under the card's top padding) and end, where what it heads begins, and
/// how tall that is — what can scroll under the caption kept whole.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CardCaption {
    top: f32,
    bottom: f32,
    lead: f32,
    rows: f32,
}

/// One stretch of a screen: a card stop, or a run of one group's rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ScrollRun {
    Card(usize),
    Group { group: usize, rows: Range<usize> },
}

/// What one screen of a list draws ([`ScrollPlan::screen`]).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Screen {
    /// The stops it shows: every one whose content ends in its room.
    pub(super) view: Range<usize>,
    /// The stops it draws: those, and the ones after them down to the page's
    /// bottom (its peek) — on the last screen the ones before them, the first
    /// of them cut at the list's top.
    pub(super) drawn: Range<usize>,
    /// How far past `drawn`'s top the list is scrolled (the last screen).
    pub(super) offset: f32,
    /// How tall the list draws.
    pub(super) height: f32,
    /// How far from the list's top what it draws is live: its room, or a
    /// first stop taller than that.
    pub(super) live: f32,
    /// Whether the list goes on past the screen, so the strip under its room
    /// holds a peek that fades.
    pub(super) fades: bool,
    /// Whether the screen is the whole list.
    pub(super) whole: bool,
    /// The caption of the run the screen starts in, scrolled into or past it
    /// (the last screen, a nudged one): kept whole at the list's top while
    /// the run's rows scroll under it, or let go — its words the top edge
    /// cuts then paint nothing ([`ScrollPlan::top_caption`]).
    pub(super) caption: Option<TopCaption>,
    /// Where a hard edge — no strip fading under the room — cuts through the
    /// caption of the card the screen draws last, and where that caption
    /// ends, both from the card's top: the words it cuts paint nothing, and
    /// the card's border, padding and backdrop fill on to the edge
    /// ([`ScrollPlan::edge_caption`]), so the screen never ends on a caption
    /// cut through its words nor on a band of blank under the card it cuts.
    pub(super) edge_caption: Option<(f32, f32)>,
}

/// The caption of the run a screen starts in, at the list's top
/// ([`Screen::caption`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TopCaption {
    /// How far under its card's top what the caption heads begins (the
    /// group's lead).
    pub(super) lead: f32,
    /// Whether the screen keeps it whole, the run's rows scrolled under it;
    /// where not, the list's top edge cuts the card, and the caption's words
    /// it would cut through paint nothing.
    pub(super) kept: bool,
}

impl ScrollPlan {
    pub(super) fn new(gap: f32) -> Self {
        Self {
            gap,
            ..Self::default()
        }
    }

    /// The height the list has. A page sets it once it knows what it pins.
    pub(super) fn set_room(&mut self, room: f32) {
        self.room = room;
    }

    pub(super) const fn room(&self) -> f32 {
        self.room
    }

    /// The room kept under the list for a status bar that is not up
    /// ([`seat_room`]).
    #[cfg(test)]
    pub(super) const fn overhang(&self) -> f32 {
        self.overhang
    }

    pub(super) const fn gap(&self) -> f32 {
        self.gap
    }

    pub(super) fn len(&self) -> usize {
        self.heights.len()
    }

    /// Where each stop the screen whose first stop is `top` draws sits on it
    /// ([`Self::screen`]): its index, how far under the list's top it starts,
    /// and how tall it is. A test groups what the compiled screen paints by
    /// it, to judge from the paint which stops the screen shows whole.
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
        self.groups.push(None);
        self.control_rows.push(if keys.is_empty() {
            Vec::new()
        } else {
            vec![(0.0, 0.0, height)]
        });
        self.keys.push(keys);
        self.heights.len() - 1
    }

    /// [`Self::card`] put in at stop `index`, before the stops there (a
    /// field page's preview that cannot be pinned leads its list): its
    /// controls, none for a preview, and the stops after it move down one.
    pub(super) fn insert_card(&mut self, index: usize, height: f32, keys: Vec<UiKey>) {
        let index = index.min(self.heights.len());
        self.heights.insert(index, height);
        self.ink.insert(index, height);
        self.ink_top.insert(index, 0.0);
        self.first_controls
            .insert(index, (!keys.is_empty()).then_some(height));
        self.previews.insert(index, Vec::new());
        self.card_captions.insert(index, None);
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
    }

    /// A group card: `head` is everything a run of its rows pays besides
    /// the rows (padding and caption), `lead` the part of it above the run's
    /// first row. Its rows follow with [`Self::row`].
    pub(super) fn group(&mut self, head: f32, lead: f32) -> usize {
        self.heads.push(head);
        self.leads.push(lead.max(0.0));
        self.captions.push(None);
        self.row_gaps.push(0.0);
        self.heads.len() - 1
    }

    /// Say that `group`'s card sets `gap` between its rows, each of its stops
    /// `gap` taller than its rows for it: where a run's rows END, so a caption
    /// is kept over them exactly while some of them scroll under it, as the
    /// card draws it (`keep_caption`).
    pub(super) fn set_row_gap(&mut self, group: usize, gap: f32) {
        if let Some(row_gap) = self.row_gaps.get_mut(group) {
            *row_gap = gap.max(0.0);
        }
    }

    /// How tall the rows of `group`'s run over `rows` are in its card: the
    /// run's stops less the share of the gap the last carries.
    fn run_rows(&self, group: usize, rows: &Range<usize>) -> f32 {
        let stops = self.heights[rows.clone()].iter().sum::<f32>();
        (stops - self.row_gaps.get(group).copied().unwrap_or(0.0)).max(0.0)
    }

    /// Say that `group`'s card paints a caption from `top` under its top (its
    /// top padding) down to `bottom`: a hard edge lands on the caption's top
    /// rather than cut through it.
    pub(super) fn set_caption(&mut self, group: usize, top: f32, bottom: f32) {
        if let Some(caption) = self.captions.get_mut(group) {
            *caption = Some((top.max(0.0), bottom.max(top)));
        }
    }

    /// Where stop `index` starts in a run that starts with it: under its
    /// group's lead, or at the top of a card.
    fn lead_of(&self, index: usize) -> f32 {
        self.groups[index].map_or(0.0, |group| self.leads[group])
    }

    /// What a run of `group`'s rows draws under its last row: the card's
    /// padding there.
    fn closing(&self, group: usize) -> f32 {
        (self.heads[group] - self.leads[group]).max(0.0)
    }

    /// Whether stop `index` starts a run of its own: a card, or a group's row
    /// after a stop of another.
    fn starts_run(&self, index: usize) -> bool {
        index == 0 || self.groups[index].is_none() || self.groups[index] != self.groups[index - 1]
    }

    /// Say where the controls of the last stop added sit: for each of its
    /// rows that holds one, where the row starts in the stop, and where its
    /// controls start and how tall they are.
    pub(super) fn set_control_rows(&mut self, rows: Vec<(f32, f32, f32)>) {
        if let Some(first) = self.first_controls.last_mut() {
            *first = rows.first().map(|(_, at, height)| at + height);
        }
        if let Some(last) = self.control_rows.last_mut() {
            *last = rows;
        }
    }

    /// Say how far under its top the last stop added has its first row of
    /// controls end, where that is less than its first control row's
    /// ([`Self::set_control_rows`]): a row of buttons over another.
    pub(super) fn set_first_controls(&mut self, end: Option<f32>) {
        if let (Some(first), Some(end)) = (self.first_controls.last_mut(), end) {
            *first = Some(first.map_or(end, |was| was.min(end)));
        }
    }

    /// Say how far under its top what the last stop added draws ends: its
    /// rows and words, not the padding under them. A stop is shown where
    /// that is in the room ([`Self::view`]).
    pub(super) fn set_ink(&mut self, ink: f32) {
        if let (Some(last), Some(height)) = (self.ink.last_mut(), self.heights.last()) {
            *last = ink.clamp(0.0, *height);
        }
    }

    /// Say how far under its top what the last stop added draws begins: the
    /// padding over its rows aside.
    pub(super) fn set_ink_top(&mut self, top: f32) {
        if let (Some(last), Some(ink)) = (self.ink_top.last_mut(), self.ink.last()) {
            *last = top.clamp(0.0, *ink);
        }
    }

    /// Measure the last stop added from `node`, laid out `height` tall: where
    /// its controls sit and where what it holds ends, its own card's padding
    /// under that aside.
    pub(super) fn measure(&mut self, node: &UiNode, height: f32) {
        self.set_control_rows(control_rows_in(node, height));
        self.set_first_controls(first_control_row(node, height).map(|(top, tall)| top + tall));
        let content = content_extent(node, height);
        self.set_ink(content.map_or(0.0, |(top, tall)| top + tall));
        self.set_ink_top(content.map_or(0.0, |(top, _)| top));
        if let Some(last) = self.card_captions.last_mut() {
            *last = caption_extent(node);
        }
    }

    /// Where stop `index`'s first row starts on a screen whose first stop is
    /// `from`: after the runs before it and the gap, under its group's lead
    /// where it starts a run, else right after the stop before it.
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

    /// Where stop `k` starts, stop `k - 1` starting at `before`.
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

    /// Where the card that draws stop `index` begins on a screen whose first
    /// stop is `from`: over its group's caption where it starts a run.
    fn card_top(&self, from: usize, index: usize) -> f32 {
        let top = self.stop_top(from, index);
        if index == from || self.starts_run(index) {
            top - self.lead_of(index)
        } else {
            top
        }
    }

    /// Where the card that draws stop `index` ends on a screen whose first
    /// stop is `from`, `index` the last of it the screen draws: under the
    /// stop, and its group's padding there.
    fn card_bottom(&self, from: usize, index: usize) -> f32 {
        self.stop_top(from, index)
            + self.heights[index]
            + self.groups[index].map_or(0.0, |group| self.closing(group))
    }

    /// How far down a screen whose first stop is `from` stop `index` must
    /// draw to be SHOWN: its content — and, for the list's last stop, its
    /// card's padding past what the page keeps for a status bar, so the end
    /// of the list is on screen.
    fn shown_by(&self, from: usize, index: usize) -> f32 {
        let end = self.stop_top(from, index) + self.ink[index];
        if index + 1 < self.len() {
            return end;
        }
        let bottom = self.stop_top(from, index)
            + self.heights[index]
            + self.groups[index].map_or(0.0, |group| self.closing(group));
        end.max(bottom - self.kept)
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
        self.groups.push(Some(group));
        self.control_rows.push(if keys.is_empty() {
            Vec::new()
        } else {
            vec![(0.0, 0.0, height)]
        });
        self.keys.push(keys);
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

    /// The stops a screen whose first stop is `top` SHOWS: every one from
    /// there whose content ends in the room ([`Self::shown_by`]), and the
    /// first whatever its height.
    pub(super) fn view(&self, top: usize) -> Range<usize> {
        let n = self.len();
        if n == 0 {
            return 0..0;
        }
        let top = top.min(n - 1);
        let mut end = top + 1;
        let mut at = self.stop_top(top, top);
        while end < n {
            at = self.next_top(at, end);
            let shown = if end + 1 == n {
                self.shown_by(top, end) <= self.room + 0.01
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

    /// Whether stop `index`, drawn from `at` under the list's top on a screen
    /// whose room ends `room` down, is SHOWN there (THE ONE MEASURE): its
    /// controls whole in the room, and what it draws ending on the screen —
    /// in the room, or the words under its controls in the strip under the
    /// room while that strip is the list's ([`Self::overhang`]: no bar takes
    /// it). A stop with no control is shown where all of it is in the room.
    /// The list's last stop is measured by [`Self::shown_by`], so the list's
    /// end is reached in the room.
    fn shows_at(&self, at: f32, index: usize, room: f32) -> bool {
        at + self.controls_of(index).1 <= room + 0.01
            && at + self.ink[index] <= room + self.overhang + 0.01
    }

    /// The last first stop the list scrolls to: the least whose screen shows
    /// the last stop, so the last screen is full and no screen before it
    /// already shows everything after it.
    pub(super) fn limit(&self) -> usize {
        let n = self.len();
        if n == 0 {
            return 0;
        }
        let mut top = n - 1;
        while top > 0 && self.view(top - 1).end >= n {
            top -= 1;
        }
        top
    }

    /// The first stop of the screen that shows the stops just before `top`
    /// on to it, so Page Up turns back over the stops before a screen as Page
    /// Down turns over those after it.
    pub(super) fn page_before(&self, top: usize) -> usize {
        let top = top.min(self.len());
        let mut first = top.saturating_sub(1);
        while first > 0 && self.view(first - 1).end >= top {
            first -= 1;
        }
        first
    }

    /// How far down a screen whose first stop is `top` stop `index`'s
    /// controls end (where it has none, what it draws): what a screen draws
    /// to show them whole, the words and padding under them aside.
    fn whole_by(&self, top: usize, index: usize) -> f32 {
        self.stop_top(top, index) + self.controls_of(index).1
    }

    /// Where stop `index`'s controls start and end under its top (where it
    /// has none, what it draws).
    fn controls_of(&self, index: usize) -> (f32, f32) {
        self.control_rows[index]
            .iter()
            .map(|(_, at, height)| (*at, at + height))
            .reduce(|(top, bottom), (at, end)| (top.min(at), bottom.max(end)))
            .unwrap_or((0.0, self.ink[index]))
    }

    /// THE FIRST SCREEN'S LEAD ([`super::PageChrome`]): how tall a room the
    /// list's first screen needs to show its first stop (THE ONE MEASURE)
    /// and, after it, the first row of controls the list holds, whole in the
    /// room — what the page leads with and the first thing after it a person
    /// can do. A short window sheds the page's chrome before this leaves the
    /// first screen.
    pub(super) fn first_screen_need(&self) -> f32 {
        let n = self.len();
        if n == 0 {
            return 0.0;
        }
        let lead = self.lead_of(0);
        let first = if n == 1 {
            self.shown_by(0, 0)
        } else {
            (lead + self.controls_of(0).1).max(lead + self.ink[0] - self.overhang)
        };
        let second = (1..n)
            .find_map(|stop| self.first_controls[stop].map(|end| self.stop_top(0, stop) + end))
            .unwrap_or(0.0);
        first.max(second)
    }

    /// The least scroll from `top` whose screen shows stop `index`, measured
    /// on the screen as it draws ([`Self::shows_revealed`]): the last screen
    /// shows whole the stops over its seat that its top edge cuts only the
    /// padding of, so the keyboard a Page Down took to one of them stays on
    /// that screen — never scrolled up to the stop's own seat, whose screen
    /// cuts the list's last stop.
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

    /// Record the segmented controls in `node`, a stop just added
    /// ([`UiNode::segmented`]): each segment resolves to the key its control
    /// is listed by ([`segmented_control_key`]).
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

    /// The control `key` is to the list: itself, or for a segment the
    /// segmented control it is one choice of ([`Self::add_segments`]).
    pub(super) fn resolve<'a>(&'a self, key: &'a UiKey) -> &'a UiKey {
        self.segments
            .iter()
            .find(|(segment, _)| segment == key)
            .map_or(key, |(_, control)| control)
    }

    /// The least scroll from `top` whose screen shows stop `index`'s controls
    /// whole in its room: a press, a change or a landing on a control the
    /// screen already shows whole moves nothing, its words and padding cut or
    /// not; one an edge cuts scrolls the least that shows it whole.
    pub(super) fn reveal_controls(&self, index: usize, top: usize) -> usize {
        self.reveal_span(index, top, self.controls_of(index))
    }

    /// [`Self::reveal_controls`] for a landing that SELECTS stop `index`
    /// ([`Reveal::LandWhole`]): all of the stop, the words under its controls
    /// and its selection's bottom edge too, whole in the room wherever the
    /// room holds it; a stop taller than the room lands at its top. The
    /// least scroll by stops; [`Self::land_selected`] seats it past its
    /// caption where that is what keeps it from being whole.
    pub(super) fn reveal_selected(&self, index: usize, top: usize) -> usize {
        self.reveal_span(index, top, (0.0, self.heights[index]))
    }

    /// THE LANDING THAT SELECTS A STOP DRAWS IT WHOLE WHEREVER THE ROOM HOLDS
    /// IT ([`Reveal::LandWhole`]): the screen [`Self::reveal_selected`] lands
    /// on, and where that screen still cuts the stop though the stop is no
    /// taller than the room — the caption of the run it starts, kept whole
    /// over it, takes the room the stop needs (a SESSIONS card continued at
    /// 2× text: its caption's 43 pt lead over a 422 pt block in a 429 pt
    /// room) — a seat past that caption: the run's caption let go and the
    /// list scrolled so the stop starts at the room's top, the caption
    /// scrolled off whole by its lead, never cut through its words
    /// ([`Seat::bare`]). As far as the list's end lets it: its last card
    /// never comes up past the room's edge. Only a stop taller than the
    /// room keeps the landing at its top. `None` for the seat where the
    /// stops' own seat shows it whole, or no seat does.
    pub(super) fn land_selected(&self, index: usize, top: usize) -> (usize, Option<Seat>) {
        let top = self.reveal_selected(index, top);
        let span = (0.0, self.heights[index]);
        if self.heights[index] > self.room + 0.01 {
            return (top, None);
        }
        // Whole IN THE ROOM: a first stop whose lead and words run past the
        // room is live past it ([`Screen::live`]), and the list's edge cuts
        // it there all the same.
        let base = self.stop_screen(top.min(self.limit()));
        if base.whole || !base.drawn.contains(&index) || self.whole_in_room(&base, index, span) {
            return (top, None);
        }
        (top, self.bare_seat(base, index, span))
    }

    /// THE SEAT PAST THE CAPTION ([`Seat::bare`]) on `base`, a screen the
    /// stops seat that cuts the part of stop `index` from `from` to `to`
    /// under its top: the caption of the run the screen starts in let go and
    /// the list scrolled on so the stop starts at the room's top — the
    /// caption scrolled off whole by its lead, never cut through its words —
    /// or, where the stop is taller than the room, so the part ends at the
    /// room's edge. As far as the list's end lets it: its last card never
    /// comes up past the room's edge. `None` where that seat does not show
    /// the part whole in the room either.
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

    /// THE SEAT THAT SHOWS WHAT THE STOPS CANNOT (the final check of
    /// 2026-10-20, F1): where the screen the stops seat for a reveal — `top`,
    /// from [`Self::reveal`] or [`Self::reveal_controls`] — still cuts the
    /// part of stop `index` the reveal must show ([`Self::revealed_span`]),
    /// though that part is no taller than the room, the list scrolls on into
    /// the stop: by the least nudge that shows the part whole under the
    /// caption the screen then keeps over the run it starts in, as a press's
    /// nudge scrolls ([`Self::press_nudge`]); else past that caption
    /// ([`Self::bare_seat`]), as a landing that selects a stop seats it
    /// ([`Self::land_selected`]). A stop taller than its caption leaves room
    /// for — the `Every session` row with its Timer open at 2× text on the
    /// lab's narrow page, 486 pt under a 43 pt caption in a 451 pt room —
    /// otherwise never showed its last line by any route: the keyboard's
    /// focus on `at 18:00` drew nothing and scrolled nothing, and the next
    /// stop down was the next screen. `None` where the stops' own seat shows
    /// it whole, or no seat does.
    pub(super) fn seat_span(&self, index: usize, top: usize, span: (f32, f32)) -> Option<Seat> {
        if span.1 - span.0 > self.room + 0.01 {
            return None;
        }
        let base = self.stop_screen(top.min(self.limit()));
        // A stop the screen draws, or one after them that a scroll from here
        // brings in (a Timer's choices, listed after its button's part).
        if base.whole || index < base.drawn.start || self.whole_in_room(&base, index, span) {
            return None;
        }
        let at = self.stop_top(base.drawn.start, index) - base.offset;
        let end = self.card_bottom(base.drawn.start, self.len() - 1) - base.offset;
        let floor = self.caption_floor(base.caption, &base.drawn, index);
        // Up by what the room's edge hides, else down by what the top cuts.
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

    /// [`Self::shows_span_on`] within the list's room itself, never past it
    /// where a tall first stop makes the screen live further.
    fn whole_in_room(&self, screen: &Screen, index: usize, span: (f32, f32)) -> bool {
        let room = Screen {
            live: screen.live.min(self.room),
            ..screen.clone()
        };
        self.shows_span_on(&room, index, span)
    }

    /// The least scroll from `top` whose screen shows the part of stop
    /// `index` from `from` to `to` under its top whole in its room.
    fn reveal_span(&self, index: usize, top: usize, (from, to): (f32, f32)) -> usize {
        let limit = self.limit();
        let top = top.min(limit);
        if self.shows_span(index, top, (from, to)) {
            return top;
        }
        if index < top {
            return index.min(limit);
        }
        let mut first = top + 1;
        while first < index && self.stop_top(first, index) + to > self.room + 0.01 {
            first += 1;
        }
        first.min(limit)
    }

    /// Whether the screen whose first stop is `top`, as it draws
    /// ([`Self::screen`], nudged where the render seated it so), shows the
    /// part of stop `index` from `from` to `to` under its top whole in its
    /// room: under the caption the screen keeps whole over the group run it
    /// starts in, and above the room's edge.
    fn shows_span(&self, index: usize, top: usize, span: (f32, f32)) -> bool {
        self.shows_span_on(&self.screen(top.min(self.limit())), index, span)
    }

    /// [`Self::shows_span`] on `screen` as it draws.
    fn shows_span_on(&self, screen: &Screen, index: usize, (from, to): (f32, f32)) -> bool {
        if !screen.drawn.contains(&index) {
            return false;
        }
        let at = self.stop_top(screen.drawn.start, index) - screen.offset;
        let floor = self.caption_floor(screen.caption, &screen.drawn, index);
        at + from >= floor - 0.01 && at + to <= screen.live + 0.01
    }

    /// Where the room starts for stop `index` on a screen drawing `drawn`
    /// whose top caption is `caption` ([`Screen::caption`]): under it where
    /// the screen keeps it whole and `index` is of the run it heads, else at
    /// the list's top.
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

    /// Whether the screen whose first stop is `top`, as it draws, shows stop
    /// `index`'s controls whole in its room ([`Self::reveal_controls`]).
    pub(super) fn shows_controls(&self, index: usize, top: usize) -> bool {
        self.shows_span(index, top, self.controls_of(index))
    }

    /// Whether the screen whose first stop is `top`, as it draws, shows stop
    /// `index` whole as the keyboard reads it ([`Reveal::Keep`]): its
    /// controls and its words, so a move to it scrolls nothing.
    #[cfg(test)]
    pub(super) fn shows_for_keyboard(&self, index: usize, top: usize) -> bool {
        self.shows_revealed(index, top, Reveal::Keep)
    }

    /// Whether the screen whose first stop is `top`, as it draws, already
    /// shows what `reveal` brings into view of stop `index` whole: for the
    /// keyboard's, its controls and what it draws down to the end of its
    /// words ([`Self::reveal`] shows a stop whose content ends in the room;
    /// a nudge hides less than a control above the first one, its padding);
    /// its controls for a press or a landing; all of it for a landing that
    /// selects it.
    fn shows_revealed(&self, index: usize, top: usize, reveal: Reveal) -> bool {
        self.shows_span(index, top, self.revealed_span(index, reveal))
    }

    /// The part of stop `index` that `reveal` brings into view whole, from
    /// and to under the stop's top ([`Self::shows_revealed`]).
    fn revealed_span(&self, index: usize, reveal: Reveal) -> (f32, f32) {
        match reveal {
            Reveal::Keep => (self.controls_of(index).0, self.ink[index]),
            Reveal::Pressed | Reveal::Land => self.controls_of(index),
            Reveal::LandWhole => (0.0, self.heights[index]),
        }
    }

    /// What the screen whose first stop is `top` draws (THE ONE MEASURE): the
    /// stops it shows ([`Self::view`]); where more follow, the peek after
    /// them drawn on to the page's bottom — the room and the strip under it
    /// kept for a status bar that is not up — down to the last stop any of
    /// which shows; on the last screen, the stops before them, scrolled so the
    /// last stop's content ends at the room's edge. No screen ends on a band
    /// of blank. Where the render seated this screen nudged
    /// ([`Self::press_nudge`]), it draws that much further down the list.
    pub(super) fn screen(&self, top: usize) -> Screen {
        let screen = self.stop_screen(top);
        match self.seated {
            Some(seat)
                if seat.top == screen.view.start && (seat.nudge.abs() > 0.01 || seat.bare) =>
            {
                self.nudged(screen, seat.nudge, seat.bare)
            }
            _ => screen,
        }
    }

    /// Whether the seat the render chose lets go of the caption of the run
    /// the screen whose first stop is `top` starts in ([`Seat::bare`]).
    fn bare_at(&self, top: usize) -> bool {
        self.seated.is_some_and(|seat| seat.top == top && seat.bare)
    }

    /// [`Self::screen`] as the stops seat it, with no nudge.
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
            };
        }
        let view = self.view(top);
        // Live and at full strength: the room, a first stop taller than it,
        // and the words of a stop it shows that end in the strip under it.
        let base = self
            .room
            .max(self.ink[view.start] + self.lead_of(view.start));
        // The list never draws past the page's bottom — its room and the
        // strip under it. A first stop taller than that (a page a tall
        // question squeezes) is cut by the list's edge, live where it shows
        // as every cut is, never run on under the page's edge or over what
        // follows the list.
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
            };
        }
        if view.end < n {
            // THE SCREEN ENDS ON A CUT: it draws the stops after its own down
            // to the last any of which shows — a card after a gap it starts
            // only where [`PEEK_MIN`] of it does, a few points reading as a
            // stray line. Rows that touch (a list box's) go on as a group's
            // rows do: any of one that shows is drawn.
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
            // NO SCREEN ENDS ON BLANK UNDER A CUT: it draws on to the page's
            // bottom. A hard edge — no strip fading under the room: a bar is
            // up, or the page keeps none — cuts the card that crosses it
            // there, live where it shows (the edge rule), and where it would
            // cut through that card's caption the caption's cut words paint
            // nothing, its border, padding and backdrop filling on to the edge
            // ([`Self::edge_caption`]). Where no card after the screen shows
            // [`PEEK_MIN`] of itself, the screen ends on its last card's
            // boundary, the next screen starting at the card after it.
            let drawn = view.start..end;
            return Screen {
                live: self.live_on(&drawn, 0.0, None, base),
                edge_caption: self.edge_caption(&drawn, 0.0, reach),
                drawn,
                view,
                offset: 0.0,
                height: reach,
                fades: true,
                whole: false,
                caption: None,
            };
        }
        // The last screen: from the stops before it, the least that fill it.
        let end_of = |start: usize| {
            let content = self.shown_by(start, n - 1);
            let bottom = self.span(start..n);
            (content, bottom)
        };
        let mut start = view.start;
        while start > 0 {
            let (content, bottom) = end_of(start);
            if content + 1.0 >= self.room || bottom + 1.0 >= self.room + self.kept {
                break;
            }
            start -= 1;
        }
        let (content, bottom) = end_of(start);
        let offset = (content - self.room)
            .max(bottom - self.room - self.kept)
            .max(0.0);
        let height = (bottom - offset).min(reach).max(self.room.min(bottom));
        let caption = self.top_caption(start..n, offset);
        Screen {
            view,
            live: self.live_on(&(start..n), offset, caption, self.room),
            drawn: start..n,
            offset,
            height,
            fades: false,
            whole: false,
            caption,
            edge_caption: None,
        }
    }

    /// The stops a screen drawing `drawn`, scrolled `offset` past its first
    /// stop's seat with `caption` at its top, SHOWS (THE ONE MEASURE, on the
    /// screen as it draws): the run of them that each start under the room's
    /// top — under a caption kept there over their run — and are shown where
    /// they are ([`Self::shows_at`]; the list's last stop where what it draws
    /// ends in the room, the strip under it holding nothing of a stop with
    /// none after it). Empty where it shows none whole. THE LAST STOP IS
    /// SHOWN BY WHAT IT DRAWS, as every stop is: a screen that draws its
    /// content whole, its card's closing padding cut by the edge, shows it,
    /// though only the last screen scrolls on to the list's end
    /// ([`Self::view`], [`Self::limit`]). Measured to the list's end here,
    /// the screen before the last showed the last stop's words and controls
    /// whole uncounted, and Page Up from the last screen turned back onto it
    /// (Software Update, Appearance, Cursor Styles, Window at 60x20).
    fn shown_on(
        &self,
        drawn: &Range<usize>,
        offset: f32,
        caption: Option<TopCaption>,
    ) -> Range<usize> {
        let n = self.len();
        let mut shown: Option<Range<usize>> = None;
        for stop in drawn.clone() {
            let at = self.stop_top(drawn.start, stop) - offset;
            let whole = at + self.ink_top[stop] >= self.caption_floor(caption, drawn, stop) - 0.01
                && if stop + 1 == n {
                    at + self.controls_of(stop).1 <= self.room + 0.01
                        && at + self.ink[stop] <= self.room + 0.01
                } else {
                    self.shows_at(at, stop, self.room)
                };
            match (whole, shown.as_mut()) {
                (true, Some(run)) => run.end = stop + 1,
                (true, None) => shown = Some(stop..stop + 1),
                (false, Some(_)) => break,
                (false, None) => {}
            }
        }
        shown.unwrap_or(drawn.start..drawn.start)
    }

    /// How far from the list's top a screen drawing `drawn`, scrolled
    /// `offset` with `caption` at its top, is live and drawn at full
    /// strength: `base` (its room, or a first stop taller than that), and on
    /// down to the end of the words of the last stop it shows where those end
    /// in the strip under the room — words, no control, so a bar that comes
    /// takes nothing a person can press — its peek fading only under them.
    fn live_on(
        &self,
        drawn: &Range<usize>,
        offset: f32,
        caption: Option<TopCaption>,
        base: f32,
    ) -> f32 {
        let shown = self.shown_on(drawn, offset, caption);
        match shown.end.checked_sub(1) {
            Some(last) if !shown.is_empty() && last + 1 < self.len() => base.max(
                (self.stop_top(drawn.start, last) - offset + self.ink[last])
                    .min(self.room + self.overhang),
            ),
            _ => base,
        }
    }

    /// THE STOPS THE SCREEN WHOSE FIRST STOP IS `top` SHOWS, as it draws
    /// ([`Self::screen`]: scrolled where the render seated it, the last
    /// screen scrolled to the list's end): what Page Up and Down turn over,
    /// and where the keyboard a page turn takes with it lands. A seated,
    /// unscrolled screen shows its [`Self::view`]; one that shows no stop
    /// whole shows its first, whatever its height, as the view does.
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

    /// PAGE DOWN: the first stop of the screen after the one whose first stop
    /// is `top` — the stop after the last that screen shows, seated at its
    /// stop — or the list's last screen where that is further.
    pub(super) fn page_after(&self, top: usize) -> usize {
        self.shown(top).end.min(self.limit())
    }

    /// PAGE UP from the screen whose first stop is `top`: the screen that ends
    /// on the stop before the first this one shows, its first stop and how
    /// far past that stop's seat it is scrolled. Seated at a stop where a
    /// screen seated there ends on it ([`Self::page_before`]); else scrolled
    /// so that stop's content ends at the room's edge, as the last screen
    /// ends on the list's last stop, the stop above it cut at the list's top —
    /// a seated screen would show again a stop this one shows or never show
    /// one before it. At the list's top, its first screen.
    pub(super) fn page_up(&self, top: usize) -> (usize, Option<f32>) {
        let first = self.shown(top).start;
        if first == 0 {
            return (0, None);
        }
        // The screen seated there, as it draws: one that draws `first` whole
        // shows it again.
        let seated = self.page_before(first);
        let seated_screen = self.stop_screen(seated.min(self.limit()));
        if seated == 0
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
        // The stops from `start` on to `first`, the last of them ending at
        // the room's edge: the greatest `start` whose stops fill the room.
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

    /// The caption a screen drawing `drawn`, scrolled `offset` into its first
    /// stop, keeps at the list's top ([`Screen::caption`]): a run of a group
    /// over the top keeps its caption whole there, and so does a whole card
    /// the scroll would cut into or past its caption — its content scrolls
    /// under the caption. Where it has too little content to scroll under it
    /// (`list_screen_node`), the caption the edge would cut through paints
    /// none of its words. The last screen, and a screen a press nudged
    /// ([`Self::nudged`]), read this one rule.
    fn top_caption(&self, drawn: Range<usize>, offset: f32) -> Option<TopCaption> {
        match self.runs(drawn).first() {
            Some(ScrollRun::Group { group, rows }) => {
                let kept = offset > 0.01 && offset < self.run_rows(*group, rows) - 0.01;
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

    /// Where a screen drawing `drawn`, scrolled `offset`, `height` tall, cuts
    /// through the caption of the card it draws last, from that card's top:
    /// the edge, and where the caption ends ([`Screen::edge_caption`]). Only
    /// where the edge is hard — no strip fades under the room — and the card
    /// peeks in after a gap: where the strip fades, the peek is a picture
    /// drawn on to the page's bottom. A CAPTION IS DRAWN WHOLE OR NOT AT ALL:
    /// the words the edge would cut paint nothing, and the card goes on to
    /// the edge, so no band of blank is left under it.
    fn edge_caption(&self, drawn: &Range<usize>, offset: f32, height: f32) -> Option<(f32, f32)> {
        if self.overhang > 0.01 {
            return None;
        }
        let last = drawn.end.checked_sub(1)?;
        let (top, bottom) = self.caption_band(drawn.start, last)?;
        let edge = height + offset;
        let card = self.card_top(drawn.start, last);
        (edge > top + 0.5 && edge < bottom - 0.5).then_some((edge - card, bottom - card))
    }

    /// Where the words of stop `index`'s card caption begin and end on a
    /// screen whose first stop is `from`: `None` where the stop does not start
    /// a card after a gap, or its card has no caption the plan knows.
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

    /// THE PRESS'S NUDGE: how much further the screen whose first stop is
    /// `top` scrolls so a control of stop `index` an edge of the room cuts —
    /// some of it in the room, where a press lands — shows whole: its hidden
    /// part, no more, so the control stays under the pointer that pressed it,
    /// as a platform list scrolls a clicked row into view. Up (positive) for
    /// one the room's bottom edge cuts, so it ends at that edge; down
    /// (negative) for one cut at the top (the last screen's first stop, or
    /// under the caption it keeps), so it starts where the room does. A
    /// stop's worth of scroll — a row is taller than the part of a control a
    /// cut leaves — would carry it out from under the pointer. Measured on
    /// the screen as it draws, nudged already where an earlier press nudged
    /// it ([`Self::screen`]), and the whole nudge from the stops' seat
    /// returned. `None` where the screen shows it whole or does not draw it,
    /// where it is taller than the room, or where the list ends too soon to
    /// scroll that far ([`Self::reveal_controls`] then moves by stops).
    pub(super) fn press_nudge(&self, index: usize, top: usize) -> Option<f32> {
        let top = top.min(self.limit());
        let seat = self.stop_screen(top).offset;
        let screen = self.screen(top);
        if screen.whole || !screen.drawn.contains(&index) {
            return None;
        }
        let start = screen.drawn.start;
        let at = self.stop_top(start, index) - screen.offset;
        let (from, to) = self.controls_of(index);
        let (control_top, control_bottom) = (at + from, at + to);
        let live = screen.live;
        // Where the room starts for the control: under a caption kept whole
        // over the run the screen starts in, else the list's top — on the
        // screen as it draws, and on the screen nudged.
        let floor_now = self.caption_floor(screen.caption, &screen.drawn, index);
        let nudge = if control_bottom > live + 0.01 && control_top < live - 0.01 {
            control_bottom - live
        } else if control_top < floor_now - 0.01 && control_bottom > floor_now + 0.01 {
            control_top - floor_now
        } else {
            return None;
        };
        let offset = screen.offset + nudge;
        let then = self.nudged(self.stop_screen(top), offset - seat, self.bare_at(top));
        let floor_then = self.caption_floor(then.caption, &then.drawn, index);
        // The list's ends bound the scroll: never above its first stop, and
        // its last card never comes up past the room's edge.
        let end = self.card_bottom(start, self.len() - 1) - screen.offset;
        (offset >= -0.01
            && nudge <= end - live + 0.01
            && control_top - nudge >= floor_then - 0.01
            && control_bottom - nudge <= live + 0.01)
            .then_some(offset - seat)
    }

    /// Whether the screen whose first stop is `top` can still stand scrolled
    /// `nudge` past its stops' seat ([`Self::press_nudge`]) as the list
    /// measures now: never above its first stop, and its last card never up
    /// past the room's edge — a window that grew, or stops that went, can
    /// leave an earlier press's nudge with nothing to stand on.
    fn nudge_fits(&self, top: usize, nudge: f32) -> bool {
        let screen = self.stop_screen(top.min(self.limit()));
        if screen.whole || self.len() == 0 {
            return false;
        }
        let end = self.card_bottom(screen.drawn.start, self.len() - 1) - screen.offset;
        screen.offset + nudge >= -0.01 && nudge <= end - screen.live + 0.01
    }

    /// Whether `seat` can still stand as the list measures now
    /// ([`Self::nudge_fits`]); a seat past a caption ([`Seat::bare`]) needs
    /// only that the list's last card not come up past the room's edge.
    fn seat_fits(&self, seat: Seat) -> bool {
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

    /// `screen` scrolled `nudge` further ([`Self::press_nudge`]): the stops
    /// that come into the page then drawn on, the first cut at the list's top
    /// edge and live where it shows, its captions kept and landed on as the
    /// last screen's and every screen's are ([`Self::top_caption`],
    /// [`Self::edge_caption`]).
    fn nudged(&self, screen: Screen, nudge: f32, bare: bool) -> Screen {
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
        // A press's nudge back toward the run's caption never brings back
        // one the screen let go: it would come down over the control the
        // nudge shows whole under the pointer. A seat past the caption
        // lets it go.
        let caption = self
            .top_caption(drawn.clone(), offset)
            .map(|caption| TopCaption {
                kept: !bare
                    && caption.kept
                    && (nudge >= 0.0 || screen.caption.is_some_and(|caption| caption.kept)),
                ..caption
            });
        // A seat past the caption ([`Seat::bare`]) scrolls its first stop's
        // lead off: what it draws is live as far as the stop's words reach,
        // no further than the room where they end in it.
        let base = if bare {
            self.room.max(screen.live - nudge)
        } else {
            self.room
        };
        Screen {
            live: self.live_on(&drawn, offset, caption, base),
            edge_caption: self.edge_caption(&drawn, offset, reach),
            drawn,
            offset,
            height: reach,
            // Scrolled back from the last screen's end, the list goes on past
            // the room again.
            fades: screen.fades || offset < screen.offset - 0.01,
            caption,
            ..screen
        }
    }

    /// The stop holding control `key` (a segment: its control's).
    pub(super) fn stop_of(&self, key: &UiKey) -> Option<usize> {
        let key = self.resolve(key);
        self.keys.iter().position(|keys| keys.contains(key))
    }

    /// Say which live previews the last stop added draws: the
    /// [`UiContent::SettingsPreview`] nodes under `nodes`.
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

    /// Where control `key` (a segment: its control) is among the page's
    /// controls, top to bottom.
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
    /// A control the keyboard just reached, kept in view until the list is
    /// scrolled: its whole stop, words too ([`ScrollPlan::reveal`]).
    #[default]
    Keep,
    /// A control a person pressed or changed
    /// ([`SettingsViewState::reveal_pressed`]): a screen that shows it whole
    /// keeps its place, its note cut or not, and one an edge cuts scrolls
    /// whole by its hidden part, under the pointer
    /// ([`ScrollPlan::press_nudge`]), else by the least stops that show it
    /// whole ([`ScrollPlan::reveal_controls`]).
    Pressed,
    /// Where the page LANDS ([`SettingsViewState::land_on_control`]): its
    /// stop's controls whole in the list's room; the next event after a
    /// render has seated it ends it.
    Land,
    /// A landing that SELECTS its control's whole stop
    /// ([`SettingsViewState::land_on_stop`]): all of the stop whole in the
    /// room, its words under its controls and its selection's bottom edge
    /// too, wherever the room holds it ([`ScrollPlan::reveal_selected`]).
    LandWhole,
}

/// Which list a record measured: a page's list stays the same list while its
/// route, its search and the category sheet stay put.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ScrollContext {
    route: SettingsRoute,
    search: String,
    categories: bool,
}

/// What the last render of a scrolling page measured and where it put the
/// list. `drawn` and `landed` are consumed by the next event
/// ([`SettingsViewState::settle_page_scroll`]).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ScrollRecord {
    context: ScrollContext,
    plan: ScrollPlan,
    top: usize,
    drawn: Option<usize>,
    /// The landing this render seated ([`SettingsViewState::land_on_control`]):
    /// its control, found on the list and followed.
    landed: Option<UiKey>,
}

impl ScrollRecord {
    /// The stop the list's screen started at.
    pub(super) fn top(&self) -> usize {
        self.top
    }

    /// Whether `state` shows the list this record was drawn for.
    pub(super) fn same_context(&self, state: &SettingsViewState) -> bool {
        self.context == state.scroll_context()
    }
}

impl SettingsViewState {
    pub(super) fn scroll_context(&self) -> ScrollContext {
        ScrollContext {
            route: self.route,
            search: self.search.trim().to_string(),
            categories: self.compact_navigation,
        }
    }

    /// Seat the list a render measured: clamp the scroll so the last screen
    /// is full, follow a pending reveal ([`Self::page_reveal`]), and record
    /// the plan, the bound and the stops shown for the reducer. Returns the
    /// stops this screen shows.
    pub(super) fn seat_scroll(&self, plan: &mut ScrollPlan) -> Range<usize> {
        let limit = plan.limit();
        let mut top = self.page_scroll.min(limit);
        let focus = self.common.last_focus.as_ref();
        // Where the last screen of this list showed `key`, if nothing has
        // scrolled the list since: its first stop and `key`'s stop then.
        let last_shown = |key: &UiKey| {
            let record = self.scroll_record.borrow();
            let record = record.as_ref()?;
            let stop = record.plan.stop_of(key)?;
            (record.context == self.scroll_context()
                && record.top == top
                && record.plan.shown(record.top).contains(&stop))
            .then_some((record.top, stop))
        };
        let first_screen = self
            .scroll_record
            .borrow()
            .as_ref()
            .is_none_or(|record| record.context != self.scroll_context());
        // THE NUDGE STANDS: the hidden part a press scrolled a cut control by
        // ([`ScrollPlan::press_nudge`]) is part of the list's place, as a
        // platform list holds where a click scrolled it — the last screen's,
        // where nothing has scrolled the list since
        // ([`Self::drop_list_nudge`]).
        let carried = self
            .scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.context == self.scroll_context() && record.top == top)
            .and_then(|record| record.plan.seated)
            .filter(|seat| seat.top == top);
        // THE LIST HOLDS ITS PLACE: where stops above the control a person
        // is on came or went since the last screen (a note a choice retired,
        // a row Reset took away, a row above listed in parts or whole again
        // as a republish changed its height), the list moves by as many, so
        // that control stays where it was on screen — and with no control
        // the person is on, the last screen's first stop, by the first
        // control it holds (a list at its top stays there). A seat it
        // carried moves with it.
        let shown_first = || {
            let record = self.scroll_record.borrow();
            let record = record.as_ref()?;
            (record.context == self.scroll_context() && record.top == top && record.top > 0)
                .then(|| record.plan.keys.get(record.top)?.first().cloned())
                .flatten()
        };
        let first_control = (self.page_reveal.is_none() && focus.is_none())
            .then(shown_first)
            .flatten();
        let anchor = self
            .page_reveal
            .as_ref()
            .or(focus)
            .or(first_control.as_ref());
        let held = anchor.and_then(last_shown);
        let mut carried = carried;
        if let (Some(key), Some((last_top, last))) = (anchor, held)
            && let Some(index) = plan.stop_of(key)
        {
            let moved = (last_top + index).saturating_sub(last).min(limit);
            carried = carried.map(|seat| Seat { top: moved, ..seat });
            top = moved;
        }
        plan.seated = carried.filter(|seat| seat.top == top && plan.seat_fits(*seat));
        let mut landed = None;
        let landing = matches!(self.reveal_as, Reveal::Land | Reveal::LandWhole);
        // A LANDING YIELDS TO A KEYBOARD ALREADY ON THE LIST: a list not yet
        // drawn since it was reset lands on its current item (the saved
        // cursor style, the theme in use), but where the keyboard reached
        // another of its stops before that first screen (a press a driver
        // sent ahead of the frame), that stop is the one shown, and the
        // landing is let go as drawn: never a first screen without the
        // control the keyboard is on.
        let keyboard_stop = focus
            .filter(|key| landing && first_screen && self.page_reveal.as_ref() != Some(*key))
            .and_then(|key| plan.stop_of(key));
        let reveal = match keyboard_stop {
            Some(index) => Some((index, Reveal::Keep)),
            None => self
                .page_reveal
                .as_ref()
                .and_then(|key| plan.stop_of(key))
                .map(|index| (index, self.reveal_as)),
        };
        if let Some((index, reveal_as)) = reveal {
            // A press, a change or a landing shows the control whole where it
            // is not already (a landing that selects its stop, all of the
            // stop; a press on one an edge cuts by its hidden part, so it
            // stays under the pointer); the keyboard shows its whole stop,
            // words too. A screen a press nudged that shows it so keeps its
            // place, nudge and all.
            if landing {
                landed.clone_from(&self.page_reveal);
            }
            if plan.seated.is_none() || !plan.shows_revealed(index, top, reveal_as) {
                // Measured on the screen as it draws, an earlier nudge in it.
                let nudge = if reveal_as == Reveal::Pressed {
                    plan.press_nudge(index, top)
                } else {
                    None
                };
                let bare = plan.bare_at(top);
                plan.seated = nudge.map(|nudge| Seat {
                    bare,
                    ..Seat::nudged(top, nudge)
                });
                // A PRESS WHOSE CONTROL THE SCREEN DOES NOT DRAW YET — a Timer
                // opened under its button, its choices a part of the row
                // listed after the button's — scrolls by the least that shows
                // it, from where the list stands ([`ScrollPlan::seat_span`]),
                // so what was pressed stays on the screen: a reveal by whole
                // stops seated the choices' part first and the row went
                // off the top.
                let reached = (nudge.is_none() && reveal_as == Reveal::Pressed)
                    .then(|| plan.seat_span(index, top, plan.revealed_span(index, reveal_as)))
                    .flatten();
                if let Some(seat) = reached {
                    plan.seated = Some(seat);
                } else if nudge.is_none() {
                    top = match reveal_as {
                        Reveal::Keep => plan.reveal(index, top),
                        Reveal::Pressed | Reveal::Land => plan.reveal_controls(index, top),
                        Reveal::LandWhole => {
                            let (landed, seat) = plan.land_selected(index, top);
                            plan.seated = seat;
                            landed
                        }
                    };
                }
            }
            // WHERE THE STOPS' SEAT STILL CUTS WHAT THE REVEAL MUST SHOW in
            // the list's room — under the caption it keeps, or at the room's
            // edge, which cuts a first stop taller than the room though it
            // reads as live past it — the list scrolls on into the stop
            // ([`ScrollPlan::seat_span`]). A landing that selects its stop
            // has its own seat ([`ScrollPlan::land_selected`]).
            if plan.seated.is_none() && reveal_as != Reveal::LandWhole {
                let span = plan.revealed_span(index, reveal_as);
                plan.seated = plan.seat_span(index, top, span);
            }
        } else if let Some(key) = focus
            && let Some(index) = plan.stop_of(key)
            // The list's first screen shows the control the keyboard is on,
            // its whole stop; after that, nothing scrolled the list since the
            // last screen, which showed it: a status bar that came, a shorter
            // window, a taller preview keep that control in view — a bar that
            // takes the strip under the facts of the row it is on covers
            // only those words and never moves the list.
            && (first_screen || held.is_some())
        {
            let reveal = if first_screen {
                Reveal::Keep
            } else {
                Reveal::Pressed
            };
            if !plan.shows_revealed(index, top, reveal) {
                plan.seated = None;
                top = match reveal {
                    Reveal::Keep => plan.reveal(index, top),
                    _ => plan.reveal_controls(index, top),
                };
            }
        }
        let view = plan.view(top);
        self.record_result_page_limit(limit);
        self.page_window.set(Some(plan.shown(top).len().max(1)));
        self.scroll_record.replace(Some(ScrollRecord {
            context: self.scroll_context(),
            plan: plan.clone(),
            top,
            drawn: Some(top),
            landed,
        }));
        view
    }

    /// Adopt where the last render put this page's list, once: the scroll it
    /// clamped to a grown window or a changed layout, or the stop it scrolled
    /// to so a control stays in view.
    ///
    /// A LANDING STANDS UNTIL A RENDER HAS SEATED IT: only the render that
    /// found the landed control on the list and followed it lets the landing
    /// go — not an event that comes before any render has drawn it. A page
    /// already open is landed on by `Harness settings…` (Settings ▸ Harness's
    /// `harness/land`) while the refreshes its arrival started are still
    /// out, and their replies (`HarnessChanged`, `PackagesChanged`,
    /// `UpdateChanged`) reach the view before its next frame: each settles
    /// against the record of the frame drawn BEFORE the landing, which
    /// never saw it.
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
        // The landing is drawn: the list holds the place it chose.
        let landed = record.landed.take();
        if matches!(self.reveal_as, Reveal::Land | Reveal::LandWhole)
            && landed.is_some()
            && landed == self.page_reveal
        {
            self.reveal_as = Reveal::Keep;
            self.page_reveal = None;
        }
    }

    /// A control the keyboard, a pointer or a commit just moved to stays in
    /// view ([`Self::page_reveal`]) until the list is scrolled — except on a
    /// list not drawn since it was reset, which keeps the reveal its landing
    /// chose ([`Self::reset_page_scroll`]): the focus a navigation lands on
    /// (a sidebar item, the toolbar's button) is never on the list, so it
    /// could only take the saved style's row out of view. A press (`pressed`)
    /// only brings what an edge cut into view ([`Self::reveal_pressed`]).
    ///
    /// A LANDING STANDS UNTIL A RENDER HAS SEATED IT over a focus the list
    /// does not hold: Settings ▸ Themes' Dark | Light segment, pinned over
    /// the list, lands the list on its side's row while the keyboard goes to
    /// the segment, and the segment's move must not take that landing away.
    pub(super) fn reveal_moved_focus(&mut self, before: Option<UiKey>, pressed: bool) {
        if self.page_reveal.is_some() && self.scroll_record.borrow().is_none() {
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

    /// THE LIST SCROLLED (a wheel, a key, a reader's scroll, a page opened
    /// at its top): the nudge a press gave it ([`ScrollPlan::press_nudge`])
    /// goes, and it seats on its stops again — at an end of the list too,
    /// where the scroll moves no stop, so a wheel up at the top shows the top.
    pub(super) fn drop_list_nudge(&self) {
        if let Some(record) = self.scroll_record.borrow_mut().as_mut() {
            record.plan.seated = None;
        }
    }

    /// Whether the list the last render of this page measured holds `key`.
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

    /// The stops the last render of this page showed, as it drew them
    /// ([`ScrollPlan::shown`]).
    pub(super) fn scroll_shown(&self) -> Option<(ScrollPlan, Range<usize>)> {
        let context = self.scroll_context();
        self.scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.context == context)
            .map(|record| (record.plan.clone(), record.plan.shown(record.top)))
    }

    /// PAGE DOWN (`forward`) OR PAGE UP ON THE PAGE'S LIST: from the screen
    /// the last render drew, to the stop after the last it showed whole, or
    /// to the screen that ends on the stop before its first — seated at a
    /// stop where one does, else scrolled so that stop ends at the room's
    /// edge ([`ScrollPlan::page_up`]), a place the list holds as it holds a
    /// press's nudge until something scrolls it. `false` where the page has
    /// no list the last render measured (Messages measures its own).
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
                    (top, record.plan.limit())
                })
        };
        let Some((top, limit)) = turned else {
            return false;
        };
        self.reduce_page_scroll(limit, super::SettingsPageScrollCommand::Absolute(top));
        true
    }

    /// Bring `key` into view at the next render when the page's list holds
    /// it off screen, as a person scrolls to a row before pressing it, so a
    /// control verb reaches every listed control; `false` when the list the
    /// last render measured has no such control.
    pub(crate) fn reveal_listed_control(&mut self, key: &UiKey) -> bool {
        let listed = self
            .scroll_plan()
            .is_some_and(|plan| plan.stop_of(key).is_some())
            // A row a closed disclosure holds (a Themes Community theme)
            // is listed once the disclosure opens, as a hand opens it.
            || self.open_disclosure_holding(key);
        if listed {
            self.reveal_control(key.clone());
        }
        listed
    }

    /// Keep `key` in view at the next render, its whole stop: the keyboard
    /// reached it.
    pub(super) fn reveal_control(&mut self, key: UiKey) {
        self.page_reveal = Some(key);
        self.reveal_as = Reveal::Keep;
    }

    /// Keep control `key` in view at the next render, only bringing it whole
    /// into the room where an edge cuts it: a person pressed it, or changed
    /// what it shows, so a screen that shows it whole never moves under the
    /// pointer, and the status bar the change brings up never covers it.
    pub(super) fn reveal_pressed(&mut self, key: UiKey) {
        self.page_reveal = Some(key);
        self.reveal_as = Reveal::Pressed;
    }

    /// A press on list control `key` an edge cuts brings it whole into the
    /// list's room ([`Self::reveal_pressed`]), however the press came: a
    /// pointer moves the keyboard to it before its action arrives, and a
    /// screen reader's press moves no keyboard at all. One the screen shows
    /// whole moves nothing, and a reveal the action asked for itself (its
    /// page_reveal no longer `before`) stands.
    pub(super) fn reveal_press(&mut self, key: &UiKey, before: Option<&UiKey>) {
        if self.page_reveal.as_ref() != before {
            return;
        }
        let context = self.scroll_context();
        let cut = self
            .scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.context == context)
            .and_then(|record| {
                let index = record.plan.stop_of(key)?;
                // As the screen draws, a press's nudge in it: one an edge
                // cuts on the list's last screen is cut too, though no stop
                // further on shows it.
                Some(!record.plan.shows_controls(index, record.top))
            });
        if cut == Some(true) {
            self.reveal_pressed(key.clone());
        }
    }

    /// A page opened FOR one of its controls (the Autopilot menu's `Harness settings…`,
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §10.3.3 item 8): the
    /// list LANDS on `key`'s stop — a landing's reveal, which the render
    /// follows whether or not one has measured the list yet, and which a
    /// stop already drawn whole leaves where it is — and the keyboard stays
    /// where it was.
    pub(super) fn land_on_control(&mut self, key: UiKey) {
        self.page_reveal = Some(key);
        self.reveal_as = Reveal::Land;
    }

    /// [`Self::land_on_control`] for a landing that SELECTS `key`'s whole
    /// stop (a Settings ▸ Harness SESSIONS row, drawn on the accent card
    /// around its row, its state, its day and its timer: §10.3.1): the list
    /// lands so all of the stop is drawn wherever the screen holds it — not
    /// only its buttons, which left the words under them and the selection's
    /// bottom edge off screen.
    pub(super) fn land_on_stop(&mut self, key: UiKey) {
        self.page_reveal = Some(key);
        self.reveal_as = Reveal::LandWhole;
    }

    /// A page opened AT ITS TOP (Settings ▸ Harness from View ▸ Coding Agent
    /// Harness…, §10.3.1): the list starts at its first stop, and whatever it
    /// was keeping in view goes — a landing still waiting for a row the page
    /// does not list yet among them, so that row never pulls the page when it
    /// comes. The keyboard stays where it was.
    pub(super) fn land_at_top(&mut self) {
        self.drop_list_nudge();
        self.page_scroll = 0;
        self.page_reveal = None;
        self.reveal_as = Reveal::Keep;
    }

    /// Forget the list: a new route, search or sheet starts at its top —
    /// except that a list with a current item opens with its row in view:
    /// Cursor Styles the saved style's, as Down and every cursor style
    /// control land on it, and Settings ▸ Themes the highlighted theme's (its
    /// ✓), whole, as its Dark | Light segment lands on it (the review of the
    /// owner's pages, 2026-10-20: a theme listed below the first screen
    /// opened out of view, the page naming a theme no row showed), where the
    /// list shows that row ([`SettingsViewState::showcase_landing`]). The
    /// keyboard stays where it is.
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

    /// UP AND DOWN MOVE THE KEYBOARD THROUGH THE PAGE (design ruling 264):
    /// from a control in the list to the one before or after it, wherever it
    /// is, and the list follows. `false` when the keyboard is not on one of
    /// the list's controls, or there is no next one: the list scrolls instead.
    pub(super) fn step_list_focus(&mut self, lines: i32) -> bool {
        let Some(plan) = self.scroll_plan() else {
            return false;
        };
        let Some(current) = self.common.last_focus.clone() else {
            return false;
        };
        let controls: Vec<&UiKey> = plan.controls().map(|(_, key)| key).collect();
        // A segment steps as its control does: on to the next row.
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

    /// A PAGE TURN TAKES THE KEYBOARD WITH IT: where Page Down (`forward`)
    /// or Page Up scrolled the control the keyboard is on off the screen, the
    /// keyboard moves to the first control the new screen shows (Page Up: the
    /// last), so the next arrow key steps on from there and never jumps the
    /// list back. On Cursor Styles the preview follows the keyboard, so it
    /// follows the page too. A keyboard outside the list stays where it is.
    pub(super) fn keep_list_focus_on_screen(&mut self, forward: bool) {
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
        if plan
            .shown(self.page_scroll.min(plan.limit()))
            .contains(&stop)
        {
            return;
        }
        self.focus_list_screen(forward);
    }

    /// The keyboard to the first control the list's screen shows (`forward`;
    /// else its last), ringed. A screen with no control leaves it where it
    /// is.
    pub(super) fn focus_list_screen(&mut self, forward: bool) {
        let Some(plan) = self.scroll_plan() else {
            return;
        };
        let shown = plan.shown(self.page_scroll.min(plan.limit()));
        let mut on_screen = plan
            .controls()
            .filter(|(stop, _)| shown.contains(stop))
            .map(|(_, key)| key);
        let target = if forward {
            on_screen.next()
        } else {
            on_screen.last()
        };
        if let Some(key) = target.cloned() {
            self.land_list_focus(key);
        }
    }

    /// HOME AND END TAKE THE KEYBOARD TO THE LIST'S ENDS: from a control in
    /// the list, Home (`first`) to its first control and End to its last,
    /// wherever on the list the keyboard was, even on a screen that already
    /// shows that end — so the next arrow steps on from there, and on Cursor
    /// Styles the preview shows the row it lands on. A keyboard outside the
    /// list stays where it is.
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

    /// Put the keyboard on list control `key`, ringed and kept in view. A
    /// LANDING OPENS NO EDIT: the arrows, Page Up/Down and Home/End put the
    /// keyboard on a text field as on any control, with no caret, so Home
    /// and End go on acting on the list; typing (or a click, or Tab) opens
    /// the edit. An edit on the control left ends, keeping its draft.
    fn land_list_focus(&mut self, key: UiKey) {
        self.clear_field_edit();
        self.list_carried = Some(key.clone());
        self.common.last_focus = Some(key.clone());
        self.common.focus_visible = true;
        self.reveal_control(key);
    }

    /// THE SIDEBAR IS ONE TAB STOP (the macOS sidebar convention): Tab or
    /// Shift-Tab from one of its rows to the window's next control (`next`,
    /// in Tab `order`) leaves the sidebar in one step — Tab to the first
    /// control after it, Shift-Tab to the one before it — and entering it
    /// from outside lands on the selected route's row. Its arrow keys move
    /// between rows. `None` keeps `next`.
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

    /// TAB WALKS THE WHOLE LIST, not just its screen: Tab (`forward`) or
    /// Shift-Tab from `current` to the window's next control in Tab `order`
    /// (`next`) is kept within the list's screen, but where it would leave
    /// the list from one of its controls it moves to the list's next or
    /// previous control instead, and where it enters the list from a control
    /// outside it starts at the list's first control (Shift-Tab: its last);
    /// the list scrolls to it. A focus that is no stop of `order` and no
    /// control of the list (it left the order, s-25) is not entering: `next`
    /// is already where it stood. `None` keeps `next`.
    pub(crate) fn tab_past_screen(
        &self,
        current: &UiKey,
        next: &UiKey,
        forward: bool,
        order: &[UiKey],
    ) -> Option<UiKey> {
        let plan = self.scroll_plan()?;
        let controls: Vec<&UiKey> = plan.controls().map(|(_, key)| key).collect();
        // A segment is its control here: Tab from one leaves the control,
        // and never lands on another segment of it.
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

/// Whether `node` holds a control at all, an unavailable one too: a disabled
/// slider still belongs to its row, so its words keep with it.
fn holds_control(node: &UiNode) -> bool {
    !node.paint_only && (is_control(node) || node.children.iter().any(holds_control))
}

/// Whether `node`, a card's row, is A NOTE ON THE ROWS OVER IT: words and no
/// control, every line of them a status line ([`SemanticRole::Status`]) — the
/// note a run of rows shares, which each of their controls says
/// ([`SplitCard::stops`]). It says how the rows over it stand, so it never
/// heads the control after it.
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

/// Where the nodes under `node` that `counts` picks sit in it, laid out
/// `height` tall as the compiler lays it out: from the top of the first to
/// the bottom of the last, `None` where there are none. `counts` says of a
/// node whether all of it counts (`Some(true)`), none of it (`Some(false)`),
/// or what of its children does (`None`).
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

/// [`extent_where`] over `node`'s children alone, inside its padding.
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

/// A control: a button, a switch, a slider, a text field, a colour wheel,
/// or a focusable custom node.
fn is_control(node: &UiNode) -> bool {
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

/// Where the controls `node` holds sit in it, laid out `height` tall as the
/// compiler lays it out: from the top of the first to the bottom of the
/// last, `None` where it holds none. A setting row's control sits under its
/// padding and over its note, so a list edge that cuts the row can be told
/// whether it cuts the control.
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

/// Where the controls of a stop that is `node`, `height` tall, sit in it
/// ([`ScrollPlan::set_control_rows`]): row by row where it is a column of
/// rows (a card: its words, then a row of buttons, then more), so the plan
/// knows where its first row of controls ends; else all of them at once.
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

/// Where the first row of controls under `node`, laid out `height` tall,
/// sits in it: the control itself, the first row of a column that holds one
/// (looked into where that row is a column of rows too: a card's rows of
/// buttons), or all of a row's controls. `None` where it holds none.
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

/// The rows of `node`, a group laid out in a column `height` tall, as the
/// compiler lays them out: where each starts in it, how tall it is, and the
/// row. `None` for any other node.
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

/// The key a segmented control is listed by ([`UiNode::segmented`]): its
/// selected segment, else its first that takes the keyboard — the segment
/// that carries the setting's own key on a Settings row.
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

/// Whether all of `node` is drawn where it is laid out: anything but a
/// group, and a group that paints a backdrop of its own (a card, a sidebar,
/// a code canvas); of any other group, what its children draw.
fn paints_whole(node: &UiNode) -> Option<bool> {
    match &node.content {
        UiContent::Group(spec) if !spec.paints_backdrop() => None,
        _ => Some(true),
    }
}

/// Where what `node` draws sits in it, laid out `height` tall as the compiler
/// lays it out: from the top of the first thing it paints to the bottom of
/// the last — words, controls, pictures, a card's backdrop — the padding
/// around them aside. `None` where it paints nothing.
pub(super) fn ink_extent(node: &UiNode, height: f32) -> Option<(f32, f32)> {
    extent_where(node, height, &paints_whole)
}

/// Where what a stop that is `node`, `height` tall, holds sits in it: the
/// words, controls and pictures inside its own card's padding, the backdrop
/// of that card aside ([`ScrollPlan::measure`]).
fn content_extent(node: &UiNode, height: f32) -> Option<(f32, f32)> {
    match &node.content {
        UiContent::Group(_) => children_extent(node, height, &paints_whole),
        _ => Some((0.0, height)),
    }
}

/// Where a card stop's own caption sits in it: its first child where that is
/// a heading ([`is_heading`]) over the rest of a column card, under the card's
/// top padding, and the gap after it. `None` for anything else.
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
            // As [`keep_caption`] measures the rows it scrolls under it.
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

/// Every control under `node` a person can focus (or will, once it is done
/// saving), in authoring order; a segmented control once, by its own key
/// ([`segmented_control_key`]).
pub(super) fn focus_keys(node: &UiNode) -> Vec<UiKey> {
    fn walk(node: &UiNode, out: &mut Vec<UiKey>) {
        if node.paint_only {
            return;
        }
        if node.segmented {
            out.extend(segmented_control_key(node));
            return;
        }
        // A control that is busy saving is still the row's control.
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

/// The list itself: `children` are the screen's runs, top to bottom, the
/// stops it shows and no peek. A screen that shows only part of the list
/// carries the scroll thumb over its trailing edge ([`MESSAGES_SCROLL_AUDIT`],
/// paint-only); the list is the screen reader's scrolling surface
/// ([`SemanticRole::List`]).
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
    };
    list_screen_node(key, label, plan, &screen, children)
}

/// Give `plan` its `room` and the strip under it a page keeps for a status
/// bar ([`Overhang`]). A list that fits whole in the two, with no control
/// reaching past the room — only words and padding in the strip — takes both
/// and does not scroll. Any other keeps the strip for its peek
/// ([`ScrollPlan::screen`]): a control there would be one a bar that comes
/// takes from under the pointer.
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

/// The stops a screen whose shown stops are `view` draws
/// ([`ScrollPlan::screen`]).
pub(super) fn drawn_stops(plan: &ScrollPlan, view: &Range<usize>) -> Range<usize> {
    plan.screen(view.start).drawn
}

/// [`scroll_list_node`] for a screen that draws its PEEK
/// ([`ScrollPlan::screen`]): `children` are the runs of `drawn`, the stops
/// `view` shows and those after them drawn on into the strip under the room
/// — on the last screen the stops before them, scrolled so the last stop
/// ends at the room's edge. The list's room is live where it shows a node,
/// cut or whole; the strip holds a picture that fades ([`UiNode::room`]).
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
    // THE LAST SCREEN KEEPS ITS CAPTION: where the list's scroll would cut
    // into the caption of the group run over its top, that run's card keeps
    // its caption whole at the list's top and scrolls its rows under it, as
    // every screen that starts inside a group repeats its caption.
    if let (Some(caption), Some(first)) = (screen.caption, children.first_mut()) {
        if caption.kept && keep_caption(first, caption.lead, offset) {
            offset = 0.0;
        } else {
            hide_cut_caption(first, caption.lead, offset);
        }
    }
    // A hard edge through the caption of the card drawn last: its cut words
    // paint nothing, and the card goes on to the edge.
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
    // The thumb's track is what the screen draws: the list's height where a
    // peek fills it, else down to its last card, never past it onto the
    // backdrop.
    let filled = screen.fades || screen.drawn != screen.view || screen.offset > 0.0;
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

/// Make `card` — a group run's card, its caption (`caption` tall from its
/// top, padding included) over its rows — keep that caption whole with its
/// rows scrolled `offset` under it, the card that much shorter. `false`, the
/// card as it was, where its rows are not `offset` tall: the list scrolls.
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
    .children(rows)
    .paint_only()
    .peek();
    card.children.push(scrolled);
    true
}

/// Where `card`'s caption — its children within `caption` of its top, padding
/// included — is cut through by an edge of the list `offset` under the card's
/// top — the top edge where its rows are too short to scroll under it whole
/// ([`keep_caption`]), a hard bottom edge ([`Screen::edge_caption`]) — make
/// the cut words paint nothing: the card's backdrop shows there, so no line
/// of words is cut through at either edge.
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

/// Seat a PAGE'S OWN scrolling list in the page's right gutter: each of the
/// page column's direct `children` that is a [`scroll_list_screen`] reaches
/// `gutter` (the page's right inset) past the page's content edge
/// ([`Layout::outdent`]); its list keeps its cards to that edge (the same
/// `gutter` as padding), and its thumb, its node now spanning the gutter, is
/// drawn there in the terminal's resting idiom ([`PAGE_SCROLL_AUDIT`]), as
/// WinUI's `ScrollViewer` sets its bar at the pane's edge beside the content
/// it scrolls. A list inside a card or a panel (Messages' log, the Themes
/// chooser) keeps its thumb down its own edge. The thumb used to run inside
/// the cards' right padding and across the gaps between them (the 2026-10-18
/// judge's `J4F-6-settings-scroller-own-idiom`).
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

/// A card the list lays out row by row: its head (a heading) leads every run
/// of its rows a screen shows, so each screen still reads as that card, and
/// a screen ends between two rows, never inside one.
#[derive(Clone)]
pub(super) struct SplitCard {
    pub(super) key: String,
    pub(super) spec: GroupSpec,
    pub(super) padding: Insets,
    pub(super) gap: f32,
    pub(super) head: Vec<(UiNode, f32)>,
    pub(super) rows: Vec<(UiNode, f32)>,
}

/// Whether `node` is a card's heading: its text, or a paint-only frame that
/// insets it (Settings ▸ Harness's SESSIONS, whose rows' blocks take the
/// card's padding: `native_settings_harness::row_cards_inset`).
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
    /// A column card authored whole — every child a fixed height — as rows,
    /// a leading heading as its head. `None` for anything else.
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

    /// How many of the card's rows that hold a control a screen `room` tall
    /// shows whole when the card starts at its top: its stops
    /// ([`Self::stops`]) in order, as many as fit under its head (one at
    /// least, as a screen always shows one stop).
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

    /// What a run of the card's rows pays besides each row and its gap: the
    /// padding, the head and the gaps around it ([`ScrollPlan::group`]).
    pub(super) fn height_of_head(&self) -> f32 {
        self.height_of(0..0) - if self.head.is_empty() { self.gap } else { 0.0 }
    }

    /// Where the controls of each of `rows` sit, each `(top, height)` from
    /// the top of the first ([`ScrollPlan::set_control_rows`]).
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

    /// How far under the first of `rows` what they draw ends: their words
    /// and controls, not the padding under them ([`ScrollPlan::set_ink`]).
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

    /// How far under the top of a stop of `rows` their first row of controls
    /// ends ([`ScrollPlan::set_first_controls`]).
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

    /// How far under the top of a stop of `rows` what they draw begins
    /// ([`ScrollPlan::set_ink_top`]).
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

    /// Where the card's head (its caption) begins and ends under its top: its
    /// top padding, and the head's lines with the gaps between them. `None`
    /// for a card with no head.
    pub(super) fn caption(&self) -> Option<(f32, f32)> {
        (!self.head.is_empty()).then(|| {
            let head = self.head.iter().map(|(_, height)| height).sum::<f32>()
                + self.head.len().saturating_sub(1) as f32 * self.gap;
            (self.padding.top, self.padding.top + head)
        })
    }

    /// Where a run's first row starts in the card: under its top padding
    /// and its head ([`ScrollPlan::group`]).
    pub(super) fn height_of_lead(&self) -> f32 {
        self.padding.top
            + self.head.iter().map(|(_, height)| height).sum::<f32>()
            + self.head.len() as f32 * self.gap
    }

    /// The card's rows grouped into list stops, so no screen ends on a card's
    /// words without the control they belong to: a row with no control keeps
    /// with the row after it while a control still follows in the card (a
    /// description with the first control, a CLI's state with its switch, an
    /// unavailable slider's words with it); a status line (a
    /// [`SemanticRole::Status`] group) right under a control's row is that
    /// control's own and keeps with it instead, and where the two outgrow a
    /// screen it is the stop right after that row, never the next control's —
    /// each only while the stop still fits `room` under the card's head. A
    /// stop is a control with its own words. Rows with no control after the
    /// card's last control (a caption on the switches above it, About's build
    /// facts) are a stop each, so a screen holds as many as fit: joined to
    /// the switch over them, a hard edge between the two left that switch,
    /// its own note and all, drawn whole on a screen that could not count it,
    /// and Page Down showed it again (Packages at 80x24, its consent note
    /// under the reroute switch). So is A NOTE ON THE ROWS OVER IT
    /// ([`notes_rows_over`]: the note a run of rows shares) wherever it sits,
    /// as a list of settings lists it: it heads no control after it. Joined
    /// to the next row's control (Cursor Styles' trail colours' note to
    /// Bloom), it made a screen whose top edge cut the note uncount the Bloom
    /// switch it drew whole, and Page Up showed Bloom again.
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
        // Whether `row` is a status line right under a control's row: it says
        // how that control stands, so it belongs to it. Every other row of
        // words keeps the forward rule (the AI card's CLI state row heads the
        // switch after it, though the previous CLI's switch is right above).
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
        // A note on rows over it: a control over it in the card says it.
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
    /// The most of a screen `room` tall one of the card's rows may take, the
    /// card's padding and its head over it: what a run of that row alone
    /// leaves it ([`Self::height_of`]).
    fn row_limit(&self, room: f32) -> f32 {
        room - self.height_of_lead() - self.padding.bottom
    }

    /// Whether `row` of the card is listed a part at a time on a screen
    /// `room` tall ([`row_parts`]): it is taller than the screen shows whole
    /// under the card's head ([`Self::row_limit`]) — except A ROW WITH AN
    /// OPEN DISCLOSURE no taller than the screen itself, which stays one with
    /// what it opened: a landing on it lets the card's caption go and draws
    /// it whole ([`ScrollPlan::land_selected`]; Settings ▸ Harness's session
    /// row with its Timer open).
    fn splits(&self, (node, height): &(UiNode, f32), room: f32) -> bool {
        *height > self.row_limit(room) + 0.01
            && splittable(node)
            && !(*height <= room + 0.01 && holds_open_disclosure(node))
    }

    /// Whether the card's head, led over every run of its rows, would take
    /// more of a screen `room` tall than it leaves those rows: a caption
    /// wrapped to four lines and an action under it at 2× text in a
    /// 22-column window (Packages' AUTOMATIC MAINTENANCE, ACTIVITY and Open
    /// Log), which left no row a line's room and no screen a row whole.
    fn head_starves(&self, room: f32) -> bool {
        let head = self.head.iter().map(|(_, height)| height).sum::<f32>()
            + self.head.len().saturating_sub(1) as f32 * self.gap;
        !self.head.is_empty() && !self.rows.is_empty() && self.row_limit(room) < head - 0.01
    }

    /// Whether the card is laid out again for a screen `room` tall
    /// ([`Self::fitted`]).
    pub(super) fn outgrows(&self, room: f32) -> bool {
        self.head_starves(room) || self.rows.iter().any(|row| self.splits(row, room))
    }

    /// The card fitted to a screen `room` tall: a head that would starve its
    /// rows ([`Self::head_starves`]) listed once, as its first rows, rather
    /// than over every run; then each row the screen cannot show whole under
    /// what heads it listed a part at a time ([`Self::splits`],
    /// [`row_parts`]).
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
                    if card.splits(&row, room) {
                        row_parts(row, limit)
                    } else {
                        vec![row]
                    }
                })
                .collect(),
            ..card
        }
    }
}

/// Whether `node` holds a disclosure that is open: a control whose
/// `expanded` state is set.
pub(super) fn holds_open_disclosure(node: &UiNode) -> bool {
    let open = match &node.content {
        UiContent::Button(control) => control.state.expanded == Some(true),
        _ => false,
    };
    open || node.children.iter().any(holds_open_disclosure)
}

/// Whether a row can be listed a part at a time ([`row_parts`]): a group of
/// children set in a column, not a segmented control.
fn splittable(node: &UiNode) -> bool {
    matches!(node.content, UiContent::Group(_))
        && node.layout.flow == Flow::Column
        && node.layout.offset == 0.0
        && !node.segmented
        && !node.children.is_empty()
}

/// The heights the children of a column `height` tall take, as the layout
/// gives them ([`crate::native_ui`]'s column flow): a fixed one its own, a
/// fraction its share of the room the gaps leave, and the room the others
/// leave shared among those that fill it.
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

/// A ROW TALLER THAN ITS SCREEN, LISTED A PART AT A TIME: the list scrolls by
/// whole rows, so a row taller than what a screen leaves it under its card's
/// head ([`SplitCard::row_limit`]) was drawn from its top and its rest was on
/// no screen — at 22 columns and 2× text, Packages' status detail ("This copy
/// of" … and nothing of "Install aterm from alab.systems") and every row of
/// its consent card past a line or two, its switches included. Its children,
/// one under another, are gathered into parts no taller than `limit` (a part
/// a child alone where one is; a child taller than that split the same way
/// first), each a row of the card and so a stop of the list. The first part
/// keeps the row's key and its group (its name, whole, said once); the rest
/// are keyed `<row>/part-<n>`, unlabeled groups in the row's style. A row
/// that fits, or that is not a column, is one row.
pub(super) fn row_parts((node, height): (UiNode, f32), limit: f32) -> Vec<(UiNode, f32)> {
    if height <= limit + 0.01 || !splittable(&node) {
        return vec![(node, height)];
    }
    let heights = column_heights(&node, height);
    let mut template = node;
    let children = std::mem::take(&mut template.children);
    let (gap, padding) = (template.layout.gap, template.layout.padding);
    let inner = (limit - padding.top - padding.bottom).max(1.0);
    // Each child at the height the column gave it, now its own: a part's
    // height is its children's.
    let pieces: Vec<(UiNode, f32)> = children
        .into_iter()
        .zip(heights)
        .flat_map(|(mut child, height)| {
            child.layout.height = Length::Fixed(height);
            row_parts((child, height), inner)
        })
        .collect();
    let mut parts: Vec<Vec<(UiNode, f32)>> = vec![Vec::new()];
    let mut used = 0.0_f32;
    for (piece, height) in pieces {
        let part = parts.last_mut().expect("one part");
        let cost = if part.is_empty() {
            height
        } else {
            gap + height
        };
        if !part.is_empty() && used + cost > inner + 0.01 {
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
            (part, height)
        })
        .collect()
}

/// `items` with every split card's rows fitted to a screen `room` tall
/// ([`SplitCard::fitted`]): borrowed where no row outgrows it.
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

/// One item of a [`list_page`]: a card that is always whole, or one listed
/// row by row.
#[derive(Clone)]
pub(super) enum ListItem {
    Card(Box<UiNode>, f32),
    Split(SplitCard),
}

impl ListItem {
    /// A card node listed row by row where it is a column of fixed rows,
    /// whole where it is not.
    pub(super) fn of(node: UiNode, height: f32) -> Self {
        match SplitCard::from_column(&node) {
            Some(card) => Self::Split(card),
            None => Self::Card(Box::new(node), height),
        }
    }
}

/// Each list stop's item, and its rows where the item is split.
type ItemStops = Vec<(usize, Option<Range<usize>>)>;

/// `items` measured as stops `gap` apart for a list `room` tall: a whole card
/// is one stop, a split card a stop per run of rows ([`SplitCard::stops`]).
fn plan_items(items: &[ListItem], gap: f32, room: f32) -> (ScrollPlan, ItemStops) {
    let mut plan = ScrollPlan::new(gap);
    let mut stops: ItemStops = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            ListItem::Card(node, height) => {
                plan.card(*height, focus_keys(node));
                plan.measure(node, *height);
                plan.note_previews([&**node]);
                plan.add_segments(node);
                stops.push((index, None));
            }
            ListItem::Split(card) if card.rows.is_empty() => {
                plan.card(card.height(), focus_keys(&card.node(0..card.rows.len())));
                plan.note_previews(card.head.iter().map(|(node, _)| node));
                stops.push((index, None));
            }
            ListItem::Split(card) => {
                let group = plan.group(card.height_of_head(), card.height_of_lead());
                plan.set_row_gap(group, card.gap);
                if let Some((top, bottom)) = card.caption() {
                    plan.set_caption(group, top, bottom);
                }
                for rows in card.stops(room) {
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
                    plan.set_control_rows(card.control_rows(rows.clone()));
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

/// The height the tallest stop of `items` takes on screen in a list at most
/// `room` tall: what a page pinning something over the list keeps room for,
/// so every stop can be seen whole.
pub(super) fn tallest_span(items: &[ListItem], gap: f32, room: f32) -> f32 {
    let (plan, _) = plan_items(&fit_rows(items, room), gap, room);
    (0..plan.len())
        .map(|stop| plan.span(stop..stop + 1))
        .fold(0.0, f32::max)
}

/// The height the first `stops` stops of `items` take on screen together, in
/// a list at most `room` tall.
pub(super) fn leading_span(items: &[ListItem], gap: f32, room: f32, stops: usize) -> f32 {
    let (plan, _) = plan_items(&fit_rows(items, room), gap, room);
    plan.span(0..stops.min(plan.len()))
}

/// What the first screen of `items` shows whole, listed in `room` with the
/// `overhang` kept under it for a status bar ([`list_page_peeking`]): `None`
/// where its first item is not whole on it (a card listed row by row counts
/// every row), else how many of `keys` sit in stops it shows whole. Measured
/// on the plan the list draws, so a page choosing between forms of its content
/// asks the question its screen answers.
pub(super) fn first_screen_holds(
    items: &[ListItem],
    gap: f32,
    room: f32,
    overhang: Overhang,
    keys: &[UiKey],
) -> Option<usize> {
    let (mut plan, stops) = plan_items(&fit_rows(items, room), gap, room);
    seat_room(&mut plan, room, overhang);
    // `view` seats a first stop taller than the room too; that one is not
    // shown whole.
    let whole =
        |stop: usize| plan.view(0).contains(&stop) && plan.span(0..stop + 1) <= plan.room() + 0.01;
    let first_item = stops.iter().take_while(|(item, _)| *item == 0).count();
    (first_item > 0 && (0..first_item).all(whole)).then(|| {
        keys.iter()
            .filter(|key| plan.stop_of(key).is_some_and(whole))
            .count()
    })
}

/// `items` as one scrolling list in `room`, `gap` apart, seated for this
/// view: a whole card is one stop, a split card a stop per row.
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

/// [`list_page`] whose screens draw their peek ([`scroll_list_screen`]) in
/// the strip under the list the page keeps for a status bar (`overhang`); a
/// list that fits whole in its room and that strip, nothing to press in the
/// strip, takes both and does not scroll ([`seat_room`]).
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

/// A list's items seated for this render: the plan measured from them, each
/// stop's item, the stops the screen shows and the stops it draws.
///
/// A LIST IS SEATED ONCE PER RENDER: [`SettingsViewState::seat_scroll`]
/// records where it put the list, and the next seating starts from that
/// record — the place a list holds while rows above it come or go, a
/// reveal on its first screen — so a second seating of the same render can
/// choose another top. Every node the render builds for the list (the
/// screen's rows and, for [`list_page_whole`], the rows off it) comes from
/// this one seating, so no row is built twice.
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
        let (mut plan, stops) = plan_items(items, gap, room);
        seat_room(&mut plan, room, overhang);
        let view = state.seat_scroll(&mut plan);
        let drawn = drawn_stops(&plan, &view);
        Self {
            plan,
            stops,
            view,
            drawn,
        }
    }

    /// The nodes of the runs the screen draws.
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

    /// The stops in `range`, off the screen ([`UiNode::offscreen`]) where
    /// scrolling would bring them: a card whole; a split card's run, only the
    /// rows that run holds (a card may run on past the screen's edge, its
    /// other rows on it), under a key of its own (`side`: "above" or
    /// "below") and without its caption, which the screen's run of it
    /// already carries.
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

    /// The list's scroll frame around `children`, the screen's runs.
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

/// [`list_page_peeking`] whose rows off the screen are in the tree too
/// ([`UiNode::offscreen`]): laid out above and below the screen where
/// scrolling brings them, so a screen reader and `controls` see every row
/// the list holds, while only the screen is painted, hit and focusable.
pub(super) fn list_page_whole(
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
    let drawn = seated.drawn.clone();
    let above = seated.offscreen_children(&items, 0..drawn.start, "above");
    let below = seated.offscreen_children(&items, drawn.end..seated.plan.len(), "below");
    // The screen's own rows start where the rows above them end: what those
    // nodes take (a split card's part above carries no caption of its own).
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
        // What of the screen's rows its room does not show — cut away by its
        // edge, or drawn in the strip under it — is in the tree off the
        // screen too ([`crate::native_ui::ListRoom::lists_all`]).
        if let Some(room) = list.room.as_mut() {
            room.lists_all = true;
        }
        let screen = std::mem::take(&mut list.children);
        list.children = above.into_iter().chain(screen).chain(below).collect();
    }
    page
}

/// The node keyed `key` under `node`.
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

    /// A ROW WHOSE SECOND LINE THE STRIP HOLDS IS SHOWN, AND DRAWN WHOLE: a
    /// stop whose controls are whole in the room and whose words under them
    /// end in the strip kept for a status bar is one the screen shows, drawn
    /// live and at full strength down to its words' end (the peek fades only
    /// under them), so Page Down turns past it; with a bar up, the strip is
    /// the bar's and the stop is shown only where it ends in the room. A row
    /// of Modified, whose facts sit on its second line, was painted whole in
    /// the strip and turned onto again. CONTROLS: a stop whose control
    /// reaches into the strip, and one with no control, are shown only in
    /// the room.
    #[test]
    fn a_row_whose_second_line_the_strip_holds_is_shown_and_drawn_whole() {
        let plan = |overhang: f32, note: f32, control: f32| {
            let mut plan = ScrollPlan::new(10.0);
            plan.card(80.0, vec![UiKey::new("first")]);
            // A row: its control from 4 to 4 + control, its note under it.
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
        // The row starts at 90: its control ends at 130, in the room; its
        // 18pt note ends at 148, in the strip.
        let shown = plan(44.0, 18.0, 36.0);
        assert_eq!(shown.view(0), 0..2, "the row is shown");
        let screen = shown.screen(0);
        assert_eq!(screen.live, 148.0, "drawn whole down to its note's end");
        assert_eq!(shown.page_after(0), 2, "Page Down turns past it");
        // With a bar up the strip is the bar's: the note under it is not shown.
        let barred = plan(0.0, 18.0, 36.0);
        assert_eq!(barred.view(0), 0..1);
        assert_eq!(barred.screen(0).live, 140.0);
        // CONTROLS: a control that reaches into the strip, a note that runs
        // past it, and a stop with no control are shown only in the room.
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

    /// PAGE UP ENDS ON THE STOP BEFORE THE SCREEN IT LEAVES: where no screen
    /// seated at a stop ends exactly there — the one that reaches it shows a
    /// stop of the screen being left again, the next one down skips one — it
    /// lands on the screen scrolled so that stop ends at the room's edge, the
    /// stop over it cut at the list's top, as the last screen ends on the
    /// list's last stop; Page Down from there comes back. From the last
    /// screen it turns back from the first stop that screen shows whole, not
    /// from the stop it is seated at. CONTROL: where a seated screen ends
    /// there, Page Up seats at it.
    /// THE FIRST SCREEN'S LEAD is the list's first stop and, after it, the
    /// first ROW of controls it holds: under a card's words its first row of
    /// buttons, not the rows of buttons under that (Security's MACOS ACCESS
    /// block puts Open Privacy & Security… first and wraps the rest under
    /// it), and past a stop with no control to the one after. CONTROL: a list
    /// of one stop needs that stop.
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
        // A card: 12pt padding, 52pt of words, then its actions, two rows of
        // buttons 8pt apart under their own 12pt padding: the first row ends
        // 12 + 52 + 12 + 44 = 120 under the card's top, the second at 172.
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
        // The card starts at 78.
        assert_eq!(plan.first_screen_need(), 78.0 + 120.0);
        // Past a stop with no control, to the next that holds one.
        let mut quiet = ScrollPlan::new(10.0);
        quiet.card(68.0, vec![UiKey::new("lead")]);
        quiet.card(30.0, Vec::new());
        quiet.card(196.0, focus_keys(&card));
        quiet.measure(&card, 196.0);
        quiet.set_room(400.0);
        assert_eq!(quiet.first_screen_need(), 118.0 + 120.0);
        // CONTROL: one stop.
        let mut alone = ScrollPlan::new(10.0);
        alone.card(90.0, Vec::new());
        alone.set_room(400.0);
        assert_eq!(alone.first_screen_need(), 90.0);
    }

    #[test]
    fn page_up_ends_on_the_stop_before_the_screen_it_leaves() {
        // Room 100, 10pt gaps: two short cards, a tall one, short ones.
        let plan = cards(&[30.0, 30.0, 90.0, 30.0, 30.0, 30.0, 30.0], 100.0, 10.0);
        assert_eq!(plan.view(4), 4..6);
        // The least seated screen that reaches 4 is 3's, which shows 4 again
        // (3..5); the one over it, 2's, shows only the tall card (2..3) and
        // skips 3.
        assert_eq!(plan.page_before(4), 3);
        assert_eq!((plan.view(3), plan.view(2)), (3..5, 2..3));
        let (top, nudge) = plan.page_up(4);
        assert_eq!((top, nudge), (2, Some(30.0)), "3 ends at the room's edge");
        let mut landed = plan.clone();
        landed.seated = nudge.map(|nudge| Seat::nudged(top, nudge));
        assert_eq!(landed.shown(top), 3..4, "the tall card cut at the top");
        assert_eq!(landed.page_after(top), 4, "Page Down comes back");
        // From the last screen, which shows whole a stop before the one it is
        // seated at (its top edge cuts only that stop's padding), Page Up
        // turns back from that stop.
        let mut padded = ScrollPlan::new(10.0);
        for _ in 0..6 {
            padded.card(50.0, Vec::new());
            padded.set_ink_top(10.0);
        }
        padded.set_room(105.0);
        let last = padded.limit();
        assert_eq!((last, padded.view(last)), (5, 5..6));
        assert_eq!(padded.shown(last), 4..6, "4 whole under its cut padding");
        assert_eq!(padded.page_up(last), (3, None));
        assert_eq!(padded.shown(3), 3..4);
        // CONTROL: equal stops, a seated screen ends on the stop before.
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
        // A stop that would outgrow the room stays a row.
        assert_eq!(card.stops(60.0), vec![0..1, 1..2, 2..3, 3..4]);

        // A status line right under a control's row (its switch) keeps with
        // it; where the two outgrow a screen it is the stop right after it,
        // never joined to the next control it says nothing about.
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
        // Other words under a control go with the next one, as a CLI's state
        // row goes with its switch.
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

    /// A NOTE ON THE ROWS OVER IT IS A STOP OF ITS OWN: the note a run of
    /// rows shares, its lines status lines, under the run's switches and
    /// over another switch, heads none — joined to the switch after it, a
    /// screen whose top edge cut the note uncounted that switch drawn whole,
    /// and Page Up showed it again (Cursor Styles: the trail colours' note
    /// over Bloom; Harness: the coding agents' note over Autopilot).
    /// CONTROLS: a card's first words, status lines too (Software Update's
    /// status over Install Now), and words that are not status lines (a CLI's
    /// state) still head the control after them.
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
        // CONTROLS: the card's first words, and words that are not a status.
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

    /// A HARD EDGE UNDER A SWITCH AND OVER ITS CARD'S CAPTION: the screen
    /// that draws the switch whole counts it, and Page Down turns to the
    /// caption. Joined to the caption under it, the switch was drawn whole on
    /// a screen that could not count it, and the next screen showed it again
    /// (Packages at 80x24: the reroute switch, its own note and all, over the
    /// consent note).
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
        // The switch ends 160pt down the list, the caption 185pt: the edge at
        // 170pt falls between them.
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

    /// A card of words alone is a stop per row, so a screen holds as many
    /// as fit; an unavailable control still keeps its words with it.
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

    /// The thumb's track is what the screen draws: down to the last card on
    /// a last screen that draws no peek, never past it onto the backdrop.
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

    /// The stops after a screen are drawn down to the last any of which its
    /// edge leaves in view, a card it starts only where [`PEEK_MIN`] of it
    /// shows — never a stray line of one.
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

        // A control peeking through the bottom moves by only its hidden
        // part. Its visible piece remains under the point that pressed it.
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

    /// THE SCREEN ENDS ON A CARD'S BOUNDARY, NEVER ON A CUT OVER BLANK: where
    /// no card after a screen would show [`PEEK_MIN`] (its edge in the gap
    /// between two cards), the screen ends on the last card it shows, whole,
    /// and the next screen starts at the card after it. Cutting that card
    /// PEEK_MIN short of its bottom on a hard edge left a band of blank as
    /// tall as the gap and the cut under it (Packages at 80x24: 36pt).
    /// CONTROL: where the strip kept under the room for a status bar fades,
    /// the same edge draws on to the page's bottom too.
    #[test]
    fn a_screen_whose_edge_falls_between_cards_ends_on_the_card_before() {
        let mut plan = ScrollPlan::new(5.0);
        let group = plan.group(40.0, 20.0);
        plan.row(group, 60.0, Vec::new());
        plan.row(group, 40.0, Vec::new());
        plan.card(50.0, Vec::new());
        plan.set_room(145.0);
        // The rows end at 120 and the card's padding at 140; the next card
        // would start at 145, the list's edge.
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

    /// A CAPTION IS KEPT EXACTLY WHILE ITS ROWS SCROLL UNDER IT, AND A PRESS'S
    /// NUDGE NEVER BRINGS BACK ONE THE SCREEN LET GO. Each of a group's stops
    /// carries its share of the card's row gap, so a run's rows end that gap
    /// above its stops' sum; the plan kept the caption over the last 4pt of
    /// that — where the card (`keep_caption`) lets it go and draws the row
    /// cut at the top edge — so a press on the cut switch there measured it
    /// under a caption that was not drawn, and a nudge that brought the
    /// caption back over it sent it out from under the pointer (Security at
    /// 738x418). CONTROLS: a run with more rows under the caption keeps it,
    /// and a nudge on a screen that kept its caption keeps it.
    #[test]
    fn a_caption_is_kept_while_its_rows_scroll_under_it_and_a_nudge_never_brings_one_back() {
        let mut plan = ScrollPlan::new(10.0);
        let group = plan.group(50.0, 40.0);
        plan.set_row_gap(group, 4.0);
        plan.set_caption(group, 14.0, 36.0);
        plan.row(group, 66.0, vec![UiKey::new("a")]);
        plan.row(group, 66.0, vec![UiKey::new("b")]);
        plan.set_room(200.0);
        // The last row's 62pt under a 64pt scroll: nothing of it scrolls under
        // the caption, so it is let go; 60pt in, 2pt does, and it is kept.
        let tail = plan.top_caption(1..2, 64.0);
        assert_eq!(
            tail, None,
            "64pt into a 62pt run: let go (past its words, so none to hide)"
        );
        // Without the row gap the plan kept it there.
        let mut gapless = plan.clone();
        gapless.set_row_gap(group, 0.0);
        assert_eq!(
            gapless.top_caption(1..2, 64.0).map(|caption| caption.kept),
            Some(true)
        );
        assert_eq!(
            plan.top_caption(1..2, 60.0),
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
        // The screen that let it go, nudged back 20pt: its caption stays let go,
        // so the row the nudge shows whole starts at the list's top.
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
        };
        let nudged = plan.nudged(let_go.clone(), -20.0, false);
        assert_eq!(nudged.offset, 44.0);
        assert!(
            nudged.caption.is_none_or(|caption| !caption.kept),
            "{:?}",
            nudged.caption
        );
        assert_eq!(plan.caption_floor(nudged.caption, &nudged.drawn, 1), 0.0);
        // CONTROL: a screen that kept its caption keeps it, nudged back.
        let kept = Screen {
            offset: 60.0,
            caption: plan.top_caption(1..2, 60.0),
            ..let_go
        };
        let nudged = plan.nudged(kept, -16.0, false);
        assert_eq!(nudged.caption.map(|caption| caption.kept), Some(true));
        assert_eq!(plan.caption_floor(nudged.caption, &nudged.drawn, 1), 40.0);
    }

    /// A HARD EDGE NEVER CUTS THROUGH A CARD'S CAPTION, AND LEAVES NO BLANK
    /// UNDER THE CARD IT CUTS: the card after the screen peeks in on to the
    /// edge, and where the edge would cut through its caption's words those
    /// words paint nothing ([`Screen::edge_caption`]: the edge and the
    /// caption's end, from the card's top). Landing on the caption's top left
    /// a band of blank as tall as the cut part of the caption under the card
    /// (Software Update at 60x20). CONTROLS: an edge past the caption keeps it
    /// whole over the row it heads; where the strip fades, the peek is a
    /// picture drawn on to the page's bottom; a card whose caption the plan
    /// does not know is drawn to the edge.
    #[test]
    fn a_hard_edge_never_cuts_through_a_cards_caption_nor_stops_short() {
        let plan = |caption: bool| {
            let mut plan = ScrollPlan::new(10.0);
            plan.card(100.0, Vec::new());
            let group = plan.group(52.0, 40.0);
            if caption {
                plan.set_caption(group, 12.0, 34.0);
            }
            plan.row(group, 44.0, Vec::new());
            plan.row(group, 44.0, Vec::new());
            plan
        };
        // The card ends at 100; the next one starts at 110, its caption's
        // words at 122..144, its first row at 150.
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
        past.set_room(150.0);
        let screen = past.screen(0);
        assert_eq!(
            (screen.height, screen.edge_caption),
            (150.0, None),
            "past the caption: drawn to the edge, the caption whole"
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

    /// A STOP TALLER THAN THE PAGE IS CUT BY THE LIST'S EDGE: a screen shows
    /// its first stop whatever its height, but the list never draws past its
    /// room and the strip under it (a page a tall question squeezes), so
    /// nothing runs on under the page's edge. CONTROL: a stop that fits draws
    /// whole.
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

    /// The last screen ends on the last stop and is as full as the others:
    /// the stop before it peeks in over it, cut at the list's top, and the
    /// list is scrolled so the last stop ends at its bottom edge.
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

        // At the last screen a top-cut control moves down only enough
        // to become whole; the list cannot move above its first drawn stop.
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

    /// A PAGE'S OWN LIST DRAWS ITS THUMB IN THE PAGE'S GUTTER
    /// ([`seat_page_scroller`]): a page column with a 75 pt right inset,
    /// clipped as a Settings page is, holding a scrolled list of cards.
    /// Seated, the cards keep to the page's content edge, while the thumb's
    /// node spans the gutter, clipped to it, and paints there the terminal's
    /// resting thumb: 2 pt under the Fluent chrome and 3 under the Classic,
    /// 2 pt from the page's right edge as the terminal's is from the
    /// window's, the whole gutter less that clear of every card. A list
    /// inside a card (Messages' log) is left alone. NEGATIVE CONTROL:
    /// unseated, the thumb paints inside the cards' column (the 2026-10-18
    /// judge's `J4F-6-settings-scroller-own-idiom`).
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
        // The thumb: the one narrow panel the page paints.
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
            // The Fluent resting tone on every chrome.
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
        // A list inside a card keeps its thumb.
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
}
