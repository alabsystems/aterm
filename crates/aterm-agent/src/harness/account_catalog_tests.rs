// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A HOME WITH NO SECRETS DIRECTORY KEEPS NO CATALOG, SO NO REPORT CALLS ONE UNAVAILABLE:
/// `read_with` gives `None` there, not the reader's error. CONTROL: once the directory is
/// there, the reader's error stands (a catalog set up and not read is unavailable), and with
/// no home known there is none either.
#[cfg(unix)]
#[test]
fn a_home_with_no_secrets_directory_keeps_no_catalog() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch home");
    let home = tmp.path();
    let reader: &Reader = &|_: &Path| Err("proposals: unreadable".to_owned());
    assert_eq!(read_with(Some(reader), Some(home)), None);
    let undeclared: &Reader = &|_: &Path| Ok(None);
    let dir = aterm_secrets::client::Paths::for_home(home).dir;
    std::fs::create_dir_all(&dir).expect("the secrets directory");
    assert_eq!(
        read_with(Some(reader), Some(home)),
        Some(Err("proposals: unreadable".to_owned())),
        "CONTROL: a catalog set up and not read is unavailable"
    );
    assert_eq!(
        read_with(Some(undeclared), Some(home)),
        None,
        "a catalog table nobody declared is no failure to read one"
    );
    assert_eq!(read_with(Some(reader), None), None, "CONTROL: no home");
}

pub(super) fn account() -> Account {
    Account {
        key: "account-one".into(),
        operation: "operation-one".into(),
        label: "Shared one".into(),
        provider: "anthropic".into(),
        method: Some("oauth".into()),
        email: Some("one@example.test".into()),
        realm: None,
        secret_reference: true,
        required_browser: false,
        conflicted: false,
        stopped: false,
        import: None,
        import_link: None,
    }
}

#[test]
fn a_shared_description_never_grants_login_authority() {
    let catalog = Ok(Catalog {
        table: "shared-table".into(),
        history_complete: true,
        rows: vec![account()],
        ..Default::default()
    });
    let value = report(
        &catalog,
        Cli::Claude,
        Here {
            roster: Some(&Roster::default()),
            own: None,
        },
    );
    assert_eq!(value["accounts"][0]["key"].as_str(), Some("account-one"));
    assert_eq!(value["accounts"][0]["local_label"], Value::Null);
    assert_eq!(
        value["accounts"][0]["login_authority"].as_str(),
        Some("none")
    );
    assert!(value["accounts"][0]["secret_reference"].as_bool().unwrap());
    assert_eq!(Roster::default().rows.len(), 0);
}

#[test]
fn an_unsupported_profile_is_explicit_without_granting_authority() {
    let mut row = account();
    row.required_browser = true;
    let catalog = Ok(Catalog {
        rows: vec![row],
        history_complete: true,
        ..Default::default()
    });
    let report = report_all(&catalog);
    assert_eq!(
        report["accounts"][0]["state"].as_str(),
        Some("unsupported-preparation")
    );
    assert_eq!(
        report["accounts"][0]["preparation_reason"].as_str(),
        Some(NAMED_BROWSER_UNSUPPORTED)
    );
    assert_eq!(
        report["accounts"][0]["login_authority"].as_str(),
        Some("none")
    );
    assert!(text_all(&catalog).contains(NAMED_BROWSER_UNSUPPORTED));
}

/// A REPORT LISTS THIS MAC'S ROWS AND COUNTS THE REST, and keeps a row whose source is in
/// question, which no count would show. CONTROLS: `text`, the `accounts table` listing, still
/// has every row, and another CLI's row is neither listed nor counted.
#[test]
fn a_report_lists_this_macs_rows_and_counts_the_rest() {
    let with = |label: &str, provider: &str| Account {
        label: label.into(),
        provider: provider.into(),
        email: Some(format!("{label}@example.test")),
        ..account()
    };
    let mut questioned = with("questioned", "anthropic");
    questioned.conflicted = true;
    let catalog = Ok(Catalog {
        table: "table".into(),
        history_complete: true,
        rows: vec![
            with("mine", "anthropic"),
            with("theirs", "anthropic"),
            with("further", "anthropic"),
            questioned,
            with("codexrow", "openai"),
        ],
        ..Default::default()
    });
    let roster = Roster {
        rows: vec![accounts::Row {
            label: "local".into(),
            email: Some("mine@example.test".into()),
            ..Default::default()
        }],
    };
    let listed = Here {
        roster: Some(&roster),
        own: None,
    };
    let here = text_here(&catalog, Cli::Claude, listed);
    assert!(
        here.contains("2 of its 4 rows are not on this Mac"),
        "{here}"
    );
    assert!(here.contains("<mine@example.test>"), "{here}");
    assert!(here.contains("local local"), "{here}");
    assert!(here.contains("<questioned@example.test>"), "{here}");
    assert!(
        !here.contains("theirs@") && !here.contains("further@"),
        "{here}"
    );
    assert!(!here.contains("codexrow@"), "{here}");
    let all = text(&catalog, Cli::Claude, listed);
    for label in ["mine", "theirs", "further", "questioned"] {
        assert!(all.contains(&format!("<{label}@example.test>")), "{all}");
    }
    assert!(
        text_here(&catalog, Cli::Claude, Here::default()).contains("3 of its 4 rows"),
        "with no roster only the questioned row stays"
    );
    // A Mac that lists no account runs the login its home holds: the catalog row naming that
    // address is this Mac's own, not `not listed here`.
    // NEGATIVE CONTROL: with `local_words` reading the roster alone, the row is left out of
    // `text_here` and the count reads `3 of its 4 rows`.
    let own = Here {
        roster: None,
        own: Some("mine@example.test"),
    };
    let here = text_here(&catalog, Cli::Claude, own);
    assert!(here.contains("2 of its 4 rows"), "{here}");
    assert!(
        here.contains("<mine@example.test>") && here.contains("local this Mac's own login"),
        "{here}"
    );
    let unavailable = Err("unreadable".to_owned());
    assert_eq!(
        text_here(&unavailable, Cli::Claude, Here::default()),
        text(&unavailable, Cli::Claude, Here::default())
    );
}

#[test]
fn only_a_unique_matching_local_identity_supplies_display_facts() {
    let mut roster = Roster {
        rows: vec![accounts::Row {
            label: "local".into(),
            email: account().email,
            ..Default::default()
        }],
    };
    assert_eq!(account().local_label(&roster), Some("local"));
    roster.rows.push(roster.rows[0].clone());
    assert_eq!(account().local_label(&roster), None);
    roster.rows.pop();
    let mut changed = account();
    changed.conflicted = true;
    assert_eq!(changed.local_label(&roster), None);
    changed.conflicted = false;
    changed.realm = Some("mainland-cn".into());
    assert_eq!(changed.local_label(&roster), None);
    roster.rows[0].held_by = Some("other-mac".into());
    assert_eq!(account().local_label(&roster), None);
    roster.rows[0].held_by = None;
    roster.rows[0].email = None;
    roster.rows[0].identity_email = account().email;
    assert_eq!(account().local_label(&roster), Some("local"));
}

#[test]
fn incomplete_or_duplicate_shared_identity_withholds_local_status() {
    let roster = Roster {
        rows: vec![accounts::Row {
            label: "local".into(),
            email: account().email,
            ..Default::default()
        }],
    };
    let mut catalog = Catalog {
        table: "table".into(),
        history_complete: true,
        rows: vec![account()],
        ..Default::default()
    };
    assert_eq!(
        catalog.local_label(&catalog.rows[0], &roster),
        Some("local")
    );
    catalog.history_complete = false;
    assert_eq!(catalog.local_label(&catalog.rows[0], &roster), None);
    catalog.history_complete = true;
    let mut duplicate = account();
    duplicate.key = "another".into();
    duplicate.provider = "claude".into();
    catalog.rows.push(duplicate);
    assert_eq!(catalog.local_label(&catalog.rows[0], &roster), None);
}

#[test]
fn imported_markers_remain_distinct_and_authentication_is_unspecified() {
    let mut row = account();
    row.provider = "google".into();
    row.method = None;
    row.import = Some(ImportOrigin {
        table: "imports".into(),
        source: "source-one".into(),
        record: 9,
        provider_column: Some(4),
        presence: Some(false),
        machine_hint: Some("m3.2".into()),
        unclassified_columns: vec![7],
    });
    let catalog = Ok(Catalog {
        rows: vec![row],
        history_complete: true,
        ..Default::default()
    });
    let report = report_all(&catalog);
    assert_eq!(report["accounts"].as_array().unwrap().len(), 1);
    let row = &report["accounts"][0];
    assert_eq!(row["provider"].as_str(), Some("google"));
    assert_eq!(row["state"].as_str(), Some("not-indicated"));
    assert_eq!(row["method"], Value::Null);
    assert_eq!(row["import"]["presence_marker"].as_bool(), Some(false));
    assert_eq!(row["import"]["record"].as_u64(), Some(9));
    assert_eq!(row["login_authority"].as_str(), Some("none"));
}
