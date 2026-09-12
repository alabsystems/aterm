// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BRIDGE LAUNCHER — the half of `Scope::Bridge` that makes the authority
//! real. A2 built the scope, the seam and the fail-closed halt; this starts the
//! process that holds them.
//!
//! ```text
//! [fabric]
//! command = "aterm-link serve --fleet lab --broker /tmp/f.sock --cap-file ~/.config/aterm/fabric.cap"
//! ```
//!
//! DEFAULT OFF, exactly like the embedded operator (`ATERM_OPERATOR=1`,
//! `operator_host.rs:66-67`), and the env var `ATERM_FABRIC_COMMAND` overrides
//! the config key the way every other launch knob in this process does
//! (`flag > env > config > default`).
//!
//! ## What the child gets, and what it does not
//!
//! Two `socketpair(AF_UNIX)`s. The child inherits the far ends at fds 3 and 4
//! ([`aterm_uds::spawnfd`]); this process serves the near ends through its
//! ordinary request loop with [`crate::control::Scope::Bridge`] PRE-RESOLVED, so
//! no `AUTH` line is read and no token exists. **The authority is the
//! descriptor.** A second process cannot present it, an agent inside a session
//! cannot ask for it, and there is no file to steal — the only way to be the
//! bridge is to be the process this function spawned.
//!
//! The child gets NO token and NO socket path, and its environment is aterm's own
//! with [`aterm_types::domain::is_ai_env_var`] applied — the SAME deny list the
//! PTY spawn seam runs (`aterm-pty`'s `build_child_env`), applied HERE because
//! that one is a different seam and protects a different child. So aterm's
//! identity (`ATERM_SESSION_ID`, `ATERM_LAUNCH_NONCE`), its control-socket path,
//! the `ATERM_EDGE_READ`/`WRITE`/`SIGNAL` bearer secrets and `ATERM_EDGE_TOKENS`
//! path, and the fabric credentials of an OUTER instance — `ATERM_LINK_BROKER`,
//! `ATERM_LINK_CAP_FILE`, `ATERM_LINK_FLEET` and `ATERM_FABRIC_COMMAND`, the four
//! names `ENV_DENY_VARS` lists — do not reach it. Those four NAMES, and NOT the
//! whole-prefix glob this header used to claim: `ENV_DENY_PREFIXES` carries no
//! `ATERM_LINK_` rule, and the two other variables under that prefix are
//! inherited ON PURPOSE — `ATERM_LINK_FAULT` and `ATERM_LINK_NOTIFY_FAULT` are
//! the e2e harness's crash switches, which
//! `the_bridge_child_inherits_no_identity_no_socket_and_no_edge_secret` REQUIRES
//! to survive the filter. A future variable under that prefix that is a
//! CREDENTIAL rather than a fault switch needs its own deny-list entry of its
//! own name; the glob would have said it was already covered. Everything else —
//! `PATH`, `HOME`, `XDG_STATE_HOME` (which `aterm-link` needs for its state dir),
//! locale — is inherited, and that is stated rather than promised away: the
//! header used to say the child got "none of aterm's own environment beyond what
//! it needs" while `Command` inherited the parent block in full, which put the
//! per-op edge-token secrets that audit finding F1 moved OUT of env into a
//! `/proc`-readable environment block belonging to the one process that holds
//! `Scope::Bridge`.
//!
//! The child's fabric credentials remain its own business (`--cap-file`).
//!
//! ## Why it is supervised, and why that is not a way around the halt
//!
//! §11.2: "the instance relaunches the child with back-off". When either
//! descriptor closes, [`crate::fabric::bridge_lost`] holds every session the
//! bridge ever governed — that lands BEFORE any relaunch, from the
//! `BridgeLostGuard` on the serving thread, and a relaunched bridge lifts what it
//! wants lifted with `hold off`. So killing the bridge is strictly worse for an
//! agent than leaving it alone, supervision or not: the halt does not wait for
//! the supervisor's opinion.

use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The env override for `[fabric] command`. Precedence is env > config, the same
/// order every other launch knob in this process follows.
const FABRIC_COMMAND_ENV: &str = "ATERM_FABRIC_COMMAND";

/// The back-off between relaunches, and its ceiling. A bridge that fails at
/// startup (a bad cap file, an unreachable broker) must not become a fork bomb;
/// a bridge that was killed once must come back promptly.
const RELAUNCH_MIN: Duration = Duration::from_millis(200);
const RELAUNCH_MAX: Duration = Duration::from_secs(30);

/// How long a child must RUN before its exit counts as "it was working, then it
/// stopped" rather than "it cannot start".
///
/// THE TEST IS DURATION, NOT SPAWN SUCCESS, and that distinction is the whole
/// point of the ceiling above. `supervise` used to reset the back-off on every
/// `Ok(child)` — i.e. whenever `Command::spawn` succeeded — and escalate only on
/// `Err`, which is the binary being MISSING. The failure the doc actually names
/// (a bad cap file, an unreachable broker) spawns fine and exits within
/// milliseconds, so it reset the floor every time: five process spawns a second,
/// two socketpairs and two aterm threads each, `bridge_lost` on every cycle, for
/// the life of the instance, and `RELAUNCH_MAX` unreachable.
const RELAUNCH_HEALTHY: Duration = Duration::from_secs(5);

/// The configured bridge command, or `None` when the fabric is off.
///
/// Split on whitespace, not through a shell: a command that needs a shell needs
/// `sh -c` spelled out, and going through one implicitly would make every
/// character of this string an injection surface for whoever can write the
/// config.
#[must_use]
pub(crate) fn configured_command(config: &crate::app_config::Config) -> Option<Vec<String>> {
    let raw = std::env::var(FABRIC_COMMAND_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            config
                .fabric
                .as_ref()
                .and_then(|f| f.command.clone())
                .filter(|s| !s.trim().is_empty())
        })?;
    let argv: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
    (!argv.is_empty()).then_some(argv)
}

/// THE SUPERVISOR RECORD — one per process, and its `armed` half is ONE-WAY.
///
/// scope-waiver: the phrase describes the `static SUPERVISOR` below, and a
/// `static` is one per process by the language rather than by a discipline an
/// instance count could check — there is no field whose replication would
/// falsify it. The half that IS falsifiable is behavioural (one supervisor
/// THREAD, not one record) and it is enforced in code, not in this prose:
/// [`arm`] checks `armed` and sets it under ONE hold of that static's lock,
/// and `supervisor` is registered in the lock-order census's `GUARD_HELPERS`
/// so its callers' invisible holds are placed in the graph.
///
/// [`supervise`] loops forever with no stop handle, so a second supervisor would
/// be a second relaunch loop over the same instance: two bridge children racing
/// to be served against one `BRIDGE_CONTEXT`, each one's exit halting every
/// session the other governed, and no way to take either back. [`arm`] therefore
/// checks and sets under ONE lock, and `fabric attach` on an armed instance is
/// refused rather than stacked.
///
/// `configured` is what the instance was LAUNCHED with — `[fabric] command` or
/// `$ATERM_FABRIC_COMMAND` as [`spawn_supervisor`] read them at startup —
/// recorded so a bare `fabric attach` and `fabric status` can name it without
/// the control thread re-reading the config file (the process-wide config
/// service owns that read; see the note on `control::spawn`'s `network_config`).
/// A command added to the config AFTER launch is therefore not seen here: the
/// verb takes it as `fabric attach <argv...>`, which is the case the verb exists
/// for.
struct Supervisor {
    /// The argv the supervisor thread was armed with — `Some` exactly once per
    /// process, and never `None` again.
    armed: Option<Vec<String>>,
    /// The command the instance was launched with, if any.
    configured: Option<Vec<String>>,
}

static SUPERVISOR: Mutex<Supervisor> = Mutex::new(Supervisor {
    armed: None,
    configured: None,
});

fn supervisor() -> std::sync::MutexGuard<'static, Supervisor> {
    SUPERVISOR.lock().unwrap_or_else(|p| p.into_inner())
}

/// Why [`arm`] did not start a supervisor.
#[derive(Debug)]
pub(crate) enum ArmError {
    /// The argv was empty. Nothing is armed.
    NoCommand,
    /// `argv[0]` cannot be executed — see [`preflight`] for `reason`. Refused
    /// BEFORE the latch closes: nothing is armed, and an `attach` with the
    /// corrected command is allowed.
    NotExecutable {
        program: String,
        reason: &'static str,
    },
    /// One is already running, with THIS argv. The request's argv was NOT
    /// applied — the latch is one-way, which is the whole reason this is an
    /// error and not an `OK already`: a caller who passed a different command
    /// must learn that it did not take.
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

/// Launch and SUPERVISE the bridge, if one is configured. Returns whether a
/// supervisor thread started — `false` means the fabric is off or a thread could
/// not be spawned, never a half-attached bridge.
///
/// Called once, from the control server, after the socket is bound and
/// `BRIDGE_CONTEXT` is published: a bridge that attached earlier would find a
/// half-built process. It is ALSO the one place the configured command is
/// recorded for `fabric status` and a bare `fabric attach` — so an instance
/// launched with no `[fabric] command` records `None`, which is what a later
/// bare `attach` answers `ERR fabric no command` from.
pub(crate) fn spawn_supervisor(config: &crate::app_config::Config) -> bool {
    let configured = configured_command(config);
    supervisor().configured = configured.clone();
    let Some(argv) = configured else {
        return false;
    };
    match arm(argv) {
        Ok(()) => true,
        // Startup runs once and before the socket serves a request, so this arm
        // cannot lose to a `fabric attach`; if it ever did, a supervisor IS
        // running, which is what the `bool` answers.
        Err(ArmError::AlreadySupervised(_)) => true,
        // The configured command cannot run. It stays RECORDED (`fabric status`
        // names it; a bare `fabric attach` re-tries it), it is NOT armed — the
        // supervisor used to retry a typo for ever from here — and the latch
        // stays open, so `fabric attach <corrected...>` is the remedy, not a
        // relaunch.
        Err(ArmError::NotExecutable { program, reason }) => {
            aterm_log::warn!(
                "fabric bridge supervisor not started: `{program}` is {reason}; \
                 `aterm ctl fabric attach <command...>` arms it once the command is fixed"
            );
            false
        }
        Err(e) => {
            aterm_log::warn!("fabric bridge supervisor could not start: {e:?}");
            false
        }
    }
}

/// Arm the supervisor with `argv` — the ONE seam that starts a `supervise`
/// thread, shared by the startup path ([`spawn_supervisor`]) and the running
/// instance's `fabric attach`. Exactly once per process: the check and the set
/// happen under the record's lock, so two concurrent attaches cannot both
/// spawn.
///
/// `argv` is executed directly, never through a shell — the caller has already
/// split it on whitespace exactly as [`configured_command`] splits the config
/// string, so a metacharacter is one more argument and not a second command.
pub(crate) fn arm(argv: Vec<String>) -> Result<(), ArmError> {
    if argv.is_empty() {
        return Err(ArmError::NoCommand);
    }
    let mut guard = supervisor();
    if let Some(running) = &guard.armed {
        return Err(ArmError::AlreadySupervised(running.clone()));
    }
    // BEFORE THE LATCH CLOSES, and under its lock so the check-and-set stays
    // one step: a program that cannot run arms nothing, and the slot stays
    // open for the corrected command. (After the `armed` check on purpose: a
    // caller on an armed instance is owed "already supervised", the answer to
    // what it asked, whatever it typed.)
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
    // TELL THE ENDPOINT A BRIDGE IS COMING. `fabric=absent` means both "the
    // first bridge has not attached yet" and "no bridge will ever attach",
    // and a `post --wait` parked in those two states is owed opposite advice:
    // wait, versus stop waiting — this instance has no fabric. Recorded here
    // rather than inferred from the config at the far end, because THIS is
    // the one place that knows a supervisor really started (a config with a
    // `[fabric] command` whose thread failed to spawn is the `Err` case), and
    // it is the same place for `fabric attach` as for startup, so the two
    // cannot flip the latch at different moments.
    crate::fabric::note_bridge_supervised();
    Ok(())
}

/// PRE-FLIGHT: whether `argv[0]` can be executed at all, checked BEFORE the
/// one-way latch closes. `Err` is the `reason=` token the verb reports.
///
/// Measured before this existed: `fabric attach /typo` answered `OK attached`,
/// the supervisor retried the bad argv for ever (back-off to [`RELAUNCH_MAX`]),
/// and the instance's only attach slot was wedged for its lifetime — the one
/// remedy being the relaunch the verb exists to avoid. The same typo in
/// `[fabric] command` did the same from startup.
///
/// The name is resolved the way `Command::spawn` will resolve it: a word with a
/// `/` is a path (relative to this process's cwd, which the child inherits); a
/// bare word is searched on `PATH` — THIS process's `PATH`, which is the child's
/// too, since [`filter_child_env`] passes it through — the way `execvp` searches
/// it, so a candidate that exists but is not executable is skipped, not
/// reported, and an unset `PATH` falls back to `execvp`'s own default. What is
/// checked is exactly what a spawn refuses synchronously: existence (`ENOENT`),
/// and the execute bit on a regular file (`EACCES`). A program that passes and
/// still fails to start — a script whose interpreter is missing, an ACL the mode
/// bits do not show — is the supervisor's to retry, logged, which is what every
/// attach did before this check; a program this refuses is one no retry would
/// ever have started.
///
/// Pure over `(program, path)` so the resolution is tested without mutating the
/// process environment.
fn preflight(program: &str, path: Option<&std::ffi::OsStr>) -> Result<(), &'static str> {
    if program.contains('/') {
        return match std::fs::metadata(program) {
            Err(_) => Err("not-found"),
            Ok(md) if !md.is_file() => Err("not-a-file"),
            Ok(md) if !is_executable(&md) => Err("no-exec-bit"),
            Ok(_) => Ok(()),
        };
    }
    // `_PATH_DEFPATH`: what `execvp` searches when `PATH` is unset. macOS spells
    // it `/usr/bin:/bin`, glibc `/bin:/usr/bin`; the same two directories.
    let default = std::ffi::OsStr::new("/usr/bin:/bin");
    let found = std::env::split_paths(path.unwrap_or(default)).any(|dir| {
        std::fs::metadata(dir.join(program)).is_ok_and(|md| md.is_file() && is_executable(&md))
    });
    if found { Ok(()) } else { Err("not-on-path") }
}

/// Any execute bit. What `execve` requires of the mode; the uid-specific
/// refinement (`access(2)`) is deliberately not attempted here — a wrong pass
/// falls back to the supervisor's logged retry, a wrong refusal would block a
/// program that runs.
fn is_executable(md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o111 != 0
}

/// Clear the record so a test section starts from "nothing armed, nothing
/// configured". Called from `fabric::with_link_reset`, which serializes every
/// test that touches the process-wide link — the `armed` half is a one-way
/// latch in production and only a test may re-open it. A supervisor thread a
/// previous section armed keeps running; it cannot reach the link (no
/// `BRIDGE_CONTEXT` is published in a test process, so `attach_fabric_bridge`
/// serves nothing), it only retries its argv with the back-off above.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    let mut g = supervisor();
    g.armed = None;
    g.configured = None;
}

/// Launch, wait, back off, launch again — forever, because the fabric is a
/// standing service and this process is its supervisor.
fn supervise(argv: &[String]) {
    let mut backoff = RELAUNCH_MIN;
    loop {
        match launch_once(argv) {
            Ok(mut child) => {
                aterm_log::info!("fabric bridge started: {}", argv.join(" "));
                let started = Instant::now();
                let status = child.wait();
                let ran = started.elapsed();
                aterm_log::warn!(
                    "fabric bridge exited after {ran:?} ({status:?}); every session it governed \
                     is held"
                );
                backoff = next_backoff(backoff, Some(ran));
            }
            Err(e) => {
                backoff = next_backoff(backoff, None);
                aterm_log::warn!("fabric bridge could not start ({e}); retrying in {backoff:?}");
            }
        }
        std::thread::sleep(backoff);
    }
}

/// The next back-off, given the current one and how long the child RAN — `None`
/// when `Command::spawn` itself failed and no child ever existed.
///
/// A bridge that ran for [`RELAUNCH_HEALTHY`] is not a configuration error, so
/// its restart starts from the floor again. Everything else — a spawn failure, or
/// a child that spawned and exited immediately — DOUBLES, which is the only way
/// the ceiling is ever reached.
///
/// Extracted as a pure function so the escalation is unit-testable without a
/// process, a clock or a sleep: `supervise` itself never returns.
fn next_backoff(current: Duration, ran: Option<Duration>) -> Duration {
    match ran {
        Some(ran) if ran >= RELAUNCH_HEALTHY => RELAUNCH_MIN,
        _ => (current * 2).min(RELAUNCH_MAX),
    }
}

/// One launch: two socketpairs, the child, and both near ends attached.
fn launch_once(argv: &[String]) -> std::io::Result<std::process::Child> {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    let (near_verb, far_verb) = UnixStream::pair()?;
    let (near_push, far_push) = UnixStream::pair()?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        // THE ENVIRONMENT IS FILTERED, not inherited whole. `Command` inherits the
        // parent block by default, which handed the one process holding
        // `Scope::Bridge` aterm's own identity, its control-socket path, and — when
        // this aterm is itself nested — the OUTER instance's `ATERM_EDGE_*` bearer
        // secrets, the very values audit finding F1 moved out of env into a 0600
        // file. `env_clear` + the deny list is the same rule the PTY spawn seam
        // applies to a child shell (`aterm-pty`'s `build_child_env`), applied at
        // this seam because that one does not cover it.
        .env_clear()
        .envs(filter_child_env(std::env::vars_os()))
        // The child's stdin is NOTHING. Its two real channels are fds 3 and 4,
        // and leaving stdin attached to aterm's own would let a bridge read
        // whatever aterm was launched with.
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let child = aterm_uds::spawnfd::spawn_with_two_fds(
        cmd,
        OwnedFd::from(far_verb),
        OwnedFd::from(far_push),
    )?;

    // ATTACH BOTH, and only after the spawn succeeded: a near end served against
    // a child that never started would report `fabric=connected` for a bridge
    // that does not exist. Either end closing fires the fail-closed halt, which
    // is exactly §11.2's "when either fd closes".
    // ONE GENERATION PER LAUNCH, shared by both lanes: either lane closing still
    // reports the link lost, and neither lane's late-unwinding guard can report a
    // LATER launch's live bridge disconnected. See
    // [`crate::fabric::BridgeGeneration`].
    let generation = crate::fabric::next_bridge_generation();
    if !crate::control::attach_fabric_bridge(near_verb, generation) {
        aterm_log::warn!("fabric bridge verb lane could not be served");
    }
    if !crate::control::attach_fabric_bridge(near_push, generation) {
        aterm_log::warn!("fabric bridge push lane could not be served");
    }
    Ok(child)
}

/// aterm's environment with the deny list applied — what the bridge child gets.
///
/// A DENY LIST AND NOT AN ALLOW LIST, on purpose: `aterm-link serve` legitimately
/// needs `HOME`/`XDG_STATE_HOME` for its state directory, `PATH`, and the locale,
/// and an allow list would have to grow every time it learns a new one — silently
/// breaking the bridge each time it did not. The deny list is the canonical
/// [`aterm_types::domain::is_ai_env_var`], so a var added there is stripped here
/// too, with no second copy to drift.
///
/// Non-UTF-8 keys pass through, which is safe because every deny-listed name is
/// ASCII — the same reasoning `aterm-pty`'s `is_denied_env_key` records.
/// PURE IN ITS INPUT, so the wiring is unit-tested without mutating the
/// process-global environment — the same shape (and for the same reason) as
/// `aterm-pty`'s `build_child_env`.
fn filter_child_env(
    inherited: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    inherited
        .filter(|(k, _)| {
            k.to_str()
                .is_none_or(|k| !aterm_types::domain::is_ai_env_var(k))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::{Config, FabricConfig};

    /// A config with `[fabric] command = <cmd>`.
    fn with_command(cmd: Option<&str>) -> Config {
        Config {
            fabric: Some(FabricConfig {
                command: cmd.map(str::to_string),
            }),
            ..Config::default()
        }
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

    /// THE PRE-FLIGHT RESOLVES `argv[0]` THE WAY THE SPAWN WILL. A path is a
    /// path; a bare name walks `PATH` like `execvp` — a directory that holds a
    /// non-executable file of that name is skipped, not reported — and an unset
    /// `PATH` is the libc default, not "nowhere". Pure over the `PATH` value,
    /// so nothing here touches the process environment.
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

        // A bare name: found on the given PATH, not found on a PATH without it,
        // and found on the default when PATH is unset (`true` lives there).
        assert_eq!(
            preflight("true", Some(OsStr::new("/nonexistent:/usr/bin:/bin"))),
            Ok(())
        );
        assert_eq!(
            preflight("true", Some(OsStr::new("/nonexistent"))),
            Err("not-on-path")
        );
        assert_eq!(preflight("true", None), Ok(()));

        // execvp semantics: the first PATH entry holds a `true` that is NOT
        // executable; the search moves on to the one that is.
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

    /// DEFAULT OFF. An instance with no `[fabric]` table and no env var launches
    /// nothing — the same secure default the embedded operator keeps, and the
    /// reason a fabric-unaware machine pays nothing for the feature existing.
    #[test]
    fn the_fabric_is_off_unless_it_is_configured() {
        aterm_log::env::scoped_unset(FABRIC_COMMAND_ENV, || {
            assert_eq!(configured_command(&Config::default()), None);
            assert_eq!(configured_command(&with_command(None)), None);
            assert_eq!(
                configured_command(&with_command(Some("   "))),
                None,
                "a blank command is OFF, not an empty argv"
            );
        });
    }

    /// The env var OVERRIDES the config key — `env > config`, the precedence
    /// every other launch knob in this process follows.
    #[test]
    fn the_env_var_wins_over_the_config_key() {
        let cfg = with_command(Some("from-config --flag"));
        aterm_log::env::scoped(FABRIC_COMMAND_ENV, "aterm-link serve --fleet lab", || {
            assert_eq!(
                configured_command(&cfg),
                Some(vec![
                    "aterm-link".to_string(),
                    "serve".to_string(),
                    "--fleet".to_string(),
                    "lab".to_string(),
                ])
            );
        });
        aterm_log::env::scoped_unset(FABRIC_COMMAND_ENV, || {
            assert_eq!(
                configured_command(&cfg),
                Some(vec!["from-config".to_string(), "--flag".to_string()])
            );
        });
    }

    /// THE COMMAND NEVER REACHES A SHELL. It is split on whitespace and executed
    /// directly, so a metacharacter is one more ARGUMENT and not a second
    /// command — otherwise every byte of this config key would be an injection
    /// surface for whoever can write the file, and the config file is exactly
    /// what a prompt-injected agent with Owner scope would reach for.
    #[test]
    fn a_metacharacter_is_an_argument_and_not_a_second_command() {
        let cfg = Config::default();
        aterm_log::env::scoped(FABRIC_COMMAND_ENV, "bridge ; rm -rf /", || {
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
    /// A CHILD THAT SPAWNS AND EXITS AT ONCE ESCALATES — the failure
    /// `RELAUNCH_MIN`/`RELAUNCH_MAX`'s own doc names ("a bad cap file, an
    /// unreachable broker").
    ///
    /// `supervise` used to reset the back-off on every `Ok(child)`, i.e. whenever
    /// `Command::spawn` SUCCEEDED, and escalate only on `Err` — which is the
    /// binary being missing. A bad `--cap-file` makes `Bridge::new` fail,
    /// `aterm-link serve` print and exit 1 in milliseconds, and `launch_once`
    /// still return `Ok`: five spawns a second, two socketpairs and two aterm
    /// threads each, `bridge_lost` on every cycle, for the life of the instance,
    /// with `RELAUNCH_MAX` unreachable.
    #[test]
    fn a_bridge_that_exits_immediately_escalates_its_backoff() {
        let instant = Duration::from_millis(5);

        // Spawned fine, ran for 5 ms, three times over: doubling, not resetting.
        let mut backoff = RELAUNCH_MIN;
        for _ in 0..3 {
            backoff = next_backoff(backoff, Some(instant));
        }
        assert_eq!(backoff, RELAUNCH_MIN * 8, "an immediate exit must escalate");

        // And it reaches the ceiling rather than running away.
        for _ in 0..20 {
            backoff = next_backoff(backoff, Some(instant));
        }
        assert_eq!(backoff, RELAUNCH_MAX);

        // A spawn failure escalates exactly as it always did.
        assert_eq!(next_backoff(RELAUNCH_MIN, None), RELAUNCH_MIN * 2);

        // A bridge that RAN is not a configuration error: back to the floor, so a
        // bridge somebody killed comes back promptly.
        assert_eq!(
            next_backoff(RELAUNCH_MAX, Some(RELAUNCH_HEALTHY)),
            RELAUNCH_MIN
        );
        assert_eq!(
            next_backoff(RELAUNCH_MAX, Some(Duration::from_secs(3600))),
            RELAUNCH_MIN
        );

        // The boundary, stated: one tick short of healthy is still "cannot start".
        assert_eq!(
            next_backoff(
                RELAUNCH_MIN,
                Some(RELAUNCH_HEALTHY - Duration::from_millis(1))
            ),
            RELAUNCH_MIN * 2
        );
    }

    /// THE BRIDGE CHILD DOES NOT INHERIT ATERM'S SECRETS.
    ///
    /// `Command` inherits the parent block by default and this seam applied no
    /// filter, so the one process holding `Scope::Bridge` also held — in a
    /// `/proc`-readable environment block — aterm's own identity, its
    /// control-socket path, and, when this aterm is itself nested, the OUTER
    /// instance's `ATERM_EDGE_READ`/`WRITE`/`SIGNAL` bearer secrets: the values
    /// audit finding F1 deliberately moved out of env into a 0600 file. The module
    /// header claimed the child got "none of aterm's own environment beyond what
    /// it needs" while it got all of it.
    #[test]
    fn the_bridge_child_inherits_no_identity_no_socket_and_no_edge_secret() {
        let pair = |k: &str, v: &str| (std::ffi::OsString::from(k), std::ffi::OsString::from(v));
        let inherited = vec![
            pair("PATH", "/usr/bin"),
            pair("HOME", "/home/a"),
            pair("XDG_STATE_HOME", "/home/a/.local/state"),
            pair("ATERM_LINK_FAULT", "kill-after-deliver"),
            pair("ATERM_EDGE_READ", "0011"),
            pair("ATERM_EDGE_WRITE", "2233"),
            pair("ATERM_EDGE_SIGNAL", "4455"),
            pair("ATERM_EDGE_TOKENS", "/run/aterm/edge.tok"),
            pair("ATERM_CONTROL_SOCK", "/run/aterm/ctl.sock"),
            pair("ATERM_SESSION_ID", "s-0123456789abcdef0123"),
            pair("ATERM_LAUNCH_NONCE", "0".repeat(32).as_str()),
            pair("ATERM_LINK_CAP_FILE", "/etc/aterm/outer.cap"),
            pair("ATERM_FABRIC_COMMAND", "aterm-link serve --fleet outer"),
            pair("ANTHROPIC_API_KEY", "sk-x"),
        ];
        let kept = filter_child_env(inherited.into_iter());
        let has = |k: &str| kept.iter().any(|(n, _)| n == std::ffi::OsStr::new(k));

        for secret in [
            "ATERM_EDGE_READ",
            "ATERM_EDGE_WRITE",
            "ATERM_EDGE_SIGNAL",
            "ATERM_EDGE_TOKENS",
            "ATERM_CONTROL_SOCK",
            "ATERM_SESSION_ID",
            "ATERM_LAUNCH_NONCE",
            "ATERM_LINK_CAP_FILE",
            "ATERM_FABRIC_COMMAND",
            "ANTHROPIC_API_KEY",
        ] {
            assert!(!has(secret), "the bridge child must not inherit {secret}");
        }

        // A DENY LIST, NOT AN ALLOW LIST. `aterm-link serve` needs `HOME` and
        // `XDG_STATE_HOME` for its state directory and `PATH` to be executable at
        // all, and the e2e harness arms `ATERM_LINK_FAULT` on `aterm-gui` and
        // relies on the CHILD inheriting it (`aterm-link/tests/harness/mod.rs`).
        // An allow list would have broken every one of those silently.
        for keeper in ["PATH", "HOME", "XDG_STATE_HOME", "ATERM_LINK_FAULT"] {
            assert!(has(keeper), "the bridge child still needs {keeper}");
        }
    }

    /// The filter is WIRED, not merely available: `launch_once` clears the child's
    /// environment and rebuilds it through [`filter_child_env`]. A pure unit test
    /// of the filter proves nothing if the spawn does not call it.
    #[test]
    fn the_launch_seam_actually_applies_the_filter() {
        let src = include_str!("fabric_launch.rs");
        let production = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .map(|(p, _)| p)
            .expect("fabric_launch.rs has a tests module");
        assert!(
            production.contains(".env_clear()"),
            "the child's environment must be cleared before it is rebuilt"
        );
        assert!(
            production.contains(".envs(filter_child_env(std::env::vars_os()))"),
            "the child's environment must come through the deny-listed filter"
        );
    }

    /// THE HEADER NAMES THE VARIABLES THAT ARE ACTUALLY DENIED.
    ///
    /// It used to claim the glob `ATERM_LINK_*`, and there is no `ATERM_LINK_`
    /// prefix rule: `ENV_DENY_PREFIXES` does not contain one and `ENV_DENY_VARS`
    /// denies four EXACT names. The two other variables under that prefix —
    /// `ATERM_LINK_FAULT` and `ATERM_LINK_NOTIFY_FAULT` — pass straight through,
    /// and the sibling test above REQUIRES that they do. A reader who took the
    /// glob at face value would add the next `ATERM_LINK_*` credential without a
    /// deny-list entry, because the header said the prefix was already covered.
    /// aterm ships no evidence manifest, so this header IS the claim.
    #[test]
    fn the_header_names_the_denied_variables_rather_than_a_glob_it_does_not_enforce() {
        let src = include_str!("fabric_launch.rs");
        let header = src
            .split_once(
                "
use std::process::",
            )
            .map(|(h, _)| h)
            .expect("fabric_launch.rs keeps its module header");
        assert!(
            !header.contains("ATERM_LINK_*"),
            "the header must not claim a prefix rule the filter does not have"
        );
        for denied in [
            "ATERM_LINK_BROKER",
            "ATERM_LINK_CAP_FILE",
            "ATERM_LINK_FLEET",
            "ATERM_FABRIC_COMMAND",
        ] {
            assert!(header.contains(denied), "the header omits {denied}");
            assert!(
                aterm_types::domain::is_ai_env_var(denied),
                "{denied} is named as denied but the filter lets it through"
            );
        }
        // And the deliberate keeper is named as one, because a reader who does not
        // know it is deliberate will "fix" it.
        assert!(header.contains("ATERM_LINK_FAULT"));
        assert!(
            !aterm_types::domain::is_ai_env_var("ATERM_LINK_FAULT"),
            "the e2e harness relies on the child inheriting this"
        );
    }

    /// THE SUPERVISOR IS ARMED ONCE PER PROCESS, AND THE LATCH IS ONE-WAY.
    ///
    /// `supervise` loops forever with no stop handle, so a second `arm` would be
    /// a second relaunch loop over the same instance. The check-and-set is under
    /// one lock, the endpoint's `supervised` latch flips at the same moment, and
    /// an empty argv arms nothing — `launch_once` indexes `argv[0]`, and a
    /// thread panicking on it would have "armed" a supervisor that runs nothing.
    ///
    /// The argv names a program that does not exist, ON PURPOSE: `Command::spawn`
    /// fails at once, so the thread this leaks into the test process spawns no
    /// child and only sleeps out its back-off (to the 30 s ceiling).
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

            // THE PRE-FLIGHT: a program that cannot run is refused BEFORE the
            // latch closes — nothing armed, nothing flipped — so the slot is
            // not wedged by a typo. Measured before it existed: `/nonexistent`
            // was `Ok`, and the only remedy was a relaunch.
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
            // A regular file with no execute bit — the shape a path to a config
            // file, or a script never `chmod +x`ed, hits.
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

            // AND THE SLOT IS STILL OPEN: the corrected command arms.
            let argv = vec![true_program(), "serve".to_string()];
            assert!(
                arm(argv.clone()).is_ok(),
                "the refused attempts left the latch open"
            );
            assert!(crate::fabric::bridge_supervised(), "arming IS the latch");
            assert_eq!(status().armed.as_deref(), Some(argv.as_slice()));

            // The second arm is refused and names what is running — the
            // request's argv did NOT take, and a caller must be told so. The
            // latch check comes BEFORE the pre-flight (this argv would fail
            // it): "already supervised" is the answer to what was asked.
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

    /// THE STARTUP PATH RECORDS THE CONFIGURED COMMAND EVEN WHEN IT ARMS NOTHING.
    ///
    /// `fabric status` and a bare `fabric attach` read it, and an instance
    /// launched with no `[fabric] command` must record `None` — that is the
    /// `ERR fabric no command` a later bare attach answers from — while one
    /// launched WITH a command records it and arms through the same seam the
    /// verb uses, so the two cannot flip the endpoint's latch at different
    /// moments.
    #[test]
    fn the_startup_path_records_the_configured_command_and_arms_through_the_one_seam() {
        aterm_log::env::scoped_unset(FABRIC_COMMAND_ENV, || {
            crate::fabric::with_link_reset(|| {
                assert!(!spawn_supervisor(&Config::default()));
                let s = status();
                assert!(s.armed.is_none() && s.configured.is_none());
                assert!(!crate::fabric::bridge_supervised());
            });
            // A configured command that cannot run: RECORDED — `fabric status`
            // names it and a bare `attach` re-tries it — but NOT armed, where the
            // supervisor used to retry the typo for ever, and the latch says so
            // (`post` answers `no-bridge=1`, which is the truth).
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
}
