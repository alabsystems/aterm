// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The own-rendered, introspectable MENU command-palette overlay: a frosted [`DrawPrim`]
//! card built from the platform-neutral [`crate::menu::MENU_MODEL`], so the application
//! menu joins Settings + About inside the WYSIWYG introspection pipeline and works on EVERY
//! platform — including Linux, which has no native menu bar at all ([`crate::menu`] stubs
//! `install` there).
//!
//! Each command flattens to a [`PaletteRow`] (section + label + typed target + visual
//! accelerator). Menu rows resolve `enabled` / `checked` from live `App` state exactly as
//! the native `validateMenuItem:` path would. The active native tab app contributes its
//! own command metadata in a clearly scoped section; those rows retain the exact window,
//! instance, view, lifecycle generation, and reducer action that produced them. A
//! type-to-filter `query` narrows the combined list (fuzzy subsequence over `label` +
//! `section`). Menu activation still converges on `Wake::MenuAction`; native activation is
//! generation checked before it can become `AppEvent::Action`.
//!
//! Mirrors the [`crate::about`] overlay's pure-model + tray-painter shape and reuses the
//! settings [`Roles`] colour system.

use std::borrow::Cow;
use std::time::Instant;

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

/// Chrome rows framing the scrolling command band: a centred title + the pinned query row
/// on top (2), and the key-hint footer on the bottom (1).
const CHROME_ROWS: usize = 3;

/// The session-connection command rows (design §2.3) — palette-only ids with
/// NO menu-bar item (the bar mirror stays exactly the bar; `MENU_MODEL`
/// completeness proofs pin them OUT of the model). Listed here so the palette
/// and the `invoke` verb (which resolves names through
/// [`PaletteState::action_by_name`]) reach them; the picker/sheet resolve the
/// peer parameter after dispatch.
const CONNECTION_ROWS: &[(&str, MenuAction)] = &[
    ("Connect to Session…", MenuAction::ConnectToSession),
    ("Configure Connection…", MenuAction::ConfigureConnection),
    ("Disconnect Session…", MenuAction::DisconnectSession),
    ("Show Connection Map", MenuAction::ShowConnectionMap),
];

/// Maximum logical width of the floating command card. Wide windows should leave enough
/// of the owning app visible to read as a modal layer, not stretch a short command label
/// across an entire desktop-sized surface.
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
fn palette_layout(state: &PaletteState, g: &SettingsGeom) -> PaletteLayout {
    let tray_w = (g.cols as f32 * g.cw).max(0.0);
    let tray_h = (g.panel_rows as f32 * g.ch).max(0.0);
    let desired_margin = (g.cw * 2.0).max(16.0);
    let max_margin = (tray_w * 0.5 - g.cw.max(0.0)).max(0.0);
    let margin = desired_margin.min(max_margin);
    let card_w = (tray_w - margin * 2.0).clamp(0.0, MAX_CARD_WIDTH);
    let card_rows = state.wanted_rows().min(g.panel_rows);
    let card_h = (card_rows as f32 * g.ch).clamp(0.0, tray_h);
    let card_x = ((tray_w - card_w) * 0.5).max(0.0);
    let card_y = ((tray_h - card_h) * 0.5).max(0.0);
    PaletteLayout {
        card: (card_x, card_y, card_w, card_h),
        card_rows,
        body_rows: state.body().min(card_rows.saturating_sub(CHROME_ROWS)),
    }
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

/// The two command domains share pixels, filtering, controls, and accessibility while
/// keeping their dispatch authorities distinct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaletteTarget {
    Menu(MenuAction),
    Native(NativeCommandTarget),
}

impl PaletteTarget {
    fn menu(&self) -> Option<MenuAction> {
        match self {
            Self::Menu(action) => Some(*action),
            Self::Native(_) => None,
        }
    }

    fn control_name(&self) -> Cow<'_, str> {
        match self {
            Self::Menu(action) => Cow::Owned(format!("{action:?}")),
            Self::Native(target) => Cow::Borrowed(target.action.as_str()),
        }
    }
}

// Retain the compact `row.action == MenuAction::…` idiom used by the menu-model tests.
impl PartialEq<MenuAction> for PaletteTarget {
    fn eq(&self, other: &MenuAction) -> bool {
        self.menu() == Some(*other)
    }
}

/// One flattened command — a single row of the palette. Menu rows come from a
/// `MenuEntry::Item`; native rows come from the active app's [`Command`] metadata.
/// Disabled commands remain visible but cannot produce a target.
#[derive(Clone, Debug)]
pub(crate) struct PaletteRow {
    /// The owning top-level menu title ("File", "View", …) — shown as a dim section chip.
    pub section: Cow<'static, str>,
    /// The command label, exactly as the native menu item reads. `Cow` because ONE row is
    /// dynamic: the Version section's ApplyUpdate row carries the LIVE staged/realized
    /// version ("↑ Update to v0.26 — restart now"), rewritten by [`PaletteState::resolve`];
    /// every other row keeps its static model label.
    pub label: Cow<'static, str>,
    /// The command this row dispatches. Menu and native authority never alias.
    pub action: PaletteTarget,
    /// The VISUAL key-equivalent character ("" for none) — right-aligned as an accelerator.
    pub key: &'static str,
    /// The accelerator's modifier mask (rendered ⌘ / ⇧⌘ / ⌃⌘ before `key`).
    pub mods: MenuMods,
    /// Native-app shortcut metadata. Menu rows leave this empty and derive their visual
    /// accelerator from `mods` + `key`, preserving menu-bar parity.
    pub shortcut: Cow<'static, str>,
    /// Whether the command is currently actionable; a disabled row paints dim and Enter
    /// on it is a no-op (mirroring `validateMenuItem:` greying).
    pub enabled: bool,
    /// A checkbox state for toggle commands (Settings, full-screen); `None`
    /// for plain commands. `Some(true)` paints a check glyph.
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

/// Live `App` predicates that resolve per-row `enabled`/`checked` — the SAME conditions the
/// native `validateMenuItem:` path uses. Computed by `App::palette_live` and fed to
/// [`PaletteState::resolve`], so `palette.rs` stays pure + unit-testable (no `App`).
#[derive(Clone, Debug, Default)]
pub(crate) struct PaletteLive {
    /// A text selection exists (gates Copy).
    pub has_selection: bool,
    /// A native Settings tab is open (checkmark on its singleton toggle).
    pub settings_open: bool,
    /// The FRONT session's effective matrix-rain state (its runtime override,
    /// else the config `enabled` bit) — the checkmark on Matrix Rain. False
    /// when no terminal is frontmost (the row is disabled there anyway).
    pub rain_on: bool,
    /// The front window's PROMOTABLE kitty (its tenured program cat on glass,
    /// else the launch kitty) is the currently pinned favourite — the
    /// checkmark on Favourite This Kitty. Never needs a front terminal (with
    /// no window at all it asks about the launch kitty).
    pub kitty_favourited: bool,
    /// Process-wide serious mode is active (checkmark on Serious Mode).
    pub serious_mode: bool,
    /// The window is full-screen (checkmark on Enter Full Screen).
    pub fullscreen: bool,
    /// The front window has more than one tab (gates Next/Previous Tab + Close Tab).
    pub multi_tab: bool,
    /// The active whole tab is a native app rather than a terminal pane tree. Terminal-only
    /// split/session-window commands remain discoverable but are honestly disabled.
    pub native_tab_active: bool,
    /// The frontmost window's front content IS a terminal session. Strictly
    /// stronger than `!native_tab_active`: also false when NO window is
    /// frontmost (macOS keeps running windowless), so per-session rows refuse
    /// instead of silently no-op'ing through the invoke fence.
    pub terminal_front: bool,
    /// Whether the front window can PRESENT a rename editor. Off macOS the
    /// editor is drawn by the tab strip, so `tab_strip_rows = 0` leaves nowhere
    /// to put it and the row must grey out rather than accept a dead click.
    pub can_rename: bool,
    /// At least one native tab snapshot is available for identity-fresh reopen.
    pub can_reopen_closed_tab: bool,
    /// A non-expired split-leaf recovery record is available.
    pub can_reopen_closed_view: bool,
    /// This platform has a real local-file picker. Picker-backed rows remain visible but
    /// disabled where `menu::choose_local_file` cannot produce a path.
    pub local_file_picker_available: bool,
    /// A strictly-newer `(build, version)` is STAGED (the `App.relaunch` nudge): the
    /// Version section shows the one-click "↑ Update to v<staged> — restart now" row.
    pub staged: Option<(u64, String)>,
    /// The post-update REALIZED arrow is live: `(new version, its spawn instant)` — the
    /// Version section shows the TIME-FADED "↑ Updated to v<new>" row (alpha decays over
    /// [`crate::relaunch_notice::REALIZED_ARROW_TTL`]). Ignored while `staged` is `Some`
    /// (a newer staged build supersedes the celebration).
    pub realized: Option<(String, Instant)>,
    /// `MotionPolicy::Reduced` is in force: the realized row's fade FREEZES at full
    /// alpha (a decaying element is itself motion; Reduced pins every amplitude).
    pub reduced_motion: bool,
    /// Accelerator HINTS for menu rows, as `(action, display chord)` pairs — the
    /// EFFECTIVE `[keybindings]` chord for each menu action that has one
    /// (platform seeds with the user's rebinds/unbinds applied, resolved by
    /// `Keybindings::display_chord_for`). Populated by `App::palette_live` OFF
    /// macOS only: macOS menu rows keep their native ⌘ key-equivalents, and off
    /// macOS these hints replace the "" that [`accel`] honestly blanks ⌘ chords
    /// to — so the palette (and the AccessKit description built from the same
    /// string) shows the chord that actually works here. Empty = no hints (the
    /// pure-test default).
    pub menu_accels: Vec<(MenuAction, String)>,
}

impl PaletteState {
    /// Build the palette from [`MENU_MODEL`], every command enabled and unchecked (the pure,
    /// `App`-free default the unit tests use), then the session-connection
    /// command rows ([`CONNECTION_ROWS`]) — ids that live in NO menu bar
    /// (design §2.3: "all palette/`invoke`-reachable"), appended BESIDE the
    /// model so the bar mirror stays exactly the bar. `App::palette_enter`
    /// calls [`Self::resolve`] right after to fold in live enabled/checked
    /// state.
    pub(crate) fn new() -> Self {
        let mut rows = Vec::new();
        for section in MENU_MODEL {
            for entry in section.entries {
                if let MenuEntry::Item {
                    label,
                    action,
                    key,
                    mods,
                } = entry
                {
                    rows.push(PaletteRow {
                        section: Cow::Borrowed(section.title),
                        label: Cow::Borrowed(label),
                        action: PaletteTarget::Menu(*action),
                        key,
                        mods: *mods,
                        shortcut: Cow::Borrowed(""),
                        enabled: true,
                        checked: None,
                    });
                }
            }
        }
        for (label, action) in CONNECTION_ROWS {
            rows.push(PaletteRow {
                section: Cow::Borrowed("Connections"),
                label: Cow::Borrowed(label),
                action: PaletteTarget::Menu(*action),
                key: "",
                mods: MenuMods::None,
                shortcut: Cow::Borrowed(""),
                enabled: true,
                checked: None,
            });
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
        }
    }

    /// Prepend the active native app's command metadata. Native rows deliberately sit above
    /// global menu rows so the scoped commands are visible without scrolling, while both
    /// domains retain the same filter/selection/painting/control/a11y observers.
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

    /// Replace only app-scoped rows, preserving the global `MENU_MODEL`, live query, and
    /// menu resolution. Active-tab changes use this to remove native commands on terminal
    /// tabs or retarget a different native app without rebuilding menu state.
    pub(crate) fn replace_native_commands(&mut self, scope: Option<NativeCommandScope>) {
        self.pointer_over = None;
        self.pointer_armed = None;
        self.rows
            .retain(|row| matches!(&row.action, PaletteTarget::Menu(_)));
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
    /// toggle commands (Settings, full-screen) get a checkmark; selection- and
    /// tab-gated commands are disabled when their precondition is absent. Everything else
    /// stays enabled (the honest "no predicate ⇒ always available").
    ///
    /// The Version section's ApplyUpdate row is additionally DYNAMIC — the one place the
    /// palette diverges from the static model, mirroring the live macOS Version menu
    /// (`menu::update_version_menu`):
    ///   * STAGED: "↑ Update to v<staged> — restart now" — ONE Enter applies (the
    ///     owner's "click-upgrade" ask);
    ///   * REALIZED (fresh post-update): the TIME-FADED "↑ Updated to v<new>" arrow
    ///     (activating it takes ApplyUpdate's nothing-staged fallback: the details
    ///     overlay — informative, never a blind restart);
    ///   * NEITHER: the row is REMOVED (a dead "update" command would be noise).
    ///
    /// Idempotent: called at every open AND on live transitions (`palette_refresh_live`),
    /// so it must insert/rewrite/remove from any prior state.
    pub(crate) fn resolve(&mut self, live: &PaletteLive) {
        // Metadata changed under the gesture: a later release must not inherit authority
        // from the row set painted before this resolve.
        self.pointer_over = None;
        self.pointer_armed = None;
        self.realized_since = None;
        self.realized_frozen = live.reduced_motion;
        // Same label law as the menu row (`menu::staged_apply_label`): a staged build
        // may share the running build's display version, so fall back to the build
        // number rather than offer to update to the version already on screen. Plain
        // `↑`, not the colour emoji — this is own-rendered text with no emoji face.
        let dynamic_label: Option<Cow<'static, str>> = if let Some((build, v)) = &live.staged {
            Some(Cow::Owned(crate::menu::staged_apply_label(
                "\u{2191}", *build, v,
            )))
        } else if let Some((v, since)) = &live.realized {
            self.realized_since = Some(*since);
            Some(Cow::Owned(format!("\u{2191} Updated to v{v}")))
        } else {
            None
        };
        let pos = self
            .rows
            .iter()
            .position(|r| r.action == MenuAction::ApplyUpdate);
        match (dynamic_label, pos) {
            (Some(label), Some(i)) => self.rows[i].label = label,
            (Some(label), None) => {
                // Re-insert at the HEAD of the Version section (just before its About
                // row) — the slot `PaletteState::new` gave it before a prior resolve
                // removed it.
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
            // Accelerator hint (off macOS): carry the EFFECTIVE binding-table
            // chord for this menu action in the native-`shortcut` slot, so
            // `row_accel` — and the AccessKit description folded from the same
            // string — renders the chord that actually works here instead of
            // the "" that [`accel`] correctly blanks ⌘ equivalents to. The hint
            // survives `platform_accel` untouched (it is not a macOS spelling).
            // Idempotent like the rest of resolve: re-resolve rewrites or
            // clears it, and an empty `menu_accels` (macOS, pure tests) leaves
            // every menu row exactly as `PaletteState::new` built it.
            row.shortcut = match live.menu_accels.iter().find(|(a, _)| *a == action) {
                Some((_, chord)) => Cow::Owned(chord.clone()),
                None => Cow::Borrowed(""),
            };
            match action {
                MenuAction::ToggleSettings => row.checked = Some(live.settings_open),
                MenuAction::ToggleFullScreen => row.checked = Some(live.fullscreen),
                MenuAction::ToggleSeriousMode => row.checked = Some(live.serious_mode),
                // Per-session state: the checkmark mirrors the FRONT session's
                // effective rain; a native whole tab — or no window at all —
                // has no session to toggle, so the row honestly disables (and
                // the invoke fence refuses) instead of silently no-op'ing.
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
                // The pin is SESSION metadata, so a native whole tab has nothing
                // to rename: the row disables (and the invoke fence refuses)
                // instead of opening an editor over a surface with no session.
                MenuAction::RenameSession => {
                    row.enabled = live.terminal_front && live.can_rename;
                }
                // "Check for Updates…" drives the IN-APP updater, which exists
                // only on macOS (`aterm_update::enabled()` is cfg-gated there).
                // Off macOS the row used to sit enabled and silently no-op —
                // the check never starts and no staged build can exist — so it
                // greys out (and the socket `invoke` refuses by name) instead,
                // the same honesty rule as RenameSession/ToggleMatrixRain
                // above. NOT gated on live updater state: on macOS the action
                // always at least opens the Software Update route, so the row
                // stays unconditionally enabled there (byte-identical
                // behaviour). The Settings route itself remains reachable off
                // macOS via Settings… — only the dead update VERB is refused.
                MenuAction::SoftwareUpdate => {
                    row.enabled = cfg!(target_os = "macos");
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
                // The connected-spawn presets take the FOCUSED session as
                // their origin (design §2.3) — no front terminal, no origin,
                // so the rows honestly disable (and the invoke path refuses)
                // instead of silently no-op'ing. The picker/configure/
                // disconnect ids act FROM the focused session too; the map is
                // instance-wide and stays enabled.
                MenuAction::NewControlledWindow
                | MenuAction::NewControlledTab
                | MenuAction::NewControllerWindow
                | MenuAction::NewControllerTab
                | MenuAction::ConnectToSession
                | MenuAction::ConfigureConnection
                | MenuAction::DisconnectSession => {
                    row.enabled = live.terminal_front;
                }
                _ => {}
            }
        }
        // A live re-resolve can shrink the row set (the update row was removed) while a
        // filter/cursor is active — re-clamp so painter and cursor never diverge.
        self.clamp_scroll();
    }

    /// The paint alpha for `row` at `now`: `1.0` for every ordinary row; the REALIZED
    /// "↑ Updated to v<new>" row fades per `relaunch_notice::realized_alpha` — computed
    /// at PAINT time (not snapshotted) so an OPEN palette steps down as the fingerprint's
    /// elapsed bucket advances. Frozen at full under `MotionPolicy::Reduced`.
    pub(crate) fn row_alpha(&self, row: &PaletteRow, now: Instant) -> f32 {
        if row.action != MenuAction::ApplyUpdate {
            return 1.0;
        }
        let Some(since) = self.realized_since else {
            return 1.0; // the staged "restart now" row never fades
        };
        if self.realized_frozen {
            return 1.0;
        }
        crate::relaunch_notice::realized_alpha(now.duration_since(since))
    }

    /// Append a character to the filter and reset the cursor to the top of the (renarrowed)
    /// list. Non-printing controls are ignored by the caller before this point.
    pub(crate) fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
        self.scroll = 0;
        self.pointer_over = None;
        self.pointer_armed = None;
    }

    /// Delete the last filter character (Backspace), resetting the cursor to the top.
    pub(crate) fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
        self.scroll = 0;
        self.pointer_over = None;
        self.pointer_armed = None;
    }

    /// The indices (into `rows`) that pass the current fuzzy filter, in menu order. An empty
    /// query matches everything; otherwise every query char must appear, in order, somewhere
    /// in the lowercased `"label  section"` haystack (a classic subsequence fuzzy match, which
    /// also covers plain substrings).
    ///
    /// PERF: the haystack is streamed and lowercased lazily rather than built with `format!`.
    /// This is called a linear number of times per pointer event (hit testing walks slots, and
    /// each layout derivation asks for `wanted_rows` + `body`), and the palette carries ~40–60
    /// rows, so the two `String`s per row this used to allocate dominated pointer motion while
    /// the modal was open. The query is still lowercased once per call, not once per row.
    pub(crate) fn filtered(&self) -> Vec<usize> {
        let q = self.query.to_ascii_lowercase();
        (0..self.rows.len())
            .filter(|&i| {
                let r = &self.rows[i];
                let hay = r
                    .label
                    .chars()
                    .chain("  ".chars())
                    .chain(r.section.chars())
                    .map(|c| c.to_ascii_lowercase());
                fuzzy_subsequence(&q, hay)
            })
            .collect()
    }

    /// The visible command band height: the filtered count, capped at [`MAX_CMD_ROWS`]. The
    /// single value the painter, the scroll clamp, and [`Self::move_selection`] agree on.
    fn body(&self) -> usize {
        self.filtered().len().min(MAX_CMD_ROWS)
    }

    /// Move the cursor by `delta` over the FILTERED set (wrapping), keeping it on-screen.
    pub(crate) fn move_selection(&mut self, delta: isize) {
        self.pointer_over = None;
        self.pointer_armed = None;
        let n = self.filtered().len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        self.clamp_scroll();
    }

    /// Scroll the visible filtered band without wrapping. The selected row keeps its
    /// relative slot inside the viewport, so the invariant
    /// `scroll <= selected < scroll + body` remains true at both clamps. A wheel gesture
    /// cancels any armed click; the app immediately re-hit-tests the stationary pointer
    /// against the newly painted rows.
    pub(crate) fn scroll_by(&mut self, delta: isize) -> bool {
        let n = self.filtered().len();
        let body = self.body();
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
            self.clamp_scroll();
        }
        before
            != (
                self.selected,
                self.scroll,
                self.pointer_over.clone(),
                self.pointer_armed.clone(),
            )
    }

    /// Move the cursor to a specific FILTERED-set index (an OS accessibility Focus/Click on a
    /// row lands here), clamped into range and kept on-screen. A no-op when nothing matches.
    #[cfg_attr(not(a11y_tree), allow(dead_code))]
    pub(crate) fn select(&mut self, idx: usize) {
        self.pointer_over = None;
        self.pointer_armed = None;
        self.select_filtered_index(idx);
    }

    fn select_filtered_index(&mut self, idx: usize) {
        let n = self.filtered().len();
        if n == 0 {
            return;
        }
        self.selected = idx.min(n - 1);
        self.clamp_scroll();
    }

    /// Hover a filtered row, moving the visible selection wash with the pointer. `None`
    /// means the modal still owns the pointer but it is outside a painted command row.
    pub(crate) fn pointer_hover(&mut self, idx: Option<usize>) -> bool {
        let target = idx.and_then(|idx| {
            let visible = self.filtered();
            visible.get(idx).map(|&row| self.rows[row].action.clone())
        });
        let before = (self.selected, self.scroll, self.pointer_over.clone());
        if let Some(idx) = idx {
            self.select_filtered_index(idx);
        }
        self.pointer_over = target;
        before != (self.selected, self.scroll, self.pointer_over.clone())
    }

    /// Arm the exact hovered target on left press. Disabled rows still receive honest
    /// selection feedback; [`Self::pointer_release`] keeps them inert.
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
            let visible = self.filtered();
            visible.get(idx).is_some_and(|&row| self.rows[row].enabled)
        });
        let activate = enabled && armed.is_some() && armed == self.pointer_over;
        (changed, activate)
    }

    pub(crate) fn pointer_over_row(&self) -> bool {
        self.pointer_over.is_some()
    }

    /// Keep `selected` within `[scroll, scroll + body)` and `scroll` within bounds — the
    /// same discipline the Settings body uses, so painter and cursor never diverge.
    fn clamp_scroll(&mut self) {
        let n = self.filtered().len();
        let body = self.body();
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
        let vis = self.filtered();
        let &idx = vis.get(self.selected)?;
        let row = &self.rows[idx];
        row.enabled.then(|| row.action.clone())
    }

    /// Menu-only compatibility projection used by the menu control gateway and standing
    /// menu-model tests. Native commands are available through [`Self::selected_target`]
    /// and can never be mistaken for a `MenuAction`.
    #[cfg(test)]
    pub(crate) fn selected_action(&self) -> Option<MenuAction> {
        self.selected_target()?.menu()
    }

    /// The overlay height (rows) the card wants: the title + query chrome, the (capped)
    /// command band, and the hint footer. Never `0` (at least one command band row) so the
    /// splice always has a card to paint.
    pub(crate) fn wanted_rows(&self) -> usize {
        CHROME_ROWS + self.filtered().len().clamp(1, MAX_CMD_ROWS)
    }

    /// `(scroll, total, visible)` for `controls front`: rows scrolled past, the full row
    /// count, and the command rows shown (capped at `MAX_CMD_ROWS`).
    pub(crate) fn scroll_extent(&self) -> (usize, usize, usize) {
        (self.scroll, self.rows.len(), self.body())
    }

    /// Machine-readable lines for the `controls menu` introspection verb — the SAME rows the
    /// card paints (section / label / action / accelerator / enabled / checked), so screen ==
    /// introspection.
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        let vis = self.filtered();
        let mut out = Vec::with_capacity(vis.len() + 2);
        out.push(format!(
            "menu rows={} shown={} selected={} query={:?}",
            self.rows.len(),
            vis.len(),
            self.selected,
            self.query,
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

    /// A fingerprint of everything the card paints (query + cursor + scroll + every row's
    /// label/enabled/checked/action), folded into the frame's `RepaintKey` so opening /
    /// typing / moving forces exactly one present. Never `0` while open (`0` is the closed
    /// sentinel). While the REALIZED arrow row is live (and unfrozen), the ~30s elapsed
    /// bucket is folded too, so an OPEN palette re-presents once per fade step (the
    /// `about_to_wait` sweep arms the matching wake) — the notice.rs quantized-fp pattern.
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.query.hash(&mut h);
        self.selected.hash(&mut h);
        self.scroll.hash(&mut h);
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

/// The command palette's accessibility tree — the FIFTH observer of the SAME [`PaletteState`]
/// the pixels ([`palette_tray`]) and the `controls menu` verb ([`PaletteState::controls_lines`])
/// read. A window root parents a [`accesskit::Role::ListBox`] whose children are the FILTERED
/// rows (one [`accesskit::Role::MenuItem`] each: label = command label, description = section,
/// `toggled` for checkable rows), so a screen reader sees exactly the rows on screen. An
/// ENABLED row carries [`accesskit::Action::Click`] (activate) + [`accesskit::Action::Focus`]
/// (move the cursor); a DISABLED row omits `Click` — Enter on a greyed command is inert, the
/// same rule [`PaletteState::selected_action`] enforces. Focus follows `selected`.
///
/// Id contract (matched by the Palette branch of `App::on_accessibility_action`): the window
/// root is `NodeId(0)`; each row id combines a hash of the filtered target set with its slot.
/// A request queued against an old native view/generation therefore cannot address a new
/// row after a tab switch. The ListBox container lives at a disjoint sentinel id.
#[cfg(a11y_tree)]
pub(crate) fn palette_a11y(state: &PaletteState) -> accesskit::TreeUpdate {
    use accesskit::{Action, Node, NodeId, Role, Toggled, Tree, TreeId, TreeUpdate};

    // High + disjoint from the `row_index + 1` control ids (mirrors settings' `GROUP_BASE`).
    const LIST: NodeId = NodeId(u64::MAX);
    let root_id = NodeId(0);
    let vis = state.filtered();
    // Derive the epoch ONCE. It is slot-independent by construction ("cursor/focus movement
    // deliberately does not enter this hash") and `state` is immutable here, so every
    // per-row `a11y_node_id` call was recomputing the identical value — and each derivation
    // itself walks the whole filtered set. That made the publish O(rows²) with a Debug-format
    // allocation per inner step, on a path driven by every palette keystroke and hover.
    let epoch = state.a11y_epoch();

    let mut nodes: Vec<(NodeId, Node)> = Vec::with_capacity(vis.len() + 2);
    let mut items: Vec<NodeId> = Vec::with_capacity(vis.len());
    for (slot, &i) in vis.iter().enumerate() {
        let row = &state.rows[i];
        let id = a11y_node_id_for(epoch, slot);
        let mut node = Node::new(Role::MenuItem);
        node.set_label(row.label.as_ref());
        // Through the platform filter (audit I9): this string is what a screen
        // reader SPEAKS, so it is the one place a macOS-only "Cmd-F" does actual
        // harm — Narrator reading out a chord the keyboard does not have.
        //
        // Deliberately `platform_accel(&row.shortcut)` and NOT the full
        // `row_accel`: the latter would ALSO fold a menu row's ⌘-glyph
        // key-equivalent into the description, which is a macOS-side change to
        // what VoiceOver says about every menu row. This lane is about removing
        // a lie off macOS, not about adding speech on it.
        let accel = platform_accel(&row.shortcut);
        let description = if accel.is_empty() {
            row.section.to_string()
        } else {
            format!("{} \u{b7} {accel}", row.section)
        };
        node.set_description(description);
        if let Some(on) = row.checked {
            node.set_toggled(Toggled::from(on));
        }
        node.add_action(Action::Focus);
        if row.enabled {
            node.add_action(Action::Click);
        }
        nodes.push((id, node));
        items.push(id);
    }

    let mut list = Node::new(Role::ListBox);
    list.set_children(items);
    nodes.push((LIST, list));

    let mut root = Node::new(Role::Window);
    root.set_label("Commands");
    root.set_children(vec![LIST]);
    nodes.push((root_id, root));

    // Focus the selected row's node (clamped); the root when nothing matches the filter.
    let focus = if vis.is_empty() {
        root_id
    } else {
        a11y_node_id_for(epoch, state.selected.min(vis.len() - 1))
    };

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(root_id)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

/// Mint a palette row node id from an ALREADY-derived epoch. The single-shot decode path
/// ([`PaletteState::a11y_filtered_index`]) keeps using the `&self` method; only bulk minting,
/// where the epoch is loop-invariant, goes through here.
#[cfg(a11y_tree)]
fn a11y_node_id_for(epoch: u32, slot: usize) -> accesskit::NodeId {
    accesskit::NodeId((u64::from(epoch) << 32) | (slot as u64 + 1))
}

/// Transient per-window state for the command palette (mirrors `AboutState`'s `Option`
/// slot). While a window holds `Some(PaletteState)`, keystrokes drive it: printable chars
/// filter, Up/Down move the cursor, Enter dispatches the command, Esc closes.
pub(crate) struct PaletteState {
    /// Every flattened command from [`MENU_MODEL`], in menu order (the Version section's
    /// ApplyUpdate row is dynamic — rewritten/removed by [`PaletteState::resolve`]).
    rows: Vec<PaletteRow>,
    /// The type-to-filter query (fuzzy subsequence over label + section).
    query: String,
    /// The cursor position WITHIN the filtered set (not `rows`).
    selected: usize,
    /// First filtered row shown in the command band (scroll offset).
    scroll: usize,
    /// Exact row currently under the pointer. Keyboard/accessibility selection does not
    /// imply pointer ownership, so this remains distinct from `selected`.
    pointer_over: Option<PaletteTarget>,
    /// Exact target armed by a left press. It prevents release after a filter/tab change
    /// from activating a different command that happens to occupy the same visual slot.
    pointer_armed: Option<PaletteTarget>,
    /// When the post-update REALIZED arrow row spawned (`Some` only while `resolve` saw
    /// a live realized state and no staged build) — drives [`Self::row_alpha`]'s fade and
    /// the fingerprint's elapsed bucket.
    realized_since: Option<Instant>,
    /// `MotionPolicy::Reduced` snapshot: freeze the realized fade at full alpha.
    realized_frozen: bool,
}

impl PaletteState {
    /// Resolve an action BY NAME — the `action={:?}` token [`Self::controls_lines`]
    /// prints (e.g. `NewTab`) — for the socket `invoke` verb. Refusal names its reason:
    /// a row the live [`PaletteState::resolve`] predicates DISABLED errs as disabled
    /// (never a silent no-op — the same `validateMenuItem:` conditions the native bar
    /// enforces), an unmatched name errs as unknown. An enabled row wins over a
    /// disabled duplicate (the Version section may render an action in two states).
    pub(crate) fn action_by_name(&self, name: &str) -> Result<MenuAction, String> {
        // LEGACY SPELLING: `FavouriteSessionKitty` was the wire name until the
        // launch-kitty ruling (2026-08-17) retired the session kitty; scripts
        // that still say it reach the renamed action.
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

    /// AccessKit row identity epoch. Cursor/focus movement deliberately does not enter this
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
            }
            row.enabled.hash(&mut hash);
        }
        let epoch = hash.finish() as u32;
        if epoch == u32::MAX {
            u32::MAX - 1
        } else {
            epoch
        }
    }

    #[cfg(a11y_tree)]
    fn a11y_node_id(&self, slot: usize) -> accesskit::NodeId {
        a11y_node_id_for(self.a11y_epoch(), slot)
    }

    /// Decode only a node minted by the current filtered target epoch. An old native
    /// generation or tab scope cannot redirect a delayed screen-reader Click.
    #[cfg(a11y_tree)]
    pub(crate) fn a11y_filtered_index(&self, node: accesskit::NodeId) -> Option<usize> {
        let slot = usize::try_from(node.0 & u64::from(u32::MAX))
            .ok()?
            .checked_sub(1)?;
        (self.a11y_node_id(slot) == node && slot < self.filtered().len()).then_some(slot)
    }
}

/// Format a visual accelerator, e.g. `⌘K` / `⇧⌘N` / `⌃⌘F`; empty when the command has no
/// key-equivalent. Purely cosmetic — the real keystroke is handled by `App::on_key` /
/// the native bar; here it just labels the row like a menu.
fn accel(mods: MenuMods, key: &str) -> String {
    // Off macOS these ⌘ key-equivalents mirror a native menu bar that does not
    // exist, AND the real chord differs (Copy is Ctrl+Shift+C, Settings is
    // Ctrl+Shift+S — not ⌘,), so painting a ⌘ glyph, or a naive ⌘→Ctrl rewrite,
    // would MISLEAD a Linux user who has no ⌘ key. Blank here; the TRUE chord
    // arrives instead through `PaletteLive::menu_accels` (resolved from the
    // effective [keybindings] table by `App::palette_live`), which `resolve`
    // writes into the row's `shortcut` slot — so `row_accel` never consults
    // this "" for a menu action that has a real local chord. On macOS the
    // palette mirrors the menu bar verbatim.
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

/// Drop the macOS-only key-equivalents from a native app's `shortcut` string, off
/// macOS. `·`-separated alternatives are kept individually, so "Cmd-S · C-x C-s"
/// becomes "C-x C-s" on Windows/Linux — the half that is actually true there.
///
/// AUDIT I9. `row_accel` used to pass `row.shortcut` through VERBATIM, which walked
/// straight past the platform-aware blanking [`accel`] applies to menu rows a few
/// lines above — so the palette over Settings advertised "Cmd-F"/"Cmd-Z" on
/// Windows, and (worse, because it is not merely cosmetic) the string is folded
/// into the AccessKit `description`, so Narrator READ "Cmd-F" ALOUD to a user with
/// no Cmd key. `accel`'s own comment already ratified the rule this restores: off
/// macOS a ⌘ glyph "would MISLEAD".
///
/// REJECTED — rewriting `Cmd-` to `Ctrl-` mechanically. It would be a LIE at
/// exactly the rows that matter: Settings ▸ Search is `Ctrl+Shift+F` off macOS
/// (NOT Ctrl+F), Reader ▸ Copy is `Ctrl+Shift+C`, and the editor's `Cmd-S` has no
/// Ctrl twin at all outside Windows. A mechanical rewrite trades an obviously
/// foreign chord for a plausible WRONG one, which is the worse failure: the user
/// tries it and something else happens. (Some rows DO translate — `settings/undo`
/// really is Ctrl+Z off macOS — which is precisely why the translation belongs to
/// each `commands()` site, where the dispatch is known, and not to a blanket
/// string rewrite here.)
///
/// Where a platform-true chord DOES exist, the honest answer is to say so at the
/// SOURCE (`commands()` emits the right string per platform) — this function is the
/// backstop that keeps a future macOS-only chord from leaking again. What counts
/// as macOS-only is [`accel_is_mac_only`], which is deliberately spelling-agnostic:
/// every literal in the tree happens to use the ASCII `Cmd-` form today, so a
/// prefix test would have full coverage RIGHT NOW and silently lose it the first
/// time someone writes `⌘S` or `Cmd+S`.
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

/// Whether ONE accelerator alternative names a chord that exists only on macOS,
/// in any of the spellings this tree might grow.
///
/// PURE and cfg-free, so both answers are testable in one build — `platform_accel`
/// itself is `cfg!`-selected, so a table driven only through it asserts one of its
/// two columns per platform and the other is never executed anywhere.
///
/// - ⌘ / `Cmd` / `Command` is macOS-only outright: there is no Command key here.
/// - ⌥ / `Opt` / `Option` names a key that DOES exist off macOS — as Alt — but
///   the ⌥/Option SPELLING is macOS notation, and this function's whole rule is
///   to say nothing rather than show a foreign chord (the string is also folded
///   into the AccessKit description a screen reader speaks aloud). A part that
///   genuinely means the Windows/Linux key is spelled `Alt-`/`M-` and survives.
fn accel_is_mac_only(part: &str) -> bool {
    const MAC_ONLY_WORDS: &[&str] = &["Cmd", "Command", "Opt", "Option"];
    if part.contains('\u{2318}') || part.contains('\u{2325}') {
        return true;
    }
    // A leading modifier word, in either separator spelling. Checked on the WHOLE
    // part (not just its head) so `Shift-Cmd-S` is caught too.
    part.split(['-', '+'])
        .any(|segment| MAC_ONLY_WORDS.contains(&segment))
}

/// One accelerator projection for every observer. Native rows carry their command
/// metadata through the platform-aware [`platform_accel`] filter; menu rows retain
/// the platform-aware visual key-equivalent policy of [`accel`]. Both observers —
/// the painted card AND the AccessKit description a screen reader speaks — go
/// through here, so what is SHOWN and what is SAID cannot drift apart.
fn row_accel(row: &PaletteRow) -> String {
    if row.shortcut.is_empty() {
        accel(row.mods, row.key)
    } else {
        platform_accel(&row.shortcut)
    }
}

/// Exact painted selection/hit rectangle for one VISIBLE command slot, deriving the layout
/// itself. The tray painter and the pointer hit-test both reach the same geometry through
/// `palette_row_rect_in` against the layout they already hold, so this layout-deriving entry
/// is only what the tests measure with — it lets them assert painter and hit-test against ONE
/// rectangle, which is what keeps resize, zoom, and native DPI conversion from drifting the
/// interactive row away from its visible wash and ring. Test build only: no painter or
/// hit-test path calls it, so shipping it would be unreachable code.
#[cfg(test)]
pub(crate) fn palette_row_rect(
    state: &PaletteState,
    g: &SettingsGeom,
    slot: usize,
) -> Option<(f32, f32, f32, f32)> {
    palette_row_rect_in(&palette_layout(state, g), g, slot)
}

/// `palette_row_rect` against an ALREADY-derived layout. `palette_layout` costs two
/// `PaletteState::filtered()` derivations (`wanted_rows` + `body`), and each of those
/// allocates two `String`s per palette row — so re-deriving it once per slot inside a hit
/// test made a single pointer motion do ~30x the filtering work of the frame that painted
/// those same rows. The layout is a pure function of `(state, g)` and neither moves during
/// the scan, so hoisting it is a plain common-subexpression elimination.
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
    let visible = state.filtered();
    let layout = palette_layout(state, g);
    for slot in 0..layout.body_rows {
        let (rx, ry, rw, rh) = palette_row_rect_in(&layout, g, slot)?;
        if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
            let filtered = state.scroll + slot;
            return visible.get(filtered).map(|_| filtered);
        }
    }
    None
}

/// True iff every char of `needle` (already lowercased) appears in `haystack` (already
/// lowercased) in order — a subsequence match. An empty needle always matches.
///
/// The haystack is an ITERATOR, not a `&str`, so [`PaletteState::filtered`] can lowercase
/// `"label  section"` lazily instead of `format!`-ing and lowercasing a fresh `String` for
/// every row on every call. The body only ever consumed `haystack.chars()` anyway.
/// `pub(crate)`: the session picker ([`crate::session_picker`]) filters with the SAME
/// match, so the two type-to-filter surfaces can never rank/miss differently.
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
pub(crate) fn palette_tray(state: &PaletteState, g: &SettingsGeom, theme: Theme) -> TrayInput {
    let r = Roles::from_theme(theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let layout = palette_layout(state, g);
    let (card_x, card_y, card_w, card_h) = layout.card;
    let radius = (ch * 0.6).min(14.0);
    let mut prims: Vec<DrawPrim> = vec![
        // A small two-step shadow separates the modal from the still-visible owner app.
        DrawPrim::Panel {
            x: card_x - 3.0,
            y: card_y + 2.0,
            w: card_w + 6.0,
            h: card_h + 6.0,
            radius: radius + 3.0,
            fill: rgba([0, 0, 0], 0x2A),
            blur: false,
        },
        DrawPrim::Panel {
            x: card_x - 1.0,
            y: card_y + 2.0,
            w: card_w + 2.0,
            h: card_h + 3.0,
            radius: radius + 1.0,
            fill: rgba([0, 0, 0], 0x30),
            blur: false,
        },
        // Opaque: the shared tray rasterizer does not implement backdrop blur, so
        // translucency here would make the owning app's text ghost through the commands.
        DrawPrim::Panel {
            x: card_x,
            y: card_y,
            w: card_w,
            h: card_h,
            radius,
            fill: rgba(r.surface, 0xFF),
            blur: false,
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
    ];

    // A run at an EXPLICIT baseline (mixed-size runs on one visual row share the
    // baseline of the row's dominant size — the shared-baseline rule). The command
    // list is the terminal (Mono) face; a Ui-face row would opt in via `text_prim`.
    // Takes the full RGBA so the realized-arrow row can fade its runs (every other
    // caller passes `rgba(color, 0xFF)`).
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

    // Title (centred) — "Commands".
    {
        let title = "Commands";
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
    let vis = state.filtered();
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
    let count = format!("{}/{}", vis.len(), state.rows.len());
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
    // Sampled ONCE per paint for the realized-arrow fade (`row_alpha`); the fingerprint's
    // elapsed bucket is what schedules the re-presents, so per-paint sampling is exact.
    let now = Instant::now();
    for slot in 0..body {
        let Some(&idx) = vis.get(state.scroll + slot) else {
            break;
        };
        let row = &state.rows[idx];
        // Whole-row alpha: 1.0 everywhere except the fading realized-arrow row.
        let ra = state.row_alpha(row, now);
        let fade = |base: u8| -> u8 { (f32::from(base) * ra) as u8 };
        let y0 = card_y + (2 + slot) as f32 * ch;
        let is_sel = state.scroll + slot == state.selected;
        if is_sel {
            // The same `layout` the card above was placed from — re-deriving it here would
            // re-run the whole filter twice (see `palette_row_rect_in`).
            let (x, y, width, height) = palette_row_rect_in(&layout, g, slot)
                .expect("painted palette slot has a row rectangle");
            prims.push(DrawPrim::Panel {
                x,
                y,
                w: width,
                h: height,
                radius: ch * 0.3,
                fill: rgba(r.accent, 0x22),
                blur: false,
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
        // The row mixes THREE sizes (Body label, Caption chip + accelerator);
        // they all share the baseline of the dominant Body run — mixed-size
        // runs on one visual row never sit on different baselines.
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
        // Label — dim when disabled; the realized-arrow row rides its fade alpha.
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
        // Section chip (dim) right after the label, on the SAME baseline.
        let lw = text_w(&row.label, body.get());
        text_at(
            &mut prims,
            lx + lw + cw * 1.0,
            row_base,
            cap,
            row.section.to_string(),
            rgba(r.text_tertiary, fade(0xFF)),
        );
        // Accelerator — right-aligned, on the SAME baseline.
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

    // Key-hint footer on the last row.
    let hint = "\u{2191}\u{2193} move   \u{23ce} run   type to filter   esc close";
    let fsize = TypeStep::Caption.px(px);
    let hint_w = text_w(hint, fsize.get());
    let fx = card_x + fit((card_w - hint_w) * 0.5, cw, card_w - hint_w - cw);
    text(
        &mut prims,
        fx,
        card_y + layout.card_rows.saturating_sub(1) as f32 * ch,
        fsize,
        hint.to_string(),
        rgba(r.text_tertiary, 0xFF),
    );
    prims.push(DrawPrim::ClipPop);

    TrayInput {
        prims,
        card: layout.card,
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
        // Same count as the model's Item entries PLUS the palette-only
        // session-connection rows (design §2.3 — ids with no bar item).
        let model_items = MENU_MODEL
            .iter()
            .flat_map(|sec| sec.entries.iter())
            .filter(|e| matches!(e, MenuEntry::Item { .. }))
            .count();
        assert_eq!(s.rows.len(), model_items + CONNECTION_ROWS.len());
    }

    /// The §2.3 connection ids have palette rows (design: "all palette/
    /// `invoke`-reachable") in their own section, resolvable BY NAME for the
    /// `invoke` verb; the session-subject ones gate on a front terminal while
    /// the instance-wide map stays enabled.
    #[test]
    fn connection_ids_have_palette_rows_and_gate_on_a_front_terminal() {
        let mut s = PaletteState::new();
        for (label, action) in CONNECTION_ROWS {
            let row = s
                .rows
                .iter()
                .find(|r| r.action == *action)
                .unwrap_or_else(|| panic!("{action:?} has a palette row"));
            assert_eq!(row.section, "Connections");
            assert_eq!(row.label, *label);
            // Resolvable by its invoke name (the `invoke` verb's path).
            assert_eq!(s.action_by_name(&format!("{action:?}")), Ok(*action));
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

    /// AUDIT I9 — the accelerator projection, both platforms in one table.
    ///
    /// macOS keeps every string verbatim (it mirrors the menu bar there).
    /// Elsewhere the `Cmd-` alternatives are dropped and the platform-true ones
    /// survive, so a `·`-separated pair collapses to its portable half rather
    /// than to nothing, and a lone `Cmd-*` collapses to the empty string — the
    /// same "say nothing rather than mislead" rule `accel` already applied to
    /// menu rows.
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

    /// Keyboard audit #6 — menu-row accelerator HINTS. `resolve` writes the
    /// effective binding-table chord (supplied via `PaletteLive::menu_accels`)
    /// into the menu row's `shortcut` slot, so `row_accel` — the ONE projection
    /// both the painted card and the AccessKit description read — shows the
    /// chord that actually works here. An action with no hint keeps its
    /// platform default, and a re-resolve WITHOUT hints clears a previously
    /// applied one (resolve stays idempotent per-live-state).
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
        // Hints are per-resolve live state: resolving without them CLEARS.
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

    /// The backstop's PREDICATE, tested on every platform — `platform_accel` is
    /// `cfg!`-selected, so a table driven only through it executes one of its two
    /// columns per build and the other is proven nowhere.
    ///
    /// Spelling-agnostic on purpose: every literal in the tree uses the ASCII
    /// `Cmd-` form today, so a bare `starts_with("Cmd-")` had complete coverage
    /// right up until the first `⌘S` or `Cmd+S`, and would then have leaked a
    /// foreign chord into the painted card AND into the AccessKit description a
    /// screen reader speaks aloud.
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
        // …and everything that IS reachable off macOS survives, including the
        // Alt/Meta spellings that mean the very same physical key as ⌥.
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
        // The row's METADATA is preserved verbatim (that is this test's subject);
        // its PROJECTION is platform-aware — a lone macOS key-equivalent leaves
        // nothing behind off macOS (audit I9).
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
        s.backspace();
        assert!(s.filtered().len() >= narrowed.len());
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
    fn resolve_sets_checkmarks() {
        let mut s = PaletteState::new();
        s.resolve(&PaletteLive {
            settings_open: false,
            ..Default::default()
        });
        let settings = s
            .rows
            .iter()
            .find(|r| r.action == MenuAction::ToggleSettings)
            .unwrap();
        assert_eq!(settings.checked, Some(false));
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

    /// The View ▸ Matrix Rain row mirrors the FRONT session's effective rain
    /// state as its checkmark and — like the split commands — honestly
    /// disables over a native whole tab (no session to toggle there).
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

        // The windowless-app state (macOS keeps running with every window
        // closed): NOT a native tab, but no terminal either — the row must
        // disable so the invoke fence refuses instead of silently no-op'ing.
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

    /// The View ▸ Favourite This Kitty row reports whether the LAUNCH kitty
    /// is the current pin, and — unlike the rain toggle — stays ENABLED with
    /// no front terminal: the launch kitty is process-wide (owner ruling,
    /// 2026-08-17), so a native whole tab and the windowless-app state can
    /// still pin it.
    #[test]
    fn favourite_row_checks_when_pinned_and_stays_enabled_without_a_terminal() {
        let mut s = PaletteState::new();
        s.resolve(&PaletteLive {
            kitty_favourited: true,
            terminal_front: true,
            can_rename: true,
            ..Default::default()
        });
        // (checked, enabled) for the row — PaletteRow is not `Copy`, and the
        // two bits are the whole contract this test pins.
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

        // The windowless-app state (macOS keeps running with every window
        // closed): not a native tab, but no terminal either.
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

    /// The same two rows against the LIVE platform predicate rather than a hand-
    /// written bool — the wiring `App::palette_live` actually feeds in.
    ///
    /// This is where the Windows gap showed: the predicate was literally
    /// `cfg!(target_os = "macos")`, so the rows greyed out on a platform where
    /// BOTH document runtimes already work (the socket drives them today) purely
    /// because no picker was linked. With `menu::choose_local_file` answering
    /// through `IFileOpenDialog`, a picker-bearing platform must report live rows.
    #[test]
    fn the_file_rows_track_the_live_platform_predicate() {
        let available = crate::menu::local_file_picker_available();
        let mut state = PaletteState::new();
        state.resolve(&PaletteLive {
            local_file_picker_available: available,
            ..PaletteLive::default()
        });
        for action in [MenuAction::OpenMarkdown, MenuAction::OpenEditor] {
            let row = state.rows.iter().find(|row| row.action == action).unwrap();
            assert_eq!(
                row.enabled, available,
                "{action:?} must be exactly as enabled as the picker is real"
            );
            #[cfg(any(target_os = "macos", windows))]
            assert!(row.enabled, "{action:?} on a platform that HAS a picker");
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

    #[test]
    fn controls_lines_serialize_rows() {
        let s = PaletteState::new();
        let lines = s.controls_lines();
        assert!(lines[0].starts_with("menu rows="));
        assert!(lines.iter().any(|l| l.contains("action=Copy")));
        assert!(lines.iter().any(|l| l.contains("action=Help")));
    }

    /// STAGED (complaint 2, "click-upgrade"): resolve adds the one-click
    /// "↑ Update to v<staged> — restart now" row at the HEAD of the Version section,
    /// enabled, full-alpha, and activatable straight to `ApplyUpdate`.
    #[test]
    fn resolve_staged_adds_one_click_update_row() {
        let model = aterm_spec::derive::native_update_menu_activation_model();
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
        assert_eq!(row.label, "\u{2191} Update to v9.9 \u{2014} restart now");
        assert!(row.enabled, "one click must work");
        // It sits directly BEFORE the About row (head of the Version section).
        assert_eq!(s.rows[i + 1].action, MenuAction::Version);
        // The staged row never fades (it is not the realized arrow).
        assert_eq!(s.row_alpha(&s.rows[i], Instant::now()), 1.0);
        // Filtering to it and pressing Enter dispatches ApplyUpdate.
        for c in "restart now".chars() {
            s.push_char(c);
        }
        assert_eq!(s.selected_action(), Some(MenuAction::ApplyUpdate));
        assert!(model.fire("DecodeApplyTag", &mut model_state));
        assert!(model.fire("DispatchApply", &mut model_state));
        assert_eq!(model_state["apply_dispatched"], 1);
        // And `controls menu` (screen == introspection) lists it.
        assert!(
            s.controls_lines()
                .iter()
                .any(|l| l.contains("action=ApplyUpdate") && l.contains("Update to v9.9")),
            "controls menu shows the staged row"
        );
    }

    /// NEITHER staged nor realized: the ApplyUpdate row is REMOVED — a dead "update"
    /// command would be noise (and the native Version menu shows only About then too).
    /// Idempotent: a second resolve with a staged build re-inserts it.
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

    /// REALIZED (complaint 3): the post-update arrow row appears with a TIME-FADED
    /// alpha — full at boot, decayed mid-TTL, zero at/after TTL — and freezes at full
    /// under reduced motion. A staged build SUPERSEDES the celebration row.
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
        assert_eq!(row.label, "\u{2191} Updated to v9.9");
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
        assert!(row.label.contains("Update to v10.0"), "{:?}", row.label);
    }

    /// The realized fade re-presents an OPEN palette: the fingerprint folds the ~30s
    /// elapsed bucket while unexpired (different buckets ⇒ different fps), and FREEZES
    /// under reduced motion (same fp — no repaint churn for a pinned-alpha row).
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
        // Frozen: the bucket is NOT folded, so elapsed time does not churn the fp.
        let frozen0 = mk(now, true);
        let frozen1 = mk(now - REALIZED_BUCKET - Duration::from_secs(1), true);
        assert_eq!(frozen0, frozen1, "reduced motion pins the fp");
    }

    #[test]
    fn palette_tray_paints_card_and_title() {
        let s = PaletteState::new();
        let g = SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 160,
            panel_rows: s.wanted_rows() + 10,
        };
        let t = palette_tray(&s, &g, Theme::default());
        let layout = palette_layout(&s, &g);
        assert_eq!(t.card, layout.card);
        let (card_x, card_y, card_w, card_h) = t.card;
        assert!(
            t.prims.iter().any(|primitive| matches!(
                primitive,
                DrawPrim::Panel {
                    x,
                    y,
                    w,
                    h,
                    fill,
                    blur: false,
                    ..
                } if *x == card_x
                    && *y == card_y
                    && *w == card_w
                    && *h == card_h
                    && fill[3] == 0xFF
            )),
            "the returned card rect has an opaque body panel"
        );
        assert!(
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == "Commands")),
            "the title is painted"
        );
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

    /// ANTI-DIVERGENCE: the a11y MenuItem set equals `filtered()` (the SAME rows the card
    /// paints and `controls menu` reports as `shown=`), focus follows `selected`, and a
    /// DISABLED row omits `Click` (matching `selected_action`'s inert-greyed rule).
    #[cfg(a11y_tree)]
    #[test]
    fn palette_a11y_lists_filtered_rows() {
        use accesskit::{Action, Role};

        let mut s = PaletteState::new();
        for c in "tab".chars() {
            s.push_char(c);
        }
        let vis = s.filtered();
        assert!(vis.len() >= 2, "expected several tab commands");
        let update = palette_a11y(&s);

        // One MenuItem per filtered row, at the current target epoch + slot, labelled like
        // the row.
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
        // The a11y item count agrees with the `controls menu` shown= count (screen==introspection).
        let header = s
            .controls_lines()
            .into_iter()
            .find(|l| l.starts_with("menu rows="))
            .unwrap();
        assert!(header.contains(&format!("shown={}", vis.len())));

        // A DISABLED row (Copy with no selection) is focusable but not clickable.
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

    /// NEGATIVE CONTROL (non-vacuity): typing a filter that narrows the list removes the
    /// corresponding MenuItem nodes — the tree reflects the live model, not a static list.
    #[cfg(a11y_tree)]
    #[test]
    fn palette_a11y_tree_tracks_the_filter() {
        use accesskit::Role;
        let count = |s: &PaletteState| {
            palette_a11y(s)
                .nodes
                .iter()
                .filter(|(_, n)| n.role() == Role::MenuItem)
                .count()
        };
        let mut s = PaletteState::new();
        let all = count(&s);
        for c in "settings".chars() {
            s.push_char(c);
        }
        let narrowed = count(&s);
        assert!(narrowed < all, "filtering drops MenuItem nodes");
        assert_eq!(narrowed, s.filtered().len(), "and matches the filtered set");
    }

    #[cfg(a11y_tree)]
    #[test]
    fn native_rows_have_the_same_a11y_title_scope_shortcut_and_enabled_state() {
        use accesskit::Action;

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
        // AUDIT I9 — this is the string a SCREEN READER speaks. Off macOS the
        // macOS-only key-equivalent must not be in it: Narrator saying "Cmd-F"
        // to a user with no Cmd key was the defect.
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

    /// A screen-reader Click may arrive after the user switched native tabs. Its node id
    /// must fail closed instead of selecting the same visual slot in the replacement app.
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

    /// Gated visual preview (`ATERM_PALETTE_PREVIEW=path`) → PNG at 2×-Retina-ish metrics.
    #[test]
    fn preview_palette_overlay() {
        let Ok(path) = std::env::var("ATERM_PALETTE_PREVIEW") else {
            return;
        };
        let mut s = PaletteState::new();
        s.resolve(&PaletteLive {
            has_selection: true,
            multi_tab: true,
            ..Default::default()
        });
        let (cw, ch, px) = (16.0_f32, 34.0_f32, 26.0_f32);
        let cols = 56usize;
        let panel_rows = s.wanted_rows() + 8;
        let g = SettingsGeom {
            cw,
            ch,
            font_px: px,
            cols,
            panel_rows,
        };
        let tray = palette_tray(&s, &g, Theme::default());
        let (buf, pw, ph) = crate::tray_raster::rasterize_tray(
            &tray.prims,
            (cols as f32 * cw) as u32,
            (panel_rows as f32 * ch) as u32,
            1.0,
            [22, 24, 30, 255],
        );
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, pw, ph);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&buf).unwrap();
        }
        std::fs::write(&path, &out).unwrap();
    }
}
