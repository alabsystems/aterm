// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cast drift`: a COUNTERFACTUAL REPLAY AUDIT of a session's asciicast against
//! the screen it produced.
//!
//! THE FAILURE IT NAMES. A net-zero resize run (64 -> 63 -> 64 rows within a
//! millisecond) that lands on the ALTERNATE screen shifts every row up one: the
//! shrink demotes the top row and the alt grid keeps no history, so the grow
//! appends a blank row instead of revealing it. (The engine now undoes a flap
//! with NO output between its halves: its resize undo hands the demoted row
//! back. Any output between them, even a mode set that draws nothing, drops
//! the undo. An engine that predates it shifted on every such flap, so the
//! replays can drop the undo before each resize, as that engine did
//! ([`Undo`], `undo=off` on the wire): a client-side report against an aterm
//! older than this verb always does, and a saved recording is replayed so only
//! when that alone reproduces the given screen.) An app that repaints only when
//! its SIGWINCH handler reads a CHANGED size (Claude Code, like Node's
//! `tty.WriteStream`) never sees a pair that completed before the handler ran,
//! so its later diff frames land on shifted content for good. Measured on a live
//! window on 2026-09-28: one flap in 41 did exactly that, and nothing in aterm
//! could say so without an investigator bisecting replays by hand.
//!
//! THE METHOD. Fold the recording through fresh headless engines, varying only
//! how the resize runs are applied:
//!
//! - FAITHFUL applies every resize as recorded. It must reproduce the live screen
//!   (the FIDELITY gate); if it does not, the recording cannot explain the grid
//!   and nothing is attributed (`verdict=unfaithful`). That gate is this audit's
//!   negative control: a dropped burst or an evicted head can never accuse a
//!   resize.
//! - STABLE skips EVERY net-zero candidate run and collapses every other
//!   candidate run to its final geometry — the only geometry an app whose handler
//!   ran after the run can have seen. It is the screen the app BELIEVES it drew.
//!   Every candidate, however many: a cap here would let the culprit fall
//!   outside it and cancel out of both replays.
//! - The CULPRIT is named from the faithful replay's own engine journal: the
//!   candidate runs holding an alt-screen resize that demoted or pushed rows no
//!   later grow handed back (the rows nothing can bring back). Only when the journal names none (a primary-
//!   screen rotation, a ConPTY seam) is it attributed by ONLY(r) — run `r`
//!   collapsed alone — over the newest `max_runs` candidates.
//!
//! A candidate is a run of two or more resizes after the ANCHOR: the last output
//! that replaced the whole screen (`ESC[2J`, or an alt-screen entry that clears)
//! with no alt-screen toggle after it. A run before the last full clear cannot
//! explain present drift for an app that clears. A RUN groups resizes by time
//! alone ([`RUN_GAP_SECS`]): output between two halves of a flap does not split
//! it, because a frame the app drew there (a spinner tick) is no sign its
//! SIGWINCH handler ran, and the recording's `o`/`r` order is only approximately
//! the engine's (a burst is stamped when the writer records it).
//!
//! A RETURN is a candidate too: an alternate-screen resize after the anchor and
//! the last alt toggle whose moved rows no grow handed back (the faithful
//! replay's journal), that no run candidate holds, through the first later
//! resize that brings the geometry back to where it stood before it — however
//! far apart, output between included. It is the flap TIME split: an app whose
//! handler ran only after the size came back (a busy event loop) read no change
//! and drew its diffs onto the moved rows, exactly as for a flap one run holds,
//! so the stable replay skips the whole span. An app that did read the smaller
//! size and repainted every row at it, then again at the size that came back,
//! lands on the same screen either way and reads `clean`. A resize whose
//! geometry never came back is the app's to repaint (its handler reads a
//! change whenever it runs), so it is replayed as recorded, as ever.
//!
//! PURITY. No I/O, no clock, no GUI type: the server (`aterm-gui`, over its
//! recorder snapshot and the live engine) and the one binary's client path (over
//! a fetched `cast` and `text`, for a server that predates the verb, or saved
//! files offline) run THIS code, so the two faces cannot disagree. It lives here
//! rather than in `aterm-core` because it is a control-protocol report — an
//! asciicast reader and a wire renderer — built on the engine, and this crate is
//! the headless half of the protocol that already owns [`visible_row`] and
//! [`pct_encode`], the row text and the token encoding the report is made of.

/// The resize seam policy the replays apply, re-exported so a caller that
/// links only this crate (the one binary's client path) can name it.
pub use aterm_core::grid::ResizePolicy;
use aterm_core::terminal::Terminal;

use crate::wire::{pct_encode, visible_row};

/// The request grammar, as the wire states it on a usage error.
pub const USAGE: &str = "ERR usage: cast drift [max_runs=<k>] [rows=<n>] [seed=auto|alt|primary]";

/// Candidate runs varied by default (newest first).
pub const DEFAULT_MAX_RUNS: usize = 8;

/// The most candidate runs one request may vary: each costs one more fold of
/// the whole recording, so this bounds the work at `2 + MAX_RUNS` folds (one
/// more when a saved screen, which names no buffer, has to pick the seed).
pub const MAX_RUNS: usize = 16;

/// Two resizes closer than this are one run, whatever output lies between: the
/// app's SIGWINCH handler may have read only the second size. A heuristic, and
/// named as one — the measured flaps were 0.5-0.9 ms apart.
pub const RUN_GAP_SECS: f64 = 0.250;

/// One recorded event. `D` is the output burst's storage: the server hands its
/// recorder's shared `Arc<[u8]>`s straight through, the asciicast reader owns
/// `Vec<u8>`s.
#[derive(Debug, Clone, PartialEq)]
pub enum CastEvent<D> {
    /// `[t, "o", data]`: program output. `t` is seconds on the cast's own
    /// timeline, exactly as `cast` prints it.
    Output {
        /// Seconds since the (possibly rebased) recording start.
        t: f64,
        /// The burst's bytes.
        data: D,
    },
    /// `[t, "r", "<cols>x<rows>"]`: a geometry change the engine applied.
    Resize {
        /// Seconds since the (possibly rebased) recording start.
        t: f64,
        /// New width.
        cols: u16,
        /// New height.
        rows: u16,
    },
}

impl<D> CastEvent<D> {
    /// The event's timestamp.
    #[must_use]
    pub fn t(&self) -> f64 {
        match self {
            CastEvent::Output { t, .. } | CastEvent::Resize { t, .. } => *t,
        }
    }
}

/// Bursts the reader could not hand to the recorder (a full writer queue), as
/// the header's `aterm_dropped` object discloses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CastDrops {
    /// Bursts dropped.
    pub bursts: u64,
    /// Bytes in them.
    pub bytes: u64,
}

/// The asciicast header facts the audit reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastHeader {
    /// Width at the recording's start (cols).
    pub width: u16,
    /// Height at the recording's start (rows).
    pub height: u16,
    /// Head events drop-oldest evicted (`aterm_truncated.evicted_events`): the
    /// leading engine state is incomplete when this is non-zero.
    pub evicted: u64,
    /// `Some` when the recorder counts drops (0 included); `None` for a
    /// recording whose producer does not say — a file from an older aterm.
    pub dropped: Option<CastDrops>,
}

/// A parsed asciicast v2 recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Asciicast {
    /// The header line's facts.
    pub header: CastHeader,
    /// The `o` and `r` events, in file order (other kinds are skipped).
    pub events: Vec<CastEvent<Vec<u8>>>,
}

impl Asciicast {
    /// The recording as it stood at `until` seconds: every event after it
    /// dropped. What a saved cast is cut to when it is held against a screen
    /// captured at that instant.
    #[must_use]
    pub fn until(mut self, until: f64) -> Self {
        self.events.retain(|e| e.t() <= until);
        self
    }
}

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
    /// The alternate screen when the recording's first alt toggle is an EXIT, or
    /// when it has none and the live screen is on the alternate buffer (an
    /// ADOPTED session, whose cast starts at adoption mid-app); primary when it
    /// has none and the screen is on the primary buffer. With no toggle and no
    /// word on the buffer (a saved screen dump), the one of the two that
    /// reproduces the screen; with no screen either, primary — where a resize
    /// flap is lossless, so a guess never manufactures an accusation.
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
    pub fn word(self) -> &'static str {
        match self {
            SeedBasis::Arg => "arg",
            SeedBasis::Toggle => "toggle",
            SeedBasis::Live => "live",
            SeedBasis::Fidelity => "fidelity",
            SeedBasis::Default => "default",
        }
    }
}

/// Whether the replays keep the engine's RESIZE UNDO (the grid hands the rows
/// an alt-screen shrink took off back to a grow with nothing output between).
/// An aterm before 2026-09-28 had none: every such flap shifted its screen,
/// and its recording replayed with the undo reproduces a screen it never had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Undo {
    /// This engine's: what the server answers with, over its own recording.
    #[default]
    Keep,
    /// Dropped before every resize, as the older engine applied them (`undo=off`
    /// on the wire): a client-side report against an aterm older than this
    /// verb, which is older than the undo too.
    Drop,
    /// Kept, unless only the replay without it reproduces the screen (then
    /// `undo=off`): a saved recording, whose aterm is not known. With no screen
    /// it is kept.
    Match,
}

/// The parsed request tail, and [`Undo`], which is not on the wire: the one
/// binary's client path sets it for the aterm it computes against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriftArgs {
    /// Candidate runs attributed one at a time (ONLY folds) and listed, newest
    /// first, when the engine journal names no culprit (`max_runs=`, at most
    /// [`MAX_RUNS`]; 0 = no such attribution). The verdict always weighs every
    /// candidate.
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
    /// The screen is what the app believes it drew (STABLE matches).
    Clean,
    /// The recording reproduces the screen, and the screen is NOT what the app
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
    pub fn word(self) -> &'static str {
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
    pub fn word(self) -> &'static str {
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
    pub fn len(&self) -> usize {
        self.geoms.len().saturating_sub(1)
    }

    /// Whether the run holds no resize (never, for a recorded run).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the run ends where it started.
    #[must_use]
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
    /// RETURN): all of them are collapsed in the stable replay.
    pub eligible: usize,
    /// The candidate runs LISTED (a `run` line each), oldest first: every
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
    /// The faithful replay's final geometry, `(cols, rows)`.
    pub geom: (u16, u16),
    /// The header's eviction count.
    pub evicted: u64,
    /// The header's drop count.
    pub dropped: Option<CastDrops>,
    /// Engines folded over the recording.
    pub folds: usize,
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
    /// Record how the screen and the recording were cut. A racy cut of a report
    /// held against the SCREEN turns a `desync` or `unfaithful` finding into
    /// `unknown`: the difference may be a burst the recording had not taken
    /// yet. `clean` stays clean — a racy read that still matches accuses
    /// nothing. A report of the replays against each other ([`analyze_replay`],
    /// what a caller answers after a racy cut) reads no screen and keeps its
    /// verdict.
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

    /// The wire form: `OK <n> <header k=v…>` then `n` lines — the candidate
    /// `run` lines (oldest first), the `row` lines (ascending, at most `rows=`)
    /// and one `cursor` line when there is a finding. `ms` is the caller's
    /// measure of the analysis (this module owns no clock); ` undo=off` follows
    /// it when the replays dropped the resize undo, and `computed` names where
    /// it ran when that is not the server (`computed=client`).
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
             cut={} folds={} ms={ms}",
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
        );
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

// ---------------------------------------------------------------------------
// The analysis
// ---------------------------------------------------------------------------

/// A position in the recording: (event index, byte offset in its burst).
type Pos = (usize, usize);

/// What a byte scan of the output finds: the alt-screen toggles and the full
/// clears the anchor and the seed are decided from.
#[derive(Default)]
struct Marks {
    /// Whether the FIRST alt-screen toggle is an exit (`Some(true)`), an entry
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

/// The RETURNS (see the module note): from each `lossy` resize (event indices,
/// ascending, already cut to those after the anchor and the last alt toggle)
/// that no candidate in `held` and no earlier return covers, through the first
/// later resize that brings the geometry back to where it stood before it. A
/// lossy resize whose geometry never came back yields none.
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
struct Replay {
    rows: Vec<String>,
    cursor: (u16, u16),
    geom: (u16, u16),
    /// Event indices of the applied resizes the engine journalled as LOSSY on
    /// the alternate screen (rows demoted or pushed off that no later grow's
    /// resize undo handed back), when the fold was asked to watch its journal.
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

/// How every fold of one audit applies resizes: the seam policy, and whether
/// the engine keeps its resize undo ([`Undo`]).
#[derive(Clone, Copy)]
struct Engine {
    policy: ResizePolicy,
    keep_undo: bool,
}

/// Fold the recording through one fresh engine; `journal` also collects the
/// lossy alt-screen resizes ([`Replay::lossy`]).
///
/// The engine's ALT-SCREEN ARCHIVE is off: it keeps text of committed
/// alternate-screen frames for scrolling back (measured: most of a fold's time
/// on a Claude recording) and never touches the grid a replay is read from.
/// Without the undo (`engine.keep_undo` false) the active grid's is dropped
/// before every resize, so a grow appends as an engine before it did.
fn fold<D: AsRef<[u8]>>(
    header: &CastHeader,
    events: &[CastEvent<D>],
    seed_alt: bool,
    mask: Option<&[bool]>,
    engine: Engine,
    journal: bool,
) -> Replay {
    let mut term = Terminal::new(header.height.max(1), header.width.max(1));
    term.set_alt_archive_enabled(false);
    if seed_alt {
        term.process(b"\x1b[?1049h");
    }
    // Each alt-screen resize that took rows off the screen, with how many are
    // still gone: a grow the engine's resize undo served hands the newest ones
    // back (LIFO, as the undo does), so a quiet flap leaves nothing here.
    let mut moved: Vec<(usize, u32)> = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        match ev {
            CastEvent::Output { data, .. } => term.process(data.as_ref()),
            CastEvent::Resize { cols, rows, .. } => {
                if mask.is_none_or(|m| m[i]) {
                    if !engine.keep_undo {
                        term.grid_mut().drop_resize_undo();
                    }
                    term.resize_with_policy(*rows, *cols, engine.policy);
                    if journal {
                        let (reports, _) =
                            term.resize_journal_since(term.resize_ordinal().saturating_sub(1));
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
        }
    }
    let mut lossy: Vec<usize> = moved.into_iter().map(|(i, _)| i).collect();
    lossy.dedup();
    let rows = (0..term.rows() as usize)
        .map(|r| visible_row(&term, r))
        .collect();
    let c = term.cursor();
    Replay {
        rows,
        cursor: (c.row, c.col),
        geom: (term.cols(), term.rows()),
        lossy,
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
/// offline without one: FAITHFUL then stands in for it and fidelity reads `-`).
/// `policy` is the session's resize seam policy (`ConPty` behind a Windows
/// pseudoconsole, else `Native`). The cut is recorded afterwards with
/// [`DriftReport::settle_cut`].
#[must_use]
pub fn analyze<D: AsRef<[u8]>>(
    header: &CastHeader,
    events: &[CastEvent<D>],
    live: Option<&LiveScreen>,
    args: &DriftArgs,
    policy: ResizePolicy,
) -> DriftReport {
    analyze_inner(header, events, live, live.and_then(|l| l.alt), args, policy)
}

/// The audit of the replays against EACH OTHER (FAITHFUL against STABLE,
/// `against=replay fidelity=-`), seeded from the live buffer when `alt` says
/// it. What a live caller answers when the screen would not hold still
/// (`cut=racy`): no screen is compared, so a racing app cannot make the
/// answer `unknown` forever, at the cost of the fidelity gate.
#[must_use]
pub fn analyze_replay<D: AsRef<[u8]>>(
    header: &CastHeader,
    events: &[CastEvent<D>],
    alt: Option<bool>,
    args: &DriftArgs,
    policy: ResizePolicy,
) -> DriftReport {
    analyze_inner(header, events, None, alt, args, policy)
}

fn analyze_inner<D: AsRef<[u8]>>(
    header: &CastHeader,
    events: &[CastEvent<D>],
    live: Option<&LiveScreen>,
    alt_hint: Option<bool>,
    args: &DriftArgs,
    policy: ResizePolicy,
) -> DriftReport {
    let marks = scan_marks(events);
    let seeded = |keep_undo: bool| {
        let engine = Engine { policy, keep_undo };
        analyze_seeded(header, events, &marks, live, alt_hint, args, engine)
    };
    match args.undo {
        Undo::Keep => seeded(true),
        Undo::Drop => seeded(false),
        // A saved recording of an aterm that predates the undo, held against
        // its screen: this engine's replay says `unfaithful` wherever a quiet
        // flap shifted that screen. The replay without the undo is taken only
        // when it alone reproduces the screen, as the seed pick is.
        Undo::Match => {
            let mut kept = seeded(true);
            if live.is_none() || kept.fidelity != Some(false) {
                return kept;
            }
            let mut dropped = seeded(false);
            if dropped.fidelity == Some(true) {
                dropped.folds += kept.folds;
                dropped
            } else {
                kept.folds += dropped.folds;
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
    // No screen and nothing that says which buffer the recording began on (a
    // saved recording of an adopted session, with no toggle): the verdict on
    // one guessed seed is no finding. Fold the other; when the two disagree
    // the answer is `unknown`, `seed=ambiguous` — pass `seed=alt|primary`.
    // Any resize at all: a RETURN is a candidate only on the seed whose
    // journal saw it lose rows, so none here says nothing about the other.
    if live.is_none() && seed_basis == SeedBasis::Default && report.runs_total > 0 {
        let other = audit(
            header, events, marks, None, !seed_alt, seed_basis, args, engine,
        );
        report.folds += other.folds;
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
        geom: (header.width, header.height),
        evicted: header.evicted,
        dropped: header.dropped,
        folds: 0,
        cursor_live: live.and_then(|l| l.cursor),
        cursor_other: None,
        cut: None,
        list_rows: args.list_rows,
        undo_dropped: !engine.keep_undo,
    };
    if events.is_empty() {
        return report;
    }

    // THE ANCHOR: the last full clear, provided no alt toggle follows it (an
    // exit brings back a screen the clear never touched).
    let anchor = match (marks.last_clear, marks.last_toggle) {
        (Some(clear), Some(toggle)) if clear < toggle => None,
        (clear, _) => clear.map(|(ev, _)| ev),
    };
    report.anchor = anchor.map(|ev| events[ev].t());

    let runs = group_runs(header, events, &marks.clears);
    report.runs_total = runs.len();

    let mut faithful = fold(header, events, seed_alt, None, engine, true);
    report.folds = 1;
    // A screen with no word on its buffer, a recording with no toggle: let the
    // fidelity gate pick the seed. The alt screen is taken only when it alone
    // reproduces the screen.
    if seed_basis == SeedBasis::Default
        && let Some(live) = live
        && !diff_rows(&live.rows, &faithful.rows).is_empty()
    {
        let alt = fold(header, events, true, None, engine, true);
        report.folds += 1;
        if diff_rows(&live.rows, &alt.rows).is_empty() {
            faithful = alt;
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
    // THE CANDIDATES, oldest first: every run of two or more resizes after the
    // anchor, and every RETURN (a lossy resize no such run holds, through the
    // resize that brought its geometry back); a run a return spans is folded
    // into it.
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

    // STABLE collapses EVERY candidate: the verdict never depends on a cap.
    let stable = if eligible.is_empty() {
        None
    } else {
        report.folds += 1;
        let mask = apply_mask(events, &eligible);
        Some(fold(header, events, seed_alt, Some(&mask), engine, false))
    };
    let stable_ref = stable.as_ref().unwrap_or(&faithful);

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

    // ATTRIBUTION. One candidate is the culprit by construction (its ONLY fold
    // IS the stable one). Otherwise the faithful replay's own journal names
    // them: every candidate holding a lossy alt-screen resize on the screen
    // still showing (after the last toggle).
    let blame: Vec<bool> = if eligible.len() == 1 {
        vec![true]
    } else {
        let journal: Vec<bool> = eligible
            .iter()
            .map(|run| lossy.iter().any(|i| (run.first..=run.last).contains(i)))
            .collect();
        if journal.iter().any(|b| *b) {
            journal
        } else {
            // No lossy resize to point at: vary the newest `max_runs` alone.
            // A run whose collapse alone explains every drift row is the
            // culprit; failing a sole one, every run whose collapse alone
            // changes a drift row (each contributes).
            let tried: Vec<usize> = (newest..eligible.len()).collect();
            let only: Vec<Replay> = tried
                .iter()
                .map(|&i| {
                    let mask = apply_mask(events, &eligible[i..=i]);
                    fold(header, events, seed_alt, Some(&mask), engine, false)
                })
                .collect();
            report.folds += only.len();
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
        }
    };
    report.candidates = listed(&blame);
    report.capped = eligible.len() - report.candidates.len();
    report
}

// ---------------------------------------------------------------------------
// The asciicast v2 reader
// ---------------------------------------------------------------------------

/// Parse the text `cast` answers (and `asciinema rec` writes): a JSON header
/// object on the first non-blank line, then one `[t, "<kind>", "<data>"]` array
/// per line. `o` and `r` events are kept; any other kind (`i`, `m`) is skipped.
/// The header's `aterm_truncated.evicted_events` and `aterm_dropped` are read
/// when present. Errors name the 1-based line.
pub fn parse_asciicast(text: &str) -> Result<Asciicast, String> {
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty());
    let (hn, hl) = lines
        .next()
        .ok_or_else(|| "empty recording: no header line".to_string())?;
    let header = parse_header(hl).map_err(|e| format!("line {}: {e}", hn + 1))?;
    let mut events = Vec::new();
    for (n, line) in lines {
        if let Some(ev) = parse_event(line).map_err(|e| format!("line {}: {e}", n + 1))? {
            events.push(ev);
        }
    }
    Ok(Asciicast { header, events })
}

fn parse_header(line: &str) -> Result<CastHeader, String> {
    let Json::Obj(fields) = parse_json(line)? else {
        return Err("the header is not a JSON object".to_string());
    };
    let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    match get("version").and_then(Json::as_u64) {
        Some(2) => {}
        _ => return Err("not an asciicast v2 header (version != 2)".to_string()),
    }
    let dim = |key: &str| -> Result<u16, String> {
        get(key)
            .and_then(Json::as_u64)
            .map(|v| v.clamp(1, u64::from(u16::MAX)) as u16)
            .ok_or_else(|| format!("the header has no integer `{key}`"))
    };
    let evicted = get("aterm_truncated")
        .and_then(|t| t.get("evicted_events"))
        .and_then(Json::as_u64)
        .unwrap_or(0);
    let dropped = get("aterm_dropped").map(|d| CastDrops {
        bursts: d.get("bursts").and_then(Json::as_u64).unwrap_or(0),
        bytes: d.get("bytes").and_then(Json::as_u64).unwrap_or(0),
    });
    Ok(CastHeader {
        width: dim("width")?,
        height: dim("height")?,
        evicted,
        dropped,
    })
}

fn parse_event(line: &str) -> Result<Option<CastEvent<Vec<u8>>>, String> {
    let Json::Arr(items) = parse_json(line)? else {
        return Err("an event is not a JSON array".to_string());
    };
    let [t, kind, data] = items.as_slice() else {
        return Err("an event is not a [time, kind, data] triple".to_string());
    };
    let t = t
        .as_f64()
        .filter(|t| t.is_finite() && *t >= 0.0)
        .ok_or_else(|| "an event time is not a non-negative number".to_string())?;
    let (Json::Str(kind), Json::Str(data)) = (kind, data) else {
        return Err("an event kind or data is not a string".to_string());
    };
    match kind.as_str() {
        "o" => Ok(Some(CastEvent::Output {
            t,
            data: data.clone().into_bytes(),
        })),
        "r" => {
            let (cols, rows) = data
                .split_once('x')
                .and_then(|(c, r)| Some((c.parse::<u16>().ok()?, r.parse::<u16>().ok()?)))
                .ok_or_else(|| format!("a resize is not <cols>x<rows>: {data:?}"))?;
            Ok(Some(CastEvent::Resize { t, cols, rows }))
        }
        _ => Ok(None),
    }
}

/// The JSON the reader needs: a value tree with numbers kept as their literal
/// (so a timestamp is parsed once, exactly).
#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool,
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) => n.parse::<u64>().ok(),
            _ => None,
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => n.parse::<f64>().ok(),
            _ => None,
        }
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Nesting deeper than this is refused: the reader recurses per level, and a
/// hostile file must not be able to overflow the stack.
const MAX_JSON_DEPTH: usize = 32;

fn parse_json(text: &str) -> Result<Json, String> {
    let mut p = JsonReader {
        s: text.as_bytes(),
        i: 0,
    };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err("trailing bytes after the JSON value".to_string());
    }
    Ok(v)
}

struct JsonReader<'a> {
    s: &'a [u8],
    i: usize,
}

impl JsonReader<'_> {
    fn ws(&mut self) {
        while self
            .s
            .get(self.i)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&b) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_JSON_DEPTH {
            return Err("JSON nested too deep".to_string());
        }
        self.ws();
        match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut fields = Vec::new();
                if self.eat(b'}') {
                    return Ok(Json::Obj(fields));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    if !self.eat(b':') {
                        return Err("expected `:` in a JSON object".to_string());
                    }
                    let v = self.value(depth + 1)?;
                    fields.push((key, v));
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b'}') {
                        return Ok(Json::Obj(fields));
                    }
                    return Err("expected `,` or `}` in a JSON object".to_string());
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b']') {
                        return Ok(Json::Arr(items));
                    }
                    return Err("expected `,` or `]` in a JSON array".to_string());
                }
            }
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.word(b"true", Json::Bool),
            Some(b'f') => self.word(b"false", Json::Bool),
            Some(b'n') => self.word(b"null", Json::Null),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.i;
                while self
                    .s
                    .get(self.i)
                    .is_some_and(|b| matches!(b, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
                {
                    self.i += 1;
                }
                // The span is ASCII by the loop's own filter.
                let lit = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                if lit.parse::<f64>().is_err() {
                    return Err(format!("bad JSON number {lit:?}"));
                }
                Ok(Json::Num(lit))
            }
            Some(_) => Err("unexpected byte in JSON".to_string()),
            None => Err("unexpected end of JSON".to_string()),
        }
    }

    fn word(&mut self, w: &[u8], v: Json) -> Result<Json, String> {
        if self.s[self.i..].starts_with(w) {
            self.i += w.len();
            Ok(v)
        } else {
            Err("bad JSON literal".to_string())
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .s
            .get(self.i..self.i + 4)
            .ok_or_else(|| "truncated \\u escape".to_string())?;
        let text = std::str::from_utf8(digits).map_err(|_| "bad \\u escape".to_string())?;
        let v = u32::from_str_radix(text, 16).map_err(|_| "bad \\u escape".to_string())?;
        self.i += 4;
        Ok(v)
    }

    /// A JSON string at the cursor (which must be on its `"`). Escapes decode,
    /// `\uXXXX` included; a surrogate pair joins into one scalar, and a LONE
    /// surrogate becomes U+FFFD (it names no character).
    fn string(&mut self) -> Result<String, String> {
        if self.s.get(self.i) != Some(&b'"') {
            return Err("expected a JSON string".to_string());
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let start = self.i;
            while self
                .s
                .get(self.i)
                .is_some_and(|b| *b != b'"' && *b != b'\\')
            {
                self.i += 1;
            }
            out.extend_from_slice(&self.s[start..self.i]);
            match self.s.get(self.i) {
                None => return Err("unterminated JSON string".to_string()),
                Some(b'"') => {
                    self.i += 1;
                    // The source is a `&str` and every escape pushes a whole
                    // scalar, so this is valid UTF-8; lossy is the total form.
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
                Some(_) => {
                    self.i += 1;
                    let esc = *self
                        .s
                        .get(self.i)
                        .ok_or_else(|| "unterminated JSON escape".to_string())?;
                    self.i += 1;
                    let ch = match esc {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{08}',
                        b'f' => '\u{0C}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => return Err("bad JSON escape".to_string()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
    }

    /// The scalar a `\uXXXX` (cursor just past the `u`) names, joining a
    /// high surrogate with the `\uXXXX` low surrogate that follows it.
    fn unicode_escape(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        if (0xD800..0xDC00).contains(&hi) {
            if self.s[self.i..].starts_with(b"\\u") {
                let save = self.i;
                self.i += 2;
                let lo = self.hex4()?;
                if (0xDC00..0xE000).contains(&lo) {
                    let scalar = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                    return Ok(char::from_u32(scalar).unwrap_or('\u{FFFD}'));
                }
                // Not a low surrogate: the high one stands alone, and the
                // escape after it is read on its own.
                self.i = save;
            }
            return Ok('\u{FFFD}');
        }
        Ok(char::from_u32(hi).unwrap_or('\u{FFFD}'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recording under construction, with the LIVE engine driven by the
    /// identical operations beside it — the screen the audit is held against.
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
        /// screen BEFORE the recording starts, as an adopted session is.
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

        /// A 64 -> 63 -> 64-shaped flap (here rows -> rows-1 -> rows), 0.7 ms
        /// apart, split by output that draws nothing (a mode set, the first
        /// bytes of a SIGWINCH handler): the app never saw a changed size to
        /// repaint for, and the output dropped the engine's resize undo, so the
        /// grow appends and the screen stays shifted. Returns its first
        /// resize's time.
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

    /// THE INCIDENT, in eight rows: an alt-screen app draws a frame with its
    /// cursor above a non-blank footer, the window flaps 8 -> 7 -> 8 with only
    /// a mode set between (nothing drawn), and the app — which never saw a
    /// size change — sends a diff. Every row but the diffed one is shifted up by one, and the audit
    /// names the flap and exactly those rows, with what the app believes they say.
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
            " seed=primary seed_basis=toggle geom=20x8 evicted=0 dropped=0 cut=- folds=2 ms=7"
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

    /// The incident's flap with NOTHING between its halves, on this engine: the
    /// resize undo hands the demoted row back, so the live screen is exactly
    /// what the app believes it drew, and the audit, holding the flap as a
    /// candidate, finds nothing to attribute to it.
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

    /// THE FLAP TIME SPLIT (review of 2026-09-28, reproduced live): the halves
    /// of the incident's flap land 1.6 s apart, output that draws nothing (a
    /// mode set) between them, and the app — whose handler ran only once the
    /// size had come back — sends a diff. No run holds both halves, so before
    /// RETURNS the audit had no candidate and called the shifted screen
    /// `clean`. The shrink is lossy in the faithful journal and the grow brings
    /// its geometry back: that span is a candidate, and the audit names it.
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

    /// The controls for RETURNS. (1) An app that DID read the smaller size and
    /// repainted every row at it, then every row again once it came back, all
    /// without clearing: the span is a candidate, and collapsing it lands on
    /// the same screen — `clean`. (2) Nothing output between the halves: the
    /// undo handed the row back, the journal holds nothing lossy, no
    /// candidate. (3) A shrink that stays: the app's to repaint (its handler
    /// reads a change whenever it runs), replayed as recorded — `clean`, no
    /// candidate.
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

    /// A recording made by an aterm BEFORE the resize undo, held against that
    /// aterm's screen: its quiet flap shifted the screen, which this engine's
    /// replay cannot reproduce (`unfaithful`). Replayed without the undo
    /// (`Undo::Drop`, what the client path uses against an aterm older than
    /// this verb) it is faithful and the flap is named, `undo=off`; `Undo::Match`
    /// (a saved recording) takes that replay only because it alone reproduces
    /// the screen, and keeps the undo for a screen this engine made.
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
            // Match folded both engines; Drop only its own.
            let folds = if undo == Undo::Match { 4 } else { 2 };
            assert_eq!(report.folds, folds, "{undo:?}");
        }

        // This engine's screen of the same session: the undo is kept.
        let mut new = Rec::new(20, 8, false);
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

    /// NEGATIVE CONTROL: with blank rows below the cursor the shrink only trims
    /// them (nothing moves) and the grow appends one back. The flap IS a
    /// candidate, and it is correctly found harmless.
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

    /// THE FIDELITY GATE: a recording that lost a burst cannot reproduce the
    /// screen, and says so (`unfaithful`, rows as live/replayed) — it never
    /// turns the gap into an accusation against the flap beside it.
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

    /// 83 flaps, the first 82 healed by the app's repaint and only the last one
    /// missed: the anchor (the last clear) bounds the search to that one run,
    /// so it is the one culprit and the work is two folds, not 84.
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

    /// An ADOPTED session's recording starts mid-app: no `?1049h` in it, while
    /// the live engine is on the alt screen. The seed follows the live screen,
    /// so the replay is faithful; forced onto the primary screen (where a flap
    /// is lossless) the same recording fails the fidelity gate instead.
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
        // No screen at all: nothing says which buffer the recording began on.
        // The primary replay (where the flap moves nothing) calls it clean, the
        // alt one a desync: a guessed seed is no finding, so the answer is
        // `unknown`, `seed=ambiguous`, and `seed=alt` settles it.
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

    /// Two candidate runs, one harmless (blank tail at the time) and one
    /// destructive: the faithful replay's journal names the destructive one
    /// alone, with no attribution fold.
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

    /// A busy app draws BETWEEN the flap's halves (a spinner tick on its
    /// footer; or the recorder stamped a burst the engine took before the
    /// flap between its halves): the output does not split the run, which
    /// stays one net-zero candidate, and the desync is named.
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
    /// eight after it only trim the blank row it left. The verdict weighs every
    /// candidate (the culprit is the OLDEST, past the default `max_runs` of
    /// eight), and the journal names that one run alone; a culprit is always
    /// listed, whatever `max_runs` lists besides.
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

    /// With no lossy alt-screen resize to point at — the PRIMARY screen's
    /// corner, where a shrink with the cursor on row 0 pushes the bottom row
    /// into history and the grow reveals it on top, rotating the screen — the
    /// culprit falls back to the ONLY(r) folds over the newest `max_runs`.
    #[test]
    fn a_primary_rotation_is_attributed_by_the_only_folds() {
        let mut rec = Rec::new(20, 8, false);
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

    /// A screen that will not hold still (`cut=racy`) is answered with the
    /// replays against each other: `against=replay fidelity=-`, seeded from the
    /// live buffer, and the racy cut does not turn that verdict `unknown`.
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

    /// Offline with no screen: FAITHFUL stands in for the screen, fidelity
    /// reads `-`, and the verdict is FAITHFUL against STABLE.
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
                .ends_with(" cut=file folds=2 ms=0 computed=client")
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

    /// An alt-screen EXIT after the last clear brings back a screen the clear
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

    #[test]
    fn parser_reads_events_with_every_escape() {
        let text = concat!(
            "{\"version\": 2, \"width\": 135, \"height\": 63, \"env\": {\"TERM\": \"xterm\"}}\n",
            "[0.150484, \"o\", \"\\u001b[16t\\\"q\\\\ \\/ \\b\\f\\n\\r\\t\"]\n",
            "\n",
            "[1.655311, \"r\", \"135x64\"]\n",
            "[1.7, \"i\", \"typed\"]\n",
            "[1.8, \"o\", \"\\ud83d\\ude00 \\u00e9 ✻\"]\n",
            "[1.9, \"o\", \"lone \\ud83d x \\ude00 y\"]\n",
            "[2.0, \"o\", \"hi then \\ud83d\\u0041\"]\n",
        );
        let cast = parse_asciicast(text).expect("parses");
        assert_eq!(
            cast.header,
            CastHeader {
                width: 135,
                height: 63,
                evicted: 0,
                dropped: None,
            }
        );
        assert_eq!(cast.events.len(), 5, "the `i` event is skipped");
        assert_eq!(
            cast.events[0],
            CastEvent::Output {
                t: 0.150484,
                data: b"\x1b[16t\"q\\ / \x08\x0c\n\r\t".to_vec(),
            }
        );
        assert_eq!(
            cast.events[1],
            CastEvent::Resize {
                t: 1.655311,
                cols: 135,
                rows: 64,
            }
        );
        let text_of = |i: usize| match &cast.events[i] {
            CastEvent::Output { data, .. } => String::from_utf8(data.clone()).unwrap(),
            CastEvent::Resize { .. } => panic!("not output"),
        };
        assert_eq!(text_of(2), "😀 é ✻", "a surrogate pair joins");
        assert_eq!(text_of(3), "lone \u{FFFD} x \u{FFFD} y", "lone surrogates");
        assert_eq!(
            text_of(4),
            "hi then \u{FFFD}A",
            "a high surrogate before a BMP escape"
        );
        assert_eq!(
            fmt_t(cast.events[1].t()),
            "1.655311",
            "the time round-trips"
        );
    }

    #[test]
    fn parser_reads_the_truncation_and_drop_disclosures() {
        let text = concat!(
            "{\"version\": 2, \"width\": 80, \"height\": 24, \"aterm_truncated\": ",
            "{\"evicted_events\": 17, \"note\": \"drop-oldest evicted the head\"}, ",
            "\"aterm_dropped\": {\"bursts\": 3, \"bytes\": 4096}}\n",
            "[0.000000, \"o\", \"x\"]\n",
        );
        let cast = parse_asciicast(text).expect("parses");
        assert_eq!(cast.header.evicted, 17);
        assert_eq!(
            cast.header.dropped,
            Some(CastDrops {
                bursts: 3,
                bytes: 4096,
            })
        );
        assert_eq!(cast.clone().until(0.0).events.len(), 1);
        assert_eq!(cast.until(-1.0).events.len(), 0);
    }

    #[test]
    fn parser_names_the_bad_line() {
        for (text, want) in [
            ("", "no header"),
            (
                "{\"version\": 1, \"width\": 80, \"height\": 24}\n",
                "line 1",
            ),
            ("{\"version\": 2, \"height\": 24}\n", "`width`"),
            (
                "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.1, \"o\", \"x\"]\n[0.2, \"r\", \"80by24\"]\n",
                "line 3",
            ),
            (
                "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.1, \"o\", \"\\q\"]\n",
                "line 2",
            ),
            (
                "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.1, \"o\", \"open\n",
                "line 2",
            ),
        ] {
            let err = parse_asciicast(text).expect_err(text);
            assert!(err.contains(want), "{err} should name {want}");
        }
        let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(parse_json(&deep).is_err(), "nesting is bounded");
    }
}
