// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The anchor attributes are `macro_rules!` ones where Cargo resolved no proc macro
// under them (crates/aterm-spec-anchor/anchor_rules.rs).
#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Resource sandbox for spawned children.
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
    /// POSIX: whether each value also becomes the hard limit, so the program
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
    /// On POSIX these are soft limits under the inherited hard ceiling: a
    /// guardrail against a runaway program that the program or shell rc can
    /// raise (`ulimit -n 10240` or `65536`) without `ulimit` errors at shell
    /// startup. A per-process ceiling would not bound a program that forks
    /// anyway (POSIX has no process cap here). The hostile mode's set is
    /// [`Self::contained`]. On Windows the Job Object binds them.
    #[must_use]
    // #[inline] so the MIR crosses the crate boundary and callers' Trust gates
    // (aterm-pty) verify this body instead of assuming an absent callee.
    #[inline]
    pub fn shell_default() -> Self {
        Limits {
            cpu_seconds: None,
            // macOS refuses (`EINVAL`) any `RLIMIT_AS` below the process's
            // current mapped size (~415 GiB on arm64 before running user code),
            // so a 16 GiB cap cannot be expressed there and a value large
            // enough to be accepted bounds nothing real.
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
    /// as hard limits too, so a hostile contained program cannot raise them
    /// back with `ulimit -n`.
    ///
    /// What binds on POSIX is per process: there is no process cap here, so a
    /// program that forks holds 8192 descriptors in each child, and a shell rc
    /// that tries to raise the descriptor limit past 8192 is refused and keeps
    /// 8192.
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
    /// particular: it caps virtual address space, which CUDA/ML runtimes, the JVM,
    /// Go and the sanitizers reserve far beyond resident use, so any finite cap
    /// breaks real programs while bounding nothing. This mode confines through
    /// the capability gate; [`Self::shell_default`] is the hardened opt-in.
    #[must_use]
    pub fn inherit() -> Self {
        Limits::default()
    }

    /// Apply these limits to the current process. Call in the child, after
    /// `fork`, before `exec`. Requires a `Trusted`+ [`Cap<Sandbox>`].
    ///
    /// The cap gate hard-fails; the `setrlimit` calls are best-effort: every
    /// limit is attempted, so one the OS refuses (a 16 GiB `RLIMIT_AS` on macOS)
    /// cannot keep the others from installing, and the first error is returned
    /// after all were tried.
    ///
    /// An `Err` means confinement did not fully install and the caller must fail
    /// closed: `aterm-pty`'s child `_exit(126)`s before `execve`.
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
    // PROJECTION: the L2 obligation needs only the projection name (Trust does
    // not execute it) onto the model's `<<applied, done>>` in the host's
    // configuration `<<Req, Sup>>`.
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
    /// `Trusted`+ [`Cap<Sandbox>`].
    ///
    /// Query-modify-write: `LimitFlags` already on the job (`KILL_ON_JOB_CLOSE`)
    /// are kept. Memory, CPU time, the active-process cap and
    /// [`Limits::restrict_ui`] are enforced; `open_files` / `file_size` have no
    /// Job Object analog.
    ///
    /// # Errors
    /// `PermissionDenied` if the capability tier is too low; otherwise the first
    /// Job Object OS error; the caller must fail closed (terminate the
    /// never-resumed child).
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

    /// True when every numeric limit is `None` and `restrict_ui` is `false`.
    #[must_use]
    pub const fn is_unrestricted(&self) -> bool {
        self.cpu_seconds.is_none()
            && self.address_space.is_none()
            && self.file_size.is_none()
            && self.open_files.is_none()
            && self.active_processes.is_none()
            && !self.restrict_ui
    }

    /// Combine two [`Limits`] policies by taking the stricter (smaller) bound
    /// for each resource and the boolean OR of `restrict_ui` and `binding`.
    #[must_use]
    pub const fn merge_stricter(self, other: Self) -> Self {
        const fn min_opt_u64(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            match (a, b) {
                (Some(x), Some(y)) => Some(if x < y { x } else { y }),
                (Some(x), None) => Some(x),
                (None, Some(y)) => Some(y),
                (None, None) => None,
            }
        }
        const fn min_opt_u32(a: Option<u32>, b: Option<u32>) -> Option<u32> {
            match (a, b) {
                (Some(x), Some(y)) => Some(if x < y { x } else { y }),
                (Some(x), None) => Some(x),
                (None, Some(y)) => Some(y),
                (None, None) => None,
            }
        }
        Self {
            cpu_seconds: min_opt_u64(self.cpu_seconds, other.cpu_seconds),
            address_space: min_opt_u64(self.address_space, other.address_space),
            file_size: min_opt_u64(self.file_size, other.file_size),
            open_files: min_opt_u64(self.open_files, other.open_files),
            active_processes: min_opt_u32(self.active_processes, other.active_processes),
            restrict_ui: self.restrict_ui || other.restrict_ui,
            binding: self.binding || other.binding,
        }
    }

    /// Encode this [`Limits`] policy into a canonical 38-byte little-endian
    /// wire frame:
    /// - byte `0`: presence bitmask (`bit 0 = cpu_seconds`, `bit 1 = address_space`,
    ///   `bit 2 = file_size`, `bit 3 = open_files`, `bit 4 = active_processes`)
    /// - byte `1`: flags (`bit 0 = restrict_ui`, `bit 1 = binding`)
    /// - bytes `2..10`: `cpu_seconds` (`0` when absent)
    /// - bytes `10..18`: `address_space` (`0` when absent)
    /// - bytes `18..26`: `file_size` (`0` when absent)
    /// - bytes `26..34`: `open_files` (`0` when absent)
    /// - bytes `34..38`: `active_processes` (`0` when absent)
    #[must_use]
    pub const fn encode_wire_le(&self) -> [u8; 38] {
        let mut mask = 0u8;
        let cpu = match self.cpu_seconds {
            Some(v) => {
                mask |= 1 << 0;
                v
            }
            None => 0,
        };
        let aspace = match self.address_space {
            Some(v) => {
                mask |= 1 << 1;
                v
            }
            None => 0,
        };
        let fsize = match self.file_size {
            Some(v) => {
                mask |= 1 << 2;
                v
            }
            None => 0,
        };
        let nofile = match self.open_files {
            Some(v) => {
                mask |= 1 << 3;
                v
            }
            None => 0,
        };
        let nproc = match self.active_processes {
            Some(v) => {
                mask |= 1 << 4;
                v
            }
            None => 0,
        };
        let flags = (self.restrict_ui as u8) | ((self.binding as u8) << 1);
        let c = cpu.to_le_bytes();
        let a = aspace.to_le_bytes();
        let f = fsize.to_le_bytes();
        let n = nofile.to_le_bytes();
        let p = nproc.to_le_bytes();
        [
            mask, flags, c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7], a[0], a[1], a[2], a[3],
            a[4], a[5], a[6], a[7], f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7], n[0], n[1],
            n[2], n[3], n[4], n[5], n[6], n[7], p[0], p[1], p[2], p[3],
        ]
    }

    /// Decode a canonical 38-byte little-endian [`Limits`] wire frame,
    /// rejecting unknown mask/flag bits or non-zero payload fields when the
    /// corresponding presence bit is clear.
    #[must_use]
    pub const fn decode_wire_le(wire: &[u8; 38]) -> Option<Self> {
        let mask = wire[0];
        let flags = wire[1];
        if (mask & !0b0001_1111) != 0 || (flags & !0b0000_0011) != 0 {
            return None;
        }
        let cpu = u64::from_le_bytes([
            wire[2], wire[3], wire[4], wire[5], wire[6], wire[7], wire[8], wire[9],
        ]);
        let aspace = u64::from_le_bytes([
            wire[10], wire[11], wire[12], wire[13], wire[14], wire[15], wire[16], wire[17],
        ]);
        let fsize = u64::from_le_bytes([
            wire[18], wire[19], wire[20], wire[21], wire[22], wire[23], wire[24], wire[25],
        ]);
        let nofile = u64::from_le_bytes([
            wire[26], wire[27], wire[28], wire[29], wire[30], wire[31], wire[32], wire[33],
        ]);
        let nproc = u32::from_le_bytes([wire[34], wire[35], wire[36], wire[37]]);

        let cpu_seconds = if (mask & (1 << 0)) != 0 {
            Some(cpu)
        } else if cpu == 0 {
            None
        } else {
            return None;
        };
        let address_space = if (mask & (1 << 1)) != 0 {
            Some(aspace)
        } else if aspace == 0 {
            None
        } else {
            return None;
        };
        let file_size = if (mask & (1 << 2)) != 0 {
            Some(fsize)
        } else if fsize == 0 {
            None
        } else {
            return None;
        };
        let open_files = if (mask & (1 << 3)) != 0 {
            Some(nofile)
        } else if nofile == 0 {
            None
        } else {
            return None;
        };
        let active_processes = if (mask & (1 << 4)) != 0 {
            Some(nproc)
        } else if nproc == 0 {
            None
        } else {
            return None;
        };
        Some(Self {
            cpu_seconds,
            address_space,
            file_size,
            open_files,
            active_processes,
            restrict_ui: (flags & 1) != 0,
            binding: (flags & 2) != 0,
        })
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

    // Windows: `apply` is a capability-gated no-op — the fail-closed gate still
    // denies a weak cap, and a Trusted cap succeeds without installing any limit.
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
        let _serialized = nofile_guard();
        let auth = unsafe { Authority::root_authority() };
        let cap: Cap<Sandbox> = auth.grant(Tier::Trusted);
        set_limit(libc::RLIMIT_NOFILE, Some(300), false).expect("a known soft limit");
        let before = (current(libc::RLIMIT_CPU), current(libc::RLIMIT_NOFILE));
        Limits::inherit().apply(&cap).unwrap();
        assert_eq!(
            (current(libc::RLIMIT_CPU), current(libc::RLIMIT_NOFILE)),
            before
        );
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

    /// A child that installed [`Limits::contained`] cannot raise its descriptor
    /// limit back (`ulimit -n`), while one that installed [`Limits::shell_default`]
    /// can.
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

    /// Each limit lands on its own resource, and one above the hard ceiling is clamped under
    /// it rather than refused (a refusal fails the child closed: no shell where the host's
    /// hard `RLIMIT_NOFILE` is below the Safety set's), a binding one too, whose hard limit
    /// is the clamped value and never the asked one (raising a hard limit is refused). The
    /// child lowers its own ceiling.
    #[cfg(unix)]
    #[test]
    fn each_limit_lands_on_its_resource_clamped_under_the_hard_ceiling() {
        const CEILING: u64 = 1000;
        let auth = unsafe { Authority::root_authority() };
        let cap: Cap<Sandbox> = auth.grant(Tier::Trusted);
        let mismatch = |binding: bool| {
            in_forked_child(move || {
                let read = |resource| {
                    let mut lim = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    // SAFETY: a valid resource id and out-param.
                    unsafe { libc::getrlimit(resource, &mut lim) };
                    lim
                };
                // The soft limit starts below the ceiling, so reading the ceiling back
                // means the asked limit landed (clamped), not that it was left alone.
                let lowered = libc::rlimit {
                    rlim_cur: (CEILING / 2) as libc::rlim_t,
                    rlim_max: CEILING as libc::rlim_t,
                };
                // SAFETY: a valid resource id and a fully initialized rlimit.
                if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lowered) } != 0 {
                    return 1;
                }
                let (cpu, fsize) = (1u64 << 20, 1u64 << 40);
                let want_cpu = cpu.min(read(libc::RLIMIT_CPU).rlim_max);
                let want_fsize = fsize.min(read(libc::RLIMIT_FSIZE).rlim_max);
                let limits = Limits {
                    cpu_seconds: Some(cpu),
                    file_size: Some(fsize),
                    open_files: Some(CEILING + 1),
                    binding,
                    ..Limits::inherit()
                };
                if limits.apply(&cap).is_err() {
                    return 2;
                }
                let nofile = read(libc::RLIMIT_NOFILE);
                let got = [
                    (read(libc::RLIMIT_CPU).rlim_cur, want_cpu),
                    (read(libc::RLIMIT_FSIZE).rlim_cur, want_fsize),
                    (nofile.rlim_cur, CEILING),
                    (nofile.rlim_max, CEILING),
                ];
                match got.iter().position(|&(got, want)| got != want) {
                    Some(row) => 10 + row as i32,
                    None => 0,
                }
            })
        };
        for binding in [false, true] {
            assert_eq!(
                mismatch(binding),
                0,
                "binding {binding}: 1: setup, 2: refused, 10+: the row read back wrong"
            );
        }
    }

    #[test]
    fn limits_wire_codec_and_merge_stricter_round_trip() {
        assert!(Limits::inherit().is_unrestricted());
        assert!(!Limits::shell_default().is_unrestricted());
        let merged = Limits::shell_default().merge_stricter(Limits {
            cpu_seconds: Some(300),
            open_files: Some(1024),
            binding: true,
            ..Limits::inherit()
        });
        assert_eq!(merged.cpu_seconds, Some(300));
        assert_eq!(merged.open_files, Some(1024));
        assert!(merged.binding);
        assert!(merged.restrict_ui);

        let wire = merged.encode_wire_le();
        assert_eq!(Limits::decode_wire_le(&wire), Some(merged));
        let mut bad = wire;
        bad[0] = 0xff;
        assert_eq!(Limits::decode_wire_le(&bad), None);
    }
}
