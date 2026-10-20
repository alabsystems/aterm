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
//! DEFAULT OFF. The config key is the one spelling in a shipped
//! aterm: `ATERM_FABRIC_COMMAND`, which used to override it, is a DEVELOPMENT
//! seam ([`aterm_types::dev_seam!`]) — the fabric world tests
//! (`crates/aterm/tests/fabric_world`) launch their debug instances with it,
//! and a release build reads none.
//!
//! ## What the child gets, and what it does not
//!
//! Two `socketpair(AF_UNIX)`s. The child inherits the far ends at fds 3 and 4
//! ([`aterm_uds::spawnfd`]) and no other descriptor above stdio — on Darwin the
//! launch is a `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`, so nothing it
//! does not name (Metal's unflagged shader-cache files, another connection's
//! socketpair end mid-`pair()`) can reach the child; this process serves the near ends through its
//! ordinary request loop with [`crate::control::Scope::Bridge`] PRE-RESOLVED, so
//! no `AUTH` line is read and no token exists. **The authority is the
//! descriptor.** A second process cannot present it and there is no file to
//! steal — the only way to be the bridge is to be the process this function
//! spawned. WHICH process that is, is chosen by the instance's CONFIGURATION
//! and nothing else: the `[fabric] command` it was launched with, or the one
//! its aterm.toml names when a running instance is armed with `fabric attach`
//! ([`command_on_disk`], read again at the attach — `aterm fabric on` writes
//! the file and then attaches). The verb is `Owner`-scope, and a client in a
//! session holds `Owner` too, so the verb may only arm, ONCE per process (the
//! latch), what the config already names: an argv of the caller's choosing
//! that the config does not name is refused (`ERR fabric not configured`). So
//! no token chooses the process that holds `Scope::Bridge`; whoever can write
//! aterm.toml can, which is aterm's same-uid boundary, as for the node cap.
//! (The 2026-10-08 ruling had let the Owner token choose it, on the ground that
//! Owner already reaches every local session; the 2026-10-15 audit, r4-17, held
//! that it left `deliver`'s attested `trust=human` rows and the fleet halt to
//! whichever process an injected agent named, and closed it.)
//!
//! The child gets NO token and NO socket path, and its environment is aterm's own
//! with [`aterm_types::domain::is_ai_env_key`] applied — the SAME deny list the
//! PTY spawn seam runs (`aterm-pty`'s `build_child_env`), applied HERE because
//! that one is a different seam and protects a different child. So aterm's
//! identity (`ATERM_SESSION_ID`, `ATERM_LAUNCH_NONCE`), its control-socket path,
//! its `ATERM_EDGE_TOKENS` path (and the retired `ATERM_EDGE_READ`/`WRITE`/`SIGNAL`
//! bearer secrets, should an older outer aterm have injected them), and the
//! fabric credentials of an OUTER instance — `ATERM_LINK_BROKER`,
//! `ATERM_LINK_CAP_FILE`, `ATERM_LINK_FLEET` and `ATERM_FABRIC_COMMAND`, the four
//! names `ENV_DENY_VARS` lists — do not reach it. Those four NAMES, and NOT the
//! whole-prefix glob this header used to claim: `ENV_DENY_PREFIXES` carries no
//! `ATERM_LINK_` rule, and the three other variables under that prefix are
//! inherited ON PURPOSE — `ATERM_LINK_FAULT` and `ATERM_LINK_NOTIFY_FAULT` are
//! the e2e harness's crash switches and `ATERM_LINK_WAIT_TALLY` the fabric
//! bench's count of the bridge's waits on the broker (development seams: only a
//! debug build's bridge reads them), which
//! `the_bridge_child_inherits_no_identity_no_socket_and_no_edge_secret` REQUIRES
//! to survive the filter. A future variable under that prefix that is a
//! CREDENTIAL rather than a development seam needs its own deny-list entry of its
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
//! §11.2: "the instance relaunches the child with back-off". Killing the bridge
//! lifts nothing: a hold already standing stays where the instance keeps it,
//! and a relaunched bridge lifts only what its first `Last` read of the fleet's
//! halt rows shows withdrawn. When either descriptor closes,
//! [`crate::fabric::bridge_lost`] runs BEFORE any relaunch, from the
//! `BridgeLostGuard` on the serving thread. By default (the owner's direction of
//! 2026-09-29) it leaves every session the bridge governed running, each with
//! `hold_advisory=fabric-lost`, so a halt published while no bridge is attached
//! lands at the relaunch's `Last` read. Under `[fabric] hold_on_bridge_lost =
//! true` it holds them `reason=fabric-lost origin=fleet` instead, and then the
//! halt does not wait for the supervisor's opinion.
//!
//! ## How this process LEAVES its bridge — `bye quit` and `bye handoff`
//!
//! Both a quit and a seamless update's Commit close the lanes, and the bridge
//! cannot tell the two apart by the close. They need opposite roster answers
//! (design §6): a quit's sessions end with it and must be WITHDRAWN — one
//! `state=exited` row each, so mail to them is refused at the sender instead
//! of queued for nobody — while a handoff's successor adopts the same sids and
//! its own bridge re-advertises them, so nothing may be withdrawn. So this
//! process SAYS which, first ([`bye`]): `EVENT * bye <quit|handoff>` on the
//! push lane, which the bridge takes with `link bye <word>`. A quit waits for
//! that ack, bounded ([`BYE_WAIT`]), and retires the supervisor so no bridge is
//! relaunched to re-advertise what it withdrew; a handoff says it at the
//! Commit and waits for nothing. Saying nothing — a crash — withdraws nothing
//! either: the node's will reads `away`.

use aterm_types::fabric_grammar::Bye;
use aterm_uds::spawnfd::{ChildStdio, LaneChild, LaneCommand};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A development build's override for `[fabric] command` (the e2e harness's): read
/// only through [`aterm_types::dev_seam!`], so a shipped aterm has the config key
/// alone.
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
/// `Ok(child)` — i.e. whenever the spawn succeeded — and escalate only on
/// `Err`, which is the binary being MISSING. The failure the doc actually names
/// (a bad cap file, an unreachable broker) spawns fine and exits within
/// milliseconds, so it reset the floor every time: five process spawns a second,
/// two socketpairs and two aterm threads each, `bridge_lost` on every cycle, for
/// the life of the instance, and `RELAUNCH_MAX` unreachable.
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
/// way: it cannot hand off. Read only through [`aterm_types::dev_seam!`].
const QUIT_BYE_SEAM: &str = "ATERM_DEBUG_QUIT_BYE";

/// The configured bridge command, or `None` when the fabric is off.
///
/// Never through a shell: a command that needs a shell needs `sh -c` spelled
/// out, and going through one implicitly would make every character of the
/// key an injection surface for whoever can write the config. The dev seam is
/// a line, split on whitespace; `[fabric] command` is a line split the same
/// way or an argv array whose words are taken as written
/// ([`crate::app_config::FabricCommand`], `aterm_types::fabric_command`), so a
/// program path with a space in it is one word.
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

/// `[fabric] hold_on_bridge_lost`: `true` restores the fail-closed sweep (a
/// lost bridge holds every session it governed); absent or `false` — the
/// default since the owner's direction of 2026-09-29 — leaves them running
/// with an advisory ([`crate::fabric::bridge_lost`]).
#[must_use]
pub(crate) fn hold_on_bridge_lost(config: &crate::app_config::Config) -> bool {
    config
        .fabric
        .as_ref()
        .and_then(|f| f.hold_on_bridge_lost)
        .unwrap_or(false)
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
/// to be served against one `BRIDGE_CONTEXT`, each one's exit sweeping every
/// session the other governed as lost, and no way to take either back. [`arm`]
/// therefore checks and sets under ONE lock, and `fabric attach` on an armed
/// instance is refused rather than stacked.
///
/// `configured` is what the instance was LAUNCHED with — `[fabric] command` (or a
/// development build's `$ATERM_FABRIC_COMMAND` seam) as [`spawn_supervisor`] read
/// it at startup — recorded so `fabric status` can name it. A command added to
/// the config AFTER launch is not seen here; `fabric attach` reads the file
/// again for it ([`command_on_disk`]), which is the case the verb exists for.
struct Supervisor {
    /// The argv the supervisor thread was armed with — `Some` exactly once per
    /// process, and never `None` again.
    armed: Option<Vec<String>>,
    /// The command the instance was launched with, if any.
    configured: Option<Vec<String>>,
    /// The argv a SEAMLESS UPDATE's outgoing process was armed with by `fabric
    /// attach` (round seven of the update audit, finding 7), put here by the
    /// successor's intake ([`carry_attached`]) and taken by [`spawn_supervisor`]
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
/// withdrew. One-way, like [`Supervisor::armed`]. An atomic and not a field of
/// that record because [`supervise`] reads it, and the supervise thread is
/// spawned under the record's lock ([`arm`]).
static RETIRED: AtomicBool = AtomicBool::new(false);

/// The update attempt whose Commit said `bye handoff` last
/// ([`bye_at_handoff`]); `0`, which no attempt is, before any. A Commit that
/// stands down and retries is the same attempt, and the word is said once.
static HANDOFF_BYE_ATTEMPT: AtomicU64 = AtomicU64::new(0);

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
    // `[fabric] hold_on_bridge_lost`: read before any bridge can attach, so the
    // first loss already answers to it (`fabric::bridge_lost`). Absent is the
    // default since 2026-09-29: a lost bridge halts nothing.
    crate::fabric::set_hold_on_bridge_lost(hold_on_bridge_lost(config));
    let configured = configured_command(config);
    let carried = {
        let mut g = supervisor();
        g.configured = configured.clone();
        g.carried.take()
    };
    // A BRIDGE ARMED AT RUNTIME COMES BACK AFTER AN UPDATE (round seven of the
    // update audit, finding 7): with no command configured, the argv the
    // outgoing process was attached to (`fabric attach`) is armed here, as that
    // process would have been by its own attach. Before this carry the
    // successor came up `fabric=absent`, mail stopped, and every carried
    // `origin=fleet` hold — which only a reconnecting bridge lifts — stood for
    // good, with nothing said. The configured command, when there is one, is
    // still the instance's.
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
        // Startup runs once and before the socket serves a request, so this arm
        // cannot lose to a `fabric attach`; if it ever did, a supervisor IS
        // running, which is what the `bool` answers.
        Err(ArmError::AlreadySupervised(_)) => true,
        // The configured command cannot run. It stays RECORDED (`fabric status`
        // names it), it is NOT armed — the supervisor used to retry a typo for
        // ever from here — and the latch stays open, so correcting `[fabric]
        // command` and a bare `fabric attach` (which reads the file again) is
        // the remedy, not a relaunch.
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
/// launched is the instance's own. `None` when neither names one; an aterm.toml
/// that cannot be read or parsed names none (fail closed: a command no config
/// can be shown to name is not the instance's). What `fabric attach` may arm,
/// beside the launch-time `configured`.
///
/// A direct read of one file on an Owner-only verb a person or `fabric on`
/// runs once, not the config service's watch: the question is what the file
/// says at the moment the latch closes.
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

/// Arm the supervisor with `argv` — the ONE seam that starts a `supervise`
/// thread, shared by the startup path ([`spawn_supervisor`]) and the running
/// instance's `fabric attach`. Exactly once per process: the check and the set
/// happen under the record's lock, so two concurrent attaches cannot both
/// spawn.
///
/// `argv` is executed directly, never through a shell — the caller has already
/// read it the way [`configured_command`] reads the config (a line split on
/// whitespace, or an argv's words as written), so a metacharacter is one more
/// argument and not a second command.
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
/// The name is resolved the way the spawn will resolve it: a word with a
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

/// TELL EVERY WATCHER HOW THIS PROCESS LEAVES (module header): push `EVENT *
/// bye <word>` through the instance stream — the bridge's push lane drains it,
/// and so does every other `sessions` subscriber (`aterm ctl subscribe @*
/// events,sessions`), bridge or none, as the subscribe help says — wake the
/// lanes now rather than at their next 250 ms tick, and wait, each bounded by
/// `wait`: for the bridge's `link bye <word>` when one is attached, then for
/// every `sessions` stream to have written the word to its client
/// ([`crate::subscribe::await_roster_written`]) before the quit closes their
/// connections. Answers whether a bridge took it; `false` at once when none is
/// attached, or when `wait` is zero (said, not waited for).
pub(crate) fn bye(
    store: &crate::session_store::Store,
    subscribers: &crate::subscribe::Subscribers,
    word: Bye,
    wait: Duration,
) -> bool {
    let bridged = crate::fabric::bridge_attached_now();
    let before = crate::fabric::byes_taken();
    let said = say(store, subscribers, word, true);
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
/// seq it was said at, `None` when it was not. With `block` false a store
/// whose lock is held skips the word rather than wait for it.
fn say(
    store: &crate::session_store::Store,
    subscribers: &crate::subscribe::Subscribers,
    word: Bye,
    block: bool,
) -> Option<u64> {
    let (seq, locals): (u64, Vec<u64>) = {
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
        (
            g.roster_seq(),
            g.snapshot().iter().map(|h| h.local_id).collect(),
        )
    };
    // THE BRIDGE WATCHES EVERY SESSION (`subscribe @*`), so waking any of
    // their subscribers wakes its push loop, which drains the journal row
    // above on that wake. With no session at all the loop's own tick does.
    if subscribers.any() {
        let set = subscribers.lock().unwrap_or_else(|p| p.into_inner());
        for local in locals {
            set.notify(local);
        }
    }
    Some(seq)
}

/// THE HANDOFF'S BYE, at a seamless update's Commit (`app_update_handoff`):
/// `bye handoff`, waiting for nothing, ONCE PER ATTEMPT — a Commit that stands
/// down and retries says nothing more — and never blocking the Commit on the
/// store's lock: a busy store skips the word, and the next try of the same
/// attempt says it. A skipped word costs nothing, because no word at all reads
/// the same as a handoff (nothing is withdrawn); the bridge only misses the
/// `ev`.
pub(crate) fn bye_at_handoff(
    store: &crate::session_store::Store,
    subscribers: &crate::subscribe::Subscribers,
    attempt: u64,
) {
    if HANDOFF_BYE_ATTEMPT.load(Ordering::SeqCst) == attempt
        || !crate::fabric::bridge_attached_now()
    {
        return;
    }
    if say(store, subscribers, Bye::Handoff, false).is_some() {
        HANDOFF_BYE_ATTEMPT.store(attempt, Ordering::SeqCst);
    }
}

/// THE QUIT'S BYE, from the one place every quit passes — the end of the
/// event loop (`lib.rs`), which a handoff's Commit never reaches (a headless
/// instance's `SIGTERM` passes it too, `owned_endpoint`): retire the
/// supervisor, then say `bye quit` to every watcher ([`bye`]) and wait for it,
/// bounded by [`BYE_WAIT`]. A bridge that has not taken it in time withdraws
/// the sessions only if it reads the word before the lanes close, and
/// otherwise leaves them to read `away` with the node — said here, so the log
/// has it.
pub(crate) fn bye_at_quit(
    store: &crate::session_store::Store,
    subscribers: &crate::subscribe::Subscribers,
) {
    RETIRED.store(true, Ordering::SeqCst);
    let word = quit_bye(aterm_types::dev_seam!(QUIT_BYE_SEAM).as_deref());
    let bridged = crate::fabric::bridge_attached_now();
    if !bye(store, subscribers, word, BYE_WAIT) && bridged {
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
    g.carried = None;
    RETIRED.store(false, Ordering::SeqCst);
    drop(g);
    set_command_on_disk_for_tests(None);
}

/// What a seamless update's manifest carries of the supervisor
/// ([`crate::session_store::SessionHandoff::fabric_attached`]): the armed argv
/// when `fabric attach` armed it — it differs from the configured command, or
/// none was configured — and nothing when the configuration armed it (the
/// successor reads the same configuration) or nothing is armed.
pub(crate) fn handoff_attached() -> Vec<String> {
    let g = supervisor();
    match &g.armed {
        Some(argv) if g.configured.as_ref() != Some(argv) => argv.clone(),
        _ => Vec::new(),
    }
}

/// The successor's intake of [`handoff_attached`]: kept for
/// [`spawn_supervisor`], which arms it when no command is configured. An empty
/// argv is nothing.
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
                // What `bridge_lost` did to the sessions it governed, which
                // `[fabric] hold_on_bridge_lost` decides.
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

/// The next back-off, given the current one and how long the child RAN — `None`
/// when the spawn itself failed and no child ever existed.
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

/// One bridge generation: the child, and a second handle on each near end, so
/// the lanes can be RETIRED when the child exits.
struct Launched {
    child: LaneChild,
    near: [std::os::unix::net::UnixStream; 2],
}

impl Launched {
    /// Wait for the bridge to exit, then SHUT BOTH LANES DOWN.
    ///
    /// The fail-closed halt fires when a near end reads its end-of-file, and
    /// that used to come only when the LAST copy of the far end closed — which
    /// is not the bridge's exit when another process holds a copy: a child this
    /// process spawned elsewhere caught the far end between `socketpair` and
    /// its `F_SETFD` (49 of 400 plain spawns racing a `pair()`, measured in the
    /// 2026-09-30 review), or one the bridge itself left behind. That holder
    /// kept the old generation's lanes — `Scope::Bridge` authority — served,
    /// and the halt unfired, for as long as it lived. `shutdown` acts on the
    /// socket, not on a descriptor, so the serving threads read end-of-file now
    /// and every copy of the far end is dead with it: the authority ends with
    /// the process it was handed to.
    ///
    /// Each half on its own ([`aterm_uds::hangup::hang_up`]): a bridge that
    /// half-closed a lane before it exited has shut that near end's read half,
    /// and Darwin refuses a both-ways `shutdown` of it, shutting NOTHING — the
    /// holder of the far end went on reading what the lane wrote.
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
    // The handles that retire the lanes (`F_DUPFD_CLOEXEC`: flagged from
    // birth), taken before anything is spawned or served.
    let near = [near_verb.try_clone()?, near_push.try_clone()?];
    let mut cmd = LaneCommand::new(&argv[0]);
    cmd.args(&argv[1..])
        // THE ENVIRONMENT IS FILTERED, not inherited whole. A spawn inherits the
        // parent block by default, which handed the one process holding
        // `Scope::Bridge` aterm's own identity, its control-socket path, and — when
        // this aterm is itself nested — the OUTER instance's `ATERM_EDGE_TOKENS`
        // path, plus the `ATERM_EDGE_*` bearer secrets themselves when that outer
        // aterm is an older one that still injected them (audit finding F1 moved
        // them into a 0600 file). The whole block replaced by the deny-listed
        // one is the same rule the PTY spawn seam applies to a child shell
        // (`aterm-pty`'s `build_child_env`), applied at this seam because that
        // one does not cover it.
        .env_exactly(filter_child_env(std::env::vars_os()))
        // The child's stdin is NOTHING. Its two real channels are fds 3 and 4,
        // and leaving stdin attached to aterm's own would let a bridge read
        // whatever aterm was launched with.
        .stdin(ChildStdio::Null)
        .stdout(ChildStdio::Inherit)
        .stderr(ChildStdio::Inherit);
    let child = aterm_uds::spawnfd::spawn_with_two_fds(
        cmd,
        OwnedFd::from(far_verb),
        OwnedFd::from(far_push),
    )?;

    // ATTACH BOTH, and only after the spawn succeeded: a near end served against
    // a child that never started would report a bridge (`fabric=stalled`, the
    // attach state) that does not exist. Either end closing fires the
    // fail-closed halt, which is exactly §11.2's "when either fd closes".
    // ONE GENERATION PER LAUNCH, shared by both lanes: either lane closing still
    // reports the link lost, and neither lane's late-unwinding guard can report a
    // LATER launch's live bridge disconnected. See
    // [`crate::fabric::BridgeGeneration`].
    let generation = crate::fabric::next_bridge_generation();
    if !attach(near_verb, generation) {
        aterm_log::warn!("fabric bridge verb lane could not be served");
    }
    if !attach(near_push, generation) {
        aterm_log::warn!("fabric bridge push lane could not be served");
    }
    Ok(Launched { child, near })
}

/// aterm's environment with the deny list applied — what the bridge child gets.
///
/// A DENY LIST AND NOT AN ALLOW LIST, on purpose: `aterm-link serve` legitimately
/// needs `HOME`/`XDG_STATE_HOME` for its state directory, `PATH`, and the locale,
/// and an allow list would have to grow every time it learns a new one — silently
/// breaking the bridge each time it did not. The deny list is the canonical
/// [`aterm_types::domain::is_ai_env_key`], the classifier `aterm-pty`'s child
/// shells are built by, so a var added there is stripped here too, with no
/// second copy to drift — non-UTF-8 keys included (it reads their lossy
/// spelling, so a deny-listed prefix on one still denies it).
/// PURE IN ITS INPUT, so the wiring is unit-tested without mutating the
/// process-global environment — the same shape (and for the same reason) as
/// `aterm-pty`'s `build_child_env`.
///
/// ONE DENIED NAME PASSES, in a build that reads development seams: the
/// fast-timers seam ([`aterm_types::domain::ENV_DEV_FAST_TIMERS`]). It is
/// denied so a NESTED aterm runs its own clock; the bridge is this instance's
/// own child, so it runs this instance's — the round period `aterm-link`'s
/// bridge reads from it. A shipped build forwards nothing, and reads no seam.
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

    /// **THE LANES ARE RETIRED WHEN THE BRIDGE EXITS, whoever else holds a far
    /// end** (2026-09-30 review, F2). The bridge here leaves a process behind
    /// holding both far ends — what a child spawned elsewhere in this process
    /// holds when it caught a lane between `socketpair` and its `F_SETFD`. The
    /// recorder holds the near ends as the serving threads do. Once the bridge
    /// is reaped, the near verb end must read its end-of-file — the read the
    /// fail-closed halt waits on — though the holder lives on (before: the read
    /// blocked until the holder died, here past the hang detector).
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
        // Both read bounds are set while the sockets are connected: Darwin
        // refuses `setsockopt` on one that is shut down.
        push.set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        verb.set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        // The holder's pid, so it is reaped by its exact pid below.
        let mut line = String::new();
        std::io::BufReader::new(&push)
            .read_line(&mut line)
            .expect("the holder's pid");
        let holder: i32 = line.trim().parse().expect("a pid");
        let status = launched.wait_and_retire().expect("reap the bridge");
        let mut byte = [0u8; 1];
        let read = verb.read(&mut byte);
        // SAFETY: a plain signal to the holder this test's bridge started, by
        // its exact pid.
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

    /// A LANE THE BRIDGE HALF-CLOSED IS RETIRED TOO. The stand-in bridge shuts
    /// its verb lane's write half (with `perl`: a shell cannot `shutdown`) and
    /// exits, leaving a holder of both far ends behind, as above. The near verb
    /// end already reads end-of-file from the half-close, so what the retire
    /// must still end is the other direction: once the bridge is reaped, a
    /// write to the lane must fail, where it reached the holder before.
    ///
    /// NEGATIVE CONTROL (run by restoring the both-ways `shutdown`): Darwin
    /// refused it for a near end whose read half the bridge's half-close had
    /// shut (`ENOTCONN`) and shut nothing, so the write still succeeded.
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
        // Set while the socket is connected: Darwin refuses `setsockopt` on one
        // that is shut down.
        push.set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        // The holder's pid, so it is reaped by its exact pid below; it is
        // written after the half-close, so the half-close has happened.
        let mut line = String::new();
        std::io::BufReader::new(&push)
            .read_line(&mut line)
            .expect("the holder's pid");
        let holder: i32 = line.trim().parse().expect("a pid");
        let status = launched.wait_and_retire().expect("reap the bridge");
        let wrote = (&verb).write(b"after the retire\n").map_err(|e| e.kind());
        // SAFETY: a plain signal to the holder this test's bridge started, by
        // its exact pid.
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
    /// launch uses. The ARRAY keeps a program path with a space in it ONE word —
    /// the dev app's own `…/aterm (dev).app/Contents/MacOS/aterm` — and its
    /// words are taken as written, a metacharacter or a quote included.
    /// NEGATIVE CONTROL: the same words as a STRING still split on whitespace,
    /// unchanged for every existing config, so the spaced path is two words
    /// there. An empty array, like a blank string, is the fabric off.
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
    /// an argv nobody wrote. NEGATIVE CONTROL: the same array with the bad
    /// element fixed parses.
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

    /// THE COMMAND NEVER REACHES A SHELL. It is split on whitespace and executed
    /// directly, so a metacharacter is one more ARGUMENT and not a second
    /// command — otherwise every byte of this config key would be an injection
    /// surface for whoever can write the file, and the config file is exactly
    /// what a prompt-injected agent with Owner scope would reach for.
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
    /// A CHILD THAT SPAWNS AND EXITS AT ONCE ESCALATES — the failure
    /// `RELAUNCH_MIN`/`RELAUNCH_MAX`'s own doc names ("a bad cap file, an
    /// unreachable broker").
    ///
    /// `supervise` used to reset the back-off on every `Ok(child)`, i.e. whenever
    /// the spawn SUCCEEDED, and escalate only on `Err` — which is the
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
    /// control-socket path, and, when this aterm is itself nested under an older
    /// aterm, the OUTER instance's `ATERM_EDGE_READ`/`WRITE`/`SIGNAL` bearer
    /// secrets: the values audit finding F1 moved out of env into a 0600 file,
    /// which an aterm from before that move still injects. The module
    /// header claimed the child got "none of aterm's own environment beyond what
    /// it needs" while it got all of it.
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

        // A DENY LIST, NOT AN ALLOW LIST. `aterm-link serve` needs `HOME` and
        // `XDG_STATE_HOME` for its state directory and `PATH` to be executable at
        // all, and the e2e harness arms `ATERM_LINK_FAULT` on `aterm-gui` and
        // relies on the CHILD inheriting it (`crates/aterm/tests/fabric_world/harness.rs`),
        // as the fabric bench's ping-pong arm does `ATERM_LINK_WAIT_TALLY`.
        // An allow list would have broken every one of those silently.
        for keeper in [
            "PATH",
            "HOME",
            "XDG_STATE_HOME",
            "ATERM_LINK_FAULT",
            "ATERM_LINK_WAIT_TALLY",
        ] {
            assert!(has(keeper), "the bridge child still needs {keeper}");
        }
        // The fast-timers seam reaches the bridge in a build that reads seams
        // (this test build is one), so a test instance's bridge runs its rounds
        // at the instance's pace; a shipped build passes it to nobody.
        assert_eq!(
            has("ATERM_DEV_FAST_TIMERS"),
            cfg!(any(debug_assertions, feature = "dev-seams")),
            "the bridge runs its instance's clock"
        );
    }

    /// A NON-UTF-8 key that carries a deny-listed PREFIX is still denied
    /// (2026-09-27). The filter let every non-UTF-8 key through on the grounds
    /// that "every deny-listed name is ASCII" — true of the exact names, but the
    /// prefixes match a key's leading bytes, so `ANTHROPIC_\xffX` kept its
    /// `ANTHROPIC_` and reached the bridge child. The last key is the control: a
    /// non-UTF-8 key with no deny-listed prefix still passes.
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

    /// THE SUPERVISOR IS ARMED ONCE PER PROCESS, AND THE LATCH IS ONE-WAY.
    ///
    /// `supervise` loops forever with no stop handle, so a second `arm` would be
    /// a second relaunch loop over the same instance. The check-and-set is under
    /// one lock, the endpoint's `supervised` latch flips at the same moment, and
    /// an empty argv arms nothing — `launch_once` indexes `argv[0]`, and a
    /// thread panicking on it would have "armed" a supervisor that runs nothing.
    ///
    /// The argv names a program that does not exist, ON PURPOSE: the spawn
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
        crate::test_env::scoped_unset(FABRIC_COMMAND_ENV, || {
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

    /// ROUND SEVEN OF THE UPDATE AUDIT, FINDING 7: a bridge armed at runtime
    /// with `fabric attach` rides the handoff manifest and is armed again by the
    /// successor, which has no command configured — before the carry it came up
    /// `fabric=absent` with its carried fleet holds unliftable. NEGATIVE
    /// CONTROLS: a bridge the CONFIGURATION armed is not carried (the successor
    /// reads the same configuration), and a configured command wins over a
    /// carried one.
    ///
    /// FAILS WITHOUT THE FIX: the manifest has no field and the successor's
    /// `status().armed` is `None`.
    #[test]
    fn a_runtime_attached_bridge_is_armed_again_after_an_update() {
        use crate::session_store::{SessionHandoff, SessionStore};
        crate::test_env::scoped_unset(FABRIC_COMMAND_ENV, || {
            let argv = vec![true_program(), "serve".to_string(), "--fleet".to_string()];
            // The producer: launched with no command, then `fabric attach`.
            let wire = crate::fabric::with_link_reset(|| {
                assert!(!spawn_supervisor(&Config::default()));
                arm(argv.clone()).expect("the attach arms");
                assert_eq!(handoff_attached(), argv);
                let manifest = SessionHandoff::from_store(&SessionStore::default());
                assert_eq!(manifest.fabric_attached, argv);
                manifest.to_toml().expect("serializes")
            });
            // The successor: its intake keeps the argv, its control server's
            // supervisor start arms it.
            crate::fabric::with_link_reset(|| {
                let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
                carry_attached(read.fabric_attached);
                assert!(spawn_supervisor(&Config::default()));
                let s = status();
                assert_eq!(s.armed, Some(argv.clone()), "the attached bridge is back");
                assert_eq!(s.configured, None);
                assert!(crate::fabric::bridge_supervised());
                // …and it rides the next handoff too.
                assert_eq!(handoff_attached(), argv);
            });
            // A configured bridge is the configuration's: nothing is carried.
            crate::fabric::with_link_reset(|| {
                let program = true_program();
                assert!(spawn_supervisor(&with_command(Some(&format!(
                    "{program} serve"
                )))));
                assert!(handoff_attached().is_empty());
            });
            // A configured command wins over a carried one.
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
    /// bridge took it, with nothing to wait for (no watcher of this store). A
    /// handoff's word is journalled and the call returns at once: the Commit
    /// does not wait on the bridge. A quit's waits until the bridge answers
    /// `link bye` — here a thread answering it the way the bridge does, once
    /// the row is there to be pushed.
    #[test]
    fn a_quit_says_bye_to_every_watcher_and_waits_only_for_a_bridge_to_take_it() {
        crate::fabric::with_link_reset(|| {
            let store = crate::session_store::new_store();
            let subscribers = crate::subscribe::new_registry();
            let seq = |store: &crate::session_store::Store| {
                store.read().unwrap_or_else(|p| p.into_inner()).roster_seq()
            };
            assert!(!bye(
                &store,
                &subscribers,
                Bye::Quit,
                aterm_spec::wait::MUST_HAPPEN
            ));
            assert_eq!(seq(&store), 1, "no bridge, and said all the same");

            let generation = crate::fabric::next_bridge_generation();
            crate::fabric::bridge_attached(generation);
            assert!(!bye(&store, &subscribers, Bye::Handoff, Duration::ZERO));
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
            assert!(bye(
                &store,
                &subscribers,
                Bye::Quit,
                aterm_spec::wait::MUST_HAPPEN
            ));
            assert_eq!(bridge.join().expect("the bridge's answer"), "OK\n");
        });
    }

    /// **A RETIRED SUPERVISOR RELAUNCHES NOTHING.** Once a quit has told its
    /// bridge `bye quit` the supervisor returns instead of launching, so no
    /// bridge comes back to re-advertise the sessions the quit withdrew. The
    /// argv names a program that does not exist: a supervisor that did not
    /// return would retry it for ever, which the hang detector catches.
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
