// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File-capability host for native Markdown and editor documents.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use amacs::store::{DocumentId, DocumentSnapshot};
use aterm_gui_consent::durable::Flush;

use crate::io::{
    AtomicSaveProof, AtomicSaveResult, AtomicSaveStage, ContentFingerprint, Decoded, DiskEncoding,
    ObservedFileVersion, SaveCause, SaveError, SaveGeneration, SavePlan, SaveReducer,
    SaveReduction, SaveTouch,
};

/// The save-side half of admission's decoding ([`Decoded::hold`]).
pub use crate::io::encode_for_disk;

mod folder;
use folder::Folder;

pub const DEFAULT_DOCUMENT_LIMIT: usize = 32 * 1024 * 1024;
/// Maximum bytes a single atomic save or proof observation accepts.
const MAX_ATOMIC_SAVE_BYTES: usize = DEFAULT_DOCUMENT_LIMIT;
/// [`DEFAULT_DOCUMENT_LIMIT`] as a byte count the contracted predicates compare.
const DOCUMENT_LIMIT_LEN: u64 = DEFAULT_DOCUMENT_LIMIT as u64;
const _: () =
    assert!(DOCUMENT_LIMIT_LEN == 33_554_432 && MAX_ATOMIC_SAVE_BYTES == DEFAULT_DOCUMENT_LIMIT);
/// The most a bounded read reserves before it reads: the sentinel byte of a
/// read at the document limit.
const READ_RESERVE_CEILING: usize = DEFAULT_DOCUMENT_LIMIT + 1;

trust_contract_fn! {
    /// Whether `len` bytes exceed [`DEFAULT_DOCUMENT_LIMIT`].
    #[must_use]
    fn exceeds_document_limit(len: u64) -> bool
    clauses [ensures result == (len > 33554432) by exceeds_document_limit_is_threshold]
    {
        len > DOCUMENT_LIMIT_LEN
    }
}

verify_island! {
    clean {
        theorem exceeds_document_limit_is_threshold (len : UInt64) :
            trust_import_amacs_files__host__exceeds_document_limit len
                = decide (33554432 < len) := by
          simp only [trust_import_amacs_files__host__exceeds_document_limit]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentGrantId(NonZeroU64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantAccess {
    ReadOnly,
    ReadWrite,
}

impl GrantAccess {
    const fn permits_write(self) -> bool {
        matches!(self, Self::ReadWrite)
    }

    const fn merged(self, other: Self) -> Self {
        if self.permits_write() || other.permits_write() {
            Self::ReadWrite
        } else {
            Self::ReadOnly
        }
    }
}

#[derive(Clone, Debug)]
pub struct DocumentGrant {
    pub id: DocumentGrantId,
    pub canonical_uri: String,
    pub path: PathBuf,
    pub(crate) access: GrantAccess,
    target: AtomicFileTarget,
}

impl DocumentGrant {
    #[cfg(all(any(test, debug_assertions, feature = "test-support"), unix))]
    pub fn logical_path(&self) -> &Path {
        self.target.logical_path()
    }

    pub const fn target(&self) -> &AtomicFileTarget {
        &self.target
    }

    pub fn targets_logical_path(&self, path: &Path) -> bool {
        self.target.contains_logical_path(path)
    }

    /// Revalidate every path and symlink identity captured when this grant was minted.
    pub fn validate_current_binding(&self) -> Result<(), DocumentHostError> {
        validate_atomic_target(&self.target)
    }
}

/// The logical-to-physical binding captured when a writer reads a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomicFileTarget {
    logical_path: PathBuf,
    target_path: PathBuf,
    logical_symlinks: Vec<LogicalSymlinkBinding>,
    additional_logical_bindings: Vec<LogicalPathBinding>,
}

/// One additional spelling admitted for a canonical document already open in
/// this process.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogicalPathBinding {
    logical_path: PathBuf,
    logical_symlinks: Vec<LogicalSymlinkBinding>,
}

/// Stable identity of one admitted symlink in a config path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogicalSymlinkBinding {
    path: PathBuf,
    destination: PathBuf,
    identity: Option<u128>,
    modified_ns: Option<u64>,
    len: u64,
}

impl AtomicFileTarget {
    pub fn logical_path(&self) -> &Path {
        &self.logical_path
    }

    pub fn target_path(&self) -> &Path {
        &self.target_path
    }

    pub(crate) fn contains_logical_path(&self, path: &Path) -> bool {
        self.logical_path == path
            || self
                .additional_logical_bindings
                .iter()
                .any(|binding| binding.logical_path == path)
            || resolve_atomic_target(path, self.admits_config_symlinks())
                .is_ok_and(|candidate| candidate.target_path == self.target_path)
    }

    fn admits_config_symlinks(&self) -> bool {
        !self.logical_symlinks.is_empty()
            || self
                .additional_logical_bindings
                .iter()
                .any(|binding| !binding.logical_symlinks.is_empty())
    }

    fn retain_logical_binding(&mut self, candidate: &Self) {
        debug_assert_eq!(self.target_path, candidate.target_path);
        let candidate_binding = LogicalPathBinding {
            logical_path: candidate.logical_path.clone(),
            logical_symlinks: candidate.logical_symlinks.clone(),
        };
        if self.logical_path == candidate_binding.logical_path
            && self.logical_symlinks == candidate_binding.logical_symlinks
        {
            return;
        }
        if self
            .additional_logical_bindings
            .contains(&candidate_binding)
        {
            return;
        }

        if !candidate_binding.logical_symlinks.is_empty() {
            let previous = LogicalPathBinding {
                logical_path: std::mem::replace(
                    &mut self.logical_path,
                    candidate_binding.logical_path,
                ),
                logical_symlinks: std::mem::replace(
                    &mut self.logical_symlinks,
                    candidate_binding.logical_symlinks,
                ),
            };
            if !self.additional_logical_bindings.contains(&previous) {
                self.additional_logical_bindings.push(previous);
            }
        } else {
            self.additional_logical_bindings.push(candidate_binding);
        }
    }
}

/// Exact disk generation used as the compare-and-swap baseline by every aterm config/document writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomicFileBaseline {
    pub target: AtomicFileTarget,
    pub observed: ObservedFileVersion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomicFileContents {
    pub baseline: AtomicFileBaseline,
    pub bytes: Vec<u8>,
}

/// Result of the shared serialized file-commit authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtomicCommitResult {
    Committed(AtomicSaveProof),
    Conflict {
        observed: ObservedFileVersion,
        message: String,
    },
    Failed {
        stage: AtomicSaveStage,
        /// The step's own account, for a log; never words for a person.
        message: String,
        /// What the step met, for a caller to word ([`SaveCause`]).
        cause: Option<SaveCause>,
    },
    /// The atomic replacement succeeded, but a later durability, binding or content-proof step failed.
    PublishedUnverified {
        stage: AtomicSaveStage,
        observed: Option<ObservedFileVersion>,
        message: String,
        cause: Option<SaveCause>,
    },
}

/// A failed commit step: its log message and optional [`SaveCause`].
#[derive(Debug)]
struct StepFailure {
    message: String,
    cause: Option<SaveCause>,
}

impl StepFailure {
    fn io(touch: SaveTouch, message: String, error: &std::io::Error) -> Self {
        Self::met(touch, message, error.kind())
    }

    fn met(touch: SaveTouch, message: impl Into<String>, kind: std::io::ErrorKind) -> Self {
        Self {
            message: message.into(),
            cause: Some(SaveCause { kind, touch }),
        }
    }

    fn own(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cause: None,
        }
    }

    /// A refused read of the document: the I/O error's kind where it had one.
    fn read(error: &DocumentHostError) -> Self {
        Self {
            message: error.to_string(),
            cause: error.io_kind().map(|kind| SaveCause {
                kind,
                touch: SaveTouch::Read,
            }),
        }
    }
}

#[derive(Clone, Debug)]
pub struct GrantedDocument {
    pub directory: Option<amacs::store::DirectoryListing>,
    pub grant: DocumentGrant,
    pub text: String,
    pub observed: ObservedFileVersion,
}

/// One local file or directory admitted but not yet granted.
#[derive(Clone, Debug)]
pub struct AdmittedDocument {
    pub directory: Option<amacs::store::DirectoryListing>,
    pub(crate) canonical_uri: String,
    pub(crate) path: PathBuf,
    target: AtomicFileTarget,
    pub(crate) text: String,
    pub(crate) observed: ObservedFileVersion,
}

impl AdmittedDocument {
    /// The canonical identity resolved by admission, before minting a grant.
    pub fn canonical_uri(&self) -> &str {
        &self.canonical_uri
    }

    /// Whether no file exists yet: admitted with [`AdmitOptions::allow_missing`] as an empty text, which the first save creates.
    pub const fn is_missing(&self) -> bool {
        !self.observed.exists
    }

    /// How the admitted text maps back to the file's bytes.
    pub const fn encoding(&self) -> DiskEncoding {
        self.observed.encoding()
    }
}

/// What an admission accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmitOptions {
    /// The most bytes the file may hold.
    pub limit: usize,
    /// Bind symbolic links in the path (Manual's dotfiles config) instead of refusing them, as every other document does.
    pub allow_config_symlinks: bool,
    /// Admit a file that does not exist yet, in a directory that does, as an empty text (`amacs NEWFILE`, an `edit` with `create=1`).
    pub allow_missing: bool,
}

impl AdmitOptions {
    /// An ordinary document: up to [`DEFAULT_DOCUMENT_LIMIT`], no symbolic link, and the file must exist.
    pub const DOCUMENT: Self = Self {
        limit: DEFAULT_DOCUMENT_LIMIT,
        allow_config_symlinks: false,
        allow_missing: false,
    };
}

impl Default for AdmitOptions {
    fn default() -> Self {
        Self::DOCUMENT
    }
}

/// The blocking half of `DocumentGrantStore::open_local`.
pub fn admit_local_file(
    uri: &str,
    limit: usize,
    allow_config_symlinks: bool,
) -> Result<AdmittedDocument, DocumentHostError> {
    admit_local_file_opts(
        uri,
        AdmitOptions {
            limit,
            allow_config_symlinks,
            allow_missing: false,
        },
    )
}

/// [`admit_local_file`] with every option.
pub fn admit_local_file_opts(
    uri: &str,
    options: AdmitOptions,
) -> Result<AdmittedDocument, DocumentHostError> {
    let requested = file_uri_path(uri)?;
    if fs::symlink_metadata(&requested).is_ok_and(|metadata| metadata.is_dir()) {
        let target = resolve_atomic_target(&requested, options.allow_config_symlinks)?;
        let path = target.target_path.clone();
        let directory = read_directory(&path, options.limit)?;
        let observed = ObservedFileVersion::observed(directory.text.as_bytes(), None, None);
        return Ok(AdmittedDocument {
            canonical_uri: path_to_file_uri(&path)?,
            path,
            target,
            text: directory.text.clone(),
            observed,
            directory: Some(directory),
        });
    }
    let contents = read_atomic_file_with_config_symlinks(
        &requested,
        options.limit,
        options.allow_missing,
        options.allow_config_symlinks,
    )?;
    let path = contents.baseline.target.target_path.clone();
    if !path.is_absolute() {
        return Err(DocumentHostError::NotAbsolute);
    }
    if !contents.baseline.observed.exists {
        require_existing_directory(&path)?;
    }
    let canonical_uri = path_to_file_uri(&path)?;
    let decoded = decode_utf8(contents.bytes)?;
    Ok(AdmittedDocument {
        directory: None,
        canonical_uri,
        path,
        target: contents.baseline.target,
        text: decoded.text,
        observed: contents.baseline.observed.with_encoding(decoded.encoding),
    })
}

/// Read a bounded, sorted directory listing, keeping exact paths apart from labels.
pub fn read_directory(
    path: &Path,
    limit: usize,
) -> Result<amacs::store::DirectoryListing, DocumentHostError> {
    let mut entries = Vec::new();
    let mut size = 0usize;
    for entry in fs::read_dir(path).map_err(|error| open_read_error(path, error))? {
        let entry = entry.map_err(|error| open_read_error(path, error))?;
        let name = entry.file_name();
        size = size.saturating_add(name.len()).saturating_add(128);
        if size > limit {
            return Err(DocumentHostError::TooLarge { limit });
        }
        let kind = entry
            .file_type()
            .map_err(|error| open_read_error(&entry.path(), error))?;
        entries.push((!kind.is_dir(), name, entry.path(), kind));
    }
    entries.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut listing = amacs::store::DirectoryListing {
        text: format!(
            "{}\nRET/f: open   n/p: move   ^: parent   g: refresh   q: close\n\n",
            path.to_string_lossy().escape_debug()
        ),
        parent: path.parent().map(path_to_file_uri).transpose()?,
        entries: Vec::new(),
    };
    if let Some(parent) = &listing.parent {
        let start = listing.text.len();
        listing.text.push_str("  ../\n");
        listing
            .entries
            .push((start..listing.text.len(), parent.clone()));
    }
    for (_, name, entry_path, kind) in entries {
        let start = listing.text.len();
        listing.text.push_str("  ");
        for ch in name.to_string_lossy().chars() {
            if ch.is_control() {
                listing.text.extend(ch.escape_default());
            } else {
                listing.text.push(ch);
            }
        }
        if kind.is_dir() {
            listing.text.push('/');
        }
        if kind.is_symlink() {
            listing.text.push('@');
        }
        listing.text.push('\n');
        listing
            .entries
            .push((start..listing.text.len(), path_to_file_uri(&entry_path)?));
    }
    if listing.text.len() > limit {
        return Err(DocumentHostError::TooLarge { limit });
    }
    Ok(listing)
}

/// Verify that `path`'s parent directory exists ([`Directories::Existing`]).
fn require_existing_directory(path: &Path) -> Result<(), DocumentHostError> {
    let Some(parent) = path.parent() else {
        return Err(DocumentHostError::NotAFile);
    };
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(DocumentHostError::NotAFile),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            kind: Some(std::io::ErrorKind::NotFound),
            message: format!("{}{MISSING_SUFFIX}", parent.display()),
        }),
        Err(error) => Err(open_read_error(parent, error)),
    }
}

/// The canonical path and `file://` URI amacs opens `path` as.
pub fn open_path(path: &Path) -> Result<(PathBuf, String), DocumentHostError> {
    let canonical = match fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if fs::symlink_metadata(path).is_ok() {
                return Err(DocumentHostError::SymlinkComponent {
                    path: path.to_path_buf(),
                    dangling: true,
                });
            }
            let Some(name) = path.file_name() else {
                return Err(open_read_error(path, error));
            };
            let directory = match path.parent() {
                Some(directory) if !directory.as_os_str().is_empty() => directory,
                _ => Path::new("."),
            };
            match fs::canonicalize(directory) {
                Ok(directory) => directory.join(name),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(DocumentHostError::Io {
                        stage: AtomicSaveStage::Preflight,
                        kind: Some(std::io::ErrorKind::NotFound),
                        message: format!("{}{MISSING_SUFFIX}", directory.display()),
                    });
                }
                Err(error) => return Err(open_read_error(directory, error)),
            }
        }
        Err(error) => return Err(open_read_error(path, error)),
    };
    let uri = path_to_file_uri(&canonical)?;
    Ok((canonical, uri))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentHostError {
    UnsupportedScheme,
    RemoteAuthority,
    MalformedUri,
    InvalidEncoding,
    NotAbsolute,
    NotAFile,
    TooLarge {
        limit: usize,
    },
    InvalidUtf8,
    UnknownGrant,
    Io {
        stage: AtomicSaveStage,
        message: String,
        /// The kind of the I/O error behind it, where one was.
        kind: Option<std::io::ErrorKind>,
    },
    ChangedWhileReading,
    SymlinkComponent {
        path: PathBuf,
        /// Whether the final symbolic link points to a missing target ([`open_path`]).
        dangling: bool,
    },
    TargetRetargeted,
    /// The target is a DATALESS file.
    #[cfg(unix)]
    NotDownloaded {
        path: PathBuf,
    },
}

/// How a refused read of a file that is not there ends its message; the one
/// spelling [`DocumentHostError::refusal_word`] reads `missing` from.
const MISSING_SUFFIX: &str = " does not exist";

impl DocumentHostError {
    /// One word for why an open refused, for a client to act on (the `edit` verb's `ERR refused <word>`).
    pub fn refusal_word(&self) -> &'static str {
        match self {
            Self::NotAFile => "not-regular",
            Self::TooLarge { .. } => "too-large",
            Self::InvalidUtf8 => "invalid-utf8",
            Self::SymlinkComponent { .. } => "symlink",
            #[cfg(unix)]
            Self::NotDownloaded { .. } => "file-provider",
            Self::Io { stage, message, .. }
                if *stage == AtomicSaveStage::Preflight && message.ends_with(MISSING_SUFFIX) =>
            {
                "missing"
            }
            Self::UnsupportedScheme
            | Self::RemoteAuthority
            | Self::MalformedUri
            | Self::InvalidEncoding
            | Self::NotAbsolute => "bad-uri",
            Self::Io { .. }
            | Self::UnknownGrant
            | Self::ChangedWhileReading
            | Self::TargetRetargeted => "unreadable",
        }
    }

    /// The kind of the I/O error behind a refused read, where one was.
    pub const fn io_kind(&self) -> Option<std::io::ErrorKind> {
        match self {
            Self::Io { kind, .. } => *kind,
            _ => None,
        }
    }
}

impl std::fmt::Display for DocumentHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedScheme => f.write_str("only local file: documents are supported"),
            Self::RemoteAuthority => f.write_str("remote file URI authorities are not allowed"),
            Self::MalformedUri => f.write_str("malformed file URI"),
            Self::InvalidEncoding => f.write_str("file URI has invalid percent encoding"),
            Self::NotAbsolute => f.write_str("document path must be absolute"),
            Self::NotAFile => f.write_str("document target is not a regular file"),
            Self::TooLarge { limit } => write!(f, "document exceeds the {limit}-byte limit"),
            Self::InvalidUtf8 => f.write_str("document is not valid UTF-8"),
            Self::UnknownGrant => f.write_str("unknown document grant"),
            Self::Io { stage, message, .. } => write!(f, "{stage:?}: {message}"),
            Self::ChangedWhileReading => f.write_str("document changed while it was being read"),
            Self::SymlinkComponent {
                path,
                dangling: true,
            } => write!(
                f,
                "{} is a symbolic link to a file that does not exist",
                path.display()
            ),
            Self::SymlinkComponent { path, .. } => write!(
                f,
                "symbolic-link path components are not writable capabilities: {}",
                path.display()
            ),
            Self::TargetRetargeted => {
                f.write_str("document path now resolves to a different file; reopen it")
            }
            #[cfg(unix)]
            Self::NotDownloaded { path } => write!(
                f,
                "{} is evicted from iCloud Drive and must be downloaded in Finder first",
                path.display()
            ),
        }
    }
}

#[derive(Default)]
pub struct DocumentGrantStore {
    next_id: u64,
    grants: BTreeMap<DocumentGrantId, DocumentGrant>,
    by_uri: BTreeMap<String, DocumentGrantId>,
}

/// Persistence ownership keyed by canonical document identity.
#[derive(Default)]
pub struct DocumentPersistenceStore {
    documents: BTreeMap<DocumentId, DocumentPersistence>,
}

struct DocumentPersistence {
    grant: DocumentGrantId,
    save: SaveReducer,
    observed_image: Option<ObservedDocumentImage>,
}

/// The exact text of one observed version, as the document holds it.
struct ObservedDocumentImage {
    version: ObservedFileVersion,
    /// The text as the document holds it, which `version`'s encoding maps to
    /// the file's bytes.
    bytes: Arc<[u8]>,
    /// The text a fresh read of those bytes admits, where it is not `bytes`.
    reread: Option<Arc<[u8]>>,
}

impl ObservedDocumentImage {
    fn new(version: ObservedFileVersion, bytes: Arc<[u8]>) -> Self {
        let reread = fresh_reading(&bytes, version.encoding());
        Self {
            version,
            bytes,
            reread,
        }
    }

    /// Whether `text` is this version's text, as the document holds it or as a
    /// fresh read of its bytes gives it: either way the same file bytes.
    fn holds(&self, text: &str) -> bool {
        self.bytes.as_ref() == text.as_bytes() || self.reread.as_deref() == Some(text.as_bytes())
    }

    /// The text a fresh read of this version admits.
    fn reading(&self) -> &[u8] {
        self.reread.as_deref().unwrap_or(&self.bytes)
    }
}

/// Which texts an observation of the same bytes keeps the document's reading
/// and mapping for ([`DocumentPersistenceStore::accept_observation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SameReading {
    /// The document's reading or a fresh read's: the buffer is left as it is.
    Either,
    /// The document's reading alone: the buffer now holds the text accepted.
    Exact,
}

/// The text admission reads back from the bytes a document holding `text`
/// with `encoding` saves, when that is not `text` itself.
fn fresh_reading(text: &[u8], encoding: DiskEncoding) -> Option<Arc<[u8]>> {
    let unmarked_mark = !encoding.bom && text.starts_with(&crate::io::UTF8_BOM);
    let held_crlf = matches!(encoding.eol, crate::io::LineEnding::Lf)
        && matches!(crate::io::line_ending_of(text), crate::io::LineEnding::CrLf);
    if !unmarked_mark && !held_crlf {
        return None;
    }
    let text = std::str::from_utf8(text).ok()?;
    let reading = decode_utf8(encode_for_disk(text, encoding)).ok()?.text;
    (reading != text).then(|| Arc::from(reading.as_bytes()))
}

#[derive(Clone, Debug)]
pub struct PendingDocumentSave {
    pub grant: DocumentGrantId,
    pub plan: SavePlan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistenceError {
    UnknownDocument,
    GrantMismatch,
    Save(SaveError),
}

impl DocumentPersistenceStore {
    pub fn register(
        &mut self,
        document: DocumentId,
        grant: DocumentGrantId,
        observed: ObservedFileVersion,
        text: &str,
    ) -> Result<(), PersistenceError> {
        if let Some(existing) = self.documents.get(&document) {
            return (existing.grant == grant)
                .then_some(())
                .ok_or(PersistenceError::GrantMismatch);
        }
        self.documents.insert(
            document,
            DocumentPersistence {
                grant,
                save: SaveReducer::new(document, observed),
                observed_image: observed
                    .exists
                    .then(|| ObservedDocumentImage::new(observed, Arc::from(text.as_bytes()))),
            },
        );
        Ok(())
    }

    pub fn begin(
        &mut self,
        snapshot: &DocumentSnapshot,
    ) -> Result<PendingDocumentSave, PersistenceError> {
        let persistence = self
            .documents
            .get_mut(&snapshot.id)
            .ok_or(PersistenceError::UnknownDocument)?;
        let plan = persistence
            .save
            .begin(snapshot)
            .map_err(PersistenceError::Save)?;
        Ok(PendingDocumentSave {
            grant: persistence.grant,
            plan,
        })
    }

    pub fn complete(
        &mut self,
        document: DocumentId,
        generation: SaveGeneration,
        result: AtomicSaveResult,
    ) -> Result<SaveReduction, PersistenceError> {
        let persistence = self
            .documents
            .get_mut(&document)
            .ok_or(PersistenceError::UnknownDocument)?;
        let pending_bytes = persistence
            .save
            .pending()
            .filter(|plan| plan.generation == generation)
            .map(|plan| Arc::clone(&plan.bytes));
        let reduction = persistence.save.complete(generation, result);
        match &reduction {
            SaveReduction::Durable(_) => {
                let version = persistence.save.observed();
                persistence.observed_image =
                    pending_bytes.map(|bytes| ObservedDocumentImage::new(version, bytes));
            }
            SaveReduction::ReboundEquivalent(_)
            | SaveReduction::Conflict(_)
            | SaveReduction::Failed { .. }
            | SaveReduction::ReconcileRequired { .. }
            | SaveReduction::Cancelled
            | SaveReduction::Stale => {}
        }
        Ok(reduction)
    }

    pub fn observed(&self, document: DocumentId) -> Option<ObservedFileVersion> {
        self.documents
            .get(&document)
            .map(|persistence| persistence.save.observed())
    }

    /// Whether `text` is the exact text of the current observed version.
    pub fn observed_text_matches(&self, document: DocumentId, text: &str) -> bool {
        self.documents.get(&document).is_some_and(|persistence| {
            persistence.save.observed().exists
                && persistence.observed_image.as_ref().is_some_and(|image| {
                    image.version == persistence.save.observed() && image.holds(text)
                })
        })
    }

    /// [`Self::observed_text_matches`] against the last exact image, which a fingerprint-only rebind leaves at its older version.
    pub fn last_observed_text_matches(&self, document: DocumentId, text: &str) -> bool {
        self.documents.get(&document).is_some_and(|persistence| {
            persistence
                .observed_image
                .as_ref()
                .is_some_and(|image| image.holds(text))
        })
    }

    /// The fingerprint of the text a fresh read of the observed version admits, which is what reopening the file compares a draft journal's base with.
    pub fn observed_text_fingerprint(&self, document: DocumentId) -> Option<ContentFingerprint> {
        let persistence = self.documents.get(&document)?;
        let observed = persistence.save.observed();
        let image = persistence.observed_image.as_ref()?;
        (observed.exists && crate::io::content_is_equivalent(image.version, observed))
            .then(|| ContentFingerprint::of(image.reading()))
    }

    pub fn save_in_flight(&self, document: DocumentId) -> bool {
        self.documents
            .get(&document)
            .is_some_and(|persistence| persistence.save.pending().is_some())
    }

    /// Drop `document`'s saver, which nothing is saving through ([`Self::save_in_flight`] is false), and name the grant it held.
    pub fn forget(&mut self, document: DocumentId) -> Option<DocumentGrantId> {
        debug_assert!(!self.save_in_flight(document));
        self.documents
            .remove(&document)
            .map(|persistence| persistence.grant)
    }

    pub fn grant(&self, document: DocumentId) -> Option<DocumentGrantId> {
        self.documents
            .get(&document)
            .map(|persistence| persistence.grant)
    }

    /// Rebind `document`'s baseline to `observed`, whose exact text is `text` (a fresh read), leaving the buffer as it is.
    pub fn accept_observation(
        &mut self,
        document: DocumentId,
        observed: ObservedFileVersion,
        text: &str,
    ) -> Result<(), PersistenceError> {
        self.rebind(document, observed, text, SameReading::Either)
    }

    /// [`Self::accept_observation`] for an observation whose fresh read the caller has just made the buffer's text (a reload, or a discard of local edits).
    pub fn accept_reloaded_observation(
        &mut self,
        document: DocumentId,
        observed: ObservedFileVersion,
        text: &str,
    ) -> Result<(), PersistenceError> {
        self.rebind(document, observed, text, SameReading::Exact)
    }

    /// Rebind `document`'s baseline to `observed`, a fresh read of a file that changed under the buffer, for a save that replaces that version with the buffer's text.
    pub fn accept_overwrite(
        &mut self,
        document: DocumentId,
        observed: ObservedFileVersion,
    ) -> Result<(), PersistenceError> {
        let persistence = self
            .documents
            .get_mut(&document)
            .ok_or(PersistenceError::UnknownDocument)?;
        let encoding = persistence.save.observed().encoding();
        persistence
            .save
            .accept_observation(observed.with_encoding(encoding))
            .map_err(PersistenceError::Save)?;
        persistence.observed_image = None;
        Ok(())
    }

    fn rebind(
        &mut self,
        document: DocumentId,
        observed: ObservedFileVersion,
        text: &str,
        same_reading: SameReading,
    ) -> Result<(), PersistenceError> {
        let persistence = self
            .documents
            .get_mut(&document)
            .ok_or(PersistenceError::UnknownDocument)?;
        let current = persistence.save.observed();
        let kept = crate::io::content_is_equivalent(current, observed)
            && persistence
                .observed_image
                .as_ref()
                .is_some_and(|image| match same_reading {
                    SameReading::Either => image.holds(text),
                    SameReading::Exact => image.bytes.as_ref() == text.as_bytes(),
                });
        if kept {
            let observed = observed.with_encoding(current.encoding());
            persistence
                .save
                .accept_observation(observed)
                .map_err(PersistenceError::Save)?;
            if let Some(image) = persistence.observed_image.as_mut() {
                image.version = observed;
            }
            return Ok(());
        }
        persistence
            .save
            .accept_observation(observed)
            .map_err(PersistenceError::Save)?;
        persistence.observed_image = observed
            .exists
            .then(|| ObservedDocumentImage::new(observed, Arc::from(text.as_bytes())));
        Ok(())
    }
}

impl DocumentGrantStore {
    pub const fn new() -> Self {
        Self {
            next_id: 1,
            grants: BTreeMap::new(),
            by_uri: BTreeMap::new(),
        }
    }

    /// Mint or upgrade the single process-local grant for one canonical file.
    #[cfg(test)]
    pub(crate) fn open_local(
        &mut self,
        uri: &str,
        access: GrantAccess,
        limit: usize,
    ) -> Result<GrantedDocument, DocumentHostError> {
        self.open_local_with_config_symlinks(uri, access, limit, false)
    }

    /// Config-Manual grant minting binds a complete logical symlink chain while retaining the ordinary document host's stricter default.
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    pub fn open_local_config(
        &mut self,
        uri: &str,
        access: GrantAccess,
        limit: usize,
    ) -> Result<GrantedDocument, DocumentHostError> {
        self.open_local_with_config_symlinks(uri, access, limit, true)
    }

    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    fn open_local_with_config_symlinks(
        &mut self,
        uri: &str,
        access: GrantAccess,
        limit: usize,
        allow_config_symlinks: bool,
    ) -> Result<GrantedDocument, DocumentHostError> {
        let admitted = admit_local_file(uri, limit, allow_config_symlinks)?;
        self.grant_admitted(admitted, access)
    }

    /// Mint or upgrade the single process-local grant for one already-admitted file.
    pub fn grant_admitted(
        &mut self,
        admitted: AdmittedDocument,
        access: GrantAccess,
    ) -> Result<GrantedDocument, DocumentHostError> {
        let AdmittedDocument {
            directory,
            canonical_uri,
            path,
            target: admitted_target,
            text,
            observed,
        } = admitted;
        let access = if directory.is_some() {
            GrantAccess::ReadOnly
        } else {
            access
        };
        let grant = if let Some(id) = self.by_uri.get(&canonical_uri).copied() {
            let grant = self
                .grants
                .get_mut(&id)
                .ok_or(DocumentHostError::UnknownGrant)?;
            validate_atomic_target(&grant.target)?;
            if grant.target.target_path != admitted_target.target_path {
                return Err(DocumentHostError::TargetRetargeted);
            }
            grant.target.retain_logical_binding(&admitted_target);
            grant.access = if directory.is_some() {
                GrantAccess::ReadOnly
            } else {
                grant.access.merged(access)
            };
            grant.clone()
        } else {
            let raw = self.next_id.max(1);
            let id = DocumentGrantId(NonZeroU64::new(raw).ok_or(DocumentHostError::MalformedUri)?);
            self.next_id = raw.checked_add(1).ok_or(DocumentHostError::MalformedUri)?;
            let grant = DocumentGrant {
                id,
                canonical_uri: canonical_uri.clone(),
                path,
                access,
                target: admitted_target,
            };
            self.by_uri.insert(canonical_uri, id);
            self.grants.insert(id, grant.clone());
            grant
        };
        Ok(GrantedDocument {
            directory,
            grant,
            text,
            observed,
        })
    }

    pub fn get(&self, id: DocumentGrantId) -> Option<&DocumentGrant> {
        self.grants.get(&id)
    }

    /// Drop the grant of a released document.
    pub fn forget(&mut self, id: DocumentGrantId) {
        if let Some(grant) = self.grants.remove(&id) {
            self.by_uri.remove(&grant.canonical_uri);
        }
    }

    pub fn cloned_grant(&self, id: DocumentGrantId) -> Option<DocumentGrant> {
        self.grants.get(&id).cloned()
    }

    /// Refresh a directory through its original path binding.
    pub fn refresh_directory(
        &self,
        id: DocumentGrantId,
        limit: usize,
    ) -> Result<amacs::store::DirectoryListing, DocumentHostError> {
        let grant = self
            .grants
            .get(&id)
            .ok_or(DocumentHostError::UnknownGrant)?;
        grant.validate_current_binding()?;
        read_directory(&grant.path, limit)
    }

    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    pub fn id_for_uri(&self, canonical_uri: &str) -> Option<DocumentGrantId> {
        self.by_uri.get(canonical_uri).copied()
    }

    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    pub fn execute_save(&self, id: DocumentGrantId, plan: &SavePlan) -> AtomicSaveResult {
        let Some(grant) = self.grants.get(&id) else {
            return failed(AtomicSaveStage::Preflight, "unknown document grant");
        };
        if !grant.access.permits_write() {
            return failed(AtomicSaveStage::Preflight, "document grant is read-only");
        }
        execute_atomic_save_after(&grant.target, plan, std::time::Duration::ZERO)
    }

    /// Re-observe one already-minted file capability without widening it or accepting a caller-supplied path.
    pub fn refresh_local(
        &self,
        id: DocumentGrantId,
        limit: usize,
    ) -> Result<GrantedDocument, DocumentHostError> {
        self.refresh_bound(id, limit, false)
    }

    /// [`Self::refresh_local`] for a document never saved, whose baseline is a missing file ([`AdmitOptions::allow_missing`]).
    pub fn refresh_local_allowing_missing(
        &self,
        id: DocumentGrantId,
        limit: usize,
    ) -> Result<GrantedDocument, DocumentHostError> {
        self.refresh_bound(id, limit, true)
    }

    fn refresh_bound(
        &self,
        id: DocumentGrantId,
        limit: usize,
        allow_missing: bool,
    ) -> Result<GrantedDocument, DocumentHostError> {
        let grant = self
            .grants
            .get(&id)
            .cloned()
            .ok_or(DocumentHostError::UnknownGrant)?;
        let contents = read_bound_file(&grant.target, limit, allow_missing)?;
        let decoded = decode_utf8(contents.bytes)?;
        Ok(GrantedDocument {
            directory: None,
            grant,
            text: decoded.text,
            observed: contents.baseline.observed.with_encoding(decoded.encoding),
        })
    }
}

/// Execute a save using only the exact capability minted by the UI-thread grant store.
pub fn execute_granted_save(grant: &DocumentGrant, plan: &SavePlan) -> AtomicSaveResult {
    execute_granted_save_with_elapsed(grant, plan, std::time::Duration::ZERO)
}

/// Execute the same granted save with time already spent waiting for its write lock.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn execute_granted_save_after(
    grant: &DocumentGrant,
    plan: &SavePlan,
    elapsed: std::time::Duration,
) -> AtomicSaveResult {
    execute_granted_save_with_elapsed(grant, plan, elapsed)
}

fn execute_granted_save_with_elapsed(
    grant: &DocumentGrant,
    plan: &SavePlan,
    elapsed: std::time::Duration,
) -> AtomicSaveResult {
    if !grant.access.permits_write() {
        return failed(AtomicSaveStage::Preflight, "document grant is read-only");
    }
    execute_atomic_save_after(&grant.target, plan, elapsed)
}

/// Decode file bytes into a buffer [`Decoded`], stripping any UTF-8 BOM and normalizing pure CRLF to LF.
fn decode_utf8(bytes: Vec<u8>) -> Result<Decoded, DocumentHostError> {
    // Decode after the BOM into a new String so the prover does not need to track an in-place drain range.
    if let [b0, b1, b2, text @ ..] = bytes.as_slice()
        && is_utf8_bom(*b0, *b1, *b2)
    {
        let text = std::str::from_utf8(text).map_err(|_| DocumentHostError::InvalidUtf8)?;
        return Ok(Decoded::hold(text.to_owned(), true));
    }
    let text = String::from_utf8(bytes).map_err(|_| DocumentHostError::InvalidUtf8)?;
    Ok(Decoded::hold(text, false))
}

trust_contract_fn! {
    /// Whether a file's first three bytes are the UTF-8 byte order mark, EF BB BF
    /// (U+FEFF), the one prefix admission takes off before the text is decoded.
    #[must_use]
    fn is_utf8_bom(b0: u8, b1: u8, b2: u8) -> bool
    clauses [ensures result == (b0 == 239 && (b1 == 187 && b2 == 191)) by is_utf8_bom_is_ef_bb_bf]
    {
        b0 == 0xef && b1 == 0xbb && b2 == 0xbf
    }
}

verify_island! {
    clean {
        theorem is_utf8_bom_is_ef_bb_bf (b0 : UInt8) (b1 : UInt8) (b2 : UInt8) :
            trust_import_amacs_files__host__is_utf8_bom b0 b1 b2
                = (decide (b0 = 239) && (decide (b1 = 187) && decide (b2 = 191))) := by
          simp only [trust_import_amacs_files__host__is_utf8_bom]
    }
}

/// The absolute path a `file://` URI names: amacs's codec ([`amacs::protocol::uri`]), which the `edit` client spells its URIs with.
pub fn file_uri_path(uri: &str) -> Result<PathBuf, DocumentHostError> {
    amacs::protocol::uri::file_uri_to_path(uri).map_err(DocumentHostError::from)
}

/// RFC 8089 drive paths carry one URI root slash (`/C:/...`) which is not a
/// Windows path root; [`amacs::protocol::uri::windows_drive_path_text`] holds
/// the rule, which `file_uri_path` applies on Windows.
#[cfg(test)]
fn windows_file_uri_path_text(decoded: &str) -> Result<String, DocumentHostError> {
    amacs::protocol::uri::windows_drive_path_text(decoded).map_err(DocumentHostError::from)
}

/// The canonical `file://` spelling of an absolute path (amacs's codec).
pub fn path_to_file_uri(path: &Path) -> Result<String, DocumentHostError> {
    amacs::protocol::uri::path_to_file_uri(path).map_err(DocumentHostError::from)
}

impl From<amacs::protocol::uri::UriError> for DocumentHostError {
    fn from(error: amacs::protocol::uri::UriError) -> Self {
        use amacs::protocol::uri::UriError;
        match error {
            UriError::UnsupportedScheme => Self::UnsupportedScheme,
            UriError::RemoteAuthority => Self::RemoteAuthority,
            UriError::Malformed => Self::MalformedUri,
            UriError::InvalidEncoding => Self::InvalidEncoding,
            UriError::NotAbsolute => Self::NotAbsolute,
        }
    }
}

enum RegularReadHandle {
    Missing,
    Open { file: File, metadata: fs::Metadata },
}

fn open_read_error(path: &Path, error: std::io::Error) -> DocumentHostError {
    DocumentHostError::Io {
        stage: AtomicSaveStage::Preflight,
        kind: Some(error.kind()),
        message: format!("could not open {}: {error}", path.display()),
    }
}

/// Open one final physical target without ever waiting for a special file.
#[cfg(unix)]
fn open_regular_read(path: &Path) -> Result<RegularReadHandle, DocumentHostError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    regular_read_opened(path, options.open(path))
}

/// What a no-follow, non-blocking open for reading of the target at `path` met: the regular
/// file, nothing there, or why not.
#[cfg(unix)]
fn regular_read_opened(
    path: &Path,
    opened: std::io::Result<File>,
) -> Result<RegularReadHandle, DocumentHostError> {
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RegularReadHandle::Missing);
        }
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(DocumentHostError::SymlinkComponent {
                path: path.to_path_buf(),
                dangling: false,
            });
        }
        Err(error) => return Err(open_read_error(path, error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| open_read_error(path, error))?;
    if !metadata.file_type().is_file() {
        return Err(DocumentHostError::NotAFile);
    }
    Ok(RegularReadHandle::Open { file, metadata })
}

#[cfg(windows)]
fn open_regular_read(path: &Path) -> Result<RegularReadHandle, DocumentHostError> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RegularReadHandle::Missing);
        }
        Err(error) => return Err(open_read_error(path, error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| open_read_error(path, error))?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(DocumentHostError::NotAFile);
    }
    Ok(RegularReadHandle::Open { file, metadata })
}

#[cfg(not(any(unix, windows)))]
fn open_regular_read(path: &Path) -> Result<RegularReadHandle, DocumentHostError> {
    let before_path = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RegularReadHandle::Missing);
        }
        Err(error) => return Err(open_read_error(path, error)),
    };
    if !before_path.file_type().is_file() {
        return Err(DocumentHostError::NotAFile);
    }
    let file = File::open(path).map_err(|error| open_read_error(path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| open_read_error(path, error))?;
    let after_path = fs::symlink_metadata(path).map_err(|error| open_read_error(path, error))?;
    if !metadata.file_type().is_file()
        || !after_path.file_type().is_file()
        || metadata_token(&before_path) != metadata_token(&metadata)
        || metadata_token(&after_path) != metadata_token(&metadata)
    {
        return Err(DocumentHostError::ChangedWhileReading);
    }
    Ok(RegularReadHandle::Open { file, metadata })
}

/// Classify one failed `read(2)` on an open regular file, mapping `EDEADLK` to [`DocumentHostError::NotDownloaded`].
fn read_error(path: &Path, error: std::io::Error) -> DocumentHostError {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::EDEADLK) {
        return DocumentHostError::NotDownloaded {
            path: path.to_path_buf(),
        };
    }
    DocumentHostError::Io {
        stage: AtomicSaveStage::Preflight,
        kind: Some(error.kind()),
        message: format!("could not read {}: {error}", path.display()),
    }
}

fn bounded_read(
    path: &Path,
    file: &mut File,
    metadata: &fs::Metadata,
    limit: usize,
) -> Result<(Vec<u8>, fs::Metadata), DocumentHostError> {
    let sentinel_limit = limit.saturating_add(1);
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len().saturating_add(1))
            .unwrap_or(sentinel_limit)
            .min(sentinel_limit)
            .min(READ_RESERVE_CEILING),
    );
    std::io::Read::by_ref(file)
        .take(u64::try_from(sentinel_limit).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|error| read_error(path, error))?;
    let after = file.metadata().map_err(|error| DocumentHostError::Io {
        stage: AtomicSaveStage::Preflight,
        kind: Some(error.kind()),
        message: error.to_string(),
    })?;
    if !after.file_type().is_file() || metadata_token(metadata) != metadata_token(&after) {
        return Err(DocumentHostError::ChangedWhileReading);
    }
    Ok((bytes, after))
}

/// What a read holds the file it opened to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Admit {
    /// Any regular file.
    Any,
    /// `aterm.toml`, by the one rule every reader of it holds
    /// ([`aterm_types::dirs::aterm_config_refusal`]): this user's or root's, with no group or
    /// other write bit.
    AtermConfig,
}

fn read_stable_file(
    path: &Path,
    limit: usize,
    allow_missing: bool,
    admit: Admit,
) -> Result<(Vec<u8>, ObservedFileVersion), DocumentHostError> {
    let RegularReadHandle::Open { mut file, metadata } = open_regular_read(path)? else {
        if allow_missing {
            return Ok((Vec::new(), ObservedFileVersion::missing()));
        }
        return Err(DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            kind: Some(std::io::ErrorKind::NotFound),
            message: format!("{}{MISSING_SUFFIX}", path.display()),
        });
    };
    #[cfg(unix)]
    if admit == Admit::AtermConfig
        && let Some(why) = aterm_types::dirs::aterm_config_file_refusal(&metadata)
    {
        return Err(DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            kind: Some(std::io::ErrorKind::PermissionDenied),
            message: format!("{}: {why}", path.display()),
        });
    }
    #[cfg(not(unix))]
    let _ = admit;
    let (bytes, after) = bounded_read(path, &mut file, &metadata, limit)?;
    if bytes.len() > limit {
        return Err(DocumentHostError::TooLarge { limit });
    }
    if after.len() != bytes.len() as u64 {
        return Err(DocumentHostError::ChangedWhileReading);
    }
    let observed = observed_version(&after, &bytes);
    Ok((bytes, observed))
}

/// Read one logical path and capture the exact target + file generation later used by [`commit_atomic_bytes`].
#[cfg(any(test, debug_assertions, feature = "test-support"))]
pub fn read_atomic_file(
    logical_path: &Path,
    limit: usize,
    allow_missing: bool,
) -> Result<AtomicFileContents, DocumentHostError> {
    read_atomic_file_with_config_symlinks(logical_path, limit, allow_missing, false)
}

/// Manual/config-only counterpart of [`read_atomic_file`]: `aterm.toml`, a symlink to it
/// followed, read only from a file this user or root owns that no other user can write, by the
/// rule every reader of it holds ([`aterm_types::dirs::aterm_config_refusal`]: the `[swarm]`
/// switch and Commons' policy read it so too).
pub fn read_config_atomic_file(
    logical_path: &Path,
    limit: usize,
    allow_missing: bool,
) -> Result<AtomicFileContents, DocumentHostError> {
    read_atomic_file_with_config_symlinks(logical_path, limit, allow_missing, true)
}

fn read_atomic_file_with_config_symlinks(
    logical_path: &Path,
    limit: usize,
    allow_missing: bool,
    allow_config_symlinks: bool,
) -> Result<AtomicFileContents, DocumentHostError> {
    let target = resolve_atomic_target(logical_path, allow_config_symlinks)?;
    // The config reads are `aterm.toml`'s, held to the rule every reader of it holds.
    let admit = if allow_config_symlinks {
        Admit::AtermConfig
    } else {
        Admit::Any
    };
    let (bytes, observed) = read_stable_file(target.target_path(), limit, allow_missing, admit)?;
    validate_atomic_target(&target)?;
    Ok(AtomicFileContents {
        baseline: AtomicFileBaseline { target, observed },
        bytes,
    })
}

fn read_bound_file(
    target: &AtomicFileTarget,
    limit: usize,
    allow_missing: bool,
) -> Result<AtomicFileContents, DocumentHostError> {
    validate_atomic_target(target)?;
    let (bytes, observed) =
        read_stable_file(target.target_path(), limit, allow_missing, Admit::Any)?;
    validate_atomic_target(target)?;
    Ok(AtomicFileContents {
        baseline: AtomicFileBaseline {
            target: target.clone(),
            observed,
        },
        bytes,
    })
}

fn resolve_atomic_target(
    logical_path: &Path,
    allow_config_symlinks: bool,
) -> Result<AtomicFileTarget, DocumentHostError> {
    if !logical_path.is_absolute() {
        return Err(DocumentHostError::NotAbsolute);
    }
    if logical_path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(DocumentHostError::MalformedUri);
    }
    let normalized = normalize_process_temp_root(logical_path)?;
    let logical_symlinks = logical_symlink_bindings(&normalized)?;
    if !allow_config_symlinks && let Some(binding) = logical_symlinks.first() {
        return Err(DocumentHostError::SymlinkComponent {
            path: binding.path.clone(),
            dangling: false,
        });
    }
    let target_path = canonical_target_path(&normalized)?;
    let confirmed_symlinks = logical_symlink_bindings(&normalized)?;
    if !allow_config_symlinks && let Some(binding) = confirmed_symlinks.first() {
        return Err(DocumentHostError::SymlinkComponent {
            path: binding.path.clone(),
            dangling: false,
        });
    }
    if confirmed_symlinks != logical_symlinks {
        return Err(DocumentHostError::TargetRetargeted);
    }
    let confirmed = canonical_target_path(&normalized)?;
    if confirmed != target_path {
        return Err(DocumentHostError::TargetRetargeted);
    }
    reject_symlink_components(&target_path)?;
    Ok(AtomicFileTarget {
        logical_path: logical_path.to_path_buf(),
        target_path,
        logical_symlinks,
        additional_logical_bindings: Vec::new(),
    })
}

fn canonical_target_path(path: &Path) -> Result<PathBuf, DocumentHostError> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            canonicalize_allow_missing(path)
        }
        Err(error) => Err(DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            kind: Some(error.kind()),
            message: error.to_string(),
        }),
    }
}

/// Rewrite the std-provided process temporary root to its canonical spelling.
fn normalize_process_temp_root(path: &Path) -> Result<PathBuf, DocumentHostError> {
    let temporary = std::env::temp_dir();
    let Ok(suffix) = path.strip_prefix(&temporary) else {
        return Ok(path.to_path_buf());
    };
    let root = fs::canonicalize(&temporary).map_err(|error| DocumentHostError::Io {
        stage: AtomicSaveStage::Preflight,
        kind: Some(error.kind()),
        message: format!(
            "could not canonicalize the process temporary root {}: {error}",
            temporary.display()
        ),
    })?;
    Ok(root.join(suffix))
}

/// Walk one absolute path component by component and hand every component that exists to
/// `visit`, with its `symlink_metadata`.
fn walk_path_components<F>(path: &Path, mut visit: F) -> Result<(), DocumentHostError>
where
    F: FnMut(&Path, &fs::Metadata) -> Result<(), DocumentHostError>,
{
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component.as_os_str());
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        let is_root = matches!(component, std::path::Component::RootDir);
        let metadata = match fs::symlink_metadata(&prefix) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) if is_root && root_stat_is_tolerable(&error) => continue,
            Err(error) => {
                return Err(DocumentHostError::Io {
                    stage: AtomicSaveStage::Preflight,
                    kind: Some(error.kind()),
                    message: format!("could not validate {}: {error}", prefix.display()),
                });
            }
        };
        visit(&prefix, &metadata)?;
    }
    Ok(())
}

/// Whether a failure to stat a *root* component should be walked past instead of
/// failing the admission.
#[cfg(windows)]
fn root_stat_is_tolerable(error: &std::io::Error) -> bool {
    const ERROR_INVALID_FUNCTION: i32 = 1;
    const ERROR_INVALID_NAME: i32 = 123;
    matches!(
        error.raw_os_error(),
        Some(ERROR_INVALID_FUNCTION | ERROR_INVALID_NAME)
    )
}

#[cfg(not(windows))]
const fn root_stat_is_tolerable(_error: &std::io::Error) -> bool {
    false
}

fn reject_symlink_components(path: &Path) -> Result<(), DocumentHostError> {
    walk_path_components(path, |prefix, metadata| {
        if metadata.file_type().is_symlink() {
            return Err(DocumentHostError::SymlinkComponent {
                path: prefix.to_path_buf(),
                dangling: false,
            });
        }
        Ok(())
    })
}

/// Collect every existing symbolic-link component in the caller's logical
/// spelling.
fn logical_symlink_bindings(path: &Path) -> Result<Vec<LogicalSymlinkBinding>, DocumentHostError> {
    let mut bindings = Vec::new();
    walk_path_components(path, |prefix, metadata| {
        if !metadata.file_type().is_symlink() {
            return Ok(());
        }
        let destination = fs::read_link(prefix).map_err(|error| DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            kind: Some(error.kind()),
            message: format!("could not read symbolic link {}: {error}", prefix.display()),
        })?;
        fs::canonicalize(prefix).map_err(|error| DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            kind: Some(error.kind()),
            message: format!(
                "could not resolve symbolic link {}: {error}",
                prefix.display()
            ),
        })?;
        bindings.push(LogicalSymlinkBinding {
            path: prefix.to_path_buf(),
            destination,
            identity: file_identity(metadata),
            modified_ns: modified_ns(metadata),
            len: metadata.len(),
        });
        Ok(())
    })?;
    Ok(bindings)
}

fn canonicalize_allow_missing(path: &Path) -> Result<PathBuf, DocumentHostError> {
    let mut cursor = path;
    let mut suffix = Vec::new();
    loop {
        match fs::canonicalize(cursor) {
            Ok(mut existing) => {
                for component in suffix.iter().rev() {
                    existing.push(component);
                }
                return Ok(existing);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor.file_name().ok_or_else(|| DocumentHostError::Io {
                    stage: AtomicSaveStage::Preflight,
                    kind: None,
                    message: format!("{} has no existing ancestor", path.display()),
                })?;
                suffix.push(name.to_os_string());
                cursor = cursor.parent().ok_or_else(|| DocumentHostError::Io {
                    stage: AtomicSaveStage::Preflight,
                    kind: None,
                    message: format!("{} has no parent", path.display()),
                })?;
            }
            Err(error) => {
                return Err(DocumentHostError::Io {
                    stage: AtomicSaveStage::Preflight,
                    kind: Some(error.kind()),
                    message: error.to_string(),
                });
            }
        }
    }
}

fn validate_atomic_target(target: &AtomicFileTarget) -> Result<(), DocumentHostError> {
    validate_logical_binding(
        &LogicalPathBinding {
            logical_path: target.logical_path.clone(),
            logical_symlinks: target.logical_symlinks.clone(),
        },
        &target.target_path,
    )?;
    for binding in &target.additional_logical_bindings {
        validate_logical_binding(binding, &target.target_path)?;
    }
    Ok(())
}

fn validate_logical_binding(
    binding: &LogicalPathBinding,
    target_path: &Path,
) -> Result<(), DocumentHostError> {
    let current =
        resolve_atomic_target(&binding.logical_path, !binding.logical_symlinks.is_empty())?;
    if current.target_path != target_path || current.logical_symlinks != binding.logical_symlinks {
        return Err(DocumentHostError::TargetRetargeted);
    }
    Ok(())
}

/// Observe enough bytes to decide equality against a known bounded generation.
pub fn observe_file(path: &Path, content_limit: usize) -> Result<ObservedFileVersion, String> {
    observe_target(path, content_limit).map_err(|failure| failure.message)
}

/// [`observe_file`], its failure keeping the I/O error behind a refused read
/// for a commit to report ([`StepFailure::read`]).
fn observe_target(path: &Path, content_limit: usize) -> Result<ObservedFileVersion, StepFailure> {
    observe_opened(path, open_regular_read(path), content_limit)
}

/// [`observe_target`] of the target at `path` as its open for reading met it (`opened`).
fn observe_opened(
    path: &Path,
    opened: Result<RegularReadHandle, DocumentHostError>,
    content_limit: usize,
) -> Result<ObservedFileVersion, StepFailure> {
    let opened = opened.map_err(|error| StepFailure::read(&error))?;
    let RegularReadHandle::Open { mut file, metadata } = opened else {
        return Ok(ObservedFileVersion::missing());
    };
    let (bytes, after) = bounded_read(path, &mut file, &metadata, content_limit)
        .map_err(|error| StepFailure::read(&error))?;
    if bytes.len() <= content_limit && after.len() != bytes.len() as u64 {
        return Err(StepFailure::own(
            "target changed while it was being observed",
        ));
    }
    Ok(ObservedFileVersion {
        exists: true,
        identity: file_identity(&after),
        modified_ns: modified_ns(&after),
        len: after.len(),
        content: ContentFingerprint::of(&bytes),
        encoding: DiskEncoding::PLAIN,
    })
}

fn observation_limit(len: u64) -> usize {
    usize::try_from(len)
        .unwrap_or(MAX_ATOMIC_SAVE_BYTES)
        .min(MAX_ATOMIC_SAVE_BYTES)
}

fn observed_version(metadata: &fs::Metadata, bytes: &[u8]) -> ObservedFileVersion {
    ObservedFileVersion::observed(bytes, file_identity(metadata), modified_ns(metadata))
}

fn metadata_token(metadata: &fs::Metadata) -> (Option<u128>, Option<u64>, u64) {
    (
        file_identity(metadata),
        modified_ns(metadata),
        metadata.len(),
    )
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> Option<u128> {
    use std::os::unix::fs::MetadataExt as _;
    Some((u128::from(metadata.dev()) << 64) | u128::from(metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> Option<u128> {
    None
}

fn modified_ns(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
}

fn execute_atomic_save_after(
    target: &AtomicFileTarget,
    plan: &SavePlan,
    elapsed: std::time::Duration,
) -> AtomicSaveResult {
    match commit_atomic_bytes_with_lock_elapsed(
        &AtomicFileBaseline {
            target: target.clone(),
            observed: plan.expected,
        },
        &plan.file_bytes,
        next_temporary_seed(),
        Flush::Durable,
        Directories::Existing,
        elapsed,
    ) {
        AtomicCommitResult::Committed(proof) => AtomicSaveResult::Committed(proof),
        AtomicCommitResult::Conflict { observed, .. } => AtomicSaveResult::Conflict {
            observed,
            equivalent_rebind_allowed: validate_atomic_target(target).is_ok(),
        },
        AtomicCommitResult::Failed {
            stage,
            message,
            cause,
        } => AtomicSaveResult::Failed {
            stage,
            message,
            cause,
        },
        AtomicCommitResult::PublishedUnverified {
            stage,
            observed,
            message,
            cause,
        } => AtomicSaveResult::PublishedUnverified {
            stage,
            observed,
            message,
            cause,
        },
    }
}

fn write_lock_not_regular(path: &Path) -> String {
    format!(
        "save lock {} is not a regular non-link file; remove the hostile entry and retry Save",
        path.display()
    )
}

/// The save lock every commit of `path` (in `parent`) takes, a hidden
/// sibling of it.
fn save_lock_path(path: &Path, parent: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_else(|| OsStr::new("document"));
    parent.join(save_lock_name(name))
}

/// The name of the save lock of the file `name`, beside it.
fn save_lock_name(name: &OsStr) -> String {
    format!(".{}.aterm-write.lock", name.to_string_lossy())
}

/// The save lock a commit of `path` takes, which a person may have to remove when it is another user's ([`SaveTouch::Lock`]).
pub fn save_lock_of(path: &Path) -> Option<PathBuf> {
    path.parent().map(|parent| save_lock_path(path, parent))
}

/// What a failed open of the save lock at `path` was doing: opening the lock
/// already there (another user's, say), or making it in the folder.
#[cfg(not(unix))]
fn lock_open_touch(path: &Path) -> SaveTouch {
    if fs::symlink_metadata(path).is_ok() {
        SaveTouch::Lock
    } else {
        SaveTouch::Folder
    }
}

/// Open one sibling write lock without following or waiting on its final
/// component.
#[cfg(unix)]
fn ensure_write_lock_owner(
    path: &Path,
    owner_uid: u32,
    effective_uid: u32,
) -> Result<(), StepFailure> {
    if owner_uid != effective_uid {
        return Err(StepFailure::met(
            SaveTouch::Lock,
            format!("save lock {} is not owned by this user", path.display()),
            std::io::ErrorKind::PermissionDenied,
        ));
    }
    Ok(())
}

/// The save lock at `path`, opened in its folder (by its canonical path, as a commit's target
/// names it) as a commit opens it ([`Folder::open_write_lock`]).
#[cfg(all(test, unix))]
fn open_write_lock(path: &Path) -> Result<File, StepFailure> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(StepFailure::own("save lock has no folder"));
    };
    let folder = fs::canonicalize(parent)
        .and_then(|parent| Folder::open(&parent, false))
        .map_err(|error| StepFailure::io(SaveTouch::Folder, error.to_string(), &error))?;
    folder.open_write_lock(name, path)
}

/// Keep the save lock just opened at `path` only if it is a regular file this user owns, made
/// owner-only.
#[cfg(unix)]
fn admit_write_lock(file: File, path: &Path) -> Result<File, StepFailure> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let metadata = file.metadata().map_err(|error| {
        StepFailure::io(
            SaveTouch::Lock,
            format!("could not inspect save lock {}: {error}", path.display()),
            &error,
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(StepFailure::own(write_lock_not_regular(path)));
    }
    // SAFETY: `geteuid` takes no pointers or preconditions and always returns the effective UID.
    let effective_uid = unsafe { libc::geteuid() };
    ensure_write_lock_owner(path, metadata.uid(), effective_uid)?;
    if metadata.permissions().mode() & 0o7777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                StepFailure::io(
                    SaveTouch::Lock,
                    format!("could not protect save lock {}: {error}", path.display()),
                    &error,
                )
            })?;
    }
    Ok(file)
}

#[cfg(windows)]
fn open_write_lock(path: &Path) -> Result<File, StepFailure> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| {
            StepFailure::io(
                lock_open_touch(path),
                format!("could not open save lock {}: {error}", path.display()),
                &error,
            )
        })?;
    let metadata = file.metadata().map_err(|error| {
        StepFailure::io(
            SaveTouch::Lock,
            format!("could not inspect save lock {}: {error}", path.display()),
            &error,
        )
    })?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(StepFailure::own(write_lock_not_regular(path)));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_write_lock(path: &Path) -> Result<File, StepFailure> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(StepFailure::own(write_lock_not_regular(path)));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(StepFailure::io(
                SaveTouch::Lock,
                format!("could not inspect save lock {}: {error}", path.display()),
                &error,
            ));
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| {
            StepFailure::io(
                lock_open_touch(path),
                format!("could not open save lock {}: {error}", path.display()),
                &error,
            )
        })?;
    if !file
        .metadata()
        .map_err(|error| {
            StepFailure::io(
                SaveTouch::Lock,
                format!("could not inspect save lock {}: {error}", path.display()),
                &error,
            )
        })?
        .file_type()
        .is_file()
    {
        return Err(StepFailure::own(write_lock_not_regular(path)));
    }
    Ok(file)
}

/// The one serialized commit authority for Manual document saves and every structured Settings write.
pub fn commit_atomic_bytes(baseline: &AtomicFileBaseline, bytes: &[u8]) -> AtomicCommitResult {
    commit_atomic_bytes_with_seed(
        baseline,
        bytes,
        next_temporary_seed(),
        Flush::Durable,
        Directories::Create,
    )
}

/// [`commit_atomic_bytes`] with its temporary and its directory entry only ORDERED (`aterm_durable::ordered`) ahead of what follows, not made durable.
pub fn commit_atomic_bytes_ordered(
    baseline: &AtomicFileBaseline,
    bytes: &[u8],
) -> AtomicCommitResult {
    commit_atomic_bytes_with_seed(
        baseline,
        bytes,
        next_temporary_seed(),
        Flush::Ordered,
        Directories::Create,
    )
}

/// Whether a commit may make the directories above its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Directories {
    /// Make any that are missing: a config file belongs where its folder does
    /// (`~/.config/aterm`), which the first write may have to make.
    Create,
    /// Make none: a document's save writes its one file. A missing directory
    /// means an existing file went with it (a conflict), and fails the first
    /// save of a missing file.
    Existing,
}

impl Directories {
    /// The mode a commit's temporary is created with, which the umask narrows; a target that
    /// exists then gives the temporary its own mode. A config commit's ([`Self::Create`]) is
    /// born with no group or other write bit, so a missing `aterm.toml` it makes passes the rule
    /// every reader of it holds ([`aterm_types::dirs::aterm_config_refusal`]) under any umask,
    /// a user-private-group login's `002` included, where `0o666` would make it `664` and every
    /// later read refuse it. A document's save keeps `0o666`: the umask decides, as an editor's.
    const fn new_file_mode(self) -> u32 {
        match self {
            Self::Create => 0o644,
            Self::Existing => 0o666,
        }
    }
}

fn write_lock_busy() -> String {
    "save target is busy; retry Save after the other writer finishes".to_string()
}

/// Optional journal work on the event loop yields promptly to a sibling writer.
pub const WRITE_LOCK_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(25);

/// A required save waits for its sibling writer, with a minute hang detector.
const SAVE_LOCK_HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// Why [`acquire_advisory_lock_within`] gave up.
pub enum AdvisoryLockRefusal {
    Busy,
    Failed(std::io::Error),
}

/// THE bounded advisory-lock retry for this crate and aterm-gui.
pub fn acquire_advisory_lock_within(
    lock_file: File,
    budget: std::time::Duration,
) -> Result<HeldAdvisoryLock, AdvisoryLockRefusal> {
    acquire_advisory_lock_within_clock(lock_file, budget, std::time::Instant::now)
}

/// The same native advisory-lock wait, with its monotonic clock supplied by the caller.
///
/// # Errors
/// [`AdvisoryLockRefusal::Busy`] when a held lock outlasts the budget, or
/// [`AdvisoryLockRefusal::Failed`] for another native lock error.
pub fn acquire_advisory_lock_within_clock(
    mut lock_file: File,
    budget: std::time::Duration,
    mut now: impl FnMut() -> std::time::Instant,
) -> Result<HeldAdvisoryLock, AdvisoryLockRefusal> {
    let deadline = now().checked_add(budget);
    let mut backoff = std::time::Duration::from_millis(1);
    loop {
        match HeldAdvisoryLock::try_lock_keeping(lock_file) {
            Ok(held) => return Ok(held),
            Err((_, std::fs::TryLockError::Error(error))) => {
                return Err(AdvisoryLockRefusal::Failed(error));
            }
            Err((refused, std::fs::TryLockError::WouldBlock)) => {
                lock_file = refused;
                let remaining =
                    deadline.map_or(budget, |deadline| deadline.saturating_duration_since(now()));
                if remaining.is_zero() {
                    return Err(AdvisoryLockRefusal::Busy);
                }
                std::thread::sleep(backoff.min(remaining));
                backoff = backoff
                    .saturating_mul(2)
                    .min(std::time::Duration::from_millis(8));
            }
        }
    }
}

/// An advisory lock this crate or aterm-gui took.
#[derive(Debug)]
pub struct HeldAdvisoryLock(File);

impl HeldAdvisoryLock {
    /// Take `lock_file`'s exclusive lock without waiting: the held guard, or why not (`WouldBlock` when another description holds it).
    pub fn try_lock(lock_file: File) -> Result<Self, std::fs::TryLockError> {
        Self::try_lock_keeping(lock_file).map_err(|(_, refusal)| refusal)
    }

    /// [`Self::try_lock`], handing a refused file back beside the refusal: for a caller that waits and tries the same description again.
    pub fn try_lock_keeping(lock_file: File) -> Result<Self, (File, std::fs::TryLockError)> {
        let lock_file: std::fs::File = lock_file;
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        let taken = lock_file.try_lock();
        match taken {
            Ok(()) => Ok(Self(lock_file)),
            Err(refusal) => Err((lock_file, refusal)),
        }
    }

    /// Take `lock_file`'s exclusive lock, waiting for a holder to let go.
    pub fn lock(lock_file: File) -> std::io::Result<Self> {
        let lock_file: std::fs::File = lock_file;
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        lock_file.lock()?;
        Ok(Self(lock_file))
    }

    /// Underlying locked file accessed under the lock.
    pub const fn file(&self) -> &File {
        &self.0
    }
}

impl Drop for HeldAdvisoryLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The save path's rendering of [`acquire_advisory_lock_within`]: same loop, same
/// rules, this module's vocabulary.
fn acquire_write_lock_within(
    lock_file: File,
    budget: std::time::Duration,
) -> Result<HeldAdvisoryLock, StepFailure> {
    acquire_advisory_lock_within(lock_file, budget).map_err(|refusal| match refusal {
        AdvisoryLockRefusal::Busy => StepFailure::met(
            SaveTouch::Lock,
            write_lock_busy(),
            std::io::ErrorKind::WouldBlock,
        ),
        AdvisoryLockRefusal::Failed(error) => StepFailure::io(
            SaveTouch::Lock,
            format!("could not acquire save lock: {error}"),
            &error,
        ),
    })
}

fn acquire_write_lock_after(
    lock_file: File,
    elapsed: std::time::Duration,
    acquire: impl FnOnce(File, std::time::Duration) -> Result<HeldAdvisoryLock, StepFailure>,
) -> Result<HeldAdvisoryLock, StepFailure> {
    let left = SAVE_LOCK_HANG
        .checked_sub(elapsed)
        .filter(|left| !left.is_zero())
        .ok_or_else(|| {
            StepFailure::met(
                SaveTouch::Lock,
                write_lock_busy(),
                std::io::ErrorKind::WouldBlock,
            )
        })?;
    acquire(lock_file, left)
}

/// Retry an independently failed preflight at least once, then while this grace from the
/// first attempt lasts: a folder replaced between its open and its lock can settle even
/// when the first attempt alone outran the grace. The write lock has its own hang detector.
const PREFLIGHT_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// THE POINTS OF A COMMIT A TEST ACTS AT: what another process may do to the folder between
/// one step of a commit and the next.
#[cfg(test)]
#[path = "host_seam_tests.rs"]
pub(crate) mod seam;

/// The points of a commit: none acts outside the tests.
#[cfg(not(test))]
mod seam {
    #[derive(Clone, Copy)]
    pub(crate) enum Step {
        Folder,
        Opened,
        Lock,
        Temporary,
        Validate,
        Publish,
        Observe,
    }

    /// Nothing acts here.
    #[inline]
    pub(crate) const fn reached(_: Step) {}
}

/// Take the write lock and prove the target is still the generation the plan was built
/// on: the lock, the observation limit, and the target's folder ([`Folder`]), which every
/// later step of the commit works in.
fn lock_and_verify_target(
    baseline: &AtomicFileBaseline,
    directories: Directories,
    lock_elapsed: std::time::Duration,
) -> Result<(HeldAdvisoryLock, usize, Folder), AtomicCommitResult> {
    let target = &baseline.target;
    if let Err(error) = validate_atomic_target(target) {
        return Err(atomic_validation_failure(
            target,
            AtomicSaveStage::Preflight,
            error,
        ));
    }
    let path = target.target_path();
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(atomic_failed(
            AtomicSaveStage::CreateTemporary,
            StepFailure::own("target has no parent"),
        ));
    };
    seam::reached(seam::Step::Folder);
    let folder = match Folder::open(parent, directories == Directories::Create) {
        Ok(folder) => folder,
        Err(error) if folder::is_link(&error) => {
            return Err(atomic_validation_failure(
                target,
                AtomicSaveStage::Preflight,
                DocumentHostError::SymlinkComponent {
                    path: parent.to_path_buf(),
                    dangling: false,
                },
            ));
        }
        Err(error) if directories == Directories::Existing && folder::is_missing(&error) => {
            let message = format!("{}{MISSING_SUFFIX}", parent.display());
            return Err(if baseline.observed.exists {
                AtomicCommitResult::Conflict {
                    observed: ObservedFileVersion::missing(),
                    message,
                }
            } else {
                atomic_failed(
                    AtomicSaveStage::CreateTemporary,
                    StepFailure::met(SaveTouch::Folder, message, std::io::ErrorKind::NotFound),
                )
            });
        }
        Err(error) => {
            return Err(atomic_failed(
                AtomicSaveStage::CreateTemporary,
                StepFailure::io(SaveTouch::Folder, error.to_string(), &error),
            ));
        }
    };
    seam::reached(seam::Step::Opened);
    if let Err(error) = validate_atomic_target(target) {
        return Err(atomic_validation_failure(
            target,
            AtomicSaveStage::Preflight,
            error,
        ));
    }
    seam::reached(seam::Step::Lock);
    let lock_file = folder
        .open_write_lock(
            OsStr::new(&save_lock_name(name)),
            &save_lock_path(path, parent),
        )
        .map_err(|failure| atomic_failed(AtomicSaveStage::Preflight, failure))?;
    let lock = acquire_write_lock_after(lock_file, lock_elapsed, acquire_write_lock_within)
        .map_err(|failure| atomic_failed(AtomicSaveStage::Preflight, failure))?;

    if let Err(error) = validate_atomic_target(target) {
        return Err(atomic_validation_failure(
            target,
            AtomicSaveStage::Preflight,
            error,
        ));
    }
    hold_folder(&folder, path)
        .map_err(|failure| atomic_failed(AtomicSaveStage::Preflight, failure))?;
    let baseline_limit = observation_limit(baseline.observed.len);
    let actual = observe_in(&folder, path, baseline_limit)
        .map_err(|failure| atomic_failed(AtomicSaveStage::Preflight, failure))?;
    if crate::io::detect_version_conflict(baseline.observed, actual).is_some() {
        return Err(AtomicCommitResult::Conflict {
            observed: actual,
            message: "target changed since it was read".to_string(),
        });
    }
    Ok((lock, baseline_limit, folder))
}

/// Whether the folder of the target at `path` is still `folder`, the one the commit opened and
/// works in (device and inode): a folder moved or replaced since, or reached through a link
/// now, fails the step as a folder that went missing while saving, and the next Save opens the
/// folder the path names then.
fn hold_folder(folder: &Folder, path: &Path) -> Result<(), StepFailure> {
    let Some(parent) = path.parent() else {
        return Err(StepFailure::own("target has no parent"));
    };
    match folder.is_reached_by(parent) {
        Ok(true) => Ok(()),
        Ok(false) => Err(StepFailure::met(
            SaveTouch::Folder,
            format!(
                "{} is no longer the folder this save opened",
                parent.display()
            ),
            std::io::ErrorKind::NotFound,
        )),
        Err(error) => Err(StepFailure::io(
            SaveTouch::Folder,
            format!("could not check {}: {error}", parent.display()),
            &error,
        )),
    }
}

/// [`observe_target`] of the target at `path`, read through `folder`.
fn observe_in(
    folder: &Folder,
    path: &Path,
    content_limit: usize,
) -> Result<ObservedFileVersion, StepFailure> {
    let name = path.file_name().unwrap_or_else(|| OsStr::new("document"));
    observe_opened(path, folder.open_regular_read(name, path), content_limit)
}

fn commit_atomic_bytes_with_seed(
    baseline: &AtomicFileBaseline,
    bytes: &[u8],
    seed: u64,
    flush: Flush,
    directories: Directories,
) -> AtomicCommitResult {
    commit_atomic_bytes_with_lock_elapsed(
        baseline,
        bytes,
        seed,
        flush,
        directories,
        std::time::Duration::ZERO,
    )
}

fn commit_atomic_bytes_with_lock_elapsed(
    baseline: &AtomicFileBaseline,
    bytes: &[u8],
    seed: u64,
    flush: Flush,
    directories: Directories,
    lock_elapsed: std::time::Duration,
) -> AtomicCommitResult {
    if exceeds_document_limit(bytes.len() as u64) {
        return atomic_failed(
            AtomicSaveStage::Preflight,
            StepFailure::met(
                SaveTouch::Replace,
                format!("save exceeds the {DOCUMENT_LIMIT_LEN}-byte document limit"),
                std::io::ErrorKind::FileTooLarge,
            ),
        );
    }
    let target = &baseline.target;
    let path = target.target_path();
    let name = path.file_name().unwrap_or_else(|| OsStr::new("document"));

    let deadline = std::time::Instant::now().checked_add(PREFLIGHT_RETRY_BUDGET);
    let mut backoff = std::time::Duration::from_millis(1);
    let mut retried = false;
    let (_lock_file, baseline_limit, folder) = loop {
        match lock_and_verify_target(baseline, directories, lock_elapsed) {
            Ok(ready) => break ready,
            Err(verdict) => {
                if matches!(
                    &verdict,
                    AtomicCommitResult::Failed {
                        stage: AtomicSaveStage::Preflight,
                        cause: Some(SaveCause {
                            touch: SaveTouch::Lock,
                            kind: std::io::ErrorKind::WouldBlock
                        }),
                        ..
                    }
                ) {
                    return verdict;
                }
                if !matches!(
                    verdict,
                    AtomicCommitResult::Failed {
                        stage: AtomicSaveStage::Preflight,
                        ..
                    }
                ) {
                    return verdict;
                }
                let remaining = deadline.map_or(PREFLIGHT_RETRY_BUDGET, |deadline| {
                    deadline.saturating_duration_since(std::time::Instant::now())
                });
                if remaining.is_zero() && retried {
                    return verdict;
                }
                retried = true;
                std::thread::sleep(backoff.min(remaining));
                backoff = backoff
                    .saturating_mul(2)
                    .min(std::time::Duration::from_millis(8));
            }
        }
    };

    seam::reached(seam::Step::Temporary);
    let (temporary, mut file) =
        match folder.create_temporary(name, seed, directories.new_file_mode()) {
            Ok(created) => created,
            Err(failure) => return atomic_failed(AtomicSaveStage::CreateTemporary, failure),
        };
    let result: Result<AtomicSaveProof, AtomicCommitResult> = (|| {
        if let Some(permissions) = folder.regular_permissions(name) {
            file.set_permissions(permissions).map_err(|error| {
                atomic_failed(
                    AtomicSaveStage::CreateTemporary,
                    StepFailure::io(SaveTouch::Folder, error.to_string(), &error),
                )
            })?;
        }
        file.write_all(bytes).map_err(|error| {
            atomic_failed(
                AtomicSaveStage::WriteTemporary,
                StepFailure::io(SaveTouch::Folder, error.to_string(), &error),
            )
        })?;
        aterm_gui_consent::durable::finish(&file, flush).map_err(|error| {
            atomic_failed(
                AtomicSaveStage::SyncTemporary,
                StepFailure::io(SaveTouch::Folder, error.to_string(), &error),
            )
        })?;
        drop(file);

        seam::reached(seam::Step::Validate);
        validate_atomic_target(target).map_err(|error| {
            atomic_validation_failure(target, AtomicSaveStage::RenameTarget, error)
        })?;
        hold_folder(&folder, path)
            .map_err(|failure| atomic_failed(AtomicSaveStage::RenameTarget, failure))?;
        let before_rename = observe_in(&folder, path, baseline_limit)
            .map_err(|failure| atomic_failed(AtomicSaveStage::RenameTarget, failure))?;
        if let Some(conflict) = crate::io::detect_version_conflict(baseline.observed, before_rename)
        {
            return Err(AtomicCommitResult::Conflict {
                observed: before_rename,
                message: format!("target changed before rename: {conflict:?}"),
            });
        }
        seam::reached(seam::Step::Publish);
        if let Err(error) = folder.publish(&temporary, name, before_rename.exists) {
            let after_error = observe_in(&folder, path, bytes.len()).map_err(|failure| {
                atomic_published_unverified(
                    AtomicSaveStage::RenameTarget,
                    None,
                    StepFailure::io(
                        SaveTouch::Replace,
                        format!(
                            "replacement reported {error}; its target could not be reconciled: {}",
                            failure.message
                        ),
                        &error,
                    ),
                )
            })?;
            if after_error.exists
                && after_error.len == bytes.len() as u64
                && after_error.content == ContentFingerprint::of(bytes)
            {
                return Err(atomic_published_unverified(
                    AtomicSaveStage::RenameTarget,
                    Some(after_error),
                    StepFailure::io(
                        SaveTouch::Replace,
                        format!(
                            "replacement reported {error}, but the desired bytes are now visible"
                        ),
                        &error,
                    ),
                ));
            }
            if let Some(conflict) =
                crate::io::detect_version_conflict(baseline.observed, after_error)
            {
                return Err(AtomicCommitResult::Conflict {
                    observed: after_error,
                    message: format!("target changed during rename: {conflict:?}"),
                });
            }
            return Err(atomic_failed(
                AtomicSaveStage::RenameTarget,
                StepFailure::io(SaveTouch::Replace, error.to_string(), &error),
            ));
        }
        folder.sync(flush).map_err(|error| {
            atomic_published_unverified(
                AtomicSaveStage::SyncDirectory,
                observe_in(&folder, path, bytes.len()).ok(),
                StepFailure::io(SaveTouch::Folder, error.to_string(), &error),
            )
        })?;
        seam::reached(seam::Step::Observe);
        validate_atomic_target(target)
            .map_err(|error| StepFailure::read(&error))
            .and_then(|()| hold_folder(&folder, path))
            .map_err(|failure| {
                atomic_published_unverified(
                    AtomicSaveStage::ObserveCommitted,
                    observe_in(&folder, path, bytes.len()).ok(),
                    failure,
                )
            })?;
        let observed = observe_in(&folder, path, bytes.len()).map_err(|failure| {
            atomic_published_unverified(AtomicSaveStage::ObserveCommitted, None, failure)
        })?;
        if !observed.exists
            || observed.len != bytes.len() as u64
            || observed.content != ContentFingerprint::of(bytes)
        {
            return Err(atomic_published_unverified(
                AtomicSaveStage::VerifyProof,
                Some(observed),
                StepFailure::own("committed bytes did not match the save plan"),
            ));
        }
        Ok(AtomicSaveProof {
            observed,
            temporary_synced: true,
            renamed_over_target: true,
            directory_synced: true,
        })
    })();
    if result.is_err() {
        let _ = folder.remove(&temporary);
    }
    match result {
        Ok(proof) => AtomicCommitResult::Committed(proof),
        Err(verdict) => verdict,
    }
}

fn next_temporary_seed() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = aterm_time::since_epoch().map_or(0, |duration| duration.as_nanos() as u64);
    ordinal ^ nanos.rotate_left(17)
}

fn atomic_conflict(target: &AtomicFileTarget, message: String) -> AtomicCommitResult {
    let observed = resolve_atomic_target(target.logical_path(), target.admits_config_symlinks())
        .ok()
        .and_then(|current| observe_file(current.target_path(), MAX_ATOMIC_SAVE_BYTES).ok())
        .or_else(|| observe_file(target.target_path(), MAX_ATOMIC_SAVE_BYTES).ok())
        .unwrap_or_else(ObservedFileVersion::missing);
    AtomicCommitResult::Conflict { observed, message }
}

fn atomic_validation_failure(
    target: &AtomicFileTarget,
    stage: AtomicSaveStage,
    error: DocumentHostError,
) -> AtomicCommitResult {
    match error {
        conflict @ (DocumentHostError::TargetRetargeted
        | DocumentHostError::SymlinkComponent { .. }) => {
            atomic_conflict(target, conflict.to_string())
        }
        other => atomic_failed(stage, StepFailure::read(&other)),
    }
}

fn atomic_failed(stage: AtomicSaveStage, failure: StepFailure) -> AtomicCommitResult {
    AtomicCommitResult::Failed {
        stage,
        message: failure.message,
        cause: failure.cause,
    }
}

fn atomic_published_unverified(
    stage: AtomicSaveStage,
    observed: Option<ObservedFileVersion>,
    failure: StepFailure,
) -> AtomicCommitResult {
    AtomicCommitResult::PublishedUnverified {
        stage,
        observed,
        message: failure.message,
        cause: failure.cause,
    }
}

/// Flush a directory entry change to the device with `flush`.
#[cfg(unix)]
pub fn sync_directory(path: &Path, flush: Flush) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;

    aterm_gui_consent::durable::finish(
        &OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)?,
        flush,
    )
}

#[cfg(not(unix))]
pub fn sync_directory(_path: &Path, _flush: Flush) -> std::io::Result<()> {
    // Windows rename durability comes from the file replacement call; directories cannot be opened as normal files.
    Ok(())
}

/// Publish `temporary` at `target`: replace it when it existed, or create it only if it is still absent.
#[cfg(not(windows))]
pub fn replace_file(temporary: &Path, target: &Path, target_existed: bool) -> std::io::Result<()> {
    if target_existed {
        fs::rename(temporary, target)
    } else {
        fs::hard_link(temporary, target)?;
        fs::remove_file(temporary)
    }
}

#[cfg(windows)]
pub fn replace_file(temporary: &Path, target: &Path, target_existed: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const REPLACEFILE_WRITE_THROUGH: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both UTF-16 buffers are NUL-terminated and live for the call; the
    // optional backup/exclusion/reserved pointers are null as required.
    let replaced = unsafe {
        if target_existed {
            ReplaceFileW(
                target.as_ptr(),
                temporary.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } else {
            MoveFileExW(temporary.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH)
        }
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn failed(stage: AtomicSaveStage, message: impl Into<String>) -> AtomicSaveResult {
    AtomicSaveResult::Failed {
        stage,
        message: message.into(),
        cause: None,
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
