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
//! ([`ScrollPlan::view`]) — the list's last stop as every other, with the
//! list's end. ONE BOUNDARY: the screen draws what it shows whole, live and
//! at full strength, down to the first control after those stops that the
//! room does not hold whole; from there it draws its peek, a picture fading
//! out to the list's bottom, never live ([`ScrollPlan::screen`]): a row whose
//! switch ran a few points past the room's edge was painted at full strength,
//! live above the edge only, and Page Down turned onto it again. The scroll's
//! bound, Page Up and Down, the keyboard's reveal and what the screen draws
//! all read that one measure, so a stop a screen shows is never shown again
//! by the next, and none it draws whole goes uncounted. Page Down turns to
//! the stop after the last the screen shows ([`ScrollPlan::page_after`]);
//! Page Up to the screen that ends on the stop before its first, seated at a
//! stop where one does and else scrolled so that stop ends at the room's
//! edge, as the last screen is ([`ScrollPlan::page_up`]). The bound is the
//! least first stop whose screen, as it draws, shows it and every stop after
//! it ([`ScrollPlan::limit`]), so no turn onto the last screen only moves the
//! list. Only a turn that reaches an end of the list — its full last screen,
//! its first — shows again what the screen before showed, as a platform
//! list's last page does. The wheel, the arrows and a reader's scroll move a
//! stop at a time, passing over a note on the rows over it that its row
//! shows with it ([`ScrollPlan::passes_over`]).
//!
//! THE EDGE RULE ([`UiNode::room`]): a control the list's top edge cuts is
//! live where it shows — drawn with its name, else not drawn at all, as is a
//! sliver of one ([`crate::native_ui::LIST_PEEK_MIN`]) — and a press on one
//! scrolls it whole by its hidden part and no more, so it stays under the
//! pointer that pressed it, as a platform list scrolls a clicked row into
//! view: it comes down to start where the room does
//! ([`ScrollPlan::press_seat_span`]). A control past the stops the screen
//! shows is its peek: a press on its picture scrolls it whole into the room,
//! ending at the room's edge, and works nothing
//! ([`SettingsViewState::press_list_picture`]). The list holds the place the
//! nudge gave it, as a platform list holds where a click scrolled it, until
//! something scrolls it ([`SettingsViewState::drop_list_nudge`]): the next
//! press, or the keyboard, on a control it shows whole moves nothing. The
//! strip under the room, which the page keeps for a status bar that is not
//! up, holds no live control: the words under the controls of the last stop
//! the screen shows, drawn whole, and under them only the peek, drawn on to
//! the page's bottom so no screen ends on a band of blank — so the bar that
//! comes takes nothing a person can press and no row moves for it (a bar
//! over a shown row's facts covers those words, and the keyboard on that
//! row keeps its place). On the last screen the stop before the screen's
//! first peeks in over the top, cut at the list's top edge, and the list's
//! end sits as low as the page lets it — on into the strip while no bar takes
//! it, at the room's edge while one does ([`ScrollPlan::last_screen`]) — so
//! that screen is as full as every other; a bar that comes holds the place
//! the last screen drew, and a scroll to the end with the bar up shows the
//! last card whole ([`SettingsViewState::seat_scroll`]).
//!
//! A HARD EDGE NEVER CUTS THROUGH A CAPTION, AND NO SCREEN ENDS ON BLANK
//! UNDER A CARD IT CUTS: the screen draws on to the page's bottom, its peek
//! fading from where the stops it shows end — in the strip, and in the room
//! where no strip is kept (a bar is up, the page keeps none); where nothing
//! fades at the edge, a caption the edge would cut through paints none of
//! its words, the card's border, padding and backdrop going on to the edge
//! ([`ScrollPlan::screen`]). Where no card after the screen shows
//! [`PEEK_MIN`] of itself, a screen that is not the list's first scrolls on
//! into its first card's padding until the next card peeks in
//! ([`ScrollPlan::peek_seat`]); the list's first screen, and one whose first
//! stop has too little padding over it, ends on its last card's boundary,
//! the next screen starting at the card after it.
//!
//! A LIST IN A PANEL SNAPPED TO ITS ROWS ENDS ON WHOLE STOPS
//! ([`ListFoot::Whole`]: Settings ▸ Themes' chooser, beside the demo or
//! under it, and the Customize sheet in its place): its screens draw only the stops they show,
//! each seated at a stop — the last screen at the least first stop that
//! shows the list's end — with no peek under the room and none cut over its
//! top, the rest of the room blank where the next stop does not fit whole;
//! no press, reveal or Page Up scrolls it past a stop's seat. A section's
//! caption stays with its first row there: the two fit whole or neither is
//! drawn.
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
/// ([`ScrollPlan::screen`]) — and the least of a control an edge leaves in
/// view (the compiler's one rule for both, [`crate::native_ui::LIST_PEEK_MIN`]).
pub(super) const PEEK_MIN: f32 = crate::native_ui::LIST_PEEK_MIN;

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

/// How a screen that does not show the whole list ends at the list's bottom
/// edge ([`ScrollPlan::screen`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ListFoot {
    /// On a cut: the stops after the screen drawn on to the page's bottom
    /// (its peek), and the last screen scrolled so the list's last stop ends
    /// at the room's edge, the stop before it cut at the list's top — every
    /// Settings page's list.
    #[default]
    Peek,
    /// On the last stop it shows: a screen draws only the stops whole in its
    /// room, seated at a stop, the rest of the room blank — the last screen
    /// too — so no stop is drawn cut at either edge (a list in a panel whose
    /// height is snapped to its rows: Settings ▸ Themes' chooser beside the
    /// demo or under it, where a stop taller than a row left its foot cut
    /// mid-row, and the last screen a row cut under a caption). Only
    /// a first stop taller than the room is cut, as it is on every list.
    Whole,
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
    /// Whether each stop is A NOTE ON THE ROWS OVER IT ([`notes_rows_over`],
    /// a run's shared note): words that say how the rows above stand, and
    /// so no seat for a scroll by stops where they and their row fit one
    /// screen ([`Self::passes_over`]).
    notes: Vec<bool>,
    /// The gap each group's card sets between its rows, which each of its
    /// stops carries as its share ([`Self::set_row_gap`]): a run's rows end
    /// that much above the sum of its stops.
    row_gaps: Vec<f32>,
    keys: Vec<Vec<UiKey>>,
    /// What each stop IS: the keys of the nodes it lists — a whole card's, a
    /// split card's rows' — by which the list keeps its place when it is
    /// measured into other stops ([`Self::stop_of_identity`]).
    ids: Vec<Vec<UiKey>>,
    /// What else each stop draws, by which it is known after what it lists
    /// and its controls ([`Self::set_within`]): the heading a group's first
    /// stop draws over it, then the nodes inside what it lists.
    within: Vec<Vec<UiKey>>,
    /// The live previews each stop draws (their node keys): what a page's
    /// preview cadence asks whether a screen draws ([`Self::stop_of_preview`]).
    previews: Vec<Vec<UiKey>>,
    /// Each stop's rows that hold a control: where the row starts in the
    /// stop, and where its controls start and how tall they are.
    control_rows: Vec<Vec<(f32, f32, f32)>>,
    /// Each control a stop holds, by key: where it starts in the stop and
    /// how tall it is ([`Self::span_of`]): what a press brings into view.
    spans: Vec<Vec<(UiKey, f32, f32)>>,
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
    /// The strip the page keeps for a bar whether or not one is up: what a
    /// screen's own seat is measured to ([`Self::peek_seat`]), so a bar that
    /// comes moves no row of it.
    kept: f32,
    /// What the screens give up to a plain status whose bar is taller than
    /// the room the page kept for one ([`Self::give_to_status`]): a screen
    /// lets its caption go as it would with that room, so the status never
    /// moves the stops it shows ([`Self::starts_bare`]).
    given: f32,
    /// Whether the screen whose first stop is the first of these lets its
    /// run's caption go, as the last screen at that stop did
    /// ([`Self::starts_bare`], [`SettingsViewState::seat_scroll`]): held
    /// until the list scrolls.
    held_start: Option<(usize, bool)>,
    gap: f32,
    /// The screen the render seated ([`SettingsViewState::seat_scroll`]) and
    /// how far past its stops' seat it is scrolled, where a press found a
    /// control an edge of the room cuts: that control's hidden part
    /// ([`Self::press_seat_span`]). Every [`Self::screen`] of that first stop
    /// draws so scrolled — the render's, and the record's that the reducer
    /// reads. It is part of the list's place: the next render keeps it until
    /// something scrolls the list ([`SettingsViewState::drop_list_nudge`]) or
    /// a reveal needs what the nudged screen does not show whole.
    seated: Option<Seat>,
    /// Each segment of a segmented control the list holds, and the key the
    /// control is listed by ([`Self::add_segments`]): a segment is that one
    /// control to the list's keyboard, never a control of its own.
    segments: Vec<(UiKey, UiKey)>,
    /// How a screen ends at the list's bottom edge: on a cut, or on whole
    /// stops ([`ListFoot`]). A list that ends on whole stops is never
    /// scrolled past a stop's seat: no press's nudge
    /// ([`Self::press_seat_span`]), no seat past a caption or into a stop
    /// ([`Self::land_selected`], [`Self::seat_span`]), no Page Up anchored
    /// past one ([`Self::page_up`]).
    foot: ListFoot,
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
    /// ([`ScrollPlan::press_seat_span`]).
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
    /// How far from the list's top what it draws is live and at full
    /// strength — its room, a first stop taller than that, the words of its
    /// last shown stop under the room — never past the first control after
    /// the stops it shows that the room does not hold whole, where its peek
    /// starts ([`ScrollPlan::live_on`]).
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
    /// Where a hard edge — no fade under the room: no strip, and no control
    /// past the stops the screen shows for its peek to fade from — cuts
    /// through the caption of the card the screen draws last, and where that
    /// caption ends, both from the card's top: the words it cuts paint
    /// nothing, and the card's border, padding and backdrop fill on to the
    /// edge ([`ScrollPlan::edge_caption`]), so the screen never ends on a
    /// caption cut through its words nor on a band of blank under the card
    /// it cuts.
    pub(super) edge_caption: Option<(f32, f32)>,
    /// Whether the screen lets go of the caption of the run it starts in,
    /// scrolled off whole by its lead, so its first stop is drawn whole
    /// from the room's top ([`ScrollPlan::starts_bare`], [`Seat::bare`]): a
    /// nudge never brings it back.
    pub(super) bare: bool,
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

    /// How the list's screens end at its bottom edge ([`ListFoot`]).
    pub(super) fn set_foot(&mut self, foot: ListFoot) {
        self.foot = foot;
    }

    /// Say that the list's room is `given` shorter than the page kept it,
    /// for a plain status's taller bar ([`Self::given`]).
    pub(super) fn give_to_status(&mut self, given: f32) {
        self.given = given.max(0.0);
    }

    /// Whether the list's screens give room up to a plain status's taller
    /// bar ([`Self::give_to_status`]).
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

    /// The room kept under the list for a status bar that is not up
    /// ([`seat_room`]).
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

    /// Whether stop `index` draws anything: words, a control, a picture.
    #[cfg(test)]
    pub(super) fn paints(&self, index: usize) -> bool {
        self.ink[index] > self.ink_top[index] + 0.01
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

    /// How far under its last row's stop a run of `group`'s rows ends: the
    /// card's padding there, less the share of the card's gap that stop
    /// carries and the card does not draw under its last row. Less than
    /// nothing where the gap is more than the padding (the Customize sheet's
    /// sections: no padding, 4pt gaps), so the run ends inside that stop, as
    /// its card draws it ([`Self::run_height`]); raised to nothing, it put
    /// what came after the run that much below where the list drew it.
    fn closing(&self, group: usize) -> f32 {
        self.heads[group] - self.leads[group]
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

    /// Say that the last stop added is a note on the rows over it
    /// ([`Self::notes`]).
    pub(super) fn set_note(&mut self, note: bool) {
        if let Some(last) = self.notes.last_mut() {
            *last = note;
        }
    }

    /// Whether stop `index` is a row whose run goes on with a note on the
    /// rows over it ([`Self::notes`]) that a screen seated at the row shows
    /// with it — the two fit one screen ([`Self::passes_over`]).
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

    /// Whether stop `index` is no seat for a scroll by stops — the wheel,
    /// the arrows, a reader's scroll: a note its row shows with it
    /// ([`Self::paired`]). A scroll a stop at a time from that row would take
    /// the row off the screen and leave its note alone at the top, under its
    /// card's repeated caption, cut off from what it explains (Settings ▸
    /// Harness at 102x29: the Gemini row's note, "Off, aterm leaves that
    /// agent's sessions alone…", alone under SUPERVISION); it scrolls on past
    /// the note instead. Page Down reads on from the row onto the note, as a
    /// page turn does, and a note that cannot share a screen with its row
    /// stands alone where it must.
    pub(super) fn passes_over(&self, index: usize) -> bool {
        index > 0 && index < self.len() && self.paired(index - 1)
    }

    /// Say where each control the last stop added holds sits in it: its
    /// key, where it starts and how tall it is.
    pub(super) fn set_control_spans(&mut self, spans: Vec<(UiKey, f32, f32)>) {
        if let Some(last) = self.spans.last_mut() {
            *last = spans;
        }
    }

    /// The part of stop `index` a `reveal` of control `key` brings into
    /// view ([`Self::revealed_span`]) — but where a plain status's taller
    /// bar shortens the room ([`Self::gives_to_status`]), for a press (or
    /// the keyboard a page turn held), that control itself where the stop
    /// knows where it sits: the switch pressed stays whole where it was,
    /// the Reset its change brings under it may stay cut under the bar,
    /// where showing that Reset too lifted the switch 39pt at 22x24.
    pub(super) fn span_of(&self, index: usize, key: Option<&UiKey>, reveal: Reveal) -> (f32, f32) {
        let pressed = matches!(reveal, Reveal::Pressed | Reveal::Hold) && self.gives_to_status();
        key.filter(|_| pressed)
            .and_then(|key| self.control_span(index, key))
            .unwrap_or_else(|| self.revealed_span(index, reveal))
    }

    /// Where control `key` sits in stop `index`, from and to under the
    /// stop's top, where the stop knows ([`Self::set_control_spans`]).
    fn control_span(&self, index: usize, key: &UiKey) -> Option<(f32, f32)> {
        let control = self.resolve(key);
        self.spans
            .get(index)?
            .iter()
            .find(|(span, _, _)| span == key || span == control)
            .map(|(_, top, height)| (*top, top + height))
    }

    /// Measure the last stop added from `node`, laid out `height` tall: where
    /// its controls sit and where what it holds ends, its own card's padding
    /// under that aside.
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

    /// How far under stop `index`'s top what it draws ends where it is the
    /// list's last stop: its words, and its card's closing padding under it
    /// — THE LIST'S END ([`Self::shows_end_at`]).
    fn end_under(&self, index: usize) -> f32 {
        (self.heights[index] + self.groups[index].map_or(0.0, |group| self.closing(group)))
            .max(self.ink[index])
    }

    /// THE LIST'S LAST STOP IS SHOWN AS EVERY STOP IS, AND THE LIST'S END
    /// WITH IT: drawn from `at` under the list's top, its controls whole in
    /// the room and all its card draws down to its bottom border — its words
    /// and its closing padding, the end of the list — on the screen: in the
    /// room, or in the strip under it while that strip is the list's (no bar
    /// takes it, [`Self::overhang`]), as the words under any stop's controls
    /// may end there ([`Self::shows_at`]). Measured against the room alone,
    /// a screen that drew the last stop whole, its note in the strip, did
    /// not count it, and the next Page Down only scrolled the list a few
    /// points to put that note in the room (Cursor Styles at 150x48 with
    /// Beam saved: 4pt, nothing new); and with a bar up, the last card's
    /// padding the page kept for that bar was cut by the room's edge on every
    /// screen, End too (Terminal at 102x29, Modified at 80x24).
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

    /// The stops a screen seated at its first stop `top` SHOWS: every one
    /// from there that is shown where it is ([`Self::shows_at`]; the list's
    /// last stop with the list's end, [`Self::shows_end_at`]), and the first
    /// whatever its height.
    pub(super) fn view(&self, top: usize) -> Range<usize> {
        let n = self.len();
        if n == 0 {
            return 0..0;
        }
        let top = top.min(n - 1);
        let mut end = top + 1;
        // On the screen as it seats: past its run's caption where it lets
        // that go ([`Self::starts_bare`]).
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

    /// Whether stop `index`, drawn from `at` under the list's top on a screen
    /// whose room ends `room` down, is SHOWN there (THE ONE MEASURE): its
    /// controls whole in the room, and what it draws ending on the screen —
    /// in the room, or the words under its controls in the strip under the
    /// room while that strip is the list's ([`Self::overhang`]: no bar takes
    /// it). A stop with no control is shown where all of it is in the room.
    /// The list's last stop is measured with the list's end where the bound
    /// and the seat are concerned ([`Self::shows_end_at`]).
    fn shows_at(&self, at: f32, index: usize, room: f32) -> bool {
        at + self.controls_of(index).1 <= room + 0.01
            && at + self.ink[index] <= room + self.overhang + 0.01
    }

    /// The last first stop the list scrolls to: the least whose screen, AS
    /// IT DRAWS — the last screen ([`Self::last_screen`]), its end where the
    /// page lets the list's end sit — shows it and every stop after it, so
    /// the last screen is full and no screen before it already draws whole
    /// everything after it: no turn onto the last screen shows only what the
    /// screen before it drew whole. Measured on the stops' seat, the bound
    /// stood a stop past the screen that drew the last stop whole, and the
    /// turn onto it only moved the list (Cursor Styles at 150x48 with Beam
    /// saved: the Adjust card 4pt up, its list 32pt shorter). A list that
    /// ends on whole stops is seated there ([`ListFoot::Whole`]).
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

    /// Whether the screen whose first stop is `top`, drawn as the list's
    /// last screen, shows `top` and every stop after it ([`Self::limit`]),
    /// the caption of the run it starts in kept as that stop's own seat
    /// keeps it ([`Self::starts_bare`]): no screen lets a caption go only to
    /// reach the list's end a stop sooner — two rows under their caption at
    /// the end of a list that one screen holds whole only with the caption
    /// scrolled off were drawn on no screen under it.
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

    /// THE LAST SCREEN whose first stop is `top`: from the stops before it,
    /// the least that fill it, scrolled so the list's end sits as low as the
    /// page lets it — on into the strip under the room while no bar takes
    /// that strip, at the room's edge while one does — and never so low that
    /// the last stop's controls leave the room ([`Self::shows_end_at`]).
    /// The stop before the screen's first peeks in over the top, cut at the
    /// list's top edge. Its end is where the list's end is with the strip
    /// as it is now: a bar that comes over it holds the place it drew
    /// ([`SettingsViewState::seat_scroll`]), so no row moves for the bar,
    /// and a scroll to the end with the bar up shows the last card whole in
    /// the room the bar leaves.
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
        // The last screen that starts with a stop lets its run's caption go
        // where keeping it would cut that stop: one the caption leaves no
        // room whole ([`Self::starts_bare`]), or one the scroll to the
        // list's end (its last card's closing padding) would carry under
        // the caption kept over it — Window's Build badge row at 220x12 at
        // 2× text, the last stop, was cut 8pt under BUILD BADGE on the only
        // screen that drew it. Its caption scrolls with it then, whole or
        // its cut words not drawn.
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
            (lead + self.controls_of(0).1).max(lead + self.end_under(0) - self.overhang)
        } else {
            (lead + self.controls_of(0).1).max(lead + self.ink[0] - self.overhang)
        };
        let second = (1..n)
            .find_map(|stop| self.first_controls[stop].map(|end| self.stop_top(0, stop) + end))
            .unwrap_or(0.0);
        first.max(second)
    }

    /// How many of the list's stops the screen that starts with each cuts —
    /// its controls not whole in the room, or its words running on past the
    /// room and the strip under it: a screen that shows less than one whole
    /// stop ([`super::PageChrome`] gives the list a status bar's room where
    /// that cuts fewer).
    pub(super) fn cut_stops(&self) -> usize {
        (0..self.len()).filter(|&stop| self.cuts(stop)).count()
    }

    /// How many of the list's groups the screen that starts with their first
    /// stop draws without their caption, let go so that stop is whole
    /// ([`Self::starts_bare`]): a name no screen may show, as the screen
    /// before scrolls it into the strip under its room or never reaches it
    /// ([`super::PageChrome`] gives the list a status bar's room where that
    /// lets go of fewer, its cut stops weighed first).
    pub(super) fn bare_captions(&self) -> usize {
        (0..self.len())
            .filter(|&stop| self.starts_run(stop) && self.starts_bare(stop))
            .count()
    }

    /// Whether the screen that starts with stop `index` cuts it
    /// ([`Self::cut_stops`]): as that screen draws, past its run's caption
    /// where it lets that go ([`Self::starts_bare`]) — counted under the
    /// caption, Cursor Styles' trail sliders at 80x16 at 2× text (114pt
    /// rows under a 50pt caption in a 156pt list, each whole on its bare
    /// screen) read as cut, and kept the page from listing its preview,
    /// which left its 92pt style tiles an 88pt list. THE LIST'S LAST STOP is
    /// measured as the last screen draws it ([`Self::last_screen`]): its
    /// controls in the room and its card down to the list's end on the
    /// screen, its top cut where those are taller than that — Manual's row
    /// for a key only aterm.toml holds, 74pt in the 68pt list of 220x12 at
    /// 2× text, was whole on no screen.
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

    /// [`Self::reveal_span`] for a landing that SELECTS stop `index`
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
        if self.heights[index] > self.room + 0.01 || self.foot == ListFoot::Whole {
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
    /// from [`Self::reveal`] or [`Self::reveal_span`] — still cuts the
    /// part of stop `index` the reveal must show ([`Self::revealed_span`]),
    /// though that part is no taller than the room, the list scrolls on into
    /// the stop: by the least nudge that shows the part whole under the
    /// caption the screen then keeps over the run it starts in, as a press's
    /// nudge scrolls ([`Self::press_seat_span`]); else past that caption
    /// ([`Self::bare_seat`]), as a landing that selects a stop seats it
    /// ([`Self::land_selected`]). A stop taller than its caption leaves room
    /// for — the `Every session` row with its Timer open at 2× text on the
    /// lab's narrow page, 486 pt under a 43 pt caption in a 451 pt room —
    /// otherwise never showed its last line by any route: the keyboard's
    /// focus on `at 18:00` drew nothing and scrolled nothing, and the next
    /// stop down was the next screen. `None` where the stops' own seat shows
    /// it whole, or no seat does.
    pub(super) fn seat_span(&self, index: usize, top: usize, span: (f32, f32)) -> Option<Seat> {
        if span.1 - span.0 > self.room + 0.01 || self.foot == ListFoot::Whole {
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
    /// `index` from `from` to `to` under its top whole in its room: a press,
    /// a change or a landing on a control the screen already shows whole
    /// moves nothing, its words and padding cut or not; one an edge cuts
    /// scrolls the least that shows it whole.
    pub(super) fn reveal_span(&self, index: usize, top: usize, (from, to): (f32, f32)) -> usize {
        let limit = self.limit();
        let top = top.min(limit);
        if self.shows_span(index, top, (from, to)) {
            return top;
        }
        // A stop the screen starts with stays its first: no later screen
        // shows it at all ([`Self::seat_span`] scrolls on into it).
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

    /// Whether the screen whose first stop is `top`, as it draws
    /// ([`Self::screen`], nudged where the render seated it so), shows the
    /// part of stop `index` from `from` to `to` under its top whole in its
    /// room: under the caption the screen keeps whole over the group run it
    /// starts in, and above the room's edge.
    pub(super) fn shows_span(&self, index: usize, top: usize, span: (f32, f32)) -> bool {
        self.shows_span_on(&self.screen(top.min(self.limit())), index, span)
    }

    /// [`Self::shows_span`] on `screen` as it draws.
    fn shows_span_on(&self, screen: &Screen, index: usize, (from, to): (f32, f32)) -> bool {
        self.live_band(screen, index)
            .is_some_and(|(at, floor, live)| at + from >= floor - 0.01 && at + to <= live + 0.01)
    }

    /// How much of the part of stop `index` from `from` to `to` under its
    /// top the screen whose first stop is `top`, as it draws, shows in its
    /// room: what of it is under the caption the screen keeps and above the
    /// room's edge ([`Self::shows_span`]), none where it draws no part of
    /// the stop.
    fn shown_of_span(&self, index: usize, top: usize, (from, to): (f32, f32)) -> f32 {
        self.live_band(&self.screen(top.min(self.limit())), index)
            .map_or(0.0, |(at, floor, live)| {
                ((at + to).min(live) - (at + from).max(floor)).max(0.0)
            })
    }

    /// Where stop `index` starts on `screen` as it draws, under the list's
    /// top, and the band the screen draws live: from under the caption it
    /// keeps over the stop's run to the room's edge. `None` where the screen
    /// draws no part of the stop.
    fn live_band(&self, screen: &Screen, index: usize) -> Option<(f32, f32, f32)> {
        if !screen.drawn.contains(&index) {
            return None;
        }
        let at = self.stop_top(screen.drawn.start, index) - screen.offset;
        let floor = self.caption_floor(screen.caption, &screen.drawn, index);
        // A first stop taller than the room reads as live past it, but a
        // plain status's bar that took room from the list stands over what
        // is past it ([`Self::gives_to_status`]): the room's edge cuts there,
        // as a press's nudge measures it ([`Self::press_seat_span`]).
        let live = if self.gives_to_status() {
            screen.live.min(self.room)
        } else {
            screen.live
        };
        Some((at, floor, live))
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
    /// `index` as the keyboard's landing on it reveals it — its controls and
    /// the words under them whole, its padding cut or not
    /// ([`Reveal::Keep`]): where a page turn puts the keyboard, so the
    /// render after the turn draws the turn's screen
    /// ([`SettingsViewState::focus_list_screen`]).
    pub(super) fn shows_landing(&self, index: usize, top: usize) -> bool {
        self.shows_revealed(index, top, Reveal::Keep)
    }

    /// Whether the screen whose first stop is `top`, as it draws, shows stop
    /// `index`'s controls whole in its room ([`Self::reveal_span`]).
    #[cfg(test)]
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
            Reveal::Pressed | Reveal::Land | Reveal::Hold => self.controls_of(index),
            Reveal::LandWhole => (0.0, self.heights[index]),
        }
    }

    /// Whether the screen whose first stop is `top`, as it draws, draws some
    /// of stop `index`'s controls live: inside its room, under the caption
    /// it keeps, cut by an edge or whole.
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
    /// for that one control, where the stop says where it sits): a stacked
    /// row's Reset on its name line is live over the room's edge where the
    /// switch under it is not, and Page Up that held the keyboard on the
    /// stop's last control left it on that switch, which the screen did not
    /// draw (Modified at 22x24).
    pub(super) fn draws_control_live(&self, index: usize, key: &UiKey, top: usize) -> bool {
        let span = self
            .control_span(index, key)
            .map(|(from, to)| (from, to - from));
        match span {
            Some(span) => self.draws_live(index, top, std::iter::once(span)),
            None => self.draws_controls_live(index, top),
        }
    }

    /// The control of stop `index` the keyboard can be held on where the
    /// screen whose first stop is `top` draws it live
    /// ([`Self::draws_control_live`]): `key`, the control the stop is listed
    /// by, where it is drawn live, else the first other control the stop
    /// holds that is. A segmented control wrapped onto lines is listed by
    /// its selected segment, which may sit on a line under the room's edge
    /// while the line over it is drawn live: Keyboard & Input's Predictive
    /// echo at 22x24 at 2× text, whose Page Down found no control it could
    /// hold the keyboard on and left it on Option as Meta, a screen back.
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

    /// Whether the screen whose first stop is `top`, as it draws, draws some
    /// of the parts of stop `index` at `spans` (each `(from, height)` under
    /// the stop's top) live: inside its room, under the caption it keeps,
    /// cut by an edge or whole.
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
        // One the top edge cuts may be drawn without its name, and so not
        // at all ([`crate::native_ui::LIST_PEEK_MIN`]'s rule): never counted.
        spans.any(|(from, height)| {
            at + from < live - 0.5 && at + from >= floor - 0.5 && at + from + height > floor + 0.5
        })
    }

    /// What the screen whose first stop is `top` draws (THE ONE MEASURE): the
    /// stops it shows ([`Self::view`]); where more follow, the peek after
    /// them drawn on to the page's bottom — the room and the strip under it
    /// kept for a status bar that is not up — down to the last stop any of
    /// which shows; on the last screen, the stops before them, scrolled so the
    /// last stop's content ends at the room's edge. No screen ends on a band
    /// of blank. Where the render seated this screen nudged
    /// ([`Self::press_seat_span`]), it draws that much further down the list.
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
                bare: false,
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
                bare: false,
            };
        }
        // A LIST THAT ENDS ON WHOLE STOPS ([`ListFoot::Whole`]) draws the
        // stops it shows and nothing after them, seated at its first — the
        // last screen too, from the least first stop that shows the list's
        // last ([`Self::limit`]) — the rest of its room blank: no peek under
        // the room, and none cut over its top.
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
        // THE SCREEN ENDS ON A FADE: it draws the stops after its own down
        // to the last any of which shows — a card after a gap it starts
        // only where [`PEEK_MIN`] of it does, a few points reading as a
        // stray line. Rows that touch (a list box's) go on as a group's
        // rows do: any of one that shows is drawn. What it draws past the
        // stops it shows is its peek, a picture fading out to the page's
        // bottom ([`Self::live_on`]) — in the strip kept for a status bar,
        // and in the room where no strip is (a bar is up, the page keeps
        // none) — so no edge cuts a control live, nor a line of words, and
        // the screen never ends on blank under a card it cuts. Where no card
        // after the screen shows [`PEEK_MIN`] of itself, the screen ends on
        // its last card's boundary, the next screen starting at the card
        // after it.
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
        // A WHOLE STOP BEATS ITS CAPTION ([`Self::starts_bare`]): the
        // screen scrolls the run's caption off by its lead.
        let drop = self.seat_drop(screen.view.start);
        let screen = if drop > 0.0 {
            self.nudged(screen, drop, true)
        } else {
            screen
        };
        self.peek_seat(screen)
    }

    /// What the screen whose first stop is `top` lacks of the next card's
    /// least peek, and the padding over its first stop that a peek seat may
    /// scroll into ([`Self::peek_seat`]); `None` where no next card follows
    /// it after a gap.
    #[cfg(test)]
    pub(super) fn peek_lack(&self, top: usize) -> Option<(f32, f32)> {
        self.peek_need(&self.screen(top.min(self.limit())))
    }

    /// What `screen` lacks of [`PEEK_MIN`] of the card after the stops it
    /// draws, measured to the foot of the strip the page keeps for a status
    /// bar, up or not, so a bar that comes moves no row of it; and the
    /// padding over what its top draws first — the top padding of the card
    /// it heads, over that card's caption, else its first stop's own — that
    /// a peek seat may scroll into. `None` where no card follows it after a
    /// gap.
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

    /// THE PEEK SEAT: `screen`, a screen that is not the list's first, ending
    /// on a whole card with less than [`PEEK_MIN`] of the card after it in
    /// view — the edge falling in the gap between them, or just past the next
    /// card's top — scrolled on by what that peek lacks, where its first
    /// stop's top padding holds that much ([`Self::peek_need`]): the next
    /// card then shows its border, corners and some padding under the edge,
    /// and the screen shows the stops it did (its first stop's words and
    /// controls stay whole, only the padding over them cut). Moving the edge
    /// alone could only end the screen on a whole card over a band of blank
    /// (Window at 150x48: 24pt under SESSION, BUILD BADGE next; Appearance at
    /// 167x36: 26pt under TEXT & CONTRAST, its next card 2pt short of the
    /// least) or cut the card over a band of blank (Packages at 80x24,
    /// measured worse). The list's first screen starts at the page's top, and
    /// a first stop with too little padding over it (a row of buttons flush
    /// with its card's top) keeps its seat, as does a screen already scrolled
    /// past its first stop's seat (one that let its caption go for a stop
    /// that barely fits): those screens end on their card's boundary.
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

    /// A WHOLE STOP BEATS ITS CAPTION: whether the screen whose first stop is
    /// `top` lets go of the caption of the run it starts in — the group's
    /// caption, repeated over a screen that starts inside its card, or over
    /// its first row — scrolled off whole by its lead, so the stop starts at
    /// the room's top. Where the caption and the stop cannot both be whole
    /// in the room (a 2× text row's 64pt control under a 50pt caption in a
    /// 100pt room, 102x12: Cursor Styles cut 21 of its 37 stops on every
    /// screen) but the stop alone is (THE ONE MEASURE, [`Self::shows_at`]).
    /// A list that ends on whole stops keeps a section's caption with its
    /// first row ([`ListFoot::Whole`]).
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
        // Measured in the room the page kept: a status's taller bar cuts
        // the screen's foot, never moves its top.
        let room = self.room + self.given;
        self.foot == ListFoot::Peek
            && lead > 0.01
            && !self.shows_at(lead, top, room)
            && self.shows_at(0.0, top, room)
    }

    /// How far a screen whose first stop is `top` is scrolled past that
    /// stop's seat to let its run's caption go ([`Self::starts_bare`]): the
    /// run's lead, or nothing.
    fn seat_drop(&self, top: usize) -> f32 {
        if self.starts_bare(top) {
            self.lead_of(top)
        } else {
            0.0
        }
    }

    /// The stops a screen drawing `drawn`, scrolled `offset` past its first
    /// stop's seat with `caption` at its top, SHOWS (THE ONE MEASURE, on the
    /// screen as it draws): the run of them that each start under the room's
    /// top — under a caption kept there over their run — and are shown where
    /// they are ([`Self::shows_at`]). Empty where it shows none whole. THE
    /// LAST STOP IS SHOWN BY WHAT IT DRAWS, as every stop is — its controls in
    /// the room and its words on the screen, its card's closing padding cut
    /// by the edge or not — though only the last screen scrolls on to the
    /// list's end ([`Self::view`], [`Self::limit`]). Measured to the list's
    /// end, the screen before the last showed the last stop's words and
    /// controls whole uncounted, and Page Up from the last screen turned
    /// back onto it (Software Update, Appearance, Cursor Styles, Window at
    /// 60x20).
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

    /// The lines of controls stop `index` holds, top to bottom, each from
    /// and to under its top: its controls where it knows each
    /// ([`Self::set_control_spans`]), else its rows of controls, those that
    /// share any height merged into one line.
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

    /// ONE BOUNDARY FOR WHAT A SCREEN SHOWS AND WHAT IT DRAWS AS SHOWN: how far
    /// from the list's top a screen drawing `drawn`, scrolled `offset` with
    /// `caption` at its top, is live and drawn at full strength. The room —
    /// and under it the words of the last stop the screen shows, where those
    /// end in the strip — but never past the top of the first control after
    /// the stops it shows that the room does not hold whole:
    /// from there down the screen draws its peek, a picture fading out to the
    /// list's bottom, never live, a press on it scrolling what it pressed into
    /// the room and working nothing ([`SettingsViewState::press_list_picture`]).
    /// So a control is drawn live and whole, or fading from its top as a
    /// picture, never full strength over a part no press reaches: a row whose
    /// controls ran 3 to 30pt past the room's edge was painted at 0.87 and
    /// more of full strength, live above the edge only, uncounted — Page Down
    /// turned onto it again (Appearance, Sound, Terminal and four more pages
    /// at 102x29), and a press on its lower part only scrolled it. A row of
    /// controls whole in the room stays live (a card's first row of buttons,
    /// its next row past the edge). `base` where the screen shows no stop
    /// whole (a first stop taller than the room is live as far as it reaches).
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
        // Where the card of the first stop after those shown begins: down to
        // it the screen holds only the last shown card's padding and the gap,
        // and from it the peek fades, its top drawn at full strength where it
        // starts in the strip — at the strip's foot, past the fade's first
        // points, a card's top was a picture faded out of sight.
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
            // The first line of controls after the stops the screen shows
            // that the room does not hold whole — a row's control and the
            // name beside it, a card's next row of buttons, a stacked row's
            // switch under its name and Reset — is where the peek starts;
            // the lines over it stay live. A stop whose controls the room
            // holds, its words past the page's bottom or under a bar, stays
            // live: the bar covers only its words.
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
    ///
    /// A WHOLE ROW BEATS A KEPT CAPTION: where the scroll leaves a group's
    /// first row under its caption whole at the list's top — the row's words
    /// and controls in the room, only the card's padding and caption over
    /// them scrolled off — the caption scrolls with its card instead of being
    /// kept over that row, which it would cut, as a whole stop beats its
    /// caption on a screen seated at it ([`Self::starts_bare`]); the row
    /// names itself. (A whole card's caption is its one control's name, and
    /// stays kept over it.) Kept, it
    /// came down over the row a few points of scroll had left whole, and
    /// the edge rule took that row's cut control, name and all: the last
    /// screen of Cursor & Motion at 102x29 drew EFFECT POLICY over an empty
    /// card, Serious Mode gone under it; a press on Terminal's Accent color
    /// picture at 102x29 hid the Ambiguous-character width row under TEXT
    /// DIRECTION & WIDTH. Past that the stop is cut at the top anyway, and
    /// the caption is kept over the rows scrolling under it.
    ///
    /// A KEPT CAPTION HEADS WHAT IS UNDER IT: kept only while what its rows
    /// draw goes on under it for at least the least of a control a list's
    /// edge leaves drawn ([`PEEK_MIN`]); under that, no control and no line
    /// of words is drawn there, and the caption would head an empty sliver
    /// of its card. It scrolls with its card then, and what the rows still
    /// show is the screen's peek over its top. Kept, Top Settings' CURSOR
    /// TRAIL headed 1pt of its Cursor style row on the last screen at 102x29
    /// (Cantarell's measures).
    fn top_caption(&self, drawn: Range<usize>, offset: f32) -> Option<TopCaption> {
        let first = drawn.start;
        match self.runs(drawn).first() {
            Some(ScrollRun::Group { group, rows }) => {
                // A scroll into the card's top padding, over its caption,
                // only moves the card: the caption is kept once the scroll
                // reaches it.
                let padding = self.captions[*group].map_or(0.0, |(top, _)| top);
                let at = self.lead_of(first) - offset;
                let row_whole = offset > self.ink_top[first] + 0.01
                    && at + self.ink_top[first] >= -0.01
                    && self.shows_at(at, first, self.room);
                // Where what the run's rows draw ends under their top: the
                // rows' end, or their last words or control where those end
                // over it.
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

    /// Where a screen drawing `drawn`, scrolled `offset`, `height` tall and
    /// live `live` down, cuts through the caption of the card it draws last,
    /// from that card's top: the edge, and where the caption ends
    /// ([`Screen::edge_caption`]). Only where the edge is hard — nothing
    /// fades under the room: no strip, and the screen live down to its edge
    /// ([`Self::live_on`]) — and the card peeks in after a gap: where its
    /// peek fades, the caption fades out with it. A CAPTION IS DRAWN WHOLE OR
    /// NOT AT ALL: the words the edge would cut paint nothing, and the card
    /// goes on to the edge, so no band of blank is left under it.
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
    /// one the room's bottom edge cuts, or one drawn only in the strip under
    /// the room (a press on its picture there), so it ends at that edge; down
    /// (negative) for one cut at the top (the last screen's first stop, or
    /// under the caption it keeps), so it starts where the room does. A
    /// stop's worth of scroll — a row is taller than the part of a control a
    /// cut leaves — would carry it out from under the pointer. Measured on
    /// the screen as it draws, nudged already where an earlier press nudged
    /// it ([`Self::screen`]), and the whole nudge from the stops' seat
    /// returned as the seat it takes ([`Seat::nudge`]). `None` where the
    /// screen shows it whole or does not draw it, where it is taller than
    /// the room, or where the list ends too soon to scroll that far
    /// ([`Self::reveal_span`] then moves by stops).
    ///
    /// The control is the part of the stop from `from` to `to` under its
    /// top ([`Self::span_of`]: the control pressed). It is scrolled by its
    /// hidden part under the caption the screen keeps, or — where that
    /// caption, kept, would come down over the control (a 64pt switch under
    /// a 50pt caption in a 106pt room) — by the same least part with the
    /// caption let go ([`Seat::bare`]), its cut words not drawn, rather than
    /// by the whole lead that scrolls it off: the control moves no more than
    /// it must.
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
        // A control is whole in the room: a first stop taller than it reads
        // as live past it, words and all, but the list's edge cuts what of
        // its controls is under it (a bar that came over it); and one the
        // screen draws as its peek, fading under the room's edge, comes up
        // to end at that edge.
        let live = self.room;
        // Where the room starts for the control: under a caption kept whole
        // over the run the screen starts in, else the list's top — on the
        // screen as it draws, and on the screen nudged.
        let floor_now = self.caption_floor(screen.caption, &screen.drawn, index);
        // Cut by the room's bottom edge, or drawn only in the strip under it
        // (its picture there, which a press nudges in all the same:
        // [`SettingsViewState::press_list_picture`]).
        let nudge = if control_bottom > live + 0.01 && control_top < screen.height - 0.01 {
            control_bottom - live
        } else if control_top < floor_now - 0.01 && control_bottom > floor_now + 0.01 {
            control_top - floor_now
        } else {
            return None;
        };
        let offset = screen.offset + nudge;
        // The list's ends bound the scroll: never above its first stop, and
        // its last card never comes up past the room's edge.
        let end = self.card_bottom(start, self.len() - 1) - screen.offset;
        // Whether the screen nudged, its caption let go or not, shows the
        // part of the stop from `from` to `to` under its top whole.
        let shows = |bare: bool, (from, to): (f32, f32)| {
            let then = self.nudged(self.stop_screen(top), offset - seat, bare);
            let floor_then = self.caption_floor(then.caption, &then.drawn, index);
            offset >= -0.01
                && nudge <= end - live + 0.01
                && at + from - nudge >= floor_then - 0.01
                && at + to - nudge <= live + 0.01
        };
        let bare = self.bare_at(top);
        // A WHOLE STOP BEATS ITS CAPTION here too ([`Self::starts_bare`]):
        // where the caption kept over the run would come down over the
        // words that name the control — its row's name stacked over it,
        // nudged up under the caption by a taller status's bar (Appearance's
        // Follow system switch at 80x24 at 2× text, under THEME) — and the
        // screen that lets the caption go shows the whole stop, it goes.
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

    /// The nudge from its stops' seat a press on stop `index`'s controls
    /// takes on the screen whose first stop is `top` ([`Self::press_seat_span`]).
    #[cfg(test)]
    pub(super) fn press_nudge(&self, index: usize, top: usize) -> Option<f32> {
        (index < self.len())
            .then(|| self.press_seat_span(index, top, self.controls_of(index)))
            .flatten()
            .map(|seat| seat.nudge)
    }

    /// Whether the screen whose first stop is `top` can still stand scrolled
    /// `nudge` past its stops' seat ([`Self::press_seat_span`]) as the list
    /// measures now: never above its first stop, and its last card never up
    /// past the room's edge — a window that grew, or stops that went, can
    /// leave an earlier press's nudge with nothing to stand on.
    fn nudge_fits(&self, top: usize, nudge: f32) -> bool {
        let screen = self.stop_screen(top.min(self.limit()));
        if screen.whole || self.len() == 0 {
            return false;
        }
        let end = self.card_bottom(screen.drawn.start, self.len() - 1) - screen.offset;
        screen.offset + nudge >= -0.01 && nudge <= end - screen.live.max(self.room) + 0.01
    }

    /// Whether `seat` can still stand as the list measures now
    /// ([`Self::nudge_fits`]); a seat past a caption ([`Seat::bare`]) needs
    /// only that the list's last card not come up past the room's edge.
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

    /// `screen` scrolled `nudge` further ([`Self::press_seat_span`]): the stops
    /// that come into the page then drawn on, the first cut at the list's top
    /// edge, its captions kept and landed on as the last screen's and every
    /// screen's are ([`Self::top_caption`]), its peek fading as every
    /// screen's does ([`Self::live_on`]).
    fn nudged(&self, screen: Screen, nudge: f32, bare: bool) -> Screen {
        // A screen that let its caption go keeps it gone.
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
        let live = self.live_on(&drawn, offset, caption, base);
        Screen {
            live,
            edge_caption: self.edge_caption(&drawn, offset, reach, live),
            drawn,
            offset,
            height: reach,
            // Scrolled back from the last screen's end, the list goes on past
            // the room again.
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

    /// Say what the last stop added lists: the keys of its nodes
    /// ([`Self::stop_of_identity`]).
    pub(super) fn set_ids(&mut self, ids: Vec<UiKey>) {
        if let Some(last) = self.ids.len().checked_sub(1) {
            self.set_ids_of(last, ids);
        }
    }

    /// [`Self::set_ids`] for stop `index` (a card put in before the others,
    /// [`Self::insert_card`]).
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

    /// What stop `index` is known by: the nodes it lists, the controls it
    /// holds, then what else it draws ([`Self::within`]).
    pub(super) fn identities(&self, index: usize) -> impl Iterator<Item = &UiKey> {
        self.ids
            .get(index)
            .into_iter()
            .flatten()
            .chain(self.keys.get(index).into_iter().flatten())
            .chain(self.within.get(index).into_iter().flatten())
    }

    /// The stop that lists node `id` or holds control `id`, else the one
    /// that draws it ([`Self::within`]: a card's title that is a stop of its
    /// own at another text size and its card's heading at this one, a
    /// heading set in a title row at one size and alone at the other):
    /// where a stop of another measure of this list is now
    /// ([`Self::identities`]).
    pub(super) fn stop_of_identity(&self, id: &UiKey) -> Option<usize> {
        self.ids
            .iter()
            .position(|ids| ids.contains(id))
            .or_else(|| self.stop_of(id))
            .or_else(|| self.within.iter().position(|within| within.contains(id)))
    }

    /// The stop that holds what a press found at `key`: its control, a
    /// looping preview it draws (whose Pause/Play is no list control), or a
    /// node it lists — what a press on its picture in the strip, or on its
    /// part an edge cuts, brings into the room
    /// ([`SettingsViewState::press_list_picture`]).
    pub(super) fn stop_holding(&self, key: &UiKey) -> Option<usize> {
        self.stop_of(key)
            .or_else(|| self.stop_of_preview(key))
            .or_else(|| self.stop_of_identity(key))
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
    /// ([`ScrollPlan::press_seat_span`]), else by the least stops that show it
    /// whole ([`ScrollPlan::reveal_span`]).
    Pressed,
    /// Where the page LANDS ([`SettingsViewState::land_on_control`]): its
    /// stop's controls whole in the list's room; the next event after a
    /// render has seated it ends it, and a frame before that event with
    /// nothing changed draws the screen that render drew — one that measures
    /// the list otherwise (a resize, another text size) lands again.
    Land,
    /// A landing that SELECTS its control's whole stop
    /// ([`SettingsViewState::land_on_stop`]): all of the stop whole in the
    /// room, its words under its controls and its selection's bottom edge
    /// too, wherever the room holds it ([`ScrollPlan::reveal_selected`]).
    LandWhole,
    /// The keyboard a page turn put on a control the new screen draws only
    /// cut by its edge, no control of it whole
    /// ([`SettingsViewState::focus_list_screen`]): the list stays where the
    /// turn put it, the control live where it shows.
    Hold,
}

/// Which list a record measured: a page's list stays the same list while its
/// route, its search and the category sheet stay put, and while the Reset
/// All question does not take its place ([`SettingsViewState::open_question_scroll`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ScrollContext {
    route: SettingsRoute,
    search: String,
    categories: bool,
    question: bool,
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
    /// The scroll the render was asked to draw (`page_scroll` then): what
    /// the scroll still is until an event adopts `drawn`.
    asked: usize,
    /// The landing this render seated ([`SettingsViewState::land_on_control`]):
    /// its control, found on the list and followed.
    landed: Option<UiKey>,
    /// Whether the place this screen drew is still the list's: nothing has
    /// scrolled it since ([`SettingsViewState::drop_list_nudge`]), so the
    /// next render draws it where it was ([`SettingsViewState::seat_scroll`]).
    hold: bool,
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

    /// Whether nothing has scrolled the list since this screen was drawn:
    /// the scroll ([`SettingsViewState::scroll_now`]), clamped as this
    /// screen's list clamped it, is the screen's — a measure that lists the
    /// list in fewer stops clamps it lower, and the place it held is no less
    /// the one to keep.
    fn unscrolled(&self, state: &SettingsViewState) -> bool {
        self.context == state.scroll_context()
            && self.top == state.scroll_now().min(self.plan.limit())
    }
}

impl SettingsViewState {
    /// The list's scroll as the next event adopts it
    /// ([`Self::settle_page_scroll`]): where the last render put the list,
    /// until that event comes or something sets the scroll, else the scroll.
    /// A RENDER BEFORE THAT EVENT SEATS THE LIST FROM THERE, so it draws the
    /// screen the last one drew: a first screen that scrolled to the control
    /// the keyboard is on ([`Self::seat_scroll`]) read as scrolled away from
    /// on the next frame, which drew the list's top again (Settings ▸ Themes
    /// at 444x180 at 2× text, Customize open, showed its strip and Done
    /// first, then its demo); and a text size that kept a scrolled list at
    /// the stop it was at drew that stop, then the scroll it had been asked
    /// for (Keyboard & Input at 1219x522 made 2×).
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

    /// THE RESET ALL QUESTION SCROLLS ON ITS OWN in a compact window, where
    /// it can take the page's list's place ([`super::reset_all_question_page`]):
    /// opened, its list starts at its top, and the page's list keeps where
    /// it was, to come back to there once the question closes
    /// ([`Self::settle_question_scroll`]).
    pub(super) fn open_question_scroll(&mut self) {
        if self.question_scroll.is_none() {
            self.question_scroll = Some((self.scroll_context(), self.page_scroll));
            self.page_scroll = 0;
            self.page_reveal = None;
        }
    }

    /// The question closed: the page's list comes back where it was, unless
    /// another page has taken its place meanwhile (that page starts at its
    /// own top).
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

    /// Seat the list a render measured: clamp the scroll so the last screen
    /// is full, follow a pending reveal ([`Self::page_reveal`]), and record
    /// the plan, the bound and the stops shown for the reducer. Returns the
    /// stops this screen shows.
    pub(super) fn seat_scroll(&self, plan: &mut ScrollPlan) -> Range<usize> {
        let limit = plan.limit();
        let mut top = self.scroll_now().min(limit);
        let focus = self.common.last_focus.as_ref();
        // Where the last screen of this list showed `key`, if nothing has
        // scrolled the list since: its first stop and `key`'s stop then.
        let last_shown = |key: &UiKey| {
            let record = self.scroll_record.borrow();
            let record = record.as_ref()?;
            let stop = record.plan.stop_of(key)?;
            (record.unscrolled(self) && record.plan.shown(record.top).contains(&stop))
                .then_some((record.top, stop))
        };
        // Whether the last screen of this list, if nothing has scrolled it
        // since, showed `key` whole as a press shows it: what the keyboard's
        // control keeps of it ([`ScrollPlan::span_of`]).
        let last_whole = |key: &UiKey| {
            let record = self.scroll_record.borrow();
            record.as_ref().is_some_and(|record| {
                record.unscrolled(self)
                    && record.plan.stop_of(key).is_some_and(|stop| {
                        let span = record.plan.span_of(stop, Some(key), Reveal::Pressed);
                        record.plan.shows_span(stop, record.top, span)
                    })
            })
        };
        // Whether the last screen of this list, if nothing has scrolled it
        // since, showed more of `key`'s control than the screen whose first
        // stop is `top` shows on `now`, the control's stop `index` there:
        // something that came since cut it — a status bar, a shorter window,
        // a taller preview.
        let cut_since = |key: &UiKey, now: &ScrollPlan, index: usize, top: usize| {
            let shown = |plan: &ScrollPlan, stop: usize, top: usize| {
                let span = plan
                    .control_span(stop, key)
                    .unwrap_or_else(|| plan.controls_of(stop));
                plan.shown_of_span(stop, top, span)
            };
            last_shown(key).is_some_and(|(last_top, last)| {
                let record = self.scroll_record.borrow();
                record.as_ref().is_some_and(|record| {
                    shown(&record.plan, last, last_top) > shown(now, index, top) + 0.5
                })
            })
        };
        // A SCREEN KEEPS ITS CAPTION, OR LETS IT GO, UNTIL THE LIST SCROLLS
        // ([`ScrollPlan::starts_bare`]): a stop that grows or shrinks in
        // place under it — a Timer pressed open, its choices under it —
        // never moves the screen by the caption's lead. Opened at 2× text
        // in a 504x660 page, the Every session row's first part grew as
        // tall as the room, the screen let the SESSIONS caption go, and the
        // Timer just pressed jumped 53pt up. Decided again where the list
        // scrolls, or its room changes.
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
        // THE NUDGE STANDS: the hidden part a press scrolled a cut control by
        // ([`ScrollPlan::press_seat_span`]) is part of the list's place, as a
        // platform list holds where a click scrolled it — the last screen's,
        // where nothing has scrolled the list since
        // ([`Self::drop_list_nudge`]).
        let carried = self
            .scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.unscrolled(self))
            .and_then(|record| record.plan.seated.filter(|seat| seat.top == record.top));
        // THE LIST HOLDS ITS PLACE: where stops above the control a person
        // is on came or went since the last screen (a note a choice retired,
        // a row Reset took away, a row above listed in parts or whole again
        // as a republish changed its height), the list moves by as many, so
        // that control stays where it was on screen — and with no control
        // of the last screen the person is on (none, or the keyboard off the
        // list: on the search field, on the sidebar row that opened the
        // page), the last screen's first stop BY WHAT IT IS — the nodes it
        // lists, its controls ([`ScrollPlan::stop_of_identity`]), the first
        // stop after it listed again where its own first is not (a list at
        // its top stays there): a text size that lists the same list in
        // other stops leaves it at the stop it was at (Themes at 2× text
        // lists its demo and strip as two stops more, and kept the number;
        // Harness's notes hold no control to know them by). A seat it carried
        // moves with it.
        let shown_first = || {
            let record = self.scroll_record.borrow();
            let record = record
                .as_ref()
                .filter(|record| record.unscrolled(self) && record.top > 0)?;
            let first = record.plan.shown(record.top).start;
            (first..record.plan.len()).find_map(|stop| {
                record
                    .plan
                    .identities(stop)
                    .find_map(|id| plan.stop_of_identity(id))
                    .map(|index| (record.top, stop, index))
            })
        };
        // A LANDING A SCREEN HAS DRAWN IS LET GO AS DRAWN by a frame that
        // measures the list as that screen did — the same room, each stop as
        // tall — as the next event lets it go ([`Self::settle_page_scroll`]),
        // so a frame before that event with nothing changed draws the same
        // screen: Themes opened with the keyboard already on its Community
        // disclosure drew the disclosure, the landing yielding to it, and the
        // next frame landed on Nord, the theme in use. This screen passes it
        // on for that event to let go. A LIST MEASURED AGAIN LANDS AGAIN: a
        // window resized or a text size changed sends the view no event, so
        // a screen measured otherwise keeps the landing — let go, Themes
        // opened on Nord at 1144x700 and made 1144x462 lost Nord's row.
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
                    && (record.plan.room - plan.room).abs() < 0.5
                    && record.plan.heights == plan.heights
            })
            .and_then(|record| record.landed.clone());
        let landing = land && drawn_landing.is_none();
        // A control a page turn held the keyboard on reveals nothing
        // ([`Reveal::Hold`]).
        let revealing = self
            .page_reveal
            .as_ref()
            .filter(|_| self.reveal_as != Reveal::Hold && drawn_landing.is_none());
        let kept_in_view = revealing.or(focus);
        let held_in_view = kept_in_view.and_then(last_shown);
        // The place held: the last screen's first stop, the stop it was held
        // by then, and that stop now.
        let held = match (kept_in_view, held_in_view) {
            (Some(key), Some((last_top, last))) => {
                plan.stop_of(key).map(|index| (last_top, last, index))
            }
            (Some(_), None) if revealing.is_some() => None,
            _ => revealing.is_none().then(shown_first).flatten(),
        };
        let mut carried = carried;
        if let Some((last_top, last, index)) = held {
            // A stop the list now numbers otherwise is never left over the
            // screen's top: on the last screen, whose first stop the list's
            // end puts above its scroll, the scroll counted past it (the
            // last screen of Appearance at 220x12, its first stop the Output
            // streak row, was one stop further on at 2× text and showed
            // only the row's note). One numbered as it was keeps the scroll —
            // but for one a last screen drew whole over its seat, where the
            // list measured again (another text size) seats there a screen
            // that is not the last: from there it showed only the stops
            // after it (Sound at 102x29: at 2× the screen started a stop past
            // the row the 1× end drew whole), so the scroll comes to it.
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
        // THE LIST HOLDS WHERE IT DREW: where nothing has scrolled it since
        // a screen drawn off its stops' seat — the last screen, scrolled to
        // the list's end, or a screen a press nudged or Page Up anchored —
        // the next screen draws the stop the place was held by (the control
        // the person is on, else that screen's top stop) where it was, so no
        // row moves for what changed under it: a stop the press added after
        // the list's end (Cursor Styles at 220x60 with Off saved, a style
        // pressed on the last screen: its Adjust card came after the end,
        // the same first stop was no longer the last screen's, and every
        // row went 40pt up from under the pointer), or a status bar that
        // came over the strip the last screen drew its end into. Where that
        // place no longer fits the list (it shrank, the window grew), the
        // list seats as its stops say.
        if let Some((seat_top, seat)) = self.held_place(plan, top, held) {
            top = seat_top;
            plan.seated = seat;
        }
        let mut landed = drawn_landing;
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
            Some(index) => Some((index, Reveal::Keep, None)),
            None => revealing.and_then(|key| {
                plan.stop_holding(key)
                    .map(|index| (index, self.reveal_as, Some(key)))
            }),
        };
        if let Some((index, reveal_as, key)) = reveal {
            // A TALLER STATUS NEVER MOVES THE STOP THE KEYBOARD IS ON MORE THAN
            // ITS CONTROL NEEDS: where the last screen showed it and the bar
            // that came shortens the room ([`ScrollPlan::gives_to_status`]),
            // the keyboard's reveal keeps its control whole, as a press's
            // does — revealing its words too scrolled Top Settings' Follow
            // system switch 50pt up under a three-line save failure.
            let reveal_as =
                if reveal_as == Reveal::Keep && held_in_view.is_some() && plan.gives_to_status() {
                    Reveal::Pressed
                } else {
                    reveal_as
                };
            // What the reveal brings into view: a press, the control pressed.
            let span = plan.span_of(index, key, reveal_as);
            // A press, a change or a landing shows the control whole where it
            // is not already (a landing that selects its stop, all of the
            // stop; a press on one an edge cuts by its hidden part, so it
            // stays under the pointer); the keyboard shows its whole stop,
            // words too. A screen a press nudged that shows it so keeps its
            // place, nudge and all.
            if landing {
                landed.clone_from(&self.page_reveal);
            }
            if plan.seated.is_none() || !plan.shows_span(index, top, span) {
                // Measured on the screen as it draws, an earlier nudge in it.
                let pressed = if reveal_as == Reveal::Pressed {
                    plan.press_seat_span(index, top, span)
                } else {
                    None
                };
                let nudge = pressed.map(|seat| seat.nudge);
                plan.seated = pressed;
                // A PRESS WHOSE CONTROL THE SCREEN DOES NOT DRAW YET — a Timer
                // opened under its button, its choices a part of the row
                // listed after the button's — scrolls by the least that shows
                // it, from where the list stands ([`ScrollPlan::seat_span`]),
                // so what was pressed stays on the screen: a reveal by whole
                // stops seated the choices' part first and the row went
                // off the top.
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
            // WHERE THE STOPS' SEAT STILL CUTS WHAT THE REVEAL MUST SHOW in
            // the list's room — under the caption it keeps, or at the room's
            // edge, which cuts a first stop taller than the room though it
            // reads as live past it — the list scrolls on into the stop
            // ([`ScrollPlan::seat_span`]). A landing that selects its stop
            // has its own seat ([`ScrollPlan::land_selected`]).
            if plan.seated.is_none() && reveal_as != Reveal::LandWhole {
                plan.seated = plan.seat_span(index, top, span);
            }
        } else if let Some(key) = focus
            && let Some(index) = plan.stop_of(key)
            // The list's first screen shows the control the keyboard is on,
            // its whole stop; after that, nothing scrolled the list since the
            // last screen, which showed it whole or more of it than this one
            // would: a status bar that came, a shorter window, a taller
            // preview keep that control in view — a bar that takes the strip
            // under the facts of the row it is on covers only those words and
            // never moves the list. ONE THE LAST SCREEN DREW CUT STAYS SO
            // UNTIL SOMETHING CUTS IT MORE, as the next frame draws the same
            // screen: kept in view wherever the last screen showed it at all,
            // Home at 444x292 at 2× text under a short status, the wheel gone
            // down the list and back, drew its Follow system switch cut 8pt by
            // the room's edge, then whole 8pt higher on the next frame; kept
            // only where it was whole, the refusal of the switch's write at
            // 300x292, come after the wheel left it cut, covered it (#92).
            && (first_screen || last_whole(key) || cut_since(key, plan, index, top))
        {
            let reveal = if first_screen {
                Reveal::Keep
            } else {
                Reveal::Pressed
            };
            let span = plan.span_of(index, Some(key), reveal);
            if !plan.shows_span(index, top, span) {
                // NOTHING THE PERSON PRESSED MOVES MORE THAN IT MUST: a bar
                // that came over the control the keyboard is on (a status
                // whose words take more than the room kept for one) scrolls
                // it whole by its hidden part, as a press on it does
                // ([`ScrollPlan::press_seat_span`]), so it stays where it was
                // but for that; only where the list cannot scroll so does it
                // move by stops. By stops, a status at 22x24 lifted the
                // switch just pressed 39pt for a 13pt cut.
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
            }
        }
        let view = plan.screen(top).view;
        self.record_result_page_limit(limit);
        self.page_window.set(Some(plan.shown(top).len().max(1)));
        self.scroll_record.replace(Some(ScrollRecord {
            context: self.scroll_context(),
            plan: plan.clone(),
            top,
            drawn: Some(top),
            asked: self.page_scroll,
            landed,
            hold: true,
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
    /// already open is landed on by the Autopilot menu's `Harness settings…`
    /// (Settings ▸ Harness's `harness/land`) while the refreshes its arrival
    /// started are still out, and their replies (`HarnessChanged`, `PackagesChanged`,
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

    /// Scroll the page's list to `top` as an event does: the event first
    /// adopts where the last render put the list ([`Self::settle_page_scroll`]).
    /// A TEST SETS THE SCROLL SO, never by `page_scroll` alone: written over
    /// a render no event has settled, a scroll equal to the one that render
    /// was asked to draw reads as no scroll at all ([`Self::scroll_now`]),
    /// and the next render draws where that one did — Themes' screen 0,
    /// asked for after a first render that landed on the theme in use, drew
    /// the landing.
    #[cfg(test)]
    pub(crate) fn scroll_page_to(&mut self, top: usize) {
        self.settle_page_scroll();
        self.page_scroll = top;
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
        // The keyboard a page turn held on a cut control stays held there.
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

    /// Where the screen the last render drew holds the list on `plan`
    /// ([`Self::seat_scroll`]'s THE LIST HOLDS WHERE IT DREW), where that
    /// screen was the list's last — scrolled to the list's end, where the
    /// strip under the room lets that end sit ([`ScrollPlan::last_screen`]):
    /// the first stop and seat that draw the stop the place was held by —
    /// `held`'s, the control the person is on, as the last screen's stop and
    /// this plan's, else that screen's top stop — at the height it was drawn
    /// at, where the stops' own seat at `top` does not already; and a press's
    /// nudge it carried, kept as far past the list's end as it was. `None`
    /// where nothing holds the list there: it scrolled since, its screen was
    /// not the last (the stops and the nudge hold it, [`Self::seat_scroll`]),
    /// or the place does not fit the list as it is now measured.
    fn held_place(
        &self,
        plan: &ScrollPlan,
        top: usize,
        held: Option<(usize, usize, usize)>,
    ) -> Option<(usize, Option<Seat>)> {
        let record = self.scroll_record.borrow();
        let record = record.as_ref()?;
        let was = &record.plan;
        if !record.hold
            || !record.unscrolled(self)
            || record.top < was.limit()
            || plan.foot == ListFoot::Whole
            || plan.len() == 0
        {
            return None;
        }
        // A PRESS'S NUDGE ON THE LAST SCREEN stands as far past the list's
        // end as it drew, though the end moved with the strip (a bar came).
        if let Some(seat) = was.seated.filter(|seat| seat.top == record.top)
            && top == record.top
            && top >= plan.limit()
        {
            let (before, now) = (was.stop_screen(record.top), plan.stop_screen(top));
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
        let drawn = was.screen(record.top);
        if drawn.whole {
            return None;
        }
        let first = drawn.drawn.start;
        // The stop the place is held by, as that screen drew it and as this
        // plan lists it.
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
        // Where the stops' seat at `top` draws it there already, they hold it.
        let seated = plan.stop_screen(top.min(plan.limit()));
        if seated.drawn.start <= anchor && (on(&seated) - at_was).abs() < 0.5 {
            return None;
        }
        // The screen that starts where that screen started, scrolled so the
        // stop is where it was.
        let start = was
            .identities(first)
            .find_map(|id| plan.stop_of_identity(id))
            .filter(|start| *start <= anchor && *start <= plan.limit())?;
        let base = plan.stop_screen(start);
        if base.view.start != start || base.whole {
            return None;
        }
        // Only a list measured as it was over that stop holds it there: one
        // laid out again at another text size or width keeps its place by
        // the stops it lists ([`Self::seat_scroll`]'s THE LIST HOLDS ITS
        // PLACE).
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

    /// THE LIST SCROLLED (a wheel, a key, a reader's scroll, a page opened
    /// at its top): the nudge a press gave it ([`ScrollPlan::press_seat_span`])
    /// goes, and it seats on its stops again — at an end of the list too,
    /// where the scroll moves no stop, so a wheel up at the top shows the top.
    pub(super) fn drop_list_nudge(&self) {
        if let Some(record) = self.scroll_record.borrow_mut().as_mut() {
            record.plan.seated = None;
            record.hold = false;
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

    /// A PRESS ON A CONTROL'S PICTURE IN THE STRIP under the list's room
    /// ([`crate::native_ui::CompiledUi::list_picture_at`]) nudges it into the
    /// room ([`ScrollPlan::press_seat_span`]) and works nothing, as a platform
    /// list scrolls a row a click finds out of view into view: a click on
    /// what looks like a control is never silently lost, and the strip still
    /// holds nothing live a status bar that comes could take. `false` where
    /// no stop of the list the last render measured holds what was pressed
    /// ([`ScrollPlan::stop_holding`]: a control, or a looping preview).
    pub(super) fn press_list_picture(&mut self, key: UiKey) -> bool {
        let listed = self
            .scroll_plan()
            .is_some_and(|plan| plan.stop_holding(&key).is_some());
        if listed {
            self.reveal_pressed(key);
            return true;
        }
        // A SIDEBAR ROW ITS LIST'S EDGE DREW WITHOUT ITS NAME
        // ([`crate::native_ui::LIST_PEEK_MIN`]'s rule): the press takes the
        // keyboard there, and the sidebar scrolls to keep the keyboard's row
        // whole; it opens no page it did not name.
        if key.as_str().starts_with("settings/nav/") {
            self.common.last_focus = Some(key);
            return true;
        }
        false
    }

    /// A PRESS KEEPS ITS CONTROL IN VIEW, however the press came: a pointer
    /// puts the keyboard on the control itself before its action arrives (no
    /// focus change the reducer sees), and a screen reader's press moves no
    /// keyboard at all — so a press on list control `key` keeps it in view
    /// as pressed ([`Self::reveal_pressed`]): one an edge cuts comes whole
    /// into the list's room by its hidden part, one the screen shows whole
    /// moves nothing, and neither moves for what the press changes on the
    /// page — the chrome and the preview it was seated under, the rows it
    /// brings — nor goes under the status it brings up. A pointer press on
    /// Follow system at 80x16 left no control in view: the rows it added
    /// had the page choose its chrome again, the preview went into the
    /// list, and the switch jumped 78pt up from under the pointer (a Tab and
    /// Space kept it where it was). A reveal the action asked for itself
    /// (its page_reveal no longer `before`) stands.
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

    /// A page opened FOR one of its controls (Settings ▸ Harness from the
    /// Autopilot menu's `Harness settings…`,
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

    /// Whether the stop holding the control the list keeps in view
    /// ([`Self::page_reveal`]) is no taller than the list's room, as the
    /// page was last seated: none kept, or one not listed, is.
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

    /// A PAGE TURN TAKES THE KEYBOARD WITH IT (#84): where Page Down or Page
    /// Up scrolled the control the keyboard is on off the screen, the
    /// keyboard moves to the first control the new screen shows whole, as
    /// a person reads it from its top — Page Up too, as a platform list's
    /// Page Up takes its selection to the first row in view (it went to the
    /// screen's last control, and the next Down arrow scrolled the list
    /// back to the screen the turn left). On Cursor Styles the preview
    /// follows the keyboard, so it follows the page too. A keyboard outside
    /// the list stays where it is.
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

    /// The keyboard to the first control the list's screen shows, ringed: of
    /// the stops it shows as the keyboard's landing reveals them, controls
    /// and words whole ([`ScrollPlan::shows_landing`]) — a stop its edge cuts
    /// only in its padding among them, whose control was skipped for the
    /// next stop's (Top Settings, Text & Fonts, Tab Color, Ai and Terminal
    /// at 80x24 at 2× text); where none of them holds a control (a screen of
    /// a row's note over the next row, that row's slider cut by the room's
    /// edge — Appearance's Minimum contrast note at 80x24 at 2× text), the
    /// first control the screen draws live, cut by its edge, held there with
    /// the list where the turn put it ([`Reveal::Hold`]) — of a segmented
    /// control, the first segment it draws live ([`ScrollPlan::live_control`]):
    /// the keyboard is never left on a control the screen does not draw. A
    /// screen that draws no control leaves it where it is.
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

    /// A STATUS NEVER COVERS WHAT THE PERSON IS ON (#92): where a plain
    /// status's bar takes so much of `plan`'s room — laid out `room` tall, as
    /// the page kept it — that what is left is shorter than the control the
    /// person just changed or the keyboard is on, note how much shorter
    /// ([`Self::status_overrun`]): [`super::page`] seats the page again with
    /// the bar that much shorter, its words ending where it ends, so the
    /// control stays whole where it was, or scrolled by its hidden part as a
    /// press scrolls one ([`ScrollPlan::press_seat_span`]). At 22x24 at 2×
    /// text "Allow programs to read the clipboard turned on" took 228pt of the
    /// window and left the switch just pressed a 40pt list: under the bar,
    /// where a second click landed on Undo. A control taller than the room
    /// even with no bar up asks nothing of the bar.
    pub(super) fn note_status_room(&self, plan: &ScrollPlan, room: f32) {
        let given = plan.given;
        if given <= 0.01 || self.measuring_calm.get() {
            return;
        }
        let Some(key) = self
            .page_reveal
            .as_ref()
            .or(self.common.last_focus.as_ref())
        else {
            return;
        };
        let Some(index) = plan.stop_holding(key).or_else(|| plan.stop_of(key)) else {
            return;
        };
        let (top, bottom) = plan.span_of(index, Some(key), Reveal::Pressed);
        let need = bottom - top;
        let overrun = given - (room - need);
        if need <= room + 0.01 && overrun > 0.01 {
            self.status_overrun
                .set(self.status_overrun.get().max(overrun));
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

    /// TAB ENTERS A LIST WHOSE SCREEN SHOWS NONE OF ITS CONTROLS: where a
    /// Tab (`forward`) or Shift-Tab passes over the page's list in reading
    /// order and Tab `order` holds none of its controls (its screen shows
    /// words only — a hero, a status card, a row's note), the list's first
    /// control (Shift-Tab: its last). The keyboard never went into such a
    /// list: Tab from the sidebar or the toolbar went round the window's
    /// chrome, on 17 pages at 102x12 at 2× text, 13 at 220x12, 7 at 60x20 and
    /// 5 at 80x24. `None` where the screen shows one of its controls (Tab
    /// walks into it, [`Self::tab_past_screen`]) or the list holds none.
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

/// The keys of every node inside `node`, in order, onto `out`
/// ([`ScrollPlan::set_within`]).
fn keys_within(node: &UiNode, out: &mut Vec<UiKey>) {
    for child in &node.children {
        out.push(child.key.clone());
        keys_within(child, out);
    }
}

/// Each control `node`, laid out `height` tall, holds, and where it sits in
/// it as the compiler lays it out: its key, its top and its height
/// ([`ScrollPlan::set_control_spans`]).
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
/// Whether `drawn`, a key [`focus_keys`] gives, is control `key`: itself,
/// or for a segmented control any of its segments (the one it gives is its
/// chosen segment, which carries the control's key, else its first).
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
        // A control that is busy (a check it started still running) is
        // still the row's control.
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
        bare: false,
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

/// [`seat_room`] for a list a plain status's bar may stand under: its
/// screens give up what that bar takes beyond the room the page kept for
/// one ([`SettingsViewState::status_extra`]) from their bottom, the page and
/// its stops laid out as with that `room`; and the list notes where the bar
/// would leave it less than the control the person is on needs whole
/// ([`SettingsViewState::note_status_room`]).
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
    // backdrop — save on a list that ends on whole stops, inside a panel
    // snapped to them, where it is the list's room on every screen, so it
    // never changes length as the screens' whole stops do.
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

/// Make `card` — a group run's card, its caption (`caption` tall from its
/// top, padding included) over its rows — keep that caption whole with its
/// rows scrolled `offset` under it, the card that much shorter. `false`, the
/// card as it was, where its rows are not `offset` tall: the list scrolls.
/// The rows scroll in a room of their own under the caption
/// ([`UiNode::under_caption`]): a row its edge cuts is live where it shows,
/// a line of words it cuts through paints nothing, and every row it leaves
/// whole is audited as any other.
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

    /// Where each control `rows` hold sits, from the top of the first
    /// ([`ScrollPlan::set_control_spans`]).
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

    /// Whether every one of `rows` is A NOTE ON THE ROWS OVER IT
    /// ([`notes_rows_over`]), a control over it in the card: a stop of such
    /// rows says how the rows above stand ([`ScrollPlan::set_note`]).
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
    /// The most of a screen `room` tall one of the card's rows may take, the
    /// card's padding and its head over it: what a run of that row alone
    /// leaves it ([`Self::height_of`]).
    fn row_limit(&self, room: f32) -> f32 {
        room - self.height_of_lead() - self.padding.bottom
    }

    /// Whether `row` of the card is listed a part at a time on a screen
    /// `room` tall ([`row_parts`]): it is taller than the screen shows whole
    /// under the card's head ([`Self::row_limit`]) — except A ROW WITH AN
    /// OPEN DISCLOSURE whose words and controls are no taller than the screen
    /// itself (the inset round them not counted), which stays one with
    /// what it opened: a landing on it lets the card's caption go and draws
    /// it whole ([`ScrollPlan::land_selected`]; Settings ▸ Harness's session
    /// row with its Timer open).
    fn splits(&self, (node, height): &(UiNode, f32), room: f32) -> bool {
        // What it draws: the row less the inset round its words (a SESSIONS
        // row's outline, [`crate::native_settings`]' `row_cards_inset`),
        // which a landing may let the room's edge take.
        let drawn = height - node.layout.padding.top - node.layout.padding.bottom;
        *height > self.row_limit(room) + 0.01
            && splittable(node)
            && !(drawn <= room + 0.01 && holds_open_disclosure(node))
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
                    // A row with an open disclosure is listed in parts as tall
                    // as the screen: a landing on it lets the card's caption
                    // go and draws its first part whole, what it opened with
                    // it where that fits.
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
///
/// A BOXED ROW'S PIECES READ AS ONE CARD: where the row paints a box (a
/// card's style), each piece's edges toward the pieces before and after it
/// are open ([`crate::native_ui::Joins`]), its face carried over the `gap`
/// its list keeps between them, so the pieces stand as one box — not a box
/// of their own each, closed at both ends.
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
    // Each child at the height the column gave it, now its own: a part's
    // height is its children's.
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
        // A CONTROL IS NEVER LISTED WITHOUT THE WORDS THAT NAME IT: the
        // row's first part — its name, the Reset its name line ends with
        // where the setting is changed — keeps the first control under it
        // wherever the two fit a screen (`most`), the part then taller than
        // a screen holds under its caption, which its screen lets go
        // ([`ScrollPlan::starts_bare`]). Cut between them, at 2× text in a
        // 102x12 window every Appearance slider and switch was on a screen
        // with no name over it. Read off the name line's place, never what
        // it holds, so a change that brings its Reset re-cuts nothing.
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
    // A box of a card's style ([`crate::native_ui`]'s group painter).
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
                    // What else it draws: the card's heading over its first
                    // stop — at another text size a stop of its own (Home's
                    // card titles, Themes' section captions at 1×), and a
                    // screen that started with it starts with this stop —
                    // and what is inside its rows.
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
/// reveal on its first screen — as the next frame's does. Every node the
/// render builds for the list (the screen's rows and, for
/// [`list_page_whole`], the rows off it) comes from this one seating, so
/// no row is built twice.
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

    /// [`Self::new`] for a list whose screens end at its bottom edge as
    /// `foot` says. Its stops are cut for `room`; its screens give up what a
    /// taller plain status's bar takes beyond the room the page kept for
    /// one ([`SettingsViewState::status_extra`]), from their bottom: the
    /// stops, and the switch just pressed over the bar, stay where they
    /// were.
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

/// [`list_page`] whose rows off the screen are in the tree too
/// ([`UiNode::offscreen`]): laid out above and below the screen where
/// scrolling brings them, so a screen reader and `controls` see every row
/// the list holds, while only the screen is painted, hit and focusable. Its
/// screens end on whole stops ([`ListFoot::Whole`]): a list in a panel, with
/// no page's bottom under it to draw a peek on.
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
        assert_eq!(
            screen.live, 158.0,
            "drawn whole down to its note's end, and on over the gap to the next card"
        );
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
    /// screen, seated at the first stop it shows whole though it scrolls into
    /// that stop's padding, it turns back from that stop. CONTROL: where a
    /// seated screen ends there, Page Up seats at it.
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
        // From the last screen, which shows its first stop whole though its
        // top edge cuts that stop's padding, Page Up turns back from that
        // stop.
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

    /// A RUN'S NEXT STOP STARTS WHERE ITS CARD DRAWS IT, WHATEVER THE CARD'S
    /// PADDING (the audit of the Customize sheet, whose sections
    /// have no padding and 4pt gaps: each row stop carries its 4pt share of
    /// the gap, which the card's last row does not draw, and the plan put
    /// what came after a section 4pt below where the list drew it — Save as
    /// left off a screen it fitted on, 40pt of the list blank). For split
    /// cards with a caption and without, their bottom padding 0, 2, 4 and
    /// 20pt against a 4pt gap, then a whole card, on a screen from each
    /// row: the card starts a list gap under the run before it as the list
    /// lays that run out (the height of [`SplitCard::node`], which the plan
    /// never reads: a run's head drifting from the card's drawn padding and
    /// gaps is a red here), and a list as tall as that run, the gap and the
    /// card shows it. NEGATIVE CONTROL: a list 1pt shorter does not.
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
                    // The run as the list lays it out from this row (each
                    // stop is one row): the card's own node, at its fixed
                    // height, a list gap over the next run.
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
                    // NEGATIVE CONTROL: a list 1pt shorter.
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

    /// The Themes chooser's stops beside the demo, 32pt rows in a room of
    /// four: two rows, a captioned section of six (its 32pt caption over
    /// them), a 40pt disclosure and two more rows, each a control; and how
    /// its screens end (`foot`).
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

    /// A LIST THAT ENDS ON WHOLE STOPS DRAWS NONE CUT ([`ListFoot::Whole`]):
    /// on every screen of the chooser's stops, the screen draws exactly the
    /// stops it shows, seated at its first, all of them inside the room —
    /// the last screen too, seated at the least stop that shows the list's
    /// end — and the thumb's track is the room on every screen; Page Up
    /// turns back by stops, never anchored past one, and a press on a
    /// control seats nothing past its stop. NEGATIVE CONTROL: the same stops
    /// ending on a cut draw a stop past the room on some screen, scroll the
    /// last one into its first stop, and a press on the stop its edge cuts
    /// nudges the list past that stop's seat.
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
        // NEGATIVE CONTROL: ending on a cut.
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

    /// A control the screen draws only in the strip under its room — its
    /// picture there, which a press nudges in (#51) — is nudged up by all of
    /// it, to end at the room's edge, as one the edge cuts is by its hidden
    /// part: never a stop's worth of scroll that would carry it further.
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
        // The fourth card's control, 126..166, wholly under the room's edge.
        assert_eq!(
            plan.press_nudge(3, 0),
            Some(66.0),
            "126..166 becomes 60..100"
        );
        plan.seated = Some(Seat::nudged(0, 66.0));
        assert!(plan.shows_controls(3, 0), "whole in the room");
        assert_eq!(plan.press_nudge(3, 0), None, "whole moves nothing");
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
    /// 738x418). Kept, it heads what its rows draw under it: at least
    /// [`PEEK_MIN`] of them, less being a sliver nothing is drawn in. CONTROLS:
    /// a run with more rows under the caption keeps it, and a nudge on a
    /// screen that kept its caption keeps it.
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
        // the caption, so it is let go; 44pt in, 18pt does, and it is kept.
        let tail = plan.top_caption(1..2, 64.0);
        assert_eq!(
            tail, None,
            "64pt into a 62pt run: let go (past its words, so none to hide)"
        );
        // 48pt in, the gap leaves 14pt under it, a sliver: let go — where
        // without the row gap the plan counted 18pt and kept it.
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
        // A sliver under it heads nothing: let go.
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
        // CONTROL: a screen that kept its caption keeps it, nudged back.
        let kept = Screen {
            offset: 44.0,
            caption: plan.top_caption(1..2, 44.0),
            ..let_go
        };
        let nudged = plan.nudged(kept, -2.0, false);
        assert_eq!(nudged.caption.map(|caption| caption.kept), Some(true));
        assert_eq!(plan.caption_floor(nudged.caption, &nudged.drawn, 1), 40.0);
    }

    /// A HARD EDGE NEVER CUTS A CONTROL LIVE NOR A CAPTION THROUGH ITS WORDS,
    /// AND LEAVES NO BLANK UNDER THE CARD IT CUTS: where no strip is kept
    /// under the room (a bar is up, the page keeps none), the card after the
    /// screen peeks in on to the edge. From the first control the room does
    /// not hold whole, the peek is a picture fading out to the edge, never
    /// live — cut there and live, a row lost its words to the edge and kept a
    /// pressable face (Software Update's switch at 80x24 with no name,
    /// Terminal's Bidirectional text segments with no words) — and where the
    /// edge cuts only words, a caption it would cut through paints none of
    /// them ([`Screen::edge_caption`]: the edge and the caption's end, from
    /// the card's top). CONTROLS: an edge past the caption cuts the card's
    /// first row and fades from its top, the caption whole; where the strip
    /// fades, the peek is a picture drawn on to the page's bottom; a card
    /// whose caption the plan does not know is drawn to the edge.
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

    /// A WHOLE STOP BEATS ITS CAPTION (#96): a group of rows whose 64pt
    /// controls sit under a 50pt caption in a 100pt list (2× text in a
    /// 102x12 window) cuts every row on the screen that starts with it,
    /// caption kept; that screen lets the caption go instead, scrolled off
    /// by its lead, and draws the row whole from the room's top — so the
    /// wheel (a screen per stop) and Page Down (the stop after a screen)
    /// land every row whole. A row that fits under the caption keeps it.
    /// Fails with the screens seated under their captions.
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
            // The last screen scrolls only as far as the list's end.
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
        // CONTROL: a 40pt row fits under the caption, which stays.
        let mut fits = ScrollPlan::new(4.0);
        let group = fits.group(60.0, 50.0);
        fits.row(group, 44.0, vec![UiKey::new("a")]);
        fits.row(group, 44.0, vec![UiKey::new("b")]);
        fits.set_room(100.0);
        assert!(!fits.starts_bare(0) && !fits.starts_bare(1));
        assert!(!fits.screen(0).bare);
    }

    /// A STOP A BARE SCREEN SHOWS WHOLE IS NOT A CUT STOP: the measure the
    /// page's chrome is chosen by ([`ScrollPlan::cut_stops`]) counts a stop
    /// as the screen that starts with it draws it — past its run's caption
    /// where that screen lets the caption go ([`ScrollPlan::starts_bare`]).
    /// Counted under the caption, Cursor Styles' 114pt trail-slider rows
    /// under a 50pt caption in a 156pt list read as cut, and the page kept
    /// its preview pinned over an 88pt list its 92pt tiles did not fit.
    /// CONTROL: a row taller than the room is cut, caption or not.
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

    /// THE LAST SCREEN NEVER CUTS ITS FIRST STOP UNDER A KEPT CAPTION: where
    /// the list's last stop is the screen's first and the scroll to the
    /// list's end — its card's closing padding — would carry it under the
    /// caption kept over it, the caption scrolls with it instead: Window's
    /// Build badge row at 220x12 at 2× text, 76pt of ink under a 50pt
    /// caption in a 132pt list, was cut 8pt on the only screen that drew
    /// it. Fails with the caption kept.
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

    /// A REVEAL NEVER SCROLLS PAST THE STOP IT REVEALS: where the screen
    /// starting with a stop still cuts the part a reveal must show — all of
    /// a stop taller than the room, its padding under its words included,
    /// for a landing that selects it — the reveal by stops keeps that stop
    /// first, as no later screen shows it at all. It returned the stop after
    /// it.
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

    /// Cursor Styles at 150x48 with Beam saved, as the plan measures it:
    /// three cards, then the Adjust card's caption over five rows, the last a
    /// slider row whose note runs 2pt past the room into the strip kept for
    /// a status bar; `now` of that strip free.
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

    /// NO TURN ONTO THE LAST SCREEN ONLY MOVES THE LIST: the screen seated
    /// at the Adjust card's first row draws its last row whole — its slider
    /// in the room, its note 2pt into the strip, the card's closing padding
    /// on the screen — so it is the list's last screen, and Page Down from
    /// the screen before lands on it, showing its last rows. The bound stood
    /// a stop further on, measured to the room alone, and the turn onto it
    /// moved the card 4pt and shortened the list 32pt, nothing new on it
    /// (SC1). The last screen ends the list at the strip's foot, no band of
    /// the strip left blank under it, the card before peeking in over its
    /// top. CONTROL: with a bar up the strip is the bar's, and the last
    /// screen ends the list at the room's edge, its last card whole there.
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

    /// WITH A STATUS BAR UP, THE LAST SCREEN SHOWS THE LAST CARD WHOLE: the
    /// strip the page keeps for the bar is the bar's, so the last screen ends
    /// the list — its last card's closing padding and border — at the room's
    /// edge, not past it under the bar. It let that padding fall in the kept
    /// strip, so End with a bar up cut the last card by it on every page
    /// (Terminal at 102x29: 14pt; Modified at 80x24: 18pt). CONTROL: with no
    /// bar up, the end may sit in the strip, the last stop's controls in the
    /// room.
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

    /// ONE BOUNDARY FOR WHAT A SCREEN SHOWS AND DRAWS AS SHOWN: a row whose
    /// switch runs 3pt past the room's edge into the strip is the screen's
    /// peek, live nowhere and fading from its top — the screen's live, full
    /// strength part ends at that switch's top — so the screen that shows it
    /// is the next (Page Down turns onto it, never past it) and nothing on
    /// this one is painted whole over a part a press does not reach. It was
    /// painted at 0.87 of full strength and more, live above the edge only,
    /// and Page Down turned onto it again (Appearance's Selection color at
    /// 102x29). CONTROLS: a card's row of buttons whole in the room stays
    /// live though the card's next row runs past the edge; with nothing cut
    /// the screen is live to the room's edge.
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
        // The row starts at 210, its switch at 222..258: 3pt past the room.
        let screen = plan.screen(0);
        assert_eq!(plan.shown(0), 0..1);
        assert_eq!(
            screen.live, 222.0,
            "live and at full strength to the switch's top"
        );
        assert!(screen.fades);
        assert!(!plan.draws_controls_live(1, 0), "the switch is a picture");
        assert_eq!(plan.page_after(0), 1, "the next screen shows it");
        // A card whose first row of buttons is whole in the room, its next
        // row past the edge: the first row stays live.
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
        // The card starts at 160: its first row 180..224, its second
        // 232..276, past the room.
        assert_eq!(card.screen(0).live, 232.0);
        assert!(card.draws_control_live(1, &UiKey::new("one"), 0));
        assert!(!card.draws_control_live(1, &UiKey::new("two"), 0));
        // The keyboard a turn holds on the stop: the control asked for where
        // it is drawn live, else the first of the stop's that is — a wrapped
        // segmented control's first line, its selected segment past the edge.
        assert_eq!(
            card.live_control(1, &UiKey::new("one"), 0),
            Some(&UiKey::new("one"))
        );
        assert_eq!(
            card.live_control(1, &UiKey::new("two"), 0),
            Some(&UiKey::new("one"))
        );
        assert_eq!(card.live_control(2, &UiKey::new("two"), 0), None);
        // Nothing cut: live to the room's edge.
        let whole = cards(&[100.0, 100.0, 100.0], 230.0, 10.0);
        assert_eq!(whole.screen(0).live, 230.0);
    }

    /// THE PEEK SEAT: a screen that is not the list's first, ending on a
    /// whole card whose next card would show less than [`PEEK_MIN`] under
    /// the edge, scrolls on by what that peek lacks where its first stop's
    /// top padding holds that much — the next card then peeks in its border
    /// and corners, the screen showing what it did — rather than end on the
    /// card over a band of blank with nothing of the next in view (Window at
    /// 150x48 under SESSION, Appearance at 167x36 under TEXT & CONTRAST).
    /// CONTROLS: the list's first screen starts at the page's top; a first
    /// stop with less padding over it than the peek lacks keeps its seat.
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
        // Seated at b: b 0..136, c 148..284, d from 296 — 4pt of it under
        // the edge, 12pt short of its least peek, inside b's 12pt padding.
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
        // CONTROL: the list's first screen starts at the page's top.
        assert_eq!(padded.screen(0).offset, 0.0);
        // CONTROL: 8pt of padding over b holds no 12pt nudge.
        let flush = plan(8.0);
        assert_eq!(flush.screen(1).offset, 0.0);
        assert!(!flush.screen(1).drawn.contains(&3));
    }

    /// A WHOLE ROW BEATS A KEPT CAPTION: a screen scrolled a few points into
    /// a group's card — a press's nudge, the last screen's end — whose first
    /// row the list's top leaves whole draws the card's caption scrolling
    /// with it, not kept over that row: kept, it came down over the row and
    /// cut it, and the edge rule took the row's cut control, name and all
    /// (Cursor & Motion's last screen at 102x29: EFFECT POLICY over an empty
    /// card). CONTROL: scrolled past the row's words, the caption is kept
    /// over the rows scrolling under it.
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
        // Seated at the group's first row, its caption over it; a press
        // nudges the list 16pt: the row's top is 24pt under the list's top.
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
        // CONTROL: 48pt on, the row's words are over the list's top; the
        // caption is kept over the rows under it.
        let past = plan.nudged(seated, 48.0, false);
        assert!(past.caption.is_some_and(|caption| caption.kept), "{past:?}");
    }
}
