// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The readings series: one append-only history of each account's usage figures, the first
//! slice of the series `docs/DESIGN-account-stats-2026-10-15.md` §5.1 designs, kept for the
//! accounts outlook's pace (`docs/DESIGN-accounts-outlook-2026-10-25.md` §4).
//!
//! The series lives beside the accounts store, never in it: `<harness_state>/accounts/stats/`
//! holds one `<series>.jsonl` per account, a directory of mode 0700 and files of mode 0600,
//! under its own lock (`stats/.lock`), never `accounts.lock`. A row carries a label's figures
//! and the machine that read them, and never an email, a uuid or prose.
//!
//! Three kinds of row are written: `fig` (a recorded tap reading's windows), `beat` (a recorded
//! reading whose figures equal the last `fig` row's) and `plan` (a row's tier, learned from the
//! footer's gated profile read).

use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_json::{Map, Value};

use super::accounts::{self, WindowFig};

/// The directory under the accounts store the series live in.
pub const DIR: &str = "stats";

/// The series' own lock file in [`DIR`].
pub const LOCK_FILE: &str = ".lock";

/// The row schema.
pub const SCHEMA: u64 = 1;

/// How long a row is kept: one week and the pace's 24 h lookback.
pub const KEEP_S: i64 = 8 * 86_400;

/// A file with more rows than this is trimmed ([`KEEP_ROWS`]).
pub const TRIM_ROWS: usize = 8192;

/// A file larger than this is trimmed ([`KEEP_ROWS`]).
pub const TRIM_BYTES: u64 = 2 * 1024 * 1024;

/// A file past this size is counted for [`TRIM_ROWS`] on an append.
const COUNT_FROM_BYTES: u64 = 480 * 1024;

/// What a trim keeps: the newest rows within [`KEEP_S`], at most this many.
pub const KEEP_ROWS: usize = 4096;

/// The most bytes one row may take.
pub const ROW_MAX: usize = 512;

/// How long a writer waits for [`LOCK_FILE`] before it drops its row.
pub const LOCK_WAIT: Duration = Duration::from_millis(50);

/// The most bytes a reader takes of one file.
pub const READ_MAX: u64 = 2 * TRIM_BYTES;

/// How far back from a file's end a writer looks for the last `fig` row.
const LAST_FIG_BYTES: u64 = 4096;

/// One window of a `fig` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fig {
    /// Tenths of a percent used.
    pub pct_x10: u32,
    /// The window's reset.
    pub reset: i64,
    /// When the figure was served.
    pub at: i64,
    /// Where it came from ([`accounts::SOURCE_TAP`], [`accounts::SOURCE_SAMPLE`]).
    pub src: String,
    /// A limit reset used inside the window ([`WindowFig::epoch_at`]).
    pub epoch: Option<i64>,
}

impl Fig {
    /// The series' copy of a store figure.
    #[must_use]
    pub fn of(fig: &WindowFig) -> Fig {
        Fig {
            pct_x10: fig.pct_x10,
            reset: fig.reset,
            at: fig.at,
            src: fig.source.clone(),
            epoch: fig.epoch_at,
        }
    }

    fn same_figure(&self, other: &Fig) -> bool {
        (self.pct_x10, self.reset, self.epoch) == (other.pct_x10, other.reset, other.epoch)
    }
}

/// A row's tier, as the footer's gated profile read named it ([`note_plan`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlanSaid {
    /// The plan's word, lower case (`max`, `pro`).
    pub word: String,
    /// Its tier where one was named (`20x`).
    pub tier: Option<String>,
    /// What named it (`profile`).
    pub src: String,
}

/// What one row says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// A recorded reading's windows.
    Fig {
        /// The five-hour window.
        five_hour: Option<Fig>,
        /// The week.
        seven_day: Option<Fig>,
    },
    /// A recorded reading whose figures equal the last `fig` row's.
    Beat,
    /// The row's tier.
    Plan(PlanSaid),
}

/// One row of a series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesRow {
    /// When it was written.
    pub t: i64,
    /// The machine that wrote it.
    pub by: String,
    /// What it says.
    pub said: Said,
}

impl SeriesRow {
    /// The row's figure of `window` ([`accounts::SESSION_LIMIT`], [`accounts::WEEK_LIMIT`]).
    #[must_use]
    pub fn fig(&self, window: &str) -> Option<&Fig> {
        match &self.said {
            Said::Fig {
                five_hour,
                seven_day,
            } => match window {
                accounts::SESSION_LIMIT => five_hour.as_ref(),
                accounts::WEEK_LIMIT => seven_day.as_ref(),
                _ => None,
            },
            Said::Beat | Said::Plan(_) => None,
        }
    }

    /// The row as one line of its file, at most [`ROW_MAX`] bytes; `None` past that.
    #[must_use]
    pub fn line(&self) -> Option<String> {
        let mut o = Map::new();
        o.insert("v".into(), Value::from(SCHEMA));
        o.insert("t".into(), Value::from(self.t));
        let kind = match &self.said {
            Said::Fig {
                five_hour,
                seven_day,
            } => {
                let mut w = Map::new();
                for (name, fig) in [
                    (accounts::SESSION_LIMIT, five_hour),
                    (accounts::WEEK_LIMIT, seven_day),
                ] {
                    if let Some(fig) = fig {
                        let mut f = Map::new();
                        f.insert("pct_x10".into(), Value::from(fig.pct_x10));
                        f.insert("reset".into(), Value::from(fig.reset));
                        f.insert("at".into(), Value::from(fig.at));
                        f.insert("src".into(), Value::from(fig.src.clone()));
                        if let Some(epoch) = fig.epoch {
                            f.insert("epoch".into(), Value::from(epoch));
                        }
                        w.insert(name.into(), Value::Object(f));
                    }
                }
                o.insert("w".into(), Value::Object(w));
                "fig"
            }
            Said::Beat => "beat",
            Said::Plan(plan) => {
                let mut p = Map::new();
                p.insert("word".into(), Value::from(plan.word.clone()));
                if let Some(tier) = &plan.tier {
                    p.insert("tier".into(), Value::from(tier.clone()));
                }
                p.insert("src".into(), Value::from(plan.src.clone()));
                o.insert("plan".into(), Value::Object(p));
                "plan"
            }
        };
        o.insert("k".into(), Value::from(kind));
        o.insert("by".into(), Value::from(self.by.clone()));
        let line = aterm_json::to_string(&Value::Object(o)).ok()?;
        (line.len() <= ROW_MAX).then_some(line)
    }

    /// One line of a series file; `None` for a line it cannot read.
    #[must_use]
    pub fn parse(line: &str) -> Option<SeriesRow> {
        if line.len() > ROW_MAX {
            return None;
        }
        let v: Value = aterm_json::from_str(line).ok()?;
        let o = v.as_object()?;
        if o.get("v").and_then(Value::as_u64) != Some(SCHEMA) {
            return None;
        }
        let t = o.get("t").and_then(Value::as_i64)?;
        let by = o.get("by").and_then(Value::as_str).unwrap_or("").to_owned();
        let word = |o: &Map, key: &str| {
            o.get(key)
                .and_then(Value::as_str)
                .map(|s| s.chars().take(accounts::WORD_MAX).collect::<String>())
        };
        let said = match o.get("k").and_then(Value::as_str)? {
            "fig" => {
                let w = o.get("w").and_then(Value::as_object)?;
                let fig = |name: &str| {
                    let f = w.get(name)?.as_object()?;
                    Some(Fig {
                        pct_x10: u32::try_from(f.get("pct_x10")?.as_u64()?)
                            .ok()
                            .filter(|p| *p <= 10 * accounts::PCT_MAX)?,
                        reset: f.get("reset")?.as_i64()?,
                        at: f.get("at")?.as_i64()?,
                        src: word(f, "src").unwrap_or_default(),
                        epoch: f.get("epoch").and_then(Value::as_i64),
                    })
                };
                Said::Fig {
                    five_hour: fig(accounts::SESSION_LIMIT),
                    seven_day: fig(accounts::WEEK_LIMIT),
                }
            }
            "beat" => Said::Beat,
            "plan" => {
                let p = o.get("plan").and_then(Value::as_object)?;
                Said::Plan(PlanSaid {
                    word: word(p, "word")?,
                    tier: word(p, "tier"),
                    src: word(p, "src").unwrap_or_default(),
                })
            }
            _ => return None,
        };
        Some(SeriesRow { t, by, said })
    }
}

/// The series of `label` on `machine`: `u-<label>@<machine>`. A renamed label starts a new one.
#[must_use]
pub fn series_name(label: &str, machine: &str) -> String {
    format!("u-{label}@{machine}")
}

/// The series directory of the accounts store under `harness_state`.
#[must_use]
pub fn dir_in(harness_state: &Path) -> PathBuf {
    accounts::Store::at(harness_state)
        .state_file()
        .parent()
        .map_or_else(|| harness_state.join("accounts"), Path::to_path_buf)
        .join(DIR)
}

/// The file of `label`'s series on `machine` under `harness_state`; `None` for a label or a
/// machine id no file may be named after.
#[must_use]
pub fn path_of(harness_state: &Path, label: &str, machine: &str) -> Option<PathBuf> {
    let machine_ok = !machine.is_empty()
        && machine.len() <= accounts::WORD_MAX
        && machine.bytes().all(|b| b.is_ascii_alphanumeric());
    (accounts::admissible_label(label) && machine_ok)
        .then(|| dir_in(harness_state).join(format!("{}.jsonl", series_name(label, machine))))
}

/// What [`note`] and [`note_plan`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Noted {
    /// A `fig` row was appended.
    Fig,
    /// A `beat` row was appended.
    Beat,
    /// A `plan` row was appended.
    Plan,
    /// The plan equals the series' last `plan` row: nothing was written.
    SamePlan,
    /// The series' lock stayed held past [`LOCK_WAIT`]: the row was dropped (the next reading
    /// comes within `READING_HEARTBEAT_S`).
    Dropped,
    /// The label or machine names no file, or the reading carried no window.
    Nothing,
}

/// The held [`LOCK_FILE`]; dropping it releases the lock.
struct SeriesLock(std::fs::File);

impl Drop for SeriesLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn open_private(path: &Path, append: bool) -> std::io::Result<std::fs::File> {
    let mut open = std::fs::OpenOptions::new();
    open.create(true);
    if append {
        open.append(true);
    } else {
        open.write(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file: std::fs::File = aterm_uds::kept_out(|| open.open(path))?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: not a regular file", path.display()),
        ));
    }
    Ok(file)
}

fn lock_within(dir: &Path, wait: Duration) -> std::io::Result<Option<SeriesLock>> {
    aterm_types::fs_restricted::ensure_private_dir(dir)?;
    let file = open_private(&dir.join(LOCK_FILE), false)?;
    let step = Duration::from_millis(10);
    let mut waited = Duration::ZERO;
    loop {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: SeriesLock's Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => return Ok(Some(SeriesLock(file))),
            Err(std::fs::TryLockError::WouldBlock) if waited < wait => {
                std::thread::sleep(step);
                waited += step;
            }
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// The last `fig` row in the end of the file at `path`, looking back [`LAST_FIG_BYTES`].
fn last_fig(path: &Path) -> Option<SeriesRow> {
    let (mut file, meta) = super::footer::open_regular_with_meta(path)?;
    let len = meta.len();
    let from = len.saturating_sub(LAST_FIG_BYTES);
    file.seek(SeekFrom::Start(from)).ok()?;
    let cap = usize::try_from(
        len.saturating_sub(from)
            .min(LAST_FIG_BYTES)
            .saturating_add(1),
    )
    .unwrap_or(0);
    let mut text = String::with_capacity(cap);
    file.take(LAST_FIG_BYTES).read_to_string(&mut text).ok()?;
    text.lines()
        .rev()
        .filter_map(SeriesRow::parse)
        .find(|row| matches!(row.said, Said::Fig { .. }))
}

fn append_locked(path: &Path, row: &SeriesRow, now: i64) -> std::io::Result<()> {
    let Some(mut line) = row.line() else {
        return Ok(());
    };
    line.push('\n');
    let mut file = open_private(path, true)?;
    file.write_all(line.as_bytes())?;
    let len = file.metadata()?.len();
    drop(file);
    if len > COUNT_FROM_BYTES {
        trim_locked(path, now, len > TRIM_BYTES)?;
    }
    Ok(())
}

/// Rewrites the series at `path` to its newest [`KEEP_ROWS`] rows within [`KEEP_S`] of `now`
/// (its newest `plan` row kept however old), where it has more than [`TRIM_ROWS`] rows or
/// `oversize` says it passed [`TRIM_BYTES`]: a staged file, flushed, then renamed over it. The
/// caller holds [`LOCK_FILE`].
fn trim_locked(path: &Path, now: i64, oversize: bool) -> std::io::Result<()> {
    let Some(text) = accounts::read_bounded(path, READ_MAX)? else {
        return Ok(());
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= TRIM_ROWS && !oversize {
        return Ok(());
    }
    let rows: Vec<(&str, Option<SeriesRow>)> =
        lines.iter().map(|l| (*l, SeriesRow::parse(l))).collect();
    let plan_at = rows.iter().rposition(|(_, row)| {
        row.as_ref()
            .is_some_and(|r| matches!(r.said, Said::Plan(_)))
    });
    let kept: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, (_, row))| {
            row.as_ref()
                .is_some_and(|r| now.saturating_sub(r.t) < KEEP_S)
        })
        .map(|(i, _)| i)
        .collect();
    let kept = &kept[kept.len().saturating_sub(KEEP_ROWS)..];
    let mut out = String::new();
    if let Some(plan) = plan_at.filter(|at| !kept.contains(at)) {
        out.push_str(rows[plan].0);
        out.push('\n');
    }
    for i in kept {
        out.push_str(rows[*i].0);
        out.push('\n');
    }
    super::upgrade_models::write_atomic_mode(path, &out, Some(0o600))
}

/// Appends `label`'s recorded reading on `machine` to its series under `harness_state`: a `fig`
/// row of its windows, or a `beat` where they equal the last `fig` row's. Called right after the
/// accounts store recorded the reading ([`accounts::note_reading_held`] gave events and held
/// nothing back), so its cadence is the store's: a whole-percent change or the reading heartbeat.
/// # Errors
/// The directory, the lock or the file could not be made or written.
pub fn note(
    harness_state: &Path,
    label: &str,
    machine: &str,
    five_hour: Option<&WindowFig>,
    seven_day: Option<&WindowFig>,
    now: i64,
) -> std::io::Result<Noted> {
    note_within(
        harness_state,
        label,
        machine,
        (five_hour.map(Fig::of), seven_day.map(Fig::of)),
        now,
        LOCK_WAIT,
    )
}

/// [`note`] of series figures, waiting at most `wait` for the series lock.
pub(crate) fn note_within(
    harness_state: &Path,
    label: &str,
    machine: &str,
    (five_hour, seven_day): (Option<Fig>, Option<Fig>),
    now: i64,
    wait: Duration,
) -> std::io::Result<Noted> {
    let Some(path) = path_of(harness_state, label, machine) else {
        return Ok(Noted::Nothing);
    };
    if five_hour.is_none() && seven_day.is_none() {
        return Ok(Noted::Nothing);
    }
    let dir = dir_in(harness_state);
    let Some(_lock) = lock_within(&dir, wait)? else {
        return Ok(Noted::Dropped);
    };
    let same = last_fig(&path).is_some_and(|last| {
        let same = |a: Option<&Fig>, b: &Option<Fig>| match (a, b) {
            (Some(a), Some(b)) => a.same_figure(b),
            (None, None) => true,
            _ => false,
        };
        same(last.fig(accounts::SESSION_LIMIT), &five_hour)
            && same(last.fig(accounts::WEEK_LIMIT), &seven_day)
    });
    let (said, noted) = if same {
        (Said::Beat, Noted::Beat)
    } else {
        (
            Said::Fig {
                five_hour,
                seven_day,
            },
            Noted::Fig,
        )
    };
    append_locked(
        &path,
        &SeriesRow {
            t: now,
            by: machine.to_owned(),
            said,
        },
        now,
    )?;
    Ok(noted)
}

/// Whether a tap reading's transaction recorded it ([`accounts::note_reading_held`]'s `events`
/// and `held_back`): it gave events and held nothing back. Only then does the series take a row.
#[must_use]
pub fn recorded(events: &[accounts::Event], held_back: bool) -> bool {
    !events.is_empty() && !held_back
}

/// [`note`] where the reading was [`recorded`], with the store's figures of `label` after the
/// transaction (`figures`: its five-hour window and its week); [`Noted::Nothing`] where not.
/// # Errors
/// As [`note`].
pub fn note_recorded(
    harness_state: &Path,
    label: &str,
    machine: &str,
    figures: (Option<&WindowFig>, Option<&WindowFig>),
    recorded: bool,
    now: i64,
) -> std::io::Result<Noted> {
    if !recorded {
        return Ok(Noted::Nothing);
    }
    note(harness_state, label, machine, figures.0, figures.1, now)
}

/// Appends `plan` to `label`'s series on `machine` under `harness_state`, unless the series'
/// newest `plan` row says the same.
/// # Errors
/// As [`note`].
pub fn note_plan(
    harness_state: &Path,
    label: &str,
    machine: &str,
    plan: &PlanSaid,
    now: i64,
) -> std::io::Result<Noted> {
    let Some(path) = path_of(harness_state, label, machine) else {
        return Ok(Noted::Nothing);
    };
    let dir = dir_in(harness_state);
    let Some(_lock) = lock_within(&dir, LOCK_WAIT)? else {
        return Ok(Noted::Dropped);
    };
    let held = read_path(&path);
    if newest_plan(&held).is_some_and(|said| said.word == plan.word && said.tier == plan.tier) {
        return Ok(Noted::SamePlan);
    }
    append_locked(
        &path,
        &SeriesRow {
            t: now,
            by: machine.to_owned(),
            said: Said::Plan(plan.clone()),
        },
        now,
    )?;
    Ok(Noted::Plan)
}

/// The series' newest `plan` row's plan.
#[must_use]
pub fn newest_plan(rows: &[SeriesRow]) -> Option<&PlanSaid> {
    rows.iter().rev().find_map(|row| match &row.said {
        Said::Plan(plan) => Some(plan),
        _ => None,
    })
}

fn read_path(path: &Path) -> Vec<SeriesRow> {
    accounts::read_bounded(path, READ_MAX)
        .ok()
        .flatten()
        .map(|text| text.lines().filter_map(SeriesRow::parse).collect())
        .unwrap_or_default()
}

/// Every readable row of `label`'s series on `machine` under `harness_state`, oldest first.
/// Readers take no lock.
#[must_use]
pub fn read(harness_state: &Path, label: &str, machine: &str) -> Vec<SeriesRow> {
    path_of(harness_state, label, machine)
        .map(|path| read_path(&path))
        .unwrap_or_default()
}

/// What a [`Tail`] last saw of its file: its identity, length and change time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Stamp {
    ino: u64,
    len: u64,
    mtime_ns: i128,
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    let meta = std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)?;
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt as _;
        meta.ino()
    };
    #[cfg(not(unix))]
    let ino = 0;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(i128::MAX));
    Some(Stamp {
        ino,
        len: meta.len(),
        mtime_ns,
    })
}

/// An incremental reader of one series file: a `stat` per [`Tail::refresh`], the appended bytes
/// only while the file grows, the whole file (at most [`READ_MAX`]) after a trim's rename.
#[derive(Debug, Clone, Default)]
pub struct Tail {
    path: PathBuf,
    stamp: Option<Stamp>,
    offset: u64,
    rows: Vec<SeriesRow>,
}

impl Tail {
    /// A reader of the file at `path`, nothing read yet.
    #[must_use]
    pub fn new(path: PathBuf) -> Tail {
        Tail {
            path,
            ..Tail::default()
        }
    }

    /// The file it reads.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The rows read, oldest first.
    #[must_use]
    pub fn rows(&self) -> &[SeriesRow] {
        &self.rows
    }

    /// Reads what changed since the last call; `true` where the rows changed.
    pub fn refresh(&mut self) -> bool {
        let stamp = stamp_of(&self.path);
        if stamp == self.stamp {
            return false;
        }
        let grown = match (self.stamp, stamp) {
            (Some(was), Some(now)) => was.ino == now.ino && now.len >= self.offset,
            _ => false,
        };
        self.stamp = stamp;
        let Some(stamp) = stamp else {
            let had = !self.rows.is_empty();
            self.rows.clear();
            self.offset = 0;
            return had;
        };
        if !grown {
            self.rows.clear();
            self.offset = 0;
        }
        let Some(mut file) = super::footer::open_regular(&self.path) else {
            return false;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let max_read = READ_MAX.min(stamp.len.saturating_sub(self.offset));
        let cap = usize::try_from(max_read.saturating_add(1)).unwrap_or(0);
        let mut bytes = Vec::with_capacity(cap);
        if file.take(max_read).read_to_end(&mut bytes).is_err() {
            return false;
        }
        let whole = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |at| at + 1);
        let before = self.rows.len();
        self.rows.extend(
            String::from_utf8_lossy(&bytes[..whole])
                .lines()
                .filter_map(SeriesRow::parse),
        );
        self.offset += whole as u64;
        if self.rows.len() > TRIM_ROWS {
            let cut = self.rows.len() - KEEP_ROWS;
            let plan = self.rows[..cut]
                .iter()
                .rev()
                .find(|row| matches!(row.said, Said::Plan(_)))
                .cloned();
            self.rows.drain(..cut);
            if let Some(plan) = plan {
                self.rows.insert(0, plan);
            }
        }
        self.rows.len() != before || !grown
    }
}

#[cfg(test)]
#[path = "account_series_tests.rs"]
mod tests;
