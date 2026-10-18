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
//! it is, and the stop after a screen PEEKS in under it, cut at the list's
//! edge — on the last screen the stop before it peeks in over it, cut at the
//! top — so no screen ends on a band of blank and a card an edge cuts reads
//! as going on ([`scroll_list_screen`]). The room a page keeps under its list
//! for a status bar holds that peek while no bar is up ([`drawn_stops`]): the
//! rows a screen shows, and the keyboard, stay above it, so a bar that comes
//! takes only the peek.
//!
//! A group's rows draw inside the group's card. A screen that starts in the
//! middle of a group repeats the group's caption over its first row there.
//!
//! The render measures the page into a [`ScrollPlan`] and records it
//! ([`SettingsViewState::seat_scroll`]); the reducer reads that record to
//! bound the scroll, to move the keyboard between the page's controls, and
//! to adopt where the render put the list ([`SettingsViewState::settle_page_scroll`]).

use std::ops::Range;

use crate::native_ui::{
    AuditedCustomNode, Flow, GroupSpec, Insets, Layout, Length, MESSAGES_SCROLL_AUDIT,
    SemanticRole, StyleRef, UiContent, UiKey, UiNode,
};

use super::{SettingsRoute, SettingsViewState};

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
    keys: Vec<Vec<UiKey>>,
    /// Each stop's rows that hold a control: where the row starts in the
    /// stop, and where its controls start and how tall they are
    /// ([`Self::clear_cut`], [`Self::clear_shown`]).
    control_rows: Vec<Vec<(f32, f32, f32)>>,
    room: f32,
    /// The room under the list kept for a status bar that is not up: the
    /// screen draws its peek on into it ([`drawn_stops`]).
    overhang: f32,
    gap: f32,
}

/// One stretch of a screen: a card stop, or a run of one group's rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ScrollRun {
    Card(usize),
    Group { group: usize, rows: Range<usize> },
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
    pub(super) const fn overhang(&self) -> f32 {
        self.overhang
    }

    pub(super) const fn gap(&self) -> f32 {
        self.gap
    }

    pub(super) fn len(&self) -> usize {
        self.heights.len()
    }

    /// A stop that is a whole card, `height` tall, holding `keys`.
    pub(super) fn card(&mut self, height: f32, keys: Vec<UiKey>) -> usize {
        self.heights.push(height);
        self.groups.push(None);
        self.control_rows.push(if keys.is_empty() {
            Vec::new()
        } else {
            vec![(0.0, 0.0, height)]
        });
        self.keys.push(keys);
        self.heights.len() - 1
    }

    /// A group card: `head` is everything a run of its rows pays besides
    /// the rows (padding and caption), `lead` the part of it above the run's
    /// first row. Its rows follow with [`Self::row`].
    pub(super) fn group(&mut self, head: f32, lead: f32) -> usize {
        self.heads.push(head);
        self.leads.push(lead.max(0.0));
        self.heads.len() - 1
    }

    /// Where stop `index` starts in a run that starts with it: under its
    /// group's lead, or at the top of a card.
    fn lead_of(&self, index: usize) -> f32 {
        self.groups[index].map_or(0.0, |group| self.leads[group])
    }

    /// Say where the controls of the last stop added sit: for each of its
    /// rows that holds one, where the row starts in the stop, and where its
    /// controls start and how tall they are.
    pub(super) fn set_control_rows(&mut self, rows: Vec<(f32, f32, f32)>) {
        if let Some(last) = self.control_rows.last_mut() {
            *last = rows;
        }
    }

    /// Where stop `index`'s first row starts on a screen whose first stop is
    /// `from`: after the runs before it and the gap, under its group's lead
    /// where it starts a run, else right after the stop before it.
    fn stop_top(&self, from: usize, index: usize) -> f32 {
        if index <= from {
            return self.lead_of(index);
        }
        match self.groups[index] {
            Some(group) if self.groups[index - 1] == Some(group) => {
                self.stop_top(from, index - 1) + self.heights[index - 1]
            }
            _ => self.span(from..index) + self.gap + self.lead_of(index),
        }
    }

    /// The most of stop `index` at or under `shown` from its top that leaves
    /// none of its controls' rows nearly whole: a row the list's bottom edge
    /// leaves more than half of is cut through its middle instead, so it
    /// reads as the peek it is ([`Self::clear_cut`] for the top edge).
    fn clear_shown(&self, index: usize, shown: f32) -> f32 {
        self.control_rows[index]
            .iter()
            .find(|(_, top, height)| shown > top + height / 2.0 && shown < top + height - 0.5)
            .map_or(shown, |(_, top, height)| top + height / 2.0)
    }

    /// The least cut at or past `cut` from the top of stop `index` that
    /// leaves none of its controls nearly whole: the top edge takes all of a
    /// row it cuts at all ([`UiNode::peek_above`]), so a row cut above the
    /// middle of its controls is cut through it instead, and reads as the
    /// peek it is, never as a whole row that answers nothing.
    fn clear_cut(&self, index: usize, cut: f32) -> f32 {
        self.control_rows[index]
            .iter()
            .find(|(row, top, height)| cut > row + 0.5 && cut < top + height / 2.0)
            .map_or(cut, |(_, top, height)| top + height / 2.0)
    }

    /// A row of `group`, `height` tall with its share of the card's gaps.
    pub(super) fn row(&mut self, group: usize, height: f32, keys: Vec<UiKey>) -> usize {
        self.heights.push(height);
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

    /// The stops a screen whose first stop is `top` shows: every one from
    /// there that fits whole, and the first whatever its height.
    pub(super) fn view(&self, top: usize) -> Range<usize> {
        let n = self.len();
        if n == 0 {
            return 0..0;
        }
        let top = top.min(n - 1);
        let mut end = top + 1;
        while end < n && self.span(top..end + 1) <= self.room + 0.01 {
            end += 1;
        }
        top..end
    }

    /// The last first stop the list scrolls to: the least whose screen
    /// reaches the last stop, so the last screen is full.
    pub(super) fn limit(&self) -> usize {
        let n = self.len();
        if n == 0 {
            return 0;
        }
        let mut top = n - 1;
        while top > 0 && self.span(top - 1..n) <= self.room + 0.01 {
            top -= 1;
        }
        top
    }

    /// The first stop of the screen that ends just before `top`, so Page Up
    /// turns back over the stops before a screen as Page Down turns over
    /// those after it.
    pub(super) fn page_before(&self, top: usize) -> usize {
        let top = top.min(self.len());
        let mut first = top.saturating_sub(1);
        while first > 0 && self.span(first - 1..top) <= self.room + 0.01 {
            first -= 1;
        }
        first
    }

    /// [`Self::reveal`], a stop whose controls are drawn whole in the room
    /// kept under the list for a status bar that is not up (the screen's
    /// peek) taken as shown.
    pub(super) fn reveal_drawn(&self, index: usize, top: usize) -> usize {
        let top = top.min(self.limit());
        if index >= top && self.whole_by(top, index) <= self.room + self.overhang + 0.01 {
            return top;
        }
        self.reveal(index, top)
    }

    /// How far down a screen whose first stop is `top` stop `index`'s
    /// controls end (where it has none, the stop itself): what a screen
    /// draws to show them whole, its card's padding under them aside.
    fn whole_by(&self, top: usize, index: usize) -> f32 {
        self.stop_top(top, index)
            + self.control_rows[index]
                .iter()
                .map(|(_, at, height)| at + height)
                .reduce(f32::max)
                .unwrap_or(self.heights[index])
    }

    /// The least scroll from `top` whose screen shows stop `index` whole.
    pub(super) fn reveal(&self, index: usize, top: usize) -> usize {
        let limit = self.limit();
        let top = top.min(limit);
        if index < top {
            return index.min(limit);
        }
        if self.view(top).contains(&index) {
            return top;
        }
        let mut first = top + 1;
        while first < index && self.span(first..index + 1) > self.room + 0.01 {
            first += 1;
        }
        first.min(limit)
    }

    /// The stop holding control `key`.
    pub(super) fn stop_of(&self, key: &UiKey) -> Option<usize> {
        self.keys.iter().position(|keys| keys.contains(key))
    }

    /// The page's controls, top to bottom, with their stops.
    pub(super) fn controls(&self) -> impl Iterator<Item = (usize, &UiKey)> {
        self.keys
            .iter()
            .enumerate()
            .flat_map(|(stop, keys)| keys.iter().map(move |key| (stop, key)))
    }
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
/// list. `drawn` is consumed by the next event ([`SettingsViewState::settle_page_scroll`]).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ScrollRecord {
    context: ScrollContext,
    plan: ScrollPlan,
    top: usize,
    drawn: Option<usize>,
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
    pub(super) fn seat_scroll(&self, plan: &ScrollPlan) -> Range<usize> {
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
                && record.plan.view(record.top).contains(&stop))
            .then_some((record.top, stop))
        };
        let first_screen = self
            .scroll_record
            .borrow()
            .as_ref()
            .is_none_or(|record| record.context != self.scroll_context());
        // THE LIST HOLDS ITS PLACE: where stops above the control a person
        // is on came or went since the last screen (a note a choice retired,
        // a row Reset took away), the list moves by as many, so that control
        // stays where it was on screen.
        let anchor = self.page_reveal.as_ref().or(focus);
        let held = anchor.and_then(last_shown);
        if let (Some(key), Some((last_top, last))) = (anchor, held)
            && let Some(index) = plan.stop_of(key)
        {
            top = (last_top + index).saturating_sub(last).min(limit);
        }
        if let Some(index) = self.page_reveal.as_ref().and_then(|key| plan.stop_of(key)) {
            top = if self.landing_reveal {
                plan.reveal_drawn(index, top)
            } else {
                plan.reveal(index, top)
            };
        } else if let Some(key) = focus
            && let Some(index) = plan.stop_of(key)
            // The list's first screen shows the control the keyboard is on;
            // after that, nothing scrolled the list since the last screen,
            // which showed it: a status bar that came, a shorter window, a
            // taller preview keep it in view.
            && (first_screen || held.is_some())
        {
            top = plan.reveal(index, top);
        }
        let view = plan.view(top);
        self.record_result_page_limit(limit);
        self.page_window.set(Some(view.len().max(1)));
        self.scroll_record.replace(Some(ScrollRecord {
            context: self.scroll_context(),
            plan: plan.clone(),
            top,
            drawn: Some(top),
        }));
        view
    }

    /// Adopt where the last render put this page's list, once: the scroll it
    /// clamped to a grown window or a changed layout, or the stop it scrolled
    /// to so a control stays in view.
    pub(super) fn settle_page_scroll(&mut self) {
        let context = self.scroll_context();
        let mut record = self.scroll_record.borrow_mut();
        let Some(record) = record.as_mut().filter(|record| record.context == context) else {
            return;
        };
        if let Some(top) = record.drawn.take() {
            self.page_scroll = top;
        }
        // The landing is drawn: the list holds the place it chose.
        if self.landing_reveal {
            self.landing_reveal = false;
            self.page_reveal = None;
        }
    }

    /// A control the keyboard, a pointer or a commit just moved to stays in
    /// view ([`Self::page_reveal`]) until the list is scrolled — except on a
    /// list not drawn since it was reset, which keeps the reveal its landing
    /// chose ([`Self::reset_page_scroll`]): the focus a navigation lands on
    /// (a sidebar item, the toolbar's button) is never on the list, so it
    /// could only take the saved style's row out of view.
    pub(super) fn reveal_moved_focus(&mut self, before: Option<UiKey>) {
        if self.page_reveal.is_some() && self.scroll_record.borrow().is_none() {
            return;
        }
        if self.common.last_focus != before
            && let Some(key) = self.common.last_focus.clone()
        {
            self.page_reveal = Some(key);
            self.landing_reveal = false;
        }
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

    /// The stops the last render of this page showed.
    pub(super) fn scroll_shown(&self) -> Option<(ScrollPlan, Range<usize>)> {
        let context = self.scroll_context();
        self.scroll_record
            .borrow()
            .as_ref()
            .filter(|record| record.context == context)
            .map(|record| (record.plan.clone(), record.plan.view(record.top)))
    }

    /// Keep `key` in view at the next render.
    pub(super) fn reveal_control(&mut self, key: UiKey) {
        self.page_reveal = Some(key);
        self.landing_reveal = false;
    }

    /// A page opened FOR one of its controls (Settings ▸ Harness's `⚙`,
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §10.3.3 item 8): the
    /// list LANDS on `key`'s stop — a landing's reveal, which the render
    /// follows whether or not one has measured the list yet, and which a
    /// stop already drawn whole leaves where it is — and the keyboard stays
    /// where it was.
    pub(super) fn land_on_control(&mut self, key: UiKey) {
        self.page_reveal = Some(key);
        self.landing_reveal = true;
    }

    /// Forget the list: a new route, search or sheet starts at its top —
    /// except that the Cursor Styles list has a current item, so it opens with
    /// the saved style's row in view, as Down and every cursor style control
    /// land on it. The keyboard stays where it is.
    pub(super) fn reset_page_scroll(&mut self) {
        self.page_scroll = 0;
        self.page_reveal = (self.route == SettingsRoute::CursorStyles
            && self.search.trim().is_empty()
            && !self.compact_navigation)
            .then(|| {
                UiKey::new(super::cursor_style_row_key(&super::saved_cursor_style(
                    self,
                )))
            });
        self.landing_reveal = self.page_reveal.is_some();
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
        let Some(index) = controls.iter().position(|key| **key == current) else {
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
            .view(self.page_scroll.min(plan.limit()))
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
        let shown = plan.view(self.page_scroll.min(plan.limit()));
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

    /// Put the keyboard on list control `key`, ringed and kept in view.
    fn land_list_focus(&mut self, key: UiKey) {
        self.focus_field(Some(&key));
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
    /// Shift-Tab from `current` to the window's next control in Tab order
    /// (`next`) is kept within the list's screen, but where it would leave
    /// the list from one of its controls it moves to the list's next or
    /// previous control instead, and where it enters the list from outside
    /// it starts at the list's first control (Shift-Tab: its last); the list
    /// scrolls to it. `None` keeps `next`.
    pub(crate) fn tab_past_screen(
        &self,
        current: &UiKey,
        next: &UiKey,
        forward: bool,
    ) -> Option<UiKey> {
        let plan = self.scroll_plan()?;
        let controls: Vec<&UiKey> = plan.controls().map(|(_, key)| key).collect();
        let target = match (
            controls.iter().position(|key| *key == current),
            controls.contains(&next),
        ) {
            (Some(index), false) if forward => controls.get(index + 1),
            (Some(index), false) => controls.get(index.checked_sub(1)?),
            (None, true) if forward => controls.first(),
            (None, true) => controls.last(),
            (Some(_), true) | (None, false) => None,
        }?;
        (*target != next).then(|| (*target).clone())
    }
}

/// Whether `node` holds a control at all, an unavailable one too: a disabled
/// slider still belongs to its row, so its words keep with it.
fn holds_control(node: &UiNode) -> bool {
    if node.paint_only {
        return false;
    }
    let control = match &node.content {
        UiContent::Button(_) | UiContent::Switch(_) | UiContent::Slider(_) => true,
        UiContent::TextField(_) => true,
        UiContent::Custom(spec) => spec.focusable,
        _ => false,
    };
    control || node.children.iter().any(holds_control)
}

/// Where the controls `node` holds sit in it, laid out `height` tall as the
/// compiler lays it out: from the top of the first to the bottom of the
/// last, `None` where it holds none. A setting row's control sits under its
/// padding and over its note, so a list edge that cuts the row can be told
/// whether it cuts the control.
fn control_extent(node: &UiNode, height: f32) -> Option<(f32, f32)> {
    if node.paint_only {
        return None;
    }
    let control = match &node.content {
        UiContent::Button(_)
        | UiContent::Switch(_)
        | UiContent::Slider(_)
        | UiContent::TextField(_) => true,
        UiContent::Custom(spec) => spec.focusable,
        _ => false,
    };
    if control {
        return Some((0.0, height));
    }
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
            if let Some((top, height)) = control_extent(child, tall) {
                add(y + top, height);
            }
            y += tall + layout.gap;
        }
    } else {
        for child in &node.children {
            let tall = length(child.layout.height, inner)
                .map_or(inner, |value| value.min(inner))
                .max(0.0);
            if let Some((top, height)) = control_extent(child, tall) {
                add(layout.padding.top + top, height);
            }
        }
    }
    extent
}

/// Where the controls of a stop that is `node`, `height` tall, sit in it
/// ([`ScrollPlan::set_control_rows`]).
pub(super) fn control_rows_in(node: &UiNode, height: f32) -> Vec<(f32, f32, f32)> {
    control_extent(node, height)
        .map(|(top, tall)| (0.0, top, tall))
        .into_iter()
        .collect()
}

/// Every control under `node` a person can focus (or will, once it is done
/// saving), in authoring order.
pub(super) fn focus_keys(node: &UiNode) -> Vec<UiKey> {
    fn walk(node: &UiNode, out: &mut Vec<UiKey>) {
        if node.paint_only {
            return;
        }
        // A control that is busy saving is still the row's control.
        let focusable = match &node.content {
            UiContent::Button(control) => control.state.enabled || control.state.busy,
            UiContent::Switch(control) => control.state.enabled || control.state.busy,
            UiContent::Slider(control) => control.state.enabled || control.state.busy,
            UiContent::TextField(control) => control.state.enabled || control.state.busy,
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

/// The list itself: `children` are the screen's runs, top to bottom. A
/// screen that shows only part of the list carries the scroll thumb over
/// its trailing edge ([`MESSAGES_SCROLL_AUDIT`], paint-only); the list is
/// the screen reader's scrolling surface ([`SemanticRole::List`]).
pub(super) fn scroll_list_node(
    key: &str,
    label: &str,
    plan: &ScrollPlan,
    view: Range<usize>,
    children: Vec<UiNode>,
) -> UiNode {
    let drawn = view.clone();
    scroll_list_screen(key, label, plan, view, drawn, children)
}

/// Give `plan` its `room`, and the `overhang` kept under it for a status bar
/// too where the whole list fits in the two: a page that fits needs no
/// scrolling (when a bar comes, the list keeps the focused control in view).
/// A list that scrolls keeps the overhang for its peek ([`drawn_stops`]).
pub(super) fn seat_room(plan: &mut ScrollPlan, room: f32, overhang: f32) {
    let overhang = overhang.max(0.0);
    if overhang > 0.0 && plan.span(0..plan.len()) <= room + overhang + 0.01 {
        plan.set_room(room + overhang);
        plan.overhang = 0.0;
    } else {
        plan.set_room(room);
        plan.overhang = overhang;
    }
}

/// The stops a screen draws: the ones it shows ([`ScrollPlan::view`]) and,
/// where more follow, the ones after them down to the first the list's edge
/// cuts, so the screen always ends on a cut, never on a card that looks
/// complete. The last screen ends on the last stop, so it draws the stops
/// before it instead, up to the first the list's top edge cuts: it is as
/// full as every other screen ([`scroll_list_screen`]).
///
/// While no status bar is up, the room a page keeps under its list for one
/// ([`super::settings_list_overhang`]) holds the peek too, so no screen ends
/// on a band of blank: the peek is drawn down to the page's bottom. The
/// stops a screen shows, and so the scroll and where the keyboard is seated,
/// stay in the room above it, so a bar that comes takes only that peek and
/// the rows a screen shows whole never change for it. The last screen has
/// nothing after its last stop, so it draws none there.
pub(super) fn drawn_stops(plan: &ScrollPlan, view: &Range<usize>) -> Range<usize> {
    let n = plan.len();
    if view.start == 0 && view.end >= n {
        return view.clone();
    }
    let height = screen_height(plan, view);
    if view.end >= n {
        let mut start = view.start;
        while start > 0 && plan.span(start..view.end) + 1.0 < height {
            start -= 1;
        }
        return start..view.end;
    }
    let reach = drawn_height(plan, view);
    let mut end = view.end;
    while end < n && plan.span(view.start..end) <= reach + 0.01 {
        end += 1;
    }
    view.start..end
}

/// How tall a screen's list is: a whole list its stops; a scrolled one its
/// room (one stop taller than the room is seated whole and the page clips
/// it).
fn screen_height(plan: &ScrollPlan, view: &Range<usize>) -> f32 {
    if view.start == 0 && view.end >= plan.len() {
        return plan.span(view.clone());
    }
    plan.room().max(plan.span(view.start..view.start + 1))
}

/// How tall a screen draws: its room ([`screen_height`]), and where stops
/// follow it, the room kept under it for a status bar that is not up, which
/// holds its peek ([`drawn_stops`]) — or a first stop taller than the room
/// takes from it.
fn drawn_height(plan: &ScrollPlan, view: &Range<usize>) -> f32 {
    let height = screen_height(plan, view);
    if view.end >= plan.len() {
        return height;
    }
    (plan.room() + plan.overhang()).max(height)
}

/// [`scroll_list_node`] for a screen that draws its PEEK ([`drawn_stops`]):
/// the stops after its last whole stop, cut at the list's edge, or on the
/// last screen the stops before its first, cut at the list's top, so the
/// reader sees the list goes on and no screen ends on a band of blank. The
/// caller marks them ([`UiNode::peek`], and [`UiNode::cut`] on a card an
/// edge cuts). The stops a screen shows, and so the scroll, never change
/// for them.
pub(super) fn scroll_list_screen(
    key: &str,
    label: &str,
    plan: &ScrollPlan,
    view: Range<usize>,
    drawn: Range<usize>,
    children: Vec<UiNode>,
) -> UiNode {
    let whole = view.start == 0 && view.end >= plan.len();
    let room = screen_height(plan, &view);
    // A screen with stops after it draws its peek on into the room kept for
    // a status bar that is not up ([`drawn_stops`]) — down to the page's
    // bottom, or where that would leave a row of the stop it cuts nearly
    // whole, only to that row's middle: a peek is dead to the pointer and the
    // keyboard, so it must read as cut, never as a whole row that answers
    // nothing.
    let mut height = drawn_height(plan, &view);
    if !whole && drawn.end > view.end && drawn.start == view.start {
        let last = drawn.end - 1;
        let top = plan.stop_top(view.start, last);
        let shown = height - top;
        let clear = plan.clear_shown(last, shown);
        if clear < shown {
            height = top + clear;
        }
    }
    // The last screen's leading peek: the list is scrolled so its last stop
    // ends at the list's bottom edge — or, where that would cut a row of the
    // stop over it by less than half, a little further, so that row is cut
    // through its middle: a peek over the top edge is dead to the pointer
    // and the keyboard, and a row cut only in its padding would look whole
    // and answer nothing. The last stop then ends that little above the edge
    // (half a control at most: a setting row's few points, a Themes gallery
    // card's 190pt row up to 95), whether or not a bar is up. A target is
    // never drawn cut and live, so the stop over the edge cannot show whole
    // instead: the last stop would go past the edge.
    let offset = if !whole && drawn.start < view.start {
        let offset = (plan.span(drawn.start..view.end) - room).max(0.0);
        let lead = plan.lead_of(drawn.start);
        let cut = offset - lead;
        let clear = plan.clear_cut(drawn.start, cut);
        if cut > 0.0 && clear > cut {
            lead + clear
        } else {
            offset
        }
    } else {
        0.0
    };
    let fills = !whole && (drawn.end > view.end || drawn.start < view.start);
    // The thumb's track is what the screen draws: the whole list where a
    // peek fills it, else down to its last card, never past it onto the
    // backdrop.
    let track = if fills {
        height
    } else {
        plan.span(view.clone()).min(room)
    };
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
            .height(Length::Fixed(height))
            .gap(plan.gap())
            .scrolled(offset)
            .clipped(),
    )
    .children(children);
    if whole {
        return list;
    }
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
            .height(Length::Fixed(height)),
    )
    .children(vec![list, thumb])
}

/// A card the list lays out row by row: its head (a heading) leads every run
/// of its rows a screen shows, so each screen still reads as that card, and
/// a screen ends between two rows, never inside one.
pub(super) struct SplitCard {
    pub(super) key: String,
    pub(super) spec: GroupSpec,
    pub(super) padding: Insets,
    pub(super) gap: f32,
    pub(super) head: Vec<(UiNode, f32)>,
    pub(super) rows: Vec<(UiNode, f32)>,
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
        let leads = rows.len() > 1
            && matches!(&rows[0].0.content, UiContent::Text(text) if text.role == SemanticRole::Heading);
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
    /// screen it is the stop right after that row, never the next control's;
    /// and trailing rows with no control keep with the row before (a caption
    /// with the switches it explains) — each only while the stop still fits
    /// `room` under the card's head. A card of words alone (About's build
    /// facts) is a stop per row, so a screen holds as many of them as fit.
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
        let mut stops: Vec<Range<usize>> = Vec::new();
        for row in 0..self.rows.len() {
            match stops.last_mut() {
                Some(stop) if under_control(row) && height(stop.start..row + 1) <= room + 0.01 => {
                    stop.end = row + 1;
                }
                Some(stop)
                    if !has_control(stop)
                        && !under_control(stop.start)
                        && controls[row..].iter().any(|control| *control)
                        && height(stop.start..row + 1) <= room + 0.01 =>
                {
                    stop.end = row + 1;
                }
                _ => stops.push(row..row + 1),
            }
        }
        if let [.., before, last] = stops.as_slice()
            && !has_control(last)
            && height(before.start..last.end) <= room + 0.01
        {
            let end = last.end;
            stops.pop();
            if let Some(before) = stops.last_mut() {
                before.end = end;
            }
        }
        stops
    }

    /// The card holding `rows`, its head over them, the rows outside
    /// `shown` a list's peek ([`scroll_list_screen`]): those after it, cut at
    /// the list's bottom edge ([`UiNode::peek`]), or those before it, cut at
    /// its top with the head above them ([`UiNode::peek_above`]). `shown`
    /// empty at `rows.end`: the whole run is over the top.
    pub(super) fn node_peeking(&self, rows: Range<usize>, shown: Range<usize>) -> UiNode {
        let before = shown.start > rows.start;
        let cut = before || rows.end > shown.end;
        let mut card = self.node(rows.clone());
        let head = self.head.len();
        for (index, child) in card.children.iter_mut().enumerate() {
            let row = (index >= head).then(|| rows.start + index - head);
            *child = match row {
                None if before => child.clone().peek_above(),
                Some(row) if row < shown.start => child.clone().peek_above(),
                Some(row) if row >= shown.end => child.clone().peek(),
                _ => continue,
            };
        }
        if cut { card.cut() } else { card }
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

/// One item of a [`list_page`]: a card that is always whole, or one listed
/// row by row.
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
                plan.set_control_rows(control_rows_in(node, *height));
                stops.push((index, None));
            }
            ListItem::Split(card) if card.rows.is_empty() => {
                plan.card(card.height(), focus_keys(&card.node(0..card.rows.len())));
                stops.push((index, None));
            }
            ListItem::Split(card) => {
                let group = plan.group(card.height_of_head(), card.height_of_lead());
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
    let (plan, _) = plan_items(items, gap, room);
    (0..plan.len())
        .map(|stop| plan.span(stop..stop + 1))
        .fold(0.0, f32::max)
}

/// The height the first `stops` stops of `items` take on screen together, in
/// a list at most `room` tall.
pub(super) fn leading_span(items: &[ListItem], gap: f32, room: f32, stops: usize) -> f32 {
    let (plan, _) = plan_items(items, gap, room);
    plan.span(0..stops.min(plan.len()))
}

/// How tall a screen must draw to show the first `stops` stops of `items`
/// with their controls whole, in a list at most `room` tall: as
/// [`leading_span`], less the padding under the last one's controls.
pub(super) fn leading_whole(items: &[ListItem], gap: f32, room: f32, stops: usize) -> f32 {
    let (plan, _) = plan_items(items, gap, room);
    match stops.min(plan.len()) {
        0 => 0.0,
        stops => plan.whole_by(0, stops - 1),
    }
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
    list_page_peeking(state, key, label, items, room, gap, 0.0)
}

/// [`list_page`] whose screens draw their peek ([`scroll_list_screen`]); a
/// list that fits whole in its room and the `overhang` the page keeps under
/// it for a status bar takes both and does not scroll ([`seat_room`]).
pub(super) fn list_page_peeking(
    state: &SettingsViewState,
    key: &str,
    label: &str,
    items: Vec<ListItem>,
    room: f32,
    gap: f32,
    overhang: f32,
) -> UiNode {
    let (mut plan, stops) = plan_items(&items, gap, room);
    seat_room(&mut plan, room, overhang);
    let view = state.seat_scroll(&plan);
    let drawn = drawn_stops(&plan, &view);
    let children: Vec<UiNode> = plan
        .runs(drawn.clone())
        .into_iter()
        .filter_map(|run| match run {
            ScrollRun::Card(stop) => {
                let node = match &items[stops[stop].0] {
                    ListItem::Card(node, _) => (**node).clone(),
                    ListItem::Split(card) => card.node(0..card.rows.len()),
                };
                Some(if stop < view.start {
                    node.peek_above()
                } else if stop >= view.end {
                    node.peek()
                } else {
                    node
                })
            }
            ScrollRun::Group { rows, .. } => {
                let ListItem::Split(card) = &items[stops[rows.start].0] else {
                    return None;
                };
                let first = stops[rows.start].1.as_ref()?.start;
                let end = stops[rows.end - 1].1.as_ref()?.end;
                // The card's rows outside the screen's stops are its peek.
                if rows.start >= view.end {
                    return Some(card.node(first..end).peek());
                }
                let shown = rows.start.max(view.start)..rows.end.min(view.end);
                let shown_rows = if shown.is_empty() {
                    end..end
                } else {
                    stops[shown.start].1.as_ref()?.start..stops[shown.end - 1].1.as_ref()?.end
                };
                Some(card.node_peeking(first..end, shown_rows))
            }
        })
        .collect();
    scroll_list_screen(key, label, &plan, view, drawn, children)
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
        assert_eq!(card.stops(1_000.0), vec![0..2, 2..4]);
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

    fn words(key: &str) -> UiNode {
        UiNode::new(
            key,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
    }

    /// A card of words alone is a stop per row (only its last keeps with
    /// the one before), so a screen holds as many as fit; an unavailable
    /// control still keeps its words with it.
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
        assert_eq!(card.stops(1_000.0), vec![0..1, 1..2, 2..4]);

        let mut slider = Control::new(
            crate::native_ui::SliderSpec {
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

    /// The stops after a screen are drawn down to the first the list's edge
    /// cuts, and none past its room: the room a page keeps under the list for
    /// a status bar holds no row a bar could take away.
    #[test]
    fn a_screen_draws_on_to_the_first_stop_its_edge_cuts() {
        let plan = cards(&[40.0, 40.0, 40.0, 40.0, 40.0], 100.0, 10.0);
        let view = plan.view(0);
        assert_eq!(view, 0..2);
        assert_eq!(drawn_stops(&plan, &view), 0..3, "the next stop peeks");
        let node = scroll_list_screen("list", "List", &plan, view.clone(), 0..3, Vec::new());
        assert_eq!(
            node.layout.height,
            Length::Fixed(plan.room()),
            "the peek is cut at the list's room"
        );
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
