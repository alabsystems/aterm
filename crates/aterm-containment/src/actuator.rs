// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Spawn-seam containment actuator — the bridge from the policy DATA MODEL
//! (mode → [`Capabilities`](crate::Capabilities)) to a real, logged decision at
//! the one place aterm forks a child shell.
//!
//! ## What this is, honestly (`ATERM_DESIGN` §0.1 / §5.6)
//!
//! The rest of this crate is a *policy data model*: it maps a [`ContainmentMode`]
//! to a [`Capabilities`](crate::Capabilities) set whose monotonicity/non-escalation
//! is encoded as Kani proof harnesses (opt-in; a TLA+ model is planned but not yet
//! in-tree). That is a property of THAT MAPPING only — it does NOT, by itself, make
//! the operating system enforce anything; before this module nothing consulted it
//! at the spawn seam.
//!
//! This module is the actuation seam. Given the resolved mode it produces a
//! [`SpawnDecision`] that the GUI launcher consults BEFORE handing the PTY seam a
//! spawn capability. What is actuated TODAY:
//!
//! 1. **Process-capability gate (actuated).** The [`ProcessCapability`] for the
//!    mode is checked. `Full`/`Restricted`/`NoFork` all permit the *initial*
//!    interactive shell (`NoFork` means "no fork — exec only, for the initial
//!    shell"), so a normal `$SHELL` still spawns; but the decision, the mode, and
//!    the fact that it is permitted are recorded via the containment audit log.
//! 2. **Resource limits (actuated, elsewhere).** `aterm-sandbox` installs
//!    `setrlimit` bounds in the child before exec, fail-closed (`aterm-pty`).
//! 3. **OS NETWORK + SECRET-FS + PRIVATE-DATA sandbox (actuated).** In
//!    `Containment` mode (network policy = [`None`](crate::NetworkCapability::None),
//!    FS policy = [`TmpOnly`](crate::FsCapability::TmpOnly)) on macOS, the spawn is
//!    wrapped with `/usr/bin/sandbox-exec -p <SBPL>` applying the profile from
//!    [`crate::sbpl::profile_for`] — `(version 1)(allow default)(deny network*)`
//!    PLUS a conservative `(deny file-read* file-write* …)` over (a) a small fixed
//!    set of SECRET/credential stores under `$HOME` (`.ssh`, `.aws`, `.gnupg`,
//!    `.config/gh`, `.config/aterm`, `.netrc`, …) and (b) a fixed set of
//!    PRIVATE-USER-DATA stores (`Documents`, `Desktop`, `Downloads`, media, and the
//!    local Mail / Messages / keychain / cookies / browser-profile databases — see
//!    [`crate::sbpl::PRIVATE_SUBDIRS`]). The kernel Seatbelt then DENIES all network
//!    access AND read/write of those credential and private-data stores to the child
//!    shell and everything it runs — while the rest of the filesystem (`~/.zshrc`,
//!    dyld, `/dev/tty`, the tool caches/preferences under `~/Library`, …) stays
//!    available so a normal `$SHELL` works. [`SpawnDecision::Permit::sbpl`] carries
//!    the per-user SBPL profile so the launcher can build that wrap; the GUI fails
//!    CLOSED if the wrapper is missing (it refuses to spawn an unsandboxed shell when
//!    the policy demands the sandbox). [`os_sandbox_actuated`] is the build/platform
//!    capability check; [`network_sandbox_actuated`] reports whether the sandbox is
//!    in force for a given mode.
//!
//! What is **NOT** actuated yet (honest, deferred):
//!
//! - **GENERAL OS FILESYSTEM scoping (macOS Seatbelt `file-*` / Endpoint
//!   Security).** Beyond the conservative SECRET + PRIVATE-DATA sets above, the
//!   profile is deliberately `(allow default)` for the filesystem: a blanket
//!   `(deny file-*)` base tight enough to be meaningful also breaks a normal
//!   `$SHELL` (dyld, `path_helper`, the user's rc, `/dev/tty`). Turning the policy
//!   inside-out into a positive per-[`FsCapability`](crate::FsCapability) allowlist
//!   (deny-by-default, allow the specific roots a tier needs) is an explicit
//!   FOLLOW-UP. The audit log and [`os_sandbox_actuated`]/[`network_sandbox_actuated`]
//!   say exactly this — network: enforced; secret-dir + private-data read/write:
//!   enforced; general filesystem: not yet scoped.
//! - **Non-macOS platforms.** `sandbox-exec` is macOS-only; on other targets
//!   [`os_sandbox_actuated`] is `false` and `Containment` falls back to the
//!   rlimit + process-cap posture with an explicit audit line (a Linux
//!   seccomp/Landlock lane is the follow-up there).
//!
//! The policy model's intended formal spec is a `tla/Containment.tla` model
//! (planned, NOT yet in-tree; the in-tree checks are the Kani harnesses); see
//! `ATERM_DESIGN` §5.6.

use crate::audit::{log_denial, log_posture};
use crate::capability::{NetworkCapability, ProcessCapability};
use crate::mode::ContainmentMode;
use crate::policy::ContainmentPolicy;

/// Audit subsystem label for spawn-seam containment events.
const SUBSYSTEM: &str = "spawn";

/// Whether THIS BUILD/PLATFORM can actuate a real OS sandbox at the spawn seam.
///
/// `true` only on macOS, where the spawn seam wraps a network-denied
/// (`Containment`) spawn with `/usr/bin/sandbox-exec` applying the Seatbelt profile
/// from [`crate::sbpl::profile_for`], and the kernel enforces `(deny network*)`
/// plus the conservative secret-dir `(deny file-read* file-write* …)` on the child
/// (verified by the enforcement-proof tests in this module). `false` on every other
/// platform — there `sandbox-exec` does not exist and `Containment` falls back to
/// the rlimit + process-cap posture (a seccomp/Landlock lane is the follow-up).
///
/// IMPORTANT, honest scope: even when `true`, only the **network**, a
/// **conservative SECRET-directory set** (`~/.ssh`, `~/.aws`, `~/.gnupg`,
/// `~/.config/gh`, `~/.config/aterm`, `~/.netrc`, …) and a **fixed
/// PRIVATE-USER-DATA set** (`~/Documents`, `~/Downloads`, media, the local
/// Mail/Messages/keychain/cookies/browser-profile stores) are OS-enforced. The
/// GENERAL filesystem is NOT scoped (the profile is `(allow default)` for the rest
/// of the filesystem so a normal shell works) — that is an explicit follow-up.
/// Callers must not read this as full "filesystem isolation". Use
/// [`network_sandbox_actuated`] to know whether the sandbox is in force for a
/// particular mode.
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
/// (the profile embeds the canonicalized `$HOME` secret paths, so it is no longer a
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
    /// passes to `/usr/bin/sandbox-exec -p <sbpl>` to actuate the network deny AND
    /// the conservative secret-directory deny. It is `Some` exactly when
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
    },
    /// Spawning is denied for this mode. A denial was logged to the containment
    /// audit trail. (No current mode reaches this — the initial shell is allowed
    /// in every mode, `NoFork` included — but the variant exists so a future,
    /// stricter policy denies fail-closed rather than silently permitting.)
    Deny {
        /// The mode this decision was made for.
        mode: ContainmentMode,
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
/// and (for `Containment`) whether/how the OS network sandbox backs it.
///
/// This is the seam wiring required by `ATERM_DESIGN` §5.6: the spawn is gated on
/// the containment decision and the chosen mode is logged. For `Containment` mode
/// on macOS the returned [`SpawnDecision::Permit`] carries `os_sandbox: true` and
/// the per-user SBPL profile (`sbpl: Some(...)`) the launcher MUST apply via
/// `sandbox-exec` to deny network AND the conservative secret-directory set at the
/// OS level. For every other mode (and every non-macOS platform) `os_sandbox` is
/// `false`, `sbpl` is `None`, and an explicit audit line records that the OS
/// sandbox is NOT in force — an auditable choice, not a silent gap. (Even when the
/// sandbox IS in force, the GENERAL filesystem beyond the secret set is not scoped
/// — see the module docs; the audit line states this.)
///
/// The initial interactive shell is permitted in every mode (including
/// `Containment`/`NoFork`, whose contract is "exec only, for the initial
/// shell"). A hypothetical future `ProcessCapability` below `NoFork` would
/// fail closed via [`SpawnDecision::Deny`].
#[must_use]
pub fn decide(mode: ContainmentMode) -> SpawnDecision {
    let process_cap = ContainmentPolicy::process(mode);
    // Resolve the OS sandbox posture for this mode. `os_sandbox` is true ONLY for a
    // Containment spawn on a platform that can actuate (macOS); `sbpl` is the
    // per-user network-deny + secret-deny profile in that case and `None`
    // otherwise. The two are kept in lockstep (sbpl.is_some() == os_sandbox).
    let os_sandbox = network_sandbox_actuated(mode);
    let sbpl: Option<String> = if os_sandbox {
        crate::sbpl::profile_for(&ContainmentPolicy::capabilities(mode))
    } else {
        None
    };
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
        // Honest record that the OS sandbox IS in force for this spawn — network
        // AND the conservative secret-directory set AND the private-user-data set are
        // denied; and, just as honestly, that the GENERAL filesystem is NOT scoped.
        log_posture(
            SUBSYSTEM,
            "os-network-sandbox",
            mode,
            "OS sandbox ACTUATED via sandbox-exec (deny network*; deny read+write of secret dirs ~/.ssh ~/.aws ~/.gnupg ~/.config/gh ~/.config/aterm ~/.netrc; deny read+write of private data ~/Documents ~/Desktop ~/Downloads media ~/Library/{Mail,Messages,Keychains,Cookies,Safari} browser-profiles); general filesystem NOT scoped (follow-up)",
        );
    } else {
        // Explicit, non-silent record that the OS sandbox is NOT in force for this
        // mode (network permitted by policy, or non-macOS platform). Platform-
        // selected reason: on Windows the rlimit half is ALSO absent, and the
        // record must never overstate the posture (the Unix bytes are unchanged).
        log_posture(
            SUBSYSTEM,
            "os-network-sandbox",
            mode,
            if cfg!(windows) {
                "OS sandbox not actuated (non-macOS); process-cap gate only (resource limits unavailable on this platform)"
            } else {
                "OS sandbox not actuated (network permitted by policy, or non-macOS); rlimits + process-cap gate only"
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
            }
        }
        // Defensive default: any future, more-restrictive variant fails closed.
        #[allow(unreachable_patterns)]
        _ => {
            log_denial(
                SUBSYSTEM,
                "spawn initial shell",
                mode,
                "process capability denies fork/exec",
            );
            SpawnDecision::Deny { mode }
        }
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
    fn every_mode_permits_the_initial_shell() {
        for mode in [
            ContainmentMode::Master,
            ContainmentMode::User,
            ContainmentMode::Safety,
            ContainmentMode::Containment,
        ] {
            let d = decide(mode);
            assert!(
                d.is_permitted(),
                "the initial shell must be permitted in {mode} mode, got {d:?}"
            );
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
            let expect_os = network_sandbox_actuated(mode);
            match decide(mode) {
                SpawnDecision::Permit {
                    mode: m,
                    os_sandbox,
                    sbpl,
                } => {
                    assert_eq!(m, mode);
                    assert_eq!(os_sandbox, expect_os, "os_sandbox posture for {mode}");
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

    #[test]
    fn decision_is_permitted_helper_matches_variant() {
        assert!(
            SpawnDecision::Permit {
                mode: ContainmentMode::User,
                os_sandbox: false,
                sbpl: None,
            }
            .is_permitted()
        );
        assert!(
            !SpawnDecision::Deny {
                mode: ContainmentMode::Containment
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
    /// of `(allow default)` + a SMALL secret deny: a NORMAL shell must keep working.
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
