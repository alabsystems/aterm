// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Themes page: theme chooser, live demo stage, action strip, and Customize sheet.

use super::*;
use crate::control_theme::ThemeTrialIntent;
use crate::settings_preview::theme_card::{AttentionRow, Section, ShowcaseCard, showcase_cards};

/// The Customize sheet: colour rows, the palette and Save as theme.
#[path = "native_settings_showcase_customize.rs"]
mod customize;
/// The page's layout: the chooser, the demo and the strip.
#[path = "native_settings_showcase_view.rs"]
mod view;

#[cfg(test)]
pub(super) use view::SHEET_TITLE;
pub(super) use view::{
    BlocksShared, ListRoom, SharedBlocks, SharedDemo, SharedSheet, SharedStrip,
    demo_font_candidate, demo_spec, laid_last, list_laid, list_room, themes_page,
};
#[cfg(test)]
pub(super) use view::{list_blocks, section_items};

/// Every action the page mints starts with this.
pub(super) const SHOWCASE_ACTION_PREFIX: &str = "settings/showcase/";
/// The chooser's filter field (its node key and its action).
pub(super) const SHOWCASE_FILTER: &str = "settings/showcase/filter";
const FILTER_CLEAR: &str = "settings/showcase/filter-clear";
/// The Save as theme name field.
pub(super) const SHOWCASE_SAVE_NAME: &str = "settings/showcase/save-name";
/// The palette slot editor's field.
pub(super) const SHOWCASE_PALETTE_SLOT: &str = "settings/showcase/palette-slot";
/// Keep the tried theme.
pub(super) const SHOWCASE_KEEP: &str = "settings/showcase/keep";
/// Revert the tried theme.
pub(super) const SHOWCASE_REVERT: &str = "settings/showcase/revert";
/// Keep the tried theme's colours and pin the current type settings.
pub(super) const SHOWCASE_KEEP_COLOURS: &str = "settings/showcase/keep-colours";
/// Save as theme's "Include type settings" checkbox.
pub(super) const SHOWCASE_SAVE_TYPE: &str = "settings/showcase/save-type";
/// Open the Save as theme row.
pub(super) const SHOWCASE_SAVE_AS: &str = "settings/showcase/save-as";
/// The chooser's list of themes (a list box).
pub(super) const SHOWCASE_LIST: &str = "settings/showcase/list";
const CARD_PREFIX: &str = "settings/showcase/card/";
const KEEP_MORE: &str = "settings/showcase/keep-more";
const CUSTOMIZE: &str = "settings/showcase/customize";
const CUSTOMIZE_DONE: &str = "settings/showcase/customize-done";
const PACK: &str = "settings/showcase/pack";
const FOLLOW_SYSTEM: &str = "settings/showcase/follow-system";
const SIDE_PREFIX: &str = "settings/showcase/side/";
const SAVE_CONFIRM: &str = "settings/showcase/save";
const SAVE_CANCEL: &str = "settings/showcase/save-cancel";
const IMPORT: &str = "settings/showcase/import";
const OPEN_FOLDER: &str = "settings/showcase/open-folder";
const PALETTE_PREFIX: &str = "settings/showcase/palette/";
const PALETTE_SLOT_RESET: &str = "settings/showcase/palette-slot-reset";
const PALETTE_SLOT_CLOSE: &str = "settings/showcase/palette-slot-close";
const PALETTE_RESET_ALL: &str = "settings/showcase/palette-reset";
const CARD_OPEN: &str = "open";
const CARD_TRASH: &str = "trash";

const SHOWCASE_TITLE: &str = "Themes";
/// Trailing gutter reserved for the scroll thumb.
pub(super) const THUMB_GUTTER: f32 = 12.0;

/// Color override keys edited by the Customize sheet.
pub(super) const SHOWCASE_COLOR_KEYS: [&str; 5] = [
    prefs::EDIT_FOREGROUND,
    prefs::EDIT_BACKGROUND,
    prefs::EDIT_CURSOR_COLOR,
    prefs::EDIT_SELECTION_COLOR,
    prefs::EDIT_SELECTION_FOREGROUND,
];

/// Where the page's trial stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ShowcaseTrial {
    /// Nothing is tried: the terminal draws the configured theme.
    #[default]
    Idle,
    /// `id` is previewed live without writing config.
    Trying { id: String },
    /// Keep wrote `theme` for `id` and is waiting for the config patch to settle.
    Keeping {
        id: String,
        /// The newest of the trial's Keeps on its way.
        operation: OperationId,
        /// Prior values of keys written by this trial's Keep operations, for Revert.
        under: BTreeMap<String, Option<String>>,
    },
}

/// The open Save as theme row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SaveAsState {
    pub(crate) name: crate::native_text_input::TextInputState,
    /// Validation or write error for the save name.
    pub(crate) error: Option<String>,
    /// Whether the saved theme file includes Customize typography settings.
    pub(crate) include_type: bool,
}

/// Typography config keys included when [`SaveAsState::include_type`] is enabled.
pub(super) const SAVE_TYPE_KEYS: [&str; 5] = [
    prefs::EDIT_FONT_FAMILY,
    prefs::EDIT_LINE_HEIGHT,
    prefs::EDIT_LIGATURES,
    prefs::EDIT_FONT_WEIGHT,
    prefs::EDIT_FONT_SIZE_SCALE,
];

/// Unsaved drafts carried into a newly saved theme and cleared once it is selected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CarriedDrafts {
    fields: BTreeMap<String, crate::native_text_input::TextInputState>,
    slot: Option<(usize, crate::native_text_input::TextInputState)>,
}

/// Saved theme waiting for file publication and catalog discovery before selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingSelect {
    id: String,
    operation: OperationId,
    saved: bool,
    theme: ConfigEdit,
    side: Option<aterm_types::Appearance>,
    clears: Vec<ConfigEdit>,
    drafts: CarriedDrafts,
}

/// The palette slot being edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SlotEditor {
    pub(crate) index: usize,
    pub(crate) input: crate::native_text_input::TextInputState,
    opened: aterm_types::Rgb,
}

/// Community pack theme waiting for its file write and catalog listing before Keep writes `theme`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingKeep {
    pub(crate) operation: OperationId,
    pub(crate) id: String,
    pub(crate) written: bool,
}

/// Strip status message and the theme id it describes after a trial settles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settled {
    pub(crate) id: String,
    pub(crate) words: String,
}

/// A measured [`ShowcaseState::name_floors`] entry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct NameFloor {
    pub(super) cards: usize,
    pub(super) scale: u32,
    pub(super) heading: bool,
    pub(super) floor: f32,
}

/// A measured [`ShowcaseState::strip_names`] entry: for which rows (`cards`,
/// by address), at which text scale, and in which title rooms (beside a
/// trial's buttons without Keep ▾ and with it, their bits, and whether the
/// theme a trial replaces sets type, so every trial shows Keep ▾).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct StripNames {
    pub(super) cards: usize,
    pub(super) scale: u32,
    pub(super) rooms: (u32, u32, bool),
    /// The most lines a name takes in its room.
    pub(super) lines: usize,
    /// Whether a word of a name is wider than its room.
    pub(super) broken: bool,
}

/// Settings ▸ Themes' view state.
#[derive(Debug)]
pub(crate) struct ShowcaseState {
    pub(crate) filter: crate::native_text_input::TextInputState,
    /// The Customize sheet stands in the chooser's place.
    pub(crate) sheet_open: bool,
    /// Whether the community pack disclosure is expanded.
    pub(crate) pack_open: bool,
    /// The Keep split button's one-item list (Keep colours only) is open.
    pub(crate) keep_more: bool,
    /// The chooser's rows, built from the catalog this view admitted.
    pub(crate) cards: Arc<[ShowcaseCard]>,
    /// Theme files that failed to load.
    pub(crate) attention: Arc<[AttentionRow]>,
    pub(crate) trial: ShowcaseTrial,
    /// Appearance side of the last highlighted theme when following system appearance.
    pub(crate) highlight_side: Option<aterm_types::Appearance>,
    /// The palette slot being edited.
    pub(crate) slot: Option<SlotEditor>,
    pub(crate) save: Option<SaveAsState>,
    pub(crate) pending_select: Option<PendingSelect>,
    /// A community theme's Keep, waiting for its file and the catalog.
    pub(crate) pending_keep: Option<PendingKeep>,
    selecting: Option<(OperationId, String, CarriedDrafts)>,
    /// The Yours row whose Trash was pressed once: a second press moves it.
    pub(crate) trash_armed: Option<String>,
    /// Error from the last Keep or community pack add.
    pub(crate) error: Option<String>,
    /// Last settled strip announcement ([`Settled`]).
    pub(crate) settled: Option<Settled>,
    order: RefCell<Vec<String>>,
    page_rows: Cell<usize>,
    /// Cached minimum width measurements for theme names ([`view::name_floor`]).
    pub(super) name_floors: RefCell<Vec<NameFloor>>,
    /// The strip's names, as last measured ([`view::strip_names`]): a frame
    /// measures every name once, not once a layout.
    pub(super) strip_names: RefCell<Vec<StripNames>>,
    /// Animation clock epoch when the page opened.
    pub(crate) demo_epoch_ms: Cell<Option<u64>>,
    /// Animation phase at the last user input on the page.
    pub(crate) last_input_phase_ms: Cell<u64>,
    /// Whether user input occurred since the last render.
    pub(crate) input_pending: Cell<bool>,
    /// Whether Escape requested a revert of an in-flight Keep.
    pub(crate) revert_asked: bool,
}

impl ShowcaseState {
    pub(super) fn new(themes: &crate::app_config::ThemeCatalog) -> Self {
        let cards: Arc<[ShowcaseCard]> = showcase_cards(themes).into();
        Self {
            filter: crate::native_text_input::TextInputState::new(String::new()),
            sheet_open: false,
            pack_open: false,
            keep_more: false,
            attention: crate::settings_preview::theme_card::attention_rows(themes, &cards).into(),
            cards,
            trial: ShowcaseTrial::Idle,
            highlight_side: None,
            slot: None,
            save: None,
            pending_select: None,
            pending_keep: None,
            selecting: None,
            trash_armed: None,
            error: None,
            settled: None,
            order: RefCell::new(Vec::new()),
            page_rows: Cell::new(8),
            name_floors: RefCell::new(Vec::new()),
            strip_names: RefCell::new(Vec::new()),
            demo_epoch_ms: Cell::new(None),
            last_input_phase_ms: Cell::new(0),
            input_pending: Cell::new(true),
            revert_asked: false,
        }
    }

    /// The filter's query, trimmed and lowercased.
    pub(crate) fn query(&self) -> String {
        self.filter.value().trim().to_lowercase()
    }

    /// Indices into `cards` admitted by the current filter and disclosure state.
    pub(crate) fn visible(&self) -> Vec<usize> {
        self.visible_for(&self.query())
    }

    /// Indices into `cards` admitted by `query` (trimmed, lowercase).
    pub(crate) fn visible_for(&self, query: &str) -> Vec<usize> {
        let reach = self.reach_for(query);
        self.cards
            .iter()
            .enumerate()
            .filter(|(_, card)| {
                (card.section != Section::Community || self.pack_open || !query.is_empty())
                    && card.matches(query, reach)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Match reach ([`MatchReach`]) for `query` across attention rows and cards.
    pub(crate) fn reach_for(&self, query: &str) -> MatchReach {
        MatchReach::of(
            self.attention
                .iter()
                .any(|row| attention_row_matches(row, query, MatchReach::Words))
                || self
                    .cards
                    .iter()
                    .any(|card| card.matches(query, MatchReach::Words)),
        )
    }

    /// The rows the last render listed, in order.
    #[cfg(test)]
    pub(crate) fn listed(&self) -> Vec<String> {
        self.order.borrow().clone()
    }

    fn card(&self, id: &str) -> Option<&ShowcaseCard> {
        self.cards
            .iter()
            .find(|card| card.id.eq_ignore_ascii_case(id))
    }

    pub(super) fn card_by_slug(&self, slug: &str) -> Option<&ShowcaseCard> {
        self.cards.iter().find(|card| card.slug == slug)
    }

    fn trial_id(&self) -> Option<&str> {
        match &self.trial {
            ShowcaseTrial::Idle => None,
            ShowcaseTrial::Trying { id } | ShowcaseTrial::Keeping { id, .. } => Some(id),
        }
    }

    /// The label a theme id reads by: its row's name, else the id.
    pub(super) fn label_of(&self, id: &str) -> String {
        self.card(id)
            .map_or_else(|| id.to_string(), |card| card.label.clone())
    }

    /// Something happened on the page: the demo plays again.
    pub(crate) fn note_input(&self) {
        self.input_pending.set(true);
    }
}

/// Whether `row` matches `query` under `reach`.
pub(crate) fn attention_row_matches(row: &AttentionRow, query: &str, reach: MatchReach) -> bool {
    query.is_empty()
        || crate::native_config_language::candidate_match_score(
            &row.id.to_lowercase(),
            query,
            reach,
        )
        .is_some()
}

/// Which of the page's own text fields the keyboard is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShowcaseText {
    Filter,
    SaveName,
    PaletteSlot,
}

impl ShowcaseText {
    pub(super) fn for_key(key: &str) -> Option<Self> {
        match key {
            SHOWCASE_FILTER => Some(Self::Filter),
            SHOWCASE_SAVE_NAME => Some(Self::SaveName),
            SHOWCASE_PALETTE_SLOT => Some(Self::PaletteSlot),
            _ => None,
        }
    }
}

impl SettingsViewState {
    /// Whether the Themes page is visible without an active search or compact category overlay.
    pub(super) fn showcase_on_glass(&self) -> bool {
        self.route == SettingsRoute::Themes
            && self.search.trim().is_empty()
            && !self.compact_navigation
    }

    /// Appearance side of the highlighted theme when following system with no manual side chosen.
    pub(super) fn showcase_highlight_side(&self) -> Option<aterm_types::Appearance> {
        let following = prefs::theme_mode(self.raw_value(prefs::EDIT_THEME).as_deref())
            == prefs::ThemeMode::FollowSystem;
        (self.showcase_on_glass() && following)
            .then_some(self.showcase.highlight_side)
            .flatten()
    }

    fn theme_made_for(&self, id: &str) -> Option<aterm_types::Appearance> {
        self.assets
            .themes
            .meta(id.trim())
            .map(|meta| meta.appearance)
            .or_else(|| self.showcase.card(id.trim()).map(|card| card.appearance))
    }

    /// Active theme preview or in-flight Keep intent for the host.
    pub(crate) fn theme_trial_intent(&self) -> Option<ThemeTrialIntent> {
        if self.route != SettingsRoute::Themes {
            return None;
        }
        if !self.showcase_on_glass() && matches!(self.showcase.trial, ShowcaseTrial::Trying { .. })
        {
            return None;
        }
        match &self.showcase.trial {
            ShowcaseTrial::Idle => None,
            ShowcaseTrial::Trying { id } => Some(ThemeTrialIntent::Show(id.clone())),
            ShowcaseTrial::Keeping { id, .. } => Some(ThemeTrialIntent::Keeping(id.clone())),
        }
    }

    /// End the trial of `id` and record `why` when the host cannot preview it.
    pub(crate) fn refuse_theme_trial(&mut self, id: &str, why: &str) {
        if self
            .showcase
            .trial_id()
            .is_some_and(|tried| tried.eq_ignore_ascii_case(id))
        {
            self.showcase.trial = ShowcaseTrial::Idle;
            self.showcase.highlight_side = None;
            self.feedback = Some(format!("Can\u{2019}t try {id}: {why}"));
            self.common.presentation_revision = self.common.presentation_revision.saturating_add(1);
        }
    }

    /// Rebuild chooser cards from the catalog and end any trial whose theme is no longer valid.
    pub(super) fn refresh_showcase(&mut self) {
        let cards: Arc<[ShowcaseCard]> = showcase_cards(&self.assets.themes).into();
        self.showcase.attention =
            crate::settings_preview::theme_card::attention_rows(&self.assets.themes, &cards).into();
        self.showcase.cards = cards;
        self.showcase.name_floors.borrow_mut().clear();
        self.showcase.strip_names.borrow_mut().clear();
        self.showcase.trash_armed = None;
        self.reseat_focus_off_a_gone_row();
        if let Some(id) = self.showcase.trial_id().map(str::to_string)
            && let Err(reason) = self.assets.themes.resolve(&id)
            && self.assets.themes.pack_fallback(&id).is_none()
        {
            self.showcase.trial = ShowcaseTrial::Idle;
            self.showcase.highlight_side = None;
            self.showcase.error = Some(if self.assets.themes.lists(&id) {
                reason
            } else {
                format!("{id} is no longer in the themes folder")
            });
        }
    }

    fn reseat_focus_off_a_gone_row(&mut self) {
        if self.route != SettingsRoute::Themes || self.showcase.sheet_open {
            return;
        }
        let Some(rest) = self
            .common
            .last_focus
            .as_ref()
            .and_then(|key| key.as_str().strip_prefix(CARD_PREFIX))
        else {
            return;
        };
        let (slug, action) = match rest.split_once('/') {
            Some((slug, action)) => (slug.to_string(), Some(action.to_string())),
            None => (rest.to_string(), None),
        };
        let visible: Vec<usize> = self.showcase.visible();
        let listed = |slug: &str| {
            visible
                .iter()
                .any(|&index| self.showcase.cards[index].slug == slug)
        };
        let still_there = match action.as_deref() {
            Some(_) => {
                visible.iter().any(|&index| {
                    let card = &self.showcase.cards[index];
                    card.slug == slug && card.section == Section::Yours
                }) || self.showcase.attention.iter().any(|row| row.slug == slug)
            }
            None => listed(&slug),
        };
        if still_there {
            return;
        }
        let order = self.showcase.order.borrow().clone();
        let target = order
            .iter()
            .position(|row| *row == slug)
            .and_then(|at| {
                order[at + 1..]
                    .iter()
                    .find(|row| listed(row))
                    .or_else(|| order[..at].iter().rev().find(|row| listed(row)))
                    .cloned()
            })
            .or_else(|| self.highlighted_slug().filter(|row| listed(row)));
        let key = target.map_or_else(
            || UiKey::new(SHOWCASE_FILTER),
            |row| UiKey::new(format!("{CARD_PREFIX}{row}")),
        );
        self.common.last_focus = Some(key.clone());
        self.reveal_control(key);
    }

    /// Currently highlighted slug before a config or catalog update.
    pub(super) fn showcase_highlight_before_change(&self) -> Option<String> {
        (self.route == SettingsRoute::Themes)
            .then(|| self.highlighted_slug())
            .flatten()
    }

    /// Reconcile settled strip text and keyboard focus after a non-trial config or catalog change.
    pub(super) fn follow_showcase_change(&mut self, highlighted_before: Option<String>) {
        if self
            .showcase
            .settled
            .as_ref()
            .is_some_and(|settled| !settled.id.eq_ignore_ascii_case(&self.configured_theme_id()))
        {
            self.showcase.settled = None;
        }
        if self.route != SettingsRoute::Themes || self.showcase.trial != ShowcaseTrial::Idle {
            return;
        }
        let (Some(before), Some(now)) = (highlighted_before, self.highlighted_slug()) else {
            return;
        };
        if now != before && self.showcase_focused_card().as_deref() == Some(before.as_str()) {
            let key = UiKey::new(format!("{CARD_PREFIX}{now}"));
            self.common.last_focus = Some(key.clone());
            self.reveal_control(key);
        }
    }

    /// End any active trial and clear transient page state when leaving the Themes page.
    pub(super) fn leave_showcase(&mut self) {
        self.showcase.trial = ShowcaseTrial::Idle;
        self.showcase.highlight_side = None;
        self.showcase.slot = None;
        self.showcase.save = None;
        self.showcase.sheet_open = false;
        self.showcase.keep_more = false;
        self.showcase.pending_select = None;
        self.showcase.pending_keep = None;
        self.showcase.trash_armed = None;
        self.showcase.error = None;
        self.showcase.settled = None;
        self.showcase.demo_epoch_ms.set(None);
        self.showcase.note_input();
    }

    /// End the active trial and return the highlight to the configured theme.
    pub(super) fn revert_showcase(&mut self) {
        self.showcase.pending_keep = None;
        self.showcase.keep_more = false;
        if matches!(self.showcase.trial, ShowcaseTrial::Trying { .. }) {
            self.showcase.trial = ShowcaseTrial::Idle;
            self.showcase.highlight_side = None;
            self.showcase.error = None;
            let id = self.configured_theme_id();
            let words = format!("Reverted to {}", self.showcase.label_of(&id));
            self.showcase.settled = Some(Settled { id, words });
            self.common.last_focus = self
                .highlighted_slug()
                .map(|slug| UiKey::new(format!("{CARD_PREFIX}{slug}")))
                .or_else(|| self.common.last_focus.take());
        }
    }

    /// Handle Escape on the Themes page, dismissing the innermost active transient state.
    pub(super) fn cancel_showcase(&mut self) -> bool {
        if !self.showcase_on_glass() {
            return false;
        }
        if self.showcase.trash_armed.take().is_some() {
            self.feedback = None;
            return true;
        }
        if self.showcase.keep_more {
            self.showcase.keep_more = false;
            self.common.last_focus = Some(UiKey::new(KEEP_MORE));
            return true;
        }
        if let Some(slot) = self.showcase.slot.take() {
            self.common.last_focus = Some(UiKey::new(format!("{PALETTE_PREFIX}{}", slot.index)));
            return true;
        }
        if self.showcase.save.take().is_some() {
            self.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_AS));
            return true;
        }
        if self.showcase.sheet_open {
            self.close_showcase_sheet();
            return true;
        }
        if !self.showcase.filter.value().is_empty()
            && self.showcase_text_focus() == Some(ShowcaseText::Filter)
        {
            self.showcase.filter = crate::native_text_input::TextInputState::new(String::new());
            self.invalidate_result_page_limit();
            self.scroll_to_results();
            return true;
        }
        if self.showcase.pending_select.take().is_some() {
            self.feedback = Some("The saved theme will not be selected automatically".to_string());
            return true;
        }
        if matches!(self.showcase.trial, ShowcaseTrial::Trying { .. }) {
            self.revert_showcase();
            return true;
        }
        if matches!(self.showcase.trial, ShowcaseTrial::Keeping { .. }) {
            self.showcase.revert_asked = true;
            return true;
        }
        false
    }

    /// Close the Customize sheet and return focus to the Customize button.
    pub(super) fn close_showcase_sheet(&mut self) {
        self.showcase.sheet_open = false;
        self.showcase.slot = None;
        self.showcase.save = None;
        self.invalidate_result_page_limit();
        self.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
        if let Some(row) = self.showcase_landing() {
            self.land_on_control(row);
        }
        self.common.last_focus = Some(UiKey::new(CUSTOMIZE));
    }

    /// A FILTER CHANGE SHOWS THE FILTER AND THEN ITS FIRST RESULTS (R5 of
    /// [`Self::seat_scroll`]): where the list lists the filter, the list
    /// stays where it shows the filter whole, else goes to the filter's
    /// stop, and reveals the first result, the filter kept in view as the
    /// keyboard's control; else the list from its top. Sent to the top, a
    /// letter typed in the filter at 178x348 showed the demo and the strip,
    /// the results off the screen. The page's chrome is chosen again for the
    /// new results: held for the short list, Erase at 380x300 brought the
    /// whole list back 16pt shorter and its first result off the screen.
    pub(super) fn scroll_to_results(&mut self) {
        let filter = UiKey::new(SHOWCASE_FILTER);
        let shown = self.keyboard_shown(true).is_some_and(|key| key == filter);
        let stop = self.scroll_plan().and_then(|plan| plan.stop_of(&filter));
        self.drop_list_nudge();
        self.page_chrome_at.set(None);
        self.page_chromes.borrow_mut().clear();
        if !shown || stop.is_none() {
            let at = stop.unwrap_or(0);
            self.reduce_page_scroll(at, SettingsPageScrollCommand::Absolute(at));
        }
        let first =
            self.showcase.visible().first().map(|&index| {
                UiKey::new(format!("{CARD_PREFIX}{}", self.showcase.cards[index].slug))
            });
        if let Some(first) = first.filter(|_| stop.is_some()) {
            self.reveal_control(first);
        }
    }

    /// Scroll target key for the highlighted chooser row when the chooser appears.
    pub(super) fn showcase_landing(&self) -> Option<UiKey> {
        if !self.showcase_on_glass() || self.showcase.sheet_open {
            return None;
        }
        let slug = self.highlighted_slug()?;
        self.showcase
            .visible()
            .into_iter()
            .any(|index| self.showcase.cards[index].slug == slug)
            .then(|| self.highlighted_row())
            .flatten()
    }

    /// The page text field the keyboard is in, if any.
    pub(super) fn showcase_text_focus(&self) -> Option<ShowcaseText> {
        if self.route != SettingsRoute::Themes {
            return None;
        }
        match ShowcaseText::for_key(self.common.last_focus.as_ref()?.as_str())? {
            ShowcaseText::Filter if !self.showcase.sheet_open => Some(ShowcaseText::Filter),
            ShowcaseText::SaveName if self.showcase.save.is_some() => Some(ShowcaseText::SaveName),
            ShowcaseText::PaletteSlot if self.showcase.slot.is_some() => {
                Some(ShowcaseText::PaletteSlot)
            }
            _ => None,
        }
    }

    pub(super) fn showcase_input_mut(
        &mut self,
        target: ShowcaseText,
    ) -> Option<&mut crate::native_text_input::TextInputState> {
        match target {
            ShowcaseText::Filter => Some(&mut self.showcase.filter),
            ShowcaseText::SaveName => self.showcase.save.as_mut().map(|save| &mut save.name),
            ShowcaseText::PaletteSlot => self.showcase.slot.as_mut().map(|slot| &mut slot.input),
        }
    }

    /// Theme id currently previewed by a trial, if any.
    pub(super) fn showcase_preview_id(&self) -> Option<String> {
        if !self.showcase_on_glass() {
            return None;
        }
        self.showcase.trial_id().map(str::to_string)
    }

    /// Theme id rendered by the demo stage.
    pub(super) fn showcase_stage_theme_id(&self) -> Option<String> {
        if !self.showcase_on_glass() {
            return None;
        }
        Some(
            self.showcase_preview_id()
                .unwrap_or_else(|| self.configured_theme_id()),
        )
    }

    /// The row the keyboard is on, as its slug.
    pub(super) fn showcase_focused_card(&self) -> Option<String> {
        if !self.showcase_on_glass() || self.showcase.sheet_open {
            return None;
        }
        card_slug_of(self.common.last_focus.as_ref()?).map(str::to_string)
    }

    /// Whether navigation keys step the chooser highlight.
    pub(super) fn showcase_steps_highlight(&self) -> bool {
        self.showcase_focused_card().is_some()
            || self.showcase_pack_focused()
            || self.showcase_text_focus() == Some(ShowcaseText::Filter)
    }

    /// Move the chooser highlight to `id`, starting or ending a trial as appropriate.
    pub(super) fn highlight_card(&mut self, id: &str) -> bool {
        self.showcase.note_input();
        if matches!(&self.showcase.trial, ShowcaseTrial::Keeping { id: kept, .. }
            if kept.eq_ignore_ascii_case(id))
        {
            return true;
        }
        self.showcase.pending_select = None;
        self.showcase.keep_more = false;
        self.showcase.error = None;
        self.showcase.pending_keep = None;
        self.showcase.highlight_side = self.theme_made_for(id);
        if id.eq_ignore_ascii_case(&self.configured_theme_id()) {
            if self.showcase.trial != ShowcaseTrial::Idle {
                self.showcase.settled = Some(Settled {
                    id: id.to_string(),
                    words: format!("Reverted to {}", self.showcase.label_of(id)),
                });
            }
            self.showcase.trial = ShowcaseTrial::Idle;
        } else {
            self.showcase.settled = None;
            self.showcase.trial = ShowcaseTrial::Trying { id: id.to_string() };
        }
        true
    }

    /// Step the chooser highlight by `delta` visible rows and move focus with it.
    pub(super) fn step_highlight(&mut self, delta: isize) -> bool {
        let order = self.showcase.order.borrow().clone();
        if order.is_empty() {
            return true;
        }
        let from_pack = self.showcase_pack_focused();
        let target = if from_pack {
            let first = order
                .iter()
                .position(|slug| {
                    self.showcase
                        .card_by_slug(slug)
                        .is_some_and(|card| card.section == Section::Community)
                })
                .unwrap_or(order.len());
            if delta > 0 && first == order.len() {
                let _ = self.step_list_focus(1);
                return true;
            }
            let from = first as isize - isize::from(delta > 0);
            usize::try_from(from.saturating_add(delta).max(0))
                .unwrap_or(0)
                .min(order.len() - 1)
        } else {
            let current = self
                .showcase_focused_card()
                .filter(|slug| order.contains(slug))
                .or_else(|| self.highlighted_slug().filter(|slug| order.contains(slug)));
            match current.and_then(|slug| order.iter().position(|row| *row == slug)) {
                Some(at) => at.saturating_add_signed(delta).min(order.len() - 1),
                None if delta < 0 => order.len() - 1,
                None => 0,
            }
        };
        let slug = order[target].clone();
        let Some(id) = self
            .showcase
            .card_by_slug(&slug)
            .map(|card| card.id.clone())
        else {
            return true;
        };
        if !self.highlight_card(&id) {
            return true;
        }
        if from_pack || self.showcase_focused_card().is_some() {
            let key = UiKey::new(format!("{CARD_PREFIX}{slug}"));
            self.common.last_focus = Some(key.clone());
            self.common.focus_visible = true;
            self.reveal_control(key);
        } else {
            self.land_on_control(UiKey::new(format!("{CARD_PREFIX}{slug}")));
        }
        true
    }

    /// Route Tab / Shift-Tab across a theme row run as a single list-box tab stop.
    pub(crate) fn tab_past_options(
        &self,
        current: Option<&UiKey>,
        next: &UiKey,
        forward: bool,
        order: &[UiKey],
    ) -> Option<UiKey> {
        if self.route != SettingsRoute::Themes || self.showcase.sheet_open {
            return None;
        }
        let owner = |key: &UiKey| {
            let rest = key.as_str().strip_prefix(CARD_PREFIX)?;
            let slug = rest.split('/').next().unwrap_or(rest);
            self.showcase
                .card_by_slug(slug)
                .map(|card| card.slug.as_str())
        };
        let row = |key: &UiKey| card_slug_of(key).is_some() && owner(key).is_some();
        let plan = self.scroll_plan()?;
        let controls: Vec<UiKey> = plan.controls().map(|(_, key)| key.clone()).collect();
        let at = controls.iter().position(|key| key == next)?;
        owner(next)?;
        let mut start = at;
        while start > 0 && owner(&controls[start - 1]).is_some() {
            start -= 1;
        }
        let mut end = at;
        while controls
            .get(end + 1)
            .is_some_and(|key| owner(key).is_some())
        {
            end += 1;
        }
        let run = &controls[start..=end];
        let highlighted = self
            .highlighted_slug()
            .map(|slug| UiKey::new(format!("{CARD_PREFIX}{slug}")))
            .filter(|key| controls.contains(key));
        let leave = || {
            let past = if forward {
                controls.get(end + 1)
            } else {
                start.checked_sub(1).and_then(|index| controls.get(index))
            };
            past.cloned().or_else(|| {
                let inside = |key: &UiKey| run.contains(key);
                let target = if forward {
                    let last = order.iter().rposition(inside)?;
                    order.get(last + 1).or_else(|| order.first())
                } else {
                    let first = order.iter().position(inside)?;
                    first
                        .checked_sub(1)
                        .and_then(|index| order.get(index))
                        .or_else(|| order.last())
                }?;
                (!inside(target)).then(|| target.clone())
            })
        };
        if let Some(current) = current.filter(|current| run.contains(current)) {
            if owner(current) == owner(next) {
                return None;
            }
            return leave().filter(|target| target != next);
        }
        let stop = match &highlighted {
            Some(key) if run.contains(key) => key.clone(),
            Some(_) => return leave().filter(|target| target != next),
            None => run.iter().find(|key| row(key))?.clone(),
        };
        (stop != *next).then_some(stop)
    }

    /// Whether the keyboard is on the Community disclosure.
    pub(super) fn showcase_pack_focused(&self) -> bool {
        self.route == SettingsRoute::Themes
            && !self.showcase.sheet_open
            && self
                .common
                .last_focus
                .as_ref()
                .is_some_and(|key| key.as_str() == PACK)
    }

    /// Open or close the Community disclosure.
    pub(super) fn set_showcase_pack_open(&mut self, open: bool) {
        if self.showcase.pack_open != open {
            self.showcase.pack_open = open;
            self.invalidate_result_page_limit();
            self.common.presentation_revision = self.common.presentation_revision.saturating_add(1);
        }
    }

    /// The rows a screen of the chooser holds.
    pub(super) fn showcase_page_rows(&self) -> usize {
        self.showcase.page_rows.get().max(1)
    }

    /// Row count to step for Page Up (`-1`) or Page Down (`+1`) based on measured scroll stops.
    pub(super) fn showcase_page_step(&self, direction: isize) -> isize {
        let direction = direction.signum();
        let fallback = direction * self.showcase_page_rows() as isize;
        let Some(plan) = self.scroll_plan() else {
            return fallback;
        };
        let order = self.showcase.order.borrow();
        let row_stop =
            |index: usize| plan.stop_of(&UiKey::new(format!("{CARD_PREFIX}{}", order[index])));
        let (here, from) = if self.showcase_pack_focused() {
            let first = order
                .iter()
                .position(|slug| {
                    self.showcase
                        .card_by_slug(slug)
                        .is_some_and(|card| card.section == Section::Community)
                })
                .unwrap_or(order.len());
            let Some(here) = plan.stop_of(&UiKey::new(PACK)) else {
                return fallback;
            };
            (here, first as isize - isize::from(direction > 0))
        } else {
            let Some(at) = self
                .showcase_focused_card()
                .or_else(|| self.highlighted_slug())
                .and_then(|slug| order.iter().position(|row| *row == slug))
            else {
                return fallback;
            };
            let Some(here) = row_stop(at) else {
                return fallback;
            };
            (here, at as isize)
        };
        let on = |screen: &std::ops::Range<usize>, index: usize| {
            row_stop(index).is_some_and(|stop| screen.contains(&stop))
        };
        let target = if direction > 0 {
            let screen = plan.view(here + 1);
            let next = usize::try_from(from + 1).unwrap_or(0);
            (next..order.len())
                .rev()
                .find(|index| on(&screen, *index))
                .or((next < order.len()).then_some(next))
        } else {
            let screen = plan.page_before(here)..here;
            let before = usize::try_from(from).unwrap_or(0);
            (0..before)
                .find(|index| on(&screen, *index))
                .or_else(|| before.checked_sub(1))
        };
        target.map_or(0, |target| target as isize - from)
    }

    /// The highlighted row: the trial's, else the theme in use's.
    pub(crate) fn highlighted_slug(&self) -> Option<String> {
        let id = self
            .showcase
            .trial_id()
            .map(str::to_string)
            .unwrap_or_else(|| self.configured_theme_id());
        self.showcase.card(&id).map(|card| card.slug.clone())
    }

    /// The highlighted row's key ([`Self::highlighted_slug`]).
    pub(super) fn highlighted_row(&self) -> Option<UiKey> {
        self.highlighted_slug()
            .map(|slug| UiKey::new(format!("{CARD_PREFIX}{slug}")))
    }

    /// Expand the Community disclosure if `key` names a row inside it.
    pub(crate) fn open_disclosure_holding(&mut self, key: &UiKey) -> bool {
        let Some(slug) = card_slug_of(key) else {
            return false;
        };
        let closed = !self.showcase.pack_open
            && self
                .showcase
                .card_by_slug(slug)
                .is_some_and(|card| card.section == Section::Community);
        if closed {
            self.showcase.pack_open = true;
            self.invalidate_result_page_limit();
        }
        closed
    }

    /// Configured theme id for the currently displayed appearance side.
    pub(super) fn configured_theme_id(&self) -> String {
        self.theme_id_drawn_by(self.raw_value(prefs::EDIT_THEME))
    }

    /// Theme id that an active trial replaces on the displayed appearance side.
    pub(super) fn theme_id_under_trial(&self) -> String {
        self.theme_id_drawn_by(self.theme_under_trial())
    }

    fn theme_id_drawn_by(&self, theme: Option<String>) -> String {
        let side = theme_visible_side(self, self.presented_motion.get().system_dark);
        let config = Config {
            theme: theme.or_else(|| {
                self.environment_override(prefs::EDIT_THEME)
                    .map(|active| active.effective.clone())
            }),
            ..Config::default()
        };
        config.resolved_colors(side, &self.assets.themes, None).id
    }

    fn keep_value(&self, id: &str) -> (Option<String>, Option<aterm_types::Appearance>) {
        let raw = self.raw_value(prefs::EDIT_THEME);
        let side = chosen_theme_side(self)
            .or_else(|| self.theme_made_for(id))
            .unwrap_or_else(|| os_theme_side(self.presented_motion.get().system_dark));
        let (value, side) =
            prefs::theme_choice_value(raw.as_deref(), side, id, &self.assets.themes);
        (Some(value), side)
    }

    /// Resolved colors for the demo stage, including active trial and drafts.
    pub(super) fn showcase_colors(&self) -> crate::resolved_colors::ResolvedColors {
        let system_dark = self.presented_motion.get().system_dark;
        preview_resolved_colors_with(self, system_dark, self.showcase.trial_id())
    }

    /// Typography declared by the saved `theme` for the displayed appearance side.
    pub(crate) fn theme_typography(&self) -> crate::resolved_typography::ResolvedTypography {
        let side = theme_visible_side(self, self.presented_motion.get().system_dark);
        let config = Config {
            theme: self.raw_value(prefs::EDIT_THEME),
            ..Config::default()
        };
        config.resolved_typography_with_flag(side, &self.assets.themes, None)
    }

    /// Font size scale from the saved theme when unset in config (`1.0` by default).
    pub(crate) fn theme_typography_scale(&self) -> f32 {
        self.theme_typography()
            .applied
            .font_size_scale_milli
            .map_or(1.0, |milli| f32::from(milli) / 1000.0)
    }

    fn theme_palette(&self) -> [aterm_types::Rgb; 16] {
        let system_dark = self.presented_motion.get().system_dark;
        let side = theme_visible_side(self, system_dark);
        let config = Config {
            theme: self.raw_value(prefs::EDIT_THEME),
            ..Config::default()
        };
        config
            .resolved_colors_previewing(side, &self.assets.themes, self.showcase.trial_id())
            .scheme
            .ansi
    }

    fn palette_entries(&self) -> Vec<String> {
        let text = self.field_inputs.get(prefs::EDIT_PALETTE).map_or_else(
            || {
                self.field_by_key(prefs::EDIT_PALETTE)
                    .and_then(|field| field.seed.clone())
                    .unwrap_or_default()
            },
            |input| input.value().to_string(),
        );
        text.split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn set_palette_draft(&mut self, entries: &[String]) {
        let text = entries.join(", ");
        self.field_inputs.insert(
            prefs::EDIT_PALETTE.to_string(),
            crate::native_text_input::TextInputState::new(text),
        );
        self.invalidate_result_page_limit();
    }

    fn slot_on_list(&self, index: usize) -> bool {
        self.palette_entries().len() > index
    }

    fn slot_override_draft(&self) -> Option<(usize, aterm_types::Rgb)> {
        let slot = self.showcase.slot.as_ref()?;
        if self.slot_on_list(slot.index) {
            return None;
        }
        aterm_types::scheme::parse_hex(slot.input.value().trim())
            .filter(|color| *color != slot.opened)
            .map(|color| (slot.index, color))
    }

    /// Effective `palette_overrides` map including any open slot draft.
    pub(super) fn showcase_palette_overrides(&self) -> Option<BTreeMap<String, String>> {
        let prefix = format!("{}.", prefs::EDIT_PALETTE_OVERRIDES);
        let mut table: BTreeMap<String, String> = self
            .raw_values
            .iter()
            .filter_map(|(key, value)| {
                key.strip_prefix(&prefix)
                    .map(|slot| (slot.to_string(), value.clone()))
            })
            .collect();
        if let Some((index, color)) = self.slot_override_draft() {
            table.insert(index.to_string(), hex_of(color));
        }
        (!table.is_empty()).then_some(table)
    }

    fn configured_theme_ids(&self) -> [String; 2] {
        let config = Config {
            theme: self.raw_value(prefs::EDIT_THEME).or_else(|| {
                self.environment_override(prefs::EDIT_THEME)
                    .map(|active| active.effective.clone())
            }),
            ..Config::default()
        };
        [
            aterm_types::Appearance::Dark,
            aterm_types::Appearance::Light,
        ]
        .map(|side| config.resolved_colors(side, &self.assets.themes, None).id)
    }
}

fn card_slug_of(key: &UiKey) -> Option<&str> {
    key.as_str()
        .strip_prefix(CARD_PREFIX)
        .filter(|slug| !slug.contains('/'))
}

/// Return `entries` with slot `index` set to `value` if `index < entries.len()`.
pub(super) fn palette_with(entries: &[String], index: usize, value: &str) -> Option<Vec<String>> {
    let mut out = entries.to_vec();
    *out.get_mut(index)? = value.to_string();
    Some(out)
}

/// Reset slot `index` in `entries` to `theme`'s color and trim trailing entries matching `theme`.
pub(super) fn palette_reset(
    entries: &[String],
    index: usize,
    theme: &[aterm_types::Rgb; 16],
) -> Vec<String> {
    let mut out = entries.to_vec();
    if index < out.len() {
        out[index] = hex_of(theme[index.min(15)]);
    }
    while let Some(last) = out.last() {
        let slot = out.len() - 1;
        let same = slot < 16
            && aterm_types::scheme::parse_hex(last).is_some_and(|color| color == theme[slot]);
        if same {
            out.pop();
        } else {
            break;
        }
    }
    out
}

/// Spoken label for a `palette_overrides.<index>` key (`"Palette colour 4, blue"`).
pub(super) fn palette_slot_name(key: &str) -> Option<String> {
    let index = key
        .strip_prefix(prefs::EDIT_PALETTE_OVERRIDES)?
        .strip_prefix('.')?
        .parse::<usize>()
        .ok()?;
    let name = SLOT_NAMES.get(index)?;
    Some(format!("Palette color {index}, {name}"))
}

fn hex_of(color: aterm_types::Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

/// The ANSI slot names, for a palette chip's spoken label.
pub(super) const SLOT_NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright black",
    "bright red",
    "bright green",
    "bright yellow",
    "bright blue",
    "bright magenta",
    "bright cyan",
    "bright white",
];

/// Why `name` cannot be saved as a new theme, or `None` when it can.
pub(super) fn save_name_problem(
    name: &str,
    themes: &crate::app_config::ThemeCatalog,
) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some("Name the theme".to_string());
    }
    if let Err(why) = aterm_types::scheme::validate_user_theme_name(name) {
        let mut chars = why.chars();
        return Some(
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default(),
        );
    }
    if aterm_types::scheme::builtin_theme(name).is_some() {
        return Some(format!("{name} is a built-in theme"));
    }
    if themes
        .listing()
        .any(|(id, _)| id.eq_ignore_ascii_case(name))
    {
        return Some(format!("A theme named {name} already exists"));
    }
    None
}

/// First unused `<label> Custom`, `<label> Custom 2`, … theme name.
pub(super) fn suggested_save_name(label: &str, themes: &crate::app_config::ThemeCatalog) -> String {
    let label = label.trim();
    let stem = label
        .rsplit_once(' ')
        .filter(|(_, last)| !last.is_empty() && last.chars().all(|ch| ch.is_ascii_digit()))
        .map_or(label, |(stem, _)| stem);
    let label = stem
        .strip_suffix(" Custom")
        .filter(|base| !base.trim().is_empty())
        .unwrap_or(label);
    let base: String = format!("{label} Custom")
        .chars()
        .filter(|ch| !matches!(ch, '/' | '\\' | ',' | '[' | ']' | ':') && !ch.is_control())
        .collect();
    let base = base.trim().to_string();
    let mut name = base.clone();
    let mut n = 2;
    while save_name_problem(&name, themes).is_some() && n < 100 {
        name = format!("{base} {n}");
        n += 1;
    }
    name
}

/// Serialize `resolved` as a native theme file named `name` along with its carried override keys.
pub(super) fn save_as_text(
    resolved: &crate::resolved_colors::ResolvedColors,
    name: &str,
    from_label: &str,
    typography: &aterm_types::scheme::ThemeTypography,
) -> Result<String, String> {
    let mut scheme = resolved.scheme.clone();
    scheme.name = aterm_types::scheme_import::sanitize_text(name).unwrap_or_default();
    scheme.appearance = resolved.reported;
    let description = if typography.is_empty() {
        format!("Saved from {from_label} with your color settings")
    } else {
        format!("Saved from {from_label} with your color and type settings")
    };
    let mut theme = aterm_types::scheme::ParsedTheme::colours_only(
        scheme,
        true,
        aterm_types::scheme_import::sanitize_text(&description),
        None,
    );
    theme.typography = typography.clone();
    let text = aterm_types::scheme_import::to_native_conf_package(
        &theme,
        &["Saved by Settings \u{25b8} Themes \u{25b8} Save as theme"],
    );
    let parsed = aterm_types::scheme::parse_theme_str(&text).map_err(|error| error.to_string())?;
    if parsed != theme {
        return Err("the theme did not read back as written".to_string());
    }
    Ok(text)
}

impl SettingsViewState {
    /// Typography settings to serialize when "Include type settings" is enabled.
    pub(super) fn save_typography(&self) -> aterm_types::scheme::ThemeTypography {
        let mut typography = aterm_types::scheme::ThemeTypography::NONE;
        for key in SAVE_TYPE_KEYS {
            let drafted = preview_field_text(self, key, "");
            let value = Some(drafted.trim().to_string())
                .filter(|value| !value.is_empty())
                .or_else(|| self.theme_fallback_for(key).map(|(value, _)| value));
            let Some(value) = value else { continue };
            let probe = format!("fg = #ffffff\nbg = #000000\n{key} = {value}\n");
            if let Ok(parsed) = aterm_types::scheme::parse_theme_str(&probe) {
                typography.fill_from(&parsed.typography);
            }
        }
        typography
    }

    /// Whether any non-default typography setting is active or drafted.
    pub(super) fn save_type_in_force(&self) -> bool {
        SAVE_TYPE_KEYS.iter().any(|key| {
            self.raw_value(key).is_some()
                || self.field_has_unsaved_draft(key)
                || self.theme_fallback_for(key).is_some()
        })
    }
}

/// Config keys carried by a theme saved from `resolved` so they can be cleared on select.
pub(super) fn carried_override_keys(
    resolved: &crate::resolved_colors::ResolvedColors,
    typography: &aterm_types::scheme::ThemeTypography,
) -> Vec<String> {
    let overrides = format!("{}.", prefs::EDIT_PALETTE_OVERRIDES);
    let source_keys = resolved.config_keys();
    let fully_carried = |prefix: &str| {
        !resolved
            .issues
            .iter()
            .any(|issue| issue.key.starts_with(prefix))
            && source_keys
                .iter()
                .filter_map(|key| key.strip_prefix(prefix))
                .all(|slot| {
                    slot.trim_end_matches(']')
                        .parse::<usize>()
                        .is_ok_and(|index| index < 16)
                })
    };
    let clear_list = fully_carried("palette[");
    let clear_overrides = fully_carried(&overrides);
    let mut keys: Vec<String> = Vec::new();
    for key in source_keys {
        let key = if key.starts_with("palette[") {
            if !clear_list {
                continue;
            }
            prefs::EDIT_PALETTE.to_string()
        } else if key.starts_with(&overrides) {
            if !clear_overrides {
                continue;
            }
            prefs::EDIT_PALETTE_OVERRIDES.to_string()
        } else {
            key
        };
        if (SHOWCASE_COLOR_KEYS.contains(&key.as_str())
            || key == prefs::EDIT_PALETTE
            || key == prefs::EDIT_PALETTE_OVERRIDES)
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
    keys.extend(
        typography
            .keys()
            .into_iter()
            .filter(|key| SAVE_TYPE_KEYS.contains(key))
            .map(str::to_string),
    );
    keys
}

/// Number of active color and palette overrides on top of the theme.
pub(super) fn override_count(resolved: &crate::resolved_colors::ResolvedColors) -> usize {
    let mut named = 0;
    let mut slots = BTreeSet::new();
    for key in resolved.config_keys() {
        if SHOWCASE_COLOR_KEYS.contains(&key.as_str()) {
            named += 1;
            continue;
        }
        let slot = key
            .strip_prefix("palette[")
            .and_then(|rest| rest.strip_suffix(']'))
            .or_else(|| {
                key.strip_prefix(prefs::EDIT_PALETTE_OVERRIDES)
                    .and_then(|rest| rest.strip_prefix('.'))
            })
            .and_then(|index| index.parse::<usize>().ok());
        slots.extend(slot);
    }
    named + slots.len()
}

/// `count` and its noun, singular or plural: `1 color`, `3 colors`.
pub(super) fn count_noun(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// Format `count` as `"1 custom color"` / `"N custom colors"`.
pub(super) fn custom_colors(count: usize) -> String {
    count_noun(count, "custom color", "custom colors")
}

/// Strip note summarizing active custom color overrides, if any.
pub(super) fn override_note(resolved: &crate::resolved_colors::ResolvedColors) -> Option<String> {
    let count = override_count(resolved);
    (count > 0).then(|| format!("{} on every theme", custom_colors(count)))
}

/// The page's palette commands while it is the route.
pub(super) fn showcase_commands(view: &SettingsViewState) -> Vec<Command> {
    let trying = view.showcase.trial_id().is_some();
    vec![
        Command {
            id: ActionId::new(SHOWCASE_KEEP),
            title: "Themes: Keep Trial".to_string(),
            shortcut: None,
            enabled: trying,
        },
        Command {
            id: ActionId::new(SHOWCASE_REVERT),
            title: "Themes: Revert Trial".to_string(),
            shortcut: None,
            enabled: trying,
        },
        Command {
            id: ActionId::new(SHOWCASE_KEEP_COLOURS),
            title: "Themes: Keep Colors Only".to_string(),
            shortcut: None,
            enabled: view
                .showcase
                .trial_id()
                .is_some_and(|id| view.trial_typography(id).keeps_colours_only()),
        },
        Command {
            id: ActionId::new(SHOWCASE_SAVE_AS),
            title: "Themes: Save as Theme\u{2026}".to_string(),
            shortcut: None,
            enabled: true,
        },
    ]
}

impl SettingsApp {
    /// A press on the page ([`SHOWCASE_ACTION_PREFIX`]).
    pub(super) fn reduce_showcase_action(
        &mut self,
        view: &mut SettingsViewState,
        action: &str,
        value: Option<&SemanticInput>,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        let text = match value {
            Some(SemanticInput::Text(text)) => Some(text.as_str()),
            _ => None,
        };
        view.showcase.note_input();
        let armed = view.showcase.trash_armed.take();
        if let Some(rest) = action.strip_prefix(CARD_PREFIX) {
            match rest.split_once('/') {
                None => self.activate_card(view, rest, cx),
                Some((slug, CARD_OPEN)) => Self::open_card_file(view, slug, cx),
                Some((slug, CARD_TRASH)) => Self::trash_card(view, slug, armed, cx),
                Some(_) => {}
            }
        } else if action == SHOWCASE_KEEP {
            self.keep_trial(view, cx);
        } else if action == KEEP_MORE {
            view.showcase.keep_more = !view.showcase.keep_more;
            view.common.last_focus = Some(UiKey::new(if view.showcase.keep_more {
                SHOWCASE_KEEP_COLOURS
            } else {
                KEEP_MORE
            }));
        } else if action == SHOWCASE_KEEP_COLOURS {
            view.showcase.keep_more = false;
            self.keep_colours_only(view, cx);
        } else if action == SHOWCASE_REVERT {
            self.revert_trial(view, cx);
        } else if action == CUSTOMIZE {
            view.showcase.sheet_open = true;
            view.showcase.keep_more = false;
            view.invalidate_result_page_limit();
            view.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
            view.common.last_focus = Some(UiKey::new(CUSTOMIZE_DONE));
        } else if action == CUSTOMIZE_DONE {
            self.close_palette_slot(view, cx);
            view.close_showcase_sheet();
        } else if action == PACK {
            view.showcase.pack_open = match value {
                Some(SemanticInput::Bool(open)) => *open,
                _ => !view.showcase.pack_open,
            };
            view.invalidate_result_page_limit();
            view.common.last_focus = Some(UiKey::new(PACK));
        } else if action == FOLLOW_SYSTEM {
            let _ = self.reduce_action(
                view,
                ActionInvocation {
                    id: ActionId::new(THEME_MODE_ACTION),
                    value: value.cloned(),
                },
                cx,
            );
            view.common.last_focus = Some(UiKey::new(FOLLOW_SYSTEM));
        } else if let Some(side) = action
            .strip_prefix(SIDE_PREFIX)
            .and_then(parse_appearance_word)
        {
            view.theme_preview_side = Some(side);
            if matches!(view.showcase.trial, ShowcaseTrial::Trying { .. }) {
                view.showcase.trial = ShowcaseTrial::Idle;
            }
            view.showcase.error = None;
            view.common.last_focus = Some(UiKey::new(action));
            if let Some(slug) = view.highlighted_slug() {
                view.land_on_control(UiKey::new(format!("{CARD_PREFIX}{slug}")));
            }
        } else if action == FILTER_CLEAR {
            view.showcase.filter = crate::native_text_input::TextInputState::new(String::new());
            view.invalidate_result_page_limit();
            view.scroll_to_results();
            view.common.last_focus = Some(UiKey::new(SHOWCASE_FILTER));
        } else if action == SHOWCASE_FILTER {
            if let Some(text) = text {
                view.showcase.filter = crate::native_text_input::TextInputState::new(text.into());
                view.invalidate_result_page_limit();
                view.scroll_to_results();
            }
            view.common.last_focus = Some(UiKey::new(SHOWCASE_FILTER));
        } else if action == SHOWCASE_SAVE_AS {
            let label = view
                .showcase
                .trial_id()
                .map(str::to_string)
                .unwrap_or_else(|| view.configured_theme_id());
            let label = view
                .assets
                .themes
                .meta(&label)
                .map_or(label, |meta| meta.display_name);
            let name = suggested_save_name(&label, &view.assets.themes);
            let mut input = crate::native_text_input::TextInputState::new(name);
            input.select_all();
            view.showcase.save = Some(SaveAsState {
                name: input,
                error: None,
                include_type: false,
            });
            view.showcase.sheet_open = true;
            view.invalidate_result_page_limit();
            view.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_NAME));
        } else if action == SHOWCASE_SAVE_NAME {
            if let (Some(text), Some(save)) = (text, view.showcase.save.as_mut()) {
                save.name = crate::native_text_input::TextInputState::new(text.into());
                save.error = save_name_problem(text, &view.assets.themes);
            }
            view.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_NAME));
        } else if action == SAVE_CONFIRM {
            self.save_as_theme(view, cx);
        } else if action == SHOWCASE_SAVE_TYPE {
            if let Some(save) = view.showcase.save.as_mut() {
                save.include_type = match value {
                    Some(SemanticInput::Bool(wanted)) => *wanted,
                    _ => !save.include_type,
                };
            }
            view.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_TYPE));
        } else if action == SAVE_CANCEL {
            view.showcase.save = None;
            view.invalidate_result_page_limit();
            view.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_AS));
        } else if action == IMPORT || action == OPEN_FOLDER {
            let (request, feedback) = if action == OPEN_FOLDER {
                (
                    ThemesRequest::OpenFolder,
                    "Opening the themes folder\u{2026}",
                )
            } else {
                (
                    ThemesRequest::Import,
                    "Choose a theme file to import\u{2026}",
                )
            };
            let operation = cx.themes(request);
            view.pending.insert(operation, PendingAction::Themes);
            view.feedback = Some(feedback.to_string());
        } else if let Some(index) = action
            .strip_prefix(PALETTE_PREFIX)
            .and_then(|index| index.parse::<usize>().ok())
            .filter(|index| *index < 16)
        {
            self.close_palette_slot(view, cx);
            let shown = view.showcase_colors().scheme.ansi[index];
            let mut input = crate::native_text_input::TextInputState::new(hex_of(shown));
            input.select_all();
            view.showcase.slot = Some(SlotEditor {
                index,
                input,
                opened: shown,
            });
            view.invalidate_result_page_limit();
            view.common.last_focus = Some(UiKey::new(SHOWCASE_PALETTE_SLOT));
            view.reveal_control(UiKey::new(SHOWCASE_PALETTE_SLOT));
        } else if action == SHOWCASE_PALETTE_SLOT {
            if let Some(text) = text {
                self.edit_palette_slot(view, text);
            }
            view.common.last_focus = Some(UiKey::new(SHOWCASE_PALETTE_SLOT));
        } else if action == PALETTE_SLOT_RESET {
            self.reset_palette_slot(view, cx);
        } else if action == PALETTE_SLOT_CLOSE {
            let index = view.showcase.slot.as_ref().map(|slot| slot.index);
            self.close_palette_slot(view, cx);
            if let Some(index) = index {
                view.common.last_focus = Some(UiKey::new(format!("{PALETTE_PREFIX}{index}")));
            }
            view.invalidate_result_page_limit();
        } else if action == PALETTE_RESET_ALL {
            self.reset_palette(view, cx);
        }
        view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
        cx.repaint(crate::native_app::DamageRegion::All);
        EventResult::Handled
    }

    fn activate_card(&mut self, view: &mut SettingsViewState, slug: &str, cx: &mut UpdateCx<'_>) {
        let Some(card) = view.showcase.card_by_slug(slug).cloned() else {
            view.feedback = Some("That theme is no longer listed".to_string());
            return;
        };
        view.common.last_focus = Some(UiKey::new(format!("{CARD_PREFIX}{slug}")));
        if matches!(&view.showcase.trial, ShowcaseTrial::Trying { id } if id.eq_ignore_ascii_case(&card.id))
        {
            self.keep_trial(view, cx);
            return;
        }
        if card.section == Section::Community {
            view.showcase.pack_open = true;
            view.invalidate_result_page_limit();
        }
        let _ = view.highlight_card(&card.id);
    }

    fn open_card_file(view: &mut SettingsViewState, slug: &str, cx: &mut UpdateCx<'_>) {
        let file = view
            .showcase
            .card_by_slug(slug)
            .filter(|card| card.section == Section::Yours)
            .map(|card| (card.id.clone(), card.label.clone()))
            .or_else(|| {
                view.showcase
                    .attention
                    .iter()
                    .find(|row| row.slug == slug)
                    .map(|row| (row.id.clone(), row.id.clone()))
            });
        let Some((id, label)) = file else {
            view.feedback = Some("That theme is no longer listed".to_string());
            return;
        };
        view.common.last_focus = Some(UiKey::new(format!("{CARD_PREFIX}{slug}/{CARD_OPEN}")));
        let operation = cx.themes(ThemesRequest::OpenFile { id });
        view.pending.insert(operation, PendingAction::Themes);
        view.feedback = Some(format!("Opening {label}\u{2026}"));
    }

    fn trash_card(
        view: &mut SettingsViewState,
        slug: &str,
        armed: Option<String>,
        cx: &mut UpdateCx<'_>,
    ) {
        let Some(card) = view
            .showcase
            .card_by_slug(slug)
            .filter(|card| card.section == Section::Yours)
            .cloned()
        else {
            view.feedback = Some("That theme is no longer listed".to_string());
            return;
        };
        view.common.last_focus = Some(UiKey::new(format!("{CARD_PREFIX}{slug}/{CARD_TRASH}")));
        if view
            .configured_theme_ids()
            .iter()
            .any(|id| id.eq_ignore_ascii_case(&card.id))
        {
            view.feedback = Some(format!(
                "{} is in use \u{b7} choose another theme first",
                card.label
            ));
            return;
        }
        if view
            .showcase
            .trial_id()
            .is_some_and(|id| id.eq_ignore_ascii_case(&card.id))
        {
            view.feedback = Some(format!("Revert the trial of {} first", card.label));
            return;
        }
        if armed.as_deref() != Some(slug) {
            view.showcase.trash_armed = Some(slug.to_string());
            view.feedback = Some(format!(
                "Press Trash again to move {} to the Trash",
                card.label
            ));
            return;
        }
        let operation = cx.themes(ThemesRequest::Trash { id: card.id });
        view.pending.insert(operation, PendingAction::Themes);
        view.feedback = Some(format!("Moving {} to the Trash\u{2026}", card.label));
    }

    /// Revert the active trial, or write back prior values if a Keep is already in flight.
    pub(super) fn revert_trial(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        view.showcase.pending_select = None;
        view.showcase.revert_asked = false;
        if let ShowcaseTrial::Keeping { id, under, .. } = view.showcase.trial.clone() {
            let back: Vec<ConfigEdit> = under
                .into_iter()
                .filter_map(|(key, before)| {
                    let now = view.raw_value(&key);
                    (now != before).then_some(ConfigEdit {
                        key,
                        expected: ExpectedConfigValue::Exact(now),
                        value: before,
                    })
                })
                .collect();
            let operation = (!back.is_empty()).then(|| {
                Self::send_config(
                    view,
                    cx,
                    ConfigPatch {
                        base_revision: self.config_revision,
                        edits: back,
                    },
                )
            });
            view.showcase.trial = ShowcaseTrial::Trying { id };
            view.revert_showcase();
            let to = view.showcase.label_of(&view.configured_theme_id());
            view.feedback = Some(match operation {
                Some(operation) => {
                    view.name_write(operation, WriteKind::Revert { to: to.clone() });
                    format!("Reverting to {to}\u{2026}")
                }
                None => format!("Reverted to {to}"),
            });
            return;
        }
        view.revert_showcase();
    }

    fn keep_trial(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        let id = match view.showcase.trial.clone() {
            ShowcaseTrial::Trying { id } => id,
            ShowcaseTrial::Keeping { id, operation, .. } => {
                if !view.showcase_select_is(operation) {
                    view.showcase.keep_more = false;
                    self.keep_over_keeping(view, cx, id);
                }
                return;
            }
            ShowcaseTrial::Idle => return,
        };
        view.showcase.keep_more = false;
        if view.assets.themes.resolve(&id).is_err()
            && view.assets.themes.pack_fallback(&id).is_some()
        {
            if view.showcase.pending_keep.is_some() {
                return;
            }
            let operation = cx.themes(ThemesRequest::AddPack { id: id.clone() });
            view.pending.insert(operation, PendingAction::Themes);
            view.feedback = Some(format!("Adding {}\u{2026}", view.showcase.label_of(&id)));
            view.showcase.pending_keep = Some(PendingKeep {
                operation,
                id,
                written: false,
            });
            return;
        }
        let (value, side) = view.keep_value(&id);
        if view.raw_value(prefs::EDIT_THEME) == value {
            view.showcase.trial = ShowcaseTrial::Idle;
            return;
        }
        let patch = ConfigPatch {
            base_revision: self.config_revision,
            edits: vec![ConfigEdit {
                key: prefs::EDIT_THEME.to_string(),
                expected: ExpectedConfigValue::Exact(view.raw_value(prefs::EDIT_THEME)),
                value,
            }],
        };
        let under = view.trial_under(&patch.edits);
        let operation = Self::send_config(view, cx, patch);
        view.feedback = Some(keeping_words(view, &id, side));
        view.showcase.trial = ShowcaseTrial::Keeping {
            id,
            operation,
            under,
        };
    }

    fn keep_over_keeping(
        &mut self,
        view: &mut SettingsViewState,
        cx: &mut UpdateCx<'_>,
        id: String,
    ) {
        let (value, side) = view.keep_value(&id);
        let mut under = view.trial_under(&[]);
        under
            .entry(prefs::EDIT_THEME.to_string())
            .or_insert_with(|| view.raw_value(prefs::EDIT_THEME));
        let edits: Vec<ConfigEdit> = under
            .iter()
            .filter_map(|(key, before)| {
                let want = if key == prefs::EDIT_THEME {
                    value.clone()
                } else {
                    before.clone()
                };
                let now = view.raw_value(key);
                (now != want).then(|| ConfigEdit {
                    key: key.clone(),
                    expected: ExpectedConfigValue::Exact(now),
                    value: want,
                })
            })
            .collect();
        if edits.is_empty() {
            return;
        }
        let operation = Self::send_config(
            view,
            cx,
            ConfigPatch {
                base_revision: self.config_revision,
                edits,
            },
        );
        view.feedback = Some(keeping_words(view, &id, side));
        view.showcase.trial = ShowcaseTrial::Keeping {
            id,
            operation,
            under,
        };
    }

    fn keep_colours_only(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        let id = match view.showcase.trial.clone() {
            ShowcaseTrial::Trying { id } => id,
            ShowcaseTrial::Keeping { id, operation, .. } if !view.showcase_select_is(operation) => {
                id
            }
            ShowcaseTrial::Keeping { .. } | ShowcaseTrial::Idle => return,
        };
        let trial = view.trial_typography(&id);
        if !trial.keeps_colours_only() {
            if let Some(why) = trial.why_not_colours_only(view) {
                view.feedback = Some(why);
            }
            return;
        }
        let (value, _) = view.keep_value(&id);
        let mut edits = vec![ConfigEdit {
            key: prefs::EDIT_THEME.to_string(),
            expected: ExpectedConfigValue::Exact(view.raw_value(prefs::EDIT_THEME)),
            value,
        }];
        for change in &trial.changes {
            if let Some(pin) = &change.pin {
                edits.push(ConfigEdit {
                    key: change.key.to_string(),
                    expected: ExpectedConfigValue::Exact(view.raw_value(change.key)),
                    value: Some(pin.clone()),
                });
            }
        }
        if edits
            .iter()
            .all(|edit| edit.expected == ExpectedConfigValue::Exact(edit.value.clone()))
        {
            return;
        }
        let under = view.trial_under(&edits);
        let patch = ConfigPatch {
            base_revision: self.config_revision,
            edits,
        };
        let operation = Self::send_config(view, cx, patch);
        let label = view
            .showcase
            .card(&id)
            .map_or_else(|| id.clone(), |card| card.label.clone());
        view.name_write(
            operation,
            WriteKind::KeepColours {
                label: label.clone(),
            },
        );
        view.feedback = Some(format!(
            "Keeping {label}'s colors and your type settings\u{2026}"
        ));
        view.showcase.trial = ShowcaseTrial::Keeping {
            id,
            operation,
            under,
        };
    }

    /// Settle an in-flight Keep or save-select operation once its config patch finishes.
    pub(super) fn finish_showcase_keep(
        view: &mut SettingsViewState,
        operation: OperationId,
        applied: bool,
    ) -> bool {
        let mut written = None;
        if view
            .showcase
            .selecting
            .as_ref()
            .is_some_and(|(selecting, ..)| *selecting == operation)
            && let Some((_, saved, drafts)) = view.showcase.selecting.take()
        {
            if applied {
                drop_carried_drafts(view, &drafts);
            }
            written = Some(saved);
        }
        let ShowcaseTrial::Keeping {
            id,
            operation: keeping,
            ..
        } = &view.showcase.trial
        else {
            return false;
        };
        if *keeping != operation {
            return false;
        }
        if applied {
            let kept = written.as_deref().unwrap_or(id).to_string();
            view.showcase.settled = Some(Settled {
                words: format!("Kept {}", view.showcase.label_of(&kept)),
                id: kept.clone(),
            });
            view.showcase.error = None;
            view.showcase.trial = ShowcaseTrial::Idle;
            if view.common.last_focus.as_ref().is_some_and(|key| {
                [
                    SHOWCASE_KEEP,
                    KEEP_MORE,
                    SHOWCASE_KEEP_COLOURS,
                    SHOWCASE_REVERT,
                ]
                .contains(&key.as_str())
            }) {
                view.common.last_focus = Some(UiKey::new(view.showcase.card(&kept).map_or_else(
                    || CUSTOMIZE.to_string(),
                    |card| format!("{CARD_PREFIX}{}", card.slug),
                )));
            }
        } else {
            view.showcase.trial = ShowcaseTrial::Trying { id: id.clone() };
        }
        !applied
    }

    fn save_as_theme(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        let Some(name) = view
            .showcase
            .save
            .as_ref()
            .map(|save| save.name.value().trim().to_string())
        else {
            return;
        };
        if let Some(problem) = save_name_problem(&name, &view.assets.themes) {
            if let Some(save) = view.showcase.save.as_mut() {
                save.error = Some(problem);
            }
            view.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_NAME));
            return;
        }
        let resolved = view.showcase_colors();
        let from = view
            .assets
            .themes
            .meta(&resolved.id)
            .map_or_else(|| resolved.id.clone(), |meta| meta.display_name);
        let typography = if view
            .showcase
            .save
            .as_ref()
            .is_some_and(|save| save.include_type)
        {
            view.save_typography()
        } else {
            aterm_types::scheme::ThemeTypography::NONE
        };
        let text = match save_as_text(&resolved, &name, &from, &typography) {
            Ok(text) => text,
            Err(why) => {
                if let Some(save) = view.showcase.save.as_mut() {
                    save.error = Some(format!("Couldn\u{2019}t save {name}: {why}"));
                }
                return;
            }
        };
        let carried = carried_override_keys(&resolved, &typography);
        let fields = carried
            .iter()
            .filter(|key| view.field_has_unsaved_draft(key))
            .filter(|key| {
                key.as_str() != prefs::EDIT_PALETTE
                    || view.field_inputs.get(key.as_str()).is_some_and(|input| {
                        input
                            .value()
                            .split(',')
                            .filter(|entry| !entry.trim().is_empty())
                            .all(|entry| aterm_types::scheme::parse_hex(entry.trim()).is_some())
                    })
            })
            .filter_map(|key| {
                view.field_inputs
                    .get(key)
                    .map(|input| (key.clone(), input.clone()))
            })
            .collect();
        let drafts = CarriedDrafts {
            fields,
            slot: view
                .slot_override_draft()
                .filter(|_| {
                    carried
                        .iter()
                        .any(|key| key == prefs::EDIT_PALETTE_OVERRIDES)
                })
                .and_then(|(index, _)| {
                    view.showcase
                        .slot
                        .as_ref()
                        .map(|slot| (index, slot.input.clone()))
                }),
        };
        let clears = carried
            .into_iter()
            .filter(|key| view.is_explicit(key))
            .map(|key| ConfigEdit {
                expected: ExpectedConfigValue::Exact(view.raw_value(&key)),
                key,
                value: None,
            })
            .collect();
        let (theme, side) = view.keep_value(&name);
        let theme = ConfigEdit {
            key: prefs::EDIT_THEME.to_string(),
            expected: ExpectedConfigValue::Exact(view.raw_value(prefs::EDIT_THEME)),
            value: theme,
        };
        let operation = cx.themes(ThemesRequest::Save {
            id: name.clone(),
            text,
        });
        view.pending.insert(operation, PendingAction::Themes);
        view.showcase.pending_select = Some(PendingSelect {
            id: name.clone(),
            operation,
            saved: false,
            theme,
            side,
            clears,
            drafts,
        });
        view.showcase.save = None;
        view.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_AS));
        view.feedback = Some(format!("Saving {name}\u{2026}"));
        view.invalidate_result_page_limit();
    }

    /// Complete pending pack keeps or saved-theme selections once the catalog lists the new file.
    pub(super) fn showcase_after_snapshot(
        &mut self,
        view: &mut SettingsViewState,
        cx: &mut UpdateCx<'_>,
    ) {
        if let Some(PendingKeep {
            id, written: true, ..
        }) = &view.showcase.pending_keep
            && view.assets.themes.resolve(id).is_ok()
        {
            let id = id.clone();
            view.showcase.pending_keep = None;
            if matches!(&view.showcase.trial, ShowcaseTrial::Trying { id: tried } if tried.eq_ignore_ascii_case(&id))
            {
                self.keep_trial(view, cx);
            }
            view.invalidate_result_page_limit();
        }
        let Some(select) = view.showcase.pending_select.as_ref() else {
            return;
        };
        if !select.saved || view.assets.themes.resolve(&select.id).is_err() {
            return;
        }
        let Some(select) = view.showcase.pending_select.take() else {
            return;
        };
        if std::iter::once(&select.theme)
            .chain(&select.clears)
            .any(|edit| edit.expected != ExpectedConfigValue::Exact(view.raw_value(&edit.key)))
        {
            view.feedback = Some(format!(
                "Saved {} \u{b7} your settings changed; choose it when ready",
                select.id
            ));
            return;
        }
        let mut select = select;
        let (value, side) = view.keep_value(&select.id);
        select.theme.value = value;
        select.side = side;
        if side.is_some() {
            view.showcase.highlight_side = side;
        }
        let has_clears = !select.clears.is_empty();
        let mut edits = vec![select.theme];
        edits.extend(select.clears);
        let patch = ConfigPatch {
            base_revision: self.config_revision,
            edits,
        };
        let under = view.trial_under(&patch.edits);
        let operation = Self::send_config(view, cx, patch);
        view.showcase.selecting = Some((operation, select.id.clone(), select.drafts));
        view.showcase.trial = match view.showcase.trial_id() {
            Some(tried) => ShowcaseTrial::Keeping {
                id: tried.to_string(),
                operation,
                under,
            },
            None => ShowcaseTrial::Idle,
        };
        view.feedback = Some(if select.side.is_some() && has_clears {
            format!(
                "Selected {} \u{b7} its color settings came off the other side too",
                select.id
            )
        } else {
            format!("Selected {}", select.id)
        });
    }

    /// Handle completion of a "Save as theme" file write.
    pub(super) fn finish_showcase_save(
        view: &mut SettingsViewState,
        operation: OperationId,
        outcome: &ThemesOutcome,
    ) -> bool {
        let Some(select) = view.showcase.pending_select.as_mut() else {
            return false;
        };
        if select.operation != operation {
            return false;
        }
        if matches!(outcome, ThemesOutcome::Saved { id } if id == &select.id) {
            select.saved = true;
            return true;
        }
        let failed = match outcome {
            ThemesOutcome::Failed { message } => message.as_str(),
            _ => "",
        };
        let Some(select) = view.showcase.pending_select.take() else {
            return false;
        };
        if failed.is_empty() {
            return false;
        }
        let mut chars = failed.chars();
        let reason: String = chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
        view.showcase.save = Some(SaveAsState {
            name: crate::native_text_input::TextInputState::new(select.id),
            error: Some(reason),
            include_type: false,
        });
        view.showcase.sheet_open = true;
        view.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_NAME));
        view.invalidate_result_page_limit();
        false
    }

    /// Update the open palette slot draft and sync positional `palette` drafts when applicable.
    pub(super) fn edit_palette_slot(&mut self, view: &mut SettingsViewState, text: &str) {
        let Some(slot) = view.showcase.slot.as_mut() else {
            return;
        };
        slot.input = crate::native_text_input::TextInputState::new(text.to_string());
        let index = slot.index;
        Self::draft_palette_slot(view, index);
    }

    fn draft_palette_slot(view: &mut SettingsViewState, index: usize) {
        let Some(value) = view
            .showcase
            .slot
            .as_ref()
            .map(|slot| slot.input.value().trim().to_string())
        else {
            return;
        };
        let Some(color) = aterm_types::scheme::parse_hex(&value) else {
            return;
        };
        if let Some(entries) = palette_with(&view.palette_entries(), index, &hex_of(color)) {
            view.set_palette_draft(&entries);
        }
        view.invalidate_result_page_limit();
    }

    fn commit_palette(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        let list = view.field_has_unsaved_draft(prefs::EDIT_PALETTE);
        let slot = view.slot_override_draft();
        let slot_key = slot.map(|(index, _)| crate::resolved_colors::palette_override_key(index));
        if !list && slot.is_none() {
            return;
        }
        let mut edits = Vec::new();
        if list {
            let entries = view.palette_entries();
            edits.push(ConfigEdit {
                key: prefs::EDIT_PALETTE.to_string(),
                expected: ExpectedConfigValue::Exact(view.raw_value(prefs::EDIT_PALETTE)),
                value: (!entries.is_empty()).then(|| entries.join(", ")),
            });
        }
        if let (Some((_, color)), Some(key)) = (slot, slot_key) {
            edits.push(ConfigEdit {
                expected: ExpectedConfigValue::Exact(view.raw_value(&key)),
                key,
                value: Some(hex_of(color)),
            });
            if let Some(editor) = view.showcase.slot.as_mut() {
                editor.opened = color;
            }
        }
        let patch = ConfigPatch {
            base_revision: self.config_revision,
            edits,
        };
        Self::send_config(view, cx, patch);
        view.feedback = Some("Applying\u{2026}".to_string());
    }

    fn close_palette_slot(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        if let Some((index, color)) = view.slot_override_draft() {
            let key = crate::resolved_colors::palette_override_key(index);
            let patch = ConfigPatch {
                base_revision: self.config_revision,
                edits: vec![ConfigEdit {
                    expected: ExpectedConfigValue::Exact(view.raw_value(&key)),
                    key,
                    value: Some(hex_of(color)),
                }],
            };
            Self::send_config(view, cx, patch);
            view.feedback = Some("Applying\u{2026}".to_string());
        }
        view.showcase.slot = None;
    }

    fn reset_palette_slot(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        let Some(index) = view.showcase.slot.as_ref().map(|slot| slot.index) else {
            return;
        };
        let theme = view.theme_palette();
        let on_list = view.slot_on_list(index);
        let key = crate::resolved_colors::palette_override_key(index);
        let mut edits = Vec::new();
        if view.is_explicit(&key) {
            edits.push(ConfigEdit {
                expected: ExpectedConfigValue::Exact(view.raw_value(&key)),
                key,
                value: None,
            });
        }
        if on_list {
            let entries = palette_reset(&view.palette_entries(), index, &theme);
            let drafted = view.field_has_unsaved_draft(prefs::EDIT_PALETTE);
            view.set_palette_draft(&entries);
            if !drafted {
                edits.push(ConfigEdit {
                    key: prefs::EDIT_PALETTE.to_string(),
                    expected: ExpectedConfigValue::Exact(view.raw_value(prefs::EDIT_PALETTE)),
                    value: (!entries.is_empty()).then(|| entries.join(", ")),
                });
            }
        }
        if let Some(editor) = view.showcase.slot.as_mut() {
            editor.input = crate::native_text_input::TextInputState::new(hex_of(theme[index]));
            editor.opened = theme[index];
        }
        if !edits.is_empty() {
            let patch = ConfigPatch {
                base_revision: self.config_revision,
                edits,
            };
            Self::send_config(view, cx, patch);
            view.feedback = Some("Applying\u{2026}".to_string());
        }
        view.invalidate_result_page_limit();
    }

    fn reset_palette(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        view.field_inputs.remove(prefs::EDIT_PALETTE);
        if view.editing_field.as_deref() == Some(prefs::EDIT_PALETTE) {
            view.editing_field = None;
        }
        view.showcase.slot = None;
        let keys: Vec<&str> = [prefs::EDIT_PALETTE, prefs::EDIT_PALETTE_OVERRIDES]
            .into_iter()
            .filter(|key| view.is_explicit(key))
            .collect();
        if keys.is_empty() {
            return;
        }
        let edits = keys
            .into_iter()
            .map(|key| ConfigEdit {
                key: key.to_string(),
                expected: ExpectedConfigValue::Exact(view.raw_value(key)),
                value: None,
            })
            .collect();
        let patch = ConfigPatch {
            base_revision: self.config_revision,
            edits,
        };
        Self::send_config(view, cx, patch);
        view.feedback = Some("Resetting the palette\u{2026}".to_string());
        view.invalidate_result_page_limit();
    }

    /// Handle text editing events in the filter, save-name, or palette-slot fields.
    pub(super) fn reduce_showcase_text(
        &mut self,
        view: &mut SettingsViewState,
        target: ShowcaseText,
        event: TextInputEvent,
        cx: &mut UpdateCx<'_>,
    ) {
        let submit = matches!(event, TextInputEvent::Submit);
        let Some(input) = view.showcase_input_mut(target) else {
            return;
        };
        let before = input.value().to_string();
        apply_text_input_event(input, event);
        let changed = input.value() != before;
        match target {
            ShowcaseText::Filter => {
                if changed {
                    view.invalidate_result_page_limit();
                    view.scroll_to_results();
                }
                let listed = view.showcase.trial_id().is_some_and(|id| {
                    view.showcase
                        .visible()
                        .into_iter()
                        .any(|index| view.showcase.cards[index].id.eq_ignore_ascii_case(id))
                });
                if submit && listed {
                    self.keep_trial(view, cx);
                }
            }
            ShowcaseText::SaveName => {
                if changed && let Some(save) = view.showcase.save.as_mut() {
                    save.error = save_name_problem(save.name.value(), &view.assets.themes);
                }
                if submit {
                    self.save_as_theme(view, cx);
                }
            }
            ShowcaseText::PaletteSlot => {
                if changed && let Some(index) = view.showcase.slot.as_ref().map(|slot| slot.index) {
                    Self::draft_palette_slot(view, index);
                }
                if submit {
                    self.commit_palette(view, cx);
                }
            }
        }
        view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
        cx.repaint(crate::native_app::DamageRegion::All);
    }

    /// A pointer position in a page text field.
    pub(super) fn reduce_showcase_pointer(
        view: &mut SettingsViewState,
        action: &str,
        byte: usize,
        extend: bool,
        cx: &mut UpdateCx<'_>,
    ) -> bool {
        let Some(target) = ShowcaseText::for_key(action) else {
            return false;
        };
        if let Some(input) = view.showcase_input_mut(target) {
            apply_pointer_selection(input, byte, extend);
            view.common.last_focus = Some(UiKey::new(action));
            view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
            cx.repaint(crate::native_app::DamageRegion::All);
        }
        true
    }

    /// Text an assistive technology inserts into a page field.
    #[cfg(any(a11y_tree, test))]
    pub(super) fn insert_showcase_text(
        &mut self,
        view: &mut SettingsViewState,
        target: ShowcaseText,
        text: &str,
    ) {
        if let Some(input) = view.showcase_input_mut(target) {
            input.insert(text);
        }
        match target {
            ShowcaseText::Filter => {
                view.invalidate_result_page_limit();
                view.scroll_to_results();
            }
            ShowcaseText::SaveName => {
                if let Some(save) = view.showcase.save.as_mut() {
                    save.error = save_name_problem(save.name.value(), &view.assets.themes);
                }
            }
            ShowcaseText::PaletteSlot => {
                if let Some(index) = view.showcase.slot.as_ref().map(|slot| slot.index) {
                    Self::draft_palette_slot(view, index);
                }
            }
        }
    }
}

fn keeping_words(
    view: &SettingsViewState,
    id: &str,
    side: Option<aterm_types::Appearance>,
) -> String {
    let label = view.showcase.label_of(id);
    let following = prefs::theme_mode(view.raw_value(prefs::EDIT_THEME).as_deref())
        == prefs::ThemeMode::FollowSystem;
    match side {
        Some(side) => format!("Keeping {label} for {} mode\u{2026}", appearance_word(side)),
        None if following => format!("Keeping {label} for light and dark mode\u{2026}"),
        None => format!("Keeping {label}\u{2026}"),
    }
}

fn drop_carried_drafts(view: &mut SettingsViewState, drafts: &CarriedDrafts) {
    for (key, input) in &drafts.fields {
        if view.field_inputs.get(key) == Some(input) {
            view.field_inputs.remove(key);
            if view.editing_field.as_deref() == Some(key.as_str()) {
                view.editing_field = None;
            }
        }
    }
    if let Some((index, input)) = &drafts.slot
        && view
            .showcase
            .slot
            .as_ref()
            .is_some_and(|slot| slot.index == *index && slot.input == *input)
    {
        view.showcase.slot = None;
    }
    view.invalidate_result_page_limit();
}

fn text_width(text: &str, role: SemanticRole, style: StyleRef) -> f32 {
    let (px, face) = crate::native_ui::text_paint_metrics(role, style);
    crate::tray_raster::ui_text_width_for(face, text, px)
}

fn fit_text(text: &str, role: SemanticRole, style: StyleRef, width: f32) -> String {
    let (px, face) = crate::native_ui::text_paint_metrics(role, style);
    crate::native_ui::whole_word_cut(
        text,
        crate::native_ui::elide_text_label(text, width.max(0.0), px, face),
    )
}

fn line(
    key: impl Into<String>,
    text: &str,
    role: SemanticRole,
    style: StyleRef,
    width: f32,
    height: f32,
) -> UiNode {
    UiNode::new(
        key.into(),
        UiContent::Text(TextSpec {
            text: fit_text(text, role, style, width),
            role,
            style,
        }),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(height)),
    )
}

fn button(
    key: &str,
    label: &str,
    visual: &str,
    style: StyleRef,
    state: ControlState,
    width: Option<f32>,
) -> UiNode {
    let fitted = width.map_or_else(
        || visual.to_string(),
        |width| crate::native_ui::fit_native_button_label(visual, (width - 20.0).max(0.0)),
    );
    UiNode::new(
        key,
        UiContent::Button(
            Control::new(
                ButtonSpec::new(label).visual_label(fitted),
                ActionId::new(key),
            )
            .state(state)
            .style(style),
        ),
    )
    .layout(
        Layout::default()
            .width(width.map_or(Length::Fill, Length::Fixed))
            .height(Length::Fill),
    )
}

fn hug_width(label: &str) -> f32 {
    crate::native_ui::native_button_label_width(label) + 21.0
}

fn focused(state: &SettingsViewState, key: &str) -> bool {
    state
        .common
        .last_focus
        .as_ref()
        .is_some_and(|focus| focus.as_str() == key)
}

fn text_field(
    state: &SettingsViewState,
    key: &str,
    label: &str,
    placeholder: &str,
    input: &crate::native_text_input::TextInputState,
    swatch: Option<[u8; 3]>,
    invalid: bool,
) -> UiNode {
    let (value, _) = input.display_projection();
    UiNode::new(
        key,
        UiContent::TextField(
            Control::new(
                TextFieldSpec {
                    label: label.to_string(),
                    placeholder: (!placeholder.is_empty()).then(|| placeholder.to_string()),
                    secret: false,
                    visual_value: None,
                    input: Some(input.projection()),
                    swatch,
                    swatch_line: false,
                    trailing_hint: None,
                    description: None,
                    end_button: false,
                },
                ActionId::new(key),
            )
            .value(SemanticValue::Text(value))
            .state(ControlState {
                focused: focused(state, key),
                invalid,
                ..ControlState::default()
            })
            .style(StyleRef::Secondary),
        ),
    )
    .layout(Layout::default().width(Length::Fill).height(Length::Fill))
}

fn strip_button_height() -> f32 {
    28.0_f32.max(24.0 * settings_text_scale())
}

fn control_row(key: &str, label: &str, children: Vec<UiNode>) -> (UiNode, f32) {
    let height = customize::sheet_row_height();
    (
        UiNode::new(key, UiContent::Group(GroupSpec::new(label)))
            .layout(Layout::row().height(Length::Fixed(height)).gap(6.0))
            .children(children),
        height,
    )
}

/// A typography setting that would change if a tried theme is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TypeChange {
    pub(super) key: &'static str,
    /// Value the setting takes once the theme is kept.
    pub(super) theme: String,
    /// Config value that pins the current appearance (`None` when unpinnable).
    pub(super) pin: Option<String>,
}

/// What a tried theme's typography would do once kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TrialTypography {
    /// The type settings that would change, in table order.
    pub(super) changes: Vec<TypeChange>,
}

impl TrialTypography {
    /// Whether the theme changes typography and every change can be pinned.
    pub(super) fn keeps_colours_only(&self) -> bool {
        !self.changes.is_empty() && self.changes.iter().all(|change| change.pin.is_some())
    }

    fn why_not_colours_only(&self, state: &SettingsViewState) -> Option<String> {
        let unpinned: Vec<String> = self
            .changes
            .iter()
            .filter(|change| change.pin.is_none())
            .map(|change| type_label(state, change.key))
            .collect();
        (!unpinned.is_empty()).then(|| {
            format!(
                "Keep colours only is unavailable: no setting keeps the font's own {} \
                 apart from this theme",
                unpinned.join(", ").to_lowercase()
            )
        })
    }
}

impl SettingsViewState {
    fn theme_under_trial(&self) -> Option<String> {
        if let ShowcaseTrial::Keeping { under, .. } = &self.showcase.trial
            && let Some(before) = under.get(prefs::EDIT_THEME)
        {
            return before.clone();
        }
        self.raw_value(prefs::EDIT_THEME)
    }

    fn trial_under(&self, edits: &[ConfigEdit]) -> BTreeMap<String, Option<String>> {
        let mut under = match &self.showcase.trial {
            ShowcaseTrial::Keeping { under, .. } => under.clone(),
            ShowcaseTrial::Idle | ShowcaseTrial::Trying { .. } => BTreeMap::new(),
        };
        for edit in edits {
            let ExpectedConfigValue::Exact(before) = &edit.expected;
            under
                .entry(edit.key.clone())
                .or_insert_with(|| before.clone());
        }
        under
    }

    fn showcase_select_is(&self, operation: OperationId) -> bool {
        self.showcase
            .selecting
            .as_ref()
            .is_some_and(|(selecting, ..)| *selecting == operation)
    }

    fn typography_presence_config(&self, theme: Option<String>) -> Config {
        let mut config = Config {
            theme,
            ..Config::default()
        };
        let set = |key: &str| self.is_explicit(key);
        if set(prefs::EDIT_FONT_FAMILY) {
            config.font_family = Some(String::new());
        }
        let text = |key: &str| set(key).then(String::new);
        config.font_family_bold = text(prefs::EDIT_FONT_FAMILY_BOLD);
        config.font_family_italic = text(prefs::EDIT_FONT_FAMILY_ITALIC);
        config.font_family_bold_italic = text(prefs::EDIT_FONT_FAMILY_BOLD_ITALIC);
        config.symbol_font = text(prefs::EDIT_SYMBOL_FONT);
        config.emoji_font = text(prefs::EDIT_EMOJI_FONT);
        config.display_font = self.raw_value("display_font");
        if set(prefs::EDIT_FALLBACK_FONTS) {
            config.fallback_fonts = Some(crate::app_config::FontList(Vec::new()));
        }
        if set(prefs::EDIT_FONT_FEATURES) {
            config.font_features = Some(Vec::new());
        }
        config.line_height = set(prefs::EDIT_LINE_HEIGHT).then_some(1.0);
        config.font_size_scale = set(prefs::EDIT_FONT_SIZE_SCALE).then_some(1.0);
        config.ligatures = set(prefs::EDIT_LIGATURES).then_some(true);
        config.font_synthetic_style = set(prefs::EDIT_FONT_SYNTHETIC_STYLE).then_some(true);
        config.font_weight = set(prefs::EDIT_FONT_WEIGHT).then_some(400);
        config
    }

    fn type_value_under(
        &self,
        key: &'static str,
        typo: &crate::resolved_typography::ResolvedTypography,
    ) -> (String, Option<String>) {
        if let Some(value) = prefs::theme_value_for(&Config::default(), typo, key) {
            return (type_value_words(key, &value), Some(value));
        }
        self.default_type_value(key)
    }

    fn default_type_value(&self, key: &'static str) -> (String, Option<String>) {
        match key {
            prefs::EDIT_FONT_FAMILY => {
                let name = self
                    .font_families
                    .as_deref()
                    .and_then(|families| families.automatic.clone())
                    .unwrap_or_else(|| "aterm's own face".to_string());
                (name, Some(String::new()))
            }
            prefs::EDIT_FONT_FAMILY_BOLD
            | prefs::EDIT_FONT_FAMILY_ITALIC
            | prefs::EDIT_FONT_FAMILY_BOLD_ITALIC
            | prefs::EDIT_SYMBOL_FONT
            | prefs::EDIT_EMOJI_FONT
            | prefs::EDIT_FALLBACK_FONTS => ("aterm's own choice".to_string(), Some(String::new())),
            prefs::EDIT_FONT_FEATURES => ("none".to_string(), Some("[]".to_string())),
            prefs::EDIT_LIGATURES | prefs::EDIT_FONT_SYNTHETIC_STYLE => {
                ("on".to_string(), Some("true".to_string()))
            }
            prefs::EDIT_FONT_WEIGHT => ("the font's own".to_string(), None),
            prefs::EDIT_LINE_HEIGHT => {
                let milli = (crate::app_config::DEFAULT_LINE_HEIGHT * 1000.0).round() as u16;
                let number = aterm_types::scheme_typography::milli_text(milli);
                (number.clone(), Some(number))
            }
            _ => ("1.0".to_string(), Some("1.0".to_string())),
        }
    }

    /// Typography changes that keeping `id` would introduce relative to the current state.
    pub(super) fn trial_typography(&self, id: &str) -> TrialTypography {
        let (value, _) = self.keep_value(id);
        let side = theme_visible_side(self, self.presented_motion.get().system_dark);
        let flag = self
            .environment_override(prefs::EDIT_FONT_FAMILY)
            .map(|active| active.effective.clone());
        let resolve = |theme: Option<String>| {
            self.typography_presence_config(theme)
                .resolved_typography_with_flag(side, &self.assets.themes, flag.as_deref())
        };
        let tried = resolve(value);
        let base = self.theme_under_trial();
        let configured = resolve(base.clone());
        let base_typography = Config {
            theme: base,
            ..Config::default()
        }
        .resolved_typography_with_flag(side, &self.assets.themes, None);
        let mut out = TrialTypography::default();
        for spec in aterm_types::scheme::THEME_KEYS
            .iter()
            .filter(|spec| spec.group == aterm_types::scheme::ThemeKeyGroup::Typography)
        {
            let theme = match tried.applied.value_text(spec.key) {
                Some(theme) => theme,
                None if configured.applied.value_text(spec.key).is_some() => {
                    self.default_type_value(spec.name).1.unwrap_or_default()
                }
                None => continue,
            };
            let (current, pin) = self.type_value_under(spec.name, &base_typography);
            if current != type_value_words(spec.name, &theme) {
                out.changes.push(TypeChange {
                    key: spec.name,
                    theme,
                    pin,
                });
            }
        }
        out
    }
}

fn type_value_words(key: &str, value: &str) -> String {
    match (key, value.trim()) {
        (prefs::EDIT_LIGATURES | prefs::EDIT_FONT_SYNTHETIC_STYLE, "true") => "on".to_string(),
        (prefs::EDIT_LIGATURES | prefs::EDIT_FONT_SYNTHETIC_STYLE, "false") => "off".to_string(),
        (_, value) => value.to_string(),
    }
}

fn type_label(state: &SettingsViewState, key: &str) -> String {
    state
        .field_by_key(key)
        .map_or_else(|| key.to_string(), |field| field.label.to_string())
}
