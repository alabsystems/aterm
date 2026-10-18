// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resource sandbox for spawned children (ATERM_DESIGN WS-G).
//!
//! Confines the child `$SHELL` with POSIX `setrlimit` bounds, applied in the
//! child after `fork` and before `exec` so the shell and everything it runs
//! inherit them. [`Limits::apply`] requires a [`Cap<Sandbox>`] from `aterm-cap`,
//! whose root mint is sealed behind its `launcher-mint` feature.
//!
//! Resource limits only: filesystem/network scoping is `aterm-containment`'s
//! Seatbelt lane.
//!
//! The cap gate and [`Limits`] are shared; the actuator is per-platform.
//! `src/unix.rs` is the `setrlimit` loop. On Windows [`Limits::apply`] is a
//! cap-gated no-op and `Limits::apply_to_job` is the Job Object lane, which
//! `aterm-pty`'s ConPTY seam calls on the suspended child's job.
//!
//! STATUS (§0.1): tested (limits are read back); not yet Trust-proven.

use std::io;

use aterm_cap::{Cap, Tier};

#[cfg(unix)]
#[path = "unix.rs"]
mod imp;
#[cfg(windows)]
#[path = "windows.rs"]
mod imp;

/// The effect a capability authorizes here: installing a resource sandbox.
pub enum Sandbox {}

/// Resource limits to apply; `None` leaves a limit unchanged. On POSIX each
/// value is installed as the soft limit, and as the hard limit too when
/// [`Limits::binding`] is set (`set_limit` in the unix actuator). On Windows
/// `apply` installs nothing; `Limits::apply_to_job` is the Windows lane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// CPU seconds (`RLIMIT_CPU`).
    pub cpu_seconds: Option<u64>,
    /// Address space / virtual memory in bytes (`RLIMIT_AS`).
    pub address_space: Option<u64>,
    /// Max written file size in bytes (`RLIMIT_FSIZE`).
    pub file_size: Option<u64>,
    /// Max open file descriptors (`RLIMIT_NOFILE`).
    pub open_files: Option<u64>,
    /// Max concurrently-active processes in the sandbox Job Object (Windows
    /// `ActiveProcessLimit`): a fork-bomb bound on the child and its
    /// descendants. Windows only; inert on POSIX, which has no per-job class.
    pub active_processes: Option<u32>,
    /// Windows only: install Job Object UI restrictions (clipboard, desktop
    /// switch, display / system parameters, global atoms, `ExitWindows`) so a
    /// hostile child cannot act through the shared window station. `false`
    /// (the daily-driver default) leaves UI access unrestricted.
    pub restrict_ui: bool,
    /// POSIX: whether each value also becomes the HARD limit, so the program
    /// cannot raise it back (an unprivileged process can lower its hard limit
    /// but never raise it). `false` keeps the inherited hard ceiling, so a soft
    /// limit stays a guardrail the `$SHELL`'s rc can raise (`ulimit -n 65536`).
    /// Windows ignores it: a Job Object limit binds the child already.
    pub binding: bool,
}

impl Limits {
    /// The Safety-mode set for an interactive shell: caps fds, address space
    /// (not on macOS) and, on Windows, active processes and UI access; CPU and
    /// file size stay unbounded (a shell legitimately runs long and writes large
    /// files).
    ///
    /// On POSIX these are SOFT limits under the inherited hard ceiling: a
    /// guardrail against a runaway program, which the program, or the person's
    /// rc, can raise. Decided 2026-10-10 (P1 of the r13 policy hunt): Safety's
    /// program is not hostile, and a common rc line raises the descriptor limit
    /// past 8192 (`ulimit -n 10240` or `65536`). Measured on macOS 26.6 under a
    /// hard ceiling of 8192: zsh prints `ulimit: value exceeds hard limit`,
    /// bash and sh `cannot modify limit: Operation not permitted`, at every
    /// shell start, and the shell stays at 8192; under the kept ceiling all
    /// three raise it. A per-process ceiling would not bound a program that
    /// forks anyway (POSIX has no process cap here). The hostile mode's set is
    /// [`Self::contained`]. On Windows the Job Object binds them.
    #[must_use]
    // #[inline] so the MIR crosses the crate boundary and callers' Trust gates
    // (aterm-pty) verify this body instead of assuming an absent callee.
    #[inline]
    pub fn shell_default() -> Self {
        Limits {
            cpu_seconds: None,
            // macOS refuses (`EINVAL`) any `RLIMIT_AS` below the process's
            // CURRENT mapped size, and an arm64 process already maps ~415 GiB
            // before it runs a line (measured 2026-09-23, macOS 26.6: python3
            // and zsh alike), so the 16 GiB cap below cannot be expressed
            // there, and a value large enough to be accepted bounds nothing
            // real.
            address_space: if cfg!(target_os = "macos") {
                None
            } else {
                Some(16 * 1024 * 1024 * 1024) // 16 GiB
            },
            file_size: None,
            open_files: Some(8192),
            // Enough for a parallel build, low enough to stop a fork bomb.
            active_processes: Some(512),
            // Hardened modes confine an untrusted shell: lock its window-station
            // access.
            restrict_ui: true,
            binding: false,
        }
    }

    /// The Containment-mode set: [`Self::shell_default`]'s values, installed
    /// as hard limits too, so the contained program cannot raise them back.
    ///
    /// RULING 2026-10-10 (P1 of the r13 policy hunt): Containment holds a
    /// hostile program, so its limits bind. Until this ruling they went in
    /// soft under the inherited hard ceiling (unlimited for `RLIMIT_NOFILE` on
    /// macOS), and one `ulimit -n` by the contained program lifted them. The
    /// cost, accepted: a contained shell whose rc raises the descriptor limit
    /// past 8192 prints the shell's refusal at every start and keeps 8192
    /// (measured, see [`Self::shell_default`]). What binds is per process:
    /// POSIX sets no process cap here, so a program that forks holds 8192
    /// descriptors in each child.
    #[must_use]
    #[inline]
    pub fn contained() -> Self {
        Limits {
            binding: true,
            ..Self::shell_default()
        }
    }

    /// The daily-driver limits (User mode): every limit `None`, so the shell
    /// inherits the login shell's `rlimit`s unchanged. No `RLIMIT_AS` in
    /// particular: it caps VIRTUAL address space, which CUDA/ML runtimes, the JVM,
    /// Go and the sanitizers reserve far beyond resident use, so any finite cap
    /// breaks real programs while bounding nothing. This mode confines through
    /// the capability gate; [`Self::shell_default`] is the hardened opt-in.
    #[must_use]
    pub fn inherit() -> Self {
        Limits::default()
    }

    /// Apply these limits to the CURRENT process. Call in the child, after
    /// `fork`, before `exec`. Requires a `Trusted`+ [`Cap<Sandbox>`].
    ///
    /// The cap gate hard-fails; the `setrlimit` calls are best-effort: every
    /// limit is attempted, so one the OS refuses (a 16 GiB `RLIMIT_AS` on macOS)
    /// cannot keep the others from installing, and the first error is returned
    /// after all were tried.
    ///
    /// An `Err` means confinement did not fully install and the caller must fail
    /// closed: `aterm-pty`'s child `_exit(126)`s before `execve` (ATERM_DESIGN
    /// §5.6, exit-before-exec).
    ///
    /// # Errors
    /// `PermissionDenied` if the capability tier is too low; otherwise the first
    /// `setrlimit` OS error encountered (after attempting every limit).
    ///
    /// SPEC: the `Apply` action of `aterm_models_core::sandbox_model` (`Sandbox`). Its `AllSupportedApplied` invariant (every
    /// requested limit the OS supports is installed) is what the best-effort loop
    /// guarantees. No Tier-1 binding: the loop runs post-fork and may not
    /// allocate; `unsupported_limit_does_not_block_the_working_ones` reads the
    /// limit back instead.
    // PROJECTION (TRUST_VACUITY_GATE §2.2): the L2 obligation needs only the
    // projection NAME (Trust does not execute it) onto the model's
    // `<<applied, done>>` in the configuration `<<Req, Sup>>` the host is.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "sandbox",
            action = "Apply",
            project = "aterm_sandbox::Sandbox::project_apply"
        )
    )]
    pub fn apply(&self, cap: &Cap<Sandbox>) -> io::Result<()> {
        aterm_cap::require(cap, Tier::Trusted)
            .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;
        imp::apply_limits(self)
    }

    /// Apply these limits to the Job Object of a still-suspended ConPTY child, so
    /// the kernel enforces them on it and its descendants: the Windows analog of
    /// [`Self::apply`] (Windows has no pre-exec `fork` seam). Requires a
    /// `Trusted`+ [`Cap<Sandbox>`] (SEC-2).
    ///
    /// Query-modify-write: `LimitFlags` already on the job (`KILL_ON_JOB_CLOSE`)
    /// are kept. Memory, CPU time, the active-process cap and
    /// [`Limits::restrict_ui`] are enforced; `open_files` / `file_size` have no
    /// Job Object analog.
    ///
    /// # Errors
    /// `PermissionDenied` if the capability tier is too low; otherwise the first
    /// Job Object OS error; the caller must fail closed (terminate the
    /// never-resumed child, ATERM_DESIGN §5.6).
    #[cfg(windows)]
    pub fn apply_to_job(
        &self,
        cap: &Cap<Sandbox>,
        job: std::os::windows::io::RawHandle,
    ) -> io::Result<()> {
        aterm_cap::require(cap, Tier::Trusted)
            .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;
        imp::apply_to_job(self, job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_cap::Authority;
    #[cfg(unix)]
    use imp::{RlimitResource, set_limit};

    /// `RLIMIT_*` is process-wide and the tests run concurrently in one process,
    /// so tests that mutate NOFILE would see each other's read-backs.
    #[cfg(unix)]
    static NOFILE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Serialize a NOFILE-mutating test. Poison is ignored deliberately: one test
    /// panicking must not cascade into unrelated failures in the others.
    #[cfg(unix)]
    fn nofile_guard() -> std::sync::MutexGuard<'static, ()> {
        NOFILE_TESTS.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current `rlimit` of `resource`.
    #[cfg(unix)]
    fn rlimit(resource: RlimitResource) -> libc::rlimit {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: valid resource id + out-param.
        let rc = unsafe { libc::getrlimit(resource, &mut lim) };
        assert_eq!(rc, 0, "getrlimit failed");
        lim
    }

    /// The current soft limit of `resource`.
    #[cfg(unix)]
    fn current(resource: RlimitResource) -> u64 {
        rlimit(resource).rlim_cur
    }

    /// The current hard ceiling of `resource`.
    #[cfg(unix)]
    fn current_hard(resource: RlimitResource) -> u64 {
        rlimit(resource).rlim_max
    }

    #[test]
    fn apply_requires_a_trusted_capability() {
        let auth = unsafe { Authority::root_authority() };
        let weak: Cap<Sandbox> = auth.grant(Tier::Untrusted);
        let err = Limits {
            cpu_seconds: Some(123456),
            ..Default::default()
        }
        .apply(&weak)
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    // Windows: `apply` is a capability-gated NO-OP — the fail-closed gate still
    // denies a weak cap (SEC-2 parity with the Unix child's exit-before-exec), and a
    // Trusted cap succeeds without installing any limit.
    #[cfg(windows)]
    #[test]
    fn windows_apply_is_a_capgated_noop() {
        let auth = unsafe { Authority::root_authority() };
        let trusted: Cap<Sandbox> = auth.grant(Tier::Trusted);
        Limits::shell_default()
            .apply(&trusted)
            .expect("Windows apply with a Trusted cap is an Ok(()) no-op");
        let weak: Cap<Sandbox> = auth.grant(Tier::Untrusted);
        let err = Limits::shell_default().apply(&weak).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::PermissionDenied,
            "the cap gate must fail closed even though the actuator is a no-op"
        );
    }

    #[cfg(unix)]
    #[test]
    fn none_limits_are_a_no_op() {
        let auth = unsafe { Authority::root_authority() };
        let cap: Cap<Sandbox> = auth.grant(Tier::Trusted);
        let before = current(libc::RLIMIT_CPU);
        Limits::default().apply(&cap).unwrap();
        assert_eq!(current(libc::RLIMIT_CPU), before);
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_limit_does_not_block_the_working_ones() {
        let _serialized = nofile_guard();
        // A refused RLIMIT_AS must not keep NOFILE from installing.
        let auth = unsafe { Authority::root_authority() };
        let cap: Cap<Sandbox> = auth.grant(Tier::Trusted);
        let target = 256u64;
        // macOS refuses any AS below the process's current mapped size, and a
        // 64-bit Mach-O maps a 4 GiB `__PAGEZERO` before anything else, so 1 GiB
        // is refused on every Mac (macOS accepts 64 TiB). Off macOS 64 TiB is
        // accepted and harmless, where 1 GiB would cap this test process.
        let address_space: u64 = if cfg!(target_os = "macos") {
            1024 * 1024 * 1024
        } else {
            64 * 1024 * 1024 * 1024 * 1024
        };
        let result = Limits {
            address_space: Some(address_space),
            open_files: Some(target),
            ..Default::default()
        }
        .apply(&cap);
        if cfg!(target_os = "macos") {
            assert!(
                result.is_err(),
                "macOS must refuse an RLIMIT_AS below the mapped size, or this test \
                 exercises nothing"
            );
        }
        assert_eq!(
            current(libc::RLIMIT_NOFILE),
            target,
            "NOFILE must be applied even when an earlier limit is unsupported"
        );
    }

    #[test]
    fn shell_default_omits_unsupported_address_space_on_macos() {
        // macOS must not request RLIMIT_AS: 16 GiB is below what every process
        // already maps, so it would EINVAL. Construction-only: no setrlimit.
        let d = Limits::shell_default();
        #[cfg(target_os = "macos")]
        assert_eq!(
            d.address_space, None,
            "macOS must not request a finite RLIMIT_AS"
        );
        #[cfg(not(target_os = "macos"))]
        assert!(
            d.address_space.is_some(),
            "non-macOS should bound the address space"
        );
        assert!(
            d.open_files.is_some(),
            "the working NOFILE limit must remain"
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_preserves_the_hard_ceiling_so_a_shell_can_raise_its_soft_limit() {
        let _serialized = nofile_guard();
        // A hard-limit clamp makes a `.zshrc` `ulimit -n 65536` abort shell
        // startup with `value exceeds hard limit`: apply must leave the hard
        // ceiling alone, and the soft limit must stay raisable up to it.
        let auth = unsafe { Authority::root_authority() };
        let cap: Cap<Sandbox> = auth.grant(Tier::Trusted);

        let hard_before = current_hard(libc::RLIMIT_NOFILE);
        let applied = 256u64;
        assert!(
            hard_before > applied,
            "test precondition: inherited hard NOFILE ({hard_before}) must exceed the applied soft default"
        );

        Limits {
            open_files: Some(applied),
            ..Default::default()
        }
        .apply(&cap)
        .unwrap();

        assert_eq!(
            current_hard(libc::RLIMIT_NOFILE),
            hard_before,
            "apply must NOT lower the hard NOFILE ceiling (the `ulimit: value exceeds hard limit` regression)"
        );

        // The shell rc raising its soft limit above the applied default.
        let raise = core::cmp::min(applied * 4, hard_before);
        set_limit(libc::RLIMIT_NOFILE, Some(raise), false)
            .expect("raising the soft limit must succeed");
        assert!(
            current(libc::RLIMIT_NOFILE) >= applied,
            "soft limit must be raisable above the applied sandbox default"
        );
    }

    /// Run `probe` in a forked child, as the spawn seam's child runs
    /// [`Limits::apply`] after `fork` and before `exec`, and return its exit
    /// code. A hard limit, once lowered, stays lowered for the process, so only
    /// a child may lower one. `probe` must be async-signal-safe: rlimit
    /// syscalls only.
    #[cfg(unix)]
    fn in_forked_child(probe: impl FnOnce() -> i32) -> i32 {
        // SAFETY: the child runs only `probe` (getrlimit/setrlimit, no
        // allocation) and `_exit`, all async-signal-safe after `fork`.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed: {}", io::Error::last_os_error());
        if pid == 0 {
            let code = probe();
            // SAFETY: ends the child without running the parent's destructors.
            unsafe { libc::_exit(code) };
        }
        let mut status = 0;
        // SAFETY: reaping our own child into a valid out-param.
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status), "the probe child did not exit");
        libc::WEXITSTATUS(status)
    }

    /// RULING 2026-10-10 (P1 of the r13 policy hunt): a child that installed
    /// [`Limits::contained`] cannot raise its descriptor limit back, as
    /// `ulimit -n` would (soft and hard to one more than the cap); one that
    /// installed [`Limits::shell_default`] can, which is what makes the first
    /// refusal mean something.
    #[cfg(unix)]
    #[test]
    fn a_contained_limit_cannot_be_raised_back() {
        const RAISED: i32 = 0;
        const REFUSED: i32 = 1;
        const NOT_INSTALLED: i32 = 2;
        let _serialized = nofile_guard();
        let auth = unsafe { Authority::root_authority() };
        let cap: Cap<Sandbox> = auth.grant(Tier::Trusted);
        let open_files = Limits::contained()
            .open_files
            .expect("the set caps open files");
        let hard = current_hard(libc::RLIMIT_NOFILE);
        assert!(
            hard > open_files,
            "test precondition: inherited hard NOFILE ({hard}) must exceed the cap"
        );
        let raise_after = |limits: Limits| {
            in_forked_child(move || {
                if limits.apply(&cap).is_err() {
                    return NOT_INSTALLED;
                }
                let above = (open_files + 1) as libc::rlim_t;
                let lim = libc::rlimit {
                    rlim_cur: above,
                    rlim_max: above,
                };
                // SAFETY: a valid resource id and a fully initialized rlimit.
                if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } == 0 {
                    RAISED
                } else {
                    REFUSED
                }
            })
        };
        assert_eq!(
            raise_after(Limits::contained()),
            REFUSED,
            "a contained program raised its descriptor limit back"
        );
        assert_eq!(
            raise_after(Limits::shell_default()),
            RAISED,
            "Safety's soft limit must stay raisable"
        );
    }
}
