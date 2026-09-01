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

/// Launch and SUPERVISE the bridge, if one is configured. Returns whether a
/// supervisor thread started — `false` means the fabric is off or a thread could
/// not be spawned, never a half-attached bridge.
///
/// Called once, from the control server, after the socket is bound and
/// `BRIDGE_CONTEXT` is published: a bridge that attached earlier would find a
/// half-built process.
pub(crate) fn spawn_supervisor(config: &crate::app_config::Config) -> bool {
    let Some(argv) = configured_command(config) else {
        return false;
    };
    std::thread::Builder::new()
        .name("aterm-fabric-launch".to_string())
        .spawn(move || supervise(&argv))
        .is_ok()
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
}
