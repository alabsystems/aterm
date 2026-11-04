// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LabView (docs/DESIGN-alab-lab-model-2026-11-01.md §0, §7, step 11): what the harness reads of
//! the registry, projected once from merged cells and the facts rows, pure. Machines, accounts
//! and the live preferences between them; this Mac is the machine its own facts row binds. A
//! preference orders a pool and never limits one: nothing here answers "may this account run
//! here", only "which accounts does this Mac prefer, in what order", "which Macs prefer this
//! account" and "where do this address's codes arrive". Beside the preferences, the journal's
//! `place` rows ([`super::journal::place::Places`]) say what each Mac runs that it does not
//! prefer: [`LabView::holding`] ("which Macs hold A") and [`LabView::runs_on`] ("what runs on
//! m2") read both, and neither is a limit. No provider is called and nothing is written; the
//! caller caches the view and projects again only when the inventory moves.

#[cfg(unix)]
use super::journal::place::{Places, Why};
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

/// One live `place` row, by the machine its writer's facts row binds: `machine` runs `account`,
/// which it does not prefer, for `why`.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub machine: String,
    pub account: String,
    pub why: Why,
    pub since: u64,
}

/// The registry as the harness reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LabView {
    /// The machine this Mac's live facts row binds (its `merged_into` when it states one, since
    /// only a Mac's own facts row may rebind it, §4.6), when that row is live.
    pub me: Option<String>,
    /// Each facts row's Commons uuid (32 hex) to the machine it binds, the same way, where that
    /// machine row is live: the one path by which a journal row names a Mac (§6).
    pub bound: BTreeMap<String, String>,
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
        let bound: BTreeMap<String, String> = facts
            .rows
            .iter()
            .filter_map(|(uuid, row)| {
                let value = &row.value;
                let mac = value
                    .get("merged_into")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("mac").and_then(Value::as_str))?;
                machines
                    .get(mac)
                    .is_some_and(|machine| machine.live)
                    .then(|| (uuid.clone(), mac.to_owned()))
            })
            .collect();
        let me = me_uuid.and_then(|uuid| bound.get(uuid)).cloned();
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
            bound,
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

    /// The live place rows of `places` by the machine each writer's facts row binds (a row whose
    /// writer binds no live machine names no Mac and is left out), less any whose machine prefers
    /// the account too (its preference already says so), by account then machine label.
    #[cfg(unix)]
    pub fn placements(&self, places: &Places) -> Vec<Placement> {
        let mut out: Vec<Placement> = places
            .rows
            .iter()
            .filter_map(|row| {
                let machine = self.bound.get(&row.uuid)?;
                self.accounts.contains_key(&row.account).then_some(())?;
                let prefers = self
                    .preferences
                    .iter()
                    .any(|p| p.machine == *machine && p.account == row.account);
                (!prefers).then(|| Placement {
                    machine: machine.clone(),
                    account: row.account.clone(),
                    why: row.why,
                    since: row.since,
                })
            })
            .collect();
        out.sort_by_key(|p| {
            (
                p.account.clone(),
                label_number(self.machine_label(&p.machine)),
                p.machine.clone(),
            )
        });
        out.dedup_by(|a, b| a.account == b.account && a.machine == b.machine);
        out
    }

    /// Which Macs hold `account` (§7): the machines that prefer it, in [`Self::preferring`]'s
    /// order, then those whose live place rows say they run it. Neither list is a limit.
    #[cfg(unix)]
    pub fn holding(&self, account: &str, places: &Places) -> Vec<String> {
        let mut out: Vec<String> = self
            .preferring(account)
            .into_iter()
            .map(|p| p.machine.clone())
            .collect();
        for placed in self.placements(places) {
            if placed.account == account && !out.contains(&placed.machine) {
                out.push(placed.machine);
            }
        }
        out
    }

    /// What runs on `machine` (§7): its live preferences by rank, and its live place rows.
    #[cfg(unix)]
    pub fn runs_on(&self, machine: &str, places: &Places) -> (Vec<&Preference>, Vec<Placement>) {
        let placed = self
            .placements(places)
            .into_iter()
            .filter(|p| p.machine == machine)
            .collect();
        (self.preferred_by(machine).collect(), placed)
    }

    /// Where codes for address `pk` arrive (`mail::effective`), with this Mac's `routes`.
    pub fn mailbox(
        &self,
        pk: &str,
        routes: &dyn Fn(&str) -> Option<String>,
    ) -> Option<mail::Effective> {
        mail::effective(&self.addresses, &self.observed, routes, pk)
    }

    /// Every live address, lower-cased, whose codes the lab itself places in one mailbox, to
    /// that mailbox's key: the address's `mailbox` column, through `alias_of`, or a mailbox's own
    /// complete address list ([`mail::effective`] with no routes, a guess left out). What the
    /// harness's mail routes take before any `receives` pattern (step 11, "a route keyed by
    /// `mailbox`"), on a Mac that can name the mailbox's login.
    pub fn mailboxes(&self) -> BTreeMap<String, String> {
        self.addresses
            .rows
            .iter()
            .filter(|(_, row)| row.live())
            .filter_map(|(pk, row)| {
                let addr = text(row, "addr")?;
                let found = mail::effective(&self.addresses, &self.observed, &|_| None, pk)?;
                (found.how != mail::How::Hint).then(|| (addr.to_ascii_lowercase(), found.mailbox))
            })
            .collect()
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
