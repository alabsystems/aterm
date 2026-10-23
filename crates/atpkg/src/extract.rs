// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tar-slip-safe archive extraction and tree-root folding.

use std::cell::Cell;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use crate::tarread::EntryType as TarEntryType;

mod zipread;

/// Reason an archive entry was rejected during extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractReject {
    /// Entry path is absolute.
    AbsolutePath,
    /// Entry path contains `..`.
    ParentTraversal,
    /// Entry path is empty after normalization.
    EmptyPath,
    /// Destination or symlink target escapes the stage root.
    RootEscape,
    /// Disallowed entry type or symlink where forbidden by policy.
    DisallowedKind,
    /// Hardlink target has not been extracted as a regular file.
    HardlinkTargetMissing,
    /// Entry path traverses an earlier symlink in the archive.
    ThroughSymlink,
    /// Path collision involving a symlink or directory/file mismatch.
    Occupied,
    /// Filesystem stored a laid entry under different bytes (e.g. NFC/NFD or case folding).
    Respelled,
    /// Single-file archive does not contain exactly one bare-named regular file.
    NotSingleFile,
}

/// Archive entry classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Regular file.
    Regular,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
    /// Hard link.
    Hardlink,
    /// Unsupported special entry (device, FIFO, etc.).
    Other,
}

/// Extraction options for vendor archives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractOptions {
    /// Leading path components stripped from each entry.
    pub strip_components: u32,
    /// Policy governing symbolic links.
    pub symlinks: SymlinkPolicy,
    /// Whether descriptive global PAX headers are permitted.
    pub allow_global_pax: bool,
}

/// Policy for extracting symbolic links.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SymlinkPolicy {
    #[default]
    Refuse,
    /// Admit relative symlinks that resolve lexically within the stage root.
    InRoot,
    /// Admit non-empty leaf symlinks, including external targets.
    ExternalLeaf,
}

/// Whether `.tar.zst` extraction requires a zstd frame content checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameChecksum {
    /// Verify the frame checksum when present.
    IfPresent,
    /// Refuse frames without a content checksum.
    Required,
}

/// Steps 2–4 of [`vet_entry`]: the pure component walk. Accepts only `Normal` segments
fn vet_components(raw: &Path) -> Result<PathBuf, ExtractReject> {
    // An absolute entry path can never be made relative to the root.
    if raw.is_absolute() {
        return Err(ExtractReject::AbsolutePath);
    }
    let mut rel = PathBuf::new();
    for comp in raw.components() {
        match comp {
            Component::Normal(c) => rel.push(c),
            Component::CurDir => {}
            Component::ParentDir => return Err(ExtractReject::ParentTraversal),
            Component::RootDir | Component::Prefix(_) => return Err(ExtractReject::AbsolutePath),
        }
    }
    if rel.as_os_str().is_empty() {
        return Err(ExtractReject::EmptyPath);
    }
    Ok(rel)
}

/// Drop the first `strip` components of an already-vetted relative path. `None` when
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
fn leading_curdirs(raw: &Path) -> u32 {
    let n = raw
        .components()
        .take_while(|c| matches!(c, Component::CurDir))
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Step 5 of [`vet_entry`]: the join must still be under the root (defence in depth
fn join_under(root: &Path, rel: &Path) -> Result<PathBuf, ExtractReject> {
    let dest = root.join(rel);
    if !dest.starts_with(root) {
        return Err(ExtractReject::RootEscape);
    }
    Ok(dest)
}

/// Vet an archive entry path against `root` with no component stripping.
pub fn vet_entry(root: &Path, raw: &Path, kind: EntryKind) -> Result<PathBuf, ExtractReject> {
    match kind {
        EntryKind::Regular | EntryKind::Directory => {}
        EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
            return Err(ExtractReject::DisallowedKind);
        }
    }
    let rel = vet_components(raw)?;
    join_under(root, &rel)
}

/// Vet an archive entry path against `root`, stripping `strip_components` leading components.
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

/// Vet a hardlink entry and its link target under `root`.
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

/// Vet a symlink entry and its target under `root` according to `policy`.
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

/// Failure during archive extraction.
#[derive(Debug)]
pub enum ExtractError {
    /// I/O, decompression, or container format error.
    Io {
        /// Underlying I/O error.
        err: io::Error,
        /// Operation that failed.
        op: &'static str,
        /// Path involved in the failed operation, if known.
        path: Option<PathBuf>,
    },
    /// Entry rejected by path or type vetting.
    Rejected(ExtractReject, PathBuf),
    /// Archive exceeded size, structure, or entry limits.
    TooLarge(TooLargeReason),
}

/// Decompression-bomb or tar-bomb limit that refused the archive.
#[derive(Debug)]
pub enum TooLargeReason {
    /// Uncompressed content byte cap (`max_total_bytes`) exceeded.
    ContentCap,
    /// Archive structural overhead cap exceeded.
    StructureCap,
    /// Entry count cap (`max_entries`) exceeded.
    EntryCap,
    /// Single-entry structural header/extension budget exceeded.
    StructuralBudget,
    /// Directory member declared a non-zero body size.
    DirectoryWithBody(PathBuf),
    /// Hardlink member declared a non-zero body size.
    HardlinkWithBody(PathBuf),
    /// Symlink member declared a non-zero body size.
    SymlinkWithBody(PathBuf),
    /// Zip symlink target exceeded 4096 bytes.
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
fn classify(ty: TarEntryType) -> EntryKind {
    match ty {
        TarEntryType::Regular | TarEntryType::Continuous => EntryKind::Regular,
        TarEntryType::Directory => EntryKind::Directory,
        TarEntryType::Symlink => EntryKind::Symlink,
        TarEntryType::Link => EntryKind::Hardlink,
        TarEntryType::Other => EntryKind::Other,
    }
}

/// Sanitize an archive mode (`0o755` for dirs/executables, `0o644` for regular files).
pub fn safe_mode(entry_mode: u32, is_dir: bool) -> u32 {
    if is_dir || entry_mode & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}

/// The per-entry structural byte budget: the most decompressed bytes the tar reader may
const TAR_ENTRY_STRUCTURAL_BUDGET: u64 = 1 << 20;

/// Marker error the [`CappedReader`] raises when the structural budget is exhausted, so
#[derive(Debug)]
struct SizeCapExceeded;

impl std::fmt::Display for SizeCapExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("decompressed size cap exceeded")
    }
}

impl std::error::Error for SizeCapExceeded {}

/// Wraps the decompressor and bounds every byte the tar reader pulls against a shared,
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
        let cap = usize::try_from(remaining).map_or(buf.len(), |r| r.min(buf.len()));
        let n = self.inner.read(&mut buf[..cap])?;
        self.budget.set(remaining.saturating_sub(n as u64));
        Ok(n)
    }
}

/// Translate a tar/io error into [`ExtractError`], mapping the [`CappedReader`]'s
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
fn map_tar_io_at(e: io::Error, op: &'static str, path: &Path) -> ExtractError {
    match map_tar_io(e) {
        ExtractError::Io { err, .. } => io_at(err, op, path),
        other => other,
    }
}

/// One node of the staged tree as the digest sees it: a regular file (one per inode) or
#[derive(Debug, Clone)]
enum Node {
    /// The folded permission bits (`platform::folded_mode`, masked to `0o7777`) and the
    File(u32, String),
    /// Lowercase-hex sha256 of the raw target bytes ([`crate::tree::symlink_target_sha`]).
    Link(String),
}

/// Extraction-time accumulator for [`crate::tree::tree_root`].
#[derive(Debug)]
pub(crate) struct TreeAccumulator {
    /// relpath bytes → index into `nodes`: which node that path currently names. Sorted,
    paths: std::collections::BTreeMap<Vec<u8>, usize>,
    /// One entry per distinct node laid down.
    nodes: Vec<Node>,
    /// The directories this extraction made, as a tree of exact names: `dirs[i]` maps each
    dirs: Vec<std::collections::BTreeMap<Vec<u8>, usize>>,
}

impl TreeAccumulator {
    /// Create an empty accumulator.
    pub(crate) fn new() -> Self {
        Self {
            paths: std::collections::BTreeMap::new(),
            nodes: Vec::new(),
            dirs: vec![std::collections::BTreeMap::new()],
        }
    }

    /// The index of the directory this extraction made as `name` inside directory
    fn child_dir(&self, parent: usize, name: &[u8]) -> Option<usize> {
        self.dirs.get(parent)?.get(name).copied()
    }

    /// Record that this extraction made directory `name` inside directory `parent`, and
    fn record_dir(&mut self, parent: usize, name: Vec<u8>) -> usize {
        let child = self.dirs.len();
        self.dirs.push(std::collections::BTreeMap::new());
        if let Some(children) = self.dirs.get_mut(parent) {
            children.insert(name, child);
        }
        child
    }

    /// Record a regular file written at `rel`.
    pub(crate) fn record_file(&mut self, rel: Vec<u8>, mode: u32, content_sha_hex: String) {
        let existing = self.paths.get(&rel).copied();
        if let Some(node) = existing
            && let Some(slot) = self.nodes.get_mut(node)
            && matches!(slot, Node::File(..))
        {
            *slot = Node::File(mode, content_sha_hex);
            return;
        }
        self.nodes.push(Node::File(mode, content_sha_hex));
        let node = self.nodes.len().saturating_sub(1);
        self.paths.insert(rel, node);
    }

    /// Whether this extraction already wrote a regular file (or a hardlink to one) at
    fn wrote_file(&self, rel: &[u8]) -> bool {
        self.paths
            .get(rel)
            .is_some_and(|&node| matches!(self.nodes.get(node), Some(Node::File(..))))
    }

    /// Record a hardlink at `rel` aliasing the already-extracted `target_rel`. False (fail
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

    /// Record a symlink written at `rel` with raw `target` bytes.
    pub(crate) fn record_symlink(&mut self, rel: Vec<u8>, target: &[u8]) {
        self.nodes
            .push(Node::Link(crate::tree::symlink_target_sha(target)));
        let node = self.nodes.len().saturating_sub(1);
        self.paths.insert(rel, node);
    }

    /// Map of recorded relative paths to folded permission modes.
    pub(crate) fn declared_modes(&self) -> crate::tree::DeclaredModes {
        let mut modes = crate::tree::DeclaredModes::new();
        for (rel, node) in &self.paths {
            if let Some(Node::File(mode, _)) = self.nodes.get(*node) {
                modes.insert(rel.clone(), *mode);
            }
        }
        modes
    }

    /// Verify that on-disk filenames under `dest` match the recorded paths byte-for-byte.
    /// # Errors
    /// Returns [`ExtractError`] on any [`ExtractReject::Respelled`] mismatch or directory read failure.
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

    /// Compute the final `tree_root` SHA-256 hex digest.
    pub(crate) fn root(self) -> String {
        let mut lines: Vec<Vec<u8>> = Vec::with_capacity(self.paths.len());
        for (rel, node) in &self.paths {
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

/// Relative path bytes of `path` under `root` for `tree_root`.
pub(crate) fn rel_bytes_under(root: &Path, path: &Path) -> Result<Vec<u8>, ExtractError> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| ExtractError::Rejected(ExtractReject::RootEscape, path.to_path_buf()))?;
    Ok(crate::tree::rel_line_bytes(rel))
}

/// Create `dir` and missing ancestors under `root`, rejecting symlinks and respelled names.
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

/// Verify that `dest_root` is an existing empty directory.
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
struct Layer<'a> {
    root: &'a Path,
    opts: ExtractOptions,
    /// The signed content cap, decremented as bytes are written (or drained).
    remaining: u64,
    /// A tar's structure allowance: the signed cap again, decremented as headers, padding
    structure_remaining: u64,
    max_entries: u64,
    count: u64,
    /// The per-entry structural budget of the tar [`CappedReader`], shared so
    budget: Rc<Cell<u64>>,
    /// One copy buffer for the whole archive: a 64 KiB local in `write_capped` would be
    copy_buf: Vec<u8>,
    /// The tree_root of what we are writing, folded as we write it (see
    tree: Option<TreeAccumulator>,
    /// Whether any symlink has been laid yet. Until one has, no entry can pass through or
    laid_symlink: bool,
    /// The parent directory the last laid entry needed, known to be a directory
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
    fn ensure_dir(&mut self, dir: &Path, raw: &Path) -> Result<(), ExtractError> {
        match self.tree.as_mut() {
            Some(tree) => make_dir_exact(self.root, dir, raw, tree),
            None => std::fs::create_dir_all(dir).map_err(|e| io_at(e, "create_dir_all", dir)),
        }
    }

    /// Lay down a directory entry. A non-zero `declared_size` (the effective body size) is
    fn directory(&mut self, raw: &Path, declared_size: u64, mode: u32) -> Result<(), ExtractError> {
        if declared_size != 0 {
            return Err(ExtractError::TooLarge(TooLargeReason::DirectoryWithBody(
                raw.to_path_buf(),
            )));
        }
        let vetted = match vet_entry_stripped(self.root, raw, EntryKind::Directory, self.strip()) {
            Ok(v) => v,
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
        self.last_parent = Some(dest);
        Ok(())
    }

    /// Lay down a regular file from `body`, capped and (when folding) hashed as it is
    fn regular(&mut self, raw: &Path, mode: u32, body: impl Read) -> Result<u64, ExtractError> {
        let Some(dest) = vet_entry_stripped(self.root, raw, EntryKind::Regular, self.strip())
            .map_err(|r| ExtractError::Rejected(r, raw.to_path_buf()))?
        else {
            // Stripped away: the body still has to be consumed for the stream to
            return drain_capped(body, &mut self.remaining, &self.budget, &mut self.copy_buf);
        };
        self.guard_ancestors(&dest, raw)?;
        self.refuse_if_link(&dest, raw)?;
        self.ensure_parent(&dest, raw)?;
        let rel = match self.tree {
            Some(_) => Some(rel_bytes_under(self.root, &dest)?),
            None => None,
        };
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
        self.guard_ancestors(&target_abs, raw)?;
        let target_meta = std::fs::symlink_metadata(&target_abs).map_err(|_| {
            ExtractError::Rejected(ExtractReject::HardlinkTargetMissing, raw.to_path_buf())
        })?;
        if !target_meta.is_file() {
            return Err(ExtractError::Rejected(
                ExtractReject::HardlinkTargetMissing,
                raw.to_path_buf(),
            ));
        }
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
        std::fs::hard_link(&target_abs, &dest).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                ExtractError::Rejected(ExtractReject::Occupied, raw.to_path_buf())
            } else {
                io_at(e, "hard_link", &dest)
            }
        })?;
        if let (Some(tree), Some((dest_rel, target_rel))) = (self.tree.as_mut(), rels)
            && !tree.record_alias(dest_rel, &target_rel)
        {
            // Unreachable: the fold answered for this target above. Fail closed rather
            return Err(ExtractError::Rejected(
                ExtractReject::HardlinkTargetMissing,
                raw.to_path_buf(),
            ));
        }
        Ok(())
    }

    /// Lay down a symlink under the selected target policy, only onto a path
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

/// Create symlink `link -> target`.
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

/// Extract a `.tar.zst` archive into `dest_root`.
pub fn extract_tar_zst(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
) -> Result<(), ExtractError> {
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

/// Extract a `.tar.zst` archive into `dest_root` and fold its `tree_root`.
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

/// Extract a `.tar.gz` archive into `dest_root` and fold its `tree_root`.
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

/// Extract an uncompressed `.tar` archive into `dest_root` and fold its `tree_root`.
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

/// Extract a `.zip` archive into `dest_root` and fold its `tree_root`.
pub(crate) fn extract_zip_tree(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    opts: ExtractOptions,
) -> Result<TreeAccumulator, ExtractError> {
    zipread::extract(archive, dest_root, max_total_bytes, max_entries, opts)
}

/// Archive container format for [`extract_tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// Gzip-compressed tar (`.tar.gz`, `.tgz`).
    TarGz,
    /// Uncompressed tar (`.tar`).
    Tar,
    /// Zip archive (`.zip`).
    Zip,
}

/// Extract `archive` into empty directory `dest_root` using `container` and `opts`.
/// # Errors
/// Returns [`ExtractError`] if `dest_root` is non-empty or any entry fails vetting or extraction.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trailer {
    /// Left unread. The `tar-gz` lane: its reader refuses trailing bytes that are not a
    Unread,
    /// Read to the end of the stream, under one entry's structural budget and charged to
    Drained,
}

/// The one tar extraction body, over an already-decompressed `decoded` stream. `fold`
fn extract_stream(
    decoded: impl Read,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    fold: bool,
    opts: ExtractOptions,
    trailer: Trailer,
) -> Result<Option<TreeAccumulator>, ExtractError> {
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
    let mut entries = tar.entries().map_err(map_tar_io)?;
    loop {
        budget.set(TAR_ENTRY_STRUCTURAL_BUDGET);
        let next = entries.next_entry().map_err(map_tar_io)?;
        layer.charge_structure(TAR_ENTRY_STRUCTURAL_BUDGET.saturating_sub(budget.get()))?;
        let Some(mut entry) = next else {
            break;
        };
        layer.next_entry()?;
        let raw = entry.path()?.into_owned();
        let kind = classify(entry.header().entry_type());
        let mode = entry.header().mode().unwrap_or(0o644);
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
                let target = entry
                    .link_name()
                    .map_err(map_tar_io)?
                    .map_or_else(PathBuf::new, std::borrow::Cow::into_owned);
                layer.symlink(&raw, &target, declared)?;
            }
            EntryKind::Directory => layer.directory(&raw, declared, mode)?,
            EntryKind::Regular => {
                // The reader hands out content bytes through the CappedReader; the
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

/// Metadata observed while writing a capped regular file.
#[derive(Debug)]
pub(crate) struct WrittenFile {
    /// Folded permission bits (`0o644` or `0o755`).
    pub(crate) mode: u32,
    /// Lowercase-hex SHA-256 of the written file bytes.
    pub(crate) content_sha_hex: String,
    /// Total content bytes written.
    pub(crate) len: u64,
}

/// The file [`write_capped`] streams into: the handle its caller opened (how it was
struct Sink<'p> {
    file: std::fs::File,
    dest: &'p Path,
    mode: u32,
}

/// Stream `reader` into `sink`'s freshly opened file with `sink.mode`, decrementing
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
        let n = reader
            .read(&mut *buf)
            .map_err(|e| map_tar_io_at(e, "read", dest))?;
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
        let take = n;
        let n = n as u64;
        if n > *remaining {
            return Err(ExtractError::TooLarge(TooLargeReason::ContentCap));
        }
        *remaining = remaining.saturating_sub(n);
        len = len.saturating_add(n);
        let chunk = match buf.get(..take) {
            Some(c) => c,
            None => &buf[..],
        };
        if let Some(hasher) = hasher.as_mut() {
            hasher.update(chunk);
        }
        f.write_all(chunk).map_err(|e| io_at(e, "write", dest))?;
        crate::progress::extract_tick(n);
    }
    crate::platform::set_mode_on(&f, mode).map_err(|e| io_at(e, "set_mode", dest))?;
    match hasher {
        Some(hasher) => {
            let meta = f.metadata().map_err(|e| io_at(e, "fstat", dest))?;
            Ok(WrittenFile {
                mode: crate::platform::folded_mode(&meta, mode),
                content_sha_hex: hasher.finalize_hex(),
                len,
            })
        }
        None => Ok(WrittenFile {
            mode: 0,
            content_sha_hex: String::new(),
            len,
        }),
    }
}

/// Consume `reader` to EOF without writing, through the same caps as [`write_capped`]:
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

/// Stage a single raw binary from `reader` to `dest` with `mode`.
pub(crate) fn stage_file(
    reader: impl Read,
    dest: &Path,
    mode: u32,
    max_total_bytes: u64,
) -> Result<WrittenFile, ExtractError> {
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

/// Extract the single regular file named `member_name` from a `.tar.gz` archive to `dest`.
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

/// Test archive builders shared across extraction and installation tests.
#[cfg(test)]
pub(crate) mod fixtures {
    /// Build a raw 512-byte USTAR header.
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
        h[148..156].copy_from_slice(b"        ");
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

    /// Build a tar archive from `(name, typeflag, linkname, content, mode)` tuples.
    pub(crate) fn tar_bytes(entries: &[(&str, u8, &str, &[u8], u32)]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (name, tf, link, content, mode) in entries {
            tar.extend_from_slice(&raw_header_mode(name, *tf, link, content.len(), *mode));
            tar.extend_from_slice(content);
            let pad = (512 - content.len() % 512) % 512;
            tar.resize(tar.len() + pad, 0);
        }
        tar.resize(tar.len() + 1024, 0);
        tar
    }

    /// Wrap `raw` in a checksummed zstd frame.
    pub(crate) fn zstd_bytes(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        aterm_zstd::frame::write_raw(&mut out, raw, aterm_zstd::frame::Checksum::Xxh64).unwrap();
        out
    }

    /// Wrap `raw` in a RFC 1952 gzip member.
    pub(crate) fn gzip_bytes(raw: &[u8]) -> Vec<u8> {
        let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
        out.extend_from_slice(&deflate_bytes(raw));
        out.extend_from_slice(&crc32(raw).to_le_bytes());
        out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        out
    }

    /// Wrap `raw` in stored RFC 1951 DEFLATE blocks.
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

    /// Single member specification for [`zip_bytes`].
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

    /// Build a complete zip archive (optionally with ZIP64 headers).
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
        assert_eq!(
            vet_symlink(&r, Path::new("top/bin/x"), Path::new("../y"), 1).unwrap(),
            Some(r.join("bin/x"))
        );
        assert_eq!(
            vet_symlink(&r, Path::new("top"), Path::new("y"), 1).unwrap(),
            None
        );
        assert_eq!(
            vet_symlink(&r, Path::new("top/x"), Path::new("../y"), 1),
            Err(ExtractReject::RootEscape)
        );
    }

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
    fn fused_and_walked(tree: TreeAccumulator, root: &Path) -> (String, String) {
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
    #[cfg(unix)]
    const OPEN_UMASK_ENV: &str = "ATPKG_EXTRACT_OPEN_UMASK_DIR";
    /// That probe's name, as `--exact` wants it.
    #[cfg(unix)]
    const OPEN_UMASK_PROBE: &str =
        "extract::tests::probe_stages_its_directories_0755_under_an_open_umask";

    /// The child half of [`a_folding_lane_makes_every_directory_0755_whatever_the_umask`]:
    #[cfg(unix)]
    #[test]
    fn probe_stages_its_directories_0755_under_an_open_umask() {
        let Some(dir) = std::env::var_os(OPEN_UMASK_ENV) else {
            return;
        };
        // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail.
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
    #[test]
    fn a_zstd_lane_checks_a_checksum_past_the_tar_end() {
        let d = dest("checksum-past-end");
        let block = aterm_zstd::frame::BLOCK_MAX;
        let body = vec![b'x'; block - 1024];
        let mut tar = raw_header("f", b'0', "", body.len()).to_vec();
        tar.extend_from_slice(&body);
        tar.resize(block + 512, 0);
        tar.resize(tar.len().div_ceil(10_240) * 10_240, 0);
        let mut zst = Vec::new();
        aterm_zstd::frame::write_raw(&mut zst, &tar, aterm_zstd::frame::Checksum::Xxh64).unwrap();
        let good = d.join("good.tar.zst");
        std::fs::write(&good, &zst).unwrap();
        let last = zst.len() - 1;
        zst[last] ^= 1;
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
    #[test]
    fn fused_tree_root_is_byte_identical_to_the_on_disk_walk() {
        let d = dest("fused-parity");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
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
                ("share/doc/readme", b'0', "", b"hello, again", 0o644),
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
    #[test]
    fn a_member_the_filesystem_names_like_another_never_replaces_it_unseen() {
        let d = dest("fs-folded-names");
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
            let kinds = [
                ("file", b'0', "g", ""),
                ("dir", b'5', "sub/", ""),
                ("symlink", b'2', "l", "f"),
                ("hardlink", b'1', "h", first.as_str()),
            ];
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
        assert_ne!(base, other_mode, "mode must move the fused root");
    }

    /// The fold's precondition: a destination that already holds something is refused
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
        fold(&root).unwrap();
        assert!(fold(&d.join("absent")).is_ok());
        let err = fold(&root).unwrap_err();
        assert!(
            matches!(&err, ExtractError::Io { err, .. } if err.kind() == io::ErrorKind::AlreadyExists),
            "got {err:?}"
        );
        let planted = d.join("planted");
        std::fs::create_dir_all(&planted).unwrap();
        std::fs::write(planted.join("not-ours"), b"planted").unwrap();
        assert!(fold(&planted).is_err());
    }

    /// The vendor lane options every test below uses: strip one leading component and
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

        let r1 = d.join("gz1");
        extract_tar_gz_tree(&gz, &r1, 10_000_000, 10_000, vendor(1)).unwrap();
        assert!(
            r1.join("top/bin/gh").is_file(),
            "the `.` is the stripped component, as it is for the authoring tar"
        );
        assert!(!r1.join("bin/gh").exists(), "one component too deep");

        let r2 = d.join("gz2");
        extract_tar_gz_tree(&gz, &r2, 10_000_000, 10_000, vendor(2)).unwrap();
        assert!(r2.join("bin/gh").is_file());

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
                if root.join("real").is_file() {
                    assert_eq!(std::fs::read(root.join("real")).unwrap(), b"r", "{lane}");
                }
            }
        }
    }

    /// A hardlink whose target is named through a symlink (`alias -> lnk/f`, `lnk -> real`)
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
        let mut enc = plain.clone();
        let cd = enc
            .windows(4)
            .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        enc[cd + 8] |= 1;
        refuse("encrypted", &enc);
        let mut meth = plain.clone();
        meth[cd + 10] = 12;
        refuse("method", &meth);
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
        refuse("truncated", &plain[..plain.len() - 10]);
        let mut trailing = plain.clone();
        trailing.extend_from_slice(b"garbage");
        refuse("trailing", &trailing);
        let mut lying = plain.clone();
        let readme_cd = lying.windows(6).rposition(|w| w == b"README").unwrap();
        let rec = readme_cd - 46;
        assert_eq!(&lying[rec..rec + 4], &[0x50, 0x4b, 0x01, 0x02]);
        lying[rec + 24..rec + 28].copy_from_slice(&3u32.to_le_bytes());
        refuse("lying-size", &lying);
        refuse("empty", b"");
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
    #[test]
    fn stage_file_lays_one_capped_file_and_folds_it() {
        let d = dest("stage-file");
        let dest_path = d.join("bin/claude");
        std::fs::create_dir_all(dest_path.parent().unwrap()).unwrap();
        let payload = b"the binary";
        let w = stage_file(&payload[..], &dest_path, 0o755, 1 << 20).unwrap();
        assert_eq!(w.mode, 0o755, "the raw-binary lane folds mode 0755");
        let mut tree = TreeAccumulator::new();
        tree.record_file(b"bin/claude".to_vec(), w.mode, w.content_sha_hex);
        let (fused, walked) = fused_and_walked(tree, &d);
        assert_eq!(fused, walked);
        let err = stage_file(&payload[..], &d.join("capped"), 0o755, 4).unwrap_err();
        assert!(matches!(err, ExtractError::TooLarge(_)), "capped: {err:?}");
    }

    /// A disk failure mid-bundle names the step and the path it was on (EACCES from a
    #[cfg(unix)]
    #[test]
    fn an_io_failure_names_the_operation_and_the_path() {
        let d = dest("io-context");
        let root = d.join("staging");
        std::fs::create_dir_all(&root).unwrap();
        let archive = make_archive(&d, "tool", &[("bin/tool", b'0', "", b"#!/bin/sh\n")]);
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
        assert_eq!(w.mode, 0o755);
        let mut h = aterm_digest::Sha256::new();
        h.update(body);
        assert_eq!(
            w.content_sha_hex,
            aterm_codec::hex::encode_lower(&h.finalize())
        );
    }
}
