// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Disk Custody band: a fixed band of the System Resources band, under the Deck's rows and
//! over Network State, that shows at a glance how aterm manages and tracks the disk
//! (`docs/DESIGN-disk-custody-2026-11-12.md` §4; the owner, 2026-11-15: "I need to see the new
//! disk management and tracking UX in the system resources UX on the top of aterm. it's a new
//! panel").
//!
//! It reads only what the Snapshot carries for the Disk lens, and draws it with the lens's own
//! pen and drawing functions, so the band and the lens never disagree:
//!
//! - **Space**: Space now's bar ([`super::draw_space_bar`]): rebuildable solid, costly hatched,
//!   kept grey, the warn, clean and emergency ticks and the cap's caret; its words under it.
//! - **3 weeks**: the Filling chart's three-week columns ([`super::weeks_trace_of`]), small, with
//!   each removal the journal holds as a tick under them and the range they span in words.
//! - **What aterm did**: what aterm freed today and the newest deed of the disk journal, in the
//!   words What the software did lists ([`super::deed_words`]).
//! - **Held by**: the largest holdings from the rows Held by lists ([`super::held_lines`]): the
//!   session's mark, the project, the program and the size.
//! - **Next pass**: the build output DC-04's budget sized against its target, and what the
//!   budget's pass does now: used space against the cap as the pass judges it, removal off,
//!   or a pass that found no more it may take.
//!
//! The band is a record: nothing in it is an alert, a badge or a request, and nothing in it
//! acts. A press on any part opens the Disk lens pinned, and a rest of the pointer the hover
//! lens, as a press or a rest on the Deck's Disk row does ([`HitKind::DiskCustody`]). A figure
//! not read is drawn as not read, never as 0. At a narrower width it sheds whole parts in one
//! fixed order ([`LADDER`]) and never draws one text over another or past its part.

use aterm_vitals::model::{DeedBy, DiskDeed, ProcScope, Resource, Snapshot, VolumeSample};
use aterm_vitals::sys::{BudgetRead, held};
use aterm_vitals::words;

use super::{
    ClassSpans, Ctx, DiskSpan, EPS, HELD_OLD_WALK_S, HELD_STALE_S, HeldCells, HeldLine, HeldShown,
    NOTE_PT, Pen, Trace, WEEK_MERGE, deed_words, draw_columns, draw_held_mark, draw_removal_ticks,
    draw_space_bar, draw_unread_runs, gb, held_lines, level_lines, volume_spans, watched,
    weeks_said, weeks_trace, weeks_trace_of,
};
use crate::deck_face::FaceMetrics;
use crate::stats_deck::{DeckGeom, DeckInput};
use crate::stats_paint::{Bar, Hit, HitKind, Painter, panel};
use crate::widget::{DrawPrim, rgba};

/// The band's fixed height, pt: two lines, as Network State's. It holds no state, width or
/// reading of its own, so the rows a tab's program gets never move with what it shows.
pub(crate) const HEIGHT_PT: f64 = 30.0;

/// The band's height in device px at `scale`.
pub(crate) fn height_px(scale: f64) -> usize {
    (HEIGHT_PT * scale).round().max(0.0) as usize
}

/// The band's name, its label at the left of line 1.
pub(crate) const NAME: &str = "Disk Custody";

/// Where the label starts, where the parts start after it (Network State's cell column), the
/// room between two parts and the right margin, pt.
const LEFT: f64 = 12.0;
const PARTS_X: f64 = 100.0;
const GAP: f64 = 16.0;
const RIGHT: f64 = 12.0;
/// The two lines' baselines from the band's top, pt.
const BASE1: f64 = 11.0;
const BASE2: f64 = 24.0;
/// Space now's bar on line 1: its top and height, pt, and its widths, widest first.
const BAR_Y: f64 = 3.5;
const BAR_H: f64 = 7.0;
const BAR_W: [f64; 3] = [160.0, 120.0, 88.0];
/// The three-week columns on line 1: their top and foot, pt, the removal ticks' foot, and the
/// strip's widths, widest first.
const STRIP_Y0: f64 = 2.5;
const STRIP_Y1: f64 = 13.0;
const TICK_Y1: f64 = 15.5;
const STRIP_W: [f64; 2] = [120.0, 84.0];
/// The three-week chart's columns merged into one of the band's: 168 columns of 3 hours become
/// 84 of 6, a point or more each at the narrower width, so no column is under a device px.
const STRIP_MERGE: usize = 2;
/// A sizing older than this, s (two of the budget's hourly sizings), says its age beside its
/// figures.
const SIZED_STALE_S: u64 = 2 * 3600;
/// The most of What aterm did each form takes, pt: its words are cut to it with "…".
const DID_W: [f64; 2] = [280.0, 210.0];
/// Held by: the most holdings it considers, a project's most width, pt, the room of the
/// session's mark, and the room between two of its columns.
const HELD_MOST: usize = 8;
const HELD_PROJECT_W: f64 = 110.0;
const HELD_MARK: f64 = 10.0;
const HELD_COL_GAP: f64 = 12.0;

/// One part of the band, in the order it lays them left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Part {
    /// Space now's bar and words.
    Space,
    /// The used space over three weeks, removals marked.
    Weeks,
    /// What aterm freed today and the journal's newest deed.
    Did,
    /// The largest holdings.
    Held,
    /// The budget's next pass.
    Next,
    /// The band as a whole: its label and the room no part takes.
    Band,
}

impl Part {
    /// Its name to a reader, after the band's.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Space => "space",
            Self::Weeks => "used over 3 weeks",
            Self::Did => "what aterm did",
            Self::Held => "held by",
            Self::Next => "next pass",
            Self::Band => "the band",
        }
    }
}

/// A part drawn as words on its two lines: its forms, widest first, each `(line 1, line 2)`
/// (either may be empty), how many bytes at the end of each line a cut keeps whole (a deed's
/// bytes and age: the words before them are cut first; none where a form has no entry), what it
/// says to a reader, and whether both lines are quiet (the secondary words' ink): a heading over
/// words that say nothing is read yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lines {
    pub(crate) forms: Vec<(String, String)>,
    pub(crate) tails: Vec<(usize, usize)>,
    pub(crate) said: String,
    pub(crate) quiet: bool,
}

impl Lines {
    /// `heading` over "not read yet", both quiet.
    fn unread(heading: &str) -> Self {
        Self::quiet(
            heading,
            "not read yet",
            &format!("{heading}: not read yet."),
        )
    }

    /// `heading` over `words`, both quiet, said as `said`.
    fn quiet(heading: &str, words: &str, said: &str) -> Self {
        Self {
            forms: vec![(heading.to_owned(), words.to_owned())],
            tails: Vec::new(),
            said: said.to_owned(),
            quiet: true,
        }
    }
}

/// What Space now's bar draws ([`draw_space_bar`]): the volume as read, its used bytes by
/// class, the level lines and the budget's cap.
#[derive(Clone, Debug, PartialEq)]
struct SpaceBar {
    v: VolumeSample,
    spans: Option<ClassSpans>,
    lines: Vec<(&'static str, u64, bool)>,
    cap: Option<u64>,
}

/// Space now in the band.
#[derive(Clone, Debug, PartialEq)]
struct SpaceDoc {
    /// The bar; `None` before the volume is read, which draws the bar hollow.
    bar: Option<SpaceBar>,
    /// Its words on line 2, widest first.
    pub(crate) words: Vec<String>,
    pub(crate) said: String,
}

/// The used space over three weeks in the band.
pub(crate) struct WeeksDoc {
    /// The chart's columns merged for the band ([`STRIP_MERGE`]) and the axis they are drawn on,
    /// GB; `None` where none is read.
    trace: Option<(Trace, (f64, f64))>,
    /// Line 2's caption, widest first; line 1's words where no column is read.
    pub(crate) words: Vec<String>,
    pub(crate) said: String,
}

/// One holding in the band: its session's mark ([`HeldCells::mark`]), its project, its kind of
/// folder and the folder its folder is in (which tell it apart from a holding the band draws
/// beside it by the same project's name: [`tells`]), the program at work in it and its size as
/// Held by draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Holder {
    pub(crate) mark: Option<(bool, bool)>,
    pub(crate) project: String,
    pub(crate) worktree: held::Worktree,
    /// Its kind of folder in the band's words: the kind's word, and for aterm's own folders
    /// (its state and what is left of its caches folder, one kind) the folder they are in.
    pub(crate) folder: String,
    pub(crate) parent: String,
    pub(crate) exe: Option<String>,
    pub(crate) size: String,
}

/// What tells each of `drawn` apart from another of them by the same project's name (aterm's own
/// state, its cache and its tooling are all `aterm`): its kind of folder, or where the kind is
/// shared too the folder its folder is in; `None` where no other drawn holding shares its name.
/// Read over the holdings a width draws, so a holding shed at that width tells nothing apart.
pub(crate) fn tells(drawn: &[Holder]) -> Vec<Option<String>> {
    drawn
        .iter()
        .map(|h| {
            let mates: Vec<&Holder> = drawn.iter().filter(|o| o.project == h.project).collect();
            let same_kind = mates.iter().filter(|o| o.worktree == h.worktree).count() > 1;
            if mates.len() < 2 {
                None
            } else if same_kind && !h.parent.is_empty() {
                Some(h.parent.clone())
            } else {
                Some(h.folder.clone())
            }
        })
        .collect()
}

/// Held by in the band: the holdings largest first, or the words that stand for them where
/// there are none to draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeldDoc {
    pub(crate) holders: Vec<Holder>,
    /// Line 1 and line 2 where no holding is drawn: not gathered yet, or none named.
    pub(crate) none: Option<(String, String)>,
    pub(crate) said: String,
}

/// What the band shows, projected from a Snapshot off the paint's layout.
pub(crate) struct Doc {
    space: SpaceDoc,
    pub(crate) weeks: WeeksDoc,
    pub(crate) did: Lines,
    pub(crate) held: HeldDoc,
    pub(crate) next: Lines,
}

/// The band's projection of `snap` for the front tab's sessions `panes`, its times in `loc`.
fn doc(c: Ctx<'_>) -> Doc {
    Doc {
        space: space_doc(c),
        weeks: weeks_doc(c),
        did: did_doc(c),
        held: held_doc(c),
        next: next_doc(c.snap),
    }
}

/// Space now: the bar as the lens draws it, and its words, widest first: the space and the cap,
/// the space, used and free, used.
fn space_doc(c: Ctx<'_>) -> SpaceDoc {
    let snap = c.snap;
    let Some(v) = snap.volume.clone().filter(|v| v.total > 0) else {
        return SpaceDoc {
            bar: None,
            words: vec!["not read yet".into()],
            said: "Space: not read yet.".into(),
        };
    };
    let d = watched(snap);
    let space =
        words::space_line(Resource::Disk, snap).unwrap_or_else(|| words::disk_space_line(&v));
    let lines = level_lines(d);
    let (spans, walk) = volume_spans(snap, &v, c.panes);
    let cap = d.and_then(aterm_vitals::model::DiskFacts::cap);
    // Widest first: every figure; used, free and the cap; used and free; used. The total is
    // the first to go (the Deck's Disk row and its header say it), the cap the next.
    let pieces: Vec<&str> = space.split(" \u{b7} ").collect();
    let used_free = pieces[..pieces.len().min(2)].join(" \u{b7} ");
    let mut forms = Vec::with_capacity(4);
    match cap {
        Some(cap) => {
            let cap = format!("cap {} used", gb(cap as f64));
            forms.push(format!("{space} \u{b7} {cap}"));
            forms.push(format!("{used_free} \u{b7} {cap}"));
        }
        None => forms.push(space.clone()),
    }
    forms.push(used_free);
    forms.push(pieces[0].to_owned());
    forms.dedup();
    let mut said = vec![format!("Space: {space}.")];
    if let Some(spans) = &spans {
        said.push(format!("{}.", spans.said()));
        said.extend(walk.clause().map(|w| format!("{w}.")));
    }
    if let Some(cap) = cap {
        said.push(format!("The budget's cap is {} used.", gb(cap as f64)));
    }
    if !lines.is_empty() {
        let at: Vec<String> = lines
            .iter()
            .map(|(w, b, _)| format!("{w} at {} free", gb(*b as f64)))
            .collect();
        said.push(format!("Lines: {}.", at.join(", ")));
    }
    SpaceDoc {
        bar: Some(SpaceBar {
            v,
            spans,
            lines,
            cap,
        }),
        words: forms,
        said: said.join(" "),
    }
}

/// The used space over three weeks: the Filling chart's hours merged [`STRIP_MERGE`] of its
/// columns to one of the band's ([`weeks_trace_of`]), on an axis of their own whose range the
/// caption says (a 1 GB rise fills the strip's height, so the strip says what that height is);
/// said as the lens's chart is ([`weeks_said`]).
fn weeks_doc(c: Ctx<'_>) -> WeeksDoc {
    let (lens, start) = weeks_trace(c);
    let said = weeks_said(c, &lens, start);
    let (trace, _) = weeks_trace_of(c, WEEK_MERGE * STRIP_MERGE);
    let read: Vec<f64> = trace
        .vals
        .iter()
        .chain(&trace.peaks)
        .flatten()
        .copied()
        .collect();
    let Some(&first) = read.first() else {
        return WeeksDoc {
            trace: None,
            words: vec!["3 weeks: not read yet".into(), "not read yet".into()],
            said,
        };
    };
    let (mn, mx) = read
        .iter()
        .fold((first, first), |(a, b), &v| (a.min(v), b.max(v)));
    let (lo, step) = super::axis(mn, mx, 2, trace.min_span);
    let hi = lo + 2.0 * step;
    let dec = super::decimals(step).max(super::decimals(lo));
    let range = format!("{lo:.dec$}\u{2013}{hi:.dec$} GB");
    WeeksDoc {
        trace: Some((trace, (lo, hi))),
        // The narrow form is the range alone, so the strip it captions outlasts the words of
        // What aterm did and Next pass: its meter and the wide form say what it spans.
        words: vec![format!("used {range}, last 3 weeks"), range],
        said: format!("{said} The band's strip spans {lo:.dec$} to {hi:.dec$} GB used."),
    }
}

/// `ms` as an age: `14 min ago`.
fn ago(ms: i64) -> String {
    format!(
        "{} ago",
        words::duration(u64::try_from(ms.max(0)).unwrap_or(0))
    )
}

/// A deed in the band's words: its subject (`aterm`, the tab, `a tab`), what it did, how many
/// rows it stands for, its bytes and how long ago, widest first; and how many bytes at the end
/// of each are its bytes and age, which a cut keeps whole.
fn deed_forms(c: Ctx<'_>, d: &DiskDeed) -> (String, String, usize) {
    let who = match &d.by {
        DeedBy::Aterm => "aterm".to_owned(),
        DeedBy::Tab(Some(k)) => c.scope_word(ProcScope::Tab(*k)),
        DeedBy::Tab(None) => "a tab".to_owned(),
    };
    let what = deed_words(&d.kind, &d.by);
    let times = if d.n > 1 {
        format!(" \u{d7}{}", d.n)
    } else {
        String::new()
    };
    let figure = match (d.bytes.filter(|&b| b > 0), d.free) {
        (Some(b), _) => format!(" {}", gb(b as f64)),
        (None, Some(free)) => format!(", {} free", gb(free as f64)),
        (None, None) => String::new(),
    };
    let age = ago(c.snap.at_unix_ms.saturating_sub(d.at_unix_ms));
    let tail = format!("{figure} \u{b7} {age}");
    (
        format!("{who} {what}{times}{tail}"),
        format!("{what}{tail}"),
        tail.len(),
    )
}

/// What aterm did: line 1 what it freed today, line 2 the journal's newest deed; with nothing
/// freed today the newest deed on line 1 and how many more the Disk lens lists on line 2. A
/// journal not read yet is not an empty one: its deed reads not read.
fn did_doc(c: Ctx<'_>) -> Lines {
    const HEAD: &str = "What aterm did";
    let snap = c.snap;
    // Before the host's first disk facts the journal is not read: that is not an empty one.
    let Some(d) = snap.facts.disk.as_ref() else {
        return Lines::unread(HEAD);
    };
    let freed = d.freed_today.as_ref().map(|(b, ..)| gb(*b as f64));
    let newest = d.deeds.iter().max_by_key(|deed| deed.at_unix_ms);
    let more = d.deeds.len().saturating_sub(1);
    let deed = newest.map(|deed| deed_forms(c, deed));
    let mut said = Vec::new();
    if let Some(f) = &freed {
        said.push(format!("aterm freed {f} today."));
    }
    if let Some((long, ..)) = &deed {
        said.push(format!("Newest in the disk journal: {long}."));
    }
    if more > 0 {
        said.push(format!(
            "The Disk lens lists {more} more of the last 3 weeks."
        ));
    }
    let forms = match (freed, deed) {
        (Some(f), Some((long, short, tail))) => Lines {
            forms: vec![
                (format!("aterm freed {f} today"), long),
                (format!("freed {f} today"), short),
            ],
            tails: vec![(0, tail), (0, tail)],
            said: String::new(),
            quiet: false,
        },
        (Some(f), None) => {
            let journal = if d.journal_read {
                ""
            } else {
                said.push("The disk journal is not read yet.".into());
                "journal not read yet"
            };
            Lines {
                forms: vec![
                    (format!("aterm freed {f} today"), journal.to_owned()),
                    (format!("freed {f} today"), journal.to_owned()),
                ],
                tails: Vec::new(),
                said: String::new(),
                quiet: false,
            }
        }
        (None, Some((long, short, tail))) => {
            let (m_long, m_short) = match more {
                0 => (String::new(), String::new()),
                n => (format!("{n} more in the Disk lens"), format!("{n} more")),
            };
            Lines {
                forms: vec![(long, m_long), (short, m_short)],
                tails: vec![(tail, 0), (tail, 0)],
                said: String::new(),
                quiet: false,
            }
        }
        (None, None) if !d.journal_read => return Lines::unread(HEAD),
        (None, None) => {
            said.push("The disk journal holds nothing of the last 3 weeks.".into());
            Lines {
                forms: vec![(HEAD.to_owned(), "nothing in the last 3 weeks".into())],
                tails: Vec::new(),
                said: String::new(),
                quiet: false,
            }
        }
    };
    Lines {
        said: said.join(" "),
        ..forms
    }
}

/// Held by: the largest holdings, as the lens lists them, less the line for the bytes no
/// holding names.
fn held_doc(c: Ctx<'_>) -> HeldDoc {
    let snap = c.snap;
    let Some(h) = snap.facts.disk.as_ref().and_then(|d| d.held.as_deref()) else {
        return HeldDoc {
            holders: Vec::new(),
            none: Some(("Held by".into(), "not read yet".into())),
            said: "Held by: not read yet.".into(),
        };
    };
    let used = snap
        .volume
        .as_ref()
        .map(aterm_vitals::model::VolumeSample::used);
    let named: Vec<HeldLine> = held_lines(h, used, c.panes)
        .into_iter()
        .filter(|l| !l.is_other())
        .collect();
    if named.is_empty() {
        return HeldDoc {
            holders: Vec::new(),
            none: Some(("Held by".into(), "no holding named".into())),
            said: "Held by: no holding named.".into(),
        };
    }
    // At a glance the band lists who holds the most: a holding whose walk has not sized it yet
    // holds no figure to rank, and the lens lists it.
    let lines: Vec<HeldLine> = named
        .into_iter()
        .filter(|l| l.unread.is_none() && l.bytes > 0)
        .collect();
    if lines.is_empty() {
        return HeldDoc {
            holders: Vec::new(),
            none: Some(("Held by".into(), "not sized yet".into())),
            said: "Held by: no holding is sized yet; the Disk lens lists them.".into(),
        };
    }
    let said: Vec<String> = lines
        .iter()
        .map(|l| {
            format!(
                "{} {}, {}, {}",
                l.project,
                l.worktree.word(),
                l.session_words(),
                l.size().said()
            )
        })
        .collect();
    let mut said = format!("Held by, largest first: {}.", said.join("; "));
    if let Some(walk) = h.walk.clause() {
        said.push(' ');
        said.push_str(&super::sentence(walk));
    }
    // The rows' age, as the lens says it: the band reads rows gathered at the host's own pace,
    // and sizes from walks that ended earlier still.
    let dur = |s: u64| words::duration(s.saturating_mul(1000));
    if let Some(age) = snap
        .facts
        .disk
        .as_ref()
        .and_then(|d| d.held_age_s)
        .filter(|&age| age > HELD_STALE_S)
    {
        said.push_str(&format!(" As of {} ago.", dur(age)));
    }
    if let Some(walk) = h.oldest_walk_s.filter(|&s| s >= HELD_OLD_WALK_S) {
        said.push_str(&format!(" Sizes up to {} old.", dur(walk)));
    }
    let shown = &lines[..lines.len().min(HELD_MOST)];
    let holders = shown
        .iter()
        .map(|l| {
            let cells = HeldCells::of(&HeldShown::Line(l.clone()));
            // aterm's own folders are `aterm` by project and one kind by kind: its state and
            // what is left of its caches folder; the folder each is in says which.
            let folder = match (l.worktree, l.parent.is_empty()) {
                (held::Worktree::Aterm, false) => l.parent.clone(),
                (held::Worktree::Aterm, true) => "aterm's own".to_owned(),
                (w, _) => w.word().to_owned(),
            };
            Holder {
                mark: cells.mark,
                project: cells.project,
                worktree: l.worktree,
                folder,
                parent: l.parent.clone(),
                exe: (l.mark == held::Mark::AtWork)
                    .then(|| l.exe.clone())
                    .flatten(),
                size: cells.size,
            }
        })
        .collect();
    HeldDoc {
        holders,
        none: None,
        said,
    }
}

/// What the budget's pass does now (`docs/DESIGN-disk-custody-2026-11-12.md` §3.2), as it
/// judges it: line 1 the build output the budget sized against its target (floors, `≥`, where
/// the sizing was cut short, and its age where it is older than [`SIZED_STALE_S`]), line 2 the
/// used space against the cap, measured now, and what the pass does about it: nothing under the
/// cap; over it, nothing where removal is off or nothing rebuildable is sized, nothing more where
/// its last pass found no more it may take and its memo holds, else it removes build output no
/// live session uses. A budget not sized reads as not read, and a process with no disk watch says
/// it runs none. It speaks of the budget's pass alone: the low-disk, daily and run-end passes
/// keep their own rules.
fn next_doc(snap: &Snapshot) -> Lines {
    const HEAD: &str = "Next pass";
    if snap.facts.disk.is_none() {
        return Lines::unread(HEAD);
    }
    let Some(d) = watched(snap) else {
        return Lines::quiet(
            HEAD,
            "no disk watch in this aterm",
            "Next pass: this aterm runs no disk watch, so no pass of its own runs.",
        );
    };
    let b = match d.budget {
        BudgetRead::Sized(b) => b,
        BudgetRead::Warming => {
            return Lines::quiet(
                HEAD,
                "budget not sized yet",
                "Next pass: the budget is not sized yet.",
            );
        }
        BudgetRead::NotSampled => {
            return Lines::quiet(
                HEAD,
                "budget not sized on this volume",
                "Next pass: the budget is not sized on this volume.",
            );
        }
        BudgetRead::Off => {
            return Lines {
                forms: vec![(HEAD.into(), "budget off (disk.cap_used_gib)".into())],
                tails: Vec::new(),
                said: "Next pass: the budget is off: disk.cap_used_gib is 0 or not a whole \
                       number, or aterm.toml cannot be read."
                    .into(),
                quiet: false,
            };
        }
    };
    let least = if b.partial { "\u{2265} " } else { "" };
    let (r, t) = (gb(b.rebuildable as f64), gb(b.target as f64));
    let short = format!("build output {least}{r} of {least}{t}");
    let mut figures = format!("{short} budget");
    let mut said = vec![format!(
        "The budget sized {least}{r} of build output against a target of {least}{t}."
    )];
    if b.partial {
        said.push("The sizing was cut short, so those figures are lower bounds.".into());
    }
    if b.sized_s > SIZED_STALE_S {
        let age = words::duration(b.sized_s.saturating_mul(1000));
        figures.push_str(&format!(" \u{b7} sized {age} ago"));
        said.push(format!("It was sized {age} ago."));
    }
    // The pass's own test: used space now against the cap; the sizing's figures where the
    // volume is not read.
    let used = snap
        .volume
        .as_ref()
        .filter(|v| v.total > 0)
        .map(VolumeSample::used);
    let (over, under) = match used {
        Some(u) => (u.saturating_sub(b.cap), b.cap.saturating_sub(u)),
        None => (
            b.rebuildable.saturating_sub(b.target),
            b.target.saturating_sub(b.rebuildable),
        ),
    };
    let cap = gb(b.cap as f64);
    let (long2, short2, verdict) = if over == 0 {
        let u = gb(under as f64);
        (
            format!("under the cap by {u}: the budget's pass takes nothing"),
            "under the cap: takes nothing".to_owned(),
            format!("Used space is {u} under the {cap} cap, so the budget's pass takes nothing."),
        )
    } else {
        let o = gb(over as f64);
        let now_s = snap.at_unix_ms.div_euclid(1000);
        if b.rebuildable == 0 {
            (
                format!("over the cap by {o}: no build output is sized to take"),
                format!("over by {o}: none sized"),
                format!(
                    "Used space is {o} over the {cap} cap; the budget sized no build output, \
                     so its pass has nothing to take."
                ),
            )
        } else if d.cleanup == Some(false) {
            (
                format!("over the cap by {o} \u{b7} cleanup off (disk.auto_free_gib = 0)"),
                format!("over by {o} \u{b7} cleanup off"),
                format!(
                    "Used space is {o} over the {cap} cap, and removal is off \
                     (disk.auto_free_gib is 0, or aterm.toml cannot be read): no pass removes \
                     anything."
                ),
            )
        } else if let Some(at) = d.budget_unmet_unix {
            let age = words::duration(u64::try_from(now_s - at).unwrap_or(0).saturating_mul(1000));
            (
                format!("over the cap by {o} \u{b7} the last pass found no more it may take"),
                format!("over by {o} \u{b7} no more to take"),
                format!(
                    "Used space is {o} over the {cap} cap. The budget's last pass, {age} ago, \
                     found no more it may take; it plans again when a live session ends or \
                     starts, the build output grows, or a day after that pass."
                ),
            )
        } else {
            (
                format!(
                    "over the cap by {o}: the next pass removes build output no live session uses"
                ),
                format!("over by {o}: the next pass trims"),
                format!(
                    "Used space is {o} over the {cap} cap, so the budget's next pass removes \
                     build output no live session uses."
                ),
            )
        }
    };
    said.insert(0, format!("Next pass: {verdict}"));
    Lines {
        forms: vec![(figures, long2), (short, short2)],
        tails: Vec::new(),
        said: said.join(" "),
        quiet: false,
    }
}

/// Which form of each part a width keeps: the label, Space now's bar and words (indices into
/// [`BAR_W`] and [`SpaceDoc::words`]), and each other part's form, or how many holdings, where
/// it is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Choice {
    pub(crate) label: bool,
    pub(crate) bar: usize,
    pub(crate) words: usize,
    pub(crate) weeks: Option<usize>,
    pub(crate) did: Option<usize>,
    pub(crate) held: Option<usize>,
    pub(crate) next: Option<usize>,
}

/// One step a band too wide for its room takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// One column of holdings fewer (two holdings), down to one column.
    FewerHeld,
    /// The three-week strip at its narrower width ([`STRIP_W`]) and shorter caption.
    WeeksNarrow,
    /// The three-week strip shed.
    WeeksDrop,
    /// What aterm did in its short form.
    DidShort,
    /// Space now's bar at its next narrower width ([`BAR_W`]).
    BarNarrow,
    /// Held by shed.
    HeldDrop,
    /// Next pass in its short form.
    NextShort,
    /// What aterm did shed.
    DidDrop,
    /// Space now's words at their next shorter form ([`SpaceDoc::words`]).
    WordsShorter,
    /// Next pass shed.
    NextDrop,
    /// The band's label shed: the parts start at the left margin.
    LabelDrop,
}

/// The fixed order a band too wide sheds in: the columns of holdings past the first, the
/// volume's total under Space now's bar, the long words of What aterm did and Next pass, then
/// the three-week strip (the trend outranks words), then whole parts, Space now last of all.
/// Space now's bar and its words are never shed: past the last step they are cut to the room.
pub(crate) const LADDER: [Step; 15] = [
    Step::FewerHeld,
    Step::WordsShorter,
    Step::DidShort,
    Step::NextShort,
    Step::WeeksNarrow,
    Step::WeeksDrop,
    Step::BarNarrow,
    Step::HeldDrop,
    Step::DidDrop,
    Step::WordsShorter,
    Step::BarNarrow,
    Step::NextDrop,
    Step::WordsShorter,
    Step::LabelDrop,
    Step::WordsShorter,
];

impl Choice {
    /// Every part at its widest.
    fn widest(doc: &Doc) -> Self {
        Self {
            label: true,
            bar: 0,
            words: 0,
            weeks: Some(0),
            did: Some(0),
            held: Some(doc.held.holders.len().max(1)),
            next: Some(0),
        }
    }

    /// `step` taken, within what `doc` has: a part with one form left keeps it.
    fn take(&mut self, step: Step, doc: &Doc) {
        let shorter = |i: Option<usize>, n: usize| i.map(|i| (i + 1).min(n.saturating_sub(1)));
        match step {
            Step::FewerHeld => {
                self.held = self.held.map(|k| if k > 2 { (k - 1) / 2 * 2 } else { k });
            }
            Step::WeeksNarrow => self.weeks = shorter(self.weeks, STRIP_W.len()),
            Step::WeeksDrop => self.weeks = None,
            Step::DidShort => self.did = shorter(self.did, doc.did.forms.len()),
            Step::BarNarrow => self.bar = (self.bar + 1).min(BAR_W.len() - 1),
            Step::HeldDrop => self.held = None,
            Step::NextShort => self.next = shorter(self.next, doc.next.forms.len()),
            Step::DidDrop => self.did = None,
            Step::WordsShorter => {
                self.words = (self.words + 1).min(doc.space.words.len().saturating_sub(1));
            }
            Step::NextDrop => self.next = None,
            Step::LabelDrop => self.label = false,
        }
    }
}

/// One part as laid: where it starts and how wide it is, pt from the band's left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placed {
    pub(crate) part: Part,
    pub(crate) x: f64,
    pub(crate) w: f64,
}

/// The band as a width lays it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Laid {
    pub(crate) choice: Choice,
    /// The parts it draws, left to right.
    pub(crate) parts: Vec<Placed>,
}

/// The widest of `texts` at the band's size.
fn widest<'t>(m: &FaceMetrics, texts: impl IntoIterator<Item = &'t str>) -> f64 {
    texts
        .into_iter()
        .map(|t| m.width(t, NOTE_PT, false))
        .fold(0.0, f64::max)
}

/// One holding's width: its mark's room, its project cut to [`HELD_PROJECT_W`], what tells it
/// apart (`tell`), its program and its size.
fn holder_w(m: &FaceMetrics, h: &Holder, tell: Option<&str>) -> f64 {
    let project = m.width(
        &m.fit(&[&h.project], HELD_PROJECT_W, NOTE_PT, false),
        NOTE_PT,
        false,
    );
    let quiet = |w: Option<&str>| w.map_or(0.0, |w| 4.0 + m.width(w, NOTE_PT, false));
    HELD_MARK
        + project
        + quiet(tell)
        + quiet(h.exe.as_deref())
        + 6.0
        + m.width(&h.size, NOTE_PT, false)
}

/// Held by's columns for `k` holdings, two to a column: each column's width, its holdings told
/// apart among those `k` ([`tells`]).
fn held_columns(m: &FaceMetrics, doc: &HeldDoc, k: usize) -> Vec<f64> {
    if let Some((a, b)) = &doc.none {
        return vec![widest(m, [a.as_str(), b.as_str()])];
    }
    let drawn = &doc.holders[..k.min(doc.holders.len())];
    let tells = tells(drawn);
    drawn
        .iter()
        .zip(&tells)
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|col| {
            col.iter()
                .map(|(h, t)| holder_w(m, h, t.as_deref()))
                .fold(0.0, f64::max)
        })
        .collect()
}

/// Each kept part's width under `choice`.
fn widths(m: &FaceMetrics, doc: &Doc, choice: &Choice) -> Vec<(Part, f64)> {
    let lines_w = |l: &Lines, i: usize| {
        let (a, b) = &l.forms[i.min(l.forms.len() - 1)];
        widest(m, [a.as_str(), b.as_str()])
    };
    let mut out = Vec::with_capacity(5);
    let words = &doc.space.words[choice.words.min(doc.space.words.len() - 1)];
    out.push((
        Part::Space,
        BAR_W[choice.bar].max(m.width(words, NOTE_PT, false)),
    ));
    if let Some(i) = choice.weeks {
        let caption = &doc.weeks.words[i.min(doc.weeks.words.len() - 1)];
        out.push((
            Part::Weeks,
            STRIP_W[i].max(m.width(caption, NOTE_PT, false)),
        ));
    }
    if let Some(i) = choice.did {
        out.push((
            Part::Did,
            lines_w(&doc.did, i).min(DID_W[i.min(DID_W.len() - 1)]),
        ));
    }
    if let Some(k) = choice.held {
        let cols = held_columns(m, &doc.held, k);
        let w = cols.iter().sum::<f64>() + HELD_COL_GAP * cols.len().saturating_sub(1) as f64;
        out.push((Part::Held, w));
    }
    if let Some(i) = choice.next {
        out.push((Part::Next, lines_w(&doc.next, i)));
    }
    out
}

/// Where the parts start under `choice`.
const fn first_x(choice: &Choice) -> f64 {
    if choice.label { PARTS_X } else { LEFT }
}

/// The band laid `w` pt wide: every part at its widest, then the steps of [`LADDER`] in order
/// until the parts fit between their start and the right margin; Space now, which no step
/// sheds, cut to the room that is left.
pub(crate) fn lay(m: &FaceMetrics, doc: &Doc, w: f64) -> Laid {
    let room = |choice: &Choice| (w - RIGHT - first_x(choice)).max(0.0);
    let need = |choice: &Choice| {
        let ws = widths(m, doc, choice);
        ws.iter().map(|(_, w)| w).sum::<f64>() + GAP * ws.len().saturating_sub(1) as f64
    };
    let mut choice = Choice::widest(doc);
    let mut steps = LADDER.iter().copied().peekable();
    while need(&choice) > room(&choice) + EPS {
        let Some(&step) = steps.peek() else {
            break;
        };
        // A step that sheds a column of holdings repeats down to one column.
        if step == Step::FewerHeld && choice.held.is_some_and(|k| k > 2) {
            choice.take(step, doc);
            continue;
        }
        choice.take(step, doc);
        steps.next();
    }
    let mut x = first_x(&choice);
    let end = (w - RIGHT).max(x);
    let parts = widths(m, doc, &choice)
        .into_iter()
        .map(|(part, pw)| {
            let pw = pw.min((end - x).max(0.0));
            let placed = Placed { part, x, w: pw };
            x += pw + GAP;
            placed
        })
        .collect();
    Laid { choice, parts }
}

/// The band painted: its prims and hits in the card's px, how it was laid, and in a test the
/// box of every text it drew.
pub(crate) struct Painted {
    pub(crate) prims: Vec<DrawPrim>,
    pub(crate) hits: Vec<Hit>,
    #[cfg(test)]
    pub(crate) laid: Laid,
    #[cfg(test)]
    pub(crate) boxes: Vec<super::TextBox>,
}

/// The context the band reads: `input`'s snapshot (an empty one before the first reading), its
/// front tab and its clock.
fn with_ctx<R>(input: &DeckInput<'_>, m: &FaceMetrics, f: impl FnOnce(Ctx<'_>) -> R) -> R {
    let empty;
    let snap = match input.snap {
        Some(snap) => snap,
        None => {
            empty = Snapshot::default();
            &empty
        }
    };
    f(Ctx {
        snap,
        m,
        panes: &input.tab_panes,
        tab_label: &input.tab_label,
        loc: input.locale,
        bar: Bar::Global,
        two: false,
        cw: 0.0,
        fresh: input.live.fresh,
        disk_span: DiskSpan::Weeks,
    })
}

/// The band drawn across `g`'s width from `top` (card px down), `height_px` tall, from `input`'s
/// reading, its words measured by `m`.
pub(crate) fn paint(g: &DeckGeom, top: f32, input: &DeckInput<'_>, m: &FaceMetrics) -> Painted {
    with_ctx(input, m, |c| {
        let doc = doc(c);
        paint_doc(g, top, input, m, &doc, c.panes)
    })
}

/// [`paint`] of a projection already made.
pub(crate) fn paint_doc(
    g: &DeckGeom,
    top: f32,
    input: &DeckInput<'_>,
    m: &FaceMetrics,
    doc: &Doc,
    panes: &[u64],
) -> Painted {
    let scale = g.scale.max(0.5);
    let s = f64::from(scale);
    let mut inks = input.inks;
    if input.contrast {
        inks.hollow = inks.words2;
        inks.rim = inks.words2;
    }
    let height = height_px(s) as f32;
    let mut pen = Pen {
        p: Painter::new(&inks, scale, g.font_px, input.dwc),
        m,
        inks,
        ic: input.contrast,
        s,
        ox: 0.0,
        oy: top,
        draw: true,
        line_tab: 0.0,
        #[cfg(test)]
        boxes: Vec::new(),
    };
    pen.p.prims.push(DrawPrim::ClipPush {
        x: 0.0,
        y: top,
        w: g.w,
        h: height,
    });
    pen.p
        .prims
        .push(panel(0.0, top, g.w, height, 0.0, rgba(inks.ground, 255)));
    pen.p
        .prims
        .push(panel(0.0, top, g.w, 1.0, 0.0, rgba(inks.grid, 255)));
    let laid = lay(m, doc, f64::from(g.w) / s);
    if laid.choice.label {
        pen.text(LEFT, BASE1, NAME, NOTE_PT, true, inks.words2);
    }
    let mut hits = Vec::with_capacity(laid.parts.len() + 1);
    for placed in &laid.parts {
        match placed.part {
            Part::Space => draw_space(&mut pen, placed, &laid.choice, &doc.space),
            Part::Weeks => draw_weeks(&mut pen, placed, &laid.choice, &doc.weeks),
            Part::Did => draw_lines(&mut pen, placed, &doc.did, laid.choice.did, false),
            Part::Held => draw_held(&mut pen, placed, &laid.choice, &doc.held, panes),
            Part::Next => draw_lines(&mut pen, placed, &doc.next, laid.choice.next, true),
            Part::Band => {}
        }
        hits.push(Hit {
            rect: ((placed.x * s) as f32, top, (placed.w * s) as f32, height),
            kind: HitKind::DiskCustody(placed.part),
        });
    }
    hits.push(Hit {
        rect: (0.0, top, g.w, height),
        kind: HitKind::DiskCustody(Part::Band),
    });
    pen.p.prims.push(DrawPrim::ClipPop);
    Painted {
        prims: pen.p.prims,
        hits,
        #[cfg(test)]
        laid,
        #[cfg(test)]
        boxes: pen.boxes,
    }
}

/// `s` cut to `w` with "…" at `(x, base)`.
fn text_in(pen: &mut Pen<'_>, x: f64, base: f64, s: &str, w: f64, ink: aterm_vitals::ink::Rgb) {
    let cut = pen.cut(s, w, NOTE_PT, false);
    pen.text(x, base, &cut, NOTE_PT, false, ink);
}

/// `s` at `(x, base)` cut to `w` with "…", its last `tail` bytes kept whole where they fit: the
/// words before them are cut instead.
fn text_kept(
    pen: &mut Pen<'_>,
    (x, base): (f64, f64),
    s: &str,
    tail: usize,
    w: f64,
    ink: aterm_vitals::ink::Rgb,
) {
    let split = s.len().saturating_sub(tail);
    if tail == 0 || !s.is_char_boundary(split) || pen.width(s, NOTE_PT, false) <= w + EPS {
        return text_in(pen, x, base, s, w, ink);
    }
    let (head, end) = s.split_at(split);
    let end_w = pen.width(end, NOTE_PT, false);
    if end_w >= w {
        return text_in(pen, x, base, s, w, ink);
    }
    let head = pen.cut(head, w - end_w, NOTE_PT, false);
    let head_w = pen.text(x, base, &head, NOTE_PT, false, ink);
    pen.text(x + head_w, base, end, NOTE_PT, false, ink);
}

/// Space now: the lens's bar on line 1 (hollow where the volume is not read), its words on
/// line 2.
fn draw_space(pen: &mut Pen<'_>, p: &Placed, choice: &Choice, doc: &SpaceDoc) {
    let bar_w = BAR_W[choice.bar].min(p.w);
    match &doc.bar {
        Some(bar) => draw_space_bar(
            pen,
            (p.x, BAR_Y, bar_w, BAR_H),
            &bar.v,
            bar.spans,
            &bar.lines,
            bar.cap,
        ),
        None => {
            let ink = rgba(pen.inks.hollow, 0xFF);
            pen.hollow(p.x, BAR_Y, bar_w, BAR_H, ink);
        }
    }
    let words = &doc.words[choice.words.min(doc.words.len() - 1)];
    let ink = pen.inks.words2;
    text_in(pen, p.x, BASE2, words, p.w, ink);
}

/// The used space over three weeks: the merged columns on line 1 as the lens's chart draws them
/// ([`draw_columns`]), on the axis the caption says, each unread run hollow along the foot
/// ([`draw_unread_runs`]) and each removal a tick under it ([`draw_removal_ticks`]); the caption
/// on line 2. Where no column is read, the strip's outline and that it is not read.
fn draw_weeks(pen: &mut Pen<'_>, p: &Placed, choice: &Choice, doc: &WeeksDoc) {
    let i = choice.weeks.unwrap_or(0);
    let words = &doc.words[i.min(doc.words.len() - 1)];
    let inks = pen.inks;
    let (x0, x1) = (p.x, p.x + STRIP_W[i].min(p.w));
    let Some((t, axis)) = &doc.trace else {
        // Not read: the strip's outline over no column, as Space now's bar is drawn.
        pen.hollow(
            x0,
            STRIP_Y0,
            x1 - x0,
            STRIP_Y1 - STRIP_Y0,
            rgba(inks.hollow, 0xFF),
        );
        text_in(pen, p.x, BASE2, words, p.w, inks.words2);
        return;
    };
    draw_columns(pen, t, (x0, STRIP_Y0, x1, STRIP_Y1), *axis, |_| {});
    draw_unread_runs(pen, t, (x0, x1, STRIP_Y1), 2.0);
    draw_removal_ticks(pen, t, (x0, x1), (STRIP_Y1 + 0.75, TICK_Y1));
    text_in(pen, p.x, BASE2, words, p.w, inks.words2);
}

/// A part of two lines of words, form `form`: line 1 in the words ink and line 2 in the
/// secondary one, or the other way round where line 2 is the statement (`statement`).
fn draw_lines(pen: &mut Pen<'_>, p: &Placed, doc: &Lines, form: Option<usize>, statement: bool) {
    let i = form.unwrap_or(0).min(doc.forms.len() - 1);
    let (a, b) = &doc.forms[i];
    let (ta, tb) = doc.tails.get(i).copied().unwrap_or_default();
    let (ink1, ink2) = if doc.quiet {
        (pen.inks.words2, pen.inks.words2)
    } else if statement {
        (pen.inks.words2, pen.inks.words)
    } else {
        (pen.inks.words, pen.inks.words2)
    };
    text_kept(pen, (p.x, BASE1), a, ta, p.w, ink1);
    text_kept(pen, (p.x, BASE2), b, tb, p.w, ink2);
}

/// Held by: the kept holdings two to a column, each its session's mark, its project, its kind of
/// folder where the project's name is shared and the program at work in it, its size ending the
/// column; or the words that stand for them.
fn draw_held(pen: &mut Pen<'_>, p: &Placed, choice: &Choice, doc: &HeldDoc, panes: &[u64]) {
    let inks = pen.inks;
    if let Some((a, b)) = &doc.none {
        text_in(pen, p.x, BASE1, a, p.w, inks.words2);
        text_in(pen, p.x, BASE2, b, p.w, inks.words2);
        return;
    }
    let k = choice.held.unwrap_or(1).min(doc.holders.len());
    let cols = held_columns(pen.m, doc, k);
    let drawn = &doc.holders[..k];
    let tells = tells(drawn);
    let rows: Vec<(&Holder, Option<&str>)> = drawn
        .iter()
        .zip(tells.iter().map(Option::as_deref))
        .collect();
    let mut x = p.x;
    for (col, w) in rows.chunks(2).zip(cols) {
        for (row, &(h, tell)) in col.iter().enumerate() {
            let base = if row == 0 { BASE1 } else { BASE2 };
            draw_held_mark(pen, x, base, h.mark, panes);
            let project = pen.cut(&h.project, HELD_PROJECT_W, NOTE_PT, false);
            let mut tx = x + HELD_MARK;
            tx += pen.text(tx, base, &project, NOTE_PT, false, inks.words);
            for quiet in [tell, h.exe.as_deref()].into_iter().flatten() {
                tx += 4.0;
                tx += pen.text(tx, base, quiet, NOTE_PT, false, inks.words2);
            }
            // The sizes end their column, so a column reads as a table.
            pen.text_end(x + w, base, &h.size, NOTE_PT, false, inks.words);
        }
        x += w + HELD_COL_GAP;
    }
}

/// The band's parts as a reader meets them, `(name, words)`, for those a band `w` px wide at
/// `scale` draws: the same projection and layout the paint uses.
pub(crate) fn meters(
    input: &DeckInput<'_>,
    m: &FaceMetrics,
    (w, scale): (f32, f32),
) -> Vec<(String, String)> {
    with_ctx(input, m, |c| {
        let doc = doc(c);
        let laid = lay(m, &doc, f64::from(w) / f64::from(scale.max(0.5)));
        laid.parts
            .iter()
            .map(|placed| {
                let said = match placed.part {
                    Part::Space => doc.space.said.clone(),
                    Part::Weeks => doc.weeks.said.clone(),
                    Part::Did => doc.did.said.clone(),
                    Part::Held => doc.held.said.clone(),
                    Part::Next => doc.next.said.clone(),
                    Part::Band => String::new(),
                };
                (format!("{NAME}: {}", placed.part.word()), said)
            })
            .collect()
    })
}

#[cfg(test)]
#[path = "stats_disk_band_tests.rs"]
mod tests;
