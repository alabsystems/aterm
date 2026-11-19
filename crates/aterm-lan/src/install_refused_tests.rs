// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::lan::install::{CodeDirectories, test_image};

const ARM: u32 = 0x0100_000c;

/// A scratch Mac: a bundle's executable of `build`, and where root's record goes.
struct Scratch {
    _dir: aterm_tempfile::TempDir,
    source: PathBuf,
    code: PathBuf,
    record: PathBuf,
}

fn scratch(build: &[u8]) -> Scratch {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("aterm.app");
    let code = source.join("Contents/MacOS/aterm");
    std::fs::create_dir_all(code.parent().unwrap()).unwrap();
    std::fs::write(&code, test_image(ARM, &[build])).unwrap();
    let record = dir.path().join("lan-broker.refused");
    Scratch {
        source,
        code,
        record,
        _dir: dir,
    }
}

fn key_of(source: &Path, build: &[u8]) -> String {
    refused::broker_key(
        source,
        &CodeDirectories::of_bytes(&test_image(ARM, &[build])).unwrap(),
    )
    .unwrap()
}

/// The broker that serves takes out its own install's lines, the definite, the pending and the
/// stalled one, and keeps another bundle's and another build's; the record goes once nothing is
/// left. NEGATIVE CONTROLS: no record, and a record of other keys alone, are left as they stand.
/// Run: taking out the whole record loses the other bundle's line.
#[test]
fn a_serving_broker_takes_out_only_its_own_installs_lines() {
    let mac = scratch(b"this build");
    let mine = key_of(&mac.source, b"this build");
    let other_build = key_of(&mac.source, b"that build");
    let other_bundle = key_of(Path::new("/Users//a/Applications/aterm.app"), b"this build");
    assert!(!served(&mac.record, &mac.source, &mac.code).unwrap());
    assert!(!mac.record.exists());

    let others = format!("{other_build}\n{other_bundle}\n");
    std::fs::write(&mac.record, &others).unwrap();
    assert!(!served(&mac.record, &mac.source, &mac.code).unwrap());
    assert_eq!(std::fs::read_to_string(&mac.record).unwrap(), others);

    std::fs::write(
        &mac.record,
        format!("{mine}\nno-verdict 1 b1 {mine}\nstalled b1 {mine}\n{others}"),
    )
    .unwrap();
    assert!(served(&mac.record, &mac.source, &mac.code).unwrap());
    let kept: BTreeSet<String> = std::fs::read_to_string(&mac.record)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(kept, BTreeSet::from([other_build, other_bundle]));

    std::fs::write(&mac.record, format!("stalled b1 {mine}\n")).unwrap();
    assert!(served(&mac.record, &mac.source, &mac.code).unwrap());
    assert!(!mac.record.exists(), "a record of nothing goes");
}

/// A record the broker did not write is no record it changes: a link, a file another account
/// may write, and one larger than a record are refused and left as they stand.
#[test]
fn a_record_the_broker_does_not_own_is_left_as_it_stands() {
    use std::os::unix::fs::PermissionsExt as _;
    let mac = scratch(b"this build");
    let mine = key_of(&mac.source, b"this build");
    let elsewhere = mac.record.with_file_name("elsewhere");
    std::fs::write(&elsewhere, format!("{mine}\n")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &mac.record).unwrap();
    assert!(served(&mac.record, &mac.source, &mac.code).is_err());
    assert_eq!(
        std::fs::read_to_string(&elsewhere).unwrap(),
        format!("{mine}\n")
    );
    std::fs::remove_file(&mac.record).unwrap();

    std::fs::write(&mac.record, format!("{mine}\n")).unwrap();
    std::fs::set_permissions(&mac.record, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(served(&mac.record, &mac.source, &mac.code).is_err());
    assert_eq!(
        std::fs::read_to_string(&mac.record).unwrap(),
        format!("{mine}\n")
    );

    std::fs::set_permissions(&mac.record, std::fs::Permissions::from_mode(0o644)).unwrap();
    let big = format!("{mine}\n{}\n", "x".repeat(MAX));
    std::fs::write(&mac.record, &big).unwrap();
    assert!(served(&mac.record, &mac.source, &mac.code).is_err());
    assert_eq!(std::fs::read_to_string(&mac.record).unwrap(), big);
}

/// A success that lands while the broker takes out its install's lines (root's own readiness
/// hello is usually the broker's first) waits for the broker's change under the record's lock,
/// then removes the record: the other build's line never outlives the success. NEGATIVE CONTROL
/// (run): a broker that changes the record without its lock lets the removal in between, then
/// writes the other build's line back.
#[test]
fn a_success_while_the_broker_takes_out_its_lines_is_not_undone() {
    use aterm_uds::file_lock::LockFile;
    let mac = scratch(b"this build");
    let mine = key_of(&mac.source, b"this build");
    let other = key_of(&mac.source, b"that build");
    std::fs::write(&mac.record, format!("no-verdict 1 b1 {mine}\n{other}\n")).unwrap();
    // Root's apply succeeds between the broker's reading and its change: it removes the record
    // where the lock is free now, else once the lock comes free, as root's own removal does.
    let mut waited = false;
    let lock = refused::lock_of(&mac.record);
    assert!(
        served_between(&mac.record, &mac.source, &mac.code, || {
            match LockFile::try_take(&lock).unwrap() {
                Some(_held) => std::fs::remove_file(&mac.record).unwrap(),
                None => waited = true,
            }
        })
        .unwrap()
    );
    if waited {
        let _held = refused::hold(&mac.record).unwrap();
        std::fs::remove_file(&mac.record).unwrap();
    }
    assert!(!mac.record.exists(), "the success's removal stands");
}

/// A record removed between the broker's reading and its removal, by a root of a build before
/// the lock, is removed: no failure is logged for it. NEGATIVE CONTROL (run): a removal that
/// reads not-found as an error fails the broker's change.
#[test]
fn a_record_already_gone_when_the_broker_removes_it_is_removed() {
    let mac = scratch(b"this build");
    let mine = key_of(&mac.source, b"this build");
    std::fs::write(&mac.record, format!("{mine}\n")).unwrap();
    let removed = served_between(&mac.record, &mac.source, &mac.code, || {
        std::fs::remove_file(&mac.record).unwrap();
    });
    assert!(matches!(removed, Ok(true)), "{removed:?}");
    assert!(!mac.record.exists());
}

/// The broker reads its install's key before it takes the record's lock, so root, waiting for
/// the lock, waits on no reading of the bundle's code: with the lock held by root, a code that
/// cannot be read is refused at once, and the record is left as it stands. NEGATIVE CONTROL
/// (run): a broker that reads the code under the lock waits out the lock and is refused for it.
#[test]
fn the_broker_reads_its_key_before_it_takes_the_lock() {
    use aterm_uds::file_lock::LockFile;
    let mac = scratch(b"this build");
    let mine = key_of(&mac.source, b"this build");
    std::fs::write(&mac.record, format!("{mine}\n")).unwrap();
    let _root = LockFile::try_take(&refused::lock_of(&mac.record))
        .unwrap()
        .expect("the lock is free");
    let gone = mac.source.join("Contents/MacOS/gone");
    let error = served(&mac.record, &mac.source, &gone).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
    assert_eq!(
        std::fs::read_to_string(&mac.record).unwrap(),
        format!("{mine}\n")
    );
}
