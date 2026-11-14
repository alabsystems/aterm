// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native macOS application menu bar (`NSApp.mainMenu`).

/// Pure arbitration behind AppKit's synchronous `applicationShouldTerminate:`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) enum NativeTerminateDecision {
    Dispatch(u64),
    DeferExisting(u64),
    AllowExit(u64),
}

#[cfg(any(target_os = "macos", test))]
impl NativeTerminateDecision {
    const fn generation(self) -> u64 {
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

    /// Cancel whichever quit is pending, answering its generation.
    const fn cancel_current(&mut self) -> Option<u64> {
        if self.exiting.is_some() {
            return None;
        }
        self.pending.take()
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

thread_local! {
    /// The generation whose `terminate:` AppKit was told to wait on (`NSTerminateLater`,
    /// [`offer_terminate`]): a cancel of it owes AppKit NO, once ([`answer_cancelled`]); once it
    /// completes, the event loop's exit answers YES (aterm-window), so AppKit waits until this
    /// process ends. AppKit asks, waits and is answered on the main thread, where the App runs,
    /// so this is that thread's fact, as aterm-window's record of the reply it owes is.
    static AWAITED: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

#[cfg(unix)]
pub(crate) fn native_termination_is_current(generation: u64) -> bool {
    with_native_terminate(|state| state.is_current(generation))
}

/// Whether AppKit waits on this process's reply to a quit (`NSTerminateLater`), which only the
/// event loop's exit (YES) or a cancel of that quit (NO) gives: until then the process ends by no
/// other path.
#[cfg(unix)]
pub(crate) fn appkit_awaits_quit_reply() -> bool {
    AWAITED.get().is_some()
}

/// A `terminate:` from AppKit that `asker` asked for, admitted by the arbiter and noted as theirs,
/// as a test sets a quit up; AppKit's own ask goes through [`ask_native_terminate`].
#[cfg(test)]
pub(crate) fn admit_native_terminate(asker: crate::system_quit::Asker) -> NativeTerminateDecision {
    with_native_terminate(|state| {
        let decision = state.request();
        crate::system_quit::note(decision.generation(), asker);
        decision
    })
}

/// AppKit's `terminate:` that `asker` asked for, `answerable` saying whether a reply could come
/// from AppKit's wait (aterm-window's `ReplyWait`), admitted by the arbiter and answered as
/// [`terminate_reply`] decides. An ask the reply takes up, or one past the quit's point of no
/// return, is noted as `asker`'s (`system_quit::note`); a refused one ([`TerminateReply::Cancel`])
/// marks nothing and leaves the arbiter as it stood, the fresh generation its ask minted cancelled
/// with it.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) fn ask_native_terminate(
    asker: crate::system_quit::Asker,
    answerable: bool,
) -> TerminateReply {
    let reply = with_native_terminate(|state| {
        let decision = state.request();
        let reply = terminate_reply(decision, asker, answerable);
        match (reply, decision) {
            (TerminateReply::Cancel, NativeTerminateDecision::Dispatch(generation)) => {
                let _ = state.cancel(generation);
            }
            (TerminateReply::Cancel, _) => {}
            _ => crate::system_quit::note(decision.generation(), asker),
        }
        reply
    });
    if !answerable
        && reply == TerminateReply::Cancel
        && matches!(asker, crate::system_quit::Asker::System)
    {
        aterm_log::info!(
            "a logout, restart or shutdown was asked while aterm's own alert or panel was open, \
             where no reply could come: refused, so macOS abandons it, and aterm stays"
        );
    }
    reply
}

/// What AppKit's `applicationShouldTerminate:` answers a `terminate:` ([`terminate_reply`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) enum TerminateReply {
    /// The quit is past its point of no return: AppKit terminates now.
    Now,
    /// AppKit's ask is refused (`NSTerminateCancel`) and nothing is taken up: a person's second
    /// ask of the quit pending, which answers nothing its question does not, so that quit goes on
    /// as it stood; or a logout, restart or shutdown asked where no reply could come, which macOS
    /// abandons once refused, so aterm does not quit under it either.
    Cancel,
    /// The window takes up the quit of the generation (`Wake::NativeTerminateRequested`,
    /// `App::native_termination_ready`) and AppKit's ask is cancelled (`NSTerminateCancel`), so
    /// nothing waits on a reply: a person's own quit (the Dock's, AppleScript's), whose question
    /// stands as long as the person leaves it, and a Quit & Reopen asked where no reply could
    /// come, which quits once aterm's alert or panel returns and leaves its reopen helper
    /// (`quit_reopen`). A logout or Quit & Reopen sent onto such a quit later is put to aterm,
    /// since AppKit then waits on nothing.
    TakeUp(u64),
    /// The window takes up the quit of the generation and AppKit waits on its one reply
    /// (`NSTerminateLater`, [`offer_terminate`]): YES from the event loop's exit once the quit's
    /// barriers let it go and the run's end is done, NO when the quit is cancelled. A quit the
    /// system (a logout, restart or shutdown) or System Settings' Quit & Reopen asked for, new or
    /// sent onto the quit pending, which makes that quit theirs (`system_quit::explicit`): it asks
    /// nothing, so AppKit's wait lasts only through the barriers and the run's end. While AppKit
    /// waits it puts no second `terminate:` to aterm (measured on macOS 27.2: it terminates at
    /// once, `applicationWillTerminate:` the only word aterm gets), which is why a person's quit,
    /// whose wait the person sets, is never a `Later`.
    Later(u64),
}

/// The reply to `decision`, a `terminate:` that `asker` asked for as the arbiter admitted it,
/// `answerable` saying whether a reply could come from AppKit's wait. A person's quit is taken up
/// with AppKit's ask cancelled, whether or not a reply could come, since nothing waits on it; a
/// person's second ask of the quit pending answers nothing the standing question does not and is
/// refused. The system's and Quit & Reopen's quit, new or sent onto the quit pending, is taken up
/// with AppKit waiting on its reply, where one could come. Where none could (AppKit asking from
/// inside aterm's own alert or panel, whose loop dispatched the ask), a Quit & Reopen is taken up
/// with AppKit's ask cancelled, and a logout, restart or shutdown is refused. Pure.
#[cfg(any(target_os = "macos", test))]
pub(crate) const fn terminate_reply(
    decision: NativeTerminateDecision,
    asker: crate::system_quit::Asker,
    answerable: bool,
) -> TerminateReply {
    use crate::system_quit::Asker;
    use NativeTerminateDecision::{AllowExit, DeferExisting, Dispatch};
    match (decision, asker) {
        (AllowExit(_), _) => TerminateReply::Now,
        (DeferExisting(_), Asker::Person) => TerminateReply::Cancel,
        (Dispatch(generation), Asker::Person) => TerminateReply::TakeUp(generation),
        (Dispatch(generation) | DeferExisting(generation), _) if answerable => {
            TerminateReply::Later(generation)
        }
        (Dispatch(generation) | DeferExisting(generation), Asker::Reopen) => {
            TerminateReply::TakeUp(generation)
        }
        (Dispatch(_) | DeferExisting(_), Asker::System) => TerminateReply::Cancel,
    }
}

/// What AppKit hears for `reply` once the window was offered the quit it takes up (`post`:
/// whether an event loop took its `Wake::NativeTerminateRequested`): a quit none takes (none yet,
/// or gone) is cancelled here and AppKit terminates now; a [`TerminateReply::Later`] taken AppKit
/// waits on ([`AWAITED`]) until a cancel refuses it or the loop's exit lets it go; a
/// [`TerminateReply::TakeUp`] taken AppKit hears cancelled and waits on nothing. Pure but for
/// `post`, the arbiter and this thread's record of what AppKit waits on.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) fn offer_terminate(
    reply: TerminateReply,
    post: impl FnOnce(u64) -> bool,
) -> TerminateReply {
    let (TerminateReply::TakeUp(generation) | TerminateReply::Later(generation)) = reply else {
        return reply;
    };
    if !post(generation) {
        let _ = cancel_native_termination(generation);
        return TerminateReply::Now;
    }
    if matches!(reply, TerminateReply::Later(_)) {
        AWAITED.set(Some(generation));
    }
    reply
}

/// Cancel the quit of `generation` if it is the one pending.
#[cfg(unix)]
pub(crate) fn cancel_native_termination(generation: u64) -> bool {
    let cancelled = with_native_terminate(|state| {
        let cancelled = state.cancel(generation);
        if cancelled {
            crate::system_quit::forget(generation);
        }
        cancelled
    });
    if cancelled {
        answer_cancelled(generation);
    }
    cancelled
}

/// Cancel whichever quit is pending.
pub(crate) fn cancel_current_native_termination() -> bool {
    let cancelled = with_native_terminate(|state| {
        let cancelled = state.cancel_current();
        if let Some(generation) = cancelled {
            crate::system_quit::forget(generation);
        }
        cancelled
    });
    if let Some(generation) = cancelled {
        answer_cancelled(generation);
    }
    cancelled.is_some()
}

/// The quit of `generation` was cancelled: when AppKit waits on it, it hears NO, once, outside
/// the arbiter's lock; a logout, restart, shutdown or Quit & Reopen under it is abandoned, as
/// macOS abandons one for any app that refuses it.
fn answer_cancelled(generation: u64) {
    if AWAITED.get() != Some(generation) {
        return;
    }
    AWAITED.set(None);
    aterm_log::info!(
        "quit {generation} will not happen: macOS is answered no, and a logout, restart, \
         shutdown or Quit & Reopen it stood for is cancelled"
    );
    #[cfg(test)]
    REFUSED.with_borrow_mut(|refused| refused.push(generation));
    #[cfg(target_os = "macos")]
    let _ = aterm_window::platform::macos::refuse_deferred_termination();
}

#[cfg(test)]
thread_local! {
    /// Each generation AppKit heard NO for on this thread, in order ([`answer_cancelled`]).
    static REFUSED: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The NOs AppKit heard on this thread since the last take.
#[cfg(test)]
pub(crate) fn take_refusals_for_test() -> Vec<u64> {
    REFUSED.take()
}

/// Serialises the tests that drive the process-wide arbiter and the system-quit mark.
#[cfg(test)]
pub(crate) fn terminate_test_serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    with_native_terminate(|state| *state = NativeTerminateArbiter::new());
    crate::system_quit::clear_for_test();
    AWAITED.set(None);
    let _ = take_refusals_for_test();
    guard
}

#[cfg(unix)]
pub(crate) fn complete_native_termination(generation: u64) -> bool {
    with_native_terminate(|state| state.complete(generation))
}

pub(crate) fn complete_current_native_termination() -> bool {
    with_native_terminate(NativeTerminateArbiter::complete_current)
}

/// One menu command, independent of AppKit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuAction {
    /// About aterm — open the dedicated native About tab.
    About,
    /// Check for Updates….
    SoftwareUpdate,
    /// Version — the top-level menu whose title shows `v<version>`.
    Version,
    /// One-click update (`App::apply_update_or_details`).
    ApplyUpdate,
    /// Open aterm.toml.
    Preferences,
    /// Quit aterm.
    Quit,
    /// New Window — a new in-process window of this App, sharing its sessions.
    NewWindow,
    /// New Tab — a new in-window session (`App::open_tab`).
    NewTab,
    /// Open one local UTF-8 document; Markdown starts in Read mode.
    OpenEditor,
    /// Save the active editor document through its existing file grant.
    Save,
    /// Reopen the most recently closed whole tab with fresh runtime identities.
    ReopenClosedTab,
    /// Reinsert the most recently closed split leaf at its original topology path.
    ReopenClosedView,
    /// Move Tab to New Window.
    MoveTabToNewWindow,
    /// Move Tab to Next Window.
    MoveTabToNextWindow,
    /// Open Session in New Window.
    ViewSessionInNewWindow,
    /// Close Tab — close the focused pane.
    CloseTab,
    /// Copy the selection (`App::copy_selection`).
    Copy,
    /// Paste the clipboard (`App::paste_clipboard`).
    Paste,
    /// Select All (`App::select_all`).
    SelectAll,
    /// Find… — search the active tab (`App::find_requested`).
    Find,
    /// Find Next — resume/step to the next search match.
    FindNext,
    /// Find Previous — resume/step to the previous match.
    FindPrev,
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
    /// (`App::toggle_matrix_rain`): a per-session override of `[matrix_rain]`, either way, until
    /// the session ends.
    ToggleMatrixRain,
    /// Promote the front window's promotable kitty.
    FavouriteKitty,
    /// Wear the next cat in the collection, one per press, wrapping at the end.
    NextKitty,
    /// Toggle the process-wide serious-mode policy.
    ToggleSeriousMode,
    /// Focus or create the process-singleton Settings tab.
    ToggleSettings,
    /// View ▸ Stats Bars: Deck.
    StatsBarsDeck,
    /// View ▸ Stats Bars: Sill.
    StatsBarsSill,
    /// View ▸ Stats Bars.
    StatsBarsOff,
    /// View ▸ Deck Size: Automatic.
    DeckSizeAuto,
    /// View ▸ Deck Size: Full.
    DeckSizeFull,
    /// View ▸ Deck Size: Folded.
    DeckSizeFolded,
    /// View ▸ Stats Lens (⌘I).
    ToggleStatsLens,
    /// Open the peer and shared-state close-up, including while resource bars are hidden.
    NetworkState,
    /// View ▸ Rewind Pane (⇧⌘R): freeze the focused pane on its recorded past, or return it to live.
    RewindPane,
    /// View ▸ Export Pane History…: the focused pane's kept history as an asciicast file a save panel names.
    ExportPaneHistory,
    /// View ▸ Forget Pane History: drop the focused pane's kept history (`App::forget_front_history`).
    ForgetPaneHistory,
    /// View ▸ Forget All History: drop every pane's kept history (`App::forget_all_history`).
    ForgetAllHistory,
    /// View ▸ Record Keys Not Echoed…: the person's own grant, in aterm's owner alert, for the
    /// front window's live recording to also keep the keys aterm cannot see echoed while the
    /// program in front of its tab stays there, or its stop (`App::toggle_unseen_keys`).
    RecordUnseenKeys,
    /// Open Settings at the Packages route.
    Packages,
    /// Open Settings at the Messages route.
    Messages,
    /// Aterm ▸ System State…: focus or create the System State tab in the front window.
    SystemState,
    /// Open the read-only Allocation pane beside the focused view, or in a tab when space is tight.
    Allocation,
    /// Open the native email client.
    Mail,
    /// Toggle the command palette overlay (`App::toggle_palette`).
    OpenPalette,
    /// View ▸ Choose Theme….
    ChooseTheme,
    /// View ▸ Browse & Customize Themes… and the palette Theme section's last row.
    BrowseThemes,
    /// View ▸ Choose Tab Theme….
    ChooseTabTheme,
    /// View ▸ Clear Tab Theme.
    ClearTabTheme,
    /// Copy the right-clicked tab's session id (the registry `sid`) to the clipboard.
    CopySessionId,
    /// Copy the right-clicked tab's shell-reported cwd to the clipboard, raw.
    CopyCwd,
    /// Overview… ⌃⌘O: every session of this aterm on one card.
    Overview,
    /// Workers…: the window's list of the workers it hosts.
    Workers,
    /// Next That Needs You ⇧⌘J.
    NextNeed,
    /// Harness Log for This Tab ⇧⌘L.
    HarnessLog,
    /// Take Over This Tab: the person's hold on the focused session.
    TakeOver,
    /// Hand Back This Tab: lift the person's hold, and only it (`App::hand_back`).
    HandBack,
    /// Hand Back All: lift the person's hold on every tab that has one
    /// (`App::hand_back_all`).
    HandBackAll,
    /// Read This Mac: `aterm ctl survey` as a child, its text in a document tab.
    ReadThisMac,
    /// Read the Fleet: `aterm ctl survey --fleet --timeout 3` into a document tab.
    ReadFleet,
    /// Summarize the Fleet in a New Tab (Summarize This Mac in a New Tab with no fleet).
    SummarizeFleet,
    /// Message Bus: Status.
    FabricStatus,
    /// Message Bus: Turn On….
    FabricOn,
    /// Message Bus: Turn Off….
    FabricOff,
    /// Fleet Machines — `aterm fleet peers` into a document tab.
    FleetMachines,
    /// Edit the focused pane's session pin in place on the tab strip.
    RenameSession,
    /// Set Role… — the same inline editor over the active tab.
    SetRole,
    /// Edit ▸ Reset Terminal (`crate::manual_reset`).
    ResetTerminal,
    /// View ▸ Coding Agent Harness….
    HarnessSettings,
    /// Minimise the window.
    Minimize,
    /// Zoom (toggle maximised) the window.
    Zoom,
    /// Show the next tab (`App::cycle_tab(true)`).
    NextTab,
    /// Show the previous tab (`App::cycle_tab(false)`).
    PrevTab,
    /// Help — open the bundled, offline features guide (`open_help_url`).
    Help,
    /// Change this Mac's number and nickname from the machine menu.
    RenameMachine,
}

impl MenuAction {
    /// The integer stored in the menu item's `tag`.
    #[must_use]
    pub(crate) const fn tag(self) -> isize {
        match self {
            MenuAction::About => 1,
            MenuAction::Preferences => 2,
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
            MenuAction::ToggleSettings => 30,
            MenuAction::OpenPalette => 31,
            MenuAction::SoftwareUpdate => 35,
            MenuAction::Version => 36,
            MenuAction::ApplyUpdate => 37,
            MenuAction::ReopenClosedTab => 38,
            MenuAction::OpenEditor => 40,
            MenuAction::ReopenClosedView => 41,
            MenuAction::CopySessionId => 42,
            MenuAction::CopyCwd => 43,
            MenuAction::ToggleMatrixRain => 44,
            MenuAction::ToggleSeriousMode => 45,
            MenuAction::FavouriteKitty => 46,
            MenuAction::Packages => 47,
            MenuAction::RenameSession => 48,
            MenuAction::Overview => 54,
            MenuAction::Workers => 99,
            MenuAction::Allocation => 100,
            MenuAction::NextKitty => 57,
            MenuAction::HarnessLog => 60,
            MenuAction::TakeOver => 61,
            MenuAction::HandBack => 62,
            MenuAction::HandBackAll => 104,
            MenuAction::FabricStatus => 63,
            MenuAction::FabricOn => 64,
            MenuAction::FabricOff => 65,
            MenuAction::SetRole => 66,
            MenuAction::Messages => 69,
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
            MenuAction::NetworkState => 95,
            MenuAction::RewindPane => 93,
            MenuAction::ExportPaneHistory => 94,
            MenuAction::ForgetPaneHistory => 96,
            MenuAction::ForgetAllHistory => 97,
            MenuAction::RecordUnseenKeys => 101,
            MenuAction::DeckSizeAuto => 88,
            MenuAction::DeckSizeFull => 89,
            MenuAction::DeckSizeFolded => 90,
            MenuAction::Save => 91,
            MenuAction::SystemState => 92,
            MenuAction::Mail => 98,
            MenuAction::ChooseTabTheme => 77,
            MenuAction::ClearTabTheme => 78,
            MenuAction::RenameMachine => 103,
        }
    }

    /// Inverse of [`MenuAction::tag`].
    #[must_use]
    #[cfg(any(target_os = "macos", test))]
    pub(crate) const fn from_tag(tag: isize) -> Option<MenuAction> {
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
            40 => MenuAction::OpenEditor,
            41 => MenuAction::ReopenClosedView,
            42 => MenuAction::CopySessionId,
            43 => MenuAction::CopyCwd,
            44 => MenuAction::ToggleMatrixRain,
            45 => MenuAction::ToggleSeriousMode,
            46 => MenuAction::FavouriteKitty,
            47 => MenuAction::Packages,
            48 => MenuAction::RenameSession,
            54 => MenuAction::Overview,
            99 => MenuAction::Workers,
            100 => MenuAction::Allocation,
            57 => MenuAction::NextKitty,
            60 => MenuAction::HarnessLog,
            61 => MenuAction::TakeOver,
            62 => MenuAction::HandBack,
            104 => MenuAction::HandBackAll,
            63 => MenuAction::FabricStatus,
            64 => MenuAction::FabricOn,
            65 => MenuAction::FabricOff,
            66 => MenuAction::SetRole,
            69 => MenuAction::Messages,
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
            95 => MenuAction::NetworkState,
            88 => MenuAction::DeckSizeAuto,
            89 => MenuAction::DeckSizeFull,
            90 => MenuAction::DeckSizeFolded,
            91 => MenuAction::Save,
            92 => MenuAction::SystemState,
            98 => MenuAction::Mail,
            93 => MenuAction::RewindPane,
            94 => MenuAction::ExportPaneHistory,
            96 => MenuAction::ForgetPaneHistory,
            97 => MenuAction::ForgetAllHistory,
            101 => MenuAction::RecordUnseenKeys,
            103 => MenuAction::RenameMachine,
            _ => return None,
        })
    }
}

/// Fold a legacy `invoke` spelling onto the current Debug token.
#[must_use]
pub(crate) fn canonical_invoke_name(name: &str) -> &str {
    match name {
        "HoldSession" => "TakeOver",
        "LiftHold" => "HandBack",
        "OpenLedger" | "LedgerForSession" => "HarnessLog",
        "ShowConnectionMap" | "Fleet" | "ShowFleet" => "Overview",
        other => other,
    }
}

/// What a retired `invoke` name answers instead (`ERR retired: <this>`).
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

        "TogglePresenceBand" | "TogglePresenceRim" | "ToggleNeedsInWindows" => {
            Some("automatic attention UI was removed")
        }
        "ShowIdentity" | "NewWindowWithIdentity" | "NewTabWithIdentity" => {
            Some("named session identities were removed")
        }
        _ => None,
    }
}

/// Commands that need a live terminal leaf.
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
            | MenuAction::HarnessLog
            | MenuAction::TakeOver
            | MenuAction::HandBack
            | MenuAction::SetRole
            | MenuAction::ResetTerminal
            | MenuAction::ChooseTabTheme
            | MenuAction::ClearTabTheme
            | MenuAction::RewindPane
            | MenuAction::ExportPaneHistory
            | MenuAction::ForgetPaneHistory
            | MenuAction::RecordUnseenKeys
    )
}

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

/// Declare a [`Published`] value.
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
    static ACTIVE_TAB_IS_TERMINAL: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(true);
}

pub(crate) fn set_active_tab_is_terminal(terminal: bool) {
    ACTIVE_TAB_IS_TERMINAL
        .published(|bit| bit.store(terminal, std::sync::atomic::Ordering::Relaxed));
}

menu_static! {
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
    static ANY_PERSON_HOLD: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
}

/// Publish whether Hand Back All has a hold to lift: the person holds a tab, the front one or
/// not.
pub(crate) fn set_any_person_hold(held: bool) {
    ANY_PERSON_HOLD.published(|bit| bit.store(held, std::sync::atomic::Ordering::Relaxed));
}

/// The published person holds ([`set_any_person_hold`]).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn any_person_hold() -> bool {
    ANY_PERSON_HOLD.published(|bit| bit.load(std::sync::atomic::Ordering::Relaxed))
}

menu_static! {
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

/// The published Stats Bars form ([`set_stats_bars_form`]).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn stats_bars_form() -> crate::stats_deck::StatsBars {
    let bits = STATS_BARS_FORM.published(|bit| bit.load(std::sync::atomic::Ordering::Relaxed));
    crate::stats_deck::StatsBars::ALL
        .get(usize::from(bits))
        .copied()
        .unwrap_or(crate::stats_deck::StatsBars::Deck)
}

menu_static! {
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

/// The published Deck Size choice ([`set_deck_size_pref`]).
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

/// The Agents menu's live bits, as `validateMenuItem:` needs them when the menu opens.
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
    /// Before the first publish.
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

/// The live tool tip a native row carries at the moment the menu opens, where it says live state.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn native_item_tip(action: MenuAction) -> Option<String> {
    match action {
        MenuAction::SummarizeFleet => agents_live()
            .summarize_block
            .map(|why| format!("{} {why}", action.help())),
        MenuAction::RewindPane | MenuAction::ExportPaneHistory | MenuAction::ForgetPaneHistory
            if !menu_live().can_rewind =>
        {
            Some(crate::rewind::REWIND_OFF_TIP.to_string())
        }
        _ => None,
    }
}

/// The full-screen row's title.
pub(crate) const fn full_screen_title(fullscreen: bool) -> &'static str {
    if fullscreen {
        "Exit Full Screen"
    } else {
        "Enter Full Screen"
    }
}

/// The ⌘W row's title: on a split tab ⌘W closes the focused pane (`App::close_active_tab`).
pub(crate) const fn close_row_title(split: bool) -> &'static str {
    if split { "Close Pane" } else { "Close Tab" }
}

/// The front window's live state that the palette's `resolve` reads from `PaletteLive`.
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
    /// The focused pane records its screen, so Rewind can open on it.
    pub can_rewind: bool,
    /// The front window's glow is recorded (`aos attach`), so Record Keys Not Echoed… can grant.
    pub aos_attached: bool,
    /// The front window's recording carries the person's grant (its checkmark).
    pub aos_unseen: bool,
}

#[cfg(any(target_os = "macos", test))]
impl MenuLive {
    /// Before the first publish.
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
        can_rewind: false,
        aos_attached: false,
        aos_unseen: false,
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
            | (self.can_rewind as u16) << 11
            | (self.aos_attached as u16) << 12
            | (self.aos_unseen as u16) << 13
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
            can_rewind: b & (1 << 11) != 0,
            aos_attached: b & (1 << 12) != 0,
            aos_unseen: b & (1 << 13) != 0,
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
#[cfg(any(target_os = "macos", test))]
pub(crate) fn native_menu_checked(action: MenuAction) -> Option<bool> {
    match action {
        MenuAction::StatsBarsDeck | MenuAction::StatsBarsSill | MenuAction::StatsBarsOff => {
            Some(action.stats_bars_form() == Some(stats_bars_form()))
        }
        MenuAction::DeckSizeAuto | MenuAction::DeckSizeFull | MenuAction::DeckSizeFolded => {
            Some(action.deck_size_pref() == Some(deck_size_pref()))
        }
        MenuAction::ToggleSeriousMode => Some(menu_live().serious_mode),
        MenuAction::ToggleMatrixRain => Some(menu_live().rain_on),
        MenuAction::FavouriteKitty => Some(menu_live().kitty_favourited),
        MenuAction::RecordUnseenKeys => Some(menu_live().aos_unseen),
        _ => None,
    }
}

/// The title a native row carries at the moment the menu opens, where it names live state.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn native_live_title(action: MenuAction) -> Option<&'static str> {
    match action {
        MenuAction::CloseTab => Some(close_row_title(menu_live().front_tab_split)),
        MenuAction::SummarizeFleet => Some(summarize_title(agents_live().in_fleet)),
        _ => None,
    }
}

/// The title `chrome` prints for a native row tagged `tag`.
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
    match action {
        MenuAction::TakeOver if front_person_hold() && !front_remote_hold() => return false,
        MenuAction::HandBack if !front_person_hold() => return false,
        MenuAction::HandBackAll if !any_person_hold() => return false,
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
    let live = menu_live();
    let precondition = match action {
        MenuAction::Copy => live.has_selection,
        MenuAction::Save => live.can_save,
        MenuAction::Find => live.can_find,
        MenuAction::FindNext | MenuAction::FindPrev => live.can_find_again,
        MenuAction::NextTab | MenuAction::PrevTab => live.multi_tab,
        MenuAction::ReopenClosedTab => live.can_reopen_closed_tab,
        MenuAction::ReopenClosedView => live.can_reopen_closed_view,
        MenuAction::RewindPane | MenuAction::ExportPaneHistory | MenuAction::ForgetPaneHistory => {
            live.can_rewind
        }
        MenuAction::RecordUnseenKeys => live.aos_attached,
        _ => true,
    };
    precondition
        && (!requires_terminal_tab(action)
            || ACTIVE_TAB_IS_TERMINAL
                .published(|bit| bit.load(std::sync::atomic::Ordering::Relaxed)))
}

/// Take Over's tool tip: what the person's hold does, and what any hold costs the whole instance.
#[cfg(any(unix, a11y_tree, test))]
pub(crate) const TAKE_OVER_TIP: &str = concat!(
    "Only you type here until you hand it back. aterm's harness, managers and scripts get ERR \
     halted; the agent keeps running its turn. ",
    aterm_types::hold_cost!()
);

/// The control authority a socket caller must hold to reach a [`MenuAction`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InvokeAuthority {
    /// Benign: the `invoke` verb's base `WriteInput` gate suffices.
    WriteInput,
    /// Rewrites durable `aterm.toml` on invoke or raises the durable-config surface.
    ConfigWrite,
    /// Moves text onto or off the OS pasteboard. The op admits the verb; the copy or paste is
    /// the person's own, so it refuses a socket caller's `invoke` (`App::refuse_clipboard_gesture`)
    /// unless a development build's person stand-in fired it (`control_media::person_invoke_act`).
    ClipboardWrite,
    /// Owner-only, which no single fine op expresses.
    OwnerOnly,
    /// The person's own: no socket scope fires it, the Owner token included.
    PersonOnly,
}

impl MenuAction {
    /// The [`InvokeAuthority`] a socket caller needs to fire this action via `invoke <action>`.
    pub(crate) const fn invoke_authority(self) -> InvokeAuthority {
        let _ = crate::command_registry::menu_command(self);
        use InvokeAuthority::{ClipboardWrite, ConfigWrite, OwnerOnly, PersonOnly, WriteInput};
        match self {
            MenuAction::Copy
            | MenuAction::Paste
            | MenuAction::SelectAll
            | MenuAction::CopySessionId
            | MenuAction::CopyCwd => ClipboardWrite,
            MenuAction::Preferences
            | MenuAction::ToggleSettings
            | MenuAction::Packages
            | MenuAction::HarnessSettings
            | MenuAction::BrowseThemes
            | MenuAction::Messages
            | MenuAction::ToggleSeriousMode => ConfigWrite,
            MenuAction::SystemState | MenuAction::Mail => ConfigWrite,
            MenuAction::OpenPalette | MenuAction::SoftwareUpdate | MenuAction::ApplyUpdate => {
                OwnerOnly
            }
            MenuAction::Overview
            | MenuAction::Workers
            | MenuAction::Allocation
            | MenuAction::HarnessLog
            | MenuAction::ReadThisMac
            | MenuAction::NetworkState
            | MenuAction::ReadFleet
            | MenuAction::SummarizeFleet
            | MenuAction::FleetMachines
            | MenuAction::FabricStatus
            | MenuAction::FabricOn
            | MenuAction::FabricOff => OwnerOnly,
            MenuAction::TakeOver | MenuAction::HandBack | MenuAction::HandBackAll => PersonOnly,
            MenuAction::StatsBarsDeck | MenuAction::StatsBarsSill | MenuAction::StatsBarsOff => {
                ConfigWrite
            }
            MenuAction::DeckSizeAuto | MenuAction::DeckSizeFull | MenuAction::DeckSizeFolded => {
                ConfigWrite
            }
            MenuAction::ToggleStatsLens
            | MenuAction::ExportPaneHistory
            | MenuAction::RecordUnseenKeys
            | MenuAction::RenameMachine => PersonOnly,
            MenuAction::ForgetAllHistory => OwnerOnly,
            MenuAction::ChooseTheme => ConfigWrite,
            MenuAction::About
            | MenuAction::Version
            | MenuAction::Quit
            | MenuAction::NewWindow
            | MenuAction::NewTab
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
            | MenuAction::ToggleMatrixRain
            | MenuAction::FavouriteKitty
            | MenuAction::NextKitty
            | MenuAction::RenameSession
            | MenuAction::SetRole
            | MenuAction::NextNeed
            | MenuAction::ResetTerminal
            | MenuAction::ChooseTabTheme
            | MenuAction::ClearTabTheme
            | MenuAction::RewindPane
            | MenuAction::ForgetPaneHistory
            | MenuAction::Minimize
            | MenuAction::Zoom
            | MenuAction::NextTab
            | MenuAction::PrevTab
            | MenuAction::Help => WriteInput,
        }
    }

    /// Whether firing this action writes input bytes to a session's PTY.
    #[must_use]
    pub(crate) const fn writes_pty_input(self) -> bool {
        match self {
            MenuAction::Paste => true,
            MenuAction::StatsBarsDeck
            | MenuAction::StatsBarsSill
            | MenuAction::StatsBarsOff
            | MenuAction::DeckSizeAuto
            | MenuAction::DeckSizeFull
            | MenuAction::DeckSizeFolded
            | MenuAction::ToggleStatsLens
            | MenuAction::NetworkState
            | MenuAction::RewindPane
            | MenuAction::ExportPaneHistory
            | MenuAction::ForgetPaneHistory
            | MenuAction::ForgetAllHistory
            | MenuAction::RecordUnseenKeys
            | MenuAction::RenameMachine => false,
            MenuAction::About
            | MenuAction::SoftwareUpdate
            | MenuAction::Version
            | MenuAction::ApplyUpdate
            | MenuAction::Preferences
            | MenuAction::Quit
            | MenuAction::NewWindow
            | MenuAction::NewTab
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
            | MenuAction::Packages
            | MenuAction::HarnessSettings
            | MenuAction::Messages
            | MenuAction::SystemState
            | MenuAction::Allocation
            | MenuAction::Mail
            | MenuAction::OpenPalette
            | MenuAction::ChooseTheme
            | MenuAction::BrowseThemes
            | MenuAction::CopySessionId
            | MenuAction::CopyCwd
            | MenuAction::Overview
            | MenuAction::Workers
            | MenuAction::NextNeed
            | MenuAction::HarnessLog
            | MenuAction::TakeOver
            | MenuAction::HandBack
            | MenuAction::HandBackAll
            | MenuAction::ReadThisMac
            | MenuAction::ReadFleet
            | MenuAction::SummarizeFleet
            | MenuAction::FleetMachines
            | MenuAction::FabricStatus
            | MenuAction::FabricOn
            | MenuAction::FabricOff
            | MenuAction::RenameSession
            | MenuAction::SetRole
            | MenuAction::ResetTerminal
            | MenuAction::ChooseTabTheme
            | MenuAction::ClearTabTheme
            | MenuAction::Minimize
            | MenuAction::Zoom
            | MenuAction::NextTab
            | MenuAction::PrevTab
            | MenuAction::Help => false,
        }
    }

    /// The row's help: one sentence.
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
            MenuAction::OpenEditor => "Open a local file. Markdown starts in Read mode.",
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
            MenuAction::SystemState => {
                "Open System State: this Mac's setup and secrets vault, and what each step needs."
            }
            MenuAction::Allocation => "Show this computer's CPU lease and resource observations.",
            MenuAction::Mail => "Open Mail: accounts, folders, messages and drafts.",
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
            MenuAction::Workers => {
                "Show the workers this window hosts: what each is doing, how each ended, and its \
                 result."
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
            MenuAction::HandBackAll => {
                "Hand back every tab you took over: your holds lift; any other hold stays."
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
                "Show each window's stats deck Folded, however tall the window is; saved in \
                 aterm.toml."
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
            MenuAction::NetworkState => {
                "Show known aterm machines, observed reachability and shared account and secret access."
            }
            MenuAction::RewindPane => {
                "Show this pane's recent past, read-only: Left and Right step through it, Esc \
                 returns to live."
            }
            MenuAction::ExportPaneHistory => {
                "Save this pane's kept screen history as an asciicast file, where you choose."
            }
            MenuAction::ForgetPaneHistory => {
                "Drop this pane's kept screen history; it keeps recording from now."
            }
            MenuAction::ForgetAllHistory => {
                "Drop the kept screen history of every pane; each keeps recording from now."
            }
            MenuAction::RecordUnseenKeys => {
                "Keep this window's recording going through keys aterm cannot see echoed, such as \
                 vim's normal mode, while the program in front of this tab stays there. aterm \
                 asks first, and marks each such key in the recording."
            }
            MenuAction::Help => "Open the aterm guide.",
            MenuAction::RenameMachine => "Change this Mac's number and nickname.",
        }
    }

    /// Whether this platform can do what the row offers.
    #[must_use]
    pub(crate) const fn offered_on_this_platform(self) -> bool {
        match self {
            MenuAction::SoftwareUpdate => aterm_update::enabled(),
            // Windows has no checker to open, and still installs a build staged on the machine:
            // the row shows only while one is staged (`menu_chrome_lines`' `apply_row`).
            MenuAction::ApplyUpdate => {
                aterm_update::enabled() || aterm_update::staged_notice_enabled()
            }
            MenuAction::SystemState | MenuAction::RecordUnseenKeys | MenuAction::RenameMachine => {
                cfg!(target_os = "macos")
            }
            _ => true,
        }
    }

    /// Recover a [`MenuAction`] from the `{:?}` Debug token carried on the wire by `invoke <Name>`.
    #[must_use]
    pub(crate) fn from_invoke_name(name: &str) -> Option<MenuAction> {
        match canonical_invoke_name(name) {
            "About" => Some(MenuAction::About),
            "SoftwareUpdate" => Some(MenuAction::SoftwareUpdate),
            "Version" => Some(MenuAction::Version),
            "RenameMachine" => Some(MenuAction::RenameMachine),
            "ApplyUpdate" => Some(MenuAction::ApplyUpdate),
            "Preferences" => Some(MenuAction::Preferences),
            "Quit" => Some(MenuAction::Quit),
            "NewWindow" => Some(MenuAction::NewWindow),
            "NewTab" => Some(MenuAction::NewTab),
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
            "SystemState" => Some(MenuAction::SystemState),
            "Allocation" => Some(MenuAction::Allocation),
            "Mail" => Some(MenuAction::Mail),
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
            "Workers" => Some(MenuAction::Workers),
            "NextNeed" => Some(MenuAction::NextNeed),
            "HarnessLog" => Some(MenuAction::HarnessLog),
            "ReadThisMac" => Some(MenuAction::ReadThisMac),
            "ReadFleet" => Some(MenuAction::ReadFleet),
            "SummarizeFleet" => Some(MenuAction::SummarizeFleet),
            "FleetMachines" => Some(MenuAction::FleetMachines),
            "TakeOver" => Some(MenuAction::TakeOver),
            "HandBack" => Some(MenuAction::HandBack),
            "HandBackAll" => Some(MenuAction::HandBackAll),
            "FabricStatus" => Some(MenuAction::FabricStatus),
            "FabricOn" => Some(MenuAction::FabricOn),
            "FabricOff" => Some(MenuAction::FabricOff),
            "SetRole" => Some(MenuAction::SetRole),
            "StatsBarsDeck" => Some(MenuAction::StatsBarsDeck),
            "StatsBarsSill" => Some(MenuAction::StatsBarsSill),
            "StatsBarsOff" => Some(MenuAction::StatsBarsOff),
            "DeckSizeAuto" => Some(MenuAction::DeckSizeAuto),
            "DeckSizeFull" => Some(MenuAction::DeckSizeFull),
            "DeckSizeFolded" => Some(MenuAction::DeckSizeFolded),
            "ToggleStatsLens" => Some(MenuAction::ToggleStatsLens),
            "NetworkState" => Some(MenuAction::NetworkState),
            "RewindPane" => Some(MenuAction::RewindPane),
            "ExportPaneHistory" => Some(MenuAction::ExportPaneHistory),
            "ForgetPaneHistory" => Some(MenuAction::ForgetPaneHistory),
            "ForgetAllHistory" => Some(MenuAction::ForgetAllHistory),
            "RecordUnseenKeys" => Some(MenuAction::RecordUnseenKeys),
            "ResetTerminal" => Some(MenuAction::ResetTerminal),
            "ChooseTabTheme" => Some(MenuAction::ChooseTabTheme),
            "ClearTabTheme" => Some(MenuAction::ClearTabTheme),
            _ => None,
        }
    }
}

/// Modifier mask of a menu item's visual key-equivalent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuMods {
    /// No modifier (or no key-equivalent at all when `key` is empty).
    None,
    Command,
    CommandShift,
    CommandControl,
}

/// One entry of a menu section.
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
    /// This entry's item, if it is one.
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
    Item {
        label: "Settings…",
        action: MenuAction::ToggleSettings,
        key: ",",
        mods: MenuMods::Command,
    },
    Item {
        label: "System State…",
        action: MenuAction::SystemState,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Allocation…",
        action: MenuAction::Allocation,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Mail…",
        action: MenuAction::Mail,
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

const QUIT_LABEL: &str = if cfg!(windows) { "Exit" } else { "Quit aterm" };

const ZOOM_LABEL: &str = zoom_title(false);

/// Window ▸ Zoom's title for a window that is (or is not) `maximized`.
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
    Item {
        label: "Workers…",
        action: MenuAction::Workers,
        key: "",
        mods: MenuMods::None,
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
    Item {
        label: "Hand Back All",
        action: MenuAction::HandBackAll,
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

/// Commands with no bar item, grouped under the menu whose matter they are.
pub(crate) const PALETTE_ONLY: &[(&str, &str, MenuAction)] = &[
    ("aterm", "Check for Updates…", MenuAction::SoftwareUpdate),
    ("aterm", "Packages…", MenuAction::Packages),
    ("aterm", "Messages…", MenuAction::Messages),
    ("aterm", "Open aterm.toml", MenuAction::Preferences),
    ("Agents", "Message Bus: Status", MenuAction::FabricStatus),
    ("Agents", "Message Bus: Turn On…", MenuAction::FabricOn),
    ("Agents", "Message Bus: Turn Off…", MenuAction::FabricOff),
    ("Agents", "Fleet Machines", MenuAction::FleetMachines),
];

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
    Separator,
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
    Item {
        label: "Network State",
        action: MenuAction::NetworkState,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    Item {
        label: "Rewind Pane",
        action: MenuAction::RewindPane,
        key: "r",
        mods: MenuMods::CommandShift,
    },
    Item {
        label: "Export Pane History\u{2026}",
        action: MenuAction::ExportPaneHistory,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Forget Pane History",
        action: MenuAction::ForgetPaneHistory,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Forget All History",
        action: MenuAction::ForgetAllHistory,
        key: "",
        mods: MenuMods::None,
    },
    Item {
        label: "Record Keys Not Echoed\u{2026}",
        action: MenuAction::RecordUnseenKeys,
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
        label: "Coding Agent Harness…",
        action: MenuAction::HarnessSettings,
        key: "",
        mods: MenuMods::None,
    },
    Separator,
    Item {
        label: "Choose Theme…",
        action: MenuAction::ChooseTheme,
        key: "",
        mods: MenuMods::None,
    },
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

/// The Version menu's live menu-bar title.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub(crate) fn version_menu_bar_title(attention: bool) -> String {
    let facts = VersionFacts::this_build();
    let wrench = if facts.release {
        ""
    } else {
        "\u{1F6E0}\u{FE0F} "
    };
    let signature = version_signature(facts, VersionRung::Full).unwrap_or_default();
    let arrow = if attention { " \u{2B06}\u{FE0F}" } else { "" };
    format!("{wrench}{signature}{arrow}")
}

/// How much of the version signature a surface has room for ([`version_signature`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub(crate) enum VersionRung {
    /// `v<version>` on a release.
    Full,
    /// A dev build without its hash.
    Short,
}

/// The compile-time facts a version signature is composed from, stamped by `build.rs` and read
/// from [`crate::build_info`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct VersionFacts<'a> {
    /// A release lane produced the binary ([`crate::build_info::is_release_build`]).
    pub(crate) release: bool,
    /// The display version, `MAJOR.MINOR.0` ([`crate::build_info::version_display`]).
    pub(crate) version: &'a str,
    /// Commits since the newest release tag ([`crate::build_info::dev_commits`]).
    pub(crate) dev_commits: &'a str,
    /// The short commit, `-dirty` suffixed for a dirty tree ([`crate::build_info::git_commit`]).
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

/// The version signature, without any glyph a surface adds.
#[must_use]
pub(crate) fn version_signature(facts: VersionFacts<'_>, rung: VersionRung) -> Option<String> {
    if facts.release {
        let majmin = facts.version.strip_suffix(".0").unwrap_or(facts.version);
        return match rung {
            VersionRung::Full => Some(format!("v{majmin}")),
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

/// The text of Windows' tab-strip version label (`tab_bar::TabHit::Version`).
#[must_use]
pub(crate) fn strip_version_label(rung: VersionRung, attention: bool) -> Option<String> {
    let text = strip_version_text(VersionFacts::this_build(), rung)?;
    Some(if attention {
        format!("{text} \u{2191}")
    } else {
        text
    })
}

/// The strip label's quiet form of the version.
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

/// What a click on the tab-strip version label fires.
#[must_use]
pub(crate) fn version_slot_click(attention: bool) -> MenuAction {
    if attention && MenuAction::SoftwareUpdate.offered_on_this_platform() {
        MenuAction::SoftwareUpdate
    } else {
        MenuAction::Version
    }
}

/// The apply-now label for a staged build, given the arrow the calling surface can render.
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

/// The post-update row's words, the Version menu's and the palette's alike.
#[must_use]
pub(crate) fn realized_row_label(arrow: &str, version: &str) -> String {
    format!("{arrow} Updated to aterm v{version}")
}

/// Whether the menu-bar Version arrow shows.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub(crate) const fn bar_title_attention(
    staged_present: bool,
    on_hold: bool,
    _realized: bool,
) -> bool {
    staged_present && !on_hold
}

/// The staged update the Version menu may offer, given the running build.
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

/// The whole menu bar, declaratively.
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
    MenuSection {
        title: "Version",
        entries: VERSION_MENU,
    },
    MenuSection {
        title: MACHINE_SECTION,
        entries: MACHINE_MENU,
    },
];

/// The machine menu's title in [`MENU_MODEL`]; the bar shows the machine's own title instead
/// (`m8 Heron`, [`crate::machine_title`]).
pub(crate) const MACHINE_SECTION: &str = "Machine";

/// The machine menu's actions, below its information row (§5.2).
const MACHINE_MENU: &[MenuEntry] = &[Item {
    label: "Rename This Mac\u{2026}",
    action: MenuAction::RenameMachine,
    key: "",
    mods: MenuMods::None,
}];

/// Serialise [`MENU_MODEL`] to the `chrome` verb's menu lines, leaving out a section this
/// platform offers none of (the machine menu off macOS).
#[cfg(any(not(target_os = "macos"), test))]
pub(crate) fn menu_chrome_lines(apply_row: Option<&str>, front_tab_split: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(MENU_MODEL.len() + 1);
    for section in MENU_MODEL.iter().filter(|s| offers_any(s.entries)) {
        out.extend(chrome_lines_for(
            section.title,
            section.entries,
            apply_row,
            front_tab_split,
        ));
    }
    out
}

/// Whether this platform offers any row of `entries`.
#[cfg(any(not(target_os = "macos"), test))]
fn offers_any(entries: &[MenuEntry]) -> bool {
    entries
        .iter()
        .flat_map(MenuEntry::items)
        .any(|(_, action, _, _)| action.offered_on_this_platform())
}

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
            MenuEntry::Item { action, .. } if !action.offered_on_this_platform() => {}
            MenuEntry::Item {
                action: MenuAction::ApplyUpdate,
                ..
            } => labels.extend(apply_row.map(str::to_string)),
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
    MenuHandle, choose_local_file, choose_local_image, choose_save_path, confirm, confirm_owner,
    defer_quit_for_terminate, install, launch_error, open_file_in_workspace, open_help_url,
    order_front_regardless, reveal_file_in_workspace, show_handoff_target_version,
    update_machine_menu, update_version_menu,
};

static PROCESS_HEADLESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that this process is headless.
pub(crate) fn mark_process_headless() {
    PROCESS_HEADLESS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether this process is headless, for a presenter that refuses quietly.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
pub(crate) fn process_is_headless() -> bool {
    PROCESS_HEADLESS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether a helper about to present OS UI (a modal, or an open that fronts another app).
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

/// Off macOS there is no NSWorkspace, and "Open Log" spawns nothing in its place.
#[cfg(not(target_os = "macos"))]
pub(crate) fn open_file_in_workspace(_path: &std::path::Path) -> bool {
    false
}

/// Off macOS there is no Finder to select a file.
#[cfg(not(target_os = "macos"))]
pub(crate) fn reveal_file_in_workspace(_path: &std::path::Path) -> bool {
    false
}

/// Non-macOS no-op handle.
#[cfg(not(target_os = "macos"))]
pub(crate) type MenuHandle = ();

/// Non-macOS stub: there is no platform menu bar, so nothing is installed (`None`).
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
#[cfg(not(target_os = "macos"))]
pub(crate) fn update_version_menu(
    _handle: &MenuHandle,
    _staged: Option<(u64, &str)>,
    _trouble: Option<&ApplyTrouble>,
    _on_hold: bool,
    _realized: bool,
) {
}

/// Windows: the shell's own Common Item Dialog (`IFileOpenDialog`).
#[cfg(windows)]
pub(crate) fn choose_local_file(title: &str, prompt: &str) -> Option<std::path::PathBuf> {
    crate::file_picker_win::choose(title, prompt)
}

/// Windows: [`choose_local_file`] offering only image files.
#[cfg(windows)]
pub(crate) fn choose_local_image(title: &str, prompt: &str) -> Option<std::path::PathBuf> {
    crate::file_picker_win::choose_image(title, prompt)
}

/// No save panel off macOS: nothing is written.
#[cfg(not(target_os = "macos"))]
pub(crate) fn choose_save_path(
    _title: &str,
    _prompt: &str,
    _name: &str,
) -> Option<std::path::PathBuf> {
    None
}

/// No native picker is linked on the remaining targets (Linux).
#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn choose_local_file(_title: &str, _prompt: &str) -> Option<std::path::PathBuf> {
    None
}

/// No picker here.
#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn choose_local_image(_title: &str, _prompt: &str) -> Option<std::path::PathBuf> {
    None
}

/// Whether [`choose_local_file`] can actually produce a path here.
#[cfg(target_os = "macos")]
pub(crate) const fn local_file_picker_available() -> bool {
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

/// Record an operational error without opening a modal or desktop notification: aterm.log keeps
/// its title (aterm's own words) and its body's length, never the body, which can name the
/// person's file (DESIGN-session-log §3.6; PRIV4-3: an alert's text until 1a7cad3eb5 made it a
/// log line). The body keeps its home: each caller writes it to stderr whole.
pub(crate) fn notify(title: &'static str, body: &str) {
    aterm_log::warn!("{title}: {}", aterm_log::words::Len(body));
}

/// Help, off macOS: the project page in the default browser.
#[cfg(not(target_os = "macos"))]
pub(crate) fn open_help_url() {
    crate::app_mouse::open_url_external(&help_url());
}

fn help_url() -> String {
    format!(
        "https://github.com/{}/{}",
        aterm_update_core::DEFAULT_OWNER,
        aterm_update_core::DEFAULT_REPO
    )
}

/// Which section of Privacy & Security a deep link aims.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PrivacyPane {
    /// Full Disk Access.
    FullDiskAccess,
    /// The per-app Files & Folders list.
    FilesAndFolders,
    /// The Developer Tools list (`kTCCServiceDeveloperTool`).
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

/// Full Disk Access, through the modern Settings extension.
#[cfg(any(target_os = "macos", test))]
pub(crate) const SETTINGS_FULL_DISK_ACCESS: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles";

#[cfg(any(target_os = "macos", test))]
const SETTINGS_FULL_DISK_ACCESS_LEGACY: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// The per-app Files & Folders list, through the modern Settings extension.
#[cfg(any(target_os = "macos", test))]
pub(crate) const SETTINGS_FILES_AND_FOLDERS: &str = "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_FilesAndFolders";

#[cfg(any(target_os = "macos", test))]
const SETTINGS_FILES_AND_FOLDERS_LEGACY: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders";

/// The Developer Tools list, through the modern Settings extension (the consent module's spelling).
#[cfg(any(target_os = "macos", test))]
pub(crate) const SETTINGS_DEVELOPER_TOOLS: &str = aterm_containment::consent::DEVTOOLS_SETTINGS_URL;

#[cfg(any(target_os = "macos", test))]
const SETTINGS_DEVELOPER_TOOLS_LEGACY: &str =
    aterm_containment::consent::DEVTOOLS_SETTINGS_URL_LEGACY;

#[cfg(any(target_os = "macos", test))]
const SETTINGS_PRIVACY_ROOT: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension";

/// Every URL that may be tried for `pane`, best first.
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

/// What [`macos::open_privacy_settings`] achieved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SettingsOpen {
    /// An anchored URL was accepted.
    Anchored,
    /// Both anchors were refused.
    #[cfg(target_os = "macos")]
    PaneRoot,
    /// Nothing opened — off the main thread, off macOS, or the shell refused every candidate.
    Refused,
}

impl SettingsOpen {
    /// Whether System Settings opened at all (anchored or at the pane root).
    pub(crate) const fn opened(self) -> bool {
        !matches!(self, Self::Refused)
    }
}

/// Open System Settings at `pane`.
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
    use aterm_macos_ui::appkit::class;
    use aterm_objc::{BlockPtr, Id, Obj, RcBlock, Retained, Sel, autoreleasepool, sel};
    use aterm_window::event_loop::EventLoopProxy;

    use crate::appkit::consts::{
        NS_ALERT_FIRST_BUTTON_RETURN, NS_ALERT_SECOND_BUTTON_RETURN,
        NS_EVENT_MODIFIER_FLAG_COMMAND, NS_EVENT_MODIFIER_FLAG_CONTROL,
        NS_EVENT_MODIFIER_FLAG_SHIFT, NS_MODAL_RESPONSE_OK,
    };
    use crate::appkit::{self, MainThread};

    use super::{
        MENU_MODEL, MenuAction, PrivacyPane, SettingsOpen, TerminateReply, privacy_settings_urls,
    };
    use crate::Wake;

    static TERMINATE_PROXY: std::sync::OnceLock<EventLoopProxy<Wake>> = std::sync::OnceLock::new();

    /// What [`install`] returns.
    pub(crate) struct MenuHandle {
        /// The single `menuAction:` relay target every item is wired to.
        target: Retained<MenuTarget>,
        /// The Version menu's top-level bar item (title `v<version>[ ⬆️]`).
        version_item: Obj,
        /// The machine's top-level bar item right of Version (title `m8 Heron`), hidden until
        /// its reader's first read ([`update_machine_menu`]).
        machine_item: Obj,
    }

    aterm_objc::declare_class! {
        /// The target of every menu item.
        pub(crate) struct MenuTarget: NSObject {
            const NAME: &str = "ATermMenuTarget";
            type Ivars = EventLoopProxy<Wake>;

            /// `menuAction:`, wired to every item.
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

            /// AppKit asks this just before displaying or dispatching an item.
            @sel(validateMenuItem:)
            fn validate_menu_item(&self, sender: Id) -> aterm_objc::Bool {
                if sender.is_null() {
                    return aterm_objc::Bool::NO;
                }
                // SAFETY: AppKit supplied a live NSMenuItem; reading its integer tag has no side
                // effects.
                let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                let Some(action) = MenuAction::from_tag(tag) else {
                    return aterm_objc::Bool::NO;
                };
                if let Some(on) = super::native_menu_checked(action) {
                    // SAFETY: a plain setter on the live NSMenuItem AppKit
                    // passed, main thread, no preconditions.
                    unsafe { appkit::send_v_isize(sender, sel!(setState:), isize::from(on)) };
                }
                if matches!(action, MenuAction::ToggleFullScreen) {
                    stamp_full_screen_title(sender);
                }
                if let Some(title) = super::native_live_title(action) {
                    set_item_title(sender, title);
                }
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

    fn stamp_full_screen_title(item: Id) {
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

    fn set_item_title(item: Id, title: &str) {
        if let Some(title) = appkit::nsstring(title) {
            // SAFETY: `-setTitle:` is `-(void)(NSString *)` on the live
            // NSMenuItem the caller holds; it COPIES its argument.
            unsafe { appkit::send_v_id(item, sel!(setTitle:), title.id()) };
        }
    }

    /// Build aterm's menu bar and install it as the shared application's main menu.
    pub(crate) fn install(proxy: &EventLoopProxy<Wake>) -> Option<MenuHandle> {
        let _windowed = crate::platform::Windowed::get()?;
        let main_thread = MainThread::new()?;
        let _ = TERMINATE_PROXY.set(proxy.clone());
        let target = MenuTarget::alloc_init(main_thread, proxy.clone())?;

        let main = new_menu()?;

        for section in MENU_MODEL {
            // Version and the machine menu are attached after the rest, with live titles.
            if section.title == "Version" || section.title == super::MACHINE_SECTION {
                continue;
            }
            let _ = attach_submenu(
                &main,
                section.title,
                build_section(&target, section.entries)?,
            );
        }
        let version_item = attach_submenu(
            &main,
            &super::version_menu_bar_title(false),
            build_version_menu(&target, None, None, false, false)?,
        )?;
        let machine_item = attach_submenu(&main, "", new_menu()?)?;
        // SAFETY: `-setHidden:` is `-(void)(BOOL)`, a plain main-thread setter on the live
        // NSMenuItem just attached.
        unsafe { appkit::send_v_bool(machine_item.id(), sel!(setHidden:), true) };

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
            machine_item,
        })
    }

    /// Title the machine's bar item `view`'s title (`m8 Heron`, §5.1 of
    /// `docs/DESIGN-machine-names-2026-11-04.md`) over a menu whose first row says what it is
    /// (`m8 · Heron · in a fleet`), above the machine section's actions, and show it.
    pub(crate) fn update_machine_menu(
        handle: &MenuHandle,
        view: &crate::machine_title::MachineView,
    ) {
        let Some(_main_thread) = MainThread::new() else {
            return;
        };
        let title = crate::machine_title::machine_menu_bar_title(view);
        let (Some(ns_title), Some(menu)) = (appkit::nsstring(&title), new_menu()) else {
            return;
        };
        let Some(row) = new_item(
            &crate::machine_title::machine_menu_info_row(view),
            Sel::NULL,
            "",
        ) else {
            return;
        };
        // SAFETY: `-addItem:` is `-(void)(NSMenuItem *)` and retains; `-setTitle:` is
        // `-(void)(NSString *)` and copies; `-setSubmenu:` is `-(void)(NSMenu *)` and retains;
        // `-setHidden:` is `-(void)(BOOL)`: plain main-thread setters on live objects, so the
        // +1 row, title and menu may drop at the end of this frame. A row with no action is
        // disabled under the menu's default autoenabling, so it reads as information.
        unsafe { appkit::send_v_id(menu.id(), sel!(addItem:), row.id()) };
        add_separator(&menu);
        if fill_section(&menu, &handle.target, super::MACHINE_MENU).is_none() {
            return;
        }
        // SAFETY: as above.
        unsafe {
            appkit::send_v_id(menu.id(), sel!(setTitle:), ns_title.id());
            appkit::send_v_id(handle.machine_item.id(), sel!(setTitle:), ns_title.id());
            appkit::send_v_id(handle.machine_item.id(), sel!(setSubmenu:), menu.id());
            appkit::send_v_bool(handle.machine_item.id(), sel!(setHidden:), false);
        }
    }

    /// Re-sync the live Version menu (title and items) to the update state.
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

    /// The outgoing side of a seamless handoff.
    pub fn show_handoff_target_version(handle: &MenuHandle, title: &str) {
        let Some(_main_thread) = MainThread::new() else {
            return;
        };
        let Some(submenu) = build_version_menu(&handle.target, None, None, false, false) else {
            return;
        };
        set_version_menu(&_main_thread, handle, title, &submenu);
    }

    fn set_version_menu(
        _main_thread: &MainThread,
        handle: &MenuHandle,
        title: &str,
        submenu: &Obj,
    ) {
        let Some(ns_title) = appkit::nsstring(title) else {
            return;
        };
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

    fn build_section(target: &MenuTarget, entries: &[super::MenuEntry]) -> Option<Obj> {
        let menu = new_menu()?;
        fill_section(&menu, target, entries)?;
        Some(menu)
    }

    /// Append `entries` to `menu`, as [`build_section`] builds a section.
    fn fill_section(menu: &Obj, target: &MenuTarget, entries: &[super::MenuEntry]) -> Option<()> {
        for entry in entries {
            match entry {
                super::MenuEntry::Item {
                    label,
                    action,
                    key,
                    mods,
                } => add_item_mods(menu, target, label, *action, key, mods_mask(*mods)),
                super::MenuEntry::Submenu { label, entries } => {
                    attach_submenu(menu, label, build_section(target, entries)?)?;
                }
                super::MenuEntry::Separator => add_separator(menu),
            }
        }
        Some(())
    }

    fn mods_mask(mods: super::MenuMods) -> usize {
        match mods {
            super::MenuMods::None => 0,
            super::MenuMods::Command => command_mask(),
            super::MenuMods::CommandShift => command_shift_mask(),
            super::MenuMods::CommandControl => command_control_mask(),
        }
    }

    const fn command_mask() -> usize {
        NS_EVENT_MODIFIER_FLAG_COMMAND
    }

    const fn command_control_mask() -> usize {
        NS_EVENT_MODIFIER_FLAG_COMMAND | NS_EVENT_MODIFIER_FLAG_CONTROL
    }

    const fn command_shift_mask() -> usize {
        NS_EVENT_MODIFIER_FLAG_COMMAND | NS_EVENT_MODIFIER_FLAG_SHIFT
    }

    fn new_menu() -> Option<Obj> {
        // SAFETY: `+alloc` gives a +1 uninitialised NSMenu and `-init` consumes
        // it, so `Obj::from_owned` adopts exactly one +1.
        unsafe { Obj::from_owned(appkit::send_id(appkit::alloc(class(c"NSMenu")), sel!(init))) }
    }

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

    /// Present the system open panel and return exactly the one local path the user approved.
    pub(crate) fn choose_local_file(title: &str, prompt: &str) -> Option<std::path::PathBuf> {
        open_panel(title, prompt, &[])
    }

    /// [`choose_local_file`] offering only images.
    pub(crate) fn choose_local_image(title: &str, prompt: &str) -> Option<std::path::PathBuf> {
        open_panel(title, prompt, &["public.image"])
    }

    /// A save panel titled `title` offering `name`: the path the person chose, `None` when they cancelled.
    pub(crate) fn choose_save_path(
        title: &str,
        prompt: &str,
        name: &str,
    ) -> Option<std::path::PathBuf> {
        if super::os_ui_refused(title) {
            return None;
        }
        let _main_thread = MainThread::new()?;
        let _park = crate::watchdog::park_modal();
        let title = appkit::nsstring(title)?;
        let prompt = appkit::nsstring(prompt)?;
        let name = appkit::nsstring(name)?;
        // SAFETY: NSSavePanel is created and run on AppKit's main thread (`_main_thread` is the
        // witness). `+savePanel` is `-(id)` and returns an AUTORELEASED panel, borrowed for the
        // length of this pool. The setters are `-(void)(BOOL)` and `-(void)(NSString *)`;
        // `-runModal` is `-(NSModalResponse)` and owns its nested modal loop; `-URL` and `-path`
        // are `-(id)` accessors valid until the pool pops.
        autoreleasepool(|_| unsafe {
            let panel = appkit::send_id(class(c"NSSavePanel").as_id(), sel!(savePanel));
            if panel.is_null() {
                return None;
            }
            appkit::send_v_bool(panel, sel!(setCanCreateDirectories:), true);
            appkit::send_v_id(panel, sel!(setTitle:), title.id());
            appkit::send_v_id(panel, sel!(setPrompt:), prompt.id());
            appkit::send_v_id(panel, sel!(setNameFieldStringValue:), name.id());
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

    fn open_panel(title: &str, prompt: &str, types: &[&str]) -> Option<std::path::PathBuf> {
        if super::os_ui_refused(title) {
            return None;
        }
        let _main_thread = MainThread::new()?;
        let _park = crate::watchdog::park_modal();
        let title = appkit::nsstring(title)?;
        let prompt = appkit::nsstring(prompt)?;
        let types = types
            .iter()
            .map(|kind| appkit::nsstring(kind))
            .collect::<Option<Vec<Obj>>>()?;
        let type_ids = types.iter().map(Obj::id).collect::<Vec<Id>>();
        // SAFETY: NSOpenPanel is created and run on AppKit's main thread (`_main_thread` is the
        // witness). `+openPanel` is `-(id)` and returns an AUTORELEASED panel — borrowed for the
        // length of this pool, which is why the whole body is inside one. The setters are
        // `-(void)(BOOL)` and `-(void)(NSString *)`; `-runModal` is `-(NSModalResponse)`, i.e.
        // `NSInteger`, and owns its nested modal loop; `-URL` and `-path` are `-(id)` accessors
        // valid until the pool pops.
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

    /// Ask before a menu action changes state; unavailable UI supplies no approval.
    pub(crate) fn confirm(title: &str, body: &str, proceed_label: &str) -> Option<bool> {
        ask(title, body, proceed_label, AlertDefault::Proceed)
    }

    /// [`confirm`] for an owner gesture that changes this Mac.
    pub(crate) fn confirm_owner(title: &str, body: &str, proceed_label: &str) -> Option<bool> {
        ask(title, body, proceed_label, AlertDefault::Cancel)
    }

    /// A launch failure has one acknowledgement and grants no permission or setup decision.
    pub(crate) fn launch_error(_windowed: crate::platform::Windowed, title: &str, body: &str) {
        if super::os_ui_refused(title) {
            return;
        }
        let Some(mtm) = MainThread::new() else {
            return;
        };
        let (Some(title), Some(body), Some(close)) = (
            appkit::nsstring(title),
            appkit::nsstring(body),
            appkit::nsstring("Close"),
        ) else {
            return;
        };
        let Some(alert) = crate::app_icon::alert(mtm) else {
            return;
        };
        // SAFETY: the caller holds the windowed-launch proof and all AppKit construction
        // runs on the proved main thread. NSAlert copies its strings; runModal returns
        // only an acknowledgement, never authorization.
        unsafe {
            let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
            appkit::send_v_bool(app, sel!(activateIgnoringOtherApps:), true);
            appkit::send_v_id(alert.id(), sel!(setMessageText:), title.id());
            appkit::send_v_id(alert.id(), sel!(setInformativeText:), body.id());
            let _ = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), close.id());
            let _ = appkit::send_isize(alert.id(), sel!(runModal));
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum AlertDefault {
        /// Return (with any modifiers, via `alert_keys`) proceeds.
        Proceed,
        /// Return and Escape cancel (via `alert_keys`, which sends both to Cancel).
        Cancel,
    }

    fn ask(title: &str, body: &str, proceed_label: &str, default: AlertDefault) -> Option<bool> {
        if super::os_ui_refused(title) {
            return None;
        }
        let mtm = MainThread::new()?;
        let _park = crate::watchdog::park_modal();
        let _up = crate::alert_keys::ConfirmationUp::hold();
        // No key queued before the question opened (a burst's, or one typed ahead) answers it;
        // a queue that will not drain leaves Return and Escape on Cancel.
        let default = if crate::hwkey::fence_queued_keys(mtm) {
            default
        } else {
            aterm_log::warn!("confirmation: the key queue would not drain; Return cancels");
            AlertDefault::Cancel
        };
        let (Some(title), Some(body), Some(proceed), Some(cancel)) = (
            appkit::nsstring(title),
            appkit::nsstring(body),
            appkit::nsstring(proceed_label),
            appkit::nsstring("Cancel"),
        ) else {
            // Foundation refused a string, so no alert can be built.
            return None;
        };
        // An alert whose icon draws with no IconServices call on the main thread.
        let alert = crate::app_icon::alert(mtm)?;
        // SAFETY: standard `NSAlert` setters and `runModal`, on the main thread
        // (`MainThread::new()` above proves it). `-setMessageText:` and `-setInformativeText:` are
        // `-(void)(NSString *)` and copy their argument, so the +1 strings may drop at the end of
        // this frame. `-addButtonWithTitle:` is `-(NSButton *)(NSString *)` and `-window` is
        // `-(NSWindow *)`, both +0, so both are retained into `Obj`, not adopted. `-runModal` is
        // `-(NSModalResponse)`, an `NSInteger`.
        unsafe {
            appkit::send_v_id(alert.id(), sel!(setMessageText:), title.id());
            appkit::send_v_id(alert.id(), sel!(setInformativeText:), body.id());
            if default == AlertDefault::Cancel {
                let refuse = Obj::retain(appkit::send_id_id(
                    alert.id(),
                    sel!(addButtonWithTitle:),
                    cancel.id(),
                ));
                let proceed_button =
                    appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), proceed.id());
                if !proceed_button.is_null() {
                    // `-setRefusesFirstResponder:` is `-(void)(BOOL)` on NSControl,
                    // which the NSButton `-addButtonWithTitle:` returns is.
                    appkit::send_v_bool(proceed_button, sel!(setRefusesFirstResponder:), true);
                }
                // AppKit gives a first button titled Cancel Escape and no Return, so Return would
                // press nothing: the key watch sends Return and Escape both to Cancel.
                let panel = Obj::retain(appkit::send_id(alert.id(), sel!(window)));
                let _keys = match (panel, refuse) {
                    (Some(panel), Some(refuse)) => Obj::retain(refuse.id()).and_then(|on_return| {
                        crate::alert_keys::watch_alert_keys(panel, None, on_return, refuse)
                    }),
                    _ => None,
                };
                let response = appkit::send_isize(alert.id(), sel!(runModal));
                return Some(response == NS_ALERT_SECOND_BUTTON_RETURN);
            }
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

    /// Who asked for the `terminate:` AppKit is deciding, by the sender and the quit reason of
    /// the Apple event it answers ([`crate::system_quit::asker_of`]); with no event, a person.
    /// The quit reason (`why?`) and sender pid (`spid`) attributes of a quit Apple event.
    ///
    /// # Safety
    /// `event` is a live `NSAppleEventDescriptor`.
    unsafe fn quit_event_facts(event: Id) -> (Option<u32>, Option<i32>) {
        // SAFETY: `-attributeDescriptorForKeyword:` is `-(NSAppleEventDescriptor *)(AEKeyword)`,
        // `@@:I`: an `AEKeyword` is a 32-bit `FourCharCode`. It returns a descriptor the event
        // keeps (or nil), borrowed for this frame. `-enumCodeValue` is `-(OSType)` (`I@:`) and
        // `-int32Value` `-(SInt32)` (`i@:`).
        unsafe {
            let attribute_for: unsafe extern "C-unwind" fn(Id, Sel, u32) -> Id =
                aterm_objc::checked_msg();
            let attribute = |keyword: u32| {
                let descriptor =
                    attribute_for(event, sel!(attributeDescriptorForKeyword:), keyword);
                (!descriptor.is_null()).then_some(descriptor)
            };
            let reason = attribute(crate::system_quit::KEY_QUIT_REASON)
                .map(|descriptor| appkit::send_u32(descriptor, sel!(enumCodeValue)));
            let sender = attribute(crate::quit_reopen::KEY_SENDER_PID)
                .map(|descriptor| appkit::send_i32(descriptor, sel!(int32Value)));
            (reason, sender)
        }
    }

    fn terminate_asker() -> crate::system_quit::Asker {
        // SAFETY: Foundation sends on the main thread inside `applicationShouldTerminate:`.
        // `+sharedAppleEventManager` and `-currentAppleEvent` return BORROWED objects (nil outside
        // an event); the event is live for this pool.
        let read = autoreleasepool(|_| unsafe {
            let manager = appkit::send_id(
                class(c"NSAppleEventManager").as_id(),
                sel!(sharedAppleEventManager),
            );
            if manager.is_null() {
                return None;
            }
            let event = appkit::send_id(manager, sel!(currentAppleEvent));
            if event.is_null() {
                return None;
            }
            Some(quit_event_facts(event))
        });
        let Some((reason, pid)) = read else {
            return crate::system_quit::Asker::Person;
        };
        let sender = pid
            .filter(|pid| *pid > 0)
            .and_then(aterm_uds::process::read_process_path);
        let asker = crate::system_quit::asker_of(reason, sender.as_deref());
        aterm_log::info!(
            "a quit Apple event from pid {pid:?} ({}), reason {}: {asker:?}",
            sender
                .as_deref()
                .map_or_else(|| "no path".into(), std::path::Path::to_string_lossy),
            reason.map_or_else(
                || "none".to_owned(),
                |code| String::from_utf8_lossy(&code.to_be_bytes()).into_owned()
            ),
        );
        asker
    }

    /// AppKit's synchronous `applicationShouldTerminate:` hook, as [`super::ask_native_terminate`]
    /// decides (`wait`: whether a reply could come from AppKit's wait, which it cannot from inside
    /// the window's own handler, a modal alert or panel aterm runs): a quit the window takes up
    /// goes to it as `Wake::NativeTerminateRequested` ([`super::offer_terminate`]), and AppKit
    /// either waits on its reply (`NSTerminateLater`: the system's or Quit & Reopen's quit) or
    /// hears it cancelled (a person's quit, which waits on their answer); with no event loop to
    /// take it (none yet, or gone) the quit is cancelled and AppKit terminates now.
    pub(crate) fn defer_quit_for_terminate(
        wait: aterm_window::platform::macos::ReplyWait,
    ) -> aterm_window::platform::macos::TerminateAnswer {
        use aterm_window::platform::macos::{ReplyWait, TerminateAnswer};
        let asker = terminate_asker();
        let reply = super::ask_native_terminate(asker, wait == ReplyWait::Answerable);
        let posted = |generation| {
            TERMINATE_PROXY.get().is_some_and(|proxy| {
                proxy
                    .send_event(Wake::NativeTerminateRequested { generation })
                    .is_ok()
            })
        };
        match super::offer_terminate(reply, posted) {
            TerminateReply::Now => TerminateAnswer::Now,
            TerminateReply::Cancel | TerminateReply::TakeUp(_) => TerminateAnswer::Cancel,
            TerminateReply::Later(_) => TerminateAnswer::Later,
        }
    }

    /// Help ▸ aterm Help: open the bundled offline guide.
    pub(crate) fn open_help_url() {
        if let Some(help) = bundled_resource("Help.html") {
            open_in_workspace(&help, true);
        } else {
            open_in_workspace(&super::help_url(), false);
        }
    }

    fn bundled_resource(name: &str) -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        let res = exe.parent()?.parent()?.join("Resources").join(name);
        res.is_file().then(|| res.to_string_lossy().into_owned())
    }

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

    /// Open a local file aterm itself names.
    pub(crate) fn open_file_in_workspace(path: &std::path::Path) -> bool {
        path.to_str().is_some_and(|s| open_in_workspace(s, true))
    }

    /// Show the local file `path` aterm itself names.
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

    const SECOND_OPEN_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

    /// `-[NSWorkspace openURL:configuration:completionHandler:]`.
    pub(super) type OpenUrl = unsafe extern "C-unwind" fn(Id, Sel, Id, Id, BlockPtr);
    /// Its handler: `void (^)(NSRunningApplication *app, NSError *error)`.
    pub(super) type OpenedHandler = fn(Id, Id);
    /// `-[NSWorkspace URLForApplicationToOpenURL:]`.
    pub(super) type AppForUrl = unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id;

    /// Open System Settings at `pane`, without waiting for it: the first URL an app is
    /// registered for is handed to LaunchServices ([`open_url_unwaited`]), and the answer says
    /// which it was, not that System Settings came up (a failed open is logged when macOS says so).
    pub(crate) fn open_privacy_settings(pane: PrivacyPane) -> SettingsOpen {
        if MainThread::new().is_none() {
            return SettingsOpen::Refused;
        }
        let urls = privacy_settings_urls(pane);
        for (i, url) in urls.iter().enumerate() {
            let anchor = i + 1 < urls.len();
            if !opens_with_an_app(url)
                || !open_url_unwaited(url, anchor.then_some(SECOND_OPEN_DELAY))
            {
                continue;
            }
            return if anchor {
                SettingsOpen::Anchored
            } else {
                SettingsOpen::PaneRoot
            };
        }
        SettingsOpen::Refused
    }

    /// Whether an app is registered to open the URL `s`: LaunchServices' answer from its own
    /// database, which launches and asks no app.
    fn opens_with_an_app(s: &str) -> bool {
        let Some(ns) = appkit::nsstring(s) else {
            return false;
        };
        // SAFETY: `+URLWithString:` is `-(id)(NSString *)`, autoreleased (nil for a string that
        // is not a URL, checked); `+sharedWorkspace` is the `-(id)` singleton;
        // `-URLForApplicationToOpenURL:` is `AppForUrl` on it, its answer only tested for nil.
        // Every borrow lives inside this pool.
        autoreleasepool(|_| unsafe {
            let url = appkit::send_id_id(class(c"NSURL").as_id(), sel!(URLWithString:), ns.id());
            let ws = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            if url.is_null() || ws.is_null() {
                return false;
            }
            let app_for: AppForUrl = aterm_objc::checked_msg();
            !app_for(ws, sel!(URLForApplicationToOpenURL:), url).is_null()
        })
    }

    /// Hand the URL `s` to LaunchServices and return at once: System Settings launches while
    /// this thread goes on (the synchronous `-openURL:` held the frame being drawn 204 ms at a
    /// hand-over). With `anchor`, the URL is opened again that long after the first open
    /// completed: a System Settings that was not running comes up at its root, and the second
    /// open lands on the pane's section. `false` when nothing was handed over. Any thread.
    fn open_url_unwaited(s: &'static str, anchor: Option<std::time::Duration>) -> bool {
        if super::os_ui_refused(&format!("opening {s}")) {
            return false;
        }
        let Some(ns) = appkit::nsstring(s) else {
            return false;
        };
        let opened = move |_app: Id, error: Id| {
            if !error.is_null() {
                aterm_log::warn!("System Settings did not open {s}: macOS answered an error");
                return;
            }
            let Some(delay) = anchor else {
                return;
            };
            let spawned = std::thread::Builder::new()
                .name("aterm-settings-anchor".to_owned())
                .spawn(move || {
                    std::thread::sleep(delay);
                    let _ = open_url_unwaited(s, None);
                });
            if let Err(e) = spawned {
                aterm_log::warn!("System Settings' section was not opened again: {e}");
            }
        };
        // SAFETY: `+URLWithString:` is `-(id)(NSString *)`, autoreleased (nil for a string that
        // is not a URL, checked); `+sharedWorkspace` and `+[NSWorkspaceOpenConfiguration
        // configuration]` are `-(id)` accessors, autoreleased and checked; the handler is
        // `OpenedHandler`, exactly `opened`'s; `-openURL:configuration:completionHandler:` is
        // `OpenUrl` on the workspace, which copies the handler and retains the URL and the
        // configuration until it calls the handler. Every borrow lives inside this pool.
        autoreleasepool(|_| unsafe {
            let url = appkit::send_id_id(class(c"NSURL").as_id(), sel!(URLWithString:), ns.id());
            let ws = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            let config = appkit::send_id(
                class(c"NSWorkspaceOpenConfiguration").as_id(),
                sel!(configuration),
            );
            if url.is_null() || ws.is_null() || config.is_null() {
                return false;
            }
            let Some(handler) = RcBlock::of::<OpenedHandler, _>(opened) else {
                return false;
            };
            let open: OpenUrl = aterm_objc::checked_msg();
            open(
                ws,
                sel!(openURL:configuration:completionHandler:),
                url,
                config,
                handler.as_block_ptr(),
            );
            true
        })
    }

    /// Put `window` — a panel aterm itself built, never another app's — in front
    /// of every app's windows WITHOUT activating aterm (`-orderFrontRegardless`).
    pub(crate) fn order_front_regardless(window: Id, what: &str) -> bool {
        if super::os_ui_refused(what) || MainThread::new().is_none() || window.is_null() {
            return false;
        }
        // SAFETY: `-orderFrontRegardless` is `-(void)` on the live `NSWindow` the
        // caller owns, on the main thread (checked above).
        unsafe { appkit::send_v(window, sel!(orderFrontRegardless)) };
        true
    }

    #[cfg(test)]
    mod tests;
}

#[cfg(test)]
#[path = "menu_tests.rs"]
mod tests;
