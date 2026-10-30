// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only account-list composition from the retained Commons proposals.

use aterm_agent::harness::account_catalog::{Account, Catalog, ImportOrigin};
use aterm_db::{
    accounts::proposals::{Proposals, Snapshot},
    import::commons::Imports,
};
use aterm_secrets::client::Paths;
use std::path::Path;

pub(super) fn read(home: &Path) -> Result<Catalog, String> {
    let paths = Paths::for_home(home);
    let mut catalog = Catalog {
        history_complete: true,
        ..Default::default()
    };
    match Proposals::at(paths.clone()).snapshot() {
        Ok(snapshot) => catalog = project(&snapshot),
        Err(why) => catalog.source_errors.push(format!("proposals: {why}")),
    }
    match Imports::at(paths).snapshot() {
        Ok(snapshot) => {
            let imports = project_imports(&snapshot);
            catalog.import_table = imports.import_table;
            catalog.history_complete &= imports.history_complete;
            catalog.rows.extend(imports.rows);
        }
        Err(why) => catalog.source_errors.push(format!("imports: {why}")),
    }
    if catalog.table.is_empty() && catalog.import_table.is_none() {
        return Err(catalog.source_errors.join("; "));
    }
    catalog.history_complete &= catalog.source_errors.is_empty();
    Ok(catalog)
}

fn complete(snapshot: &Snapshot) -> bool {
    snapshot.withheld == 0
        && snapshot
            .operations
            .iter()
            .all(|o| o.available && o.refusal.is_none())
}

fn project(snapshot: &Snapshot) -> Catalog {
    let mut rows = Vec::new();
    for ((collection, key), cell) in &snapshot.cells {
        if collection != "accounts" {
            continue;
        }
        for candidate in &cell.candidates {
            let value = &candidate.value;
            if value.is_null() {
                continue;
            }
            let text = |field: &str| value[field].as_str().map(str::to_owned);
            rows.push(Account {
                key: key.clone(),
                operation: aterm_codec::hex::encode_lower(&candidate.operation),
                label: text("label").unwrap_or_else(|| key.clone()),
                provider: text("provider").unwrap_or_default(),
                method: text("method"),
                email: text("email"),
                realm: text("realm"),
                secret_reference: value.get("secret_ref").is_some_and(|v| !v.is_null()),
                conflicted: cell.candidates.len() != 1,
                stopped: cell.stopped,
                import: None,
            });
        }
    }
    Catalog {
        table: aterm_codec::hex::encode_lower(&snapshot.scope.digest()),
        history_complete: complete(snapshot),
        rows,
        ..Default::default()
    }
}

fn project_imports(snapshot: &Snapshot) -> Catalog {
    let table = aterm_codec::hex::encode_lower(&snapshot.scope.digest());
    let mut rows = Vec::new();
    for ((collection, source), cell) in &snapshot.cells {
        if collection != "sources" {
            continue;
        }
        for candidate in &cell.candidates {
            let Some(imported) = candidate.value["rows"].as_array() else {
                continue;
            };
            for row in imported {
                let Some(record) = row["record"].as_u64() else {
                    continue;
                };
                let account = |provider: Option<&aterm_json::Value>| {
                    let column = provider.and_then(|provider| provider["column"].as_u64());
                    Account {
                        key: format!("import:{source}:{record}:{}", column.unwrap_or(0)),
                        operation: aterm_codec::hex::encode_lower(&candidate.operation),
                        label: format!("Source row {record}"),
                        provider: provider
                            .and_then(|provider| provider["provider"].as_str())
                            .unwrap_or("unspecified")
                            .into(),
                        method: None,
                        email: row["email"].as_str().map(str::to_owned),
                        realm: None,
                        secret_reference: false,
                        conflicted: cell.candidates.len() != 1,
                        stopped: cell.stopped,
                        import: Some(ImportOrigin {
                            table: table.clone(),
                            source: source.clone(),
                            record,
                            provider_column: column,
                            presence: provider
                                .and_then(|provider| provider["presence_marker"].as_bool()),
                            machine_hint: row["machine_slot_hint"].as_str().map(str::to_owned),
                            unclassified_columns: row["unclassified_columns"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(aterm_json::Value::as_u64)
                                .collect(),
                        }),
                    }
                };
                match row["providers"].as_array() {
                    Some(providers) if !providers.is_empty() => {
                        rows.extend(providers.iter().map(|provider| account(Some(provider))))
                    }
                    _ => rows.push(account(None)),
                }
            }
        }
    }
    Catalog {
        import_table: Some(table),
        history_complete: complete(snapshot),
        rows,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
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
    fn shared_catalog_preserves_conflicts_and_only_projects_credential_metadata() {
        let value = json!({"provider":"anthropic", "method":"oauth", "label":"Shared", "email":"one@example.test", "secret_ref":{"vault":"alab", "entry":"account-key"}});
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
}
