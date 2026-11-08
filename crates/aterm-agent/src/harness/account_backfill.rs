// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account backfill (`docs/DESIGN-account-stats-2026-10-15.md` §5 "Backfill"): the usage
//! walls a Claude Code home's own transcripts record from before this Mac's readings series
//! began, booked as `close` rows ([`account_series::SRC_BACKFILL`]) that `accounts history`
//! makes in memory at each run and never writes, with the recorder on or off. The series has
//! covered everything since the outlook began writing it, the transcripts hold what came before,
//! and writing would need a transcript walk on a harness worker. Only `accounts history` reads
//! it, so no decision does (§8.2). Of each wall it keeps when its row was written, its window and
//! that window's reset, never a row's text.

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
    /// else its file's last write), and where the read was cut, the last write of the newest
    /// transcript it did not read whole (a row it did not read was written by then), or `now`
    /// where the walk itself was cut (a transcript it did not list may hold a row of any time).
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

/// `raw`, one transcript line, folded into `out`: only a line that holds `"quotaLimits"` and
/// `"rejected"`, or `Login successful`, is parsed, and of it only the projection the transcript
/// readers share ([`super::transcript::conversation`]). A wall row
/// ([`super::upgrade::wall_of_row`]: no spend or credit wall) of the five-hour window or the week
/// that names its reset adds a [`Wall`]; a `/login` that succeeded
/// ([`super::upgrade::is_login_success_row`]) raises [`Walls::unsure_through`] to its row's time,
/// else to `written`, its file's last write.
fn fold(raw: &[u8], written: i64, out: &mut Walls) {
    let Ok(line) = std::str::from_utf8(raw) else {
        return;
    };
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
        out.unsure(at.unwrap_or(written));
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
/// [`super::accounts_sync::each_bounded_line`]), into `out`, spending at most `left` bytes (what
/// it reads is taken off) and stopping at `limits`' deadline; `None` where it was read to the end
/// it had when opened, else what cut it.
fn read_one(found: &spend::Found, left: &mut u64, limits: &Limits, out: &mut Walls) -> Option<Cut> {
    let Some((file, meta)) = super::footer::open_regular_with_meta(&found.path) else {
        return Some(Cut::Walk);
    };
    let written = written(&meta);
    let mut reader = BufReader::new(Budgeted {
        inner: file.take(*left),
        limits,
    });
    let through =
        super::accounts_sync::each_bounded_line(&mut reader, super::usage::MAX_LINE_BYTES, |raw| {
            fold(raw, written, out)
        });
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
/// them, and a deadline), with the newest instant through which the home may have served another
/// login ([`Walls::unsure_through`]) and what cut the read short, at `now`.
#[must_use]
pub fn read(projects: &Path, since: i64, limits: &Limits, now: i64) -> Walls {
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
            read_one(found, &mut left, limits, &mut out)
        };
        if let Some(cut) = cut {
            out.cut = Some(cut);
            out.unsure(written(&found.meta));
            break;
        }
    }
    out
}

/// The first instant the series rows `rows` speak of window `w` (a reading of it, its close's
/// first figure, a day that holds it), and the window anchors (resets) they hold of it.
fn covered(rows: &[&SeriesRow], w: &str) -> (Option<i64>, BTreeSet<i64>) {
    let mut first: Option<i64> = None;
    let mut anchors = BTreeSet::new();
    for row in rows {
        let (at, reset) = match &row.said {
            Said::Fig { .. } => match row.fig(w) {
                Some(fig) => (row.t.min(fig.at), Some(fig.reset)),
                None => continue,
            },
            Said::Close(c) if c.w == w => (c.first, Some(c.reset)),
            Said::Day(d) if w == SESSION_LIMIT && d.five_hour.is_some() => (row.t, None),
            Said::Day(d) if w == WEEK_LIMIT => match d.seven_day {
                Some(week) => (row.t, Some(week.reset)),
                None => continue,
            },
            _ => continue,
        };
        first = Some(first.map_or(at, |t| t.min(at)));
        anchors.extend(reset);
    }
    (first, anchors)
}

/// THE CLOSES `walls` BOOK TO ONE ACCOUNT, whose series rows (every file's, [`SeriesRow`]) are
/// `rows`, at `now`. Where `another_seen` says when another account was last seen on the home
/// ([`super::account_stats::another_seen`], the spend attribution's rule), a wall is booked only
/// after it and after [`Walls::unsure_through`] (before a `/login`, or in what a cut read did not
/// reach, the home may have served another login), only before the series' first word of its
/// window ([`covered`]: the series has covered everything since), never under a window anchor
/// the series holds (which the series' own close speaks for), only inside its own window and by
/// `now`. The walls of one window and reset are one close (F6), from its first wall to its last,
/// full (`peak` and `end` 1000, `walled`), `n` its walls, with no resets used recorded; it ended
/// at its reset, or `early` at the first wall of the next anchor booked, or the series' first
/// word, where that came first, and only where it has ended ([`account_series::CLOSE_SLACK_S`]
/// after its reset, as a derived close). Each says [`account_series::SRC_BACKFILL`] and carries
/// `keys`, the account's lab key and account key, and `by`, this machine. Sorted by `t`.
#[must_use]
pub fn book<'a>(
    walls: &Walls,
    rows: impl IntoIterator<Item = &'a SeriesRow>,
    another_seen: Option<i64>,
    (keys, by): ((Option<&str>, Option<AccountKey>), &str),
    now: i64,
) -> Vec<SeriesRow> {
    let rows: Vec<&SeriesRow> = rows.into_iter().collect();
    let after = another_seen.max(walls.unsure_through);
    let mut out = Vec::new();
    for (w, len) in [
        (SESSION_LIMIT, SESSION_WINDOW_S),
        (WEEK_LIMIT, WEEK_WINDOW_S),
    ] {
        let (series_from, anchors) = covered(&rows, w);
        // Each anchor's first wall, last wall and walls.
        let mut by_reset: BTreeMap<i64, (i64, i64, u32)> = BTreeMap::new();
        for wall in walls.walls.iter().filter(|wall| {
            wall.w == w
                && after.is_none_or(|t| wall.at > t)
                && series_from.is_none_or(|t| wall.at < t)
                && !anchors.contains(&wall.reset)
                && wall.at >= wall.reset.saturating_sub(len)
                && wall.at <= wall.reset
                && wall.at <= now
        }) {
            let (first, last, n) = by_reset.entry(wall.reset).or_insert((wall.at, wall.at, 0));
            *first = (*first).min(wall.at);
            *last = (*last).max(wall.at);
            *n = n.saturating_add(1);
        }
        let booked: Vec<(i64, (i64, i64, u32))> = by_reset.into_iter().collect();
        for (i, (reset, (first, last, n))) in booked.iter().copied().enumerate() {
            let newer = booked
                .get(i + 1)
                .map(|(_, (next, _, _))| *next)
                .or(series_from);
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
