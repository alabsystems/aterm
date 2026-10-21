// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SETTINGS ▸ THEMES: one job, show your terminal in another theme, then
//! keep it or go back.
//!
//! The page is three parts ([`view`] lays them out):
//!
//! * **The chooser** — one 32pt row per theme ([`ShowcaseCard`], built once
//!   per catalog generation): aterm's signature themes first, then the other
//!   built-ins, the person's own files, and the community pack behind one
//!   disclosure. A filter matches a name, id, description, `dark`/`light`,
//!   `yours`, `font` or `community`. The row the keyboard or the pointer
//!   picks is the trial: highlight equals trial, and the theme in use is no
//!   trial. Hover only tints.
//! * **The demo** — a real terminal parser and the shipping renderer playing
//!   a scripted shell session ([`crate::settings_theme_demo`]) in the
//!   candidate theme, its window chrome in the theme's chrome tokens.
//! * **The strip** — one row under the demo, always reserved, offering one
//!   obvious action per moment: Customize while idle, Revert and Keep while a
//!   theme is tried, Done while the Customize sheet is open.
//!
//! TRYING A THEME. The view records the trial ([`ShowcaseTrial::Trying`]) and
//! the host draws it everywhere through the transient preview lane
//! ([`crate::App::sync_settings_theme_trial`]), writing nothing; a community
//! pack theme is drawn from the embedded pack, so no file is written either.
//! Keep writes `theme` once through the patch lane (while the theme follows
//! the system, only one side: the one the person chose in the segment, else
//! the one the kept theme is made for, which the segment shows meanwhile);
//! a pack theme's Keep
//! adds its file first and writes `theme` once the catalog lists it. Revert,
//! Esc, leaving the page, Search Results or the category list replacing it,
//! and closing Settings all end the trial the same way: no view asks for it
//! any more, so the host reverts.
//!
//! THE CUSTOMIZE SHEET ([`customize`]) replaces the chooser: the five colour
//! overrides with their Resets, the sixteen palette slots, and "Save as
//! theme…", which writes the colours the demo shows as a new theme file
//! through the host's one theme write path, then — once the catalog lists it
//! — selects it and clears the overrides it now carries, in one patch.

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
pub(super) use view::{demo_font_candidate, demo_spec, themes_page};
#[cfg(test)]
pub(super) use view::{list_blocks, section_items};

/// Every action the page mints starts with this.
pub(super) const SHOWCASE_ACTION_PREFIX: &str = "settings/showcase/";
/// The chooser's filter field (its node key and its action).
pub(super) const SHOWCASE_FILTER: &str = "settings/showcase/filter";
/// The filter's Clear, over its end while it holds a query.
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
/// A row: `settings/showcase/card/<slug>`.
const CARD_PREFIX: &str = "settings/showcase/card/";
/// The Keep split button's chevron: shows Keep colours only.
const KEEP_MORE: &str = "settings/showcase/keep-more";
/// Open the Customize sheet, and close it.
const CUSTOMIZE: &str = "settings/showcase/customize";
const CUSTOMIZE_DONE: &str = "settings/showcase/customize-done";
/// The community pack's disclosure.
const PACK: &str = "settings/showcase/pack";
/// Follow system light/dark (Appearance's switch).
const FOLLOW_SYSTEM: &str = "settings/showcase/follow-system";
/// The Dark | Light segment while the theme follows the system:
/// `settings/showcase/side/<dark|light>`.
const SIDE_PREFIX: &str = "settings/showcase/side/";
const SAVE_CONFIRM: &str = "settings/showcase/save";
const SAVE_CANCEL: &str = "settings/showcase/save-cancel";
const IMPORT: &str = "settings/showcase/import";
const OPEN_FOLDER: &str = "settings/showcase/open-folder";
/// A palette slot's chip: `settings/showcase/palette/<0..15>`.
const PALETTE_PREFIX: &str = "settings/showcase/palette/";
const PALETTE_SLOT_RESET: &str = "settings/showcase/palette-slot-reset";
const PALETTE_SLOT_CLOSE: &str = "settings/showcase/palette-slot-close";
/// Reset every palette slot: the `palette` list and the `palette_overrides`
/// table.
const PALETTE_RESET_ALL: &str = "settings/showcase/palette-reset";
/// A Yours row's file actions: `settings/showcase/card/<slug>/<verb>`.
const CARD_OPEN: &str = "open";
const CARD_TRASH: &str = "trash";

/// The page's heading.
const SHOWCASE_TITLE: &str = "Themes";
/// The room a scrolling column keeps clear at its trailing edge for the
/// list's scroll thumb, which paints 3pt wide at 7pt in.
pub(super) const THUMB_GUTTER: f32 = 12.0;

/// The colour overrides the Customize sheet edits, in the order it lists
/// them (`COLOR_KEYS` also holds the cursor trail's two colours, which a
/// theme does not set).
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
    /// `id` is drawn everywhere in place of the configured theme; nothing
    /// is written.
    Trying { id: String },
    /// Keep wrote `theme` for `id`; the trial stands until that write lands
    /// (or is refused, which goes back to Trying).
    Keeping { id: String, operation: OperationId },
}

/// The open Save as theme row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SaveAsState {
    pub(crate) name: crate::native_text_input::TextInputState,
    /// Why the typed name cannot be saved, as it is typed (or why the last
    /// write failed).
    pub(crate) error: Option<String>,
    /// "Include type settings": the file also carries the Customize view's
    /// type rows (family, line height, ligatures, weight, size scale; never
    /// `font_px`). Off by default.
    pub(crate) include_type: bool,
}

/// The type rows Save as theme can carry ("Include type settings"):
/// Customize's type settings a theme file has a key for (never `font_px`).
pub(super) const SAVE_TYPE_KEYS: [&str; 5] = [
    prefs::EDIT_FONT_FAMILY,
    prefs::EDIT_LINE_HEIGHT,
    prefs::EDIT_LIGATURES,
    prefs::EDIT_FONT_WEIGHT,
    prefs::EDIT_FONT_SIZE_SCALE,
];

/// The drafts a saved theme carries, as they were when it was saved: they
/// stay drafts until the theme is selected, then go (each only while it
/// still holds the saved value, so a later edit survives).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CarriedDrafts {
    /// Each drafted registry field exactly as submitted (text, selection
    /// and history): retyping the same text is a newer draft.
    fields: BTreeMap<String, crate::native_text_input::TextInputState>,
    /// The palette slot editor exactly as submitted: a later same-colour
    /// edit can carry newer selection and undo state too.
    slot: Option<(usize, crate::native_text_input::TextInputState)>,
}

/// A saved theme waiting for the write, then for the catalog to list it,
/// then to be selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingSelect {
    id: String,
    operation: OperationId,
    /// The file write must report its own successful publication before a
    /// catalog observation may trigger the config patch.
    saved: bool,
    /// The admitted intent, including exact raw values from the Save click.
    /// A later snapshot may observe it, never rebase it onto newer edits.
    theme: ConfigEdit,
    side: Option<aterm_types::Appearance>,
    /// The override keys the saved theme now carries: cleared with the
    /// selection, so they are not applied twice.
    clears: Vec<ConfigEdit>,
    drafts: CarriedDrafts,
}

/// The palette slot being edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SlotEditor {
    pub(crate) index: usize,
    pub(crate) input: crate::native_text_input::TextInputState,
    /// The colour the slot shows without this editor's draft: what the field
    /// opened with, or what it last committed.
    opened: aterm_types::Rgb,
}

/// A community pack theme being kept: the add request, the theme id, and
/// whether its file is written. Once written and listed by the catalog, Keep
/// writes `theme`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingKeep {
    pub(crate) operation: OperationId,
    pub(crate) id: String,
    pub(crate) written: bool,
}

/// What the strip's live region settled on when a trial ended, and the theme
/// those words name. The words stand only while that theme is the one in use
/// on the side the page shows: an Undo, a save's select, the Follow switch,
/// the Dark | Light segment, a hand edit or the system's appearance can put
/// another theme in use without a trial, and the strip then says that one.
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

/// Settings ▸ Themes' view state. View-local, never config.
#[derive(Debug)]
pub(crate) struct ShowcaseState {
    pub(crate) filter: crate::native_text_input::TextInputState,
    /// The Customize sheet stands in the chooser's place.
    pub(crate) sheet_open: bool,
    /// The community pack's rows are shown (a filter shows its matches
    /// whether or not).
    pub(crate) pack_open: bool,
    /// The Keep split button's one-item list (Keep colours only) is open.
    pub(crate) keep_more: bool,
    /// The chooser's rows, built from the catalog this view admitted.
    pub(crate) cards: Arc<[ShowcaseCard]>,
    /// The theme files that did not load, for NEEDS ATTENTION.
    pub(crate) attention: Arc<[AttentionRow]>,
    pub(crate) trial: ShowcaseTrial,
    /// The side the theme the highlight last moved to is made for, by the
    /// catalog: while the theme follows the system and the person has chosen
    /// no side in the Dark | Light segment, the side the page shows and Keep
    /// writes ([`SettingsViewState::showcase_highlight_side`]), so a theme is
    /// filed by the side it is made for, never by the OS's. `None` (the OS
    /// side) until the highlight moves, after a Revert, and for a theme the
    /// catalog does not know.
    pub(crate) highlight_side: Option<aterm_types::Appearance>,
    /// The palette slot being edited.
    pub(crate) slot: Option<SlotEditor>,
    pub(crate) save: Option<SaveAsState>,
    pub(crate) pending_select: Option<PendingSelect>,
    /// A community theme's Keep, waiting for its file and the catalog.
    pub(crate) pending_keep: Option<PendingKeep>,
    /// The select patch a save sent, and the drafts it carries: they go
    /// when it lands.
    selecting: Option<(OperationId, String, CarriedDrafts)>,
    /// The Yours row whose Trash was pressed once: a second press moves it.
    pub(crate) trash_armed: Option<String>,
    /// Why the last Keep (or a community theme's add) did not land: the
    /// strip's second line, in the danger ink, until the next trial.
    pub(crate) error: Option<String>,
    /// What the strip's live region last settled on ("Kept Dracula",
    /// "Reverted to Aterm Night"), until the next trial starts or the theme
    /// in use is no longer the one it names ([`Settled`]).
    pub(crate) settled: Option<Settled>,
    /// The rows the chooser lists, in order, as the last render laid them
    /// out: the arrows, Home, End and the page keys walk them.
    order: RefCell<Vec<String>>,
    /// The rows a screen of the list holds, as the last render laid it out
    /// (Page Up and Page Down move by it).
    page_rows: Cell<usize>,
    /// The least a theme's name keeps beside what shares its line, as last
    /// measured: for which rows (`cards`, by address), at which text scale and
    /// in which face (the strip's title, a row's name), so a frame measures
    /// every name once, not once a layout ([`view::name_floor`]).
    pub(super) name_floors: RefCell<Vec<NameFloor>>,
    /// The animation clock's reading when the page opened: the demo's
    /// phase counts from it, so a theme change never restarts the tape.
    pub(crate) demo_epoch_ms: Cell<Option<u64>>,
    /// The clock's reading at the last input on the page: three loops after
    /// it the demo rests on its settled frame.
    pub(crate) last_input_phase_ms: Cell<u64>,
    /// An input arrived since the last render read the clock (reducers have
    /// none): the next render stamps it.
    pub(crate) input_pending: Cell<bool>,
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
            demo_epoch_ms: Cell::new(None),
            last_input_phase_ms: Cell::new(0),
            input_pending: Cell::new(true),
        }
    }

    /// The filter's query, trimmed and lowercased.
    pub(crate) fn query(&self) -> String {
        self.filter.value().trim().to_lowercase()
    }

    /// The rows the filter admits, as indices into `cards`, in list order.
    /// Community rows are admitted while the disclosure is open or a query
    /// is typed.
    pub(crate) fn visible(&self) -> Vec<usize> {
        self.visible_for(&self.query())
    }

    /// The rows `query` (trimmed, lowercase) admits: [`Self::visible`] for
    /// that filter.
    pub(crate) fn visible_for(&self, query: &str) -> Vec<usize> {
        self.cards
            .iter()
            .enumerate()
            .filter(|(_, card)| {
                (card.section != Section::Community || self.pack_open || !query.is_empty())
                    && card.matches(query)
            })
            .map(|(index, _)| index)
            .collect()
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

    /// The id the trial is about, while there is one.
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

/// Which of the page's own text fields the keyboard is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShowcaseText {
    Filter,
    SaveName,
    PaletteSlot,
}

impl SettingsViewState {
    /// Whether the Themes page is the surface on glass: its route, with no
    /// Search Results and no compact category list replacing it. Its trial,
    /// previews and Escape steps belong to it only then; another surface
    /// replacing it ends them ([`SettingsViewState::page_replaced`]).
    pub(super) fn showcase_on_glass(&self) -> bool {
        self.route == SettingsRoute::Themes
            && self.search.trim().is_empty()
            && !self.compact_navigation
    }

    /// THE SIDE OF THE THEME THE HIGHLIGHT IS ON, while the Themes page is
    /// on glass and the theme follows the system: the side the page shows
    /// where the person chose none ([`theme_visible_side`]) — the segment
    /// shows it, the strip says it ("for Light") and Keep writes it, so
    /// keeping a light theme while the OS is dark files it under Light (a
    /// theme is filed by the side it is made for). Over one theme there is no
    /// side to file under, and the OS's stays the side shown.
    pub(super) fn showcase_highlight_side(&self) -> Option<aterm_types::Appearance> {
        let following = prefs::theme_mode(self.raw_value(prefs::EDIT_THEME).as_deref())
            == prefs::ThemeMode::FollowSystem;
        (self.showcase_on_glass() && following)
            .then_some(self.showcase.highlight_side)
            .flatten()
    }

    /// The side a theme `id` is made for (declared or measured), as the
    /// catalog admitted it, else as the chooser's row for it says (a
    /// community theme not yet added); `None` for one neither knows.
    fn theme_made_for(&self, id: &str) -> Option<aterm_types::Appearance> {
        self.assets
            .themes
            .meta(id.trim())
            .map(|meta| meta.appearance)
            .or_else(|| self.showcase.card(id.trim()).map(|card| card.appearance))
    }

    /// What this view asks of the live theme: the theme it tries, or keeps
    /// drawn while its Keep write is in flight. `None` off the page, except
    /// a Keep in flight, which the host holds until its write lands.
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

    /// The host could not draw the tried theme `id`: the trial ends and the
    /// status line says why (the strip is back to Idle).
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

    /// The page's rows follow a new catalog; a trial whose theme no longer
    /// draws ends, and says why: its file is gone (and the community pack
    /// has none by that name), or its file no longer loads.
    pub(super) fn refresh_showcase(&mut self) {
        let cards: Arc<[ShowcaseCard]> = showcase_cards(&self.assets.themes).into();
        self.showcase.attention =
            crate::settings_preview::theme_card::attention_rows(&self.assets.themes, &cards).into();
        self.showcase.cards = cards;
        self.showcase.name_floors.borrow_mut().clear();
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

    /// A ROW THAT LEAVES THE LIST TAKES NO KEYBOARD WITH IT: where the
    /// keyboard is on a row (or its Open or Trash) that the new catalog no
    /// longer lists — its file moved to the Trash here, or gone from the
    /// folder — it goes to the row that followed it as the last render
    /// listed them, else the one before it, else the highlighted row, and
    /// that row is shown. Never on nothing: a key no tree holds makes the
    /// next Tab start over at the window's first control.
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
            // A row's file actions are a Yours row's (or a NEEDS ATTENTION
            // row's Open).
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

    /// What [`Self::follow_showcase_change`] compares: the row highlighted
    /// before the configuration or the catalog changes.
    pub(super) fn showcase_highlight_before_change(&self) -> Option<String> {
        (self.route == SettingsRoute::Themes)
            .then(|| self.highlighted_slug())
            .flatten()
    }

    /// The configuration or the catalog changed under the page without a
    /// trial (an Undo or Redo, a save's select, the Follow switch, a hand
    /// edit, a rescan): the strip's settled words go once they no longer name
    /// the theme in use, and a keyboard that was on the highlighted row
    /// (`highlighted_before`) goes with the highlight to the row of the theme
    /// now in use, shown — the keyboard never stays on a row the highlight
    /// has left.
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

    /// Leaving the page: the trial ends (the host reverts it) and the page's
    /// transient state goes. The filter and the disclosure stay, so a return
    /// finds the list as it was left; the demo starts its tape again.
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

    /// End the trial: the highlight goes back to the theme in use, and the
    /// live region says so once.
    pub(super) fn revert_showcase(&mut self) {
        self.showcase.pending_keep = None;
        self.showcase.keep_more = false;
        if matches!(self.showcase.trial, ShowcaseTrial::Trying { .. }) {
            self.showcase.trial = ShowcaseTrial::Idle;
            // Back to what the terminal draws: the side the OS picks, unless
            // the person chose one.
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

    /// Escape on the page, first applicable step: a Trash waiting for its
    /// second press, the palette slot editor or the save name (its draft let
    /// go), the Customize sheet, the filter's text (while the keyboard is in
    /// it), a save or community add still waiting, the trial. Whether it took
    /// the Escape; otherwise the page is left the normal way.
    pub(super) fn cancel_showcase(&mut self) -> bool {
        // Escape aimed at Search Results or the category list is theirs.
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
            self.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
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
        false
    }

    /// Close the Customize sheet: the chooser comes back, the keyboard on
    /// Customize, the list on the highlighted row ([`Self::showcase_landing`]).
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

    /// THE CHOOSER COMES ON GLASS ON ITS HIGHLIGHTED ROW — Settings ▸ Themes
    /// opening, a return to it from another page, Search Results or the
    /// category list, the Customize sheet closing: the row of the theme in
    /// use (or the one being tried) is where the list lands, whole, as Cursor
    /// Styles lands on the saved style's row. The list's first screen used to
    /// leave it wherever it fell: off the screen on a short window, or cut
    /// by the chooser's bottom edge (GitHub Light at 220×60). `None` where
    /// the list does not show that row (the filter hides it, or the closed
    /// Community holds it), so no landing waits to pull the list later.
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
        match self.common.last_focus.as_ref()?.as_str() {
            SHOWCASE_FILTER if !self.showcase.sheet_open => Some(ShowcaseText::Filter),
            SHOWCASE_SAVE_NAME if self.showcase.save.is_some() => Some(ShowcaseText::SaveName),
            SHOWCASE_PALETTE_SLOT if self.showcase.slot.is_some() => {
                Some(ShowcaseText::PaletteSlot)
            }
            _ => None,
        }
    }

    fn showcase_input_mut(
        &mut self,
        target: ShowcaseText,
    ) -> Option<&mut crate::native_text_input::TextInputState> {
        match target {
            ShowcaseText::Filter => Some(&mut self.showcase.filter),
            ShowcaseText::SaveName => self.showcase.save.as_mut().map(|save| &mut save.name),
            ShowcaseText::PaletteSlot => self.showcase.slot.as_mut().map(|slot| &mut slot.input),
        }
    }

    /// The theme the page draws in place of the configured one: the trial.
    /// Hover never previews (highlight equals trial).
    pub(super) fn showcase_preview_id(&self) -> Option<String> {
        if !self.showcase_on_glass() {
            return None;
        }
        self.showcase.trial_id().map(str::to_string)
    }

    /// The theme the demo draws: the trial, else the configured theme on the
    /// side the page shows (never the raw `dark:…,light:…` value).
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

    /// Whether Up, Down, Home, End and the page keys move the chooser's
    /// highlight: the keyboard is on a row, or in the filter.
    pub(super) fn showcase_steps_highlight(&self) -> bool {
        self.showcase_focused_card().is_some()
            || self.showcase_pack_focused()
            || self.showcase_text_focus() == Some(ShowcaseText::Filter)
    }

    /// Move the highlight to `id`: that theme is tried, or, when it is the
    /// theme in use, the trial ends. Nothing is written, and a saved theme
    /// waiting to be selected no longer is: the person chose another row.
    /// The live region says it once, after the frame's install. Whether the
    /// highlight moved (it stays while a Keep is landing).
    pub(super) fn highlight_card(&mut self, id: &str) -> bool {
        self.showcase.note_input();
        if matches!(self.showcase.trial, ShowcaseTrial::Keeping { .. }) {
            self.feedback = Some("Keeping the theme you tried\u{2026}".to_string());
            return false;
        }
        self.showcase.pending_select = None;
        self.showcase.keep_more = false;
        self.showcase.error = None;
        self.showcase.pending_keep = None;
        // The page shows the side the highlighted theme is made for (where
        // the person chose none), so the theme in use there is what it is
        // compared with: the light side's own theme, highlighted on a dark
        // OS, is in use, not a trial.
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

    /// Up and Down (`delta` ±1), Page Up and Page Down (± a screen), Home and
    /// End (± everything) in the chooser: the highlight moves along the rows
    /// the list shows, trying the one it lands on, and the keyboard goes with
    /// it — unless it is in the filter, where it stays. From the Community
    /// disclosure it moves as from between the rows over it and its own, so
    /// the keyboard never lands on a row the highlight is not on.
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
                // Closed: no row is under it, so Down leaves it as it
                // leaves any control in the list.
                let _ = self.step_list_focus(1);
                return true;
            }
            // The disclosure sits at `first - ½`: Down lands on `first`, Up
            // on the row over it.
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
            // The keyboard stays with the highlight on the row being kept.
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

    /// THE THEME LIST IS ONE TAB STOP (the list-box pattern its roles
    /// claim): Tab (`forward`) or Shift-Tab from `current` toward `next`,
    /// where `next` is a theme row or a Yours row's file action, enters the
    /// run of rows it belongs to at the highlighted row, and from inside
    /// the run leaves it in one step for the control past its far end; the
    /// highlighted row's own Open and Trash stay on the way. A run that does
    /// not hold the highlighted row, while another does, is passed over;
    /// where none does (the filter hides it), a run is entered at its first
    /// row. `order` is the window's Tab order, for leaving a run the list
    /// ends with. The arrows move between rows. `None` keeps `next`.
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
        // The card a row or a Yours row's file action belongs to.
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
                // The list ends with this run: the window's next stop.
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
            // The highlighted row's own file actions are on the way out.
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

    /// PAGE UP AND PAGE DOWN TURN THE CHOOSER BY THE SCREEN IT DRAWS: how
    /// far along the list's rows (`direction` +1, Page Down, or -1) the
    /// highlight moves ([`Self::step_highlight`]), measured on the screens of
    /// the list the last render seated. Page Down lands on the last row whole
    /// on the screen whose first stop is the one after the keyboard's, so
    /// every row between is whole on the screen it lands on; Page Up on the
    /// first row of the screen that ends at the stop before it; where that
    /// screen holds no row, the next row. `0` where no row lies that way:
    /// the page keys then turn the list's page past its rows, as every
    /// list's do. (A step of the list's room in row heights skipped a row
    /// wherever a screen repeated a section's caption over its first row: at
    /// 102×29 One Dark was only ever a 1pt peek.) Where no screen was
    /// measured, the rows a screen holds.
    pub(super) fn showcase_page_step(&self, direction: isize) -> isize {
        let direction = direction.signum();
        let fallback = direction * self.showcase_page_rows() as isize;
        let Some(plan) = self.scroll_plan() else {
            return fallback;
        };
        let order = self.showcase.order.borrow();
        let row_stop =
            |index: usize| plan.stop_of(&UiKey::new(format!("{CARD_PREFIX}{}", order[index])));
        // Where the step counts from: the keyboard's stop, and the place
        // among the rows the highlight moves from — the keyboard's row, or,
        // from the Community disclosure, between the rows over it and its
        // own, as [`Self::step_highlight`] reads it.
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

    /// Open the Community disclosure when `key` is the row of a community
    /// theme it holds closed: `true` when it did.
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

    /// The theme the configuration draws on the side the page shows (what
    /// Keep replaces, and what a card marked Current names).
    pub(super) fn configured_theme_id(&self) -> String {
        let side = theme_visible_side(self, self.presented_motion.get().system_dark);
        let config = Config {
            theme: self.raw_value(prefs::EDIT_THEME).or_else(|| {
                self.environment_override(prefs::EDIT_THEME)
                    .map(|active| active.effective.clone())
            }),
            ..Config::default()
        };
        config.resolved_colors(side, &self.assets.themes, None).id
    }

    /// The value Keep writes for `id`, by the rule for a choice whose side is
    /// named ([`prefs::theme_choice_value`]): over one theme, the name; over a
    /// Follow-system split, the split with only one side replaced — the side
    /// the person chose in the page's Dark | Light segment (as a slot
    /// picker's side is theirs), else the side `id` is made for (a theme is
    /// filed by the side it is made for: a light theme kept while the OS is
    /// dark goes to Light), else, for a theme the catalog does not know, the
    /// side the OS picks. The segment shows that same side while the
    /// highlight is on `id` ([`Self::showcase_highlight_side`]).
    fn keep_value(&self, id: &str) -> (Option<String>, Option<aterm_types::Appearance>) {
        let raw = self.raw_value(prefs::EDIT_THEME);
        let side = chosen_theme_side(self)
            .or_else(|| self.theme_made_for(id))
            .unwrap_or_else(|| os_theme_side(self.presented_motion.get().system_dark));
        let (value, side) =
            prefs::theme_choice_value(raw.as_deref(), side, id, &self.assets.themes);
        (Some(value), side)
    }

    /// The colours the page's demo draws: the trial
    /// (else the configured theme) under every override and draft.
    pub(super) fn showcase_colors(&self) -> crate::resolved_colors::ResolvedColors {
        let system_dark = self.presented_motion.get().system_dark;
        preview_resolved_colors_with(self, system_dark, self.showcase.trial_id())
    }

    /// The type settings the saved `theme` names for the side the page shows
    /// (never a trial: Try is colour-only). Resolved over the theme alone, so
    /// `declared` and `applied` agree; a row the config sets beats it there.
    pub(crate) fn theme_typography(&self) -> crate::resolved_typography::ResolvedTypography {
        let side = theme_visible_side(self, self.presented_motion.get().system_dark);
        let config = Config {
            theme: self.raw_value(prefs::EDIT_THEME),
            ..Config::default()
        };
        config.resolved_typography_with_flag(side, &self.assets.themes, None)
    }

    /// The text size scale the saved theme applies where the config sets none
    /// (`1.0` when it applies none).
    pub(crate) fn theme_typography_scale(&self) -> f32 {
        self.theme_typography()
            .applied
            .font_size_scale_milli
            .map_or(1.0, |milli| f32::from(milli) / 1000.0)
    }

    /// The theme's own sixteen colours under the stage, without any palette
    /// override: what a palette slot resets to.
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

    /// The palette entries as authored or drafted (`#rrggbb`, in slot
    /// order; may be shorter or longer than sixteen).
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

    /// Record `entries` as the palette draft (the comma form the save path
    /// writes), and make it the edited field so close and carry see it.
    fn set_palette_draft(&mut self, entries: &[String]) {
        let text = entries.join(", ");
        self.field_inputs.insert(
            prefs::EDIT_PALETTE.to_string(),
            crate::native_text_input::TextInputState::new(text),
        );
        self.invalidate_result_page_limit();
    }

    /// Whether slot `index` is written in the person's positional `palette`
    /// list (else it is a `palette_overrides` slot).
    fn slot_on_list(&self, index: usize) -> bool {
        self.palette_entries().len() > index
    }

    /// The slot editor's draft for a `palette_overrides` slot: the colour
    /// typed, while it parses and differs from what the slot shows without it.
    fn slot_override_draft(&self) -> Option<(usize, aterm_types::Rgb)> {
        let slot = self.showcase.slot.as_ref()?;
        if self.slot_on_list(slot.index) {
            return None;
        }
        aterm_types::scheme::parse_hex(slot.input.value().trim())
            .filter(|color| *color != slot.opened)
            .map(|color| (slot.index, color))
    }

    /// The `palette_overrides` table the stage resolves with: the slots
    /// `aterm.toml` sets, and the open slot's draft over them. `None` when
    /// there are none.
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

    /// The theme the configuration draws on each side: a theme either side
    /// uses is in use.
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

/// The slug a card key names (`settings/showcase/card/<slug>`), not one of
/// its file actions' keys.
fn card_slug_of(key: &UiKey) -> Option<&str> {
    key.as_str()
        .strip_prefix(CARD_PREFIX)
        .filter(|slug| !slug.contains('/'))
}

/// `entries` with slot `index` set to `value`, when the list reaches that
/// slot. A slot past the list's end is not written here: it is a
/// `palette_overrides` slot, so the list never grows to reach it (growing it
/// would copy today's theme into every slot before it).
pub(super) fn palette_with(entries: &[String], index: usize, value: &str) -> Option<Vec<String>> {
    let mut out = entries.to_vec();
    *out.get_mut(index)? = value.to_string();
    Some(out)
}

/// `entries` with slot `index` back on the theme's colour, and every
/// trailing entry that equals the theme's dropped (an empty list removes the
/// key). A slot before others the person set stays written, as the theme's
/// colour: positions in the list cannot have holes.
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

/// The palette slot a `palette_overrides.<index>` key tunes, by its index
/// and ANSI name ("Palette colour 4, blue"), as its chip is spoken; `None`
/// for any other key.
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

/// The first `<label> Custom`, `<label> Custom 2`, … no theme answers to.
/// A label that is itself such a name (a theme saved here, customized
/// again) counts from its own base: `Dracula Custom` suggests
/// `Dracula Custom 2`, never `Dracula Custom Custom`.
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

/// The native theme file Save as theme writes for `resolved` under `name`,
/// and the overrides it carries (to clear once it is selected). The text is
/// read back before it is returned: what the page offers to write parses to
/// exactly the colours it shows.
pub(super) fn save_as_text(
    resolved: &crate::resolved_colors::ResolvedColors,
    name: &str,
    from_label: &str,
    typography: &aterm_types::scheme::ThemeTypography,
) -> Result<String, String> {
    let mut scheme = resolved.scheme.clone();
    // The name as the file spells it (`aterm theme new` and import do the
    // same): the writer drops a space before `#` or `;` and caps the length,
    // and the read-back below compares the scheme whole. The file itself is
    // still named by the id the person typed.
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
    /// The type settings "Include type settings" writes: each of
    /// [`SAVE_TYPE_KEYS`] as the page shows it in force (a draft, the saved
    /// value, else the current theme's), when a theme file can carry it
    /// (a font named by family, not a path; a number in the theme's range).
    pub(super) fn save_typography(&self) -> aterm_types::scheme::ThemeTypography {
        let mut typography = aterm_types::scheme::ThemeTypography::NONE;
        for key in SAVE_TYPE_KEYS {
            let drafted = preview_field_text(self, key, "");
            let value = Some(drafted.trim().to_string())
                .filter(|value| !value.is_empty())
                .or_else(|| self.theme_fallback_for(key).map(|(value, _)| value));
            let Some(value) = value else { continue };
            // One key at a time through the theme reader, so a value a theme
            // cannot carry drops alone.
            let probe = format!("fg = #ffffff\nbg = #000000\n{key} = {value}\n");
            if let Ok(parsed) = aterm_types::scheme::parse_theme_str(&probe) {
                typography.fill_from(&parsed.typography);
            }
        }
        typography
    }

    /// Whether any type setting is in force beyond aterm's defaults: set in
    /// aterm.toml, drafted, or carried by the current theme. Only then does
    /// Save as theme offer "Include type settings".
    pub(super) fn save_type_in_force(&self) -> bool {
        SAVE_TYPE_KEYS.iter().any(|key| {
            self.raw_value(key).is_some()
                || self.field_has_unsaved_draft(key)
                || self.theme_fallback_for(key).is_some()
        })
    }
}

/// The config keys a theme saved from `resolved` now carries: every colour
/// override in force, the `palette` list and the `palette_overrides` table
/// once each for all their slots.
pub(super) fn carried_override_keys(
    resolved: &crate::resolved_colors::ResolvedColors,
    typography: &aterm_types::scheme::ThemeTypography,
) -> Vec<String> {
    let overrides = format!("{}.", prefs::EDIT_PALETTE_OVERRIDES);
    let source_keys = resolved.config_keys();
    // A native theme carries sixteen ANSI slots, while either config source
    // can name extended colours (or contain text that did not parse). Keep
    // such a container whole: selecting this file cannot authorize deleting
    // values the file never serialized. The ordinary override notice keeps
    // reporting those retained settings.
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
    // The type settings the file carries too, so selecting it lets them go.
    keys.extend(
        typography
            .keys()
            .into_iter()
            .filter(|key| SAVE_TYPE_KEYS.contains(key))
            .map(str::to_string),
    );
    keys
}

/// How many of the person's colour settings draw on top of every theme: each
/// of the five colour overrides in force, and each palette slot the `palette`
/// list or the `palette_overrides` table sets.
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

/// The one noun for the person's color settings ([`override_count`]): the
/// Customize button, the strip, the sheet and what a reader hears all count
/// them in it.
pub(super) fn custom_colors(count: usize) -> String {
    count_noun(count, "custom color", "custom colors")
}

/// The strip's note while the person's color settings draw on top of every
/// theme (`2 custom colors on every theme`), whether a theme is tried or
/// not — a tried theme is drawn under them too — or `None` when none does.
pub(super) fn override_note(resolved: &crate::resolved_colors::ResolvedColors) -> Option<String> {
    let count = override_count(resolved);
    (count > 0).then(|| format!("{} on every theme", custom_colors(count)))
}

/// The page's palette commands while it is the route.
pub(super) fn showcase_commands(view: &SettingsViewState) -> Vec<Command> {
    let trying = matches!(view.showcase.trial, ShowcaseTrial::Trying { .. });
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
            // Only while the tried theme would change the face, and every
            // change can be kept apart.
            enabled: match &view.showcase.trial {
                ShowcaseTrial::Trying { id } => view.trial_typography(id).keeps_colours_only(),
                _ => false,
            },
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
        // A Trash press arms only the next press of that same Trash.
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
            view.showcase.pending_select = None;
            view.revert_showcase();
        } else if action == CUSTOMIZE {
            view.showcase.sheet_open = true;
            view.showcase.keep_more = false;
            view.invalidate_result_page_limit();
            view.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
            view.common.last_focus = Some(UiKey::new(CUSTOMIZE_DONE));
        } else if action == CUSTOMIZE_DONE {
            // The open slot's draft is saved first, as its own Done saves it.
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
            // The switch is Appearance's Follow system, written at once:
            // a setting, not a trial.
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
            // The segment picks the side Keep writes; the highlight goes to
            // that side's theme, and the list lands on its row (✓ and all)
            // while the keyboard stays on the segment.
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
            view.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
            // The field takes the keyboard: Clear is gone with the query.
            view.common.last_focus = Some(UiKey::new(SHOWCASE_FILTER));
        } else if action == SHOWCASE_FILTER {
            if let Some(text) = text {
                view.showcase.filter = crate::native_text_input::TextInputState::new(text.into());
                view.invalidate_result_page_limit();
                view.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
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
            if themes_busy(view) {
                view.feedback = Some("Working on the themes folder\u{2026}".to_string());
            } else {
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
            }
        } else if let Some(index) = action
            .strip_prefix(PALETTE_PREFIX)
            .and_then(|index| index.parse::<usize>().ok())
            .filter(|index| *index < 16)
        {
            // Another slot: the open one's draft is saved first, as Done
            // saves it.
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

    /// A row was pressed (a click, Return or Space on it, or `act`): the
    /// highlight moves to it, which tries it. The theme in use is no trial,
    /// so pressing its row reverts. The row already tried is pressed by Return
    /// (the keyboard is on it), so that press keeps it.
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
        // A community row pressed while its section is closed (a filter or
        // `act` reached it) opens the section, so the row it tries is shown.
        if card.section == Section::Community {
            view.showcase.pack_open = true;
            view.invalidate_result_page_limit();
        }
        let _ = view.highlight_card(&card.id);
    }

    /// A Yours row's Open, or a NEEDS ATTENTION row's: show its file.
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
        if themes_busy(view) {
            view.feedback = Some("Working on the themes folder\u{2026}".to_string());
            return;
        }
        let operation = cx.themes(ThemesRequest::OpenFile { id });
        view.pending.insert(operation, PendingAction::Themes);
        view.feedback = Some(format!("Opening {label}\u{2026}"));
    }

    /// A Yours card's Trash: the first press asks, the second moves the file
    /// to the Trash. A theme in use (either side of a split), or the one
    /// being tried, stays.
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
        if themes_busy(view) {
            view.feedback = Some("Working on the themes folder\u{2026}".to_string());
            return;
        }
        let operation = cx.themes(ThemesRequest::Trash { id: card.id });
        view.pending.insert(operation, PendingAction::Themes);
        view.feedback = Some(format!("Moving {} to the Trash\u{2026}", card.label));
    }

    /// Keep the tried theme: one `theme` write through the patch lane (one
    /// side of a Follow-system pair), the trial standing until it lands.
    fn keep_trial(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        let ShowcaseTrial::Trying { id } = view.showcase.trial.clone() else {
            return;
        };
        view.showcase.keep_more = false;
        // A community theme is drawn from the embedded pack: Keep adds its
        // file first, and writes `theme` once the catalog lists it
        // ([`Self::showcase_after_snapshot`]).
        if view.assets.themes.resolve(&id).is_err()
            && view.assets.themes.pack_fallback(&id).is_some()
        {
            if view.showcase.pending_keep.is_some() {
                return;
            }
            if themes_busy(view) {
                view.feedback = Some("Working on the themes folder\u{2026}".to_string());
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
        if Self::reject_pending_key(view, prefs::EDIT_THEME, cx) {
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
        let operation = cx.config_patch(patch.clone());
        view.pend_config(operation, patch);
        let label = view
            .showcase
            .card(&id)
            .map_or_else(|| id.clone(), |card| card.label.clone());
        let following = prefs::theme_mode(view.raw_value(prefs::EDIT_THEME).as_deref())
            == prefs::ThemeMode::FollowSystem;
        view.feedback = Some(match side {
            Some(side) => format!("Keeping {label} for {} mode\u{2026}", appearance_word(side)),
            None if following => format!("Keeping {label} for light and dark mode\u{2026}"),
            None => format!("Keeping {label}\u{2026}"),
        });
        view.showcase.trial = ShowcaseTrial::Keeping { id, operation };
    }

    /// Keep the tried theme's colours and the type settings in force now:
    /// ONE patch writes `theme` (as Keep does) and pins each type setting the
    /// theme would change at its current value, so the face stays. Where a
    /// change has no aterm.toml value that keeps the current face (aterm's own
    /// `font_weight`, the font's own weight), nothing is written and the
    /// feedback says why: the button is drawn disabled then, so this is the
    /// palette or a stale press.
    fn keep_colours_only(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        let ShowcaseTrial::Trying { id } = view.showcase.trial.clone() else {
            return;
        };
        let trial = view.trial_typography(&id);
        if !trial.keeps_colours_only() {
            if let Some(why) = trial.why_not_colours_only(view) {
                view.feedback = Some(why);
            }
            return;
        }
        if Self::reject_pending_key(view, prefs::EDIT_THEME, cx) {
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
        let patch = ConfigPatch {
            base_revision: self.config_revision,
            edits,
        };
        let operation = cx.config_patch(patch.clone());
        view.pend_config(operation, patch);
        let label = view
            .showcase
            .card(&id)
            .map_or_else(|| id.clone(), |card| card.label.clone());
        view.feedback = Some(format!(
            "Keeping {label}'s colors and your type settings\u{2026}"
        ));
        view.showcase.trial = ShowcaseTrial::Keeping { id, operation };
    }

    /// A config patch finished. A Keep (or a save's select) that landed
    /// ends the trial (the reload draws the kept theme); one that did not
    /// goes back to trying, and this says so (`true`). A save's select that
    /// landed also lets go of the drafts the saved theme now carries.
    pub(super) fn finish_showcase_keep(
        view: &mut SettingsViewState,
        operation: OperationId,
        applied: bool,
    ) -> bool {
        // The theme the patch wrote: a save's select writes the saved one,
        // whatever trial it ended.
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
            // The strip's Keep, Keep colours only and Revert go with the
            // trial: a keyboard on one of them lands on the kept row (else
            // on Customize, which takes their place), never on nothing.
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
            // The trial stands; the strip says why the write did not land.
            view.showcase.trial = ShowcaseTrial::Trying { id: id.clone() };
        }
        !applied
    }

    /// Save as theme: write the colours the stage shows as a new theme file.
    /// Nothing the person typed is let go here: the drafts stay drafts (the
    /// stage keeps drawing them) until the saved theme is selected.
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
        if themes_busy(view) {
            view.feedback = Some("Working on the themes folder\u{2026}".to_string());
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
            // A partially invalid palette may contribute some stage colours,
            // but the file does not carry its invalid text. Keep that draft.
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
            // A retained table may still shadow the saved slot. Its editor
            // remains a draft until the person applies it.
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
        // The name row closes and Save as theme takes its place again: the
        // keyboard goes back to it, as Cancel and Escape send it, never on
        // nothing.
        view.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_AS));
        view.feedback = Some(format!("Saving {name}\u{2026}"));
        view.invalidate_result_page_limit();
    }

    /// After a snapshot: a saved theme the catalog now lists is selected,
    /// and the overrides it carries are cleared, in one patch. A trial that
    /// stands keeps drawing until that patch lands, as Keep's does, so the
    /// screen goes from the tried theme straight to the saved one.
    pub(super) fn showcase_after_snapshot(
        &mut self,
        view: &mut SettingsViewState,
        cx: &mut UpdateCx<'_>,
    ) {
        // A community theme's Keep whose file is written and now listed:
        // the `theme` write follows, as any Keep's.
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
        // Still waiting for the write to report, or for the catalog to see it.
        if !select.saved || view.assets.themes.resolve(&select.id).is_err() {
            return;
        }
        let Some(select) = view.showcase.pending_select.take() else {
            return;
        };
        if std::iter::once(&select.theme)
            .chain(&select.clears)
            .any(|edit| {
                edit.expected != ExpectedConfigValue::Exact(view.raw_value(&edit.key))
                    || Self::reject_pending_key(view, &edit.key, cx)
            })
        {
            view.feedback = Some(format!(
                "Saved {} \u{b7} your settings changed; choose it when ready",
                select.id
            ));
            return;
        }
        // The value is chosen now, by the rule Keep uses, against the raw
        // value the Save click saw (checked unchanged above) and the catalog
        // that now knows the saved theme's appearance: a light theme saved
        // on a dark OS is then made for the other side, as a catalog theme
        // would be, instead of a name nobody knew yet. The page then shows
        // the side it went to, so the saved theme is the one highlighted.
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
        let operation = cx.config_patch(patch.clone());
        view.pend_config(operation, patch);
        view.showcase.selecting = Some((operation, select.id.clone(), select.drafts));
        view.showcase.trial = match view.showcase.trial_id() {
            Some(tried) => ShowcaseTrial::Keeping {
                id: tried.to_string(),
                operation,
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

    /// The Save as theme write finished. Its own success lets the catalog's
    /// listing select it; a write that failed reopens the name row with the
    /// name typed and the reason, and the drafts were never let go. Whether
    /// the save is now waiting to be selected.
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

    /// The palette slot field's text changed: on a slot the person's
    /// `palette` list reaches, a valid colour becomes that entry in the list's
    /// draft; on any other slot the field itself is the draft (the stage and
    /// chips follow both).
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

    /// Save the palette drafts in one patch: the `palette` list's draft, and
    /// the open slot's `palette_overrides` draft.
    fn commit_palette(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        let list = view.field_has_unsaved_draft(prefs::EDIT_PALETTE);
        let slot = view.slot_override_draft();
        let slot_key = slot.map(|(index, _)| crate::resolved_colors::palette_override_key(index));
        if !list && slot.is_none() {
            return;
        }
        let mut edits = Vec::new();
        if list {
            if Self::reject_pending_key(view, prefs::EDIT_PALETTE, cx) {
                return;
            }
            let entries = view.palette_entries();
            edits.push(ConfigEdit {
                key: prefs::EDIT_PALETTE.to_string(),
                expected: ExpectedConfigValue::Exact(view.raw_value(prefs::EDIT_PALETTE)),
                value: (!entries.is_empty()).then(|| entries.join(", ")),
            });
        }
        if let (Some((_, color)), Some(key)) = (slot, slot_key) {
            if Self::reject_pending_key(view, &key, cx) {
                return;
            }
            edits.push(ConfigEdit {
                expected: ExpectedConfigValue::Exact(view.raw_value(&key)),
                key,
                value: Some(hex_of(color)),
            });
            // Committed: the slot now shows this colour without a draft.
            if let Some(editor) = view.showcase.slot.as_mut() {
                editor.opened = color;
            }
        }
        let patch = ConfigPatch {
            base_revision: self.config_revision,
            edits,
        };
        let operation = cx.config_patch(patch.clone());
        view.pend_config(operation, patch);
        view.feedback = Some("Applying\u{2026}".to_string());
    }

    /// Close the palette slot editor, saving its `palette_overrides` draft
    /// (a list draft stays a draft, as any other field's does).
    fn close_palette_slot(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        if let Some((index, color)) = view.slot_override_draft() {
            let key = crate::resolved_colors::palette_override_key(index);
            if !Self::reject_pending_key(view, &key, cx) {
                let patch = ConfigPatch {
                    base_revision: self.config_revision,
                    edits: vec![ConfigEdit {
                        expected: ExpectedConfigValue::Exact(view.raw_value(&key)),
                        key,
                        value: Some(hex_of(color)),
                    }],
                };
                let operation = cx.config_patch(patch.clone());
                view.pend_config(operation, patch);
                view.feedback = Some("Applying\u{2026}".to_string());
            }
        }
        view.showcase.slot = None;
    }

    /// The edited slot goes back to the theme's colour: its
    /// `palette_overrides` key is removed, and on a slot the `palette` list
    /// reaches the list's entry goes back too (in the draft when one is
    /// held, else saved at once).
    fn reset_palette_slot(&mut self, view: &mut SettingsViewState, cx: &mut UpdateCx<'_>) {
        let Some(index) = view.showcase.slot.as_ref().map(|slot| slot.index) else {
            return;
        };
        let theme = view.theme_palette();
        let on_list = view.slot_on_list(index);
        let key = crate::resolved_colors::palette_override_key(index);
        let mut edits = Vec::new();
        if view.is_explicit(&key) {
            if Self::reject_pending_key(view, &key, cx) {
                return;
            }
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
                if Self::reject_pending_key(view, prefs::EDIT_PALETTE, cx) {
                    return;
                }
                edits.push(ConfigEdit {
                    key: prefs::EDIT_PALETTE.to_string(),
                    expected: ExpectedConfigValue::Exact(view.raw_value(prefs::EDIT_PALETTE)),
                    value: (!entries.is_empty()).then(|| entries.join(", ")),
                });
            }
        }
        // The slot without any override is the theme's colour (a slot the
        // list reaches is back on it above).
        if let Some(editor) = view.showcase.slot.as_mut() {
            editor.input = crate::native_text_input::TextInputState::new(hex_of(theme[index]));
            editor.opened = theme[index];
        }
        if !edits.is_empty() {
            let patch = ConfigPatch {
                base_revision: self.config_revision,
                edits,
            };
            let operation = cx.config_patch(patch.clone());
            view.pend_config(operation, patch);
            view.feedback = Some("Applying\u{2026}".to_string());
        }
        view.invalidate_result_page_limit();
    }

    /// Reset Palette: every slot back on the theme's colours — the `palette`
    /// list and the `palette_overrides` table removed in one patch, and the
    /// palette drafts let go.
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
        if keys.is_empty()
            || keys
                .iter()
                .any(|key| Self::reject_pending_key(view, key, cx))
        {
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
        let operation = cx.config_patch(patch.clone());
        view.pend_config(operation, patch);
        view.feedback = Some("Resetting the palette\u{2026}".to_string());
        view.invalidate_result_page_limit();
    }

    /// One editing event in a page text field. Return in the filter keeps
    /// the trial its rows list; in the name field it saves; in the palette
    /// slot it saves the palette.
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
                    view.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
                }
                // Return in the filter keeps the theme its arrows tried,
                // while the filter still lists it: a row the text hid is not
                // what Return in a search field takes.
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
        let target = match action {
            SHOWCASE_FILTER => ShowcaseText::Filter,
            SHOWCASE_SAVE_NAME => ShowcaseText::SaveName,
            SHOWCASE_PALETTE_SLOT => ShowcaseText::PaletteSlot,
            _ => return false,
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
                view.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
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

/// Whether a themes-folder request is in flight (one at a time).
fn themes_busy(view: &SettingsViewState) -> bool {
    view.pending
        .values()
        .any(|pending| matches!(pending, PendingAction::Themes))
}

/// The selected saved theme carries `drafts` now: each goes, while it still
/// holds the value that was saved (a later edit stays a draft).
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

/// The width `text` paints at in the face `role`/`style` paints it in.
fn text_width(text: &str, role: SemanticRole, style: StyleRef) -> f32 {
    let (px, face) = crate::native_ui::text_paint_metrics(role, style);
    crate::tray_raster::ui_text_width_for(face, text, px)
}

/// `text` fitted to `width` in the face `role`/`style` paints it in.
fn fit_text(text: &str, role: SemanticRole, style: StyleRef, width: f32) -> String {
    let (px, face) = crate::native_ui::text_paint_metrics(role, style);
    crate::native_ui::elide_text_label(text, width.max(0.0), px, face)
}

/// A one-line text node `height` tall, fitted to `width`.
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

/// A button `width` wide (Fill when `None`) with `label` fitted to it.
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

/// A button as wide as its label needs: the painter's 10pt lead and its
/// 10pt end, so the words sit centred, and a point of slack so the measure
/// never elides them.
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

/// A page text field.
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

/// The height of the strip's buttons and the chooser's small controls.
fn strip_button_height() -> f32 {
    28.0_f32.max(24.0 * settings_text_scale())
}

/// A row of controls one control tall.
fn control_row(key: &str, label: &str, children: Vec<UiNode>) -> (UiNode, f32) {
    let height = customize::sheet_row_height();
    (
        UiNode::new(key, UiContent::Group(GroupSpec::new(label)))
            .layout(Layout::row().height(Length::Fixed(height)).gap(6.0))
            .children(children),
        height,
    )
}

/// One type setting a tried theme would change once kept: its key, the
/// value it takes, and what keeps the value in force now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TypeChange {
    pub(super) key: &'static str,
    /// The value the setting takes once the theme is kept, as aterm.toml
    /// writes it (empty: aterm's own choice).
    pub(super) theme: String,
    /// What "Keep colours only" writes to keep the current value, `None`
    /// when no setting keeps it (aterm's own `font_weight`).
    pub(super) pin: Option<String>,
}

/// What a tried theme's typography would do once kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TrialTypography {
    /// The type settings that would change, in table order.
    pub(super) changes: Vec<TypeChange>,
}

impl TrialTypography {
    /// Whether "Keep colours only" can keep the face exactly: the theme
    /// changes some type setting, and every change has a value that pins it.
    pub(super) fn keeps_colours_only(&self) -> bool {
        !self.changes.is_empty() && self.changes.iter().all(|change| change.pin.is_some())
    }

    /// Why "Keep colours only" is not offered for a theme that changes the
    /// face: the settings no aterm.toml value keeps as they are now.
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
    /// A config that sets exactly the typography keys aterm.toml sets (with
    /// stand-in values: only whether each is set matters to the resolver),
    /// and `theme`.
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

    /// The value in force now for the typography key `key` (aterm.toml
    /// leaves it unset): the current theme's, else aterm's default, in the
    /// words [`type_value_words`] compares in, with what keeps it.
    fn current_type_value(&self, key: &'static str) -> (String, Option<String>) {
        if let Some((value, _)) = self.theme_fallback_for(key) {
            return (type_value_words(key, &value), Some(value));
        }
        self.default_type_value(key)
    }

    /// aterm's own value for the typography key `key`, whatever the theme:
    /// its words and the aterm.toml value that keeps it (`None` for aterm's
    /// own `font_weight`, which no value names).
    fn default_type_value(&self, key: &'static str) -> (String, Option<String>) {
        match key {
            prefs::EDIT_FONT_FAMILY => {
                let name = self
                    .font_families
                    .as_deref()
                    .and_then(|families| families.automatic.clone())
                    .unwrap_or_else(|| "aterm's own face".to_string());
                // A blank family is aterm's own choice and outranks a theme.
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

    /// What trying `id` would change in the type once kept: computed against
    /// the value Keep would write, the side the page shows, and the keys
    /// aterm.toml sets (and `--font`). A setting the tried theme sets that
    /// differs from the value in force is a change; so is one only the
    /// configured theme sets, which Keep returns to aterm's own. Empty
    /// between two colours-only themes.
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
        let configured = resolve(self.raw_value(prefs::EDIT_THEME));
        let mut out = TrialTypography::default();
        for spec in aterm_types::scheme::THEME_KEYS
            .iter()
            .filter(|spec| spec.group == aterm_types::scheme::ThemeKeyGroup::Typography)
        {
            let theme = match tried.applied.value_text(spec.key) {
                Some(theme) => theme,
                // Keep drops the configured theme's setting: aterm's own
                // value comes back.
                None if configured.applied.value_text(spec.key).is_some() => {
                    self.default_type_value(spec.name).1.unwrap_or_default()
                }
                None => continue,
            };
            let (current, pin) = self.current_type_value(spec.name);
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

/// A typography value in one vocabulary on both sides of a comparison: a
/// switch reads `on`/`off`, anything else as written.
fn type_value_words(key: &str, value: &str) -> String {
    match (key, value.trim()) {
        (prefs::EDIT_LIGATURES | prefs::EDIT_FONT_SYNTHETIC_STYLE, "true") => "on".to_string(),
        (prefs::EDIT_LIGATURES | prefs::EDIT_FONT_SYNTHETIC_STYLE, "false") => "off".to_string(),
        (_, value) => value.to_string(),
    }
}

/// A typography key's field label (`Font family`), else the key.
fn type_label(state: &SettingsViewState, key: &str) -> String {
    state
        .field_by_key(key)
        .map_or_else(|| key.to_string(), |field| field.label.to_string())
}
