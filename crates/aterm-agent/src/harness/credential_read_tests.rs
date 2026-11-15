// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A vault under `home` that delivers each of `names`, which this Mac reads in place.
fn applied(home: &std::path::Path, names: &[&str]) {
    let entries: Vec<(&str, &[u8])> = names.iter().map(|name| (*name, &b"fixture"[..])).collect();
    crate::vault_entry::seed(home, &entries);
}

fn resolution<'a>(read: &'a CredentialRead, want: &str) -> &'a Resolution {
    let CredentialRead::Resolved(all) = read else {
        panic!("not resolved: {read:?}");
    };
    &all.iter().find(|(w, _)| w == want).expect(want).1
}

/// FOUND: a want the registry holds an entry for resolves to that entry and its purpose,
/// and nothing else, never a value; nothing is filed for it.
#[test]
fn a_held_want_resolves_to_its_registry_entry() {
    let home = aterm_tempfile::tempdir().unwrap();
    applied(
        home.path(),
        &["release-notary", "gh_access_token", "ai-pool.toml"],
    );
    let read = resolve(home.path(), &["notary".to_owned()]);
    let Resolution::Held(entries) = resolution(&read, "notary") else {
        panic!("{read:?}");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "release-notary");
    assert!(!format!("{read:?}").contains("digest"));
    assert!(
        !home
            .path()
            .join(".config/aterm/secrets/needs.toml")
            .exists()
    );
}

/// NEGATIVE CONTROL: a want the registry holds no entry for, on a Mac that applied a registry
/// or none, is never held: it is filed in the need ledger, or says why it could not be.
#[test]
fn a_missing_want_is_filed_never_held() {
    let home = aterm_tempfile::tempdir().unwrap();
    let read = resolve(home.path(), &["gh:release".to_owned()]);
    assert!(
        !matches!(resolution(&read, "gh:release"), Resolution::Held(_)),
        "{read:?}"
    );
    applied(home.path(), &["gh_access_token"]);
    let read = resolve(
        home.path(),
        &["gh:release".to_owned(), "nonsense".to_owned()],
    );
    assert!(
        !matches!(resolution(&read, "gh:release"), Resolution::Held(_)),
        "{read:?}"
    );
    assert!(
        matches!(resolution(&read, "nonsense"), Resolution::Failed(_)),
        "{read:?}"
    );
}
