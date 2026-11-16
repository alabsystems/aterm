// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `sys disk held`: who holds home's volume, and what aterm's own passes do with each holding
//! by themselves. A record, not a request: it decides nothing, moves nothing and asks nothing
//! of anyone. The host gathers the rows ([`Holdings`]) from what it already holds; this module
//! balances them against the volume's used bytes ([`balance`]) and prints them as lines and as
//! JSON.

use super::json::Jv;
use super::view::{self, Line};
use super::{Ctx, VolumeFacts, exe_word, pct, under};
use crate::model::{Snapshot, VolumeSample};
use crate::words;

/// Whether the session that made a holding is at work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// A session aterm's table holds claims it.
    AtWork,
    /// Sessions claim it, and aterm's table holds none of them now: each has closed. A session
    /// the table still holds is at work however long it has been idle.
    Gone,
    /// No session claims it.
    NoSession,
}

impl Mark {
    /// The word `session=` prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::AtWork => "at-work",
            Self::Gone => "gone",
            Self::NoSession => "none",
        }
    }
}

/// What a holding would cost to have again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// A build rebuilds it.
    Rebuildable,
    /// It can be had again, at a cost a build does not pay back quickly.
    Costly,
    /// Someone's work or aterm's own state: kept.
    Kept,
    /// What no row names.
    Unclassified,
}

impl Class {
    /// The word `class=` prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Rebuildable => "rebuildable",
            Self::Costly => "costly",
            Self::Kept => "kept",
            Self::Unclassified => "unclassified",
        }
    }
}

/// What kind of folder a holding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Worktree {
    /// A repository's main checkout.
    Checkout,
    /// A linked git worktree.
    Linked,
    /// A folder no repository holds.
    Folder,
    /// A project's build output: its build mirror, a `target` in it or linked out of it.
    Build,
    /// One of aterm's build caches (the shared one, or the root it moved from), less the build
    /// output rows of its own.
    Cache,
    /// aterm's tooling cache: the pinned tools its checks provision.
    Tooling,
    /// aterm's own folders, less the cache rows of their own: its state, and in its caches
    /// folder what no cache row holds (the disk reserve, virtual machines, …).
    Aterm,
    /// A `[sys] disk_roots` opt-in.
    OptIn,
    /// What no row holds.
    Other,
}

impl Worktree {
    /// The word `worktree=` prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Checkout => "checkout",
            Self::Linked => "linked",
            Self::Folder => "folder",
            Self::Build => "build",
            Self::Cache => "cache",
            Self::Tooling => "tooling",
            Self::Aterm => "aterm",
            Self::OptIn => "opt-in",
            Self::Other => "unclassified",
        }
    }
}

/// What the next of aterm's own passes does with a holding by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// No aterm pass removes it.
    Nothing,
    /// It is not aterm's: an opt-in aterm only measures.
    NotAterms,
    /// A pass would take it, and aterm's disk cleanup is off (`disk.auto_free_gib = 0`, or an
    /// aterm.toml that cannot be read).
    CleanupOff,
    /// The low-disk tier pass takes it, oldest-idle first.
    TierPass,
    /// The daily build-cache pass takes its parts unwritten for `stale_days`, and a worktree's
    /// whole build output once that worktree is gone.
    DailyCache {
        /// The days a part stays unwritten before the pass takes it.
        stale_days: u64,
    },
    /// The daily build-cache pass takes each entry in it whole once nothing used it for
    /// `after_s` seconds.
    DailyUnused {
        /// The seconds an entry stays unused before the pass takes it.
        after_s: u64,
    },
    /// A verify run that ends in its build directory, where an earlier one kept its record of
    /// the units it used, trims the units the runs stopped using; and, where the daily pass
    /// runs, that pass takes parts unwritten for `stale_days`.
    RunEnd {
        /// The daily pass's days, where it runs.
        stale_days: Option<u64>,
    },
    /// No walk measured it, so no pass can be named for it.
    Unknown,
}

impl Next {
    /// The word `pass=` prints: the pass that reaches it first.
    #[must_use]
    pub fn pass(self) -> &'static str {
        match self {
            Self::Nothing | Self::NotAterms | Self::CleanupOff => "none",
            Self::TierPass => "tier",
            Self::DailyCache { .. } | Self::DailyUnused { .. } => "daily-cache",
            Self::RunEnd { .. } => "run-end",
            Self::Unknown => "unknown",
        }
    }

    /// The days the daily pass waits, where this names it.
    #[must_use]
    pub fn stale_days(self) -> Option<u64> {
        match self {
            Self::DailyCache { stale_days } => Some(stale_days),
            Self::RunEnd { stale_days } => stale_days,
            _ => None,
        }
    }

    /// The seconds an entry stays unused before the daily pass takes it, where this names that.
    #[must_use]
    pub fn unused_s(self) -> Option<u64> {
        match self {
            Self::DailyUnused { after_s } => Some(after_s),
            _ => None,
        }
    }

    /// The statement `next:` prints: what the pass does, in words, with no command in it.
    #[must_use]
    pub fn says(self) -> String {
        match self {
            Self::Nothing => "nothing, no aterm pass removes it".to_owned(),
            Self::NotAterms => "nothing, not aterm's to remove".to_owned(),
            Self::CleanupOff => "nothing, aterm's disk cleanup is off".to_owned(),
            Self::TierPass => "removed oldest-idle first when the disk runs low".to_owned(),
            Self::DailyCache { stale_days } => format!(
                "the daily pass takes parts unwritten for {}, and a worktree's whole build output \
                 once that worktree is gone",
                days(stale_days)
            ),
            Self::DailyUnused { after_s } => format!(
                "the daily pass takes each entry nothing used for {}",
                span(after_s)
            ),
            Self::RunEnd { stale_days } => {
                let mut out =
                    "a verify run that ends here takes the units its runs stopped using".to_owned();
                if let Some(d) = stale_days {
                    out.push_str(&format!(
                        "; the daily pass takes parts unwritten for {}",
                        days(d)
                    ));
                }
                out
            }
            Self::Unknown => "not known, no walk measured it".to_owned(),
        }
    }

    /// [`Self::says`] in a few words, for a column that has no room for it (the Disk lens's
    /// rows): the same statement, with no command in it.
    #[must_use]
    pub fn brief(self) -> String {
        match self {
            Self::Nothing => "nothing".to_owned(),
            Self::NotAterms => "nothing, not aterm's".to_owned(),
            Self::CleanupOff => "nothing, cleanup is off".to_owned(),
            Self::TierPass => "removed when the disk runs low".to_owned(),
            Self::DailyCache { stale_days } => {
                format!("daily pass, parts unwritten {}", days(stale_days))
            }
            Self::DailyUnused { after_s } => {
                format!("daily pass, entries unused {}", span(after_s))
            }
            Self::RunEnd { .. } => "trimmed when a verify run ends here".to_owned(),
            Self::Unknown => "not known".to_owned(),
        }
    }

    /// [`Self::brief`] in a word or two, for a column too narrow for it: when the pass comes,
    /// with no command in it. A column never cuts a statement: it draws this whole or nothing.
    #[must_use]
    pub fn short(self) -> &'static str {
        match self {
            Self::Nothing | Self::NotAterms | Self::CleanupOff => "nothing",
            Self::TierPass => "low-disk pass",
            Self::DailyCache { .. } | Self::DailyUnused { .. } => "daily pass",
            Self::RunEnd { .. } => "at a run's end",
            Self::Unknown => "not known",
        }
    }
}

fn days(n: u64) -> String {
    count(n, "day")
}

/// `secs` in the largest whole unit of days, hours and minutes that holds it.
fn span(secs: u64) -> String {
    const DAY: u64 = 24 * 60 * 60;
    if secs >= DAY && secs.is_multiple_of(DAY) {
        count(secs / DAY, "day")
    } else if secs >= 3600 && secs.is_multiple_of(3600) {
        count(secs / 3600, "hour")
    } else {
        count(secs.div_ceil(60), "minute")
    }
}

fn count(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// Where the walks the sizes stand on are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Walk {
    /// Every holding's root has a complete walk.
    Complete,
    /// A walk is queued or running for one of them: sizes are lower bounds.
    Walking,
    /// The walker runs and nothing asks it for a walk now (the Deck out of view, no plain
    /// `sys disk`), so a root no walk measured stays unmeasured.
    Idle,
    /// The walker does not run (not started yet, or its thread did not start): nothing is
    /// measured anew.
    #[default]
    Stopped,
}

impl Walk {
    /// The word `walk=` prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Walking => "walking",
            Self::Idle => "idle",
            Self::Stopped => "stopped",
        }
    }

    /// The summary's clause on it, after a ` · `; none where every walk is complete.
    #[must_use]
    pub fn clause(self) -> Option<&'static str> {
        match self {
            Self::Complete => None,
            Self::Walking => Some("still walking: sizes are lower bounds"),
            Self::Idle => Some("no walk is asked for now: each size is its last walk's"),
            Self::Stopped => Some("the disk walker is not running: no size is measured anew"),
        }
    }

    /// [`Self::clause`] in a few words, for a line that has no room for it (the Disk lens's
    /// headings and one-line forms); none where every walk is complete.
    #[must_use]
    pub fn brief(self) -> Option<&'static str> {
        match self {
            Self::Complete => None,
            Self::Walking => Some("still walking"),
            Self::Idle => Some("sizes from the last walk"),
            Self::Stopped => Some("walker not running"),
        }
    }
}

/// A holding's bytes, as the walker's last board has them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bytes {
    /// A complete walk's figure.
    Measured(u64),
    /// A floor: the largest folders a walk listed, which may leave some of it out.
    AtLeast(u64),
    /// Not measured, and why (one of [`super::UNAVAILABLE_WORDS`]).
    Unmeasured(&'static str),
}

impl Bytes {
    /// The bytes it counts toward the volume's used: 0 when not measured.
    #[must_use]
    pub fn counted(self) -> u64 {
        match self {
            Self::Measured(n) | Self::AtLeast(n) => n,
            Self::Unmeasured(_) => 0,
        }
    }
}

/// One holding: a folder on home's volume, who made it and what happens to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holding {
    /// Its project's name: the folder's last component (a build mirror's, its workspace's).
    pub project: String,
    /// What kind of folder it is.
    pub worktree: Worktree,
    /// Its path.
    pub path: String,
    /// The sessions at work (local keys) that claim it: its path prints only to a caller in one
    /// of their tabs ([`Ctx::path_ok`]).
    pub keys: Vec<u64>,
    /// Whether the session that made it is at work.
    pub mark: Mark,
    /// The tab of the session at work that claims it most nearly (its folder deepest), 1-based.
    pub tab: Option<usize>,
    /// The program whose folder it is, in that session.
    pub exe: Option<String>,
    /// Its bytes.
    pub bytes: Bytes,
    /// What it would cost to have again.
    pub class: Class,
    /// What the next pass does with it.
    pub next: Next,
}

/// `sys disk held`'s rows, gathered by the host: once per reply, and by its probe thread at its
/// own pace for the Disk lens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Holdings {
    /// One per holding, in no order ([`balance`] orders them).
    pub rows: Vec<Holding>,
    /// Where the walks the sizes stand on are.
    pub walk: Walk,
    /// Seconds since the oldest walk the figures stand on ended.
    pub oldest_walk_s: Option<u64>,
}

/// One holding as the reply prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    /// Its place, from 1, largest first.
    pub n: usize,
    /// Its index in [`Holdings::rows`].
    pub i: usize,
    /// Its bytes after the clamp: never more than the used bytes the rows above it leave.
    pub bytes: u64,
    /// The clamp cut it.
    pub clamped: bool,
}

/// The rows of a reply and what is left of the used bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Balanced {
    /// Each holding, largest first, the unmeasured last.
    pub rows: Vec<Shown>,
    /// The used bytes no row holds: the last row; `None` where used is not known.
    pub other: Option<u64>,
}

/// `rows` against the volume's `used` bytes: largest first by counted bytes, the unmeasured
/// last, ties by path; each row's bytes cut to what the rows above it leave of `used` (cloned
/// files count once per copy, and a walk older than a large free counts what is gone), and
/// the rest of `used` as the other row. So the shown bytes and the other row sum to `used`
/// exactly. With `used` unknown nothing is cut and there is no other row.
#[must_use]
pub fn balance(rows: &[Holding], used: Option<u64>) -> Balanced {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    let unmeasured = |h: &Holding| matches!(h.bytes, Bytes::Unmeasured(_));
    order.sort_by(|&a, &b| {
        let (ra, rb) = (&rows[a], &rows[b]);
        unmeasured(ra)
            .cmp(&unmeasured(rb))
            .then(rb.bytes.counted().cmp(&ra.bytes.counted()))
            .then(ra.path.cmp(&rb.path))
    });
    let mut left = used;
    let mut shown = Vec::with_capacity(order.len());
    for (at, i) in order.into_iter().enumerate() {
        let counted = rows[i].bytes.counted();
        let bytes = left.map_or(counted, |l| counted.min(l));
        if let Some(l) = left.as_mut() {
            *l -= bytes;
        }
        shown.push(Shown {
            n: at + 1,
            i,
            bytes,
            clamped: bytes < counted,
        });
    }
    Balanced {
        rows: shown,
        other: left,
    }
}

/// The volume as `words` reads it.
fn sample(v: &VolumeFacts) -> VolumeSample {
    VolumeSample {
        total: v.total,
        free: v.free,
        reserved: v.reserved,
        mount: v.mount.clone(),
    }
}

/// How fast used grew over the last hour, bytes an hour; 0 where it shrank.
fn filling_per_h(s: &Snapshot) -> Option<u64> {
    s.facts
        .disk_rates
        .rate60
        .filter(|r| r.is_finite())
        .map(|r| (r.max(0.0) * 3600.0).round() as u64)
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// The lines of `sys disk held`: the volume's space and budget, the header, one `holding` line
/// per row, the other row last, and the summary.
pub(super) fn text(ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    if !ctx.owner_class {
        lines.push(view::note(
            "held",
            "held unavailable(refused) (holdings beyond this tab are the owner's)",
        ));
        return;
    }
    let Some(h) = ctx.held else {
        lines.push(view::note("held", "held unavailable(not_sampled)"));
        return;
    };
    let volume = ctx.volume.map(sample);
    if let Some(v) = &volume {
        lines.push(view::line(
            "space",
            format!("space {}", words::disk_space_line(v)),
        ));
    }
    lines.push(view::line(
        "budget",
        format!(
            "budget {}",
            view::budget_words(ctx.volume.map(|v| v.budget))
        ),
    ));
    let used = volume.as_ref().map(VolumeSample::used);
    let b = balance(&h.rows, used);
    let rows = b.rows.len() + usize::from(b.other.is_some());
    lines.push(view::line(
        "held",
        format!(
            "held used_bytes={} total_bytes={} rows={rows} filling_bytes_per_h={} walk={} \
             oldest_walk_s={}",
            view::rec(used, "not_sampled"),
            view::rec(volume.as_ref().map(|v| v.total), "not_sampled"),
            view::rec(filling_per_h(s), "warming"),
            h.walk.word(),
            view::rec(h.oldest_walk_s, "not_sampled"),
        ),
    ));
    for shown in &b.rows {
        lines.push(view::line(
            format!("holding {}", shown.n),
            row_text(ctx, &h.rows[shown.i], shown),
        ));
    }
    if let Some(other) = b.other {
        let n = b.rows.len() + 1;
        lines.push(view::line(
            format!("holding {n}"),
            format!(
                "holding n={n} project=other worktree={} session={} bytes={other} class={} \
                 clamped=no pass={} next: {}",
                Worktree::Other.word(),
                Mark::NoSession.word(),
                Class::Unclassified.word(),
                Next::Unknown.pass(),
                Next::Unknown.says()
            ),
        ));
    }
    let measured = b
        .rows
        .iter()
        .map(|r| r.bytes)
        .fold(0u64, u64::saturating_add);
    let mut summary = format!(
        "summary measured_bytes={measured} other_bytes={}",
        view::rec(b.other, "not_sampled")
    );
    if let Some(clause) = h.walk.clause() {
        summary.push_str(" · ");
        summary.push_str(clause);
    }
    lines.push(view::note("summary", summary));
}

/// One `holding` line: its path only where the caller may read it, else where it lies.
fn row_text(ctx: &Ctx<'_>, h: &Holding, shown: &Shown) -> String {
    let mut out = format!(
        "holding n={} project={} worktree={} session={}",
        shown.n,
        pct(&h.project),
        h.worktree.word(),
        h.mark.word()
    );
    if let Some(t) = h.tab {
        out.push_str(&format!(" tab={t}"));
    }
    if let Some(exe) = h.exe.as_deref() {
        out.push_str(&format!(" exe={}", exe_word(exe)));
    }
    let bytes = match h.bytes {
        Bytes::Measured(_) => shown.bytes.to_string(),
        Bytes::AtLeast(_) => format!("\u{2265}{}", shown.bytes),
        Bytes::Unmeasured(why) => view::unavailable(why),
    };
    let place = if ctx.path_ok(&h.keys) {
        format!("path={}", pct(&h.path))
    } else {
        format!("under={}", under(&h.path, ctx.home))
    };
    out.push_str(&format!(
        " bytes={bytes} class={} clamped={} {place} pass={} next: {}",
        h.class.word(),
        yes(shown.clamped),
        h.next.pass(),
        h.next.says()
    ));
    out
}

/// `sys disk held --json`'s `held` object: the lines' figures, its rows the `holding` lines'
/// (the other row's included, with no path).
pub(super) fn json(ctx: &Ctx<'_>, s: &Snapshot) -> Jv {
    if !ctx.owner_class {
        return Jv::why("refused");
    }
    let Some(h) = ctx.held else {
        return Jv::why("not_sampled");
    };
    let used = ctx.volume.map(|v| sample(v).used());
    let b = balance(&h.rows, used);
    let mut rows: Vec<Jv> = b
        .rows
        .iter()
        .map(|shown| row_json(ctx, &h.rows[shown.i], shown))
        .collect();
    if let Some(other) = b.other {
        rows.push(Jv::obj([
            ("n", Jv::u(b.rows.len() as u64 + 1)),
            ("project", Jv::s("other")),
            ("worktree", Jv::s(Worktree::Other.word())),
            ("session", Jv::s(Mark::NoSession.word())),
            ("bytes", Jv::u(other)),
            ("clamped", Jv::Bool(false)),
            ("class", Jv::s(Class::Unclassified.word())),
            ("next", next_json(Next::Unknown)),
        ]));
    }
    Jv::obj([
        ("used_bytes", Jv::known_or(used, "not_sampled")),
        (
            "total_bytes",
            Jv::known_or(ctx.volume.map(|v| v.total), "not_sampled"),
        ),
        ("budget", view::budget_json(ctx.volume.map(|v| v.budget))),
        (
            "filling_bytes_per_h",
            Jv::known_or(filling_per_h(s), "warming"),
        ),
        ("walk", Jv::s(h.walk.word())),
        (
            "oldest_walk_s",
            Jv::known_or(h.oldest_walk_s, "not_sampled"),
        ),
        ("rows", Jv::Arr(rows)),
    ])
}

fn row_json(ctx: &Ctx<'_>, h: &Holding, shown: &Shown) -> Jv {
    let mut o = Jv::obj([
        ("n", Jv::u(shown.n as u64)),
        ("project", Jv::s(h.project.clone())),
        ("worktree", Jv::s(h.worktree.word())),
        ("session", Jv::s(h.mark.word())),
    ]);
    if let Some(t) = h.tab {
        o.set("tab", Jv::u(t as u64));
    }
    if let Some(exe) = h.exe.as_deref() {
        o.set("exe", Jv::s(exe_word(exe)));
    }
    o.set(
        "bytes",
        match h.bytes {
            Bytes::Measured(_) => Jv::u(shown.bytes),
            Bytes::AtLeast(_) => Jv::at_least(shown.bytes, "largest_folders"),
            Bytes::Unmeasured(why) => Jv::why(why),
        },
    );
    o.set("clamped", Jv::Bool(shown.clamped));
    o.set("class", Jv::s(h.class.word()));
    o.set(
        "path",
        if ctx.path_ok(&h.keys) {
            Jv::s(h.path.clone())
        } else {
            Jv::obj([("under", Jv::s(under(&h.path, ctx.home)))])
        },
    );
    o.set("next", next_json(h.next));
    o
}

fn next_json(next: Next) -> Jv {
    let mut o = Jv::obj([("pass", Jv::s(next.pass())), ("says", Jv::s(next.says()))]);
    if let Some(d) = next.stale_days() {
        o.set("stale_days", Jv::u(d));
    }
    if let Some(s) = next.unused_s() {
        o.set("unused_s", Jv::u(s));
    }
    o
}

#[cfg(test)]
#[path = "held_tests.rs"]
mod tests;
