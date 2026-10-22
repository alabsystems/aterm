// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The native macOS application menu bar (`NSApp.mainMenu`).
//!
//! Installed once, with the first OS window (`App::attach_os_window`), so a headless process
//! builds none. An item is a dispatch stub: it shows its key equivalent and, when chosen, posts
//! [`Wake::MenuAction`](crate::Wake) with a [`MenuAction`], marked so keys typed behind its key
//! equivalent wait for its dispatch (`tab_spawn::post_native_menu_action`), which
//! `App::dispatch_menu_action` turns into the same `App` command the keybinding calls. The action
//! rides in the item's `NSMenuItem.tag`, so one selector (`menuAction:`) serves every item. AppKit
//! holds the target weakly, so [`install`] returns it retained for `App` to keep.
//!
//! Everything imperative is `#[cfg(target_os = "macos")]`. On other targets the
//! [`MenuAction`] enum and a no-op [`install`] still exist so the workspace builds
//! everywhere and `Wake::MenuAction { action, mark }` is a valid variant on every target.

// macOS-only menu bar: off macOS `install` is a no-op stub, and the helpers
// only the AppKit menu reads are gated to macOS (and the tests).

/// Pure arbitration behind AppKit's synchronous `applicationShouldTerminate:`
/// callback. AppKit is answered immediately, while the real quit decision is
/// deferred onto the event loop where `App` owns document durability. A stable
/// generation makes delayed/duplicate callbacks harmless. Every answer names
/// the generation it belongs to — the new one, the one still pending, or the
/// one whose quit is already under way — so a system quit is noted against the
/// quit it will actually ride ([`crate::system_quit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) enum NativeTerminateDecision {
    Dispatch(u64),
    DeferExisting(u64),
    AllowExit(u64),
}

#[cfg(any(target_os = "macos", test))]
impl NativeTerminateDecision {
    fn generation(self) -> u64 {
        match self {
            Self::Dispatch(generation)
            | Self::DeferExisting(generation)
            | Self::AllowExit(generation) => generation,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct NativeTerminateArbiter {
    /// Minted by [`Self::request`], which only the macOS `terminate:` relay calls.
    #[cfg(any(target_os = "macos", test))]
    next_generation: u64,
    pending: Option<u64>,
    /// The generation whose quit became irreversible ([`Self::complete`]).
    exiting: Option<u64>,
}

impl NativeTerminateArbiter {
    const fn new() -> Self {
        Self {
            #[cfg(any(target_os = "macos", test))]
            next_generation: 1,
            pending: None,
            exiting: None,
        }
    }

    #[cfg(any(target_os = "macos", test))]
    fn request(&mut self) -> NativeTerminateDecision {
        if let Some(generation) = self.exiting {
            return NativeTerminateDecision::AllowExit(generation);
        }
        if let Some(generation) = self.pending {
            return NativeTerminateDecision::DeferExisting(generation);
        }
        let generation = self.next_generation.max(1);
        self.next_generation = generation.wrapping_add(1).max(1);
        self.pending = Some(generation);
        NativeTerminateDecision::Dispatch(generation)
    }

    #[cfg(any(unix, test))]
    fn is_current(self, generation: u64) -> bool {
        self.exiting.is_none() && self.pending == Some(generation)
    }

    #[cfg(any(unix, test))]
    fn cancel(&mut self, generation: u64) -> bool {
        if self.pending != Some(generation) || self.exiting.is_some() {
            return false;
        }
        self.pending = None;
        true
    }

    fn cancel_current(&mut self) -> bool {
        if self.exiting.is_some() {
            return false;
        }
        self.pending.take().is_some()
    }

    fn complete(&mut self, generation: u64) -> bool {
        if self.pending != Some(generation) || self.exiting.is_some() {
            return false;
        }
        self.pending = None;
        self.exiting = Some(generation);
        true
    }

    fn complete_current(&mut self) -> bool {
        let Some(generation) = self.pending else {
            return false;
        };
        self.complete(generation)
    }
}

static NATIVE_TERMINATE: std::sync::Mutex<NativeTerminateArbiter> =
    std::sync::Mutex::new(NativeTerminateArbiter::new());

fn with_native_terminate<R>(f: impl FnOnce(&mut NativeTerminateArbiter) -> R) -> R {
    let mut state = NATIVE_TERMINATE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    f(&mut state)
}

#[cfg(unix)]
pub(crate) fn native_termination_is_current(generation: u64) -> bool {
    with_native_terminate(|state| state.is_current(generation))
}

/// A `terminate:` from AppKit, admitted: the arbiter's answer, and — when the
/// quit Apple Event names a logout, a restart or a shutdown (`systems`) — the
/// system's mark on the generation that answer belongs to. Both under the one
/// arbiter lock, so no cancel can fall between them and leave a mark behind.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn admit_native_terminate(systems: bool) -> NativeTerminateDecision {
    with_native_terminate(|state| {
        let decision = state.request();
        if systems {
            crate::system_quit::note(decision.generation());
        }
        decision
    })
}

/// Cancel the quit of `generation` if it is the one pending. A cancelled quit is no longer the
/// system's either ([`crate::system_quit::forget`]): every cancel runs through here or
/// [`cancel_current_native_termination`], so a logout aterm answered Cancel to never marks the
/// person's next ⌘Q.
#[cfg(unix)]
pub(crate) fn cancel_native_termination(generation: u64) -> bool {
    with_native_terminate(|state| {
        let cancelled = state.cancel(generation);
        if cancelled {
            crate::system_quit::forget(generation);
        }
        cancelled
    })
}

/// Cancel whichever quit is pending; see [`cancel_native_termination`].
pub(crate) fn cancel_current_native_termination() -> bool {
    with_native_terminate(|state| {
        let pending = state.pending;
        let cancelled = state.cancel_current();
        if cancelled && let Some(generation) = pending {
            crate::system_quit::forget(generation);
        }
        cancelled
    })
}

/// Serialises the tests that drive the process-wide arbiter and the system-quit mark, and hands
/// each a fresh arbiter. No test reaches the paths that complete a quit (they need an
/// `ActiveEventLoop`), so the reset discards nothing real.
#[cfg(test)]
pub(crate) fn terminate_test_serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    with_native_terminate(|state| *state = NativeTerminateArbiter::new());
    crate::system_quit::clear_for_test();
    guard
}

#[cfg(unix)]
pub(crate) fn complete_native_termination(generation: u64) -> bool {
    with_native_terminate(|state| state.complete(generation))
}

pub(crate) fn complete_current_native_termination() -> bool {
    with_native_terminate(NativeTerminateArbiter::complete_current)
}

/// One menu command, independent of AppKit. Its [`MenuAction::tag`] is the `NSMenuItem.tag`,
/// decoded by [`MenuAction::from_tag`]; `App::dispatch_menu_action` calls the matching command.
/// Standard responder items (minimise, zoom, full screen, hide, quit) route here too, not via
/// `nil`-target selectors, so the whole menu has one dispatch path into `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuAction {
    // App menu
    /// About aterm — open the About route in the native Settings tab.
    About,
    /// Check for Updates… — open the Software Update route in the native Settings tab and start
    /// a fresh check. The Version menu's "Update details…" and the [`MenuAction::ApplyUpdate`]
    /// fallback open the same route.
    SoftwareUpdate,
    /// Version — the top-level menu whose title shows `v<version>`, with a trailing ⬆️ while
    /// an update is staged ([`update_version_menu`], [`staged_for_version_menu`]). Its About
    /// item opens the native About route. Off macOS, Windows' tab-strip version label fires it
    /// too ([`version_slot_click`]).
    Version,
    /// One-click update (`App::apply_update_or_details`), fired by the Version menu's install
    /// item, the palette's Version row, the "Update ready" pill and the off-macOS tab-strip ↻.
    /// A strictly newer staged build applies at once (`Wake::ApplyStagedUpdate`); one on hold or
    /// nothing staged opens the Software Update route instead.
    ApplyUpdate,
    /// Open aterm.toml — the canonical config in Settings ▸ Manual's assisted editor, on every
    /// platform.
    Preferences,
    /// Quit aterm.
    Quit,
    // File menu
    /// New Window — a new in-process window of this App, sharing its sessions
    /// (`App::open_window_off_loop`); no second process is spawned.
    NewWindow,
    /// New Tab — a new in-window session (`App::open_tab`).
    NewTab,
    /// Choose one local UTF-8 file and open it in a read-only Markdown tab.
    OpenMarkdown,
    /// Choose one local UTF-8 file and open it in the native editor.
    OpenEditor,
    /// Save the active editor document through its existing file grant.
    Save,
    /// Reopen the most recently closed whole tab with fresh runtime identities.
    ReopenClosedTab,
    /// Reinsert the most recently closed split leaf at its original topology path.
    ReopenClosedView,
    /// Move Tab to New Window — pull the active tab out into a fresh in-process
    /// window (`App::detach_active_tab`).
    MoveTabToNewWindow,
    /// Move Tab to Next Window — move the active tab into the next existing window
    /// (wrapping; `App::migrate_active_tab_to_next_window`).
    MoveTabToNextWindow,
    /// Open Session in New Window — show the active session in a second window
    /// (same live grid in two windows; `App::open_active_session_in_new_window`).
    ViewSessionInNewWindow,
    /// Close Tab — close the focused pane; a tab's last pane closes the tab
    /// (`App::close_active_tab`). The row reads Close Pane on a split tab
    /// ([`close_row_title`]).
    CloseTab,
    // Edit menu
    /// Copy the selection (`App::copy_selection`).
    Copy,
    /// Paste the clipboard (`App::paste_clipboard`).
    Paste,
    /// Select All (`App::select_all`).
    SelectAll,
    /// Find… — enter Cmd-F find mode (`App::search_enter`).
    Find,
    /// Find Next — resume/step to the next search match.
    FindNext,
    /// Find Previous — resume/step to the previous match.
    FindPrev,
    // View menu
    /// Toggle the window's full-screen state (winit `set_fullscreen`).
    ToggleFullScreen,
    /// Increase font size (`set_font_size(font_size + step)`).
    FontIncrease,
    /// Decrease font size (`set_font_size(font_size - step)`).
    FontDecrease,
    /// Actual Size — reset to the default font size (`set_font_size(default)`).
    FontActualSize,
    /// Split the focused pane left/right (`split_focused_pane(Vertical)`).
    SplitVertical,
    /// Split the focused pane top/bottom (`split_focused_pane(Horizontal)`).
    SplitHorizontal,
    /// Toggle matrix rain for the front session of the frontmost window
    /// (`App::toggle_matrix_rain`): a per-session override of `[matrix_rain]`, either way,
    /// until the session ends. Terminal-only ([`requires_terminal_tab`]). The palette
    /// checkmark mirrors the front session's effective state.
    ToggleMatrixRain,
    /// Promote the front window's promotable kitty — the tenured program cat
    /// on its glass, else the launch kitty (the base cat generated at launch,
    /// per computer) — into the durable kitty
    /// registry and pin it as the cursor companion (`App::favourite_kitty_checked`).
    /// Since a pinned favourite outranks both program cats
    /// and the launch kitty, this is also how a cat is kept across
    /// launches and programs. Process-wide (never terminal-only: the launch
    /// kitty exists without a front session). The palette row's checkmark
    /// reports whether that promotable kitty is the current pin. One-way: the
    /// pin is transferable, not toggleable. Wire id 46 is kept.
    FavouriteKitty,
    /// Wear the next cat in the collection, one per press, wrapping at the end.
    /// [`Self::FavouriteKitty`] pins the cat that would ride anyway, so it never puts on a
    /// different one. `kitty wear <key>` on the control socket is the addressed form.
    NextKitty,
    /// Toggle the process-wide serious-mode policy. While enabled it suppresses
    /// every audible and decorative effect without overwriting the underlying
    /// preferences; disabling it restores those requested settings exactly.
    ToggleSeriousMode,
    /// Focus or create the process-singleton Settings tab. The app-menu Settings…
    /// item — ⌘, — uses the standard macOS settings chord.
    ToggleSettings,
    /// View ▸ Show Needs in Windows — checkable: the window's needs
    /// border and its pill, on or off together. Persisted as `[presence] rim`
    /// through the one serialized config lane
    /// (`App::user_toggle_needs_in_windows`). `TogglePresenceRim` (the same key) is its alias.
    ToggleNeedsInWindows,
    /// View ▸ Stats Bars: Deck — one of three radio rows: the three bars
    /// at full size under the titlebar, the default. Persisted as
    /// `[sys] stats_bars = "deck"` through the one serialized config lane
    /// (`App::user_set_stats_bars`); a change into or out of the Deck
    /// re-grids every window once.
    StatsBarsDeck,
    /// View ▸ Stats Bars: Sill — the compact form, three light lanes in the
    /// window's bottom pad that take no terminal row (`"sill"`).
    StatsBarsSill,
    /// View ▸ Stats Bars: Off — no bars (`"off"`).
    StatsBarsOff,
    /// View ▸ Deck Size: Automatic — one of three radio rows
    /// (`docs/DESIGN-stats-deck-v2-2026-10-20.md` §2.2, §11.5): each window's
    /// Deck is Full or Folded by its height, the default. Persisted as
    /// `[sys] stats_deck_size = "auto"` through the one serialized config
    /// lane (`App::user_set_deck_size`); every window re-decides its form,
    /// and one whose Deck changes height re-grids once.
    DeckSizeAuto,
    /// View ▸ Deck Size: Full — every window's Deck Full (`"full"`).
    DeckSizeFull,
    /// View ▸ Deck Size: Folded — every window's Deck Folded (`"folded"`).
    DeckSizeFolded,
    /// View ▸ Stats Lens (⌘I): open the keyboard lens, the bars' close-up,
    /// on the front window, or close the open one (`App::toggle_stats_lens`).
    ToggleStatsLens,
    /// Open Settings at the Packages route (the batteries-included toolchain
    /// surface: install/update the ALab toolset, posture, consent switches).
    /// The app-menu Packages… item beneath Settings… — the menu-bar path to the
    /// same page the seed notice pill points at.
    Packages,
    /// Open Settings at the Messages route:
    /// everything aterm told the person, newest first; the page the band's `Details ›` opens.
    Messages,
    /// Toggle the command palette overlay (`App::toggle_palette`).
    OpenPalette,
    /// View ▸ Choose Theme… — open the command palette scoped to its Theme
    /// section (`App::open_theme_palette`): the arrow keys preview each theme
    /// live, Enter writes `theme` to aterm.toml, Esc puts the configured theme
    /// back. Bindable as `choose_theme`.
    ChooseTheme,
    /// View ▸ Browse & Customize Themes… and the palette Theme section's last
    /// row: the one Settings tab, at Settings ▸ Themes — the Theme Showcase,
    /// where every theme can be tried on a live stage and its fonts and
    /// colors tuned. Instance-level: no session needed.
    BrowseThemes,
    /// View ▸ Choose Tab Theme… — the palette's Theme section scoped to the
    /// front session (`App::open_tab_theme_palette`): the arrow keys draw each
    /// theme on this tab only, Enter keeps it as the tab's own theme
    /// (`theme session set`), Esc puts the tab's previous theme back. Nothing
    /// is written to aterm.toml. Terminal-only; no default binding.
    ChooseTabTheme,
    /// View ▸ Clear Tab Theme — the front session draws the instance theme
    /// again (`theme session clear`). Terminal-only.
    ClearTabTheme,
    // Tab-strip context menu. These two live only in the per-tab right-click
    // menu the native strip pops (`toolbar.rs` / `session_chrome::compose_tab_menu`),
    // not in the menu bar, so they are absent from `MENU_MODEL`. They ride the
    // same tag→action decode as every bar item.
    /// Copy the right-clicked tab's session id (the registry `sid`) to the
    /// clipboard — the handle an agent needs to address this session over the
    /// control socket (`meta`, `timeline`, `turn`, …).
    CopySessionId,
    /// Copy the right-clicked tab's shell-reported cwd to the clipboard, raw (never
    /// `~`-abbreviated: a pasted path must be real).
    CopyCwd,
    /// Overview… ⌃⌘O: every session of this aterm on one card — what
    /// each needs, else what it is doing, its task, who drives it, and who
    /// manages whom — `App::open_overview` (app_overview.rs) on the front
    /// window. `ShowConnectionMap`, `Fleet` and `ShowFleet` are aliases.
    Overview,
    // The Agents menu: what needs the person, everyone at a glance, stepping in.
    /// Next That Needs You ⇧⌘J: go to the most pressing local need, stuck
    /// before decide; pressed again, to the next (`App::next_need`, the cursor
    /// keyed by need so a need that resolved drops out).
    NextNeed,
    /// Harness Log for This Tab ⇧⌘L: the ledger
    /// key's action (`App::open_session_ledger`: `aterm drive ledger @<sid>
    /// --format html`, opened in the browser). `LedgerForSession` and
    /// `OpenLedger` are its aliases.
    HarnessLog,
    /// Take Over This Tab:
    /// the person's hold on the focused session (`fabric::person_hold`,
    /// `origin=person`, `App::take_over`) — only the person types there until
    /// they hand it back. Greyed while the person's hold already stands.
    /// [`InvokeAuthority::PersonOnly`]: no socket fires it. `HoldSession` is an alias.
    TakeOver,
    /// Hand Back This Tab: lift the person's hold, and only it
    /// (`App::hand_back`). Enabled only while it stands.
    /// [`InvokeAuthority::PersonOnly`]; `LiftHold` is an alias.
    HandBack,
    /// Read This Mac: `aterm ctl survey` as a child, its text in a document
    /// tab (`App::run_cli_to_tab`).
    ReadThisMac,
    /// Read the Fleet: `aterm ctl survey --fleet --timeout 3` into a
    /// document tab. Live only in a fleet (`summarize::in_fleet`).
    ReadFleet,
    /// Summarize the Fleet in a New Tab (Summarize This Mac in a New Tab with
    /// no fleet): one worker, raised, no role, `from=ctl`
    /// (`App::summarize_in_new_tab`). Grey with the reason when no worker can
    /// start here.
    SummarizeFleet,
    // Palette-only rows (`PALETTE_ONLY`): no bar item, a palette row and an
    // `invoke` name each.
    /// Message Bus: Status — `aterm fabric` (the bus's status screen) into a
    /// document tab. A bus that is off is an Info record, never an error.
    FabricStatus,
    /// Message Bus: Turn On… — `aterm fabric on`, behind a confirmation
    /// sheet. Owner only.
    FabricOn,
    /// Message Bus: Turn Off… — `aterm fabric off`, behind a confirmation
    /// sheet. Owner only.
    FabricOff,
    /// Fleet Machines — `aterm fleet peers` into a document tab.
    FleetMachines,
    // Window menu
    /// Edit the focused pane's session pin in place on the tab strip — the keyboard/menu twin
    /// of double-clicking a tab. "Rename Session", not "Rename Tab": it sets session metadata
    /// (`meta set title`) and a tab can hold several split sessions. Also in the tab context
    /// menu.
    RenameSession,
    /// Set Role… — the same inline editor over the active tab, editing the focused session's
    /// `meta role` (the presence band's first slot). Terminal-only.
    SetRole,
    /// Edit ▸ Reset Terminal (`crate::manual_reset`): return the front session's terminal modes,
    /// a torn escape sequence and an open OSC 8 link to host defaults, keeping the screen and
    /// scrollback. The menu twin of the `reset` verb. Terminal-only.
    ResetTerminal,
    /// View ▸ Coding Agent Harness…: the one Settings tab, at Settings ▸ Harness — the harness's
    /// switches, the default Autopilot, each session's Autopilot and what the
    /// harness did. The menu and palette twin of the Autopilot menu's
    /// `Harness settings…` (`App::open_harness_settings`). Instance-level: no session needed.
    HarnessSettings,
    /// Minimise the window.
    Minimize,
    /// Zoom (toggle maximised) the window.
    Zoom,
    /// Show the next tab (`App::cycle_tab(true)`).
    NextTab,
    /// Show the previous tab (`App::cycle_tab(false)`).
    PrevTab,
    // Help menu
    /// Help — open the bundled, offline features guide (`open_help_url`).
    Help,
}

impl MenuAction {
    /// The integer stored in the menu item's `tag`: stable, starting at 1 (0 is the
    /// `NSMenuItem` default, so an untagged item never looks like an action).
    #[must_use]
    pub(crate) fn tag(self) -> isize {
        match self {
            MenuAction::About => 1,
            MenuAction::Preferences => 2,
            // 3 is retired (was Hide aterm). A retired tag is never reused.
            MenuAction::Quit => 4,
            MenuAction::NewWindow => 5,
            MenuAction::NewTab => 6,
            MenuAction::CloseTab => 7,
            MenuAction::Copy => 8,
            MenuAction::Paste => 9,
            MenuAction::SelectAll => 10,
            MenuAction::Find => 11,
            MenuAction::ToggleFullScreen => 12,
            MenuAction::Minimize => 13,
            MenuAction::Zoom => 14,
            MenuAction::Help => 15,
            MenuAction::MoveTabToNewWindow => 16,
            MenuAction::ViewSessionInNewWindow => 17,
            MenuAction::MoveTabToNextWindow => 18,
            MenuAction::FindNext => 19,
            MenuAction::FindPrev => 20,
            MenuAction::FontIncrease => 21,
            MenuAction::FontDecrease => 22,
            MenuAction::FontActualSize => 23,
            MenuAction::SplitVertical => 24,
            MenuAction::SplitHorizontal => 25,
            MenuAction::NextTab => 26,
            MenuAction::PrevTab => 27,
            // 28, 29, 32 and 33 are retired (bottom-HUD commands).
            MenuAction::ToggleSettings => 30,
            MenuAction::OpenPalette => 31,
            // 34 is retired (a separate Check for Updates… alert).
            MenuAction::SoftwareUpdate => 35,
            MenuAction::Version => 36,
            MenuAction::ApplyUpdate => 37,
            MenuAction::ReopenClosedTab => 38,
            MenuAction::OpenMarkdown => 39,
            MenuAction::OpenEditor => 40,
            MenuAction::ReopenClosedView => 41,
            MenuAction::CopySessionId => 42,
            MenuAction::CopyCwd => 43,
            MenuAction::ToggleMatrixRain => 44,
            MenuAction::ToggleSeriousMode => 45,
            MenuAction::FavouriteKitty => 46,
            MenuAction::Packages => 47,
            MenuAction::RenameSession => 48,
            // 49-53, 55 and 56 are retired (connection presets and ids; their
            // invoke names answer `ERR retired`).
            MenuAction::Overview => 54,
            MenuAction::NextKitty => 57,
            // 58 is retired (was Fleet).
            // 59 is retired (was Inbox).
            MenuAction::HarnessLog => 60,
            MenuAction::TakeOver => 61,
            MenuAction::HandBack => 62,
            MenuAction::FabricStatus => 63,
            MenuAction::FabricOn => 64,
            MenuAction::FabricOff => 65,
            MenuAction::SetRole => 66,
            // 67 is retired (was Presence Band); 68 is Show Needs in Windows.
            MenuAction::ToggleNeedsInWindows => 68,
            MenuAction::Messages => 69,
            // 70-72 retired (named session identities).
            MenuAction::ResetTerminal => 73,
            MenuAction::HarnessSettings => 74,
            MenuAction::ChooseTheme => 75,
            MenuAction::BrowseThemes => 76,
            MenuAction::NextNeed => 79,
            MenuAction::ReadThisMac => 80,
            MenuAction::ReadFleet => 81,
            MenuAction::SummarizeFleet => 82,
            MenuAction::FleetMachines => 83,
            MenuAction::StatsBarsDeck => 84,
            MenuAction::StatsBarsSill => 85,
            MenuAction::StatsBarsOff => 86,
            MenuAction::ToggleStatsLens => 87,
            // The Deck's size (Stats Deck v2, 2026-10-20).
            MenuAction::DeckSizeAuto => 88,
            MenuAction::DeckSizeFull => 89,
            MenuAction::DeckSizeFolded => 90,
            MenuAction::Save => 91,
            // Per-session themes (palette-256-session).
            MenuAction::ChooseTabTheme => 77,
            MenuAction::ClearTabTheme => 78,
        }
    }

    /// Inverse of [`MenuAction::tag`]; `None` for an unknown, retired or zero tag, so the action
    /// selector ignores a tag it cannot decode rather than dispatching the wrong command.
    #[must_use]
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn from_tag(tag: isize) -> Option<MenuAction> {
        Some(match tag {
            1 => MenuAction::About,
            2 => MenuAction::Preferences,
            4 => MenuAction::Quit,
            5 => MenuAction::NewWindow,
            6 => MenuAction::NewTab,
            7 => MenuAction::CloseTab,
            8 => MenuAction::Copy,
            9 => MenuAction::Paste,
            10 => MenuAction::SelectAll,
            11 => MenuAction::Find,
            12 => MenuAction::ToggleFullScreen,
            13 => MenuAction::Minimize,
            14 => MenuAction::Zoom,
            15 => MenuAction::Help,
            16 => MenuAction::MoveTabToNewWindow,
            17 => MenuAction::ViewSessionInNewWindow,
            18 => MenuAction::MoveTabToNextWindow,
            19 => MenuAction::FindNext,
            20 => MenuAction::FindPrev,
            21 => MenuAction::FontIncrease,
            22 => MenuAction::FontDecrease,
            23 => MenuAction::FontActualSize,
            24 => MenuAction::SplitVertical,
            25 => MenuAction::SplitHorizontal,
            26 => MenuAction::NextTab,
            27 => MenuAction::PrevTab,
            30 => MenuAction::ToggleSettings,
            31 => MenuAction::OpenPalette,
            35 => MenuAction::SoftwareUpdate,
            36 => MenuAction::Version,
            37 => MenuAction::ApplyUpdate,
            38 => MenuAction::ReopenClosedTab,
            39 => MenuAction::OpenMarkdown,
            40 => MenuAction::OpenEditor,
            41 => MenuAction::ReopenClosedView,
            42 => MenuAction::CopySessionId,
            43 => MenuAction::CopyCwd,
            44 => MenuAction::ToggleMatrixRain,
            45 => MenuAction::ToggleSeriousMode,
            46 => MenuAction::FavouriteKitty,
            47 => MenuAction::Packages,
            48 => MenuAction::RenameSession,
            // 49-53, 55 and 56 retired (the connection presets and ids).
            54 => MenuAction::Overview,
            57 => MenuAction::NextKitty,
            // 58 retired (was Fleet) — see `tag`.
            // 59 retired (was Inbox).
            60 => MenuAction::HarnessLog,
            61 => MenuAction::TakeOver,
            62 => MenuAction::HandBack,
            63 => MenuAction::FabricStatus,
            64 => MenuAction::FabricOn,
            65 => MenuAction::FabricOff,
            66 => MenuAction::SetRole,
            // 67 retired (was Presence Band).
            68 => MenuAction::ToggleNeedsInWindows,
            69 => MenuAction::Messages,
            // 70-72 retired (named session identities).
            73 => MenuAction::ResetTerminal,
            74 => MenuAction::HarnessSettings,
            75 => MenuAction::ChooseTheme,
            76 => MenuAction::BrowseThemes,
            77 => MenuAction::ChooseTabTheme,
            78 => MenuAction::ClearTabTheme,
            79 => MenuAction::NextNeed,
            80 => MenuAction::ReadThisMac,
            81 => MenuAction::ReadFleet,
            82 => MenuAction::SummarizeFleet,
            83 => MenuAction::FleetMachines,
            84 => MenuAction::StatsBarsDeck,
            85 => MenuAction::StatsBarsSill,
            86 => MenuAction::StatsBarsOff,
            87 => MenuAction::ToggleStatsLens,
            88 => MenuAction::DeckSizeAuto,
            89 => MenuAction::DeckSizeFull,
            90 => MenuAction::DeckSizeFolded,
            91 => MenuAction::Save,
            _ => return None,
        })
    }
}

/// Fold a legacy `invoke` spelling onto the current Debug token. The rule of
/// this table: a wire name an agent may already call is never broken — it is
/// aliased. Every earlier name still resolves as itself (pinned by
/// `every_earlier_invoke_name_resolves_or_is_retired`); the aliases here are the
/// spellings a caller could reasonably have written for a row whose bar
/// identity moved:
///
/// * `OpenLedger`, `LedgerForSession` — the ledger key's `[keybindings]`
///   action name and the Fabric menu's "Ledger for This Session" row, both
///   now Harness Log for This Tab ([`MenuAction::HarnessLog`]).
/// * `ShowConnectionMap`, `Fleet` / `ShowFleet` — the Connection Map's row and
///   the Fabric menu's retired Fleet row, both now the Overview
///   ([`MenuAction::Overview`]).
/// * `HoldSession` / `LiftHold` — the Fabric menu's halt pair, now Take Over
///   and Hand Back on the person's slot: the same row, the hold
///   the person meant. [`InvokeAuthority::PersonOnly`], so both spellings
///   answer `ERR denied` over the wire.
/// * `TogglePresenceRim` — View ▸ Presence Rim, whose key (`[presence] rim`)
///   Show Needs in Windows writes ([`MenuAction::ToggleNeedsInWindows`]).
///
/// A name whose row is gone with nothing in its place is not aliased: it is
/// retired ([`retired_invoke_name`]) and answers `ERR retired: <what to use>`.
#[must_use]
pub(crate) fn canonical_invoke_name(name: &str) -> &str {
    match name {
        "HoldSession" => "TakeOver",
        "LiftHold" => "HandBack",
        "OpenLedger" | "LedgerForSession" => "HarnessLog",
        "ShowConnectionMap" | "Fleet" | "ShowFleet" => "Overview",
        "TogglePresenceRim" => "ToggleNeedsInWindows",
        other => other,
    }
}

/// What a retired `invoke` name answers instead (`ERR retired: <this>`), or
/// `None` for a name that is not retired. These rows went with nothing that does
/// what they did, so the name is refused with the thing to use instead —
/// never resolved to a different act, never an unknown-action error that
/// says nothing.
#[must_use]
pub(crate) fn retired_invoke_name(name: &str) -> Option<&'static str> {
    match name {
        "Inbox" | "ShowInbox" => Some("use aterm ctl @<sid> inbox"),
        "ConnectToSession"
        | "ConfigureConnection"
        | "DisconnectSession"
        | "NewControlledWindow"
        | "NewControlledTab"
        | "NewControllerWindow"
        | "NewControllerTab" => Some(
            "every in-session client already holds Owner; start workers with spawn agent=claude",
        ),
        // The row went with S7 and the band itself with the needs border
        // (S8): what replaces it has a switch of its own.
        "TogglePresenceBand" => Some(
            "the presence band is gone; the needs border and its pill replace it \
             (ToggleNeedsInWindows)",
        ),
        "ShowIdentity" | "NewWindowWithIdentity" | "NewTabWithIdentity" => {
            Some("named session identities were removed")
        }
        _ => None,
    }
}

/// Commands whose implementation requires a live terminal leaf. Native apps are
/// currently whole-tab surfaces, so advertising these while one is active would
/// promise a split/session operation the host cannot perform.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub(crate) const fn requires_terminal_tab(action: MenuAction) -> bool {
    matches!(
        action,
        MenuAction::SplitVertical
            | MenuAction::SplitHorizontal
            | MenuAction::ViewSessionInNewWindow
            | MenuAction::ToggleMatrixRain
            | MenuAction::RenameSession
            // The Agents menu's session rows: the harness log and the person's
            // hold are the focused session's; the role is its metadata, like
            // the pin. Overview, the reads, Summarize and the bus commands
            // are instance-wide and deliberately not listed.
            | MenuAction::HarnessLog
            | MenuAction::TakeOver
            | MenuAction::HandBack
            | MenuAction::SetRole
            | MenuAction::ResetTerminal
            // A tab's own theme is the front session's.
            | MenuAction::ChooseTabTheme
            | MenuAction::ClearTabTheme
    )
}

/// A value the App publishes for AppKit's synchronous `validateMenuItem:`,
/// which cannot reach the App; [`Published::published`] is its one access.
/// A shipping build keeps one per process, written and read on the main
/// thread. A test binary keeps one per thread (`menu_static!`): each test
/// runs on a thread of its own and every test App publishes (each focus runs
/// `sync_active_session`), so a value shared by the process would hand a test
/// another test's bits between its write and its read. A test reads exactly
/// what it, or the App it drives, published.
trait Published<T: 'static> {
    fn published<R>(&'static self, f: impl FnOnce(&T) -> R) -> R;
}

#[cfg(not(test))]
impl<T: 'static> Published<T> for T {
    fn published<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        f(self)
    }
}

#[cfg(test)]
impl<T: 'static> Published<T> for std::thread::LocalKey<T> {
    fn published<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        self.with(f)
    }
}

/// Declare a [`Published`] value: a `static` in a shipping build, a
/// `thread_local!` in a test binary.
macro_rules! menu_static {
    ($(#[$meta:meta])* static $name:ident: $ty:ty = $init:expr;) => {
        $(#[$meta])*
        #[cfg(not(test))]
        static $name: $ty = $init;
        #[cfg(test)]
        thread_local! {
            $(#[$meta])*
            static $name: $ty = const { $init };
        }
    };
}

menu_static! {
    /// The active-content bit read synchronously by AppKit's `validateMenuItem:`.
    /// `sync_active_session` publishes it at the same stabilization point used for
    /// title, toolbar, and control-handle changes, so opening the menu never exposes
    /// terminal-only actions as enabled over a native whole tab.
    static ACTIVE_TAB_IS_TERMINAL: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(true);
}

pub(crate) fn set_active_tab_is_terminal(terminal: bool) {
    ACTIVE_TAB_IS_TERMINAL
        .published(|bit| bit.store(terminal, std::sync::atomic::Ordering::Relaxed));
}

menu_static! {
    /// Whether the front window can actually PRESENT an inline rename editor. Off
    /// macOS the editor is drawn by the tab strip, so `tab_strip_rows = 0` leaves
    /// nowhere to put it — and an action that cannot run must not look available.
    /// Published exactly like [`set_active_tab_is_terminal`], from the same
    /// stabilization point, because AppKit validates menu commands synchronously
    /// when a menu opens.
    static RENAME_SURFACE_AVAILABLE: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(true);
}

pub(crate) fn set_rename_surface_available(available: bool) {
    RENAME_SURFACE_AVAILABLE
        .published(|bit| bit.store(available, std::sync::atomic::Ordering::Relaxed));
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn rename_surface_available() -> bool {
    RENAME_SURFACE_AVAILABLE.published(|bit| bit.load(std::sync::atomic::Ordering::Relaxed))
}

menu_static! {
    /// Whether Hand Back has a hold to lift on the FRONT window's focused
    /// session — the PERSON'S (`origin=person`, Take Over), or an Owner
    /// client's where no person's stands (`fabric::Holds::hand_back_origin`)
    /// — as the native menu's synchronous `validateMenuItem:` needs it: Take
    /// Over is live without it, Hand Back with it. Published by
    /// `App::publish_front_hold` from the same stabilization point as
    /// [`set_active_tab_is_terminal`] AND from every presence refresh of the
    /// front window, because a hold arrives on a fabric wake, not on a tab
    /// switch. A fleet hold moves neither row: it is not the window's to lift.
    static FRONT_PERSON_HOLD: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
}

/// Publish whether Hand Back has a hold to lift on the front session.
pub(crate) fn set_front_person_hold(held: bool) {
    FRONT_PERSON_HOLD.published(|bit| bit.store(held, std::sync::atomic::Ordering::Relaxed));
}

/// The published front person hold ([`set_front_person_hold`]).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn front_person_hold() -> bool {
    FRONT_PERSON_HOLD.published(|bit| bit.load(std::sync::atomic::Ordering::Relaxed))
}

menu_static! {
    /// Whether the hold Hand Back would lift on the front session is ANOTHER
    /// MAC'S (the step-in's `origin=remote`): Take Over is live beside Hand
    /// Back then — the person wins. Published with [`FRONT_PERSON_HOLD`] by
    /// `App::publish_front_hold`.
    static FRONT_REMOTE_HOLD: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
}

/// Publish whether another Mac holds the front session.
pub(crate) fn set_front_remote_hold(held: bool) {
    FRONT_REMOTE_HOLD.published(|bit| bit.store(held, std::sync::atomic::Ordering::Relaxed));
}

/// The published front remote hold ([`set_front_remote_hold`]).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn front_remote_hold() -> bool {
    FRONT_REMOTE_HOLD.published(|bit| bit.load(std::sync::atomic::Ordering::Relaxed))
}

menu_static! {
    /// View ▸ Show Needs in Windows' LIVE state (`[presence] rim`, as the App
    /// currently applies it), so `validateMenuItem:` can stamp the checkmark
    /// synchronously when the menu opens. Published through
    /// [`set_needs_in_windows`] by `App::adopt_presence_config` (load, reload),
    /// `App::set_presence_bit` (every toggle) and `App::sync_active_session`.
    static NEEDS_IN_WINDOWS: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(true);
}

/// Publish View ▸ Show Needs in Windows' checkmark.
pub(crate) fn set_needs_in_windows(on: bool) {
    NEEDS_IN_WINDOWS.published(|bit| bit.store(on, std::sync::atomic::Ordering::Relaxed));
}

menu_static! {
    /// View ▸ Stats Bars' LIVE form (`[sys] stats_bars` as the App applies
    /// it: 0 Deck, 1 Sill, 2 Off), so `validateMenuItem:` can stamp the
    /// radio's checkmark synchronously. Published through
    /// [`set_stats_bars_form`] by the config apply (load, reload, the
    /// radio's own write), `App::publish_menu_live` on the way to every wait
    /// and `App::sync_active_session`.
    static STATS_BARS_FORM: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
}

/// Publish View ▸ Stats Bars' radio.
pub(crate) fn set_stats_bars_form(form: crate::stats_deck::StatsBars) {
    let bits = crate::stats_deck::StatsBars::ALL
        .iter()
        .position(|f| *f == form)
        .unwrap_or(0) as u8;
    STATS_BARS_FORM.published(|bit| bit.store(bits, std::sync::atomic::Ordering::Relaxed));
}

/// The published Stats Bars form ([`set_stats_bars_form`]); the Deck before
/// any publish, as the config defaults. Read by the native menu's checkmark
/// ([`native_menu_checked`]), so it exists where that does.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn stats_bars_form() -> crate::stats_deck::StatsBars {
    let bits = STATS_BARS_FORM.published(|bit| bit.load(std::sync::atomic::Ordering::Relaxed));
    crate::stats_deck::StatsBars::ALL
        .get(usize::from(bits))
        .copied()
        .unwrap_or(crate::stats_deck::StatsBars::Deck)
}

menu_static! {
    /// View ▸ Deck Size's LIVE choice (`[sys] stats_deck_size` as the App
    /// applies it: 0 Automatic, 1 Full, 2 Folded), published and read exactly
    /// as [`STATS_BARS_FORM`] is.
    static DECK_SIZE_PREF: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
}

/// Publish View ▸ Deck Size's radio.
pub(crate) fn set_deck_size_pref(pref: crate::stats_plan::DeckSizePref) {
    let bits = crate::stats_plan::DeckSizePref::ALL
        .iter()
        .position(|p| *p == pref)
        .unwrap_or(0) as u8;
    DECK_SIZE_PREF.published(|bit| bit.store(bits, std::sync::atomic::Ordering::Relaxed));
}

/// The published Deck Size choice ([`set_deck_size_pref`]); Automatic before
/// any publish, as the config defaults. Read by the native menu's checkmark
/// ([`native_menu_checked`]), so it exists where that does.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn deck_size_pref() -> crate::stats_plan::DeckSizePref {
    let bits = DECK_SIZE_PREF.published(|bit| bit.load(std::sync::atomic::Ordering::Relaxed));
    crate::stats_plan::DeckSizePref::ALL
        .get(usize::from(bits))
        .copied()
        .unwrap_or_default()
}

impl MenuAction {
    /// The Stats Bars form a radio row names, or `None` for any other row.
    pub(crate) const fn stats_bars_form(self) -> Option<crate::stats_deck::StatsBars> {
        match self {
            MenuAction::StatsBarsDeck => Some(crate::stats_deck::StatsBars::Deck),
            MenuAction::StatsBarsSill => Some(crate::stats_deck::StatsBars::Sill),
            MenuAction::StatsBarsOff => Some(crate::stats_deck::StatsBars::Off),
            _ => None,
        }
    }

    /// The Deck Size choice a radio row names, or `None` for any other row.
    pub(crate) const fn deck_size_pref(self) -> Option<crate::stats_plan::DeckSizePref> {
        match self {
            MenuAction::DeckSizeAuto => Some(crate::stats_plan::DeckSizePref::Auto),
            MenuAction::DeckSizeFull => Some(crate::stats_plan::DeckSizePref::Full),
            MenuAction::DeckSizeFolded => Some(crate::stats_plan::DeckSizePref::Folded),
            _ => None,
        }
    }
}

/// The Agents menu's live bits, as `validateMenuItem:` needs them when the
/// menu opens (it cannot reach the App): Next That Needs You is live while a
/// local need stands, Overview while a window exists, Read the Fleet and
/// Fleet Machines in a fleet, and Summarize while a worker can start here —
/// its tool tip says why not, and its title names the fleet or this Mac.
/// Published by `App::publish_menu_live` on the way to every wait. The
/// default (every bit off) is the palette's pure-test state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentsLive {
    /// A session of this aterm has a need ([`MenuAction::NextNeed`]).
    pub needs_local: bool,
    /// This Mac is in a fleet ([`MenuAction::ReadFleet`]).
    pub in_fleet: bool,
    /// A window exists ([`MenuAction::Overview`]).
    pub any_window: bool,
    /// Why Summarize cannot start a worker here, `None` when it can.
    pub summarize_block: Option<String>,
}

impl AgentsLive {
    /// Before the first publish: the rows that need no fact live, the rest
    /// grey (a fleet, a need are facts).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) const UNPUBLISHED: AgentsLive = AgentsLive {
        needs_local: false,
        in_fleet: false,
        any_window: true,
        summarize_block: None,
    };
}

#[cfg(any(target_os = "macos", test))]
menu_static! {
    /// The Agents menu's live bits as last published: read by the native menu's
    /// `validateMenuItem:` (macOS), written on the way to a wait
    /// (`App::publish_agents_live`), so neither exists where no native menu does.
    static AGENTS_LIVE: std::sync::Mutex<AgentsLive> =
        std::sync::Mutex::new(AgentsLive::UNPUBLISHED);
}

/// Publish the Agents menu's live bits.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn set_agents_live(live: AgentsLive) {
    AGENTS_LIVE.published(|slot| {
        let mut slot = slot.lock().unwrap_or_else(|p| p.into_inner());
        if *slot != live {
            *slot = live;
        }
    });
}

/// The last published [`AgentsLive`].
#[cfg(any(target_os = "macos", test))]
pub(crate) fn agents_live() -> AgentsLive {
    AGENTS_LIVE.published(|slot| slot.lock().unwrap_or_else(|p| p.into_inner()).clone())
}

/// Summarize's title: the fleet's, or this Mac's with no fleet.
pub(crate) const fn summarize_title(in_fleet: bool) -> &'static str {
    if in_fleet {
        "Summarize the Fleet in a New Tab"
    } else {
        "Summarize This Mac in a New Tab"
    }
}

/// The LIVE tool tip a native row carries at the moment the menu opens, where
/// it says live state: Summarize's says why it cannot start. `None` keeps the
/// row's help sentence.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn native_item_tip(action: MenuAction) -> Option<String> {
    match action {
        MenuAction::SummarizeFleet => agents_live()
            .summarize_block
            .map(|why| format!("{} {why}", action.help())),
        _ => None,
    }
}

/// The full-screen row's title: it names what the row does now. The palette
/// row and the native View item both take it from here.
pub(crate) const fn full_screen_title(fullscreen: bool) -> &'static str {
    if fullscreen {
        "Exit Full Screen"
    } else {
        "Enter Full Screen"
    }
}

/// The ⌘W row's title: on a split tab ⌘W closes the focused pane (`App::close_active_tab`), so
/// the row says so. Shared by the palette row and the native File item; the tab context menu's
/// "Close Tab" really closes the tab and keeps its words.
pub(crate) const fn close_row_title(split: bool) -> &'static str {
    if split { "Close Pane" } else { "Close Tab" }
}

/// The front window's live state that the palette's `resolve` reads from `PaletteLive` and the
/// native bar's `validateMenuItem:` needs synchronously: the checkmarks (Serious Mode, Matrix
/// Rain, Favourite This Kitty), the enabled bits (Copy, Show Next / Previous Tab, the two Reopen
/// rows) and the ⌘W title. `App::publish_menu_live` publishes it before every wait, since these
/// move on a selection drag, a toggle or a close, not only on a tab switch.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MenuLive {
    /// A text selection exists (Copy).
    pub has_selection: bool,
    /// The active editor document has changes its file grant can save.
    pub can_save: bool,
    /// The focused content can open a search.
    pub can_find: bool,
    /// The focused content has a query Find Next/Previous can navigate.
    pub can_find_again: bool,
    /// The front window has two or more tabs (Show Next / Previous Tab).
    pub multi_tab: bool,
    /// A closed tab can be reopened (Reopen Closed Tab).
    pub can_reopen_closed_tab: bool,
    /// A closed pane can be reopened (Reopen Closed Pane).
    pub can_reopen_closed_view: bool,
    /// Serious mode is on (its checkmark).
    pub serious_mode: bool,
    /// The front session's matrix rain is on (its checkmark).
    pub rain_on: bool,
    /// The front window's kitty is the pinned favourite (its checkmark).
    pub kitty_favourited: bool,
    /// The front tab is split, so ⌘W closes a pane (the row's title).
    pub front_tab_split: bool,
}

#[cfg(any(target_os = "macos", test))]
impl MenuLive {
    /// Before the first publish: Save and Find disabled, other rows enabled, nothing checked.
    pub(crate) const UNPUBLISHED: MenuLive = MenuLive {
        has_selection: true,
        can_save: false,
        can_find: false,
        can_find_again: false,
        multi_tab: true,
        can_reopen_closed_tab: true,
        can_reopen_closed_view: true,
        serious_mode: false,
        rain_on: false,
        kitty_favourited: false,
        front_tab_split: false,
    };

    const fn bits(self) -> u16 {
        (self.has_selection as u16)
            | (self.multi_tab as u16) << 1
            | (self.can_reopen_closed_tab as u16) << 2
            | (self.can_reopen_closed_view as u16) << 3
            | (self.serious_mode as u16) << 4
            | (self.rain_on as u16) << 5
            | (self.kitty_favourited as u16) << 6
            | (self.front_tab_split as u16) << 7
            | (self.can_save as u16) << 8
            | (self.can_find as u16) << 9
            | (self.can_find_again as u16) << 10
    }

    const fn from_bits(b: u16) -> MenuLive {
        MenuLive {
            has_selection: b & 1 != 0,
            can_save: b & (1 << 8) != 0,
            can_find: b & (1 << 9) != 0,
            can_find_again: b & (1 << 10) != 0,
            multi_tab: b & (1 << 1) != 0,
            can_reopen_closed_tab: b & (1 << 2) != 0,
            can_reopen_closed_view: b & (1 << 3) != 0,
            serious_mode: b & (1 << 4) != 0,
            rain_on: b & (1 << 5) != 0,
            kitty_favourited: b & (1 << 6) != 0,
            front_tab_split: b & (1 << 7) != 0,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
menu_static! {
    static MENU_LIVE: std::sync::atomic::AtomicU16 =
        std::sync::atomic::AtomicU16::new(MenuLive::UNPUBLISHED.bits());
}

/// Publish the front window's [`MenuLive`].
#[cfg(any(target_os = "macos", test))]
pub(crate) fn set_menu_live(live: MenuLive) {
    MENU_LIVE.published(|bits| bits.store(live.bits(), std::sync::atomic::Ordering::Relaxed));
}

/// The last published [`MenuLive`] ([`MenuLive::UNPUBLISHED`] before any).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn menu_live() -> MenuLive {
    MenuLive::from_bits(MENU_LIVE.published(|bits| bits.load(std::sync::atomic::Ordering::Relaxed)))
}

/// The checkmark a checkable native row shows, or `None` for a plain command.
/// The same rows the palette checkmarks (`PaletteState::resolve`).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn native_menu_checked(action: MenuAction) -> Option<bool> {
    match action {
        MenuAction::ToggleNeedsInWindows => {
            Some(NEEDS_IN_WINDOWS.published(|bit| bit.load(std::sync::atomic::Ordering::Relaxed)))
        }
        MenuAction::StatsBarsDeck | MenuAction::StatsBarsSill | MenuAction::StatsBarsOff => {
            Some(action.stats_bars_form() == Some(stats_bars_form()))
        }
        MenuAction::DeckSizeAuto | MenuAction::DeckSizeFull | MenuAction::DeckSizeFolded => {
            Some(action.deck_size_pref() == Some(deck_size_pref()))
        }
        MenuAction::ToggleSeriousMode => Some(menu_live().serious_mode),
        MenuAction::ToggleMatrixRain => Some(menu_live().rain_on),
        MenuAction::FavouriteKitty => Some(menu_live().kitty_favourited),
        _ => None,
    }
}

/// The title a native row carries at the moment the menu opens, where it
/// names live state; `None` keeps the row's static label.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn native_live_title(action: MenuAction) -> Option<&'static str> {
    match action {
        MenuAction::CloseTab => Some(close_row_title(menu_live().front_tab_split)),
        MenuAction::SummarizeFleet => Some(summarize_title(agents_live().in_fleet)),
        _ => None,
    }
}

/// The title `chrome` prints for a native row tagged `tag`: its live title where it
/// has one ([`native_live_title`]), else `stamped`, the title the item carries.
/// `validateMenuItem:` stamps a live title only when the menu opens or a key
/// equivalent fires, so the item's own title can name state that has since moved
/// (⌘W on a split tab leaves Close Pane on a tab that is no longer split).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn native_row_title(tag: isize, stamped: String) -> String {
    MenuAction::from_tag(tag)
        .and_then(native_live_title)
        .map_or(stamped, str::to_string)
}

#[cfg(any(target_os = "macos", test))]
fn native_menu_action_enabled(action: MenuAction) -> bool {
    if matches!(action, MenuAction::RenameSession | MenuAction::SetRole)
        && !rename_surface_available()
    {
        return false;
    }
    // The person's hold pair reads the live projection: Take Over without
    // it, Hand Back with it. The Agents rows read their own live bits.
    match action {
        MenuAction::TakeOver if front_person_hold() && !front_remote_hold() => return false,
        MenuAction::HandBack if !front_person_hold() => return false,
        MenuAction::NextNeed
        | MenuAction::Overview
        | MenuAction::ReadFleet
        | MenuAction::FleetMachines
        | MenuAction::SummarizeFleet => {
            let agents = agents_live();
            let live = match action {
                MenuAction::NextNeed => agents.needs_local,
                MenuAction::Overview => agents.any_window,
                MenuAction::ReadFleet | MenuAction::FleetMachines => agents.in_fleet,
                _ => agents.summarize_block.is_none(),
            };
            if !live {
                return false;
            }
        }
        _ => {}
    }
    // The palette's preconditions (`PaletteState::resolve`): a row that would do nothing greys
    // out. A disabled row's key equivalent falls to the first responder, normally `on_key`,
    // whose ⌘C / ⇧⌘] / ⇧⌘T arms do the same, so a stale bit costs no keystroke. An open overlay
    // swallows the ⌘ chord (`App::on_key_overlay_mode`) and a native rename field has no ⌘C of
    // its own, so a live rename keeps Copy enabled (`App::publish_menu_live`).
    let live = menu_live();
    let precondition = match action {
        MenuAction::Copy => live.has_selection,
        MenuAction::Save => live.can_save,
        MenuAction::Find => live.can_find,
        MenuAction::FindNext | MenuAction::FindPrev => live.can_find_again,
        MenuAction::NextTab | MenuAction::PrevTab => live.multi_tab,
        MenuAction::ReopenClosedTab => live.can_reopen_closed_tab,
        MenuAction::ReopenClosedView => live.can_reopen_closed_view,
        _ => true,
    };
    precondition
        && (!requires_terminal_tab(action)
            || ACTIVE_TAB_IS_TERMINAL
                .published(|bit| bit.load(std::sync::atomic::Ordering::Relaxed)))
}

/// Take Over's tool tip: what
/// the person's hold does, and what any hold costs the whole instance, in
/// the one sentence the You hold section and `aterm ctl help hold` use too
/// ([`aterm_types::control_verbs::HOLD_COST`]). Gated as [`MenuAction::help`],
/// its one reader.
#[cfg(any(unix, a11y_tree, test))]
pub(crate) const TAKE_OVER_TIP: &str = concat!(
    "Only you type here until you hand it back. aterm's harness, managers and scripts get ERR \
     halted; the agent keeps running its turn. ",
    aterm_types::hold_cost!()
);

/// The control authority a socket caller must hold to reach a [`MenuAction`] through the
/// `invoke <action>` verb (and its `open <surface>` twins). The `invoke` verb's base op
/// is the benign `WriteInput` (it drives the human input vocabulary), but several menu
/// actions reach a strictly-greater capability that a plain child `WriteInput` edge must
/// not inherit — this is the classification the control layer's `argument_escalation`
/// reads to fence those indirect seams. Compiler-exhaustive over every variant (see
/// [`MenuAction::invoke_authority`]): a new `MenuAction` cannot compile until it is
/// classified here, so the fence can never silently miss a newly-added privileged action.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InvokeAuthority {
    /// Benign: the `invoke` verb's base `WriteInput` gate suffices (view, window, tab and
    /// navigation state, runtime-only font zoom — nothing durable or exfiltrating).
    WriteInput,
    /// Rewrites durable `aterm.toml` on invoke or raises the durable-config surface
    /// (`Preferences` opens aterm.toml in the native editor;
    /// `ToggleSettings` raises Top Settings) — the `ConfigWrite` fine op.
    ConfigWrite,
    /// Moves text onto or off the OS pasteboard — the `copy` verb's `ClipboardWrite` fine op.
    ClipboardWrite,
    /// Owner-only, which no single fine op expresses: a gateway to every action
    /// (`OpenPalette`) or a re-exec of a staged update (`SoftwareUpdate`/`ApplyUpdate`, twins
    /// of the Owner-only `update` verb).
    OwnerOnly,
    /// The person's own: no socket scope fires it, the Owner token included —
    /// every in-session client holds Owner, so [`Self::OwnerOnly`] would let
    /// the held agent invoke its own Hand Back. Take Over and Hand Back
    /// (`invoke TakeOver|HandBack`, and their aliases) answer `ERR denied`;
    /// the window's menu, palette, tab menu, Overview and pill reach them.
    /// The stats lens (`invoke ToggleStatsLens`) is the person's too: the
    /// window's View menu and ⌘I reach it.
    PersonOnly,
}

impl MenuAction {
    /// The [`InvokeAuthority`] a socket caller needs to fire this action via `invoke <action>`.
    /// Exhaustive and fail-closed: a new `MenuAction` must be classified here.
    pub(crate) fn invoke_authority(self) -> InvokeAuthority {
        // Every menu face goes through the command registry, which carries the finer effect
        // ceiling native reducers use, before mapping to the socket-op classes below.
        let _ = crate::command_registry::menu_command(self);
        use InvokeAuthority::{ClipboardWrite, ConfigWrite, OwnerOnly, PersonOnly, WriteInput};
        match self {
            // The clipboard boundary, the `copy` verb's fine op; the tab-context copies put
            // session id and cwd text on the pasteboard.
            MenuAction::Copy
            | MenuAction::Paste
            | MenuAction::SelectAll
            | MenuAction::CopySessionId
            | MenuAction::CopyCwd => ClipboardWrite,
            // Durable `aterm.toml` writes / the security-knob config surface.
            // `Packages`, `HarnessSettings` and `Messages` raise the SAME
            // durable-config Settings tab as `ToggleSettings`, just at their
            // routes — same fence.
            MenuAction::Preferences
            | MenuAction::ToggleSettings
            | MenuAction::Packages
            | MenuAction::HarnessSettings
            | MenuAction::BrowseThemes
            | MenuAction::Messages
            | MenuAction::ToggleSeriousMode => ConfigWrite,
            // Gateway to every action + the staged-update re-exec twins.
            MenuAction::OpenPalette | MenuAction::SoftwareUpdate | MenuAction::ApplyUpdate => {
                OwnerOnly
            }
            // The Agents menu. The Overview lists every session of the
            // instance (the `open overview` OwnerOnly class). The harness log
            // runs a child on this instance's own socket and raises the
            // browser; Read This Mac / the Fleet and Fleet Machines run a
            // child that discloses the whole Mac or fleet; Summarize starts a
            // worker (`spawn agent=` is Owner-only on the wire). The three bus
            // commands run `aterm fabric` as a child: `on`/`off` rewrite the
            // machine's bus (the verb is Owner-only by its own doc),
            // and `status` discloses all of it. No fine op expresses any of
            // these.
            MenuAction::Overview
            | MenuAction::HarnessLog
            | MenuAction::ReadThisMac
            | MenuAction::ReadFleet
            | MenuAction::SummarizeFleet
            | MenuAction::FleetMachines
            | MenuAction::FabricStatus
            | MenuAction::FabricOn
            | MenuAction::FabricOff => OwnerOnly,
            // The person's hold is the person's: a held agent holds Owner,
            // so no scope may take a tab over or hand it back for them.
            MenuAction::TakeOver | MenuAction::HandBack => PersonOnly,
            // Show Needs in Windows writes `[presence] rim` in aterm.toml —
            // durable config, the `ConfigWrite` fine op, exactly as Serious
            // Mode is classified.
            MenuAction::ToggleNeedsInWindows => ConfigWrite,
            // The Stats Bars radio writes `[sys] stats_bars` in aterm.toml:
            // durable config, as Show Needs in Windows is.
            MenuAction::StatsBarsDeck | MenuAction::StatsBarsSill | MenuAction::StatsBarsOff => {
                ConfigWrite
            }
            // The Deck Size radio writes `[sys] stats_deck_size`: the same.
            MenuAction::DeckSizeAuto | MenuAction::DeckSizeFull | MenuAction::DeckSizeFolded => {
                ConfigWrite
            }
            // The lens is the person's close-up, summoned by their hand
            // (design §12.2: agents never summon the lens). Every in-session
            // client holds Owner, so only the person's own class keeps agents
            // from opening it.
            MenuAction::ToggleStatsLens => PersonOnly,
            // The theme palette writes `theme` to aterm.toml on Enter, and a
            // caller that may open it may also press that Enter: the
            // `settings set theme` class, not the palette gateway's Owner-only
            // one (its rows reach no other action).
            MenuAction::ChooseTheme => ConfigWrite,
            // Benign runtime/view/window/tab state. Font zoom is runtime-only
            // (`set_font_size` changes the live size; it does NOT persist to `aterm.toml`), so it
            // stays `WriteInput`. `Quit` is a denial of service, not a capability escalation, so it
            // is left with the base gate rather than over-reaching to Owner-only.
            MenuAction::About
            | MenuAction::Version
            | MenuAction::Quit
            | MenuAction::NewWindow
            | MenuAction::NewTab
            | MenuAction::OpenMarkdown
            | MenuAction::OpenEditor
            | MenuAction::Save
            | MenuAction::ReopenClosedTab
            | MenuAction::ReopenClosedView
            | MenuAction::MoveTabToNewWindow
            | MenuAction::MoveTabToNextWindow
            | MenuAction::ViewSessionInNewWindow
            | MenuAction::CloseTab
            | MenuAction::Find
            | MenuAction::FindNext
            | MenuAction::FindPrev
            | MenuAction::ToggleFullScreen
            | MenuAction::FontIncrease
            | MenuAction::FontDecrease
            | MenuAction::FontActualSize
            | MenuAction::SplitVertical
            | MenuAction::SplitHorizontal
            // Runtime-only per-session visual toggle: nothing durable is written.
            | MenuAction::ToggleMatrixRain
            // Writes only the machine-owned toy ledger (`kitty-collectibles.toml` and its
            // `kitty-log.toml` mirror), never `aterm.toml`, so not `ConfigWrite`.
            | MenuAction::FavouriteKitty
            // Same ledger: moves one pin stamp; also the class of the `kitty` control verb.
            | MenuAction::NextKitty
            // Opens the inline pin editor. Its eventual write is `meta set title`,
            // which the control verb table's `meta` form already classifies as
            // `WriteInput` (not `ConfigWrite` — nothing durable on disk is
            // rewritten), so this matches rather than tunnels under it.
            | MenuAction::RenameSession
            // Same editor, same eventual write (`meta set role`).
            | MenuAction::SetRole
            // Moves focus to the tab that needs the person: what `raise`
            // does, and it writes nothing.
            | MenuAction::NextNeed
            // The `reset` verb's twin, and the verb is `Write`: it moves the
            // front session's terminal modes, writes no input and nothing
            // durable.
            | MenuAction::ResetTerminal
            // `theme session set|clear`'s twins: one session's colours,
            // runtime only (carried by an update and a restore), the verb's
            // `Write` class.
            | MenuAction::ChooseTabTheme
            | MenuAction::ClearTabTheme
            | MenuAction::Minimize
            | MenuAction::Zoom
            | MenuAction::NextTab
            | MenuAction::PrevTab
            | MenuAction::Help => WriteInput,
        }
    }

    /// Whether firing this action writes input bytes to a session's PTY — the
    /// question the unread-input gate (`crate::input_stall::gate`)
    /// asks of `invoke <action>`. Exhaustive for the reason
    /// [`Self::invoke_authority`] is: a new action that pastes must be
    /// classified here, not default to "writes nothing" and slip past the gate.
    /// Only `Paste` writes input (the OS clipboard into the front tab's PTY);
    /// the rest move view, window or tab state, the pasteboard, or config.
    #[must_use]
    pub(crate) const fn writes_pty_input(self) -> bool {
        match self {
            MenuAction::Paste => true,
            // The stats bars' form and the Deck's size are config; the lens
            // is a view.
            MenuAction::StatsBarsDeck
            | MenuAction::StatsBarsSill
            | MenuAction::StatsBarsOff
            | MenuAction::DeckSizeAuto
            | MenuAction::DeckSizeFull
            | MenuAction::DeckSizeFolded
            | MenuAction::ToggleStatsLens => false,
            MenuAction::About
            | MenuAction::SoftwareUpdate
            | MenuAction::Version
            | MenuAction::ApplyUpdate
            | MenuAction::Preferences
            | MenuAction::Quit
            | MenuAction::NewWindow
            | MenuAction::NewTab
            | MenuAction::OpenMarkdown
            | MenuAction::OpenEditor
            | MenuAction::Save
            | MenuAction::ReopenClosedTab
            | MenuAction::ReopenClosedView
            | MenuAction::MoveTabToNewWindow
            | MenuAction::MoveTabToNextWindow
            | MenuAction::ViewSessionInNewWindow
            | MenuAction::CloseTab
            | MenuAction::Copy
            | MenuAction::SelectAll
            | MenuAction::Find
            | MenuAction::FindNext
            | MenuAction::FindPrev
            | MenuAction::ToggleFullScreen
            | MenuAction::FontIncrease
            | MenuAction::FontDecrease
            | MenuAction::FontActualSize
            | MenuAction::SplitVertical
            | MenuAction::SplitHorizontal
            | MenuAction::ToggleMatrixRain
            | MenuAction::FavouriteKitty
            | MenuAction::NextKitty
            | MenuAction::ToggleSeriousMode
            | MenuAction::ToggleSettings
            | MenuAction::ToggleNeedsInWindows
            | MenuAction::Packages
            | MenuAction::HarnessSettings
            | MenuAction::Messages
            | MenuAction::OpenPalette
            | MenuAction::ChooseTheme
            | MenuAction::BrowseThemes
            | MenuAction::CopySessionId
            | MenuAction::CopyCwd
            | MenuAction::Overview
            | MenuAction::NextNeed
            | MenuAction::HarnessLog
            | MenuAction::TakeOver
            | MenuAction::HandBack
            | MenuAction::ReadThisMac
            | MenuAction::ReadFleet
            | MenuAction::SummarizeFleet
            | MenuAction::FleetMachines
            | MenuAction::FabricStatus
            | MenuAction::FabricOn
            | MenuAction::FabricOff
            | MenuAction::RenameSession
            | MenuAction::SetRole
            // Resets the terminal's modes; nothing reaches the program's input.
            | MenuAction::ResetTerminal
            // Recolour one session; nothing reaches its input.
            | MenuAction::ChooseTabTheme
            | MenuAction::ClearTabTheme
            | MenuAction::Minimize
            | MenuAction::Zoom
            | MenuAction::NextTab
            | MenuAction::PrevTab
            | MenuAction::Help => false,
        }
    }

    /// The row's help: one sentence, the native `NSMenuItem` tool tip and the palette row's
    /// accessibility description. Exhaustive, so a new action cannot ship without one. Plain
    /// sentences: no command text, no mail body, nothing a session wrote.
    #[must_use]
    #[cfg(any(unix, a11y_tree, test))]
    pub(crate) const fn help(self) -> &'static str {
        match self {
            MenuAction::About => "Open About aterm: the build, version and signing details.",
            MenuAction::SoftwareUpdate => "Open Software Update and check for a newer build now.",
            MenuAction::Version => "Open About aterm from the version badge.",
            MenuAction::ApplyUpdate => {
                "Install the staged update now; with none staged, or one on hold, open Software \
                 Update."
            }
            MenuAction::Preferences => "Open aterm.toml in the assisted config editor.",
            MenuAction::Quit if cfg!(windows) => "Close every aterm window and exit.",
            MenuAction::Quit => "Quit aterm.",
            MenuAction::NewWindow => "Open a new window of this aterm.",
            MenuAction::NewTab => "Open a new terminal tab in this window.",
            MenuAction::OpenMarkdown => "Choose a local file and open it as read-only Markdown.",
            MenuAction::OpenEditor => "Choose a local file and open it in the editor.",
            MenuAction::Save => "Save changes to the active file.",
            MenuAction::ReopenClosedTab => "Reopen the tab closed most recently.",
            MenuAction::ReopenClosedView => {
                "Reopen the pane closed most recently where it was, or in a new tab if that \
                 spot is gone."
            }
            MenuAction::MoveTabToNewWindow => "Move this tab into a window of its own.",
            MenuAction::MoveTabToNextWindow => "Move this tab into the next window.",
            MenuAction::ViewSessionInNewWindow => {
                "Show this session in a second window, the same live grid in both."
            }
            MenuAction::CloseTab => "Close the focused pane; a tab's last pane closes the tab.",
            MenuAction::Copy => "Copy the selection.",
            MenuAction::Paste => "Paste the clipboard.",
            MenuAction::SelectAll => "Select the whole screen.",
            MenuAction::Find => "Search the active tab.",
            MenuAction::FindNext => "Step to the next match.",
            MenuAction::FindPrev => "Step to the previous match.",
            MenuAction::ToggleFullScreen => "Enter or leave full screen.",
            MenuAction::FontIncrease => "Make the text larger for this run.",
            MenuAction::FontDecrease => "Make the text smaller for this run.",
            MenuAction::FontActualSize => "Return the text to its configured size.",
            MenuAction::SplitVertical => "Split the focused pane left and right.",
            MenuAction::SplitHorizontal => "Split the focused pane top and bottom.",
            MenuAction::ToggleMatrixRain => "Toggle matrix rain under this session's text.",
            MenuAction::FavouriteKitty => "Keep the kitty on this window as the pinned favourite.",
            MenuAction::NextKitty => "Wear the next kitty in the collection.",
            MenuAction::ToggleSeriousMode => {
                "Suppress every sound and decoration until turned back off."
            }
            MenuAction::ToggleSettings => "Open Settings.",
            MenuAction::Packages => "Open Settings at Packages: the ALab toolchain.",
            MenuAction::Messages => "Open Settings at Messages: everything aterm told you.",
            MenuAction::HarnessSettings => {
                "Open Settings at Harness: the coding agent harness and its Autopilot."
            }
            MenuAction::OpenPalette => "Open the command palette: every command, by name.",
            MenuAction::ChooseTheme => {
                "Choose a theme from a list: arrow keys preview it, Enter keeps it, Esc puts \
                 the current one back."
            }
            MenuAction::BrowseThemes => {
                "Open Settings at Themes: try every theme on a live preview and tune its fonts \
                 and colors."
            }
            MenuAction::CopySessionId => {
                "Copy this session's id, which scripts and other sessions use to reach it."
            }
            MenuAction::CopyCwd => "Copy this session's working directory.",
            MenuAction::Overview => {
                "Show every session in this aterm: what it needs, what it is doing, its task, \
                 who drives it, and who manages whom."
            }
            MenuAction::NextNeed => {
                "Go to the tab that needs you most; again, to the next one that needs you."
            }
            MenuAction::HarnessLog => {
                "Write what the harness did in this tab, and what was typed, and open it in the \
                 browser."
            }
            MenuAction::TakeOver => TAKE_OVER_TIP,
            MenuAction::HandBack => {
                "Hand this tab back: your hold lifts; any other hold on it stays."
            }
            MenuAction::ReadThisMac => {
                "Read every session on this Mac (aterm ctl survey) in a tab; no model runs."
            }
            MenuAction::ReadFleet => {
                "Read every Mac in the fleet (aterm ctl survey --fleet) in a tab; no model runs."
            }
            MenuAction::SummarizeFleet => {
                "Start one agent in a new tab that reads the survey once and writes what needs \
                 you, what everyone is doing and what finished."
            }
            MenuAction::FabricStatus => "Show the message bus's status as a tab.",
            MenuAction::FabricOn => "Turn the message bus on for this machine, after you confirm.",
            MenuAction::FabricOff => {
                "Turn the message bus off for this machine, after you confirm."
            }
            MenuAction::FleetMachines => "List the fleet's machines (aterm fleet peers) in a tab.",
            MenuAction::RenameSession => "Name this session; the tab shows the name.",
            MenuAction::SetRole => {
                "Give this session a role; its tab and every list of needs use it."
            }
            MenuAction::ResetTerminal => {
                "Hand this terminal's keys, mouse and modes back to their defaults; \
                 keeps the screen and scrollback."
            }
            MenuAction::ChooseTabTheme => {
                "Choose a theme for this tab only: arrow keys preview it here, Enter keeps it, \
                 Esc puts the tab's theme back."
            }
            MenuAction::ClearTabTheme => "Draw this tab in the window's theme again.",
            MenuAction::Minimize => "Minimise this window.",
            MenuAction::Zoom if cfg!(windows) => "Maximize this window, or restore it.",
            MenuAction::Zoom => "Zoom this window.",
            MenuAction::NextTab => "Show the next tab.",
            MenuAction::PrevTab => "Show the previous tab.",
            MenuAction::ToggleNeedsInWindows => {
                "Show or hide the border and the titlebar pill that say a window's tab needs \
                 you; saved in aterm.toml."
            }
            MenuAction::StatsBarsDeck => {
                "Show the Global, aterm and Tab bars at full size under the titlebar; saved in \
                 aterm.toml."
            }
            MenuAction::StatsBarsSill => {
                "Show the Global, aterm and Tab bars as light lanes in the window's bottom \
                 padding, which takes no terminal row; saved in aterm.toml."
            }
            MenuAction::StatsBarsOff => "Hide the stats bars; saved in aterm.toml.",
            MenuAction::DeckSizeAuto => {
                "Show each window's stats deck Full when the window is tall and Folded when it \
                 is short; saved in aterm.toml."
            }
            MenuAction::DeckSizeFull => {
                "Show the stats deck Full, a row per resource, in every window; saved in \
                 aterm.toml."
            }
            MenuAction::DeckSizeFolded => {
                "Show the stats deck Folded into two blocks in every window; saved in aterm.toml."
            }
            MenuAction::ToggleStatsLens => {
                "Open the stats bars' close-up, or close it: arrows move between instruments \
                 and bars, Escape closes."
            }
            MenuAction::Help => "Open the aterm guide.",
        }
    }

    /// Whether this platform can do what the row offers: the update rows only where the in-app
    /// updater exists ([`aterm_update::enabled`]). The palette, the native bar and the `chrome`
    /// lines all ask this.
    #[must_use]
    pub(crate) fn offered_on_this_platform(self) -> bool {
        match self {
            MenuAction::SoftwareUpdate | MenuAction::ApplyUpdate => aterm_update::enabled(),
            _ => true,
        }
    }

    /// Recover a [`MenuAction`] from the `{:?}` Debug token carried on the wire by
    /// `invoke <Name>` (the exact token [`crate::palette::PaletteState::action_by_name`]
    /// matches and `controls menu` prints), or `None` for an unknown token. The control
    /// layer uses this to classify a socket `invoke` BEFORE the action is dispatched, so
    /// a scoped edge is fenced from the privileged actions at the authority layer.
    #[must_use]
    pub(crate) fn from_invoke_name(name: &str) -> Option<MenuAction> {
        match canonical_invoke_name(name) {
            "About" => Some(MenuAction::About),
            "SoftwareUpdate" => Some(MenuAction::SoftwareUpdate),
            "Version" => Some(MenuAction::Version),
            "ApplyUpdate" => Some(MenuAction::ApplyUpdate),
            "Preferences" => Some(MenuAction::Preferences),
            "Quit" => Some(MenuAction::Quit),
            "NewWindow" => Some(MenuAction::NewWindow),
            "NewTab" => Some(MenuAction::NewTab),
            "OpenMarkdown" => Some(MenuAction::OpenMarkdown),
            "OpenEditor" => Some(MenuAction::OpenEditor),
            "Save" => Some(MenuAction::Save),
            "ReopenClosedTab" => Some(MenuAction::ReopenClosedTab),
            "ReopenClosedView" => Some(MenuAction::ReopenClosedView),
            "MoveTabToNewWindow" => Some(MenuAction::MoveTabToNewWindow),
            "MoveTabToNextWindow" => Some(MenuAction::MoveTabToNextWindow),
            "ViewSessionInNewWindow" => Some(MenuAction::ViewSessionInNewWindow),
            "CloseTab" => Some(MenuAction::CloseTab),
            "Copy" => Some(MenuAction::Copy),
            "Paste" => Some(MenuAction::Paste),
            "SelectAll" => Some(MenuAction::SelectAll),
            "Find" => Some(MenuAction::Find),
            "FindNext" => Some(MenuAction::FindNext),
            "FindPrev" => Some(MenuAction::FindPrev),
            "ToggleFullScreen" => Some(MenuAction::ToggleFullScreen),
            "FontIncrease" => Some(MenuAction::FontIncrease),
            "FontDecrease" => Some(MenuAction::FontDecrease),
            "FontActualSize" => Some(MenuAction::FontActualSize),
            "SplitVertical" => Some(MenuAction::SplitVertical),
            "SplitHorizontal" => Some(MenuAction::SplitHorizontal),
            "ToggleMatrixRain" => Some(MenuAction::ToggleMatrixRain),
            "FavouriteKitty" => Some(MenuAction::FavouriteKitty),
            "NextKitty" => Some(MenuAction::NextKitty),
            "ToggleSeriousMode" => Some(MenuAction::ToggleSeriousMode),
            "ToggleSettings" => Some(MenuAction::ToggleSettings),
            "Packages" => Some(MenuAction::Packages),
            "Messages" => Some(MenuAction::Messages),
            "HarnessSettings" => Some(MenuAction::HarnessSettings),
            "RenameSession" => Some(MenuAction::RenameSession),
            "OpenPalette" => Some(MenuAction::OpenPalette),
            "ChooseTheme" => Some(MenuAction::ChooseTheme),
            "BrowseThemes" => Some(MenuAction::BrowseThemes),
            "Minimize" => Some(MenuAction::Minimize),
            "Zoom" => Some(MenuAction::Zoom),
            "NextTab" => Some(MenuAction::NextTab),
            "PrevTab" => Some(MenuAction::PrevTab),
            "Help" => Some(MenuAction::Help),
            "CopySessionId" => Some(MenuAction::CopySessionId),
            "CopyCwd" => Some(MenuAction::CopyCwd),
            "Overview" => Some(MenuAction::Overview),
            "NextNeed" => Some(MenuAction::NextNeed),
            "HarnessLog" => Some(MenuAction::HarnessLog),
            "ReadThisMac" => Some(MenuAction::ReadThisMac),
            "ReadFleet" => Some(MenuAction::ReadFleet),
            "SummarizeFleet" => Some(MenuAction::SummarizeFleet),
            "FleetMachines" => Some(MenuAction::FleetMachines),
            "TakeOver" => Some(MenuAction::TakeOver),
            "HandBack" => Some(MenuAction::HandBack),
            "FabricStatus" => Some(MenuAction::FabricStatus),
            "FabricOn" => Some(MenuAction::FabricOn),
            "FabricOff" => Some(MenuAction::FabricOff),
            "SetRole" => Some(MenuAction::SetRole),
            "ToggleNeedsInWindows" => Some(MenuAction::ToggleNeedsInWindows),
            "StatsBarsDeck" => Some(MenuAction::StatsBarsDeck),
            "StatsBarsSill" => Some(MenuAction::StatsBarsSill),
            "StatsBarsOff" => Some(MenuAction::StatsBarsOff),
            "DeckSizeAuto" => Some(MenuAction::DeckSizeAuto),
            "DeckSizeFull" => Some(MenuAction::DeckSizeFull),
            "DeckSizeFolded" => Some(MenuAction::DeckSizeFolded),
            "ToggleStatsLens" => Some(MenuAction::ToggleStatsLens),
            "ResetTerminal" => Some(MenuAction::ResetTerminal),
            "ChooseTabTheme" => Some(MenuAction::ChooseTabTheme),
            "ClearTabTheme" => Some(MenuAction::ClearTabTheme),
            _ => None,
        }
    }
}

/// Modifier mask of a menu item's visual key-equivalent. Platform-neutral; the macOS
/// builder maps it to `NSEventModifierFlags`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuMods {
    /// No modifier (or no key-equivalent at all when `key` is empty).
    None,
    /// ⌘
    Command,
    /// ⇧⌘
    CommandShift,
    /// ⌃⌘
    CommandControl,
}

/// One entry of a menu section: a command, a submenu or a divider.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MenuEntry {
    Separator,
    Submenu {
        label: &'static str,
        entries: &'static [MenuEntry],
    },
    Item {
        label: &'static str,
        action: MenuAction,
        /// Lowercase key-equivalent character ("" for none), visual only: `App::on_key`
        /// handles the keystroke.
        key: &'static str,
        mods: MenuMods,
    },
}

impl MenuEntry {
    /// This entry's item, if it is one — the one walk every consumer shares
    /// (the completeness proof, the palette, the accelerator hints), including
    /// commands nested in submenus.
    pub(crate) fn items(
        &self,
    ) -> impl Iterator<Item = (&'static str, MenuAction, &'static str, MenuMods)> + '_ {
        let mut pending = Vec::new();
        let mut item = match self {
            MenuEntry::Item {
                label,
                action,
                key,
                mods,
            } => Some((*label, *action, *key, *mods)),
            MenuEntry::Submenu { entries, .. } => {
                pending.push(entries.iter());
                None
            }
            MenuEntry::Separator => None,
        };
        // Ordinary items allocate nothing. A submenu retains only its current
        // ancestry while walking the same commands in menu order.
        std::iter::from_fn(move || {
            if let Some(item) = item.take() {
                return Some(item);
            }
            loop {
                match pending.last_mut()?.next() {
                    Some(MenuEntry::Item {
                        label,
                        action,
                        key,
                        mods,
                    }) => {
                        return Some((*label, *action, *key, *mods));
                    }
                    Some(MenuEntry::Submenu { entries, .. }) => pending.push(entries.iter()),
                    Some(MenuEntry::Separator) => {}
                    None => {
                        pending.pop();
                    }
                }
            }
        })
    }
}

/// A top-level menu (App / File / …) and its entries.
pub(crate) struct MenuSection {
    pub title: &'static str,
    pub entries: &'static [MenuEntry],
}

use MenuEntry::{Item, Separator, Submenu};

use crate::update_apply_trouble::ApplyTrouble;

const APP_MENU: &[MenuEntry] = &[
    Item {
        label: "About aterm",
        action: MenuAction::About,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    // The ONE update entry point: opens the Software Update route and checks in one gesture.
    // Live only where the in-app updater exists (`MenuAction::offered_on_this_platform`):
    // elsewhere the palette leaves the row out (`palette::PaletteState::new`) rather than
    // listing a row that can never act, and the `chrome` menu lines leave it out too.
    Item {
        label: "Check for Updates…",
        action: MenuAction::SoftwareUpdate,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    Item {
        label: "Settings…",
        action: MenuAction::ToggleSettings,
        key: ",",
        mods: MenuMods::Command,
    },
    Item {
        label: "Packages…",
        action: MenuAction::Packages,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Messages…",
        action: MenuAction::Messages,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Open aterm.toml",
        action: MenuAction::Preferences,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    Item {
        label: QUIT_LABEL,
        action: MenuAction::Quit,
        key: "q",
        mods: MenuMods::Command,
    },
];

/// The quit row's words: Windows names the verb Exit (every Windows app's File ▸
/// Exit); macOS and Linux keep Quit aterm.
const QUIT_LABEL: &str = if cfg!(windows) { "Exit" } else { "Quit aterm" };

/// Window ▸ Zoom's words in the model, before [`zoom_title`] reads the window's
/// state. On Windows "Zoom" reads as the font zoom beside Increase Font Size; the
/// verb there is Maximize (and Restore Down on a maximized window).
const ZOOM_LABEL: &str = zoom_title(false);

/// Window ▸ Zoom's title for a window that is (or is not) `maximized`: the
/// caption button's own tooltip on Windows, the green button's Zoom elsewhere.
pub(crate) const fn zoom_title(maximized: bool) -> &'static str {
    if !cfg!(windows) {
        "Zoom"
    } else if maximized {
        "Restore Down"
    } else {
        "Maximize"
    }
}

const FILE_MENU: &[MenuEntry] = &[
    Item {
        label: "New Window",
        action: MenuAction::NewWindow,
        key: "n",
        mods: MenuMods::Command,
    },
    Item {
        label: "New Terminal Tab",
        action: MenuAction::NewTab,
        key: "t",
        mods: MenuMods::Command,
    },
    Separator,
    Item {
        label: "Open Markdown…",
        action: MenuAction::OpenMarkdown,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Open File…",
        action: MenuAction::OpenEditor,
        key: "o",
        mods: MenuMods::Command,
    },
    Item {
        label: "Save",
        action: MenuAction::Save,
        key: "s",
        mods: MenuMods::Command,
    },
    Item {
        label: "Reopen Closed Tab",
        action: MenuAction::ReopenClosedTab,
        key: "t",
        mods: MenuMods::CommandShift,
    },
    Item {
        label: "Reopen Closed Pane",
        action: MenuAction::ReopenClosedView,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    Item {
        label: "Close Tab",
        action: MenuAction::CloseTab,
        key: "w",
        mods: MenuMods::Command,
    },
];

/// The Agents menu: what needs the person, everyone at a glance, stepping in,
/// and reading the Mac or the fleet. Placed between View and Window, where an
/// application's own menus go on macOS. Live greying comes from
/// `validateMenuItem:`'s statics ([`AgentsLive`], [`front_person_hold`]); the
/// keystrokes are the menu's own key equivalents (`MENU_CMD_CHORDS`), and
/// ⇧⌘L is `App::on_key`'s (`BUILTIN_CMD_CHORDS`).
const AGENTS_MENU: &[MenuEntry] = &[
    Item {
        label: "Next That Needs You",
        action: MenuAction::NextNeed,
        key: "j",
        mods: MenuMods::CommandShift,
    },
    Item {
        label: "Overview…",
        action: MenuAction::Overview,
        key: "o",
        mods: MenuMods::CommandControl,
    },
    Separator,
    Item {
        label: "Take Over This Tab",
        action: MenuAction::TakeOver,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Hand Back This Tab",
        action: MenuAction::HandBack,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    Item {
        label: "Harness Log for This Tab",
        action: MenuAction::HarnessLog,
        key: "l",
        mods: MenuMods::CommandShift,
    },
    Item {
        label: "Set Role…",
        action: MenuAction::SetRole,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    Item {
        label: "Read This Mac",
        action: MenuAction::ReadThisMac,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Read the Fleet",
        action: MenuAction::ReadFleet,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: summarize_title(true),
        action: MenuAction::SummarizeFleet,
        key: "",
        mods: MenuMods::None,
    },
];

/// The palette-only rows:
/// commands with no bar item, listed in the palette under [`PALETTE_ONLY_SECTION`]
/// and reachable by `invoke`. The message bus's administration lives here and
/// on the CLI (`aterm fabric`), never in a menu: a bus that is off (the
/// default) is mentioned nowhere else.
pub(crate) const PALETTE_ONLY: &[(&str, MenuAction)] = &[
    ("Message Bus: Status", MenuAction::FabricStatus),
    ("Message Bus: Turn On…", MenuAction::FabricOn),
    ("Message Bus: Turn Off…", MenuAction::FabricOff),
    ("Fleet Machines", MenuAction::FleetMachines),
];

/// The palette section the [`PALETTE_ONLY`] rows sit in: the menu whose
/// matter they are.
pub(crate) const PALETTE_ONLY_SECTION: &str = "Agents";

const FIND_MENU: &[MenuEntry] = &[
    Item {
        label: "Find…",
        action: MenuAction::Find,
        key: "f",
        mods: MenuMods::Command,
    },
    Item {
        label: "Find Next",
        action: MenuAction::FindNext,
        key: "g",
        mods: MenuMods::Command,
    },
    Item {
        label: "Find Previous",
        action: MenuAction::FindPrev,
        key: "g",
        mods: MenuMods::CommandShift,
    },
];

const EDIT_MENU: &[MenuEntry] = &[
    Item {
        label: "Copy",
        action: MenuAction::Copy,
        key: "c",
        mods: MenuMods::Command,
    },
    Item {
        label: "Paste",
        action: MenuAction::Paste,
        key: "v",
        mods: MenuMods::Command,
    },
    Item {
        label: "Select All",
        action: MenuAction::SelectAll,
        key: "a",
        mods: MenuMods::Command,
    },
    Separator,
    Submenu {
        label: "Find",
        entries: FIND_MENU,
    },
    Separator,
    // No key equivalent: Terminal.app's ⌥⌘R needs an Option modifier `MenuMods` does not carry,
    // and every free ⌘ letter is the shell's.
    Item {
        label: "Reset Terminal",
        action: MenuAction::ResetTerminal,
        key: "",
        mods: MenuMods::None,
    },
];

const VIEW_MENU: &[MenuEntry] = &[
    Item {
        label: "Increase Font Size",
        action: MenuAction::FontIncrease,
        key: "+",
        mods: MenuMods::Command,
    },
    Item {
        label: "Decrease Font Size",
        action: MenuAction::FontDecrease,
        key: "-",
        mods: MenuMods::Command,
    },
    Item {
        label: "Actual Size",
        action: MenuAction::FontActualSize,
        key: "0",
        mods: MenuMods::Command,
    },
    Separator,
    Item {
        label: "Split Right",
        action: MenuAction::SplitVertical,
        key: "d",
        mods: MenuMods::Command,
    },
    Item {
        label: "Split Down",
        action: MenuAction::SplitHorizontal,
        key: "d",
        mods: MenuMods::CommandShift,
    },
    Separator,
    Item {
        label: full_screen_title(false),
        action: MenuAction::ToggleFullScreen,
        key: "f",
        mods: MenuMods::CommandControl,
    },
    Separator,
    // The needs border and its pill (design §4, replacing Presence Band and
    // Presence Rim): checkable, on by default, persisted as `[presence] rim`.
    Item {
        label: "Show Needs in Windows",
        action: MenuAction::ToggleNeedsInWindows,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    // The stats bars' form (design §12.2): a radio of three, the Deck by
    // default, persisted as `[sys] stats_bars`; the Deck's size (Stats Deck
    // v2 §11.5), a radio of three, Automatic by default, persisted as
    // `[sys] stats_deck_size`; then the lens, ⌘I.
    Item {
        label: "Stats Bars: Deck",
        action: MenuAction::StatsBarsDeck,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Stats Bars: Sill",
        action: MenuAction::StatsBarsSill,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Stats Bars: Off",
        action: MenuAction::StatsBarsOff,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Deck Size: Automatic",
        action: MenuAction::DeckSizeAuto,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Deck Size: Full",
        action: MenuAction::DeckSizeFull,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Deck Size: Folded",
        action: MenuAction::DeckSizeFolded,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Stats Lens",
        action: MenuAction::ToggleStatsLens,
        key: "i",
        mods: MenuMods::Command,
    },
    Separator,
    Item {
        label: "Serious Mode",
        action: MenuAction::ToggleSeriousMode,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Matrix Rain",
        action: MenuAction::ToggleMatrixRain,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Favourite This Kitty",
        action: MenuAction::FavouriteKitty,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Next Kitty",
        action: MenuAction::NextKitty,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    // Settings at the /harness route (the Autopilot design, §1): the coding
    // agent harness's switches (Claude Code's, Codex's), Autopilot and what
    // it did — the bar twin of the Autopilot menu's `Harness settings…`,
    // named as the page is.
    Item {
        label: "Coding Agent Harness…",
        action: MenuAction::HarnessSettings,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    // The palette scoped to its Theme section: live preview on the arrow
    // keys, Enter keeps. No key equivalent; `choose_theme` binds one.
    Item {
        label: "Choose Theme…",
        action: MenuAction::ChooseTheme,
        key: "",
        mods: MenuMods::None,
    },
    // Settings at the /themes route: the Theme Showcase, every theme on a
    // live stage with its fonts and colors to tune.
    // The same palette scoped to the front tab: its own theme, nothing
    // written to aterm.toml. No key equivalent.
    Item {
        label: "Choose Tab Theme…",
        action: MenuAction::ChooseTabTheme,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Clear Tab Theme",
        action: MenuAction::ClearTabTheme,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Browse & Customize Themes…",
        action: MenuAction::BrowseThemes,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Command Palette…",
        action: MenuAction::OpenPalette,
        key: "p",
        mods: MenuMods::CommandShift,
    },
];

const WINDOW_MENU: &[MenuEntry] = &[
    Item {
        label: "Minimize",
        action: MenuAction::Minimize,
        key: "m",
        mods: MenuMods::Command,
    },
    Item {
        label: ZOOM_LABEL,
        action: MenuAction::Zoom,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    Item {
        label: "Show Next Tab",
        action: MenuAction::NextTab,
        key: "]",
        mods: MenuMods::CommandShift,
    },
    Item {
        label: "Show Previous Tab",
        action: MenuAction::PrevTab,
        key: "[",
        mods: MenuMods::CommandShift,
    },
    Separator,
    Item {
        label: "Move Tab to New Window",
        action: MenuAction::MoveTabToNewWindow,
        key: "n",
        mods: MenuMods::CommandShift,
    },
    Item {
        label: "Move Tab to Next Window",
        action: MenuAction::MoveTabToNextWindow,
        key: "m",
        mods: MenuMods::CommandShift,
    },
    Item {
        label: "Open Session in New Window",
        action: MenuAction::ViewSessionInNewWindow,
        key: "o",
        mods: MenuMods::CommandShift,
    },
    Separator,
    // No key equivalent: every free ⌘ letter near "rename" is the bar's or the shell's. Bind
    // `rename_session` in `[keybindings]` instead.
    Item {
        label: "Rename Session…",
        action: MenuAction::RenameSession,
        key: "",
        mods: MenuMods::None,
    },
];

const HELP_MENU: &[MenuEntry] = &[Item {
    label: "aterm Help",
    action: MenuAction::Help,
    key: "",
    mods: MenuMods::None,
}];

/// The top-level Version menu. Its live macOS title is `v<version>` ([`version_menu_bar_title`]);
/// the model says "Version" because it is serialised only off macOS and in tests, where a
/// runtime version would be nondeterministic. The one-click apply comes first (the palette
/// relabels it with the live version and drops it when nothing applies; [`update_version_menu`]
/// rebuilds the live NSMenu likewise and adds a staged-only "Update details…" row), then About.
const VERSION_MENU: &[MenuEntry] = &[
    Item {
        label: "↑ Install update now",
        action: MenuAction::ApplyUpdate,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "About aterm — build & version…",
        action: MenuAction::Version,
        key: "",
        mods: MenuMods::None,
    },
];

/// The Version menu's LIVE menu-bar title: `v<version>`, plus a trailing ⬆️ while
/// `attention` holds. The live caller ([`update_version_menu`]) sets `attention` for a
/// STAGED update ONLY — the always-visible bar badge means "an update is waiting, act
/// on it", so an apply that re-execs into that build clears it at once. The post-update
/// REALIZED celebration is NOT a bar badge; it lives in the menu's "Updated to aterm v…"
/// row and its palette twin (self-dismissing after
/// [`crate::relaunch_notice::REALIZED_ARROW_TTL`]). The color emoji is safe HERE:
/// AppKit renders NSMenu titles/items with the system font + Apple Color Emoji
/// fallback. In-window overlay surfaces (the palette rows, Windows' tab-strip
/// version label) must use plain `↑` instead — the own-rendered text stack has no
/// color-emoji face (verified coverage).
///
/// The text between the glyphs is [`version_signature`]'s FULL rung; Windows' tab
/// strip shows its quiet form ([`strip_version_label`]), and the bar alone adds
/// the colour glyphs AppKit can draw.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub(crate) fn version_menu_bar_title(attention: bool) -> String {
    let facts = VersionFacts::this_build();
    // A dev build leads with the wrench; a release does not.
    let wrench = if facts.release {
        ""
    } else {
        "\u{1F6E0}\u{FE0F} "
    };
    let signature = version_signature(facts, VersionRung::Full).unwrap_or_default();
    let arrow = if attention { " \u{2B06}\u{FE0F}" } else { "" };
    format!("{wrench}{signature}{arrow}")
}

/// How much of the version signature a surface has room for
/// ([`version_signature`]). The macOS menu bar always shows [`Self::Full`];
/// Windows' tab strip steps down to [`Self::Short`] before it hides the label
/// (`tab_bar::layout_segments_with_version`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub(crate) enum VersionRung {
    /// `v<version>` on a release; `v<maj.min>.<dev commits>+g<hash>[*] · DEV` on a
    /// dev build.
    Full,
    /// A dev build without its hash: `v<maj.min>.<dev commits>[*] · DEV`. A release
    /// has no short rung — `v<version>` is already as short as it is honest.
    Short,
}

/// The compile-time facts a version signature is composed from, stamped by `build.rs` and read
/// from [`crate::build_info`]. Separate from the constants so one test binary can compose every
/// kind of build (release, dev, dirty, no git).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct VersionFacts<'a> {
    /// A release lane produced the binary ([`crate::build_info::is_release_build`]).
    pub(crate) release: bool,
    /// The display version, `MAJOR.MINOR.0` ([`crate::build_info::version_display`]).
    pub(crate) version: &'a str,
    /// Commits since the newest release tag ([`crate::build_info::dev_commits`]).
    pub(crate) dev_commits: &'a str,
    /// The short commit, `-dirty` suffixed for a dirty tree
    /// ([`crate::build_info::git_commit`]).
    pub(crate) commit: &'a str,
}

impl VersionFacts<'static> {
    /// The running binary's own facts.
    #[must_use]
    pub(crate) fn this_build() -> Self {
        Self {
            release: crate::build_info::is_release_build(),
            version: crate::build_info::version_display(),
            dev_commits: crate::build_info::dev_commits(),
            commit: crate::build_info::git_commit(),
        }
    }
}

/// The version signature, without any glyph a surface adds: `v0.101.0` on a release,
/// `v0.101.1576+g25a5963 · DEV` on a dev build (`*` after the hash for a dirty tree), and on
/// the [`VersionRung::Short`] rung the dev form without its hash (`v0.101.1576 · DEV`). `None`
/// only for a release's short rung.
///
/// A release's third slot is always 0, so the dev counter there (commits since the newest
/// release tag) marks a non-release unambiguously. Display only: none of this reaches an update
/// comparison. The AppKit bar and the own-rendered face both cover U+00B7.
#[must_use]
pub(crate) fn version_signature(facts: VersionFacts<'_>, rung: VersionRung) -> Option<String> {
    if facts.release {
        return match rung {
            VersionRung::Full => Some(format!("v{}", facts.version)),
            VersionRung::Short => None,
        };
    }
    let majmin = facts
        .version
        .rsplit_once('.')
        .map_or(facts.version, |(mm, _)| mm);
    let star = if facts.commit.ends_with("-dirty") {
        "*"
    } else {
        ""
    };
    let counter = facts.dev_commits;
    Some(match rung {
        VersionRung::Full => {
            let short: String = facts.commit.chars().take(7).collect();
            format!("v{majmin}.{counter}+g{short}{star} \u{00B7} DEV")
        }
        VersionRung::Short => format!("v{majmin}.{counter}{star} \u{00B7} DEV"),
    })
}

/// The text of Windows' tab-strip version label (`tab_bar::TabHit::Version`): this
/// build's [`strip_version_text`] at `rung`, plus ` ↑` (a space and U+2191) while
/// `attention` holds — plain, because the own-rendered strip has no colour-emoji
/// face. `attention` is the bar's own staged-only rule ([`staged_for_version_menu`]),
/// so a realized update never lights it. `None` for a release's short rung.
#[must_use]
pub(crate) fn strip_version_label(rung: VersionRung, attention: bool) -> Option<String> {
    let text = strip_version_text(VersionFacts::this_build(), rung)?;
    Some(if attention {
        format!("{text} \u{2191}")
    } else {
        text
    })
}

/// The strip label's quiet form of the version: `v<maj.min>.<dev commits>` on a
/// dev build (`v0.106.3605`) and `v<maj.min>` on its short rung; a release's
/// `v<version>` and no short rung, as [`version_signature`].
///
/// The commit and a dirty tree's mark stay one click away: the label opens
/// Settings ▸ About ([`version_slot_click`]), which names the whole build.
#[must_use]
pub(crate) fn strip_version_text(facts: VersionFacts<'_>, rung: VersionRung) -> Option<String> {
    if facts.release {
        return version_signature(facts, rung);
    }
    let majmin = facts
        .version
        .rsplit_once('.')
        .map_or(facts.version, |(mm, _)| mm);
    Some(match rung {
        VersionRung::Full => format!("v{majmin}.{}", facts.dev_commits),
        VersionRung::Short => format!("v{majmin}"),
    })
}

/// What a click on the tab-strip version label fires: Software Update while an update is staged
/// and this platform has an updater ([`MenuAction::offered_on_this_platform`]), otherwise
/// [`MenuAction::Version`] (Settings ▸ About). Never an install: the strip's leading `↻` is the
/// one-click apply.
#[must_use]
pub(crate) fn version_slot_click(attention: bool) -> MenuAction {
    if attention && MenuAction::SoftwareUpdate.offered_on_this_platform() {
        MenuAction::SoftwareUpdate
    } else {
        MenuAction::Version
    }
}

/// The apply-now label for a staged build, given the arrow the calling surface can render
/// (`⬆️` in an NSMenu, `↑` in own-rendered text). The install is the overlap handoff, which
/// keeps every window, tab and live shell, so the row never asks for a restart.
///
/// [`crate::update_words::staged_build_name`] names the build number when a strictly newer build
/// carries the running one's display version (the updater orders by build number), so the row
/// never offers to update a build to itself. `trouble` (`App::apply_trouble_for`) replaces the
/// bare "now" with [`ApplyTrouble::row_tail`]: "install now" beside a build that already failed
/// to start is not credible. `on_hold` (`App::latched_activation_trial_spent`) reads "aterm vX is
/// on hold" instead of offering it; a press then opens the details.
#[must_use]
pub(crate) fn staged_apply_label(
    arrow: &str,
    build: u64,
    version: &str,
    trouble: Option<&ApplyTrouble>,
    on_hold: bool,
) -> String {
    let what = crate::update_words::staged_build_name(build, version);
    match (on_hold, trouble) {
        (true, Some(trouble)) => format!("{arrow} {what} is on hold — {}", trouble.row_tail()),
        (true, None) => format!(
            "{arrow} {what} is on hold — {}",
            crate::update_words::NEWER_RELEASE_REPLACES
        ),
        (false, None) => format!("{arrow} Install {what} now"),
        (false, Some(trouble)) => format!("{arrow} Install {what} — {}", trouble.row_tail()),
    }
}

/// The post-update row's words, the Version menu's and the palette's alike (`arrow` as for
/// [`staged_apply_label`]). No age: the row stays up for
/// [`crate::relaunch_notice::REALIZED_ARROW_TTL`] (ten minutes) unrewritten, so "just now" would
/// be false for most of its life.
#[must_use]
pub(crate) fn realized_row_label(arrow: &str, version: &str) -> String {
    format!("{arrow} Updated to aterm v{version}")
}

/// Whether the menu-bar Version arrow shows: for a staged update not on hold (no press may start
/// a held build), never for the post-update `realized` state. That lives on self-dismissing
/// surfaces (the menu's "Updated to aterm v…" row and its palette twin), so an apply that
/// re-execs into the staged build clears the badge the instant it lands.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub(crate) fn bar_title_attention(staged_present: bool, on_hold: bool, _realized: bool) -> bool {
    staged_present && !on_hold
}

/// The staged update the Version menu may offer, given the running build: only one strictly
/// newer. The updater orders builds by number, so a staged record naming this build or an older
/// one is a record the process outlived (the build it was handed off into, or one a newer
/// install overtook), and offering it would make a landed update look pending. Every surface
/// that draws the offer (`App::refresh_version_menu`) asks this first.
#[must_use]
pub(crate) fn staged_for_version_menu(
    staged: Option<(u64, &str)>,
    running_build: u64,
) -> Option<(u64, &str)> {
    staged.filter(|(build, _)| *build > running_build)
}

/// The Version title an outgoing process publishes for its successor once that
/// successor's proof is in: the successor's own plain `v<version>`,
/// no arrow — or `None` when this process cannot name it (a same-image relaunch
/// is not a new version; a staged record for another build is not this target).
///
/// If the outgoing parent quits while not frontmost and the successor's
/// launch-time activation is denied, the menu bar keeps the menu the parent
/// published last until the user clicks the successor; publishing the
/// successor's title on the outgoing side avoids leaving the stale arrow up
/// without stealing focus.
#[cfg(any(unix, test))]
#[must_use]
pub(crate) fn handoff_target_bar_title(
    target_build: u64,
    same_image: bool,
    staged: Option<(u64, &str)>,
) -> Option<String> {
    if same_image {
        return None;
    }
    let (build, version) = staged?;
    (build == target_build && !version.is_empty()).then(|| format!("v{version}"))
}

/// The whole menu bar, declaratively — the platform-neutral description the
/// cross-platform `chrome` introspection serialiser ([`menu_chrome_lines`]) renders so
/// the menu is introspectable on every platform (off macOS there is no `NSMenu` to read),
/// the palette flattens, and the macOS `install` builds its
/// `NSMenu`s from item-for-item (`macos::build_section`), so the model is the menu:
/// what `chrome` prints off macOS and what the live bar reads back agree by
/// construction, not by a second hand-kept list. Order is the standard Mac arrangement
/// (App / File / Edit / View / <the app's own: Agents> / Window / Help), the App section
/// titled with the app name by convention; the Version menu is the one section the
/// builder composes dynamically ([`macos::build_version_menu`], the documented
/// divergence — its rows depend on the update state).
///
/// A `#[test]` asserts every [`MenuAction`] appears here exactly once (the tab-context
/// copies and the [`PALETTE_ONLY`] rows excepted), submenus included.
pub(crate) const MENU_MODEL: &[MenuSection] = &[
    MenuSection {
        title: "aterm",
        entries: APP_MENU,
    },
    MenuSection {
        title: "File",
        entries: FILE_MENU,
    },
    MenuSection {
        title: "Edit",
        entries: EDIT_MENU,
    },
    MenuSection {
        title: "View",
        entries: VIEW_MENU,
    },
    MenuSection {
        title: "Agents",
        entries: AGENTS_MENU,
    },
    MenuSection {
        title: "Window",
        entries: WINDOW_MENU,
    },
    MenuSection {
        title: "Help",
        entries: HELP_MENU,
    },
    // Last, so `v<version>` is the rightmost item in the bar, a quiet trailing badge.
    MenuSection {
        title: "Version",
        entries: VERSION_MENU,
    },
];

/// Serialise [`MENU_MODEL`] to the `chrome` verb's menu lines — `menu "<title>": a, b, …`
/// of the non-separator item labels — byte-matching the macOS live-`NSMenu` reader in
/// `app_introspect::read_native_chrome`, so the cross-platform (off-macOS) `chrome`
/// reports the SAME logical menu the macOS bar shows.
///
/// A submenu prints twice, as both readers do: `<label> ▸` in its parent's line, then its own
/// line titled `"<parent> ▸ <label>"`. The Version menu's first row is dynamic: `apply_row` is
/// its live label ([`crate::palette::apply_row_label`]) and `None` leaves it out. ⌘W's title
/// follows `front_tab_split` ([`close_row_title`]).
#[cfg(any(not(target_os = "macos"), test))]
pub(crate) fn menu_chrome_lines(apply_row: Option<&str>, front_tab_split: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(MENU_MODEL.len() + 1);
    for section in MENU_MODEL {
        out.extend(chrome_lines_for(
            section.title,
            section.entries,
            apply_row,
            front_tab_split,
        ));
    }
    out
}

/// One section's line. Shared with nothing on macOS (the live reader walks the
/// `NSMenu`), but the SHAPE it prints is the contract that reader byte-matches.
#[cfg(any(not(target_os = "macos"), test))]
fn chrome_lines_for(
    title: &str,
    entries: &[MenuEntry],
    apply_row: Option<&str>,
    front_tab_split: bool,
) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    let mut submenus = Vec::new();
    for e in entries {
        match e {
            // A row this platform cannot act on is not reported (`offered_on_this_platform`).
            MenuEntry::Item { action, .. } if !action.offered_on_this_platform() => {}
            // The dynamic update row: its live label, or nothing.
            MenuEntry::Item {
                action: MenuAction::ApplyUpdate,
                ..
            } => labels.extend(apply_row.map(str::to_string)),
            // ⌘W's row names what it closes now.
            MenuEntry::Item {
                action: MenuAction::CloseTab,
                ..
            } => labels.push(close_row_title(front_tab_split).to_string()),
            MenuEntry::Item { label, .. } => labels.push((*label).to_string()),
            MenuEntry::Submenu { label, entries } => {
                labels.push(format!("{label} ▸"));
                submenus.extend(chrome_lines_for(
                    &format!("{title} ▸ {label}"),
                    entries,
                    apply_row,
                    front_tab_split,
                ));
            }
            MenuEntry::Separator => {}
        }
    }
    let mut lines = vec![format!("menu {title:?}: {}", labels.join(", "))];
    lines.extend(submenus);
    lines
}

#[cfg(target_os = "macos")]
pub(crate) use macos::{
    MenuHandle, choose_local_file, choose_local_image, confirm, confirm_owner,
    defer_quit_for_terminate, install, notify, ok_alert, open_file_in_workspace, open_help_url,
    order_front_regardless, reveal_file_in_workspace, show_handoff_target_version,
    update_version_menu,
};

/// Whether this process was launched `--headless`, set once by `main_entry`
/// ([`mark_process_headless`]). A headless process has no application object, so a modal
/// (`choose_local_file`, `confirm`, `notify`) would park the main thread in a nested run loop
/// nothing can see or dismiss, and an `NSWorkspace` open would steal focus for a process with
/// no glass. Process-wide because its readers are free functions.
static PROCESS_HEADLESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that this process is headless. Called once, by `main_entry`.
pub(crate) fn mark_process_headless() {
    PROCESS_HEADLESS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether this process is headless, for a presenter that refuses quietly: the
/// desktop-notification delivery (`notify::deliver`), whose refusal would
/// otherwise print one stderr line per program notification, and the macOS
/// notice center (`notice_center`), which a headless process never opens.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
pub(crate) fn process_is_headless() -> bool {
    PROCESS_HEADLESS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether a helper about to present OS UI (a modal, or an open that fronts
/// another app) must refuse because this process is headless — saying so on
/// stderr, which is the only surface a headless instance has. Every such
/// helper in the macOS module asks this FIRST.
#[cfg(target_os = "macos")]
pub(crate) fn os_ui_refused(what: &str) -> bool {
    let refused = PROCESS_HEADLESS.load(std::sync::atomic::Ordering::Relaxed);
    if refused {
        crate::logging::stderr_line!(
            "aterm-gui: {what} needs a window; a headless instance presents no OS UI"
        );
    }
    refused
}

/// Off macOS there is no NSWorkspace, and "Open Log" spawns nothing in its place: the
/// Settings page names the file's path in the log's feedback instead. `false`.
#[cfg(not(target_os = "macos"))]
pub(crate) fn open_file_in_workspace(_path: &std::path::Path) -> bool {
    false
}

/// Off macOS there is no Finder to select a file in: `false`, and the Manual
/// page names the file's path instead.
#[cfg(not(target_os = "macos"))]
pub(crate) fn reveal_file_in_workspace(_path: &std::path::Path) -> bool {
    false
}

/// Non-macOS no-op handle: there is no platform menu off macOS. Held by `App` in
/// the same field on every target so the struct shape is platform-independent.
#[cfg(not(target_os = "macos"))]
pub(crate) type MenuHandle = ();

/// Non-macOS stub: there is no platform menu bar, so nothing is installed (`None`). The shared
/// signature keeps `AppRt::install_menu` identical on every target.
#[cfg(not(target_os = "macos"))]
pub(crate) fn install(
    _proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
) -> Option<MenuHandle> {
    None
}

/// Non-macOS stub: no native menu bar to retitle for a successor.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn show_handoff_target_version(_handle: &MenuHandle, _title: &str) {}

/// Non-macOS stub: no native menu bar, so there is no Version menu to retitle/rebuild.
/// The palette's Version-section rows are the cross-platform mirror of this state,
/// and, on Windows, the tab strip's trailing version label ([`strip_version_label`]).
#[cfg(not(target_os = "macos"))]
pub(crate) fn update_version_menu(
    _handle: &MenuHandle,
    _staged: Option<(u64, &str)>,
    _trouble: Option<&ApplyTrouble>,
    _on_hold: bool,
    _realized: bool,
) {
}

/// Windows: the shell's own Common Item Dialog (`IFileOpenDialog`), with the
/// macOS panel's exact semantics — one existing local file, aliases resolved, no
/// type restriction, cancel is `None`. See [`crate::file_picker_win`].
#[cfg(windows)]
pub(crate) fn choose_local_file(title: &str, prompt: &str) -> Option<std::path::PathBuf> {
    crate::file_picker_win::choose(title, prompt)
}

/// Windows: [`choose_local_file`] offering only image files.
#[cfg(windows)]
pub(crate) fn choose_local_image(title: &str, prompt: &str) -> Option<std::path::PathBuf> {
    crate::file_picker_win::choose_image(title, prompt)
}

/// No portable native picker is linked on the REMAINING targets (Linux). The
/// global action remains visible in the cross-platform command palette, but
/// acquires no filesystem authority there — and every surface that would spend
/// one asks [`local_file_picker_available`] first, so the row greys out instead
/// of accepting a dead click.
#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn choose_local_file(_title: &str, _prompt: &str) -> Option<std::path::PathBuf> {
    None
}

/// No picker here either; see [`choose_local_file`].
#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn choose_local_image(_title: &str, _prompt: &str) -> Option<std::path::PathBuf> {
    None
}

/// Whether [`choose_local_file`] can actually produce a path here — the one
/// predicate every picker-backed surface gates on (the palette's File rows, and
/// Settings ▸ Wallpaper's "Choose Image…"), so a platform gains or loses the rows and
/// the button together.
///
/// Windows answers with a live probe rather than a `cfg!` — see
/// [`crate::file_picker_win::available`].
#[cfg(target_os = "macos")]
pub(crate) fn local_file_picker_available() -> bool {
    true
}

#[cfg(windows)]
pub(crate) fn local_file_picker_available() -> bool {
    crate::file_picker_win::available()
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn local_file_picker_available() -> bool {
    false
}

/// Windows: a real modal alert. A GUI-subsystem launch has no stderr, so without it a failure
/// routed through `notify` (a rejected document path, an update-check result) reaches no one.
#[cfg(windows)]
pub(crate) fn notify(title: &str, body: &str) {
    crate::win_alert_ok(title, body);
}

/// Linux has no native alert wired: the caller's own `eprintln!` is the channel, and a Linux
/// GUI launch keeps its stderr.
#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn notify(_title: &str, _body: &str) {}

/// Help, off macOS: the project page in the default browser, through the helper a Ctrl-clicked
/// link uses (`open_url_external`: `ShellExecuteW` on Windows, `xdg-open` on Linux). No other
/// target has the `.app`'s bundled `Help.html`.
#[cfg(not(target_os = "macos"))]
pub(crate) fn open_help_url() {
    crate::app_mouse::open_url_external(&help_url());
}

/// The project page — Help's destination when no bundled guide is reachable (every non-macOS
/// target, and macOS outside the `.app`). `is_safe_url`-clean because `open_url_external`
/// trusts its input. The public channel the updater reads
/// ([`aterm_update_core::DEFAULT_OWNER`] / [`aterm_update_core::DEFAULT_REPO`]), not
/// `[workspace.package] repository`, which is the private staging repo.
fn help_url() -> String {
    format!(
        "https://github.com/{}/{}",
        aterm_update_core::DEFAULT_OWNER,
        aterm_update_core::DEFAULT_REPO
    )
}

// ---------------------------------------------------------------------------
// System Settings deep links — the macOS privacy panes
// ---------------------------------------------------------------------------

/// Which section of Privacy & Security a deep link aims at. Only registered anchors in the
/// Settings extension's `searchTerms` index are spellable; the per-folder strings in its Mach-O
/// (`Privacy_DocumentsFolder` and siblings) name sub-rows of the one Files & Folders section,
/// not anchors, so they are not offered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PrivacyPane {
    /// Full Disk Access — the single grant macOS offers for this class. Which folders it covers
    /// is unmeasured, so neither this module nor its callers may name one.
    FullDiskAccess,
    /// The per-app Files & Folders list — the repair route, where an already-answered folder
    /// row can be turned back on.
    FilesAndFolders,
    /// The Developer Tools list (`kTCCServiceDeveloperTool`): apps allowed to "run software
    /// locally that does not meet the system's security policy". A human adds an app there
    /// with `+`; aterm only opens the page. `Privacy_DevTools` is a registered anchor. On
    /// macOS 26 the page also holds the Processor Trace switch, a different setting.
    DeveloperTools,
}

impl PrivacyPane {
    /// Every pane, for the tests that hold each to the same rules.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 3] = [
        Self::FullDiskAccess,
        Self::FilesAndFolders,
        Self::DeveloperTools,
    ];
}

/// Full Disk Access, through the modern Settings extension. The scheme is
/// `x-apple.systempreferences`, with a dot; the hyphenated spelling is a different string, the
/// one `crate::is_safe_url`'s rejection test names, so [`open_privacy_settings`] does not borrow
/// the link allowlist.
#[cfg(any(target_os = "macos", test))]
pub(crate) const SETTINGS_FULL_DISK_ACCESS: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles";

/// Full Disk Access, through the legacy pane id the extension still declares
/// for itself (`legacyBundleIdentifier`). Tried only if the modern id is
/// refused outright.
#[cfg(any(target_os = "macos", test))]
const SETTINGS_FULL_DISK_ACCESS_LEGACY: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// The per-app Files & Folders list, through the modern Settings extension.
#[cfg(any(target_os = "macos", test))]
pub(crate) const SETTINGS_FILES_AND_FOLDERS: &str = "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_FilesAndFolders";

/// Files & Folders, through the legacy pane id.
#[cfg(any(target_os = "macos", test))]
const SETTINGS_FILES_AND_FOLDERS_LEGACY: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders";

/// The Developer Tools list, through the modern Settings extension (the consent module's
/// spelling).
#[cfg(any(target_os = "macos", test))]
pub(crate) const SETTINGS_DEVELOPER_TOOLS: &str = aterm_containment::consent::DEVTOOLS_SETTINGS_URL;

/// The Developer Tools list, through the legacy pane id (the consent module's spelling).
#[cfg(any(target_os = "macos", test))]
const SETTINGS_DEVELOPER_TOOLS_LEGACY: &str =
    aterm_containment::consent::DEVTOOLS_SETTINGS_URL_LEGACY;

/// The Privacy & Security page with no anchor (no `?`), the degradation target: always the last
/// of [`privacy_settings_urls`] (`the_last_candidate_is_the_unanchored_pane_root`).
#[cfg(any(target_os = "macos", test))]
const SETTINGS_PRIVACY_ROOT: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension";

/// Every URL that may be tried for `pane`, best first: the modern extension id, the legacy
/// alias, then the unanchored pane root. None is Apple-documented; the root keeps this from
/// being a dead end, and the caller shows [`privacy_settings_path_words`] for the rest.
#[cfg(any(target_os = "macos", test))]
pub(crate) const fn privacy_settings_urls(pane: PrivacyPane) -> [&'static str; 3] {
    match pane {
        PrivacyPane::FullDiskAccess => [
            SETTINGS_FULL_DISK_ACCESS,
            SETTINGS_FULL_DISK_ACCESS_LEGACY,
            SETTINGS_PRIVACY_ROOT,
        ],
        PrivacyPane::FilesAndFolders => [
            SETTINGS_FILES_AND_FOLDERS,
            SETTINGS_FILES_AND_FOLDERS_LEGACY,
            SETTINGS_PRIVACY_ROOT,
        ],
        PrivacyPane::DeveloperTools => [
            SETTINGS_DEVELOPER_TOOLS,
            SETTINGS_DEVELOPER_TOOLS_LEGACY,
            SETTINGS_PRIVACY_ROOT,
        ],
    }
}

/// The route to `pane` in words, for when the anchor did not land the person on the section.
/// Always offered: `openURL:` accepting a URL means System Settings took it, not that it
/// scrolled to the row. Names the pane only — no folder, no claim about what a grant covers.
pub(crate) const fn privacy_settings_path_words(pane: PrivacyPane) -> &'static str {
    match pane {
        PrivacyPane::FullDiskAccess => {
            "System Settings \u{25b8} Privacy & Security \u{25b8} Full Disk Access"
        }
        PrivacyPane::FilesAndFolders => {
            "System Settings \u{25b8} Privacy & Security \u{25b8} Files & Folders"
        }
        PrivacyPane::DeveloperTools => aterm_containment::consent::DEVTOOLS_SETTINGS_WORDS,
    }
}

/// What [`macos::open_privacy_settings`] achieved. On anything but [`Self::Anchored`] the words
/// are the person's only route, so the caller must say them, not just log.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SettingsOpen {
    /// An anchored URL was accepted. System Settings opened; whether it
    /// scrolled to the section is not observable from here, which is why the
    /// second open exists and why the words stay on screen.
    Anchored,
    /// Both anchors were refused; the un-anchored Privacy & Security page
    /// opened instead. The caller must show [`privacy_settings_path_words`].
    #[cfg(target_os = "macos")]
    PaneRoot,
    /// Nothing opened — off the main thread, off macOS, or the shell refused
    /// every candidate. The caller must show the words and must not claim a
    /// pane is up.
    Refused,
}

impl SettingsOpen {
    /// Whether System Settings opened at all (anchored or at the pane root).
    pub(crate) const fn opened(self) -> bool {
        !matches!(self, Self::Refused)
    }
}

/// Open System Settings at `pane` — the owner-gesture entry point the Security
/// page presses, the Developer Tools hand-over's opener (`devtools_helper::Reach::live`), which the
/// window's own lane and `aterm ctl devtools hand-over` reach at a quiet
/// moment, and the Full Disk Access hand-over's opener
/// (`devtools_helper::FdaReach::live`), which only the window's own launch
/// check reaches at a quiet moment.
///
/// macOS does the work in [`macos::open_privacy_settings`], which documents why
/// a Settings URL may be opened from here and never from a hyperlink. Off macOS
/// there is no System Settings and no TCC, so nothing opens and the answer is
/// [`SettingsOpen::Refused`] — the same instruction to the caller as a refused
/// anchor: show the route in words.
///
/// A degraded outcome is logged with that route, because a windowed launch has
/// no stderr and "the button did nothing visible" is otherwise unexplainable
/// after the fact. The caller still has to show the words; the log is the
/// diagnostic copy, not the user-facing one.
pub(crate) fn open_privacy_settings(pane: PrivacyPane) -> SettingsOpen {
    #[cfg(target_os = "macos")]
    let outcome = macos::open_privacy_settings(pane);
    #[cfg(not(target_os = "macos"))]
    let outcome = SettingsOpen::Refused;
    if outcome != SettingsOpen::Anchored {
        aterm_log::warn!(
            "System Settings deep link did not take ({outcome:?}); the route is {}",
            privacy_settings_path_words(pane)
        );
    }
    outcome
}

#[cfg(target_os = "macos")]
mod macos {
    use aterm_objc::{Id, Obj, Retained, Sel, autoreleasepool, class, sel};
    use aterm_window::event_loop::EventLoopProxy;

    use crate::appkit::consts::{
        NS_ALERT_FIRST_BUTTON_RETURN, NS_ALERT_SECOND_BUTTON_RETURN,
        NS_EVENT_MODIFIER_FLAG_COMMAND, NS_EVENT_MODIFIER_FLAG_CONTROL,
        NS_EVENT_MODIFIER_FLAG_SHIFT, NS_MODAL_RESPONSE_OK,
    };
    use crate::appkit::{self, MainThread};

    use super::{
        MENU_MODEL, MenuAction, NativeTerminateDecision, PrivacyPane, SettingsOpen,
        privacy_settings_urls,
    };
    use crate::Wake;

    /// Installed before the AppKit terminate hook. `EventLoopProxy` is the only
    /// cross-thread/callback capability retained here; all document/application
    /// state remains on the event-loop thread.
    static TERMINATE_PROXY: std::sync::OnceLock<EventLoopProxy<Wake>> = std::sync::OnceLock::new();

    /// What [`install`] returns: the retained action target and the Version menu's top-level
    /// `NSMenuItem`, which [`update_version_menu`] retitles and rebuilds as an update stages,
    /// realizes or expires. AppKit holds item targets weakly, so `App` keeps this for the
    /// process lifetime. `()` off macOS.
    pub(crate) struct MenuHandle {
        /// The single `menuAction:` relay target every item is wired to.
        target: Retained<MenuTarget>,
        /// The Version menu's top-level bar item (title `v<version>[ ⬆️]`), whose
        /// submenu [`update_version_menu`] rebuilds on update-state transitions.
        version_item: Obj,
    }

    aterm_objc::declare_class! {
        /// The target of every menu item: relays `menuAction:` (the sender's `tag`, decoded to a
        /// [`MenuAction`]) as [`Wake::MenuAction`] for `App` to dispatch on the loop's next turn.
        /// No menu logic lives here. `pub(crate)` only so `MenuHandle` and `install`'s return
        /// type are not more private than it; [`aterm_objc::Retained`] is `!Send` and
        /// instantiation takes a [`MainThread`] witness.
        pub(crate) struct MenuTarget: NSObject {
            const NAME: &str = "ATermMenuTarget";
            type Ivars = EventLoopProxy<Wake>;

            /// `menuAction:`, wired to every item: `sender` is the clicked `NSMenuItem`, whose
            /// `tag` decodes to the action. A tag that does not decode is ignored.
            @sel(menuAction:)
            fn menu_action(&self, sender: Id) {
                if sender.is_null() {
                    return;
                }
                // SAFETY: `sender` is the live NSMenuItem AppKit passed as the
                // action sender; `-tag` is `-(NSInteger)` with no side effects.
                let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                if let Some(action) = MenuAction::from_tag(tag) {
                    // Fire-and-forget: a closed loop (app shutting down) just
                    // drops the event — mirrors every other `send_event` here.
                    let _ = crate::tab_spawn::post_native_menu_action(self.ivars(), action);
                }
            }

            /// AppKit asks this just before displaying or dispatching an item. Terminal-only
            /// commands grey out over a native whole tab.
            ///
            /// Returns [`aterm_objc::Bool`], not `bool`: on the `x86_64-apple-darwin` slice
            /// `BOOL` is `signed char`, so `bool` is the wrong ABI type.
            @sel(validateMenuItem:)
            fn validate_menu_item(&self, sender: Id) -> aterm_objc::Bool {
                if sender.is_null() {
                    return aterm_objc::Bool::NO;
                }
                // SAFETY: AppKit supplied a live NSMenuItem; reading its integer tag
                // has no side effects. Unknown/untagged items fail closed.
                let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                let Some(action) = MenuAction::from_tag(tag) else {
                    return aterm_objc::Bool::NO;
                };
                // A CHECKABLE row (View ▸ Show Needs in Windows, Serious Mode,
                // Matrix Rain, Favourite This Kitty) shows the live
                // state as its checkmark, stamped here because AppKit asks at
                // exactly the moment the menu opens — the same live-projection
                // rule the enabled bit follows. `-setState:` is
                // `-(void)(NSControlStateValue)`, an NSInteger: 1 on, 0 off.
                if let Some(on) = super::native_menu_checked(action) {
                    // SAFETY: a plain setter on the live NSMenuItem AppKit
                    // passed, main thread, no preconditions.
                    unsafe { appkit::send_v_isize(sender, sel!(setState:), isize::from(on)) };
                }
                // The full-screen row names what it does now, as its palette row does.
                if matches!(action, MenuAction::ToggleFullScreen) {
                    stamp_full_screen_title(sender);
                }
                // So does ⌘W's: Close Pane on a split tab, Close Tab otherwise
                // (and Summarize names the fleet or this Mac).
                if let Some(title) = super::native_live_title(action) {
                    set_item_title(sender, title);
                }
                // Summarize's tool tip says why it cannot start, when it cannot.
                if matches!(action, MenuAction::SummarizeFleet) {
                    let tip = super::native_item_tip(action)
                        .unwrap_or_else(|| action.help().to_string());
                    if let Some(tip) = appkit::nsstring(&tip) {
                        // SAFETY: `-setToolTip:` is `-(void)(NSString *)` on the
                        // live NSMenuItem AppKit passed; it COPIES its argument.
                        unsafe { appkit::send_v_id(sender, sel!(setToolTip:), tip.id()) };
                    }
                }
                aterm_objc::Bool::new(super::native_menu_action_enabled(action))
            }
        }
    }

    /// Retitle the full-screen item for the main window's state
    /// (`NSWindowStyleMaskFullScreen`, `1 << 14`): `Exit Full Screen` while it
    /// is full screen, `Enter Full Screen` otherwise or with no main window.
    fn stamp_full_screen_title(item: Id) {
        // Only items of the bar `install` built reach here, and it builds one
        // only windowed; the proof keeps a headless `+sharedApplication` out
        // all the same.
        let Some(_windowed) = crate::platform::Windowed::get() else {
            return;
        };
        // SAFETY: `+sharedApplication` and `-mainWindow` are `-(id)` accessors
        // read on the main thread (AppKit calls `validateMenuItem:` there);
        // `-styleMask` is `-(NSWindowStyleMask)`, an `NSUInteger`, sent only to
        // a non-nil window.
        let fullscreen = unsafe {
            let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
            let window = if app.is_null() {
                app
            } else {
                appkit::send_id(app, sel!(mainWindow))
            };
            !window.is_null() && appkit::send_usize(window, sel!(styleMask)) & (1usize << 14) != 0
        };
        set_item_title(item, super::full_screen_title(fullscreen));
    }

    /// Retitle a live menu item (`validateMenuItem:`'s live titles).
    fn set_item_title(item: Id, title: &str) {
        if let Some(title) = appkit::nsstring(title) {
            // SAFETY: `-setTitle:` is `-(void)(NSString *)` on the live
            // NSMenuItem the caller holds; it COPIES its argument.
            unsafe { appkit::send_v_id(item, sel!(setTitle:), title.id()) };
        }
    }

    /// Build aterm's menu bar and install it as the shared application's main
    /// menu. Returns the retained action [`MenuTarget`] for the caller to keep
    /// alive (AppKit holds menu-item targets only weakly). Called from `resumed`
    /// after the window exists and only when NOT headless.
    ///
    /// Best-effort: if we are somehow off the main thread (`MainThread::new`
    /// is `None`) the menu is simply not installed — never a panic. The winit
    /// event loop always runs `resumed` on the main thread, so in practice the
    /// marker is always present.
    pub(crate) fn install(proxy: &EventLoopProxy<Wake>) -> Option<MenuHandle> {
        // A menu bar is a windowed process's: headless, the `-setMainMenu:`
        // below would first CREATE the application object.
        let _windowed = crate::platform::Windowed::get()?;
        let main_thread = MainThread::new()?;
        let _ = TERMINATE_PROXY.set(proxy.clone());
        let target = MenuTarget::alloc_init(main_thread, proxy.clone())?;

        let main = new_menu()?;

        // Each section is built from `MENU_MODEL`, in its order; the Version section's rows are
        // dynamic, so it is built below instead.
        for section in MENU_MODEL {
            if section.title == "Version" {
                continue;
            }
            let _ = attach_submenu(
                &main,
                section.title,
                build_section(&target, section.entries)?,
            );
        }
        // The Version menu goes last (rightmost), installed plain; `App::refresh_version_menu`
        // retitles it through the retained item. A bare top-level item with an action greys out
        // in the main menu bar, so its commands live in this submenu.
        let version_item = attach_submenu(
            &main,
            &super::version_menu_bar_title(false),
            build_version_menu(&target, None, None, false, false)?,
        )?;

        // SAFETY: `+sharedApplication` is `-(id)` and is the main-thread AppKit
        // singleton accessor (`_main_thread` is the witness); `-setMainMenu:` is
        // `-(void)(NSMenu *)` and RETAINS the menu, which is why `main` may drop
        // at the end of this frame.
        unsafe {
            let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
            if app.is_null() {
                return None;
            }
            appkit::send_v_id(app, sel!(setMainMenu:), main.id());
        }
        Some(MenuHandle {
            target,
            version_item,
        })
    }

    /// Re-sync the live Version menu (title and items) to the update state, from
    /// `App::refresh_version_menu` (on `Wake::UpdateStaged`, the post-update boot and the
    /// realized-arrow TTL sweep). `staged` is the strictly newer `(build, version)` ready to
    /// apply; `realized` marks the freshly updated window. Rebuilding the submenu, not toggling
    /// hidden flags, keeps the items an exact function of the state. The bar arrow tracks
    /// `staged` only ([`super::bar_title_attention`]). Off the main thread it is a no-op.
    pub(crate) fn update_version_menu(
        handle: &MenuHandle,
        staged: Option<(u64, &str)>,
        trouble: Option<&super::ApplyTrouble>,
        on_hold: bool,
        realized: bool,
    ) {
        let Some(_main_thread) = MainThread::new() else {
            return;
        };
        let title = super::version_menu_bar_title(super::bar_title_attention(
            staged.is_some(),
            on_hold,
            realized,
        ));
        let Some(submenu) = build_version_menu(&handle.target, staged, trouble, on_hold, realized)
        else {
            return;
        };
        set_version_menu(&_main_thread, handle, &title, &submenu);
    }

    /// The outgoing side of a seamless handoff: retitle this process's Version item to the
    /// successor's plain `title` (`super::handoff_target_bar_title`) over the About-only
    /// submenu, so the bar is right even if no app claims it after this process exits. Called
    /// from the successor's ProofReady, which leaves the ≥30 ms dispatch fence for AppKit to
    /// publish it before Commit; a returned attempt re-syncs the item
    /// (`App::refresh_version_menu`). Steals no focus.
    pub fn show_handoff_target_version(handle: &MenuHandle, title: &str) {
        let Some(_main_thread) = MainThread::new() else {
            return;
        };
        let Some(submenu) = build_version_menu(&handle.target, None, None, false, false) else {
            return;
        };
        set_version_menu(&_main_thread, handle, title, &submenu);
    }

    /// Put `title` and `submenu` on the Version bar item (`_main_thread` is the
    /// witness that this runs where AppKit requires).
    fn set_version_menu(
        _main_thread: &MainThread,
        handle: &MenuHandle,
        title: &str,
        submenu: &Obj,
    ) {
        let Some(ns_title) = appkit::nsstring(title) else {
            return;
        };
        // Set the title on both the bar item and the submenu: which one AppKit takes for a
        // top-level title has varied by toolkit version.
        // SAFETY: `-setTitle:` is `-(void)(NSString *)` and `-setSubmenu:` is
        // `-(void)(NSMenu *)`, both plain main-thread setters on live objects
        // that COPY/RETAIN their argument, so the +1 title and the submenu may
        // drop at the end of this frame.
        unsafe {
            appkit::send_v_id(submenu.id(), sel!(setTitle:), ns_title.id());
            appkit::send_v_id(handle.version_item.id(), sel!(setTitle:), ns_title.id());
            appkit::send_v_id(handle.version_item.id(), sel!(setSubmenu:), submenu.id());
        }
    }

    /// The Version submenu for the given update state (the palette rewrites
    /// [`super::VERSION_MENU`]'s apply row the same way):
    ///   * staged: "⬆️ Install aterm v<staged> now" (one click installs in place), About, then
    ///     "Update details…". A build on hold reads "⬆️ aterm v<staged> is on hold — …" and its
    ///     click opens the details ([`super::staged_apply_label`]).
    ///   * realized (fresh post-update, nothing staged): "⬆️ Updated to aterm v<current>"
    ///     (fires About), then About.
    ///   * neither: just About.
    fn build_version_menu(
        target: &MenuTarget,
        staged: Option<(u64, &str)>,
        trouble: Option<&super::ApplyTrouble>,
        on_hold: bool,
        realized: bool,
    ) -> Option<Obj> {
        let menu = new_menu()?;
        if let Some((build, version)) = staged {
            add_item(
                &menu,
                target,
                &super::staged_apply_label("\u{2B06}\u{FE0F}", build, version, trouble, on_hold),
                MenuAction::ApplyUpdate,
                "",
                false,
            );
            add_separator(&menu);
        } else if realized {
            add_item(
                &menu,
                target,
                &super::realized_row_label(
                    "\u{2B06}\u{FE0F}",
                    crate::build_info::version_display(),
                ),
                MenuAction::Version,
                "",
                false,
            );
            add_separator(&menu);
        }
        add_item(
            &menu,
            target,
            "About aterm — build & version…",
            MenuAction::Version,
            "",
            false,
        );
        if staged.is_some() {
            // The details surface, one row under the one-click apply — mirrors the App
            // menu's "Check for Updates…" (same SoftwareUpdate action, staged-only here).
            add_item(
                &menu,
                target,
                "Update details…",
                MenuAction::SoftwareUpdate,
                "",
                false,
            );
        }
        Some(menu)
    }

    /// Build one menu from the model's entries: every [`MenuEntry::Item`] as an
    /// item wired to `menuAction:` with the model's key equivalent and modifier
    /// mask and the action's [`MenuAction::help`] as its tool tip, every
    /// separator as a separator. The one builder for
    /// every static section — so the live bar the `chrome`
    /// verb reads back on macOS and the model `menu_chrome_lines` prints off
    /// it are the same tree by construction.
    fn build_section(target: &MenuTarget, entries: &[super::MenuEntry]) -> Option<Obj> {
        let menu = new_menu()?;
        for entry in entries {
            match entry {
                super::MenuEntry::Item {
                    label,
                    action,
                    key,
                    mods,
                } => add_item_mods(&menu, target, label, *action, key, mods_mask(*mods)),
                super::MenuEntry::Submenu { label, entries } => {
                    attach_submenu(&menu, label, build_section(target, entries)?)?;
                }
                super::MenuEntry::Separator => add_separator(&menu),
            }
        }
        Some(menu)
    }

    /// The model's platform-neutral modifier onto the AppKit mask.
    fn mods_mask(mods: super::MenuMods) -> usize {
        match mods {
            super::MenuMods::None => 0,
            super::MenuMods::Command => command_mask(),
            super::MenuMods::CommandShift => command_shift_mask(),
            super::MenuMods::CommandControl => command_control_mask(),
        }
    }

    /// `Cmd` modifier mask (the default for a single-letter key equivalent).
    fn command_mask() -> usize {
        NS_EVENT_MODIFIER_FLAG_COMMAND
    }

    /// `Cmd-Ctrl` mask (Enter Full Screen's ⌃⌘F).
    fn command_control_mask() -> usize {
        NS_EVENT_MODIFIER_FLAG_COMMAND | NS_EVENT_MODIFIER_FLAG_CONTROL
    }

    /// `Cmd-Shift` mask (Move Tab to New Window's ⇧⌘N).
    fn command_shift_mask() -> usize {
        NS_EVENT_MODIFIER_FLAG_COMMAND | NS_EVENT_MODIFIER_FLAG_SHIFT
    }

    /// `[[NSMenu alloc] init]`, +1, or `None` if the allocation failed.
    fn new_menu() -> Option<Obj> {
        // SAFETY: `+alloc` gives a +1 uninitialised NSMenu and `-init` consumes
        // it, so `Obj::from_owned` adopts exactly one +1.
        unsafe { Obj::from_owned(appkit::send_id(appkit::alloc(class(c"NSMenu")), sel!(init))) }
    }

    /// `[[NSMenuItem alloc] initWithTitle:action:keyEquivalent:]`, +1. `action`
    /// is [`Sel::NULL`] for an item that only holds a submenu.
    fn new_item(title: &str, action: Sel, key: &str) -> Option<Obj> {
        let title = appkit::nsstring(title)?;
        let key = appkit::nsstring(key)?;
        // SAFETY: NSMenuItem's designated initializer,
        // `-(id)(NSString *, SEL, NSString *)`; a nil SEL is its documented
        // "no action" value. `+alloc` is +1 and the initializer consumes it.
        // Both strings are live +1 NSStrings the initializer copies.
        unsafe {
            Obj::from_owned(appkit::send_id_id_sel_id(
                appkit::alloc(class(c"NSMenuItem")),
                sel!(initWithTitle:action:keyEquivalent:),
                title.id(),
                action,
                key.id(),
            ))
        }
    }

    /// Build one menu item wired to `menuAction:` on `target`, tagged with `action`, and append
    /// it to `menu`. `key` is the lowercase key-equivalent character ("" for none); `cmd` adds
    /// ⌘. The equivalent is visual only: `App::on_key` handles the keystroke.
    fn add_item(
        menu: &Obj,
        target: &MenuTarget,
        title: &str,
        action: MenuAction,
        key: &str,
        cmd: bool,
    ) {
        let mods = if cmd { command_mask() } else { 0 };
        add_item_mods(menu, target, title, action, key, mods);
    }

    /// As [`add_item`] but with an explicit modifier mask (for non-⌘ equivalents
    /// like Enter Full Screen's ⌃⌘F).
    fn add_item_mods(
        menu: &Obj,
        target: &MenuTarget,
        title: &str,
        action: MenuAction,
        key: &str,
        mods: usize,
    ) {
        let Some(item) = new_item(title, sel!(menuAction:), key) else {
            return;
        };
        // The help sentence is the tool tip, which VoiceOver reads as the item's help.
        let tip = appkit::nsstring(action.help());
        // SAFETY: plain setters on a fresh NSMenuItem, then `-addItem:` on the
        // live menu. `-setTarget:` is `-(void)(id)` and holds the target WEAKLY
        // (which is why `MenuHandle` retains it), `-setTag:` is
        // `-(void)(NSInteger)`, `-setToolTip:` is `-(void)(NSString *)` and
        // COPIES its argument, and `-setKeyEquivalentModifierMask:` is
        // `-(void)(NSEventModifierFlags)`, an `NSUInteger` bitmask.
        unsafe {
            appkit::send_v_id(item.id(), sel!(setTarget:), target.as_id());
            appkit::send_v_isize(item.id(), sel!(setTag:), action.tag());
            if let Some(tip) = tip {
                appkit::send_v_id(item.id(), sel!(setToolTip:), tip.id());
            }
            if !key.is_empty() {
                appkit::send_v_usize(item.id(), sel!(setKeyEquivalentModifierMask:), mods);
            }
            appkit::send_v_id(menu.id(), sel!(addItem:), item.id());
        }
    }

    /// Append a separator line to `menu`.
    fn add_separator(menu: &Obj) {
        // SAFETY: `+separatorItem` is `-(id)` and returns a shared, AUTORELEASED
        // item — borrowed for the length of `-addItem:`, which retains it into
        // the menu. The pool it lives in is the caller's; every path into this
        // module opens one.
        unsafe {
            let sep = appkit::send_id(class(c"NSMenuItem").as_id(), sel!(separatorItem));
            if !sep.is_null() {
                appkit::send_v_id(menu.id(), sel!(addItem:), sep);
            }
        }
    }

    /// Present the system open panel and return exactly the one local path the user
    /// approved. The panel grants no directory or multiple-file authority; the caller
    /// still canonicalizes, bounds, UTF-8-validates, and mints the process-local
    /// document grant before reading the file.
    pub(crate) fn choose_local_file(title: &str, prompt: &str) -> Option<std::path::PathBuf> {
        open_panel(title, prompt, &[])
    }

    /// [`choose_local_file`] offering only images: the panel greys out every
    /// file whose type does not conform to `public.image`.
    pub(crate) fn choose_local_image(title: &str, prompt: &str) -> Option<std::path::PathBuf> {
        open_panel(title, prompt, &["public.image"])
    }

    /// The open panel, limited to `types` (type identifiers or extensions;
    /// empty offers every file).
    fn open_panel(title: &str, prompt: &str, types: &[&str]) -> Option<std::path::PathBuf> {
        if super::os_ui_refused(title) {
            return None;
        }
        let _main_thread = MainThread::new()?;
        // `-runModal` below spins a nested run loop for as long as the panel is
        // up; park the main-thread watchdog for exactly that long.
        let _park = crate::watchdog::park_modal();
        let title = appkit::nsstring(title)?;
        let prompt = appkit::nsstring(prompt)?;
        let types = types
            .iter()
            .map(|kind| appkit::nsstring(kind))
            .collect::<Option<Vec<Obj>>>()?;
        let type_ids = types.iter().map(Obj::id).collect::<Vec<Id>>();
        // SAFETY: NSOpenPanel is created and run on AppKit's main thread
        // (`_main_thread` is the witness). `+openPanel` is `-(id)` and returns an
        // AUTORELEASED panel — borrowed for the length of this pool, which is
        // why the whole body is inside one. The setters are `-(void)(BOOL)` and
        // `-(void)(NSString *)`; `-runModal` is `-(NSModalResponse)`, i.e.
        // `NSInteger`, and owns its nested modal loop; `-URL` and `-path` are
        // `-(id)` accessors valid until the pool pops. Only an affirmative
        // response is converted to a local path.
        autoreleasepool(|_| unsafe {
            let panel = appkit::send_id(class(c"NSOpenPanel").as_id(), sel!(openPanel));
            if panel.is_null() {
                return None;
            }
            appkit::send_v_bool(panel, sel!(setCanChooseFiles:), true);
            appkit::send_v_bool(panel, sel!(setCanChooseDirectories:), false);
            appkit::send_v_bool(panel, sel!(setAllowsMultipleSelection:), false);
            appkit::send_v_bool(panel, sel!(setResolvesAliases:), true);
            appkit::send_v_id(panel, sel!(setTitle:), title.id());
            appkit::send_v_id(panel, sel!(setPrompt:), prompt.id());
            if !type_ids.is_empty() {
                // `+arrayWithObjects:count:` copies the pointers and retains
                // the strings `types` keeps alive; the array is autoreleased
                // into this pool, and `-setAllowedFileTypes:` (`-(void)(NSArray
                // *)`, type identifiers or extensions) copies it.
                let allowed = appkit::send_id_idptr_usize(
                    class(c"NSArray").as_id(),
                    sel!(arrayWithObjects:count:),
                    type_ids.as_ptr(),
                    type_ids.len(),
                );
                if allowed.is_null() {
                    return None;
                }
                appkit::send_v_id(panel, sel!(setAllowedFileTypes:), allowed);
            }
            if appkit::send_isize(panel, sel!(runModal)) != NS_MODAL_RESPONSE_OK {
                return None;
            }
            let url = appkit::send_id(panel, sel!(URL));
            if url.is_null() {
                return None;
            }
            let path = appkit::send_id(url, sel!(path));
            if path.is_null() {
                return None;
            }
            Some(std::path::PathBuf::from(appkit::nsstring_to_rust(path)))
        })
    }

    /// Attach `submenu` under a new action-less top-level item titled `title` on `bar`,
    /// returning the retained item (the Version menu is retitled through it, see
    /// [`update_version_menu`]). The submenu gets the same title: AppKit takes a top-level bar
    /// title from the submenu on some paths.
    fn attach_submenu(bar: &Obj, title: &str, submenu: Obj) -> Option<Obj> {
        let ns_title = appkit::nsstring(title)?;
        let item = new_item(title, Sel::NULL, "")?;
        // SAFETY: `-setTitle:` is `-(void)(NSString *)`, `-setSubmenu:` is `-(void)(NSMenu *)`
        // and retains, `-addItem:` is `-(void)(NSMenuItem *)` and retains, so both the +1 title
        // and the moved `submenu` may drop at the end of this frame.
        unsafe {
            appkit::send_v_id(submenu.id(), sel!(setTitle:), ns_title.id());
            appkit::send_v_id(item.id(), sel!(setSubmenu:), submenu.id());
            appkit::send_v_id(bar.id(), sel!(addItem:), item.id());
        }
        Some(item)
    }

    /// Show a native modal confirmation (a ⌘Q quit, or a close that would lose work) and block
    /// until the user answers. `proceed_label` titles the affirmative, default button; a Cancel
    /// button is always added and Escape maps to it. Returns `true` iff the user proceeds.
    /// `runModal` spins a nested modal loop on the main thread, so this is safe straight from
    /// the winit event handler.
    ///
    /// # Return with ⌘ held
    ///
    /// The default button's Return equivalent has an empty modifier mask and AppKit matches key
    /// equivalents exactly, so ⌘Return (⌘ still held after ⌘Q) would not answer. The
    /// [`crate::alert_keys`] interceptor is installed for the `runModal` call: Return (any
    /// modifiers) proceeds, Escape cancels, everything else passes through.
    pub(crate) fn confirm(title: &str, body: &str, proceed_label: &str) -> bool {
        // Proceed whenever no alert could be shown (headless included, as
        // `confirm_destructive_close` does): a quit that cannot ask must not wedge.
        ask(title, body, proceed_label, AlertDefault::Proceed).unwrap_or(true)
    }

    /// [`confirm`] for an owner gesture that changes this Mac (the Security panel's reset,
    /// warm-up and *Move to Trash*). Fails closed: with no window, off the main thread or with
    /// no alert built, nobody is asked and it returns `None`, which the caller must not read as
    /// a decline. Control input arrives as a `Wake`, queued while `runModal` spins, so no
    /// control verb can answer it. Since a program can choose when the alert appears, its
    /// default button is Cancel and the destructive button refuses keyboard focus: only a
    /// deliberate click on `proceed_label` says yes.
    pub(crate) fn confirm_owner(title: &str, body: &str, proceed_label: &str) -> Option<bool> {
        ask(title, body, proceed_label, AlertDefault::Cancel)
    }

    /// Which button of [`ask`]'s alert Return presses.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum AlertDefault {
        /// Return (with any modifiers, via `alert_keys`) proceeds: ⌘Q's alert.
        Proceed,
        /// Return and Escape cancel, and no key watch is installed.
        Cancel,
    }

    /// The alert behind [`confirm`] and [`confirm_owner`]: `None` when none
    /// could be shown, else whether the user chose to proceed.
    fn ask(title: &str, body: &str, proceed_label: &str, default: AlertDefault) -> Option<bool> {
        if super::os_ui_refused(title) || MainThread::new().is_none() {
            return None;
        }
        // `-runModal` below spins a nested run loop until the user answers;
        // park the main-thread watchdog for exactly that long.
        let _park = crate::watchdog::park_modal();
        let (Some(title), Some(body), Some(proceed), Some(cancel)) = (
            appkit::nsstring(title),
            appkit::nsstring(body),
            appkit::nsstring(proceed_label),
            appkit::nsstring("Cancel"),
        ) else {
            // Foundation refused a string, so no alert can be built.
            return None;
        };
        // A nil alert is "could not ask" too.
        // SAFETY: `+[NSAlert new]` is `+(instancetype)` and +1 (alloc+init), which is what
        // `Obj::from_owned` adopts.
        let alert =
            unsafe { Obj::from_owned(appkit::send_id(class(c"NSAlert").as_id(), sel!(new))) }?;
        // SAFETY: standard `NSAlert` setters and `runModal`, on the main thread
        // (`MainThread::new()` above proves it). `-setMessageText:` and `-setInformativeText:`
        // are `-(void)(NSString *)` and copy their argument, so the +1 strings may drop at the
        // end of this frame. `-addButtonWithTitle:` is `-(NSButton *)(NSString *)` and `-window`
        // is `-(NSWindow *)`, both +0, so both are retained into `Obj`, not adopted. The alert
        // keeps the default `NSAlertStyleWarning`. `-runModal` is `-(NSModalResponse)`, an
        // `NSInteger`.
        unsafe {
            appkit::send_v_id(alert.id(), sel!(setMessageText:), title.id());
            appkit::send_v_id(alert.id(), sel!(setInformativeText:), body.id());
            if default == AlertDefault::Cancel {
                // The first button added is the default, so Cancel goes first: Return and
                // Escape both cancel, and the destructive button refuses keyboard focus, so
                // neither Tab nor Space reaches it. No key watch: its job is to make Return
                // proceed.
                let _ = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), cancel.id());
                let proceed_button =
                    appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), proceed.id());
                if !proceed_button.is_null() {
                    // `-setRefusesFirstResponder:` is `-(void)(BOOL)` on NSControl,
                    // which the NSButton `-addButtonWithTitle:` returns is.
                    appkit::send_v_bool(proceed_button, sel!(setRefusesFirstResponder:), true);
                }
                let response = appkit::send_isize(alert.id(), sel!(runModal));
                return Some(response == NS_ALERT_SECOND_BUTTON_RETURN);
            }
            // The first button added is the default (Return with an empty modifier mask, hence
            // the key watch below): proceed. The second is Cancel (AppKit binds Escape to it).
            let accept = Obj::retain(appkit::send_id_id(
                alert.id(),
                sel!(addButtonWithTitle:),
                proceed.id(),
            ));
            let refuse = Obj::retain(appkit::send_id_id(
                alert.id(),
                sel!(addButtonWithTitle:),
                cancel.id(),
            ));
            let panel = Obj::retain(appkit::send_id(alert.id(), sel!(window)));
            // Dropped when this function returns, the moment `runModal` comes back, so the
            // interceptor lives exactly as long as the alert. With a button or the panel
            // missing there is nothing to click: no watch, and the alert keeps its stock keys.
            let _keys = match (panel, accept, refuse) {
                (Some(panel), Some(accept), Some(refuse)) => {
                    crate::alert_keys::watch_alert_keys(panel, None, accept, refuse)
                }
                _ => None,
            };
            let response = appkit::send_isize(alert.id(), sel!(runModal));
            Some(response == NS_ALERT_FIRST_BUTTON_RETURN)
        }
    }

    /// Show an informational alert (one OK button) and block until it is dismissed: a file that
    /// could not open, a failed start. Off the main thread it does nothing.
    pub(crate) fn notify(title: &str, body: &str) {
        if super::os_ui_refused(title) || MainThread::new().is_none() {
            return;
        }
        // `-runModal` below spins a nested run loop until the user dismisses
        // the alert; park the main-thread watchdog for exactly that long.
        let _park = crate::watchdog::park_modal();
        autoreleasepool(|_| {
            let Some(alert) = ok_alert(title, body) else {
                return;
            };
            // SAFETY: main thread (checked above); `-runModal` is
            // `-(NSModalResponse)` on the live alert.
            let _ = unsafe { appkit::send_isize(alert.id(), sel!(runModal)) };
        });
    }

    /// [`notify`]'s alert, built and not yet run: also the launch-fatal alert's
    /// (`launch_alert`), which puts its panel in before asking to be the active
    /// app. Main thread only, as every caller checks first. `None` when AppKit
    /// builds none.
    pub(crate) fn ok_alert(title: &str, body: &str) -> Option<Obj> {
        let (Some(title), Some(body), Some(ok)) = (
            appkit::nsstring(title),
            appkit::nsstring(body),
            appkit::nsstring("OK"),
        ) else {
            return None;
        };
        // SAFETY: standard `NSAlert` construction + setters on the main thread.
        // `+new` is +1 (alloc+init) and lands in `Obj`;
        // `-setMessageText:`/`-setInformativeText:` are `-(void)(NSString *)`;
        // `-addButtonWithTitle:` is `-(id)` returning a BORROWED button this
        // caller does not keep.
        unsafe {
            let alert = Obj::from_owned(appkit::send_id(class(c"NSAlert").as_id(), sel!(new)))?;
            appkit::send_v_id(alert.id(), sel!(setMessageText:), title.id());
            appkit::send_v_id(alert.id(), sel!(setInformativeText:), body.id());
            let _ = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), ok.id());
            Some(alert)
        }
    }

    /// Whether the `terminate:` AppKit is asking about now is the system's — a logout, a restart
    /// or a shutdown: the quit Apple Event being handled carries a `kAEQuitReason` naming one
    /// ([`crate::system_quit::reason_is_the_systems`]). A Dock Quit or an AppleScript `quit`
    /// carries none; ⌘Q is the app menu's and never reaches here. A failed read is no reason.
    fn terminate_is_the_systems() -> bool {
        // SAFETY: Foundation sends on the main thread inside
        // `applicationShouldTerminate:`. `+sharedAppleEventManager` and
        // `-currentAppleEvent` return BORROWED objects (nil outside an event);
        // `-attributeDescriptorForKeyword:` takes an `AEKeyword` (a 32-bit
        // `FourCharCode`, passed in a full register) and returns a borrowed
        // descriptor or nil; `-enumCodeValue` returns an `OSType`, the low
        // 32 bits of the return register.
        autoreleasepool(|_| unsafe {
            let manager = appkit::send_id(
                class(c"NSAppleEventManager").as_id(),
                sel!(sharedAppleEventManager),
            );
            if manager.is_null() {
                return false;
            }
            let event = appkit::send_id(manager, sel!(currentAppleEvent));
            if event.is_null() {
                return false;
            }
            let reason = appkit::send_id_usize(
                event,
                sel!(attributeDescriptorForKeyword:),
                crate::system_quit::KEY_QUIT_REASON as usize,
            );
            if reason.is_null() {
                return false;
            }
            let code = (appkit::send_usize(reason, sel!(enumCodeValue)) & 0xFFFF_FFFF) as u32;
            crate::system_quit::reason_is_the_systems(code)
        })
    }

    /// AppKit's synchronous `applicationShouldTerminate:` hook. The first request
    /// is vetoed and posted to the typed event loop; duplicates remain vetoed while
    /// the same generation is awaiting confirmation/save proofs. Once `App` marks
    /// the generation complete, a re-entrant terminate is allowed (normal aterm
    /// shutdown uses `ActiveEventLoop::exit` and does not need to re-enter AppKit).
    pub(crate) fn defer_quit_for_terminate() -> bool {
        // A restart, logout or shutdown is an exit the person did not choose, so its quit keeps
        // the agents aterm hosts for the next launch (`crate::system_quit`), noted against this
        // request's generation so a cancel takes the mark with it.
        let decision = super::admit_native_terminate(terminate_is_the_systems());
        match decision {
            NativeTerminateDecision::AllowExit(_) => true,
            NativeTerminateDecision::DeferExisting(_) => false,
            NativeTerminateDecision::Dispatch(generation) => {
                let Some(proxy) = TERMINATE_PROXY.get() else {
                    let _ = super::cancel_native_termination(generation);
                    return true;
                };
                if proxy
                    .send_event(Wake::NativeTerminateRequested { generation })
                    .is_err()
                {
                    let _ = super::cancel_native_termination(generation);
                    return true;
                }
                false
            }
        }
    }

    /// Help ▸ aterm Help: open the bundled offline guide (`Contents/Resources/Help.html`, put
    /// there by aterm-release's `bundle.rs`) in the default browser, or the project page when
    /// running outside the `.app`.
    pub(crate) fn open_help_url() {
        if let Some(help) = bundled_resource("Help.html") {
            open_in_workspace(&help, true);
        } else {
            open_in_workspace(&super::help_url(), false);
        }
    }

    /// Resolve a file inside the running app bundle's `Contents/Resources/`, returning
    /// its path only when the file exists. The executable lives at
    /// `<app>/Contents/MacOS/<bin>`, so resources are two levels up then `Resources/`.
    fn bundled_resource(name: &str) -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        let res = exe.parent()?.parent()?.join("Resources").join(name);
        res.is_file().then(|| res.to_string_lossy().into_owned())
    }

    /// Open `s` via `NSWorkspace openURL:` — a file path (`is_file`) as a `file://` URL,
    /// otherwise an absolute URL. Main thread only. Returns whether the shell accepted the URL,
    /// which is not whether the app did what it asked (see [`open_privacy_settings`]); `false`
    /// also covers a refusal, so "not attempted" never reads as "opened".
    fn open_in_workspace(s: &str, is_file: bool) -> bool {
        if super::os_ui_refused(&format!("opening {s}")) || MainThread::new().is_none() {
            return false;
        }
        let Some(ns) = appkit::nsstring(s) else {
            return false;
        };
        // SAFETY: `+fileURLWithPath:` / `+URLWithString:` are `-(id)(NSString *)`
        // convenience constructors returning an AUTORELEASED NSURL (nil for a
        // string that is not a URL, which is checked); `+sharedWorkspace` is the
        // `-(id)` singleton; `-openURL:` is `-(BOOL)(NSURL *)` and its answer is
        // handed straight back. Both borrows live inside this pool.
        autoreleasepool(|_| unsafe {
            let url = appkit::send_id_id(
                class(c"NSURL").as_id(),
                if is_file {
                    sel!(fileURLWithPath:)
                } else {
                    sel!(URLWithString:)
                },
                ns.id(),
            );
            if url.is_null() {
                return false;
            }
            let ws = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            if ws.is_null() {
                return false;
            }
            appkit::send_bool_id(ws, sel!(openURL:), url)
        })
    }

    /// Open a local file aterm itself names (Settings ▸ Packages' "Open Log", for the package log
    /// atpkg resolved) through `NSWorkspace openURL:` with a `file://` URL, so macOS opens it in
    /// the app that owns the type. No shell or Terminal is spawned, and nothing a program printed
    /// can reach this (the link allowlist stays closed). Main thread only; `false` when refused.
    pub(crate) fn open_file_in_workspace(path: &std::path::Path) -> bool {
        path.to_str().is_some_and(|s| open_in_workspace(s, true))
    }

    /// Show the local file `path` aterm itself names — Settings ▸ Manual's
    /// aterm.toml — selected in a Finder window: `NSWorkspace
    /// selectFile:inFileViewerRootedAtPath:` with an empty root (Finder's own
    /// viewer). Its answer is Finder's: `false` when the file could not be
    /// selected (a missing file), so "Showed" is said only of what was shown.
    /// Main thread only; `false` when refused.
    pub(crate) fn reveal_file_in_workspace(path: &std::path::Path) -> bool {
        let Some(s) = path.to_str() else {
            return false;
        };
        if super::os_ui_refused(&format!("revealing {s}")) || MainThread::new().is_none() {
            return false;
        }
        let (Some(file), Some(root)) = (appkit::nsstring(s), appkit::nsstring("")) else {
            return false;
        };
        // SAFETY: `+sharedWorkspace` is the `-(id)` singleton;
        // `-selectFile:inFileViewerRootedAtPath:` is `-(BOOL)(NSString *,
        // NSString *)`, and its answer is handed straight back. The two
        // strings live inside this pool.
        autoreleasepool(|_| unsafe {
            let ws = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            if ws.is_null() {
                return false;
            }
            appkit::send_bool_id_id(
                ws,
                sel!(selectFile:inFileViewerRootedAtPath:),
                file.id(),
                root.id(),
            )
        })
    }

    /// How long after the first open the same URL is issued again. The anchor
    /// after `?` is not Apple-documented and is not always honoured on the
    /// first open (design Appendix A.2); a second issue about a second later is
    /// the shipped workaround. Long enough that System Settings has finished
    /// launching cold, short enough that it is one gesture rather than two.
    const SECOND_OPEN_DELAY_SECS: f64 = 1.0;

    /// Open System Settings at `pane` — twice, about [`SECOND_OPEN_DELAY_SECS`] apart, the second
    /// a delayed perform on the main run loop ([`open_in_workspace_after_delay`]) — and report how
    /// far it got.
    ///
    /// The URL is a `&'static str` compiled into aterm with no untrusted bytes, reached from
    /// the owner's presses (a button on the Security page, a digest line's capsule) and from
    /// the Developer Tools (`App::hand_devtools_to_owner`) and Full Disk Access
    /// (`App::hand_fda_to_owner`) hand-over lanes at a quiet moment (`devtools_helper::quiet`).
    /// It goes to `NSWorkspace` directly rather than through the link path, so the allowlist
    /// stays closed.
    ///
    /// Candidates come best first from [`privacy_settings_urls`], whose last has no anchor, so a
    /// refused anchor still opens Privacy & Security ([`SettingsOpen::PaneRoot`]); off the main
    /// thread nothing is attempted ([`SettingsOpen::Refused`]). Either way the caller shows
    /// [`privacy_settings_path_words`].
    pub(crate) fn open_privacy_settings(pane: PrivacyPane) -> SettingsOpen {
        if MainThread::new().is_none() {
            return SettingsOpen::Refused;
        }
        let urls = privacy_settings_urls(pane);
        for (i, url) in urls.iter().enumerate() {
            if !open_in_workspace(url, false) {
                continue;
            }
            // The last candidate is the un-anchored pane root, and re-issuing a
            // URL with no anchor buys nothing but a second activation.
            if i + 1 < urls.len() {
                open_in_workspace_after_delay(url, SECOND_OPEN_DELAY_SECS);
                return SettingsOpen::Anchored;
            }
            return SettingsOpen::PaneRoot;
        }
        SettingsOpen::Refused
    }

    /// Put `window` — a panel aterm itself built, never another app's — in front
    /// of every app's windows WITHOUT activating aterm (`-orderFrontRegardless`):
    /// the Developer Tools helper panel beside System Settings
    /// (`devtools_helper`), which must show while System Settings stays the
    /// active app. `false`, and nothing shown, on a headless process or off the
    /// main thread.
    ///
    /// Here and not in the panel's own module so the headless refusal this
    /// module's pin holds every OS-UI site to covers it: a headless instance has
    /// no glass to put a panel on, and a panel it could raise would float over
    /// the owner's desktop with nothing behind it.
    pub(crate) fn order_front_regardless(window: Id, what: &str) -> bool {
        if super::os_ui_refused(what) || MainThread::new().is_none() || window.is_null() {
            return false;
        }
        // SAFETY: `-orderFrontRegardless` is `-(void)` on the live `NSWindow` the
        // caller owns, on the main thread (checked above).
        unsafe { appkit::send_v(window, sel!(orderFrontRegardless)) };
        true
    }

    /// Issue `s` through `NSWorkspace openURL:` again after `secs`.
    /// `performSelector:withObject:afterDelay:` schedules on the calling thread's run loop and
    /// this runs only on the main thread, so the second open runs there while the thread stays
    /// free meanwhile: no worker, no timer to own, nothing to cancel. The perform uses the
    /// default run-loop mode, so during a tracking loop (a drag, an open menu) the second open
    /// lands late, when tracking ends, but is never lost.
    fn open_in_workspace_after_delay(s: &str, secs: f64) {
        if super::os_ui_refused(&format!("opening {s}")) || MainThread::new().is_none() {
            return;
        }
        let Some(ns) = appkit::nsstring(s) else {
            return;
        };
        // SAFETY: standard AppKit. Every `s` reaching here is one of this module's own
        // `&'static str` URL constants, so `URLWithString:` is non-nil. `NSWorkspace` retains
        // the receiver and the argument until the perform fires, so nothing here outlives this
        // frame. The delayed perform invokes `openURL:` (really `BOOL`-returning) as
        // `id`-returning; the value lands in the same return register on every target aterm
        // builds for and is discarded.
        autoreleasepool(|_| unsafe {
            let url = appkit::send_id_id(class(c"NSURL").as_id(), sel!(URLWithString:), ns.id());
            let ws = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            if url.is_null() || ws.is_null() {
                return;
            }
            let perform: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64) =
                aterm_objc::checked_msg();
            perform(
                ws,
                sel!(performSelector:withObject:afterDelay:),
                sel!(openURL:),
                url,
                secs,
            );
        });
    }

    #[cfg(test)]
    mod tests {
        use aterm_objc::{Bool, ClassType, Encode, class, class_name, method_types, sel};

        use super::MenuTarget;
        use crate::appkit;

        /// The registered class is what the runtime says it is.
        #[test]
        fn the_registered_class_is_what_the_runtime_reports() {
            let cls = MenuTarget::class();
            assert!(!cls.is_null());
            assert_eq!(class(c"ATermMenuTarget"), cls);
            // SAFETY: `cls` is the class this module registered.
            unsafe {
                assert_eq!(class_name(cls), c"ATermMenuTarget");
                assert_eq!(class_name(aterm_objc::superclass_of(cls)), c"NSObject");
                assert!(appkit::send_bool_sel(
                    cls.as_id(),
                    sel!(instancesRespondToSelector:),
                    sel!(menuAction:)
                ));
                assert!(appkit::send_bool_sel(
                    cls.as_id(),
                    sel!(instancesRespondToSelector:),
                    sel!(validateMenuItem:)
                ));
            }
        }

        /// The method encodings, read back from the runtime with `method_getTypeEncoding`.
        /// `validateMenuItem:` registers `"B@:@"` on `aarch64-apple-darwin` and `"c@:@"` on
        /// `x86_64-apple-darwin` (`@encode(BOOL)` differs), so the expectation is built from
        /// `Bool::ENCODING`, and the literal is asserted on aarch64 so a wrong `Bool::ENCODING`
        /// cannot make the test vacuous.
        #[test]
        fn the_runtime_reports_the_encodings_the_table_says() {
            let cls = MenuTarget::class();
            // SAFETY: `cls` is the live registered class.
            unsafe {
                assert_eq!(
                    method_types(cls, sel!(menuAction:)).as_deref(),
                    Some("v@:@")
                );
                assert_eq!(
                    method_types(cls, sel!(validateMenuItem:)),
                    Some(format!("{}@:@", Bool::ENCODING))
                );
                assert_eq!(method_types(cls, sel!(dealloc)).as_deref(), Some("v@:"));
            }
            #[cfg(target_arch = "aarch64")]
            assert_eq!(Bool::ENCODING, "B");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MENU_MODEL, MenuAction, MenuEntry, menu_chrome_lines, native_menu_action_enabled,
        set_active_tab_is_terminal, staged_apply_label, version_menu_bar_title,
    };

    /// The updater orders by build number (the display version never affects an
    /// update comparison), so a strictly-newer staged build may share the running
    /// build's version string, and the apply row must never be spellable as the badge.
    #[test]
    fn the_apply_row_can_never_read_as_the_version_already_running() {
        let running = crate::build_info::version_display();
        let badge = version_menu_bar_title(true);

        // The colliding case: the staged build carries the same version string.
        let same = staged_apply_label("\u{2B06}\u{FE0F}", 1_785_910_394, running, None, false);
        assert!(
            same.contains("build 1785910394"),
            "a same-version staged build must be identified by its build number: {same}"
        );
        assert!(
            !same.contains(&format!("v{running}")),
            "the offer must not name the version the badge already shows \
             (badge {badge:?}, offer {same:?})"
        );

        // The ordinary case is untouched: a genuinely different version is named.
        let newer = staged_apply_label("\u{2B06}\u{FE0F}", 1_785_910_394, "9.9.9", None, false);
        assert!(
            newer.contains("v9.9.9") && !newer.contains("build"),
            "a differing version is still named directly: {newer}"
        );
    }

    /// The always-visible offer must carry the apply lane's verdict on itself:
    /// a scheduled retry asks for nothing; a lane that stopped names the action
    /// that resumes it; a build on hold offers no start.
    #[test]
    fn the_apply_row_admits_when_applying_has_already_been_tried() {
        use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};

        let label = |retry, held| {
            let trouble = ApplyTrouble::new(2, "handoff proof ended ChildDied", retry)
                .expect("two failed applies");
            staged_apply_label("\u{2191}", 1_787_699_398, "9.9.9", Some(&trouble), held)
        };
        let clean = staged_apply_label("\u{2191}", 1_787_699_398, "9.9.9", None, false);
        assert!(clean.ends_with(" now"), "{clean}");
        let scheduled = label(ApplyRetry::Scheduled, false);
        assert!(
            scheduled.contains("v9.9.9") && scheduled.contains("twice"),
            "{scheduled}"
        );
        assert!(!scheduled.contains("ChildDied"), "{scheduled}");
        assert!(!scheduled.ends_with(" now"), "{scheduled}");
        assert!(label(ApplyRetry::ManualOnly, false).ends_with(" now"));
        for held in [
            label(ApplyRetry::NewerRelease, true),
            staged_apply_label("\u{2191}", 1_787_699_398, "9.9.9", None, true),
        ] {
            assert!(held.contains("on hold") && !held.contains(" now"), "{held}");
        }
    }

    /// The palette-only actions (`PALETTE_ONLY`): no bar item, a palette row
    /// and an `invoke` name each.
    fn palette_only_actions() -> Vec<MenuAction> {
        super::PALETTE_ONLY.iter().map(|(_, a)| *a).collect()
    }

    /// Every action the bar, the palette and the tab menu offer: the model's
    /// items, the palette-only rows and the tab menu's own copy rows.
    fn offered_actions() -> Vec<MenuAction> {
        model_items()
            .into_iter()
            .map(|(_, action, _, _)| action)
            .chain(palette_only_actions())
            .chain([MenuAction::CopySessionId, MenuAction::CopyCwd])
            .collect()
    }

    /// Every `invoke` name an agent's script may already carry, verbatim. Each
    /// resolves (to the action it named, or the alias its row became) or answers
    /// `ERR retired: <what to use>` — never an unknown action. Pinned by
    /// `every_earlier_invoke_name_resolves_or_is_retired`.
    const EARLIER_INVOKE_NAMES: &[&str] = &[
        "About",
        "SoftwareUpdate",
        "Version",
        "ApplyUpdate",
        "Preferences",
        "Quit",
        "NewWindow",
        "NewTab",
        "OpenMarkdown",
        "OpenEditor",
        "ReopenClosedTab",
        "ReopenClosedView",
        "MoveTabToNewWindow",
        "MoveTabToNextWindow",
        "ViewSessionInNewWindow",
        "NewControlledWindow",
        "NewControlledTab",
        "NewControllerWindow",
        "NewControllerTab",
        "CloseTab",
        "Copy",
        "Paste",
        "SelectAll",
        "Find",
        "FindNext",
        "FindPrev",
        "ToggleFullScreen",
        "FontIncrease",
        "FontDecrease",
        "FontActualSize",
        "SplitVertical",
        "SplitHorizontal",
        "ToggleMatrixRain",
        "FavouriteKitty",
        "NextKitty",
        "ToggleSeriousMode",
        "ToggleSettings",
        "Packages",
        "RenameSession",
        "OpenPalette",
        "Minimize",
        "Zoom",
        "NextTab",
        "PrevTab",
        "Help",
        "CopySessionId",
        "CopyCwd",
        "ConnectToSession",
        "ShowConnectionMap",
        "ConfigureConnection",
        "DisconnectSession",
        "Inbox",
        "ShowInbox",
        "LedgerForSession",
        "OpenLedger",
        "HoldSession",
        "LiftHold",
        "Fleet",
        "ShowFleet",
        "FabricStatus",
        "FabricOn",
        "FabricOff",
        "SetRole",
        "ShowIdentity",
        "TogglePresenceBand",
        "TogglePresenceRim",
    ];

    /// Every item of the model, submenus flattened.
    fn model_items() -> Vec<(&'static str, MenuAction, &'static str, super::MenuMods)> {
        MENU_MODEL
            .iter()
            .flat_map(|s| s.entries.iter())
            .flat_map(MenuEntry::items)
            .collect()
    }

    /// The tab-context actions are a deliberate non-bar vocabulary: their invoke
    /// names round-trip (so `invoke CopySessionId` is fenceable + dispatchable),
    /// each classifies at its honest boundary (the copies move text onto the
    /// pasteboard), and they must not leak into `MENU_MODEL` (the bar mirror
    /// stays exactly the bar).
    #[test]
    fn tab_context_actions_round_trip_and_stay_out_of_the_bar() {
        for a in [MenuAction::CopySessionId, MenuAction::CopyCwd] {
            let name = format!("{a:?}");
            assert_eq!(MenuAction::from_invoke_name(&name), Some(a));
            assert_eq!(
                a.invoke_authority(),
                super::InvokeAuthority::ClipboardWrite,
                "{a:?} boundary"
            );
            assert!(
                !model_items().iter().any(|(_, action, _, _)| *action == a),
                "{a:?} is context-menu-only, never a bar item"
            );
        }
    }

    /// Every earlier `invoke` name resolves or is retired: a name resolves to
    /// the action its canonical spelling names, or is retired with a replacement
    /// (`ERR retired: <what to use>`), and no name is both.
    #[test]
    fn every_earlier_invoke_name_resolves_or_is_retired() {
        for name in EARLIER_INVOKE_NAMES {
            match (
                MenuAction::from_invoke_name(name),
                super::retired_invoke_name(name),
            ) {
                (Some(action), None) => assert_eq!(
                    format!("{action:?}"),
                    super::canonical_invoke_name(name),
                    "{name} names the action its canonical spelling does"
                ),
                (None, Some(instead)) => assert!(!instead.is_empty(), "{name}"),
                other => panic!("{name}: resolves or is retired, never both or neither: {other:?}"),
            }
        }
        for (old, new) in [
            ("HoldSession", MenuAction::TakeOver),
            ("LiftHold", MenuAction::HandBack),
            ("LedgerForSession", MenuAction::HarnessLog),
            ("OpenLedger", MenuAction::HarnessLog),
            ("ShowConnectionMap", MenuAction::Overview),
            ("Fleet", MenuAction::Overview),
            ("ShowFleet", MenuAction::Overview),
            ("TogglePresenceRim", MenuAction::ToggleNeedsInWindows),
        ] {
            assert_eq!(MenuAction::from_invoke_name(old), Some(new), "{old}");
        }
        for retired in [
            "Inbox",
            "ShowInbox",
            "ConnectToSession",
            "ConfigureConnection",
            "DisconnectSession",
            "NewControlledWindow",
            "NewControlledTab",
            "NewControllerWindow",
            "NewControllerTab",
            "TogglePresenceBand",
        ] {
            let refusal = crate::control::control_media::invoke_refusal(retired)
                .unwrap_or_else(|| panic!("{retired} is refused before the hop"));
            assert!(refusal.starts_with("ERR retired: "), "{retired}: {refusal}");
            assert!(
                crate::palette::PaletteState::new()
                    .action_by_name(retired)
                    .is_err_and(|e| e.starts_with("retired: ")),
                "{retired}: the palette's lookup says so too"
            );
        }
        // Control: a live name is neither refused nor retired.
        assert_eq!(
            crate::control::control_media::invoke_refusal("NewTab"),
            None
        );
    }

    /// The Agents menu sits where an app's own menu goes, between View and Window,
    /// and holds its rows in order with their chords (⇧⌘J, ⌃⌘O, ⇧⌘L). Its
    /// classification: Take Over and Hand Back are the person's own; Next
    /// That Needs You and Set Role… are `WriteInput` (they move focus or open
    /// an editor); every other row sees the whole Mac or fleet and is
    /// Owner-only. The session rows need a focused session; the rest never
    /// grey for want of one.
    #[test]
    fn the_agents_menu_holds_its_rows_in_order_with_their_classes() {
        use super::{InvokeAuthority, MenuMods};
        let titles: Vec<&str> = MENU_MODEL.iter().map(|s| s.title).collect();
        let agents_at = titles
            .iter()
            .position(|t| *t == "Agents")
            .expect("an Agents menu");
        assert_eq!(titles[agents_at - 1], "View");
        assert_eq!(titles[agents_at + 1], "Window");
        assert!(!titles.contains(&"Fabric"), "the Fabric menu is gone");
        let agents = &MENU_MODEL[agents_at];
        let rows: Vec<(MenuAction, &str, MenuMods)> = agents
            .entries
            .iter()
            .flat_map(MenuEntry::items)
            .map(|(_, action, key, mods)| (action, key, mods))
            .collect();
        assert_eq!(
            rows,
            [
                (MenuAction::NextNeed, "j", MenuMods::CommandShift),
                (MenuAction::Overview, "o", MenuMods::CommandControl),
                (MenuAction::TakeOver, "", MenuMods::None),
                (MenuAction::HandBack, "", MenuMods::None),
                (MenuAction::HarnessLog, "l", MenuMods::CommandShift),
                (MenuAction::SetRole, "", MenuMods::None),
                (MenuAction::ReadThisMac, "", MenuMods::None),
                (MenuAction::ReadFleet, "", MenuMods::None),
                (MenuAction::SummarizeFleet, "", MenuMods::None),
            ]
        );
        assert_eq!(
            agents
                .entries
                .iter()
                .filter(|e| matches!(e, MenuEntry::Separator))
                .count(),
            3
        );
        for (action, ..) in &rows {
            let want = match action {
                MenuAction::TakeOver | MenuAction::HandBack => InvokeAuthority::PersonOnly,
                MenuAction::NextNeed | MenuAction::SetRole => InvokeAuthority::WriteInput,
                _ => InvokeAuthority::OwnerOnly,
            };
            assert_eq!(action.invoke_authority(), want, "{action:?}");
            let session_row = matches!(
                action,
                MenuAction::TakeOver
                    | MenuAction::HandBack
                    | MenuAction::HarnessLog
                    | MenuAction::SetRole
            );
            assert_eq!(
                super::requires_terminal_tab(*action),
                session_row,
                "{action:?}"
            );
        }
        // The palette-only rows: no bar item, Owner-only like the menu's reads.
        for (_, action) in super::PALETTE_ONLY {
            assert!(
                !model_items().iter().any(|(_, a, _, _)| a == action),
                "{action:?} is palette-only"
            );
            assert_eq!(
                action.invoke_authority(),
                InvokeAuthority::OwnerOnly,
                "{action:?}"
            );
        }
    }

    /// The person's hold pair reads the live projection: no person hold ⇒
    /// Take Over live, Hand Back not; the person's hold ⇒ the other way. No
    /// session at all greys both with every other session row.
    #[test]
    fn take_over_and_hand_back_follow_the_front_person_hold() {
        set_active_tab_is_terminal(true);
        super::set_front_person_hold(false);
        assert!(native_menu_action_enabled(MenuAction::TakeOver));
        assert!(!native_menu_action_enabled(MenuAction::HandBack));
        super::set_front_person_hold(true);
        assert!(!native_menu_action_enabled(MenuAction::TakeOver));
        assert!(native_menu_action_enabled(MenuAction::HandBack));
        // Another Mac's hold: the person may take it or hand it back.
        super::set_front_remote_hold(true);
        assert!(native_menu_action_enabled(MenuAction::TakeOver));
        assert!(native_menu_action_enabled(MenuAction::HandBack));
        super::set_front_remote_hold(false);
        super::set_front_person_hold(false);
        set_active_tab_is_terminal(false);
        assert!(!native_menu_action_enabled(MenuAction::TakeOver));
        assert!(!native_menu_action_enabled(MenuAction::HarnessLog));
        set_active_tab_is_terminal(true);
    }

    /// The Agents rows grey from their live bits, as the palette's do: Next
    /// That Needs You only with a local need, Overview only with a window,
    /// Read the Fleet only in a fleet; Summarize greys with its block, and
    /// its tool tip says why, and its title names the fleet or this Mac.
    #[test]
    fn the_agents_rows_grey_from_their_live_bits() {
        use super::{AgentsLive, native_item_tip, native_live_title, set_agents_live};
        set_active_tab_is_terminal(true);
        let quiet = AgentsLive::default();
        set_agents_live(quiet.clone());
        for a in [
            MenuAction::NextNeed,
            MenuAction::Overview,
            MenuAction::ReadFleet,
            MenuAction::FleetMachines,
        ] {
            assert!(
                !native_menu_action_enabled(a),
                "{a:?} greys without its fact"
            );
        }
        assert!(native_menu_action_enabled(MenuAction::SummarizeFleet));
        assert!(
            native_menu_action_enabled(MenuAction::ReadThisMac),
            "always live"
        );
        assert_eq!(native_item_tip(MenuAction::SummarizeFleet), None);
        set_agents_live(AgentsLive {
            needs_local: true,
            in_fleet: true,
            any_window: true,
            summarize_block: Some("(claude not found)".to_string()),
        });
        for a in [
            MenuAction::NextNeed,
            MenuAction::Overview,
            MenuAction::ReadFleet,
            MenuAction::FleetMachines,
        ] {
            assert!(native_menu_action_enabled(a), "{a:?} live with its fact");
        }
        assert!(!native_menu_action_enabled(MenuAction::SummarizeFleet));
        assert!(
            native_item_tip(MenuAction::SummarizeFleet)
                .is_some_and(|tip| tip.ends_with("(claude not found)")),
            "the greyed row says why"
        );
        assert_eq!(
            native_live_title(MenuAction::SummarizeFleet),
            Some(super::summarize_title(true))
        );
        set_agents_live(quiet);
        assert_eq!(
            native_live_title(MenuAction::SummarizeFleet),
            Some(super::summarize_title(false))
        );
        set_agents_live(AgentsLive::UNPUBLISHED);
    }

    /// View ▸ Show Needs in Windows is checkable and reads its checkmark from
    /// the published live state.
    #[test]
    fn the_needs_row_is_checkable_from_the_live_state() {
        super::set_needs_in_windows(false);
        assert_eq!(
            super::native_menu_checked(MenuAction::ToggleNeedsInWindows),
            Some(false)
        );
        super::set_needs_in_windows(true);
        assert_eq!(
            super::native_menu_checked(MenuAction::ToggleNeedsInWindows),
            Some(true)
        );
        assert_eq!(super::native_menu_checked(MenuAction::Overview), None);
        assert_eq!(
            MenuAction::ToggleNeedsInWindows.invoke_authority(),
            super::InvokeAuthority::ConfigWrite,
            "the toggle writes aterm.toml"
        );
        let view = MENU_MODEL.iter().find(|s| s.title == "View").unwrap();
        assert!(
            view.entries
                .iter()
                .flat_map(MenuEntry::items)
                .any(|(_, a, _, _)| a == MenuAction::ToggleNeedsInWindows)
        );
    }

    /// The native bar checks and greys what the palette does (Serious Mode, Matrix Rain and
    /// Favourite This Kitty checkmarks; Copy, Show Next / Previous Tab and the Reopen rows
    /// enabled bits), from the same facts (`MenuLive`, published beside `PaletteLive`).
    #[test]
    fn the_native_bar_checks_and_greys_what_the_palette_does() {
        use super::{MenuLive, menu_live, native_live_title, native_menu_checked, set_menu_live};
        set_active_tab_is_terminal(true);
        let before = menu_live();
        // Before the first publish: content-dependent Find and Save stay disabled.
        set_menu_live(MenuLive::UNPUBLISHED);
        assert_eq!(menu_live(), MenuLive::UNPUBLISHED);
        for a in [
            MenuAction::Copy,
            MenuAction::NextTab,
            MenuAction::PrevTab,
            MenuAction::ReopenClosedTab,
            MenuAction::ReopenClosedView,
        ] {
            assert!(native_menu_action_enabled(a), "{a:?}");
        }
        assert_eq!(native_live_title(MenuAction::CloseTab), Some("Close Tab"));
        for action in [MenuAction::Find, MenuAction::FindNext, MenuAction::FindPrev] {
            assert!(!native_menu_action_enabled(action), "{action:?}");
        }

        let idle = MenuLive {
            has_selection: false,
            can_save: false,
            can_find: false,
            can_find_again: false,
            multi_tab: false,
            can_reopen_closed_tab: false,
            can_reopen_closed_view: false,
            serious_mode: true,
            rain_on: true,
            kitty_favourited: true,
            front_tab_split: true,
        };
        set_menu_live(idle);
        assert_eq!(menu_live(), idle, "every bit round-trips");
        assert!(!native_menu_action_enabled(MenuAction::Save));
        set_menu_live(MenuLive {
            can_save: true,
            ..idle
        });
        assert!(native_menu_action_enabled(MenuAction::Save));
        for (open, again) in [(true, false), (true, true), (false, false)] {
            set_menu_live(MenuLive {
                can_find: open,
                can_find_again: again,
                ..idle
            });
            assert_eq!(native_menu_action_enabled(MenuAction::Find), open);
            assert_eq!(native_menu_action_enabled(MenuAction::FindNext), again);
            assert_eq!(native_menu_action_enabled(MenuAction::FindPrev), again);
            assert_eq!(menu_live().can_find, open);
            assert_eq!(menu_live().can_find_again, again);
        }
        set_menu_live(idle);
        for a in [
            MenuAction::Copy,
            MenuAction::NextTab,
            MenuAction::PrevTab,
            MenuAction::ReopenClosedTab,
            MenuAction::ReopenClosedView,
        ] {
            assert!(!native_menu_action_enabled(a), "{a:?} greys");
        }
        assert!(native_menu_action_enabled(MenuAction::Paste), "untouched");
        for a in [
            MenuAction::ToggleSeriousMode,
            MenuAction::ToggleMatrixRain,
            MenuAction::FavouriteKitty,
        ] {
            assert_eq!(native_menu_checked(a), Some(true), "{a:?}");
        }
        // ⌘W on a split tab closes the focused pane, and the row says so.
        assert_eq!(native_live_title(MenuAction::CloseTab), Some("Close Pane"));
        assert_eq!(native_live_title(MenuAction::Copy), None);

        let off = MenuLive {
            serious_mode: false,
            rain_on: false,
            kitty_favourited: false,
            front_tab_split: false,
            ..MenuLive::UNPUBLISHED
        };
        set_menu_live(off);
        for a in [
            MenuAction::ToggleSeriousMode,
            MenuAction::ToggleMatrixRain,
            MenuAction::FavouriteKitty,
        ] {
            assert_eq!(native_menu_checked(a), Some(false), "{a:?}");
        }
        assert_eq!(native_live_title(MenuAction::CloseTab), Some("Close Tab"));
        set_menu_live(before);
    }

    /// `chrome` on macOS reads each row's title off its `NSMenuItem`, and ⌘W's
    /// is stamped only when the menu validates: ⌘W on a split tab stamps Close
    /// Pane, then closes the pane, and the item keeps Close Pane for a tab that
    /// is no longer split. The reader prints the live title instead.
    #[test]
    fn chrome_reads_the_close_row_live_not_as_last_stamped() {
        use super::{MenuLive, menu_live, native_row_title, set_menu_live};
        let before = menu_live();
        let close = MenuAction::CloseTab.tag();
        set_menu_live(MenuLive {
            front_tab_split: false,
            ..MenuLive::UNPUBLISHED
        });
        assert_eq!(
            native_row_title(close, "Close Pane".to_string()),
            "Close Tab"
        );
        set_menu_live(MenuLive {
            front_tab_split: true,
            ..MenuLive::UNPUBLISHED
        });
        assert_eq!(
            native_row_title(close, "Close Tab".to_string()),
            "Close Pane"
        );
        // A row with no live title, and an item that is not ours (tag 0), keep
        // the title they carry.
        let copy = MenuAction::Copy.tag();
        assert_eq!(native_row_title(copy, "Copy".to_string()), "Copy");
        assert_eq!(
            native_row_title(0, "Emoji & Symbols".to_string()),
            "Emoji & Symbols"
        );
        set_menu_live(before);
    }

    /// Every command the bar and the tab menu carry decodes back from the
    /// integer its `NSMenuItem` carries, and no two share one, so a press
    /// dispatches exactly its own command. It also decodes from its `invoke`
    /// name, the token the control socket's authority fence classifies
    /// (`control::argument_escalation`): a name the fence cannot decode escalates
    /// nothing, while the palette still dispatches it, so an Owner-only
    /// action would run under the base `WriteInput` gate. The default tag (0)
    /// and any unknown tag or name decode to `None`, so an untagged item never
    /// dispatches a real command.
    #[test]
    fn every_action_decodes_from_its_tag_and_its_invoke_name() {
        let mut by_tag = std::collections::HashMap::new();
        for a in offered_actions() {
            if let Some(other) = by_tag.insert(a.tag(), a) {
                assert_eq!(other, a, "{a:?} and {other:?} share tag {}", a.tag());
            }
            assert_eq!(MenuAction::from_tag(a.tag()), Some(a), "{a:?}");
            assert_eq!(
                MenuAction::from_invoke_name(&format!("{a:?}")),
                Some(a),
                "the invoke fence must decode {a:?}"
            );
        }
        for tag in 0..=255isize {
            if let Some(a) = MenuAction::from_tag(tag) {
                assert_eq!(a.tag(), tag, "tag {tag} decodes to {a:?}");
            }
        }
        assert_eq!(MenuAction::from_tag(0), None);
        assert_eq!(MenuAction::from_tag(-1), None);
        assert_eq!(MenuAction::from_tag(9999), None);
        assert_eq!(MenuAction::from_invoke_name("NopeNotAnAction"), None);
        assert_eq!(MenuAction::from_invoke_name(""), None);
    }

    #[test]
    fn staged_update_menu_action_is_selectable_on_every_active_tab_kind() {
        let model = aterm_models_misc::native_update_menu_activation_model();
        let mut state = model.init_state();
        for action in ["StageUpdate", "RefreshStagedVersionMenu"] {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        assert!(
            model_items()
                .iter()
                .any(|(_, action, _, _)| *action == MenuAction::ApplyUpdate),
            "the portable/native shared menu description carries ApplyUpdate"
        );
        set_active_tab_is_terminal(false);
        assert!(
            native_menu_action_enabled(MenuAction::ApplyUpdate),
            "the update row must remain clickable while Settings is frontmost"
        );
        assert!(
            !native_menu_action_enabled(MenuAction::SplitVertical),
            "the fixture still disables genuinely terminal-only commands"
        );
        assert_eq!(
            MenuAction::from_tag(MenuAction::ApplyUpdate.tag()),
            Some(MenuAction::ApplyUpdate),
            "the enabled native row must decode to the exact apply command"
        );
        assert!(model.fire("DecodeApplyTag", &mut state));
        assert!(model.fire("DispatchApply", &mut state));
        assert_eq!(state["apply_dispatched"], 1);

        set_active_tab_is_terminal(true);
        assert!(native_menu_action_enabled(MenuAction::ApplyUpdate));

        // Negative control: the modeled buggy predicate disables the row with a native tab
        // frontmost, violating the obligation, and cannot reach decode/dispatch.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut disabled = buggy.init_state();
        assert!(buggy.fire("StageUpdate", &mut disabled));
        assert!(buggy.fire("RefreshStagedVersionMenu", &mut disabled));
        assert_eq!(disabled["terminal_tab"], 0);
        assert_eq!(disabled["row_enabled"], 0);
        assert!(buggy.successors("DecodeApplyTag", &disabled).is_empty());
    }

    /// The menu tree, structurally — the `chrome` serialiser's lines over
    /// section titles and row counts, so a label's rewording never churns it.
    /// One `menu "<title>": …` line per section, in the standard Mac order with
    /// the app's own Agents menu between View and Window; each line lists
    /// exactly the rows its section offers here (the dynamic update row only
    /// while live); submenu contents have their own lines, with no separator artifacts.
    #[test]
    fn chrome_lines_render_titled_sections() {
        let lines = menu_chrome_lines(None, false);
        let titles: Vec<&str> = MENU_MODEL.iter().map(|s| s.title).collect();
        assert_eq!(
            titles,
            [
                "aterm", "File", "Edit", "View", "Agents", "Window", "Help", "Version"
            ]
        );
        assert_eq!(
            lines.len(),
            titles.len() + 1,
            "sections plus the Find submenu"
        );
        for section in MENU_MODEL {
            let head = format!("menu {:?}: ", section.title);
            let line = lines.iter().find(|line| line.starts_with(&head)).unwrap();
            let offered = section
                .entries
                .iter()
                .filter(|entry| match entry {
                    MenuEntry::Item { action, .. } => {
                        action.offered_on_this_platform() && *action != MenuAction::ApplyUpdate
                    }
                    MenuEntry::Submenu { .. } => true,
                    MenuEntry::Separator => false,
                })
                .count();
            let listed = line[head.len()..]
                .split(", ")
                .filter(|label| !label.is_empty())
                .count();
            assert_eq!(listed, offered, "{line}");
        }
        assert!(
            !lines.iter().any(|l| l.contains(", ,")),
            "no separator artifact: {lines:?}"
        );
    }

    #[test]
    fn find_submenu_keeps_commands_shortcuts_and_palette_access() {
        let edit = super::MENU_MODEL
            .iter()
            .find(|section| section.title == "Edit")
            .unwrap();
        let nested = edit
            .entries
            .iter()
            .find_map(|entry| match entry {
                MenuEntry::Submenu {
                    label: "Find",
                    entries,
                } => Some(*entries),
                _ => None,
            })
            .expect("one Find submenu");
        assert!(
            !edit.entries.iter().any(|entry| matches!(
                entry,
                MenuEntry::Item {
                    action: MenuAction::Find | MenuAction::FindNext | MenuAction::FindPrev,
                    ..
                }
            )),
            "search commands are grouped, not repeated beside their submenu"
        );
        let commands: Vec<_> = nested
            .iter()
            .flat_map(MenuEntry::items)
            .map(|(_, action, key, mods)| (action, key, mods))
            .collect();
        assert_eq!(
            commands,
            [
                (MenuAction::Find, "f", super::MenuMods::Command),
                (MenuAction::FindNext, "g", super::MenuMods::Command),
                (MenuAction::FindPrev, "g", super::MenuMods::CommandShift),
            ]
        );
        let chrome = super::chrome_lines_for("Edit", edit.entries, None, false);
        assert!(chrome[0].contains("Find ▸"));
        let submenu_rows = chrome[1]
            .strip_prefix("menu \"Edit ▸ Find\": ")
            .expect("the nested menu has its own inspection path");
        assert_eq!(submenu_rows.split(", ").count(), commands.len());
        let palette = crate::palette::PaletteState::new();
        for name in ["Find", "FindNext", "FindPrev"] {
            let action = MenuAction::from_invoke_name(name).unwrap();
            assert_eq!(palette.action_by_name(name), Ok(action));
            assert_eq!(
                model_items()
                    .iter()
                    .filter(|(_, found, _, _)| *found == action)
                    .count(),
                1
            );
        }
    }

    /// `chrome` offers "Check for Updates…" exactly where an updater exists
    /// ([`aterm_update::enabled`]), and the Version menu's install row only there and only while
    /// it is live, agreeing with `controls menu` (the palette) and `update status`.
    #[test]
    fn update_rows_are_not_offered_where_no_updater_exists() {
        let offered = |label: &str| {
            menu_chrome_lines(None, false)
                .iter()
                .any(|l| l.contains(label))
        };
        let updater = aterm_update::enabled();
        // Anchor the oracle to the hosts this was measured on, so a change to
        // the updater's platform list cannot silently pass through here.
        if cfg!(windows) {
            assert!(!updater, "Windows has no in-app updater");
        }
        if cfg!(target_os = "macos") {
            assert!(updater, "macOS has the in-app updater");
        }
        assert_eq!(
            MenuAction::SoftwareUpdate.offered_on_this_platform(),
            updater
        );
        assert_eq!(MenuAction::ApplyUpdate.offered_on_this_platform(), updater);
        assert_eq!(offered("Check for Updates…"), updater);
        // Nothing staged: no install row, and never the model's placeholder.
        assert!(!offered("Install"), "{:?}", menu_chrome_lines(None, false));
        // Nothing else is platform-gated.
        for section in MENU_MODEL {
            for (_, action, _, _) in section.entries.iter().flat_map(MenuEntry::items) {
                if !matches!(action, MenuAction::SoftwareUpdate | MenuAction::ApplyUpdate) {
                    assert!(action.offered_on_this_platform(), "{action:?}");
                }
            }
        }
    }

    /// The load-bearing key equivalents are transcribed correctly: a wrong chord silently
    /// shadows a primary keybinding on macOS.
    #[test]
    fn critical_key_equivalents_are_correct() {
        let item = |action: MenuAction| {
            model_items()
                .into_iter()
                .find_map(|(_, a, key, mods)| (a == action).then_some((key, mods)))
                .unwrap_or_else(|| panic!("{action:?} not in MENU_MODEL"))
        };
        // ⇧⌘O, not "d", which would shadow ⇧⌘D (Split Down).
        assert_eq!(
            item(MenuAction::ViewSessionInNewWindow),
            ("o", super::MenuMods::CommandShift)
        );
        assert_eq!(
            item(MenuAction::ReopenClosedTab),
            ("t", super::MenuMods::CommandShift)
        );
        assert_eq!(
            item(MenuAction::SplitVertical),
            ("d", super::MenuMods::Command)
        );
        assert_eq!(
            item(MenuAction::SplitHorizontal),
            ("d", super::MenuMods::CommandShift)
        );
        assert_eq!(
            item(MenuAction::ToggleFullScreen),
            ("f", super::MenuMods::CommandControl)
        );
        // ⌘, is the Settings tab's; the aterm.toml escape hatch carries no chord to shadow it.
        assert_eq!(
            item(MenuAction::ToggleSettings),
            (",", super::MenuMods::Command)
        );
        assert_eq!(item(MenuAction::Preferences), ("", super::MenuMods::None));
        assert_eq!(
            item(MenuAction::OpenEditor),
            ("o", super::MenuMods::Command)
        );
        assert_eq!(item(MenuAction::OpenMarkdown), ("", super::MenuMods::None));
        // ⇧⌘L is the ledger key (`cmd+shift+l` in BUILTIN_CMD_CHORDS), shown on
        // the Harness Log row; ⇧⌘J and ⌃⌘O are the Agents menu's own. No other
        // row may claim any of them.
        for (action, key, mods) in [
            (MenuAction::HarnessLog, "l", super::MenuMods::CommandShift),
            (MenuAction::NextNeed, "j", super::MenuMods::CommandShift),
            (MenuAction::Overview, "o", super::MenuMods::CommandControl),
        ] {
            assert_eq!(item(action), (key, mods), "{action:?}");
            let claimed: Vec<MenuAction> = model_items()
                .into_iter()
                .filter(|(_, _, k, m)| *k == key && *m == mods)
                .map(|(_, a, _, _)| a)
                .collect();
            assert_eq!(claimed, [action], "{key} {mods:?}");
        }
    }

    /// The signature for every kind of build, on both rungs, composed from
    /// synthetic facts — the test binary is one dev build, so this is the only
    /// place a release, a dirty tree and a build without git are exercised.
    #[test]
    fn version_signature_composes_every_build_kind() {
        use super::{VersionFacts, VersionRung, version_signature};
        let facts = |release: bool, dev_commits: &'static str, commit: &'static str| VersionFacts {
            release,
            version: "0.101.0",
            dev_commits,
            commit,
        };
        let release = facts(true, "1576", "25a5963a4c21");
        assert_eq!(
            version_signature(release, VersionRung::Full).as_deref(),
            Some("v0.101.0")
        );
        assert_eq!(
            version_signature(release, VersionRung::Short),
            None,
            "a release has no short rung"
        );
        let dev = facts(false, "1576", "25a5963a4c21");
        assert_eq!(
            version_signature(dev, VersionRung::Full).as_deref(),
            Some("v0.101.1576+g25a5963 \u{00B7} DEV")
        );
        assert_eq!(
            version_signature(dev, VersionRung::Short).as_deref(),
            Some("v0.101.1576 \u{00B7} DEV")
        );
        let dirty = facts(false, "1576", "25a5963a4c21-dirty");
        assert_eq!(
            version_signature(dirty, VersionRung::Full).as_deref(),
            Some("v0.101.1576+g25a5963* \u{00B7} DEV")
        );
        assert_eq!(
            version_signature(dirty, VersionRung::Short).as_deref(),
            Some("v0.101.1576* \u{00B7} DEV"),
            "the short rung keeps the dirty mark"
        );
        // No git at build time: `build.rs` stamps `unknown` and a zero counter,
        // and the signature says exactly that rather than inventing a hash.
        let no_git = facts(false, "0", "unknown");
        assert_eq!(
            version_signature(no_git, VersionRung::Full).as_deref(),
            Some("v0.101.0+gunknown \u{00B7} DEV")
        );
        assert_eq!(
            version_signature(no_git, VersionRung::Short).as_deref(),
            Some("v0.101.0 \u{00B7} DEV")
        );
    }

    /// The macOS bar title is the full signature plus the colour glyphs only
    /// AppKit can draw: the wrench before it on a dev build and ⬆️ after it under
    /// attention. The strip writes its quiet form with a plain ` ↑` instead.
    #[test]
    fn the_bar_title_is_the_full_signature_plus_its_colour_glyphs() {
        use super::{
            VersionFacts, VersionRung, strip_version_label, version_menu_bar_title,
            version_signature,
        };
        let plain =
            version_signature(VersionFacts::this_build(), VersionRung::Full).expect("a full rung");
        let wrench = if crate::build_info::is_release_build() {
            ""
        } else {
            "\u{1F6E0}\u{FE0F} "
        };
        for attention in [false, true] {
            let arrow = if attention { " \u{2B06}\u{FE0F}" } else { "" };
            assert_eq!(
                version_menu_bar_title(attention),
                format!("{wrench}{plain}{arrow}"),
                "attention={attention}"
            );
        }
        let quiet = strip_version_label(VersionRung::Full, false).expect("a full rung");
        assert_eq!(
            strip_version_label(VersionRung::Full, true),
            Some(format!("{quiet} \u{2191}")),
            "the strip's attention mark is a plain U+2191 after one space"
        );
    }

    /// The strip's version is quiet: the version and the dev counter, no hash,
    /// no dirty mark, no `· DEV` (every Windows build is one); the short rung is
    /// the bare `v<maj.min>`. A release keeps its `v<version>`.
    #[test]
    fn the_strip_label_is_the_quiet_version() {
        use super::{VersionFacts, VersionRung, strip_version_text};
        let dirty = VersionFacts {
            release: false,
            version: "0.106.0",
            dev_commits: "3605",
            commit: "8b6f0a8c1d2e-dirty",
        };
        assert_eq!(
            strip_version_text(dirty, VersionRung::Full).as_deref(),
            Some("v0.106.3605")
        );
        assert_eq!(
            strip_version_text(dirty, VersionRung::Short).as_deref(),
            Some("v0.106")
        );
        let release = VersionFacts {
            release: true,
            ..dirty
        };
        assert_eq!(
            strip_version_text(release, VersionRung::Full).as_deref(),
            Some("v0.106.0")
        );
        assert_eq!(strip_version_text(release, VersionRung::Short), None);
    }

    /// The own-rendered strip has no colour-emoji face, so its label must never
    /// carry one: no character past the BMP, no variation selector, and neither
    /// of the bar's colour glyphs, on either rung, with or without attention.
    #[test]
    fn the_strip_label_carries_no_colour_emoji() {
        use super::{VersionFacts, VersionRung, strip_version_label, version_signature};
        for rung in [VersionRung::Full, VersionRung::Short] {
            for attention in [false, true] {
                let Some(label) = strip_version_label(rung, attention) else {
                    assert!(
                        crate::build_info::is_release_build() && rung == VersionRung::Short,
                        "only a release's short rung is absent"
                    );
                    continue;
                };
                for c in label.chars() {
                    assert!(u32::from(c) <= 0xFFFF, "{c:?} is past the BMP in {label:?}");
                    assert!(
                        !matches!(c, '\u{FE0F}' | '\u{1F6E0}' | '\u{2B06}'),
                        "{c:?} is a colour glyph in {label:?}"
                    );
                }
                assert!(
                    !label.contains("DEV"),
                    "the strip label is quiet: {label:?}"
                );
            }
        }
        let dirty = VersionFacts {
            release: false,
            version: "0.101.0",
            dev_commits: "1576",
            commit: "25a5963a4c21-dirty",
        };
        let short = version_signature(dirty, VersionRung::Short).expect("a dev short rung");
        assert!(short.contains('*'), "a dirty short rung keeps `*`: {short}");
    }

    /// The label opens About — and the update details only where an update is staged and this
    /// platform has an updater to describe. It never installs.
    #[test]
    fn version_slot_click_opens_about_and_details_only_where_offered() {
        use super::{MenuAction, version_slot_click};
        assert_eq!(version_slot_click(false), MenuAction::Version);
        let staged = version_slot_click(true);
        if MenuAction::SoftwareUpdate.offered_on_this_platform() {
            assert_eq!(staged, MenuAction::SoftwareUpdate);
        } else {
            assert_eq!(staged, MenuAction::Version);
        }
        #[cfg(windows)]
        assert_eq!(
            staged,
            MenuAction::Version,
            "Windows has no updater, so the label is always About"
        );
        for attention in [false, true] {
            assert_ne!(
                version_slot_click(attention),
                MenuAction::ApplyUpdate,
                "a version label never installs"
            );
        }
    }

    /// The title an outgoing process publishes for its successor is the
    /// successor's plain `v<version>`, and only when it can name it.
    #[test]
    fn the_handoff_target_title_is_the_successor_s_plain_version() {
        let b = 1_790_278_596;
        assert_eq!(
            super::handoff_target_bar_title(b, false, Some((b, "0.92.0"))),
            Some("v0.92.0".to_string())
        );
        assert_eq!(
            super::handoff_target_bar_title(b, true, Some((b, "0.92.0"))),
            None
        );
        assert_eq!(
            super::handoff_target_bar_title(b, false, Some((b + 1, "0.93.0"))),
            None
        );
        assert_eq!(
            super::handoff_target_bar_title(b, false, Some((b, ""))),
            None
        );
        assert_eq!(super::handoff_target_bar_title(b, false, None), None);
    }

    /// A staged record for the running build or an older one is not offered (plain title, no
    /// install row); a strictly newer build is, and a running build of `0` (a dev binary
    /// without a build number) filters nothing.
    #[test]
    fn the_version_menu_never_offers_the_running_build() {
        let running = 1_790_278_596;
        for stale in [running, running - 158_101] {
            let staged = super::staged_for_version_menu(Some((stale, "0.92.0")), running);
            assert_eq!(staged, None, "build {stale} is not newer than {running}");
            let title = super::version_menu_bar_title(super::bar_title_attention(
                staged.is_some(),
                false,
                false,
            ));
            assert_eq!(
                title,
                super::version_menu_bar_title(false),
                "no arrow over the running build"
            );
            assert!(!title.contains('\u{2B06}'), "{title}");
        }
        assert_eq!(
            super::staged_for_version_menu(Some((running + 1, "0.93.0")), running),
            Some((running + 1, "0.93.0")),
            "a strictly newer build is still the offer"
        );
        assert_eq!(
            super::staged_for_version_menu(Some((running, "0.92.0")), 0),
            Some((running, "0.92.0")),
            "an unnamed running build filters nothing"
        );
        assert_eq!(super::staged_for_version_menu(None, running), None);
    }

    /// The menu-bar arrow tracks a staged update only, never the post-update `realized` state,
    /// so an apply that re-execs into the staged build clears it at once.
    #[test]
    fn menu_bar_arrow_tracks_staged_not_realized() {
        // Action-needed staged update -> arrow.
        assert!(
            super::bar_title_attention(true, false, false),
            "staged shows the arrow"
        );
        // The post-update celebration is not a persistent bar badge.
        assert!(
            !super::bar_title_attention(false, false, true),
            "realized alone must not light the persistent bar arrow"
        );
        assert!(
            !super::bar_title_attention(false, false, false),
            "quiet steady state: no arrow"
        );
        // If a newer build stages right after an update, staged still wins.
        assert!(
            super::bar_title_attention(true, false, true),
            "staged wins over realized"
        );
        // A build on hold: no press may start it, so no act-now arrow.
        assert!(
            !super::bar_title_attention(true, true, false),
            "a staged build on hold shows no arrow"
        );
        // End to end: realized-only renders a plain title, no ⬆️ glyph.
        let realized_only =
            super::version_menu_bar_title(super::bar_title_attention(false, false, true));
        assert!(
            !realized_only.contains('\u{2B06}'),
            "realized-only bar title carries no arrow: {realized_only}"
        );
    }

    #[test]
    fn native_terminate_arbiter_deduplicates_and_allows_only_after_completion() {
        let mut arbiter = super::NativeTerminateArbiter::new();
        let super::NativeTerminateDecision::Dispatch(first) = arbiter.request() else {
            panic!("first request must dispatch");
        };
        assert_eq!(
            arbiter.request(),
            super::NativeTerminateDecision::DeferExisting(first)
        );
        assert!(arbiter.is_current(first));
        assert!(arbiter.complete(first));
        assert_eq!(
            arbiter.request(),
            super::NativeTerminateDecision::AllowExit(first)
        );
    }

    /// A quit that reaches an update's successor before Commit is resolved: the event loop
    /// swallows the wake (`App::drops_wake_before_commit`), so the drop cancels the generation
    /// (AppKit already answered Cancel) and its system mark, and the next request dispatches
    /// instead of meeting `DeferExisting` for the rest of the process's life.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_dropped_candidate_terminate_frees_the_arbiter() {
        let _serial = super::terminate_test_serial();
        let super::NativeTerminateDecision::Dispatch(logout) = super::admit_native_terminate(true)
        else {
            panic!("a first request dispatches");
        };
        assert!(crate::system_quit::asked_for(logout));
        let mut app = crate::App::headless_for_test();
        app.incoming_handoff_pending = true;
        assert!(
            app.drops_wake_before_commit(&crate::Wake::NativeTerminateRequested {
                generation: logout
            }),
            "before Commit the wake is still swallowed"
        );
        assert!(
            !crate::menu::native_termination_is_current(logout),
            "the swallowed quit is resolved"
        );
        assert!(
            !crate::system_quit::asked(),
            "a cancelled logout marks no later quit"
        );
        let next = super::admit_native_terminate(false);
        assert!(
            matches!(next, super::NativeTerminateDecision::Dispatch(g) if g != logout),
            "the next logout reaches the event loop: {next:?}"
        );
        // A committed successor handles it; the drop no longer applies.
        app.incoming_handoff_pending = false;
        assert!(
            !app.drops_wake_before_commit(&crate::Wake::NativeTerminateRequested {
                generation: next.generation()
            })
        );
        assert!(crate::menu::native_termination_is_current(
            next.generation()
        ));
        assert!(super::cancel_current_native_termination());
    }

    #[test]
    fn native_terminate_arbiter_rejects_stale_generations_after_retry() {
        let mut arbiter = super::NativeTerminateArbiter::new();
        let super::NativeTerminateDecision::Dispatch(first) = arbiter.request() else {
            panic!("first request must dispatch");
        };
        assert!(arbiter.cancel(first));
        let super::NativeTerminateDecision::Dispatch(second) = arbiter.request() else {
            panic!("retry must dispatch a fresh generation");
        };
        assert_ne!(first, second);
        assert!(!arbiter.cancel(first));
        assert!(!arbiter.complete(first));
        assert!(arbiter.is_current(second));
        assert!(arbiter.cancel_current());
        assert!(!arbiter.is_current(second));
    }

    /// Help's destination passes `is_safe_url`, the gate `open_url_external` trusts its callers
    /// to have run.
    #[test]
    fn the_help_destination_passes_the_url_gate() {
        let url = super::help_url();
        assert!(
            crate::is_safe_url(&url),
            "{url} must satisfy the same allowlist a clicked link does"
        );
    }

    // ---- the System Settings deep link (design §3.4, §3.7, Appendix A.2) --------

    /// Every candidate uses the dotted `x-apple.systempreferences` scheme. The hyphenated
    /// spelling is a different string (the one the link allowlist's rejection list names), and a
    /// one-character slip turns every deep link into a no-op.
    #[test]
    fn every_settings_deep_link_uses_the_dotted_scheme() {
        for pane in super::PrivacyPane::ALL {
            for url in super::privacy_settings_urls(pane) {
                assert!(
                    url.starts_with("x-apple.systempreferences:"),
                    "the scheme is dotted: {url:?}"
                );
                assert!(
                    !url.contains("x-apple-systempreferences"),
                    "the hyphenated spelling is a different scheme: {url:?}"
                );
            }
        }
        assert!(
            super::SETTINGS_FULL_DISK_ACCESS.ends_with("?Privacy_AllFiles"),
            "Full Disk Access is anchored at Privacy_AllFiles"
        );
        assert!(
            super::SETTINGS_FILES_AND_FOLDERS.ends_with("?Privacy_FilesAndFolders"),
            "the per-app repair route is anchored at Privacy_FilesAndFolders"
        );
        assert!(
            super::SETTINGS_DEVELOPER_TOOLS.ends_with("?Privacy_DevTools"),
            "the Developer Tools list is anchored at Privacy_DevTools"
        );
    }

    /// The last candidate is the unanchored Privacy & Security page, the same for every pane,
    /// so the deep link is never a dead end; only the three registered anchors ever ship
    /// (`Privacy_DocumentsFolder` and its siblings are not anchors).
    #[test]
    fn the_last_candidate_is_the_unanchored_pane_root() {
        let fda = super::privacy_settings_urls(super::PrivacyPane::FullDiskAccess);
        for urls in super::PrivacyPane::ALL.map(super::privacy_settings_urls) {
            assert_eq!(urls[2], fda[2], "every pane degrades to the same page");
            let (root, anchored) = urls.split_last().expect("three candidates");
            assert!(
                !root.contains('?'),
                "the degradation target carries no anchor: {root:?}"
            );
            for url in anchored {
                assert!(
                    url.contains('?'),
                    "the first candidates are anchored: {url:?}"
                );
            }
        }
        for url in super::PrivacyPane::ALL
            .iter()
            .flat_map(|pane| super::privacy_settings_urls(*pane))
        {
            let anchor = url.split_once('?').map(|(_, a)| a);
            assert!(
                matches!(
                    anchor,
                    None | Some(
                        "Privacy_AllFiles" | "Privacy_FilesAndFolders" | "Privacy_DevTools"
                    )
                ),
                "only a REGISTERED anchor may ship: {url:?}"
            );
        }
    }

    /// `is_safe_url` keeps rejecting every deep link: a pane a program can raise is a consent
    /// surface that program controls. The deep link has its own path to `NSWorkspace`, so the
    /// allowlist is never widened for it.
    #[test]
    fn program_output_still_cannot_open_a_settings_pane() {
        for pane in super::PrivacyPane::ALL {
            for url in super::privacy_settings_urls(pane) {
                assert!(
                    !crate::is_safe_url(url),
                    "a clicked link must never be able to open Settings: {url:?}"
                );
            }
        }
    }
}
