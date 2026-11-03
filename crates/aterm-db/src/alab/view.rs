// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LabView (docs/DESIGN-alab-lab-model-2026-11-01.md §0, §7, step 11): what the harness reads of
//! the registry, projected once from merged cells and the facts rows, pure. Machines, accounts
//! and the live preferences between them; this Mac is the machine its own facts row binds. A
//! preference orders a pool and never limits one: nothing here answers "may this account run
//! here", only "which accounts does this Mac prefer, in what order", "which Macs prefer this
//! account" and "where do this address's codes arrive". No provider is called and nothing is
//! written; the caller caches the view and projects again only when the inventory moves.

use super::{
    mail,
    registry::{self, ACCOUNT, ADDRESS, MACHINE, PREFERENCE},
};
use crate::typed::{
    cell::{self, Cells, Projection},
    owned::OwnedRead,
};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// A machine row, by its key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub pk: String,
    /// `m22`; `None` while the label is conflicted.
    pub label: Option<String>,
    pub aliases: Vec<String>,
    pub live: bool,
}

/// An account row, by the canonical account key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub pk: String,
    /// The canonical realm (`claude`, `codex`, `kimi:global`, …).
    pub realm: Option<String>,
    /// What the row is called: the owner's or the import's label, else its default
    /// ([`registry::account_label`]). It names no machine.
    pub label: Option<String>,
    /// What a Mac called the row before labels named no machine (`m8-2`, `kimi-m16`).
    pub legacy_label: Option<String>,
    /// The login address, local to this process: the registry types it `email`, so it is
    /// shown masked and never printed by this view.
    pub login: Option<String>,
    /// The provider identity, when the login is an alias.
    pub identity: Option<String>,
    pub status: Option<String>,
    pub region: Option<String>,
    pub flow: Option<String>,
    pub browser: Option<String>,
    pub live: bool,
}

impl Account {
    /// Whether a roster may gain a row for it unasked: a live row whose status lets a sign-in be
    /// tried. A paused, broken or banned account keeps any row a Mac already has, and gains none.
    pub fn joinable(&self) -> bool {
        self.live
            && self
                .status
                .as_deref()
                .is_none_or(|status| matches!(status, "pending" | "active"))
    }
}

/// One live preference: `machine` prefers `account`, smaller `rank` first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preference {
    pub machine: String,
    pub account: String,
    pub rank: Option<u64>,
}

/// The registry as the harness reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LabView {
    /// The machine this Mac's live facts row binds (its `merged_into` when it states one, since
    /// only a Mac's own facts row may rebind it, §4.6), when that row is live.
    pub me: Option<String>,
    pub machines: BTreeMap<String, Machine>,
    pub accounts: BTreeMap<String, Account>,
    /// Live preferences on live machines, by machine key then rank.
    pub preferences: Vec<Preference>,
    addresses: Projection,
    observed: mail::Observed,
}

fn text(row: &cell::Row, column: &str) -> Option<String> {
    row.get(column).and_then(Value::as_str).map(str::to_owned)
}

/// The ordering key of a machine label's number, so `m3` sorts before `m22`.
fn label_number(label: Option<&str>) -> u64 {
    label
        .and_then(|label| label.strip_prefix('m'))
        .map(|rest| {
            rest.bytes()
                .take_while(u8::is_ascii_digit)
                .collect::<Vec<_>>()
        })
        .and_then(|digits| std::str::from_utf8(&digits).ok()?.parse().ok())
        .unwrap_or(u64::MAX)
}

impl LabView {
    /// Project `cells`, this Mac's facts row among `facts` at `me_uuid` (32 hex), and the
    /// mailbox observations `obs`.
    pub fn read(cells: &Cells, facts: &OwnedRead, obs: &OwnedRead, me_uuid: Option<&str>) -> Self {
        let machines: BTreeMap<String, Machine> = cell::project(&MACHINE, cells)
            .rows
            .into_iter()
            .map(|(pk, row)| {
                let aliases = row
                    .get("aliases")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let machine = Machine {
                    pk: pk.clone(),
                    label: text(&row, "label"),
                    aliases,
                    live: row.live(),
                };
                (pk, machine)
            })
            .collect();
        let addresses = cell::project(&ADDRESS, cells);
        let address = |pk: Option<String>| {
            let row = addresses.rows.get(&pk?)?;
            text(row, "addr")
        };
        let accounts = cell::project(&ACCOUNT, cells)
            .rows
            .into_iter()
            .map(|(pk, row)| {
                let account = Account {
                    label: registry::account_label(&pk, &row),
                    realm: text(&row, "realm"),
                    legacy_label: text(&row, "legacy_label"),
                    login: address(text(&row, "login")),
                    identity: address(text(&row, "identity")),
                    status: text(&row, "status"),
                    region: text(&row, "region"),
                    flow: text(&row, "flow"),
                    browser: text(&row, "browser"),
                    live: row.live(),
                    pk: pk.clone(),
                };
                (pk, account)
            })
            .collect::<BTreeMap<_, _>>();
        let me = me_uuid
            .and_then(|uuid| facts.rows.get(uuid))
            .and_then(|row| {
                let value = &row.value;
                value
                    .get("merged_into")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("mac").and_then(Value::as_str))
                    .map(str::to_owned)
            })
            .filter(|mac| machines.get(mac).is_some_and(|machine| machine.live));
        let mut preferences: Vec<Preference> = cell::project(&PREFERENCE, cells)
            .rows
            .into_iter()
            .filter(|(_, row)| row.live())
            .filter_map(|(_, row)| {
                let machine = text(&row, "machine")?;
                let account = text(&row, "account")?;
                (machines.get(&machine).is_some_and(|m| m.live) && accounts.contains_key(&account))
                    .then(|| Preference {
                        machine,
                        account,
                        rank: row.get("rank").and_then(Value::as_u64),
                    })
            })
            .collect();
        preferences.sort_by(|a, b| {
            (&a.machine, a.rank.is_none(), a.rank, &a.account).cmp(&(
                &b.machine,
                b.rank.is_none(),
                b.rank,
                &b.account,
            ))
        });
        let observed = mail::observed(obs.rows.iter().map(|(key, row)| (key.as_str(), &row.value)));
        Self {
            me,
            machines,
            accounts,
            preferences,
            addresses,
            observed,
        }
    }

    /// The label of machine `pk`.
    pub fn machine_label(&self, pk: &str) -> Option<&str> {
        self.machines.get(pk)?.label.as_deref()
    }

    /// This Mac's label.
    pub fn my_label(&self) -> Option<&str> {
        self.machine_label(self.me.as_deref()?)
    }

    /// The accounts `machine` prefers, smaller rank first and an unranked one last.
    pub fn preferred_by(&self, machine: &str) -> impl Iterator<Item = &Preference> {
        self.preferences
            .iter()
            .filter(move |p| p.machine == machine)
    }

    /// The machines that prefer `account`, by rank, then by the smaller label number: the
    /// tie-break `ai-pool.toml`'s warm list is truncated by (§9).
    pub fn preferring(&self, account: &str) -> Vec<&Preference> {
        let mut rows: Vec<&Preference> = self
            .preferences
            .iter()
            .filter(|p| p.account == account)
            .collect();
        rows.sort_by_key(|p| {
            (
                p.rank.is_none(),
                p.rank,
                label_number(self.machine_label(&p.machine)),
                p.machine.clone(),
            )
        });
        rows
    }

    /// Where codes for address `pk` arrive (`mail::effective`), with this Mac's `routes`.
    pub fn mailbox(
        &self,
        pk: &str,
        routes: &dyn Fn(&str) -> Option<String>,
    ) -> Option<mail::Effective> {
        mail::effective(&self.addresses, &self.observed, routes, pk)
    }

    /// Every address with an `alias_of` link, lower-cased, to [`Self::alias_targets`] of it.
    pub fn aliases(&self) -> BTreeMap<String, Vec<String>> {
        self.addresses
            .rows
            .values()
            .filter(|row| row.live() && row.get("alias_of").is_some())
            .filter_map(|row| text(row, "addr"))
            .map(|addr| {
                let targets = self.alias_targets(&addr);
                (addr.to_ascii_lowercase(), targets)
            })
            .filter(|(_, targets)| !targets.is_empty())
            .collect()
    }

    /// The live addresses `address` is an alias of, nearest first, following `alias_of` (§0):
    /// the addresses whose mail routes receive its auth codes. Stops at a retired link or a cycle.
    pub fn alias_targets(&self, address: &str) -> Vec<String> {
        let Ok(mut at) = crate::typed::keys::address_pk(address) else {
            return Vec::new();
        };
        let mut seen = BTreeSet::from([at.clone()]);
        let mut out = Vec::new();
        while let Some(next) = self
            .addresses
            .rows
            .get(&at)
            .and_then(|row| text(row, "alias_of"))
        {
            let Some(row) = self.addresses.rows.get(&next).filter(|row| row.live()) else {
                break;
            };
            if !seen.insert(next.clone()) {
                break;
            }
            if let Some(addr) = text(row, "addr") {
                out.push(addr);
            }
            at = next;
        }
        out
    }
}

#[cfg(test)]
mod tests;
