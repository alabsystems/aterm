// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Off-loop PTY spawning for new tabs, splits, and windows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_window::event::{InputOrigin, WindowEvent};
use aterm_window::event_loop::{ActiveEventLoop, EventLoopProxy};
use aterm_window::keyboard::ModifiersState;
use aterm_window::window::WindowId as WinitWindowId;

use crate::app_take_over::GestureMarks;
use crate::platform::AppRt as _;
use crate::{App, Session, Wake, WindowId};

const SPAWN_HOLD: Duration = Duration::from_secs(2 * aterm_pty::START_HANG.as_secs());

const MENU_HOLD: Duration = aterm_pty::START_HANG;

const FORK_PARK_WAIT: Duration = Duration::from_millis(3);

const SWEEP_HOLD: Duration = Duration::from_millis(15);

const MAX_HELD_INPUT: usize = crate::MAX_DEFERRED_HANDOFF_INPUT;

const NO_NEW_WINDOW: &str = "could not open a new window";

const QUITTING: &str = "aterm is quitting before the session started";

/// Who waits for a tab, split, or window spawned off the loop.
pub(crate) enum TabSpawnThen {
    /// Interactive action where the tab or pane itself is the answer.
    Nothing,
    /// Wire `spawn`: replies `OK <sid>` once registered (raising `window` if asked) or `ERR <why>`.
    Reply { reply: SpawnReply, raise: bool },
    /// Wire `tab new`: replies `OK <active> <count>` once installed or `ERR <why>`.
    Counts(Sender<Result<(usize, usize), String>>),
    /// Main-thread callback invoked with the registered `sid` or error reason.
    Run(SpawnDone),
    /// Recheck a deferred request before installing a successful spawn or moving focus.
    /// A refusal quietly drops the session and answers `then` with an error.
    Before { check: SpawnCheck, then: SpawnDone },
}

/// Main-thread callback run once an off-loop spawn settles.
pub(crate) type SpawnDone = Box<dyn FnOnce(&mut App, Result<String, String>)>;

/// Main-thread check of the destination before a successful spawn is installed.
type SpawnCheck = Box<dyn Fn(&mut App, WindowId) -> bool>;

impl TabSpawnThen {
    fn may_land(&self, app: &mut App, window: WindowId) -> bool {
        match self {
            Self::Before { check, .. } => check(app, window),
            _ => true,
        }
    }

    /// Deliver `outcome` to the waiter, raising `window` first when requested.
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
                answer_ctl_spawn(app, reply, outcome);
            }
            Self::Counts(reply) => {
                if let Ok(sid) = &outcome {
                    crate::control::mark_ctl_spawned(&app.store, sid);
                }
                let counts = outcome.map(|_| {
                    app.windows.get(&window).map_or((0, 0), |ws| {
                        (ws.tab_set.active_index().unwrap_or(0), ws.tab_set.len())
                    })
                });
                let _ = reply.send(counts);
            }
            Self::Run(then) | Self::Before { then, .. } => then(app, outcome),
        }
    }
}

/// A wire spawn's answer channel, and the request its tab joins where it has one
/// (ATERM-KI-1109-3): answering settles the request first, on the main thread, so the tab is
/// queued for the request journal before anything can queue that tab's leaving.
pub(crate) struct SpawnReply {
    tx: Sender<Result<String, String>>,
    request: Option<crate::requests_host::Pending>,
}

impl SpawnReply {
    /// The answer `tx` takes, settling `request`.
    pub(crate) fn new(
        tx: Sender<Result<String, String>>,
        request: Option<crate::requests_host::Pending>,
    ) -> Self {
        Self { tx, request }
    }

    /// Settle the request with `outcome` ([`crate::requests_host::Pending::settle`]), then send
    /// `outcome` to the control thread.
    ///
    /// # Errors
    /// The control thread no longer waits for it.
    pub(crate) fn send(
        mut self,
        outcome: Result<String, String>,
    ) -> Result<(), std::sync::mpsc::SendError<Result<String, String>>> {
        if let Some(request) = self.request.take() {
            request.settle(outcome.as_ref().ok().map(String::as_str));
        }
        self.tx.send(outcome)
    }
}

impl From<Sender<Result<String, String>>> for SpawnReply {
    fn from(tx: Sender<Result<String, String>>) -> Self {
        Self::new(tx, None)
    }
}

/// Answer a control client's spawn with `outcome`, first marking the session it started as
/// `ctl spawn`'s ([`crate::SessionCtx::spawn_mark`]), so its `reveal` says `spawned` from the
/// moment the client can name it. Whether the client took the answer.
pub(crate) fn answer_ctl_spawn(
    app: &App,
    reply: SpawnReply,
    outcome: Result<String, String>,
) -> bool {
    if let Ok(sid) = &outcome {
        crate::control::mark_ctl_spawned(&app.store, sid);
    }
    reply.send(outcome).is_ok()
}

/// Where a session spawned off the loop lands in its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpawnPlace {
    /// A new tab. A worker the ask parks ([`SpawnAsk::parks`]) goes at the end of the parked tabs,
    /// behind the front. Any other goes at the end of the shown ones, ahead of the parked tabs,
    /// and is made active unless the ask lands it behind ([`SpawnAsk::background`]).
    Tab,
    /// A new pane splitting the window's focused pane in this direction.
    Split(crate::pane::SplitDir),
    /// The sole tab of a new window built on landing (`from` is the origin window).
    Window { from: Option<WindowId> },
}

impl SpawnPlace {
    const fn asked_from(self, window: WindowId) -> Option<WindowId> {
        match self {
            Self::Window { from } => from,
            Self::Tab | Self::Split(_) => Some(window),
        }
    }

    /// Log why the spawn failed and, for a tab a person asked for, tell them; a tab nobody asked
    /// for ([`SpawnAsk::unasked`]) asks nobody to open it again, and its caller logs it.
    fn failed(self, app: &mut App, error: &str, unasked: bool) -> String {
        match self {
            Self::Tab => {
                crate::logging::stderr_line!("aterm-gui: could not open a new tab: {error}");
                if !unasked {
                    app.post_message(crate::message_reporters::new_tab_failed(error));
                }
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

/// OS hooks for landing a spawn (inert in unit tests).
pub(crate) trait LandingOs {
    /// Whether the event loop is exiting.
    fn exiting(&self) -> bool;
    /// Create window `wid`'s OS window and present surface.
    fn attach(&self, app: &mut App, wid: WindowId) -> bool;
}

impl LandingOs for ActiveEventLoop {
    fn exiting(&self) -> bool {
        ActiveEventLoop::exiting(self)
    }

    fn attach(&self, app: &mut App, wid: WindowId) -> bool {
        app.headless || app.attach_os_window(self, wid)
    }
}

/// One spawn job executed by the spawner thread.
pub(crate) struct TabSpawnJob {
    id: u64,
    window: WindowId,
    rows: u16,
    cols: u16,
    cell_px: Option<(u16, u16)>,
    factory: crate::spawn::SessionFactory,
    proxy: EventLoopProxy<Wake>,
    /// Explicit cwd override; otherwise resolved from `pane_folders`.
    cwd: Option<String>,
    pane_folders: Vec<crate::spawn_folder::PaneFolder>,
    /// The session a controller-as-tab preset observes.
    observe: Option<aterm_session::SessionId>,
    /// A worker's task file, written by `spawn_session` once the sid is known.
    task: Option<crate::task_launch::TaskSeed>,
    completion: Arc<TabSpawnCompletion>,
    /// Warm spare to validate and adopt, falling back to a cold spawn on miss.
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
    /// Target folder a parked spare `cd`s to on release.
    pub(crate) retarget: Arc<OnceLock<String>>,
}

/// Why the control verb's `split=` lands nowhere beside a worker on show.
pub(crate) const WORKER_SPLIT_REFUSED: &str = "spawn split= does not land beside a worker under the \
     Workers tab: choose Keep as Tab on its row first, or split a person's tab";

/// Admission facts captured when a spawn is booked on the main thread.
#[derive(Clone, Default)]
pub(crate) enum SpawnAdmission {
    #[default]
    Plain,
    Connected {
        origin: aterm_session::SessionId,
        local: u64,
    },
    /// Single-flight reservation for `App::summarize_in_new_tab`.
    Summary,
}

impl SpawnAdmission {
    /// Whether a worker asked for this way parks, by who asked. A plain ask is `spawn agent=`, the
    /// disk clean-up worker's or the credit claim worker's: nobody asked to see it. A person's
    /// Summarize asked for a new tab, and a connected spawn's tab belongs beside its origin.
    pub(crate) const fn parks_worker(&self) -> bool {
        match self {
            Self::Plain => true,
            Self::Connected { .. } | Self::Summary => false,
        }
    }
}

/// Parameters for an off-loop spawn request.
#[derive(Default)]
pub(crate) struct SpawnAsk {
    pub(crate) admission: SpawnAdmission,
    /// Program and argv to start without configured-shell integration or global `-e`.
    /// This starts on the worker before landing: a caller deferring effects until admission
    /// must start only an inert waiting shell here, then submit its work after landing.
    pub(crate) exec_command: Option<Vec<std::ffi::OsString>>,
    /// Explicit cwd (`None` inherits `folder_of` or the focused pane's folder).
    pub(crate) cwd: Option<String>,
    /// Origin pane folder to inherit before the focused pane's.
    pub(crate) folder_of: Option<crate::spawn_folder::PaneFolder>,
    /// Session id for `ATERM_OBSERVE_SESSION_ID`.
    pub(crate) observe: Option<aterm_session::SessionId>,
    /// Worker task file seed (`ATERM_TASK_FILE`), cleaned up on failure.
    pub(crate) task: Option<crate::task_launch::TaskSeed>,
    /// A driver-owned plain shell: clear a global command and require the explicit cwd.
    pub(crate) shell: bool,
    /// Asked over another Mac's fleet drive, a worker or a plain shell: it parks under the
    /// Workers tab and names no lineage parent, never the tab the person happened to have focused
    /// (ATERM-KI-1109-3).
    pub(crate) fleet: bool,
    /// Land a new tab behind the active tab without moving focus.
    pub(crate) background: bool,
    /// A tab nobody asked for (System State's Command Line Tools install,
    /// [`crate::clt_install`]): the window's input while it spawns stays where it was typed,
    /// never held to replay into the new tab, as a person's own new tab's typing is.
    pub(crate) unasked: bool,
    /// Explicit session theme (`None` inherits the divided pane's theme on a split).
    pub(crate) theme: Option<crate::session_theme::SessionTheme>,
    /// The control verb's `split=`: refused where it would land beside a worker on show
    /// ([`WORKER_SPLIT_REFUSED`]), unlike a person's own split of the pane in front of them.
    pub(crate) refuses_worker_split: bool,
}

impl SpawnAsk {
    /// Whether this ask, landing as a tab, parks ([`crate::tab_model::Tab::parked`]): a worker's,
    /// asked for by whoever [`SpawnAdmission::parks_worker`] says, and anything a fleet drive asks
    /// for ([`Self::fleet`]).
    pub(crate) const fn parks(&self) -> bool {
        (self.task.is_some() || self.fleet) && self.admission.parks_worker()
    }

    /// A remote driver's fresh shell never holds the person's input to its hosting window.
    pub(crate) fn for_fleet_shell(mut self) -> Self {
        self.shell = true;
        self.unasked = true;
        self
    }

    /// A plain ask inheriting the focused pane's folder unless named.
    pub(crate) fn new(cwd: Option<&str>) -> Self {
        Self {
            cwd: cwd.map(str::to_owned),
            ..Self::default()
        }
    }

    /// A wire spawn's explicitly named folder.
    pub(crate) fn requested(cwd: Option<String>) -> Self {
        Self {
            cwd,
            ..Self::default()
        }
    }
}

/// Capture this request's program without changing the factory other tabs inherit.
/// A worker clears the inherited command; an explicit command always wins.
fn tab_spawn_factory(
    mut factory: crate::spawn::SessionFactory,
    exec_command: Option<Vec<std::ffi::OsString>>,
    own_shell: bool,
) -> crate::spawn::SessionFactory {
    if let Some(command) = exec_command {
        factory.exec_command = Some(command);
        factory.integrate = false;
    } else if own_shell {
        // A requested worker gets its own shell, not the host instance's `-e` command.
        // The explicit --no-shell-integration policy remains the factory's.
        factory.exec_command = None;
    }
    factory.reader_after_install = true;
    factory
}

/// A plain drive owns a new shell, not an idle spare with another session's history, and what
/// another Mac's drive asks for starts with that Mac's mark in its environment
/// ([`for_fleet_drive`]), which a shell already running has not.
fn can_adopt_spare(ask: &SpawnAsk) -> bool {
    ask.task.is_none() && !ask.shell && !ask.fleet
}

/// `factory` for a tab another Mac's fleet drive asked for ([`SpawnAsk::fleet`]): everything
/// the tab runs carries the mark of another machine's request
/// (`aterm_agent::harness::remote_renew::FOR_PEER_ENV`), so a sign-in started in it opens no
/// window and no browser on this Mac's desk. It is in the environment from the tab's first
/// process on, which a mark made when the spawn is answered would not be.
fn for_fleet_drive(mut factory: crate::spawn::SessionFactory) -> crate::spawn::SessionFactory {
    let mark = aterm_agent::harness::remote_renew::FOR_PEER_ENV;
    factory.env_add.retain(|(name, _)| name != mark);
    factory.env_add.push((mark.to_owned(), "1".to_owned()));
    factory
}

/// Result returned by the spawner thread ([`crate::Wake::TabSpawned`]).
pub(crate) struct TabSpawned {
    /// The reserved session id.
    pub(crate) id: u64,
    /// The window the tab lands in.
    pub(crate) window: WindowId,
    /// The session, or why there is none.
    pub(crate) session: std::io::Result<Session>,
    /// Gate parking the session's reader until install (`None` off unix).
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
    /// When the spawner started on the job ([`TabSpawnCompletion::note_picked`]).
    picked: Option<Instant>,
}

/// Shared slot holding a spawn job's result or cancellation state.
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

    fn note_picked(&self, at: Instant) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).picked = Some(at);
    }

    fn picked(&self) -> Option<Instant> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).picked
    }

    fn verdict_in(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .ready
            .is_some()
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
        drop(rejected);
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
        drop(done);
    }
}

struct InFlight {
    id: u64,
    /// Fallback session id if an adopted warm spare misses on the spawner.
    alt_id: Option<u64>,
    /// Target window id (reserved ahead of time for a new window).
    window: WindowId,
    place: SpawnPlace,
    asked: Instant,
    /// Lineage parent session id.
    parent: Option<aterm_session::SessionId>,
    /// Engine color generation (`App::engine_color_fanouts`) when booked.
    colors: u64,
    /// Grid and cell box at which the session was spawned.
    grid: SpawnGrid,
    then: TabSpawnThen,
    admission: SpawnAdmission,
    /// [`SpawnAsk::background`]: a tab lands behind the window's active one.
    background: bool,
    /// [`SpawnAsk::parks`]: a tab lands parked under the window's Workers tab.
    parks: bool,
    /// [`SpawnAsk::fleet`]: the parked tab is a fleet drive's ([`crate::tab_model::Tab::fleet`]).
    fleet: bool,
    /// [`SpawnAsk::refuses_worker_split`].
    refuses_worker_split: bool,
    /// [`SpawnAsk::unasked`]: the window holds no input for it.
    unasked: bool,
    completion: Arc<TabSpawnCompletion>,
    /// The newborn's own theme, as asked ([`SpawnAsk::theme`]).
    theme: Option<crate::session_theme::SessionTheme>,
    /// The act it was booked under ([`crate::tab_marks::acting`]), which its tab's open names.
    acting: Option<crate::tab_marks::Acting>,
}

type SpawnGrid = (u16, u16, Option<(u16, u16)>);

/// One input event a window holds for a spawn in flight.
#[derive(Debug)]
struct Held {
    /// The OS window it was typed into.
    typed_into: WinitWindowId,
    /// The window it is for: its own, or the new window asked from it.
    target: WindowId,
    /// In-flight spawn id waited on, or `None` while waiting on [`Self::menu`].
    spawn: Option<u64>,
    event: WindowEvent,
    /// Deadline after which the event is dropped: [`MENU_HOLD`] from the menu
    /// mark's post, or for a spawn [`SPAWN_HOLD`] from its start, brought up
    /// to date as the spawner moves ([`TabSpawns::refresh_holds`]).
    until: Instant,
    /// Pending menu mark waited on before dispatch ([`MenuMark::NONE`] once resolved).
    menu: MenuMark,
    /// Whether a modifier change for a new window was already applied to `typed_into`.
    applied: bool,
    /// Where it came from, decided as it arrived (`App::input_origin`): its replay runs marked
    /// as that then, whatever event AppKit is dispatching.
    origin: InputOrigin,
}

/// A held input event released for replay ([`App::replay_window_event`]): the OS window it goes
/// through, the event, and where it came from ([`Held::origin`]).
pub(crate) type Released = (WinitWindowId, WindowEvent, InputOrigin);

const fn is_pointer(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::CursorMoved { .. }
            | WindowEvent::CursorEntered { .. }
            | WindowEvent::CursorLeft { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
    )
}

const fn is_keystroke(event: &WindowEvent) -> bool {
    matches!(event, WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed())
        || matches!(event, WindowEvent::Ime(aterm_window::event::Ime::Commit(_)))
}

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

fn sift_held(
    held: Vec<Held>,
    landed: bool,
    through: Option<WinitWindowId>,
    now: Instant,
) -> (Vec<Released>, u32) {
    let mut replay = Vec::with_capacity(held.len());
    let mut dropped = 0u32;
    let mut pressed = Vec::new();
    for h in held {
        if landed && now < h.until {
            let via = if is_pointer(&h.event) {
                h.typed_into
            } else {
                through.unwrap_or(h.typed_into)
            };
            replay.push((via, h.event, h.origin));
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
            replay.push((h.typed_into, h.event, h.origin));
        }
    }
    (replay, dropped)
}

/// In-flight spawns and input held while they start.
#[derive(Default)]
pub(crate) struct TabSpawns {
    in_flight: Vec<InFlight>,
    /// Input events held for in-flight spawns or pending menu marks, in arrival order.
    held: Vec<Held>,
    /// Keystrokes refused past [`MAX_HELD_INPUT`], per window.
    dropped: Vec<(WindowId, u32)>,
    /// Windows needing a modifier snapshot clear after replay due to dropped keys or focus loss.
    incoherent: Vec<WindowId>,
    /// The menu commands posted and not yet dispatched ([`MenuMarks`]).
    pub(crate) menu: Arc<MenuMarks>,
    /// Active replay floor preventing replayed input from waiting on earlier bookings.
    floor: Option<ReplayFloor>,
    /// A test's app with no event loop books each spawn as one with a loop would and sends no
    /// job: the test lands it ([`App::install_spawned_tab`]).
    #[cfg(test)]
    pub(crate) unsent: bool,
}

/// Cutoff marks so replayed input waits only for spawns or menu posts triggered during the replay.
#[derive(Clone, Copy, Debug)]
struct ReplayFloor {
    /// Highest menu mark minted when replay began.
    marks: u64,
    /// First fresh booking id of the replay (`None` when releasing menu-held input).
    spawns: Option<u64>,
}

impl TabSpawns {
    /// Whether no spawn is queued or running.
    pub(crate) const fn is_empty(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// Whether a Summarize-in-New-Tab spawn is currently in flight.
    pub(crate) fn summary_pending(&self) -> bool {
        self.in_flight
            .iter()
            .any(|t| matches!(t.admission, SpawnAdmission::Summary))
    }

    /// Deadline until which the status sweep is held for a recently booked spawn.
    pub(crate) fn status_sweep_held_until(&self, now: Instant) -> Option<Instant> {
        self.in_flight
            .iter()
            .map(|t| t.asked + SWEEP_HOLD)
            .max()
            .filter(|at| *at > now)
    }

    fn hold_target(&self, window: WindowId, now: Instant) -> Option<(WindowId, Instant, u64)> {
        let after = self.floor.and_then(|floor| floor.spawns);
        self.in_flight
            .iter()
            .rev()
            .find(|t| {
                !t.unasked
                    && t.place.asked_from(t.window) == Some(window)
                    && now < self.hold_end(t, now)
                    && after.is_none_or(|first| t.alt_id.unwrap_or(t.id) >= first)
            })
            .map(|t| (t.window, self.hold_end(t, now), t.id))
    }

    fn hold_from(&self, t: &InFlight) -> Instant {
        self.in_flight
            .iter()
            .take_while(|o| o.id != t.id)
            .chain(std::iter::once(t))
            .filter_map(|o| o.completion.picked())
            .fold(t.asked, Instant::max)
    }

    fn hold_end(&self, t: &InFlight, now: Instant) -> Instant {
        if t.completion.verdict_in() {
            now + SPAWN_HOLD
        } else {
            self.hold_from(t) + SPAWN_HOLD
        }
    }

    fn refresh_holds(&mut self, now: Instant) {
        let ends: Vec<(u64, Instant)> = self
            .in_flight
            .iter()
            .map(|t| (t.id, self.hold_end(t, now)))
            .collect();
        for h in &mut self.held {
            if let Some(&(_, end)) = h
                .spawn
                .and_then(|id| ends.iter().find(|(spawn, _)| *spawn == id))
            {
                h.until = h.until.max(end);
            }
        }
    }

    fn hold(&mut self, held: Held) {
        if let Some(last) = self.held.last_mut()
            && last.typed_into == held.typed_into
            && last.target == held.target
            && last.spawn == held.spawn
            && last.menu == held.menu
            && last.origin == held.origin
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

    /// Mark `window` (and any new window requested from it) for modifier repair after focus loss.
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

/// Identifier for a posted [`Wake::MenuAction`] ([`Self::NONE`] when unmarked).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MenuMark(u64);

impl MenuMark {
    /// No mark: nothing waits for its dispatch.
    pub(crate) const NONE: Self = Self(0);
}

/// Tracks posted menu commands not yet dispatched so input typed meanwhile waits for them.
#[derive(Debug, Default)]
pub(crate) struct MenuMarks(Mutex<MarksState>);

#[derive(Debug, Default)]
struct MarksState {
    /// The last mark given out; marks count from one.
    minted: u64,
    /// Pending `(mark, posted_at)` pairs in post order.
    pending: Vec<(MenuMark, Instant)>,
}

impl MenuMarks {
    fn mint(&self, now: Instant) -> MenuMark {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        state
            .pending
            .retain(|(_, at)| now.saturating_duration_since(*at) < MENU_HOLD);
        state.minted += 1;
        let mark = MenuMark(state.minted);
        state.pending.push((mark, now));
        mark
    }

    /// Complete `mark`, returning `true` if it was still pending.
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

    fn pending(&self, now: Instant, after: u64) -> Option<(MenuMark, Instant)> {
        let state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        state
            .pending
            .last()
            .copied()
            .filter(|(mark, at)| mark.0 > after && now.saturating_duration_since(*at) < MENU_HOLD)
    }

    fn minted(&self) -> u64 {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).minted
    }
}

static NATIVE_MENU_MARKS: OnceLock<Arc<MenuMarks>> = OnceLock::new();

/// The window and focused leaf from which a palette posted a menu command, and whose gesture
/// chose it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MenuOrigin {
    window: WindowId,
    content: Option<crate::front_content::FrontContent>,
    /// The marks of the gesture that chose the row: the command runs under them
    /// ([`App::dispatch_marked_menu_action`]), so a person-only act in a control client's
    /// refuses the client, and an edit an accessibility client's or another process's input
    /// makes is no person's ([`crate::owner_keys_save`]).
    chooser: GestureMarks,
}

#[cfg(target_os = "macos")]
impl MenuOrigin {
    pub(crate) const fn window(self) -> WindowId {
        self.window
    }

    /// A deferred person-owned operation may land only in the content that asked.
    pub(crate) fn is_current(self, app: &App) -> bool {
        app.frontmost_window == Some(self.window)
            && self.content.is_some()
            && app.palette_menu_origin(self.window).map(|now| now.content) == Some(self.content)
    }

    /// A completed child cannot act for a window or content that was closed or replaced.
    pub(crate) fn is_retained(self, app: &App) -> bool {
        let Some(content) = self.content else {
            return false;
        };
        let view = content.view();
        if !app.windows.get(&self.window).is_some_and(|window| {
            window
                .tab_set
                .tabs()
                .iter()
                .any(|tab| tab.root.contains(view))
        }) {
            return false;
        }
        match (content, app.view_store.get(view).copied()) {
            (
                crate::front_content::FrontContent::Terminal { session, .. },
                Some(crate::tab_model::View::Terminal(terminal)),
            ) => terminal.session == session && app.pool.get(session).is_some(),
            (
                crate::front_content::FrontContent::Native { instance, .. },
                Some(crate::tab_model::View::Native(native)),
            ) => native.instance == instance,
            _ => false,
        }
    }
}

#[cfg(test)]
impl MenuOrigin {
    /// Whether a control client's gesture chose the row.
    #[cfg(unix)]
    pub(crate) const fn controller(self) -> bool {
        self.chooser.controller
    }

    /// Whether an accessibility client's request chose the row.
    pub(crate) const fn assistive(self) -> bool {
        self.chooser.assistive
    }

    /// Whether a key or click another process posted chose the row.
    pub(crate) const fn relayed(self) -> bool {
        self.chooser.relayed
    }
}

/// Serve `marks` to the native menu and toolbar's posts.
pub(crate) fn serve_native_menu_marks(marks: &Arc<MenuMarks>) {
    let _ = NATIVE_MENU_MARKS.set(Arc::clone(marks));
}

/// Post `action` as a [`Wake::MenuAction`] recorded in `marks`, run under `gesture`, the marks
/// of the gesture that posted it.
pub(crate) fn post_menu_action(
    proxy: &EventLoopProxy<Wake>,
    marks: Option<&MenuMarks>,
    action: crate::menu::MenuAction,
    gesture: GestureMarks,
) -> bool {
    post_menu_action_from(proxy, marks, action, None, gesture)
}

/// Post `action` as a [`Wake::MenuAction`], run under `gesture` and the marks of `origin`'s
/// chooser ([`App::run_posted_menu_action`]).
fn post_menu_action_from(
    proxy: &EventLoopProxy<Wake>,
    marks: Option<&MenuMarks>,
    action: crate::menu::MenuAction,
    origin: Option<MenuOrigin>,
    gesture: GestureMarks,
) -> bool {
    let mark = marks.map_or(MenuMark::NONE, |marks| marks.mint(Instant::now()));
    let sent = proxy
        .send_event(Wake::MenuAction {
            action,
            mark,
            origin,
            gesture,
        })
        .is_ok();
    if !sent && let Some(marks) = marks {
        marks.settle(mark);
    }
    sent
}

/// Post `action` from a native menu or toolbar callback using [`NATIVE_MENU_MARKS`], marked by
/// where the key or click that chose it came from ([`native_post_origin`]).
#[cfg(target_os = "macos")]
pub(crate) fn post_native_menu_action(
    proxy: &EventLoopProxy<Wake>,
    action: crate::menu::MenuAction,
) -> bool {
    post_menu_action_from(
        proxy,
        NATIVE_MENU_MARKS.get().map(Arc::as_ref),
        action,
        None,
        GestureMarks::of(native_post_origin()),
    )
}

/// Where the native menu or toolbar item AppKit runs now was chosen from: the key that chose it
/// by its key equivalent (⌘V is Edit ▸ Paste) or the click that chose it, one an `hwkey` burst
/// posted (a control client's gesture) or another process posted (relayed), which takes the
/// person's own event path with no mark of its own ([`crate::hwkey::current_event_origin`]).
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) fn native_post_origin() -> InputOrigin {
    crate::hwkey::current_event_origin()
}

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

/// Delay the status observer's `next` wake until `held` expires.
pub(crate) fn held_status_wake(next: Option<Instant>, held: Option<Instant>) -> Option<Instant> {
    match (next, held) {
        (Some(wake), Some(at)) => Some(wake.max(at)),
        (wake, _) => wake,
    }
}

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
        observe,
        task,
        completion,
        adopt,
    } = job;
    if !start_picked(&completion, Instant::now()) {
        return;
    }
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
    let (session, reader_gate) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let cwd = resolved.unwrap_or_else(|| {
            cwd.or_else(|| crate::spawn_folder::PaneFolder::first_taken(pane_folders))
        });
        let session = state_answers().and_then(|()| {
            crate::watchdog::forks_wait_for_park(park_wait, || {
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
                    task.as_ref(),
                    None,
                )
            })
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

/// Whether [`run`] goes on with a job at `at`: not when cancelled; else the pick is noted, so the hold on the tab's keys runs from it.
fn start_picked(completion: &TabSpawnCompletion, at: Instant) -> bool {
    if completion.cancelled() {
        return false;
    }
    completion.note_picked(at);
    true
}

fn state_answers() -> std::io::Result<()> {
    static LOCAL: OnceLock<bool> = OnceLock::new();
    state_answers_with(
        aterm_types::dirs::state_dir().as_deref(),
        |state| *LOCAL.get_or_init(|| !crate::spawn_folder::on_network_volume(state)),
        crate::spawn_folder::volume_answers,
    )
}

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

fn publish_completion(
    completion: &Arc<TabSpawnCompletion>,
    done: TabSpawned,
    post: impl FnOnce(Wake) -> bool,
) {
    if !completion.store(done) {
        return;
    }
    if !post(Wake::TabSpawned(Arc::clone(completion))) {
        completion.cancel();
    }
}

/// Start `session`'s reader and helpers parked on a closed gate opened at install.
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

/// Hold a tab's spawn until a test releases it. The test waits on the `.held` marker's being
/// there while this process runs, so it is whole by its rename and flushed by nothing.
fn hold_for_test(completion: &TabSpawnCompletion) {
    let Some(path) = aterm_types::dev_seam!("ATERM_DEBUG_SPAWN_HOLD") else {
        return;
    };
    let mut held = path.clone();
    held.push(".held");
    let _ = aterm_uds::atomic_file::write_flushed(std::path::Path::new(&held), b"", 0o600, None);
    let path = std::path::PathBuf::from(path);
    let until = Instant::now() + Duration::from_secs(60);
    while !completion.cancelled() && !path.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl App {
    /// Cancel in-flight spawns, join the spawner thread, and drop any uninstalled sessions at exit.
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

    /// Book a new tab in `owner` and dispatch its shell spawn off the event loop.
    pub(crate) fn open_tab_off_loop(
        &mut self,
        owner: WindowId,
        cwd_override: Option<&str>,
        then: TabSpawnThen,
    ) {
        let ask = SpawnAsk::new(cwd_override);
        self.open_off_loop(owner, SpawnPlace::Tab, ask, then);
    }

    /// Book a split of `owner`'s focused pane in `dir` and dispatch its shell spawn off the loop.
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
        let Some(proxy) = self.spawn_proxy() else {
            then.answer(self, owner, Err("no event loop to spawn on".to_string()));
            return;
        };
        let grid = self.spawn_grid(owner);
        let parent = self.spawn_parent(owner, &ask);
        self.book_off_loop(owner, place, grid, parent, ask, then, proxy);
    }

    /// The lineage parent of a tab `ask` opens in `owner`: the window's focused session, but none
    /// for a fleet drive's ask ([`SpawnAsk::fleet`]), which is no child of whatever tab the person
    /// happened to have focused.
    pub(crate) fn spawn_parent(
        &self,
        owner: WindowId,
        ask: &SpawnAsk,
    ) -> Option<aterm_session::SessionId> {
        if ask.fleet {
            None
        } else {
            self.new_tab_parent(owner)
        }
    }

    /// Book a new window on the current turn (ignored in headless mode).
    pub(crate) fn open_new_window(&mut self) {
        if self.headless {
            aterm_log::info!("headless: ignoring a new window");
            return;
        }
        self.open_window_off_loop(SpawnAsk::default(), None, TabSpawnThen::Nothing);
    }

    /// Reserve a new window and session id and dispatch its first shell spawn off the event loop.
    pub(crate) fn open_window_off_loop(
        &mut self,
        ask: SpawnAsk,
        lineage_parent: Option<aterm_session::SessionId>,
        then: TabSpawnThen,
    ) {
        let (window, place, grid) = self.reserve_window();
        let Some(proxy) = self.spawn_proxy() else {
            let why = place.failed(self, "no event loop to host the new window", false);
            then.answer(self, window, Err(why));
            return;
        };
        self.book_off_loop(window, place, grid, lineage_parent, ask, then, proxy);
    }

    /// Where a booking's job posts its landing: the event loop's proxy, or, in a test's app that
    /// books unsent ([`TabSpawns::unsent`]), nowhere; `None` where no spawn can be booked.
    fn spawn_proxy(&self) -> Option<Option<EventLoopProxy<Wake>>> {
        #[cfg(test)]
        if self.proxy.is_none() && self.tab_spawns.unsent {
            return Some(None);
        }
        self.proxy.clone().map(Some)
    }

    fn reserve_window(&mut self) -> (WindowId, SpawnPlace, SpawnGrid) {
        let place = SpawnPlace::Window {
            from: self.frontmost_window,
        };
        let window = WindowId(self.next_window_id);
        self.next_window_id += 1;
        let ((rows, cols), _) = self.new_window_frame(None);
        (window, place, (rows, cols, self.spawn_cell_px(window)))
    }

    /// Reserve a session id, record the in-flight spawn, and send the job to the spawner thread.
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
        proxy: Option<EventLoopProxy<Wake>>,
    ) {
        let inherited = self.inherited_theme(window, place, ask.theme.as_ref());
        let tried = if inherited.is_some() {
            Err("theme")
        } else if !can_adopt_spare(&ask) {
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
        let parks = ask.parks();
        let fleet_parks = parks && ask.fleet;
        let SpawnAsk {
            admission,
            exec_command,
            cwd,
            folder_of,
            observe,
            task,
            shell,
            fleet,
            background,
            unasked,
            theme,
            refuses_worker_split,
        } = ask;
        let theme = theme.or(inherited);
        self.note_update_handoff_activity();
        let id = self.next_session_id;
        self.next_session_id += 1;
        let (rows, cols, cell_px) = grid;
        let mut colors = self.engine_color_fanouts;
        let (inflight_id, alt_id, adopt) = match adopted {
            Some((slot, session)) => {
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
        let mut factory = tab_spawn_factory(
            self.session_factory.clone(),
            exec_command,
            task.is_some() || shell,
        );
        factory.require_cwd = shell;
        let factory = if fleet {
            for_fleet_drive(factory)
        } else {
            factory
        };
        #[cfg(windows)]
        let factory = {
            let mut factory = factory;
            factory.shell_appearance =
                self.spawn_shell_appearance(theme.as_ref().map(|theme| theme.selector.as_str()));
            factory
        };
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
            parks,
            fleet: fleet_parks,
            refuses_worker_split,
            unasked,
            completion: Arc::clone(&completion),
            theme,
            acting: crate::tab_marks::acting(),
        });
        // A test's unsent booking stands as booked; the test lands it.
        let Some(proxy) = proxy else {
            return;
        };
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
            observe,
            task,
            completion,
            adopt,
        };
        if let Some(job) = send_to_spawner(job) {
            run(job, Duration::ZERO);
        }
    }

    /// Handle [`crate::Wake::TabSpawned`]: install the session, answer its waiter, settle input.
    pub(crate) fn land_tab_spawn(
        &mut self,
        el: &ActiveEventLoop,
        completion: Arc<TabSpawnCompletion>,
    ) {
        let Some(done) = completion.take() else {
            return;
        };
        let window = done.window;
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

    /// Settle input held for `spawn` in `window` and replay or repair modifiers as needed.
    pub(crate) fn settle_landed_spawn_input(
        &mut self,
        window: WindowId,
        spawn: Option<u64>,
        landed: bool,
    ) -> (Vec<Released>, Option<WindowId>) {
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

    /// Replay settled `events` under [`ReplayFloor`] and clear `repair`'s modifier snapshot.
    pub(crate) fn replay_settled_input(
        &mut self,
        el: &ActiveEventLoop,
        replay: Vec<Released>,
        repair: Option<WindowId>,
    ) {
        self.replaying(true, |app| {
            for (winit_id, event, origin) in replay {
                app.replay_window_event(el, winit_id, event, origin);
            }
        });
        if let Some(ws) = repair.and_then(|at| self.windows.get_mut(&at)) {
            ws.mods = ModifiersState::empty();
        }
    }

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

    /// Post `action` as a marked [`Wake::MenuAction`] on this App's event loop, run under the
    /// marks of the gesture being dispatched now ([`Self::gesture_marks`]): a posted ⌘Q, or a
    /// `pointer` press on the version slot, posts its action as the client's.
    pub(crate) fn post_menu_action(&self, action: crate::menu::MenuAction) -> bool {
        let gesture = self.gesture_marks();
        self.proxy.as_ref().is_some_and(|proxy| {
            post_menu_action(proxy, Some(&self.tab_spawns.menu), action, gesture)
        })
    }

    /// Where a palette row is chosen in `window`, and the marks of the gesture choosing it.
    pub(crate) fn palette_menu_origin(&self, window: WindowId) -> Option<MenuOrigin> {
        self.windows.get(&window).map(|state| MenuOrigin {
            window,
            content: state.front_content,
            chooser: self.gesture_marks(),
        })
    }

    /// A palette command cannot follow a later focus, tab or pane change.
    pub(crate) fn post_palette_menu_action(
        &self,
        window: WindowId,
        action: crate::menu::MenuAction,
    ) -> bool {
        let Some(origin) = self.palette_menu_origin(window) else {
            return false;
        };
        self.proxy.as_ref().is_some_and(|proxy| {
            post_menu_action_from(
                proxy,
                Some(&self.tab_spawns.menu),
                action,
                Some(origin),
                GestureMarks::default(),
            )
        })
    }

    pub(crate) fn menu_action_for_origin(
        &mut self,
        action: crate::menu::MenuAction,
        origin: Option<MenuOrigin>,
    ) -> Option<crate::menu::MenuAction> {
        if let Some(origin) = origin
            && (self.frontmost_window != Some(origin.window)
                || self
                    .palette_menu_origin(origin.window)
                    .map(|now| now.content)
                    != Some(origin.content))
        {
            self.pending_action_refusal =
                Some("palette command cancelled because its active content changed".to_string());
            return None;
        }
        Some(action)
    }

    /// Dispatch `action`, claim input held under `mark`, and replay released events.
    pub(crate) fn dispatch_marked_menu_action(
        &mut self,
        el: &ActiveEventLoop,
        action: crate::menu::MenuAction,
        mark: MenuMark,
        origin: Option<MenuOrigin>,
        gesture: GestureMarks,
    ) {
        let booked = self.tab_spawns.in_flight.len();
        self.run_posted_menu_action(action, origin, gesture, |app, action| {
            app.dispatch_menu_action(el, action);
        });
        if self.tab_spawns.menu.settle(mark) {
            let release = self.claim_menu_held_input(mark, booked);
            self.replaying(false, |app| {
                for (winit_id, event, origin) in release {
                    app.replay_window_event(el, winit_id, event, origin);
                }
            });
        }
    }

    /// Run a posted menu `action` with `dispatch` unless its origin moved, under every mark the
    /// post carried (`gesture`) and every mark its origin's chooser had.
    pub(crate) fn run_posted_menu_action(
        &mut self,
        action: crate::menu::MenuAction,
        origin: Option<MenuOrigin>,
        gesture: GestureMarks,
        dispatch: impl FnOnce(&mut Self, crate::menu::MenuAction),
    ) {
        let marks = origin.map_or(gesture, |origin| gesture.union(origin.chooser));
        if let Some(action) = self.menu_action_for_origin(action, origin) {
            self.with_gesture_marks(marks, |app| dispatch(app, action));
        }
    }

    /// Resolve input held under `mark`: rebind to spawns past `booked`, or return for replay.
    pub(crate) fn claim_menu_held_input(&mut self, mark: MenuMark, booked: usize) -> Vec<Released> {
        let spawns = &mut self.tab_spawns;
        if !spawns.held.iter().any(|h| h.menu == mark) {
            return Vec::new();
        }
        let now = Instant::now();
        let fresh: Vec<(Option<WindowId>, WindowId, Instant, u64)> = spawns
            .in_flight
            .get(booked..)
            .unwrap_or_default()
            .iter()
            .map(|t| {
                (
                    t.place.asked_from(t.window),
                    t.window,
                    spawns.hold_end(t, now),
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
                release.push((h.typed_into, h.event, h.origin));
                continue;
            };
            if Some(target) != typed {
                if is_pointer(&h.event) {
                    release.push((h.typed_into, h.event, h.origin));
                    continue;
                }
                if matches!(h.event, WindowEvent::ModifiersChanged(_)) {
                    release.push((h.typed_into, h.event.clone(), h.origin));
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

    /// [`Self::install_spawned`] with an inert [`LandingOs`] for tests.
    #[cfg(test)]
    pub(crate) fn install_spawned_tab(&mut self, done: TabSpawned) -> bool {
        self.install_spawned(done, &test_support::InertOs::ATTACHES)
    }

    /// Install a completed [`TabSpawned`], start its reader, and answer its waiter.
    pub(crate) fn install_spawned(&mut self, done: TabSpawned, os: &dyn LandingOs) -> bool {
        self.tab_spawns.refresh_holds(Instant::now());
        let TabSpawned {
            id,
            window,
            session,
            reader_gate,
        } = done;
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
        let spare_grid = self
            .spares
            .transit
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.grid);
        let adopted_spare = spare_grid.is_some();
        let (
            place,
            parent,
            colors,
            grid,
            then,
            admission,
            theme,
            background,
            parks,
            fleet,
            refuses_worker_split,
            unasked,
            acting,
        ) = match self
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
                    t.parks,
                    t.fleet,
                    t.refuses_worker_split,
                    t.unasked,
                    t.acting,
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
                false,
                false,
                false,
                false,
                None,
            ),
        };
        let new_window = matches!(place, SpawnPlace::Window { .. });
        let admission_plain = matches!(admission, SpawnAdmission::Plain);
        let outcome = match session {
            _ if os.exiting() => Err(QUITTING.to_string()),
            Err(e) => Err(place.failed(self, &e.to_string(), unasked)),
            Ok(_) if !then.may_land(self, window) => {
                Err("the request is no longer ready to open its tab".to_string())
            }
            Ok(_) if !new_window && !self.windows.contains_key(&window) => {
                Err("the window closed before the session started".to_string())
            }
            Ok(mut session) => 'land: {
                let host = (!new_window).then_some(window);
                if let Err(why) = self.validate_spawn_admission(&admission, host) {
                    break 'land Err(why);
                }
                if let SpawnPlace::Split(dir) = place {
                    if refuses_worker_split && self.worker_on_show(window) {
                        break 'land Err(WORKER_SPLIT_REFUSED.to_string());
                    }
                    if let Err(why) = self.split_refusal(window, dir) {
                        break 'land Err(why);
                    }
                }
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
                        let sid = session.ctx.self_id.as_str().to_string();
                        let acted = crate::tab_marks::ActScope::resume(acting);
                        let installed = match place {
                            SpawnPlace::Tab => {
                                let kept = background
                                    .then(|| self.windows.get(&window)?.tab_set.active_index())
                                    .flatten();
                                match kept {
                                    _ if parks => {
                                        self.install_tab_session_parked(
                                            window, session, parent, fleet,
                                        );
                                    }
                                    Some(index) => {
                                        self.install_tab_session_behind(
                                            window, session, parent, index,
                                        );
                                    }
                                    None => self.install_tab_session(window, session, parent),
                                }
                                if grid.is_some_and(|grid| grid != self.spawn_grid(window)) {
                                    self.resize_panes_scoped(window, false);
                                }
                                Ok(())
                            }
                            SpawnPlace::Split(dir) => {
                                self.install_split_session(window, dir, session)
                            }
                            SpawnPlace::Window { .. } => {
                                self.install_window_session(window, session, parent, grid, os)
                            }
                        };
                        drop(acted);
                        if installed.is_ok() && theme.is_some() {
                            self.set_session_theme(id, theme);
                        }
                        installed.map(|()| {
                            if let Some(gate) = reader_gate {
                                gate.release();
                            }
                            if adopted_spare {
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
                    Err(e) => Err(place.failed(self, &e, unasked)),
                }
            }
        };
        let landed = outcome.is_ok();
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

    /// Build window `wid` around `session`, register it under `parent`, and attach its OS window.
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
            self.close_window_logical(wid);
            self.post_message(crate::message_reporters::new_window_failed(
                "the window could not be opened",
            ));
            return Err(NO_NEW_WINDOW.to_string());
        }
        self.sync_active_session();
        Ok(())
    }

    /// Theme inherited by a spawn at `place` in `window` when `asked` is `None`.
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

    /// Theme inherited by a split of `window`'s focused pane.
    pub(crate) fn split_theme(
        &self,
        window: WindowId,
    ) -> Option<crate::session_theme::SessionTheme> {
        let focused = self.focused_session_id(window)?;
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

    fn catch_up_colors(&self, session: &Session, asked: u64) {
        if asked == self.engine_color_fanouts {
            return;
        }
        if let Some(tc) = &self.session_factory.terminal_config {
            crate::session_theme::install_session_config(session, &Arc::new(tc.clone()));
        }
    }

    /// Hold `event` for an in-flight spawn or pending menu mark in `wid`, or return `Some(event)`.
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
            Some((mark, posted)) => (wid, posted + MENU_HOLD, mark, None),
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
            origin: self.input_origin,
        });
        now_too
    }

    /// Settle input in `typed_into` whose [`Held::until`] has expired by `now`.
    pub(crate) fn expire_tab_spawn_input(
        &mut self,
        typed_into: WinitWindowId,
        now: Instant,
    ) -> (Vec<Released>, Option<WindowId>) {
        let spawns = &mut self.tab_spawns;
        spawns.refresh_holds(now);
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
                 (it had not started {} s after the spawner began it)",
                SPAWN_HOLD.as_secs()
            );
            self.post_message(crate::message_reporters::keystrokes_dropped(dropped));
        }
        (replay, repair)
    }

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

    /// Settle all held input for `wid` once no spawn remains in flight to it.
    pub(crate) fn settle_tab_spawn_input(
        &mut self,
        wid: WindowId,
        landed: bool,
    ) -> (Vec<Released>, Option<WindowId>) {
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
#[path = "tab_spawn_test_support_tests.rs"]
pub(crate) mod test_support;

#[cfg(all(test, unix))]
#[path = "tab_spawn_tests.rs"]
pub(crate) mod tests;
