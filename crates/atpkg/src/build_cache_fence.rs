// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Process ownership at the build-cache removal boundary.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use super::{KeepReason, RetainedCheck};
use aterm_uds::holders::PathScan;

pub type Processes<'a> = &'a dyn Fn(&[PathBuf], &[u32]) -> std::io::Result<PathScan>;

fn unused(scan: std::io::Result<PathScan>) -> Result<(), KeepReason> {
    let scan = scan.map_err(|_| KeepReason::ProcessUnavailable)?;
    if let Some(user) = scan.users.first() {
        let how = match user.how {
            "cwd" => "working directory",
            "exe" => "executable",
            "map" => "mapped file",
            _ => "open file",
        };
        return Err(KeepReason::ProcessUsed {
            pid: user.pid,
            how,
            observed: user.path.clone(),
        });
    }
    if let Some((pid, _)) = scan
        .unreadable
        .iter()
        .find(|(_, exe)| !aterm_uds::holders::unix_platform_service(exe.as_deref()))
    {
        return Err(KeepReason::ProcessUnreadable(*pid));
    }
    Ok(())
}

/// A pass's injected process probe; fixtures use an inert arm, never the owner's processes.
pub(super) struct Fence<'a> {
    processes: Processes<'a>,
    told: Cell<bool>,
    kept: Option<RefCell<Vec<RetainedCheck>>>,
}

impl<'a> Fence<'a> {
    pub(super) fn new(processes: Processes<'a>) -> Self {
        Self {
            processes,
            told: Cell::new(false),
            kept: None,
        }
    }

    pub(super) fn with_reporting(mut self) -> Self {
        self.kept = Some(RefCell::new(Vec::new()));
        self
    }

    pub(super) fn note(&self, path: &Path, reason: KeepReason) {
        self.note_with(path, || reason);
    }

    /// Build diagnostic-only evidence only when this pass collects reports.
    pub(super) fn note_with(&self, path: &Path, reason: impl FnOnce() -> KeepReason) {
        if let Some(kept) = &self.kept {
            super::report::add(&mut kept.borrow_mut(), path, reason());
        }
    }

    pub(super) fn admit(&self, path: &Path, reason: KeepReason, allowed: bool) -> bool {
        if !allowed {
            self.note(path, reason);
        }
        allowed
    }

    pub(super) fn retained(&self) -> Vec<RetainedCheck> {
        self.kept
            .as_ref()
            .map_or_else(Vec::new, |kept| kept.borrow().clone())
    }

    /// Both spellings matter after a rename: a running image's recorded path may still name
    /// its original cache. Neither a resumed stage nor its replacement may hide that holder.
    pub(super) fn clear(&self, original: &Path, current: &Path) -> bool {
        self.clear_all(current, &[original, current])
    }

    /// Scan every spelling as before, but report the current candidate alone.
    /// An original image spelling need not still name a directory after staging.
    pub(super) fn clear_all(&self, subject: &Path, paths: &[&Path]) -> bool {
        let mut dirs = Vec::new();
        for &path in paths {
            if !dirs.iter().any(|dir: &PathBuf| dir == path) {
                dirs.push(path.to_path_buf());
            }
            if let Ok(real) = path.canonicalize()
                && !dirs.contains(&real)
            {
                dirs.push(real);
            }
            // The original entry may already have moved. Resolve its surviving parent
            // so a relative path or a symlinked parent still names the kernel's old image.
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            if let Some(name) = path.file_name()
                && let Ok(parent) = parent.canonicalize()
            {
                let original = parent.join(name);
                if !dirs.contains(&original) {
                    dirs.push(original);
                }
            }
        }
        match unused((self.processes)(&dirs, &[std::process::id()])) {
            Ok(()) => true,
            Err(reason) => {
                self.note(subject, reason.clone());
                // One bounded explanation per pass, including a failed or incomplete scan.
                if !self.told.replace(true) {
                    aterm_log::warn!("build cache cleanup check stopped: {reason}");
                }
                false
            }
        }
    }
}

impl Default for Fence<'static> {
    fn default() -> Self {
        #[cfg(not(test))]
        let processes: Processes<'static> = &live;
        #[cfg(test)]
        let processes: Processes<'static> = &|_, _| Ok(PathScan::default());
        Self::new(processes)
    }
}

/// The regular disk pass's kernel reader, retaining this remover's executable and cwd while
/// ignoring only its own lock/directory descriptors. Including self in the reader also keeps
/// legitimate same-image children from being mistaken for a skipped parent's pre-exec fork.
#[cfg(any(not(test), target_os = "macos", target_os = "linux"))]
pub(super) fn live(dirs: &[PathBuf], skip: &[u32]) -> std::io::Result<PathScan> {
    observed(dirs, skip, &kernel)
}

#[cfg(any(not(test), target_os = "macos", target_os = "linux"))]
fn kernel(dirs: &[PathBuf], skip: &[u32]) -> std::io::Result<PathScan> {
    #[cfg(all(debug_assertions, target_vendor = "apple"))]
    {
        match aterm_uds::holders::lab_jail_pids() {
            Some(pids) => aterm_uds::holders::scan_path_users_of(&pids?, dirs, skip),
            None => aterm_uds::holders::scan_path_users(dirs, skip),
        }
    }
    #[cfg(not(all(debug_assertions, target_vendor = "apple")))]
    aterm_uds::holders::scan_path_users(dirs, skip)
}

fn observed(dirs: &[PathBuf], skip: &[u32], processes: Processes<'_>) -> std::io::Result<PathScan> {
    let me = std::process::id();
    let skipped: Vec<u32> = skip.iter().copied().filter(|pid| *pid != me).collect();
    let mut scan = processes(dirs, &skipped)?;
    scan.users.retain(|user| user.pid != me || user.how != "fd");
    Ok(scan)
}

#[cfg(test)]
#[path = "build_cache_fence_tests.rs"]
mod tests;
