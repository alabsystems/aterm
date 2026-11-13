// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tar-slip-safe archive extraction and tree-root folding.

use std::cell::Cell;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use crate::tarread::EntryType as TarEntryType;

mod ahead;
mod behind;

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
    /// An npm binary package member other than `package/package.json`, the files beside it,
    /// and one `package/bin/<exe>.br` ([`stage_npm_brotli_binary`]).
    NotNpmBinary,
    /// An npm binary package's binary decoded to a length other than the one the vendor
    /// names for it.
    DecodedSize,
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

/// The pure component walk of [`vet_entry`].
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

/// Drop the first `strip` components of an already-vetted relative path.
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

/// The last step of [`vet_entry`]: the join must still be under the root.
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

/// Attach the failing operation and the path it was on to an I/O error ([`ExtractError::Io`]).
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
            ExtractError::Rejected(ExtractReject::NotNpmBinary, p) if p.as_os_str().is_empty() => f
                .write_str(
                    "the npm package holds no package/package.json or no package/bin binary; it \
                     must hold one of each",
                ),
            ExtractError::Rejected(ExtractReject::NotNpmBinary, p) => write!(
                f,
                "rejected entry {p:?}: an npm binary package holds only regular files named \
                 package/<name>, among them one package.json, and one package/bin/<exe>.br \
                 (this is another kind, another place, or a second one)"
            ),
            ExtractError::Rejected(ExtractReject::DecodedSize, p) => write!(
                f,
                "rejected entry {p:?}: it decodes to a length other than the binary's, which \
                 the vendor's own copy names"
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

/// Map a tar entry type to our [`EntryKind`] ([`vet_entry`]).
const fn classify(ty: TarEntryType) -> EntryKind {
    match ty {
        TarEntryType::Regular | TarEntryType::Continuous => EntryKind::Regular,
        TarEntryType::Directory => EntryKind::Directory,
        TarEntryType::Symlink => EntryKind::Symlink,
        TarEntryType::Link => EntryKind::Hardlink,
        TarEntryType::Other => EntryKind::Other,
    }
}

/// Sanitize an archive mode (`0o755` for dirs/executables, `0o644` for regular files).
pub const fn safe_mode(entry_mode: u32, is_dir: bool) -> u32 {
    if is_dir || entry_mode & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}

/// The per-entry structural byte budget.
const TAR_ENTRY_STRUCTURAL_BUDGET: u64 = 1 << 20;

/// Marker error the [`CappedReader`] raises when the structural budget is exhausted.
#[derive(Debug)]
struct SizeCapExceeded;

impl std::fmt::Display for SizeCapExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("decompressed size cap exceeded")
    }
}

impl std::error::Error for SizeCapExceeded {}

/// Wraps the decompressor and bounds every byte the tar reader pulls against a shared,
/// per-entry-reset budget ([`write_capped`]).
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

/// Translate a tar/io error into [`ExtractError`] ([`CappedReader`], [`SizeCapExceeded`]).
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

/// [`map_tar_io`] with the I/O half attributed to `op` on `path`.
fn map_tar_io_at(e: io::Error, op: &'static str, path: &Path) -> ExtractError {
    match map_tar_io(e) {
        ExtractError::Io { err, .. } => io_at(err, op, path),
        other => other,
    }
}

/// One node of the staged tree as the digest sees it.
#[derive(Debug, Clone)]
enum Node {
    /// The folded permission bits and the lowercase-hex content sha256.
    File(u32, String),
    /// Lowercase-hex sha256 of the raw target bytes ([`crate::tree::symlink_target_sha`]).
    Link(String),
}

/// Extraction-time accumulator for [`crate::tree::tree_root`].
#[derive(Debug)]
pub(crate) struct TreeAccumulator {
    /// relpath bytes → index into `nodes`.
    paths: std::collections::BTreeMap<Vec<u8>, usize>,
    /// One entry per distinct node laid down.
    nodes: Vec<Node>,
    /// The directories this extraction made, as a tree of exact names.
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

    /// The index of the directory this extraction made as `name` inside directory `parent`.
    fn child_dir(&self, parent: usize, name: &[u8]) -> Option<usize> {
        self.dirs.get(parent)?.get(name).copied()
    }

    /// Record that this extraction made directory `name` inside directory `parent`.
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

    /// Whether this extraction already wrote a regular file at exactly `rel`
    /// ([`Self::record_file`]).
    fn wrote_file(&self, rel: &[u8]) -> bool {
        self.paths
            .get(rel)
            .is_some_and(|&node| matches!(self.nodes.get(node), Some(Node::File(..))))
    }

    /// Record a hardlink at `rel` aliasing the already-extracted `target_rel` ([`Layer::hardlink`],
    /// [`Self::wrote_file`]).
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
    /// Returns [`ExtractError`] on any [`ExtractReject::Respelled`] mismatch or directory read
    /// failure.
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

/// Require an empty real directory, or an absent root the caller can create.
/// Inspection failures are errors, never evidence that a destination is empty.
pub(crate) fn require_empty_destination(dest_root: &Path) -> Result<(), ExtractError> {
    match std::fs::symlink_metadata(dest_root) {
        Ok(meta) if meta.is_symlink() => {
            return Err(ExtractError::Rejected(
                ExtractReject::ThroughSymlink,
                dest_root.to_path_buf(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_at(error, "symlink_metadata", dest_root)),
    }
    let mut existing =
        std::fs::read_dir(dest_root).map_err(|error| io_at(error, "read_dir", dest_root))?;
    if let Some(entry) = existing.next() {
        let found = entry
            .map_err(|error| io_at(error, "read_dir", dest_root))?
            .path();
        let msg = format!(
            "extraction destination is not empty: {}",
            found.to_string_lossy()
        );
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, msg).into());
    }
    Ok(())
}

/// One extraction in progress.
struct Layer<'a> {
    root: &'a Path,
    opts: ExtractOptions,
    /// The signed content cap, decremented as bytes are written (or drained).
    remaining: u64,
    /// A tar's structure allowance ([`Self::charge_structure`]).
    structure_remaining: u64,
    max_entries: u64,
    count: u64,
    /// The per-entry structural budget of the tar [`CappedReader`] ([`write_capped`]).
    budget: Rc<Cell<u64>>,
    /// One copy buffer for the whole archive.
    copy_buf: Vec<u8>,
    /// The tree_root of what we are writing, folded as we write it (see [`TreeAccumulator`]).
    tree: Option<TreeAccumulator>,
    /// Whether any symlink has been laid yet.
    laid_symlink: bool,
    /// The parent directory the last laid entry needed ([`Layer::ensure_parent`]).
    last_parent: Option<PathBuf>,
    /// The writers a large file's bytes go to ([`behind`]); none writes every file here.
    behind: Option<&'a behind::WriteBehind>,
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
        require_empty_destination(root)?;
        std::fs::create_dir_all(root).map_err(|e| io_at(e, "create_dir_all", root))?;
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
            behind: None,
        })
    }

    /// Count one more entry against the entry cap.
    const fn next_entry(&mut self) -> Result<(), ExtractError> {
        self.count = self.count.saturating_add(1);
        if self.count > self.max_entries {
            return Err(ExtractError::TooLarge(TooLargeReason::EntryCap));
        }
        Ok(())
    }

    /// Charge `n` decompressed structural bytes.
    const fn charge_structure(&mut self, n: u64) -> Result<(), ExtractError> {
        if n > self.structure_remaining {
            return Err(ExtractError::TooLarge(TooLargeReason::StructureCap));
        }
        self.structure_remaining = self.structure_remaining.saturating_sub(n);
        Ok(())
    }

    const fn strip(&self) -> u32 {
        self.opts.strip_components
    }

    /// Every proper ancestor of `dest` below the root must be a real directory (or not exist yet),
    /// never a symlink.
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

    /// A file/directory/hardlink entry landing ON a symlink laid earlier would write through it.
    fn refuse_if_link(&self, dest: &Path, raw: &Path) -> Result<(), ExtractError> {
        if self.laid_symlink && std::fs::symlink_metadata(dest).is_ok_and(|m| m.is_symlink()) {
            return Err(ExtractError::Rejected(
                ExtractReject::Occupied,
                raw.to_path_buf(),
            ));
        }
        Ok(())
    }

    /// Make sure `dest`'s parent directory exists.
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

    /// Make directory `dir` and its missing ancestors ([`make_dir_exact`]).
    fn ensure_dir(&mut self, dir: &Path, raw: &Path) -> Result<(), ExtractError> {
        match self.tree.as_mut() {
            Some(tree) => make_dir_exact(self.root, dir, raw, tree),
            None => std::fs::create_dir_all(dir).map_err(|e| io_at(e, "create_dir_all", dir)),
        }
    }

    /// Lay down a directory entry.
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

    /// Lay down a regular file from `body`, capped and (when folding) hashed as it is written.
    fn regular(&mut self, raw: &Path, mode: u32, body: impl Read) -> Result<u64, ExtractError> {
        let Some(dest) = vet_entry_stripped(self.root, raw, EntryKind::Regular, self.strip())
            .map_err(|r| ExtractError::Rejected(r, raw.to_path_buf()))?
        else {
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
        if replace && let Some(behind) = self.behind {
            // A path laid once already is laid again: its earlier bytes are all written
            // before it is reopened.
            behind.flush().map_err(|(e, at)| io_at(e, "write", &at))?;
        }
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
        let written = write_capped_behind(
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
            self.behind,
        )?;
        if let (Some(tree), Some(rel)) = (self.tree.as_mut(), rel) {
            tree.record_file(rel, written.mode, written.content_sha_hex);
        }
        Ok(written.len)
    }

    /// Lay down a hardlink.
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
            return Err(ExtractError::Rejected(
                ExtractReject::HardlinkTargetMissing,
                raw.to_path_buf(),
            ));
        }
        Ok(())
    }

    /// Lay down a symlink under the selected target policy.
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

/// Extract a `.tar.zst` archive into an empty real directory `dest_root`, creating it if absent.
/// # Errors
/// An occupied, symlinked or unreadable destination, an invalid archive or an exceeded bound.
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
    extract_tar_zst_tree_from(
        crate::metadata_io::open_regular(archive)?,
        dest_root,
        max_total_bytes,
        max_entries,
        opts,
        checksum,
    )
}

/// [`extract_tar_zst_tree`] over the archive's bytes as `input` yields them, decoded ahead of
/// the walk ([`ahead::decoded_ahead`]).
pub(crate) fn extract_tar_zst_tree_from(
    input: impl Read + Send,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    opts: ExtractOptions,
    checksum: FrameChecksum,
) -> Result<TreeAccumulator, ExtractError> {
    let (walked, _) = ahead::decoded_ahead(zstd_reader(input, checksum), |decoded| {
        extract_stream(
            decoded,
            dest_root,
            max_total_bytes,
            max_entries,
            true,
            opts,
            Trailer::Drained,
        )
    });
    folded(walked?)
}

/// The zstd decoder over `archive` ([`FrameChecksum::Required`]).
fn zstd_decoder(
    archive: &Path,
    checksum: FrameChecksum,
) -> io::Result<aterm_zstd::Decoder<io::BufReader<std::fs::File>>> {
    Ok(zstd_reader(
        crate::metadata_io::open_regular(archive)?,
        checksum,
    ))
}

/// The bytes the decoder reads from its input at a time.
const COMPRESSED_READ: usize = 256 * 1024;

/// The zstd decoder over `input` ([`FrameChecksum::Required`]).
fn zstd_reader<R: Read>(
    input: R,
    checksum: FrameChecksum,
) -> aterm_zstd::Decoder<io::BufReader<R>> {
    let decoder =
        aterm_zstd::Decoder::with_buffer(io::BufReader::with_capacity(COMPRESSED_READ, input));
    match checksum {
        FrameChecksum::IfPresent => decoder,
        FrameChecksum::Required => decoder.require_checksum(),
    }
}

/// Extract a `.tar.gz` archive into `dest_root` and fold its `tree_root`.
pub(crate) fn extract_tar_gz_tree(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    opts: ExtractOptions,
) -> Result<TreeAccumulator, ExtractError> {
    extract_tar_gz_tree_from(
        crate::metadata_io::open_regular(archive)?,
        dest_root,
        max_total_bytes,
        max_entries,
        opts,
    )
}

/// [`extract_tar_gz_tree`] over the archive's bytes as `input` yields them, decoded ahead of
/// the walk ([`ahead::decoded_ahead`]).
pub(crate) fn extract_tar_gz_tree_from(
    input: impl Read + Send,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    opts: ExtractOptions,
) -> Result<TreeAccumulator, ExtractError> {
    let decoder = aterm_codec::inflate::stream::GzipReader::new(input);
    let (walked, _) = ahead::decoded_ahead(decoder, |decoded| {
        extract_stream(
            decoded,
            dest_root,
            max_total_bytes,
            max_entries,
            true,
            opts,
            Trailer::Unread,
        )
    });
    folded(walked?)
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
        crate::metadata_io::open_regular(archive)?,
        dest_root,
        max_total_bytes,
        max_entries,
        true,
        opts,
        Trailer::Drained,
    )?)
}

const ZIP_S_IFMT: u32 = 0o170_000;
const ZIP_S_IFLNK: u32 = 0o120_000;
const ZIP_S_IFREG: u32 = 0o100_000;
const ZIP_S_IFDIR: u32 = 0o040_000;
/// A symlink target longer than `PATH_MAX` names no valid link.
const MAX_LINK_TARGET: u64 = 4096;

/// `zip: <what>` as an `InvalidData` error, worded as `aterm_codec::zip`'s own.
fn zip_bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("zip: {what}"))
}

/// Regular / directory / symlink / other, from the name's trailing slash and the Unix
/// type bits (when the host supplied them).
fn zip_classify(name: &[u8], unix_mode: u32) -> EntryKind {
    if name.last() == Some(&b'/') {
        return EntryKind::Directory;
    }
    match unix_mode & ZIP_S_IFMT {
        0 | ZIP_S_IFREG => EntryKind::Regular,
        ZIP_S_IFDIR => EntryKind::Directory,
        ZIP_S_IFLNK => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

/// The member name as a path — raw bytes on Unix, UTF-8 or refused elsewhere.
fn zip_name_to_path(bytes: &[u8]) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| zip_bad("member name is not UTF-8"))
    }
}

/// Extract a `.zip` archive into `dest_root` through a [`Layer`] and fold its `tree_root`.
///
/// `aterm_codec::zip` reads the archive; this lane counts each declared record against
/// the layer before reading it, refuses encrypted members, NUL or backslash names, kinds
/// other than regular, directory and (by policy) symlink, and a body or link target
/// whose length disagrees with its record.
pub(crate) fn extract_zip_tree(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    opts: ExtractOptions,
) -> Result<TreeAccumulator, ExtractError> {
    let mut zip = aterm_codec::zip::Archive::open(crate::metadata_io::open_regular(archive)?)?;
    let budget = Rc::new(Cell::new(TAR_ENTRY_STRUCTURAL_BUDGET));
    let mut layer = Layer::open(dest_root, max_total_bytes, max_entries, true, opts, budget)?;
    for _ in 0..zip.len() {
        layer.next_entry()?;
        let rec = zip
            .next_entry()?
            .ok_or_else(|| zip_bad("central directory holds fewer records than declared"))?;
        if rec.encrypted() {
            return Err(zip_bad("encrypted members are not supported").into());
        }
        if rec.name.contains(&0) || rec.name.contains(&b'\\') {
            return Err(zip_bad("member name contains NUL or a backslash").into());
        }
        let raw = zip_name_to_path(&rec.name)?;
        let unix_mode = rec.unix_mode();
        match zip_classify(&rec.name, unix_mode) {
            EntryKind::Directory => {
                layer.directory(&raw, rec.uncomp_size, unix_mode & 0o7777)?;
            }
            EntryKind::Regular => {
                let body = zip.body(&rec)?;
                let consumed = layer.regular(&raw, unix_mode & 0o7777, body)?;
                if consumed != rec.uncomp_size {
                    return Err(zip_bad("member body disagrees with its declared size").into());
                }
            }
            EntryKind::Symlink => {
                if opts.symlinks == SymlinkPolicy::Refuse {
                    return Err(ExtractError::Rejected(ExtractReject::DisallowedKind, raw));
                }
                if rec.uncomp_size > MAX_LINK_TARGET {
                    return Err(ExtractError::TooLarge(TooLargeReason::LinkTargetTooLong(
                        raw.clone(),
                    )));
                }
                let body = zip.body(&rec)?;
                let mut target = Vec::with_capacity(
                    usize::try_from(rec.uncomp_size.saturating_add(1)).unwrap_or(0),
                );
                body.take(MAX_LINK_TARGET.saturating_add(1))
                    .read_to_end(&mut target)?;
                if target.len() as u64 != rec.uncomp_size {
                    return Err(zip_bad("symlink target disagrees with its declared size").into());
                }
                let target = zip_name_to_path(&target)?;
                layer.symlink(&raw, &target, 0)?;
            }
            EntryKind::Hardlink | EntryKind::Other => {
                return Err(ExtractError::Rejected(ExtractReject::DisallowedKind, raw));
            }
        }
    }
    // Past the last declared record: the records must fill the directory's declared size.
    if zip.next_entry()?.is_some() {
        return Err(zip_bad("central directory holds more records than declared").into());
    }
    folded(layer.finish())
}

/// Archive container format for [`extract_tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// Zstandard-compressed tar (`.tar.zst`).
    TarZst,
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
        Container::TarZst => extract_tar_zst_tree(
            archive,
            dest_root,
            max_total_bytes,
            entries,
            opts,
            FrameChecksum::IfPresent,
        ),
        Container::TarGz => extract_tar_gz_tree(archive, dest_root, max_total_bytes, entries, opts),
        Container::Tar => extract_tar_tree(archive, dest_root, max_total_bytes, entries, opts),
        Container::Zip => extract_zip_tree(archive, dest_root, max_total_bytes, entries, opts),
    }
    .map(|_| ())
}

/// The fold a `fold = true` extraction always produces.
fn folded(tree: Option<TreeAccumulator>) -> Result<TreeAccumulator, ExtractError> {
    tree.ok_or_else(|| io::Error::other("extraction produced no tree_root").into())
}

/// What [`extract_stream`] does with the decoded bytes after the tar's end-of-archive marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trailer {
    /// Left unread.
    Unread,
    /// Read to the end of the stream.
    Drained,
}

/// The one tar extraction body ([`crate::tree::tree_root`], [`extract_tar_zst_tree`],
/// [`extract_tar_zst`]).
fn extract_stream(
    decoded: impl Read,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    fold: bool,
    opts: ExtractOptions,
    trailer: Trailer,
) -> Result<Option<TreeAccumulator>, ExtractError> {
    let entries = |behind: Option<&behind::WriteBehind>| {
        extract_entries(
            decoded,
            dest_root,
            max_total_bytes,
            max_entries,
            fold,
            opts,
            trailer,
            behind,
        )
    };
    // Only a folding walk knows which paths it laid already ([`Layer::regular`]).
    if fold {
        behind::run(entries)
    } else {
        entries(None)
    }
}

/// [`extract_stream`]'s walk, with `behind` writing large files' bytes when given.
#[allow(
    clippy::too_many_arguments,
    reason = "extract_stream's inputs plus the writers it runs the walk with"
)]
fn extract_entries(
    decoded: impl Read,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    fold: bool,
    opts: ExtractOptions,
    trailer: Trailer,
    behind: Option<&behind::WriteBehind>,
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
    layer.behind = behind;
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
    if let Some(behind) = behind {
        behind.flush().map_err(|(e, at)| io_at(e, "write", &at))?;
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

/// The file [`write_capped`] streams into: the handle its caller opened ([`Layer::regular`]).
struct Sink<'p> {
    file: std::fs::File,
    dest: &'p Path,
    mode: u32,
}

/// Stream `reader` into `sink`'s freshly opened file with `sink.mode` ([`ExtractError::TooLarge`]).
fn write_capped(
    reader: impl Read,
    sink: Sink<'_>,
    remaining: &mut u64,
    budget: &Rc<Cell<u64>>,
    buf: &mut [u8],
    fold: bool,
) -> Result<WrittenFile, ExtractError> {
    write_capped_behind(reader, sink, remaining, budget, buf, fold, None)
}

/// [`write_capped`], the bytes past a file's first [`behind::CHUNK`] handed to `behind`'s
/// writers in pieces at their offsets when it is given: what the file holds is complete once
/// `behind` is flushed.
fn write_capped_behind(
    mut reader: impl Read,
    sink: Sink<'_>,
    remaining: &mut u64,
    budget: &Rc<Cell<u64>>,
    buf: &mut [u8],
    fold: bool,
    behind: Option<&behind::WriteBehind>,
) -> Result<WrittenFile, ExtractError> {
    let Sink { file, dest, mode } = sink;
    let f = std::sync::Arc::new(file);
    let mut hasher = fold.then(crate::tree::BulkSha256::new);
    let mut len: u64 = 0;
    // The piece being gathered for `behind`, from the offset it starts at. Bytes before the
    // first CHUNK go through the file's cursor here; every byte from the first piece on goes
    // at its own offset, so the two never meet.
    let mut piece: Option<(u64, Vec<u8>)> = None;
    let mut path: Option<std::sync::Arc<PathBuf>> = None;
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
        let before = len;
        len = len.saturating_add(n);
        let chunk = match buf.get(..take) {
            Some(c) => c,
            None => &buf[..],
        };
        if let Some(hasher) = hasher.as_mut() {
            hasher.update(chunk);
        }
        match behind {
            Some(behind) if before >= behind::CHUNK as u64 => {
                let (_, gathered) = piece.get_or_insert_with(|| (before, behind.buffer()));
                gathered.extend_from_slice(chunk);
                if gathered.len() >= behind::CHUNK
                    && let Some((at, full)) = piece.take()
                {
                    let path = path.get_or_insert_with(|| std::sync::Arc::new(dest.to_path_buf()));
                    behind
                        .hand(&f, path, at, full)
                        .map_err(|(e, failed)| io_at(e, "write", &failed))?;
                }
            }
            _ => (&*f)
                .write_all(chunk)
                .map_err(|e| io_at(e, "write", dest))?,
        }
        crate::progress::extract_tick(n);
    }
    if let (Some(behind), Some((at, rest))) = (behind, piece.take()) {
        let path = path.get_or_insert_with(|| std::sync::Arc::new(dest.to_path_buf()));
        behind
            .hand(&f, path, at, rest)
            .map_err(|(e, failed)| io_at(e, "write", &failed))?;
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

/// Consume `reader` to EOF without writing, through the same caps as [`write_capped`].
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
    let file =
        crate::platform::open_create_write(dest, mode).map_err(|e| io_at(e, "open", dest))?;
    stage_open_file(reader, file, dest, mode, max_total_bytes)
}

/// The same bounded writer for a caller's descriptor-relative, exclusively created file.
pub(crate) fn stage_open_file(
    reader: impl Read,
    file: std::fs::File,
    dest: &Path,
    mode: u32,
    max_total_bytes: u64,
) -> Result<WrittenFile, ExtractError> {
    let budget = Rc::new(Cell::new(TAR_ENTRY_STRUCTURAL_BUDGET));
    let mut buf = vec![0u8; 64 * 1024];
    let mut remaining = max_total_bytes;
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
    let file = crate::metadata_io::open_regular(archive).map_err(|e| io_at(e, "open", archive))?;
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

/// The largest `package/package.json` an npm binary package carries (grok 1.0.70's are under
/// 400 bytes).
const NPM_PACKAGE_JSON_CAP: u64 = 4 * 1024;

/// What the files beside an npm binary package's manifest (its README, its third-party
/// notices) may add up to; they are read and dropped.
const NPM_BESIDE_CAP: u64 = 4 << 20;

/// What [`stage_npm_brotli_binary`] staged.
#[derive(Debug)]
pub(crate) struct NpmBinary {
    /// The binary written to the destination.
    pub(crate) written: WrittenFile,
    /// The package's `package/package.json`, for the caller to hold to its version document.
    pub(crate) package_json: Vec<u8>,
}

/// Stage an npm binary package (the vendor-direct lane's `npm-brotli-binary`, grok's
/// `@xai-official/grok-<plat>`): from the gzip'd tar at `archive`, its one
/// `package/bin/<br_name>` streamed through Brotli into `dest` with `mode` at exactly `size`
/// bytes, and its `package/package.json` handed back. Every member is a regular file named
/// `package/<name>` or `package/bin/<name>` by one bare component; the files in `package/`
/// beside the manifest are read and dropped.
///
/// # Errors
/// [`ExtractError::Rejected`] with [`ExtractReject::NotNpmBinary`] for a member of another
/// kind or place, a second manifest or binary, or a package lacking either; with
/// [`ExtractReject::DecodedSize`] for a binary decoding to other than `size` bytes;
/// [`ExtractError::TooLarge`] for a manifest over 4 KiB, files beside it over 4 MiB, or tar
/// structure past its budgets; [`ExtractError::Io`] for a gzip, tar or Brotli stream that
/// does not read, or a write that fails.
pub(crate) fn stage_npm_brotli_binary(
    archive: &Path,
    br_name: &str,
    dest: &Path,
    mode: u32,
    size: u64,
) -> Result<NpmBinary, ExtractError> {
    let file = crate::metadata_io::open_regular(archive).map_err(|e| io_at(e, "open", archive))?;
    let budget = Rc::new(Cell::new(TAR_ENTRY_STRUCTURAL_BUDGET));
    let mut tar = crate::tarread::Archive::new(CappedReader {
        inner: aterm_codec::inflate::stream::GzipReader::new(file),
        budget: Rc::clone(&budget),
    });
    let mut entries = tar.entries().map_err(map_tar_io)?;
    let mut structure = size.saturating_add(NPM_BESIDE_CAP);
    let mut beside = NPM_BESIDE_CAP;
    let mut buf = vec![0u8; 64 * 1024];
    let (mut written, mut manifest) = (None, None);
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
        let not_npm = |raw: PathBuf| ExtractError::Rejected(ExtractReject::NotNpmBinary, raw);
        if classify(entry.header().entry_type()) != EntryKind::Regular {
            return Err(not_npm(raw));
        }
        let place = raw
            .to_str()
            .and_then(|name| name.strip_prefix("package/"))
            .map(|rest| match rest.strip_prefix("bin/") {
                Some(in_bin) => (true, in_bin),
                None => (false, rest),
            });
        match place {
            Some((true, name)) if name == br_name && written.is_none() => {
                let file = crate::platform::open_create_write(dest, mode)
                    .map_err(|e| io_at(e, "open", dest))?;
                let mut remaining = size;
                let w = write_capped(
                    aterm_brotli::Decoder::new(&mut entry),
                    Sink { file, dest, mode },
                    &mut remaining,
                    &budget,
                    &mut buf,
                    true,
                )
                .map_err(|e| match e {
                    ExtractError::TooLarge(TooLargeReason::ContentCap) => {
                        ExtractError::Rejected(ExtractReject::DecodedSize, raw.clone())
                    }
                    other => other,
                })?;
                if w.len != size {
                    return Err(ExtractError::Rejected(ExtractReject::DecodedSize, raw));
                }
                written = Some(w);
            }
            Some((false, "package.json")) if manifest.is_none() => {
                let mut bytes = Vec::with_capacity(
                    usize::try_from(
                        entry
                            .entry_size()
                            .min(NPM_PACKAGE_JSON_CAP)
                            .saturating_add(1),
                    )
                    .unwrap_or(0),
                );
                (&mut entry)
                    .take(NPM_PACKAGE_JSON_CAP + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| map_tar_io_at(e, "read", &raw))?;
                if bytes.len() as u64 > NPM_PACKAGE_JSON_CAP {
                    return Err(ExtractError::TooLarge(TooLargeReason::ContentCap));
                }
                manifest = Some(bytes);
            }
            Some((false, name))
                if name != "package.json" && crate::vendor::bare_file_name(name) =>
            {
                drain_capped(&mut entry, &mut beside, &budget, &mut buf)?;
            }
            _ => return Err(not_npm(raw)),
        }
    }
    match (written, manifest) {
        (Some(written), Some(package_json)) => Ok(NpmBinary {
            written,
            package_json,
        }),
        _ => Err(ExtractError::Rejected(
            ExtractReject::NotNpmBinary,
            PathBuf::new(),
        )),
    }
}

/// Test archive builders shared across extraction and installation tests.
#[cfg(test)]
#[path = "extract_fixtures_tests.rs"]
pub(crate) mod fixtures;

#[cfg(test)]
#[path = "extract_tests.rs"]
mod tests;
