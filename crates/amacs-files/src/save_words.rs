// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Human-readable descriptions for why an atomic file save or open failed.

use std::io::ErrorKind;
use std::path::Path;

use crate::host::{DocumentHostError, PersistenceError, save_lock_of};
use crate::io::{AtomicSaveStage, SaveCause, SaveError, SaveTouch};

/// The program reporting the failure, used where a sentence needs a subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
    /// aterm's own saves: aterm.toml from Settings, Manual's documents.
    Aterm,
    /// amacs's terminal face (`amacs FILE`, `aterm edit FILE`).
    Amacs,
}

impl Speaker {
    #[inline]
    const fn name(self) -> &'static str {
        match self {
            Self::Aterm => "aterm",
            Self::Amacs => "amacs",
        }
    }
}

/// Where a save stopped.
#[derive(Debug)]
pub enum Failure<'a> {
    /// No folder to keep aterm.toml in: neither `HOME` nor
    /// `XDG_CONFIG_HOME` names one.
    NoPath,
    /// Reading the file before writing it was refused.
    Read(&'a DocumentHostError),
    /// Reading the file to open or reload it was refused.
    Open(&'a DocumentHostError),
    /// The document's save record refused to start the save.
    Begin(&'a PersistenceError),
    /// A write step failed before the target file was replaced.
    Write {
        stage: AtomicSaveStage,
        cause: Option<SaveCause>,
    },
    /// The target file was (or may have been) replaced, but a post-replace check failed.
    Unconfirmed {
        stage: AtomicSaveStage,
        cause: Option<SaveCause>,
    },
}

/// Formats a lower-case clause describing why saving or opening `path` failed.
#[must_use]
pub fn words(speaker: Speaker, path: &Path, failure: &Failure<'_>) -> String {
    let who = speaker.name();
    let file = path
        .file_name()
        .map_or_else(|| "the file".into(), |name| name.to_string_lossy());
    let folder = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| "its folder".to_string(), home_abbreviated);
    match failure {
        Failure::NoPath => {
            format!("{who} can\u{2019}t find your home folder (HOME isn\u{2019}t set)")
        }
        Failure::Read(error) => read_words(who, &file, &folder, error, false),
        Failure::Open(error) => read_words(who, &file, &folder, error, true),
        Failure::Begin(error) => begin_words(&file, error),
        Failure::Write { stage, cause } => {
            let kind = cause.map(|cause| cause.kind);
            medium_words(&folder, kind)
                .or_else(|| cause.and_then(|cause| caused_words(who, path, &file, &folder, cause)))
                .unwrap_or_else(|| step_words(who, &file, &folder, *stage))
        }
        Failure::Unconfirmed { stage, cause } => {
            medium_words(&folder, cause.map(|cause| cause.kind))
                .unwrap_or_else(|| unconfirmed_words(who, &file, *stage))
        }
    }
}

fn home_abbreviated(path: &Path) -> String {
    static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let shown = path.to_string_lossy();
    let home = HOME.get_or_init(|| std::env::var("HOME").ok().filter(|home| !home.is_empty()));
    match home
        .as_deref()
        .and_then(|home| shown.strip_prefix(home))
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
    {
        Some(rest) => format!("~{rest}"),
        None => shown.into_owned(),
    }
}

fn begin_words(file: &str, error: &PersistenceError) -> String {
    match error {
        PersistenceError::UnknownDocument
        | PersistenceError::GrantMismatch
        | PersistenceError::Save(SaveError::WrongDocument) => {
            format!("{file} isn\u{2019}t open any more")
        }
        PersistenceError::Save(SaveError::SaveInFlight) => {
            format!("a save of {file} is still on its way \u{b7} try again in a moment")
        }
        PersistenceError::Save(SaveError::GenerationExhausted) => {
            format!("{file} has been saved too many times since it was opened \u{b7} open it again")
        }
        PersistenceError::Save(SaveError::ConflictRequiresReconcile) => {
            format!("{file} changed on disk since it was read")
        }
        PersistenceError::Save(SaveError::ReconcileRequired) => {
            format!("an earlier save of {file} wasn\u{2019}t confirmed")
        }
    }
}

fn medium_words(folder: &str, kind: Option<ErrorKind>) -> Option<String> {
    Some(match kind? {
        ErrorKind::ReadOnlyFilesystem => format!("the disk holding {folder} is read-only"),
        ErrorKind::StorageFull => {
            "the disk is full \u{b7} free some space, then try again".to_string()
        }
        ErrorKind::QuotaExceeded => {
            "your disk quota is used up \u{b7} free some space, then try again".to_string()
        }
        ErrorKind::TimedOut
        | ErrorKind::StaleNetworkFileHandle
        | ErrorKind::NetworkDown
        | ErrorKind::NetworkUnreachable
        | ErrorKind::HostUnreachable
        | ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::NotConnected => format!(
            "the volume holding {folder} isn\u{2019}t answering \u{b7} try again once it\u{2019}s back"
        ),
        _ => return None,
    })
}

fn caused_words(
    who: &str,
    path: &Path,
    file: &str,
    folder: &str,
    cause: SaveCause,
) -> Option<String> {
    Some(match (cause.touch, cause.kind) {
        (SaveTouch::Read, ErrorKind::PermissionDenied) => {
            format!("{file} isn\u{2019}t readable \u{b7} check its permissions")
        }
        (SaveTouch::Lock, ErrorKind::PermissionDenied) => {
            let lock =
                save_lock_of(path).map_or_else(|| "it".to_string(), |lock| home_abbreviated(&lock));
            format!("{who} can\u{2019}t open its save lock \u{b7} delete {lock}, then try again")
        }
        (SaveTouch::Replace, ErrorKind::PermissionDenied) => {
            format!("{file} can\u{2019}t be replaced: it\u{2019}s locked, or another user owns it")
        }
        (SaveTouch::Folder, ErrorKind::PermissionDenied) => {
            format!("{folder} isn\u{2019}t writable \u{b7} check its permissions")
        }
        (_, ErrorKind::NotADirectory | ErrorKind::AlreadyExists) => {
            format!("{folder} isn\u{2019}t a folder")
        }
        (_, ErrorKind::NotFound) => {
            format!("{folder} went missing while saving \u{b7} try again")
        }
        (_, ErrorKind::WouldBlock) => {
            format!("another program is saving {file} \u{b7} try again in a moment")
        }
        (_, ErrorKind::ResourceBusy | ErrorKind::ExecutableFileBusy) => {
            format!("{file} is in use \u{b7} try again in a moment")
        }
        (_, ErrorKind::FileTooLarge) => format!("{file} would be larger than {who} can save"),
        (_, ErrorKind::Interrupted) => "the save was interrupted \u{b7} try again".to_string(),
        (SaveTouch::Read, _) => format!("{who} couldn\u{2019}t read {file}"),
        _ => return None,
    })
}

fn step_words(who: &str, file: &str, folder: &str, stage: AtomicSaveStage) -> String {
    match stage {
        AtomicSaveStage::Preflight => {
            format!("{who} couldn\u{2019}t get {file} ready to save \u{b7} try again")
        }
        AtomicSaveStage::CreateTemporary => format!("{who} couldn\u{2019}t write in {folder}"),
        AtomicSaveStage::WriteTemporary | AtomicSaveStage::SyncTemporary => {
            "the disk didn\u{2019}t take the write \u{b7} try again".to_string()
        }
        AtomicSaveStage::RenameTarget => {
            format!("{file} couldn\u{2019}t be replaced \u{b7} try again")
        }
        AtomicSaveStage::SyncDirectory
        | AtomicSaveStage::ObserveCommitted
        | AtomicSaveStage::VerifyProof => "the disk didn\u{2019}t confirm the write".to_string(),
    }
}

fn unconfirmed_words(who: &str, file: &str, stage: AtomicSaveStage) -> String {
    match stage {
        AtomicSaveStage::RenameTarget => {
            format!("{who} couldn\u{2019}t tell whether {file} was replaced")
        }
        AtomicSaveStage::ObserveCommitted => format!("{who} couldn\u{2019}t read {file} back"),
        AtomicSaveStage::VerifyProof => {
            format!("{file} read back different from what was saved")
        }
        _ => "the disk didn\u{2019}t confirm the write".to_string(),
    }
}

fn read_words(
    who: &str,
    file: &str,
    folder: &str,
    error: &DocumentHostError,
    opening: bool,
) -> String {
    match error {
        DocumentHostError::SymlinkComponent { dangling: true, .. } => {
            return format!("{file} is a link to a file that isn\u{2019}t there");
        }
        DocumentHostError::InvalidEncoding => {
            return format!("{who} can\u{2019}t open a file whose name isn\u{2019}t UTF-8");
        }
        _ => {}
    }
    match error.refusal_word() {
        "not-regular" => format!("{file} isn\u{2019}t a regular file"),
        "too-large" => format!("{file} is too large for {who} to read"),
        "invalid-utf8" => format!("{file} isn\u{2019}t UTF-8 text"),
        "symlink" => format!("a link on the way to {file} changed \u{b7} try again"),
        "file-provider" => format!(
            "{file} is in iCloud Drive and isn\u{2019}t downloaded \u{b7} download it in Finder"
        ),
        "missing" if opening => format!("{folder} doesn\u{2019}t exist"),
        "missing" => format!("{folder} went missing while saving \u{b7} try again"),
        _ => medium_words(folder, error.io_kind()).unwrap_or_else(|| match error.io_kind() {
            Some(ErrorKind::PermissionDenied) => {
                format!("{file} isn\u{2019}t readable \u{b7} check its permissions")
            }
            _ => format!("{who} couldn\u{2019}t read {file}"),
        }),
    }
}

#[cfg(test)]
#[path = "save_words_tests.rs"]
mod tests;
