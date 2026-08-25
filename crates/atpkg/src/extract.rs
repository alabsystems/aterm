// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tar-slip-safe extraction vetting (§4.2/§15) — the fail-closed decision for whether a
//! single archive entry may be written under the per-program store root.
//!
//! A `.tar.zst` bundle is attacker-influenced input (it is only signature-verified as a
//! *compressed blob*; the extracted layout is re-checked against `tree_root` afterwards,
//! §8). The classic tar-slip escapes — an absolute path, a `..` traversal, or a
//! symlink/hardlink that redirects a later write outside the store — must each abort the
//! WHOLE staged group, never partially extract. This module is the pure, dependency-free
//! core of that defence: [`vet_entry`] turns a `(raw path, kind)` pair into either a
//! **safe absolute destination under the root** or a [`ExtractReject`]. The tar reader
//! (a later increment) feeds every entry through it before writing a single byte.
//!
//! **Fail closed by construction:**
//! * absolute paths, `..` components, and root/prefix components are refused;
//! * symlinks and any non-regular/non-directory/non-hardlink entry type are refused
//!   outright — a symlink is both the classic slip vector and a TOCTOU hazard, and the
//!   `exposes` shims are created by `atpkg` *after* extraction, never unpacked from the
//!   archive;
//! * an in-root HARDLINK is admitted only after [`vet_hardlink`] walks BOTH ends through
//!   the same component vet AND the target is already an extracted regular file — a
//!   toolchain sysroot dedups identical binaries this way (`cargo`↔`targo`,
//!   `trustc`↔`rustc`, shared dylibs), and the materialized alias hashes into `tree_root`
//!   exactly like a regular file; an escaping, absolute, or forward-referencing link
//!   aborts the whole stage;
//! * the validated path is re-joined to the root and confirmed to still be under it
//!   (defence in depth against any normalization surprise).

use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use aterm_digest::Sha256;

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
    /// The lexically-joined destination escaped the store root (belt-and-suspenders).
    RootEscape,
    /// A symlink or any non-regular/non-directory/non-hardlink entry — refused
    /// outright. (An in-root hardlink is separately vetted by [`vet_hardlink`];
    /// one that fails that vet lands here or in the path rejections above.)
    DisallowedKind,
    /// A hardlink whose target had not been extracted by the time the link entry
    /// appeared — a forward/self reference no honest archiver produces.
    HardlinkTargetMissing,
}

/// The kind of a tar entry, as the tar reader classifies it.
/// [`Regular`](EntryKind::Regular) and [`Directory`](EntryKind::Directory) are
/// extracted after [`vet_entry`]; a [`Hardlink`](EntryKind::Hardlink) is laid
/// down only after the stricter [`vet_hardlink`] (both ends walked, target
/// already extracted, in-root); everything else is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    Regular,
    /// A directory.
    Directory,
    /// A symbolic link (refused — slip/TOCTOU vector; and the signed
    /// `tree_root` is symlink-free by contract).
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
    //    a hardlink must come through [`vet_hardlink`], which walks BOTH ends.
    match kind {
        EntryKind::Regular | EntryKind::Directory => {}
        EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
            return Err(ExtractReject::DisallowedKind);
        }
    }
    // 2. An absolute entry path can never be made relative to the root.
    if raw.is_absolute() {
        return Err(ExtractReject::AbsolutePath);
    }
    // 3. Component-walk: accept ONLY `Normal` segments. `..` is traversal; a root/prefix
    //    component is an absolute escape; `.` is dropped. This distinguishes a `..`
    //    *component* from a filename that merely contains dots (`foo..bar` stays a Normal
    //    component, correctly allowed).
    let mut rel = PathBuf::new();
    for comp in raw.components() {
        match comp {
            Component::Normal(c) => rel.push(c),
            Component::CurDir => {}
            Component::ParentDir => return Err(ExtractReject::ParentTraversal),
            Component::RootDir | Component::Prefix(_) => return Err(ExtractReject::AbsolutePath),
        }
    }
    // 4. A path that was empty, or only `.`/separators, names no target.
    // `OsStr::is_empty` goes via `call1`: std's INLINED `unsafe` (the `OsStr`
    // byte-slice cast) is otherwise attributed to this function's span as a
    // missing-SAFETY-comment refutation under the strict Trust gate (see
    // `lib.rs`). Same call, same receiver; behavior identical.
    if crate::call1(std::ffi::OsStr::is_empty, rel.as_os_str()) {
        return Err(ExtractReject::EmptyPath);
    }
    // 5. Defence in depth: the join must still be under the root.
    let dest = root.join(&rel);
    if !dest.starts_with(root) {
        return Err(ExtractReject::RootEscape);
    }
    Ok(dest)
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

/// A failure while extracting a `.tar.zst` bundle. Any variant aborts the WHOLE stage —
/// the caller removes the (partial) `dest_root` so a half-extracted tree never activates.
#[derive(Debug)]
pub enum ExtractError {
    /// An I/O / decompression / tar-format error.
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

/// Map a `tar` entry type to our [`EntryKind`]; anything that is not a plain file or
/// directory is treated as a disallowed kind (refused by [`vet_entry`]).
fn classify(ty: tar::EntryType) -> EntryKind {
    match ty {
        tar::EntryType::Regular | tar::EntryType::Continuous => EntryKind::Regular,
        tar::EntryType::Directory => EntryKind::Directory,
        tar::EntryType::Symlink => EntryKind::Symlink,
        tar::EntryType::Link => EntryKind::Hardlink,
        _ => EntryKind::Other,
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
/// each entry (see [`extract_tar_zst`]) so it never accumulates, and refunded for file
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

/// Wraps the zstd decoder and bounds every byte the tar reader pulls against a shared,
/// per-entry-reset budget. This catches the GNU longname/longlink/PAX extension-header
/// bodies the `tar` crate reads INSIDE its `entries()` iterator, before per-file
/// write-capping can ever see them — without it a single extension header declaring a
/// huge size (up to 2^64) with a highly-compressible body is a decompression bomb that
/// [`write_capped`]'s per-file cap never bounds. File-content reads are refunded by
/// `write_capped` (they are separately bounded by the signed content cap), so the
/// budget constrains only structural bytes and a legitimate multi-GiB file still streams.
struct CappedReader<R> {
    inner: R,
    budget: std::rc::Rc<std::cell::Cell<u64>>,
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
/// The line format and the fold are not reimplemented here — [`crate::tree::entry_line`]
/// and [`crate::tree::root_of_entry_lines`] are the single source of both, shared with
/// the on-disk walk. What this type has to get right is only the three INPUTS per file,
/// and it models the filesystem the walk would have observed rather than the tar stream:
///
/// * **relpath** — `dest.strip_prefix(dest_root)`, i.e. exactly what
///   [`crate::tree::tree_root`]'s walk computes for the same file, built by the same
///   `Path::join` of the same vetted components (so the separator and the raw OS bytes
///   agree on every platform).
/// * **mode** — read BACK from the file after `set_mode`, through the same
///   `platform::permission_mode` the walk uses, so a filesystem that stores something
///   other than what we asked for (or Windows, which has no POSIX bits and reports 0)
///   moves both digests together.
/// * **content digest** — hashed chunk by chunk as [`write_capped`] writes it.
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
/// place to be approximately right.
struct TreeAccumulator {
    /// relpath bytes → index into `nodes`: WHICH inode that path currently names.
    /// A `BTreeMap` because the fold wants the lines sorted anyway and because it makes
    /// the duplicate-path lookup exact rather than best-effort.
    paths: std::collections::BTreeMap<Vec<u8>, usize>,
    /// One entry per distinct inode laid down: `(permission bits as
    /// `platform::permission_mode` reports them, masked to 0o7777; lowercase-hex
    /// content sha256)`.
    nodes: Vec<(u32, String)>,
}

impl TreeAccumulator {
    fn new() -> Self {
        Self {
            paths: std::collections::BTreeMap::new(),
            nodes: Vec::new(),
        }
    }

    /// Record a regular file written at `rel`. A path written twice re-uses its node —
    /// the second `open(O_TRUNC)` lands on the same inode, so every alias of it observes
    /// the new content and the new mode, which is exactly what the on-disk walk would
    /// report.
    fn record_file(&mut self, rel: Vec<u8>, mode: u32, content_sha_hex: String) {
        // `copied()` first so the `paths` borrow is over before `nodes` is taken
        // mutably — the two are disjoint fields, but not relying on that keeps the
        // borrow shape obvious to a reader as well as to the compiler.
        let existing = self.paths.get(&rel).copied();
        if let Some(node) = existing
            && let Some(slot) = self.nodes.get_mut(node)
        {
            *slot = (mode, content_sha_hex);
            return;
        }
        self.nodes.push((mode, content_sha_hex));
        // `len() - 1` is the index just pushed; spelled saturating so the index
        // arithmetic carries no panic obligation (the `push` above makes `len() >= 1`).
        let node = self.nodes.len().saturating_sub(1);
        self.paths.insert(rel, node);
    }

    /// Record a hardlink at `rel` aliasing the already-extracted `target_rel`.
    ///
    /// Fails CLOSED when the target is not one of the files this extraction wrote: the
    /// caller has already proved a regular file exists at that path on disk, so the only
    /// way to reach here is a `dest_root` that was not empty when extraction started —
    /// which the staging contract forbids (`verify_and_stage` sweeps and re-creates it).
    /// Guessing a digest for a file we did not write is precisely what this digest is
    /// supposed to refuse.
    fn record_alias(&mut self, rel: Vec<u8>, target_rel: &[u8]) -> bool {
        let Some(&node) = self.paths.get(target_rel) else {
            return false;
        };
        self.paths.insert(rel, node);
        true
    }

    /// Fold every recorded path into the tree root, through the SAME formatter and the
    /// SAME sort+SHA-256 the on-disk walk uses.
    fn root(self) -> String {
        let mut lines: Vec<Vec<u8>> = Vec::with_capacity(self.paths.len());
        for (rel, node) in &self.paths {
            // A node index always came from `self.nodes`, so the `get` never misses;
            // skipping a miss (rather than indexing) keeps the fold panic-free, and a
            // skipped line could only ever produce a MISMATCHING root, i.e. fail closed.
            if let Some((mode, sha)) = self.nodes.get(*node) {
                lines.push(crate::tree::entry_line(rel, *mode, sha));
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
fn rel_bytes_under(root: &Path, path: &Path) -> Result<Vec<u8>, ExtractError> {
    let rel = path.strip_prefix(root).map_err(|_| {
        ExtractError::Rejected(ExtractReject::RootEscape, path.to_path_buf())
    })?;
    // `platform::os_str_bytes` goes via `call1` — see `tree.rs`'s walk for why (std's
    // inlined `unsafe` in the `OsStr` byte-slice cast is otherwise attributed here).
    Ok(crate::call1(crate::platform::os_str_bytes, rel.as_os_str()).to_vec())
}

/// Extract a `.tar.zst` bundle at `archive` into `dest_root`, with **every entry vetted
/// before a byte is written** ([`vet_entry`]). On the first rejected entry — or on
/// exceeding `max_total_bytes` / `max_entries` — extraction aborts with an
/// [`ExtractError`]; the caller then removes the partial `dest_root` (a half-extracted
/// bundle must never activate, §7). Symlinks/hardlinks/exotic entries are refused, modes
/// are sanitized (no setuid/setgid/sticky, no group/other-write; executables `0o755`,
/// else `0o644`), and the uncompressed size is capped (the cap is derived from the
/// *signed* manifest, never a header field).
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
    extract_inner(archive, dest_root, max_total_bytes, max_entries, false).map(|_| ())
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
    match extract_inner(archive, dest_root, max_total_bytes, max_entries, true)? {
        Some(root) => Ok(root),
        // Unreachable: `fold = true` always builds an accumulator. Fail CLOSED rather
        // than substitute a default — an empty string here would be compared against the
        // signed `tree_root` and refused anyway, but saying so is better than relying on
        // that.
        None => Err(ExtractError::Io(io::Error::other(
            "extraction produced no tree_root",
        ))),
    }
}

/// The one extraction body. `fold` decides whether it also accumulates the
/// [`crate::tree::tree_root`] of what it writes (see [`extract_tar_zst_rooted`]) or
/// performs the historical write-only pass ([`extract_tar_zst`]).
fn extract_inner(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    fold: bool,
) -> Result<Option<String>, ExtractError> {
    // THE precondition the folded digest stands on, ENFORCED rather than assumed.
    //
    // The accumulator describes the files this extraction WROTE; the on-disk walk it
    // replaces described every regular file under `dest_root`. Those are the same set
    // only while `dest_root` starts empty — which is what the staging contract provides
    // (`verify_and_stage` sweeps the scratch siblings and re-creates `incoming` under
    // the store lock) and what `TreeAccumulator::record_alias` already relies on to
    // refuse a hardlink onto a file it did not write. If a stranger's file were ever
    // present, the walk would have hashed it into a MISMATCHING root (fail closed) while
    // the fold would silently ignore it (fail OPEN), which is the one direction a
    // re-verify may never move. One `read_dir` per stage buys that back.
    //
    // A `dest_root` that does not exist yet is fine — extraction creates it — and is the
    // shape several in-crate probes use.
    if fold
        && let Ok(mut existing) = std::fs::read_dir(dest_root)
        && let Some(entry) = existing.next()
    {
        let mut msg =
            String::from("extraction destination is not empty; refusing to fold a tree_root \
that would not describe everything under it: ");
        msg.push_str(&crate::call1(
            std::path::Path::to_string_lossy,
            &entry.map(|e| e.path()).unwrap_or_else(|_| dest_root.to_path_buf()),
        ));
        return Err(ExtractError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            msg,
        )));
    }
    let file = std::fs::File::open(archive)?;
    let decoder = zstd::Decoder::new(file)?;
    // Bound the decompressed bytes the tar reader pulls for each entry's STRUCTURAL
    // parts (header, padding, GNU/PAX extension-header body — the last read inside
    // `entries()`'s `next()`, BEFORE write_capped can see it), so an extension-header
    // bomb cannot decompress unbounded memory. `budget` is shared with the reader; we
    // reset it per entry (structural reads never accumulate) and `write_capped` refunds
    // file-content bytes (separately bounded by the signed `max_total_bytes` cap), so a
    // legitimate large file still streams while a single entry's structural reads stay
    // under TAR_ENTRY_STRUCTURAL_BUDGET.
    let budget = std::rc::Rc::new(std::cell::Cell::new(TAR_ENTRY_STRUCTURAL_BUDGET));
    let mut tar = tar::Archive::new(CappedReader {
        inner: decoder,
        budget: std::rc::Rc::clone(&budget),
    });
    // We drive extraction ourselves — never tar's `unpack` — so every entry is vetted.
    let mut remaining = max_total_bytes;
    // ONE copy buffer for the whole archive. A `[0u8; 64 * 1024]` local inside
    // `write_capped` would be zero-initialized per ENTRY and LLVM cannot elide it (the
    // buffer goes to an opaque `Read::read`), so every unpacked file paid a 16-page stack
    // probe plus a 64 KiB `bzero` on top of the zstd decode and the write — gigabyte-scale
    // memset across a real toolchain bundle. Heap `vec!`, not a boxed array literal:
    // `Box::new([0u8; N])` materializes the array on the stack first.
    let mut copy_buf = vec![0u8; 64 * 1024];
    let mut count: u64 = 0;
    // The tree_root of what we are about to write, folded as we write it (see
    // [`TreeAccumulator`]) — this is the pass `verify_and_stage` no longer has to make
    // over the payload a second time.
    let mut tree = fold.then(TreeAccumulator::new);
    let mut entries = tar.entries().map_err(map_tar_io)?;
    loop {
        // Fresh per-entry structural allowance (does NOT roll over, so a bomb at any
        // entry position is capped at TAR_ENTRY_STRUCTURAL_BUDGET, not that × count).
        budget.set(TAR_ENTRY_STRUCTURAL_BUDGET);
        // `next()` is where the tar crate resolves GNU longname/longlink/PAX extension
        // headers by reading their body — a budget trip here maps to TooLarge.
        let Some(entry) = entries.next() else { break };
        let mut entry = entry.map_err(map_tar_io)?;
        count += 1;
        if count > max_entries {
            return Err(ExtractError::TooLarge);
        }
        let raw = entry.path()?.into_owned();
        let kind = classify(entry.header().entry_type());
        // A hardlink is vetted by its own two-ended walk (below), not vet_entry.
        if kind == EntryKind::Hardlink {
            let target = entry
                .link_name()
                .map_err(map_tar_io)?
                .ok_or_else(|| ExtractError::Rejected(ExtractReject::DisallowedKind, raw.clone()))?
                .into_owned();
            let (dest, target_abs) = vet_hardlink(dest_root, &raw, &target)
                .map_err(|r| ExtractError::Rejected(r, raw.clone()))?;
            // The target must already be an extracted REGULAR FILE: honest
            // archivers emit the file before its links, and linking to a
            // directory/nothing names no valid bundle. `symlink_metadata` so a
            // (impossible-by-construction, but belt-and-suspenders) symlink at
            // the target is not followed.
            let target_meta = std::fs::symlink_metadata(&target_abs).map_err(|_| {
                ExtractError::Rejected(ExtractReject::HardlinkTargetMissing, raw.clone())
            })?;
            if !target_meta.is_file() {
                return Err(ExtractError::Rejected(
                    ExtractReject::HardlinkTargetMissing,
                    raw,
                ));
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // A same-inode alias of already-counted bytes: no content stream,
            // no size-cap charge; the entry COUNT cap above still applies.
            std::fs::hard_link(&target_abs, &dest)?;
            // …and, for the digest, a same-NODE alias: it reuses the target's already
            // computed (mode, content sha) instead of re-hashing the same inode through
            // a second name, which is what the on-disk walk had to do.
            let aliased = match tree.as_mut() {
                Some(tree) => {
                    let dest_rel = rel_bytes_under(dest_root, &dest)?;
                    let target_rel = rel_bytes_under(dest_root, &target_abs)?;
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
                    raw,
                ));
            }
            continue;
        }
        let dest =
            vet_entry(dest_root, &raw, kind).map_err(|r| ExtractError::Rejected(r, raw.clone()))?;
        let mode = entry.header().mode().unwrap_or(0o644);
        match kind {
            EntryKind::Directory => {
                // The size cap is enforced only against bytes WRITTEN (write_capped, for
                // regular files). A directory entry is never written through write_capped,
                // yet the tar reader still decompresses that entry's declared `size` to
                // skip past its (absent) body on the next iteration — an uncapped
                // decompression-bomb vector if the attacker-chosen header declares a huge
                // size. A legitimate directory always declares size 0, so a non-zero
                // declared size names no valid bundle: refuse it fail-closed.
                let declared = entry.header().entry_size().unwrap_or(0);
                if declared != 0 {
                    return Err(ExtractError::TooLarge);
                }
                std::fs::create_dir_all(&dest)?;
                crate::platform::set_mode(&dest, safe_mode(mode, true))?;
            }
            EntryKind::Regular => {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let written = write_capped(
                    &mut entry,
                    &dest,
                    safe_mode(mode, false),
                    &mut remaining,
                    &budget,
                    &mut copy_buf,
                    fold,
                )?;
                if let Some(tree) = tree.as_mut() {
                    let rel = rel_bytes_under(dest_root, &dest)?;
                    tree.record_file(rel, written.mode, written.content_sha_hex);
                }
            }
            // vet_entry already refused these (hardlinks took their own arm
            // above); unreachable but kept fail-closed.
            EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
                return Err(ExtractError::Rejected(ExtractReject::DisallowedKind, raw));
            }
        }
    }
    Ok(tree.map(TreeAccumulator::root))
}

/// What [`write_capped`] observed about the file it just laid down — the two per-file
/// inputs a `tree_root` entry line needs beside the path. Both are placeholders
/// (`0` / `""`) when the caller asked for no fold, and are then never read.
struct WrittenFile {
    /// The permission bits as [`crate::platform::permission_mode`] reports them for the
    /// file we just wrote, masked to `0o7777`. READ BACK (an `fstat` on the handle we
    /// still hold, after `set_mode`) rather than assumed to be the requested `mode`, so
    /// this is the same number the on-disk walk would have read — including `0` on
    /// Windows, which has no POSIX bits.
    mode: u32,
    /// Lowercase-hex SHA-256 of the content, accumulated over the SAME 64 KiB chunks the
    /// copy loop writes. Byte-for-byte the digest `tree::file_sha256` would produce by
    /// reading the file back: the same bytes, the same streaming `Sha256`.
    content_sha_hex: String,
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
/// `buf` is the caller's single reusable copy buffer (see [`extract_tar_zst`]).
fn write_capped(
    mut reader: impl Read,
    dest: &Path,
    mode: u32,
    remaining: &mut u64,
    budget: &std::rc::Rc<std::cell::Cell<u64>>,
    buf: &mut [u8],
    fold: bool,
) -> Result<WrittenFile, ExtractError> {
    let mut f = crate::platform::open_create_write(dest, mode)?;
    // One `Option` test per 64 KiB chunk when folding is off — unmeasurable next to the
    // decompress and the write it sits between.
    let mut hasher = fold.then(Sha256::new);
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
        }),
        // Not folding: neither field is read, so neither is paid for.
        None => Ok(WrittenFile {
            mode: 0,
            content_sha_hex: String::new(),
        }),
    }
}

#[cfg(test)]
mod tests {
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

    /// [`raw_header`] with an explicit archive mode, so the parity fixtures can carry
    /// the EXECUTABLE bit (`safe_mode` maps any `x` to `0o755`) — the mode component of
    /// a `tree_root` entry line is otherwise never exercised with two distinct values.
    /// `raw_header` delegates here with `0o644`, which renders `"0000644\0"`: the
    /// header bytes every existing fixture sees are unchanged.
    fn raw_header_mode(
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
        let mut tar = Vec::new();
        for (name, tf, link, content, mode) in entries {
            tar.extend_from_slice(&raw_header_mode(name, *tf, link, content.len(), *mode));
            tar.extend_from_slice(content);
            let pad = (512 - content.len() % 512) % 512;
            tar.resize(tar.len() + pad, 0);
        }
        tar.resize(tar.len() + 1024, 0); // end-of-archive marker
        let path = dir.join(format!("{label}.tar.zst"));
        let f = std::fs::File::create(&path).unwrap();
        let mut enc = zstd::Encoder::new(f, 0).unwrap();
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap();
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

    // A symlink entry aborts extraction (refused outright by vet_entry).
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
        let f = std::fs::File::create(&path).unwrap();
        let mut enc = zstd::Encoder::new(f, 0).unwrap();
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap();
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
        let f = std::fs::File::create(&path).unwrap();
        let mut enc = zstd::Encoder::new(f, 0).unwrap();
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap();
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
        assert_eq!(std::fs::read(root.join("lib/big.so")).unwrap().len(), 200_000);
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
                std::fs::metadata(root.join(p)).unwrap().permissions().mode() & 0o7777
            };
            assert_eq!(mode_of("bin/targo"), 0o755, "the exec mode class");
            assert_eq!(mode_of("share/doc/readme"), 0o644, "the non-exec mode class");
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
}
