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
/// thread: the attach once the lanes are served ([`launch_serving`]), the loss once its bridge
/// has exited ([`Launched::retire`]). Told there, one generation's attach, its loss and the next
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
        return match std::fs::metadata(program) {
            Err(_) => Err("not-found"),
            Ok(md) if !md.is_file() => Err("not-a-file"),
            Ok(md) if !is_executable(&md) => Err("no-exec-bit"),
            Ok(_) => Ok(()),
        };
    }
    let default = std::ffi::OsStr::new("/usr/bin:/bin");
    let found = std::env::split_paths(path.unwrap_or(default)).any(|dir| {
        std::fs::metadata(dir.join(program)).is_ok_and(|md| md.is_file() && is_executable(&md))
    });
    if found { Ok(()) } else { Err("not-on-path") }
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

/// THE QUIT'S BYE.
pub(crate) fn bye_at_quit(store: &crate::session_store::Store) {
    RETIRED.store(true, Ordering::SeqCst);
    let word = quit_bye(aterm_types::dev_seam!(QUIT_BYE_SEAM).as_deref());
    let bridged = crate::fabric::bridge_attached_now();
    if !bye(store, word, BYE_WAIT) && bridged {
        aterm_log::warn!(
            "fabric: the bridge did not take `bye {}` within {:.1}s; this instance's \
             sessions are withdrawn only if it reads the word before the lanes close, \
             and read `away` with its node otherwise",
            word.word(),
            BYE_WAIT.as_secs_f64()
        );
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

    /// Hang up the lanes of a bridge that has exited and tell its generation's loss, waiting on
    /// none of them: what is left is their ending ([`Retiring`]).
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
        for lane in &self.near {
            aterm_uds::hangup::hang_up(lane);
        }
        if let Some(lost) = self.lost {
            lost(self.generation);
        }
        Retiring {
            lanes_ended: self.lanes_ended,
        }
    }
}

/// A retired generation's lanes, ending behind the next launch ([`Launched::retire`]).
struct Retiring {
    /// Disconnected once no lane holds a [`LaneEnded`] any more.
    lanes_ended: std::sync::mpsc::Receiver<std::convert::Infallible>,
}

impl Retiring {
    /// Wait for every lane to let go of its [`LaneEnded`], warning if one has not `within` the
    /// bridge's exit: whether all had.
    fn watch(self, within: Duration) -> bool {
        let ended = !matches!(
            self.lanes_ended.recv_timeout(within),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        if !ended {
            aterm_log::warn!(
                "fabric bridge lanes had not ended {:.1}s after its exit; the next bridge runs \
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
            lost: Box::new(crate::control::fabric_bridge_exited),
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
    Ok(Launched {
        child,
        near,
        generation,
        lost: served.then_some(told.lost),
        lanes_ended,
    })
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
mod tests {
    use super::*;
    use crate::app_config::{Config, FabricCommand, FabricConfig};

    fn with_command(cmd: Option<&str>) -> Config {
        with_argv(cmd.map(aterm_types::fabric_command::split_line))
    }

    fn with_argv(argv: Option<Vec<String>>) -> Config {
        Config {
            fabric: Some(FabricConfig {
                command: argv.map(FabricCommand),
                hold_on_bridge_lost: None,
            }),
            ..Config::default()
        }
    }

    fn parsed(text: &str) -> Result<Config, String> {
        Config::parse(text).map_err(|e| e.to_string())
    }

    /// A [`Told`] that tells the process-wide link nothing, for a test about the lanes alone.
    fn untold() -> Told {
        Told {
            attached: |_| {},
            lost: Box::new(|_| {}),
        }
    }

    fn true_program() -> String {
        ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("a `true` binary")
            .to_string()
    }

    /// The bridge's argv is aterm.toml's `[fabric] command` (cap paths, broker addresses): the
    /// started line names its program's file name and counts its arguments (DESIGN §3.6).
    #[test]
    fn the_started_line_names_the_program_and_counts_its_arguments() {
        let argv = vec![true_program(), "QZP7E7E1".to_string()];
        let launches = std::cell::Cell::new(0_u32);
        let logged = crate::test_log::logged_while(|| {
            supervise_with(
                &argv,
                |argv| {
                    launches.set(launches.get() + 1);
                    launch_serving(argv, |_, _, _| true, untold())
                },
                || launches.get() > 0,
                |_| {},
            );
        });
        assert_eq!(launches.get(), 1, "one launch, then retired");
        assert!(
            logged
                .iter()
                .any(|line| line == "fabric bridge started: true (1 args)"),
            "the started line names its program: {} lines",
            logged.len()
        );
        assert!(
            !logged.iter().any(|line| line.contains("QZP7E7E")),
            "the log keeps the bridge's arguments"
        );
        assert_eq!(program_word("/opt/QZP7E7E2 x/bridge"), "bridge");
        assert_eq!(
            program_word("/opt/x/QZP7E7E3 bridge"),
            "?",
            "a name that is no word"
        );
    }

    #[test]
    fn the_pre_flight_resolves_argv0_the_way_spawn_will() {
        use std::ffi::OsStr;
        let truth = true_program();
        let bin_dir = std::path::Path::new(&truth).parent().unwrap();

        assert_eq!(preflight(&truth, None), Ok(()));
        assert_eq!(preflight("/nonexistent/aterm-link", None), Err("not-found"));
        assert_eq!(
            preflight(bin_dir.to_str().unwrap(), None),
            Err("not-a-file")
        );

        assert_eq!(
            preflight("true", Some(OsStr::new("/nonexistent:/usr/bin:/bin"))),
            Ok(())
        );
        assert_eq!(
            preflight("true", Some(OsStr::new("/nonexistent"))),
            Err("not-on-path")
        );
        assert_eq!(preflight("true", None), Ok(()));

        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-fabric-path-")
            .suffix(".noindex")
            .tempdir()
            .unwrap();
        let dir = scratch.path();
        std::fs::write(dir.join("true"), b"not a program\n").unwrap();
        std::fs::set_permissions(
            dir.join("true"),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644),
        )
        .unwrap();
        let mut path = dir.as_os_str().to_os_string();
        path.push(":");
        path.push(bin_dir.as_os_str());
        assert_eq!(
            preflight("true", Some(&path)),
            Ok(()),
            "a non-executable candidate is skipped"
        );
        assert_eq!(
            preflight("true", Some(dir.as_os_str())),
            Err("not-on-path"),
            "and alone it is no match at all"
        );
        assert_eq!(
            preflight(dir.join("true").to_str().unwrap(), None),
            Err("no-exec-bit"),
            "named by path, the same file is refused for its mode"
        );
    }

    #[test]
    fn the_lanes_are_retired_when_the_bridge_exits_whoever_else_holds_a_far_end() {
        use std::io::{BufRead as _, Read as _};
        use std::os::unix::net::UnixStream;
        let served = std::sync::Arc::new(std::sync::Mutex::new(Vec::<UnixStream>::new()));
        let recorder = {
            let served = std::sync::Arc::clone(&served);
            move |near: UnixStream, _generation, _ended| {
                served.lock().unwrap().push(near);
                true
            }
        };
        let argv: Vec<String> = ["/bin/sh", "-c", "sleep 300 & echo $! >&4; exit 0"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let mut launched =
            launch_serving(&argv, recorder, untold()).expect("launch the stand-in bridge");
        let (mut verb, push) = {
            let mut near = served.lock().unwrap();
            assert_eq!(near.len(), 2, "both lanes served");
            let push = near.pop().unwrap();
            (near.pop().unwrap(), push)
        };
        push.set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        verb.set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(&push)
            .read_line(&mut line)
            .expect("the holder's pid");
        let holder: i32 = line.trim().parse().expect("a pid");
        let status = launched.wait().expect("reap the bridge");
        launched.retire();
        let mut byte = [0u8; 1];
        let read = verb.read(&mut byte);
        // SAFETY: sends a signal to the live holder pid started by this test's bridge.
        unsafe {
            libc::kill(holder, libc::SIGKILL);
        }
        assert!(status.success(), "{status:?}");
        assert_eq!(
            read.map_err(|e| e.kind()),
            Ok(0),
            "the near end read no end-of-file while another process held the far one"
        );
    }

    #[test]
    fn a_lane_the_bridge_half_closed_is_retired_whoever_else_holds_a_far_end() {
        use std::io::{BufRead as _, Write as _};
        use std::os::unix::net::UnixStream;
        let perl = ["/usr/bin/perl", "/bin/perl"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("a `perl` binary");
        let served = std::sync::Arc::new(std::sync::Mutex::new(Vec::<UnixStream>::new()));
        let recorder = {
            let served = std::sync::Arc::clone(&served);
            move |near: UnixStream, _generation, _ended| {
                served.lock().unwrap().push(near);
                true
            }
        };
        let script = format!(
            "{perl} -e 'open(my $s, \"+<&=\", 3) or die \"fd 3: $!\"; \
             shutdown($s, 1) or die \"shutdown: $!\"' || exit 1; \
             sleep 300 & echo $! >&4; exit 0"
        );
        let argv: Vec<String> = ["/bin/sh", "-c", &script]
            .iter()
            .map(ToString::to_string)
            .collect();
        let mut launched =
            launch_serving(&argv, recorder, untold()).expect("launch the stand-in bridge");
        let (verb, push) = {
            let mut near = served.lock().unwrap();
            assert_eq!(near.len(), 2, "both lanes served");
            let push = near.pop().unwrap();
            (near.pop().unwrap(), push)
        };
        push.set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(&push)
            .read_line(&mut line)
            .expect("the holder's pid");
        let holder: i32 = line.trim().parse().expect("a pid");
        let status = launched.wait().expect("reap the bridge");
        launched.retire();
        let wrote = (&verb).write(b"after the retire\n").map_err(|e| e.kind());
        // SAFETY: sends a signal to the live holder pid started by this test's bridge.
        unsafe {
            libc::kill(holder, libc::SIGKILL);
        }
        assert!(status.success(), "{status:?}");
        assert_eq!(
            wrote,
            Err(std::io::ErrorKind::BrokenPipe),
            "a write to a retired lane reached the far end's holder"
        );
    }

    #[test]
    fn the_fabric_is_off_unless_it_is_configured() {
        if !crate::test_alone::alone() {
            return;
        }
        crate::test_env::scoped_unset(FABRIC_COMMAND_ENV, || {
            assert_eq!(configured_command(&Config::default()), None);
            assert_eq!(configured_command(&with_command(None)), None);
            assert_eq!(
                configured_command(&with_command(Some("   "))),
                None,
                "a blank command is OFF, not an empty argv"
            );
        });
    }

    #[test]
    fn the_argv_form_keeps_a_spaced_program_whole_and_the_line_still_splits() {
        if !crate::test_alone::alone() {
            return;
        }
        crate::test_env::scoped_unset(FABRIC_COMMAND_ENV, || {
            let program = "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm";
            let argv = parsed(&format!(
                "[fabric]\ncommand = [\"{program}\", \"link\", \"serve\", \";\", \"it's\"]\n"
            ))
            .expect("an argv parses");
            assert_eq!(
                configured_command(&argv),
                Some(vec![
                    program.to_string(),
                    "link".to_string(),
                    "serve".to_string(),
                    ";".to_string(),
                    "it's".to_string(),
                ])
            );
            let line = parsed(&format!("[fabric]\ncommand = \"{program} link serve\"\n"))
                .expect("a line parses");
            assert_eq!(
                configured_command(&line),
                Some(vec![
                    "/Users//u/Applications/aterm".to_string(),
                    "(dev).app/Contents/MacOS/aterm".to_string(),
                    "link".to_string(),
                    "serve".to_string(),
                ]),
                "the line form splits on whitespace, as it always has"
            );
            for off in ["command = []", "command = \"  \""] {
                let cfg = parsed(&format!("[fabric]\n{off}\n")).expect(off);
                assert_eq!(configured_command(&cfg), None, "{off} is the fabric off");
            }
        });
    }

    #[test]
    fn a_malformed_argv_is_refused_with_its_reason() {
        for (value, why) in [
            (r#"["aterm", "", "serve"]"#, "element 2 is empty"),
            (r#"["aterm", "a\u0000b"]"#, "element 2 holds a NUL"),
            (r#"["aterm", 3]"#, "a string"),
            (r#"["aterm", ["link"]]"#, "a string"),
            ("3", "an array of strings"),
            ("true", "an array of strings"),
        ] {
            let err = parsed(&format!("[fabric]\ncommand = {value}\n"))
                .expect_err(&format!("{value} is refused"));
            assert!(err.contains(why), "{value}: {err}");
        }
        let fixed = parsed("[fabric]\ncommand = [\"aterm\", \"link\", \"serve\"]\n")
            .expect("the fixed array parses");
        assert_eq!(
            fixed.fabric.and_then(|f| f.command),
            Some(FabricCommand(vec![
                "aterm".to_string(),
                "link".to_string(),
                "serve".to_string()
            ]))
        );
    }

    #[test]
    fn a_metacharacter_is_an_argument_and_not_a_second_command() {
        if !crate::test_alone::alone() {
            return;
        }
        let cfg = Config::default();
        crate::test_env::scoped(FABRIC_COMMAND_ENV, "bridge ; rm -rf /", || {
            assert_eq!(
                configured_command(&cfg),
                Some(vec![
                    "bridge".to_string(),
                    ";".to_string(),
                    "rm".to_string(),
                    "-rf".to_string(),
                    "/".to_string(),
                ]),
                "a shell would have run the second command"
            );
        });
    }
    #[test]
    fn a_healthy_run_relaunches_at_once_and_a_young_death_escalates_from_the_floor() {
        let instant = Duration::from_millis(5);

        assert_eq!(
            next_backoff(RELAUNCH_MAX, Some(RELAUNCH_HEALTHY)),
            Duration::ZERO
        );
        assert_eq!(
            next_backoff(RELAUNCH_MAX, Some(Duration::from_secs(3600))),
            Duration::ZERO
        );
        assert_eq!(
            next_backoff(
                RELAUNCH_MIN,
                Some(RELAUNCH_HEALTHY - Duration::from_millis(1))
            ),
            RELAUNCH_MIN * 2,
            "just short of healthy is a young death"
        );

        assert_eq!(
            next_backoff(Duration::ZERO, Some(instant)),
            RELAUNCH_MIN,
            "the relaunch that dies young waits again, or a healthy run could start a fork bomb"
        );
        assert_eq!(
            next_backoff(next_backoff(RELAUNCH_MAX, Some(RELAUNCH_HEALTHY)), None),
            RELAUNCH_MIN,
            "so does the relaunch that cannot start"
        );
        assert_eq!(next_backoff(RELAUNCH_MIN, None), RELAUNCH_MIN * 2);

        let mut backoff = RELAUNCH_MIN;
        for _ in 0..3 {
            backoff = next_backoff(backoff, Some(instant));
        }
        assert_eq!(backoff, RELAUNCH_MIN * 8, "an immediate exit must escalate");

        for _ in 0..20 {
            backoff = next_backoff(backoff, Some(instant));
        }
        assert_eq!(backoff, RELAUNCH_MAX);
    }

    /// What the loop itself waits, not only what [`next_backoff`] answers: nothing between a
    /// healthy run and the next launch, and the floor, doubling, after each that dies young.
    #[test]
    fn the_loop_waits_nothing_after_a_healthy_run_and_the_floor_after_a_young_death() {
        let healthy = vec![
            ["/bin/sleep", "/usr/bin/sleep"]
                .into_iter()
                .find(|p| std::path::Path::new(p).exists())
                .expect("a `sleep` binary")
                .to_string(),
            (RELAUNCH_HEALTHY + Duration::from_millis(100))
                .as_secs_f64()
                .to_string(),
        ];
        let young = vec![true_program()];
        let launches = std::cell::Cell::new(0_u32);
        let paused = std::cell::RefCell::new(Vec::new());
        supervise_with(
            &young,
            |argv| {
                let first = launches.get() == 0;
                launches.set(launches.get() + 1);
                launch_serving(
                    if first { &healthy } else { argv },
                    |_, _, _| true,
                    untold(),
                )
            },
            || launches.get() == 3,
            |waited| paused.borrow_mut().push(waited),
        );
        assert_eq!(
            paused.into_inner(),
            [Duration::ZERO, RELAUNCH_MIN, RELAUNCH_MIN * 2]
        );
    }

    /// What [`the_link_over_a_relaunch`] read.
    struct Relaunch {
        /// What the link's hook told its hearer, the node log (`session_log_node::install`), as
        /// `(state, generation)` in order.
        heard: Vec<(String, u64)>,
        /// What the loop asked to wait after each bridge's exit.
        paused: Vec<Duration>,
        /// Whether both of the first bridge's lanes, hung up, were still up when the second
        /// bridge had attached and freed the session.
        freed_beside_the_first: bool,
        /// The governed session's hold once the first bridge's lanes had said their loss, the
        /// second bridge still up and serving it.
        hold_after_the_late_loss: Option<String>,
    }

    /// Two bridges under the supervisor's own loop, its pauses recorded and not slept, over one
    /// session they govern, with `[fabric] hold_on_bridge_lost` on. Each bridge reads its verb
    /// lane until aterm's end of it closes for writing, so its verb lane says when it exits.
    ///
    /// The lanes are what `control::attach_fabric_bridge` and its `serve_bridge` do of a lane,
    /// reduced to the attach, reading to the hang-up and then saying the loss (`BridgeLostGuard`).
    /// The first bridge frees the session (a `hold off`, which governs it) and exits. Its lanes
    /// stay past their hang-up until the second bridge has attached and freed the session as a
    /// reconnecting bridge's `hold off` does. The second bridge stays up until both of them have
    /// said their loss and the session's hold is read.
    ///
    /// `exit_tells_loss` is the supervisor telling each generation's loss at its exit, as
    /// `control::fabric_bridge_exited` does; with it off, the lanes alone say it.
    fn the_link_over_a_relaunch(exit_tells_loss: bool) -> Relaunch {
        use std::os::unix::net::UnixStream;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize};

        crate::fabric::set_hold_on_bridge_lost(true);
        let log = crate::session_log_node::node_log();
        let before = log.last_seq();
        let store = crate::session_store::new_store();
        let handle = crate::session_store::test_handle(1);
        let sid = handle.sid.as_str().to_string();
        let ctx = handle.ctx.clone();
        store.write().unwrap().register(handle);
        let argv: Vec<String> = ["/bin/sh", "-c", "exec cat <&3 >/dev/null"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let threads = Arc::new(Mutex::new(Vec::new()));
        let freed = Arc::new(AtomicBool::new(false));
        let lingered = Arc::new(AtomicUsize::new(0));
        let late_said = Arc::new(AtomicUsize::new(0));
        let hold_read = Arc::new(Mutex::new(None::<Option<String>>));
        let answers = Arc::new(Mutex::new(Vec::new()));
        let exits = Arc::new(Mutex::new(Vec::new()));
        let lane = |first: bool| {
            let (store, sid, threads) = (store.clone(), sid.clone(), Arc::clone(&threads));
            let (freed, lingered) = (Arc::clone(&freed), Arc::clone(&lingered));
            let (late_said, hold_read, ctx) =
                (Arc::clone(&late_said), Arc::clone(&hold_read), ctx.clone());
            let (answers, exits) = (Arc::clone(&answers), Arc::clone(&exits));
            let lanes = AtomicUsize::new(0);
            move |near: UnixStream,
                  generation: crate::fabric::BridgeGeneration,
                  ended: LaneEnded| {
                let verb = lanes.fetch_add(1, Ordering::SeqCst) == 0;
                let (store, sid) = (store.clone(), sid.clone());
                let (freed, lingered) = (Arc::clone(&freed), Arc::clone(&lingered));
                let (late_said, hold_read, ctx) =
                    (Arc::clone(&late_said), Arc::clone(&hold_read), ctx.clone());
                let answers = Arc::clone(&answers);
                let free = {
                    let store = store.clone();
                    move || {
                        let rest = format!("{sid} off");
                        crate::fabric::cmd_hold(&store, &rest, crate::fabric::HoldIssuer::Bridge)
                    }
                };
                let must = |done: &dyn Fn() -> bool| {
                    aterm_spec::wait::within(
                        aterm_spec::wait::MUST_HAPPEN,
                        Duration::from_millis(1),
                        || done().then_some(()),
                    )
                    .is_some()
                };
                // Asserting nothing here: a lane that panicked before its shutdown would leave its
                // bridge reading for good and the supervisor's wait with it. The test asserts what
                // the lanes recorded once every one has been joined.
                let exit = {
                    let exits = Arc::clone(&exits);
                    move |near: &UnixStream| {
                        let shut = near.shutdown(std::net::Shutdown::Write);
                        exits.lock().unwrap().push(shut.map_err(|e| e.kind()));
                    }
                };
                let thread = crate::qos::spawn_lane_thread(
                    "aterm-fabric-bridge".to_string(),
                    crate::qos::Role::Responsive,
                    move || {
                        let _ended = ended;
                        crate::fabric::set_lane_generation(generation);
                        crate::fabric::bridge_attached(generation);
                        if first {
                            if verb {
                                let answer = free();
                                exit(&near);
                                answers.lock().unwrap().push(("first", answer));
                            }
                            let _ = std::io::copy(&mut &near, &mut std::io::sink());
                            if must(&|| freed.load(Ordering::SeqCst)) {
                                lingered.fetch_add(1, Ordering::SeqCst);
                            }
                            crate::fabric::bridge_lost(&store, generation);
                            late_said.fetch_add(1, Ordering::SeqCst);
                        } else {
                            if verb {
                                let answer = free();
                                freed.store(true, Ordering::SeqCst);
                                let _ = must(&|| late_said.load(Ordering::SeqCst) == 2);
                                *hold_read.lock().unwrap() =
                                    Some(ctx.fabric.hold().map(|hold| hold.reason));
                                exit(&near);
                                answers.lock().unwrap().push(("second", answer));
                            }
                            let _ = std::io::copy(&mut &near, &mut std::io::sink());
                            crate::fabric::bridge_lost(&store, generation);
                        }
                    },
                )
                .expect("a lane's thread");
                threads.lock().unwrap().push(thread);
                true
            }
        };
        let told = || Told {
            attached: crate::fabric::bridge_attached,
            lost: if exit_tells_loss {
                let store = store.clone();
                Box::new(move |generation| {
                    crate::fabric::bridge_lost(&store, generation);
                })
            } else {
                Box::new(|_| {})
            },
        };
        let launches = std::cell::Cell::new(0_u32);
        let paused = std::cell::RefCell::new(Vec::new());
        supervise_with(
            &argv,
            |argv| {
                let first = launches.get() == 0;
                launches.set(launches.get() + 1);
                launch_serving(argv, lane(first), told())
            },
            || launches.get() == 2,
            |waited| paused.borrow_mut().push(waited),
        );
        for thread in std::mem::take(&mut *threads.lock().unwrap()) {
            thread.join().expect("a lane's thread");
        }
        assert_eq!(
            *exits.lock().unwrap(),
            [Ok(()), Ok(())],
            "each bridge's verb lane shut for writing"
        );
        assert_eq!(
            *answers.lock().unwrap(),
            [
                ("first", "OK hold=0\n".to_string()),
                ("second", "OK hold=0\n".to_string())
            ],
            "the first bridge governs the session, and the second frees it"
        );
        let word = |line: &str, key: &str| -> String {
            line.split(key)
                .nth(1)
                .and_then(|rest| rest.split(' ').next())
                .unwrap_or_else(|| panic!("no{key} in {line}"))
                .to_string()
        };
        let heard = log
            .lines()
            .into_iter()
            .filter(|l| {
                l.split(' ').next().and_then(|s| s.parse::<u64>().ok()) > Some(before)
                    && l.contains(" mark n link ")
            })
            .map(|l| {
                let generation = word(&l, " gen=").parse().expect("a generation");
                (word(&l, " state="), generation)
            })
            .collect();
        let hold_after_the_late_loss = hold_read
            .lock()
            .unwrap()
            .take()
            .expect("the second bridge read the hold");
        Relaunch {
            heard,
            paused: paused.into_inner(),
            freed_beside_the_first: lingered.load(Ordering::SeqCst) == 2,
            hold_after_the_late_loss,
        }
    }

    /// THE ORDER A RELAUNCH AT ONCE RESTS ON, however long the old generation's lanes take: the
    /// first bridge's lanes say their loss only after the second has attached, and the link still
    /// reads the first `disconnected` ahead of the second's attach. Its negative control: the same
    /// relaunch with the loss said by the lanes alone (planted by `exit_tells_loss` off) loses the
    /// first one's `Lost` to the second one's attach.
    #[test]
    fn a_relaunched_generation_attaches_after_the_old_generations_loss() {
        crate::session_log_node::install();
        crate::fabric::with_link_reset(|| {
            let Relaunch { heard, .. } = the_link_over_a_relaunch(true);
            let states: Vec<&str> = heard.iter().map(|(state, _)| state.as_str()).collect();
            assert_eq!(
                states,
                ["stalled", "disconnected", "stalled", "disconnected"],
                "attach, loss, attach, loss: {heard:?}"
            );
            assert_eq!(heard[0].1, heard[1].1, "{heard:?}");
            assert_eq!(heard[2].1, heard[3].1, "{heard:?}");
            assert!(heard[1].1 < heard[2].1, "{heard:?}");
        });
        crate::fabric::with_link_reset(|| {
            let Relaunch { heard, .. } = the_link_over_a_relaunch(false);
            let lost: Vec<u64> = heard
                .iter()
                .filter(|(state, _)| state == "disconnected")
                .map(|(_, generation)| *generation)
                .collect();
            let attached = heard.iter().rev().find(|(state, _)| state == "stalled");
            assert_eq!(
                Some(lost.as_slice()),
                attached.map(|(_, generation)| std::slice::from_ref(generation)),
                "said by the lanes alone, the old generation's loss lands past the attach and is \
                 no transition: {heard:?}"
            );
        });
    }

    /// A RELAUNCH WAITS ON NO LANE OF THE BRIDGE IT REPLACES, AND THAT BRIDGE'S LATE LOSS HOLDS NO
    /// SESSION THE NEW ONE FREED. Under `[fabric] hold_on_bridge_lost`, the first bridge's exit
    /// holds the session it governed; the second launches after the back-off alone, while the
    /// first's lanes are still up, and frees it. The first's lanes then say their loss, a dead
    /// generation's, and the session stays free.
    ///
    /// NEGATIVE CONTROL: without the `superseded` check on `HoldAct::Lost` in
    /// `aterm_gui_fabric::apply_hold_as`, the late loss holds the session again
    /// (`hold_after_the_late_loss` reads `Some("fabric-lost")`, 2026-11-05).
    #[test]
    fn a_relaunch_waits_on_no_old_lane_and_the_old_ones_late_loss_holds_no_session() {
        crate::session_log_node::install();
        crate::fabric::with_link_reset(|| {
            let relaunch = the_link_over_a_relaunch(true);
            assert_eq!(
                relaunch.paused,
                [RELAUNCH_MIN * 2, RELAUNCH_MIN * 4],
                "the loop waits the back-off after each young death and nothing else"
            );
            assert!(
                relaunch.freed_beside_the_first,
                "the second bridge attached and freed the session while the first's lanes were \
                 still up: {:?}",
                relaunch.heard
            );
            assert_eq!(
                relaunch.hold_after_the_late_loss, None,
                "a dead generation's late loss held a session the new bridge serves"
            );
        });
    }

    #[test]
    fn lanes_that_do_not_end_are_warned_of_and_hold_no_relaunch_up() {
        use std::os::unix::net::UnixStream;
        let kept = std::sync::Arc::new(Mutex::new(Vec::<(UnixStream, LaneEnded)>::new()));
        let recorder = {
            let kept = std::sync::Arc::clone(&kept);
            move |near: UnixStream,
                  _generation: crate::fabric::BridgeGeneration,
                  ended: LaneEnded| {
                kept.lock().unwrap().push((near, ended));
                true
            }
        };
        let argv = vec![true_program()];
        let warning = |logged: &[String]| logged.iter().any(|l| l.contains("had not ended"));

        let mut wedged =
            launch_serving(&argv, &recorder, untold()).expect("launch the stand-in bridge");
        wedged.wait().expect("reap the bridge");
        let retiring = wedged.retire();
        let logged = crate::test_log::logged_while(|| {
            assert!(!retiring.watch(Duration::from_millis(1)), "a lane is held");
        });
        assert!(warning(&logged), "{logged:?}");

        let mut ended =
            launch_serving(&argv, |_, _, _| true, untold()).expect("launch the stand-in bridge");
        ended.wait().expect("reap the bridge");
        let retiring = ended.retire();
        let logged = crate::test_log::logged_while(|| {
            assert!(retiring.watch(LANES_END_WAIT), "no lane is held");
        });
        assert!(!warning(&logged), "{logged:?}");
    }

    #[test]
    fn the_bridge_child_inherits_no_identity_no_socket_and_no_edge_secret() {
        let pair = |k: &str, v: &str| (std::ffi::OsString::from(k), std::ffi::OsString::from(v));
        let inherited = vec![
            pair("PATH", "/usr/bin"),
            pair("HOME", "/home/a"),
            pair("XDG_STATE_HOME", "/home/a/.local/state"),
            pair("ATERM_LINK_FAULT", "kill-after-publish"),
            pair("ATERM_LINK_WAIT_TALLY", "1"),
            pair("ATERM_EDGE_READ", "0011"),
            pair("ATERM_EDGE_WRITE", "2233"),
            pair("ATERM_EDGE_SIGNAL", "4455"),
            pair("ATERM_EDGE_TOKENS", "/run/aterm/edge.tok"),
            pair("ATERM_SESSION_ID", "s-0123456789abcdef0123"),
            pair("ATERM_LAUNCH_NONCE", "0".repeat(32).as_str()),
            pair("ATERM_LINK_BROKER", "/run/aterm/broker.sock"),
            pair("ATERM_LINK_CAP_FILE", "/etc/aterm/outer.cap"),
            pair("ATERM_LINK_FLEET", "outer"),
            pair("ATERM_FABRIC_COMMAND", "aterm-link serve --fleet outer"),
            pair("ANTHROPIC_API_KEY", "sk-x"),
            pair("ATERM_DEV_FAST_TIMERS", "1"),
        ];
        let kept = filter_child_env(inherited.into_iter());
        let has = |k: &str| kept.iter().any(|(n, _)| n == std::ffi::OsStr::new(k));

        for secret in [
            "ATERM_EDGE_READ",
            "ATERM_EDGE_WRITE",
            "ATERM_EDGE_SIGNAL",
            "ATERM_EDGE_TOKENS",
            "ATERM_SESSION_ID",
            "ATERM_LAUNCH_NONCE",
            "ATERM_LINK_BROKER",
            "ATERM_LINK_CAP_FILE",
            "ATERM_LINK_FLEET",
            "ATERM_FABRIC_COMMAND",
            "ANTHROPIC_API_KEY",
        ] {
            assert!(!has(secret), "the bridge child must not inherit {secret}");
        }

        for keeper in [
            "PATH",
            "HOME",
            "XDG_STATE_HOME",
            "ATERM_LINK_FAULT",
            "ATERM_LINK_WAIT_TALLY",
        ] {
            assert!(has(keeper), "the bridge child still needs {keeper}");
        }
        assert_eq!(
            has("ATERM_DEV_FAST_TIMERS"),
            cfg!(any(debug_assertions, feature = "dev-seams")),
            "the bridge runs its instance's clock"
        );
    }

    #[test]
    fn the_supervisor_arms_once_and_records_what_it_runs() {
        crate::fabric::with_link_reset(|| {
            let fresh = status();
            assert!(fresh.armed.is_none() && fresh.configured.is_none());
            assert!(!crate::fabric::bridge_supervised());
            assert!(matches!(arm(Vec::new()), Err(ArmError::NoCommand)));
            assert!(
                !crate::fabric::bridge_supervised(),
                "an empty argv must not flip the endpoint's latch"
            );

            for (argv0, reason) in [
                ("/nonexistent/aterm-link", "not-found"),
                ("/", "not-a-file"),
                ("aterm-link-no-such-program-4f2c", "not-on-path"),
            ] {
                match arm(vec![argv0.to_string(), "serve".to_string()]) {
                    Err(ArmError::NotExecutable {
                        program,
                        reason: got,
                    }) => {
                        assert_eq!(program, argv0);
                        assert_eq!(got, reason, "{argv0}");
                    }
                    other => panic!("{argv0} must be refused as not executable: {other:?}"),
                }
                assert!(status().armed.is_none(), "{argv0} must arm nothing");
                assert!(
                    !crate::fabric::bridge_supervised(),
                    "{argv0} must not flip the endpoint's latch"
                );
            }
            let scratch = aterm_tempfile::Builder::new()
                .prefix("aterm-fabric-preflight-")
                .suffix(".noindex")
                .tempdir()
                .unwrap();
            let plain = scratch.path().join("not-a-program");
            std::fs::write(&plain, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(
                &plain,
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644),
            )
            .unwrap();
            let plain_str = plain.to_string_lossy().into_owned();
            assert!(matches!(
                arm(vec![plain_str.clone()]),
                Err(ArmError::NotExecutable {
                    reason: "no-exec-bit",
                    ..
                })
            ));
            assert!(status().armed.is_none() && !crate::fabric::bridge_supervised());

            let argv = vec![true_program(), "serve".to_string()];
            assert!(
                arm(argv.clone()).is_ok(),
                "the refused attempts left the latch open"
            );
            assert!(crate::fabric::bridge_supervised(), "arming IS the latch");
            assert_eq!(status().armed.as_deref(), Some(argv.as_slice()));

            let second = vec!["/nonexistent/other".to_string()];
            match arm(second) {
                Err(ArmError::AlreadySupervised(running)) => assert_eq!(running, argv),
                other => panic!("a second arm must be refused: {other:?}"),
            }
            assert_eq!(
                status().armed.as_deref(),
                Some(argv.as_slice()),
                "the first argv stays"
            );
        });
    }

    #[test]
    fn the_startup_path_records_the_configured_command_and_arms_through_the_one_seam() {
        if !crate::test_alone::alone() {
            return;
        }
        crate::test_env::scoped_unset(FABRIC_COMMAND_ENV, || {
            crate::fabric::with_link_reset(|| {
                assert!(!spawn_supervisor(&Config::default()));
                let s = status();
                assert!(s.armed.is_none() && s.configured.is_none());
                assert!(!crate::fabric::bridge_supervised());
            });
            crate::fabric::with_link_reset(|| {
                assert!(!spawn_supervisor(&with_command(Some(
                    "/nonexistent/aterm-link serve --fleet lab"
                ))));
                let s = status();
                assert_eq!(
                    s.configured,
                    Some(vec![
                        "/nonexistent/aterm-link".to_string(),
                        "serve".to_string(),
                        "--fleet".to_string(),
                        "lab".to_string(),
                    ])
                );
                assert!(s.armed.is_none(), "a command that cannot run arms nothing");
                assert!(!crate::fabric::bridge_supervised());
            });
            crate::fabric::with_link_reset(|| {
                let program = true_program();
                assert!(spawn_supervisor(&with_command(Some(&format!(
                    "{program} serve --fleet lab"
                )))));
                let want = vec![
                    program,
                    "serve".to_string(),
                    "--fleet".to_string(),
                    "lab".to_string(),
                ];
                let s = status();
                assert_eq!(s.configured, Some(want.clone()));
                assert_eq!(s.armed, Some(want));
                assert!(crate::fabric::bridge_supervised());
            });
        });
    }

    #[test]
    fn a_runtime_attached_bridge_is_armed_again_after_an_update() {
        if !crate::test_alone::alone() {
            return;
        }
        use crate::session_store::{SessionHandoff, SessionStore};
        crate::test_env::scoped_unset(FABRIC_COMMAND_ENV, || {
            let argv = vec![true_program(), "serve".to_string(), "--fleet".to_string()];
            let wire = crate::fabric::with_link_reset(|| {
                assert!(!spawn_supervisor(&Config::default()));
                arm(argv.clone()).expect("the attach arms");
                assert_eq!(handoff_attached(), argv);
                let manifest = SessionHandoff::from_store(&SessionStore::default());
                assert_eq!(manifest.fabric_attached, argv);
                manifest.to_toml().expect("serializes")
            });
            crate::fabric::with_link_reset(|| {
                let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
                carry_attached(read.fabric_attached);
                assert!(spawn_supervisor(&Config::default()));
                let s = status();
                assert_eq!(s.armed, Some(argv.clone()), "the attached bridge is back");
                assert_eq!(s.configured, None);
                assert!(crate::fabric::bridge_supervised());
                assert_eq!(handoff_attached(), argv);
            });
            crate::fabric::with_link_reset(|| {
                let program = true_program();
                assert!(spawn_supervisor(&with_command(Some(&format!(
                    "{program} serve"
                )))));
                assert!(handoff_attached().is_empty());
            });
            crate::fabric::with_link_reset(|| {
                carry_attached(vec!["/nonexistent/carried".to_string()]);
                let program = true_program();
                assert!(spawn_supervisor(&with_command(Some(&format!(
                    "{program} serve"
                )))));
                assert_eq!(status().armed, Some(vec![program, "serve".to_string()]));
            });
        });
    }

    #[test]
    fn a_quit_says_bye_to_every_watcher_and_waits_only_for_a_bridge_to_take_it() {
        crate::fabric::with_link_reset(|| {
            let store = crate::session_store::new_store();
            let seq = |store: &crate::session_store::Store| {
                store.read().unwrap_or_else(|p| p.into_inner()).roster_seq()
            };
            assert!(!bye(&store, Bye::Quit, aterm_spec::wait::MUST_HAPPEN));
            assert_eq!(seq(&store), 1, "no bridge, and said all the same");

            let generation = crate::fabric::next_bridge_generation();
            crate::fabric::bridge_attached(generation);
            assert!(!bye(&store, Bye::Handoff, Duration::ZERO));
            assert_eq!(seq(&store), 2, "said, and not waited for");

            let bridge = {
                let store = store.clone();
                std::thread::spawn(move || {
                    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                    while seq(&store) < 3 {
                        assert!(Instant::now() < deadline, "the quit was never said");
                        aterm_uds::deadline::poll_yield();
                    }
                    crate::fabric::cmd_link(&store, "bye quit", Some(generation))
                })
            };
            assert!(bye(&store, Bye::Quit, aterm_spec::wait::MUST_HAPPEN));
            assert_eq!(bridge.join().expect("the bridge's answer"), "OK\n");
        });
    }

    #[test]
    fn a_retired_supervisor_relaunches_nothing() {
        crate::fabric::with_link_reset(|| {
            RETIRED.store(true, Ordering::SeqCst);
            let (done, returned) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                supervise(&["/nonexistent/aterm-link".to_string()]);
                let _ = done.send(());
            });
            assert!(
                returned.recv_timeout(aterm_spec::wait::MUST_HAPPEN).is_ok(),
                "a retired supervisor went on launching"
            );
        });
    }

    /// The Overview reads the fabric where the supervisor names a command either way: armed
    /// alone (a bridge `aterm fabric on` attached) or configured alone (a `[fabric] command`
    /// whose preflight failed), as well as both; with neither it names none.
    #[test]
    fn an_armed_or_a_configured_command_alone_names_the_bridge() {
        let status = |armed: bool, configured: bool| SupervisorStatus {
            armed: armed.then(|| vec!["bridge".to_owned()]),
            configured: configured.then(|| vec!["bridge".to_owned()]),
        };
        assert!(!status(false, false).names_a_command());
        assert!(status(true, false).names_a_command(), "attached at runtime");
        assert!(status(false, true).names_a_command(), "configured, unarmed");
        assert!(status(true, true).names_a_command());
    }

    #[test]
    fn a_quit_says_quit_unless_the_seam_names_handoff() {
        use std::ffi::OsStr;
        assert_eq!(quit_bye(None), Bye::Quit);
        assert_eq!(quit_bye(Some(OsStr::new("handoff"))), Bye::Handoff);
        for other in ["", "quit", "1", "Handoff"] {
            assert_eq!(quit_bye(Some(OsStr::new(other))), Bye::Quit, "{other:?}");
        }
    }
}
