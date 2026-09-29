// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Spawn-seam containment actuator — the bridge from the policy DATA MODEL
//! (mode → [`Capabilities`](crate::Capabilities)) to a real, logged decision at
//! the one place aterm forks a child shell.
//!
//! ## What is enforced (`ATERM_DESIGN` §0.1 / §5.6) — the one scope statement
//!
//! The rest of this crate maps a [`ContainmentMode`] to a
//! [`Capabilities`](crate::Capabilities) set; that mapping enforces nothing by
//! itself. This module turns it into a [`SpawnDecision`] that both launchers
//! (`aterm` and the window) consult BEFORE handing the PTY seam a spawn
//! capability, and audits it. Per mode:
//!
//! | Mode | Enforced |
//! |---|---|
//! | Master, User | Nothing beyond the capability-gated spawn; the shell keeps aterm's own resource limits. |
//! | Safety | Hardened resource limits (`aterm-sandbox`: rlimits on Unix, the Job Object on Windows). No OS sandbox, on any platform. |
//! | Containment | The hardened limits PLUS the OS sandbox below — and on a platform without one, NO SHELL (see "Fail closed"). |
//!
//! **The Containment OS sandbox (macOS Seatbelt).** The spawn is wrapped with
//! `/usr/bin/sandbox-exec -p <SBPL>`, the profile from [`crate::sbpl::profile_for`]
//! (its module doc lists the rules in order). The kernel then, for the child shell
//! and everything it runs:
//!
//! - denies ALL network (`(deny network*)`);
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
//! Other READS stay allowed so a normal `$SHELL` works (dyld, `path_helper`, the rc
//! files, `/dev/tty`): a deny-by-default READ allowlist breaks the shell and is
//! not pursued. Every rule is proven against the live kernel by the enforcement
//! tests in this module. [`SpawnDecision::Permit::sbpl`] carries the per-user
//! profile; the PTY seam refuses to spawn if the wrapper binary is missing.
//!
//! **Fail closed.** Decided by the owner 2026-09-25: `Containment` NEVER degrades
//! to a weaker posture. Where [`os_sandbox_actuated`] is false — Linux, Windows,
//! every non-macOS target — [`decide`] returns [`SpawnDecision::Deny`] naming the
//! platform gap, and both launchers exit without starting a shell. That refusal
//! is the product off macOS: Containment is macOS-only by scope (decided
//! 2026-09-27 under the owner's standing direction; `docs/ATERM_DESIGN.md` §5.6
//! — Landlock/seccomp are not the launch target).

use crate::audit::{log_denial, log_posture};
use crate::capability::{NetworkCapability, ProcessCapability};
use crate::mode::ContainmentMode;
use crate::policy::ContainmentPolicy;

/// Audit subsystem label for spawn-seam containment events.
const SUBSYSTEM: &str = "spawn";

/// Why [`decide`] refuses a `Containment` spawn on a platform with no OS sandbox.
/// Both launchers print it verbatim.
pub const NO_OS_SANDBOX_REASON: &str = "the sandbox exists only on macOS, so no shell was started";

/// Whether THIS BUILD/PLATFORM can actuate a real OS sandbox at the spawn seam:
/// `true` only on macOS (Seatbelt via `sandbox-exec`). What that sandbox enforces
/// is the module doc's scope statement; where this is `false`, `Containment`
/// spawns are refused. Use [`network_sandbox_actuated`] for the per-mode answer.
#[must_use]
pub const fn os_sandbox_actuated() -> bool {
    cfg!(target_os = "macos")
}

/// Whether the OS NETWORK sandbox is actually in force for a spawn in `mode`.
///
/// `true` iff this build/platform can actuate ([`os_sandbox_actuated`]) AND the
/// mode's network policy is [`None`](crate::NetworkCapability::None) (i.e.
/// `Containment` — the only mode that denies network). For every other mode the
/// policy permits network, so no sandbox is applied and this is `false`.
///
/// This is the truthful per-spawn statement the audit log and the GUI use: it is
/// `true` ONLY for the spawn that is genuinely wrapped in `sandbox-exec` with the
/// network-deny profile — never flipped optimistically.
#[must_use]
pub fn network_sandbox_actuated(mode: ContainmentMode) -> bool {
    os_sandbox_actuated() && ContainmentPolicy::network(mode) == NetworkCapability::None
}

/// The actuated decision for the single spawn seam, given a containment mode.
///
/// Not `Copy`: the `Permit` variant carries an owned, per-user `sbpl` `String`
/// (the profile embeds the canonicalized `$HOME` paths, so it is not a
/// `&'static`). It stays `Clone`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpawnDecision {
    /// Spawning the initial shell is permitted.
    ///
    /// `os_sandbox` records whether a real OS sandbox backs this spawn (see
    /// [`network_sandbox_actuated`]): `true` only for a `Containment` spawn on
    /// macOS, where the launcher MUST wrap the child in `sandbox-exec` with `sbpl`.
    /// When `false`, an explicit audit line was logged so an OS-unconfined posture
    /// is never silent.
    ///
    /// `sbpl`, when `Some`, is the per-user Seatbelt profile string the launcher
    /// passes to `/usr/bin/sandbox-exec -p <sbpl>`. It is `Some` exactly when
    /// `os_sandbox` is `true`. The launcher fails CLOSED if it cannot apply a
    /// `Some(sbpl)` (e.g. the wrapper binary is missing): it must NOT spawn an
    /// unsandboxed shell when the policy demands the sandbox.
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

/// Decide — and AUDIT — whether the single PTY spawn seam may run for `mode`,
/// and (for `Containment`) how the OS sandbox backs it.
///
/// This is the seam wiring required by `ATERM_DESIGN` §5.6: the spawn is gated on
/// the containment decision and the chosen mode is logged. For `Containment` on
/// macOS the returned [`SpawnDecision::Permit`] carries `os_sandbox: true` and the
/// per-user SBPL profile the launcher MUST apply via `sandbox-exec`. For
/// `Containment` anywhere else it is [`SpawnDecision::Deny`] with
/// [`NO_OS_SANDBOX_REASON`] — never a silently weaker shell. For every other mode
/// `os_sandbox` is `false`, `sbpl` is `None`, and an audit line records that no OS
/// sandbox is in force.
///
/// The initial interactive shell is otherwise permitted in every mode (including
/// `Containment`/`NoFork`, whose contract is "exec only, for the initial
/// shell"). A hypothetical future `ProcessCapability` below `NoFork` would
/// fail closed via [`SpawnDecision::Deny`].
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
    // Containment demands the OS sandbox. Where the platform has none, refuse
    // the spawn outright — the owner's fail-closed ruling (2026-09-25).
    if denies_network && !sandbox_available {
        log_denial(SUBSYSTEM, "spawn initial shell", mode, NO_OS_SANDBOX_REASON);
        return SpawnDecision::Deny {
            mode,
            reason: NO_OS_SANDBOX_REASON,
        };
    }
    // Resolve the OS sandbox posture for this mode. `os_sandbox` is true ONLY for a
    // Containment spawn on a platform that can actuate (macOS); `sbpl` is the
    // per-user profile in that case and `None` otherwise. The two are kept in
    // lockstep (sbpl.is_some() == os_sandbox).
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

    // Audit the OS-sandbox posture for the chosen mode through the containment
    // audit target so operators see one stream. These are POSTURE records, NOT
    // denials — they use `log_posture` (no `DENIED:` prefix) so a denial-stream
    // filter never miscounts a permitted/actuated spawn as a security denial.
    if os_sandbox {
        log_posture(
            SUBSYSTEM,
            "os-network-sandbox",
            mode,
            applied_posture(home_scoped),
        );
    } else {
        // Explicit, non-silent record that no OS sandbox is in force for this mode
        // (its policy permits network). Safety's hardened limits apply as rlimits
        // on Unix and through the Job Object on Windows
        // (`aterm_sandbox::Limits::shell_default`); Master and User keep
        // aterm's own (`Limits::inherit`, chosen by both launchers) — the
        // launching shell's for a session, launchd's for the window — so their
        // record claims no limits.
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
        // Every currently-defined capability permits the INITIAL shell.
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

/// What an applied sandbox enforces, from what its profile holds (pass
/// [`SpawnDecision::Permit`]'s `home_scoped`): the shell-history allowance and
/// the credential and private-data read denies are joined onto `$HOME`, so a
/// home that did not resolve gets neither, and a `$HOME` other than the
/// account's home leaves that home's stores readable. The aterm.log posture
/// record and the window's `--verbose` line both print this one wording.
#[must_use]
pub fn applied_posture(home_scoped: bool) -> &'static str {
    if home_scoped {
        "OS sandbox applied: no network; writes only to temp dirs and shell history; no access to credential stores or private data under $HOME"
    } else {
        "OS sandbox applied: no network; writes only to temp dirs; $HOME did not resolve, so credential stores and private data can still be read"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_sandbox_actuation_matches_platform() {
        // The build/platform capability: macOS can actuate (sandbox-exec), other
        // platforms cannot. This is the truthful platform claim — NOT a per-spawn
        // claim (that is `network_sandbox_actuated`).
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
        // The owner's ruling (2026-09-25): on a platform with no OS sandbox the
        // Containment spawn is REFUSED, naming the gap — never a weaker shell.
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
        // the profile begins with the network deny. Everything else:
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
                            profile.starts_with(crate::sbpl::NETWORK_DENY_PROFILE),
                            "{mode} sbpl must begin with the network deny; got {profile}"
                        );
                    }
                }
                other => panic!("{mode} must Permit, got {other:?}"),
            }
        }
    }

    #[test]
    fn non_containment_spawn_is_never_os_sandboxed() {
        // The byte-identical-spawn guarantee at the decision layer: User (the
        // default interactive mode), Safety, and Master must ALL come back with
        // os_sandbox=false and sbpl=None, so the launcher applies no sandbox-exec
        // wrap and the spawn is exactly as before.
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

    // ===================================================================
    // ENFORCEMENT PROOF (macOS) — the whole point of the actuation.
    //
    // These tests spawn a probe via the EXACT same `sandbox-exec -p <sbpl> <prog>
    // <args>` wrapping the actuator/launcher uses, attempt an operation the
    // profile denies, and assert it FAILS — proving the Seatbelt sandbox actually
    // enforces on this box. Without these passing, `os_sandbox_actuated()` must
    // NOT report macOS as actuating.
    // ===================================================================

    /// Build the sandbox-exec argv the launcher builds: `sandbox-exec -p <sbpl>
    /// <prog> <args...>`. Mirrors the wrap done in `aterm-pty::spawn_shell`.
    #[cfg(target_os = "macos")]
    fn sandbox_wrap(sbpl: &str, prog: &str, args: &[&str]) -> std::process::Command {
        let mut cmd = std::process::Command::new(crate::sbpl::SANDBOX_EXEC_PATH);
        cmd.arg("-p").arg(sbpl).arg(prog).args(args);
        cmd
    }

    /// PROOF 1 (network) — HERMETIC, no external network. A loopback TCP listener
    /// is bound in this (parent) test process; a probe (`/usr/bin/nc`) tries to
    /// connect to it. WITHOUT the sandbox the connect SUCCEEDS; with the SAME
    /// `sandbox-exec -p '(deny network*)'` wrap the actuator emits, the connect
    /// FAILS. The differential against the one live listener — only the sandbox
    /// wrap changed — is the proof Seatbelt denies network on this box.
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
        // The full per-user profile begins with the exact network deny (it may also
        // carry the secret-dir denies); the network deny is what THIS proof tests.
        assert!(profile.starts_with(crate::sbpl::NETWORK_DENY_PROFILE));

        // Loopback listener in the PARENT. An accept thread keeps draining so the
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

        // CONTROL: no sandbox → the loopback connect SUCCEEDS (rc == 0). This
        // proves the probe + listener work, so a failure under the sandbox is
        // attributable to the sandbox, not a broken probe.
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

        // SANDBOXED: same probe, same listener, wrapped in the actuator's
        // `sandbox-exec -p '(deny network*)'` → the connect MUST FAIL.
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

        // Tidy up the accept thread (a connect we never made would block it on
        // the 2nd accept; drop the listener by letting the thread time-bound
        // itself — it returns after the control connect's single accept and one
        // more accept attempt which the OS may immediately error once we exit).
        // We do not join unconditionally to avoid a hang if the 2nd accept blocks;
        // instead make a throwaway connect to unblock it, then join.
        let _ = std::net::TcpStream::connect(("127.0.0.1", port));
        let _ = accepter.join();
    }

    /// PROOF 2 (explicit EPERM string) — the same `sandbox-exec -p <sbpl> <prog>`
    /// wrap, but with a file-read-deny profile over a real temp file, so the
    /// kernel's denial surfaces as the exact "Operation not permitted" (EPERM)
    /// text. This nails the enforcement SEMANTICS (it is a permission denial, not
    /// some unrelated failure) using the same wrapping mechanism. NOTE: the path
    /// is CANONICALIZED — `/tmp` is a symlink to `/private/tmp` on macOS and
    /// Seatbelt matches the canonical path, so a non-canonical literal would NOT
    /// match (a footgun this test deliberately avoids and documents).
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_file_read_deny_yields_operation_not_permitted() {
        use std::io::Write;

        // A real temp file with known contents, then its CANONICAL path.
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

    /// PROOF 3 (SHELL-COMPAT, FULL generated Containment profile) — the whole point
    /// of `(allow default)` + targeted denies: a NORMAL shell must keep working.
    /// We take the EXACT profile the actuator hands the launcher for `Containment`
    /// (network deny + the canonicalized secret-dir denies, scoped under the real
    /// `$HOME`) and run an ordinary command pipeline under it. It MUST exit 0 with
    /// the expected output — i.e. the secret-scoping did NOT break the shell.
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
        // It is the network deny PLUS a file deny — never a blanket FS deny.
        assert!(profile.starts_with(crate::sbpl::NETWORK_DENY_PROFILE));

        // Ordinary, non-secret operations: echo, pwd, a directory listing, and a
        // read of a normal system file that exists on macOS. None of these touch a
        // secret path, so all must succeed under the full Containment profile.
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

    /// PROOF 4 (SECRET-DENY + TARGETED scope, FULL generated Containment profile) —
    /// the security payload. Under the EXACT actuator profile for `Containment`:
    ///   (a) a probe file planted inside a denied secret dir (`$HOME/.aws`) is NOT
    ///       readable — `cat` fails with EPERM ("Operation not permitted"); AND
    ///   (b) a NON-secret file (`/etc/hosts`) IS still readable (exit 0).
    /// (a)+(b) together prove the deny is TARGETED at the secret set, not a blanket
    /// filesystem deny. The probe is created under a real secret dir and cleaned up.
    #[cfg(target_os = "macos")]
    #[test]
    fn secret_deny_full_containment_profile_blocks_secret_but_allows_nonsecret() {
        use std::io::Write;

        // Need a real, resolvable $HOME for the profile to scope under. If $HOME is
        // somehow unset in the test env, the profile is network-only and this proof
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

        // (a) SANDBOXED read of the secret probe → MUST be denied (EPERM).
        let denied = sandbox_wrap(&profile, "/bin/cat", &[secret_probe_s.as_str()])
            .output()
            .expect("run cat on secret probe under Containment profile");
        let denied_stderr = String::from_utf8_lossy(&denied.stderr).into_owned();
        let denied_stdout = String::from_utf8_lossy(&denied.stdout).into_owned();

        // (b) SANDBOXED read of a NON-secret file (/etc/hosts) → MUST succeed.
        let allowed = sandbox_wrap(&profile, "/bin/cat", &["/etc/hosts"])
            .output()
            .expect("run cat on /etc/hosts under Containment profile");
        let allowed_ok = allowed.status.success();

        // Clean up the probe (and the dir if we created it) BEFORE asserting, so a
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
        // (b) non-secret still allowed → the scope is TARGETED, not blanket.
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
    /// would have created is removed. CONTROL: under the network deny alone the
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
        // CONTROL: the `$HOME` probe under the network deny alone — writable,
        // so its denial above is the write confinement's.
        let control_ok = sandboxed_append_ok(crate::sbpl::NETWORK_DENY_PROFILE, &denied[0]);
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

    /// A scratch `$HOME` for a live proof that must plant files: under the test
    /// binary's target directory, which lies outside every
    /// [`crate::sbpl::WRITABLE_ROOTS`] entry on a normal checkout. `None` (the
    /// proof says so and stops) where the target directory is itself under a
    /// writable root, since every write there is allowed and the proof would
    /// prove nothing.
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

    /// PROOF 5b (HISTORY, FULL generated Containment profile over a scratch
    /// `$HOME`) — the history allowance works, and cannot be turned into a write
    /// anywhere else.
    ///
    /// - zsh saves its history there as it does at a prompt (lock, append, and a
    ///   save-by-copy through `.zsh_history.new`) with no error;
    /// - the contained shell cannot make a history name a symlink — neither to
    ///   `$HOME` nor to `~/.zshrc` — nor hard-link `~/.zshrc` there;
    /// - a symlink already at a history name carries no write: Seatbelt judges
    ///   the file it resolves to, which is not allowed;
    /// - the next session's profile, generated over a `~/.zsh_history` pointed
    ///   at `$HOME` (planted from outside, since the session may not), names no
    ///   part of `$HOME`, and that session cannot write `~/.zshrc` or create
    ///   `~/Library/LaunchAgents`.
    ///
    /// CONTROL: the same `~/.zshrc` append succeeds under the network deny alone.
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
        .stdin(std::process::Stdio::null())
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
        let control = sandboxed_append_ok(crate::sbpl::NETWORK_DENY_PROFILE, &zshrc);
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

    /// PROOF 6 (LOGIN SHELL) — the user's own login shell, rc files and all,
    /// starts and exits 0 under the full Containment profile: the write
    /// confinement and the denies do not break `$SHELL -lic`.
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
        .stdin(std::process::Stdio::null())
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
