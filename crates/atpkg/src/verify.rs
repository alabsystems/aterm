// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::store::Layout;

/// What a vendor-direct tree was staged from, as its `.vendor` record says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedFrom {
    /// The payload digest the vendor's own documents named.
    Documents,
    /// Bytes signed with the vendor's Developer ID, whose digest the lane measured, no vendor
    /// document naming one (grok before npm's registry vouched for it,
    /// [`crate::vendor_direct::Anchor`]).
    DeveloperIdMeasured,
    /// npm's registry-signed tarball of the vendor's build, its binary measured at stage and
    /// matched by the vendor's own copy.
    RegistryAgreed {
        /// The staged binary's sha256 (lowercase hex).
        sha256: String,
    },
    /// The payload digest the vendor's own documents named, its tree copied from another
    /// row's complete build of those same bytes instead of downloaded again.
    Sibling {
        /// The row whose build the tree was copied from.
        program: String,
    },
}

impl StagedFrom {
    /// What `record` says its tree was staged from.
    #[must_use]
    pub(crate) fn of(record: &crate::vendor_direct::VendorRecord) -> Self {
        match (&record.anchor, &record.measured_sha256, &record.tree_from) {
            (crate::vendor_direct::Anchor::XaiDeveloperId, _, _) => Self::DeveloperIdMeasured,
            (_, Some(sha256), _) => Self::RegistryAgreed {
                sha256: sha256.clone(),
            },
            (_, None, Some(program)) => Self::Sibling {
                program: program.clone(),
            },
            (_, None, None) => Self::Documents,
        }
    }
}

/// The result of verifying one program's active build against its recorded signed root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// An active development link, whose bytes have no signed store attestation.
    DevLinked {
        /// The checkout whose executable tools the shims run.
        checkout: std::path::PathBuf,
    },
    /// A recorded development link could not be read or no longer resolves to its tools.
    DevLinkUnavailable {
        /// The read or resolution failure.
        error: String,
    },
    /// The recomputed store tree matches the recorded signed `tree_root`.
    Match {
        /// The active build audited.
        build: u64,
    },
    /// The store tree does not match the recorded signed root.
    Drift {
        /// The active build audited.
        build: u64,
        /// The signed root recorded at install/update.
        expected: String,
        /// The root just recomputed over the on-disk tree.
        got: String,
    },
    /// No signed `tree_root` was recorded.
    NoSignedRoot {
        /// The active build, if any.
        build: Option<u64>,
    },
    /// The program has no selected store build or recorded development link.
    NotInstalled,
    /// The on-disk tree could not be read to recompute its root.
    Unreadable {
        /// The active build.
        build: u64,
        /// The IO error string.
        error: String,
    },
    /// A sysroot bundle carrying a `toolchain` symlink from install-time wiring.
    WiredSysroot {
        /// The active build.
        build: u64,
    },
    /// A vendor-direct build attested against its `.vendor` record.
    VendorRoot {
        /// The active build audited.
        build: u64,
        /// Who published it.
        vendor: String,
        /// What the tree was staged from.
        staged_from: StagedFrom,
        /// `None` when the tree matches; the recomputed root when it drifted.
        drift: Option<String>,
    },
    /// The active build differs from the build the recorded root is for.
    BuildMismatch {
        /// The build now active on PATH.
        active: u64,
        /// The build the recorded root was captured for.
        recorded: Option<u64>,
    },
}

impl VerifyOutcome {
    /// Whether `atpkg verify` counts this outcome a pass (exit 0).
    #[must_use]
    pub const fn passes(&self) -> bool {
        matches!(
            self,
            Self::Match { .. } | Self::WiredSysroot { .. } | Self::VendorRoot { drift: None, .. }
        )
    }
}

/// Verify one program's active build against the signed `tree_root` recorded in `status.toml`.
#[must_use]
pub fn verify_program(layout: &Layout, program: &str) -> VerifyOutcome {
    let active = crate::ops::active_build_of(layout, program);
    let store_selected = crate::ops::which(layout, program)
        .and_then(|path| crate::ops::program_build_of_target(&path))
        .is_some_and(|(name, build)| name == program && Some(build) == active);
    match crate::linkmode::live_linked_checkout(layout, program) {
        Ok(Some(checkout)) => return VerifyOutcome::DevLinked { checkout },
        Err(error) if !store_selected => {
            return VerifyOutcome::DevLinkUnavailable {
                error: error.to_string(),
            };
        }
        // A store shim that actually runs still verifies its store build even if a stale
        // link marker describes a different selection.
        Ok(None) | Err(_) => {}
    }
    let st = crate::status::read(layout);
    let ps = st.as_ref().and_then(|s| s.programs.get(program));
    let recorded_build = ps.and_then(|p| p.installed_build);
    let recorded_root = ps.map(|p| p.tree_root.clone()).unwrap_or_default();

    let Some(build) = active else {
        return VerifyOutcome::NotInstalled;
    };
    if crate::vendor_direct::is_vendor_build(build) {
        return verify_vendor_build(layout, program, build);
    }
    let (recorded_build, recorded_root) =
        match kept_legacy_root(layout, program, build, recorded_build, &recorded_root) {
            Some(root) => (Some(build), root),
            None => (recorded_build, recorded_root),
        };
    if recorded_root.is_empty() {
        return VerifyOutcome::NoSignedRoot { build: Some(build) };
    }
    if recorded_build != Some(build) {
        return VerifyOutcome::BuildMismatch {
            active: build,
            recorded: recorded_build,
        };
    }
    let build_dir = layout.build_dir(program, build);
    if std::fs::symlink_metadata(build_dir.join("toolchain"))
        .is_ok_and(|m| crate::platform::is_reparse(&m))
    {
        return VerifyOutcome::WiredSysroot { build };
    }
    match walked_root(&build_dir) {
        Ok(got) if got.eq_ignore_ascii_case(&recorded_root) => VerifyOutcome::Match { build },
        Ok(got) => VerifyOutcome::Drift {
            build,
            expected: recorded_root,
            got,
        },
        Err(e) => VerifyOutcome::Unreadable {
            build,
            error: e.to_string(),
        },
    }
}

/// The on-disk root of `build_dir`, folded the way its stage folded it.
fn walked_root(build_dir: &std::path::Path) -> std::io::Result<String> {
    let modes = crate::store::declared_modes(build_dir)?;
    crate::tree::tree_root_declared(build_dir, &modes)
}

/// The signed root a vendor program's stamp kept for its legacy index `build`, when the recorded
/// row does not attest that build itself.
fn kept_legacy_root(
    layout: &Layout,
    program: &str,
    build: u64,
    recorded_build: Option<u64>,
    recorded_root: &str,
) -> Option<String> {
    if recorded_build == Some(build) && !recorded_root.is_empty() {
        return None;
    }
    crate::vendor_direct::ProgramStamp::read(layout, program)?
        .legacy_root_of(build)
        .map(str::to_string)
}

/// Verify a vendor-direct build against its `.vendor` record's root.
fn verify_vendor_build(layout: &Layout, program: &str, build: u64) -> VerifyOutcome {
    let build_dir = layout.build_dir(program, build);
    let Some(record) = crate::vendor_direct::complete_record(&build_dir) else {
        return VerifyOutcome::NoSignedRoot { build: Some(build) };
    };
    match walked_root(&build_dir) {
        Ok(got) => VerifyOutcome::VendorRoot {
            build,
            staged_from: StagedFrom::of(&record),
            vendor: record.vendor,
            drift: (!got.eq_ignore_ascii_case(&record.tree_root)).then_some(got),
        },
        Err(e) => VerifyOutcome::Unreadable {
            build,
            error: e.to_string(),
        },
    }
}

/// Verify active store programs and report development links without signed attestation.
#[must_use]
pub fn verify_all(layout: &Layout) -> Vec<(String, VerifyOutcome)> {
    let mut programs: std::collections::BTreeSet<_> =
        crate::ops::active_builds(layout).into_keys().collect();
    let error = match crate::linkmode::linked_programs_checked(layout) {
        Ok(linked) => {
            programs.extend(linked);
            None
        }
        Err(error) => Some((
            "*links*".to_string(),
            VerifyOutcome::DevLinkUnavailable {
                error: error.to_string(),
            },
        )),
    };
    programs
        .into_iter()
        .map(|p| {
            let o = verify_program(layout, &p);
            (p, o)
        })
        .chain(error)
        .collect()
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
