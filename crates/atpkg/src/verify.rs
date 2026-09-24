// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg verify [program]` (§12) — an offline drift/integrity audit of the installed store
//! against the SIGNED `tree_root` recorded at install/update time.
//!
//! It recomputes [`crate::tree::tree_root`] over the ACTIVE build dir and compares it to the
//! release-key-verified value persisted in `status.toml` ([`crate::status::ProgramStatus::tree_root`]).
//! That recorded root came from a manifest whose signature was checked over exact bytes
//! before parse (verify-before-parse), so this attests that what is on disk still matches
//! what was signed — it is NEVER a self-generated `files.sha256` (aterm-pkg's dropped
//! mistake). Read-only: no parse path, no filesystem mutation.
//!
//! A vendor-direct build is attested by its `.vendor` record instead, and a vendor program
//! rolled back to the legacy index build its first vendor install replaced — whose row
//! that install overwrote — by the signed root the program's stamp kept for that build.
//! Neither asks an index.
//!
//! `status.toml` shares the store's trust boundary (both under the 0700 hardened prefix), so
//! `verify` defends against accidental corruption / non-privileged drift, NOT against an
//! adversary who already owns the prefix (they could rewrite both). A future
//! `atpkg verify --online` could re-fetch + re-verify the manifest to remove the status.toml
//! dependency.

use crate::store::Layout;

/// The result of verifying one program's active build against its recorded signed root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The recomputed store tree matches the recorded signed `tree_root`.
    Match {
        /// The active build audited.
        build: u64,
    },
    /// The store tree does NOT match the recorded signed root (drift / tampering / corruption).
    Drift {
        /// The active build audited.
        build: u64,
        /// The signed root recorded at install/update.
        expected: String,
        /// The root just recomputed over the on-disk tree.
        got: String,
    },
    /// No signed `tree_root` was recorded (installed before verify support / a loose
    /// manifest). Cannot attest — this is NOT a pass (fail-closed).
    NoSignedRoot {
        /// The active build, if any.
        build: Option<u64>,
    },
    /// The program has no active build (not installed / not on PATH).
    NotInstalled,
    /// The on-disk tree could not be read to recompute its root.
    Unreadable {
        /// The active build.
        build: u64,
        /// The IO error string.
        error: String,
    },
    /// A relocated sysroot bundle: the sanctioned install-time toolchain wiring
    /// ([`crate::sysroot::relocate_sysroot`]) adds a `toolchain` SYMLINK inside the build tree
    /// AFTER the signed root was captured over the pristine payload, so the on-disk tree
    /// intentionally differs from the recorded root and cannot be tree-attested. Informational
    /// (exit 0), NOT a failure. Full tree-attestation is available with the default
    /// `self-contained` bundle (which has no post-install mutation). This is an audit
    /// convenience, not a security gate — the real integrity gate is the tree_root re-verify
    /// AT INSTALL (`crate::install`), before any wiring.
    WiredSysroot {
        /// The active build.
        build: u64,
    },
    /// A vendor-direct build, attested against the root folded when it was staged from its
    /// vendor's authenticated digest (its `.vendor` record) — no index signed this root.
    VendorRoot {
        /// The active build audited.
        build: u64,
        /// Who published it (`Anthropic`).
        vendor: String,
        /// `None` when the tree matches; the recomputed root when it drifted.
        drift: Option<String>,
    },
    /// The active build differs from the build the recorded root is for (e.g. post-rollback),
    /// so the recorded root cannot attest the live tree.
    BuildMismatch {
        /// The build now active on PATH.
        active: u64,
        /// The build the recorded root was captured for.
        recorded: Option<u64>,
    },
}

/// Verify one program's ACTIVE build against the signed `tree_root` recorded in `status.toml`.
/// Fail-closed, IN ORDER: not-installed ⇒ [`VerifyOutcome::NotInstalled`]; no recorded root ⇒
/// [`VerifyOutcome::NoSignedRoot`] (NOT a pass); recorded root is for a different build ⇒
/// [`VerifyOutcome::BuildMismatch`]; then recompute + compare (case-insensitive, mirroring
/// the install-time re-verify). A vendor program's legacy build the row does not attest is
/// judged against the root its stamp kept for it ([`kept_legacy_root`]) before those two.
#[must_use]
pub fn verify_program(layout: &Layout, program: &str) -> VerifyOutcome {
    let active = crate::ops::active_builds(layout).get(program).copied();
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
    // A vendor program's retained legacy build, once the recorded row no longer carries
    // its root: the signed root the program's stamp kept for exactly that build.
    let (recorded_build, recorded_root) =
        match kept_legacy_root(layout, program, build, recorded_build, &recorded_root) {
            Some(root) => (Some(build), root),
            None => (recorded_build, recorded_root),
        };
    // No signed record means no assurance: fail closed. A source-build provenance
    // sidecar used to be consulted here first, reporting a lower-assurance
    // SourceBuilt outcome; the source-build lane is gone, so an unsigned build is
    // simply unverified.
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
    // A rustup-linked sysroot bundle carries a sanctioned `toolchain` SYMLINK from the
    // install-time toolchain wiring (added AFTER the signed root was captured), which
    // `tree::tree_root` cannot walk. This is not drift or tampering — report it as such
    // rather than a false Unreadable failure. (Self-contained bundles have no such symlink
    // and get the full strict attestation below.)
    // `is_reparse`, not `is_symlink`: on Windows the wired `toolchain` link is a directory
    // JUNCTION (`is_symlink()` reports false), which the walk would otherwise descend into.
    if std::fs::symlink_metadata(build_dir.join("toolchain"))
        .is_ok_and(|m| crate::platform::is_reparse(&m))
    {
        return VerifyOutcome::WiredSysroot { build };
    }
    match crate::tree::tree_root(&build_dir) {
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

/// The signed root a vendor program's stamp kept for its legacy index `build`, when the
/// recorded row does not attest that build itself; `None` for every other program.
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

/// A vendor-direct build against its `.vendor` record's root; no record beside a complete
/// build means no assurance ([`VerifyOutcome::NoSignedRoot`]).
fn verify_vendor_build(layout: &Layout, program: &str, build: u64) -> VerifyOutcome {
    let build_dir = layout.build_dir(program, build);
    let Some(record) = crate::vendor_direct::complete_record(&build_dir) else {
        return VerifyOutcome::NoSignedRoot { build: Some(build) };
    };
    match crate::tree::tree_root(&build_dir) {
        Ok(got) => VerifyOutcome::VendorRoot {
            build,
            vendor: record.vendor,
            drift: (!got.eq_ignore_ascii_case(&record.tree_root)).then_some(got),
        },
        Err(e) => VerifyOutcome::Unreadable {
            build,
            error: e.to_string(),
        },
    }
}

/// Verify EVERY active program (those live on PATH). An uninstalled status-only leftover is
/// not audited, so `verify_all` never emits false [`VerifyOutcome::NotInstalled`] noise.
#[must_use]
pub fn verify_all(layout: &Layout) -> Vec<(String, VerifyOutcome)> {
    crate::ops::active_builds(layout)
        .into_keys()
        .map(|p| {
            let o = verify_program(layout, &p);
            (p, o)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activate::{activate_channel, install_shims};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-verify-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    /// Lay down a COMPLETE, activated build with `bin/<program>`; return its dir.
    fn install(layout: &Layout, program: &str, build: u64) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin").join(program), b"#!/bin/true\n").unwrap();
        install_shims(
            layout,
            &dir,
            &[program.to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        activate_channel(layout, "stable", &dir).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        dir
    }

    fn record(layout: &Layout, program: &str, build: Option<u64>, root: &str) {
        let mut programs = crate::status::read(layout)
            .map(|s| s.programs)
            .unwrap_or_default();
        programs.insert(
            program.to_string(),
            crate::status::ProgramStatus {
                installed_build: build,
                state: "active".into(),
                tree_root: root.into(),
            },
        );
        let s = crate::status::Status {
            schema: 1,
            programs,
            ..Default::default()
        };
        crate::status::write(layout, &s).unwrap();
    }

    #[test]
    fn verify_matches_recorded_signed_root() {
        let l = layout("match");
        let dir = install(&l, "ay", 18);
        let root = crate::tree::tree_root(&dir).unwrap();
        record(&l, "ay", Some(18), &root);
        assert_eq!(verify_program(&l, "ay"), VerifyOutcome::Match { build: 18 });
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn verify_detects_drift() {
        let l = layout("drift");
        let dir = install(&l, "ay", 18);
        let root = crate::tree::tree_root(&dir).unwrap();
        record(&l, "ay", Some(18), &root);
        // Mutate the on-disk tree AFTER recording the signed root.
        std::fs::write(dir.join("bin/ay"), b"tampered").unwrap();
        assert!(
            matches!(
                verify_program(&l, "ay"),
                VerifyOutcome::Drift { build: 18, .. }
            ),
            "a mutated store tree drifts from the signed root"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn verify_no_signed_root_is_not_a_pass() {
        let l = layout("nosigned");
        install(&l, "ay", 18);
        record(&l, "ay", Some(18), ""); // empty recorded root
        let o = verify_program(&l, "ay");
        assert_eq!(o, VerifyOutcome::NoSignedRoot { build: Some(18) });
        assert!(
            !matches!(o, VerifyOutcome::Match { .. }),
            "empty root is fail-closed, not a pass"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A vendor program rolled back to the legacy index build its first vendor install
    /// replaced: the row names another build (or that build with no root), and the root its
    /// stamp kept for exactly that build attests it — a match, and drift when the tree moves.
    /// A root kept for another legacy build attests nothing.
    #[test]
    fn a_vendor_programs_legacy_build_is_attested_by_its_kept_root() {
        let l = layout("legacy-kept");
        let legacy = 2_026_091_901;
        let dir = install(&l, "claude", legacy);
        let root = crate::tree::tree_root(&dir).unwrap();
        let vendor = crate::vendor_direct::Version::parse("2.1.280")
            .unwrap()
            .build_id();
        record(&l, "claude", Some(vendor), &"b".repeat(64));
        assert_eq!(
            verify_program(&l, "claude"),
            VerifyOutcome::BuildMismatch {
                active: legacy,
                recorded: Some(vendor)
            },
            "nothing kept: the row cannot attest the legacy build"
        );
        crate::vendor_direct::ProgramStamp::record_legacy_root(&l, "claude", legacy - 1, &root)
            .unwrap();
        assert!(
            matches!(
                verify_program(&l, "claude"),
                VerifyOutcome::BuildMismatch { .. }
            ),
            "a root kept for another build attests nothing"
        );
        crate::vendor_direct::ProgramStamp::record_legacy_root(&l, "claude", legacy, &root)
            .unwrap();
        assert_eq!(
            verify_program(&l, "claude"),
            VerifyOutcome::Match { build: legacy }
        );
        record(&l, "claude", Some(legacy), "");
        assert_eq!(
            verify_program(&l, "claude"),
            VerifyOutcome::Match { build: legacy },
            "a rollback row recorded with no root"
        );
        std::fs::write(dir.join("bin/claude"), b"tampered").unwrap();
        assert!(matches!(
            verify_program(&l, "claude"),
            VerifyOutcome::Drift { build, .. } if build == legacy
        ));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn verify_build_mismatch() {
        let l = layout("mismatch");
        let dir = install(&l, "ay", 18);
        let root = crate::tree::tree_root(&dir).unwrap();
        record(&l, "ay", Some(17), &root); // recorded for a different build
        assert_eq!(
            verify_program(&l, "ay"),
            VerifyOutcome::BuildMismatch {
                active: 18,
                recorded: Some(17)
            }
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn verify_not_installed() {
        let l = layout("notinstalled");
        assert_eq!(verify_program(&l, "ghost"), VerifyOutcome::NotInstalled);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn verify_all_covers_active_programs() {
        let l = layout("all");
        let ay = install(&l, "ay", 18);
        let ny = install(&l, "ny", 9);
        record(&l, "ay", Some(18), &crate::tree::tree_root(&ay).unwrap());
        record(&l, "ny", Some(9), &crate::tree::tree_root(&ny).unwrap());
        // Drift ny.
        std::fs::write(ny.join("bin/ny"), b"tampered").unwrap();
        let outcomes: std::collections::BTreeMap<_, _> = verify_all(&l).into_iter().collect();
        assert_eq!(
            outcomes.get("ay"),
            Some(&VerifyOutcome::Match { build: 18 })
        );
        assert!(matches!(
            outcomes.get("ny"),
            Some(VerifyOutcome::Drift { .. })
        ));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
