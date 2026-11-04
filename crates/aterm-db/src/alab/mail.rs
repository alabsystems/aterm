// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where an address's codes arrive (docs/DESIGN-alab-lab-model-2026-11-01.md §0, §6): the owner's
//! mailbox if set; else, for an alias, wherever the address it is linked with receives; else the
//! one mailbox whose latest complete `mailbox-obs` lists it; else, for a `+tag` address, the one
//! mailbox whose latest complete observation lists its live base address (a mailbox delivers a
//! `+tag` to its base, and the list is the mailbox saying it receives the base: the link the list
//! supplies); else, on a Mac that holds the admins-audience `ai-mailbox.toml`, its route; else the
//! lead's guess. Pure, over the registry's
//! projection, so every Mac reads one answer from the same merged rows. A mailbox's login never
//! appears here: its rows hold only keys.

use crate::typed::cell::Projection;
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// How an address's mailbox was decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// `address.mailbox`.
    Owner,
    /// One complete observation lists it.
    Observed,
    /// `ai-mailbox.toml`'s routes, on a Mac that holds them.
    Routed,
    /// `address.mailbox_hint`.
    Hint,
}

/// An address's effective mailbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    /// The mailbox's key.
    pub mailbox: String,
    pub how: How,
    /// The address rows followed through `alias_of` to reach it, nearest first.
    pub via: Vec<String>,
}

/// The mailboxes whose latest complete observation lists each address: `address pk → mailbox
/// pks`. More than one for an address decides nothing.
pub type Observed = BTreeMap<String, BTreeSet<String>>;

/// The observations in `mailbox-obs` rows (each `{mailbox, complete, addresses, at}` under key
/// `<mailbox>.<uuid32>`): per mailbox the complete row whose key prefix matches `mailbox` with the
/// latest claimed time, its smallest key breaking a tie, so every Mac picks one. An incomplete row
/// or one whose key names another mailbox says nothing.
pub fn observed<'a>(rows: impl IntoIterator<Item = (&'a str, &'a Value)>) -> Observed {
    let mut latest: BTreeMap<&str, (u64, &str, &Value)> = BTreeMap::new();
    for (key, row) in rows {
        let Some(mailbox) = row
            .get("mailbox")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if key
            .rsplit_once('.')
            .is_none_or(|(prefix, _)| prefix != mailbox)
        {
            continue;
        }
        if row.get("complete").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let Some(at) = row.get("at").and_then(Value::as_u64) else {
            continue;
        };
        let newer = latest.get(mailbox).is_none_or(|(held_at, held_key, _)| {
            (at, std::cmp::Reverse(key)) > (*held_at, std::cmp::Reverse(*held_key))
        });
        if newer {
            latest.insert(mailbox, (at, key, row));
        }
    }
    let mut out = Observed::new();
    for (mailbox, (_, _, row)) in latest {
        for address in row
            .get("addresses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            out.entry(address.to_owned())
                .or_default()
                .insert(mailbox.to_owned());
        }
    }
    out
}

fn column<'a>(addresses: &'a Projection, pk: &str, name: &str) -> Option<&'a str> {
    addresses
        .rows
        .get(pk)?
        .get(name)?
        .as_str()
        .filter(|s| !s.is_empty())
}

/// The next live address `pk` is linked with.
fn linked<'a>(addresses: &'a Projection, pk: &str) -> Option<&'a str> {
    let target = column(addresses, pk, "alias_of")?;
    addresses.rows.get(target).filter(|row| row.live())?;
    Some(target)
}

/// The address `address` is a `+tag` of, by the same rule `mailbox_routes.rs` reads a route.
pub(crate) fn untagged(address: &str) -> Option<String> {
    let (local, domain) = address.split_once('@')?;
    let (base, _) = local.split_once('+')?;
    (!base.is_empty()).then(|| format!("{base}@{domain}"))
}

/// The live base address of `pk`'s `+tag` and the one mailbox whose latest complete observation
/// lists that base: the alias link an address list supplies, `(base pk, mailbox pk)`. Only the
/// observation that supplies the link decides it, so a base two mailboxes list, or one only a
/// route or a guess places, links nothing and the tag keeps its own routes. Derived on every read
/// and never stored (§3.11), so the owner's and the importer's `alias_of` stay theirs alone.
pub fn listed_base(
    addresses: &Projection,
    observed: &Observed,
    pk: &str,
) -> Option<(String, String)> {
    let base = untagged(column(addresses, pk, "addr")?)?;
    let base = crate::typed::keys::address_pk(&base).ok()?;
    addresses.rows.get(&base).filter(|row| row.live())?;
    let [mailbox] = observed.get(&base)?.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    let mailbox = mailbox.clone();
    Some((base, mailbox))
}

/// Whether following `pk`'s alias links returns to an address already followed.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Follow",
        project = "aterm_db::alab::view::tests::follow_transition"
    )
)]
pub fn alias_cycle(addresses: &Projection, pk: &str) -> bool {
    let mut seen = BTreeSet::from([pk]);
    let mut at = pk;
    while let Some(next) = linked(addresses, at) {
        if !seen.insert(next) {
            return true;
        }
        at = next;
    }
    false
}

/// The effective mailbox of address `pk`. `routes` answers for an address on a Mac that holds
/// `ai-mailbox.toml`, and nothing elsewhere. A link to a retired or absent address, or a cycle,
/// is left and read past: `check` reports it.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Follow",
        project = "aterm_db::alab::view::tests::follow_transition"
    )
)]
pub fn effective(
    addresses: &Projection,
    observed: &Observed,
    routes: &dyn Fn(&str) -> Option<String>,
    pk: &str,
) -> Option<Effective> {
    follow(addresses, observed, routes, pk, &mut BTreeSet::new())
}

fn follow(
    addresses: &Projection,
    observed: &Observed,
    routes: &dyn Fn(&str) -> Option<String>,
    pk: &str,
    seen: &mut BTreeSet<String>,
) -> Option<Effective> {
    if !addresses
        .rows
        .get(pk)
        .is_some_and(crate::typed::cell::Row::live)
        || !seen.insert(pk.to_owned())
    {
        return None;
    }
    let found = |mailbox: &str, how| {
        Some(Effective {
            mailbox: mailbox.to_owned(),
            how,
            via: Vec::new(),
        })
    };
    if let Some(mailbox) = column(addresses, pk, "mailbox") {
        return found(mailbox, How::Owner);
    }
    if let Some(target) = linked(addresses, pk)
        && let Some(mut reached) = follow(addresses, observed, routes, target, seen)
    {
        reached.via.insert(0, target.to_owned());
        return Some(reached);
    }
    if let Some(mailboxes) = observed.get(pk)
        && let [mailbox] = mailboxes.iter().collect::<Vec<_>>().as_slice()
    {
        return found(mailbox, How::Observed);
    }
    if let Some((base, mailbox)) = listed_base(addresses, observed, pk) {
        return Some(Effective {
            mailbox,
            how: How::Observed,
            via: vec![base],
        });
    }
    if let Some(mailbox) = routes(pk).filter(|s| !s.is_empty()) {
        return found(&mailbox, How::Routed);
    }
    column(addresses, pk, "mailbox_hint").and_then(|mailbox| found(mailbox, How::Hint))
}

#[cfg(test)]
mod tests;
