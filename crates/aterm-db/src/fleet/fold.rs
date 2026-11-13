// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet tables' one join: [`fold`] reads what four readers offered (the registry's
//! LabView, the journal window, the network observer and the fleet reads) and gives the
//! machines, accounts and agents relations. It is pure: it reads no file, asks no machine and
//! no provider, and the same inputs at the same instant fold to the same rows.
//!
//! - **Machines**: every registry row in the order of its label's number, then every machine
//!   that answered a read and that no registry row names (`unregistered`, last, never dropped).
//!   A read's answer finds its row by the `m<N>` its header names, else by the number its host
//!   name starts with; an observer's peer and a journal row by the Commons uuid a facts row
//!   binds; this Mac's own answer finds the row its facts row binds, whatever its header says.
//!   Where two sightings name one machine, this Mac's own answer stands over a peer's and a
//!   newer one over an older, and the rest are counted ([`Counts::shadowed`]). A read says
//!   whether a machine answers for [`REACH_KEEP_MS`] and no longer.
//! - **Accounts**: every registry row by provider then label. Usage is the journal's reading,
//!   read again as of the fold's instant: a window whose reset has passed has no figure.
//!   `expected` comes from the registry's preferences and from nothing else; `active` is
//!   evidence, each piece with the word of what gave it, and is stored nowhere. An agent an
//!   unproven answer listed is no evidence.
//! - **Agents**: every agent session a machine listed in an answer no older than
//!   [`AGENTS_KEEP_MS`]. Its account is the registry row its word resolves to; a word that
//!   resolves to none is dropped and counted ([`Counts::unresolved_accounts`]).

use super::read::{AccountWord, Harness, Latest, OWN_ID, Sighted};
use super::rows::{
    ACCOUNT_STATUS, AccountRef, AccountRow, AgentRow, Evidence, EvidenceHow, MACHINE_STATUS,
    MachineRef, MachineRow, RETIRED, Reach, UNREGISTERED, Via, WindowRow, WindowState,
};
use crate::alab::{
    journal::{
        event::Window,
        seen::{Seen, Usage, WindowQuota},
    },
    view::LabView,
};
use crate::fleet::read::At;
use crate::typed::keys;
use aterm_codec::hex;
use aterm_types::machine_name::Number;
use aterm_types::network_state::{NetworkStateSnapshot, PeerObservation, Reachability};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// How long a machine's last answer speaks for its agents: past it the machine has no agent
/// counts and no agent rows. Its agents are then unknown, not idle and not gone.
pub const AGENTS_KEEP_MS: u64 = 2 * 3_600_000;

/// How long a fleet read says whether a machine answers: five of the glance's one-minute reads.
/// Past it a machine the read heard is `unread`, not `up`, and one it found silent is `unread`,
/// not `silent`: nothing current says either way. The fleet reads' input is then held as
/// [`super::HELD_STALE`].
pub const REACH_KEEP_MS: u64 = 300_000;

/// One input of the fold, as its reader left it.
#[derive(Clone, Debug, Default)]
pub enum Source<T> {
    /// Its reader has not offered anything yet.
    #[default]
    Never,
    /// It is turned off on this Mac.
    Off,
    /// The reader's last read, and when it was made (Unix milliseconds).
    Live { value: T, at_ms: u64 },
    /// The reader could not read, or read a part only: `why` is its closed word, and `last`
    /// what the rows still show of it, with the time that was read.
    Held {
        last: Option<(T, u64)>,
        why: &'static str,
    },
}

impl<T> Source<T> {
    /// What was read, live or held.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Live { value, .. } => Some(value),
            Self::Held { last, .. } => last.as_ref().map(|(value, _)| value),
            Self::Never | Self::Off => None,
        }
    }

    /// When [`Self::value`] was read.
    pub fn at_ms(&self) -> Option<u64> {
        match self {
            Self::Live { at_ms, .. } => Some(*at_ms),
            Self::Held { last, .. } => last.as_ref().map(|(_, at)| *at),
            Self::Never | Self::Off => None,
        }
    }

    /// Its state, without its value.
    pub fn state(&self) -> SourceState {
        match self {
            Self::Never => SourceState::Never,
            Self::Off => SourceState::Off,
            Self::Live { at_ms, .. } => SourceState::Live { at_ms: *at_ms },
            Self::Held { last, why } => SourceState::Held {
                why,
                at_ms: last.as_ref().map(|(_, at)| *at),
            },
        }
    }
}

impl<T: Clone> Source<T> {
    /// This source after a read that failed for `why`: what it held stays, marked held.
    #[must_use]
    pub fn held(&self, why: &'static str) -> Self {
        let last = match self {
            Self::Live { value, at_ms } => Some((value.clone(), *at_ms)),
            Self::Held { last, .. } => last.clone(),
            Self::Never | Self::Off => None,
        };
        Self::Held { last, why }
    }
}

/// What an input's reader last did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceState {
    /// Nothing offered yet.
    Never,
    /// Turned off here.
    Off,
    /// Read at `at_ms`.
    Live { at_ms: u64 },
    /// The last read failed, read a part only or is too old to speak for now, as `why` says;
    /// what the rows show is from `at_ms`, or from nothing.
    Held {
        why: &'static str,
        at_ms: Option<u64>,
    },
}

impl SourceState {
    /// `live`, `held`, `never` or `off`.
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Off => "off",
            Self::Live { .. } => "live",
            Self::Held { .. } => "held",
        }
    }

    /// When the rows' part of this input was read.
    pub const fn at_ms(&self) -> Option<u64> {
        match self {
            Self::Live { at_ms } => Some(*at_ms),
            Self::Held { at_ms, .. } => *at_ms,
            Self::Never | Self::Off => None,
        }
    }

    /// Why it is held.
    pub const fn why(&self) -> Option<&'static str> {
        match self {
            Self::Held { why, .. } => Some(why),
            _ => None,
        }
    }
}

/// Each input's state: an empty relation under an input that was never read is not an empty
/// fleet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sources {
    /// The registry: machines, accounts and preferences.
    pub lab: SourceState,
    /// The journal window: usage, claims, places, sign-ins.
    pub journal: SourceState,
    /// The Commons network observer.
    pub network: SourceState,
    /// The fleet reads.
    pub read: SourceState,
}

/// What the fold reads.
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    pub lab: Source<Arc<LabView>>,
    pub journal: Source<Arc<Seen>>,
    pub network: Source<Arc<NetworkStateSnapshot>>,
    pub read: Source<Arc<Latest>>,
}

/// What the fold left out of the rows, counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Agent rows whose account word names no registry account.
    pub unresolved_accounts: u32,
    /// Sightings that named a machine another sighting already stood for.
    pub shadowed: u32,
}

/// The fleet relations, as one fold made them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetTables {
    pub machines: Vec<MachineRow>,
    pub accounts: Vec<AccountRow>,
    pub agents: Vec<AgentRow>,
    pub sources: Sources,
    pub counts: Counts,
}

impl FleetTables {
    /// The tables before anything was offered: no rows, every input `never`. It is what
    /// folding nothing gives, made without a fold.
    pub fn unread() -> Self {
        Self {
            machines: Vec::new(),
            accounts: Vec::new(),
            agents: Vec::new(),
            sources: Sources {
                lab: SourceState::Never,
                journal: SourceState::Never,
                network: SourceState::Never,
                read: SourceState::Never,
            },
            counts: Counts::default(),
        }
    }
}

/// One fold: the tables, and the next instant they change with no new input (a window's reset
/// passes, a wall, a claim or a place ends, a read stops saying which machines answer, a
/// machine's last answer stops speaking for its agents, an observer's contact lapses).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folded {
    pub tables: FleetTables,
    pub next_change_ms: Option<u64>,
}

/// Unix seconds as milliseconds.
const fn ms(seconds: u64) -> u64 {
    seconds.saturating_mul(1_000)
}

/// The word of `words` that `raw` is.
fn status_word(words: &'static [&'static str], raw: Option<&str>) -> Option<&'static str> {
    let raw = raw?;
    words.iter().copied().find(|word| *word == raw)
}

/// Each `m<N>` label and alias to the one machine row it names, as `aterm db alab` reads a
/// machine's name: a live row over a retired one, a row whose label it is over one whose alias
/// it is, and a name two such rows share names neither.
fn machines_by_name(lab: &LabView) -> BTreeMap<&str, Option<&str>> {
    let mut best: BTreeMap<&str, ((bool, bool), Option<&str>)> = BTreeMap::new();
    for machine in lab.machines.values() {
        let names = machine
            .label
            .iter()
            .map(|label| (label.as_str(), true))
            .chain(machine.aliases.iter().map(|alias| (alias.as_str(), false)));
        for (name, is_label) in names {
            let rank = (machine.live, is_label);
            match best.get_mut(name) {
                Some((held, pk)) if *held == rank => {
                    if *pk != Some(machine.pk.as_str()) {
                        *pk = None;
                    }
                }
                Some((held, _)) if *held > rank => {}
                _ => {
                    best.insert(name, (rank, Some(machine.pk.as_str())));
                }
            }
        }
    }
    best.into_iter().map(|(name, (_, pk))| (name, pk)).collect()
}

/// Each account label and legacy label to the one account it names; a name two accounts share
/// names neither.
fn accounts_by_name(lab: &LabView) -> BTreeMap<&str, Option<&str>> {
    let mut by_name: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for account in lab.accounts.values() {
        for name in account.label.iter().chain(account.legacy_label.iter()) {
            by_name
                .entry(name.as_str())
                .and_modify(|pk| {
                    if *pk != Some(account.pk.as_str()) {
                        *pk = None;
                    }
                })
                .or_insert(Some(account.pk.as_str()));
        }
    }
    by_name
}

/// The realms an address is tried under where its harness names none.
const EVERY_REALM: &[&str] = &[
    "claude",
    "codex",
    "google",
    "meta",
    "grok",
    "kimi:global",
    "kimi:mainland-cn",
];

/// The registry account `word` names for a session of `harness`: an address by the canonical
/// account key of the harness's realm (of every realm, where the harness names none, and then
/// only when exactly one answers), a label by the one account that goes by it.
fn resolve(
    lab: &LabView,
    by_name: &BTreeMap<&str, Option<&str>>,
    harness: Harness,
    word: &AccountWord,
) -> Option<String> {
    let word = word.as_str();
    if !word.contains('@') {
        return by_name.get(word).copied().flatten().map(str::to_owned);
    }
    let key = |realm: &&str| {
        keys::account_pk(realm, word)
            .ok()
            .filter(|pk| lab.accounts.contains_key(pk))
    };
    let realms = harness.realms();
    if realms.is_empty() {
        let mut found = EVERY_REALM.iter().filter_map(key);
        let first = found.next()?;
        return found.next().is_none().then_some(first);
    }
    realms.iter().find_map(key)
}

/// The order of a machine: by its label's number, unnumbered after, then by label and key.
fn machine_order<'a>(
    label: Option<&'a str>,
    number: Option<u32>,
    key: &'a str,
) -> (bool, u32, &'a str, &'a str) {
    (
        number.is_none(),
        number.unwrap_or(u32::MAX),
        label.unwrap_or(""),
        key,
    )
}

/// The sighting that stands for a machine among several: this Mac's own, else the one that
/// answered last, else the one named last.
fn stands<'a>(a: (&'a str, &'a Sighted), b: (&'a str, &'a Sighted)) -> (&'a str, &'a Sighted) {
    fn rank<'s>(id: &'s str, s: &Sighted) -> (bool, Option<u64>, u64, std::cmp::Reverse<&'s str>) {
        (
            s.how == super::read::How::Own,
            s.answered.as_ref().map(|at| at.at_ms),
            s.named_ms,
            std::cmp::Reverse(id),
        )
    }
    if rank(b.0, b.1) > rank(a.0, a.1) {
        b
    } else {
        a
    }
}

/// One window's row from the journal's quotas of it (the account's own first, then its
/// sub-accounts'), read again as of `now_s`, and the Mac that read its figure.
fn window_row(quotas: &[&WindowQuota], now_s: u64) -> (WindowRow, Option<(u64, String)>) {
    // The account's own reading stands; with none, the most used workspace's does (readings
    // are snapshots and never add up).
    let pick = quotas
        .iter()
        .filter_map(|q| q.usage.as_ref().map(|usage| (q, usage)))
        .max_by_key(|(q, usage)| {
            let used = match usage {
                Usage::Current { pct_x10, .. } => u32::from(*pct_x10),
                Usage::ResetUnread { .. } => 0,
            };
            (q.pk.starts_with("ac-"), used)
        });
    let Some((_, usage)) = pick else {
        return (WindowRow::UNREAD, None);
    };
    match usage {
        Usage::Current {
            pct_x10,
            reset,
            read,
            by,
            ..
        } => {
            if reset.is_some_and(|reset| reset <= now_s) {
                (
                    WindowRow {
                        used_x10: None,
                        reset_ms: reset.map(ms),
                        state: WindowState::ResetPassed,
                    },
                    Some((*read, by.clone())),
                )
            } else {
                (
                    WindowRow {
                        used_x10: Some(At::new(*pct_x10, ms(*read))),
                        reset_ms: reset.map(ms),
                        state: WindowState::Current,
                    },
                    Some((*read, by.clone())),
                )
            }
        }
        Usage::ResetUnread { reset, read, by } => (
            WindowRow {
                used_x10: None,
                reset_ms: Some(ms(*reset)),
                state: WindowState::ResetPassed,
            },
            Some((*read, by.clone())),
        ),
    }
}

/// A machine row's sighting facts. `newest_ms` is the newest read that could have named the
/// machine: any read for this Mac, a read that asked the fleet for a peer.
fn sighted_facts(
    row: &mut MachineRow,
    sighted: &Sighted,
    newest_ms: u64,
    now_ms: u64,
    next: &mut Next,
) {
    row.host.clone_from(&sighted.host);
    if let Some(answered) = &sighted.answered {
        let at = answered.at_ms;
        row.proven = Some(!sighted.unproven);
        row.build = answered.v.build.clone().map(|build| At::new(build, at));
        row.disk_free_mb = answered.v.home_free_mb.map(|free| At::new(free, at));
        row.last_seen_ms = Some(at);
        if now_ms.saturating_sub(at) <= AGENTS_KEEP_MS {
            row.sessions = answered.v.sessions.map(|sessions| At::new(sessions, at));
            row.tally = answered.v.tally.map(|tally| At::new(tally, at));
            row.agents_listed = Some(u32::try_from(answered.v.agents.len()).unwrap_or(u32::MAX));
            next.at(at.saturating_add(AGENTS_KEEP_MS).saturating_add(1));
        }
    }
    // Only the newest read says whether it answers now, and only while that read is young.
    if sighted.named_ms == newest_ms && now_ms.saturating_sub(newest_ms) <= REACH_KEEP_MS {
        row.how = Via::of(sighted.how);
        row.reach_at_ms = Some(newest_ms);
        match &sighted.silent {
            Some(silent) => {
                row.reach = Reach::Silent;
                row.reach_why.clone_from(&silent.v);
            }
            None => row.reach = Reach::Up,
        }
        next.at(newest_ms.saturating_add(REACH_KEEP_MS).saturating_add(1));
    }
}

/// The newest read that could have named `sighted`: any read for this Mac, whose windows every
/// read asks; a read that asked the fleet for a peer.
fn newest_for(latest: Option<&Latest>, sighted: &Sighted) -> u64 {
    latest.map_or(0, |latest| match sighted.how {
        super::read::How::Own => latest.read_at_ms,
        super::read::How::Survey => latest.fleet_at_ms,
    })
}

/// The earliest instant ahead of `now_ms` that a fold's rows change by themselves.
struct Next {
    now_ms: u64,
    at_ms: Option<u64>,
}

impl Next {
    fn at(&mut self, at_ms: u64) {
        if at_ms > self.now_ms {
            self.at_ms = Some(self.at_ms.map_or(at_ms, |held| held.min(at_ms)));
        }
    }
}

/// Fold `inputs` as of `now_ms` (Unix milliseconds).
pub fn fold(inputs: &Inputs, now_ms: u64) -> Folded {
    let lab = inputs.lab.value().map(Arc::as_ref);
    let seen = inputs.journal.value().map(Arc::as_ref);
    let network = inputs.network.value().map(|snapshot| &snapshot.network);
    let latest = inputs.read.value().map(Arc::as_ref);
    let now_s = now_ms / 1_000;
    let mut next = Next {
        now_ms,
        at_ms: None,
    };
    let mut counts = Counts::default();

    // Which registry machine each sighting names, and the one that stands for it.
    let by_name = lab.map(machines_by_name).unwrap_or_default();
    let mut standing: BTreeMap<&str, (&str, &Sighted)> = BTreeMap::new();
    let mut unregistered: Vec<(&str, &Sighted)> = Vec::new();
    for (id, sighted) in latest.iter().flat_map(|latest| &latest.machines) {
        // This Mac is the machine its own facts row binds, whatever its header says: the header
        // is local configuration, which a renumbering can leave naming another machine.
        let me = (id == OWN_ID)
            .then(|| lab.and_then(|lab| lab.me.as_deref()))
            .flatten()
            .filter(|me| lab.is_some_and(|lab| lab.machines.contains_key(*me)));
        let named = me
            .or_else(|| {
                sighted
                    .label
                    .as_deref()
                    .and_then(|label| by_name.get(label).copied().flatten())
            })
            .or_else(|| {
                let number = sighted.host.as_deref().and_then(Number::leading)?;
                let label = format!("m{}", number.get());
                by_name.get(label.as_str()).copied().flatten()
            });
        match named {
            Some(pk) => {
                let candidate = (id.as_str(), sighted);
                match standing.get_mut(pk) {
                    Some(held) => {
                        *held = stands(*held, candidate);
                        counts.shadowed = counts.shadowed.saturating_add(1);
                    }
                    None => {
                        standing.insert(pk, candidate);
                    }
                }
            }
            None => unregistered.push((id.as_str(), sighted)),
        }
    }
    // The observer's peers and the journal's presence rows, by the machine each uuid binds.
    let mut peers: BTreeMap<&str, &PeerObservation> = BTreeMap::new();
    let mut uuids: BTreeMap<&str, &str> = BTreeMap::new();
    let mut sums: BTreeMap<&str, (u64, Option<String>)> = BTreeMap::new();
    if let Some(lab) = lab {
        for (uuid, pk) in &lab.bound {
            uuids.entry(pk.as_str()).or_insert(uuid.as_str());
        }
        for peer in network.iter().flat_map(|network| &network.peers) {
            let uuid = hex::encode_lower(&peer.identity.machine_id);
            if let Some(pk) = lab.bound.get(&uuid) {
                let held = peers.entry(pk.as_str()).or_insert(peer);
                if peer.last_seen_ms > held.last_seen_ms {
                    *held = peer;
                }
            }
        }
        for (uuid, sum, read_s) in seen
            .iter()
            .flat_map(|seen| seen.sums.iter().map(|(uuid, sum)| (uuid, sum, seen.now)))
        {
            let (Some(pk), Some(at)) = (
                lab.bound.get(uuid),
                sum.get("at").and_then(aterm_json::Value::as_u64),
            ) else {
                continue;
            };
            // The row's time is its writer's clock: one ahead of the journal's read is held to
            // that read, as a claim's is (`Seen::claim`), so no machine was seen in the future.
            let at = at.min(read_s);
            let build = sum
                .get("aterm")
                .and_then(aterm_json::Value::as_str)
                .and_then(|build| super::read::word(build, 48));
            let held = sums.entry(pk.as_str()).or_insert((at, build.clone()));
            if at > held.0 {
                *held = (at, build);
            }
        }
    }

    // Machines: every registry row, then the answers no row names.
    let mut machines: Vec<MachineRow> = Vec::new();
    // Each machine row's sighting, for the agents relation.
    let mut sighted_of: BTreeMap<String, &Sighted> = BTreeMap::new();
    for machine in lab.iter().flat_map(|lab| lab.machines.values()) {
        let mut row = MachineRow::unread(machine.pk.clone());
        row.label.clone_from(&machine.label);
        row.number = machine
            .label
            .as_deref()
            .and_then(Number::leading)
            .map(Number::get);
        row.status = if machine.live {
            status_word(MACHINE_STATUS, machine.status.as_deref())
        } else {
            Some(RETIRED)
        };
        row.os = machine.os.as_deref().and_then(|v| super::read::word(v, 32));
        row.chip = machine
            .chip
            .as_deref()
            .and_then(|v| super::read::word(v, 32));
        row.ram_gb = machine.ram_gb;
        // The memory pair's capacity is the registry's figure; nothing carries its use yet.
        row.mem.capacity = machine
            .ram_gb
            .zip(inputs.lab.at_ms())
            .map(|(gb, at)| At::new(gb.saturating_mul(1_024), at));
        row.uuid = uuids
            .get(machine.pk.as_str())
            .map(|uuid| (*uuid).to_owned());
        row.accounts_expected =
            lab.map(|lab| u32::try_from(lab.preferred_by(&machine.pk).count()).unwrap_or(u32::MAX));
        if let Some((_, sighted)) = standing.get(machine.pk.as_str()) {
            let newest = newest_for(latest, sighted);
            sighted_facts(&mut row, sighted, newest, now_ms, &mut next);
            sighted_of.insert(row.machine.clone(), sighted);
        }
        if let Some(peer) = peers.get(machine.pk.as_str()) {
            if peer.first_seen_ms > 0 {
                row.first_seen_ms = Some(peer.first_seen_ms);
            }
            if peer.last_seen_ms > 0 {
                row.last_seen_ms = row.last_seen_ms.max(Some(peer.last_seen_ms));
            }
            if row.reach == Reach::Unread {
                row.how = Via::Observer;
                if peer.reachability_at(Some(now_ms)) == Reachability::Reachable {
                    row.reach = Reach::Up;
                    row.reach_at_ms = peer.last_checked_ms.or(Some(peer.last_seen_ms));
                    let lapses = peer
                        .last_seen_ms
                        .min(peer.last_checked_ms.unwrap_or(peer.last_seen_ms))
                        .saturating_add(peer.reachable_for_ms());
                    next.at(lapses.saturating_add(1));
                }
            }
        }
        if let Some((at, build)) = sums.get(machine.pk.as_str()) {
            let at = ms(*at);
            row.last_seen_ms = row.last_seen_ms.max(Some(at));
            // The newer reading of its build stands, an answer's or its journal row's.
            if let Some(build) = build
                && row.build.as_ref().is_none_or(|held| held.at_ms < at)
            {
                row.build = Some(At::new(build.clone(), at));
            }
            if row.how == Via::None {
                row.how = Via::Journal;
            }
        }
        machines.push(row);
    }
    machines.sort_by(|a, b| {
        machine_order(a.label.as_deref(), a.number, &a.machine).cmp(&machine_order(
            b.label.as_deref(),
            b.number,
            &b.machine,
        ))
    });
    let mut strangers: Vec<MachineRow> = unregistered
        .iter()
        .map(|(id, sighted)| {
            let mut row = MachineRow::unread((*id).to_owned());
            row.label.clone_from(&sighted.label);
            row.number = sighted
                .label
                .as_deref()
                .or(sighted.host.as_deref())
                .and_then(Number::leading)
                .map(Number::get);
            // With no registry read, nothing says the registry does not name it.
            row.status = lab.map(|_| UNREGISTERED);
            let newest = newest_for(latest, sighted);
            sighted_facts(&mut row, sighted, newest, now_ms, &mut next);
            sighted_of.insert(row.machine.clone(), sighted);
            row
        })
        .collect();
    strangers.sort_by(|a, b| {
        (
            machine_order(a.label.as_deref(), a.number, ""),
            &a.host,
            &a.machine,
        )
            .cmp(&(
                machine_order(b.label.as_deref(), b.number, ""),
                &b.host,
                &b.machine,
            ))
    });
    machines.extend(strangers);

    // Agents: each listed session of each machine whose answer still speaks for them.
    let accounts_by_name = lab.map(accounts_by_name).unwrap_or_default();
    let mut agents: Vec<AgentRow> = Vec::new();
    for row in &machines {
        let Some((sighted, answered)) = sighted_of
            .get(&row.machine)
            .and_then(|sighted| sighted.answered.as_ref().map(|at| (sighted, at)))
            .filter(|(_, at)| now_ms.saturating_sub(at.at_ms) <= AGENTS_KEEP_MS)
        else {
            continue;
        };
        let (how, proven) = (sighted.how, !sighted.unproven);
        let mut listed: Vec<AgentRow> = answered
            .v
            .agents
            .iter()
            .map(|agent| {
                let account = match &agent.account {
                    None => AccountRef::Unnamed,
                    Some(word) => lab
                        .and_then(|lab| resolve(lab, &accounts_by_name, agent.harness, word))
                        .map_or(AccountRef::Unresolved, AccountRef::Resolved),
                };
                if account == AccountRef::Unresolved {
                    counts.unresolved_accounts = counts.unresolved_accounts.saturating_add(1);
                }
                AgentRow {
                    machine: row.machine.clone(),
                    label: row.label.clone(),
                    sid: agent.sid.clone(),
                    harness: agent.harness,
                    state: agent.state,
                    state_why: agent.state_why.clone(),
                    state_since_ms: agent
                        .state_age_ms
                        .map(|age| answered.at_ms.saturating_sub(age)),
                    task: agent.task,
                    ctx_left_pct: agent.ctx_left_pct,
                    usage_5h_pct: agent.usage_5h_pct,
                    usage_7d_pct: agent.usage_7d_pct,
                    account,
                    model: agent.model.clone(),
                    read_at_ms: answered.at_ms,
                    how,
                    proven,
                }
            })
            .collect();
        listed.sort_by(|a, b| a.sid.cmp(&b.sid));
        listed.dedup_by(|a, b| a.sid == b.sid);
        agents.extend(listed);
    }

    // Accounts: every registry row, its usage as of now, and where it is expected and seen.
    let machine_ref = |pk: &str| MachineRef {
        machine: pk.to_owned(),
        label: lab.and_then(|lab| lab.machine_label(pk)).map(str::to_owned),
    };
    let bound = |uuid: &str| lab.and_then(|lab| lab.bound.get(uuid)).map(String::as_str);
    let journal_at = inputs.journal.at_ms().unwrap_or(0);
    let known_to_journal: BTreeSet<String> = seen.map(Seen::accounts).unwrap_or_default();
    // Whether any machine's answer still speaks for its agents: with none, how many sessions
    // run on an account is unread, not zero.
    let agents_read = machines.iter().any(|row| row.agents_listed.is_some());
    let evidence_read = seen.is_some() || agents_read;
    let mut active_on: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut accounts: Vec<AccountRow> = Vec::new();
    for account in lab.iter().flat_map(|lab| lab.accounts.values()) {
        let pk = account.pk.as_str();
        let quotas = seen.map(|seen| seen.quota(pk)).unwrap_or_default();
        let of = |window: Window| -> Vec<&WindowQuota> {
            quotas.iter().filter(|q| q.window == window).collect()
        };
        let (w5h, read5) = window_row(&of(Window::FiveHour), now_s);
        let (w7d, read7) = window_row(&of(Window::Week), now_s);
        let wall_until = quotas
            .iter()
            .filter_map(|q| q.wall.as_ref())
            .filter(|wall| wall.until.is_none_or(|until| until > now_s))
            .map(|wall| wall.until)
            .max_by_key(|until| until.unwrap_or(u64::MAX));
        let walled = known_to_journal
            .contains(pk)
            .then_some(wall_until.is_some());
        let wall_until_ms = wall_until.flatten().map(ms);
        let read_by = [read5, read7]
            .into_iter()
            .flatten()
            .max_by_key(|(read, _)| *read)
            .and_then(|(_, uuid)| bound(&uuid).and_then(|pk| machine_ref(pk).label));
        let next_reset_ms = [w5h.reset_ms, w7d.reset_ms, wall_until_ms]
            .into_iter()
            .flatten()
            .filter(|at| *at > now_ms)
            .min();
        // A window's figure goes, and a wall ends, at the instant its time comes.
        for at in [w5h, w7d]
            .iter()
            .filter(|w| w.state == WindowState::Current)
            .filter_map(|w| w.reset_ms)
            .chain(wall_until_ms)
        {
            next.at(at);
        }

        // An unproven answer's word for the account its agent runs on is a claim: it is no
        // evidence and no sighted session of the account.
        let mut evidence: Vec<Evidence> = Vec::new();
        for agent in agents.iter().filter(|agent| {
            agent.proven && agent.account == AccountRef::Resolved(account.pk.clone())
        }) {
            evidence.push(Evidence {
                on: MachineRef {
                    machine: agent.machine.clone(),
                    label: agent.label.clone(),
                },
                how: EvidenceHow::Agent,
                since_ms: None,
                at_ms: agent.read_at_ms,
            });
        }
        // Sessions are counted before the evidence keeps one piece per (machine, source).
        let agent_sessions = agents_read.then(|| u32::try_from(evidence.len()).unwrap_or(u32::MAX));
        if let Some(seen) = seen {
            // A claim and a place stop being evidence at the instant each ends.
            for claim in seen.claims.get(pk).into_iter().flatten() {
                if let Some(machine) = bound(&claim.uuid).filter(|_| claim.until > now_s) {
                    next.at(ms(claim.until));
                    evidence.push(Evidence {
                        on: machine_ref(machine),
                        how: EvidenceHow::Claim,
                        since_ms: Some(ms(claim.since)),
                        at_ms: journal_at,
                    });
                }
            }
            for place in seen.places.rows.iter().filter(|row| row.account == pk) {
                if let Some(machine) = bound(&place.uuid).filter(|_| place.until > now_s) {
                    next.at(ms(place.until));
                    evidence.push(Evidence {
                        on: machine_ref(machine),
                        how: EvidenceHow::Place,
                        since_ms: Some(ms(place.since)),
                        at_ms: journal_at,
                    });
                }
            }
            for (uuid, signed) in seen.signins.get(pk).into_iter().flatten() {
                if let Some(machine) = bound(uuid).filter(|_| signed.state == "in") {
                    evidence.push(Evidence {
                        on: machine_ref(machine),
                        how: EvidenceHow::SignedIn,
                        since_ms: Some(ms(signed.t)),
                        at_ms: journal_at,
                    });
                }
            }
        }
        evidence.sort_by_key(|e| {
            let number = e.on.label.as_deref().and_then(Number::leading);
            (
                number.is_none(),
                number,
                e.on.machine.clone(),
                e.how,
                e.since_ms,
            )
        });
        evidence.dedup_by(|a, b| a.on.machine == b.on.machine && a.how == b.how);
        let last_use = seen.and_then(|seen| seen.last_use.get(pk));
        accounts.push(AccountRow {
            account: account.pk.clone(),
            provider: account.provider.clone(),
            slug: account.label.clone(),
            login: account.login.as_deref().map(str::to_ascii_lowercase),
            status: if account.live {
                status_word(ACCOUNT_STATUS, account.status.as_deref())
            } else {
                Some(RETIRED)
            },
            plan: account.plan.clone(),
            w5h,
            w7d,
            walled,
            wall_until_ms,
            read_by,
            next_reset_ms,
            expected: lab
                .iter()
                .flat_map(|lab| lab.preferring(pk))
                .map(|preference| machine_ref(&preference.machine))
                .collect(),
            active: evidence_read.then_some(evidence),
            agents: agent_sessions,
            last_use_ms: last_use.map(|(_, t)| ms(*t)),
            last_use_on: last_use
                .and_then(|(uuid, _)| bound(uuid).and_then(|pk| machine_ref(pk).label)),
        });
    }
    accounts.sort_by(|a, b| {
        (
            a.provider.is_none(),
            &a.provider,
            a.slug.is_none(),
            &a.slug,
            &a.account,
        )
            .cmp(&(
                b.provider.is_none(),
                &b.provider,
                b.slug.is_none(),
                &b.slug,
                &b.account,
            ))
    });
    for account in &accounts {
        for evidence in account.active.iter().flatten() {
            active_on
                .entry(evidence.on.machine.as_str())
                .or_default()
                .insert(account.account.as_str());
        }
    }
    let active_counts: BTreeMap<String, u32> = active_on
        .iter()
        .map(|(machine, on)| {
            (
                (*machine).to_owned(),
                u32::try_from(on.len()).unwrap_or(u32::MAX),
            )
        })
        .collect();
    for row in &mut machines {
        row.accounts_active = (evidence_read && lab.is_some())
            .then(|| active_counts.get(&row.machine).copied().unwrap_or(0));
    }

    // A read too old to say which machines answer is held, not live: reads that stopped (the
    // glance is not watched, this Mac left the LAN, it slept) are no current reading.
    let read = match inputs.read.state() {
        SourceState::Live { at_ms } => {
            next.at(at_ms.saturating_add(REACH_KEEP_MS).saturating_add(1));
            if now_ms.saturating_sub(at_ms) > REACH_KEEP_MS {
                SourceState::Held {
                    why: super::HELD_STALE,
                    at_ms: Some(at_ms),
                }
            } else {
                SourceState::Live { at_ms }
            }
        }
        state => state,
    };

    Folded {
        tables: FleetTables {
            machines,
            accounts,
            agents,
            sources: Sources {
                lab: inputs.lab.state(),
                journal: inputs.journal.state(),
                network: inputs.network.state(),
                read,
            },
            counts,
        },
        next_change_ms: next.at_ms,
    }
}
