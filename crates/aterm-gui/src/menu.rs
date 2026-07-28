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
    /// check: current + staged build, what's-new notes, and Install & Relaunch. The same
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
    /// "⬆️ Update to v<staged> — restart now" item / the palette's Version row / the
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
    /// New Window — a fresh independent aterm process (`open_new_window`).
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
    /// Promote the FRONT SESSION's own kitty into the durable kitty registry and
    /// pin it as the cursor companion (`App::favourite_session_kitty`). Owner:
    /// "there is a unique kitty chosen per session … if somebody really likes
    /// that kitty it goes into the kitty registry". Terminal-only
    /// ([`requires_terminal_tab`]): a native whole tab has no session, hence no
    /// session kitty. The palette row's checkmark reports whether THIS session's
    /// kitty is the current pin. One-way: the pin is transferable, not
    /// toggleable.
    FavouriteSessionKitty,
    /// Toggle the process-wide serious-mode policy. While enabled it suppresses
    /// every audible and decorative effect without overwriting the underlying
    /// preferences; disabling it restores those requested settings exactly.
    ToggleSeriousMode,
    /// Focus or create the process-singleton Settings tab. The app-menu Settings…
    /// item — ⌘, — uses the standard macOS settings chord.
    ToggleSettings,
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
    // Window menu
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
            MenuAction::FavouriteSessionKitty => 46,
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
            46 => MenuAction::FavouriteSessionKitty,
            _ => return None,
        })
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
            // The favourite promotes the front SESSION's own kitty — a native
            // whole tab has no session, so it has no session kitty to pin.
            | MenuAction::FavouriteSessionKitty
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

fn native_menu_action_enabled(action: MenuAction) -> bool {
    !requires_terminal_tab(action)
        || ACTIVE_TAB_IS_TERMINAL.load(std::sync::atomic::Ordering::Relaxed)
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
            MenuAction::Preferences
            | MenuAction::ToggleSettings
            | MenuAction::ToggleSeriousMode => ConfigWrite,
            // Gateway to every action + the staged-update re-exec twins.
            MenuAction::OpenPalette | MenuAction::SoftwareUpdate | MenuAction::ApplyUpdate => {
                OwnerOnly
            }
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
            | MenuAction::FavouriteSessionKitty
            | MenuAction::Minimize
            | MenuAction::Zoom
            | MenuAction::NextTab
            | MenuAction::PrevTab
            | MenuAction::Help => WriteInput,
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
        match name {
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
            "FavouriteSessionKitty" => Some(MenuAction::FavouriteSessionKitty),
            "ToggleSeriousMode" => Some(MenuAction::ToggleSeriousMode),
            "ToggleSettings" => Some(MenuAction::ToggleSettings),
            "OpenPalette" => Some(MenuAction::OpenPalette),
            "Minimize" => Some(MenuAction::Minimize),
            "Zoom" => Some(MenuAction::Zoom),
            "NextTab" => Some(MenuAction::NextTab),
            "PrevTab" => Some(MenuAction::PrevTab),
            "Help" => Some(MenuAction::Help),
            "CopySessionId" => Some(MenuAction::CopySessionId),
            "CopyCwd" => Some(MenuAction::CopyCwd),
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
#[cfg_attr(target_os = "macos", allow(dead_code))]
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
}

/// A top-level menu (App / File / …) and its entries.
pub struct MenuSection {
    pub title: &'static str,
    pub entries: &'static [MenuEntry],
}

use MenuEntry::{Item, Separator};

const APP_MENU: &[MenuEntry] = &[
    Item {
        label: "About aterm",
        action: MenuAction::About,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    // The ONE update entry point: opens the Software Update route and checks in one gesture.
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
        label: "Favourite Session Kitty",
        action: MenuAction::FavouriteSessionKitty,
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
];

const HELP_MENU: &[MenuEntry] = &[Item {
    label: "aterm Help",
    action: MenuAction::Help,
    key: "",
    mods: MenuMods::None,
}];

/// The WHOLE menu bar, declaratively — the platform-neutral description the
/// cross-platform `chrome` introspection serialiser ([`menu_chrome_lines`]) renders so
/// the menu is introspectable on EVERY platform (off macOS there is no `NSMenu` to read).
/// Order is the standard Mac arrangement (App / File / Edit / View / Window / Help); the
/// App section is titled with the app name by convention.
///
/// It MIRRORS the macOS `NSMenu` the `install` builder constructs item-for-item (a
/// `#[test]` asserts every [`MenuAction`] appears here exactly once, so an action added
/// to one and not the other fails CI). Unifying `install` to build directly from this
/// model is a safe follow-up; it is kept descriptive here to avoid rewriting the
/// (host-only, untestable-in-CI) objc2 menu construction.
// On macOS the `chrome` verb reads the LIVE `NSMenu`, so the model + serialiser are used
// only off macOS (and by tests) — not dead, just per-target. The chain from `MENU_MODEL`
// keeps the sections/consts/types alive, so this one allow covers them.
#[cfg_attr(target_os = "macos", allow(dead_code))]
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
        label: "↑ Update — restart now",
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
/// REALIZED celebration is NOT a bar badge; it lives in the menu's "Updated to v… just
/// now" row and the transient LEVEL-UP notice / palette twin (self-dismissing after
/// [`crate::relaunch_notice::REALIZED_ARROW_TTL`]). The color emoji is safe HERE:
/// AppKit renders NSMenu titles/items with the system font + Apple Color Emoji
/// fallback. In-window overlay surfaces (palette rows, notice pill) must use plain `↑`
/// instead — the own-rendered text stack has no color-emoji face (verified coverage).
#[must_use]
pub(crate) fn version_menu_bar_title(attention: bool) -> String {
    let base = format!("v{}", crate::build_info::version_display());
    if attention {
        format!("{base} \u{2B06}\u{FE0F}")
    } else {
        base
    }
}

/// Whether the always-visible menu-bar Version arrow should show. It tracks a STAGED
/// update ONLY (action needed) — deliberately NOT the post-update `realized`
/// celebration. The celebration is carried by self-dismissing surfaces (the menu's
/// "Updated to v… just now" row, the LEVEL-UP notice, the palette twin), so an apply
/// that re-execs into the staged build (`staged` → `None`) clears the persistent bar
/// badge the instant it lands, instead of leaving an arrow up for the full realized
/// TTL that reads as "the update never resolved".
#[must_use]
pub(crate) fn bar_title_attention(staged_present: bool, _realized: bool) -> bool {
    staged_present
}

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
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn menu_chrome_lines() -> Vec<String> {
    MENU_MODEL
        .iter()
        .map(|section| {
            let labels: Vec<&str> = section
                .entries
                .iter()
                .filter_map(|e| match e {
                    MenuEntry::Item { label, .. } => Some(*label),
                    MenuEntry::Separator => None,
                })
                .collect();
            format!("menu {:?}: {}", section.title, labels.join(", "))
        })
        .collect()
}

#[cfg(target_os = "macos")]
pub use macos::{
    MenuHandle, choose_local_file, confirm, defer_quit_for_terminate, install, notify,
    open_help_url, update_version_menu,
};

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
pub fn update_version_menu(_handle: &MenuHandle, _staged: Option<(u64, &str)>, _realized: bool) {}

/// No portable native picker is linked off macOS. The global action remains visible in
/// the cross-platform command palette, but acquires no filesystem authority here.
#[cfg(not(target_os = "macos"))]
pub fn choose_local_file(_title: &str, _prompt: &str) -> Option<std::path::PathBuf> {
    None
}

/// Non-macOS stub: no native alert; the "Check for Updates" result is logged instead.
#[cfg(not(target_os = "macos"))]
pub fn notify(_title: &str, _body: &str) {}

/// Non-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn open_help_url() {}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{
        ClassType, DeclaredClass, class, declare_class, msg_send, msg_send_id, mutability, sel,
    };
    use objc2_app_kit::{
        NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem, NSModalResponseOK, NSOpenPanel,
    };
    use objc2_foundation::{MainThreadMarker, NSString};
    use winit::event_loop::EventLoopProxy;

    use super::{MenuAction, NativeTerminateArbiter, NativeTerminateDecision};
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
        version_item: Retained<NSMenuItem>,
    }

    declare_class!(
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
        pub(crate) struct MenuTarget;

        // SAFETY:
        // - NSObject imposes no subclassing requirements.
        // - InteriorMutable is the safe default; we never mutate the proxy.
        // - MenuTarget has no Drop impl beyond the auto-generated ivar drop.
        unsafe impl ClassType for MenuTarget {
            type Super = objc2::runtime::NSObject;
            type Mutability = mutability::InteriorMutable;
            const NAME: &'static str = "ATermMenuTarget";
        }

        impl DeclaredClass for MenuTarget {
            type Ivars = EventLoopProxy<Wake>;
        }

        unsafe impl MenuTarget {
            /// `menuAction:` — the single selector wired to every item. `sender`
            /// is the clicked `NSMenuItem`; its `tag` decodes to the action. A tag
            /// that doesn't decode is ignored (no dispatch), so a stray/zero tag
            /// is inert rather than firing the wrong command.
            #[method(menuAction:)]
            fn menu_action(&self, sender: Option<&NSMenuItem>) {
                let Some(item) = sender else { return };
                // SAFETY: `item` is the live NSMenuItem AppKit passed as the
                // action sender; `tag` is a plain getter with no side effects.
                let tag = unsafe { item.tag() };
                if let Some(action) = MenuAction::from_tag(tag) {
                    // Fire-and-forget: a closed loop (app shutting down) just
                    // drops the event — mirrors every other `send_event` here.
                    let _ = self.ivars().send_event(Wake::MenuAction { action });
                }
            }

            /// AppKit asks this immediately before displaying/dispatching a menu
            /// item. Terminal-only commands grey out over a native whole tab instead
            /// of accepting a click that the host can only ignore.
            #[method(validateMenuItem:)]
            fn validate_menu_item(&self, sender: Option<&NSMenuItem>) -> bool {
                let Some(item) = sender else {
                    return false.into();
                };
                // SAFETY: AppKit supplied a live NSMenuItem; reading its integer tag
                // has no side effects. Unknown/untagged items fail closed.
                let tag = unsafe { item.tag() };
                MenuAction::from_tag(tag).is_some_and(super::native_menu_action_enabled)
            }
        }
    );

    impl MenuTarget {
        /// Allocate a target owning `proxy`. `mtm` proves we are on the main
        /// thread (AppKit requirement), which the winit loop guarantees.
        fn new(mtm: MainThreadMarker, proxy: EventLoopProxy<Wake>) -> Retained<Self> {
            let this = mtm.alloc().set_ivars(proxy);
            // SAFETY: plain `[super init]` on a freshly allocated instance.
            unsafe { msg_send_id![super(this), init] }
        }
    }

    /// Build aterm's menu bar and install it as the shared application's main
    /// menu. Returns the retained action [`MenuTarget`] for the caller to keep
    /// alive (AppKit holds menu-item targets only weakly). Called from `resumed`
    /// after the window exists and only when NOT headless.
    ///
    /// Best-effort: if we are somehow off the main thread (`MainThreadMarker::new`
    /// is `None`) the menu is simply not installed — never a panic. The winit
    /// event loop always runs `resumed` on the main thread, so in practice the
    /// marker is always present.
    pub fn install(proxy: &EventLoopProxy<Wake>) -> Option<MenuHandle> {
        let mtm = MainThreadMarker::new()?;
        let _ = TERMINATE_PROXY.set(proxy.clone());
        let app = NSApplication::sharedApplication(mtm);
        let target = MenuTarget::new(mtm, proxy.clone());

        let main = NSMenu::new(mtm);

        // Each submenu is built in full, then attached under its top-level title.
        // Order is App / File / Edit / View / Window / Help, the standard Mac
        // arrangement — preserved exactly by the order of these calls.
        let _ = attach_submenu(mtm, &main, "aterm", build_app_menu(mtm, &target));
        let _ = attach_submenu(mtm, &main, "File", build_file_menu(mtm, &target));
        let _ = attach_submenu(mtm, &main, "Edit", build_edit_menu(mtm, &target));
        let _ = attach_submenu(mtm, &main, "View", build_view_menu(mtm, &target));
        let _ = attach_submenu(mtm, &main, "Window", build_window_menu(mtm, &target));
        let _ = attach_submenu(mtm, &main, "Help", build_help_menu(mtm, &target));
        // The version identity goes LAST — after Help, so `v<version>` is the rightmost
        // menu-bar title (a quiet trailing build badge). Installed in its PLAIN state
        // (no update staged at boot); `App::refresh_version_menu` retitles it (via
        // [`update_version_menu`], through the retained item below) when an update
        // stages or the post-update realized arrow appears/expires. A bare top-level
        // item with an action greys out in the main menu bar, so its commands are
        // reached via this submenu (the reliable, idiomatic AppKit shape).
        let version_item = attach_submenu(
            mtm,
            &main,
            &super::version_menu_bar_title(false),
            build_version_menu(mtm, &target, None, false),
        );

        app.setMainMenu(Some(&main));
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
    /// item set an exact function of the state — no stale "restart now" rows. Best-effort:
    /// off the main thread it is a no-op (never a panic), like every AppKit helper here.
    ///
    /// The persistent MENU-BAR arrow tracks `staged` ONLY — it means "an update is
    /// waiting, act on it". After an apply re-execs into that build `staged` is `None`,
    /// so the bar arrow clears the instant the update lands (no 10-min lingering badge
    /// that reads as "the update never resolved"). The freshly-REALIZED celebration
    /// still lives INSIDE the menu — its "Updated to v… just now" row — and in the
    /// transient LEVEL-UP notice / palette twin, both of which self-dismiss; only the
    /// always-visible bar badge is gated to the action-needed state.
    pub fn update_version_menu(handle: &MenuHandle, staged: Option<(u64, &str)>, realized: bool) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let title =
            super::version_menu_bar_title(super::bar_title_attention(staged.is_some(), realized));
        let submenu = build_version_menu(mtm, &handle.target, staged, realized);
        // Set the title on BOTH the bar item and the submenu: AppKit takes a top-level
        // bar title from whichever is authoritative for the toolkit version in play
        // (historically the submenu's title), so writing both is the robust retitle.
        // SAFETY: plain main-thread setters on live retained objects; the NSStrings
        // outlive the calls (same contract as every setter in `add_item_mods`).
        unsafe {
            submenu.setTitle(&NSString::from_str(&title));
            handle.version_item.setTitle(&NSString::from_str(&title));
        }
        handle.version_item.setSubmenu(Some(&submenu));
    }

    /// Build the App menu (titled with the app name by convention): About,
    /// Settings (⌘, — the native tab), Open aterm.toml in Manual, Quit. Items and separators
    /// preserved verbatim from [`install`].
    fn build_app_menu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenu> {
        let app_menu = NSMenu::new(mtm);
        add_item(
            mtm,
            &app_menu,
            target,
            "About aterm",
            MenuAction::About,
            "",
            false,
        );
        add_separator(mtm, &app_menu);
        // The ONE update entry point — opens the overlay and checks in one gesture.
        add_item(
            mtm,
            &app_menu,
            target,
            "Check for Updates…",
            MenuAction::SoftwareUpdate,
            "",
            false,
        );
        add_separator(mtm, &app_menu);
        // ⌘, — the standard macOS settings chord — focuses the own-rendered native
        // Settings tab (the separate Preferences NSWindow is retired). The menu key
        // equivalent IS the shortcut: AppKit's performKeyEquivalent dispatches it
        // into the Wake::MenuAction relay before keyDown reaches on_key.
        add_item(
            mtm,
            &app_menu,
            target,
            "Settings…",
            MenuAction::ToggleSettings,
            ",",
            true,
        );
        add_item(
            mtm,
            &app_menu,
            target,
            "Open aterm.toml",
            MenuAction::Preferences,
            "",
            false,
        );
        add_separator(mtm, &app_menu);
        add_item(
            mtm,
            &app_menu,
            target,
            "Quit aterm",
            MenuAction::Quit,
            "q",
            true,
        );
        app_menu
    }

    /// The Version submenu for the given update state. Mirrors [`super::VERSION_MENU`]
    /// in the portable model (whose ApplyUpdate row the palette rewrites the same way):
    ///   * STAGED: "⬆️ Update to v<staged> — restart now" (ONE click applies — the
    ///     owner's "click-upgrade" ask), then About, then "Update details…" (the
    ///     Software Update route stays reachable as the DETAILS surface).
    ///   * REALIZED (fresh post-update, no new stage): "⬆️ Updated to v<current> just
    ///     now" (fires About — the celebration row is informative, not destructive),
    ///     then About.
    ///   * NEITHER: just About — the quiet steady-state badge menu.
    fn build_version_menu(
        mtm: MainThreadMarker,
        target: &MenuTarget,
        staged: Option<(u64, &str)>,
        realized: bool,
    ) -> Retained<NSMenu> {
        let menu = NSMenu::new(mtm);
        if let Some((_, version)) = staged {
            add_item(
                mtm,
                &menu,
                target,
                &format!("\u{2B06}\u{FE0F} Update to v{version} — restart now"),
                MenuAction::ApplyUpdate,
                "",
                false,
            );
            add_separator(mtm, &menu);
        } else if realized {
            add_item(
                mtm,
                &menu,
                target,
                &format!(
                    "\u{2B06}\u{FE0F} Updated to v{} just now",
                    crate::build_info::version_display()
                ),
                MenuAction::Version,
                "",
                false,
            );
            add_separator(mtm, &menu);
        }
        add_item(
            mtm,
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
                mtm,
                &menu,
                target,
                "Update details…",
                MenuAction::SoftwareUpdate,
                "",
                false,
            );
        }
        menu
    }

    /// Build the File menu: New Window/Tab, the tab-relocation commands, and
    /// Close Tab. Items, modifier masks, and separators preserved verbatim from
    /// [`install`].
    fn build_file_menu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenu> {
        let file = NSMenu::new(mtm);
        add_item(
            mtm,
            &file,
            target,
            "New Window",
            MenuAction::NewWindow,
            "n",
            true,
        );
        add_item(
            mtm,
            &file,
            target,
            "New Terminal Tab",
            MenuAction::NewTab,
            "t",
            true,
        );
        add_separator(mtm, &file);
        add_item(
            mtm,
            &file,
            target,
            "Open Markdown…",
            MenuAction::OpenMarkdown,
            "",
            false,
        );
        add_item(
            mtm,
            &file,
            target,
            "Open File in Editor…",
            MenuAction::OpenEditor,
            "o",
            true,
        );
        add_item_mods(
            mtm,
            &file,
            target,
            "Reopen Closed Native Tab",
            MenuAction::ReopenClosedTab,
            "t",
            command_shift_mask(),
        );
        add_separator(mtm, &file);
        // Cmd-Shift-N moves the active tab out into a new in-process window.
        add_item_mods(
            mtm,
            &file,
            target,
            "Move Tab to New Window",
            MenuAction::MoveTabToNewWindow,
            "n",
            command_shift_mask(),
        );
        // Cmd-Shift-M moves the active tab into the NEXT existing window (wrapping).
        add_item_mods(
            mtm,
            &file,
            target,
            "Move Tab to Next Window",
            MenuAction::MoveTabToNextWindow,
            "m",
            command_shift_mask(),
        );
        // Cmd-Shift-O opens the active session in a SECOND window (same live grid in
        // two windows — watch a log in one, type in another). The key MUST match
        // on_key's Cmd-Shift-O: AppKit's performKeyEquivalent intercepts a menu key
        // equivalent BEFORE the keyDown reaches on_key, so a "d" here would shadow
        // Cmd-Shift-D (SplitHorizontal) and make that primary chord keyboard-dead.
        add_item_mods(
            mtm,
            &file,
            target,
            "Open Session in New Window",
            MenuAction::ViewSessionInNewWindow,
            "o",
            command_shift_mask(),
        );
        add_separator(mtm, &file);
        add_item(
            mtm,
            &file,
            target,
            "Close Tab",
            MenuAction::CloseTab,
            "w",
            true,
        );
        file
    }

    /// Build the Edit menu: Copy, Paste, Select All, Find. Items and separators
    /// preserved verbatim from [`install`].
    fn build_edit_menu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenu> {
        let edit = NSMenu::new(mtm);
        add_item(mtm, &edit, target, "Copy", MenuAction::Copy, "c", true);
        add_item(mtm, &edit, target, "Paste", MenuAction::Paste, "v", true);
        add_item(
            mtm,
            &edit,
            target,
            "Select All",
            MenuAction::SelectAll,
            "a",
            true,
        );
        add_separator(mtm, &edit);
        add_item(mtm, &edit, target, "Find…", MenuAction::Find, "f", true);
        add_item(
            mtm,
            &edit,
            target,
            "Find Next",
            MenuAction::FindNext,
            "g",
            true,
        );
        add_item_mods(
            mtm,
            &edit,
            target,
            "Find Previous",
            MenuAction::FindPrev,
            "g",
            command_shift_mask(),
        );
        edit
    }

    /// Build the View menu: Enter Full Screen. Modifier mask preserved verbatim
    /// from [`install`].
    fn build_view_menu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenu> {
        let view = NSMenu::new(mtm);
        // Font size: ⌘= / ⌘- / ⌘0 (the chords App::on_key_font_zoom already handles).
        add_item(
            mtm,
            &view,
            target,
            "Increase Font Size",
            MenuAction::FontIncrease,
            "+",
            true,
        );
        add_item(
            mtm,
            &view,
            target,
            "Decrease Font Size",
            MenuAction::FontDecrease,
            "-",
            true,
        );
        add_item(
            mtm,
            &view,
            target,
            "Actual Size",
            MenuAction::FontActualSize,
            "0",
            true,
        );
        add_separator(mtm, &view);
        // Splits: ⌘D (left/right) and ⇧⌘D (top/bottom), matching on_key.
        add_item(
            mtm,
            &view,
            target,
            "Split Right",
            MenuAction::SplitVertical,
            "d",
            true,
        );
        add_item_mods(
            mtm,
            &view,
            target,
            "Split Down",
            MenuAction::SplitHorizontal,
            "d",
            command_shift_mask(),
        );
        add_separator(mtm, &view);
        // Cmd-Ctrl-F is the macOS-standard Enter Full Screen equivalent.
        add_item_mods(
            mtm,
            &view,
            target,
            "Enter Full Screen",
            MenuAction::ToggleFullScreen,
            "f",
            command_control_mask(),
        );
        add_separator(mtm, &view);
        // Process-wide effect suppression. No key-equivalent: it is bindable as
        // `toggle_serious_mode` and also exposed through the command palette.
        add_item(
            mtm,
            &view,
            target,
            "Serious Mode",
            MenuAction::ToggleSeriousMode,
            "",
            false,
        );
        // PHOSPHOR matrix rain — the per-session toggle (front session of the
        // frontmost window; greys out over a native whole tab via
        // `requires_terminal_tab`). No key-equivalent; `[keybindings]` can bind
        // `toggle_matrix_rain` for a chord.
        add_item(
            mtm,
            &view,
            target,
            "Matrix Rain",
            MenuAction::ToggleMatrixRain,
            "",
            false,
        );
        // Promote the front session's own kitty into the durable registry.
        // Terminal-only, so `requires_terminal_tab` greys it over a native
        // whole tab. No key-equivalent: a rare, one-way act, reachable from the
        // menu bar and the ⇧⌘P palette (the cross-platform surface).
        add_item(
            mtm,
            &view,
            target,
            "Favourite Session Kitty",
            MenuAction::FavouriteSessionKitty,
            "",
            false,
        );
        add_separator(mtm, &view);
        add_separator(mtm, &view);
        // The own-rendered, cross-platform command palette (⇧⌘P). A real menu key
        // equivalent, so AppKit's performKeyEquivalent dispatches it into the SAME
        // Wake::MenuAction relay — making the palette reachable by keyboard on macOS
        // (where platform_defaults ships no keybindings).
        add_item_mods(
            mtm,
            &view,
            target,
            "Command Palette…",
            MenuAction::OpenPalette,
            "p",
            command_shift_mask(),
        );
        view
    }

    /// Build the Window menu: Minimize, Zoom. Items preserved verbatim from
    /// [`install`].
    fn build_window_menu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenu> {
        let window = NSMenu::new(mtm);
        add_item(
            mtm,
            &window,
            target,
            "Minimize",
            MenuAction::Minimize,
            "m",
            true,
        );
        add_item(mtm, &window, target, "Zoom", MenuAction::Zoom, "", false);
        add_separator(mtm, &window);
        // Tab navigation: ⇧⌘] / ⇧⌘[ (the chords on_key already handles).
        add_item_mods(
            mtm,
            &window,
            target,
            "Show Next Tab",
            MenuAction::NextTab,
            "]",
            command_shift_mask(),
        );
        add_item_mods(
            mtm,
            &window,
            target,
            "Show Previous Tab",
            MenuAction::PrevTab,
            "[",
            command_shift_mask(),
        );
        window
    }

    /// Build the Help menu: aterm Help. Item preserved verbatim from [`install`].
    fn build_help_menu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenu> {
        let help = NSMenu::new(mtm);
        add_item(
            mtm,
            &help,
            target,
            "aterm Help",
            MenuAction::Help,
            "",
            false,
        );
        help
    }

    /// `Cmd` modifier mask (the default for a single-letter key equivalent).
    fn command_mask() -> NSEventModifierFlags {
        NSEventModifierFlags::NSEventModifierFlagCommand
    }

    /// `Cmd-Ctrl` mask (Enter Full Screen's standard equivalent).
    fn command_control_mask() -> NSEventModifierFlags {
        NSEventModifierFlags(
            NSEventModifierFlags::NSEventModifierFlagCommand.0
                | NSEventModifierFlags::NSEventModifierFlagControl.0,
        )
    }

    /// `Cmd-Shift` mask (Move Tab to New Window's ⇧⌘N equivalent).
    fn command_shift_mask() -> NSEventModifierFlags {
        NSEventModifierFlags(
            NSEventModifierFlags::NSEventModifierFlagCommand.0
                | NSEventModifierFlags::NSEventModifierFlagShift.0,
        )
    }

    /// Build one menu item wired to `menuAction:` on `target`, tagged with
    /// `action`, and append it to `menu`. `key` is the lowercase key-equivalent
    /// character ("" for none); `cmd` adds the ⌘ modifier (a Cmd shortcut). The
    /// equivalent is VISUAL only — it just renders next to the item; the actual
    /// keystroke is still handled by `App::on_key`.
    fn add_item(
        mtm: MainThreadMarker,
        menu: &NSMenu,
        target: &MenuTarget,
        title: &str,
        action: MenuAction,
        key: &str,
        cmd: bool,
    ) {
        let mods = if cmd {
            command_mask()
        } else {
            NSEventModifierFlags(0)
        };
        add_item_mods(mtm, menu, target, title, action, key, mods);
    }

    /// As [`add_item`] but with an explicit modifier mask (for non-⌘ equivalents
    /// like Enter Full Screen's ⌃⌘F).
    fn add_item_mods(
        mtm: MainThreadMarker,
        menu: &NSMenu,
        target: &MenuTarget,
        title: &str,
        action: MenuAction,
        key: &str,
        mods: NSEventModifierFlags,
    ) {
        let title = NSString::from_str(title);
        let key = NSString::from_str(key);
        // Build with the menuAction: selector so AppKit dispatches to `target`.
        let sel: Sel = sel!(menuAction:);
        // SAFETY: standard NSMenuItem construction + plain setters on a fresh
        // instance, all on the main thread (`mtm`). The selector exists on
        // MenuTarget (declared above). `setTarget`/`setTag`/`setKeyEquivalent*`
        // have no preconditions beyond a live receiver.
        unsafe {
            let item: Retained<NSMenuItem> = NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &title,
                Some(sel),
                &key,
            );
            // Deref-coerce MenuTarget -> NSObject -> AnyObject for the `id`
            // target argument (same pattern as accessibility.rs).
            let target_obj: &AnyObject = target;
            item.setTarget(Some(target_obj));
            item.setTag(action.tag());
            if !key.is_empty() {
                item.setKeyEquivalentModifierMask(mods);
            }
            menu.addItem(&item);
        }
    }

    /// Append a separator line to `menu`.
    fn add_separator(mtm: MainThreadMarker, menu: &NSMenu) {
        let sep = NSMenuItem::separatorItem(mtm);
        // `addItem` is a safe binding in objc2-app-kit; no `unsafe` needed.
        menu.addItem(&sep);
    }

    /// Present the system open panel and return exactly the one local path the user
    /// approved. The panel grants no directory or multiple-file authority; the caller
    /// still canonicalizes, bounds, UTF-8-validates, and mints the process-local
    /// document grant before reading the file.
    pub fn choose_local_file(title: &str, prompt: &str) -> Option<std::path::PathBuf> {
        let mtm = MainThreadMarker::new()?;
        let title = NSString::from_str(title);
        let prompt = NSString::from_str(prompt);
        // SAFETY: NSOpenPanel is created and run on AppKit's main thread. These are
        // plain property setters; `runModal` owns its nested modal loop and URL until
        // it returns. Only an affirmative response is converted to a local path.
        unsafe {
            let panel = NSOpenPanel::openPanel(mtm);
            panel.setCanChooseFiles(true);
            panel.setCanChooseDirectories(false);
            panel.setAllowsMultipleSelection(false);
            panel.setResolvesAliases(true);
            panel.setTitle(Some(&title));
            panel.setPrompt(Some(&prompt));
            if panel.runModal() != NSModalResponseOK {
                return None;
            }
            let url = panel.URL()?;
            let path = url.path()?;
            Some(std::path::PathBuf::from(path.to_string()))
        }
    }

    /// Attach `submenu` under a new top-level item titled `title` on `bar`, returning
    /// the retained bar item so a caller can keep a live handle to it (the Version
    /// menu is retitled/rebuilt through its item — see [`update_version_menu`]; the
    /// other menus ignore the return). The item carries no action (its only job is to
    /// hold the submenu). The submenu is titled to match: AppKit takes a top-level
    /// bar title from the submenu on some paths, so both must agree.
    fn attach_submenu(
        mtm: MainThreadMarker,
        bar: &NSMenu,
        title: &str,
        submenu: Retained<NSMenu>,
    ) -> Retained<NSMenuItem> {
        let title = NSString::from_str(title);
        // SAFETY: standard top-level menu-item creation + title/submenu setters on
        // fresh instances, all on the main thread.
        unsafe {
            submenu.setTitle(&title);
            let item: Retained<NSMenuItem> = NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &title,
                None,
                &NSString::from_str(""),
            );
            item.setSubmenu(Some(&submenu));
            bar.addItem(&item);
            item
        }
    }

    /// Show a native modal confirmation alert (a ⌘Q quit, or a close gesture that
    /// would lose work) and block until the user answers. `title` is the primary
    /// message, `body` the secondary explanatory line, and `proceed_label` titles the
    /// affirmative (destructive) button — the DEFAULT button, so Return confirms; a
    /// "Cancel" button is always added and Escape maps to it. Returns `true` iff the
    /// user chose to proceed.
    ///
    /// `runModal` spins a nested modal run loop on the main thread (the standard
    /// AppKit pattern, the same one native file pickers use), so it is safe to call
    /// straight from the winit event handler. Best-effort: if somehow off the main
    /// thread it returns `true` (proceed) so a quit can never wedge.
    pub fn confirm(title: &str, body: &str, proceed_label: &str) -> bool {
        if MainThreadMarker::new().is_none() {
            return true;
        }
        let title = NSString::from_str(title);
        let body = NSString::from_str(body);
        let proceed = NSString::from_str(proceed_label);
        let cancel = NSString::from_str("Cancel");
        // SAFETY: standard `NSAlert` construction + setters + `runModal`, all on the
        // main thread. Every operand is a valid, retained object for the call;
        // `runModal` returns the clicked button's `NSModalResponse` (an `isize`). The
        // alert keeps the default `NSAlertStyleWarning` (the app-icon caution panel).
        unsafe {
            let alert: Retained<AnyObject> = msg_send_id![class!(NSAlert), new];
            let _: () = msg_send![&alert, setMessageText: &*title];
            let _: () = msg_send![&alert, setInformativeText: &*body];
            // First button added is the default (responds to Return): the PROCEED
            // action. The second is Cancel (AppKit binds Escape to it).
            let _: Retained<AnyObject> = msg_send_id![&alert, addButtonWithTitle: &*proceed];
            let _: Retained<AnyObject> = msg_send_id![&alert, addButtonWithTitle: &*cancel];
            let response: isize = msg_send![&alert, runModal];
            // NSAlertFirstButtonReturn == 1000 → the user clicked PROCEED.
            response == 1000
        }
    }

    /// Show a simple informational alert (a single OK button) and block until the user
    /// dismisses it — the visible result of App menu ▸ Check for Updates…. `title` is the
    /// primary line, `body` the details (version + "what changed"). Best-effort: off the
    /// main thread it does nothing. `runModal` is the same nested-modal pattern `confirm`
    /// uses, safe to call straight from the winit event handler.
    pub fn notify(title: &str, body: &str) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let title = NSString::from_str(title);
        let body = NSString::from_str(body);
        let ok = NSString::from_str("OK");
        // SAFETY: standard `NSAlert` construction + setters + `runModal`, all on the main
        // thread; every operand is a valid, retained object for the call.
        unsafe {
            let alert: Retained<AnyObject> = msg_send_id![class!(NSAlert), new];
            let _: () = msg_send![&alert, setMessageText: &*title];
            let _: () = msg_send![&alert, setInformativeText: &*body];
            let _: Retained<AnyObject> = msg_send_id![&alert, addButtonWithTitle: &*ok];
            let _: isize = msg_send![&alert, runModal];
        }
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
            open_in_workspace("https://github.com/alabsystems/aterm", false);
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
    fn open_in_workspace(s: &str, is_file: bool) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let ns = NSString::from_str(s);
        // SAFETY: `NSURL`/`NSWorkspace` are standard AppKit; the string is valid and
        // retained for the call. `openURL:` returns BOOL, which we ignore.
        unsafe {
            let url: Retained<AnyObject> = if is_file {
                msg_send_id![class!(NSURL), fileURLWithPath: &*ns]
            } else {
                msg_send_id![class!(NSURL), URLWithString: &*ns]
            };
            let ws: Retained<AnyObject> = msg_send_id![class!(NSWorkspace), sharedWorkspace];
            let _: bool = msg_send![&ws, openURL: &*url];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MENU_MODEL, MenuAction, MenuEntry, menu_chrome_lines, native_menu_action_enabled,
        set_active_tab_is_terminal,
    };

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
        MenuAction::ToggleSeriousMode,
        MenuAction::ToggleMatrixRain,
        MenuAction::FavouriteSessionKitty,
        MenuAction::ToggleSettings,
        MenuAction::OpenPalette,
        MenuAction::Help,
        MenuAction::Version,
        MenuAction::ApplyUpdate,
    ];

    /// The actions that live ONLY in the tab-strip CONTEXT menu (session-metadata
    /// stage 2, `session_chrome::compose_tab_menu`) — deliberately NOT in the menu
    /// bar, hence not in [`ALL_ACTIONS`]/`MENU_MODEL`. (`CloseTab` also appears in
    /// the context menu, but it is a BAR action first and lives in the list above.)
    /// Kept as a named twin list so the round-trip/uniqueness proofs cover the
    /// whole enum: `ALL_ACTIONS ∪ TAB_CONTEXT_ACTIONS`.
    const TAB_CONTEXT_ACTIONS: &[MenuAction] = &[MenuAction::CopySessionId, MenuAction::CopyCwd];

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
    /// they classify as the clipboard boundary, and they must NOT leak into
    /// `MENU_MODEL` (the bar mirror stays exactly the bar).
    #[test]
    fn tab_context_actions_round_trip_and_stay_out_of_the_bar() {
        for a in TAB_CONTEXT_ACTIONS.iter().copied() {
            let name = format!("{a:?}");
            assert_eq!(MenuAction::from_invoke_name(&name), Some(a));
            assert_eq!(
                a.invoke_authority(),
                super::InvokeAuthority::ClipboardWrite,
                "{a:?} moves text onto the pasteboard"
            );
            assert!(
                !MENU_MODEL.iter().any(|s| s
                    .entries
                    .iter()
                    .any(|e| matches!(e, MenuEntry::Item { action, .. } if *action == a))),
                "{a:?} is context-menu-only, never a bar item"
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
        let model = aterm_spec::derive::native_update_menu_activation_model();
        let mut state = model.init_state();
        for action in ["StageUpdate", "RefreshStagedVersionMenu"] {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        assert!(
            MENU_MODEL
                .iter()
                .any(|section| section.entries.iter().any(|entry| {
                    matches!(
                        entry,
                        MenuEntry::Item {
                            action: MenuAction::ApplyUpdate,
                            ..
                        }
                    )
                })),
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

    /// MENU_MODEL mirrors the macOS builders item-for-item: every command appears in it
    /// EXACTLY once, and no extra/unknown action. This is what keeps the cross-platform
    /// `chrome` serialisation in lockstep with the native menu (and fails CI if a command
    /// is added to one and not the other).
    #[test]
    fn menu_model_covers_every_action_exactly_once() {
        let mut model: Vec<MenuAction> = MENU_MODEL
            .iter()
            .flat_map(|s| s.entries.iter())
            .filter_map(|e| match e {
                MenuEntry::Item { action, .. } => Some(*action),
                MenuEntry::Separator => None,
            })
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

    /// The `chrome` serialiser emits one `menu "<title>": …` line per section, in the
    /// standard Mac order, with the section's non-separator item labels.
    #[test]
    fn chrome_lines_render_titled_sections() {
        let lines = menu_chrome_lines();
        let titles: Vec<&str> = lines.iter().map(|l| l.as_str()).collect();
        assert_eq!(
            lines.len(),
            7,
            "one line per top-level menu (incl. Version)"
        );
        assert!(titles[0].starts_with("menu \"aterm\": "), "{:?}", titles[0]);
        // The Version menu sits LAST — after Help (rightmost); it carries the ONE-CLICK
        // update apply (the primary update affordance) then About.
        assert!(titles[5].starts_with("menu \"Help\": "), "{:?}", titles[5]);
        assert!(
            titles[6].starts_with("menu \"Version\": "),
            "{:?}",
            titles[6]
        );
        assert!(
            titles[6].contains("About aterm — build & version…"),
            "Version menu opens About: {:?}",
            titles[6]
        );
        assert!(
            titles[6].contains("↑ Update — restart now"),
            "Version menu carries the one-click update apply: {:?}",
            titles[6]
        );
        assert!(titles[1].starts_with("menu \"File\": "));
        assert!(titles[3].starts_with("menu \"View\": "));
        // Separators are skipped; labels are comma-joined.
        assert!(
            titles[1]
                .contains("New Window, New Terminal Tab, Open Markdown…, Open File in Editor…"),
            "File labels in order: {:?}",
            titles[1]
        );
        assert!(
            titles[1].contains("Reopen Closed Tab") && titles[1].contains("Reopen Closed View"),
            "tab and split-view recovery are separately discoverable: {:?}",
            titles[1]
        );
        assert!(
            titles[0].contains("Settings…") && titles[0].contains("Open aterm.toml"),
            "the app menu lists Settings (⌘,) and the aterm.toml escape hatch: {:?}",
            titles[0]
        );
        assert!(
            !titles[0].contains("Hide aterm"),
            "the Hide item is removed from the app menu: {:?}",
            titles[0]
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
            MENU_MODEL
                .iter()
                .flat_map(|s| s.entries.iter())
                .find_map(|e| match e {
                    MenuEntry::Item {
                        action: a,
                        key,
                        mods,
                        ..
                    } if *a == action => Some((*key, *mods)),
                    _ => None,
                })
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
                    // Same reason: no session ⇒ no session kitty to promote.
                    | MenuAction::FavouriteSessionKitty
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
        assert!(plain.starts_with('v'), "{plain}");
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
}
