// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Spawn-seam containment actuator turning [`Capabilities`](crate::Capabilities) into a logged [`SpawnDecision`].

use crate::audit::{log_denial, log_posture};
use crate::capability::{NetworkCapability, ProcessCapability};
use crate::mode::ContainmentMode;
use crate::policy::ContainmentPolicy;

/// Audit subsystem label for spawn-seam containment events.
const SUBSYSTEM: &str = "spawn";

/// Refusal reason returned by [`decide`] for `Containment` on platforms without an OS sandbox.
pub const NO_OS_SANDBOX_REASON: &str = "the sandbox exists only on macOS, so no shell was started";

/// Whether this platform can actuate a real OS sandbox at the spawn seam (`true` on macOS).
#[must_use]
pub const fn os_sandbox_actuated() -> bool {
    cfg!(target_os = "macos")
}

/// Whether the OS network sandbox is active for a spawn in `mode`.
#[must_use]
pub fn network_sandbox_actuated(mode: ContainmentMode) -> bool {
    os_sandbox_actuated() && ContainmentPolicy::network(mode) == NetworkCapability::None
}

/// Actuated decision at the PTY spawn seam for a [`ContainmentMode`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpawnDecision {
    /// Spawning the initial shell is permitted.
    Permit {
        /// Mode this decision was made for.
        mode: ContainmentMode,
        /// Whether a real OS sandbox backs this permit ([`network_sandbox_actuated`]).
        os_sandbox: bool,
        /// SBPL profile applied via `sandbox-exec` (`Some` iff `os_sandbox`).
        sbpl: Option<String>,
        /// Whether `sbpl` includes `$HOME`-scoped shell-history and credential rules.
        home_scoped: bool,
    },
    /// Spawning is denied for this mode.
    Deny {
        /// Mode this decision was made for.
        mode: ContainmentMode,
        /// Refusal reason printed by the launcher.
        reason: &'static str,
    },
}

impl SpawnDecision {
    /// Whether this decision permits spawning.
    #[must_use]
    pub const fn is_permitted(&self) -> bool {
        matches!(self, SpawnDecision::Permit { .. })
    }
}

/// Evaluates and audits whether the PTY spawn seam may run for `mode`.
#[must_use]
pub fn decide(mode: ContainmentMode) -> SpawnDecision {
    decide_on(
        mode,
        os_sandbox_actuated(),
        crate::sbpl::home_dir().as_deref(),
    )
}

/// [`decide`] with the platform's sandbox capability and the `$HOME` the
fn decide_on(mode: ContainmentMode, sandbox_available: bool, home: Option<&str>) -> SpawnDecision {
    let process_cap = ContainmentPolicy::process(mode);
    let denies_network = ContainmentPolicy::network(mode) == NetworkCapability::None;
    if denies_network && !sandbox_available {
        log_denial(SUBSYSTEM, "spawn initial shell", mode, NO_OS_SANDBOX_REASON);
        return SpawnDecision::Deny {
            mode,
            reason: NO_OS_SANDBOX_REASON,
        };
    }
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

    if os_sandbox {
        log_posture(
            SUBSYSTEM,
            "os-network-sandbox",
            mode,
            applied_posture(home_scoped),
        );
    } else {
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
        ProcessCapability::Full | ProcessCapability::Restricted | ProcessCapability::NoFork => {
            SpawnDecision::Permit {
                mode,
                os_sandbox,
                sbpl,
                home_scoped,
            }
        }
        #[allow(unreachable_patterns)]
        _ => {
            let reason = "process capability denies fork/exec";
            log_denial(SUBSYSTEM, "spawn initial shell", mode, reason);
            SpawnDecision::Deny { mode, reason }
        }
    }
}

/// Summary string describing the enforced sandbox posture for `home_scoped`.
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
        for mode in [
            ContainmentMode::User,
            ContainmentMode::Safety,
            ContainmentMode::Containment,
        ] {
            if mode == ContainmentMode::Containment && !os_sandbox_actuated() {
                // refused there — `containment_fails_closed_without_an_os_sandbox`
                continue;
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
    #[test]
    fn home_scoped_follows_the_home_not_the_sandbox() {
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
        assert_ne!(applied_posture(true), applied_posture(false));
    }

    /// Build the sandbox-exec argv the launcher builds: `sandbox-exec -p <sbpl>
    #[cfg(target_os = "macos")]
    fn sandbox_wrap(sbpl: &str, prog: &str, args: &[&str]) -> aterm_uds::spawnfd::Command {
        let mut cmd = aterm_uds::spawnfd::command(crate::sbpl::SANDBOX_EXEC_PATH);
        cmd.arg("-p").arg(sbpl).arg(prog).args(args);
        cmd
    }

    /// Proof 1 (network), hermetic: a loopback TCP listener in this process; a
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_network_deny_blocks_loopback_connect() {
        use std::io::Write;
        use std::net::TcpListener;

        let sbpl = decide(ContainmentMode::Containment);
        let SpawnDecision::Permit {
            os_sandbox: true,
            sbpl: Some(profile),
            ..
        } = sbpl
        else {
            panic!("Containment on macOS must actuate the network sandbox; got {sbpl:?}");
        };
        assert!(profile.starts_with(crate::sbpl::BASE_PROFILE));

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let accepter = std::thread::spawn(move || {
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
        let nc_args = ["-G", "2", "-w", "2", "-z", "127.0.0.1", port_s.as_str()];

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

        let _ = std::net::TcpStream::connect(("127.0.0.1", port));
        let _ = accepter.join();
    }

    /// Proof 2 (EPERM): the same wrap with a file-read-deny profile over a real
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_file_read_deny_yields_operation_not_permitted() {
        use std::io::Write;

        let dir = std::env::temp_dir();
        let path = dir.join(format!("aterm-sbpl-enforce-{}.txt", std::process::id()));
        {
            let mut f = std::fs::File::create(&path).expect("create temp file");
            f.write_all(b"SECRET-ENFORCE-PROBE")
                .expect("write temp file");
        }
        let canon = std::fs::canonicalize(&path).expect("canonicalize temp path");
        let canon_s = canon.to_str().expect("utf8 path").to_string();

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

        let profile =
            format!("(version 1)(allow default)(deny file-read* (literal \"{canon_s}\"))");
        let sandboxed = sandbox_wrap(&profile, "/bin/cat", &[canon_s.as_str()])
            .output()
            .expect("run cat under sandbox-exec");
        let stderr = String::from_utf8_lossy(&sandboxed.stderr);
        let stdout = String::from_utf8_lossy(&sandboxed.stdout);

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
        assert!(profile.starts_with(crate::sbpl::BASE_PROFILE));

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
    #[cfg(target_os = "macos")]
    #[test]
    fn secret_deny_full_containment_profile_blocks_secret_but_allows_nonsecret() {
        use std::io::Write;

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

        let control = std::process::Command::new("/bin/cat")
            .arg(&secret_probe_s)
            .output()
            .expect("run cat control on secret probe");
        let control_ok = control.status.success()
            && String::from_utf8_lossy(&control.stdout).contains("SECRET-DENY-PROBE");

        let denied = sandbox_wrap(&profile, "/bin/cat", &[secret_probe_s.as_str()])
            .output()
            .expect("run cat on secret probe under Containment profile");
        let denied_stderr = String::from_utf8_lossy(&denied.stderr).into_owned();
        let denied_stdout = String::from_utf8_lossy(&denied.stdout).into_owned();

        let allowed = sandbox_wrap(&profile, "/bin/cat", &["/etc/hosts"])
            .output()
            .expect("run cat on /etc/hosts under Containment profile");
        let allowed_ok = allowed.status.success();

        let _ = std::fs::remove_file(&secret_probe);
        if !dir_pre_existed {
            let _ = std::fs::remove_dir(&secret_dir);
        }

        assert!(
            control_ok,
            "CONTROL FAILED: unsandboxed cat must read the probe"
        );
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
        let agents_made = !agents_existed
            && sandboxed_sh_ok(&profile, &format!("mkdir '{}'", launch_agents.display()));
        let control_ok = sandboxed_append_ok(crate::sbpl::BASE_PROFILE, &denied[0]);
        if agents_made {
            let _ = std::fs::remove_dir(&launch_agents);
        }
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
    #[cfg(target_os = "macos")]
    fn scratch_home_outside_writable_roots(tag: &str) -> std::path::PathBuf {
        let refuse = |why: String| -> ! {
            aterm_spec::host::could_not_run(format!("no scratch HOME for the {tag} proof: {why}"))
        };
        let exe = std::env::current_exe().unwrap_or_else(|e| refuse(format!("current_exe: {e}")));
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
            // SAFETY: `mach_task_self_` is libSystem's cached task send right, and `flavor` writes `port` only on success.
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
    #[cfg(target_os = "macos")]
    #[test]
    fn enforcement_proof_task_ports_stay_outside() {
        if let Ok(target) = std::env::var(TASK_PORT_TARGET) {
            task_port_probe_child(target.parse().expect("the parent's target pid"));
            return;
        }
        aterm_uds::deadline::require_trust_service();
        struct Target(aterm_uds::spawnfd::Child);
        impl Drop for Target {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

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
    #[cfg(target_os = "macos")]
    const REFUSED: &str = "1100";

    /// Launch Services' daemon and its open/open-URL endpoints, the Apple Events
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
        "com.apple.cfprefsd.daemon",
        "com.apple.SecurityServer",
        "com.apple.backgroundtaskmanagementagent",
        "com.apple.pasteboard.1",
        "com.apple.bsd.dirhelper",
        "com.apple.system.opendirectoryd.api",
        "com.apple.distributed_notifications@Uv3",
    ];

    /// The child half of PROOFS 8 and 10 under a profile: asks bootstrap for a send
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
                // SAFETY: `bootstrap_port` is initialized before `main`, `name` is NUL-terminated, and `port` is written only on success.
                let kr = unsafe { bootstrap_look_up(bootstrap_port, name.as_ptr(), &raw mut port) };
                if port != 0 {
                    // SAFETY: `port` is a valid send right just returned by `bootstrap_look_up`.
                    unsafe { mach_port_deallocate(mach_task_self_, port) };
                }
                kr.to_string()
            })
            .collect();
        println!("LOOKUPS {}", codes.join("/"));
    }

    /// Each of `names` looked up by [`mach_lookup_probe_child`] under `profile`,
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
        // SAFETY: Both strings are NUL-terminated literals and `sandbox_check` only reads its arguments.
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
    #[cfg(target_os = "macos")]
    const TOOL_HANG: std::time::Duration = std::time::Duration::from_secs(180);

    /// `program`'s first line of output, run unsandboxed — the CONTROL a
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
    #[cfg(target_os = "macos")]
    #[test]
    fn shell_compat_tool_set_runs_under_containment() {
        use crate::capability::FsCapability;
        let home = scratch_home_outside_writable_roots("toolset");
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
        let user =
            unsandboxed_line("/usr/bin/id", &["-un"]).expect("CONTROL: id -un names the user");
        let group =
            unsandboxed_line("/usr/bin/id", &["-gn"]).expect("CONTROL: id -gn names the group");
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
