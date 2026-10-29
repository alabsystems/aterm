// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Account claims with no broker (`docs/DESIGN-fluid-swarm-2026-10-28.md` F19, §3.5.3).
//!
//! Each Mac signs one row into the Commons claims table (`aterm_db::accounts::claims`, a ring of
//! UTC-day tables): its whole claim set. Every
//! Mac reads every row and settles each account the same way: the senior live claim wins
//! ([`senior_lease`]), and a junior yields at its next turn end, because [`settle`] records the
//! senior as the account's holder and the turn-end advice moves a session off an account another
//! Mac holds (`accounts::WhyNot::LeasedElsewhere`). Nothing brokers and nothing else is published.
//!
//! - **A row** names its Mac (the Commons machine of the binding that signed it, never a body
//!   field), its label, a counter (its revision: of two rows from one Mac the higher wins) and
//!   `until`, past which its claims lapse unless the Mac writes the row again.
//! - **A claim** is an account ([`LeaseKey`]), its kind ([`Kind::Home`] where the account's home
//!   is this Mac, else [`Kind::Lease`]) and `since`: when its uninterrupted claim began, which
//!   ranks it. Home use is a claim like any other, so a cold claim elsewhere sees it.
//! - **Live.** A claim is live while its row's `until` is ahead. A lease also lapses on a Mac whose
//!   Commons hears its peers and has not heard the lease's Mac within [`PRESENCE_TTL_S`]
//!   ([`Presence`], the retired broker's lease TTL): a Mac that left the LAN holds no lease there after
//!   that, and holds again once it is heard. Home use does not lapse so: the account's home Mac
//!   uses it wherever it is, so a cold claim elsewhere waits for its row's `until`. A Mac's own
//!   claims are live to it until `until`.
//! - **A split.** Each side settles what it hears, so a split longer than a claim's life can give
//!   an account one user on each side; at the heal the union holds both claims, every Mac picks
//!   the same senior one, and the junior yields at its next turn end. A claim's `since` survives
//!   its row's renewals, so a Mac that has heard only an older row of the senior still ranks it.
//! - **Few writes.** A row goes into the table of the UTC day it is written on, which takes a
//!   bounded number of rows and is deleted two days later, once every row in it has lapsed (a row
//!   lives [`HORIZON_S`], at most a day). So a Mac writes its row only when its claim set changes
//!   or `until` is under [`RENEW_WITHIN_S`] away ([`claimed`], [`released`]), and its usage tap
//!   reads the table at most once per [`RECHECK_S`] for each account.
//! - **Use.** A Mac keeps a claim while it uses the account ([`in_use`]): it claimed it (a launch
//!   or a usage tap) within [`IDLE_S`], or a session here was noted on it within
//!   [`SESSION_FRESH_S`]. Local stores combine that evidence under a private per-home lock,
//!   so an empty store cannot give up a different store's live claim. Evidence expires at its
//!   recorded deadline; a closed session may therefore keep its claim until then. Any other
//!   claim goes at the Mac's next write or look. The window looks every [`RECHECK_S`] (the node
//!   job, while no window runs). Before the Mac sleeps the window keeps what it still uses at
//!   most [`SESSION_FRESH_S`] more ([`quieted`]), so peers read its accounts free then.
//! - **Claude Code's accounts.** Only Claude Code's rows are claimed, as only they were leased:
//!   Claude Code alone has a usage tap to renew a claim, so another CLI's rows stay unclaimed.

use std::collections::BTreeMap;

use aterm_json::{Map, Value};

use super::accounts::{self, Cli, Roster};
use crate::ai::connector::Region;

#[path = "claims_activity.rs"]
mod activity;

/// How long a written row's claims last: a row written now lapses this much later unless written
/// again.
pub const HORIZON_S: i64 = 24 * 60 * 60;

/// A Mac writes its row again when its `until` is less than this far ahead.
pub const RENEW_WITHIN_S: i64 = 12 * 60 * 60;

/// A claim lapses on a Mac whose Commons last heard the claim's Mac longer ago than this.
pub const PRESENCE_TTL_S: i64 = 20 * 60;

/// A presence reading older than this says nothing: the observer stopped looking.
pub const PRESENCE_CURRENT_S: i64 = 60;

/// A claim this Mac has not claimed again (a launch or a usage tap) for this long, and no session
/// here was noted on within [`SESSION_FRESH_S`], is given up.
pub const IDLE_S: i64 = 60 * 60;

/// A session row noted this recently still counts as use: twice the hour the harness lets a
/// running session's row stand before it notes it again.
pub const SESSION_FRESH_S: i64 = 2 * 60 * 60;

/// The usage tap reads the table for one account at most this often.
pub const RECHECK_S: i64 = 5 * 60;

/// The most claims one row holds.
pub const MAX_CLAIMS: usize = 16;

/// The schema version a row carries.
const ROW_VERSION: u64 = 1;

/// Canonical `(provider_realm, email_lower)` identifier for a pooled account a claim names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LeaseKey {
    /// Canonical provider realm (`claude`, `codex`, `grok`, `kimi:global`, `kimi:mainland-cn`).
    pub pool: String,
    /// Lowercase ASCII-trimmed account email.
    pub email: String,
}

impl LeaseKey {
    /// Construct and validate a [`LeaseKey`].
    ///
    /// # Errors
    /// Returns a human-readable reason when `pool` names an unknown provider realm or `email`
    /// fails [`accounts::email_refusal`].
    pub fn new(pool: &str, email: &str) -> Result<Self, String> {
        let canon_pool = canonical_pool_realm(pool)
            .ok_or_else(|| format!("unknown lease provider realm `{pool}`"))?;
        let trimmed = email.trim_ascii();
        if let Some(why) = accounts::email_refusal(trimmed) {
            return Err(format!("invalid lease email: {why}"));
        }
        Ok(Self {
            pool: canon_pool.to_owned(),
            email: trimmed.to_ascii_lowercase(),
        })
    }

    /// Construct a [`LeaseKey`] from a [`Cli`] and optional [`Region`].
    ///
    /// # Errors
    /// Returns an error if `email` is invalid.
    pub fn for_cli(cli: Cli, region: Option<Region>, email: &str) -> Result<Self, String> {
        let realm = match (cli, region) {
            (Cli::Claude, _) => "claude",
            (Cli::Codex, _) => "codex",
            (Cli::Grok, _) => "grok",
            (Cli::Kimi, Some(Region::MainlandCn)) => "kimi:mainland-cn",
            (Cli::Kimi, Some(Region::Global) | None) => "kimi:global",
        };
        Self::new(realm, email)
    }
}

fn canonical_pool_realm(raw: &str) -> Option<&'static str> {
    match raw.trim_ascii().to_ascii_lowercase().as_str() {
        "" | "claude" | "anthropic" => Some("claude"),
        "codex" | "openai" => Some("codex"),
        "grok" | "x" | "xai" => Some("grok"),
        "kimi" | "kimi:global" | "kimi-global" => Some("kimi:global"),
        "kimi:mainland-cn" | "kimi-mainland-cn" | "kimi:cn" | "kimi-cn" => Some("kimi:mainland-cn"),
        _ => None,
    }
}

/// What a claim is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The account's home Mac (`warm[0]` in `ai-pool.toml`) uses it.
    Home,
    /// Any other Mac uses it.
    Lease,
}

impl Kind {
    /// The word a row carries.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Lease => "lease",
        }
    }

    /// Read a row's word; a word this build does not know is no claim it can rank.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "home" => Some(Self::Home),
            "lease" => Some(Self::Lease),
            _ => None,
        }
    }
}

/// One account one Mac claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The account.
    pub key: LeaseKey,
    /// Home use or a lease.
    pub kind: Kind,
    /// When this uninterrupted claim began (Unix seconds): the earlier ranks senior.
    pub since: i64,
}

/// One Mac's row: every account it claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The Commons machine (lowercase hex) of the binding that signed the row.
    pub machine: String,
    /// The Mac's `m<N>` label as it wrote it; empty when it has none.
    pub label: String,
    /// The row's revision: 1 for a Mac's first row, one more for each row after.
    pub counter: u64,
    /// The claims lapse at this instant (Unix seconds) unless the Mac writes its row again.
    pub until: i64,
    /// The claims, one per account, in account order.
    pub claims: Vec<Claim>,
}

impl Row {
    /// The row's body as the table stores it; the machine is the signer's, so it is not in it.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let claims = self
            .claims
            .iter()
            .map(|claim| {
                let mut c = Map::new();
                c.insert("pool".to_owned(), Value::from(claim.key.pool.as_str()));
                c.insert("email".to_owned(), Value::from(claim.key.email.as_str()));
                c.insert("kind".to_owned(), Value::from(claim.kind.word()));
                c.insert("since".to_owned(), Value::from(claim.since));
                Value::Object(c)
            })
            .collect();
        let mut body = Map::new();
        body.insert("v".to_owned(), Value::from(ROW_VERSION));
        body.insert("label".to_owned(), Value::from(self.label.as_str()));
        body.insert("counter".to_owned(), Value::from(self.counter));
        body.insert("until".to_owned(), Value::from(self.until));
        body.insert("claims".to_owned(), Value::Array(claims));
        Value::Object(body)
    }

    /// Read a row `machine` signed. A claim this build cannot read (an unknown kind or realm, a
    /// bad email) is left out; the rest of the row still counts.
    ///
    /// # Errors
    /// The body is not a row: not an object, no counter or `until`, or no claim list.
    pub fn from_value(machine: &str, body: &Value) -> Result<Self, String> {
        let object = body
            .as_object()
            .ok_or_else(|| "a claim row is not an object".to_owned())?;
        let counter = object
            .get("counter")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .ok_or_else(|| "a claim row has no counter".to_owned())?;
        let until = object
            .get("until")
            .and_then(Value::as_i64)
            .ok_or_else(|| "a claim row has no until".to_owned())?;
        let label = object
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim_ascii()
            .to_owned();
        let list = object
            .get("claims")
            .and_then(Value::as_array)
            .ok_or_else(|| "a claim row has no claim list".to_owned())?;
        let mut claims: Vec<Claim> = Vec::new();
        for item in list.iter().take(MAX_CLAIMS) {
            let read = || -> Option<Claim> {
                let item = item.as_object()?;
                let key = LeaseKey::new(item.get("pool")?.as_str()?, item.get("email")?.as_str()?)
                    .ok()?;
                Some(Claim {
                    key,
                    kind: Kind::from_word(item.get("kind")?.as_str()?)?,
                    since: item.get("since")?.as_i64()?,
                })
            };
            if let Some(claim) = read()
                && !claims.iter().any(|c| c.key == claim.key)
            {
                claims.push(claim);
            }
        }
        claims.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(Self {
            machine: machine.to_owned(),
            label,
            counter,
            until,
            claims,
        })
    }

    /// This row's claim on `key`.
    #[must_use]
    pub fn claim(&self, key: &LeaseKey) -> Option<&Claim> {
        self.claims.iter().find(|c| &c.key == key)
    }
}

/// When this Mac's Commons last heard each peer, as its Network State observed it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Presence {
    /// When the observation was made (Unix seconds).
    pub at: i64,
    /// Each peer's machine (lowercase hex) and when it was last heard (Unix seconds).
    pub heard: BTreeMap<String, i64>,
}

/// Who reads the rows: this Mac's machine and what its Commons hears.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    /// This Mac's Commons machine (lowercase hex).
    pub me: String,
    /// What this Mac's Commons hears, where it observes its peers.
    pub presence: Option<Presence>,
}

impl View {
    /// True where this Mac has current evidence that it has not heard `machine` for longer than
    /// [`PRESENCE_TTL_S`]. No observation, a stale or future one, or a machine it never heard, is no such
    /// evidence.
    #[must_use]
    pub fn unheard(&self, machine: &str, now: i64) -> bool {
        machine != self.me
            && self.presence.as_ref().is_some_and(|presence| {
                (0..=PRESENCE_CURRENT_S).contains(&now.saturating_sub(presence.at))
                    && presence
                        .heard
                        .get(machine)
                        .is_some_and(|last| presence.at.saturating_sub(*last) > PRESENCE_TTL_S)
            })
    }

    /// True while `row`'s `claim` is live to this Mac at `now`: its row has not lapsed and, for a
    /// lease, its Mac is not [`unheard`](Self::unheard).
    #[must_use]
    pub fn live(&self, row: &Row, claim: &Claim, now: i64) -> bool {
        row.until > now && (claim.kind == Kind::Home || !self.unheard(&row.machine, now))
    }
}

/// The Mac that holds an account, and its claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// Its Commons machine.
    pub machine: String,
    /// Its label as its row names it.
    pub label: String,
    /// Its claim's kind.
    pub kind: Kind,
    /// When its claim began.
    pub since: i64,
    /// When its row lapses.
    pub until: i64,
}

/// Whichever of two live claims on one account is senior: the earlier `since`, then the lower
/// label, then the lower machine (`DESIGN-alab-swarm` §6.4), so every Mac picks the same one.
#[must_use]
pub fn senior_lease<'a>(a: &'a Holder, b: &'a Holder) -> &'a Holder {
    if (a.since, &a.label, &a.machine) <= (b.since, &b.label, &b.machine) {
        a
    } else {
        b
    }
}

/// Every Mac's row as this Mac holds them: the union of what it wrote and what it heard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Book {
    rows: BTreeMap<String, Row>,
}

impl Book {
    /// A book of `rows`.
    #[must_use]
    pub fn of(rows: impl IntoIterator<Item = Row>) -> Self {
        let mut book = Self::default();
        for row in rows {
            book.insert(row);
        }
        book
    }

    /// Take `row`: of two rows from one Mac, the higher counter stands (then the later `until`).
    /// Rows from different Macs never replace each other.
    pub fn insert(&mut self, row: Row) {
        match self.rows.get(&row.machine) {
            Some(held) if (held.counter, held.until) >= (row.counter, row.until) => {}
            _ => {
                self.rows.insert(row.machine.clone(), row);
            }
        }
    }

    /// Take every row `other` holds: a heal is this union.
    pub fn merge(&mut self, other: &Self) {
        for row in other.rows.values() {
            self.insert(row.clone());
        }
    }

    /// `machine`'s row.
    #[must_use]
    pub fn row(&self, machine: &str) -> Option<&Row> {
        self.rows.get(machine)
    }

    /// Every row, by machine.
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.rows.values()
    }

    /// Every live claim on `key` as `view` sees it at `now`.
    pub fn live_claims<'a>(
        &'a self,
        key: &'a LeaseKey,
        view: &'a View,
        now: i64,
    ) -> impl Iterator<Item = Holder> + 'a {
        self.rows.values().filter_map(move |row| {
            let claim = row.claim(key).filter(|claim| view.live(row, claim, now))?;
            Some(Holder {
                machine: row.machine.clone(),
                label: row.label.clone(),
                kind: claim.kind,
                since: claim.since,
                until: row.until,
            })
        })
    }

    /// The senior live claim on `key` as `view` sees it at `now`: the earliest `since`, then the
    /// lower label, then the lower machine ([`senior_lease`]).
    #[must_use]
    pub fn senior(&self, key: &LeaseKey, view: &View, now: i64) -> Option<Holder> {
        self.live_claims(key, view, now)
            .reduce(|held, holder| senior_lease(&held, &holder).clone())
    }
}

/// This Mac, as its row names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mine {
    /// Its Commons machine.
    pub machine: String,
    /// Its label, or empty.
    pub label: String,
}

/// The row this Mac writes to claim `key` as `kind` at `now`, keeping each other claim `keep`
/// passes, or `None` where its row already holds just that and is not due again
/// ([`RENEW_WITHIN_S`]), or has no room for `key` ([`MAX_CLAIMS`]), when its live row does not
/// hold `key`. A claim the row already holds keeps its `since`; a lapsed row starts every claim
/// afresh.
#[must_use]
pub fn claimed(
    current: Option<&Row>,
    me: &Mine,
    key: &LeaseKey,
    kind: Kind,
    keep: impl Fn(&Claim) -> bool,
    now: i64,
) -> Option<Row> {
    let live = current.filter(|row| row.until > now);
    let mut claims: Vec<Claim> = live
        .map(|row| {
            row.claims
                .iter()
                .filter(|claim| &claim.key == key || keep(claim))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    match claims.iter_mut().find(|claim| &claim.key == key) {
        Some(claim) => claim.kind = kind,
        None => {
            if claims.len() >= MAX_CLAIMS {
                return None;
            }
            claims.push(Claim {
                key: key.clone(),
                kind,
                since: now,
            });
        }
    }
    claims.sort_by(|a, b| a.key.cmp(&b.key));
    if let Some(row) = live
        && row.claims == claims
        && row.until.saturating_sub(now) >= RENEW_WITHIN_S
        && row.label == me.label
    {
        return None;
    }
    Some(Row {
        machine: me.machine.clone(),
        label: me.label.clone(),
        counter: current.map_or(1, |row| row.counter.saturating_add(1)),
        until: now.saturating_add(HORIZON_S),
        claims,
    })
}

/// The row this Mac writes to give up each claim `drop` names at `now`, or `None` where its live
/// row holds none of them.
#[must_use]
pub fn released(current: Option<&Row>, drop: impl Fn(&Claim) -> bool, now: i64) -> Option<Row> {
    let row = current.filter(|row| row.until > now && row.claims.iter().any(&drop))?;
    Some(Row {
        counter: row.counter.saturating_add(1),
        claims: row
            .claims
            .iter()
            .filter(|claim| !drop(claim))
            .cloned()
            .collect(),
        ..row.clone()
    })
}

/// The row this Mac writes as it goes quiet at `now` (before it sleeps): each claim `drop` names
/// given up, and the rest kept no longer than [`SESSION_FRESH_S`] more, the most a claim it took
/// before the quiet can still count as use ([`in_use`]), so other Macs read a sleeping Mac's
/// accounts free then, not at its row's `until` up to a day away. `None` where its live row
/// changes neither way. The first claim after the quiet renews the row ([`RENEW_WITHIN_S`]), its
/// claims' `since` kept while the row is still live.
#[must_use]
pub fn quieted(current: Option<&Row>, drop: impl Fn(&Claim) -> bool, now: i64) -> Option<Row> {
    let row = current.filter(|row| row.until > now)?;
    let claims: Vec<Claim> = row
        .claims
        .iter()
        .filter(|claim| !drop(claim))
        .cloned()
        .collect();
    let until = if claims.is_empty() {
        row.until
    } else {
        row.until.min(now.saturating_add(SESSION_FRESH_S))
    };
    if claims.len() == row.claims.len() && until == row.until {
        return None;
    }
    Some(Row {
        counter: row.counter.saturating_add(1),
        until,
        claims,
        ..row.clone()
    })
}

/// The realms `cli`'s accounts are claimed in ([`LeaseKey::for_cli`]).
const fn realms(cli: Cli) -> &'static [&'static str] {
    match cli {
        Cli::Claude => &["claude"],
        Cli::Codex => &["codex"],
        Cli::Grok => &["grok"],
        Cli::Kimi => &["kimi:global", "kimi:mainland-cn"],
    }
}

/// Whether this Mac still uses `claim`'s account at `now`, as `state` (`cli`'s store) records
/// it: this Mac claimed one of `keys`' rows for it within [`IDLE_S`] (`State::claimed_at`: a
/// launch, or a running session's usage tap), or a session here was noted on that row within
/// [`SESSION_FRESH_S`]. This store records no use of a claim in another CLI's realm, so that one
/// lasts [`IDLE_S`] from its `since`.
#[must_use]
pub fn in_use(
    cli: Cli,
    state: &accounts::State,
    keys: &[(String, LeaseKey)],
    claim: &Claim,
    now: i64,
) -> bool {
    let within = |at: i64, life: i64| now.saturating_sub(at) < life;
    if !realms(cli).contains(&claim.key.pool.as_str()) {
        return within(claim.since, IDLE_S);
    }
    keys.iter().any(|(label, key)| {
        key == &claim.key
            && (state
                .claimed_at
                .get(label)
                .is_some_and(|at| within(*at, IDLE_S))
                || state.sessions.values().any(|session| {
                    &session.account == label && within(session.at, SESSION_FRESH_S)
                }))
    })
}

/// Each roster row's account: its label and its claim key, for every row with an email.
#[must_use]
pub fn keys(cli: Cli, roster: &Roster) -> Vec<(String, LeaseKey)> {
    roster
        .rows
        .iter()
        .filter_map(|row| {
            let email = row.email.as_deref()?;
            LeaseKey::for_cli(cli, row.vendor.region, email)
                .ok()
                .map(|key| (row.label.clone(), key))
        })
        .collect()
}

/// Record in `state` who holds each of `keys` as `book` reads to `view` at `now`: another Mac's
/// senior live claim makes the row `LeasedElsewhere` until its row lapses; this Mac's own senior
/// claim is held here; an account nobody claims holds nothing.
pub fn settle(
    state: &mut accounts::State,
    keys: &[(String, LeaseKey)],
    book: &Book,
    view: &View,
    now: i64,
) {
    let mine = state
        .swarm_label
        .clone()
        .unwrap_or_else(|| state.machine.clone());
    for (label, key) in keys {
        match book.senior(key, view, now) {
            Some(holder) if holder.machine == view.me => {
                state.note_lease(label, &mine, holder.until, holder.since);
            }
            Some(holder) => {
                // Another Mac's label can be this Mac's own while a label clash settles (§3.2):
                // its claim is still another Mac's.
                let named = if holder.label.is_empty() || state.owns_swarm_label(&holder.label) {
                    format!("claim:{}", holder.machine)
                } else {
                    holder.label
                };
                state.note_lease(label, &named, holder.until, holder.since);
            }
            None => state.clear_lease(label),
        }
    }
}

/// The Commons table the rows ride, as the process that links `aterm-db` installs it
/// ([`install`]).
pub trait Table: Send + Sync {
    /// Every row the table of the Commons under `home` holds, and this Mac's machine.
    ///
    /// # Errors
    /// This Mac has no Commons to read, or its table cannot be opened.
    fn read(&self, home: &std::path::Path) -> Result<Tabled, String>;

    /// Write this Mac's row into the table of the Commons under `home`.
    ///
    /// # Errors
    /// The row was not written: this Mac is no Commons writer, today's tables are full, or its
    /// writer lock was busy.
    fn write(&self, home: &std::path::Path, row: &Row) -> Result<(), String>;
}

/// What a [`Table`] read gives.
#[derive(Debug, Clone)]
pub struct Tabled {
    /// This Mac's Commons machine (lowercase hex).
    pub me: String,
    /// Every row the table holds.
    pub book: Book,
    /// Whether this Mac can write its row, and why not.
    pub writable: Result<(), String>,
}

/// What to do with this Mac's claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act<'a> {
    /// Read the rows and record who holds what.
    Look,
    /// This Mac goes quiet (it is about to sleep): [`Act::Look`], and the claims it keeps last at
    /// most [`SESSION_FRESH_S`] more ([`quieted`]).
    Quiet,
    /// Claim this row's account (a launch, or a session using it).
    Claim(&'a str),
}

/// What a [`touch`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Touched {
    /// Claims do not apply here: no `ai-pool.toml`, no table, or the row has no account.
    NotApplicable,
    /// The rows were read and recorded.
    Looked,
    /// This Mac's row already held the claim and is not due.
    Held,
    /// This Mac wrote its row with the claim.
    Claimed,
    /// Another Mac's senior live claim holds the account; this Mac claimed nothing.
    HeldElsewhere(Holder),
    /// This Mac could not read its account records or read/write its claim row; it keeps to
    /// its home accounts and the claims it holds.
    Unrecorded(String),
}

/// [`touch`] against `table` under `home` with `presence`, on `state` (`cli`'s store's, inside
/// its transaction), for the accounts `keys` names. This single-store primitive does not open
/// the per-home activity ledger; runtime callers use [`touch`], which combines all stores' use.
#[expect(
    clippy::too_many_arguments,
    reason = "the table, its home, presence, the store's state, its CLI and keys, the act and now"
)]
pub fn touch_with(
    table: &dyn Table,
    home: &std::path::Path,
    presence: Option<Presence>,
    state: &mut accounts::State,
    cli: Cli,
    keys: &[(String, LeaseKey)],
    act: Act<'_>,
    now: i64,
) -> Touched {
    touch_using(table, home, presence, state, cli, keys, act, now, Ok(None))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the claim operation plus optional durable machine-wide activity"
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimObservation",
        action = "Fault",
        reason = "Bounded environment scenario: the conformance test breaks the activity gate or its save, or makes the claim table unwritable or full, before Touch"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimObservation",
        action = "Peer",
        reason = "Bounded environment scenario: the conformance test writes a readable peer claim before Touch"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimObservation",
        action = "Skew",
        reason = "Bounded environment scenario: the conformance test moves the activity book's clock ahead before Touch"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimObservation",
        action = "OwnExpired",
        reason = "Bounded environment scenario: the conformance test writes this machine's own expiring claim before Touch"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "AccountClaimObservation",
        action = "Touch",
        project = "aterm_agent::harness::claims::activity::tests::observation_transition"
    )
)]
fn touch_using(
    table: &dyn Table,
    home: &std::path::Path,
    presence: Option<Presence>,
    state: &mut accounts::State,
    cli: Cli,
    keys: &[(String, LeaseKey)],
    act: Act<'_>,
    now: i64,
    activity: Result<Option<&mut activity::Held>, String>,
) -> Touched {
    let mut read = match table.read(home) {
        Ok(read) => read,
        Err(why) => {
            state.unrecorded = true;
            return Touched::Unrecorded(why);
        }
    };
    let view = View {
        me: read.me.clone(),
        presence,
    };
    let mine = Mine {
        machine: read.me.clone(),
        label: state.swarm_label.clone().unwrap_or_default(),
    };
    let key_of = |label: &str| {
        keys.iter()
            .find(|(row, _)| row == label)
            .map(|(_, key)| key.clone())
    };
    let observed_now = now;
    let (mut activity, mut refused) = match activity {
        Ok(active) => (active, None),
        Err(why) => (None, Some(why)),
    };
    let mine_now = read.book.row(&view.me).cloned();
    let now = match activity.as_mut() {
        Some(active) => match active.observe(&view.me, mine_now.as_ref(), state, keys, now) {
            Ok(now) => now,
            Err(why) => {
                refused = Some(why);
                now
            }
        },
        None => now,
    };
    // Peer expiry uses caller time, but our own already-lapsed row cannot outrank a
    // peer merely because this caller captured its timestamp before the local floor.
    if mine_now.as_ref().is_some_and(|row| row.until <= now) {
        read.book.rows.remove(&view.me);
    }
    let used = |claim: &Claim| {
        activity
            .as_ref()
            .is_some_and(|active| active.live(&claim.key, now))
            || in_use(cli, state, keys, claim, now)
    };
    let (outcome, row) = match act {
        Act::Look => (
            Touched::Looked,
            released(mine_now.as_ref(), |c| !used(c), now),
        ),
        Act::Quiet => (
            Touched::Looked,
            quieted(mine_now.as_ref(), |c| !used(c), now),
        ),
        Act::Claim(label) => match key_of(label) {
            None => (Touched::NotApplicable, None),
            Some(key) => {
                let kind = if state.home_accounts.contains(label) {
                    Kind::Home
                } else {
                    Kind::Lease
                };
                // A local clock floor never expires a peer's lease ahead of caller time.
                match read.book.senior(&key, &view, observed_now) {
                    Some(holder) if holder.machine != view.me => {
                        (Touched::HeldElsewhere(holder), None)
                    }
                    _ => match claimed(mine_now.as_ref(), &mine, &key, kind, used, now) {
                        Some(row) => (Touched::Claimed, Some(row)),
                        None if mine_now
                            .as_ref()
                            .filter(|row| row.until > now)
                            .and_then(|row| row.claim(&key))
                            .is_some() =>
                        {
                            (Touched::Held, None)
                        }
                        None => (
                            Touched::Unrecorded(format!(
                                "this Mac's claim row holds {MAX_CLAIMS} claims"
                            )),
                            None,
                        ),
                    },
                }
            }
        },
    };
    let record_claim = if let Act::Claim(label) = act {
        !matches!(outcome, Touched::NotApplicable | Touched::HeldElsewhere(_))
            && (state.home_accounts.contains(label)
                || matches!(outcome, Touched::Held)
                || (matches!(outcome, Touched::Claimed) && read.writable.is_ok()))
    } else {
        false
    };
    // Evidence must survive before a published row can depend on it. A failed row write
    // may leave conservative evidence until expiry, but cannot make a false claim succeed.
    if refused.is_none()
        && let Some(active) = activity.as_mut()
    {
        let recorded = (|| {
            if let Act::Claim(label) = act
                && record_claim
                && let Some(key) = key_of(label)
            {
                active.claim(key, now)?;
            }
            active.save()
        })();
        if let Err(why) = recorded {
            refused = Some(why);
        }
    }
    let outcome = match (refused.as_ref(), row) {
        (Some(_), _) if matches!(outcome, Touched::HeldElsewhere(_)) => outcome,
        (Some(why), _) => Touched::Unrecorded(why.clone()),
        (None, None) => outcome,
        (None, Some(row)) => match read.writable.clone().and_then(|()| table.write(home, &row)) {
            Ok(()) => {
                read.book.insert(row);
                outcome
            }
            Err(why) => Touched::Unrecorded(why),
        },
    };
    if let Act::Claim(label) = act
        && record_claim
        && refused.is_none()
    {
        state.claimed_at.insert(label.to_owned(), now);
    }
    state
        .claimed_at
        .retain(|_, at| now.saturating_sub(*at) < IDLE_S);
    // Local write failures must not hide a readable senior peer from turn-end advice.
    settle(state, keys, &read.book, &view, observed_now);
    state.unrecorded =
        refused.is_some() || read.writable.is_err() || matches!(outcome, Touched::Unrecorded(_));
    outcome
}

type PresenceRead = Box<dyn Fn() -> Option<Presence> + Send + Sync>;

static TABLE: std::sync::OnceLock<Box<dyn Table>> = std::sync::OnceLock::new();
static PRESENCE: std::sync::OnceLock<PresenceRead> = std::sync::OnceLock::new();
/// One claim at a time in this process: a row is read, changed and written whole.
static TOUCHING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Install the table this process's claims ride (`crates/aterm` installs the Commons one).
/// Once per process; a second install is ignored.
pub fn install(table: Box<dyn Table>) {
    let _ = TABLE.set(table);
}

/// Install this process's reader of what its Commons hears (the window's Network State).
/// Once per process; a second install is ignored.
pub fn install_presence(read: PresenceRead) {
    let _ = PRESENCE.set(read);
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static TEST_TABLE: std::cell::RefCell<Option<std::sync::Arc<dyn Table>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with `table` as this thread's claims table in place of the installed one.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn with_table<T>(table: std::sync::Arc<dyn Table>, f: impl FnOnce() -> T) -> T {
    let held = TEST_TABLE.with(|slot| slot.replace(Some(table)));
    let out = f();
    TEST_TABLE.with(|slot| *slot.borrow_mut() = held);
    out
}

#[cfg(test)]
thread_local! {
    static TEST_PRESENCE: std::cell::RefCell<Option<Presence>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with `presence` as what this thread's Commons hears, in place of the installed reader
/// (the census's tests read it; the census is Unix only).
#[cfg(all(test, unix))]
pub(crate) fn with_presence<T>(presence: Presence, f: impl FnOnce() -> T) -> T {
    let held = TEST_PRESENCE.with(|slot| slot.replace(Some(presence)));
    let out = f();
    TEST_PRESENCE.with(|slot| *slot.borrow_mut() = held);
    out
}

/// What this process's Commons hears now, where its installed reader observes its peers.
fn presence_now() -> Option<Presence> {
    #[cfg(test)]
    if let Some(presence) = TEST_PRESENCE.with(|slot| slot.borrow().clone()) {
        return Some(presence);
    }
    PRESENCE.get().and_then(|presence| presence())
}

/// `f` on this process's table, where one is installed.
fn on_table<T>(f: impl FnOnce(&dyn Table) -> T) -> Option<T> {
    #[cfg(any(test, debug_assertions))]
    if let Some(table) = TEST_TABLE.with(|slot| slot.borrow().clone()) {
        return Some(f(table.as_ref()));
    }
    TABLE.get().map(|table| f(table.as_ref()))
}

/// Every row the table under `home` holds and how this Mac sees them, where this process has a
/// table and it is readable.
#[must_use]
pub fn read_book(home: &std::path::Path) -> Option<(View, Book)> {
    let read = on_table(|table| table.read(home))?.ok()?;
    let view = View {
        me: read.me,
        presence: presence_now(),
    };
    Some((view, read.book))
}

/// Claim or look at `cli`'s accounts for this Mac at `now` (`act`), and record in its accounts
/// store who holds each row's account; either gives up claims whose combined activity across
/// this home's stores expired. Positive activity is saved before the claim row, under a private
/// per-home lock. Roster and binding reads share the account state writer lock. Unreadable
/// records or activity refuse publication. Claims apply to Claude Code where `ai-pool.toml`
/// is present and this process has a table ([`install`]).
pub fn touch(
    home: &std::path::Path,
    state_dir: &std::path::Path,
    roster_path: Option<&std::path::Path>,
    cli: Cli,
    act: Act<'_>,
    now: i64,
) -> Touched {
    if cli != Cli::Claude
        || !home
            .join(".secrets")
            .join(super::ai_pool::AI_POOL_FILE)
            .is_file()
    {
        return Touched::NotApplicable;
    }
    on_table(|table| touch_on(table, home, state_dir, roster_path, cli, act, now))
        .unwrap_or(Touched::NotApplicable)
}

fn touch_on(
    table: &dyn Table,
    home: &std::path::Path,
    state_dir: &std::path::Path,
    roster_path: Option<&std::path::Path>,
    cli: Cli,
    act: Act<'_>,
    now: i64,
) -> Touched {
    let store = accounts::Store::at_for(state_dir, cli, home);
    let path = roster_path.map_or_else(
        || home.join(".config/aterm").join(cli.accounts().roster_file),
        std::path::Path::to_path_buf,
    );
    touch_reading(table, home, &store, cli, act, now, || {
        Roster::load_for(cli, &path)
    })
}

fn touch_reading(
    table: &dyn Table,
    home: &std::path::Path,
    store: &accounts::Store,
    cli: Cli,
    act: Act<'_>,
    now: i64,
    read_roster: impl FnOnce() -> Result<Roster, String>,
) -> Touched {
    let _one = TOUCHING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let presence = presence_now();
    let mut outcome = Touched::NotApplicable;
    if let Err(why) = store.transact(now, &mut None, |state| {
        // The roster and bindings belong to this state transaction. A managed edit
        // cannot replace a label's identity while its old key is being claimed.
        let raw = read_roster()?;
        let (logins, _) = store.load_logins(now)?;
        let keys = keys(cli, &accounts::bound(&raw, &logins));
        let mut active = activity::Held::take(home);
        let activity = active
            .as_mut()
            .map(|held| Some(&mut *held))
            .map_err(|why| why.clone());
        outcome = touch_using(table, home, presence, state, cli, &keys, act, now, activity);
        Ok(Vec::new())
    }) {
        return Touched::Unrecorded(why);
    }
    outcome
}

/// The usage tap's claim on `label` ([`touch`] with [`Act::Claim`]), at most once per
/// [`RECHECK_S`] for each account of each store; `None` where it was not due.
pub fn tapped(
    home: &std::path::Path,
    state_dir: &std::path::Path,
    roster_path: Option<&std::path::Path>,
    cli: Cli,
    label: &str,
    now: i64,
) -> Option<Touched> {
    type Recent = BTreeMap<(std::path::PathBuf, String, String), i64>;
    static RECENT: std::sync::Mutex<Recent> = std::sync::Mutex::new(BTreeMap::new());
    {
        let mut recent = RECENT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        recent.retain(|_, at| (0..RECHECK_S).contains(&now.saturating_sub(*at)));
        let account = (
            state_dir.to_path_buf(),
            cli.id().to_owned(),
            label.to_owned(),
        );
        if recent.contains_key(&account) {
            return None;
        }
        recent.insert(account, now);
    }
    Some(touch(
        home,
        state_dir,
        roster_path,
        cli,
        Act::Claim(label),
        now,
    ))
}

#[cfg(test)]
#[path = "claims_tests.rs"]
pub(crate) mod tests;
