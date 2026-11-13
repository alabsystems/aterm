// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::account_catalog::{Account, Catalog};

#[test]
fn shared_accounts_are_reported_when_no_local_roster_can_be_read() {
    let catalog = Ok(Catalog {
        table: "table-one".into(),
        history_complete: true,
        rows: vec![Account {
            key: "shared-one".into(),
            operation: "operation-one".into(),
            label: "Review me".into(),
            provider: "anthropic".into(),
            method: Some("oauth".into()),
            email: Some("shared@example.test".into()),
            realm: None,
            secret_reference: true,
            required_browser: false,
            conflicted: false,
            stopped: false,
            import: None,
            import_link: None,
        }],
        ..Default::default()
    });
    let mut output = Vec::new();
    let refused = accounts_refused_json(None, "no local roster", 1);
    write_combined_accounts_report(
        Cli::Claude,
        1,
        true,
        refused.as_bytes(),
        None,
        (Some(&catalog), None),
        &mut output,
    );
    let report: Value = aterm_json::from_slice(&output).unwrap();
    assert_eq!(
        report["shared_accounts"]["accounts"][0]["key"].as_str(),
        Some("shared-one")
    );
    assert_eq!(
        report["shared_accounts"]["accounts"][0]["local_label"],
        Value::Null
    );
    assert_eq!(
        report["shared_accounts"]["accounts"][0]["login_authority"].as_str(),
        Some("none")
    );
    assert_eq!(report["refused"].as_str(), Some("no local roster"));
}
