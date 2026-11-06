// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Opt-in native capability proof, using only a synthetic private keychain.

#![cfg(target_os = "macos")]

use aterm_machines::port::claude_keychain::{
    test_prepare_private_keychain, test_private_keychain_diagnostic, test_read_private_keychain,
    test_seed_private_keychain, test_vendor_scope,
};
use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn security(args: &[&OsStr]) -> std::process::Output {
    let mut command = aterm_uds::spawnfd::command("/usr/bin/security");
    command.args(args);
    aterm_uds::deadline::output_until(&mut command, Instant::now() + Duration::from_secs(60))
        .expect("synthetic Keychain command exceeded its deadline")
}

fn search_state() -> (Vec<u8>, Vec<u8>) {
    let list = security(&["list-keychains".as_ref(), "-d".as_ref(), "user".as_ref()]);
    let default = security(&["default-keychain".as_ref(), "-d".as_ref(), "user".as_ref()]);
    assert!(list.status.success() && default.status.success());
    (list.stdout, default.stdout)
}

struct Keychain(PathBuf);

impl Drop for Keychain {
    fn drop(&mut self) {
        let mut command = aterm_uds::spawnfd::command("/usr/bin/security");
        command.arg("delete-keychain").arg(&self.0);
        let _ = aterm_uds::deadline::output_until(
            &mut command,
            Instant::now() + Duration::from_secs(60),
        );
    }
}

fn read(program: &Path, stage: &Path) -> Result<Option<aterm_secrets::SecretBytes>, String> {
    test_read_private_keychain(program, stage, Instant::now() + Duration::from_secs(60))
        .map_err(|error| error.to_string())
}

fn create_keychain(stage: &Path) -> Keychain {
    let directory = stage.join(".synthetic-keychain");
    aterm_uds::private_file::create_private_dir(&directory).unwrap();
    let keychain = Keychain(directory.join("synthetic.keychain-db"));
    let created = security(&[
        "create-keychain".as_ref(),
        "-p".as_ref(),
        "synthetic-fixture-password".as_ref(),
        keychain.0.as_os_str(),
    ]);
    assert!(created.status.success(), "private keychain creation failed");
    let unlocked = security(&[
        "unlock-keychain".as_ref(),
        "-p".as_ref(),
        "synthetic-fixture-password".as_ref(),
        keychain.0.as_os_str(),
    ]);
    assert!(unlocked.status.success(), "private keychain unlock failed");
    keychain
}

#[test]
#[ignore = "explicit native ACL capability proof; creates only its own private synthetic keychain"]
fn security_created_item_does_not_grant_the_no_ui_child_access() {
    let before = search_state();
    let temp = aterm_tempfile::tempdir().unwrap();
    let logins = temp.path().join("logins");
    aterm_uds::private_file::create_private_dir(&logins).unwrap();
    let stage = logins.join("synthetic+1");
    aterm_uds::private_file::create_private_dir(&stage).unwrap();
    let keychain = create_keychain(&stage);
    assert!(
        search_state() == before,
        "private creation changed search preferences"
    );

    let program = aterm_tempfile::bin_exe("aterm-machines-dev");
    let program = program.as_path();
    assert!(read(program, &stage).unwrap().is_none());
    let (service, account) = test_vendor_scope(&stage).unwrap();
    let inserted = security(&[
        "add-generic-password".as_ref(),
        "-U".as_ref(),
        "-a".as_ref(),
        account.as_ref(),
        "-s".as_ref(),
        service.as_ref(),
        "-X".as_ref(),
        "73796e7468657469632d636c617564652d66697874757265".as_ref(),
        keychain.0.as_os_str(),
    ]);
    assert!(
        inserted.status.success(),
        "native synthetic item creation failed"
    );
    let mut creator_read = security(&[
        "find-generic-password".as_ref(),
        "-a".as_ref(),
        account.as_ref(),
        "-s".as_ref(),
        service.as_ref(),
        "-w".as_ref(),
        keychain.0.as_os_str(),
    ]);
    let creator_has_exact_item = creator_read.status.success()
        && creator_read.stdout.trim_ascii_end() == b"synthetic-claude-fixture";
    aterm_uds::secret::scrub(&mut creator_read.stdout);
    assert!(
        creator_has_exact_item,
        "native creator cannot read its synthetic item"
    );
    let result = read(program, &stage);
    let diagnostic = result.as_ref().err().map(|_| {
        test_private_keychain_diagnostic(program, &stage, Instant::now() + Duration::from_secs(60))
            .unwrap_or_else(|error| error.to_string())
    });
    drop(keychain);
    assert!(
        search_state() == before,
        "synthetic proof changed search preferences"
    );
    assert!(
        result.is_err(),
        "native writer unexpectedly granted another reader access"
    );
    eprintln!("default native access: {diagnostic:?}");
}

#[test]
#[ignore = "explicit native ACL capability proof; creates only its own private synthetic keychain"]
fn precreated_access_survives_the_native_update_command() {
    let before = search_state();
    let temp = aterm_tempfile::tempdir().unwrap();
    let logins = temp.path().join("logins");
    aterm_uds::private_file::create_private_dir(&logins).unwrap();
    let stage = logins.join("synthetic+1");
    aterm_uds::private_file::create_private_dir(&stage).unwrap();
    let keychain = create_keychain(&stage);
    let program = aterm_tempfile::bin_exe("aterm-machines-dev");
    let program = program.as_path();
    let metadata =
        || std::fs::symlink_metadata(&keychain.0).map(|m| (m.dev(), m.ino(), m.mode(), m.nlink()));
    let before_seed = metadata();
    let seeded =
        test_seed_private_keychain(program, &stage, Instant::now() + Duration::from_secs(60));
    if let Err(error) = seeded {
        let after_seed = metadata();
        let private = aterm_uds::private_file::open_private(
            keychain.0.parent().unwrap(),
            "synthetic.keychain-db",
        )
        .is_ok();
        let read_back =
            read(program, &stage).map(|value| value.is_some_and(|value| value.as_slice() == b"{}"));
        eprintln!(
            "synthetic seed scope: before={before_seed:?} after={after_seed:?} private={private} read={read_back:?}"
        );
        drop(keychain);
        assert!(
            search_state() == before,
            "failed seed changed search preferences"
        );
        panic!("synthetic seed failed: {error}");
    }
    assert!(
        read(program, &stage)
            .unwrap()
            .is_some_and(|value| value.as_slice() == b"{}")
    );
    assert!(
        test_seed_private_keychain(program, &stage, Instant::now() + Duration::from_secs(60))
            .is_err(),
        "seeding cannot alter an existing item"
    );
    let (service, account) = test_vendor_scope(&stage).unwrap();
    let updated = security(&[
        "add-generic-password".as_ref(),
        "-U".as_ref(),
        "-a".as_ref(),
        account.as_ref(),
        "-s".as_ref(),
        service.as_ref(),
        "-X".as_ref(),
        "73796e7468657469632d636c617564652d66697874757265".as_ref(),
        keychain.0.as_os_str(),
    ]);
    assert!(updated.status.success(), "native synthetic update failed");
    let result = read(program, &stage);
    let diagnostic = result.as_ref().err().map(|_| {
        test_private_keychain_diagnostic(program, &stage, Instant::now() + Duration::from_secs(60))
            .unwrap_or_else(|error| error.to_string())
    });
    drop(keychain);
    assert!(
        search_state() == before,
        "synthetic ACL proof changed search preferences"
    );
    let bytes = result.unwrap_or_else(|error| {
        panic!("updated native item could not be read: {error}; query metadata: {diagnostic:?}")
    });
    assert!(bytes.is_some_and(|value| value.as_slice() == b"synthetic-claude-fixture"));
}

#[test]
#[ignore = "explicit native ownership proof; creates only private synthetic keychains"]
fn stage_preparer_resumes_only_its_owned_item_without_replacing_credentials() {
    let before = search_state();
    let temp = aterm_tempfile::tempdir().unwrap();
    let logins = temp.path().join("logins");
    aterm_uds::private_file::create_private_dir(&logins).unwrap();
    let stage = logins.join("owned+1");
    aterm_uds::private_file::create_private_dir(&stage).unwrap();
    let keychain = create_keychain(&stage);
    let program = aterm_tempfile::bin_exe("aterm-machines-dev");
    let program = program.as_path();
    let prepare = |stage: &Path| {
        test_prepare_private_keychain(program, stage, Instant::now() + Duration::from_secs(60))
    };
    let held = aterm_uds::private_file::PrivateLock::take(
        &stage,
        ".aterm-claude-keychain-owner.lock",
        Duration::ZERO,
        "busy",
    )
    .unwrap();
    assert!(prepare(&stage).is_err());
    assert!(!stage.join(".aterm-claude-keychain-owner").exists());
    drop(held);
    if let Err(error) = prepare(&stage) {
        let names: Vec<_> = std::fs::read_dir(&stage)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let marker_len = std::fs::metadata(stage.join(".aterm-claude-keychain-owner"))
            .map(|metadata| metadata.len())
            .ok();
        eprintln!("synthetic prepare refused: entries={names:?} marker_len={marker_len:?}");
        drop(keychain);
        assert!(search_state() == before);
        panic!("owned prepare refused: {error}");
    }
    let marker = std::fs::read(stage.join(".aterm-claude-keychain-owner")).unwrap();
    prepare(&stage).unwrap();
    assert_eq!(
        std::fs::read(stage.join(".aterm-claude-keychain-owner")).unwrap(),
        marker
    );
    let (service, account) = test_vendor_scope(&stage).unwrap();
    let updated = security(&[
        "add-generic-password".as_ref(),
        "-U".as_ref(),
        "-a".as_ref(),
        account.as_ref(),
        "-s".as_ref(),
        service.as_ref(),
        "-X".as_ref(),
        "73796e7468657469632d636c617564652d66697874757265".as_ref(),
        keychain.0.as_os_str(),
    ]);
    assert!(updated.status.success());
    prepare(&stage).unwrap();
    assert!(
        read(program, &stage)
            .unwrap()
            .is_some_and(|v| v.as_slice() == b"synthetic-claude-fixture")
    );
    let mut other = marker.clone();
    *other.last_mut().unwrap() ^= 1;
    std::fs::write(stage.join(".aterm-claude-keychain-owner"), &other).unwrap();
    assert!(prepare(&stage).is_err());
    assert!(
        read(program, &stage)
            .unwrap()
            .is_some_and(|v| v.as_slice() == b"synthetic-claude-fixture")
    );
    let moved = logins.join("moved+1");
    aterm_uds::private_file::create_private_dir(&moved).unwrap();
    aterm_uds::private_file::write_private(&moved, ".aterm-claude-keychain-owner", &marker)
        .unwrap();
    let moved_keychain = create_keychain(&moved);
    assert!(prepare(&moved).is_err());
    drop(moved_keychain);
    drop(keychain);
    assert!(
        search_state() == before,
        "owned preparation changed search preferences"
    );
}

#[test]
#[ignore = "explicit native ownership proof; creates only a private synthetic keychain"]
fn stage_preparer_leaves_an_unowned_native_item_unchanged() {
    let before = search_state();
    let temp = aterm_tempfile::tempdir().unwrap();
    let logins = temp.path().join("logins");
    aterm_uds::private_file::create_private_dir(&logins).unwrap();
    let stage = logins.join("foreign+1");
    aterm_uds::private_file::create_private_dir(&stage).unwrap();
    let keychain = create_keychain(&stage);
    let program = aterm_tempfile::bin_exe("aterm-machines-dev");
    let program = program.as_path();
    let (service, account) = test_vendor_scope(&stage).unwrap();
    let inserted = security(&[
        "add-generic-password".as_ref(),
        "-a".as_ref(),
        account.as_ref(),
        "-s".as_ref(),
        service.as_ref(),
        "-X".as_ref(),
        "666f726569676e".as_ref(),
        keychain.0.as_os_str(),
    ]);
    assert!(inserted.status.success());
    assert!(
        test_prepare_private_keychain(program, &stage, Instant::now() + Duration::from_secs(60))
            .is_err()
    );
    assert!(
        read(program, &stage).is_err(),
        "foreign access policy was not replaced"
    );
    let mut creator = security(&[
        "find-generic-password".as_ref(),
        "-a".as_ref(),
        account.as_ref(),
        "-s".as_ref(),
        service.as_ref(),
        "-w".as_ref(),
        keychain.0.as_os_str(),
    ]);
    let same = creator.status.success() && creator.stdout.trim_ascii_end() == b"foreign";
    aterm_uds::secret::scrub(&mut creator.stdout);
    drop(keychain);
    assert!(same, "foreign data was not replaced");
    assert!(
        search_state() == before,
        "refused preparation changed search preferences"
    );
}
