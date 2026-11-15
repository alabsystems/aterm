// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A vault whose fleet cannot be judged (no Commons here) takes no registry value: the store's
/// admission and its put are refused, naming why, and nothing is put.
/// NEGATIVE CONTROL: with `Vault::put` calling `put_entry` under `InPlace::for_test` instead of
/// the fleet's judgment, the put lands: `` assertion failed: refused.is_err() ``.
#[test]
fn a_vault_whose_fleet_is_not_judged_takes_no_registry_value() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("admin"));
    let source = root.path().join("test.vault");
    client::create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let store = Vault::at(paths.clone());
    let own = crate::typed::secret::test_entry(("address", "password", "ad-x"), 1);
    let error = store.admit().unwrap_err().to_string();
    assert!(error.contains("no registry secret is written"), "{error}");
    let refused = store.put(&own, &Secret::new("registry-value-1".into()));
    assert!(refused.is_err());
    assert_eq!(client::read_in_place(&paths, &own).unwrap(), None);
}

#[test]
/// The store reads, puts and removes its own entries in place, and reaches no other credential.
fn the_registry_store_reaches_only_registry_entries() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("admin"));
    let source = root.path().join("test.vault");
    client::create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let mut admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        "gh_access_token",
        b"api-key-value",
    )
    .unwrap();
    admin.resolving(&aterm_secrets::text::Audience::Everyone, "gh_access_token");
    client::admin_publish(&paths, &admin, &doc).unwrap();
    client::sync(&paths, None).unwrap();
    let store = Vault::admitted(paths.clone()).unwrap();
    // Negative controls: a credential that is no registry cell's is never read, replaced or
    // removed through the registry's store.
    assert!(store.get("gh_access_token").is_err());
    assert!(
        store
            .put("gh_access_token", &Secret::new("replaced-1".into()))
            .is_err()
    );
    assert!(store.remove("gh_access_token").is_err());
    assert_eq!(
        client::read_in_place(&paths, "gh_access_token")
            .unwrap()
            .unwrap()
            .as_slice(),
        b"api-key-value"
    );
    let own = crate::typed::secret::entry_name("address", "password", "ad-x").unwrap();
    store
        .put(&own, &Secret::new("registry-value-1".into()))
        .unwrap();
    assert!(
        store
            .get(&own)
            .unwrap()
            .unwrap()
            .same(&Secret::new("registry-value-1".into()))
    );
    let second = crate::typed::secret::test_entry(("address", "password", "ad-x"), 2);
    store
        .put(&second, &Secret::new("registry-value-2".into()))
        .unwrap();
    store.remove(&own).unwrap();
    assert!(store.get(&own).unwrap().is_none());
}

/// The vault's owed record is durable (a new handle on the same vault reads it), names its owning
/// run and takes only registry entries: an entry this live process owes is never orphaned, one
/// whose owner is gone, is another process now, or cannot be read is. A run's `paid` clears only
/// its own records and a sweep's `cleared` only ended runs', so another live run's record stays.
/// NEGATIVE CONTROLS: a credential's name is never owed, so no sweep could remove it; with
/// `live` always false, `` a live owner's entry is not orphaned `` fails; with `paid` clearing
/// ended runs' records too, `` a run pays only its own records `` fails; with `paid` clearing
/// every record, `` another live run's record stays `` fails.
#[test]
fn the_owed_record_names_its_run_and_takes_only_registry_entries() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("admin"));
    let source = root.path().join("test.vault");
    client::create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let mine = crate::typed::secret::entry_name("address", "password", "ad-x").unwrap();
    let gone = crate::typed::secret::test_entry(("address", "password", "ad-x"), 2);
    let reused = crate::typed::secret::test_entry(("address", "password", "ad-x"), 3);
    let garbled = crate::typed::secret::test_entry(("address", "password", "ad-x"), 4);
    let vault = Vault::at(paths.clone());
    vault
        .owe(&[mine.clone(), gone.clone(), reused.clone(), garbled.clone()])
        .unwrap();
    assert!(vault.owe(&["gh_access_token".to_owned()]).is_err());
    let dir = vault.owed_dir();
    std::fs::write(dir.join(&gone), "2147483646 1").unwrap();
    std::fs::write(dir.join(&reused), format!("{} 1", std::process::id())).unwrap();
    std::fs::write(dir.join(&garbled), "not an owner").unwrap();
    let again = Vault::at(paths.clone());
    assert_eq!(
        again.orphaned().unwrap(),
        BTreeSet::from([gone.clone(), reused.clone(), garbled.clone()]),
        "a live owner's entry is not orphaned"
    );
    // A run pays only its own records; an ended run's go only by a sweep's clearing.
    again
        .paid(&BTreeSet::from([gone.clone(), reused.clone()]))
        .unwrap();
    assert_eq!(
        again.orphaned().unwrap(),
        BTreeSet::from([gone.clone(), reused.clone(), garbled.clone()]),
        "a run pays only its own records"
    );
    again
        .cleared(&BTreeSet::from([gone, reused, mine.clone()]))
        .unwrap();
    assert_eq!(
        Vault::at(paths.clone()).orphaned().unwrap(),
        BTreeSet::from([garbled])
    );
    assert!(dir.join(&mine).exists());
    assert!(dir.join(&mine).exists());

    // Another live run (this test's parent) that owes an entry keeps its record when this run
    // pays it: it pays its own debt.
    let parent = owner(std::os::unix::process::parent_id()).unwrap();
    std::fs::write(dir.join(&mine), &parent).unwrap();
    Vault::at(paths.clone())
        .paid(&BTreeSet::from([mine.clone()]))
        .unwrap();
    assert!(dir.join(&mine).exists(), "another live run's record stays");
    std::fs::write(dir.join(&mine), owner(std::process::id()).unwrap()).unwrap();
    Vault::at(paths.clone())
        .paid(&BTreeSet::from([mine.clone()]))
        .unwrap();
    assert!(!dir.join(&mine).exists());
}

/// A put the vault has not posted is never a stored one: `EntryWrite::Pending` (signed on this
/// Mac, or a draft its vault pass may still drop) is refused, naming the entry and never a
/// value, and only `EntryWrite::Here` counts.
/// NEGATIVE CONTROL: with `stored` taking `Pending` as `Ok`, the pending put passes:
/// `` assertion failed: pending.is_err() ``.
#[test]
fn a_put_not_yet_in_the_vault_is_never_stored() {
    let own = crate::typed::secret::test_entry(("address", "password", "ad-x"), 1);
    assert!(stored(&own, client::EntryWrite::Here).is_ok());
    let pending = stored(&own, client::EntryWrite::Pending);
    assert!(pending.is_err());
    let error = pending.unwrap_err().to_string();
    assert!(
        error.contains("not in the vault yet") && error.contains(&own),
        "{error}"
    );
}
