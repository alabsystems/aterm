// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::native_document_io::{AtomicSaveStage, SaveCause, SaveTouch};
use crate::save_words::{Failure, Speaker, words};
use std::io::ErrorKind;
use std::path::Path;

fn save_with(
    path: &Path,
    before: impl FnOnce(),
    after: impl FnOnce(),
) -> (crate::prefs::SaveOutcome, String) {
    use crate::native_config_service::{
        ConfigKeyEdit, ConfigPatchRequest, ConfigPatchResult, ExpectedValue, VersionedConfigService,
    };
    crate::test_scratch::write_config(path, "theme = \"Default\"\n");
    let mut service = VersionedConfigService::load_path(path).unwrap();
    let ConfigPatchResult::Applied { snapshot, .. } = service.patch(ConfigPatchRequest {
        base_revision: service.snapshot().revision,
        edits: vec![ConfigKeyEdit {
            key: "theme".into(),
            expected: ExpectedValue::Exact(Some("Default".to_string())),
            value: Some("Nord".to_string()),
        }],
    }) else {
        panic!("the patch reduces before it is written");
    };
    let plan = service.persistence_plan(snapshot);
    before();
    let outcome = crate::prefs::save_prefs_snapshot(&plan);
    after();
    (outcome, std::fs::read_to_string(path).unwrap())
}

#[test]
fn a_refused_save_says_where_and_why() {
    use std::os::unix::fs::PermissionsExt as _;

    let mode = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let dir = std::env::temp_dir().join(format!("aterm-save-words-{}-refused", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    let lock = dir.join(".aterm.toml.aterm-write.lock");
    assert_eq!(
        crate::native_document_host::save_lock_of(&path).as_deref(),
        Some(lock.as_path())
    );
    let expect = |stage, touch| {
        words(
            Speaker::Aterm,
            &path,
            &Failure::Write {
                stage,
                cause: Some(SaveCause {
                    kind: ErrorKind::PermissionDenied,
                    touch,
                }),
            },
        )
    };
    let (outcome, after) = save_with(&path, || mode(&dir, 0o500), || mode(&dir, 0o700));
    let folder = expect(AtomicSaveStage::Preflight, SaveTouch::Folder);
    assert!(folder.contains("isn\u{2019}t writable"), "{folder}");
    assert_eq!(outcome, crate::prefs::SaveOutcome::Error(folder.clone()));
    assert_eq!(after, "theme = \"Default\"\n");
    std::fs::write(&lock, b"").unwrap();
    let (outcome, after) = save_with(&path, || mode(&dir, 0o500), || mode(&dir, 0o700));
    assert_eq!(
        outcome,
        crate::prefs::SaveOutcome::Error(expect(
            AtomicSaveStage::CreateTemporary,
            SaveTouch::Folder
        ))
    );
    assert_eq!(after, "theme = \"Default\"\n");
    let (outcome, after) = save_with(&path, || mode(&path, 0o200), || mode(&path, 0o600));
    let unreadable = expect(AtomicSaveStage::Preflight, SaveTouch::Read);
    assert!(unreadable.contains("isn\u{2019}t readable"), "{unreadable}");
    assert_eq!(
        outcome,
        crate::prefs::SaveOutcome::Error(unreadable.clone())
    );
    assert_eq!(after, "theme = \"Default\"\n");
    let (outcome, after) = save_with(&path, || mode(&lock, 0o400), || mode(&lock, 0o600));
    let locked = expect(AtomicSaveStage::Preflight, SaveTouch::Lock);
    assert!(locked.contains("delete "), "{locked}");
    assert_eq!(outcome, crate::prefs::SaveOutcome::Error(locked.clone()));
    assert_eq!(after, "theme = \"Default\"\n");
    let _ = std::fs::remove_dir_all(dir);
}
