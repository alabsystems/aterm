// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db alab check` (docs/DESIGN-alab-lab-model-2026-11-01.md §4.15, §6): the lab's own
//! rules over the registry's converged rows, on top of the generic reference, unique and
//! conflict findings (`typed::check`). Per-cell merge can break a rule no single edit broke, so
//! readers report it and nothing repairs it silently. A finding names collections, keys and
//! columns, never a value.

use super::registry::{ACCOUNT, ADDRESS, COLLECTIONS, MACHINE, SEAT};
use crate::typed::{
    Kind,
    cell::{self, Cells, Projection},
    check,
};
use aterm_json::Value;
use std::collections::BTreeMap;

/// The most live seats one address holds (`ai_pool.rs` refuses a warm set past 3).
pub const MAX_SEATS: usize = 3;

/// One lab finding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    /// Its word.
    pub kind: &'static str,
    pub collection: &'static str,
    /// The keys it concerns.
    pub pks: Vec<String>,
}

/// The lab rules over `cells`, then every generic finding.
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
    let live = |p: &Projection, pk: &str| p.rows.get(pk).is_some_and(cell::Row::live);
    let text =
        |row: &cell::Row, column: &str| row.get(column).and_then(Value::as_str).map(str::to_owned);
    // Seats: per address, at most one live home seat and at most three live seats; no live seat
    // on a machine or for an address that is no longer live.
    let mut by_address: BTreeMap<String, Vec<(String, Option<String>)>> = BTreeMap::new();
    for (pk, row) in projections[SEAT.name].rows.iter().filter(|(_, r)| r.live()) {
        let machine = pk
            .strip_prefix("st-")
            .and_then(|rest| rest.rsplit_once('-'))
            .map(|(hex, _)| format!("mc-{hex}"));
        if let Some(machine) = machine
            && machines.rows.contains_key(&machine)
            && !live(machines, &machine)
        {
            found.push(Finding {
                kind: "seat-on-retired-machine",
                collection: SEAT.name,
                pks: vec![pk.clone(), machine],
            });
        }
        if let Some(address) = text(row, "address") {
            if addresses.rows.contains_key(&address) && !live(addresses, &address) {
                found.push(Finding {
                    kind: "seat-for-retired-address",
                    collection: SEAT.name,
                    pks: vec![pk.clone(), address.clone()],
                });
            }
            by_address
                .entry(address)
                .or_default()
                .push((pk.clone(), text(row, "role")));
        }
    }
    for (address, seats) in by_address {
        let homes: Vec<String> = seats
            .iter()
            .filter(|(_, role)| role.as_deref() == Some("home"))
            .map(|(pk, _)| pk.clone())
            .collect();
        if homes.len() > 1 {
            found.push(Finding {
                kind: "two-home-seats",
                collection: SEAT.name,
                pks: [vec![address.clone()], homes].concat(),
            });
        }
        if seats.len() > MAX_SEATS {
            found.push(Finding {
                kind: "too-many-seats",
                collection: SEAT.name,
                pks: [vec![address], seats.into_iter().map(|(pk, _)| pk).collect()].concat(),
            });
        }
    }
    // Accounts: a live account's login is a live address.
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
    // Addresses: the owner's mailbox and the lead's guess disagree.
    for (pk, row) in addresses.rows.iter().filter(|(_, r)| r.live()) {
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
