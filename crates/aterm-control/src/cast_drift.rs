// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cast drift`: a counterfactual replay audit of a session's asciicast against
//! the screen it produced.
//!
//! The failure it names: a net-zero resize run (64 -> 63 -> 64 rows within a
//! millisecond) landing on the alternate screen shifts every row up one, since
//! the shrink demotes the top row and the alt grid keeps no history, so the grow
//! appends a blank row. The engine's resize undo hands the row back when nothing
//! is output between the halves; any output, even a mode set, drops the undo
//! (replays can drop it before each resize: [`Undo`], `undo=off` on the wire).
//! An app that repaints only when its SIGWINCH handler reads a changed size
//! (Claude Code, Node's `tty.WriteStream`) never sees a completed pair, so its
//! later diff frames land on shifted content.
//!
//! The method: fold the recording through fresh headless engines
//! ([`ReplayEngine`], set up as the session's), varying only how resize runs
//! are applied.
//!
//! - Faithful applies every resize as recorded and must reproduce the live
//!   screen (the fidelity gate); otherwise nothing is attributed
//!   (`verdict=unfaithful`), so a dropped burst or evicted head never accuses a
//!   resize.
//! - Stable skips every net-zero candidate run and collapses every other to its
//!   final geometry: the screen the app believes it drew. Every candidate is
//!   included, so the culprit cannot fall outside a cap.
//! - The culprit is named from the faithful replay's engine journal: the
//!   candidates holding an alt-screen resize whose demoted or pushed rows no
//!   later grow handed back. When the journal names none (a primary-screen
//!   rotation, a ConPTY seam), attribution is by ONLY(r), run `r` collapsed
//!   alone, over the newest `max_runs` candidates.
//!
//! A candidate is a run of two or more resizes after the anchor: the last output
//! that replaced the whole screen (`ESC[2J`, or a clearing alt-screen entry)
//! with no alt toggle after it. Runs group resizes by time alone
//! ([`RUN_GAP_SECS`]), since output between a flap's halves is no sign the
//! handler ran and the recording's `o`/`r` order only approximates the engine's.
//!
//! A return is a candidate too: an alt-screen resize after the anchor and last
//! toggle whose moved rows no grow handed back, held by no run, through the
//! first later resize restoring the prior geometry (however far apart). An app
//! whose handler ran only after the size came back drew onto the moved rows, so
//! the stable replay skips the span; an app that repainted at both sizes lands
//! on the same screen and reads `clean`. A resize whose geometry never came back
//! is the app's to repaint and is replayed as recorded.
//!
//! Every candidate lies after the anchor, so every variant (stable, ONLY)
//! folds what faithful folds through the anchor event. Faithful forks its
//! engine there ([`ReplayEngine::fork`]) and each variant folds only the events
//! after it; with no anchor, or an engine that cannot be forked there, a
//! variant folds from the start (`fold_events=` counts what was folded). The
//! fidelity gate runs before any variant, so `unfaithful` folds none.
//!
//! Pure: no I/O, clock or GUI type. The server (`aterm-gui`, over its recorder
//! and live engine) and the client path (over a fetched `cast` and `text`, or
//! saved files) run this same code over the same values: the recording as
//! [`crate::asciicast`] models it, which the server writes for `cast` and the
//! client reads back. It lives in this crate, the headless half of the control
//! protocol, beside [`visible_row`] and [`pct_encode`].

/// The resize seam policy the replays apply, re-exported so a caller that
/// links only this crate (the one binary's client path) can name it.
pub use aterm_core::grid::{ResizeLaw, ResizePolicy};

use crate::asciicast::{CastDrops, CastEvent, CastHeader, HandoffHeader};
use crate::replay::ReplayEngine;
use crate::wire::{pct_encode, visible_row};

/// The request grammar, as the wire states it on a usage error.
pub const USAGE: &str = "ERR usage: cast drift [max_runs=<k>] [rows=<n>] [seed=auto|alt|primary]";

/// Candidate runs varied by default (newest first).
pub const DEFAULT_MAX_RUNS: usize = 8;

/// The most candidate runs one request may vary: each costs a fold of the
/// recording (of its events after the anchor, from the anchor fork, when the
/// replay could fork there), bounding work at `2 + MAX_RUNS` folds (one more
/// when a saved screen must pick the seed).
pub const MAX_RUNS: usize = 16;

/// Two resizes closer than this are one run, whatever output lies between: the
/// app's SIGWINCH handler may have read only the second size. A heuristic
/// (flaps are typically under a millisecond apart).
pub const RUN_GAP_SECS: f64 = 0.250;

/// The screen the recording is held against.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LiveScreen {
    /// Every visible row's text ([`visible_row`], or a `text` reply's rows).
    pub rows: Vec<String>,
    /// The cursor (row, col), when known.
    pub cursor: Option<(u16, u16)>,
    /// Whether the screen is on the alternate buffer, when known.
    pub alt: Option<bool>,
}

/// Which buffer the replay engines start on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Seed {
    /// The alternate screen when the first alt toggle is an exit, or when there
    /// is none and the live screen is on the alternate buffer (an adopted
    /// session, whose cast starts mid-app); primary when there is none and the
    /// screen is primary. With no toggle and no buffer information (a saved
    /// screen dump), whichever reproduces the screen; with no screen, primary,
    /// where a flap is lossless and a guess cannot accuse.
    #[default]
    Auto,
    /// Start on the alternate screen.
    Alt,
    /// Start on the primary screen.
    Primary,
}

/// Why the replays started on the buffer they did (`seed_basis=` on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedBasis {
    /// `seed=alt|primary` asked for it.
    Arg,
    /// The recording's first alt-screen toggle (an exit means it began on alt).
    Toggle,
    /// The live screen's buffer (the recording has no toggle).
    Live,
    /// Only one seed's faithful replay reproduced the given screen.
    Fidelity,
    /// Nothing said: primary, the seed on which a flap moves nothing.
    Default,
}

impl SeedBasis {
    /// The wire word.
    #[must_use]
    #[inline]
    pub const fn word(self) -> &'static str {
        match self {
            SeedBasis::Arg => "arg",
            SeedBasis::Toggle => "toggle",
            SeedBasis::Live => "live",
            SeedBasis::Fidelity => "fidelity",
            SeedBasis::Default => "default",
        }
    }
}

/// Whether the replays keep the engine's resize undo (a grow gets back the rows
/// an alt-screen shrink took when nothing was output between). A recording from
/// an engine without the undo, replayed with it, reproduces a screen it never
/// had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Undo {
    /// This engine's: what the server answers with, over its own recording.
    #[default]
    Keep,
    /// Dropped before every resize (`undo=off` on the wire): a client-side
    /// report against an aterm older than this verb (and the undo).
    Drop,
    /// Kept, unless only the replay without it reproduces the screen (then
    /// `undo=off`): a saved recording, whose aterm is not known. With no screen
    /// it is kept, and so it is for a recording whose header says its engine
    /// kept the undo ([`CastHeader::resize_undo`]).
    Match,
}

/// The parsed request tail, and [`Undo`], which is not on the wire: the one
/// binary's client path sets it for the aterm it computes against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriftArgs {
    /// Candidate runs attributed one at a time (only folds) and listed, newest
    /// first, when the journal names no culprit (`max_runs=`, at most
    /// [`MAX_RUNS`]; 0 = none). The verdict always weighs every candidate.
    pub max_runs: usize,
    /// At most this many `row` lines (`rows=`); `None` lists every drift row.
    pub list_rows: Option<usize>,
    /// The replay's starting buffer (`seed=`).
    pub seed: Seed,
    /// Whether the replays keep the resize undo ([`Undo::Keep`] from
    /// [`DriftArgs::parse`]).
    pub undo: Undo,
}

impl Default for DriftArgs {
    fn default() -> Self {
        Self {
            max_runs: DEFAULT_MAX_RUNS,
            list_rows: None,
            seed: Seed::Auto,
            undo: Undo::Keep,
        }
    }
}

impl DriftArgs {
    /// Parse `[max_runs=<k>] [rows=<n>] [seed=auto|alt|primary]` (any order).
    /// `max_runs` past [`MAX_RUNS`] is held at it. Anything else is refused with
    /// [`USAGE`], the line the caller answers.
    pub fn parse(rest: &str) -> Result<Self, &'static str> {
        let mut args = Self::default();
        for tok in rest.split_whitespace() {
            let (key, value) = tok.split_once('=').ok_or(USAGE)?;
            match key {
                "max_runs" => {
                    args.max_runs = value.parse::<usize>().map_err(|_| USAGE)?.min(MAX_RUNS);
                }
                "rows" => args.list_rows = Some(value.parse::<usize>().map_err(|_| USAGE)?),
                "seed" => {
                    args.seed = match value {
                        "auto" => Seed::Auto,
                        "alt" => Seed::Alt,
                        "primary" => Seed::Primary,
                        _ => return Err(USAGE),
                    };
                }
                _ => return Err(USAGE),
            }
        }
        Ok(args)
    }
}

/// The audit's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The recording holds no event.
    Empty,
    /// The screen is what the app believes it drew (stable matches).
    Clean,
    /// The recording reproduces the screen, and the screen is not what the app
    /// believes it drew: a resize run it never repainted after moved content.
    Desync,
    /// The recording does not reproduce the screen; nothing is attributed.
    Unfaithful,
    /// The screen moved while it was being read, so a difference from it
    /// cannot be told from a race; or, with no screen, the two seeds the replay
    /// could start from disagree (`seed=ambiguous`).
    Unknown,
}

impl Verdict {
    /// The wire word.
    #[must_use]
    #[inline]
    pub const fn word(self) -> &'static str {
        match self {
            Verdict::Empty => "empty",
            Verdict::Clean => "clean",
            Verdict::Desync => "desync",
            Verdict::Unfaithful => "unfaithful",
            Verdict::Unknown => "unknown",
        }
    }
}

/// How the live screen and the recording were read against each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    /// The content sequence did not move across the reads, which began at least
    /// a settle interval after it was first sampled: every burst the engine had
    /// processed was in the recording.
    Quiescent,
    /// The content moved while it was being read; a difference is `unknown`.
    Racy,
    /// Saved files (the offline form): no live engine to cut.
    File,
}

impl Cut {
    /// The wire word.
    #[must_use]
    #[inline]
    pub const fn word(self) -> &'static str {
        match self {
            Cut::Quiescent => "quiescent",
            Cut::Racy => "racy",
            Cut::File => "file",
        }
    }
}

/// One resize run, as the report lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Index of its first resize in the event list.
    pub first: usize,
    /// Index of its last resize.
    pub last: usize,
    /// Its first resize's timestamp (the cast's own spelling, 6 decimals).
    pub t: f64,
    /// Milliseconds from its first resize to its last.
    pub gap_ms: f64,
    /// The geometry chain, `(cols, rows)`, starting with the geometry before it.
    pub geoms: Vec<(u16, u16)>,
    /// Milliseconds from its last resize to the next output, if any came.
    pub first_out_ms: Option<f64>,
    /// Milliseconds from its last resize to the next full clear, if any came.
    pub clear_ms: Option<f64>,
    /// Whether this run is (one of) the drift's cause.
    pub culprit: bool,
}

impl Run {
    /// Resizes in the run.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.geoms.len().saturating_sub(1)
    }

    /// Whether the run holds no resize (never, for a recorded run).
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the run ends where it started.
    #[must_use]
    #[inline]
    pub fn net_zero(&self) -> bool {
        self.geoms.first() == self.geoms.last()
    }
}

/// One row the audit found wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftRow {
    /// Screen row (0-based).
    pub row: usize,
    /// What the screen (or, with no screen, the faithful replay) shows.
    pub live: String,
    /// What the app believes it drew (`desync`) or what the recording
    /// replays to (`unfaithful`).
    pub other: String,
}

/// The whole report; [`DriftReport::render`] is its wire form.
#[derive(Debug, Clone, PartialEq)]
pub struct DriftReport {
    /// The answer.
    pub verdict: Verdict,
    /// `Some(pass)` when a screen was given; `None` offline without one.
    pub fidelity: Option<bool>,
    /// The rows found wrong, ascending.
    pub drift: Vec<DriftRow>,
    /// Every resize run in the recording.
    pub runs_total: usize,
    /// Every candidate (after the anchor: a run of two or more resizes, or a
    /// return): all of them are collapsed in the stable replay.
    pub eligible: usize,
    /// The candidate runs listed (a `run` line each), oldest first: every
    /// culprit and the newest `max_runs`.
    pub candidates: Vec<Run>,
    /// Candidate runs not listed (`eligible - candidates.len()`); they are in
    /// the verdict all the same.
    pub capped: usize,
    /// The anchor's timestamp, `None` when no full clear bounds the search.
    pub anchor: Option<f64>,
    /// Whether the replays started on the alternate screen.
    pub seed_alt: bool,
    /// With no screen and no toggle to seed from, the replays on the two seeds
    /// disagreed: the verdict is `unknown` and the header says `seed=ambiguous`.
    pub seed_ambiguous: bool,
    /// Why they did.
    pub seed_basis: SeedBasis,
    /// With no screen, the replays under the resize laws the header names
    /// ([`CastHeader::resize_laws`]) disagreed: the verdict is `unknown` and
    /// the header says ` law=ambiguous`.
    pub law_ambiguous: bool,
    /// The faithful replay's final geometry, `(cols, rows)`.
    pub geom: (u16, u16),
    /// The header's eviction count.
    pub evicted: u64,
    /// The header's drop count.
    pub dropped: Option<CastDrops>,
    /// The header's handoff: the recording starts at a seamless update, so
    /// the screen before it is in no replay.
    pub handoff: Option<HandoffHeader>,
    /// Engines folded over the recording: each faithful replay, then each
    /// variant (stable, ONLY), from the start or from the anchor fork.
    pub folds: usize,
    /// Events those folds applied: every event for a fold from the start, the
    /// events after the anchor for a variant folded from the anchor fork.
    pub fold_events: usize,
    /// The live cursor (or the faithful replay's with no screen).
    pub cursor_live: Option<(u16, u16)>,
    /// The cursor the drift's `other` side has.
    pub cursor_other: Option<(u16, u16)>,
    /// How the cut was taken; see [`DriftReport::settle_cut`].
    pub cut: Option<Cut>,
    /// The `rows=` cap on listed rows.
    pub list_rows: Option<usize>,
    /// The replays dropped the engine's resize undo ([`Undo`]): the header
    /// then says ` undo=off`.
    pub undo_dropped: bool,
}

impl DriftReport {
    /// Record how the screen and recording were cut. A racy cut of a report
    /// held against the screen turns `desync` or `unfaithful` into `unknown`
    /// (the difference may be a burst not yet recorded); `clean` stays clean. A
    /// replay-against-replay report ([`analyze_replay`]) reads no screen and
    /// keeps its verdict.
    pub fn settle_cut(&mut self, cut: Cut) {
        self.cut = Some(cut);
        if cut == Cut::Racy
            && self.fidelity.is_some()
            && matches!(self.verdict, Verdict::Desync | Verdict::Unfaithful)
        {
            self.verdict = Verdict::Unknown;
        }
    }

    /// The culprit runs' timestamps, comma-joined, or `-`.
    #[must_use]
    pub fn culprit_word(&self) -> String {
        let ts: Vec<String> = self
            .candidates
            .iter()
            .filter(|r| r.culprit)
            .map(|r| fmt_t(r.t))
            .collect();
        if ts.is_empty() {
            "-".to_string()
        } else {
            ts.join(",")
        }
    }

    /// The wire form: `OK <n> <header k=v…>` then `n` lines: the candidate
    /// `run` lines (oldest first), the `row` lines (ascending, at most `rows=`)
    /// and one `cursor` line when there is a finding. `ms` is the caller's
    /// timing (this module has no clock), after `folds=` and `fold_events=`
    /// ([`DriftReport::fold_events`]). After it come ` carried=0
    /// from_build=<n|->` (the tail every text surface prints for a seamless
    /// update) when the recording starts at one, ` law=ambiguous` when the
    /// replays under the header's resize laws disagreed, ` undo=off` when they
    /// dropped the undo, and `computed`, naming where it ran when not the
    /// server (`computed=client`).
    #[must_use]
    pub fn render(&self, ms: u64, computed: Option<&str>) -> String {
        let mut body = String::new();
        let mut n = 0usize;
        for run in &self.candidates {
            body.push_str(&format!(
                "run t={} n={} gap_ms={:.3} geom={} net={} first_out_ms={} clear_ms={} culprit={}\n",
                fmt_t(run.t),
                run.len(),
                run.gap_ms,
                geom_chain(&run.geoms),
                if run.net_zero() {
                    "zero".to_string()
                } else {
                    run.geoms.last().map_or_else(|| "-".to_string(), |g| fmt_geom(*g))
                },
                fmt_ms(run.first_out_ms),
                fmt_ms(run.clear_ms),
                u8::from(run.culprit),
            ));
            n += 1;
        }
        let other_key = if self.fidelity == Some(false) {
            "replayed"
        } else {
            "expected"
        };
        let listed = self.list_rows.unwrap_or(usize::MAX);
        for row in self.drift.iter().take(listed) {
            body.push_str(&format!(
                "row {} live={} {other_key}={}\n",
                row.row,
                pct_encode(&row.live),
                pct_encode(&row.other)
            ));
            n += 1;
        }
        if !self.drift.is_empty()
            && let Some(other) = self.cursor_other
        {
            body.push_str(&format!(
                "cursor live={} {other_key}={}\n",
                self.cursor_live.map_or_else(|| "-".to_string(), fmt_cursor),
                fmt_cursor(other)
            ));
            n += 1;
        }
        let mut head = format!(
            "OK {n} verdict={} fidelity={} against={} drift_rows={} runs={} candidates={} \
             capped={} culprit={} anchor={} seed={} seed_basis={} geom={} evicted={} dropped={} \
             cut={} folds={} fold_events={} ms={ms}",
            self.verdict.word(),
            match self.fidelity {
                Some(true) => "pass",
                Some(false) => "fail",
                None => "-",
            },
            if self.fidelity.is_some() {
                "screen"
            } else {
                "replay"
            },
            self.drift.len(),
            self.runs_total,
            self.eligible,
            self.capped,
            self.culprit_word(),
            self.anchor.map_or_else(|| "-".to_string(), fmt_t),
            if self.seed_ambiguous {
                "ambiguous"
            } else if self.seed_alt {
                "alt"
            } else {
                "primary"
            },
            self.seed_basis.word(),
            fmt_geom(self.geom),
            self.evicted,
            self.dropped
                .map_or_else(|| "-".to_string(), |d| d.bursts.to_string()),
            self.cut.map_or("-", Cut::word),
            self.folds,
            self.fold_events,
        );
        if let Some(handoff) = self.handoff {
            head.push_str(" carried=0 from_build=");
            match handoff.from_build {
                Some(build) => head.push_str(&build.to_string()),
                None => head.push('-'),
            }
        }
        if self.law_ambiguous {
            head.push_str(" law=ambiguous");
        }
        if self.undo_dropped {
            head.push_str(" undo=off");
        }
        if let Some(where_) = computed {
            head.push_str(" computed=");
            head.push_str(where_);
        }
        head.push('\n');
        head.push_str(&body);
        head
    }
}

/// `t` as the cast prints it: seconds with six decimals.
#[must_use]
pub fn fmt_t(t: f64) -> String {
    format!("{t:.6}")
}

fn fmt_ms(ms: Option<f64>) -> String {
    ms.map_or_else(|| "-".to_string(), |v| format!("{v:.3}"))
}

fn fmt_geom((cols, rows): (u16, u16)) -> String {
    format!("{cols}x{rows}")
}

fn fmt_cursor((row, col): (u16, u16)) -> String {
    format!("{row},{col}")
}

fn geom_chain(geoms: &[(u16, u16)]) -> String {
    let parts: Vec<String> = geoms.iter().map(|g| fmt_geom(*g)).collect();
    parts.join(">")
}

// The analysis.

/// A position in the recording: (event index, byte offset in its burst).
type Pos = (usize, usize);

/// What a byte scan of the output finds: the alt-screen toggles and the full
/// clears the anchor and the seed are decided from.
#[derive(Default)]
struct Marks {
    /// Whether the first alt-screen toggle is an exit (`Some(true)`), an entry
    /// (`Some(false)`), or there is none.
    first_toggle_exit: Option<bool>,
    /// The last alt-screen toggle of any kind.
    last_toggle: Option<Pos>,
    /// The last full-screen replacement (`ESC[2J`, `?1049h`, `?1047h`).
    last_clear: Option<Pos>,
    /// Per event: whether it holds a full-screen replacement.
    clears: Vec<bool>,
}

/// The escape sequences the scan recognizes: (bytes, is a toggle, toggle is an
/// exit, replaces the whole screen).
const MARKS: &[(&[u8], bool, bool, bool)] = &[
    (b"\x1b[2J", false, false, true),
    (b"\x1b[?1049h", true, false, true),
    (b"\x1b[?1047h", true, false, true),
    (b"\x1b[?47h", true, false, false),
    (b"\x1b[?1049l", true, true, false),
    (b"\x1b[?1047l", true, true, false),
    (b"\x1b[?47l", true, true, false),
];

fn scan_marks<D: AsRef<[u8]>>(events: &[CastEvent<D>]) -> Marks {
    let mut m = Marks {
        clears: vec![false; events.len()],
        ..Marks::default()
    };
    for (i, ev) in events.iter().enumerate() {
        let CastEvent::Output { data, .. } = ev else {
            continue;
        };
        let bytes = data.as_ref();
        for (off, _) in bytes.iter().enumerate().filter(|(_, b)| **b == 0x1b) {
            let tail = &bytes[off..];
            for &(seq, toggle, exit, clears) in MARKS {
                if !tail.starts_with(seq) {
                    continue;
                }
                if toggle {
                    if m.first_toggle_exit.is_none() {
                        m.first_toggle_exit = Some(exit);
                    }
                    m.last_toggle = Some((i, off));
                }
                if clears {
                    m.last_clear = Some((i, off));
                    m.clears[i] = true;
                }
            }
        }
    }
    m
}

/// Group the resizes into runs ([`RUN_GAP_SECS`] apart at most, whatever
/// output lies between: see the module note).
fn group_runs<D>(header: &CastHeader, events: &[CastEvent<D>], clears: &[bool]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut open: Option<Run> = None;
    let mut cur = (header.width, header.height);
    let mut last_t = 0.0f64;
    for (i, ev) in events.iter().enumerate() {
        match ev {
            CastEvent::Output { .. } => {}
            CastEvent::Resize { t, cols, rows } => {
                let joins = open.is_some() && *t - last_t <= RUN_GAP_SECS;
                match open.as_mut() {
                    Some(run) if joins => {
                        run.last = i;
                        run.geoms.push((*cols, *rows));
                    }
                    _ => {
                        if let Some(run) = open.take() {
                            runs.push(run);
                        }
                        open = Some(Run {
                            first: i,
                            last: i,
                            t: *t,
                            gap_ms: 0.0,
                            geoms: vec![cur, (*cols, *rows)],
                            first_out_ms: None,
                            clear_ms: None,
                            culprit: false,
                        });
                    }
                }
                last_t = *t;
                cur = (*cols, *rows);
            }
        }
    }
    if let Some(run) = open.take() {
        runs.push(run);
    }
    follow_up(events, clears, &mut runs);
    runs
}

/// Each run's gap and follow-up facts (the next output and the next full clear
/// after its last resize), from one backward pass over the events.
fn follow_up<D>(events: &[CastEvent<D>], clears: &[bool], runs: &mut [Run]) {
    if runs.is_empty() {
        return;
    }
    let mut next_out: Vec<Option<f64>> = vec![None; events.len() + 1];
    let mut next_clear: Vec<Option<f64>> = vec![None; events.len() + 1];
    for i in (0..events.len()).rev() {
        next_out[i] = next_out[i + 1];
        next_clear[i] = next_clear[i + 1];
        if let CastEvent::Output { t, .. } = &events[i] {
            next_out[i] = Some(*t);
            if clears[i] {
                next_clear[i] = Some(*t);
            }
        }
    }
    for run in runs {
        let t_last = events[run.last].t();
        run.gap_ms = (t_last - run.t) * 1000.0;
        run.first_out_ms = next_out[run.last + 1].map(|t| (t - t_last) * 1000.0);
        run.clear_ms = next_clear[run.last + 1].map(|t| (t - t_last) * 1000.0);
    }
}

/// The returns (see the module note): from each `lossy` resize (event indices,
/// ascending, after the anchor and last alt toggle) not covered by a `held`
/// candidate or an earlier return, through the first later resize restoring
/// the prior geometry. A resize whose geometry never came back yields none.
fn return_chains<D>(
    header: &CastHeader,
    events: &[CastEvent<D>],
    clears: &[bool],
    held: &[Run],
    lossy: &[usize],
) -> Vec<Run> {
    let resize_to = |ev: &CastEvent<D>| match ev {
        CastEvent::Resize { cols, rows, .. } => Some((*cols, *rows)),
        CastEvent::Output { .. } => None,
    };
    // The geometry in force before each event.
    let mut before = Vec::with_capacity(events.len());
    let mut cur = (header.width, header.height);
    for ev in events {
        before.push(cur);
        if let Some(to) = resize_to(ev) {
            cur = to;
        }
    }
    let mut chains: Vec<Run> = Vec::new();
    for &i in lossy {
        let covers = |run: &Run| (run.first..=run.last).contains(&i);
        if i >= events.len() || held.iter().any(covers) || chains.iter().any(covers) {
            continue;
        }
        let from = before[i];
        let Some(back) = (i + 1..events.len()).find(|&j| resize_to(&events[j]) == Some(from))
        else {
            continue;
        };
        let geoms: Vec<(u16, u16)> = std::iter::once(from)
            .chain(events[i..=back].iter().filter_map(resize_to))
            .collect();
        chains.push(Run {
            first: i,
            last: back,
            t: events[i].t(),
            gap_ms: 0.0,
            geoms,
            first_out_ms: None,
            clear_ms: None,
            culprit: false,
        });
    }
    follow_up(events, clears, &mut chains);
    chains
}

/// A replayed engine's facts.
#[cfg_attr(test, derive(Debug, PartialEq))]
struct Replay {
    rows: Vec<String>,
    cursor: (u16, u16),
    geom: (u16, u16),
    /// Event indices of applied resizes the engine journalled as lossy on the
    /// alternate screen (rows no later grow's undo handed back), when the fold
    /// watched its journal.
    lossy: Vec<usize>,
}

/// Which resizes a variant applies. `collapse` lists the runs it collapses: a
/// net-zero run is skipped whole, any other keeps only its last resize.
fn apply_mask<D>(events: &[CastEvent<D>], collapse: &[&Run]) -> Vec<bool> {
    let mut mask = vec![true; events.len()];
    for run in collapse {
        let end = if run.net_zero() {
            run.last + 1
        } else {
            run.last
        };
        for slot in &mut mask[run.first..end] {
            *slot = false;
        }
    }
    mask
}

/// The console-host posture of the live engine a recording came from, which
/// every replay of it takes: the resize seam `policy` (`ConPty` behind a
/// Windows pseudoconsole, else `Native`), and whether a dead app's alternate
/// screen is left by the HOST (OpenConsole: `Terminal::set_host_resyncs_alt_screen`)
/// rather than locally at the shell's next prompt. A bare [`ResizePolicy`] is
/// the posture with the local leave, every host's but OpenConsole's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Posture {
    /// The resize seam policy the session resizes under.
    pub policy: ResizePolicy,
    /// The session's host leaves a dead app's alternate screen itself, and the
    /// recording carries the `?1049l` it forwarded.
    pub host_resyncs_alt_screen: bool,
}

impl From<ResizePolicy> for Posture {
    fn from(policy: ResizePolicy) -> Self {
        Self {
            policy,
            host_resyncs_alt_screen: false,
        }
    }
}

/// How every fold of one audit applies resizes: the session's posture, and
/// whether the engine keeps its resize undo ([`Undo`]).
#[derive(Clone, Copy)]
struct Engine {
    policy: ResizePolicy,
    host_resyncs: bool,
    keep_undo: bool,
    /// The resize law the replay lays its rows out under, one of the
    /// recording's ([`CastHeader::resize_laws`], `Terminal::set_resize_law`).
    law: ResizeLaw,
}

/// Fold the recording through one fresh [`ReplayEngine`] set up as the
/// session's (its posture, and `engine.law`, the resize law the rows are laid
/// out under); `journal` also collects the lossy alt-screen resizes
/// ([`Replay::lossy`]). `fork_after` names the event after which a copy of
/// the engine is taken ([`ReplayEngine::fork`]) for the variants that fold
/// the same events up to there; it comes back beside the replay, `None` when
/// the engine could not be forked there.
fn fold<D: AsRef<[u8]>>(
    header: &CastHeader,
    events: &[CastEvent<D>],
    seed_alt: bool,
    mask: Option<&[bool]>,
    engine: Engine,
    journal: bool,
    fork_after: Option<usize>,
) -> (Replay, Option<ReplayEngine>) {
    let mut replay = ReplayEngine::new(header.height, header.width, engine.policy)
        .with_resize_law(engine.law)
        .with_host_resyncs_alt_screen(engine.host_resyncs);
    if seed_alt {
        replay.process(b"\x1b[?1049h");
    }
    // Each alt-screen resize that took rows off, with how many are still gone;
    // a grow served by the undo hands the newest back (LIFO), so a quiet flap
    // leaves nothing here.
    let mut moved: Vec<(usize, u32)> = Vec::new();
    let mut fork = None;
    for (i, ev) in events.iter().enumerate() {
        step(
            &mut replay,
            i,
            ev,
            mask,
            engine,
            journal.then_some(&mut moved),
        );
        if fork_after == Some(i) {
            fork = replay.fork();
        }
    }
    let mut lossy: Vec<usize> = moved.into_iter().map(|(i, _)| i).collect();
    lossy.dedup();
    (facts(&replay, lossy), fork)
}

/// Fold `events[from..]` through `replay`, an engine that has folded the
/// events before `from` (a [`ReplayEngine::fork`]).
fn fold_rest<D: AsRef<[u8]>>(
    mut replay: ReplayEngine,
    events: &[CastEvent<D>],
    from: usize,
    mask: Option<&[bool]>,
    engine: Engine,
) -> Replay {
    for (i, ev) in events.iter().enumerate().skip(from) {
        step(&mut replay, i, ev, mask, engine, None);
    }
    facts(&replay, Vec::new())
}

/// Apply event `i`: output is processed; a resize the mask keeps is applied
/// (without the undo, `engine.keep_undo` false, after dropping the active
/// grid's undo) and, with `moved`, its journal folded into the lossy ledger.
fn step<D: AsRef<[u8]>>(
    replay: &mut ReplayEngine,
    i: usize,
    ev: &CastEvent<D>,
    mask: Option<&[bool]>,
    engine: Engine,
    moved: Option<&mut Vec<(usize, u32)>>,
) {
    match ev {
        CastEvent::Output { data, .. } => replay.process(data.as_ref()),
        CastEvent::Resize { cols, rows, .. } => {
            if mask.is_some_and(|m| !m[i]) {
                return;
            }
            if !engine.keep_undo {
                replay.drop_resize_undo();
            }
            replay.resize(*rows, *cols);
            let Some(moved) = moved else { return };
            let term = replay.terminal();
            let (reports, _) = term.resize_journal_since(term.resize_ordinal().saturating_sub(1));
            for r in reports.iter().filter(|r| r.alt && !r.reflowed) {
                let off = u32::from(r.demoted) + u32::from(r.pushed);
                if off > 0 {
                    moved.push((i, off));
                }
                let mut back = u32::from(r.restored_top) + u32::from(r.restored_bottom);
                while back > 0 {
                    let Some(last) = moved.last_mut() else { break };
                    let take = back.min(last.1);
                    last.1 -= take;
                    back -= take;
                    if last.1 == 0 {
                        moved.pop();
                    }
                }
            }
        }
    }
}

/// What a folded engine shows.
fn facts(replay: &ReplayEngine, lossy: Vec<usize>) -> Replay {
    let term = replay.terminal();
    let rows = (0..term.rows() as usize)
        .map(|r| visible_row(term, r))
        .collect();
    let c = term.cursor();
    Replay {
        rows,
        cursor: (c.row, c.col),
        geom: (term.cols(), term.rows()),
        lossy,
    }
}

/// The variant folds of one audit (stable, and each ONLY(r)): from a fork of
/// the faithful replay taken just after the anchor event when there is one
/// (every candidate's first resize lies after the anchor, so up to there each
/// variant folds exactly what faithful folded), else from the start.
struct Variants<'a, D> {
    header: &'a CastHeader,
    events: &'a [CastEvent<D>],
    seed_alt: bool,
    engine: Engine,
    /// The fork and the first event it has not folded.
    fork: Option<(ReplayEngine, usize)>,
}

impl<D: AsRef<[u8]>> Variants<'_, D> {
    /// Fold the variant `mask`, counted in `report`. `again`: another variant
    /// will fold after this one, so it folds a copy of the fork and keeps the
    /// fork for that one.
    fn fold(&mut self, mask: &[bool], again: bool, report: &mut DriftReport) -> Replay {
        report.folds += 1;
        let start = match (self.fork.take(), again) {
            (Some((fork, from)), true) => {
                let copy = fork.fork();
                self.fork = Some((fork, from));
                copy.map(|copy| (copy, from))
            }
            (fork, _) => fork,
        };
        let Some((replay, from)) = start else {
            report.fold_events += self.events.len();
            let (full, _) = fold(
                self.header,
                self.events,
                self.seed_alt,
                Some(mask),
                self.engine,
                false,
                None,
            );
            return full;
        };
        report.fold_events += self.events.len() - from;
        let rest = fold_rest(replay, self.events, from, Some(mask), self.engine);
        // Every fold in this crate's tests also folds the variant from the
        // start: a fork must never change what a variant shows.
        #[cfg(test)]
        {
            let (full, _) = fold(
                self.header,
                self.events,
                self.seed_alt,
                Some(mask),
                self.engine,
                false,
                None,
            );
            assert_eq!(rest, full, "a forked variant folds what its full fold does");
        }
        rest
    }
}

/// The rows where `a` and `b` differ (a missing row reads as blank).
fn diff_rows(a: &[String], b: &[String]) -> Vec<usize> {
    (0..a.len().max(b.len()))
        .filter(|&r| row_at(a, r) != row_at(b, r))
        .collect()
}

fn row_at(rows: &[String], r: usize) -> &str {
    rows.get(r).map_or("", |s| s.trim_end())
}

fn same_on(a: &[String], b: &[String], rows: &[usize]) -> bool {
    rows.iter().all(|&r| row_at(a, r) == row_at(b, r))
}

/// Run the audit. `live` is the screen to hold the recording against (`None`
/// offline: faithful stands in and fidelity reads `-`). `posture` is the
/// session's ([`Posture`]; a bare [`ResizePolicy`] is its resize seam policy,
/// `ConPty` behind a Windows pseudoconsole, else `Native`). Record the cut
/// afterwards with [`DriftReport::settle_cut`].
#[must_use]
pub fn analyze<D: AsRef<[u8]>>(
    header: &CastHeader,
    events: &[CastEvent<D>],
    live: Option<&LiveScreen>,
    args: &DriftArgs,
    posture: impl Into<Posture>,
) -> DriftReport {
    analyze_inner(
        header,
        events,
        live,
        live.and_then(|l| l.alt),
        args,
        posture.into(),
    )
}

/// The audit of the replays against each other (faithful vs stable,
/// `against=replay fidelity=-`), seeded from the live buffer when `alt` says
/// so. For a live screen that will not hold still (`cut=racy`): no screen is
/// compared, so a racing app cannot keep the answer `unknown`, at the cost of
/// the fidelity gate.
#[must_use]
pub fn analyze_replay<D: AsRef<[u8]>>(
    header: &CastHeader,
    events: &[CastEvent<D>],
    alt: Option<bool>,
    args: &DriftArgs,
    posture: impl Into<Posture>,
) -> DriftReport {
    analyze_inner(header, events, None, alt, args, posture.into())
}

fn analyze_inner<D: AsRef<[u8]>>(
    header: &CastHeader,
    events: &[CastEvent<D>],
    live: Option<&LiveScreen>,
    alt_hint: Option<bool>,
    args: &DriftArgs,
    posture: Posture,
) -> DriftReport {
    let marks = scan_marks(events);
    let under = |keep_undo: bool, law: ResizeLaw| {
        let engine = Engine {
            policy: posture.policy,
            host_resyncs: posture.host_resyncs_alt_screen,
            keep_undo,
            law,
        };
        analyze_seeded(header, events, &marks, live, alt_hint, args, engine)
    };
    // Under the recording's resize law: the newest it names, and an older one
    // only when the newer do not reproduce the live screen (two aterms wrote
    // `aterm_resize_undo: 2`). A replay under a law the recording's engine did
    // not keep reads every row the laws place differently as drift. With no
    // screen to choose by, every law it names is folded, and when their
    // answers differ the answer is `unknown`, `law=ambiguous`, as two seeds'
    // are (`analyze_seeded`).
    let seeded = |keep_undo: bool| {
        let mut laws = header.resize_laws.iter().copied();
        let mut report = under(keep_undo, laws.next().unwrap_or_default());
        if live.is_none() {
            for law in laws {
                let other = under(keep_undo, law);
                report.folds += other.folds;
                report.fold_events += other.fold_events;
                report.law_ambiguous |= (other.verdict, other.culprit_word(), &other.drift)
                    != (report.verdict, report.culprit_word(), &report.drift);
            }
            if report.law_ambiguous {
                report.verdict = Verdict::Unknown;
                report.drift.clear();
                report.cursor_other = None;
                for run in &mut report.candidates {
                    run.culprit = false;
                }
            }
            return report;
        }
        for law in laws {
            if report.fidelity != Some(false) {
                break;
            }
            let mut older = under(keep_undo, law);
            if older.fidelity == Some(true) {
                older.folds += report.folds;
                older.fold_events += report.fold_events;
                return older;
            }
            report.folds += older.folds;
            report.fold_events += older.fold_events;
        }
        report
    };
    match args.undo {
        Undo::Keep => seeded(true),
        Undo::Drop => seeded(false),
        // A saved recording from an aterm without the undo reads `unfaithful`
        // wherever a quiet flap shifted its screen; the undo-less replay is
        // taken only when it alone reproduces the screen.
        Undo::Match => {
            let mut kept = seeded(true);
            // A recording that says its engine kept the undo is replayed under
            // it only: a fidelity failure there is an undo regression, not an
            // older aterm, and must stay `unfaithful`.
            if live.is_none() || kept.fidelity != Some(false) || header.resize_undo == Some(true) {
                return kept;
            }
            let mut dropped = seeded(false);
            if dropped.fidelity == Some(true) {
                dropped.folds += kept.folds;
                dropped.fold_events += kept.fold_events;
                dropped
            } else {
                kept.folds += dropped.folds;
                kept.fold_events += dropped.fold_events;
                kept
            }
        }
    }
}

/// [`analyze_inner`] on one [`Engine`]: the seed chosen, and on a guessed one
/// with no screen, the other seed folded too.
fn analyze_seeded<D: AsRef<[u8]>>(
    header: &CastHeader,
    events: &[CastEvent<D>],
    marks: &Marks,
    live: Option<&LiveScreen>,
    alt_hint: Option<bool>,
    args: &DriftArgs,
    engine: Engine,
) -> DriftReport {
    let (seed_alt, seed_basis) = match args.seed {
        Seed::Alt => (true, SeedBasis::Arg),
        Seed::Primary => (false, SeedBasis::Arg),
        Seed::Auto => match (marks.first_toggle_exit, alt_hint) {
            (Some(exit), _) => (exit, SeedBasis::Toggle),
            (None, Some(alt)) => (alt, SeedBasis::Live),
            (None, None) => (false, SeedBasis::Default),
        },
    };
    let mut report = audit(
        header, events, marks, live, seed_alt, seed_basis, args, engine,
    );
    // No screen and no indication of the starting buffer (a saved recording
    // of an adopted session with no toggle): one guessed seed is no finding.
    // Fold the other; if they disagree the answer is `unknown`,
    // `seed=ambiguous` (pass `seed=alt|primary`). Any resize counts, since a
    // return is a candidate only on the seed whose journal saw rows lost.
    if live.is_none() && seed_basis == SeedBasis::Default && report.runs_total > 0 {
        let other = audit(
            header, events, marks, None, !seed_alt, seed_basis, args, engine,
        );
        report.folds += other.folds;
        report.fold_events += other.fold_events;
        if other.verdict != report.verdict {
            report.verdict = Verdict::Unknown;
            report.seed_ambiguous = true;
            report.drift.clear();
            report.cursor_other = None;
            for run in &mut report.candidates {
                run.culprit = false;
            }
        }
    }
    report
}

/// One audit on one seed (see the module note for the method).
#[allow(
    clippy::too_many_arguments,
    reason = "the audit's inputs, each read once; a struct would only rename them"
)]
fn audit<D: AsRef<[u8]>>(
    header: &CastHeader,
    events: &[CastEvent<D>],
    marks: &Marks,
    live: Option<&LiveScreen>,
    mut seed_alt: bool,
    mut seed_basis: SeedBasis,
    args: &DriftArgs,
    engine: Engine,
) -> DriftReport {
    let mut report = DriftReport {
        verdict: Verdict::Empty,
        fidelity: live.map(|_| true),
        drift: Vec::new(),
        runs_total: 0,
        eligible: 0,
        candidates: Vec::new(),
        capped: 0,
        anchor: None,
        seed_alt,
        seed_ambiguous: false,
        seed_basis,
        law_ambiguous: false,
        geom: (header.width, header.height),
        evicted: header.evicted,
        dropped: header.dropped,
        handoff: header.handoff,
        folds: 0,
        fold_events: 0,
        cursor_live: live.and_then(|l| l.cursor),
        cursor_other: None,
        cut: None,
        list_rows: args.list_rows,
        undo_dropped: !engine.keep_undo,
    };
    if events.is_empty() {
        return report;
    }

    // The anchor: the last full clear, provided no alt toggle follows it (an
    // exit brings back a screen the clear never touched).
    let anchor = match (marks.last_clear, marks.last_toggle) {
        (Some(clear), Some(toggle)) if clear < toggle => None,
        (clear, _) => clear.map(|(ev, _)| ev),
    };
    report.anchor = anchor.map(|ev| events[ev].t());

    let runs = group_runs(header, events, &marks.clears);
    report.runs_total = runs.len();

    // The faithful replay also forks just after the anchor event, where the
    // variants start to differ from it at the earliest. Every candidate holds
    // two resizes after the anchor (a run of two or more, or a lossy resize
    // and the later one restoring its geometry), so with fewer there is no
    // variant to fork for.
    let fork_after = anchor.filter(|&a| {
        events[a + 1..]
            .iter()
            .filter(|ev| matches!(ev, CastEvent::Resize { .. }))
            .nth(1)
            .is_some()
    });
    let (mut faithful, mut fork) = fold(header, events, seed_alt, None, engine, true, fork_after);
    report.folds = 1;
    report.fold_events = events.len();
    // No buffer information and no toggle: the fidelity gate picks the seed;
    // alt only when it alone reproduces the screen.
    if seed_basis == SeedBasis::Default
        && let Some(live) = live
        && !diff_rows(&live.rows, &faithful.rows).is_empty()
    {
        let (alt, alt_fork) = fold(header, events, true, None, engine, true, fork_after);
        report.folds += 1;
        report.fold_events += events.len();
        if diff_rows(&live.rows, &alt.rows).is_empty() {
            (faithful, fork) = (alt, alt_fork);
            seed_alt = true;
            seed_basis = SeedBasis::Fidelity;
            report.seed_alt = seed_alt;
            report.seed_basis = seed_basis;
        }
    }
    report.geom = faithful.geom;

    // The lossy alt-screen resizes on the screen still showing: after the
    // last toggle, and (for a candidate) after the anchor.
    let since_toggle = marks.last_toggle.map(|(ev, _)| ev);
    let lossy: Vec<usize> = faithful
        .lossy
        .iter()
        .copied()
        .filter(|&i| since_toggle.is_none_or(|t| i > t))
        .collect();
    // The candidates, oldest first: every run of two or more resizes after
    // the anchor, and every return; a run a return spans is folded into it.
    let mut owned: Vec<Run> = runs
        .iter()
        .filter(|r| r.len() >= 2 && anchor.is_none_or(|a| r.first > a))
        .cloned()
        .collect();
    let after_anchor: Vec<usize> = lossy
        .iter()
        .copied()
        .filter(|&i| anchor.is_none_or(|a| i > a))
        .collect();
    let returns = return_chains(header, events, &marks.clears, &owned, &after_anchor);
    if !returns.is_empty() {
        owned.retain(|r| {
            !returns
                .iter()
                .any(|c| c.first <= r.first && r.last <= c.last)
        });
        owned.extend(returns);
        owned.sort_by_key(|r| r.first);
    }
    let eligible: Vec<&Run> = owned.iter().collect();
    report.eligible = eligible.len();

    // The screen the finding is about: the live one, or the faithful replay.
    let live_rows: &[String] = live.map_or(&faithful.rows, |l| &l.rows);
    if report.cursor_live.is_none() && live.is_none() {
        report.cursor_live = Some(faithful.cursor);
    }
    // Listed until a culprit says otherwise: the newest `max_runs`.
    let newest = eligible.len().saturating_sub(args.max_runs);
    let listed = |culprits: &[bool]| -> Vec<Run> {
        eligible
            .iter()
            .zip(culprits)
            .enumerate()
            .filter(|(i, (_, culprit))| *i >= newest || **culprit)
            .map(|(_, (run, culprit))| Run {
                culprit: *culprit,
                ..(*run).clone()
            })
            .collect()
    };
    let none = vec![false; eligible.len()];
    report.candidates = listed(&none);
    report.capped = eligible.len() - report.candidates.len();

    if live.is_some() {
        let unfaithful = diff_rows(live_rows, &faithful.rows);
        if !unfaithful.is_empty() {
            report.fidelity = Some(false);
            report.verdict = Verdict::Unfaithful;
            report.cursor_other = Some(faithful.cursor);
            report.drift = unfaithful
                .into_iter()
                .map(|r| DriftRow {
                    row: r,
                    live: row_at(live_rows, r).to_string(),
                    other: row_at(&faithful.rows, r).to_string(),
                })
                .collect();
            return report;
        }
    }

    // The faithful journal names every candidate holding a lossy alt-screen
    // resize on the screen still showing. Known before any variant folds: it
    // decides whether ONLY folds can follow the stable one.
    let journal: Vec<bool> = eligible
        .iter()
        .map(|run| lossy.iter().any(|i| (run.first..=run.last).contains(i)))
        .collect();
    let may_vary = eligible.len() > 1 && newest < eligible.len() && !journal.contains(&true);
    let mut variants = Variants {
        header,
        events,
        seed_alt,
        engine,
        fork: fork.zip(fork_after).map(|(fork, a)| (fork, a + 1)),
    };
    // Stable collapses every candidate: the verdict never depends on a cap.
    let stable = (!eligible.is_empty())
        .then(|| variants.fold(&apply_mask(events, &eligible), may_vary, &mut report));
    let stable_ref = stable.as_ref().unwrap_or(&faithful);
    let drift = diff_rows(live_rows, &stable_ref.rows);
    if drift.is_empty() {
        report.verdict = Verdict::Clean;
        return report;
    }
    report.verdict = Verdict::Desync;
    report.cursor_other = Some(stable_ref.cursor);
    report.drift = drift
        .iter()
        .map(|&r| DriftRow {
            row: r,
            live: row_at(live_rows, r).to_string(),
            other: row_at(&stable_ref.rows, r).to_string(),
        })
        .collect();

    // Attribution. A single candidate is the culprit by construction (its
    // only fold is the stable one). Otherwise the journal's culprits, and when
    // it names none, the ONLY folds'.
    let blame: Vec<bool> = if eligible.len() == 1 {
        vec![true]
    } else if !may_vary {
        journal
    } else {
        // No lossy resize to point at: vary the newest `max_runs` alone.
        // A run whose collapse alone explains every drift row is the
        // culprit; failing a sole one, every run whose collapse alone
        // changes a drift row (each contributes).
        let tried: Vec<usize> = (newest..eligible.len()).collect();
        let only: Vec<Replay> = tried
            .iter()
            .enumerate()
            .map(|(k, &i)| {
                let mask = apply_mask(events, &eligible[i..=i]);
                variants.fold(&mask, k + 1 < tried.len(), &mut report)
            })
            .collect();
        let sole: Vec<bool> = only
            .iter()
            .map(|o| same_on(&o.rows, &stable_ref.rows, &drift))
            .collect();
        let hits: Vec<bool> = if sole.iter().any(|s| *s) {
            sole
        } else {
            only.iter()
                .map(|o| !same_on(&o.rows, &faithful.rows, &drift))
                .collect()
        };
        let mut blame = none.clone();
        for (i, hit) in tried.into_iter().zip(hits) {
            blame[i] = hit;
        }
        blame
    };
    report.candidates = listed(&blame);
    report.capped = eligible.len() - report.candidates.len();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::terminal::Terminal;

    /// A recording under construction, with the live engine driven by the same
    /// operations beside it: the screen the audit is held against.
    struct Rec {
        header: CastHeader,
        events: Vec<CastEvent<Vec<u8>>>,
        live: Terminal,
        t: f64,
        /// The live engine stands in for an aterm before the resize undo: it
        /// drops the undo before every resize, so a quiet flap shifts it too.
        pre_undo: bool,
    }

    impl Rec {
        /// A `cols`x`rows` recording; `adopted` puts the live engine on the alt
        /// screen before the recording starts, as an adopted session is.
        fn new(cols: u16, rows: u16, adopted: bool) -> Self {
            let mut live = Terminal::new(rows, cols);
            if adopted {
                live.process(b"\x1b[?1049h");
            }
            Self {
                header: CastHeader {
                    width: cols,
                    height: rows,
                    evicted: 0,
                    dropped: Some(CastDrops::default()),
                    resize_undo: None,
                    // The live engine below lays its rows out under the
                    // current law.
                    resize_laws: CastHeader::laws_named(Some(ResizeLaw::default().header_value())),
                    handoff: None,
                },
                events: Vec::new(),
                live,
                t: 0.0,
                pre_undo: false,
            }
        }

        fn out(&mut self, dt: f64, bytes: &[u8]) -> f64 {
            self.t += dt;
            self.live.process(bytes);
            self.events.push(CastEvent::Output {
                t: self.t,
                data: bytes.to_vec(),
            });
            self.t
        }

        fn resize(&mut self, dt: f64, cols: u16, rows: u16) -> f64 {
            self.t += dt;
            if self.pre_undo {
                self.live.grid_mut().drop_resize_undo();
            }
            self.live.resize(rows, cols);
            self.events.push(CastEvent::Resize {
                t: self.t,
                cols,
                rows,
            });
            self.t
        }

        /// A 64 -> 63 -> 64-shaped flap (rows -> rows-1 -> rows), 0.7 ms
        /// apart, split by output that draws nothing (a mode set): the app sees
        /// no size change, the output drops the resize undo, and the screen
        /// stays shifted. Returns the first resize's time.
        fn flap(&mut self, dt: f64) -> f64 {
            let (cols, rows) = (self.live.cols(), self.live.rows());
            let t = self.resize(dt, cols, rows - 1);
            self.out(0.0003, b"\x1b[?1000h");
            self.resize(0.0004, cols, rows);
            t
        }

        /// The same flap with NO output between its halves: the engine's
        /// resize undo hands the demoted row back, so nothing shifts.
        fn quiet_flap(&mut self, dt: f64) -> f64 {
            let (cols, rows) = (self.live.cols(), self.live.rows());
            let t = self.resize(dt, cols, rows - 1);
            self.resize(0.0007, cols, rows);
            t
        }

        fn screen(&self) -> LiveScreen {
            let c = self.live.cursor();
            LiveScreen {
                rows: (0..self.live.rows() as usize)
                    .map(|r| visible_row(&self.live, r))
                    .collect(),
                cursor: Some((c.row, c.col)),
                alt: Some(self.live.is_alternate_screen()),
            }
        }

        fn audit(&self, args: &DriftArgs) -> DriftReport {
            analyze(
                &self.header,
                &self.events,
                Some(&self.screen()),
                args,
                ResizePolicy::Native,
            )
        }
    }

    /// An audit folds the engine the session ran, console-host posture
    /// included. A ConPTY tab's live engine honors conhost's `?9001h`
    /// (`apply_pty_keyboard_posture`), so a killed pager's alternate screen
    /// is left the conhost way at the shell's next `133;A`. The replay, folded
    /// under `ResizePolicy::ConPty`, must leave it the same way, or every such
    /// Windows tab reads `unfaithful`. The native run is the control: the same
    /// recording on a Unix tab lays the screen out differently, so the ConPTY
    /// run exercises the conhost layout.
    #[test]
    fn an_audit_replays_the_console_host_posture_of_its_policy() {
        let (cols, rows) = (40u16, 10u16);
        let mut chunks: Vec<Vec<u8>> = vec![
            b"\x1b[?9001h\x1b[?1004h".to_vec(),
            b"history 1\r\nhistory 2\r\nC:\\>less file\r\n\x1b[?1049h\x1b[H\x1b[2J".to_vec(),
        ];
        for i in 1..rows {
            chunks.push(format!("pager {i}\r\n").into_bytes());
        }
        chunks.push(b"\x1b]133;D;-1\x07\x1b]133;A\x07C:\\>".to_vec());
        let mut cursors = Vec::new();
        for policy in [ResizePolicy::ConPty, ResizePolicy::Native] {
            let mut rec = Rec::new(cols, rows, false);
            rec.live.set_console_host_posture(policy);
            for chunk in &chunks {
                rec.out(0.01, chunk);
            }
            let screen = rec.screen();
            let report = analyze(
                &rec.header,
                &rec.events,
                Some(&screen),
                &DriftArgs::default(),
                policy,
            );
            assert_eq!(report.fidelity, Some(true), "{policy:?}: {report:?}");
            assert_eq!(report.cursor_live, screen.cursor, "{policy:?}");
            cursors.push(screen.cursor);
        }
        assert_ne!(
            cursors[0], cursors[1],
            "the recording must reach the conhost layout"
        );
    }

    /// Under OpenConsole the live engine leaves a killed pager's screen on the
    /// `?1049l` the host forwards, not at the prompt
    /// (`Terminal::set_host_resyncs_alt_screen`). A replay folded under that
    /// posture reproduces the live screen; the same recording under the bare
    /// policy (the prompt's local leave) reads `unfaithful`, the control.
    #[test]
    fn an_audit_replays_a_host_resync_under_the_sessions_posture() {
        let (cols, rows) = (40u16, 10u16);
        let mut rec = Rec::new(cols, rows, false);
        rec.live.set_console_host_posture(ResizePolicy::ConPty);
        rec.live.set_host_resyncs_alt_screen(true);
        rec.out(0.01, b"\x1b[?9001h\x1b[?1004h");
        rec.out(
            0.01,
            b"history 1\r\nC:\\>less file\r\n\x1b[?1049h\x1b[H\x1b[2J",
        );
        for i in 1..rows {
            rec.out(0.01, format!("pager {i}\r\n").as_bytes());
        }
        rec.out(0.01, b"\x1b]133;D;-1\x07\x1b]133;A\x07C:\\>");
        assert!(
            rec.live.take_alt_screen_resync_request(),
            "the host is asked"
        );
        // The resync client's switch, forwarded, then the shell's next line.
        rec.out(0.01, b"\x1b[?1049l");
        rec.out(0.01, b"echo x\r\nx\r\n\x1b]133;D;0\x07\x1b]133;A\x07C:\\>");
        assert!(!rec.live.is_alternate_screen());
        let screen = rec.screen();
        let args = DriftArgs::default();
        let posture = Posture {
            policy: ResizePolicy::ConPty,
            host_resyncs_alt_screen: true,
        };
        let report = analyze(&rec.header, &rec.events, Some(&screen), &args, posture);
        assert_eq!(report.fidelity, Some(true), "{report:?}");
        let local = analyze(
            &rec.header,
            &rec.events,
            Some(&screen),
            &args,
            ResizePolicy::ConPty,
        );
        assert_eq!(local.fidelity, Some(false), "the control: {local:?}");
    }

    /// A Claude-shaped 8-row full repaint: synchronized, `ESC[2J`, every row by
    /// absolute CUP, the cursor parked on the composer (row 5) with a non-blank
    /// footer below it (rows 6-7). `rows_drawn` < 8 leaves the tail blank.
    fn frame(tag: &str, rows_drawn: usize) -> Vec<u8> {
        let mut s = String::from("\x1b[?2026h\x1b[2J\x1b[H");
        for r in 0..rows_drawn {
            s.push_str(&format!("\x1b[{};1H{tag}{r}", r + 1));
        }
        s.push_str("\x1b[6;3H\x1b[?2026l");
        s.into_bytes()
    }

    /// A diff-only frame, the renderer's steady state: CUP + EL + text + SGR,
    /// never a clear.
    fn diff(row: usize, text: &str) -> Vec<u8> {
        format!(
            "\x1b[?2026h\x1b[{};1H\x1b[K\x1b[1m{text}\x1b[22m\x1b[6;3H\x1b[?2026l",
            row + 1
        )
        .into_bytes()
    }

    fn rows_of(report: &DriftReport) -> Vec<usize> {
        report.drift.iter().map(|d| d.row).collect()
    }

    /// An alt-screen app draws a frame with its cursor above a non-blank
    /// footer, the window flaps 8 -> 7 -> 8 with only a mode set between, and
    /// the app sends a diff. Every row but the diffed one is shifted up one;
    /// the audit names the flap and exactly those rows, with what the app
    /// believes they say.
    #[test]
    fn drift_finds_a_net_zero_flap_on_the_alt_screen() {
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 8));
        let flap = rec.flap(1.0);
        rec.out(0.005, &diff(2, "B2"));
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
        assert_eq!(report.fidelity, Some(true));
        assert_eq!(report.culprit_word(), fmt_t(flap));
        assert_eq!(report.candidates.len(), 1);
        assert!(report.candidates[0].net_zero());
        assert_eq!(rows_of(&report), vec![0, 1, 3, 4, 5, 6, 7]);
        // Expected = the no-resize fold: what the app believes it drew.
        let expected: Vec<String> = report.drift.iter().map(|d| d.other.clone()).collect();
        assert_eq!(expected, ["A0", "A1", "A3", "A4", "A5", "A6", "A7"]);
        assert_eq!(
            report.drift[6].live, "",
            "the grow appended a blank bottom row"
        );
        assert_eq!(
            report.drift[0].live, "A1",
            "the top row was demoted and dropped"
        );
        assert!(
            !report.seed_alt,
            "the recording enters the alt screen itself, so the replay starts on primary"
        );
        assert_eq!(report.folds, 2, "FAITHFUL + STABLE; ONLY is STABLE here");
        // The wire: the header counts the lines that follow, every line is one
        // `kind k=v…` record.
        let wire = report.render(7, None);
        let mut lines = wire.lines();
        let head = lines.next().unwrap();
        let n: usize = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        assert_eq!(lines.clone().count(), n);
        assert!(head.starts_with(&format!(
            "OK {n} verdict=desync fidelity=pass against=screen drift_rows=7 runs=1 candidates=1 \
             capped=0 culprit={} anchor=",
            fmt_t(flap)
        )));
        assert!(head.ends_with(
            " seed=primary seed_basis=toggle geom=20x8 evicted=0 dropped=0 cut=- folds=2 \
             fold_events=10 ms=7"
        ));
        assert_eq!(
            lines.next().unwrap(),
            format!(
                "run t={} n=2 gap_ms=0.700 geom=20x8>20x7>20x8 net=zero first_out_ms=5.000 \
                 clear_ms=- culprit=1",
                fmt_t(flap)
            )
        );
        assert_eq!(lines.next().unwrap(), "row 0 live=A1 expected=A0");
        assert_eq!(lines.last().unwrap(), "cursor live=5,2 expected=5,2");
    }

    /// The same flap with nothing between its halves: the resize undo hands
    /// the row back, the live screen is what the app believes it drew, and the
    /// audit finds nothing to attribute.
    #[test]
    fn a_quiet_flap_is_undone_and_drift_is_clean() {
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 8));
        rec.quiet_flap(1.0);
        rec.out(0.005, &diff(2, "B2"));
        let live = rec.screen();
        assert_eq!(live.rows[0], "A0", "the demoted row came back");
        assert_eq!(live.rows[7], "A7", "no blank row appended");
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
        assert_eq!(report.fidelity, Some(true));
        assert_eq!(report.candidates.len(), 1, "still a net-zero candidate");
        assert!(report.candidates[0].net_zero());
        assert!(report.drift.is_empty());

        // A real flap, split by a mode set, after the quiet one: the journal
        // blames only the split one (the quiet one's rows came back).
        let split = rec.flap(1.0);
        rec.out(0.005, &diff(2, "B3"));
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
        assert_eq!(report.culprit_word(), fmt_t(split));
    }

    /// A flap split in time: the halves land 1.6 s apart with a mode set
    /// between, and the app (whose handler ran after the size came back) sends
    /// a diff. No run holds both halves; the shrink is lossy in the faithful
    /// journal and the grow restores the geometry, so the span is a return
    /// candidate and the audit names it.
    #[test]
    fn a_flap_time_split_is_a_return_and_is_named() {
        for gap in [1.6, 0.4] {
            let mut rec = Rec::new(20, 8, false);
            rec.out(0.1, b"\x1b[?1049h");
            rec.out(0.1, &frame("A", 8));
            let shrink = rec.resize(1.0, 20, 7);
            rec.out(0.0003, b"\x1b[?1004h");
            rec.resize(gap, 20, 8);
            rec.out(0.005, &diff(2, "B2"));
            let report = rec.audit(&DriftArgs::default());
            assert_eq!(report.verdict, Verdict::Desync, "gap {gap}: {report:?}");
            assert_eq!(report.fidelity, Some(true));
            assert_eq!((report.runs_total, report.eligible), (2, 1), "{report:?}");
            assert_eq!(report.culprit_word(), fmt_t(shrink));
            let run = &report.candidates[0];
            assert!(run.net_zero() && run.culprit);
            assert_eq!(run.geoms, [(20, 8), (20, 7), (20, 8)]);
            assert!(run.gap_ms > RUN_GAP_SECS * 1000.0, "{}", run.gap_ms);
            assert_eq!(rows_of(&report), vec![0, 1, 3, 4, 5, 6, 7]);
            assert_eq!(report.drift[0].live, "A1", "shifted up one row");
            assert_eq!(report.drift[0].other, "A0");
            assert_eq!(report.folds, 2, "FAITHFUL + STABLE; the journal attributes");
            let wire = report.render(0, None);
            assert!(
                wire.contains(" geom=20x8>20x7>20x8 net=zero first_out_ms=5.000 "),
                "{wire}"
            );
            assert!(!wire.contains(" undo=off"), "{wire}");
        }
    }

    /// Controls for returns: (1) an app that repainted every row at the
    /// smaller size and again at the restored size, without clearing: a
    /// candidate that collapses to the same screen, `clean`; (2) nothing output
    /// between the halves: the undo restored the row, no candidate; (3) a
    /// shrink that stays: replayed as recorded, `clean`, no candidate.
    #[test]
    fn a_return_the_app_repainted_through_is_clean_and_a_kept_size_is_no_candidate() {
        let repaint = |rows: usize| {
            let mut s = String::from("\x1b[?2026h");
            for r in 0..rows {
                s.push_str(&format!("\x1b[{};1H\x1b[KR{r}", r + 1));
            }
            s.push_str("\x1b[6;3H\x1b[?2026l");
            s.into_bytes()
        };
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 8));
        rec.resize(1.0, 20, 7);
        rec.out(0.02, &repaint(7));
        rec.resize(1.6, 20, 8);
        rec.out(0.02, &repaint(8));
        rec.out(0.5, &diff(2, "B2"));
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
        assert_eq!(report.eligible, 1, "the span is weighed: {report:?}");

        let mut quiet = Rec::new(20, 8, false);
        quiet.out(0.1, b"\x1b[?1049h");
        quiet.out(0.1, &frame("A", 8));
        quiet.resize(1.0, 20, 7);
        quiet.resize(1.6, 20, 8);
        quiet.out(0.005, &diff(2, "B2"));
        assert_eq!(quiet.screen().rows[0], "A0", "the undo handed the row back");
        let report = quiet.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
        assert_eq!((report.runs_total, report.eligible), (2, 0));

        let mut kept = Rec::new(20, 8, false);
        kept.out(0.1, b"\x1b[?1049h");
        kept.out(0.1, &frame("A", 8));
        kept.resize(1.0, 20, 7);
        kept.out(0.005, &diff(2, "B2"));
        let report = kept.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
        assert_eq!((report.runs_total, report.eligible), (1, 0));
    }

    /// A recording from an aterm without the resize undo, held against its
    /// screen: this engine's replay is `unfaithful`. Replayed with
    /// `Undo::Drop` it is faithful and the flap is named (`undo=off`);
    /// `Undo::Match` takes that replay only because it alone reproduces the
    /// screen, and keeps the undo for a screen this engine made.
    #[test]
    fn a_recording_from_before_the_undo_replays_without_it() {
        let mut old = Rec::new(20, 8, false);
        old.pre_undo = true;
        old.out(0.1, b"\x1b[?1049h");
        old.out(0.1, &frame("A", 8));
        let flap = old.quiet_flap(1.0);
        old.out(0.005, &diff(2, "B2"));
        assert_eq!(old.screen().rows[0], "A1", "the older engine shifted it");

        let kept = old.audit(&DriftArgs::default());
        assert_eq!(kept.verdict, Verdict::Unfaithful, "{kept:?}");
        assert!(!kept.undo_dropped);
        for undo in [Undo::Drop, Undo::Match] {
            let report = old.audit(&DriftArgs {
                undo,
                ..DriftArgs::default()
            });
            assert_eq!(report.verdict, Verdict::Desync, "{undo:?}: {report:?}");
            assert_eq!(report.fidelity, Some(true));
            assert_eq!(report.culprit_word(), fmt_t(flap));
            assert!(report.undo_dropped);
            assert_eq!(rows_of(&report), vec![0, 1, 3, 4, 5, 6, 7]);
            let wire = report.render(3, Some("client"));
            assert!(
                wire.lines()
                    .next()
                    .unwrap()
                    .ends_with(" ms=3 undo=off computed=client"),
                "{wire}"
            );
            // Match folded both engines (the kept one only its faithful
            // replay: it failed the fidelity gate); Drop only its own.
            let folds = if undo == Undo::Match { 3 } else { 2 };
            assert_eq!(report.folds, folds, "{undo:?}");
        }

        // This engine's screen of the same session: the undo is kept. Its
        // recording carries the key, and says nothing of a law on the wire.
        let mut new = Rec::new(20, 8, false);
        new.header.resize_undo = Some(true);
        new.out(0.1, b"\x1b[?1049h");
        new.out(0.1, &frame("A", 8));
        new.quiet_flap(1.0);
        new.out(0.005, &diff(2, "B2"));
        let report = new.audit(&DriftArgs {
            undo: Undo::Match,
            ..DriftArgs::default()
        });
        assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
        assert!(!report.undo_dropped);
        assert_eq!(report.folds, 2, "one engine: its fidelity passed");
        assert!(!report.render(3, None).contains("undo="));
        // With no screen there is nothing to match: the undo is kept.
        let blind = analyze(
            &old.header,
            &old.events,
            None,
            &DriftArgs {
                undo: Undo::Match,
                ..DriftArgs::default()
            },
            ResizePolicy::Native,
        );
        assert!(!blind.undo_dropped);
        assert_eq!(blind.verdict, Verdict::Clean, "{blind:?}");
    }

    /// A recording that SAYS its engine kept the undo (`aterm_resize_undo`) is
    /// never replayed without it. An engine whose undo failed to hand a row back
    /// makes exactly the recording the historical law would reproduce, so
    /// matching it would hide the regression: it stays `unfaithful`, with no
    /// culprit and no `undo=off`.
    #[test]
    fn a_keyed_recording_is_never_replayed_without_the_undo() {
        let mut broken = Rec::new(20, 8, false);
        broken.header.resize_undo = Some(true);
        broken.pre_undo = true;
        broken.out(0.1, b"\x1b[?1049h");
        broken.out(0.1, &frame("A", 8));
        broken.quiet_flap(1.0);
        broken.out(0.005, &diff(2, "B2"));
        for undo in [Undo::Keep, Undo::Match] {
            let report = broken.audit(&DriftArgs {
                undo,
                ..DriftArgs::default()
            });
            assert_eq!(report.verdict, Verdict::Unfaithful, "{undo:?}: {report:?}");
            assert!(!report.undo_dropped, "{undo:?}");
            assert_eq!(
                report.folds, 1,
                "{undo:?}: no second law was tried, and no variant after the gate failed"
            );
            let wire = report.render(3, Some("client"));
            assert!(!wire.contains("undo=off"), "{wire}");
            assert!(!report.candidates.iter().any(|c| c.culprit), "{report:?}");
        }
        // The same recording without the key: an older aterm, matched to its
        // screen.
        broken.header.resize_undo = None;
        let report = broken.audit(&DriftArgs {
            undo: Undo::Match,
            ..DriftArgs::default()
        });
        assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
        assert!(report.undo_dropped);
    }

    /// A primary screen with history and a blank
    /// row under the cursor, flapped a row with nothing between. This engine
    /// hands the trimmed blank back, so its recording (law 2) replays faithfully
    /// and nothing moved: `clean`. A recording from an aterm before the
    /// hand-back (law 1) shifted every row down one; replayed under its own law
    /// (the trims forgotten) it is faithful, and the flap is named.
    #[test]
    fn a_primary_trim_flap_replays_under_the_law_its_recording_names() {
        let record = |older: bool| {
            let mut rec = Rec::new(20, 8, false);
            rec.header.resize_undo = Some(true);
            if older {
                rec.header.resize_laws = &[ResizeLaw::Undo];
                rec.live.set_resize_law(ResizeLaw::Undo);
            }
            for i in 0..20 {
                rec.out(0.01, format!("h{i}\r\n").as_bytes());
            }
            rec.out(0.1, b"\x1b[H\x1b[2Jtop\r\n");
            let mut flap = 0.0;
            for (dt, rows) in [(1.0, 7), (0.0007, 8)] {
                let t = rec.resize(dt, 20, rows);
                if rows == 7 {
                    flap = t;
                }
            }
            (rec, flap)
        };
        let (new, _) = record(false);
        assert_eq!(new.screen().rows[0], "top", "the blank came back");
        let report = new.audit(&DriftArgs::default());
        assert_eq!(report.fidelity, Some(true), "{report:?}");
        assert_eq!(report.verdict, Verdict::Clean, "{report:?}");

        let (old, flap) = record(true);
        assert_eq!(old.screen().rows[1], "top", "the older law moved every row");
        let report = old.audit(&DriftArgs::default());
        assert_eq!(report.fidelity, Some(true), "{report:?}");
        assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
        assert_eq!(report.culprit_word(), fmt_t(flap));
    }

    /// A recording replays under the resize law its aterm laid the rows out
    /// under, which its header names (`aterm_resize_undo`), so the rows a law
    /// moved are attributed to the flap that moved them, never to the replay. A
    /// full screen with the cursor homed flapped a row (the bottom-push corner),
    /// and a zoom in then out over a line that wraps at the narrower width (the
    /// rewrap corner): the first law that wrote 2 rotated the pushed row to the
    /// top, both that wrote 2 moved the rows the rewrap moved, and the current
    /// law (3) moves neither. Each recording, with the header its aterm wrote,
    /// replays faithfully, 2 under whichever of its two laws reproduces the
    /// screen, and a flap that moved rows is named. Replayed under the current
    /// law instead, the older ones read `unfaithful`.
    #[test]
    fn a_recording_replays_under_the_resize_law_its_header_names() {
        /// A corner's name, what paints it, and the flap's sizes (cols, rows).
        type Corner = (&'static str, &'static [u8], &'static [(u16, u16)]);
        let corners: [Corner; 2] = [
            (
                "bottom push",
                b"\x1b[H\x1b[2JP0\r\nP1\r\nP2\r\nP3\r\nP4\r\nP5\r\nP6\r\nP7\x1b[H",
                &[(20, 7), (20, 8)],
            ),
            (
                "rewrap",
                b"\x1b[H\x1b[2Jtop\r\nLLLLLLLLLLLLLLLLLLL1\r\nend\r\n",
                &[(19, 7), (20, 8)],
            ),
        ];
        for (corner, paint, path) in corners {
            for law in [
                ResizeLaw::HandBack,
                ResizeLaw::PushedRows,
                ResizeLaw::Rewrap,
            ] {
                let mut rec = Rec::new(20, 8, false);
                rec.live.set_resize_law(law);
                rec.header.resize_undo = Some(true);
                rec.header.resize_laws = CastHeader::laws_named(Some(law.header_value()));
                for i in 0..20 {
                    rec.out(0.01, format!("h{i}\r\n").as_bytes());
                }
                rec.out(0.1, paint);
                let before = rec.screen();
                let mut flap = None;
                let mut dt = 1.0;
                for &(cols, rows) in path {
                    flap.get_or_insert(rec.resize(dt, cols, rows));
                    dt = 0.0007;
                }
                let moved = rec.screen() != before;
                let older = match law {
                    ResizeLaw::HandBack => true,
                    ResizeLaw::PushedRows => corner == "rewrap",
                    _ => false,
                };
                assert_eq!(moved, older, "{corner}, {law:?}");
                let report = rec.audit(&DriftArgs::default());
                assert_eq!(report.fidelity, Some(true), "{corner}, {law:?}: {report:?}");
                if moved {
                    assert_eq!(report.verdict, Verdict::Desync, "{corner}, {law:?}");
                    assert_eq!(report.culprit_word(), fmt_t(flap.unwrap_or_default()));
                } else {
                    assert_eq!(report.verdict, Verdict::Clean, "{corner}, {law:?}");
                }
                rec.header.resize_laws = &[ResizeLaw::Rewrap];
                let current = rec.audit(&DriftArgs::default());
                if older {
                    assert_eq!(current.fidelity, Some(false), "{corner}, {law:?}");
                    assert_eq!(current.verdict, Verdict::Unfaithful, "{corner}, {law:?}");
                }
            }
        }
    }

    /// With no screen, a recording whose header names two laws
    /// (`aterm_resize_undo: 2`) is replayed under both, as one with no toggle
    /// is on both seeds: the bottom-push flap an aterm of the older one
    /// recorded moved its rows under that law and none under the newer, so
    /// the answer is `unknown`, `law=ambiguous`, where the newer law alone
    /// (all an offline replay read) called it clean. The screen settles it,
    /// and a recording whose header names one law is answered by it.
    #[test]
    fn with_no_screen_two_laws_that_disagree_answer_unknown() {
        let primary = DriftArgs {
            seed: Seed::Primary,
            ..DriftArgs::default()
        };
        let record = |law: ResizeLaw| {
            let mut rec = Rec::new(20, 8, false);
            rec.live.set_resize_law(law);
            rec.header.resize_undo = Some(true);
            rec.header.resize_laws = CastHeader::laws_named(Some(law.header_value()));
            for i in 0..20 {
                rec.out(0.01, format!("h{i}\r\n").as_bytes());
            }
            rec.out(
                0.1,
                b"\x1b[H\x1b[2JP0\r\nP1\r\nP2\r\nP3\r\nP4\r\nP5\r\nP6\r\nP7\x1b[H",
            );
            let flap = rec.resize(1.0, 20, 7);
            rec.resize(0.0007, 20, 8);
            (rec, flap)
        };
        let offline = |header: &CastHeader, rec: &Rec| {
            analyze(header, &rec.events, None, &primary, ResizePolicy::Native)
        };
        let (rec, flap) = record(ResizeLaw::HandBack);
        let blind = offline(&rec.header, &rec);
        assert_eq!(blind.verdict, Verdict::Unknown, "{blind:?}");
        assert!(blind.law_ambiguous && blind.drift.is_empty(), "{blind:?}");
        assert_eq!(blind.culprit_word(), "-");
        assert!(blind.render(0, None).contains(" ms=0 law=ambiguous\n"));
        for (law, verdict) in [
            (ResizeLaw::PushedRows, Verdict::Clean),
            (ResizeLaw::HandBack, Verdict::Desync),
        ] {
            let mut alone = rec.header.clone();
            alone.resize_laws = std::slice::from_ref(match law {
                ResizeLaw::PushedRows => &ResizeLaw::PushedRows,
                _ => &ResizeLaw::HandBack,
            });
            let report = offline(&alone, &rec);
            assert_eq!(report.verdict, verdict, "{law:?}: {report:?}");
            assert!(!report.law_ambiguous);
        }
        let seen = rec.audit(&primary);
        assert_eq!(seen.verdict, Verdict::Desync, "{seen:?}");
        assert_eq!(seen.culprit_word(), fmt_t(flap));
        assert!(!seen.law_ambiguous);
        let (rec, _) = record(ResizeLaw::Rewrap);
        let blind = offline(&rec.header, &rec);
        assert_eq!(blind.verdict, Verdict::Clean, "{blind:?}");
        assert!(!blind.law_ambiguous);
    }

    /// The same screen flapped by a drag that grows first (9 then 8 rows) or
    /// overshoots on the way back (7, 9, 8): the grow reveals history and the
    /// shrink back demotes it again, so this engine's recording replays
    /// faithfully and nothing moved.
    #[test]
    fn a_primary_flap_that_grows_first_replays_clean() {
        for heights in [&[9u16, 8][..], &[7, 9, 8]] {
            let mut rec = Rec::new(20, 8, false);
            rec.header.resize_undo = Some(true);
            for i in 0..20 {
                rec.out(0.01, format!("h{i}\r\n").as_bytes());
            }
            rec.out(0.1, b"\x1b[H\x1b[2Jtop\r\n");
            let mut dt = 1.0;
            for &rows in heights {
                rec.resize(dt, 20, rows);
                dt = 0.0007;
            }
            assert_eq!(rec.screen().rows[0], "top", "{heights:?}: nothing moved");
            let report = rec.audit(&DriftArgs::default());
            assert_eq!(report.fidelity, Some(true), "{heights:?}: {report:?}");
            assert_eq!(report.verdict, Verdict::Clean, "{heights:?}: {report:?}");
        }
    }

    /// The same screen flapped by a font zoom in then out, a zoom out then in, and
    /// a corner drag brought back: the rows and the columns move together, the
    /// recording replays faithfully, and nothing moved.
    #[test]
    fn a_primary_zoom_flap_replays_clean() {
        let paths: [&[(u16, u16)]; 3] = [
            &[(19, 7), (20, 8)],
            &[(21, 9), (20, 8)],
            &[(20, 7), (19, 7), (19, 8), (20, 8)],
        ];
        for path in paths {
            let mut rec = Rec::new(20, 8, false);
            rec.header.resize_undo = Some(true);
            for i in 0..20 {
                rec.out(0.01, format!("h{i}\r\n").as_bytes());
            }
            rec.out(0.1, b"\x1b[H\x1b[2Jtop\r\n");
            let mut dt = 1.0;
            for &(cols, rows) in path {
                rec.resize(dt, cols, rows);
                dt = 0.0007;
            }
            assert_eq!(rec.screen().rows[0], "top", "{path:?}: nothing moved");
            let report = rec.audit(&DriftArgs::default());
            assert_eq!(report.fidelity, Some(true), "{path:?}: {report:?}");
            assert_eq!(report.verdict, Verdict::Clean, "{path:?}: {report:?}");
        }
    }

    /// Claude's healthy case (82 of 83 measured flaps): the app DID see the
    /// size change and repainted with `ESC[2J`, which replaces the displaced
    /// screen. The clear is the anchor, so the flap is not even a candidate.
    #[test]
    fn drift_is_clean_when_ed2_follows_the_flap() {
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 8));
        rec.flap(1.0);
        rec.out(0.005, &frame("C", 8));
        rec.out(0.5, &diff(2, "B2"));
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
        assert_eq!(report.fidelity, Some(true));
        assert_eq!(report.candidates.len(), 0);
        assert_eq!(report.runs_total, 1);
        assert_eq!(report.folds, 1);
        assert!(report.render(0, None).starts_with("OK 0 verdict=clean"));
    }

    /// Negative control: with blank rows below the cursor the shrink only trims
    /// them and the grow appends one back; the flap is a candidate, found
    /// harmless.
    #[test]
    fn drift_is_clean_when_the_tail_is_blank() {
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 6));
        rec.flap(1.0);
        rec.out(0.005, &diff(2, "B2"));
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
        assert_eq!(report.candidates.len(), 1);
        assert!(!report.candidates[0].culprit);
        assert_eq!(report.culprit_word(), "-");
    }

    /// The fidelity gate: a recording that lost a burst cannot reproduce the
    /// screen and says so (`unfaithful`, rows as live/replayed), never accusing
    /// the flap beside the gap.
    #[test]
    fn drift_reports_unfaithful_not_desync_when_a_burst_is_missing() {
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 8));
        rec.flap(1.0);
        rec.out(0.005, &diff(2, "B2"));
        rec.events.pop();
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Unfaithful, "{report:?}");
        assert_eq!(report.fidelity, Some(false));
        assert_eq!(report.culprit_word(), "-");
        assert_eq!(rows_of(&report), vec![2]);
        let wire = report.render(0, None);
        assert!(wire.contains("row 2 live=B2 replayed=A3\n"), "{wire}");
        assert!(wire.contains("cursor live=5,2 replayed="), "{wire}");
        // A racy cut downgrades it to `unknown`, never upgrades it.
        let mut racy = report.clone();
        racy.settle_cut(Cut::Racy);
        assert_eq!(racy.verdict, Verdict::Unknown);
    }

    /// 83 flaps, the first 82 healed by a repaint: the anchor (the last clear)
    /// bounds the search to the last run, the one culprit, in two folds.
    #[test]
    fn drift_attributes_the_one_culprit_among_many_healed_flaps() {
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 8));
        for k in 0..82 {
            rec.flap(60.0);
            rec.out(0.005, &frame(if k % 2 == 0 { "C" } else { "A" }, 8));
        }
        let last = rec.flap(60.0);
        rec.out(0.005, &diff(2, "B2"));
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
        assert_eq!(report.runs_total, 83);
        assert_eq!(report.candidates.len(), 1, "the anchor bounds the work");
        assert_eq!(report.culprit_word(), fmt_t(last));
        assert!(report.folds <= 3, "folds={}", report.folds);
        // With the cap at zero no run is attributed alone, but the verdict
        // still weighs every candidate, and a culprit is always listed.
        let capped = rec.audit(&DriftArgs {
            max_runs: 0,
            ..DriftArgs::default()
        });
        assert_eq!(capped.verdict, Verdict::Desync);
        assert_eq!((capped.eligible, capped.capped), (1, 0));
        assert_eq!(capped.culprit_word(), fmt_t(last));
    }

    /// An adopted session's recording starts mid-app (no `?1049h`) while the
    /// live engine is on the alt screen. The seed follows the live screen, so
    /// the replay is faithful; forced to primary (where a flap is lossless) it
    /// fails the fidelity gate.
    #[test]
    fn drift_seeds_alt_for_an_adopted_cast() {
        let mut rec = Rec::new(20, 8, true);
        rec.out(0.1, &frame("A", 8));
        let flap = rec.flap(1.0);
        rec.out(0.005, &diff(2, "B2"));
        let report = rec.audit(&DriftArgs::default());
        assert!(report.seed_alt);
        assert_eq!(report.fidelity, Some(true), "{report:?}");
        assert_eq!(report.verdict, Verdict::Desync);
        assert_eq!(report.culprit_word(), fmt_t(flap));
        assert_eq!(report.seed_basis, SeedBasis::Live);
        let forced = rec.audit(&DriftArgs {
            seed: Seed::Primary,
            ..DriftArgs::default()
        });
        assert!(!forced.seed_alt);
        assert_eq!(forced.seed_basis, SeedBasis::Arg);
        assert_eq!(forced.verdict, Verdict::Unfaithful);
        // A saved screen dump says nothing about its buffer: the fidelity gate
        // picks the seed that reproduces it.
        let mut dump = rec.screen();
        dump.alt = None;
        dump.cursor = None;
        let chosen = analyze(
            &rec.header,
            &rec.events,
            Some(&dump),
            &DriftArgs::default(),
            ResizePolicy::Native,
        );
        assert_eq!(chosen.seed_basis, SeedBasis::Fidelity, "{chosen:?}");
        assert!(chosen.seed_alt);
        assert_eq!(chosen.verdict, Verdict::Desync);
        assert_eq!(chosen.folds, 3, "primary FAITHFUL, alt FAITHFUL, STABLE");
        // No screen: the primary replay calls it clean, the alt one a desync,
        // so the answer is `unknown`, `seed=ambiguous`; `seed=alt` settles it.
        let blind = analyze(
            &rec.header,
            &rec.events,
            None,
            &DriftArgs::default(),
            ResizePolicy::Native,
        );
        assert_eq!(blind.seed_basis, SeedBasis::Default);
        assert_eq!(blind.verdict, Verdict::Unknown, "{blind:?}");
        assert!(blind.seed_ambiguous && blind.drift.is_empty());
        assert_eq!(blind.culprit_word(), "-");
        assert_eq!(blind.folds, 4, "FAITHFUL + STABLE on each seed");
        assert!(
            blind
                .render(0, None)
                .contains(" verdict=unknown fidelity=- against=replay drift_rows=0 ")
        );
        assert!(
            blind
                .render(0, None)
                .contains(" seed=ambiguous seed_basis=default ")
        );
        let told = analyze(
            &rec.header,
            &rec.events,
            None,
            &DriftArgs {
                seed: Seed::Alt,
                ..DriftArgs::default()
            },
            ResizePolicy::Native,
        );
        assert_eq!(told.verdict, Verdict::Desync);
        assert_eq!(told.culprit_word(), fmt_t(flap));
    }

    /// Two candidates, one harmless (blank tail) and one destructive: the
    /// journal names the destructive one alone, with no attribution fold.
    #[test]
    fn drift_attributes_between_two_candidates() {
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 6));
        rec.flap(1.0);
        rec.out(0.005, b"\x1b[7;1HF6\x1b[8;1HF7\x1b[6;3H");
        let bad = rec.flap(1.0);
        rec.out(0.005, &diff(2, "B2"));
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
        assert_eq!(report.candidates.len(), 2);
        assert_eq!(report.culprit_word(), fmt_t(bad));
        assert!(!report.candidates[0].culprit && report.candidates[1].culprit);
        assert_eq!(report.folds, 2, "FAITHFUL + STABLE: the journal attributes");
    }

    /// Output between the flap's halves (a spinner tick, or a burst stamped
    /// late by the recorder) does not split the run; it stays one net-zero
    /// candidate and the desync is named.
    #[test]
    fn a_draw_between_the_halves_does_not_hide_the_flap() {
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 8));
        let (cols, rows) = (rec.live.cols(), rec.live.rows());
        let t = rec.resize(1.0, cols, rows - 1);
        rec.out(0.004, b"\x1b[8;1H\x1b[Kspin\x1b[6;3H");
        rec.resize(0.002, cols, rows);
        rec.out(0.005, &diff(2, "B2"));
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
        assert_eq!(report.fidelity, Some(true));
        assert_eq!((report.runs_total, report.eligible), (1, 1));
        assert!(report.candidates[0].net_zero());
        assert_eq!(report.culprit_word(), fmt_t(t));
        assert_eq!(report.drift[0].live, "A1", "shifted up one row");
    }

    /// Nine flaps with no clear between: the first demotes the top row, the
    /// rest only trim the blank row it left. Every candidate is weighed (the
    /// culprit is the oldest, past the default `max_runs` of eight), the
    /// journal names it alone, and a culprit is always listed.
    #[test]
    fn every_candidate_is_weighed_and_the_journal_names_the_culprit() {
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 8));
        let first = rec.flap(1.0);
        for _ in 0..8 {
            rec.flap(1.0);
        }
        rec.out(0.005, &diff(2, "B2"));
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
        assert_eq!((report.eligible, report.capped), (9, 0));
        assert_eq!(report.culprit_word(), fmt_t(first));
        assert_eq!(report.folds, 2, "no attribution fold");
        let narrow = rec.audit(&DriftArgs {
            max_runs: 2,
            ..DriftArgs::default()
        });
        assert_eq!(narrow.verdict, Verdict::Desync);
        assert_eq!((narrow.candidates.len(), narrow.capped), (3, 6));
        assert!(narrow.candidates[0].culprit, "the culprit is listed first");
        let none = rec.audit(&DriftArgs {
            max_runs: 0,
            ..DriftArgs::default()
        });
        assert_eq!(none.verdict, Verdict::Desync);
        assert_eq!((none.candidates.len(), none.capped), (1, 8));
        assert!(
            none.render(0, None)
                .contains(" runs=9 candidates=9 capped=8 culprit=")
        );
    }

    /// With no lossy alt-screen resize (the primary screen of an aterm before
    /// the rows hand-back, law 1, where a shrink with the cursor on row 0
    /// pushes the bottom row into history and the grow reveals it on top),
    /// attribution falls back to ONLY(r) folds over the newest `max_runs`.
    #[test]
    fn a_primary_rotation_is_attributed_by_the_only_folds() {
        let mut rec = Rec::new(20, 8, false);
        rec.header.resize_laws = &[ResizeLaw::Undo];
        rec.live.set_resize_law(ResizeLaw::Undo);
        let mut paint = String::new();
        for r in 0..8 {
            paint.push_str(&format!("\x1b[{};1HP{r}", r + 1));
        }
        paint.push_str("\x1b[H");
        rec.out(0.1, paint.as_bytes());
        rec.flap(1.0);
        // The rotation moved the cursor to row 1; the app homes it again.
        rec.out(0.005, b"\x1b[H");
        rec.flap(1.0);
        rec.out(0.005, b"\x1b[3;1H\x1b[KQ2\x1b[H");
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
        assert!(!report.seed_alt);
        assert_eq!(report.eligible, 2);
        assert_eq!(report.folds, 4, "FAITHFUL + STABLE + ONLY x2: {report:?}");
        assert!(
            report.candidates.iter().all(|run| run.culprit),
            "each rotation contributes: {report:?}"
        );
        let unattributed = rec.audit(&DriftArgs {
            max_runs: 0,
            ..DriftArgs::default()
        });
        assert_eq!(unattributed.verdict, Verdict::Desync);
        assert_eq!(
            (unattributed.folds, unattributed.culprit_word()),
            (2, "-".to_string())
        );
    }

    /// The variants fold from a fork of the faithful replay taken just after
    /// the anchor event, and `Variants::fold` holds every such fold in this
    /// module's tests to the variant's fold from the start. Here the fork is
    /// taken: with a clear first, the stable fold and both ONLY folds fold only
    /// the events after it; without one, every variant folds them all, to the
    /// same answer. Negative control: past the first candidate's first resize
    /// its hand-back stands, which a copy cannot hold, so no fork is taken
    /// there.
    #[test]
    fn forked_variants_equal_full_folds() {
        // Two primary-screen rotations, which the journal cannot name, so the
        // ONLY folds run (`a_primary_rotation_is_attributed_by_the_only_folds`):
        // a recording of an aterm before the rows hand-back, law 1.
        let record = |clear: &str| {
            let mut rec = Rec::new(20, 8, false);
            rec.header.resize_laws = &[ResizeLaw::Undo];
            rec.live.set_resize_law(ResizeLaw::Undo);
            let mut paint = String::from(clear);
            for r in 0..8 {
                paint.push_str(&format!("\x1b[{};1HP{r}", r + 1));
            }
            paint.push_str("\x1b[H");
            rec.out(0.1, paint.as_bytes());
            rec.flap(1.0);
            rec.out(0.005, b"\x1b[H");
            rec.flap(1.0);
            rec.out(0.005, b"\x1b[3;1H\x1b[KQ2\x1b[H");
            rec
        };
        let anchored = record("\x1b[2J");
        let n = anchored.events.len();
        let forked = anchored.audit(&DriftArgs::default());
        assert_eq!(forked.anchor, Some(anchored.events[0].t()), "{forked:?}");
        assert_eq!(forked.folds, 4, "FAITHFUL + STABLE + ONLY x2: {forked:?}");
        assert_eq!(
            forked.fold_events,
            n + 3 * (n - 1),
            "each variant folds the events after the anchor"
        );
        let full = record("").audit(&DriftArgs::default());
        assert_eq!(full.anchor, None);
        assert_eq!((full.folds, full.fold_events), (4, 4 * n));
        assert_eq!(
            (forked.verdict, forked.culprit_word(), &forked.drift),
            (full.verdict, full.culprit_word(), &full.drift)
        );

        let (header, events) = (&anchored.header, &anchored.events);
        let engine = Engine {
            policy: ResizePolicy::Native,
            host_resyncs: false,
            keep_undo: true,
            law: ResizeLaw::Undo,
        };
        let runs = group_runs(header, events, &scan_marks(events).clears);
        let mask = apply_mask(events, &runs.iter().collect::<Vec<_>>());
        let (stable, _) = fold(header, events, false, Some(&mask), engine, false, None);
        let first = runs[0].first;
        let at = |k: usize| {
            let (_, fork) = fold(header, events, false, None, engine, true, Some(k));
            fold_rest(fork.expect("forks"), events, k + 1, Some(&mask), engine)
        };
        assert_eq!(at(first - 1), stable, "forked at the anchor");
        // After the output between the flap's halves, which dropped the undo,
        // the resize's hand-back still stands: a copy could not hold it, so no
        // fork is taken there (`Grid::replay_fork`) and the variant folds whole.
        let (_, past) = fold(header, events, false, None, engine, true, Some(first + 1));
        assert!(
            past.is_none(),
            "no fork past a resize whose hand-back stands"
        );
    }

    /// A racy screen (`cut=racy`) is answered with the replays against each
    /// other (`against=replay fidelity=-`, seeded from the live buffer), and
    /// the racy cut does not make that verdict `unknown`.
    #[test]
    fn a_racy_cut_is_answered_by_the_replays() {
        let mut rec = Rec::new(20, 8, true);
        rec.out(0.1, &frame("A", 8));
        let flap = rec.flap(1.0);
        rec.out(0.005, &diff(2, "B2"));
        let mut report = analyze_replay(
            &rec.header,
            &rec.events,
            Some(true),
            &DriftArgs::default(),
            ResizePolicy::Native,
        );
        report.settle_cut(Cut::Racy);
        assert_eq!(report.verdict, Verdict::Desync, "{report:?}");
        assert_eq!(
            (report.fidelity, report.seed_basis),
            (None, SeedBasis::Live)
        );
        assert_eq!(report.culprit_word(), fmt_t(flap));
        let head = report.render(0, None);
        assert!(
            head.starts_with("OK 9 verdict=desync fidelity=- against=replay "),
            "{head}"
        );
        assert!(head.lines().next().unwrap().contains(" cut=racy "));
    }

    /// Offline with no screen: faithful stands in for the screen, fidelity
    /// reads `-`, and the verdict is faithful against stable.
    #[test]
    fn drift_without_a_screen_compares_the_replays() {
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, b"\x1b[?1049h");
        rec.out(0.1, &frame("A", 8));
        rec.flap(1.0);
        rec.out(0.005, &diff(2, "B2"));
        let mut report = analyze(
            &rec.header,
            &rec.events,
            None,
            &DriftArgs::default(),
            ResizePolicy::Native,
        );
        report.settle_cut(Cut::File);
        assert_eq!(report.verdict, Verdict::Desync);
        assert_eq!(report.fidelity, None);
        let wire = report.render(0, Some("client"));
        assert!(
            wire.starts_with("OK 9 verdict=desync fidelity=- against=replay drift_rows=7"),
            "{wire}"
        );
        assert!(
            wire.lines()
                .next()
                .unwrap()
                .ends_with(" cut=file folds=2 fold_events=10 ms=0 computed=client")
        );
        // `rows=` caps the listing, never the count.
        let capped = DriftReport {
            list_rows: Some(2),
            ..report
        };
        let wire = capped.render(0, None);
        assert!(wire.starts_with("OK 4 verdict=desync"), "{wire}");
        assert!(wire.contains(" drift_rows=7 "));
    }

    #[test]
    fn drift_of_an_empty_recording_is_empty() {
        let header = CastHeader {
            width: 80,
            height: 24,
            evicted: 0,
            dropped: None,
            resize_undo: None,
            resize_laws: &[ResizeLaw::Undo],
            handoff: None,
        };
        let events: Vec<CastEvent<Vec<u8>>> = Vec::new();
        let report = analyze(
            &header,
            &events,
            None,
            &DriftArgs::default(),
            ResizePolicy::Native,
        );
        assert_eq!(report.verdict, Verdict::Empty);
        assert_eq!(report.folds, 0);
        assert!(
            report
                .render(0, None)
                .starts_with("OK 0 verdict=empty fidelity=- against=replay drift_rows=0 runs=0")
        );
        assert!(report.render(0, None).contains(" dropped=- "));
    }

    /// An alt-screen exit after the last clear brings back a screen the clear
    /// never touched, so there is no anchor; the first toggle being an exit
    /// seeds the alt screen.
    #[test]
    fn an_alt_exit_after_the_last_clear_drops_the_anchor() {
        let mut rec = Rec::new(20, 8, true);
        rec.out(0.1, &frame("A", 8));
        rec.out(0.1, b"\x1b[?1049l");
        let report = rec.audit(&DriftArgs::default());
        assert_eq!(report.anchor, None);
        assert!(report.seed_alt);
        assert_eq!(report.verdict, Verdict::Clean, "{report:?}");
    }

    #[test]
    fn args_parse_the_grammar_and_refuse_the_rest() {
        assert_eq!(DriftArgs::parse(""), Ok(DriftArgs::default()));
        assert_eq!(
            DriftArgs::parse("rows=3 max_runs=40 seed=alt"),
            Ok(DriftArgs {
                max_runs: MAX_RUNS,
                list_rows: Some(3),
                seed: Seed::Alt,
                undo: Undo::Keep,
            })
        );
        // The undo is not on the wire: only the client path sets it.
        for bad in [
            "max_runs",
            "rows=-1",
            "seed=main",
            "frames",
            "count=2",
            "undo=off",
        ] {
            assert_eq!(DriftArgs::parse(bad), Err(USAGE), "{bad}");
        }
    }

    /// A recording that starts at a seamless update says so in its header, and
    /// the report computed from the text alone (offline, or client-side)
    /// carries it, after `ms=`, as the tail every text surface prints for a
    /// handoff. Negative control: the same recording without the key prints no
    /// tail.
    #[test]
    fn offline_drift_sees_the_handoff() {
        use crate::asciicast::{parse, write_into};
        let mut rec = Rec::new(20, 8, false);
        rec.out(0.1, &frame("A", 8));
        rec.flap(1.0);
        let offline = |handoff: Option<HandoffHeader>| {
            let header = CastHeader {
                resize_undo: Some(true),
                handoff,
                ..rec.header.clone()
            };
            let mut text = String::new();
            write_into(&mut text, &header, &rec.events);
            let cast = parse(&text).expect("parses");
            let mut report = analyze(
                &cast.header,
                &cast.events,
                None,
                &DriftArgs::default(),
                ResizePolicy::Native,
            );
            report.settle_cut(Cut::File);
            let wire = report.render(0, Some("client"));
            wire.lines().next().unwrap().to_string()
        };
        let built = offline(Some(HandoffHeader {
            from_build: Some(42),
        }));
        assert!(
            built.ends_with(" ms=0 carried=0 from_build=42 computed=client"),
            "{built}"
        );
        let unknown = offline(Some(HandoffHeader { from_build: None }));
        assert!(
            unknown.ends_with(" ms=0 carried=0 from_build=- computed=client"),
            "{unknown}"
        );
        let plain = offline(None);
        assert!(
            plain.ends_with(" ms=0 computed=client") && !plain.contains("carried="),
            "{plain}"
        );
    }
}
