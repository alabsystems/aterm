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
#[cfg(any(windows, test))]
mod job_fold;

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
#[path = "lib_tests.rs"]
mod tests;
