// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stored fleet inventory shown in aRegistry. A configuration is a live preference
//! link. Machine observations come from authenticated, self-keyed machine-facts records.

use super::{label_number, preferences, unconflicted_text};
use crate::alab::{
    account_names,
    registry::{ACCOUNT, ADDRESS, MACHINE, MACHINE_FACTS, PREFERENCE, bound_machine},
};
use crate::typed::{self, View, cell, keys, owned::OwnedRow};
use aterm_file::AccountNames;
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// A retained machine row and the evidence attached to that identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub key: String,
    pub name: String,
    pub status: String,
    /// Configured accounts in preference order, each by its key and display name.
    pub assignments: Vec<(String, String)>,
    /// The latest signed machine facts. More than one survives when no retained evidence
    /// orders those statements, including reports from separate enrollments.
    pub observations: Vec<Observation>,
}

/// One machine's own signed statement, with the time that writer recorded in its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    /// The writer's authenticated Commons machine identity.
    pub writer: String,
    pub at_ms: Option<u64>,
    pub summary: String,
}

fn status(row: &cell::Row) -> String {
    if row
        .fields
        .get(typed::LIFECYCLE)
        .is_some_and(cell::Field::conflicted)
    {
        return "Lifecycle conflict".into();
    }
    if row.raw.contains(typed::LIFECYCLE) {
        return "Unknown lifecycle".into();
    }
    if let Some(word) = row.lifecycle().filter(|word| *word != "live") {
        return word.to_owned();
    }
    if row
        .fields
        .get("status")
        .is_some_and(cell::Field::conflicted)
    {
        return "Status conflict".into();
    }
    if row.raw.contains("status") {
        return "Unknown status".into();
    }
    unconflicted_text(row, "status").unwrap_or_else(|| "Status not recorded".into())
}

fn account_labels(accounts: &BTreeMap<String, AccountNames>) -> BTreeMap<String, String> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for name in accounts.values().filter_map(|a| a.name.as_deref()) {
        *counts.entry(name).or_default() += 1;
    }
    accounts
        .iter()
        .map(|(key, account)| {
            let slug = if account.slug.is_empty() {
                key.as_str()
            } else {
                &account.slug
            };
            let name = match account.name.as_deref() {
                Some(name) if counts[name] > 1 => format!("{name} · {slug}"),
                Some(name) => name.to_owned(),
                None => slug.to_owned(),
            };
            (key.clone(), name)
        })
        .collect()
}

fn observation(row: &OwnedRow) -> Observation {
    // An owned write authenticates the signer and bounds the body. Validate each allowed
    // field against its schema before displaying it, including on older or malformed rows.
    let fact = |name| {
        let value = row.value.get(name)?;
        cell::validate(&MACHINE_FACTS, MACHINE_FACTS.column(name)?, value).ok()?;
        Some(value)
    };
    let mut words = Vec::new();
    for name in ["hw_model", "chip", "arch"] {
        if let Some(word) = fact(name).and_then(Value::as_str).filter(|w| !w.is_empty()) {
            words.push(word.to_owned());
        }
    }
    if let Some(gb) = fact("ram_gb").and_then(Value::as_u64) {
        words.push(format!("{gb} GB RAM"));
    }
    if words.is_empty() {
        words.push("Machine facts recorded".into());
    }
    Observation {
        writer: keys::uuid32(&row.machine),
        at_ms: fact("at").and_then(Value::as_u64),
        summary: words.join(" · "),
    }
}

/// Keep every maximal signed statement where several enrollments bind the same machine.
fn latest(view: &View, reports: &[&OwnedRow]) -> Vec<Observation> {
    if let [only] = reports {
        return vec![observation(only)];
    }
    let mut superseded = BTreeSet::new();
    for report in reports {
        if !superseded.contains(&report.operation) {
            let before = view.causal.ancestors(&report.operation);
            superseded.extend(
                reports
                    .iter()
                    .filter(|other| before.contains(&other.operation))
                    .map(|other| other.operation),
            );
        }
    }
    reports
        .iter()
        .filter(|row| !superseded.contains(&row.operation))
        .map(|row| observation(row))
        .collect()
}

/// Read the fleet inventory from one held registry generation, without opening other tables
/// or consulting a machine, a provider or a credential store.
#[must_use]
pub fn machines(view: &View) -> Vec<Machine> {
    let projected = view.project(&MACHINE);
    let accounts = account_names::of_projected(&view.project(&ACCOUNT), &view.project(&ADDRESS));
    let names = account_labels(&accounts);
    let preferences = preferences(view.project(&PREFERENCE), |machine, account| {
        projected.rows.get(machine).is_some_and(cell::Row::live) && accounts.contains_key(account)
    });
    let mut assignments = BTreeMap::<String, Vec<(String, String)>>::new();
    for preference in preferences {
        let links = assignments.entry(preference.machine).or_default();
        if !links.iter().any(|(key, _)| key == &preference.account) {
            let name = names[&preference.account].clone();
            links.push((preference.account, name));
        }
    }
    let mut facts: BTreeMap<&str, Vec<&OwnedRow>> = BTreeMap::new();
    if let Some(owned) = view.owned.get(MACHINE_FACTS.name) {
        for row in owned.rows.values() {
            if let Some(machine) = bound_machine(&row.value) {
                facts.entry(machine).or_default().push(row);
            }
        }
    }

    let mut machines: Vec<_> = projected
        .rows
        .into_iter()
        .map(|(key, row)| {
            let reports = facts.remove(key.as_str()).unwrap_or_default();
            let observations = latest(view, &reports);
            let name = unconflicted_text(&row, "label").unwrap_or_else(|| key.clone());
            Machine {
                name,
                status: status(&row),
                assignments: assignments.remove(&key).unwrap_or_default(),
                observations,
                key,
            }
        })
        .collect();
    machines.sort_by(|a, b| {
        (label_number(Some(&a.name)), &a.name, &a.key).cmp(&(
            label_number(Some(&b.name)),
            &b.name,
            &b.key,
        ))
    });
    machines
}
