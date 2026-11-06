// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node service's metrics publisher (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.14,
//! §11 Q4): metrics pushed, not polled. Once a minute it projects this Mac's own rows and
//! publishes on its `metrics.log` each one that changed:
//!
//! - one quota row per account row of each pooled CLI, from `harness::cli::survey_accounts` (its
//!   plan the one `aterm harness accounts` names) and the journal book's facts for the account
//!   ([`aterm_db::alab::journal::book_facts`]); an account the log holds a row of that the
//!   roster no longer lists is published once as removed ([`QuotaRow::ended`]);
//! - one agy row per home whose last reading the agy store keeps (`<harness>/agy-quota/`);
//! - one place row per placement the book holds (a placement the book no longer holds is
//!   published once as ended), and the pools no survey could read;
//! - the token windows: one `Spend` per Claude home and one set of rollout cursors per Codex home,
//!   kept while they are used, hand one sample per message or turn, and each closed minute and
//!   hour window whose summary grew is published, again at one revision more when a late sample
//!   grows it.
//!
//! Between passes each window pushes its needs rows to the node service
//! (`aterm_gui::install_window_needs`), and the Mac's `now/needs` row is published on change.
//!
//! The pass is the node service's own, not the journal flusher's: the flusher runs in whichever
//! process holds the Commons owner lease (a window, after a node restart), and not at all while
//! Commons is off, so a publisher on its pass would fall silent with the journal still on. The
//! book it reads is the one the flusher saves, whichever process runs it.
//!
//! Everything it reads is a store on this Mac: it never signs in, never asks a provider and never
//! reads another Mac. With no mesh open (the `MESH` switch off, a node without stores) it reads
//! and publishes nothing. `[alab] journal = false` stops the account rows and token windows, as it
//! stops the journal, and what was served while it was off is never published: its samples are
//! dropped and a window that spans it is withheld, by spans kept beside the journal's spool
//! (`metrics-off`) so a restart keeps them. The needs row is not account data and is published as
//! presence is. What a pass publishes is decided against what this Mac's log holds: the first
//! pass of a process reads its own shared rows back, so a restart neither republishes an
//! unchanged row nor restarts a window's revision at 0.

use astream_wire::summary::{Summary, Window};
use aterm_agent::harness::{
    accounts::{self, Condition},
    agy_quota_store, claude_homes,
    cli::{SurveyAccount, survey_accounts},
    codex_rollout::{self, Rollouts, Turn},
    disk::discover::{ClaudeCensus, claude_dirs, looked_at, running_claude_census},
    spend::{self, Limits, PriceTable, Served, Spend},
    upgrade_models::family_version,
};
use aterm_db::alab::journal::{self as journal, BookFacts, Pk, Spool};
use aterm_metrics::row::{
    AgyGroup, AgyRow, Label, ManagerEntry, NEEDS_MAX, NeedsRow, PCT_MAX, PlaceRow, QuotaRow,
    REMAINING_MAX, Row, SignIn, UnreadRow, WindowRecord, Word,
};
use aterm_metrics::rules::{HOUR_AGE, MINUTE_AGE};
use aterm_metrics::subject::{
    AccountKey, Class, Cli, Family, HomeKey, Metric, Node, Now, Subject, TokenClass, Width,
};
use aterm_types::MutexExt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How often the node service runs a pass.
const PASS_EVERY: Duration = Duration::from_secs(60);

/// The most bytes one pass reads from the transcripts of one Claude home, or from one Codex
/// home's rollouts. A first pass that reads less than the horizon holds goes on next pass.
const FOLD_BYTES: u64 = 64 << 20;

/// How long one pass may fold.
const FOLD_FOR: Duration = Duration::from_secs(20);

/// How long a sample matters: the hour class's retention, and the hour it rounds into.
const HORIZON_S: i64 = HOUR_AGE.as_secs().cast_signed() + 3600;

/// The most samples held, and apart from them the most keys of messages served while the journal
/// was off. Past it a new sample is not taken, and every window of its CLI that spans its time is
/// withheld while retention keeps it: a window summarizing part of its samples is never
/// published.
const SAMPLES_MAX: usize = 1 << 20;

/// The most homes of one CLI folded at once. A Claude home is folded while it is listed (the
/// default, a login's, a running Claude Code's, one a launch noted within the horizon), and after
/// that while it served within the horizon and its folder is there; a listed home past it takes the place of the one not listed
/// that served longest ago, and where every one held is listed, the CLI's windows of the horizon
/// are withheld.
const HOMES_MAX: usize = 64;

/// How far past a pass's `now` a fold takes a row's time as written, for a row written while the
/// pass runs ([`FOLD_FOR`] and the listing before it).
const FOLD_AHEAD_S: i64 = 60;

/// The file beside the journal's spool that keeps the spans `[alab] journal` was off.
const OFF_FILE: &str = "metrics-off";

/// The most off spans kept: past it the oldest two are joined into one.
const OFF_SPANS_MAX: usize = 64;

/// How long a read of this Mac's own rows on its log may wait.
const READ_WITHIN: Duration = Duration::from_secs(30);

/// The most pages one read-back takes of one window width.
const READ_PAGES_MAX: usize = 256;

/// The records one page of a read-back asks for.
const READ_PAGE: u32 = 256;

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of change detection (`metrics-every-row`): a pass on a thread that
    /// sets it publishes every row it projects, changed or not, as a publisher that compared
    /// nothing would. A lab instance turns it on for its whole process with `metrics-every-row`
    /// in `ATERM_TEST_NEGATIVE_CONTROL`.
    pub(crate) static EVERY_ROW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`EVERY_ROW`] is on: set on this thread, or named for this process.
#[cfg(any(test, debug_assertions))]
fn every_row() -> bool {
    EVERY_ROW.with(std::cell::Cell::get) || aterm_types::negative_control::on("metrics-every-row")
}

/// Where the rows go: this Mac's `metrics.log`, or a test's.
pub(crate) trait Log {
    /// The Mac the log publishes as.
    fn node(&self) -> Node;

    /// Publish `body` on `subject`.
    ///
    /// # Errors
    /// What the log refuses, `WouldBlock` while its lease is not settled.
    fn publish(&self, subject: &str, body: &[u8]) -> io::Result<()>;

    /// What this Mac shares on the log now: the newest row of each snapshot subject (quota and
    /// now rows) and every window record retention keeps, oldest first.
    ///
    /// # Errors
    /// Where a read cannot be made or is refused.
    fn published(&self) -> io::Result<Vec<(String, Vec<u8>)>>;
}

/// The node service's `metrics.log` as a [`Log`].
struct NodeLog {
    mesh: Arc<aterm_mesh::Mesh>,
    node: Node,
}

impl NodeLog {
    /// The mesh the node service runs now, if any.
    fn current() -> Option<Self> {
        let mesh = aterm_mesh::current()?;
        let node = Node::parse(mesh.principal())?;
        Some(Self { mesh, node })
    }
}

impl Log for NodeLog {
    fn node(&self) -> Node {
        self.node
    }

    fn publish(&self, subject: &str, body: &[u8]) -> io::Result<()> {
        self.mesh
            .publish(aterm_mesh::MeshLog::Metrics, subject, body)
            .map(drop)
    }

    fn published(&self) -> io::Result<Vec<(String, Vec<u8>)>> {
        let shared = aterm_metrics::subject::shared_of(self.node);
        let mut client =
            self.mesh
                .read_client(aterm_mesh::MeshLog::Metrics, &shared, Some(READ_WITHIN))?;
        let mut out = Vec::new();
        let opened = |body: Vec<u8>| astream_broker::origin::payload(&body).map(<[u8]>::to_vec);
        // The snapshot classes keep their last row each, whatever its age: the last-value face.
        let (face, _) = client.last_all(&shared)?;
        for (_, subject, body) in face {
            let snapshot = Subject::parse(&subject)
                .is_ok_and(|s| matches!(s.class(), Class::Quota | Class::Now));
            if let (true, Some(payload)) = (snapshot, opened(body)) {
                out.push((subject, payload));
            }
        }
        // Each width's records, every window retention still holds, read from its floor.
        for width in Width::ALL {
            let filter = format!("/m/{}/shared/{}/*", self.node, width.segment());
            let (oldest, head) = client.oldest(&filter)?;
            let mut from = oldest.unwrap_or(0);
            let mut pages = 0;
            while from < head {
                pages += 1;
                if pages > READ_PAGES_MAX {
                    return Err(io::Error::other(format!(
                        "{filter} did not read back within {READ_PAGES_MAX} pages"
                    )));
                }
                let (records, (next, _)) = client.fetch(from, &filter, READ_PAGE)?;
                for (_, subject, body) in records {
                    if let Some(payload) = opened(body) {
                        out.push((subject, payload));
                    }
                }
                if next <= from {
                    break;
                }
                from = next;
            }
        }
        Ok(out)
    }
}

/// The homes one CLI's folds read now, and whether they are every home it can serve from.
pub(crate) struct Homes {
    pub(crate) dirs: Vec<PathBuf>,
    /// Whether every place a home is found in was read: `false` while the running processes or
    /// the account records could not be, when a home may be missing and the CLI's windows wait.
    pub(crate) complete: bool,
}

/// Where the publisher reads this Mac's stores.
pub(crate) struct Places {
    /// The `aterm.toml` whose rosters list the accounts.
    pub(crate) config: Option<PathBuf>,
    /// The harness state: the accounts store and the agy readings.
    pub(crate) harness: Option<PathBuf>,
    /// The person's home, where the non-Claude stores are.
    pub(crate) home: Option<PathBuf>,
    /// The journal's spool, beside which its book and the off spans ([`OFF_FILE`]) are kept.
    pub(crate) spool: Option<Spool>,
    /// The Claude homes at a time ([`claude_homes_at`]): the default one, each login home the
    /// accounts records name, each home a running Claude Code uses and each one a launch noted.
    pub(crate) claude_homes: Box<dyn Fn(i64) -> Homes + Send + Sync>,
    /// The Codex homes at a time: the default one and each login home the accounts records name.
    pub(crate) codex_homes: Box<dyn Fn(i64) -> Homes + Send + Sync>,
}

impl Places {
    /// The node service's.
    fn of_process() -> Self {
        let home = aterm_types::dirs::home_dir();
        let config = aterm_types::dirs::aterm_config_path();
        let harness = aterm_agent::harness::cli::harness_state_dir().ok();
        let codex = std::env::var("CODEX_HOME").ok();
        let codex_home = codex_rollout::codex_home(
            codex.as_deref(),
            home.as_deref().and_then(Path::to_str),
            None,
        );
        let records = (config.clone(), harness.clone(), home.clone());
        let claude_records = records.clone();
        Self {
            config,
            harness,
            home,
            spool: Spool::local(),
            claude_homes: Box::new(move |now| {
                let census = running_claude_census(claude_records.2.as_deref());
                claude_homes_at(&claude_records, census, now)
            }),
            codex_homes: Box::new(move |now| {
                let logins = login_homes(&records, accounts::Cli::Codex, now);
                let mut dirs: Vec<PathBuf> = codex_home.iter().cloned().collect();
                for dir in logins.iter().flatten() {
                    if !dirs.contains(dir) {
                        dirs.push(dir.clone());
                    }
                }
                Homes {
                    dirs,
                    complete: logins.is_some(),
                }
            }),
        }
    }
}

/// Where the account records are: `(aterm.toml, the harness state, the person's home)`.
type Records = (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>);

/// The Claude homes a pass folds at `now`: the default one, each login home the account records
/// name ([`login_homes`]), each home a running Claude Code uses (`census`), and each home a launch
/// in an aterm tab noted within the horizon ([`claude_homes::read`]), which names a run that
/// started and ended between two passes. A noted home is judged again by this person's home, as
/// the census judges a running one ([`looked_at`]), so none under a folder a read could raise a
/// privacy prompt for is folded. Not complete while the census or the records could not be read,
/// when a home may be missing.
pub(crate) fn claude_homes_at(records: &Records, census: ClaudeCensus, now: i64) -> Homes {
    let (_, harness, home) = records;
    let logins = login_homes(records, accounts::Cli::Claude, now);
    let complete = census.is_ok() && logins.is_some();
    let running = census.map(|running| running.dirs).unwrap_or_default();
    let since = now.saturating_sub(HORIZON_S);
    let launched = harness
        .iter()
        .flat_map(|harness| claude_homes::read(harness, now))
        .filter(|(dir, at)| {
            *at >= since && home.as_deref().is_some_and(|h| looked_at(dir, Some(h)))
        })
        .map(|(dir, _)| dir);
    let known = logins
        .unwrap_or_default()
        .into_iter()
        .chain(running)
        .chain(launched);
    Homes {
        dirs: claude_dirs(home.as_deref(), known),
        complete,
    }
}

/// Every login home `cli`'s account records name (each row's, its renewals', and the ones they
/// bound it away from), from `(config, harness state, home)`; `None` where they cannot be read.
fn login_homes(
    (config, harness, home): &Records,
    cli: accounts::Cli,
    now: i64,
) -> Option<Vec<PathBuf>> {
    let (Some(config), Some(harness)) = (config, harness) else {
        return None;
    };
    let roster = accounts::roster_beside_for(config, cli)?;
    let store = match (cli, home) {
        (accounts::Cli::Claude, _) => accounts::Store::at(harness),
        (_, Some(home)) => accounts::Store::at_for(harness, cli, home),
        (_, None) => return None,
    };
    let records = store.read_account_records(Some(&roster), now).ok()?;
    let rows = records.raw_roster.rows.iter().chain(&records.roster.rows);
    let named = rows
        .flat_map(|row| std::iter::once(&row.dir).chain(&row.was))
        .chain(
            records
                .logins
                .labels
                .values()
                .flat_map(|binding| std::iter::once(&binding.dir).chain(&binding.was)),
        );
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in named.flatten().map(PathBuf::from) {
        if dir.is_absolute() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    Some(dirs)
}

/// The spans of serve time whose samples are not all held: a sample served in one is not taken,
/// and a window that overlaps one is never published.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Gaps {
    /// When `[alab] journal` was off, `[since, until)`, Unix seconds; the last open (`None`)
    /// while it is off still.
    off: Vec<(i64, Option<i64>)>,
    /// Each CLI's samples not taken at [`SAMPLES_MAX`]: the first and the last time served.
    lost: BTreeMap<Cli, (i64, i64)>,
}

impl Gaps {
    /// Whether `at` was served while the journal was off.
    fn off_at(&self, at: i64) -> bool {
        self.off
            .iter()
            .any(|(since, until)| *since <= at && until.is_none_or(|until| at < until))
    }

    /// Whether `cli`'s `window` spans a sample that is not held.
    fn withholds(&self, cli: Cli, window: Window) -> bool {
        let (start, end) = (ms_to_s(window.start_ms()), ms_to_s(window.end_ms()));
        let overlaps = |since: i64, until: i64| since < end && start < until;
        self.off
            .iter()
            .any(|(since, until)| overlaps(*since, until.unwrap_or(i64::MAX)))
            || self
                .lost
                .get(&cli)
                .is_some_and(|(first, last)| overlaps(*first, last.saturating_add(1)))
    }

    fn lose(&mut self, cli: Cli, at: i64) {
        let span = self.lost.entry(cli).or_insert((at, at));
        *span = (span.0.min(at), span.1.max(at));
    }

    /// Forget what ended before `before`: nothing it spans is published any more.
    fn expire(&mut self, before: i64) {
        self.off
            .retain(|(_, until)| until.is_none_or(|until| until >= before));
        self.lost.retain(|_, (_, last)| *last >= before);
    }

    /// The off spans as [`OFF_FILE`] keeps them: `<since> <until>` a line, `-` for one still open.
    fn off_text(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for (since, until) in &self.off {
            match until {
                Some(until) => {
                    let _ = writeln!(out, "{since} {until}");
                }
                None => {
                    let _ = writeln!(out, "{since} -");
                }
            }
        }
        out
    }

    /// The off spans `text` keeps; a line that does not read is skipped.
    fn off_of(text: &str) -> Vec<(i64, Option<i64>)> {
        text.lines()
            .filter_map(|line| {
                let (since, until) = line.split_once(' ')?;
                let since = since.parse().ok()?;
                let until = match until {
                    "-" => None,
                    until => Some(until.parse().ok()?),
                };
                Some((since, until))
            })
            .take(OFF_SPANS_MAX)
            .collect()
    }
}

fn ms_to_s(ms: u64) -> i64 {
    i64::try_from(ms / 1000).unwrap_or(i64::MAX)
}

/// One served message or turn, as a window counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    /// Unix seconds it was served: its first row's, kept through every raise.
    at: i64,
    family: Family,
    /// Its tokens of each class, in [`TokenClass::ALL`]'s order.
    tokens: [u64; 4],
}

impl Sample {
    fn total(&self) -> u64 {
        self.tokens.iter().fold(0, |sum, n| sum.saturating_add(*n))
    }
}

/// The samples of the horizon: by key, where the source keys them (a raise replaces the counts
/// its key holds), and the unkeyed ones, each handed once.
#[derive(Default)]
struct Samples {
    keyed: BTreeMap<(Cli, u64), Sample>,
    loose: Vec<(Cli, Sample)>,
    /// The keys of messages served while the journal was off, with when, so a later raise of one
    /// is not taken either; at most [`SAMPLES_MAX`], past which a raise served after its span is.
    void: BTreeMap<(Cli, u64), i64>,
}

impl Samples {
    fn take(&mut self, gaps: &mut Gaps, cli: Cli, key: Option<u64>, sample: Sample) {
        if let Some(key) = key {
            if self.void.contains_key(&(cli, key)) {
                return;
            }
            if let Some(held) = self.keyed.get_mut(&(cli, key)) {
                held.tokens = sample.tokens;
                return;
            }
        }
        if gaps.off_at(sample.at) {
            if let Some(key) = key {
                if self.void.len() < SAMPLES_MAX {
                    self.void.insert((cli, key), sample.at);
                } else {
                    gaps.lose(cli, sample.at);
                }
            }
            return;
        }
        if self.keyed.len() + self.loose.len() >= SAMPLES_MAX {
            gaps.lose(cli, sample.at);
            return;
        }
        match key {
            Some(key) => {
                self.keyed.insert((cli, key), sample);
            }
            None => self.loose.push((cli, sample)),
        }
    }

    fn expire(&mut self, before: i64) {
        self.keyed.retain(|_, s| s.at >= before);
        self.loose.retain(|(_, s)| s.at >= before);
        self.void.retain(|_, at| *at >= before);
    }

    fn each(&self) -> impl Iterator<Item = (Cli, &Sample)> {
        self.keyed
            .iter()
            .map(|((cli, _), s)| (*cli, s))
            .chain(self.loose.iter().map(|(cli, s)| (*cli, s)))
    }

    /// Every window of the CLIs in `whole` that `gaps` does not withhold, with its summary: each
    /// sample in its minute total and its hour's class of its family.
    fn windows(&self, whole: &BTreeSet<Cli>, gaps: &Gaps) -> BTreeMap<(Metric, u64), Summary> {
        let mut out: BTreeMap<(Metric, u64), Summary> = BTreeMap::new();
        let mut put = |metric: Metric, at_ms: u64, value: u64| {
            if let Some(window) = Window::containing(at_ms, metric.width().window_len())
                && !gaps.withholds(metric.cli(), window)
            {
                out.entry((metric, window.start_ms()))
                    .or_default()
                    .record(value);
            }
        };
        for (cli, sample) in self.each().filter(|(cli, _)| whole.contains(cli)) {
            let Some(at_ms) = u64::try_from(sample.at)
                .ok()
                .and_then(|s| s.checked_mul(1000))
            else {
                continue;
            };
            if let Some(metric) = Metric::tokens(cli) {
                put(metric, at_ms, sample.total());
            }
            for (class, value) in TokenClass::ALL.into_iter().zip(sample.tokens) {
                if let Some(metric) = Metric::class(cli, sample.family, class) {
                    put(metric, at_ms, value);
                }
            }
        }
        out
    }
}

/// A Claude home's spend, and when it last served or was found running.
struct Home {
    spend: Spend,
    last: i64,
}

/// What the folds keep while the process runs.
struct Folds {
    prices: PriceTable,
    claude: BTreeMap<PathBuf, Home>,
    codex: BTreeMap<PathBuf, Rollouts>,
    samples: Samples,
    gaps: Gaps,
    /// When the last pass that saw the journal on ran.
    last_on: Option<i64>,
}

impl Folds {
    fn new(gaps: Gaps) -> Self {
        Self {
            prices: PriceTable::from_toml(spend::PRICES_TOML).unwrap_or_default(),
            claude: BTreeMap::new(),
            codex: BTreeMap::new(),
            samples: Samples::default(),
            gaps,
            last_on: None,
        }
    }

    /// Note whether the journal is on at `now`: turning off opens a span from the last pass that
    /// saw it on (what was served since was served while it may have been off), or from the
    /// horizon's start when this process saw it on at no pass (it may have been off all along);
    /// turning on closes it at `now`. Answers whether the spans changed.
    fn note_publishing(&mut self, publishing: bool, now: i64) -> bool {
        let open = self
            .gaps
            .off
            .last()
            .is_some_and(|(_, until)| until.is_none());
        let changed = match (publishing, open) {
            (true, true) => {
                if let Some(last) = self.gaps.off.last_mut() {
                    last.1 = Some(now);
                }
                true
            }
            (false, false) => {
                let since = self.last_on.unwrap_or(now.saturating_sub(HORIZON_S));
                self.gaps.off.push((since, None));
                if self.gaps.off.len() > OFF_SPANS_MAX {
                    // The first two as one span over both, whatever their order.
                    let (since, until) = self.gaps.off.remove(0);
                    let next = &mut self.gaps.off[0];
                    next.0 = next.0.min(since);
                    next.1 = next.1.zip(until).map(|(a, b)| a.max(b));
                }
                true
            }
            _ => false,
        };
        if publishing {
            self.last_on = Some(now);
        }
        changed
    }

    /// Fold what every transcript and rollout appended since the last pass, and answer the CLIs
    /// whose homes were all listed and folded and whose folds read to their end: only their
    /// windows are whole.
    fn fold(&mut self, places: &Places, now: i64) -> BTreeSet<Cli> {
        let Self {
            prices,
            claude,
            codex,
            samples,
            gaps,
            ..
        } = self;
        let limits = Limits {
            bytes: FOLD_BYTES,
            until: Some(Instant::now() + FOLD_FOR),
        };
        let mut whole = BTreeSet::from([Cli::Claude, Cli::Codex]);
        let since = now.saturating_sub(HORIZON_S);
        // A row a home wrote after the pass began is its own minute's, not clamped into the one
        // before; a window still closes by the pass's own `now`.
        let ahead = now.saturating_add(FOLD_AHEAD_S);
        let listed = (places.claude_homes)(now);
        if !listed.complete {
            whole.remove(&Cli::Claude);
        }
        for home in &listed.dirs {
            if let Some(held) = claude.get_mut(home) {
                held.last = now;
            }
        }
        claude.retain(|home, held| home.is_dir() && held.last >= since);
        for home in &listed.dirs {
            if claude.contains_key(home) {
                continue;
            }
            if claude.len() >= HOMES_MAX {
                // Room from the home not listed now that served longest ago: what it served is
                // held as samples, and a home that serves again is listed again.
                let idle = claude
                    .iter()
                    .filter(|(held, _)| !listed.dirs.contains(held))
                    .min_by_key(|(_, held)| held.last)
                    .map(|(held, _)| held.clone());
                let Some(idle) = idle else {
                    gaps.lose(Cli::Claude, since);
                    gaps.lose(Cli::Claude, now);
                    continue;
                };
                claude.remove(&idle);
            }
            let spend = Spend::new(home, prices);
            claude.insert(home.clone(), Home { spend, last: now });
        }
        for held in claude.values_mut() {
            let mut last = held.last;
            let report = held
                .spend
                .fold_claude_home_each(prices, ahead, limits, &mut |served| {
                    last = last.max(served.at);
                    samples.take(gaps, Cli::Claude, served.key, claude_sample(&served));
                });
            held.last = last;
            if !report.caught_up || report.walk_cut {
                whole.remove(&Cli::Claude);
            }
        }
        let listed = (places.codex_homes)(now);
        if !listed.complete {
            whole.remove(&Cli::Codex);
        }
        if listed.dirs.len() > HOMES_MAX {
            gaps.lose(Cli::Codex, since);
            gaps.lose(Cli::Codex, now);
        }
        let folded: Vec<&PathBuf> = listed.dirs.iter().take(HOMES_MAX).collect();
        codex.retain(|home, _| folded.contains(&home));
        for home in folded {
            if !codex.contains_key(home) {
                codex.insert(home.clone(), Rollouts::default());
            }
            let Some(rollouts) = codex.get_mut(home) else {
                continue;
            };
            let caught_up = rollouts.fold(home, since, FOLD_BYTES, &mut |path, turn| {
                samples.take(
                    gaps,
                    Cli::Codex,
                    Some(codex_key(path, turn)),
                    codex_sample(turn),
                );
            });
            if !caught_up {
                whole.remove(&Cli::Codex);
            }
        }
        samples.expire(since);
        whole
    }
}

fn claude_sample(served: &Served<'_>) -> Sample {
    let family = family_version(served.model).map(|(family, _)| family);
    let t = served.tokens;
    Sample {
        at: served.at,
        family: Family::of_claude(family.as_deref()),
        tokens: [t.input, t.output, t.cache_read, t.cache_write],
    }
}

fn codex_sample(turn: &Turn) -> Sample {
    let t = spend::codex_message(turn).tokens;
    Sample {
        at: turn.at,
        family: Family::Gpt,
        tokens: [t.input, t.output, t.cache_read, t.cache_write],
    }
}

/// A turn's key in its home: FNV-1a/64 of its rollout's path and its key there, which is unique
/// within one rollout only.
fn codex_key(path: &Path, turn: &Turn) -> u64 {
    let h = astream_wire::hash::fnv1a_64_update(
        astream_wire::hash::FNV1A_64_INIT,
        path.as_os_str().as_encoded_bytes(),
    );
    astream_wire::hash::fnv1a_64_update(h, &turn.key.to_be_bytes())
}

/// What this Mac's log holds of its own rows, as the publisher last saw or wrote them.
struct Shown {
    node: Node,
    /// Each snapshot subject's row, encoded with its `at` cleared.
    rows: BTreeMap<Subject, Vec<u8>>,
    /// Each window's highest revision and its summary.
    windows: BTreeMap<(Metric, u64), (u64, Summary)>,
}

impl Shown {
    /// What `log` holds of `node`'s rows.
    fn read(log: &dyn Log, node: Node) -> io::Result<Self> {
        let mut shown = Self {
            node,
            rows: BTreeMap::new(),
            windows: BTreeMap::new(),
        };
        for (subject, body) in log.published()? {
            let Ok(parsed) = Subject::parse(&subject) else {
                continue;
            };
            if parsed.node() != node {
                continue;
            }
            match Row::decode_at(&parsed, &body) {
                Ok(Row::Window(record)) => {
                    let key = (record.metric(), record.window().start_ms());
                    let newer = shown
                        .windows
                        .get(&key)
                        .is_none_or(|(revision, _)| record.revision() >= *revision);
                    if newer {
                        shown
                            .windows
                            .insert(key, (record.revision(), record.summary().clone()));
                    }
                }
                Ok(mut row) => {
                    if let Some(timeless) = timeless(&mut row) {
                        shown.rows.insert(parsed, timeless);
                    }
                }
                Err(_) => {}
            }
        }
        Ok(shown)
    }

    /// An ended row for each place the log holds live that `projected` no longer names: the book
    /// forgets a place once its end is written to the journal, so the end is published here.
    fn ended_places(&self, projected: &[(Subject, Row)], now: u64) -> Vec<(Subject, Row)> {
        let named: BTreeSet<Subject> = projected
            .iter()
            .filter_map(|(s, _)| {
                matches!(
                    s,
                    Subject::Now {
                        now: Now::Place(_),
                        ..
                    }
                )
                .then_some(*s)
            })
            .collect();
        self.rows
            .iter()
            .filter_map(|(subject, body)| {
                let Subject::Now {
                    now: Now::Place(_), ..
                } = subject
                else {
                    return None;
                };
                if named.contains(subject) {
                    return None;
                }
                let Ok(Row::Place(mut place)) = Row::decode_at(subject, body) else {
                    return None;
                };
                if place.until <= now {
                    return None;
                }
                place.at = now;
                place.until = now.max(place.since);
                Some((*subject, Row::Place(place)))
            })
            .collect()
    }

    /// A removed row ([`QuotaRow::ended`]) for each account the log holds a row of, under a CLI
    /// whose accounts `accounts` read, that none of the account rows read names: the roster no
    /// longer lists it, and its last row would otherwise show it on every Mac at its age. A CLI
    /// whose read failed ends nothing, nor does a row whose label a row read with no key holds,
    /// and a row already removed is left as it is.
    fn ended_accounts(&self, accounts: &Accounts, now: u64) -> Vec<(Subject, Row)> {
        self.rows
            .iter()
            .filter_map(|(subject, body)| {
                let Subject::Quota { cli, .. } = subject else {
                    return None;
                };
                if !accounts.read.contains(cli) || accounts.named.contains(subject) {
                    return None;
                }
                let Ok(Row::Quota(row)) = Row::decode_at(subject, body) else {
                    return None;
                };
                let listed = accounts
                    .unkeyed
                    .get(cli)
                    .is_some_and(|labels| labels.contains(row.label.as_str()));
                (!row.removed() && !listed).then(|| (*subject, Row::Quota(row.ended(now))))
            })
            .collect()
    }
}

/// `row`'s encoding with its `at` cleared: what change detection compares.
fn timeless(row: &mut Row) -> Option<Vec<u8>> {
    let at = match row {
        Row::Quota(r) => std::mem::replace(&mut r.at, 0),
        Row::Agy(r) => std::mem::replace(&mut r.at, 0),
        Row::Place(r) => std::mem::replace(&mut r.at, 0),
        Row::Needs(r) => std::mem::replace(&mut r.at, 0),
        Row::Unread(r) => std::mem::replace(&mut r.at, 0),
        Row::Window(_) => return None,
    };
    let encoded = row.encode();
    match row {
        Row::Quota(r) => r.at = at,
        Row::Agy(r) => r.at = at,
        Row::Place(r) => r.at = at,
        Row::Needs(r) => r.at = at,
        Row::Unread(r) => r.at = at,
        Row::Window(_) => {}
    }
    Some(encoded)
}

/// Whether a row that is not on the log reads as `row`: an empty needs or unread row says what
/// no row says.
fn absent_reads_as(row: &Row) -> bool {
    match row {
        Row::Needs(r) => r.needs.is_empty() && r.held.is_empty() && r.managers.is_empty(),
        Row::Unread(r) => r.pools.is_empty(),
        _ => false,
    }
}

/// What one pass or one needs change published.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    /// Snapshot rows published.
    pub(crate) rows: usize,
    /// Window records published.
    pub(crate) windows: usize,
    /// The log's own rows could not be read back, so nothing was published.
    pub(crate) unread: bool,
    /// A publish was refused, and the pass stopped there.
    pub(crate) refused: Option<io::ErrorKind>,
}

/// What a pass hands [`Publisher::publish`].
struct Projected {
    /// The snapshot rows, where the pass projected them: a place the log holds that they no
    /// longer name is published as ended, and an account the roster no longer lists as removed.
    rows: Option<Snapshot>,
    windows: BTreeMap<(Metric, u64), Summary>,
}

/// One pass's snapshot rows ([`Publisher::snapshot`]).
struct Snapshot {
    rows: Vec<(Subject, Row)>,
    accounts: Accounts,
}

/// Which accounts a pass read.
#[derive(Default)]
struct Accounts {
    /// The CLIs whose account rows were read.
    read: BTreeSet<Cli>,
    /// The quota subject of each account row read that has a key, whether or not its row
    /// projected: an account the roster still lists.
    named: BTreeSet<Subject>,
    /// The label of each account row read that has no key this pass: the roster still lists
    /// the account a row of that label names, whatever its key was.
    unkeyed: BTreeMap<Cli, BTreeSet<String>>,
}

/// The publisher: what the folds keep, and what the log holds.
pub(crate) struct Publisher {
    places: Places,
    folds: Mutex<Folds>,
    shown: Mutex<Option<Shown>>,
    /// The needs row the windows' latest pushes make, once the node service has read them.
    needs: Mutex<Option<NeedsRow>>,
}

impl Publisher {
    pub(crate) fn new(places: Places) -> Self {
        let off = places
            .spool
            .as_ref()
            .and_then(|spool| std::fs::read_to_string(spool.dir().join(OFF_FILE)).ok())
            .map(|text| Gaps::off_of(&text))
            .unwrap_or_default();
        Self {
            places,
            folds: Mutex::new(Folds::new(Gaps {
                off,
                ..Gaps::default()
            })),
            shown: Mutex::new(None),
            needs: Mutex::new(None),
        }
    }

    /// One pass at `now` (Unix seconds): note whether the journal is on, then, where there is a
    /// log, fold, project and publish what changed on it. With `publishing` off the folds still
    /// read (so what was served while it was off is dropped, never published later), and only
    /// the needs row is published.
    pub(crate) fn pass(&self, log: Option<&dyn Log>, now: u64, publishing: bool) -> Report {
        let now_s = i64::try_from(now).unwrap_or(i64::MAX);
        let mut folds = self.folds.lock_or_recover();
        if folds.note_publishing(publishing, now_s) {
            self.keep_off(&folds.gaps);
        }
        let Some(log) = log else {
            return Report::default();
        };
        let whole = folds.fold(&self.places, now_s);
        folds.gaps.expire(now_s.saturating_sub(HORIZON_S));
        let windows = if publishing {
            folds.samples.windows(&whole, &folds.gaps)
        } else {
            BTreeMap::new()
        };
        drop(folds);
        let rows = publishing.then(|| self.snapshot(log.node(), now));
        self.publish(log, now, Projected { rows, windows })
    }

    /// Keep the off spans beside the spool, so a restart drops what they span too.
    fn keep_off(&self, gaps: &Gaps) {
        let Some(spool) = &self.places.spool else {
            return;
        };
        let dir = spool.dir();
        if aterm_uds::private_file::ensure_private_dir(dir).is_ok() {
            let _ =
                aterm_uds::private_file::write_private(dir, OFF_FILE, gaps.off_text().as_bytes());
        }
    }

    /// The windows pushed `windows` (each one's needs row, by its pid): publish the Mac's row on
    /// `log` when it changed. With no log the row waits for the next pass that has one.
    pub(crate) fn needs(
        &self,
        log: Option<&dyn Log>,
        windows: &BTreeMap<u32, NeedsRow>,
        now: u64,
    ) -> Report {
        *self.needs.lock_or_recover() = Some(merged_needs(windows, now));
        let Some(log) = log else {
            return Report::default();
        };
        self.publish(
            log,
            now,
            Projected {
                rows: None,
                windows: BTreeMap::new(),
            },
        )
    }

    fn publish(&self, log: &dyn Log, now: u64, projected: Projected) -> Report {
        let mut report = Report::default();
        let node = log.node();
        let mut held = self.shown.lock_or_recover();
        if held.as_ref().is_none_or(|shown| shown.node != node) {
            match Shown::read(log, node) {
                Ok(shown) => *held = Some(shown),
                Err(_) => {
                    *held = None;
                    report.unread = true;
                    return report;
                }
            }
        }
        let Some(shown) = held.as_mut() else {
            return report;
        };
        let mut rows = match projected.rows {
            Some(Snapshot { mut rows, accounts }) => {
                let ended = shown.ended_places(&rows, now);
                let removed = shown.ended_accounts(&accounts, now);
                rows.extend(ended);
                rows.extend(removed);
                rows
            }
            None => Vec::new(),
        };
        // Read with what the log holds locked, so a newer push published meanwhile is never
        // published over by an older copy.
        if let Some(needs) = self.needs.lock_or_recover().clone() {
            rows.push((NeedsRow::subject(node), Row::Needs(needs)));
        }
        #[cfg(any(test, debug_assertions))]
        let every = every_row();
        #[cfg(not(any(test, debug_assertions)))]
        let every = false;
        let mut named = BTreeSet::new();
        for (subject, row) in &mut rows {
            if matches!(row, Row::Window(_)) {
                continue;
            }
            // One row a subject, the first: a second (two account rows on one key) would publish
            // over it on every pass.
            if !named.insert(*subject) {
                continue;
            }
            let Some(timeless) = timeless(row) else {
                continue;
            };
            let same = match shown.rows.get(subject) {
                Some(was) => *was == timeless,
                None => absent_reads_as(row),
            };
            if same && !every {
                continue;
            }
            if let Err(e) = log.publish(&subject.to_string(), &row.encode()) {
                report.refused = Some(e.kind());
                return report;
            }
            shown.rows.insert(*subject, timeless);
            report.rows += 1;
        }
        let now_ms = now.saturating_mul(1000);
        // The oldest a window may start and hold every sample served in it: a Claude home's fold
        // reads back to its spend's horizon.
        let folded_ms = u64::try_from(
            i64::try_from(now)
                .unwrap_or(i64::MAX)
                .saturating_add(FOLD_AHEAD_S)
                .saturating_sub(spend::HORIZON_S),
        )
        .unwrap_or(0)
        .saturating_mul(1000);
        for ((metric, start), summary) in &projected.windows {
            let Some(window) = Window::starting(*start, metric.width().window_len()) else {
                continue;
            };
            if window.end_ms() > now_ms || *start < folded_ms || !kept(*metric, window, now_ms) {
                continue;
            }
            let was = shown.windows.get(&(*metric, *start));
            let revision = match was {
                None => 0,
                Some((revision, held)) => {
                    let grew = summary != held
                        && summary.count() >= held.count()
                        && summary.sum() >= held.sum();
                    if !grew && !every {
                        continue;
                    }
                    revision.saturating_add(1)
                }
            };
            let Some(record) = WindowRecord::new(node, *metric, *start, revision, summary.clone())
            else {
                continue;
            };
            if let Err(e) = log.publish(&record.subject().to_string(), &record.encode()) {
                report.refused = Some(e.kind());
                return report;
            }
            shown
                .windows
                .insert((*metric, *start), (revision, summary.clone()));
            report.windows += 1;
        }
        shown.windows.retain(|(metric, start), _| {
            Window::starting(*start, metric.width().window_len())
                .is_some_and(|window| kept(*metric, window, now_ms))
        });
        report
    }

    /// This Mac's quota, agy, place and unread rows at `now`, and which accounts it read.
    fn snapshot(&self, node: Node, now: u64) -> Snapshot {
        let places = &self.places;
        let mut rows = Vec::new();
        let mut listed = Accounts::default();
        let book = places
            .spool
            .as_ref()
            .map(|spool| journal::book_facts(spool, now))
            .unwrap_or_default();
        let mut unread = Vec::new();
        if let (Some(config), Some(harness)) = (&places.config, &places.harness) {
            let now_s = i64::try_from(now).unwrap_or(i64::MAX);
            for pool in accounts::Cli::ALL {
                let read = survey_accounts(pool, (config, harness, places.home.as_deref()), now_s);
                let Ok(read) = read else {
                    unread.extend(Word::new(pool.id()));
                    continue;
                };
                let Some(cli) = metrics_cli(pool) else {
                    continue;
                };
                listed.read.insert(cli);
                for account in &read.rows {
                    match account.pk.as_deref().and_then(AccountKey::parse) {
                        Some(key) => listed.named.extend(Subject::quota(node, cli, key)),
                        None => {
                            listed
                                .unkeyed
                                .entry(cli)
                                .or_default()
                                .insert(account.label.clone());
                        }
                    }
                    if let Some(row) = quota_row(cli, account, &book, now)
                        && let Some(subject) = row.subject(node)
                    {
                        rows.push((subject, Row::Quota(row)));
                    }
                }
            }
        } else {
            // With no roster or store to read, no pool was read.
            unread.extend(
                accounts::Cli::ALL
                    .iter()
                    .filter_map(|pool| Word::new(pool.id())),
            );
        }
        rows.push((
            UnreadRow::subject(node),
            Row::Unread(UnreadRow {
                at: now,
                pools: unread,
            }),
        ));
        if let Some(harness) = &places.harness {
            for row in agy_rows(harness) {
                rows.push((row.subject(node), Row::Agy(row)));
            }
        }
        for (pk, place) in &book.places {
            let (Some(account), Some(word)) =
                (AccountKey::parse(pk.as_str()), Word::new(place.why.word()))
            else {
                continue;
            };
            let row = PlaceRow {
                at: now,
                account,
                place: word,
                since: place.since,
                until: place.until,
            };
            rows.push((row.subject(node), Row::Place(row)));
        }
        Snapshot {
            rows,
            accounts: listed,
        }
    }
}

/// Whether retention still keeps `metric`'s `window` at `now_ms`: a window past its class's age
/// is neither published nor remembered.
fn kept(metric: Metric, window: Window, now_ms: u64) -> bool {
    let age = match metric.width() {
        Width::Minute => MINUTE_AGE,
        Width::Hour => HOUR_AGE,
    };
    let age_ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
    window.end_ms().saturating_add(age_ms) > now_ms
}

/// The metrics CLI of a pool whose rows are per account; agy's quota is per home.
const fn metrics_cli(pool: accounts::Cli) -> Option<Cli> {
    match pool {
        accounts::Cli::Claude => Some(Cli::Claude),
        accounts::Cli::Codex => Some(Cli::Codex),
        accounts::Cli::Grok => Some(Cli::Grok),
        accounts::Cli::Kimi => Some(Cli::Kimi),
        accounts::Cli::Agy => None,
    }
}

/// An account row as its quota row: `None` for a row the registry has no key for, or whose label
/// the row grammar refuses.
fn quota_row(cli: Cli, account: &SurveyAccount, book: &BookFacts, now: u64) -> Option<QuotaRow> {
    let pk = account.pk.as_deref()?;
    let key = AccountKey::parse(pk)?;
    let facts = Pk::new(pk).and_then(|pk| book.accounts.get(&pk));
    let secs = |t: Option<i64>| t.and_then(|t| u64::try_from(t).ok());
    let pct = |p: Option<u32>| p.map(|p| u16::try_from(p).unwrap_or(PCT_MAX).min(PCT_MAX));
    Some(QuotaRow {
        at: secs(account.read_at).unwrap_or(now),
        cli,
        account: key,
        label: Label::new(&account.label)?,
        condition: Word::new(account.condition.as_str())?,
        enrolled: account.enrolled,
        pct5: pct(account.five_hour_pct),
        pct7: pct(account.seven_day_pct),
        // A bound no read named is no reset: `merge::usage` takes a window's identity from this
        // field, and holds a figure with none to its one-window rule.
        five_hour_reset: secs(
            account
                .five_hour_reset
                .filter(|_| account.five_hour_reset_named),
        ),
        week_reset: secs(account.week_reset.filter(|_| account.week_reset_named)),
        walled_until: match account.condition {
            Condition::Limited { until } => secs(Some(until)),
            _ => None,
        },
        last_used: secs(account.used_at),
        signin: facts.and_then(|f| f.signin).and_then(|(state, t)| {
            Some(SignIn {
                state: Word::new(state)?,
                t,
            })
        }),
        last_start: facts.and_then(|f| f.last_start),
        last_end: facts.and_then(|f| f.last_end),
        sessions_7d: facts.map_or(0, |f| f.sessions_7d),
        plan: plan_word(account),
    })
}

/// `account`'s plan as one word: the word the accounts view names, then `-` and its tier where
/// one was seen (`max-20x`); the word alone where the two do not make one.
fn plan_word(account: &SurveyAccount) -> Option<Word> {
    let word = account.plan.as_deref()?;
    account
        .plan_tier
        .as_deref()
        .and_then(|tier| {
            let mut joined = [0u8; 32];
            let need = word.len().checked_add(1)?.checked_add(tier.len())?;
            let slice = joined.get_mut(..need)?;
            slice[..word.len()].copy_from_slice(word.as_bytes());
            slice[word.len()] = b'-';
            slice[word.len() + 1..].copy_from_slice(tier.as_bytes());
            Word::new(std::str::from_utf8(slice).ok()?)
        })
        .or_else(|| Word::new(word))
}

/// The last reading of each agy home the store keeps under `harness`.
fn agy_rows(harness: &Path) -> Vec<AgyRow> {
    let Ok(dir) = std::fs::read_dir(harness.join(agy_quota_store::DIR)) else {
        return Vec::new();
    };
    let mut rows: Vec<AgyRow> = dir
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let key = name.to_str()?.strip_suffix(".json")?;
            let home = HomeKey::parse(key)?;
            let reading = agy_quota_store::Store::new(harness, key).last()?;
            Some(agy_row(home, &reading))
        })
        .collect();
    rows.sort_unstable_by_key(|row| row.home);
    rows
}

fn agy_row(home: HomeKey, reading: &agy_quota_store::Reading) -> AgyRow {
    let mut seen = BTreeSet::new();
    let groups = reading
        .quota
        .groups
        .iter()
        .flat_map(|group| &group.buckets)
        .filter_map(|bucket| {
            if !seen.insert(bucket.id.as_str()) {
                return None;
            }
            let id = Word::new(&bucket.id)?;
            Some(AgyGroup {
                id,
                remaining: bucket.remaining.map(basis_points),
                reset_at: bucket.reset_at().and_then(|t| u64::try_from(t).ok()),
                off: bucket.disabled,
            })
        })
        .collect();
    AgyRow {
        at: u64::try_from(reading.as_of).unwrap_or(0),
        home,
        groups,
    }
}

/// A share in `0..=1` as basis points.
fn basis_points(share: f64) -> u16 {
    let points = (share.clamp(0.0, 1.0) * f64::from(REMAINING_MAX)).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=REMAINING_MAX first"
    )]
    let points = points as u16;
    points.min(REMAINING_MAX)
}

/// One row of every window's needs: each list joined, a session listed once (its first window's
/// entry), the managers' workers summed, in session order, at most [`NEEDS_MAX`] of each.
fn merged_needs(windows: &BTreeMap<u32, NeedsRow>, now: u64) -> NeedsRow {
    let mut needs = BTreeMap::new();
    let mut held = BTreeMap::new();
    let mut managers: BTreeMap<_, u32> = BTreeMap::new();
    for row in windows.values() {
        for entry in &row.needs {
            needs.entry(entry.sid).or_insert_with(|| entry.clone());
        }
        for entry in &row.held {
            held.entry(entry.sid).or_insert_with(|| entry.clone());
        }
        for entry in &row.managers {
            let workers = managers.entry(entry.sid).or_default();
            *workers = workers.saturating_add(entry.workers);
        }
    }
    NeedsRow {
        at: now,
        needs: needs.into_values().take(NEEDS_MAX).collect(),
        held: held.into_values().take(NEEDS_MAX).collect(),
        managers: managers
            .into_iter()
            .take(NEEDS_MAX)
            .map(|(sid, workers)| ManagerEntry { sid, workers })
            .collect(),
    }
}

/// The node service's publisher.
static PUBLISHER: OnceLock<Publisher> = OnceLock::new();

/// Start publishing from `aterm node serve`: a pass every [`PASS_EVERY`] on a thread of its own,
/// and the Mac's needs row on each change of the windows' needs.
pub(crate) fn start() {
    let publisher = PUBLISHER.get_or_init(|| Publisher::new(Places::of_process()));
    let _ = aterm_gui::install_window_needs(Arc::new(move |windows| {
        let log = NodeLog::current();
        let now = aterm_time::unix_secs().unwrap_or(0);
        publisher.needs(log.as_ref().map(|l| l as &dyn Log), windows, now);
    }));
    let publishing = journal::Publishing::new(aterm_types::dirs::aterm_config_path());
    let spawned = std::thread::Builder::new()
        .name("aterm-metrics".to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(PASS_EVERY);
                let log = NodeLog::current();
                let now = aterm_time::unix_secs().unwrap_or(0);
                publisher.pass(log.as_ref().map(|l| l as &dyn Log), now, publishing.now());
            }
        });
    if let Err(error) = spawned {
        aterm_log::warn!("node: no metrics publisher: {error}");
    }
}

#[cfg(test)]
#[path = "metrics_publish_tests.rs"]
mod tests;
