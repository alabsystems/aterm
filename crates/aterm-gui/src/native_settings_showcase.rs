// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SETTINGS ▸ THEMES, the Theme Showcase: every theme aterm can draw as a
//! live miniature, a way to try one on the whole app before keeping it, the
//! colour and type settings a theme is tuned with, and "Save as theme…".
//!
//! The page has two views behind one toolbar segment:
//!
//! * **Gallery** — a grid of theme cards ([`ShowcaseCard`], built once per
//!   catalog generation): aterm's signature themes first, then the other
//!   built-ins, then the person's own files. Chips narrow it to dark, light or
//!   yours, and a filter field matches a name, id, description or appearance.
//!   Each card is one button labelled `<name>, dark|light, text contrast X:1`
//!   over a paint-only specimen tile in that theme's own colours.
//!   Your own themes' cards also carry Open (the file) and Trash (moves the
//!   file to the Trash, after a second press; never the theme in use).
//! * **Customize** — the existing registry rows for the font family, size,
//!   line height, ligatures and weight, the five colour overrides and the
//!   look rows (minimum contrast, bold is bright, background opacity), each
//!   with its Reset; the sixteen palette slots as chips; and the stage
//!   specimen, which draws the drafts as they are typed. A slot is written
//!   as one `palette_overrides.<index>` key, so a tuned slot follows every
//!   theme without pinning the slots before it; a person who already has a
//!   positional `palette` list edits that list where it reaches. Every edit
//!   goes through the page's ordinary draft and patch lane: the same drafts,
//!   the same Return, the same OCC patch, the same Undo.
//!
//! On a Wide page the stage stands beside both views, drawing the card under
//! the pointer or the keyboard in the terminal's own font before anything is
//! tried. While colour settings of the person's are in force, the bar over
//! the gallery says so: they draw on top of every theme tried.
//!
//! TRYING A THEME. Activating a card tries it: the view records the trial
//! ([`ShowcaseTrial::Trying`]) and the host draws it everywhere through the
//! transient preview lane ([`crate::App::sync_settings_theme_trial`]), writing
//! nothing. A sticky bar reads "Trying <name>" with Keep and Revert. Keep (or
//! activating the tried card again, or Return with nothing focused) writes
//! `theme` once through the patch lane — the one side shown when the theme
//! follows the system — and the trial stands until that write's reload draws
//! the theme for real. Revert, Esc, leaving the page and closing Settings all
//! end it the same way: no view asks for it any more, so the host reverts.
//!
//! SAVE AS THEME writes the colours the stage shows (the theme with every
//! override and draft over it) as a new theme file through the host's one
//! theme write path, then — once the catalog lists it — selects it and clears
//! the overrides it now carries, in one patch. The drafts it carried stay
//! drafts until that patch lands; a failed write reopens the name row with
//! the reason, and nothing typed is lost.

use super::*;
use crate::control_theme::ThemeTrialIntent;
use crate::settings_preview::theme_card::{
    ShowcaseCard, ShowcaseTier, showcase_cards, tile_height,
};

/// Every action the page mints starts with this.
pub(super) const SHOWCASE_ACTION_PREFIX: &str = "settings/showcase/";
/// The gallery's filter field (its node key and its action).
pub(super) const SHOWCASE_FILTER: &str = "settings/showcase/filter";
/// The Save as theme name field.
pub(super) const SHOWCASE_SAVE_NAME: &str = "settings/showcase/save-name";
/// The palette slot editor's field.
pub(super) const SHOWCASE_PALETTE_SLOT: &str = "settings/showcase/palette-slot";
/// Keep the tried theme.
pub(super) const SHOWCASE_KEEP: &str = "settings/showcase/keep";
/// Revert the tried theme.
pub(super) const SHOWCASE_REVERT: &str = "settings/showcase/revert";
/// Open the Save as theme row.
pub(super) const SHOWCASE_SAVE_AS: &str = "settings/showcase/save-as";
/// A card: `settings/showcase/card/<slug>`.
const CARD_PREFIX: &str = "settings/showcase/card/";
/// A chip: `settings/showcase/chip/<all|dark|light|yours>`.
const CHIP_PREFIX: &str = "settings/showcase/chip/";
const VIEW_GALLERY: &str = "settings/showcase/view/gallery";
const VIEW_CUSTOMIZE: &str = "settings/showcase/view/customize";
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
/// From the bar's override note to the Customize view, where each override
/// has its Reset.
const SHOW_OVERRIDES: &str = "settings/showcase/show-overrides";
/// A Yours card's file actions: `settings/showcase/card/<slug>/<verb>`.
const CARD_OPEN: &str = "open";
const CARD_TRASH: &str = "trash";

/// The page's heading.
const SHOWCASE_TITLE: &str = "Themes";
/// The gap between cards, and between the gallery and the stage.
const GUTTER: f32 = 12.0;
/// The room a scrolling gallery keeps clear at its trailing edge for the
/// list's scroll thumb, which paints 3pt wide at 7pt in: clear of a card's
/// frame and of the tile inset inside it.
const THUMB_GUTTER: f32 = 12.0;
/// The padding between a card's edge and its tile and text.
const CARD_PAD: f32 = 6.0;
/// The stage column beside a Wide gallery.
const STAGE_WIDTH: f32 = 280.0;

/// The colour overrides the Customize view edits, in the order it lists
/// them (`COLOR_KEYS` also holds the cursor trail's two colours, which a
/// theme does not set).
pub(super) const SHOWCASE_COLOR_KEYS: [&str; 5] = [
    prefs::EDIT_FOREGROUND,
    prefs::EDIT_BACKGROUND,
    prefs::EDIT_CURSOR_COLOR,
    prefs::EDIT_SELECTION_COLOR,
    prefs::EDIT_SELECTION_FOREGROUND,
];

/// The look settings the Customize view edits beside the colours: they
/// change how any theme reads, and a theme file does not carry them, so Save
/// as theme neither writes nor clears them.
pub(super) const SHOWCASE_LOOK_KEYS: [&str; 3] = [
    prefs::EDIT_MINIMUM_CONTRAST,
    prefs::EDIT_BOLD_IS_BRIGHT,
    prefs::EDIT_BACKGROUND_OPACITY,
];

/// The type settings the Customize view edits. `font_weight` is a real key
/// (it acts on a font with a `wght` axis), so it is listed.
pub(super) const SHOWCASE_TYPE_KEYS: [&str; 5] = [
    prefs::EDIT_FONT_FAMILY,
    prefs::EDIT_FONT_PX,
    prefs::EDIT_LINE_HEIGHT,
    prefs::EDIT_LIGATURES,
    prefs::EDIT_FONT_WEIGHT,
];

/// The gallery's appearance and origin chips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ShowcaseChip {
    #[default]
    All,
    Dark,
    Light,
    Yours,
}

impl ShowcaseChip {
    pub(crate) const ALL: [Self; 4] = [Self::All, Self::Dark, Self::Light, Self::Yours];

    const fn word(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Yours => "yours",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::Yours => "Yours",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|chip| chip.word() == word)
    }

    /// Whether the chip admits `card`.
    pub(crate) fn admits(self, card: &ShowcaseCard) -> bool {
        match self {
            Self::All => true,
            Self::Dark => card.appearance == aterm_types::Appearance::Dark,
            Self::Light => card.appearance == aterm_types::Appearance::Light,
            Self::Yours => card.tier == ShowcaseTier::Yours,
        }
    }
}

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
}

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

/// Settings ▸ Themes' view state. View-local, never config.
#[derive(Debug)]
pub(crate) struct ShowcaseState {
    pub(crate) filter: crate::native_text_input::TextInputState,
    pub(crate) chip: ShowcaseChip,
    /// The Customize view (else the Gallery).
    pub(crate) customize: bool,
    /// The gallery's cards, built from the catalog this view admitted.
    pub(crate) cards: Arc<[ShowcaseCard]>,
    pub(crate) trial: ShowcaseTrial,
    /// The palette slot being edited.
    pub(crate) slot: Option<SlotEditor>,
    pub(crate) save: Option<SaveAsState>,
    pub(crate) pending_select: Option<PendingSelect>,
    /// The select patch a save sent, and the drafts it carries: they go
    /// when it lands.
    selecting: Option<(OperationId, CarriedDrafts)>,
    /// The Yours card whose Trash was pressed once: a second press moves it.
    pub(crate) trash_armed: Option<String>,
    /// The gallery's cards as the last render laid them out, row by row:
    /// Left and Right step through them in order, Up and Down by row.
    drawn: RefCell<Vec<Vec<String>>>,
}

impl ShowcaseState {
    pub(super) fn new(themes: &crate::app_config::ThemeCatalog) -> Self {
        Self {
            filter: crate::native_text_input::TextInputState::new(String::new()),
            chip: ShowcaseChip::All,
            customize: false,
            cards: showcase_cards(themes).into(),
            trial: ShowcaseTrial::Idle,
            slot: None,
            save: None,
            pending_select: None,
            selecting: None,
            trash_armed: None,
            drawn: RefCell::new(Vec::new()),
        }
    }

    /// The cards the chip and filter admit, as indices into `cards`.
    pub(crate) fn visible(&self) -> Vec<usize> {
        let query = self.filter.value().trim().to_lowercase();
        self.cards
            .iter()
            .enumerate()
            .filter(|(_, card)| self.chip.admits(card) && card.matches(&query))
            .map(|(index, _)| index)
            .collect()
    }

    /// The gallery's cards as the last render laid them out, row by row.
    #[cfg(test)]
    pub(crate) fn drawn_rows(&self) -> Vec<Vec<String>> {
        self.drawn.borrow().clone()
    }

    fn card(&self, id: &str) -> Option<&ShowcaseCard> {
        self.cards
            .iter()
            .find(|card| card.id.eq_ignore_ascii_case(id))
    }

    fn card_by_slug(&self, slug: &str) -> Option<&ShowcaseCard> {
        self.cards.iter().find(|card| card.slug == slug)
    }

    /// Whether the last render listed the card `slug`.
    fn is_drawn(&self, slug: &str) -> bool {
        self.drawn
            .borrow()
            .iter()
            .any(|row| row.iter().any(|drawn| drawn == slug))
    }

    /// The id the trial is about, while there is one.
    fn trial_id(&self) -> Option<&str> {
        match &self.trial {
            ShowcaseTrial::Idle => None,
            ShowcaseTrial::Trying { id } | ShowcaseTrial::Keeping { id, .. } => Some(id),
        }
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
    /// What this view asks of the live theme: the theme it tries, or keeps
    /// drawn while its Keep write is in flight. `None` off the page.
    pub(crate) fn theme_trial_intent(&self) -> Option<ThemeTrialIntent> {
        if self.route != SettingsRoute::Themes {
            return None;
        }
        match &self.showcase.trial {
            ShowcaseTrial::Idle => None,
            ShowcaseTrial::Trying { id } => Some(ThemeTrialIntent::Show(id.clone())),
            ShowcaseTrial::Keeping { id, .. } => Some(ThemeTrialIntent::Keeping(id.clone())),
        }
    }

    /// The host could not draw the tried theme `id`: the trial ends and the
    /// page says why.
    pub(crate) fn refuse_theme_trial(&mut self, id: &str, why: &str) {
        if self
            .showcase
            .trial_id()
            .is_some_and(|tried| tried.eq_ignore_ascii_case(id))
        {
            self.showcase.trial = ShowcaseTrial::Idle;
            self.feedback = Some(format!("Can\u{2019}t try {id}: {why}"));
            self.common.presentation_revision = self.common.presentation_revision.saturating_add(1);
        }
    }

    /// The page's cards follow a new catalog; a trial whose theme is gone
    /// ends, and says so.
    pub(super) fn refresh_showcase(&mut self) {
        self.showcase.cards = showcase_cards(&self.assets.themes).into();
        self.showcase.trash_armed = None;
        if let Some(id) = self.showcase.trial_id().map(str::to_string)
            && self.assets.themes.resolve(&id).is_err()
        {
            self.showcase.trial = ShowcaseTrial::Idle;
            self.feedback = Some(format!("{id} is no longer in the themes folder"));
        }
    }

    /// Leaving the page: the trial ends (the host reverts it) and the page's
    /// transient state goes. The chip, the filter and the view stay, so a
    /// return finds the gallery as it was left.
    pub(super) fn leave_showcase(&mut self) {
        self.showcase.trial = ShowcaseTrial::Idle;
        self.showcase.slot = None;
        self.showcase.save = None;
        self.showcase.pending_select = None;
        self.showcase.trash_armed = None;
    }

    /// Escape on the page, deepest first: a Trash waiting for its second
    /// press, the palette slot editor (its draft let go), the Save as row,
    /// the trial, the filter. Whether it took the Escape.
    pub(super) fn cancel_showcase(&mut self) -> bool {
        if self.route != SettingsRoute::Themes {
            return false;
        }
        if self.showcase.trash_armed.take().is_some() {
            self.feedback = None;
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
        if self.showcase.pending_select.take().is_some() {
            self.feedback = Some("The saved theme will not be selected automatically".to_string());
            return true;
        }
        if matches!(self.showcase.trial, ShowcaseTrial::Trying { .. }) {
            self.showcase.trial = ShowcaseTrial::Idle;
            self.feedback = Some("Reverted".to_string());
            return true;
        }
        if !self.showcase.filter.value().is_empty() {
            self.showcase.filter = crate::native_text_input::TextInputState::new(String::new());
            self.invalidate_result_page_limit();
            self.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
            return true;
        }
        false
    }

    /// The page text field the keyboard is in, if any.
    pub(super) fn showcase_text_focus(&self) -> Option<ShowcaseText> {
        if self.route != SettingsRoute::Themes {
            return None;
        }
        match self.common.last_focus.as_ref()?.as_str() {
            SHOWCASE_FILTER => Some(ShowcaseText::Filter),
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

    /// The card under the pointer, else the one the keyboard is on: what the
    /// stage beside a Wide gallery previews before anything is tried. None
    /// while a Settings search shows its results in the page's place: no card
    /// is on screen then.
    pub(super) fn showcase_candidate(&self) -> Option<&ShowcaseCard> {
        if self.route != SettingsRoute::Themes
            || self.showcase.customize
            || !self.search.trim().is_empty()
        {
            return None;
        }
        [
            self.common.hovered.as_ref(),
            self.common.last_focus.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter_map(card_slug_of)
        .find(|slug| self.showcase.is_drawn(slug))
        .and_then(|slug| self.showcase.card_by_slug(slug))
    }

    /// Whether the page draws the stage: in Customize, and beside a Wide
    /// gallery (before the first frame the width is not known, so yes).
    pub(super) fn showcase_draws_stage(&self) -> bool {
        self.showcase.customize
            || !matches!(
                self.presented_width.get(),
                Some(SettingsWidth::Compact | SettingsWidth::Medium)
            )
    }

    /// The theme selector the stage draws for the page's candidate: the
    /// hovered or focused card, else the trial.
    pub(super) fn showcase_preview_id(&self) -> Option<String> {
        if self.route != SettingsRoute::Themes {
            return None;
        }
        self.showcase_candidate()
            .map(|card| card.id.clone())
            .or_else(|| self.showcase.trial_id().map(str::to_string))
    }

    /// The theme the stage's badge names: the candidate it draws, else the
    /// configured theme on the side the page shows (never the raw
    /// `dark:…,light:…` value).
    pub(super) fn showcase_stage_theme_id(&self) -> Option<String> {
        if self.route != SettingsRoute::Themes {
            return None;
        }
        Some(
            self.showcase_preview_id()
                .unwrap_or_else(|| self.configured_theme_id()),
        )
    }

    /// The card the keyboard is on, as its slug.
    pub(super) fn showcase_focused_card(&self) -> Option<String> {
        if self.route != SettingsRoute::Themes {
            return None;
        }
        card_slug_of(self.common.last_focus.as_ref()?).map(str::to_string)
    }

    /// The theme the configuration draws on the side the page shows (what
    /// Keep replaces, and what a card marked Current names).
    fn configured_theme_id(&self) -> String {
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

    /// The value Keep writes for `id`, by the rule every theme picker shares
    /// ([`prefs::theme_choice_value`]): over a plain name, the name; over a
    /// Follow-system split, the split with only the side the page shows
    /// replaced; over an unset theme, a split that keeps following the OS
    /// (`None`, no key, where a choice leaves the pair an unset theme draws).
    fn keep_value(&self, id: &str) -> (Option<String>, Option<aterm_types::Appearance>) {
        let raw = self.raw_value(prefs::EDIT_THEME);
        let side = theme_visible_side(self, self.presented_motion.get().system_dark);
        prefs::theme_choice_value(raw.as_deref(), side, id, &self.assets.themes)
    }

    /// The colours the page's stage draws with nothing hovered: the trial
    /// (else the configured theme) under every override and draft.
    fn showcase_colors(&self) -> crate::resolved_colors::ResolvedColors {
        let system_dark = self.presented_motion.get().system_dark;
        preview_resolved_colors_with(self, system_dark, self.showcase.trial_id())
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
            .resolved_colors(side, &self.assets.themes, self.showcase.trial_id())
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
    Some(format!("Palette colour {index}, {name}"))
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
pub(super) fn suggested_save_name(label: &str, themes: &crate::app_config::ThemeCatalog) -> String {
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
) -> Result<String, String> {
    let mut scheme = resolved.scheme.clone();
    // The name as the file spells it (`aterm theme new` and import do the
    // same): the writer drops a space before `#` or `;` and caps the length,
    // and the read-back below compares the scheme whole. The file itself is
    // still named by the id the person typed.
    scheme.name = aterm_types::scheme_import::sanitize_text(name).unwrap_or_default();
    scheme.appearance = resolved.reported;
    let description = format!("Saved from {from_label} with your colour settings");
    let text = aterm_types::scheme_import::to_native_conf(
        &scheme,
        true,
        Some(&description),
        None,
        &["Saved by Settings \u{25b8} Themes \u{25b8} Save as theme"],
    );
    let parsed = aterm_types::scheme::parse_theme_str(&text).map_err(|error| error.to_string())?;
    if parsed.scheme != scheme {
        return Err("the theme did not read back as written".to_string());
    }
    Ok(text)
}

/// The config keys a theme saved from `resolved` now carries: every colour
/// override in force, the `palette` list and the `palette_overrides` table
/// once each for all their slots.
pub(super) fn carried_override_keys(
    resolved: &crate::resolved_colors::ResolvedColors,
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
    keys
}

/// What the person's colour settings in force do to every theme tried, in
/// words (`Your background and 3 palette colours`), or `None` when none is.
pub(super) fn override_note(resolved: &crate::resolved_colors::ResolvedColors) -> Option<String> {
    let mut named: Vec<String> = Vec::new();
    let mut slots = BTreeSet::new();
    for key in resolved.config_keys() {
        let word = match key.as_str() {
            "foreground" => "foreground",
            "background" => "background",
            "cursor_color" => "cursor colour",
            "selection_color" => "selection colour",
            "selection_foreground" => "selection text colour",
            _ => {
                let slot = key
                    .strip_prefix("palette[")
                    .and_then(|rest| rest.strip_suffix(']'))
                    .or_else(|| {
                        key.strip_prefix(prefs::EDIT_PALETTE_OVERRIDES)
                            .and_then(|rest| rest.strip_prefix('.'))
                    })
                    .and_then(|index| index.parse::<usize>().ok());
                slots.extend(slot);
                continue;
            }
        };
        named.push(word.to_string());
    }
    match slots.len() {
        0 => {}
        1 => named.push("1 palette colour".to_string()),
        count => named.push(format!("{count} palette colours")),
    }
    let last = named.pop()?;
    Some(if named.is_empty() {
        format!("Your {last}")
    } else {
        format!("Your {} and {last}", named.join(", "))
    })
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
        } else if action == SHOWCASE_REVERT {
            view.showcase.pending_select = None;
            if matches!(view.showcase.trial, ShowcaseTrial::Trying { .. }) {
                view.showcase.trial = ShowcaseTrial::Idle;
                view.feedback = Some("Reverted".to_string());
            }
        } else if let Some(word) = action.strip_prefix(CHIP_PREFIX) {
            let Some(chip) = ShowcaseChip::from_word(word) else {
                return EventResult::Handled;
            };
            view.showcase.chip = chip;
            view.invalidate_result_page_limit();
            view.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
            view.common.last_focus = Some(UiKey::new(action));
        } else if action == VIEW_GALLERY || action == VIEW_CUSTOMIZE || action == SHOW_OVERRIDES {
            view.showcase.customize = action != VIEW_GALLERY;
            self.close_palette_slot(view, cx);
            view.invalidate_result_page_limit();
            view.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
            // The keyboard stays on the segment pressed (the bar's link lands
            // on the segment it switched to).
            view.common.last_focus = Some(UiKey::new(if action == SHOW_OVERRIDES {
                VIEW_CUSTOMIZE
            } else {
                action
            }));
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
            });
            view.showcase.customize = true;
            view.invalidate_result_page_limit();
            // The name row leads the Customize view's first page: turn to
            // it, or the field the keyboard is now in is on a page not shown.
            view.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
            view.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_NAME));
        } else if action == SHOWCASE_SAVE_NAME {
            if let (Some(text), Some(save)) = (text, view.showcase.save.as_mut()) {
                save.name = crate::native_text_input::TextInputState::new(text.into());
                save.error = save_name_problem(text, &view.assets.themes);
            }
            view.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_NAME));
        } else if action == SAVE_CONFIRM {
            self.save_as_theme(view, cx);
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

    /// A card was activated: try it, or keep it when it is the one tried.
    fn activate_card(&mut self, view: &mut SettingsViewState, slug: &str, cx: &mut UpdateCx<'_>) {
        let Some(card) = view.showcase.card_by_slug(slug).cloned() else {
            view.feedback = Some("That theme is no longer listed".to_string());
            return;
        };
        view.showcase.pending_select = None;
        view.common.last_focus = Some(UiKey::new(format!("{CARD_PREFIX}{slug}")));
        match &view.showcase.trial {
            ShowcaseTrial::Trying { id } if id.eq_ignore_ascii_case(&card.id) => {
                self.keep_trial(view, cx);
            }
            ShowcaseTrial::Keeping { .. } => {
                view.feedback = Some("Keeping the theme you tried\u{2026}".to_string());
            }
            _ if card.id.eq_ignore_ascii_case(&view.configured_theme_id()) => {
                view.showcase.trial = ShowcaseTrial::Idle;
                view.feedback = Some(format!("{} is the theme in use", card.label));
            }
            _ => {
                view.showcase.trial = ShowcaseTrial::Trying {
                    id: card.id.clone(),
                };
                // The sticky bar says it, with Keep and Revert.
                view.feedback = None;
            }
        }
    }

    /// A Yours card's Open: show its file.
    fn open_card_file(view: &mut SettingsViewState, slug: &str, cx: &mut UpdateCx<'_>) {
        let Some(card) = view
            .showcase
            .card_by_slug(slug)
            .filter(|card| card.tier == ShowcaseTier::Yours)
            .cloned()
        else {
            view.feedback = Some("That theme is no longer listed".to_string());
            return;
        };
        view.common.last_focus = Some(UiKey::new(format!("{CARD_PREFIX}{slug}/{CARD_OPEN}")));
        if themes_busy(view) {
            view.feedback = Some("Working on the themes folder\u{2026}".to_string());
            return;
        }
        let operation = cx.themes(ThemesRequest::OpenFile { id: card.id });
        view.pending.insert(operation, PendingAction::Themes);
        view.feedback = Some(format!("Opening {}\u{2026}", card.label));
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
            .filter(|card| card.tier == ShowcaseTier::Yours)
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
        if Self::reject_pending_key(view, prefs::EDIT_THEME, cx) {
            return;
        }
        let (value, side) = view.keep_value(&id);
        if view.raw_value(prefs::EDIT_THEME) == value {
            view.showcase.trial = ShowcaseTrial::Idle;
            return;
        }
        // A pair that draws the kept theme on both sides is kept for both.
        let both_sides = value
            .as_deref()
            .and_then(prefs::theme_pair_names)
            .is_some_and(|(dark, light)| dark.eq_ignore_ascii_case(&light));
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
            Some(_) if both_sides => format!("Keeping {label} for light and dark mode\u{2026}"),
            Some(side) => format!("Keeping {label} for {} mode\u{2026}", appearance_word(side)),
            None if following => format!("Keeping {label} for light and dark mode\u{2026}"),
            None => format!("Keeping {label}\u{2026}"),
        });
        view.showcase.trial = ShowcaseTrial::Keeping { id, operation };
    }

    /// A config patch finished. A Keep (or a save's select) that landed
    /// ends the trial (the reload draws the kept theme); one that did not
    /// goes back to trying. A save's select that landed also lets go of the
    /// drafts the saved theme now carries.
    pub(super) fn finish_showcase_keep(
        view: &mut SettingsViewState,
        operation: OperationId,
        applied: bool,
    ) {
        if view
            .showcase
            .selecting
            .as_ref()
            .is_some_and(|(selecting, _)| *selecting == operation)
            && let Some((_, drafts)) = view.showcase.selecting.take()
            && applied
        {
            drop_carried_drafts(view, &drafts);
        }
        let ShowcaseTrial::Keeping {
            id,
            operation: keeping,
        } = &view.showcase.trial
        else {
            return;
        };
        if *keeping != operation {
            return;
        }
        view.showcase.trial = if applied {
            ShowcaseTrial::Idle
        } else {
            ShowcaseTrial::Trying { id: id.clone() }
        };
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
        let text = match save_as_text(&resolved, &name, &from) {
            Ok(text) => text,
            Err(why) => {
                if let Some(save) = view.showcase.save.as_mut() {
                    save.error = Some(format!("Couldn\u{2019}t save {name}: {why}"));
                }
                return;
            }
        };
        let carried = carried_override_keys(&resolved);
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
        // would be, instead of a name nobody knew yet.
        let mut select = select;
        let (value, side) = view.keep_value(&select.id);
        select.theme.value = value;
        select.side = side;
        let has_clears = !select.clears.is_empty();
        let mut edits = vec![select.theme];
        edits.extend(select.clears);
        let patch = ConfigPatch {
            base_revision: self.config_revision,
            edits,
        };
        let operation = cx.config_patch(patch.clone());
        view.pend_config(operation, patch);
        view.showcase.selecting = Some((operation, select.drafts));
        view.showcase.trial = match view.showcase.trial_id() {
            Some(tried) => ShowcaseTrial::Keeping {
                id: tried.to_string(),
                operation,
            },
            None => ShowcaseTrial::Idle,
        };
        view.feedback = Some(if select.side.is_some() && has_clears {
            format!(
                "Selected {} \u{b7} its colour settings came off the other side too",
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
        });
        view.showcase.customize = true;
        view.common.last_focus = Some(UiKey::new(SHOWCASE_SAVE_NAME));
        view.invalidate_result_page_limit();
        false
    }

    /// The palette slot field's text changed: on a slot the person's
    /// `palette` list reaches, a valid colour becomes that entry in the list's
    /// draft; on any other slot the field itself is the draft (the stage and
    /// chips follow both).
    fn edit_palette_slot(&mut self, view: &mut SettingsViewState, text: &str) {
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

    /// One editing event in a page text field. Return in the filter moves the
    /// keyboard to the first card; in the name field it saves; in the palette
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
                if submit {
                    let first = view.showcase.visible().first().copied();
                    if let Some(first) = first {
                        let slug = view.showcase.cards[first].slug.clone();
                        view.common.last_focus = Some(UiKey::new(format!("{CARD_PREFIX}{slug}")));
                    }
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

    /// An arrow key while a card has the keyboard: Left and Right step along
    /// the gallery's cards (`rows` 0, `delta` ±1), Up and Down to the card
    /// below or above in the next row (`rows` ±1), and the list follows. Past
    /// the first or last card the keyboard leaves the gallery for the control
    /// before or after it. Whether the key was the gallery's.
    pub(super) fn step_card_focus(
        &mut self,
        view: &mut SettingsViewState,
        rows: i32,
        delta: i32,
    ) -> bool {
        let Some(slug) = view.showcase_focused_card() else {
            return false;
        };
        let grid = view.showcase.drawn.borrow().clone();
        let Some((row, column)) = grid.iter().enumerate().find_map(|(row, cards)| {
            cards
                .iter()
                .position(|card| *card == slug)
                .map(|column| (row, column))
        }) else {
            return false;
        };
        let target = if rows == 0 {
            let flat: Vec<&String> = grid.iter().flatten().collect();
            let at = grid[..row].iter().map(Vec::len).sum::<usize>() + column;
            usize::try_from(at as i64 + i64::from(delta))
                .ok()
                .and_then(|index| flat.get(index).map(|card| (*card).clone()))
        } else {
            usize::try_from(row as i64 + i64::from(rows.signum()))
                .ok()
                .and_then(|row| grid.get(row))
                .and_then(|cards| cards.get(column.min(cards.len().saturating_sub(1))))
                .cloned()
        };
        let Some(card) = target else {
            view.step_list_focus(if rows == 0 { delta } else { rows }.signum());
            return true;
        };
        let key = UiKey::new(format!("{CARD_PREFIX}{card}"));
        view.common.last_focus = Some(key.clone());
        view.common.focus_visible = true;
        view.reveal_control(key);
        true
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

/// A button as wide as its label needs.
fn hug_width(label: &str) -> f32 {
    crate::native_ui::native_button_label_width(label) + 28.0
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

/// A row of controls one control tall.
fn control_row(key: &str, label: &str, children: Vec<UiNode>) -> (UiNode, f32) {
    let height = scaled_control_height();
    (
        UiNode::new(key, UiContent::Group(GroupSpec::new(label)))
            .layout(Layout::row().height(Length::Fixed(height)).gap(6.0))
            .children(children),
        height,
    )
}

/// The sticky bars over the page: the trial bar while a theme is tried
/// (what is tried, with Keep and Revert), and over the gallery the note that
/// the person's colour settings draw on top of every theme, with the way to
/// them. Nothing while neither applies (the cards mark the theme in use).
fn trial_bar(
    state: &SettingsViewState,
    width: SettingsWidth,
    page_width: f32,
) -> Vec<(UiNode, f32)> {
    let mut rows: Vec<(UiNode, f32)> = trial_row(state, width, page_width).into_iter().collect();
    if !state.showcase.customize
        && let Some(note) = override_note(&state.showcase_colors())
    {
        rows.push(override_row(state, width, page_width, &note));
    }
    rows
}

/// The bar's override note: `<Your background and 2 palette colours> stay
/// on top of every theme`, and Customize, where each has its Reset.
fn override_row(
    state: &SettingsViewState,
    width: SettingsWidth,
    page_width: f32,
    note: &str,
) -> (UiNode, f32) {
    let text = if width == SettingsWidth::Compact {
        format!("{note} stay on top")
    } else {
        format!("{note} stay on top of every theme")
    };
    let link = hug_width("Customize");
    let text_width = (page_width - link - 6.0 - 12.0).max(0.0);
    let height = scaled_control_height();
    (
        UiNode::new(
            "settings/top/showcase-overrides",
            UiContent::Group(GroupSpec::new("Your colour settings").style(StyleRef::Secondary)),
        )
        .layout(
            Layout::row()
                .height(Length::Fixed(height))
                .padding(Insets {
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                    left: 12.0,
                })
                .gap(6.0),
        )
        .children(vec![
            line(
                "settings/top/showcase-overrides/note",
                &text,
                SemanticRole::Status,
                StyleRef::Quiet,
                text_width,
                height,
            ),
            button(
                SHOW_OVERRIDES,
                "Show your colour settings in Customize",
                "Customize",
                StyleRef::Quiet,
                ControlState {
                    focused: focused(state, SHOW_OVERRIDES),
                    ..ControlState::default()
                },
                Some(link),
            ),
        ]),
        height,
    )
}

/// The trial bar while a theme is tried.
fn trial_row(
    state: &SettingsViewState,
    width: SettingsWidth,
    page_width: f32,
) -> Option<(UiNode, f32)> {
    let showcase = &state.showcase;
    let label_of = |id: &str| {
        showcase
            .card(id)
            .map_or_else(|| id.to_string(), |card| card.label.clone())
    };
    let compact = width == SettingsWidth::Compact;
    let action_width = |label: &str| {
        if compact {
            hug_width(label).min(96.0 * settings_text_scale().min(1.5))
        } else {
            hug_width(label).max(88.0)
        }
    };
    let pending = state.config_key_pending(prefs::EDIT_THEME);
    let (status, mut controls): (String, Vec<UiNode>) = match &showcase.trial {
        ShowcaseTrial::Trying { id } => {
            let label = label_of(id);
            let keep = action_width("Keep");
            let revert = action_width("Revert");
            let hint = if compact {
                format!("Trying {label}")
            } else {
                format!("Trying {label} \u{2014} Return keeps, Esc reverts")
            };
            (
                hint,
                vec![
                    button(
                        SHOWCASE_KEEP,
                        &format!("Keep {label}"),
                        "Keep",
                        StyleRef::Primary,
                        ControlState {
                            enabled: !pending,
                            focused: focused(state, SHOWCASE_KEEP),
                            ..ControlState::default()
                        },
                        Some(keep),
                    ),
                    button(
                        SHOWCASE_REVERT,
                        &format!("Revert {label}"),
                        "Revert",
                        StyleRef::Secondary,
                        ControlState {
                            focused: focused(state, SHOWCASE_REVERT),
                            ..ControlState::default()
                        },
                        Some(revert),
                    ),
                ],
            )
        }
        ShowcaseTrial::Keeping { id, .. } => (
            format!("Keeping {}\u{2026}", label_of(id)),
            vec![button(
                SHOWCASE_KEEP,
                "Keep",
                "Keep",
                StyleRef::Primary,
                ControlState {
                    enabled: false,
                    busy: true,
                    ..ControlState::default()
                },
                Some(action_width("Keep")),
            )],
        ),
        ShowcaseTrial::Idle => return None,
    };
    let controls_width: f32 = controls
        .iter()
        .map(|node| match node.layout.width {
            Length::Fixed(width) => width + 6.0,
            _ => 0.0,
        })
        .sum();
    let text_width = (page_width - controls_width).max(0.0);
    let height = scaled_control_height();
    let status_node = UiNode::new(
        "settings/showcase/status",
        UiContent::Group(GroupSpec {
            label: Some(status.clone()),
            role: SemanticRole::Status,
            style: StyleRef::Plain,
        }),
    )
    .layout(Layout::default().width(Length::Fill).height(Length::Fill))
    .children(vec![
        UiNode::new(
            "settings/showcase/status/visual",
            UiContent::Text(TextSpec {
                text: fit_text(
                    &status,
                    SemanticRole::Text,
                    StyleRef::Plain,
                    text_width - 24.0,
                ),
                role: SemanticRole::Text,
                style: StyleRef::Plain,
            }),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill))
        .paint_only(),
    ]);
    controls.insert(0, status_node);
    Some((
        UiNode::new(
            "settings/top/showcase-bar",
            UiContent::Group(GroupSpec::new("Theme trial").style(StyleRef::Secondary)),
        )
        .layout(
            Layout::row()
                .height(Length::Fixed(height))
                .padding(Insets {
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                    left: 12.0,
                })
                .gap(6.0),
        )
        .children(controls),
        height,
    ))
}

/// The gallery's footer: how many theme files loaded, Import… and the
/// themes folder.
fn gallery_footer(state: &SettingsViewState, column_width: f32) -> (UiNode, f32) {
    let themes = &state.assets.themes;
    let loaded = themes.ready_names().count();
    let files = themes.limits().seen.max(themes.listing().count());
    let status = match (loaded, files) {
        (_, 0) => "No theme files of your own yet".to_string(),
        (loaded, files) if loaded == files => format!(
            "{files} theme {} of your own",
            if files == 1 { "file" } else { "files" }
        ),
        (loaded, files) => format!("{loaded} of {files} theme files loaded"),
    };
    let import = hug_width("Import\u{2026}");
    let folder = hug_width("Themes Folder");
    let text_width = (column_width - import - folder - 12.0).max(0.0);
    control_row(
        "settings/showcase/footer",
        "Theme files",
        vec![
            line(
                "settings/showcase/footer/status",
                &status,
                SemanticRole::Status,
                StyleRef::Quiet,
                text_width,
                scaled_control_height(),
            ),
            button(
                IMPORT,
                "Import a theme from another terminal\u{2026}",
                "Import\u{2026}",
                StyleRef::Quiet,
                ControlState {
                    enabled: crate::menu::local_file_picker_available(),
                    focused: focused(state, IMPORT),
                    ..ControlState::default()
                },
                Some(import),
            ),
            button(
                OPEN_FOLDER,
                "Open themes folder",
                "Themes Folder",
                StyleRef::Quiet,
                ControlState {
                    focused: focused(state, OPEN_FOLDER),
                    ..ControlState::default()
                },
                Some(folder),
            ),
        ],
    )
}

/// The narrowest the gallery's filter field gets on a row shared with the
/// chips: its placeholder, inset as a button's label is.
fn filter_minimum_width() -> f32 {
    hug_width("Filter themes")
}

/// Whether the gallery's toolbar row at `row_width` seats the chips beside
/// the view segment while the filter keeps [`filter_minimum_width`]. A row
/// too narrow for them gives the chips a row of their own.
pub(super) fn gallery_toolbar_fits_chips(row_width: f32) -> bool {
    const ROW_GAP: f32 = 6.0;
    let segments: f32 = ["Gallery", "Customize"]
        .iter()
        .map(|visual| hug_width(visual).max(72.0))
        .sum();
    let chips: f32 = ShowcaseChip::ALL
        .iter()
        .map(|chip| hug_width(chip.label()).max(56.0))
        .sum();
    // The segment's two buttons and the filter, with their gaps.
    let base = segments + filter_minimum_width() + 2.0 * ROW_GAP;
    base + chips + ShowcaseChip::ALL.len() as f32 * ROW_GAP <= row_width
}

/// How the toolbar lays out its controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolbarShape {
    /// One row: the view segment, the filter (or Save as theme), the chips.
    Row,
    /// The chips on a row of their own under the segment and filter.
    ChipsBelow,
}

impl ToolbarShape {
    /// The shape a page of `width` gives a toolbar `row_width` wide: a
    /// Compact row too narrow for the chips beside the filter
    /// ([`gallery_toolbar_fits_chips`]) puts them on a row of their own.
    fn for_row(width: SettingsWidth, row_width: f32) -> Self {
        if width == SettingsWidth::Compact && !gallery_toolbar_fits_chips(row_width) {
            Self::ChipsBelow
        } else {
            Self::Row
        }
    }
}

/// The toolbar: the view segment, then the gallery's filter and chips or
/// the Customize view's Save as theme, in `shape`.
fn toolbar_rows(
    state: &SettingsViewState,
    shape: ToolbarShape,
    page_width: f32,
) -> Vec<(UiNode, f32)> {
    let compact = shape != ToolbarShape::Row;
    let showcase = &state.showcase;
    let segment = |key: &str, label: &str, visual: &str, selected: bool| {
        let width = hug_width(visual).max(72.0);
        UiNode::new(
            key,
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(label).visual_label(visual),
                    ActionId::new(key),
                )
                .state(ControlState {
                    selected,
                    focused: focused(state, key),
                    ..ControlState::default()
                })
                .style(StyleRef::Navigation),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(width))
                .height(Length::Fill),
        )
    };
    let gallery = segment(
        VIEW_GALLERY,
        "Gallery: every theme",
        "Gallery",
        !showcase.customize,
    );
    let customize = segment(
        VIEW_CUSTOMIZE,
        "Customize: fonts, colours and palette",
        "Customize",
        showcase.customize,
    );
    let mut rows = Vec::new();
    if showcase.customize {
        let save = button(
            SHOWCASE_SAVE_AS,
            "Save as theme\u{2026}",
            "Save as Theme\u{2026}",
            StyleRef::Secondary,
            ControlState {
                selected: showcase.save.is_some(),
                focused: focused(state, SHOWCASE_SAVE_AS),
                ..ControlState::default()
            },
            Some(hug_width("Save as Theme\u{2026}").min(page_width / 2.0)),
        );
        let spacer = UiNode::new(
            "settings/showcase/toolbar/spacer",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill));
        rows.push(control_row(
            "settings/showcase/toolbar",
            "Theme view",
            vec![gallery, customize, spacer, save],
        ));
        return rows;
    }
    let chips: Vec<UiNode> = ShowcaseChip::ALL
        .into_iter()
        .map(|chip| {
            let key = format!("{CHIP_PREFIX}{}", chip.word());
            let visual = chip.label();
            UiNode::new(
                key.clone(),
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new(match chip {
                            ShowcaseChip::All => "Show every theme".to_string(),
                            ShowcaseChip::Yours => "Show your own themes".to_string(),
                            _ => format!("Show {} themes", chip.word()),
                        })
                        .visual_label(visual),
                        ActionId::new(key.clone()),
                    )
                    .state(ControlState {
                        selected: showcase.chip == chip,
                        focused: focused(state, &key),
                        ..ControlState::default()
                    })
                    .style(StyleRef::Navigation),
                ),
            )
            .layout(
                Layout::default()
                    .width(if compact {
                        Length::Fill
                    } else {
                        Length::Fixed(hug_width(visual).max(56.0))
                    })
                    .height(Length::Fill),
            )
        })
        .collect();
    let filter = text_field(
        state,
        SHOWCASE_FILTER,
        "Filter themes by name, description, dark or light",
        "Filter themes",
        &showcase.filter,
        None,
        false,
    );
    if compact {
        rows.push(control_row(
            "settings/showcase/toolbar",
            "Theme view",
            vec![gallery, customize, filter],
        ));
        rows.push(control_row(
            "settings/showcase/chips",
            "Theme filters",
            chips,
        ));
    } else {
        let mut children = vec![gallery, customize, filter];
        children.extend(chips);
        rows.push(control_row(
            "settings/showcase/toolbar",
            "Theme view",
            children,
        ));
    }
    rows
}

/// A card's height at the page's text scale; `short` draws the shorter
/// specimen, and its caption takes `meta_lines` lines.
fn card_height(short: bool, meta_lines: usize) -> f32 {
    let scale = settings_text_scale();
    CARD_PAD
        + tile_height(short, scale)
        + 5.0
        + title_height()
        + 1.0
        + meta_lines as f32 * meta_height()
        + CARD_PAD
}

/// The most lines a card's caption wraps to before it ends in an ellipsis.
const META_LINES: usize = 3;

/// A card's caption: Dark or Light, then its description. The tier is the
/// heading over the card's row ("SIGNATURE · 4 THEMES"), filtered or not, so
/// the caption does not say it again.
fn card_meta(card: &ShowcaseCard) -> String {
    let mut description = vec![
        if card.appearance == aterm_types::Appearance::Dark {
            "Dark"
        } else {
            "Light"
        }
        .to_string(),
    ];
    if !card.description.is_empty() {
        description.push(card.description.clone());
    }
    description.join(" \u{b7} ")
}

/// The lines a row of cards `card_width` wide gives its captions: what its
/// longest caption wraps to, so a description reads whole, up to `most`
/// (at most [`META_LINES`]).
fn row_meta_lines<'a>(
    cards: impl Iterator<Item = &'a ShowcaseCard>,
    card_width: f32,
    most: usize,
) -> usize {
    let (px, _) = crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Quiet);
    let inner = (card_width - 2.0 * CARD_PAD).max(0.0);
    cards
        .map(|card| wrapped_copy_lines(&card_meta(card), px, inner).len())
        .max()
        .unwrap_or(1)
        .clamp(1, most.clamp(1, META_LINES))
}

fn title_height() -> f32 {
    18.0_f32.max(17.0 * settings_text_scale())
}

fn meta_height() -> f32 {
    16.0_f32.max(15.0 * settings_text_scale())
}

/// How many cards a row of `width` points holds.
fn card_columns(width: f32) -> usize {
    let minimum = 200.0 * settings_text_scale().min(1.5);
    (((width + GUTTER) / (minimum + GUTTER)).floor() as usize).clamp(1, 4)
}

/// One card: the button that tries it (its whole area), the tile, and its
/// name and line under the tile; a Yours card also carries Open and Trash on
/// a row of their own under those.
fn card_node(
    state: &SettingsViewState,
    card: &ShowcaseCard,
    short: bool,
    card_width: f32,
    meta_lines: usize,
    configured: &str,
) -> UiNode {
    let key = format!("{CARD_PREFIX}{}", card.slug);
    let tried = state
        .showcase
        .trial_id()
        .is_some_and(|id| id.eq_ignore_ascii_case(&card.id));
    let current = card.id.eq_ignore_ascii_case(configured);
    // A Yours card's Open and Trash take a row of their own under its name
    // and line: beside them, at three cards a row, they left the name no
    // room at all ("…  IN USE").
    let yours = card.tier == ShowcaseTier::Yours;
    let inner = (card_width - 2.0 * CARD_PAD).max(0.0);
    let kicker = if tried {
        "TRYING"
    } else if current {
        "IN USE"
    } else {
        ""
    };
    let kicker_width = if kicker.is_empty() {
        0.0
    } else {
        let (px, face) =
            crate::native_ui::text_paint_metrics(SemanticRole::Status, StyleRef::Accent);
        crate::tray_raster::ui_text_width_for(face, kicker, px).ceil() + 2.0
    };
    let meta = card_meta(card);
    // The kicker is paint: assistive technology hears the same state in the
    // description, the theme in use included while another is tried.
    let spoken = match (tried, current) {
        (true, _) => format!("Trying \u{b7} {meta}"),
        (false, true) => format!("In use \u{b7} {meta}"),
        (false, false) => meta.clone(),
    };
    let mut spec = ButtonSpec::new(card.accessible_label()).visual_label("");
    spec.description = Some(spoken);
    let hit = UiNode::new(
        key.clone(),
        UiContent::Button(
            Control::new(spec, ActionId::new(key.clone()))
                .value(SemanticValue::Text(card.id.clone()))
                .state(ControlState {
                    selected: tried || (current && state.showcase.trial_id().is_none()),
                    focused: focused(state, &key),
                    ..ControlState::default()
                })
                .style(StyleRef::Secondary),
        ),
    )
    .layout(Layout::default().width(Length::Fill).height(Length::Fill));
    let tile = UiNode::new(
        format!("{key}/tile"),
        UiContent::Custom(AuditedCustomNode {
            audit_id: crate::native_ui::THEME_CARD_AUDIT,
            role: SemanticRole::Group,
            label: card.label.clone(),
            value: Some(card.tile_value(short)),
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
        }),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(tile_height(short, settings_text_scale()))),
    )
    .paint_only();
    let mut title_children = vec![
        UiNode::new(
            format!("{key}/name"),
            UiContent::Text(TextSpec {
                text: fit_text(
                    &card.label,
                    SemanticRole::Text,
                    StyleRef::Primary,
                    inner - kicker_width - 4.0,
                ),
                role: SemanticRole::Text,
                style: StyleRef::Primary,
            }),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill))
        .paint_only(),
    ];
    if !kicker.is_empty() {
        title_children.push(
            UiNode::new(
                format!("{key}/kicker"),
                UiContent::Text(TextSpec {
                    text: kicker.to_string(),
                    role: SemanticRole::Status,
                    style: StyleRef::Accent,
                }),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(kicker_width))
                    .height(Length::Fill),
            )
            .paint_only(),
        );
    }
    let title = UiNode::new(
        format!("{key}/title"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::row().height(Length::Fixed(title_height())).gap(4.0))
    .children(title_children)
    .paint_only();
    // The caption wraps to its row's lines, the last ending in an ellipsis
    // only where it is longer still; the button's description holds it whole.
    let (mut meta, _) = wrapped_copy_node(
        &format!("{key}/meta"),
        &meta,
        SemanticRole::Text,
        StyleRef::Quiet,
        inner,
        meta_height(),
        Some(meta_lines),
    );
    meta.layout.height = Length::Fixed(meta_lines as f32 * meta_height());
    let meta = meta.paint_only();
    let face = UiNode::new(
        format!("{key}/face"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Insets::all(CARD_PAD))
            .gap(0.0),
    )
    .children(vec![
        tile,
        spacer_node(&format!("{key}/gap-1"), 5.0),
        title,
        spacer_node(&format!("{key}/gap-2"), 1.0),
        meta,
    ])
    .paint_only();
    let mut children = vec![hit, face];
    if yours {
        children.push(card_file_actions(state, card, &key));
    }
    UiNode::new(
        format!("{key}/frame"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(card_height_for(card.tier, short, meta_lines))),
    )
    .children(children)
}

/// The height of a Yours card's Open and Trash row.
fn file_actions_height() -> f32 {
    22.0_f32.max(20.0 * settings_text_scale())
}

/// A card of `tier` whose caption takes `meta_lines` lines: a Yours card is
/// taller by its Open and Trash row.
fn card_height_for(tier: ShowcaseTier, short: bool, meta_lines: usize) -> f32 {
    card_height(short, meta_lines)
        + if tier == ShowcaseTier::Yours {
            file_actions_height() + 4.0
        } else {
            0.0
        }
}

/// A Yours card's Open and Trash, right-aligned on the card's last row, under
/// its name and line (the tile keeps its contrast badge and swatches in
/// view). Trash reads Confirm while one more press moves the file.
fn card_file_actions(state: &SettingsViewState, card: &ShowcaseCard, key: &str) -> UiNode {
    let height = file_actions_height();
    let armed = state.showcase.trash_armed.as_deref() == Some(card.slug.as_str());
    let open_key = format!("{key}/{CARD_OPEN}");
    let trash_key = format!("{key}/{CARD_TRASH}");
    let trash_visual = if armed { "Confirm" } else { "Trash" };
    // The theme in use (either side of a split) is never moved.
    let in_use = state
        .configured_theme_ids()
        .iter()
        .any(|id| id.eq_ignore_ascii_case(&card.id));
    let small = |action: &str, label: String, visual: &str| {
        let mut node = button(
            action,
            &label,
            visual,
            StyleRef::Secondary,
            ControlState {
                enabled: action == open_key || !in_use,
                focused: focused(state, action),
                ..ControlState::default()
            },
            Some(hug_width(if action == open_key {
                "Open"
            } else {
                "Confirm"
            })),
        );
        node.layout.height = Length::Fixed(height);
        node
    };
    let spacer = UiNode::new(
        format!("{key}/actions/spacer"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::default().width(Length::Fill).height(Length::Fill));
    let row = UiNode::new(
        format!("{key}/actions/row"),
        UiContent::Group(GroupSpec::new(format!("{} file", card.label))),
    )
    .layout(Layout::row().height(Length::Fixed(height)).gap(4.0))
    .children(vec![
        spacer,
        small(
            &open_key,
            format!("Open the {} theme file", card.label),
            "Open",
        ),
        small(
            &trash_key,
            if armed {
                format!("Confirm moving {} to the Trash", card.label)
            } else {
                format!("Move {} to the Trash", card.label)
            },
            trash_visual,
        ),
    ]);
    let top = UiNode::new(
        format!("{key}/actions/top"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::default().width(Length::Fill).height(Length::Fill));
    UiNode::new(
        format!("{key}/actions"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Insets {
                top: 0.0,
                right: CARD_PAD,
                bottom: CARD_PAD,
                left: CARD_PAD,
            }),
    )
    .children(vec![top, row])
}

fn spacer_node(key: &str, height: f32) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(height)),
    )
    .paint_only()
}

/// The height of a tier heading.
fn tier_heading_height() -> f32 {
    22.0_f32.max(16.0 * settings_text_scale())
}

/// A block of the paged body: a node built already, or a gallery row whose
/// cards are built only when its page is drawn.
enum PageBlock {
    Node(Box<UiNode>),
    Row(GalleryRow),
}

/// One row of gallery cards, unbuilt: the tier, the row's place in it, the
/// cards (indices into the gallery's cards), on a tier's first row how many
/// cards the tier holds (for its heading), and its captions' line count.
struct GalleryRow {
    tier: ShowcaseTier,
    row_index: usize,
    cards: Vec<usize>,
    heading: Option<usize>,
    /// The lines every caption in the row takes, so the cards stand level.
    meta_lines: usize,
}

/// The gallery's blocks — each a whole row of cards (the first row of a
/// tier with its heading) — with their heights and the slugs each draws.
/// No card is built here: the list builds a row's node when it lays it out
/// ([`gallery_row_node`]). A `gutter` (the scroll thumb's) is kept clear at
/// the trailing edge: the column count is the full width's, so a list that
/// scrolls keeps its columns.
fn gallery_blocks(
    state: &SettingsViewState,
    short: bool,
    column_width: f32,
    most_lines: usize,
    gutter: f32,
) -> Vec<(PageBlock, f32, Vec<String>)> {
    let columns = card_columns(column_width);
    let column_width = (column_width - gutter).max(0.0);
    let showcase = &state.showcase;
    let visible = showcase.visible();
    if visible.is_empty() {
        let query = showcase.filter.value().trim();
        let text = if query.is_empty() {
            format!(
                "No {} themes",
                showcase
                    .chip
                    .word()
                    .replace("yours", "theme files of your own")
            )
        } else {
            format!("No theme matches \u{201c}{query}\u{201d}")
        };
        let height = scaled_control_height();
        return vec![(
            PageBlock::Node(Box::new(line(
                "settings/showcase/empty",
                &text,
                SemanticRole::Status,
                StyleRef::Quiet,
                column_width,
                height,
            ))),
            height,
            Vec::new(),
        )];
    }
    let card_width = (column_width - GUTTER * (columns - 1) as f32) / columns as f32;
    let mut blocks = Vec::new();
    let mut start = 0;
    while start < visible.len() {
        let tier = showcase.cards[visible[start]].tier;
        let end = visible[start..]
            .iter()
            .position(|index| showcase.cards[*index].tier != tier)
            .map_or(visible.len(), |offset| start + offset);
        for (row_index, row) in visible[start..end].chunks(columns).enumerate() {
            let slugs: Vec<String> = row
                .iter()
                .map(|index| showcase.cards[*index].slug.clone())
                .collect();
            let meta_lines = row_meta_lines(
                row.iter().map(|index| &showcase.cards[*index]),
                card_width,
                most_lines,
            );
            let heading = (row_index == 0).then_some(end - start);
            let height = card_height_for(tier, short, meta_lines)
                + if heading.is_some() {
                    tier_heading_height() + 4.0
                } else {
                    0.0
                };
            blocks.push((
                PageBlock::Row(GalleryRow {
                    tier,
                    row_index,
                    cards: row.to_vec(),
                    heading,
                    meta_lines,
                }),
                height,
                slugs,
            ));
        }
        start = end;
    }
    blocks
}

/// Build one gallery row's node: its cards (a short last row padded to the
/// others' width) under the tier's heading on its first row.
fn gallery_row_node(
    state: &SettingsViewState,
    row: &GalleryRow,
    short: bool,
    column_width: f32,
    gutter: f32,
    configured: &str,
) -> UiNode {
    let showcase = &state.showcase;
    // The columns are the full width's; the cards share it less the gutter.
    let columns = card_columns(column_width);
    let column_width = (column_width - gutter).max(0.0);
    let card_width = (column_width - GUTTER * (columns - 1) as f32) / columns as f32;
    let GalleryRow {
        tier,
        row_index,
        cards,
        heading,
        meta_lines,
    } = row;
    let mut cells: Vec<UiNode> = cards
        .iter()
        .map(|index| {
            card_node(
                state,
                &showcase.cards[*index],
                short,
                card_width,
                *meta_lines,
                configured,
            )
        })
        .collect();
    let row_key = format!(
        "settings/showcase/row/{}/{row_index}",
        tier.label().to_ascii_lowercase().replace(' ', "-")
    );
    // A short last row keeps its cards the width of the rest.
    for pad in cards.len()..columns {
        cells.push(
            UiNode::new(
                format!("{row_key}/pad-{pad}"),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
        );
    }
    let row_height = card_height_for(*tier, short, *meta_lines);
    let row_node = UiNode::new(
        row_key.clone(),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::row().height(Length::Fixed(row_height)).gap(GUTTER))
    .children(cells);
    let Some(count) = heading else {
        return row_node;
    };
    let heading_height = tier_heading_height();
    let heading = line(
        format!("{row_key}/heading"),
        &format!(
            "{} \u{b7} {count} {}",
            tier.label(),
            if *count == 1 { "THEME" } else { "THEMES" }
        ),
        SemanticRole::Heading,
        StyleRef::Quiet,
        column_width,
        heading_height,
    );
    UiNode::new(
        format!("{row_key}/block"),
        UiContent::Group(GroupSpec::new(format!(
            "{} themes",
            tier.label().to_lowercase()
        ))),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(heading_height + 4.0 + row_height))
            .gap(4.0),
    )
    .children(vec![heading, row_node])
}

/// The stage: the Appearance specimen in the terminal's own font, drawing
/// the hovered or focused card (else the trial, else the configured theme)
/// under every override and draft.
fn stage_node(state: &SettingsViewState, height: f32, cx: &ViewCx<'_>) -> Option<UiNode> {
    // The stage shows the theme, or — while the font picker is open — the
    // typography sample of the family under the highlight.
    let key = state
        .choice_picker
        .as_ref()
        .filter(|picker| picker.fonts)
        .map_or(prefs::EDIT_THEME, |picker| picker.key.as_str());
    let spec = renderer_preview_spec_for_key_with_font(
        state,
        key,
        cx.animation_phase_ms,
        cx.motion,
        cx.terminal_font_px,
        cx.terminal_scale,
        cx.semantic_font.as_ref(),
    )?
    // The stage shows one theme: its window sample takes that theme's side,
    // not the OS's, so a light theme is not framed by a dark titlebar.
    .with_window_following_theme(true);
    Some(crate::settings_preview::preview_node(
        "settings/showcase/stage",
        spec,
        height,
    ))
}

/// A Customize section's heading over its first row.
fn with_heading(key: &str, heading: &str, block: (UiNode, f32), width: f32) -> (UiNode, f32) {
    let heading_height = tier_heading_height();
    let height = heading_height + 4.0 + block.1;
    (
        UiNode::new(
            format!("{key}/block"),
            UiContent::Group(GroupSpec::new(heading.to_lowercase())),
        )
        .layout(Layout::column().height(Length::Fixed(height)).gap(4.0))
        .children(vec![
            line(
                format!("{key}/heading"),
                heading,
                SemanticRole::Heading,
                StyleRef::Quiet,
                width,
                heading_height,
            ),
            block.0,
        ]),
        height,
    )
}

/// One registry row of the Customize view, with its Reset (enabled only
/// for a value the file sets, or Discard while a draft is held).
fn customize_row(
    state: &SettingsViewState,
    key: &str,
    width: SettingsWidth,
    column_width: f32,
) -> Option<(UiNode, f32)> {
    let index = state.field_index_of(key)?;
    let field = state.legacy.fields.get(index)?;
    // The page's column is narrower than a field page's beside the stage:
    // the control gives up width before the label is cut.
    let control_width = (column_width * 0.42).clamp(160.0, 300.0 * settings_text_scale().min(1.25));
    let mut row = finish_setting_row_narrowed(
        state,
        index,
        field,
        width,
        column_width,
        prefs::footnote_of(key),
        control_width,
    );
    let explicit = state.is_explicit(key);
    let colour = SHOWCASE_COLOR_KEYS.contains(&key).then_some(field.label);
    reset_button_for_theme(&mut row, key, explicit, colour);
    let height = fixed_height(&row);
    Some((row, height))
}

/// The row's Reset says what it resets to and is off while there is nothing
/// to reset. `colour` is a colour row's label: its Reset names the setting
/// as the row does.
fn reset_button_for_theme(node: &mut UiNode, key: &str, explicit: bool, colour: Option<&str>) {
    let reset_key = format!("settings/reset/{key}");
    if node.key.as_str() == reset_key {
        if let UiContent::Button(control) = &mut node.content {
            control.state.enabled = control.state.enabled && explicit;
            if let Some(label) = colour {
                control.spec.label = format!("Reset {label} to the theme's colour");
            }
        }
        return;
    }
    for child in &mut node.children {
        reset_button_for_theme(child, key, explicit, colour);
    }
}

/// The palette: sixteen chips (normal over bright), the open slot's editor,
/// and Reset palette.
fn palette_blocks(state: &SettingsViewState, column_width: f32) -> Vec<(UiNode, f32)> {
    let colors = state.showcase_colors().scheme.ansi;
    let chip_height = 32.0_f32.max(28.0 * settings_text_scale());
    let chip_row = |row: usize| {
        let chips: Vec<UiNode> = (row * 8..row * 8 + 8)
            .map(|index| {
                let key = format!("{PALETTE_PREFIX}{index}");
                let hex = hex_of(colors[index]);
                let selected = state
                    .showcase
                    .slot
                    .as_ref()
                    .is_some_and(|slot| slot.index == index);
                let hit = UiNode::new(
                    key.clone(),
                    UiContent::Button(
                        Control::new(
                            ButtonSpec::new(format!(
                                "Palette colour {index}, {}: {hex}",
                                SLOT_NAMES[index]
                            ))
                            .visual_label(""),
                            ActionId::new(key.clone()),
                        )
                        .value(SemanticValue::Text(hex.clone()))
                        .state(ControlState {
                            selected,
                            focused: focused(state, &key),
                            ..ControlState::default()
                        })
                        // The slot being edited wears the selected chip's
                        // accent rim and wash; a Secondary button's selected
                        // state is all but hidden under its swatch.
                        .style(if selected {
                            StyleRef::Navigation
                        } else {
                            StyleRef::Secondary
                        }),
                    ),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill));
                let swatch = UiNode::new(
                    format!("{key}/swatch"),
                    UiContent::Custom(AuditedCustomNode {
                        audit_id: crate::native_ui::THEME_SWATCH_AUDIT,
                        role: SemanticRole::Group,
                        // The swatch paints no words: its button above says
                        // the slot and its hex.
                        label: String::new(),
                        value: Some(hex),
                        action: None,
                        focusable: false,
                        focused: false,
                        focus_visible: false,
                    }),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill))
                .paint_only();
                let face = UiNode::new(
                    format!("{key}/face"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .padding(Insets::all(4.0)),
                )
                .children(vec![swatch])
                .paint_only();
                UiNode::new(
                    format!("{key}/frame"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill))
                .children(vec![hit, face])
            })
            .collect();
        UiNode::new(
            format!("settings/showcase/palette-row/{row}"),
            UiContent::Group(GroupSpec::new(if row == 0 {
                "Normal colours"
            } else {
                "Bright colours"
            })),
        )
        .layout(Layout::row().height(Length::Fixed(chip_height)).gap(6.0))
        .children(chips)
    };
    let grid_height = 2.0 * chip_height + 6.0;
    let grid = UiNode::new(
        "settings/showcase/palette",
        UiContent::Group(GroupSpec::new("Palette")),
    )
    .layout(Layout::column().height(Length::Fixed(grid_height)).gap(6.0))
    .children(vec![chip_row(0), chip_row(1)]);
    let pending = state.config_key_pending(prefs::EDIT_PALETTE)
        || state.config_key_pending(prefs::EDIT_PALETTE_OVERRIDES);
    let mut controls = Vec::new();
    if let Some(SlotEditor { index, input, .. }) = state.showcase.slot.as_ref() {
        let swatch = aterm_types::scheme::parse_hex(input.value().trim())
            .map(|color| [color.r, color.g, color.b]);
        controls.push(text_field(
            state,
            SHOWCASE_PALETTE_SLOT,
            &format!("Palette colour {index}, {}", SLOT_NAMES[*index]),
            "#rrggbb",
            input,
            swatch,
            swatch.is_none(),
        ));
        controls.push(button(
            PALETTE_SLOT_RESET,
            &format!("Reset palette colour {index} to the theme's"),
            "Reset Colour",
            StyleRef::Quiet,
            ControlState {
                enabled: !pending,
                ..ControlState::default()
            },
            Some(hug_width("Reset Colour")),
        ));
        controls.push(button(
            PALETTE_SLOT_CLOSE,
            "Close the palette colour editor",
            "Done",
            StyleRef::Quiet,
            ControlState::default(),
            Some(hug_width("Done")),
        ));
    } else {
        controls.push(
            UiNode::new(
                "settings/showcase/palette-hint",
                UiContent::Text(TextSpec {
                    text: fit_text(
                        "Choose a colour to change it; Return saves",
                        SemanticRole::Text,
                        StyleRef::Quiet,
                        column_width - hug_width("Reset Palette") - 6.0,
                    ),
                    role: SemanticRole::Text,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
        );
    }
    controls.push(button(
        PALETTE_RESET_ALL,
        "Reset the palette to the theme's colours",
        "Reset Palette",
        StyleRef::Quiet,
        ControlState {
            enabled: (state.is_explicit(prefs::EDIT_PALETTE)
                || state.is_explicit(prefs::EDIT_PALETTE_OVERRIDES)
                || state.field_has_unsaved_draft(prefs::EDIT_PALETTE))
                && !pending,
            focused: focused(state, PALETTE_RESET_ALL),
            ..ControlState::default()
        },
        Some(hug_width("Reset Palette")),
    ));
    // The swatches and their editor are one block, so they page together:
    // choosing a swatch never opens its editor on a page not shown.
    let (tools, tools_height) = control_row(
        "settings/showcase/palette-tools",
        "Palette colour",
        controls,
    );
    let section_height = grid_height + 6.0 + tools_height;
    let section = UiNode::new(
        "settings/showcase/palette-and-tools",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(section_height))
            .gap(6.0),
    )
    .children(vec![grid, tools]);
    vec![with_heading(
        "settings/showcase/palette-section",
        "PALETTE",
        (section, section_height),
        column_width,
    )]
}

/// The Save as theme row: the name, Save and Cancel, and why a name cannot
/// be used.
fn save_blocks(state: &SettingsViewState, column_width: f32) -> Vec<(UiNode, f32)> {
    let Some(save) = state.showcase.save.as_ref() else {
        return Vec::new();
    };
    let problem = save.error.clone();
    let field = text_field(
        state,
        SHOWCASE_SAVE_NAME,
        "New theme name",
        "Theme name",
        &save.name,
        None,
        problem.is_some(),
    );
    let confirm = button(
        SAVE_CONFIRM,
        "Save the colours shown as a new theme",
        "Save",
        StyleRef::Secondary,
        ControlState {
            enabled: problem.is_none(),
            focused: focused(state, SAVE_CONFIRM),
            ..ControlState::default()
        },
        Some(hug_width("Save").max(72.0)),
    );
    let cancel = button(
        SAVE_CANCEL,
        "Cancel saving a theme",
        "Cancel",
        StyleRef::Quiet,
        ControlState {
            focused: focused(state, SAVE_CANCEL),
            ..ControlState::default()
        },
        Some(hug_width("Cancel")),
    );
    let mut blocks = vec![with_heading(
        "settings/showcase/save-section",
        "SAVE AS THEME",
        control_row(
            "settings/showcase/save-row",
            "Save as theme",
            vec![field, confirm, cancel],
        ),
        column_width,
    )];
    let note = problem.unwrap_or_else(|| {
        "Writes the colours shown to your themes folder, then uses it".to_string()
    });
    let height = 20.0_f32.max(18.0 * settings_text_scale());
    blocks.push((
        line(
            "settings/showcase/save-note",
            &note,
            SemanticRole::Status,
            StyleRef::Quiet,
            column_width,
            height,
        ),
        height,
    ));
    blocks
}

/// The Customize view's blocks: the stage (unless it stands beside), the
/// Save as row, the type rows, the colour rows, the look rows and the
/// palette.
fn customize_blocks(
    state: &SettingsViewState,
    width: SettingsWidth,
    column_width: f32,
    stage_beside: bool,
    cx: &ViewCx<'_>,
) -> Vec<(UiNode, f32)> {
    let mut blocks = Vec::new();
    if !stage_beside {
        let height = renderer_preview_height(width);
        if let Some(stage) = stage_node(state, height, cx) {
            blocks.push((stage, height));
        }
    }
    blocks.extend(save_blocks(state, column_width));
    for (heading, keys) in [
        ("TYPE", SHOWCASE_TYPE_KEYS.as_slice()),
        ("COLOURS", SHOWCASE_COLOR_KEYS.as_slice()),
        ("LOOK", SHOWCASE_LOOK_KEYS.as_slice()),
    ] {
        let mut first = true;
        for key in keys {
            let Some(row) = customize_row(state, key, width, column_width) else {
                continue;
            };
            blocks.push(if first {
                with_heading(
                    &format!("settings/showcase/{}", heading.to_lowercase()),
                    heading,
                    row,
                    column_width,
                )
            } else {
                row
            });
            first = false;
        }
    }
    blocks.extend(palette_blocks(state, column_width));
    blocks
}

/// THE PAGE: its heading, the trial bar and toolbar (pinned above the list
/// where a screen still holds a block under them), then the gallery's rows
/// or the Customize view's sections as one scrolling list; on a Wide page
/// the stage stands beside it.
pub(super) fn theme_showcase_page(
    state: &SettingsViewState,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> Vec<UiNode> {
    let viewport = cx.viewport;
    let compact = width == SettingsWidth::Compact;
    let gap = settings_page_gap(state, width, viewport);
    let maximum = page_maximum(SettingsRoute::Themes, width);
    let page_width = settings_page_content_width(viewport.width, width, maximum);
    let (mut out, mut used) = pinned_heading(
        width,
        viewport,
        gap,
        SHOWCASE_TITLE,
        route_subtitle(SettingsRoute::Themes),
    );
    // An open picker (the Customize view's font family) is the page's whole
    // decision surface, as on a field page: the stage above it previews the
    // highlighted row, and Done or Escape brings the rows back.
    if state.showcase.customize && state.choice_picker.is_some() {
        if let Some(stage) = stage_node(state, renderer_preview_height(width), cx) {
            out.push(stage);
        }
        out.extend(choice_picker_node(state, width, viewport, &out));
        return out;
    }
    let body = settings_list_body_height(state, width, viewport);
    // On a Wide page the stage, in the terminal's own font, stands beside
    // the gallery (drawing the card under the pointer or the keyboard) and
    // beside the Customize view's rows.
    let stage_beside = width == SettingsWidth::Wide;
    let column_width = if stage_beside {
        (page_width - STAGE_WIDTH - GUTTER).max(0.0)
    } else {
        page_width
    };
    let sticky_rows = |gutter: f32| -> Vec<(UiNode, f32)> {
        let row_width = (page_width - gutter).max(0.0);
        let mut sticky: Vec<(UiNode, f32)> = trial_bar(state, width, row_width);
        sticky.extend(toolbar_rows(
            state,
            ToolbarShape::for_row(width, row_width),
            row_width,
        ));
        sticky
    };
    let mut sticky = sticky_rows(0.0);
    // A short page draws the shorter specimen, so a screen holds two rows.
    let short = compact || body < 640.0;
    // The trial bar and the toolbar stay pinned over the list while it keeps
    // room for its tallest block; on a short host or at large type they
    // scroll with it instead.
    let sticky_height: f32 = sticky.iter().map(|(_, height)| height + gap).sum();
    let to_first_cards = |blocks: &[(PageBlock, f32, Vec<String>)]| -> f32 {
        blocks
            .iter()
            .position(|(_, _, slugs)| !slugs.is_empty())
            .map_or(0.0, |row| {
                blocks[..=row]
                    .iter()
                    .map(|(_, height, _)| height + gap)
                    .sum()
            })
    };
    let list_blocks = |gutter: f32| -> Vec<(PageBlock, f32, Vec<String>)> {
        if state.showcase.customize {
            // Its rows are cards, whose own padding holds the thumb.
            return customize_blocks(state, width, column_width, stage_beside, cx)
                .into_iter()
                .map(|(node, height)| (PageBlock::Node(Box::new(node)), height, Vec::new()))
                .collect();
        }
        // Captions read whole where a screen holds the toolbar and a whole
        // row of cards; a host too short for that wraps them to fewer lines
        // (the card's description is whole in its spoken label) before its
        // first screen would hold no card at all.
        let mut most_lines = META_LINES;
        let mut blocks = gallery_blocks(state, short, column_width, most_lines, gutter);
        while most_lines > 1 && body - sticky_height < to_first_cards(&blocks) {
            most_lines -= 1;
            blocks = gallery_blocks(state, short, column_width, most_lines, gutter);
        }
        let (footer, height) = gallery_footer(state, (column_width - gutter).max(0.0));
        blocks.push((PageBlock::Node(Box::new(footer)), height, Vec::new()));
        blocks
    };
    let mut blocks = list_blocks(0.0);
    // A short gallery's title gives way where that is what lets its first
    // screen hold the toolbar and a whole row of cards (the default 80x24
    // window): the sidebar and the window already name the page.
    let first_cards = to_first_cards(&blocks);
    if !state.showcase.customize
        && body - used - sticky_height < first_cards
        && body - sticky_height >= first_cards
    {
        out.clear();
        used = 0.0;
    }
    let tallest = blocks
        .iter()
        .map(|(_, height, _)| *height)
        .fold(0.0_f32, f32::max);
    // A feedback bar can leave too little room even after the toolbar
    // scrolls away. Give the optional title to the tallest complete block
    // when that alone makes it fit; otherwise no page can show it whole.
    if !state.showcase.customize && body - used < tallest && body >= tallest {
        out.clear();
        used = 0.0;
    }
    let moved = body - used - sticky_height < tallest;
    if !moved {
        used += sticky_height;
    }
    // THE THUMB'S GUTTER: a list that scrolls draws its thumb at its trailing
    // edge (`paint_messages_scroll`, 3pt at 7pt in), and the gallery's rows
    // run to that edge (the Filter field, the chips, the last card's frame),
    // so where the list scrolls they are laid out THUMB_GUTTER short of it.
    // Other pages' cards hold the thumb in their own padding.
    let span = |sticky: &[(UiNode, f32)], blocks: &[(PageBlock, f32, Vec<String>)]| {
        let heights: Vec<f32> = sticky
            .iter()
            .filter(|_| moved)
            .map(|(_, height)| *height)
            .chain(blocks.iter().map(|(_, height, _)| *height))
            .collect();
        heights.iter().sum::<f32>() + heights.len().saturating_sub(1) as f32 * gap
    };
    let overhang = settings_list_overhang(state, width, viewport);
    let scrolls = span(&sticky, &blocks) > (body - used).max(0.0) + overhang + 0.01;
    let gutter = if scrolls { THUMB_GUTTER } else { 0.0 };
    if scrolls {
        blocks = list_blocks(gutter);
        if moved {
            sticky = sticky_rows(gutter);
        }
    }
    let clear_of_thumb = |mut node: UiNode| {
        node.layout.padding.right += gutter;
        node
    };
    // A gallery block clears the thumb (a row of cards does when it is
    // built, below); the Customize view's cards hold it in their padding.
    let clear_block = |block: PageBlock| match block {
        PageBlock::Node(node) if !state.showcase.customize => {
            PageBlock::Node(Box::new(clear_of_thumb(*node)))
        }
        block => block,
    };
    if moved {
        let mut list: Vec<(PageBlock, f32, Vec<String>)> = std::mem::take(&mut sticky)
            .into_iter()
            .map(|(node, height)| {
                (
                    PageBlock::Node(Box::new(clear_of_thumb(node))),
                    height,
                    Vec::new(),
                )
            })
            .collect();
        list.extend(
            blocks
                .into_iter()
                .map(|(block, height, slugs)| (clear_block(block), height, slugs)),
        );
        blocks = list;
    } else {
        out.extend(sticky.into_iter().map(|(node, _)| node));
        blocks = blocks
            .into_iter()
            .map(|(block, height, slugs)| (clear_block(block), height, slugs))
            .collect();
    }
    // Every card is in the list; the keyboard steps through them in order
    // and the list follows (`step_card_focus`).
    *state.showcase.drawn.borrow_mut() = blocks
        .iter()
        .filter(|(_, _, slugs)| !slugs.is_empty())
        .map(|(_, _, slugs)| slugs.clone())
        .collect();
    let room = (body - used).max(0.0);
    let configured = state.configured_theme_id();
    let items: Vec<ListItem> = blocks
        .into_iter()
        .map(|(block, height, slugs)| {
            let node = match block {
                PageBlock::Node(node) => *node,
                PageBlock::Row(row) => clear_of_thumb(gallery_row_node(
                    state,
                    &row,
                    short,
                    column_width,
                    gutter,
                    &configured,
                )),
            };
            // A row of cards is one stop: its cards scroll together.
            if slugs.is_empty() {
                ListItem::of(node, height)
            } else {
                ListItem::Card(Box::new(node), height)
            }
        })
        .collect();
    let list = scroll::list_page_peeking(
        state,
        "settings/list",
        if state.showcase.customize {
            "Customize theme"
        } else {
            "Themes"
        },
        items,
        room,
        gap,
        overhang,
    );
    if stage_beside {
        out.push(showcase_beside_stage(state, list, room, cx));
    } else {
        out.push(list);
    }
    out
}

/// The words under the stage. The stage draws a sample (the card pointed at
/// or the one being tried), never the app-wide state: whether a theme is
/// being tried across the app is the trial bar's to say, so a caption that
/// outlives a Revert never claims a preview the window is not showing.
pub(super) fn stage_caption(sample: Option<&str>, customize: bool) -> String {
    match sample {
        Some(label) => format!("Sample: {label}"),
        None if customize => "In your terminal font and colours".to_string(),
        None => "Point at a theme to see a sample here".to_string(),
    }
}

/// The list with the stage column beside it: the rows scroll, the stage
/// stays.
fn showcase_beside_stage(
    state: &SettingsViewState,
    list: UiNode,
    room: f32,
    cx: &ViewCx<'_>,
) -> UiNode {
    let caption_height = 20.0_f32.max(16.0 * settings_text_scale());
    // The Appearance specimen draws three rows and its palette: past the
    // height every other Settings page gives it, the stage is empty ground.
    let stage_height =
        (room - caption_height - 6.0).min(renderer_preview_height(SettingsWidth::Wide));
    let mut side = Vec::new();
    if stage_height >= 96.0
        && let Some(stage) = stage_node(state, stage_height, cx)
    {
        let sample = state
            .showcase_candidate()
            .map(|card| card.label.clone())
            .or_else(|| {
                state.showcase.trial_id().map(|id| {
                    state
                        .showcase
                        .card(id)
                        .map_or_else(|| id.to_string(), |card| card.label.clone())
                })
            });
        let caption = stage_caption(sample.as_deref(), state.showcase.customize);
        side.push(stage);
        side.push(line(
            "settings/showcase/stage-caption",
            &caption,
            SemanticRole::Text,
            StyleRef::Quiet,
            STAGE_WIDTH,
            caption_height,
        ));
    }
    let height = fixed_height(&list).max(if side.is_empty() {
        0.0
    } else {
        stage_height + 6.0 + caption_height
    });
    let column = UiNode::new(
        "settings/showcase/body",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::column().width(Length::Fill).height(Length::Fill))
    .children(vec![list]);
    let stage_column = UiNode::new(
        "settings/showcase/stage-column",
        UiContent::Group(GroupSpec::new("Theme preview")),
    )
    .layout(
        Layout::column()
            .width(Length::Fixed(STAGE_WIDTH))
            .height(Length::Fill)
            .gap(6.0),
    )
    .children(side);
    UiNode::new(
        "settings/showcase/layout",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::row().height(Length::Fixed(height)).gap(GUTTER))
    .children(vec![column, stage_column])
}
