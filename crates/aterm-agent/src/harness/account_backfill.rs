// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account backfill (`docs/DESIGN-account-stats-2026-10-15.md` §5 "Backfill"): the usage
//! walls a Claude Code home's own transcripts record of windows this Mac's readings series holds
//! no word of, booked as `close` rows ([`account_series::SRC_BACKFILL`]) that `accounts history`
//! makes in memory at each run and never writes, with the recorder on or off. The series holds
//! only what the usage tap read while it was on, of the sessions aterm hosted: the walls from
//! before it began, from a stretch the tap was off, and from a session run elsewhere are in the
//! transcripts alone. Writing them would need a transcript walk on a harness worker. Only
//! `accounts history` reads it, so no decision does (§8.2). Of each wall it keeps when its row
//! was written, its window and that window's reset, never a row's text.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::time::UNIX_EPOCH;

use super::account_series::{self, Close, Said, SeriesRow};
use super::accounts::{SESSION_LIMIT, SESSION_WINDOW_S, WEEK_LIMIT, WEEK_WINDOW_S};
use super::profile::AccountKey;
use super::spend::{self, Limits};

/// The most bytes one read takes from a home's transcripts, over all of them.
pub const READ_BYTES: u64 = 1 << 30;

/// A wall's figure: 100%, in tenths, which a close reads as `walled`.
const FULL_X10: u32 = 1000;

/// One usage wall a transcript recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wall {
    /// When its row was written.
    pub at: i64,
    /// Its window's reset (`resetsAt`), one instant for every wall of a window (the design's F6).
    pub reset: i64,
    /// Its window ([`SESSION_LIMIT`], [`WEEK_LIMIT`]).
    pub w: &'static str,
}

/// What cut a read short ([`Walls::cut`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    /// Its deadline passed.
    Time,
    /// It read its byte cap.
    Bytes,
    /// The walk over the transcripts stopped at its bounds, or a transcript or a directory could
    /// not be read.
    Walk,
}

impl Cut {
    /// The word `--json` says it by.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Cut::Time => "time",
            Cut::Bytes => "bytes",
            Cut::Walk => "walk",
        }
    }
}

/// What one read of a home's transcripts found ([`read`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Walls {
    /// The walls, in the order read.
    pub walls: Vec<Wall>,
    /// THE NEWEST INSTANT THROUGH WHICH THE HOME MAY HAVE SERVED ANOTHER LOGIN, as far as its
    /// transcripts tell: the newest `/login` that succeeded in a transcript read (its row's time,
    /// else its file's last write); the end of each conversation another login held, whose
    /// `session_context` named another address than the home's (where a later one named the
    /// home's, that row's time, else the transcript's last write); and where the read was cut,
    /// the last write of the newest transcript it did not read whole (a row it did not read was
    /// written by then, and one written since came after the home's profile was read, so it says
    /// nothing of whose an earlier wall was), or `now` where the walk itself was cut (a
    /// transcript it did not list may hold a row of any time).
    pub unsure_through: Option<i64>,
    /// What cut the read short; `None`: every transcript written since the read's `since` was
    /// read to the end it had.
    pub cut: Option<Cut>,
}

impl Walls {
    fn unsure(&mut self, at: i64) {
        self.unsure_through = Some(self.unsure_through.map_or(at, |t| t.max(at)));
    }
}

/// A reader that ends where `limits`' deadline passed, as at the end of its file.
struct Budgeted<'a, R> {
    inner: R,
    limits: &'a Limits,
}

impl<R: Read> Read for Budgeted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.limits.over() {
            return Ok(0);
        }
        self.inner.read(buf)
    }
}

/// The instant a file was last written, Unix seconds.
fn written(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// One transcript as [`fold`] reads it: the address the home's profile is signed in as, which is
/// compared and never kept, the transcript's last write, and whether its newest
/// `session_context` named another address.
struct Transcript<'a> {
    address: &'a str,
    written: i64,
    theirs: bool,
}

/// `raw`, one line of the transcript `file`, folded into `out`: only a line that holds
/// `"quotaLimits"` and `"rejected"`, `Login successful`, or a `session_context` with a
/// `userEmail` is parsed, and of a wall or a `/login` only the projection the transcript readers
/// share ([`super::transcript::conversation`]). A `session_context` ([`super::accounts_sync`]'s
/// reading of it) that names another address than the home's marks the conversation another
/// login's; one that names the home's after it raises [`Walls::unsure_through`] to its row's
/// time, else to the transcript's last write. A `/login` that succeeded
/// ([`super::upgrade::is_login_success_row`]) raises it to its row's time, else to the
/// transcript's last write. A wall row ([`super::upgrade::wall_of_row`]: no spend or credit
/// wall) of the five-hour window or the week that names its reset adds a [`Wall`].
fn fold(raw: &[u8], file: &mut Transcript<'_>, out: &mut Walls) {
    let Ok(line) = std::str::from_utf8(raw) else {
        return;
    };
    if line.contains("\"session_context\"") && line.contains("\"userEmail\"") {
        if let Some(named) = super::accounts_sync::session_context_email(line) {
            let ours = super::accounts::email_matches(file.address, &named);
            if ours && file.theirs {
                let at = super::transcript::conversation(line)
                    .ok()
                    .and_then(|v| super::upgrade::row_time(&v))
                    .and_then(|t| i64::try_from(t).ok());
                out.unsure(at.unwrap_or(file.written));
            }
            file.theirs = !ours;
        }
        return;
    }
    let wall = line.contains("\"quotaLimits\"") && line.contains("\"rejected\"");
    let login = line.contains("Login successful");
    if !wall && !login {
        return;
    }
    let Ok(v) = super::transcript::conversation(line) else {
        return;
    };
    if login && super::upgrade::is_login_success_row(&v) {
        let at = super::upgrade::row_time(&v).and_then(|t| i64::try_from(t).ok());
        out.unsure(at.unwrap_or(file.written));
        return;
    }
    let Some(found) = super::upgrade::wall_of_row(&v) else {
        return;
    };
    let w = match found.window.as_deref() {
        Some(SESSION_LIMIT) => SESSION_LIMIT,
        Some(WEEK_LIMIT) => WEEK_LIMIT,
        _ => return,
    };
    let (Ok(at), Some(Ok(reset))) = (i64::try_from(found.at), found.until.map(i64::try_from))
    else {
        return;
    };
    out.walls.push(Wall { at, reset, w });
}

/// One transcript read from its start, line by line ([`super::usage::MAX_LINE_BYTES`], through
/// [`super::accounts_sync::each_bounded_line`]), into `out`, its `session_context` compared with
/// `address` ([`fold`]), spending at most `left` bytes (what it reads is taken off) and stopping
/// at `limits`' deadline; `None` where it was read to the end it had when opened, else what cut
/// it. A line past the bound is passed over: none of the rows the fold reads (a limit's
/// synthetic message, a local command's output, a `session_context` attachment) comes near it,
/// so such a line is a tool's result or an inlined file, which the fold passes over too.
fn read_one(
    found: &spend::Found,
    address: &str,
    left: &mut u64,
    limits: &Limits,
    out: &mut Walls,
) -> Option<Cut> {
    let Some((file, meta)) = super::footer::open_regular_with_meta(&found.path) else {
        return Some(Cut::Walk);
    };
    let mut transcript = Transcript {
        address,
        written: written(&meta),
        theirs: false,
    };
    let mut reader = BufReader::new(Budgeted {
        inner: file.take(*left),
        limits,
    });
    let through =
        super::accounts_sync::each_bounded_line(&mut reader, super::usage::MAX_LINE_BYTES, |raw| {
            fold(raw, &mut transcript, out);
        });
    if transcript.theirs {
        out.unsure(transcript.written);
    }
    let read = left.saturating_sub(reader.get_ref().inner.limit());
    *left = left.saturating_sub(read);
    if through.is_err() {
        Some(Cut::Walk)
    } else if read >= meta.len() {
        None
    } else if limits.over() {
        Some(Cut::Time)
    } else {
        Some(Cut::Bytes)
    }
}

/// The walls the transcripts under `projects` (a Claude Code home's, [`spend::walk`]) last
/// written at or after `since` record, read newest first within `limits` (bytes over all of
/// them, and a deadline), each `session_context` compared with `address`, the address the
/// home's profile is signed in as (held for the comparison only), with the newest instant
/// through which the home may have served another login ([`Walls::unsure_through`]) and what cut
/// the read short, at `now`.
#[must_use]
pub fn read(projects: &Path, address: &str, since: i64, limits: &Limits, now: i64) -> Walls {
    let mut out = Walls::default();
    let walk = spend::walk(projects, since, limits);
    if walk.cut || walk.unread > 0 {
        out.cut = Some(if limits.over() { Cut::Time } else { Cut::Walk });
        out.unsure(now);
        return out;
    }
    let mut left = limits.bytes;
    for found in &walk.files {
        let cut = if limits.over() {
            Some(Cut::Time)
        } else if left == 0 {
            Some(Cut::Bytes)
        } else {
            read_one(found, address, &mut left, limits, &mut out)
        };
        if let Some(cut) = cut {
            out.cut = Some(cut);
            out.unsure(written(&found.meta));
            break;
        }
    }
    out
}

/// What one account's series holds of a window ([`held`]): the anchors (resets) it names, and
/// the stretches it read the window in, by start, each with the latest end of any stretch up to
/// it.
struct Held {
    anchors: BTreeSet<i64>,
    seen: Vec<(i64, i64)>,
    ends: Vec<i64>,
}

impl Held {
    /// `seen`'s stretches, sorted, with the latest end up to each.
    fn of(anchors: BTreeSet<i64>, mut seen: Vec<(i64, i64)>) -> Held {
        seen.sort_unstable();
        let ends = seen
            .iter()
            .scan(i64::MIN, |latest, (_, hi)| {
                *latest = (*latest).max(*hi);
                Some(*latest)
            })
            .collect();
        Held {
            anchors,
            seen,
            ends,
        }
    }

    /// Whether a stretch the series read the window in meets `[lo, hi]`.
    fn meets(&self, lo: i64, hi: i64) -> bool {
        let upto = self.seen.partition_point(|(start, _)| *start <= hi);
        upto.checked_sub(1).is_some_and(|i| self.ends[i] >= lo)
    }

    /// Whether the window that resets at `reset`, `len` long, is one the series holds: it names
    /// its anchor, or read the window at an instant inside it.
    fn holds(&self, reset: i64, len: i64) -> bool {
        self.anchors.contains(&reset) || self.meets(reset.saturating_sub(len), reset)
    }
}

/// What the series files `files` (one account's rows of each, by `t`) hold of window `w`: each
/// reading's instant (a beat as the reading it repeats), each close's first to last reading, and
/// their anchors; a `day` row's week reset and the five-hour anchors it names, whatever else its
/// day holds (another file's readings of the day say nothing of the windows this row counts);
/// and a `day` row's whole day only where it stands alone, its day holding none of those (a
/// tier let them go; a five-hour window the row names by anchor ended inside its day, so the
/// whole day holds it): compaction writes a day's row the day after, beside the rows it was made
/// from, and the day's first reading, not its midnight, is when the series began to read it.
fn held(files: &[Vec<SeriesRow>], w: &str) -> Held {
    let mut anchors = BTreeSet::new();
    let mut seen: Vec<(i64, i64)> = Vec::new();
    let mut days: Vec<(i64, i64)> = Vec::new();
    for rows in files {
        let mut last_fig: Option<&SeriesRow> = None;
        for row in rows {
            let read = match &row.said {
                Said::Fig { .. } => {
                    last_fig = Some(row);
                    row.fig(w)
                        .map(|fig| (fig.at.min(row.t), fig.at.max(row.t), fig.reset))
                }
                Said::Beat => last_fig
                    .and_then(|fig| fig.fig(w))
                    .map(|fig| (row.t, row.t, fig.reset)),
                Said::Close(c) if c.w == w => {
                    Some((c.first.min(c.last), c.last.max(c.first), c.reset))
                }
                Said::Day(d) => {
                    let end = row.t.saturating_add(d.span_s).saturating_sub(1);
                    if w == WEEK_LIMIT
                        && let Some(week) = d.seven_day
                    {
                        anchors.insert(week.reset);
                        days.push((row.t, end));
                    } else if w == SESSION_LIMIT
                        && let Some(five) = &d.five_hour
                    {
                        anchors.extend(five.anchors.iter().map(|(reset, _)| *reset));
                        days.push((row.t, end));
                    }
                    None
                }
                Said::Close(_) | Said::Plan(_) | Said::Event(_) => None,
            };
            if let Some((lo, hi, anchor)) = read {
                seen.push((lo, hi));
                anchors.insert(anchor);
            }
        }
    }
    let finer = Held::of(BTreeSet::new(), seen);
    let mut seen = finer.seen.clone();
    seen.extend(days.into_iter().filter(|(lo, hi)| !finer.meets(*lo, *hi)));
    Held::of(anchors, seen)
}

/// THE CLOSES `walls` BOOK TO ONE ACCOUNT, whose series rows are `files` (each file's rows of the
/// account, by `t`), at `now`. A wall is booked only after `another_seen`, the newest instant
/// another account was seen on the home (the spend attribution's rule,
/// [`super::account_stats::another_seen`], with the series' own partings,
/// [`account_series::taken_over`]), and after [`Walls::unsure_through`] (before a `/login`, in
/// another login's conversation, or in what a cut read did not reach, the home may have served
/// another login); only of a window the series holds no word of ([`held`]: no reading inside it,
/// no anchor of it, no day it stands alone on), which the series' own close would speak for
/// otherwise; only inside its own window and by `now`. The walls of one window and reset are one
/// close (F6), from its first wall to its last, full (`peak` and `end` 1000, `walled`), `n` its
/// walls, with no resets used recorded; it ended at its reset, or `early` at the first wall of
/// the next anchor booked where that came first, and only where it has ended
/// ([`account_series::CLOSE_SLACK_S`] after its reset, as a derived close). Each says
/// [`account_series::SRC_BACKFILL`] and carries `keys`, the account's lab key and account key,
/// and `by`, this machine. Sorted by `t`.
#[must_use]
pub fn book(
    walls: &Walls,
    files: &[Vec<SeriesRow>],
    another_seen: Option<i64>,
    (keys, by): ((Option<&str>, Option<AccountKey>), &str),
    now: i64,
) -> Vec<SeriesRow> {
    let after = another_seen.max(walls.unsure_through);
    let mut out = Vec::new();
    for (w, len) in [
        (SESSION_LIMIT, SESSION_WINDOW_S),
        (WEEK_LIMIT, WEEK_WINDOW_S),
    ] {
        let held = held(files, w);
        // Each anchor's first wall, last wall and walls.
        let mut by_reset: BTreeMap<i64, (i64, i64, u32)> = BTreeMap::new();
        for wall in walls.walls.iter().filter(|wall| {
            wall.w == w
                && after.is_none_or(|t| wall.at > t)
                && wall.at >= wall.reset.saturating_sub(len)
                && wall.at <= wall.reset
                && wall.at <= now
                && !held.holds(wall.reset, len)
        }) {
            let (first, last, n) = by_reset.entry(wall.reset).or_insert((wall.at, wall.at, 0));
            *first = (*first).min(wall.at);
            *last = (*last).max(wall.at);
            *n = n.saturating_add(1);
        }
        let booked: Vec<(i64, (i64, i64, u32))> = by_reset.into_iter().collect();
        for (i, (reset, (first, last, n))) in booked.iter().copied().enumerate() {
            let newer = booked.get(i + 1).map(|(_, (next, _, _))| *next);
            if newer.is_none() && now < reset.saturating_add(account_series::CLOSE_SLACK_S) {
                continue;
            }
            let early = newer.is_some_and(|at| at < reset);
            let ended = newer.map_or(reset, |at| at.min(reset)).max(last);
            out.push(SeriesRow {
                t: ended,
                by: by.to_owned(),
                said: Said::Close(Close {
                    w,
                    reset,
                    from: reset.saturating_sub(len),
                    peak: FULL_X10,
                    end: FULL_X10,
                    first,
                    last,
                    n,
                    walled: true,
                    epochs: Vec::new(),
                    early,
                    src: account_series::SRC_BACKFILL.into(),
                    ac: keys.0.map(str::to_owned),
                    key: keys.1,
                }),
            });
        }
    }
    out.sort_by_key(|row| row.t);
    out
}

#[cfg(test)]
#[path = "account_backfill_tests.rs"]
mod tests;
