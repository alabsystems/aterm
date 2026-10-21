// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tar-slip-safe extraction vetting (§4.2/§15) — the fail-closed decision for whether a
//! single archive entry may be written under the per-program store root.
//!
//! A bundle is attacker-influenced input (it is only signature-verified as a
//! *compressed blob*; the extracted layout is re-checked against `tree_root` afterwards,
//! §8). The classic tar-slip escapes — an absolute path, a `..` traversal, or a
//! symlink/hardlink that redirects a later write outside the store — must each abort the
//! WHOLE staged group, never partially extract. This module is the pure, dependency-free
//! core of that defence: [`vet_entry`] turns a `(raw path, kind)` pair into either a
//! **safe absolute destination under the root** or a [`ExtractReject`]. Every reader
//! (the first-party tar parser in `crate::tarread`, the first-party zip parser in
//! `extract/zipread.rs`) feeds every entry through it before writing a single byte.
//!
//! **Fail closed by construction:**
//! * absolute paths, `..` components, and root/prefix components are refused;
//! * any non-regular/non-directory/non-link entry type is refused, except
//!   explicitly admitted descriptive global PAX metadata, which creates no file;
//! * a SYMLINK is refused outright by the release-bundle lane (`binary`,
//!   `sysroot-bundle`: the `exposes` shims are created by `atpkg` *after* extraction,
//!   never unpacked from the archive). The `https` payload lanes
//!   ([`SymlinkPolicy::InRoot`]) admit one ONLY when its target resolves
//!   *lexically inside the stage root* ([`vet_symlink`]), and then refuse any later entry
//!   that would write THROUGH it ([`ExtractReject::ThroughSymlink`] /
//!   [`ExtractReject::Occupied`]) — so a link can never redirect a write, and the tree
//!   the digest describes is the tree on disk. The verified Homebrew bundle alone
//!   preserves external targets as opaque leaf links ([`SymlinkPolicy::ExternalLeaf`]);
//!   the same guards forbid writing on or through every link;
//! * an in-root HARDLINK is admitted only after [`vet_hardlink_stripped`] walks BOTH ends through
//!   the same component vet, NEITHER path passes through a link laid earlier (`lstat`
//!   and `link` decline to follow only the LAST component, so the target's ancestors are
//!   walked too), AND the target is already an extracted regular file — a
//!   toolchain sysroot dedups identical binaries this way (`cargo`↔`targo`,
//!   `trustc`↔`rustc`, shared dylibs), and the materialized alias hashes into `tree_root`
//!   exactly like a regular file; an escaping, absolute, or forward-referencing link
//!   aborts the whole stage;
//! * every NAME a folding extraction lays — a file, a link, and each directory on the
//!   way to it — is created exclusively unless this extraction laid that exact name
//!   before, so a member the FILESYSTEM names the same as another's (`bin/tool` and
//!   `BIN/TOOL`, or `Dir/f` and `dir/g`, on case-insensitive APFS or NTFS; NFC and NFD
//!   spellings on APFS) is refused instead of silently replacing it or moving into its
//!   directory ([`ExtractReject::Occupied`]); and a name the filesystem stores under
//!   other bytes than it was given (HFS+ decomposing Unicode, Win32 dropping a trailing
//!   dot) is refused when the stage reads its names back ([`ExtractReject::Respelled`]);
//! * the validated path is re-joined to the root and confirmed to still be under it
//!   (defence in depth against any normalization surprise).
//!
//! **One writer for every lane.** The decompressors differ (zstd, gzip, deflate-in-zip)
//! and the container parsers differ (tar, zip), but everything that touches the disk —
//! the vet, `strip_components`, the entry-count and byte caps, the mode sanitizing, the
//! symlink discipline and the `tree_root` fold — is one code path, [`Layer`]. A slip rule
//! that held for `.tar.zst` therefore holds for `.tar.gz` and `.zip` by construction, not
//! by parallel implementation.

use std::cell::Cell;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use crate::tarread::EntryType as TarEntryType;

mod zipread;

/// Why an archive entry was refused. Any variant aborts the entire staged group — a
/// half-extracted bundle never activates (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractReject {
    /// The entry path is absolute (`/etc/...`) — it must be relative to the store root.
    AbsolutePath,
    /// The entry path contains a `..` component (directory traversal).
    ParentTraversal,
    /// The entry path is empty or resolves to nothing after dropping `.` components.
    EmptyPath,
    /// The lexically-joined destination escaped the store root (belt-and-suspenders),
    /// or a symlink's target resolves outside it.
    RootEscape,
    /// A symlink where the lane refuses them, or any non-regular/non-directory/non-link
    /// entry — refused outright. (An in-root hardlink is separately vetted by
    /// [`vet_hardlink_stripped`]; one that fails that vet lands here or in the path rejections
    /// above.)
    DisallowedKind,
    /// A hardlink whose target had not been extracted by the time the link entry
    /// appeared — a forward/self reference no honest archiver produces.
    HardlinkTargetMissing,
    /// The entry's destination passes THROUGH a symlink laid down earlier in the same
    /// archive: the bytes would land somewhere other than the path the digest records.
    ThroughSymlink,
    /// A symlink entry names a path something already occupies, a file/directory entry
    /// names a path an earlier symlink occupies, or an entry names a file, or passes
    /// through a directory, that the filesystem resolves to a name ANOTHER member laid
    /// under different bytes (a case-insensitive or normalization-insensitive spelling of
    /// it). No honest archiver produces any of them for the filesystem it is unpacked on;
    /// each would make the on-disk tree disagree with the folded digest.
    Occupied,
    /// The filesystem stored a name the extraction laid under OTHER bytes, or laid nothing
    /// for it: HFS+ decomposes an NFC name to NFD, Win32 drops a trailing `.` or space,
    /// and a device name such as `nul` writes no file. Nothing collided (that is
    /// [`Self::Occupied`]); the one name came back changed or not at all, so the walk of
    /// the staged tree would never find the line the fold recorded. Found when the stage
    /// reads its names back (`TreeAccumulator::require_spelled_back`); the path is the
    /// first recorded name the disk does not hold under those bytes.
    Respelled,
    /// A single-file archive ([`stage_tar_gz_member`]) that is not EXACTLY ONE member
    /// named by one bare component at its root: a second member, a nested or dotted name
    /// (`dir/agy`, `./agy`, `agy/`), or — carried with an empty path — no member at all.
    NotSingleFile,
}

/// The kind of an archive entry, as the reader classifies it.
/// [`Regular`](EntryKind::Regular) and [`Directory`](EntryKind::Directory) are
/// extracted after [`vet_entry`]; a [`Hardlink`](EntryKind::Hardlink) is laid
/// down only after the stricter [`vet_hardlink_stripped`] (both ends walked, target
/// already extracted, in-root); a [`Symlink`](EntryKind::Symlink) only where the
/// lane admits them and after [`vet_symlink`]; everything else is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    Regular,
    /// A directory.
    Directory,
    /// A symbolic link. Refused by the release-bundle lane; admitted by the vendor
    /// lanes only when its target resolves inside the stage root.
    Symlink,
    /// A hard link. Allowed ONLY when [`vet_hardlink_stripped`] proves both the entry
    /// path and the link target resolve inside the store root and the target
    /// is already extracted — a toolchain sysroot dedups identical binaries
    /// this way (`cargo`/`targo`, `trustc`/`rustc`, shared dylibs), and the
    /// materialized file hashes into `tree_root` exactly like a regular file.
    Hardlink,
    /// Any other type (device, fifo, …) — refused.
    Other,
}

/// How an `https`-protocol archive is laid down. The release-bundle lane uses
/// [`ExtractOptions::default`]: nothing stripped, symlinks refused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractOptions {
    /// Leading path components dropped from every entry (GNU tar `--strip-components`):
    /// `gh_2.80.0_macOS_arm64/bin/gh` → `bin/gh` at `1`. An entry with no components
    /// left is skipped, its body drained through the same byte cap. Hardlink targets are
    /// stripped the same way. The FULL path is vetted BEFORE stripping, so `..` above the
    /// strip depth is still a refusal, never a way in.
    pub strip_components: u32,
    /// Which symlink targets may be preserved. All admitted links are leaves:
    /// later entries may never write on or through them.
    pub symlinks: SymlinkPolicy,
    /// Admit strictly parsed descriptive global PAX metadata. No global path,
    /// link, size, sparse or unknown semantics are accepted or applied.
    pub allow_global_pax: bool,
}

/// A link target is stored verbatim and hashed as bytes, never followed while
/// extracting. The destination is always vetted beneath the staging root.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SymlinkPolicy {
    #[default]
    Refuse,
    /// Relative targets must resolve lexically within the stage.
    InRoot,
    /// Preserve nonempty targets as opaque leaf links, including external ones.
    /// Only the checksum-verified Homebrew bundle selects this policy.
    ExternalLeaf,
}

/// Whether a `.tar.zst` stage demands each zstd frame's content checksum. The decoder
/// verifies the checksum of every frame that carries one either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameChecksum {
    /// A frame without one decodes: the `zstd` CLI (the packers' `zstd -19`) writes one by
    /// default, but libzstd's library encoder does not, and atpkg-publish's in-process
    /// packer is such a producer. Its rows sign a `tree_root`, which is the check there.
    IfPresent,
    /// A frame without one is refused at its header, before any of its content is laid
    /// ([`aterm_zstd::DecodeError::NoChecksum`]). For a row that signs no `tree_root`,
    /// whose decoded tree nothing else compares (`install::verify_and_stage` step 3):
    /// either the signed root or the producer's checksum catches a decoding defect
    /// (docs/AUDIT-third-party-2026-10-15.md ZC.5).
    Required,
}

/// Steps 2–4 of [`vet_entry`]: the pure component walk. Accepts ONLY `Normal` segments
/// (`.` dropped; `..` is traversal; a root/prefix component is an absolute escape) and
/// refuses a path that names nothing. Returns the validated RELATIVE path.
fn vet_components(raw: &Path) -> Result<PathBuf, ExtractReject> {
    // An absolute entry path can never be made relative to the root.
    if raw.is_absolute() {
        return Err(ExtractReject::AbsolutePath);
    }
    // Component-walk. This distinguishes a `..` *component* from a filename that merely
    // contains dots (`foo..bar` stays a Normal component, correctly allowed).
    let mut rel = PathBuf::new();
    for comp in raw.components() {
        match comp {
            Component::Normal(c) => rel.push(c),
            Component::CurDir => {}
            Component::ParentDir => return Err(ExtractReject::ParentTraversal),
            Component::RootDir | Component::Prefix(_) => return Err(ExtractReject::AbsolutePath),
        }
    }
    // A path that was empty, or only `.`/separators, names no target.
    if rel.as_os_str().is_empty() {
        return Err(ExtractReject::EmptyPath);
    }
    Ok(rel)
}

/// Drop the first `strip` components of an already-vetted relative path. `None` when
/// nothing remains — the entry is skipped, as GNU tar skips it.
fn strip_leading(rel: &Path, strip: u32) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut dropped: u32 = 0;
    for comp in rel.components() {
        if dropped < strip {
            dropped = dropped.saturating_add(1);
            continue;
        }
        out.push(comp);
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// How many leading `.` components the entry path carries AS WRITTEN (`./x` → 1,
/// `././x` → 2, `x` → 0) — the count `strip_components` has to spend on them, because
/// the tar that AUTHORS these rows spends it: measured on bsdtar 3.5.3 (libarchive
/// 3.7.4), `tar -x --strip-components 1` over an archive of `./top/bin/gh` lays
/// `top/bin/gh`, having spent the strip on the `.`. [`vet_components`] drops `.` before
/// anything can count it, so the count is taken from the raw path instead and subtracted
/// from the budget [`strip_leading`] then spends on the vetted one. Without that, a
/// `tar -C dir -c .` archive lays one component DEEPER on every client than in the tree
/// the authoring ceremony staged with the system tar and signed the `tree_root` of — a
/// row that verifies and then installs nothing.
fn leading_curdirs(raw: &Path) -> u32 {
    let n = raw
        .components()
        .take_while(|c| matches!(c, Component::CurDir))
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Step 5 of [`vet_entry`]: the join must still be under the root (defence in depth
/// against any normalization surprise).
fn join_under(root: &Path, rel: &Path) -> Result<PathBuf, ExtractReject> {
    let dest = root.join(rel);
    if !dest.starts_with(root) {
        return Err(ExtractReject::RootEscape);
    }
    Ok(dest)
}

/// Vet one archive entry against the store `root`. On success returns the safe absolute
/// destination (`root` joined with the validated relative path); otherwise a fail-closed
/// [`ExtractReject`]. `root` is the per-program staging directory the caller owns; it is
/// returned-into only for [`EntryKind::Regular`]/[`EntryKind::Directory`] entries whose
/// path is purely relative and traversal-free.
///
/// Pure and side-effect-free (no filesystem access), so the full adversarial matrix is
/// unit-testable without writing any files.
pub fn vet_entry(root: &Path, raw: &Path, kind: EntryKind) -> Result<PathBuf, ExtractReject> {
    // 1. Only real files/dirs are laid down through THIS vet. Symlinks (the
    //    classic slip + TOCTOU vector) and exotic types are refused outright;
    //    a hardlink must come through [`vet_hardlink_stripped`], which walks BOTH ends;
    //    a symlink the lane admits must come through [`vet_symlink`].
    match kind {
        EntryKind::Regular | EntryKind::Directory => {}
        EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
            return Err(ExtractReject::DisallowedKind);
        }
    }
    // 2–4. The component walk.
    let rel = vet_components(raw)?;
    // 5. Defence in depth: the join must still be under the root.
    join_under(root, &rel)
}

/// [`vet_entry`] with `strip_components`: the FULL raw path is vetted first (so `..` or
/// an absolute prefix above the strip depth is still refused), THEN the leading
/// components are dropped. `Ok(None)` when nothing remains — skip the entry. A leading
/// `.` is one of the components the strip spends itself on ([`leading_curdirs`]), as it
/// is for the tar that writes these archives.
pub fn vet_entry_stripped(
    root: &Path,
    raw: &Path,
    kind: EntryKind,
    strip: u32,
) -> Result<Option<PathBuf>, ExtractReject> {
    match kind {
        EntryKind::Regular | EntryKind::Directory => {}
        EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
            return Err(ExtractReject::DisallowedKind);
        }
    }
    let rel = vet_components(raw)?;
    let Some(rel) = strip_leading(&rel, strip.saturating_sub(leading_curdirs(raw))) else {
        return Ok(None);
    };
    join_under(root, &rel).map(Some)
}

/// Vet one HARDLINK entry: the entry path AND the link target must each pass
/// the exact [`vet_entry`] component walk (relative, no `..`, no root/prefix,
/// non-empty, joined-under-root), with `strip_components` applied to BOTH ends (GNU
/// tar strips link targets too). Returns `(dest, target)` as safe absolute paths. The
/// link target is resolved against the store `root` (tar hardlink names are
/// archive-relative), so a target can never name anything outside the tree being
/// extracted — an escaping hardlink is structurally unreachable.
///
/// `Ok(None)` when the ENTRY strips away; a target that strips away while the entry
/// does not is [`ExtractReject::HardlinkTargetMissing`] — nothing extracted can be its
/// target. Pure like [`vet_entry`]; the existence-at-link-time check (the same
/// rejection) lives at the extraction site, where the filesystem is in scope.
pub fn vet_hardlink_stripped(
    root: &Path,
    raw: &Path,
    link_target: &Path,
    strip: u32,
) -> Result<Option<(PathBuf, PathBuf)>, ExtractReject> {
    let Some(dest) = vet_entry_stripped(root, raw, EntryKind::Regular, strip)? else {
        return Ok(None);
    };
    let Some(target) = vet_entry_stripped(root, link_target, EntryKind::Regular, strip)? else {
        return Err(ExtractReject::HardlinkTargetMissing);
    };
    Ok(Some((dest, target)))
}

/// Vet one SYMLINK entry for the lanes that admit them: the entry path passes the
/// component walk (and `strip`), and the target — which is created VERBATIM, so the
/// on-disk link and the digest agree — must resolve *lexically inside the root*. A
/// relative target is resolved from the link's own directory, `..` popping one level;
/// popping above the root is [`ExtractReject::RootEscape`], an absolute target is
/// [`ExtractReject::AbsolutePath`], an empty one [`ExtractReject::EmptyPath`]. Returns
/// the link's safe absolute path; `Ok(None)` when the entry strips away.
///
/// Lexical on purpose: the resolution never consults the filesystem, so it cannot be
/// raced. That is only sound because `..` is admitted as a LEADING run and nowhere else
/// ([`ExtractReject::RootEscape`] after any normal component): a `..` that follows a
/// component pops from wherever that component points on disk, and when it is a link laid
/// earlier in the same archive the lexical walk and the disk disagree — `a/b/c/up ->
/// ../../..` and `bin -> a/b/c/up/../OUTSIDE` each vet in-root and chain out of it.
/// Leading pops start from the link's own directory, which is always a
/// real directory (the ancestors and the landing path are guarded against links); after
/// them the target only descends, through entries each vetted in-root, so no chain of
/// links can climb out whatever order they are laid in.
pub fn vet_symlink(
    root: &Path,
    raw: &Path,
    target: &Path,
    strip: u32,
) -> Result<Option<PathBuf>, ExtractReject> {
    let rel = vet_components(raw)?;
    let Some(rel) = strip_leading(&rel, strip.saturating_sub(leading_curdirs(raw))) else {
        return Ok(None);
    };
    if target.as_os_str().is_empty() {
        return Err(ExtractReject::EmptyPath);
    }
    if target.is_absolute() {
        return Err(ExtractReject::AbsolutePath);
    }
    // The link lives `depth` directories below the root; the target walks from there.
    let mut depth: usize = rel.components().count().saturating_sub(1);
    let mut descended = false;
    for comp in target.components() {
        match comp {
            Component::Normal(_) => {
                descended = true;
                depth = depth.saturating_add(1);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 || descended {
                    return Err(ExtractReject::RootEscape);
                }
                depth = depth.saturating_sub(1);
            }
            Component::RootDir | Component::Prefix(_) => return Err(ExtractReject::AbsolutePath),
        }
    }
    join_under(root, &rel).map(Some)
}

/// A failure while extracting a bundle. Any variant aborts the WHOLE stage — the caller
/// removes the (partial) `dest_root` so a half-extracted tree never activates.
#[derive(Debug)]
pub enum ExtractError {
    /// An I/O / decompression / container-format error.
    ///
    /// `op` names the step that failed and `path` the filesystem path it was on —
    /// the two things a bare errno cannot say. A 508-member toolchain bundle that
    /// dies at member 300 has to name the member and the destination, because the
    /// caller then DELETES the partial `dest_root` and nothing else survives to say
    /// how far it got; and on macOS an `EPERM` is usually privacy consent arriving
    /// with no dialog, where the whole question is *which path* needs it.
    ///
    /// The two travel together: `path: None` (with `op` `""`) is an error from a reader
    /// with no path in scope, rendered `io: <errno>`.
    Io {
        /// The underlying error.
        err: io::Error,
        /// The operation that failed (`"create_dir_all"`, `"open"`, `"write"`,
        /// `"set_mode"`, `"hard_link"`, `"symlink"`, `"read"`, `"fstat"`), or `""`.
        op: &'static str,
        /// The path the operation was attempted on, when the failing site knew it.
        path: Option<PathBuf>,
    },
    /// An entry failed [`vet_entry`] — a tar-slip escape. Carries the offending raw path.
    Rejected(ExtractReject, PathBuf),
    /// The bundle tripped one of the decompression-bomb / tar-bomb defences — see
    /// [`TooLargeReason`] for WHICH one, and on which member. The caps come from the
    /// *signed* `disk_installed`/`size` (§9), never an attacker-chosen header field.
    TooLarge(TooLargeReason),
}

/// WHICH bomb defence refused the bundle.
///
/// Seven unrelated refusals: one shared "size cap" line would send the operator to
/// chase `disk_installed` when the cause is one malformed member. Each renders as what
/// it is, and the header-shaped ones carry the raw member path.
#[derive(Debug)]
pub enum TooLargeReason {
    /// The signed uncompressed-content cap (`max_total_bytes`) was reached mid-stream.
    ContentCap,
    /// A tar's STRUCTURE — every header, the padding after each body, GNU/PAX extension
    /// headers and their bodies — exceeded its own allowance, which is the signed cap
    /// again, counted apart from content (see [`TAR_ENTRY_STRUCTURAL_BUDGET`]).
    StructureCap,
    /// The signed entry-count cap (`max_entries`) was reached.
    EntryCap,
    /// One entry's STRUCTURAL reads — its header, its content padding, or a GNU
    /// longname/longlink or PAX extension body — exceeded the per-entry structural
    /// budget (`TAR_ENTRY_STRUCTURAL_BUDGET`).
    StructuralBudget,
    /// A DIRECTORY member declared a nonzero body size — a skip-bomb the byte cap
    /// never sees. Carries the offending raw member path.
    DirectoryWithBody(PathBuf),
    /// A HARDLINK member declared a nonzero body size. Carries the raw member path.
    HardlinkWithBody(PathBuf),
    /// A SYMLINK member declared a nonzero body size. Carries the raw member path.
    SymlinkWithBody(PathBuf),
    /// A zip symlink member whose declared target exceeds the 4096-byte link-target
    /// limit. Carries the raw member path.
    LinkTargetTooLong(PathBuf),
}

impl From<io::Error> for ExtractError {
    fn from(e: io::Error) -> Self {
        ExtractError::Io {
            err: e,
            op: "",
            path: None,
        }
    }
}

/// Attach the failing operation and the path it was on to an I/O error — the context a
/// bare `?` into [`ExtractError::Io`] throws away. Every `Layer` site that touches the
/// disk goes through this, so a mid-bundle failure names the step and the member.
fn io_at(err: io::Error, op: &'static str, path: &Path) -> ExtractError {
    ExtractError::Io {
        err,
        op,
        path: Some(path.to_path_buf()),
    }
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::Io { err, op, path } => match path {
                Some(p) => write!(f, "io: {op} {p:?}: {err}"),
                None => write!(f, "io: {err}"),
            },
            ExtractError::Rejected(ExtractReject::Occupied, p) => write!(
                f,
                "rejected entry {p:?}: path already taken by an earlier entry (the entry, or \
                 a directory on its way, has an earlier entry's name or one differing only in \
                 case or Unicode form)"
            ),
            ExtractError::Rejected(ExtractReject::Respelled, p) => write!(
                f,
                "rejected entry {p:?}: the filesystem did not store the name as laid (it \
                 rewrites names, e.g. HFS+ decomposing Unicode or Windows dropping a \
                 trailing dot), so the installed tree could never match its recorded root"
            ),
            ExtractError::Rejected(ExtractReject::NotSingleFile, p) if p.as_os_str().is_empty() => {
                f.write_str("the archive holds no member; it must hold exactly one regular file")
            }
            ExtractError::Rejected(ExtractReject::NotSingleFile, p) => write!(
                f,
                "rejected entry {p:?}: the archive must hold exactly one regular file, named \
                 at its root (this is a second member or a name that is not one bare component)"
            ),
            ExtractError::Rejected(r, p) => write!(f, "rejected entry {p:?}: {r:?}"),
            ExtractError::TooLarge(r) => std::fmt::Display::fmt(r, f),
        }
    }
}

impl std::error::Error for ExtractError {}

impl std::fmt::Display for TooLargeReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TooLargeReason::ContentCap => {
                f.write_str("bundle exceeded the signed uncompressed-size cap")
            }
            TooLargeReason::StructureCap => f.write_str(
                "bundle's tar headers and extension records exceeded the signed uncompressed-size cap",
            ),
            TooLargeReason::EntryCap => f.write_str("bundle exceeded the signed entry-count cap"),
            TooLargeReason::StructuralBudget => f.write_str(
                "one entry's header/extension reads exceeded the per-entry structural budget",
            ),
            TooLargeReason::DirectoryWithBody(p) => {
                write!(f, "directory member {p:?} declares a nonzero body size")
            }
            TooLargeReason::HardlinkWithBody(p) => {
                write!(f, "hardlink member {p:?} declares a nonzero body size")
            }
            TooLargeReason::SymlinkWithBody(p) => {
                write!(f, "symlink member {p:?} declares a nonzero body size")
            }
            TooLargeReason::LinkTargetTooLong(p) => write!(
                f,
                "symlink member {p:?} declares a link target over the 4096-byte limit"
            ),
        }
    }
}

impl std::error::Error for TooLargeReason {}

/// Map a tar entry type to our [`EntryKind`]; anything that is not a plain file,
/// directory or link is treated as a disallowed kind (refused by [`vet_entry`]).
fn classify(ty: TarEntryType) -> EntryKind {
    match ty {
        TarEntryType::Regular | TarEntryType::Continuous => EntryKind::Regular,
        TarEntryType::Directory => EntryKind::Directory,
        TarEntryType::Symlink => EntryKind::Symlink,
        TarEntryType::Link => EntryKind::Hardlink,
        TarEntryType::Other => EntryKind::Other,
    }
}

/// Sanitize an archive file mode: never setuid/setgid/sticky, never group/other-writable.
/// Executables (any `x` bit set) become `0o755`, everything else `0o644`, dirs `0o755`.
/// Deterministic, so the post-extraction [`crate::tree::tree_root`] matches what the
/// publish-side producer (Phase 6) computes the same way.
///
/// THE ONE MODE RULE of a staged tree, whatever lane laid it: the archive members
/// ([`Layer::regular`], [`Layer::directory`]), the directories a folding lane makes on a
/// member's way ([`make_dir_exact`]), and the `.app` bundle the `dmg` lane's `ditto`
/// copied (`install::vet_copied_tree`, AT3 in docs/AUDIT-atpkg-inputs-2026-10-04.md),
/// so no lane lays a bit another one strips. The single-binary lanes' one file
/// (`raw-binary`, `tar-gz-binary`) is the executable class, `0o755`.
pub fn safe_mode(entry_mode: u32, is_dir: bool) -> u32 {
    if is_dir || entry_mode & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}

/// The PER-ENTRY structural byte budget: the maximum decompressed bytes the tar reader
/// may pull for a SINGLE entry's non-content parts — its header, its content padding,
/// and (crucially) any GNU longname/longlink or PAX extension-header body. Reset before
/// each entry (see [`extract_stream`]) so it never accumulates, and refunded for file
/// content in [`write_capped`], so it bounds ONLY the structural reads. 1 MiB dwarfs any
/// legitimate header/`PATH_MAX` long name / PAX record, and it is what bounds the MEMORY
/// one entry's structure can take (the reader holds an extension body whole).
///
/// It is not what bounds their TOTAL. A per-entry allowance times the entry count is the
/// same slack as one budget scaled by `max_entries`: counted per entry alone, a 12 KB
/// `.tar.zst` of PAX `comment` records decompresses 297 MiB under the 1 MiB signed cap of
/// the smallest row, and `MAX_ENTRIES = 4_000_000` such entries are ~4 TiB of
/// decompression work. Every structural byte is therefore ALSO charged to an allowance of its own
/// ([`Layer::charge_structure`]) the size of the signed cap, so the whole decompressed stream is at
/// most twice the signed cap, and an entry costs at least one 512-byte header of it.
///
/// Its own allowance, not the content's: a tree of small files carries more structure
/// than content (a 512-byte header and up to 511 bytes of padding per file, plus any PAX
/// record), and `disk_installed` signed only the content, so one shared counter refused
/// honest bundles — `/usr/share/terminfo` tarred with the packers' flags is 2.9 MB of
/// content in a 6.4 MB stream against a 5.9 MB cap. The packers sign at least half
/// the decompressed stream (`atpkg_publish::artifact::tarwrite`), so a bundle they sign
/// passes either count by construction.
const TAR_ENTRY_STRUCTURAL_BUDGET: u64 = 1 << 20;

/// Marker error the [`CappedReader`] raises when the structural budget is exhausted, so
/// extraction maps it to `TooLargeReason::StructuralBudget` (not a generic I/O error) no
/// matter WHERE the over-read happens — a directory skip, or a GNU/PAX extension-header
/// body read INSIDE the entries iterator, before per-file write-capping can see it.
#[derive(Debug)]
struct SizeCapExceeded;

impl std::fmt::Display for SizeCapExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("decompressed size cap exceeded")
    }
}

impl std::error::Error for SizeCapExceeded {}

/// Wraps the decompressor and bounds every byte the tar reader pulls against a shared,
/// per-entry-reset budget. This catches the GNU longname/longlink/PAX extension-header
/// bodies the reader pulls INSIDE its `entries()` iterator, before per-file
/// write-capping can ever see them — without it a single extension header declaring a
/// huge size (up to 2^64) with a highly-compressible body is a decompression bomb that
/// [`write_capped`]'s per-file cap never bounds. File-content reads are refunded by
/// `write_capped` (they are separately bounded by the signed content cap), so the
/// budget constrains only structural bytes and a legitimate multi-GiB file still streams.
struct CappedReader<R> {
    inner: R,
    budget: Rc<Cell<u64>>,
}

impl<R: Read> Read for CappedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.budget.get();
        if remaining == 0 {
            return Err(io::Error::other(SizeCapExceeded));
        }
        // Never offer the inner reader more room than the budget allows, so a single
        // read cannot decompress past the cap (tar's `read_to_end` grows its buffer as
        // it reads, so an uncapped `buf` would let one call overshoot by a lot).
        let cap = usize::try_from(remaining).map_or(buf.len(), |r| r.min(buf.len()));
        let n = self.inner.read(&mut buf[..cap])?;
        // The `Read` contract guarantees `n <= cap <= remaining`; saturating keeps a
        // nonconforming reader from wrapping the budget.
        self.budget.set(remaining.saturating_sub(n as u64));
        Ok(n)
    }
}

/// Translate a tar/io error into [`ExtractError`], mapping the [`CappedReader`]'s
/// [`SizeCapExceeded`] marker (wherever in the stream it fired) to `TooLarge`.
fn map_tar_io(e: io::Error) -> ExtractError {
    if e.get_ref()
        .is_some_and(|inner| inner.is::<SizeCapExceeded>())
    {
        ExtractError::TooLarge(TooLargeReason::StructuralBudget)
    } else {
        ExtractError::Io {
            err: e,
            op: "",
            path: None,
        }
    }
}

/// [`map_tar_io`] with the I/O half attributed to `op` on `path`; the cap half already
/// carries its own reason, so it passes through untouched.
fn map_tar_io_at(e: io::Error, op: &'static str, path: &Path) -> ExtractError {
    match map_tar_io(e) {
        ExtractError::Io { err, .. } => io_at(err, op, path),
        other => other,
    }
}

/// One node of the staged tree as the digest sees it: a regular file (one per inode) or
/// a symlink (one per path — links share nothing).
#[derive(Debug, Clone)]
enum Node {
    /// `(the folded permission bits — `platform::folded_mode`, masked to 0o7777;
    /// lowercase-hex content sha256)`.
    File(u32, String),
    /// Lowercase-hex sha256 of the raw target bytes ([`crate::tree::symlink_target_sha`]).
    Link(String),
}

/// The extraction-time twin of [`crate::tree::tree_root`]: the same digest, folded from
/// the bytes the extractor already has in a register, instead of re-opening and
/// re-reading the whole payload off disk afterwards.
///
/// # Why this exists
///
/// `verify_and_stage` used to make TWO full passes over the *uncompressed* payload: one
/// to write it, then one to hash it. For the dominant shipped member (`trust-5520`,
/// signed `disk_installed` = 3,439,406,710 B over 508 tar entries, several of them
/// 175–600 MB) the second pass is 3.44 GB of read I/O issued immediately after 3.44 GB
/// of dirty writeback — a genuine disk read on any machine with less free RAM than the
/// bundle, which is every machine for this bundle. The SHA-256 CPU is not saved (it is
/// the same bytes hashed once either way, ~0.26 s per 630 MB with hardware SHA); what
/// is deleted is one whole pass of I/O plus ~480 file opens.
///
/// # Why it is the SAME digest, not a similar one
///
/// The line format and the fold are not reimplemented here — [`crate::tree::entry_line`],
/// [`crate::tree::symlink_line`] and [`crate::tree::root_of_entry_lines`] are the single
/// source of all three, shared with the on-disk walk. What this type has to get right
/// is only the INPUTS per entry, and it models the filesystem the walk would have
/// observed rather than the archive stream:
///
/// * **relpath** — `dest.strip_prefix(dest_root)`, i.e. exactly what
///   [`crate::tree::tree_root`]'s walk computes for the same file, built by the same
///   `Path::join` of the same vetted (and stripped) components, and spelled into line
///   bytes by the same [`crate::tree::rel_line_bytes`] (`/`-separated on every
///   platform, see `crate::tree`'s module docs).
/// * **mode** — [`crate::platform::folded_mode`]: on Unix read BACK from the file after
///   `set_mode`, through the same `platform::permission_mode` the walk uses, so a
///   filesystem that stores something other than what we asked for moves both digests
///   together; on Windows, which stores no POSIX bits and reads back `0`, the mode the
///   extractor was asked to apply — the only value that reproduces a root a producer
///   with real bits signed. The declared modes are also kept ([`Self::declared_modes`])
///   so the on-disk walk can fold the same slot there.
/// * **content digest** — hashed chunk by chunk as [`write_capped`] writes it.
/// * **symlink target** — the verbatim bytes handed to `symlink(2)`, which are the
///   bytes `read_link` hands the walk back.
///
/// # The inode model, and why a path table alone is not enough
///
/// The walk sees the FINAL state of the tree, so anything that makes two archive entries
/// share one inode must make them share one digest here:
///
/// * a **hardlink** entry aliases its target's node (this is also strictly cheaper than
///   the walk, which re-hashes the alias through the shared inode);
/// * a **duplicate path** re-opens the SAME inode with `O_TRUNC`, so every alias of it
///   changes too — modelled by reusing the node the path already names instead of
///   pushing a new one.
///
/// Both are pathological in a bundle produced by the publisher's directory walk; they
/// are modelled anyway because "the digest the re-verify compares against" is not a
/// place to be approximately right. A path that is a SYMLINK is never re-opened: the
/// [`Layer`] refuses any entry landing on, or passing through, a link it laid
/// ([`ExtractReject::Occupied`] / [`ExtractReject::ThroughSymlink`]), which is what
/// keeps this model exact without modelling link resolution.
///
/// # Names are the archive's bytes, so the disk must spell them the same
///
/// A line's relpath is the member's own bytes; the walk's is the names the directory
/// listing hands back. They agree only while every name the extraction lays is one the
/// filesystem stores apart from every other — and "the same name" is the filesystem's
/// question, not the bytes': `bin/tool` and `BIN/TOOL`, or `Dir/f` and `dir/g`, are one
/// path on case-insensitive APFS (the macOS default) and NTFS, and so are the NFC and
/// NFD spellings of `café` on APFS. So a folding extraction lays every name — a file
/// ([`TreeAccumulator::wrote_file`]), a link, and each directory on the way to either
/// ([`make_dir_exact`], which keeps the directories it made in `dirs`) — exclusively,
/// unless it laid that exact name before, and refuses one the filesystem already holds
/// ([`ExtractReject::Occupied`]). The rule is the bytes, not a case or Unicode fold of
/// them, so it is the same answer on every filesystem.
///
/// That refuses two names the filesystem takes for one. It cannot see ONE name the
/// filesystem stores under other bytes, which collides with nothing: HFS+ decomposes an
/// NFC `café` to NFD, Win32 drops a trailing `.` or space and lays no file at all for a
/// device name such as `nul`. Staged onto HFS+, an NFC `café/f` extracted cleanly and
/// recorded a root the walk never reproduces. So the stage reads the names back once
/// before it hands out the root ([`Self::require_spelled_back`]: directory listings only,
/// no content) and refuses a tree the disk does not spell as the fold does
/// ([`ExtractReject::Respelled`]). A filesystem that keeps every name it is given (APFS,
/// ext4) never meets that refusal, and NTFS meets it only for a name the Win32 layer
/// rewrites. On any filesystem, wherever a stage succeeds, the fold is the archive's and
/// the disk spells it back.
#[derive(Debug)]
pub(crate) struct TreeAccumulator {
    /// relpath bytes → index into `nodes`: WHICH node that path currently names.
    /// A `BTreeMap` because the fold wants the lines sorted anyway and because it makes
    /// the duplicate-path lookup exact rather than best-effort.
    paths: std::collections::BTreeMap<Vec<u8>, usize>,
    /// One entry per distinct node laid down.
    nodes: Vec<Node>,
    /// The directories this extraction made, as a tree of exact names: `dirs[i]` maps the
    /// name bytes of each child directory of directory `i` to the child's own index, and
    /// `dirs[0]` is the root. A tree of components, not a set of whole relpaths, so a deep
    /// member costs its name bytes once, never once per ancestor: the memory is linear in
    /// the member names read, which the structure allowance already bounds.
    dirs: Vec<std::collections::BTreeMap<Vec<u8>, usize>>,
}

impl TreeAccumulator {
    /// An empty fold over a root that exists and is empty — the staging contract
    /// ([`require_empty_destination`]).
    pub(crate) fn new() -> Self {
        Self {
            paths: std::collections::BTreeMap::new(),
            nodes: Vec::new(),
            dirs: vec![std::collections::BTreeMap::new()],
        }
    }

    /// The index of the directory this extraction made as `name` inside directory
    /// `parent`, under exactly those bytes.
    fn child_dir(&self, parent: usize, name: &[u8]) -> Option<usize> {
        self.dirs.get(parent)?.get(name).copied()
    }

    /// Record that this extraction made directory `name` inside directory `parent`, and
    /// return its index.
    fn record_dir(&mut self, parent: usize, name: Vec<u8>) -> usize {
        let child = self.dirs.len();
        self.dirs.push(std::collections::BTreeMap::new());
        if let Some(children) = self.dirs.get_mut(parent) {
            children.insert(name, child);
        }
        child
    }

    /// Record a regular file written at `rel`. A path written twice re-uses its node —
    /// the second `open(O_TRUNC)` lands on the same inode, so every alias of it observes
    /// the new content and the new mode, which is exactly what the on-disk walk would
    /// report.
    pub(crate) fn record_file(&mut self, rel: Vec<u8>, mode: u32, content_sha_hex: String) {
        // `copied()` first so the `paths` borrow is over before `nodes` is taken
        // mutably — the two are disjoint fields, but not relying on that keeps the
        // borrow shape obvious to a reader as well as to the compiler.
        let existing = self.paths.get(&rel).copied();
        if let Some(node) = existing
            && let Some(slot) = self.nodes.get_mut(node)
            && matches!(slot, Node::File(..))
        {
            *slot = Node::File(mode, content_sha_hex);
            return;
        }
        self.nodes.push(Node::File(mode, content_sha_hex));
        // `len() - 1` is the index just pushed; spelled saturating so the index
        // arithmetic carries no panic obligation (the `push` above makes `len() >= 1`).
        let node = self.nodes.len().saturating_sub(1);
        self.paths.insert(rel, node);
    }

    /// Whether this extraction already wrote a regular file (or a hardlink to one) at
    /// exactly `rel` — the one name a later member may re-open and truncate, because the
    /// fold models it ([`Self::record_file`]). Any other name that already resolves on
    /// disk was folded onto it by the filesystem, and [`Layer::regular`] refuses it.
    fn wrote_file(&self, rel: &[u8]) -> bool {
        self.paths
            .get(rel)
            .is_some_and(|&node| matches!(self.nodes.get(node), Some(Node::File(..))))
    }

    /// Record a hardlink at `rel` aliasing the already-extracted `target_rel`.
    ///
    /// Fails CLOSED when the target is not one of the regular files this extraction
    /// wrote under exactly these bytes. [`Layer::hardlink`] asks [`Self::wrote_file`]
    /// before it links, so this never fires there; it stays a refusal rather than a
    /// guess, because guessing a digest for a file we did not write is precisely what
    /// this digest is supposed to refuse.
    fn record_alias(&mut self, rel: Vec<u8>, target_rel: &[u8]) -> bool {
        let Some(&node) = self.paths.get(target_rel) else {
            return false;
        };
        if !matches!(self.nodes.get(node), Some(Node::File(..))) {
            return false;
        }
        self.paths.insert(rel, node);
        true
    }

    /// Record a symlink at `rel` whose raw target bytes are `target`. Links share no
    /// node with anything (a link is never an alias and never re-opened), so each is
    /// its own node.
    pub(crate) fn record_symlink(&mut self, rel: Vec<u8>, target: &[u8]) {
        self.nodes
            .push(Node::Link(crate::tree::symlink_target_sha(target)));
        let node = self.nodes.len().saturating_sub(1);
        self.paths.insert(rel, node);
    }

    /// The mode each recorded REGULAR FILE folds — keyed by its line bytes, the map the
    /// on-disk walk needs on a filesystem that stores no permission bits
    /// ([`crate::tree::tree_root_declared`]). An alias reports its target's node, so two
    /// names of one inode carry one mode, exactly as the walk would read them. Links
    /// have no mode slot and are not listed.
    pub(crate) fn declared_modes(&self) -> crate::tree::DeclaredModes {
        let mut modes = crate::tree::DeclaredModes::new();
        for (rel, node) in &self.paths {
            if let Some(Node::File(mode, _)) = self.nodes.get(*node) {
                modes.insert(rel.clone(), *mode);
            }
        }
        modes
    }

    /// Refuse a staged tree the disk does not spell as this fold does: the recorded paths
    /// must be exactly the names a listing of `dest` returns ([`crate::tree::line_names`]),
    /// or the root folded here is one the walk of `dest` — the one `aterm pkg verify`
    /// takes — never reproduces (see the type's docs). A stage asks this once, after its
    /// last name is laid and before it hands out the root.
    ///
    /// # Errors
    /// [`ExtractReject::Respelled`] naming the first recorded path the listing lacks (else
    /// the first listed path the fold lacks), and the listing's I/O errors.
    pub(crate) fn require_spelled_back(&self, dest: &Path) -> Result<(), ExtractError> {
        let listed = crate::tree::line_names(dest).map_err(|e| io_at(e, "read_dir", dest))?;
        if listed.iter().eq(self.paths.keys()) {
            return Ok(());
        }
        let blamed = self
            .paths
            .keys()
            .find(|rel| listed.binary_search(rel).is_err())
            .or_else(|| listed.iter().find(|rel| !self.paths.contains_key(*rel)))
            .map_or_else(PathBuf::new, |rel| {
                PathBuf::from(String::from_utf8_lossy(rel).into_owned())
            });
        Err(ExtractError::Rejected(ExtractReject::Respelled, blamed))
    }

    /// Fold every recorded path into the tree root, through the SAME formatters and the
    /// SAME sort+SHA-256 the on-disk walk uses.
    pub(crate) fn root(self) -> String {
        let mut lines: Vec<Vec<u8>> = Vec::with_capacity(self.paths.len());
        for (rel, node) in &self.paths {
            // A node index always came from `self.nodes`, so the `get` never misses;
            // skipping a miss (rather than indexing) keeps the fold panic-free, and a
            // skipped line could only ever produce a MISMATCHING root, i.e. fail closed.
            match self.nodes.get(*node) {
                Some(Node::File(mode, sha)) => {
                    lines.push(crate::tree::entry_line(rel, *mode, sha));
                }
                Some(Node::Link(target_sha)) => {
                    lines.push(crate::tree::symlink_line(rel, target_sha));
                }
                None => {}
            }
        }
        crate::tree::root_of_entry_lines(lines)
    }
}

/// The line bytes of `path` relative to `root` — the relpath half of a `tree_root`
/// entry line, computed exactly as [`crate::tree::tree_root`]'s walk computes it, through
/// the one spelling function ([`crate::tree::rel_line_bytes`]: raw OS bytes,
/// `/`-separated on every platform).
///
/// Both ends are already inside `root` by construction ([`vet_entry`] re-checked the
/// join), so the `strip_prefix` cannot fail; it is mapped to the same fail-closed
/// `RootEscape` rejection anyway rather than unwrapped.
pub(crate) fn rel_bytes_under(root: &Path, path: &Path) -> Result<Vec<u8>, ExtractError> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| ExtractError::Rejected(ExtractReject::RootEscape, path.to_path_buf()))?;
    Ok(crate::tree::rel_line_bytes(rel))
}

/// Make directory `dir` and every missing ancestor of it below `root`, ONE component at a
/// time, for an extraction folding into `tree`; `raw` is the member blamed on refusal.
///
/// A component `tree` already holds under exactly these bytes is this extraction's own
/// directory and costs nothing. Any other is created with a plain `mkdir`, which is
/// exclusive: it fails `EEXIST` when the name already resolves, and in a root that started
/// empty everything there was laid by this extraction under OTHER bytes — `dir` after
/// `Dir` on case-insensitive APFS or NTFS, the NFD spelling of a name after its NFC one on
/// APFS, or a file or link of that name. The member would then land under the earlier
/// spelling on disk while the fold recorded its own, a line the walk never finds, so it is
/// refused as [`ExtractReject::Occupied`] (see [`TreeAccumulator`]). `create_dir_all` asks
/// "is there a directory here?", which a folding filesystem answers yes to for the wrong
/// name; this asks "did WE make this name?", which only the bytes answer.
///
/// The file and link creates make the same decision for the last component
/// ([`Layer::regular`], [`Layer::symlink`], [`Layer::hardlink`]); this is the one maker of
/// directories for every folding lane and for the links a stage adds afterwards.
///
/// Each directory it makes is given the directory mode of [`safe_mode`], `0755`, as a
/// directory MEMBER is: `mkdir` alone leaves `0777 & ~umask`, so before 2026-10-10 a
/// directory no member named (every parent of a file in an archive that lists no
/// directories) was group-writable under umask `002` and world-writable under `000`.
pub(crate) fn make_dir_exact(
    root: &Path,
    dir: &Path,
    raw: &Path,
    tree: &mut TreeAccumulator,
) -> Result<(), ExtractError> {
    let rejected = |r| ExtractError::Rejected(r, raw.to_path_buf());
    let rel = dir
        .strip_prefix(root)
        .map_err(|_| rejected(ExtractReject::RootEscape))?;
    let mut parent = 0;
    let mut cur = root.to_path_buf();
    for comp in rel.components() {
        // A vetted destination is `Normal` components only; anything else would make
        // `cur` name a path the component tree does not, so it is refused, not followed.
        let Component::Normal(name) = comp else {
            return Err(rejected(ExtractReject::RootEscape));
        };
        cur.push(name);
        let name = crate::platform::os_str_bytes(name);
        parent = match tree.child_dir(parent, name) {
            Some(made) => made,
            None => {
                std::fs::create_dir(&cur).map_err(|e| {
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        rejected(ExtractReject::Occupied)
                    } else {
                        io_at(e, "create_dir", &cur)
                    }
                })?;
                crate::platform::set_mode(&cur, safe_mode(0, true))
                    .map_err(|e| io_at(e, "set_mode", &cur))?;
                tree.record_dir(parent, name.to_vec())
            }
        };
    }
    Ok(())
}

/// THE precondition the folded digest stands on, ENFORCED rather than assumed.
///
/// The accumulator describes the entries this extraction WROTE; the on-disk walk it
/// replaces described every entry under `dest_root`. Those are the same set only while
/// `dest_root` starts empty — which is what the staging contract provides
/// (`verify_and_stage` sweeps the scratch siblings and re-creates `incoming` under the
/// store lock) and what `TreeAccumulator::record_alias` already relies on to refuse a
/// hardlink onto a file it did not write. If a stranger's file were ever present, the
/// walk would have hashed it into a MISMATCHING root (fail closed) while the fold would
/// silently ignore it (fail OPEN), which is the one direction a re-verify may never
/// move. One `read_dir` per stage buys that back.
///
/// A `dest_root` that does not exist yet is fine — extraction creates it — and is the
/// shape several in-crate probes use.
pub(crate) fn require_empty_destination(dest_root: &Path) -> Result<(), ExtractError> {
    if let Ok(mut existing) = std::fs::read_dir(dest_root)
        && let Some(entry) = existing.next()
    {
        let found = entry.map_or_else(|_| dest_root.to_path_buf(), |e| e.path());
        let msg = format!(
            "extraction destination is not empty: {}",
            found.to_string_lossy()
        );
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, msg).into());
    }
    Ok(())
}

/// One extraction in progress: the vetted, capped, digest-folding WRITER every archive
/// lane lays its entries through. The container readers (tar in [`extract_stream`], zip
/// in `zipread`) only parse and hand over `(raw path, kind, mode, body)`; everything
/// that touches the disk goes through these methods, so the slip rules, the caps, the
/// mode sanitizing, the symlink discipline and the fold are one code path for every
/// container and every compressor.
struct Layer<'a> {
    root: &'a Path,
    opts: ExtractOptions,
    /// The signed content cap, decremented as bytes are written (or drained).
    remaining: u64,
    /// A tar's STRUCTURE allowance — the signed cap again, decremented as headers,
    /// padding and extension bodies are decompressed ([`Self::charge_structure`]).
    /// Untouched by the zip lane, whose reader has no structural stream.
    structure_remaining: u64,
    max_entries: u64,
    count: u64,
    /// The per-entry structural budget of the tar [`CappedReader`], shared so
    /// [`write_capped`] can refund content bytes into it. The zip lane has no
    /// structural reader; it hands over a budget nothing decrements.
    budget: Rc<Cell<u64>>,
    /// ONE copy buffer for the whole archive. A `[0u8; 64 * 1024]` local inside
    /// `write_capped` would be zero-initialized per ENTRY and LLVM cannot elide it (the
    /// buffer goes to an opaque `Read::read`), so every unpacked file paid a 16-page
    /// stack probe plus a 64 KiB `bzero` on top of the decode and the write —
    /// gigabyte-scale memset across a real toolchain bundle. Heap `vec!`, not a boxed
    /// array literal: `Box::new([0u8; N])` materializes the array on the stack first.
    copy_buf: Vec<u8>,
    /// The tree_root of what we are writing, folded as we write it (see
    /// [`TreeAccumulator`]) — so `verify_and_stage` need not walk the payload a second
    /// time. `None` for a write-only pass.
    tree: Option<TreeAccumulator>,
    /// Whether any symlink has been laid down yet. Until one has, no entry can pass
    /// through or land on a link, so the per-entry `lstat` walks are skipped entirely —
    /// a sysroot bundle with tens of thousands of files pays nothing for a discipline
    /// it never triggers.
    laid_symlink: bool,
    /// The parent directory the last laid entry needed, already known to be a directory
    /// — so a RUN of entries in one directory asks for it once instead of once per
    /// entry. See [`Layer::ensure_parent`].
    last_parent: Option<PathBuf>,
}

impl<'a> Layer<'a> {
    fn open(
        root: &'a Path,
        max_total_bytes: u64,
        max_entries: u64,
        fold: bool,
        opts: ExtractOptions,
        budget: Rc<Cell<u64>>,
    ) -> Result<Self, ExtractError> {
        if fold {
            require_empty_destination(root)?;
            // The fold's directory tree starts AT the root ([`TreeAccumulator::new`]), so
            // the root is made here — the one directory whose own name is in no line.
            std::fs::create_dir_all(root).map_err(|e| io_at(e, "create_dir_all", root))?;
        }
        Ok(Self {
            root,
            opts,
            remaining: max_total_bytes,
            structure_remaining: max_total_bytes,
            max_entries,
            count: 0,
            budget,
            copy_buf: vec![0u8; 64 * 1024],
            tree: fold.then(TreeAccumulator::new),
            laid_symlink: false,
            last_parent: None,
        })
    }

    /// Count one more entry against the entry cap.
    fn next_entry(&mut self) -> Result<(), ExtractError> {
        self.count = self.count.saturating_add(1);
        if self.count > self.max_entries {
            return Err(ExtractError::TooLarge(TooLargeReason::EntryCap));
        }
        Ok(())
    }

    /// Charge `n` decompressed STRUCTURAL bytes — a tar header, the padding before it, a
    /// GNU/PAX extension header and its body — against the structure allowance (the signed
    /// cap, counted apart from content). The per-entry [`TAR_ENTRY_STRUCTURAL_BUDGET`]
    /// bounds what ONE entry's structure may hold in memory; this bounds what all of them
    /// may cost together, so no count of content-free entries decompresses more than the
    /// signed size admits.
    fn charge_structure(&mut self, n: u64) -> Result<(), ExtractError> {
        if n > self.structure_remaining {
            return Err(ExtractError::TooLarge(TooLargeReason::StructureCap));
        }
        self.structure_remaining = self.structure_remaining.saturating_sub(n);
        Ok(())
    }

    fn strip(&self) -> u32 {
        self.opts.strip_components
    }

    /// Every proper ancestor of `dest` below the root must be a real directory (or not
    /// exist yet), never a symlink: a write through one lands somewhere the digest does
    /// not describe. Free until the first symlink is laid.
    fn guard_ancestors(&self, dest: &Path, raw: &Path) -> Result<(), ExtractError> {
        if !self.laid_symlink {
            return Ok(());
        }
        let rel = dest
            .strip_prefix(self.root)
            .map_err(|_| ExtractError::Rejected(ExtractReject::RootEscape, raw.to_path_buf()))?;
        let mut cur = self.root.to_path_buf();
        let mut comps = rel.components().peekable();
        while let Some(c) = comps.next() {
            if comps.peek().is_none() {
                break;
            }
            cur.push(c);
            if std::fs::symlink_metadata(&cur).is_ok_and(|m| m.is_symlink()) {
                return Err(ExtractError::Rejected(
                    ExtractReject::ThroughSymlink,
                    raw.to_path_buf(),
                ));
            }
        }
        Ok(())
    }

    /// A file/directory/hardlink entry landing ON a symlink laid earlier would write
    /// through it (`open(O_TRUNC)` follows the link). Free until the first symlink.
    fn refuse_if_link(&self, dest: &Path, raw: &Path) -> Result<(), ExtractError> {
        if self.laid_symlink && std::fs::symlink_metadata(dest).is_ok_and(|m| m.is_symlink()) {
            return Err(ExtractError::Rejected(
                ExtractReject::Occupied,
                raw.to_path_buf(),
            ));
        }
        Ok(())
    }

    /// Make sure `dest`'s parent directory exists, remembering the last one so a RUN of
    /// entries in the same directory pays for it once.
    ///
    /// The make stays ([`Self::ensure_dir`]) — an archive may omit directory members
    /// entirely, and a file whose parent has no member of its own still has to land.
    /// What goes is repeating it per FILE: on a parent that already exists
    /// `create_dir_all` is not free, it is a `mkdir` that fails `EEXIST` plus an `is_dir`
    /// `stat`, each re-resolving a path as deep as
    /// `<prefix>/store/trust/<build>.incoming-<pid>/lib/rustlib/...`. The shipped
    /// `trust` sysroot lays 4114 regular files across 918 directories, so 4114 of those
    /// pairs where 918 say the same thing. Tar (and zip) members arrive grouped by
    /// directory because a directory walk wrote them, so remembering ONE path collapses
    /// the run; an archive that interleaves directories simply pays what it paid before.
    ///
    /// Skipping is sound because nothing can turn a directory this extraction already
    /// made into something else: [`Layer::symlink`] refuses any path anything occupies
    /// ([`ExtractReject::Occupied`]), and no method here removes a directory. The memo
    /// compares paths byte for byte, so it only ever elides a call for the exact name
    /// that call already answered, and would have been a no-op.
    fn ensure_parent(&mut self, dest: &Path, raw: &Path) -> Result<(), ExtractError> {
        let Some(parent) = dest.parent() else {
            return Ok(());
        };
        if self.last_parent.as_deref() == Some(parent) {
            return Ok(());
        }
        self.ensure_dir(parent, raw)?;
        self.last_parent = Some(parent.to_path_buf());
        Ok(())
    }

    /// Make directory `dir` and its missing ancestors. A folding lane makes each
    /// component under its exact name or refuses it ([`make_dir_exact`]): its root began
    /// empty, so a name already there that it did not make is a filesystem fold. The
    /// write-only lane folds nothing and may land in a populated tree, so an existing
    /// directory is simply used, as tar uses it.
    fn ensure_dir(&mut self, dir: &Path, raw: &Path) -> Result<(), ExtractError> {
        match self.tree.as_mut() {
            Some(tree) => make_dir_exact(self.root, dir, raw, tree),
            None => std::fs::create_dir_all(dir).map_err(|e| io_at(e, "create_dir_all", dir)),
        }
    }

    /// Lay down a directory entry. `declared_size` is the entry's EFFECTIVE body size
    /// — a legitimate directory always declares 0, and a non-zero one is a
    /// decompression-bomb vector the byte cap never sees (the reader skips the body
    /// without writing it), so it is refused on the header alone.
    fn directory(&mut self, raw: &Path, declared_size: u64, mode: u32) -> Result<(), ExtractError> {
        if declared_size != 0 {
            return Err(ExtractError::TooLarge(TooLargeReason::DirectoryWithBody(
                raw.to_path_buf(),
            )));
        }
        let vetted = match vet_entry_stripped(self.root, raw, EntryKind::Directory, self.strip()) {
            Ok(v) => v,
            // A DIRECTORY member that names the stage root ITSELF — `.`, `./`, `././` —
            // asks for nothing: the root is already there (`Layer::open` made it), no
            // `tree_root` line describes it, and no byte lands. So it is the same no-op
            // as a member the strip consumes, not a refusal. Every archive written as
            // `tar -C dir -c .` OPENS with exactly this member (bsdtar and GNU tar both
            // emit it), and the vendor `tar-gz`/`tar-zst` lanes do not get to choose how
            // an upstream builds its tarball — refusing it aborted the install of an
            // otherwise honest archive at its first entry. The member's mode is
            // deliberately NOT applied: the root's bits are the store's, never an
            // archive's. `EmptyPath` stays a hard refusal everywhere it names no target
            // at all — regular files, symlinks, and both ends of a hardlink.
            Err(ExtractReject::EmptyPath) => return Ok(()),
            Err(r) => return Err(ExtractError::Rejected(r, raw.to_path_buf())),
        };
        let Some(dest) = vetted else {
            return Ok(());
        };
        self.guard_ancestors(&dest, raw)?;
        self.refuse_if_link(&dest, raw)?;
        self.ensure_dir(&dest, raw)?;
        crate::platform::set_mode(&dest, safe_mode(mode, true))
            .map_err(|e| io_at(e, "set_mode", &dest))?;
        // A directory member is normally followed by its own files: record it as known
        // to exist, so the first of them does not re-ask for what we just made.
        self.last_parent = Some(dest);
        Ok(())
    }

    /// Lay down a regular file from `body`, capped and (when folding) hashed as it is
    /// written. Returns the number of content bytes consumed — written, or drained when
    /// the entry stripped away — so a container that declares sizes can cross-check.
    fn regular(&mut self, raw: &Path, mode: u32, body: impl Read) -> Result<u64, ExtractError> {
        let Some(dest) = vet_entry_stripped(self.root, raw, EntryKind::Regular, self.strip())
            .map_err(|r| ExtractError::Rejected(r, raw.to_path_buf()))?
        else {
            // Stripped away: the body still has to be consumed for the stream to
            // advance, through the SAME caps as a body that is written.
            return drain_capped(body, &mut self.remaining, &self.budget, &mut self.copy_buf);
        };
        self.guard_ancestors(&dest, raw)?;
        self.refuse_if_link(&dest, raw)?;
        self.ensure_parent(&dest, raw)?;
        // THE CREATE. A folding lane starts from an empty root, and `ensure_parent` has
        // just proved every directory on the way is one it made under exactly these
        // bytes, so a name that already resolves here is one of two things. This exact
        // path, written earlier: a duplicate member, re-opened and truncated as tar does,
        // which the fold models by reusing its node. Or a name the filesystem FOLDS onto
        // another member's file — `bin/tool` and `bin/TOOL` on case-insensitive APFS or
        // NTFS, the NFC and NFD spellings of one name on APFS: its bytes would replace
        // that member's while the fold kept both, so the root would describe a file that
        // is not on disk. That is refused, as a link landing on an occupied name is.
        // `O_EXCL` asks the filesystem's own name equality, at no cost to the create it
        // replaces.
        let rel = match self.tree {
            Some(_) => Some(rel_bytes_under(self.root, &dest)?),
            None => None,
        };
        // The write-only lane folds nothing, and keeps tar's last-wins.
        let replace = match (self.tree.as_ref(), rel.as_deref()) {
            (Some(tree), Some(rel)) => tree.wrote_file(rel),
            _ => true,
        };
        let mode = safe_mode(mode, false);
        let opened = if replace {
            crate::platform::open_create_write(&dest, mode)
        } else {
            crate::platform::open_create_new_write(&dest, mode)
        };
        let file = opened.map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                ExtractError::Rejected(ExtractReject::Occupied, raw.to_path_buf())
            } else {
                io_at(e, "open", &dest)
            }
        })?;
        let written = write_capped(
            body,
            Sink {
                file,
                dest: &dest,
                mode,
            },
            &mut self.remaining,
            &self.budget,
            &mut self.copy_buf,
            self.tree.is_some(),
        )?;
        if let (Some(tree), Some(rel)) = (self.tree.as_mut(), rel) {
            tree.record_file(rel, written.mode, written.content_sha_hex);
        }
        Ok(written.len)
    }

    /// Lay down a hardlink: both ends vetted (and stripped), the target already an
    /// extracted REGULAR FILE, the alias recorded against the target's node.
    /// `declared_size` is the entry's EFFECTIVE body size. A hardlink is a pure alias
    /// — bsdtar and GNU tar both write 0 — and a non-zero one is the same skip-bomb as
    /// a directory's or a symlink's, only worse: nothing here reads that body, so the
    /// reader discards it on its next `next_entry` bounded by the per-entry STRUCTURAL
    /// budget alone (reset to 1 MiB before every entry), and the signed
    /// `max_total_bytes` cap never sees a byte of it — slack that scales with
    /// `max_entries` all over again. Refused on the header, before any body is read.
    fn hardlink(
        &mut self,
        raw: &Path,
        target: &Path,
        declared_size: u64,
    ) -> Result<(), ExtractError> {
        if declared_size != 0 {
            return Err(ExtractError::TooLarge(TooLargeReason::HardlinkWithBody(
                raw.to_path_buf(),
            )));
        }
        let Some((dest, target_abs)) = vet_hardlink_stripped(self.root, raw, target, self.strip())
            .map_err(|r| ExtractError::Rejected(r, raw.to_path_buf()))?
        else {
            return Ok(());
        };
        self.guard_ancestors(&dest, raw)?;
        self.refuse_if_link(&dest, raw)?;
        // …and the TARGET's ancestors, for the same reason and before anything touches
        // it: `symlink_metadata` and `hard_link` decline to follow only the LAST
        // component, so a target named through a directory link this archive laid
        // earlier (`lnk/f`, `lnk -> real`) would be lstat'd as a regular file and
        // PHYSICALLY LINKED to `real/f` first, with nothing but the fold's
        // `record_alias` — which cannot find `lnk/f` among the paths it wrote — to
        // refuse it afterwards (and nothing at all in the write-only lane, which folds
        // no tree). The vet decides it, lexically, before the link exists.
        self.guard_ancestors(&target_abs, raw)?;
        // The target must already be an extracted REGULAR FILE: honest
        // archivers emit the file before its links, and linking to a
        // directory/nothing names no valid bundle. `symlink_metadata` so a
        // symlink AT the target is not followed either — a link is not a regular file,
        // so the `is_file` check below refuses it.
        let target_meta = std::fs::symlink_metadata(&target_abs).map_err(|_| {
            ExtractError::Rejected(ExtractReject::HardlinkTargetMissing, raw.to_path_buf())
        })?;
        if !target_meta.is_file() {
            return Err(ExtractError::Rejected(
                ExtractReject::HardlinkTargetMissing,
                raw.to_path_buf(),
            ));
        }
        // A folding lane also asks the FOLD, before the link exists: the target must be a
        // file this extraction wrote under exactly these bytes. The `lstat` above is
        // answered by the filesystem's own name equality, so on case-insensitive APFS a
        // target spelled `DIR/F` passes it for a written `dir/f` — a name no line carries,
        // which only the bytes can refuse.
        let rels = match self.tree.as_ref() {
            Some(tree) => {
                let target_rel = rel_bytes_under(self.root, &target_abs)?;
                if !tree.wrote_file(&target_rel) {
                    return Err(ExtractError::Rejected(
                        ExtractReject::HardlinkTargetMissing,
                        raw.to_path_buf(),
                    ));
                }
                Some((rel_bytes_under(self.root, &dest)?, target_rel))
            }
            None => None,
        };
        self.ensure_parent(&dest, raw)?;
        // A same-inode alias of already-counted bytes: no content stream, no
        // size-cap charge (a body-carrying alias was refused on the header
        // above); the entry COUNT cap still applies. `link(2)` never replaces, so a
        // name already there — this one, or one the filesystem folds it onto — is
        // occupied, as it is for a file or a symlink.
        std::fs::hard_link(&target_abs, &dest).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                ExtractError::Rejected(ExtractReject::Occupied, raw.to_path_buf())
            } else {
                io_at(e, "hard_link", &dest)
            }
        })?;
        // …and, for the digest, a same-NODE alias: it reuses the target's already
        // computed (mode, content sha) instead of re-hashing the same inode through
        // a second name, which is what the on-disk walk had to do.
        if let (Some(tree), Some((dest_rel, target_rel))) = (self.tree.as_mut(), rels)
            && !tree.record_alias(dest_rel, &target_rel)
        {
            // Unreachable: the fold answered for this very target above, and nothing
            // in between removes a file. Fail closed rather than invent a digest for
            // bytes we did not write.
            return Err(ExtractError::Rejected(
                ExtractReject::HardlinkTargetMissing,
                raw.to_path_buf(),
            ));
        }
        Ok(())
    }

    /// Lay down a symlink under the selected target policy, only onto a path
    /// nothing occupies, created with the VERBATIM
    /// target so the digest and the disk agree. `declared_size` is the entry's body
    /// size as the container declared it: a tar symlink carries no body (a non-zero
    /// declaration is the same skip-bomb as a directory's); the zip lane reads the
    /// target OUT of the body and passes 0.
    fn symlink(
        &mut self,
        raw: &Path,
        target: &Path,
        declared_size: u64,
    ) -> Result<(), ExtractError> {
        if self.opts.symlinks == SymlinkPolicy::Refuse {
            return Err(ExtractError::Rejected(
                ExtractReject::DisallowedKind,
                raw.to_path_buf(),
            ));
        }
        if declared_size != 0 {
            return Err(ExtractError::TooLarge(TooLargeReason::SymlinkWithBody(
                raw.to_path_buf(),
            )));
        }
        let destination = match self.opts.symlinks {
            SymlinkPolicy::InRoot => vet_symlink(self.root, raw, target, self.strip()),
            SymlinkPolicy::ExternalLeaf if !target.as_os_str().is_empty() => {
                vet_entry_stripped(self.root, raw, EntryKind::Regular, self.strip())
            }
            SymlinkPolicy::ExternalLeaf => Err(ExtractReject::EmptyPath),
            SymlinkPolicy::Refuse => Err(ExtractReject::DisallowedKind),
        };
        let Some(dest) = destination.map_err(|r| ExtractError::Rejected(r, raw.to_path_buf()))?
        else {
            return Ok(());
        };
        self.guard_ancestors(&dest, raw)?;
        if std::fs::symlink_metadata(&dest).is_ok() {
            return Err(ExtractError::Rejected(
                ExtractReject::Occupied,
                raw.to_path_buf(),
            ));
        }
        self.ensure_parent(&dest, raw)?;
        create_symlink(target, &dest).map_err(|e| io_at(e, "symlink", &dest))?;
        if let Some(tree) = self.tree.as_mut() {
            let rel = rel_bytes_under(self.root, &dest)?;
            let target_bytes = crate::platform::os_str_bytes(target.as_os_str());
            tree.record_symlink(rel, target_bytes);
        }
        self.laid_symlink = true;
        Ok(())
    }

    fn finish(self) -> Option<TreeAccumulator> {
        self.tree
    }
}

/// Create the symlink `link -> target` with the target bytes VERBATIM (what the digest
/// records and what `read_link` hands back). Unix only: no platform atpkg stages a
/// vendor archive on lacks it, and a platform without it fails closed here rather than
/// laying down a tree the digest could not describe.
pub(crate) fn create_symlink(target: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(not(unix))]
    {
        let _ = (target, link);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlink entries are not supported on this platform",
        ))
    }
}

/// Extract a `.tar.zst` bundle at `archive` into `dest_root`, with **every entry vetted
/// before a byte is written** ([`vet_entry`]). On the first rejected entry — or on
/// exceeding `max_total_bytes` / `max_entries` — extraction aborts with an
/// [`ExtractError`]; the caller then removes the partial `dest_root` (a half-extracted
/// bundle must never activate, §7). Symlinks/exotic entries are refused, modes are
/// sanitized (no setuid/setgid/sticky, no group/other-write; executables `0o755`, else
/// `0o644`), and the uncompressed size is capped (the cap is derived from the *signed*
/// manifest, never a header field).
///
/// `dest_root` should already exist as a hardened (`0700`, owned-by-uid) directory the
/// caller owns; entries are written strictly beneath it.
///
/// The zstd decoder is first-party (`aterm_zstd::Decoder`). It verifies the content
/// checksum of every frame that carries one, so the stream is read to its end, past the
/// tar's end-of-archive marker; a frame without a checksum decodes.
///
/// Returns `()`; the crate-private `extract_tar_zst_tree` is the same extraction folding the
/// `tree_root` of what it wrote, which is what the staging path consumes.
pub fn extract_tar_zst(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
) -> Result<(), ExtractError> {
    // `fold = false`: the historical write-only extraction, with no content hashing and
    // no per-file `fstat` — its callers want only the bytes on disk.
    extract_stream(
        zstd_decoder(archive, FrameChecksum::IfPresent)?,
        dest_root,
        max_total_bytes,
        max_entries,
        false,
        ExtractOptions::default(),
        Trailer::Drained,
    )
    .map(|_| ())
}

/// [`extract_tar_zst`], folding the [`crate::tree::tree_root`] of the tree it writes —
/// computed from the bytes as they were written (see the `TreeAccumulator` docs in this
/// module), so the staging path does not have to re-read the whole payload back off disk
/// to learn the same number. The digest is over WHAT THE EXTRACTOR WROTE; that is a real
/// semantic difference from walking the finished tree, and it is `verify_and_stage`'s to
/// argue (see the re-verify step there).
///
/// Takes [`ExtractOptions`] and hands back the OPEN fold so the
/// stage can record the `links` it creates afterwards before closing the root. The
/// `tar-zst` vendor lane and (with default options) the release-bundle lane; `checksum`
/// is the stage's [`FrameChecksum`] rule.
pub(crate) fn extract_tar_zst_tree(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    opts: ExtractOptions,
    checksum: FrameChecksum,
) -> Result<TreeAccumulator, ExtractError> {
    folded(extract_stream(
        zstd_decoder(archive, checksum)?,
        dest_root,
        max_total_bytes,
        max_entries,
        true,
        opts,
        Trailer::Drained,
    )?)
}

/// The zstd decoder over `archive` (first-party `aterm_zstd::Decoder`: a frame's content
/// checksum is verified when the frame carries one, and required under
/// [`FrameChecksum::Required`]).
fn zstd_decoder(
    archive: &Path,
    checksum: FrameChecksum,
) -> io::Result<aterm_zstd::Decoder<io::BufReader<std::fs::File>>> {
    let decoder = aterm_zstd::Decoder::new(std::fs::File::open(archive)?);
    Ok(match checksum {
        FrameChecksum::IfPresent => decoder,
        FrameChecksum::Required => decoder.require_checksum(),
    })
}

/// The `tar-gz` vendor lane: the SAME tar extraction as [`extract_tar_zst_tree`] behind
/// a gzip decoder (first-party `aterm_codec::inflate::stream`; multi-member, as `gzip -d`
/// is, and CRC/length-checked per member). Every vet, cap and fold rule is inherited by
/// construction — only the decompressor differs.
pub(crate) fn extract_tar_gz_tree(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    opts: ExtractOptions,
) -> Result<TreeAccumulator, ExtractError> {
    let file = std::fs::File::open(archive)?;
    let decoder = aterm_codec::inflate::stream::GzipReader::new(file);
    folded(extract_stream(
        decoder,
        dest_root,
        max_total_bytes,
        max_entries,
        true,
        opts,
        Trailer::Unread,
    )?)
}

/// Uncompressed tar, using the same per-entry vet, caps and writer as the
/// gzip/zstd lanes. Abrew uses this instead of handing archive paths to system tar.
pub(crate) fn extract_tar_tree(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    opts: ExtractOptions,
) -> Result<TreeAccumulator, ExtractError> {
    folded(extract_stream(
        std::fs::File::open(archive)?,
        dest_root,
        max_total_bytes,
        max_entries,
        true,
        opts,
        Trailer::Drained,
    )?)
}

/// The `zip` vendor lane: the first-party central-directory reader in `zipread`, laying
/// every member through the same [`Layer`] as the tar lanes (see that module for what
/// it supports and refuses, and why it is first-party rather than `ditto -x -k`).
pub(crate) fn extract_zip_tree(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    opts: ExtractOptions,
) -> Result<TreeAccumulator, ExtractError> {
    zipread::extract(archive, dest_root, max_total_bytes, max_entries, opts)
}

/// The fold a `fold = true` extraction always produces. Fail CLOSED on its absence
/// rather than substitute a default — an empty root would be compared against the
/// signed `tree_root` and refused anyway, but saying so is better than relying on that.
fn folded(tree: Option<TreeAccumulator>) -> Result<TreeAccumulator, ExtractError> {
    tree.ok_or_else(|| io::Error::other("extraction produced no tree_root").into())
}

/// What [`extract_stream`] does with the decoded bytes after the tar's end-of-archive
/// marker: the marker's second block, the record padding `tar` writes after it, and
/// anything else the stream holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trailer {
    /// Left unread. The `tar-gz` lane: its reader refuses trailing bytes that are not a
    /// gzip member, which a vendor archive padded past its last member would hit.
    Unread,
    /// Read to the end of the stream, under one entry's structural budget and charged to
    /// the structure allowance like any other tar structure. The zstd lanes: the decoder
    /// checks a frame's content checksum when it reads the frame's last block, which can
    /// lie past the marker (and a later frame entirely), so a tar that ends early would
    /// otherwise skip the check.
    Drained,
}

/// The one tar extraction body, over an already-decompressed `decoded` stream. `fold`
/// decides whether it also accumulates the [`crate::tree::tree_root`] of what it writes
/// (see [`extract_tar_zst_tree`]) or performs the historical write-only pass
/// ([`extract_tar_zst`]); `trailer` what happens past the tar's end.
fn extract_stream(
    decoded: impl Read,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    fold: bool,
    opts: ExtractOptions,
    trailer: Trailer,
) -> Result<Option<TreeAccumulator>, ExtractError> {
    // Bound the decompressed bytes the tar reader pulls for each entry's STRUCTURAL
    // parts (header, padding, GNU/PAX extension-header body — the last read inside
    // `entries()`'s `next()`, BEFORE write_capped can see it), so an extension-header
    // bomb cannot decompress unbounded memory. `budget` is shared with the reader; we
    // reset it per entry (structural reads never accumulate) and `write_capped` refunds
    // file-content bytes (separately bounded by the signed `max_total_bytes` cap), so a
    // legitimate large file still streams while a single entry's structural reads stay
    // under TAR_ENTRY_STRUCTURAL_BUDGET. What each entry's structure DID cost is then
    // charged to the structure allowance (`Layer::charge_structure`): the per-entry
    // budget bounds one entry, the signed cap bounds the stream's structure as a whole.
    let budget = Rc::new(Cell::new(TAR_ENTRY_STRUCTURAL_BUDGET));
    let mut layer = Layer::open(
        dest_root,
        max_total_bytes,
        max_entries,
        fold,
        opts,
        Rc::clone(&budget),
    )?;
    let mut tar = crate::tarread::Archive::new(CappedReader {
        inner: decoded,
        budget: Rc::clone(&budget),
    });
    // We drive extraction ourselves — the reader is a PARSER with no `unpack` at all
    // (crate::tarread) — so every entry is vetted before a byte reaches the disk.
    let mut entries = tar.entries().map_err(map_tar_io)?;
    loop {
        // Fresh per-entry structural allowance (does NOT roll over, so one entry's
        // structure is capped at TAR_ENTRY_STRUCTURAL_BUDGET wherever it sits); their SUM
        // is bounded by the structure allowance, charged just below.
        budget.set(TAR_ENTRY_STRUCTURAL_BUDGET);
        // `next_entry()` is where the reader resolves GNU longname/longlink/PAX
        // extension headers by reading their body through this same CappedReader — a
        // budget trip here maps to TooLarge.
        let next = entries.next_entry().map_err(map_tar_io)?;
        // Everything the reader pulled since the reset is structure — the previous
        // entry's padding, extension headers and bodies, this header — because content is
        // read only below, through `write_capped`/`drain_capped`, which refund it.
        layer.charge_structure(TAR_ENTRY_STRUCTURAL_BUDGET.saturating_sub(budget.get()))?;
        let Some(mut entry) = next else {
            break;
        };
        layer.next_entry()?;
        let raw = entry.path()?.into_owned();
        let kind = classify(entry.header().entry_type());
        let mode = entry.header().mode().unwrap_or(0o644);
        // The EFFECTIVE size — the reader's, after any PAX `size` record overrode the
        // header field. Reading `header()` here would let an `x` record declare 0
        // while the ustar field declares gigabytes (or the reverse), which is the
        // exact reader-disagreement the hardlink/directory/symlink size guards close.
        let declared = entry.entry_size();
        match kind {
            EntryKind::Hardlink => {
                let target = entry
                    .link_name()
                    .map_err(map_tar_io)?
                    .ok_or_else(|| {
                        ExtractError::Rejected(ExtractReject::DisallowedKind, raw.clone())
                    })?
                    .into_owned();
                layer.hardlink(&raw, &target, declared)?;
            }
            EntryKind::Symlink => {
                if opts.symlinks == SymlinkPolicy::Refuse {
                    return Err(ExtractError::Rejected(ExtractReject::DisallowedKind, raw));
                }
                // An absent link name is an EMPTY target, which the vet refuses as
                // `EmptyPath` — the same verdict the zip lane reaches for an empty body.
                let target = entry
                    .link_name()
                    .map_err(map_tar_io)?
                    .map_or_else(PathBuf::new, std::borrow::Cow::into_owned);
                layer.symlink(&raw, &target, declared)?;
            }
            EntryKind::Directory => layer.directory(&raw, declared, mode)?,
            EntryKind::Regular => {
                // The reader hands out content bytes through the CappedReader; the
                // per-file cap and the structural refund happen in write_capped.
                layer.regular(&raw, mode, &mut entry)?;
            }
            EntryKind::Other if opts.allow_global_pax && entry.header().is_global_pax() => {
                let _ =
                    vet_entry_stripped(dest_root, &raw, EntryKind::Regular, opts.strip_components)
                        .map_err(|reject| ExtractError::Rejected(reject, raw.clone()))?;
                let before = budget.get();
                entry.read_descriptive_global_pax().map_err(map_tar_io)?;
                layer.charge_structure(before.saturating_sub(budget.get()))?;
            }
            EntryKind::Other => {
                return Err(ExtractError::Rejected(ExtractReject::DisallowedKind, raw));
            }
        }
    }
    if trailer == Trailer::Drained {
        budget.set(TAR_ENTRY_STRUCTURAL_BUDGET);
        io::copy(&mut tar.into_inner(), &mut io::sink()).map_err(map_tar_io)?;
        layer.charge_structure(TAR_ENTRY_STRUCTURAL_BUDGET.saturating_sub(budget.get()))?;
    }
    Ok(layer.finish())
}

/// What [`write_capped`] observed about the file it just laid down — the two per-file
/// inputs a `tree_root` entry line needs beside the path, plus its length. The digest
/// fields are placeholders (`0` / `""`) when the caller asked for no fold, and are then
/// never read.
#[derive(Debug)]
pub(crate) struct WrittenFile {
    /// The permission bits the fold records for the file we just wrote, masked to
    /// `0o7777` — [`crate::platform::folded_mode`] over an `fstat` of the handle we
    /// still hold (after `set_mode`) and the requested `mode`. On Unix that is the mode
    /// READ BACK rather than the request, the same number the on-disk walk reads; on
    /// Windows, which stores no POSIX bits, it is the request itself — the only value a
    /// producer's signed root can be reproduced from (`crate::tree`'s module docs).
    pub(crate) mode: u32,
    /// Lowercase-hex SHA-256 of the content, accumulated over the SAME 64 KiB chunks the
    /// copy loop writes. Byte-for-byte the digest `tree::file_sha256` would produce by
    /// reading the file back: the same bytes, the same streaming `tree::BulkSha256`.
    pub(crate) content_sha_hex: String,
    /// Content bytes written.
    pub(crate) len: u64,
}

/// The file [`write_capped`] streams into: the handle its caller opened (how it was
/// created is the caller's decision — see [`Layer::regular`]), the path it was opened
/// at, for messages, and the permission bits to leave on it.
struct Sink<'p> {
    file: std::fs::File,
    dest: &'p Path,
    mode: u32,
}

/// Stream `reader` into `sink`'s freshly opened file with permission `sink.mode`,
/// decrementing `remaining`; abort with [`ExtractError::TooLarge`] the moment the running
/// total would exceed the cap (so a decompression bomb is stopped mid-stream, never fully
/// written).
///
/// With `fold`, also returns the file's [`WrittenFile`] digest+mode, hashed inline: the
/// bytes are already in the copy buffer, so this is the one pass over the payload the
/// staging path needs. Without it, nothing is hashed and no `fstat` is issued — the
/// historical write-only loop.
///
/// `buf` is the caller's single reusable copy buffer (see [`Layer`]).
fn write_capped(
    mut reader: impl Read,
    sink: Sink<'_>,
    remaining: &mut u64,
    budget: &Rc<Cell<u64>>,
    buf: &mut [u8],
    fold: bool,
) -> Result<WrittenFile, ExtractError> {
    let Sink {
        file: mut f,
        dest,
        mode,
    } = sink;
    // One `Option` test per 64 KiB chunk when folding is off — unmeasurable next to the
    // decompress and the write it sits between.
    let mut hasher = fold.then(crate::tree::BulkSha256::new);
    let mut len: u64 = 0;
    loop {
        // The read flows through the CappedReader (structural budget); map a budget
        // trip to TooLarge rather than a generic I/O error.
        let n = reader
            .read(&mut *buf)
            .map_err(|e| map_tar_io_at(e, "read", dest))?;
        if n == 0 {
            break;
        }
        // The `Read` contract guarantees `n <= buf.len()`; the clamp keeps a
        // nonconforming reader from panicking the slice below.
        let n = if n <= buf.len() { n } else { buf.len() };
        // Refund the file-content bytes to the structural budget: file content is
        // separately bounded by `remaining` (the signed content cap), so it must not
        // deplete the per-entry structural budget — otherwise a legitimate file larger
        // than TAR_ENTRY_STRUCTURAL_BUDGET would trip it mid-stream.
        budget.set(
            budget
                .get()
                .saturating_add(n as u64)
                .min(TAR_ENTRY_STRUCTURAL_BUDGET),
        );
        let take = n;
        let n = n as u64;
        if n > *remaining {
            return Err(ExtractError::TooLarge(TooLargeReason::ContentCap));
        }
        // Guarded by the early return just above (`n <= *remaining` here), so the
        // saturation never engages; it carries the no-underflow proof.
        *remaining = remaining.saturating_sub(n);
        len = len.saturating_add(n);
        // `take <= buf.len()` by the clamp above; `get` + full-slice fallback is a
        // no-op restatement of that bound in a panic-free shape (the gate could
        // not carry the clamp across the intervening statements).
        let chunk = match buf.get(..take) {
            Some(c) => c,
            None => &buf[..],
        };
        // Hash the EXACT slice that is written, in the same order — so the digest
        // describes the file's contents by construction, not by a second reading of it.
        if let Some(hasher) = hasher.as_mut() {
            hasher.update(chunk);
        }
        f.write_all(chunk).map_err(|e| io_at(e, "write", dest))?;
        // Live-progress meter (R5): this is the ONE in-process byte loop of an
        // install, so the extract phase's honest byte source is exactly here. One
        // relaxed atomic load per 64 KiB chunk when no `--progress-file` pass is
        // live — unmeasurable next to the decompress and the write it sits between.
        crate::progress::extract_tick(n);
    }
    // Force the sanitized mode even if umask or a pre-existing file loosened it —
    // through the handle we are still holding (`fchmod`), not by resolving `dest` a
    // second time. Same bits, same failure mapping, minus one full path walk per file:
    // the shipped `trust` sysroot lays 4114 of them, each under a
    // `<prefix>/store/trust/<build>.incoming-<pid>/lib/rustlib/...` path. It is also
    // the same property the read-back below already relies on — the mode lands on the
    // inode we wrote, with no second resolution to race.
    crate::platform::set_mode_on(&f, mode).map_err(|e| io_at(e, "set_mode", dest))?;
    // Read the mode BACK, from the handle still open on the file we just wrote (so this
    // is the inode's stored value, not our request), through the very function the
    // on-disk walk uses. `File::metadata` is an `fstat` — no path resolution, nothing
    // to race, and no second open. Where the inode stores no bits (Windows) the fold
    // takes the request instead — `platform::folded_mode` decides, per platform.
    match hasher {
        Some(hasher) => {
            let meta = f.metadata().map_err(|e| io_at(e, "fstat", dest))?;
            Ok(WrittenFile {
                mode: crate::platform::folded_mode(&meta, mode),
                content_sha_hex: hasher.finalize_hex(),
                len,
            })
        }
        // Not folding: neither digest field is read, so neither is paid for.
        None => Ok(WrittenFile {
            mode: 0,
            content_sha_hex: String::new(),
            len,
        }),
    }
}

/// Consume `reader` to EOF without writing, through the SAME caps as [`write_capped`]:
/// the bytes of an entry `strip_components` skipped still count against the signed
/// content cap (a skipped bomb is still a bomb) and still refund the structural budget.
/// Returns the bytes drained.
fn drain_capped(
    mut reader: impl Read,
    remaining: &mut u64,
    budget: &Rc<Cell<u64>>,
    buf: &mut [u8],
) -> Result<u64, ExtractError> {
    let mut len: u64 = 0;
    loop {
        let n = reader.read(&mut *buf).map_err(map_tar_io)?;
        if n == 0 {
            break;
        }
        let n = if n <= buf.len() { n } else { buf.len() };
        budget.set(
            budget
                .get()
                .saturating_add(n as u64)
                .min(TAR_ENTRY_STRUCTURAL_BUDGET),
        );
        let n = n as u64;
        if n > *remaining {
            return Err(ExtractError::TooLarge(TooLargeReason::ContentCap));
        }
        *remaining = remaining.saturating_sub(n);
        len = len.saturating_add(n);
    }
    Ok(len)
}

/// Lay ONE file down from `reader` at `dest` with permission `mode`, capped at
/// `max_total_bytes`, and hand back what was written — the `raw-binary` vendor lane's
/// whole extraction (the download IS the binary), through the very same write loop and
/// mode read-back the archive lanes use, so `bin/<entry>` folds into the digest exactly
/// as an archived `bin/<entry>` would.
pub(crate) fn stage_file(
    reader: impl Read,
    dest: &Path,
    mode: u32,
    max_total_bytes: u64,
) -> Result<WrittenFile, ExtractError> {
    // No structural reader in this lane; the budget is a refund sink nothing drains.
    let budget = Rc::new(Cell::new(TAR_ENTRY_STRUCTURAL_BUDGET));
    let mut buf = vec![0u8; 64 * 1024];
    let mut remaining = max_total_bytes;
    let file =
        crate::platform::open_create_write(dest, mode).map_err(|e| io_at(e, "open", dest))?;
    write_capped(
        reader,
        Sink { file, dest, mode },
        &mut remaining,
        &budget,
        &mut buf,
        true,
    )
}

/// The `tar-gz-binary` vendor lane's whole extraction: the gzip tar at `archive` holds
/// EXACTLY ONE member, a regular file named by one bare component at its root, and that
/// member's body is laid at `dest` with permission `mode`. The member's own name is vetted
/// and dropped (the caller names the file). The body streams through the reader, the
/// structural budget and allowance of [`extract_tar_gz_tree`] and the write loop of
/// [`stage_file`], capped at `max_total_bytes`, so the file folds exactly as a
/// `raw-binary` download of the same bytes would.
///
/// Refused before a byte of it is written: an archive with no member or a member whose
/// name is not one bare component ([`ExtractReject::NotSingleFile`]), or a directory,
/// link, device or other non-regular member ([`ExtractReject::DisallowedKind`]). A tar
/// names its members as it goes, so a SECOND member shows only after the first one's
/// body; it refuses the stage the same way, and the caller removes what was laid, as for
/// any refusal mid-archive.
pub(crate) fn stage_tar_gz_member(
    archive: &Path,
    dest: &Path,
    mode: u32,
    max_total_bytes: u64,
) -> Result<WrittenFile, ExtractError> {
    let file = std::fs::File::open(archive).map_err(|e| io_at(e, "open", archive))?;
    let budget = Rc::new(Cell::new(TAR_ENTRY_STRUCTURAL_BUDGET));
    let mut tar = crate::tarread::Archive::new(CappedReader {
        inner: aterm_codec::inflate::stream::GzipReader::new(file),
        budget: Rc::clone(&budget),
    });
    let mut entries = tar.entries().map_err(map_tar_io)?;
    let mut remaining = max_total_bytes;
    let mut structure = max_total_bytes;
    let mut buf = vec![0u8; 64 * 1024];
    let mut written = None;
    loop {
        // Each entry's structure is budgeted, and charged to the stream's allowance, as
        // `extract_stream` does it (`Layer::charge_structure`).
        budget.set(TAR_ENTRY_STRUCTURAL_BUDGET);
        let next = entries.next_entry().map_err(map_tar_io)?;
        structure = structure
            .checked_sub(TAR_ENTRY_STRUCTURAL_BUDGET.saturating_sub(budget.get()))
            .ok_or(ExtractError::TooLarge(TooLargeReason::StructureCap))?;
        let Some(mut entry) = next else {
            break;
        };
        let raw = entry.path()?.into_owned();
        if written.is_some() {
            return Err(ExtractError::Rejected(ExtractReject::NotSingleFile, raw));
        }
        if classify(entry.header().entry_type()) != EntryKind::Regular {
            return Err(ExtractError::Rejected(ExtractReject::DisallowedKind, raw));
        }
        if !raw.to_str().is_some_and(crate::vendor::bare_file_name) {
            return Err(ExtractError::Rejected(ExtractReject::NotSingleFile, raw));
        }
        let file =
            crate::platform::open_create_write(dest, mode).map_err(|e| io_at(e, "open", dest))?;
        written = Some(write_capped(
            &mut entry,
            Sink { file, dest, mode },
            &mut remaining,
            &budget,
            &mut buf,
            true,
        )?);
    }
    written.ok_or_else(|| ExtractError::Rejected(ExtractReject::NotSingleFile, PathBuf::new()))
}

/// Hand-built archive bytes shared by the extraction tests here and the staging tests
/// in `install.rs`: raw USTAR headers (so adversarial names the high-level writers
/// refuse to emit — `../`, symlinks, absolute paths — can be injected), the three
/// compressors, and a minimal ZIP writer with Unix external attributes and an optional
/// ZIP64 rendering.
#[cfg(test)]
pub(crate) mod fixtures {
    /// Build a single raw USTAR header with an explicit archive mode.
    pub(crate) fn raw_header_mode(
        name: &str,
        typeflag: u8,
        linkname: &str,
        size: usize,
        mode: u32,
    ) -> [u8; 512] {
        let mut h = [0u8; 512];
        let nb = name.as_bytes();
        let n = nb.len().min(100);
        h[..n].copy_from_slice(&nb[..n]);
        h[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        "); // checksum field = spaces while summing
        h[156] = typeflag;
        let lb = linkname.as_bytes();
        let l = lb.len().min(100);
        h[157..157 + l].copy_from_slice(&lb[..l]);
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    }

    /// Raw tar bytes (`(name, typeflag, linkname, content, mode)` per entry) plus the
    /// two trailing zero blocks.
    pub(crate) fn tar_bytes(entries: &[(&str, u8, &str, &[u8], u32)]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (name, tf, link, content, mode) in entries {
            tar.extend_from_slice(&raw_header_mode(name, *tf, link, content.len(), *mode));
            tar.extend_from_slice(content);
            let pad = (512 - content.len() % 512) % 512;
            tar.resize(tar.len() + pad, 0);
        }
        tar.resize(tar.len() + 1024, 0); // end-of-archive marker
        tar
    }

    /// One zstd frame holding `raw` in raw blocks, checksummed as `zstd` writes it
    /// (`aterm_zstd::frame::write_raw`): a valid frame for the extractor, not a small one.
    pub(crate) fn zstd_bytes(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        aterm_zstd::frame::write_raw(&mut out, raw, aterm_zstd::frame::Checksum::Xxh64).unwrap();
        out
    }

    /// A gzip member (RFC 1952) around [`deflate_bytes`].
    pub(crate) fn gzip_bytes(raw: &[u8]) -> Vec<u8> {
        let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
        out.extend_from_slice(&deflate_bytes(raw));
        out.extend_from_slice(&crc32(raw).to_le_bytes());
        out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        out
    }

    /// Raw DEFLATE (RFC 1951) in stored blocks: no encoder between the fixture
    /// and the bytes the reader sees.
    pub(crate) fn deflate_bytes(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(raw.len() + 5 * (raw.len() / 0xFFFF + 1));
        let mut rest = raw;
        loop {
            let (block, tail) = rest.split_at(rest.len().min(0xFFFF));
            out.push(u8::from(tail.is_empty()));
            let len = block.len() as u16;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(block);
            if tail.is_empty() {
                return out;
            }
            rest = tail;
        }
    }

    /// One member of a test zip. `mode` is the FULL `st_mode` (type bits included) that
    /// lands in the Unix external attributes; `0` means "no Unix attributes at all"
    /// (the version-made-by host is then FAT, as a Windows zip would carry).
    pub(crate) struct ZipMember<'a> {
        pub(crate) name: &'a str,
        pub(crate) mode: u32,
        pub(crate) data: &'a [u8],
        pub(crate) deflate: bool,
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn le16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn le32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn le64(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    /// A complete zip file. With `zip64`, every member's sizes/offset move into a ZIP64
    /// extra field and the end record is rendered as a ZIP64 EOCD + locator, exactly as
    /// a large real archive would be.
    pub(crate) fn zip_bytes(members: &[ZipMember<'_>], zip64: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for m in members {
            let body: Vec<u8> = if m.deflate {
                deflate_bytes(m.data)
            } else {
                m.data.to_vec()
            };
            let method: u16 = if m.deflate { 8 } else { 0 };
            let crc = crc32(m.data);
            let local_offset = out.len() as u64;
            // Local file header.
            le32(&mut out, 0x0403_4b50);
            le16(&mut out, 20);
            le16(&mut out, 0);
            le16(&mut out, method);
            le16(&mut out, 0);
            le16(&mut out, 0);
            le32(&mut out, crc);
            le32(&mut out, body.len() as u32);
            le32(&mut out, m.data.len() as u32);
            le16(&mut out, m.name.len() as u16);
            le16(&mut out, 0);
            out.extend_from_slice(m.name.as_bytes());
            out.extend_from_slice(&body);
            // Central directory record.
            let host: u16 = if m.mode == 0 { 0 } else { 3 };
            le32(&mut central, 0x0201_4b50);
            le16(&mut central, (host << 8) | 20);
            le16(&mut central, 20);
            le16(&mut central, 0);
            le16(&mut central, method);
            le16(&mut central, 0);
            le16(&mut central, 0);
            le32(&mut central, crc);
            let mut extra = Vec::new();
            if zip64 {
                le32(&mut central, 0xFFFF_FFFF);
                le32(&mut central, 0xFFFF_FFFF);
                le16(&mut extra, 0x0001);
                le16(&mut extra, 24);
                le64(&mut extra, m.data.len() as u64);
                le64(&mut extra, body.len() as u64);
                le64(&mut extra, local_offset);
            } else {
                le32(&mut central, body.len() as u32);
                le32(&mut central, m.data.len() as u32);
            }
            le16(&mut central, m.name.len() as u16);
            le16(&mut central, extra.len() as u16);
            le16(&mut central, 0);
            le16(&mut central, 0);
            le16(&mut central, 0);
            le32(&mut central, m.mode << 16);
            le32(
                &mut central,
                if zip64 {
                    0xFFFF_FFFF
                } else {
                    local_offset as u32
                },
            );
            central.extend_from_slice(m.name.as_bytes());
            central.extend_from_slice(&extra);
        }
        let cd_offset = out.len() as u64;
        let cd_size = central.len() as u64;
        out.extend_from_slice(&central);
        if zip64 {
            let eocd64_offset = out.len() as u64;
            le32(&mut out, 0x0606_4b50);
            le64(&mut out, 44);
            le16(&mut out, 45);
            le16(&mut out, 45);
            le32(&mut out, 0);
            le32(&mut out, 0);
            le64(&mut out, members.len() as u64);
            le64(&mut out, members.len() as u64);
            le64(&mut out, cd_size);
            le64(&mut out, cd_offset);
            le32(&mut out, 0x0706_4b50);
            le32(&mut out, 0);
            le64(&mut out, eocd64_offset);
            le32(&mut out, 1);
            le32(&mut out, 0x0605_4b50);
            le16(&mut out, 0xFFFF);
            le16(&mut out, 0xFFFF);
            le16(&mut out, 0xFFFF);
            le16(&mut out, 0xFFFF);
            le32(&mut out, 0xFFFF_FFFF);
            le32(&mut out, 0xFFFF_FFFF);
            le16(&mut out, 0);
        } else {
            le32(&mut out, 0x0605_4b50);
            le16(&mut out, 0);
            le16(&mut out, 0);
            le16(&mut out, members.len() as u16);
            le16(&mut out, members.len() as u16);
            le32(&mut out, cd_size as u32);
            le32(&mut out, cd_offset as u32);
            le16(&mut out, 0);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{
        ZipMember, gzip_bytes, raw_header_mode, tar_bytes, zip_bytes, zstd_bytes,
    };
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn root() -> PathBuf {
        PathBuf::from("/store/ay/staging")
    }

    /// The entry vet, every classic tar-slip shape (the §15 "every escape fixture aborts"
    /// matrix): safe relative paths resolve under the root (a `.` component dropped, dots
    /// within a name a plain name); `..` anywhere, an absolute path, an empty or dot-only
    /// path, and any link or exotic kind — however in-root its path — are refused.
    #[test]
    fn vet_entry_admits_in_root_paths_and_refuses_every_escape() {
        use EntryKind::{Directory as D, Hardlink, Other, Regular as F, Symlink};
        let r = root();
        for (raw, kind, at) in [
            ("bin/ay", F, "bin/ay"),
            ("share/doc", D, "share/doc"),
            ("a/./b", F, "a/b"),
            ("lib/foo..bar.so", F, "lib/foo..bar.so"),
        ] {
            assert_eq!(vet_entry(&r, Path::new(raw), kind), Ok(r.join(at)), "{raw}");
        }
        for (raw, kind, why) in [
            ("../etc/passwd", F, ExtractReject::ParentTraversal),
            ("a/../../b", F, ExtractReject::ParentTraversal),
            ("..", D, ExtractReject::ParentTraversal),
            ("/etc/passwd", F, ExtractReject::AbsolutePath),
            ("/", D, ExtractReject::AbsolutePath),
            ("", F, ExtractReject::EmptyPath),
            (".", D, ExtractReject::EmptyPath),
            ("./././", D, ExtractReject::EmptyPath),
            ("bin/ay", Symlink, ExtractReject::DisallowedKind),
            ("bin/ay", Hardlink, ExtractReject::DisallowedKind),
            ("bin/ay", Other, ExtractReject::DisallowedKind),
        ] {
            assert_eq!(
                vet_entry(&r, Path::new(raw), kind),
                Err(why),
                "{raw} {kind:?}"
            );
        }
    }

    /// `strip_components` drops leading components AFTER the full path is vetted: `..`
    /// or an absolute prefix above the strip depth is still a refusal, and an entry with
    /// nothing left is a skip (`None`), never an empty path.
    #[test]
    fn strip_components_vets_the_full_path_first() {
        let r = root();
        assert_eq!(
            vet_entry_stripped(&r, Path::new("gh_2.80.0/bin/gh"), EntryKind::Regular, 1).unwrap(),
            Some(r.join("bin/gh"))
        );
        assert_eq!(
            vet_entry_stripped(&r, Path::new("gh_2.80.0/"), EntryKind::Directory, 1).unwrap(),
            None
        );
        assert_eq!(
            vet_entry_stripped(&r, Path::new("a/b/c"), EntryKind::Regular, 3).unwrap(),
            None
        );
        // A leading `.` is a component the strip SPENDS itself on, as the authoring tar
        // spends it (`dot_rooted_archives_extract_like_the_tar_that_authored_them`); a
        // symlink member strips by the same accounting.
        assert_eq!(
            vet_symlink(&r, Path::new("./top/bin/link"), Path::new("gh"), 1).unwrap(),
            Some(r.join("top/bin/link"))
        );
        assert_eq!(
            vet_entry_stripped(&r, Path::new("../x/y"), EntryKind::Regular, 1),
            Err(ExtractReject::ParentTraversal),
            "a `..` above the strip depth is not a way in"
        );
        assert_eq!(
            vet_entry_stripped(&r, Path::new("/x/y"), EntryKind::Regular, 1),
            Err(ExtractReject::AbsolutePath)
        );
        assert_eq!(
            vet_entry_stripped(&r, Path::new("x/../y"), EntryKind::Regular, 1),
            Err(ExtractReject::ParentTraversal)
        );
        // Hardlinks strip both ends; a target that strips away is missing.
        assert_eq!(
            vet_hardlink_stripped(
                &r,
                Path::new("top/bin/cargo"),
                Path::new("top/bin/targo"),
                1
            )
            .unwrap(),
            Some((r.join("bin/cargo"), r.join("bin/targo")))
        );
        assert_eq!(
            vet_hardlink_stripped(&r, Path::new("top/bin/cargo"), Path::new("targo"), 1),
            Err(ExtractReject::HardlinkTargetMissing)
        );
        assert_eq!(
            vet_hardlink_stripped(&r, Path::new("top"), Path::new("top/bin/targo"), 1).unwrap(),
            None
        );
    }

    /// The symlink vet: in-root targets (relative from the link's own directory) pass;
    /// anything that pops above the root, a `..` after a normal component, an absolute
    /// target, or an empty one is refused; the resolution is lexical (a `..` after a `..`
    /// that reached the root escapes even if a later component would come back in).
    #[test]
    fn symlink_vet_resolves_targets_lexically_inside_the_root() {
        let r = root();
        let ok = |raw: &str, target: &str| {
            vet_symlink(&r, Path::new(raw), Path::new(target), 0)
                .unwrap_or_else(|e| panic!("{raw} -> {target}: {e:?}"))
        };
        assert_eq!(
            ok("bin/emacs", "../Emacs.app/Contents/MacOS/Emacs"),
            Some(r.join("bin/emacs"))
        );
        assert_eq!(
            ok("lib/libfoo.dylib", "libfoo.1.dylib"),
            Some(r.join("lib/libfoo.dylib"))
        );
        assert_eq!(ok("a/b/c", "../../x"), Some(r.join("a/b/c")));
        assert_eq!(ok("a/b/c", "./../d/./e"), Some(r.join("a/b/c")));
        assert_eq!(ok("top", "sub/dir"), Some(r.join("top")));
        let bad = |raw: &str, target: &str| {
            vet_symlink(&r, Path::new(raw), Path::new(target), 0).unwrap_err()
        };
        assert_eq!(bad("top", "../outside"), ExtractReject::RootEscape);
        assert_eq!(bad("a/b/c", "../../../x"), ExtractReject::RootEscape);
        assert_eq!(
            bad("a/b/c", "../../../a/b/d"),
            ExtractReject::RootEscape,
            "lexical: no coming back"
        );
        // `..` only as a leading run: after a normal component it would pop from inside
        // whatever that component is on disk — possibly a link laid earlier (K1).
        assert_eq!(
            bad("bin", "a/b/c/up/../OUTSIDE"),
            ExtractReject::RootEscape,
            "a non-leading `..` can chain through an earlier link"
        );
        assert_eq!(bad("a/b/c", "../d/../e"), ExtractReject::RootEscape);
        assert_eq!(bad("a/b/c", "./d/.."), ExtractReject::RootEscape);
        assert_eq!(bad("bin/x", "/etc/passwd"), ExtractReject::AbsolutePath);
        assert_eq!(bad("bin/x", ""), ExtractReject::EmptyPath);
        assert_eq!(bad("../x", "y"), ExtractReject::ParentTraversal);
        assert_eq!(bad("/x", "y"), ExtractReject::AbsolutePath);
        // Stripping applies to the link's own path; a stripped-away link is skipped.
        assert_eq!(
            vet_symlink(&r, Path::new("top/bin/x"), Path::new("../y"), 1).unwrap(),
            Some(r.join("bin/x"))
        );
        assert_eq!(
            vet_symlink(&r, Path::new("top"), Path::new("y"), 1).unwrap(),
            None
        );
        // …and the depth is the STRIPPED depth: `top/x -> ../y` would be in-root before
        // stripping (from `top/`), but at the root after it, so it escapes.
        assert_eq!(
            vet_symlink(&r, Path::new("top/x"), Path::new("../y"), 1),
            Err(ExtractReject::RootEscape)
        );
    }

    // === extraction driver (.tar.zst) ===

    fn dest(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-ex-{label}-"))
    }

    /// Build a single raw USTAR header — so tests can inject adversarial names (`../`,
    /// symlinks) that the high-level `tar::Builder` would refuse to *write*.
    fn raw_header(name: &str, typeflag: u8, linkname: &str, size: usize) -> [u8; 512] {
        raw_header_mode(name, typeflag, linkname, size, 0o644)
    }

    /// Assemble raw tar bytes (`(name, typeflag, linkname, content)` per entry) + the
    /// trailing two zero blocks, then zstd-compress them to `dir/<label>.tar.zst`.
    fn make_archive(dir: &Path, label: &str, entries: &[(&str, u8, &str, &[u8])]) -> PathBuf {
        let moded: Vec<_> = entries
            .iter()
            .map(|(n, tf, l, c)| (*n, *tf, *l, *c, 0o644u32))
            .collect();
        make_archive_moded(dir, label, &moded)
    }

    /// The fold `tree` produced and the on-disk walk that must agree with it, byte for
    /// byte: the plain walk where the inode stores modes (Unix — `tree_root_declared`
    /// never consults the record there), the declared-mode walk where it stores none
    /// (Windows, where the plain walk folds `0` and can agree with nothing a producer
    /// signed). ONE call shape for every parity site, so the Windows fix is exercised
    /// by the same tests that pin the Unix contract.
    fn fused_and_walked(tree: TreeAccumulator, root: &Path) -> (String, String) {
        // Every parity site is also a tree the stage's name check must pass: a refusal
        // here would be a false one, on a filesystem that kept every name.
        tree.require_spelled_back(root)
            .expect("the disk spells back every name the fold records");
        let walked = crate::tree::tree_root_declared(root, &tree.declared_modes()).unwrap();
        (tree.root(), walked)
    }

    /// [`make_archive`] with a per-entry archive mode. Same bytes for a `0o644` entry.
    #[allow(clippy::type_complexity)]
    fn make_archive_moded(
        dir: &Path,
        label: &str,
        entries: &[(&str, u8, &str, &[u8], u32)],
    ) -> PathBuf {
        let path = dir.join(format!("{label}.tar.zst"));
        std::fs::write(&path, zstd_bytes(&tar_bytes(entries))).unwrap();
        path
    }

    /// The env that turns [`probe_stages_its_directories_0755_under_an_open_umask`] on:
    /// the scratch directory it stages under.
    #[cfg(unix)]
    const OPEN_UMASK_ENV: &str = "ATPKG_EXTRACT_OPEN_UMASK_DIR";
    /// That probe's name, as `--exact` wants it.
    #[cfg(unix)]
    const OPEN_UMASK_PROBE: &str =
        "extract::tests::probe_stages_its_directories_0755_under_an_open_umask";

    /// The child half of [`a_folding_lane_makes_every_directory_0755_whatever_the_umask`]:
    /// under umask `000`, a tar.zst whose files arrive with no directory member of their
    /// own (`share/` and `share/doc/` exist only as their parents) and one that names its
    /// directory (`bin/`) stages every directory at `0755`, the archive lanes' one
    /// directory mode (`safe_mode`), not at `0777 & ~umask`. Outside that child (the env
    /// unset) it does nothing: the umask is the whole process's, so it is set only in a
    /// process running this one test.
    #[cfg(unix)]
    #[test]
    fn probe_stages_its_directories_0755_under_an_open_umask() {
        let Some(dir) = std::env::var_os(OPEN_UMASK_ENV) else {
            return;
        };
        // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail; this
        // process runs this one test (`--exact`), so no other create sees the mask.
        unsafe { libc::umask(0) };
        let d = PathBuf::from(dir);
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        let archive = make_archive_moded(
            &d,
            "implicit",
            &[
                ("share/doc/readme", b'0', "", b"hello", 0o644),
                ("bin/", b'5', "", b"", 0o777),
                ("bin/tool", b'0', "", b"#!/bin/sh\n", 0o755),
            ],
        );
        let tree = extract_tar_zst_tree(
            &archive,
            &root,
            10_000_000,
            10_000,
            ExtractOptions::default(),
            FrameChecksum::IfPresent,
        )
        .unwrap();
        let (fused, walked) = fused_and_walked(tree, &root);
        assert_eq!(fused, walked);
        for dir in ["share", "share/doc", "bin"] {
            let mode = std::fs::metadata(root.join(dir))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(mode, 0o755, "{dir}: staged under umask 000");
        }
    }

    /// AT3's check of the archive lanes (2026-10-10): the directories a folding lane
    /// makes because a member's path passes through them are made by
    /// [`make_dir_exact`], and they get the same `0755` a directory member gets, whatever
    /// umask the stage runs under; a plain `mkdir` left them at `0777 & ~umask`, so a
    /// stage under umask `002` (what Debian's `login.defs` gives a user whose group is
    /// their own) laid them group-writable and one under `000` world-writable. The umask is
    /// process-wide, so the stage runs in a child of this test binary that runs only
    /// [`probe_stages_its_directories_0755_under_an_open_umask`].
    #[cfg(unix)]
    #[test]
    fn a_folding_lane_makes_every_directory_0755_whatever_the_umask() {
        let d = dest("open-umask");
        let out = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", OPEN_UMASK_PROBE])
            .env(OPEN_UMASK_ENV, d.as_os_str())
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the probe under umask 000: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The release-bundle lane's link refusals: a symlink entry, refused outright however
    /// in-root its target, and a hardlink whose target leaves the root, is absolute, or is
    /// a forward reference (not yet extracted) — the §9 fixtures for links. None is laid.
    #[test]
    fn the_release_lane_refuses_symlinks_and_escaping_or_dangling_hardlinks() {
        let d = dest("release-links");
        let empty = b"".as_slice();
        for (label, entries, never) in [
            ("symlink", vec![("link", b'2', "bin/real", empty)], "link"),
            (
                "hl-escape",
                vec![
                    ("bin/real", b'0', "", b"x".as_slice()),
                    ("bin/evil", b'1', "../../etc/passwd", empty),
                ],
                "bin/evil",
            ),
            (
                "hl-abs",
                vec![("bin/evil", b'1', "/etc/passwd", empty)],
                "bin/evil",
            ),
            (
                "hl-forward",
                vec![("bin/evil", b'1', "bin/not-yet", empty)],
                "bin/evil",
            ),
        ] {
            let root = d.join(label);
            let archive = make_archive(&d, label, &entries);
            let err = extract_tar_zst_tree(
                &archive,
                &root,
                10_000_000,
                10_000,
                ExtractOptions::default(),
                FrameChecksum::IfPresent,
            )
            .unwrap_err();
            assert!(
                matches!(err, ExtractError::Rejected(..)),
                "{label}: {err:?}"
            );
            assert!(
                std::fs::symlink_metadata(root.join(never)).is_err(),
                "{label}: {never} was laid"
            );
        }
    }

    /// Every bomb defence of the tar lanes refuses with its OWN reason — a caller matches
    /// on it, and an operator once chased `disk_installed` for what was a directory
    /// header's size — naming the member where there is one, and lays nothing for it:
    ///
    /// * a directory, symlink or hardlink header declaring a body: the reader decompresses
    ///   that body to skip it, yet the content cap counts only bytes WRITTEN, so it is
    ///   refused on the header alone, under a cap far over the body as well as one under
    ///   it (until 2026-10-04 N hardlink bodies decompressed ~1 MiB each, charged to no
    ///   cap: ~4 TB at the production `MAX_ENTRIES`);
    /// * a GNU longname extension body, read INSIDE the tar iterator before any per-file
    ///   write cap — bounded by the structural budget, not memory;
    /// * content past the signed cap, and one entry past the entry cap.
    #[test]
    fn every_tar_bomb_defence_refuses_with_its_own_reason() {
        use TooLargeReason as Why;
        /// `(label, tar, content cap, entry cap, member never laid, the refusal)`.
        type Case<'a> = (
            &'a str,
            &'a [u8],
            u64,
            u64,
            &'a str,
            fn(&ExtractError) -> bool,
        );
        let d = dest("bombs");
        let eoa = |mut tar: Vec<u8>| {
            tar.resize(tar.len() + 1024, 0);
            tar
        };
        let dir_bomb = eoa(raw_header("payload/", b'5', "", 50 << 20).to_vec());
        let link_bomb = eoa(raw_header_mode("lnk", b'2', "target", 50 << 20, 0o777).to_vec());
        let hardlink_body = vec![0u8; 1_000_000];
        let hardlink_bomb = tar_bytes(&[
            ("bin/targo", b'0', "", b"the one binary", 0o644),
            ("bin/cargo", b'1', "bin/targo", &hardlink_body, 0o644),
        ]);
        let longname = 8usize << 20;
        let mut ext_bomb = raw_header("././@LongLink", b'L', "", longname).to_vec();
        ext_bomb.resize(ext_bomb.len() + longname, 0);
        let ext_bomb = eoa(ext_bomb);
        let big = tar_bytes(&[("data", b'0', "", &[b'x'; 4096], 0o644)]);
        let cases: [Case<'_>; 7] = [
            (
                "directory body",
                &dir_bomb,
                10_000_000,
                10_000,
                "payload",
                |e| matches!(e, ExtractError::TooLarge(Why::DirectoryWithBody(m)) if m == Path::new("payload/")),
            ),
            ("symlink body", &link_bomb, 10_000_000, 10, "lnk", |e| {
                matches!(e, ExtractError::TooLarge(Why::SymlinkWithBody(_)))
            }),
            (
                "hardlink body, small cap",
                &hardlink_bomb,
                64 * 1024,
                10_000,
                "bin/cargo",
                |e| matches!(e, ExtractError::TooLarge(Why::HardlinkWithBody(_))),
            ),
            (
                "hardlink body, large cap",
                &hardlink_bomb,
                10_000_000,
                10_000,
                "bin/cargo",
                |e| matches!(e, ExtractError::TooLarge(Why::HardlinkWithBody(_))),
            ),
            ("longname body", &ext_bomb, 64 * 1024, 16, "", |e| {
                matches!(e, ExtractError::TooLarge(Why::StructuralBudget))
            }),
            ("content", &big, 1024, 10_000, "", |e| {
                matches!(e, ExtractError::TooLarge(Why::ContentCap))
            }),
            ("entries", &big, 10_000_000, 0, "", |e| {
                matches!(e, ExtractError::TooLarge(Why::EntryCap))
            }),
        ];
        for (i, (label, tar, cap, entries, never, want)) in cases.into_iter().enumerate() {
            let archive = d.join(format!("{i}.tar.zst"));
            std::fs::write(&archive, zstd_bytes(tar)).unwrap();
            let root = d.join(format!("{i}"));
            let err = extract_tar_zst_tree(
                &archive,
                &root,
                cap,
                entries,
                vendor(0),
                FrameChecksum::IfPresent,
            )
            .unwrap_err();
            assert!(want(&err), "{label}: {err:?}");
            if !never.is_empty() {
                assert!(
                    std::fs::symlink_metadata(root.join(never)).is_err(),
                    "{label}: {never} was laid"
                );
            }
        }
    }

    /// The tar STRUCTURE counts against the signed cap, not file content alone: every byte
    /// the reader decompresses — headers, padding, extension bodies — is charged to an
    /// allowance the size of `max_total_bytes`, kept apart from content's. The per-kind
    /// body guards above refuse three spellings of a content-free entry that decompresses
    /// for free; this is the class they are spellings of. Two archives with no content
    /// byte at all, each well over a 1 MiB cap (the floor `install::size_cap` gives the
    /// smallest row):
    ///
    /// * PAX `x` headers whose one `comment` record runs to ~1 MB (read whole, then
    ///   discarded), each before an empty directory — before 2026-10-04 300 of them, a
    ///   12 KB `.tar.zst`, decompressed 297 MiB under that cap, and `MAX_ENTRIES` of them
    ///   about 4 TiB;
    /// * bare directory headers, which bound the COUNT of content-free entries by the
    ///   signed size: one 512-byte header of the cap each.
    ///
    /// Pinned by the count, not a clock: the cap admits exactly the decompressed stream
    /// (every header, and the end-of-archive marker, which a zstd lane reads to the end of
    /// the stream for the frame's checksum) and refuses one fewer.
    #[test]
    fn the_tar_structure_counts_against_the_signed_cap() {
        let d = dest("structure-cap");
        let pax_body_len = 1_040_000usize;
        let mut record = format!("{pax_body_len} comment=").into_bytes();
        record.resize(pax_body_len - 1, b'a');
        record.push(b'\n');
        let mut pax = Vec::new();
        for _ in 0..4 {
            pax.extend_from_slice(&raw_header("PaxHeader/d", b'x', "", pax_body_len));
            pax.extend_from_slice(&record);
            pax.resize(pax.len().div_ceil(512) * 512, 0);
            pax.extend_from_slice(&raw_header("d/", b'5', "", 0));
        }
        pax.resize(pax.len() + 1024, 0);
        let mut dirs = Vec::new();
        for _ in 0..3_000 {
            dirs.extend_from_slice(&raw_header("d/", b'5', "", 0));
        }
        dirs.resize(dirs.len() + 1024, 0);
        for (label, tar) in [("pax", pax), ("dirs", dirs)] {
            let archive = d.join(format!("{label}.tar.zst"));
            std::fs::write(&archive, zstd_bytes(&tar)).unwrap();
            let refused = extract_tar_zst_tree(
                &archive,
                &d.join(format!("{label}-1mib")),
                1 << 20,
                10_000,
                ExtractOptions::default(),
                FrameChecksum::IfPresent,
            );
            assert!(
                matches!(
                    refused,
                    Err(ExtractError::TooLarge(TooLargeReason::StructureCap))
                ),
                "{label}: {} decompressed bytes and not one of content passed a 1 MiB cap: \
                 {:?}",
                tar.len(),
                refused.map(|_| ())
            );
            // The zstd lanes read past the end-of-archive marker to the end of the stream.
            let pulled = tar.len() as u64;
            for (cap, admitted) in [(pulled, true), (pulled - 1, false)] {
                let got = extract_tar_zst_tree(
                    &archive,
                    &d.join(format!("{label}-{cap}")),
                    cap,
                    10_000,
                    ExtractOptions::default(),
                    FrameChecksum::IfPresent,
                );
                assert_eq!(
                    got.is_ok(),
                    admitted,
                    "{label}: cap {cap} against {pulled} pulled bytes: {:?}",
                    got.map(|_| ())
                );
            }
        }
    }

    /// Structure and content are two allowances of the signed size, never one counter: a
    /// tree of small files carries more structure than content (a 512-byte header and the
    /// padding to the next block per file), and the packers signed `disk_installed` as the
    /// content's apparent size. 16 files of 760 bytes are 12 kB of content and 13 kB of
    /// structure: against a cap of 2 x the content (`install::size_cap`'s rule) each fits
    /// and their sum does not — the shape of `/usr/share/terminfo` (2.9 MB of content,
    /// 6.4 MB stream, 5.9 MB cap), which one shared counter refused at every install. The
    /// extractor takes the cap as given; `size_cap`'s 1 MiB floor, which a real tree this
    /// small sits under, is install's, and clearing it took 700 files laid three times.
    ///
    /// Pinned by count: a cap equal to the structure admits the whole tree (its content,
    /// smaller, fits its own allowance; one counter would need the sum), and one byte less
    /// is refused as `StructureCap`.
    #[test]
    fn a_small_file_tree_spends_structure_and_content_apart() {
        let d = dest("small-files");
        let (files, size) = (16usize, 760usize);
        let mut tar = Vec::new();
        for i in 0..files {
            tar.extend_from_slice(&raw_header(&format!("share/{i:05}"), b'0', "", size));
            tar.extend(std::iter::repeat_n(b'x', size));
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
        tar.resize(tar.len() + 1024, 0);
        let content = (files * size) as u64;
        // Everything the zstd lane decompresses (the whole stream) but content.
        let structure = tar.len() as u64 - content;
        assert!(
            structure > content,
            "the fixture must carry more structure than content: {structure} vs {content}"
        );
        let archive = d.join("small.tar.zst");
        std::fs::write(&archive, zstd_bytes(&tar)).unwrap();
        let signed_cap = 2 * content;
        assert!(
            content + structure > signed_cap,
            "one shared counter must refuse what two allowances admit: {content} + \
             {structure} vs {signed_cap}"
        );
        let root = d.join("signed-cap");
        let tree = extract_tar_zst_tree(
            &archive,
            &root,
            signed_cap,
            10_000,
            ExtractOptions::default(),
            FrameChecksum::IfPresent,
        );
        assert!(
            tree.is_ok(),
            "{content} B of content and {structure} B of structure, each under the \
             {signed_cap} B cap, must install: {:?}",
            tree.map(|_| ())
        );
        assert_eq!(
            std::fs::read(root.join(format!("share/{:05}", files - 1))).unwrap(),
            vec![b'x'; size]
        );
        let at = |cap: u64| {
            extract_tar_zst_tree(
                &archive,
                &d.join(format!("cap-{cap}")),
                cap,
                10_000,
                ExtractOptions::default(),
                FrameChecksum::IfPresent,
            )
            .map(|_| ())
        };
        let exact = at(structure);
        assert!(
            exact.is_ok(),
            "cap {structure} = the structure, over the {content} B content: {exact:?}"
        );
        let under = at(structure - 1);
        assert!(
            matches!(
                under,
                Err(ExtractError::TooLarge(TooLargeReason::StructureCap))
            ),
            "cap {} is one byte under the structure: {under:?}",
            structure - 1
        );
    }

    /// The refusal a zstd lane's decoder raised, inside the extraction error.
    fn zstd_refusal(err: &ExtractError) -> Option<&aterm_zstd::DecodeError> {
        match err {
            ExtractError::Io { err, .. } => err.get_ref()?.downcast_ref(),
            _ => None,
        }
    }

    /// A zstd lane reads the stream to its end, so a frame's content checksum is checked
    /// even where the tar ends a block before the frame does. Here the end-of-archive
    /// marker's first block closes the frame's first 128 KiB block, and the last block
    /// holds only what follows it (the second marker block and `tar`'s record padding);
    /// the decoder checks the checksum when it reads that block. A damaged checksum is
    /// refused, and the undamaged frame extracts. The negative control: with the trailer
    /// left unread (the `tar-gz` lane's way) the damaged archive extracts, so the
    /// refusal is the drain's.
    #[test]
    fn a_zstd_lane_checks_a_checksum_past_the_tar_end() {
        let d = dest("checksum-past-end");
        let block = aterm_zstd::frame::BLOCK_MAX;
        let body = vec![b'x'; block - 1024];
        let mut tar = raw_header("f", b'0', "", body.len()).to_vec();
        tar.extend_from_slice(&body);
        tar.resize(block + 512, 0); // the first marker block ends at `block`, then the second
        tar.resize(tar.len().div_ceil(10_240) * 10_240, 0);
        let mut zst = Vec::new();
        aterm_zstd::frame::write_raw(&mut zst, &tar, aterm_zstd::frame::Checksum::Xxh64).unwrap();
        let good = d.join("good.tar.zst");
        std::fs::write(&good, &zst).unwrap();
        let last = zst.len() - 1;
        zst[last] ^= 1; // the checksum's last byte
        let bad = d.join("bad.tar.zst");
        std::fs::write(&bad, &zst).unwrap();

        extract_tar_zst(&good, &d.join("good"), 1 << 20, 10).unwrap();
        assert_eq!(std::fs::read(d.join("good/f")).unwrap(), body);
        for (label, result) in [
            (
                "write-only",
                extract_tar_zst(&bad, &d.join("bad"), 1 << 20, 10),
            ),
            (
                "folding",
                extract_tar_zst_tree(
                    &bad,
                    &d.join("bad-tree"),
                    1 << 20,
                    10,
                    ExtractOptions::default(),
                    FrameChecksum::IfPresent,
                )
                .map(|_| ()),
            ),
        ] {
            let err = result.unwrap_err();
            assert!(
                matches!(
                    zstd_refusal(&err),
                    Some(aterm_zstd::DecodeError::Checksum { .. })
                ),
                "{label}: {err:?}"
            );
        }
        let unread = extract_stream(
            zstd_decoder(&bad, FrameChecksum::IfPresent).unwrap(),
            &d.join("unread"),
            1 << 20,
            10,
            false,
            ExtractOptions::default(),
            Trailer::Unread,
        );
        assert!(unread.is_ok(), "{:?}", unread.map(|_| ()));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// [`FrameChecksum::Required`] refuses a frame that carries no content checksum at its
    /// header: nothing is laid. The same archive extracts under
    /// [`FrameChecksum::IfPresent`], and the checksummed frame of the same tar under
    /// `Required`.
    #[test]
    fn a_required_checksum_refuses_a_frame_without_one_before_any_entry() {
        let d = dest("checksum-required");
        let tar = tar_bytes(&[("bin/ay", b'0', "", b"#!/bin/true\n".as_slice(), 0o755)]);
        let mut bare = Vec::new();
        aterm_zstd::frame::write_raw(&mut bare, &tar, aterm_zstd::frame::Checksum::Omitted)
            .unwrap();
        let (bare_zst, checked_zst) = (d.join("bare.tar.zst"), d.join("checked.tar.zst"));
        std::fs::write(&bare_zst, bare).unwrap();
        std::fs::write(&checked_zst, zstd_bytes(&tar)).unwrap();
        let at = |archive: &Path, label: &str, checksum: FrameChecksum| {
            let root = d.join(label);
            let got = extract_tar_zst_tree(
                archive,
                &root,
                1 << 20,
                10,
                ExtractOptions::default(),
                checksum,
            )
            .map(|_| ());
            (root, got)
        };

        let (root, refused) = at(&bare_zst, "refused", FrameChecksum::Required);
        let err = refused.unwrap_err();
        assert_eq!(
            zstd_refusal(&err),
            Some(&aterm_zstd::DecodeError::NoChecksum),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read_dir(&root).map_or(0, Iterator::count),
            0,
            "nothing is laid before the refusal"
        );
        for (archive, label, checksum) in [
            (&bare_zst, "if-present", FrameChecksum::IfPresent),
            (&checked_zst, "checked", FrameChecksum::Required),
        ] {
            let (root, got) = at(archive, label, checksum);
            assert!(got.is_ok(), "{label}: {got:?}");
            assert_eq!(
                std::fs::read(root.join("bin/ay")).unwrap(),
                b"#!/bin/true\n",
                "{label}"
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// THE parity gate for the fused digest (§8 cross-version byte contract).
    ///
    /// `verify_and_stage` compares the extraction's own root against the SIGNED
    /// `tree_root`, and those signed values were computed — by the publisher, and by
    /// every earlier client that re-verified — with `tree::tree_root` over a finished
    /// directory. So "the fused root is a good hash of the tree" is not the property
    /// that matters: it must be the SAME 64 hex characters the on-disk walk produces,
    /// for every shape the extractor can lay down. One archive covers them all:
    ///
    /// * nested directories and an explicit directory entry (dirs contribute no line),
    /// * an EMPTY file (a real entry, digest of nothing),
    /// * both mode classes — `safe_mode` maps any `x` bit to `0o755`, else `0o644`,
    /// * a file spanning many 64 KiB copy-buffer chunks (the streaming hash),
    /// * a HARDLINK, which the walk hashes through a second name and the accumulator
    ///   aliases to one node,
    /// * a DUPLICATE path (the second `open(O_TRUNC)` lands on the same inode),
    /// * a duplicate that overwrites a hardlink TARGET, so both names change content —
    ///   the case a path→digest table without the inode indirection would get wrong.
    ///
    /// Fails if the fused root ever drifts from the walk in either direction.
    #[test]
    fn fused_tree_root_is_byte_identical_to_the_on_disk_walk() {
        let d = dest("fused-parity");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        // Multi-chunk content: > 2 × the 64 KiB copy buffer, so the streaming hash is
        // exercised across reads rather than in one shot.
        let big: Vec<u8> = (0..200_000usize).map(|i| (i % 251) as u8).collect();
        let archive = make_archive_moded(
            &d,
            "parity",
            &[
                ("bin/", b'5', "", b"", 0o755),
                ("bin/targo", b'0', "", b"the one binary", 0o755),
                ("bin/cargo", b'1', "bin/targo", b"", 0o644),
                ("share/doc/readme", b'0', "", b"hello", 0o644),
                ("share/empty", b'0', "", b"", 0o644),
                ("lib/big.so", b'0', "", big.as_slice(), 0o644),
                // Duplicate path: last write wins on the same inode.
                ("share/doc/readme", b'0', "", b"hello, again", 0o644),
                // Duplicate that overwrites a HARDLINK TARGET: `bin/cargo` aliases this
                // inode, so the walk sees BOTH names carrying the new bytes.
                ("bin/targo", b'0', "", b"the replaced binary", 0o755),
            ],
        );
        let tree = extract_tar_zst_tree(
            &archive,
            &root,
            10_000_000,
            10_000,
            ExtractOptions::default(),
            FrameChecksum::IfPresent,
        )
        .unwrap();
        let (fused, walked) = fused_and_walked(tree, &root);
        assert_eq!(
            fused, walked,
            "the extraction-time root must equal the on-disk walk byte for byte"
        );
        assert_eq!(fused.len(), 64);
        // Two-sided reach guard: the fixture must actually have exercised the shapes.
        // Without this the assertion above passes vacuously if the corpus ever shrinks
        // to "one plain file" (or, worse, to nothing).
        assert_eq!(
            std::fs::read(root.join("bin/cargo")).unwrap(),
            b"the replaced binary",
            "the alias must carry the target's FINAL bytes"
        );
        assert_eq!(
            std::fs::read(root.join("share/doc/readme")).unwrap(),
            b"hello, again"
        );
        assert_eq!(
            std::fs::read(root.join("lib/big.so")).unwrap().len(),
            200_000
        );
        assert!(std::fs::read(root.join("share/empty")).unwrap().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(root.join("bin/cargo")).unwrap().ino(),
                std::fs::metadata(root.join("bin/targo")).unwrap().ino(),
                "the hardlink shape must be present for the parity to mean anything"
            );
            let mode_of = |p: &str| {
                std::fs::metadata(root.join(p))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777
            };
            assert_eq!(mode_of("bin/targo"), 0o755, "the exec mode class");
            assert_eq!(
                mode_of("share/doc/readme"),
                0o644,
                "the non-exec mode class"
            );
        }
    }

    /// Two members whose names differ in bytes but name ONE file on the filesystem being
    /// unpacked onto — a case pair on case-insensitive APFS or NTFS, the NFC and NFD
    /// spellings of `café` on APFS. Before 2026-10-04 the second silently replaced the
    /// first while the fold recorded both, so the root described a `bin/tool` holding
    /// `benign` over a disk whose `bin/tool` held the other member's bytes, and a signed
    /// root folded the same way passed it. Now the second is refused (`O_EXCL`); where
    /// the filesystem keeps both names (case- and normalization-sensitive), both are laid
    /// and the fold equals the walk. Either way the fold never describes a file the disk
    /// does not hold — through the tar lane and the zip lane, which share the writer.
    #[test]
    fn a_member_the_filesystem_names_like_another_never_replaces_it_unseen() {
        let d = dest("fs-folded-names");
        // `bin/TOOL`, not `BIN/TOOL`: the pair shares its directory under one spelling, so
        // what refuses it is the FILE's exclusive create; a folded directory is the test
        // below's.
        for (first, second) in [("bin/tool", "bin/TOOL"), ("caf\u{e9}", "cafe\u{301}")] {
            let entries = [
                (first, b'0', "", b"benign".as_slice(), 0o755),
                (second, b'0', "", b"other!".as_slice(), 0o755),
            ];
            let label = first.len().to_string();
            for (lane, got, root) in in_both_lanes(&d, &label, &entries, vendor(0)) {
                match got {
                    Ok(tree) => {
                        let (fused, walked) = fused_and_walked(tree, &root);
                        assert_eq!(fused, walked, "{lane}: the fold must describe the disk");
                    }
                    Err(ExtractError::Rejected(ExtractReject::Occupied, p)) => {
                        assert_eq!(p, Path::new(second), "{lane}: the second member");
                        assert_eq!(
                            std::fs::read(root.join(first)).unwrap(),
                            b"benign",
                            "{lane}: the first member is left as it was"
                        );
                    }
                    Err(e) => panic!("{lane}: {e:?}"),
                }
            }
        }
    }

    /// The DIRECTORY half of the test above. Two members whose directory components
    /// differ in bytes but name ONE directory on the filesystem being unpacked onto —
    /// `Dir` and `dir` on case-insensitive APFS or NTFS, the NFC and NFD spellings of
    /// `café` on APFS, at the top or below a shared parent. Made with `create_dir_all`,
    /// the second member's directory silently WAS the first's: its file, link or alias
    /// landed in `Dir/` on disk while the fold recorded it under `dir/`, so the root
    /// described a path the walk never finds and `aterm pkg verify` called an untampered
    /// install drifted. A component this extraction did not make under those exact bytes
    /// is created exclusively and refused as `Occupied` when the filesystem already has
    /// it; where the filesystem keeps both spellings apart (case- and normalization-
    /// sensitive), both are laid and the fold equals the walk. The same two members with
    /// the directory spelled ONE way always extract, so the refusal is the spelling's,
    /// not the member kind's. Every kind (a link only where the platform lays links),
    /// through the tar lane and the zip lane (which has no hardlinks), which share the
    /// writer.
    #[test]
    fn a_directory_the_filesystem_names_like_another_never_merges_unseen() {
        let d = dest("fs-folded-dirs");
        let spellings = [
            ("Dir", "dir"),
            ("caf\u{e9}", "cafe\u{301}"),
            ("a/Sub", "a/sub"),
        ];
        for (pair, (first_dir, folded_dir)) in spellings.into_iter().enumerate() {
            let first = format!("{first_dir}/f");
            // (kind, tar typeflag, leaf under the second spelling, link name)
            let kinds = [
                ("file", b'0', "g", ""),
                ("dir", b'5', "sub/", ""),
                ("symlink", b'2', "l", "f"),
                ("hardlink", b'1', "h", first.as_str()),
            ];
            // `create_symlink` lays links on Unix only; elsewhere the lane refuses them.
            let kinds = kinds
                .into_iter()
                .filter(|(kind, ..)| cfg!(unix) || *kind != "symlink");
            for (kind, flag, leaf, link) in kinds {
                for (spelled, second_dir) in [("exact", first_dir), ("folded", folded_dir)] {
                    let second = format!("{second_dir}/{leaf}");
                    let body: &[u8] = if flag == b'0' { b"other!" } else { b"" };
                    let entries = [
                        (first.as_str(), b'0', "", b"benign".as_slice(), 0o644),
                        (second.as_str(), flag, link, body, 0o644),
                    ];
                    let label = format!("{pair}-{kind}-{spelled}");
                    for (lane, got, root) in in_both_lanes(&d, &label, &entries, vendor(0)) {
                        match got {
                            Ok(tree) => {
                                let (fused, walked) = fused_and_walked(tree, &root);
                                assert_eq!(
                                    fused, walked,
                                    "{lane}: the fold must describe the disk"
                                );
                            }
                            Err(ExtractError::Rejected(ExtractReject::Occupied, p))
                                if spelled == "folded" =>
                            {
                                assert_eq!(p, Path::new(&second), "{lane}: the second");
                                assert_eq!(
                                    std::fs::read(root.join(&first)).unwrap(),
                                    b"benign",
                                    "{lane}: the first member is left as it was"
                                );
                            }
                            Err(e) => panic!("{lane}: {e:?}"),
                        }
                    }
                }
            }
        }
    }

    /// The case the two tests above cannot see: ONE name the filesystem stores under
    /// other bytes, or lays nothing for, collides with nothing. Staged onto HFS+, an
    /// archive holding only an NFC `café/f` extracted cleanly, the disk held the NFD
    /// spelling, and the root the stage recorded was one the walk never reproduces, so
    /// `aterm pkg verify` called the untampered install drifted (Win32 dropping a
    /// trailing dot, and a device name such as `nul` that lays no file, are the same
    /// class). The stage reads its names back and refuses the tree as `Respelled`.
    ///
    /// APFS and ext4 keep both spellings as given, so the filesystem's part is played
    /// by a rename to exactly the bytes HFS+ stores, and by a removal for the name that
    /// lays nothing; as laid, the same tree passes. Through the tar and the zip lane.
    #[test]
    fn a_name_the_filesystem_stores_under_other_bytes_is_refused() {
        let d = dest("fs-respelled");
        let entries = [
            ("caf\u{e9}/f", b'0', "", b"one".as_slice(), 0o644),
            ("bin/tool", b'0', "", b"two".as_slice(), 0o755),
        ];
        type Respell = fn(&Path);
        let respellings: [(&str, Respell, &str); 2] = [
            (
                "nfd",
                |root| {
                    std::fs::rename(root.join("caf\u{e9}"), root.join("cafe\u{301}")).unwrap();
                },
                "caf\u{e9}/f",
            ),
            (
                "vanished",
                |root| std::fs::remove_file(root.join("bin/tool")).unwrap(),
                "bin/tool",
            ),
        ];
        for (how, respell, blamed) in respellings {
            for (lane, got, root) in in_both_lanes(&d, how, &entries, vendor(0)) {
                let tree = got.unwrap_or_else(|e| panic!("{lane}: {e:?}"));
                tree.require_spelled_back(&root)
                    .unwrap_or_else(|e| panic!("{lane}: as laid, the names pass: {e:?}"));
                respell(&root);
                match tree.require_spelled_back(&root) {
                    Err(ExtractError::Rejected(ExtractReject::Respelled, p)) => {
                        assert_eq!(p, Path::new(blamed), "{lane}: the recorded name");
                    }
                    other => panic!("{lane}: must be refused, got {other:?}"),
                }
            }
        }
    }

    /// The other side of the parity gate: the fused root must MOVE for exactly the
    /// mutations the on-disk walk moves for. A digest that agreed with the walk but
    /// ignored content or mode would pass the test above and detect nothing.
    #[test]
    fn the_fused_root_moves_with_content_and_mode_exactly_as_the_walk_does() {
        /// Extract a one-file bundle and return the (walk-agreeing) fused root.
        fn one_file(d: &Path, label: &str, payload: &[u8], mode: u32) -> String {
            let root = d.join(label);
            std::fs::create_dir_all(&root).unwrap();
            let archive = make_archive_moded(d, label, &[("bin/ay", b'0', "", payload, mode)]);
            let tree = extract_tar_zst_tree(
                &archive,
                &root,
                10_000_000,
                10_000,
                ExtractOptions::default(),
                FrameChecksum::IfPresent,
            )
            .unwrap();
            let (fused, walked) = fused_and_walked(tree, &root);
            assert_eq!(fused, walked);
            fused
        }
        let d = dest("fused-sensitive");
        let base = one_file(&d, "a", b"payload", 0o644);
        let other_content = one_file(&d, "b", b"payload!", 0o644);
        let other_mode = one_file(&d, "c", b"payload", 0o755);
        assert_eq!(other_mode.len(), 64);
        assert_ne!(base, other_content, "content must move the fused root");
        // On Unix the mode is read back from the inode; on Windows, which stores no
        // permission bits, the fold records the DECLARED mode (`0755` here against
        // `0644`) — so the root moves with the archive's mode class on every platform,
        // exactly as the producer's did when it signed the row (until 2026-09-22 the
        // Windows fold was a constant `0`, and no signed root could ever match it).
        assert_ne!(base, other_mode, "mode must move the fused root");
    }

    /// The fold's precondition, enforced: a destination that already holds something is
    /// REFUSED rather than folded.
    ///
    /// This is the one direction the fused digest could have moved that a re-verify may
    /// never move. The on-disk walk hashed every regular file under the root, so a
    /// stranger's file produced a MISMATCHING root and the stage failed closed; a fold
    /// over "what the extractor wrote" would not have seen it at all and would have
    /// passed. The staging contract already guarantees an empty scratch dir — this makes
    /// the guarantee load-bearing instead of merely documented.
    #[test]
    fn a_non_empty_destination_is_refused_rather_than_folded() {
        let d = dest("fused-nonempty");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        let archive = make_archive(&d, "occupied", &[("bin/ay", b'0', "", b"payload")]);
        let fold = |dest: &Path| {
            extract_tar_zst_tree(
                &archive,
                dest,
                10_000_000,
                10_000,
                ExtractOptions::default(),
                FrameChecksum::IfPresent,
            )
        };
        // Empty, or not there yet: extraction proceeds.
        fold(&root).unwrap();
        assert!(fold(&d.join("absent")).is_ok());
        // The SAME call over the now-occupied root is refused, so a second extraction
        // can never fold a root that omits what is already there…
        let err = fold(&root).unwrap_err();
        assert!(
            matches!(&err, ExtractError::Io { err, .. } if err.kind() == io::ErrorKind::AlreadyExists),
            "got {err:?}"
        );
        // …and a stranger's file alone is refused just the same: the case the walk used
        // to catch and the fold would not have.
        let planted = d.join("planted");
        std::fs::create_dir_all(&planted).unwrap();
        std::fs::write(planted.join("not-ours"), b"planted").unwrap();
        assert!(fold(&planted).is_err());
    }

    // === the vendor lanes: gzip, zip, strip_components, in-root symlinks ===

    /// The vendor lane options every test below uses: strip one leading component and
    /// admit in-root symlinks — the `gh` archive shape.
    fn vendor(strip: u32) -> ExtractOptions {
        ExtractOptions {
            strip_components: strip,
            symlinks: SymlinkPolicy::InRoot,
            allow_global_pax: false,
        }
    }

    #[cfg(unix)]
    #[test]
    fn external_leaf_links_preserve_targets_without_redirecting_extraction() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let outside = scratch.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("owner"), b"keep").unwrap();
        let target = outside.to_str().unwrap();
        let opts = ExtractOptions {
            symlinks: SymlinkPolicy::ExternalLeaf,
            ..ExtractOptions::default()
        };
        let extract = |entries: &[(&str, u8, &str, &[u8], u32)], label: &str, options| {
            let archive = scratch.path().join(format!("{label}.gz"));
            std::fs::write(&archive, gzip_bytes(&tar_bytes(entries))).unwrap();
            extract_tar_gz_tree(&archive, &scratch.path().join(label), 1 << 20, 20, options)
        };
        let leaf = ("leaf", b'2', target, b"".as_slice(), 0o755);
        for (label, options, reason) in [
            (
                "refused",
                ExtractOptions::default(),
                ExtractReject::DisallowedKind,
            ),
            ("in-root", vendor(0), ExtractReject::AbsolutePath),
        ] {
            assert!(matches!(extract(&[leaf], label, options),
                Err(ExtractError::Rejected(got, _)) if got == reason));
        }
        let tree = extract(&[leaf], "external", opts).unwrap();
        let root = scratch.path().join("external");
        assert_eq!(std::fs::read_link(root.join("leaf")).unwrap(), outside);
        let (fused, walked) = fused_and_walked(tree, &root);
        assert_eq!(fused, walked);
        let relative = ("leaf", b'2', "../outside", b"".as_slice(), 0o755);
        extract(&[relative], "relative", opts).unwrap();
        assert_eq!(
            std::fs::read_link(scratch.path().join("relative/leaf")).unwrap(),
            Path::new("../outside")
        );

        for (label, next, reason) in [
            (
                "on",
                ("leaf", b'0', "", b"overwrite".as_slice(), 0o644),
                ExtractReject::Occupied,
            ),
            (
                "through",
                ("leaf/owner", b'0', "", b"overwrite".as_slice(), 0o644),
                ExtractReject::ThroughSymlink,
            ),
            (
                "link-through",
                ("alias", b'1', "leaf/owner", b"".as_slice(), 0o644),
                ExtractReject::ThroughSymlink,
            ),
            (
                "new-link-through",
                ("leaf/alias", b'2', "owner", b"".as_slice(), 0o644),
                ExtractReject::ThroughSymlink,
            ),
        ] {
            assert!(
                matches!(extract(&[leaf, next], label, opts),
                Err(ExtractError::Rejected(got, _)) if got == reason),
                "{label}"
            );
            assert_eq!(std::fs::read(outside.join("owner")).unwrap(), b"keep");
            assert!(!outside.join("alias").exists());
        }
        for (label, name) in [("traversal", "../outside/alias"), ("absolute", target)] {
            assert!(extract(&[(name, b'2', target, b"", 0o755)], label, opts).is_err());
            assert_eq!(std::fs::read(outside.join("owner")).unwrap(), b"keep");
        }
    }

    fn global_pax_record(key: &str, value: &str) -> Vec<u8> {
        let payload = format!("{key}={value}\n");
        let mut len = payload.len() + 2;
        loop {
            let record = format!("{len} {payload}");
            if record.len() == len {
                return record.into_bytes();
            }
            len = record.len();
        }
    }

    #[test]
    fn descriptive_global_pax_is_explicit_strict_and_charged_to_the_same_caps() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let opts = ExtractOptions {
            allow_global_pax: true,
            ..ExtractOptions::default()
        };
        let record = global_pax_record("comment", &"a".repeat(800));
        let tar = tar_bytes(&[
            ("pax_global_header", b'g', "", &record, 0o644),
            ("plain", b'0', "", b"", 0o644),
        ]);
        let archive = scratch.path().join("global.zst");
        std::fs::write(&archive, zstd_bytes(&tar)).unwrap();
        let extract = |label: &str, cap, count, options| {
            extract_tar_zst_tree(
                &archive,
                &scratch.path().join(label),
                cap,
                count,
                options,
                FrameChecksum::IfPresent,
            )
        };
        assert!(matches!(
            extract("default", 1 << 20, 2, ExtractOptions::default()),
            Err(ExtractError::Rejected(ExtractReject::DisallowedKind, _))
        ));
        extract("complete", tar.len() as u64, 2, opts).unwrap();
        assert!(scratch.path().join("complete/plain").is_file());
        assert!(!scratch.path().join("complete/pax_global_header").exists());
        assert!(matches!(
            extract("one-byte-less", tar.len() as u64 - 1, 2, opts),
            Err(ExtractError::TooLarge(TooLargeReason::StructureCap))
        ));
        assert!(matches!(
            extract("one-entry-less", 1 << 20, 1, opts),
            Err(ExtractError::TooLarge(TooLargeReason::EntryCap))
        ));

        for (index, body) in [
            global_pax_record("path", "../outside"),
            global_pax_record("linkpath", "/outside"),
            global_pax_record("size", "0"),
            global_pax_record("GNU.sparse.map", "0,1"),
            global_pax_record("hdrcharset", "BINARY"),
            global_pax_record("unknown", "value"),
            b"12 comment=bad\n".to_vec(),
            b"15 comment=bad".to_vec(),
        ]
        .into_iter()
        .enumerate()
        {
            let bytes = tar_bytes(&[
                ("pax_global_header", b'g', "", &body, 0o644),
                ("plain", b'0', "", b"never", 0o644),
            ]);
            std::fs::write(&archive, zstd_bytes(&bytes)).unwrap();
            let label = format!("bad-{index}");
            assert!(extract(&label, 1 << 20, 2, opts).is_err(), "{index}");
            assert!(!scratch.path().join(label).join("plain").exists());
        }
        let mut bytes = raw_header("pax_global_header", b'g', "", record.len()).to_vec();
        bytes.extend_from_slice(&record[..record.len() - 1]);
        std::fs::write(&archive, zstd_bytes(&bytes)).unwrap();
        assert!(extract("truncated", 1 << 20, 2, opts).is_err());
    }

    /// An upstream tarball written as `tar -C dir -c .` — the shape both bsdtar and GNU
    /// tar produce — opens with a root-naming `./` DIRECTORY member and names every path
    /// `./…`. The authoring ceremony stages such an archive with the SYSTEM tar and signs
    /// the `tree_root` of what that lays, so the client lanes have to lay the same tree:
    /// before the fix the first member aborted the lane with `Rejected(EmptyPath, "./")`
    /// and every install of an otherwise honest row failed.
    #[test]
    fn dot_rooted_archives_extract_like_the_tar_that_authored_them() {
        let d = dest("dot-root");
        let tar = tar_bytes(&[
            ("./", b'5', "", b"", 0o755),
            ("./top/", b'5', "", b"", 0o755),
            ("./top/bin/", b'5', "", b"", 0o755),
            ("./top/bin/gh", b'0', "", b"#!/bin/sh\necho gh\n", 0o755),
            ("./top/LICENSE", b'0', "", b"MIT", 0o644),
        ]);
        let gz = d.join("dot.tar.gz");
        std::fs::write(&gz, gzip_bytes(&tar)).unwrap();

        // strip 0: `./` is the root itself — a no-op — and the rest lands exactly where
        // `tar -xzf … -C st` lays it. The fold still equals the on-disk walk.
        let r0 = d.join("gz0");
        let (root0, walk0) = fused_and_walked(
            extract_tar_gz_tree(&gz, &r0, 10_000_000, 10_000, vendor(0)).unwrap(),
            &r0,
        );
        assert_eq!(
            std::fs::read(r0.join("top/bin/gh")).unwrap(),
            b"#!/bin/sh\necho gh\n"
        );
        assert_eq!(std::fs::read(r0.join("top/LICENSE")).unwrap(), b"MIT");
        assert_eq!(root0, walk0, "fold vs walk");

        // strip 1: the leading `.` IS the component the strip spends itself on —
        // `top/bin/gh`, not `bin/gh` — which is the tree the authoring tar staged.
        let r1 = d.join("gz1");
        extract_tar_gz_tree(&gz, &r1, 10_000_000, 10_000, vendor(1)).unwrap();
        assert!(
            r1.join("top/bin/gh").is_file(),
            "the `.` is the stripped component, as it is for the authoring tar"
        );
        assert!(!r1.join("bin/gh").exists(), "one component too deep");

        // strip 2 reaches past it, dropping `top` as well.
        let r2 = d.join("gz2");
        extract_tar_gz_tree(&gz, &r2, 10_000_000, 10_000, vendor(2)).unwrap();
        assert!(r2.join("bin/gh").is_file());

        // A root-naming member is a no-op ONLY as a directory: as a FILE it still names
        // no target, and the lane still refuses it.
        let bad = tar_bytes(&[("./", b'0', "", b"pwned", 0o644)]);
        let bad_gz = d.join("bad.tar.gz");
        std::fs::write(&bad_gz, gzip_bytes(&bad)).unwrap();
        let err = extract_tar_gz_tree(&bad_gz, &d.join("gz-bad"), 10_000_000, 10_000, vendor(0))
            .unwrap_err();
        assert!(
            matches!(&err, ExtractError::Rejected(ExtractReject::EmptyPath, _)),
            "a regular file naming the root is still EmptyPath: {err:?}"
        );
    }

    /// The tar `entries` laid by the tar.gz lane and — when they hold no hardlink, which
    /// zip has not — by the zip lane too, each under its own root in `d`.
    #[allow(clippy::type_complexity)]
    fn in_both_lanes(
        d: &Path,
        label: &str,
        entries: &[(&str, u8, &str, &[u8], u32)],
        opts: ExtractOptions,
    ) -> Vec<(String, Result<TreeAccumulator, ExtractError>, PathBuf)> {
        let gz = d.join(format!("{label}.tar.gz"));
        std::fs::write(&gz, gzip_bytes(&tar_bytes(entries))).unwrap();
        let root = d.join(format!("{label}-gz"));
        let got = extract_tar_gz_tree(&gz, &root, 10_000_000, 10_000, opts);
        let mut lanes = vec![(format!("gz {label}"), got, root)];
        if entries.iter().all(|(_, tf, ..)| *tf != b'1') {
            let members: Vec<ZipMember<'_>> = entries
                .iter()
                .map(|(name, tf, link, body, mode)| ZipMember {
                    name,
                    mode: match tf {
                        b'5' => 0o040_755,
                        b'2' => 0o120_777,
                        _ => 0o100_000 | mode,
                    },
                    data: if *tf == b'2' { link.as_bytes() } else { body },
                    deflate: false,
                })
                .collect();
            let zip = d.join(format!("{label}.zip"));
            std::fs::write(&zip, zip_bytes(&members, false)).unwrap();
            let root = d.join(format!("{label}-zip"));
            let got = extract_zip_tree(&zip, &root, 10_000_000, 10_000, opts);
            lanes.push((format!("zip {label}"), got, root));
        }
        lanes
    }

    /// Slip refusals per lane: `..`, an absolute path, an escaping symlink and an
    /// absolute symlink each abort the gzip AND the zip lane with nothing written outside
    /// the root — and with `strip_components` set, so stripping is proven not to be a
    /// way around the vet.
    #[test]
    fn gzip_and_zip_lanes_refuse_every_slip_shape() {
        let d = dest("lanes-slip");
        for (label, name, tf, link, want) in [
            (
                "dotdot",
                "top/../../OUTSIDE",
                b'0',
                "",
                ExtractReject::ParentTraversal,
            ),
            ("abs", "/OUTSIDE", b'0', "", ExtractReject::AbsolutePath),
            (
                "link-escape",
                "top/bin/x",
                b'2',
                "../../OUTSIDE",
                ExtractReject::RootEscape,
            ),
            (
                "link-abs",
                "top/bin/x",
                b'2',
                "/etc/passwd",
                ExtractReject::AbsolutePath,
            ),
            (
                "link-empty",
                "top/bin/x",
                b'2',
                "",
                ExtractReject::EmptyPath,
            ),
        ] {
            // A tar symlink carries no body (a body would be the skip-bomb refusal).
            let body: &[u8] = if tf == b'2' { b"" } else { b"pwned" };
            let entries = [
                ("top/ok", b'0', "", b"fine".as_slice(), 0o644),
                (name, tf, link, body, 0o644),
            ];
            for (lane, got, _) in in_both_lanes(&d, label, &entries, vendor(1)) {
                assert!(
                    matches!(&got, Err(ExtractError::Rejected(r, _)) if *r == want),
                    "{lane}: {:?}",
                    got.map(|_| ())
                );
            }
        }
        assert!(
            !d.join("OUTSIDE").exists(),
            "nothing may land outside the roots"
        );
    }

    /// A symlink can never redirect a later write: an entry whose path passes THROUGH a
    /// link laid earlier, a file entry landing ON a link, and a link landing on an
    /// existing path are each refused — in the tar lane and the zip lane alike.
    #[cfg(unix)]
    #[test]
    fn writes_through_or_onto_a_symlink_are_refused() {
        /// `(label, tar entries, expected rejection)`.
        type Case = (
            &'static str,
            &'static [(&'static str, u8, &'static str, &'static [u8], u32)],
            ExtractReject,
        );
        let d = dest("through-link");
        let cases: &[Case] = &[
            (
                "through",
                &[
                    ("real/", b'5', "", b"", 0o755),
                    ("lib", b'2', "real", b"", 0o777),
                    ("lib/x", b'0', "", b"through", 0o644),
                ],
                ExtractReject::ThroughSymlink,
            ),
            (
                "onto",
                &[
                    ("real", b'0', "", b"r", 0o644),
                    ("alias", b'2', "real", b"", 0o777),
                    ("alias", b'0', "", b"overwrite", 0o644),
                ],
                ExtractReject::Occupied,
            ),
            (
                "link-over-file",
                &[
                    ("real", b'0', "", b"r", 0o644),
                    ("real", b'2', "elsewhere", b"", 0o777),
                ],
                ExtractReject::Occupied,
            ),
            (
                "hardlink-onto-link",
                &[
                    ("real", b'0', "", b"r", 0o644),
                    ("alias", b'2', "real", b"", 0o777),
                    ("alias", b'1', "real", b"", 0o644),
                ],
                ExtractReject::Occupied,
            ),
        ];
        for (label, entries, want) in cases {
            for (lane, got, root) in in_both_lanes(&d, label, entries, vendor(0)) {
                assert!(
                    matches!(&got, Err(ExtractError::Rejected(r, _)) if r == want),
                    "{lane}: {:?}",
                    got.map(|_| ())
                );
                // `real` must still hold its own bytes: nothing wrote through the link.
                if root.join("real").is_file() {
                    assert_eq!(std::fs::read(root.join("real")).unwrap(), b"r", "{lane}");
                }
            }
        }
    }

    /// A hardlink whose TARGET is named through a symlink is refused by the VET, before
    /// the link is made. `lstat` and `link` decline to follow only the last component,
    /// so `alias -> lnk/f` (with `lnk -> real` laid earlier in the same archive) was
    /// once proved "a regular file", physically linked as a second name for `real/f`,
    /// and only then refused — by the digest fold failing to find `lnk/f` among the
    /// paths it had written, which is a refusal by accident, in the folding lanes only.
    #[cfg(unix)]
    #[test]
    fn a_hardlink_target_named_through_a_symlink_is_refused_before_the_link_exists() {
        use std::os::unix::fs::MetadataExt;
        let d = dest("hl-through-link");
        let entries: &[(&str, u8, &str, &[u8], u32)] = &[
            ("real/", b'5', "", b"", 0o755),
            ("real/f", b'0', "", b"the one copy", 0o644),
            ("lnk", b'2', "real", b"", 0o777),
            ("alias", b'1', "lnk/f", b"", 0o644),
        ];
        let tar = tar_bytes(entries);
        let gz = d.join("a.tar.gz");
        std::fs::write(&gz, gzip_bytes(&tar)).unwrap();
        let zst = d.join("a.tar.zst");
        std::fs::write(&zst, zstd_bytes(&tar)).unwrap();
        let root_gz = d.join("gz");
        let root_zst = d.join("zst");
        let lanes = [
            (
                "gz",
                extract_tar_gz_tree(&gz, &root_gz, 10_000_000, 10_000, vendor(0)).map(|_| ()),
                &root_gz,
            ),
            (
                "zst",
                extract_tar_zst_tree(
                    &zst,
                    &root_zst,
                    10_000_000,
                    10_000,
                    vendor(0),
                    FrameChecksum::IfPresent,
                )
                .map(|_| ()),
                &root_zst,
            ),
        ];
        for (label, got, root) in lanes {
            let err = got.unwrap_err();
            assert!(
                matches!(
                    &err,
                    ExtractError::Rejected(ExtractReject::ThroughSymlink, _)
                ),
                "{label}: {err:?}"
            );
            assert!(
                std::fs::symlink_metadata(root.join("alias")).is_err(),
                "{label}: the alias was laid down before the refusal"
            );
            assert_eq!(
                std::fs::metadata(root.join("real/f")).unwrap().nlink(),
                1,
                "{label}: the target must keep its single name"
            );
        }
    }

    /// Two symlinks that each vet in-root cannot ADD UP to one that leaves it: a `..` that
    /// follows a component which is itself a link pops from wherever that link points on
    /// disk, not from where the lexical walk thinks it is. `a/b/c/up -> ../../..` is the
    /// root; `bin -> a/b/c/up/../OUTSIDE` then walks out of it. Refused in the tar and zip
    /// lanes, and in either order (audit K1, 2026-09-12).
    #[cfg(unix)]
    #[test]
    fn a_chain_of_in_root_symlinks_cannot_escape_the_root() {
        type Entry = (&'static str, u8, &'static str, &'static [u8], u32);
        let d = dest("link-chain");
        std::fs::create_dir_all(d.join("OUTSIDE")).unwrap();
        let dir: Entry = ("a/b/c/", b'5', "", b"", 0o755);
        let up: Entry = ("a/b/c/up", b'2', "../../..", b"", 0o777);
        let bin: Entry = ("bin", b'2', "a/b/c/up/../OUTSIDE", b"", 0o777);
        let file: Entry = ("gh-real", b'0', "", b"x", 0o755);
        let orders: [(&str, [Entry; 4]); 2] = [
            ("up-first", [dir, up, bin, file]),
            ("bin-first", [bin, dir, up, file]),
        ];
        for (label, entries) in &orders {
            for (lane, got, root) in in_both_lanes(&d, label, entries, vendor(0)) {
                let escaped = std::fs::canonicalize(root.join("bin"))
                    .is_ok_and(|p| p == std::fs::canonicalize(d.join("OUTSIDE")).unwrap());
                assert!(
                    matches!(
                        &got,
                        Err(ExtractError::Rejected(
                            ExtractReject::RootEscape | ExtractReject::ThroughSymlink,
                            _
                        ))
                    ),
                    "{lane}: {:?} (bin escaped: {escaped})",
                    got.map(|_| ())
                );
            }
        }
    }

    /// The caps hold in the gzip and zip lanes alike: the byte cap trips mid-body (a
    /// deflate bomb stops mid-stream); a body stripped away entirely is still charged and,
    /// under a cap it fits, lays nothing and folds the empty root the walk of the
    /// pre-created scratch agrees with; and the entry cap trips. A zip symlink whose target
    /// is implausibly long is refused before it is read.
    #[test]
    fn gzip_and_zip_lanes_enforce_the_same_caps() {
        type Lane =
            fn(&Path, &Path, u64, u64, ExtractOptions) -> Result<TreeAccumulator, ExtractError>;
        let d = dest("lanes-caps");
        let big = vec![b'x'; 4096];
        let write = |name: &str, bytes: Vec<u8>| {
            let p = d.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        let member = |name, mode, data| ZipMember {
            name,
            mode,
            data,
            deflate: true,
        };
        let one_tar = tar_bytes(&[("top/data", b'0', "", &big, 0o644)]);
        let two_tar = tar_bytes(&[("a", b'0', "", b"1", 0o644), ("b", b'0', "", b"2", 0o644)]);
        let lanes: [(&str, Lane, PathBuf, PathBuf); 2] = [
            (
                "gz",
                extract_tar_gz_tree,
                write("big.tar.gz", gzip_bytes(&one_tar)),
                write("two.tar.gz", gzip_bytes(&two_tar)),
            ),
            (
                "zip",
                extract_zip_tree,
                write(
                    "big.zip",
                    zip_bytes(&[member("top/data", 0o100_644, &big)], false),
                ),
                write(
                    "two.zip",
                    zip_bytes(
                        &[member("a", 0o100_644, b"1"), member("b", 0o100_644, b"2")],
                        false,
                    ),
                ),
            ),
        ];
        for (label, lane, one, two) in lanes {
            let root = |case: &str| d.join(format!("{label}-{case}"));
            assert!(
                matches!(
                    lane(&one, &root("cap"), 1024, 10_000, vendor(1)),
                    Err(ExtractError::TooLarge(TooLargeReason::ContentCap))
                ),
                "{label}: byte cap"
            );
            assert!(
                matches!(
                    lane(&one, &root("strip-cap"), 1024, 10_000, vendor(2)),
                    Err(ExtractError::TooLarge(_))
                ),
                "{label}: a stripped-away body is charged"
            );
            std::fs::create_dir_all(root("strip-ok")).unwrap();
            let ok = lane(&one, &root("strip-ok"), 8192, 10_000, vendor(2)).unwrap();
            let (fused, walked) = fused_and_walked(ok, &root("strip-ok"));
            assert_eq!(fused, walked, "{label}: stripped away is not written");
            assert!(
                matches!(
                    lane(&two, &root("entries"), 10_000, 1, vendor(0)),
                    Err(ExtractError::TooLarge(TooLargeReason::EntryCap))
                ),
                "{label}: entry cap"
            );
        }
        let long = vec![b'a'; 5000];
        let zip = write(
            "longlink.zip",
            zip_bytes(&[member("lnk", 0o120_777, &long)], false),
        );
        let err =
            extract_zip_tree(&zip, &d.join("zip-longlink"), 10_000_000, 10, vendor(0)).unwrap_err();
        assert!(
            matches!(
                err,
                ExtractError::TooLarge(TooLargeReason::LinkTargetTooLong(_))
            ),
            "zip long link: {err:?}"
        );
    }

    /// What the zip reader refuses on its own format rules — each fail-closed, none a
    /// panic: encryption, an unsupported method, a backslash in a name, a truncated file,
    /// a body that disagrees with its declared size, trailing garbage, and a non-Unix
    /// member whose attributes carry no mode (laid down `0644`, never executable). And the
    /// ZIP64 rendering of a perfectly ordinary archive extracts identically.
    #[test]
    fn the_zip_reader_refuses_what_it_does_not_understand_and_reads_zip64() {
        let d = dest("zip-format");
        let members = [
            ZipMember {
                name: "bin/tool",
                mode: 0o100_755,
                data: b"#!/bin/sh\n",
                deflate: true,
            },
            ZipMember {
                name: "README",
                mode: 0o100_644,
                data: b"read me",
                deflate: false,
            },
        ];
        let plain = zip_bytes(&members, false);
        let z64 = zip_bytes(&members, true);
        assert_ne!(plain, z64, "the fixture must really render two encodings");
        let p = d.join("plain.zip");
        std::fs::write(&p, &plain).unwrap();
        let q = d.join("z64.zip");
        std::fs::write(&q, &z64).unwrap();
        let (root_p, walk_p) = fused_and_walked(
            extract_zip_tree(&p, &d.join("plain"), 10_000, 10, vendor(0)).unwrap(),
            &d.join("plain"),
        );
        let root_q = extract_zip_tree(&q, &d.join("z64"), 10_000, 10, vendor(0))
            .unwrap()
            .root();
        assert_eq!(root_p, root_q, "ZIP64 is an encoding, not a different tree");
        assert_eq!(root_p, walk_p);

        let refuse = |label: &str, bytes: &[u8]| {
            let path = d.join(format!("{label}.zip"));
            std::fs::write(&path, bytes).unwrap();
            let err = extract_zip_tree(&path, &d.join(label), 10_000, 10, vendor(0)).unwrap_err();
            assert!(matches!(err, ExtractError::Io { .. }), "{label}: {err:?}");
        };
        // Encrypted: flip the general-purpose flag bit 0 in the central record.
        let mut enc = plain.clone();
        let cd = enc
            .windows(4)
            .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        enc[cd + 8] |= 1;
        refuse("encrypted", &enc);
        // Unsupported method (bzip2 = 12) in the central record.
        let mut meth = plain.clone();
        meth[cd + 10] = 12;
        refuse("method", &meth);
        // Backslash in a name.
        refuse(
            "backslash",
            &zip_bytes(
                &[ZipMember {
                    name: "bin\\tool",
                    mode: 0o100_644,
                    data: b"x",
                    deflate: false,
                }],
                false,
            ),
        );
        // Truncated: drop the tail (the EOCD is gone).
        refuse("truncated", &plain[..plain.len() - 10]);
        // Trailing garbage after the EOCD.
        let mut trailing = plain.clone();
        trailing.extend_from_slice(b"garbage");
        refuse("trailing", &trailing);
        // A stored member whose declared uncompressed size disagrees with its body.
        let mut lying = plain.clone();
        let readme_cd = lying.windows(6).rposition(|w| w == b"README").unwrap();
        // The central record for README starts 46 bytes before its name; uncompressed
        // size sits at +24.
        let rec = readme_cd - 46;
        assert_eq!(&lying[rec..rec + 4], &[0x50, 0x4b, 0x01, 0x02]);
        lying[rec + 24..rec + 28].copy_from_slice(&3u32.to_le_bytes());
        refuse("lying-size", &lying);
        // Empty file.
        refuse("empty", b"");
        // No Unix attributes: everything is a regular file at 0644, never executable.
        let fat = d.join("fat.zip");
        std::fs::write(
            &fat,
            zip_bytes(
                &[ZipMember {
                    name: "bin/tool",
                    mode: 0,
                    data: b"#!/bin/sh\n",
                    deflate: false,
                }],
                false,
            ),
        )
        .unwrap();
        let fat_root = d.join("fat");
        extract_zip_tree(&fat, &fat_root, 10_000, 10, vendor(0)).unwrap();
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(fat_root.join("bin/tool"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o644
        );
    }

    /// `stage_file` — the raw-binary lane's whole extraction — lays one file down with the
    /// requested mode, caps it, and hands back the digest the walk would compute.
    #[test]
    fn stage_file_lays_one_capped_file_and_folds_it() {
        let d = dest("stage-file");
        let dest_path = d.join("bin/claude");
        std::fs::create_dir_all(dest_path.parent().unwrap()).unwrap();
        let payload = b"the binary";
        let w = stage_file(&payload[..], &dest_path, 0o755, 1 << 20).unwrap();
        // The folded mode is the one requested on EVERY platform: read back on Unix,
        // declared on Windows — a `0` here is the exact defect that made every Windows
        // `claude` install fail its re-verify (2026-09-22).
        assert_eq!(w.mode, 0o755, "the raw-binary lane folds mode 0755");
        let mut tree = TreeAccumulator::new();
        tree.record_file(b"bin/claude".to_vec(), w.mode, w.content_sha_hex);
        let (fused, walked) = fused_and_walked(tree, &d);
        assert_eq!(fused, walked);
        let err = stage_file(&payload[..], &d.join("capped"), 0o755, 4).unwrap_err();
        assert!(matches!(err, ExtractError::TooLarge(_)), "capped: {err:?}");
    }

    /// A disk failure mid-bundle names the step AND the path it was on.
    ///
    /// The caller deletes the partial `dest_root` on any failure, so `io: No space left
    /// on device (os error 28)` was the entire record of a 508-member bundle that died
    /// at member 300 — no member, no destination, no read-vs-write. On macOS the same
    /// bare line covered an `EPERM` that is usually privacy consent arriving with no
    /// dialog, where the operator's only question is which path needs it. EACCES from a
    /// write-protected stage root is that shape, without needing a full disk.
    #[cfg(unix)]
    #[test]
    fn an_io_failure_names_the_operation_and_the_path() {
        let d = dest("io-context");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        let archive = make_archive(&d, "tool", &[("bin/tool", b'0', "", b"#!/bin/sh\n")]);
        // Drop write permission on the stage root: laying `bin/` under it is EACCES.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
        let got = extract_tar_zst(&archive, &root, 10_000_000, 10_000);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let Err(err) = got else {
            aterm_uds::host::could_not_run(
                "a lay under a mode-0500 root succeeded: root, or a file system that ignores modes",
            );
        };
        let ExtractError::Io { path, .. } = &err else {
            panic!("want Io, got {err:?}");
        };
        assert_eq!(path.as_deref(), Some(root.join("bin").as_path()));
    }

    /// `write_capped` forces the sanitized mode through the HANDLE it is still holding.
    ///
    /// The chmod used to go by path — `platform::set_mode(dest, mode)`, a full
    /// resolution of a `<build>.incoming-<pid>/lib/rustlib/...` path — while `f` was
    /// open and one line away from being `fstat`ed for those very bits, once per file
    /// (4114 of them in the shipped `trust` sysroot). A reader that unlinks `dest`
    /// mid-body is the witness that the handle is what gets chmod'd now: the bytes, the
    /// `fchmod` and the mode read-back all describe the file we wrote, where the path
    /// form had nothing left to resolve and failed the whole extraction with `ENOENT`.
    #[cfg(unix)]
    #[test]
    fn write_capped_sets_the_mode_through_the_open_handle() {
        /// Hands the body over in one read, then unlinks `dest` behind the writer.
        struct UnlinkMidBody<'a> {
            body: &'a [u8],
            dest: &'a Path,
            done: bool,
        }
        impl Read for UnlinkMidBody<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.done {
                    return Ok(0);
                }
                self.done = true;
                let n = self.body.len().min(buf.len());
                buf[..n].copy_from_slice(&self.body[..n]);
                std::fs::remove_file(self.dest)?;
                Ok(n)
            }
        }

        let d = dest("fchmod-handle");
        let path = d.join("lib/rustlib/aarch64-apple-darwin/lib/libstd.rlib");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = b"rlib bytes";
        let budget = Rc::new(Cell::new(TAR_ENTRY_STRUCTURAL_BUDGET));
        let mut buf = vec![0u8; 64 * 1024];
        let mut remaining: u64 = 1 << 20;
        let w = write_capped(
            UnlinkMidBody {
                body,
                dest: &path,
                done: false,
            },
            Sink {
                file: crate::platform::open_create_write(&path, 0o755).unwrap(),
                dest: &path,
                mode: 0o755,
            },
            &mut remaining,
            &budget,
            &mut buf,
            true,
        )
        .unwrap();
        assert_eq!(w.len, body.len() as u64);
        // Read back off the same handle: the mode reached the inode we wrote.
        assert_eq!(w.mode, 0o755);
        // The scalar reference, not the fold's own `BulkSha256`.
        let mut h = aterm_digest::Sha256::new();
        h.update(body);
        assert_eq!(
            w.content_sha_hex,
            aterm_codec::hex::encode_lower(&h.finalize())
        );
    }
}
