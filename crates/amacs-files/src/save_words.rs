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

/// `path` with a leading `$HOME` component as `~` (`~` for home itself,
/// `~/notes` below it, any other path as it is), `$HOME` read once.
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

/// Why the document's save record would not start a save of `file`.
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

/// What the disk or the volume holding `folder` says, which reads the same
/// at any step and whatever it was doing.
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

/// What a step that met an I/O error while doing `cause.touch` says of it,
/// where its kind says more than the step does.
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
        // A lock file left by another user (e.g. `sudo`) blocks every save until removed.
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

/// What a step that changed nothing says where nothing more is known of why.
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

/// What a step after the file may have been replaced says: never worded as
/// one that changed nothing.
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

/// Why reading `file` before writing it (or, `opening`, to open it) was
/// refused: by its refusal, and for a refused read the I/O error's kind.
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
mod tests {
    use super::*;

    const STAGES: [AtomicSaveStage; 8] = [
        AtomicSaveStage::Preflight,
        AtomicSaveStage::CreateTemporary,
        AtomicSaveStage::WriteTemporary,
        AtomicSaveStage::SyncTemporary,
        AtomicSaveStage::RenameTarget,
        AtomicSaveStage::SyncDirectory,
        AtomicSaveStage::ObserveCommitted,
        AtomicSaveStage::VerifyProof,
    ];

    const TOUCHES: [SaveTouch; 4] = [
        SaveTouch::Folder,
        SaveTouch::Lock,
        SaveTouch::Read,
        SaveTouch::Replace,
    ];

    /// Every kind a step of the writer can meet.
    const KINDS: [ErrorKind; 19] = [
        ErrorKind::PermissionDenied,
        ErrorKind::ReadOnlyFilesystem,
        ErrorKind::StorageFull,
        ErrorKind::QuotaExceeded,
        ErrorKind::NotFound,
        ErrorKind::NotADirectory,
        ErrorKind::AlreadyExists,
        ErrorKind::WouldBlock,
        ErrorKind::FileTooLarge,
        ErrorKind::TimedOut,
        ErrorKind::StaleNetworkFileHandle,
        ErrorKind::Interrupted,
        ErrorKind::ResourceBusy,
        ErrorKind::InvalidInput,
        ErrorKind::InvalidData,
        ErrorKind::UnexpectedEof,
        ErrorKind::WriteZero,
        ErrorKind::Unsupported,
        ErrorKind::Other,
    ];

    /// Every cause a step can report: none, and each kind at each touch.
    fn causes() -> Vec<Option<SaveCause>> {
        std::iter::once(None)
            .chain(KINDS.iter().flat_map(|&kind| {
                TOUCHES
                    .iter()
                    .map(move |&touch| Some(SaveCause { kind, touch }))
            }))
            .collect()
    }

    fn path() -> std::path::PathBuf {
        std::path::PathBuf::from("/Volumes/Shared/aterm/aterm.toml")
    }

    fn write(stage: AtomicSaveStage, kind: ErrorKind, touch: SaveTouch) -> String {
        words(
            Speaker::Aterm,
            &path(),
            &Failure::Write {
                stage,
                cause: Some(SaveCause { kind, touch }),
            },
        )
    }

    /// What the system would have said for `kind`, as the old words put it
    /// after the step's name ("save failed at CreateTemporary (Permission
    /// denied (os error 13))").
    fn system_words(kind: ErrorKind) -> String {
        std::io::Error::from(kind).to_string()
    }

    /// What is wrong with `said` as words for a person about a failed save
    /// whose writer met `kind`: a step's name, an errno, the system's own
    /// text for the kind, no word of the file, its folder, the disk or the
    /// volume, or a capital where a lead comes before it.
    fn problems(said: &str, kind: Option<ErrorKind>) -> Vec<String> {
        let mut found = Vec::new();
        for stage in STAGES {
            if said.contains(&format!("{stage:?}")) {
                found.push(format!("names the step {stage:?}"));
            }
        }
        if said.contains("os error") {
            found.push("carries an errno".to_string());
        }
        if let Some(kind) = kind
            && said
                .to_lowercase()
                .contains(&system_words(kind).to_lowercase())
        {
            found.push(format!("carries the system's words for {kind:?}"));
        }
        if ![
            "aterm.toml",
            "/Volumes/Shared/aterm",
            "folder",
            "disk",
            "volume",
            "save",
        ]
        .iter()
        .any(|subject| said.contains(subject))
        {
            found.push("says nothing of the file, its folder or the disk".to_string());
        }
        if said.chars().next().is_some_and(char::is_uppercase) {
            found.push("opens with a capital, where a lead comes before it".to_string());
        }
        found
    }

    /// EVERY FAILURE SAYS WHAT WENT WRONG IN A PERSON'S WORDS: for each step,
    /// each kind and each thing the step was doing, written or unconfirmed,
    /// the words name no step and carry neither the system's error text nor
    /// an errno, and they say something about the file, its folder, the disk
    /// or the volume. Negative control: the old words (`{path} save failed at
    /// {stage:?} ({error})`) fail for every step and kind.
    #[test]
    fn every_step_and_kind_is_worded_for_a_person() {
        let path = path();
        for stage in STAGES {
            for cause in causes() {
                let kind = cause.map(|cause| cause.kind);
                for failure in [
                    Failure::Write { stage, cause },
                    Failure::Unconfirmed { stage, cause },
                ] {
                    let said = words(Speaker::Aterm, &path, &failure);
                    let found = problems(&said, kind);
                    assert!(found.is_empty(), "{failure:?}: {said:?} {found:?}");
                }
                let error = kind.map_or_else(
                    || "could not allocate a unique sibling temporary file".to_string(),
                    |kind| format!("{} (os error 13)", system_words(kind)),
                );
                let old = format!("{} save failed at {stage:?} ({error})", path.display());
                assert!(!problems(&old, kind).is_empty(), "{old:?} passes");
            }
        }
        let said = words(Speaker::Aterm, &path, &Failure::NoPath);
        assert!(problems(&said, None).is_empty(), "{said:?}");
    }

    /// WHAT THE PERSON CAN DO FOLLOWS WHAT WENT WRONG AND WHERE: a folder that
    /// is not writable is named, a file that is not readable is named, a save
    /// lock this user cannot open is named for deleting, a file that cannot
    /// be replaced says so; a full disk and an exhausted quota say to free
    /// space; a read-only disk and an unanswering volume name the folder
    /// they hold; a lock another writer holds says to wait; and a step that
    /// changed nothing is never worded as one that may have.
    #[test]
    fn each_kind_says_its_own_thing() {
        let denied = |stage, touch| write(stage, ErrorKind::PermissionDenied, touch);
        let folder = denied(AtomicSaveStage::CreateTemporary, SaveTouch::Folder);
        assert!(
            folder.starts_with("/Volumes/Shared/aterm isn\u{2019}t writable"),
            "{folder}"
        );
        assert_eq!(
            denied(AtomicSaveStage::Preflight, SaveTouch::Folder),
            folder
        );
        let mut seen = std::collections::BTreeSet::new();
        for touch in TOUCHES {
            assert!(
                seen.insert(denied(AtomicSaveStage::Preflight, touch)),
                "{touch:?} reads as another touch"
            );
        }
        let unreadable = denied(AtomicSaveStage::Preflight, SaveTouch::Read);
        assert!(
            unreadable.starts_with("aterm.toml isn\u{2019}t readable"),
            "{unreadable}"
        );
        let lock = denied(AtomicSaveStage::Preflight, SaveTouch::Lock);
        assert!(
            lock.contains("delete /Volumes/Shared/aterm/.aterm.toml.aterm-write.lock"),
            "{lock}"
        );
        let replace = denied(AtomicSaveStage::RenameTarget, SaveTouch::Replace);
        assert!(
            replace.starts_with("aterm.toml can\u{2019}t be replaced"),
            "{replace}"
        );
        for kind in [ErrorKind::StorageFull, ErrorKind::QuotaExceeded] {
            assert!(
                write(AtomicSaveStage::WriteTemporary, kind, SaveTouch::Folder)
                    .contains("free some space"),
                "{kind:?}"
            );
        }
        for kind in [ErrorKind::ReadOnlyFilesystem, ErrorKind::TimedOut] {
            assert!(
                write(AtomicSaveStage::CreateTemporary, kind, SaveTouch::Folder)
                    .contains("/Volumes/Shared/aterm"),
                "{kind:?}"
            );
        }
        assert!(
            write(
                AtomicSaveStage::Preflight,
                ErrorKind::WouldBlock,
                SaveTouch::Lock
            )
            .contains("try again in a moment")
        );
        let mut seen = std::collections::BTreeSet::new();
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::ReadOnlyFilesystem,
            ErrorKind::StorageFull,
            ErrorKind::QuotaExceeded,
            ErrorKind::NotFound,
            ErrorKind::NotADirectory,
            ErrorKind::WouldBlock,
            ErrorKind::FileTooLarge,
            ErrorKind::TimedOut,
            ErrorKind::Interrupted,
            ErrorKind::ResourceBusy,
        ] {
            assert!(
                seen.insert(write(
                    AtomicSaveStage::CreateTemporary,
                    kind,
                    SaveTouch::Folder
                )),
                "{kind:?} reads as another kind"
            );
        }
        let path = path();
        for stage in &STAGES[..5] {
            let stage = *stage;
            let written = words(
                Speaker::Aterm,
                &path,
                &Failure::Write { stage, cause: None },
            );
            let unconfirmed = words(
                Speaker::Aterm,
                &path,
                &Failure::Unconfirmed { stage, cause: None },
            );
            assert_ne!(written, unconfirmed, "{stage:?}");
        }
    }

    /// A REFUSED READ SAYS WHY IN THE FILE'S TERMS, never the reader's
    /// account of it: a file this user cannot read says so, as the same
    /// refusal met mid-save does, and an unanswering volume names its folder.
    #[test]
    fn a_refused_read_is_worded_by_its_refusal() {
        let path = path();
        let io = |kind| DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            message:
                "could not open /Volumes/Shared/aterm/aterm.toml: Permission denied (os error 13)"
                    .to_string(),
            kind,
        };
        for error in [
            DocumentHostError::NotAFile,
            DocumentHostError::TooLarge { limit: 1 },
            DocumentHostError::InvalidUtf8,
            io(None),
            io(Some(ErrorKind::PermissionDenied)),
            io(Some(ErrorKind::TimedOut)),
        ] {
            let said = words(Speaker::Aterm, &path, &Failure::Read(&error));
            assert!(problems(&said, error.io_kind()).is_empty(), "{said}");
            assert!(!said.contains(&error.to_string()), "{said}");
        }
        assert_eq!(
            words(
                Speaker::Aterm,
                &path,
                &Failure::Read(&io(Some(ErrorKind::PermissionDenied)))
            ),
            write(
                AtomicSaveStage::Preflight,
                ErrorKind::PermissionDenied,
                SaveTouch::Read
            ),
            "before the write or in it, an unreadable file reads the same"
        );
        assert!(
            words(
                Speaker::Aterm,
                &path,
                &Failure::Read(&io(Some(ErrorKind::TimedOut)))
            )
            .contains("/Volumes/Shared/aterm isn\u{2019}t answering")
        );
    }

    /// A REFUSED OPEN IS WORDED AS A SAVE'S REFUSED READ IS, but a folder
    /// that is not there does not exist: it did not go missing while saving
    /// (what an open of aterm.toml in a missing folder said).
    #[test]
    fn a_refused_open_says_a_missing_folder_does_not_exist() {
        let path = path();
        let missing = DocumentHostError::Io {
            stage: AtomicSaveStage::Preflight,
            message: "/Volumes/Shared/aterm does not exist".to_string(),
            kind: Some(ErrorKind::NotFound),
        };
        assert_eq!(missing.refusal_word(), "missing");
        assert_eq!(
            words(Speaker::Amacs, &path, &Failure::Open(&missing)),
            "/Volumes/Shared/aterm doesn\u{2019}t exist"
        );
        assert!(words(Speaker::Amacs, &path, &Failure::Read(&missing)).contains("while saving"));
        for error in [
            DocumentHostError::NotAFile,
            DocumentHostError::TooLarge { limit: 1 },
            DocumentHostError::InvalidUtf8,
            DocumentHostError::Io {
                stage: AtomicSaveStage::Preflight,
                message: "could not open /Volumes/Shared/aterm/aterm.toml: Permission denied (os error 13)".to_string(),
                kind: Some(ErrorKind::PermissionDenied),
            },
        ] {
            let opened = words(Speaker::Amacs, &path, &Failure::Open(&error));
            assert_eq!(opened, words(Speaker::Amacs, &path, &Failure::Read(&error)));
            assert!(problems(&opened, error.io_kind()).is_empty(), "{opened}");
        }
    }

    /// A LINK TO NOTHING SAYS SO, AND A NAME THAT IS NOT UTF-8: neither is
    /// a link that changed on the way (which a retry may pass) nor a file
    /// that could not be read, and neither carries the host's own words.
    #[test]
    fn a_link_to_nothing_and_a_name_not_utf8_say_so() {
        let path = path();
        let link = |dangling| DocumentHostError::SymlinkComponent {
            path: path.clone(),
            dangling,
        };
        let dangling = words(Speaker::Amacs, &path, &Failure::Open(&link(true)));
        assert_eq!(
            dangling,
            "aterm.toml is a link to a file that isn\u{2019}t there"
        );
        let raced = words(Speaker::Amacs, &path, &Failure::Open(&link(false)));
        assert!(raced.contains("changed \u{b7} try again"), "{raced}");
        let unnamed = words(
            Speaker::Amacs,
            &path,
            &Failure::Open(&DocumentHostError::InvalidEncoding),
        );
        assert_eq!(
            unnamed,
            "amacs can\u{2019}t open a file whose name isn\u{2019}t UTF-8"
        );
        for (said, error) in [
            (dangling, link(true)),
            (unnamed, DocumentHostError::InvalidEncoding),
        ] {
            assert!(!said.contains(&error.to_string()), "{said}");
        }
    }

    /// A SAVE ITS RECORD WOULD NOT START SAYS WHY IN THE FILE'S TERMS, for
    /// each refusal the record gives: a document no longer open, a save
    /// still on its way, the disk changed or an earlier save unconfirmed,
    /// a record out of generations. Each reads as itself and none carries
    /// the reducer's own name for it (`Save(SaveInFlight)`, the words amacs's
    /// terminal face and Manual printed, `{error:?}`).
    #[test]
    fn a_refused_start_is_worded_by_its_refusal() {
        let path = path();
        let refusals = [
            PersistenceError::UnknownDocument,
            PersistenceError::GrantMismatch,
            PersistenceError::Save(SaveError::WrongDocument),
            PersistenceError::Save(SaveError::GenerationExhausted),
            PersistenceError::Save(SaveError::SaveInFlight),
            PersistenceError::Save(SaveError::ConflictRequiresReconcile),
            PersistenceError::Save(SaveError::ReconcileRequired),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for error in &refusals {
            let said = words(Speaker::Amacs, &path, &Failure::Begin(error));
            assert!(problems(&said, None).is_empty(), "{error:?}: {said:?}");
            assert!(said.contains("aterm.toml"), "{error:?}: {said:?}");
            let debug = format!("{error:?}");
            assert!(
                !said.contains(&debug) && !debug.contains(&said),
                "{error:?}: {said:?}"
            );
            seen.insert(said);
        }
        assert_eq!(seen.len(), refusals.len() - 2, "{seen:?}");
        assert!(
            words(
                Speaker::Amacs,
                &path,
                &Failure::Begin(&PersistenceError::Save(SaveError::SaveInFlight))
            )
            .contains("try again in a moment")
        );
    }

    /// THE WORDS NAME THE PROGRAM THE PERSON IS USING: where a sentence needs
    /// a subject, amacs's terminal face says "amacs" and aterm says "aterm",
    /// and otherwise both say the same thing.
    #[test]
    fn each_face_names_itself() {
        let path = std::path::PathBuf::from("/srv/notes/notes.md");
        let mut named = 0;
        for stage in STAGES {
            for cause in causes() {
                for failure in [
                    Failure::Write { stage, cause },
                    Failure::Unconfirmed { stage, cause },
                ] {
                    let unlocked =
                        |said: String| said.replace(".notes.md.aterm-write.lock", "LOCK");
                    let aterm = unlocked(words(Speaker::Aterm, &path, &failure));
                    let amacs = unlocked(words(Speaker::Amacs, &path, &failure));
                    assert!(!amacs.contains("aterm"), "{failure:?}: {amacs:?}");
                    assert_eq!(aterm.replace("aterm", "amacs"), amacs, "{failure:?}");
                    named += usize::from(amacs.contains("amacs"));
                }
            }
        }
        assert!(named > 0, "some words name the program");
    }
}
