// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A new tab's, split's or window's shell starts OFF the event loop.
//!
//! Opening a tab ran the whole PTY spawn inside the event-loop turn that asked
//! for it: the start-folder check (up to 250 ms), the recursion edge file and
//! graph entries, the PTY pair, the fork and the wait for the child's exec
//! verdict (bounded at 10 s, then up to 2 s of reap). About 3 ms on a quiet
//! machine, 8-10 ms under load, and 12 s with every window frozen when the
//! folder or the shell sat on a network volume that stopped answering.
//!
//! Now the turn only books the tab ([`App::open_tab_off_loop`]), the split
//! ([`App::open_split_off_loop`]) or the window ([`App::open_window_off_loop`]):
//! it reserves the session id (and a window's id), copies the spawn
//! factory and hands [`crate::spawn::spawn_session`] to the one `aterm-spawner`
//! thread. The folder a pane held by another program reported is checked
//! there too ([`crate::spawn_folder::PaneFolder`]), not in the turn, and so is
//! a request's agent identity ([`IdentityPrep`]). The
//! session comes back in [`crate::Wake::TabSpawned`]; the main thread attaches
//! its reader and installs the tab or pane as Cmd-T and Cmd-D always did
//! ([`App::land_tab_spawn`]). Whoever waits is answered then
//! ([`TabSpawnThen`]): the wire `spawn` replies `OK <sid>` only once the sid
//! is registered, `tab new` counts the tab once it is in the window, a
//! worker's spawn (`spawn agent=claude`, the menu bar's worker launch) is
//! made a worker once its newborn is registered, and a connected preset's tab
//! is connected to its origin. A spawn that fails says so
//! through the same `new_tab_failed` or `split_failed` row; a window that
//! closed meanwhile drops the session, whose teardown hangs the shell up and
//! reaps it. A split is refused at landing as at asking (the focused pane is
//! shared, too small, or not a terminal), because the window may have changed
//! in between; the session drops the same way.
//!
//! What is typed into the window meanwhile is for the new tab: Cmd-T then `ls`
//! put `ls` in the new tab, because the loop only read the keys after the
//! spawn. So the window's keys, modifiers and IME wait
//! ([`App::defer_tab_spawn_input`]) and replay in order once the tab lands. If
//! it never does, the keys are dropped and counted, never typed into the tab
//! the person left; the modifier changes still replay, so no chord is stranded.
//! The pointer waits with them for a tab or split landing in the window it
//! points at, so a click after Cmd-T lands after the tab, not before it. A hold
//! is bounded ([`INPUT_HOLD`]): what is held past it is dropped as for a spawn
//! that never landed, the moment the window's next input arrives or the spawn
//! settles, so later keys never reach the old tab while earlier ones replay
//! into the new one ([`App::expire_tab_spawn_input`]). Each event waits for the
//! spawn that was newest when it was typed, and replays when that one lands,
//! held again only for what the replay itself books
//! ([`App::settle_landed_spawn_input`], [`App::replay_settled_input`]): Cmd-T,
//! `ls`, Cmd-T runs `ls` in the first new tab even when the second Cmd-T came
//! through the menu, whose wake no hold delays.
//!
//! A menu command reaches the loop a turn late: the native menu's key
//! equivalent (Cmd-T, Cmd-N), the toolbar's `+` and the palette post a
//! [`Wake::MenuAction`] that winit delivers only once AppKit has dispatched
//! every key already queued behind it. Each post is marked ([`MenuMarks`]), so
//! what is typed before its dispatch waits for it, then for the spawn it booked
//! ([`App::claim_menu_held_input`]), or goes on as typed if it booked none.
//!
//! A new window (Cmd-N, the menu, the identity picker's window rows, a
//! connected preset's `place=window`) is built around its first session, so it
//! is made when that session lands ([`App::install_spawned`]): installed at the
//! grid the shell was spawned at, frontmost, and given its OS window there
//! ([`LandingOs`], an inert arm in tests). It appears one spawn later; Cmd-N
//! then `ls` still puts `ls` in it, because what is typed into the window it
//! was asked from waits for it and replays through its own OS window. A spawn
//! that fails says so through the same `new_window_failed` row as before, and
//! a surface that cannot be made rolls the window and its session back. Once
//! the loop is exiting (a quit, the last window's close) nothing lands: no
//! window is made and no tab installed for a process on its way out, and the
//! session drops as if the quit had cancelled it.
//!
//! The spawner also starts the session's reader and helper threads, parked on a
//! closed gate (`spawn::prepare_deferred_reader`, the overlap handoff's own
//! primitive); the main thread opens it once the tab is registered, so the
//! reader's first `Wake::Ready` or `Wake::Exit` cannot reach the loop before the
//! session it names (both are ignored for a session the registry does not know).
//! Off unix the main thread attaches the reader there instead.
//!
//! The filesystem work a spawn needs before its fork (the PTY pair, the
//! exec-status channel, the child's edge-token file) is done ahead by
//! [`crate::spawn_prep`], which the spawn takes from when it holds one, so the
//! spawner's own share of a new tab is the fork and the exec verdict.
//!
//! Jobs run one at a time, in order, so tabs land in the order they were asked
//! for. A spawn stalled on a dead volume holds the ones behind it, as the main
//! thread did, but nothing else waits for it.
//!
//! Not here: session 0 (spawned before the event loop runs, its shell already
//! overlapping the rest of the launch), and restore and adoption (one spawn
//! per restore slice). They spawn in the turn as before. The spawner still
//! forks this process, so the copy-on-write faults that follow a fork still
//! land on whichever thread writes first.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_window::application::ApplicationHandler as _;
use aterm_window::event::WindowEvent;
use aterm_window::event_loop::{ActiveEventLoop, EventLoopProxy};
use aterm_window::keyboard::ModifiersState;
use aterm_window::window::WindowId as WinitWindowId;

use crate::agent_identity::{self, SpawnAdmit};
use crate::platform::AppRt as _;
use crate::{App, Session, Wake, WindowId};

/// How long a window's input waits for a tab in flight to it before it goes back
/// to what the window shows. Past the exec verdict's 10 s budget, its 2 s reap
/// and the folder checks, a spawn still out is wedged, and a window whose keys
/// went nowhere for longer would read as frozen.
const INPUT_HOLD: Duration = Duration::from_secs(15);

/// How long a spawn's fork waits for the event loop to park
/// ([`crate::watchdog::forks_wait_for_park`]): longer than the booking turn and
/// a status sweep of every tab, shorter than anything a person would notice.
const FORK_PARK_WAIT: Duration = Duration::from_millis(3);

/// How long the event loop holds its status sweep after it books a spawn
/// ([`TabSpawns::status_sweep_held_until`]), so the fork, which waits for the
/// loop to park, does not first wait out a sweep of every tab (about 2 ms at 40
/// tabs on this host's slow cores, 2026-10-17). The sweep after the landing
/// classifies every session due by then; one that lands later than this gets
/// its sweep at the end of the hold.
const SWEEP_HOLD: Duration = Duration::from_millis(15);

/// How many input events a window holds for a tab in flight; past it the newest
/// is dropped and counted, as the pre-Commit queue does
/// ([`crate::MAX_DEFERRED_HANDOFF_INPUT`]). Pointer motion and a scroll in
/// progress fold into the event before them ([`coalesce`]), so moving the
/// pointer cannot fill it.
const MAX_HELD_INPUT: usize = crate::MAX_DEFERRED_HANDOFF_INPUT;

/// What a waiter for a window that never opened hears: the words the window
/// built in the turn answered a connected preset with.
const NO_NEW_WINDOW: &str = "could not open a new window";

/// What a waiter for a spawn the quit overtook hears.
const QUITTING: &str = "aterm is quitting before the session started";

/// Who waits for a tab, split or window spawned off the loop.
pub(crate) enum TabSpawnThen {
    /// Cmd-T, Cmd-D, the menu, the strip's `+`: the tab or pane itself is the
    /// answer.
    Nothing,
    /// The wire `spawn`: `OK <sid>` once the session is registered, the hosting
    /// window raised first when `raise` says so; `ERR <why>` when it never lands.
    Reply {
        reply: Sender<Result<String, String>>,
        raise: bool,
    },
    /// The wire `tab new`: `OK <active> <count>` of the window once the tab is
    /// in it; `ERR <why>` when it never lands.
    Counts(Sender<Result<(usize, usize), String>>),
    /// A main-thread row that acts on the newborn at once (a worker's spawn,
    /// the menu bar's worker launch, a connected preset): run with its sid
    /// once it is registered, or with why not.
    Run(SpawnDone),
}

/// A main-thread step that runs once a spawn off the loop settles: with the
/// newborn's sid, registered, or with why there is none. It never leaves the
/// main thread, so it need not be `Send`.
pub(crate) type SpawnDone = Box<dyn FnOnce(&mut App, Result<String, String>)>;

impl TabSpawnThen {
    /// Answer whoever waits with `outcome`, the hosting `window` raised for a
    /// landed reply that asked for it.
    pub(crate) fn answer(self, app: &mut App, window: WindowId, outcome: Result<String, String>) {
        match self {
            Self::Nothing => {}
            Self::Reply { reply, raise } => {
                if raise
                    && outcome.is_ok()
                    && let Some(os_window) =
                        app.windows.get(&window).and_then(|ws| ws.os_window.clone())
                {
                    app.apprt.window_bring_to_front(&os_window);
                }
                let _ = reply.send(outcome);
            }
            Self::Counts(reply) => {
                let counts = outcome.map(|_| {
                    app.windows.get(&window).map_or((0, 0), |ws| {
                        (ws.tab_set.active_index().unwrap_or(0), ws.tab_set.len())
                    })
                });
                let _ = reply.send(counts);
            }
            Self::Run(then) => then(app, outcome),
        }
    }
}

/// Where a session spawned off the loop lands in its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpawnPlace {
    /// A new last tab, made active.
    Tab,
    /// A new pane splitting the window's focused pane in this direction, and
    /// focused.
    Split(crate::pane::SplitDir),
    /// The sole tab of a new window, made frontmost: the window is built around
    /// the session as it lands. `from` is the window it was asked from, whose
    /// input waits for it.
    Window { from: Option<WindowId> },
}

impl SpawnPlace {
    /// The window a spawn landing here in `window` was asked from: whose input
    /// waits for it and whose focused pane's folder it inherits. A tab's or
    /// split's own, or the window a new one was asked from.
    fn asked_from(self, window: WindowId) -> Option<WindowId> {
        match self {
            Self::Window { from } => from,
            Self::Tab | Self::Split(_) => Some(window),
        }
    }

    /// Say that the spawn for this place failed with `error`, in the log and
    /// in the row its gesture posts, and return the waiter's reason.
    fn failed(self, app: &mut App, error: &str) -> String {
        match self {
            Self::Tab => {
                crate::logging::stderr_line!("aterm-gui: could not open a new tab: {error}");
                app.post_message(crate::message_reporters::new_tab_failed(error));
                error.to_string()
            }
            Self::Split(_) => {
                crate::logging::stderr_line!("aterm-gui: could not split the pane: {error}");
                app.post_message(crate::message_reporters::split_failed(error));
                format!("could not split the pane: {error}")
            }
            Self::Window { .. } => {
                crate::logging::stderr_line!("aterm-gui: could not open a new window: {error}");
                app.post_message(crate::message_reporters::new_window_failed(error));
                NO_NEW_WINDOW.to_string()
            }
        }
    }
}

/// The OS side of a landing: whether the loop is on its way out, and the OS
/// window a new window lands in. The event loop in a run; a test passes an
/// inert arm, so no landing test reaches the window server.
pub(crate) trait LandingOs {
    /// The loop is exiting (a quit, the last window's close): nothing lands.
    fn exiting(&self) -> bool;
    /// Create window `wid`'s OS window and present surface; `false` when it
    /// cannot be made.
    fn attach(&self, app: &mut App, wid: WindowId) -> bool;
}

impl LandingOs for ActiveEventLoop {
    fn exiting(&self) -> bool {
        ActiveEventLoop::exiting(self)
    }

    fn attach(&self, app: &mut App, wid: WindowId) -> bool {
        // Headless keeps the window logical, as a window built in the turn did.
        app.headless || app.attach_os_window(self, wid)
    }
}

/// One tab's spawn, as the spawner thread runs it: [`crate::spawn::spawn_session`]'s
/// arguments, with the factory copied (`reader_after_install` set).
pub(crate) struct TabSpawnJob {
    id: u64,
    window: WindowId,
    rows: u16,
    cols: u16,
    cell_px: Option<(u16, u16)>,
    factory: crate::spawn::SessionFactory,
    proxy: EventLoopProxy<Wake>,
    /// The folder the caller named; else the first of `pane_folders` a check
    /// here takes.
    cwd: Option<String>,
    pane_folders: Vec<crate::spawn_folder::PaneFolder>,
    identity: Option<String>,
    /// What is done with `identity` before the fork.
    identity_prep: IdentityPrep,
    /// The session a controller-as-tab preset observes.
    observe: Option<aterm_session::SessionId>,
    /// A worker's task file, written by `spawn_session` once the sid is known.
    task: Option<crate::task_launch::TaskSeed>,
    completion: Arc<TabSpawnCompletion>,
    /// An adopted warm spare (`crate::spare`): handed over as it is when its
    /// check passes, else dropped and the cold spawn above runs under `id`.
    adopt: Option<Box<AdoptSpare>>,
}

/// A warm spare a Cmd-T adopts, on its way through the spawner.
pub(crate) struct AdoptSpare {
    /// The id it was spawned under, which it keeps.
    pub(crate) id: u64,
    pub(crate) session: Session,
    pub(crate) check: crate::spare::AdoptCheck,
    /// Where the spawner says why it was not handed over.
    pub(crate) miss: Arc<OnceLock<&'static str>>,
    /// Where the spawner names the folder a parked spare `cd`s to at its
    /// release ([`crate::spare::AdoptCheck::verdict`]).
    pub(crate) retarget: Arc<OnceLock<String>>,
}

/// Admission facts captured when a spawn is booked on the main thread.
#[derive(Default)]
pub(crate) enum SpawnAdmission {
    #[default]
    Plain,
    Connected {
        origin: aterm_session::SessionId,
        local: u64,
    },
    /// Summarize in a New Tab's worker (`App::summarize_in_new_tab`):
    /// single-flight while it books ([`TabSpawns::summary_pending`]); it lands
    /// as a plain tab does.
    Summary,
}

/// What a spawn off the loop is asked for, beyond its place.
#[derive(Default)]
pub(crate) struct SpawnAsk {
    pub(crate) admission: SpawnAdmission,
    /// The folder the caller named (`spawn cwd=`); `None` inherits
    /// `folder_of`'s, else the focused pane's.
    pub(crate) cwd: Option<String>,
    /// The pane whose folder the newborn inherits before the focused pane's (a
    /// connected preset's origin), read in the turn and checked on the spawner
    /// like the focused pane's.
    pub(crate) folder_of: Option<crate::spawn_folder::PaneFolder>,
    /// The agent identity the newborn spawns under; `None` is the human's own.
    pub(crate) identity: Option<String>,
    /// What the spawner does with `identity` before the fork.
    pub(crate) identity_prep: IdentityPrep,
    /// The session a controller-as-tab preset observes
    /// (`ATERM_OBSERVE_SESSION_ID`).
    pub(crate) observe: Option<aterm_session::SessionId>,
    /// A WORKER's task file (`spawn agent=claude`, `task_launch`): written by
    /// `spawn::spawn_session` once the newborn's sid is known, and named to its
    /// shell as `ATERM_TASK_FILE`. The main thread keeps a clone, which
    /// removes the file when the worker does not start.
    pub(crate) task: Option<crate::task_launch::TaskSeed>,
    /// Keep the window's active tab (a wire `spawn` that does not raise): the
    /// newborn TAB lands behind it, the `tab_strip` model's NewTab then
    /// SelectTab, so the person's view stays put. A split focuses its new pane
    /// whatever this says.
    pub(crate) background: bool,
    /// The newborn's own theme (`spawn theme=`), validated when asked; a
    /// split with none inherits the divided pane's ([`App::install_spawned`]).
    pub(crate) theme: Option<crate::session_theme::SessionTheme>,
}

/// What the spawner does with a spawn's agent identity before it forks, where
/// the identities folder's metadata (it can sit on a slow volume) holds no
/// window, and only once the main thread admitted the spawn, so a spawn
/// refused there (its window, its `of=`) makes no identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum IdentityPrep {
    /// Only the spawn's own check that it exists (a picker row naming one from
    /// the roster).
    #[default]
    Exists,
    /// Admit it as a request's spawn does
    /// ([`crate::agent_identity::check_for_spawn`]): an `inherited` one must
    /// exist; one the request named is made first, the verb's only create
    /// path. Its folders are private before the fork, its primer writes off
    /// this thread, and the admission holds it against `identities forget`
    /// until the shell's own does ([`provision`]). Refused, the waiter hears
    /// why in the words the request's turn refused it with, and no failure row
    /// is posted ([`Refusal`]).
    Admit { inherited: bool },
    /// Make it first, if it does not exist (the picker's new-identity row), and
    /// admit it as a named one: a failure is the tab's failure row.
    Create,
}

impl SpawnAsk {
    /// A plain ask: the folder and identity, nothing made, nothing observed.
    pub(crate) fn new(cwd: Option<&str>, identity: Option<&str>) -> Self {
        Self {
            cwd: cwd.map(str::to_owned),
            identity: identity.map(str::to_owned),
            ..Self::default()
        }
    }

    /// A wire `spawn`'s ask: the folder it named and the identity it wears,
    /// admitted on the spawner; `inherited` when the request named none and
    /// the aimed session's is passed down.
    pub(crate) fn requested(
        cwd: Option<String>,
        identity: Option<String>,
        inherited: bool,
    ) -> Self {
        Self {
            cwd,
            identity,
            identity_prep: IdentityPrep::Admit { inherited },
            ..Self::default()
        }
    }
}

/// A spawn the spawner refused before forking, for a reason its waiter hears
/// verbatim, with no failure row: what the request's turn refused before the
/// check moved to the spawner ([`IdentityPrep::Admit`]).
#[derive(Debug)]
struct Refusal(String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// The words of the [`Refusal`] `e` carries, if the spawner refused the spawn
/// rather than failed it.
fn refusal(e: &std::io::Error) -> Option<String> {
    e.get_ref()
        .and_then(|inner| inner.downcast_ref::<Refusal>())
        .map(|refused| refused.0.clone())
}

/// What the spawner thread hands back ([`crate::Wake::TabSpawned`]).
pub(crate) struct TabSpawned {
    /// The reserved session id.
    pub(crate) id: u64,
    /// The window the tab lands in.
    pub(crate) window: WindowId,
    /// The session, or why there is none.
    pub(crate) session: std::io::Result<Session>,
    /// The closed gate the session's reader is parked on, opened once the tab is
    /// registered; `None` when the reader is still to be attached (off unix).
    pub(crate) reader_gate: Option<crate::spawn::DeferredReaderGate>,
}

impl std::fmt::Debug for TabSpawned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TabSpawned")
            .field("id", &self.id)
            .field("window", &self.window)
            .field("spawned", &self.session.as_ref().map(|_| ()))
            .finish()
    }
}

#[derive(Default)]
struct CompletionState {
    cancelled: bool,
    ready: Option<TabSpawned>,
}

/// The existing InFlight entry owns this job's one result. A wake only points
/// to it, so normal Quit can drop an uninstalled Session even when winit still
/// holds its completion event. No second result queue or registry is needed.
#[derive(Default)]
pub(crate) struct TabSpawnCompletion(Mutex<CompletionState>);

impl std::fmt::Debug for TabSpawnCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        f.debug_struct("TabSpawnCompletion")
            .field("cancelled", &state.cancelled)
            .field("ready", &state.ready)
            .finish()
    }
}

impl TabSpawnCompletion {
    fn cancelled(&self) -> bool {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).cancelled
    }

    fn store(&self, done: TabSpawned) -> bool {
        let rejected = {
            let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
            if state.cancelled || state.ready.is_some() {
                Some(done)
            } else {
                state.ready = Some(done);
                None
            }
        };
        let accepted = rejected.is_none();
        drop(rejected); // Like cancellation, teardown happens outside the lock.
        accepted
    }

    fn take(&self) -> Option<TabSpawned> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .ready
            .take()
    }

    fn cancel(&self) {
        let done = {
            let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
            state.cancelled = true;
            state.ready.take()
        };
        // Session teardown may signal/reap and wake its reader. Never do that
        // while holding the publication lock a finishing worker needs.
        drop(done);
    }
}

/// A tab, split or window in flight, as the main thread remembers it.
struct InFlight {
    id: u64,
    /// An adopted warm spare's fallback (`crate::spare`): the id a cold spawn
    /// lands under when the spawner finds the spare no longer valid. `None` for
    /// every cold spawn.
    alt_id: Option<u64>,
    /// The window it lands in: a tab's or split's own, or a new window's
    /// reserved id.
    window: WindowId,
    place: SpawnPlace,
    asked: Instant,
    /// A tab's family-tree parent, read when the tab was asked for. A split's
    /// is the pane it splits at landing. A window's is its registry lineage
    /// parent: a controlled window's origin, else none.
    parent: Option<aterm_session::SessionId>,
    /// The engine colors its factory copy carried: the engine-config generation
    /// (`App::engine_color_fanouts`), which also moves with the scheme baseline
    /// (`TerminalConfig::color_scheme` rides in the projection).
    colors: u64,
    /// The window's grid and cell box the session is spawned at: rows, cols and
    /// `spawn_cell_px`.
    grid: SpawnGrid,
    then: TabSpawnThen,
    admission: SpawnAdmission,
    /// [`SpawnAsk::background`]: a tab lands behind the window's active one.
    background: bool,
    completion: Arc<TabSpawnCompletion>,
    /// The newborn's own theme, as asked ([`SpawnAsk::theme`]).
    theme: Option<crate::session_theme::SessionTheme>,
}

/// A window's grid (rows, cols) and cell box in device pixels, as a spawn takes
/// them ([`App::spawn_cell_px`]).
type SpawnGrid = (u16, u16, Option<(u16, u16)>);

/// One input event a window holds for a spawn in flight.
#[derive(Debug)]
struct Held {
    /// The OS window it was typed into.
    typed_into: WinitWindowId,
    /// The window it is for: its own, or the new window asked from it.
    target: WindowId,
    /// The spawn it waits for (its in-flight id): the newest that took the
    /// focus of the window it was typed into when it was typed. `None` while
    /// it waits for a menu command's dispatch instead ([`Self::menu`]).
    spawn: Option<u64>,
    event: WindowEvent,
    /// When its hold ends: [`INPUT_HOLD`] after the spawn it waits for was
    /// asked for (or its menu command posted). Settled later, it is dropped as
    /// for a spawn that never landed, never replayed late.
    until: Instant,
    /// The menu command it waits for, typed after that command was posted and
    /// before the loop dispatched it ([`MenuMarks`]): held under the window it
    /// was typed into until the dispatch says for what. [`MenuMark::NONE`]
    /// once that is known.
    menu: MenuMark,
    /// A modifier change held for a new window that also reached the window
    /// it was typed into as typed ([`App::defer_tab_spawn_input`]).
    applied: bool,
}

/// Pointer input: held only for a tab or split landing in the window it points
/// at ([`App::defer_tab_spawn_input`]), and replayed there. A file dragged in
/// is not held: its drop is its own gesture, at the window it lands on.
fn is_pointer(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::CursorMoved { .. }
            | WindowEvent::CursorEntered { .. }
            | WindowEvent::CursorLeft { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
    )
}

/// A key press or an IME commit: what the "Keystrokes dropped" row counts.
fn is_keystroke(event: &WindowEvent) -> bool {
    matches!(event, WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed())
        || matches!(event, WindowEvent::Ime(aterm_window::event::Ime::Commit(_)))
}

/// Fold `next` into `last` when it only carries on what `last` did: the pointer
/// moving on, or a scroll in progress by the same kind of step. A click lands
/// where the last move left the pointer, so the moves between are no input of
/// their own, and a scroll's steps add up to the same scroll.
fn coalesce(last: &mut WindowEvent, next: &WindowEvent) -> bool {
    use aterm_window::event::{MouseScrollDelta, TouchPhase};
    match (last, next) {
        (
            WindowEvent::CursorMoved {
                device_id: was,
                position: at,
            },
            WindowEvent::CursorMoved {
                device_id,
                position,
            },
        ) if was == device_id => {
            *at = *position;
            true
        }
        (
            WindowEvent::MouseWheel {
                device_id: was,
                delta: sum,
                phase: TouchPhase::Moved,
            },
            WindowEvent::MouseWheel {
                device_id,
                delta,
                phase: TouchPhase::Moved,
            },
        ) if was == device_id => match (sum, delta) {
            (MouseScrollDelta::LineDelta(x, y), MouseScrollDelta::LineDelta(dx, dy)) => {
                *x += dx;
                *y += dy;
                true
            }
            (MouseScrollDelta::PixelDelta(at), MouseScrollDelta::PixelDelta(by)) => {
                at.x += by.x;
                at.y += by.y;
                true
            }
            _ => false,
        },
        _ => false,
    }
}

/// What settling `held` (one target's, in order) hands back to replay, and how
/// many keystrokes it drops. An event still within its hold whose spawn
/// `landed` replays: keys, modifiers and IME through `through` (the target's
/// own OS window; a new window's input was typed into the one it was asked
/// from), the pointer where it was typed. Any other (its spawn never landed,
/// or it waited past its hold) keeps only what leaves the window it was typed
/// into coherent: its modifier changes (unless they already reached it as
/// typed, [`Held::applied`]), the pointer's position, entering and leaving, and the
/// release of a button pressed before the hold. Keystrokes are dropped and
/// counted; clicks and scrolls are dropped, a click's release with its press.
fn sift_held(
    held: Vec<Held>,
    landed: bool,
    through: Option<WinitWindowId>,
    now: Instant,
) -> (Vec<(WinitWindowId, WindowEvent)>, u32) {
    let mut replay = Vec::with_capacity(held.len());
    let mut dropped = 0u32;
    // The buttons whose press was dropped here: their releases go with them.
    let mut pressed = Vec::new();
    for h in held {
        if landed && now < h.until {
            let via = if is_pointer(&h.event) {
                h.typed_into
            } else {
                through.unwrap_or(h.typed_into)
            };
            replay.push((via, h.event));
            continue;
        }
        let keep = match &h.event {
            WindowEvent::ModifiersChanged(_) => !h.applied,
            WindowEvent::CursorMoved { .. }
            | WindowEvent::CursorEntered { .. }
            | WindowEvent::CursorLeft { .. } => true,
            WindowEvent::MouseInput { state, button, .. } if state.is_pressed() => {
                pressed.push(*button);
                false
            }
            WindowEvent::MouseInput { button, .. } => {
                match pressed.iter().position(|was| was == button) {
                    Some(at) => {
                        pressed.remove(at);
                        false
                    }
                    None => true,
                }
            }
            event => {
                if is_keystroke(event) {
                    dropped = dropped.saturating_add(1);
                }
                false
            }
        };
        if keep {
            replay.push((h.typed_into, h.event));
        }
    }
    (replay, dropped)
}

/// The App's side of the tabs in flight, and what was typed while they were.
#[derive(Default)]
pub(crate) struct TabSpawns {
    in_flight: Vec<InFlight>,
    /// Input typed into a window with a spawn in flight that takes its focus
    /// (or a menu command not yet dispatched that may book one), in arrival
    /// order.
    held: Vec<Held>,
    /// Keystrokes refused past [`MAX_HELD_INPUT`], per window.
    dropped: Vec<(WindowId, u32)>,
    /// Windows whose held input is no longer a coherent key stream: a drop broke
    /// a press/release pair, or a focus loss cleared the modifiers it was typed
    /// under. Their modifier snapshot is cleared after the replay.
    incoherent: Vec<WindowId>,
    /// The menu commands posted and not yet dispatched ([`MenuMarks`]).
    pub(crate) menu: Arc<MenuMarks>,
    /// Held input is being replayed: what was booked or posted before the
    /// replay began came after it was typed, so it waits for none of that.
    floor: Option<ReplayFloor>,
}

/// What input being replayed was typed before ([`TabSpawns::floor`]), so a
/// replayed event is held again only for what the replay itself books or
/// posts: a replayed Cmd-T's tab takes the keys typed after it, but a spawn
/// or menu command already pending when the replay began does not take keys
/// typed before it.
#[derive(Clone, Copy, Debug)]
struct ReplayFloor {
    /// The last menu mark minted when the replay began: every mark up to it.
    marks: u64,
    /// The first fresh booking id of the replay: every earlier reservation
    /// is passed over, even when two clock readings are equal. `None` for
    /// input a menu command's dispatch handed back, which may still be for
    /// a spawn booked before that command was posted.
    spawns: Option<u64>,
}

impl TabSpawns {
    /// No queued or running spawn still depends on this process. Unlike the
    /// input hold, replacement cannot time out a job that has not settled.
    pub(crate) fn is_empty(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// The in-flight entry is Summarize's reservation: every error,
    /// cancellation and successful landing removes it before delivering the
    /// result.
    pub(crate) fn summary_pending(&self) -> bool {
        self.in_flight
            .iter()
            .any(|t| matches!(t.admission, SpawnAdmission::Summary))
    }

    /// While a spawn booked less than [`SWEEP_HOLD`] ago is in flight, the
    /// instant the event loop's status sweep may run again: it waits for the
    /// fork rather than the fork for it. `None` when nothing holds it.
    pub(crate) fn status_sweep_held_until(&self, now: Instant) -> Option<Instant> {
        self.in_flight
            .iter()
            .map(|t| t.asked + SWEEP_HOLD)
            .max()
            .filter(|at| *at > now)
    }

    /// The window input typed into `window` at `now` is for, if it waits, when
    /// that wait ends, and the spawn it waits for: the one the newest spawn
    /// asked for within [`INPUT_HOLD`] that takes `window`'s focus lands in. A
    /// tab or split lands in `window` itself; a new window asked from `window`
    /// became the one it typed into. Replayed input waits for no spawn asked
    /// before the replay's first booking id ([`ReplayFloor::spawns`]).
    fn hold_target(&self, window: WindowId, now: Instant) -> Option<(WindowId, Instant, u64)> {
        let after = self.floor.and_then(|floor| floor.spawns);
        self.in_flight
            .iter()
            .rev()
            .find(|t| {
                t.place.asked_from(t.window) == Some(window)
                    && now.saturating_duration_since(t.asked) < INPUT_HOLD
                    // An adopted spare's id predates this booking; its
                    // fallback is the fresh reservation made for this ask.
                    && after.is_none_or(|first| t.alt_id.unwrap_or(t.id) >= first)
            })
            .map(|t| (t.window, t.asked + INPUT_HOLD, t.id))
    }

    /// Hold `held`, folded into the event before it when it only carries that
    /// on ([`coalesce`]). Past [`MAX_HELD_INPUT`] it is refused: a keystroke is
    /// counted, and the target's stream is marked for a modifier repair, since
    /// the refused event may be a chord's release.
    fn hold(&mut self, held: Held) {
        if let Some(last) = self.held.last_mut()
            && last.typed_into == held.typed_into
            && last.target == held.target
            && last.spawn == held.spawn
            && last.menu == held.menu
            && coalesce(&mut last.event, &held.event)
        {
            last.until = last.until.max(held.until);
            return;
        }
        if self.held.len() < MAX_HELD_INPUT {
            self.held.push(held);
            return;
        }
        if is_keystroke(&held.event) {
            match self.dropped.iter_mut().find(|(w, _)| *w == held.target) {
                Some((_, n)) => *n = n.saturating_add(1),
                None => self.dropped.push((held.target, 1)),
            }
        }
        if !self.incoherent.contains(&held.target) {
            self.incoherent.push(held.target);
        }
    }

    /// `window` lost focus: input held from it was typed under modifiers the
    /// loss just cleared, so its replay ends with a repair, whether it is for
    /// `window` or for a new window asked from it.
    pub(crate) fn note_focus_loss(&mut self, window: WindowId) {
        let targets: Vec<WindowId> = std::iter::once(window)
            .chain(
                self.in_flight
                    .iter()
                    .filter(|t| t.window != window && t.place.asked_from(t.window) == Some(window))
                    .map(|t| t.window),
            )
            .collect();
        for target in targets {
            if self.held.iter().any(|h| h.target == target) && !self.incoherent.contains(&target) {
                self.incoherent.push(target);
            }
        }
    }
}

/// Which post of a menu command a [`Wake::MenuAction`] is ([`MenuMarks`]):
/// [`Self::NONE`] for one posted unmarked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MenuMark(u64);

impl MenuMark {
    /// No mark: nothing waits for its dispatch.
    pub(crate) const NONE: Self = Self(0);
}

/// MENU COMMANDS POSTED AND NOT YET DISPATCHED (sweep #32). Input typed while
/// one is pending waits for its dispatch ([`App::defer_tab_spawn_input`]),
/// then for the spawn it booked ([`App::claim_menu_held_input`]). The App
/// keeps one, shared with the native menu and toolbar
/// ([`serve_native_menu_marks`]), whose callbacks post without the App.
#[derive(Debug, Default)]
pub(crate) struct MenuMarks(Mutex<MarksState>);

#[derive(Debug, Default)]
struct MarksState {
    /// The last mark given out; marks count from one.
    minted: u64,
    /// The marks posted and not yet dispatched, oldest first, with when each
    /// was posted.
    pending: Vec<(MenuMark, Instant)>,
}

impl MenuMarks {
    /// Mark a post at `now`. Marks older than [`INPUT_HOLD`] go: nothing waits
    /// for them any more.
    fn mint(&self, now: Instant) -> MenuMark {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        state
            .pending
            .retain(|(_, at)| now.saturating_duration_since(*at) < INPUT_HOLD);
        state.minted += 1;
        let mark = MenuMark(state.minted);
        state.pending.push((mark, now));
        mark
    }

    /// `mark` was dispatched (or its post refused): `true` when it was still
    /// pending, so input may be waiting for it.
    pub(crate) fn settle(&self, mark: MenuMark) -> bool {
        if mark == MenuMark::NONE {
            return false;
        }
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        match state.pending.iter().position(|(m, _)| *m == mark) {
            Some(at) => {
                state.pending.remove(at);
                true
            }
            None => false,
        }
    }

    /// The newest mark posted within [`INPUT_HOLD`] of `now` and not yet
    /// dispatched, and when it was posted; only one minted after the mark
    /// `after` counts (replayed input was typed before every mark up to it).
    fn pending(&self, now: Instant, after: u64) -> Option<(MenuMark, Instant)> {
        let state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        state
            .pending
            .last()
            .copied()
            .filter(|(mark, at)| mark.0 > after && now.saturating_duration_since(*at) < INPUT_HOLD)
    }

    /// The last mark given out (0 before the first).
    fn minted(&self) -> u64 {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).minted
    }
}

/// The App's marks, for the posts its native menu and toolbar make
/// ([`post_native_menu_action`]). Served once, by the windowed App that
/// installs them; unset (headless, every test) those posts go unmarked.
static NATIVE_MENU_MARKS: OnceLock<Arc<MenuMarks>> = OnceLock::new();

/// Serve `marks` to the native menu and toolbar's posts.
pub(crate) fn serve_native_menu_marks(marks: &Arc<MenuMarks>) {
    let _ = NATIVE_MENU_MARKS.set(Arc::clone(marks));
}

/// Post `action` as a [`Wake::MenuAction`] marked in `marks`, so input typed
/// before the loop dispatches it waits for it. `false` when the loop is gone.
pub(crate) fn post_menu_action(
    proxy: &EventLoopProxy<Wake>,
    marks: Option<&MenuMarks>,
    action: crate::menu::MenuAction,
) -> bool {
    let mark = marks.map_or(MenuMark::NONE, |marks| marks.mint(Instant::now()));
    let sent = proxy.send_event(Wake::MenuAction { action, mark }).is_ok();
    if !sent && let Some(marks) = marks {
        marks.settle(mark);
    }
    sent
}

/// [`post_menu_action`] for a native menu item or toolbar button, marked in
/// the marks the App served ([`serve_native_menu_marks`]).
#[cfg(target_os = "macos")]
pub(crate) fn post_native_menu_action(
    proxy: &EventLoopProxy<Wake>,
    action: crate::menu::MenuAction,
) -> bool {
    post_menu_action(proxy, NATIVE_MENU_MARKS.get().map(Arc::as_ref), action)
}

/// Hand `job` to the spawner thread, starting it on first use. The job comes
/// back when the thread could not start or has gone.
struct Spawner {
    tx: Mutex<Option<Sender<TabSpawnJob>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    stopping: Arc<AtomicBool>,
}

static SPAWNER: OnceLock<Option<Spawner>> = OnceLock::new();

fn send_to_spawner(job: TabSpawnJob) -> Option<TabSpawnJob> {
    let spawner = SPAWNER.get_or_init(|| {
        let (tx, rx) = channel::<TabSpawnJob>();
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        crate::qos::spawn_lane_thread("aterm-spawner", crate::qos::Role::Interactive, move || {
            // A person who pressed Cmd-T watches for the tab: the keystroke path.
            for job in rx {
                if worker_stopping.load(Ordering::Acquire) {
                    job.completion.cancel();
                } else {
                    run(job, FORK_PARK_WAIT);
                }
            }
        })
        .ok()
        .map(|worker| Spawner {
            tx: Mutex::new(Some(tx)),
            worker: Mutex::new(Some(worker)),
            stopping,
        })
    });
    match spawner {
        Some(spawner) => match spawner
            .tx
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            Some(tx) => tx.send(job).err().map(|refused| refused.0),
            None => {
                job.completion.cancel();
                Some(job)
            }
        },
        None => Some(job),
    }
}

/// Stop queue admission and take the one worker's join ownership. Called only
/// after the event loop ended; cancelling every InFlight slot precedes the join.
fn stop_spawner() -> Option<std::thread::JoinHandle<()>> {
    let spawner = SPAWNER.get()?.as_ref()?;
    spawner.stopping.store(true, Ordering::Release);
    spawner.tx.lock().unwrap_or_else(|p| p.into_inner()).take();
    spawner
        .worker
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
}

/// Spawn one tab's session and post it back. A panic is a failed spawn, never a
/// missing answer: the window's input waits for this one.
/// The status observer's own wake `next` while [`TabSpawns::status_sweep_held_until`]
/// holds the sweep until `held`: never before the hold's end, where the held
/// sweep runs. The sweep it skipped left its slots' observation instants as they
/// were, so `next` can already be past, and armed as it is every turn of the
/// hold would wake the loop at once, again and again, and the fork waiting for
/// the loop to park would wait out its whole bound.
pub(crate) fn held_status_wake(next: Option<Instant>, held: Option<Instant>) -> Option<Instant> {
    match (next, held) {
        (Some(wake), Some(at)) => Some(wake.max(at)),
        (wake, _) => wake,
    }
}

/// Run one spawn job; `park_wait` bounds how long its fork waits for the event
/// loop to park ([`FORK_PARK_WAIT`] on the spawner thread; zero when the job runs
/// inline in the loop's own turn, where no park can come until it returns).
fn run(job: TabSpawnJob, park_wait: Duration) {
    let TabSpawnJob {
        id,
        window,
        rows,
        cols,
        cell_px,
        factory,
        proxy,
        cwd,
        pane_folders,
        identity,
        identity_prep,
        observe,
        task,
        completion,
        adopt,
    } = job;
    if completion.cancelled() {
        return;
    }
    // An adopted spare (design §8 step 2): its rc generation and folder are
    // checked here, off the main thread, against the folder this spawn would
    // start in cold (a held pane's is checked here too); valid, it is handed
    // over at once, its reader already running, a parked one told the folder
    // it `cd`s to when it is elsewhere. Otherwise it goes (a warm one with its
    // whole session) and this spawn runs cold under the fallback id, in the
    // folder already resolved.
    let mut resolved = None;
    if let Some(adopt) = adopt {
        let AdoptSpare {
            id: spare_id,
            session,
            check,
            miss,
            retarget,
        } = *adopt;
        let want = cwd
            .clone()
            .or_else(|| crate::spawn_folder::PaneFolder::first_taken(pane_folders.iter().cloned()));
        match check.verdict(&session, want.as_deref(), factory.cwd.as_deref()) {
            Ok(dir) => {
                if let Some(dir) = dir {
                    let _ = retarget.set(dir);
                }
                publish_completion(
                    &completion,
                    TabSpawned {
                        id: spare_id,
                        window,
                        session: Ok(session),
                        reader_gate: None,
                    },
                    |wake| proxy.send_event(wake).is_ok(),
                );
                return;
            }
            Err(reason) => {
                let _ = miss.set(reason);
                if check.kill {
                    crate::spare::kill_session(session.pid);
                }
                drop(session);
                resolved = Some(want);
            }
        }
    }
    hold_for_test(&completion);
    if completion.cancelled() {
        return;
    }
    // The reader's start sits inside the same catch: a panic there would leave
    // the tab in flight forever, and its window's held input with it.
    let (session, reader_gate) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // A held pane's folder is checked here, where a hung volume holds only
        // this thread, for at most the check's budget per pane (once: a spare
        // that missed already resolved it).
        let cwd = resolved.unwrap_or_else(|| {
            cwd.or_else(|| crate::spawn_folder::PaneFolder::first_taken(pane_folders))
        });
        let session = state_answers()
            .and_then(|()| provision(identity.as_deref(), identity_prep))
            .and_then(|admission| {
                // Forked once the event loop parks, bounded, waited at the fork
                // itself (`watchdog::forks_wait_for_park`): the turn that booked
                // this tab and the sweep after it would otherwise wait out the
                // fork's malloc locks.
                let session = crate::watchdog::forks_wait_for_park(park_wait, || {
                    crate::spawn::spawn_session(
                        id,
                        window,
                        rows,
                        cols,
                        cell_px,
                        &factory,
                        &proxy,
                        cwd.as_deref(),
                        observe.as_ref(),
                        identity.as_deref(),
                        task.as_ref(),
                        None,
                    )
                });
                // Held until here: the session holds its identity itself from the
                // shell's own check on (`agent_identity::admit_existing`), so no
                // forget lands between this admission and the fork.
                drop(admission);
                session
            });
        park_reader(session, window, &proxy, &factory)
    }))
    .unwrap_or_else(|_| (Err(std::io::Error::other("the spawn panicked")), None));
    publish_completion(
        &completion,
        TabSpawned {
            id,
            window,
            session,
            reader_gate,
        },
        |wake| proxy.send_event(wake).is_ok(),
    );
}

/// Whether aterm's own folder ([`aterm_types::dirs::state_dir`]) answers, for
/// a spawn about to write there before its fork (the shell integration's
/// loader, the child's edge file), whose shell then starts from the home folder
/// it is in. On a network volume that stopped answering the spawn is refused
/// by name instead of holding this thread, and every tab asked after it, for
/// the mount's whole timeout: a home that stopped answering mid-run held New
/// Tab and each spawn after it until the server came back (a macOS 27 guest,
/// 2026-10-20). [`state_answers_with`] over this process's folder, judged
/// local once: a local one costs a spawn a load.
fn state_answers() -> std::io::Result<()> {
    static LOCAL: OnceLock<bool> = OnceLock::new();
    state_answers_with(
        aterm_types::dirs::state_dir().as_deref(),
        |state| *LOCAL.get_or_init(|| !crate::spawn_folder::on_network_volume(state)),
        crate::spawn_folder::volume_answers,
    )
}

/// [`state_answers`], PURE over the folder, whether its volume is local, and
/// whether a look at it answered ([`crate::spawn_folder::volume_answers`]),
/// asked only of a folder that is not local.
fn state_answers_with(
    state: Option<&std::path::Path>,
    local: impl FnOnce(&std::path::Path) -> bool,
    answers: impl FnOnce(&std::path::Path) -> bool,
) -> std::io::Result<()> {
    match state {
        Some(state) if !local(state) && !answers(state) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("{} did not answer", state.display()),
        )),
        _ => Ok(()),
    }
}

/// Ready agent identity `identity` for the spawn as `prep` says: admit it, or
/// make it first, and hand back the admission, which holds the identity
/// against `identities forget` while the spawner keeps it. With
/// [`IdentityPrep::Exists`] the spawn's own check that it exists is all.
fn provision(
    identity: Option<&str>,
    prep: IdentityPrep,
) -> std::io::Result<Option<agent_identity::Admission>> {
    provision_with(identity, prep, agent_identity::check_for_spawn)
}

/// [`provision`] through `admit` ([`crate::agent_identity::check_for_spawn`]
/// in a run; a test's own root and primer handoff).
fn provision_with(
    identity: Option<&str>,
    prep: IdentityPrep,
    admit: impl FnOnce(&str, SpawnAdmit) -> std::io::Result<agent_identity::Admission>,
) -> std::io::Result<Option<agent_identity::Admission>> {
    let Some(name) = identity else {
        return Ok(None);
    };
    match prep {
        IdentityPrep::Exists => Ok(None),
        IdentityPrep::Admit { inherited } => {
            let how = if inherited {
                SpawnAdmit::Inherited
            } else {
                SpawnAdmit::Named
            };
            admit(name, how).map(Some).map_err(|e| {
                std::io::Error::new(e.kind(), Refusal(format!("identity {name}: {e}")))
            })
        }
        IdentityPrep::Create => admit(name, SpawnAdmit::Named)
            .map(Some)
            .map_err(|e| std::io::Error::new(e.kind(), format!("identity {name}: {e}"))),
    }
}

fn publish_completion(
    completion: &Arc<TabSpawnCompletion>,
    done: TabSpawned,
    post: impl FnOnce(Wake) -> bool,
) {
    if !completion.store(done) {
        return; // Cancelled: the rejected Session dropped outside the lock.
    }
    if !post(Wake::TabSpawned(Arc::clone(completion))) {
        completion.cancel();
    }
}

/// Start `session`'s reader and helpers parked on a closed gate the main thread
/// opens at install, so not even the thread starts are left to the event loop.
#[cfg(unix)]
fn park_reader(
    session: std::io::Result<Session>,
    window: WindowId,
    proxy: &EventLoopProxy<Wake>,
    factory: &crate::spawn::SessionFactory,
) -> (
    std::io::Result<Session>,
    Option<crate::spawn::DeferredReaderGate>,
) {
    let gate = crate::spawn::DeferredReaderGate::closed();
    let session = session.and_then(|mut session| {
        crate::spawn::prepare_deferred_reader(&mut session, window, proxy, factory, &gate)
            .map_err(std::io::Error::other)?;
        Ok(session)
    });
    (session, Some(gate))
}

/// Off unix the main thread attaches the reader at install.
#[cfg(not(unix))]
fn park_reader(
    session: std::io::Result<Session>,
    _window: WindowId,
    _proxy: &EventLoopProxy<Wake>,
    _factory: &crate::spawn::SessionFactory,
) -> (
    std::io::Result<Session>,
    Option<crate::spawn::DeferredReaderGate>,
) {
    (session, None)
}

/// Dev seam `$ATERM_DEBUG_SPAWN_HOLD=<path>` (`aterm_types::dev_seam!`): before it
/// spawns, the spawner thread creates `<path>.held` and waits, up to a minute, for
/// `<path>` to exist, so a test can hold a tab's spawn in flight and watch the
/// event loop keep answering.
fn hold_for_test(completion: &TabSpawnCompletion) {
    let Some(path) = aterm_types::dev_seam!("ATERM_DEBUG_SPAWN_HOLD") else {
        return;
    };
    let mut held = path.clone();
    held.push(".held");
    let _ = std::fs::write(&held, b"");
    let path = std::path::PathBuf::from(path);
    let until = Instant::now() + Duration::from_secs(60);
    while !completion.cancelled() && !path.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl App {
    /// The final-exit seam for work not yet installed in the session pool.
    /// Runs after winit stops, before the final reap wait and keeper BYE. No
    /// worker can fork or publish another Session after it returns.
    pub(crate) fn shutdown_tab_spawns(&mut self) {
        let worker = stop_spawner();
        for pending in std::mem::take(&mut self.tab_spawns.in_flight) {
            pending.completion.cancel();
            pending
                .then
                .answer(self, pending.window, Err(QUITTING.to_string()));
        }
        self.tab_spawns.held.clear();
        self.tab_spawns.dropped.clear();
        self.tab_spawns.incoherent.clear();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }

    /// Open a new tab in `owner` with its shell spawned off the event loop: reserve
    /// the session id, remember the tab is in flight, and hand the spawn to the
    /// spawner thread. `cwd_override` is the folder the caller named (`None`
    /// inherits the focused pane's). Nothing to host it (no such window, no
    /// event loop) answers `then` at once.
    pub(crate) fn open_tab_off_loop(
        &mut self,
        owner: WindowId,
        cwd_override: Option<&str>,
        then: TabSpawnThen,
    ) {
        let ask = SpawnAsk::new(cwd_override, None);
        self.open_off_loop(owner, SpawnPlace::Tab, ask, then);
    }

    /// Split `owner`'s focused pane in `dir` with the new pane's shell spawned off
    /// the event loop, asked for as `ask` says. The caller has refused what
    /// cannot be split ([`App::split_refusal`]); the landing asks again, since
    /// the window may change while the shell starts.
    pub(crate) fn open_split_off_loop(
        &mut self,
        owner: WindowId,
        dir: crate::pane::SplitDir,
        ask: SpawnAsk,
        then: TabSpawnThen,
    ) {
        self.open_off_loop(owner, SpawnPlace::Split(dir), ask, then);
    }

    /// Book one spawn landing at `place` in `owner`, asked for as `ask` says.
    pub(crate) fn open_off_loop(
        &mut self,
        owner: WindowId,
        place: SpawnPlace,
        ask: SpawnAsk,
        then: TabSpawnThen,
    ) {
        if !self.windows.contains_key(&owner) {
            then.answer(
                self,
                owner,
                Err("no window to host the session".to_string()),
            );
            return;
        }
        // A real run always has a proxy; guard rather than panic (test-only None).
        let Some(proxy) = self.proxy.clone() else {
            then.answer(self, owner, Err("no event loop to spawn on".to_string()));
            return;
        };
        // A split spawns at the window grid too; its landing sizes every pane,
        // the newborn included, to its sub-rect.
        let grid = self.spawn_grid(owner);
        let parent = self.new_tab_parent(owner);
        self.book_off_loop(owner, place, grid, parent, ask, then, proxy);
    }

    /// Cmd-N, its keybinding action and [`crate::Wake::CreateWindow`]: book a
    /// new window now, in the turn that asked, so what is typed after the chord
    /// already waits for it. A replay of held keys stays in order with it: the
    /// keys after a replayed Cmd-N are held again for the window it booked.
    /// Headless keeps its one logical window and ignores it.
    pub(crate) fn open_new_window(&mut self) {
        if self.headless {
            aterm_log::info!("headless: ignoring a new window");
            return;
        }
        self.open_window_off_loop(SpawnAsk::default(), None, TabSpawnThen::Nothing);
    }

    /// Open a new window with its first shell spawned off the event loop: reserve
    /// the window's id and the session's, and hand the spawn to the spawner
    /// thread; the window is made when the session lands. It takes the front
    /// window's grid and its focused pane's folder, as Cmd-N always did, and
    /// what is typed into the front window meanwhile waits for it.
    /// `lineage_parent` is a controlled window's origin. No event loop to spawn
    /// on says so and answers `then` at once.
    pub(crate) fn open_window_off_loop(
        &mut self,
        ask: SpawnAsk,
        lineage_parent: Option<aterm_session::SessionId>,
        then: TabSpawnThen,
    ) {
        let (window, place, grid) = self.reserve_window();
        // A real run always has a proxy; guard rather than panic (test-only None).
        let Some(proxy) = self.proxy.clone() else {
            let why = place.failed(self, "no event loop to host the new window");
            then.answer(self, window, Err(why));
            return;
        };
        self.book_off_loop(window, place, grid, lineage_parent, ask, then, proxy);
    }

    /// A new window's booking: its id, minted now so the session's wakes are
    /// stamped with the window that will own them (a spawn that fails burns it;
    /// ids are monotonic, not dense); its place, asked from the front window;
    /// and the grid it is made at, the front window's.
    fn reserve_window(&mut self) -> (WindowId, SpawnPlace, SpawnGrid) {
        let place = SpawnPlace::Window {
            from: self.frontmost_window,
        };
        let window = WindowId(self.next_window_id);
        self.next_window_id += 1;
        // `window` is not in `self.windows` yet, so the cell box is the shared
        // backend's; `apply_window_scale` corrects it after the attach, as for
        // a window built in the turn.
        let ((rows, cols), _) = self.new_window_frame(None);
        (window, place, (rows, cols, self.spawn_cell_px(window)))
    }

    /// Book one spawn landing at `place` in `window` at `grid`: reserve the
    /// session id, remember it in flight and hand it to the spawner thread.
    #[allow(
        clippy::too_many_arguments,
        reason = "where the spawn lands, at what grid, under which parent, what it \
                  asks for, who waits and the loop to post to are each a per-spawn \
                  datum; a wrapper struct would relocate the list, not simplify it"
    )]
    fn book_off_loop(
        &mut self,
        window: WindowId,
        place: SpawnPlace,
        grid: SpawnGrid,
        parent: Option<aterm_session::SessionId>,
        ask: SpawnAsk,
        then: TabSpawnThen,
        proxy: EventLoopProxy<Wake>,
    ) {
        // THE WARM SPARE (design §7.3, §8): a plain ask whose gates all pass
        // takes the spare, keeping its id; the cold spawn below is its fallback,
        // under a fresh id. A miss is counted and the cold path runs unchanged.
        // A split of a pane with its own theme inherits it (`split_theme`),
        // decided here once: a spare's already-running reader would miss it,
        // so such a split takes no spare, and the landing applies exactly what
        // was decided now, whatever the focus is by then. A worker's spawn
        // (`spawn agent=claude`) takes none either: its task file is written
        // once its own sid is known and named to its shell as
        // `ATERM_TASK_FILE`, which a spare's shell, already running, never saw.
        let inherited = self.inherited_theme(window, place, ask.theme.as_ref());
        let tried = if inherited.is_some() {
            Err("theme")
        } else if ask.task.is_some() {
            Err("ask")
        } else {
            self.try_adopt_spare(window, place.asked_from(window), grid, &ask)
        };
        let adopted = match tried {
            Ok(adopted) => Some(adopted),
            Err(reason) => {
                self.spares.note_miss(reason);
                self.publish_spare();
                None
            }
        };
        let SpawnAsk {
            admission,
            cwd,
            folder_of,
            identity,
            identity_prep,
            observe,
            task,
            background,
            theme,
        } = ask;
        let theme = theme.or(inherited);
        // A queued shell is not in an overlap's manifest, even before its
        // completion wake can arrive and invalidate that capture.
        self.note_update_handoff_activity();
        let id = self.next_session_id;
        self.next_session_id += 1;
        let (rows, cols, cell_px) = grid;
        let mut colors = self.engine_color_fanouts;
        let (inflight_id, alt_id, adopt) = match adopted {
            Some((slot, session)) => {
                // The spare's engine was built with the colours of its booking:
                // the landing catches it up as for any tab in flight.
                colors = slot.colors;
                let miss = Arc::new(OnceLock::new());
                let retarget = Arc::new(OnceLock::new());
                let warm = slot.state.warm();
                let now = Instant::now();
                self.spares.transit.push(crate::spare::Transit {
                    id: slot.id,
                    link: Arc::clone(&slot.link),
                    tier: slot.state,
                    grid: slot.grid,
                    age: now.saturating_duration_since(slot.booked),
                    rc_ms: slot
                        .link
                        .witness
                        .start_seen
                        .get()
                        .and_then(|seen| {
                            session
                                .shell_start
                                .as_ref()
                                .map(|st| seen.at.saturating_duration_since(st.at))
                        })
                        .map(|d| d.as_millis() as u64),
                    inputs: slot.inputs.clone(),
                    miss: Arc::clone(&miss),
                    retarget: Arc::clone(&retarget),
                });
                let adopt = AdoptSpare {
                    id: slot.id,
                    session,
                    check: crate::spare::AdoptCheck {
                        cwd: slot.cwd.clone(),
                        generation: warm.then(|| (slot.inputs.clone(), slot.generation)),
                        link: Arc::clone(&slot.link),
                        kill: warm && slot.state != crate::spare::SlotState::Ready,
                        retarget: slot.state == crate::spare::SlotState::Parked
                            && self.spare_retargets(slot.inputs.shell),
                    },
                    miss,
                    retarget,
                };
                self.publish_spare();
                (slot.id, Some(id), Some(Box::new(adopt)))
            }
            None => (id, None, None),
        };
        // Inherit the focused pane's cwd so a new tab, pane or window opens
        // where the user is, unless the caller overrode it or named the pane to
        // inherit from; a held pane's is checked on the spawner. A window
        // inherits from the window it was asked from.
        let pane_folders = if cwd.is_none() {
            folder_of
                .into_iter()
                .chain(
                    place
                        .asked_from(window)
                        .and_then(|from| self.focused_pane_folder(from)),
                )
                .collect()
        } else {
            Vec::new()
        };
        let mut factory = self.session_factory.clone();
        factory.reader_after_install = true;
        // The shell's `COLORFGBG` names the side of the theme this session will
        // draw, which lands only after the shell has started.
        #[cfg(windows)]
        {
            factory.shell_appearance = self.spawn_shell_appearance(
                theme.as_ref().map(|theme| theme.selector.as_str()),
                identity.as_deref(),
            );
        }
        let completion = Arc::new(TabSpawnCompletion::default());
        self.tab_spawns.in_flight.push(InFlight {
            id: inflight_id,
            alt_id,
            window,
            place,
            asked: Instant::now(),
            parent,
            colors,
            grid,
            then,
            admission,
            background,
            completion: Arc::clone(&completion),
            theme,
        });
        let job = TabSpawnJob {
            id,
            window,
            rows,
            cols,
            cell_px,
            factory,
            proxy,
            cwd,
            pane_folders,
            identity,
            identity_prep,
            observe,
            task,
            completion,
            adopt,
        };
        if let Some(job) = send_to_spawner(job) {
            // No spawner thread: spawn in this turn, as before, and land it as
            // usual; the fork waits for no park, which this turn is holding off.
            run(job, Duration::ZERO);
        }
    }

    /// [`crate::Wake::TabSpawned`]: install the tab, pane or window, answer for it,
    /// then replay what was typed for it meanwhile (or drop it, if nothing landed).
    pub(crate) fn land_tab_spawn(
        &mut self,
        el: &ActiveEventLoop,
        completion: Arc<TabSpawnCompletion>,
    ) {
        let Some(done) = completion.take() else {
            return;
        };
        let window = done.window;
        // The in-flight id held input waits under: an adopted spare's fallback
        // lands under its alternate id.
        let spawn = self
            .tab_spawns
            .in_flight
            .iter()
            .find(|t| t.id == done.id || t.alt_id == Some(done.id))
            .map(|t| t.id);
        let landed = self.install_spawned(done, el);
        let (replay, repair) = self.settle_landed_spawn_input(window, spawn, landed);
        self.replay_settled_input(el, replay, repair);
    }

    /// [`Self::settle_tab_spawn_input`] for the spawn `spawn` that just landed
    /// (or failed) in `window`. While another spawn is still in flight to that
    /// window, what was typed while this one was the newest (and so is for it)
    /// settles now, into the tab that just took the window's focus, and what
    /// waits for the later one waits on: a Cmd-T, `ls`, Cmd-T runs `ls` in the
    /// first new tab, as it does when nothing is in flight. The window's
    /// refused keystrokes and modifier repair settle with its last spawn.
    pub(crate) fn settle_landed_spawn_input(
        &mut self,
        window: WindowId,
        spawn: Option<u64>,
        landed: bool,
    ) -> (Vec<(WinitWindowId, WindowEvent)>, Option<WindowId>) {
        let spawns = &mut self.tab_spawns;
        let Some(spawn) = spawn.filter(|_| spawns.in_flight.iter().any(|t| t.window == window))
        else {
            return self.settle_tab_spawn_input(window, landed);
        };
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut spawns.held)
            .into_iter()
            .partition(|h| h.target == window && h.spawn == Some(spawn));
        spawns.held = rest;
        let through = self
            .winit_to_window
            .iter()
            .find(|(_, w)| **w == window)
            .map(|(id, _)| *id)
            .filter(|_| landed);
        let mine = if landed {
            self.without_strip_presses(mine)
        } else {
            mine
        };
        let (replay, dropped) = sift_held(mine, landed, through, Instant::now());
        if dropped > 0 {
            aterm_log::warn!(
                "{dropped} key(s) typed while a new tab was starting were dropped ({})",
                if landed {
                    "it started past the hold"
                } else {
                    "it never started"
                }
            );
            self.post_message(crate::message_reporters::keystrokes_dropped(dropped));
        }
        (replay, None)
    }

    /// Replay what settled for a spawn ([`Self::settle_tab_spawn_input`],
    /// [`Self::expire_tab_spawn_input`]) through the event path, in order, then
    /// clear the modifier snapshot of the window `repair` names. It was typed
    /// before every spawn and menu command still pending, so it waits for none
    /// of them again ([`ReplayFloor`]): only for what it books itself.
    pub(crate) fn replay_settled_input(
        &mut self,
        el: &ActiveEventLoop,
        replay: Vec<(WinitWindowId, WindowEvent)>,
        repair: Option<WindowId>,
    ) {
        self.replaying(true, |app| {
            for (winit_id, event) in replay {
                app.window_event(el, winit_id, event);
            }
        });
        if let Some(ws) = repair.and_then(|at| self.windows.get_mut(&at)) {
            ws.mods = ModifiersState::empty();
        }
    }

    /// Run `replay` with held input held again only for what is booked or
    /// posted from now on: every menu mark minted so far and (`spawns`) every
    /// booking before the next session id passed over ([`ReplayFloor`]). The
    /// floor before it is put back after, so a replay inside a replay keeps its own.
    fn replaying<R>(&mut self, spawns: bool, replay: impl FnOnce(&mut Self) -> R) -> R {
        let floor = ReplayFloor {
            marks: self.tab_spawns.menu.minted(),
            spawns: spawns.then_some(self.next_session_id),
        };
        let was = self.tab_spawns.floor.replace(floor);
        let out = replay(self);
        self.tab_spawns.floor = was;
        out
    }

    /// Post `action` as this App's marked [`Wake::MenuAction`]
    /// ([`post_menu_action`]): the palette's, the tab strip's and the key
    /// path's own posts. `false` with no loop to post to.
    pub(crate) fn post_menu_action(&self, action: crate::menu::MenuAction) -> bool {
        self.proxy
            .as_ref()
            .is_some_and(|proxy| post_menu_action(proxy, Some(&self.tab_spawns.menu), action))
    }

    /// [`Wake::MenuAction`]: dispatch `action`, then settle the input that
    /// waited for its post `mark` ([`Self::claim_menu_held_input`]) and replay
    /// what goes on as typed.
    pub(crate) fn dispatch_marked_menu_action(
        &mut self,
        el: &ActiveEventLoop,
        action: crate::menu::MenuAction,
        mark: MenuMark,
    ) {
        let booked = self.tab_spawns.in_flight.len();
        self.dispatch_menu_action(el, action);
        if self.tab_spawns.menu.settle(mark) {
            let release = self.claim_menu_held_input(mark, booked);
            // Typed before any mark still pending, and after every spawn
            // booked before this command was posted, which it may be for.
            self.replaying(false, |app| {
                for (winit_id, event) in release {
                    app.window_event(el, winit_id, event);
                }
            });
        }
    }

    /// The input that waited for menu post `mark`, once it is dispatched. Where
    /// the dispatch booked a spawn (in flight past index `booked`) taking the
    /// focus of the window the input was typed into, the input waits for that
    /// spawn as if typed after its booking: a tab's or split's all of it, a new
    /// window's its keys and modifiers (which also reach the window typed into
    /// now, as [`Self::defer_tab_spawn_input`] hands them over), while pointer
    /// input goes on to the window it points at. The rest goes on as typed:
    /// returned, in order, to be handled now.
    pub(crate) fn claim_menu_held_input(
        &mut self,
        mark: MenuMark,
        booked: usize,
    ) -> Vec<(WinitWindowId, WindowEvent)> {
        let spawns = &mut self.tab_spawns;
        if !spawns.held.iter().any(|h| h.menu == mark) {
            return Vec::new();
        }
        // Each spawn the dispatch booked: the window it was asked from, the
        // window it lands in and when input held for it stops waiting.
        let fresh: Vec<(Option<WindowId>, WindowId, Instant, u64)> = spawns
            .in_flight
            .get(booked..)
            .unwrap_or_default()
            .iter()
            .map(|t| {
                (
                    t.place.asked_from(t.window),
                    t.window,
                    t.asked + INPUT_HOLD,
                    t.id,
                )
            })
            .collect();
        let mut release = Vec::new();
        let mut kept = Vec::with_capacity(spawns.held.len());
        for mut h in std::mem::take(&mut spawns.held) {
            if h.menu != mark {
                kept.push(h);
                continue;
            }
            let typed = self.winit_to_window.get(&h.typed_into).copied();
            let spawn = fresh
                .iter()
                .rev()
                .find(|(from, ..)| typed.is_some() && *from == typed);
            let Some(&(_, target, until, id)) = spawn else {
                release.push((h.typed_into, h.event));
                continue;
            };
            if Some(target) != typed {
                if is_pointer(&h.event) {
                    release.push((h.typed_into, h.event));
                    continue;
                }
                if matches!(h.event, WindowEvent::ModifiersChanged(_)) {
                    release.push((h.typed_into, h.event.clone()));
                    h.applied = true;
                }
            }
            h.target = target;
            h.spawn = Some(id);
            h.until = until;
            h.menu = MenuMark::NONE;
            kept.push(h);
        }
        spawns.held = kept;
        release
    }

    /// [`Self::install_spawned`] with the inert OS arm: a window lands logical,
    /// as headless keeps it.
    #[cfg(test)]
    pub(crate) fn install_spawned_tab(&mut self, done: TabSpawned) -> bool {
        self.install_spawned(done, &test_support::InertOs::ATTACHES)
    }

    /// The install half of [`Self::land_tab_spawn`]: install and register the
    /// tab, pane or window, start its reader, answer its waiter. `false` when
    /// nothing landed: the spawn failed (said through `new_tab_failed`,
    /// `split_failed` or `new_window_failed`) or was refused before its fork
    /// (said only to its waiter), its window closed, its split is
    /// refused now, a new window's OS window could not be made, or the loop is
    /// exiting (the session drops, and its teardown stops the parked reader and
    /// hangs up and reaps the shell). `os` is the event loop in a run.
    pub(crate) fn install_spawned(&mut self, done: TabSpawned, os: &dyn LandingOs) -> bool {
        let TabSpawned {
            id,
            window,
            session,
            reader_gate,
        } = done;
        // An adopted spare's fallback lands under its alternate id: the spare
        // is retired for good, and the miss the spawner found is counted.
        if let Some(t) = self
            .tab_spawns
            .in_flight
            .iter()
            .find(|t| t.alt_id == Some(id))
            && let Some(at) = self.spares.transit.iter().position(|tr| tr.id == t.id)
        {
            let transit = self.spares.transit.remove(at);
            self.spares
                .note_miss(transit.miss.get().copied().unwrap_or("stale"));
            self.spares.tombstone_public(transit.id);
            self.publish_spare();
        }
        // An adopted spare lands under its own id with its reader running, at
        // the grid it was spawned at.
        let spare_grid = self
            .spares
            .transit
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.grid);
        let adopted_spare = spare_grid.is_some();
        let (place, parent, colors, grid, then, admission, theme, background) = match self
            .tab_spawns
            .in_flight
            .iter()
            .position(|t| t.id == id || t.alt_id == Some(id))
        {
            Some(at) => {
                let t = self.tab_spawns.in_flight.remove(at);
                (
                    t.place,
                    t.parent,
                    Some(t.colors),
                    Some(t.grid),
                    t.then,
                    t.admission,
                    t.theme,
                    t.background,
                )
            }
            None => (
                SpawnPlace::Tab,
                None,
                None,
                None,
                TabSpawnThen::Nothing,
                SpawnAdmission::Plain,
                None,
                false,
            ),
        };
        let new_window = matches!(place, SpawnPlace::Window { .. });
        let admission_plain = matches!(admission, SpawnAdmission::Plain);
        let outcome = match session {
            // A quit (or the last window's close) overtook it: as the quit's
            // cancel would, the session drops with its reader still parked.
            _ if os.exiting() => Err(QUITTING.to_string()),
            // Refused before the fork: in the request's own words, no row.
            Err(e) => Err(refusal(&e).unwrap_or_else(|| place.failed(self, &e.to_string()))),
            Ok(_) if !new_window && !self.windows.contains_key(&window) => {
                Err("the window closed before the session started".to_string())
            }
            Ok(mut session) => 'land: {
                // A new window needs only its origin alive; a tab must still be
                // beside it.
                let host = (!new_window).then_some(window);
                if let Err(why) = self.validate_spawn_admission(&admission, host) {
                    break 'land Err(why);
                }
                // The pane a split divides is the window's focused one NOW, judged
                // as at asking: the window may have changed while the shell
                // started. Refused, the session drops with its reader still parked.
                if let SpawnPlace::Split(dir) = place
                    && let Err(why) = self.split_refusal(window, dir)
                {
                    break 'land Err(why);
                }
                // Parked on the spawner, or attached here (off unix). A real run
                // always has a proxy (test-only None: a stub has no reader). An
                // adopted spare's reader is already running: it gets the cold
                // path's own session wiring instead (design §8 step 4).
                let attached = match (&reader_gate, self.proxy.clone()) {
                    _ if adopted_spare => {
                        self.prepare_adopted_spare(&mut session, window);
                        Ok(())
                    }
                    (None, Some(proxy)) => crate::spawn::attach_reader(
                        &mut session,
                        window,
                        &proxy,
                        &self.session_factory,
                    ),
                    _ => Ok(()),
                };
                match attached {
                    Ok(()) => {
                        if let Some(colors) = colors {
                            self.catch_up_colors(&session, colors);
                        }
                        // `theme` is what booking decided: asked, or for a
                        // split the divided pane's own as it was then.
                        let sid = session.ctx.self_id.as_str().to_string();
                        let installed = match place {
                            SpawnPlace::Tab => {
                                // A background tab lands behind the tab active
                                // NOW, which is selected again once it is in.
                                let kept = background
                                    .then(|| self.windows.get(&window)?.tab_set.active_index())
                                    .flatten();
                                self.install_tab_session(window, session, parent);
                                // The window re-gridded (a resize, a font zoom)
                                // while the tab was in flight: those passes sized
                                // only the panes it had, so size the newborn's
                                // now, before its reader reads.
                                if grid.is_some_and(|grid| grid != self.spawn_grid(window)) {
                                    self.resize_panes(window);
                                }
                                if let Some(index) = kept {
                                    self.switch_tab_in(window, index);
                                }
                                Ok(())
                            }
                            // The install sizes every pane, the newborn included,
                            // before its reader reads.
                            SpawnPlace::Split(dir) => {
                                self.install_split_session(window, dir, session)
                            }
                            SpawnPlace::Window { .. } => {
                                self.install_window_session(window, session, parent, grid, os)
                            }
                        };
                        // Its own theme on the engine before its reader runs.
                        if installed.is_ok() && theme.is_some() {
                            self.set_session_theme(id, theme);
                        }
                        // Registered: the reader may tell the loop about it now.
                        // Not installed, the session has dropped with its gate shut.
                        // An adopted spare goes live and is told `go` (`crate::spare`);
                        // a cold tab's visible rc run is one the spare's gate learns from.
                        installed.map(|()| {
                            if let Some(gate) = reader_gate {
                                gate.release();
                            }
                            if adopted_spare {
                                // Sized before its release, so its first prompt
                                // is drawn at the grid it lands at; a tab whose
                                // window grid is the spare's has nothing to size.
                                if !matches!(place, SpawnPlace::Tab)
                                    || spare_grid != Some(self.spawn_grid(window))
                                {
                                    self.resize_panes(window);
                                }
                                self.adopt_live(id, window);
                            } else if admission_plain {
                                let inputs = self.spare_gen_inputs();
                                self.observe_spare_run(id, inputs);
                            }
                            self.spare_poke();
                            sid
                        })
                    }
                    Err(e) => Err(place.failed(self, &e)),
                }
            }
        };
        let landed = outcome.is_ok();
        // An adopted spare that did not land (its window closed, a split now
        // refused, the loop exiting) dropped with the session: it is retired.
        if adopted_spare
            && !landed
            && let Some(at) = self.spares.transit.iter().position(|t| t.id == id)
        {
            let transit = self.spares.transit.remove(at);
            self.spares.tombstone_public(transit.id);
        }
        then.answer(self, window, outcome);
        landed
    }

    /// Build window `wid` around its first `session`, landed: install it at the
    /// grid the shell was spawned at, which is the window's grid by
    /// construction, frontmost, with the session registered under `parent`,
    /// then give it its OS window. A surface that cannot be made rolls the
    /// window and its session back and says so, as the window built in the
    /// turn did.
    ///
    /// Refines `WindowRouting.CreateWindow` (Cmd-N, the menu, a picker row) and
    /// `ConnectedCreateWindow` (`spawn connected= place=window`, whose
    /// hosted-origin precondition [`App::connected_spawn_precheck`] enforces)
    /// TOGETHER WITH its booking, [`App::reserve_window`], which minted the id:
    /// `window_routing_conformance` validates the pair as one transition.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "window_routing",
            action = "CreateWindow",
            project = "aterm_gui::App::project_window_routing"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "window_routing",
            action = "ConnectedCreateWindow",
            project = "aterm_gui::App::project_window_routing"
        )
    )]
    fn install_window_session(
        &mut self,
        wid: WindowId,
        session: Session,
        parent: Option<aterm_session::SessionId>,
        grid: Option<SpawnGrid>,
        os: &dyn LandingOs,
    ) -> Result<(), String> {
        let (rows, cols) = grid.map_or_else(
            || self.new_window_frame(None).0,
            |(rows, cols, _)| (rows, cols),
        );
        self.install_window_state_with_parent(wid, session, rows, cols, parent);
        if !os.attach(self, wid) {
            // GPU surface failed: no present-less black window.
            self.close_window_logical(wid);
            self.post_message(crate::message_reporters::new_window_failed(
                "the window could not be opened",
            ));
            return Err(NO_NEW_WINDOW.to_string());
        }
        // Again now that the OS surface exists, so the title lands at once.
        self.sync_active_session();
        Ok(())
    }

    /// The theme a spawn booked at `place` in `window` inherits when it asks
    /// for none (`asked`): a split's [`Self::split_theme`], judged at
    /// booking; a tab or a window inherits none.
    pub(crate) fn inherited_theme(
        &self,
        window: WindowId,
        place: SpawnPlace,
        asked: Option<&crate::session_theme::SessionTheme>,
    ) -> Option<crate::session_theme::SessionTheme> {
        match place {
            SpawnPlace::Split(_) if asked.is_none() => self.split_theme(window),
            SpawnPlace::Split(_) | SpawnPlace::Tab | SpawnPlace::Window { .. } => None,
        }
    }

    /// The theme a split of `window`'s focused pane inherits: that pane's own
    /// theme, as [`crate::session_theme::SessionThemeOrigin::Split`]; `None`
    /// when it has none (an identity theme is not carried: it resolves from
    /// the newborn's own identity).
    pub(crate) fn split_theme(
        &self,
        window: WindowId,
    ) -> Option<crate::session_theme::SessionTheme> {
        let focused = self.focused_session_id(window)?;
        // A Choose Tab Theme… highlight is tentative: the prior is inherited.
        self.kept_session_theme(focused)
            .map(|theme| crate::session_theme::SessionTheme {
                selector: theme.selector.clone(),
                origin: crate::session_theme::SessionThemeOrigin::Split,
            })
    }

    /// `window`'s grid and cell box as a spawn takes them now.
    pub(crate) fn spawn_grid(&self, window: WindowId) -> SpawnGrid {
        let (rows, cols) = self
            .windows
            .get(&window)
            .map_or((0, 0), |ws| (ws.rows, ws.cols));
        (rows, cols, self.spawn_cell_px(window))
    }

    /// Bring a newborn engine up to the colors installed while its tab was in
    /// flight: `App::install_colors` reaches only the sessions in the pool, and
    /// the engine was built from the factory as it was when the tab was asked
    /// for (`asked`, the engine-config generation). The projection carries the
    /// scheme baseline, so one `apply_config` catches up both. A new session
    /// has no DEC 2031 subscriber yet, so no report is owed.
    fn catch_up_colors(&self, session: &Session, asked: u64) {
        if asked == self.engine_color_fanouts {
            return;
        }
        if let Some(tc) = &self.session_factory.terminal_config {
            let mut term = crate::term_lock(&session.term);
            term.apply_config(tc);
            session.ctx.crash_journal.host_settings(&term, Some(tc));
        }
    }

    /// Hold `event`, typed into `wid`, for the spawn in flight that takes `wid`'s
    /// focus, if one does (asked for within [`INPUT_HOLD`]): a tab or split in
    /// `wid`, or a new window asked from it. Keys, modifiers and IME, the
    /// pre-Commit queue's set, and the pointer ([`is_pointer`]) for a tab or
    /// split, which lands under it; a new window leaves the pointer to the
    /// window it points at, and a modifier change held for it reaches that
    /// window now as well, so a click there is not read under a chord already
    /// released. While a menu command posted earlier is not yet dispatched
    /// ([`MenuMarks`]), the event waits for it instead, under `wid`
    /// ([`Self::claim_menu_held_input`]). `Some(event)` hands it back to be
    /// handled now.
    pub(crate) fn defer_tab_spawn_input(
        &mut self,
        wid: WindowId,
        id: WinitWindowId,
        event: WindowEvent,
    ) -> Option<WindowEvent> {
        let pointer = is_pointer(&event);
        if !pointer && !crate::handoff_deferrable_input(&event) {
            return Some(event);
        }
        let now = Instant::now();
        let spawns = &mut self.tab_spawns;
        let after = spawns.floor.map_or(0, |floor| floor.marks);
        let (target, until, menu, spawn) = match spawns.menu.pending(now, after) {
            Some((mark, posted)) => (wid, posted + INPUT_HOLD, mark, None),
            None => match spawns.hold_target(wid, now) {
                Some((target, until, spawn)) => (target, until, MenuMark::NONE, Some(spawn)),
                None => return Some(event),
            },
        };
        if pointer && target != wid {
            return Some(event);
        }
        let now_too = (target != wid && matches!(event, WindowEvent::ModifiersChanged(_)))
            .then(|| event.clone());
        spawns.hold(Held {
            typed_into: id,
            target,
            spawn,
            event,
            until,
            menu,
            applied: now_too.is_some(),
        });
        now_too
    }

    /// Input typed into `typed_into` whose hold ended by `now`
    /// ([`Held::until`]): settled before the window's next input is handled,
    /// as for a spawn that never landed ([`sift_held`]), so nothing typed later
    /// reaches the tab the window shows while earlier keys still wait for one
    /// that may land. A target left holding nothing settles its refused
    /// keystrokes and modifier repair with it. Returns the events to replay
    /// and the window to repair after them, as
    /// [`Self::settle_tab_spawn_input`] does; nothing (one scan) when nothing
    /// expired.
    pub(crate) fn expire_tab_spawn_input(
        &mut self,
        typed_into: WinitWindowId,
        now: Instant,
    ) -> (Vec<(WinitWindowId, WindowEvent)>, Option<WindowId>) {
        let spawns = &mut self.tab_spawns;
        let expired = |h: &Held| h.typed_into == typed_into && h.until <= now;
        if !spawns.held.iter().any(expired) {
            return (Vec::new(), None);
        }
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut spawns.held)
            .into_iter()
            .partition(expired);
        spawns.held = kept;
        let typed = self.winit_to_window.get(&typed_into).copied();
        let mut dropped = 0u32;
        let mut repair = None;
        let mut targets: Vec<WindowId> = Vec::new();
        for h in &gone {
            if !targets.contains(&h.target) {
                targets.push(h.target);
            }
        }
        for target in targets {
            if spawns.held.iter().any(|h| h.target == target) {
                continue;
            }
            if let Some(at) = spawns.dropped.iter().position(|(w, _)| *w == target) {
                dropped = dropped.saturating_add(spawns.dropped.remove(at).1);
            }
            if let Some(at) = spawns.incoherent.iter().position(|w| *w == target) {
                spawns.incoherent.remove(at);
                repair = typed;
            }
        }
        let (replay, keys) = sift_held(gone, false, None, now);
        let dropped = dropped.saturating_add(keys);
        if dropped > 0 {
            aterm_log::warn!(
                "{dropped} key(s) typed while a new tab or window was starting were dropped \
                 (it had not started {} s after it was asked for)",
                INPUT_HOLD.as_secs()
            );
            self.post_message(crate::message_reporters::keystrokes_dropped(dropped));
        }
        (replay, repair)
    }

    /// `held`, bound for a spawn that landed, without the presses made on its
    /// window's in-grid tab strip and their releases. The landing gave the
    /// strip a chip, and its tabs share the band equally, so every chip moved:
    /// replayed, such a press would hit whatever is under it now, a `✕` the
    /// person never aimed at among them. Dropped, the click is simply not made
    /// (as on macOS, whose strip is native and never held, it was for what the
    /// strip showed). Where each press was is where the moves held before it
    /// left the pointer, else where the window last saw it.
    fn without_strip_presses(&self, held: Vec<Held>) -> Vec<Held> {
        if !self.tab_strip_enabled() {
            return held;
        }
        let mut at: Vec<(WinitWindowId, (f64, f64))> = Vec::new();
        let mut pressed: Vec<(WinitWindowId, aterm_window::event::MouseButton)> = Vec::new();
        held.into_iter()
            .filter(|h| match &h.event {
                WindowEvent::CursorMoved { position, .. } => {
                    match at.iter_mut().find(|(id, _)| *id == h.typed_into) {
                        Some((_, xy)) => *xy = (position.x, position.y),
                        None => at.push((h.typed_into, (position.x, position.y))),
                    }
                    true
                }
                WindowEvent::MouseInput { state, button, .. } if state.is_pressed() => {
                    let Some(wid) = self.winit_to_window.get(&h.typed_into).copied() else {
                        return true;
                    };
                    let (x, y) = at
                        .iter()
                        .find(|(id, _)| *id == h.typed_into)
                        .map(|(_, xy)| *xy)
                        .or_else(|| self.windows.get(&wid).map(|ws| ws.last_cursor_px))
                        .unwrap_or_default();
                    let on_strip = self.strip_col_at(wid, x, y).is_some();
                    if on_strip {
                        pressed.push((h.typed_into, *button));
                    }
                    !on_strip
                }
                WindowEvent::MouseInput { button, .. } => {
                    match pressed.iter().position(|p| *p == (h.typed_into, *button)) {
                        Some(i) => {
                            pressed.remove(i);
                            false
                        }
                        None => true,
                    }
                }
                _ => true,
            })
            .collect()
    }

    /// What the input held for `wid` becomes once nothing is in flight to it
    /// ([`sift_held`]): every event, in order, when the last spawn `landed`
    /// within its hold, through `wid`'s own OS window (a new window's input
    /// was typed into the one it was asked from), the pointer where it was
    /// typed; else only what keeps the window it was typed into coherent (its
    /// modifier changes, the pointer's position). Keys dropped either way are
    /// said in one row.
    /// Returns the events to replay and the window whose modifiers need a
    /// repair after them, if any: the one the replay goes through, so a new
    /// window that never opened repairs the window its modifier changes replay
    /// back into. Nothing while another spawn is still in flight to `wid`, and
    /// input still waiting for a menu command's dispatch waits on.
    pub(crate) fn settle_tab_spawn_input(
        &mut self,
        wid: WindowId,
        landed: bool,
    ) -> (Vec<(WinitWindowId, WindowEvent)>, Option<WindowId>) {
        let through = self
            .winit_to_window
            .iter()
            .find(|(_, w)| **w == wid)
            .map(|(id, _)| *id)
            .filter(|_| landed);
        let spawns = &mut self.tab_spawns;
        if spawns.in_flight.iter().any(|t| t.window == wid) {
            return (Vec::new(), None);
        }
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut spawns.held)
            .into_iter()
            .partition(|h| h.target == wid && h.menu == MenuMark::NONE);
        spawns.held = rest;
        let dropped = spawns
            .dropped
            .iter()
            .position(|(w, _)| *w == wid)
            .map_or(0, |at| spawns.dropped.remove(at).1);
        let repair = match spawns.incoherent.iter().position(|w| *w == wid) {
            Some(at) => {
                spawns.incoherent.remove(at);
                // Every event held for `wid` was typed into one window: `wid`
                // itself, or the window a new one was asked from.
                match mine.first() {
                    Some(h) if !landed => Some(
                        self.winit_to_window
                            .get(&h.typed_into)
                            .copied()
                            .unwrap_or(wid),
                    ),
                    _ => Some(wid),
                }
            }
            None => None,
        };
        let mine = if landed {
            self.without_strip_presses(mine)
        } else {
            mine
        };
        let now = Instant::now();
        let late = landed && mine.iter().any(|h| h.until <= now);
        let (replay, keys) = sift_held(mine, landed, through, now);
        let dropped = dropped.saturating_add(keys);
        if dropped > 0 {
            aterm_log::warn!(
                "{dropped} key(s) typed while a new tab or window was starting were dropped ({})",
                if !landed {
                    "it never started"
                } else if late {
                    "it started past the hold"
                } else {
                    "past what the window holds"
                }
            );
            self.post_message(crate::message_reporters::keystrokes_dropped(dropped));
        }
        (replay, repair)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Arc;

    /// Book a tab in flight to `wid` the way [`App::open_tab_off_loop`] does,
    /// without a spawner: the test plays the spawner's part.
    #[cfg(unix)]
    pub(crate) fn book(app: &mut App, wid: WindowId, then: TabSpawnThen) -> u64 {
        let id = app.next_session_id;
        app.next_session_id += 1;
        let parent = app.new_tab_parent(wid);
        let colors = app.engine_color_fanouts;
        let grid = app.spawn_grid(wid);
        app.tab_spawns.in_flight.push(InFlight {
            id,
            alt_id: None,
            window: wid,
            place: SpawnPlace::Tab,
            asked: Instant::now(),
            parent,
            colors,
            grid,
            then,
            admission: SpawnAdmission::Plain,
            background: false,
            completion: Arc::new(TabSpawnCompletion::default()),
            theme: None,
        });
        id
    }

    /// Book a new window in flight the way [`App::open_window_off_loop`] does,
    /// without a spawner: the test plays the spawner's part. Returns the
    /// reserved session id and window.
    pub(crate) fn book_window(
        app: &mut App,
        parent: Option<aterm_session::SessionId>,
        admission: SpawnAdmission,
        then: TabSpawnThen,
    ) -> (u64, WindowId) {
        let (window, place, grid) = app.reserve_window();
        let id = app.next_session_id;
        app.next_session_id += 1;
        let colors = app.engine_color_fanouts;
        app.tab_spawns.in_flight.push(InFlight {
            id,
            alt_id: None,
            window,
            place,
            asked: Instant::now(),
            parent,
            colors,
            grid,
            then,
            admission,
            background: false,
            completion: Arc::new(TabSpawnCompletion::default()),
            theme: None,
        });
        (id, window)
    }

    /// The OS side of a landing without the window server: whether the loop is
    /// exiting, and whether a window's surface can be made.
    pub(crate) struct InertOs {
        pub(crate) exiting: bool,
        pub(crate) attaches: bool,
    }

    impl InertOs {
        /// A running loop whose windows attach.
        pub(crate) const ATTACHES: Self = Self {
            exiting: false,
            attaches: true,
        };
    }

    impl LandingOs for InertOs {
        fn exiting(&self) -> bool {
            self.exiting
        }

        fn attach(&self, _app: &mut App, _wid: WindowId) -> bool {
            self.attaches
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::test_support::book;
    use super::*;
    use crate::SinkWriter;
    use aterm_window::event::Ime;
    use std::sync::Arc;

    /// A pipe whose write end backs a stub session's sink, so a test reads what
    /// reached that session's PTY.
    fn piped_session(id: u64) -> (Session, std::fs::File) {
        let mut fds = [0; 2];
        // SAFETY: `fds` is a two-int array, as `pipe` requires.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: the read end was just opened and is owned by nothing else.
        let read = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fds[0]) };
        // SAFETY: the write end was just opened; the sink takes sole ownership.
        let write =
            unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fds[1]) };
        let sink = Arc::new(SinkWriter::new_owned(write));
        (crate::stub_session_with_sink(id, sink), read)
    }

    /// Everything readable on `read` now, without waiting for more.
    fn drain(read: &std::fs::File) -> Vec<u8> {
        use std::io::Read as _;
        use std::os::fd::AsRawFd as _;
        // SAFETY: `read` is an open descriptor this test owns.
        unsafe { libc::fcntl(read.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        let mut r = read;
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        out
    }

    pub(crate) fn book_admitted(
        app: &mut App,
        wid: WindowId,
        admission: SpawnAdmission,
        then: TabSpawnThen,
    ) -> u64 {
        let id = book(app, wid, then);
        app.tab_spawns.in_flight.last_mut().unwrap().admission = admission;
        id
    }

    /// Summarize is reserved for the whole lifetime of its pending spawn,
    /// including a late settlement; failed/closed jobs release that reservation.
    #[test]
    fn summarize_waits_for_pending_spawn_settlement() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(!app.tab_spawns.summary_pending());
        let (tx, rx) = channel();
        let id = book_admitted(
            &mut app,
            wid,
            SpawnAdmission::Summary,
            TabSpawnThen::Reply {
                reply: tx,
                raise: false,
            },
        );
        assert!(
            app.tab_spawns.summary_pending(),
            "a second Summarize must not book another worker"
        );
        assert!(!app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Err(std::io::Error::other("exec refused")),
            reader_gate: None
        }));
        assert_eq!(rx.try_recv(), Ok(Err("exec refused".to_string())));
        assert!(
            !app.tab_spawns.summary_pending(),
            "failure releases the reservation"
        );
        let gone = WindowId(77);
        let id = book_admitted(
            &mut app,
            gone,
            SpawnAdmission::Summary,
            TabSpawnThen::Nothing,
        );
        assert!(app.tab_spawns.summary_pending());
        assert!(!app.install_spawned_tab(TabSpawned {
            id,
            window: gone,
            session: Ok(crate::stub_session(id)),
            reader_gate: None
        }));
        assert!(
            !app.tab_spawns.summary_pending(),
            "closed host cancels the reservation"
        );
    }

    /// [`book`] for a split of `wid`'s focused pane in `dir`.
    pub(crate) fn book_split(
        app: &mut App,
        wid: WindowId,
        dir: crate::pane::SplitDir,
        then: TabSpawnThen,
    ) -> u64 {
        let id = book(app, wid, then);
        let theme = app.inherited_theme(wid, SpawnPlace::Split(dir), None);
        if let Some(t) = app.tab_spawns.in_flight.last_mut() {
            t.place = SpawnPlace::Split(dir);
            t.theme = theme;
        }
        id
    }

    /// Always clean up this test's exact child, including panic/failed readiness.
    /// The master is owned here only until handed to the Session's sink.
    struct Holdout {
        identity: aterm_pty::ShellIdentity,
        master: Option<std::os::fd::OwnedFd>,
    }

    impl Holdout {
        fn wait_until_absent(&self) -> bool {
            let until = Instant::now() + Duration::from_secs(60);
            while self.identity.verify() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            !self.identity.verify()
        }
    }

    impl Drop for Holdout {
        fn drop(&mut self) {
            if self.identity.verify() {
                // SAFETY: this is our still-verified child's own process group,
                // never a name or an unrelated PID. All failure paths reap it.
                unsafe { libc::killpg(self.identity.pid, libc::SIGKILL) };
            }
            aterm_pty::reap_shell(self.identity);
        }
    }

    /// A real exec'd shell that has proved its HUP-ignoring disposition before
    /// the test can quit. No event loop, window, owner config or agent runs.
    fn hup_ignoring_session(app: &App, id: u64) -> (Session, Holdout) {
        use std::os::fd::{AsRawFd, FromRawFd};
        let command: Vec<std::ffi::OsString> = [
            "/bin/sh",
            "-c",
            "trap '' HUP; printf 'OWNED-HOLDOUT-READY\\n'; exec /bin/sleep 600",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let shell = aterm_pty::spawn_shell_with_pid_cell_px(
            24,
            80,
            &app.session_factory.spawn_cap,
            &app.session_factory.sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&command),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
            None,
        )
        .expect("the private owned PTY child starts");
        let mut holdout = Holdout {
            identity: aterm_pty::record_spawned_shell(shell.pid),
            // SAFETY: the new PTY master has no other owner yet.
            master: Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(shell.master) }),
        };
        aterm_pty::set_nonblocking(shell.master, true).unwrap();
        let mut seen = Vec::new();
        let until = Instant::now() + Duration::from_secs(60);
        while Instant::now() < until
            && !seen
                .windows(b"OWNED-HOLDOUT-READY".len())
                .any(|s| s == b"OWNED-HOLDOUT-READY")
        {
            let mut bytes = [0; 128];
            let n = aterm_pty::read(shell.master, &mut bytes);
            if n > 0 {
                seen.extend_from_slice(&bytes[..n as usize]);
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert!(
            seen.windows(b"OWNED-HOLDOUT-READY".len())
                .any(|s| s == b"OWNED-HOLDOUT-READY"),
            "the owned child never proved readiness"
        );
        assert!(holdout.identity.verify());
        let master = holdout.master.take().unwrap();
        let raw = master.as_raw_fd();
        let sink = Arc::new(SinkWriter::new_owned(master));
        let mut session = crate::stub_session_with_sink(id, sink);
        session.master = raw;
        session.pid = shell.pid;
        session.shell_identity = holdout.identity;
        (session, holdout)
    }

    /// Normal Quit must reap a child whose successful completion is still in
    /// the event queue: it is live but has never entered the session pool.
    #[test]
    fn quitting_reaps_a_hup_ignoring_tab_still_in_the_completion_queue() {
        let mut app = App::headless_for_test();
        let (tx, rx) = channel();
        let id = book(
            &mut app,
            WindowId(0),
            TabSpawnThen::Reply {
                reply: tx,
                raise: false,
            },
        );
        let (session, holdout) = hup_ignoring_session(&app, id);
        let completion = Arc::clone(&app.tab_spawns.in_flight.last().unwrap().completion);
        let mut queued = None;
        publish_completion(
            &completion,
            TabSpawned {
                id,
                window: WindowId(0),
                session: Ok(session),
                reader_gate: None,
            },
            |wake| {
                queued = Some(wake);
                true
            },
        );
        let queued = queued.expect("the worker queued its successful completion");
        // The existing pool-only exit cleanup cannot see this successful spawn.
        app.pool.hang_up_at_exit();
        assert!(
            holdout.identity.verify(),
            "the test's queued child must be alive outside the pool"
        );
        app.shutdown_tab_spawns();
        assert!(
            aterm_pty::wait_for_reaps(Duration::from_secs(60)),
            "owned exit reaps did not settle"
        );
        assert!(
            holdout.wait_until_absent(),
            "Quit abandoned the uninstalled HUP-ignoring child"
        );
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(60)), Ok(Err(_))),
            "the pending wire spawn was not refused at Quit"
        );
        assert!(
            matches!(queued, Wake::TabSpawned(_)),
            "the completion remains queued, not installed"
        );
    }

    /// A worker that already owns a live child when Quit cancels it must drop
    /// that Session before its join completes, without posting a new wake.
    #[test]
    fn a_spawn_finishing_after_quit_drops_its_owned_child_before_join() {
        let mut app = App::headless_for_test();
        let id = book(&mut app, WindowId(0), TabSpawnThen::Nothing);
        let completion = Arc::clone(&app.tab_spawns.in_flight.last().unwrap().completion);
        let (session, holdout) = hup_ignoring_session(&app, id);
        let (finish, ready) = channel();
        let (posted, wakes) = channel();
        let worker = std::thread::spawn(move || {
            // The child is already owned by this stack before cancellation;
            // the explicit rendezvous makes publication happen after Quit.
            ready.recv_timeout(Duration::from_secs(60)).unwrap();
            publish_completion(
                &completion,
                TabSpawned {
                    id,
                    window: WindowId(0),
                    session: Ok(session),
                    reader_gate: None,
                },
                |wake| posted.send(wake).is_ok(),
            );
        });
        app.shutdown_tab_spawns();
        finish.send(()).unwrap();
        worker.join().unwrap();
        assert!(
            wakes.try_recv().is_err(),
            "a cancelled spawn posted a completion after Quit"
        );
        assert!(aterm_pty::wait_for_reaps(Duration::from_secs(60)));
        assert!(
            holdout.wait_until_absent(),
            "the joined worker left its HUP-ignoring child alive"
        );
    }

    /// Whether a "Keystrokes dropped" row saying `what` is up.
    fn said_dropped(app: &App, what: &str) -> bool {
        app.messages.live_rows().any(|l| {
            l.msg.title == "Keystrokes dropped" && l.msg.detail.iter().any(|d| d.contains(what))
        })
    }

    fn typed(text: &str) -> WindowEvent {
        WindowEvent::Ime(Ime::Commit(text.to_string()))
    }

    /// Replay what settles for `wid` the way a live IME commit is handled.
    fn replay(app: &mut App, wid: WindowId, events: Vec<(WinitWindowId, WindowEvent)>) {
        for (_, event) in events {
            if let WindowEvent::Ime(Ime::Commit(text)) = event {
                app.on_ime_commit(wid, text);
            }
        }
    }

    /// A tab spawned off the loop lands as Cmd-T's always did: under its reserved
    /// id, registered and active, and the wire `spawn` waiting on it is answered
    /// with its sid only then, once the sid resolves in the registry. What was
    /// typed into the window meanwhile was held, and reaches the NEW tab's PTY in
    /// order after it lands; the tab the person left gets none of it.
    #[test]
    fn a_tab_spawned_off_the_loop_lands_registered_and_gets_what_was_typed_meanwhile() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        let (old, old_pty) = piped_session(app.next_session_id);
        app.push_stub_tab(wid, old);
        let tabs = app.windows[&wid].tab_set.len();
        let (tx, rx) = channel();
        let id = book(
            &mut app,
            wid,
            TabSpawnThen::Reply {
                reply: tx,
                raise: false,
            },
        );
        for text in ["l", "s", "\r"] {
            assert!(
                app.defer_tab_spawn_input(wid, winit_id, typed(text))
                    .is_none()
            );
        }
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, WindowEvent::RedrawRequested)
                .is_some(),
            "only keys, modifiers and IME wait"
        );
        assert!(
            rx.try_recv().is_err(),
            "nothing is answered before the tab lands"
        );

        let (new, new_pty) = piped_session(id);
        let sid = new.ctx.self_id.clone();
        let completion = Arc::clone(&app.tab_spawns.in_flight.last().unwrap().completion);
        let mut queued = None;
        publish_completion(
            &completion,
            TabSpawned {
                id,
                window: wid,
                session: Ok(new),
                reader_gate: None,
            },
            |wake| {
                queued = Some(wake);
                true
            },
        );
        let Wake::TabSpawned(completion) = queued.unwrap() else {
            panic!("the worker posted something other than its tab completion");
        };
        assert!(app.install_spawned_tab(completion.take().unwrap()));
        assert_eq!(rx.try_recv(), Ok(Ok(sid.as_str().to_string())));
        {
            let store = app.store.read().unwrap_or_else(|p| p.into_inner());
            assert_eq!(
                store.by_sid(&sid).map(|h| h.local_id),
                Some(id),
                "registered"
            );
        }
        assert_eq!(
            app.focused_session_id(wid),
            Some(id),
            "the new tab is the active one"
        );
        assert_eq!(
            app.windows[&wid].tab_set.len(),
            tabs + 1,
            "one tab appended"
        );

        let (events, repair) = app.settle_tab_spawn_input(wid, true);
        assert_eq!(repair, None);
        replay(&mut app, wid, events);
        assert_eq!(drain(&new_pty), b"ls\r", "in order, to the new tab");
        assert!(
            drain(&old_pty).is_empty(),
            "never to the tab the person left"
        );
        assert!(app.tab_spawns.held.is_empty() && app.tab_spawns.in_flight.is_empty());
    }

    /// A spawn that fails lands no tab: the waiter gets the error, the failure is
    /// said, and what was typed for the tab is dropped, never typed into the tab
    /// the person left. Its modifier changes still replay, so no chord strands.
    #[test]
    fn a_failed_spawn_lands_nothing_and_drops_what_was_typed_for_it() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        let (old, old_pty) = piped_session(app.next_session_id);
        app.push_stub_tab(wid, old);
        let tabs = app.windows[&wid].tab_set.len();
        let (tx, rx) = channel();
        let id = book(
            &mut app,
            wid,
            TabSpawnThen::Reply {
                reply: tx,
                raise: false,
            },
        );
        let mods = WindowEvent::ModifiersChanged(aterm_window::event::Modifiers::default());
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("rm"))
                .is_none()
        );
        assert!(app.defer_tab_spawn_input(wid, winit_id, mods).is_none());

        let refused = std::io::Error::other("exec failed");
        assert!(!app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Err(refused),
            reader_gate: None
        }));
        assert_eq!(rx.try_recv(), Ok(Err("exec failed".to_string())));
        assert_eq!(app.windows[&wid].tab_set.len(), tabs, "no tab");
        assert!(
            app.messages
                .live_rows()
                .any(|l| l.msg.title == "Couldn't open a new tab"),
            "the failure is said"
        );

        let (events, _) = app.settle_tab_spawn_input(wid, false);
        assert!(
            events
                .iter()
                .all(|(_, e)| matches!(e, WindowEvent::ModifiersChanged(_))),
            "only the modifier changes replay: {events:?}"
        );
        assert_eq!(events.len(), 1);
        replay(&mut app, wid, events);
        assert!(
            drain(&old_pty).is_empty(),
            "the keys never reach the tab left behind"
        );
        assert!(
            said_dropped(&app, "retype the last key"),
            "the dropped key is said"
        );
    }

    /// A REQUEST'S IDENTITY IS ADMITTED ON THE SPAWNER, before the fork, and
    /// a refusal there is still the request's own answer: the waiter hears
    /// `identity <name>: <why>`, the words the request's turn refused it with,
    /// and no "Couldn't open a new tab" row goes up, since nobody gestured.
    /// NEGATIVE CONTROL: a failure that is not a refusal still posts its row
    /// (the test above).
    #[test]
    fn an_identity_the_spawner_refuses_answers_its_waiter_without_a_failure_row() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-spawn-admit")
            .tempdir()
            .expect("scratch dir");
        crate::test_env::scoped("ATERM_STATE_HOME", scratch.path().join("state"), || {
            let refused = provision(Some("ghost"), IdentityPrep::Admit { inherited: true })
                .expect_err("nobody made `ghost`");
            assert_eq!(
                refusal(&refused).as_deref(),
                Some("identity ghost: no such identity ghost")
            );

            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            let (old, _old_pty) = piped_session(app.next_session_id);
            app.push_stub_tab(wid, old);
            let tabs = app.windows[&wid].tab_set.len();
            let (tx, rx) = channel();
            let id = book(
                &mut app,
                wid,
                TabSpawnThen::Reply {
                    reply: tx,
                    raise: false,
                },
            );
            assert!(!app.install_spawned_tab(TabSpawned {
                id,
                window: wid,
                session: Err(refused),
                reader_gate: None
            }));
            assert_eq!(
                rx.try_recv(),
                Ok(Err("identity ghost: no such identity ghost".to_string()))
            );
            assert_eq!(app.windows[&wid].tab_set.len(), tabs, "no tab");
            assert!(
                !app.messages
                    .live_rows()
                    .any(|l| l.msg.title == "Couldn't open a new tab"),
                "a request's refusal is its reply, not a row"
            );
        });
    }

    /// A window that closed while its tab was in flight lands nothing: the session
    /// is dropped, which hangs up and reaps its shell, and the waiter hears why.
    #[test]
    fn a_tab_whose_window_closed_is_dropped_and_its_waiter_told() {
        let mut app = App::headless_for_test();
        let (tx, rx) = channel();
        let gone = WindowId(77);
        let id = app.next_session_id;
        app.next_session_id += 1;
        app.tab_spawns.in_flight.push(InFlight {
            id,
            alt_id: None,
            window: gone,
            place: SpawnPlace::Tab,
            asked: Instant::now(),
            parent: None,
            colors: app.engine_color_fanouts,
            grid: app.spawn_grid(gone),
            then: TabSpawnThen::Reply {
                reply: tx,
                raise: false,
            },
            admission: SpawnAdmission::Plain,
            background: false,
            completion: Arc::new(TabSpawnCompletion::default()),
            theme: None,
        });
        let (session, _pty) = piped_session(id);
        let sid = session.ctx.self_id.clone();
        assert!(!app.install_spawned_tab(TabSpawned {
            id,
            window: gone,
            session: Ok(session),
            reader_gate: None
        }));
        assert!(matches!(rx.try_recv(), Ok(Err(why)) if why.contains("window closed")));
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        assert!(store.by_sid(&sid).is_none(), "never registered");
    }

    /// Input waits only while a tab asked for within the hold is in flight to its
    /// window: another window's input, and input past the hold, is handled now.
    #[test]
    fn input_waits_only_for_a_tab_in_flight_to_its_window_and_within_the_hold() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("a"))
                .is_some(),
            "no tab in flight"
        );
        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(WindowId(9), winit_id, typed("b"))
                .is_some(),
            "another window's input is its own"
        );
        let stale = Instant::now().checked_sub(INPUT_HOLD + Duration::from_secs(1));
        if let Some(stale) = stale {
            app.tab_spawns.in_flight[0].asked = stale;
            assert!(
                app.defer_tab_spawn_input(wid, winit_id, typed("c"))
                    .is_some(),
                "past the hold, input goes to what the window shows"
            );
        }
        assert_eq!(app.tab_spawns.in_flight[0].id, id);
    }

    /// A tab not yet registered is part of this process's work: replacing the
    /// process before it lands would lose both the tab and its waiting input.
    /// Admission must defer before consulting replacement authority; once the
    /// spawn settles, ordinary authority checks resume.
    #[test]
    fn an_update_waits_for_a_tab_spawn_to_settle_before_admission() {
        let mut app = App::headless_for_test();
        let id = book(&mut app, WindowId(0), TabSpawnThen::Nothing);
        let apply = |app: &mut App| {
            app.apply_staged_update_now(
                crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0, 0),
                crate::native_updater_service::ApplyMode::Immediate,
                None,
                None,
            )
            .expect_err("the fixture supplies no replacement authority")
        };
        assert!(
            apply(&mut app).is_activity_deferred(),
            "an unregistered tab must keep its process alive"
        );
        assert!(!app.install_spawned_tab(TabSpawned {
            id,
            window: WindowId(0),
            session: Err(std::io::Error::other("exec refused")),
            reader_gate: None,
        }));
        assert!(
            !apply(&mut app).is_activity_deferred(),
            "a settled spawn no longer holds replacement admission"
        );
    }

    /// The hold is bounded: past it the newest event is dropped and counted, and the
    /// window's replay ends with a modifier repair, since a drop can break a
    /// press/release pair. A focus loss with input held marks the same repair.
    #[test]
    fn held_input_is_bounded_and_a_drop_or_focus_loss_repairs_the_modifiers() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        for n in 0..MAX_HELD_INPUT + 3 {
            assert!(
                app.defer_tab_spawn_input(wid, winit_id, typed(&format!("{n}")))
                    .is_none()
            );
        }
        assert_eq!(app.tab_spawns.held.len(), MAX_HELD_INPUT);
        app.tab_spawns.in_flight.retain(|t| t.id != id);
        let (events, repair) = app.settle_tab_spawn_input(wid, true);
        assert_eq!(events.len(), MAX_HELD_INPUT, "the kept prefix replays");
        assert!(matches!(&events[0].1, WindowEvent::Ime(Ime::Commit(t)) if t == "0"));
        assert_eq!(repair, Some(wid), "a drop repairs the modifiers");
        assert!(said_dropped(&app, "retype the last 3 keys"));

        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("x"))
                .is_none()
        );
        app.tab_spawns.note_focus_loss(wid);
        app.tab_spawns.in_flight.retain(|t| t.id != id);
        let (_, repair) = app.settle_tab_spawn_input(wid, true);
        assert_eq!(
            repair,
            Some(wid),
            "a focus loss with input held repairs the modifiers"
        );
    }

    /// A scheme installed while a tab was in flight reaches its engine as it lands:
    /// the scheme change went to the sessions in the pool, and the newborn's engine
    /// was built from the factory as it was when the tab was asked for.
    #[test]
    fn a_tab_lands_with_the_scheme_installed_while_it_was_in_flight() {
        use aterm_types::Appearance;
        // `bump`: whether the install moved the engine-config generation, as
        // `App::install_colors` does with every projection change.
        let land = |bump: bool| {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            let id = book(&mut app, wid, TabSpawnThen::Nothing);
            let (session, _pty) = piped_session(id);
            crate::term_lock(&session.term).set_color_scheme(Appearance::Dark);
            let tc = app.session_factory.terminal_config.get_or_insert_default();
            tc.color_scheme = Some(Appearance::Light);
            if bump {
                app.engine_color_fanouts += 1;
            }
            let term = Arc::clone(&session.term);
            assert!(app.install_spawned_tab(TabSpawned {
                id,
                window: wid,
                session: Ok(session),
                reader_gate: None
            }));
            crate::term_lock(&term).color_scheme()
        };
        assert_eq!(land(true), Appearance::Light);
        // NEG: an unchanged generation runs no `apply_config`.
        assert_eq!(land(false), Appearance::Dark);
    }

    /// A window that re-gridded while its tab was in flight (a resize, a font zoom)
    /// sized only the panes it had then: the newborn, spawned at the old grid, is
    /// sized to the window's grid as it lands, and its program sees that size.
    #[test]
    fn a_tab_lands_at_the_grid_its_window_took_while_it_was_in_flight() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        let (session, _pty) = piped_session(id);
        let term = Arc::clone(&session.term);
        {
            let mut t = crate::term_lock(&term);
            t.resize(rows, cols);
        }
        let (later_rows, later_cols) = (rows + 7, cols + 11);
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.rows = later_rows;
            ws.cols = later_cols;
        }
        assert!(app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(session),
            reader_gate: None
        }));
        let t = crate::term_lock(&term);
        assert_eq!(
            (t.rows(), t.cols()),
            (later_rows, later_cols),
            "the window's grid, not the one it was asked at"
        );
    }

    /// With two tabs in flight to one window, held input waits for the last: the
    /// keys typed after both were asked for belong to the tab that ends up active.
    #[test]
    fn held_input_waits_for_the_last_tab_in_flight_to_its_window() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        let first = book(&mut app, wid, TabSpawnThen::Nothing);
        let second = book(&mut app, wid, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("k"))
                .is_none()
        );
        let (one, _pty1) = piped_session(first);
        assert!(app.install_spawned_tab(TabSpawned {
            id: first,
            window: wid,
            session: Ok(one),
            reader_gate: None
        }));
        let (events, _) = app.settle_tab_spawn_input(wid, true);
        assert!(events.is_empty(), "the second is still in flight");
        let (two, pty2) = piped_session(second);
        assert!(app.install_spawned_tab(TabSpawned {
            id: second,
            window: wid,
            session: Ok(two),
            reader_gate: None
        }));
        let (events, _) = app.settle_tab_spawn_input(wid, true);
        replay(&mut app, wid, events);
        assert_eq!(app.focused_session_id(wid), Some(second));
        assert_eq!(drain(&pty2), b"k");
    }

    /// A split spawned off the loop lands as Cmd-D's always did: a second pane
    /// beside the focused one, focused and registered, sized to its half before
    /// its reader reads (it was spawned at the window's grid), and the wire
    /// `spawn split=` waiting on it is answered with its sid only then.
    #[test]
    fn a_split_spawned_off_the_loop_lands_as_a_focused_pane_sized_to_its_half() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
        let (tx, rx) = channel();
        let id = book_split(
            &mut app,
            wid,
            crate::pane::SplitDir::Vertical,
            TabSpawnThen::Reply {
                reply: tx,
                raise: false,
            },
        );
        assert!(
            rx.try_recv().is_err(),
            "nothing is answered before it lands"
        );
        let (session, _pty) = piped_session(id);
        let sid = session.ctx.self_id.clone();
        let term = Arc::clone(&session.term);
        crate::term_lock(&term).resize(rows, cols);
        assert!(app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(session),
            reader_gate: None
        }));
        assert_eq!(rx.try_recv(), Ok(Ok(sid.as_str().to_string())));
        assert_eq!(
            app.active_tree(wid).map(crate::pane::PaneTree::len),
            Some(2)
        );
        assert_eq!(
            app.focused_session_id(wid),
            Some(id),
            "the new pane is focused"
        );
        {
            let store = app.store.read().unwrap_or_else(|p| p.into_inner());
            assert_eq!(
                store.by_sid(&sid).map(|h| h.local_id),
                Some(id),
                "registered"
            );
        }
        let t = crate::term_lock(&term);
        assert!(
            t.cols() <= cols / 2 && t.rows() <= rows,
            "the newborn is sized to its half, not the window: {}x{} of {cols}x{rows}",
            t.cols(),
            t.rows()
        );
        drop(t);
        assert!(app.structural_invariants_ok());
    }

    /// Spec `session-theme-carry`: a split from a pane with its own theme
    /// lands with that theme (origin `split`) on its engine before its reader
    /// runs; a new tab from the same window lands with none. A theme asked
    /// for at spawn beats the inherited one.
    #[test]
    fn split_inherits_tab_does_not() {
        use crate::session_theme::{SessionTheme, SessionThemeOrigin};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let hearth = aterm_types::scheme::builtin("Hearth").expect("built-in");
        app.set_session_theme(
            0,
            Some(SessionTheme {
                selector: "Hearth".to_string(),
                origin: SessionThemeOrigin::Ctl,
            }),
        );
        let land = |app: &mut App, id: u64| {
            let (session, _pty) = piped_session(id);
            assert!(app.install_spawned_tab(TabSpawned {
                id,
                window: wid,
                session: Ok(session),
                reader_gate: None
            }));
        };
        let split = book_split(
            &mut app,
            wid,
            crate::pane::SplitDir::Vertical,
            TabSpawnThen::Nothing,
        );
        land(&mut app, split);
        assert_eq!(
            app.pool.theme(split),
            Some(&SessionTheme {
                selector: "Hearth".to_string(),
                origin: SessionThemeOrigin::Split,
            })
        );
        assert_eq!(
            crate::term_lock(&app.pool.get(split).expect("landed").term).default_background(),
            hearth.background
        );
        // An asked theme wins over the pane's.
        let asked = book_split(
            &mut app,
            wid,
            crate::pane::SplitDir::Horizontal,
            TabSpawnThen::Nothing,
        );
        app.tab_spawns.in_flight.last_mut().expect("booked").theme = Some(SessionTheme {
            selector: "Linen".to_string(),
            origin: SessionThemeOrigin::Spawn,
        });
        land(&mut app, asked);
        assert_eq!(
            app.pool
                .theme(asked)
                .map(|t| (t.selector.as_str(), t.origin)),
            Some(("Linen", SessionThemeOrigin::Spawn))
        );
        let tab = book(&mut app, wid, TabSpawnThen::Nothing);
        land(&mut app, tab);
        assert_eq!(app.pool.theme(tab), None, "a new tab takes none");
    }

    /// Spec `session-theme-carry`: a split's inherited theme is decided when
    /// it is booked. A split booked from an unthemed pane (which may have
    /// taken a warm spare whose rc ran under the instance colours) lands with
    /// no theme even when the focused pane has one by then.
    #[test]
    fn a_split_inherits_what_the_pane_had_when_it_was_booked() {
        use crate::session_theme::{SessionTheme, SessionThemeOrigin};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let split = book_split(
            &mut app,
            wid,
            crate::pane::SplitDir::Vertical,
            TabSpawnThen::Nothing,
        );
        app.set_session_theme(
            0,
            Some(SessionTheme {
                selector: "Hearth".to_string(),
                origin: SessionThemeOrigin::Ctl,
            }),
        );
        let (session, _pty) = piped_session(split);
        assert!(app.install_spawned_tab(TabSpawned {
            id: split,
            window: wid,
            session: Ok(session),
            reader_gate: None
        }));
        assert_eq!(
            app.pool.theme(split),
            None,
            "booked unthemed, lands unthemed"
        );
    }

    /// The window can change while a split's shell starts: a split its focused
    /// pane can no longer take is refused as it lands, in the words a refusal at
    /// asking uses. The session drops unregistered, the tree is untouched, and
    /// the person is told.
    #[test]
    fn a_split_the_window_can_no_longer_take_is_refused_as_it_lands() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (tx, rx) = channel();
        let id = book_split(
            &mut app,
            wid,
            crate::pane::SplitDir::Vertical,
            TabSpawnThen::Reply {
                reply: tx,
                raise: false,
            },
        );
        // Narrowed below a left/right split's 33 columns meanwhile.
        assert!(app.apply_term_resize(wid, 24, 32), "geometry applies");
        let (session, _pty) = piped_session(id);
        let sid = session.ctx.self_id.clone();
        assert!(!app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(session),
            reader_gate: None
        }));
        assert!(
            matches!(rx.try_recv(), Ok(Err(why)) if why.starts_with("Split refused: this pane is 32x24 cells")),
            "the waiter hears the refusal"
        );
        assert_eq!(
            app.active_tree(wid).map(crate::pane::PaneTree::len),
            Some(1)
        );
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        assert!(store.by_sid(&sid).is_none(), "never registered");
        drop(store);
        assert!(
            app.messages
                .live_rows()
                .any(|l| l.msg.title == "Couldn't split the pane"),
            "the refusal is said"
        );
        assert!(app.tab_spawns.in_flight.is_empty());
        assert!(app.structural_invariants_ok());
    }

    /// `tab new` counts the new tab: its reply is the window's active index and
    /// tab count once the tab is in it, and a tab that never lands is refused,
    /// never counted as if it had.
    #[test]
    fn a_tab_new_is_counted_once_its_tab_is_in_the_window() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let tabs = app.windows[&wid].tab_set.len();
        let (tx, rx) = channel();
        let id = book(&mut app, wid, TabSpawnThen::Counts(tx));
        assert!(rx.try_recv().is_err(), "nothing is counted before it lands");
        let (session, _pty) = piped_session(id);
        assert!(app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(session),
            reader_gate: None
        }));
        assert_eq!(
            rx.try_recv(),
            Ok(Ok((tabs, tabs + 1))),
            "the new last tab is active"
        );

        let (tx, rx) = channel();
        let id = book(&mut app, wid, TabSpawnThen::Counts(tx));
        assert!(!app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Err(std::io::Error::other("exec refused")),
            reader_gate: None
        }));
        assert_eq!(rx.try_recv(), Ok(Err("exec refused".to_string())));
        assert_eq!(app.windows[&wid].tab_set.len(), tabs + 1, "no tab added");
    }

    /// A row that acts on the newborn at once (a worker's spawn, the menu bar's worker launch)
    /// runs as the tab lands, with a sid the registry already resolves; a spawn that fails
    /// hands it the reason instead.
    #[test]
    fn a_row_acting_on_the_newborn_runs_with_its_registered_sid() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let seen: Rc<RefCell<Vec<Result<bool, String>>>> = Rc::default();
        let run = |seen: &Rc<RefCell<Vec<Result<bool, String>>>>| {
            let seen = Rc::clone(seen);
            TabSpawnThen::Run(Box::new(move |app: &mut App, landed| {
                let registered = landed.map(|sid| {
                    let store = app.store.read().unwrap_or_else(|p| p.into_inner());
                    store.by_sid(&aterm_session::SessionId::new(sid)).is_some()
                });
                seen.borrow_mut().push(registered);
            }))
        };
        let id = book(&mut app, wid, run(&seen));
        assert!(
            seen.borrow().is_empty(),
            "nothing runs before the tab lands"
        );
        let (session, _pty) = piped_session(id);
        assert!(app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(session),
            reader_gate: None
        }));
        let id = book(&mut app, wid, run(&seen));
        assert!(!app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Err(std::io::Error::other("exec refused")),
            reader_gate: None
        }));
        assert_eq!(
            *seen.borrow(),
            [Ok(true), Err("exec refused".to_string())],
            "once each: the registered sid, then the reason"
        );
    }

    /// A booked spawn holds the event loop's status sweep for [`SWEEP_HOLD`]
    /// from its booking, the newest booking's hold winning, so its fork does not
    /// wait out a sweep of every tab; a landing ends the hold. NEGATIVE CONTROL:
    /// nothing in flight holds nothing.
    #[test]
    fn a_booked_spawn_holds_the_status_sweep_until_it_lands_or_the_hold_ends() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let now = Instant::now();
        assert_eq!(app.tab_spawns.status_sweep_held_until(now), None);
        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        let asked = app.tab_spawns.in_flight[0].asked;
        assert_eq!(
            app.tab_spawns.status_sweep_held_until(asked),
            Some(asked + SWEEP_HOLD)
        );
        assert_eq!(
            app.tab_spawns.status_sweep_held_until(asked + SWEEP_HOLD),
            None,
            "the hold ends"
        );
        app.tab_spawns.in_flight[0].asked = asked - SWEEP_HOLD;
        let newer = book(&mut app, wid, TabSpawnThen::Nothing);
        let newest = app.tab_spawns.in_flight[1].asked;
        assert_eq!(
            app.tab_spawns.status_sweep_held_until(newest),
            Some(newest + SWEEP_HOLD),
            "the newest booking's hold"
        );
        for landed in [id, newer] {
            let (session, _pty) = piped_session(landed);
            assert!(app.install_spawned_tab(TabSpawned {
                id: landed,
                window: wid,
                session: Ok(session),
                reader_gate: None
            }));
        }
        assert_eq!(
            app.tab_spawns.status_sweep_held_until(newest),
            None,
            "landed, nothing holds it"
        );
    }

    /// While the sweep is held, the status observer's wake waits for the hold's
    /// end: one the skipped sweep left already past would otherwise be armed in
    /// the past on every turn of the hold, a spin that keeps the loop from the
    /// park its spawn's fork waits for. A later wake and no wake are its own,
    /// and an unheld wake is untouched. NEGATIVE CONTROL: unheld, a past wake
    /// stays past.
    #[test]
    fn a_held_sweep_arms_no_status_wake_before_the_hold_ends() {
        let mut app = App::headless_for_test();
        book(&mut app, WindowId(0), TabSpawnThen::Nothing);
        let asked = app.tab_spawns.in_flight[0].asked;
        let now = asked + Duration::from_millis(1);
        let held = app.tab_spawns.status_sweep_held_until(now);
        assert_eq!(held, Some(asked + SWEEP_HOLD));
        let past = asked;
        let later = asked + SWEEP_HOLD + Duration::from_millis(5);
        assert_eq!(
            held_status_wake(Some(past), held),
            held,
            "past: the hold's end"
        );
        assert_eq!(held_status_wake(Some(later), held), Some(later));
        assert_eq!(held_status_wake(None, held), None);
        assert_eq!(held_status_wake(Some(past), None), Some(past));
    }

    /// A SPAWN WHOSE STATE FOLDER DID NOT ANSWER IS REFUSED BY NAME, before
    /// anything is written or forked: on a volume that is not local whose look
    /// did not answer, the spawner says which folder (`TimedOut`, a failure
    /// row's words, never a [`Refusal`]) and goes on to the next tab, where it
    /// waited out the mount; a local folder is never looked at, and one that
    /// answered, or none at all, lets the spawn go on. NEGATIVE CONTROL: a gate
    /// that does not ask the look lets the silent folder through.
    #[test]
    fn a_spawn_whose_state_folder_did_not_answer_is_refused_by_name() {
        let state = std::path::Path::new("/aterm-test/net-home/Library/Application Support/aterm");
        let refused = state_answers_with(Some(state), |_| false, |_| false)
            .expect_err("a silent network folder");
        assert_eq!(refused.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(
            refused.to_string(),
            format!("{} did not answer", state.display())
        );
        assert_eq!(refusal(&refused), None, "a failure, said in a row");
        assert!(state_answers_with(Some(state), |_| false, |_| true).is_ok());
        assert!(
            state_answers_with(
                Some(state),
                |_| true,
                |_| panic!("a local folder is not looked at")
            )
            .is_ok()
        );
        assert!(state_answers_with(None, |_| false, |_| false).is_ok());
    }

    /// The spawner makes the identity a request names (`spawn identity=<name>`,
    /// `Admit` with nothing inherited) or a new-identity row names (`Create`)
    /// before the shell that wears it, so nothing is made for a spawn the main
    /// thread refused before booking it, and the primer is handed off rather
    /// than waited for. An inherited one and a roster row's make nothing. A
    /// request's refusal is its own words ([`Refusal`]); the row's is a failure.
    /// NEGATIVE CONTROL: the inherited admission of a missing name is refused.
    #[test]
    fn the_spawner_makes_a_named_identity_before_its_shell() {
        use crate::agent_identity::check_for_spawn_in;
        let state = aterm_tempfile::tempdir().expect("scratch state root");
        let root = state.path().join("identities");
        let primed = std::cell::RefCell::new(Vec::new());
        let admit = |name: &str, how: SpawnAdmit| {
            check_for_spawn_in(&root, name, how, |dir, _| {
                primed.borrow_mut().push(dir.to_path_buf());
            })
        };
        let inherited = provision_with(
            Some("worker"),
            IdentityPrep::Admit { inherited: true },
            admit,
        )
        .expect_err("an inherited identity nobody made is refused");
        assert_eq!(
            refusal(&inherited).as_deref(),
            Some("identity worker: no such identity worker")
        );
        assert!(!root.exists(), "and none is made");
        assert!(
            provision_with(
                Some("worker"),
                IdentityPrep::Admit { inherited: false },
                admit
            )
            .is_ok()
        );
        assert!(root.join("worker/.claude").is_dir(), "a request's is made");
        assert!(provision_with(Some("gamma"), IdentityPrep::Create, admit).is_ok());
        assert!(root.join("gamma").is_dir(), "a new-identity row's is made");
        assert_eq!(
            *primed.borrow(),
            [root.join("worker"), root.join("gamma")],
            "each primer handed off, none written in the admission"
        );
        assert!(provision_with(Some("delta"), IdentityPrep::Exists, admit).is_ok());
        assert!(!root.join("delta").exists(), "a roster row makes none");
        assert!(provision_with(None, IdentityPrep::Create, admit).is_ok());
        let refused = provision_with(Some("Bad Name"), IdentityPrep::Create, admit)
            .expect_err("the grammar refuses it");
        assert!(
            refused.to_string().starts_with("identity Bad Name: "),
            "{refused}"
        );
        assert!(refusal(&refused).is_none(), "a row's failure is no refusal");
    }

    /// [`piped_session`] with its pipe standing in for the PTY master, so the
    /// session registers as a spawned one does (inert stubs, master -1, do not).
    /// The sink owns the descriptor, as it owns a real master.
    pub(crate) fn registered_piped_session(id: u64) -> (Session, std::fs::File) {
        let (mut session, read) = piped_session(id);
        session.master = session.ctx.sink.master();
        (session, read)
    }

    /// Whether a "Couldn't open a new window" row saying `what` is up.
    fn said_no_window(app: &App, what: &str) -> bool {
        app.messages.live_rows().any(|l| {
            l.msg.title == "Couldn't open a new window"
                && l.msg.detail.iter().any(|d| d.contains(what))
        })
    }

    /// A new window spawned off the loop is made as its session lands: under the
    /// id reserved when it was asked for, frontmost, its one tab the newborn,
    /// registered with no parent, and at the grid the shell was spawned at (the
    /// front window's then), even though the front window re-gridded meanwhile.
    /// What was typed into the window it was asked from waited for it and
    /// replays through its own OS window, in order; the window left behind gets
    /// none of it, and another window's input was never held.
    #[test]
    fn a_window_spawned_off_the_loop_lands_frontmost_and_gets_what_was_typed() {
        let mut app = App::headless_for_test();
        let from = WindowId(0);
        let from_winit = WinitWindowId::from(0u64);
        app.winit_to_window.insert(from_winit, from);
        let (old, old_pty) = piped_session(app.next_session_id);
        app.push_stub_tab(from, old);
        let windows = app.windows.len();
        let asked_grid = (app.windows[&from].rows, app.windows[&from].cols);
        let reserved = WindowId(app.next_window_id);
        let (tx, rx) = channel();
        let (id, wid) = test_support::book_window(
            &mut app,
            None,
            SpawnAdmission::Plain,
            TabSpawnThen::Reply {
                reply: tx,
                raise: true,
            },
        );
        assert_eq!(
            wid, reserved,
            "the window's id is minted when it is asked for"
        );
        assert!(
            !app.windows.contains_key(&wid),
            "no window before its session"
        );
        assert_eq!(app.frontmost_window, Some(from));
        for text in ["l", "s", "\r"] {
            assert!(
                app.defer_tab_spawn_input(from, from_winit, typed(text))
                    .is_none(),
                "typed into the asking window, for the new one"
            );
        }
        assert!(
            app.defer_tab_spawn_input(WindowId(9), WinitWindowId::from(9u64), typed("x"))
                .is_some(),
            "another window's input is its own"
        );
        // The front window re-grids while the shell starts.
        if let Some(ws) = app.windows.get_mut(&from) {
            ws.rows += 7;
            ws.cols += 11;
        }
        assert!(
            rx.try_recv().is_err(),
            "nothing is answered before it lands"
        );

        let (new, new_pty) = registered_piped_session(id);
        let sid = new.ctx.self_id.clone();
        assert!(app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(new),
            reader_gate: None,
        }));
        assert_eq!(rx.try_recv(), Ok(Ok(sid.as_str().to_string())));
        assert_eq!(app.windows.len(), windows + 1, "one window made");
        assert_eq!(
            app.frontmost_window,
            Some(wid),
            "the new window is in front"
        );
        let ws = &app.windows[&wid];
        assert_eq!(ws.tab_set.len(), 1);
        assert_eq!(
            (ws.rows, ws.cols),
            asked_grid,
            "made at the grid its shell was spawned at"
        );
        assert_eq!(app.focused_session_id(wid), Some(id));
        {
            let store = app.store.read().unwrap_or_else(|p| p.into_inner());
            let handle = store.by_sid(&sid).expect("registered");
            assert_eq!((handle.local_id, handle.parent.as_ref()), (id, None));
        }
        assert!(app.structural_invariants_ok());

        // The OS window the attach made.
        let own_winit = WinitWindowId::from(1u64);
        app.winit_to_window.insert(own_winit, wid);
        assert_eq!(app.settle_tab_spawn_input(from, true).0.len(), 0);
        let (events, repair) = app.settle_tab_spawn_input(wid, true);
        assert_eq!(repair, None);
        assert!(
            events.iter().all(|(through, _)| *through == own_winit),
            "replayed through the new window's own OS window"
        );
        replay(&mut app, wid, events);
        assert_eq!(drain(&new_pty), b"ls\r", "in order, to the new window");
        assert!(
            drain(&old_pty).is_empty(),
            "never to the window left behind"
        );
        assert!(app.tab_spawns.held.is_empty() && app.tab_spawns.in_flight.is_empty());
    }

    /// Input typed after the latest ask is for what that ask lands in: a new
    /// window asked for after a tab takes the keys from then on, and a tab asked
    /// for after a new window takes them back. A focus loss of the asking window
    /// repairs the new window's modifiers after its replay.
    #[test]
    fn held_input_is_for_the_latest_ask_and_a_focus_loss_repairs_the_new_window() {
        let mut app = App::headless_for_test();
        let from = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        let tab = book(&mut app, from, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(from, winit_id, typed("t"))
                .is_none()
        );
        let (_, wid) =
            test_support::book_window(&mut app, None, SpawnAdmission::Plain, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(from, winit_id, typed("w"))
                .is_none()
        );
        let later = book(&mut app, from, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(from, winit_id, typed("u"))
                .is_none()
        );
        let held: Vec<(WindowId, String)> = app
            .tab_spawns
            .held
            .iter()
            .map(|h| match &h.event {
                WindowEvent::Ime(Ime::Commit(t)) => (h.target, t.clone()),
                other => panic!("only commits were typed: {other:?}"),
            })
            .collect();
        assert_eq!(
            held,
            [
                (from, "t".to_string()),
                (wid, "w".to_string()),
                (from, "u".to_string())
            ]
        );
        app.tab_spawns.note_focus_loss(from);
        app.tab_spawns
            .in_flight
            .retain(|t| t.id != tab && t.id != later && t.window != wid);
        let (events, repair) = app.settle_tab_spawn_input(wid, true);
        assert_eq!(events.len(), 1);
        assert_eq!(
            repair,
            Some(wid),
            "typed under modifiers the asking window's focus loss cleared"
        );
    }

    /// A new window whose spawn fails is never made: the failure is said in the
    /// same row the window built in the turn posted, the waiter hears it, and
    /// what was typed for the window is dropped and counted. Its modifier changes
    /// reached the window they were typed into as they were typed, so no chord
    /// strands there and none replays twice, and when that window lost focus
    /// meanwhile its modifiers are repaired, not the window that never opened.
    #[test]
    fn a_window_whose_spawn_failed_is_never_made() {
        let mut app = App::headless_for_test();
        let from = WindowId(0);
        let from_winit = WinitWindowId::from(0u64);
        app.winit_to_window.insert(from_winit, from);
        let (old, old_pty) = piped_session(app.next_session_id);
        app.push_stub_tab(from, old);
        let windows = app.windows.len();
        let (tx, rx) = channel();
        let (id, wid) = test_support::book_window(
            &mut app,
            None,
            SpawnAdmission::Plain,
            TabSpawnThen::Reply {
                reply: tx,
                raise: false,
            },
        );
        let mods = WindowEvent::ModifiersChanged(aterm_window::event::Modifiers::default());
        assert!(
            app.defer_tab_spawn_input(from, from_winit, typed("rm"))
                .is_none()
        );
        assert_eq!(
            app.defer_tab_spawn_input(from, from_winit, mods.clone()),
            Some(mods),
            "held for the new window, and handed to the one typed into now"
        );
        app.tab_spawns.note_focus_loss(from);

        assert!(!app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Err(std::io::Error::other("the pty could not be opened")),
            reader_gate: None,
        }));
        assert_eq!(rx.try_recv(), Ok(Err(NO_NEW_WINDOW.to_string())));
        assert_eq!(app.windows.len(), windows, "no window");
        assert!(!app.windows.contains_key(&wid));
        assert_eq!(app.frontmost_window, Some(from));
        assert!(said_no_window(&app, "the pty could not be opened"));

        let (events, repair) = app.settle_tab_spawn_input(wid, false);
        assert!(
            events.is_empty(),
            "the modifier change already reached where it was typed: {events:?}"
        );
        assert_eq!(
            repair,
            Some(from),
            "the repair goes back to where the modifier changes were typed"
        );
        replay(&mut app, from, events);
        assert!(
            drain(&old_pty).is_empty(),
            "the keys never reach the window left"
        );
        assert!(said_dropped(&app, "retype the last key"));
    }

    /// A new window whose OS surface cannot be made is rolled back with its
    /// session: no window, nothing in the pool or the registry, the front back
    /// where it was, and the shell, one that ignores SIGHUP, reaped. It says so,
    /// and the waiter hears it.
    #[test]
    fn a_window_whose_surface_cannot_be_made_rolls_back_and_reaps_its_shell() {
        let mut app = App::headless_for_test();
        let from = WindowId(0);
        let windows = app.windows.len();
        let (tx, rx) = channel();
        let (id, wid) = test_support::book_window(
            &mut app,
            None,
            SpawnAdmission::Plain,
            TabSpawnThen::Reply {
                reply: tx,
                raise: false,
            },
        );
        let (session, holdout) = hup_ignoring_session(&app, id);
        let sid = session.ctx.self_id.clone();
        let no_surface = test_support::InertOs {
            exiting: false,
            attaches: false,
        };
        assert!(!app.install_spawned(
            TabSpawned {
                id,
                window: wid,
                session: Ok(session),
                reader_gate: None,
            },
            &no_surface,
        ));
        assert_eq!(rx.try_recv(), Ok(Err(NO_NEW_WINDOW.to_string())));
        assert_eq!(app.windows.len(), windows);
        assert!(!app.windows.contains_key(&wid));
        assert_eq!(app.frontmost_window, Some(from));
        assert!(app.pool.get(id).is_none());
        assert!(
            app.store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .by_sid(&sid)
                .is_none()
        );
        assert!(app.structural_invariants_ok());
        assert!(said_no_window(&app, "the window could not be opened"));
        assert!(
            aterm_pty::wait_for_reaps(Duration::from_secs(60)),
            "the rollback's reap did not settle"
        );
        assert!(
            holdout.wait_until_absent(),
            "the rolled-back window's shell outlived it"
        );
    }

    /// Once the loop is exiting nothing lands: a new window is not made and a
    /// tab not installed for a process on its way out. Each session drops, its
    /// waiter hears the quit, and no failure row goes up.
    #[test]
    fn nothing_lands_once_the_loop_is_exiting() {
        let mut app = App::headless_for_test();
        let from = WindowId(0);
        let windows = app.windows.len();
        let tabs = app.windows[&from].tab_set.len();
        let rows = app.messages.live_rows().count();
        let exiting = test_support::InertOs {
            exiting: true,
            attaches: true,
        };
        let (tab_tx, tab_rx) = channel();
        let tab = book(
            &mut app,
            from,
            TabSpawnThen::Reply {
                reply: tab_tx,
                raise: false,
            },
        );
        let (win_tx, win_rx) = channel();
        let (window, wid) = test_support::book_window(
            &mut app,
            None,
            SpawnAdmission::Plain,
            TabSpawnThen::Reply {
                reply: win_tx,
                raise: false,
            },
        );
        for (id, at) in [(tab, from), (window, wid)] {
            assert!(!app.install_spawned(
                TabSpawned {
                    id,
                    window: at,
                    session: Ok(crate::stub_session(id)),
                    reader_gate: None,
                },
                &exiting,
            ));
            assert!(app.pool.get(id).is_none());
        }
        assert_eq!(tab_rx.try_recv(), Ok(Err(QUITTING.to_string())));
        assert_eq!(win_rx.try_recv(), Ok(Err(QUITTING.to_string())));
        assert_eq!(app.windows.len(), windows);
        assert_eq!(app.windows[&from].tab_set.len(), tabs);
        assert_eq!(app.messages.live_rows().count(), rows, "no failure is said");
        assert!(app.tab_spawns.in_flight.is_empty());
    }

    /// The `new_window` keybinding and Cmd-N ask for their window in the key's
    /// own turn, as Cmd-T asks for its tab, so the keys after them, a replay's
    /// included, wait for the window they booked instead of typing where they
    /// were pressed. Here no event loop hosts the window, so each booking says
    /// so at once. Headless keeps its one logical window and ignores them.
    #[test]
    fn a_new_window_key_books_its_window_in_its_own_turn() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let next = app.next_window_id;
        app.dispatch_action(wid, crate::keybinding::Action::NewWindow);
        assert_eq!(app.next_window_id, next, "headless ignores it");
        assert!(!said_no_window(&app, "no event loop"));

        app.headless = false;
        app.dispatch_action(wid, crate::keybinding::Action::NewWindow);
        assert_eq!(app.next_window_id, next + 1, "the key minted its window");
        assert!(said_no_window(&app, "no event loop to host the new window"));
        // Cmd-N is a hardcoded chord where the platform's convention has one.
        if crate::app_input::HARDCODED_SUPER_CHORDS {
            if let Some(ws) = app.windows.get_mut(&wid) {
                ws.mods = ModifiersState::SUPER;
            }
            app.on_key(
                wid,
                aterm_window::event::KeyEvent::synthetic_for_test(
                    aterm_window::keyboard::PhysicalKey::Code(
                        aterm_window::keyboard::KeyCode::KeyN,
                    ),
                    aterm_window::keyboard::Key::Character("n".into()),
                    Some("n".into()),
                    aterm_window::keyboard::KeyLocation::Standard,
                    aterm_window::event::ElementState::Pressed,
                    false,
                ),
            );
            assert_eq!(app.next_window_id, next + 2, "Cmd-N minted its window");
        }
    }

    fn moved(x: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: aterm_window::event::DeviceId::dummy(),
            position: aterm_window::dpi::PhysicalPosition::new(x, 1.0),
        }
    }

    fn click(state: aterm_window::event::ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: aterm_window::event::DeviceId::dummy(),
            state,
            button: aterm_window::event::MouseButton::Left,
        }
    }

    fn wheel(lines: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: aterm_window::event::DeviceId::dummy(),
            delta: aterm_window::event::MouseScrollDelta::LineDelta(0.0, lines),
            phase: aterm_window::event::TouchPhase::Moved,
        }
    }

    fn mods() -> WindowEvent {
        WindowEvent::ModifiersChanged(aterm_window::event::Modifiers::default())
    }

    /// What `events` are, in order, as short names: a commit's text, `mods`,
    /// `move <x>`, `press`, `release`, `wheel <lines>`.
    fn named(events: &[(WinitWindowId, WindowEvent)]) -> Vec<String> {
        events
            .iter()
            .map(|(_, event)| match event {
                WindowEvent::Ime(Ime::Commit(t)) => t.clone(),
                WindowEvent::ModifiersChanged(_) => "mods".to_string(),
                WindowEvent::CursorMoved { position, .. } => format!("move {}", position.x),
                WindowEvent::MouseInput { state, .. } if state.is_pressed() => "press".into(),
                WindowEvent::MouseInput { .. } => "release".to_string(),
                WindowEvent::MouseWheel {
                    delta: aterm_window::event::MouseScrollDelta::LineDelta(_, y),
                    ..
                } => format!("wheel {y}"),
                other => panic!("not a test event: {other:?}"),
            })
            .collect()
    }

    /// THE POINTER WAITS WITH THE KEYS FOR A TAB LANDING UNDER IT (sweep #31):
    /// a click after Cmd-T is held and replays after the tab lands, in order
    /// with the keys around it, through the window it was typed into. The
    /// pointer's motion and a scroll in progress fold into one event each, so
    /// moving the mouse cannot fill the hold. Another window's pointer, and a
    /// pointer over the window a new window was asked from, are handled now; a
    /// modifier change held for a new window reaches the window typed into now
    /// as well. NEGATIVE CONTROL: with nothing in flight the pointer is
    /// handled now.
    #[test]
    fn the_pointer_waits_with_the_keys_for_a_tab_landing_under_it() {
        use aterm_window::event::ElementState::{Pressed, Released};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        app.winit_to_window.insert(winit_id, wid);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, click(Pressed))
                .is_some(),
            "nothing in flight"
        );
        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        for event in [
            typed("a"),
            moved(1.0),
            moved(2.0),
            moved(3.0),
            click(Pressed),
            click(Released),
            wheel(1.0),
            wheel(2.0),
            typed("b"),
        ] {
            assert!(app.defer_tab_spawn_input(wid, winit_id, event).is_none());
        }
        assert!(
            app.defer_tab_spawn_input(WindowId(9), winit_id, click(Pressed))
                .is_some(),
            "another window's pointer is its own"
        );
        app.tab_spawns.in_flight.retain(|t| t.id != id);
        let (events, repair) = app.settle_tab_spawn_input(wid, true);
        assert_eq!(repair, None);
        assert_eq!(
            named(&events),
            ["a", "move 3", "press", "release", "wheel 3", "b"]
        );
        assert!(events.iter().all(|(via, _)| *via == winit_id));

        // A new window asked from `wid`: the pointer over `wid` is `wid`'s, and
        // a modifier change is held for the new window and handed back too.
        let (_, window) =
            test_support::book_window(&mut app, None, SpawnAdmission::Plain, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, click(Pressed))
                .is_some()
        );
        assert_eq!(
            app.defer_tab_spawn_input(wid, winit_id, mods()),
            Some(mods())
        );
        assert!(
            app.tab_spawns
                .held
                .iter()
                .all(|h| h.target == window && h.applied)
        );
    }

    /// A CLICK HELD ON THE IN-GRID TAB STRIP IS NOT REPLAYED ONTO THE STRIP
    /// THE LANDING RE-LAID: the new tab's chip moved every chip, so the press
    /// (and its release) is dropped, while a click in the grid replays after
    /// the tab as typed, and so does a press whose pointer moved off the strip
    /// before it. NEGATIVE CONTROL: with no strip, the same press replays.
    #[test]
    fn a_click_held_on_the_tab_strip_is_dropped_not_replayed_onto_a_moved_chip() {
        use aterm_window::event::ElementState::{Pressed, Released};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        app.winit_to_window.insert(winit_id, wid);
        let (cw, ch) = app.win_cell_size(wid);
        let (cw, ch, pad) = (cw as f64, ch as f64, app.win_pad(wid) as f64);
        let on_strip = (pad + cw * 2.5, pad + ch * 0.5);
        let in_grid = (pad + cw * 10.5, pad + ch * 6.5);
        let at = |(x, y): (f64, f64)| WindowEvent::CursorMoved {
            device_id: aterm_window::event::DeviceId::dummy(),
            position: aterm_window::dpi::PhysicalPosition::new(x, y),
        };
        let clicks = |app: &mut App, strip: bool| {
            app.tab_strip_rows = u16::from(strip);
            if let Some(ws) = app.windows.get_mut(&wid) {
                ws.last_cursor_px = on_strip;
            }
            let id = book(app, wid, TabSpawnThen::Nothing);
            for event in [
                click(Pressed),
                click(Released),
                at(in_grid),
                click(Pressed),
                click(Released),
                at(on_strip),
                at(in_grid),
                click(Pressed),
                click(Released),
            ] {
                assert!(app.defer_tab_spawn_input(wid, winit_id, event).is_none());
            }
            app.tab_spawns.in_flight.retain(|t| t.id != id);
            app.settle_tab_spawn_input(wid, true).0
        };
        let strip = clicks(&mut app, true);
        assert!(app.strip_col_at(wid, on_strip.0, on_strip.1).is_some());
        assert!(app.strip_col_at(wid, in_grid.0, in_grid.1).is_none());
        let grid_x = format!("move {}", in_grid.0);
        assert_eq!(
            named(&strip),
            [
                grid_x.as_str(),
                "press",
                "release",
                grid_x.as_str(),
                "press",
                "release"
            ]
        );
        assert_eq!(
            named(&clicks(&mut app, false)),
            [
                "press",
                "release",
                grid_x.as_str(),
                "press",
                "release",
                grid_x.as_str(),
                "press",
                "release"
            ],
            "no strip: all of it (the two moves in a row fold into one)"
        );
    }

    /// WHAT IS HELD PAST THE HOLD IS DROPPED, NEVER REPLAYED LATE (sweep
    /// #31): the window's next input past it settles what it held first, as
    /// for a spawn that never landed (the key dropped and said, the modifier
    /// change and the pointer's position kept, the click dropped with its
    /// release), so the later key goes to the tab the window shows and the
    /// earlier one is not typed into the tab that lands after. A spawn that
    /// lands past the hold drops what waited for it the same way. NEGATIVE
    /// CONTROL: input settled inside the hold replays.
    #[test]
    fn input_held_past_the_hold_is_dropped_never_replayed_late() {
        use aterm_window::event::ElementState::{Pressed, Released};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        app.winit_to_window.insert(winit_id, wid);
        let (new, new_pty) = piped_session(app.next_session_id);
        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        for event in [
            typed("a"),
            mods(),
            moved(4.0),
            click(Pressed),
            click(Released),
        ] {
            assert!(app.defer_tab_spawn_input(wid, winit_id, event).is_none());
        }
        let inside = app.expire_tab_spawn_input(winit_id, Instant::now());
        assert!(inside.0.is_empty(), "inside the hold nothing settles");
        assert_eq!(app.tab_spawns.held.len(), 5);

        let past = Instant::now() + INPUT_HOLD;
        let (events, repair) = app.expire_tab_spawn_input(winit_id, past);
        assert_eq!(named(&events), ["mods", "move 4"]);
        assert_eq!(repair, None);
        assert!(app.tab_spawns.held.is_empty());
        assert!(said_dropped(&app, "retype the last key"));

        // The spawn lands after: nothing typed before the hold ended reaches it.
        assert!(app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(new),
            reader_gate: None,
        }));
        let (events, _) = app.settle_tab_spawn_input(wid, true);
        assert!(events.is_empty(), "{events:?}");
        assert!(
            drain(&new_pty).is_empty(),
            "never replayed into the late tab"
        );

        // A spawn landing past the hold drops what waited for it.
        let late = book(&mut app, wid, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("c"))
                .is_none()
        );
        let ended = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("a monotonic clock past its first millisecond");
        app.tab_spawns.held.iter_mut().for_each(|h| h.until = ended);
        app.tab_spawns.in_flight.retain(|t| t.id != late);
        let (events, _) = app.settle_tab_spawn_input(wid, true);
        assert!(
            events.is_empty(),
            "a late landing replays nothing: {events:?}"
        );
    }

    /// A REFUSED EVENT PAST THE CAP IS COUNTED ONLY WHEN IT IS A KEYSTROKE: a
    /// click refused past [`MAX_HELD_INPUT`] is no key to retype, but it still
    /// breaks the stream, so the window's modifiers are repaired.
    #[test]
    fn only_a_keystroke_refused_past_the_cap_is_counted() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        for n in 0..MAX_HELD_INPUT {
            assert!(
                app.defer_tab_spawn_input(wid, winit_id, typed(&format!("{n}")))
                    .is_none()
            );
        }
        let pressed = click(aterm_window::event::ElementState::Pressed);
        assert!(app.defer_tab_spawn_input(wid, winit_id, pressed).is_none());
        app.tab_spawns.in_flight.retain(|t| t.id != id);
        let (events, repair) = app.settle_tab_spawn_input(wid, true);
        assert_eq!(events.len(), MAX_HELD_INPUT);
        assert_eq!(repair, Some(wid));
        assert!(
            !app.messages
                .live_rows()
                .any(|l| l.msg.title == "Keystrokes dropped"),
            "a click is no keystroke"
        );
    }

    /// INPUT TYPED BEFORE A MENU COMMAND IS DISPATCHED WAITS FOR IT (sweep
    /// #32): Cmd-T through the native menu's key equivalent posts a marked
    /// wake, and `ls` typed before the loop dispatches it is held under the
    /// window, then for the tab the dispatch booked, and replays into it once
    /// it lands. A command that books nothing hands what waited back in order;
    /// one that books a new window takes the keys and modifiers for it, and
    /// leaves the pointer to the window it points at. Two commands posted
    /// before either is dispatched each take what was typed after their own
    /// post. A mark older than the hold holds nothing, and input a dispatch
    /// hands back waits for no later mark. NEGATIVE CONTROL: with no mark,
    /// what is typed with nothing in flight is handled now.
    #[test]
    fn input_typed_before_a_menu_command_is_dispatched_waits_for_it() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        app.winit_to_window.insert(winit_id, wid);
        let marks = Arc::clone(&app.tab_spawns.menu);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("x"))
                .is_some(),
            "no mark, nothing in flight"
        );

        // Cmd-T: the dispatch books a tab, which takes what waited.
        let mark = marks.mint(Instant::now());
        for text in ["l", "s"] {
            assert!(
                app.defer_tab_spawn_input(wid, winit_id, typed(text))
                    .is_none()
            );
        }
        let (new, new_pty) = piped_session(app.next_session_id);
        let booked = app.tab_spawns.in_flight.len();
        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        assert!(marks.settle(mark));
        assert!(app.claim_menu_held_input(mark, booked).is_empty());
        assert!(app.tab_spawns.held.iter().all(|h| h.menu == MenuMark::NONE));
        assert!(app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(new),
            reader_gate: None,
        }));
        let (events, _) = app.settle_tab_spawn_input(wid, true);
        replay(&mut app, wid, events);
        assert_eq!(drain(&new_pty), b"ls", "into the tab the menu opened");

        // A command that books nothing hands what waited back, in order.
        let mark = marks.mint(Instant::now());
        for text in ["q", "r"] {
            assert!(
                app.defer_tab_spawn_input(wid, winit_id, typed(text))
                    .is_none()
            );
        }
        let booked = app.tab_spawns.in_flight.len();
        assert!(marks.settle(mark));
        let back = app.claim_menu_held_input(mark, booked);
        assert_eq!(named(&back), ["q", "r"]);
        assert!(app.tab_spawns.held.is_empty());

        // Two posts before either dispatch: each takes what followed it, and
        // a new window leaves the pointer and a copy of the modifiers behind.
        let first = marks.mint(Instant::now());
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("a"))
                .is_none()
        );
        let second = marks.mint(Instant::now());
        for event in [typed("b"), mods(), moved(5.0)] {
            assert!(app.defer_tab_spawn_input(wid, winit_id, event).is_none());
        }
        let booked = app.tab_spawns.in_flight.len();
        let (_, one) =
            test_support::book_window(&mut app, None, SpawnAdmission::Plain, TabSpawnThen::Nothing);
        assert!(marks.settle(first));
        assert!(app.claim_menu_held_input(first, booked).is_empty());
        let booked = app.tab_spawns.in_flight.len();
        let (_, two) =
            test_support::book_window(&mut app, None, SpawnAdmission::Plain, TabSpawnThen::Nothing);
        assert!(marks.settle(second));
        let back = app.claim_menu_held_input(second, booked);
        assert_eq!(named(&back), ["mods", "move 5"]);
        let held: Vec<(WindowId, String)> = app
            .tab_spawns
            .held
            .iter()
            .map(|h| {
                (
                    h.target,
                    named(&[(h.typed_into, h.event.clone())]).remove(0),
                )
            })
            .collect();
        assert_eq!(
            held,
            [
                (one, "a".to_string()),
                (two, "b".to_string()),
                (two, "mods".to_string())
            ]
        );
        app.tab_spawns.held.clear();
        app.tab_spawns.in_flight.clear();

        // A mark past the hold holds nothing; handed-back input waits for none.
        let stale = Instant::now()
            .checked_sub(INPUT_HOLD + Duration::from_secs(1))
            .expect("a monotonic clock past the hold");
        let _ = marks.mint(stale);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("y"))
                .is_some()
        );
        let _ = marks.mint(Instant::now());
        let handed_back = app.replaying(false, |app| {
            app.defer_tab_spawn_input(wid, winit_id, typed("z"))
        });
        assert!(handed_back.is_some(), "typed before the pending mark");
    }

    /// What [`App::replay_settled_input`] does with `events` past the event
    /// loop: each goes through the hold again under the replay's floor, and
    /// a commit not held again reaches the window's active tab.
    fn redeliver(app: &mut App, wid: WindowId, events: Vec<(WinitWindowId, WindowEvent)>) {
        app.replaying(true, |app| {
            for (via, event) in events {
                if let Some(WindowEvent::Ime(Ime::Commit(text))) =
                    app.defer_tab_spawn_input(wid, via, event)
                {
                    app.on_ime_commit(wid, text);
                }
            }
        });
    }

    /// INPUT GOES TO THE SPAWN THAT WAS NEWEST WHEN IT WAS TYPED, whatever
    /// is booked after it: Cmd-T, `ls`, Cmd-T (the second through the native
    /// menu, whose wake no hold delays) runs `ls` in the first new tab and
    /// what follows in the second, each as its tab lands; and Cmd-T, `ls`,
    /// Cmd-N runs `ls` in the new tab, not in the new window booked after it
    /// was typed, while what is typed after Cmd-N still waits for the window.
    /// NEGATIVE CONTROL: replayed input still waits for a spawn it books
    /// itself (the replay's own Cmd-T).
    #[test]
    fn replayed_input_goes_to_the_spawn_that_was_newest_when_it_was_typed() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        app.winit_to_window.insert(winit_id, wid);
        let (old, old_pty) = piped_session(app.next_session_id);
        app.push_stub_tab(wid, old);

        // Cmd-T, `ls`, Cmd-T, `pwd`: two tabs in flight to one window.
        let (first, first_pty) = piped_session(app.next_session_id);
        let a = book(&mut app, wid, TabSpawnThen::Nothing);
        for text in ["l", "s"] {
            assert!(
                app.defer_tab_spawn_input(wid, winit_id, typed(text))
                    .is_none()
            );
        }
        let (second, second_pty) = piped_session(app.next_session_id);
        let b = book(&mut app, wid, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("pwd"))
                .is_none()
        );
        assert!(app.install_spawned_tab(TabSpawned {
            id: a,
            window: wid,
            session: Ok(first),
            reader_gate: None,
        }));
        let (events, _) = app.settle_landed_spawn_input(wid, Some(a), true);
        redeliver(&mut app, wid, events);
        assert_eq!(drain(&first_pty), b"ls", "the first tab's own keys, now");
        assert!(app.install_spawned_tab(TabSpawned {
            id: b,
            window: wid,
            session: Ok(second),
            reader_gate: None,
        }));
        let (events, _) = app.settle_landed_spawn_input(wid, Some(b), true);
        redeliver(&mut app, wid, events);
        assert_eq!(drain(&second_pty), b"pwd");
        assert!(drain(&old_pty).is_empty(), "the tab left gets none");

        // Cmd-T, `ls`, Cmd-N, `top`: the new window was booked after `ls`.
        let (third, third_pty) = piped_session(app.next_session_id);
        let c = book(&mut app, wid, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("ls"))
                .is_none()
        );
        let (_, window) =
            test_support::book_window(&mut app, None, SpawnAdmission::Plain, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("top"))
                .is_none()
        );
        assert!(app.install_spawned_tab(TabSpawned {
            id: c,
            window: wid,
            session: Ok(third),
            reader_gate: None,
        }));
        let (events, _) = app.settle_landed_spawn_input(wid, Some(c), true);
        redeliver(&mut app, wid, events);
        assert_eq!(drain(&third_pty), b"ls", "never the window booked after it");
        let waiting: Vec<_> = app.tab_spawns.held.iter().map(|h| h.target).collect();
        assert_eq!(waiting, [window], "`top` still waits for the window");
        app.tab_spawns.held.clear();
        app.tab_spawns.in_flight.clear();

        // A spawn the replay books takes what is replayed after it.
        let d = app.replaying(true, |app| {
            let d = book(app, wid, TabSpawnThen::Nothing);
            assert!(
                app.defer_tab_spawn_input(wid, winit_id, typed("x"))
                    .is_none()
            );
            d
        });
        assert_eq!(
            app.tab_spawns
                .held
                .iter()
                .map(|h| h.spawn)
                .collect::<Vec<_>>(),
            [Some(d)]
        );

        // Captured equal clock readings cannot decide which booking came
        // after replay began. Only the booking identity changes here.
        app.tab_spawns.held.clear();
        app.tab_spawns.in_flight.clear();
        let same_tick = Instant::now();
        let spare = app.next_session_id;
        app.next_session_id += 1; // an older spare's already-reserved id
        let before = book(&mut app, wid, TabSpawnThen::Nothing);
        app.tab_spawns.in_flight.last_mut().unwrap().asked = same_tick;
        app.replaying(true, |app| {
            assert_eq!(
                app.tab_spawns.hold_target(wid, same_tick),
                None,
                "older booking"
            );
            let fresh = book(app, wid, TabSpawnThen::Nothing);
            app.tab_spawns.in_flight.last_mut().unwrap().asked = same_tick;
            assert_eq!(
                app.tab_spawns.hold_target(wid, same_tick),
                Some((wid, same_tick + INPUT_HOLD, fresh)),
                "first booking made by this replay, despite the equal clock"
            );
            // The next ask adopts that older spare. Its fallback names the
            // fresh booking; the old spare id remains the held-input owner.
            let fallback = book(app, wid, TabSpawnThen::Nothing);
            let adopted = app.tab_spawns.in_flight.last_mut().unwrap();
            adopted.asked = same_tick;
            adopted.id = spare;
            adopted.alt_id = Some(fallback);
            assert_eq!(
                app.tab_spawns.hold_target(wid, same_tick),
                Some((wid, same_tick + INPUT_HOLD, spare)),
                "an adopted spare uses the new reservation for admission"
            );
        });
        // The menu's handed-back input deliberately keeps access to a
        // booking that preceded the menu post; that separate policy stays.
        app.tab_spawns.in_flight.retain(|t| t.id == before);
        app.replaying(false, |app| {
            assert_eq!(
                app.tab_spawns.hold_target(wid, same_tick),
                Some((wid, same_tick + INPUT_HOLD, before))
            );
        });
        app.tab_spawns.in_flight.clear();
    }
}
