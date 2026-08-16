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
//! 2. **Tar-slip-safe extraction** — into a scratch SIBLING of the build dir via
//!    [`crate::extract::extract_tar_zst`] (every entry vetted; size-capped from the signed
//!    `disk_installed`). The live tree is never extracted over.
//! 3. **Apply-time re-verify (TOCTOU)** — the extracted tree's [`crate::tree::tree_root`]
//!    equals the signed `artifact.tree_root` (when the producer set one). An already-
//!    extracted tree can't be re-checked against the compressed `sha256`, so this closes
//!    the extract→activate window: a file swapped post-extraction moves the root.
//! 4. **Atomic swap, with rollback** — only a tree that passed every check above is renamed
//!    into `build_dir`, and only then is the build marked complete.
//!
//! Any failure removes the scratch tree and returns fail-closed — a half- or wrongly-staged
//! build never reaches activation, and **a stage that cannot install the new build must not
//! have uninstalled the old one**.

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
/// ready-to-activate tree. On any failure the partial tree is removed and the error
/// returned fail-closed — and, crucially, **whatever was installed at `build_dir` before
/// the call is still installed and still marked complete**.
///
/// # Why this stages beside the build instead of into it
///
/// The previous shape was `remove_dir_all(build_dir)` → extract → verify → mark. Three
/// things follow from deleting first, and all three were real:
///
/// * **The live tree died at the first byte of extraction.** Re-installing an
///   already-present build (a repair, a re-run, a `decide()` that re-Installs) destroyed a
///   working toolchain before it had a verified replacement; any failure after that point
///   — bad archive, disk full, Ctrl-C — left the user with nothing.
/// * **The completeness marker survived the delete.** It is a SIBLING file
///   (`<build>.ready`, deliberately outside the hashed tree), so `remove_dir_all` did not
///   touch it: after an interrupted stage the store still answered "build N is installed"
///   for a tree that was gone or half-written, and `decide()` therefore never repaired it.
/// * **The debris was unreclaimable.** A marker-less build dir is invisible to
///   `list_installed`, and GC only reclaims what `list_installed` returns, so an
///   interrupted install leaked its partial tree until someone deleted it by hand.
///
/// Now the tree is built in a scratch sibling and only becomes `build_dir` once it has
/// passed every check, and the marker is cleared for the duration of the swap. Crash at
/// any point and the store is left in one of exactly two honest states: the OLD build,
/// complete; or NO build, unmarked and therefore re-installable.
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

    // 2. Extract into a scratch SIBLING (tar-slip-safe, size-capped from the signed size).
    //    The live tree is untouched throughout. Any scratch left by a killed earlier run is
    //    swept first — the store lock guarantees its owner is gone.
    crate::store::sweep_stage_scratch(build_dir);
    let incoming = crate::store::incoming_dir(build_dir).ok_or_else(|| {
        StageError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "build dir has no name",
        ))
    })?;
    std::fs::create_dir_all(&incoming).map_err(StageError::Io)?;
    if let Err(e) = extract_tar_zst(archive, &incoming, size_cap(artifact), MAX_ENTRIES) {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(StageError::Extract(e));
    }

    // 3. Apply-time re-verify (TOCTOU): the extracted tree must match the signed tree_root
    //    (when the producer emitted one). A mismatch — tamper or partial extract — aborts,
    //    and the previously-installed build is still there, untouched.
    if !artifact.tree_root.is_empty() {
        let got = match tree_root(&incoming) {
            Ok(r) => r,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&incoming);
                return Err(StageError::Io(e));
            }
        };
        if !got.eq_ignore_ascii_case(&artifact.tree_root) {
            let _ = std::fs::remove_dir_all(&incoming);
            return Err(StageError::TreeRootMismatch {
                expected: artifact.tree_root.clone(),
                got,
            });
        }
    }

    // 4. SWAP. Marker down first (mid-swap, the build is honestly not complete), then the
    //    old tree aside, then the verified tree into place, then the old tree reclaimed.
    if let Err(e) = swap_into_place(build_dir, &incoming) {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(StageError::Io(e));
    }

    // 5. Mark the build COMPLETE — the last step, written atomically AFTER the tree_root
    //    re-verify (so the marker itself is never part of the hashed tree) and after the
    //    swap. Its presence is what distinguishes a fully-installed build from one left
    //    partial by a crash; `list_installed` skips marker-less build dirs so such a
    //    partial is re-installed, not treated as up-to-date.
    if let Err(e) = crate::store::mark_build_ready(build_dir) {
        // LEAVE THE TREE. By this point the swap has succeeded and the outgoing tree has
        // already been reclaimed, so deleting the new one is the single enumerated path
        // that ends with NEITHER the old build nor the new — the exact state the module
        // invariant forbids, reached by a 3-byte write failing (realistically: the volume
        // is full, which is precisely the condition this module exists for), and reached
        // with a `current` link already naming this build when a live build was re-staged.
        //
        // The tree on disk passed every check above, so keeping it is not keeping a
        // partial: it is the verified toolchain, honestly UNMARKED. `list_installed` skips
        // marker-less dirs, so it reads as not-installed and the next run re-stages it; a
        // `current` link that named this build still resolves to correct bytes instead of
        // dangling. The old "take the tree with it so the next run stages cleanly rather
        // than extracting over a stranger" reasoning belonged to the delete-then-extract
        // flow — nothing extracts into `build_dir` any more, it is swapped onto, so an
        // unmarked leftover is never extracted over. It is also fully reclaimable: the next
        // stage renames it aside and deletes it, and `gc`'s partial arm sweeps it once
        // nothing claims it.
        return Err(StageError::Io(e));
    }
    if let Some(parent) = build_dir.parent() {
        crate::store::sync_dir(parent);
    }
    Ok(())
}

/// Move `incoming` onto `build_dir`, retiring whatever was there.
///
/// The ordering is the whole point, so it is spelled out rather than inlined:
///
/// 1. `clear_build_ready` — from here until step 5 of the caller the build is not
///    complete, and every reader agrees with that.
/// 2. `build_dir` → `<build>.superseded-<pid>` (skipped when nothing is installed).
/// 3. `incoming` → `build_dir`. If THIS fails, the old tree is moved back: a stage that
///    cannot install the new build must not have uninstalled the old one.
/// 4. the superseded tree is reclaimed.
///
/// Both renames restore the marker on failure when the outgoing tree was complete before
/// the call. That is not cosmetic bookkeeping: the marker is the ONLY thing that makes a
/// tree visible to `list_installed`, so leaving a perfectly good toolchain unmarked
/// downgrades "the update did not happen" into "re-download and re-extract gigabytes".
fn swap_into_place(build_dir: &Path, incoming: &Path) -> std::io::Result<()> {
    // Whether the tree we are replacing was itself complete decides what a rollback may
    // claim about it: re-marking a tree that was NEVER complete would promote a crash
    // leftover to "installed" on the way out of an unrelated failure.
    let was_complete = crate::store::build_is_complete(build_dir);
    crate::store::clear_build_ready(build_dir)?;
    if let Some(parent) = build_dir.parent() {
        crate::store::sync_dir(parent);
    }
    let superseded = crate::store::superseded_dir(build_dir);
    let retired = match superseded {
        Some(ref old) if build_dir.exists() => {
            let _ = std::fs::remove_dir_all(old);
            if let Err(e) = std::fs::rename(build_dir, old) {
                // The swap could not even BEGIN. A rename that returns an error has not
                // moved anything, so the old tree is provably still at `build_dir`,
                // exactly as complete as it was one line ago; the marker we just took down
                // is the only thing that changed, and re-marking asserts nothing new.
                if was_complete {
                    let _ = crate::store::mark_build_ready(build_dir);
                }
                return Err(e);
            }
            true
        }
        _ => false,
    };
    if let Err(e) = std::fs::rename(incoming, build_dir) {
        // Put the old build back before reporting the failure — a rollback here is the
        // difference between "the update did not happen" and "the toolchain is gone".
        if retired && let Some(ref old) = superseded {
            restore_outgoing(old, build_dir, was_complete);
        }
        return Err(e);
    }
    if retired && let Some(ref old) = superseded {
        let _ = std::fs::remove_dir_all(old);
    }
    Ok(())
}

/// Move the parked outgoing tree back to `build_dir` and, only if it was complete BEFORE the
/// swap, re-mark it. `true` when the tree really came back.
///
/// The `was_complete` guard and the restore guard are two different questions and both have
/// to be asked. [`crate::store::mark_build_ready`] has no existence precondition — it writes
/// a temp file beside the build and renames it onto `<build>.ready` without ever looking at
/// `<build>` — so re-marking after a restore that FAILED writes a completeness marker for a
/// tree that is not there. Nothing believes such a marker today (every reader of
/// `build_is_complete` enumerates directories first), and nothing reclaims it either, so it
/// would sit there as a durable lie one refactor away from being believed.
fn restore_outgoing(old: &Path, build_dir: &Path, was_complete: bool) -> bool {
    if std::fs::rename(old, build_dir).is_err() {
        return false;
    }
    if was_complete {
        let _ = crate::store::mark_build_ready(build_dir);
    }
    true
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

    /// One raw USTAR header + padded body for a regular file.
    fn tar_entry(name: &str, content: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        let nb = name.as_bytes();
        h[..nb.len()].copy_from_slice(nb);
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

        let mut out = h.to_vec();
        out.extend_from_slice(content);
        out.resize(out.len() + (512 - content.len() % 512) % 512, 0);
        out
    }

    /// zstd-compress a raw tar stream (plus its two zero end-of-archive blocks) to `path`.
    fn seal(path: PathBuf, mut tar: Vec<u8>) -> PathBuf {
        tar.resize(tar.len() + 1024, 0);
        let f = std::fs::File::create(&path).unwrap();
        let mut enc = zstd::Encoder::new(f, 0).unwrap();
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap();
        path
    }

    /// A minimal raw USTAR + zstd archive with one regular file `bin/ay`.
    fn make_archive(dir: &Path) -> PathBuf {
        seal(
            dir.join("ay-18.tar.zst"),
            tar_entry("bin/ay", b"#!/bin/true\nthe ay binary"),
        )
    }

    /// An archive whose EXTRACTION fails part-way: a good first entry (so real bytes land in
    /// the staging tree and the extractor is genuinely mid-flight), then a `../escape` entry
    /// that [`crate::extract::vet_entry`] refuses as a tar-slip.
    ///
    /// Its own sha256 is whatever it is — the caller signs THAT value, so the download-
    /// integrity gate passes and the failure lands where this test needs it: in step 2.
    fn make_slip_archive(dir: &Path) -> PathBuf {
        let mut tar = tar_entry("bin/ay", b"a plausible first entry");
        tar.extend(tar_entry("../escape", b"pwned"));
        seal(dir.join("slip-18.tar.zst"), tar)
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

    /// The three facts every staging test needs: a scratch root, a good archive, and the
    /// artifact whose sha256 + tree_root HONESTLY describe that archive.
    ///
    /// The tree_root is learned by extracting once through the same extractor the real stage
    /// uses, not by asking `verify_and_stage` for it — a fixture that got its expected value
    /// out of the function under test can be satisfied by a function that checks nothing.
    struct Bundle {
        dir: PathBuf,
        archive: PathBuf,
        art: Artifact,
    }

    fn bundle(label: &str) -> Bundle {
        let dir = tmp(label);
        let archive = make_archive(&dir);
        let sha = file_sha256(&archive).unwrap();
        let probe = dir.join("probe");
        extract_tar_zst(&archive, &probe, 1 << 20, 1000).unwrap();
        let root = tree_root(&probe).unwrap();
        std::fs::remove_dir_all(&probe).unwrap();
        assert_eq!(root.len(), 64, "the fixture must carry a real tree_root");
        Bundle {
            dir,
            archive,
            art: artifact(&sha, &root),
        }
    }

    impl Bundle {
        /// `store/ay/18` under this bundle's scratch root.
        fn build(&self) -> PathBuf {
            self.dir.join("store/ay/18")
        }

        /// Install build 18 for real, through the real stage, and leave a witness file inside
        /// the installed tree. Returns `(build_dir, witness)`.
        ///
        /// The witness is how every "the old build survived" assertion below distinguishes
        /// *this* tree from a replacement that merely happens to contain the same files.
        fn installed(&self) -> (PathBuf, PathBuf) {
            let build = self.build();
            verify_and_stage(&self.art, &self.archive, &build).unwrap();
            assert!(
                crate::store::build_is_complete(&build),
                "the fixture is only interesting once the build is really installed"
            );
            let witness = build.join("this-tree-survived");
            std::fs::write(&witness, b"old").unwrap();
            (build, witness)
        }
    }

    /// Every stage-scratch sibling currently sitting beside `build_dir`.
    fn scratch_beside(build_dir: &Path) -> Vec<String> {
        let Some(parent) = build_dir.parent() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(parent) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| n.contains(".incoming-") || n.contains(".superseded-"))
            .collect();
        out.sort();
        out
    }

    // Happy path: correct sha256 + correct tree_root ⇒ the tree is staged.
    #[test]
    fn verifies_and_stages_a_good_bundle() {
        let b = bundle("good");
        let build = b.build();
        verify_and_stage(&b.art, &b.archive, &build).unwrap();
        assert_eq!(
            std::fs::read(build.join("bin/ay")).unwrap(),
            b"#!/bin/true\nthe ay binary"
        );
        assert!(crate::store::build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    // A wrong (compressed) sha256 is refused before extraction; nothing is staged.
    #[test]
    fn rejects_sha256_mismatch() {
        let b = bundle("badsha");
        let build = b.build();
        let err = verify_and_stage(&artifact("deadbeef", ""), &b.archive, &build).unwrap_err();
        assert!(
            matches!(err, StageError::Sha256Mismatch { .. }),
            "got {err:?}"
        );
        assert!(
            !build.exists(),
            "nothing should be staged on a sha mismatch"
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    // A wrong tree_root (TOCTOU / tamper) aborts and removes the partial stage.
    #[test]
    fn rejects_tree_root_mismatch_and_cleans_up() {
        let b = bundle("badroot");
        let build = b.build();
        let bad = artifact(&b.art.sha256, &"a".repeat(64));
        let err = verify_and_stage(&bad, &b.archive, &build).unwrap_err();
        assert!(
            matches!(err, StageError::TreeRootMismatch { .. }),
            "got {err:?}"
        );
        assert!(
            !build.exists(),
            "a tree_root mismatch must remove the partial stage"
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// THE GOVERNING INVARIANT, at the step that used to destroy the toolchain first: a
    /// stage whose EXTRACTION fails must not have uninstalled the old build.
    ///
    /// The old shape was `remove_dir_all(build_dir)` → extract, so the live tree was already
    /// gone by the time the extractor read its first entry. A bad archive, a full disk or a
    /// ^C anywhere after that left the user with no toolchain at all — while the SIBLING
    /// `<build>.ready` marker, which the delete never touched, still claimed the build was
    /// installed. Here the archive's first entry extracts fine and its second is a tar-slip:
    /// real bytes are written, then the stage aborts, and the installed build is untouched.
    #[test]
    fn a_failed_extraction_leaves_the_installed_build_intact_and_complete() {
        let b = bundle("extract-fail");
        let (build, witness) = b.installed();

        // A DIFFERENT archive, correctly signed for its own bytes, so the sha256 gate passes
        // and the failure lands in extraction rather than before it.
        let slip = make_slip_archive(&b.dir);
        let slip_art = artifact(&file_sha256(&slip).unwrap(), "");

        let err = verify_and_stage(&slip_art, &slip, &build).unwrap_err();
        assert!(
            matches!(err, StageError::Extract(_)),
            "the fixture must fail IN extraction, else this proves nothing: got {err:?}"
        );
        assert!(
            witness.exists() && std::fs::read(&witness).unwrap() == b"old",
            "the previously-installed tree was destroyed by a stage that then failed"
        );
        assert_eq!(
            std::fs::read(build.join("bin/ay")).unwrap(),
            b"#!/bin/true\nthe ay binary",
            "the old tree's contents must be the OLD ones, not the aborted extract's"
        );
        assert!(
            crate::store::build_is_complete(&build),
            "the surviving build must still be marked complete"
        );
        assert!(
            !build.parent().unwrap().join("escape").exists(),
            "the slip entry must not have escaped either"
        );
        assert!(scratch_beside(&build).is_empty(), "and no scratch leaked");
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// The same invariant one step later: a tree_root mismatch — tamper, or a truncated
    /// extract that still parsed — aborts AFTER the whole tree is on disk, the latest point
    /// at which the old shape had already destroyed the live one.
    #[test]
    fn a_failed_re_stage_leaves_the_installed_build_untouched() {
        let b = bundle("restage-fail");
        let (build, witness) = b.installed();

        let bad = artifact(&b.art.sha256, &"a".repeat(64));
        let err = verify_and_stage(&bad, &b.archive, &build).unwrap_err();
        assert!(
            matches!(err, StageError::TreeRootMismatch { .. }),
            "got {err:?}"
        );
        assert!(
            witness.exists(),
            "the previously-installed tree was destroyed by a stage that then failed"
        );
        assert_eq!(std::fs::read(&witness).unwrap(), b"old");
        assert!(
            crate::store::build_is_complete(&build),
            "the surviving build must still be marked complete"
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// A successful re-stage REPLACES the tree (the witness from the old tree is gone) and
    /// the build is complete again — the swap is not a no-op that leaves stale bytes.
    #[test]
    fn a_successful_re_stage_replaces_the_tree_and_re_marks_it() {
        let b = bundle("restage-ok");
        let (build, witness) = b.installed();

        verify_and_stage(&b.art, &b.archive, &build).unwrap();
        assert!(
            !witness.exists(),
            "the swap must install the NEW tree, not merge into the old one"
        );
        assert_eq!(
            std::fs::read(build.join("bin/ay")).unwrap(),
            b"#!/bin/true\nthe ay binary"
        );
        assert!(crate::store::build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// No stage — successful or failed — may leave scratch behind. Scratch is invisible to
    /// `list_installed` (non-numeric name) and so, historically, to every reclaim path.
    #[test]
    fn no_stage_leaves_scratch_siblings_behind() {
        let b = bundle("scratch");
        let build = b.build();

        verify_and_stage(&b.art, &b.archive, &build).unwrap();
        assert!(
            scratch_beside(&build).is_empty(),
            "after success: {:?}",
            scratch_beside(&build)
        );
        let bad = artifact(&b.art.sha256, &"b".repeat(64));
        let _ = verify_and_stage(&bad, &b.archive, &build).unwrap_err();
        assert!(
            scratch_beside(&build).is_empty(),
            "after failure: {:?}",
            scratch_beside(&build)
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// Scratch left by a KILLED earlier run is swept by the next stage of that build —
    /// the process that owned it cannot come back (every mutating verb holds the store
    /// lock), so its debris is ours to reclaim.
    #[test]
    fn a_new_stage_sweeps_scratch_left_by_a_killed_run() {
        let b = bundle("sweep");
        let build = b.build();
        std::fs::create_dir_all(build.parent().unwrap()).unwrap();
        // A half-extracted tree from a run that never finished.
        let orphan = build.with_file_name("18.incoming-999999");
        std::fs::create_dir_all(orphan.join("bin")).unwrap();
        std::fs::write(orphan.join("bin/half"), b"partial").unwrap();
        assert!(orphan.exists(), "the fixture starts with debris on disk");

        verify_and_stage(&b.art, &b.archive, &build).unwrap();
        assert!(!orphan.exists(), "the orphaned scratch was not swept");
        assert!(crate::store::build_is_complete(&build));
        assert!(scratch_beside(&build).is_empty());
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// A SWAP THAT CANNOT EVEN BEGIN MUST PUT THE MARKER BACK.
    ///
    /// `swap_into_place` takes the completeness marker down first, because for the duration
    /// of the swap the build honestly is not complete. If the very first rename then fails,
    /// the old tree has not moved an inch — but without restoring the marker it now reads as
    /// "not installed" to `list_installed`, `decide` and every reclaim path, so a working
    /// toolchain gets re-downloaded and re-extracted for nothing.
    ///
    /// The failure is induced the only way that is portable and needs no hook: a regular FILE
    /// occupies the `<build>.superseded-<pid>` path, and renaming a directory onto a
    /// non-directory fails.
    ///
    /// Driven through `swap_into_place` rather than `verify_and_stage`, because the stage's
    /// own `sweep_stage_scratch` now RECLAIMS a stray non-directory at a scratch path (it
    /// used to leak there forever, reclaimable by neither sweeper) and so clears this
    /// blocker before the swap ever sees it. That is the better behaviour and it is asserted
    /// separately by `a_stray_file_at_a_scratch_path_is_reclaimed_not_leaked`; the guarantee
    /// under test here is what `swap_into_place` does when its first rename fails, whatever
    /// the cause.
    #[test]
    fn a_swap_that_cannot_begin_restores_the_marker_it_took_down() {
        let b = bundle("swap-blocked");
        let (build, witness) = b.installed();

        let blocker = crate::store::superseded_dir(&build).unwrap();
        std::fs::write(&blocker, b"not a directory").unwrap();
        let incoming = crate::store::incoming_dir(&build).unwrap();
        std::fs::create_dir_all(incoming.join("bin")).unwrap();

        let err = swap_into_place(&build, &incoming).unwrap_err();
        assert!(
            blocker.is_file(),
            "the fixture only exercises the failed-retire path while the blocker is there"
        );
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::NotADirectory,
            "the fixture must fail at the FIRST rename, not somewhere else"
        );
        assert!(
            witness.exists(),
            "the old tree never moved and must still be here"
        );
        assert!(
            crate::store::build_is_complete(&build),
            "a swap that could not begin must leave the build as complete as it found it"
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// THE ROLLBACK. If the SECOND rename fails — the one that puts the verified tree where
    /// the old one used to be — the old tree is already parked in `<build>.superseded-<pid>`
    /// and `build_dir` does not exist. Leaving it there is exactly the state this whole
    /// module exists to forbid: no old build, no new build.
    ///
    /// Driven through the real `swap_into_place` with the `incoming` tree absent, which is
    /// the one way to make ONLY the second rename fail in-process: both renames happen in the
    /// same directory, so anything that blocks the second (permissions, an occupied
    /// destination) blocks the first as well.
    #[test]
    fn a_failed_final_rename_rolls_the_old_build_back_and_re_marks_it() {
        let b = bundle("rollback-complete");
        let (build, witness) = b.installed();

        let incoming = crate::store::incoming_dir(&build).unwrap();
        assert!(
            !incoming.exists(),
            "the fixture depends on the incoming tree being absent"
        );

        let err = swap_into_place(&build, &incoming).unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "the fixture must fail at the rename, not somewhere else"
        );
        assert!(
            witness.exists() && std::fs::read(&witness).unwrap() == b"old",
            "the old build must be back at its own path, not stranded in .superseded-"
        );
        assert!(
            crate::store::build_is_complete(&build),
            "it was complete before the swap, so it must be complete after the rollback"
        );
        assert!(
            scratch_beside(&build).is_empty(),
            "the rollback must not leave the superseded tree behind: {:?}",
            scratch_beside(&build)
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// …and the other half of that decision. A rollback restores the TREE unconditionally,
    /// but re-marks it ready only if it was ready to begin with. Re-marking a tree that was
    /// never complete would promote a crash leftover to "installed" on the way out of an
    /// unrelated failure — the manager would then run a half-extracted toolchain and report
    /// it up to date.
    #[test]
    fn a_rollback_never_promotes_a_tree_that_was_never_complete() {
        let b = bundle("rollback-incomplete");
        let (build, witness) = b.installed();
        // Demote it to what a crash between extract and mark leaves: a populated tree with
        // no marker.
        crate::store::clear_build_ready(&build).unwrap();
        assert!(
            !crate::store::build_is_complete(&build),
            "the fixture is only interesting while the tree is marker-less"
        );

        let incoming = crate::store::incoming_dir(&build).unwrap();
        assert!(!incoming.exists());
        let err = swap_into_place(&build, &incoming).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);

        assert!(witness.exists(), "the tree still comes back");
        assert!(
            !crate::store::build_is_complete(&build),
            "a rollback must never mark a build that was not complete before it"
        );
        assert!(scratch_beside(&build).is_empty());
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// The marker is written LAST, and it is a SIBLING — so it is never part of the tree the
    /// signed `tree_root` covers. Two consequences, both asserted: the freshly-staged tree
    /// still hashes to the signed value with the marker in place, and the marker file itself
    /// lives outside `build_dir`.
    #[test]
    fn the_marker_is_written_last_and_is_never_part_of_the_hashed_tree() {
        let b = bundle("marker-last");
        let build = b.build();
        verify_and_stage(&b.art, &b.archive, &build).unwrap();

        assert!(crate::store::build_is_complete(&build));
        assert_eq!(
            tree_root(&build).unwrap(),
            b.art.tree_root,
            "the marker must not have moved the tree_root — it is not inside the tree"
        );
        let marker = build.with_file_name("18.ready");
        assert!(marker.is_file(), "the marker is the SIBLING <build>.ready");
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// A PARTIAL IS NEVER TRUSTED AS INSTALLED. A fresh install killed (here: failed) mid
    /// extraction must leave nothing that `list_installed` will report, because everything
    /// downstream — `decide`, `atpkg list`, GC's reclaim — takes that list as the truth about
    /// what is on disk. The successful stage in the same layout is what makes the empty
    /// result meaningful rather than an artefact of a mis-built prefix.
    #[test]
    fn a_partial_tree_from_a_failed_fresh_install_is_never_listed_as_installed() {
        let b = bundle("partial-not-installed");
        let layout = crate::store::Layout {
            prefix: b.dir.join("prefix"),
        };
        let build = layout.build_dir("ay", 18);

        let slip = make_slip_archive(&b.dir);
        let slip_art = artifact(&file_sha256(&slip).unwrap(), "");
        let err = verify_and_stage(&slip_art, &slip, &build).unwrap_err();
        assert!(matches!(err, StageError::Extract(_)), "got {err:?}");

        assert!(
            crate::ops::list_installed(&layout).is_empty(),
            "a failed fresh install must leave nothing that reads as installed: {:?}",
            crate::ops::list_installed(&layout)
        );
        assert!(!crate::store::build_is_complete(&build));
        assert!(scratch_beside(&build).is_empty(), "and no scratch either");

        // Non-vacuity: the very same layout DOES report a build once one really installs.
        verify_and_stage(&b.art, &b.archive, &build).unwrap();
        assert_eq!(
            crate::ops::list_installed(&layout),
            vec![("ay".to_string(), 18u64)]
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// Block `mark_build_ready`'s temp write by planting a NON-EMPTY DIRECTORY at the exact
    /// `<parent>/.ready.tmp-<pid>` path it writes to: `fs::write` then fails EISDIR.
    ///
    /// This is the one failure the marker step can actually suffer that nothing earlier in
    /// the chain trips over — `clear_build_ready` only touches `<n>.ready`, and
    /// `sweep_stage_scratch` only matches `<n>.incoming-*` / `<n>.superseded-*` — so the
    /// swap completes and the failure lands exactly where the test needs it: step 5.
    fn block_the_marker_write(build_dir: &Path) -> PathBuf {
        let parent = build_dir.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        let blocker = parent.join(format!(".ready.tmp-{}", std::process::id()));
        std::fs::create_dir_all(&blocker).unwrap();
        std::fs::write(blocker.join("occupied"), b"x").unwrap();
        blocker
    }

    // THE INVARIANT AT ITS LAST STEP. When the marker write fails the swap has ALREADY
    // succeeded and the old tree has already been reclaimed, so removing the new tree is
    // the one enumerated path that ends with NEITHER build. It must not: the tree on disk
    // passed every check, so it is left in place, honestly unmarked (`list_installed`
    // skips it, the next run re-stages it) — and a `current` link that named this build
    // still resolves to a verified toolchain instead of dangling.
    #[test]
    fn a_marker_write_that_fails_after_the_swap_keeps_the_verified_tree() {
        let b = bundle("marker-fail-keeps-tree");
        let (build, _witness) = b.installed();
        let blocker = block_the_marker_write(&build);
        assert!(
            crate::store::build_is_complete(&build),
            "PRECONDITION: build 18 is installed and complete before the re-stage"
        );

        let err = verify_and_stage(&b.art, &b.archive, &build).unwrap_err();
        assert!(
            matches!(err, StageError::Io(_)),
            "the marker write is what failed: {err:?}"
        );

        // The governing invariant: this stage could not FINISH installing the new build, so
        // it must not have left the store with nothing.
        assert!(
            build.is_dir(),
            "a failed marker write destroyed a fully verified, correctly swapped-in tree"
        );
        assert_eq!(
            std::fs::read(build.join("bin/ay")).unwrap(),
            b"#!/bin/true\nthe ay binary",
            "and the tree left behind is the VERIFIED one, not a partial"
        );
        // Honest: unmarked, so it reads as not-installed and the next run re-stages it.
        assert!(
            !crate::store::build_is_complete(&build),
            "a build that could not be marked must never read as complete"
        );

        // Non-vacuity: clear the blocker and the very same call marks it ready.
        std::fs::remove_dir_all(&blocker).unwrap();
        verify_and_stage(&b.art, &b.archive, &build).unwrap();
        assert!(crate::store::build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    // THE TWO-RENAME CRASH WINDOW. A SIGKILL between `rename(build, superseded)` and
    // `rename(incoming, build)` leaves the ONLY copy of the old tree at
    // `<build>.superseded-<pid>` with nothing at `<build>`. Routine housekeeping used to
    // `remove_dir_all` it — deleting the user's toolchain as scratch. It must be MOVED
    // BACK instead: a crash is not a reason to lose the only tree there is.
    #[test]
    fn a_crash_between_the_two_renames_recovers_the_old_tree_instead_of_deleting_it() {
        let b = bundle("crash-window-recover");
        let (build, witness) = b.installed();
        // Reproduce the killed-mid-swap state exactly: marker down, tree parked at the
        // superseded name, nothing at the build path.
        let superseded = crate::store::superseded_dir(&build).unwrap();
        crate::store::clear_build_ready(&build).unwrap();
        std::fs::rename(&build, &superseded).unwrap();
        assert!(
            !build.exists() && superseded.is_dir(),
            "PRECONDITION: mid-swap"
        );

        crate::store::sweep_stage_scratch(&build);

        assert!(
            build.is_dir(),
            "the only copy of the old tree was deleted as scratch — nothing is left"
        );
        assert!(
            build.join(witness.file_name().unwrap()).exists(),
            "and it is THAT tree, not a lookalike"
        );
        // Deliberately NOT re-marked: the swap cleared the marker before the rename, so
        // whether the tree was complete is unrecoverable from disk. Unmarked means the next
        // run re-stages it — honest, and it never promotes a partial to "installed".
        assert!(
            !crate::store::build_is_complete(&build),
            "recovery must never claim a completeness it cannot prove"
        );
        assert!(!superseded.exists(), "and the scratch name is released");
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    // The recovery above is NARROW on purpose. When `<build>` is present the superseded
    // sibling is genuine leftover scratch (the swap got past its second rename), and the
    // sweep must still delete it — otherwise a killed run leaks a whole tree forever.
    #[test]
    fn a_superseded_sibling_beside_a_live_build_is_still_swept() {
        let b = bundle("crash-window-narrow");
        let (build, _) = b.installed();
        let superseded = crate::store::superseded_dir(&build).unwrap();
        std::fs::create_dir_all(superseded.join("bin")).unwrap();
        std::fs::write(superseded.join("bin/ay"), b"leftover").unwrap();
        assert!(
            build.is_dir() && superseded.is_dir(),
            "PRECONDITION: both the live tree and the leftover exist"
        );

        crate::store::sweep_stage_scratch(&build);

        assert!(!superseded.exists(), "genuine scratch is still reclaimed");
        assert!(build.is_dir(), "and the live build is untouched");
        assert!(
            crate::store::build_is_complete(&build),
            "a sweep beside a live build must not disturb its marker"
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    // The store sweep's recogniser must honour the SAME promise `gc::is_stage_scratch`
    // writes down: an unguarded `remove_dir_all` in a directory the user can also put
    // things in only fires on the producer's exact shape. `<build>.incoming-drafts` is not
    // ours to delete — and GC already refuses it, so the two halves must agree.
    #[test]
    fn a_lookalike_sibling_survives_the_store_sweep_exactly_as_it_survives_gc() {
        let b = bundle("sweep-shape");
        let (build, _) = b.installed();
        let parent = build.parent().unwrap();
        let user_dir = parent.join("18.incoming-drafts");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join("notes.md"), b"mine").unwrap();
        // Non-vacuity: a REAL scratch name beside it, to prove the sweep ran at all.
        let real = crate::store::incoming_dir(&build).unwrap();
        std::fs::create_dir_all(&real).unwrap();

        crate::store::sweep_stage_scratch(&build);

        assert!(!real.exists(), "PRECONDITION: the sweep really ran");
        assert!(
            user_dir.join("notes.md").exists(),
            "'looks like something we made' is not a good enough test for remove_dir_all"
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    // A NON-DIRECTORY at a scratch path was reclaimed by neither sweeper: the store sweep
    // called `remove_dir_all` (which fails on a file) and GC skipped it on its `is_dir`
    // filter. It leaks forever, and while it sits there it makes every stage of that build
    // by a process with that pid fail at the swap. The store sweep must take it.
    #[test]
    fn a_stray_file_at_a_scratch_path_is_reclaimed_not_leaked() {
        let b = bundle("sweep-stray-file");
        let (build, _) = b.installed();
        let blocker = crate::store::superseded_dir(&build).unwrap();
        std::fs::write(&blocker, b"not a directory").unwrap();
        assert!(
            blocker.is_file(),
            "PRECONDITION: a regular file is in the way"
        );

        crate::store::sweep_stage_scratch(&build);
        assert!(
            !blocker.exists(),
            "a stray file at a scratch path leaks forever if the sweep only removes dirs"
        );

        // And with it gone the stage that it was blocking now succeeds end to end.
        verify_and_stage(&b.art, &b.archive, &build).unwrap();
        assert!(crate::store::build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    // A rollback may only re-mark a tree it actually put back. `mark_build_ready` has no
    // existence precondition — it writes a temp beside the build and renames it onto
    // `<n>.ready` without ever looking at `<n>` — so a rollback whose restoring rename ALSO
    // failed would leave a completeness marker asserting a tree that is not there.
    //
    // Driven through `restore_outgoing` directly rather than through `verify_and_stage`,
    // and honestly so: the compound failure needs BOTH renames in one directory to fail,
    // and on a real filesystem anything that blocks the second (permissions, an occupied
    // destination) blocks the first as well. `restore_outgoing` is the production function,
    // not a restatement of it, and a missing source is a real rename failure.
    #[test]
    fn a_rollback_that_cannot_restore_the_tree_writes_no_marker() {
        let b = bundle("rollback-restore-fails");
        let build = b.build();
        std::fs::create_dir_all(build.parent().unwrap()).unwrap();
        let vanished = crate::store::superseded_dir(&build).unwrap();
        assert!(
            !vanished.exists(),
            "PRECONDITION: the tree to restore is not there, so the rename must fail"
        );

        assert!(
            !restore_outgoing(&vanished, &build, true),
            "a failed restore must report itself as failed"
        );
        assert!(
            !crate::store::build_is_complete(&build),
            "a rollback that could not restore the tree must not claim it is complete"
        );

        // Non-vacuity: with a real tree to move back, the SAME call restores AND re-marks.
        std::fs::create_dir_all(vanished.join("bin")).unwrap();
        assert!(restore_outgoing(&vanished, &build, true));
        assert!(build.join("bin").is_dir(), "the tree really came back");
        assert!(crate::store::build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    // And the other half of the same guard: a restore that SUCCEEDS for a tree that was not
    // complete before the swap must still not mark it. Re-marking there would promote a
    // crash leftover to "installed" on the way out of an unrelated failure.
    #[test]
    fn a_successful_restore_never_marks_a_tree_that_was_not_complete() {
        let b = bundle("restore-guard-incomplete");
        let build = b.build();
        std::fs::create_dir_all(build.parent().unwrap()).unwrap();
        let old = crate::store::superseded_dir(&build).unwrap();
        std::fs::create_dir_all(old.join("bin")).unwrap();

        assert!(restore_outgoing(&old, &build, false));
        assert!(
            build.join("bin").is_dir(),
            "PRECONDITION: the tree came back"
        );
        assert!(
            !crate::store::build_is_complete(&build),
            "a rollback must never mark a build that was not complete before it"
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }
}
