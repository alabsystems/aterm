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

use crate::document_store::{DocumentId, DocumentSnapshot};
use crate::native_document_io::{
    AtomicSaveProof, AtomicSaveResult, AtomicSaveStage, ContentFingerprint, ObservedFileVersion,
    SaveError, SaveGeneration, SavePlan, SaveReducer, SaveReduction,
};

pub(crate) const DEFAULT_DOCUMENT_LIMIT: usize = 32 * 1024 * 1024;

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
        let requested = file_uri_path(uri)?;
        let path = fs::canonicalize(requested).map_err(|error| DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            message: error.to_string(),
        })?;
        if !path.is_absolute() {
            return Err(DocumentHostError::NotAbsolute);
        }
        let canonical_uri = path_to_file_uri(&path)?;
        let (bytes, observed) = read_stable_file(&path, limit)?;
        let text = decode_utf8(bytes)?;

        let grant = if let Some(id) = self.by_uri.get(&canonical_uri).copied() {
            let grant = self
                .grants
                .get_mut(&id)
                .ok_or(DocumentHostError::UnknownGrant)?;
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
        execute_atomic_save(&grant.path, plan)
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
        let (bytes, observed) = read_stable_file(&grant.path, limit)?;
        let text = decode_utf8(bytes)?;
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
    execute_atomic_save(&grant.path, plan)
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
    let path = PathBuf::from(decoded);
    if !path.is_absolute() {
        return Err(DocumentHostError::NotAbsolute);
    }
    Ok(path)
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

fn read_stable_file(
    path: &Path,
    limit: usize,
) -> Result<(Vec<u8>, ObservedFileVersion), DocumentHostError> {
    let mut file = File::open(path).map_err(|error| DocumentHostError::Io {
        stage: AtomicSaveStage::Preflight,
        message: error.to_string(),
    })?;
    let before = file.metadata().map_err(|error| DocumentHostError::Io {
        stage: AtomicSaveStage::Preflight,
        message: error.to_string(),
    })?;
    if !before.is_file() {
        return Err(DocumentHostError::NotAFile);
    }
    if before.len() > limit as u64 {
        return Err(DocumentHostError::TooLarge { limit });
    }
    let mut bytes = Vec::with_capacity((before.len() as usize).min(limit));
    std::io::Read::by_ref(&mut file)
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            message: error.to_string(),
        })?;
    if bytes.len() > limit {
        return Err(DocumentHostError::TooLarge { limit });
    }
    let after = file.metadata().map_err(|error| DocumentHostError::Io {
        stage: AtomicSaveStage::Preflight,
        message: error.to_string(),
    })?;
    if metadata_token(&before) != metadata_token(&after) || after.len() != bytes.len() as u64 {
        return Err(DocumentHostError::ChangedWhileReading);
    }
    let observed = observed_version(&after, &bytes);
    Ok((bytes, observed))
}

fn observe_file(path: &Path) -> Result<ObservedFileVersion, String> {
    match fs::read(path) {
        Ok(bytes) => {
            let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
            Ok(observed_version(&metadata, &bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ObservedFileVersion::missing())
        }
        Err(error) => Err(error.to_string()),
    }
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

fn execute_atomic_save(path: &Path, plan: &SavePlan) -> AtomicSaveResult {
    let actual = match observe_file(path) {
        Ok(actual) => actual,
        Err(message) => return failed(AtomicSaveStage::Preflight, message),
    };
    if plan.preflight(actual).is_err() {
        return AtomicSaveResult::Conflict { observed: actual };
    }

    let Some(parent) = path.parent() else {
        return failed(AtomicSaveStage::CreateTemporary, "target has no parent");
    };
    let name = path.file_name().unwrap_or_else(|| OsStr::new("document"));
    let temporary = parent.join(format!(
        ".{}.aterm-{}-{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        plan.generation.0
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options
            .open(&temporary)
            .map_err(|error| (AtomicSaveStage::CreateTemporary, error.to_string()))?;
        if let Ok(metadata) = fs::metadata(path) {
            fs::set_permissions(&temporary, metadata.permissions())
                .map_err(|error| (AtomicSaveStage::CreateTemporary, error.to_string()))?;
        }
        file.write_all(&plan.bytes)
            .map_err(|error| (AtomicSaveStage::WriteTemporary, error.to_string()))?;
        file.sync_all()
            .map_err(|error| (AtomicSaveStage::SyncTemporary, error.to_string()))?;
        drop(file);

        // Re-observe immediately before publication. This catches ordinary watcher/editor
        // races; the rename remains the single atomic visibility point.
        let before_rename =
            observe_file(path).map_err(|message| (AtomicSaveStage::RenameTarget, message))?;
        plan.preflight(before_rename).map_err(|conflict| {
            (
                AtomicSaveStage::RenameTarget,
                format!("target changed before rename: {conflict:?}"),
            )
        })?;
        replace_file(&temporary, path)
            .map_err(|error| (AtomicSaveStage::RenameTarget, error.to_string()))?;
        sync_directory(parent)
            .map_err(|error| (AtomicSaveStage::SyncDirectory, error.to_string()))?;
        let observed =
            observe_file(path).map_err(|message| (AtomicSaveStage::ObserveCommitted, message))?;
        if !observed.exists || observed.content != ContentFingerprint::of(&plan.bytes) {
            return Err((
                AtomicSaveStage::VerifyProof,
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
        Ok(proof) => AtomicSaveResult::Committed(proof),
        Err((stage, message)) if stage == AtomicSaveStage::RenameTarget => {
            match observe_file(path) {
                Ok(observed) if plan.preflight(observed).is_err() => {
                    AtomicSaveResult::Conflict { observed }
                }
                _ => failed(stage, message),
            }
        }
        Err((stage, message)) => failed(stage, message),
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
fn replace_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(temporary, target)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const REPLACEFILE_WRITE_THROUGH: u32 = 0x0000_0001;
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
        ReplaceFileW(
            target.as_ptr(),
            temporary.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
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
}
