// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bridge launcher: starts and supervises the process that holds `Scope::Bridge`.

use aterm_types::fabric_grammar::Bye;
use aterm_uds::spawnfd::{ChildStdio, LaneChild, LaneCommand};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const FABRIC_COMMAND_ENV: &str = "ATERM_FABRIC_COMMAND";

/// The back-off's floor, for a bridge that died young or could not start: what keeps one that
/// fails at startup from fork-bombing. A healthy run's bridge has none ([`next_backoff`]).
const RELAUNCH_MIN: Duration = Duration::from_millis(200);
const RELAUNCH_MAX: Duration = Duration::from_secs(30);

/// A run this long was no startup failure: its bridge comes back at once, so a loop of
/// healthy-then-dying runs launches about once per this.
const RELAUNCH_HEALTHY: Duration = Duration::from_secs(5);

/// How long a retired generation's lanes get to end before a warning says they have not
/// ([`Retiring::watch`]). They should: hung up, each has only a verb it is already in to finish,
/// and then the loss to say. No relaunch waits on them.
const LANES_END_WAIT: Duration = Duration::from_secs(60);

/// What a served lane holds until its thread is done, its loss said ([`Retiring::watch`]).
pub(crate) type LaneEnded = std::sync::mpsc::Sender<std::convert::Infallible>;

/// What the supervisor tells the link of a generation one of whose lanes it served, on its own
/// thread: the attach once a lane is served ([`launch_serving`]), the loss when its bridge
/// exits or cannot serve both lanes ([`Launched::retire`]). Told there, its attach, loss and the next
/// one's attach reach the link in that order, however its lanes are scheduled and however long
/// they take to end. Each lane tells the attach too, before it serves a verb, and the loss as it
/// ends; the link takes a generation's attach once.
struct Told {
    attached: fn(crate::fabric::BridgeGeneration),
    lost: Box<dyn FnOnce(crate::fabric::BridgeGeneration)>,
}

/// How long a QUIT waits for its bridge to take `bye quit` ([`bye`]).
pub(crate) const BYE_WAIT: Duration = Duration::from_secs(1);

const QUIT_BYE_SEAM: &str = "ATERM_DEBUG_QUIT_BYE";

/// The configured bridge command, or `None` when the fabric is off.
#[must_use]
pub(crate) fn configured_command(config: &crate::app_config::Config) -> Option<Vec<String>> {
    if !config.swarm_enabled() {
        return None;
    }
    let argv = aterm_types::dev_seam!(FABRIC_COMMAND_ENV)
        .map(|s| aterm_types::fabric_command::split_line(&s.to_string_lossy()))
        .filter(|argv| !argv.is_empty())
        .or_else(|| {
            config
                .fabric
                .as_ref()
                .and_then(|f| f.command.as_ref())
                .map(|c| c.0.clone())
        })?;
    (!argv.is_empty()).then_some(argv)
}

/// `[fabric] hold_on_bridge_lost`.
#[must_use]
pub(crate) fn hold_on_bridge_lost(config: &crate::app_config::Config) -> bool {
    config
        .fabric
        .as_ref()
        .and_then(|f| f.hold_on_bridge_lost)
        .unwrap_or(false)
}

struct Supervisor {
    /// The argv the supervisor thread was armed with — `Some` exactly once per process.
    armed: Option<Vec<String>>,
    /// The command the instance was launched with, if any.
    configured: Option<Vec<String>>,
    /// The argv a seamless update's outgoing process was armed with by `fabric attach`.
    carried: Option<Vec<String>>,
}

static SUPERVISOR: Mutex<Supervisor> = Mutex::new(Supervisor {
    armed: None,
    configured: None,
    carried: None,
});

static RETIRED: AtomicBool = AtomicBool::new(false);

static HANDOFF_BYE_ATTEMPT: AtomicU64 = AtomicU64::new(0);

fn supervisor() -> std::sync::MutexGuard<'static, Supervisor> {
    SUPERVISOR.lock().unwrap_or_else(|p| p.into_inner())
}

/// Why [`arm`] did not start a supervisor.
#[derive(Debug)]
pub(crate) enum ArmError {
    /// The argv was empty. Nothing is armed.
    NoCommand,
    /// `argv[0]` cannot be executed ([`preflight`] gives `reason`).
    NotExecutable {
        program: String,
        reason: &'static str,
    },
    /// One is already running.
    AlreadySupervised(Vec<String>),
    /// The thread could not be spawned. Nothing is armed; a retry is allowed.
    Thread(std::io::Error),
}

/// What `fabric status` reports about the supervisor.
pub(crate) struct SupervisorStatus {
    pub(crate) armed: Option<Vec<String>>,
    pub(crate) configured: Option<Vec<String>>,
}

impl SupervisorStatus {
    /// Whether the supervisor names a command: armed (an `aterm fabric on`'s attach) or
    /// configured (`[fabric] command`), either without the other.
    pub(crate) const fn names_a_command(&self) -> bool {
        self.armed.is_some() || self.configured.is_some()
    }
}

/// A snapshot of the [`Supervisor`] record.
pub(crate) fn status() -> SupervisorStatus {
    let g = supervisor();
    SupervisorStatus {
        armed: g.armed.clone(),
        configured: g.configured.clone(),
    }
}

/// Launch and supervise the bridge.
pub(crate) fn spawn_supervisor(config: &crate::app_config::Config) -> bool {
    crate::fabric::set_hold_on_bridge_lost(hold_on_bridge_lost(config));
    let configured = configured_command(config);
    let carried = {
        let mut g = supervisor();
        g.configured = configured.clone();
        g.carried.take()
    };
    let argv = match (configured, carried) {
        (Some(argv), _) => argv,
        (None, Some(argv)) => {
            aterm_log::info!(
                "fabric bridge supervisor: re-arming the command the outgoing process was \
                 attached to (`fabric attach`) across the update"
            );
            argv
        }
        (None, None) => return false,
    };
    match arm(argv) {
        Ok(()) => true,
        Err(ArmError::AlreadySupervised(_)) => true,
        Err(ArmError::NotExecutable { program, reason }) => {
            aterm_log::warn!(
                "fabric bridge supervisor not started: `{}` is {reason}; fix [fabric] \
                 command in aterm.toml, then `aterm ctl fabric attach` arms it",
                program_word(&program)
            );
            false
        }
        Err(e) => {
            aterm_log::warn!("fabric bridge supervisor could not start: {e:?}");
            false
        }
    }
}

/// The bridge command this instance's configuration names NOW.
#[cfg(not(test))]
pub(crate) fn command_on_disk() -> Option<Vec<String>> {
    let config = crate::app_config::config_path()
        .and_then(|path| {
            aterm_uds::atomic_file::read_regular_text(
                &path,
                aterm_uds::atomic_file::MAX_CONFIG_BYTES,
            )
            .ok()
        })
        .and_then(|text| crate::app_config::Config::parse(&text).ok())
        .unwrap_or_default();
    configured_command(&config)
}

/// [`command_on_disk`] in a test process.
#[cfg(test)]
pub(crate) fn command_on_disk() -> Option<Vec<String>> {
    COMMAND_ON_DISK_FOR_TESTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

#[cfg(test)]
static COMMAND_ON_DISK_FOR_TESTS: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// Say what aterm.toml's `[fabric] command` names for [`command_on_disk`].
#[cfg(test)]
pub(crate) fn set_command_on_disk_for_tests(argv: Option<Vec<String>>) {
    *COMMAND_ON_DISK_FOR_TESTS
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = argv;
}

/// Arm the supervisor with `argv`.
pub(crate) fn arm(argv: Vec<String>) -> Result<(), ArmError> {
    if argv.is_empty() {
        return Err(ArmError::NoCommand);
    }
    let mut guard = supervisor();
    if let Some(running) = &guard.armed {
        return Err(ArmError::AlreadySupervised(running.clone()));
    }
    if let Err(reason) = preflight(&argv[0], std::env::var_os("PATH").as_deref()) {
        return Err(ArmError::NotExecutable {
            program: argv[0].clone(),
            reason,
        });
    }
    let thread_argv = argv.clone();
    std::thread::Builder::new()
        .name("aterm-fabric-launch".to_string())
        .spawn(move || supervise(&thread_argv))
        .map_err(ArmError::Thread)?;
    guard.armed = Some(argv);
    crate::fabric::note_bridge_supervised();
    Ok(())
}

fn preflight(program: &str, path: Option<&std::ffi::OsStr>) -> Result<(), &'static str> {
    if program.contains('/') {
        return preflight_path(std::path::Path::new(program));
    }
    let default = std::ffi::OsStr::new("/usr/bin:/bin");
    let found = std::env::split_paths(path.unwrap_or(default))
        .any(|dir| preflight_path(&dir.join(program)).is_ok());
    if found { Ok(()) } else { Err("not-on-path") }
}

fn preflight_path(path: &std::path::Path) -> Result<(), &'static str> {
    match std::fs::metadata(path) {
        // A sandbox can permit process-exec while denying metadata through a protected
        // directory. Only the actual spawn can decide whether that command may run.
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(()),
        Err(_) => Err("not-found"),
        Ok(md) if !md.is_file() => Err("not-a-file"),
        Ok(md) if !is_executable(&md) => Err("no-exec-bit"),
        Ok(_) => Ok(()),
    }
}

fn is_executable(md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o111 != 0
}

/// TELL EVERY WATCHER HOW THIS PROCESS LEAVES.
pub(crate) fn bye(store: &crate::session_store::Store, word: Bye, wait: Duration) -> bool {
    let bridged = crate::fabric::bridge_attached_now();
    let before = crate::fabric::byes_taken();
    let said = say(store, word, true);
    let taken = bridged && !wait.is_zero() && crate::fabric::await_bye_taken(before, wait);
    if let Some(seq) = said
        && !wait.is_zero()
        && !crate::subscribe::await_roster_written(store, seq, wait)
    {
        aterm_log::info!(
            "fabric: a `sessions` watcher had not been written `bye {}` within {:.1}s; it \
             reads the connection's end instead",
            word.word(),
            wait.as_secs_f64()
        );
    }
    taken
}

fn say(store: &crate::session_store::Store, word: Bye, block: bool) -> Option<u64> {
    let seq = {
        let mut g = if block {
            store.write().unwrap_or_else(|p| p.into_inner())
        } else {
            match store.try_write() {
                Ok(g) => g,
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return None,
            }
        };
        g.say_bye(word);
        g.roster_seq()
    };
    Some(seq)
}

/// THE HANDOFF'S BYE.
pub(crate) fn bye_at_handoff(store: &crate::session_store::Store, attempt: u64) {
    if HANDOFF_BYE_ATTEMPT.load(Ordering::SeqCst) == attempt
        || !crate::fabric::bridge_attached_now()
    {
        return;
    }
    if say(store, Bye::Handoff, false).is_some() {
        HANDOFF_BYE_ATTEMPT.store(attempt, Ordering::SeqCst);
    }
}

/// THE QUIT'S BYE, after its closes (fix unit Q1): every live session's log closes `out=app-quit`
/// ([`crate::session_log::close_logs_at_quit`], the one producer the bus mirror and the journal
/// both carry), the bus is given those closes within the quit's budget, and only then does the
/// bridge hear `bye quit` ([`quit_in_order`]).
pub(crate) fn bye_at_quit(store: &crate::session_store::Store) {
    RETIRED.store(true, Ordering::SeqCst);
    let word = quit_bye(aterm_types::dev_seam!(QUIT_BYE_SEAM).as_deref());
    let bridged = crate::fabric::bridge_attached_now();
    let budget = crate::session_log_journal::quit_budget();
    let order = quit_in_order(store, word, budget, &LiveBus::of(store));
    aterm_log::info!(
        "fabric: the quit closed {} session log(s): {} before its `bye {}` (taken: {})",
        order.closed,
        order.landing.word(),
        word.word(),
        order.taken
    );
    if order.closed > 0 && order.landing == Landing::Missed {
        aterm_log::warn!(
            "fabric: {} session close(s) of the quit had not landed on the bus within {:.1}s; \
             the journal holds them, and the bus reads the bridge's withdrawal instead",
            order.closed,
            budget.as_secs_f64()
        );
    }
    if !order.taken && bridged {
        aterm_log::warn!(
            "fabric: the bridge did not take `bye {}` within {:.1}s; this instance's \
             sessions are withdrawn only if it reads the word before the lanes close, \
             and read `away` with its node otherwise",
            word.word(),
            BYE_WAIT.as_secs_f64()
        );
    }
}

/// How a quit's closes met the bus before its bye ([`quit_in_order`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Landing {
    /// The bus holds every live log through its close, or the quit closed none.
    Landed,
    /// No bridge could land them now (none mirrors, its link is futile, its mirror is stopped).
    Unserved,
    /// The budget ran out first, or the link went futile while it waited: the quit went on.
    Missed,
    /// The quit's word was `bye handoff` (the QA seam): the closes followed the word.
    After,
}

impl Landing {
    /// The words aterm.log says it in ([`bye_at_quit`]).
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Landed => "landed on the bus",
            Self::Unserved => "not waited for (no bridge could land them)",
            Self::Missed => "missed the budget",
            Self::After => "closed after the word",
        }
    }
}

/// What a quit did before and at its bye.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QuitOrder {
    /// How many live session logs the quit closed.
    pub(crate) closed: usize,
    /// How those closes met the bus.
    pub(crate) landing: Landing,
    /// Whether the bridge took the bye ([`bye`]).
    pub(crate) taken: bool,
}

/// What a quit's order waits on, injected so a test drives its clock and plays its bridge.
pub(crate) trait QuitBus {
    /// The monotonic now.
    fn now(&self) -> Instant;
    /// Whether the quit's closes can land now ([`crate::fabric::mirror_lands_now`]).
    fn lands(&self) -> bool;
    /// How many landings this store answered so far ([`crate::session_log_mirror::Shared`]).
    fn landings(&self) -> u64;
    /// Park until a landing past `seen`, or `until`, whichever is first.
    fn park(&self, seen: u64, until: Instant);
    /// Say the bye ([`bye`]): whether the bridge took it.
    fn bye(&self, store: &crate::session_store::Store, word: Bye) -> bool;
}

/// The longest one park of a live quit lasts, so the link is read again ([`QuitBus::lands`]):
/// a bridge that went away, or a broker it reported unreachable, ends the wait at once.
pub(crate) const QUIT_PARK_SLICE: Duration = Duration::from_millis(20);

/// The live bus: the store's own landings and the link's state.
struct LiveBus {
    shared: std::sync::Arc<crate::session_log_mirror::Shared>,
}

impl LiveBus {
    fn of(store: &crate::session_store::Store) -> Self {
        Self {
            shared: store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .mirror()
                .shared(),
        }
    }
}

impl QuitBus for LiveBus {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn lands(&self) -> bool {
        crate::fabric::mirror_lands_now()
    }

    fn landings(&self) -> u64 {
        self.shared.landings()
    }

    fn park(&self, seen: u64, until: Instant) {
        let wait = until.saturating_duration_since(Instant::now());
        self.shared.await_landing(seen, wait.min(QUIT_PARK_SLICE));
    }

    fn bye(&self, store: &crate::session_store::Store, word: Bye) -> bool {
        bye(store, word, BYE_WAIT)
    }
}

/// THE QUIT'S ORDER (fix unit Q1): close, land, then bye. It lives here, on the instance's side
/// of the link, because the instance owns the quit, its budget and the live logs the landings
/// move; the bridge's mirror only has to take what it is woken for (`EVENT * quit-closes`), as
/// it takes any other record, and the bridge's own `bye` handling stays as it was. Every live
/// session's log closes `out=app-quit` first ([`crate::session_log::close_logs_at_quit`]): the
/// one producer, whose records the journal's last passes write as any other. Then, while a
/// bridge can land them ([`QuitBus::lands`]), the quit waits at most `budget` for the bus to hold
/// every live log through its close ([`crate::session_log_mirror::quit_closes_landed`]), and
/// only then says `bye quit`: a bus reader reads the close before the bridge withdraws the
/// session. A bus that cannot be reached never blocks the quit: an unserved or futile link is
/// not waited for, and a budget that runs out moves on, the journal holding the closes either
/// way. The QA seam's `bye handoff` keeps the word first, as a handoff (whose sessions go on)
/// says it, and closes the logs after it for the journal.
pub(crate) fn quit_in_order(
    store: &crate::session_store::Store,
    word: Bye,
    budget: Duration,
    bus: &impl QuitBus,
) -> QuitOrder {
    if word == Bye::Handoff {
        let taken = bus.bye(store, word);
        let closed = crate::session_log::close_logs_at_quit(store);
        return QuitOrder {
            closed,
            landing: Landing::After,
            taken,
        };
    }
    let closed = crate::session_log::close_logs_at_quit(store);
    let landing = if closed == 0 {
        Landing::Landed
    } else {
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .say_quit_closes();
        land_quit_closes(store, budget, bus)
    };
    let taken = bus.bye(store, word);
    QuitOrder {
        closed,
        landing,
        taken,
    }
}

/// The wait of [`quit_in_order`]: until the bus holds every live log, the link cannot land them,
/// or `budget` passed.
fn land_quit_closes(
    store: &crate::session_store::Store,
    budget: Duration,
    bus: &impl QuitBus,
) -> Landing {
    if !bus.lands() {
        return Landing::Unserved;
    }
    let deadline = bus.now() + budget;
    loop {
        let seen = bus.landings();
        if crate::session_log_mirror::quit_closes_landed(store) {
            return Landing::Landed;
        }
        if !bus.lands() || bus.now() >= deadline {
            return Landing::Missed;
        }
        bus.park(seen, deadline);
    }
}

fn quit_bye(seam: Option<&std::ffi::OsStr>) -> Bye {
    match seam.and_then(std::ffi::OsStr::to_str) {
        Some("handoff") => Bye::Handoff,
        _ => Bye::Quit,
    }
}

/// Clear the record for a test section.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    let mut g = supervisor();
    g.armed = None;
    g.configured = None;
    g.carried = None;
    RETIRED.store(false, Ordering::SeqCst);
    drop(g);
    set_command_on_disk_for_tests(None);
}

/// What a seamless update's manifest carries of the supervisor.
pub(crate) fn handoff_attached() -> Vec<String> {
    let g = supervisor();
    match &g.armed {
        Some(argv) if g.configured.as_ref() != Some(argv) => argv.clone(),
        _ => Vec::new(),
    }
}

/// The successor's intake of [`handoff_attached`].
pub(crate) fn carry_attached(argv: Vec<String>) {
    if !argv.is_empty() {
        supervisor().carried = Some(argv);
    }
}

fn supervise(argv: &[String]) {
    supervise_with(
        argv,
        launch_once,
        || RETIRED.load(Ordering::SeqCst),
        std::thread::sleep,
    );
}

/// The word aterm.log keeps of the bridge's program: its file name where that is a word
/// ([`aterm_log::words::word`]), never the path or the arguments, which are aterm.toml's
/// `[fabric] command` (cap paths, broker addresses: a config value, DESIGN §3.6).
fn program_word(program: &str) -> &str {
    std::path::Path::new(program)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .map_or("?", aterm_log::words::word)
}

/// [`supervise`] over its `launch`, its `retired` word and its `pause` between two launches.
fn supervise_with(
    argv: &[String],
    launch: impl Fn(&[String]) -> std::io::Result<Launched>,
    retired: impl Fn() -> bool,
    pause: impl Fn(Duration),
) {
    let mut backoff = RELAUNCH_MIN;
    loop {
        if retired() {
            aterm_log::info!("fabric bridge supervisor retired: aterm is quitting");
            return;
        }
        match launch(argv) {
            Ok(mut launched) => {
                aterm_log::info!(
                    "fabric bridge started: {} ({} args)",
                    argv.first().map_or("?", |program| program_word(program)),
                    argv.len().saturating_sub(1)
                );
                let started = Instant::now();
                let status = launched.wait();
                let ran = started.elapsed();
                launched.retire().watch_behind(LANES_END_WAIT);
                backoff = next_backoff(backoff, Some(ran));
                let how = match &status {
                    Ok(s) => s.to_string(),
                    Err(e) => e.to_string(),
                };
                let loss = if crate::fabric::holds_on_bridge_lost() {
                    "every session it governed is held reason=fabric-lost"
                } else {
                    "the loss holds no session it governed (hold_advisory=fabric-lost)"
                };
                aterm_log::warn!(
                    "fabric bridge stopped ({how}) after {:.1}s; {loss} — restarting in {:.1}s",
                    ran.as_secs_f64(),
                    backoff.as_secs_f64()
                );
            }
            Err(e) => {
                backoff = next_backoff(backoff, None);
                aterm_log::warn!(
                    "fabric bridge could not start ({e}); retrying in {:.1}s",
                    backoff.as_secs_f64()
                );
            }
        }
        pause(backoff);
    }
}

/// The wait before the next launch: none after a healthy run, doubling from [`RELAUNCH_MIN`] up
/// to [`RELAUNCH_MAX`] after a run that was not (`ran`) or a launch that failed (`None`).
fn next_backoff(current: Duration, ran: Option<Duration>) -> Duration {
    match ran {
        Some(ran) if ran >= RELAUNCH_HEALTHY => Duration::ZERO,
        _ => (current * 2).clamp(RELAUNCH_MIN, RELAUNCH_MAX),
    }
}

struct Launched {
    child: LaneChild,
    near: [std::os::unix::net::UnixStream; 2],
    generation: crate::fabric::BridgeGeneration,
    /// [`Told::lost`], when one of its lanes was served.
    lost: Option<Box<dyn FnOnce(crate::fabric::BridgeGeneration)>>,
    /// Disconnected once no lane holds a [`LaneEnded`] any more.
    lanes_ended: std::sync::mpsc::Receiver<std::convert::Infallible>,
}

impl Launched {
    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait()
    }

    /// Revoke a bridge's lanes and tell its generation's loss after exit or failed startup.
    /// Neither lane work nor an unconfirmed child exit delays the next generation: pending
    /// child custody stays with the reaper, and the lanes finish through [`Retiring`].
    ///
    /// THE ORDER A RELAUNCH AT ONCE RESTS ON: the loss is told here, on the supervisor's thread,
    /// before it launches the next generation ([`Told`]), so the link's hearers read this
    /// generation `disconnected` ahead of the next one's attach and, under `[fabric]
    /// hold_on_bridge_lost`, every session it governed is held before a new bridge can free it.
    /// A `hold` its lanes had already read lands before that sweep takes its session, or moves
    /// nothing (`fabric::HOLD_LANE_LOST`): no verb of a lost generation lifts the sweep's hold
    /// or halts a session the next bridge serves. Each lane says the loss again as it ends
    /// (`BridgeLostGuard`). Ahead of the next attach that sweeps again (a session a verb still in
    /// flight touched is held too). Past it the generation is a dead one, whose loss neither
    /// moves the link nor holds or advises a session the new bridge serves
    /// (`fabric::bridge_lost_waking`).
    fn retire(self) -> Retiring {
        let Self {
            mut child,
            near,
            generation,
            lost,
            lanes_ended,
        } = self;
        for lane in &near {
            aterm_uds::hangup::hang_up(lane);
        }
        if let Some(lost) = lost {
            lost(generation);
        }
        if !matches!(child.try_wait(), Ok(Some(_))) {
            let killed = child.kill();
            let waited = child.try_wait();
            if !matches!(&waited, Ok(Some(_))) {
                if killed.is_err() || waited.is_err() {
                    aterm_log::warn!(
                        "fabric bridge retirement retains its child after cleanup errors: \
                         kill={:?}, wait={:?}",
                        killed.err(),
                        waited.err()
                    );
                }
                aterm_uds::reaper::hand_off(move || matches!(child.try_wait(), Ok(Some(_))));
            }
        }
        Retiring { lanes_ended }
    }
}

/// A retired generation's lanes, ending behind the next launch ([`Launched::retire`]).
struct Retiring {
    /// Disconnected once no lane holds a [`LaneEnded`] any more.
    lanes_ended: std::sync::mpsc::Receiver<std::convert::Infallible>,
}

impl Retiring {
    /// Wait for every lane to let go of its [`LaneEnded`], warning if one has not `within` the
    /// bridge's retirement: whether all had.
    fn watch(self, within: Duration) -> bool {
        let ended = !matches!(
            self.lanes_ended.recv_timeout(within),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        if !ended {
            aterm_log::warn!(
                "fabric bridge lanes had not ended {:.1}s after retirement; the next bridge runs \
                 beside them, and their late loss neither moves the link nor holds a session",
                within.as_secs_f64()
            );
        }
        ended
    }

    /// [`Self::watch`] on a thread of its own, so the next launch waits on none of the lanes.
    fn watch_behind(self, within: Duration) {
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-fabric-retire".to_string(),
            crate::qos::Role::Background,
            move || {
                self.watch(within);
            },
        );
        if let Err(e) = spawned {
            aterm_log::warn!(
                "fabric bridge lanes cannot be watched to their end ({e}); one that does not end \
                 goes unwarned of"
            );
        }
    }
}

fn launch_once(argv: &[String]) -> std::io::Result<Launched> {
    launch_serving(
        argv,
        crate::control::attach_fabric_bridge,
        Told {
            attached: crate::fabric::bridge_attached,
            lost: Box::new(crate::control::fabric_bridge_retired),
        },
    )
}

fn launch_serving(
    argv: &[String],
    attach: impl Fn(std::os::unix::net::UnixStream, crate::fabric::BridgeGeneration, LaneEnded) -> bool,
    told: Told,
) -> std::io::Result<Launched> {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    // Every end is kept out of children from its creation (`aterm_uds::kept_out`): the near ends
    // live as long as the bridge, and the far ones reach the bridge's child alone (the spawn
    // hands a kept-out lane to its own child, `aterm_uds::spawnfd::spawn_with_two_fds`).
    let (near_verb, far_verb) = aterm_uds::kept_out(UnixStream::pair)?;
    let (near_push, far_push) = aterm_uds::kept_out(UnixStream::pair)?;
    let near = [
        aterm_uds::kept_out(|| near_verb.try_clone())?,
        aterm_uds::kept_out(|| near_push.try_clone())?,
    ];
    let mut cmd = LaneCommand::new(&argv[0]);
    cmd.args(&argv[1..])
        .env_exactly(filter_child_env(std::env::vars_os()))
        .stdin(ChildStdio::Null)
        .stdout(ChildStdio::Inherit)
        .stderr(ChildStdio::Inherit);
    let child = aterm_uds::spawnfd::spawn_with_two_fds(
        cmd,
        OwnedFd::from(far_verb),
        OwnedFd::from(far_push),
    )?;

    let generation = crate::fabric::next_bridge_generation();
    let (ended, lanes_ended) = std::sync::mpsc::channel();
    let verb = attach(near_verb, generation, ended.clone());
    if !verb {
        aterm_log::warn!("fabric bridge verb lane could not be served");
    }
    let push = attach(near_push, generation, ended);
    if !push {
        aterm_log::warn!("fabric bridge push lane could not be served");
    }
    let served = verb || push;
    if served {
        (told.attached)(generation);
    }
    let launched = Launched {
        child,
        near,
        generation,
        lost: served.then_some(told.lost),
        lanes_ended,
    };
    if !verb || !push {
        launched.retire().watch_behind(LANES_END_WAIT);
        return Err(std::io::Error::other(
            "fabric bridge needs both lanes served",
        ));
    }
    Ok(launched)
}

fn filter_child_env(
    inherited: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let seams = cfg!(any(debug_assertions, feature = "dev-seams"));
    inherited
        .filter(|(k, _)| {
            !aterm_types::domain::is_ai_env_key(k)
                || (seams && k == aterm_types::domain::ENV_DEV_FAST_TIMERS)
        })
        .collect()
}

#[cfg(test)]
#[path = "fabric_launch_tests.rs"]
mod tests;
