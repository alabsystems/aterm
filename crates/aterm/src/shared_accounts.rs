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

/// The Commons account catalog under `home`: `None` where neither of its tables is declared there,
/// the machine having set up no catalog, which is no failure to read one. Where one is declared and
/// the other is not, the other's history is missing, so the catalog says it is incomplete.
pub(super) fn read(home: &Path) -> Result<Option<Catalog>, String> {
    let paths = Paths::for_home(home);
    let proposals = Proposals::at(paths.clone()).snapshot_if_declared();
    let imports = Imports::at(paths).snapshot_if_declared();
    if matches!((&proposals, &imports), (Ok(None), Ok(None))) {
        return Ok(None);
    }
    const UNDECLARED: &str = "application table is not initialized";
    let mut catalog = Catalog {
        history_complete: true,
        ..Default::default()
    };
    match proposals {
        Ok(Some(snapshot)) => catalog = project(&snapshot),
        Ok(None) => catalog
            .source_errors
            .push(format!("proposals: {UNDECLARED}")),
        Err(why) => catalog.source_errors.push(format!("proposals: {why}")),
    }
    match imports {
        Ok(Some(snapshot)) => {
            let imports = project_imports(&snapshot);
            catalog.import_table = imports.import_table;
            catalog.history_complete &= imports.history_complete;
            catalog.rows.extend(imports.rows);
        }
        Ok(None) => catalog.source_errors.push(format!("imports: {UNDECLARED}")),
        Err(why) => catalog.source_errors.push(format!("imports: {why}")),
    }
    if catalog.table.is_empty() && catalog.import_table.is_none() {
        return Err(catalog.source_errors.join("; "));
    }
    catalog.history_complete &= catalog.source_errors.is_empty();
    Ok(Some(catalog))
}

fn complete(snapshot: &Snapshot) -> bool {
    snapshot.history_complete()
}

fn required_browser(value: &aterm_json::Value) -> bool {
    value
        .get("details")
        .and_then(|details| details.get("browser-profile"))
        .is_some_and(|detail| detail["kind"].as_str() == Some("requirement"))
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
                required_browser: required_browser(value),
                conflicted: cell.candidates.len() != 1,
                stopped: cell.stopped,
                import: None,
                import_link: value["source"]["import"]
                    .as_object()
                    .filter(|v| {
                        v.get("kind").and_then(aterm_json::Value::as_str) == Some("source-ref")
                    })
                    .and_then(|v| v.get("value"))
                    .and_then(aterm_json::Value::as_str)
                    .map(str::to_owned),
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
        let conflicted = cell.candidates.len() != 1;
        for candidate in &cell.candidates {
            let Some(imported) = candidate.value["rows"].as_array() else {
                continue;
            };
            let operation = aterm_codec::hex::encode_lower(&candidate.operation);
            for row in imported {
                let Some(record) = row["record"].as_u64() else {
                    continue;
                };
                let label = format!("Source row {record}");
                let email = row["email"].as_str().map(str::to_owned);
                let machine_hint = row["machine_slot_hint"].as_str().map(str::to_owned);
                let unclassified_columns: Vec<u64> = row["unclassified_columns"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(aterm_json::Value::as_u64)
                    .collect();
                let account_with =
                    |provider: Option<&aterm_json::Value>,
                     label: String,
                     email: Option<String>,
                     machine_hint: Option<String>,
                     unclassified_columns: Vec<u64>| {
                        let column = provider.and_then(|provider| provider["column"].as_u64());
                        Account {
                            key: format!("import:{source}:{record}:{}", column.unwrap_or(0)),
                            operation: operation.clone(),
                            label,
                            provider: provider
                                .and_then(|provider| provider["provider"].as_str())
                                .unwrap_or("unspecified")
                                .into(),
                            method: None,
                            email,
                            realm: None,
                            secret_reference: false,
                            required_browser: false,
                            conflicted,
                            stopped: cell.stopped,
                            import: Some(ImportOrigin {
                                table: table.clone(),
                                source: source.clone(),
                                record,
                                provider_column: column,
                                presence: provider
                                    .and_then(|provider| provider["presence_marker"].as_bool()),
                                machine_hint,
                                unclassified_columns,
                            }),
                            import_link: None,
                        }
                    };
                match row["providers"].as_array() {
                    Some(providers) if !providers.is_empty() => {
                        let last = providers.len() - 1;
                        for provider in &providers[..last] {
                            rows.push(account_with(
                                Some(provider),
                                label.clone(),
                                email.clone(),
                                machine_hint.clone(),
                                unclassified_columns.clone(),
                            ));
                        }
                        rows.push(account_with(
                            Some(&providers[last]),
                            label,
                            email,
                            machine_hint,
                            unclassified_columns,
                        ));
                    }
                    _ => rows.push(account_with(
                        None,
                        label,
                        email,
                        machine_hint,
                        unclassified_columns,
                    )),
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
#[path = "shared_accounts_tests.rs"]
mod tests;
