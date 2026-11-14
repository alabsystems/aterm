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
    registry::{self, ACCOUNT, ADDRESS, AddressIndex, MACHINE, PREFERENCE},
};
use crate::typed::{
    cell::{self, Cells, Projection},
    keys,
    owned::OwnedRead,
};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The registry nickname's value and provenance, without applying terminal naming grammar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MachineNickname {
    /// The cell is missing or explicitly null.
    #[default]
    Unset,
    /// One value, with its original spelling and automatic-write provenance.
    Named { word: String, automatic: bool },
    /// Concurrent values conflict, or the field cannot be interpreted as nullable text.
    Conflict,
}

/// A machine row, by its key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub pk: String,
    /// `m22`; `None` while the label is conflicted or unreadable.
    pub label: Option<String>,
    /// The nickname's text, automatic-write provenance and conflict state.
    pub nickname: MachineNickname,
    pub aliases: Vec<String>,
    /// The registry's status word (`planned`, `active`, `parked`, `lost`).
    pub status: Option<String>,
    /// The operating system the registry records for it.
    pub os: Option<String>,
    /// The chip the registry records for it.
    pub chip: Option<String>,
    /// The memory the registry records for it, in GB.
    pub ram_gb: Option<u64>,
    pub live: bool,
}

/// An account row, by the canonical account key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub pk: String,
    /// The provider's key (`anthropic`, `openai`, …).
    pub provider: Option<String>,
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
    /// The plan's key, where the registry names one.
    pub plan: Option<String>,
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
    /// `addresses` by what each row's `addr` says now: how an address names its row.
    address_index: AddressIndex,
    observed: mail::Observed,
}

fn text(row: &cell::Row, column: &str) -> Option<String> {
    row.get(column).and_then(Value::as_str).map(str::to_owned)
}

fn unconflicted_text(row: &cell::Row, column: &str) -> Option<String> {
    if row.raw.contains(column) || row.fields.get(column)?.conflicted() {
        return None;
    }
    text(row, column)
}

fn machine_nickname(row: &cell::Row) -> MachineNickname {
    if row.raw.contains("nickname") {
        return MachineNickname::Conflict;
    }
    match row.fields.get("nickname") {
        None => MachineNickname::Unset,
        Some(cell::Field::Value(candidate)) => match &candidate.value.x {
            Value::Null => MachineNickname::Unset,
            Value::String(word) => MachineNickname::Named {
                word: word.clone(),
                automatic: candidate.value.a,
            },
            _ => MachineNickname::Conflict,
        },
        Some(_) => MachineNickname::Conflict,
    }
}

/// The ordering key of a machine label's number, so `m3` sorts before `m22`.
fn label_number(label: Option<&str>) -> u64 {
    label
        .and_then(|label| label.strip_prefix('m'))
        .map(|rest| {
            let len = rest.bytes().take_while(u8::is_ascii_digit).count();
            &rest[..len]
        })
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(u64::MAX)
}

/// By machine key, then smaller rank first and an unranked one last, then account.
fn sort_preferences(preferences: &mut [Preference]) {
    preferences.sort_by(|a, b| {
        (&a.machine, a.rank.is_none(), a.rank, &a.account).cmp(&(
            &b.machine,
            b.rank.is_none(),
            b.rank,
            &b.account,
        ))
    });
}

impl LabView {
    /// Read canonical identities from the authenticated registry view.
    pub fn read_view(
        view: &crate::typed::View,
        facts: &OwnedRead,
        obs: &OwnedRead,
        me_uuid: Option<&str>,
    ) -> Self {
        Self::read_projected(&view.cells, view.project(&MACHINE), facts, obs, me_uuid)
    }

    /// Project `cells`, this Mac's facts row among `facts` at `me_uuid` (32 hex), and the
    /// mailbox observations `obs`.
    #[cfg(test)]
    pub(crate) fn read(
        cells: &Cells,
        facts: &OwnedRead,
        obs: &OwnedRead,
        me_uuid: Option<&str>,
    ) -> Self {
        Self::read_projected(cells, cell::project(&MACHINE, cells), facts, obs, me_uuid)
    }

    fn read_projected(
        cells: &Cells,
        machines: cell::Projection,
        facts: &OwnedRead,
        obs: &OwnedRead,
        me_uuid: Option<&str>,
    ) -> Self {
        let machines: BTreeMap<String, Machine> = machines
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
                    label: unconflicted_text(&row, "label"),
                    nickname: machine_nickname(&row),
                    aliases,
                    status: text(&row, "status"),
                    os: text(&row, "os"),
                    chip: text(&row, "chip"),
                    ram_gb: row.get("ram_gb").and_then(Value::as_u64),
                    live: row.live(),
                };
                (pk, machine)
            })
            .collect();
        let addresses = cell::project(&ADDRESS, cells);
        let address = |pk: Option<&str>| {
            let row = addresses.rows.get(pk?)?;
            text(row, "addr")
        };
        let accounts = cell::project(&ACCOUNT, cells)
            .rows
            .into_iter()
            .map(|(pk, row)| {
                let account = Account {
                    label: registry::account_label(&pk, &row),
                    provider: text(&row, "provider"),
                    realm: text(&row, "realm"),
                    legacy_label: text(&row, "legacy_label"),
                    login: address(row.get("login").and_then(Value::as_str)),
                    identity: address(row.get("identity").and_then(Value::as_str)),
                    status: text(&row, "status"),
                    plan: text(&row, "plan"),
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
                let mac = registry::bound_machine(&row.value)?;
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
                let machine = row.get("machine").and_then(Value::as_str)?;
                let account = row.get("account").and_then(Value::as_str)?;
                (machines.get(machine).is_some_and(|m| m.live) && accounts.contains_key(account))
                    .then(|| Preference {
                        machine: machine.to_owned(),
                        account: account.to_owned(),
                        rank: row.get("rank").and_then(Value::as_u64),
                    })
            })
            .collect();
        sort_preferences(&mut preferences);
        let observed = mail::observed(obs.rows.iter().map(|(key, row)| (key.as_str(), &row.value)));
        let address_index = AddressIndex::of(&addresses);
        Self {
            me,
            bound,
            machines,
            accounts,
            preferences,
            addresses,
            address_index,
            observed,
        }
    }

    /// The view the shared files `ai/accounts`, `ai/links` and `lab/machines` hold
    /// (docs/DESIGN-shared-files-2026-11-11.md §4, SF-04): what [`Self::read`] projects from the
    /// registry, read from this Mac's snapshots, which answer in every custody state. This Mac is
    /// the live machine the snapshot marks `=here` (the one its own facts row binds when the pass
    /// ran), and each seat of `=seats` is bound as [`Self::bound`] says. A label or nickname whose
    /// cell is a conflict, or one the guards refused, is read as [`Self::read`] reads a conflicted
    /// register. The snapshots carry no address rows: [`Self::mailbox`], [`Self::mailboxes`] and
    /// [`Self::aliases`] answer nothing here, and mail metadata is read from the registry.
    #[must_use]
    pub fn of_files(
        accounts: &aterm_file::Read,
        links: &aterm_file::Read,
        machines: &aterm_file::Read,
    ) -> Self {
        let value = |row: &aterm_file::Row, column: &str| {
            row.values
                .get(column)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        // A register's own value: the cell shows exactly the value, with no conflict and no
        // refusal in its place.
        let unconflicted = |row: &aterm_file::Row, column: &str| {
            value(row, column).filter(|text| row.cell(column) == text)
        };
        let shown = |row: &aterm_file::Row, column: &str| {
            Some(row.cell(column).to_owned()).filter(|cell| !cell.is_empty())
        };
        let yes = |row: &aterm_file::Row, column: &str| row.cell(column) == "yes";
        let machine_rows: BTreeMap<String, Machine> = machines
            .rows()
            .iter()
            .map(|row| {
                let nickname = if row.cell("nickname").is_empty() {
                    MachineNickname::Unset
                } else if let Some(word) = unconflicted(row, "nickname") {
                    MachineNickname::Named {
                        word,
                        automatic: yes(row, "nickname_auto"),
                    }
                } else {
                    MachineNickname::Conflict
                };
                let aliases = row
                    .values
                    .get("aliases")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let machine = Machine {
                    pk: row.pk.clone(),
                    label: unconflicted(row, "label"),
                    nickname,
                    aliases,
                    status: value(row, "status"),
                    os: value(row, "os"),
                    chip: value(row, "chip"),
                    ram_gb: row.values.get("ram_gb").and_then(Value::as_u64),
                    live: row.live,
                };
                (row.pk.clone(), machine)
            })
            .collect();
        let live = |pk: &str| machine_rows.get(pk).is_some_and(|machine| machine.live);
        let mut bound = BTreeMap::new();
        let mut me = None;
        for row in machines.rows().iter().filter(|row| live(&row.pk)) {
            for uuid in row
                .cell("seats")
                .split(", ")
                .filter(|uuid| !uuid.is_empty())
            {
                bound.insert(uuid.to_owned(), row.pk.clone());
            }
            if yes(row, "here") {
                me = Some(row.pk.clone());
            }
        }
        let account_rows: BTreeMap<String, Account> = accounts
            .rows()
            .iter()
            .map(|row| {
                let pk = row.pk.clone();
                let realm = value(row, "realm");
                let account = Account {
                    label: value(row, "label")
                        .or_else(|| keys::default_account_label(realm.as_deref()?, &pk).ok()),
                    legacy_label: value(row, "legacy_label"),
                    provider: value(row, "provider"),
                    plan: value(row, "plan"),
                    login: shown(row, "login_email"),
                    // `=email` is the identity's address where one is set.
                    identity: if row.cell("identity").is_empty() {
                        None
                    } else {
                        shown(row, "email")
                    },
                    status: value(row, "status"),
                    region: value(row, "region"),
                    flow: value(row, "flow"),
                    browser: value(row, "browser"),
                    live: row.live,
                    realm,
                    pk: pk.clone(),
                };
                (pk, account)
            })
            .collect();
        let mut preferences: Vec<Preference> = links
            .rows()
            .iter()
            .filter(|row| row.live)
            .filter_map(|row| {
                let machine = value(row, "machine")?;
                let account = value(row, "account")?;
                (live(&machine) && account_rows.contains_key(&account)).then(|| Preference {
                    rank: row.values.get("rank").and_then(Value::as_u64),
                    machine,
                    account,
                })
            })
            .collect();
        sort_preferences(&mut preferences);
        Self {
            me,
            bound,
            machines: machine_rows,
            accounts: account_rows,
            preferences,
            ..Self::default()
        }
    }

    /// The label of machine `pk`.
    pub fn machine_label(&self, pk: &str) -> Option<&str> {
        self.machines.get(pk)?.label.as_deref()
    }

    /// This Mac's label.
    pub fn my_label(&self) -> Option<&str> {
        self.my_machine()?.label.as_deref()
    }

    /// The live machine row this Mac's own facts bind, when one is available. Labels and
    /// nicknames are kept as projected: naming policy belongs to the caller.
    pub fn my_machine(&self) -> Option<&Machine> {
        self.machines
            .get(self.me.as_deref()?)
            .filter(|machine| machine.live)
    }

    /// The accounts `machine` prefers, smaller rank first and an unranked one last.
    pub fn preferred_by(&self, machine: &str) -> impl Iterator<Item = &Preference> {
        let start = self
            .preferences
            .partition_point(|p| p.machine.as_str() < machine);
        self.preferences[start..]
            .iter()
            .take_while(move |p| p.machine == machine)
    }

    /// The machines that prefer `account`, by rank, then by the smaller label number: the
    /// tie-break `ai-pool.toml`'s warm list is truncated by (§9).
    pub fn preferring(&self, account: &str) -> Vec<&Preference> {
        let mut rows: Vec<&Preference> = self
            .preferences
            .iter()
            .filter(|p| p.account == account)
            .collect();
        rows.sort_by(|a, b| {
            (
                a.rank.is_none(),
                a.rank,
                label_number(self.machine_label(&a.machine)),
                &a.machine,
            )
                .cmp(&(
                    b.rank.is_none(),
                    b.rank,
                    label_number(self.machine_label(&b.machine)),
                    &b.machine,
                ))
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
                let prefers = self.preferred_by(machine).any(|p| p.account == row.account);
                (!prefers).then(|| Placement {
                    machine: machine.clone(),
                    account: row.account.clone(),
                    why: row.why,
                    since: row.since,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            (
                &a.account,
                label_number(self.machine_label(&a.machine)),
                &a.machine,
            )
                .cmp(&(
                    &b.account,
                    label_number(self.machine_label(&b.machine)),
                    &b.machine,
                ))
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
        if self.accounts.contains_key(account) {
            let mut placed: Vec<&String> = places
                .rows
                .iter()
                .filter(|row| row.account == account)
                .filter_map(|row| self.bound.get(&row.uuid))
                .filter(|machine| !out.contains(machine))
                .collect();
            placed.sort_by(|a, b| {
                (label_number(self.machine_label(a)), *a)
                    .cmp(&(label_number(self.machine_label(b)), *b))
            });
            placed.dedup();
            out.extend(placed.into_iter().cloned());
        }
        out
    }

    /// What runs on `machine` (§7): its live preferences by rank, and its live place rows.
    #[cfg(unix)]
    pub fn runs_on(&self, machine: &str, places: &Places) -> (Vec<&Preference>, Vec<Placement>) {
        let preferred: Vec<&Preference> = self.preferred_by(machine).collect();
        let mut placed: Vec<Placement> = places
            .rows
            .iter()
            .filter(|row| self.bound.get(&row.uuid).is_some_and(|m| m == machine))
            .filter(|row| {
                self.accounts.contains_key(&row.account)
                    && !preferred.iter().any(|p| p.account == row.account)
            })
            .map(|row| Placement {
                machine: machine.to_owned(),
                account: row.account.clone(),
                why: row.why,
                since: row.since,
            })
            .collect();
        placed.sort_by(|a, b| a.account.cmp(&b.account));
        placed.dedup_by(|a, b| a.account == b.account);
        (preferred, placed)
    }

    /// Where codes for address `pk` arrive (`mail::effective`), with this Mac's `routes`.
    pub fn mailbox(
        &self,
        pk: &str,
        routes: &dyn Fn(&str) -> Option<String>,
    ) -> Option<mail::Effective> {
        mail::effective(
            &self.addresses,
            &self.address_index,
            &self.observed,
            routes,
            pk,
        )
    }

    /// Every live address, lower-cased, whose codes the lab itself places in one mailbox, to
    /// that mailbox's key: the address's `mailbox` column, through `alias_of`, or a mailbox's own
    /// complete address list ([`mail::effective`] with no routes, a guess left out). What the
    /// harness's mail routes take before any `receives` pattern (step 11, "a route keyed by
    /// `mailbox`"), on a Mac that can name the mailbox's login. An address is placed only by the
    /// one row it names ([`AddressIndex::row`]), so an address several rows say is placed nowhere.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FleetMachineAssignment",
            action = "Follow",
            project = "aterm_db::alab::view::tests::follow_transition"
        )
    )]
    pub fn mailboxes(&self) -> BTreeMap<String, String> {
        self.addresses
            .rows
            .iter()
            .filter(|(_, row)| row.live())
            .filter_map(|(pk, row)| {
                let addr = row.get("addr").and_then(Value::as_str)?;
                (self.address_index.row(addr) == Some(pk.as_str())).then_some(())?;
                let found = mail::effective(
                    &self.addresses,
                    &self.address_index,
                    &self.observed,
                    &|_| None,
                    pk,
                )?;
                (found.how != mail::How::Hint).then(|| (addr.to_ascii_lowercase(), found.mailbox))
            })
            .collect()
    }

    /// Every address with an `alias_of` link, lower-cased, to the live addresses its row is an
    /// alias of, as [`Self::alias_targets`] reads them: only from the one row the address names
    /// ([`AddressIndex::row`]), so an address several rows say routes nowhere, as
    /// [`Self::mailboxes`] places it nowhere.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FleetMachineAssignment",
            action = "Follow",
            project = "aterm_db::alab::view::tests::follow_transition"
        )
    )]
    pub fn aliases(&self) -> BTreeMap<String, Vec<String>> {
        self.addresses
            .rows
            .iter()
            .filter(|(_, row)| row.live() && row.get("alias_of").is_some())
            .filter_map(|(pk, row)| Some((pk, row.get("addr").and_then(Value::as_str)?)))
            .filter(|(pk, addr)| self.address_index.row(addr) == Some(pk.as_str()))
            .filter_map(|(pk, addr)| {
                let targets = self.alias_targets_of(pk);
                (!targets.is_empty()).then(|| (addr.to_ascii_lowercase(), targets))
            })
            .collect()
    }

    /// The live addresses `address` is an alias of, nearest first, following `alias_of` (§0):
    /// the addresses whose mail routes receive its auth codes, from the one row whose `addr`
    /// says `address` now ([`AddressIndex::row`]). Stops at a retired link or a cycle.
    pub fn alias_targets(&self, address: &str) -> Vec<String> {
        self.address_index
            .row(address)
            .map(|pk| self.alias_targets_of(pk))
            .unwrap_or_default()
    }

    /// [`Self::alias_targets`] from row `pk`.
    fn alias_targets_of(&self, pk: &str) -> Vec<String> {
        let mut at = pk;
        let mut seen = BTreeSet::from([pk]);
        let mut out = Vec::new();
        while let Some(next) = self
            .addresses
            .rows
            .get(at)
            .and_then(|row| row.get("alias_of"))
            .and_then(Value::as_str)
        {
            let Some((next_key, row)) = self
                .addresses
                .rows
                .get_key_value(next)
                .filter(|(_, row)| row.live())
            else {
                break;
            };
            if !seen.insert(next_key.as_str()) {
                break;
            }
            if let Some(addr) = text(row, "addr") {
                out.push(addr);
            }
            at = next_key.as_str();
        }
        out
    }
}

#[cfg(test)]
mod tests;
