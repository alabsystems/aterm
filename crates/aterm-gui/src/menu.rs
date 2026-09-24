// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The native macOS application MENU BAR (the Apple `NSMenu` main menu).
//!
//! aterm installs a standard Mac menu bar — App (aterm) / File / Edit / View /
//! Window / Help with the usual items — so it presents as a native app. The menu
//! is built and installed once, after the window exists, in [`crate::App::resumed`]
//! (skipped under `--headless`, so tests create no menu and stay byte-identical).
//!
//! **No behavior duplication.** A menu item is a thin DISPATCH stub: it carries a
//! VISUAL key-equivalent only (so the shortcut shows next to the item) and, when
//! clicked, posts a [`Wake::MenuAction`](crate::Wake) carrying a [`MenuAction`].
//! The main loop's `user_event` turns that into a call on the SAME `App` command
//! method the existing keybinding uses (see `App::dispatch_menu_action`). The
//! real keypresses still flow through `App::on_key` exactly as before — the menu
//! adds a second entry point to the existing commands, never a parallel one.
//!
//! Each item's [`MenuAction`] is encoded in its `NSMenuItem.tag` (a plain
//! integer), so a SINGLE Objective-C action selector (`menuAction:`) reads the
//! sender's tag and forwards it — no per-item method, no per-item Rust object.
//! The action target is a small custom `NSObject` subclass that owns an
//! [`EventLoopProxy<Wake>`]; AppKit holds a target only weakly, so [`install`]
//! returns the retained target for the caller (`App`) to keep alive for the whole
//! run loop.
//!
//! Everything imperative is `#[cfg(target_os = "macos")]`. On other targets the
//! [`MenuAction`] enum and a no-op [`install`] still exist so the workspace builds
//! everywhere and `Wake::MenuAction { action }` is a valid variant on every target.

// macOS-only menu bar: on Linux `install` is a no-op stub, so the action
// enum/dispatch helpers here are intentionally unused there.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

/// Pure arbitration behind AppKit's synchronous `applicationShouldTerminate:`
/// callback. AppKit is answered immediately, while the real quit decision is
/// deferred onto the event loop where `App` owns document durability. A stable
/// generation makes delayed/duplicate callbacks harmless.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeTerminateDecision {
    Dispatch(u64),
    DeferExisting,
    AllowExit,
}

#[derive(Clone, Copy, Debug)]
struct NativeTerminateArbiter {
    next_generation: u64,
    pending: Option<u64>,
    exiting: bool,
}

impl NativeTerminateArbiter {
    const fn new() -> Self {
        Self {
            next_generation: 1,
            pending: None,
            exiting: false,
        }
    }

    fn request(&mut self) -> NativeTerminateDecision {
        if self.exiting {
            return NativeTerminateDecision::AllowExit;
        }
        if self.pending.is_some() {
            return NativeTerminateDecision::DeferExisting;
        }
        let generation = self.next_generation.max(1);
        self.next_generation = generation.wrapping_add(1).max(1);
        self.pending = Some(generation);
        NativeTerminateDecision::Dispatch(generation)
    }

    fn is_current(self, generation: u64) -> bool {
        !self.exiting && self.pending == Some(generation)
    }

    fn cancel(&mut self, generation: u64) -> bool {
        if self.pending != Some(generation) || self.exiting {
            return false;
        }
        self.pending = None;
        true
    }

    fn cancel_current(&mut self) -> bool {
        if self.exiting {
            return false;
        }
        self.pending.take().is_some()
    }

    fn complete(&mut self, generation: u64) -> bool {
        if self.pending != Some(generation) || self.exiting {
            return false;
        }
        self.pending = None;
        self.exiting = true;
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

pub(crate) fn native_termination_is_current(generation: u64) -> bool {
    with_native_terminate(|state| state.is_current(generation))
}

pub(crate) fn cancel_native_termination(generation: u64) -> bool {
    with_native_terminate(|state| state.cancel(generation))
}

pub(crate) fn cancel_current_native_termination() -> bool {
    with_native_terminate(NativeTerminateArbiter::cancel_current)
}

pub(crate) fn complete_native_termination(generation: u64) -> bool {
    with_native_terminate(|state| state.complete(generation))
}

pub(crate) fn complete_current_native_termination() -> bool {
    with_native_terminate(NativeTerminateArbiter::complete_current)
}

/// One menu command, identified independently of AppKit. The discriminant is the
/// integer stored in the originating `NSMenuItem.tag` and round-tripped back via
/// [`MenuAction::from_tag`]; `user_event` matches on the value to call the
/// matching existing `App` command method (`App::dispatch_menu_action`).
///
/// Standard AppKit responder items (window minimise/zoom/fullscreen, hide, quit)
/// are routed through here too, rather than via `nil`-target responder selectors,
/// so the WHOLE menu has one uniform, auditable dispatch path that lands in `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    // App menu
    /// About aterm — open the About route in the native Settings tab.
    About,
    /// Check for Updates… — the DETAILS update entry point. Opens the Software Update
    /// route in the native Settings tab AND kicks off a fresh
    /// check: current + staged build, what's-new notes, and Update to Latest Now. The same
    /// route the Version menu's "Update details…" item and the [`MenuAction::ApplyUpdate`]
    /// nothing-staged fallback open. (The old separate "Check for Updates…" NSAlert action
    /// — tag 34 — is folded into this one.)
    SoftwareUpdate,
    /// Version — the dedicated top-level menu-bar menu whose title shows `v<version>`
    /// (glanceable build/version identity; a trailing ⬆️ while an update is staged or
    /// freshly realized — see [`update_version_menu`]). Choosing its About item opens the
    /// native About route, which carries the full build & versioning
    /// details. Replaces the old floating top-right badge as the primary version surface.
    Version,
    /// ONE-CLICK UPDATE (`App::apply_update_or_details`): the Version menu's
    /// "⬆️ Install aterm v<staged> now" item / the palette's
    /// Version row / the
    /// "Update ready" notice pill / the off-macOS tab-strip ↻ all fire this. A
    /// strictly-newer STAGED build applies immediately (the same re-exec path
    /// `Wake::ApplyStagedUpdate` takes — no intermediate route change, per the owner's
    /// "click-upgrade" ask); with nothing actually staged it falls back to the Software
    /// Update route (honest details, never a silent dead click or a blind restart).
    ApplyUpdate,
    /// Open aterm.toml — open the canonical config in Settings ▸ Manual's native,
    /// assisted editor. This is cross-platform and shares the exact document/save host.
    Preferences,
    /// Quit aterm.
    Quit,
    // File menu
    /// New Window — a new in-process window of this App, sharing its sessions
    /// (`App::create_window_internal`); no second process is spawned.
    NewWindow,
    /// New Tab — a new in-window session (`App::open_tab`).
    NewTab,
    /// Choose one local UTF-8 file and open it in a read-only Markdown tab.
    OpenMarkdown,
    /// Choose one local UTF-8 file and open it in the native editor.
    OpenEditor,
    /// Reopen the most recently closed whole tab with fresh runtime identities.
    ReopenClosedTab,
    /// Reinsert the most recently closed split leaf at its original topology path.
    ReopenClosedView,
    /// Move Tab to New Window — pull the active tab out into a fresh in-process
    /// window (`App::detach_active_tab`).
    MoveTabToNewWindow,
    /// Move Tab to Next Window — move the active tab into the NEXT EXISTING window
    /// (wrapping; `App::migrate_active_tab_to_next_window`).
    MoveTabToNextWindow,
    /// Open Session in New Window — show the active session in a SECOND window
    /// (same live grid in two windows; `App::open_active_session_in_new_window`).
    ViewSessionInNewWindow,
    /// New Controlled Session in New Window (session connections, design §2.3):
    /// spawn a session the FOCUSED session holds a `both` connection over, in a
    /// fresh window; lineage `parent = focused` (`App::spawn_connected_session`).
    NewControlledWindow,
    /// New Controlled Session as Tab — the same mint, placed beside the focused
    /// session in its window.
    NewControlledTab,
    /// New Controller Session in New Window — the INVERSE preset: the newborn
    /// holds `both` over the focused session (a supervisor), its shell receives
    /// `ATERM_OBSERVE_SESSION_ID=<focused sid>`; a lineage root.
    NewControllerWindow,
    /// New Controller Session as Tab — the controller preset placed as a tab.
    NewControllerTab,
    /// Close Tab — close the active tab (`App::close_active_tab`).
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
    /// Increase font size (`set_font_px(font_px + step)`).
    FontIncrease,
    /// Decrease font size (`set_font_px(font_px - step)`).
    FontDecrease,
    /// Actual Size — reset to the default font size (`set_font_px(default)`).
    FontActualSize,
    /// Split the focused pane left/right (`split_focused_pane(Vertical)`).
    SplitVertical,
    /// Split the focused pane top/bottom (`split_focused_pane(Horizontal)`).
    SplitHorizontal,
    /// Toggle PHOSPHOR matrix rain for the FRONT SESSION of the frontmost window
    /// (`App::toggle_matrix_rain` — a per-session runtime override that wins over
    /// the `[matrix_rain]` config bit, in either direction, until the session
    /// ends). Terminal-only ([`requires_terminal_tab`]): a native whole tab has
    /// no session to toggle. The palette row's checkmark mirrors the front
    /// session's effective state.
    ToggleMatrixRain,
    /// Promote the front window's PROMOTABLE kitty — the tenured program cat
    /// on its glass, else the launch kitty (the base cat generated at launch,
    /// per computer; owner rulings 2026-08-17) — into the durable kitty
    /// registry and pin it as the cursor companion (`App::favourite_kitty_checked`).
    /// Owner: "if somebody really likes that kitty it goes into the kitty
    /// registry" — and since a pinned favourite outranks both program cats
    /// and the launch kitty, this is also how a cat you like is KEPT across
    /// launches and programs. Process-wide (never terminal-only: the launch
    /// kitty exists without a front session). The palette row's checkmark
    /// reports whether that promotable kitty is the current pin. One-way: the
    /// pin is transferable, not toggleable. Wire id 46 and the legacy invoke
    /// spelling `FavouriteSessionKitty` are kept ([`canonical_invoke_name`]).
    FavouriteKitty,
    /// **WEAR THE NEXT CAT IN THE COLLECTION.** The switch
    /// [`Self::FavouriteKitty`] cannot make: that one pins the cat that would
    /// ride ANYWAY (the focused window's tenured program cat, else the launch
    /// kitty), so pressing it never puts on a DIFFERENT cat — to wear another
    /// you had to make it appear first, by relaunching or by running a program
    /// long enough to earn tenure. This walks the collection instead, one cat
    /// per press, wrapping at the end, which is a picker you can use without
    /// a picker. `kitty wear <key>` on the control socket is the addressed
    /// form for anyone who knows which cat they want.
    NextKitty,
    /// Toggle the process-wide serious-mode policy. While enabled it suppresses
    /// every audible and decorative effect without overwriting the underlying
    /// preferences; disabling it restores those requested settings exactly.
    ToggleSeriousMode,
    /// Focus or create the process-singleton Settings tab. The app-menu Settings…
    /// item — ⌘, — uses the standard macOS settings chord.
    ToggleSettings,
    /// View ▸ Presence Band — checkable. Checked (the default): the band row
    /// under the tab bar follows its fold law; unchecked: the row is hidden
    /// and the window's `chrome` reports an empty band. Persisted as
    /// `[presence] band` in aterm.toml through the one serialized config lane
    /// (`App::user_toggle_presence`).
    TogglePresenceBand,
    /// View ▸ Presence Rim — checkable. Unchecked hides the colour rim (the
    /// band, when shown, still says the fact in words). Persisted as
    /// `[presence] rim`.
    TogglePresenceRim,
    /// Open Settings AT the Packages route (the batteries-included toolchain
    /// surface: install/update the ALab toolset, posture, consent switches).
    /// The app-menu Packages… item beneath Settings… — the menu-bar path to the
    /// same page the seed notice pill points at.
    Packages,
    /// Open Settings AT the Messages route (docs/DESIGN-unified-messages-2026-09-21.md
    /// §4): everything aterm told the person, newest first. The app-menu
    /// Messages… item beneath Packages… — the menu-bar path to the page the
    /// band's `Details ›` opens.
    Messages,
    /// Toggle the own-rendered, cross-platform command PALETTE overlay
    /// (`App::toggle_palette`).
    OpenPalette,
    // Tab-strip CONTEXT menu (session-metadata stage 2). These two live ONLY in
    // the per-tab right-click menu the native strip pops (`toolbar.rs` /
    // `session_chrome::compose_tab_menu`) — NOT in the menu bar, so they are
    // deliberately absent from `MENU_MODEL` (see the tests' TAB_CONTEXT_ACTIONS
    // twin list). They ride the same tag→action decode as every bar item.
    /// Copy the right-clicked tab's session id (the registry `sid`) to the
    /// clipboard — the handle an agent needs to address this session over the
    /// control socket (`meta`, `timeline`, `turn`, …).
    CopySessionId,
    /// Copy the right-clicked tab's shell-reported cwd (RAW, never
    /// `~`-abbreviated — a pasted path must be real) to the clipboard.
    CopyCwd,
    /// Connect to Session… (session connections, design §2.3): open the
    /// connection PICKER for the subject session (`session.connect_to`) —
    /// its selection opens the shared confirm/configure card for
    /// subject ⇄ chosen. Context-menu + palette; the tab-menu path carries
    /// the clicked tab as the subject, the palette/invoke path the front one.
    ConnectToSession,
    /// Show Connection Map (design §5): raise the instance's aggregated
    /// connection map. The map surface is a later slice — dispatch routes to
    /// the palette until it lands.
    ShowConnectionMap,
    /// Configure Connection… (`session.configure_connection`, design §2.3):
    /// the §2.5 sheet directly when the subject has exactly ONE connected
    /// peer, the picker when several, a refusal when none. Not a bar item;
    /// palette + `invoke` reachable like the picker/map ids.
    ConfigureConnection,
    /// Disconnect Session… (`session.disconnect`, design §2.3): dissolve the
    /// subject's connection — directly with one peer, via the picker when
    /// ambiguous, NEVER by guessing. A Fabric-menu row since round 19.
    DisconnectSession,
    // Fabric menu (round 19, SPEC19 §9): the menu bar's face of the fabric —
    // what `aterm fabric`, the inbox, the halt and the ledger already do on
    // the wire, reachable by a human from the bar.
    /// Fleet… — the fleet screen. Round 20 builds `/fleet`; until it lands
    /// this opens the Sessions/Connection Map (`App::open_connection_map`),
    /// and the item's help says so. Instance-wide, never greyed.
    Fleet,
    /// Inbox… — THIS window's focused session's inbox, METADATA ONLY (the
    /// `inbox --peek --meta` rows: id, offset, sender, kind, trust — never a
    /// body), opened as a Markdown tab. Terminal-only: a native tab has no
    /// inbox.
    Inbox,
    /// Ledger for This Session — the round-19 ledger key's action
    /// (`App::open_session_ledger`: `aterm drive ledger @<sid> --format html`,
    /// opened in the browser), with its ⇧⌘L accelerator shown on the row.
    LedgerForSession,
    /// Hold This Session — the Owner's LOCAL halt on the focused session
    /// (`fabric::cmd_hold` with `HoldIssuer::Owner`, `origin=local`). Greyed
    /// while any hold already stands; under a FLEET hold the row carries the
    /// fleet's reason and says it cannot be lifted here.
    HoldSession,
    /// Lift Hold (This Session) — lift the LOCAL hold. Enabled only while a
    /// local hold stands: a fleet hold is the bridge's to lift, so the row
    /// greys with the fleet reason (design §11.2, the withdrawn "a human
    /// lifts it at the GUI" — this is that lift, for the LOCAL origin only).
    LiftHold,
    /// Fabric Status… — runs `aterm fabric` (the status screen: config,
    /// broker, every instance's bridge, every inbox) as a child and opens its
    /// text in a Markdown tab.
    FabricStatus,
    /// Turn Fabric On… — the round-13 `aterm fabric on`, behind a
    /// confirmation sheet. Owner only.
    FabricOn,
    /// Turn Fabric Off… — `aterm fabric off`, behind a confirmation sheet.
    /// Owner only.
    FabricOff,
    // Window menu
    /// Edit the FOCUSED PANE's session pin in place on the tab strip — the
    /// keyboard/menu twin of double-clicking a tab. Named "Rename SESSION", not
    /// "Rename Tab", because it mutates session metadata (`meta set title`) and
    /// a tab can hold several split sessions. A bar item (not context-only) so
    /// it earns a palette row and an `invoke` name; it ALSO appears in the tab
    /// context menu, exactly as `CloseTab` does.
    RenameSession,
    /// Set Role… — the same inline editor over the active tab, editing the
    /// focused session's `meta role` (the presence band's first slot and the
    /// name a driven peer's band prints for its driver). Terminal-only, like
    /// the pin. (Round 18's identities put "Show Identity" — read-only —
    /// directly under this row; round 18 landed on main after this menu was
    /// designed, so the row is a follow-up: omitted here, not disabled.)
    SetRole,
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
    /// The integer stored in the menu item's `tag`. Stable, dense, starting at 1
    /// (0 is the `NSMenuItem` default tag, reserved so an untagged item never
    /// looks like a real action).
    #[must_use]
    pub fn tag(self) -> isize {
        match self {
            MenuAction::About => 1,
            MenuAction::Preferences => 2,
            // 3 was Hide aterm, removed — the tag stays reserved so it can never
            // be reused for a different command by accident.
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
            // 28, 29, 32, and 33 were bottom-HUD commands. Keep them reserved.
            MenuAction::ToggleSettings => 30,
            MenuAction::OpenPalette => 31,
            // 34 was "Check for Updates…" (a separate NSAlert check) — folded into
            // SoftwareUpdate (35), the details entry point. The tag stays reserved so
            // it can never be reused for a different command by accident.
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
            MenuAction::NewControlledWindow => 49,
            MenuAction::NewControlledTab => 50,
            MenuAction::NewControllerWindow => 51,
            MenuAction::NewControllerTab => 52,
            MenuAction::ConnectToSession => 53,
            MenuAction::ShowConnectionMap => 54,
            MenuAction::ConfigureConnection => 55,
            MenuAction::DisconnectSession => 56,
            MenuAction::NextKitty => 57,
            // Round 19 (SPEC19 §9): the Fabric menu, Set Role…, the presence
            // toggles. Dense from 58; the retired tags above stay retired.
            MenuAction::Fleet => 58,
            MenuAction::Inbox => 59,
            MenuAction::LedgerForSession => 60,
            MenuAction::HoldSession => 61,
            MenuAction::LiftHold => 62,
            MenuAction::FabricStatus => 63,
            MenuAction::FabricOn => 64,
            MenuAction::FabricOff => 65,
            MenuAction::SetRole => 66,
            MenuAction::TogglePresenceBand => 67,
            MenuAction::TogglePresenceRim => 68,
            // Phase 2 of the unified message system (2026-09-22).
            MenuAction::Messages => 69,
        }
    }

    /// Inverse of [`MenuAction::tag`]: recover the action from a menu item's tag,
    /// or `None` for an unknown/zero tag (defensive — the action selector ignores
    /// a tag it can't decode rather than dispatching the wrong command).
    #[must_use]
    pub fn from_tag(tag: isize) -> Option<MenuAction> {
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
            // 28, 29, 32, and 33 are retired bottom-HUD tags.
            30 => MenuAction::ToggleSettings,
            31 => MenuAction::OpenPalette,
            // 34 retired (was CheckForUpdates) — see `tag`.
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
            49 => MenuAction::NewControlledWindow,
            50 => MenuAction::NewControlledTab,
            51 => MenuAction::NewControllerWindow,
            52 => MenuAction::NewControllerTab,
            53 => MenuAction::ConnectToSession,
            54 => MenuAction::ShowConnectionMap,
            55 => MenuAction::ConfigureConnection,
            56 => MenuAction::DisconnectSession,
            57 => MenuAction::NextKitty,
            58 => MenuAction::Fleet,
            59 => MenuAction::Inbox,
            60 => MenuAction::LedgerForSession,
            61 => MenuAction::HoldSession,
            62 => MenuAction::LiftHold,
            63 => MenuAction::FabricStatus,
            64 => MenuAction::FabricOn,
            65 => MenuAction::FabricOff,
            66 => MenuAction::SetRole,
            67 => MenuAction::TogglePresenceBand,
            68 => MenuAction::TogglePresenceRim,
            69 => MenuAction::Messages,
            _ => return None,
        })
    }
}

/// Fold a legacy `invoke` spelling onto the current Debug token. The rule of
/// this table: a wire name an agent may already call is NEVER broken — it is
/// aliased. Round 19's menu rework (SPEC19 §9) renamed no variant, so every
/// pre-round-19 name still resolves as itself (pinned by
/// `every_pre_round_19_invoke_name_still_resolves`); the aliases here are the
/// spellings a caller could reasonably have written for a row whose bar
/// identity moved:
///
/// * `FavouriteSessionKitty` — the wire name of the favourite action until the
///   launch-kitty ruling (2026-08-17) retired the session kitty and renamed the
///   action [`MenuAction::FavouriteKitty`]; the numeric wire id (46) never moved.
/// * `OpenLedger` — the `[keybindings]` action name of the ledger key, for a
///   script that reaches the Fabric menu's "Ledger for This Session" row by
///   the name it already knows ([`MenuAction::LedgerForSession`]).
/// * `ShowFleet` / `ShowInbox` — the `Show…` spelling every earlier map row
///   used, for the Fabric menu's Fleet… and Inbox… rows.
#[must_use]
pub(crate) fn canonical_invoke_name(name: &str) -> &str {
    match name {
        "FavouriteSessionKitty" => "FavouriteKitty",
        "OpenLedger" => "LedgerForSession",
        "ShowFleet" => "Fleet",
        "ShowInbox" => "Inbox",
        other => other,
    }
}

/// Commands whose implementation requires a live terminal leaf. Native apps are
/// currently whole-tab surfaces, so advertising these while one is active would
/// promise a split/session operation the host cannot perform.
#[must_use]
pub(crate) const fn requires_terminal_tab(action: MenuAction) -> bool {
    matches!(
        action,
        MenuAction::SplitVertical
            | MenuAction::SplitHorizontal
            | MenuAction::ViewSessionInNewWindow
            // The rain toggle acts on the front SESSION; a native whole tab
            // has none, so the item greys out rather than dead-clicking.
            | MenuAction::ToggleMatrixRain
            // The pin is SESSION metadata; a native whole tab owns no session,
            // so the item greys rather than opening an editor over nothing.
            | MenuAction::RenameSession
            // The connected-spawn presets take the FOCUSED session as their
            // origin (the S of the §2.3 spawn rows) — a native whole tab has
            // no session to connect, so they grey out there.
            | MenuAction::NewControlledWindow
            | MenuAction::NewControlledTab
            | MenuAction::NewControllerWindow
            | MenuAction::NewControllerTab
            // The picker connects THIS session to a chosen peer — same
            // origin-needs-a-session rule as the presets, which also covers
            // the configure/disconnect ids (they act FROM the subject
            // session). (The map is instance-wide and deliberately not
            // listed.)
            | MenuAction::ConnectToSession
            | MenuAction::ConfigureConnection
            | MenuAction::DisconnectSession
            // The Fabric menu's SESSION rows (round 19): the inbox, the
            // ledger and the halt are the focused session's; the role is its
            // metadata, like the pin. Fleet…, the map and the three fabric
            // commands are instance-wide and deliberately not listed.
            | MenuAction::Inbox
            | MenuAction::LedgerForSession
            | MenuAction::HoldSession
            | MenuAction::LiftHold
            | MenuAction::SetRole
    )
}

/// The active-content bit read synchronously by AppKit's `validateMenuItem:`.
/// `sync_active_session` publishes it at the same stabilization point used for
/// title, toolbar, and control-handle changes, so opening the menu never exposes
/// terminal-only actions as enabled over a native whole tab.
static ACTIVE_TAB_IS_TERMINAL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

pub(crate) fn set_active_tab_is_terminal(terminal: bool) {
    ACTIVE_TAB_IS_TERMINAL.store(terminal, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the front window can actually PRESENT an inline rename editor. Off
/// macOS the editor is drawn by the tab strip, so `tab_strip_rows = 0` leaves
/// nowhere to put it — and an action that cannot run must not look available.
/// Published exactly like [`set_active_tab_is_terminal`], from the same
/// stabilization point, because AppKit validates menu commands synchronously
/// when a menu opens.
static RENAME_SURFACE_AVAILABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

pub(crate) fn set_rename_surface_available(available: bool) {
    RENAME_SURFACE_AVAILABLE.store(available, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn rename_surface_available() -> bool {
    RENAME_SURFACE_AVAILABLE.load(std::sync::atomic::Ordering::Relaxed)
}

/// The standing hold on the FRONT window's focused session, as the native
/// menu's synchronous `validateMenuItem:` needs it (the Packages doctrine —
/// SPEC19 §9: "every item's enabled state comes from the live projection,
/// never a stale index"). Published by [`set_front_hold`] from the same
/// stabilization point as [`set_active_tab_is_terminal`] AND from every
/// presence refresh of the front window, because a hold arrives on a fabric
/// wake, not on a tab switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrontHold {
    /// No hold stands: Hold This Session is live, Lift Hold is not.
    None,
    /// An `origin=local` hold stands: Lift Hold is live, Hold is not.
    Local,
    /// An `origin=fleet` hold stands: NEITHER is live — the bridge's to lift.
    Fleet,
}

static FRONT_HOLD: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
/// The standing hold's reason (the pct-encoded token `status hold=` carries),
/// beside the kind: the native bar's greyed halt pair says it when the hold
/// is the fleet's (SPEC19 §9), as the palette's rows already did.
static FRONT_HOLD_REASON: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Publish the front session's hold and its reason (`""` with no hold).
pub(crate) fn set_front_hold(hold: FrontHold, reason: &str) {
    let v = match hold {
        FrontHold::None => 0,
        FrontHold::Local => 1,
        FrontHold::Fleet => 2,
    };
    {
        let mut r = FRONT_HOLD_REASON.lock().unwrap_or_else(|p| p.into_inner());
        r.clear();
        if hold != FrontHold::None {
            r.push_str(reason);
        }
    }
    FRONT_HOLD.store(v, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn front_hold() -> FrontHold {
    match FRONT_HOLD.load(std::sync::atomic::Ordering::Relaxed) {
        1 => FrontHold::Local,
        2 => FrontHold::Fleet,
        _ => FrontHold::None,
    }
}

/// The published hold's reason token (empty with no hold).
pub(crate) fn front_hold_reason() -> String {
    FRONT_HOLD_REASON
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// The tool tip a native row carries at the moment the menu opens — the LIVE
/// projection, like its enabled bit: the row's help sentence, except the halt
/// pair under a FLEET hold, which says the fleet's reason and that the hold
/// cannot be lifted here. Stamped by `validateMenuItem:` on macOS.
pub(crate) fn native_item_tip(action: MenuAction) -> String {
    if matches!(action, MenuAction::HoldSession | MenuAction::LiftHold)
        && front_hold() == FrontHold::Fleet
    {
        return hold_row_reason(&front_hold_reason());
    }
    action.help().to_string()
}

/// The two View ▸ Presence checkables' LIVE state (`[presence] band` / `rim`,
/// as the App currently applies them), so `validateMenuItem:` can stamp the
/// checkmark synchronously when the menu opens. Published by
/// `App::publish_presence_toggles` on load, reload and every toggle.
static PRESENCE_BAND_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static PRESENCE_RIM_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub(crate) fn set_presence_toggles(band: bool, rim: bool) {
    PRESENCE_BAND_ON.store(band, std::sync::atomic::Ordering::Relaxed);
    PRESENCE_RIM_ON.store(rim, std::sync::atomic::Ordering::Relaxed);
}

/// The checkmark a checkable native row shows, or `None` for a plain command.
pub(crate) fn native_menu_checked(action: MenuAction) -> Option<bool> {
    match action {
        MenuAction::TogglePresenceBand => {
            Some(PRESENCE_BAND_ON.load(std::sync::atomic::Ordering::Relaxed))
        }
        MenuAction::TogglePresenceRim => {
            Some(PRESENCE_RIM_ON.load(std::sync::atomic::Ordering::Relaxed))
        }
        _ => None,
    }
}

/// The process-wide menu statics above (`ACTIVE_TAB_IS_TERMINAL`, `FRONT_HOLD`,
/// the presence bits) are shared by every test in the binary: a test that
/// MUTATES one holds this lock so two of them cannot interleave (the App-side
/// tests in `app_fabric_menu` publish through the same statics).
#[cfg(test)]
pub(crate) static MENU_STATICS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod headless_os_ui_tests {
    /// Every function in this file that parks the watchdog for a modal
    /// (`park_modal`) or asks `NSWorkspace` to open something (`openURL:`)
    /// consults [`super::os_ui_refused`] before it — a SOURCE pin, because the
    /// refusal only bites in a real headless process on the main thread, which
    /// a unit test cannot be. A new modal or open helper added without the
    /// check fails here by name.
    #[test]
    fn every_modal_and_workspace_open_asks_the_headless_refusal_first() {
        let src = include_str!("menu.rs");
        let lines: Vec<&str> = src.lines().collect();
        let is_fn = |l: &str| {
            let t = l.trim_start();
            t.starts_with("fn ") || t.starts_with("pub fn ") || t.starts_with("pub(crate) fn ")
        };
        let mut sites = 0;
        for (i, line) in lines.iter().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            // Spelled in pieces so this scan never matches its own needles — and
            // split before `!(`, so the crate's selector census
            // (aterm-objc tests/gui_sent_prototypes.rs) never reads one either.
            let parks = code.contains(concat!("park_", "modal()"));
            let opens = code.contains(concat!("sel", "!(openURL:)"));
            if !(parks || opens) {
                continue;
            }
            sites += 1;
            let start = (0..i)
                .rev()
                .find(|&j| is_fn(lines[j]))
                .unwrap_or_else(|| panic!("menu.rs:{}: no enclosing fn", i + 1));
            let asked = lines[start..i].iter().any(|l| {
                l.split("//")
                    .next()
                    .unwrap_or("")
                    .contains(concat!("os_ui_", "refused("))
            });
            assert!(
                asked,
                "menu.rs:{}: `{}` presents OS UI without asking `os_ui_refused` first \
                 (enclosing fn at menu.rs:{})",
                i + 1,
                line.trim(),
                start + 1
            );
        }
        // choose_local_file, confirm, notify park; open_in_workspace and its
        // delayed twin open. Fewer means the scan went blind.
        assert!(sites >= 5, "found only {sites} modal/open sites");
    }

    /// The refusal is off until `main_entry` marks the process headless — a
    /// unit-test process never is, so windowed behaviour is untouched here.
    #[test]
    fn a_process_not_marked_headless_is_never_refused() {
        assert!(!super::os_ui_refused("a probe"));
    }
}

fn native_menu_action_enabled(action: MenuAction) -> bool {
    if matches!(action, MenuAction::RenameSession | MenuAction::SetRole)
        && !rename_surface_available()
    {
        return false;
    }
    // The halt pair reads the live hold projection: Hold only with nothing
    // standing, Lift only against a LOCAL hold; a fleet hold greys both, and
    // its reason rides the greyed row's tool tip, stamped at the same moment
    // (`native_item_tip`, `hold_row_reason`).
    match action {
        MenuAction::HoldSession if front_hold() != FrontHold::None => return false,
        MenuAction::LiftHold if front_hold() != FrontHold::Local => return false,
        _ => {}
    }
    !requires_terminal_tab(action)
        || ACTIVE_TAB_IS_TERMINAL.load(std::sync::atomic::Ordering::Relaxed)
}

/// The words a greyed halt row carries when a FLEET hold stands: the reason,
/// and that it cannot be lifted here — the same sentence the presence band
/// speaks (`held, <reason>, fleet, cannot be lifted here`). `reason` is the
/// hold's pct-encoded token as `status hold=` carries it; a human reads the
/// decoded words (`main broken`, not `main%20broken`) — through the band's
/// own [`crate::presence::hold_reason_words`], which sanitizes what the
/// decode lets out: the token is bridge-supplied, and a `%e2%80%ae` in it
/// would otherwise reverse the row's text in the native tool tip, the
/// palette row and the a11y description (round 19's second review).
#[must_use]
pub(crate) fn hold_row_reason(reason: &str) -> String {
    format!(
        "fleet hold: {}, cannot be lifted here",
        crate::presence::hold_reason_words(reason)
    )
}

/// The CONTROL-AUTHORITY a socket caller must hold to reach a [`MenuAction`] through the
/// `invoke <action>` verb (and its `open <surface>` twins). The `invoke` verb's BASE op
/// is the benign `WriteInput` (it drives the human input vocabulary), but several menu
/// actions reach a strictly-greater capability that a plain child `WriteInput` edge must
/// NOT inherit — this is the classification the control layer's `escalated_op` reads to
/// fence those indirect seams. Compiler-EXHAUSTIVE over every variant (see
/// [`MenuAction::invoke_authority`]): a new `MenuAction` cannot compile until it is
/// classified here, so the fence can never silently miss a newly-added privileged action.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InvokeAuthority {
    /// Benign: the `invoke` verb's base `WriteInput` gate suffices (view/window/tab/
    /// navigation state, runtime-only font zoom — nothing durable or exfiltrating).
    WriteInput,
    /// Rewrites durable `aterm.toml` on invoke or raises the durable-config surface
    /// (`Preferences` opens Settings ▸ Manual;
    /// `ToggleSettings` raises Top Settings) — the `ConfigWrite` fine op.
    ConfigWrite,
    /// Moves the selection onto / reads it off the OS pasteboard — the same exfil/inject
    /// boundary as the `copy` verb, the `ClipboardWrite` fine op.
    ClipboardWrite,
    /// OWNER-ONLY: no single fine op expresses it. Either a GATEWAY that can reach EVERY
    /// action (`OpenPalette` — the palette dispatches any `MenuAction`) or a RE-EXEC of a
    /// staged update (`SoftwareUpdate`/`ApplyUpdate`, the MenuAction twins of the
    /// already-Owner-only `update` verb). Only the per-instance god token may run these.
    OwnerOnly,
}

impl MenuAction {
    /// The [`InvokeAuthority`] a socket caller must satisfy to fire this action via
    /// `invoke <action>`. EXHAUSTIVE and fail-closed: every variant is classified, so
    /// adding a `MenuAction` forces a decision here (and thus in the control-layer
    /// fence) rather than silently defaulting a new privileged action to `WriteInput`.
    pub(crate) fn invoke_authority(self) -> InvokeAuthority {
        // Force every platform menu face through the canonical command registry
        // before adapting to the legacy socket-op vocabulary below. The registry
        // carries the finer effect ceiling used by native reducers.
        let _ = crate::command_registry::menu_command(self);
        use InvokeAuthority::{ClipboardWrite, ConfigWrite, OwnerOnly, WriteInput};
        match self {
            // Clipboard exfil/inject boundary — the `copy` verb's fine op. The
            // tab-context copies land here too: `CopySessionId`/`CopyCwd` move
            // session identity/cwd text onto the OS pasteboard, the exact
            // boundary the fine op fences.
            MenuAction::Copy
            | MenuAction::Paste
            | MenuAction::SelectAll
            | MenuAction::CopySessionId
            | MenuAction::CopyCwd => ClipboardWrite,
            // Durable `aterm.toml` writes / the security-knob config surface.
            // `Packages` and `Messages` raise the SAME durable-config Settings
            // tab as `ToggleSettings`, just at their routes — same fence.
            MenuAction::Preferences
            | MenuAction::ToggleSettings
            | MenuAction::Packages
            | MenuAction::Messages
            | MenuAction::ToggleSeriousMode => ConfigWrite,
            // Gateway to every action + the staged-update re-exec twins.
            MenuAction::OpenPalette | MenuAction::SoftwareUpdate | MenuAction::ApplyUpdate => {
                OwnerOnly
            }
            // The connected-spawn presets MINT standing session-connection
            // authority over the focused session (design §5.3/§6) — the
            // `invoke` twin of the `spawn connected=` OwnerOnly escalation
            // fence: no fine op expresses "may create authority between
            // sessions", so only the god token may fire them.
            MenuAction::NewControlledWindow
            | MenuAction::NewControlledTab
            | MenuAction::NewControllerWindow
            | MenuAction::NewControllerTab => OwnerOnly,
            // The connection PICKER can mint the same standing authority the
            // presets do, the MAP is the instance-wide aggregated view (§5.3),
            // and configure/disconnect REWRITE/DISSOLVE standing authority —
            // all `invoke` twins of the `open connections` / `connect` /
            // `disconnect` OwnerOnly class.
            MenuAction::ConnectToSession
            | MenuAction::ShowConnectionMap
            | MenuAction::ConfigureConnection
            | MenuAction::DisconnectSession => OwnerOnly,
            // The Fabric menu (round 19). Fleet… is the map's twin (the
            // aggregated view). Inbox… puts a session's mail METADATA on the
            // human's screen — a disclosure of the same class as the map. The
            // ledger runs a child on THIS instance's own socket and raises the
            // browser. Hold/Lift are the `hold` verb, which is Owner-class on
            // the wire (`Access::OwnerOnly`) — a child edge must not halt or
            // lift. The three fabric commands run `aterm fabric` as a child:
            // `on`/`off` rewrite the machine's fabric (the round-13 verb is
            // Owner-only by its own doc), and `status` discloses the whole
            // fabric. No fine op expresses any of these.
            MenuAction::Fleet
            | MenuAction::Inbox
            | MenuAction::LedgerForSession
            | MenuAction::HoldSession
            | MenuAction::LiftHold
            | MenuAction::FabricStatus
            | MenuAction::FabricOn
            | MenuAction::FabricOff => OwnerOnly,
            // The two presence checkables write `[presence]` in aterm.toml —
            // durable config, the `ConfigWrite` fine op, exactly as Serious
            // Mode is classified.
            MenuAction::TogglePresenceBand | MenuAction::TogglePresenceRim => ConfigWrite,
            // Benign runtime/view/window/tab state. Font zoom is runtime-only
            // (`set_font_px` pins `font_px`; it does NOT persist to `aterm.toml`), so it
            // stays `WriteInput`. `Quit` is a denial of service, not a capability escalation, so it
            // is left with the base gate rather than over-reaching to Owner-only.
            MenuAction::About
            | MenuAction::Version
            | MenuAction::Quit
            | MenuAction::NewWindow
            | MenuAction::NewTab
            | MenuAction::OpenMarkdown
            | MenuAction::OpenEditor
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
            // Writes ONLY the machine-owned toy ledger (`kitty-collectibles.toml`
            // and its `kitty-log.toml` mirror) — never `aterm.toml`, no security
            // knob, no capability escalation. That is why it is not `ConfigWrite`.
            | MenuAction::FavouriteKitty
            // Same ledger, same reasoning: walking to the next collected cat
            // moves one pin stamp in the toy ledger and changes what the user
            // is looking at. Nothing durable outside it, so `WriteInput`, which
            // is also the class the `kitty` control verb carries.
            | MenuAction::NextKitty
            // Opens the inline pin editor. Its eventual write is `meta set title`,
            // which the control layer's own `escalated_op` already classifies as
            // `WriteInput` (not `ConfigWrite` — nothing durable on disk is
            // rewritten), so this matches rather than tunnels under it.
            | MenuAction::RenameSession
            // Same editor, same eventual write (`meta set role`), same class.
            | MenuAction::SetRole
            | MenuAction::Minimize
            | MenuAction::Zoom
            | MenuAction::NextTab
            | MenuAction::PrevTab
            | MenuAction::Help => WriteInput,
        }
    }

    /// The row's HELP: one sentence a human reads as the item's tooltip (the
    /// native `NSMenuItem` tool tip) and a screen reader hears as the palette
    /// row's description — the accessibility label SPEC19 §9 asks for on every
    /// item. Exhaustive, so a new action cannot ship without one. Plain
    /// sentences: no command text, no mail body, nothing a session wrote.
    #[must_use]
    pub(crate) const fn help(self) -> &'static str {
        match self {
            MenuAction::About => "Open About aterm: the build, version and signing details.",
            MenuAction::SoftwareUpdate => "Open Software Update and check for a newer build now.",
            MenuAction::Version => "Open About aterm from the version badge.",
            MenuAction::ApplyUpdate => {
                "Apply the staged update in place; every shell keeps running."
            }
            MenuAction::Preferences => "Open aterm.toml in the assisted config editor.",
            MenuAction::Quit => "Quit aterm.",
            MenuAction::NewWindow => "Open a new window of this aterm.",
            MenuAction::NewTab => "Open a new terminal tab in this window.",
            MenuAction::OpenMarkdown => "Choose a local file and open it as read-only Markdown.",
            MenuAction::OpenEditor => "Choose a local file and open it in the editor.",
            MenuAction::ReopenClosedTab => "Reopen the tab closed most recently.",
            MenuAction::ReopenClosedView => "Reinsert the split view closed most recently.",
            MenuAction::MoveTabToNewWindow => "Move this tab into a window of its own.",
            MenuAction::MoveTabToNextWindow => "Move this tab into the next window.",
            MenuAction::ViewSessionInNewWindow => {
                "Show this session in a second window, the same live grid in both."
            }
            MenuAction::NewControlledWindow => {
                "Spawn a session this one drives (a `both` connection), in a new window."
            }
            MenuAction::NewControlledTab => {
                "Spawn a session this one drives (a `both` connection), as a tab beside it."
            }
            MenuAction::NewControllerWindow => {
                "Spawn a supervisor that drives this session, in a new window."
            }
            MenuAction::NewControllerTab => {
                "Spawn a supervisor that drives this session, as a tab beside it."
            }
            MenuAction::CloseTab => "Close this tab.",
            MenuAction::Copy => "Copy the selection.",
            MenuAction::Paste => "Paste the clipboard.",
            MenuAction::SelectAll => "Select the whole screen.",
            MenuAction::Find => "Search the scrollback.",
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
            MenuAction::OpenPalette => "Open the command palette: every command, by name.",
            MenuAction::CopySessionId => "Copy this session's id, the handle `aterm ctl` takes.",
            MenuAction::CopyCwd => "Copy this session's working directory.",
            MenuAction::ConnectToSession => {
                "Choose a session to connect this one to, then confirm the direction."
            }
            MenuAction::ShowConnectionMap => "Show every session connection in this aterm.",
            MenuAction::ConfigureConnection => "Change the direction of this session's connection.",
            MenuAction::DisconnectSession => "Dissolve this session's connection.",
            MenuAction::Fleet => {
                "Show the fleet. Until the fleet screen lands (round 20) this opens \
                 the Sessions and Connection Map."
            }
            MenuAction::Inbox => {
                "Open this session's inbox as a tab: who wrote, what kind, how trusted; \
                 never the text of a message."
            }
            MenuAction::LedgerForSession => {
                "Write this session's ledger (`aterm drive ledger`) and open it in the browser."
            }
            MenuAction::HoldSession => {
                "Halt every driver of this session (a local hold); its shell keeps running."
            }
            MenuAction::LiftHold => "Lift this session's local hold.",
            MenuAction::FabricStatus => "Run `aterm fabric` and open its status screen as a tab.",
            MenuAction::FabricOn => {
                "Turn the fabric on for this machine (`aterm fabric on`), after confirming."
            }
            MenuAction::FabricOff => {
                "Turn the fabric off for this machine (`aterm fabric off`), after confirming."
            }
            MenuAction::RenameSession => "Name this session; the tab shows the name.",
            MenuAction::SetRole => {
                "Give this session a role (its `meta role`), the word the presence band leads with."
            }
            MenuAction::Minimize => "Minimise this window.",
            MenuAction::Zoom => "Zoom this window.",
            MenuAction::NextTab => "Show the next tab.",
            MenuAction::PrevTab => "Show the previous tab.",
            MenuAction::TogglePresenceBand => {
                "Show the presence band under the tab bar (saved in aterm.toml as [presence] band)."
            }
            MenuAction::TogglePresenceRim => {
                "Show the presence rim around the window (saved in aterm.toml as [presence] rim)."
            }
            MenuAction::Help => "Open the aterm guide.",
        }
    }

    /// Recover a [`MenuAction`] from the `{:?}` Debug token carried on the wire by
    /// `invoke <Name>` (the exact token [`crate::palette::PaletteState::action_by_name`]
    /// matches and `controls menu` prints), or `None` for an unknown token. The control
    /// layer uses this to classify a socket `invoke` BEFORE the action is dispatched, so
    /// a scoped edge is fenced from the privileged actions at the authority layer. Kept
    /// in lockstep with the enum by `invoke_name_round_trips`.
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
            "ReopenClosedTab" => Some(MenuAction::ReopenClosedTab),
            "ReopenClosedView" => Some(MenuAction::ReopenClosedView),
            "MoveTabToNewWindow" => Some(MenuAction::MoveTabToNewWindow),
            "MoveTabToNextWindow" => Some(MenuAction::MoveTabToNextWindow),
            "ViewSessionInNewWindow" => Some(MenuAction::ViewSessionInNewWindow),
            "NewControlledWindow" => Some(MenuAction::NewControlledWindow),
            "NewControlledTab" => Some(MenuAction::NewControlledTab),
            "NewControllerWindow" => Some(MenuAction::NewControllerWindow),
            "NewControllerTab" => Some(MenuAction::NewControllerTab),
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
            "RenameSession" => Some(MenuAction::RenameSession),
            "OpenPalette" => Some(MenuAction::OpenPalette),
            "Minimize" => Some(MenuAction::Minimize),
            "Zoom" => Some(MenuAction::Zoom),
            "NextTab" => Some(MenuAction::NextTab),
            "PrevTab" => Some(MenuAction::PrevTab),
            "Help" => Some(MenuAction::Help),
            "CopySessionId" => Some(MenuAction::CopySessionId),
            "CopyCwd" => Some(MenuAction::CopyCwd),
            "ConnectToSession" => Some(MenuAction::ConnectToSession),
            "ShowConnectionMap" => Some(MenuAction::ShowConnectionMap),
            "ConfigureConnection" => Some(MenuAction::ConfigureConnection),
            "DisconnectSession" => Some(MenuAction::DisconnectSession),
            "Fleet" => Some(MenuAction::Fleet),
            "Inbox" => Some(MenuAction::Inbox),
            "LedgerForSession" => Some(MenuAction::LedgerForSession),
            "HoldSession" => Some(MenuAction::HoldSession),
            "LiftHold" => Some(MenuAction::LiftHold),
            "FabricStatus" => Some(MenuAction::FabricStatus),
            "FabricOn" => Some(MenuAction::FabricOn),
            "FabricOff" => Some(MenuAction::FabricOff),
            "SetRole" => Some(MenuAction::SetRole),
            "TogglePresenceBand" => Some(MenuAction::TogglePresenceBand),
            "TogglePresenceRim" => Some(MenuAction::TogglePresenceRim),
            _ => None,
        }
    }
}

/// Modifier mask of a menu item's visual key-equivalent. Platform-neutral; the macOS
/// builder maps it to `NSEventModifierFlags`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuMods {
    /// No modifier (or no key-equivalent at all when `key` is empty).
    None,
    /// ⌘
    Command,
    /// ⇧⌘
    CommandShift,
    /// ⌃⌘
    CommandControl,
}

/// One entry of a menu section: a command item or a divider.
// macOS reads the live NSMenu for `chrome`, so these fields are read only off macOS + in
// tests (the serialiser/builder consume them there) — allow the per-target "never read".
#[derive(Clone, Copy, Debug)]
pub enum MenuEntry {
    Separator,
    Item {
        label: &'static str,
        action: MenuAction,
        /// Lowercase key-equivalent character ("" for none) — VISUAL only; the real
        /// keystroke is handled by `App::on_key`.
        key: &'static str,
        mods: MenuMods,
    },
    /// A nested menu (round 19: File ▸ Driving). Its `entries` are the same
    /// vocabulary — items and separators — one level down; the serialiser
    /// prints the row as `<label> ▸` in its parent's line and then the
    /// submenu on a line of its own (`menu "File ▸ Driving": …`), the
    /// palette flattens its items under `label`, and the macOS builder
    /// attaches it as a real `NSMenu`. One level deep on purpose: the chrome
    /// grammar and the palette chip both read better flat.
    Submenu {
        label: &'static str,
        entries: &'static [MenuEntry],
    },
}

impl MenuEntry {
    /// Walk this entry's ITEMS — its own, or a submenu's, in order — the one
    /// flattening every consumer shares (the completeness proof, the palette,
    /// the accelerator hints), so a submenu can never hide a command from one
    /// of them.
    pub(crate) fn items(
        &self,
    ) -> impl Iterator<Item = (&'static str, MenuAction, &'static str, MenuMods)> {
        let own = match self {
            MenuEntry::Item {
                label,
                action,
                key,
                mods,
            } => Some((*label, *action, *key, *mods)),
            MenuEntry::Separator | MenuEntry::Submenu { .. } => None,
        };
        let nested: &'static [MenuEntry] = match self {
            MenuEntry::Submenu { entries, .. } => entries,
            _ => &[],
        };
        own.into_iter().chain(nested.iter().filter_map(|e| match e {
            MenuEntry::Item {
                label,
                action,
                key,
                mods,
            } => Some((*label, *action, *key, *mods)),
            MenuEntry::Separator | MenuEntry::Submenu { .. } => None,
        }))
    }
}

/// A top-level menu (App / File / …) and its entries.
pub struct MenuSection {
    pub title: &'static str,
    pub entries: &'static [MenuEntry],
}

use MenuEntry::{Item, Separator};

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
    // macOS-only as a live verb: off macOS the in-app updater lane does not exist, so the
    // palette's resolve pass disables this row (`palette::PaletteState::resolve`) rather
    // than letting it silently no-op.
    Item {
        label: "Check for Updates…",
        action: MenuAction::SoftwareUpdate,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    // ⌘, focuses or creates the native Settings tab. "Open aterm.toml" opens
    // or reuses the separate native config Editor tab used by Manual.
    Item {
        label: "Settings…",
        action: MenuAction::ToggleSettings,
        key: ",",
        mods: MenuMods::Command,
    },
    // Settings at the /packages route — the batteries-included toolchain
    // surface (install/update the ALab toolset; the seed notice points here).
    Item {
        label: "Packages…",
        action: MenuAction::Packages,
        key: "",
        mods: MenuMods::None,
    },
    // Settings at the /messages route — the log behind the message band
    // (design §4.1); the band's `Details ›` opens the same page at an entry.
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
        label: "Quit aterm",
        action: MenuAction::Quit,
        key: "q",
        mods: MenuMods::Command,
    },
];

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
    // ROUND 18'S IDENTITY ROWS SLOT HERE, between New Terminal Tab and the
    // Driving submenu: "New Window With Identity…" and "New Tab With
    // Identity…", each an identity-picker submenu listing `identities` by
    // name plus "New identity…" (SPEC19 §9). They are OMITTED — not disabled
    // — in this slice: round 18 (identities) landed on main after this menu
    // was designed, so the two `Submenu` entries and the picker rows they
    // hold are the follow-up that reads the `identities` roster.
    Separator,
    // DRIVING (round 19): the session-connection spawn presets (design §2.3)
    // — the pre-fabric driving model's four rows, moved under one submenu
    // UNCHANGED: same labels, same actions, still no key equivalents
    // (deliberate, authority-minting acts).
    MenuEntry::Submenu {
        label: "Driving",
        entries: DRIVING_MENU,
    },
    Separator,
    Item {
        label: "Open Markdown…",
        action: MenuAction::OpenMarkdown,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Open File in Editor…",
        action: MenuAction::OpenEditor,
        key: "o",
        mods: MenuMods::Command,
    },
    Item {
        label: "Reopen Closed Tab",
        action: MenuAction::ReopenClosedTab,
        key: "t",
        mods: MenuMods::CommandShift,
    },
    Item {
        label: "Reopen Closed View",
        action: MenuAction::ReopenClosedView,
        key: "",
        mods: MenuMods::None,
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
    Item {
        label: "Close Tab",
        action: MenuAction::CloseTab,
        key: "w",
        mods: MenuMods::Command,
    },
];

/// File ▸ Driving — the four connected-spawn presets, exactly the rows the
/// File menu carried flat before round 19.
const DRIVING_MENU: &[MenuEntry] = &[
    Item {
        label: "New Controlled Session in New Window",
        action: MenuAction::NewControlledWindow,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "New Controlled Session as Tab",
        action: MenuAction::NewControlledTab,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "New Controller Session in New Window",
        action: MenuAction::NewControllerWindow,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "New Controller Session as Tab",
        action: MenuAction::NewControllerTab,
        key: "",
        mods: MenuMods::None,
    },
];

/// The FABRIC menu (round 19, SPEC19 §9) — the bar's face of the fabric,
/// replacing the palette-only "Connections" section: the fleet, this
/// session's inbox and ledger, the halt pair, the four connection rows
/// (unchanged), and the three `aterm fabric` commands. Placed between View
/// and Window, where an application's own menus go on macOS.
const FABRIC_MENU: &[MenuEntry] = &[
    Item {
        label: "Fleet…",
        action: MenuAction::Fleet,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Inbox…",
        action: MenuAction::Inbox,
        key: "",
        mods: MenuMods::None,
    },
    // The ledger key's accelerator, shown: ⇧⌘L is the chord `on_key` handles
    // (`cmd+shift+l` in BUILTIN_CMD_CHORDS); as with every ⌘ equivalent here
    // the keystroke itself is `App::on_key`'s. Off macOS the palette shows
    // the effective `open_ledger` chord instead (`menu_binding`).
    Item {
        label: "Ledger for This Session",
        action: MenuAction::LedgerForSession,
        key: "l",
        mods: MenuMods::CommandShift,
    },
    Separator,
    Item {
        label: "Hold This Session",
        action: MenuAction::HoldSession,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Lift Hold (This Session)",
        action: MenuAction::LiftHold,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    Item {
        label: "Connect to Session…",
        action: MenuAction::ConnectToSession,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Configure Connection…",
        action: MenuAction::ConfigureConnection,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Disconnect Session…",
        action: MenuAction::DisconnectSession,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Show Connection Map",
        action: MenuAction::ShowConnectionMap,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    Item {
        label: "Fabric Status…",
        action: MenuAction::FabricStatus,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Turn Fabric On…",
        action: MenuAction::FabricOn,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Turn Fabric Off…",
        action: MenuAction::FabricOff,
        key: "",
        mods: MenuMods::None,
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
        label: "Enter Full Screen",
        action: MenuAction::ToggleFullScreen,
        key: "f",
        mods: MenuMods::CommandControl,
    },
    Separator,
    // The presence surfaces (round 19): both checkable, both on by default,
    // both persisted under `[presence]`.
    Item {
        label: "Presence Band",
        action: MenuAction::TogglePresenceBand,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Presence Rim",
        action: MenuAction::TogglePresenceRim,
        key: "",
        mods: MenuMods::None,
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
        label: "Zoom",
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
    // No key equivalent by default: every free ⌘ letter near "rename" is either
    // claimed by the bar or by the shell. The chord is bindable instead
    // (`rename_session` in `[keybindings]`).
    Item {
        label: "Rename Session…",
        action: MenuAction::RenameSession,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Set Role…",
        action: MenuAction::SetRole,
        key: "",
        mods: MenuMods::None,
    },
    // ROUND 18'S "Show Identity" (read-only) SLOTS HERE, under Set Role…,
    // with the identity picker rows above. Omitted in this slice, not disabled.
];

const HELP_MENU: &[MenuEntry] = &[Item {
    label: "aterm Help",
    action: MenuAction::Help,
    key: "",
    mods: MenuMods::None,
}];

/// The dedicated top-level Version menu. Its live macOS title is the runtime
/// `v<version>` string (see [`version_menu_bar_title`] — with a trailing ⬆️ while an
/// update is staged/realized); the model uses the stable "Version" placeholder (the
/// model is only serialised off-macOS + in tests, where a runtime version string would
/// be non-deterministic). Two items: the ONE-CLICK update apply first (the PRIMARY
/// update affordance — the palette rewrites its label with the live staged/realized
/// version and REMOVES it when neither applies; the live NSMenu is rebuilt likewise by
/// [`update_version_menu`], which also appends a staged-only "Update details…"
/// SoftwareUpdate item — the one deliberate, documented dynamic divergence from this
/// static model), then About.
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
/// REALIZED celebration is NOT a bar badge; it lives in the menu's "Updated to aterm v… just
/// now" row and the transient LEVEL-UP notice / palette twin (self-dismissing after
/// [`crate::relaunch_notice::REALIZED_ARROW_TTL`]). The color emoji is safe HERE:
/// AppKit renders NSMenu titles/items with the system font + Apple Color Emoji
/// fallback. In-window overlay surfaces (palette rows, notice pill) must use plain `↑`
/// instead — the own-rendered text stack has no color-emoji face (verified coverage).
#[must_use]
pub(crate) fn version_menu_bar_title(attention: bool) -> String {
    let base = if crate::build_info::IS_RELEASE_BUILD {
        format!("v{}", crate::build_info::version_display())
    } else {
        // THE DEV SIGNATURE (owner, 2026-08-16: "some kind of marker in the
        // menu bar where the version numbers are so that I know what is a
        // development build … using the 3rd developer number and the hash").
        // A release's third slot is always literal 0, so the DEV COUNTER in
        // that slot (commits since the newest release tag) is an unambiguous
        // non-release signature; the short hash pins WHICH dev build, and the
        // trailing marker makes it readable at a glance without parsing
        // numbers. A dirty tree keeps its `*`. The display version is
        // display-only by contract, so none of this can reach an update
        // comparison.
        let v = crate::build_info::version_display();
        let majmin = v.rsplit_once('.').map_or(v, |(mm, _)| mm);
        let commit = crate::build_info::GIT_COMMIT;
        let dirty = if commit.ends_with("-dirty") { "*" } else { "" };
        let short: String = commit.chars().take(7).collect();
        format!(
            "\u{1F6E0}\u{FE0F} v{}.{}+g{short}{dirty} \u{00B7} DEV",
            majmin,
            crate::build_info::DEV_COMMITS,
        )
    };
    if attention {
        format!("{base} \u{2B06}\u{FE0F}")
    } else {
        base
    }
}

/// The apply-now label for a staged build, given the arrow glyph the calling surface
/// can actually render (`⬆️` in an NSMenu, plain `↑` in own-rendered overlay text).
///
/// WHY THE BUILD NUMBER IS NOT COSMETIC HERE. The updater orders releases by the
/// monotonic BUILD NUMBER, never by the display version — `build_info` states the
/// version is "display-only by contract" and "can never affect an update comparison".
/// So a strictly-newer build may legally carry the SAME `MAJOR.MINOR.PATCH` as the
/// running one, and the ledger really does ship such pairs (two distinct 0.11.0
/// builds). Meanwhile the menu-bar badge prints the RUNNING version and this row
/// printed the STAGED version, both dropping the one field that distinguishes them —
/// so a machine running a 0.14.0 build with the 0.14.0 RELEASE staged over it showed
/// "v0.14.0 ⬆️" above "Update to v0.14.0", i.e. an offer to update itself to itself.
/// Nothing was wrong with the update; the label just could not express it.
///
/// So: name the version when it actually differs, and fall back to the build number
/// — the thing the updater is really comparing — when it does not.
///
/// # The words are the update row's (2026-09-23)
///
/// "Install aterm vX now": the verb the status bar's row uses ("Updating to aterm
/// vX", "click to install"), so one update is described in one vocabulary on every
/// surface. The install is the in-session overlap handoff, which hands every
/// window, tab, split and live shell to the successor, so the row never asks for a
/// restart (it read "restart now" over exactly that mechanism until 2026-08-30),
/// and it no longer repeats "shells keep running" either — the row said it on every
/// surface of one flow.
///
/// `trouble` is the apply lane's standing failure for this exact build
/// (`App::apply_trouble_for`). While one is present the bare "now" is REPLACED by
/// [`crate::update_apply_trouble::ApplyTrouble::row_tail`], because a clean "install
/// now" beside a build that has already refused to start twice is an instruction
/// the machine has no reason to believe. On 2026-08-21 this row read "⬆️ Update to
/// v0.56.0 — restart now" for hours while `aterm ctl update status` carried
/// `failing_applies=2` and the reason both attempts died — the whole defect, in one
/// label. The tail still ends on what the row DOES, so a manual-only latch says
/// "try again now" and a scheduled retry says it will, instead of demanding an
/// action that is already on its way.
#[must_use]
pub(crate) fn staged_apply_label(
    arrow: &str,
    build: u64,
    version: &str,
    trouble: Option<&ApplyTrouble>,
) -> String {
    let what = if version == crate::build_info::version_display() {
        format!("build {build}")
    } else {
        format!("aterm v{version}")
    };
    match trouble {
        None => format!("{arrow} Install {what} now"),
        Some(trouble) => format!("{arrow} Install {what} — {}", trouble.row_tail()),
    }
}

/// Whether the always-visible menu-bar Version arrow should show. It tracks a STAGED
/// update ONLY (action needed) — deliberately NOT the post-update `realized`
/// celebration. The celebration is carried by self-dismissing surfaces (the menu's
/// "Updated to aterm v… just now" row, the LEVEL-UP notice, the palette twin), so an apply
/// that re-execs into the staged build (`staged` → `None`) clears the persistent bar
/// badge the instant it lands, instead of leaving an arrow up for the full realized
/// TTL that reads as "the update never resolved".
#[must_use]
pub(crate) fn bar_title_attention(staged_present: bool, _realized: bool) -> bool {
    staged_present
}

/// The WHOLE menu bar, declaratively — the platform-neutral description the
/// cross-platform `chrome` introspection serialiser ([`menu_chrome_lines`]) renders so
/// the menu is introspectable on EVERY platform (off macOS there is no `NSMenu` to read),
/// the palette flattens, AND — since round 19 — the macOS `install` builds its
/// `NSMenu`s from item-for-item (`macos::build_section`), so the model IS the menu:
/// what `chrome` prints off macOS and what the live bar reads back agree by
/// construction, not by a second hand-kept list. Order is the standard Mac arrangement
/// (App / File / Edit / View / <the app's own: Fabric> / Window / Help), the App section
/// titled with the app name by convention; the Version menu is the one section the
/// builder composes dynamically ([`macos::build_version_menu`], the documented
/// divergence — its rows depend on the update state).
///
/// A `#[test]` asserts every [`MenuAction`] appears here exactly once (the tab-context
/// copies excepted), submenus included.
pub const MENU_MODEL: &[MenuSection] = &[
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
        title: "Fabric",
        entries: FABRIC_MENU,
    },
    MenuSection {
        title: "Window",
        entries: WINDOW_MENU,
    },
    MenuSection {
        title: "Help",
        entries: HELP_MENU,
    },
    // The version identity lives in its own top-level menu placed LAST — after Help,
    // i.e. rightmost in the menu bar — so `v<version>` reads as a quiet trailing badge
    // and one click reaches About.
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
/// A SUBMENU (round 19, File ▸ Driving) prints twice, by the same rule both readers
/// follow: in its parent's line as `<label> ▸` (so the parent line stays one
/// comma-separated list of rows a human sees), and then on the very next line as its
/// own section titled `"<parent> ▸ <label>"` with its rows. One level deep, which is
/// all the model allows.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn menu_chrome_lines() -> Vec<String> {
    let mut out = Vec::with_capacity(MENU_MODEL.len() + 1);
    for section in MENU_MODEL {
        out.extend(chrome_lines_for(section.title, section.entries));
    }
    out
}

/// One section's line(s): the section itself, then each submenu's own line, in the
/// order the submenus appear. Shared with nothing on macOS (the live reader walks the
/// `NSMenu`), but the SHAPE it prints is the contract that reader byte-matches.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn chrome_lines_for(title: &str, entries: &[MenuEntry]) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    let mut nested: Vec<String> = Vec::new();
    for e in entries {
        match e {
            MenuEntry::Item { label, .. } => labels.push((*label).to_string()),
            MenuEntry::Separator => {}
            MenuEntry::Submenu {
                label,
                entries: sub,
            } => {
                labels.push(format!("{label} \u{25b8}"));
                let sub_title = format!("{title} \u{25b8} {label}");
                nested.extend(chrome_lines_for(&sub_title, sub));
            }
        }
    }
    let mut out = vec![format!("menu {title:?}: {}", labels.join(", "))];
    out.append(&mut nested);
    out
}

#[cfg(target_os = "macos")]
pub use macos::{
    MenuHandle, choose_local_file, confirm, confirm_owner, defer_quit_for_terminate, install,
    notify, open_file_in_workspace, open_help_url, update_version_menu,
};

/// Whether this process was launched `--headless`, published ONCE by
/// `main_entry` the moment the mode is decided ([`mark_process_headless`]).
///
/// A process-wide fact rather than an `App` field because the helpers that
/// read it are free functions every subsystem calls, and because the fact it
/// guards is process-wide too: a headless launch is `Prohibited`
/// (`launch_posture` in `lib.rs`), so AppKit will not present a window for it
/// at all. A modal it tried to run (`choose_local_file`, `confirm`, `notify`)
/// would park the main thread in a nested run loop nothing can see or dismiss,
/// and an `NSWorkspace` open (`open_in_workspace`: Open Log, Help, a Privacy
/// pane) would bring ANOTHER app to the front — the focus theft the posture
/// exists to stop, through a different door. Guarding here covers every caller,
/// including ones written later; the per-caller guards (the palette's picker
/// rows, the document-open path) stay as the honest "disabled" answer.
static PROCESS_HEADLESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that this process is headless. Called once, by `main_entry`.
pub(crate) fn mark_process_headless() {
    PROCESS_HEADLESS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether a helper about to present OS UI (a modal, or an open that fronts
/// another app) must refuse because this process is headless — saying so on
/// stderr, which is the only surface a headless instance has. Every such
/// helper in the macOS module asks this FIRST; a unit test pins that.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
pub fn open_file_in_workspace(_path: &std::path::Path) -> bool {
    false
}

/// Non-macOS no-op handle: there is no platform menu off macOS. Held by `App` in
/// the same field on every target so the struct shape is platform-independent.
#[cfg(not(target_os = "macos"))]
pub type MenuHandle = ();

/// Non-macOS stub: no platform menu bar exists, so installing one is a no-op that
/// installs nothing (`None`). Returns `Option<MenuHandle>` so the `resumed` call
/// site (`self._menu = menu::install(..)`) is identical on every target.
#[cfg(not(target_os = "macos"))]
pub fn install(_proxy: &winit::event_loop::EventLoopProxy<crate::Wake>) -> Option<MenuHandle> {
    None
}

/// Non-macOS stub: no native menu bar, so there is no Version menu to retitle/rebuild.
/// The palette's Version-section rows are the cross-platform mirror of this state.
#[cfg(not(target_os = "macos"))]
pub fn update_version_menu(
    _handle: &MenuHandle,
    _staged: Option<(u64, &str)>,
    _trouble: Option<&ApplyTrouble>,
    _realized: bool,
) {
}

/// Windows: the shell's own Common Item Dialog (`IFileOpenDialog`), with the
/// macOS panel's exact semantics — one existing local file, aliases resolved, no
/// type restriction, cancel is `None`. See [`crate::file_picker_win`].
#[cfg(windows)]
pub fn choose_local_file(title: &str, prompt: &str) -> Option<std::path::PathBuf> {
    crate::file_picker_win::choose(title, prompt)
}

/// No portable native picker is linked on the REMAINING targets (Linux). The
/// global action remains visible in the cross-platform command palette, but
/// acquires no filesystem authority there — and every surface that would spend
/// one asks [`local_file_picker_available`] first, so the row greys out instead
/// of accepting a dead click.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn choose_local_file(_title: &str, _prompt: &str) -> Option<std::path::PathBuf> {
    None
}

/// Whether [`choose_local_file`] can actually produce a path here — the ONE
/// predicate every picker-backed surface gates on (the palette's File rows, and
/// Settings ▸ Wallpaper's "Choose Image…").
///
/// It exists because those surfaces used to disagree: the palette greyed its rows
/// out on `cfg!(target_os = "macos")` while the Settings button stayed
/// unconditionally enabled, so on Windows one surface was honestly wrong (the
/// capability was there, only the dialog was missing) and the other was a dead
/// click. Routing both through here means a platform gains or loses the rows and
/// the button together, and can never gain one without the other.
///
/// Windows answers with a live probe rather than a `cfg!` — see
/// [`crate::file_picker_win::available`].
#[cfg(target_os = "macos")]
pub fn local_file_picker_available() -> bool {
    true
}

#[cfg(windows)]
pub fn local_file_picker_available() -> bool {
    crate::file_picker_win::available()
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn local_file_picker_available() -> bool {
    false
}

/// WINDOWS: a real modal alert. This was an empty body until the parity audit
/// found the consequence — a GUI-subsystem launch has no stderr, so every
/// failure routed through `notify` (a rejected document path, an update-check
/// result) reached the user through NO channel at all. The picker ships an
/// unrestricted `*.*` row, so one click can reach any of the document host's
/// rejections; silence there is the same class of defect as the dead button
/// the picker itself was added to fix.
#[cfg(windows)]
pub fn notify(title: &str, body: &str) {
    crate::win_alert_ok(title, body);
}

/// The remaining platforms (Linux) still have no native alert to wire; the
/// caller's own `eprintln!` is the channel there, and a Linux GUI launch does
/// keep its stderr.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn notify(_title: &str, _body: &str) {}

/// Help, off macOS: the project page in the default browser, through the SAME
/// helper a Ctrl-clicked link goes through (`open_url_external` —
/// `ShellExecuteW` on Windows, `xdg-open` on Linux).
///
/// This was an empty body while the palette's Help row stayed enabled on both
/// platforms, so the only command surface Windows has answered a row with
/// nothing at all. There is no bundled `Help.html` to prefer here: the macOS arm
/// reads it out of the `.app`'s `Contents/Resources`, which no other target has.
#[cfg(not(target_os = "macos"))]
pub fn open_help_url() {
    crate::app_mouse::open_url_external(HELP_URL);
}

/// The project page — Help's destination when no bundled guide is reachable
/// (every non-macOS target, and macOS outside the `.app`). One const so the two
/// arms cannot drift, and `is_safe_url`-clean because `open_url_external` trusts
/// its input.
const HELP_URL: &str = "https://github.com/alabsystems/aterm";

// ---------------------------------------------------------------------------
// System Settings deep links — the macOS privacy panes (design §3.4, §3.7)
// ---------------------------------------------------------------------------

/// Which section of Privacy & Security a deep link aims at.
///
/// Only two sections are spellable, and both are REGISTERED anchors in the
/// Settings extension's own `searchTerms` index (design Appendix A.2). The
/// per-folder strings that also appear in the extension's Mach-O
/// (`Privacy_DocumentsFolder` and its siblings) are NOT anchors — they name
/// sub-rows of one combined Files-and-Folders section — so they are not
/// offered here and cannot be reached by mistake.
///
/// `allow(dead_code)`: nothing CONSTRUCTS a pane yet — the Security block that
/// presses [`open_privacy_settings`] is Phase 2's, and the allow comes off with
/// it, exactly as that function's own note says.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PrivacyPane {
    /// Full Disk Access — the single grant macOS offers for this class.
    ///
    /// Which folders HOLDING it covers has not been measured on this machine
    /// (design §7 S4 is unrun), so neither this module nor its callers may say
    /// that the grant covers any particular one. Nothing in this block names a
    /// folder; the route strings below name only the pane.
    FullDiskAccess,
    /// The per-app Files & Folders list — §3.7's repair route, where a folder
    /// row that was already answered can be turned back on.
    FilesAndFolders,
}

/// Full Disk Access, through the modern Settings extension.
///
/// The scheme is `x-apple.systempreferences` — **with a dot**. The hyphenated
/// `x-apple-systempreferences` spelling is a different string, and it is the
/// one `crate::is_safe_url`'s rejection test names; keeping the two apart is
/// why [`open_privacy_settings`] documents its own call site rather than
/// borrowing the link allowlist.
pub(crate) const SETTINGS_FULL_DISK_ACCESS: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles";

/// Full Disk Access, through the legacy pane id the extension still declares
/// for itself (`legacyBundleIdentifier`). Tried only if the modern id is
/// refused outright.
const SETTINGS_FULL_DISK_ACCESS_LEGACY: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// The per-app Files & Folders list, through the modern Settings extension.
pub(crate) const SETTINGS_FILES_AND_FOLDERS: &str = "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_FilesAndFolders";

/// Files & Folders, through the legacy pane id.
const SETTINGS_FILES_AND_FOLDERS_LEGACY: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders";

/// The Privacy & Security page with NO anchor — the degradation target. It
/// carries no `?`, which is the invariant
/// `the_last_candidate_is_the_unanchored_pane_root` pins: the last entry of
/// [`privacy_settings_urls`] must always be an anchor-free page that opens
/// even if every anchor stops resolving.
const SETTINGS_PRIVACY_ROOT: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension";

/// Every URL that may be tried for `pane`, best first: the modern extension
/// id, the legacy alias, then the un-anchored pane root.
///
/// The last entry is why this can never be a dead end. None of the three is
/// Apple-documented; if the anchors stop resolving, the Privacy & Security
/// page still opens and the caller shows [`privacy_settings_path_words`] so
/// the rest of the route is in words.
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
    }
}

/// The route to `pane` in words — what a person reads when the anchor did not
/// land them on the section.
///
/// Always available to the caller, not only on the degraded path: an accepted
/// anchor is not a HONOURED anchor (`openURL:` answers "System Settings took
/// the URL", never "it scrolled to the row"), so the surface that opens the
/// pane can always say where to look. It names the pane and nothing else — no
/// folder, and no claim about what a grant there covers.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const fn privacy_settings_path_words(pane: PrivacyPane) -> &'static str {
    match pane {
        PrivacyPane::FullDiskAccess => {
            "System Settings \u{25b8} Privacy & Security \u{25b8} Full Disk Access"
        }
        PrivacyPane::FilesAndFolders => {
            "System Settings \u{25b8} Privacy & Security \u{25b8} Files & Folders"
        }
    }
}

/// What [`macos::open_privacy_settings`] actually achieved.
///
/// The caller needs the difference because the degraded outcomes change what
/// it must SAY, not merely what it logs: on anything but [`Self::Anchored`]
/// the words are the only route the person has.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SettingsOpen {
    /// An anchored URL was accepted. System Settings opened; whether it
    /// scrolled to the section is not observable from here, which is why the
    /// second open exists and why the words stay on screen.
    Anchored,
    /// Both anchors were refused; the un-anchored Privacy & Security page
    /// opened instead. The caller must show [`privacy_settings_path_words`].
    PaneRoot,
    /// Nothing opened — off the main thread, off macOS, or the shell refused
    /// every candidate. The caller must show the words and must not claim a
    /// pane is up.
    Refused,
}

/// Open System Settings at `pane` — the owner-gesture entry point the Security
/// page presses (design §3.4's FDA button, §3.7's repair link).
///
/// macOS does the work in [`macos::open_privacy_settings`], which documents why
/// a Settings URL may be opened from here and never from a hyperlink. Off macOS
/// there is no System Settings and no TCC, so nothing opens and the answer is
/// [`SettingsOpen::Refused`] — the same instruction to the caller as a refused
/// anchor: show the route in words.
///
/// A degraded outcome is logged with that route, because a windowed launch has
/// no stderr and "the button did nothing visible" is otherwise unexplainable
/// after the fact. The caller still has to SHOW the words; the log is the
/// diagnostic copy, not the user-facing one.
///
/// `allow(dead_code)`: this is the seam Phase 2's Security block presses, and
/// `native_settings.rs` is the only caller it will ever have. The allow comes
/// off with that call site.
#[allow(dead_code)]
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
    // THE SEAM IS CLOSED. `confirm` below was the ONLY thing in this module
    // still on `objc2`, held there because its key interceptor is
    // `crate::alert_keys`, whose `RcBlock` event monitor is shared with the
    // multi-line-paste SHEET in `lib.rs` — a different subsystem with its own
    // `NSAlert`, its own completion block and its own `PasteConfirm` state.
    // Porting one without the other would have left two spellings of the same
    // monitor, so W13 ported the whole modal-alert subsystem in one move:
    // `alert_keys.rs`, this function, and the sheet. There is no seam left to
    // import, which is why this block names only first-party crates.
    use winit::event_loop::EventLoopProxy;

    use crate::appkit::consts::{
        NS_ALERT_FIRST_BUTTON_RETURN, NS_EVENT_MODIFIER_FLAG_COMMAND,
        NS_EVENT_MODIFIER_FLAG_CONTROL, NS_EVENT_MODIFIER_FLAG_SHIFT, NS_MODAL_RESPONSE_OK,
    };
    use crate::appkit::{self, MainThread};

    use super::{
        MENU_MODEL, MenuAction, NativeTerminateArbiter, NativeTerminateDecision, PrivacyPane,
        SettingsOpen, privacy_settings_urls,
    };
    use crate::Wake;

    /// Installed before the AppKit terminate hook. `EventLoopProxy` is the only
    /// cross-thread/callback capability retained here; all document/application
    /// state remains on the event-loop thread.
    static TERMINATE_PROXY: std::sync::OnceLock<EventLoopProxy<Wake>> = std::sync::OnceLock::new();

    /// What [`install`] returns: the retained action target PLUS the Version menu's
    /// top-level `NSMenuItem` (rightmost in the bar), kept so [`update_version_menu`]
    /// can retitle it and rebuild its submenu live when an update stages / realizes /
    /// expires. AppKit references a menu item's target WEAKLY, so the target must
    /// outlive the run loop — `App` holds this handle in a field for the process
    /// lifetime. Named the same on every platform (`()` off macOS).
    pub struct MenuHandle {
        /// The single `menuAction:` relay target every item is wired to.
        target: Retained<MenuTarget>,
        /// The Version menu's top-level bar item (title `v<version>[ ⬆️]`), whose
        /// submenu [`update_version_menu`] rebuilds on update-state transitions.
        version_item: Obj,
    }

    aterm_objc::declare_class! {
        /// The target object for every menu item. Owns the `EventLoopProxy<Wake>`
        /// and exposes one `menuAction:` selector: it reads the sending
        /// `NSMenuItem`'s `tag`, decodes a [`MenuAction`], and posts a
        /// [`Wake::MenuAction`] so the main loop dispatches it on `App` (off the
        /// AppKit menu-tracking call, on the next loop turn). No menu logic lives
        /// here — it is a pure relay from AppKit into the existing `Wake` channel.
        ///
        /// `pub(crate)` so the `MenuHandle` alias (held in an `App` field) and
        /// `install`'s return type are not "more private than the item" — the
        /// type itself is never named outside this module.
        ///
        /// Declared with [`aterm_objc::declare_class!`]. The class name, the
        /// superclass, both selectors and the behaviour are unchanged from the
        /// `objc2` declaration this replaces. What objc2 spelled
        /// `type Mutability = InteriorMutable` has no counterpart and needs none:
        /// the proxy is never mutated, [`aterm_objc::Retained`] is
        /// unconditionally `!Send`, and the two entry points take a
        /// [`MainThread`] witness.
        pub(crate) struct MenuTarget: NSObject {
            const NAME: &str = "ATermMenuTarget";
            type Ivars = EventLoopProxy<Wake>;

            /// `menuAction:` — the single selector wired to every item. `sender`
            /// is the clicked `NSMenuItem`; its `tag` decodes to the action. A tag
            /// that doesn't decode is ignored (no dispatch), so a stray/zero tag
            /// is inert rather than firing the wrong command.
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
                    let _ = self.ivars().send_event(Wake::MenuAction { action });
                }
            }

            /// AppKit asks this immediately before displaying/dispatching a menu
            /// item. Terminal-only commands grey out over a native whole tab instead
            /// of accepting a click that the host can only ignore.
            ///
            /// The return is [`aterm_objc::Bool`], not Rust's `bool`, and that is
            /// D3's rule rather than a style choice: on the `x86_64-apple-darwin`
            /// compat slice `BOOL` is `signed char`, so `bool` is the wrong ABI
            /// type. objc2 hid the conversion inside `#[method]`; here it is
            /// written once, in the return position.
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
                // A CHECKABLE row (View ▸ Presence Band / Rim) shows the live
                // state as its checkmark, stamped here because AppKit asks at
                // exactly the moment the menu opens — the same live-projection
                // rule the enabled bit follows. `-setState:` is
                // `-(void)(NSControlStateValue)`, an NSInteger: 1 on, 0 off.
                if let Some(on) = super::native_menu_checked(action) {
                    // SAFETY: a plain setter on the live NSMenuItem AppKit
                    // passed, main thread, no preconditions.
                    unsafe { appkit::send_v_isize(sender, sel!(setState:), isize::from(on)) };
                }
                // The halt pair's tool tip is live too: under a FLEET hold the
                // greyed row says the fleet's reason and that it cannot be
                // lifted here (SPEC19 §9), and it returns to the row's help
                // when the hold lifts.
                stamp_native_tip(sender, action);
                aterm_objc::Bool::new(super::native_menu_action_enabled(action))
            }
        }
    }

    /// Stamp the halt pair's LIVE tool tip on `item` (`native_item_tip`):
    /// called by `validateMenuItem:` at the moment the menu opens, so the
    /// greyed row explains itself. Every other row keeps the help sentence
    /// `add_item_mods` set once.
    fn stamp_native_tip(item: Id, action: MenuAction) {
        if !matches!(action, MenuAction::HoldSession | MenuAction::LiftHold) {
            return;
        }
        if let Some(tip) = appkit::nsstring(&super::native_item_tip(action)) {
            // SAFETY: `-setToolTip:` is `-(void)(NSString *)` on the live
            // NSMenuItem the caller holds; it COPIES its argument.
            unsafe { appkit::send_v_id(item, sel!(setToolTip:), tip.id()) };
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
    pub fn install(proxy: &EventLoopProxy<Wake>) -> Option<MenuHandle> {
        let main_thread = MainThread::new()?;
        let _ = TERMINATE_PROXY.set(proxy.clone());
        let target = MenuTarget::alloc_init(main_thread, proxy.clone())?;

        let main = new_menu()?;

        // Each section is built FROM THE MODEL (`MENU_MODEL`, round 19) and
        // attached under its title, in the model's order — App / File / Edit /
        // View / Fabric / Window / Help; the Version menu below is the one
        // dynamic section, so the model's static placeholder for it is skipped.
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
        // The version identity goes LAST — after Help, so `v<version>` is the rightmost
        // menu-bar title (a quiet trailing build badge). Installed in its PLAIN state
        // (no update staged at boot); `App::refresh_version_menu` retitles it (via
        // [`update_version_menu`], through the retained item below) when an update
        // stages or the post-update realized arrow appears/expires. A bare top-level
        // item with an action greys out in the main menu bar, so its commands are
        // reached via this submenu (the reliable, idiomatic AppKit shape).
        let version_item = attach_submenu(
            &main,
            &super::version_menu_bar_title(false),
            build_version_menu(&target, None, None, false)?,
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

    /// Re-sync the LIVE Version menu (title + items) to the update state — the hook the
    /// `Wake::UpdateStaged` handler, the post-update boot, and the realized-arrow TTL
    /// sweep call (`App::refresh_version_menu`). `staged` is the strictly-newer
    /// `(build, version)` ready to apply; `realized` marks the freshly-updated arrow
    /// window. Rebuilding the submenu (rather than toggling item hidden-flags) keeps the
    /// item set an exact function of the state — no stale "install now" rows. Best-effort:
    /// off the main thread it is a no-op (never a panic), like every AppKit helper here.
    ///
    /// The persistent MENU-BAR arrow tracks `staged` ONLY — it means "an update is
    /// waiting, act on it". After an apply re-execs into that build `staged` is `None`,
    /// so the bar arrow clears the instant the update lands (no 10-min lingering badge
    /// that reads as "the update never resolved"). The freshly-REALIZED celebration
    /// still lives INSIDE the menu — its "Updated to aterm v… just now" row — and in the
    /// transient LEVEL-UP notice / palette twin, both of which self-dismiss; only the
    /// always-visible bar badge is gated to the action-needed state.
    pub fn update_version_menu(
        handle: &MenuHandle,
        staged: Option<(u64, &str)>,
        trouble: Option<&super::ApplyTrouble>,
        realized: bool,
    ) {
        let Some(_main_thread) = MainThread::new() else {
            return;
        };
        let title =
            super::version_menu_bar_title(super::bar_title_attention(staged.is_some(), realized));
        let Some(submenu) = build_version_menu(&handle.target, staged, trouble, realized) else {
            return;
        };
        let Some(ns_title) = appkit::nsstring(&title) else {
            return;
        };
        // Set the title on BOTH the bar item and the submenu: AppKit takes a top-level
        // bar title from whichever is authoritative for the toolkit version in play
        // (historically the submenu's title), so writing both is the robust retitle.
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

    /// The Version submenu for the given update state. Mirrors [`super::VERSION_MENU`]
    /// in the portable model (whose ApplyUpdate row the palette rewrites the same way):
    ///   * STAGED: "⬆️ Install aterm v<staged> now" (ONE click installs in place — the
    ///     owner's "click-upgrade" ask), then About, then
    ///     "Update details…" (the
    ///     Software Update route stays reachable as the DETAILS surface).
    ///   * REALIZED (fresh post-update, no new stage): "⬆️ Updated to aterm v<current> just
    ///     now" (fires About — the celebration row is informative, not destructive),
    ///     then About.
    ///   * NEITHER: just About — the quiet steady-state badge menu.
    fn build_version_menu(
        target: &MenuTarget,
        staged: Option<(u64, &str)>,
        trouble: Option<&super::ApplyTrouble>,
        realized: bool,
    ) -> Option<Obj> {
        let menu = new_menu()?;
        if let Some((build, version)) = staged {
            add_item(
                &menu,
                target,
                &super::staged_apply_label("\u{2B06}\u{FE0F}", build, version, trouble),
                MenuAction::ApplyUpdate,
                "",
                false,
            );
            add_separator(&menu);
        } else if realized {
            add_item(
                &menu,
                target,
                &format!(
                    "\u{2B06}\u{FE0F} Updated to aterm v{} just now",
                    crate::build_info::version_display()
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
    /// separator as a separator, and every [`MenuEntry::Submenu`] as a real
    /// nested `NSMenu` (one level, as the model allows). The ONE builder for
    /// every static section since round 19 — so the live bar the `chrome`
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
                super::MenuEntry::Separator => add_separator(&menu),
                super::MenuEntry::Submenu {
                    label,
                    entries: sub,
                } => {
                    if let Some(sub) = build_section(target, sub) {
                        let _ = attach_submenu(&menu, label, sub);
                    }
                }
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

    /// `Cmd-Ctrl` mask (Enter Full Screen's standard equivalent).
    fn command_control_mask() -> usize {
        NS_EVENT_MODIFIER_FLAG_COMMAND | NS_EVENT_MODIFIER_FLAG_CONTROL
    }

    /// `Cmd-Shift` mask (Move Tab to New Window's ⇧⌘N equivalent).
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

    /// Build one menu item wired to `menuAction:` on `target`, tagged with
    /// `action`, and append it to `menu`. `key` is the lowercase key-equivalent
    /// character ("" for none); `cmd` adds the ⌘ modifier (a Cmd shortcut). The
    /// equivalent is VISUAL only — it just renders next to the item; the actual
    /// keystroke is still handled by `App::on_key`.
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
        // Build with the menuAction: selector so AppKit dispatches to `target`.
        let Some(item) = new_item(title, sel!(menuAction:), key) else {
            return;
        };
        // The row's help sentence (`MenuAction::help`) is its tool tip — what a
        // human hovers and what VoiceOver reads as the item's help (round 19:
        // an accessibility label on every item).
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
    pub fn choose_local_file(title: &str, prompt: &str) -> Option<std::path::PathBuf> {
        if super::os_ui_refused(title) {
            return None;
        }
        let _main_thread = MainThread::new()?;
        // `-runModal` below spins a nested run loop for as long as the panel is
        // up; park the main-thread watchdog for exactly that long.
        let _park = crate::watchdog::park_modal();
        let title = appkit::nsstring(title)?;
        let prompt = appkit::nsstring(prompt)?;
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

    /// Attach `submenu` under a new top-level item titled `title` on `bar`, returning
    /// the retained bar item so a caller can keep a live handle to it (the Version
    /// menu is retitled/rebuilt through its item — see [`update_version_menu`]; the
    /// other menus ignore the return). The item carries no action (its only job is to
    /// hold the submenu). The submenu is titled to match: AppKit takes a top-level
    /// bar title from the submenu on some paths, so both must agree.
    fn attach_submenu(bar: &Obj, title: &str, submenu: Obj) -> Option<Obj> {
        let ns_title = appkit::nsstring(title)?;
        // A top-level bar item carries NO action — its only job is to hold the
        // submenu — which is [`Sel::NULL`], the value W2 had to add to
        // `aterm-objc` to express this at all.
        let item = new_item(title, Sel::NULL, "")?;
        // SAFETY: `-setTitle:` is `-(void)(NSString *)`, `-setSubmenu:` is
        // `-(void)(NSMenu *)` and RETAINS, `-addItem:` is `-(void)(NSMenuItem *)`
        // and RETAINS — so both the +1 title and the moved `submenu` may drop at
        // the end of this frame, exactly as the objc2 form's `Retained` did.
        unsafe {
            appkit::send_v_id(submenu.id(), sel!(setTitle:), ns_title.id());
            appkit::send_v_id(item.id(), sel!(setSubmenu:), submenu.id());
            appkit::send_v_id(bar.id(), sel!(addItem:), item.id());
        }
        Some(item)
    }

    /// Show a native modal confirmation alert (a ⌘Q quit, or a close gesture that
    /// would lose work) and block until the user answers. `title` is the primary
    /// message, `body` the secondary explanatory line, and `proceed_label` titles the
    /// affirmative (destructive) button — the DEFAULT button; a "Cancel" button is
    /// always added and Escape maps to it. Returns `true` iff the user chose to proceed.
    ///
    /// `runModal` spins a nested modal run loop on the main thread (the standard
    /// AppKit pattern, the same one native file pickers use), so it is safe to call
    /// straight from the winit event handler. Best-effort: if somehow off the main
    /// thread it returns `true` (proceed) so a quit can never wedge.
    ///
    /// # Return with ⌘ held
    ///
    /// The default button's Return equivalent carries an EMPTY modifier mask and
    /// AppKit's key-equivalent match is exact, so ⌘Return answers this alert no more
    /// than it answered the paste sheet — and ⌘Q is precisely a gesture that leaves ⌘
    /// under the finger. So the same [`crate::alert_keys`] interceptor is installed for
    /// the duration of the `runModal` call: Return (any modifiers) clicks PROCEED,
    /// Escape clicks Cancel, everything else passes through. The watch is a LOCAL whose
    /// scope ends with the blocking call, so it cannot outlive the alert.
    pub fn confirm(title: &str, body: &str, proceed_label: &str) -> bool {
        // Headless never shows a confirm: proceed, as `confirm_destructive_close`
        // already does for a headless instance. Every other case where no alert
        // could be shown proceeds too: a quit that cannot ask must not wedge.
        ask(title, body, proceed_label).unwrap_or(true)
    }

    /// [`confirm`] for an owner gesture that changes this Mac — the Security
    /// panel's reset, warm-up and *Move to Trash*. It fails CLOSED: with no
    /// window, off the main thread, or with no alert built, the answer is no.
    /// Control input arrives as a `Wake`, which is queued while `runModal` spins
    /// and never becomes an event the alert receives, so no control verb can
    /// answer it.
    pub fn confirm_owner(title: &str, body: &str, proceed_label: &str) -> bool {
        ask(title, body, proceed_label) == Some(true)
    }

    /// The alert behind [`confirm`] and [`confirm_owner`]: `None` when none
    /// could be shown, else whether the user chose to proceed.
    fn ask(title: &str, body: &str, proceed_label: &str) -> Option<bool> {
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
        // A missing alert is "could not ask" too. The `objc2` form could not
        // reach this case — `msg_send_id![…, new]` panicked on nil.
        // SAFETY: `+[NSAlert new]` is `+(instancetype)` and +1 (alloc+init),
        // which is what `Obj::from_owned` adopts.
        let alert =
            unsafe { Obj::from_owned(appkit::send_id(class(c"NSAlert").as_id(), sel!(new))) }?;
        // SAFETY: standard `NSAlert` setters + `runModal`, all on the main thread
        // (`MainThread::new()` above proves it). `-setMessageText:` and
        // `-setInformativeText:` are `-(void)(NSString *)` and COPY their argument,
        // so the +1 strings may drop at the end of this frame.
        // `-addButtonWithTitle:` is `-(NSButton *)(NSString *)` and `-window` is
        // `-(NSWindow *)` — both +0, so both are RETAINED into `Obj` rather than
        // adopted, which is the retain `objc2`'s `Retained` returns carried. The
        // alert keeps the default `NSAlertStyleWarning` (the app-icon caution panel).
        // `-runModal` is `-(NSModalResponse)`, an `NSInteger`.
        unsafe {
            appkit::send_v_id(alert.id(), sel!(setMessageText:), title.id());
            appkit::send_v_id(alert.id(), sel!(setInformativeText:), body.id());
            // First button added is the default (Return, with an EMPTY modifier mask —
            // hence the key watch below): the PROCEED action. The second is Cancel
            // (AppKit binds Escape to it).
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
            // Dropped when this function returns — i.e. the moment `runModal` comes
            // back — so the interceptor's lifetime is exactly the alert's.
            //
            // A missing button or panel means there is nothing to click, so the watch
            // is simply not installed and the alert keeps its stock keys. That is the
            // same degradation `watch_alert_keys` already documents for the case where
            // AppKit declines the monitor, and `confirm` still asks its question.
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

    /// Show a simple informational alert (a single OK button) and block until the user
    /// dismisses it — the visible result of App menu ▸ Check for Updates…. `title` is the
    /// primary line, `body` the details (version + "what changed"). Best-effort: off the
    /// main thread it does nothing. `runModal` is the same nested-modal pattern `confirm`
    /// uses, safe to call straight from the winit event handler.
    pub fn notify(title: &str, body: &str) {
        if super::os_ui_refused(title) || MainThread::new().is_none() {
            return;
        }
        // `-runModal` below spins a nested run loop until the user dismisses
        // the alert; park the main-thread watchdog for exactly that long.
        let _park = crate::watchdog::park_modal();
        let (Some(title), Some(body), Some(ok)) = (
            appkit::nsstring(title),
            appkit::nsstring(body),
            appkit::nsstring("OK"),
        ) else {
            return;
        };
        // SAFETY: standard `NSAlert` construction + setters + `runModal`, all on
        // the main thread. `+new` is +1 (alloc+init) and lands in `Obj`;
        // `-setMessageText:`/`-setInformativeText:` are `-(void)(NSString *)`;
        // `-addButtonWithTitle:` is `-(id)` returning a BORROWED button this
        // caller does not keep; `-runModal` is `-(NSModalResponse)`.
        autoreleasepool(|_| unsafe {
            let Some(alert) =
                Obj::from_owned(appkit::send_id(class(c"NSAlert").as_id(), sel!(new)))
            else {
                return;
            };
            appkit::send_v_id(alert.id(), sel!(setMessageText:), title.id());
            appkit::send_v_id(alert.id(), sel!(setInformativeText:), body.id());
            let _ = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), ok.id());
            let _ = appkit::send_isize(alert.id(), sel!(runModal));
        });
    }

    /// AppKit's synchronous `applicationShouldTerminate:` hook. The first request
    /// is vetoed and posted to the typed event loop; duplicates remain vetoed while
    /// the same generation is awaiting confirmation/save proofs. Once `App` marks
    /// the generation complete, a re-entrant terminate is allowed (normal aterm
    /// shutdown uses `ActiveEventLoop::exit` and does not need to re-enter AppKit).
    pub fn defer_quit_for_terminate() -> bool {
        let decision = super::with_native_terminate(NativeTerminateArbiter::request);
        match decision {
            NativeTerminateDecision::AllowExit => true,
            NativeTerminateDecision::DeferExisting => false,
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

    /// Help ▸ aterm Help: open the bundled, offline features guide
    /// (`Contents/Resources/Help.html`, bundled by the ship tool — aterm-release
    /// `bundle.rs`) in the default browser. Falls back to the project page when
    /// running outside the `.app` (e.g. `cargo run`), where no bundled resource exists.
    pub fn open_help_url() {
        if let Some(help) = bundled_resource("Help.html") {
            open_in_workspace(&help, true);
        } else {
            open_in_workspace(super::HELP_URL, false);
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

    /// Open `s` via `NSWorkspace openURL:` — a file path (`is_file`) becomes a
    /// `file://` URL, otherwise it is parsed as an absolute URL. Best-effort; main
    /// thread only.
    ///
    /// Returns what `openURL:` answered: whether the shell ACCEPTED the URL —
    /// which is not the same as "the app did what the URL asked", and for the
    /// Settings anchors is emphatically not the same (see
    /// [`open_privacy_settings`]). Help ignores it; the deep link needs it to
    /// walk its fallback chain. `false` also covers the off-main-thread refusal,
    /// so a caller that reads the answer cannot mistake "not attempted" for
    /// "opened".
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

    /// Open a LOCAL FILE aterm itself names — Settings ▸ Packages' "Open Log", for the
    /// package log atpkg resolved (Phase 4) — through `NSWorkspace openURL:` with a
    /// `file://` URL: macOS opens it in the app that owns the type (Console for a `.log`).
    /// No shell and no Terminal are spawned, and nothing a program printed can reach this
    /// (the link path's allowlist stays closed). Main thread only; `false` when refused.
    pub fn open_file_in_workspace(path: &std::path::Path) -> bool {
        path.to_str().is_some_and(|s| open_in_workspace(s, true))
    }

    /// How long after the first open the same URL is issued again. The anchor
    /// after `?` is not Apple-documented and is not always honoured on the
    /// first open (design Appendix A.2); a second issue about a second later is
    /// the shipped workaround. Long enough that System Settings has finished
    /// launching cold, short enough that it is one gesture rather than two.
    const SECOND_OPEN_DELAY_SECS: f64 = 1.0;

    /// Open System Settings at `pane` — twice, roughly [`SECOND_OPEN_DELAY_SECS`]
    /// apart — and report how far it got.
    ///
    /// # Why a Settings URL may be opened here, and never from a link
    ///
    /// `crate::is_safe_url` is the allowlist a Cmd-clicked hyperlink from
    /// PROGRAM OUTPUT must pass, and it admits only `http`/`https`/`mailto`. Its
    /// rejection test names an `x-apple-systempreferences:` URL on purpose:
    /// output that a program printed must never be able to open a privacy pane,
    /// because a pane a program can raise is a consent surface that program
    /// controls — the same rule that keeps the warm-up and the `tccutil` repair
    /// off the control verbs (design §3.5, §3.7). **Nothing here widens that
    /// allowlist, and it must not be widened to accommodate this path.**
    ///
    /// This URL is a different kind of thing: a `&'static str` compiled into
    /// aterm, reached only from a button on aterm's own Security page — an owner
    /// gesture, at a moment the owner chose, with no untrusted bytes anywhere in
    /// it. It goes to `NSWorkspace` directly rather than through the link path,
    /// so the two never share a gate and the allowlist stays closed.
    ///
    /// # Twice, without blocking
    ///
    /// The second issue is a delayed perform on the main run loop, NOT a sleep:
    /// this function returns to the event loop immediately and the second open
    /// arrives later as an ordinary run-loop callback (see
    /// [`open_in_workspace_after_delay`]).
    ///
    /// # Never a dead end
    ///
    /// Candidates are tried best-first from [`privacy_settings_urls`], whose last
    /// entry carries no anchor at all. If both anchored URLs are refused, the
    /// Privacy & Security page still opens and the answer is
    /// [`SettingsOpen::PaneRoot`], which tells the caller to show
    /// [`privacy_settings_path_words`] — the route in words. Off the main thread
    /// nothing is attempted and the answer is [`SettingsOpen::Refused`], which is
    /// the same instruction to the caller.
    pub fn open_privacy_settings(pane: PrivacyPane) -> SettingsOpen {
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

    /// Issue `s` through `NSWorkspace openURL:` again after `secs`.
    ///
    /// `performSelector:withObject:afterDelay:` schedules on the CALLING thread's
    /// run loop, and the only way in is from the main thread
    /// ([`open_privacy_settings`] has already checked, and this re-checks), so the
    /// second open runs on the main thread while the main thread stays free in the
    /// meantime. No worker, no timer object to own, and nothing to cancel: the
    /// worst case is one extra `openURL:` for a pane the owner asked for a second
    /// earlier.
    ///
    /// Best-effort in one respect worth naming: a delayed perform is scheduled in
    /// the default run-loop mode, so if the main thread is inside a tracking loop
    /// (a drag, an open menu) when the second open comes due, it lands when
    /// tracking ends rather than on the second. That only ever makes the second
    /// issue LATE; it cannot lose it, and the first open has already happened.
    fn open_in_workspace_after_delay(s: &str, secs: f64) {
        if super::os_ui_refused(&format!("opening {s}")) || MainThread::new().is_none() {
            return;
        }
        let Some(ns) = appkit::nsstring(s) else {
            return;
        };
        // SAFETY: standard AppKit. Every `s` reaching here is one of this module's
        // own `&'static str` URL constants, so `URLWithString:` is non-nil.
        // `NSWorkspace` retains both the receiver and the argument until the
        // perform fires, so nothing here has to outlive this frame. The scheduled
        // selector is `openURL:`, whose real return is `BOOL` while the delayed
        // perform invokes it as `id`-returning; the value lands in the same return
        // register on every target aterm builds for and is discarded either way.
        autoreleasepool(|_| unsafe {
            let url = appkit::send_id_id(class(c"NSURL").as_id(), sel!(URLWithString:), ns.id());
            let ws = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            if url.is_null() || ws.is_null() {
                return;
            }
            let perform: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64) = aterm_objc::msg();
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
        use std::sync::atomic::{AtomicUsize, Ordering};

        use aterm_objc::{
            Bool, ClassType, Encode, Id, Obj, autoreleasepool, class, class_name, method_types, sel,
        };

        use super::{MenuTarget, add_separator, new_item, new_menu};
        use crate::appkit;

        /// How many `validateMenuItem:` calls the probe answered, and what it
        /// was asked about. A real `MenuTarget`'s ivar is an
        /// `EventLoopProxy<Wake>`, which needs a live winit `EventLoop`; the
        /// probe is a second expansion of the identical shape whose bodies
        /// record instead of posting.
        static VALIDATED: AtomicUsize = AtomicUsize::new(0);
        static ACTIONED: AtomicUsize = AtomicUsize::new(0);

        aterm_objc::declare_class! {
            struct MenuProbe: NSObject {
                const NAME: &str = "ATermMenuProbe";
                type Ivars = ();

                @sel(menuAction:)
                fn menu_action(&self, sender: Id) {
                    // SAFETY: AppKit passed the live sending NSMenuItem.
                    let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                    ACTIONED.fetch_add(usize::try_from(tag).unwrap_or(0), Ordering::SeqCst);
                }

                /// Enabled iff the item's tag is ODD — an arbitrary rule whose
                /// only job is to make AppKit's OWN reading of the returned
                /// `BOOL` observable in `-isEnabled`.
                @sel(validateMenuItem:)
                fn validate_menu_item(&self, sender: Id) -> Bool {
                    VALIDATED.fetch_add(1, Ordering::SeqCst);
                    // SAFETY: AppKit passed the live NSMenuItem being validated.
                    let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                    Bool::new(tag % 2 != 0)
                }
            }
        }

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

        /// THE PROOF for the ENCODINGS — read back out of the runtime with
        /// `method_getTypeEncoding`, against the verified table.
        ///
        /// `validateMenuItem:` is the D3 shape: `- (BOOL)validateMenuItem:(id)`
        /// registers `"B@:@"` on `aarch64-apple-darwin` and `"c@:@"` on the
        /// `x86_64-apple-darwin` compat slice, because `@encode(BOOL)` differs
        /// between them. The expectation is written FROM `Bool::ENCODING`
        /// rather than hard-coded, which is what makes it a check on both
        /// arches from one source; the literal is asserted alongside on the arch
        /// that can execute, so a `Bool::ENCODING` that went wrong could not
        /// make this test vacuous.
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

        /// THE PROOF for BEHAVIOUR, and FOUNDATION is what reads the `BOOL`.
        ///
        /// `NSMethodSignature` + `NSInvocation` is not a detour, it is the
        /// exact machinery the registered encoding exists for: a method type
        /// string is what AppKit reaches for on every path that does not send
        /// the selector directly, and `NSInvocation` decodes the return value
        /// **by that string**. So this asks Foundation three questions the port
        /// cannot answer for itself —
        ///
        /// * what does `- (BOOL)validateMenuItem:(id)` look like from outside?
        ///   (`-methodReturnType` must be `Bool::ENCODING`, `-methodReturnLength`
        ///   must be one byte, `-numberOfArguments` must be three)
        /// * what does invoking it return for an item AppKit itself tagged?
        /// * and does the answer differ for the two tags?
        ///
        /// — and a wrong encoding fails all three rather than half-passing: a
        /// `"q"` return would report length 8, an `"@"` return would report a
        /// pointer, and `NSInvocation` would copy the wrong number of bytes out.
        ///
        /// REFUTED, and the refutation is why this shape: the obvious test was
        /// `-[NSMenu update]`, AppKit's own NSMenuValidation pass. It reaches
        /// the target through `NSApplication`, and a libtest process has no
        /// `NSApplication` and cannot make one (`+sharedApplication` is
        /// main-thread-only; libtest runs every test on a spawned thread). It
        /// counted **0** `validateMenuItem:` calls, silently — exactly the
        /// "compiles, does nothing, reports success" shape this campaign has
        /// already been caught by once.
        #[test]
        fn foundation_reads_the_declared_bool_through_the_registered_encoding() {
            VALIDATED.store(0, Ordering::SeqCst);
            ACTIONED.store(0, Ordering::SeqCst);
            let probe = MenuProbe::alloc_init(crate::appkit::test_witness(), ()).expect("probe");
            autoreleasepool(|_| {
                // SAFETY: every send below is cast to the exact prototype named
                // beside it. `-methodSignatureForSelector:` is `-(id)(SEL)`;
                // `+invocationWithMethodSignature:` is `-(id)(id)`;
                // `-setSelector:` is `-(void)(SEL)`; `-setArgument:atIndex:` is
                // `-(void)(void *, NSInteger)` and Foundation COPIES the bytes
                // at the pointer, so `&mut arg` need only outlive the call;
                // `-invokeWithTarget:` is `-(void)(id)`; `-getReturnValue:` is
                // `-(void)(void *)` and writes `methodReturnLength` bytes, which
                // is asserted to be `size_of::<Bool>()` before the call.
                unsafe {
                    let menu = new_menu().expect("NSMenu");
                    appkit::send_v_bool(menu.id(), sel!(setAutoenablesItems:), true);
                    let mut items = Vec::new();
                    for tag in 1..=2 {
                        let item =
                            new_item(&format!("row {tag}"), sel!(menuAction:), "").expect("item");
                        appkit::send_v_id(item.id(), sel!(setTarget:), probe.as_id());
                        appkit::send_v_isize(item.id(), sel!(setTag:), tag);
                        appkit::send_v_id(menu.id(), sel!(addItem:), item.id());
                        items.push(item);
                    }
                    add_separator(&menu);

                    // What Foundation reads out of the REGISTERED encoding.
                    let sig_for: unsafe extern "C-unwind" fn(
                        Id,
                        aterm_objc::Sel,
                        aterm_objc::Sel,
                    ) -> Id = aterm_objc::msg();
                    let sig = sig_for(
                        probe.as_id(),
                        sel!(methodSignatureForSelector:),
                        sel!(validateMenuItem:),
                    );
                    assert!(
                        !sig.is_null(),
                        "Foundation could not build a signature for the declared method"
                    );
                    // THE PRODUCTION CLASS, not the probe. A judge planted a
                    // wrong return type on `MenuTarget::validate_menu_item` and
                    // this test PASSED, because every signature below came from
                    // `MenuProbe` — a copy that carries the same shape by hand.
                    // Only the encoding test caught the plant. The probe is
                    // still what gets INVOKED (constructing a real `MenuTarget`
                    // needs an `EventLoopProxy` this test has no event loop to
                    // give), but the signature Foundation is asked to agree with
                    // now comes from the class that ships, via the class-side
                    // `+instanceMethodSignatureForSelector:` — which needs no
                    // instance at all.
                    let cls_sig_for: unsafe extern "C-unwind" fn(
                        aterm_objc::ClassPtr,
                        aterm_objc::Sel,
                        aterm_objc::Sel,
                    ) -> Id = aterm_objc::msg();
                    let target_sig = cls_sig_for(
                        MenuTarget::class(),
                        sel!(instanceMethodSignatureForSelector:),
                        sel!(validateMenuItem:),
                    );
                    assert!(
                        !target_sig.is_null(),
                        "Foundation could not build a signature for MenuTarget's declared method"
                    );
                    let target_ret_type: unsafe extern "C-unwind" fn(
                        Id,
                        aterm_objc::Sel,
                    )
                        -> *const std::ffi::c_char = aterm_objc::msg();
                    let target_ret = std::ffi::CStr::from_ptr(target_ret_type(
                        target_sig,
                        sel!(methodReturnType),
                    ));
                    assert_eq!(
                        target_ret.to_str().expect("ascii"),
                        Bool::ENCODING,
                        "MenuTarget — the class that ships — disagrees with the encoding table"
                    );
                    assert_eq!(
                        appkit::send_usize(target_sig, sel!(methodReturnLength)),
                        size_of::<Bool>(),
                        "MenuTarget's registered return length is not a Bool's"
                    );
                    assert_eq!(
                        appkit::send_usize(target_sig, sel!(numberOfArguments)),
                        3,
                        "MenuTarget's registered argument count moved"
                    );
                    let ret_type: unsafe extern "C-unwind" fn(
                        Id,
                        aterm_objc::Sel,
                    )
                        -> *const std::ffi::c_char = aterm_objc::msg();
                    let ret = std::ffi::CStr::from_ptr(ret_type(sig, sel!(methodReturnType)));
                    assert_eq!(
                        ret.to_str().expect("ascii"),
                        Bool::ENCODING,
                        "NSMethodSignature disagrees with the encoding table"
                    );
                    assert_eq!(
                        appkit::send_usize(sig, sel!(methodReturnLength)),
                        size_of::<Bool>()
                    );
                    assert_eq!(appkit::send_usize(sig, sel!(numberOfArguments)), 3);

                    // …and what it returns, per tag, decoded by that string.
                    for (i, item) in items.iter().enumerate() {
                        let tag = i as isize + 1;
                        let inv = appkit::send_id_id(
                            class(c"NSInvocation").as_id(),
                            sel!(invocationWithMethodSignature:),
                            sig,
                        );
                        assert!(!inv.is_null());
                        let set_sel: unsafe extern "C-unwind" fn(
                            Id,
                            aterm_objc::Sel,
                            aterm_objc::Sel,
                        ) = aterm_objc::msg();
                        set_sel(inv, sel!(setSelector:), sel!(validateMenuItem:));
                        let set_arg: unsafe extern "C-unwind" fn(
                            Id,
                            aterm_objc::Sel,
                            *mut std::ffi::c_void,
                            isize,
                        ) = aterm_objc::msg();
                        let mut arg = item.id();
                        set_arg(
                            inv,
                            sel!(setArgument:atIndex:),
                            std::ptr::from_mut(&mut arg).cast(),
                            2,
                        );
                        appkit::send_v_id(inv, sel!(invokeWithTarget:), probe.as_id());
                        let mut out = Bool::NO;
                        let get_ret: unsafe extern "C-unwind" fn(
                            Id,
                            aterm_objc::Sel,
                            *mut std::ffi::c_void,
                        ) = aterm_objc::msg();
                        get_ret(
                            inv,
                            sel!(getReturnValue:),
                            std::ptr::from_mut(&mut out).cast(),
                        );
                        assert_eq!(
                            out.as_bool(),
                            tag % 2 != 0,
                            "NSInvocation decoded the wrong BOOL for tag {tag}"
                        );
                    }
                    assert_eq!(VALIDATED.load(Ordering::SeqCst), 2);

                    // The separator AppKit made really is one.
                    let sep = appkit::send_id_isize(menu.id(), sel!(itemAtIndex:), 2);
                    assert!(appkit::send_bool(sep, sel!(isSeparatorItem)));

                    // …and the action leg, through the runtime's own dispatch
                    // of the target/action pair AppKit stored.
                    let target_of: unsafe extern "C-unwind" fn(Id, aterm_objc::Sel) -> Id =
                        aterm_objc::msg();
                    assert_eq!(target_of(items[1].id(), sel!(target)), probe.as_id());
                    let perform_sel: unsafe extern "C-unwind" fn(
                        Id,
                        aterm_objc::Sel,
                        aterm_objc::Sel,
                        Id,
                    ) -> Id = aterm_objc::msg();
                    perform_sel(
                        probe.as_id(),
                        sel!(performSelector:withObject:),
                        sel!(menuAction:),
                        items[1].id(),
                    );
                }
            });
            assert_eq!(ACTIONED.load(Ordering::SeqCst), 2);
        }

        /// The menu bar this module builds is the menu bar it built before: the
        /// same titles, in the same order, with the same submenu item titles,
        /// tags, key equivalents and modifier masks — read back out of AppKit.
        ///
        /// This is the regression check the port owes a USER-VISIBLE surface. It
        /// does not install the bar (that needs `NSApp` and the main thread); it
        /// builds the identical structure through the ported constructors and
        /// interrogates it.
        #[test]
        fn the_built_menu_bar_has_the_titles_tags_and_masks_it_had() {
            let probe = MenuProbe::alloc_init(crate::appkit::test_witness(), ()).expect("probe");
            autoreleasepool(|_| {
                // SAFETY: plain AppKit accessors on menus this test built.
                unsafe {
                    // The App menu, whole, as `build_app_menu` composes it.
                    let menu = new_menu().expect("NSMenu");
                    super::add_item(
                        &menu,
                        probe_as_menu_target(&probe),
                        "Settings…",
                        super::MenuAction::ToggleSettings,
                        ",",
                        true,
                    );
                    super::add_item_mods(
                        &menu,
                        probe_as_menu_target(&probe),
                        "Enter Full Screen",
                        super::MenuAction::ToggleFullScreen,
                        "f",
                        super::command_control_mask(),
                    );
                    let settings = appkit::send_id_isize(menu.id(), sel!(itemAtIndex:), 0);
                    assert_eq!(
                        appkit::nsstring_to_rust(appkit::send_id(settings, sel!(title))),
                        "Settings…"
                    );
                    assert_eq!(
                        appkit::send_isize(settings, sel!(tag)),
                        super::MenuAction::ToggleSettings.tag()
                    );
                    assert_eq!(
                        appkit::nsstring_to_rust(appkit::send_id(settings, sel!(keyEquivalent))),
                        ","
                    );
                    assert_eq!(
                        appkit::send_usize(settings, sel!(keyEquivalentModifierMask)),
                        super::command_mask()
                    );
                    let full = appkit::send_id_isize(menu.id(), sel!(itemAtIndex:), 1);
                    assert_eq!(
                        appkit::send_usize(full, sel!(keyEquivalentModifierMask)),
                        super::command_control_mask()
                    );
                    assert_ne!(super::command_control_mask(), super::command_mask());

                    // A submenu attaches under a titled, action-less bar item.
                    let bar = new_menu().expect("NSMenu");
                    let sub = new_menu().expect("NSMenu");
                    let item = super::attach_submenu(&bar, "Version", sub).expect("attached");
                    assert_eq!(
                        appkit::nsstring_to_rust(appkit::send_id(item.id(), sel!(title))),
                        "Version"
                    );
                    // The item was BUILT with `Sel::NULL`, and AppKit then
                    // rewrote its action to its own `submenuAction:` when the
                    // submenu was attached — measured here rather than assumed;
                    // the first version of this assertion expected nil and was
                    // wrong. What matters is that nothing of ours is left on it.
                    let action_of: unsafe extern "C-unwind" fn(
                        Id,
                        aterm_objc::Sel,
                    )
                        -> aterm_objc::Sel = aterm_objc::msg();
                    let action = action_of(item.id(), sel!(action));
                    assert_ne!(action, sel!(menuAction:));
                    assert_eq!(action, sel!(submenuAction:));
                    let attached = appkit::send_id(item.id(), sel!(submenu));
                    assert!(!attached.is_null());
                    assert_eq!(
                        appkit::nsstring_to_rust(appkit::send_id(attached, sel!(title))),
                        "Version"
                    );
                    assert_eq!(appkit::send_isize(bar.id(), sel!(numberOfItems)), 1);
                }
            });
        }

        /// SPEC19 §9: under a FLEET hold the native bar's halt pair greys WITH
        /// the fleet's reason. The palette's rows carried it; the bar's rows
        /// said only their help (`FrontHold` was a bare `AtomicU8`, and
        /// `validateMenuItem:` set only the state). The real `FABRIC_MENU` is
        /// built through the ported constructors with the probe target; the
        /// tip is what `validateMenuItem:` stamps at the moment the menu opens,
        /// so the stamp is called here as AppKit would call it.
        #[test]
        fn a_fleet_hold_greys_the_native_halt_pair_with_its_reason() {
            use super::super::{FABRIC_MENU, FrontHold, MENU_STATICS, MenuAction};
            let _statics = MENU_STATICS.lock().unwrap_or_else(|p| p.into_inner());
            super::super::set_active_tab_is_terminal(true);
            let probe = MenuProbe::alloc_init(crate::appkit::test_witness(), ()).expect("probe");
            autoreleasepool(|_| {
                let menu = super::build_section(probe_as_menu_target(&probe), FABRIC_MENU)
                    .expect("the Fabric menu");
                let halt = [MenuAction::HoldSession, MenuAction::LiftHold];
                // SAFETY: plain accessors on the menu this test built and the
                // live items it holds.
                let items: Vec<(Id, MenuAction)> = unsafe {
                    let n = appkit::send_isize(menu.id(), sel!(numberOfItems));
                    (0..n)
                        .filter_map(|i| {
                            let item = appkit::send_id_isize(menu.id(), sel!(itemAtIndex:), i);
                            let action = MenuAction::from_tag(appkit::send_isize(item, sel!(tag)))?;
                            halt.contains(&action).then_some((item, action))
                        })
                        .collect()
                };
                assert_eq!(items.len(), 2, "both halt rows are on the Fabric menu");

                super::super::set_front_hold(FrontHold::Fleet, "main%20broken");
                for (item, action) in &items {
                    assert!(!super::super::native_menu_action_enabled(*action));
                    super::stamp_native_tip(*item, *action);
                    // SAFETY: `-toolTip` is `-(NSString *)` on a live NSMenuItem.
                    let tip =
                        unsafe { appkit::nsstring_to_rust(appkit::send_id(*item, sel!(toolTip))) };
                    assert!(
                        tip.contains("cannot be lifted here") && tip.contains("main broken"),
                        "{action:?}: the greyed native row says nothing of the fleet hold; its tool tip is {tip:?}"
                    );
                }

                // The hold lifts: the pair's tips return to their help.
                super::super::set_front_hold(FrontHold::None, "");
                for (item, action) in &items {
                    super::stamp_native_tip(*item, *action);
                    // SAFETY: as above.
                    let tip =
                        unsafe { appkit::nsstring_to_rust(appkit::send_id(*item, sel!(toolTip))) };
                    assert_eq!(tip, action.help(), "{action:?}");
                }
            });
        }

        /// `add_item` takes a `&MenuTarget`; the probe is a different declared
        /// class of the same shape, so this reinterprets it for the two calls
        /// above. Sound for exactly the reason the trampolines are: both types
        /// are zero-sized opaque markers AT the instance address, and
        /// `add_item` only ever asks for `as_id()`.
        fn probe_as_menu_target(probe: &aterm_objc::Retained<MenuProbe>) -> &MenuTarget {
            // SAFETY: `MenuTarget` is a zero-sized marker that borrows no bytes
            // of its own; the reference is used only to reach `as_id()`, which
            // re-derives the object pointer from the reference's ADDRESS. That
            // address is the probe's live instance.
            unsafe { &*std::ptr::from_ref::<MenuProbe>(probe).cast::<MenuTarget>() }
        }

        /// The generated `-dealloc` drops the Rust ivars.
        #[test]
        fn dropping_a_declared_instance_drops_its_ivars() {
            static DROPS: AtomicUsize = AtomicUsize::new(0);
            struct Spy;
            impl Drop for Spy {
                fn drop(&mut self) {
                    DROPS.fetch_add(1, Ordering::SeqCst);
                }
            }
            aterm_objc::declare_class! {
                struct MenuDropProbe: NSObject {
                    const NAME: &str = "ATermMenuDropProbe";
                    type Ivars = Spy;

                    @sel(ping)
                    fn ping(&self) {}
                }
            }
            DROPS.store(0, Ordering::SeqCst);
            let t = MenuDropProbe::alloc_init(crate::appkit::test_witness(), Spy).expect("probe");
            assert_eq!(DROPS.load(Ordering::SeqCst), 0);
            drop(t);
            assert_eq!(DROPS.load(Ordering::SeqCst), 1);
        }

        /// Keeps `Obj` used even if a future edit drops the only other use.
        #[allow(dead_code)]
        fn _obj_is_the_owning_type(o: Obj) -> Id {
            o.id()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MENU_MODEL, MenuAction, MenuEntry, menu_chrome_lines, native_menu_action_enabled,
        set_active_tab_is_terminal, staged_apply_label, version_menu_bar_title,
    };

    /// Both document runtimes already worked on Windows — the socket can drive
    /// them today — so the rows greying out there was the picker's absence
    /// leaking into a capability claim. With a real `IFileOpenDialog` linked, the
    /// two platforms that HAVE a picker must both report one.
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn a_platform_with_a_picker_says_so() {
        assert!(
            super::local_file_picker_available(),
            "macOS has NSOpenPanel and Windows has IFileOpenDialog"
        );
    }

    /// REGRESSION: the menu bar badged "v0.14.0 ⬆️" over a row reading "Update to
    /// v0.14.0 — apply now" (today "Install aterm v0.14.0 now"), which reads as an
    /// offer to update a build to itself.
    ///
    /// Nothing was wrong with the update. The updater orders by BUILD NUMBER (the
    /// display version is documented as never affecting an update comparison), so a
    /// strictly-newer staged build may legally share the running build's version
    /// string — the ledger ships such pairs. The badge printed the running version
    /// and the row printed the staged version, and BOTH dropped the build, which was
    /// the only field that told them apart.
    ///
    /// The invariant: the apply row must never be spellable as the badge.
    #[test]
    fn the_apply_row_can_never_read_as_the_version_already_running() {
        let running = crate::build_info::version_display();
        let badge = version_menu_bar_title(true);

        // The colliding case: staged build carries the SAME version string.
        let same = staged_apply_label("\u{2B06}\u{FE0F}", 1_785_910_394, running, None);
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
        let newer = staged_apply_label("\u{2B06}\u{FE0F}", 1_785_910_394, "9.9.9", None);
        assert!(
            newer.contains("v9.9.9") && !newer.contains("build"),
            "a differing version is still named directly: {newer}"
        );
    }

    /// THE ALWAYS-VISIBLE OFFER MUST CARRY THE APPLY LANE'S VERDICT ON ITSELF.
    ///
    /// This row is the affordance the owner actually looked at for hours on
    /// 2026-08-21: "⬆️ Update to v0.56.0 — restart now" (the tail of the day), above
    /// a control socket reporting `failing_applies=2` and the reason both attempts
    /// died. A row that tells you to apply, over a build that has already refused to
    /// start twice, is not merely incomplete — it is instructing you to repeat
    /// something the program privately knows has not worked. And the clean row says
    /// what pressing it does in the update row's own words — install, now — never a
    /// restart.
    #[test]
    fn the_apply_row_admits_when_applying_has_already_been_tried() {
        use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};

        let clean = staged_apply_label("\u{2191}", 1_787_699_398, "9.9.9", None);
        assert_eq!(clean, "\u{2191} Install aterm v9.9.9 now");

        let trouble = ApplyTrouble::new(
            2,
            "overlap handoff failed safely: handoff proof ended ChildDied",
            ApplyRetry::Scheduled,
        )
        .expect("two failed applies");
        let troubled = staged_apply_label("\u{2191}", 1_787_699_398, "9.9.9", Some(&trouble));
        assert!(
            troubled.contains("v9.9.9"),
            "the offer still names what it is offering: {troubled}"
        );
        assert!(
            troubled.contains("twice"),
            "…and how many attempts have already died: {troubled}"
        );
        assert!(
            troubled.contains("didn\u{2019}t start"),
            "…and why, in words: {troubled}"
        );
        assert!(
            !troubled.contains("ChildDied"),
            "never the proof-outcome enum name: {troubled}"
        );
        assert!(
            !troubled.contains(" now") && troubled.ends_with("will try again"),
            "a scheduled retry must not demand an action that is already on its way: \
             {troubled}"
        );

        // The latched lane keeps the call to action, because there it is true.
        let latched = ApplyTrouble::new(2, "handoff proof ended ChildDied", ApplyRetry::ManualOnly)
            .expect("two failed applies");
        let latched = staged_apply_label("\u{2191}", 1_787_699_398, "9.9.9", Some(&latched));
        assert!(
            latched.ends_with("try again now"),
            "a lane that has stopped names the action that resumes it: {latched}"
        );
        // …and none of them asks for a restart: the apply is in place.
        assert!(
            !clean.contains("restart")
                && !troubled.contains("restart")
                && !latched.contains("restart"),
            "{clean} / {troubled} / {latched}"
        );
    }

    /// The canonical set of every menu command — shared by the tag round-trip test and
    /// the `MENU_MODEL` completeness test (so adding a command means updating one list).
    const ALL_ACTIONS: &[MenuAction] = &[
        MenuAction::About,
        MenuAction::SoftwareUpdate,
        MenuAction::Preferences,
        MenuAction::Quit,
        MenuAction::NewWindow,
        MenuAction::NewTab,
        MenuAction::OpenMarkdown,
        MenuAction::OpenEditor,
        MenuAction::ReopenClosedTab,
        MenuAction::ReopenClosedView,
        MenuAction::MoveTabToNewWindow,
        MenuAction::MoveTabToNextWindow,
        MenuAction::ViewSessionInNewWindow,
        MenuAction::NewControlledWindow,
        MenuAction::NewControlledTab,
        MenuAction::NewControllerWindow,
        MenuAction::NewControllerTab,
        MenuAction::CloseTab,
        MenuAction::Copy,
        MenuAction::Paste,
        MenuAction::SelectAll,
        MenuAction::Find,
        MenuAction::FindNext,
        MenuAction::FindPrev,
        MenuAction::ToggleFullScreen,
        MenuAction::FontIncrease,
        MenuAction::FontDecrease,
        MenuAction::FontActualSize,
        MenuAction::SplitVertical,
        MenuAction::SplitHorizontal,
        MenuAction::Minimize,
        MenuAction::Zoom,
        MenuAction::NextTab,
        MenuAction::PrevTab,
        MenuAction::RenameSession,
        MenuAction::ToggleSeriousMode,
        MenuAction::ToggleMatrixRain,
        MenuAction::FavouriteKitty,
        MenuAction::NextKitty,
        MenuAction::ToggleSettings,
        MenuAction::Packages,
        MenuAction::Messages,
        MenuAction::OpenPalette,
        MenuAction::Help,
        MenuAction::Version,
        MenuAction::ApplyUpdate,
        // Round 19: the Fabric menu (the four connection ids became BAR rows
        // there), Set Role…, and the two presence checkables.
        MenuAction::Fleet,
        MenuAction::Inbox,
        MenuAction::LedgerForSession,
        MenuAction::HoldSession,
        MenuAction::LiftHold,
        MenuAction::ConnectToSession,
        MenuAction::ConfigureConnection,
        MenuAction::DisconnectSession,
        MenuAction::ShowConnectionMap,
        MenuAction::FabricStatus,
        MenuAction::FabricOn,
        MenuAction::FabricOff,
        MenuAction::SetRole,
        MenuAction::TogglePresenceBand,
        MenuAction::TogglePresenceRim,
    ];

    /// Every `invoke` name that resolved BEFORE round 19's menu rework, verbatim
    /// — the names an agent's script may already carry. SPEC19 §9: "add
    /// aliases, never break a name an agent may already call". Pinned by
    /// `every_pre_round_19_invoke_name_still_resolves`.
    const PRE_ROUND_19_INVOKE_NAMES: &[&str] = &[
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
        "FavouriteSessionKitty",
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
    ];

    /// The NON-BAR action vocabulary: the tab-strip CONTEXT menu's own copy
    /// rows (session-metadata stage 2, `session_chrome::compose_tab_menu`) —
    /// deliberately NOT in the menu bar, hence not in [`ALL_ACTIONS`]/
    /// `MENU_MODEL`. (`CloseTab`, `RenameSession` and the connection ids also
    /// appear in the context menu, but they are BAR actions first and live in
    /// the list above; the connection ids moved into the bar's Fabric menu in
    /// round 19.) Kept as a named twin list so the round-trip/uniqueness
    /// proofs cover the whole enum: `ALL_ACTIONS ∪ TAB_CONTEXT_ACTIONS`.
    const TAB_CONTEXT_ACTIONS: &[MenuAction] = &[MenuAction::CopySessionId, MenuAction::CopyCwd];

    /// Every item of the model, submenus flattened — the one walk the
    /// completeness and key-equivalent proofs share.
    fn model_items() -> Vec<(&'static str, MenuAction, &'static str, super::MenuMods)> {
        MENU_MODEL
            .iter()
            .flat_map(|s| s.entries.iter())
            .flat_map(MenuEntry::items)
            .collect()
    }

    /// Every action's tag round-trips through `from_tag`, and the tags are
    /// distinct ACROSS the bar and tab-context vocabularies (so the integer
    /// carried in an NSMenuItem — bar item or context-menu item — identifies
    /// exactly one command; no two items share a dispatch).
    #[test]
    fn tags_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for a in ALL_ACTIONS
            .iter()
            .chain(TAB_CONTEXT_ACTIONS.iter())
            .copied()
        {
            assert!(a.tag() >= 1, "tag 0 is reserved for untagged items");
            assert!(seen.insert(a.tag()), "duplicate tag {} for {a:?}", a.tag());
            assert_eq!(
                MenuAction::from_tag(a.tag()),
                Some(a),
                "round-trip failed for {a:?}"
            );
        }
    }

    /// The tab-context actions are a deliberate NON-bar vocabulary: their invoke
    /// names round-trip (so `invoke CopySessionId` is fenceable + dispatchable),
    /// each classifies at its honest boundary (the copies move text onto the
    /// pasteboard), and they must NOT leak into `MENU_MODEL` (the bar mirror
    /// stays exactly the bar). The connection ids, once in this list, are bar
    /// rows of the Fabric menu since round 19 and keep their OwnerOnly class
    /// there (`the_fabric_menu_is_owner_only_and_its_session_rows_need_a_session`).
    #[test]
    fn tab_context_actions_round_trip_and_stay_out_of_the_bar() {
        for a in TAB_CONTEXT_ACTIONS.iter().copied() {
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

    /// ROUND 19 (SPEC19 §9): every pre-rework `invoke` name still resolves —
    /// to the same action it always named — and the three aliases the rework
    /// added resolve to the rows whose bar identity moved. A renamed variant
    /// would fail the first loop; a dropped alias the second.
    #[test]
    fn every_pre_round_19_invoke_name_still_resolves() {
        for name in PRE_ROUND_19_INVOKE_NAMES {
            let action = MenuAction::from_invoke_name(name)
                .unwrap_or_else(|| panic!("{name} resolved before round 19 and must still"));
            let canonical = super::canonical_invoke_name(name);
            assert_eq!(
                format!("{action:?}"),
                canonical,
                "{name} must name the action it always named"
            );
        }
        assert_eq!(
            MenuAction::from_invoke_name("OpenLedger"),
            Some(MenuAction::LedgerForSession)
        );
        assert_eq!(
            MenuAction::from_invoke_name("ShowFleet"),
            Some(MenuAction::Fleet)
        );
        assert_eq!(
            MenuAction::from_invoke_name("ShowInbox"),
            Some(MenuAction::Inbox)
        );
    }

    /// THE FABRIC MENU (SPEC19 §9): its rows, in order, are the tree the owner
    /// asked for — Fleet…, Inbox…, the ledger with its accelerator, the halt
    /// pair, the four connection rows unchanged, then the three `aterm fabric`
    /// commands — and it sits where an app's own menu goes, between View and
    /// Window. Every row is Owner-only on the `invoke` fence, no exception;
    /// the session rows need a focused session, the instance rows never grey.
    #[test]
    fn the_fabric_menu_is_owner_only_and_its_session_rows_need_a_session() {
        let titles: Vec<&str> = MENU_MODEL.iter().map(|s| s.title).collect();
        assert_eq!(
            titles,
            [
                "aterm", "File", "Edit", "View", "Fabric", "Window", "Help", "Version"
            ]
        );
        let fabric = MENU_MODEL
            .iter()
            .find(|s| s.title == "Fabric")
            .expect("a Fabric menu");
        let rows: Vec<(&str, MenuAction)> = fabric
            .entries
            .iter()
            .flat_map(MenuEntry::items)
            .map(|(label, action, _, _)| (label, action))
            .collect();
        assert_eq!(
            rows,
            [
                ("Fleet…", MenuAction::Fleet),
                ("Inbox…", MenuAction::Inbox),
                ("Ledger for This Session", MenuAction::LedgerForSession),
                ("Hold This Session", MenuAction::HoldSession),
                ("Lift Hold (This Session)", MenuAction::LiftHold),
                ("Connect to Session…", MenuAction::ConnectToSession),
                ("Configure Connection…", MenuAction::ConfigureConnection),
                ("Disconnect Session…", MenuAction::DisconnectSession),
                ("Show Connection Map", MenuAction::ShowConnectionMap),
                ("Fabric Status…", MenuAction::FabricStatus),
                ("Turn Fabric On…", MenuAction::FabricOn),
                ("Turn Fabric Off…", MenuAction::FabricOff),
            ]
        );
        // The ledger row SHOWS the ledger key's chord.
        let ledger = fabric
            .entries
            .iter()
            .flat_map(MenuEntry::items)
            .find(|(_, a, _, _)| *a == MenuAction::LedgerForSession)
            .unwrap();
        assert_eq!((ledger.2, ledger.3), ("l", super::MenuMods::CommandShift));
        for (_, action) in &rows {
            assert_eq!(
                action.invoke_authority(),
                super::InvokeAuthority::OwnerOnly,
                "{action:?}: the fabric is the owner's"
            );
        }
        for a in [
            MenuAction::Inbox,
            MenuAction::LedgerForSession,
            MenuAction::HoldSession,
            MenuAction::LiftHold,
            MenuAction::ConnectToSession,
            MenuAction::ConfigureConnection,
            MenuAction::DisconnectSession,
        ] {
            assert!(super::requires_terminal_tab(a), "{a:?} is the session's");
        }
        for a in [
            MenuAction::Fleet,
            MenuAction::ShowConnectionMap,
            MenuAction::FabricStatus,
            MenuAction::FabricOn,
            MenuAction::FabricOff,
        ] {
            assert!(!super::requires_terminal_tab(a), "{a:?} is instance-wide");
        }
    }

    /// THE HALT PAIR reads the live hold projection (the Packages doctrine):
    /// nothing standing ⇒ Hold live, Lift not; a LOCAL hold ⇒ Lift live, Hold
    /// not; a FLEET hold ⇒ neither (the bridge's to lift), and the greyed row's
    /// words carry the fleet's reason.
    #[test]
    fn hold_and_lift_follow_the_front_hold_projection() {
        let _statics = super::MENU_STATICS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        set_active_tab_is_terminal(true);
        super::set_front_hold(super::FrontHold::None, "");
        assert!(native_menu_action_enabled(MenuAction::HoldSession));
        assert!(!native_menu_action_enabled(MenuAction::LiftHold));
        assert_eq!(
            super::native_item_tip(MenuAction::HoldSession),
            MenuAction::HoldSession.help()
        );
        super::set_front_hold(super::FrontHold::Local, "pause");
        assert!(!native_menu_action_enabled(MenuAction::HoldSession));
        assert!(native_menu_action_enabled(MenuAction::LiftHold));
        assert_eq!(super::front_hold_reason(), "pause");
        assert_eq!(
            super::native_item_tip(MenuAction::LiftHold),
            MenuAction::LiftHold.help(),
            "a local hold's rows say their help: the pair is not greyed"
        );
        super::set_front_hold(super::FrontHold::Fleet, "main%20broken");
        assert!(!native_menu_action_enabled(MenuAction::HoldSession));
        assert!(!native_menu_action_enabled(MenuAction::LiftHold));
        assert_eq!(
            super::hold_row_reason("main%20broken"),
            "fleet hold: main broken, cannot be lifted here"
        );
        // The decode is not the step that lets a bridge-supplied byte
        // through: a bidi override, a newline and an escape in the token are
        // stripped before the words reach the native tool tip, the palette
        // row or the a11y description (round 19's second review).
        let s = super::hold_row_reason("main%e2%80%aebroken%0a%1b[31m");
        assert!(!s.contains('\u{202e}'), "bidi override leaks: {s:?}");
        assert!(!s.contains('\n'), "newline leaks: {s:?}");
        assert!(!s.contains('\u{1b}'), "escape leaks: {s:?}");
        assert_eq!(s, "fleet hold: mainbroken [31m, cannot be lifted here");
        for a in [MenuAction::HoldSession, MenuAction::LiftHold] {
            assert_eq!(
                super::native_item_tip(a),
                "fleet hold: main broken, cannot be lifted here",
                "{a:?}: the greyed row says why"
            );
        }
        super::set_front_hold(super::FrontHold::None, "");
        assert_eq!(super::front_hold_reason(), "");
        // No session at all: the pair greys with every other session row.
        set_active_tab_is_terminal(false);
        assert!(!native_menu_action_enabled(MenuAction::HoldSession));
        assert!(!native_menu_action_enabled(MenuAction::Inbox));
        assert!(native_menu_action_enabled(MenuAction::Fleet));
        set_active_tab_is_terminal(true);
    }

    /// The two View ▸ Presence rows are CHECKABLE and read their checkmark
    /// from the published live state; every other row is a plain command.
    #[test]
    fn the_presence_rows_are_checkable_from_the_live_state() {
        let _statics = super::MENU_STATICS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        super::set_presence_toggles(true, false);
        assert_eq!(
            super::native_menu_checked(MenuAction::TogglePresenceBand),
            Some(true)
        );
        assert_eq!(
            super::native_menu_checked(MenuAction::TogglePresenceRim),
            Some(false)
        );
        assert_eq!(super::native_menu_checked(MenuAction::Fleet), None);
        super::set_presence_toggles(true, true);
        assert_eq!(
            MenuAction::TogglePresenceBand.invoke_authority(),
            super::InvokeAuthority::ConfigWrite,
            "the toggles write aterm.toml"
        );
        let view = MENU_MODEL.iter().find(|s| s.title == "View").unwrap();
        let labels: Vec<&str> = view
            .entries
            .iter()
            .flat_map(MenuEntry::items)
            .map(|(l, _, _, _)| l)
            .collect();
        assert!(labels.contains(&"Presence Band") && labels.contains(&"Presence Rim"));
    }

    /// Every action carries a HELP sentence (the tool tip / accessibility
    /// description SPEC19 §9 asks for): non-empty, one line, and — the band's
    /// own law — never a command, a body or a title a session wrote.
    #[test]
    fn every_action_has_a_one_line_help_sentence() {
        for a in ALL_ACTIONS.iter().chain(TAB_CONTEXT_ACTIONS).copied() {
            let help = a.help();
            assert!(!help.trim().is_empty(), "{a:?} has no help");
            assert!(!help.contains('\n'), "{a:?}: one line");
            assert!(help.ends_with('.'), "{a:?}: a sentence");
        }
        assert!(
            MenuAction::Fleet.help().contains("Connection Map"),
            "Fleet… says it opens the map until round 20"
        );
    }

    /// FILE ▸ DRIVING carries the four presets UNCHANGED (same labels, same
    /// actions, no key equivalents), and the identity rows are absent — not
    /// disabled — in this slice (the round-18 follow-up adds them).
    #[test]
    fn file_driving_holds_the_four_presets_unchanged_and_identity_is_omitted() {
        let file = MENU_MODEL.iter().find(|s| s.title == "File").unwrap();
        let driving = file
            .entries
            .iter()
            .find_map(|e| match e {
                MenuEntry::Submenu { label, entries } if *label == "Driving" => Some(*entries),
                _ => None,
            })
            .expect("File ▸ Driving");
        let rows: Vec<(&str, MenuAction, &str)> = driving
            .iter()
            .flat_map(MenuEntry::items)
            .map(|(l, a, k, _)| (l, a, k))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "New Controlled Session in New Window",
                    MenuAction::NewControlledWindow,
                    ""
                ),
                (
                    "New Controlled Session as Tab",
                    MenuAction::NewControlledTab,
                    ""
                ),
                (
                    "New Controller Session in New Window",
                    MenuAction::NewControllerWindow,
                    ""
                ),
                (
                    "New Controller Session as Tab",
                    MenuAction::NewControllerTab,
                    ""
                ),
            ]
        );
        let labels: Vec<&str> = model_items().iter().map(|(l, _, _, _)| *l).collect();
        assert!(
            !labels.iter().any(|l| l.contains("Identity")),
            "identity rows are omitted in this slice, not disabled: {labels:?}"
        );
    }

    /// The connected-spawn presets MINT session-connection authority over the
    /// focused session, so their `invoke` fence is OwnerOnly — the MenuAction
    /// twin of the `spawn connected=` escalation arm (design §5.3/§6). They
    /// are also terminal-only: the origin is the FOCUSED session, absent under
    /// a native whole tab.
    #[test]
    fn connected_spawn_presets_are_owner_only_and_terminal_only() {
        for a in [
            MenuAction::NewControlledWindow,
            MenuAction::NewControlledTab,
            MenuAction::NewControllerWindow,
            MenuAction::NewControllerTab,
        ] {
            assert_eq!(
                a.invoke_authority(),
                super::InvokeAuthority::OwnerOnly,
                "{a:?} mints standing authority — only the god token may invoke it"
            );
            assert!(
                super::requires_terminal_tab(a),
                "{a:?} needs a focused session"
            );
        }
    }

    /// Every action's `{:?}` Debug token round-trips through `from_invoke_name` (the
    /// name→variant map the control-layer `invoke` fence reads), and every action has a
    /// compiler-classified `invoke_authority`. Iterating `ALL_ACTIONS` (pinned to be the
    /// whole enum by `menu_model_covers_every_action_exactly_once`) ties the string map
    /// to the real variant set, so a new action cannot slip past the fence unclassified.
    #[test]
    fn invoke_name_round_trips() {
        for a in ALL_ACTIONS.iter().copied() {
            let name = format!("{a:?}");
            assert_eq!(
                MenuAction::from_invoke_name(&name),
                Some(a),
                "from_invoke_name must recover {a:?} from its Debug token {name:?}"
            );
            // Exercises the exhaustive classifier (a decision exists for every variant).
            let _ = a.invoke_authority();
        }
        assert_eq!(MenuAction::from_invoke_name("NopeNotAnAction"), None);
        assert_eq!(MenuAction::from_invoke_name(""), None);
    }

    /// The default `NSMenuItem` tag (0) and any unknown tag decode to `None`, so
    /// an untagged item never dispatches a real command.
    #[test]
    fn unknown_tag_is_none() {
        assert_eq!(MenuAction::from_tag(0), None);
        assert_eq!(MenuAction::from_tag(-1), None);
        assert_eq!(MenuAction::from_tag(9999), None);
    }

    #[test]
    fn staged_update_menu_action_is_selectable_on_every_active_tab_kind() {
        let _statics = super::MENU_STATICS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let model = aterm_spec::derive::native_update_menu_activation_model();
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

        // Tier-1 negative control: the modeled old predicate is not merely
        // unexercised. With a native tab frontmost it disables the row, violates
        // the obligation, and cannot reach decode/dispatch.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut disabled = buggy.init_state();
        assert!(buggy.fire("StageUpdate", &mut disabled));
        assert!(buggy.fire("RefreshStagedVersionMenu", &mut disabled));
        assert_eq!(disabled["terminal_tab"], 0);
        assert_eq!(disabled["row_enabled"], 0);
        assert!(buggy.successors("DecodeApplyTag", &disabled).is_empty());
    }

    /// MENU_MODEL IS the macOS menu (the builder reads it item-for-item since round
    /// 19): every command appears in it EXACTLY once, submenus included, and no
    /// extra/unknown action. This is what keeps the cross-platform `chrome`
    /// serialisation in lockstep with the native menu (and fails CI if a command is
    /// added to the enum and not the model).
    #[test]
    fn menu_model_covers_every_action_exactly_once() {
        let mut model: Vec<MenuAction> = model_items()
            .into_iter()
            .map(|(_, action, _, _)| action)
            .collect();
        let mut expected: Vec<MenuAction> = ALL_ACTIONS.to_vec();
        let sort_key = |a: &MenuAction| a.tag();
        model.sort_by_key(sort_key);
        expected.sort_by_key(sort_key);
        assert_eq!(
            model, expected,
            "MENU_MODEL must list every MenuAction exactly once (and no extras)"
        );
    }

    /// THE MENU TREE GOLDEN — the `chrome` serialiser's lines, whole, as round 19
    /// (SPEC19 §9, "the menu reworked to align") left them. One `menu "<title>": …`
    /// line per section in the standard Mac order plus the app's own Fabric menu
    /// between View and Window; a submenu prints as `<label> ▸` in its parent's line
    /// and then as its own `menu "<parent> ▸ <label>": …` line right after.
    ///
    /// WHY IT CHANGED (the reason the golden records): the File menu used to carry
    /// the pre-fabric driving model flat — "New Controlled Session in New Window /
    /// as Tab", "New Controller Session in New Window / as Tab" — and the four
    /// connection rows lived in a palette-only "Connections" section; nothing in
    /// the bar named the fabric, the fleet, the ledger, an inbox, a session's role
    /// or the presence surfaces. Now the four presets sit under File ▸ Driving
    /// unchanged, a Fabric menu carries the fleet, this session's inbox and ledger,
    /// the halt pair, the connection rows and the three `aterm fabric` commands,
    /// Window gains Set Role…, and View gains the two presence checkables. The
    /// identity rows (round 18) are omitted in this slice, not disabled.
    #[test]
    fn chrome_lines_render_titled_sections() {
        let lines = menu_chrome_lines();
        let expected = [
            "menu \"aterm\": About aterm, Check for Updates…, Settings…, Packages…, \
             Messages…, Open aterm.toml, Quit aterm",
            "menu \"File\": New Window, New Terminal Tab, Driving ▸, Open Markdown…, \
             Open File in Editor…, Reopen Closed Tab, Reopen Closed View, \
             Move Tab to New Window, Move Tab to Next Window, Open Session in New Window, \
             Close Tab",
            "menu \"File ▸ Driving\": New Controlled Session in New Window, \
             New Controlled Session as Tab, New Controller Session in New Window, \
             New Controller Session as Tab",
            "menu \"Edit\": Copy, Paste, Select All, Find…, Find Next, Find Previous",
            "menu \"View\": Increase Font Size, Decrease Font Size, Actual Size, Split Right, \
             Split Down, Enter Full Screen, Presence Band, Presence Rim, Serious Mode, \
             Matrix Rain, Favourite This Kitty, Next Kitty, Command Palette…",
            "menu \"Fabric\": Fleet…, Inbox…, Ledger for This Session, Hold This Session, \
             Lift Hold (This Session), Connect to Session…, Configure Connection…, \
             Disconnect Session…, Show Connection Map, Fabric Status…, Turn Fabric On…, \
             Turn Fabric Off…",
            "menu \"Window\": Minimize, Zoom, Show Next Tab, Show Previous Tab, \
             Rename Session…, Set Role…",
            "menu \"Help\": aterm Help",
            "menu \"Version\": ↑ Install update now, About aterm — build & version…",
        ];
        assert_eq!(
            lines, expected,
            "the menu tree golden (see the doc for why it moved)"
        );
        // No separator artifacts (a stray ", ," from an unfiltered Separator).
        assert!(
            !lines.iter().any(|l| l.contains(", ,")),
            "separators must be filtered"
        );
    }

    /// The anti-shadowing key-equivalents the comments call load-bearing are transcribed
    /// correctly (a wrong chord here silently shadows a primary keybinding on macOS).
    #[test]
    fn critical_key_equivalents_are_correct() {
        let item = |action: MenuAction| {
            model_items()
                .into_iter()
                .find_map(|(_, a, key, mods)| (a == action).then_some((key, mods)))
                .unwrap_or_else(|| panic!("{action:?} not in MENU_MODEL"))
        };
        // ⇧⌘O (must NOT be "d", which would shadow ⇧⌘D split — see the menu.rs comment).
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
        // ⌘, belongs to the Settings tab (the ONE settings surface); the
        // aterm.toml escape hatch must NOT carry a chord that would shadow it.
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
        // the Fabric row; no other row may claim it.
        assert_eq!(
            item(MenuAction::LedgerForSession),
            ("l", super::MenuMods::CommandShift)
        );
        let on_l: Vec<MenuAction> = model_items()
            .into_iter()
            .filter(|(_, _, k, m)| *k == "l" && *m == super::MenuMods::CommandShift)
            .map(|(_, a, _, _)| a)
            .collect();
        assert_eq!(on_l, [MenuAction::LedgerForSession]);
    }

    #[test]
    fn terminal_only_actions_are_identified_exhaustively() {
        for action in ALL_ACTIONS.iter().copied() {
            let expected = matches!(
                action,
                MenuAction::SplitVertical
                    | MenuAction::SplitHorizontal
                    | MenuAction::ViewSessionInNewWindow
                    // Per-session rain acts on the front SESSION — no session
                    // under a native whole tab, so the item greys out there.
                    | MenuAction::ToggleMatrixRain
                    // Same reason: the pin is SESSION metadata.
                    | MenuAction::RenameSession
                    // The connected-spawn presets need a FOCUSED session origin.
                    | MenuAction::NewControlledWindow
                    | MenuAction::NewControlledTab
                    | MenuAction::NewControllerWindow
                    | MenuAction::NewControllerTab
                    // The connection ids act FROM the focused session.
                    | MenuAction::ConnectToSession
                    | MenuAction::ConfigureConnection
                    | MenuAction::DisconnectSession
                    // The Fabric menu's session rows and the role editor.
                    | MenuAction::Inbox
                    | MenuAction::LedgerForSession
                    | MenuAction::HoldSession
                    | MenuAction::LiftHold
                    | MenuAction::SetRole
            );
            assert_eq!(
                super::requires_terminal_tab(action),
                expected,
                "terminal-context classification drifted for {action:?}"
            );
        }
    }

    /// The Version menu's bar title: plain `v<version>` at rest; a TRAILING ⬆️ (the
    /// user's "emoji uparrow under version") while `attention` holds — the attention
    /// draw pointing at the version-number menu. The base must be a prefix of the
    /// attention form (retitling never rewrites the version).
    #[test]
    fn version_menu_bar_title_carries_the_arrow_only_under_attention() {
        let plain = super::version_menu_bar_title(false);
        let arrowed = super::version_menu_bar_title(true);
        // A test binary is a DEV build, so the base leads with the dev
        // signature's wrench rather than the bare `v` a release shows.
        assert!(plain.contains('v'), "{plain}");
        assert!(!plain.contains('\u{2B06}'), "no arrow at rest: {plain}");
        assert!(
            arrowed.starts_with(&plain),
            "retitle only appends: {arrowed}"
        );
        assert!(
            arrowed.ends_with(" \u{2B06}\u{FE0F}"),
            "TRAILING emoji arrow (\"v0.25 ⬆️\" shape): {arrowed}"
        );
    }

    /// THE DEV SIGNATURE (owner, 2026-08-16: a marker in the menu bar "so
    /// that I know what is a development build … using the 3rd developer
    /// number and the hash"): a binary the release cutter did not produce —
    /// this test binary — must wear the dev counter in the third slot, the
    /// short commit hash, and the DEV marker. A release build
    /// (`ATERM_APP_RELEASE_VERSION` present) keeps its clean `v<version>`;
    /// that arm is proven by the discriminator being exactly the release
    /// env's presence, which the cutter's identity self-check already pins.
    #[test]
    fn a_dev_build_wears_its_signature_in_the_bar_title() {
        // A test binary is never a release build — and as a const block this
        // fails the COMPILE of any test build that sets the release env,
        // which is the actual invariant.
        const {
            assert!(
                !crate::build_info::IS_RELEASE_BUILD,
                "a test binary is never a release build"
            )
        };
        let t = super::version_menu_bar_title(false);
        assert!(t.starts_with('\u{1F6E0}'), "the wrench leads: {t}");
        assert!(t.ends_with(" \u{00B7} DEV"), "the marker trails: {t}");
        assert!(t.contains("+g"), "the hash is pinned: {t}");
        assert!(
            t.contains(&format!(".{}+", crate::build_info::DEV_COMMITS)),
            "the third slot is the dev counter, not a release patch: {t}"
        );
    }

    /// The PERSISTENT menu-bar arrow tracks a STAGED update ONLY — never the post-update
    /// `realized` celebration. This is the fix for "the Update icon in the menu bar is
    /// NOT resolved": before, a freshly-applied update lit the SAME bar arrow for the
    /// full realized TTL (10 min), indistinguishable from the pre-update "waiting" arrow,
    /// so an update that DID land still looked unresolved. Now an apply re-execs into the
    /// staged build (`staged` → `None`) and the bar arrow clears immediately; the "just
    /// updated" celebration lives only in self-dismissing surfaces.
    #[test]
    fn menu_bar_arrow_tracks_staged_not_realized() {
        // Action-needed staged update -> arrow.
        assert!(
            super::bar_title_attention(true, false),
            "staged shows the arrow"
        );
        // Fresh post-update celebration is NOT a persistent bar badge.
        assert!(
            !super::bar_title_attention(false, true),
            "realized alone must not light the persistent bar arrow"
        );
        assert!(
            !super::bar_title_attention(false, false),
            "quiet steady state: no arrow"
        );
        // If a newer build stages right after an update, staged still wins.
        assert!(
            super::bar_title_attention(true, true),
            "staged wins over realized"
        );
        // End to end: realized-only renders a plain title, no ⬆️ glyph.
        let realized_only = super::version_menu_bar_title(super::bar_title_attention(false, true));
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
            super::NativeTerminateDecision::DeferExisting
        );
        assert!(arbiter.is_current(first));
        assert!(arbiter.complete(first));
        assert_eq!(arbiter.request(), super::NativeTerminateDecision::AllowExit);
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

    /// Help's destination must survive the gate `open_url_external` trusts its
    /// callers to have run: off macOS the row now actually opens a browser, and
    /// the URL it hands the OS shell must be one `is_safe_url` accepts —
    /// http(s)/mailto only, no control bytes, no `file://`.
    #[test]
    fn the_help_destination_passes_the_url_gate() {
        assert!(
            crate::is_safe_url(super::HELP_URL),
            "{} must satisfy the same allowlist a clicked link does",
            super::HELP_URL
        );
    }

    // ---- the System Settings deep link (design §3.4, §3.7, Appendix A.2) --------

    /// THE SCHEME HAS A DOT. `x-apple.systempreferences` is the real scheme;
    /// `x-apple-systempreferences` (hyphen) is a different string, and it is the
    /// one the link allowlist's rejection list names. A one-character slip here
    /// is invisible on screen and turns every deep link into a no-op, so every
    /// candidate is pinned — modern id, legacy alias and the un-anchored root
    /// alike.
    #[test]
    fn every_settings_deep_link_uses_the_dotted_scheme() {
        for pane in [
            super::PrivacyPane::FullDiskAccess,
            super::PrivacyPane::FilesAndFolders,
        ] {
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
    }

    /// NEVER A DEAD END. None of these URLs is Apple-documented, so the last
    /// candidate must be a page with NO anchor at all: if both anchors stop
    /// resolving, Privacy & Security still opens and the caller falls back to
    /// the route in words. Both panes must degrade to the SAME root — the
    /// combined Files-and-Folders section lives on the one page — and only the
    /// two registered anchors may ever appear, never a per-folder pseudo-anchor
    /// (`Privacy_DocumentsFolder` and its siblings are not anchors; Appendix
    /// A.2).
    #[test]
    fn the_last_candidate_is_the_unanchored_pane_root() {
        let fda = super::privacy_settings_urls(super::PrivacyPane::FullDiskAccess);
        let files = super::privacy_settings_urls(super::PrivacyPane::FilesAndFolders);
        for urls in [&fda, &files] {
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
        assert_eq!(fda[2], files[2], "both panes degrade to the same page");
        for url in fda.iter().chain(files.iter()) {
            let anchor = url.split_once('?').map(|(_, a)| a);
            assert!(
                matches!(
                    anchor,
                    None | Some("Privacy_AllFiles" | "Privacy_FilesAndFolders")
                ),
                "only a REGISTERED anchor may ship: {url:?}"
            );
        }
        // The words never claim a folder, and never claim what a grant covers —
        // that claim belongs to S4, which has not been run.
        for pane in [
            super::PrivacyPane::FullDiskAccess,
            super::PrivacyPane::FilesAndFolders,
        ] {
            let words = super::privacy_settings_path_words(pane);
            assert!(
                words.starts_with("System Settings"),
                "the words are a route, not a sentence: {words:?}"
            );
            assert!(
                !words.to_ascii_lowercase().contains("cover"),
                "coverage is S4's claim to make, not this route's: {words:?}"
            );
        }
    }

    /// THE ALLOWLIST STAYS CLOSED. `is_safe_url` decides what a Cmd-clicked
    /// hyperlink out of PROGRAM OUTPUT may open, and it must keep rejecting
    /// every one of these: a Settings pane a program can raise is a consent
    /// surface that program controls. The deep link is safe for the opposite
    /// reason — it is a compiled-in constant behind an owner gesture in aterm's
    /// own UI — and it reaches `NSWorkspace` on its own path, so widening the
    /// allowlist is never the way to make it work. This test fails the moment
    /// someone tries.
    #[test]
    fn program_output_still_cannot_open_a_settings_pane() {
        for pane in [
            super::PrivacyPane::FullDiskAccess,
            super::PrivacyPane::FilesAndFolders,
        ] {
            for url in super::privacy_settings_urls(pane) {
                assert!(
                    !crate::is_safe_url(url),
                    "a clicked link must never be able to open Settings: {url:?}"
                );
            }
        }
    }
}
