// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::macos_setup::spotlight_scope::Entry;
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

/// A Mac's record files held in memory: each path's bytes and the uid that owns it.
#[derive(Default)]
struct Files(BTreeMap<PathBuf, (u32, Vec<u8>)>);

impl Files {
    fn with(mut self, path: &str, uid: u32, bytes: &str) -> Self {
        self.0
            .insert(PathBuf::from(path), (uid, bytes.as_bytes().to_vec()));
        self
    }
}

impl Reads for Files {
    fn macos(&self) -> bool {
        true
    }

    fn euid(&self) -> u32 {
        501
    }

    fn home(&self) -> Option<PathBuf> {
        None
    }

    fn entry(&self, path: &Path) -> io::Result<Entry> {
        let (uid, _) = self.0.get(path).ok_or(io::ErrorKind::NotFound)?;
        Ok(Entry {
            uid: *uid,
            dir: false,
            symlink: false,
        })
    }

    fn names(&self, _: &Path) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn root_file(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        match self.0.get(path) {
            Some((0, bytes)) if bytes.len() <= max => Ok(bytes.clone()),
            Some(_) => Err(io::ErrorKind::PermissionDenied.into()),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }

    fn volume_status(&self) -> Result<Vec<u8>, String> {
        Err("not read".into())
    }
}

/// A record stands for a key root's own file lists, and for every key once it lists [`KEYS`].
/// NEGATIVE CONTROLS: no record, a record listing other keys, a key that is only part of a
/// listed line, a file root does not own, and inputs that could not be read (no key) each
/// stand for nothing, so the step is asked for.
#[test]
fn a_record_stands_only_as_roots_file_listing_this_key() {
    let record = |uid: u32, text: &str| Files::default().with(DEVELOPER_MODE, uid, text);
    let key = |key: Option<&str>| {
        let key = key.map(str::to_owned);
        move || key
    };
    assert!(stands(
        &record(0, "off\n"),
        DEVELOPER_MODE,
        key(Some("off"))
    ));
    assert!(stands(
        &record(0, "member 502\noff\n"),
        DEVELOPER_MODE,
        key(Some("off"))
    ));
    for (why, files, listed) in [
        ("no record", Files::default(), Some("off")),
        ("another key", record(0, "member 501\n"), Some("off")),
        (
            "part of a line",
            record(0, "member 5011\n"),
            Some("member 501"),
        ),
        ("not root's file", record(501, "off\n"), Some("off")),
        ("no key", record(0, "off\n"), None),
        (
            "another step's record",
            Files::default().with(LAN_BROKER, 0, "off\n"),
            Some("off"),
        ),
    ] {
        assert!(!stands(&files, DEVELOPER_MODE, key(listed)), "{why}");
    }
    // No record: the step's inputs are not read at all.
    assert!(!stands(&Files::default(), LAN_BROKER, || unreachable!(
        "a key read with no record"
    )));

    let keys = |count: usize| -> String { (0..count).map(|n| format!("build {n}\n")).collect() };
    let full = Files::default().with(LAN_BROKER, 0, &keys(KEYS));
    assert!(stands(&full, LAN_BROKER, key(Some("a build never tried"))));
    assert!(stands(&full, LAN_BROKER, key(None)));
    let short = Files::default().with(LAN_BROKER, 0, &keys(KEYS - 1));
    assert!(!stands(
        &short,
        LAN_BROKER,
        key(Some("a build never tried"))
    ));
    assert!(stands(&short, LAN_BROKER, key(Some("build 0"))));
}

/// Developer mode's key names the reading that failed: `off` for any account, or the account
/// whose authorization is missing.
#[test]
fn the_developer_mode_key_names_the_reading() {
    assert_eq!(developer_mode_key(false, 501), "off");
    assert_eq!(developer_mode_key(false, 502), "off");
    assert_eq!(developer_mode_key(true, 501), "member 501");
    assert_ne!(developer_mode_key(true, 501), developer_mode_key(true, 502));
}

/// The broker's key names the build and the bundle it was installed from.
/// NEGATIVE CONTROLS: one changed code byte at the same length, one more byte, another CPU
/// type, the same bytes split over two directories, and another bundle path each give another
/// key; a path with a line break gives none.
#[cfg(unix)]
#[test]
fn the_broker_key_names_the_build_and_the_bundle() {
    use aterm_uds::lan::install::{CodeDirectories, test_image};
    let code = |cpu: u32, directories: &[&[u8]]| {
        CodeDirectories::of_bytes(&test_image(cpu, directories)).unwrap()
    };
    let arm = 0x0100_000c;
    let app = Path::new("/Applications/aterm.app");
    let key = broker_key(app, &code(arm, &[b"this build"])).unwrap();
    assert_eq!(
        broker_key(app, &code(arm, &[b"this build"])),
        Some(key.clone())
    );
    assert!(key.ends_with(" /Applications/aterm.app"), "{key}");
    assert_eq!(key.len(), 32 + " /Applications/aterm.app".len(), "{key}");
    for (why, other) in [
        (
            "one changed byte",
            broker_key(app, &code(arm, &[b"that build"])),
        ),
        (
            "one more byte",
            broker_key(app, &code(arm, &[b"this build."])),
        ),
        (
            "another CPU",
            broker_key(app, &code(0x0100_0007, &[b"this build"])),
        ),
        (
            "two directories",
            broker_key(app, &code(arm, &[b"this ", b"build"])),
        ),
        (
            "another bundle",
            broker_key(
                Path::new("/Users//a/Applications/aterm.app"),
                &code(arm, &[b"this build"]),
            ),
        ),
    ] {
        assert!(other.is_some_and(|other| other != key), "{why}");
    }
    assert_eq!(
        broker_key(
            Path::new("/Users//a\n/aterm.app"),
            &code(arm, &[b"this build"])
        ),
        None
    );
}
