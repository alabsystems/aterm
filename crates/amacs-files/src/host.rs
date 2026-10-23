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
    pub grant: DocumentGrant,
    pub text: String,
    pub observed: ObservedFileVersion,
}

/// One local file admitted but not yet granted.
#[derive(Clone, Debug)]
pub struct AdmittedDocument {
    pub(crate) canonical_uri: String,
    pub(crate) path: PathBuf,
    target: AtomicFileTarget,
    pub(crate) text: String,
    pub(crate) observed: ObservedFileVersion,
}

impl AdmittedDocument {
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
        canonical_uri,
        path,
        target: contents.baseline.target,
        text: decoded.text,
        observed: contents.baseline.observed.with_encoding(decoded.encoding),
    })
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
            canonical_uri,
            path,
            target: admitted_target,
            text,
            observed,
        } = admitted;
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
            grant.access = grant.access.merged(access);
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
        execute_atomic_save(&grant.target, plan)
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
            grant,
            text: decoded.text,
            observed: contents.baseline.observed.with_encoding(decoded.encoding),
        })
    }
}

/// Execute a save using only the exact capability minted by the UI-thread grant store.
pub fn execute_granted_save(grant: &DocumentGrant, plan: &SavePlan) -> AtomicSaveResult {
    if !grant.access.permits_write() {
        return failed(AtomicSaveStage::Preflight, "document grant is read-only");
    }
    execute_atomic_save(&grant.target, plan)
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
    let file = match options.open(path) {
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
        usize::try_from(metadata.len())
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

fn read_stable_file(
    path: &Path,
    limit: usize,
    allow_missing: bool,
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

/// Manual/config-only counterpart of [`read_atomic_file`].
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
    let (bytes, observed) = read_stable_file(target.target_path(), limit, allow_missing)?;
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
    let (bytes, observed) = read_stable_file(target.target_path(), limit, allow_missing)?;
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
    let opened = open_regular_read(path).map_err(|error| StepFailure::read(&error))?;
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

fn execute_atomic_save(target: &AtomicFileTarget, plan: &SavePlan) -> AtomicSaveResult {
    match commit_atomic_bytes_with_seed(
        &AtomicFileBaseline {
            target: target.clone(),
            observed: plan.expected,
        },
        &plan.file_bytes,
        next_temporary_seed(),
        Flush::Durable,
        Directories::Existing,
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
    parent.join(format!(".{}.aterm-write.lock", name.to_string_lossy()))
}

/// The save lock a commit of `path` takes, which a person may have to remove when it is another user's ([`SaveTouch::Lock`]).
pub fn save_lock_of(path: &Path) -> Option<PathBuf> {
    path.parent().map(|parent| save_lock_path(path, parent))
}

/// What a failed open of the save lock at `path` was doing: opening the lock
/// already there (another user's, say), or making it in the folder.
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
fn open_write_lock(path: &Path) -> Result<File, StepFailure> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
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
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            StepFailure::io(
                SaveTouch::Lock,
                format!("could not protect save lock {}: {error}", path.display()),
                &error,
            )
        })?;
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

fn write_lock_busy() -> String {
    "save target is busy; retry Save after the other writer finishes".to_string()
}

/// Bounded contention budget for the sibling write lock.
pub const WRITE_LOCK_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(25);

/// Why [`acquire_advisory_lock_within`] gave up.
pub enum AdvisoryLockRefusal {
    Busy,
    Failed(std::io::Error),
}

/// THE bounded advisory-lock retry for this crate and aterm-gui.
pub fn acquire_advisory_lock_within(
    mut lock_file: File,
    budget: std::time::Duration,
) -> Result<HeldAdvisoryLock, AdvisoryLockRefusal> {
    let deadline = std::time::Instant::now().checked_add(budget);
    let mut backoff = std::time::Duration::from_millis(1);
    loop {
        match HeldAdvisoryLock::try_lock_keeping(lock_file) {
            Ok(held) => return Ok(held),
            Err((_, std::fs::TryLockError::Error(error))) => {
                return Err(AdvisoryLockRefusal::Failed(error));
            }
            Err((refused, std::fs::TryLockError::WouldBlock)) => {
                lock_file = refused;
                let remaining = deadline.map_or(budget, |deadline| {
                    deadline.saturating_duration_since(std::time::Instant::now())
                });
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

fn acquire_write_lock(lock_file: File) -> Result<HeldAdvisoryLock, StepFailure> {
    acquire_write_lock_within(lock_file, WRITE_LOCK_RETRY_BUDGET)
}

/// Bounded retry for the pre-publication phase of a commit.
const PREFLIGHT_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// Take the write lock and prove the target is still the generation the plan was built
/// on.
fn lock_and_verify_target(
    baseline: &AtomicFileBaseline,
    directories: Directories,
) -> Result<(HeldAdvisoryLock, usize), AtomicCommitResult> {
    let target = &baseline.target;
    if let Err(error) = validate_atomic_target(target) {
        return Err(atomic_validation_failure(
            target,
            AtomicSaveStage::Preflight,
            error,
        ));
    }
    let path = target.target_path();
    let Some(parent) = path.parent() else {
        return Err(atomic_failed(
            AtomicSaveStage::CreateTemporary,
            StepFailure::own("target has no parent"),
        ));
    };
    match directories {
        Directories::Create => {
            if let Err(error) = fs::create_dir_all(parent) {
                return Err(atomic_failed(
                    AtomicSaveStage::CreateTemporary,
                    StepFailure::io(SaveTouch::Folder, error.to_string(), &error),
                ));
            }
        }
        Directories::Existing => {
            if !fs::metadata(parent).is_ok_and(|metadata| metadata.is_dir()) {
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
        }
    }
    if let Err(error) = validate_atomic_target(target) {
        return Err(atomic_validation_failure(
            target,
            AtomicSaveStage::Preflight,
            error,
        ));
    }
    let lock_path = save_lock_path(path, parent);
    let lock_file = open_write_lock(&lock_path)
        .map_err(|failure| atomic_failed(AtomicSaveStage::Preflight, failure))?;
    let lock = acquire_write_lock(lock_file)
        .map_err(|failure| atomic_failed(AtomicSaveStage::Preflight, failure))?;

    if let Err(error) = validate_atomic_target(target) {
        return Err(atomic_validation_failure(
            target,
            AtomicSaveStage::Preflight,
            error,
        ));
    }
    let baseline_limit = observation_limit(baseline.observed.len);
    let actual = observe_target(path, baseline_limit)
        .map_err(|failure| atomic_failed(AtomicSaveStage::Preflight, failure))?;
    if crate::io::detect_version_conflict(baseline.observed, actual).is_some() {
        return Err(AtomicCommitResult::Conflict {
            observed: actual,
            message: "target changed since it was read".to_string(),
        });
    }
    Ok((lock, baseline_limit))
}

fn commit_atomic_bytes_with_seed(
    baseline: &AtomicFileBaseline,
    bytes: &[u8],
    seed: u64,
    flush: Flush,
    directories: Directories,
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
    let Some(parent) = path.parent() else {
        return atomic_failed(
            AtomicSaveStage::CreateTemporary,
            StepFailure::own("target has no parent"),
        );
    };
    let name = path.file_name().unwrap_or_else(|| OsStr::new("document"));

    let deadline = std::time::Instant::now() + PREFLIGHT_RETRY_BUDGET;
    let mut backoff = std::time::Duration::from_millis(1);
    let (_lock_file, baseline_limit) = loop {
        match lock_and_verify_target(baseline, directories) {
            Ok(ready) => break ready,
            Err(verdict) => {
                if !matches!(
                    verdict,
                    AtomicCommitResult::Failed {
                        stage: AtomicSaveStage::Preflight,
                        ..
                    }
                ) {
                    return verdict;
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return verdict;
                }
                std::thread::sleep(backoff.min(remaining));
                backoff = backoff
                    .saturating_mul(2)
                    .min(std::time::Duration::from_millis(8));
            }
        }
    };

    let (temporary, mut file) = match create_unique_temporary(parent, name, seed) {
        Ok(created) => created,
        Err(failure) => return atomic_failed(AtomicSaveStage::CreateTemporary, failure),
    };
    let result: Result<AtomicSaveProof, AtomicCommitResult> = (|| {
        if let Ok(metadata) = fs::metadata(path) {
            fs::set_permissions(&temporary, metadata.permissions()).map_err(|error| {
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

        validate_atomic_target(target).map_err(|error| {
            atomic_validation_failure(target, AtomicSaveStage::RenameTarget, error)
        })?;
        let before_rename = observe_target(path, baseline_limit)
            .map_err(|failure| atomic_failed(AtomicSaveStage::RenameTarget, failure))?;
        if let Some(conflict) = crate::io::detect_version_conflict(baseline.observed, before_rename)
        {
            return Err(AtomicCommitResult::Conflict {
                observed: before_rename,
                message: format!("target changed before rename: {conflict:?}"),
            });
        }
        if let Err(error) = replace_file(&temporary, path, before_rename.exists) {
            let after_error = observe_file(path, bytes.len()).map_err(|message| {
                atomic_published_unverified(
                    AtomicSaveStage::RenameTarget,
                    None,
                    StepFailure::io(
                        SaveTouch::Replace,
                        format!(
                            "replacement reported {error}; its target could not be reconciled: {message}"
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
        sync_directory(parent, flush).map_err(|error| {
            atomic_published_unverified(
                AtomicSaveStage::SyncDirectory,
                observe_file(path, bytes.len()).ok(),
                StepFailure::io(SaveTouch::Folder, error.to_string(), &error),
            )
        })?;
        validate_atomic_target(target).map_err(|error| {
            atomic_published_unverified(
                AtomicSaveStage::ObserveCommitted,
                observe_file(path, bytes.len()).ok(),
                StepFailure::read(&error),
            )
        })?;
        let observed = observe_target(path, bytes.len()).map_err(|failure| {
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
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    match result {
        Ok(proof) => AtomicCommitResult::Committed(proof),
        Err(verdict) => verdict,
    }
}

fn create_unique_temporary(
    parent: &Path,
    name: &OsStr,
    seed: u64,
) -> Result<(PathBuf, File), StepFailure> {
    for attempt in 0_u64..64 {
        let temporary = parent.join(format!(
            ".{}.aterm-{}-{}-{}.tmp",
            name.to_string_lossy(),
            std::process::id(),
            seed,
            attempt
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        match options.open(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(StepFailure::io(
                    SaveTouch::Folder,
                    error.to_string(),
                    &error,
                ));
            }
        }
    }
    Err(StepFailure::own(
        "could not allocate a unique sibling temporary file",
    ))
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
mod tests {
    use super::*;
    use crate::io::{SaveReducer, SaveReduction};

    fn unique_file(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-native-document-host-{name}-"))
            .tempdir()
            .unwrap()
            .keep();
        let path = dir.join("hello world.md");
        fs::write(&path, bytes).unwrap();
        path
    }

    fn file_uri(path: &Path) -> String {
        path_to_file_uri(path).unwrap()
    }

    /// Well-formed host-absolute URI of a missing file.
    const MISSING_DOC_URI: &str = if cfg!(windows) {
        "file:///C:/definitely/missing/aterm-doc.md"
    } else {
        "file:///definitely/missing/aterm-doc.md"
    };

    /// `walk_path_components` admits a canonicalized path without stat-ing a Windows verbatim prefix.
    #[test]
    fn component_walk_admits_a_canonicalized_path() {
        let path = unique_file("canonical-walk", b"walk\n");
        let canonical = fs::canonicalize(&path).unwrap();
        #[cfg(windows)]
        {
            assert!(
                canonical.as_os_str().to_string_lossy().starts_with(r"\\?\"),
                "expected fs::canonicalize to produce a verbatim path, got {}",
                canonical.display()
            );
        }
        reject_symlink_components(&canonical).unwrap();
        assert!(logical_symlink_bindings(&canonical).unwrap().is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A temp-rooted document, as `normalize_process_temp_root` rewrites it, admits end to end.
    #[test]
    fn component_walk_admits_a_process_temporary_root_path() {
        let path = unique_file("temporary-walk", b"walk\n");
        let normalized = normalize_process_temp_root(&path).unwrap();
        assert!(logical_symlink_bindings(&normalized).unwrap().is_empty());
        reject_symlink_components(&normalized).unwrap();
        let contents = read_config_atomic_file(&path, 1024, false).unwrap();
        assert_eq!(contents.bytes, b"walk\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn local_uri_is_canonical_bounded_and_reuses_grant() {
        let path = unique_file("open", b"# hello\n");
        let mut grants = DocumentGrantStore::new();
        let uri = file_uri(&path);
        assert!(uri.contains("hello%20world.md"));
        let first = grants
            .open_local(&uri, GrantAccess::ReadOnly, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        let second = grants
            .open_local(&uri, GrantAccess::ReadWrite, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        assert_eq!(first.text, "# hello\n");
        assert_eq!(first.grant.id, second.grant.id);
        assert_eq!(second.grant.access, GrantAccess::ReadWrite);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// One document opened the way a face opens one: admitted, granted for
    /// writing, held in a store and registered for saving.
    struct OpenDocument {
        grants: DocumentGrantStore,
        persistence: DocumentPersistenceStore,
        documents: amacs::store::DocumentStore,
        document: DocumentId,
        granted: GrantedDocument,
    }

    const ALLOW_MISSING: AdmitOptions = AdmitOptions {
        allow_missing: true,
        ..AdmitOptions::DOCUMENT
    };

    fn open_document(path: &Path, options: AdmitOptions) -> OpenDocument {
        let admitted = admit_local_file_opts(&file_uri(path), options).unwrap();
        let mut grants = DocumentGrantStore::new();
        let granted = grants
            .grant_admitted(admitted, GrantAccess::ReadWrite)
            .unwrap();
        let mut documents = crate::test_document_store();
        let document = documents.open(granted.grant.canonical_uri.clone(), granted.text.clone());
        let mut persistence = DocumentPersistenceStore::default();
        persistence
            .register(document, granted.grant.id, granted.observed, &granted.text)
            .unwrap();
        OpenDocument {
            grants,
            persistence,
            documents,
            document,
            granted,
        }
    }

    impl OpenDocument {
        fn text(&self) -> String {
            self.documents
                .snapshot(self.document)
                .unwrap()
                .text
                .to_string()
        }

        fn set_text(&mut self, text: &str) {
            let before = self.documents.snapshot(self.document).unwrap();
            let outcome = self.documents.transact(
                self.document,
                before.seq,
                vec![amacs::store::TextEdit {
                    range: 0..before.text.len(),
                    insert: text.to_string(),
                }],
            );
            assert!(
                matches!(outcome, amacs::store::DocumentTxnOutcome::Committed { .. }),
                "{outcome:?}"
            );
        }

        fn save(&mut self) -> SaveReduction {
            let snapshot = self.documents.snapshot(self.document).unwrap();
            let pending = self.persistence.begin(&snapshot).unwrap();
            let result = execute_granted_save(&self.granted.grant, &pending.plan);
            self.persistence
                .complete(self.document, pending.plan.generation, result)
                .unwrap()
        }

        fn encoding(&self) -> DiskEncoding {
            self.persistence.observed(self.document).unwrap().encoding()
        }
    }

    /// With `allow_missing`, a missing file admits as empty text and is created on the first save.
    #[test]
    fn missing_with_allow_missing_is_empty_and_first_save_creates() {
        let path = unique_file("allow-missing", b"");
        fs::remove_file(&path).unwrap();
        let admitted = admit_local_file_opts(&file_uri(&path), ALLOW_MISSING).unwrap();
        assert!(admitted.is_missing());
        assert_eq!(admitted.text, "");
        assert_eq!(admitted.encoding(), DiskEncoding::PLAIN);

        let mut opened = open_document(&path, ALLOW_MISSING);
        assert!(!opened.granted.observed.exists);
        opened.set_text("first line\n");
        let snapshot = opened.documents.snapshot(opened.document).unwrap();
        let pending = opened.persistence.begin(&snapshot).unwrap();
        assert!(
            !path.exists(),
            "admission, the grant, an edit and the save plan create nothing"
        );
        let result = execute_granted_save(&opened.granted.grant, &pending.plan);
        assert!(matches!(
            opened
                .persistence
                .complete(opened.document, pending.plan.generation, result)
                .unwrap(),
            SaveReduction::Durable(checkpoint) if checkpoint.seq == snapshot.seq
        ));
        assert_eq!(fs::read(&path).unwrap(), b"first line\n");
        assert!(opened.persistence.observed(opened.document).unwrap().exists);

        opened.set_text("second\n");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), b"second\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Creating the file externally after a missing-file admission causes an existence conflict on first save.
    #[test]
    fn first_save_refuses_if_the_file_appeared() {
        let path = unique_file("missing-then-appeared", b"");
        fs::remove_file(&path).unwrap();
        let mut opened = open_document(&path, ALLOW_MISSING);
        opened.set_text("ours\n");
        fs::write(&path, b"theirs\n").unwrap();
        assert_eq!(
            opened.save(),
            SaveReduction::Conflict(crate::io::VersionConflict::Existence)
        );
        assert_eq!(fs::read(&path).unwrap(), b"theirs\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Missing files without `allow_missing` and missing parent directories with `allow_missing` are refused.
    #[test]
    fn missing_without_allow_missing_is_refused() {
        let path = unique_file("missing-refused", b"");
        fs::remove_file(&path).unwrap();
        let uri = file_uri(&path);
        for refused in [
            admit_local_file(&uri, DEFAULT_DOCUMENT_LIMIT, false),
            admit_local_file_opts(&uri, AdmitOptions::DOCUMENT),
            admit_local_file_opts(&uri, AdmitOptions::default()),
        ] {
            let error = refused.unwrap_err();
            assert_eq!(error.refusal_word(), "missing", "{error}");
        }

        let nested = path.parent().unwrap().join("no such folder").join("new.md");
        let error = admit_local_file_opts(&file_uri(&nested), ALLOW_MISSING).unwrap_err();
        assert_eq!(error.refusal_word(), "missing", "{error}");
        assert!(!nested.parent().unwrap().exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A byte order mark is not in the text, and every save puts it back.
    #[test]
    fn bom_round_trips_byte_exact() {
        let bytes = b"\xef\xbb\xbf# title\nbody\n";
        let path = unique_file("bom", bytes);
        let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
        assert_eq!(opened.text(), "# title\nbody\n");
        assert_eq!(
            opened.encoding(),
            DiskEncoding {
                bom: true,
                eol: crate::io::LineEnding::Lf
            }
        );
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), bytes, "a save without edits");

        opened.set_text("# title\nbody\nmore\n");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(
            fs::read(&path).unwrap(),
            b"\xef\xbb\xbf# title\nbody\nmore\n"
        );
        assert!(
            opened
                .persistence
                .observed_text_matches(opened.document, &opened.text())
        );
        let refreshed = opened
            .grants
            .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        assert_eq!(refreshed.text, opened.text());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A file whose every line ends in CRLF is held with LF and saved with
    /// CRLF, a line typed into it included; a byte order mark beside it stays.
    #[test]
    fn crlf_file_round_trips_with_lf_in_buffer() {
        for (name, mark) in [("crlf", &b""[..]), ("crlf-bom", &crate::io::UTF8_BOM[..])] {
            let bytes = [mark, &b"one\r\ntwo\r\n"[..]].concat();
            let path = unique_file(name, &bytes);
            let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
            assert_eq!(opened.text(), "one\ntwo\n");
            assert_eq!(opened.encoding().eol, crate::io::LineEnding::CrLf);
            assert_eq!(opened.encoding().bom, !mark.is_empty());
            assert!(matches!(opened.save(), SaveReduction::Durable(_)));
            assert_eq!(
                fs::read(&path).unwrap(),
                bytes,
                "{name}: a save without edits"
            );

            opened.set_text("one\ntwo\nthree\n");
            assert!(matches!(opened.save(), SaveReduction::Durable(_)));
            assert_eq!(
                fs::read(&path).unwrap(),
                [mark, &b"one\r\ntwo\r\nthree\r\n"[..]].concat(),
                "{name}"
            );
            let refreshed = opened
                .grants
                .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
                .unwrap();
            assert_eq!(refreshed.text, "one\ntwo\nthree\n", "{name}");
            assert!(
                opened
                    .persistence
                    .observed_text_matches(opened.document, &refreshed.text),
                "{name}: the watch finds the buffer's own save unchanged"
            );
            let _ = fs::remove_dir_all(path.parent().unwrap());
        }
    }

    /// Accepting an overwrite rebinds the baseline while preserving the buffer's line-ending mapping.
    #[test]
    fn an_overwrite_keeps_the_buffers_mapping() {
        for (name, read, theirs, ours, written) in [
            (
                "overwrite-crlf",
                &b"one\r\ntwo\r\n"[..],
                &b"theirs\n"[..],
                "ours\n",
                &b"ours\r\n"[..],
            ),
            (
                "overwrite-mixed",
                b"a\r\nb\n",
                b"x\r\ny\r\n",
                "a\r\nb\nc\n",
                b"a\r\nb\nc\n",
            ),
        ] {
            let path = unique_file(name, read);
            let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
            opened.set_text(ours);
            fs::write(&path, theirs).unwrap();
            assert!(
                matches!(opened.save(), SaveReduction::Conflict(_)),
                "{name}"
            );
            let snapshot = opened.documents.snapshot(opened.document).unwrap();
            assert!(matches!(
                opened.persistence.begin(&snapshot),
                Err(PersistenceError::Save(SaveError::ConflictRequiresReconcile))
            ));
            assert_eq!(fs::read(&path).unwrap(), theirs, "{name}: nothing written");

            let fresh = opened
                .grants
                .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
                .unwrap();
            opened
                .persistence
                .accept_overwrite(opened.document, fresh.observed)
                .unwrap();
            assert!(matches!(opened.save(), SaveReduction::Durable(_)), "{name}");
            assert_eq!(fs::read(&path).unwrap(), written, "{name}");
            let _ = fs::remove_dir_all(path.parent().unwrap());
        }
    }

    /// One line feed without a CR before it keeps the whole file as it is: its
    /// CRs stay in the text, and saves write the text's own bytes.
    #[test]
    fn mixed_eol_is_byte_exact() {
        for (name, bytes) in [
            ("mixed", &b"a\r\nb\nc\r\n"[..]),
            ("leading-lf", b"\na\r\nb\r\n"),
            ("bare-cr", b"a\rb\r"),
            ("no-break", b"a\r"),
        ] {
            let path = unique_file(name, bytes);
            let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
            assert_eq!(opened.text().as_bytes(), bytes, "{name}");
            assert_eq!(opened.encoding(), DiskEncoding::PLAIN, "{name}");
            assert!(matches!(opened.save(), SaveReduction::Durable(_)));
            assert_eq!(fs::read(&path).unwrap(), bytes, "{name}");

            let edited = format!("{}x\n", opened.text());
            opened.set_text(&edited);
            assert!(matches!(opened.save(), SaveReduction::Durable(_)));
            assert_eq!(fs::read(&path).unwrap(), edited.as_bytes(), "{name}");
            let _ = fs::remove_dir_all(path.parent().unwrap());
        }
    }

    /// The mapping is the buffer's, not a fresh reading of each save's bytes.
    #[test]
    fn a_document_keeps_the_mapping_it_was_read_with_across_saves() {
        let path = unique_file("mixed-becomes-crlf", b"a\r\nb\n");
        let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
        opened.set_text("a\r\n");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), b"a\r\n");
        assert_eq!(opened.encoding(), DiskEncoding::PLAIN);
        opened.set_text("a\r\nc\r\n");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), b"a\r\nc\r\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());

        let path = unique_file("crlf-without-break", b"a\r\nb\r\n");
        let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
        opened.set_text("ab");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), b"ab");
        let refreshed = opened
            .grants
            .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        assert_eq!(refreshed.observed.encoding(), DiskEncoding::PLAIN);
        opened
            .persistence
            .accept_observation(opened.document, refreshed.observed, &refreshed.text)
            .unwrap();
        assert_eq!(opened.encoding().eol, crate::io::LineEnding::CrLf);
        opened.set_text("a\nb\n");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), b"a\r\nb\r\n");

        fs::write(&path, b"x\ny\n").unwrap();
        let refreshed = opened
            .grants
            .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        opened.set_text(&refreshed.text);
        opened
            .persistence
            .accept_observation(opened.document, refreshed.observed, &refreshed.text)
            .unwrap();
        assert_eq!(opened.encoding(), DiskEncoding::PLAIN);
        opened.set_text("x\ny\nz\n");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), b"x\ny\nz\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A buffer held as it is can save bytes that a fresh read takes another
    /// way: every break CRLF reads back with LF, and a leading U+FEFF reads
    /// back as a mark.
    #[test]
    fn a_save_read_back_another_way_is_unchanged() {
        for (name, start, held, reading, encoding) in [
            (
                "new-crlf",
                None,
                "x\r\ny\r\n",
                "x\ny\n",
                DiskEncoding {
                    bom: false,
                    eol: crate::io::LineEnding::CrLf,
                },
            ),
            (
                "mixed-to-crlf",
                Some(&b"a\r\nb\n"[..]),
                "a\r\n",
                "a\n",
                DiskEncoding {
                    bom: false,
                    eol: crate::io::LineEnding::CrLf,
                },
            ),
            (
                "leading-feff",
                None,
                "\u{feff}hi\n",
                "hi\n",
                DiskEncoding {
                    bom: true,
                    eol: crate::io::LineEnding::Lf,
                },
            ),
        ] {
            let path = unique_file(name, start.unwrap_or(b""));
            let options = if start.is_none() {
                fs::remove_file(&path).unwrap();
                ALLOW_MISSING
            } else {
                AdmitOptions::DOCUMENT
            };
            let mut opened = open_document(&path, options);
            opened.set_text(held);
            assert!(matches!(opened.save(), SaveReduction::Durable(_)), "{name}");
            assert_eq!(fs::read(&path).unwrap(), held.as_bytes(), "{name}");
            assert_eq!(opened.encoding(), DiskEncoding::PLAIN, "{name}");

            let refreshed = opened
                .grants
                .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
                .unwrap();
            assert_eq!(refreshed.text, reading, "{name}: the fresh read");
            assert_eq!(refreshed.observed.encoding(), encoding, "{name}");
            let baseline = opened.persistence.observed(opened.document).unwrap();
            assert_eq!(
                crate::io::detect_version_conflict(baseline, refreshed.observed),
                None,
                "{name}"
            );
            let document = opened.document;
            for text in [held, reading] {
                assert!(
                    opened.persistence.observed_text_matches(document, text),
                    "{name}: {text:?}"
                );
                assert!(
                    opened
                        .persistence
                        .last_observed_text_matches(document, text),
                    "{name}: {text:?}"
                );
            }
            assert!(
                !opened
                    .persistence
                    .observed_text_matches(document, &format!("{reading}z")),
                "{name}: another text is still a change"
            );
            assert_eq!(
                opened.persistence.observed_text_fingerprint(document),
                Some(ContentFingerprint::of(reading.as_bytes())),
                "{name}: a reopen admits the fresh reading"
            );

            opened
                .persistence
                .accept_observation(document, refreshed.observed, &refreshed.text)
                .unwrap();
            assert_eq!(opened.encoding(), DiskEncoding::PLAIN, "{name}");
            assert!(
                opened.persistence.observed_text_matches(document, held),
                "{name}"
            );
            assert!(matches!(opened.save(), SaveReduction::Durable(_)), "{name}");
            assert_eq!(
                fs::read(&path).unwrap(),
                held.as_bytes(),
                "{name}: the second save"
            );

            fs::remove_file(&path).unwrap();
            fs::write(&path, held.as_bytes()).unwrap();
            let rewritten = opened
                .grants
                .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
                .unwrap();
            let baseline = opened.persistence.observed(document).unwrap();
            assert!(
                crate::io::content_is_equivalent(baseline, rewritten.observed),
                "{name}"
            );
            assert!(
                opened
                    .persistence
                    .last_observed_text_matches(document, &rewritten.text),
                "{name}"
            );
            opened
                .persistence
                .accept_observation(document, rewritten.observed, &rewritten.text)
                .unwrap();
            assert_eq!(opened.encoding(), DiskEncoding::PLAIN, "{name}");
            opened.set_text(&format!("{held}z"));
            assert!(matches!(opened.save(), SaveReduction::Durable(_)), "{name}");
            assert_eq!(
                fs::read(&path).unwrap(),
                format!("{held}z").as_bytes(),
                "{name}: an edit after the rebind"
            );
            let _ = fs::remove_dir_all(path.parent().unwrap());
        }
    }

    /// Reloading adopts the fresh reading's encoding unless it is equivalent to the buffer's existing reading.
    #[test]
    fn a_reload_takes_the_fresh_reading_and_its_mapping() {
        let path = unique_file("reload-crlf", b"");
        fs::remove_file(&path).unwrap();
        let mut opened = open_document(&path, ALLOW_MISSING);
        opened.set_text("x\r\ny\r\n");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        opened.set_text("x\r\ny\r\nlocal edit");
        let refreshed = opened
            .grants
            .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        opened.set_text(&refreshed.text);
        opened
            .persistence
            .accept_reloaded_observation(opened.document, refreshed.observed, &refreshed.text)
            .unwrap();
        assert_eq!(opened.encoding().eol, crate::io::LineEnding::CrLf);
        assert!(
            opened
                .persistence
                .observed_text_matches(opened.document, "x\ny\n")
        );
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), b"x\r\ny\r\n");
        opened.set_text("x\ny\nz\n");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), b"x\r\ny\r\nz\r\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());

        let path = unique_file("reload-crlf-no-break", b"a\r\nb\r\n");
        let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
        opened.set_text("ab");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        let refreshed = opened
            .grants
            .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        opened
            .persistence
            .accept_reloaded_observation(opened.document, refreshed.observed, &refreshed.text)
            .unwrap();
        assert_eq!(opened.encoding().eol, crate::io::LineEnding::CrLf);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// The draft-journal fingerprint is the admitted text's, which is not the
    /// file's own when admission took a mark or CRs off.
    #[test]
    fn the_observed_text_fingerprint_is_the_admitted_texts() {
        let bytes = b"\xef\xbb\xbfone\r\ntwo\r\n";
        let path = unique_file("text-fingerprint", bytes);
        let opened = open_document(&path, AdmitOptions::DOCUMENT);
        let observed = opened.persistence.observed(opened.document).unwrap();
        assert_eq!(observed.content, ContentFingerprint::of(bytes));
        assert_eq!(
            opened
                .persistence
                .observed_text_fingerprint(opened.document),
            Some(ContentFingerprint::of(b"one\ntwo\n"))
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());

        let path = unique_file("text-fingerprint-missing", b"");
        fs::remove_file(&path).unwrap();
        let opened = open_document(&path, ALLOW_MISSING);
        assert_eq!(
            opened
                .persistence
                .observed_text_fingerprint(opened.document),
            None,
            "no file, no image"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Document saves never create missing parent directories.
    #[test]
    fn a_document_save_makes_no_folder() {
        let root = unique_file("no-folder", b"");
        fs::remove_file(&root).unwrap();
        let dir = root.parent().unwrap().join("sub");
        fs::create_dir(&dir).unwrap();

        let missing = dir.join("new.md");
        let mut opened = open_document(&missing, ALLOW_MISSING);
        opened.set_text("text\n");
        fs::remove_dir(&dir).unwrap();
        let reduction = opened.save();
        assert!(
            matches!(
                &reduction,
                SaveReduction::Failed {
                    stage: AtomicSaveStage::CreateTemporary,
                    message,
                    cause: Some(SaveCause {
                        kind: std::io::ErrorKind::NotFound,
                        touch: SaveTouch::Folder,
                    }),
                } if message.ends_with(MISSING_SUFFIX)
            ),
            "{reduction:?}"
        );
        assert!(!dir.exists(), "the first save made no folder");

        fs::create_dir(&dir).unwrap();
        let saved = dir.join("saved.md");
        fs::write(&saved, b"saved\n").unwrap();
        let mut opened = open_document(&saved, AdmitOptions::DOCUMENT);
        opened.set_text("edited\n");
        fs::remove_file(&saved).unwrap();
        fs::remove_dir(&dir).unwrap();
        assert_eq!(
            opened.save(),
            SaveReduction::Conflict(crate::io::VersionConflict::Existence)
        );
        assert!(!dir.exists(), "a save made no folder");
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    /// A never-saved document's file is still absent on a refresh: no change,
    /// through the missing-baseline refresh.
    #[test]
    fn a_missing_document_refreshes_as_missing_until_its_file_appears() {
        let path = unique_file("missing-refresh", b"");
        fs::remove_file(&path).unwrap();
        let opened = open_document(&path, ALLOW_MISSING);
        let id = opened.granted.grant.id;
        assert_eq!(
            opened
                .grants
                .refresh_local(id, DEFAULT_DOCUMENT_LIMIT)
                .unwrap_err()
                .refusal_word(),
            "missing"
        );
        let absent = opened
            .grants
            .refresh_local_allowing_missing(id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        assert!(!absent.observed.exists);
        assert_eq!(absent.text, "");
        assert_eq!(
            crate::io::detect_version_conflict(opened.granted.observed, absent.observed),
            None
        );

        fs::write(&path, b"from elsewhere\r\n").unwrap();
        let appeared = opened
            .grants
            .refresh_local_allowing_missing(id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        assert!(appeared.observed.exists);
        assert_eq!(appeared.text, "from elsewhere\n");
        assert_eq!(
            crate::io::detect_version_conflict(opened.granted.observed, appeared.observed),
            Some(crate::io::VersionConflict::Existence)
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// `open_path` resolves symlinked parent directories for both existing and missing files.
    #[cfg(unix)]
    #[test]
    fn open_path_resolves_a_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let path = unique_file("open-path", b"");
        let dir = path.parent().unwrap();
        let real = dir.join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("present.md"), b"here\n").unwrap();
        let link = dir.join("link");
        symlink(&real, &link).unwrap();
        let canonical = fs::canonicalize(&real).unwrap();

        let (present, uri) = open_path(&link.join("present.md")).unwrap();
        assert_eq!(present, canonical.join("present.md"));
        assert_eq!(uri, path_to_file_uri(&present).unwrap());
        assert_eq!(
            admit_local_file_opts(&uri, AdmitOptions::DOCUMENT)
                .unwrap()
                .text,
            "here\n"
        );

        let (missing, uri) = open_path(&link.join("new file.md")).unwrap();
        assert_eq!(missing, canonical.join("new file.md"));
        assert!(uri.ends_with("/real/new%20file.md"), "{uri}");
        assert!(
            admit_local_file_opts(&uri, ALLOW_MISSING)
                .unwrap()
                .is_missing()
        );
        assert!(matches!(
            admit_local_file_opts(&file_uri(&link.join("new file.md")), ALLOW_MISSING),
            Err(DocumentHostError::SymlinkComponent { .. })
        ));

        symlink(dir.join("nowhere"), dir.join("dangling")).unwrap();
        assert!(matches!(
            open_path(&dir.join("dangling")),
            Err(DocumentHostError::SymlinkComponent { .. })
        ));
        assert_eq!(
            open_path(&dir.join("no such folder").join("x.md"))
                .unwrap_err()
                .refusal_word(),
            "missing"
        );

        #[cfg(target_os = "macos")]
        {
            let name = format!("aterm-open-path-{}-absent.md", std::process::id());
            let (resolved, _) = open_path(&Path::new("/tmp").join(&name)).unwrap();
            assert_eq!(resolved, Path::new("/private/tmp").join(&name));
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// `EDEADLK` from `read(2)` on an evicted dataless file maps to `DocumentHostError::NotDownloaded`.
    #[cfg(unix)]
    #[test]
    fn edeadlk_on_read_is_reported_as_not_downloaded() {
        let path = Path::new("/Users//someone/Library/Mobile Documents/notes.md");
        let evicted = read_error(path, std::io::Error::from_raw_os_error(libc::EDEADLK));
        assert_eq!(
            evicted,
            DocumentHostError::NotDownloaded {
                path: path.to_path_buf()
            }
        );

        let other = read_error(path, std::io::Error::from_raw_os_error(libc::EIO));
        assert!(
            matches!(
                &other,
                DocumentHostError::Io {
                    stage: AtomicSaveStage::Preflight,
                    ..
                }
            ),
            "{other:?}"
        );
    }

    /// Admission then grant composes to the synchronous open: admission is a pure function
    /// of the URI, granting is the store's only mutation, and admitting twice mints one grant.
    #[test]
    fn admission_then_grant_matches_the_synchronous_open() {
        let path = unique_file("two-phase", b"# split\n");
        let uri = file_uri(&path);
        let admitted = admit_local_file(&uri, DEFAULT_DOCUMENT_LIMIT, false).unwrap();
        assert_eq!(admitted.text, "# split\n");
        assert!(admitted.canonical_uri.starts_with("file:///"));
        assert!(admitted.path.is_absolute());

        let mut grants = DocumentGrantStore::new();
        let first = grants
            .grant_admitted(admitted.clone(), GrantAccess::ReadOnly)
            .unwrap();
        let second = grants
            .grant_admitted(admitted, GrantAccess::ReadWrite)
            .unwrap();
        assert_eq!(first.grant.id, second.grant.id);
        assert_eq!(second.grant.access, GrantAccess::ReadWrite);
        assert_eq!(first.text, "# split\n");
        assert_eq!(first.observed, second.observed);

        let direct = grants
            .open_local(&uri, GrantAccess::ReadOnly, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        assert_eq!(direct.grant.id, first.grant.id);
        assert_eq!(direct.grant.canonical_uri, first.grant.canonical_uri);
        assert!(
            matches!(
                admit_local_file(MISSING_DOC_URI, 16, false),
                Err(DocumentHostError::Io { .. })
            ),
            "a missing file fails at admission, before any grant state exists"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn malformed_remote_binary_and_oversize_inputs_fail_closed() {
        let mut grants = DocumentGrantStore::new();
        assert!(matches!(
            grants.open_local("https://example.com/a", GrantAccess::ReadOnly, 10),
            Err(DocumentHostError::UnsupportedScheme)
        ));
        assert!(matches!(
            grants.open_local("file://example.com/a", GrantAccess::ReadOnly, 10),
            Err(DocumentHostError::RemoteAuthority)
        ));
        assert!(matches!(
            grants.open_local("file:///tmp/%zz", GrantAccess::ReadOnly, 10),
            Err(DocumentHostError::InvalidEncoding)
        ));
        let path = unique_file("invalid", &[0xff]);
        assert!(matches!(
            grants.open_local(&file_uri(&path), GrantAccess::ReadOnly, 10),
            Err(DocumentHostError::InvalidUtf8)
        ));
        fs::write(&path, b"too long").unwrap();
        assert!(matches!(
            grants.open_local(&file_uri(&path), GrantAccess::ReadOnly, 2),
            Err(DocumentHostError::TooLarge { limit: 2 })
        ));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// `decode_utf8` strips at most one leading UTF-8 BOM and validates the remaining bytes.
    #[test]
    fn decoding_takes_off_one_leading_bom_and_still_checks_the_rest() {
        let decode = |bytes: &[u8]| decode_utf8(bytes.to_vec()).map(|decoded| decoded.text);
        assert_eq!(decode(b"\xef\xbb\xbfhi").unwrap(), "hi");
        assert_eq!(decode(b"\xef\xbb\xbf").unwrap(), "");
        assert_eq!(
            decode(b"\xef\xbb\xbf\xef\xbb\xbf").unwrap(),
            "\u{feff}",
            "only the first BOM is taken off"
        );
        assert_eq!(decode(b"hi\xef\xbb\xbf").unwrap(), "hi\u{feff}");
        assert_eq!(decode(b"").unwrap(), "");
        assert!(matches!(
            decode(b"\xef\xbb"),
            Err(DocumentHostError::InvalidUtf8)
        ));
        assert!(matches!(
            decode(b"\xef\xbb\xbf\xff"),
            Err(DocumentHostError::InvalidUtf8)
        ));
    }

    /// A budget the clock cannot add to now is no ceiling: the wait neither
    /// panics working out its deadline nor gives up while the lock is held.
    #[test]
    fn a_lock_budget_past_the_clock_waits_without_a_ceiling() {
        let path = unique_file("unbounded-lock-budget", b"");
        let open = || {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap()
        };
        let Ok(held) = acquire_advisory_lock_within(open(), std::time::Duration::MAX) else {
            panic!("an uncontended lock is taken at once");
        };

        let (sent, outcome) = std::sync::mpsc::channel();
        let contender = open();
        let waiter = std::thread::spawn(move || {
            let taken = acquire_advisory_lock_within(contender, std::time::Duration::MAX);
            let _ = sent.send(taken.is_ok());
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(held);
        assert_eq!(
            outcome.recv_timeout(std::time::Duration::from_secs(60)),
            Ok(true),
            "the unbounded wait takes the lock once it is let go"
        );
        waiter.join().unwrap();
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn save_lock_refuses_fifo_and_symlink_without_touching_the_target() {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::symlink;

        let path = unique_file("special-save-lock", b"before");
        let contents = read_atomic_file(&path, DEFAULT_DOCUMENT_LIMIT, false).unwrap();
        let lock_path = path.parent().unwrap().join(format!(
            ".{}.aterm-write.lock",
            path.file_name().unwrap().to_string_lossy()
        ));
        let lock_c =
            std::ffi::CString::new(lock_path.as_os_str().as_bytes()).expect("save-lock FIFO path");
        // SAFETY: `lock_c` is a live NUL-terminated pathname and `mkfifo` retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(lock_c.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(matches!(
            commit_atomic_bytes(&contents.baseline, b"after"),
            AtomicCommitResult::Failed {
                stage: AtomicSaveStage::Preflight,
                message,
                cause: None,
            } if message.contains("regular non-link")
        ));
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the FIFO save lock parked the commit"
        );
        assert_eq!(fs::read(&path).unwrap(), b"before");

        fs::remove_file(&lock_path).unwrap();
        let victim = path.parent().unwrap().join("lock-victim");
        fs::write(&victim, b"victim").unwrap();
        symlink(&victim, &lock_path).unwrap();
        assert!(matches!(
            commit_atomic_bytes(&contents.baseline, b"after"),
            AtomicCommitResult::Failed {
                stage: AtomicSaveStage::Preflight,
                ..
            }
        ));
        assert_eq!(fs::read(&path).unwrap(), b"before");
        assert_eq!(fs::read(&victim).unwrap(), b"victim");
        assert!(
            fs::symlink_metadata(&lock_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A lock holder that releases within the wait budget is ridden out without failing the save.
    #[test]
    fn a_transient_save_lock_holder_is_ridden_out_instead_of_refusing_the_save() {
        let path = unique_file("transient-save-lock", b"before");
        let lock_path = path.parent().unwrap().join(format!(
            ".{}.aterm-write.lock",
            path.file_name().unwrap().to_string_lossy()
        ));
        let held = HeldAdvisoryLock::lock(open_write_lock(&lock_path).unwrap()).unwrap();
        let probe = open_write_lock(&lock_path).unwrap();

        let (entered, saw_entry) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            entered.send(()).unwrap();
            acquire_write_lock_within(probe, aterm_spec::wait::MUST_HAPPEN)
        });
        saw_entry.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(held);

        assert!(waiter.join().unwrap().is_ok());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn held_save_lock_returns_busy_and_retry_commits() {
        let path = unique_file("held-save-lock", b"before");
        let contents = read_atomic_file(&path, DEFAULT_DOCUMENT_LIMIT, false).unwrap();
        let lock_path = path.parent().unwrap().join(format!(
            ".{}.aterm-write.lock",
            path.file_name().unwrap().to_string_lossy()
        ));
        let held = HeldAdvisoryLock::lock(open_write_lock(&lock_path).unwrap()).unwrap();

        let started = std::time::Instant::now();
        assert!(matches!(
            commit_atomic_bytes(&contents.baseline, b"after"),
            AtomicCommitResult::Failed {
                stage: AtomicSaveStage::Preflight,
                message,
                cause: Some(SaveCause {
                    kind: std::io::ErrorKind::WouldBlock,
                    touch: SaveTouch::Lock,
                }),
            } if message.contains("busy") && message.contains("retry Save")
        ));
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the held save lock parked the commit"
        );
        assert_eq!(fs::read(&path).unwrap(), b"before");

        drop(held);
        assert!(matches!(
            commit_atomic_bytes(&contents.baseline, b"after"),
            AtomicCommitResult::Committed(_)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"after");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// The save lock is free the instant its hold drops, even while a copy of the holder's
    /// description lives (standing in for a shell forked mid-save).
    #[test]
    fn a_released_save_lock_is_free_while_a_forked_copy_of_it_lives() {
        let path = unique_file("save-lock-forked-copy", b"before");
        let contents = read_atomic_file(&path, DEFAULT_DOCUMENT_LIMIT, false).unwrap();
        let lock_path = path.parent().unwrap().join(format!(
            ".{}.aterm-write.lock",
            path.file_name().unwrap().to_string_lossy()
        ));

        let Ok((held, _)) = lock_and_verify_target(&contents.baseline, Directories::Create) else {
            panic!("an uncontended preflight takes the save lock");
        };
        let forked_copy = held.0.try_clone().unwrap();
        drop(held);

        let next = HeldAdvisoryLock::try_lock(open_write_lock(&lock_path).unwrap());
        assert!(
            next.is_ok(),
            "the save lock outlived its hold on the forked copy of its description"
        );
        drop(next);
        drop(forked_copy);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Documents under `$TMPDIR` resolve cleanly across macOS's `/var` compatibility symlink.
    #[test]
    fn temporary_root_document_resolves_despite_the_var_compatibility_symlink() {
        let path = unique_file("temp-root-normalize", b"body");
        let target = resolve_atomic_target(&path, false)
            .expect("a document under the process temporary root must be admissible");
        let canonical_root = fs::canonicalize(std::env::temp_dir()).unwrap();
        assert!(
            target.target_path().starts_with(&canonical_root),
            "target {:?} must sit under the canonical temp root {:?}",
            target.target_path(),
            canonical_root
        );
        assert_eq!(target.logical_path(), path.as_path());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A path outside the temporary root claims no symlink exemption, so the rewrite
    /// leaves it untouched.
    #[test]
    fn paths_outside_the_temporary_root_are_not_rewritten() {
        let outside = PathBuf::from("/usr/share/aterm-does-not-exist/doc.md");
        assert_eq!(
            normalize_process_temp_root(&outside).unwrap(),
            outside,
            "a non-temporary path must be returned verbatim"
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_final_target_symlink_swap_is_refused_by_the_opened_handle_seam() {
        use std::os::unix::fs::symlink;

        let path = unique_file("canonical-final-link", b"original");
        let displaced = path.with_extension("displaced");
        let target = resolve_atomic_target(&path, false).unwrap();
        fs::rename(&path, &displaced).unwrap();
        symlink(&displaced, &path).unwrap();

        assert!(matches!(
            read_stable_file(target.target_path(), 1024, false),
            Err(DocumentHostError::SymlinkComponent { path: rejected, .. })
                if rejected == target.target_path()
        ));
        assert_eq!(fs::read(&displaced).unwrap(), b"original");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_preflight_bounds_external_growth_and_rejects_oversized_desired_bytes() {
        let path = unique_file("bounded-save-preflight", b"old");
        let contents = read_atomic_file(&path, 1024, false).unwrap();
        fs::write(&path, b"theirs").unwrap();
        let grown_len = (MAX_ATOMIC_SAVE_BYTES as u64).saturating_mul(4);
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(grown_len)
            .unwrap();

        let sampled = observe_file(&path, contents.baseline.observed.len as usize).unwrap();
        assert_eq!(sampled.len, grown_len);
        assert_eq!(sampled.content, ContentFingerprint::of(b"thei"));
        assert!(matches!(
            crate::io::reduce_file_watch(crate::io::FileWatchInput {
                baseline: contents.baseline.observed,
                observed: sampled,
                document_dirty: true,
                save_in_flight: false,
            }),
            crate::io::FileWatchReduction::ConflictDirty {
                change: crate::io::VersionConflict::Content,
            }
        ));
        assert!(matches!(
            commit_atomic_bytes(&contents.baseline, b"ours"),
            AtomicCommitResult::Conflict { observed, .. } if observed.len == grown_len
        ));

        fs::write(&path, b"old").unwrap();
        let fresh = read_atomic_file(&path, 1024, false).unwrap();
        let oversized = vec![b'x'; MAX_ATOMIC_SAVE_BYTES + 1];
        assert!(matches!(
            commit_atomic_bytes(&fresh.baseline, &oversized),
            AtomicCommitResult::Failed {
                stage: AtomicSaveStage::Preflight,
                message,
                cause: Some(SaveCause {
                    kind: std::io::ErrorKind::FileTooLarge,
                    ..
                }),
            } if message.contains("document limit")
        ));
        assert_eq!(fs::read(&path).unwrap(), b"old");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn windows_drive_file_uri_text_round_trips_without_a_posix_root() {
        assert_eq!(
            windows_file_uri_path_text("/C:/Users//Ada/My File.toml").unwrap(),
            "C:/Users//Ada/My File.toml"
        );
        assert_eq!(windows_file_uri_path_text("/z:/").unwrap(), "z:/");
        assert!(matches!(
            windows_file_uri_path_text("//server/share/aterm.toml"),
            Err(DocumentHostError::RemoteAuthority)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_path_and_file_uri_round_trip_through_real_path_rules() {
        let path = Path::new(r"C:\Users\Ada\My File.toml");
        let uri = path_to_file_uri(path).unwrap();
        assert_eq!(uri, "file:///C:/Users//Ada/My%20File.toml");
        assert_eq!(file_uri_path(&uri).unwrap(), path);
    }

    #[test]
    fn validation_io_is_not_mislabeled_as_a_disk_conflict() {
        let path = unique_file("validation-class", b"old");
        let target = read_atomic_file(&path, 1024, false)
            .unwrap()
            .baseline
            .target;
        assert!(matches!(
            atomic_validation_failure(
                &target,
                AtomicSaveStage::Preflight,
                DocumentHostError::Io {
                    stage: AtomicSaveStage::Preflight,
                    kind: Some(std::io::ErrorKind::PermissionDenied),
                    message: "permission denied".to_string(),
                },
            ),
            AtomicCommitResult::Failed {
                stage: AtomicSaveStage::Preflight,
                message,
                cause: Some(SaveCause {
                    kind: std::io::ErrorKind::PermissionDenied,
                    touch: SaveTouch::Read,
                }),
            } if message.contains("permission denied")
        ));
        assert!(matches!(
            atomic_validation_failure(
                &target,
                AtomicSaveStage::Preflight,
                DocumentHostError::TargetRetargeted,
            ),
            AtomicCommitResult::Conflict { .. }
        ));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn atomic_save_proof_is_accepted_by_reducer() {
        let path = unique_file("save", b"old");
        let mut grants = DocumentGrantStore::new();
        let opened = grants
            .open_local(&file_uri(&path), GrantAccess::ReadWrite, 1024)
            .unwrap();
        let mut documents = crate::test_document_store();
        let document = documents.open(opened.grant.canonical_uri.clone(), "new".to_string());
        let snapshot = documents.snapshot(document).unwrap();
        let mut reducer = SaveReducer::new(document, opened.observed);
        let plan = reducer.begin(&snapshot).unwrap();
        let result = grants.execute_save(opened.grant.id, &plan);
        let reduction = reducer.complete(plan.generation, result);
        assert!(
            matches!(reduction, SaveReduction::Durable(checkpoint) if checkpoint.seq == snapshot.seq)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert!(fs::read_dir(path.parent().unwrap()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp")
        }));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn atomic_commit_creates_a_missing_target() {
        let path = unique_file("missing-target", b"remove me");
        fs::remove_file(&path).unwrap();
        let contents = read_atomic_file(&path, 1024, true).unwrap();
        assert!(contents.bytes.is_empty());
        assert!(!contents.baseline.observed.exists);

        let proof = match commit_atomic_bytes(&contents.baseline, b"theme = \"Nord\"\n") {
            AtomicCommitResult::Committed(proof) => proof,
            other => panic!("missing-target commit failed: {other:?}"),
        };
        assert!(proof.observed.exists);
        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \"Nord\"\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A save the person makes is durable, file and entry; the empty config file a
    /// missing one is created as is only ordered (aterm-durable's counting seam).
    #[test]
    fn a_save_is_durable_and_a_created_config_file_only_ordered() {
        use aterm_durable::seam::Recording;
        let path = unique_file("flush-tiers", b"old");
        let contents = read_atomic_file(&path, 1024, true).unwrap();
        let recording = Recording::start();
        assert!(matches!(
            commit_atomic_bytes(&contents.baseline, b"saved"),
            AtomicCommitResult::Committed(_)
        ));
        let saved = recording.take();
        #[cfg(unix)]
        let expected_save = [Flush::Durable, Flush::Durable];
        #[cfg(not(unix))]
        let expected_save = [Flush::Durable];
        assert_eq!(saved, expected_save, "a save");
        fs::remove_file(&path).unwrap();
        let missing = read_atomic_file(&path, 1024, true).unwrap();
        let _ = recording.take();
        assert!(matches!(
            commit_atomic_bytes_ordered(&missing.baseline, b""),
            AtomicCommitResult::Committed(_)
        ));
        #[cfg(unix)]
        let expected_create = [Flush::Ordered, Flush::Ordered];
        #[cfg(not(unix))]
        let expected_create = [Flush::Ordered];
        assert_eq!(recording.take(), expected_create, "a created config file");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn external_change_returns_conflict_without_overwrite() {
        let path = unique_file("conflict", b"old");
        let mut grants = DocumentGrantStore::new();
        let opened = grants
            .open_local(&file_uri(&path), GrantAccess::ReadWrite, 1024)
            .unwrap();
        let mut documents = crate::test_document_store();
        let document = documents.open(opened.grant.canonical_uri.clone(), "ours".to_string());
        let mut reducer = SaveReducer::new(document, opened.observed);
        let plan = reducer
            .begin(&documents.snapshot(document).unwrap())
            .unwrap();
        fs::write(&path, "theirs").unwrap();
        assert!(matches!(
            grants.execute_save(opened.grant.id, &plan),
            AtomicSaveResult::Conflict { .. }
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn regular_target_atomic_replace_with_identical_bytes_keeps_grant_valid() {
        let path = unique_file("equivalent-generation", b"same bytes\n");
        let replacement = path.with_extension("replacement");
        let mut grants = DocumentGrantStore::new();
        let opened = grants
            .open_local(&file_uri(&path), GrantAccess::ReadWrite, 1024)
            .unwrap();
        fs::write(&replacement, b"same bytes\n").unwrap();
        fs::rename(&replacement, &path).unwrap();

        let refreshed = grants.refresh_local(opened.grant.id, 1024).unwrap();
        assert_eq!(refreshed.text, opened.text);
        assert_eq!(
            crate::io::detect_version_conflict(opened.observed, refreshed.observed),
            Some(crate::io::VersionConflict::Identity),
            "atomic replacement advances the regular-file identity even when bytes match"
        );
        refreshed.grant.validate_current_binding().unwrap();
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persistence_completion_is_document_owned_not_view_owned() {
        let path = unique_file("document-owner", b"old");
        let mut grants = DocumentGrantStore::new();
        let opened = grants
            .open_local(&file_uri(&path), GrantAccess::ReadWrite, 1024)
            .unwrap();
        let mut documents = crate::test_document_store();
        let document = documents.open(opened.grant.canonical_uri.clone(), "saved".to_string());
        let snapshot = documents.snapshot(document).unwrap();
        let mut persistence = DocumentPersistenceStore::default();
        persistence
            .register(document, opened.grant.id, opened.observed, &opened.text)
            .unwrap();
        let pending = persistence.begin(&snapshot).unwrap();

        let result = grants.execute_save(pending.grant, &pending.plan);
        let reduction = persistence
            .complete(document, pending.plan.generation, result)
            .unwrap();
        assert!(matches!(
            reduction,
            SaveReduction::Durable(checkpoint) if checkpoint.document == document
        ));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn temporary_name_collision_retries_without_truncating_the_collision() {
        let path = unique_file("temp-collision", b"old");
        let contents = read_atomic_file(&path, 1024, false).unwrap();
        let seed = 0x5a17;
        let collision = path.parent().unwrap().join(format!(
            ".{}.aterm-{}-{seed}-0.tmp",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        fs::write(&collision, "do not touch").unwrap();

        assert!(matches!(
            commit_atomic_bytes_with_seed(
                &contents.baseline,
                b"new",
                seed,
                Flush::Durable,
                Directories::Create
            ),
            AtomicCommitResult::Committed(_)
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(fs::read_to_string(&collision).unwrap(), "do not touch");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn config_symlink_chain_is_bound_and_preserved_while_ordinary_grants_fail_closed() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "aterm-native-document-retarget-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.toml");
        let real_parent = dir.join("real");
        fs::create_dir_all(&real_parent).unwrap();
        let second = real_parent.join("second.toml");
        let logical = dir.join("aterm.toml");
        let parent_alias = dir.join("parent-alias");
        let chained_final = real_parent.join("chained.toml");
        let chained_logical = parent_alias.join("chained.toml");
        fs::write(&first, "theme = \"Default\"\n").unwrap();
        fs::write(&second, "theme = \"Nord\"\n").unwrap();
        symlink("first.toml", &logical).unwrap();
        symlink(&real_parent, &parent_alias).unwrap();
        symlink("../first.toml", &chained_final).unwrap();

        let mut strict_grants = DocumentGrantStore::new();
        assert!(matches!(
            strict_grants.open_local(&file_uri(&logical), GrantAccess::ReadWrite, 4096),
            Err(DocumentHostError::SymlinkComponent { .. })
        ));
        let mut grants = DocumentGrantStore::new();
        let opened = grants
            .open_local_config(&file_uri(&logical), GrantAccess::ReadWrite, 4096)
            .unwrap();
        assert_eq!(opened.text, "theme = \"Default\"\n");
        assert_eq!(opened.grant.logical_path(), logical);
        assert_eq!(
            opened.grant.target().target_path(),
            first.canonicalize().unwrap()
        );
        assert!(matches!(
            grants.open_local(
                &file_uri(&parent_alias.join("second.toml")),
                GrantAccess::ReadWrite,
                4096,
            ),
            Err(DocumentHostError::SymlinkComponent { .. })
        ));
        let opened_parent = grants
            .open_local_config(
                &file_uri(&parent_alias.join("second.toml")),
                GrantAccess::ReadWrite,
                4096,
            )
            .unwrap();
        assert_eq!(opened_parent.text, "theme = \"Nord\"\n");
        assert_eq!(
            opened_parent.grant.logical_path(),
            parent_alias.join("second.toml")
        );
        assert_eq!(
            opened_parent.grant.target().target_path(),
            second.canonicalize().unwrap()
        );
        let opened_chain = grants
            .open_local_config(&file_uri(&chained_logical), GrantAccess::ReadWrite, 4096)
            .unwrap();
        assert_eq!(opened_chain.grant.id, opened.grant.id);
        assert_eq!(opened_chain.grant.logical_path(), chained_logical);
        assert_eq!(
            opened_chain.grant.target().target_path(),
            first.canonicalize().unwrap()
        );

        let mut documents = crate::test_document_store();
        let document = documents.open(opened.grant.canonical_uri.clone(), "updated".to_string());
        let mut reducer = SaveReducer::new(document, opened.observed);
        let plan = reducer
            .begin(&documents.snapshot(document).unwrap())
            .unwrap();
        assert!(matches!(
            grants.execute_save(opened.grant.id, &plan),
            AtomicSaveResult::Committed(_)
        ));
        assert_eq!(fs::read_to_string(&first).unwrap(), "updated");
        assert_eq!(fs::read_to_string(&second).unwrap(), "theme = \"Nord\"\n");
        assert!(
            fs::symlink_metadata(&logical)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(&logical).unwrap(),
            PathBuf::from("first.toml")
        );
        assert!(
            fs::symlink_metadata(&chained_final)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(&chained_final).unwrap(),
            PathBuf::from("../first.toml")
        );

        let parent_document = documents.open(
            opened_parent.grant.canonical_uri.clone(),
            "parent-updated".to_string(),
        );
        let mut parent_reducer = SaveReducer::new(parent_document, opened_parent.observed);
        let parent_plan = parent_reducer
            .begin(&documents.snapshot(parent_document).unwrap())
            .unwrap();
        assert!(matches!(
            grants.execute_save(opened_parent.grant.id, &parent_plan),
            AtomicSaveResult::Committed(_)
        ));
        assert_eq!(fs::read_to_string(&second).unwrap(), "parent-updated");
        assert!(
            fs::symlink_metadata(&parent_alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&parent_alias).unwrap(), real_parent);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn final_symlink_swap_is_a_conflict_and_never_redirects_the_save() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "aterm-native-document-link-swap-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.toml");
        let second = dir.join("second.toml");
        let logical = dir.join("aterm.toml");
        fs::write(&first, "theme = \"Default\"\n").unwrap();
        fs::write(&second, "theme = \"Dracula\"\n").unwrap();
        symlink("first.toml", &logical).unwrap();

        let mut grants = DocumentGrantStore::new();
        let direct = grants
            .open_local(&file_uri(&first), GrantAccess::ReadWrite, 4096)
            .unwrap();
        let opened = grants
            .open_local_config(&file_uri(&logical), GrantAccess::ReadWrite, 4096)
            .unwrap();
        assert_eq!(opened.grant.id, direct.grant.id);
        assert_eq!(opened.grant.logical_path(), logical);
        let mut documents = crate::test_document_store();
        let document = documents.open(
            opened.grant.canonical_uri.clone(),
            "theme = \"Nord\"\n".to_string(),
        );
        let mut reducer = SaveReducer::new(document, opened.observed);
        let plan = reducer
            .begin(&documents.snapshot(document).unwrap())
            .unwrap();

        fs::remove_file(&logical).unwrap();
        symlink("second.toml", &logical).unwrap();
        assert!(matches!(
            grants.execute_save(opened.grant.id, &plan),
            AtomicSaveResult::Conflict { .. }
        ));
        assert_eq!(fs::read_to_string(&first).unwrap(), "theme = \"Default\"\n");
        assert_eq!(
            fs::read_to_string(&second).unwrap(),
            "theme = \"Dracula\"\n"
        );
        assert_eq!(
            fs::read_link(&logical).unwrap(),
            PathBuf::from("second.toml")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn recreated_parent_symlink_to_same_destination_is_still_a_conflict() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "aterm-native-document-parent-relink-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let real = dir.join("real");
        let alias = dir.join("config");
        let displaced_alias = dir.join("config.previous");
        fs::create_dir_all(&real).unwrap();
        let target = real.join("aterm.toml");
        let logical = alias.join("aterm.toml");
        fs::write(&target, "theme = \"Default\"\n").unwrap();
        symlink(&real, &alias).unwrap();

        let mut grants = DocumentGrantStore::new();
        let opened = grants
            .open_local_config(&file_uri(&logical), GrantAccess::ReadWrite, 4096)
            .unwrap();
        let mut documents = crate::test_document_store();
        let document = documents.open(
            opened.grant.canonical_uri.clone(),
            "theme = \"Nord\"\n".to_string(),
        );
        let mut reducer = SaveReducer::new(document, opened.observed);
        let plan = reducer
            .begin(&documents.snapshot(document).unwrap())
            .unwrap();

        fs::rename(&alias, &displaced_alias).unwrap();
        symlink(&real, &alias).unwrap();
        assert!(matches!(
            grants.execute_save(opened.grant.id, &plan),
            AtomicSaveResult::Conflict { .. }
        ));
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "theme = \"Default\"\n"
        );
        assert_eq!(fs::read_link(&alias).unwrap(), real);
        let _ = fs::remove_dir_all(&dir);
    }
}
