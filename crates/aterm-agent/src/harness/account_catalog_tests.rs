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
            names: None,
        },
    );
    assert_eq!(value["accounts"][0]["key"].as_str(), Some("account-one"));
    assert_eq!(value["accounts"][0]["local"], Value::Null);
    assert_eq!(value["accounts"][0]["local_own"], Value::from(false));
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
        names: None,
    };
    let here = text_here(&catalog, Cli::Claude, listed);
    assert!(
        here.contains("2 of its 4 rows are not on this Mac"),
        "{here}"
    );
    assert!(here.contains("<mine@example.test>"), "{here}");
    assert!(
        here.contains("local claude mine@example.test\n"),
        "the local row by its account's name, never its label: {here}"
    );
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
        names: None,
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

/// A ROW THIS MAC HOLDS IS NAMED AS THE REPORT NAMES IT. Under a listing of the roster, which the
/// report and `accounts table` pass once they read one, the JSON's `local` is the row's account
/// key and `local_own` is false, and the text's `local` cell names the row's account by its name
/// and slug ([`Listing::shown`]); the row's label is printed nowhere. This Mac's own login, which
/// no row lists, has `local: null`, `local_own: true` and `local this Mac's own login`.
/// NEGATIVE CONTROLS, each its own run: with `Local::id`'s listed arm giving the row's label,
/// the JSON's `local` is `m2-2`; with `Local::words`' listed arm giving the label, the text
/// reads `local m2-2`; with `local_own` always false, the own login's is false; each fails this
/// (2026-11-16, mutation runs on SF-07H S7b's review settle, then restored from HEAD).
#[test]
fn a_local_row_is_named_by_its_account_in_the_json_and_the_text() {
    use crate::harness::account_names::Listing;
    use crate::harness::alab_roster::Lab;
    let email = "mine@example.test";
    let pk = aterm_db::typed::keys::account_pk("claude", email).expect("a key");
    let lab = Lab {
        names: [(
            pk.clone(),
            aterm_file::AccountNames {
                realm: "claude".into(),
                slug: "claude-mine-e".into(),
                name: Some(format!("claude {email}")),
                email: Some(email.into()),
                login: Some(email.into()),
                live: true,
            },
        )]
        .into_iter()
        .collect(),
        ..Lab::default()
    };
    let roster = Roster {
        rows: vec![accounts::Row {
            label: "m2-2".into(),
            email: Some(email.into()),
            ..Default::default()
        }],
    };
    let names = Listing::of(Cli::Claude, &roster.rows, Some(&lab));
    let catalog = Ok(Catalog {
        table: "table".into(),
        history_complete: true,
        rows: vec![Account {
            label: "Mine".into(),
            email: Some(email.into()),
            ..account()
        }],
        ..Default::default()
    });
    let here = Here {
        roster: Some(&roster),
        own: None,
        names: Some(&names),
    };
    let value = report(&catalog, Cli::Claude, here);
    let row = &value["accounts"][0];
    assert_eq!(
        (row.get("local"), row.get("local_own")),
        (Some(&Value::from(pk.as_str())), Some(&Value::from(false))),
        "the row here by its account's key: {row}"
    );
    let shown = names.shown("m2-2").into_owned();
    assert_eq!(shown, "claude mine@example.test (claude-mine-e)");
    let listed = text_here(&catalog, Cli::Claude, here);
    assert!(listed.contains(&format!("; local {shown}\n")), "{listed}");
    let every = text(&catalog, Cli::Claude, here);
    assert!(every.contains(&format!("; local {shown}\n")), "{every}");
    for printed in [value.to_string(), listed, every] {
        assert!(
            !printed
                .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
                .any(|word| word == "m2-2"),
            "the row's label is printed: {printed}"
        );
    }
    let none = Listing::of(Cli::Claude, &[], Some(&lab));
    let own = Here {
        roster: Some(&Roster::default()),
        own: Some(email),
        names: Some(&none),
    };
    let value = report(&catalog, Cli::Claude, own);
    let row = &value["accounts"][0];
    assert_eq!(
        (row.get("local"), row.get("local_own")),
        (Some(&Value::Null), Some(&Value::from(true))),
        "this Mac's own login, which no row lists: {row}"
    );
    let listed = text_here(&catalog, Cli::Claude, own);
    assert!(
        listed.contains("; local this Mac's own login\n"),
        "{listed}"
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
