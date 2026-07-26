// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File-capability host for native Markdown and editor documents.
//!
//! Reducers never receive ambient filesystem access. The owner-facing open path
//! canonicalizes one local `file:` URI into a stable grant, bounds and validates
//! the initial UTF-8 read, and hands later saves the same canonical target. Saves
//! use the proof-shaped protocol from [`crate::native_document_io`]: preflight,
//! same-directory temporary write, file sync, atomic rename, directory sync, and
//! a final content observation.

#![allow(
    dead_code,
    reason = "native document host integration is consumed by the tab opener"
)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::document_store::{DocumentId, DocumentSnapshot};
use crate::native_document_io::{
    AtomicSaveProof, AtomicSaveResult, AtomicSaveStage, ContentFingerprint, ObservedFileVersion,
    SaveError, SaveGeneration, SavePlan, SaveReducer, SaveReduction,
};

pub(crate) const DEFAULT_DOCUMENT_LIMIT: usize = 32 * 1024 * 1024;
/// Saves and their proof observations never exceed the same ceiling as a
/// document open/refresh. In particular, Manual may still open and repair a
/// config larger than the 512-KiB semantic-admission cap, up to this ordinary
/// document limit; only an editor buffer grown beyond the openable domain is
/// refused at save preflight.
const MAX_ATOMIC_SAVE_BYTES: usize = DEFAULT_DOCUMENT_LIMIT;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DocumentGrantId(NonZeroU64);

impl DocumentGrantId {
    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrantAccess {
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
pub(crate) struct DocumentGrant {
    pub(crate) id: DocumentGrantId,
    pub(crate) canonical_uri: String,
    pub(crate) path: PathBuf,
    pub(crate) access: GrantAccess,
    target: AtomicFileTarget,
}

impl DocumentGrant {
    pub(crate) fn logical_path(&self) -> &Path {
        self.target.logical_path()
    }

    pub(crate) fn target(&self) -> &AtomicFileTarget {
        &self.target
    }

    pub(crate) fn targets_logical_path(&self, path: &Path) -> bool {
        self.target.contains_logical_path(path)
    }

    /// Revalidate every path and symlink identity captured when this grant was
    /// minted. Observations may advance the regular target file generation,
    /// but may not silently rebind a changed logical capability.
    pub(crate) fn validate_current_binding(&self) -> Result<(), DocumentHostError> {
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
pub(crate) struct AtomicFileTarget {
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
    pub(crate) fn logical_path(&self) -> &Path {
        &self.logical_path
    }

    pub(crate) fn target_path(&self) -> &Path {
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
pub(crate) struct AtomicFileBaseline {
    pub(crate) target: AtomicFileTarget,
    pub(crate) observed: ObservedFileVersion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AtomicFileContents {
    pub(crate) baseline: AtomicFileBaseline,
    pub(crate) bytes: Vec<u8>,
}

/// Result of the shared serialized file-commit authority. Document saves map
/// the durable proof into their reducer; structured Settings consumes the same
/// verdict directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AtomicCommitResult {
    Committed(AtomicSaveProof),
    Conflict {
        observed: ObservedFileVersion,
        message: String,
    },
    Failed {
        stage: AtomicSaveStage,
        message: String,
    },
    /// The atomic replacement succeeded, but a later durability, binding, or
    /// content-proof step failed. Callers must retain their dirty state and
    /// reconcile by re-observing the target before retrying; reporting an
    /// ordinary pre-publication failure here would invite a blind overwrite.
    PublishedUnverified {
        stage: AtomicSaveStage,
        observed: Option<ObservedFileVersion>,
        message: String,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct GrantedDocument {
    pub(crate) grant: DocumentGrant,
    pub(crate) text: String,
    pub(crate) observed: ObservedFileVersion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DocumentHostError {
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
    ReadOnlyGrant,
    Io {
        stage: AtomicSaveStage,
        message: String,
    },
    ChangedWhileReading,
    SymlinkComponent {
        path: PathBuf,
    },
    TargetRetargeted,
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
            Self::ReadOnlyGrant => f.write_str("document grant is read-only"),
            Self::Io { stage, message } => write!(f, "{stage:?}: {message}"),
            Self::ChangedWhileReading => f.write_str("document changed while it was being read"),
            Self::SymlinkComponent { path } => write!(
                f,
                "symbolic-link path components are not writable capabilities: {}",
                path.display()
            ),
            Self::TargetRetargeted => {
                f.write_str("document path now resolves to a different file; reopen it")
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct DocumentGrantStore {
    next_id: u64,
    grants: BTreeMap<DocumentGrantId, DocumentGrant>,
    by_uri: BTreeMap<String, DocumentGrantId>,
}

/// Persistence ownership keyed by canonical document identity. Views never own a saver,
/// so closing the initiating editor cannot invalidate an in-flight durable completion.
#[derive(Default)]
pub(crate) struct DocumentPersistenceStore {
    documents: BTreeMap<DocumentId, DocumentPersistence>,
}

struct DocumentPersistence {
    grant: DocumentGrantId,
    save: SaveReducer,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingDocumentSave {
    pub(crate) grant: DocumentGrantId,
    pub(crate) plan: SavePlan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PersistenceError {
    UnknownDocument,
    GrantMismatch,
    Save(SaveError),
}

impl DocumentPersistenceStore {
    pub(crate) fn register(
        &mut self,
        document: DocumentId,
        grant: DocumentGrantId,
        observed: ObservedFileVersion,
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
            },
        );
        Ok(())
    }

    pub(crate) fn begin(
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

    pub(crate) fn complete(
        &mut self,
        document: DocumentId,
        generation: SaveGeneration,
        result: AtomicSaveResult,
    ) -> Result<SaveReduction, PersistenceError> {
        let persistence = self
            .documents
            .get_mut(&document)
            .ok_or(PersistenceError::UnknownDocument)?;
        Ok(persistence.save.complete(generation, result))
    }

    pub(crate) fn observed(&self, document: DocumentId) -> Option<ObservedFileVersion> {
        self.documents
            .get(&document)
            .map(|persistence| persistence.save.observed())
    }

    pub(crate) fn save_in_flight(&self, document: DocumentId) -> bool {
        self.documents
            .get(&document)
            .is_some_and(|persistence| persistence.save.pending().is_some())
    }

    pub(crate) fn grant(&self, document: DocumentId) -> Option<DocumentGrantId> {
        self.documents
            .get(&document)
            .map(|persistence| persistence.grant)
    }

    pub(crate) fn accept_observation(
        &mut self,
        document: DocumentId,
        observed: ObservedFileVersion,
    ) -> Result<(), PersistenceError> {
        let persistence = self
            .documents
            .get_mut(&document)
            .ok_or(PersistenceError::UnknownDocument)?;
        persistence
            .save
            .accept_observation(observed)
            .map_err(PersistenceError::Save)
    }
}

impl DocumentGrantStore {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            grants: BTreeMap::new(),
            by_uri: BTreeMap::new(),
        }
    }

    /// Mint or upgrade the single process-local grant for one canonical file.
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
    pub(crate) fn open_local_config(
        &mut self,
        uri: &str,
        access: GrantAccess,
        limit: usize,
    ) -> Result<GrantedDocument, DocumentHostError> {
        self.open_local_with_config_symlinks(uri, access, limit, true)
    }

    fn open_local_with_config_symlinks(
        &mut self,
        uri: &str,
        access: GrantAccess,
        limit: usize,
        allow_config_symlinks: bool,
    ) -> Result<GrantedDocument, DocumentHostError> {
        let requested = file_uri_path(uri)?;
        let contents =
            read_atomic_file_with_config_symlinks(&requested, limit, false, allow_config_symlinks)?;
        let path = contents.baseline.target.target_path.clone();
        if !path.is_absolute() {
            return Err(DocumentHostError::NotAbsolute);
        }
        let canonical_uri = path_to_file_uri(&path)?;
        let text = decode_utf8(contents.bytes)?;
        let observed = contents.baseline.observed;

        let admitted_target = contents.baseline.target;
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

    pub(crate) fn get(&self, id: DocumentGrantId) -> Option<&DocumentGrant> {
        self.grants.get(&id)
    }

    pub(crate) fn cloned_grant(&self, id: DocumentGrantId) -> Option<DocumentGrant> {
        self.grants.get(&id).cloned()
    }

    pub(crate) fn id_for_uri(&self, canonical_uri: &str) -> Option<DocumentGrantId> {
        self.by_uri.get(canonical_uri).copied()
    }

    pub(crate) fn execute_save(&self, id: DocumentGrantId, plan: &SavePlan) -> AtomicSaveResult {
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
    pub(crate) fn refresh_local(
        &self,
        id: DocumentGrantId,
        limit: usize,
    ) -> Result<GrantedDocument, DocumentHostError> {
        let grant = self
            .grants
            .get(&id)
            .cloned()
            .ok_or(DocumentHostError::UnknownGrant)?;
        let contents = read_bound_file(&grant.target, limit)?;
        let observed = contents.baseline.observed;
        let text = decode_utf8(contents.bytes)?;
        Ok(GrantedDocument {
            grant,
            text,
            observed,
        })
    }
}

/// Execute a save using only the exact capability minted by the UI-thread grant
/// store. This is the worker-thread seam: it cannot discover or redirect paths.
pub(crate) fn execute_granted_save(grant: &DocumentGrant, plan: &SavePlan) -> AtomicSaveResult {
    if !grant.access.permits_write() {
        return failed(AtomicSaveStage::Preflight, "document grant is read-only");
    }
    execute_atomic_save(&grant.target, plan)
}

fn decode_utf8(mut bytes: Vec<u8>) -> Result<String, DocumentHostError> {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        bytes.drain(..3);
    }
    String::from_utf8(bytes).map_err(|_| DocumentHostError::InvalidUtf8)
}

fn file_uri_path(uri: &str) -> Result<PathBuf, DocumentHostError> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or(DocumentHostError::UnsupportedScheme)?;
    if rest.contains(['?', '#']) {
        return Err(DocumentHostError::MalformedUri);
    }
    let (authority, encoded_path) = if rest.starts_with('/') {
        ("", rest)
    } else {
        rest.split_once('/')
            .map(|(authority, _path)| (authority, &rest[authority.len()..]))
            .ok_or(DocumentHostError::MalformedUri)?
    };
    if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        return Err(DocumentHostError::RemoteAuthority);
    }
    let decoded = percent_decode(encoded_path)?;
    if decoded.contains('\0') {
        return Err(DocumentHostError::MalformedUri);
    }
    #[cfg(windows)]
    let decoded = windows_file_uri_path_text(&decoded)?;
    let path = PathBuf::from(decoded);
    if !path.is_absolute() {
        return Err(DocumentHostError::NotAbsolute);
    }
    Ok(path)
}

/// RFC 8089 drive paths carry one URI root slash (`/C:/...`) which is not a
/// Windows path root. Remove exactly that slash; all other leading-slash forms
/// retain their meaning and are subsequently accepted/rejected by `Path`.
fn windows_file_uri_path_text(decoded: &str) -> Result<String, DocumentHostError> {
    let bytes = decoded.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        return Ok(decoded[1..].to_string());
    }
    if decoded.starts_with("//") {
        // UNC paths name a remote authority even when encoded with an empty URI
        // authority (`file:////server/share`). The document host is local-only.
        return Err(DocumentHostError::RemoteAuthority);
    }
    Ok(decoded.to_string())
}

fn percent_decode(value: &str) -> Result<String, DocumentHostError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(DocumentHostError::InvalidEncoding);
        }
        let high = hex(bytes[index + 1]).ok_or(DocumentHostError::InvalidEncoding)?;
        let low = hex(bytes[index + 2]).ok_or(DocumentHostError::InvalidEncoding)?;
        decoded.push(high << 4 | low);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| DocumentHostError::InvalidEncoding)
}

const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn path_to_file_uri(path: &Path) -> Result<String, DocumentHostError> {
    let path = path.to_str().ok_or(DocumentHostError::InvalidEncoding)?;
    let mut normalized = if cfg!(windows) {
        path.strip_prefix(r"\\?\")
            .unwrap_or(path)
            .replace('\\', "/")
    } else {
        path.to_string()
    };
    if cfg!(windows)
        && (normalized.starts_with("//")
            || normalized
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC/")))
    {
        return Err(DocumentHostError::RemoteAuthority);
    }
    if cfg!(windows) && !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    let mut uri = String::from("file://");
    for byte in normalized.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':') {
            uri.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(uri, "%{byte:02X}").map_err(|_| DocumentHostError::MalformedUri)?;
        }
    }
    Ok(uri)
}

enum RegularReadHandle {
    Missing,
    Open { file: File, metadata: fs::Metadata },
}

fn open_read_error(path: &Path, error: std::io::Error) -> DocumentHostError {
    DocumentHostError::Io {
        stage: AtomicSaveStage::Preflight,
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
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
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

fn bounded_read(
    file: &mut File,
    metadata: &fs::Metadata,
    limit: usize,
) -> Result<(Vec<u8>, fs::Metadata), DocumentHostError> {
    let sentinel_limit = limit.saturating_add(1);
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(sentinel_limit)
            .min(sentinel_limit),
    );
    std::io::Read::by_ref(file)
        .take(u64::try_from(sentinel_limit).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|error| DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            message: error.to_string(),
        })?;
    let after = file.metadata().map_err(|error| DocumentHostError::Io {
        stage: AtomicSaveStage::Preflight,
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
            message: format!("{} does not exist", path.display()),
        });
    };
    let (bytes, after) = bounded_read(&mut file, &metadata, limit)?;
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
pub(crate) fn read_atomic_file(
    logical_path: &Path,
    limit: usize,
    allow_missing: bool,
) -> Result<AtomicFileContents, DocumentHostError> {
    read_atomic_file_with_config_symlinks(logical_path, limit, allow_missing, false)
}

/// Manual/config-only counterpart of [`read_atomic_file`]. Every existing
/// symlink in the logical path is bound and revalidated; ordinary documents
/// remain symlink-free.
pub(crate) fn read_config_atomic_file(
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
) -> Result<AtomicFileContents, DocumentHostError> {
    validate_atomic_target(target)?;
    let (bytes, observed) = read_stable_file(target.target_path(), limit, false)?;
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
    let normalized = normalize_process_temp_root(logical_path);
    // Bind the complete caller spelling before resolution, then collect it again
    // after canonicalization. This rejects a component inserted, removed, or
    // replaced during admission rather than silently converting it into mutable
    // capability state. Config paths retain the exact bindings; ordinary document
    // paths reject the first one.
    let logical_symlinks = logical_symlink_bindings(&normalized)?;
    if !allow_config_symlinks && let Some(binding) = logical_symlinks.first() {
        return Err(DocumentHostError::SymlinkComponent {
            path: binding.path.clone(),
        });
    }
    let target_path = canonical_target_path(&normalized)?;
    let confirmed_symlinks = logical_symlink_bindings(&normalized)?;
    if !allow_config_symlinks && let Some(binding) = confirmed_symlinks.first() {
        return Err(DocumentHostError::SymlinkComponent {
            path: binding.path.clone(),
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
            message: error.to_string(),
        }),
    }
}

fn normalize_process_temp_root(path: &Path) -> PathBuf {
    let temporary = std::env::temp_dir();
    let Ok(suffix) = path.strip_prefix(&temporary) else {
        return path.to_path_buf();
    };
    fs::canonicalize(&temporary).map_or_else(|_| path.to_path_buf(), |root| root.join(suffix))
}

fn reject_symlink_components(path: &Path) -> Result<(), DocumentHostError> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component.as_os_str());
        match fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(DocumentHostError::SymlinkComponent { path: prefix });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(DocumentHostError::Io {
                    stage: AtomicSaveStage::Preflight,
                    message: format!("could not validate {}: {error}", prefix.display()),
                });
            }
        }
    }
    Ok(())
}

/// Collect every existing symbolic-link component in the caller's logical
/// spelling. A missing suffix is allowed so config creation can proceed through a
/// bound parent-directory alias. A dangling link is not a missing suffix: it must
/// fail closed rather than being replaced or used as an unbound creation target.
fn logical_symlink_bindings(path: &Path) -> Result<Vec<LogicalSymlinkBinding>, DocumentHostError> {
    let mut prefix = PathBuf::new();
    let mut bindings = Vec::new();
    for component in path.components() {
        prefix.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&prefix) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(DocumentHostError::Io {
                    stage: AtomicSaveStage::Preflight,
                    message: format!("could not validate {}: {error}", prefix.display()),
                });
            }
        };
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let destination = fs::read_link(&prefix).map_err(|error| DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            message: format!("could not read symbolic link {}: {error}", prefix.display()),
        })?;
        // Require each admitted link to resolve at admission time. In particular,
        // this distinguishes a missing config suffix from a dangling final link.
        fs::canonicalize(&prefix).map_err(|error| DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            message: format!(
                "could not resolve symbolic link {}: {error}",
                prefix.display()
            ),
        })?;
        bindings.push(LogicalSymlinkBinding {
            path: prefix.clone(),
            destination,
            identity: file_identity(&metadata),
            modified_ns: modified_ns(&metadata),
            len: metadata.len(),
        });
    }
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
                    message: format!("{} has no existing ancestor", path.display()),
                })?;
                suffix.push(name.to_os_string());
                cursor = cursor.parent().ok_or_else(|| DocumentHostError::Io {
                    stage: AtomicSaveStage::Preflight,
                    message: format!("{} has no parent", path.display()),
                })?;
            }
            Err(error) => {
                return Err(DocumentHostError::Io {
                    stage: AtomicSaveStage::Preflight,
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
fn observe_file(path: &Path, content_limit: usize) -> Result<ObservedFileVersion, String> {
    let opened = open_regular_read(path).map_err(|error| error.to_string())?;
    let RegularReadHandle::Open { mut file, metadata } = opened else {
        return Ok(ObservedFileVersion::missing());
    };
    let (bytes, after) =
        bounded_read(&mut file, &metadata, content_limit).map_err(|error| error.to_string())?;
    if bytes.len() <= content_limit && after.len() != bytes.len() as u64 {
        return Err("target changed while it was being observed".to_string());
    }
    Ok(ObservedFileVersion {
        exists: true,
        identity: file_identity(&after),
        modified_ns: modified_ns(&after),
        len: after.len(),
        content: ContentFingerprint::of(&bytes),
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
    match commit_atomic_bytes(
        &AtomicFileBaseline {
            target: target.clone(),
            observed: plan.expected,
        },
        &plan.bytes,
    ) {
        AtomicCommitResult::Committed(proof) => AtomicSaveResult::Committed(proof),
        AtomicCommitResult::Conflict { observed, .. } => AtomicSaveResult::Conflict {
            observed,
            equivalent_rebind_allowed: validate_atomic_target(target).is_ok(),
        },
        AtomicCommitResult::Failed { stage, message } => failed(stage, message),
        AtomicCommitResult::PublishedUnverified {
            stage,
            observed,
            message,
        } => AtomicSaveResult::PublishedUnverified {
            stage,
            observed,
            message,
        },
    }
}

fn write_lock_not_regular(path: &Path) -> String {
    format!(
        "save lock {} is not a regular non-link file; remove the hostile entry and retry Save",
        path.display()
    )
}

/// Open one sibling write lock without following or waiting on its final
/// component. The handle which will be locked is itself proved regular.
#[cfg(unix)]
fn open_write_lock(path: &Path) -> Result<File, String> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("could not open save lock {}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| format!("could not inspect save lock {}: {error}", path.display()))?
        .file_type()
        .is_file()
    {
        return Err(write_lock_not_regular(path));
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("could not protect save lock {}: {error}", path.display()))?;
    Ok(file)
}

#[cfg(windows)]
fn open_write_lock(path: &Path) -> Result<File, String> {
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
        .map_err(|error| format!("could not open save lock {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect save lock {}: {error}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(write_lock_not_regular(path));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_write_lock(path: &Path) -> Result<File, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(write_lock_not_regular(path));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "could not inspect save lock {}: {error}",
                path.display()
            ));
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| format!("could not open save lock {}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| format!("could not inspect save lock {}: {error}", path.display()))?
        .file_type()
        .is_file()
    {
        return Err(write_lock_not_regular(path));
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
pub(crate) fn commit_atomic_bytes(
    baseline: &AtomicFileBaseline,
    bytes: &[u8],
) -> AtomicCommitResult {
    commit_atomic_bytes_with_seed(baseline, bytes, next_temporary_seed())
}

fn commit_atomic_bytes_with_seed(
    baseline: &AtomicFileBaseline,
    bytes: &[u8],
    seed: u64,
) -> AtomicCommitResult {
    if bytes.len() > MAX_ATOMIC_SAVE_BYTES {
        return atomic_failed(
            AtomicSaveStage::Preflight,
            format!("save exceeds the {MAX_ATOMIC_SAVE_BYTES}-byte document limit"),
        );
    }
    let target = &baseline.target;
    if let Err(error) = validate_atomic_target(target) {
        return atomic_validation_failure(target, AtomicSaveStage::Preflight, error);
    }
    let path = target.target_path();
    let Some(parent) = path.parent() else {
        return atomic_failed(AtomicSaveStage::CreateTemporary, "target has no parent");
    };
    if let Err(error) = fs::create_dir_all(parent) {
        return atomic_failed(AtomicSaveStage::CreateTemporary, error.to_string());
    }
    if let Err(error) = validate_atomic_target(target) {
        return atomic_validation_failure(target, AtomicSaveStage::Preflight, error);
    }
    let name = path.file_name().unwrap_or_else(|| OsStr::new("document"));
    let lock_path = parent.join(format!(".{}.aterm-write.lock", name.to_string_lossy()));
    let lock_file = match open_write_lock(&lock_path) {
        Ok(file) => file,
        Err(message) => return atomic_failed(AtomicSaveStage::Preflight, message),
    };
    if let Err(error) = lock_file.try_lock() {
        let message = match error {
            std::fs::TryLockError::WouldBlock => {
                "save target is busy; retry Save after the other writer finishes".to_string()
            }
            std::fs::TryLockError::Error(error) => {
                format!("could not acquire save lock: {error}")
            }
        };
        return atomic_failed(AtomicSaveStage::Preflight, message);
    }

    if let Err(error) = validate_atomic_target(target) {
        return atomic_validation_failure(target, AtomicSaveStage::Preflight, error);
    }
    let baseline_limit = observation_limit(baseline.observed.len);
    let actual = match observe_file(path, baseline_limit) {
        Ok(actual) => actual,
        Err(message) => return atomic_failed(AtomicSaveStage::Preflight, message),
    };
    if crate::native_document_io::detect_version_conflict(baseline.observed, actual).is_some() {
        return AtomicCommitResult::Conflict {
            observed: actual,
            message: "target changed since it was read".to_string(),
        };
    }

    let (temporary, mut file) = match create_unique_temporary(parent, name, seed) {
        Ok(created) => created,
        Err(message) => return atomic_failed(AtomicSaveStage::CreateTemporary, message),
    };
    let result: Result<AtomicSaveProof, AtomicCommitResult> = (|| {
        if let Ok(metadata) = fs::metadata(path) {
            fs::set_permissions(&temporary, metadata.permissions()).map_err(|error| {
                atomic_failed(AtomicSaveStage::CreateTemporary, error.to_string())
            })?;
        }
        file.write_all(bytes)
            .map_err(|error| atomic_failed(AtomicSaveStage::WriteTemporary, error.to_string()))?;
        file.sync_all()
            .map_err(|error| atomic_failed(AtomicSaveStage::SyncTemporary, error.to_string()))?;
        drop(file);

        validate_atomic_target(target).map_err(|error| {
            atomic_validation_failure(target, AtomicSaveStage::RenameTarget, error)
        })?;
        let before_rename = observe_file(path, baseline_limit)
            .map_err(|message| atomic_failed(AtomicSaveStage::RenameTarget, message))?;
        if let Some(conflict) =
            crate::native_document_io::detect_version_conflict(baseline.observed, before_rename)
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
                    format!(
                        "replacement reported {error}; its target could not be reconciled: {message}"
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
                    format!("replacement reported {error}, but the desired bytes are now visible"),
                ));
            }
            if let Some(conflict) =
                crate::native_document_io::detect_version_conflict(baseline.observed, after_error)
            {
                return Err(AtomicCommitResult::Conflict {
                    observed: after_error,
                    message: format!("target changed during rename: {conflict:?}"),
                });
            }
            return Err(atomic_failed(
                AtomicSaveStage::RenameTarget,
                error.to_string(),
            ));
        }
        sync_directory(parent).map_err(|error| {
            atomic_published_unverified(
                AtomicSaveStage::SyncDirectory,
                observe_file(path, bytes.len()).ok(),
                error.to_string(),
            )
        })?;
        validate_atomic_target(target).map_err(|error| {
            atomic_published_unverified(
                AtomicSaveStage::ObserveCommitted,
                observe_file(path, bytes.len()).ok(),
                error.to_string(),
            )
        })?;
        let observed = observe_file(path, bytes.len()).map_err(|message| {
            atomic_published_unverified(AtomicSaveStage::ObserveCommitted, None, message)
        })?;
        if !observed.exists
            || observed.len != bytes.len() as u64
            || observed.content != ContentFingerprint::of(bytes)
        {
            return Err(atomic_published_unverified(
                AtomicSaveStage::VerifyProof,
                Some(observed),
                "committed bytes did not match the save plan".to_string(),
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
) -> Result<(PathBuf, File), String> {
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
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("could not allocate a unique sibling temporary file".to_string())
}

fn next_temporary_seed() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64);
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
        other => atomic_failed(stage, other.to_string()),
    }
}

fn atomic_failed(stage: AtomicSaveStage, message: impl Into<String>) -> AtomicCommitResult {
    AtomicCommitResult::Failed {
        stage,
        message: message.into(),
    }
}

fn atomic_published_unverified(
    stage: AtomicSaveStage,
    observed: Option<ObservedFileVersion>,
    message: impl Into<String>,
) -> AtomicCommitResult {
    AtomicCommitResult::PublishedUnverified {
        stage,
        observed,
        message: message.into(),
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    // Windows rename durability is provided by the file replacement path; opening a
    // directory as a normal File is not portable there.
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, target: &Path, target_existed: bool) -> std::io::Result<()> {
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
fn replace_file(temporary: &Path, target: &Path, target_existed: bool) -> std::io::Result<()> {
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
    // ReplaceFileW requires an existing destination. A first config save instead
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_store::DocumentStore;
    use crate::native_document_io::{SaveReducer, SaveReduction};

    fn unique_file(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-native-document-host-{}-{}",
            std::process::id(),
            name
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hello world.md");
        fs::write(&path, bytes).unwrap();
        path
    }

    fn file_uri(path: &Path) -> String {
        path_to_file_uri(path).unwrap()
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

    #[cfg(unix)]
    #[test]
    fn writerless_fifo_is_rejected_without_blocking_config_or_save_observation() {
        use std::os::unix::ffi::OsStrExt as _;

        const CHILD: &str = "ATERM_DOCUMENT_FIFO_TEST_CHILD";
        const PATH: &str = "ATERM_DOCUMENT_FIFO_TEST_PATH";
        if std::env::var_os(CHILD).is_some() {
            let path = PathBuf::from(std::env::var_os(PATH).unwrap());
            assert!(matches!(
                read_config_atomic_file(&path, 1024, false),
                Err(DocumentHostError::NotAFile)
            ));
            assert!(observe_file(&path, 16).is_err());
            return;
        }

        let dir =
            std::env::temp_dir().join(format!("aterm-native-document-fifo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path_c` is a live NUL-terminated pathname and mkfifo retains
        // no pointer. The private path does not exist.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        let test_name = std::thread::current()
            .name()
            .expect("test harness names the current test")
            .to_string();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test_name, "--nocapture"])
            .env(CHILD, "1")
            .env(PATH, &path)
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("writerless FIFO observation blocked past the deadline");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = fs::remove_dir_all(dir);
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
            } if message.contains("regular non-link")
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
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

    #[test]
    fn held_save_lock_returns_busy_and_retry_commits() {
        let path = unique_file("held-save-lock", b"before");
        let contents = read_atomic_file(&path, DEFAULT_DOCUMENT_LIMIT, false).unwrap();
        let lock_path = path.parent().unwrap().join(format!(
            ".{}.aterm-write.lock",
            path.file_name().unwrap().to_string_lossy()
        ));
        let held = open_write_lock(&lock_path).unwrap();
        held.lock().unwrap();

        let started = std::time::Instant::now();
        assert!(matches!(
            commit_atomic_bytes(&contents.baseline, b"after"),
            AtomicCommitResult::Failed {
                stage: AtomicSaveStage::Preflight,
                message,
            } if message.contains("busy") && message.contains("retry Save")
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(fs::read(&path).unwrap(), b"before");

        drop(held);
        assert!(matches!(
            commit_atomic_bytes(&contents.baseline, b"after"),
            AtomicCommitResult::Committed(_)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"after");
        let _ = fs::remove_dir_all(path.parent().unwrap());
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
            Err(DocumentHostError::SymlinkComponent { path: rejected })
                if rejected == target.target_path()
        ));
        assert_eq!(fs::read(&displaced).unwrap(), b"original");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn manual_can_open_and_repair_config_above_the_semantic_admission_cap() {
        let path = unique_file(
            "manual-repair-oversized-config",
            &vec![b' '; crate::native_config_service::MAX_CONFIG_FILE_BYTES + 1],
        );
        let mut grants = DocumentGrantStore::new();
        let opened = grants
            .open_local_config(
                &file_uri(&path),
                GrantAccess::ReadWrite,
                DEFAULT_DOCUMENT_LIMIT,
            )
            .unwrap();
        assert_eq!(
            opened.text.len(),
            crate::native_config_service::MAX_CONFIG_FILE_BYTES + 1
        );

        let mut documents = DocumentStore::new();
        let document = documents.open(
            opened.grant.canonical_uri.clone(),
            "theme = \"Nord\"\n".to_string(),
        );
        let mut reducer = SaveReducer::new(document, opened.observed);
        let plan = reducer
            .begin(&documents.snapshot(document).unwrap())
            .unwrap();
        assert!(matches!(
            grants.execute_save(opened.grant.id, &plan),
            AtomicSaveResult::Committed(_)
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \"Nord\"\n");
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
            crate::native_document_io::reduce_file_watch(
                crate::native_document_io::FileWatchInput {
                    baseline: contents.baseline.observed,
                    observed: sampled,
                    document_dirty: true,
                    save_in_flight: false,
                }
            ),
            crate::native_document_io::FileWatchReduction::ConflictDirty {
                change: crate::native_document_io::VersionConflict::Content,
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
                    message: "permission denied".to_string(),
                },
            ),
            AtomicCommitResult::Failed {
                stage: AtomicSaveStage::Preflight,
                message,
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
        let mut documents = DocumentStore::new();
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

    #[test]
    fn external_change_returns_conflict_without_overwrite() {
        let path = unique_file("conflict", b"old");
        let mut grants = DocumentGrantStore::new();
        let opened = grants
            .open_local(&file_uri(&path), GrantAccess::ReadWrite, 1024)
            .unwrap();
        let mut documents = DocumentStore::new();
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
            crate::native_document_io::detect_version_conflict(opened.observed, refreshed.observed),
            Some(crate::native_document_io::VersionConflict::Identity),
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
        let mut documents = DocumentStore::new();
        let document = documents.open(opened.grant.canonical_uri.clone(), "saved".to_string());
        let snapshot = documents.snapshot(document).unwrap();
        let mut persistence = DocumentPersistenceStore::default();
        persistence
            .register(document, opened.grant.id, opened.observed)
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
            commit_atomic_bytes_with_seed(&contents.baseline, b"new", seed),
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

        let mut documents = DocumentStore::new();
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
        let mut documents = DocumentStore::new();
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
        let mut documents = DocumentStore::new();
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

    /// Two independent test processes capture the same disk generation, then
    /// race distinct desired bytes through the shipping lock/CAS primitive.
    /// Exactly one may commit; the loser must observe Conflict and cannot erase
    /// the winner. This also exercises PID-separated temporary names for real.
    #[test]
    fn cross_process_same_baseline_has_one_winner_and_no_temp_collision() {
        const CHILD: &str = "ATERM_CONFIG_COMMIT_TEST_CHILD";
        if let Ok(role) = std::env::var(CHILD) {
            let path = PathBuf::from(std::env::var("ATERM_CONFIG_COMMIT_TEST_PATH").unwrap());
            let root = path.parent().unwrap();
            let baseline = read_atomic_file(&path, 4096, false).unwrap().baseline;
            fs::write(root.join(format!("ready-{role}")), b"ready").unwrap();
            let go = root.join("go");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !go.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "parent never released race"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let desired = format!("winner = {role:?}\n");
            let retry_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let verdict = loop {
                match commit_atomic_bytes(&baseline, desired.as_bytes()) {
                    AtomicCommitResult::Committed(_) => break "committed",
                    AtomicCommitResult::Conflict { .. } => break "conflict",
                    AtomicCommitResult::Failed {
                        stage: AtomicSaveStage::Preflight,
                        message,
                    } if message.contains("busy") => {
                        assert!(
                            std::time::Instant::now() < retry_deadline,
                            "save lock remained busy past the bounded retry window"
                        );
                        std::thread::yield_now();
                    }
                    other => panic!("unexpected child commit result: {other:?}"),
                }
            };
            fs::write(root.join(format!("result-{role}")), verdict).unwrap();
            return;
        }

        let test_name = std::thread::current()
            .name()
            .expect("test harness names the current test")
            .to_string();
        let root =
            std::env::temp_dir().join(format!("aterm-config-process-race-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("aterm.toml");
        fs::write(&path, "winner = \"none\"\n").unwrap();
        let mut children = Vec::new();
        for role in ["manual", "settings"] {
            children.push(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .arg("--exact")
                    .arg(&test_name)
                    .arg("--nocapture")
                    .env(CHILD, role)
                    .env("ATERM_CONFIG_COMMIT_TEST_PATH", &path)
                    .spawn()
                    .unwrap(),
            );
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while ["manual", "settings"]
            .iter()
            .any(|role| !root.join(format!("ready-{role}")).exists())
        {
            assert!(
                std::time::Instant::now() < deadline,
                "children never reached barrier"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        fs::write(root.join("go"), b"go").unwrap();
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        let results = ["manual", "settings"]
            .map(|role| fs::read_to_string(root.join(format!("result-{role}"))).unwrap());
        assert_eq!(
            results
                .iter()
                .filter(|result| result.as_str() == "committed")
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| result.as_str() == "conflict")
                .count(),
            1
        );
        let final_text = fs::read_to_string(&path).unwrap();
        assert!(final_text.contains("manual") || final_text.contains("settings"));
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        let _ = fs::remove_dir_all(&root);
    }
}
