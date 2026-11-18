// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BAND PROJECTION: what the Fleet band and the Fleet tab draw, as data. A [`Band`] is the
//! member machines in number order, each a [`MachineColumn`] with its reach, its AI accounts and
//! its agents in drawing order; the pool of accounts placed on no machine; each lane's counts;
//! eight days of the lanes' sums; and the state of the two shared services. It holds no painter
//! type: a painter, a card and a sheet each read the same figures from it.
//!
//! A value nobody read is `None`, never a zero: a machine that is not up has no load, no limit
//! count and no agent list, an account whose usage was not read has no tokens left, a [`Sum`]
//! no member was read into is `None` where it is held, and a history slot nobody sampled is
//! `None`. [`Band::check`] holds a projection to that and to its own arithmetic: each lane's
//! classes sum to its total, each count is what the members say, and no machine or account is
//! drawn twice.
//!
//! Figures are in the rows' units ([`super::rows`]): a core count in hundredths (`_x100`), a
//! share in tenths of a percent (`_x10`), memory in MiB (`_mb`).
//!
//! Nothing fills a `Band` from the tables yet. The fixture's scenes
//! ([`super::fixture::scene`]) are whole projections a painter can be built and judged on.

use super::rows::Reach;
use crate::typed::keys;
use std::collections::BTreeSet;

/// The history's slots, oldest first.
pub const SLOTS: usize = 64;
/// The span of one history slot: 3 hours, so [`SLOTS`] of them are 8 days.
pub const SLOT_MS: u64 = 3 * 3_600_000;
/// A whole share in tenths of a percent, the unit tokens left are held in.
pub const WHOLE_X10: u16 = 1000;

/// Whose model an AI account buys. The band draws one mark a provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Provider {
    /// Anthropic.
    Anthropic,
    /// OpenAI.
    OpenAi,
    /// Google.
    Google,
    /// Meta.
    Meta,
    /// xAI.
    Xai,
    /// Any provider the band has no mark of its own for (Kimi among them).
    Other,
}

impl Provider {
    /// Every provider, in the order a lane lists them.
    pub const ALL: [Self; 6] = [
        Self::Anthropic,
        Self::OpenAi,
        Self::Google,
        Self::Meta,
        Self::Xai,
        Self::Other,
    ];
    /// How many providers there are: the length of every array held by provider.
    pub const COUNT: usize = Self::ALL.len();

    /// The provider an account's realm names, read as the registry reads one
    /// ([`keys::provider_of_realm`]: `claude`, `oai`, `codex`, `gemini`, `agy`, `grok`, `x`,
    /// each provider word, in any case; the empty realm is Claude's, as the claims ring's
    /// default). A realm of a provider the band has no mark for, or of none, is [`Self::Other`].
    pub fn of(realm: &str) -> Self {
        match keys::provider_of_realm(realm) {
            Some(keys::ANTHROPIC) => Self::Anthropic,
            Some(keys::OPENAI) => Self::OpenAi,
            Some(keys::GOOGLE) => Self::Google,
            Some(keys::META) => Self::Meta,
            Some(keys::XAI) => Self::Xai,
            _ => Self::Other,
        }
    }

    /// Its provider word ([`keys::PROVIDERS`]); `other` for [`Self::Other`].
    pub const fn word(self) -> &'static str {
        match self {
            Self::Anthropic => keys::ANTHROPIC,
            Self::OpenAi => keys::OPENAI,
            Self::Google => keys::GOOGLE,
            Self::Meta => keys::META,
            Self::Xai => keys::XAI,
            Self::Other => "other",
        }
    }

    /// Its place in [`Self::ALL`].
    pub const fn index(self) -> usize {
        match self {
            Self::Anthropic => 0,
            Self::OpenAi => 1,
            Self::Google => 2,
            Self::Meta => 3,
            Self::Xai => 4,
            Self::Other => 5,
        }
    }
}

/// What an AI account that can be used is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountClass {
    /// An agent is running on it.
    InUse,
    /// Nothing runs on it and it has tokens left.
    Idle,
    /// It is at its limit until it refreshes.
    AtLimit,
    /// Nothing read it.
    Unread,
}

/// The fact an agent is drawn as (the design's decision FP-D5). Where its machine sends the
/// comprehension system's activity word, the class follows that word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentClass {
    /// Writing output: a turn is open and it wrote output in the last 2 minutes.
    Output,
    /// Running with no output: a job is running, or a turn has been open 2 minutes with no
    /// output (and not the 10 with no job that make it an error).
    Job,
    /// No turn is open and no job is running.
    Idle,
    /// Paused at its account's limit.
    Paused,
    /// Stalled or failed: a turn open 10 minutes with no output and no job, a question asked of
    /// a person, or a lost login.
    Error,
}

/// An AI account as the band draws it. `label` is the account's name on this display
/// (`claude-07`), never its login address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountMark {
    /// Its name on this display, which no other account of the projection has.
    pub label: String,
    /// Its provider.
    pub provider: Provider,
    /// What it is doing.
    pub class: AccountClass,
    /// The tokens it has left in tenths of a percent, at most [`WHOLE_X10`]: the smaller of what
    /// its windows have left. `None` where its usage was not read.
    pub left_x10: Option<u16>,
}

/// An agent as the band draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentMark {
    /// The fact it is drawn as.
    pub class: AgentClass,
    /// Its account's provider, where the account is known.
    pub provider: Option<Provider>,
}

/// A member machine: one column of the band.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineColumn {
    /// Its number, the order of the columns. `None` for a machine that answers and that the
    /// registry does not name, which is drawn after the numbered ones, by label.
    pub number: Option<u32>,
    /// Its label (`m37`).
    pub label: String,
    /// Whether it answers.
    pub reach: Reach,
    /// Its load, the deck dial's level `0..=100` (`read::Load::level_x100`). `None` unless it is
    /// up and its load was read.
    pub load_x100: Option<u8>,
    /// How many of its CPU, GPU, memory and disk are at their limit. `None` unless it is up
    /// and its limits were read.
    pub limits: Option<u8>,
    /// Whether the build it was last read on is old and it is not updating. A build outlives
    /// its answers: a machine that does not answer, or that nothing current read, keeps the
    /// mark its last read gave.
    pub old_build: bool,
    /// The AI accounts placed on it, in drawing order.
    pub accounts: Vec<AccountMark>,
    /// Its agents, in drawing order. `None` unless it is up: an empty list is a machine that
    /// was read and runs none.
    pub agents: Option<Vec<AgentMark>>,
}

/// A sum over the members that were read into it: how much is used, how much there is, and
/// how many members the two were read from, at least one. It is never a mean. A sum no member
/// was read into is `None` where it is held, never a zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sum {
    /// How much is in use, in the unit the field that holds the sum names.
    pub used: u64,
    /// How much there is, in the same unit.
    pub of: u64,
    /// How many members were read into it.
    pub members: u32,
}

impl Sum {
    /// One member's `used` of `of`.
    pub const fn one(used: u64, of: u64) -> Self {
        Self {
            used,
            of,
            members: 1,
        }
    }

    /// This sum with one more member's `used` of `of`.
    pub const fn and(self, used: u64, of: u64) -> Self {
        Self {
            used: self.used.saturating_add(used),
            of: self.of.saturating_add(of),
            members: self.members.saturating_add(1),
        }
    }

    /// `into` with one more member's `used` of `of`: the first member read makes the sum.
    pub fn gather(into: &mut Option<Self>, used: u64, of: u64) {
        *into = Some(match *into {
            Some(sum) => sum.and(used, of),
            None => Self::one(used, of),
        });
    }

    /// The share used in tenths of a percent, at most [`WHOLE_X10`]. `None` where there is
    /// nothing to have a share of.
    pub fn share_x10(self) -> Option<u16> {
        if self.of == 0 {
            return None;
        }
        let share =
            u128::from(self.used.min(self.of)) * u128::from(WHOLE_X10) / u128::from(self.of);
        u16::try_from(share).ok()
    }

    /// Whether the sum states only what it read: a member was read into it, and no more is
    /// used than there is.
    pub const fn holds(self) -> bool {
        self.members > 0 && self.used <= self.of
    }
}

/// The next instant after `now_ms` at which a count of whole minutes until `at_ms`, rounded
/// up, changes; `None` once `at_ms` is not after `now_ms`. Unix milliseconds.
pub const fn next_minute_change(now_ms: u64, at_ms: u64) -> Option<u64> {
    if at_ms <= now_ms {
        return None;
    }
    Some(now_ms + (at_ms - now_ms - 1) % 60_000 + 1)
}

/// The Machines lane's counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MachineLane {
    /// The member machines: one column each.
    pub members: u32,
    /// Of them, the ones that answer.
    pub up: u32,
    /// The ones that were asked and do not answer.
    pub silent: u32,
    /// The ones nothing current read.
    pub unread: u32,
    /// The ones on an old build that is not updating.
    pub old_build: u32,
    /// The ones with a CPU, GPU, memory or disk at its limit.
    pub at_limit: u32,
    /// Cores busy of cores of the machines that answer, in hundredths of a core.
    pub cores_x100: Option<Sum>,
    /// Memory used of memory of the machines that answer, in MiB.
    pub memory_mb: Option<Sum>,
}

/// The AI accounts lane's counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountLane {
    /// The accounts that can be used: every account the band draws.
    pub usable: u32,
    /// Of them, the ones in use.
    pub in_use: u32,
    /// The idle ones.
    pub idle: u32,
    /// The ones at their limit.
    pub at_limit: u32,
    /// The ones nothing read.
    pub unread: u32,
    /// The registry's accounts that cannot be used, which are not drawn. `None` where the
    /// registry was not read.
    pub cannot_be_used: Option<u32>,
    /// Tokens left of the accounts whose usage was read, in tenths of a percent.
    pub left_x10: Option<Sum>,
    /// The same for each provider, by [`Provider::index`].
    pub left_by: [Option<Sum>; Provider::COUNT],
    /// When the next account at its limit refreshes, Unix milliseconds.
    pub next_refresh_ms: Option<u64>,
}

/// The Agents lane's counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentLane {
    /// The agents of the machines that were read.
    pub agents: u32,
    /// Of them, the ones writing output.
    pub output: u32,
    /// The ones running with no output.
    pub job: u32,
    /// The idle ones.
    pub idle: u32,
    /// The ones paused at their account's limit.
    pub paused: u32,
    /// The ones in error.
    pub error: u32,
    /// The machines whose agents were not read.
    pub machines_unread: u32,
}

impl AgentLane {
    /// The agents that are running: writing output, or running with none.
    pub const fn running(&self) -> u32 {
        self.output.saturating_add(self.job)
    }
}

/// One history slot's sums. A kind nobody sampled in the slot is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    /// Cores busy of cores, in hundredths of a core.
    pub cores_x100: Option<Sum>,
    /// Tokens left, in tenths of a percent.
    pub left_x10: Option<Sum>,
    /// Agents running of agents, over the machines whose agents were read.
    pub agents: Option<Sum>,
    /// The build most answering machines ran.
    pub build: Option<String>,
}

/// Eight days of the lanes' sums: [`SLOTS`] slots of [`SLOT_MS`], oldest first. A slot nobody
/// sampled is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    /// The slots, oldest first.
    pub slots: [Option<Slot>; SLOTS],
    /// When the newest slot ends, Unix milliseconds.
    pub end_ms: u64,
}

impl History {
    /// A history nobody sampled, ending at `end_ms`.
    pub fn unsampled(end_ms: u64) -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            end_ms,
        }
    }
}

/// Whether a shared service can be reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    /// It answers.
    Up,
    /// It does not.
    Down,
    /// Nothing read it.
    Unread,
}

/// The two shared services the band draws only when one is down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Services {
    /// The accounts exchange.
    pub accounts: Service,
    /// The secrets vault.
    pub secrets: Service,
}

/// A lane of the band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// Machines.
    Machines,
    /// AI accounts.
    Accounts,
    /// Agents.
    Agents,
}

/// What [`Band::check`] found wrong with a projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The machines are not in number order (the numbered ones first, the others by label), or
    /// two share a place.
    Order,
    /// The machine of this label has two columns.
    MachineTwice(String),
    /// The index of this machine's own column names no machine.
    Own,
    /// A lane's classes do not sum to its total.
    Open(Lane),
    /// A lane states a count that its members do not give.
    Miscount(Lane),
    /// A lane's sum does not hold ([`Sum::holds`]) or states more members than answer.
    Sum(Lane),
    /// The account of this label is drawn twice.
    AccountTwice(String),
    /// The machine of this label is not up and states a value only an answer gives: a load, a
    /// limit count or agents.
    Unread(String),
    /// The account of this label states tokens left though nothing read it.
    UnreadAccount(String),
    /// The account of this label states more tokens left than a whole window holds.
    OverWhole(String),
    /// The history slot of this index is stated and holds no sum.
    EmptySlot(usize),
    /// The history slot of this index states a sum that does not hold ([`Sum::holds`]).
    SlotSum(usize),
}

/// The Fleet display's projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Band {
    /// The member machines in number order, one column each.
    pub machines: Vec<MachineColumn>,
    /// The index in `machines` of this machine's own column.
    pub own: Option<usize>,
    /// The accounts that can be used and are placed on no machine.
    pub pool: Vec<AccountMark>,
    /// The Machines lane's counts.
    pub machine_lane: MachineLane,
    /// The AI accounts lane's counts.
    pub account_lane: AccountLane,
    /// The Agents lane's counts.
    pub agent_lane: AgentLane,
    /// Eight days of the lanes' sums.
    pub history: History,
    /// The two shared services.
    pub services: Services,
    /// The next instant a duration this projection prints changes, Unix milliseconds; `None`
    /// where it prints none.
    pub next_change_ms: Option<u64>,
}

/// `n` as a count.
fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl Band {
    /// The lanes' counts as `machines` and `pool` give them. What no member states is left
    /// unread: the cores and memory sums, the accounts that cannot be used and the next refresh.
    pub fn lanes_of(
        machines: &[MachineColumn],
        pool: &[AccountMark],
    ) -> (MachineLane, AccountLane, AgentLane) {
        let mut m = MachineLane {
            members: count(machines.len()),
            up: 0,
            silent: 0,
            unread: 0,
            old_build: 0,
            at_limit: 0,
            cores_x100: None,
            memory_mb: None,
        };
        let mut a = AccountLane {
            usable: 0,
            in_use: 0,
            idle: 0,
            at_limit: 0,
            unread: 0,
            cannot_be_used: None,
            left_x10: None,
            left_by: [None; Provider::COUNT],
            next_refresh_ms: None,
        };
        let mut g = AgentLane {
            agents: 0,
            output: 0,
            job: 0,
            idle: 0,
            paused: 0,
            error: 0,
            machines_unread: 0,
        };
        for machine in machines {
            match machine.reach {
                Reach::Up => m.up += 1,
                Reach::Silent => m.silent += 1,
                Reach::Unread => m.unread += 1,
            }
            m.old_build += u32::from(machine.old_build);
            m.at_limit += u32::from(machine.limits.is_some_and(|n| n > 0));
            match &machine.agents {
                None => g.machines_unread += 1,
                Some(agents) => {
                    for agent in agents {
                        g.agents += 1;
                        match agent.class {
                            AgentClass::Output => g.output += 1,
                            AgentClass::Job => g.job += 1,
                            AgentClass::Idle => g.idle += 1,
                            AgentClass::Paused => g.paused += 1,
                            AgentClass::Error => g.error += 1,
                        }
                    }
                }
            }
        }
        for account in machines
            .iter()
            .flat_map(|machine| &machine.accounts)
            .chain(pool)
        {
            a.usable += 1;
            match account.class {
                AccountClass::InUse => a.in_use += 1,
                AccountClass::Idle => a.idle += 1,
                AccountClass::AtLimit => a.at_limit += 1,
                AccountClass::Unread => a.unread += 1,
            }
            if let Some(left) = account.left_x10 {
                // `check` refuses a share above the whole; the clamp only keeps the sum's
                // arithmetic whole.
                let (left, whole) = (u64::from(left.min(WHOLE_X10)), u64::from(WHOLE_X10));
                Sum::gather(&mut a.left_x10, left, whole);
                Sum::gather(&mut a.left_by[account.provider.index()], left, whole);
            }
        }
        (m, a, g)
    }

    /// Whether this projection holds: its machines are in number order, each has one column,
    /// each lane's classes sum to its total and each count is what the members give, every sum
    /// states only what it read (the lanes' and every history slot's), no account is drawn
    /// twice or holds more than a whole share, and nothing unread is stated as a value. The
    /// first fault found is the error.
    ///
    /// # Errors
    ///
    /// The first [`Fault`] found.
    pub fn check(&self) -> Result<(), Fault> {
        let places: Vec<(bool, u32, &str)> = self
            .machines
            .iter()
            .map(|machine| {
                let label = if machine.number.is_some() {
                    ""
                } else {
                    machine.label.as_str()
                };
                (machine.number.is_none(), machine.number.unwrap_or(0), label)
            })
            .collect();
        if !places.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(Fault::Order);
        }
        let mut columns = BTreeSet::new();
        for machine in &self.machines {
            if !columns.insert(machine.label.as_str()) {
                return Err(Fault::MachineTwice(machine.label.clone()));
            }
        }
        if self.own.is_some_and(|own| own >= self.machines.len()) {
            return Err(Fault::Own);
        }
        let (m, a, g) = (&self.machine_lane, &self.account_lane, &self.agent_lane);
        let total = |parts: &[u32]| parts.iter().map(|n| u64::from(*n)).sum::<u64>();
        if total(&[m.up, m.silent, m.unread]) != u64::from(m.members) {
            return Err(Fault::Open(Lane::Machines));
        }
        if total(&[a.in_use, a.idle, a.at_limit, a.unread]) != u64::from(a.usable) {
            return Err(Fault::Open(Lane::Accounts));
        }
        if total(&[g.output, g.job, g.idle, g.paused, g.error]) != u64::from(g.agents) {
            return Err(Fault::Open(Lane::Agents));
        }
        let (mut machines, mut accounts, agents) = Self::lanes_of(&self.machines, &self.pool);
        machines.cores_x100 = m.cores_x100;
        machines.memory_mb = m.memory_mb;
        accounts.cannot_be_used = a.cannot_be_used;
        accounts.next_refresh_ms = a.next_refresh_ms;
        if machines != *m {
            return Err(Fault::Miscount(Lane::Machines));
        }
        if accounts != *a {
            return Err(Fault::Miscount(Lane::Accounts));
        }
        if agents != *g {
            return Err(Fault::Miscount(Lane::Agents));
        }
        for sum in [m.cores_x100, m.memory_mb].into_iter().flatten() {
            if !sum.holds() || sum.members > m.up {
                return Err(Fault::Sum(Lane::Machines));
            }
        }
        let mut seen = BTreeSet::new();
        for account in self
            .machines
            .iter()
            .flat_map(|machine| &machine.accounts)
            .chain(&self.pool)
        {
            if !seen.insert(account.label.as_str()) {
                return Err(Fault::AccountTwice(account.label.clone()));
            }
            if account.class == AccountClass::Unread && account.left_x10.is_some() {
                return Err(Fault::UnreadAccount(account.label.clone()));
            }
            if account.left_x10.is_some_and(|left| left > WHOLE_X10) {
                return Err(Fault::OverWhole(account.label.clone()));
            }
        }
        for machine in &self.machines {
            let stated =
                machine.load_x100.is_some() || machine.limits.is_some() || machine.agents.is_some();
            if machine.reach != Reach::Up && stated {
                return Err(Fault::Unread(machine.label.clone()));
            }
        }
        for (i, slot) in self.history.slots.iter().enumerate() {
            let Some(slot) = slot else { continue };
            let sums = [slot.cores_x100, slot.left_x10, slot.agents];
            if sums.iter().all(Option::is_none) {
                return Err(Fault::EmptySlot(i));
            }
            if !sums.into_iter().flatten().all(Sum::holds) {
                return Err(Fault::SlotSum(i));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "band_tests.rs"]
mod tests;
