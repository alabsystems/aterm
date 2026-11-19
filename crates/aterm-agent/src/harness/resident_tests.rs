// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CONTEXT, END, FOLDER, PREFIX, VERSION, prepare_resident};
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

#[test]
fn resident_folder_is_created_owner_only_in_the_harness_state_directory() {
    let state = aterm_tempfile::tempdir().unwrap();
    let base = state.path().join("state");
    fs::create_dir(&base).unwrap();
    let harness_state = base.join("harness").join("claude-harness");
    fs::set_permissions(&base, fs::Permissions::from_mode(0o775)).unwrap();
    assert!(prepare_resident(&harness_state).is_err());
    assert!(!base.join("harness").exists());
    fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
    let folder = prepare_resident(&harness_state).unwrap();
    assert_eq!(folder, harness_state.join(FOLDER));
    assert_eq!(fs::metadata(&folder).unwrap().mode() & 0o777, 0o700);
    let context = folder.join(CONTEXT);
    let original = fs::metadata(&context).unwrap();
    assert_eq!(original.mode() & 0o777, 0o600);
    assert!(
        fs::read_to_string(&context)
            .unwrap()
            .contains(aterm_primer::RESIDENT_BRIEF.trim_end())
    );
    prepare_resident(&harness_state).unwrap();
    assert_eq!(original.ino(), fs::metadata(context).unwrap().ino());
}

#[cfg(target_os = "macos")]
#[test]
fn a_harness_state_parent_with_an_acl_grant_is_refused() {
    let state = aterm_tempfile::tempdir().unwrap();
    let base = state.path().join("harness");
    fs::create_dir(&base).unwrap();
    fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
    let granted = aterm_uds::spawnfd::command(crate::test_program::kept("/bin/chmod"))
        .args(["+a", "everyone allow list"])
        .arg(&base)
        .status()
        .unwrap();
    assert!(granted.success());
    let missing = base.join("claude-harness");
    assert!(prepare_resident(&missing).is_err());
    assert!(!missing.exists());
    assert!(prepare_resident(&base).is_err());
    assert!(!base.join(FOLDER).exists());
}

#[test]
fn the_resident_block_is_written_again_when_edited() {
    let state = aterm_tempfile::tempdir().unwrap();
    let context = prepare_resident(state.path()).unwrap().join(CONTEXT);
    let old = format!("before\n\n{PREFIX}{VERSION} -->\nedited\n{END}\nafter\n");
    fs::write(&context, old).unwrap();
    prepare_resident(state.path()).unwrap();
    let revised = fs::read_to_string(&context).unwrap();
    assert!(revised.starts_with("before\n\n"));
    assert!(revised.ends_with("after\n"));
    assert!(revised.contains(aterm_primer::RESIDENT_BRIEF.trim_end()));
    assert_eq!(revised.matches(PREFIX).count(), 1);
    let future = revised.replace(
        &format!("{PREFIX}{VERSION}"),
        &format!("{PREFIX}{}", VERSION + 1),
    );
    fs::write(&context, &future).unwrap();
    prepare_resident(state.path()).unwrap();
    assert_eq!(fs::read_to_string(context).unwrap(), future);
}

#[test]
fn resident_context_refuses_aliases_and_unfinished_managed_blocks() {
    let state = aterm_tempfile::tempdir().unwrap();
    let other = aterm_tempfile::tempdir().unwrap();
    let outside = other.path().join(CONTEXT);
    fs::write(&outside, "kept").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    let folder = state.path().join(FOLDER);
    symlink(other.path(), &folder).unwrap();
    assert!(prepare_resident(state.path()).is_err());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "kept");
    fs::remove_file(&folder).unwrap();
    let context = prepare_resident(state.path()).unwrap().join(CONTEXT);
    fs::remove_file(&context).unwrap();
    symlink(&outside, &context).unwrap();
    assert!(prepare_resident(state.path()).is_err());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "kept");
    fs::remove_file(&context).unwrap();
    fs::hard_link(&outside, &context).unwrap();
    assert!(prepare_resident(state.path()).is_err());
    fs::remove_file(&context).unwrap();
    prepare_resident(state.path()).unwrap();
    let broken = format!("notes\n{PREFIX}{VERSION} -->\nincomplete");
    fs::write(&context, &broken).unwrap();
    assert!(prepare_resident(state.path()).is_err());
    assert_eq!(fs::read_to_string(context).unwrap(), broken);
}

#[test]
fn primer_writes_the_resident_block_for_claude_code() {
    let state = aterm_tempfile::tempdir().unwrap();
    let aliases = aterm_tempfile::tempdir().unwrap();
    let alias = aliases.path().join("state");
    symlink(state.path(), &alias).unwrap();
    assert!(prepare_resident(&alias).is_err());
    assert!(!state.path().join(FOLDER).exists());

    let worker = state.path().join("worker");
    fs::create_dir(&worker).unwrap();
    fs::write(worker.join(CONTEXT), "ordinary worker instructions").unwrap();
    let context = prepare_resident(state.path()).unwrap().join(CONTEXT);
    let notes = "notes under version control\n\n";
    fs::write(&context, notes).unwrap();
    prepare_resident(state.path()).unwrap();
    assert!(fs::read_to_string(context).unwrap().starts_with(notes));
    assert_eq!(
        fs::read_to_string(worker.join(CONTEXT)).unwrap(),
        "ordinary worker instructions"
    );
}
