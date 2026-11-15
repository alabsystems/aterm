// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_principal_prints_its_names_and_never_a_value() {
    let principal = Principal::new(vec![(
        "AZURE_CLIENT_SECRET".into(),
        "sentinel-never-print".into(),
    )]);
    let shown = format!("{principal:?}");
    assert_eq!(shown, r#"["AZURE_CLIENT_SECRET"]"#);
}

#[cfg(unix)]
#[test]
fn a_host_that_applied_no_vault_says_so_and_files_nothing() {
    let home = aterm_tempfile::tempdir().expect("scratch home");
    let paths = aterm_secrets::client::Paths::locate_for_home(home.path());
    let why = from_vault_in(&paths, Miss::File).unwrap_err();
    assert_eq!(why, aterm_secrets::registry::NO_REGISTRY);
    assert!(!paths.dir.exists(), "no ledger was written");
}

#[cfg(unix)]
#[test]
fn the_entry_is_read_in_place_from_the_store_as_the_three_variables() {
    use aterm_secrets::client;
    use aterm_secrets::need::catalog::AZURE_SIGNING_PRINCIPAL_ENTRY as ENTRY;
    use aterm_secrets::text::Audience;
    let root = aterm_tempfile::tempdir().expect("scratch root");
    let home = root.path().join("host");
    let paths = client::Paths::for_home(&home);
    client::create(
        &paths,
        root.path().join("fixture.vault").to_str().expect("utf-8"),
        None,
        Some("host"),
    )
    .expect("vault");
    assert!(
        held(&paths, ENTRY)
            .unwrap_err()
            .ends_with("is not on this host yet")
    );
    let publish = |value: &str| {
        let admin = client::admin_open(&paths).expect("admin");
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, ENTRY, value.as_bytes())
            .expect("entry");
        client::admin_publish(&paths, &admin, &doc).expect("publish");
    };
    publish("AZURE_TENANT_ID=t\nAZURE_CLIENT_ID=c\nAZURE_CLIENT_SECRET=s\n");
    let backup = home.join(".secrets").join(ENTRY);
    if backup.exists() {
        std::fs::write(&backup, "AZURE_CLIENT_SECRET=edited\n").expect("an old file");
    }
    let principal = held(&paths, ENTRY).expect("held");
    assert_eq!(
        principal.env(),
        [
            ("AZURE_TENANT_ID".to_owned(), "t".to_owned()),
            ("AZURE_CLIENT_ID".to_owned(), "c".to_owned()),
            ("AZURE_CLIENT_SECRET".to_owned(), "s".to_owned()),
        ],
        "the store's value, whatever a file under the home holds"
    );
    publish("AZURE_CLIENT_SECRET=sentinel-never-print\n");
    let why = held(&paths, ENTRY).unwrap_err();
    assert!(why.ends_with("no AZURE_TENANT_ID line"), "{why}");
    assert!(!why.contains("sentinel"), "{why}");
}
