// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resource sandbox for spawned children (ATERM_DESIGN WS-G).
//!
//! aterm spawns a child `$SHELL`; this confines that child's resource use with
//! POSIX `setrlimit` bounds, applied in the child after `fork` and before `exec`
//! (so the limits are inherited by the shell and everything it runs). Installing
//! a sandbox is a privileged effect, so [`Limits::apply`] requires a
//! [`Cap<Sandbox>`] from `aterm-cap` (capability-gated; the cap cannot be
//! struct-literal-forged outside `aterm-cap` — see that crate for the exact,
//! honest scope of the guarantee, and §5.4 for the stronger sealed mint).
//!
//! This is the portable resource-limit layer. A macOS Seatbelt / Endpoint
//! Security profile (filesystem/network scoping) is a separate, platform-specific
//! lane on top of it and is not implemented here.
//!
//! Platform split: the cap gate and the `Limits` policy surface are shared; the
//! actuator is per-platform (`src/unix.rs` = the POSIX `setrlimit` loop,
//! `src/windows.rs` = a documented, capability-gated NO-OP until the Job Object
//! resource lane lands). [`rlimits_actuated`] tells callers which one they got so
//! the launcher startup notices stay honest.
//!
//! STATUS (per §0.1): the cap gate and `setrlimit` application are tested (the
//! application is verified by reading the limit back); not yet Trust-proven.

use std::io;

use aterm_cap::{Cap, Tier};

// Per-platform actuator behind one module name (module split, not inline cfg):
// unix = the real setrlimit loop; windows = the honest no-op.
#[cfg(unix)]
#[path = "unix.rs"]
mod imp;
#[cfg(windows)]
#[path = "windows.rs"]
mod imp;

/// The effect a capability authorizes here: installing a resource sandbox.
pub enum Sandbox {}

/// POSIX resource limits to apply. `None` leaves the corresponding limit
/// unchanged. Each value is installed as the **soft** limit; the inherited
/// **hard** ceiling is PRESERVED (never lowered) so the spawned `$SHELL` can
/// still raise its own soft limit from its rc — see `set_limit` in the unix
/// actuator. (On Windows these values are inert — no rlimit analogue is
/// installed; see [`rlimits_actuated`].)
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
    /// Max number of concurrently-active processes in the sandbox **Job Object**
    /// (Windows `ActiveProcessLimit` / `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`) — a
    /// spawn-storm / fork-bomb bound the kernel enforces on the child and
    /// everything it spawns. `None` leaves the process count unbounded.
    /// Windows-actuated only (via [`Limits::apply_to_job`]); inert on POSIX,
    /// whose `setrlimit` lane has no matching per-job class (symmetric to how
    /// `open_files` / `file_size` are inert on Windows).
    pub active_processes: Option<u32>,
    /// Windows only: install Job Object **UI restrictions** on the sandbox job
    /// (deny clipboard read/write, desktop switch, display / system-parameter
    /// changes, global atoms, and `ExitWindows`) so a hostile child cannot break
    /// out through the shared window station — log the user off, scrape the
    /// clipboard, or change the display. `false` leaves the job's UI access
    /// unrestricted (the daily-driver default). Inert off Windows.
    pub restrict_ui: bool,
}

impl Limits {
    /// A generous default for an interactive shell: cap address space and fds, but
    /// leave CPU and file size unbounded (an interactive shell legitimately runs
    /// long and writes large files).
    #[must_use]
    // #[inline] so the MIR crosses the crate boundary: callers' Trust gates
    // (aterm-pty) bundle and VERIFY this body instead of assuming an absent
    // callee. Semantics unchanged.
    #[inline]
    pub fn shell_default() -> Self {
        Limits {
            cpu_seconds: None,
            // macOS rejects ANY finite `RLIMIT_AS` (only `RLIM_INFINITY` is
            // accepted — `setrlimit` returns `EINVAL` otherwise), so leave the
            // address space unbounded there and rely on the other limits.
            address_space: if cfg!(target_os = "macos") {
                None
            } else {
                Some(16 * 1024 * 1024 * 1024) // 16 GiB
            },
            file_size: None,
            open_files: Some(8192),
            // Bound concurrently-active processes so a runaway or hostile child
            // cannot fork-bomb the box — generous enough for a parallel build,
            // low enough to matter. Windows Job Object only; inert on POSIX.
            active_processes: Some(512),
            // Hardened modes are for confining an untrusted shell, so lock the
            // job's window-station access (Windows only; inert on POSIX).
            restrict_ui: true,
        }
    }

    /// The PERMISSIVE limits for the daily-driver modes (Master / User): every
    /// limit `None`, so the spawned shell INHERITS the launching login shell's
    /// `rlimit`s unchanged — a terminal must not constrain the programs you run more
    /// than the shell that started it would. In particular it imposes NO `RLIMIT_AS`:
    /// that caps VIRTUAL address space, which CUDA/ML runtimes, the JVM, Go, and the
    /// sanitizers all RESERVE far in excess of resident use, so any finite cap breaks
    /// legitimate programs while bounding nothing real. Confinement in these modes is
    /// the capability gate (what the shell may DO), not a blanket memory cap. The
    /// hardened [`Self::shell_default`] caps (opted into via Safety / Containment)
    /// are unchanged.
    #[must_use]
    pub fn inherit() -> Self {
        Limits::default()
    }

    /// Apply these limits to the CURRENT process. Call in the child, after
    /// `fork`, before `exec`. Requires a `Trusted`+ [`Cap<Sandbox>`].
    ///
    /// The cap gate hard-fails, but the individual `setrlimit` calls are applied
    /// BEST-EFFORT: a resource the OS does not support (e.g. `RLIMIT_AS` on
    /// macOS) must NOT prevent the limits that DO work (`RLIMIT_NOFILE`) from
    /// being installed. Every limit is attempted; the first per-limit error is
    /// returned only after all have been tried, so one unsupported resource can
    /// never silently leave the child unconfined.
    ///
    /// The returned `Result` MUST NOT be discarded by a forking spawn seam: an
    /// `Err` here means confinement did not fully install, and the caller is
    /// required to fail closed (do NOT exec an unconfined child). `aterm-pty`'s
    /// child does exactly this — it `_exit(126)`s before `execve` on `Err`
    /// (ATERM_DESIGN §5.6, exit-before-exec).
    ///
    /// # Errors
    /// `PermissionDenied` if the capability tier is too low; otherwise the first
    /// `setrlimit` OS error encountered (after attempting every limit).
    ///
    /// SPEC: this is the real implementation of the `Apply` action of the external
    /// `Sandbox.tla` model (TRUST_NATIVE_TLA Phase 2, CONFINEMENT family). The spec's
    /// `AllSupportedApplied` invariant — once apply has run, EVERY restriction the
    /// policy *requested* that the OS *supports* is actually installed — is exactly
    /// the macOS no-op regression this best-effort-per-limit loop fixes: a requested
    /// limit the OS supports is never silently skipped because an earlier unsupported
    /// one (e.g. `RLIMIT_AS` on macOS) errored. Tier-1 conformance drives this method
    /// and projects `<<requested, supported, applied, done>>`
    /// (`tests/conformance_sandbox.rs`).
    // PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): `Apply` projects the real
    // best-effort-per-limit apply loop onto the spec's `<<requested, supported,
    // applied, done>>` — the projection `conformance_sandbox.rs` drives in Tier-1.
    // The L2 obligation requires the projection NAME be present (Trust does not
    // execute it); `aterm_sandbox::Sandbox::project_apply` is that witness.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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

    /// Apply these limits to a child's Windows **Job Object** so the kernel
    /// enforces them on the child and everything it spawns — the Windows analog
    /// of [`Self::apply`]'s POSIX `setrlimit` lane. Windows has no `fork` seam to
    /// run `setrlimit` in before exec, so resource confinement is installed on
    /// the job the ConPTY spawn assigns the (still-suspended) child to, BEFORE it
    /// resumes. Requires a `Trusted`+ [`Cap<Sandbox>`] — SEC-2: a weak cap can
    /// never actuate, exactly like [`Self::apply`].
    ///
    /// This is query-modify-write over the job's extended-limit info, so any
    /// `LimitFlags` the spawn seam already installed (`KILL_ON_JOB_CLOSE`) are
    /// preserved. See the Windows actuator for the exact [`Limits`] → job-limit
    /// mapping and its honest scope: memory, CPU-time, the active-process cap,
    /// and (when [`Limits::restrict_ui`] is set) Job Object UI restrictions are
    /// enforced; `open_files` / `file_size` have no Job Object analog and stay
    /// unactuated.
    ///
    /// # Errors
    /// `PermissionDenied` if the capability tier is too low; otherwise the first
    /// Job Object OS error. Like [`Self::apply`], an `Err` here means confinement
    /// did not install and the caller MUST fail closed (terminate the
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

/// Whether this build/platform actually installs the requested resource
/// limits at the spawn seam. `true` on POSIX (`setrlimit` in the child before
/// exec); `false` on Windows, where [`Limits::apply`] is a capability-gated
/// NO-OP (a Job Objects lane is the follow-up) — callers must print the
/// one-line posture notice so an unlimited child is never silent. Mirrors
/// `aterm_containment::os_sandbox_actuated` in shape and honesty.
#[must_use]
pub const fn rlimits_actuated() -> bool {
    cfg!(unix)
}

/// Whether this build/platform installs the requested resource limits on the
/// spawned child's **Job Object** at the spawn seam. `true` on Windows, where
/// [`Limits::apply_to_job`] folds the memory/CPU limits into the job the ConPTY
/// seam assigns the child to (there is no POSIX `setrlimit` lane there — see
/// [`rlimits_actuated`]); `false` elsewhere, where confinement is `setrlimit`
/// instead. The two predicates are mutually exclusive per platform, so a
/// launcher prints exactly one honest posture line. Note this reports the
/// KERNEL lane exists on this platform; the child is only actually confined
/// once the spawn seam calls [`Limits::apply_to_job`] against its job.
#[must_use]
pub const fn job_limits_actuated() -> bool {
    cfg!(windows)
}

/// The per-restriction APPLY rule of `Limits::apply`, factored out as a pure
/// function so the fail-closed "requested ∧ supported ⇒ applied" discipline is
/// testable WITHOUT mutating the process-wide rlimits (and is the seam the
/// `Sandbox.tla` Tier-1 conformance projects).
///
/// This is the body of the spec's (correct, `Buggy=FALSE`) `Apply` action, slot by
/// slot: `applied[n]' = applied[n] ∨ (requested[n] ∧ supported[n])`. The real
/// [`Limits::apply`] loop attempts every *requested* limit (a `Some(_)` field) and
/// the OS accepts it iff that resource is *supported*; an unsupported one is skipped
/// best-effort and never blocks the supported ones (the macOS `RLIMIT_AS` no-op the
/// spec's `AllSupportedApplied` invariant forbids). `applied` here is the prior
/// applied set (all-FALSE before the first apply) so the rule is monotone/idempotent,
/// exactly as the spec models it.
///
/// TOTAL (Trust L0 panic-free): the three slices are the same K restriction
/// slots, so equal lengths are the caller's invariant, not an assert —
/// mismatched lengths are clamped to the shortest (and the slot count to 64)
/// instead of panicking, a no-op for every real caller (K = 4 today).
#[must_use]
pub fn apply_step(requested: &[bool], supported: &[bool], applied: &[bool]) -> Vec<bool> {
    // The three slices are the same K restriction slots, so their lengths are
    // equal for every real caller (the Tier-1 conformance harness always passes
    // K-length vectors). Clamp to the shortest instead of asserting equality:
    // a reachable assert is a Trust L0 refutation, and under the documented
    // invariant the clamp is a no-op — same `k`, same slots, same rule.
    let k = requested.len();
    let k = if supported.len() < k {
        supported.len()
    } else {
        k
    };
    let k = if applied.len() < k { applied.len() } else { k };
    // Slot-count bound: `k` counts restriction slots (4 today — `Limits` has
    // four fields — and every kernel defines only ~16 `RLIMIT_*` resources),
    // so clamping at 64 is a no-op for every real caller.
    let k = k.min(64);
    // Allocate the slot buffer at a CONSTANT size and truncate to `k`: the
    // prover's bulk-allocation budget checks the count inside `from_elem`'s
    // own frame, where a caller-side clamp on a variable count is invisible
    // (it havocs the count — `.collect()` and `vec![false; k]` both refuted),
    // but a constant count is provably bounded. `truncate` keeps `len == k`,
    // so the result is behavior-identical. The fill loop then writes each
    // slot with the spec rule via total `get` accesses (`n < k <= len` of
    // every slice, so no `unwrap_or`/`get_mut` guard ever fires — they exist
    // to keep the function panic-free by construction).
    let mut out = vec![false; 64];
    out.truncate(k);
    let mut n = 0usize;
    while n < k {
        let was = applied.get(n).copied().unwrap_or(false);
        let req = requested.get(n).copied().unwrap_or(false);
        let sup = supported.get(n).copied().unwrap_or(false);
        if let Some(slot) = out.get_mut(n) {
            *slot = was || (req && sup);
        }
        // The `n < k` guard makes this add exact (`n + 1 <= k <= usize::MAX`,
        // it can never wrap); `wrapping_add` states that as a fact instead of
        // leaving an overflow obligation for the interval engine.
        n = n.wrapping_add(1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_cap::Authority;
    #[cfg(unix)]
    use imp::{RlimitResource, set_limit};

    // `RlimitResource` (not a bare `c_int`): on Linux the libc RLIMIT_* constants
    // and `getrlimit`'s first arg are `__rlimit_resource_t` (u32), so a `c_int`
    // parameter mismatches and the test build does not compile there.
    #[cfg(unix)]
    fn current(resource: RlimitResource) -> u64 {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: valid resource id + out-param.
        let rc = unsafe { libc::getrlimit(resource, &mut lim) };
        assert_eq!(rc, 0, "getrlimit failed");
        lim.rlim_cur
    }

    /// The current HARD ceiling (`rlim_max`) of `resource`.
    // `RlimitResource` (not a bare `c_int`): the libc RLIMIT_* constants and
    // `getrlimit`'s arg are `__rlimit_resource_t` (u32) on Linux, so a `c_int`
    // parameter mismatches and the test build does not compile there.
    #[cfg(unix)]
    fn current_hard(resource: RlimitResource) -> u64 {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: valid resource id + out-param.
        let rc = unsafe { libc::getrlimit(resource, &mut lim) };
        assert_eq!(rc, 0, "getrlimit failed");
        lim.rlim_max
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
    // denies a weak cap (SEC-2 parity with the Unix child's exit-before-exec), a
    // Trusted cap succeeds without installing any limit, and the posture predicate
    // says so honestly (the launchers print the one-line notice from it).
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
        assert!(
            !rlimits_actuated(),
            "Windows must report rlimits NOT actuated (honest posture)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_reports_rlimits_actuated() {
        assert!(rlimits_actuated(), "POSIX setrlimit lane is real");
    }

    #[cfg(unix)]
    #[test]
    fn none_limits_are_a_no_op() {
        let auth = unsafe { Authority::root_authority() };
        let cap: Cap<Sandbox> = auth.grant(Tier::Trusted);
        // All None -> Ok and nothing changed.
        let before = current(libc::RLIMIT_CPU);
        Limits::default().apply(&cap).unwrap();
        assert_eq!(current(libc::RLIMIT_CPU), before);
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_limit_does_not_block_the_working_ones() {
        // Regression: `RLIMIT_AS` EINVALs on macOS; the old `?`-early-return
        // there skipped the `RLIMIT_NOFILE` that DOES work, so the child got
        // ZERO confinement. Applying an (often-unsupported) huge AS alongside a
        // small NOFILE must STILL install NOFILE — best-effort per limit.
        let auth = unsafe { Authority::root_authority() };
        let cap: Cap<Sandbox> = auth.grant(Tier::Trusted);
        let target = 256u64;
        // 64 TiB AS: macOS rejects it (EINVAL), Linux accepts it; either way
        // NOFILE must land. (Discard the Result — on macOS apply() now reports
        // the AS error AFTER applying NOFILE.)
        let _ = Limits {
            address_space: Some(64 * 1024 * 1024 * 1024 * 1024),
            open_files: Some(target),
            ..Default::default()
        }
        .apply(&cap);
        assert_eq!(
            current(libc::RLIMIT_NOFILE),
            target,
            "NOFILE must be applied even when an earlier limit is unsupported"
        );
    }

    #[test]
    fn shell_default_omits_unsupported_address_space_on_macos() {
        // The REAL production value: macOS must NOT request RLIMIT_AS (it would
        // EINVAL and — before the best-effort fix — abort the whole apply, so
        // the child was unconfined). Construction-only: no setrlimit, no
        // process-wide fd-limit side effect / test-ordering hazard.
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
        assert_eq!(
            d.open_files,
            Some(8192),
            "the working NOFILE limit must remain"
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_actually_sets_the_limit() {
        // Lower RLIMIT_NOFILE to a value still far above what a test needs, then
        // read it back to prove `apply` performed the syscall. Lowering NOFILE is
        // safe (we only need a handful of fds) and is reversible up to the hard
        // limit if anything later raises it.
        let auth = unsafe { Authority::root_authority() };
        let cap: Cap<Sandbox> = auth.grant(Tier::Certified); // >= Trusted
        let target = 256u64;
        Limits {
            open_files: Some(target),
            ..Default::default()
        }
        .apply(&cap)
        .unwrap();
        assert_eq!(current(libc::RLIMIT_NOFILE), target);
    }

    #[cfg(unix)]
    #[test]
    fn apply_preserves_the_hard_ceiling_so_a_shell_can_raise_its_soft_limit() {
        // REGRESSION GUARD — `/Users//.../.zshrc:ulimit:N: value exceeds hard limit`.
        // The User-mode sandbox runs the user's $SHELL transparently, so it must
        // install its limit as a SOFT default and LEAVE THE HARD CEILING ALONE.
        // The old code set both soft AND hard to the requested value, clamping the
        // hard NOFILE to 8192; a `.zshrc` doing `ulimit -n 65536` then aborted shell
        // startup. This guard fails if anyone reintroduces a hard-limit clamp: it
        // proves (1) the hard ceiling is unchanged by apply, and (2) the soft limit
        // is still raisable above the applied value, up to that preserved ceiling —
        // exactly the `.zshrc` case. The earlier tests only checked the soft limit,
        // which is why the regression slipped through.
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

        // Emulate the shell rc raising its soft limit above the applied default —
        // this must SUCCEED now that the ceiling is preserved.
        let raise = core::cmp::min(applied * 4, hard_before);
        set_limit(libc::RLIMIT_NOFILE, Some(raise)).expect("raising the soft limit must succeed");
        assert!(
            current(libc::RLIMIT_NOFILE) >= applied,
            "soft limit must be raisable above the applied sandbox default"
        );
    }
}
