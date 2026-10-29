// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Resource sandbox for spawned children ([`Limits`], [`Limits::apply`], [`Cap<Sandbox>`]).

use std::io;

use aterm_cap::{Cap, Tier};

#[cfg(unix)]
#[path = "unix.rs"]
mod imp;
#[cfg(windows)]
#[path = "windows.rs"]
mod imp;

/// The effect a capability authorizes here.
pub enum Sandbox {}

impl Sandbox {
    /// Project `(applied, done)` for [`aterm_models_core::sandbox_model`]'s `Apply` step.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn project_apply(requested: bool, result: &io::Result<()>) -> (i64, i64) {
        let applied = if requested && result.is_ok() { 1 } else { 0 };
        (applied, 1)
    }
}

/// Resource limits to apply.
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
    /// Max concurrently-active processes in the sandbox Job Object (Windows `ActiveProcessLimit`).
    pub active_processes: Option<u32>,
    /// Windows only: install Job Object UI restrictions.
    pub restrict_ui: bool,
    /// POSIX: whether each value also becomes the hard limit, so the program cannot raise it back.
    pub binding: bool,
}

impl Limits {
    /// The Safety-mode set for an interactive shell.
    #[must_use]
    #[inline]
    pub const fn shell_default() -> Self {
        Limits {
            cpu_seconds: None,
            address_space: if cfg!(target_os = "macos") {
                None
            } else {
                // 16 GiB
                Some(16 * 1024 * 1024 * 1024)
            },
            file_size: None,
            open_files: Some(8192),
            active_processes: Some(512),
            restrict_ui: true,
            binding: false,
        }
    }

    /// The Containment-mode set.
    #[must_use]
    #[inline]
    pub const fn contained() -> Self {
        Limits {
            binding: true,
            ..Self::shell_default()
        }
    }

    /// The daily-driver limits (User mode).
    #[inline]
    #[must_use]
    pub const fn inherit() -> Self {
        Limits {
            cpu_seconds: None,
            address_space: None,
            file_size: None,
            open_files: None,
            active_processes: None,
            restrict_ui: false,
            binding: false,
        }
    }

    /// Apply these limits to the current process.
    ///
    /// # Errors
    /// `PermissionDenied` if the capability tier is too low.
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

    /// Apply these limits to the Job Object of a still-suspended ConPTY child.
    ///
    /// # Errors
    /// `PermissionDenied` if the capability tier is too low.
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
        // SAFETY: Valid resource id + out-param.
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
        let snapshot = || {
            [
                libc::RLIMIT_CPU,
                libc::RLIMIT_AS,
                libc::RLIMIT_FSIZE,
                libc::RLIMIT_NOFILE,
            ]
            .map(|res| {
                let lim = rlimit(res);
                (lim.rlim_cur, lim.rlim_max)
            })
        };
        let before = snapshot();
        Limits::inherit().apply(&cap).unwrap();
        assert_eq!(snapshot(), before);
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_limit_does_not_block_the_working_ones() {
        let _serialized = nofile_guard();
        let auth = unsafe { Authority::root_authority() };
        let cap: Cap<Sandbox> = auth.grant(Tier::Trusted);
        let target = 256u64;
        let hard_before = current_hard(libc::RLIMIT_NOFILE);
        assert!(
            hard_before >= target,
            "test precondition: inherited hard NOFILE ({hard_before}) must be >= {target}"
        );
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
        let d = Limits::shell_default();
        #[cfg(target_os = "macos")]
        assert_eq!(
            d.address_space, None,
            "macOS must not request a finite RLIMIT_AS"
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            d.address_space,
            Some(16 * 1024 * 1024 * 1024),
            "non-macOS should bound the address space to 16 GiB"
        );
        assert_eq!(
            d.open_files,
            Some(8192),
            "the working NOFILE limit must remain"
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_preserves_the_hard_ceiling_so_a_shell_can_raise_its_soft_limit() {
        let _serialized = nofile_guard();
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

        let raise = core::cmp::min(applied * 4, hard_before);
        set_limit(libc::RLIMIT_NOFILE, Some(raise), false)
            .expect("raising the soft limit must succeed");
        assert_eq!(
            current(libc::RLIMIT_NOFILE),
            raise,
            "soft limit must raise to the requested value under the preserved hard ceiling"
        );
    }

    /// Run `probe` in a forked child, as the spawn seam's child runs
    /// [`Limits::apply`] after `fork` and before `exec`, and return its exit
    /// code.
    #[cfg(unix)]
    fn in_forked_child(probe: impl FnOnce() -> i32) -> i32 {
        // SAFETY: the child runs only `probe` (getrlimit/setrlimit, no
        // allocation) and `_exit`, all async-signal-safe after `fork`.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed: {}", io::Error::last_os_error());
        if pid == 0 {
            let code = probe();
            // SAFETY: Ends the child without running the parent's destructors.
            unsafe { libc::_exit(code) };
        }
        let mut status = 0;
        // SAFETY: Reaping our own child into a valid out-param.
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
                // SAFETY: A valid resource id and a fully initialized rlimit.
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
    /// is the clamped value and never the asked one (raising a hard limit is refused).
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
                    // SAFETY: A valid resource id and out-param.
                    unsafe { libc::getrlimit(resource, &mut lim) };
                    lim
                };
                let lowered = libc::rlimit {
                    rlim_cur: (CEILING / 2) as libc::rlim_t,
                    rlim_max: CEILING as libc::rlim_t,
                };
                // SAFETY: A valid resource id and a fully initialized rlimit.
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
    fn apply_conforms_to_the_sandbox_model_and_rejects_mutants() {
        use aterm_models_core::sandbox_model;
        use aterm_spec::interp;
        use aterm_spec::xref::StepEvidence;

        #[cfg(unix)]
        let _serialized = nofile_guard();
        let auth = unsafe { Authority::root_authority() };
        let trusted: Cap<Sandbox> = auth.grant(Tier::Trusted);
        let untrusted: Cap<Sandbox> = auth.grant(Tier::Untrusted);

        let base = sandbox_model();
        let mut ev = StepEvidence::new("sandbox");
        let scenarios: [i64; 4] = [0, 1, 2, 3];
        let mut real_trace = Vec::with_capacity(scenarios.len());

        for idx in scenarios {
            let (req, sup) = match idx {
                0 => (1i64, 1i64),
                1 => (0, 1),
                2 => (1, 0),
                _ => (0, 0),
            };
            let model = interp::with_consts(&base, &[("Req", req), ("Sup", sup)]);
            let mut expected = model.init_state();
            assert!(model.fire("Apply", &mut expected));

            let limits = if req == 1 {
                Limits {
                    open_files: Some(256),
                    ..Limits::inherit()
                }
            } else {
                Limits::inherit()
            };
            let cap = if sup == 1 { &trusted } else { &untrusted };
            let result = ev.step("Apply", || limits.apply(cap));
            let projected = Sandbox::project_apply(req == 1, &result);
            assert_eq!(
                projected,
                (expected["applied"], expected["done"]),
                "Req={req} Sup={sup}: {result:?}"
            );
            real_trace.push(projected);
        }

        let audit = ev.audit();
        assert!(audit.is_complete(), "{}", audit.summary());

        for dial in [1i64, 2] {
            let mutant_base = interp::with_buggy(&base, dial);
            let mutant_trace: Vec<(i64, i64)> = [(1i64, 1i64), (0, 1), (1, 0), (0, 0)]
                .into_iter()
                .map(|(req, sup)| {
                    let m = interp::with_consts(&mutant_base, &[("Req", req), ("Sup", sup)]);
                    let mut s = m.init_state();
                    assert!(m.fire("Apply", &mut s));
                    (s["applied"], s["done"])
                })
                .collect();
            assert_ne!(
                mutant_trace, real_trace,
                "Buggy={dial} was not distinguished from Limits::apply"
            );
        }
    }
}
