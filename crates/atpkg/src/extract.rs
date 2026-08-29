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
//! * any non-regular/non-directory/non-link entry type is refused outright;
//! * a SYMLINK is refused outright by the release-bundle lane (`binary`,
//!   `sysroot-bundle`: the `exposes` shims are created by `atpkg` *after* extraction,
//!   never unpacked from the archive). The `https` payload lanes
//!   ([`ExtractOptions::in_root_symlinks`]) admit one ONLY when its target resolves
//!   *lexically inside the stage root* ([`vet_symlink`]), and then refuse any later entry
//!   that would write THROUGH it ([`ExtractReject::ThroughSymlink`] /
//!   [`ExtractReject::Occupied`]) — so a link can never redirect a write, and the tree
//!   the digest describes is the tree on disk;
//! * an in-root HARDLINK is admitted only after [`vet_hardlink`] walks BOTH ends through
//!   the same component vet AND the target is already an extracted regular file — a
//!   toolchain sysroot dedups identical binaries this way (`cargo`↔`targo`,
//!   `trustc`↔`rustc`, shared dylibs), and the materialized alias hashes into `tree_root`
//!   exactly like a regular file; an escaping, absolute, or forward-referencing link
//!   aborts the whole stage;
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

use aterm_digest::Sha256;

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
    /// [`vet_hardlink`]; one that fails that vet lands here or in the path rejections
    /// above.)
    DisallowedKind,
    /// A hardlink whose target had not been extracted by the time the link entry
    /// appeared — a forward/self reference no honest archiver produces.
    HardlinkTargetMissing,
    /// The entry's destination passes THROUGH a symlink laid down earlier in the same
    /// archive: the bytes would land somewhere other than the path the digest records.
    ThroughSymlink,
    /// A symlink entry names a path something already occupies, or a file/directory
    /// entry names a path an earlier symlink occupies. No honest archiver produces
    /// either; both would make the on-disk tree disagree with the folded digest.
    Occupied,
}

/// The kind of an archive entry, as the reader classifies it.
/// [`Regular`](EntryKind::Regular) and [`Directory`](EntryKind::Directory) are
/// extracted after [`vet_entry`]; a [`Hardlink`](EntryKind::Hardlink) is laid
/// down only after the stricter [`vet_hardlink`] (both ends walked, target
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
    /// A hard link. Allowed ONLY when [`vet_hardlink`] proves both the entry
    /// path and the link target resolve inside the store root and the target
    /// is already extracted — a toolchain sysroot dedups identical binaries
    /// this way (`cargo`/`targo`, `trustc`/`rustc`, shared dylibs), and the
    /// materialized file hashes into `tree_root` exactly like a regular file.
    Hardlink,
    /// Any other type (device, fifo, …) — refused.
    Other,
}

/// How an `https`-protocol archive is laid down. The release-bundle lane uses
/// [`ExtractOptions::default`]: nothing stripped, symlinks refused — byte-for-byte the
/// historical behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractOptions {
    /// Leading path components dropped from every entry (GNU tar `--strip-components`):
    /// `gh_2.80.0_macOS_arm64/bin/gh` → `bin/gh` at `1`. An entry with no components
    /// left is skipped, its body drained through the same byte cap. Hardlink targets are
    /// stripped the same way. The FULL path is vetted BEFORE stripping, so `..` above the
    /// strip depth is still a refusal, never a way in.
    pub strip_components: u32,
    /// Admit symlink entries whose target resolves lexically inside the stage root
    /// ([`vet_symlink`]); the digest records them by target bytes
    /// ([`crate::tree::symlink_line`]). Off: every symlink is
    /// [`ExtractReject::DisallowedKind`], exactly as before.
    pub in_root_symlinks: bool,
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
    // `OsStr::is_empty` goes via `call1`: std's INLINED `unsafe` (the `OsStr`
    // byte-slice cast) is otherwise attributed to this function's span as a
    // missing-SAFETY-comment refutation under the strict Trust gate (see
    // `lib.rs`). Same call, same receiver; behavior identical.
    if crate::call1(std::ffi::OsStr::is_empty, rel.as_os_str()) {
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
    if crate::call1(std::ffi::OsStr::is_empty, out.as_os_str()) {
        None
    } else {
        Some(out)
    }
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
    //    a hardlink must come through [`vet_hardlink`], which walks BOTH ends;
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
/// components are dropped. `Ok(None)` when nothing remains — skip the entry.
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
    let Some(rel) = strip_leading(&rel, strip) else {
        return Ok(None);
    };
    join_under(root, &rel).map(Some)
}

/// Vet one HARDLINK entry: the entry path AND the link target must each pass
/// the exact [`vet_entry`] component walk (relative, no `..`, no root/prefix,
/// non-empty, joined-under-root). Returns `(dest, target)` as safe absolute
/// paths. The link target is resolved against the store `root` (tar hardlink
/// names are archive-relative), so a target can never name anything outside
/// the tree being extracted — an escaping hardlink (the slip vector the old
/// blanket refusal guarded against) is still structurally unreachable.
///
/// Pure like [`vet_entry`]; the existence-at-link-time check
/// ([`ExtractReject::HardlinkTargetMissing`]) lives at the extraction site,
/// where the filesystem is in scope.
pub fn vet_hardlink(
    root: &Path,
    raw: &Path,
    link_target: &Path,
) -> Result<(PathBuf, PathBuf), ExtractReject> {
    let dest = vet_entry(root, raw, EntryKind::Regular)?;
    let target = vet_entry(root, link_target, EntryKind::Regular)?;
    Ok((dest, target))
}

/// [`vet_hardlink`] with `strip_components` applied to BOTH ends (GNU tar strips link
/// targets too). `Ok(None)` when the ENTRY strips away; a target that strips away while
/// the entry does not is [`ExtractReject::HardlinkTargetMissing`] — nothing extracted
/// can be its target.
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
/// raced, and a chain of links resolves link by link — each one vetted on its own.
pub fn vet_symlink(
    root: &Path,
    raw: &Path,
    target: &Path,
    strip: u32,
) -> Result<Option<PathBuf>, ExtractReject> {
    let rel = vet_components(raw)?;
    let Some(rel) = strip_leading(&rel, strip) else {
        return Ok(None);
    };
    if crate::call1(std::ffi::OsStr::is_empty, target.as_os_str()) {
        return Err(ExtractReject::EmptyPath);
    }
    if target.is_absolute() {
        return Err(ExtractReject::AbsolutePath);
    }
    // The link lives `depth` directories below the root; the target walks from there.
    let mut depth: usize = rel.components().count().saturating_sub(1);
    for comp in target.components() {
        match comp {
            Component::Normal(_) => depth = depth.saturating_add(1),
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
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
    Io(io::Error),
    /// An entry failed [`vet_entry`] — a tar-slip escape. Carries the offending raw path.
    Rejected(ExtractReject, PathBuf),
    /// The bundle exceeded the caller-supplied uncompressed-size or entry-count cap
    /// (decompression-bomb / tar-bomb defence). The cap comes from the *signed*
    /// `disk_installed`/`size` (§9), never an attacker-chosen header field.
    TooLarge,
}

impl From<io::Error> for ExtractError {
    fn from(e: io::Error) -> Self {
        ExtractError::Io(e)
    }
}

// Hand-rendered through `Formatter::write_str` + direct `Display::fmt`/`Debug::fmt`
// calls (no `write!`): the `write!`/`format_args!` expansion embeds `fmt::Arguments`
// construction (with inlined `unsafe`) that the strict Trust gate cannot lower and
// fails closed on. Byte-identical output (`write!` with `{}`/`{:?}` args performs
// exactly these formatter writes in sequence; no width/fill flags are used).
impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::Io(e) => {
                f.write_str("io: ")?;
                std::fmt::Display::fmt(e, f)
            }
            ExtractError::Rejected(r, p) => {
                f.write_str("rejected entry ")?;
                std::fmt::Debug::fmt(p, f)?;
                f.write_str(": ")?;
                std::fmt::Debug::fmt(r, f)
            }
            ExtractError::TooLarge => f.write_str("bundle exceeded the signed size/entry cap"),
        }
    }
}

impl std::error::Error for ExtractError {}

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
fn safe_mode(entry_mode: u32, is_dir: bool) -> u32 {
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
/// legitimate header/`PATH_MAX` long name / PAX record, yet caps a single extension-body
/// decompression bomb at 1 MiB regardless of the (huge) allowed entry COUNT — the flaw
/// in a budget scaled by `max_entries`, which in production (`MAX_ENTRIES = 4_000_000`)
/// would permit ~32 GiB of slack.
const TAR_ENTRY_STRUCTURAL_BUDGET: u64 = 1 << 20;

/// Marker error the [`CappedReader`] raises when the structural budget is exhausted, so
/// extraction maps it to [`ExtractError::TooLarge`] (not a generic I/O error) no matter
/// WHERE the over-read happens — a directory skip, or a GNU/PAX extension-header body
/// read INSIDE the entries iterator, before per-file write-capping can see it.
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
        // (Branch instead of `usize::try_from(..).unwrap_or(..).min(..)`: same value —
        // `remaining < buf.len() as u64` implies it fits in usize — in a shape whose
        // bounds the strict Trust gate can prove; `try_from` was unlowerable.)
        let len = buf.len();
        let cap = if remaining < len as u64 {
            // Dominated by the branch: `remaining` fits in usize, so the
            // truncating cast is exact (same value `try_from` produced).
            remaining as usize
        } else {
            len
        };
        // No-op re-clamp in the usize domain (`cap <= len` already holds on both
        // branches above): hands the strict Trust gate the dominating bound its
        // slice proof needs without reasoning through the u64 cast.
        let cap = if cap < len { cap } else { len };
        // `Read::read` goes via `call2`: the hardened pass name-matches any direct
        // callee named `read` against the libc `read(2)` FFI-boundary contracts,
        // which do not apply to this safe trait method (see `lib.rs`). Same call,
        // same receiver and buffer; behavior identical.
        let n = crate::call2(<R as Read>::read, &mut self.inner, &mut buf[..cap])?;
        // The `Read` contract guarantees `n <= cap <= remaining`; saturating is a
        // no-op on every conforming reader, and it carries the no-underflow proof
        // the gate refuted (it cannot constrain an external call's return value).
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
        ExtractError::TooLarge
    } else {
        ExtractError::Io(e)
    }
}

/// One node of the staged tree as the digest sees it: a regular file (one per inode) or
/// a symlink (one per path — links share nothing).
#[derive(Debug, Clone)]
enum Node {
    /// `(permission bits as `platform::permission_mode` reports them, masked to 0o7777;
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
///   `Path::join` of the same vetted (and stripped) components (so the separator and
///   the raw OS bytes agree on every platform).
/// * **mode** — read BACK from the file after `set_mode`, through the same
///   `platform::permission_mode` the walk uses, so a filesystem that stores something
///   other than what we asked for (or Windows, which has no POSIX bits and reports 0)
///   moves both digests together.
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
#[derive(Debug)]
pub(crate) struct TreeAccumulator {
    /// relpath bytes → index into `nodes`: WHICH node that path currently names.
    /// A `BTreeMap` because the fold wants the lines sorted anyway and because it makes
    /// the duplicate-path lookup exact rather than best-effort.
    paths: std::collections::BTreeMap<Vec<u8>, usize>,
    /// One entry per distinct node laid down.
    nodes: Vec<Node>,
}

impl TreeAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            paths: std::collections::BTreeMap::new(),
            nodes: Vec::new(),
        }
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

    /// Record a hardlink at `rel` aliasing the already-extracted `target_rel`.
    ///
    /// Fails CLOSED when the target is not one of the regular files this extraction
    /// wrote: the caller has already proved a regular file exists at that path on disk,
    /// so the only way to reach here is a `dest_root` that was not empty when extraction
    /// started — which the staging contract forbids (`verify_and_stage` sweeps and
    /// re-creates it). Guessing a digest for a file we did not write is precisely what
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

/// The raw OS bytes of `path` relative to `root` — the relpath half of a `tree_root`
/// entry line, computed exactly as [`crate::tree::tree_root`]'s walk computes it.
///
/// Both ends are already inside `root` by construction ([`vet_entry`] re-checked the
/// join), so the `strip_prefix` cannot fail; it is mapped to the same fail-closed
/// `RootEscape` rejection anyway rather than unwrapped.
pub(crate) fn rel_bytes_under(root: &Path, path: &Path) -> Result<Vec<u8>, ExtractError> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| ExtractError::Rejected(ExtractReject::RootEscape, path.to_path_buf()))?;
    // `platform::os_str_bytes` goes via `call1` — see `tree.rs`'s walk for why (std's
    // inlined `unsafe` in the `OsStr` byte-slice cast is otherwise attributed here).
    Ok(crate::call1(crate::platform::os_str_bytes, rel.as_os_str()).to_vec())
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
        let mut msg = String::from(
            "extraction destination is not empty; refusing to fold a tree_root \
that would not describe everything under it: ",
        );
        msg.push_str(&crate::call1(
            std::path::Path::to_string_lossy,
            &entry
                .map(|e| e.path())
                .unwrap_or_else(|_| dest_root.to_path_buf()),
        ));
        return Err(ExtractError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            msg,
        )));
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
    /// [`TreeAccumulator`]) — this is the pass `verify_and_stage` no longer has to make
    /// over the payload a second time. `None` for the historical write-only pass.
    tree: Option<TreeAccumulator>,
    /// Whether any symlink has been laid down yet. Until one has, no entry can pass
    /// through or land on a link, so the per-entry `lstat` walks are skipped entirely —
    /// a sysroot bundle with tens of thousands of files pays nothing for a discipline
    /// it never triggers.
    laid_symlink: bool,
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
        }
        Ok(Self {
            root,
            opts,
            remaining: max_total_bytes,
            max_entries,
            count: 0,
            budget,
            copy_buf: vec![0u8; 64 * 1024],
            tree: fold.then(TreeAccumulator::new),
            laid_symlink: false,
        })
    }

    /// Count one more entry against the entry cap.
    fn next_entry(&mut self) -> Result<(), ExtractError> {
        self.count = self.count.saturating_add(1);
        if self.count > self.max_entries {
            return Err(ExtractError::TooLarge);
        }
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

    /// Lay down a directory entry. `declared_size` is the entry's EFFECTIVE body size
    /// — a legitimate directory always declares 0, and a non-zero one is a
    /// decompression-bomb vector the byte cap never sees (the reader skips the body
    /// without writing it), so it is refused on the header alone.
    fn directory(&mut self, raw: &Path, declared_size: u64, mode: u32) -> Result<(), ExtractError> {
        if declared_size != 0 {
            return Err(ExtractError::TooLarge);
        }
        let Some(dest) = vet_entry_stripped(self.root, raw, EntryKind::Directory, self.strip())
            .map_err(|r| ExtractError::Rejected(r, raw.to_path_buf()))?
        else {
            return Ok(());
        };
        self.guard_ancestors(&dest, raw)?;
        self.refuse_if_link(&dest, raw)?;
        std::fs::create_dir_all(&dest)?;
        crate::platform::set_mode(&dest, safe_mode(mode, true))?;
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
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let written = write_capped(
            body,
            &dest,
            safe_mode(mode, false),
            &mut self.remaining,
            &self.budget,
            &mut self.copy_buf,
            self.tree.is_some(),
        )?;
        if let Some(tree) = self.tree.as_mut() {
            let rel = rel_bytes_under(self.root, &dest)?;
            tree.record_file(rel, written.mode, written.content_sha_hex);
        }
        Ok(written.len)
    }

    /// Lay down a hardlink: both ends vetted (and stripped), the target already an
    /// extracted REGULAR FILE, the alias recorded against the target's node.
    fn hardlink(&mut self, raw: &Path, target: &Path) -> Result<(), ExtractError> {
        let Some((dest, target_abs)) = vet_hardlink_stripped(self.root, raw, target, self.strip())
            .map_err(|r| ExtractError::Rejected(r, raw.to_path_buf()))?
        else {
            return Ok(());
        };
        self.guard_ancestors(&dest, raw)?;
        self.refuse_if_link(&dest, raw)?;
        // The target must already be an extracted REGULAR FILE: honest
        // archivers emit the file before its links, and linking to a
        // directory/nothing names no valid bundle. `symlink_metadata` so a
        // symlink at the target is not followed.
        let target_meta = std::fs::symlink_metadata(&target_abs).map_err(|_| {
            ExtractError::Rejected(ExtractReject::HardlinkTargetMissing, raw.to_path_buf())
        })?;
        if !target_meta.is_file() {
            return Err(ExtractError::Rejected(
                ExtractReject::HardlinkTargetMissing,
                raw.to_path_buf(),
            ));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A same-inode alias of already-counted bytes: no content stream,
        // no size-cap charge; the entry COUNT cap still applies.
        std::fs::hard_link(&target_abs, &dest)?;
        // …and, for the digest, a same-NODE alias: it reuses the target's already
        // computed (mode, content sha) instead of re-hashing the same inode through
        // a second name, which is what the on-disk walk had to do.
        let aliased = match self.tree.as_mut() {
            Some(tree) => {
                let dest_rel = rel_bytes_under(self.root, &dest)?;
                let target_rel = rel_bytes_under(self.root, &target_abs)?;
                tree.record_alias(dest_rel, &target_rel)
            }
            None => true,
        };
        if !aliased {
            // Unreachable for a bundle extracted into the empty scratch dir the
            // staging contract provides (the target was proved to be a regular file
            // on disk one statement ago, so it can only be a file THIS extraction
            // wrote). Fail closed rather than invent a digest for bytes we did not
            // write.
            return Err(ExtractError::Rejected(
                ExtractReject::HardlinkTargetMissing,
                raw.to_path_buf(),
            ));
        }
        Ok(())
    }

    /// Lay down a symlink — only where the lane admits them, only in-root
    /// ([`vet_symlink`]), only onto a path nothing occupies, created with the VERBATIM
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
        if !self.opts.in_root_symlinks {
            return Err(ExtractError::Rejected(
                ExtractReject::DisallowedKind,
                raw.to_path_buf(),
            ));
        }
        if declared_size != 0 {
            return Err(ExtractError::TooLarge);
        }
        let Some(dest) = vet_symlink(self.root, raw, target, self.strip())
            .map_err(|r| ExtractError::Rejected(r, raw.to_path_buf()))?
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
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        create_symlink(target, &dest)?;
        if let Some(tree) = self.tree.as_mut() {
            let rel = rel_bytes_under(self.root, &dest)?;
            let target_bytes = crate::call1(crate::platform::os_str_bytes, target.as_os_str());
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
/// Returns `()`; [`extract_tar_zst_rooted`] is the same extraction returning the
/// `tree_root` of what it wrote, which is what the staging path consumes.
pub fn extract_tar_zst(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
) -> Result<(), ExtractError> {
    // `fold = false`: the BYTE-FOR-BYTE historical extraction, with no content hashing
    // and no per-file `fstat`. Keeping this arm genuinely unhashed is not tidiness — it
    // is what lets `examples/stage_reverify_harness.rs` time the old two-pass shape
    // against the new one-pass shape IN ONE BINARY and report a win that is the win.
    // (Fold the digest here too and the harness's "legacy" arm silently pays the new
    // inline SHA-256, which on the shipped `trust` bundle is ~4.7 s of the ~6.0 s second
    // pass — i.e. it would overstate the saving by about 5x.)
    let file = std::fs::File::open(archive)?;
    let decoder = zstd::Decoder::new(file)?;
    extract_stream(
        decoder,
        dest_root,
        max_total_bytes,
        max_entries,
        false,
        ExtractOptions::default(),
    )
    .map(|_| ())
}

/// [`extract_tar_zst`], returning the [`crate::tree::tree_root`] of the tree it just
/// wrote — computed from the bytes as they were written (see the `TreeAccumulator`
/// docs in this module), so the staging path does not have to re-read the whole payload
/// back off disk to learn the same number.
///
/// The digest is over WHAT THE EXTRACTOR WROTE. That is a real semantic difference from
/// walking the finished tree, and it is `verify_and_stage`'s to argue, not this
/// function's: see the re-verify step there.
///
/// Every caller that only wants the bytes on disk keeps using [`extract_tar_zst`]. It
/// still folds the digest — the hashing is the same SHA-256 the old second pass paid,
/// moved rather than added, and the remaining callers are small fixtures/probes.
pub fn extract_tar_zst_rooted(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
) -> Result<String, ExtractError> {
    extract_tar_zst_tree(
        archive,
        dest_root,
        max_total_bytes,
        max_entries,
        ExtractOptions::default(),
    )
    .map(TreeAccumulator::root)
}

/// [`extract_tar_zst_rooted`] with [`ExtractOptions`], handing back the OPEN fold so the
/// stage can record the `links` it creates afterwards before closing the root. The
/// `tar-zst` vendor lane and (with default options) the release-bundle lane.
pub(crate) fn extract_tar_zst_tree(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    opts: ExtractOptions,
) -> Result<TreeAccumulator, ExtractError> {
    let file = std::fs::File::open(archive)?;
    let decoder = zstd::Decoder::new(file)?;
    folded(extract_stream(
        decoder,
        dest_root,
        max_total_bytes,
        max_entries,
        true,
        opts,
    )?)
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
    tree.ok_or_else(|| ExtractError::Io(io::Error::other("extraction produced no tree_root")))
}

/// The one tar extraction body, over an already-decompressed `decoded` stream. `fold`
/// decides whether it also accumulates the [`crate::tree::tree_root`] of what it writes
/// (see [`extract_tar_zst_rooted`]) or performs the historical write-only pass
/// ([`extract_tar_zst`]).
fn extract_stream(
    decoded: impl Read,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    fold: bool,
    opts: ExtractOptions,
) -> Result<Option<TreeAccumulator>, ExtractError> {
    // Bound the decompressed bytes the tar reader pulls for each entry's STRUCTURAL
    // parts (header, padding, GNU/PAX extension-header body — the last read inside
    // `entries()`'s `next()`, BEFORE write_capped can see it), so an extension-header
    // bomb cannot decompress unbounded memory. `budget` is shared with the reader; we
    // reset it per entry (structural reads never accumulate) and `write_capped` refunds
    // file-content bytes (separately bounded by the signed `max_total_bytes` cap), so a
    // legitimate large file still streams while a single entry's structural reads stay
    // under TAR_ENTRY_STRUCTURAL_BUDGET.
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
        // Fresh per-entry structural allowance (does NOT roll over, so a bomb at any
        // entry position is capped at TAR_ENTRY_STRUCTURAL_BUDGET, not that × count).
        budget.set(TAR_ENTRY_STRUCTURAL_BUDGET);
        // `next_entry()` is where the reader resolves GNU longname/longlink/PAX
        // extension headers by reading their body through this same CappedReader — a
        // budget trip here maps to TooLarge.
        let Some(mut entry) = entries.next_entry().map_err(map_tar_io)? else {
            break;
        };
        layer.next_entry()?;
        let raw = entry.path()?.into_owned();
        let kind = classify(entry.header().entry_type());
        let mode = entry.header().mode().unwrap_or(0o644);
        // The EFFECTIVE size — the reader's, after any PAX `size` record overrode the
        // header field. Reading `header()` here would let an `x` record declare 0
        // while the ustar field declares gigabytes (or the reverse), which is the
        // exact reader-disagreement the directory/symlink size guards exist to close.
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
                layer.hardlink(&raw, &target)?;
            }
            EntryKind::Symlink => {
                if !opts.in_root_symlinks {
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
            EntryKind::Other => {
                return Err(ExtractError::Rejected(ExtractReject::DisallowedKind, raw));
            }
        }
    }
    Ok(layer.finish())
}

/// What [`write_capped`] observed about the file it just laid down — the two per-file
/// inputs a `tree_root` entry line needs beside the path, plus its length. The digest
/// fields are placeholders (`0` / `""`) when the caller asked for no fold, and are then
/// never read.
#[derive(Debug)]
pub(crate) struct WrittenFile {
    /// The permission bits as [`crate::platform::permission_mode`] reports them for the
    /// file we just wrote, masked to `0o7777`. READ BACK (an `fstat` on the handle we
    /// still hold, after `set_mode`) rather than assumed to be the requested `mode`, so
    /// this is the same number the on-disk walk would have read — including `0` on
    /// Windows, which has no POSIX bits.
    pub(crate) mode: u32,
    /// Lowercase-hex SHA-256 of the content, accumulated over the SAME 64 KiB chunks the
    /// copy loop writes. Byte-for-byte the digest `tree::file_sha256` would produce by
    /// reading the file back: the same bytes, the same streaming `Sha256`.
    pub(crate) content_sha_hex: String,
    /// Content bytes written.
    pub(crate) len: u64,
}

/// Stream `reader` into a fresh file at `dest` with permission `mode`, decrementing
/// `remaining`; abort with [`ExtractError::TooLarge`] the moment the running total would
/// exceed the cap (so a decompression bomb is stopped mid-stream, never fully written).
///
/// With `fold`, also returns the file's [`WrittenFile`] digest+mode, hashed inline: the
/// bytes are already in the copy buffer, so this is the one pass over the payload the
/// staging path needs. Without it, nothing is hashed and no `fstat` is issued — the
/// historical write-only loop.
///
/// `buf` is the caller's single reusable copy buffer (see [`Layer`]).
fn write_capped(
    mut reader: impl Read,
    dest: &Path,
    mode: u32,
    remaining: &mut u64,
    budget: &Rc<Cell<u64>>,
    buf: &mut [u8],
    fold: bool,
) -> Result<WrittenFile, ExtractError> {
    let mut f = crate::platform::open_create_write(dest, mode)?;
    // One `Option` test per 64 KiB chunk when folding is off — unmeasurable next to the
    // decompress and the write it sits between.
    let mut hasher = fold.then(Sha256::new);
    let mut len: u64 = 0;
    loop {
        // The read flows through the CappedReader (structural budget); map a budget
        // trip to TooLarge rather than a generic I/O error.
        let n = reader.read(&mut *buf).map_err(map_tar_io)?;
        if n == 0 {
            break;
        }
        // The `Read` contract guarantees `n <= buf.len()`; the clamp is a no-op
        // on every conforming reader that hands the strict Trust gate the
        // dominating bound its slice proof needs (the gate cannot constrain an
        // external call's return value).
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
            return Err(ExtractError::TooLarge);
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
        f.write_all(chunk)?;
        // Live-progress meter (R5): this is the ONE in-process byte loop of an
        // install, so the extract phase's honest byte source is exactly here. One
        // relaxed atomic load per 64 KiB chunk when no `--progress-file` pass is
        // live — unmeasurable next to the decompress and the write it sits between.
        crate::progress::extract_tick(n);
    }
    // Force the sanitized mode even if umask or a pre-existing file loosened it.
    crate::platform::set_mode(dest, mode)?;
    // Read the mode BACK, from the handle still open on the file we just wrote (so this
    // is the inode's stored value, not our request), through the very function the
    // on-disk walk uses. `File::metadata` is an `fstat` — no path resolution, nothing
    // to race, and no second open.
    match hasher {
        Some(hasher) => Ok(WrittenFile {
            mode: crate::platform::permission_mode(&f.metadata()?) & 0o7777,
            content_sha_hex: crate::tree::hex(&hasher.finalize()),
            len,
        }),
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
            return Err(ExtractError::TooLarge);
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
    write_capped(reader, dest, mode, &mut remaining, &budget, &mut buf, true)
}

/// Hand-built archive bytes shared by the extraction tests here and the staging tests
/// in `install.rs`: raw USTAR headers (so adversarial names the high-level writers
/// refuse to emit — `../`, symlinks, absolute paths — can be injected), the three
/// compressors, and a minimal ZIP writer with Unix external attributes and an optional
/// ZIP64 rendering.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::io::Write;

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

    pub(crate) fn zstd_bytes(raw: &[u8]) -> Vec<u8> {
        zstd::encode_all(raw, 0).unwrap()
    }

    pub(crate) fn gzip_bytes(raw: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(raw).unwrap();
        enc.finish().unwrap()
    }

    pub(crate) fn deflate_bytes(raw: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(raw).unwrap();
        enc.finish().unwrap()
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

    // Safe relative regular files / dirs resolve to a destination under the root.
    #[test]
    fn accepts_safe_relative_entries() {
        let r = root();
        assert_eq!(
            vet_entry(&r, Path::new("bin/ay"), EntryKind::Regular).unwrap(),
            r.join("bin/ay")
        );
        assert_eq!(
            vet_entry(&r, Path::new("share/doc"), EntryKind::Directory).unwrap(),
            r.join("share/doc")
        );
        // A `.` component is dropped, not an error.
        assert_eq!(
            vet_entry(&r, Path::new("a/./b"), EntryKind::Regular).unwrap(),
            r.join("a/b")
        );
        // Dots WITHIN a name are a normal filename, not traversal.
        assert_eq!(
            vet_entry(&r, Path::new("lib/foo..bar.so"), EntryKind::Regular).unwrap(),
            r.join("lib/foo..bar.so")
        );
    }

    // Every classic tar-slip escape aborts (the §15 "every escape fixture aborts" matrix).
    #[test]
    fn rejects_traversal_and_absolute_paths() {
        let r = root();
        assert_eq!(
            vet_entry(&r, Path::new("../etc/passwd"), EntryKind::Regular),
            Err(ExtractReject::ParentTraversal)
        );
        assert_eq!(
            vet_entry(&r, Path::new("a/../../b"), EntryKind::Regular),
            Err(ExtractReject::ParentTraversal)
        );
        assert_eq!(
            vet_entry(&r, Path::new("/etc/passwd"), EntryKind::Regular),
            Err(ExtractReject::AbsolutePath)
        );
        assert_eq!(
            vet_entry(&r, Path::new("/"), EntryKind::Directory),
            Err(ExtractReject::AbsolutePath)
        );
        // Leading `..` even before a normal segment.
        assert_eq!(
            vet_entry(&r, Path::new(".."), EntryKind::Directory),
            Err(ExtractReject::ParentTraversal)
        );
    }

    // Symlinks, hardlinks, and exotic types are refused outright — regardless of where
    // their (untrusted) target points.
    #[test]
    fn rejects_links_and_exotic_kinds() {
        let r = root();
        // Even a perfectly in-root PATH is refused if the kind is a link.
        for kind in [EntryKind::Symlink, EntryKind::Hardlink, EntryKind::Other] {
            assert_eq!(
                vet_entry(&r, Path::new("bin/ay"), kind),
                Err(ExtractReject::DisallowedKind),
                "{kind:?} must be refused"
            );
        }
    }

    // Hardlinks go through the two-ended walk: both ends in-root passes;
    // an escaping / absolute / traversal TARGET is refused with the same
    // rejections as an escaping entry path (the slip vector stays closed).
    #[test]
    fn hardlink_vet_walks_both_ends() {
        let r = root();
        assert_eq!(
            vet_hardlink(&r, Path::new("bin/cargo"), Path::new("bin/targo")).unwrap(),
            (r.join("bin/cargo"), r.join("bin/targo"))
        );
        assert_eq!(
            vet_hardlink(&r, Path::new("bin/cargo"), Path::new("../outside")),
            Err(ExtractReject::ParentTraversal)
        );
        assert_eq!(
            vet_hardlink(&r, Path::new("bin/cargo"), Path::new("/etc/passwd")),
            Err(ExtractReject::AbsolutePath)
        );
        assert_eq!(
            vet_hardlink(&r, Path::new("../escape"), Path::new("bin/targo")),
            Err(ExtractReject::ParentTraversal)
        );
        assert_eq!(
            vet_hardlink(&r, Path::new("bin/cargo"), Path::new("")),
            Err(ExtractReject::EmptyPath)
        );
    }

    // Empty / dot-only paths name no target → refused.
    #[test]
    fn rejects_empty_and_dot_only_paths() {
        let r = root();
        assert_eq!(
            vet_entry(&r, Path::new(""), EntryKind::Regular),
            Err(ExtractReject::EmptyPath)
        );
        assert_eq!(
            vet_entry(&r, Path::new("."), EntryKind::Directory),
            Err(ExtractReject::EmptyPath)
        );
        assert_eq!(
            vet_entry(&r, Path::new("./././"), EntryKind::Directory),
            Err(ExtractReject::EmptyPath)
        );
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
    /// anything that pops above the root, an absolute target, or an empty one is refused;
    /// the resolution is lexical (a `..` after a `..` that reached the root escapes even
    /// if a later component would come back in).
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

    fn dest(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-ex-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
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

    // A benign bundle extracts every file under the root with sanitized modes + content.
    #[test]
    fn extracts_benign_bundle() {
        let d = dest("benign");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        let archive = make_archive(
            &d,
            "ok",
            &[
                ("bin/ay", b'0', "", b"#!/bin/true\nbinary"),
                ("share/doc/readme", b'0', "", b"hello"),
            ],
        );
        extract_tar_zst(&archive, &root, 10_000_000, 10_000).unwrap();
        assert_eq!(
            std::fs::read(root.join("bin/ay")).unwrap(),
            b"#!/bin/true\nbinary"
        );
        assert_eq!(
            std::fs::read(root.join("share/doc/readme")).unwrap(),
            b"hello"
        );
        // Modes are sanitized to 0644 (no exec bit was set in the header) — Unix-only.
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(root.join("bin/ay"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o644
        );
        // The extracted tree has a stable tree_root.
        assert_eq!(crate::tree::tree_root(&root).unwrap().len(), 64);
        let _ = std::fs::remove_dir_all(&d);
    }

    // An in-root hardlink (typeflag '1') materializes as a same-inode alias of
    // its already-extracted target — the sysroot dedup shape (`cargo`→`targo`,
    // `trustc`→`rustc`) — and the linked tree still yields a stable tree_root.
    #[test]
    fn extracts_in_root_hardlinks() {
        let d = dest("hardlink-ok");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        let archive = make_archive(
            &d,
            "hl",
            &[
                ("bin/targo", b'0', "", b"the one binary"),
                ("bin/cargo", b'1', "bin/targo", b""),
            ],
        );
        extract_tar_zst(&archive, &root, 10_000_000, 10_000).unwrap();
        assert_eq!(
            std::fs::read(root.join("bin/cargo")).unwrap(),
            b"the one binary"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(root.join("bin/cargo")).unwrap().ino(),
                std::fs::metadata(root.join("bin/targo")).unwrap().ino(),
                "hardlink must alias the target inode, not copy it"
            );
        }
        assert_eq!(crate::tree::tree_root(&root).unwrap().len(), 64);
        let _ = std::fs::remove_dir_all(&d);
    }

    // The hardlink escape matrix: a target outside the root, an absolute
    // target, and a forward reference (target not yet extracted) each abort
    // with nothing written outside the root — the §9 fixtures for links.
    #[test]
    fn aborts_on_escaping_or_dangling_hardlinks() {
        for (label, entries) in [
            (
                "hl-escape",
                vec![
                    ("bin/real", b'0', "", b"x".as_slice()),
                    ("bin/evil", b'1', "../../etc/passwd", b"".as_slice()),
                ],
            ),
            (
                "hl-abs",
                vec![("bin/evil", b'1', "/etc/passwd", b"".as_slice())],
            ),
            (
                "hl-forward",
                vec![("bin/evil", b'1', "bin/not-yet", b"".as_slice())],
            ),
        ] {
            let d = dest(label);
            let root = d.join("staging");
            std::fs::create_dir_all(&root).unwrap();
            let archive = make_archive(&d, label, &entries);
            let err = extract_tar_zst(&archive, &root, 10_000_000, 10_000).unwrap_err();
            assert!(
                matches!(err, ExtractError::Rejected(_, _)),
                "{label}: {err:?}"
            );
            assert!(
                !d.join("etc").exists() && !Path::new("/tmp/atpkg-hl-escape-proof").exists(),
                "{label}: nothing may land outside the root"
            );
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    // A `../` traversal entry aborts extraction and never writes outside the root.
    #[test]
    fn aborts_on_traversal_entry() {
        let d = dest("traversal");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        let sentinel = d.join("OUTSIDE"); // sibling of root; must never be created
        let rel_escape = "../OUTSIDE";
        let archive = make_archive(&d, "evil", &[(rel_escape, b'0', "", b"pwned")]);
        let err = extract_tar_zst(&archive, &root, 10_000_000, 10_000).unwrap_err();
        assert!(
            matches!(
                err,
                ExtractError::Rejected(ExtractReject::ParentTraversal, _)
            ),
            "got {err:?}"
        );
        assert!(
            !sentinel.exists(),
            "a traversal entry must not escape the root"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    // A symlink entry aborts extraction in the release-bundle lane (refused outright).
    #[test]
    fn aborts_on_symlink_entry() {
        let d = dest("symlink");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        let archive = make_archive(&d, "lnk", &[("link", b'2', "/etc/passwd", b"")]);
        let err = extract_tar_zst(&archive, &root, 10_000_000, 10_000).unwrap_err();
        assert!(
            matches!(
                err,
                ExtractError::Rejected(ExtractReject::DisallowedKind, _)
            ),
            "got {err:?}"
        );
        assert!(!root.join("link").exists(), "no symlink should be written");
        // …and the same through the rooted entry point with default options.
        let root2 = d.join("staging2");
        let err = extract_tar_zst_rooted(&archive, &root2, 10_000_000, 10_000).unwrap_err();
        assert!(matches!(
            err,
            ExtractError::Rejected(ExtractReject::DisallowedKind, _)
        ));
        let _ = std::fs::remove_dir_all(&d);
    }

    // The uncompressed-size cap (from the SIGNED manifest, §9) aborts a too-large bundle.
    #[test]
    fn aborts_when_over_size_cap() {
        let d = dest("toolarge");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        let archive = make_archive(&d, "big", &[("data", b'0', "", &[b'x'; 4096])]);
        // Cap below the 4096-byte payload ⇒ TooLarge.
        let err = extract_tar_zst(&archive, &root, 1024, 10_000).unwrap_err();
        assert!(matches!(err, ExtractError::TooLarge), "got {err:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    // An explicit directory entry that legitimately declares size 0 still extracts — the
    // declared-size guard must reject no valid bundle.
    #[test]
    fn extracts_bundle_with_explicit_directory_entry() {
        let d = dest("dirok");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        let archive = make_archive(
            &d,
            "dirok",
            &[
                ("share/", b'5', "", b""), // explicit directory entry, declared size 0
                ("share/readme", b'0', "", b"hi"),
            ],
        );
        extract_tar_zst(&archive, &root, 10_000_000, 10_000).unwrap();
        assert!(
            root.join("share").is_dir(),
            "the directory entry should be created"
        );
        assert_eq!(std::fs::read(root.join("share/readme")).unwrap(), b"hi");
        let _ = std::fs::remove_dir_all(&d);
    }

    // A directory entry whose header declares a non-zero `size` is a decompression-bomb
    // vector: the tar reader decompresses that many bytes to skip the entry, yet the size
    // cap counts only bytes WRITTEN (regular files), so the cap is never touched. Refuse
    // it on the header alone — before any body is decompressed — even with a generous cap.
    #[test]
    fn aborts_on_oversized_directory_entry() {
        let d = dest("dirbomb");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        // Build the archive by hand: one directory entry whose header declares 50 MiB but
        // carries no body — the fix rejects on the header, before any body is read.
        let mut tar = Vec::new();
        tar.extend_from_slice(&raw_header("payload/", b'5', "", 50 * 1024 * 1024));
        tar.resize(tar.len() + 1024, 0); // end-of-archive marker
        let path = d.join("dirbomb.tar.zst");
        std::fs::write(&path, zstd_bytes(&tar)).unwrap();
        // A generous 10 MB cap the 50 MiB declared size would blow past only if the bytes
        // were actually decompressed — but we reject on the header alone.
        let err = extract_tar_zst(&path, &root, 10_000_000, 10_000).unwrap_err();
        assert!(matches!(err, ExtractError::TooLarge), "got {err:?}");
        assert!(
            !root.join("payload").exists(),
            "the bomb directory must not be created"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    // A GNU longname ('L') EXTENSION-HEADER body is read INSIDE tar's entries() iterator
    // (before any per-file write cap), so a huge declared size with a highly-compressible
    // body is a decompression bomb the write cap never sees. The global CappedReader must
    // abort it on the extension-header read — bounding memory to the budget, not OOM.
    #[test]
    fn aborts_on_oversized_extension_header_body() {
        let d = dest("extbomb");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        // One GNU longname header declaring an 8 MiB body of NUL bytes (compresses to
        // ~nothing); no following real entry is needed — the bomb is the body read.
        let bomb: usize = 8 * 1024 * 1024;
        let mut tar = Vec::new();
        tar.extend_from_slice(&raw_header("././@LongLink", b'L', "", bomb));
        tar.resize(tar.len() + bomb, 0); // 8 MiB NUL longname body
        let pad = (512 - bomb % 512) % 512;
        tar.resize(tar.len() + pad, 0);
        tar.resize(tar.len() + 1024, 0); // end-of-archive marker
        let path = d.join("extbomb.tar.zst");
        std::fs::write(&path, zstd_bytes(&tar)).unwrap();
        // Small signed cap + few entries ⇒ a tight budget; the 8 MiB extension body,
        // though not a regular-file body, must not be readable past it.
        let err = extract_tar_zst(&path, &root, 64 * 1024, 16).unwrap_err();
        assert!(matches!(err, ExtractError::TooLarge), "got {err:?}");
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
        let fused = extract_tar_zst_rooted(&archive, &root, 10_000_000, 10_000).unwrap();
        let walked = crate::tree::tree_root(&root).unwrap();
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
        let _ = std::fs::remove_dir_all(&d);
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
            let fused = extract_tar_zst_rooted(&archive, &root, 10_000_000, 10_000).unwrap();
            assert_eq!(fused, crate::tree::tree_root(&root).unwrap());
            fused
        }
        let d = dest("fused-sensitive");
        let base = one_file(&d, "a", b"payload", 0o644);
        let other_content = one_file(&d, "b", b"payload!", 0o644);
        let other_mode = one_file(&d, "c", b"payload", 0o755);
        assert_eq!(other_mode.len(), 64);
        assert_ne!(base, other_content, "content must move the fused root");
        // Windows reports no permission bits at all (`permission_mode` = 0), so the mode
        // component is constant there — for BOTH producers, which is the point.
        #[cfg(unix)]
        assert_ne!(base, other_mode, "mode must move the fused root");
        let _ = std::fs::remove_dir_all(&d);
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

        // Empty: extraction proceeds and agrees with the walk, as always.
        let fused = extract_tar_zst_rooted(&archive, &root, 10_000_000, 10_000).unwrap();
        assert_eq!(fused, crate::tree::tree_root(&root).unwrap());

        // …and the SAME call over the now-occupied root is refused, so a second
        // extraction can never fold a root that omits what is already there.
        let err = extract_tar_zst_rooted(&archive, &root, 10_000_000, 10_000).unwrap_err();
        assert!(
            matches!(&err, ExtractError::Io(e) if e.kind() == io::ErrorKind::AlreadyExists),
            "got {err:?}"
        );

        // A stranger's file alone (no re-extraction) is refused just the same — that is
        // the case the walk used to catch and the fold would not have.
        let fresh = d.join("planted");
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join("not-ours"), b"planted").unwrap();
        assert!(extract_tar_zst_rooted(&archive, &fresh, 10_000_000, 10_000).is_err());

        // A destination that does not exist yet is still fine — extraction creates it.
        let absent = d.join("absent");
        assert!(extract_tar_zst_rooted(&archive, &absent, 10_000_000, 10_000).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An EMPTY tree folds to the same root either way (the SHA-256 of nothing), and a
    /// plain `extract_tar_zst` still returns `()` for every existing caller.
    #[test]
    fn an_empty_extraction_agrees_and_the_unit_returning_wrapper_still_works() {
        let d = dest("fused-empty");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        let archive = make_archive(&d, "empty", &[]);
        let fused = extract_tar_zst_rooted(&archive, &root, 10_000_000, 10_000).unwrap();
        assert_eq!(fused, crate::tree::tree_root(&root).unwrap());
        let root2 = d.join("staging2");
        std::fs::create_dir_all(&root2).unwrap();
        extract_tar_zst(&archive, &root2, 10_000_000, 10_000).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    // === the vendor lanes: gzip, zip, strip_components, in-root symlinks ===

    /// The vendor lane options every test below uses: strip one leading component and
    /// admit in-root symlinks — the `gh` archive shape.
    fn vendor(strip: u32) -> ExtractOptions {
        ExtractOptions {
            strip_components: strip,
            in_root_symlinks: true,
        }
    }

    /// The SAME tar bytes through all three compressors and both container readers give
    /// the SAME tree and the SAME fused root, and that root equals the on-disk walk. The
    /// fixture has the `gh` shape: a versioned top-level directory (stripped), an
    /// executable, a plain file, a nested directory, an in-root symlink and a hardlink.
    #[test]
    fn every_vendor_archive_lane_agrees_with_the_walk_and_with_each_other() {
        let d = dest("lanes-parity");
        let entries: &[(&str, u8, &str, &[u8], u32)] = &[
            ("gh_2.80.0_macOS_arm64/", b'5', "", b"", 0o755),
            (
                "gh_2.80.0_macOS_arm64/bin/gh",
                b'0',
                "",
                b"#!/bin/sh\necho gh\n",
                0o755,
            ),
            ("gh_2.80.0_macOS_arm64/LICENSE", b'0', "", b"MIT", 0o644),
            (
                "gh_2.80.0_macOS_arm64/share/man/man1/gh.1",
                b'0',
                "",
                b".TH gh 1",
                0o644,
            ),
            (
                "gh_2.80.0_macOS_arm64/bin/gh-alias",
                b'1',
                "gh_2.80.0_macOS_arm64/bin/gh",
                b"",
                0o755,
            ),
            (
                "gh_2.80.0_macOS_arm64/bin/gh-link",
                b'2',
                "../share/man/man1/gh.1",
                b"",
                0o777,
            ),
        ];
        let tar = tar_bytes(entries);
        let zst = d.join("a.tar.zst");
        std::fs::write(&zst, zstd_bytes(&tar)).unwrap();
        let gz = d.join("a.tar.gz");
        std::fs::write(&gz, gzip_bytes(&tar)).unwrap();
        let zip = d.join("a.zip");
        std::fs::write(
            &zip,
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
                    ZipMember {
                        name: "gh_2.80.0_macOS_arm64/share/man/man1/gh.1",
                        mode: 0o100_644,
                        data: b".TH gh 1",
                        deflate: true,
                    },
                    // zip has no hardlinks: the alias is a second regular file with the
                    // same bytes, which the walk (and the fold) digest identically.
                    ZipMember {
                        name: "gh_2.80.0_macOS_arm64/bin/gh-alias",
                        mode: 0o100_755,
                        data: b"#!/bin/sh\necho gh\n",
                        deflate: false,
                    },
                    ZipMember {
                        name: "gh_2.80.0_macOS_arm64/bin/gh-link",
                        mode: 0o120_777,
                        data: b"../share/man/man1/gh.1",
                        deflate: true,
                    },
                ],
                false,
            ),
        )
        .unwrap();

        let r_zst = d.join("zst");
        let root_zst = extract_tar_zst_tree(&zst, &r_zst, 10_000_000, 10_000, vendor(1))
            .unwrap()
            .root();
        let r_gz = d.join("gz");
        let root_gz = extract_tar_gz_tree(&gz, &r_gz, 10_000_000, 10_000, vendor(1))
            .unwrap()
            .root();
        let r_zip = d.join("zip");
        let root_zip = extract_zip_tree(&zip, &r_zip, 10_000_000, 10_000, vendor(1))
            .unwrap()
            .root();

        for (label, root, fused) in [
            ("zst", &r_zst, &root_zst),
            ("gz", &r_gz, &root_gz),
            ("zip", &r_zip, &root_zip),
        ] {
            assert_eq!(
                fused,
                &crate::tree::tree_root(root).unwrap(),
                "{label}: fold vs walk"
            );
            assert!(
                !root.join("gh_2.80.0_macOS_arm64").exists(),
                "{label}: the top level was stripped"
            );
            assert_eq!(
                std::fs::read(root.join("bin/gh")).unwrap(),
                b"#!/bin/sh\necho gh\n",
                "{label}"
            );
            assert_eq!(
                std::fs::read(root.join("LICENSE")).unwrap(),
                b"MIT",
                "{label}"
            );
            assert_eq!(
                std::fs::read(root.join("bin/gh-alias")).unwrap(),
                b"#!/bin/sh\necho gh\n",
                "{label}"
            );
            #[cfg(unix)]
            {
                let mode_of = |p: &str| {
                    std::fs::metadata(root.join(p))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o7777
                };
                assert_eq!(mode_of("bin/gh"), 0o755, "{label}");
                assert_eq!(mode_of("LICENSE"), 0o644, "{label}");
                assert_eq!(
                    std::fs::read_link(root.join("bin/gh-link")).unwrap(),
                    Path::new("../share/man/man1/gh.1"),
                    "{label}: the link target is verbatim"
                );
                assert_eq!(
                    std::fs::read(root.join("bin/gh-link")).unwrap(),
                    b".TH gh 1",
                    "{label}: and it resolves"
                );
            }
        }
        assert_eq!(root_zst, root_gz, "zstd and gzip are the same tar");
        assert_eq!(root_zst, root_zip, "zip lays down the same tree");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Slip refusals per lane: `..`, an absolute path, an escaping symlink and an
    /// absolute symlink each abort the gzip AND the zip lane with nothing written outside
    /// the root — and with `strip_components` set, so stripping is proven not to be a
    /// way around the vet.
    #[test]
    fn gzip_and_zip_lanes_refuse_every_slip_shape() {
        let d = dest("lanes-slip");
        let cases: &[(&str, &str, u8, &str, ExtractReject)] = &[
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
        ];
        for (label, name, tf, link, want) in cases {
            // A tar symlink carries no body (a body would be the skip-bomb refusal).
            let body: &[u8] = if *tf == b'2' { b"" } else { b"pwned" };
            let tar = tar_bytes(&[
                ("top/ok", b'0', "", b"fine", 0o644),
                (name, *tf, link, body, 0o644),
            ]);
            let gz = d.join(format!("{label}.tar.gz"));
            std::fs::write(&gz, gzip_bytes(&tar)).unwrap();
            let root = d.join(format!("{label}-gz"));
            let err = extract_tar_gz_tree(&gz, &root, 10_000_000, 10_000, vendor(1)).unwrap_err();
            assert!(
                matches!(&err, ExtractError::Rejected(r, _) if r == want),
                "gz {label}: {err:?}"
            );

            let (zmode, zdata): (u32, &[u8]) = if *tf == b'2' {
                (0o120_777, link.as_bytes())
            } else {
                (0o100_644, b"pwned")
            };
            let zip = d.join(format!("{label}.zip"));
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
                            name,
                            mode: zmode,
                            data: zdata,
                            deflate: false,
                        },
                    ],
                    false,
                ),
            )
            .unwrap();
            let root = d.join(format!("{label}-zip"));
            let err = extract_zip_tree(&zip, &root, 10_000_000, 10_000, vendor(1)).unwrap_err();
            assert!(
                matches!(&err, ExtractError::Rejected(r, _) if r == want),
                "zip {label}: {err:?}"
            );
        }
        assert!(
            !d.join("OUTSIDE").exists(),
            "nothing may land outside the roots"
        );
        assert!(!std::env::temp_dir().join("OUTSIDE").exists());
        let _ = std::fs::remove_dir_all(&d);
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
            let gz = d.join(format!("{label}.tar.gz"));
            std::fs::write(&gz, gzip_bytes(&tar_bytes(entries))).unwrap();
            let root = d.join(format!("{label}-gz"));
            let err = extract_tar_gz_tree(&gz, &root, 10_000_000, 10_000, vendor(0)).unwrap_err();
            assert!(
                matches!(&err, ExtractError::Rejected(r, _) if r == want),
                "gz {label}: {err:?}"
            );
            // `real` must still hold its own bytes: nothing wrote through the link.
            if root.join("real").is_file() {
                assert_eq!(std::fs::read(root.join("real")).unwrap(), b"r", "{label}");
            }
            if *label == "hardlink-onto-link" {
                continue; // zip has no hardlinks
            }
            let members: Vec<ZipMember<'_>> = entries
                .iter()
                .map(|(n, tf, l, c, _)| ZipMember {
                    name: n,
                    mode: match tf {
                        b'5' => 0o040_755,
                        b'2' => 0o120_777,
                        _ => 0o100_644,
                    },
                    data: if *tf == b'2' { l.as_bytes() } else { c },
                    deflate: false,
                })
                .collect();
            let zip = d.join(format!("{label}.zip"));
            std::fs::write(&zip, zip_bytes(&members, false)).unwrap();
            let root = d.join(format!("{label}-zip"));
            let err = extract_zip_tree(&zip, &root, 10_000_000, 10_000, vendor(0)).unwrap_err();
            assert!(
                matches!(&err, ExtractError::Rejected(r, _) if r == want),
                "zip {label}: {err:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The caps hold in the new lanes: the byte cap trips on a gzip AND a zip body (a
    /// deflate bomb stops mid-stream), a stripped-away body still counts against it, the
    /// entry cap trips in both, and a tar symlink declaring a body is the skip-bomb the
    /// directory guard already refuses.
    #[test]
    fn gzip_and_zip_lanes_enforce_the_same_caps() {
        let d = dest("lanes-caps");
        let big = vec![b'x'; 4096];
        let tar = tar_bytes(&[("top/data", b'0', "", &big, 0o644)]);
        let gz = d.join("big.tar.gz");
        std::fs::write(&gz, gzip_bytes(&tar)).unwrap();
        let err = extract_tar_gz_tree(&gz, &d.join("gz-cap"), 1024, 10_000, vendor(1)).unwrap_err();
        assert!(
            matches!(err, ExtractError::TooLarge),
            "gz byte cap: {err:?}"
        );
        // Stripped away entirely (strip 2 > depth) — the body is still charged.
        let err =
            extract_tar_gz_tree(&gz, &d.join("gz-strip-cap"), 1024, 10_000, vendor(2)).unwrap_err();
        assert!(
            matches!(err, ExtractError::TooLarge),
            "gz stripped-away body cap: {err:?}"
        );
        // Everything stripped away: nothing is written, the fold is the empty root, and
        // (the stage pre-creates its scratch dir) the walk over the empty dir agrees.
        std::fs::create_dir_all(d.join("gz-strip-ok")).unwrap();
        let ok = extract_tar_gz_tree(&gz, &d.join("gz-strip-ok"), 8192, 10_000, vendor(2)).unwrap();
        assert!(
            !d.join("gz-strip-ok/data").exists(),
            "stripped away means not written"
        );
        assert_eq!(
            ok.root(),
            crate::tree::tree_root(&d.join("gz-strip-ok")).unwrap()
        );

        let zip = d.join("big.zip");
        std::fs::write(
            &zip,
            zip_bytes(
                &[ZipMember {
                    name: "top/data",
                    mode: 0o100_644,
                    data: &big,
                    deflate: true,
                }],
                false,
            ),
        )
        .unwrap();
        let err = extract_zip_tree(&zip, &d.join("zip-cap"), 1024, 10_000, vendor(1)).unwrap_err();
        assert!(
            matches!(err, ExtractError::TooLarge),
            "zip byte cap: {err:?}"
        );
        let err =
            extract_zip_tree(&zip, &d.join("zip-strip-cap"), 1024, 10_000, vendor(2)).unwrap_err();
        assert!(
            matches!(err, ExtractError::TooLarge),
            "zip stripped-away body cap: {err:?}"
        );

        // Entry cap: two entries, cap of one.
        let two = tar_bytes(&[("a", b'0', "", b"1", 0o644), ("b", b'0', "", b"2", 0o644)]);
        let gz2 = d.join("two.tar.gz");
        std::fs::write(&gz2, gzip_bytes(&two)).unwrap();
        let err =
            extract_tar_gz_tree(&gz2, &d.join("gz-entries"), 10_000, 1, vendor(0)).unwrap_err();
        assert!(
            matches!(err, ExtractError::TooLarge),
            "gz entry cap: {err:?}"
        );
        let zip2 = d.join("two.zip");
        std::fs::write(
            &zip2,
            zip_bytes(
                &[
                    ZipMember {
                        name: "a",
                        mode: 0o100_644,
                        data: b"1",
                        deflate: false,
                    },
                    ZipMember {
                        name: "b",
                        mode: 0o100_644,
                        data: b"2",
                        deflate: false,
                    },
                ],
                false,
            ),
        )
        .unwrap();
        let err =
            extract_zip_tree(&zip2, &d.join("zip-entries"), 10_000, 1, vendor(0)).unwrap_err();
        assert!(
            matches!(err, ExtractError::TooLarge),
            "zip entry cap: {err:?}"
        );

        // A tar symlink declaring a body is refused on the header, like a directory.
        let mut bomb = Vec::new();
        bomb.extend_from_slice(&raw_header_mode(
            "lnk",
            b'2',
            "target",
            50 * 1024 * 1024,
            0o777,
        ));
        bomb.resize(bomb.len() + 1024, 0);
        let gz3 = d.join("linkbomb.tar.gz");
        std::fs::write(&gz3, gzip_bytes(&bomb)).unwrap();
        let err = extract_tar_gz_tree(&gz3, &d.join("gz-linkbomb"), 10_000_000, 10, vendor(0))
            .unwrap_err();
        assert!(
            matches!(err, ExtractError::TooLarge),
            "symlink body bomb: {err:?}"
        );
        // A zip symlink whose target is implausibly long is refused before it is read.
        let long = vec![b'a'; 5000];
        let zip3 = d.join("longlink.zip");
        std::fs::write(
            &zip3,
            zip_bytes(
                &[ZipMember {
                    name: "lnk",
                    mode: 0o120_777,
                    data: &long,
                    deflate: false,
                }],
                false,
            ),
        )
        .unwrap();
        let err = extract_zip_tree(&zip3, &d.join("zip-longlink"), 10_000_000, 10, vendor(0))
            .unwrap_err();
        assert!(
            matches!(err, ExtractError::TooLarge),
            "zip long link: {err:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// What the zip reader refuses on its own format rules — each fail-closed, none a
    /// panic: encryption, an unsupported method, a backslash in a name, a truncated file,
    /// a body that disagrees with its declared size, trailing garbage, and a non-Unix
    /// member whose attributes carry no mode (laid down `0644`, never executable). And the
    /// ZIP64 rendering of a perfectly ordinary archive extracts identically.
    #[test]
    fn the_zip_reader_refuses_what_it_does_not_understand_and_reads_zip64() {
        let d = dest("zip-format");
        let plain = zip_bytes(
            &[
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
            ],
            false,
        );
        let z64 = zip_bytes(
            &[
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
            ],
            true,
        );
        assert_ne!(plain, z64, "the fixture must really render two encodings");
        let p = d.join("plain.zip");
        std::fs::write(&p, &plain).unwrap();
        let q = d.join("z64.zip");
        std::fs::write(&q, &z64).unwrap();
        let root_p = extract_zip_tree(&p, &d.join("plain"), 10_000, 10, vendor(0))
            .unwrap()
            .root();
        let root_q = extract_zip_tree(&q, &d.join("z64"), 10_000, 10, vendor(0))
            .unwrap()
            .root();
        assert_eq!(root_p, root_q, "ZIP64 is an encoding, not a different tree");
        assert_eq!(root_p, crate::tree::tree_root(&d.join("plain")).unwrap());

        let refuse = |label: &str, bytes: &[u8]| {
            let path = d.join(format!("{label}.zip"));
            std::fs::write(&path, bytes).unwrap();
            let err = extract_zip_tree(&path, &d.join(label), 10_000, 10, vendor(0)).unwrap_err();
            assert!(matches!(err, ExtractError::Io(_)), "{label}: {err:?}");
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
        let _ = std::fs::remove_dir_all(&d);
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
        assert_eq!(w.len, payload.len() as u64);
        assert_eq!(std::fs::read(&dest_path).unwrap(), payload);
        let mut tree = TreeAccumulator::new();
        tree.record_file(b"bin/claude".to_vec(), w.mode, w.content_sha_hex);
        assert_eq!(tree.root(), crate::tree::tree_root(&d).unwrap());
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&dest_path).unwrap().permissions().mode() & 0o7777,
            0o755
        );
        // Capped.
        let err = stage_file(&payload[..], &d.join("capped"), 0o755, 4).unwrap_err();
        assert!(matches!(err, ExtractError::TooLarge));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A symlink recorded AFTER extraction (the stage's `links`) folds to the same root
    /// the walk reads over the finished tree — the fold is open until the stage closes it.
    #[cfg(unix)]
    #[test]
    fn a_symlink_recorded_after_extraction_folds_like_the_walk() {
        let d = dest("late-link");
        let gz = d.join("a.tar.gz");
        std::fs::write(
            &gz,
            gzip_bytes(&tar_bytes(&[(
                "Foo.app/Contents/MacOS/foo",
                b'0',
                "",
                b"#!/bin/sh\n",
                0o755,
            )])),
        )
        .unwrap();
        let root = d.join("stage");
        let mut tree = extract_tar_gz_tree(&gz, &root, 10_000, 10, vendor(0)).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        create_symlink(
            Path::new("../Foo.app/Contents/MacOS/foo"),
            &root.join("bin/foo"),
        )
        .unwrap();
        tree.record_symlink(b"bin/foo".to_vec(), b"../Foo.app/Contents/MacOS/foo");
        assert_eq!(tree.root(), crate::tree::tree_root(&root).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }
}
