// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Spawn-seam containment actuator: turns the policy data model (mode →
//! [`Capabilities`](crate::Capabilities)) into a logged decision at the one
//! place aterm forks a child shell.
//!
//! ## What is enforced (`ATERM_DESIGN` §0.1 / §5.6)
//!
//! The mode → capabilities mapping enforces nothing by itself. This module
//! produces a [`SpawnDecision`] that both launchers (`aterm` and the window)
//! consult before handing the PTY seam a spawn capability, and audits it:
//!
//! | Mode | Enforced |
//! |---|---|
//! | Master, User | Nothing beyond the capability-gated spawn; the shell keeps aterm's own resource limits. |
//! | Safety | Hardened resource limits (`aterm-sandbox`: rlimits on Unix, the Job Object on Windows). No OS sandbox. |
//! | Containment | The hardened limits plus the OS sandbox below; on a platform without one, no shell. |
//!
//! The Containment OS sandbox (macOS Seatbelt) wraps the spawn with
//! `/usr/bin/sandbox-exec -p <SBPL>`, the profile from
//! [`crate::sbpl::profile_for`] (whose module doc lists the rules). For the
//! child shell and everything it runs, the kernel:
//!
//! - denies ALL network (`(deny network*)`);
//! - refuses every task port but a process's own — control, read, inspect and
//!   name, its own children's included — so a contained `lldb`/`debugserver`
//!   cannot attach to anything ([`crate::sbpl`] rule 2);
//! - confines WRITES to the temp roots (`/private/tmp`, `/private/var/tmp`, the
//!   user's own `$TMPDIR`) and `/dev`, plus the shell's own history files, so
//!   nothing it does can plant code that a later unsandboxed shell runs
//!   (`~/.zshrc`, a `~/Library/LaunchAgents` plist, a git hook, a `$PATH`
//!   binary, a precompiled module in the user cache directory). Its working
//!   directory and `$HOME` are read-only to it: decided 2026-09-26 under the
//!   owner's standing direction, `TmpOnly` means what its name says
//!   ([`crate::sbpl`] rule 3);
//! - denies READ and write of the credential stores (`~/.ssh`, `~/.aws`,
//!   `~/.gnupg`, `~/.config/gh`, `~/.config/aterm`, `~/.netrc`, …) and of the
//!   private-data stores (`~/Documents`, `~/Desktop`, `~/Downloads`, media, and the
//!   local Mail / Messages / keychain / cookies / browser-profile databases).
//!
//! Other reads stay allowed so a normal `$SHELL` works. Every rule is proven
//! against the live kernel by the enforcement tests here.
//! [`SpawnDecision::Permit::sbpl`] carries the per-user profile; the PTY seam
//! refuses to spawn if the wrapper binary is missing.
//!
//! Fail closed: `Containment` never degrades. Where [`os_sandbox_actuated`] is
//! false (every non-macOS target), [`decide`] returns [`SpawnDecision::Deny`]
//! naming the gap and both launchers exit without a shell. Containment is
//! macOS-only by scope (`docs/ATERM_DESIGN.md` §5.6)
//! (owner, 2026-09-25; 2026-09-27).

use crate::audit::{log_denial, log_posture};
use crate::capability::{NetworkCapability, ProcessCapability};
use crate::mode::ContainmentMode;
use crate::policy::ContainmentPolicy;

/// Audit subsystem label for spawn-seam containment events.
const SUBSYSTEM: &str = "spawn";

/// Why [`decide`] refuses a `Containment` spawn on a platform with no OS sandbox.
/// Both launchers print it verbatim.
pub const NO_OS_SANDBOX_REASON: &str = "the sandbox exists only on macOS, so no shell was started";

/// Whether this build/platform can actuate a real OS sandbox at the spawn
/// seam: `true` only on macOS (Seatbelt via `sandbox-exec`). Where `false`,
/// `Containment` spawns are refused. See [`network_sandbox_actuated`] for the
/// per-mode answer.
#[must_use]
pub const fn os_sandbox_actuated() -> bool {
    cfg!(target_os = "macos")
}

/// Whether the OS network sandbox is in force for a spawn in `mode`: `true`
/// iff [`os_sandbox_actuated`] and the mode's network policy is
/// [`None`](crate::NetworkCapability::None) (only `Containment`). This is the
/// per-spawn statement the audit log and the GUI use.
#[must_use]
pub fn network_sandbox_actuated(mode: ContainmentMode) -> bool {
    os_sandbox_actuated() && ContainmentPolicy::network(mode) == NetworkCapability::None
}

/// The actuated decision for the single spawn seam, given a containment mode.
/// Not `Copy`: `Permit` carries the owned per-user `sbpl` profile.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpawnDecision {
    /// Spawning the initial shell is permitted.
    ///
    /// `os_sandbox` records whether a real OS sandbox backs this spawn (see
    /// [`network_sandbox_actuated`]): `true` only for `Containment` on macOS,
    /// where the launcher must wrap the child in `sandbox-exec` with `sbpl`.
    /// When `false`, an explicit audit line was logged.
    ///
    /// `sbpl` is `Some` exactly when `os_sandbox` is `true`: the per-user
    /// profile for `/usr/bin/sandbox-exec -p <sbpl>`. A launcher that cannot
    /// apply it (e.g. the wrapper is missing) must not spawn an unsandboxed
    /// shell.
    Permit {
        /// The mode this decision was made for.
        mode: ContainmentMode,
        /// Whether a real OS sandbox is in force (see [`network_sandbox_actuated`]).
        os_sandbox: bool,
        /// The SBPL profile to apply via `sandbox-exec`, `Some` iff `os_sandbox`.
        sbpl: Option<String>,
        /// Whether `sbpl` holds the `$HOME`-scoped rules (the shell-history write
        /// allowance and the credential and private-data read denies). `false`
        /// when there is no sandbox, or when `$HOME` did not resolve.
        home_scoped: bool,
    },
    /// Spawning is denied for this mode; a denial was logged to the containment
    /// audit trail and the launcher must exit without a shell. Reached by
    /// `Containment` wherever [`os_sandbox_actuated`] is false, and by any future
    /// process capability below `NoFork`.
    Deny {
        /// The mode this decision was made for.
        mode: ContainmentMode,
        /// Why, in words the launcher prints verbatim.
        reason: &'static str,
    },
}

impl SpawnDecision {
    /// Whether this decision permits the spawn.
    #[must_use]
    pub const fn is_permitted(&self) -> bool {
        matches!(self, SpawnDecision::Permit { .. })
    }
}

/// Decide, and audit, whether the single PTY spawn seam may run for `mode`,
/// and for `Containment` how the OS sandbox backs it (`ATERM_DESIGN` §5.6).
///
/// `Containment` on macOS: [`SpawnDecision::Permit`] with `os_sandbox: true`
/// and the per-user SBPL profile the launcher must apply. `Containment`
/// elsewhere: [`SpawnDecision::Deny`] with [`NO_OS_SANDBOX_REASON`]. Every
/// other mode: `os_sandbox` false, `sbpl` `None`, and an audit line saying no
/// OS sandbox is in force.
///
/// The initial interactive shell is otherwise permitted in every mode
/// (`Containment`/`NoFork` means "exec only, for the initial shell"); a future
/// `ProcessCapability` below `NoFork` would get [`SpawnDecision::Deny`].
#[must_use]
pub fn decide(mode: ContainmentMode) -> SpawnDecision {
    decide_on(
        mode,
        os_sandbox_actuated(),
        crate::sbpl::home_dir().as_deref(),
    )
}

/// [`decide`] with the platform's sandbox capability and the `$HOME` the
/// profile is scoped under as arguments, so the fail-closed arm is testable on
/// the one platform that has a sandbox, and the unresolved-home arm anywhere.
fn decide_on(mode: ContainmentMode, sandbox_available: bool, home: Option<&str>) -> SpawnDecision {
    let process_cap = ContainmentPolicy::process(mode);
    let denies_network = ContainmentPolicy::network(mode) == NetworkCapability::None;
    // Containment demands the OS sandbox; without one, refuse the spawn (owner, 2026-09-25).
    if denies_network && !sandbox_available {
        log_denial(SUBSYSTEM, "spawn initial shell", mode, NO_OS_SANDBOX_REASON);
        return SpawnDecision::Deny {
            mode,
            reason: NO_OS_SANDBOX_REASON,
        };
    }
    // `os_sandbox` is true only for Containment where the platform can actuate;
    // `sbpl` is then the per-user profile (`sbpl.is_some() == os_sandbox`).
    let os_sandbox = denies_network && sandbox_available;
    let scoped: Option<(String, bool)> = if os_sandbox {
        crate::sbpl::profile_for_scoped(ContainmentPolicy::capabilities(mode), home)
    } else {
        None
    };
    let home_scoped = scoped.as_ref().is_some_and(|(_, home)| *home);
    let sbpl: Option<String> = scoped.map(|(profile, _)| profile);
    debug_assert_eq!(
        sbpl.is_some(),
        os_sandbox,
        "sbpl must be Some iff os_sandbox"
    );

    // Posture records, not denials: `log_posture` (no `DENIED:` prefix), so a
    // denial-stream filter never counts a permitted spawn.
    if os_sandbox {
        log_posture(
            SUBSYSTEM,
            "os-network-sandbox",
            mode,
            applied_posture(home_scoped),
        );
    } else {
        // Explicit record that no OS sandbox is in force (the policy permits
        // network). Safety's hardened limits apply as rlimits on Unix and via
        // the Job Object on Windows (`aterm_sandbox::Limits::shell_default`);
        // Master and User keep aterm's own (`Limits::inherit`), so their record
        // claims no limits.
        log_posture(
            SUBSYSTEM,
            "os-network-sandbox",
            mode,
            if matches!(mode, ContainmentMode::Master | ContainmentMode::User) {
                "OS sandbox not applied (network permitted by policy); process-cap gate only (resource limits inherited from aterm's own process)"
            } else {
                "OS sandbox not applied (network permitted by policy); resource limits + process-cap gate only"
            },
        );
    }
    match process_cap {
        // Every currently-defined capability permits the initial shell.
        ProcessCapability::Full | ProcessCapability::Restricted | ProcessCapability::NoFork => {
            SpawnDecision::Permit {
                mode,
                os_sandbox,
                sbpl,
                home_scoped,
            }
        }
        // Defensive default: any future, more-restrictive variant fails closed.
        #[allow(unreachable_patterns)]
        _ => {
            let reason = "process capability denies fork/exec";
            log_denial(SUBSYSTEM, "spawn initial shell", mode, reason);
            SpawnDecision::Deny { mode, reason }
        }
    }
}

/// What an applied sandbox enforces, given [`SpawnDecision::Permit`]'s
/// `home_scoped`: the history allowance and the credential and private-data
/// denies are joined onto `$HOME`, so an unresolved home gets neither, and a
/// `$HOME` other than the account's leaves that account's stores readable.
/// Shared wording for the aterm.log posture record and the window's
/// `--verbose` line.
#[must_use]
pub fn applied_posture(home_scoped: bool) -> &'static str {
    if home_scoped {
        "OS sandbox applied: no network; no debugger attach (no task port but a process's own); writes only to temp dirs and shell history; no access to credential stores or private data under $HOME"
    } else {
        "OS sandbox applied: no network; no debugger attach (no task port but a process's own); writes only to temp dirs; $HOME did not resolve, so credential stores and private data can still be read"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_sandbox_actuation_matches_platform() {
        // The platform capability (not per-spawn; that is
        // `network_sandbox_actuated`).
        assert_eq!(os_sandbox_actuated(), cfg!(target_os = "macos"));
    }

    #[test]
    fn network_sandbox_in_force_only_for_containment_on_macos() {
        // Only Containment (network = None) is OS-network-sandboxed, and only on a
        // platform that can actuate. User/Safety/Master permit network → never.
        assert_eq!(
            network_sandbox_actuated(ContainmentMode::Containment),
            cfg!(target_os = "macos")
        );
        for mode in [
            ContainmentMode::Master,
            ContainmentMode::User,
            ContainmentMode::Safety,
        ] {
            assert!(
                !network_sandbox_actuated(mode),
                "{mode} permits network — must NOT be OS-network-sandboxed"
            );
        }
    }

    #[test]
    fn every_mode_permits_the_initial_shell_where_its_sandbox_exists() {
        for mode in [
            ContainmentMode::Master,
            ContainmentMode::User,
            ContainmentMode::Safety,
        ] {
            let d = decide(mode);
            assert!(
                d.is_permitted(),
                "the initial shell must be permitted in {mode} mode, got {d:?}"
            );
        }
        // Containment only where the OS sandbox exists (macOS).
        assert_eq!(
            decide(ContainmentMode::Containment).is_permitted(),
            os_sandbox_actuated()
        );
    }

    #[test]
    fn containment_fails_closed_without_an_os_sandbox() {
        // On a platform with no OS sandbox the Containment spawn is refused,
        // naming the gap (owner, 2026-09-25).
        assert_eq!(
            decide_on(ContainmentMode::Containment, false, None),
            SpawnDecision::Deny {
                mode: ContainmentMode::Containment,
                reason: NO_OS_SANDBOX_REASON,
            }
        );
        assert!(NO_OS_SANDBOX_REASON.contains("only on macOS"));
        // The other modes never demanded a sandbox, so its absence changes nothing.
        for mode in [
            ContainmentMode::Master,
            ContainmentMode::User,
            ContainmentMode::Safety,
        ] {
            assert_eq!(
                decide_on(mode, false, None),
                SpawnDecision::Permit {
                    mode,
                    os_sandbox: false,
                    sbpl: None,
                    home_scoped: false,
                },
                "{mode} must not depend on the OS sandbox"
            );
        }
        // Negative control: with a sandbox, Containment is permitted and wrapped.
        match decide_on(ContainmentMode::Containment, true, None) {
            SpawnDecision::Permit {
                os_sandbox: true,
                sbpl: Some(_),
                ..
            } => {}
            other => panic!("Containment with a sandbox must Permit+wrap, got {other:?}"),
        }
    }

    #[test]
    fn permit_carries_sbpl_iff_network_sandbox_actuated() {
        // Containment on macOS: Permit{os_sandbox:true, sbpl:Some(profile)} where
        // the profile begins with the base profile. Everything else:
        // Permit{os_sandbox:false, sbpl:None}. The sbpl and the os_sandbox flag are
        // always in lockstep.
        for mode in [
            ContainmentMode::Master,
            ContainmentMode::User,
            ContainmentMode::Safety,
            ContainmentMode::Containment,
        ] {
            if mode == ContainmentMode::Containment && !os_sandbox_actuated() {
                continue; // refused there — `containment_fails_closed_without_an_os_sandbox`
            }
            let expect_os = network_sandbox_actuated(mode);
            match decide(mode) {
                SpawnDecision::Permit {
                    mode: m,
                    os_sandbox,
                    sbpl,
                    home_scoped,
                } => {
                    assert_eq!(m, mode);
                    assert_eq!(os_sandbox, expect_os, "os_sandbox posture for {mode}");
                    assert!(
                        os_sandbox || !home_scoped,
                        "{mode}: no sandbox claims no $HOME-scoped rules"
                    );
                    assert_eq!(
                        sbpl.is_some(),
                        expect_os,
                        "sbpl must be Some iff os_sandbox for {mode}"
                    );
                    if let Some(profile) = sbpl {
                        assert!(
                            profile.starts_with(crate::sbpl::BASE_PROFILE),
                            "{mode} sbpl must begin with the base profile; got {profile}"
                        );
                    }
                }
                other => panic!("{mode} must Permit, got {other:?}"),
            }
        }
    }

    #[test]
    fn non_containment_spawn_is_never_os_sandboxed() {
        // User (the default), Safety and Master get os_sandbox=false and
        // sbpl=None, so the launcher applies no sandbox-exec wrap.
        for mode in [
            ContainmentMode::User,
            ContainmentMode::Safety,
            ContainmentMode::Master,
        ] {
            match decide(mode) {
                SpawnDecision::Permit {
                    os_sandbox, sbpl, ..
                } => {
                    assert!(!os_sandbox, "{mode} must not be OS-sandboxed");
                    assert!(sbpl.is_none(), "{mode} must carry no SBPL (no wrap)");
                }
                other => panic!("{mode} must Permit, got {other:?}"),
            }
        }
    }

    /// The applied posture claims the shell-history allowance and the credential
    /// and private-data denies only when the profile holds them, and scopes the
    /// claim to `$HOME`, the only home the denies are joined onto.
    #[test]
    fn applied_posture_claims_home_denies_only_when_emitted() {
        assert!(
            applied_posture(true)
                .ends_with("no access to credential stores or private data under $HOME")
        );
        let homeless = applied_posture(false);
        assert!(!homeless.contains("no access"), "{homeless}");
        assert!(!homeless.contains("shell history"), "{homeless}");
        // The task-port pair is in the base profile, so it holds with no home too.
        for posture in [applied_posture(true), homeless] {
            assert!(posture.contains("no debugger attach"), "{posture}");
        }
    }

    /// `home_scoped` is decided by whether the home resolved, not by whether a
    /// sandbox applies: a sandboxed spawn with no home carries neither the
    /// credential denies nor the claim. Negative control for wiring it as
    /// `home_scoped: os_sandbox`.
    #[test]
    fn home_scoped_follows_the_home_not_the_sandbox() {
        let secret = |profile: &str| profile.contains("/.ssh\"");
        match decide_on(ContainmentMode::Containment, true, None) {
            SpawnDecision::Permit {
                os_sandbox: true,
                sbpl: Some(profile),
                home_scoped: false,
                ..
            } => assert!(!secret(&profile), "no home, no credential deny: {profile}"),
            other => panic!("a homeless sandbox must Permit unscoped, got {other:?}"),
        }
        let home = aterm_tempfile::tempdir().unwrap();
        match decide_on(ContainmentMode::Containment, true, home.path().to_str()) {
            SpawnDecision::Permit {
                os_sandbox: true,
                sbpl: Some(profile),
                home_scoped: true,
                ..
            } => assert!(
                secret(&profile),
                "a home carries the credential deny: {profile}"
            ),
            other => panic!("a sandbox with a home must Permit scoped, got {other:?}"),
        }
    }

    #[test]
    fn decision_is_permitted_helper_matches_variant() {
        assert!(
            SpawnDecision::Permit {
                mode: ContainmentMode::User,
                os_sandbox: false,
                sbpl: None,
                home_scoped: false,
            }
            .is_permitted()
        );
        assert!(
            !SpawnDecision::Deny {
                mode: ContainmentMode::Containment,
                reason: NO_OS_SANDBOX_REASON,
            }
            .is_permitted()
        );
    }

    // Enforcement proof (macOS). These tests spawn a probe via the same
    // `sandbox-exec -p <sbpl> <prog> <args>` wrapping the launcher uses,
    // attempt an operation the profile denies, and assert it fails. Without
    // them passing, `os_sandbox_actuated()` must not report macOS as actuating.

    /// Build the sandbox-exec argv the launcher builds: `sandbox-exec -p <sbpl>
    /// <prog> <args...>`. Mirrors the wrap done in `aterm-pty::spawn_shell`.
    #[cfg(target_os = "macos")]
    fn sandbox_wrap(sbpl: &str, prog: &str, args: &[&str]) -> aterm_uds::spawnfd::Command {
        let mut cmd = aterm_uds::spawnfd::command(crate::sbpl::SANDBOX_EXEC_PATH);
        cmd.arg("-p").arg(sbpl).arg(prog).args(args);
        cmd
    }

    /// Proof 1 (network), hermetic: a loopback TCP listener in this process; a
    /// probe (`/usr/bin/nc`) connects without the sandbox and fails with the
    /// actuator's `sandbox-exec -p '(deny network*)'` wrap.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_network_deny_blocks_loopback_connect() {
        use std::io::Write;
        use std::net::TcpListener;

        // The exact profile the actuator hands the launcher for Containment.
        let sbpl = decide(ContainmentMode::Containment);
        let SpawnDecision::Permit {
            os_sandbox: true,
            sbpl: Some(profile),
            ..
        } = sbpl
        else {
            panic!("Containment on macOS must actuate the network sandbox; got {sbpl:?}");
        };
        // The full per-user profile begins with the base profile (it may also carry
        // the secret-dir denies); its network deny is what THIS proof tests.
        assert!(profile.starts_with(crate::sbpl::BASE_PROFILE));

        // Loopback listener in the parent. An accept thread keeps draining so the
        // control connect completes cleanly; bound to port 0 (kernel-assigned).
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let accepter = std::thread::spawn(move || {
            // Accept up to two connections (control may or may not be one) then
            // return; a short loop so the thread always terminates.
            for _ in 0..2 {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let _ = s.write_all(b"x");
                    }
                    Err(_) => break,
                }
            }
        });

        let port_s = port.to_string();
        // `-G`/`-w` bound nc's connect time so a deny fails fast instead of hanging.
        let nc_args = ["-G", "2", "-w", "2", "-z", "127.0.0.1", port_s.as_str()];

        // Control: no sandbox, the loopback connect succeeds, so a failure
        // under the sandbox is the sandbox's.
        let control = std::process::Command::new("/usr/bin/nc")
            .args(nc_args)
            .output()
            .expect("run nc control");
        assert!(
            control.status.success(),
            "control (unsandboxed) loopback connect must SUCCEED; rc={:?} stderr={}",
            control.status.code(),
            String::from_utf8_lossy(&control.stderr),
        );

        // Sandboxed: same probe and listener; the connect must fail.
        let sandboxed = sandbox_wrap(&profile, "/usr/bin/nc", &nc_args)
            .output()
            .expect("run nc under sandbox-exec");
        assert!(
            !sandboxed.status.success(),
            "DENY FAILED: loopback connect SUCCEEDED under (deny network*) — \
             the sandbox did NOT enforce. rc={:?} stderr={}",
            sandboxed.status.code(),
            String::from_utf8_lossy(&sandboxed.stderr),
        );

        // Unblock the accept thread's second accept with a throwaway connect,
        // then join.
        let _ = std::net::TcpStream::connect(("127.0.0.1", port));
        let _ = accepter.join();
    }

    /// Proof 2 (EPERM): the same wrap with a file-read-deny profile over a real
    /// temp file, so the denial surfaces as "Operation not permitted". The path
    /// is canonicalized, since Seatbelt matches `/private/tmp`, not `/tmp`.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_file_read_deny_yields_operation_not_permitted() {
        use std::io::Write;

        // A real temp file with known contents, then its canonical path.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("aterm-sbpl-enforce-{}.txt", std::process::id()));
        {
            let mut f = std::fs::File::create(&path).expect("create temp file");
            f.write_all(b"SECRET-ENFORCE-PROBE")
                .expect("write temp file");
        }
        let canon = std::fs::canonicalize(&path).expect("canonicalize temp path");
        let canon_s = canon.to_str().expect("utf8 path").to_string();

        // CONTROL: no sandbox → `cat` reads the file (rc 0, contents present).
        let control = std::process::Command::new("/bin/cat")
            .arg(&canon_s)
            .output()
            .expect("run cat control");
        assert!(
            control.status.success()
                && String::from_utf8_lossy(&control.stdout).contains("SECRET-ENFORCE-PROBE"),
            "control cat must read the file unsandboxed; rc={:?}",
            control.status.code(),
        );

        // SANDBOXED: deny read of the canonical path → cat fails with EPERM, and
        // the kernel/`cat` reports "Operation not permitted" on stderr.
        let profile =
            format!("(version 1)(allow default)(deny file-read* (literal \"{canon_s}\"))");
        let sandboxed = sandbox_wrap(&profile, "/bin/cat", &[canon_s.as_str()])
            .output()
            .expect("run cat under sandbox-exec");
        let stderr = String::from_utf8_lossy(&sandboxed.stderr);
        let stdout = String::from_utf8_lossy(&sandboxed.stdout);

        // Clean up before asserting (so a failed assert doesn't leak the file).
        let _ = std::fs::remove_file(&path);

        assert!(
            !sandboxed.status.success(),
            "DENY FAILED: cat SUCCEEDED reading a file under (deny file-read*); \
             the sandbox did NOT enforce. rc={:?}",
            sandboxed.status.code(),
        );
        assert!(
            !stdout.contains("SECRET-ENFORCE-PROBE"),
            "DENY FAILED: file contents leaked through a (deny file-read*) sandbox",
        );
        assert!(
            stderr.contains("Operation not permitted"),
            "expected an EPERM 'Operation not permitted' denial from Seatbelt, \
             got stderr: {stderr:?}",
        );
    }

    /// Proof 3 (shell compatibility): an ordinary command pipeline under the
    /// exact `Containment` profile the actuator produces (scoped under the real
    /// `$HOME`) exits 0 with the expected output.
    #[cfg(target_os = "macos")]
    #[test]
    fn shell_compat_full_containment_profile_keeps_a_normal_shell_working() {
        let decision = decide(ContainmentMode::Containment);
        let SpawnDecision::Permit {
            os_sandbox: true,
            sbpl: Some(profile),
            ..
        } = decision
        else {
            panic!("Containment on macOS must actuate the OS sandbox; got {decision:?}");
        };
        // It is the base profile PLUS a file deny — never a blanket FS deny.
        assert!(profile.starts_with(crate::sbpl::BASE_PROFILE));

        // Ordinary operations (echo, pwd, a listing, a read of a system file)
        // touch no secret path and must succeed.
        let out = sandbox_wrap(
            &profile,
            "/bin/sh",
            &[
                "-c",
                "echo hi; pwd >/dev/null; ls / >/dev/null; cat /etc/hosts >/dev/null && echo done",
            ],
        )
        .output()
        .expect("run /bin/sh under the full Containment profile");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "SHELL BROKEN: a normal shell must exit 0 under the full Containment \
             profile (network + secret-dir deny). rc={:?} stdout={stdout:?} stderr={stderr:?}",
            out.status.code(),
        );
        assert!(
            stdout.contains("hi") && stdout.contains("done"),
            "SHELL BROKEN: expected 'hi' and 'done' from the shell; stdout={stdout:?} stderr={stderr:?}",
        );
    }

    /// Proof 4 (targeted secret deny): under the exact `Containment` profile,
    /// (a) a probe file inside `$HOME/.aws` is not readable (EPERM), and (b)
    /// `/etc/hosts` still is, so the deny targets the secret set rather than
    /// the filesystem. The probe file is cleaned up.
    #[cfg(target_os = "macos")]
    #[test]
    fn secret_deny_full_containment_profile_blocks_secret_but_allows_nonsecret() {
        use std::io::Write;

        // Need a real, resolvable $HOME for the profile to scope under. If $HOME is
        // somehow unset in the test env, the profile is the base one and this proof
        // is not applicable — skip rather than false-pass.
        let Ok(home) = std::env::var("HOME") else {
            eprintln!("HOME unset in test env — skipping secret-deny proof");
            return;
        };
        if home.is_empty() {
            eprintln!("HOME empty in test env — skipping secret-deny proof");
            return;
        }

        let decision = decide(ContainmentMode::Containment);
        let SpawnDecision::Permit {
            os_sandbox: true,
            sbpl: Some(profile),
            ..
        } = decision
        else {
            panic!("Containment on macOS must actuate the OS sandbox; got {decision:?}");
        };

        // Plant a probe inside one of the DENIED secret dirs ($HOME/.aws). Create
        // the dir if needed; remember whether WE created it so cleanup is precise.
        let canon_home = std::fs::canonicalize(&home).expect("canonicalize HOME");
        let secret_dir = canon_home.join(".aws");
        let dir_pre_existed = secret_dir.exists();
        std::fs::create_dir_all(&secret_dir).expect("create $HOME/.aws for probe");
        let secret_probe = secret_dir.join(format!("aterm_probe_{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&secret_probe).expect("create secret probe");
            f.write_all(b"SECRET-DENY-PROBE")
                .expect("write secret probe");
        }
        let secret_probe_s = secret_probe.to_str().expect("utf8 path").to_string();

        // Sanity (CONTROL): unsandboxed, the probe IS readable — so a deny under the
        // sandbox is attributable to the sandbox, not a broken probe.
        let control = std::process::Command::new("/bin/cat")
            .arg(&secret_probe_s)
            .output()
            .expect("run cat control on secret probe");
        let control_ok = control.status.success()
            && String::from_utf8_lossy(&control.stdout).contains("SECRET-DENY-PROBE");

        // (a) sandboxed read of the secret probe → must be denied (EPERM).
        let denied = sandbox_wrap(&profile, "/bin/cat", &[secret_probe_s.as_str()])
            .output()
            .expect("run cat on secret probe under Containment profile");
        let denied_stderr = String::from_utf8_lossy(&denied.stderr).into_owned();
        let denied_stdout = String::from_utf8_lossy(&denied.stdout).into_owned();

        // (b) sandboxed read of a NON-secret file (/etc/hosts) → must succeed.
        let allowed = sandbox_wrap(&profile, "/bin/cat", &["/etc/hosts"])
            .output()
            .expect("run cat on /etc/hosts under Containment profile");
        let allowed_ok = allowed.status.success();

        // Clean up the probe (and the dir if we created it) before asserting, so a
        // failed assert never leaks the user's real ~/.aws contents or our probe.
        let _ = std::fs::remove_file(&secret_probe);
        if !dir_pre_existed {
            let _ = std::fs::remove_dir(&secret_dir);
        }

        assert!(
            control_ok,
            "CONTROL FAILED: unsandboxed cat must read the probe"
        );
        // (a) secret denied with EPERM.
        assert!(
            !denied.status.success(),
            "SECRET DENY FAILED: cat SUCCEEDED reading $HOME/.aws/<probe> under the \
             Containment profile. rc={:?}",
            denied.status.code(),
        );
        assert!(
            !denied_stdout.contains("SECRET-DENY-PROBE"),
            "SECRET DENY FAILED: secret contents leaked through the Containment sandbox",
        );
        assert!(
            denied_stderr.contains("Operation not permitted"),
            "expected EPERM 'Operation not permitted' denying the secret; got stderr={denied_stderr:?}",
        );
        // (b) non-secret still allowed → the scope is targeted, not blanket.
        assert!(
            allowed_ok,
            "OVER-BROAD DENY: a NON-secret file (/etc/hosts) was NOT readable under \
             the Containment profile — the secret deny is too broad. rc={:?} stderr={}",
            allowed.status.code(),
            String::from_utf8_lossy(&allowed.stderr),
        );
    }

    /// The exact profile `decide` hands the launcher for `Containment` on macOS.
    #[cfg(target_os = "macos")]
    fn containment_profile() -> String {
        match decide(ContainmentMode::Containment) {
            SpawnDecision::Permit {
                os_sandbox: true,
                sbpl: Some(profile),
                ..
            } => profile,
            other => panic!("Containment on macOS must actuate the OS sandbox; got {other:?}"),
        }
    }

    /// Open `path` for append (creating it if absent, writing nothing) under
    /// `profile`, returning whether the open succeeded.
    #[cfg(target_os = "macos")]
    fn sandboxed_append_ok(profile: &str, path: &std::path::Path) -> bool {
        sandboxed_sh_ok(profile, &format!(": >> '{}'", path.display()))
    }

    /// Run `script` with `/bin/sh -c` under `profile`; whether it exited 0.
    #[cfg(target_os = "macos")]
    fn sandboxed_sh_ok(profile: &str, script: &str) -> bool {
        sandbox_wrap(profile, "/bin/sh", &["-c", script])
            .output()
            .expect("run sh under the Containment profile")
            .status
            .success()
    }

    /// PROOF 5 (WRITE CONFINEMENT, FULL generated Containment profile) — a
    /// contained shell cannot plant anything that later runs OUTSIDE the sandbox:
    /// creating a file in `$HOME`, appending to `~/.zshrc`, or dropping a file in
    /// `~/Library/LaunchAgents` (creating that directory, where it does not exist
    /// yet) or into the user cache directory beside `$TMPDIR` all fail, while
    /// the temp roots stay writable (the history allowance
    /// has its own proof, 5b, over a scratch `$HOME`, since whether the real
    /// one's history file gets it depends on what that file is). Each probe
    /// opens for append without writing a byte, and anything a failed deny
    /// would have created is removed. CONTROL: under the base profile alone the
    /// same `$HOME` probe is writable, so its denial is the write confinement's.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_containment_confines_writes() {
        let Ok(home) = std::env::var("HOME") else {
            eprintln!("HOME unset in test env — skipping write-confinement proof");
            return;
        };
        let Ok(home) = std::fs::canonicalize(&home) else {
            eprintln!("HOME does not resolve — skipping write-confinement proof");
            return;
        };
        let profile = containment_profile();
        let pid = std::process::id();

        let mut denied = vec![
            home.join(format!(".aterm_write_probe_{pid}")),
            home.join(".zshrc"),
        ];
        let launch_agents = home.join("Library/LaunchAgents");
        let agents_existed = launch_agents.is_dir();
        if agents_existed {
            denied.push(launch_agents.join(format!("aterm_write_probe_{pid}")));
        }
        let tmp = std::fs::canonicalize(std::env::temp_dir()).expect("canonical TMPDIR");
        // Beside a per-user `…/T`: the user cache directory `…/C`, whose clang
        // module cache later unsandboxed builds read, stays read-only.
        let user_cache = tmp.parent().map(|per_user| per_user.join("C"));
        if tmp.starts_with(crate::sbpl::USER_TEMP_PARENT)
            && let Some(cache) = user_cache.filter(|c| c.is_dir())
        {
            denied.push(cache.join(format!("aterm_write_probe_{pid}")));
        }
        let allowed = [
            tmp.join(format!("aterm_write_probe_{pid}")),
            std::path::PathBuf::from(format!("/private/tmp/aterm_write_probe_{pid}")),
        ];

        let existed: Vec<bool> = denied.iter().chain(&allowed).map(|p| p.exists()).collect();
        let denied_ok: Vec<bool> = denied
            .iter()
            .map(|p| sandboxed_append_ok(&profile, p))
            .collect();
        let allowed_ok: Vec<bool> = allowed
            .iter()
            .map(|p| sandboxed_append_ok(&profile, p))
            .collect();
        // No LaunchAgents directory yet: planting one is creating it.
        let agents_made = !agents_existed
            && sandboxed_sh_ok(&profile, &format!("mkdir '{}'", launch_agents.display()));
        // CONTROL: the `$HOME` probe under the base profile alone — writable,
        // so its denial above is the write confinement's.
        let control_ok = sandboxed_append_ok(crate::sbpl::BASE_PROFILE, &denied[0]);
        if agents_made {
            let _ = std::fs::remove_dir(&launch_agents);
        }
        // Remove whatever the probes created before asserting.
        for (path, pre) in denied.iter().chain(&allowed).zip(&existed) {
            if !pre {
                let _ = std::fs::remove_file(path);
            }
        }

        assert!(
            control_ok,
            "CONTROL FAILED: {} is not writable even without the write confinement",
            denied[0].display()
        );
        for (path, ok) in denied.iter().zip(&denied_ok) {
            assert!(
                !ok,
                "WRITE CONFINEMENT FAILED: the Containment shell could write {}",
                path.display()
            );
        }
        assert!(
            !agents_made,
            "WRITE CONFINEMENT FAILED: the Containment shell could create {}",
            launch_agents.display()
        );
        for (path, ok) in allowed.iter().zip(&allowed_ok) {
            assert!(
                ok,
                "OVER-BROAD: the Containment shell could not write {}",
                path.display()
            );
        }
    }

    /// A scratch `$HOME` under the test binary's target directory, outside
    /// every [`crate::sbpl::WRITABLE_ROOTS`] entry. `None` (and the proof stops)
    /// where the target directory is itself under a writable root.
    #[cfg(target_os = "macos")]
    fn scratch_home_outside_writable_roots(tag: &str) -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        // `…/target/debug/deps/<test>` → `…/target/debug` (never `deps/`, which
        // is kept small).
        let dir = std::fs::canonicalize(exe.parent()?.parent()?).ok()?;
        if crate::sbpl::WRITABLE_ROOTS
            .iter()
            .any(|root| dir.starts_with(root))
        {
            eprintln!(
                "{} lies under a writable root — skipping the {tag} proof",
                dir.display()
            );
            return None;
        }
        let home = dir.join(format!("aterm-sbpl-home-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir(&home).ok()?;
        Some(home)
    }

    /// Proof 5b (history, over a scratch `$HOME`): the history allowance works
    /// and cannot become a write anywhere else.
    ///
    /// - zsh saves history as at a prompt (lock, append, save-by-copy through
    ///   `.zsh_history.new`) with no error;
    /// - the contained shell cannot make a history name a symlink (to `$HOME`
    ///   or `~/.zshrc`) nor hard-link `~/.zshrc` there;
    /// - a symlink already at a history name carries no write (Seatbelt judges
    ///   its target);
    /// - the next session's profile, over a `~/.zsh_history` pointed at `$HOME`
    ///   from outside, names no part of `$HOME`, and that session cannot write
    ///   `~/.zshrc` or create `~/Library/LaunchAgents`.
    ///
    /// CONTROL: the same `~/.zshrc` append succeeds under the base profile alone.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_history_allowance_widens_nothing() {
        use crate::capability::FsCapability;
        let Some(home) = scratch_home_outside_writable_roots("history") else {
            return;
        };
        let zshrc = home.join(".zshrc");
        std::fs::write(&zshrc, "echo original\n").unwrap();
        let q = |p: &std::path::Path| format!("'{}'", p.display());
        let hist = home.join(".zsh_history");
        let hist_new = home.join(".zsh_history.new");
        let hist_lock = home.join(".zsh_history.LOCK");
        let agents = home.join("Library/LaunchAgents");
        let generate = || crate::sbpl::profile_for_home(home.to_str(), None, FsCapability::TmpOnly);

        // Session 1: the history file is absent, so its allowance is in force.
        let first = generate();
        let zsh = sandbox_wrap(
            &first,
            "/bin/zsh",
            &[
                "-f",
                "-i",
                "-c",
                &format!(
                    "HISTFILE={}; HISTSIZE=10; SAVEHIST=10; print -s aterm-probe; fc -AI; fc -W",
                    q(&hist)
                ),
            ],
        )
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .expect("run zsh under the Containment profile");
        let zsh_stderr = String::from_utf8_lossy(&zsh.stderr).into_owned();
        let saved = std::fs::read_to_string(&hist).unwrap_or_default();
        let plant_home = sandboxed_sh_ok(
            &first,
            &format!("rm -f {h} && ln -s {} {h}", q(&home), h = q(&hist)),
        );
        let plant_zshrc = sandboxed_sh_ok(&first, &format!("ln -s {} {}", q(&zshrc), q(&hist_new)));
        let _ = std::fs::remove_file(&hist_new);
        let hard_link = sandboxed_sh_ok(&first, &format!("ln {} {}", q(&zshrc), q(&hist_lock)));
        let _ = std::fs::remove_file(&hist_lock);
        std::os::unix::fs::symlink(&zshrc, &hist_new).unwrap();
        let through_symlink = sandboxed_sh_ok(&first, &format!("echo PLANTED >> {}", q(&hist_new)));
        let _ = std::fs::remove_file(&hist_new);

        // Session 2: a fresh profile over `~/.zsh_history -> $HOME`.
        let _ = std::fs::remove_file(&hist);
        std::os::unix::fs::symlink(&home, &hist).unwrap();
        let second = generate();
        let zshrc_second = sandboxed_append_ok(&second, &zshrc);
        let agents_second = sandboxed_sh_ok(&second, &format!("mkdir -p {}", q(&agents)));
        let control = sandboxed_append_ok(crate::sbpl::BASE_PROFILE, &zshrc);
        let zshrc_after = std::fs::read_to_string(&zshrc).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&home);

        assert!(
            control,
            "CONTROL FAILED: the scratch ~/.zshrc is not writable without the confinement"
        );
        assert!(
            zsh.status.success() && zsh_stderr.is_empty() && saved.contains("aterm-probe"),
            "OVER-BROAD: zsh could not save its history: rc={:?} stderr={zsh_stderr:?} history={saved:?}",
            zsh.status.code()
        );
        for (made, what) in [
            (plant_home, "made ~/.zsh_history a symlink to $HOME"),
            (plant_zshrc, "made a history name a symlink to ~/.zshrc"),
            (hard_link, "hard-linked ~/.zshrc at a history name"),
            (
                through_symlink,
                "wrote ~/.zshrc through a symlink at a history name",
            ),
        ] {
            assert!(
                !made,
                "WRITE CONFINEMENT FAILED: the contained shell {what}"
            );
        }
        assert_eq!(
            zshrc_after, "echo original\n",
            "~/.zshrc changed under the Containment profile"
        );
        let home_named = format!("\"{}\")", home.display());
        assert!(
            !second.contains(&home_named),
            "WRITE CONFINEMENT FAILED: a history symlink put $HOME in the profile: {second}"
        );
        assert!(
            !zshrc_second,
            "WRITE CONFINEMENT FAILED: after a history symlink the next session could write ~/.zshrc"
        );
        assert!(
            !agents_second,
            "WRITE CONFINEMENT FAILED: after a history symlink the next session could create ~/Library/LaunchAgents"
        );
    }

    /// Proof 6 (login shell): the user's own login shell, rc files and all,
    /// exits 0 under the full Containment profile (`$SHELL -lic`).
    #[cfg(target_os = "macos")]
    #[test]
    fn shell_compat_login_shell_runs_under_containment() {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| s.starts_with('/'))
            .unwrap_or_else(|| "/bin/zsh".to_string());
        let out = sandbox_wrap(
            &containment_profile(),
            &shell,
            &["-l", "-i", "-c", "echo ok"],
        )
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .expect("run the login shell under the Containment profile");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("ok"),
            "SHELL BROKEN: `{shell} -lic` under Containment: rc={:?} stdout={:?} stderr={:?}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    /// The probe child's target, handed down by [`enforcement_proof_task_ports_stay_outside`].
    #[cfg(target_os = "macos")]
    const TASK_PORT_TARGET: &str = "ATERM_TASK_PORT_PROBE_TARGET";

    /// Re-entered by PROOF 7 under a profile: asks for its own control port and
    /// for the target's control, read, inspect and name ports, and prints the
    /// `kern_return_t`s (`self=<c> target=<c>/<r>/<i>/<n>`). Run alone it does
    /// nothing.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "PROOF 7's probe child; runs only when that proof re-enters it"]
    fn task_port_probe_child() {
        type ForPid = unsafe extern "C" fn(
            libc::mach_port_t,
            libc::pid_t,
            *mut libc::mach_port_t,
        ) -> libc::kern_return_t;
        unsafe extern "C" {
            static mach_task_self_: libc::mach_port_t;
            fn task_for_pid(
                target_tport: libc::mach_port_t,
                pid: libc::pid_t,
                task: *mut libc::mach_port_t,
            ) -> libc::kern_return_t;
            fn task_read_for_pid(
                target_tport: libc::mach_port_t,
                pid: libc::pid_t,
                task: *mut libc::mach_port_t,
            ) -> libc::kern_return_t;
            fn task_inspect_for_pid(
                target_tport: libc::mach_port_t,
                pid: libc::pid_t,
                task: *mut libc::mach_port_t,
            ) -> libc::kern_return_t;
            fn task_name_for_pid(
                target_tport: libc::mach_port_t,
                pid: libc::pid_t,
                task: *mut libc::mach_port_t,
            ) -> libc::kern_return_t;
        }
        let Some(target) = std::env::var(TASK_PORT_TARGET)
            .ok()
            .and_then(|pid| pid.parse::<libc::pid_t>().ok())
        else {
            return;
        };
        let ask = |flavor: ForPid, pid: libc::pid_t| {
            let mut port: libc::mach_port_t = 0;
            // SAFETY: `mach_task_self_` is libSystem's cached send right to this
            // task, initialised before `main`; each `task_*_for_pid` writes `port`
            // only on success, and the leaked right dies with this process.
            unsafe { flavor(mach_task_self_, pid, &raw mut port) }
        };
        let flavors: [ForPid; 4] = [
            task_for_pid,
            task_read_for_pid,
            task_inspect_for_pid,
            task_name_for_pid,
        ];
        let target_ports: Vec<String> = flavors
            .iter()
            .map(|&flavor| ask(flavor, target).to_string())
            .collect();
        println!(
            "TASK_PORTS self={} target={}",
            ask(task_for_pid, std::process::id().cast_signed()),
            target_ports.join("/")
        );
    }

    /// PROOF 7 (TASK PORTS, FULL generated Containment profile) — a contained
    /// process cannot take any task port — control, read, inspect or name — of a
    /// process outside the sandbox (the `lldb`/`debugserver` escape, silent once
    /// macOS developer mode is on), and keeps its own. The target is an ad-hoc copy of `/bin/sleep` signed with
    /// `get-task-allow`, the one kind of target an unentitled requester is granted
    /// with no prompt; the requester is this test binary, which carries no
    /// debugger entitlement, so no path here is meant to raise the administrator
    /// dialog — but one did: on 2026-09-30, on a machine with developer mode off,
    /// the probe sat under an open SecurityAgent prompt until it was killed. So
    /// the probe is bounded, and a prompt fails this proof by name.
    /// CONTROL: under `(allow default)` alone the same probe IS granted the
    /// target's port, so the denial is the profile's.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_task_ports_stay_outside() {
        // Re-signing a copy of an Apple binary waits on trustd.
        if aterm_uds::deadline::trust_service_silent() {
            return;
        }
        struct Target(aterm_uds::spawnfd::Child);
        impl Drop for Target {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let sleeper = dir.path().join("sleep");
        let entitlements = dir.path().join("get-task-allow.plist");
        std::fs::copy("/bin/sleep", &sleeper).expect("copy /bin/sleep");
        std::fs::write(
            &entitlements,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\"><dict>\
             <key>com.apple.security.get-task-allow</key><true/></dict></plist>\n",
        )
        .expect("write the entitlements");
        // Under a hang detector: a codesign macOS stops answering mid-run fails
        // this test in a minute instead of hanging the suite.
        let signed = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command("/usr/bin/codesign")
                .args(["--force", "--sign", "-", "--entitlements"])
                .arg(&entitlements)
                .arg(&sleeper),
            std::time::Instant::now() + aterm_uds::deadline::CODESIGN_HANG,
        )
        .expect("run codesign");
        assert!(
            signed.status.success(),
            "ad-hoc signing the target failed: {}",
            String::from_utf8_lossy(&signed.stderr)
        );
        let target = Target(
            aterm_uds::spawnfd::command(&sleeper)
                .arg("60")
                .spawn()
                .expect("start the target"),
        );
        let exe = std::env::current_exe().expect("this test binary");
        let probe = |profile: &str| {
            let exe = exe.to_str().expect("utf8 test binary path");
            let mut child = sandbox_wrap(
                profile,
                exe,
                &[
                    "--exact",
                    "actuator::tests::task_port_probe_child",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ],
            )
            .env(TASK_PORT_TARGET, target.0.id().to_string())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .expect("run the probe child");
            // BOUNDED. A `task_for_pid` that macOS answers with the
            // administrator dialog blocks until a person answers it, and an
            // unattended gate has nobody: measured 2026-09-30 on a machine with
            // developer mode off, the child sat at 0% CPU under an open
            // SecurityAgent prompt for 1h37m and held `tools/verify.sh` with it.
            // A probe that has not answered in PROBE_DEADLINE fails, naming
            // that cause, instead of waiting for ever. A hang detector, never a
            // latency budget: an answered probe ends in milliseconds.
            const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);
            let started = std::time::Instant::now();
            while child.try_wait().expect("poll the probe child").is_none() {
                if started.elapsed() >= PROBE_DEADLINE {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "the probe child under {profile:?} did not answer in {PROBE_DEADLINE:?}: \
                         its task_for_pid is most likely waiting on the macOS administrator \
                         dialog (developer mode off; see `DevToolsSecurity -status`), which \
                         this proof's premise says cannot be raised — a finding about this \
                         machine, not a pass"
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let out = child.wait_with_output().expect("collect the probe child");
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            // `--nocapture` puts the reading on the harness's own `test … ` line.
            let ports = stdout
                .lines()
                .find_map(|line| line.split_once("TASK_PORTS "))
                .map(|(_, ports)| ports.trim().to_string());
            ports.unwrap_or_else(|| {
                panic!(
                    "no TASK_PORTS line from the probe child; stdout={stdout:?} stderr={:?}",
                    String::from_utf8_lossy(&out.stderr)
                )
            })
        };

        let control = probe("(version 1)(allow default)");
        assert_eq!(
            control, "self=0 target=0/0/0/0",
            "CONTROL: an unsandboxed-equivalent probe must be granted every port, \
             or the denial below proves nothing"
        );
        let contained = probe(&containment_profile());
        let refused_all = contained
            .strip_prefix("self=0 target=")
            .is_some_and(|target| target.split('/').all(|kr| kr != "0"));
        assert!(
            refused_all,
            "DENY FAILED: under the Containment profile the probe must keep its own \
             task port and be refused every flavor of the outside target's; got {contained:?}"
        );
    }

    /// What tells [`launch_broker_probe_child`] to probe.
    #[cfg(target_os = "macos")]
    const BROKER_PROBE: &str = "ATERM_LAUNCH_BROKER_PROBE";

    /// Re-entered by PROOF 8 under a profile: asks bootstrap for a send right to
    /// each of [`crate::sbpl::DENIED_LAUNCH_BROKERS`] and prints the
    /// `kern_return_t`s (`BROKERS <kr>/<kr>/…`). It only looks the names up; it
    /// never sends a message, so nothing is launched. Run alone it does nothing.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "PROOF 8's probe child; runs only when that proof re-enters it"]
    fn launch_broker_probe_child() {
        unsafe extern "C" {
            static bootstrap_port: libc::mach_port_t;
            static mach_task_self_: libc::mach_port_t;
            fn bootstrap_look_up(
                bp: libc::mach_port_t,
                service_name: *const libc::c_char,
                sp: *mut libc::mach_port_t,
            ) -> libc::kern_return_t;
            fn mach_port_deallocate(
                task: libc::mach_port_t,
                name: libc::mach_port_t,
            ) -> libc::kern_return_t;
        }
        if std::env::var_os(BROKER_PROBE).is_none() {
            return;
        }
        let codes: Vec<String> = crate::sbpl::DENIED_LAUNCH_BROKERS
            .iter()
            .map(|name| {
                let name = std::ffi::CString::new(*name).expect("a service name has no NUL");
                let mut port: libc::mach_port_t = 0;
                // SAFETY: `bootstrap_port` is libSystem's bootstrap send right,
                // initialised before `main`; `bootstrap_look_up` reads the C
                // string and writes `port` only on success.
                let kr = unsafe { bootstrap_look_up(bootstrap_port, name.as_ptr(), &raw mut port) };
                if port != 0 {
                    // SAFETY: `port` is a send right this call just received.
                    unsafe { mach_port_deallocate(mach_task_self_, port) };
                }
                kr.to_string()
            })
            .collect();
        println!("BROKERS {}", codes.join("/"));
    }

    /// PROOF 8 (LAUNCH BROKERS, FULL generated Containment profile) — a contained
    /// process gets no port to Launch Services, the Apple Events daemon or the
    /// `ServiceManagement` daemon, each of which starts, drives or registers a
    /// process that does not inherit the sandbox (`open` of a `.command` file, a
    /// scripted app, a login item). CONTROL: under `(allow default)` alone every
    /// lookup succeeds, so the refusal is the profile's.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_launch_brokers_denied() {
        let exe = std::env::current_exe().expect("this test binary");
        let probe = |profile: &str| {
            let exe = exe.to_str().expect("utf8 test binary path");
            let mut cmd = sandbox_wrap(
                profile,
                exe,
                &[
                    "--exact",
                    "actuator::tests::launch_broker_probe_child",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ],
            );
            // A lookup answers at once and asks nobody anything (unlike PROOF 7's
            // `task_for_pid`, which can wait on the administrator dialog).
            let out = cmd
                .env(BROKER_PROBE, "1")
                .output()
                .expect("run the probe child");
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            stdout
                .lines()
                .find_map(|line| line.split_once("BROKERS "))
                .map(|(_, codes)| codes.trim().to_string())
                .unwrap_or_else(|| {
                    panic!(
                        "no BROKERS line from the probe child; stdout={stdout:?} stderr={:?}",
                        String::from_utf8_lossy(&out.stderr)
                    )
                })
        };

        let control = probe("(version 1)(allow default)");
        assert!(
            control.split('/').all(|kr| kr == "0"),
            "CONTROL: an unsandboxed-equivalent probe must reach every launch broker, \
             or the denial below proves nothing; got {control:?}"
        );
        let contained = probe(&containment_profile());
        assert!(
            contained.split('/').all(|kr| kr != "0"),
            "DENY FAILED: under the Containment profile every launch-broker lookup must be \
             refused; got {contained:?} for {:?}",
            crate::sbpl::DENIED_LAUNCH_BROKERS
        );
    }

    /// What tells [`preference_write_probe_child`] to probe.
    #[cfg(target_os = "macos")]
    const PREFERENCE_PROBE: &str = "ATERM_PREFERENCE_WRITE_PROBE";

    /// Re-entered by PROOF 9 under a profile: asks the sandbox, for this
    /// process, whether it may write a user preference domain — the question
    /// the preferences daemon asks before it writes `~/Library/Preferences` on
    /// a caller's behalf — and prints the answer (`PREFERENCE_WRITE <rc>`, 0 =
    /// allowed). It only asks; no preference is written. Run alone it does
    /// nothing.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "PROOF 9's probe child; runs only when that proof re-enters it"]
    fn preference_write_probe_child() {
        /// `SANDBOX_FILTER_PREFERENCE_DOMAIN` from `<sandbox/private.h>`.
        const SANDBOX_FILTER_PREFERENCE_DOMAIN: libc::c_int = 6;
        unsafe extern "C" {
            fn sandbox_check(
                pid: libc::pid_t,
                operation: *const libc::c_char,
                filter: libc::c_int,
                ...
            ) -> libc::c_int;
        }
        if std::env::var_os(PREFERENCE_PROBE).is_none() {
            return;
        }
        // SAFETY: both strings are NUL-terminated literals; `sandbox_check`
        // reads them and the pid, and writes nothing.
        let rc = unsafe {
            sandbox_check(
                std::process::id().cast_signed(),
                c"user-preference-write".as_ptr(),
                SANDBOX_FILTER_PREFERENCE_DOMAIN,
                c"com.example.aterm-containment-probe".as_ptr(),
            )
        };
        println!("PREFERENCE_WRITE {rc}");
    }

    /// PROOF 9 (PREFERENCE WRITES, FULL generated Containment profile) — the
    /// preferences daemon writes `~/Library/Preferences` outside the sandbox for
    /// whoever asks, so the file-write confinement alone left every preference
    /// domain writable from inside. Under the profile the sandbox must refuse
    /// `user-preference-write`. CONTROL: under the base profile alone (no write
    /// confinement) the same question is answered "allowed", so the refusal is
    /// the confinement's.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_preference_writes_denied() {
        let exe = std::env::current_exe().expect("this test binary");
        let probe = |profile: &str| {
            let exe = exe.to_str().expect("utf8 test binary path");
            let out = sandbox_wrap(
                profile,
                exe,
                &[
                    "--exact",
                    "actuator::tests::preference_write_probe_child",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ],
            )
            .env(PREFERENCE_PROBE, "1")
            .output()
            .expect("run the probe child");
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            stdout
                .lines()
                .find_map(|line| line.split_once("PREFERENCE_WRITE "))
                .map(|(_, rc)| rc.trim().to_string())
                .unwrap_or_else(|| {
                    panic!(
                        "no PREFERENCE_WRITE line from the probe child; stdout={stdout:?} stderr={:?}",
                        String::from_utf8_lossy(&out.stderr)
                    )
                })
        };

        let control = probe(crate::sbpl::BASE_PROFILE);
        assert_eq!(
            control, "0",
            "CONTROL: without the write confinement a preference write must be allowed, \
             or the denial below proves nothing"
        );
        let contained = probe(&containment_profile());
        assert_ne!(
            contained, "0",
            "WRITE CONFINEMENT FAILED: under the Containment profile the sandbox allows \
             user-preference-write, so the preferences daemon writes ~/Library/Preferences \
             for the contained shell"
        );
    }

    /// The wrapper binary must exist for the macOS actuation to be real. If it is
    /// ever missing, the actuator's macOS `os_sandbox_actuated()==true` claim is
    /// unbacked and the launcher's fail-closed path (refuse to spawn) is the one
    /// that must trigger. This test documents the precondition the fail-closed
    /// path defends.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_exec_wrapper_is_present_on_this_macos() {
        assert!(
            std::path::Path::new(crate::sbpl::SANDBOX_EXEC_PATH).exists(),
            "{} must exist for the macOS OS-network-sandbox to actuate; if it is \
             missing the launcher MUST fail closed (refuse to spawn) rather than \
             run an unsandboxed Containment shell",
            crate::sbpl::SANDBOX_EXEC_PATH,
        );
    }
}
