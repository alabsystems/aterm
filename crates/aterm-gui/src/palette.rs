// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The own-rendered, introspectable command palette: a [`DrawPrim`] card built from the
//! platform-neutral [`crate::menu::MENU_MODEL`], so the application menu works on every
//! platform, including Linux, which has no native menu bar ([`crate::menu`] stubs
//! `install` there).
//!
//! Each command flattens to a [`PaletteRow`]. Menu rows resolve `enabled` / `checked` from
//! live `App` state as `validateMenuItem:` would. The active native tab app contributes its
//! own section; those rows keep the exact window, instance, view, lifecycle generation and
//! reducer action that produced them. A `query` narrows the list (fuzzy subsequence over
//! `label` + `section`, plus a theme row's appearance). Menu activation converges on
//! `Wake::MenuAction`; native activation is generation-checked before it becomes
//! `AppEvent::Action`.
//!
//! A Theme section follows the menu rows: one row per theme the catalog can draw
//! ([`PaletteTheme`], built-ins then loaded theme files), its appearance as the hint.
//! Highlighting one previews it ([`PaletteState::take_theme_highlight_change`], read by
//! `App::sync_palette_theme_preview` once per event-loop turn), Enter writes it, and
//! closing the palette reverts. View ▸ Choose Theme… opens the palette scoped to that
//! section ([`PaletteState::themes_only`]).

use std::borrow::Cow;
use std::time::Instant;

#[cfg(test)]
use aterm_render::Theme;

use crate::menu::{MENU_MODEL, MenuAction, MenuEntry, MenuMods};
use crate::native_app::Command;
use crate::native_ui::ActionId;
use crate::settings::{Roles, SettingsGeom, fit, text_w};
use crate::tray_raster::row_baseline;
use crate::type_scale::{StepPx, TypeStep};
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// The most command rows the palette shows at once; beyond this the filtered list scrolls
/// (the `body` band the painter, the scroll clamp, and the selection move all agree on).
const MAX_CMD_ROWS: usize = 14;

/// Chrome rows framing the command band: the title and pinned query row on top, the
/// key-hint footer below.
const CHROME_ROWS: usize = 3;

/// Maximum logical width of the floating command card, so wide windows keep enough of the
/// owning app visible to read as a modal layer.
const MAX_CARD_WIDTH: f32 = 820.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct PaletteLayout {
    card: (f32, f32, f32, f32),
    card_rows: usize,
    body_rows: usize,
}

/// One pure geometry projection shared by paint and hit-testing. The overlay supplies the
/// whole available viewport; the command card remains content-height, width-capped, and
/// centred within it. Tiny windows retain at least a two-cell-wide card before margins
/// collapse, and height clamps to the rows that actually exist.
fn palette_layout_for_count(count: usize, g: &SettingsGeom) -> PaletteLayout {
    let tray_w = (g.cols as f32 * g.cw).max(0.0);
    let tray_h = (g.panel_rows as f32 * g.ch).max(0.0);
    let desired_margin = (g.cw * 2.0).max(16.0);
    let max_margin = (tray_w * 0.5 - g.cw.max(0.0)).max(0.0);
    let margin = desired_margin.min(max_margin);
    let card_w = (tray_w - margin * 2.0).clamp(0.0, MAX_CARD_WIDTH);
    let card_rows = (CHROME_ROWS + count.clamp(1, MAX_CMD_ROWS)).min(g.panel_rows);
    let card_h = (card_rows as f32 * g.ch).clamp(0.0, tray_h);
    let card_x = ((tray_w - card_w) * 0.5).max(0.0);
    let card_y = ((tray_h - card_h) * 0.5).max(0.0);
    PaletteLayout {
        card: (card_x, card_y, card_w, card_h),
        card_rows,
        body_rows: count
            .min(MAX_CMD_ROWS)
            .min(card_rows.saturating_sub(CHROME_ROWS)),
    }
}

#[cfg(test)]
fn palette_layout(state: &PaletteState, g: &SettingsGeom) -> PaletteLayout {
    palette_layout_for_count(state.filtered_count(), g)
}

/// Exact native reducer destination captured when its command row enters the palette.
/// Stable identity plus the lifecycle generation prevent a delayed activation from being
/// redirected to whichever app happens to be active later.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NativeCommandTarget {
    pub(crate) window: crate::WindowId,
    pub(crate) instance: crate::tab_model::AppInstanceId,
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) generation: u64,
    pub(crate) action: ActionId,
}

/// The command domains share pixels, filtering, controls, and accessibility while
/// keeping their dispatch authorities distinct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaletteTarget {
    Menu(MenuAction),
    Native(NativeCommandTarget),
    /// A Theme-section row: the theme id `theme = "…"` takes (a built-in's canonical
    /// name or a theme file's stem).
    Theme(String),
}

impl PaletteTarget {
    fn menu(&self) -> Option<MenuAction> {
        match self {
            Self::Menu(action) => Some(*action),
            Self::Native(_) | Self::Theme(_) => None,
        }
    }

    fn theme(&self) -> Option<&str> {
        match self {
            Self::Theme(id) => Some(id),
            Self::Menu(_) | Self::Native(_) => None,
        }
    }

    fn control_name(&self) -> Cow<'_, str> {
        match self {
            Self::Menu(action) => Cow::Owned(format!("{action:?}")),
            Self::Native(target) => Cow::Borrowed(target.action.as_str()),
            Self::Theme(_) => Cow::Borrowed("Theme"),
        }
    }
}

/// The Theme section's chip, and the word the theme rows' filter haystack carries.
pub(crate) const THEME_SECTION: &str = "Theme";

/// The Theme section's last row: Settings ▸ Themes, the Theme Showcase.
pub(crate) const BROWSE_THEMES_LABEL: &str = "Browse all themes\u{2026}";

/// One theme the palette offers: a built-in or a theme file that loaded. Built by
/// `App::palette_themes` from the admitted catalog, so the palette reads no file and
/// resolves nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaletteTheme {
    /// What `theme = "…"` takes.
    pub(crate) id: String,
    /// The row's text: the theme's name, joined by its id when the two differ.
    pub(crate) label: String,
    /// The appearance it is drawn for, shown as the row's hint.
    pub(crate) appearance: aterm_types::Appearance,
    /// The theme the config draws (checked).
    pub(crate) current: bool,
}

impl PaletteTheme {
    fn row(self) -> PaletteRow {
        PaletteRow {
            section: Cow::Borrowed(THEME_SECTION),
            label: Cow::Owned(self.label),
            action: PaletteTarget::Theme(self.id),
            key: "",
            mods: MenuMods::None,
            shortcut: Cow::Borrowed(match self.appearance {
                aterm_types::Appearance::Dark => "dark",
                aterm_types::Appearance::Light => "light",
            }),
            enabled: true,
            checked: self.current.then_some(true),
        }
    }
}

// Retain the compact `row.action == MenuAction::…` idiom used by the menu-model tests.
impl PartialEq<MenuAction> for PaletteTarget {
    fn eq(&self, other: &MenuAction) -> bool {
        self.menu() == Some(*other)
    }
}

/// One flattened command, a single row of the palette. Menu rows come from a
/// `MenuEntry::Item`, native rows from the active app's [`Command`] metadata, theme rows
/// from [`PaletteTheme`].
/// Disabled commands remain visible but cannot produce a target.
#[derive(Clone, Debug)]
pub(crate) struct PaletteRow {
    /// The dim section chip: the menu's title (or submenu label), the native app's name, or
    /// [`THEME_SECTION`].
    pub section: Cow<'static, str>,
    /// The command label. `Cow` because [`PaletteState::resolve`] writes some live ones:
    /// the update row's version, a fleet hold's reason.
    pub label: Cow<'static, str>,
    /// The command this row dispatches. Menu and native authority never alias.
    pub action: PaletteTarget,
    /// The visual key-equivalent character ("" for none), right-aligned as an accelerator.
    pub key: &'static str,
    /// The accelerator's modifier mask (rendered ⌘ / ⇧⌘ / ⌃⌘ before `key`).
    pub mods: MenuMods,
    /// Native-app shortcut metadata, a theme row's appearance, or off macOS a menu row's
    /// effective chord hint; when empty, a menu row derives its accelerator from `mods` +
    /// `key`.
    pub shortcut: Cow<'static, str>,
    /// Whether the command is currently actionable; a disabled row paints dim and Enter
    /// on it is a no-op (mirroring `validateMenuItem:` greying).
    pub enabled: bool,
    /// A checkbox state for toggle commands (e.g. Serious Mode); `None` for plain commands.
    pub checked: Option<bool>,
}

/// Native command snapshot prepended to the shared command surface. The section is owned
/// because app names are selected dynamically, while command titles/shortcuts arrive as
/// owned reducer metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeCommandScope {
    pub(crate) window: crate::WindowId,
    pub(crate) instance: crate::tab_model::AppInstanceId,
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) generation: u64,
    pub(crate) section: String,
    pub(crate) commands: Vec<Command>,
}

/// Live `App` predicates that resolve per-row `enabled`/`checked`, the conditions
/// `validateMenuItem:` uses. Computed by `App::palette_live` and fed to
/// [`PaletteState::resolve`], so this module stays `App`-free.
#[derive(Clone, Debug, Default)]
pub(crate) struct PaletteLive {
    /// A text selection exists (gates Copy).
    pub has_selection: bool,
    /// The front session's effective matrix-rain state (its runtime override, else the
    /// config bit): the checkmark on Matrix Rain. False with no front terminal.
    pub rain_on: bool,
    /// The front window's promotable kitty (its tenured program cat on glass, else the
    /// launch kitty) is the pinned favourite: the checkmark on Favourite This Kitty.
    pub kitty_favourited: bool,
    /// Process-wide serious mode is active (checkmark on Serious Mode).
    pub serious_mode: bool,
    /// The window is full-screen (the row reads Exit Full Screen).
    pub fullscreen: bool,
    /// The front tab is split, so ⌘W closes the focused pane (the row reads
    /// Close Pane).
    pub front_tab_split: bool,
    /// The front window has more than one tab (gates Next/Previous Tab).
    pub multi_tab: bool,
    /// The active whole tab is a native app rather than a terminal pane tree. Terminal-only
    /// split/session-window commands stay visible but disabled.
    pub native_tab_active: bool,
    /// The frontmost window's front content is a terminal session. Unlike
    /// `!native_tab_active`, also false with no window frontmost (macOS runs
    /// windowless), so per-session rows refuse instead of silently no-op'ing.
    pub terminal_front: bool,
    /// Whether the front window can present a rename editor (the tab strip, else a native
    /// toolbar); without one the row greys rather than accept a dead click.
    pub can_rename: bool,
    /// The front session has a theme of its own (gates Clear Tab Theme).
    pub tab_theme: bool,
    /// At least one native tab snapshot is available for identity-fresh reopen.
    pub can_reopen_closed_tab: bool,
    /// A non-expired split-leaf recovery record is available.
    pub can_reopen_closed_view: bool,
    /// This platform has a real local-file picker. Picker-backed rows remain visible but
    /// disabled where `menu::choose_local_file` cannot produce a path.
    pub local_file_picker_available: bool,
    /// The standing hold on the front session, if any: `fleet` decides which of the
    /// Fabric menu's Hold / Lift Hold is live, and a fleet hold's `reason` rides the
    /// greyed rows' labels.
    pub front_hold: Option<crate::presence::HoldFact>,
    /// The View ▸ Presence Band checkmark: the band is shown (`[presence] band`,
    /// as the App currently applies it).
    pub presence_band: bool,
    /// The View ▸ Presence Rim checkmark.
    pub presence_rim: bool,
    /// A strictly newer `(build, version)` is staged (`App.relaunch`): the Version section
    /// shows the one-click "↑ Install aterm v<staged> now" row.
    pub staged: Option<(u64, String)>,
    /// The apply lane's standing failure for that staged build (`App::apply_trouble_for`):
    /// it replaces the row's "now" with the attempt count, the cause, and whether the
    /// automatic lane will retry, through the same [`crate::menu::staged_apply_label`]
    /// as the Version menu.
    pub staged_trouble: Option<crate::update_apply_trouble::ApplyTrouble>,
    /// That staged build is on hold (`App::latched_activation_trial_spent`): the row reads
    /// "is on hold" and opens the details, as the Version menu does.
    pub staged_on_hold: bool,
    /// The post-update realized arrow is live, as `(new version, spawn instant)`: the
    /// Version section shows the "↑ Updated to aterm v<new>" row, fading over
    /// [`crate::relaunch_notice::REALIZED_ARROW_TTL`]. Ignored while `staged` is `Some`.
    pub realized: Option<(String, Instant)>,
    /// `MotionPolicy::Reduced` is in force: the realized row's fade freezes at full alpha.
    pub reduced_motion: bool,
    /// Accelerator hints for menu rows: the effective `[keybindings]` chord per menu
    /// action (`Keybindings::display_chord_for`). Filled off macOS only, where they
    /// replace the "" that [`accel`] blanks ⌘ chords to; macOS rows keep their native ⌘
    /// key-equivalents. Empty in pure tests.
    pub menu_accels: Vec<(MenuAction, String)>,
}

/// The Version section's dynamic row label, or `None` when the row is removed: the
/// staged install offer, else the post-update row. [`PaletteState::resolve`] shows it and
/// the off-macOS `chrome` menu lines print it (`menu::menu_chrome_lines`).
///
/// Same label law as the macOS Version menu (`menu::staged_apply_label`). Plain `↑`, not
/// the colour emoji: the palette is own-rendered text with no emoji face.
pub(crate) fn apply_row_label(live: &PaletteLive) -> Option<String> {
    if let Some((build, v)) = &live.staged {
        return Some(crate::menu::staged_apply_label(
            "\u{2191}",
            *build,
            v,
            live.staged_trouble.as_ref(),
            live.staged_on_hold,
        ));
    }
    live.realized
        .as_ref()
        .map(|(v, _)| crate::menu::realized_row_label("\u{2191}", v))
}

impl PaletteState {
    /// Build the palette from [`MENU_MODEL`], every command enabled and unchecked (the
    /// `App`-free default the unit tests use). It mirrors the bar and nothing else: every
    /// row is a model item, in model order, its section chip the menu's title or, for a
    /// submenu's rows (File ▸ Driving), the submenu's label. `App::palette_enter` calls
    /// [`Self::resolve`] right after to fold in live state.
    pub(crate) fn new() -> Self {
        let mut rows = Vec::new();
        for section in MENU_MODEL {
            for entry in section.entries {
                let chip = match entry {
                    MenuEntry::Submenu { label, .. } => *label,
                    MenuEntry::Item { .. } | MenuEntry::Separator => section.title,
                };
                for (label, action, key, mods) in entry.items() {
                    rows.push(PaletteRow {
                        section: Cow::Borrowed(chip),
                        label: Cow::Borrowed(label),
                        action: PaletteTarget::Menu(action),
                        key,
                        mods,
                        shortcut: Cow::Borrowed(""),
                        enabled: true,
                        checked: None,
                    });
                }
            }
        }
        Self {
            rows,
            query: String::new(),
            selected: 0,
            scroll: 0,
            pointer_over: None,
            pointer_armed: None,
            realized_since: None,
            realized_frozen: false,
            themes_only: false,
            theme_previewed: None,
        }
    }

    /// Append the Theme section, replacing any earlier one: one row per theme in the order
    /// given, then the Browse row. The row the cursor starts on counts as already
    /// previewed, so opening the palette changes nothing on screen.
    pub(crate) fn with_themes(mut self, themes: Vec<PaletteTheme>) -> Self {
        self.rows.retain(|row| row.section != THEME_SECTION);
        self.rows.extend(themes.into_iter().map(PaletteTheme::row));
        self.rows.push(PaletteRow {
            section: Cow::Borrowed(THEME_SECTION),
            label: Cow::Borrowed(BROWSE_THEMES_LABEL),
            action: PaletteTarget::Menu(MenuAction::BrowseThemes),
            key: "",
            mods: MenuMods::None,
            shortcut: Cow::Borrowed(""),
            enabled: true,
            checked: None,
        });
        self.clamp_scroll();
        self.theme_previewed = self.highlighted_theme().map(str::to_string);
        self
    }

    /// Scope the palette to its Theme section (View ▸ Choose Theme…): only that section
    /// passes the filter, and the cursor starts on the checked row (the theme the config
    /// draws), which counts as already previewed.
    pub(crate) fn themes_only(mut self) -> Self {
        self.themes_only = true;
        let current = self
            .filtered_indices()
            .position(|i| self.rows[i].checked == Some(true));
        self.selected = current.unwrap_or(0);
        self.scroll = 0;
        self.clamp_scroll();
        self.theme_previewed = self.highlighted_theme().map(str::to_string);
        self
    }

    /// Whether the palette is scoped to its Theme section.
    pub(crate) fn is_themes_only(&self) -> bool {
        self.themes_only
    }

    /// The theme id under the cursor, when the cursor is on a Theme row.
    pub(crate) fn highlighted_theme(&self) -> Option<&str> {
        let idx = self.filtered_indices().nth(self.selected)?;
        self.rows[idx].action.theme()
    }

    /// The highlight's theme when it differs from what the palette last asked the App
    /// to preview, recording it as asked: `Some(Some(id))` to preview `id`,
    /// `Some(None)` when the highlight left the Theme section, `None` when nothing
    /// moved. Edge-triggered, so a run of moves inside one event-loop turn costs one
    /// install, and a preview something else replaced is not fought over.
    pub(crate) fn take_theme_highlight_change(&mut self) -> Option<Option<String>> {
        let now = self.highlighted_theme().map(str::to_string);
        if now == self.theme_previewed {
            return None;
        }
        self.theme_previewed.clone_from(&now);
        Some(now)
    }

    /// Prepend the active native app's command metadata, above the menu rows so the scoped
    /// commands are visible without scrolling.
    pub(crate) fn with_native_commands(mut self, scope: NativeCommandScope) -> Self {
        self.replace_native_commands(Some(scope));
        self
    }

    /// Build the focused recovery surface returned by a native app's blocked
    /// close transaction. Unlike the ordinary palette, this contains exactly
    /// the recovery capabilities supplied by `CloseReadiness::Blocked`; global
    /// menu rows cannot push the explanation/actions out of the visible band.
    pub(crate) fn native_close_recovery(scope: NativeCommandScope) -> Self {
        let mut state = Self::new().with_native_commands(scope);
        state
            .rows
            .retain(|row| matches!(&row.action, PaletteTarget::Native(_)));
        state.selected = 0;
        state.scroll = 0;
        state.clamp_scroll();
        state
    }

    /// Whether this is the recovery-only surface for one exact live native
    /// target. Ordinary palettes always retain menu rows, so they cannot be
    /// mistaken for a close refusal when deferred teardown replay decides
    /// whether the blocker must keep focus.
    pub(crate) fn is_native_close_recovery_for(
        &self,
        window: crate::WindowId,
        view: crate::tab_model::ViewId,
    ) -> bool {
        !self.rows.is_empty()
            && self.rows.iter().all(|row| {
                matches!(
                    &row.action,
                    PaletteTarget::Native(target)
                        if target.window == window && target.view == view
                )
            })
    }

    /// Replace only app-scoped rows, keeping the menu and Theme rows, the live query, and
    /// menu resolution. Active-tab changes use this to remove native commands on terminal
    /// tabs or retarget a different native app without rebuilding menu state.
    pub(crate) fn replace_native_commands(&mut self, scope: Option<NativeCommandScope>) {
        self.pointer_over = None;
        self.pointer_armed = None;
        self.rows
            .retain(|row| !matches!(&row.action, PaletteTarget::Native(_)));
        let Some(scope) = scope else {
            self.selected = 0;
            self.scroll = 0;
            self.clamp_scroll();
            return;
        };
        let mut native = Vec::with_capacity(scope.commands.len());
        for command in scope.commands {
            native.push(PaletteRow {
                section: Cow::Owned(scope.section.clone()),
                label: Cow::Owned(command.title),
                action: PaletteTarget::Native(NativeCommandTarget {
                    window: scope.window,
                    instance: scope.instance,
                    view: scope.view,
                    generation: scope.generation,
                    action: command.id,
                }),
                key: "",
                mods: MenuMods::None,
                shortcut: Cow::Owned(command.shortcut.unwrap_or_default()),
                enabled: command.enabled,
                checked: None,
            });
        }
        native.append(&mut self.rows);
        self.rows = native;
        self.selected = 0;
        self.scroll = 0;
        self.clamp_scroll();
    }

    /// Fold live `App` state into per-row `enabled`/`checked`, mirroring `validateMenuItem:`:
    /// toggles get a checkmark; gated commands disable when their precondition is absent;
    /// everything else stays enabled.
    ///
    /// The Version section's ApplyUpdate row is dynamic, like the macOS Version menu
    /// (`menu::update_version_menu`):
    /// * staged: "↑ Install aterm v<staged> now", one Enter installs in place;
    /// * realized (fresh post-update): the time-faded "↑ Updated to aterm v<new>" row,
    ///   which takes ApplyUpdate's nothing-staged fallback, the Software Update route;
    /// * neither: the row is removed.
    ///
    /// Idempotent: called at every open and on live transitions (`palette_refresh_live`),
    /// so it inserts, rewrites or removes from any prior state.
    pub(crate) fn resolve(&mut self, live: &PaletteLive) {
        // Metadata changed under the gesture: a later release must not inherit authority
        // from the row set painted before this resolve.
        self.pointer_over = None;
        self.pointer_armed = None;
        self.realized_since = None;
        self.realized_frozen = live.reduced_motion;
        if live.staged.is_none()
            && let Some((_, since)) = &live.realized
        {
            self.realized_since = Some(*since);
        }
        let dynamic_label: Option<Cow<'static, str>> = apply_row_label(live).map(Cow::Owned);
        let pos = self
            .rows
            .iter()
            .position(|r| r.action == MenuAction::ApplyUpdate);
        match (dynamic_label, pos) {
            (Some(label), Some(i)) => self.rows[i].label = label,
            (Some(label), None) => {
                // Re-insert at the head of the Version section (before its About row),
                // the slot `PaletteState::new` gave it.
                let at = self
                    .rows
                    .iter()
                    .position(|r| r.action == MenuAction::Version)
                    .unwrap_or(self.rows.len());
                self.rows.insert(
                    at,
                    PaletteRow {
                        section: Cow::Borrowed("Version"),
                        label,
                        action: PaletteTarget::Menu(MenuAction::ApplyUpdate),
                        key: "",
                        mods: MenuMods::None,
                        shortcut: Cow::Borrowed(""),
                        enabled: true,
                        checked: None,
                    },
                );
            }
            (None, Some(i)) => {
                self.rows.remove(i);
            }
            (None, None) => {}
        }
        for row in &mut self.rows {
            let Some(action) = row.action.menu() else {
                // Native enabled state is reducer-owned command metadata. It is revalidated
                // immediately before dispatch as well as represented faithfully here.
                continue;
            };
            row.checked = None;
            row.enabled = true;
            // Off macOS the effective chord fills `shortcut`, so `row_accel` and the accessibility
            // description show the chord that works here, not the "" [`accel`] blanks ⌘ to.
            row.shortcut = match live.menu_accels.iter().find(|(a, _)| *a == action) {
                Some((_, chord)) => Cow::Owned(chord.clone()),
                None => Cow::Borrowed(""),
            };
            match action {
                // The verb carries the state, as the native View item does.
                MenuAction::ToggleFullScreen => {
                    row.label = Cow::Borrowed(crate::menu::full_screen_title(live.fullscreen));
                }
                // ⌘W closes the focused pane of a split tab; the row says which.
                MenuAction::CloseTab => {
                    row.label = Cow::Borrowed(crate::menu::close_row_title(live.front_tab_split));
                }
                MenuAction::ToggleSeriousMode => row.checked = Some(live.serious_mode),
                // Per-session: the checkmark mirrors the front session's effective
                // rain; with no front session the row disables (and the invoke fence
                // refuses) instead of silently no-op'ing.
                MenuAction::ToggleMatrixRain => {
                    row.checked = Some(live.rain_on);
                    row.enabled = live.terminal_front;
                }
                // Process-wide: the checkmark answers "is the promotable kitty
                // (program cat on glass, else the launch kitty) the pin?", and
                // the launch kitty exists whether or not a terminal is
                // frontmost, so the row stays enabled everywhere.
                MenuAction::FavouriteKitty => {
                    row.checked = Some(live.kitty_favourited);
                }
                // The name is session metadata: with no front session the row
                // disables (and the invoke fence refuses).
                MenuAction::RenameSession => {
                    row.enabled = live.terminal_front && live.can_rename;
                }
                // The in-app updater exists only where the menu model's
                // [`MenuAction::offered_on_this_platform`] says (the predicate the
                // `chrome` verb's menu lines share); elsewhere the row greys and the
                // socket `invoke` refuses. Not gated on updater state: with an updater
                // the action always at least opens the Software Update route.
                MenuAction::SoftwareUpdate => {
                    row.enabled = MenuAction::SoftwareUpdate.offered_on_this_platform();
                }
                MenuAction::Copy => row.enabled = live.has_selection,
                MenuAction::NextTab | MenuAction::PrevTab => row.enabled = live.multi_tab,
                MenuAction::ReopenClosedTab => row.enabled = live.can_reopen_closed_tab,
                MenuAction::ReopenClosedView => row.enabled = live.can_reopen_closed_view,
                MenuAction::OpenMarkdown | MenuAction::OpenEditor => {
                    row.enabled = live.local_file_picker_available;
                }
                MenuAction::SplitVertical
                | MenuAction::SplitHorizontal
                | MenuAction::ViewSessionInNewWindow => {
                    row.enabled = !live.native_tab_active;
                }
                // These act from the focused session (the connected-spawn presets take
                // it as their origin), so with no front terminal they disable and the
                // invoke path refuses. The connection map is instance-wide and stays
                // enabled.
                MenuAction::NewControlledWindow
                | MenuAction::NewControlledTab
                | MenuAction::NewControllerWindow
                | MenuAction::NewControllerTab
                | MenuAction::ConnectToSession
                | MenuAction::ConfigureConnection
                | MenuAction::DisconnectSession
                // The focused session's own rows: its inbox, ledger, identity
                // and reset.
                | MenuAction::Inbox
                | MenuAction::LedgerForSession
                | MenuAction::ShowIdentity
                | MenuAction::ResetTerminal
                | MenuAction::ChooseTabTheme => {
                    row.enabled = live.terminal_front;
                }
                // Only a tab with a theme of its own has one to clear.
                MenuAction::ClearTabTheme => {
                    row.enabled = live.terminal_front && live.tab_theme;
                }
                // Same editor as the pin, same surface rule.
                MenuAction::SetRole => {
                    row.enabled = live.terminal_front && live.can_rename;
                }
                // The halt pair reads the live hold: Hold only with nothing standing,
                // Lift only against a local hold. Under a fleet hold both grey and both
                // say why.
                MenuAction::HoldSession | MenuAction::LiftHold => {
                    let base = if action == MenuAction::HoldSession {
                        "Hold This Session"
                    } else {
                        "Lift Hold (This Session)"
                    };
                    row.label = Cow::Borrowed(base);
                    match &live.front_hold {
                        None => row.enabled = live.terminal_front && action == MenuAction::HoldSession,
                        Some(h) if h.fleet => {
                            row.enabled = false;
                            row.label = Cow::Owned(format!(
                                "{base} \u{2014} {}",
                                crate::menu::hold_row_reason(&h.reason)
                            ));
                        }
                        Some(_) => row.enabled = live.terminal_front && action == MenuAction::LiftHold,
                    }
                }
                MenuAction::TogglePresenceBand => row.checked = Some(live.presence_band),
                MenuAction::TogglePresenceRim => row.checked = Some(live.presence_rim),
                _ => {}
            }
        }
        // A live re-resolve can shrink the row set (the update row was removed) while a
        // filter/cursor is active — re-clamp so painter and cursor never diverge.
        self.clamp_scroll();
    }

    /// The paint alpha for `row` at `now`: `1.0` except the realized "↑ Updated to aterm
    /// v<new>" row, which fades per `relaunch_notice::realized_alpha`. Computed at paint
    /// time so an open palette steps down as the fingerprint's elapsed bucket advances;
    /// frozen at full under `MotionPolicy::Reduced`.
    pub(crate) fn row_alpha(&self, row: &PaletteRow, now: Instant) -> f32 {
        if row.action != MenuAction::ApplyUpdate {
            return 1.0;
        }
        let Some(since) = self.realized_since else {
            return 1.0; // the staged "install now" row never fades
        };
        if self.realized_frozen {
            return 1.0;
        }
        crate::relaunch_notice::realized_alpha(now.duration_since(since))
    }

    /// Append a character to the filter and reset the cursor to the top of the (renarrowed)
    /// list: the tests' typing seam ([`Self::edit`] is the keyboard's and the control
    /// seam's). Non-printing controls are ignored by the caller before this point.
    #[cfg(test)]
    pub(crate) fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.renarrowed();
    }

    /// Apply one keyboard edit to the filter through the editing model every in-grid
    /// one-line field shares ([`crate::app_search::apply_field_edit`]: the find bar and
    /// the inline rename), so the palette answers the same readline chords they do.
    ///
    /// The filter has no caret: it paints none and typing always appends, so the edit
    /// runs with the caret at the END and the caret it leaves is discarded. A motion
    /// (`^A`/`^E`/`^B`/`^F`, `←`/`→`, Home/End, word motion) therefore changes nothing,
    /// and a forward kill (`^K`, `^D`, `⌦`) has nothing after the caret to remove. What
    /// edits the filter is typing and the backward kills: `⌫`/`^H` a character,
    /// `^W`/`⌥⌫` (and Ctrl+`⌫` off macOS) a word, `^U`/`⌘⌫` the whole query. A changed
    /// query resets the cursor to the top of the renarrowed list, as typing does.
    /// Returns whether the query changed.
    pub(crate) fn edit(&mut self, edit: crate::app_search::SearchEdit) -> bool {
        let mut caret = self.query.len();
        let changed = crate::app_search::apply_field_edit(&mut self.query, &mut caret, edit);
        if changed {
            self.renarrowed();
        }
        changed
    }

    /// The query changed: the list re-narrows from its top, and the pointer's row
    /// identity no longer names what is under it.
    fn renarrowed(&mut self) {
        self.selected = 0;
        self.scroll = 0;
        self.pointer_over = None;
        self.pointer_armed = None;
    }

    /// The indices into `rows` in scope that pass the fuzzy filter, in row order: every
    /// query char appears, in order, in the lowercased `"label  section"`, plus a theme
    /// row's appearance (an empty query matches all). The haystack streams lazily, and
    /// count-only callers skip the list.
    pub(crate) fn filtered(&self) -> Vec<usize> {
        self.filtered_indices().collect()
    }

    fn filtered_indices(&self) -> impl Iterator<Item = usize> + '_ {
        let q = self.query.to_ascii_lowercase();
        (0..self.rows.len()).filter(move |&i| {
            let r = &self.rows[i];
            if !self.in_scope(r) {
                return false;
            }
            // A theme row's hint is its appearance, so "light" finds the light themes.
            let hint = if r.action.theme().is_some() {
                &*r.shortcut
            } else {
                ""
            };
            let hay = r
                .label
                .chars()
                .chain("  ".chars())
                .chain(r.section.chars())
                .chain(" ".chars())
                .chain(hint.chars())
                .map(|c| c.to_ascii_lowercase());
            fuzzy_subsequence(&q, hay)
        })
    }

    /// Whether `row` belongs to the palette's scope: every row, or only the Theme
    /// section under View ▸ Choose Theme….
    fn in_scope(&self, row: &PaletteRow) -> bool {
        !self.themes_only || row.section == THEME_SECTION
    }

    /// The rows the scope offers before any filter: the count's denominator.
    fn scope_len(&self) -> usize {
        if self.themes_only {
            self.rows.iter().filter(|r| self.in_scope(r)).count()
        } else {
            self.rows.len()
        }
    }

    fn filtered_count(&self) -> usize {
        if self.query.is_empty() && !self.themes_only {
            self.rows.len()
        } else {
            self.filtered_indices().count()
        }
    }

    /// The visible command band height: the filtered count, capped at [`MAX_CMD_ROWS`]. The
    /// single value the painter, the scroll clamp, and [`Self::move_selection`] agree on.
    fn body(&self) -> usize {
        self.filtered_count().min(MAX_CMD_ROWS)
    }

    /// Move the cursor by `delta` over the filtered set (wrapping), keeping it on-screen.
    pub(crate) fn move_selection(&mut self, delta: isize) {
        self.pointer_over = None;
        self.pointer_armed = None;
        let n = self.filtered_count();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        self.clamp_scroll_for_count(n);
    }

    /// Scroll the visible filtered band without wrapping. The selection keeps its slot in
    /// the viewport, so `scroll <= selected < scroll + body` holds at both clamps. A wheel
    /// gesture cancels any armed click.
    pub(crate) fn scroll_by(&mut self, delta: isize) -> bool {
        let n = self.filtered_count();
        let body = n.min(MAX_CMD_ROWS);
        let before = (
            self.selected,
            self.scroll,
            self.pointer_over.clone(),
            self.pointer_armed.clone(),
        );
        self.pointer_over = None;
        self.pointer_armed = None;
        if n == 0 || body == 0 {
            self.selected = 0;
            self.scroll = 0;
        } else {
            let max_scroll = n.saturating_sub(body);
            let relative = self.selected.saturating_sub(self.scroll).min(body - 1);
            self.scroll = self.scroll.saturating_add_signed(delta).min(max_scroll);
            self.selected = (self.scroll + relative).min(n - 1);
            self.clamp_scroll_for_count(n);
        }
        before
            != (
                self.selected,
                self.scroll,
                self.pointer_over.clone(),
                self.pointer_armed.clone(),
            )
    }

    /// Move the cursor to a filtered-set index (an OS accessibility Focus/Click on a row
    /// lands here), clamped into range and kept on-screen. A no-op when nothing matches.
    #[cfg(a11y_tree)]
    pub(crate) fn select(&mut self, idx: usize) {
        self.pointer_over = None;
        self.pointer_armed = None;
        self.select_filtered_index(idx);
    }

    fn select_filtered_index(&mut self, idx: usize) {
        let n = self.filtered_count();
        if n == 0 {
            return;
        }
        self.selected = idx.min(n - 1);
        self.clamp_scroll_for_count(n);
    }

    /// Hover a filtered row, moving the visible selection wash with the pointer. `None`
    /// means the modal still owns the pointer but it is outside a painted command row.
    pub(crate) fn pointer_hover(&mut self, idx: Option<usize>) -> bool {
        let target = idx.and_then(|idx| {
            self.filtered_indices()
                .nth(idx)
                .map(|row| self.rows[row].action.clone())
        });
        let before = (self.selected, self.scroll, self.pointer_over.clone());
        if let Some(idx) = idx {
            self.select_filtered_index(idx);
        }
        self.pointer_over = target;
        before != (self.selected, self.scroll, self.pointer_over.clone())
    }

    /// Arm the exact hovered target on left press. Disabled rows still get selection
    /// feedback; [`Self::pointer_release`] keeps them inert.
    pub(crate) fn pointer_press(&mut self, idx: Option<usize>) -> bool {
        let mut changed = self.pointer_hover(idx);
        let armed = self.pointer_over.clone();
        changed |= self.pointer_armed != armed;
        self.pointer_armed = armed;
        changed
    }

    /// Settle a left release. Activation requires the exact same typed target at press and
    /// release, and that target must still be enabled in the current row snapshot.
    pub(crate) fn pointer_release(&mut self, idx: Option<usize>) -> (bool, bool) {
        let mut changed = self.pointer_hover(idx);
        let armed = self.pointer_armed.take();
        changed |= armed.is_some();
        let enabled = idx.is_some_and(|idx| {
            self.filtered_indices()
                .nth(idx)
                .is_some_and(|row| self.rows[row].enabled)
        });
        let activate = enabled && armed.is_some() && armed == self.pointer_over;
        (changed, activate)
    }

    pub(crate) fn pointer_over_row(&self) -> bool {
        self.pointer_over.is_some()
    }

    /// Keep `selected` within `[scroll, scroll + body)` and `scroll` within bounds, so
    /// painter and cursor never diverge.
    fn clamp_scroll(&mut self) {
        self.clamp_scroll_for_count(self.filtered_count());
    }

    fn clamp_scroll_for_count(&mut self, n: usize) {
        let body = n.min(MAX_CMD_ROWS);
        if self.selected >= n {
            self.selected = n.saturating_sub(1);
        }
        if body == 0 {
            self.scroll = 0;
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + body {
            self.scroll = self.selected + 1 - body;
        }
        let max_scroll = n.saturating_sub(body);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    /// The typed target the cursor's row dispatches, or `None` when the filtered list is
    /// empty or the selected row is disabled.
    pub(crate) fn selected_target(&self) -> Option<PaletteTarget> {
        let idx = self.filtered_indices().nth(self.selected)?;
        let row = &self.rows[idx];
        row.enabled.then(|| row.action.clone())
    }

    /// [`Self::selected_target`] narrowed to a menu action, for the menu-model tests.
    #[cfg(test)]
    pub(crate) fn selected_action(&self) -> Option<MenuAction> {
        self.selected_target()?.menu()
    }

    /// The overlay height (rows) the card wants: chrome plus the capped command band, never
    /// fewer than one band row.
    #[cfg(test)]
    pub(crate) fn wanted_rows(&self) -> usize {
        CHROME_ROWS + self.filtered_count().clamp(1, MAX_CMD_ROWS)
    }

    /// `(scroll, total, visible)` for `controls front`: rows scrolled past, the scope's row
    /// count, and the command rows shown (capped at `MAX_CMD_ROWS`).
    pub(crate) fn scroll_extent(&self) -> (usize, usize, usize) {
        (self.scroll, self.scope_len(), self.body())
    }

    /// Machine-readable lines for the `controls menu` verb: the rows the card paints.
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        let vis = self.filtered();
        let mut out = Vec::with_capacity(vis.len() + 2);
        out.push(format!(
            "menu rows={} shown={} selected={} query={:?} scope={}",
            self.scope_len(),
            vis.len(),
            self.selected,
            self.query,
            if self.themes_only { "themes" } else { "all" },
        ));
        for &i in &vis {
            let r = &self.rows[i];
            out.push(format!(
                "menu row section={:?} label={:?} target={} action={} accel={:?} enabled={} checked={}",
                r.section,
                r.label,
                match &r.action {
                    PaletteTarget::Menu(_) => "menu".to_string(),
                    PaletteTarget::Native(target) => format!(
                        "native window={} instance={} view={} generation={}",
                        target.window.0,
                        target.instance.get(),
                        target.view.get(),
                        target.generation,
                    ),
                    PaletteTarget::Theme(id) => format!("theme id={id:?}"),
                },
                r.action.control_name(),
                row_accel(r),
                r.enabled,
                r.checked
                    .map_or_else(|| "none".to_string(), |b| b.to_string()),
            ));
        }
        out.push("menu action=filter|move|activate".to_string());
        out
    }

    /// A fingerprint of everything the card paints (query, cursor, scroll, scope, every row),
    /// folded into the frame's `RepaintKey`; never `0` while open (`0` is the closed
    /// sentinel). While the realized row is live and unfrozen the elapsed fade bucket is
    /// folded too, so an open palette re-presents once per fade step (the `about_to_wait`
    /// sweep arms the matching wake).
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.query.hash(&mut h);
        self.selected.hash(&mut h);
        self.scroll.hash(&mut h);
        self.themes_only.hash(&mut h);
        self.rows.len().hash(&mut h);
        for r in &self.rows {
            match &r.action {
                PaletteTarget::Menu(action) => {
                    0_u8.hash(&mut h);
                    action.tag().hash(&mut h);
                }
                PaletteTarget::Native(target) => {
                    1_u8.hash(&mut h);
                    target.hash(&mut h);
                }
                PaletteTarget::Theme(id) => {
                    2_u8.hash(&mut h);
                    id.hash(&mut h);
                }
            }
            r.section.hash(&mut h);
            r.label.hash(&mut h);
            r.shortcut.hash(&mut h);
            r.enabled.hash(&mut h);
            r.checked.hash(&mut h);
        }
        if let Some(since) = self.realized_since
            && !self.realized_frozen
        {
            let elapsed = since.elapsed();
            if elapsed < crate::relaunch_notice::REALIZED_ARROW_TTL {
                crate::relaunch_notice::realized_bucket(elapsed).hash(&mut h);
            }
        }
        h.finish() | 1
    }
}

/// The command palette's accessibility tree, read from the same [`PaletteState`] as the
/// pixels ([`palette_tray`]) and `controls menu` ([`PaletteState::controls_lines`]). A
/// window root parents a [`aterm_a11y::model::Role::ListBox`] of the filtered rows, one
/// [`aterm_a11y::model::Role::MenuItem`] each (label = command label, description = section,
/// `toggled` for checkable rows). An enabled row carries [`aterm_a11y::model::Action::Click`] and
/// [`aterm_a11y::model::Action::Focus`]; a disabled row omits `Click`, as Enter on it is inert.
/// Focus follows `selected`.
///
/// Id contract (the Palette branch of `App::on_accessibility_action`): the window root is
/// `NodeId(0)`; each row id combines a hash of the filtered target set with its slot, so a
/// request queued against an old native view/generation cannot address a new row after a
/// tab switch. The ListBox lives at a disjoint sentinel id.
#[cfg(a11y_tree)]
pub(crate) fn palette_a11y(state: &PaletteState) -> aterm_a11y::model::TreeUpdate {
    use aterm_a11y::model::{Action, Node, Role, Toggled};

    let vis = state.filtered();
    // Derived once: the epoch is slot-independent, and each derivation walks the whole
    // filtered set.
    let epoch = state.a11y_epoch();

    let rows = vis.iter().map(|&i| {
        let row = &state.rows[i];
        let mut node = Node::new(Role::MenuItem);
        node.set_label(row.label.as_ref());
        // A screen reader speaks this, so it goes through `platform_accel` (no
        // macOS-only "Cmd-F" read aloud elsewhere). Not the full `row_accel`, which
        // would also add a menu row's ⌘ key-equivalent to what VoiceOver says.
        let accel = platform_accel(&row.shortcut);
        let mut description = if accel.is_empty() {
            row.section.to_string()
        } else {
            format!("{} \u{b7} {accel}", row.section)
        };
        // The row's help sentence: the words the native item carries as its tool tip,
        // so a screen reader hears what a hover shows.
        if let Some(action) = row.action.menu() {
            description.push_str(": ");
            description.push_str(action.help());
        }
        node.set_description(description);
        if let Some(on) = row.checked {
            node.set_toggled(Toggled::from(on));
        }
        node.add_action(Action::Focus);
        if row.enabled {
            node.add_action(Action::Click);
        }
        node
    });
    let root = if state.themes_only {
        "Themes"
    } else {
        "Commands"
    };
    list_overlay_a11y(root, epoch, state.selected, rows)
}

/// The accessibility tree of a list overlay (the palette, the session picker, the
/// connection map): a Window root (`NodeId(0)`) labelled `root_label` parents a ListBox
/// at a disjoint sentinel id whose children are `rows`, one node per slot with its id
/// minted from `epoch` ([`a11y_node_id_for`]). Focus is the `selected` row's node
/// (clamped); the root when the list is empty.
#[cfg(a11y_tree)]
pub(crate) fn list_overlay_a11y(
    root_label: &str,
    epoch: u32,
    selected: usize,
    rows: impl ExactSizeIterator<Item = aterm_a11y::model::Node>,
) -> aterm_a11y::model::TreeUpdate {
    use aterm_a11y::model::{Node, NodeId, Role, Tree, TreeId, TreeUpdate};

    // High + disjoint from the `slot + 1` row ids.
    const LIST: NodeId = NodeId(u64::MAX);
    let root_id = NodeId(0);
    let mut nodes: Vec<(NodeId, Node)> = Vec::with_capacity(rows.len() + 2);
    let mut items: Vec<NodeId> = Vec::with_capacity(rows.len());
    for (slot, node) in rows.enumerate() {
        let id = a11y_node_id_for(epoch, slot);
        nodes.push((id, node));
        items.push(id);
    }
    let focus = if items.is_empty() {
        root_id
    } else {
        a11y_node_id_for(epoch, selected.min(items.len() - 1))
    };

    let mut list = Node::new(Role::ListBox);
    list.set_children(items);
    nodes.push((LIST, list));

    let mut root = Node::new(Role::Window);
    root.set_label(root_label);
    root.set_children(vec![LIST]);
    nodes.push((root_id, root));

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(root_id)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

/// Mint a list-overlay row node id from an already-derived epoch: the epoch in the high
/// half, `slot + 1` in the low.
#[cfg(a11y_tree)]
pub(crate) fn a11y_node_id_for(epoch: u32, slot: usize) -> aterm_a11y::model::NodeId {
    aterm_a11y::model::NodeId((u64::from(epoch) << 32) | (slot as u64 + 1))
}

/// Decode a row node back to its slot: `None` unless `node` was minted by `epoch` for
/// one of the `count` live rows, so a stale platform request fails closed.
#[cfg(a11y_tree)]
pub(crate) fn a11y_slot_for(
    epoch: u32,
    node: aterm_a11y::model::NodeId,
    count: usize,
) -> Option<usize> {
    let slot = usize::try_from(node.0 & u64::from(u32::MAX))
        .ok()?
        .checked_sub(1)?;
    (a11y_node_id_for(epoch, slot) == node && slot < count).then_some(slot)
}

/// Fold a row-set hash to a row-id epoch, never `u32::MAX` (the high half of the ListBox
/// sentinel id).
#[cfg(a11y_tree)]
pub(crate) fn a11y_epoch_from(hash: u64) -> u32 {
    let epoch = hash as u32;
    if epoch == u32::MAX {
        u32::MAX - 1
    } else {
        epoch
    }
}

/// Transient per-window command palette state. While a window holds `Some(PaletteState)`,
/// printable chars filter, Up/Down move the cursor, Enter dispatches, Esc closes.
pub(crate) struct PaletteState {
    /// Native rows, then every flattened command from [`MENU_MODEL`] in menu order, then
    /// the Theme section. [`PaletteState::resolve`] rewrites or removes the Version
    /// section's ApplyUpdate row.
    rows: Vec<PaletteRow>,
    /// The type-to-filter query ([`Self::filtered`]).
    query: String,
    /// The cursor position within the filtered set (not `rows`).
    selected: usize,
    /// First filtered row shown in the command band (scroll offset).
    scroll: usize,
    /// Exact row currently under the pointer. Keyboard/accessibility selection does not
    /// imply pointer ownership, so this remains distinct from `selected`.
    pointer_over: Option<PaletteTarget>,
    /// Exact target armed by a left press. It prevents release after a filter/tab change
    /// from activating a different command that happens to occupy the same visual slot.
    pointer_armed: Option<PaletteTarget>,
    /// When the realized row spawned (`Some` only while `resolve` saw a live realized
    /// state and no staged build): drives [`Self::row_alpha`] and the fingerprint's bucket.
    realized_since: Option<Instant>,
    /// `MotionPolicy::Reduced` snapshot: freeze the realized fade at full alpha.
    realized_frozen: bool,
    /// Scoped to the Theme section (View ▸ Choose Theme…).
    themes_only: bool,
    /// The theme the palette last asked the App to preview (`None` off the Theme section):
    /// the edge [`Self::take_theme_highlight_change`] detects.
    theme_previewed: Option<String>,
}

impl PaletteState {
    /// The resolved rows, in order: what the painter, `controls menu` and the a11y tree
    /// read.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[PaletteRow] {
        &self.rows
    }

    /// The filter as typed, for the key-routing tests of other modules.
    #[cfg(test)]
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// Resolve an action by name (the `action=` token [`Self::controls_lines`] prints,
    /// e.g. `NewTab`) for the socket `invoke` verb. A row [`Self::resolve`] disabled
    /// errs as disabled, an unmatched name as unknown; an enabled row wins over a
    /// disabled duplicate.
    pub(crate) fn action_by_name(&self, name: &str) -> Result<MenuAction, String> {
        // Accepted aliases (`OpenLedger`, `Fleet`/`ShowFleet`, `ShowInbox`) reach their
        // current action ([`crate::menu::canonical_invoke_name`]).
        let name = crate::menu::canonical_invoke_name(name);
        let mut seen_disabled = false;
        for r in &self.rows {
            let Some(action) = r.action.menu() else {
                continue;
            };
            if format!("{action:?}") == name {
                if r.enabled {
                    return Ok(action);
                }
                seen_disabled = true;
            }
        }
        if seen_disabled {
            Err(format!("action {name} is disabled right now"))
        } else {
            Err(format!(
                "unknown action {name:?} (list actions with `controls menu`)"
            ))
        }
    }

    /// Accessibility row identity epoch. Cursor/focus movement deliberately does not enter this
    /// hash (Focus followed by Click remains valid); row targets, lifecycle generations,
    /// enabled state, and filtering do, so stale platform requests fail closed.
    #[cfg(a11y_tree)]
    fn a11y_epoch(&self) -> u32 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.query.hash(&mut hash);
        for index in self.filtered() {
            let row = &self.rows[index];
            row.action.control_name().hash(&mut hash);
            match &row.action {
                PaletteTarget::Menu(action) => action.tag().hash(&mut hash),
                PaletteTarget::Native(target) => target.hash(&mut hash),
                PaletteTarget::Theme(id) => id.hash(&mut hash),
            }
            row.enabled.hash(&mut hash);
        }
        a11y_epoch_from(hash.finish())
    }

    #[cfg(all(a11y_tree, test))]
    fn a11y_node_id(&self, slot: usize) -> aterm_a11y::model::NodeId {
        a11y_node_id_for(self.a11y_epoch(), slot)
    }

    /// Decode only a node minted by the current filtered target epoch. An old native
    /// generation or tab scope cannot redirect a delayed screen-reader Click.
    #[cfg(a11y_tree)]
    pub(crate) fn a11y_filtered_index(&self, node: aterm_a11y::model::NodeId) -> Option<usize> {
        a11y_slot_for(self.a11y_epoch(), node, self.filtered_count())
    }
}

/// Format a visual accelerator, e.g. `⌘K` / `⇧⌘N` / `⌃⌘F`; empty when the command has no
/// key-equivalent. Cosmetic: the keystroke itself is handled by `App::on_key` / the
/// native bar.
fn accel(mods: MenuMods, key: &str) -> String {
    // Off macOS there is no ⌘ key and the real chord differs (see `platform_accel`): blank,
    // and `resolve` writes the true chord from `PaletteLive::menu_accels` into `shortcut`.
    if key.is_empty() || !cfg!(target_os = "macos") {
        return String::new();
    }
    let m = match mods {
        MenuMods::None => "",
        MenuMods::Command => "\u{2318}",
        MenuMods::CommandShift => "\u{21e7}\u{2318}",
        MenuMods::CommandControl => "\u{2303}\u{2318}",
    };
    format!("{m}{}", key.to_uppercase())
}

/// Off macOS, drops the macOS-only key-equivalents from a native app's `shortcut` string,
/// keeping each `·`-separated alternative on its own ("Cmd-S · C-x C-s" becomes "C-x C-s").
/// The string also feeds the accessibility description.
///
/// Never rewrite `Cmd-` to `Ctrl-` mechanically: Settings ▸ Search is `Ctrl+Shift+F` off
/// macOS, Reader ▸ Copy `Ctrl+Shift+C`, and the editor's `Cmd-S` has no Ctrl twin outside
/// Windows. Each `commands()` site emits the platform-true string where one exists; this is
/// the backstop.
fn platform_accel(shortcut: &str) -> String {
    if cfg!(target_os = "macos") {
        return shortcut.to_string();
    }
    shortcut
        .split('\u{b7}')
        .map(str::trim)
        .filter(|part| !part.is_empty() && !accel_is_mac_only(part))
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// Whether one accelerator alternative names a macOS-only chord, in any spelling. Pure
/// and cfg-free so both platforms' answers test in one build.
///
/// - ⌘ / `Cmd` / `Command`: no Command key exists elsewhere.
/// - ⌥ / `Opt` / `Option`: the key exists as Alt, but the spelling is macOS notation,
///   so it is dropped; a part meaning the Windows/Linux key is spelled `Alt-`/`M-`.
fn accel_is_mac_only(part: &str) -> bool {
    const MAC_ONLY_WORDS: &[&str] = &["Cmd", "Command", "Opt", "Option"];
    if part.contains('\u{2318}') || part.contains('\u{2325}') {
        return true;
    }
    // A modifier word in either separator spelling, anywhere in the part, so
    // `Shift-Cmd-S` is caught too.
    part.split(['-', '+'])
        .any(|segment| MAC_ONLY_WORDS.contains(&segment))
}

/// The one accelerator projection for every observer: native rows through
/// [`platform_accel`], menu rows through [`accel`]. The painted card and the accessibility
/// description both read it, so what is shown and what is said agree.
fn row_accel(row: &PaletteRow) -> String {
    if row.shortcut.is_empty() {
        accel(row.mods, row.key)
    } else {
        platform_accel(&row.shortcut)
    }
}

/// The painted selection/hit rectangle for one visible command slot, deriving the layout
/// itself. Test-only: painter and hit-test call `palette_row_rect_in` with the layout they
/// hold, and tests assert both against this one rectangle.
#[cfg(test)]
pub(crate) fn palette_row_rect(
    state: &PaletteState,
    g: &SettingsGeom,
    slot: usize,
) -> Option<(f32, f32, f32, f32)> {
    palette_row_rect_in(&palette_layout(state, g), g, slot)
}

/// `palette_row_rect` against an already-derived layout, shared by every visible slot.
fn palette_row_rect_in(
    layout: &PaletteLayout,
    g: &SettingsGeom,
    slot: usize,
) -> Option<(f32, f32, f32, f32)> {
    if slot >= layout.body_rows {
        return None;
    }
    let (card_x, card_y, card_w, _) = layout.card;
    Some((
        card_x + g.cw,
        card_y + (2 + slot) as f32 * g.ch + 1.0,
        (card_w - g.cw * 2.0).max(0.0),
        (g.ch - 2.0).max(0.0),
    ))
}

/// Filtered-list index under a card-local point. Only rows actually painted after the
/// current scroll offset participate; chrome, row gaps, and outside-card points miss.
pub(crate) fn palette_row_hit(
    state: &PaletteState,
    g: &SettingsGeom,
    x: f32,
    y: f32,
) -> Option<usize> {
    let count = state.filtered_count();
    let layout = palette_layout_for_count(count, g);
    for slot in 0..layout.body_rows {
        let (rx, ry, rw, rh) = palette_row_rect_in(&layout, g, slot)?;
        if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
            let filtered = state.scroll + slot;
            return (filtered < count).then_some(filtered);
        }
    }
    None
}

/// True iff every char of `needle` appears in `haystack` in order (both already
/// lowercased); an empty needle always matches. The haystack is an iterator so
/// [`PaletteState::filtered`] lowercases lazily. The session picker
/// ([`crate::session_picker`]) filters with the same match.
pub(crate) fn fuzzy_subsequence(needle: &str, haystack: impl Iterator<Item = char>) -> bool {
    let mut hay = haystack;
    'outer: for nc in needle.chars() {
        for hc in hay.by_ref() {
            if hc == nc {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

/// Paint the command palette as an opaque floating [`DrawPrim`] card: a
/// centred title, a pinned query row with a shown/total count, then one row per filtered
/// command with the accelerator right-aligned, a checkmark for `checked`, dim text for
/// disabled, and the cursor row washed + ringed. PURE: composes from Panel / Stroke / Text,
/// so it is captured WYSIWYG and renders identically on CPU + GPU.
pub(crate) fn palette_tray(
    state: &PaletteState,
    g: &SettingsGeom,
    theme: impl Into<crate::chrome_band::ChromePalette>,
) -> TrayInput {
    let r = Roles::from_theme(theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let vis = state.filtered();
    let layout = palette_layout_for_count(vis.len(), g);
    let (card_x, card_y, card_w, _) = layout.card;
    let mut prims = overlay_card_frame(layout.card, ch, &r, true);

    // A Mono run at an explicit baseline (mixed sizes on one row share the dominant
    // size's baseline), in full RGBA so the realized row can fade.
    let text_at = |prims: &mut Vec<DrawPrim>,
                   x: f32,
                   baseline: f32,
                   size: StepPx,
                   s: String,
                   color: [u8; 4]| {
        if s.is_empty() {
            return;
        }
        prims.push(text_prim(
            x,
            baseline,
            s,
            size,
            TextWeight::Regular,
            TextFace::Mono,
            color,
        ));
    };
    // A single-size run cap-height-centred in the `ch`-tall row at `y0`.
    let text =
        |prims: &mut Vec<DrawPrim>, x: f32, y0: f32, size: StepPx, s: String, color: [u8; 4]| {
            text_at(prims, x, row_baseline(y0, ch, size.get()), size, s, color);
        };

    {
        let title = if state.themes_only {
            "Themes"
        } else {
            "Commands"
        };
        let tsize = TypeStep::Title.px(px);
        let tw = crate::tray_raster::measure_text(title, tsize.get(), TextWeight::Bold);
        let x = (card_x + (card_w - tw) * 0.5).max(card_x + cw);
        prims.push(text_prim(
            x,
            card_y + row_baseline(0.0, ch, tsize.get()),
            title.to_string(),
            tsize,
            TextWeight::Bold,
            TextFace::Mono,
            rgba(r.text_primary, 0xFF),
        ));
    }

    // Pinned query row: a framed field with the live filter + a "shown of total" count.
    let qy = card_y + ch; // row 1
    prims.push(DrawPrim::Stroke {
        x: card_x + cw * 1.5,
        y: qy + 2.0,
        w: (card_w - cw * 3.0).max(0.0),
        h: ch - 4.0,
        radius: ch * 0.28,
        width: 1.25,
        color: rgba(r.separator, 0xCC),
    });
    let prompt = if state.query.is_empty() {
        "\u{203a} type to filter".to_string()
    } else {
        format!("\u{203a} {}", state.query)
    };
    let q_color = if state.query.is_empty() {
        r.text_tertiary
    } else {
        r.text_primary
    };
    text(
        &mut prims,
        card_x + cw * 2.25,
        qy,
        TypeStep::Body.px(px),
        prompt,
        rgba(q_color, 0xFF),
    );
    let count = format!("{}/{}", vis.len(), state.scope_len());
    let csize = TypeStep::Caption.px(px);
    text(
        &mut prims,
        card_x + card_w - cw * 2.25 - text_w(&count, csize.get()),
        qy,
        csize,
        count,
        rgba(r.text_tertiary, 0xFF),
    );

    // The scrolling command band, from `scroll`. Cap to the rows the card actually has.
    let body = layout.body_rows;
    let label_x = card_x + cw * 2.0;
    let accel_right = card_x + card_w - cw * 2.0;
    // Sampled once per paint for `row_alpha`; the fingerprint's elapsed bucket schedules
    // the re-presents.
    let now = Instant::now();
    for slot in 0..body {
        let Some(&idx) = vis.get(state.scroll + slot) else {
            break;
        };
        let row = &state.rows[idx];
        let ra = state.row_alpha(row, now);
        let fade = |base: u8| -> u8 { (f32::from(base) * ra) as u8 };
        let y0 = card_y + (2 + slot) as f32 * ch;
        let is_sel = state.scroll + slot == state.selected;
        if is_sel {
            // The card's own `layout`: re-deriving it would rerun the filter.
            let (x, y, width, height) = palette_row_rect_in(&layout, g, slot)
                .expect("painted palette slot has a row rectangle");
            prims.push(DrawPrim::Panel {
                x,
                y,
                w: width,
                h: height,
                radius: ch * 0.3,
                fill: rgba(r.accent, 0x22),
            });
            prims.push(DrawPrim::Stroke {
                x,
                y,
                w: width,
                h: height,
                radius: ch * 0.3,
                width: 1.5,
                color: rgba(r.accent, 0xCC),
            });
        }
        // Body label, Caption chip and accelerator share the Body run's baseline.
        let body = TypeStep::Body.px(px);
        let cap = TypeStep::Caption.px(px);
        let row_base = row_baseline(y0, ch, body.get());
        // Checkbox glyph column (toggle commands only).
        if let Some(on) = row.checked {
            let glyph = if on { "\u{2713}" } else { " " };
            text_at(
                &mut prims,
                card_x + cw * 1.5,
                row_base,
                body,
                glyph.to_string(),
                rgba(if on { r.success } else { r.text_tertiary }, fade(0xFF)),
            );
        }
        let lcolor = if row.enabled {
            r.text_primary
        } else {
            r.text_tertiary
        };
        let lx = if row.checked.is_some() {
            label_x + cw * 1.5
        } else {
            label_x
        };
        text_at(
            &mut prims,
            lx,
            row_base,
            body,
            row.label.to_string(),
            rgba(lcolor, fade(0xFF)),
        );
        let lw = text_w(&row.label, body.get());
        text_at(
            &mut prims,
            lx + lw + cw * 1.0,
            row_base,
            cap,
            row.section.to_string(),
            rgba(r.text_tertiary, fade(0xFF)),
        );
        let a = row_accel(row);
        if !a.is_empty() {
            text_at(
                &mut prims,
                accel_right - text_w(&a, cap.get()),
                row_base,
                cap,
                a,
                rgba(r.text_secondary, fade(0xFF)),
            );
        }
    }

    finish_overlay_card(
        prims,
        if state.themes_only {
            "\u{2191}\u{2193} preview   \u{23ce} keep   esc revert"
        } else {
            "\u{2191}\u{2193} move   \u{23ce} run   esc close"
        },
        layout.card,
        layout.card_rows,
        g,
        &r,
    )
}

/// Open a list overlay's tray (the palette, the session picker, the connection map): a
/// drop shadow (two-step with `contact_shadow`), the card surface and its hairline, then
/// the clip the body paints inside ([`finish_overlay_card`] pops it). Opaque: the tray
/// rasterizer has no backdrop blur, so translucency would ghost the owner's text through.
pub(crate) fn overlay_card_frame(
    card: (f32, f32, f32, f32),
    ch: f32,
    r: &Roles,
    contact_shadow: bool,
) -> Vec<DrawPrim> {
    let (card_x, card_y, card_w, card_h) = card;
    let radius = (ch * 0.6).min(14.0);
    let mut prims = vec![DrawPrim::Panel {
        x: card_x - 3.0,
        y: card_y + 2.0,
        w: card_w + 6.0,
        h: card_h + 6.0,
        radius: radius + 3.0,
        fill: rgba([0, 0, 0], 0x2A),
    }];
    if contact_shadow {
        prims.push(DrawPrim::Panel {
            x: card_x - 1.0,
            y: card_y + 2.0,
            w: card_w + 2.0,
            h: card_h + 3.0,
            radius: radius + 1.0,
            fill: rgba([0, 0, 0], 0x30),
        });
    }
    prims.extend([
        DrawPrim::Panel {
            x: card_x,
            y: card_y,
            w: card_w,
            h: card_h,
            radius,
            fill: rgba(r.surface, 0xFF),
        },
        DrawPrim::Stroke {
            x: card_x,
            y: card_y,
            w: card_w,
            h: card_h,
            radius,
            width: 1.0,
            color: rgba(r.separator, 0xE0),
        },
        DrawPrim::ClipPush {
            x: card_x,
            y: card_y,
            w: card_w,
            h: card_h,
        },
    ]);
    prims
}

/// Close a list overlay's tray: the key-`hint` footer centred on the card's last
/// row (Caption, tertiary), then the pop of the body clip [`overlay_card_frame`] pushed.
pub(crate) fn finish_overlay_card(
    mut prims: Vec<DrawPrim>,
    hint: &str,
    card: (f32, f32, f32, f32),
    card_rows: usize,
    g: &SettingsGeom,
    r: &Roles,
) -> TrayInput {
    let (card_x, card_y, card_w, _) = card;
    let (cw, ch) = (g.cw, g.ch);
    let hint_w = text_w(hint, TypeStep::Caption.px(g.font_px).get());
    let fx = card_x + fit((card_w - hint_w) * 0.5, cw, card_w - hint_w - cw);
    let y0 = card_y + card_rows.saturating_sub(1) as f32 * ch;
    let text_at = row_text_writer(g.font_px, ch);
    text_at(
        &mut prims,
        fx,
        y0,
        TypeStep::Caption,
        hint.to_string(),
        rgba(r.text_tertiary, 0xFF),
    );
    prims.push(DrawPrim::ClipPop);
    TrayInput { prims, card }
}

/// The row text writer the list overlays paint with: a Mono `step` run
/// cap-height-centred in the `ch`-tall row at `y0`; empty text paints nothing.
pub(crate) fn row_text_writer(
    px: f32,
    ch: f32,
) -> impl Fn(&mut Vec<DrawPrim>, f32, f32, TypeStep, String, crate::widget::Rgba) {
    move |prims, x, y0, step, s, color| {
        if s.is_empty() {
            return;
        }
        let size = step.px(px);
        prims.push(text_prim(
            x,
            row_baseline(y0, ch, size.get()),
            s,
            size,
            TextWeight::Regular,
            TextFace::Mono,
            color,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_flattens_every_command() {
        let s = PaletteState::new();
        // MENU_MODEL's non-separator items all appear (About, Copy, Settings…, Help, …).
        assert!(s.rows.iter().any(|r| r.action == MenuAction::About));
        assert!(s.rows.iter().any(|r| r.action == MenuAction::Copy));
        assert!(s.rows.iter().any(|r| r.action == MenuAction::Help));
        // Exactly the model's items, submenus flattened: the palette mirrors the bar.
        let model_items = MENU_MODEL
            .iter()
            .flat_map(|sec| sec.entries.iter())
            .flat_map(MenuEntry::items)
            .count();
        assert_eq!(s.rows.len(), model_items);
    }

    /// The connection ids have Fabric palette rows resolvable by name for `invoke`; the
    /// session-subject ones gate on a front terminal, the instance-wide map stays enabled.
    #[test]
    fn connection_ids_have_palette_rows_and_gate_on_a_front_terminal() {
        let mut s = PaletteState::new();
        for action in [
            MenuAction::ConnectToSession,
            MenuAction::ConfigureConnection,
            MenuAction::DisconnectSession,
            MenuAction::ShowConnectionMap,
        ] {
            assert!(
                s.rows.iter().any(|r| r.action == action),
                "{action:?} has a palette row"
            );
            // Resolvable by its invoke name (the `invoke` verb's path).
            assert_eq!(s.action_by_name(&format!("{action:?}")), Ok(action));
        }
        // No front terminal: the session-subject ids disable (and invoke
        // refuses with the disabled reason); the map row stays live.
        let live = PaletteLive {
            terminal_front: false,
            ..PaletteLive::default()
        };
        s.resolve(&live);
        for action in [
            MenuAction::ConnectToSession,
            MenuAction::ConfigureConnection,
            MenuAction::DisconnectSession,
            MenuAction::ShowIdentity,
            MenuAction::ResetTerminal,
        ] {
            let row = s.rows.iter().find(|r| r.action == action).unwrap();
            assert!(!row.enabled, "{action:?} needs a front session");
            assert!(
                s.action_by_name(&format!("{action:?}"))
                    .is_err_and(|e| e.contains("disabled")),
                "{action:?} refuses with the disabled reason"
            );
        }
        assert!(
            s.rows
                .iter()
                .find(|r| r.action == MenuAction::ShowConnectionMap)
                .unwrap()
                .enabled,
            "the map is instance-wide"
        );
    }

    /// The halt pair follows the live hold: Hold with nothing standing, Lift against a
    /// local hold, both grey with the fleet's reason under a fleet hold (and `invoke`
    /// refuses them). The presence rows check from the live bits, and the Fabric menu's
    /// session rows need a front terminal.
    #[test]
    fn the_halt_pair_follows_the_hold_and_the_presence_rows_check() {
        let mut s = PaletteState::new();
        let row = |s: &PaletteState, a: MenuAction| {
            s.rows
                .iter()
                .find(|r| r.action == a)
                .cloned()
                .unwrap_or_else(|| panic!("{a:?} has a row"))
        };
        let live = PaletteLive {
            terminal_front: true,
            can_rename: true,
            presence_band: true,
            presence_rim: false,
            ..PaletteLive::default()
        };
        s.resolve(&live);
        assert!(row(&s, MenuAction::HoldSession).enabled);
        assert!(!row(&s, MenuAction::LiftHold).enabled);
        assert_eq!(row(&s, MenuAction::TogglePresenceBand).checked, Some(true));
        assert_eq!(row(&s, MenuAction::TogglePresenceRim).checked, Some(false));
        for a in [
            MenuAction::Inbox,
            MenuAction::LedgerForSession,
            MenuAction::SetRole,
        ] {
            assert!(row(&s, a).enabled, "{a:?} with a front session");
        }

        // A local hold: Lift is the live one.
        s.resolve(&PaletteLive {
            front_hold: Some(crate::presence::HoldFact {
                reason: "pause".into(),
                fleet: false,
            }),
            ..live.clone()
        });
        assert!(!row(&s, MenuAction::HoldSession).enabled);
        assert!(row(&s, MenuAction::LiftHold).enabled);
        assert_eq!(
            row(&s, MenuAction::LiftHold).label,
            "Lift Hold (This Session)"
        );

        // A fleet hold: neither, and both say why.
        s.resolve(&PaletteLive {
            front_hold: Some(crate::presence::HoldFact {
                reason: "main%20broken".into(),
                fleet: true,
            }),
            ..live.clone()
        });
        for a in [MenuAction::HoldSession, MenuAction::LiftHold] {
            let r = row(&s, a);
            assert!(!r.enabled, "{a:?} under a fleet hold");
            assert!(
                r.label
                    .ends_with("\u{2014} fleet hold: main broken, cannot be lifted here"),
                "{a:?}: {}",
                r.label
            );
            assert!(
                s.action_by_name(&format!("{a:?}"))
                    .is_err_and(|e| e.contains("disabled")),
                "{a:?} refuses with the disabled reason"
            );
        }
        // Lifted: back to the first state, the label restored.
        s.resolve(&live);
        assert_eq!(
            row(&s, MenuAction::LiftHold).label,
            "Lift Hold (This Session)"
        );
        assert!(row(&s, MenuAction::HoldSession).enabled);

        // No front session: the session rows grey, the instance rows do not.
        s.resolve(&PaletteLive {
            terminal_front: false,
            ..PaletteLive::default()
        });
        for a in [
            MenuAction::Inbox,
            MenuAction::LedgerForSession,
            MenuAction::HoldSession,
            MenuAction::LiftHold,
            MenuAction::SetRole,
        ] {
            assert!(!row(&s, a).enabled, "{a:?} needs a session");
        }
        for a in [
            MenuAction::ShowConnectionMap,
            MenuAction::FabricStatus,
            MenuAction::FabricOn,
            MenuAction::FabricOff,
        ] {
            assert!(row(&s, a).enabled, "{a:?} is instance-wide");
        }
    }

    /// The accelerator projection, both platforms in one table: macOS keeps every string;
    /// elsewhere `Cmd-` alternatives drop, a `·`-separated pair keeps its portable half,
    /// and a lone `Cmd-*` becomes empty.
    #[test]
    fn platform_accel_keeps_only_chords_that_exist_here() {
        let cases = [
            // (source, macOS, elsewhere)
            ("Cmd-S \u{b7} C-x C-s", "Cmd-S \u{b7} C-x C-s", "C-x C-s"),
            ("Cmd-Z \u{b7} C-/", "Cmd-Z \u{b7} C-/", "C-/"),
            ("Cmd-F", "Cmd-F", ""),
            ("Cmd-Shift-Z", "Cmd-Shift-Z", ""),
            // Nothing macOS-only in these: they survive everywhere.
            ("C-s", "C-s", "C-s"),
            ("M-g g", "M-g g", "M-g g"),
            ("Shift-F8", "Shift-F8", "Shift-F8"),
            ("Esc", "Esc", "Esc"),
            ("Ctrl-Shift-F", "Ctrl-Shift-F", "Ctrl-Shift-F"),
            ("", "", ""),
        ];
        for (source, mac, other) in cases {
            let want = if cfg!(target_os = "macos") {
                mac
            } else {
                other
            };
            assert_eq!(platform_accel(source), want, "{source:?}");
        }
    }

    /// `resolve` writes `PaletteLive::menu_accels` chords into menu rows' `shortcut` slot,
    /// so `row_accel` shows the chord that works here; an action with no hint keeps its
    /// platform default, and a re-resolve without hints clears them.
    #[test]
    fn resolve_applies_menu_accel_hints_to_row_accel() {
        let by_action = |s: &PaletteState, action: MenuAction| {
            let row = s.rows.iter().find(|r| r.action == action).expect("row");
            row_accel(row)
        };
        let mut s = PaletteState::new();
        s.resolve(&PaletteLive {
            menu_accels: vec![
                (MenuAction::Copy, "Ctrl+Shift+C".to_string()),
                (MenuAction::ToggleFullScreen, "F11".to_string()),
            ],
            has_selection: true,
            ..Default::default()
        });
        assert_eq!(by_action(&s, MenuAction::Copy), "Ctrl+Shift+C");
        assert_eq!(by_action(&s, MenuAction::ToggleFullScreen), "F11");
        // In full screen the toggle's row names what it does now, and a
        // re-resolve out of full screen names it back.
        let label = |s: &PaletteState| {
            let row = s
                .rows
                .iter()
                .find(|r| r.action == MenuAction::ToggleFullScreen)
                .expect("row");
            (row.label.to_string(), row.checked)
        };
        let mut full = PaletteState::new();
        full.resolve(&PaletteLive {
            fullscreen: true,
            ..Default::default()
        });
        assert_eq!(label(&full), ("Exit Full Screen".to_string(), None));
        full.resolve(&PaletteLive::default());
        assert_eq!(label(&full), ("Enter Full Screen".to_string(), None));
        // ⌘W's row names what it closes: the focused pane of a split tab.
        let close = |s: &PaletteState| {
            s.rows
                .iter()
                .find(|r| r.action == MenuAction::CloseTab)
                .expect("row")
                .label
                .to_string()
        };
        full.resolve(&PaletteLive {
            front_tab_split: true,
            ..Default::default()
        });
        assert_eq!(close(&full), "Close Pane");
        full.resolve(&PaletteLive::default());
        assert_eq!(close(&full), "Close Tab");
        // No hint for Paste in this live set: the row paints its platform
        // default — the ⌘ menu mirror on macOS, blank elsewhere.
        assert_eq!(
            by_action(&s, MenuAction::Paste),
            if cfg!(target_os = "macos") {
                "\u{2318}V"
            } else {
                ""
            }
        );
        // Hints are per-resolve live state: resolving without them clears them.
        s.resolve(&PaletteLive {
            has_selection: true,
            ..Default::default()
        });
        assert_eq!(
            by_action(&s, MenuAction::ToggleFullScreen),
            if cfg!(target_os = "macos") {
                "\u{2303}\u{2318}F"
            } else {
                ""
            }
        );
    }

    /// [`accel_is_mac_only`] directly (`platform_accel` is `cfg!`-selected, so a table
    /// through it runs one column per build), in every spelling of a macOS chord.
    #[test]
    fn accel_mac_only_catches_every_spelling_of_a_macos_chord() {
        for mac_only in [
            "Cmd-S",
            "Cmd+S",
            "Cmd-Shift-Z",
            "Shift-Cmd-Z",
            "Command-S",
            "\u{2318}S",
            "Shift-\u{2318}S",
            "Opt-Left",
            "Option-Left",
            "\u{2325}\u{2318}C",
        ] {
            assert!(
                accel_is_mac_only(mac_only),
                "{mac_only:?} names a chord that does not exist off macOS"
            );
        }
        // …and everything reachable off macOS survives, including the Alt/Meta
        // spellings of the ⌥ key.
        for portable in [
            "C-s",
            "C-x C-s",
            "C-/",
            "M-g g",
            "M-x",
            "M-s",
            "Alt-Left",
            "Ctrl-A",
            "Ctrl-Shift-F",
            "Shift-F8",
            "F8",
            "Esc",
            "",
        ] {
            assert!(!accel_is_mac_only(portable), "{portable:?} exists here");
        }
    }

    #[test]
    fn native_commands_prepend_one_scoped_identity_safe_section() {
        let window = crate::WindowId(7);
        let instance = crate::tab_model::AppInstanceId::from_stored(11);
        let view = crate::tab_model::ViewId::from_stored(13);
        let s = PaletteState::new().with_native_commands(NativeCommandScope {
            window,
            instance,
            view,
            generation: 17,
            section: "Editor App".to_string(),
            commands: vec![
                Command {
                    id: ActionId::new("editor/save"),
                    title: "Save".to_string(),
                    shortcut: Some("Cmd-S".to_string()),
                    enabled: true,
                },
                Command {
                    id: ActionId::new("editor/disabled"),
                    title: "Unavailable".to_string(),
                    shortcut: None,
                    enabled: false,
                },
            ],
        });

        assert_eq!(s.rows[0].section, "Editor App");
        assert_eq!(s.rows[0].label, "Save");
        // The metadata is kept verbatim; the projection is platform-aware, so a lone
        // macOS key-equivalent leaves nothing off macOS.
        let expect_accel = if cfg!(target_os = "macos") {
            "Cmd-S"
        } else {
            ""
        };
        assert_eq!(row_accel(&s.rows[0]), expect_accel);
        assert!(matches!(
            &s.rows[0].action,
            PaletteTarget::Native(target)
                if target.window == window
                    && target.instance == instance
                    && target.view == view
                    && target.generation == 17
                    && target.action.as_str() == "editor/save"
        ));
        assert!(
            matches!(&s.rows[2].action, PaletteTarget::Menu(_)),
            "global menu order follows the scoped app section"
        );

        let controls = s.controls_lines();
        let expect_line = format!("accel={expect_accel:?}");
        assert!(controls.iter().any(|line| {
            line.contains("target=native window=7 instance=11 view=13 generation=17")
                && line.contains("action=editor/save")
                && line.contains(&expect_line)
        }));
    }

    fn themes() -> Vec<PaletteTheme> {
        [
            ("Default", aterm_types::Appearance::Dark, true),
            ("Gruvbox Dark", aterm_types::Appearance::Dark, false),
            ("Gruvbox Light", aterm_types::Appearance::Light, false),
        ]
        .into_iter()
        .map(|(id, appearance, current)| PaletteTheme {
            id: id.to_string(),
            label: id.to_string(),
            appearance,
            current,
        })
        .collect()
    }

    /// The Theme section follows the menu rows, each row naming its theme as
    /// its target and its appearance as the hint; the scoped palette shows only
    /// those rows, starts on the checked one, and its highlight is an edge:
    /// asked once per move, never for the row it opened on, `Some(None)` when
    /// it leaves the section.
    #[test]
    fn theme_rows_scope_and_highlight_edge() {
        let mut s = PaletteState::new().with_themes(themes());
        let menu_rows = PaletteState::new().rows.len();
        // The three themes, then the Browse row.
        assert_eq!(s.rows.len(), menu_rows + 4);
        assert!(
            s.rows[..menu_rows]
                .iter()
                .all(|r| r.action.menu().is_some())
        );
        let browse = s.rows.last().expect("the Browse row");
        assert_eq!(browse.section, THEME_SECTION);
        assert!(browse.action == MenuAction::BrowseThemes);
        // Rebuilding the section replaces it, never stacks a second.
        let rebuilt = PaletteState::new()
            .with_themes(themes())
            .with_themes(themes());
        assert_eq!(rebuilt.rows.len(), menu_rows + 4);
        assert_eq!(
            s.take_theme_highlight_change(),
            None,
            "opening asks nothing"
        );
        let lines = s.controls_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("target=theme id=\"Gruvbox Light\"")
                    && l.contains("section=\"Theme\"")),
            "{lines:?}"
        );
        for c in "gruv".chars() {
            s.push_char(c);
        }
        assert_eq!(s.highlighted_theme(), Some("Gruvbox Dark"));
        assert_eq!(
            s.take_theme_highlight_change(),
            Some(Some("Gruvbox Dark".to_string()))
        );
        assert_eq!(s.take_theme_highlight_change(), None, "unmoved, unasked");
        s.move_selection(1);
        assert_eq!(
            s.take_theme_highlight_change(),
            Some(Some("Gruvbox Light".to_string()))
        );
        while !s.query.is_empty() {
            s.edit(crate::app_search::SearchEdit::DeleteBack);
        }
        assert_eq!(
            s.take_theme_highlight_change(),
            Some(None),
            "left the section"
        );

        let mut scoped = PaletteState::new().with_themes(themes()).themes_only();
        assert!(scoped.is_themes_only());
        assert_eq!(scoped.filtered().len(), 4);
        assert_eq!(scoped.scroll_extent().1, 4, "the count is of the scope");
        assert_eq!(
            scoped.highlighted_theme(),
            Some("Default"),
            "on the checked row"
        );
        assert_eq!(scoped.take_theme_highlight_change(), None);
        scoped.move_selection(-1);
        assert_eq!(
            scoped.highlighted_theme(),
            None,
            "wraps in scope, onto Browse all themes…"
        );
        assert_eq!(scoped.take_theme_highlight_change(), Some(None));
        scoped.move_selection(-1);
        assert_eq!(scoped.highlighted_theme(), Some("Gruvbox Light"));
        // A native scope change keeps the Theme rows.
        scoped.replace_native_commands(None);
        assert_eq!(scoped.filtered().len(), 4);
    }

    #[test]
    fn filter_narrows_the_list() {
        let mut s = PaletteState::new();
        let all = s.filtered().len();
        for c in "settings".chars() {
            s.push_char(c);
        }
        let narrowed = s.filtered();
        assert!(narrowed.len() < all, "filtering narrows the set");
        assert!(
            narrowed
                .iter()
                .all(|&i| s.rows[i].action == MenuAction::ToggleSettings
                    || s.rows[i].label.to_ascii_lowercase().contains("settings")),
            "every survivor matches 'settings'"
        );
        // Backspace widens again.
        s.edit(crate::app_search::SearchEdit::DeleteBack);
        assert!(s.filtered().len() >= narrowed.len());
    }

    /// The filter edits through the shared one-line field model with its caret held
    /// at the end: typing appends, the backward kills delete, and the motions and
    /// forward kills (which need a caret before the end) change nothing. Only a
    /// changed query re-narrows from the top.
    #[test]
    fn edit_applies_the_field_model_with_the_caret_at_the_end() {
        use crate::app_search::SearchEdit;
        let mut s = PaletteState::new();
        // Most rows carry an `e`, so the cursor has somewhere to move.
        assert!(s.edit(SearchEdit::Insert("new e".to_string())));
        assert_eq!(s.query, "new e");
        assert!(s.edit(SearchEdit::DeleteBack));
        assert_eq!(s.query, "new ");
        assert!(s.filtered().len() > 1, "{:?}", s.controls_lines());
        s.move_selection(1);
        assert_eq!(s.selected, 1);
        for unchanged in [
            SearchEdit::MoveStart,
            SearchEdit::MoveEnd,
            SearchEdit::MoveCharLeft,
            SearchEdit::MoveWordLeft,
            SearchEdit::KillToEnd,
            SearchEdit::DeleteForward,
            SearchEdit::DeleteWordForward,
        ] {
            assert!(!s.edit(unchanged.clone()), "{unchanged:?}");
            assert_eq!(s.query, "new ", "{unchanged:?}");
        }
        assert_eq!(
            s.selected, 1,
            "an edit that changed nothing keeps the cursor"
        );
        // The caret a motion left is not kept: typing still appends.
        assert!(s.edit(SearchEdit::Insert("tab".to_string())));
        assert_eq!(s.query, "new tab");
        assert_eq!(s.selected, 0, "a changed query re-narrows from the top");
        assert!(s.edit(SearchEdit::DeleteWordBack));
        assert_eq!(s.query, "new ");
        assert!(s.edit(SearchEdit::KillToStart));
        assert_eq!(s.query, "");
        assert!(!s.edit(SearchEdit::DeleteBack), "nothing left to delete");
    }

    #[test]
    fn activate_yields_the_selected_action() {
        let mut s = PaletteState::new();
        for c in "copy".chars() {
            s.push_char(c);
        }
        // First filtered row is the Copy command; enabled by default.
        assert_eq!(s.selected_action(), Some(MenuAction::Copy));
        // Disable it (no selection) → Enter is inert.
        let live = PaletteLive::default();
        s.resolve(&live);
        assert_eq!(
            s.selected_action(),
            None,
            "a disabled command does not activate"
        );
        // With a selection it activates again.
        s.resolve(&PaletteLive {
            has_selection: true,
            ..Default::default()
        });
        assert_eq!(s.selected_action(), Some(MenuAction::Copy));
    }

    #[test]
    fn serious_mode_row_tracks_process_policy_on_every_content_kind() {
        let mut state = PaletteState::new();
        state.resolve(&PaletteLive {
            serious_mode: true,
            native_tab_active: true,
            ..Default::default()
        });
        let serious = state
            .rows
            .iter()
            .find(|row| row.action == MenuAction::ToggleSeriousMode)
            .expect("View menu contributes Serious Mode");
        assert_eq!(serious.checked, Some(true));
        assert!(
            serious.enabled,
            "the process-wide policy is not terminal-only"
        );

        state.resolve(&PaletteLive::default());
        let serious = state
            .rows
            .iter()
            .find(|row| row.action == MenuAction::ToggleSeriousMode)
            .unwrap();
        assert_eq!(serious.checked, Some(false));
        assert!(serious.enabled, "it remains available while windowless");
    }

    /// The View ▸ Matrix Rain row checks with the front session's effective rain and
    /// disables with no front terminal (a native whole tab, or no window).
    #[test]
    fn matrix_rain_row_checkmark_tracks_front_session_and_disables_native() {
        let mut s = PaletteState::new();
        s.resolve(&PaletteLive {
            rain_on: true,
            terminal_front: true,
            can_rename: true,
            ..Default::default()
        });
        let rain = s
            .rows
            .iter()
            .find(|r| r.action == MenuAction::ToggleMatrixRain)
            .expect("View menu contributes Matrix Rain");
        assert_eq!(rain.checked, Some(true));
        assert!(rain.enabled, "terminal front: toggleable");

        s.resolve(&PaletteLive {
            rain_on: false,
            native_tab_active: true,
            ..Default::default()
        });
        let rain = s
            .rows
            .iter()
            .find(|r| r.action == MenuAction::ToggleMatrixRain)
            .unwrap();
        assert_eq!(rain.checked, Some(false));
        assert!(!rain.enabled, "native whole tab: no session to toggle");

        // Windowless (macOS runs with every window closed): no native tab but no
        // terminal either, so the row disables and the invoke fence refuses.
        s.resolve(&PaletteLive::default());
        let rain = s
            .rows
            .iter()
            .find(|r| r.action == MenuAction::ToggleMatrixRain)
            .unwrap();
        assert!(
            !rain.enabled,
            "no window: nothing to toggle, refuse honestly"
        );
    }

    /// The View ▸ Favourite This Kitty row checks when the launch kitty is the pin and,
    /// unlike the rain toggle, stays enabled with no front terminal: the launch kitty is
    /// process-wide.
    #[test]
    fn favourite_row_checks_when_pinned_and_stays_enabled_without_a_terminal() {
        let mut s = PaletteState::new();
        s.resolve(&PaletteLive {
            kitty_favourited: true,
            terminal_front: true,
            can_rename: true,
            ..Default::default()
        });
        // (checked, enabled): the two bits this test pins.
        let row = |s: &PaletteState| {
            s.rows
                .iter()
                .find(|r| r.action == MenuAction::FavouriteKitty)
                .map(|r| (r.checked, r.enabled))
                .expect("View menu contributes Favourite This Kitty")
        };
        assert_eq!(
            row(&s),
            (Some(true), true),
            "terminal front: pinned and promotable"
        );

        s.resolve(&PaletteLive {
            native_tab_active: true,
            ..Default::default()
        });
        assert_eq!(
            row(&s),
            (Some(false), true),
            "native whole tab: the launch kitty is still there to pin"
        );

        // Windowless (macOS runs with every window closed): no native tab, no terminal.
        s.resolve(&PaletteLive::default());
        assert_eq!(
            row(&s),
            (Some(false), true),
            "no window: the launch kitty is process-wide, still promotable"
        );
    }

    #[test]
    fn reopen_closed_tab_row_tracks_live_snapshot_availability() {
        let mut s = PaletteState::new();
        s.resolve(&PaletteLive::default());
        let reopen = s
            .rows
            .iter()
            .find(|row| row.action == MenuAction::ReopenClosedTab)
            .expect("File menu contributes Reopen Closed Native Tab");
        assert!(!reopen.enabled);

        s.resolve(&PaletteLive {
            can_reopen_closed_tab: true,
            ..PaletteLive::default()
        });
        assert!(
            s.rows
                .iter()
                .find(|row| row.action == MenuAction::ReopenClosedTab)
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn local_file_open_rows_track_platform_picker_availability() {
        let mut state = PaletteState::new();
        let actions = [MenuAction::OpenMarkdown, MenuAction::OpenEditor];

        state.resolve(&PaletteLive {
            local_file_picker_available: false,
            ..PaletteLive::default()
        });
        for action in actions {
            let row = state.rows.iter().find(|row| row.action == action).unwrap();
            assert!(!row.enabled, "{action:?} disabled without a picker");
        }

        state.resolve(&PaletteLive {
            local_file_picker_available: true,
            ..PaletteLive::default()
        });
        for action in actions {
            let row = state.rows.iter().find(|row| row.action == action).unwrap();
            assert!(row.enabled, "{action:?} enabled with a picker");
        }
    }

    #[test]
    fn move_wraps_over_filtered_set() {
        let mut s = PaletteState::new();
        for c in "tab".chars() {
            s.push_char(c);
        }
        let n = s.filtered().len();
        assert!(n >= 2, "expected several tab commands");
        s.move_selection(-1);
        assert_eq!(s.selected, n - 1, "up from row 0 wraps to the last row");
        s.move_selection(1);
        assert_eq!(s.selected, 0, "and back to the top");
    }

    #[test]
    fn fingerprint_tracks_query_and_cursor() {
        let mut s = PaletteState::new();
        let a = s.fingerprint();
        assert_ne!(
            a, 0,
            "an open overlay never hashes to the closed sentinel 0"
        );
        s.push_char('f');
        let b = s.fingerprint();
        assert_ne!(b, a, "typing changes the fingerprint");
        s.move_selection(1);
        assert_ne!(s.fingerprint(), b, "moving changes the fingerprint");
    }

    /// Staged: resolve adds the one-click "↑ Install aterm v<staged> now" row at the head
    /// of the Version section, enabled, full-alpha, and activatable to `ApplyUpdate`.
    #[test]
    fn resolve_staged_adds_one_click_update_row() {
        let model = aterm_models_misc::native_update_menu_activation_model();
        let mut model_state = model.init_state();
        assert!(model.fire("StageUpdate", &mut model_state));

        let mut s = PaletteState::new();
        s.resolve(&PaletteLive {
            staged: Some((999, "9.9".to_string())),
            ..Default::default()
        });
        assert!(model.fire("RefreshStagedVersionMenu", &mut model_state));
        let i = s
            .rows
            .iter()
            .position(|r| r.action == MenuAction::ApplyUpdate)
            .expect("staged resolve keeps the ApplyUpdate row");
        let row = &s.rows[i];
        assert_eq!(row.section, "Version");
        assert_eq!(row.label, "\u{2191} Install aterm v9.9 now");
        assert!(!row.label.contains("restart"), "{}", row.label);
        assert!(row.enabled, "one click must work");
        // It sits directly before the About row, at the head of the Version section.
        assert_eq!(s.rows[i + 1].action, MenuAction::Version);
        // The staged row never fades (it is not the realized arrow).
        assert_eq!(s.row_alpha(&s.rows[i], Instant::now()), 1.0);
        // Filtering to it and pressing Enter dispatches ApplyUpdate.
        for c in "install aterm".chars() {
            s.push_char(c);
        }
        assert_eq!(s.selected_action(), Some(MenuAction::ApplyUpdate));
        assert!(model.fire("DecodeApplyTag", &mut model_state));
        assert!(model.fire("DispatchApply", &mut model_state));
        assert_eq!(model_state["apply_dispatched"], 1);
        // `controls menu` lists it too.
        assert!(
            s.controls_lines()
                .iter()
                .any(|l| l.contains("action=ApplyUpdate") && l.contains("Install aterm v9.9")),
            "controls menu shows the staged row"
        );
    }

    /// Neither staged nor realized: the ApplyUpdate row is removed, and a later resolve
    /// with a staged build re-inserts it.
    #[test]
    fn resolve_without_update_state_removes_the_row() {
        let mut s = PaletteState::new();
        assert!(
            s.rows.iter().any(|r| r.action == MenuAction::ApplyUpdate),
            "the static model carries the row"
        );
        s.resolve(&PaletteLive::default());
        assert!(
            !s.rows.iter().any(|r| r.action == MenuAction::ApplyUpdate),
            "no update state ⇒ no update row"
        );
        // Live transition: a build stages while the palette is open → re-resolve
        // re-inserts the row (the `palette_refresh_live` path).
        s.resolve(&PaletteLive {
            staged: Some((999, "9.9".to_string())),
            ..Default::default()
        });
        assert!(s.rows.iter().any(|r| r.action == MenuAction::ApplyUpdate));
    }

    /// "Check for Updates…" is enabled here exactly where the `chrome` verb's menu lines
    /// list it: both read [`MenuAction::offered_on_this_platform`]. With nothing staged
    /// the apply-now row is absent everywhere.
    #[test]
    fn update_rows_follow_the_platform_updater_on_every_surface() {
        let mut s = PaletteState::new();
        s.resolve(&PaletteLive::default());
        let enabled = s
            .rows
            .iter()
            .find(|r| r.action == MenuAction::SoftwareUpdate)
            .expect("the row stays listed, greyed where it cannot act")
            .enabled;
        assert_eq!(
            enabled,
            MenuAction::SoftwareUpdate.offered_on_this_platform()
        );
        assert_eq!(enabled, aterm_update::enabled());
        if cfg!(windows) {
            assert!(!enabled, "Windows has no in-app updater");
        }
        let chrome = crate::menu::menu_chrome_lines(
            apply_row_label(&PaletteLive::default()).as_deref(),
            false,
        );
        let chrome_offers = chrome
            .iter()
            .any(|line| line.contains("Check for Updates…"));
        assert!(
            !chrome.iter().any(|line| line.contains("Install")),
            "nothing staged: `chrome` lists no install row either: {chrome:?}"
        );
        assert_eq!(
            chrome_offers, enabled,
            "`chrome` lists the row exactly where `controls menu` enables it"
        );
        assert!(
            s.controls_lines().iter().any(|line| {
                line.contains("action=SoftwareUpdate")
                    && line.contains(&format!("enabled={enabled}"))
            }),
            "`controls menu` reports the same state"
        );
        assert!(
            !s.rows.iter().any(|r| r.action == MenuAction::ApplyUpdate),
            "nothing staged, so no apply-now row"
        );
    }

    /// Realized: the post-update row fades (full at boot, partial mid-TTL, zero at TTL),
    /// freezes at full under reduced motion, and yields to a staged build.
    #[test]
    fn resolve_realized_adds_fading_row() {
        use crate::relaunch_notice::REALIZED_ARROW_TTL;
        let now = Instant::now();
        let mut s = PaletteState::new();
        s.resolve(&PaletteLive {
            realized: Some(("9.9".to_string(), now)),
            ..Default::default()
        });
        let row = s
            .rows
            .iter()
            .find(|r| r.action == MenuAction::ApplyUpdate)
            .expect("realized resolve keeps the row");
        assert_eq!(row.label, "\u{2191} Updated to aterm v9.9");
        assert_eq!(s.row_alpha(row, now), 1.0, "full at spawn");
        let mid = s.row_alpha(row, now + REALIZED_ARROW_TTL / 2);
        assert!(mid > 0.0 && mid < 1.0, "mid-TTL alpha {mid}");
        assert_eq!(s.row_alpha(row, now + REALIZED_ARROW_TTL), 0.0, "expired");
        // Ordinary rows never fade.
        let about = s
            .rows
            .iter()
            .find(|r| r.action == MenuAction::Version)
            .unwrap();
        assert_eq!(s.row_alpha(about, now + REALIZED_ARROW_TTL / 2), 1.0);
        // Reduced motion freezes the fade at full.
        s.resolve(&PaletteLive {
            realized: Some(("9.9".to_string(), now)),
            reduced_motion: true,
            ..Default::default()
        });
        let frozen = s
            .rows
            .iter()
            .find(|r| r.action == MenuAction::ApplyUpdate)
            .unwrap();
        assert_eq!(s.row_alpha(frozen, now + REALIZED_ARROW_TTL / 2), 1.0);
        // A staged build supersedes the celebration wording.
        s.resolve(&PaletteLive {
            staged: Some((999, "10.0".to_string())),
            realized: Some(("9.9".to_string(), now)),
            ..Default::default()
        });
        let row = s
            .rows
            .iter()
            .find(|r| r.action == MenuAction::ApplyUpdate)
            .unwrap();
        assert!(row.label.contains("Install aterm v10.0"), "{:?}", row.label);
    }

    /// The fingerprint folds the realized row's elapsed bucket while unexpired, so an open
    /// palette re-presents per fade step; under reduced motion it does not.
    #[test]
    fn fingerprint_folds_the_realized_bucket() {
        use std::time::Duration;

        use crate::relaunch_notice::REALIZED_BUCKET;
        let mk = |since: Instant, reduced: bool| {
            let mut s = PaletteState::new();
            s.resolve(&PaletteLive {
                realized: Some(("9.9".to_string(), since)),
                reduced_motion: reduced,
                ..Default::default()
            });
            s.fingerprint()
        };
        let now = Instant::now();
        let bucket0 = mk(now, false);
        let bucket1 = mk(now - REALIZED_BUCKET - Duration::from_secs(1), false);
        assert_ne!(
            bucket0, bucket1,
            "a bucket edge re-fingerprints (→ one present)"
        );
        // Frozen: the bucket is not folded, so elapsed time leaves the fingerprint alone.
        let frozen0 = mk(now, true);
        let frozen1 = mk(now - REALIZED_BUCKET - Duration::from_secs(1), true);
        assert_eq!(frozen0, frozen1, "reduced motion pins the fp");
    }

    #[test]
    fn floating_card_is_centered_width_capped_and_clamped_on_small_viewports() {
        let state = PaletteState::new();
        assert_eq!(
            <PaletteState as crate::overlay::OverlayModel>::wanted_rows(&state, 40),
            40,
            "the floating card receives the whole viewport for centering"
        );
        let wide = SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 200,
            panel_rows: 40,
        };
        let layout = palette_layout(&state, &wide);
        let (x, y, width, height) = layout.card;
        let tray_w = wide.cols as f32 * wide.cw;
        let tray_h = wide.panel_rows as f32 * wide.ch;
        assert_eq!(width, MAX_CARD_WIDTH);
        assert_eq!(height, state.wanted_rows() as f32 * wide.ch);
        assert!(((x * 2.0 + width) - tray_w).abs() < f32::EPSILON);
        assert!(((y * 2.0 + height) - tray_h).abs() < f32::EPSILON);

        let small = SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 3,
            panel_rows: 2,
        };
        let layout = palette_layout(&state, &small);
        let (x, y, width, height) = layout.card;
        let tray_w = small.cols as f32 * small.cw;
        let tray_h = small.panel_rows as f32 * small.ch;
        assert!(x >= 0.0 && y >= 0.0);
        assert!(x + width <= tray_w && y + height <= tray_h);
        assert_eq!(width, small.cw * 2.0, "margins collapse before the card");
        assert_eq!(height, tray_h, "height clamps to the available rows");
        assert_eq!(layout.body_rows, 0, "chrome cannot overlap command rows");
    }

    #[test]
    fn every_painted_row_is_contained_by_the_card_and_outside_points_miss() {
        let state = PaletteState::new();
        let geom = SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 180,
            panel_rows: 36,
        };
        let layout = palette_layout(&state, &geom);
        let (card_x, card_y, card_w, card_h) = layout.card;
        for slot in 0..layout.body_rows {
            let (x, y, width, height) = palette_row_rect(&state, &geom, slot).unwrap();
            assert!(x >= card_x && y >= card_y);
            assert!(x + width <= card_x + card_w);
            assert!(y + height <= card_y + card_h);
        }
        assert_eq!(
            palette_row_hit(&state, &geom, card_x - 1.0, card_y + card_h * 0.5),
            None
        );
        assert_eq!(
            palette_row_hit(&state, &geom, card_x + card_w + 1.0, card_y + card_h * 0.5,),
            None
        );
        assert_eq!(palette_row_hit(&state, &geom, card_x, card_y - 1.0), None);
        assert_eq!(
            palette_row_hit(&state, &geom, card_x, card_y + card_h + 1.0),
            None
        );
    }

    #[test]
    fn pointer_hit_rect_is_the_exact_visible_selection_rect_even_when_scrolled() {
        let mut state = PaletteState::new();
        state.selected = state.filtered().len() - 1;
        state.clamp_scroll();
        assert!(state.scroll > 0, "fixture exercises the scrolled band");
        let geom = SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 80,
            panel_rows: state.wanted_rows(),
        };

        for slot in 0..state.body() {
            let (x, y, width, height) = palette_row_rect(&state, &geom, slot).unwrap();
            assert_eq!(
                palette_row_hit(&state, &geom, x + width * 0.5, y + height * 0.5),
                Some(state.scroll + slot)
            );
        }
        let (x, y, width, height) = palette_row_rect(&state, &geom, 0).unwrap();
        assert_eq!(palette_row_hit(&state, &geom, x - 0.01, y), None);
        assert_eq!(palette_row_hit(&state, &geom, x + width, y), None);
        assert_eq!(palette_row_hit(&state, &geom, x, y - 0.01), None);
        assert_eq!(palette_row_hit(&state, &geom, x, y + height), None);

        // The selected wash emitted by the painter uses this very rectangle.
        state.selected = state.scroll;
        let tray = palette_tray(&state, &geom, Theme::default());
        assert!(tray.prims.iter().any(|primitive| {
            matches!(primitive, DrawPrim::Panel { x: px, y: py, w, h, .. }
                if *px == x && *py == y && *w == width && *h == height)
        }));
    }

    #[test]
    fn pointer_release_requires_same_exact_enabled_target() {
        let mut state = PaletteState::new();
        assert!(state.pointer_press(Some(0)));
        let (_, activate) = state.pointer_release(Some(1));
        assert!(
            !activate,
            "pressing one row and releasing on another is inert"
        );

        let _ = state.pointer_press(Some(1));
        let (_, activate) = state.pointer_release(Some(1));
        assert!(activate, "same enabled target activates on release");

        for c in "copy".chars() {
            state.push_char(c);
        }
        state.resolve(&PaletteLive::default());
        let copy = state
            .filtered()
            .iter()
            .position(|&row| state.rows[row].action == MenuAction::Copy)
            .unwrap();
        let _ = state.pointer_press(Some(copy));
        let (_, activate) = state.pointer_release(Some(copy));
        assert!(!activate, "disabled rows remain pointer-inert");
    }

    #[test]
    fn wheel_scroll_clamps_at_both_ends_and_keeps_selection_visible() {
        let mut state = PaletteState::new();
        let count = state.filtered().len();
        let body = state.body();
        assert!(count > body, "fixture has a scrollable command set");

        assert!(state.scroll_by(isize::MAX));
        assert_eq!(state.scroll, count - body, "bottom clamp");
        assert!(state.selected >= state.scroll);
        assert!(state.selected < state.scroll + body);
        assert!(!state.scroll_by(1), "wheel cannot move beyond bottom");

        assert!(state.scroll_by(isize::MIN));
        assert_eq!(state.scroll, 0, "top clamp");
        assert!(state.selected < body);
        assert!(!state.scroll_by(-1), "wheel cannot move beyond top");
    }

    /// The a11y MenuItem set equals `filtered()` (the rows the card paints and `controls
    /// menu` counts as `shown=`), focus follows `selected`, and a disabled row omits `Click`.
    #[cfg(a11y_tree)]
    #[test]
    fn palette_a11y_lists_filtered_rows() {
        use aterm_a11y::model::{Action, Role};

        let mut s = PaletteState::new();
        for c in "tab".chars() {
            s.push_char(c);
        }
        let vis = s.filtered();
        assert!(vis.len() >= 2, "expected several tab commands");
        let update = palette_a11y(&s);

        // One MenuItem per filtered row, at the current epoch + slot, labelled like the row.
        let items = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::MenuItem)
            .count();
        assert_eq!(items, vis.len(), "one MenuItem per filtered row");
        for (slot, &i) in vis.iter().enumerate() {
            let id = s.a11y_node_id(slot);
            let (_, node) = update.nodes.iter().find(|(nid, _)| *nid == id).unwrap();
            assert_eq!(node.label(), Some(s.rows[i].label.as_ref()));
        }
        // Focus = the selected row's node.
        assert_eq!(update.focus, s.a11y_node_id(s.selected));
        // The a11y item count agrees with `controls menu`'s shown= count.
        let header = s
            .controls_lines()
            .into_iter()
            .find(|l| l.starts_with("menu rows="))
            .unwrap();
        assert!(header.contains(&format!("shown={}", vis.len())));

        // A disabled row (Copy with no selection) is focusable but not clickable.
        let mut s2 = PaletteState::new();
        for c in "copy".chars() {
            s2.push_char(c);
        }
        s2.resolve(&PaletteLive::default());
        let vis2 = s2.filtered();
        let copy_slot = vis2
            .iter()
            .position(|&i| s2.rows[i].action == MenuAction::Copy)
            .expect("Copy row present");
        let t2 = palette_a11y(&s2);
        let (_, copy_node) = t2
            .nodes
            .iter()
            .find(|(nid, _)| *nid == s2.a11y_node_id(copy_slot))
            .unwrap();
        assert!(
            !copy_node.supports_action(Action::Click),
            "disabled Copy has no Click"
        );
        assert!(
            copy_node.supports_action(Action::Focus),
            "disabled Copy is still focusable"
        );
    }

    #[cfg(a11y_tree)]
    #[test]
    fn native_rows_have_the_same_a11y_title_scope_shortcut_and_enabled_state() {
        use aterm_a11y::model::Action;

        let mut s = PaletteState::new().with_native_commands(NativeCommandScope {
            window: crate::WindowId(3),
            instance: crate::tab_model::AppInstanceId::from_stored(5),
            view: crate::tab_model::ViewId::from_stored(8),
            generation: 13,
            section: "Settings App".to_string(),
            commands: vec![
                Command {
                    id: ActionId::new("settings/search"),
                    title: "Settings: Search".to_string(),
                    shortcut: Some("Cmd-F".to_string()),
                    enabled: true,
                },
                Command {
                    id: ActionId::new("settings/undo"),
                    title: "Settings: Undo Last Change".to_string(),
                    shortcut: Some("Cmd-Z".to_string()),
                    enabled: false,
                },
            ],
        });
        for c in "settings".chars() {
            s.push_char(c);
        }
        let update = palette_a11y(&s);
        let (_, search) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == s.a11y_node_id(0))
            .expect("first native command");
        assert_eq!(search.label(), Some("Settings: Search"));
        // What a screen reader speaks: off macOS it omits the macOS-only chord.
        assert_eq!(
            search.description(),
            Some(if cfg!(target_os = "macos") {
                "Settings App \u{b7} Cmd-F"
            } else {
                "Settings App"
            })
        );
        assert!(search.supports_action(Action::Click));

        let undo_slot = s
            .filtered()
            .iter()
            .position(|&index| s.rows[index].label == "Settings: Undo Last Change")
            .expect("disabled native command remains discoverable");
        let (_, undo) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == s.a11y_node_id(undo_slot))
            .expect("disabled native command node");
        assert!(!undo.supports_action(Action::Click));
        assert!(undo.supports_action(Action::Focus));
    }

    /// A screen-reader Click that arrives after a native tab switch fails closed instead
    /// of selecting the same slot in the replacement app.
    #[cfg(a11y_tree)]
    #[test]
    fn stale_native_a11y_row_cannot_redirect_after_scope_replacement() {
        let scope = |view, generation, action: &'static str| NativeCommandScope {
            window: crate::WindowId(3),
            instance: crate::tab_model::AppInstanceId::from_stored(5),
            view: crate::tab_model::ViewId::from_stored(view),
            generation,
            section: "Native App".to_string(),
            commands: vec![Command {
                id: ActionId::new(action),
                title: "First command".to_string(),
                shortcut: None,
                enabled: true,
            }],
        };
        let mut state = PaletteState::new().with_native_commands(scope(8, 13, "first/run"));
        let stale = state.a11y_node_id(0);
        assert_eq!(state.a11y_filtered_index(stale), Some(0));

        state.replace_native_commands(Some(scope(9, 14, "second/run")));

        assert_ne!(state.a11y_node_id(0), stale);
        assert_eq!(state.a11y_filtered_index(stale), None);
    }
}
