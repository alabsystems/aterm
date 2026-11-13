// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn line(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// A private write passes none of fleet_db's gates, so it is refused on the fleet tables; a read
/// stays, the only one of a rekey's kept folder, under the session rule; and no private `init`
/// makes a dataset that says it holds them.
/// NEGATIVE CONTROL: with `run` skipping the refusal, the `put` goes through:
/// `` called `Result::unwrap_err()` on an `Ok` value ``.
#[cfg(unix)]
#[test]
fn a_private_verb_does_not_write_the_fleet_tables_and_its_read_is_a_reveal() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let fleet = tmp.path().join("fleet");
    local::ensure_fleet(&fleet, &[0x5a; 32]).unwrap();
    let d = fleet.to_str().unwrap();
    let request = "11".repeat(16);
    for words in [
        vec![
            "put",
            "--dir",
            d,
            "--collection",
            "computers",
            "--key",
            "m1",
            "--value",
            "{}",
            "--request",
            &request,
        ],
        vec![
            "revert",
            "--dir",
            d,
            "--operation",
            &request,
            "--request",
            &request,
        ],
        vec![
            "import-csv",
            "--dir",
            d,
            "--file",
            "/nonexistent",
            "--request",
            &request,
        ],
    ] {
        let refused = run(&line(&words)).unwrap_err();
        assert!(refused.to_string().contains("fleet tables"), "{refused}");
    }
    for verb in [
        "stop",
        "budget-create",
        "spend",
        "machine-name",
        "revoke-writes",
    ] {
        let refused = run(&line(&[verb, "--dir", d])).unwrap_err();
        assert!(
            refused.to_string().contains("fleet tables"),
            "{verb}: {refused}"
        );
    }
    let scan = ["scan", "--dir", d, "--collection", "emails"];
    assert!(run(&line(&scan)).is_ok());
    assert!(fleet_db_reveals(&line(&scan)).unwrap());
    assert!(
        fleet_db_reveals(&line(&[
            "get",
            "--dir",
            d,
            "--collection",
            "c",
            "--key",
            "k"
        ]))
        .unwrap()
    );

    let other = tmp.path().join("other");
    local::init(&other, "accounts").unwrap();
    let o = other.to_str().unwrap();
    assert!(!fleet_db_reveals(&line(&["scan", "--dir", o, "--collection", "c"])).unwrap());

    let reserved = tmp.path().join("reserved");
    let r = reserved.to_str().unwrap();
    assert!(run(&line(&["init", "--dir", r, "--namespace", "alab"])).is_err());
    assert!(!reserved.join("database.json").exists());
}

/// A folder no verb can open says why, to a write and to a read: no plaintext is at stake, and
/// its file is not what is wrong. The fleet tables stay unwritable behind any of them.
/// NEGATIVE CONTROL: with `holds_fleet_tables` starting at its file, each write names the file:
/// `` existing database state is not a private regular file ``.
#[cfg(unix)]
#[test]
fn a_folder_no_verb_can_open_says_why() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let tmp = aterm_tempfile::tempdir().unwrap();
    let open = tmp.path().join("open");
    local::init(&open, "accounts").unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    let healthy = tmp.path().join("healthy");
    local::init(&healthy, "accounts").unwrap();
    let link = tmp.path().join("link");
    symlink(&healthy, &link).unwrap();
    let plain = tmp.path().join("plain");
    aterm_uds::private_file::write_private(tmp.path(), "plain", b"x").unwrap();
    let fleet = tmp.path().join("fleet");
    local::ensure_fleet(&fleet, &[0x5a; 32]).unwrap();
    std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).unwrap();
    for (dir, says) in [
        (&open, "open to other users"),
        (&link, "symlink"),
        (&plain, "no directory"),
        (&fleet, "open to other users"),
    ] {
        let d = dir.to_str().unwrap();
        let refused = run(&line(&["stop", "--dir", d])).unwrap_err().to_string();
        assert!(refused.contains(says), "{d}: {refused}");
        let scan = ["scan", "--dir", d, "--collection", "c"];
        let refused = fleet_db_reveals(&line(&scan)).unwrap_err().to_string();
        assert!(refused.contains(says), "{d}: {refused}");
    }
    let gone = tmp.path().join("gone");
    let scan = ["scan", "--dir", gone.to_str().unwrap(), "--collection", "c"];
    assert!(!fleet_db_reveals(&line(&scan)).unwrap());
}

/// A neighbour holding the writer lock changes neither what a folder is nor its answer: another
/// dataset's read stays a retryable busy, and the fleet tables' stay refused.
/// NEGATIVE CONTROL: with `holds_fleet_tables` taking the lock, the other dataset reads as a
/// reveal: `` assertion failed: !fleet_db_reveals(&line(&scan)) ``.
#[cfg(unix)]
#[test]
fn a_private_verb_beside_a_writer_reads_its_folder_rightly() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let other = tmp.path().join("other");
    local::init(&other, "accounts").unwrap();
    let o = other.to_str().unwrap();
    let scan = ["scan", "--dir", o, "--collection", "c"];
    let neighbour = Database::open(&other).unwrap();
    assert!(!fleet_db_reveals(&line(&scan)).unwrap());
    assert!(run(&line(&scan)).unwrap_err().is_busy());
    drop(neighbour);

    let fleet = tmp.path().join("fleet");
    local::ensure_fleet(&fleet, &[0x5a; 32]).unwrap();
    let d = fleet.to_str().unwrap();
    let neighbour = Database::open(&fleet).unwrap();
    assert!(fleet_db_reveals(&line(&["scan", "--dir", d, "--collection", "emails"])).unwrap());
    let request = "11".repeat(16);
    let refused = run(&line(&[
        "put",
        "--dir",
        d,
        "--collection",
        "c",
        "--key",
        "k",
        "--value",
        "{}",
        "--request",
        &request,
    ]))
    .unwrap_err();
    assert!(refused.to_string().contains("fleet tables"), "{refused}");
    drop(neighbour);
}
