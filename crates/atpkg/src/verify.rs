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
    let active = crate::ops::active_builds(layout).get(program).copied();
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
mod tests {
    use super::*;
    use crate::activate::{activate_build, install_shims};
    use std::path::PathBuf;

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-verify-{label}-"))
    }

    /// Declared modes for the one-file build [`install`] lays down.
    fn modes_of(program: &str) -> crate::tree::DeclaredModes {
        let mut modes = crate::tree::DeclaredModes::new();
        let mut rel = b"bin/".to_vec();
        rel.extend_from_slice(exe_name(program).as_bytes());
        modes.insert(rel, 0o755);
        modes
    }

    /// `program`'s executable filename under `bin/`.
    fn exe_name(program: &str) -> String {
        crate::store::ToolName::new(program).unwrap().exe_file()
    }

    /// The declared-mode root for the build [`install`] lays down.
    fn signed_root(dir: &std::path::Path, program: &str) -> String {
        crate::tree::tree_root_declared(dir, &modes_of(program)).unwrap()
    }

    /// Lay down a complete, activated build with `bin/<program>`; return its dir.
    fn install(layout: &Layout, program: &str, build: u64) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin").join(exe_name(program)), b"#!/bin/true\n").unwrap();
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
    fn verify_no_signed_root_is_not_a_pass() {
        let l = layout("nosigned");
        install(&l, "ay", 18);
        record(&l, "ay", Some(18), "");
        let o = verify_program(&l, "ay");
        assert_eq!(o, VerifyOutcome::NoSignedRoot { build: Some(18) });
        assert!(
            !matches!(o, VerifyOutcome::Match { .. }),
            "empty root is fail-closed, not a pass"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A vendor program rolled back to its legacy index build is attested by the stamp's kept root.
    #[test]
    fn a_vendor_programs_legacy_build_is_attested_by_its_kept_root() {
        let l = layout("legacy-kept");
        let legacy = 2_026_091_901;
        let dir = install(&l, "claude", legacy);
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
        crate::vendor_direct::ProgramStamp::record_legacy_root(
            &l,
            "claude",
            legacy,
            &"c".repeat(64),
        )
        .unwrap();
        record(&l, "claude", Some(legacy), &root);
        assert_eq!(
            verify_program(&l, "claude"),
            VerifyOutcome::Match { build: legacy },
            "a row that attests its own build outranks the root the stamp kept"
        );
        std::fs::write(dir.join("bin").join(exe_name("claude")), b"tampered").unwrap();
        assert!(matches!(
            verify_program(&l, "claude"),
            VerifyOutcome::Drift { build, .. } if build == legacy
        ));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A vendor-direct build is attested by the declared-mode walk against its `.vendor` record.
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
            measured_sha256: None,
            agreed_with: None,
            tree_from: None,
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
                staged_from: StagedFrom::Documents,
                drift: None
            }
        );
        let grok_version = crate::vendor_direct::Version::parse("1.0.70").unwrap();
        let grok_build = grok_version.build_id();
        let grok = install(&l, "grok", grok_build);
        let measured = crate::vendor_direct::VendorRecord {
            program: "grok".into(),
            version: grok_version,
            vendor: "xAI".into(),
            source_url: "https://storage.googleapis.com/grok-build-public-artifacts/cli/\
                         grok-1.0.70-macos-aarch64"
                .into(),
            tree_root: signed_root(&grok, "grok"),
            apple_team: cfg!(target_os = "macos").then(|| {
                crate::vendor_direct::spec("grok")
                    .unwrap()
                    .apple_team
                    .to_string()
            }),
            anchor: crate::vendor_direct::Anchor::XaiDeveloperId,
            ..rec.clone()
        };
        crate::vendor_direct::durable::write_durable(
            &crate::vendor_direct::record_path(&grok).unwrap(),
            &measured.record_bytes().unwrap(),
        )
        .unwrap();
        assert_eq!(
            verify_program(&l, "grok"),
            VerifyOutcome::VendorRoot {
                build: grok_build,
                vendor: "xAI".into(),
                staged_from: StagedFrom::DeveloperIdMeasured,
                drift: None
            },
            "an older grok's digest is the lane's measure, no xAI document's"
        );
        let agreed = crate::vendor_direct::VendorRecord {
            source_url: "https://registry.npmjs.org/@xai-official/grok-darwin-arm64/-/\
                         grok-darwin-arm64-1.0.70.tgz"
                .into(),
            sha256: String::new(),
            sha512: "ab".repeat(64),
            anchor: crate::vendor_direct::Anchor::XaiNpmRegistry,
            measured_sha256: Some("cd".repeat(32)),
            agreed_with: Some(
                "https://storage.googleapis.com/grok-build-public-artifacts/cli/\
                 grok-1.0.70-macos-aarch64.zst"
                    .into(),
            ),
            ..measured.clone()
        };
        crate::vendor_direct::durable::write_durable(
            &crate::vendor_direct::record_path(&grok).unwrap(),
            &agreed.record_bytes().unwrap(),
        )
        .unwrap();
        assert_eq!(
            verify_program(&l, "grok"),
            VerifyOutcome::VendorRoot {
                build: grok_build,
                vendor: "xAI".into(),
                staged_from: StagedFrom::RegistryAgreed {
                    sha256: "cd".repeat(32)
                },
                drift: None
            },
            "grok's tarball is npm's registry-signed one, its binary matched by xAI's bucket"
        );
        std::fs::write(dir.join("bin").join(exe_name("claude")), b"tampered").unwrap();
        assert!(
            matches!(
                verify_program(&l, "claude"),
                VerifyOutcome::VendorRoot { drift: Some(_), .. }
            ),
            "a mutated vendor tree drifts"
        );
        #[cfg(unix)]
        {
            fifo(&dir.join("pipe"));
            assert!(
                matches!(
                    verify_program(&l, "claude"),
                    VerifyOutcome::Unreadable { build: b, .. } if b == build
                ),
                "a vendor tree the walk refuses is unreadable, never a match"
            );
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn verify_build_mismatch() {
        let l = layout("mismatch");
        let dir = install(&l, "ay", 18);
        let root = signed_root(&dir, "ay");
        record(&l, "ay", Some(17), &root);
        assert_eq!(
            verify_program(&l, "ay"),
            VerifyOutcome::BuildMismatch {
                active: 18,
                recorded: Some(17)
            }
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A FIFO at `path`: an entry no bundle holds, which the walk refuses.
    #[cfg(unix)]
    fn fifo(path: &std::path::Path) {
        use std::os::unix::ffi::OsStrExt as _;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c` is a live NUL-terminated path in this test's scratch layout.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }

    /// Checks wired `toolchain` symlinks, plain directories, and unreadable entries.
    #[cfg(unix)]
    #[test]
    fn the_walk_decides_wired_drifted_or_unreadable() {
        let l = layout("wired");
        let dir = install(&l, "ay", 18);
        record(&l, "ay", Some(18), &signed_root(&dir, "ay"));
        let link = dir.join("toolchain");
        std::os::unix::fs::symlink("../../no-such-toolchain", &link).unwrap();
        assert_eq!(
            verify_program(&l, "ay"),
            VerifyOutcome::WiredSysroot { build: 18 }
        );
        std::fs::remove_file(&link).unwrap();
        std::fs::create_dir(&link).unwrap();
        std::fs::write(link.join("extra"), b"walked").unwrap();
        assert!(matches!(
            verify_program(&l, "ay"),
            VerifyOutcome::Drift { build: 18, .. }
        ));
        std::fs::remove_dir_all(&link).unwrap();
        fifo(&dir.join("pipe"));
        assert!(matches!(
            verify_program(&l, "ay"),
            VerifyOutcome::Unreadable { build: 18, .. }
        ));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Only a matching tree, a matching vendor build, or a wired sysroot passes.
    #[test]
    fn only_a_matching_tree_or_a_wired_sysroot_passes() {
        use VerifyOutcome as V;
        let root = || "a".repeat(64);
        let vendor = |drift| V::VendorRoot {
            build: 1,
            vendor: "Anthropic".into(),
            staged_from: StagedFrom::Documents,
            drift,
        };
        for (o, pass) in [
            (
                V::DevLinked {
                    checkout: PathBuf::from("checkout"),
                },
                false,
            ),
            (
                V::DevLinkUnavailable {
                    error: "unreadable".into(),
                },
                false,
            ),
            (V::Match { build: 1 }, true),
            (V::WiredSysroot { build: 1 }, true),
            (vendor(None), true),
            (vendor(Some(root())), false),
            (
                V::Drift {
                    build: 1,
                    expected: root(),
                    got: root(),
                },
                false,
            ),
            (V::NoSignedRoot { build: Some(1) }, false),
            (V::NotInstalled, false),
            (
                V::Unreadable {
                    build: 1,
                    error: String::new(),
                },
                false,
            ),
            (
                V::BuildMismatch {
                    active: 1,
                    recorded: None,
                },
                false,
            ),
        ] {
            assert_eq!(o.passes(), pass, "{o:?}");
        }
    }

    #[test]
    fn verify_not_installed() {
        let l = layout("notinstalled");
        assert_eq!(verify_program(&l, "ghost"), VerifyOutcome::NotInstalled);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_development_link_is_not_a_signed_tree_or_an_absent_program() {
        let l = layout("linked");
        let store = install(&l, "ay", 18);
        record(&l, "ay", Some(18), &signed_root(&store, "ay"));
        let checkout = l.prefix.join("checkout");
        let rel = PathBuf::from("bin").join(exe_name("ay"));
        let exe = checkout.join(&rel);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"development bytes, never executed").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        crate::linkmode::link(&l, "ay", &checkout, &[rel]).unwrap();
        let linked = verify_program(&l, "ay");
        assert_eq!(
            linked,
            VerifyOutcome::DevLinked {
                checkout: checkout.canonicalize().unwrap()
            }
        );
        assert!(!linked.passes());
        assert_eq!(verify_all(&l), vec![("ay".into(), linked)]);

        std::fs::remove_file(&exe).unwrap();
        assert!(matches!(
            verify_program(&l, "ay"),
            VerifyOutcome::DevLinkUnavailable { .. }
        ));
        std::fs::write(l.link_marker("ay"), "invalid = [").unwrap();
        assert!(matches!(
            verify_program(&l, "ay"),
            VerifyOutcome::DevLinkUnavailable { .. }
        ));

        install_shims(&l, &store, &["ay".into()], crate::Aliases::Off).unwrap();
        assert_eq!(
            verify_program(&l, "ay"),
            VerifyOutcome::Match { build: 18 },
            "the actual store shim still verifies despite the stale marker"
        );
        assert_eq!(verify_program(&l, "ghost"), VerifyOutcome::NotInstalled);
    }

    /// A missing declared-mode record is `Unreadable` only where the inode stores no mode bits.
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
        record(
            &l,
            "ay",
            Some(18),
            &signed_root(&ay, "ay").to_ascii_uppercase(),
        );
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
