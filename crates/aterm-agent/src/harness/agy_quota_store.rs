// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! agy's quota over time: the last reading per agy home, kept under the harness state, the
//! history of readings it was made from, and the burn rate that history gives each limit.

use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use super::agy_quota::{self, Quota, Standing};

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
        if now < self.as_of || now - self.as_of >= ttl {
            return false;
        }
        let refilled = self
            .quota
            .groups
            .iter()
            .flat_map(|g| g.buckets.iter())
            .filter(|b| b.spent())
            .filter_map(agy_quota::Bucket::reset_at)
            .any(|at| now >= at + RESET_SETTLE_S);
        !refilled
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
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let words = home
        .iter()
        .map(|h| format!("HOME={h}"))
        .chain(flags.iter().cloned());
    for word in words {
        for b in word.bytes().chain(std::iter::once(0)) {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("h{hash:016x}")
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
        let text = std::fs::read_to_string(self.model_path()).ok()?;
        let (at, model) = text.trim().split_once(' ')?;
        let at: i64 = at.parse().ok()?;
        (now >= at && now - at < FRESH_OPEN_S && !model.is_empty()).then(|| model.to_owned())
    }

    /// Keep agy's default model as read at `now`.
    pub fn keep_default(&self, model: &str, now: i64) {
        if std::fs::create_dir_all(&self.dir).is_ok() {
            let _ = super::upgrade_models::write_atomic(
                &self.model_path(),
                &format!("{now} {model}\n"),
            );
        }
    }

    fn failed_path(&self) -> PathBuf {
        self.dir.join(format!("{}.failed", self.key))
    }

    /// The last failed read, `(unix seconds, why)`, while under [`FAIL_BACKOFF_S`] old at `now`.
    #[must_use]
    pub fn recent_failure(&self, now: i64) -> Option<(i64, String)> {
        let text = std::fs::read_to_string(self.failed_path()).ok()?;
        let (at, why) = text.trim().split_once(' ')?;
        let at: i64 = at.parse().ok()?;
        (now >= at && now - at < FAIL_BACKOFF_S).then(|| (at, why.to_owned()))
    }

    fn note_failure(&self, why: &str, now: i64) {
        if std::fs::create_dir_all(&self.dir).is_ok() {
            let one_line = why.replace('\n', " ");
            let _ = super::upgrade_models::write_atomic(
                &self.failed_path(),
                &format!("{now} {one_line}\n"),
            );
        }
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join(format!("{}.lock", self.key))
    }

    /// The last reading, if one was kept and still parses.
    #[must_use]
    pub fn last(&self) -> Option<Reading> {
        let text = std::fs::read_to_string(self.last_path()).ok()?;
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
        use std::io::Write as _;
        let path = self.history_path();
        let mut line = history_line(reading);
        line.push('\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
            .write_all(line.as_bytes())?;
        let text = std::fs::read_to_string(&path)?;
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() > 2 * HISTORY_KEEP {
            let kept = lines[lines.len() - HISTORY_KEEP..].join("\n") + "\n";
            super::upgrade_models::write_atomic(&path, &kept)?;
        }
        Ok(())
    }

    /// Every sample of the history, oldest first.
    #[must_use]
    pub fn history(&self) -> Vec<Sample> {
        std::fs::read_to_string(self.history_path())
            .map(|text| text.lines().filter_map(Sample::parse).collect())
            .unwrap_or_default()
    }

    /// Take the read lock: `None` while another reader holds a fresh one.
    #[must_use]
    pub fn try_lock(&self) -> Option<ReadLock> {
        std::fs::create_dir_all(&self.dir).ok()?;
        let path = self.lock_path();
        let take = || {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .ok()
        };
        if take().is_none() {
            let stale = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_none_or(|age| age.as_secs() >= LOCK_STALE_S);
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
                        left: p.get("left").and_then(Value::as_f64),
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
        .filter(|s| s.at >= last.at - BURN_LOOKBACK_S && s.at < last.at)
        .find_map(|s| {
            s.point(id)
                .filter(|p| !p.off && p.reset == last_point.reset && p.left.is_some())
                .map(|p| (s.at, *p))
        })?;
    let span = last.at - first.0;
    if span < BURN_MIN_SPAN_S {
        return None;
    }
    let used = first.1.left? - last_point.left?;
    let per_hour = (used * 3_600.0 / span as f64).max(0.0);
    let left = last_point.left?;
    let empty_at = (per_hour > 0.0)
        .then(|| last.at + (left / per_hour * 3_600.0) as i64)
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
/// from `source` under the read lock and kept; a failed read falls back to the stale reading.
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

/// What [`rotate_launch`] decided, with the reading it decided on.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchPick {
    /// The decision.
    pub pick: agy_quota::Pick,
    /// The model was named by the caller (`model=`), not agy's default.
    pub named: bool,
    /// Where the reading came from.
    pub got: Got,
}

/// Choose the model an agy launch runs ([`agy_quota::pick`]) for `asked` (a `model=`), or agy's
/// default when `None`, from `store`'s current reading. `None` when rotation is off, or no
/// reading or default model could be had: the launch then runs as asked.
#[must_use]
pub fn rotate_launch(
    asked: Option<&str>,
    store: &Store,
    source: &dyn Source,
    rotation: &agy_quota::Rotation,
    now: i64,
) -> Option<LaunchPick> {
    if !rotation.enabled {
        return None;
    }
    let (reading, got) = current(store, source, now, rotation.low, false).ok()?;
    let (model, named) = match asked {
        Some(m) => (m.to_owned(), true),
        None => {
            let model = match store.kept_default(now) {
                Some(m) => m,
                None => {
                    let m = source.default_model().ok()?;
                    store.keep_default(&m, now);
                    m
                }
            };
            (model, false)
        }
    };
    Some(LaunchPick {
        pick: agy_quota::pick(&reading.quota, &model, named, rotation),
        named,
        got,
    })
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
    rotate_launch(asked, &store, &source, &rotation, now)
}

#[cfg(test)]
#[path = "agy_quota_store_tests.rs"]
mod tests;
