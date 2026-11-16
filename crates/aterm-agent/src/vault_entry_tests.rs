// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn no_vault_reads_every_entry_absent() {
    let root = aterm_tempfile::tempdir().unwrap();
    assert_eq!(read_text(root.path(), "ai-pool.toml", 64), Read::Absent);
    assert_eq!(
        read_set(root.path(), &["ai-pool.toml", "ai-accounts.csv"], 64),
        [Read::Absent, Read::Absent]
    );
    assert!(!configured(root.path()));
    assert!(names(root.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn a_file_under_the_home_never_stands_for_an_entry() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    seed(home, &[("fleet-accounts.toml", b"[[account]]\n")]);
    std::fs::create_dir_all(home.join(".secrets")).unwrap();
    std::fs::write(home.join(".secrets/ai-pool.toml"), "hand placed\n").unwrap();
    std::fs::write(home.join(".secrets/fleet-accounts.toml"), "hand edited\n").unwrap();
    assert!(configured(home));
    assert_eq!(
        read_set(home, &["ai-pool.toml", "fleet-accounts.toml"], 1024),
        vec![Read::Absent, Read::Present("[[account]]\n".to_owned())]
    );
}

#[cfg(unix)]
#[test]
fn an_unusable_entry_holds_its_place() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    seed(
        home,
        &[
            ("ai-pool.toml", b"pool = 'over the limit'"),
            ("fleet-accounts.toml", &[0xff, 0xfe]),
            ("ai-accounts.csv", b"0123456789"),
        ],
    );
    let reads = read_set(
        home,
        &["ai-pool.toml", "fleet-accounts.toml", "ai-accounts.csv"],
        4,
    );
    assert!(
        reads
            .iter()
            .all(|read| matches!(read, Read::Unavailable(_)) && read.occupies()),
        "{reads:?}"
    );
}

#[cfg(unix)]
#[test]
fn the_digest_moves_with_a_value_and_hides_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    seed(home, &[("ai-mailbox.toml", b"user = 'a@example.test'\n")]);
    let names = ["ai-mailbox.toml", "mailbox.toml"];
    let first = digest(home, &names, 1024);
    assert_eq!(first, digest(home, &names, 1024));
    assert_eq!(changed_at("test", home, &names, 1024), None);
    seed(home, &[("ai-mailbox.toml", b"user = 'b@example.test'\n")]);
    assert_ne!(first, digest(home, &names, 1024));
    assert!(changed_at("test", home, &names, 1024).is_some());
    let mut unkeyed = aterm_digest::Sha256::new();
    unkeyed.update(b"user = 'a@example.test'\n");
    assert_ne!(first, unkeyed.finalize());
}

/// An entry that joins the names a reader watches is a change, not a first read. NEGATIVE
/// CONTROL: another reader of the same home starts from its own first read.
#[cfg(unix)]
#[test]
fn an_entry_that_joins_the_watched_names_is_a_change() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path();
    seed(home, &[("ai-mailbox.toml", b"user = 'a@example.test'\n")]);
    assert_eq!(changed_at("joins", home, &["ai-mailbox.toml"], 1024), None);
    seed(home, &[("m27_password", b"pw\n")]);
    let names = ["ai-mailbox.toml", "m27_password"];
    assert!(changed_at("joins", home, &names, 1024).is_some());
    assert_eq!(changed_at("joins-other", home, &names, 1024), None);
}

#[test]
fn a_source_spelling_names_an_entry() {
    assert_eq!(source_entry("vault:ai-pool.toml"), Some("ai-pool.toml"));
    assert_eq!(source_entry("vault:"), None);
    assert_eq!(source_entry("~/.secrets/ai-pool.toml"), None);
    assert_eq!(source_name("ai-pool.toml"), "vault:ai-pool.toml");
}
