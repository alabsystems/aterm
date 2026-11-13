// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Security panel *Move to Trash* worker for conflicting bundles ([`atpkg::copies::trash`]).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aterm_containment::consent::{self, Claimants, Retired};

/// The trash tool macOS 15 and later ship.
pub(crate) const TRASH_TOOL: &str = atpkg::copies::TRASH_TOOL;

/// Whether [`TRASH_TOOL`] is on this Mac.
pub(crate) fn trash_tool_present() -> bool {
    static PRESENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRESENT.get_or_init(|| Path::new(TRASH_TOOL).is_file())
}

/// Injected worker reaches.
#[derive(Clone, Copy)]
pub(crate) struct RetireArms {
    census: fn(&str) -> Claimants,
    unchanged: fn(&consent::Claimant, &str) -> bool,
    in_use: fn(&Path) -> Option<bool>,
    trash: fn(&Path) -> Result<(), String>,
}

impl RetireArms {
    const fn live() -> Self {
        Self {
            census: consent::claimants_for,
            unchanged: consent::still_claims,
            in_use: atpkg::gc::runs_from,
            trash: atpkg::copies::trash,
        }
    }

    const fn inert() -> Self {
        Self {
            census: inert_census,
            unchanged: inert_unchanged,
            in_use: inert_in_use,
            trash: inert_trash,
        }
    }
}

fn inert_census(_bundle_id: &str) -> Claimants {
    Claimants::default()
}

const fn inert_unchanged(_copy: &consent::Claimant, _bundle_id: &str) -> bool {
    false
}

const fn inert_in_use(_path: &Path) -> Option<bool> {
    None
}

fn inert_trash(_path: &Path) -> Result<(), String> {
    Err("this instance does not move files".to_string())
}

/// One owner press's outcome, per copy.
pub(crate) type RetireReport = Vec<(PathBuf, Retired)>;

#[derive(Default)]
struct Slot {
    live: bool,
    report: Option<RetireReport>,
    arrived: bool,
}

/// At most one worker, and the last report.
pub(crate) struct RetireState {
    arms: RetireArms,
    shared: Arc<Mutex<Slot>>,
}

impl RetireState {
    /// `inert()` for a headless instance, the live arms otherwise.
    pub(crate) fn new(headless: bool) -> Self {
        Self {
            arms: if headless {
                RetireArms::inert()
            } else {
                RetireArms::live()
            },
            shared: Arc::default(),
        }
    }

    #[cfg(test)]
    fn with_arms(arms: RetireArms) -> Self {
        Self {
            arms,
            shared: Arc::default(),
        }
    }

    /// The worker's slot, locked.
    fn retire_slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.shared.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Start the worker for the copies the owner was shown.
    pub(crate) fn start(
        &self,
        bundle_id: String,
        plan: Vec<PathBuf>,
        wake: impl FnOnce() + Send + 'static,
    ) -> bool {
        {
            let mut slot = self.retire_slot();
            if slot.live {
                return false;
            }
            slot.live = true;
        }
        let arms = self.arms;
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("aterm-claimant-retire".to_owned())
            .spawn(move || {
                let fresh = (arms.census)(&bundle_id);
                let report = consent::retire_claimants(
                    &plan,
                    &fresh,
                    |copy| (arms.unchanged)(copy, &bundle_id),
                    arms.in_use,
                    arms.trash,
                );
                {
                    let mut slot = shared.lock().unwrap_or_else(|p| p.into_inner());
                    slot.live = false;
                    slot.report = Some(report);
                    slot.arrived = true;
                }
                wake();
            });
        if spawned.is_err() {
            self.retire_slot().live = false;
            return false;
        }
        true
    }

    /// Whether a worker is running.
    pub(crate) fn is_live(&self) -> bool {
        self.retire_slot().live
    }

    /// The last completed press's report.
    pub(crate) fn report(&self) -> Option<RetireReport> {
        self.retire_slot().report.clone()
    }

    /// Put a finished press's report in the slot, for the host's status tests.
    #[cfg(test)]
    pub(crate) fn set_report_for_test(&self, report: RetireReport) {
        self.retire_slot().report = Some(report);
    }

    /// Whether a report arrived since the last call.
    pub(crate) fn take_arrival(&self) -> bool {
        std::mem::take(&mut self.retire_slot().arrived)
    }
}

/// The block's status line once a press's moves are done.
pub(crate) fn summary(moved: usize, of: usize) -> String {
    let copies = if of == 1 { "copy" } else { "copies" };
    match moved {
        0 => "Nothing was moved.".to_string(),
        m if m == of => format!("Moved {m} {copies} to the Trash."),
        m if of - m == 1 => {
            format!("Moved {m} of {of} copies to the Trash; the other stayed where it was.")
        }
        m => format!("Moved {m} of {of} copies to the Trash; the others stayed where they were."),
    }
}

#[cfg(test)]
#[path = "consent_retire_tests.rs"]
mod tests;
