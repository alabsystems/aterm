// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::client;
use aterm_secrets::need::catalog::AZURE_SIGNING_PRINCIPAL_ENTRY as ENTRY;
use aterm_secrets::text::Audience;

/// A file vault made at `root/fixture.vault` by the host `root/host`, which administers it, and
/// that host's paths.
fn scratch_vault(root: &std::path::Path) -> client::Paths {
    let paths = client::Paths::for_home(&root.join("host"));
    client::create(
        &paths,
        root.join("fixture.vault").to_str().expect("utf-8"),
        None,
        Some("host"),
    )
    .expect("vault");
    paths
}

/// Publish `value` as the vault's `[everyone]` entry [`ENTRY`], as an admin Mac would.
fn publish(paths: &client::Paths, value: &str) {
    let admin = client::admin_open(paths).expect("admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, ENTRY, value.as_bytes())
        .expect("entry");
    client::admin_publish(paths, &admin, &doc).expect("publish");
}

#[test]
fn a_principal_prints_its_names_and_never_a_value() {
    let principal = Principal::new(vec![(
        "AZURE_CLIENT_SECRET".into(),
        "sentinel-never-print".into(),
    )]);
    let shown = format!("{principal:?}");
    assert_eq!(shown, r#"["AZURE_CLIENT_SECRET"]"#);
}

#[test]
fn a_host_that_applied_no_vault_says_so_and_files_nothing() {
    let home = aterm_tempfile::tempdir().expect("scratch home");
    let paths = aterm_secrets::client::Paths::locate_for_home(home.path());
    let why = from_vault_in(&paths, Miss::File).unwrap_err();
    assert_eq!(why, aterm_secrets::registry::NO_REGISTRY);
    assert!(!paths.dir.exists(), "no ledger was written");
}

#[test]
fn the_entry_is_read_in_place_from_the_store_as_the_three_variables() {
    let root = aterm_tempfile::tempdir().expect("scratch root");
    let paths = scratch_vault(root.path());
    assert!(
        held(&paths, ENTRY)
            .unwrap_err()
            .ends_with("is not on this host yet")
    );
    publish(
        &paths,
        "AZURE_TENANT_ID=t\nAZURE_CLIENT_ID=c\nAZURE_CLIENT_SECRET=s\n",
    );
    let backup = paths.home.join(".secrets").join(ENTRY);
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
    publish(&paths, "AZURE_CLIENT_SECRET=sentinel-never-print\n");
    let why = held(&paths, ENTRY).unwrap_err();
    assert!(why.ends_with("no AZURE_TENANT_ID line"), "{why}");
    assert!(!why.contains("sentinel"), "{why}");
}

/// What `sign` meets on any host the vault client builds on, a Windows signer's included: a
/// vault that holds no principal files `azure-signing-principal` in this host's need ledger, once,
/// and names the request, where a doctor's look ([`Miss::Report`]) files nothing; once the vault
/// holds one, the principal is the three variables, read in place. NEGATIVE CONTROL (run on
/// Windows, 2026-11-16): with the `need::raise` of `from_vault_in` taken out, the unseeded read
/// files nothing and this test fails at the ledger's count.
#[test]
fn a_vault_that_holds_the_principal_yields_it_and_one_that_does_not_files_the_need() {
    use aterm_secrets::need::{self, Purpose, Want};
    let root = aterm_tempfile::tempdir().expect("scratch root");
    let paths = scratch_vault(root.path());

    let report = from_vault_in(&paths, Miss::Report).unwrap_err();
    assert_eq!(report, format!("this host's vault holds no {ENTRY} entry"));
    assert!(
        need::peek(&paths).expect("ledger").is_empty(),
        "a doctor's look files nothing"
    );

    let why = from_vault_in(&paths, Miss::File).unwrap_err();
    let filed = need::peek(&paths).expect("ledger");
    assert_eq!(filed.len(), 1, "{filed:?}");
    let need = &filed[0];
    assert_eq!(need.want, Want::AzureSigningPrincipal);
    assert_eq!(need.want.key(), "azure-signing-principal");
    assert_eq!(need.purpose, Purpose::WinsignAzure);
    assert_eq!(
        why,
        format!(
            "this host's vault holds no {ENTRY} entry; asked the lab's Macs for \
             azure-signing-principal (request {})",
            aterm_codec::hex::encode_lower(&need.id)
        )
    );
    from_vault_in(&paths, Miss::File).unwrap_err();
    assert_eq!(
        need::peek(&paths).expect("ledger").len(),
        1,
        "a second miss raises the same need again, never a second one"
    );

    publish(
        &paths,
        "AZURE_TENANT_ID=t\nAZURE_CLIENT_ID=c\nAZURE_CLIENT_SECRET=s\n",
    );
    let principal = from_vault_in(&paths, Miss::File).expect("the vault holds it");
    assert_eq!(
        principal.env(),
        [
            ("AZURE_TENANT_ID".to_owned(), "t".to_owned()),
            ("AZURE_CLIENT_ID".to_owned(), "c".to_owned()),
            ("AZURE_CLIENT_SECRET".to_owned(), "s".to_owned()),
        ]
    );
}
