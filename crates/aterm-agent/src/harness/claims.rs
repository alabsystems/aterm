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
//!   most [`SESSION_FRESH_S`] more ([`quieted`]), so peers read its accounts free then; as it
//!   shuts down, restarts or logs out it gives up every claim ([`Act::Off`]): the window once the
//!   system's quit went ahead or as a terminating signal ends it, the node job as it stops with no
//!   window open. Either row goes to the peers at once ([`touch_pushing`]), since a Mac about to
//!   sleep or stop may run no later repair round to carry it.
//! - **Providers.** Claude Code and Codex share the machine claim set. Each provider
//!   records its own session activity; local activity preserves the other provider's claims.
//! - **A wrong clock** (F22). Every row is placed by its writer's clock, so a Mac whose clock is
//!   a day or more off writes and reads tables its peers never read or write. The table's read
//!   says what this Mac's peers make of its clock ([`Tabled::clock`]), which peers it reads as
//!   skewed ([`Tabled::skewed`]) and which it holds for ([`Tabled::held`]: on a narrower line,
//!   every peer that counted an Off Mac against itself). A Mac they read **Off** writes no row
//!   (the table refuses it) and uses its home accounts whatever it sees, and no other
//!   ([`accounts::WhyNot::ClockOff`]); a Mac they are **split** over claims and releases its home
//!   accounts and nothing else. A Mac that is neither holds each held peer's home accounts for it
//!   ([`View::held`]) and keeps the peer's visible claims live past their `until` by as far as
//!   its clock runs behind, so it never takes an account the skewed Mac may be using and cannot
//!   show.

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

/// Providers whose account use participates in the shared machine claim set.
pub const CLAIMED_CLIS: [Cli; 2] = [Cli::Claude, Cli::Codex];

/// Canonical byte budget shared with the signed claims-table protocol.
pub const MAX_CLAIM_BYTES: usize = 60 * 1024;

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
            (Cli::Agy, _) => "agy",
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
        "agy" | "antigravity" => Some("agy"),
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
    fn within_budget(&self) -> bool {
        aterm_json::to_vec(&self.to_value()).is_ok_and(|bytes| bytes.len() <= MAX_CLAIM_BYTES)
    }
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
        if aterm_json::to_vec(body)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_CLAIM_BYTES
        {
            return Err("claim row exceeds byte budget".into());
        }
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
        for item in list {
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
    /// The home accounts of the peers this Mac holds for ([`Tabled::held`]), each held on its home
    /// Mac's behalf until the hold's `until` ([`holds`], F22): the earliest claim there is, so it
    /// is senior to every other.
    pub held: Vec<(LeaseKey, Holder)>,
    /// The peers this Mac holds for (machine, lowercase hex), each until its hold ends (a day past
    /// the reading): till then a row of theirs stays live past its `until` by as far as this Mac's
    /// clock runs ahead of theirs.
    pub skewed: BTreeMap<String, Skew>,
}

/// How a peer this Mac holds for reads on this Mac's clock (F22, [`View::skewed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Skew {
    /// The hold lasts until this instant (Unix seconds).
    pub until: i64,
    /// This Mac's clock runs at most this far ahead of the peer's (seconds, rounded up; zero or
    /// less where it does not run ahead): a row the peer dated to lapse at `until` lapses that
    /// much later here.
    pub ahead_s: i64,
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

    /// True while `row`'s `claim` is live to this Mac at `now`: its row has not lapsed on this
    /// Mac's clock (for a peer this Mac holds for, while that hold lasts, its `until` moved
    /// later by as far as this clock runs ahead of the peer's, [`Self::skewed`]), and, for a
    /// lease, its Mac is not [`unheard`](Self::unheard). A peer whose clock runs ahead, or a
    /// skew of minutes, moves it no later than the reading shows: a lease the peer left at its
    /// turn end but could not release (its clock read Off) lapses as it would have.
    #[must_use]
    pub fn live(&self, row: &Row, claim: &Claim, now: i64) -> bool {
        let late = self
            .skewed
            .get(&row.machine)
            .filter(|skew| skew.until > now)
            .map_or(0, |skew| skew.ahead_s.max(0));
        row.until.saturating_add(late) > now
            && (claim.kind == Kind::Home || !self.unheard(&row.machine, now))
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

    /// Every live claim on `key` as `view` sees it at `now`, and each hold `view` keeps on it for
    /// a skewed peer while the hold lasts ([`View::held`]).
    pub fn live_claims<'a>(
        &'a self,
        key: &'a LeaseKey,
        view: &'a View,
        now: i64,
    ) -> impl Iterator<Item = Holder> + 'a {
        let rows = self.rows.values().filter_map(move |row| {
            let claim = row.claim(key).filter(|claim| view.live(row, claim, now))?;
            Some(Holder {
                machine: row.machine.clone(),
                label: row.label.clone(),
                kind: claim.kind,
                since: claim.since,
                until: row.until,
            })
        });
        let held = view
            .held
            .iter()
            .filter(move |(held, holder)| held == key && holder.until > now)
            .map(|(_, holder)| holder.clone());
        rows.chain(held)
    }

    /// The senior live claim on `key` as `view` sees it at `now`: the earliest `since`, then the
    /// lower label, then the lower machine ([`senior_lease`]). A hold for a skewed peer
    /// ([`View::held`]) is the earliest of all.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountClaimClock",
            action = "BCold",
            project = "aterm_agent::harness::claims::tests::claim_clock_transition"
        )
    )]
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
/// ([`RENEW_WITHIN_S`]), or exceeds [`MAX_CLAIM_BYTES`], when its live row does not
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
    let row = Row {
        machine: me.machine.clone(),
        label: me.label.clone(),
        counter: current.map_or(1, |row| row.counter.saturating_add(1)),
        until: now.saturating_add(HORIZON_S),
        claims,
    };
    row.within_budget().then_some(row)
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
        Cli::Agy => &["agy"],
        Cli::Kimi => &["kimi:global", "kimi:mainland-cn"],
    }
}

/// Whether this Mac still uses `claim`'s account at `now`, as `state` (`cli`'s store) records
/// it: this Mac claimed one of `keys`' rows for it within [`IDLE_S`] (`State::claimed_at`: a
/// launch, or a running session's usage tap), or a session here was noted on that row within
/// [`SESSION_FRESH_S`]. This store records no use of a claim in another CLI's realm, so that one
/// lasts [`IDLE_S`] from its `since`. Preparation-only rows supply no execution evidence.
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
            && state.execution_verdict(label).is_ok()
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
            let email = row.expected_identity_email()?;
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

/// What its peers make of this Mac's clock (`Judge`), a peer's stale claim on its home row in its
/// view (`StalePeer`), and its clock being off as a peer reads it (`Skew`): none is code here.
/// `account_claim_clock_conforms` sets each the way the model does and binds what the code makes
/// of it.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimClock",
        action = "Judge",
        reason = "What this Mac's peers read of its clock comes from the Commons runtime's clock.state through the table's read (aterm-db's DbClaimClock): account_claim_clock_conforms sets the standing the model judges in the accounts store, and, into Off, the hold the same round gives a peer that counted it (aterm-db's every_peer_that_counted_an_off_mac_holds_for_it)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimClock",
        action = "StalePeer",
        reason = "A peer's stale lease on this Mac's home row is what the replicas carried: account_claim_clock_conforms writes it into the leases the store holds."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimClock",
        action = "Skew",
        reason = "Whether a peer holds for this Mac is that peer's clock reading (aterm-db's DbClaimClock): account_claim_clock_conforms names this Mac held in the peer's table read."
    )
)]
#[doc(hidden)]
pub fn account_claim_clock_environment() {}

/// How long a clock record a read takes lasts in the accounts store ([`accounts::Clock::until`]):
/// as long as the reading behind it stays fresh (`aterm_db::clock::FRESH_MS`).
pub const CLOCK_HOLD_S: i64 = 30 * 60;

/// Why `read`'s clock refuses a claim on a home account (`home`) or another one (F22): every claim
/// while this Mac's peers read its clock as off (the table's own refusal), and a claim on an
/// account that is not one of its homes while they are split over it.
fn clock_refusal(read: &Tabled, home: bool) -> Option<String> {
    let clock = read.clock.as_ref()?;
    match clock.standing {
        accounts::ClockStanding::Off { ahead } => {
            Some(read.writable.clone().err().unwrap_or_else(|| {
                format!(
                    "this Mac's clock reads {} of {} of the {} Macs it hears; it writes no account \
                 claim until the clock agrees",
                    if ahead { "ahead" } else { "behind" },
                    clock.against,
                    clock.of
                )
            }))
        }
        accounts::ClockStanding::Split if !home => Some(format!(
            "this Mac's clock disagrees with {} of the {} Macs it hears; it claims only its home \
             accounts until they agree",
            clock.against, clock.of
        )),
        accounts::ClockStanding::Split => None,
    }
}

/// Whether `label` names the Mac called `names` (its primary label and aliases): the same word,
/// or the same `m<N>` label however it is suffixed.
fn names(names: &[String], label: &str) -> bool {
    let label = label.trim_ascii();
    let canon = super::ai_pool::leading_m_label(label).map(|(_, canon)| canon);
    !label.is_empty()
        && names.iter().any(|name| {
            let name = name.trim_ascii();
            name.eq_ignore_ascii_case(label)
                || canon.is_some() && super::ai_pool::leading_m_label(name).map(|(_, c)| c) == canon
        })
}

/// The holds this Mac keeps for the peers it holds for ([`Tabled::held`], F22): each account in
/// `homes` (its key and its home Mac's label, `warm[0]`) whose home Mac is such a peer, held for
/// it with the earliest claim there is until its hold ends (a day past the reading). So no Mac
/// takes an account a skewed Mac may be using while its claims cannot be seen, and a Mac that
/// leases one yields it at its next turn end.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountClaimClock",
        action = "BRound",
        project = "aterm_agent::harness::claims::tests::claim_clock_transition"
    )
)]
pub fn holds(skewed: &[SkewedPeer], homes: &[(LeaseKey, String)]) -> Vec<(LeaseKey, Holder)> {
    homes
        .iter()
        .filter_map(|(key, home)| {
            let peer = skewed.iter().find(|peer| names(&peer.labels, home))?;
            Some((
                key.clone(),
                Holder {
                    machine: peer.machine.clone(),
                    label: home.clone(),
                    kind: Kind::Home,
                    since: i64::MIN,
                    until: peer.until,
                },
            ))
        })
        .collect()
}

/// An account `ai-pool.toml` names, with a home Mac: its label, its login address's
/// key and its home Mac's label (`warm[0]`).
pub(crate) type PoolHome = (String, LeaseKey, String);

/// Each `cli` account `ai-pool.toml` under `home` names with a home Mac ([`PoolHome`]);
/// none where the file cannot be read.
fn pool_homes(home: &std::path::Path, cli: Cli) -> Vec<PoolHome> {
    let Some(pool) = super::ai_pool::read_under(home) else {
        return Vec::new();
    };
    pool.accounts_for_cli(cli)
        .into_iter()
        .filter_map(|(account, label)| {
            let home = account.home_mac()?.to_owned();
            let key = LeaseKey::for_cli(cli, account.vendor.region, &account.email).ok()?;
            Some((label, key, home))
        })
        .collect()
}

/// The claim keys of `pool`'s accounts, each with its home Mac's label, as [`holds`] takes them:
/// the key [`keys`] gives the account's label (the roster's expected identity, which every claim
/// and [`Book::senior`] use), and its login address's key where that differs (a peer whose roster
/// still names the account by its login claims it so). A hold under either key holds the account
/// however a peer's roster keys it.
#[must_use]
pub(crate) fn home_keys(pool: &[PoolHome], keys: &[(String, LeaseKey)]) -> Vec<(LeaseKey, String)> {
    let mut homes = Vec::new();
    for (label, login, home) in pool {
        for (_, key) in keys.iter().filter(|(row, _)| row == label) {
            homes.push((key.clone(), home.clone()));
        }
        if !homes.iter().any(|(key, _)| key == login) {
            homes.push((login.clone(), home.clone()));
        }
    }
    homes
}

/// The claim keys of the supported accounts `ai-pool.toml` under `home` names, each with its
/// home Mac's label ([`home_keys`]), keyed through the accounts' roster file under `home`; a
/// roster that cannot be read keys them by login alone.
fn homes_at(home: &std::path::Path) -> Vec<(LeaseKey, String)> {
    CLAIMED_CLIS
        .into_iter()
        .flat_map(|cli| {
            let path = home.join(".config/aterm").join(cli.accounts().roster_file);
            let keys = Roster::load_for(cli, &path)
                .map(|roster| keys(cli, &roster))
                .unwrap_or_default();
            home_keys(&pool_homes(home, cli), &keys)
        })
        .collect()
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

    /// Hand this Mac's row in the table under `home` to its peers now, waiting until `deadline` at
    /// most for one to take it: the push a Mac makes as it goes quiet or down
    /// ([`touch_pushing`]), when no later repair round of its own may come.
    ///
    /// # Errors
    /// No peer took the row: this process serves no Commons repair, knows no peer, or the
    /// deadline came first. A table that hands nothing on says so.
    fn push(&self, home: &std::path::Path, deadline: std::time::Instant) -> Result<(), String> {
        let _ = (home, deadline);
        Err("this claims table hands nothing to peers".to_owned())
    }
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
    /// What this Mac's peers say of its clock, where they read it Split or Off (F22); `None` while
    /// they agree with it or it has no fresh reading.
    pub clock: Option<ClockRead>,
    /// The peers this Mac reads as skewed, unless its own clock is read Off (F22): the Macs a
    /// clock warning names.
    pub skewed: Vec<SkewedPeer>,
    /// The peers whose home accounts this Mac holds for them, unless its own clock is read Off
    /// (F22): every peer [`Self::skewed`] names, and every peer an Off Mac counted against itself
    /// (`aterm_db::clock::Judgement::held`).
    pub held: Vec<SkewedPeer>,
}

/// What this Mac's peers say of its clock, as a [`Table`] read gives it (F22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockRead {
    /// Split or Off.
    pub standing: accounts::ClockStanding,
    /// Under Off, at least how far off (seconds).
    pub by_s: i64,
    /// The decisive readings against this Mac's clock.
    pub against: u32,
    /// The decisive readings counted.
    pub of: u32,
}

/// A peer this Mac reads as skewed or holds for (F22): a fresh reading of its clock that does not
/// agree ([`Tabled::skewed`], [`Tabled::held`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkewedPeer {
    /// Its Commons machine (lowercase hex).
    pub machine: String,
    /// Its swarm labels, primary first, then its aliases; none where this Mac has no roster.
    pub labels: Vec<String>,
    /// Its holds last until this instant (Unix seconds).
    pub until: i64,
    /// This Mac's clock runs at most this far ahead of its (seconds, rounded up; zero or less
    /// where it does not run ahead): how much later its rows lapse here ([`Skew::ahead_s`]).
    pub ahead_s: i64,
}

impl SkewedPeer {
    /// How it reads on this Mac's clock ([`View::skewed`]).
    #[must_use]
    pub const fn skew(&self) -> Skew {
        Skew {
            until: self.until,
            ahead_s: self.ahead_s,
        }
    }
}

/// What to do with this Mac's claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act<'a> {
    /// Read the rows and record who holds what.
    Look,
    /// This Mac goes quiet (it is about to sleep): [`Act::Look`], and the claims it keeps last at
    /// most [`SESSION_FRESH_S`] more ([`quieted`]).
    Quiet,
    /// This Mac goes down (it shuts down, restarts or logs out, its window dies of a terminating
    /// signal, or its node job stops with no window open): every claim given up ([`released`]),
    /// so peers read its accounts free at once, not at its row's `until` up to a day away. No
    /// record keeps a claim then, so a roster, logins or activity record that cannot be read
    /// refuses none of it.
    Off,
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
    /// This Mac's row stands as it goes quiet or down ([`Act::Quiet`], [`Act::Off`]): written
    /// now, its claims given up or kept a shorter while, or already so (an earlier quiet, off or
    /// look wrote it, and its push may have reached no peer). The row [`touch_pushing`] hands to
    /// the peers.
    Quieted,
    /// Another Mac's senior live claim holds the account; this Mac claimed nothing.
    HeldElsewhere(Holder),
    /// The credential is held for preparation and cannot acquire execution claims.
    PreparationOnly,
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
    touch_using(
        table,
        home,
        presence,
        state,
        cli,
        keys,
        &[],
        act,
        now,
        Ok(None),
    )
}

/// [`touch_with`] with `homes` (each account's key and its home Mac's label) for the holds this Mac
/// keeps for the peers it reads as skewed ([`holds`]).
#[cfg(test)]
#[expect(
    clippy::too_many_arguments,
    reason = "touch_with's arguments and the accounts' home Macs"
)]
pub(crate) fn touch_homes(
    table: &dyn Table,
    presence: Option<Presence>,
    state: &mut accounts::State,
    cli: Cli,
    keys: &[(String, LeaseKey)],
    homes: &[(LeaseKey, String)],
    act: Act<'_>,
    now: i64,
) -> Touched {
    let home = std::path::Path::new("/nonexistent/home");
    touch_using(
        table,
        home,
        presence,
        state,
        cli,
        keys,
        homes,
        act,
        now,
        Ok(None),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the claim operation plus optional durable machine-wide activity"
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimObservation",
        action = "Prepare",
        reason = "Select a preparation-only credential before observing claims."
    )
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
    homes: &[(LeaseKey, String)],
    act: Act<'_>,
    now: i64,
    activity: Result<Option<&mut activity::Held>, String>,
) -> Touched {
    let mut read = match table.read(home) {
        Ok(read) => read,
        Err(why) => {
            state.unrecorded = true;
            return match act {
                Act::Claim(label) if state.execution_verdict(label).is_err() => {
                    Touched::PreparationOnly
                }
                _ => Touched::Unrecorded(why),
            };
        }
    };
    // What this Mac's peers say of its clock lasts in the store as long as the reading behind it
    // (F22), naming the peers it reads as skewed, and the peers it holds for have their home
    // accounts held for them.
    state.clock = read.clock.as_ref().map(|clock| accounts::Clock {
        standing: clock.standing,
        by_s: clock.by_s,
        against: clock.against,
        of: clock.of,
        peers: read
            .skewed
            .iter()
            .map(|peer| {
                peer.labels
                    .first()
                    .cloned()
                    .unwrap_or_else(|| format!("claim:{}", peer.machine))
            })
            .collect(),
        until: now.saturating_add(CLOCK_HOLD_S),
    });
    let view = View {
        me: read.me.clone(),
        presence,
        held: holds(&read.held, homes),
        skewed: read
            .held
            .iter()
            .map(|peer| (peer.machine.clone(), peer.skew()))
            .collect(),
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
    // A quiet or off goes to the peers wherever this Mac has a row, written now or not: the
    // row an earlier quiet, off or look wrote may never have reached them.
    let quiet = |row: Option<Row>| {
        let outcome = if row.is_some() || mine_now.is_some() {
            Touched::Quieted
        } else {
            Touched::Looked
        };
        (outcome, row)
    };
    let (outcome, row) = match act {
        Act::Look => (
            Touched::Looked,
            released(mine_now.as_ref(), |c| !used(c), now),
        ),
        Act::Quiet => quiet(quieted(mine_now.as_ref(), |c| !used(c), now)),
        Act::Off => quiet(released(mine_now.as_ref(), |_| true, now)),
        Act::Claim(label) if state.execution_verdict(label).is_err() => {
            (Touched::PreparationOnly, None)
        }
        // While its peers read its clock as off, this Mac writes no row (its home claims are
        // recorded as activity, and its peers hold its home rows for it); while they are split
        // over it, it claims its home accounts only.
        Act::Claim(label)
            if clock_refusal(&read, state.home_accounts.contains(label)).is_some() =>
        {
            (
                Touched::Unrecorded(
                    clock_refusal(&read, state.home_accounts.contains(label)).unwrap_or_default(),
                ),
                None,
            )
        }
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
                                "this Mac's claim row exceeds {MAX_CLAIM_BYTES} bytes"
                            )),
                            None,
                        ),
                    },
                }
            }
        },
    };
    let record_claim = if let Act::Claim(label) = act {
        !matches!(
            outcome,
            Touched::NotApplicable | Touched::HeldElsewhere(_) | Touched::PreparationOnly
        ) && (state.home_accounts.contains(label)
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
        (Some(_), _)
            if matches!(
                outcome,
                Touched::HeldElsewhere(_) | Touched::PreparationOnly
            ) =>
        {
            outcome
        }
        // An off's row gives up every claim and so depends on no evidence: a refused record
        // keeps nothing then.
        (Some(why), _) if !matches!(act, Act::Off) => Touched::Unrecorded(why.clone()),
        (_, None) => outcome,
        (_, Some(row)) => match read.writable.clone().and_then(|()| table.write(home, &row)) {
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
///
/// # Errors
/// The installed table cannot be read, including a busy membership roster. Absence means no
/// table is installed, never a transient failure of the table's read.
pub fn read_book(home: &std::path::Path) -> Result<Option<(View, Book)>, String> {
    let Some(read) = on_table(|table| table.read(home)).transpose()? else {
        return Ok(None);
    };
    let view = View {
        me: read.me,
        presence: presence_now(),
        skewed: read
            .held
            .iter()
            .map(|peer| (peer.machine.clone(), peer.skew()))
            .collect(),
        held: holds(&read.held, &homes_at(home)),
    };
    Ok(Some((view, read.book)))
}

/// Claim or look at `cli`'s accounts for this Mac at `now` (`act`), and record in its accounts
/// store who holds each row's account; either gives up claims whose combined activity across
/// this home's stores expired. Positive activity is saved before the claim row, under a private
/// per-home lock. Roster and binding reads share the account state writer lock. Unreadable
/// records or activity refuse publication. Claims apply to [`CLAIMED_CLIS`] where `ai-pool.toml`
/// is present and this process has a table ([`install`]).
pub fn touch(
    home: &std::path::Path,
    state_dir: &std::path::Path,
    roster_path: Option<&std::path::Path>,
    cli: Cli,
    act: Act<'_>,
    now: i64,
) -> Touched {
    if !applies(home, cli) {
        return Touched::NotApplicable;
    }
    on_table(|table| touch_on(table, home, state_dir, roster_path, cli, act, now))
        .unwrap_or(Touched::NotApplicable)
}

/// Whether claims apply to `cli` under `home`, where `ai-pool.toml` is present.
fn applies(home: &std::path::Path, cli: Cli) -> bool {
    CLAIMED_CLIS.contains(&cli)
        && home
            .join(".secrets")
            .join(super::ai_pool::AI_POOL_FILE)
            .is_file()
}

/// What [`touch_pushing`] did: the touch, and the push where its row went to the peers
/// ([`Table::push`]).
pub type Pushed = (Touched, Option<Result<(), String>>);

/// [`touch`] as this Mac goes quiet or down (`act` is [`Act::Quiet`] or [`Act::Off`]), then, where
/// it has a row ([`Touched::Quieted`]: written now or already so), hand the row to its peers now
/// ([`Table::push`]), waiting until `deadline` at most. Its Commons would otherwise carry the row
/// at its next repair round with a peer, which a Mac about to sleep or stop may never run.
pub fn touch_pushing(
    home: &std::path::Path,
    state_dir: &std::path::Path,
    roster_path: Option<&std::path::Path>,
    cli: Cli,
    act: Act<'_>,
    now: i64,
    deadline: std::time::Instant,
) -> Pushed {
    if !applies(home, cli) {
        return (Touched::NotApplicable, None);
    }
    on_table(|table| push_on(table, home, state_dir, roster_path, cli, act, now, deadline))
        .unwrap_or((Touched::NotApplicable, None))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the table, then touch_pushing's own arguments"
)]
fn push_on(
    table: &dyn Table,
    home: &std::path::Path,
    state_dir: &std::path::Path,
    roster_path: Option<&std::path::Path>,
    cli: Cli,
    act: Act<'_>,
    now: i64,
    deadline: std::time::Instant,
) -> Pushed {
    let touched = touch_on(table, home, state_dir, roster_path, cli, act, now);
    let pushed = (touched == Touched::Quieted).then(|| table.push(home, deadline));
    (touched, pushed)
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
    let off = matches!(act, Act::Off);
    let mut outcome = Touched::NotApplicable;
    if let Err(why) = store.transact(now, &mut None, |state| {
        if let Act::Claim(label) = act
            && state.execution_verdict(label).is_err()
        {
            // A later read or commit failure cannot replace a known execution refusal.
            outcome = Touched::PreparationOnly;
        }
        // The roster and bindings belong to this state transaction. A managed edit
        // cannot replace a label's identity while its old key is being claimed.
        let read = read_roster().and_then(|raw| {
            let (logins, _) = store.load_logins(now)?;
            Ok(keys(cli, &accounts::bound(&raw, &logins)))
        });
        // An off keeps no claim, so no record it cannot read refuses it: neither the roster and
        // logins nor the activity (busy, corrupt). A stopping Mac's claims never stand to their
        // row's `until` for want of evidence an off does not use.
        let keys = match read {
            Ok(keys) => keys,
            Err(_) if off => Vec::new(),
            Err(why) => return Err(why),
        };
        let mut active = activity::Held::take(home);
        let activity = match active.as_mut() {
            Ok(held) => Ok(Some(held)),
            Err(_) if off => Ok(None),
            Err(why) => Err(why.clone()),
        };
        let homes = if off {
            Vec::new()
        } else {
            home_keys(&pool_homes(home, cli), &keys)
        };
        outcome = touch_using(
            table, home, presence, state, cli, &keys, &homes, act, now, activity,
        );
        Ok(Vec::new())
    }) {
        return if matches!(
            outcome,
            Touched::PreparationOnly | Touched::HeldElsewhere(_)
        ) {
            outcome
        } else {
            Touched::Unrecorded(why)
        };
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
