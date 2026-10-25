// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Off-loop PTY spawning for new tabs, splits, and windows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_window::application::ApplicationHandler as _;
use aterm_window::event::WindowEvent;
use aterm_window::event_loop::{ActiveEventLoop, EventLoopProxy};
use aterm_window::keyboard::ModifiersState;
use aterm_window::window::WindowId as WinitWindowId;

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
    Reply {
        reply: Sender<Result<String, String>>,
        raise: bool,
    },
    /// Wire `tab new`: replies `OK <active> <count>` once installed or `ERR <why>`.
    Counts(Sender<Result<(usize, usize), String>>),
    /// Main-thread callback invoked with the registered `sid` or error reason.
    Run(SpawnDone),
}

/// Main-thread callback run once an off-loop spawn settles.
pub(crate) type SpawnDone = Box<dyn FnOnce(&mut App, Result<String, String>)>;

impl TabSpawnThen {
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

/// Admission facts captured when a spawn is booked on the main thread.
#[derive(Default)]
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

/// Parameters for an off-loop spawn request.
#[derive(Default)]
pub(crate) struct SpawnAsk {
    pub(crate) admission: SpawnAdmission,
    /// Explicit cwd (`None` inherits `folder_of` or the focused pane's folder).
    pub(crate) cwd: Option<String>,
    /// Origin pane folder to inherit before the focused pane's.
    pub(crate) folder_of: Option<crate::spawn_folder::PaneFolder>,
    /// Session id for `ATERM_OBSERVE_SESSION_ID`.
    pub(crate) observe: Option<aterm_session::SessionId>,
    /// Worker task file seed (`ATERM_TASK_FILE`), cleaned up on failure.
    pub(crate) task: Option<crate::task_launch::TaskSeed>,
    /// Land a new tab behind the active tab without moving focus.
    pub(crate) background: bool,
    /// Explicit session theme (`None` inherits the divided pane's theme on a split).
    pub(crate) theme: Option<crate::session_theme::SessionTheme>,
}

impl SpawnAsk {
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
    completion: Arc<TabSpawnCompletion>,
    /// The newborn's own theme, as asked ([`SpawnAsk::theme`]).
    theme: Option<crate::session_theme::SessionTheme>,
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
}

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
) -> (Vec<(WinitWindowId, WindowEvent)>, u32) {
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
                t.place.asked_from(t.window) == Some(window)
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

/// The window and focused leaf from which a palette posted a menu command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MenuOrigin {
    window: WindowId,
    content: Option<crate::front_content::FrontContent>,
}

/// Serve `marks` to the native menu and toolbar's posts.
pub(crate) fn serve_native_menu_marks(marks: &Arc<MenuMarks>) {
    let _ = NATIVE_MENU_MARKS.set(Arc::clone(marks));
}

/// Post `action` as a [`Wake::MenuAction`] recorded in `marks`.
pub(crate) fn post_menu_action(
    proxy: &EventLoopProxy<Wake>,
    marks: Option<&MenuMarks>,
    action: crate::menu::MenuAction,
) -> bool {
    post_menu_action_from(proxy, marks, action, None)
}

fn post_menu_action_from(
    proxy: &EventLoopProxy<Wake>,
    marks: Option<&MenuMarks>,
    action: crate::menu::MenuAction,
    origin: Option<MenuOrigin>,
) -> bool {
    let mark = marks.map_or(MenuMark::NONE, |marks| marks.mint(Instant::now()));
    let sent = proxy
        .send_event(Wake::MenuAction {
            action,
            mark,
            origin,
        })
        .is_ok();
    if !sent && let Some(marks) = marks {
        marks.settle(mark);
    }
    sent
}

/// Post `action` from a native menu or toolbar callback using [`NATIVE_MENU_MARKS`].
#[cfg(target_os = "macos")]
pub(crate) fn post_native_menu_action(
    proxy: &EventLoopProxy<Wake>,
    action: crate::menu::MenuAction,
) -> bool {
    post_menu_action(proxy, NATIVE_MENU_MARKS.get().map(Arc::as_ref), action)
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
        let Some(proxy) = self.proxy.clone() else {
            then.answer(self, owner, Err("no event loop to spawn on".to_string()));
            return;
        };
        let grid = self.spawn_grid(owner);
        let parent = self.new_tab_parent(owner);
        self.book_off_loop(owner, place, grid, parent, ask, then, proxy);
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
        let Some(proxy) = self.proxy.clone() else {
            let why = place.failed(self, "no event loop to host the new window");
            then.answer(self, window, Err(why));
            return;
        };
        self.book_off_loop(window, place, grid, lineage_parent, ask, then, proxy);
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
        proxy: EventLoopProxy<Wake>,
    ) {
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
            observe,
            task,
            background,
            theme,
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
        let mut factory = self.session_factory.clone();
        factory.reader_after_install = true;
        #[cfg(windows)]
        {
            factory.shell_appearance =
                self.spawn_shell_appearance(theme.as_ref().map(|theme| theme.selector.as_str()));
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

    /// Replay settled `events` under [`ReplayFloor`] and clear `repair`'s modifier snapshot.
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

    /// Post `action` as a marked [`Wake::MenuAction`] on this App's event loop.
    pub(crate) fn post_menu_action(&self, action: crate::menu::MenuAction) -> bool {
        self.proxy
            .as_ref()
            .is_some_and(|proxy| post_menu_action(proxy, Some(&self.tab_spawns.menu), action))
    }

    pub(crate) fn palette_menu_origin(&self, window: WindowId) -> Option<MenuOrigin> {
        self.windows.get(&window).map(|state| MenuOrigin {
            window,
            content: state.front_content,
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
            post_menu_action_from(proxy, Some(&self.tab_spawns.menu), action, Some(origin))
        })
    }

    pub(crate) fn menu_action_for_origin(
        &mut self,
        action: crate::menu::MenuAction,
        origin: Option<MenuOrigin>,
    ) -> Option<crate::menu::MenuAction> {
        if let Some(origin) = origin
            && (self.frontmost_window != Some(origin.window)
                || self.palette_menu_origin(origin.window) != Some(origin))
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
    ) {
        let booked = self.tab_spawns.in_flight.len();
        if let Some(action) = self.menu_action_for_origin(action, origin) {
            self.dispatch_menu_action(el, action);
        }
        if self.tab_spawns.menu.settle(mark) {
            let release = self.claim_menu_held_input(mark, booked);
            self.replaying(false, |app| {
                for (winit_id, event) in release {
                    app.window_event(el, winit_id, event);
                }
            });
        }
    }

    /// Resolve input held under `mark`: rebind to spawns past `booked`, or return for replay.
    pub(crate) fn claim_menu_held_input(
        &mut self,
        mark: MenuMark,
        booked: usize,
    ) -> Vec<(WinitWindowId, WindowEvent)> {
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
            _ if os.exiting() => Err(QUITTING.to_string()),
            Err(e) => Err(place.failed(self, &e.to_string())),
            Ok(_) if !new_window && !self.windows.contains_key(&window) => {
                Err("the window closed before the session started".to_string())
            }
            Ok(mut session) => 'land: {
                let host = (!new_window).then_some(window);
                if let Err(why) = self.validate_spawn_admission(&admission, host) {
                    break 'land Err(why);
                }
                if let SpawnPlace::Split(dir) = place
                    && let Err(why) = self.split_refusal(window, dir)
                {
                    break 'land Err(why);
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
                        let installed = match place {
                            SpawnPlace::Tab => {
                                let kept = background
                                    .then(|| self.windows.get(&window)?.tab_set.active_index())
                                    .flatten();
                                match kept {
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
                    Err(e) => Err(place.failed(self, &e)),
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
            let mut term = crate::term_lock(&session.term);
            term.apply_config(tc);
            session.ctx.crash_journal.host_settings(&term, Some(tc));
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
        });
        now_too
    }

    /// Settle input in `typed_into` whose [`Held::until`] has expired by `now`.
    pub(crate) fn expire_tab_spawn_input(
        &mut self,
        typed_into: WinitWindowId,
        now: Instant,
    ) -> (Vec<(WinitWindowId, WindowEvent)>, Option<WindowId>) {
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

    /// Book an in-flight tab to `wid` without starting the spawner thread.
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

    /// Book an in-flight window without starting the spawner thread.
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

    /// Test [`LandingOs`] without WindowServer access.
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

    #[test]
    fn a_tab_landing_behind_tells_the_front_nothing_and_the_newborn_it_is_unfocused() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let enable = |app: &mut App, id: u64| {
            crate::term_lock(&app.pool.get(id).expect("landed").term).process(b"\x1b[?1004h");
            app.on_output_wake(id, Instant::now());
        };
        let front = book(&mut app, wid, TabSpawnThen::Nothing);
        let (session, front_pty) = piped_session(front);
        assert!(app.install_spawned_tab(TabSpawned {
            id: front,
            window: wid,
            session: Ok(session),
            reader_gate: None
        }));
        enable(&mut app, front);
        assert!(
            drain(&front_pty).is_empty(),
            "the focused program hears nothing"
        );

        let behind = book(&mut app, wid, TabSpawnThen::Nothing);
        app.tab_spawns
            .in_flight
            .last_mut()
            .expect("booked")
            .background = true;
        let (session, behind_pty) = piped_session(behind);
        assert!(app.install_spawned_tab(TabSpawned {
            id: behind,
            window: wid,
            session: Ok(session),
            reader_gate: None
        }));
        assert_eq!(app.windows[&wid].tab_set.len(), 3, "the tab is in");
        assert_eq!(
            app.focused_session_id(wid),
            Some(front),
            "it landed behind the front"
        );
        assert!(
            drain(&front_pty).is_empty(),
            "the front's focus never moved: no ESC[O, ESC[I pair"
        );

        enable(&mut app, behind);
        assert_eq!(drain(&behind_pty), b"\x1b[O", "the newborn is unfocused");
        assert!(drain(&front_pty).is_empty());
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
                // SAFETY: `self.identity.pid` is our verified child's process group.
                unsafe { libc::killpg(self.identity.pid, libc::SIGKILL) };
            }
            aterm_pty::reap_shell(self.identity);
        }
    }

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

    #[test]
    fn a_spawn_finishing_after_quit_drops_its_owned_child_before_join() {
        let mut app = App::headless_for_test();
        let id = book(&mut app, WindowId(0), TabSpawnThen::Nothing);
        let completion = Arc::clone(&app.tab_spawns.in_flight.last().unwrap().completion);
        let (session, holdout) = hup_ignoring_session(&app, id);
        let (finish, ready) = channel();
        let (posted, wakes) = channel();
        let worker = std::thread::spawn(move || {
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

    fn said_dropped(app: &App, what: &str) -> bool {
        app.messages.live_rows().any(|l| {
            l.msg.title == "Keystrokes dropped" && l.msg.detail.iter().any(|d| d.contains(what))
        })
    }

    fn typed(text: &str) -> WindowEvent {
        WindowEvent::Ime(Ime::Commit(text.to_string()))
    }

    fn replay(app: &mut App, wid: WindowId, events: Vec<(WinitWindowId, WindowEvent)>) {
        for (_, event) in events {
            if let WindowEvent::Ime(Ime::Commit(text)) = event {
                app.on_ime_commit(wid, text);
            }
        }
    }

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
        let stale = Instant::now().checked_sub(SPAWN_HOLD + Duration::from_secs(1));
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

    #[test]
    fn a_tab_lands_with_the_scheme_installed_while_it_was_in_flight() {
        use aterm_types::Appearance;
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
        assert_eq!(land(false), Appearance::Dark);
    }

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

    /// [`piped_session`] with a valid master fd so the session registers in the pool.
    pub(crate) fn registered_piped_session(id: u64) -> (Session, std::fs::File) {
        let (mut session, read) = piped_session(id);
        session.master = session.ctx.sink.master();
        (session, read)
    }

    fn said_no_window(app: &App, what: &str) -> bool {
        app.messages.live_rows().any(|l| {
            l.msg.title == "Couldn't open a new window"
                && l.msg.detail.iter().any(|d| d.contains(what))
        })
    }

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

    #[test]
    fn keys_typed_for_a_tab_the_spawner_started_late_are_held_until_it_lands() {
        let exec_wait = aterm_pty::START_HANG;
        let fresh = || {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            let winit_id = WinitWindowId::from(0u64);
            app.winit_to_window.insert(winit_id, wid);
            let (old, old_pty) = piped_session(app.next_session_id);
            app.push_stub_tab(wid, old);
            let id = book(&mut app, wid, TabSpawnThen::Nothing);
            let picked = app.tab_spawns.in_flight[0].asked + Duration::from_secs(100);
            app.tab_spawns.in_flight[0].completion.note_picked(picked);
            for text in ["l", "s"] {
                assert!(
                    app.defer_tab_spawn_input(wid, winit_id, typed(text))
                        .is_none()
                );
            }
            (app, wid, winit_id, id, picked, old_pty)
        };

        let (mut app, wid, winit_id, id, picked, old_pty) = fresh();
        let late = picked + exec_wait + Duration::from_secs(5);
        let (events, _) = app.expire_tab_spawn_input(winit_id, late);
        assert!(events.is_empty(), "nothing settles: {events:?}");
        assert_eq!(app.tab_spawns.held.len(), 2, "the keys are still held");
        assert_eq!(
            app.tab_spawns.hold_target(wid, late),
            Some((wid, picked + SPAWN_HOLD, id)),
            "new keys are held for it too"
        );
        let (new, new_pty) = piped_session(id);
        assert!(app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(new),
            reader_gate: None,
        }));
        let (events, _) = app.settle_tab_spawn_input(wid, true);
        replay(&mut app, wid, events);
        assert_eq!(drain(&new_pty), b"ls", "in order, to the tab that started");
        assert!(
            drain(&old_pty).is_empty(),
            "never to the tab the person left"
        );

        let (mut app, wid, _, _, picked, _) = fresh();
        let queued = book(&mut app, wid, TabSpawnThen::Nothing);
        assert_eq!(
            app.tab_spawns.hold_target(wid, late),
            Some((wid, picked + SPAWN_HOLD, queued)),
            "held on the clock of the spawn ahead"
        );
        assert_eq!(app.tab_spawns.hold_target(wid, picked + SPAWN_HOLD), None);

        let (mut app, wid, winit_id, id, picked, _) = fresh();
        let (new, new_pty) = piped_session(id);
        let completion = Arc::clone(&app.tab_spawns.in_flight[0].completion);
        assert!(completion.store(TabSpawned {
            id,
            window: wid,
            session: Ok(new),
            reader_gate: None,
        }));
        let landed_late = picked + SPAWN_HOLD + Duration::from_secs(600);
        let (events, _) = app.expire_tab_spawn_input(winit_id, landed_late);
        assert!(events.is_empty(), "nothing settles: {events:?}");
        assert_eq!(app.tab_spawns.held.len(), 2, "held until it lands");
        let done = completion.take().expect("the verdict");
        assert!(app.install_spawned_tab(done));
        let (events, _) = app.settle_tab_spawn_input(wid, true);
        replay(&mut app, wid, events);
        assert_eq!(drain(&new_pty), b"ls");

        let (mut app, wid, winit_id, _, picked, old_pty) = fresh();
        let over = picked + SPAWN_HOLD;
        let (events, _) = app.expire_tab_spawn_input(winit_id, over);
        assert!(events.is_empty(), "keystrokes are dropped: {events:?}");
        assert!(app.tab_spawns.held.is_empty());
        assert!(said_dropped(&app, "retype the last 2 keys"));
        assert_eq!(app.tab_spawns.hold_target(wid, over), None);
        assert!(drain(&old_pty).is_empty());
    }

    #[test]
    fn the_spawners_start_times_the_hold_on_a_tabs_keys() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        let asked = app.tab_spawns.in_flight[0].asked;
        let completion = Arc::clone(&app.tab_spawns.in_flight[0].completion);
        let picked = asked + Duration::from_secs(100);
        let later = asked + SPAWN_HOLD + Duration::from_secs(1);
        assert_eq!(
            app.tab_spawns.hold_target(wid, later),
            None,
            "before the start, the hold from the ask has run out"
        );
        assert!(start_picked(&completion, picked), "a job to run");
        assert_eq!(completion.picked(), Some(picked));
        assert_eq!(
            app.tab_spawns.hold_target(wid, later),
            Some((wid, picked + SPAWN_HOLD, id)),
            "held from the spawner's start"
        );
        let cancelled = TabSpawnCompletion::default();
        cancelled.cancel();
        assert!(!start_picked(&cancelled, picked), "a cancelled job");
        assert_eq!(cancelled.picked(), None);
    }

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

        let past = Instant::now() + SPAWN_HOLD;
        let (events, repair) = app.expire_tab_spawn_input(winit_id, past);
        assert_eq!(named(&events), ["mods", "move 4"]);
        assert_eq!(repair, None);
        assert!(app.tab_spawns.held.is_empty());
        assert!(said_dropped(&app, "retype the last key"));

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

        let origin = app.palette_menu_origin(wid).unwrap();
        let mark = marks.mint(Instant::now());
        for text in ["q", "r"] {
            assert!(
                app.defer_tab_spawn_input(wid, winit_id, typed(text))
                    .is_none()
            );
        }
        let booked = app.tab_spawns.in_flight.len();
        app.frontmost_window = None;
        assert!(
            app.menu_action_for_origin(crate::menu::MenuAction::Copy, Some(origin))
                .is_none()
        );
        assert!(marks.settle(mark));
        let back = app.claim_menu_held_input(mark, booked);
        assert_eq!(named(&back), ["q", "r"]);
        assert!(app.tab_spawns.held.is_empty());
        app.frontmost_window = Some(wid);

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

        let stale = Instant::now()
            .checked_sub(MENU_HOLD + Duration::from_secs(1))
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

    #[test]
    fn replayed_input_goes_to_the_spawn_that_was_newest_when_it_was_typed() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        app.winit_to_window.insert(winit_id, wid);
        let (old, old_pty) = piped_session(app.next_session_id);
        app.push_stub_tab(wid, old);

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

        app.tab_spawns.held.clear();
        app.tab_spawns.in_flight.clear();
        let same_tick = Instant::now();
        let spare = app.next_session_id;
        app.next_session_id += 1;
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
                Some((wid, same_tick + SPAWN_HOLD, fresh)),
                "first booking made by this replay, despite the equal clock"
            );
            let fallback = book(app, wid, TabSpawnThen::Nothing);
            let adopted = app.tab_spawns.in_flight.last_mut().unwrap();
            adopted.asked = same_tick;
            adopted.id = spare;
            adopted.alt_id = Some(fallback);
            assert_eq!(
                app.tab_spawns.hold_target(wid, same_tick),
                Some((wid, same_tick + SPAWN_HOLD, spare)),
                "an adopted spare uses the new reservation for admission"
            );
        });
        app.tab_spawns.in_flight.retain(|t| t.id == before);
        app.replaying(false, |app| {
            assert_eq!(
                app.tab_spawns.hold_target(wid, same_tick),
                Some((wid, same_tick + SPAWN_HOLD, before))
            );
        });
        app.tab_spawns.in_flight.clear();
    }
}
