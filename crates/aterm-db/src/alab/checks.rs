// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db alab check` (docs/DESIGN-alab-lab-model-2026-11-01.md §4.15, §6): the lab's own
//! rules over the registry's converged rows, on top of the generic reference, unique and
//! conflict findings (`typed::check`). Per-cell merge can break a rule no single edit broke, so
//! readers report it and nothing repairs it silently. A finding names collections, keys and
//! columns, never a value.

use super::registry::{
    ACCOUNT, ADDRESS, COLLECTIONS, MACHINE, MACHINE_FACTS, MAILBOX, MAILBOX_OBS, PREFERENCE,
};
use crate::typed::{
    Kind,
    cell::{self, Cells, Projection},
    check, keys,
    owned::OwnedRead,
};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// One lab finding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    /// Its word.
    pub kind: &'static str,
    pub collection: &'static str,
    /// The keys it concerns.
    pub pks: Vec<String>,
}

/// The rules over the Macs' own rows (design §4.7): each is a finding and nothing merges. Two live
/// enrollments bind one machine; two live enrollments state one hardware hint, a clone or a
/// machine identifier VMs share; a row binds a machine the registry does not have; and a Mac's
/// facts differ from the owner's statement on its machine row, where the facts win in views.
pub fn facts(read: &OwnedRead, live: &BTreeSet<String>, machines: &Projection) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut by_machine = BTreeMap::<&str, Vec<String>>::new();
    let mut by_hint = BTreeMap::<&str, Vec<String>>::new();
    for (uuid, row) in read.rows.iter().filter(|(uuid, _)| live.contains(*uuid)) {
        let text = |column: &str| row.value.get(column).and_then(Value::as_str);
        if let Some(mac) = text("mac") {
            by_machine.entry(mac).or_default().push(uuid.clone());
            match machines.rows.get(mac) {
                None => found.push(Finding {
                    kind: "facts-machine-unknown",
                    collection: MACHINE_FACTS.name,
                    pks: vec![uuid.clone()],
                }),
                Some(machine) => {
                    let differs = ["chip", "ram_gb", "disk_gb"].into_iter().any(|column| {
                        machine.get(column).is_some_and(|stated| {
                            row.value.get(column).is_some_and(|fact| fact != stated)
                        })
                    });
                    if differs {
                        found.push(Finding {
                            kind: "facts-differ",
                            collection: MACHINE_FACTS.name,
                            pks: vec![uuid.clone(), mac.to_owned()],
                        });
                    }
                }
            }
        }
        if let Some(hint) = text("hw_hint") {
            by_hint.entry(hint).or_default().push(uuid.clone());
        }
    }
    let shared = |kind, by: BTreeMap<&str, Vec<String>>, found: &mut Vec<Finding>| {
        for pks in by.into_values().filter(|pks| pks.len() > 1) {
            found.push(Finding {
                kind,
                collection: MACHINE_FACTS.name,
                pks,
            });
        }
    };
    shared("facts-bind-one-machine", by_machine, &mut found);
    shared("hw-hint-clone", by_hint, &mut found);
    found.sort();
    found
}

/// The rules over discovered mailbox address lists (design §4.10, §6, §7): a row's key prefix
/// matches its `mailbox` cell and names a live mailbox row; two mailboxes listing one live address
/// in their latest complete observations are ambiguous; and an owner's `address.mailbox` that
/// differs from the mailbox(es) discovery positively observed for that address (or its `+tag`
/// base) is reported without overriding the owner.
pub fn mailbox_obs(
    read: &OwnedRead,
    addresses: &Projection,
    mailboxes: &Projection,
) -> Vec<Finding> {
    let mut found = Vec::new();
    for (key, row) in &read.rows {
        let mailbox = row.value.get("mailbox").and_then(Value::as_str);
        if mailbox.is_none_or(|mb| key.rsplit_once('.').is_none_or(|(prefix, _)| prefix != mb)) {
            found.push(Finding {
                kind: "mailbox-obs-key-mismatch",
                collection: MAILBOX_OBS.name,
                pks: vec![key.clone()],
            });
        }
        if let Some(mb) = mailbox {
            match mailboxes.rows.get(mb) {
                None => found.push(Finding {
                    kind: "mailbox-obs-mailbox-unknown",
                    collection: MAILBOX_OBS.name,
                    pks: vec![key.clone()],
                }),
                Some(row) if !row.live() => found.push(Finding {
                    kind: "mailbox-obs-on-retired-mailbox",
                    collection: MAILBOX_OBS.name,
                    pks: vec![key.clone(), mb.to_owned()],
                }),
                Some(_) => {}
            }
        }
    }
    let seen = super::mail::observed(read.rows.iter().map(|(k, r)| (k.as_str(), &r.value)));
    for (pk, mbs) in &seen {
        if mbs.len() > 1 && addresses.rows.get(pk).is_some_and(cell::Row::live) {
            let mut pks = Vec::with_capacity(1 + mbs.len());
            pks.push(pk.clone());
            pks.extend(mbs.iter().cloned());
            found.push(Finding {
                kind: "mailbox-obs-ambiguous",
                collection: ADDRESS.name,
                pks,
            });
        }
    }
    for (pk, row) in addresses.rows.iter().filter(|(_, r)| r.live()) {
        if let Some(owner_mb) = row.get("mailbox").and_then(Value::as_str) {
            let mut differs: Vec<String> = match seen.get(pk) {
                Some(mbs) => mbs
                    .iter()
                    .filter(|mb| mb.as_str() != owner_mb)
                    .cloned()
                    .collect(),
                None if row.get("alias_of").is_none() => {
                    super::mail::listed_base(addresses, &seen, pk)
                        .into_iter()
                        .map(|(_, mb)| mb)
                        .filter(|mb| mb != owner_mb)
                        .collect()
                }
                None => Vec::new(),
            };
            differs.sort();
            differs.dedup();
            if !differs.is_empty() {
                let mut pks = Vec::with_capacity(2 + differs.len());
                pks.push(pk.clone());
                pks.push(owner_mb.to_owned());
                pks.extend(differs);
                found.push(Finding {
                    kind: "mailbox-discovery-differs",
                    collection: ADDRESS.name,
                    pks,
                });
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// The lab rules over `cells`, then every generic finding.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Follow",
        project = "aterm_db::alab::view::tests::follow_transition"
    )
)]
pub fn lab(cells: &Cells) -> (Vec<Finding>, Vec<check::Finding>) {
    let projections: BTreeMap<&str, Projection> = COLLECTIONS
        .iter()
        .filter(|c| c.kind == Kind::Cells)
        .map(|c| (c.name, cell::project(c, cells)))
        .collect();
    let generic = check::findings(
        &COLLECTIONS,
        &projections.iter().map(|(name, p)| (*name, p)).collect(),
    );
    let mut found = Vec::new();
    let machines = &projections[MACHINE.name];
    let addresses = &projections[ADDRESS.name];
    let mailboxes = &projections[MAILBOX.name];
    let live = |p: &Projection, pk: &str| p.rows.get(pk).is_some_and(cell::Row::live);
    let text =
        |row: &cell::Row, column: &str| row.get(column).and_then(Value::as_str).map(str::to_owned);
    // Preferences: a live one is keyed by its pair and names a machine and an account still live. It is a mapping
    // and has no cap: several machines may prefer one account, and one machine any number.
    let accounts = &projections[ACCOUNT.name];
    for (pk, row) in projections[PREFERENCE.name]
        .rows
        .iter()
        .filter(|(_, r)| r.live())
    {
        // The key derives from the pair, so a row whose cells say another pair says two mappings.
        if let (Some(machine), Some(account)) = (text(row, "machine"), text(row, "account"))
            && keys::preference_pk(&machine, &account).ok().as_deref() != Some(pk.as_str())
        {
            found.push(Finding {
                kind: "preference-key-mismatch",
                collection: PREFERENCE.name,
                pks: vec![pk.clone()],
            });
        }
        if let Some(machine) = text(row, "machine")
            && machines.rows.contains_key(&machine)
            && !live(machines, &machine)
        {
            found.push(Finding {
                kind: "preference-on-retired-machine",
                collection: PREFERENCE.name,
                pks: vec![pk.clone(), machine],
            });
        }
        if let Some(account) = text(row, "account")
            && accounts.rows.contains_key(&account)
            && !live(accounts, &account)
        {
            found.push(Finding {
                kind: "preference-for-retired-account",
                collection: PREFERENCE.name,
                pks: vec![pk.clone(), account],
            });
        }
    }
    // Accounts: a live account's login and identity are live addresses.
    for (pk, row) in projections[ACCOUNT.name]
        .rows
        .iter()
        .filter(|(_, r)| r.live())
    {
        if let Some(login) = text(row, "login")
            && addresses.rows.contains_key(&login)
            && !live(addresses, &login)
        {
            found.push(Finding {
                kind: "account-login-retired",
                collection: ACCOUNT.name,
                pks: vec![pk.clone(), login],
            });
        }
        if let Some(identity) = text(row, "identity")
            && addresses.rows.contains_key(&identity)
            && !live(addresses, &identity)
        {
            found.push(Finding {
                kind: "account-identity-retired",
                collection: ACCOUNT.name,
                pks: vec![pk.clone(), identity],
            });
        }
    }
    // Machines: a live label that is another live machine's alias names two Macs.
    for (pk, row) in machines.rows.iter().filter(|(_, r)| r.live()) {
        let Some(label) = text(row, "label") else {
            continue;
        };
        for (other, other_row) in machines.rows.iter().filter(|(o, r)| *o != pk && r.live()) {
            if other_row
                .fields
                .get("aliases")
                .is_some_and(|f| f.members().contains(label.as_str()))
            {
                found.push(Finding {
                    kind: "label-ambiguous",
                    collection: MACHINE.name,
                    pks: vec![pk.clone(), other.clone()],
                });
            }
        }
    }
    // Addresses: the owner's mailbox and the lead's guess disagree; an alias link leads nowhere
    // live, or back to itself.
    for (pk, row) in addresses.rows.iter().filter(|(_, r)| r.live()) {
        if let Some(target) = text(row, "alias_of")
            && addresses.rows.contains_key(&target)
            && !live(addresses, &target)
        {
            found.push(Finding {
                kind: "alias-of-retired",
                collection: ADDRESS.name,
                pks: vec![pk.clone(), target],
            });
        }
        if super::mail::alias_cycle(addresses, pk) {
            found.push(Finding {
                kind: "alias-cycle",
                collection: ADDRESS.name,
                pks: vec![pk.clone()],
            });
        }
        if let Some(mailbox) = text(row, "mailbox")
            && mailboxes.rows.contains_key(&mailbox)
            && !live(mailboxes, &mailbox)
        {
            found.push(Finding {
                kind: "address-mailbox-retired",
                collection: ADDRESS.name,
                pks: vec![pk.clone(), mailbox],
            });
        }
        if let Some(hint) = text(row, "mailbox_hint")
            && mailboxes.rows.contains_key(&hint)
            && !live(mailboxes, &hint)
        {
            found.push(Finding {
                kind: "address-mailbox-hint-retired",
                collection: ADDRESS.name,
                pks: vec![pk.clone(), hint],
            });
        }
        if let (Some(mailbox), Some(hint)) = (text(row, "mailbox"), text(row, "mailbox_hint"))
            && mailbox != hint
        {
            found.push(Finding {
                kind: "mailbox-guess-differs",
                collection: ADDRESS.name,
                pks: vec![pk.clone()],
            });
        }
    }
    found.sort();
    found.dedup();
    (found, generic)
}
