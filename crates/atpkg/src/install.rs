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
//! 2. **Slip-safe staging** — into a scratch SIBLING of the build dir, through the lane
//!    the signed row's `payload` names ([`stage_payload`]): the historical `.tar.zst`
//!    extraction for a release bundle, or one of the `https` protocol lanes (`tar-zst` /
//!    `tar-gz` / `zip` archives with `strip_components` and in-root symlinks, a
//!    `raw-binary` that becomes `bin/<entry>`, a `dmg` whose single `.app` is copied
//!    out of the mounted image), then the row's `links` as relative symlinks under
//!    `bin/`. Every archive entry is vetted before a byte is written and size-capped from
//!    the signed `disk_installed`. The live tree is never extracted over.
//! 3. **Apply-time re-verify (TOCTOU)** — the extracted tree's [`crate::tree::tree_root`]
//!    equals the signed `artifact.tree_root` (when the producer set one). An already-
//!    extracted tree can't be re-checked against the compressed `sha256`, so this closes
//!    the extract→activate window: a file swapped post-extraction moves the root.
//!    The root is folded BY THE EXTRACTOR as it writes
//!    ([`crate::extract::extract_tar_zst_rooted`]) instead of by re-reading the whole
//!    payload back off disk — see [`verify_and_stage`] step 3 for exactly which bytes
//!    that still proves and which window it gives up.
//! 4. **Atomic swap, with rollback** — only a tree that passed every check above is renamed
//!    into `build_dir`, and only then is the build marked complete.
//!
//! Any failure removes the scratch tree and returns fail-closed — a half- or wrongly-staged
//! build never reaches activation, and **a stage that cannot install the new build must not
//! have uninstalled the old one**.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::extract::{
    ExtractError, ExtractOptions, TreeAccumulator, extract_tar_gz_tree, extract_tar_zst_tree,
    extract_zip_tree,
};
use crate::manifest::Artifact;
use crate::tree::{file_sha256, tree_root};

/// Opt-in belt-and-suspenders: when this is set to a non-empty value, [`verify_and_stage`]
/// ALSO walks the staged tree with [`crate::tree::tree_root`] and refuses the stage unless
/// the walk agrees with the root the extractor folded.
///
/// It exists so the fused digest's equivalence is checkable on a real fleet machine over
/// real bundles — not only over the unit corpus in `extract.rs` — and so an operator
/// chasing a suspected filesystem fault can re-arm the historical
/// read-it-all-back-again pass without a rebuild. It is OFF by default because turning it
/// on restores exactly the cost this module stopped paying: a second full pass over the
/// uncompressed payload (3.44 GB for the shipped `trust` member).
const DISK_REVERIFY_ENV: &str = "ATPKG_STAGE_DISK_REVERIFY";

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
    /// An `https` payload could not be laid down as the row describes: an unknown
    /// `payload` lane, an inadmissible `entry`/`links` name or target, a `links` target
    /// missing from the staged tree, or the `dmg` tooling (`hdiutil`/`ditto`)
    /// refusing the image. Names the field or tool, so a mis-authored row fails fast on
    /// the authoring machine.
    Payload(String),
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
            StageError::Payload(m) => {
                f.write_str("payload: ")?;
                f.write_str(m)
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
    //    The stage hands back the `tree_root` of what it wrote, folded from the bytes
    //    as they went past (see [`crate::extract::extract_tar_zst_rooted`]). This is
    //    the ONE pass over the uncompressed payload: the digest step 3 compares is a
    //    by-product of the writing, not a second reading of it. (The `dmg` lane is
    //    the exception — `ditto` wrote its bytes, so it walks them once.)
    let extracted_root = match stage_payload(artifact, archive, &incoming) {
        Ok(root) => root,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&incoming);
            return Err(e);
        }
    };

    // 3. Apply-time re-verify (TOCTOU): the extracted tree must match the signed tree_root
    //    (when the producer emitted one). A mismatch — tamper or partial extract — aborts,
    //    and the previously-installed build is still there, untouched.
    //
    //    WHAT THIS STILL PROVES, EXACTLY. The digest now describes the bytes the extractor
    //    WROTE rather than the bytes a subsequent walk READ BACK. Both forms refuse:
    //      * a substituted or corrupt archive — the compressed `sha256` gate in step 1
    //        already ran, and this catches anything that survives it;
    //      * a truncated, partial or aborted extraction (short files move the root);
    //      * a bundle whose laid-down layout, modes or contents differ in any way from the
    //        one the publisher signed — that is the whole point, and it is unchanged.
    //    WHAT IT GIVES UP is one thing: a mutation landing in the window BETWEEN the write
    //    and the read, inside the `0700` staging scratch, while this process holds the
    //    store lock. That window was microseconds wide, it is not the threat the private
    //    prefix is hardened against (see the module docs), and the price of keeping it was
    //    re-reading the entire payload — 3.44 GB for the shipped `trust` member, issued
    //    straight after 3.44 GB of dirty writeback.
    //
    //    The byte format is a CROSS-VERSION contract (signed manifests embed roots computed
    //    by earlier releases), so the two producers share one formatter and one fold
    //    (`tree::entry_line` / `tree::root_of_entry_lines`) and an exhaustive parity test
    //    pins them together (`extract.rs`,
    //    `fused_tree_root_is_byte_identical_to_the_on_disk_walk`). `ATPKG_STAGE_DISK_REVERIFY`
    //    re-arms the on-disk walk as a cross-check on a real machine, and `atpkg verify`
    //    — the surface whose claim really IS "what is on disk right now" — still walks the
    //    tree, unchanged.
    if !artifact.tree_root.is_empty() {
        let got = match reverified_root(&incoming, extracted_root, disk_reverify_armed()) {
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

/// Lay the verified download at `archive` down under `dest` in the shape
/// `artifact.payload` names, and return the `tree_root` of what was laid down.
///
/// THE ONE PRODUCER of a staged tree, shared by the client stage ([`verify_and_stage`],
/// step 2) and the authoring ceremony: the tool that writes a signed row's `tree_root`
/// must stage through THIS function over the same download, or the two roots disagree
/// by construction — the normalizations below (mode sanitizing, `strip_components`,
/// the `links` shape, which `.app` is copied) are not something a second implementation
/// can be trusted to reproduce. Lanes, by `payload`:
///
/// * `""` — a release bundle (`binary` / `sysroot-bundle`): `.tar.zst`, symlinks
///   refused, nothing stripped — byte-for-byte the historical extraction;
/// * `tar-zst` / `tar-gz` / `zip` — a vendor archive: `strip_components` applied and
///   in-root symlinks admitted ([`crate::extract::ExtractOptions`]), modes sanitized to
///   `0755`/`0644` exactly as for a release bundle;
/// * `raw-binary` — the download IS the binary: it becomes `bin/<entry>`, mode `0755`;
/// * `dmg` — the single `.app` at the image root, copied with `ditto` (mode bits
///   PRESERVED, not sanitized: the bundle is the vendor's, signed and notarized as
///   laid out), macOS only.
///
/// Then, for every lane, the row's `links` are created as RELATIVE symlinks
/// `bin/<name> -> ../<target>` ([`apply_links`]) so the shims resolve `bin/<tool>`. The
/// root is folded from what was written for the archive and raw lanes; the `dmg`
/// lane walks the finished tree once (its bytes were laid by `ditto`, not by this
/// process's write loop).
///
/// `dest` must be an empty (or absent) directory the caller owns — the fold's
/// precondition, enforced by every lane. On error the caller removes `dest`.
///
/// # Errors
/// [`StageError::Extract`] for anything the extractor refused (a slip, a cap, a
/// malformed container), [`StageError::Payload`] for a row the lane cannot honour, and
/// [`StageError::Io`] for the filesystem.
pub fn stage_payload(
    artifact: &Artifact,
    archive: &Path,
    dest: &Path,
) -> Result<String, StageError> {
    let cap = size_cap(artifact);
    let vendor = ExtractOptions {
        strip_components: artifact.strip_components,
        in_root_symlinks: true,
    };
    let mut folded: Option<TreeAccumulator> = match artifact.payload.as_str() {
        "" => Some(
            extract_tar_zst_tree(archive, dest, cap, MAX_ENTRIES, ExtractOptions::default())
                .map_err(StageError::Extract)?,
        ),
        "tar-zst" => Some(
            extract_tar_zst_tree(archive, dest, cap, MAX_ENTRIES, vendor)
                .map_err(StageError::Extract)?,
        ),
        "tar-gz" => Some(
            extract_tar_gz_tree(archive, dest, cap, MAX_ENTRIES, vendor)
                .map_err(StageError::Extract)?,
        ),
        "zip" => Some(
            extract_zip_tree(archive, dest, cap, MAX_ENTRIES, vendor)
                .map_err(StageError::Extract)?,
        ),
        "raw-binary" => Some(stage_raw_binary(archive, dest, &artifact.entry, cap)?),
        "dmg" => {
            stage_dmg(archive, dest)?;
            None
        }
        other => return Err(payload2("unknown payload lane: ", other)),
    };
    apply_links(dest, &artifact.links, folded.as_mut())?;
    match folded {
        Some(tree) => Ok(tree.root()),
        None => tree_root(dest).map_err(StageError::Io),
    }
}

/// A [`StageError::Payload`] from `<head><detail>` (manual concat — see `lib.rs` on
/// `format!`).
fn payload2(head: &str, detail: &str) -> StageError {
    let mut m = String::from(head);
    m.push_str(detail);
    StageError::Payload(m)
}

/// The `raw-binary` lane: the download becomes `bin/<entry>` at mode `0755` — under the
/// platform's EXECUTABLE spelling of the logical name (`bin/claude` on Unix,
/// `bin/claude.exe` on Windows: [`crate::store::ToolName::exe_file`], the file the shim
/// forwards to) — through the very write loop the archive lanes use, so the one file
/// folds into the digest exactly as an archived `bin/<entry>` would.
fn stage_raw_binary(
    archive: &Path,
    dest: &Path,
    entry: &str,
    cap: u64,
) -> Result<TreeAccumulator, StageError> {
    // Belt and braces over `vendor::check_row` (which the client ran before download):
    // this function is also the AUTHORING producer, and a separator here would be a
    // path, not a name.
    let Some(tool) = crate::store::ToolName::new(entry) else {
        return Err(payload2(
            "raw-binary entry is not a single admissible tool name: ",
            entry,
        ));
    };
    std::fs::create_dir_all(dest).map_err(StageError::Io)?;
    crate::extract::require_empty_destination(dest).map_err(StageError::Extract)?;
    let bin = dest.join("bin");
    std::fs::create_dir_all(&bin).map_err(StageError::Io)?;
    crate::platform::set_mode(&bin, 0o755).map_err(StageError::Io)?;
    let target = bin.join(tool.exe_file());
    let file = std::fs::File::open(archive).map_err(StageError::Io)?;
    let written =
        crate::extract::stage_file(file, &target, 0o755, cap).map_err(StageError::Extract)?;
    let mut tree = TreeAccumulator::new();
    let rel = crate::extract::rel_bytes_under(dest, &target).map_err(StageError::Extract)?;
    tree.record_file(rel, written.mode, written.content_sha_hex);
    Ok(tree)
}

/// Whether a `links` TARGET is admissible here: a relative, `..`-free, `.`-free,
/// separator-clean path (the same rule `vendor::check_row` applied to the signed row;
/// repeated because this is also the authoring producer).
fn link_target_admissible(target: &str) -> bool {
    if target.is_empty()
        || target.starts_with('/')
        || target.ends_with('/')
        || target.contains('\0')
        || target.contains('\\')
    {
        return false;
    }
    target
        .split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && Path::new(target)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Create the row's `links`: for every `(name, target)`, the RELATIVE symlink
/// `bin/<name> -> ../<target>`, and record it in the open fold (when there is one) so
/// the root closes over the finished tree. Fail-closed on a name that is not a tool
/// name, a target that is not a clean relative path, a target absent from the staged
/// tree (an authoring slip: the link would dangle), or a `bin/<name>` something already
/// occupies (the link would either fail or shadow an extracted entry).
fn apply_links(
    dest: &Path,
    links: &BTreeMap<String, String>,
    mut tree: Option<&mut TreeAccumulator>,
) -> Result<(), StageError> {
    if links.is_empty() {
        return Ok(());
    }
    let bin = dest.join("bin");
    for (name, target) in links {
        if crate::store::ToolName::new(name).is_none() {
            return Err(payload2(
                "links name is not an admissible tool name: ",
                name,
            ));
        }
        if !link_target_admissible(target) {
            return Err(payload2(
                "links target must be a relative, `..`-free path inside the staged tree: ",
                target,
            ));
        }
        if std::fs::symlink_metadata(dest.join(target)).is_err() {
            return Err(payload2("links target is not in the staged tree: ", target));
        }
        std::fs::create_dir_all(&bin).map_err(StageError::Io)?;
        crate::platform::set_mode(&bin, 0o755).map_err(StageError::Io)?;
        let link = bin.join(name);
        if std::fs::symlink_metadata(&link).is_ok() {
            return Err(payload2(
                "links name collides with a staged entry: bin/",
                name,
            ));
        }
        let mut rel_target = PathBuf::from("..");
        rel_target.push(target);
        crate::extract::create_symlink(&rel_target, &link).map_err(StageError::Io)?;
        if let Some(tree) = tree.as_deref_mut() {
            let rel = crate::extract::rel_bytes_under(dest, &link).map_err(StageError::Extract)?;
            let target_bytes = crate::call1(crate::platform::os_str_bytes, rel_target.as_os_str());
            tree.record_symlink(rel, target_bytes);
        }
    }
    Ok(())
}

/// The mount point for a `dmg` stage: a SIBLING of `dest` named `<dest>.mnt`. The
/// name is deliberately outside every scratch recogniser (`store::stage_scratch_of`
/// wants `<n>.incoming-<digits>`), so neither the store sweep nor GC will ever
/// `remove_dir_all` a directory that may be a live mount; the [`Mount`] guard is what
/// reclaims it, on every path.
#[cfg(target_os = "macos")]
fn dmg_mount_point(dest: &Path) -> Result<PathBuf, StageError> {
    let name = crate::call1(std::path::Path::file_name, dest)
        .and_then(|n| crate::call1(std::ffi::OsStr::to_str, n))
        .ok_or_else(|| {
            payload2(
                "dmg stage dir has no name: ",
                &crate::call1(std::path::Path::to_string_lossy, dest),
            )
        })?;
    let mut mnt = String::from(name);
    mnt.push_str(".mnt");
    Ok(dest.with_file_name(mnt))
}

/// Run `/usr/bin/<tool>` to completion with no stdin; a non-zero exit is a
/// [`StageError::Payload`] naming the tool and the tail of its stderr.
#[cfg(target_os = "macos")]
fn run_tool(tool: &str, args: &[&std::ffi::OsStr]) -> Result<(), StageError> {
    let mut path = String::from("/usr/bin/");
    path.push_str(tool);
    let out = std::process::Command::new(&path)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(StageError::Io)?;
    if out.status.success() {
        return Ok(());
    }
    let mut m = String::from(tool);
    m.push_str(" failed");
    if let Some(code) = out.status.code() {
        m.push_str(" (exit ");
        m.push_str(&crate::dec_u64(u64::from(code.unsigned_abs())));
        m.push(')');
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let tail = stderr.trim();
    if !tail.is_empty() {
        m.push_str(": ");
        // The LAST line is where hdiutil/ditto put the reason.
        m.push_str(tail.lines().last().unwrap_or(tail));
    }
    Err(StageError::Payload(m))
}

/// An attached disk image, detached on EVERY path (`Drop` for the error paths, an
/// explicit [`Mount::detach`] on the happy path so a detach failure is reported
/// rather than swallowed). The mount point directory is removed with it.
#[cfg(target_os = "macos")]
struct Mount {
    point: PathBuf,
    attached: bool,
}

#[cfg(target_os = "macos")]
impl Mount {
    /// `hdiutil attach -nobrowse -readonly -noverify -noautoopen -quiet -mountpoint
    /// <point> <image>`: not in Finder, never written, no second checksum pass (the
    /// download's sha256 gate already ran over these bytes), nothing auto-opened. With
    /// stdin closed an image that demands a license click fails instead of hanging.
    fn attach(image: &Path, dest: &Path) -> Result<Self, StageError> {
        let point = dmg_mount_point(dest)?;
        // An EMPTY leftover from a crashed run is ours; a live mount there refuses the
        // `remove_dir` and then refuses the attach below, which is the right outcome.
        let _ = std::fs::remove_dir(&point);
        std::fs::create_dir_all(&point).map_err(StageError::Io)?;
        let mut m = Mount {
            point,
            attached: false,
        };
        if let Err(e) = run_tool(
            "hdiutil",
            &[
                "attach".as_ref(),
                "-nobrowse".as_ref(),
                "-readonly".as_ref(),
                "-noverify".as_ref(),
                "-noautoopen".as_ref(),
                "-quiet".as_ref(),
                "-mountpoint".as_ref(),
                m.point.as_os_str(),
                image.as_os_str(),
            ],
        ) {
            let _ = std::fs::remove_dir(&m.point);
            return Err(e);
        }
        m.attached = true;
        Ok(m)
    }

    /// The ONE `.app` directory at the image root. Anything else there (`Applications`
    /// link, `.background`, `.DS_Store`, a README) is ignored; zero or several apps is
    /// a refusal — the row said which bundle to stage by saying there is one.
    fn single_app(&self) -> Result<PathBuf, StageError> {
        let mut apps: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(&self.point)
            .map_err(StageError::Io)?
            .flatten()
        {
            let path = entry.path();
            let is_app = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("app"));
            // A real directory, not a link to one.
            if is_app && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
                apps.push(path);
            }
        }
        apps.sort();
        if apps.len() == 1 {
            return Ok(apps.swap_remove(0));
        }
        Err(payload2(
            "dmg image root must hold exactly one .app, found ",
            &crate::dec_u64(apps.len() as u64),
        ))
    }

    fn detach_now(&mut self) -> Result<(), StageError> {
        if !self.attached {
            return Ok(());
        }
        self.attached = false;
        let quiet = run_tool(
            "hdiutil",
            &["detach".as_ref(), "-quiet".as_ref(), self.point.as_os_str()],
        );
        let r = match quiet {
            Ok(()) => Ok(()),
            // Something still has a file open on the image (Spotlight is the usual
            // culprit): force it, once.
            Err(_) => run_tool(
                "hdiutil",
                &[
                    "detach".as_ref(),
                    "-force".as_ref(),
                    "-quiet".as_ref(),
                    self.point.as_os_str(),
                ],
            ),
        };
        let _ = std::fs::remove_dir(&self.point);
        r
    }

    /// Detach and report. Consumes the guard so `Drop` has nothing left to do.
    fn detach(mut self) -> Result<(), StageError> {
        self.detach_now()
    }
}

#[cfg(target_os = "macos")]
impl Drop for Mount {
    fn drop(&mut self) {
        let _ = self.detach_now();
    }
}

/// The `dmg` lane: attach the image read-only, require exactly one `.app` at its
/// root, `ditto` that bundle into `dest` (modes, symlinks and extended attributes
/// preserved, as Finder would), refuse any link in it that leaves the stage root
/// ([`vet_copied_tree`]), detach. Nothing else on the image is copied and nothing on
/// it is executed.
#[cfg(target_os = "macos")]
fn stage_dmg(image: &Path, dest: &Path) -> Result<(), StageError> {
    std::fs::create_dir_all(dest).map_err(StageError::Io)?;
    crate::extract::require_empty_destination(dest).map_err(StageError::Extract)?;
    let mount = Mount::attach(image, dest)?;
    let app = mount.single_app()?;
    let name = crate::call1(std::path::Path::file_name, &app).ok_or_else(|| {
        payload2(
            "dmg bundle has no name: ",
            &crate::call1(std::path::Path::to_string_lossy, &app),
        )
    })?;
    let out = dest.join(name);
    run_tool("ditto", &[app.as_os_str(), out.as_os_str()])?;
    // `ditto` laid the bundle VERBATIM, links included, with nothing of ours vetting
    // them on the way — so vet them now, before the mount is released and long before
    // the swap: the archive lanes' in-root rule, applied to the finished copy.
    if let Err(e) = vet_copied_tree(dest, &out) {
        let _ = mount.detach();
        return Err(e);
    }
    mount.detach()
}

/// Every SYMLINK under `dir` (the copied `.app`) must resolve LEXICALLY inside the
/// stage `root` — the rule the archive lanes enforce per entry
/// ([`crate::extract::vet_symlink`]), applied after the fact because `ditto` preserves
/// links as the image carries them. An image whose bundle holds
/// `Contents/MacOS/x -> /usr/bin/x` or `-> ../../../..` would otherwise sit in a tree
/// the digest describes only by target bytes, and a `links` target or a shim could then
/// resolve through it to somewhere outside the store. Anything that is not a file, a
/// directory or a symlink is refused as well (the walk would refuse it at the re-verify,
/// but saying which entry is better than a bare mismatch).
#[cfg(target_os = "macos")]
fn vet_copied_tree(root: &Path, dir: &Path) -> Result<(), StageError> {
    for entry in std::fs::read_dir(dir).map_err(StageError::Io)? {
        let entry = entry.map_err(StageError::Io)?;
        let path = entry.path();
        // `DirEntry::file_type` never follows a link.
        let ft = entry.file_type().map_err(StageError::Io)?;
        if ft.is_symlink() {
            let rel = path.strip_prefix(root).map_err(|_| {
                StageError::Extract(ExtractError::Rejected(
                    crate::extract::ExtractReject::RootEscape,
                    path.clone(),
                ))
            })?;
            let target = std::fs::read_link(&path).map_err(StageError::Io)?;
            crate::extract::vet_symlink(root, rel, &target, 0)
                .map_err(|r| StageError::Extract(ExtractError::Rejected(r, rel.to_path_buf())))?;
        } else if ft.is_dir() {
            vet_copied_tree(root, &path)?;
        } else if !ft.is_file() {
            return Err(payload2(
                "dmg bundle carries an entry that is not a file, directory or symlink: ",
                &crate::call1(std::path::Path::to_string_lossy, &path),
            ));
        }
    }
    Ok(())
}

/// `dmg` needs `hdiutil` and `ditto`; off macOS the lane fails closed.
#[cfg(not(target_os = "macos"))]
fn stage_dmg(_image: &Path, _dest: &Path) -> Result<(), StageError> {
    Err(StageError::Payload(String::from(
        "dmg payloads can only be staged on macOS (hdiutil/ditto)",
    )))
}

/// Whether [`DISK_REVERIFY_ENV`] arms the on-disk cross-check. Read here, passed DOWN as a
/// bool, so the decision is one env lookup per stage and [`reverified_root`] stays a pure
/// function two tests can drive both ways without mutating process-global state (which
/// `std::env::set_var` is `unsafe` for in edition 2024, and is a data race under a
/// multi-threaded test runner regardless).
fn disk_reverify_armed() -> bool {
    std::env::var_os(DISK_REVERIFY_ENV).is_some_and(|v| !v.is_empty())
}

/// The root step 3 compares against the signed value: the one the extractor folded, or —
/// when [`DISK_REVERIFY_ENV`] is armed — that root AND a full on-disk
/// [`crate::tree::tree_root`] walk, which must agree.
///
/// Disagreement is an ERROR, never a silent preference for one of them: two producers of a
/// cross-version byte contract that differ over the same tree is exactly the condition a
/// fail-closed stage exists for, and the message names both so the drift is diagnosable
/// rather than merely fatal.
fn reverified_root(
    incoming: &Path,
    extracted_root: String,
    armed: bool,
) -> std::io::Result<String> {
    if !armed {
        return Ok(extracted_root);
    }
    let walked = tree_root(incoming)?;
    if !walked.eq_ignore_ascii_case(&extracted_root) {
        let mut msg = String::from("staged tree_root disagreement: the extraction folded ");
        msg.push_str(&extracted_root);
        msg.push_str(" but the on-disk walk read ");
        msg.push_str(&walked);
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, msg));
    }
    Ok(walked)
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
            url: String::new(),
            payload: String::new(),
            entry: String::new(),
            strip_components: 0,
            links: std::collections::BTreeMap::new(),
            vendor: String::new(),
            protocol: "github-release".into(),
            signer_team: String::new(),
            elevated: false,
            provides: vec![],
            manager: String::new(),
            package: String::new(),
            label_prefix: String::new(),
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
        // NOTE — this fixture is a live DIFFERENTIAL, not just a fixture. The expected
        // `tree_root` is learned by walking the extracted tree ON DISK, while
        // `verify_and_stage` compares against the root the extractor FOLDS as it writes.
        // Every staging test below therefore fails the moment the two producers of that
        // cross-version byte contract disagree over this bundle — hardlinks, modes, empty
        // files and all — on top of the exhaustive corpus in `extract.rs`.
        crate::extract::extract_tar_zst(&archive, &probe, 1 << 20, 1000).unwrap();
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
    /// The armed cross-check: with the on-disk walk re-armed, an agreeing pair is passed
    /// through unchanged and a DISAGREEING pair fails the stage closed, naming both roots.
    ///
    /// Driven as a pure function rather than through the env var: `std::env::set_var` is
    /// `unsafe` in edition 2024 and racy under a multi-threaded runner, and what needs
    /// pinning is the DECISION, not the lookup.
    #[test]
    fn the_armed_disk_reverify_agrees_or_fails_closed() {
        let b = bundle("armed-reverify");
        let probe = b.dir.join("armed-probe");
        let fused =
            crate::extract::extract_tar_zst_rooted(&b.archive, &probe, 1 << 20, 1000).unwrap();
        let walked = tree_root(&probe).unwrap();
        // Reach guard: the corpus must be non-empty, or "they agree" is vacuous.
        assert_eq!(fused.len(), 64);
        assert_eq!(
            fused, walked,
            "the two producers must agree over this bundle"
        );

        // Disarmed: the fused root is returned verbatim, no walk.
        assert_eq!(
            reverified_root(&probe, fused.clone(), false).unwrap(),
            fused
        );
        // Armed and agreeing: still the same 64 characters.
        assert_eq!(reverified_root(&probe, fused.clone(), true).unwrap(), fused);
        // Armed and DISAGREEING: fail closed, and say which two roots disagreed.
        let bogus = "0".repeat(64);
        let err = reverified_root(&probe, bogus.clone(), true).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains(&bogus),
            "the folded root must be named: {text}"
        );
        assert!(
            text.contains(&walked),
            "the walked root must be named: {text}"
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// A tree mutated AFTER extraction is still refused when the walk is armed — the
    /// window the fused digest gives up is exactly this one, so the escape hatch has to
    /// actually close it.
    #[test]
    fn the_armed_walk_still_catches_a_post_extraction_mutation() {
        let b = bundle("armed-mutation");
        let probe = b.dir.join("mut-probe");
        let fused =
            crate::extract::extract_tar_zst_rooted(&b.archive, &probe, 1 << 20, 1000).unwrap();
        // Mutate one extracted file in place, exactly as a TOCTOU attacker would.
        let victim = first_regular_file(&probe).expect("the fixture bundle has a file");
        std::fs::write(&victim, b"swapped after extraction").unwrap();
        assert!(
            reverified_root(&probe, fused, true).is_err(),
            "an armed re-verify must refuse a tree mutated after the write"
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// The first regular file under `dir`, in walk order — the mutation victim above.
    fn first_regular_file(dir: &Path) -> Option<PathBuf> {
        let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for e in entries {
            let p = e.path();
            let meta = std::fs::symlink_metadata(&p).ok()?;
            if meta.is_file() {
                return Some(p);
            }
            if meta.is_dir()
                && let Some(found) = first_regular_file(&p)
            {
                return Some(found);
            }
        }
        None
    }

    // ===== the https payload lanes =====

    #[cfg(unix)]
    use crate::extract::fixtures::gzip_bytes;
    use crate::extract::fixtures::{ZipMember, tar_bytes, zip_bytes};

    /// An https artifact over `archive`, signed for ITS bytes and for `root`.
    fn vendor_artifact(archive: &Path, payload: &str, root: &str) -> Artifact {
        let mut a = artifact(&file_sha256(archive).unwrap(), root);
        a.kind = if payload == "dmg" {
            "app-bundle".into()
        } else {
            "binary".into()
        };
        a.protocol = "https".into();
        a.payload = payload.into();
        a.size = std::fs::metadata(archive).unwrap().len();
        a
    }

    /// Write `content` at `dir/rel` with `mode`, creating parents — the REPLICA the
    /// expected roots below are learned from, so no expectation ever comes out of the
    /// function under test.
    #[cfg(unix)]
    fn lay(dir: &Path, rel: &str, content: &[u8], mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    fn lay_link(dir: &Path, rel: &str, target: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, p).unwrap();
    }

    /// The `gh` archive shape as tar bytes: a versioned top-level directory to strip,
    /// an executable, a plain file.
    fn gh_tar() -> Vec<u8> {
        tar_bytes(&[
            ("gh_2.80.0_macOS_arm64/", b'5', "", b"", 0o755),
            (
                "gh_2.80.0_macOS_arm64/bin/gh",
                b'0',
                "",
                b"#!/bin/sh\necho gh\n",
                0o755,
            ),
            ("gh_2.80.0_macOS_arm64/LICENSE", b'0', "", b"MIT", 0o644),
        ])
    }

    /// …and the same shape as a zip.
    fn gh_zip() -> Vec<u8> {
        zip_bytes(
            &[
                ZipMember {
                    name: "gh_2.80.0_macOS_arm64/",
                    mode: 0o040_755,
                    data: b"",
                    deflate: false,
                },
                ZipMember {
                    name: "gh_2.80.0_macOS_arm64/bin/gh",
                    mode: 0o100_755,
                    data: b"#!/bin/sh\necho gh\n",
                    deflate: true,
                },
                ZipMember {
                    name: "gh_2.80.0_macOS_arm64/LICENSE",
                    mode: 0o100_644,
                    data: b"MIT",
                    deflate: false,
                },
            ],
            false,
        )
    }

    /// The tree the `gh` shape must stage to, learned from a hand-laid replica.
    #[cfg(unix)]
    fn gh_expected_root(dir: &Path) -> String {
        let replica = dir.join("replica");
        lay(&replica, "bin/gh", b"#!/bin/sh\necho gh\n", 0o755);
        lay(&replica, "LICENSE", b"MIT", 0o644);
        tree_root(&replica).unwrap()
    }

    /// The `tar-gz` and `zip` lanes stage the `gh` shape — top level stripped, modes
    /// sanitized — to the root a hand-laid replica walks to, and a row that FORGOT its
    /// `strip_components` is refused at the re-verify (the tree is honestly different).
    #[cfg(unix)]
    #[test]
    fn tar_gz_and_zip_vendor_payloads_stage_with_strip_components() {
        let d = tmp("vendor-archives");
        let expected = gh_expected_root(&d);
        let gz = d.join("gh.tar.gz");
        std::fs::write(&gz, gzip_bytes(&gh_tar())).unwrap();
        let zip = d.join("gh.zip");
        std::fs::write(&zip, gh_zip()).unwrap();
        for (label, archive, payload) in [("gz", &gz, "tar-gz"), ("zip", &zip, "zip")] {
            let mut art = vendor_artifact(archive, payload, &expected);
            art.strip_components = 1;
            let build = d.join(format!("store/gh-{label}/18"));
            verify_and_stage(&art, archive, &build).unwrap_or_else(|e| panic!("{label}: {e}"));
            assert!(crate::store::build_is_complete(&build), "{label}");
            assert_eq!(
                std::fs::read(build.join("bin/gh")).unwrap(),
                b"#!/bin/sh\necho gh\n",
                "{label}"
            );
            assert_eq!(
                std::fs::read(build.join("LICENSE")).unwrap(),
                b"MIT",
                "{label}"
            );
            assert!(
                !build.join("gh_2.80.0_macOS_arm64").exists(),
                "{label}: stripped"
            );
            assert_eq!(
                tree_root(&build).unwrap(),
                expected,
                "{label}: the walk agrees after the swap"
            );
            assert!(scratch_beside(&build).is_empty(), "{label}");

            // Unstripped, the tree is `gh_.../bin/gh` — a different root, refused.
            let mut unstripped = vendor_artifact(archive, payload, &expected);
            unstripped.strip_components = 0;
            let build2 = d.join(format!("store/gh-{label}-unstripped/18"));
            let err = verify_and_stage(&unstripped, archive, &build2).unwrap_err();
            assert!(
                matches!(err, StageError::TreeRootMismatch { .. }),
                "{label}: {err:?}"
            );
            assert!(
                !build2.exists(),
                "{label}: nothing staged on a root mismatch"
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The `tar-zst` vendor lane is the historical extractor plus the vendor options
    /// — the same tar bytes reach the same root as the gzip lane.
    #[cfg(unix)]
    #[test]
    fn tar_zst_vendor_payload_matches_the_gzip_lane() {
        let d = tmp("vendor-zst");
        let expected = gh_expected_root(&d);
        let zst = d.join("gh.tar.zst");
        std::fs::write(&zst, zstd::encode_all(&gh_tar()[..], 0).unwrap()).unwrap();
        let mut art = vendor_artifact(&zst, "tar-zst", &expected);
        art.strip_components = 1;
        let build = d.join("store/gh/18");
        verify_and_stage(&art, &zst, &build).unwrap();
        assert_eq!(tree_root(&build).unwrap(), expected);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The `raw-binary` lane: the download becomes `bin/<entry>` at `0755`, and the
    /// root it folds is the one a hand-laid `bin/<entry>` walks to. An `entry` that is
    /// not a bare tool name is refused before anything is written.
    #[cfg(unix)]
    #[test]
    fn raw_binary_payload_becomes_bin_entry_at_0755() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("vendor-raw");
        let payload: Vec<u8> = (0..300_000usize).map(|i| (i % 253) as u8).collect();
        let dl = d.join("claude-2.1.231-darwin-arm64");
        std::fs::write(&dl, &payload).unwrap();
        let replica = d.join("replica");
        lay(&replica, "bin/claude", &payload, 0o755);
        let expected = tree_root(&replica).unwrap();

        let mut art = vendor_artifact(&dl, "raw-binary", &expected);
        art.entry = "claude".into();
        let build = d.join("store/claude/2026082601");
        verify_and_stage(&art, &dl, &build).unwrap();
        assert!(crate::store::build_is_complete(&build));
        assert_eq!(std::fs::read(build.join("bin/claude")).unwrap(), payload);
        assert_eq!(
            std::fs::metadata(build.join("bin/claude"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o755
        );
        assert_eq!(tree_root(&build).unwrap(), expected);
        // The download itself is untouched (flow reclaims it).
        assert_eq!(std::fs::metadata(&dl).unwrap().len(), payload.len() as u64);

        for bad in ["", "bin/claude", "../claude", "sudo"] {
            let mut art = vendor_artifact(&dl, "raw-binary", &expected);
            art.entry = bad.into();
            let build = d.join("store/bad/1");
            let err = verify_and_stage(&art, &dl, &build).unwrap_err();
            assert!(matches!(err, StageError::Payload(_)), "{bad:?}: {err:?}");
            assert!(!build.exists(), "{bad:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `links` lay RELATIVE symlinks `bin/<name> -> ../<target>` after extraction, fold
    /// into the root exactly as the walk reads them, and resolve to the staged file.
    /// A dangling target, a `..` target, and a name that collides with an extracted
    /// entry are each refused with nothing staged.
    #[cfg(unix)]
    #[test]
    fn links_lay_relative_symlinks_under_bin_and_fold_into_the_root() {
        let d = tmp("vendor-links");
        let gz = d.join("emacs.tar.gz");
        std::fs::write(
            &gz,
            gzip_bytes(&tar_bytes(&[
                (
                    "Emacs.app/Contents/MacOS/Emacs",
                    b'0',
                    "",
                    b"#!/bin/sh\necho emacs\n",
                    0o755,
                ),
                (
                    "Emacs.app/Contents/MacOS/bin/emacsclient",
                    b'0',
                    "",
                    b"#!/bin/sh\necho client\n",
                    0o755,
                ),
                ("bin/taken", b'0', "", b"already here", 0o644),
            ])),
        )
        .unwrap();
        let replica = d.join("replica");
        lay(
            &replica,
            "Emacs.app/Contents/MacOS/Emacs",
            b"#!/bin/sh\necho emacs\n",
            0o755,
        );
        lay(
            &replica,
            "Emacs.app/Contents/MacOS/bin/emacsclient",
            b"#!/bin/sh\necho client\n",
            0o755,
        );
        lay(&replica, "bin/taken", b"already here", 0o644);
        lay_link(&replica, "bin/emacs", "../Emacs.app/Contents/MacOS/Emacs");
        lay_link(
            &replica,
            "bin/emacsclient",
            "../Emacs.app/Contents/MacOS/bin/emacsclient",
        );
        let expected = tree_root(&replica).unwrap();

        let mut art = vendor_artifact(&gz, "tar-gz", &expected);
        art.links
            .insert("emacs".into(), "Emacs.app/Contents/MacOS/Emacs".into());
        art.links.insert(
            "emacsclient".into(),
            "Emacs.app/Contents/MacOS/bin/emacsclient".into(),
        );
        let build = d.join("store/emacs/18");
        verify_and_stage(&art, &gz, &build).unwrap();
        assert_eq!(
            std::fs::read_link(build.join("bin/emacs")).unwrap(),
            Path::new("../Emacs.app/Contents/MacOS/Emacs")
        );
        assert_eq!(
            std::fs::read(build.join("bin/emacs")).unwrap(),
            b"#!/bin/sh\necho emacs\n"
        );
        assert_eq!(
            std::fs::read(build.join("bin/emacsclient")).unwrap(),
            b"#!/bin/sh\necho client\n"
        );
        assert_eq!(tree_root(&build).unwrap(), expected);
        // Without the links the root is different: the fold really carries them.
        let plain = vendor_artifact(&gz, "tar-gz", &expected);
        let err = verify_and_stage(&plain, &gz, &d.join("store/plain/18")).unwrap_err();
        assert!(
            matches!(err, StageError::TreeRootMismatch { .. }),
            "{err:?}"
        );

        let refused: &[(&str, &str, &str)] = &[
            ("dangling", "emacs", "Emacs.app/Contents/MacOS/Nope"),
            ("dotdot", "emacs", "../outside"),
            (
                "absolute",
                "emacs",
                "/Applications/Emacs.app/Contents/MacOS/Emacs",
            ),
            ("collides", "taken", "Emacs.app/Contents/MacOS/Emacs"),
            ("bad-name", "bin/emacs", "Emacs.app/Contents/MacOS/Emacs"),
        ];
        for (label, name, target) in refused {
            let mut art = vendor_artifact(&gz, "tar-gz", &expected);
            art.links.insert((*name).into(), (*target).into());
            let build = d.join(format!("store/{label}/18"));
            let err = verify_and_stage(&art, &gz, &build).unwrap_err();
            assert!(matches!(err, StageError::Payload(_)), "{label}: {err:?}");
            assert!(!build.exists(), "{label}: nothing staged");
            assert!(scratch_beside(&build).is_empty(), "{label}: no scratch");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A slip in a vendor archive aborts the stage in extraction — nothing outside the
    /// scratch, no scratch left, and (for a re-stage) the installed build untouched —
    /// through the gzip lane and the zip lane alike.
    #[cfg(unix)]
    #[test]
    fn a_slip_in_a_vendor_archive_aborts_before_the_swap_in_every_lane() {
        let d = tmp("vendor-slip");
        let expected = gh_expected_root(&d);
        // Install the good gzip build first, so there is something to survive.
        let good = d.join("gh.tar.gz");
        std::fs::write(&good, gzip_bytes(&gh_tar())).unwrap();
        let mut art = vendor_artifact(&good, "tar-gz", &expected);
        art.strip_components = 1;
        let build = d.join("store/gh/18");
        verify_and_stage(&art, &good, &build).unwrap();
        let witness = build.join("this-tree-survived");
        std::fs::write(&witness, b"old").unwrap();

        let slip_tar = tar_bytes(&[
            ("top/ok", b'0', "", b"fine", 0o644),
            ("top/../../escape", b'0', "", b"pwned", 0o644),
        ]);
        let gz = d.join("slip.tar.gz");
        std::fs::write(&gz, gzip_bytes(&slip_tar)).unwrap();
        let zip = d.join("slip.zip");
        std::fs::write(
            &zip,
            zip_bytes(
                &[
                    ZipMember {
                        name: "top/ok",
                        mode: 0o100_644,
                        data: b"fine",
                        deflate: false,
                    },
                    ZipMember {
                        name: "top/../../escape",
                        mode: 0o100_644,
                        data: b"pwned",
                        deflate: true,
                    },
                    ZipMember {
                        name: "top/bin/x",
                        mode: 0o120_777,
                        data: b"../../../etc/passwd",
                        deflate: false,
                    },
                ],
                false,
            ),
        )
        .unwrap();
        for (label, archive, payload) in [("gz", &gz, "tar-gz"), ("zip", &zip, "zip")] {
            let mut art = vendor_artifact(archive, payload, "");
            art.strip_components = 1;
            let err = verify_and_stage(&art, archive, &build).unwrap_err();
            assert!(matches!(err, StageError::Extract(_)), "{label}: {err:?}");
            assert!(witness.exists(), "{label}: the installed build survived");
            assert!(crate::store::build_is_complete(&build), "{label}");
            assert!(scratch_beside(&build).is_empty(), "{label}: no scratch");
            assert!(
                !d.join("escape").exists() && !d.join("store/escape").exists(),
                "{label}"
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An unknown payload lane is refused before any byte is extracted; nothing staged.
    #[test]
    fn an_unknown_payload_lane_is_refused_before_extraction() {
        let b = bundle("unknown-payload");
        let mut art = b.art.clone();
        art.kind = "binary".into();
        art.protocol = "https".into();
        art.payload = "pkg".into();
        let build = b.build();
        let err = verify_and_stage(&art, &b.archive, &build).unwrap_err();
        assert!(matches!(err, StageError::Payload(_)), "{err:?}");
        assert!(!build.exists());
        assert!(scratch_beside(&build).is_empty());
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// The `dmg` lane, for real: a tiny image is built with `hdiutil create` around
    /// a `Foo.app` (an executable, a `0600` file, an internal symlink) beside the usual
    /// `Applications` link and a README; the stage copies ONLY the bundle, preserves its
    /// modes and links, lays the row's `links`, detaches the image, and the root equals a
    /// hand-laid replica's walk. Zero or two apps at the root are refused — detached
    /// either way. Runs only where `hdiutil` exists (this Mac); skips elsewhere.
    #[cfg(target_os = "macos")]
    #[test]
    fn dmg_app_payload_stages_the_single_app_and_its_links() {
        use std::os::unix::fs::PermissionsExt as _;
        if !Path::new("/usr/bin/hdiutil").exists() || !Path::new("/usr/bin/ditto").exists() {
            eprintln!("skipping: hdiutil/ditto not available");
            return;
        }
        let d = tmp("vendor-dmg");
        let src = d.join("src");
        lay(
            &src,
            "Foo.app/Contents/MacOS/foo",
            b"#!/bin/sh\necho foo\n",
            0o755,
        );
        lay(&src, "Foo.app/Contents/Info.plist", b"<plist/>", 0o644);
        lay(&src, "Foo.app/Contents/Resources/secret", b"s", 0o600);
        lay_link(&src, "Foo.app/Contents/current", "MacOS/foo");
        lay(&src, "README.txt", b"not copied", 0o644);
        lay_link(&src, "Applications", "/Applications");
        let dmg = d.join("foo.dmg");
        let status = std::process::Command::new("/usr/bin/hdiutil")
            .args(["create", "-quiet", "-srcfolder"])
            .arg(&src)
            .args(["-volname", "FooTest", "-format", "UDZO"])
            .arg(&dmg)
            .status()
            .unwrap();
        assert!(status.success(), "hdiutil create");

        let replica = d.join("replica");
        lay(
            &replica,
            "Foo.app/Contents/MacOS/foo",
            b"#!/bin/sh\necho foo\n",
            0o755,
        );
        lay(&replica, "Foo.app/Contents/Info.plist", b"<plist/>", 0o644);
        lay(&replica, "Foo.app/Contents/Resources/secret", b"s", 0o600);
        lay_link(&replica, "Foo.app/Contents/current", "MacOS/foo");
        lay_link(&replica, "bin/foo", "../Foo.app/Contents/MacOS/foo");
        let expected = tree_root(&replica).unwrap();

        let mut art = vendor_artifact(&dmg, "dmg", &expected);
        art.links
            .insert("foo".into(), "Foo.app/Contents/MacOS/foo".into());
        let build = d.join("store/foo/2026082601");
        verify_and_stage(&art, &dmg, &build).unwrap();
        assert!(crate::store::build_is_complete(&build));
        assert_eq!(
            std::fs::read(build.join("bin/foo")).unwrap(),
            b"#!/bin/sh\necho foo\n"
        );
        assert_eq!(
            std::fs::read_link(build.join("Foo.app/Contents/current")).unwrap(),
            Path::new("MacOS/foo")
        );
        assert_eq!(
            std::fs::metadata(build.join("Foo.app/Contents/Resources/secret"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o600,
            "dmg preserves mode bits"
        );
        assert!(
            !build.join("README.txt").exists(),
            "only the .app is copied"
        );
        assert!(!build.join("Applications").exists());
        assert_eq!(tree_root(&build).unwrap(), expected);
        assert!(scratch_beside(&build).is_empty());
        // The mount point (`<build>.incoming-<pid>.mnt`) is gone and the image is detached.
        let mounts: Vec<_> = std::fs::read_dir(build.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".mnt"))
            .collect();
        assert!(mounts.is_empty(), "no mount point left behind: {mounts:?}");
        let info = std::process::Command::new("/usr/bin/hdiutil")
            .arg("info")
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&info.stdout).contains(&d.display().to_string()),
            "the image must be detached"
        );

        // Two apps at the root, and none: refused, detached, nothing staged.
        for (label, apps) in [("two", &["A.app", "B.app"][..]), ("none", &[][..])] {
            let src2 = d.join(format!("src-{label}"));
            for app in apps {
                lay(&src2, &format!("{app}/Contents/MacOS/x"), b"x", 0o755);
            }
            lay(&src2, "README.txt", b"r", 0o644);
            let dmg2 = d.join(format!("{label}.dmg"));
            let status = std::process::Command::new("/usr/bin/hdiutil")
                .args(["create", "-quiet", "-srcfolder"])
                .arg(&src2)
                .args(["-volname", "Bad", "-format", "UDZO"])
                .arg(&dmg2)
                .status()
                .unwrap();
            assert!(status.success(), "hdiutil create {label}");
            let art = vendor_artifact(&dmg2, "dmg", &expected);
            let build2 = d.join(format!("store/{label}/1"));
            let err = verify_and_stage(&art, &dmg2, &build2).unwrap_err();
            assert!(matches!(err, StageError::Payload(_)), "{label}: {err:?}");
            assert!(!build2.exists(), "{label}");
            assert!(scratch_beside(&build2).is_empty(), "{label}");
            let info = std::process::Command::new("/usr/bin/hdiutil")
                .arg("info")
                .output()
                .unwrap();
            assert!(
                !String::from_utf8_lossy(&info.stdout).contains(&d.display().to_string()),
                "{label}: detached on the error path"
            );
        }

        // A link INSIDE the bundle that leaves the stage root — absolute, or `..` above
        // it — is refused after the copy and before the swap: an extraction-class
        // rejection naming the link, nothing staged, the image detached. A link that
        // climbs to the bundle's own root and back down is fine (frameworks do that).
        for (label, target, ok) in [
            ("abs-link", "/etc/passwd", false),
            ("up-link", "../../../../outside", false),
            ("in-root-link", "../../Bad.app/Contents/MacOS/x", true),
        ] {
            let src3 = d.join(format!("src-{label}"));
            lay(
                &src3,
                "Bad.app/Contents/MacOS/x",
                b"#!/bin/sh\nexit 0\n",
                0o755,
            );
            lay_link(&src3, "Bad.app/Contents/Resources/escape", target);
            let dmg3 = d.join(format!("{label}.dmg"));
            let status = std::process::Command::new("/usr/bin/hdiutil")
                .args(["create", "-quiet", "-srcfolder"])
                .arg(&src3)
                .args(["-volname", "Link", "-format", "UDZO"])
                .arg(&dmg3)
                .status()
                .unwrap();
            assert!(status.success(), "hdiutil create {label}");
            let replica3 = d.join(format!("replica-{label}"));
            lay(
                &replica3,
                "Bad.app/Contents/MacOS/x",
                b"#!/bin/sh\nexit 0\n",
                0o755,
            );
            lay_link(&replica3, "Bad.app/Contents/Resources/escape", target);
            let expected3 = tree_root(&replica3).unwrap();
            let art = vendor_artifact(&dmg3, "dmg", &expected3);
            let build3 = d.join(format!("store/{label}/1"));
            let res = verify_and_stage(&art, &dmg3, &build3);
            if ok {
                res.unwrap_or_else(|e| panic!("{label}: an in-root link is admitted: {e}"));
                assert_eq!(
                    std::fs::read_link(build3.join("Bad.app/Contents/Resources/escape")).unwrap(),
                    Path::new(target),
                    "{label}: laid verbatim"
                );
            } else {
                let err = res.unwrap_err();
                assert!(
                    matches!(err, StageError::Extract(ExtractError::Rejected(_, _))),
                    "{label}: {err:?}"
                );
                assert!(
                    err.to_string().contains("Resources/escape"),
                    "{label}: names the link: {err}"
                );
                assert!(!build3.exists(), "{label}: nothing staged");
                assert!(scratch_beside(&build3).is_empty(), "{label}");
            }
            let info = std::process::Command::new("/usr/bin/hdiutil")
                .arg("info")
                .output()
                .unwrap();
            assert!(
                !String::from_utf8_lossy(&info.stdout).contains(&d.display().to_string()),
                "{label}: detached"
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
