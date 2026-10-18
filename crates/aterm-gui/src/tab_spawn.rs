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
//! is registered, `tab new` counts the tab once it is in the window, the
//! operator row stamps the newborn it started, and a connected preset's tab is
//! connected to its origin. A spawn that fails says so
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

use winit::application::ApplicationHandler as _;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::WindowId as WinitWindowId;

use crate::platform::AppRt as _;
use crate::{App, Session, Wake, WindowId};

/// How long a window's input waits for a tab in flight to it before it goes back
/// to what the window shows. Past the exec verdict's 10 s budget, its 2 s reap
/// and the folder checks, a spawn still out is wedged, and a window whose keys
/// went nowhere for longer would read as frozen.
const INPUT_HOLD: Duration = Duration::from_secs(15);

/// How many input events a window holds for a tab in flight; past it the newest
/// is dropped and counted, as the pre-Commit queue does
/// ([`crate::MAX_DEFERRED_HANDOFF_INPUT`]).
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
    /// A main-thread row that acts on the newborn at once (the operator's
    /// Start, a connected preset): run with its sid once it is registered, or
    /// with why not.
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
    #[cfg(target_os = "macos")]
    Operator,
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
    /// The newborn's own theme (`spawn theme=`), validated when asked; a
    /// split with none inherits the divided pane's ([`App::install_spawned`]).
    pub(crate) theme: Option<crate::session_theme::SessionTheme>,
}

/// What the spawner does with a spawn's agent identity before it forks, where
/// the identities folder's metadata (it can sit on a slow volume) and a
/// primer's synced writes hold no window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum IdentityPrep {
    /// Only the spawn's own check that it exists (a picker row naming one from
    /// the roster).
    #[default]
    Exists,
    /// Admit it as a request's spawn does
    /// ([`crate::agent_identity::check_for_spawn`]): it exists, its folders are
    /// private, and an `inherited` one is re-primed off this thread. Refused,
    /// the waiter hears why in the words the request's turn refused it with,
    /// and no failure row is posted ([`Refusal`]).
    Admit { inherited: bool },
    /// Make it first, if it does not exist (the picker's new-identity row): its
    /// primer syncs every file it writes.
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
    completion: Arc<TabSpawnCompletion>,
    /// The newborn's own theme, as asked ([`SpawnAsk::theme`]).
    theme: Option<crate::session_theme::SessionTheme>,
}

/// A window's grid (rows, cols) and cell box in device pixels, as a spawn takes
/// them ([`App::spawn_cell_px`]).
type SpawnGrid = (u16, u16, Option<(u16, u16)>);

/// The App's side of the tabs in flight, and what was typed while they were.
#[derive(Default)]
pub(crate) struct TabSpawns {
    in_flight: Vec<InFlight>,
    /// Input typed into a window with a spawn in flight that takes its focus,
    /// in arrival order, under the window it is for: its own, or the new
    /// window asked from it.
    held: Vec<(WinitWindowId, WindowId, WindowEvent)>,
    /// Events refused past [`MAX_HELD_INPUT`], per window.
    dropped: Vec<(WindowId, u32)>,
    /// Windows whose held input is no longer a coherent key stream: a drop broke
    /// a press/release pair, or a focus loss cleared the modifiers it was typed
    /// under. Their modifier snapshot is cleared after the replay.
    incoherent: Vec<WindowId>,
}

impl TabSpawns {
    /// No queued or running spawn still depends on this process. Unlike the
    /// input hold, replacement cannot time out a job that has not settled.
    pub(crate) fn is_empty(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// The in-flight entry is the Start reservation: every error, cancellation
    /// and successful landing removes it before delivering the result.
    #[cfg(target_os = "macos")]
    pub(crate) fn operator_pending(&self) -> bool {
        self.in_flight
            .iter()
            .any(|t| matches!(t.admission, SpawnAdmission::Operator))
    }

    /// The window input typed into `window` at `now` is for, if it waits: the
    /// one the newest spawn asked for within [`INPUT_HOLD`] that takes
    /// `window`'s focus lands in. A tab or split lands in `window` itself; a
    /// new window asked from `window` became the one it typed into.
    fn hold_target(&self, window: WindowId, now: Instant) -> Option<WindowId> {
        self.in_flight
            .iter()
            .rev()
            .find(|t| {
                t.place.asked_from(t.window) == Some(window)
                    && now.saturating_duration_since(t.asked) < INPUT_HOLD
            })
            .map(|t| t.window)
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
            if self.held.iter().any(|(_, w, _)| *w == target) && !self.incoherent.contains(&target)
            {
                self.incoherent.push(target);
            }
        }
    }
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
                    run(job);
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
fn run(job: TabSpawnJob) {
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
        completion,
        adopt,
    } = job;
    if completion.cancelled() {
        return;
    }
    // An adopted spare (design §8 step 2): its rc generation and folder are
    // checked here, off the main thread; valid, it is handed over at once, its
    // reader already running. Otherwise it goes (a warm one with its whole
    // session) and this spawn runs cold under the fallback id.
    if let Some(adopt) = adopt {
        let AdoptSpare {
            id: spare_id,
            session,
            check,
            miss,
        } = *adopt;
        match check.miss(&session) {
            None => {
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
            Some(reason) => {
                let _ = miss.set(reason);
                if check.kill {
                    crate::spare::kill_session(session.pid);
                }
                drop(session);
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
        // this thread, for at most the check's budget per pane.
        let cwd = cwd.or_else(|| crate::spawn_folder::PaneFolder::first_taken(pane_folders));
        let session = provision(identity.as_deref(), identity_prep).and_then(|()| {
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
                None,
            )
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

/// Ready agent identity `identity` for the spawn as `prep` says: admit it, or
/// make it first. With [`IdentityPrep::Exists`] the spawn's own check that it
/// exists is all.
fn provision(identity: Option<&str>, prep: IdentityPrep) -> std::io::Result<()> {
    let Some(name) = identity else {
        return Ok(());
    };
    match prep {
        IdentityPrep::Exists => Ok(()),
        IdentityPrep::Admit { inherited } => {
            crate::agent_identity::check_for_spawn(name, inherited)
                .map(drop)
                .map_err(|e| {
                    std::io::Error::new(e.kind(), Refusal(format!("identity {name}: {e}")))
                })
        }
        IdentityPrep::Create => crate::agent_identity::ensure(name, true)
            .map(drop)
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
        // was decided now, whatever the focus is by then.
        let inherited = self.inherited_theme(window, place, ask.theme.as_ref());
        let tried = if inherited.is_some() {
            Err("theme")
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
                });
                let adopt = AdoptSpare {
                    id: slot.id,
                    session,
                    check: crate::spare::AdoptCheck {
                        cwd: slot.cwd.clone(),
                        generation: warm.then(|| (slot.inputs.clone(), slot.generation)),
                        link: Arc::clone(&slot.link),
                        kill: warm && slot.state != crate::spare::SlotState::Ready,
                    },
                    miss,
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
            completion,
            adopt,
        };
        if let Some(job) = send_to_spawner(job) {
            // No spawner thread: spawn in this turn, as before, and land it as usual.
            run(job);
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
        let landed = self.install_spawned(done, el);
        let (replay, repair) = self.settle_tab_spawn_input(window, landed);
        for (winit_id, event) in replay {
            self.window_event(el, winit_id, event);
        }
        if let Some(ws) = repair.and_then(|at| self.windows.get_mut(&at)) {
            ws.mods = ModifiersState::empty();
        }
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
        let (place, parent, colors, grid, then, admission, theme) = match self
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
                                self.install_tab_session(window, session, parent);
                                // The window re-gridded (a resize, a font zoom)
                                // while the tab was in flight: those passes sized
                                // only the panes it had, so size the newborn's
                                // now, before its reader reads.
                                if grid.is_some_and(|grid| grid != self.spawn_grid(window)) {
                                    self.resize_panes(window);
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
    /// `wid`, or a new window asked from it. Keys, modifiers and IME only, the
    /// pre-Commit queue's set. `Some(event)` hands it back to be handled now.
    pub(crate) fn defer_tab_spawn_input(
        &mut self,
        wid: WindowId,
        id: WinitWindowId,
        event: WindowEvent,
    ) -> Option<WindowEvent> {
        if !crate::handoff_deferrable_input(&event) {
            return Some(event);
        }
        let Some(target) = self.tab_spawns.hold_target(wid, Instant::now()) else {
            return Some(event);
        };
        let spawns = &mut self.tab_spawns;
        if spawns.held.len() < MAX_HELD_INPUT {
            spawns.held.push((id, target, event));
        } else {
            match spawns.dropped.iter_mut().find(|(w, _)| *w == target) {
                Some((_, n)) => *n = n.saturating_add(1),
                None => spawns.dropped.push((target, 1)),
            }
            if !spawns.incoherent.contains(&target) {
                spawns.incoherent.push(target);
            }
        }
        None
    }

    /// What the input held for `wid` becomes once nothing is in flight to it:
    /// every event, in order, when the last spawn `landed`, through `wid`'s own
    /// OS window (a new window's input was typed into the one it was asked
    /// from); only the modifier changes, where they were typed, when it did not.
    /// Keys dropped either way are said in one row. Returns the events to replay
    /// and the window whose modifiers need a repair after them, if any: the one
    /// the replay goes through, so a new window that never opened repairs the
    /// window its modifier changes replay back into. Nothing while another
    /// spawn is still in flight to `wid`.
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
            .partition(|(_, w, _)| *w == wid);
        spawns.held = rest;
        let mut dropped = spawns
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
                    Some((typed_into, _, _)) if !landed => {
                        Some(self.winit_to_window.get(typed_into).copied().unwrap_or(wid))
                    }
                    _ => Some(wid),
                }
            }
            None => None,
        };
        let mut replay = Vec::with_capacity(mine.len());
        for (winit_id, _, event) in mine {
            if landed || matches!(event, WindowEvent::ModifiersChanged(_)) {
                replay.push((through.unwrap_or(winit_id), event));
            } else if matches!(&event, WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed())
                || matches!(event, WindowEvent::Ime(winit::event::Ime::Commit(_)))
            {
                dropped = dropped.saturating_add(1);
            }
        }
        if dropped > 0 {
            aterm_log::warn!(
                "{dropped} key(s) typed while a new tab or window was starting were dropped ({})",
                if landed {
                    "past what the window holds"
                } else {
                    "it never started"
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
    use std::sync::Arc;
    use winit::event::Ime;

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

    /// Start is reserved for the whole lifetime of a pending operator spawn,
    /// including a late settlement; failed/closed jobs release that reservation.
    #[cfg(target_os = "macos")]
    #[test]
    fn operator_start_waits_for_pending_spawn_settlement() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.operator_start_available());
        let (tx, rx) = channel();
        let id = book_admitted(
            &mut app,
            wid,
            SpawnAdmission::Operator,
            TabSpawnThen::Reply {
                reply: tx,
                raise: false,
            },
        );
        assert!(
            !app.operator_start_available(),
            "a second Start must not book another operator"
        );
        assert!(!app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Err(std::io::Error::other("exec refused")),
            reader_gate: None
        }));
        assert_eq!(rx.try_recv(), Ok(Err("exec refused".to_string())));
        assert!(
            app.operator_start_available(),
            "failure releases the reservation"
        );
        let gone = WindowId(77);
        let id = book_admitted(
            &mut app,
            gone,
            SpawnAdmission::Operator,
            TabSpawnThen::Nothing,
        );
        assert!(!app.operator_start_available());
        assert!(!app.install_spawned_tab(TabSpawned {
            id,
            window: gone,
            session: Ok(crate::stub_session(id)),
            reader_gate: None
        }));
        assert!(
            app.operator_start_available(),
            "closed host cancels the reservation"
        );
    }

    /// A different operator can become live while the worker is spawning.
    /// Refuse before registering or invoking a successful launch callback.
    #[cfg(target_os = "macos")]
    #[test]
    fn operator_landing_refuses_a_newly_live_operator() {
        for occupied in [true, false] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            let (tx, rx) = channel();
            let id = book_admitted(
                &mut app,
                wid,
                SpawnAdmission::Operator,
                TabSpawnThen::Run(Box::new(move |_, result| {
                    tx.send(result).unwrap();
                })),
            );
            if occupied {
                app.operator_local_id = Some(0);
            }
            let session = crate::stub_session(id);
            let sid = session.ctx.self_id.clone();
            assert_eq!(
                app.install_spawned_tab(TabSpawned {
                    id,
                    window: wid,
                    session: Ok(session),
                    reader_gate: None
                }),
                !occupied
            );
            let answer = rx.try_recv().unwrap();
            if occupied {
                assert!(answer.is_err());
                assert!(app.pool.get(id).is_none());
                assert!(app.store.read().unwrap().by_sid(&sid).is_none());
            } else {
                assert_eq!(answer, Ok(sid.as_str().to_string()));
                assert!(app.pool.get(id).is_some());
                assert!(
                    app.operator_start_available(),
                    "settlement released the pending marker"
                );
            }
        }
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
        let mods = WindowEvent::ModifiersChanged(winit::event::Modifiers::default());
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

    /// A row that acts on the newborn at once (the operator's Start) runs as the
    /// tab lands, with a sid the registry already resolves; a spawn that fails
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

    /// The spawner makes the identity a new-identity row names before the shell
    /// that wears it, so the picker's create costs the window nothing; a row
    /// for an existing identity makes nothing, and the spawn checks it. A name
    /// the grammar refuses fails the spawn, saying which identity. Under a
    /// scratch state root (`ATERM_STATE_HOME`, under the env module's lock).
    #[test]
    fn the_spawner_makes_a_new_identity_before_its_shell() {
        let state = aterm_tempfile::tempdir().expect("scratch state root");
        crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
            let root = state.path().join("identities");
            assert!(provision(Some("gamma"), IdentityPrep::Create).is_ok());
            assert!(
                root.join("gamma").is_dir(),
                "made under the identities root"
            );
            assert_eq!(crate::agent_identity::roster(), ["gamma"]);
            assert!(provision(Some("delta"), IdentityPrep::Exists).is_ok());
            assert!(!root.join("delta").exists(), "only a create row makes one");
            assert!(provision(None, IdentityPrep::Create).is_ok());
            let refused = provision(Some("Bad Name"), IdentityPrep::Create)
                .expect_err("the grammar refuses it");
            assert!(
                refused.to_string().starts_with("identity Bad Name: "),
                "{refused}"
            );
        });
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
            .map(|(_, w, e)| match e {
                WindowEvent::Ime(Ime::Commit(t)) => (*w, t.clone()),
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
    /// replay where they were typed, so no chord strands, and when that window
    /// lost focus meanwhile its modifiers are repaired after them, not the
    /// window that never opened.
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
        let mods = WindowEvent::ModifiersChanged(winit::event::Modifiers::default());
        assert!(
            app.defer_tab_spawn_input(from, from_winit, typed("rm"))
                .is_none()
        );
        assert!(app.defer_tab_spawn_input(from, from_winit, mods).is_none());
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
        assert_eq!(events.len(), 1);
        assert!(
            matches!(events[0], (through, WindowEvent::ModifiersChanged(_)) if through == from_winit),
            "only the modifier change replays, where it was typed: {events:?}"
        );
        assert_eq!(
            repair,
            Some(from),
            "the repair follows the modifier changes back to where they were typed"
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
                winit::event::KeyEvent::synthetic_for_test(
                    winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyN),
                    winit::keyboard::Key::Character("n".into()),
                    Some("n".into()),
                    winit::keyboard::KeyLocation::Standard,
                    winit::event::ElementState::Pressed,
                    false,
                ),
            );
            assert_eq!(app.next_window_id, next + 2, "Cmd-N minted its window");
        }
    }
}
