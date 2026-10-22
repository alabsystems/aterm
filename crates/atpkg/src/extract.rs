// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tar-slip-safe archive extraction (§4.2/§15): the fail-closed decision for whether an
//! archive entry may be written under the per-program store root, and the one writer that
//! lays every lane's entries.
//!
//! A bundle is attacker-influenced input: only the compressed blob is signature-verified,
//! and the extracted layout is re-checked against `tree_root` afterwards (§8). Any slip
//! aborts the whole staged group. [`vet_entry`] turns a `(raw path, kind)` pair into a safe
//! absolute destination under the root or an [`ExtractReject`]; every reader (`tarread`,
//! `extract/zipread.rs`) feeds each entry through it before writing a byte.
//!
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
//! The decompressors (zstd, gzip, deflate-in-zip) and container parsers (tar, zip) differ,
//! but everything that touches the disk (the vet, `strip_components`, the caps, the mode
//! sanitizing, the symlink rules and the `tree_root` fold) is one code path, [`Layer`].

use std::cell::Cell;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use crate::tarread::EntryType as TarEntryType;

mod zipread;

/// Why an archive entry was refused. Any variant aborts the whole staged group (§7).
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
    /// The entry's destination passes through a symlink laid earlier in the same archive,
    /// so the bytes would land somewhere other than the path the digest records.
    ThroughSymlink,
    /// A symlink entry names an occupied path, a file/directory entry names a path an
    /// earlier symlink occupies, or an entry names a file or passes through a directory
    /// the filesystem resolves to another member's name under different bytes (case- or
    /// normalization-insensitive). Each would make the disk disagree with the digest.
    Occupied,
    /// The filesystem stored a laid name under other bytes, or laid nothing for it (HFS+
    /// decomposing NFC to NFD, Win32 dropping a trailing `.` or space, a device name such
    /// as `nul`), so the walk would never find the line the fold recorded. Found when the
    /// stage reads its names back (`TreeAccumulator::require_spelled_back`); the path is
    /// the first recorded name the disk does not hold under those bytes.
    Respelled,
    /// A single-file archive ([`stage_tar_gz_member`]) that is not exactly one member named
    /// by one bare component: a second member, a nested or dotted name (`dir/agy`,
    /// `./agy`, `agy/`), or (with an empty path) no member at all.
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
    /// `gh_2.80.0_macOS_arm64/bin/gh` → `bin/gh` at `1`. An entry with nothing left is
    /// skipped, its body drained through the same byte cap; hardlink targets strip the
    /// same way. The full path is vetted before stripping, so `..` above the strip depth
    /// is still a refusal.
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

/// Steps 2–4 of [`vet_entry`]: the pure component walk. Accepts only `Normal` segments
/// (`.` dropped; `..` is traversal; a root/prefix component is an absolute escape) and
/// refuses a path that names nothing. Returns the validated relative path.
fn vet_components(raw: &Path) -> Result<PathBuf, ExtractReject> {
    // An absolute entry path can never be made relative to the root.
    if raw.is_absolute() {
        return Err(ExtractReject::AbsolutePath);
    }
    // A `..` component is traversal; a name containing dots (`foo..bar`) is Normal.
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

/// How many leading `.` components the raw entry path carries (`./x` → 1, `././x` → 2).
/// The tar that authors these rows spends `strip_components` on them (bsdtar 3.5.3:
/// `--strip-components 1` over `./top/bin/gh` lays `top/bin/gh`), and [`vet_components`]
/// drops `.`, so the count is taken from the raw path and subtracted from the strip.
/// Otherwise a `tar -C dir -c .` archive lays one component deeper than the tree the
/// authoring ceremony signed.
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

/// Vet one archive entry against the store `root`: the safe absolute destination for a
/// purely relative, traversal-free [`EntryKind::Regular`]/[`EntryKind::Directory`] entry,
/// else a fail-closed [`ExtractReject`]. Pure (no filesystem access).
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

/// [`vet_entry`] with `strip_components`: the full raw path is vetted first (so `..` or an
/// absolute prefix above the strip depth is still refused), then the leading components
/// are dropped. `Ok(None)` when nothing remains: skip the entry. A leading `.` is one of
/// the components the strip spends ([`leading_curdirs`]).
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

/// Vet one symlink entry for the lanes that admit them: the entry path passes the
/// component walk (and `strip`), and the target, created verbatim, must resolve lexically
/// inside the root. A relative target resolves from the link's own directory; popping
/// above the root is [`ExtractReject::RootEscape`], an absolute target
/// [`ExtractReject::AbsolutePath`], an empty one [`ExtractReject::EmptyPath`]. Returns the
/// link's safe absolute path; `Ok(None)` when the entry strips away.
///
/// Lexical, so it cannot be raced. That is sound only because `..` is admitted as a
/// leading run and nowhere else: a `..` after a component pops from wherever that
/// component points on disk, and if it is an earlier link the walk and the disk disagree
/// (`a/b/c/up -> ../../..` plus `bin -> a/b/c/up/../OUTSIDE` would chain out). Leading
/// pops start from a real directory (ancestors and the landing path are guarded against
/// links), and after them the target only descends.
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

/// A failure while extracting a bundle. Any variant aborts the whole stage; the caller
/// removes the partial `dest_root`.
#[derive(Debug)]
pub enum ExtractError {
    /// An I/O / decompression / container-format error.
    ///
    /// `op` names the failing step and `path` the filesystem path it was on: the caller
    /// deletes the partial `dest_root`, so this is the only record of how far a bundle got,
    /// and on macOS an `EPERM` is usually privacy consent, where the question is which
    /// path. `path: None` (with `op` `""`) is an error from a reader with no path in scope.
    Io {
        /// The underlying error.
        err: io::Error,
        /// The operation that failed (`"open"`, `"write"`, `"set_mode"`, …), or `""`.
        op: &'static str,
        /// The path the operation was attempted on, when the failing site knew it.
        path: Option<PathBuf>,
    },
    /// An entry failed [`vet_entry`] — a tar-slip escape. Carries the offending raw path.
    Rejected(ExtractReject, PathBuf),
    /// The bundle tripped a decompression-bomb / tar-bomb defence ([`TooLargeReason`]). The
    /// caps come from the signed `disk_installed`/`size` (§9), never a header field.
    TooLarge(TooLargeReason),
}

/// Which bomb defence refused the bundle. Each renders as what it is, so a malformed
/// member is not reported as the size cap; the member-shaped ones carry the raw path.
#[derive(Debug)]
pub enum TooLargeReason {
    /// The signed uncompressed-content cap (`max_total_bytes`) was reached mid-stream.
    ContentCap,
    /// A tar's structure (headers, padding, GNU/PAX extension headers and bodies) exceeded
    /// its own allowance: the signed cap again, counted apart from content.
    StructureCap,
    /// The signed entry-count cap (`max_entries`) was reached.
    EntryCap,
    /// One entry's structural reads (header, padding, GNU longname/longlink or PAX body)
    /// exceeded the per-entry budget (`TAR_ENTRY_STRUCTURAL_BUDGET`).
    StructuralBudget,
    /// A directory member declared a nonzero body size, a skip-bomb the byte cap never
    /// sees. Carries the raw member path.
    DirectoryWithBody(PathBuf),
    /// A hardlink member declared a nonzero body size. Carries the raw member path.
    HardlinkWithBody(PathBuf),
    /// A symlink member declared a nonzero body size. Carries the raw member path.
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
/// Executables (any `x` bit set) and directories become `0o755`, everything else `0o644`.
/// Deterministic, so the staged [`crate::tree::tree_root`] matches the publisher's.
///
/// The one mode rule of a staged tree, whatever lane laid it: archive members
/// ([`Layer::regular`], [`Layer::directory`]), the directories a folding lane makes on a
/// member's way ([`make_dir_exact`]), and the `.app` bundle the `dmg` lane's `ditto`
/// copied (`install::vet_copied_tree`, AT3 in docs/AUDIT-atpkg-inputs-2026-10-04.md). The
/// single-binary lanes' one file (`raw-binary`, `tar-gz-binary`) is `0o755`.
pub fn safe_mode(entry_mode: u32, is_dir: bool) -> u32 {
    if is_dir || entry_mode & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}

/// The per-entry structural byte budget: the most decompressed bytes the tar reader may
/// pull for one entry's non-content parts (header, padding, GNU longname/longlink or PAX
/// extension body). Reset before each entry ([`extract_stream`]) and refunded for content
/// ([`write_capped`]). 1 MiB dwarfs any legitimate header, `PATH_MAX` name or PAX record;
/// it bounds the memory one entry's structure takes (the reader holds an extension body
/// whole).
///
/// The total is bounded separately, since a per-entry budget alone scales with
/// `max_entries`: every structural byte is also charged to an allowance the size of the
/// signed cap ([`Layer::charge_structure`]), so the whole stream is at most twice the
/// signed cap. That allowance is not the content's: a tree of small files carries more
/// structure than content, and `disk_installed` signs only content. The packers sign at
/// least half the decompressed stream (`atpkg_publish::artifact::tarwrite`), so their
/// bundles fit either.
const TAR_ENTRY_STRUCTURAL_BUDGET: u64 = 1 << 20;

/// Marker error the [`CappedReader`] raises when the structural budget is exhausted, so
/// extraction maps it to `TooLargeReason::StructuralBudget` wherever the over-read happens.
#[derive(Debug)]
struct SizeCapExceeded;

impl std::fmt::Display for SizeCapExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("decompressed size cap exceeded")
    }
}

impl std::error::Error for SizeCapExceeded {}

/// Wraps the decompressor and bounds every byte the tar reader pulls against a shared,
/// per-entry-reset budget, catching extension-header bodies the reader pulls inside its
/// entry iterator before [`write_capped`] sees anything. `write_capped` refunds content
/// bytes, so the budget constrains only structure and a multi-GiB file still streams.
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
    /// The folded permission bits (`platform::folded_mode`, masked to `0o7777`) and the
    /// lowercase-hex content sha256.
    File(u32, String),
    /// Lowercase-hex sha256 of the raw target bytes ([`crate::tree::symlink_target_sha`]).
    Link(String),
}

/// The extraction-time twin of [`crate::tree::tree_root`]: the same digest, folded from the
/// bytes as the extractor writes them, which saves the stage a full read pass over the
/// uncompressed payload.
///
/// The line format and the fold are shared with the on-disk walk
/// ([`crate::tree::entry_line`], [`crate::tree::symlink_line`],
/// [`crate::tree::root_of_entry_lines`]); this type supplies the inputs per entry, modelling
/// the filesystem the walk would see rather than the archive stream:
///
/// * **relpath**: the vetted, stripped destination under `dest_root`, spelled by
///   [`crate::tree::rel_line_bytes`] as the walk spells it.
/// * **mode**: [`crate::platform::folded_mode`], read back after `set_mode` on Unix (so a
///   filesystem that stores other bits moves both digests together); on Windows, which
///   stores no POSIX bits, the requested mode (also kept in [`Self::declared_modes`] for
///   the walk).
/// * **content digest**: hashed chunk by chunk as [`write_capped`] writes it.
/// * **symlink target**: the verbatim bytes handed to `symlink(2)`.
///
/// Two entries sharing one inode share one node: a hardlink aliases its target's node, and
/// a duplicate path (re-opened with `O_TRUNC`) reuses the node it already names. A symlink
/// is never re-opened, because [`Layer`] refuses any entry landing on or passing through
/// one ([`ExtractReject::Occupied`] / [`ExtractReject::ThroughSymlink`]).
///
/// A line's relpath is the member's bytes, the walk's is what the directory listing
/// returns, so they agree only while the filesystem stores every name apart and as given.
/// Two names the filesystem takes for one (`bin/tool` and `BIN/TOOL` on case-insensitive
/// APFS or NTFS, NFC and NFD `café` on APFS) are caught by laying every name exclusively
/// unless this extraction laid those exact bytes before ([`TreeAccumulator::wrote_file`],
/// [`make_dir_exact`]), refusing [`ExtractReject::Occupied`]. The rule compares bytes, not
/// a case or Unicode fold, so it answers the same on every filesystem. One name stored
/// under other bytes (HFS+ decomposing NFC, Win32 dropping a trailing `.` or laying
/// nothing for `nul`) is caught by reading the names back once
/// ([`Self::require_spelled_back`]), refusing [`ExtractReject::Respelled`].
#[derive(Debug)]
pub(crate) struct TreeAccumulator {
    /// relpath bytes → index into `nodes`: which node that path currently names. Sorted,
    /// as the fold wants the lines.
    paths: std::collections::BTreeMap<Vec<u8>, usize>,
    /// One entry per distinct node laid down.
    nodes: Vec<Node>,
    /// The directories this extraction made, as a tree of exact names: `dirs[i]` maps each
    /// child directory's name bytes to its index, and `dirs[0]` is the root. Memory is
    /// linear in the member names read, which the structure allowance bounds.
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

    /// Record a regular file written at `rel`. A path written twice reuses its node: the
    /// second `open(O_TRUNC)` lands on the same inode, so every alias sees the new content
    /// and mode, as the on-disk walk would.
    pub(crate) fn record_file(&mut self, rel: Vec<u8>, mode: u32, content_sha_hex: String) {
        // `copied()` first so the `paths` borrow ends before `nodes` is borrowed mutably.
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

    /// Record a hardlink at `rel` aliasing the already-extracted `target_rel`. False (fail
    /// closed) when the target is not a regular file this extraction wrote under exactly
    /// these bytes; [`Layer::hardlink`] checks [`Self::wrote_file`] first.
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

    /// The mode each recorded regular file folds, keyed by line bytes: the map the on-disk
    /// walk needs where the filesystem stores no permission bits
    /// ([`crate::tree::tree_root_declared`]). An alias reports its target's mode; links are
    /// not listed.
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
    /// or the walk `aterm pkg verify` takes never reproduces this root. Asked once, after
    /// the last name is laid and before the root is handed out.
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

    /// Fold every recorded path into the tree root, through the on-disk walk's formatters
    /// and sort+SHA-256.
    pub(crate) fn root(self) -> String {
        let mut lines: Vec<Vec<u8>> = Vec::with_capacity(self.paths.len());
        for (rel, node) in &self.paths {
            // Every index came from `self.nodes`; a miss is skipped rather than indexed,
            // which could only produce a mismatching root (fail closed).
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

/// The line bytes of `path` relative to `root`, the relpath half of a `tree_root` entry
/// line, through the walk's own spelling function ([`crate::tree::rel_line_bytes`]).
/// `path` is inside `root` by construction; a miss is still the fail-closed `RootEscape`.
pub(crate) fn rel_bytes_under(root: &Path, path: &Path) -> Result<Vec<u8>, ExtractError> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| ExtractError::Rejected(ExtractReject::RootEscape, path.to_path_buf()))?;
    Ok(crate::tree::rel_line_bytes(rel))
}

/// Make directory `dir` and every missing ancestor below `root`, one component at a time,
/// for an extraction folding into `tree`; `raw` is the member blamed on refusal.
///
/// A component `tree` already holds under exactly these bytes is this extraction's own
/// directory. Any other is made with a plain, exclusive `mkdir`: in a root that started
/// empty, `EEXIST` means this extraction laid that name under other bytes (`dir` after
/// `Dir` on case-insensitive APFS or NTFS, NFD after NFC on APFS, or a file or link), so
/// the member would land under the earlier spelling while the fold recorded its own. That
/// is refused as [`ExtractReject::Occupied`]. `create_dir_all` would answer "a directory
/// is here" for the wrong name.
///
/// The file and link creates make the same decision for the last component; this is the
/// one maker of directories for every folding lane and for the links a stage adds
/// afterwards. Each directory gets [`safe_mode`]'s `0755`, as a directory member does,
/// rather than `0777 & ~umask`.
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

/// The precondition the folded digest stands on, enforced: the fold describes what this
/// extraction wrote, and the on-disk walk describes everything under `dest_root`; they are
/// the same set only while `dest_root` starts empty. A stranger's file would make the walk
/// fail closed but the fold pass, so it is refused here. The staging contract provides an
/// empty root (`verify_and_stage` re-creates `incoming` under the store lock), and
/// `TreeAccumulator::record_alias` relies on it too.
///
/// A `dest_root` that does not exist yet is fine; extraction creates it.
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

/// One extraction in progress: the vetted, capped, digest-folding writer every archive
/// lane lays its entries through. The container readers (tar in [`extract_stream`], zip
/// in `zipread`) only parse and hand over `(raw path, kind, mode, body)`.
struct Layer<'a> {
    root: &'a Path,
    opts: ExtractOptions,
    /// The signed content cap, decremented as bytes are written (or drained).
    remaining: u64,
    /// A tar's structure allowance: the signed cap again, decremented as headers, padding
    /// and extension bodies are decompressed ([`Self::charge_structure`]). Unused by zip.
    structure_remaining: u64,
    max_entries: u64,
    count: u64,
    /// The per-entry structural budget of the tar [`CappedReader`], shared so
    /// [`write_capped`] can refund content bytes into it. The zip lane has no
    /// structural reader; it hands over a budget nothing decrements.
    budget: Rc<Cell<u64>>,
    /// One copy buffer for the whole archive: a 64 KiB local in `write_capped` would be
    /// zeroed per entry (LLVM cannot elide it behind `Read::read`). Heap `vec!`, not
    /// `Box::new([0u8; N])`, which builds the array on the stack first.
    copy_buf: Vec<u8>,
    /// The tree_root of what we are writing, folded as we write it (see
    /// [`TreeAccumulator`]) — so `verify_and_stage` need not walk the payload a second
    /// time. `None` for a write-only pass.
    tree: Option<TreeAccumulator>,
    /// Whether any symlink has been laid yet. Until one has, no entry can pass through or
    /// land on a link, so the per-entry `lstat` walks are skipped.
    laid_symlink: bool,
    /// The parent directory the last laid entry needed, known to be a directory
    /// ([`Layer::ensure_parent`]).
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
            // The fold's directory tree starts at the root ([`TreeAccumulator::new`]), so
            // the root is made here: the one directory whose own name is in no line.
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

    /// Charge `n` decompressed structural bytes (a tar header, padding, a GNU/PAX extension
    /// header and body) against the structure allowance. [`TAR_ENTRY_STRUCTURAL_BUDGET`]
    /// bounds one entry; this bounds all of them together by the signed size.
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

    /// Make sure `dest`'s parent directory exists, remembering the last one so a run of
    /// entries in one directory asks once (a `create_dir_all` on an existing parent is a
    /// failing `mkdir` plus a `stat`, per file). An archive may omit directory members, so
    /// the make itself stays.
    ///
    /// Skipping is sound because nothing can turn a directory this extraction made into
    /// something else: [`Layer::symlink`] refuses any occupied path and nothing here
    /// removes a directory.
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

    /// Lay down a directory entry. A non-zero `declared_size` (the effective body size) is
    /// a bomb the content cap never sees, since the reader skips the body, so it is
    /// refused on the header.
    fn directory(&mut self, raw: &Path, declared_size: u64, mode: u32) -> Result<(), ExtractError> {
        if declared_size != 0 {
            return Err(ExtractError::TooLarge(TooLargeReason::DirectoryWithBody(
                raw.to_path_buf(),
            )));
        }
        let vetted = match vet_entry_stripped(self.root, raw, EntryKind::Directory, self.strip()) {
            Ok(v) => v,
            // A directory member naming the stage root itself (`.`, `./`) is a no-op, like
            // a member the strip consumes: `Layer::open` made the root and no line
            // describes it. `tar -C dir -c .` archives open with one. Its mode is not
            // applied (the root's bits are the store's). `EmptyPath` stays a refusal for
            // files, symlinks and both ends of a hardlink.
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
            // advance, through the same caps as a body that is written.
            return drain_capped(body, &mut self.remaining, &self.budget, &mut self.copy_buf);
        };
        self.guard_ancestors(&dest, raw)?;
        self.refuse_if_link(&dest, raw)?;
        self.ensure_parent(&dest, raw)?;
        // A folding lane starts from an empty root and `ensure_parent` made every
        // directory on the way under exactly these bytes, so a name that already resolves
        // is either this exact path written earlier (a duplicate member, truncated as tar
        // does; the fold reuses its node) or a name the filesystem folds onto another
        // member's file (`bin/tool` and `bin/TOOL` on case-insensitive APFS or NTFS, NFC
        // and NFD on APFS). The second is refused: `O_EXCL` asks the filesystem's own name
        // equality.
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
    /// extracted regular file, the alias recorded against the target's node. A hardlink
    /// carries no body (bsdtar and GNU tar write 0); a non-zero `declared_size` would be
    /// discarded by the reader against the per-entry structural budget alone, never the
    /// content cap, so it is refused on the header.
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
        // …and the target's ancestors: `symlink_metadata` and `hard_link` follow every
        // component but the last, so a target named through an earlier directory link
        // (`lnk/f`, `lnk -> real`) would otherwise be linked to `real/f`.
        self.guard_ancestors(&target_abs, raw)?;
        // The target must already be an extracted regular file (honest archivers emit the
        // file before its links). `symlink_metadata`, so a symlink at the target fails
        // `is_file` rather than being followed.
        let target_meta = std::fs::symlink_metadata(&target_abs).map_err(|_| {
            ExtractError::Rejected(ExtractReject::HardlinkTargetMissing, raw.to_path_buf())
        })?;
        if !target_meta.is_file() {
            return Err(ExtractError::Rejected(
                ExtractReject::HardlinkTargetMissing,
                raw.to_path_buf(),
            ));
        }
        // A folding lane also asks the fold: the target must be a file this extraction
        // wrote under exactly these bytes. The `lstat` above uses the filesystem's name
        // equality, so on case-insensitive APFS `DIR/F` would pass for a written `dir/f`.
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
        // A same-inode alias of already-counted bytes: no content-cap charge (the entry
        // cap still applies). `link(2)` never replaces, so a name already there is
        // occupied, as for a file or a symlink.
        std::fs::hard_link(&target_abs, &dest).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                ExtractError::Rejected(ExtractReject::Occupied, raw.to_path_buf())
            } else {
                io_at(e, "hard_link", &dest)
            }
        })?;
        // …and for the digest a same-node alias, reusing the target's (mode, content sha).
        if let (Some(tree), Some((dest_rel, target_rel))) = (self.tree.as_mut(), rels)
            && !tree.record_alias(dest_rel, &target_rel)
        {
            // Unreachable: the fold answered for this target above. Fail closed rather
            // than invent a digest.
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

/// Create the symlink `link -> target` with the target bytes verbatim (what the digest
/// records and `read_link` hands back). Unix only; elsewhere it fails closed rather than
/// lay a tree the digest could not describe.
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

/// Extract a `.tar.zst` bundle at `archive` into `dest_root`, every entry vetted before a
/// byte is written ([`vet_entry`]). The first rejected entry, or exceeding
/// `max_total_bytes` / `max_entries`, aborts with an [`ExtractError`] and the caller
/// removes the partial `dest_root` (§7). Symlinks and exotic entries are refused and modes
/// are sanitized ([`safe_mode`]).
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
    // `fold = false`: no content hashing and no per-file `fstat`.
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

/// [`extract_tar_zst`], folding the [`crate::tree::tree_root`] of what it writes as it
/// writes it, so the stage need not re-read the payload. The digest is over what the
/// extractor wrote, not a walk of the finished tree; `verify_and_stage` argues why that
/// suffices.
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

/// The `tar-gz` vendor lane: the same tar extraction as [`extract_tar_zst_tree`] behind a
/// gzip decoder (`aterm_codec::inflate::stream`; multi-member as `gzip -d` is,
/// CRC/length-checked per member). Only the decompressor differs.
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

/// Which container a vetted [`extract_tree`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// A gzip-compressed tar (`.tar.gz`, `.tgz`).
    TarGz,
    /// An uncompressed tar.
    Tar,
    /// A zip archive.
    Zip,
}

/// Lay `archive` into `dest_root`, an existing empty directory, through the lanes a
/// package install lays a vendor archive with: every member's path vetted beneath the
/// root, modes from [`safe_mode`], links only as `opts.symlinks` admits, at most
/// `max_total_bytes` of content in at most `install::MAX_ENTRIES` entries. For a caller
/// outside atpkg that unpacks an upstream archive it pinned by sha256 and checked before
/// this call (`targo --unverified verify provision`). On an error the caller removes the
/// partial `dest_root`.
///
/// # Errors
/// Any [`ExtractError`]; a `dest_root` that is not empty is refused before anything is
/// laid.
pub fn extract_tree(
    archive: &Path,
    dest_root: &Path,
    container: Container,
    max_total_bytes: u64,
    opts: ExtractOptions,
) -> Result<(), ExtractError> {
    require_empty_destination(dest_root)?;
    let entries = crate::install::MAX_ENTRIES;
    match container {
        Container::TarGz => extract_tar_gz_tree(archive, dest_root, max_total_bytes, entries, opts),
        Container::Tar => extract_tar_tree(archive, dest_root, max_total_bytes, entries, opts),
        Container::Zip => extract_zip_tree(archive, dest_root, max_total_bytes, entries, opts),
    }
    .map(|_| ())
}

/// The fold a `fold = true` extraction always produces; its absence fails closed.
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
    // `budget` bounds the decompressed bytes the reader pulls for one entry's structure
    // (header, padding, GNU/PAX extension body, read before `write_capped` sees anything),
    // so an extension-header bomb cannot exhaust memory. It is reset per entry and
    // `write_capped` refunds content bytes, so a large file still streams. What each
    // entry's structure cost is charged to the signed structure allowance
    // (`Layer::charge_structure`).
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
    // `tarread` only parses (it has no `unpack`), so every entry is vetted before a byte
    // reaches the disk.
    let mut entries = tar.entries().map_err(map_tar_io)?;
    loop {
        // A fresh per-entry budget (no roll-over); the sum is charged to the structure
        // allowance just below.
        budget.set(TAR_ENTRY_STRUCTURAL_BUDGET);
        // `next_entry()` reads GNU longname/longlink/PAX bodies through the CappedReader;
        // a budget trip here maps to TooLarge.
        let next = entries.next_entry().map_err(map_tar_io)?;
        // Everything pulled since the reset is structure (the previous entry's padding,
        // extension headers, this header): content is read only through
        // `write_capped`/`drain_capped`, which refund it.
        layer.charge_structure(TAR_ENTRY_STRUCTURAL_BUDGET.saturating_sub(budget.get()))?;
        let Some(mut entry) = next else {
            break;
        };
        layer.next_entry()?;
        let raw = entry.path()?.into_owned();
        let kind = classify(entry.header().entry_type());
        let mode = entry.header().mode().unwrap_or(0o644);
        // The effective size, after any PAX `size` record overrode the header field: the
        // raw ustar field could disagree with it, which is what the size guards close.
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
                // An absent link name is an empty target, which the vet refuses as
                // `EmptyPath`, as the zip lane does for an empty body.
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
    /// The permission bits the fold records, masked to `0o7777`:
    /// [`crate::platform::folded_mode`] over an `fstat` of the handle (after `set_mode`).
    /// On Unix that is the mode read back, as the on-disk walk reads it; on Windows, which
    /// stores no POSIX bits, the requested mode (`crate::tree`'s module docs).
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

/// Stream `reader` into `sink`'s freshly opened file with `sink.mode`, decrementing
/// `remaining`; abort with [`ExtractError::TooLarge`] the moment the total would exceed it,
/// so a decompression bomb stops mid-stream.
///
/// With `fold`, the content is hashed inline and the mode read back ([`WrittenFile`]);
/// without it, nothing is hashed and no `fstat` is issued. `buf` is the caller's one
/// reusable copy buffer (see [`Layer`]).
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
        // Refund content bytes to the structural budget: content is bounded by `remaining`,
        // and a file larger than TAR_ENTRY_STRUCTURAL_BUDGET must not trip it.
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
        // `take <= buf.len()` by the clamp above; the `get` restates that bound in a
        // panic-free shape the gate can check.
        let chunk = match buf.get(..take) {
            Some(c) => c,
            None => &buf[..],
        };
        // Hash the exact slice that is written, in order, so the digest describes the
        // file's contents by construction, not by a second reading of it.
        if let Some(hasher) = hasher.as_mut() {
            hasher.update(chunk);
        }
        f.write_all(chunk).map_err(|e| io_at(e, "write", dest))?;
        // Live progress: the one in-process byte loop of an install (one relaxed atomic
        // load per chunk when no `--progress-file` pass is live).
        crate::progress::extract_tick(n);
    }
    // Force the sanitized mode even if umask or a pre-existing file loosened it, through
    // the handle (`fchmod`), not by resolving `dest` again.
    crate::platform::set_mode_on(&f, mode).map_err(|e| io_at(e, "set_mode", dest))?;
    // Read the mode back off the same handle (an `fstat`), through the function the
    // on-disk walk uses; where the inode stores no bits (Windows) `folded_mode` takes the
    // request instead.
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

/// Consume `reader` to EOF without writing, through the same caps as [`write_capped`]:
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

/// Lay one file down from `reader` at `dest` with `mode`, capped at `max_total_bytes`: the
/// `raw-binary` vendor lane's whole extraction, through the archive lanes' write loop and
/// mode read-back, so `bin/<entry>` folds as an archived `bin/<entry>` would.
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
/// exactly one regular file named by one bare component, and its body is laid at `dest`
/// with `mode` (the member's own name is vetted and dropped). The body goes through the
/// structural budget of [`extract_tar_gz_tree`] and the write loop of [`stage_file`], so
/// it folds as a `raw-binary` download of the same bytes would.
///
/// Refused before a byte is written: no member or a name that is not one bare component
/// ([`ExtractReject::NotSingleFile`]), or a non-regular member
/// ([`ExtractReject::DisallowedKind`]). A second member shows only after the first one's
/// body; it is refused the same way and the caller removes what was laid.
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

    /// One member of a test zip. `mode` is the full `st_mode` (type bits included) that
    /// lands in the Unix external attributes; `0` means no Unix attributes at all (the
    /// version-made-by host is then FAT, as a Windows zip would carry).
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

    /// `strip_components` drops leading components after the full path is vetted: `..`
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

    /// The symlink vet: in-root targets (relative to the link's directory) pass; a pop
    /// above the root, a `..` after a normal component, an absolute or an empty target is
    /// refused.
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
        // whatever that component is on disk, possibly a link laid earlier.
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
        // …and the depth is the stripped depth: `top/x -> ../y` would be in-root before
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

    /// [`raw_header_mode`] at mode `0o644`.
    fn raw_header(name: &str, typeflag: u8, linkname: &str, size: usize) -> [u8; 512] {
        raw_header_mode(name, typeflag, linkname, size, 0o644)
    }

    /// [`tar_bytes`] at mode `0o644`, zstd-compressed to `dir/<label>.tar.zst`.
    fn make_archive(dir: &Path, label: &str, entries: &[(&str, u8, &str, &[u8])]) -> PathBuf {
        let moded: Vec<_> = entries
            .iter()
            .map(|(n, tf, l, c)| (*n, *tf, *l, *c, 0o644u32))
            .collect();
        make_archive_moded(dir, label, &moded)
    }

    /// The fold `tree` produced and the on-disk walk that must agree with it: the
    /// declared-mode walk, which on Unix reads modes from the inode and on Windows (which
    /// stores none) uses the fold's declared modes.
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
    /// under umask `000`, implicit (`share/`, `share/doc/`) and explicit (`bin/`)
    /// directories stage at `0755`. With the env unset it does nothing.
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

    /// The directories a folding lane makes because a member's path passes through them
    /// ([`make_dir_exact`]) get `0755` whatever the umask, like a directory member. The
    /// umask is process-wide, so the stage runs in a child of this test binary that runs
    /// only [`probe_stages_its_directories_0755_under_an_open_umask`].
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

    /// The parity gate for the fused digest (§8): the extraction-time root must be the same
    /// 64 hex characters `tree::tree_root` computes over the finished directory, since that
    /// is what signed the row. One archive covers every shape: nested and explicit
    /// directories, an empty file, both mode classes, a multi-chunk file, a hardlink, a
    /// duplicate path, and a duplicate that overwrites a hardlink target.
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
                // Duplicate that overwrites a hardlink target: `bin/cargo` aliases this
                // inode, so the walk sees both names carrying the new bytes.
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
        // Reach guard: the fixture must actually have exercised the shapes.
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

    /// Two members whose names differ in bytes but name one file on the filesystem (a case
    /// pair on case-insensitive APFS or NTFS, NFC and NFD `café` on APFS): the second is
    /// refused (`O_EXCL`); where the filesystem keeps both names, both are laid and the
    /// fold equals the walk. Through the tar and zip lanes.
    #[test]
    fn a_member_the_filesystem_names_like_another_never_replaces_it_unseen() {
        let d = dest("fs-folded-names");
        // `bin/TOOL`, not `BIN/TOOL`: the pair shares its directory, so what refuses it is
        // the file's exclusive create (a folded directory is the next test's).
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

    /// The directory half of the test above: a directory component spelled differently
    /// (`Dir`/`dir`, NFC/NFD `café`, at the top or below a shared parent) that the
    /// filesystem folds onto another is refused as `Occupied` for every member kind; where
    /// the filesystem keeps both spellings, both are laid and the fold equals the walk. The
    /// same members spelled one way always extract. Through the tar and zip lanes.
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

    /// The other side of the parity gate: the fused root moves for exactly the mutations
    /// (content, mode) the on-disk walk moves for.
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
        // permission bits, the fold records the declared mode, so the root moves with the
        // archive's mode class on every platform.
        assert_ne!(base, other_mode, "mode must move the fused root");
    }

    /// The fold's precondition: a destination that already holds something is refused
    /// rather than folded, since a fold over what the extractor wrote would never see a
    /// stranger's file that the on-disk walk hashes.
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

    /// A `tar -C dir -c .` archive opens with a root-naming `./` directory member and names
    /// every path `./…`; the lanes lay the same tree the system tar does, and the strip
    /// spends itself on the leading `.`.
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

        // A root-naming member is a no-op only as a directory: as a file it still names
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

    /// Slip refusals per lane: `..`, an absolute path, and an escaping, absolute or empty
    /// symlink target each abort the gzip and zip lanes with `strip_components` set.
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

    /// A symlink can never redirect a later write: a path through an earlier link, a file
    /// or hardlink landing on a link, and a link landing on an existing path are each
    /// refused, in the tar and zip lanes alike.
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

    /// A hardlink whose target is named through a symlink (`alias -> lnk/f`, `lnk -> real`)
    /// is refused by the vet before the link is made: `lstat` and `link` follow every
    /// component but the last.
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

    /// Two symlinks that each vet in-root cannot add up to one that leaves it:
    /// `a/b/c/up -> ../../..` is the root, and `bin -> a/b/c/up/../OUTSIDE` would walk out
    /// of it. Refused in the tar and zip lanes, in either order.
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
        // The folded mode is the one requested on every platform: read back on Unix,
        // declared on Windows.
        assert_eq!(w.mode, 0o755, "the raw-binary lane folds mode 0755");
        let mut tree = TreeAccumulator::new();
        tree.record_file(b"bin/claude".to_vec(), w.mode, w.content_sha_hex);
        let (fused, walked) = fused_and_walked(tree, &d);
        assert_eq!(fused, walked);
        let err = stage_file(&payload[..], &d.join("capped"), 0o755, 4).unwrap_err();
        assert!(matches!(err, ExtractError::TooLarge(_)), "capped: {err:?}");
    }

    /// A disk failure mid-bundle names the step and the path it was on (EACCES from a
    /// write-protected stage root stands in for a full disk).
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

    /// `write_capped` sets the mode through the handle it holds, not by path: a reader that
    /// unlinks `dest` mid-body leaves no path to resolve, yet the write succeeds.
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
