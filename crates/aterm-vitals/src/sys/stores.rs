// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's own durable stores, as `sys disk`'s `store` lines read them: each store by a fixed
//! word, what the disk walker measured of it, the bound its code holds it to, and how soon it
//! meets that bound. A figure nothing measured reads `unread`, never 0.

use std::collections::{BTreeMap, VecDeque};

use super::json::Jv;

/// The share of its bound at or past which a store warns, in percent.
pub const WARN_PCT: u64 = 75;
/// A store that meets its bound sooner than this warns, in seconds (48 hours).
pub const WARN_FULL_IN_S: u64 = 48 * 3600;
/// Why a store's figure has no value: nothing measured it ([`super::UNAVAILABLE_WORDS`]).
pub const UNREAD: &str = "unread";
/// The one warning word a store sets in the disk view's summary.
pub const WARNING_WORD: &str = "store-near-bound";

/// The fixed words a `store` line names a store by: never an entry's name, an account or an
/// address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StoreWord {
    /// The vault's verified membership-records log.
    VaultRecords,
    /// The copies `aterm secrets serve` keeps.
    VaultServed,
    /// One Commons table (the registry, a journal week, a claims day), numbered.
    CommonsTable,
    /// The lab journal's heard archive.
    JournalHeard,
    /// The lab journal's outbox.
    JournalOutbox,
    /// One harness account store (one per pooled CLI), numbered.
    HarnessAccounts,
    /// atpkg's package store.
    Packages,
    /// aterm's logs.
    Logs,
    /// The captures the `image` verb keeps.
    Images,
    /// The telemetry rows log.
    Telemetry,
}

impl StoreWord {
    /// Every word, in the order the lines print.
    pub const ALL: [Self; 10] = [
        Self::VaultRecords,
        Self::VaultServed,
        Self::CommonsTable,
        Self::JournalHeard,
        Self::JournalOutbox,
        Self::HarnessAccounts,
        Self::Packages,
        Self::Logs,
        Self::Images,
        Self::Telemetry,
    ];

    /// The word `name=` prints.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::VaultRecords => "vault-records",
            Self::VaultServed => "vault-served",
            Self::CommonsTable => "commons-table",
            Self::JournalHeard => "journal-heard",
            Self::JournalOutbox => "journal-outbox",
            Self::HarnessAccounts => "harness-accounts",
            Self::Packages => "packages",
            Self::Logs => "logs",
            Self::Images => "images",
            Self::Telemetry => "telemetry",
        }
    }
}

/// What a store does with a write that would pass its bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtBound {
    /// It refuses the write: a store near this bound is about to stop taking writes, so its
    /// share and its time to the bound warn.
    Refuses,
    /// It discards its oldest to make room: at its bound is where it stands once it has filled,
    /// so it never warns and has no time to the bound.
    Discards,
}

impl AtBound {
    /// The word `at_bound=` prints.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Refuses => "refuses",
            Self::Discards => "discards",
        }
    }
}

/// The bound a store's code holds it to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreBound {
    /// The most bytes of what it counts.
    pub bytes: u64,
    /// What the store does at it.
    pub at: AtBound,
    /// Whether it counts the logical bytes of the store's files, which a walk reads. `false`
    /// where it counts what the store holds inside its files (a Commons table's operations):
    /// no walk reads that, so the share of it in use is unread.
    pub walked: bool,
}

/// What a walk measured of one store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreSize {
    /// Allocated bytes, hard links shared fairly.
    pub alloc: u64,
    /// Logical bytes: what the store's bound counts.
    pub logical: u64,
    /// Files in it.
    pub files: u64,
    /// Seconds since the walk that measured it ended.
    pub age_s: Option<u64>,
}

/// When a store meets its bound at its growth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullIn {
    /// Its size or its growth was not measured.
    Unread,
    /// It does not grow: it never meets the bound at this growth.
    Never,
    /// Seconds to the bound (0 at or past it).
    In(u64),
}

/// One store aterm keeps on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreFact {
    /// Its fixed word.
    pub word: StoreWord,
    /// Its number among the stores of its word (1-based), where the word names several.
    pub n: Option<u32>,
    /// Where it lies: `home`, `tmp` or `volume` ([`super::under`]).
    pub under: &'static str,
    /// Its path under the home, of fixed components only; `None` where it lies outside it.
    pub rel: Option<String>,
    /// What was measured of it; `None` where nothing measured it.
    pub size: Option<StoreSize>,
    /// The bound its code holds it to, where it enforces one.
    pub bound: Option<StoreBound>,
    /// Its allocation's move between two measurements, bytes per hour (negative: freed).
    pub growth_per_h: Option<i64>,
    /// Its logical bytes' move between the same two measurements, bytes per hour: what the room
    /// left under a bound, which counts logical bytes, is divided by.
    pub fill_per_h: Option<i64>,
}

impl StoreFact {
    /// Its name as a sentence says it: the word, and its number where it has one.
    #[must_use]
    pub fn label(&self) -> String {
        match self.n {
            Some(n) => format!("{} {n}", self.word.word()),
            None => self.word.word().to_owned(),
        }
    }

    /// The line key a delta compares it by.
    #[must_use]
    pub fn key(&self) -> String {
        format!("store {}", self.label())
    }

    /// The bytes its bound counts and the bound, where both are known: a bound that counts the
    /// files' bytes ([`StoreBound::walked`]) and a measured size.
    fn share(&self) -> Option<(u64, u64)> {
        let bound = self.bound.filter(|b| b.walked && b.bytes > 0)?;
        Some((self.size?.logical, bound.bytes))
    }

    /// The share of its bound it holds, in whole percent, rounded down; `None` without a bound,
    /// without a measured size, or where the bound counts what no walk reads.
    #[must_use]
    pub fn used_pct(&self) -> Option<u64> {
        let (held, bound) = self.share()?;
        Some(u64::try_from(u128::from(held) * 100 / u128::from(bound)).unwrap_or(u64::MAX))
    }

    /// When it meets its bound; `None` without a bound, and for a store that discards its
    /// oldest there ([`AtBound::Discards`]), which is never full.
    #[must_use]
    pub fn full_in(&self) -> Option<FullIn> {
        let bound = self.bound.filter(|b| b.at == AtBound::Refuses)?;
        let (true, Some(size)) = (bound.walked, self.size) else {
            return Some(FullIn::Unread);
        };
        let left = bound.bytes.saturating_sub(size.logical);
        if left == 0 {
            return Some(FullIn::In(0));
        }
        Some(match self.fill_per_h {
            None => FullIn::Unread,
            Some(g) if g <= 0 => FullIn::Never,
            Some(g) => FullIn::In(
                u64::try_from(u128::from(left) * 3600 / u128::from(g.unsigned_abs()))
                    .unwrap_or(u64::MAX),
            ),
        })
    }

    /// Whether it refuses writes at a bound it holds [`WARN_PCT`] of or more.
    fn past_share(&self) -> bool {
        self.refuses()
            && self.share().is_some_and(|(held, bound)| {
                u128::from(held) * 100 >= u128::from(bound) * u128::from(WARN_PCT)
            })
    }

    /// Whether it refuses writes at its bound.
    fn refuses(&self) -> bool {
        self.bound.is_some_and(|b| b.at == AtBound::Refuses)
    }

    /// Whether it warns: a store that refuses writes at its bound ([`AtBound::Refuses`]) and is
    /// at or past [`WARN_PCT`] of it, or full within [`WARN_FULL_IN_S`]. A store that discards
    /// its oldest at its bound never warns: full is where it stands.
    #[must_use]
    pub fn warns(&self) -> bool {
        self.past_share() || matches!(self.full_in(), Some(FullIn::In(s)) if s < WARN_FULL_IN_S)
    }

    /// The `store` line.
    #[must_use]
    pub fn text(&self) -> String {
        let unread = || super::view::unavailable(UNREAD);
        let known = |v: Option<u64>| v.map_or_else(unread, |v| v.to_string());
        let mut out = format!("store name={}", self.word.word());
        if let Some(n) = self.n {
            out.push_str(&format!(" n={n}"));
        }
        out.push_str(&format!(" under={}", self.under));
        if let Some(rel) = self.rel.as_deref() {
            out.push_str(&format!(" path={}", super::pct(rel)));
        }
        out.push_str(&format!(
            " alloc_bytes={} logical_bytes={} files={}",
            known(self.size.map(|s| s.alloc)),
            known(self.size.map(|s| s.logical)),
            known(self.size.map(|s| s.files)),
        ));
        if let Some(bound) = self.bound {
            out.push_str(&format!(
                " bound_bytes={} at_bound={} used_pct={}",
                bound.bytes,
                bound.at.word(),
                known(self.used_pct())
            ));
        }
        out.push_str(&format!(
            " growth_bytes_per_h={}",
            self.growth_per_h.map_or_else(unread, |g| g.to_string())
        ));
        match self.full_in() {
            Some(FullIn::In(s)) => out.push_str(&format!(" full_in_s={s}")),
            Some(FullIn::Unread) => out.push_str(&format!(" full_in_s={}", unread())),
            Some(FullIn::Never) | None => {}
        }
        out.push_str(&format!(
            " walked_s={}",
            known(self.size.and_then(|s| s.age_s))
        ));
        out
    }

    /// The `stores` array's object: the line's keys.
    #[must_use]
    pub fn json(&self) -> Jv {
        let known = |v: Option<u64>| Jv::known_or(v, UNREAD);
        let mut o = Jv::obj([("name", Jv::s(self.word.word()))]);
        if let Some(n) = self.n {
            o.set("n", Jv::u(u64::from(n)));
        }
        o.set("under", Jv::s(self.under));
        if let Some(rel) = self.rel.as_deref() {
            o.set("path", Jv::s(rel));
        }
        o.set("alloc_bytes", known(self.size.map(|s| s.alloc)));
        o.set("logical_bytes", known(self.size.map(|s| s.logical)));
        o.set("files", known(self.size.map(|s| s.files)));
        if let Some(bound) = self.bound {
            o.set("bound_bytes", Jv::u(bound.bytes));
            o.set("at_bound", Jv::s(bound.at.word()));
            o.set("used_pct", known(self.used_pct()));
        }
        o.set(
            "growth_bytes_per_h",
            self.growth_per_h.map_or_else(|| Jv::why(UNREAD), Jv::Int),
        );
        match self.full_in() {
            Some(FullIn::In(s)) => o.set("full_in_s", Jv::u(s)),
            Some(FullIn::Unread) => o.set("full_in_s", Jv::why(UNREAD)),
            Some(FullIn::Never) | None => {}
        }
        o.set("walked_age_s", known(self.size.and_then(|s| s.age_s)));
        o
    }

    /// Its entry in the JSON's `store_warnings`: which store, its share, and when it is full
    /// where that is known.
    fn warning_json(&self) -> Jv {
        let mut o = Jv::obj([("name", Jv::s(self.word.word()))]);
        if let Some(n) = self.n {
            o.set("n", Jv::u(u64::from(n)));
        }
        o.set("used_pct", Jv::known_or(self.used_pct(), UNREAD));
        if let Some(FullIn::In(s)) = self.full_in() {
            o.set("full_in_s", Jv::u(s));
        }
        o
    }

    /// Its clause in the summary's warning: its share, and when it is full where that warns.
    fn warning_words(&self) -> String {
        let mut out = self.label();
        if let Some(pct) = self.used_pct() {
            out.push_str(&format!(" {pct}%"));
        }
        if let Some(FullIn::In(s)) = self.full_in().filter(|_| self.size.is_some())
            && s < WARN_FULL_IN_S
        {
            out.push_str(&format!(" full in {}h", s / 3600));
        }
        out
    }
}

/// Whether `a` holds a larger share of its bound than `b` does of its own, exactly.
fn fuller(a: &StoreFact, b: &StoreFact) -> std::cmp::Ordering {
    match (a.share(), b.share()) {
        (Some((al, ab)), Some((bl, bb))) => {
            (u128::from(al) * u128::from(bb)).cmp(&(u128::from(bl) * u128::from(ab)))
        }
        (a, b) => a.is_some().cmp(&b.is_some()),
    }
}

/// The store that holds the largest share of its bound, among those that refuse writes there
/// ([`AtBound::Refuses`]) and have a measured share: the one a survey row names. A store that
/// discards its oldest at its bound is nobody's worst, however full.
#[must_use]
pub fn worst(stores: &[StoreFact]) -> Option<&StoreFact> {
    stores
        .iter()
        .filter(|s| s.refuses() && s.used_pct().is_some())
        .reduce(|best, s| {
            if fuller(s, best) == std::cmp::Ordering::Greater {
                s
            } else {
                best
            }
        })
}

/// The stores that warn ([`StoreFact::warns`]), the fullest first, then the soonest full.
#[must_use]
pub fn warnings(stores: &[StoreFact]) -> Vec<&StoreFact> {
    let mut out: Vec<&StoreFact> = stores.iter().filter(|s| s.warns()).collect();
    let soonest = |s: &StoreFact| match s.full_in() {
        Some(FullIn::In(s)) => s,
        _ => u64::MAX,
    };
    out.sort_by(|a, b| fuller(b, a).then(soonest(a).cmp(&soonest(b))));
    out
}

/// The summary's store warning: [`WARNING_WORD`] and the worst store, with how many more warn;
/// `None` where none does.
#[must_use]
pub fn summary_clause(stores: &[StoreFact]) -> Option<String> {
    let warn = warnings(stores);
    let first = warn.first()?;
    let more = match warn.len() - 1 {
        0 => String::new(),
        n => format!("; {n} more"),
    };
    Some(format!("{WARNING_WORD} ({}{more})", first.warning_words()))
}

/// The JSON's `store_warnings`: the stores that warn, worst first.
#[must_use]
pub fn warnings_json(stores: &[StoreFact]) -> Jv {
    Jv::Arr(
        warnings(stores)
            .into_iter()
            .map(StoreFact::warning_json)
            .collect(),
    )
}

/// A kept sample is at least this long after the one before it, in seconds: a store measured
/// more often keeps only its newest size past the last kept one.
pub const SAMPLE_EVERY_S: u64 = 1800;
/// The most samples one store keeps.
pub const SAMPLES_KEPT: usize = 64;
/// A sample older than this is dropped, in seconds (7 days).
pub const SAMPLE_KEEP_S: u64 = 7 * 86_400;
/// The most stores a history keeps; those measured longest ago go first.
pub const HISTORY_STORES: usize = 96;
/// The span a growth figure looks back over, in seconds (24 hours): its base is the oldest
/// sample inside it, else the newest one before it, and after a decrease inside it the lowest
/// sample of that decrease ([`StoreHistory::growth`]).
pub const GROWTH_OVER_S: u64 = 86_400;
/// The least span between two samples a growth figure is made from, in seconds: under it the
/// figure is unread, not a rate read off one append.
pub const GROWTH_MIN_S: u64 = 600;
/// The most bytes a history's text is read at: [`HISTORY_STORES`] stores of [`SAMPLES_KEPT`]
/// samples fit under it.
pub const HISTORY_MAX_BYTES: usize = 1024 * 1024;
/// The first line of a history's text.
const HISTORY_HEAD: &str = "aterm-disk-stores 1";

/// One size a walk measured of a store, and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreSample {
    /// When, unix seconds.
    pub at_s: u64,
    /// Allocated bytes.
    pub alloc: u64,
    /// Logical bytes.
    pub logical: u64,
    /// Files.
    pub files: u64,
}

/// A store's move between two kept sizes, in bytes per hour (negative: it shrank).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Growth {
    /// Of its allocated bytes: what the disk gained.
    pub alloc: i64,
    /// Of its logical bytes: what a bound, which counts those, lost in room.
    pub logical: i64,
}

/// The walker's history of each store's size, by the store's key: what a growth per hour is
/// read from, and the number each store of a numbered word keeps, kept across restarts as text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreHistory {
    rows: BTreeMap<String, VecDeque<StoreSample>>,
    /// Each numbered store's number, by its key ([`Self::number`]).
    numbers: BTreeMap<String, u32>,
}

/// The key of the folder a child store's `key` (`<folder>/<name>`) lies in.
fn parent(key: &str) -> Option<&str> {
    key.rsplit_once('/').map(|(parent, _)| parent)
}

impl StoreHistory {
    /// Take `sample` as `key`'s newest. The sample before it stays only where it is
    /// [`SAMPLE_EVERY_S`] or more after the one before that, or where it is the low point of a
    /// decrease (below the one before it and not above the new one), which a growth figure
    /// starts from; samples at or after the new one's time (a clock set back) go, as do those
    /// past [`SAMPLES_KEPT`].
    pub fn note(&mut self, key: &str, sample: StoreSample) {
        let rows = self.rows.entry(key.to_owned()).or_default();
        while rows.back().is_some_and(|last| last.at_s >= sample.at_s) {
            rows.pop_back();
        }
        if let [.., before, last] = rows.make_contiguous()
            && last.at_s.saturating_sub(before.at_s) < SAMPLE_EVERY_S
            && !(last.alloc < before.alloc && last.alloc <= sample.alloc)
        {
            rows.pop_back();
        }
        rows.push_back(sample);
        while rows.len() > SAMPLES_KEPT {
            rows.pop_front();
        }
    }

    /// The number `key` (`<folder>/<name>`) keeps among the stores of its folder: the one it
    /// holds, else the least number from 1 that no store of that folder holds, which is its own
    /// from now until the history drops the store ([`Self::prune`]). So a store that comes or
    /// goes renumbers no other, and a line or a delta of one number is of one store.
    pub fn number(&mut self, key: &str) -> u32 {
        if let Some(n) = self.numbers.get(key) {
            return *n;
        }
        let folder = parent(key);
        let taken: std::collections::BTreeSet<u32> = self
            .numbers
            .iter()
            .filter(|(other, _)| parent(other) == folder)
            .map(|(_, n)| *n)
            .collect();
        let n = (1..=u32::MAX)
            .find(|n| !taken.contains(n))
            .unwrap_or(u32::MAX);
        self.numbers.insert(key.to_owned(), n);
        n
    }

    /// The number `key` holds ([`Self::number`]), where it holds one.
    #[must_use]
    pub fn number_of(&self, key: &str) -> Option<u32> {
        self.numbers.get(key).copied()
    }

    /// Drop the samples older than [`SAMPLE_KEEP_S`] at `now_s`, the stores left with none, and
    /// past [`HISTORY_STORES`] the stores measured longest ago.
    pub fn prune(&mut self, now_s: u64) {
        for rows in self.rows.values_mut() {
            while rows
                .front()
                .is_some_and(|s| now_s.saturating_sub(s.at_s) > SAMPLE_KEEP_S)
            {
                rows.pop_front();
            }
        }
        self.rows.retain(|_, rows| !rows.is_empty());
        while self.rows.len() > HISTORY_STORES {
            let Some(oldest) = self
                .rows
                .iter()
                .min_by_key(|(_, rows)| rows.back().map_or(0, |s| s.at_s))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.rows.remove(&oldest);
        }
        let rows = &self.rows;
        self.numbers.retain(|key, _| rows.contains_key(key));
    }

    /// `key`'s newest sample.
    #[must_use]
    pub fn latest(&self, key: &str) -> Option<StoreSample> {
        self.rows.get(key).and_then(|rows| rows.back().copied())
    }

    /// Every key under `prefix` (`<prefix>/<name>`) with its newest sample.
    pub fn under<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = (&'a str, StoreSample)> + 'a {
        self.rows.iter().filter_map(move |(key, rows)| {
            let name = key.strip_prefix(prefix)?.strip_prefix('/')?;
            (!name.is_empty() && !name.contains('/'))
                .then(|| rows.back().map(|s| (key.as_str(), *s)))
                .flatten()
        })
    }

    /// `key`'s growth in allocated bytes per hour ([`Self::growth`]).
    #[must_use]
    pub fn growth_per_h(&self, key: &str) -> Option<i64> {
        self.growth(key).map(|g| g.alloc)
    }

    /// `key`'s growth per hour between its newest sample and its base. The base is the oldest
    /// sample within [`GROWTH_OVER_S`] of the newest, else the newest one before that; and
    /// where the store shrank after that base and has been measured since, the low point of
    /// its last decrease, so a store that drained and fills again reads the rate it fills at,
    /// not a negative one for as long as the drain stays in the span. A store whose newest
    /// sample is itself the decrease reads its move over the whole span. `None` (unread) with
    /// one sample, or with a span under [`GROWTH_MIN_S`].
    #[must_use]
    pub fn growth(&self, key: &str) -> Option<Growth> {
        let rows = self.rows.get(key)?;
        let newest = rows.len().checked_sub(1)?;
        let latest = rows.get(newest)?;
        let first = (0..newest)
            .find(|i| {
                rows.get(*i)
                    .is_some_and(|s| latest.at_s.saturating_sub(s.at_s) <= GROWTH_OVER_S)
            })
            .or_else(|| newest.checked_sub(1))?;
        let low = (first + 1..newest).rev().find(
            |i| matches!((rows.get(i - 1), rows.get(*i)), (Some(a), Some(b)) if b.alloc < a.alloc),
        );
        let base = rows.get(low.unwrap_or(first))?;
        let span = latest.at_s.saturating_sub(base.at_s);
        if span < GROWTH_MIN_S {
            return None;
        }
        let per_h = |now: u64, then: u64| {
            i64::try_from((i128::from(now) - i128::from(then)) * 3600 / i128::from(span)).ok()
        };
        Some(Growth {
            alloc: per_h(latest.alloc, base.alloc)?,
            logical: per_h(latest.logical, base.logical)?,
        })
    }

    /// The history as text: a head line, then each store's key line, its number line where it
    /// holds one, and its sample lines.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = format!("{HISTORY_HEAD}\n");
        for (key, rows) in &self.rows {
            out.push_str(&format!("k {}\n", super::pct(key)));
            if let Some(n) = self.numbers.get(key) {
                out.push_str(&format!("n {n}\n"));
            }
            for s in rows {
                out.push_str(&format!(
                    "s {} {} {} {}\n",
                    s.at_s, s.alloc, s.logical, s.files
                ));
            }
        }
        out
    }

    /// The history `text` holds: none where its head line is another's, and a line that does
    /// not read is skipped, as is a number another store of the same folder already holds.
    #[must_use]
    pub fn decode(text: &str) -> Self {
        let mut lines = text.lines();
        let mut out = Self::default();
        if lines.next() != Some(HISTORY_HEAD) {
            return out;
        }
        let mut key: Option<String> = None;
        for line in lines {
            if let Some(k) = line.strip_prefix("k ") {
                key = unpct(k);
                continue;
            }
            let Some(key) = key.as_deref() else {
                continue;
            };
            if let Some(n) = line.strip_prefix("n ") {
                if let Ok(n) = n.parse::<u32>()
                    && n > 0
                    && !out
                        .numbers
                        .iter()
                        .any(|(other, held)| *held == n && parent(other) == parent(key))
                {
                    out.numbers.insert(key.to_owned(), n);
                }
                continue;
            }
            let Some(rest) = line.strip_prefix("s ") else {
                continue;
            };
            let mut n = rest.split(' ').map(str::parse::<u64>);
            if let (Some(Ok(at_s)), Some(Ok(alloc)), Some(Ok(logical)), Some(Ok(files)), None) =
                (n.next(), n.next(), n.next(), n.next(), n.next())
            {
                out.note(
                    key,
                    StoreSample {
                        at_s,
                        alloc,
                        logical,
                        files,
                    },
                );
            }
        }
        let rows = &out.rows;
        out.numbers.retain(|key, _| rows.contains_key(key));
        out
    }
}

/// [`super::pct`]'s inverse; `None` where an escape does not read or the bytes are not UTF-8.
fn unpct(text: &str) -> Option<String> {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
#[path = "stores_tests.rs"]
mod tests;
