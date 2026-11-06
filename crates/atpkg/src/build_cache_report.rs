// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Diagnostic observations from the existing cleanup checks. These records never
//! authorize a deletion and are deliberately separate from removal intents/outcomes.

use std::path::{Path, PathBuf};

/// An existing cleanup guard that did not admit a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeepReason {
    ProcessUsed {
        pid: u32,
        how: &'static str,
        observed: PathBuf,
    },
    ProcessUnreadable(u32),
    ProcessUnavailable,
    InventoryUnavailable,
    CargoLocks,
    InventoryChanged,
    PermissionChanged,
    Custody,
    CannotStage,
    NoPartsStaged,
    IntentDeclined,
    StaleSetChanged,
    #[cfg(unix)]
    RegistrationChanged,
    #[cfg(unix)]
    PackageChanged,
    #[cfg(unix)]
    CoordinationUnavailable,
}

impl std::fmt::Display for KeepReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProcessUsed { pid, how, observed } => write!(
                f,
                "process {pid} was observed using a checked removal path {} ({how})",
                observed.display()
            ),
            Self::ProcessUnreadable(pid) => write!(f, "process {pid} could not be ruled out"),
            Self::ProcessUnavailable => f.write_str("the process ownership scan was unavailable"),
            Self::InventoryUnavailable => f.write_str("the bounded cache inventory is unavailable"),
            Self::CargoLocks => f.write_str("its Cargo locks could not all be taken"),
            Self::InventoryChanged => {
                f.write_str("its cache inventory could not be confirmed unchanged and complete")
            }
            Self::PermissionChanged => f.write_str("the cleanup caller did not permit this step"),
            Self::Custody => {
                f.write_str("its custody or bounded ancestry check did not admit removal")
            }
            Self::CannotStage => f.write_str("its name cannot be safely staged for removal"),
            Self::NoPartsStaged => f.write_str("no cache parts were staged for removal"),
            Self::IntentDeclined => f.write_str("the cleanup pass declined a removal step"),
            Self::StaleSetChanged => {
                f.write_str("no stale parts met the cleanup policy when rechecked")
            }
            #[cfg(unix)]
            Self::RegistrationChanged => {
                f.write_str("its registration or directory identity could not be confirmed")
            }
            #[cfg(unix)]
            Self::PackageChanged => {
                f.write_str("its disposable package coordination could not be confirmed")
            }
            #[cfg(unix)]
            Self::CoordinationUnavailable => {
                f.write_str("its cache coordination lock is unavailable")
            }
        }
    }
}

/// One stopped-check observation, without a freed-byte count or removal authority.
/// Its subject can be a logical name with no directory during metadata-only recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RetainedCheck {
    pub path: PathBuf,
    pub reason: KeepReason,
}

impl RetainedCheck {
    pub(crate) fn words(&self) -> String {
        format!(
            "cleanup check for {} stopped — {} (nothing removed by this check)",
            self.path.display(),
            self.reason
        )
    }
}

pub(crate) fn add(kept: &mut Vec<RetainedCheck>, path: &Path, reason: KeepReason) {
    if !kept
        .iter()
        .any(|one| one.path == path && one.reason == reason)
    {
        kept.push(RetainedCheck {
            path: path.to_path_buf(),
            reason,
        });
    }
}
