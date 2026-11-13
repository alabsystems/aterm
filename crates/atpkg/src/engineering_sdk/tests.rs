// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Real filesystem boundary controls. These do not manufacture an SDK, Ready, or Host owner.

use super::*;
use std::ffi::OsStr;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
// Valid inputs request private modes regardless of the inherited umask. Negative controls
// change only the boundary they exercise, rather than failing first on unrelated permissions.
fn private_directory(path: &std::path::Path) {
    std::fs::DirBuilder::new().mode(0o755).create(path).unwrap();
}
fn private_input(path: &std::path::Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
}
fn fixture() -> (aterm_tempfile::TempDir, fs::Pin) {
    let temp = aterm_tempfile::tempdir().unwrap();
    private_directory(&temp.path().join("tree"));
    private_directory(&temp.path().join("tree/bin"));
    private_input(&temp.path().join("tree/bin/tool"), b"actual original bytes");
    let pin = fs::Pin::open(&std::fs::canonicalize(temp.path().join("tree")).unwrap()).unwrap();
    (temp, pin)
}
#[test]
fn exact_original_inventory_refuses_file_replacement_and_restoration() {
    let (temp, pin) = fixture();
    let original = fs::Snapshot::capture(pin.held(), deadline()).unwrap();
    let path = temp.path().join("tree/bin/tool");
    std::fs::rename(&path, path.with_extension("held")).unwrap();
    private_input(&path, b"substituted bytes");
    assert!(original.recheck(pin.held(), deadline()).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(path.with_extension("held"), &path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"actual original bytes");
    assert!(original.recheck(pin.held(), deadline()).is_err());
}
#[test]
fn actual_parent_namespace_aba_stays_refused_with_original_file_unchanged() {
    let (temp, pin) = fixture();
    let original = fs::Snapshot::capture(pin.held(), deadline()).unwrap();
    let file = File::open(temp.path().join("tree/bin/tool")).unwrap();
    let before = fs::Stamp::of(&file.metadata().unwrap());
    let bin = temp.path().join("tree/bin");
    std::fs::rename(&bin, bin.with_extension("original")).unwrap();
    private_directory(&bin);
    private_input(&bin.join("tool"), b"other input");
    std::fs::remove_file(bin.join("tool")).unwrap();
    std::fs::remove_dir(&bin).unwrap();
    std::fs::rename(bin.with_extension("original"), &bin).unwrap();
    assert_eq!(before, fs::Stamp::of(&file.metadata().unwrap()));
    assert!(original.recheck(pin.held(), deadline()).is_err());
    assert!(pin.check_original_root().is_err());
}
#[test]
fn aliases_are_complete_internal_and_cannot_escape_or_cycle() {
    let (temp, pin) = fixture();
    let alias = temp.path().join("tree/bin/alias");
    std::os::unix::fs::symlink("tool", &alias).unwrap();
    assert!(fs::Snapshot::capture(pin.held(), deadline()).is_ok());
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink("../../outside", &alias).unwrap();
    assert!(fs::Snapshot::capture(pin.held(), deadline()).is_err());
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink("missing", &alias).unwrap();
    assert!(fs::Snapshot::capture(pin.held(), deadline()).is_err());
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink("alias", &alias).unwrap();
    assert!(fs::Snapshot::capture(pin.held(), deadline()).is_err());
}
#[test]
fn native_clone_is_independent_preserves_source_cursor_and_never_overwrites() {
    let (temp, pin) = fixture();
    let source_dir = fs::directory(pin.held(), OsStr::new("bin")).unwrap();
    let mut original = fs::regular(&source_dir, OsStr::new("tool")).unwrap();
    original.seek(SeekFrom::Start(3)).unwrap();
    let before = fs::Stamp::of(&original.metadata().unwrap());
    let output = fs::mkdir(pin.held(), "output").unwrap();
    fs::clone_file(&original, &output, OsStr::new("copy")).unwrap();
    let copy = fs::regular(&output, OsStr::new("copy")).unwrap();
    assert_ne!(
        before.identity(),
        fs::Stamp::of(&copy.metadata().unwrap()).identity()
    );
    assert_eq!(copy.metadata().unwrap().nlink(), 1);
    assert_eq!(original.stream_position().unwrap(), 3);
    assert_eq!(
        fs::hash(&original, deadline()).unwrap(),
        fs::hash(&copy, deadline()).unwrap()
    );
    assert!(fs::clone_file(&original, &output, OsStr::new("copy")).is_err());
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .open(temp.path().join("tree/bin/tool"))
        .unwrap();
    writer.write_all(b"NEW").unwrap();
    let mut observed = Vec::new();
    let mut copy = copy;
    copy.read_to_end(&mut observed).unwrap();
    assert_eq!(observed, b"actual original bytes");
}
#[test]
fn fresh_prefix_binding_refuses_replaced_directory_before_child_mutation() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(temp.path()).unwrap();
    let parent = fs::Pin::open(&real).unwrap();
    let original = fs::mkdir(parent.held(), "fresh").unwrap();
    let layout = crate::store::Layout {
        prefix: real.join("fresh"),
    };
    let lock = crate::lock::StoreLock::create_in_original(&layout, &original).unwrap();
    std::fs::rename(&layout.prefix, temp.path().join("saved-original")).unwrap();
    private_directory(&layout.prefix);
    // Even copying a writer-lock name cannot make a different directory the created prefix.
    std::fs::hard_link(
        temp.path().join("saved-original/store.lock"),
        layout.prefix.join("store.lock"),
    )
    .unwrap();
    assert!(crate::local_harness::fs::Parent::open_original(&lock, "trust", &original).is_err());
    assert!(!layout.prefix.join("store").exists());
    assert!(!temp.path().join("saved-original/store").exists());
}
#[test]
fn an_existing_destination_is_never_entered_or_truncated() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let parent = fs::Pin::open(&std::fs::canonicalize(temp.path()).unwrap()).unwrap();
    private_directory(&temp.path().join("existing"));
    private_input(&temp.path().join("existing/sentinel"), b"unchanged");
    assert!(fs::mkdir(parent.held(), "existing").is_err());
    assert_eq!(
        std::fs::read(temp.path().join("existing/sentinel")).unwrap(),
        b"unchanged"
    );
    assert!(!temp.path().join("existing/store.lock").exists());
}
#[test]
fn special_files_writable_inputs_and_unknown_root_entries_refuse() {
    let (temp, pin) = fixture();
    let path = temp.path().join("tree/bin/tool");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(fs::Snapshot::capture(pin.held(), deadline()).is_err());
    assert!(mapped("unclassified/input").is_err());
    assert!(mapped("PROVENANCE/child").is_err());
    assert_eq!(mapped("PROVENANCE").unwrap(), "base-origin/PROVENANCE");
}
#[test]
fn expired_deadline_is_refusal_before_native_observation() {
    let (_temp, pin) = fixture();
    assert!(fs::Snapshot::capture(pin.held(), Instant::now() - Duration::from_secs(1)).is_err());
}
#[test]
fn fresh_names_below_original_store_or_image_are_refused_without_mutation() {
    let (temp, pin) = fixture();
    let base = pin.path();
    let original = fs::Snapshot::capture(pin.held(), deadline()).unwrap();
    for destination in [
        base.join("new"),
        base.join("lib/new"),
        base.parent().unwrap().to_owned(),
    ] {
        assert!(
            disjoint_destination(&destination, base, base, &temp.path().join("image")).is_err()
        );
    }
    assert!(
        disjoint_destination(
            &temp.path().join("image/new"),
            base,
            base,
            &temp.path().join("image")
        )
        .is_err()
    );
    original.recheck(pin.held(), deadline()).unwrap();
}
#[test]
fn prefix_alias_is_refused_before_it_can_hide_input_overlap() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(temp.path()).unwrap();
    private_directory(&real.join("base"));
    std::os::unix::fs::symlink("base", real.join("alias")).unwrap();
    assert!(fs::Pin::open(&real.join("alias")).is_err());
    assert!(!real.join("base/store.lock").exists());
}
#[test]
fn runtime_profile_explicitly_omits_only_source_namespaces_and_rejects_runtime_alias_into_them() {
    let (temp, pin) = fixture();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(temp.path().join("tree/lib/rustlib/src"))
        .unwrap();
    std::fs::write(
        temp.path().join("tree/lib/rustlib/src/uncaptured"),
        b"not runtime or source custody",
    )
    .unwrap();
    let snapshot = fs::Snapshot::capture_runtime(pin.held(), deadline()).unwrap();
    assert_eq!(snapshot.omitted_source_namespaces.len(), 1);
    assert_eq!(
        snapshot.omitted_source_namespaces[0].path(),
        "lib/rustlib/src"
    );
    assert!(
        !snapshot
            .entries
            .iter()
            .any(|entry| entry.path().starts_with("lib/rustlib/src"))
    );
    // No claim about these content bytes is minted or subsequently required.
    std::fs::write(
        temp.path().join("tree/lib/rustlib/src/uncaptured"),
        b"different non-input bytes",
    )
    .unwrap();
    snapshot.recheck(pin.held(), deadline()).unwrap();
    std::os::unix::fs::symlink(
        "../lib/rustlib/src/uncaptured",
        temp.path().join("tree/bin/escape"),
    )
    .unwrap();
    assert!(fs::Snapshot::capture_runtime(pin.held(), deadline()).is_err());
}
