// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File-capability host for native Markdown and editor documents.
//!
//! Reducers never receive ambient filesystem access. The owner-facing open path
//! canonicalizes one local `file:` URI into a stable grant, bounds and validates
//! the initial UTF-8 read, and hands later saves the same canonical target. Saves
//! use the proof-shaped protocol from [`crate::io`]: preflight,
//! same-directory temporary write, file sync, atomic rename, directory sync, and
//! a final content observation.

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

/// The save-side half of admission's decoding ([`Decoded::hold`]): a face that
/// writes a document's text itself encodes it with the encoding it was admitted
/// with.
pub use crate::io::encode_for_disk;

pub const DEFAULT_DOCUMENT_LIMIT: usize = 32 * 1024 * 1024;
/// Saves and their proof observations never exceed the same ceiling as a
/// document open/refresh. In particular, Manual may still open and repair a
/// config larger than the 512-KiB semantic-admission cap, up to this ordinary
/// document limit; only an editor buffer grown beyond the openable domain is
/// refused at save preflight.
const MAX_ATOMIC_SAVE_BYTES: usize = DEFAULT_DOCUMENT_LIMIT;
/// [`DEFAULT_DOCUMENT_LIMIT`] as a byte count the contracted predicates compare.
const DOCUMENT_LIMIT_LEN: u64 = DEFAULT_DOCUMENT_LIMIT as u64;
// `exceeds_document_limit`'s clause and theorem spell the limit as the literal
// 32 MiB, and no landing check runs the Trust lane that would reject them once
// a constant moved: an ordinary build fails here instead, and the save ceiling
// stays the one limit the preflight tests.
const _: () =
    assert!(DOCUMENT_LIMIT_LEN == 33_554_432 && MAX_ATOMIC_SAVE_BYTES == DEFAULT_DOCUMENT_LIMIT);
/// The most a bounded read reserves before it reads: the sentinel byte of a
/// read at the document limit.
const READ_RESERVE_CEILING: usize = DEFAULT_DOCUMENT_LIMIT + 1;

trust_contract_fn! {
    /// Whether `len` bytes are past the document limit (32 MiB,
    /// [`DEFAULT_DOCUMENT_LIMIT`]), which no save may write: the preflight
    /// refuses such a buffer before it takes the lock.
    #[must_use]
    fn exceeds_document_limit(len: u64) -> bool
    clauses [ensures result == (len > 33554432) by exceeds_document_limit_is_threshold]
    {
        len > DOCUMENT_LIMIT_LEN
    }
}

// Spec `document-limit-threshold` (SPECS.md): for every length, the limit is
// exceeded exactly past 32 MiB. Kernel-checked against the compiled body.
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

    pub fn target(&self) -> &AtomicFileTarget {
        &self.target
    }

    pub fn targets_logical_path(&self, path: &Path) -> bool {
        self.target.contains_logical_path(path)
    }

    /// Revalidate every path and symlink identity captured when this grant was
    /// minted. Observations may advance the regular target file generation,
    /// but may not silently rebind a changed logical capability.
    pub fn validate_current_binding(&self) -> Result<(), DocumentHostError> {
        validate_atomic_target(&self.target)
    }
}

/// The logical-to-physical binding captured when a writer reads a file.
///
/// Ordinary documents admit no symbolic-link component. Dotfiles-managed
/// configuration deliberately admits them, but binds every link in the logical
/// chain (including parent-directory and final-file links) alongside the canonical
/// physical target. The stored logical path therefore remains `…/aterm.toml`
/// while saves replace the target file without replacing any link. Keeping those
/// roles separate means host APIs can report the path the user opened without
/// turning a later alias open into mutable capability state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomicFileTarget {
    logical_path: PathBuf,
    target_path: PathBuf,
    logical_symlinks: Vec<LogicalSymlinkBinding>,
    additional_logical_bindings: Vec<LogicalPathBinding>,
}

/// One additional spelling admitted for a canonical document already open in
/// this process. A single saver is shared by canonical URI, so it must retain
/// every logical capability instead of silently replacing an earlier binding.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogicalPathBinding {
    logical_path: PathBuf,
    logical_symlinks: Vec<LogicalSymlinkBinding>,
}

/// Stable identity of one admitted symlink in a config path. `path` and
/// `destination` catch a changed component or relative/absolute link spelling,
/// while inode + metadata bind an unlink/recreate even when the replacement points
/// at the same target. Unix (including macOS) supplies the inode identity; the
/// remaining fields keep the check conservative on platforms where it is
/// unavailable.
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

        // Config opens should keep their user-facing dotfiles spelling primary;
        // a later ordinary direct-path open must not displace it. Every displaced
        // primary remains a required binding, so this never weakens an existing
        // capability.
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

/// Exact disk generation used as the compare-and-swap baseline by every aterm
/// config/document writer.
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

/// Result of the shared serialized file-commit authority. Document saves map
/// the durable proof into their reducer; structured Settings consumes the same
/// verdict directly.
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
    /// The atomic replacement succeeded, but a later durability, binding, or
    /// content-proof step failed. Callers must retain their dirty state and
    /// reconcile by re-observing the target before retrying; reporting an
    /// ordinary pre-publication failure here would invite a blind overwrite.
    PublishedUnverified {
        stage: AtomicSaveStage,
        observed: Option<ObservedFileVersion>,
        message: String,
        cause: Option<SaveCause>,
    },
}

/// A failed step of a commit: its account, for a log, and the I/O error
/// behind it with what the step was doing ([`SaveCause`]), which is what a
/// caller words for a person (a folder that is not writable, a full disk);
/// `None` is a refusal of the step's own (a hostile lock entry, a target
/// with no parent, an observation that moved).
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

/// One local file ADMITTED but not yet GRANTED: the blocking half of an open
/// (path resolution, the bounded read, UTF-8 validation), done, and carried to
/// the grant store to be minted into a [`GrantedDocument`].
///
/// This is the seam that lets admission run OFF the main thread. Everything a
/// thread needs to admit a file is in the URI; everything that must stay on the
/// UI thread — the single process-local grant per canonical file — happens in
/// [`DocumentGrantStore::grant_admitted`]. The value is plain owned data and
/// crosses threads by move.
#[derive(Clone, Debug)]
pub struct AdmittedDocument {
    pub(crate) canonical_uri: String,
    pub(crate) path: PathBuf,
    target: AtomicFileTarget,
    pub(crate) text: String,
    pub(crate) observed: ObservedFileVersion,
}

impl AdmittedDocument {
    /// Whether no file exists yet: admitted with [`AdmitOptions::allow_missing`]
    /// as an empty text, which the first save creates.
    pub fn is_missing(&self) -> bool {
        !self.observed.exists
    }

    /// How the admitted text maps back to the file's bytes.
    pub fn encoding(&self) -> DiskEncoding {
        self.observed.encoding()
    }
}

/// What an admission accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmitOptions {
    /// The most bytes the file may hold.
    pub limit: usize,
    /// Bind symbolic links in the path (Manual's dotfiles config) instead of
    /// refusing them, as every other document does.
    pub allow_config_symlinks: bool,
    /// Admit a file that does not exist yet, in a directory that does, as an
    /// empty text (`amacs NEWFILE`, an `edit` with `create=1`). Its first save
    /// creates it, and refuses as a conflict if the file has appeared since.
    pub allow_missing: bool,
}

impl AdmitOptions {
    /// An ordinary document: up to [`DEFAULT_DOCUMENT_LIMIT`], no symbolic
    /// link, and the file must exist.
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

/// The blocking half of [`DocumentGrantStore::open_local`]: resolve the URI,
/// bind the atomic target, read at most `limit` bytes, validate UTF-8. This is
/// where an evicted iCloud Drive item or an unreachable network volume waits
/// (or, with the thread policy OFF, fails as
/// [`DocumentHostError::NotDownloaded`]) — which is why it takes no `&mut self`
/// and may run on a worker thread. A missing file is refused
/// ([`admit_local_file_opts`] can admit one).
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

/// [`admit_local_file`] with every option. The text must be UTF-8 and is held
/// as [`Decoded::hold`] holds it: a byte order mark comes off, and a file whose
/// every line break is CRLF is held with LF; every save puts both back.
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

/// A missing document is created in a directory that exists: a first save
/// writes one file and never makes the directories above it, and fails if the
/// directory is gone by then ([`Directories::Existing`]). A missing directory
/// is refused the way a missing file without `allow_missing` is (refusal word
/// `missing`).
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

/// The canonical path and `file://` URI amacs opens `path` as: an existing
/// file's realpath, or, for a missing file, its directory's realpath plus its
/// own name, the spelling [`AdmitOptions::allow_missing`] admits. A relative
/// path resolves against the working directory. Symbolic links in the spelling
/// are resolved here, so admission's refusal of a symbolic-link component
/// (`resolve_atomic_target`) never meets one; a dangling final link names no
/// file and is refused.
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
        /// The link names no file: the final component of a path to open
        /// is a symbolic link whose target does not exist ([`open_path`]),
        /// not a link met on the way to a file (which a retry may pass).
        dangling: bool,
    },
    TargetRetargeted,
    /// The target is a DATALESS file — evicted from iCloud Drive (or another
    /// File Provider) — and the reading thread's policy forbids waiting for the
    /// download. macOS reports exactly this as `EDEADLK` from `read(2)` when
    /// `IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES` is OFF for the thread (see
    /// aterm-diskwalk's `iopol`); open/stat/realpath all succeed first, so the
    /// read is the only place it can surface.
    #[cfg(unix)]
    NotDownloaded {
        path: PathBuf,
    },
}

/// How a refused read of a file that is not there ends its message; the one
/// spelling [`DocumentHostError::refusal_word`] reads `missing` from.
const MISSING_SUFFIX: &str = " does not exist";

impl DocumentHostError {
    /// One word for why an open refused, for a client to act on (the `edit`
    /// verb's `ERR refused <word>`, design §6.2): `not-regular`, `too-large`,
    /// `invalid-utf8`, `symlink`, `file-provider` (an evicted iCloud Drive
    /// item), `missing`, `bad-uri` (the URI names no local absolute path), or
    /// `unreadable` for the rest.
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

    /// The kind of the I/O error behind a refused read, where one was: what
    /// a caller words for a person (a file that is not readable, a volume
    /// that does not answer), never the error's own text.
    pub fn io_kind(&self) -> Option<std::io::ErrorKind> {
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

/// Persistence ownership keyed by canonical document identity. Views never own a saver,
/// so closing the initiating editor cannot invalidate an in-flight durable completion.
#[derive(Default)]
pub struct DocumentPersistenceStore {
    documents: BTreeMap<DocumentId, DocumentPersistence>,
}

struct DocumentPersistence {
    grant: DocumentGrantId,
    save: SaveReducer,
    // Fingerprints correlate effects and versions, but cannot prove byte
    // equality. A prior version's image remains a reconciliation candidate,
    // but cannot authorize a clean checkpoint of the current version.
    observed_image: Option<ObservedDocumentImage>,
}

/// The exact text of one observed version, as the document holds it.
///
/// The document's text and a fresh read of the same bytes can be two strings,
/// because the mapping is the buffer's and admission reads each file afresh
/// ([`Decoded::hold`]): a text whose every break is CRLF, held as it is (a new
/// file, or a mixed one edited that far), reads back with LF, and a text that
/// starts with U+FEFF and has no mark of its own reads back without it. Both
/// are this version; a check of a fresh read takes either ([`Self::holds`]).
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
    /// fresh read of its bytes gives it: either way the same file bytes. A
    /// fresh read that differs is shorter than those bytes (a mark or CRs come
    /// off), so it is never a buffer whose length matches the version's.
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
/// with `encoding` saves, when that is not `text` itself. Only two texts read
/// back otherwise ([`ObservedDocumentImage`]), and every other one returns
/// `None` without being encoded.
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
            // A fingerprint-only rebind does not retag the prior exact image.
            // It can confirm a fresh read, but not checkpoint the new version.
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

    /// Whether `text` is the exact text of the current observed version: as
    /// the document holds it, or as a fresh read of those bytes gives it
    /// ([`ObservedDocumentImage::holds`]). Both compare file bytes, so a
    /// buffer's own save read back is never a change.
    pub fn observed_text_matches(&self, document: DocumentId, text: &str) -> bool {
        self.documents.get(&document).is_some_and(|persistence| {
            persistence.save.observed().exists
                && persistence.observed_image.as_ref().is_some_and(|image| {
                    image.version == persistence.save.observed() && image.holds(text)
                })
        })
    }

    /// [`Self::observed_text_matches`] against the last exact image, which a
    /// fingerprint-only rebind leaves at its older version.
    pub fn last_observed_text_matches(&self, document: DocumentId, text: &str) -> bool {
        self.documents.get(&document).is_some_and(|persistence| {
            persistence
                .observed_image
                .as_ref()
                .is_some_and(|image| image.holds(text))
        })
    }

    /// The fingerprint of the text a fresh read of the observed version admits,
    /// which is what reopening the file compares a draft journal's base with.
    /// The version's own `content` fingerprints the file's bytes, which are
    /// other bytes when admission took a byte order mark or CRs off. `None`
    /// while no exact image of those bytes is held.
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

    /// Drop `document`'s saver, which nothing is saving through
    /// ([`Self::save_in_flight`] is false), and name the grant it held.
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

    /// Rebind `document`'s baseline to `observed`, whose exact text is `text`
    /// (a fresh read), leaving the buffer as it is.
    ///
    /// The same bytes keep the reading and the mapping the document already
    /// holds, read back either way ([`ObservedDocumentImage::holds`]): such a
    /// read cannot always tell the mapping (a CRLF document with no line break
    /// left reads as plain), and its text may not be the buffer's (a buffer
    /// held as it is whose every break is CRLF reads back with LF, and taking
    /// that reading's mapping would double each CR at the next save). Any
    /// other observation comes with the text a caller installs, and with that
    /// text's own mapping.
    pub fn accept_observation(
        &mut self,
        document: DocumentId,
        observed: ObservedFileVersion,
        text: &str,
    ) -> Result<(), PersistenceError> {
        self.rebind(document, observed, text, SameReading::Either)
    }

    /// [`Self::accept_observation`] for an observation whose fresh read the
    /// caller has just made the buffer's text (a reload, or a discard of local
    /// edits). The buffer holds `text` now, so it keeps its old mapping only
    /// when `text` is exactly its old reading, and otherwise takes the
    /// observation's: a fresh read that differs from the old reading brings
    /// the mapping it was read with, which writes the same bytes back.
    pub fn accept_reloaded_observation(
        &mut self,
        document: DocumentId,
        observed: ObservedFileVersion,
        text: &str,
    ) -> Result<(), PersistenceError> {
        self.rebind(document, observed, text, SameReading::Exact)
    }

    /// Rebind `document`'s baseline to `observed`, a fresh read of a file
    /// that changed under the buffer, for a save that replaces that version
    /// with the buffer's text: the overwrite a person asks for after a save
    /// refused as a conflict (the terminal face's second `C-x C-s`).
    ///
    /// The buffer keeps the mapping its text is held with, whatever the new
    /// bytes' own reading says: its save writes the text back the way it was
    /// read, never with a carriage return a different reading would add
    /// before each held one (a mixed buffer saved as CRLF) or drop (a CRLF
    /// buffer saved as LF). No exact image is kept, since the buffer is not
    /// that version's text; the save's own completion records the next.
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
    pub fn new() -> Self {
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

    /// Config-Manual grant minting binds a complete logical symlink chain while
    /// retaining the ordinary document host's stricter default.
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
        // SYNCHRONOUS admission on the caller's thread. The main-thread callers
        // that must not wait on a download go through `admit_local_file` on a
        // worker and hand the result to `grant_admitted` (App's two-phase open);
        // this form is for test/restore/effect callers that need the result now.
        let admitted = admit_local_file(uri, limit, allow_config_symlinks)?;
        self.grant_admitted(admitted, access)
    }

    /// Mint or upgrade the single process-local grant for one already-admitted
    /// file. Non-blocking: no filesystem read, only the cheap revalidation of an
    /// existing grant's binding. UI-thread only, like every other mutation of the
    /// store.
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
            // Canonical-URI reuse must not throw away the logical chain admitted
            // by this open. First prove every older alias is still valid, then
            // retain the new alias as an additional save-time obligation.
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

    /// Drop the grant of a released document. Opening its URI again admits
    /// the file afresh and mints a new grant.
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

    /// Re-observe one already-minted file capability without widening it or
    /// accepting a caller-supplied path. This is the sole file-watch read seam.
    pub fn refresh_local(
        &self,
        id: DocumentGrantId,
        limit: usize,
    ) -> Result<GrantedDocument, DocumentHostError> {
        self.refresh_bound(id, limit, false)
    }

    /// [`Self::refresh_local`] for a document never saved, whose baseline is a
    /// missing file ([`AdmitOptions::allow_missing`]): a file still absent reads
    /// back as missing and empty, which is no change, instead of failing. Use it
    /// only while the baseline is missing; a file deleted from under a saved
    /// document still fails, as `refresh_local` says.
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

/// Execute a save using only the exact capability minted by the UI-thread grant
/// store. This is the worker-thread seam: it cannot discover or redirect paths.
pub fn execute_granted_save(grant: &DocumentGrant, plan: &SavePlan) -> AtomicSaveResult {
    if !grant.access.permits_write() {
        return failed(AtomicSaveStage::Preflight, "document grant is read-only");
    }
    execute_atomic_save(&grant.target, plan)
}

/// A file's bytes as its buffer holds them ([`Decoded`]): the byte order mark
/// comes off and is remembered, the rest must be UTF-8, and a file whose every
/// line break is CRLF is held with LF ([`Decoded::hold`]). [`encode_for_disk`]
/// gives back the same bytes.
fn decode_utf8(bytes: Vec<u8>) -> Result<Decoded, DocumentHostError> {
    // The BOM arm decodes the bytes after the mark into a new String rather than
    // draining the mark out of the buffer, a range whose bound the prover cannot
    // follow (SPECS.md §2 has the in-place forms measured). The copy briefly holds
    // twice an n-byte file with a mark, no more than the engine's rope build then
    // holds beside the admitted String; a file without one decodes in place. A
    // file held as CRLF is copied once more, by `Decoded::hold`.
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

// Spec `utf8-bom-is-ef-bb-bf` (SPECS.md): for every three bytes, the BOM test
// holds exactly at EF BB BF. Kernel-checked against the compiled body.
verify_island! {
    clean {
        theorem is_utf8_bom_is_ef_bb_bf (b0 : UInt8) (b1 : UInt8) (b2 : UInt8) :
            trust_import_amacs_files__host__is_utf8_bom b0 b1 b2
                = (decide (b0 = 239) && (decide (b1 = 187) && decide (b2 = 191))) := by
          simp only [trust_import_amacs_files__host__is_utf8_bom]
    }
}

/// The absolute path a `file://` URI names: amacs's codec
/// ([`amacs::protocol::uri`]), which the `edit` client spells its URIs with.
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
/// Logical config symlinks have already been resolved and bound by
/// `AtomicFileTarget`; this seam opens only that canonical physical spelling.
/// The regular-file proof comes from the opened handle itself.
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
    // Without BACKUP_SEMANTICS, CreateFileW refuses a DIRECTORY outright with
    // ERROR_ACCESS_DENIED, so a config path that names a directory surfaced as
    // "Preflight: could not open …: Access is denied" instead of the
    // `NotAFile` verdict the Unix arm gives (O_NOFOLLOW|O_NONBLOCK opens the
    // directory, fstat classifies it) — measured 2026-09-22 by
    // `observation_rejects_non_utf8_oversize_and_non_file_inputs` on the first
    // Windows run. With the flag the directory handle opens and the SAME
    // `is_file()` check below classifies it; for a regular file the flag is
    // inert (its privilege override needs SE_BACKUP_NAME, which this process
    // never holds). The proof still comes from the opened handle.
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
    // Portable std has no no-follow open. Reject special/link entries before
    // opening (so a FIFO cannot block), prove the opened handle is regular, and
    // recheck the path entry. Metadata equality is the conservative fallback on
    // platforms without a stable file identity.
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

/// Classify one failed `read(2)` on an already-open regular file. `EDEADLK` is
/// the kernel's one signal that the file is DATALESS and this thread may not
/// wait for its download (aterm-diskwalk's `iopol`); every other error is an
/// ordinary preflight I/O failure.
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
    // The up-front reservation trusts neither the caller's limit nor the file's
    // claimed length past one byte over the document limit; a read allowed more
    // grows the buffer as the bytes actually arrive.
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

/// Read one logical path and capture the exact target + file generation later
/// used by [`commit_atomic_bytes`]. Missing files are represented by empty
/// bytes only when `allow_missing` is true.
#[cfg(any(test, debug_assertions, feature = "test-support"))]
pub fn read_atomic_file(
    logical_path: &Path,
    limit: usize,
    allow_missing: bool,
) -> Result<AtomicFileContents, DocumentHostError> {
    read_atomic_file_with_config_symlinks(logical_path, limit, allow_missing, false)
}

/// Manual/config-only counterpart of [`read_atomic_file`]. Every existing
/// symlink in the logical path is bound and revalidated; ordinary documents
/// remain symlink-free.
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
    // macOS commonly reports `/var/folders/...` as the process temporary root
    // even though `/var` is a compatibility symlink. Normalize only that
    // std-provided root before enforcing the no-symlink capability rule; this
    // keeps test/private temporary files usable without following arbitrary
    // caller-controlled aliases.
    let normalized = normalize_process_temp_root(logical_path)?;
    // Bind the complete caller spelling before resolution, then collect it again
    // after canonicalization. This rejects a component inserted, removed, or
    // replaced during admission rather than silently converting it into mutable
    // capability state. Config paths retain the exact bindings; ordinary document
    // paths reject the first one.
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
        // `normalized` is a validation implementation detail (notably for
        // macOS's std-provided `/var` -> `/private/var` temporary-directory
        // compatibility alias). Public `logical_path` consumers need the exact
        // absolute spelling they supplied so Manual, the watcher, and Settings
        // continue to name one configuration file consistently.
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
///
/// This is NOT cosmetic. macOS reports `$TMPDIR` as `/var/folders/…` while `/var` is a
/// compatibility symlink to `private/var`, so *every* path under the temporary root
/// begins with a symlink component. `resolve_atomic_target` enforces the no-symlink
/// capability rule on the very next line, which makes this rewrite the only reason a
/// temporary-root document is admissible at all.
///
/// It therefore must not degrade silently. `fs::canonicalize` is a syscall and can fail
/// transiently — `EMFILE` on a loaded machine is the realistic one, and this crate's
/// test binary runs ~2500 tests in one process. Swallowing that error returned the
/// UN-normalized path, whose `/var` component the immediately-following
/// `logical_symlink_bindings` check then rejected as
/// [`DocumentHostError::SymlinkComponent`] — reporting a capability violation against a
/// component the caller never wrote, for what was really an I/O failure. Worse, it is
/// load-dependent, so it presents as an unreproducible flake rather than a bug.
///
/// Propagate the true cause instead. A caller that legitimately cannot canonicalize its
/// own temporary root has an I/O problem and deserves to be told which one.
fn normalize_process_temp_root(path: &Path) -> Result<PathBuf, DocumentHostError> {
    let temporary = std::env::temp_dir();
    let Ok(suffix) = path.strip_prefix(&temporary) else {
        // Not under the temporary root: nothing to normalize, and no symlink exemption
        // is being claimed on this path's behalf.
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

/// Walk one absolute path component by component and hand every component that
/// exists to `visit`, together with its `symlink_metadata`. A component that does
/// not exist ends the walk successfully: callers are allowed to admit a suffix that
/// Save has not authored yet, but they have still proved every component that does
/// exist below it.
///
/// This walk used to be written out twice — once in `reject_symlink_components` and
/// once in `logical_symlink_bindings` — and both copies carried the same bug, which
/// is exactly why it now lives in one place. Two homes for one walk meant a fix
/// applied to either one alone would have left the other broken, and they are
/// independently reachable: `normalize_process_temp_root` canonicalizes on its own,
/// so anything under `std::env::temp_dir()` reached the second copy first.
///
/// `Component::Prefix` is accumulated into the path but is never stat-ed on its own,
/// and that is the whole Windows fix. `fs::canonicalize` always returns a verbatim
/// path on Windows, so the first component of a canonicalized path is the bare
/// prefix `\\?\C:` — which names the volume DEVICE, not its root directory.
/// `symlink_metadata` on it fails with ERROR_INVALID_FUNCTION (raw OS error 1), and
/// std maps that to `ErrorKind::Uncategorized`, *not* `NotFound`, so the
/// missing-component arm below never caught it and every canonicalized path fell
/// straight into the hard-error arm. The user-visible damage was total rather than
/// cosmetic: `aterm.toml` neither loaded at startup nor saved, and the app silently
/// fell back to an empty configuration document in which several effects default ON.
///
/// Skipping the prefix does not weaken the guard this walk exists to enforce. A
/// prefix is a volume or UNC designator, not a directory entry: it cannot be a
/// symlink or a reparse point, and there is nothing about it to validate. Stat-ing
/// it was never a check in the first place — on a NON-verbatim path the bare `C:`
/// resolves to the *current directory on drive C:*, so the walk was quietly
/// symlink-checking an unrelated filesystem object and reporting success on it. Both
/// the erroring case and the wrong-object case are cured by not stat-ing it at all,
/// which is why this skips the component rather than merely tolerating its error.
///
/// Everything else is stat-ed exactly as strictly as before, the root directory very
/// much included (`\\?\C:\` stats fine, and `/` always did). Unix has no prefix
/// component at all, so on macOS and Linux this walk is bit-for-bit the one it
/// replaces.
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
            // A root that cannot be stat-ed is not a capability failure and must not
            // be reported as one. Windows has pseudo-roots that the metadata call
            // simply does not accept — a device-namespace prefix such as `\\.\pipe\`
            // is the reachable example — and std lands their failures in
            // `Uncategorized`, precisely as it did for the bare verbatim prefix that
            // caused this bug. Depending on `NotFound` alone to absorb them is the
            // fragility that shipped here once already, so name the raw codes.
            // `continue` rather than `break`: an unstattable root is not a missing
            // root, so every real component underneath it still has to be validated.
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
/// failing the admission. Windows only; on Unix the root is `/` and any failure to
/// stat it is a real problem worth surfacing, so this stays closed there.
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
fn root_stat_is_tolerable(_error: &std::io::Error) -> bool {
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
/// spelling. A missing suffix is allowed so config creation can proceed through a
/// bound parent-directory alias. A dangling link is not a missing suffix: it must
/// fail closed rather than being replaced or used as an unbound creation target.
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
        // Require each admitted link to resolve at admission time. In particular,
        // this distinguishes a missing config suffix from a dangling final link.
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
/// At most `content_limit + 1` bytes are read. A stable file longer than the
/// limit returns its real metadata length plus a prefix/sentinel fingerprint;
/// callers compare length first, so this is a decisive conflict without reading
/// an attacker-grown file to EOF. When lengths can match, the complete content
/// is necessarily within the limit and its fingerprint remains exact.
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
    // A person's save writes one file and makes no folder: the first save of a
    // missing document fails if its directory went away since it was admitted.
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

/// The save lock a commit of `path` takes, which a person may have to
/// remove when it is another user's ([`SaveTouch::Lock`]); `None` for a
/// path with no folder.
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
/// component. The handle which will be locked is itself proved regular.
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

/// The one serialized commit authority used by Manual document saves and every
/// structured Settings write. A no-follow sibling advisory lock serializes
/// cooperating processes; contention fails promptly with explicit retry
/// guidance rather than blocking Manual, close, or Quit. The exact baseline is
/// rechecked under that lock and again just before rename. The unique create-new
/// temporary is synced before publication, then the containing directory and
/// committed bytes are synced/verified.
///
/// Portable filesystems do not expose compare-and-replace for an existing path.
/// A writer that ignores the sibling lock can still race the final observation
/// and rename. The host closes every portable window it can, rejects symlinks for
/// ordinary documents, binds/revalidates the complete chain for config files, and
/// never reports a post-rename failure as a clean failure, but does not claim
/// lock-free CAS against non-cooperating programs.
pub fn commit_atomic_bytes(baseline: &AtomicFileBaseline, bytes: &[u8]) -> AtomicCommitResult {
    commit_atomic_bytes_with_seed(
        baseline,
        bytes,
        next_temporary_seed(),
        Flush::Durable,
        Directories::Create,
    )
}

/// [`commit_atomic_bytes`] with its temporary and its directory entry only ORDERED
/// (`aterm_durable::ordered`) ahead of what follows, not made durable: for a file
/// whose loss after a power loss the next open repairs by itself (the empty config
/// file a missing one is created as). Every save the person makes stays durable.
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
///
/// `flock(2)` binds to the OPEN FILE DESCRIPTION, not the process: "file descriptors
/// duplicated through dup(2) or fork(2) do not result in multiple instances of a lock,
/// but rather multiple references to a single lock". A peer that forks while a save
/// holds this lock hands the child a reference the saver's own `File::drop` cannot
/// release, so the lock stays held for the child's whole fork->exec window even with
/// `O_CLOEXEC` set. Measured on this machine over 200 rounds: 0.27 ms mean / 1.24 ms
/// worst idle, 0.30 ms mean / 0.59 ms worst under load. Refusing a save for a window
/// that short is wrong.
///
/// The budget is deliberately SMALL, for two non-optional reasons:
///
///  1. This is NOT reached only from the `aterm-native-document` worker: the config
///     worker writes the same `aterm.toml` from another thread, so contention here is
///     real. The budget was sized while the retired Settings card still saved
///     SYNCHRONOUSLY on the event-loop thread, where every millisecond was a dropped
///     frame on a keypress; that caller is gone. 25 ms is ~1.5 frames and ~20x the
///     worst measured fork window.
///  2. No clock holds it small. `held_save_lock_returns_busy_and_retry_commits`
///     bounds a whole refused commit by a hang detector (a minute), which catches a
///     refusal that BLOCKS, not one that is merely slow: its old `< 1 s` deadline
///     crossed under full-suite load twice. Keep it within two frames by review.
///
/// MITIGATION for a proven hazard, NOT a proven cure for the
/// `repeated_save_during_inflight_checkpoint_persists_the_latest_sequence` flake: the
/// on-disk forensics narrow that failure to five exits and cannot distinguish them.
///
/// READ THE 200-ROUND FIGURE ABOVE AS "ORDINARY LOAD", NOT AS "WORST CASE". The same
/// window measured at n=20000 under deliberately pathological load (loadavg 118 on 18
/// cores) is p50 4.5 ms, p99 206 ms, MAX 523 ms — `aterm_pty`'s `EXEC_STATUS_BUDGET`
/// doc times exactly this interval, fork to the `O_CLOEXEC` status pipe's EOF at `execve`.
/// 25 ms does not cover that tail and is not asked to: on this path the tail is
/// absorbed by [`PREFLIGHT_RETRY_BUDGET`], which re-runs the whole side-effect-free
/// preflight (lock acquisition included) for up to 500 ms. A caller with no such
/// outer loop must size its budget against the pathological figure instead — see
/// `native_document_journal::JournalLockPatience`.
pub const WRITE_LOCK_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(25);

/// Why a bounded advisory-lock acquisition gave up. The two arms are the only two
/// outcomes [`acquire_advisory_lock_within`] can refuse with, and they are NOT
/// interchangeable: `Busy` is a report about a peer and is retryable by whoever
/// asked; `Failed` is a property of the volume or the handle and never clears by
/// waiting. Each caller renders them in its own product vocabulary — the save path
/// says "retry Save", the journal says "retry opening Manual" — which is the only
/// reason this is an enum instead of a `String`.
pub enum AdvisoryLockRefusal {
    Busy,
    Failed(std::io::Error),
}

/// THE bounded advisory-lock retry for this crate and aterm-gui. Every `flock`
/// contender — [`acquire_write_lock_within`] here,
/// `native_document_journal::take_journal_lock` and
/// `debugger_access::take_change_lock` in aterm-gui — goes through this one loop
/// with its own budget; there is no second implementation of the policy below.
///
/// Riding out a refusal at all is not optional. `flock(2)` binds to the OPEN FILE
/// DESCRIPTION, not the process: "file descriptors duplicated through dup(2) or
/// fork(2) do not result in multiple instances of a lock, but rather multiple
/// references to a single lock". A peer that forks while this lock is held hands the
/// child a reference the holder's own `File::drop` cannot release, so the lock stays
/// taken for the child's whole fork->execve window even with `O_CLOEXEC` set — and
/// aterm forks for every shell (`aterm_pty::spawn_shell_with_pid`, a hand-rolled `forkpty`).
/// A single-shot `try_lock` reports that residue as another owner, which is both a
/// false diagnosis and a lost write.
///
/// THE POLICY, in one place so it cannot drift:
///
///  * The first attempt happens BEFORE any sleep, so an uncontended caller is
///    byte-for-byte as fast as a bare `try_lock`.
///  * `WouldBlock` ONLY is retried. `ENOTSUP` (a volume refusing advisory locks),
///    `EBADF` and `EINVAL` never clear by waiting, and burning the budget on them
///    would only make a permanent refusal slow. `WouldBlock` is not spurious here:
///    std compiles `try_lock` to `flock(fd, LOCK_EX|LOCK_NB)` on Apple and maps only
///    EAGAIN/EWOULDBLOCK onto it, whose one documented cause is a genuine
///    conflicting lock.
///  * Exponential backoff to an 8 ms ceiling, each sleep clamped to what is left of
///    the budget so the wait can never overshoot what the caller asked for. A fixed
///    1 ms poll would spend the whole budget in wakeups for no extra resolution.
///  * The budget is a CEILING, never a floor: `budget` is the most this can cost,
///    and the caller — not this function — decides how much its thread can afford.
///    A budget the clock cannot add to now (`Duration::MAX`) is no ceiling: the
///    wait lasts until the lock is let go.
pub fn acquire_advisory_lock_within(
    mut lock_file: File,
    budget: std::time::Duration,
) -> Result<HeldAdvisoryLock, AdvisoryLockRefusal> {
    // A budget too large for the clock to hold a deadline is no ceiling at all.
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

/// An advisory lock this crate or aterm-gui took — through
/// [`acquire_advisory_lock_within`], [`Self::try_lock`] (the Kitty Log's ledger pair, `aterm.log`'s rotation lock, the
/// dead-instance sweep's lease probe) or [`Self::lock`] (the restore manifest) —
/// released by `LOCK_UN` when this drops, not by the close alone. Take a lock
/// through these and it is never held by a bare `File`: everywhere else the
/// workspace's `clippy.toml` fences `File::lock`/`try_lock` (the flock release
/// rule). The retry
/// above rides out a PEER'S fork residue; this removes OUR OWN. A child another
/// thread forks while the lock is held holds a copy of every descriptor until it
/// execs, so a lock released only by the close stays taken on that copy for the
/// child's whole fork->execve window (523 ms at the pathological tail measured
/// above), and the next contender — the next save, the config thread's
/// `aterm.toml` write, the journal's 25 ms event-loop take of its own lock, the
/// Kitty Log's single-try flush — waits out or is refused for a lock nobody holds.
/// `LOCK_UN` releases the open file description itself, the child's copy included
/// (the product fd-hygiene sweep of 2026-09-27). One `Drop` covers every exit,
/// early refusals included.
#[derive(Debug)]
pub struct HeldAdvisoryLock(File);

impl HeldAdvisoryLock {
    /// Take `lock_file`'s exclusive lock without waiting: the held guard, or why
    /// not (`WouldBlock` when another description holds it). A refused file is
    /// dropped holding nothing.
    pub fn try_lock(lock_file: File) -> Result<Self, std::fs::TryLockError> {
        Self::try_lock_keeping(lock_file).map_err(|(_, refusal)| refusal)
    }

    /// [`Self::try_lock`], handing a refused file back beside the refusal: for a
    /// caller that waits and tries the same description again.
    pub fn try_lock_keeping(lock_file: File) -> Result<Self, (File, std::fs::TryLockError)> {
        // The ascription is the lock-order census's File evidence.
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

    /// Take `lock_file`'s exclusive lock, WAITING for a holder to let go.
    pub fn lock(lock_file: File) -> std::io::Result<Self> {
        // The ascription is the lock-order census's File evidence.
        let lock_file: std::fs::File = lock_file;
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        lock_file.lock()?;
        Ok(Self(lock_file))
    }

    /// The locked file: what the holder reads or writes under the lock (the
    /// edge-token secret), what a journal lock's taker proves its name still
    /// names, and what a test copies to stand in for a forked child's copy of
    /// the description.
    pub fn file(&self) -> &File {
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

/// Bounded retry for the PRE-PUBLICATION phase of a commit.
///
/// Everything from the first `validate_atomic_target` through the version check only
/// READS. Six leaked fixtures of the
/// `repeated_save_during_inflight_checkpoint_persists_the_latest_sequence` flake prove
/// that on disk: the fixture directory's mtime still equals the previous save's rename
/// ctime to within 0-42 ns, so this phase created nothing, renamed nothing and
/// unlinked nothing. A `Failed` verdict carrying `AtomicSaveStage::Preflight` is
/// therefore safe to simply attempt again — and every cause it can carry is transient
/// by construction: a lock held by a peer save or by a `fork`ed child that has not yet
/// `exec`ed away its inherited descriptor, a `canonicalize`/`lstat` that failed under
/// load, an `open` that returned `EINTR`/`EIO`. On-disk forensics cannot tell those
/// exits apart, so ride out the whole CLASS instead of guessing which one fires. A
/// `Conflict` (a real verdict about the target) and every post-publication stage are
/// NOT retried.
///
/// The budget dominates the hold a legitimate saver takes. That hold is two
/// `File::sync_all` calls, which are `F_FULLFSYNC` device-cache barriers on Apple
/// targets: measured on this machine while idle, one file barrier costs 4.43 ms mean
/// and 17.3 ms worst against 0.114 ms for a plain `fsync`, and the six fixtures
/// measure the whole locked section at 82.5-279.7 ms while the volume was busy.
/// `WRITE_LOCK_RETRY_BUDGET` alone is an order of magnitude short of that.
///
/// THE COST, stated plainly because it is real: this is NOT reached only from the
/// `aterm-native-document` worker. `execute_native_config_persistence` (app_native.rs)
/// writes the same `aterm.toml` from another thread, so two saves can contend and one
/// of them waits out this budget. (The retired Settings card's synchronous event-loop
/// save, which made that wait a visible hitch, is gone.)
///
/// It is still the right trade. The budget is only consumed while a peer actually
/// holds the lock, which is bounded by that peer's own `F_FULLFSYNC` pair (82.5-279.7
/// ms measured), so 500 ms is roughly 2x the worst observed hold rather than an
/// arbitrary ceiling; an uncontended save never sleeps at all and is byte-for-byte as
/// fast as before. And the alternative is not a faster save — it is a FAILED one:
/// `App::finish_native_document_save` turns a Preflight failure into a terminal error
/// and drops `pending_saves`, so the user's latest edits are silently never written. A
/// bounded hitch beats losing an edit.
const PREFLIGHT_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// Take the write lock and prove the target is still the generation the plan was built
/// on. Side-effect free: see [`PREFLIGHT_RETRY_BUDGET`].
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
    // Every refusal below re-enters the preflight retry, whose next take must not
    // meet this attempt's own fork residue: the guard releases by LOCK_UN.
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
    // Prefer the file currently named by the logical path. For a deterministic
    // link-chain swap this gives the save reducer the actual competing generation
    // (and therefore a genuine Conflict rather than an unchanged-target failure),
    // while publication itself remains confined to the originally bound target.
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

/// Flush a directory entry change to the device with `flush`. Shared with the draft
/// journal (`native_document_journal`), whose replace path takes the same barrier.
/// `O_NOFOLLOW` costs nothing here: a save's parent comes from a canonical,
/// symlink-free target path.
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
    // Windows rename durability is provided by the file replacement path; opening a
    // directory as a normal File is not portable there.
    Ok(())
}

/// Publish `temporary` at `target`: replace it when it existed, or create it only
/// if it is still absent. Shared with the draft journal.
#[cfg(not(windows))]
pub fn replace_file(temporary: &Path, target: &Path, target_existed: bool) -> std::io::Result<()> {
    if target_existed {
        fs::rename(temporary, target)
    } else {
        // `rename` would overwrite a target created after our final missing
        // observation. A same-directory hard link is an atomic create-if-absent;
        // unlinking the temporary leaves the already-synced inode published.
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
    // ReplaceFileW requires an existing destination. A first save instead
    // publishes the same-directory temporary with MoveFileExW; omitting
    // MOVEFILE_REPLACE_EXISTING also preserves the missing-file CAS if another
    // writer creates the destination after our final observation.
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
        // The callers remove this directory: keep only one we created
        // exclusively, never a predictable name left by another run.
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

    /// A well-formed URI of a file that does not exist, absolute on the host:
    /// `/definitely/…` is not absolute on Windows, where it fails as
    /// `NotAbsolute` before the filesystem is asked — not the arm a
    /// missing-file check is about.
    const MISSING_DOC_URI: &str = if cfg!(windows) {
        "file:///C:/definitely/missing/aterm-doc.md"
    } else {
        "file:///definitely/missing/aterm-doc.md"
    };

    /// Regression guard for the Windows defect that silently disabled the entire user
    /// configuration. The component walk used to push and then stat
    /// `Component::Prefix` on its own, and on a canonicalized path that first
    /// component is the bare verbatim prefix `\\?\C:`, which addresses the volume
    /// device rather than a directory. The resulting ERROR_INVALID_FUNCTION lands in
    /// `ErrorKind::Uncategorized` rather than `NotFound`, so it slipped past the
    /// missing-component arm and failed admission outright: `aterm.toml` could neither
    /// be loaded at startup nor saved. `resolve_atomic_target` canonicalizes and then
    /// feeds the result to this walk unconditionally, so nothing about the user's
    /// machine was needed to trigger it — only Windows.
    ///
    /// The absence of exactly this test is how the bug shipped: every existing walk
    /// test used a logical spelling, which on Windows never contains a prefix that
    /// fails to stat.
    #[test]
    fn component_walk_admits_a_canonicalized_path() {
        let path = unique_file("canonical-walk", b"walk\n");
        let canonical = fs::canonicalize(&path).unwrap();
        #[cfg(windows)]
        {
            // Assert the shape the fix is about, so this cannot start passing for the
            // uninteresting reason that it stopped seeing a verbatim path at all.
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

    /// The same walk is reachable through a second, independent door. Anything under
    /// `std::env::temp_dir()` is rewritten by `normalize_process_temp_root`, which
    /// canonicalizes the temporary root itself, so on Windows `logical_symlink_bindings`
    /// is handed an already-verbatim path and used to die there — before
    /// `reject_symlink_components` was ever reached. A fix confined to one of the two
    /// former copies of this walk would have left every temp-rooted document broken,
    /// this module's own fixtures included, since `unique_file` builds them from the
    /// process temporary root.
    #[test]
    fn component_walk_admits_a_process_temporary_root_path() {
        let path = unique_file("temporary-walk", b"walk\n");
        let normalized = normalize_process_temp_root(&path).unwrap();
        assert!(logical_symlink_bindings(&normalized).unwrap().is_empty());
        reject_symlink_components(&normalized).unwrap();
        // And once end to end, through the admission path the application actually
        // calls, which is the sequence that reported "could not validate \\?\C:".
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

    /// `amacs NEWFILE` and `create=1`: a missing file admits as an empty text
    /// and nothing touches the disk until the first save, which creates it.
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

        // The created file is an ordinary one now: the next save replaces it.
        opened.set_text("second\n");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), b"second\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// The first save's baseline is "no file": a file another writer created
    /// since the admission is a conflict, and is left as that writer left it.
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

    /// Without `allow_missing` a missing file is refused as before; with it, a
    /// missing DIRECTORY still is, since a save makes one file and no folders.
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
        // What was saved is the buffer's text: the watch reads it back as such.
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

    /// A save refused because the file changed under the buffer is refused
    /// again until the disk's version is taken; taken by an overwrite, the
    /// next save writes the buffer with the mapping the buffer holds it with,
    /// whatever the new bytes would read as: a CRLF buffer over an LF file
    /// keeps its CRLF, and a mixed buffer (its CRs in the text) over a CRLF
    /// file gains no second CR.
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
    /// A mixed buffer edited until every break is CRLF still saves as held (a
    /// re-read would take its CRs for line endings and double them), and a CRLF
    /// buffer with no break left still writes CRLF once one is typed again.
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
        // Read back alone, "ab" says nothing of CRLF; accepting that same
        // reading keeps the buffer's mapping.
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

        // Another writer's LF file read back, which a caller installs as the
        // buffer, brings its own mapping: the next save writes LF.
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
    /// back as a mark. The watch's checks compare file bytes, so that save
    /// read back is no change, and accepting the read keeps the buffer's
    /// reading and mapping: the next save writes the same bytes, with no CR
    /// doubled.
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

            // The same bytes written again by another program (a newer
            // generation): the rebind keeps the buffer's mapping too.
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

    /// A reload makes the fresh read the buffer's text, so it brings that
    /// read's mapping, which writes the same bytes back; a reload of the
    /// buffer's own reading keeps the mapping that reading cannot tell.
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

    /// A document's save makes no folder: the first save of a missing file
    /// fails once its directory is gone, and a saved file whose directory went
    /// with it is an existence conflict. Neither puts the directory back.
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
    /// through the missing-baseline refresh. The ordinary refresh refuses a
    /// missing file, as before, and both read the file once it appears.
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

    /// `open_path` hands admission a spelling with no symbolic link in it: an
    /// existing file's realpath, or a missing one's directory realpath plus
    /// its name. Admission itself still refuses the linked spelling.
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

        // macOS's /tmp is itself a link, to /private/tmp.
        #[cfg(target_os = "macos")]
        {
            let name = format!("aterm-open-path-{}-absent.md", std::process::id());
            let (resolved, _) = open_path(&Path::new("/tmp").join(&name)).unwrap();
            assert_eq!(resolved, Path::new("/private/tmp").join(&name));
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// The main thread's dataless-file policy is OFF (aterm-diskwalk's `iopol`),
    /// so an evicted iCloud Drive item's first `read(2)` returns `EDEADLK`
    /// instead of blocking. That one errno — and only that one — is the
    /// "download it in Finder first" verdict; every other read failure stays an
    /// ordinary preflight I/O error.
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

    /// The two halves of an open compose to exactly what `open_local` did in one
    /// call: admission is a pure function of the URI (so a worker can run it) and
    /// granting is the store's only mutation. Admitting twice mints one grant.
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

    /// `is_utf8_bom` is proved (SPECS.md); this holds the decoder around it:
    /// only a whole leading BOM comes off, and what follows is still checked.
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
        // Give the waiter time to meet the held lock; the verdict below does not
        // depend on whether it did.
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
        // SAFETY: `lock_c` is a live NUL-terminated pathname and `mkfifo`
        // retains no pointer. The fixture directory is private to this test.
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
        // Refuses-without-BLOCKING: opening the FIFO for write parks FOREVER (no
        // reader comes), so this is a hang detector, never a latency budget. 1 s
        // crossed under full-suite load (cb8c0cff).
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

    /// The actual regression: a holder that releases INSIDE the budget is ridden out
    /// instead of refusing the save. The budget is a hang detector's minute against
    /// a 20ms hold, so no scheduler stall can turn this into a false FAIL; the
    /// refusal half is `held_save_lock_returns_busy_and_retry_commits`.
    #[test]
    fn a_transient_save_lock_holder_is_ridden_out_instead_of_refusing_the_save() {
        let path = unique_file("transient-save-lock", b"before");
        let lock_path = path.parent().unwrap().join(format!(
            ".{}.aterm-write.lock",
            path.file_name().unwrap().to_string_lossy()
        ));
        // Stands in for a forked child holding an inherited reference to this OFD.
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

        // Before the retry loop this was Err("…busy…"); with it, Ok.
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
        // Refuses-without-BLOCKING: a blocking `flock` on a lock nobody releases
        // parks FOREVER, so this is a hang detector, never a latency budget; the
        // retry budget's size is pinned by a constant, not by this clock. 1 s
        // crossed under full-suite load (cb8c0cff).
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the held save lock parked the commit"
        );
        assert_eq!(fs::read(&path).unwrap(), b"before");

        // Released by LOCK_UN (the guard's drop), not by the close alone: a fork's
        // copy outlives the close until its exec (measured up to 523 ms here), and
        // the retry's preflight gives up after 500 ms (the fd-copy sweep of
        // 2026-09-27).
        drop(held);
        assert!(matches!(
            commit_atomic_bytes(&contents.baseline, b"after"),
            AtomicCommitResult::Committed(_)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"after");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// The save lock is FREE the instant its hold drops, even while a copy of the
    /// holder's description lives. The copy stands in for a shell another thread
    /// forked mid-save: a forked child holds every descriptor until it execs, and a
    /// lock released by the close alone lives on in that copy, so the next save or
    /// `aterm.toml` write — or a refused preflight's own retry — met a lock nobody
    /// held (the product fd-hygiene sweep of 2026-09-27). The refusals after
    /// acquisition drop the same guard this success path returns.
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
        // Released by `LOCK_UN` (the guard's drop) while the forked copy lives.
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

    /// Every document under `$TMPDIR` on macOS begins with a symlink component
    /// (`/var` -> `private/var`), so `normalize_process_temp_root` is the ONLY reason
    /// such a path clears the no-symlink capability rule. Pin that it does — if the
    /// rewrite ever stops happening, this fails with `SymlinkComponent` instead of
    /// surfacing whatever actually went wrong.
    #[test]
    fn temporary_root_document_resolves_despite_the_var_compatibility_symlink() {
        let path = unique_file("temp-root-normalize", b"body");
        let target = resolve_atomic_target(&path, false)
            .expect("a document under the process temporary root must be admissible");
        // The rewrite happened: the resolved target carries the canonical root, and the
        // caller's original spelling is preserved for consumers that must echo it back.
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

    /// A path OUTSIDE the temporary root claims no symlink exemption, so it must pass
    /// through the rewrite untouched rather than acquiring one.
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

        // No ViewId or app instance participates in this completion. A controller may
        // close after issuing the request without turning a durable Ack into a stale one.
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

        // Keep the old inode allocated so recreating the exact same destination
        // is deterministically a distinct logical-link identity.
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
