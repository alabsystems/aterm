// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn only_binary_provider_markers_make_presence_claims() {
    let root = crate::tests::tmp();
    let path = root.path().join("markers.csv");
    for (cell, expected) in [
        ("1", Some(true)),
        (" 1 ", Some(true)),
        ("0", Some(false)),
        (" 0 ", Some(false)),
        ("phone required", None),
        ("email link", None),
        ("true", None),
        ("false", None),
        ("01", None),
        ("1.0", None),
        ("0.0", None),
    ] {
        std::fs::write(&path, format!("email,google\nowner@example.test,{cell}\n")).unwrap();
        let value = candidate_document(&path).unwrap();
        let row = &value["rows"][0];
        let providers = row["providers"].as_array().unwrap();
        if let Some(present) = expected {
            assert_eq!(
                providers,
                &[json!({"provider":"google","column":2,"presence_marker":present})],
                "{cell}"
            );
        } else {
            assert!(providers.is_empty(), "{cell} is unclassified, not absent");
        }
        assert_eq!(row["unclassified_columns"], json!([2]));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("email,google\nowner@example.test,{cell}\n")
        );
    }
}

#[test]
fn csv_quotes_multiline_unicode_and_empty_columns_preserve_positions() {
    let table = parse("a,,c\r\n\"two, fields\",\"a\"\"b\nü\",\r\n".as_bytes()).unwrap();
    assert_eq!(
        table,
        vec![vec!["a", "", "c"], vec!["two, fields", "a\"b\nü", ""]]
    );
    for bad in ["a,\"not closed", "\"a\"trailing", "a\"b"] {
        assert!(parse(bad.as_bytes()).is_err());
    }
}
#[test]
fn identity_columns_keep_source_positions_and_allow_no_machine_column() {
    for (headers, email, machine) in [
        (vec!["notes", " EMAIL ", "Computer", "notes"], 1, Some(2)),
        (vec!["email", "google", "notes"], 0, None),
        (vec!["computer", "email"], 1, Some(0)),
    ] {
        let csv = Csv {
            headers: headers.into_iter().map(str::to_owned).collect(),
            rows: Vec::new(),
            digest: [0; 32],
        };
        assert_eq!(csv.column("email").unwrap(), Some(email));
        assert_eq!(csv.column("computer").unwrap(), machine);
    }
}
#[test]
fn private_source_import_is_restartable_without_guessing_aliases_or_copying_notes() {
    let root = crate::tests::tmp();
    let path = root.path().join("accounts.csv");
    std::fs::write(&path,"m,email,computer,google,notes\n7,owner+two@example.invalid,m3.2,phone required,password PRIVATE-CELL\nunknown,owner@example.invalid,m100,email link,alias mapping unconfirmed\n").unwrap();
    let dir = root.path().join("db");
    crate::local::init(&dir, "accounts").unwrap();
    let report = apply(&dir, &path, [1; 16]).unwrap();
    assert_eq!(report["provider_proposals"].as_u64(), Some(2));
    let first = crate::tests::wires(&dir).len();
    apply(&dir, &path, [1; 16]).unwrap();
    assert_eq!(crate::tests::wires(&dir).len(), first);
    let db = Database::open(&dir).unwrap();
    let view = db.view();
    assert_eq!(
        view.cells
            .keys()
            .filter(|(c, _)| c == "email-addresses")
            .count(),
        2
    );
    for cell in view.cells.values() {
        for c in &cell.candidates {
            assert!(
                !aterm_json::to_string(&c.value)
                    .unwrap()
                    .contains("PRIVATE-CELL")
            );
        }
    }
    let proposals: Vec<_> = view
        .cells
        .iter()
        .filter(|((collection, _), _)| collection == "account-proposals")
        .map(|(_, cell)| &cell.candidates[0].value)
        .collect();
    assert!(
        proposals
            .iter()
            .any(|p| p["machine_name_hint"] == json!("m3") && p["slot_hint"] == json!(2))
    );
    assert_eq!(
        inspect(&path).unwrap()["source_values_printed"],
        json!(false)
    );
    assert!(inspect(root.path()).is_err());
}
