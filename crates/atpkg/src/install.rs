// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The verify-and-stage chain (§8/§9): turning a downloaded bundle into a verified,
//! ready-to-activate store build.
//!
//! This is the integrity heart of an install, independent of *how* the bytes were
//! fetched (the network fetch reuses `aterm-update-core`'s `download_to`; the manifest
//! was already root/release-signature-verified upstream, §8). Given the signed
//! [`Artifact`] and a downloaded archive on disk, [`verify_and_stage`] enforces, in order:
//!
//! 1. **Download integrity** — the COMPRESSED asset's SHA-256 equals the signed
//!    `artifact.sha256`. A corrupted/substituted download is refused before extraction.
//! 2. **Tar-slip-safe extraction** — into a fresh staging build dir via
//!    [`crate::extract::extract_tar_zst`] (every entry vetted; size-capped from the signed
//!    `disk_installed`).
//! 3. **Apply-time re-verify (TOCTOU)** — the extracted tree's [`crate::tree::tree_root`]
//!    equals the signed `artifact.tree_root` (when the producer set one). An already-
//!    extracted tree can't be re-checked against the compressed `sha256`, so this closes
//!    the extract→activate window: a file swapped post-extraction moves the root.
//!
//! Any failure removes the partial staging dir and returns fail-closed — a half- or
//! wrongly-staged build never reaches activation.

use std::path::Path;

use crate::extract::{ExtractError, extract_tar_zst};
use crate::manifest::Artifact;
use crate::tree::{file_sha256, tree_root};

/// Why staging a downloaded bundle failed. Each aborts the stage fail-closed.
#[derive(Debug)]
pub enum StageError {
    /// I/O while hashing / preparing the staging dir.
    Io(std::io::Error),
    /// The compressed asset's SHA-256 did not match the signed `artifact.sha256`.
    Sha256Mismatch { expected: String, got: String },
    /// Extraction failed (tar-slip escape, size cap, or tar/zstd error).
    Extract(ExtractError),
    /// The extracted tree's `tree_root` did not match the signed `artifact.tree_root`.
    TreeRootMismatch { expected: String, got: String },
}

// Hand-rendered through `Formatter::write_str` + direct `Display::fmt` calls (no
// `write!`): the `write!`/`format_args!` expansion embeds `fmt::Arguments`
// construction (with inlined `unsafe`) that the strict Trust gate cannot lower and
// fails closed on. Byte-identical output (`write!` with `{}` args performs exactly
// these formatter writes in sequence; no width/fill flags are used).
impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StageError::Io(e) => {
                f.write_str("io: ")?;
                std::fmt::Display::fmt(e, f)
            }
            StageError::Sha256Mismatch { expected, got } => {
                f.write_str("asset sha256 mismatch: expected ")?;
                f.write_str(expected)?;
                f.write_str(", got ")?;
                f.write_str(got)
            }
            StageError::Extract(e) => {
                f.write_str("extract: ")?;
                std::fmt::Display::fmt(e, f)
            }
            StageError::TreeRootMismatch { expected, got } => {
                f.write_str("tree_root mismatch: expected ")?;
                f.write_str(expected)?;
                f.write_str(", got ")?;
                f.write_str(got)
            }
        }
    }
}

impl std::error::Error for StageError {}

/// Uncompressed-size cap for extraction: twice the signed `disk_installed` (tolerating
/// block-rounding) but at least 1 MiB, so a decompression bomb is bounded by the *signed*
/// size, never an attacker-chosen tar header. A `disk_installed` of 0 (older/loose
/// manifest) falls back to a 2 GiB ceiling rather than unbounded.
fn size_cap(artifact: &Artifact) -> u64 {
    let signed = artifact.cost.disk_installed;
    if signed == 0 {
        2u64 << 30
    } else {
        signed.saturating_mul(2).max(1 << 20)
    }
}

/// The maximum entry count — a tar-bomb (millions of tiny entries) guard well above any
/// real toolchain bundle.
const MAX_ENTRIES: u64 = 4_000_000;

/// Verify a downloaded `archive` against the signed `artifact` and stage it at
/// `build_dir` (see the module docs). On success `build_dir` holds the verified,
/// ready-to-activate tree. On any failure the partial `build_dir` is removed and the
/// error returned fail-closed.
pub fn verify_and_stage(
    artifact: &Artifact,
    archive: &Path,
    build_dir: &Path,
) -> Result<(), StageError> {
    // 1. Download integrity — the compressed asset's sha256 must match the signed value,
    //    BEFORE we spend any work extracting it.
    let got = file_sha256(archive).map_err(StageError::Io)?;
    if !got.eq_ignore_ascii_case(&artifact.sha256) {
        return Err(StageError::Sha256Mismatch {
            expected: artifact.sha256.clone(),
            got,
        });
    }

    // 2. Fresh staging dir, then tar-slip-safe extraction (size-capped from the signed size).
    let _ = std::fs::remove_dir_all(build_dir);
    std::fs::create_dir_all(build_dir).map_err(StageError::Io)?;
    if let Err(e) = extract_tar_zst(archive, build_dir, size_cap(artifact), MAX_ENTRIES) {
        let _ = std::fs::remove_dir_all(build_dir);
        return Err(StageError::Extract(e));
    }

    // 3. Apply-time re-verify (TOCTOU): the extracted tree must match the signed tree_root
    //    (when the producer emitted one). A mismatch — tamper or partial extract — aborts.
    if !artifact.tree_root.is_empty() {
        let got = match tree_root(build_dir) {
            Ok(r) => r,
            Err(e) => {
                let _ = std::fs::remove_dir_all(build_dir);
                return Err(StageError::Io(e));
            }
        };
        if !got.eq_ignore_ascii_case(&artifact.tree_root) {
            let _ = std::fs::remove_dir_all(build_dir);
            return Err(StageError::TreeRootMismatch {
                expected: artifact.tree_root.clone(),
                got,
            });
        }
    }

    // 4. Mark the build COMPLETE — the last step, written atomically AFTER the
    //    tree_root re-verify (so the marker itself is never part of the hashed
    //    tree). Its presence is what distinguishes a fully-installed build from one
    //    left partial by a crash mid-extract; `list_installed` skips marker-less
    //    build dirs so such a partial is re-installed, not treated as up-to-date.
    if let Err(e) = crate::store::mark_build_ready(build_dir) {
        let _ = std::fs::remove_dir_all(build_dir);
        return Err(StageError::Io(e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Artifact, Cost};
    use crate::tree::tree_root;
    use std::io::Write;
    use std::path::PathBuf;

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-install-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A minimal raw USTAR + zstd archive with one regular file `bin/ay`.
    fn make_archive(dir: &Path) -> PathBuf {
        let content = b"#!/bin/true\nthe ay binary";
        let mut h = [0u8; 512];
        let name = b"bin/ay";
        h[..name.len()].copy_from_slice(name);
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());

        let mut tar = Vec::new();
        tar.extend_from_slice(&h);
        tar.extend_from_slice(content);
        tar.resize(tar.len() + (512 - content.len() % 512) % 512, 0);
        tar.resize(tar.len() + 1024, 0);

        let path = dir.join("ay-18.tar.zst");
        let f = std::fs::File::create(&path).unwrap();
        let mut enc = zstd::Encoder::new(f, 0).unwrap();
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap();
        path
    }

    fn artifact(sha256: &str, tree_root: &str) -> Artifact {
        Artifact {
            target: "aarch64-apple-darwin".into(),
            kind: "binary".into(),
            asset: "ay-18.tar.zst".into(),
            sha256: sha256.into(),
            tree_root: tree_root.into(),
            size: 0,
            reloc: "self-contained".into(),
            cost: Cost {
                download_bytes: 0,
                disk_installed: 1 << 20,
                build_seconds: 0,
            },
        }
    }

    // Happy path: correct sha256 + correct tree_root ⇒ the tree is staged.
    #[test]
    fn verifies_and_stages_a_good_bundle() {
        let d = tmp("good");
        let archive = make_archive(&d);
        let real_sha = file_sha256(&archive).unwrap();
        // Stage once just to learn the extracted tree_root, then verify the real path.
        let probe = d.join("probe");
        verify_and_stage(&artifact(&real_sha, ""), &archive, &probe).unwrap();
        let real_root = tree_root(&probe).unwrap();

        let build = d.join("store/ay/18");
        verify_and_stage(&artifact(&real_sha, &real_root), &archive, &build).unwrap();
        assert_eq!(
            std::fs::read(build.join("bin/ay")).unwrap(),
            b"#!/bin/true\nthe ay binary"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    // A wrong (compressed) sha256 is refused before extraction; nothing is staged.
    #[test]
    fn rejects_sha256_mismatch() {
        let d = tmp("badsha");
        let archive = make_archive(&d);
        let build = d.join("store/ay/18");
        let err = verify_and_stage(&artifact("deadbeef", ""), &archive, &build).unwrap_err();
        assert!(
            matches!(err, StageError::Sha256Mismatch { .. }),
            "got {err:?}"
        );
        assert!(
            !build.exists(),
            "nothing should be staged on a sha mismatch"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    // A wrong tree_root (TOCTOU / tamper) aborts and removes the partial stage.
    #[test]
    fn rejects_tree_root_mismatch_and_cleans_up() {
        let d = tmp("badroot");
        let archive = make_archive(&d);
        let real_sha = file_sha256(&archive).unwrap();
        let build = d.join("store/ay/18");
        let err =
            verify_and_stage(&artifact(&real_sha, &"a".repeat(64)), &archive, &build).unwrap_err();
        assert!(
            matches!(err, StageError::TreeRootMismatch { .. }),
            "got {err:?}"
        );
        assert!(
            !build.exists(),
            "a tree_root mismatch must remove the partial stage"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
