// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_db::{
    accounts::proposals::Snapshot,
    database::{Candidate, Cell},
    table::Scope,
};
use aterm_json::json;
use aterm_secrets::commons::journal::Trust;
use std::collections::BTreeMap;

#[test]
fn only_a_required_browser_is_labeled_an_unsupported_requirement() {
    for (kind, required) in [("requirement", true), ("note", false), ("status", false)] {
        assert_eq!(
            required_browser(&json!({"details":{"browser-profile":{"kind":kind,"value":"named"}}})),
            required
        );
    }
    assert!(!required_browser(&json!({})));
}

#[test]
fn shared_catalog_preserves_conflicts_and_only_projects_credential_metadata() {
    let value = json!({"provider":"anthropic", "method":"oauth", "label":"Shared", "email":"one@example.test", "secret_ref":{"vault":"alab", "entry":"account-key"},"details":{"browser-profile":{"kind":"requirement","value":"named"}}});
    let scope = Scope::new(
        Trust {
            network: [1; 16],
            authority: [2; 32],
            genesis: [3; 32],
        },
        [4; 16],
        1,
    )
    .unwrap();
    let snapshot = Snapshot {
        scope,
        retained: 2,
        withheld: 1,
        operations: Vec::new(),
        cells: BTreeMap::from([
            (
                ("accounts".into(), "one".into()),
                Cell {
                    stopped: false,
                    candidates: vec![
                        Candidate {
                            operation: [5; 32],
                            value: value.clone(),
                        },
                        Candidate {
                            operation: [6; 32],
                            value,
                        },
                    ],
                },
            ),
            (
                ("mailboxes".into(), "private".into()),
                Cell {
                    stopped: false,
                    candidates: vec![Candidate {
                        operation: [7; 32],
                        value: json!({"password":"not-an-account-field"}),
                    }],
                },
            ),
        ]),
    };
    let catalog = project(&snapshot);
    assert_eq!(catalog.rows.len(), 2);
    assert!(catalog.rows.iter().all(|row| row.required_browser));
    assert!(!catalog.history_complete);
    assert!(
        catalog
            .rows
            .iter()
            .all(|row| row.conflicted && row.secret_reference)
    );
    assert_ne!(catalog.rows[0].operation, catalog.rows[1].operation);
    assert_eq!(snapshot.cells.len(), 2);
}

#[test]
fn imported_source_rows_keep_presence_and_coordinates_without_auth_guesses() {
    let scope = Scope::new(
        Trust {
            network: [1; 16],
            authority: [2; 32],
            genesis: [3; 32],
        },
        [4; 16],
        1,
    )
    .unwrap();
    let source = "a".repeat(64);
    let snapshot = Snapshot {
        scope,
        retained: 1,
        withheld: 0,
        operations: Vec::new(),
        cells: BTreeMap::from([(
            ("sources".into(), source.clone()),
            Cell {
                stopped: false,
                candidates: vec![Candidate {
                    operation: [5; 32],
                    value: json!({
                        "rows":[
                            {"record":2,"email":"one@example.test","machine_slot_hint":"m3.2","unclassified_columns":[7],
                             "providers":[{"provider":"google","column":5,"presence_marker":true},{"provider":"meta","column":6,"presence_marker":false}]},
                            {"record":3,"email":null,"machine_slot_hint":null,"unclassified_columns":[],"providers":[]}
                        ]
                    }),
                }],
            },
        )]),
    };
    let catalog = project_imports(&snapshot);
    assert_eq!(catalog.rows.len(), 3);
    assert!(
        catalog
            .rows
            .iter()
            .all(|row| row.method.is_none() && !row.secret_reference)
    );
    let origins: Vec<_> = catalog
        .rows
        .iter()
        .map(|row| row.import.as_ref().unwrap())
        .collect();
    assert_eq!(
        origins
            .iter()
            .map(|source| source.presence)
            .collect::<Vec<_>>(),
        [Some(true), Some(false), None]
    );
    assert_eq!(
        origins
            .iter()
            .map(|source| source.record)
            .collect::<Vec<_>>(),
        [2, 2, 3]
    );
    assert_eq!(origins[0].source, source);
    assert_eq!(origins[0].machine_hint.as_deref(), Some("m3.2"));
    assert_eq!(catalog.rows[2].provider, "unspecified");
    assert_ne!(catalog.rows[0].key, catalog.rows[1].key);
}
