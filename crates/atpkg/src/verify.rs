// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg verify [program]` (§12): an offline drift/integrity audit of the installed store
//! against the signed `tree_root` recorded at install/update time.
//!
//! It recomputes the active build dir's root ([`crate::tree::tree_root_declared`]; the plain
//! [`crate::tree::tree_root`] wherever the inode stores modes) and compares it to the
//! release-key-verified value in `status.toml`
//! ([`crate::status::ProgramStatus::tree_root`]). That root came from a manifest whose
//! signature was checked over exact bytes before parse, so this attests that what is on disk
//! still matches what was signed — never a self-generated checksum. Read-only.
//!
//! A vendor-direct build is attested by its `.vendor` record instead, and a vendor program
//! rolled back to the legacy index build its first vendor install replaced (whose row that
//! install overwrote) by the signed root the program's stamp kept for that build. Neither
//! asks an index.
//!
//! `status.toml` shares the store's trust boundary (both under the 0700 prefix), so `verify`
//! defends against accidental corruption and non-privileged drift, not against an adversary
//! who already owns the prefix.

use crate::store::Layout;

/// The result of verifying one program's active build against its recorded signed root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The recomputed store tree matches the recorded signed `tree_root`.
    Match {
        /// The active build audited.
        build: u64,
    },
    /// The store tree does not match the recorded signed root (drift, tampering, corruption).
    Drift {
        /// The active build audited.
        build: u64,
        /// The signed root recorded at install/update.
        expected: String,
        /// The root just recomputed over the on-disk tree.
        got: String,
    },
    /// No signed `tree_root` was recorded (an older install, or a loose manifest). Cannot
    /// attest: not a pass (fail-closed).
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
    /// A sysroot bundle carrying a `toolchain` symlink from the retired install-time
    /// wiring, added after the signed root was captured over the payload, so the tree
    /// cannot be tree-attested. Informational (exit 0), not a failure: the integrity gate
    /// is the tree_root re-verify at install (`crate::install`), before any wiring.
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

/// Verify one program's active build against the signed `tree_root` recorded in `status.toml`.
/// Fail-closed, in order: not installed ⇒ [`VerifyOutcome::NotInstalled`]; no recorded root ⇒
/// [`VerifyOutcome::NoSignedRoot`] (not a pass); recorded root for a different build ⇒
/// [`VerifyOutcome::BuildMismatch`]; then recompute and compare (case-insensitive, as the
/// install-time re-verify does). A vendor program's legacy build the row does not attest is
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
    // No signed record means no assurance: fail closed.
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
    // A rustup-linked sysroot bundle carries a `toolchain` link from the retired
    // install-time wiring, added after the signed root was captured, which
    // `tree::tree_root` cannot walk: report it as such, not as drift or Unreadable.
    // `is_reparse`, not `is_symlink`: on Windows the link is a directory junction
    // (`is_symlink()` reports false), which the walk would otherwise descend into.
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
///
/// The mode slot the walk folds where the filesystem stores no permission bits
/// (Windows) is the record the stage left beside the build. Where the inode stores them
/// (Unix) the record is an empty map the walk never consults, and no file is read. A
/// Windows build with no record cannot be attested — the walk would fold `0` for every
/// file and call a healthy tree drifted — so its error names the re-stage that writes
/// one, and the caller reports it as unreadable, never as a false Drift.
///
/// One walk for both lanes: the plain [`crate::tree::tree_root`] folds mode `0` on Windows
/// and would call every vendor build there drifted.
fn walked_root(build_dir: &std::path::Path) -> std::io::Result<String> {
    let modes = crate::store::declared_modes(build_dir)?;
    crate::tree::tree_root_declared(build_dir, &modes)
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
    match walked_root(&build_dir) {
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

/// Verify every active program (those live on PATH). An uninstalled status-only leftover is
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
    use crate::activate::{activate_build, install_shims};
    use std::path::PathBuf;

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-verify-{label}-"))
    }

    /// The modes a stage would have declared for the one-file build [`install`] lays —
    /// what the walk folds on a filesystem without permission bits, and what the record
    /// beside the build says on Windows. Unix never consults it.
    fn modes_of(program: &str) -> crate::tree::DeclaredModes {
        let mut modes = crate::tree::DeclaredModes::new();
        let mut rel = b"bin/".to_vec();
        rel.extend_from_slice(exe_name(program).as_bytes());
        modes.insert(rel, 0o755);
        modes
    }

    /// `program`'s executable under `bin/` (`ay`, or `ay.exe` on Windows): the file the
    /// shim forwards to, without which `active_builds` reports no active build.
    fn exe_name(program: &str) -> String {
        crate::store::ToolName::new(program).unwrap().exe_file()
    }

    /// The root a producer would have signed for the build [`install`] lays: the
    /// declared-mode walk, which on Unix is the plain walk over the stored bits.
    fn signed_root(dir: &std::path::Path, program: &str) -> String {
        crate::tree::tree_root_declared(dir, &modes_of(program)).unwrap()
    }

    /// Lay down a complete, activated build with `bin/<program>`; return its dir.
    fn install(layout: &Layout, program: &str, build: u64) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin").join(exe_name(program)), b"#!/bin/true\n").unwrap();
        // The record the real stage leaves beside a build (a no-op on Unix).
        crate::store::write_declared_modes(&dir, &modes_of(program)).unwrap();
        install_shims(
            layout,
            &dir,
            &[program.to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        activate_build(layout, &dir).unwrap();
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
        let root = signed_root(&dir, "ay");
        record(&l, "ay", Some(18), &root);
        assert_eq!(verify_program(&l, "ay"), VerifyOutcome::Match { build: 18 });
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn verify_detects_drift() {
        let l = layout("drift");
        let dir = install(&l, "ay", 18);
        let root = signed_root(&dir, "ay");
        record(&l, "ay", Some(18), &root);
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
        // The root the stamp keeps is the index row's signed root: the declared-mode fold
        // ([`signed_root`]).
        let root = signed_root(&dir, "claude");
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
        std::fs::write(dir.join("bin").join(exe_name("claude")), b"tampered").unwrap();
        assert!(matches!(
            verify_program(&l, "claude"),
            VerifyOutcome::Drift { build, .. } if build == legacy
        ));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A vendor-direct build is attested by the same declared-mode walk as an index build,
    /// against the root its `.vendor` record holds; a mutated tree drifts, naming the root
    /// it walked.
    #[test]
    fn a_vendor_build_is_attested_by_the_walk_its_stage_folded() {
        let l = layout("vendor-walk");
        let version = crate::vendor_direct::Version::parse("2.1.283").unwrap();
        let build = version.build_id();
        let dir = install(&l, "claude", build);
        let rec = crate::vendor_direct::VendorRecord {
            schema: crate::vendor_direct::RECORD_SCHEMA,
            program: "claude".into(),
            version,
            vendor: "Anthropic".into(),
            source_url:
                "https://downloads.claude.ai/claude-code-releases/2.1.283/win32-x64/claude.exe"
                    .into(),
            sha256: "9dbe16dafed59da5cdabbfe11ad0335738c753fad794989b47f9446accd6de3a".into(),
            sha512: String::new(),
            size: 244_960_928,
            // What the stage records: the fold over the modes it declared.
            tree_root: signed_root(&dir, "claude"),
            apple_team: cfg!(target_os = "macos").then(|| {
                crate::vendor_direct::spec("claude")
                    .unwrap()
                    .apple_team
                    .to_string()
            }),
            anchor: crate::vendor_direct::Anchor::AnthropicOpenPgp,
            build_date: crate::vendor_direct::BuildDate::parse("2026-09-25T01:39:37Z"),
            verified_at: 1_790_526_112,
        };
        crate::vendor_direct::durable::write_durable(
            &crate::vendor_direct::record_path(&dir).unwrap(),
            &rec.record_bytes().unwrap(),
        )
        .unwrap();
        assert_eq!(
            verify_program(&l, "claude"),
            VerifyOutcome::VendorRoot {
                build,
                vendor: "Anthropic".into(),
                drift: None
            }
        );
        std::fs::write(dir.join("bin").join(exe_name("claude")), b"tampered").unwrap();
        assert!(
            matches!(
                verify_program(&l, "claude"),
                VerifyOutcome::VendorRoot { drift: Some(_), .. }
            ),
            "a mutated vendor tree drifts"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn verify_build_mismatch() {
        let l = layout("mismatch");
        let dir = install(&l, "ay", 18);
        let root = signed_root(&dir, "ay");
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

    /// The declared-mode record is what the walk folds where the inode stores no bits: with
    /// it a Windows build attests to the root a Unix producer signed; without it the verdict
    /// is Unreadable (naming the re-stage), never a false Drift. On Unix the record is never
    /// consulted, so removing it changes nothing.
    #[test]
    fn a_missing_declared_mode_record_is_unreadable_only_where_the_inode_has_no_bits() {
        let l = layout("modes-record");
        let dir = install(&l, "ay", 18);
        record(&l, "ay", Some(18), &signed_root(&dir, "ay"));
        assert_eq!(verify_program(&l, "ay"), VerifyOutcome::Match { build: 18 });
        crate::store::clear_declared_modes(&dir);
        let after = verify_program(&l, "ay");
        if crate::platform::HAS_POSIX_MODES {
            assert_eq!(after, VerifyOutcome::Match { build: 18 });
        } else {
            match after {
                VerifyOutcome::Unreadable { build: 18, error } => {
                    assert!(
                        error.contains("declared-mode record") && error.contains("re-stages"),
                        "the verdict names the record and the way out: {error}"
                    );
                }
                other => panic!("a build with no mode record must be unreadable, got {other:?}"),
            }
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn verify_all_covers_active_programs() {
        let l = layout("all");
        let ay = install(&l, "ay", 18);
        let ny = install(&l, "ny", 9);
        record(&l, "ay", Some(18), &signed_root(&ay, "ay"));
        record(&l, "ny", Some(9), &signed_root(&ny, "ny"));
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
