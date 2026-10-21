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
//! | User | Nothing beyond the capability-gated spawn; the shell keeps aterm's own resource limits. |
//! | Safety | Resource limits (`aterm-sandbox`): soft rlimits on Unix, a guardrail the shell's rc or its program can raise; the Job Object on Windows. No OS sandbox. |
//! | Containment | Safety's limits as hard rlimits, which the program cannot raise (`Limits::contained`), plus the OS sandbox below; on a platform without one, no shell. |
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
//! - refuses every mach service lookup but the two directory queries that name
//!   the user and their groups ([`crate::sbpl::MACH_LOOKUP_ALLOWLIST`]), so no
//!   daemon outside the sandbox works for it: nothing it does reaches Launch
//!   Services (`open`), Apple Events (`osascript`), login items, the preferences
//!   or keychain daemons or the pasteboard ([`crate::sbpl`] rule 2);
//! - confines writes to the temp roots (`/private/tmp`, `/private/var/tmp`, the
//!   user's own `$TMPDIR`) and `/dev`, plus the shell's own history files, so
//!   nothing it does can plant code that a later unsandboxed shell runs
//!   (`~/.zshrc`, a `~/Library/LaunchAgents` plist, a git hook, a `$PATH`
//!   binary, a precompiled module in the user cache directory). Its working
//!   directory and `$HOME` are read-only to it ([`crate::sbpl`] rule 3);
//! - denies read and write of the credential stores (`~/.ssh`, `~/.aws`,
//!   `~/.gnupg`, `~/.config/gh`, `~/.config/aterm`, `~/.netrc`, …) and of the
//!   private-data stores (`~/Documents`, `~/Desktop`, `~/Downloads`, media, and the
//!   local Mail / Messages / keychain / cookies / browser-profile databases).
//!
//! Other reads stay allowed so a normal `$SHELL` works. Every rule is proven
//! against the live kernel by the enforcement tests here.
//! [`SpawnDecision::Permit::sbpl`] carries the per-user profile; the PTY seam
//! refuses to spawn if the wrapper binary is missing.
//!
//! aterm itself runs outside the sandbox, so what it does at a contained
//! program's request is refused where aterm does it: the window brokers
//! nothing for a Containment tab, so its OSC 52 neither sets nor reads the
//! pasteboard, a Kitty file or shared-memory image is not read, and an OSC
//! 9/99/777 posts no desktop notification, whatever the config allows
//! (`aterm-gui`'s `host_reach`). What the person does (a
//! link click, a paste) is theirs and is not refused. The transparent session
//! (`aterm --sandbox` at a TTY) parses nothing, so the terminal around it
//! decides the program's escape sequences by its own policy.
//!
//! Fail closed: `Containment` never degrades. Where [`os_sandbox_actuated`] is
//! false (every non-macOS target), [`decide`] returns [`SpawnDecision::Deny`]
//! naming the gap and both launchers exit without a shell. Containment is
//! macOS-only by scope (`docs/ATERM_DESIGN.md` §5.6).

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
    // Containment demands the OS sandbox; without one, refuse the spawn.
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
        // network). Safety's limits apply as soft rlimits on Unix and via the
        // Job Object on Windows (`aterm_sandbox::Limits::shell_default`);
        // User keeps aterm's own (`Limits::inherit`), so its record claims no
        // limits.
        log_posture(
            SUBSYSTEM,
            "os-network-sandbox",
            mode,
            if mode == ContainmentMode::User {
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
        "OS sandbox applied: no network; no debugger attach (no task port but a process's own); no mach service lookup but user and group queries (launchd answers by its own policy); writes only to temp dirs and shell history; no access to credential stores or private data under $HOME"
    } else {
        "OS sandbox applied: no network; no debugger attach (no task port but a process's own); no mach service lookup but user and group queries (launchd answers by its own policy); writes only to temp dirs; $HOME did not resolve, so credential stores and private data can still be read"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_sandbox_in_force_only_for_containment_on_macos() {
        // Only Containment (network = None) is OS-network-sandboxed, and only on a
        // platform that can actuate. User/Safety permit network → never.
        assert_eq!(
            network_sandbox_actuated(ContainmentMode::Containment),
            cfg!(target_os = "macos")
        );
        for mode in [ContainmentMode::User, ContainmentMode::Safety] {
            assert!(
                !network_sandbox_actuated(mode),
                "{mode} permits network — must NOT be OS-network-sandboxed"
            );
        }
    }

    #[test]
    fn every_mode_permits_the_initial_shell_where_its_sandbox_exists() {
        for mode in [ContainmentMode::User, ContainmentMode::Safety] {
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
        assert_eq!(
            decide_on(ContainmentMode::Containment, false, None),
            SpawnDecision::Deny {
                mode: ContainmentMode::Containment,
                reason: NO_OS_SANDBOX_REASON,
            }
        );
        // The other modes never demanded a sandbox, so its absence changes nothing.
        for mode in [ContainmentMode::User, ContainmentMode::Safety] {
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

    /// `home_scoped` is decided by whether the home resolved, not by whether a
    /// sandbox applies: a sandboxed spawn with no home carries neither the
    /// credential denies nor the claim. Negative control for wiring it as
    /// `home_scoped: os_sandbox`.
    #[test]
    fn home_scoped_follows_the_home_not_the_sandbox() {
        // The credential deny's `.ssh` subpath, after either separator: a Windows
        // home reaches the profile spelled with `\`.
        let secret = |profile: &str| {
            ["/.ssh\"", "\\.ssh\""]
                .iter()
                .any(|tail| profile.contains(tail))
        };
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
        // The posture record tells the two apart.
        assert_ne!(applied_posture(true), applied_posture(false));
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
        // unset or empty in the test env, the profile is the base one and this
        // proof is not applicable: COULD NOT RUN, never a false pass.
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            aterm_spec::host::could_not_run("HOME is unset or empty: no secret-deny proof");
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

    /// Proof 5 (write confinement, full generated Containment profile): a
    /// contained shell cannot plant anything that later runs outside the sandbox:
    /// creating a file in `$HOME`, appending to `~/.zshrc`, or dropping a file in
    /// `~/Library/LaunchAgents` (creating that directory, where it does not exist
    /// yet) or into the user cache directory beside `$TMPDIR` all fail, while
    /// the temp roots stay writable (the history allowance
    /// has its own proof, 5b, over a scratch `$HOME`, since whether the real
    /// one's history file gets it depends on what that file is). Each probe
    /// opens for append without writing a byte, and anything a failed deny
    /// would have created is removed. Control: under the base profile alone the
    /// same `$HOME` probe is writable, so its denial is the write confinement's.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_containment_confines_writes() {
        let home = std::env::var("HOME").unwrap_or_else(|_| {
            aterm_spec::host::could_not_run("HOME is unset: no write-confinement proof")
        });
        let home = std::fs::canonicalize(&home).unwrap_or_else(|e| {
            aterm_spec::host::could_not_run(format!(
                "HOME does not resolve ({e}): no write-confinement proof"
            ))
        });
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
    /// every [`crate::sbpl::WRITABLE_ROOTS`] entry. Where the target directory
    /// is itself under a writable root (or cannot be found or written) the
    /// proof cannot be made here: COULD NOT RUN, never a pass.
    #[cfg(target_os = "macos")]
    fn scratch_home_outside_writable_roots(tag: &str) -> std::path::PathBuf {
        let refuse = |why: String| -> ! {
            aterm_spec::host::could_not_run(format!("no scratch HOME for the {tag} proof: {why}"))
        };
        let exe = std::env::current_exe().unwrap_or_else(|e| refuse(format!("current_exe: {e}")));
        // `<profile>/deps/<test>` → `<profile>` (never `deps/`, which is kept
        // small).
        let profile = exe
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap_or_else(|| refuse(format!("{} has no profile directory", exe.display())));
        let dir = std::fs::canonicalize(profile)
            .unwrap_or_else(|e| refuse(format!("{}: {e}", profile.display())));
        if crate::sbpl::WRITABLE_ROOTS
            .iter()
            .any(|root| dir.starts_with(root))
        {
            refuse(format!("{} lies under a writable root", dir.display()));
        }
        let home = dir.join(format!("aterm-sbpl-home-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir(&home).unwrap_or_else(|e| refuse(format!("{}: {e}", home.display())));
        home
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
        let home = scratch_home_outside_writable_roots("history");
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

    /// The child half of PROOF 7 under a profile: asks for its own control port and
    /// for the target's control, read, inspect and name ports, and prints the
    /// `kern_return_t`s (`self=<c> target=<c>/<r>/<i>/<n>`).
    #[cfg(target_os = "macos")]
    fn task_port_probe_child(target: libc::pid_t) {
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

    /// Proof 7 (task ports, full generated Containment profile): a contained
    /// process cannot take any task port (control, read, inspect, or name) of a
    /// process outside the sandbox, while keeping its own. The target is an ad-hoc
    /// copy of `/bin/cat` signed with `get-task-allow` (granted to an unentitled
    /// requester without a prompt); its directory is kept until it echoes a line
    /// (`aterm_uds::deadline::KeptUntilRan`). The probe is bounded so a blocked
    /// SecurityAgent prompt when developer mode is off fails the test rather than
    /// hanging. Control: under `(allow default)` the probe is granted the target's
    /// port.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_task_ports_stay_outside() {
        if let Ok(target) = std::env::var(TASK_PORT_TARGET) {
            task_port_probe_child(target.parse().expect("the parent's target pid"));
            return;
        }
        // Re-signing a copy of an Apple binary waits on trustd.
        aterm_uds::deadline::require_trust_service();
        struct Target(aterm_uds::spawnfd::Child);
        impl Drop for Target {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        // Declared before the target, so dropped after it is killed and reaped.
        let mut dir = aterm_uds::deadline::KeptUntilRan::new(
            aterm_tempfile::tempdir().expect("scratch dir").keep(),
        );
        let catter = dir.path().join("cat");
        let entitlements = dir.path().join("get-task-allow.plist");
        std::fs::copy("/bin/cat", &catter).expect("copy /bin/cat");
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
                .arg(&catter),
            std::time::Instant::now() + aterm_uds::deadline::CODESIGN_HANG,
        )
        .expect("run codesign");
        assert!(
            signed.status.success(),
            "ad-hoc signing the target failed: {}",
            String::from_utf8_lossy(&signed.stderr)
        );
        let mut target = Target(
            aterm_uds::spawnfd::command(&catter)
                .stdin(aterm_uds::spawnfd::Stdio::piped())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .spawn()
                .expect("start the target"),
        );
        // It runs until this is dropped (before the target: EOF, then the kill and reap).
        let _fed = aterm_uds::deadline::cat_echoes(&mut target.0);
        dir.ran();
        let exe = std::env::current_exe().expect("this test binary");
        let probe = |profile: &str| {
            let exe = exe.to_str().expect("utf8 test binary path");
            let mut child = sandbox_wrap(
                profile,
                exe,
                &[
                    "--exact",
                    "actuator::tests::enforcement_proof_task_ports_stay_outside",
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
            // unattended test run has nobody: on a machine with developer mode
            // off the child sits at 0% CPU under an open SecurityAgent prompt
            // for as long as the prompt stands, and holds the run with it.
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

    /// The names [`mach_lookup_probe_child`] looks up, comma-separated.
    #[cfg(target_os = "macos")]
    const LOOKUP_PROBE: &str = "ATERM_MACH_LOOKUP_PROBE";

    /// What `bootstrap_look_up` answers when the sandbox refuses the lookup
    /// (`BOOTSTRAP_NOT_PRIVILEGED`). A name no daemon serves answers 1102
    /// instead, so 1100 is the profile's refusal and nothing else.
    #[cfg(target_os = "macos")]
    const REFUSED: &str = "1100";

    /// Launch Services' daemon and its open/open-URL endpoints, the Apple Events
    /// daemon, and the `ServiceManagement` daemon behind `SMAppService`/`SMJobBless`.
    /// Each starts, drives, or registers a process that does not inherit the sandbox;
    /// refused because [`crate::sbpl::MACH_LOOKUP_ALLOWLIST`] does not name them.
    #[cfg(target_os = "macos")]
    const LAUNCH_BROKERS: &[&str] = &[
        "com.apple.coreservices.launchservicesd",
        "com.apple.lsd.open",
        "com.apple.lsd.openurl",
        "com.apple.coreservices.appleevents",
        "com.apple.xpc.smd",
    ];

    /// Daemons outside the sandbox that act on behalf of whoever looks them up.
    #[cfg(target_os = "macos")]
    const CALLER_ACTING_DAEMONS: &[&str] = &[
        // Writes preference domains into ~/Library/Preferences for its caller.
        "com.apple.cfprefsd.daemon",
        // The keychain daemon: item operations and the session's lock state.
        "com.apple.SecurityServer",
        // Adds and edits the user's login and background items.
        "com.apple.backgroundtaskmanagementagent",
        // The clipboard every other app pastes from.
        "com.apple.pasteboard.1",
        // Creates per-user directories, as root, for its caller.
        "com.apple.bsd.dirhelper",
        // Changes directory records (the allowlisted names only query).
        "com.apple.system.opendirectoryd.api",
        // Posts notifications into every app of the session.
        "com.apple.distributed_notifications@Uv3",
    ];

    /// The child half of PROOFS 8 and 10 under a profile: asks bootstrap for a send
    /// right to each name in [`LOOKUP_PROBE`] and prints the `kern_return_t`s
    /// in that order (`LOOKUPS <kr>/<kr>/…`). It only looks the names up; it
    /// never sends a message, so nothing is launched or changed.
    #[cfg(target_os = "macos")]
    fn mach_lookup_probe_child(names: &str) {
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
        let codes: Vec<String> = names
            .split(',')
            .map(|name| {
                let name = std::ffi::CString::new(name).expect("a service name has no NUL");
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
        println!("LOOKUPS {}", codes.join("/"));
    }

    /// Each of `names` looked up by [`mach_lookup_probe_child`] under `profile`,
    /// as `(name, kern_return_t)`. A lookup answers at once and asks nobody
    /// anything (unlike PROOF 7's `task_for_pid`, which can wait on the
    /// administrator dialog).
    #[cfg(target_os = "macos")]
    fn lookups<'n>(profile: &str, names: &[&'n str]) -> Vec<(&'n str, String)> {
        let exe = std::env::current_exe().expect("this test binary");
        let out = sandbox_wrap(
            profile,
            exe.to_str().expect("utf8 test binary path"),
            &[
                "--exact",
                "actuator::tests::enforcement_proof_launch_brokers_denied",
                "--nocapture",
                "--test-threads=1",
            ],
        )
        .env(LOOKUP_PROBE, names.join(","))
        .output()
        .expect("run the probe child");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let codes = stdout
            .lines()
            .find_map(|line| line.split_once("LOOKUPS "))
            .map(|(_, codes)| codes.trim().to_string())
            .unwrap_or_else(|| {
                panic!(
                    "no LOOKUPS line from the probe child; stdout={stdout:?} stderr={:?}",
                    String::from_utf8_lossy(&out.stderr)
                )
            });
        let codes: Vec<String> = codes.split('/').map(str::to_string).collect();
        assert_eq!(
            codes.len(),
            names.len(),
            "one answer per name; got {codes:?} for {names:?}"
        );
        names.iter().copied().zip(codes).collect()
    }

    /// The answers whose code is not `want`.
    #[cfg(target_os = "macos")]
    fn answered_otherwise<'n>(answers: &[(&'n str, String)], want: &str) -> Vec<(&'n str, String)> {
        answers
            .iter()
            .filter(|(_, kr)| kr != want)
            .cloned()
            .collect()
    }

    /// Proof 8 (launch brokers): under the full Containment profile, a contained
    /// process gets no port to Launch Services, the Apple Events daemon, or the
    /// `ServiceManagement` daemon because the allowlist does not name them.
    /// Control: under `(allow default)` every lookup succeeds.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_launch_brokers_denied() {
        if let Ok(names) = std::env::var(LOOKUP_PROBE) {
            mach_lookup_probe_child(&names);
            return;
        }
        let control = lookups("(version 1)(allow default)", LAUNCH_BROKERS);
        assert!(
            answered_otherwise(&control, "0").is_empty(),
            "CONTROL: an unsandboxed-equivalent probe must reach every launch broker, \
             or the denial below proves nothing; got {control:?}"
        );
        let contained = lookups(&containment_profile(), LAUNCH_BROKERS);
        let reached = answered_otherwise(&contained, REFUSED);
        assert!(
            reached.is_empty(),
            "DENY FAILED: under the Containment profile every launch-broker lookup must be \
             refused ({REFUSED}); answered otherwise: {reached:?}"
        );
    }

    /// Proof 10 (deny-by-default mach-lookup): a sample of daemons that act for
    /// their caller ([`CALLER_ACTING_DAEMONS`]) is refused (`kr` 1100) while every
    /// [`crate::sbpl::MACH_LOOKUP_ALLOWLIST`] name remains reachable (`kr` 0).
    /// Control: under `(allow default)` every name is reached.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_mach_lookup_denied_by_default() {
        let allowlist = crate::sbpl::MACH_LOOKUP_ALLOWLIST;
        let names: Vec<&str> = CALLER_ACTING_DAEMONS
            .iter()
            .chain(allowlist)
            .copied()
            .collect();
        let control = lookups("(version 1)(allow default)", &names);
        assert!(
            answered_otherwise(&control, "0").is_empty(),
            "CONTROL: an unsandboxed-equivalent probe must reach every name, or the \
             denial below proves nothing; got {control:?}"
        );
        let contained = lookups(&containment_profile(), &names);
        let (allowed, daemons): (Vec<_>, Vec<_>) = contained
            .into_iter()
            .partition(|(name, _)| allowlist.contains(name));
        let reached = answered_otherwise(&daemons, REFUSED);
        assert!(
            reached.is_empty(),
            "DENY FAILED: under the Containment profile a daemon that acts for its caller \
             must be refused ({REFUSED}); answered otherwise: {reached:?}"
        );
        let refused = answered_otherwise(&allowed, "0");
        assert!(
            refused.is_empty(),
            "OVER-BROAD: an allowlisted name must stay reachable under the Containment \
             profile; answered otherwise: {refused:?}"
        );
    }

    /// What tells [`preference_write_probe_child`] to probe.
    #[cfg(target_os = "macos")]
    const PREFERENCE_PROBE: &str = "ATERM_PREFERENCE_WRITE_PROBE";

    /// The child half of PROOF 9 under a profile: asks the sandbox, for this
    /// process, whether it may write a user preference domain — the question
    /// the preferences daemon asks before it writes `~/Library/Preferences` on
    /// a caller's behalf — and prints the answer (`PREFERENCE_WRITE <rc>`, 0 =
    /// allowed). It only asks; no preference is written.
    #[cfg(target_os = "macos")]
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

    /// Proof 9 (preference writes): `cfprefsd` writes `~/Library/Preferences`
    /// outside the sandbox on behalf of callers, so under the Containment profile
    /// the sandbox must refuse `user-preference-write`. Control: under the base
    /// profile alone the same check returns allowed.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_preference_writes_denied() {
        if std::env::var_os(PREFERENCE_PROBE).is_some() {
            preference_write_probe_child();
            return;
        }
        let exe = std::env::current_exe().expect("this test binary");
        let probe = |profile: &str| {
            let exe = exe.to_str().expect("utf8 test binary path");
            let out = sandbox_wrap(
                profile,
                exe,
                &[
                    "--exact",
                    "actuator::tests::enforcement_proof_preference_writes_denied",
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

    /// A hang detector for one contained tool, never a latency budget: each
    /// answers in about a second, a `targo` build included.
    #[cfg(target_os = "macos")]
    const TOOL_HANG: std::time::Duration = std::time::Duration::from_secs(180);

    /// `program`'s first line of output, run unsandboxed — the CONTROL a
    /// contained run is compared with.
    #[cfg(target_os = "macos")]
    fn unsandboxed_line(program: &str, args: &[&str]) -> Option<String> {
        let out = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(program).args(args),
            std::time::Instant::now() + TOOL_HANG,
        )
        .ok()?;
        let line = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()?
            .trim()
            .to_string();
        (out.status.success() && !line.is_empty()).then_some(line)
    }

    /// The first directory on `path` holding an executable `name`.
    #[cfg(target_os = "macos")]
    fn on_path(name: &str, path: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        std::env::split_paths(path)
            .map(|dir| dir.join(name))
            .find(|p| {
                std::fs::metadata(p)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    }

    /// Proof 11 (tool set): common development tools work under the full
    /// Containment profile with only [`crate::sbpl::MACH_LOOKUP_ALLOWLIST`]
    /// allowed, running in a scratch directory under `$TMPDIR`. If any case needs
    /// a tool this host lacks, the test ends COULD NOT RUN after verifying all
    /// available tools. Cases that execute a newly created Mach-O keep their
    /// directory in place if cut short (`aterm_uds::deadline::KeptUntilRan`).
    #[cfg(target_os = "macos")]
    #[test]
    fn shell_compat_tool_set_runs_under_containment() {
        use crate::capability::FsCapability;
        let home = scratch_home_outside_writable_roots("toolset");
        // The user's rc files, copied: the contained shells read them from the
        // scratch home and can write nothing back.
        if let Some(real) = std::env::var_os("HOME") {
            for rc in [
                ".zshenv",
                ".zprofile",
                ".zshrc",
                ".zlogin",
                ".bash_profile",
                ".bashrc",
                ".profile",
            ] {
                let _ = std::fs::copy(std::path::Path::new(&real).join(rc), home.join(rc));
            }
        }
        let mut work = aterm_uds::deadline::KeptUntilRan::new(
            aterm_tempfile::tempdir()
                .expect("a scratch directory under $TMPDIR")
                .keep(),
        );
        let temp = std::env::temp_dir();
        let profile =
            crate::sbpl::profile_for_home(home.to_str(), Some(&temp), FsCapability::TmpOnly);
        // CONTROL: the names an unsandboxed lookup gives, which a contained
        // tool must print too (not `501`).
        let user =
            unsandboxed_line("/usr/bin/id", &["-un"]).expect("CONTROL: id -un names the user");
        let group =
            unsandboxed_line("/usr/bin/id", &["-gn"]).expect("CONTROL: id -gn names the group");
        // The test's PATH, behind the directory of the `targo` running the
        // tests (`$CARGO`), so the contained build uses the toolchain that
        // built this binary.
        let inherited =
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin:/usr/sbin:/sbin".into());
        let toolchain = std::env::var_os("CARGO")
            .map(std::path::PathBuf::from)
            .and_then(|targo| targo.parent().map(std::path::Path::to_path_buf));
        let path = std::env::join_paths(
            toolchain
                .into_iter()
                .chain(std::env::split_paths(&inherited)),
        )
        .unwrap_or(inherited);
        let run = |script: &str| {
            let mut cmd = sandbox_wrap(&profile, "/bin/sh", &["-c", script]);
            cmd.env_clear()
                .env("PATH", &path)
                .env("HOME", &home)
                .env("TMPDIR", &temp)
                .env("USER", &user)
                .env("LOGNAME", &user)
                .env("SHELL", "/bin/zsh")
                .env("LANG", "en_US.UTF-8")
                .env("TERM", "dumb")
                .env("CARGO_HOME", work.path().join("cargo-home"))
                .current_dir(work.path());
            for kept in ["SDKROOT", "DEVELOPER_DIR"] {
                if let Some(value) = std::env::var_os(kept) {
                    cmd.env(kept, value);
                }
            }
            aterm_uds::deadline::output_until(&mut cmd, std::time::Instant::now() + TOOL_HANG)
        };

        // What a skipped tool lacks, if anything.
        let developer_tools = unsandboxed_line("/usr/bin/xcode-select", &["-p"])
            .filter(|dir| std::path::Path::new(dir).is_dir())
            .is_some();
        let lacks = |needs: &[&str]| -> Option<String> {
            needs.iter().find_map(|need| match *need {
                "developer tools" => (!developer_tools)
                    .then(|| "no developer tools (`xcode-select -p` names none)".to_string()),
                tool if tool.starts_with('/') => (!std::path::Path::new(tool).exists())
                    .then(|| format!("{tool} is not installed")),
                tool => on_path(tool, &path)
                    .is_none()
                    .then(|| format!("`{tool}` is not on PATH")),
            })
        };
        let dev = "developer tools";
        let member_check = format!("dsmemberutil checkmembership -U '{user}' -G '{group}'");
        let cases: [(&str, &[&str], &str, String); 16] = [
            (
                "a /bin/sh pipeline",
                &[],
                "echo hi | tr a-z A-Z | grep -c HI >/dev/null && cat /etc/hosts >/dev/null && echo SH-OK",
                "SH-OK".into(),
            ),
            (
                "an interactive zsh with the user's rc files",
                &["/bin/zsh"],
                "/bin/zsh -i -c 'print -r -- ZSH-I-OK:$USERNAME' </dev/null",
                format!("ZSH-I-OK:{user}"),
            ),
            (
                "a login zsh with the user's rc files",
                &["/bin/zsh"],
                "/bin/zsh -l -i -c 'print -r -- ZSH-L-OK' </dev/null",
                "ZSH-L-OK".into(),
            ),
            (
                "bash",
                &["/bin/bash"],
                "/bin/bash -l -c 'echo BASH-OK:$(whoami)' </dev/null",
                format!("BASH-OK:{user}"),
            ),
            (
                "the user's name",
                &[],
                "echo \"ID-OK:$(whoami):$(id -un):$(id -gn)\"",
                format!("ID-OK:{user}:{user}:{group}"),
            ),
            (
                "the user's group membership",
                &["/usr/bin/dsmemberutil"],
                &member_check,
                "user is a member of the group".into(),
            ),
            (
                "ls -l, date",
                &[],
                "ls -ld \"$HOME\" | awk '{print \"LS-OK:\" $3}' && date >/dev/null",
                format!("LS-OK:{user}"),
            ),
            (
                "dscacheutil",
                &["/usr/bin/dscacheutil"],
                "dscacheutil -q host -a name localhost",
                "127.0.0.1".into(),
            ),
            (
                "git",
                &[dev],
                "git init -q up && cd up && git config user.name r9 \
                 && git config user.email r9@example.invalid && echo a > f && git add f \
                 && git commit -qm init && echo b >> f && git diff --stat >/dev/null \
                 && git status --short >/dev/null && git log --oneline >/dev/null && cd .. \
                 && git clone -q \"file://$PWD/up\" clone \
                 && git -C clone log -1 --format=GIT-OK:%an",
                "GIT-OK:r9".into(),
            ),
            (
                "python3",
                &[dev],
                "python3 -c 'import ssl, json, socket, pwd, os; \
                 socket.getaddrinfo(\"localhost\", 0); \
                 print(\"PY-OK:\" + pwd.getpwuid(os.getuid()).pw_name)'",
                format!("PY-OK:{user}"),
            ),
            (
                "make",
                &[dev],
                "printf 'all:\\n\\t@echo MAKE-OK\\n' > Makefile && make -s",
                "MAKE-OK".into(),
            ),
            ("xcrun", &[dev], "xcrun --show-sdk-path", ".sdk".into()),
            (
                "clang",
                &[dev],
                "printf '#include <stdio.h>\\nint main(void){puts(\"C-OK\");return 0;}\\n' > hello.c \
                 && clang -o hello hello.c && ./hello",
                "C-OK".into(),
            ),
            (
                "ad-hoc codesign, then codesign -v and exec",
                &["/usr/bin/codesign"],
                // A copy of an Apple binary off the system volume is signed
                // ad hoc before it runs (AGENTS.md).
                "cp /bin/ls ls-copy && codesign --force --sign - ls-copy 2>/dev/null \
                 && codesign -v ls-copy && ./ls-copy -d . >/dev/null && echo CODESIGN-OK",
                "CODESIGN-OK".into(),
            ),
            (
                "targo and trustc",
                &["targo", "trustc"],
                "targo --version && trustc --version",
                "trustc".into(),
            ),
            (
                "a targo --unverified build",
                &["targo", dev],
                "mkdir -p crate/src && printf '[package]\\nname = \"probe\"\\nversion = \"0.1.0\"\\n\
                 edition = \"2021\"\\n\\n[workspace]\\n' > crate/Cargo.toml \
                 && printf 'fn main() { println!(\"TARGO-OK\"); }\\n' > crate/src/main.rs \
                 && cd crate && targo --unverified build -q && ./target/debug/probe",
                "TARGO-OK".into(),
            ),
        ];

        let mut failures = Vec::new();
        let mut lacking = Vec::new();
        let mut ran = 0;
        // Whether a case's shell was killed (or not waited for) before it ended, a program it
        // started from `work` with it.
        let mut cut_short = false;
        for (what, needs, script, expect) in &cases {
            if let Some(reason) = lacks(needs) {
                lacking.push(format!("{what}: {reason}"));
                continue;
            }
            ran += 1;
            match run(script) {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    if !out.status.success() || !stdout.contains(expect.as_str()) {
                        failures.push(format!(
                            "{what}: rc={:?}, wanted {expect:?} in stdout={stdout:?} stderr={:?}",
                            out.status.code(),
                            String::from_utf8_lossy(&out.stderr)
                        ));
                    }
                }
                Err(e) => {
                    cut_short = true;
                    failures.push(format!("{what}: {e:?}"));
                }
            }
        }
        if !cut_short {
            // Every case's shell ended on its own, and every program it started with it.
            work.ran();
        }
        let _ = std::fs::remove_dir_all(&home);
        eprintln!("TOOLSET ran {ran} of {}", cases.len());
        assert!(
            failures.is_empty(),
            "SHELL BROKEN: under the Containment profile:\n{}",
            failures.join("\n")
        );
        // The cases this host could not run are said last, after every one it
        // could run held: COULD NOT RUN, never a pass over a skipped tool.
        if !lacking.is_empty() {
            aterm_spec::host::could_not_run(format!(
                "{} of {} tool cases need what this host lacks: {}",
                lacking.len(),
                cases.len(),
                lacking.join("; ")
            ));
        }
    }

    /// Proof 12 (`open` and `osascript`): `open -W` of a background-only scratch
    /// app whose executable writes a marker outside the writable roots fails and
    /// writes nothing, and `osascript` sending Finder an Apple Event in raw four-char
    /// codes (`«class pnam» of «class sdsk»`, compiled without Finder's dictionary)
    /// gets no answer.
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_open_and_osascript_stay_refused() {
        let profile = containment_profile();
        let reached = answered_otherwise(&lookups(&profile, LAUNCH_BROKERS), REFUSED);
        assert!(
            reached.is_empty(),
            "DENY FAILED: a launch broker is reachable under the Containment profile ({reached:?}); \
             open and osascript were not run"
        );
        let outside = scratch_home_outside_writable_roots("open");
        let marker = outside.join("ran-outside");
        let scratch = aterm_tempfile::tempdir().expect("a scratch directory");
        let app = scratch.path().join("AtermContainmentProbe.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).expect("make the scratch app");
        std::fs::write(
            app.join("Contents/Info.plist"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>probe</string>\
             <key>CFBundleIdentifier</key><string>com.example.aterm-containment-probe</string>\
             <key>CFBundlePackageType</key><string>APPL</string>\
             <key>LSBackgroundOnly</key><true/></dict></plist>\n",
        )
        .expect("write the scratch app's Info.plist");
        let probe = macos.join("probe");
        std::fs::write(
            &probe,
            format!("#!/bin/sh\necho ran > '{}'\n", marker.display()),
        )
        .expect("write the scratch app's executable");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755))
                .expect("make the scratch app executable");
        }
        let deadline = || std::time::Instant::now() + TOOL_HANG;
        let app_s = app.to_str().expect("utf8 scratch path");
        let open = aterm_uds::deadline::output_until(
            &mut sandbox_wrap(&profile, "/usr/bin/open", &["-W", "-g", "-j", app_s]),
            deadline(),
        )
        .expect("run open under the Containment profile");
        let osascript = aterm_uds::deadline::output_until(
            &mut sandbox_wrap(
                &profile,
                "/usr/bin/osascript",
                &[
                    "-e",
                    "tell application \"Finder\" to get «class pnam» of «class sdsk»",
                ],
            ),
            deadline(),
        )
        .expect("run osascript under the Containment profile");
        let ran_outside = marker.exists();
        let _ = std::fs::remove_dir_all(&outside);

        assert!(
            !open.status.success() && !ran_outside,
            "DENY FAILED: open launched an app outside the sandbox under the Containment \
             profile: rc={:?} marker={ran_outside} stderr={:?}",
            open.status.code(),
            String::from_utf8_lossy(&open.stderr)
        );
        let osa_stdout = String::from_utf8_lossy(&osascript.stdout);
        let osa_stderr = String::from_utf8_lossy(&osascript.stderr);
        assert!(
            !osa_stderr.contains("(-2740)"),
            "the osascript probe did not compile, so it proves nothing: {osa_stderr:?}"
        );
        assert!(
            !osascript.status.success() && osa_stdout.trim().is_empty(),
            "DENY FAILED: Finder answered an Apple Event sent from the Containment profile: \
             rc={:?} stdout={osa_stdout:?} stderr={osa_stderr:?}",
            osascript.status.code()
        );
    }
}
