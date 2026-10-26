// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keep seam (`ATERM_DEBUG_SEAMLESS_KEEP`, [`super::keep_attempt_in`]): what an outgoing
//! process copies of its handoff's files for `lab seamless`'s privacy leg, and how it lists them.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use super::{keep_attempt_in, keep_root_of};

const PID: u32 = 4242;
const NONCE: &str = "00ab";

fn write(dir: &Path, name: &str, bytes: &[u8], mode: u32) {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("a file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("its mode");
}

fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("stat")
        .permissions()
        .mode()
        & 0o7777
}

/// A control directory as an attempt leaves it: its manifest, a control sidecar and a grid (the
/// grid made 0644 here, so a row that reads the copy's mode is told apart), beside another
/// attempt's manifest, another process's, a file of no attempt, and a link named as the
/// attempt's layout.
struct Rig {
    _tmp: aterm_tempfile::TempDir,
    dir: std::path::PathBuf,
    root: std::path::PathBuf,
    target: std::path::PathBuf,
}

fn rig() -> Rig {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let dir = tmp.path().join("run");
    std::fs::create_dir(&dir).expect("the control dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("0700");
    let stem = format!("seamless-{PID}-{NONCE}");
    write(&dir, &format!("{stem}.toml"), b"00ab\nschema = 1\n", 0o600);
    write(&dir, &format!("{stem}.s1.ctl"), b"{\"version\":1}", 0o600);
    write(&dir, &format!("{stem}.s1.grid"), b"\x00\x00\x00\x00", 0o644);
    write(
        &dir,
        &format!("seamless-{PID}-00cd.toml"),
        b"other attempt",
        0o600,
    );
    write(
        &dir,
        &format!("seamless-{}-{NONCE}.toml", PID + 1),
        b"other process",
        0o600,
    );
    write(&dir, "latest", b"no attempt", 0o600);
    let target = tmp.path().join("elsewhere.toml");
    std::fs::write(&target, b"not the attempt's").expect("a file");
    std::os::unix::fs::symlink(&target, dir.join(format!("{stem}.layout.toml"))).expect("a link");
    let root = tmp.path().join("keep");
    Rig {
        dir,
        root,
        target,
        _tmp: tmp,
    }
}

fn kept(rig: &Rig) -> std::path::PathBuf {
    rig.root.join(format!("{PID}-{NONCE}"))
}

fn index(rig: &Rig) -> Vec<String> {
    std::fs::read_to_string(kept(rig).join("index"))
        .expect("the index")
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_keep_seam_copies_only_its_attempt_byte_for_byte_and_private() {
    let rig = rig();
    assert_eq!(
        keep_attempt_in(&rig.root, &rig.dir, PID, NONCE).ok(),
        Some(3)
    );
    let mut names: Vec<String> = std::fs::read_dir(kept(&rig))
        .expect("the kept folder")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "index",
            "seamless-4242-00ab.s1.ctl",
            "seamless-4242-00ab.s1.grid",
            "seamless-4242-00ab.toml",
        ],
        "the attempt's own files and the index, no other attempt's, process's or name's"
    );
    for name in &names[1..] {
        assert_eq!(
            std::fs::read(kept(&rig).join(name)).expect("the copy"),
            std::fs::read(rig.dir.join(name)).expect("the original"),
            "{name}"
        );
        assert_eq!(mode_of(&kept(&rig).join(name)), 0o600, "{name}'s copy");
    }
    assert_eq!(mode_of(&kept(&rig)), 0o700);
    assert_eq!(mode_of(&kept(&rig).join("index")), 0o600);
}

#[test]
fn the_index_says_each_originals_own_mode_and_the_directorys() {
    let rig = rig();
    keep_attempt_in(&rig.root, &rig.dir, PID, NONCE).expect("kept");
    let rows = index(&rig);
    assert_eq!(rows[0], format!("dir 700 {}", rig.dir.display()));
    assert!(
        rows.contains(&"file 600 16 seamless-4242-00ab.toml".to_string()),
        "{rows:?}"
    );
    assert!(
        rows.contains(&"file 600 13 seamless-4242-00ab.s1.ctl".to_string()),
        "{rows:?}"
    );
    assert!(
        rows.contains(&"file 644 4 seamless-4242-00ab.s1.grid".to_string()),
        "the original's 0644, not the copy's 0600: {rows:?}"
    );
}

#[test]
fn a_link_named_as_the_attempts_file_is_listed_and_never_followed() {
    let rig = rig();
    keep_attempt_in(&rig.root, &rig.dir, PID, NONCE).expect("kept");
    assert!(
        index(&rig).contains(&"skip seamless-4242-00ab.layout.toml".to_string()),
        "{:?}",
        index(&rig)
    );
    assert!(!kept(&rig).join("seamless-4242-00ab.layout.toml").exists());
    assert!(rig.target.exists(), "the link's target is untouched");
}

#[test]
fn a_second_keep_adds_only_what_the_attempt_wrote_since() {
    let rig = rig();
    keep_attempt_in(&rig.root, &rig.dir, PID, NONCE).expect("kept");
    let first = index(&rig);
    let person = rig.dir.join(format!("seamless-{PID}-{NONCE}.person"));
    assert!(super::write_person_stamps(
        &person,
        &[(1, super::PersonStamp::Keyed(1200))]
    ));
    assert_eq!(
        keep_attempt_in(&rig.root, &rig.dir, PID, NONCE).ok(),
        Some(1)
    );
    let second = index(&rig);
    assert_eq!(
        second[..first.len()],
        first[..],
        "the rows already written stand"
    );
    assert_eq!(
        second[first.len()..],
        ["file 600 7 seamless-4242-00ab.person".to_string()],
        "the stamps the Commit wrote, once; the directory is named once"
    );
    assert_eq!(
        keep_attempt_in(&rig.root, &rig.dir, PID, NONCE).ok(),
        Some(0)
    );
    assert_eq!(index(&rig), second);
}

#[test]
fn the_seam_names_only_an_absolute_folder() {
    assert_eq!(keep_root_of(None), None);
    assert_eq!(keep_root_of(Some("".into())), None);
    assert_eq!(keep_root_of(Some("keep/here".into())), None);
    assert_eq!(
        keep_root_of(Some("/private/tmp/x/keep".into())),
        Some(std::path::PathBuf::from("/private/tmp/x/keep"))
    );
}
