// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bridge launcher: starts and supervises the process that holds `Scope::Bridge`.

use aterm_types::fabric_grammar::Bye;
use aterm_uds::spawnfd::{ChildStdio, LaneChild, LaneCommand};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The dev-build override for `[fabric] command`, read only through [`aterm_types::dev_seam!`].
const FABRIC_COMMAND_ENV: &str = "ATERM_FABRIC_COMMAND";

/// The back-off between relaunches, and its ceiling. A bridge that fails at
/// startup (a bad cap file, an unreachable broker) must not become a fork bomb;
/// a bridge that was killed once must come back promptly.
const RELAUNCH_MIN: Duration = Duration::from_millis(200);
const RELAUNCH_MAX: Duration = Duration::from_secs(30);

/// How long a child must run before its exit counts as "it was working, then it stopped"
/// rather than "it cannot start".
const RELAUNCH_HEALTHY: Duration = Duration::from_secs(5);

/// How long a QUIT waits for its bridge to take `bye quit` ([`bye`]). The
/// bridge answers before it publishes anything — the withdrawal needs aterm no
/// longer once the word is read — so this is a push-loop wake and one verb
/// round trip; the bound is for a bridge that is wedged, and then the quit
/// goes on without the withdrawal (the node's will reads `away`).
pub(crate) const BYE_WAIT: Duration = Duration::from_secs(1);

/// A development build's QA seam: `ATERM_DEBUG_QUIT_BYE=handoff` makes a quit
/// say `bye handoff` — the bridge-visible half of a committed handoff (its word,
/// then the lanes closing), which a headless instance cannot reach any other
/// way: it cannot hand off.
const QUIT_BYE_SEAM: &str = "ATERM_DEBUG_QUIT_BYE";

/// The configured bridge command, or `None` when the fabric is off.
#[must_use]
pub(crate) fn configured_command(config: &crate::app_config::Config) -> Option<Vec<String>> {
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

/// `[fabric] hold_on_bridge_lost`: `true` is the fail-closed sweep (a lost bridge holds every
/// session it governed); absent or `false`, the default, leaves them running with an advisory
/// ([`crate::fabric::bridge_lost`]).
#[must_use]
pub(crate) fn hold_on_bridge_lost(config: &crate::app_config::Config) -> bool {
    config
        .fabric
        .as_ref()
        .and_then(|f| f.hold_on_bridge_lost)
        .unwrap_or(false)
}

/// The process-wide supervisor record; `armed` is a one-way latch.
struct Supervisor {
    /// The argv the supervisor thread was armed with — `Some` exactly once per
    /// process, and never `None` again.
    armed: Option<Vec<String>>,
    /// The command the instance was launched with, if any.
    configured: Option<Vec<String>>,
    /// The argv a seamless update's outgoing process was armed with by `fabric attach`, put
    /// here by the successor's intake ([`carry_attached`]) and taken by [`spawn_supervisor`]
    /// when no `[fabric] command` is configured.
    carried: Option<Vec<String>>,
}

static SUPERVISOR: Mutex<Supervisor> = Mutex::new(Supervisor {
    armed: None,
    configured: None,
    carried: None,
});

/// The process is QUITTING and has told its bridge so ([`bye_at_quit`]): a
/// bridge that exits from here on is not relaunched — a relaunched one would
/// attach, read the roster and advertise `live` every session the quit just
/// withdrew.
static RETIRED: AtomicBool = AtomicBool::new(false);

/// The update attempt whose Commit said `bye handoff` last
/// ([`bye_at_handoff`]); `0`, which no attempt is, before any.
static HANDOFF_BYE_ATTEMPT: AtomicU64 = AtomicU64::new(0);

fn supervisor() -> std::sync::MutexGuard<'static, Supervisor> {
    SUPERVISOR.lock().unwrap_or_else(|p| p.into_inner())
}

/// Why [`arm`] did not start a supervisor.
#[derive(Debug)]
pub(crate) enum ArmError {
    /// The argv was empty. Nothing is armed.
    NoCommand,
    /// `argv[0]` cannot be executed ([`preflight`] gives `reason`). Refused before the latch
    /// closes, so nothing is armed and a corrected `attach` is allowed.
    NotExecutable {
        program: String,
        reason: &'static str,
    },
    /// One is already running, with this argv. The request's argv did not take, which is why
    /// this is an error and not `OK already`.
    AlreadySupervised(Vec<String>),
    /// The thread could not be spawned. Nothing is armed; a retry is allowed.
    Thread(std::io::Error),
}

/// What `fabric status` reports about the supervisor: the argv it runs when one
/// is armed, and the command the instance was launched with.
pub(crate) struct SupervisorStatus {
    pub(crate) armed: Option<Vec<String>>,
    pub(crate) configured: Option<Vec<String>>,
}

/// A snapshot of the [`Supervisor`] record.
pub(crate) fn status() -> SupervisorStatus {
    let g = supervisor();
    SupervisorStatus {
        armed: g.armed.clone(),
        configured: g.configured.clone(),
    }
}

/// Launch and supervise the bridge, if one is configured or carried. Returns whether a
/// supervisor thread started; `false` never means a half-attached bridge.
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
                "fabric bridge supervisor not started: `{program}` is {reason}; fix [fabric] \
                 command in aterm.toml, then `aterm ctl fabric attach` arms it"
            );
            false
        }
        Err(e) => {
            aterm_log::warn!("fabric bridge supervisor could not start: {e:?}");
            false
        }
    }
}

/// The bridge command this instance's configuration names NOW: a development
/// build's `$ATERM_FABRIC_COMMAND` seam, else `[fabric] command` in aterm.toml,
/// READ AGAIN — so the command `aterm fabric on` wrote after this instance
/// launched is the instance's own.
#[cfg(not(test))]
pub(crate) fn command_on_disk() -> Option<Vec<String>> {
    let config = crate::app_config::config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| crate::app_config::Config::parse(&text).ok())
        .unwrap_or_default();
    configured_command(&config)
}

/// [`command_on_disk`] in a test process: what the test said the file names
/// ([`set_command_on_disk_for_tests`]), never the developer's own aterm.toml.
#[cfg(test)]
pub(crate) fn command_on_disk() -> Option<Vec<String>> {
    COMMAND_ON_DISK_FOR_TESTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

#[cfg(test)]
static COMMAND_ON_DISK_FOR_TESTS: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// Say what aterm.toml's `[fabric] command` names for [`command_on_disk`], in a
/// test section ([`crate::fabric::with_link_reset`] clears it).
#[cfg(test)]
pub(crate) fn set_command_on_disk_for_tests(argv: Option<Vec<String>>) {
    *COMMAND_ON_DISK_FOR_TESTS
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = argv;
}

/// Arm the supervisor with `argv`: the one seam that starts a `supervise` thread, for startup
/// ([`spawn_supervisor`]) and `fabric attach`.
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

/// PRE-FLIGHT: whether `argv[0]` can be executed at all, checked BEFORE the
/// one-way latch closes.
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

/// Any execute bit, as `execve` requires of the mode. The uid-specific check (`access(2)`) is
/// not attempted: a wrong pass falls back to the supervisor's logged retry, a wrong refusal
/// would block a program that runs.
fn is_executable(md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o111 != 0
}

/// TELL EVERY WATCHER HOW THIS PROCESS LEAVES (module header): push `EVENT *
/// bye <word>` through the instance stream — the bridge's push lane drains it,
/// and so does every other `sessions` subscriber (`aterm ctl subscribe @*
/// events,sessions`), bridge or none, as the subscribe help says — wake the
/// lanes now rather than at their next 250 ms tick, and wait, each bounded by
/// `wait`: for the bridge's `link bye <word>` when one is attached, then for
/// every `sessions` stream to have written the word to its client
/// ([`crate::subscribe::await_roster_written`]) before the quit closes their
/// connections.
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

/// Journal `bye <word>` for the push lane and wake it ([`bye`]): the roster
/// seq it was said at, `None` when it was not.
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

/// THE HANDOFF'S BYE, at a seamless update's Commit (`app_update_handoff`):
/// `bye handoff`, waiting for nothing, ONCE PER ATTEMPT — a Commit that stands
/// down and retries says nothing more — and never blocking the Commit on the
/// store's lock: a busy store skips the word, and the next try of the same
/// attempt says it.
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

/// THE QUIT'S BYE, from the one place every quit passes — the end of the
/// event loop (`lib.rs`), which a handoff's Commit never reaches (a headless
/// instance's `SIGTERM` passes it too, `owned_endpoint`): retire the
/// supervisor, then say `bye quit` to every watcher ([`bye`]) and wait for it,
/// bounded by [`BYE_WAIT`].
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

/// The word a quit says: `quit`, unless [`QUIT_BYE_SEAM`] names `handoff`.
fn quit_bye(seam: Option<&std::ffi::OsStr>) -> Bye {
    match seam.and_then(std::ffi::OsStr::to_str) {
        Some("handoff") => Bye::Handoff,
        _ => Bye::Quit,
    }
}

/// Clear the record for a test section; called from `fabric::with_link_reset`, which
/// serializes the tests that touch the process-wide link.
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

/// What a seamless update's manifest carries of the supervisor
/// ([`crate::session_store::SessionHandoff::fabric_attached`]): the armed argv when `fabric
/// attach` armed it (it differs from the configured command), else nothing; the successor
/// reads the same configuration.
pub(crate) fn handoff_attached() -> Vec<String> {
    let g = supervisor();
    match &g.armed {
        Some(argv) if g.configured.as_ref() != Some(argv) => argv.clone(),
        _ => Vec::new(),
    }
}

/// The successor's intake of [`handoff_attached`]: kept for
/// [`spawn_supervisor`], which arms it when no command is configured.
pub(crate) fn carry_attached(argv: Vec<String>) {
    if !argv.is_empty() {
        supervisor().carried = Some(argv);
    }
}

/// Launch, wait, back off, launch again — forever, because the fabric is a
/// standing service and this process is its supervisor — until the process
/// quits and retires it ([`bye_at_quit`]).
fn supervise(argv: &[String]) {
    let mut backoff = RELAUNCH_MIN;
    loop {
        if RETIRED.load(Ordering::SeqCst) {
            aterm_log::info!("fabric bridge supervisor retired: aterm is quitting");
            return;
        }
        match launch_once(argv) {
            Ok(launched) => {
                aterm_log::info!("fabric bridge started: {}", argv.join(" "));
                let started = Instant::now();
                let status = launched.wait_and_retire();
                let ran = started.elapsed();
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
        std::thread::sleep(backoff);
    }
}

/// The next back-off, given the current one and how long the child ran (`None`: the spawn
/// failed).
fn next_backoff(current: Duration, ran: Option<Duration>) -> Duration {
    match ran {
        Some(ran) if ran >= RELAUNCH_HEALTHY => RELAUNCH_MIN,
        _ => (current * 2).min(RELAUNCH_MAX),
    }
}

/// One bridge generation: the child, and a second handle on each near end, so the lanes can be
/// retired when the child exits.
struct Launched {
    child: LaneChild,
    near: [std::os::unix::net::UnixStream; 2],
}

impl Launched {
    /// Wait for the bridge to exit, then shut both lanes down.
    fn wait_and_retire(mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.child.wait();
        for lane in &self.near {
            aterm_uds::hangup::hang_up(lane);
        }
        status
    }
}

/// One launch: two socketpairs, the child, and both near ends attached.
fn launch_once(argv: &[String]) -> std::io::Result<Launched> {
    launch_serving(argv, crate::control::attach_fabric_bridge)
}

/// [`launch_once`], serving each near end with `attach` (the control server's
/// [`crate::control::attach_fabric_bridge`]; a test's recorder).
fn launch_serving(
    argv: &[String],
    attach: impl Fn(std::os::unix::net::UnixStream, crate::fabric::BridgeGeneration) -> bool,
) -> std::io::Result<Launched> {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    let (near_verb, far_verb) = UnixStream::pair()?;
    let (near_push, far_push) = UnixStream::pair()?;
    let near = [near_verb.try_clone()?, near_push.try_clone()?];
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
    if !attach(near_verb, generation) {
        aterm_log::warn!("fabric bridge verb lane could not be served");
    }
    if !attach(near_push, generation) {
        aterm_log::warn!("fabric bridge push lane could not be served");
    }
    Ok(Launched { child, near })
}

/// aterm's environment minus [`aterm_types::domain::is_ai_env_key`]: what the bridge child gets.
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

    /// A config with `[fabric] command = <cmd>`, the LINE form every existing
    /// config spells.
    fn with_command(cmd: Option<&str>) -> Config {
        with_argv(cmd.map(aterm_types::fabric_command::split_line))
    }

    /// A config with `[fabric] command = [<argv>…]`, the ARGV form.
    fn with_argv(argv: Option<Vec<String>>) -> Config {
        Config {
            fabric: Some(FabricConfig {
                command: argv.map(FabricCommand),
                hold_on_bridge_lost: None,
            }),
            ..Config::default()
        }
    }

    /// `text` through the parser every launch and the config service use.
    fn parsed(text: &str) -> Result<Config, String> {
        Config::parse(text).map_err(|e| e.to_string())
    }

    /// A tiny real bridge: a program that exits at once. Enough to pass the
    /// pre-flight and prove a supervisor thread started; the thread outlives
    /// the test and only re-spawns `true` under the back-off.
    fn true_program() -> String {
        ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("a `true` binary")
            .to_string()
    }

    /// The pre-flight resolves `argv[0]` the way the spawn will: a path is a path; a bare name
    /// walks `PATH` like `execvp`, skipping a non-executable match; an unset `PATH` is the
    /// libc default.
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

        let dir = std::env::temp_dir().join(format!("aterm-fabric-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("true"), b"not a program\n").unwrap();
        std::fs::set_permissions(
            dir.join("true"),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644),
        )
        .unwrap();
        let mut path = dir.clone().into_os_string();
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The lanes are retired when the bridge exits though a leftover process holds both far
    /// ends: once the bridge is reaped, the near verb end reads the end-of-file the
    /// fail-closed halt waits on.
    #[test]
    fn the_lanes_are_retired_when_the_bridge_exits_whoever_else_holds_a_far_end() {
        use std::io::{BufRead as _, Read as _};
        use std::os::unix::net::UnixStream;
        let served = std::sync::Arc::new(std::sync::Mutex::new(Vec::<UnixStream>::new()));
        let recorder = {
            let served = std::sync::Arc::clone(&served);
            move |near: UnixStream, _generation| {
                served.lock().unwrap().push(near);
                true
            }
        };
        let argv: Vec<String> = ["/bin/sh", "-c", "sleep 300 & echo $! >&4; exit 0"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let launched = launch_serving(&argv, recorder).expect("launch the stand-in bridge");
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
        let status = launched.wait_and_retire().expect("reap the bridge");
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

    /// A lane the bridge half-closed is retired too. The stand-in (`perl`: a shell cannot
    /// `shutdown`) shuts its verb lane's write half and exits, leaving a holder of both far
    /// ends; once it is reaped, a write to the lane must fail.
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
            move |near: UnixStream, _generation| {
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
        let launched = launch_serving(&argv, recorder).expect("launch the stand-in bridge");
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
        let status = launched.wait_and_retire().expect("reap the bridge");
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

    /// DEFAULT OFF. An instance with no `[fabric]` table and no env var launches
    /// nothing — the secure default, and the reason a fabric-unaware machine
    /// pays nothing for the feature existing.
    #[test]
    fn the_fabric_is_off_unless_it_is_configured() {
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

    /// `[fabric] command` IS A LINE OR AN ARGV, read through the parser every
    /// launch uses.
    #[test]
    fn the_argv_form_keeps_a_spaced_program_whole_and_the_line_still_splits() {
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

    /// A MALFORMED ARGV FAILS THE PARSE, NAMING WHY — an empty element, a NUL,
    /// an element that is not a string, a value that is neither form — the
    /// way every other config type error does, so no bridge is started from
    /// an argv nobody wrote.
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

    /// The command never reaches a shell: a metacharacter is one more argument, not a second
    /// command.
    #[test]
    fn a_metacharacter_is_an_argument_and_not_a_second_command() {
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
    /// A child that spawns and exits at once (a bad cap file, an unreachable broker) escalates
    /// its back-off; only one that ran [`RELAUNCH_HEALTHY`] resets it.
    #[test]
    fn a_bridge_that_exits_immediately_escalates_its_backoff() {
        let instant = Duration::from_millis(5);

        let mut backoff = RELAUNCH_MIN;
        for _ in 0..3 {
            backoff = next_backoff(backoff, Some(instant));
        }
        assert_eq!(backoff, RELAUNCH_MIN * 8, "an immediate exit must escalate");

        for _ in 0..20 {
            backoff = next_backoff(backoff, Some(instant));
        }
        assert_eq!(backoff, RELAUNCH_MAX);

        assert_eq!(next_backoff(RELAUNCH_MIN, None), RELAUNCH_MIN * 2);

        assert_eq!(
            next_backoff(RELAUNCH_MAX, Some(RELAUNCH_HEALTHY)),
            RELAUNCH_MIN
        );
        assert_eq!(
            next_backoff(RELAUNCH_MAX, Some(Duration::from_secs(3600))),
            RELAUNCH_MIN
        );

        assert_eq!(
            next_backoff(
                RELAUNCH_MIN,
                Some(RELAUNCH_HEALTHY - Duration::from_millis(1))
            ),
            RELAUNCH_MIN * 2
        );
    }

    /// The bridge child inherits none of aterm's identity, edge secrets, outer fabric
    /// credentials or AI-tool variables.
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

    /// A non-UTF-8 key with a deny-listed prefix is still denied (a prefix matches leading
    /// bytes); one with no such prefix passes.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_key_with_a_denied_prefix_does_not_reach_the_bridge_child() {
        use std::os::unix::ffi::OsStringExt as _;
        let raw = |b: &[u8]| std::ffi::OsString::from_vec(b.to_vec());
        let inherited = vec![
            (raw(b"ANTHROPIC_\xffX"), raw(b"sk-x")),
            (raw(b"CLAUDE\xfe"), raw(b"1")),
            (raw(b"PLAIN_\xffKEY"), raw(b"kept")),
        ];
        let kept: Vec<Vec<u8>> = filter_child_env(inherited.into_iter())
            .into_iter()
            .map(|(k, _)| k.into_vec())
            .collect();
        assert_eq!(kept, vec![b"PLAIN_\xffKEY".to_vec()]);
    }

    /// The supervisor arms once per process and the latch is one-way; an empty argv arms
    /// nothing.
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
            let dir =
                std::env::temp_dir().join(format!("aterm-fabric-preflight-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let plain = dir.join("not-a-program");
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
            let _ = std::fs::remove_dir_all(&dir);
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

    /// The startup path records the configured command (`None` when there is none, which a
    /// bare `fabric attach` answers `ERR fabric no command` from) and arms through [`arm`].
    #[test]
    fn the_startup_path_records_the_configured_command_and_arms_through_the_one_seam() {
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

    /// A bridge armed at runtime with `fabric attach` rides the handoff manifest and is armed
    /// again by a successor with no command configured.
    #[test]
    fn a_runtime_attached_bridge_is_armed_again_after_an_update() {
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

    /// **A QUIT'S BYE IS SAID TO EVERY WATCHER, A HANDOFF'S IS NOT WAITED FOR,
    /// AND A BRIDGE'S TAKING OF A QUIT'S IS.** With no bridge attached the word
    /// is still journalled — a `subscribe … sessions` watcher reads it — and no
    /// bridge took it, with nothing to wait for (no watcher of this store).
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
                        std::thread::yield_now();
                    }
                    crate::fabric::cmd_link(&store, "bye quit", Some(generation))
                })
            };
            assert!(bye(&store, Bye::Quit, aterm_spec::wait::MUST_HAPPEN));
            assert_eq!(bridge.join().expect("the bridge's answer"), "OK\n");
        });
    }

    /// **A RETIRED SUPERVISOR RELAUNCHES NOTHING.** Once a quit has told its
    /// bridge `bye quit` the supervisor returns instead of launching, so no
    /// bridge comes back to re-advertise the sessions the quit withdrew.
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

    /// The QA seam's word: `handoff` alone makes a quit say `bye handoff`.
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
