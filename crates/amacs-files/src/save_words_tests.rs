// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

fn system_words(kind: ErrorKind) -> String {
    std::io::Error::from(kind).to_string()
}

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
    // The lock beside the file, joined as the platform joins it.
    let beside = std::path::Path::new("/Volumes/Shared/aterm").join(".aterm.toml.aterm-write.lock");
    assert!(
        lock.contains(&format!("delete {}", beside.display())),
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

#[test]
fn a_refused_read_is_worded_by_its_refusal() {
    let path = path();
    let io = |kind| DocumentHostError::Io {
        stage: AtomicSaveStage::Preflight,
        message: "could not open /Volumes/Shared/aterm/aterm.toml: Permission denied (os error 13)"
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
            message:
                "could not open /Volumes/Shared/aterm/aterm.toml: Permission denied (os error 13)"
                    .to_string(),
            kind: Some(ErrorKind::PermissionDenied),
        },
    ] {
        let opened = words(Speaker::Amacs, &path, &Failure::Open(&error));
        assert_eq!(opened, words(Speaker::Amacs, &path, &Failure::Read(&error)));
        assert!(problems(&opened, error.io_kind()).is_empty(), "{opened}");
    }
}

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
                let unlocked = |said: String| said.replace(".notes.md.aterm-write.lock", "LOCK");
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
