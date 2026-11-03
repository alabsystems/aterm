// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! agy's quota over time: the last reading per agy home, kept under the harness state, the
//! history of readings it was made from, and the burn rate that history gives each limit.

use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use super::agy_quota::{self, Quota, Standing};
use super::provider_read;

/// How long a reading stays current while every pool is open.
pub const FRESH_OPEN_S: i64 = 15 * 60;

/// How long a reading stays current while a pool is low or spent.
pub const FRESH_TIGHT_S: i64 = 5 * 60;

/// A refill a reading names counts as passed this long after its time, so the next read sees it.
pub const RESET_SETTLE_S: i64 = 60;

/// A read lock older than this is a crashed reader's and is taken over.
pub const LOCK_STALE_S: u64 = 90;

/// After a failed read, how long the next one waits (unless forced): a home agy is not signed
/// in to answers only after the read's whole budget, so it is not asked at every idle point.
pub const FAIL_BACKOFF_S: i64 = 5 * 60;

/// History lines kept per home; past twice this, the oldest are dropped down to it.
pub const HISTORY_KEEP: usize = 2_000;

/// The directory under the harness state the readings live in.
pub const DIR: &str = "agy-quota";

/// One reading: what agy printed, and when.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// Unix seconds the read finished.
    pub as_of: i64,
    /// The parsed quota.
    pub quota: Quota,
    /// agy's document, kept to re-parse when the reader learns more.
    pub doc: String,
}

impl Reading {
    /// Is this reading still current at `now`?
    #[must_use]
    pub fn fresh(&self, now: i64, low: f64) -> bool {
        let tight = self
            .quota
            .groups
            .iter()
            .any(|g| g.standing(low) != Standing::Open);
        let ttl = if tight { FRESH_TIGHT_S } else { FRESH_OPEN_S };
        if now < self.as_of || now.saturating_sub(self.as_of) >= ttl {
            return false;
        }
        !self.refill_passed(now)
    }

    /// Whether an active non-full bucket's recorded refill has passed its settling interval.
    #[must_use]
    pub fn refill_passed(&self, now: i64) -> bool {
        self.quota
            .groups
            .iter()
            .flat_map(|g| g.buckets.iter())
            .filter(|b| !b.disabled && b.remaining.is_some_and(|f| f + f64::EPSILON < 1.0))
            .filter_map(|b| {
                let settled = b.reset_at()?.saturating_add(RESET_SETTLE_S);
                (b.spent() || self.as_of < settled).then_some(settled)
            })
            .any(|settled| now >= settled)
    }

    /// Whether an active non-full bucket's recorded refill timestamp arrived since `self.as_of`.
    fn refill_crossed(&self, now: i64) -> bool {
        self.quota
            .groups
            .iter()
            .flat_map(|g| g.buckets.iter())
            .filter(|b| !b.disabled && b.remaining.is_some_and(|f| f + f64::EPSILON < 1.0))
            .filter_map(agy_quota::Bucket::reset_at)
            .any(|at| self.as_of < at && now >= at)
    }

    /// Whether a refill this reading records has arrived, or finished settling, since `since`
    /// (`self.as_of` or a later wall-read attempt): true once per refill or settle point.
    fn refill_arrived_since(&self, since: i64, now: i64) -> bool {
        self.quota
            .groups
            .iter()
            .flat_map(|g| g.buckets.iter())
            .filter(|b| !b.disabled && b.remaining.is_some_and(|f| f + f64::EPSILON < 1.0))
            .filter_map(agy_quota::Bucket::reset_at)
            .any(|at| {
                let settled = at.saturating_add(RESET_SETTLE_S);
                (since < at && now >= at) || (since < settled && now >= settled)
            })
    }

    /// `self.quota` projected for a stale reading at `now`: each active non-full bucket whose
    /// recorded refill has arrived (`now >= reset_at` when read before `reset_at`, or past
    /// [`RESET_SETTLE_S`] when read at or after `reset_at`) refills to `1.0`, while buckets whose
    /// refill is still in the future keep their recorded standing.
    #[must_use]
    pub fn quota_at_stale(&self, now: i64) -> Quota {
        let mut quota = self.quota.clone();
        for group in &mut quota.groups {
            for b in &mut group.buckets {
                if !b.disabled
                    && b.remaining.is_some_and(|f| f + f64::EPSILON < 1.0)
                    && b.reset_at().is_some_and(|at| {
                        let settled = at.saturating_add(RESET_SETTLE_S);
                        if self.as_of < at {
                            now >= at
                        } else {
                            (b.spent() || self.as_of < settled) && now >= settled
                        }
                    })
                {
                    b.remaining = Some(1.0);
                    b.reset = None;
                }
            }
        }
        quota
    }
}

/// A stable key for the account an agy launch reads: `home` for the owner's own HOME and no
/// home-moving flags, else a hash of the HOME it runs under and those flags, so two homes never
/// share a reading.
#[must_use]
pub fn launch_key(home: Option<&str>, flags: &[String]) -> String {
    if home.is_none() && flags.is_empty() {
        return "home".to_owned();
    }
    let mut hash = aterm_hash::Fnv1a64::new();
    let words = home
        .iter()
        .map(|h| format!("HOME={h}"))
        .chain(flags.iter().cloned());
    for word in words {
        hash.write(word.as_bytes());
        hash.write_u8(0);
    }
    format!("h{:016x}", hash.finish())
}

/// The harness state directory every window and `aterm harness` on this machine shares.
#[must_use]
pub fn machine_state() -> Option<PathBuf> {
    aterm_types::dirs::state_dir().map(|s| s.join("harness").join(super::cli::HARNESS))
}

/// Where one home's readings live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    dir: PathBuf,
    key: String,
}

impl Store {
    /// The store for `key` under the harness state `state`.
    #[must_use]
    pub fn new(state: &Path, key: &str) -> Self {
        Self {
            dir: state.join(DIR),
            key: key.to_owned(),
        }
    }

    /// The last reading's file.
    #[must_use]
    pub fn last_path(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.key))
    }

    /// The history file, one line per reading.
    #[must_use]
    pub fn history_path(&self) -> PathBuf {
        self.dir.join(format!("{}.jsonl", self.key))
    }

    fn model_path(&self) -> PathBuf {
        self.dir.join(format!("{}.model", self.key))
    }

    /// agy's default model as last read, while under [`FRESH_OPEN_S`] old at `now`.
    #[must_use]
    pub fn kept_default(&self, now: i64) -> Option<String> {
        let text = super::accounts::read_bounded(&self.model_path(), 64 * 1024)
            .ok()
            .flatten()?;
        let (at, model) = text.trim().split_once(' ')?;
        let at: i64 = at.parse().ok()?;
        (now >= at && now.saturating_sub(at) < FRESH_OPEN_S && !model.is_empty())
            .then(|| model.to_owned())
    }

    /// agy's default model as last read, regardless of age.
    #[must_use]
    pub fn last_default(&self) -> Option<String> {
        let text = super::accounts::read_bounded(&self.model_path(), 64 * 1024)
            .ok()
            .flatten()?;
        let (_, model) = text.trim().split_once(' ')?;
        (!model.is_empty()).then(|| model.to_owned())
    }

    /// Keep agy's default model as read at `now`.
    pub fn keep_default(&self, model: &str, now: i64) {
        let clean = model.trim();
        if clean.is_empty() || clean.contains(['\r', '\n']) {
            return;
        }
        if std::fs::create_dir_all(&self.dir).is_ok() {
            let _ = super::upgrade_models::write_atomic(
                &self.model_path(),
                &format!("{now} {clean}\n"),
            );
        }
    }

    fn failed_path(&self) -> PathBuf {
        self.dir.join(format!("{}.failed", self.key))
    }

    /// The last failed read, `(unix seconds, why)`, while under [`FAIL_BACKOFF_S`] old at `now`.
    #[must_use]
    pub fn recent_failure(&self, now: i64) -> Option<(i64, String)> {
        let text = super::accounts::read_bounded(&self.failed_path(), 64 * 1024)
            .ok()
            .flatten()?;
        let (at, why) = text.trim().split_once(' ')?;
        let at: i64 = at.parse().ok()?;
        (now >= at && now.saturating_sub(at) < FAIL_BACKOFF_S).then(|| (at, why.to_owned()))
    }

    pub(super) fn note_failure(&self, why: &str, now: i64) {
        if std::fs::create_dir_all(&self.dir).is_ok() {
            let one_line = why.replace(['\r', '\n'], " ");
            let _ = super::upgrade_models::write_atomic(
                &self.failed_path(),
                &format!("{now} {one_line}\n"),
            );
        }
    }

    fn wall_read_path(&self) -> PathBuf {
        self.dir.join(format!("{}.wall", self.key))
    }

    fn note_wall_read(&self, now: i64) {
        if std::fs::create_dir_all(&self.dir).is_ok() {
            let _ =
                super::upgrade_models::write_atomic(&self.wall_read_path(), &format!("{now}\n"));
        }
    }

    /// Whether a wall already had its read: one was attempted under [`WALL_READ_SPENT_S`] ago,
    /// no newer reading superseded it, and `kept` names no refill that arrived since `at`. A
    /// wall's later looks then decide on what is kept instead of asking agy again.
    fn wall_read_spent(&self, kept: Option<&Reading>, now: i64) -> bool {
        let Some(at) = super::accounts::read_bounded(&self.wall_read_path(), 64)
            .ok()
            .flatten()
            .and_then(|text| text.trim().parse::<i64>().ok())
        else {
            return false;
        };
        now >= at
            && now.saturating_sub(at) < WALL_READ_SPENT_S
            && kept.is_none_or(|r| r.as_of <= at && !r.refill_arrived_since(at, now))
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join(format!("{}.lock", self.key))
    }

    /// The last reading, if one was kept and still parses.
    #[must_use]
    pub fn last(&self) -> Option<Reading> {
        let text = super::accounts::read_bounded(&self.last_path(), 256 * 1024)
            .ok()
            .flatten()?;
        let v: Value = aterm_json::from_str(&text).ok()?;
        let o = v.as_object()?;
        if o.get("schema").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        let as_of = o.get("as_of").and_then(Value::as_i64)?;
        let doc = o.get("doc").and_then(Value::as_str)?.to_owned();
        let quota = agy_quota::parse(&doc).ok()?;
        Some(Reading { as_of, quota, doc })
    }

    /// Keep `reading` as the last one and add it to the history.
    ///
    /// # Errors
    ///
    /// The state directory or a file in it cannot be written.
    pub fn keep(&self, reading: &Reading) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let _ = std::fs::remove_file(self.failed_path());
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("as_of".to_owned(), Value::from(reading.as_of));
        o.insert("doc".to_owned(), Value::from(reading.doc.clone()));
        super::upgrade_models::write_atomic(
            &self.last_path(),
            &aterm_json::to_string(&Value::Object(o)).unwrap_or_default(),
        )?;
        self.append_history(reading)
    }

    fn append_history(&self, reading: &Reading) -> std::io::Result<()> {
        let path = self.history_path();
        let line = history_line(reading);
        let prior = super::accounts::read_bounded(&path, 2 * 1024 * 1024)
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut lines: Vec<&str> = prior.lines().collect();
        lines.push(&line);
        if lines.len() > 2 * HISTORY_KEEP {
            lines.drain(..lines.len() - HISTORY_KEEP);
        }
        let mut kept = lines.join("\n");
        kept.push('\n');
        super::upgrade_models::write_atomic(&path, &kept)
    }

    /// Every sample of the history, oldest first.
    #[must_use]
    pub fn history(&self) -> Vec<Sample> {
        super::accounts::read_bounded(&self.history_path(), 2 * 1024 * 1024)
            .ok()
            .flatten()
            .map(|text| text.lines().filter_map(Sample::parse).collect())
            .unwrap_or_default()
    }

    /// Take the read lock: `None` while another reader holds a fresh one.
    #[must_use]
    pub fn try_lock(&self) -> Option<ReadLock> {
        std::fs::create_dir_all(&self.dir).ok()?;
        let path = self.lock_path();
        let take = || {
            let mut open = std::fs::OpenOptions::new();
            open.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                open.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            open.open(&path).ok()
        };
        if take().is_none() {
            let stale = match std::fs::symlink_metadata(&path) {
                Ok(m) if m.file_type().is_dir() => false,
                Ok(m) if m.file_type().is_file() => match m.modified() {
                    Ok(t) => t.elapsed().is_ok_and(|age| age.as_secs() >= LOCK_STALE_S),
                    Err(_) => true,
                },
                Ok(_) | Err(_) => true,
            };
            if !stale {
                return None;
            }
            let _ = std::fs::remove_file(&path);
            take()?;
        }
        Some(ReadLock { path })
    }
}

/// A held read lock; dropping it lets the next reader in.
#[derive(Debug)]
pub struct ReadLock {
    path: PathBuf,
}

impl Drop for ReadLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn history_line(reading: &Reading) -> String {
    let mut buckets = Map::new();
    for b in reading.quota.groups.iter().flat_map(|g| g.buckets.iter()) {
        let mut o = Map::new();
        o.insert("left".to_owned(), Value::from(b.remaining));
        o.insert("reset".to_owned(), Value::from(b.reset_at()));
        o.insert("off".to_owned(), Value::from(b.disabled));
        buckets.insert(b.id.clone(), Value::Object(o));
    }
    let mut o = Map::new();
    o.insert("at".to_owned(), Value::from(reading.as_of));
    o.insert("buckets".to_owned(), Value::Object(buckets));
    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
}

/// One limit in one history line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// The fraction left.
    pub left: Option<f64>,
    /// Its refill, Unix seconds: two points with different refills are in different windows.
    pub reset: Option<i64>,
    /// The limit did not apply.
    pub off: bool,
}

/// One history line.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// Unix seconds.
    pub at: i64,
    /// Each limit by agy's bucket id.
    pub buckets: Vec<(String, Point)>,
}

impl Sample {
    fn parse(line: &str) -> Option<Self> {
        let v: Value = aterm_json::from_str(line).ok()?;
        let o = v.as_object()?;
        let at = o.get("at").and_then(Value::as_i64)?;
        let buckets = o
            .get("buckets")
            .and_then(Value::as_object)?
            .iter()
            .filter_map(|(id, p)| {
                let p = p.as_object()?;
                Some((
                    id.clone(),
                    Point {
                        left: p
                            .get("left")
                            .and_then(Value::as_f64)
                            .filter(|f| f.is_finite()),
                        reset: p.get("reset").and_then(Value::as_i64),
                        off: p.get("off").and_then(Value::as_bool).unwrap_or(false),
                    },
                ))
            })
            .collect();
        Some(Self { at, buckets })
    }

    fn point(&self, id: &str) -> Option<&Point> {
        self.buckets.iter().find(|(b, _)| b == id).map(|(_, p)| p)
    }
}

/// The shortest span a burn rate is measured over.
pub const BURN_MIN_SPAN_S: i64 = 10 * 60;

/// The longest span looked back over for a burn rate.
pub const BURN_LOOKBACK_S: i64 = 6 * 3_600;

/// How fast one limit is being used, from the history.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Burn {
    /// Fraction used per hour.
    pub per_hour: f64,
    /// When it runs out at this rate, Unix seconds; `None` when it refills first or is not falling.
    pub empty_at: Option<i64>,
}

/// The burn rate of bucket `id` as of the latest sample: the fall in `left` across the samples
/// of the latest window (same refill) within [`BURN_LOOKBACK_S`], over at least
/// [`BURN_MIN_SPAN_S`].
#[must_use]
pub fn burn(history: &[Sample], id: &str) -> Option<Burn> {
    let (last, last_point) = history.iter().rev().find_map(|s| {
        s.point(id)
            .filter(|p| !p.off && p.left.is_some())
            .map(|p| (s, *p))
    })?;
    let first = history
        .iter()
        .filter(|s| s.at >= last.at.saturating_sub(BURN_LOOKBACK_S) && s.at < last.at)
        .find_map(|s| {
            s.point(id)
                .filter(|p| !p.off && p.reset == last_point.reset && p.left.is_some())
                .map(|p| (s.at, *p))
        })?;
    let span = last.at.checked_sub(first.0)?;
    if span < BURN_MIN_SPAN_S {
        return None;
    }
    let used = first.1.left? - last_point.left?;
    if !used.is_finite() {
        return None;
    }
    let per_hour = (used * 3_600.0 / span as f64).max(0.0);
    if !per_hour.is_finite() {
        return None;
    }
    let left = last_point.left?;
    let secs_left = left / per_hour * 3_600.0;
    let empty_at = (per_hour > 0.0
        && secs_left.is_finite()
        && secs_left >= 0.0
        && secs_left <= i64::MAX as f64)
        .then(|| last.at.checked_add(secs_left as i64))
        .flatten()
        .filter(|at| last_point.reset.is_none_or(|reset| *at < reset));
    Some(Burn { per_hour, empty_at })
}

/// Where a reading comes from: agy itself, or (in tests) a recorded document.
pub trait Source {
    /// One `agy -p /usage --output-format json`.
    ///
    /// # Errors
    ///
    /// The read failed; the text says why.
    fn read(&self) -> Result<String, String>;

    /// agy's default model id (`agy -p /model --output-format json`).
    ///
    /// # Errors
    ///
    /// The read failed, or this source cannot say.
    fn default_model(&self) -> Result<String, String> {
        Err("this source names no default model".to_owned())
    }
}

/// agy on PATH, or a named program, under extra environment pairs and leading flags (the home a
/// worker carries).
pub struct Agy {
    /// The program.
    pub program: std::ffi::OsString,
    /// Pairs set on the read (`HOME`).
    pub env: Vec<(String, String)>,
    /// Flags ahead of `-p` (a carried `--gemini_dir`).
    pub flags: Vec<String>,
    /// How long one read may take.
    pub budget: std::time::Duration,
}

impl Agy {
    /// The `agy` on PATH under this process's own HOME.
    #[must_use]
    pub fn on_path() -> Self {
        Self {
            program: "agy".into(),
            env: Vec::new(),
            flags: Vec::new(),
            budget: agy_quota::READ_BUDGET,
        }
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut cmd = aterm_uds::spawnfd::Command::new(&self.program);
        cmd.args(&self.flags).args(args);
        for (n, v) in &self.env {
            cmd.env(n, v);
        }
        let got = super::align::capture_bounded(cmd, self.budget, agy_quota::MAX_STDOUT)?;
        if !got.success() {
            return Err(format!(
                "`agy {}` exited {}",
                args.join(" "),
                got.code
                    .map_or_else(|| "on a signal".to_owned(), |c| c.to_string())
            ));
        }
        String::from_utf8(got.stdout).map_err(|_| "agy's output is not UTF-8".to_owned())
    }
}

impl Source for Agy {
    fn read(&self) -> Result<String, String> {
        self.run(&agy_quota::USAGE_ARGS)
    }

    fn default_model(&self) -> Result<String, String> {
        self.run(&agy_quota::MODEL_ARGS)
            .and_then(|doc| agy_quota::parse_model(&doc))
    }
}

/// How long a worker's launch waits on one agy read before it launches as asked.
pub const LAUNCH_READ_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Where a [`current`] reading came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Got {
    /// The kept reading was still fresh.
    Kept,
    /// agy was asked now.
    Read,
    /// Another reader held the lock, or agy failed: the last kept reading, stale.
    Stale,
}

impl Got {
    /// The word `--json` and the HUD carry.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::Read => "read",
            Self::Stale => "stale",
        }
    }
}

/// The current reading for `store`: the kept one while fresh (unless `force`), else one read
/// from `source` under the read lock and kept, reported as `why` ([`provider_read::note`]); a
/// failed read falls back to the stale reading.
///
/// # Errors
///
/// No reading was kept and none could be made; the text says why.
pub fn current(
    store: &Store,
    source: &dyn Source,
    now: i64,
    low: f64,
    force: bool,
    why: provider_read::InUse,
) -> Result<(Reading, Got), String> {
    let kept = store.last();
    if let Some(r) = kept.as_ref().filter(|r| !force && r.fresh(now, low)) {
        return Ok((r.clone(), Got::Kept));
    }
    if !force && let Some((_, why)) = store.recent_failure(now) {
        return kept
            .map(|r| (r, Got::Stale))
            .ok_or_else(|| format!("the last read failed ({why}); the next one waits"));
    }
    let Some(_lock) = store.try_lock() else {
        return kept.map(|r| (r, Got::Stale)).ok_or_else(|| {
            "another reader is asking agy now, and nothing was kept yet".to_owned()
        });
    };
    provider_read::note(&provider_read::ProbeEvent {
        at: now,
        read: provider_read::Read::AgyUsage,
        account: None,
        login: None,
        why,
    });
    let read = source.read().and_then(|doc| {
        agy_quota::parse(&doc).map(|quota| Reading {
            as_of: now,
            quota,
            doc,
        })
    });
    match read {
        Ok(reading) => {
            let _ = std::fs::remove_file(store.failed_path());
            // An unwritable state directory costs only the cache: the reading is still current.
            let _ = store.keep(&reading);
            Ok((reading, Got::Read))
        }
        Err(why) => {
            store.note_failure(&why, now);
            kept.map(|r| (r, Got::Stale)).ok_or(why)
        }
    }
}

/// Minimum age in seconds of a kept reading before a quota wall on screen forces a fresh
/// `agy -p /usage` read ([`rotate_launch_at`]), bounding wall-triggered reads to at most one per
/// 30 seconds.
pub const WALL_FORCE_FLOOR_S: i64 = 30;

/// How long a wall's read serves every later look at a wall on the same home, unless a refill
/// the reading records arrives first ([`Store::wall_read_spent`]). The store cannot see a wall
/// end, so this is what lets a later wall read again; a wall that stands past it reads once more.
pub const WALL_READ_SPENT_S: i64 = 60 * 60;

/// What [`rotate_launch`] decided, with the reading it decided on.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchPick {
    /// The decision.
    pub pick: agy_quota::Pick,
    /// The parsed quota snapshot `pick` was decided from.
    pub quota: agy_quota::Quota,
    /// The model was named by the caller (`model=`), not agy's default.
    pub named: bool,
    /// Where the reading came from.
    pub got: Got,
}

/// Chooses the model an agy launch runs ([`agy_quota::pick`]) for `asked` (a `model=`), or agy's
/// default when `None`, from `store`'s current reading. `None` when rotation is off, no reading
/// or default model could be had, or the only reading is stale past a refill: the launch then
/// runs as asked.
#[must_use]
pub fn rotate_launch(
    asked: Option<&str>,
    store: &Store,
    source: &dyn Source,
    rotation: &agy_quota::Rotation,
    now: i64,
) -> Option<LaunchPick> {
    rotate_launch_at(asked, store, source, rotation, now, false)
}

/// [`rotate_launch`] with `at_wall`: when `true` (a running `agy` session is stopped at a quota
/// wall on screen), a kept reading at least [`WALL_FORCE_FLOOR_S`] old is refreshed and
/// [`agy_quota::pick_at_wall`] decides the model. One wall makes one read
/// ([`WALL_READ_SPENT_S`]): the loop's later looks at it decide on that reading.
#[must_use]
pub fn rotate_launch_at(
    asked: Option<&str>,
    store: &Store,
    source: &dyn Source,
    rotation: &agy_quota::Rotation,
    now: i64,
    at_wall: bool,
) -> Option<LaunchPick> {
    if !rotation.enabled {
        return None;
    }
    let kept = store.last();
    let (reading, got) = if at_wall && store.wall_read_spent(kept.as_ref(), now) {
        let reading = kept?;
        let got = on_record(&reading, now, rotation.low);
        (reading, got)
    } else {
        let force = store.recent_failure(now).is_none()
            && kept.as_ref().map_or(at_wall, |r| {
                r.refill_crossed(now)
                    || (at_wall && now.saturating_sub(r.as_of) >= WALL_FORCE_FLOOR_S)
            });
        let why = if at_wall {
            provider_read::InUse::AtWall
        } else {
            provider_read::InUse::Launching
        };
        let current_res = current(store, source, now, rotation.low, force, why);
        // A read that failed spent this wall's read too (even with nothing kept yet): it asked agy.
        let asked = matches!(current_res, Ok((_, Got::Read)))
            || store.recent_failure(now).is_some_and(|(at, _)| at == now);
        if at_wall && asked {
            store.note_wall_read(now);
        }
        current_res.ok()?
    };
    // At a live quota wall, a stale reading past any recorded refill cannot justify restarting the
    // running session.
    if got == Got::Stale && at_wall && (reading.refill_passed(now) || reading.refill_crossed(now)) {
        return None;
    }
    let (model, named) = match asked {
        Some(m) => (m.to_owned(), true),
        None => {
            let kept = store
                .kept_default(now)
                .filter(|_| !(at_wall && got == Got::Read));
            let model = match kept {
                Some(m) => m,
                None => match source.default_model() {
                    Ok(m) => {
                        store.keep_default(&m, now);
                        m
                    }
                    Err(_) => store.kept_default(now)?,
                },
            };
            (model, false)
        }
    };
    decide(reading, got, model, named, rotation, now, at_wall)
}

/// How a reading on record stands at `now` when nothing is asked of agy.
fn on_record(reading: &Reading, now: i64, low: f64) -> Got {
    if reading.fresh(now, low) && !reading.refill_crossed(now) {
        Got::Kept
    } else {
        Got::Stale
    }
}

/// The pick `reading` gives `model`.
fn decide(
    reading: Reading,
    got: Got,
    model: String,
    named: bool,
    rotation: &agy_quota::Rotation,
    now: i64,
    at_wall: bool,
) -> Option<LaunchPick> {
    let effective_quota;
    let quota_for_pick = if got == Got::Stale && !at_wall {
        effective_quota = reading.quota_at_stale(now);
        &effective_quota
    } else {
        &reading.quota
    };
    let pick = if at_wall {
        agy_quota::pick_at_wall(quota_for_pick, &model, rotation)
    } else {
        agy_quota::pick(quota_for_pick, &model, named, rotation)
    };
    // A stale reading past its refill cannot hold a launch in Wait or assert Keep; it may still
    // rotate off a pool whose own blocker has not yet refilled.
    if got == Got::Stale
        && ((reading.refill_passed(now) && !matches!(pick, agy_quota::Pick::Rotate { .. }))
            || matches!(
                pick,
                agy_quota::Pick::Wait {
                    until: Some(until),
                    ..
                } if until <= now
            ))
    {
        return None;
    }
    Some(LaunchPick {
        pick,
        quota: reading.quota,
        named,
        got,
    })
}

/// What the reading on record says of a running session's model (`asked`, or the default last
/// read), asking agy nothing: a session between turns is no launch and no wall, so it may rotate
/// on what launches and walls last read, never read for itself. `None` when rotation is off or
/// nothing is on record.
#[must_use]
pub fn rotate_on_record(
    asked: Option<&str>,
    store: &Store,
    rotation: &agy_quota::Rotation,
    now: i64,
) -> Option<LaunchPick> {
    if !rotation.enabled {
        return None;
    }
    let reading = store.last()?;
    let got = on_record(&reading, now, rotation.low);
    let (model, named) = match asked {
        Some(model) => (model.to_owned(), true),
        None => (store.last_default()?, false),
    };
    decide(reading, got, model, named, rotation, now, false)
}

/// [`rotate_on_record`] for a worker running under `env` and `flags`.
#[must_use]
pub fn rotate_worker_on_record(
    asked: Option<&str>,
    env: &[(String, String)],
    flags: &[String],
    now: i64,
) -> Option<LaunchPick> {
    let state = machine_state()?;
    let home = env
        .iter()
        .find(|(n, _)| n == super::upgrade_agy::HOME_ENV)
        .map(|(_, v)| v.as_str());
    let store = Store::new(&state, &launch_key(home, flags));
    let (rotation, _) =
        agy_quota::Rotation::from_path(aterm_types::dirs::aterm_config_path().as_deref());
    rotate_on_record(asked, &store, &rotation, now)
}

/// When an agy launch for `asked` (or the kept default model when `None`) that [`rotate_launch`]
/// refused with [`agy_quota::Pick::Wait`] may start, from `store`'s kept reading as it stands
/// (reading no usage): the earliest refill `until > now` (or `until + RESET_SETTLE_S` when a read
/// at or after `until` still saw the pre-refill zero inside the settling window) while
/// `rotation.enabled` holds and [`agy_quota::pick`] still waits, else `None`.
#[must_use]
pub fn launch_opening(
    asked: Option<&str>,
    store: &Store,
    rotation: &agy_quota::Rotation,
    now: i64,
) -> Option<i64> {
    if !rotation.enabled {
        return None;
    }
    let reading = store.last()?;
    let (model, named) = match asked {
        Some(m) => (m.to_owned(), true),
        None => (store.last_default()?, false),
    };
    match agy_quota::pick(&reading.quota, &model, named, rotation) {
        agy_quota::Pick::Wait {
            until: Some(until), ..
        } => {
            if until > now {
                Some(until)
            } else {
                let settled = until.saturating_add(RESET_SETTLE_S);
                (reading.as_of >= until && settled > now).then_some(settled)
            }
        }
        _ => None,
    }
}

/// [`launch_opening`] for a worker started with `env` and `flags`.
#[must_use]
pub fn worker_opening(
    asked: Option<&str>,
    env: &[(String, String)],
    flags: &[String],
    now: i64,
) -> Option<i64> {
    let state = machine_state()?;
    let home = env
        .iter()
        .find(|(n, _)| n == super::upgrade_agy::HOME_ENV)
        .map(|(_, v)| v.as_str());
    let store = Store::new(&state, &launch_key(home, flags));
    let (rotation, _) =
        agy_quota::Rotation::from_path(aterm_types::dirs::aterm_config_path().as_deref());
    launch_opening(asked, &store, &rotation, now)
}

/// [`rotate_launch`] for a worker started as `exe` under the starter's `env` and `flags`: this
/// machine's kept reading for that home, aterm.toml's `[harness.agy]`, and a read bounded by
/// [`LAUNCH_READ_BUDGET`].
#[must_use]
pub fn rotate_worker(
    asked: Option<&str>,
    exe: &Path,
    env: &[(String, String)],
    flags: &[String],
    now: i64,
) -> Option<LaunchPick> {
    worker_read(asked, exe, env, flags, now, false)
}

/// [`rotate_worker`] for a session stopped at a quota wall on its screen ([`rotate_launch_at`]).
#[must_use]
pub fn rotate_worker_at_wall(
    asked: Option<&str>,
    exe: &Path,
    env: &[(String, String)],
    flags: &[String],
    now: i64,
) -> Option<LaunchPick> {
    worker_read(asked, exe, env, flags, now, true)
}

fn worker_read(
    asked: Option<&str>,
    exe: &Path,
    env: &[(String, String)],
    flags: &[String],
    now: i64,
    at_wall: bool,
) -> Option<LaunchPick> {
    let state = machine_state()?;
    let home = env
        .iter()
        .find(|(n, _)| n == super::upgrade_agy::HOME_ENV)
        .map(|(_, v)| v.as_str());
    let store = Store::new(&state, &launch_key(home, flags));
    let source = Agy {
        program: exe.as_os_str().to_owned(),
        env: env.to_vec(),
        flags: flags.to_vec(),
        budget: LAUNCH_READ_BUDGET,
    };
    let (rotation, _) =
        agy_quota::Rotation::from_path(aterm_types::dirs::aterm_config_path().as_deref());
    rotate_launch_at(asked, &store, &source, &rotation, now, at_wall)
}

#[cfg(test)]
#[path = "agy_quota_store_tests.rs"]
mod tests;
