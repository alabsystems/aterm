// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-predict` — predictive local echo ("speculative echo"), mosh-style
//! instant typing, extracted verbatim from `aterm-gui` so the native app and
//! the web embedders (`aterm-wasm`, `aterm-gpu-web`) drive **one**
//! implementation of the state machine (the `aterm-effects` extraction
//! precedent).
//!
//! aterm has no local echo: a typed glyph only appears once the shell/program
//! echoes it back through the PTY. On a fast local link that round-trip is ~1 ms
//! (imperceptible), but over ssh / a loaded box / a heavy TUI it is the dominant,
//! unbounded source of felt input lag. This crate paints the typed character
//! IMMEDIATELY at the cursor — a *prediction* — and then RECONCILES it against the
//! real output when it lands, retiring confirmed predictions and flushing wrong
//! ones. Modelled on mosh's predictive echo.
//!
//! It is PURE and CLOCK-INJECTED (every entry point takes `now: Instant`), so the
//! whole state machine is unit-testable without sleeping or a real PTY — the same
//! discipline as `aterm-effects`' `cursor_glow`. On native `Instant` is exactly
//! `std::time::Instant` (byte-identical to the pre-extraction `aterm-gui` build);
//! on wasm32 it is `web_time::Instant`, so a web host can sample its real
//! monotonic clock. The host wires four seams:
//!   1. [`Predictor::predict_char_in_grid`] (or the height-blind
//!      [`Predictor::predict_char`]) / [`Predictor::predict_backspace`] on a keypress,
//!   2. [`Predictor::reconcile`] after child output is applied to the grid,
//!   3. [`Predictor::overlay`] when composing a frame (the glyphs to paint), and
//!   4. [`Predictor::reset`] when the coordinate space changes (resize / pane swap) —
//!      and [`Predictor::note_scroll`] when the grid scrolls, which moves the cell
//!      every pending guess (an ABSOLUTE row/col) is waiting on.
//!
//! A host with tabs owns one more seam: the learned echo estimate is a property of the
//! SESSION, not of the window, so it is SWAPPED across a front change
//! ([`Predictor::take_link_estimate`] / [`Predictor::restore_link_estimate`]) rather
//! than thrown away. [`Predictor::reset_session`] remains the "no estimate known" path.
//!
//! ## Safety (why this never corrupts the screen)
//! * **Adaptive display.** Predictions are always *tracked* (to measure echo RTT),
//!   but in the default `Adaptive` mode they are only *shown* after consecutive slow
//!   confirmations establish a stable high-latency link AND at least one prediction
//!   has been confirmed this epoch. One delayed scheduler turn cannot enable pixels.
//!   The gate is HYSTERETIC (open at `DISPLAY_OPEN_MS`, close only after
//!   `FAST_SAMPLES_TO_HIDE` decisive fast samples with a fast *smoothed* RTT and an
//!   empty pending set): a symmetric single-sample gate flaps under ordinary jitter,
//!   and every flap erases a ghost that is already on glass (a dim→blank→solid blink
//!   that reads as a rendering fault, not as a latency win).
//! * **Alt-screen gate.** In the alternate screen (vim/less/htop) the app owns the
//!   cursor and does not line-echo; [`Predictor::reconcile`] flushes and predicting
//!   is refused.
//! * **No unechoed flash (Adaptive).** Adaptive display requires a *confirmed* echo
//!   in the CURRENT line's epoch, so a password prompt (a line that never echoes)
//!   never displays a predicted character. The epoch is ended at the SUBMIT boundary
//!   ([`Predictor::note_line_submit`], on Enter) — not merely on a physical-row change,
//!   which the cursor does NOT undergo across logical lines on a terminal scrolled to
//!   the bottom — so a prompt inheriting a prior command's confirmation on the same
//!   bottom row cannot flash the secret. Guesses carry the epoch they were armed in, so
//!   a pre-submit guess retiring AFTER the boundary cannot re-arm the new line's gate.
//!   `Always` is the explicit power-user opt-in and does NOT carry this guarantee: it
//!   can briefly show an unechoed glyph until the glitch-window expiry, so it is
//!   unsuitable at a password prompt.
//! * **Self-healing.** Any prediction unconfirmed for the glitch window
//!   (RTT-relative — floor 250 ms, ceiling 2 s) expires,
//!   and any divergence (the app drew a different glyph, or the cursor jumped) flushes
//!   the whole set — so a wrong guess is corrected within one output burst.

use std::time::Duration;
use web_time::Instant;

/// How aggressively to DISPLAY predictions. Tracking happens regardless; this only
/// gates what is painted. Parsed from the `predictive_echo` config string.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PredictMode {
    /// Never predict (feature off). The default, so a default-constructed predictor
    /// is inert until config opts in.
    #[default]
    Off,
    /// Show predictions only when the measured echo RTT is high enough to benefit
    /// (and an echo has been confirmed this epoch). The recommended setting.
    Adaptive,
    /// Always show predictions immediately (power users / high-latency links / demos).
    Always,
}

impl PredictMode {
    /// Parse the config string (case-insensitive); unknown ⇒ `Off` (fail safe).
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        let any = |opts: &[&str]| opts.iter().any(|o| s.eq_ignore_ascii_case(o));
        if any(&["adaptive", "auto", "on", "true"]) {
            Self::Adaptive
        } else if any(&["always", "force"]) {
            Self::Always
        } else {
            Self::Off
        }
    }
}

/// Everything a [`Predictor`] has LEARNED about how fast the far end echoes, and
/// nothing about where any guess was on screen.
///
/// The estimate is a property of the SESSION — the PTY and the link behind it — not
/// of the window that happens to be showing it. A tabbed host must therefore SWAP it:
/// [`Predictor::take_link_estimate`] when a session goes to the background,
/// [`Predictor::restore_link_estimate`] when it comes forward again. Clearing it
/// instead ([`Predictor::reset_session`], the "no estimate known" path) makes every
/// front change re-earn the display latch from scratch — `SLOW_SAMPLES_TO_DISPLAY`
/// unpredicted characters at the head of every line typed after a tab switch, on
/// exactly the tab-heavy ssh workflow the feature exists for.
///
/// OPAQUE by construction: the fields are private, so a host can store one and hand
/// it back but cannot forge a "this link is slow" verdict no measurement earned.
/// [`Default`] is "nothing measured yet" — what a brand-new session starts with, and
/// exactly what `reset_session` leaves behind.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinkEstimate {
    srtt: Option<f32>,
    slow_samples: u8,
    fast_samples: u8,
    adaptive_slow: bool,
    expiry_backoff: u8,
}

/// One predicted glyph at an absolute grid cell, awaiting confirmation from output.
#[derive(Clone, Copy, Debug)]
pub struct Prediction {
    /// Grid row the guess was typed on (active-grid coords).
    pub row: u16,
    /// Grid column the guess occupies.
    pub col: u16,
    /// The predicted glyph.
    pub ch: char,
    /// When predicted — drives the unconfirmed-glitch timeout (the glitch window).
    born: Instant,
    /// The confirmation epoch (see `Predictor::epoch`) this guess was armed in. A
    /// guess armed BEFORE an Enter must never arm the NEXT line's display gate when
    /// its echo finally lands, or a password prompt inherits the submitted command's
    /// confirmation — the exact leak `note_line_submit` exists to prevent.
    epoch: u64,
}

impl Prediction {
    /// Host-test constructor (`born` is private): the ghost-painter tests in
    /// `aterm-gui`'s `app_render` build arbitrary guesses without driving a full
    /// [`Predictor`]. Stamps `born = now`; not intended for production hosts —
    /// real guesses only ever originate inside [`Predictor::predict_char`].
    pub fn test_at(row: u16, col: u16, ch: char) -> Self {
        Self {
            row,
            col,
            ch,
            born: Instant::now(),
            epoch: 0,
        }
    }
}

/// Floor of the unconfirmed-guess expiry window: below this a normal output burst has
/// not necessarily arrived yet even on a local link.
const GLITCH_FLOOR_MS: u64 = 250;
/// Ceiling of the expiry window. The window is RTT-relative (see
/// [`Predictor::glitch_window`]) so a slow link can actually keep a guess alive long
/// enough to be confirmed; this bounds how long a *wrong* guess (raw-mode key,
/// swallowed input) can sit on glass before it self-heals.
const GLITCH_CEILING_MS: u64 = 2000;
/// The expiry window as a multiple of the smoothed RTT. A fixed 250 ms window is
/// SHORTER than the echo it is waiting for on every link the feature targets: the
/// expiry then always beats the echo, nothing is ever confirmed, no RTT sample is ever
/// recorded, and the adaptive latch can never open — the feature is dead in exactly
/// its target regime. Three RTTs leaves room for one retransmit-scale outlier.
const GLITCH_SRTT_MULT: f32 = 3.0;
/// How many times consecutive timeouts may DOUBLE the glitch floor while probing a
/// cold link (250 → 500 → 1000 → 2000 ms). Bounded so an endlessly non-echoing
/// context — a password prompt, a raw-mode key — cannot walk the window upward
/// without limit, and reset by the first real confirmation ([`Predictor::record_rtt`]).
const EXPIRY_BACKOFF_MAX: u8 = 3;
/// `Adaptive` OPENS the display gate on samples at or above this latency. Well clear
/// of aterm's own local-echo distribution (single-digit ms), so ordinary scheduler
/// noise cannot be mistaken for a link that speculation can help.
const DISPLAY_OPEN_MS: f32 = 20.0;
/// `Adaptive` may CLOSE the display gate only on samples below this latency. The gap
/// to [`DISPLAY_OPEN_MS`] is the hysteresis band: samples inside it are evidence for
/// neither regime and leave the latch (and both streaks) untouched, so a link jittering
/// across one threshold cannot flap the gate — and every flap erases a ghost that is
/// already on glass.
const DISPLAY_CLOSE_MS: f32 = 8.0;
/// A single delayed event-loop turn is not evidence of a slow link. Require two
/// consecutive slow samples before Adaptive may paint.
const SLOW_SAMPLES_TO_DISPLAY: u8 = 2;
/// …and symmetric-but-stricter evidence to stop painting: three consecutive decisive
/// fast samples. Closing is the destructive direction (it erases in-flight pixels), so
/// it also requires the smoothed RTT to agree and the pending set to be empty.
const FAST_SAMPLES_TO_HIDE: u8 = 3;
/// EWMA weight for a freshly-confirmed echo sample into the smoothed RTT.
const SRTT_ALPHA: f32 = 0.3;

/// The speculative-echo state machine for one terminal pane. See the crate docs.
#[derive(Default)]
pub struct Predictor {
    mode: PredictMode,
    /// Pending predictions in type order (oldest = `first`). A guess at the right
    /// margin continues at column 0 of the NEXT row when the host supplies the grid
    /// height ([`Predictor::predict_char_in_grid`]); an unexpected row change still
    /// flushes them.
    preds: Vec<Prediction>,
    /// Smoothed echo round-trip estimate in ms (EWMA of confirmed predictions). The
    /// link property `Adaptive` consults. Coordinate-only [`reset`](Self::reset)
    /// preserves it; [`reset_session`](Self::reset_session) clears it so a slow
    /// remote pane can never make a newly focused local pane speculate visibly.
    srtt: Option<f32>,
    /// Consecutive expiry turns with no confirmation, doubling the glitch floor as a
    /// COLD-LINK probe (see [`Predictor::glitch_window`]). Deliberately NOT an input to
    /// `srtt` or to the display regime: a timeout is the absence of an echo, not a slow
    /// echo, and treating it as evidence both diverged the RTT estimate and latched
    /// speculation ON at non-echoing prompts. Reset by the first real confirmation.
    expiry_backoff: u8,
    /// Consecutive raw echo samples at or above [`DISPLAY_OPEN_MS`]. This filters
    /// isolated scheduler tails that would otherwise seed the EWMA above the visual
    /// threshold after one sample.
    slow_samples: u8,
    /// Consecutive raw echo samples below [`DISPLAY_CLOSE_MS`] — the closing streak.
    /// Separate from `slow_samples` because the two thresholds differ (hysteresis):
    /// a sample inside the band advances neither.
    fast_samples: u8,
    /// Stable Adaptive display classification. Consecutive slow samples open it;
    /// closing needs a sustained fast streak, a fast smoothed RTT, and nothing in
    /// flight — so returning from a remote foreground process to a local shell still
    /// closes it promptly, but ordinary jitter cannot blink a ghost off the glass.
    adaptive_slow: bool,
    /// Have we confirmed ≥1 prediction on the CURRENT line (epoch)? Gates display so an
    /// unechoed context never shows a guess. Reset when typing starts on a new row.
    confirmed_epoch: bool,
    /// The row the current confirmation epoch belongs to. Typing on a NEW row (e.g. a
    /// password prompt after a command's Enter) starts a fresh epoch with
    /// `confirmed_epoch = false`, so a no-echo line never inherits a prior line's
    /// confirmation and displays an unechoed secret.
    epoch_row: Option<u16>,
    /// Monotonic id of the CURRENT confirmation epoch, bumped by
    /// [`note_line_submit`](Self::note_line_submit). Guesses are tagged with it so the
    /// submit boundary can end the epoch for FUTURE guesses without discarding the
    /// pixels of in-flight ones: a stale guess still reconciles (and still feeds the
    /// RTT estimate) but can neither anchor a new guess nor arm the new line's gate.
    epoch: u64,
    /// The most recent grid width seen by `predict_char*`. Only [`reconcile`](Self::reconcile)
    /// reads it, for the two margin questions: a guess that continues at column 0 of
    /// the next row is confirmed against a real cursor parked in the DEFERRED-WRAP slot
    /// (last column of the previous row), which is where a terminal leaves it after
    /// echoing the final column — and whether the cursor it is looking at is in that
    /// slot at all (`deferred_wrap`).
    grid_cols: u16,
    /// The terminal is PARKED in the deferred-wrap slot: at the last [`reconcile`](Self::reconcile)
    /// the real cursor sat on the final column of a row whose cell was already FILLED.
    /// That parking is indistinguishable by cursor position alone from a cursor sitting
    /// on a free last column, and the difference decides where the next keystroke
    /// echoes: a parked terminal wraps it to column 0 of the next row. Anchoring a
    /// fresh guess on the raw cursor there placed it ON TOP of the glyph just echoed —
    /// a wrong character painted over a correct one every time a command line reached
    /// the right margin and the pending set had drained, flushed a burst later when
    /// `reconcile` found the real cell diverged. Recorded from the freshest real
    /// observation because `predict_char_in_grid` has no grid to consult.
    deferred_wrap: bool,
    /// A character we could not model was declined while guesses were pending. Those
    /// guesses are anchored to its LEFT and remain valid, but everything typed after it
    /// would be predicted one or more columns too far left (the real echo will insert
    /// the declined glyph). So we stop extending until the set drains and the live
    /// cursor — which the real echo moves correctly — can re-anchor us.
    anchor_lost: bool,
    /// The epoch whose still-pending guesses keep displaying after their line was
    /// submitted. Set at [`note_line_submit`](Self::note_line_submit) only when that
    /// epoch had CONFIRMED an echo, so the carried pixels are always characters the
    /// user has already watched that line echo back — never a secret.
    grandfathered_epoch: Option<u64>,
}

impl Predictor {
    /// A predictor starting in `mode` (a default-constructed one starts `Off`).
    pub fn new(mode: PredictMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// Apply a (possibly changed) display mode; a real change flushes in-flight
    /// guesses so a mode flip never leaves an orphaned overlay.
    pub fn set_mode(&mut self, mode: PredictMode) {
        if mode != self.mode {
            self.mode = mode;
            self.flush();
        }
    }

    /// Register a printable character the user just typed (and that the host wrote to
    /// the PTY), on a host that does not know its grid HEIGHT. Identical to
    /// [`predict_char_in_grid`](Self::predict_char_in_grid) with no wrap lane: a guess
    /// that would cross the right margin is declined. Prefer the grid-aware entry
    /// point — a long command line is exactly where an ssh user wants type-ahead most,
    /// and the wrap is where it used to stop.
    pub fn predict_char(&mut self, ch: char, cursor: (u16, u16), cols: u16, now: Instant) -> bool {
        // `rows = 0` ⇒ no row is available to wrap INTO, so the margin declines.
        self.predict_char_in_grid(ch, cursor, (cols, 0), now)
    }

    /// Register a printable character the user just typed. `cursor` is the live real
    /// cursor (the anchor when nothing is pending); while typing ahead, each new glyph
    /// extends from the previous prediction. `grid` is `(cols, rows)`: at the right
    /// margin the guess continues at column 0 of the next row, and only a guess that
    /// would fall off the BOTTOM of the grid is declined. Returns `true` if a guess was
    /// added.
    ///
    /// A refusal never flushes. A character we cannot model does not invalidate the
    /// guesses anchored to its LEFT — those are still exactly what the program will
    /// echo — and flushing them erased the whole word on every accented keystroke of a
    /// French/German/Spanish/UK layout. It does however cost us the anchor for
    /// everything typed AFTER it (see `anchor_lost`).
    pub fn predict_char_in_grid(
        &mut self,
        ch: char,
        cursor: (u16, u16),
        grid: (u16, u16),
        now: Instant,
    ) -> bool {
        if self.mode == PredictMode::Off {
            return false;
        }
        let (cols, rows) = grid;
        if cols == 0 {
            return false; // degenerate geometry: no cell can hold a guess
        }
        self.grid_cols = cols;
        // A drained set re-anchors on the live cursor, which the real echo has moved
        // past whatever we declined, so the block on extending lifts here.
        if self.preds.is_empty() {
            self.anchor_lost = false;
        }
        // Only single-width, self-echoing printables — width is the criterion, not
        // ASCII-ness: `é`/`ü`/`ñ` occupy exactly one cell and echo like any other
        // letter, while control (ESC sequences), zero-width (combining marks, which
        // modify the cell to their LEFT) and wide (CJK, emoji) glyphs have echo
        // geometry we cannot place. Refused glyphs are left to the real output.
        if ch.is_control() || aterm_grapheme::char_width(ch) != 1 {
            self.anchor_lost = true;
            return false;
        }
        if self.anchor_lost {
            return false;
        }
        // Anchor: extend the last pending guess OF THIS EPOCH, else the live cursor.
        // A pre-submit guess still in flight belongs to the previous logical line and
        // must not anchor the new one.
        let anchor = self.preds.iter().rev().find(|p| p.epoch == self.epoch);
        // A new line is a fresh confirmation epoch: a guess only displays (Adaptive)
        // after an echo is confirmed ON THIS LINE. So a no-echo prompt on a new row
        // (a password prompt after a command) never inherits the prior line's
        // confirmation and never shows an unechoed glyph.
        if anchor.is_none() && self.epoch_row != Some(cursor.0) {
            self.confirmed_epoch = false;
            self.epoch_row = Some(cursor.0);
        }
        let (row, col) = match anchor {
            Some(p) => (p.row, p.col.saturating_add(1)),
            // The live cursor is a free cell ONLY if the terminal is not parked in the
            // deferred-wrap slot (see `deferred_wrap`): there the cell under the cursor
            // is already echoed content and the next glyph lands on the next row. Hand
            // the shared wrap lane below an out-of-range column so the wrap — and the
            // bottom-edge decline — is decided in exactly one place.
            None if self.deferred_wrap && cursor.1.saturating_add(1) == cols => (cursor.0, cols),
            None => cursor,
        };
        let (row, col) = if col >= cols {
            // The line wraps. The next glyph lands at column 0 of the following row —
            // that is what the program will echo, so predicting it is no more
            // speculative than any other guess. Only the bottom edge is unmodellable
            // (the grid scrolls, moving every pending guess), and there we decline
            // WITHOUT flushing: the guesses to the left are still correct.
            let next = row.saturating_add(1);
            if next >= rows {
                self.anchor_lost = true;
                return false;
            }
            (next, 0)
        } else {
            (row, col)
        };
        self.preds.push(Prediction {
            row,
            col,
            ch,
            born: now,
            epoch: self.epoch,
        });
        true
    }

    /// Register a Backspace. We only cancel our OWN trailing prediction (the common
    /// "type then immediately fix" case); erasing already-committed real content is
    /// left to the program's echo (conservative). Returns `true` if state changed.
    pub fn predict_backspace(&mut self, _now: Instant) -> bool {
        if self.mode == PredictMode::Off {
            return false;
        }
        // Only OUR trailing guess on the CURRENT line: a guess left in flight by the
        // previous line's submit is not what this Backspace erases.
        if self.preds.last().is_some_and(|p| p.epoch == self.epoch) {
            self.preds.pop();
            return true;
        }
        false
    }

    /// The user SUBMITTED the line (a plain Enter). End the confirmation epoch: the
    /// NEXT line must re-confirm an echo before any prediction is displayed.
    ///
    /// LOAD-BEARING for the no-unechoed-flash guarantee. `confirmed_epoch` is
    /// otherwise keyed to the physical cursor ROW (`epoch_row`), which is REUSED
    /// across logical lines on a terminal scrolled to the bottom (the cursor stays on
    /// the last row as content scrolls up). Without a reset on submit, a non-echoing
    /// password prompt landing on the SAME bottom row as a just-confirmed command
    /// would INHERIT that confirmation and flash the secret. Resetting on the Enter
    /// boundary makes the epoch track logical INPUT lines, not physical rows — so
    /// `sudo`/`ssh`/`git push` password prompts show nothing until they prove they
    /// echo (which a password prompt never does). Cheap no-op when nothing is pending.
    ///
    /// It ends the epoch for FUTURE guesses ONLY; guesses already in flight keep their
    /// pixels. Flushing them here erased the last few typed characters at the exact
    /// instant of commit — on a slow link the tail of the command line blinked out and
    /// reappeared one RTT later, at the most attention-grabbing moment of the whole
    /// interaction, which felt LAGGIER than not predicting at all. They retire normally
    /// through [`reconcile`](Self::reconcile) (the echo lands, or the cursor jumps to
    /// the next row and the consistency check flushes) or expire at the glitch window.
    /// The password guarantee is untouched: it concerns guesses armed AFTER this
    /// boundary, and those carry the NEW epoch, which starts unconfirmed — a stale
    /// guess confirming later cannot arm it (see `Prediction::epoch`).
    pub fn note_line_submit(&mut self) {
        // Ending the epoch closes the Adaptive gate, which would blank the in-flight
        // guesses just as surely as flushing them. Guesses from an epoch that PROVED it
        // echoes keep their pixels until they retire or expire — the user has already
        // watched that line echo, so nothing unechoed is exposed by carrying them.
        self.grandfathered_epoch = self.confirmed_epoch.then_some(self.epoch);
        self.epoch = self.epoch.wrapping_add(1);
        self.confirmed_epoch = false;
        self.epoch_row = None; // the next predict_char (re)starts a fresh epoch
        self.anchor_lost = false; // the new line re-anchors on the live cursor
    }

    /// Reconcile pending predictions against fresh grid state after child output was
    /// applied. `real_cursor` is the cursor now, `alt_screen` whether the alternate
    /// screen is active, and `observe(row,col)` reads the real glyph at a cell (its
    /// `char`, or `None` if blank/space/unreadable). Confirms leading predictions the
    /// program echoed (feeding the RTT estimate and arming the epoch's display gate),
    /// and flushes on any divergence.
    pub fn reconcile(
        &mut self,
        real_cursor: Option<(u16, u16)>,
        alt_screen: bool,
        now: Instant,
        mut observe: impl FnMut(u16, u16) -> Option<char>,
    ) {
        // The alternate screen owns the cursor and does not line-echo: never predict.
        if alt_screen {
            self.flush();
            // An app-drawn cursor says nothing about a pending line wrap, and the
            // observation would be stale by the time the primary screen returns.
            self.deferred_wrap = false;
            return;
        }

        // Classify latency once per independent output/reconcile TURN, not once per
        // character. One delayed scheduler callback can retire a whole type-ahead
        // burst; counting every retired glyph as separate slow evidence would let
        // that single tail satisfy the two-confirmation latch by itself.
        //
        // The turn's sample is the OLDEST retired guess — the latency the user
        // actually waited to see their first unechoed glyph. `preds` is in type order,
        // so the LAST retired guess is always the most recently typed and therefore
        // always the smallest sample: taking the minimum meant a 5-character burst
        // echoed in one 45 ms turn reported 5 ms, and the faster the user typed the
        // more certain the shutoff — the display gate closed hardest exactly when
        // type-ahead was carrying the most text.
        let mut oldest_confirmed: Option<Duration> = None;

        // Retire confirmed leading predictions; a different glyph at the head ⇒ the
        // program diverged from our guess ⇒ flush. A still-blank head ⇒ not echoed
        // yet ⇒ stop and wait for the next burst.
        while let Some(p) = self.preds.first().copied() {
            match observe(p.row, p.col) {
                Some(c) if c == p.ch => {
                    let sample = now.saturating_duration_since(p.born);
                    self.record_rtt(sample);
                    oldest_confirmed.get_or_insert(sample);
                    // A guess from BEFORE the last submit measures the link, but must
                    // not arm the new line's display gate (password safety).
                    self.confirmed_epoch |= p.epoch == self.epoch;
                    self.preds.remove(0);
                }
                Some(_) => {
                    self.flush();
                    break;
                }
                None => {
                    // An echoed SPACE is indistinguishable from a blank cell by glyph
                    // (observe filters ' ' → None), so confirm it instead by the real
                    // cursor having advanced past it; otherwise it would wedge the queue
                    // and the cursor-consistency check below would flush all type-ahead.
                    if p.ch == ' '
                        && matches!(real_cursor, Some((rr, rc)) if rr == p.row && rc > p.col)
                    {
                        let sample = now.saturating_duration_since(p.born);
                        self.record_rtt(sample);
                        oldest_confirmed.get_or_insert(sample);
                        self.confirmed_epoch |= p.epoch == self.epoch;
                        self.preds.remove(0);
                    } else {
                        break;
                    }
                }
            }
        }
        if let Some(sample) = oldest_confirmed {
            self.record_echo_regime(sample);
        }

        // Cursor-consistency: if anything is still pending, the real cursor must sit
        // exactly at the first unconfirmed prediction (the program echoed up to it).
        // Anything else (a row change, completion rewrite, reflow) ⇒ flush.
        if let Some(&p) = self.preds.first() {
            match real_cursor {
                Some(rc) if rc == (p.row, p.col) => {}
                // A guess that continued past the right margin is legitimately AHEAD of
                // the real cursor: a terminal defers the wrap, so after echoing the
                // final column the cursor is parked THERE (last column, previous row)
                // and only moves to (row, 0) when the next glyph arrives. Treating that
                // as a divergence flushed every wrapped type-ahead one burst after it
                // was armed — the long command lines the feature exists for.
                Some((rr, rc))
                    if p.col == 0
                        && self.grid_cols > 0
                        && rr.saturating_add(1) == p.row
                        && rc.saturating_add(1) == self.grid_cols => {}
                _ => self.flush(),
            }
        }

        // Latch the deferred-wrap parking for the NEXT keystroke (see `deferred_wrap`).
        // Computed last, from this turn's real cursor, so a flush above cannot leave a
        // stale reading behind: the observation describes the SCREEN, not our guesses.
        // A blank (or space) final cell reads as "not parked" — the terminal has not
        // filled that column yet, so the next glyph really does land in it.
        self.deferred_wrap = match real_cursor {
            Some((rr, rc)) if self.grid_cols > 0 && rc.saturating_add(1) == self.grid_cols => {
                observe(rr, rc).is_some()
            }
            _ => false,
        };
    }

    /// The predictions to OVERLAY this frame. Expires stale unconfirmed guesses first
    /// (self-healing), then applies the display gate — see
    /// [`visible_count`](Self::visible_count) — which yields the leading run of guesses
    /// this frame may paint: `Off` ⇒ none; `Always` ⇒ all pending; `Adaptive` ⇒ all
    /// pending once an echo is confirmed this epoch and consecutive slow samples
    /// established a stable high-latency link (plus any guess carried across a submit
    /// by a line that had already confirmed).
    pub fn overlay(&mut self, now: Instant) -> &[Prediction] {
        self.expire(now);
        &self.preds[..self.visible_count()]
    }

    /// Retire guesses the program has not echoed within the glitch window.
    ///
    /// Only the EXPIRED head(s) go. The old code flushed the entire set, which on a
    /// link slower than the (then fixed) window destroyed the whole mechanism: the
    /// flush also cleared `confirmed_epoch`, so the echo always arrived to an empty
    /// `preds`, nothing was ever confirmed, no RTT sample was ever taken, and the
    /// adaptive latch could never open. The tail is younger than the head and has not
    /// earned its timeout yet.
    ///
    /// A timeout is itself evidence: it proves the echo took AT LEAST one window. That
    /// lower bound is fed to the estimator so a cold slow link can bootstrap — without
    /// it the window can only grow from confirmations, and on a link where the first
    /// window always expires first there are none. The sample is the WINDOW, not the
    /// guess's true age: `overlay` is frame-driven, and a backgrounded/stalled host can
    /// call it seconds late, which would otherwise blow the estimate (and with it every
    /// guess's lifetime) up to the ceiling on one dropped frame.
    fn expire(&mut self, now: Instant) {
        let window = self.glitch_window();
        let mut timed_out = false;
        while let Some(oldest) = self.preds.first() {
            if now.saturating_duration_since(oldest.born) <= window {
                break;
            }
            self.preds.remove(0);
            timed_out = true;
        }
        if timed_out {
            // A timeout WIDENS the probe window and NOTHING else. It must not reach
            // `record_rtt` or `record_echo_regime`, both of which an earlier version of
            // this fix called with `window` — two separate defects:
            //
            // (a) DIVERGENCE. `window` is itself `GLITCH_SRTT_MULT * srtt`, so feeding
            //     it back gives `srtt' = 0.7*srtt + 0.3*(3*srtt) = 1.6*srtt` — no fixed
            //     point below the ceiling. Successive unechoed keystrokes on a 1 ms LAN
            //     walked the window 250 → 383 → 613 → 981 → 1569 → 2000 ms, leaving a
            //     wrong guess on glass for two full seconds.
            //
            // (b) SAFETY. `window >= GLITCH_FLOOR_MS` is always >= `DISPLAY_OPEN_MS`, so
            //     every timeout voted "slow" and two of them latched `adaptive_slow` —
            //     ON A LOCAL LINK. The most reachable trigger is the password prompt
            //     this crate's safety docs are built around: nothing echoes, the real
            //     cursor never moves, so `reconcile` never flushes and every guess
            //     reaches here. Typing a 2-character password put a ghost on screen.
            //
            // A timeout means NO echo arrived — which is not the same observation as a
            // slow echo, and is the signature of a context where predicting is useless.
            // So the backoff is an independent probe: it only lengthens how long the
            // NEXT guess is willing to wait, and any real confirmation resets it. A
            // cold link slower than the floor still bootstraps (the window widens until
            // an echo fits inside it, and THAT confirmation is the real evidence that
            // opens the gate), but no amount of silence can ever open the gate itself.
            self.expiry_backoff = self
                .expiry_backoff
                .saturating_add(1)
                .min(EXPIRY_BACKOFF_MAX);
        }
    }

    /// How long an unconfirmed guess may live. RTT-relative, because a fixed window is
    /// the wrong unit: 250 ms is generous on a 2 ms LAN link and shorter than a single
    /// echo on the 300 ms satellite/transpacific links this feature is FOR. Floor and
    /// ceiling bound the two failure modes (expiring inside one local output burst; a
    /// wrong guess sitting on glass).
    /// The `expiry_backoff` term is the COLD-LINK probe: with no confirmation yet
    /// there is no `srtt`, so a link slower than the floor could never get an echo in
    /// under the window and could never learn anything. Doubling the floor per
    /// consecutive timeout walks it 250 → 500 → 1000 → 2000 ms until an echo fits;
    /// any confirmation resets it (see [`Self::record_rtt`]). It is bounded, and
    /// unlike feeding timeouts into `srtt` it cannot diverge or vote on the regime.
    fn glitch_window(&self) -> Duration {
        let probe = (GLITCH_FLOOR_MS << self.expiry_backoff.min(EXPIRY_BACKOFF_MAX)) as f32;
        let floor = probe.min(GLITCH_CEILING_MS as f32);
        let ms = self.srtt.map_or(floor, |s| {
            (s * GLITCH_SRTT_MULT).clamp(floor, GLITCH_CEILING_MS as f32)
        });
        Duration::from_millis(ms as u64)
    }

    /// How many LEADING pending guesses pass the display gate — shared by
    /// [`overlay`](Self::overlay) and [`is_displaying`](Self::is_displaying). `Off` ⇒
    /// none; `Always` ⇒ all; `Adaptive` ⇒ all once an echo is confirmed this epoch and
    /// consecutive slow echo samples established that prediction can help.
    ///
    /// A COUNT rather than a bool because the submit boundary can leave guesses from a
    /// confirmed line in flight while the new epoch is still unconfirmed. Those keep
    /// their pixels (they are always at the FRONT, being older) while the new line's
    /// guesses stay dark — the password guarantee applies per guess, not per frame.
    fn visible_count(&self) -> usize {
        match self.mode {
            PredictMode::Off => 0,
            PredictMode::Always => self.preds.len(),
            PredictMode::Adaptive if !self.adaptive_slow => 0,
            PredictMode::Adaptive if self.confirmed_epoch => self.preds.len(),
            PredictMode::Adaptive => self
                .preds
                .iter()
                .take_while(|p| Some(p.epoch) == self.grandfathered_epoch)
                .count(),
        }
    }

    /// Any pending predictions. Superseded in the render early-out by
    /// [`is_displaying`](Self::is_displaying) (which also respects the display gate);
    /// retained for the unit tests.
    #[cfg(test)]
    pub(crate) fn is_active(&self) -> bool {
        !self.preds.is_empty()
    }

    /// NOTHING pending — the render paths' per-frame idle guard: with no
    /// guesses in flight (and no ghost still on glass, which the caller checks
    /// via its `pred_shown`) the whole per-present predict block is skipped —
    /// no config parse, no extra term-lock acquisition, no reconcile — so an
    /// idle predictor costs zero on every presented frame (Claude Code
    /// repaints per keystroke and its no-echo gate keeps this permanently
    /// empty there). Every flush site already leaves `preds` empty, so the
    /// guard can never strand a stale deadline or an unpainted erase.
    pub fn idle(&self) -> bool {
        self.preds.is_empty()
    }

    /// Whether predictions are actually being DISPLAYED (not merely tracked) — the
    /// render early-out consults this instead of `is_active` so Adaptive on a fast
    /// local link (where nothing shows) does not force a redundant repaint per
    /// keystroke. Returns true while the oldest guess is past its expiry window so
    /// [`overlay`](Self::overlay)'s flush still gets one cleanup frame (an erase only
    /// if that guess passed the display gate; hidden Adaptive state changes no pixels).
    pub fn is_displaying(&self, now: Instant) -> bool {
        match self.preds.first() {
            None => false,
            // The same window `expire` uses: these two disagreeing would either strand
            // an erase (host thinks nothing changed) or burn a frame per repaint while
            // the guess is still legitimately in flight.
            Some(oldest) => {
                now.saturating_duration_since(oldest.born) > self.glitch_window()
                    || self.visible_count() > 0
            }
        }
    }

    /// The instant the oldest pending guess self-expires (the glitch timeout). The host
    /// arms a `WaitUntil` on this so a stale prediction is cleaned up (and, if it was
    /// visible, repainted away) via [`overlay`](Self::overlay)'s expiry flush even when
    /// no further input or output arrives.
    pub fn next_deadline(&self) -> Option<Instant> {
        let window = self.glitch_window();
        self.preds.first().map(|p| p.born + window)
    }

    /// Drop all in-flight predictions (coordinate space changed: resize, font zoom,
    /// pane ⇄ split). The RTT estimate is retained; the confirmation epoch is forgotten
    /// so the next line re-confirms before displaying.
    pub fn reset(&mut self) {
        self.flush();
        self.epoch_row = None;
        // The coordinate space changed, so the last cursor/cell reading describes a
        // screen that no longer exists — re-latch it from the next real reconcile.
        self.deferred_wrap = false;
    }

    /// The grid SCROLLED between arming and echo (output pushed content up). Every
    /// pending guess is stored at an ABSOLUTE row, so a scroll silently slides the cell
    /// each one is waiting on.
    ///
    /// [`reconcile`](Self::reconcile)'s cursor-consistency check catches most of that —
    /// the real cursor scrolls along with the text, so it stops matching — but it runs
    /// AFTER the confirmation loop, which compares glyphs only: a scrolled-in cell
    /// holding the same character confirms an echo that never happened, feeding a bogus
    /// RTT sample and arming the epoch's display gate for a line that has not echoed.
    /// The guesses are not wrong, their COORDINATES are, and nothing in this crate can
    /// re-derive them from the host's grid — so the set is retired. Cheap no-op when
    /// nothing is pending, which is the case for every scroll that happens while the
    /// user is not typing ahead.
    pub fn note_scroll(&mut self) {
        self.reset();
    }

    /// Drop all predictor state when the host switches to a terminal session it has
    /// NO estimate for. Unlike [`reset`](Self::reset), this also forgets the learned
    /// echo RTT: latency belongs to the PTY/link that produced it, not to the window
    /// containing whichever pane happens to be focused next.
    ///
    /// This is the "nothing known about this link" path. A host that can KEEP what it
    /// learned per session should swap instead — [`take_link_estimate`](Self::take_link_estimate)
    /// / [`restore_link_estimate`](Self::restore_link_estimate) — because clearing on
    /// every front change makes a tab-heavy ssh workflow re-earn the display latch
    /// after each switch, leaving the first `SLOW_SAMPLES_TO_DISPLAY` characters of
    /// every line unpredicted for a link that never changed.
    pub fn reset_session(&mut self) {
        self.reset();
        self.srtt = None;
        self.slow_samples = 0;
        self.fast_samples = 0;
        self.adaptive_slow = false;
        // The cold-link probe is the same measurement as the RTT, one window wide: a
        // widened window that outlives its link makes an unknown pane wait up to the
        // ceiling before a wrong guess self-heals. Unknown link ⇒ probe from the floor.
        self.expiry_backoff = 0;
    }

    /// Take the learned link estimate OUT, leaving the predictor exactly as
    /// [`reset_session`](Self::reset_session) would (no guesses, no epoch, no
    /// estimate). The returned [`LinkEstimate`] is the OUTGOING session's property:
    /// park it in a per-session map and hand it back with
    /// [`restore_link_estimate`](Self::restore_link_estimate) when that session is in
    /// front again, instead of throwing away a measurement the link still satisfies.
    pub fn take_link_estimate(&mut self) -> LinkEstimate {
        let est = LinkEstimate {
            srtt: self.srtt,
            slow_samples: self.slow_samples,
            fast_samples: self.fast_samples,
            adaptive_slow: self.adaptive_slow,
            expiry_backoff: self.expiry_backoff,
        };
        // Route the clearing through `reset_session` itself so "what a take leaves
        // behind" and "what an unknown session starts with" can never drift apart.
        self.reset_session();
        est
    }

    /// Install the estimate of the session now in front. `LinkEstimate::default()` is
    /// the never-measured session and is therefore equivalent to
    /// [`reset_session`](Self::reset_session).
    ///
    /// The incoming session's LINK is known; its SCREEN is not — pending guesses and
    /// the confirmation epoch belong to the pane we just left, so they go. That is
    /// what keeps the no-unechoed-flash guarantee intact across a swap: a restored
    /// `adaptive_slow` only decides that speculation is WORTH showing on this link,
    /// never that the line now under the cursor has echoed. The epoch is still earned
    /// per line, on this line.
    pub fn restore_link_estimate(&mut self, est: LinkEstimate) {
        self.reset();
        self.srtt = est.srtt;
        self.slow_samples = est.slow_samples;
        self.fast_samples = est.fast_samples;
        self.adaptive_slow = est.adaptive_slow;
        self.expiry_backoff = est.expiry_backoff;
    }

    /// Clear pending predictions and end the confirmation epoch (display re-arms only
    /// after a fresh confirmation on the next line).
    fn flush(&mut self) {
        self.preds.clear();
        self.confirmed_epoch = false;
        self.anchor_lost = false; // nothing left to be anchored to
        self.grandfathered_epoch = None; // …and nothing left to carry pixels for
    }

    /// Fold a confirmed echo latency into the smoothed RTT estimate.
    fn record_rtt(&mut self, rtt: Duration) {
        let ms = rtt.as_secs_f32() * 1000.0;
        self.srtt = Some(match self.srtt {
            None => ms,
            Some(s) => s * (1.0 - SRTT_ALPHA) + ms * SRTT_ALPHA,
        });
        // A real echo landed, so the cold-link probe has served its purpose: retire the
        // backoff and let `srtt` alone size the window from here. Resetting here rather
        // than in `expire` is what makes the probe converge — silence widens it, and
        // exactly one confirmation collapses it.
        self.expiry_backoff = 0;
    }

    /// Fold one independent output turn into the stable visual classification.
    ///
    /// A LATCH, not a comparator. The thresholds are deliberately asymmetric and
    /// separated by a dead band: the old single 6 ms line sat inside aterm's own local
    /// echo distribution and closed on ONE fast sample, so on a jittery link the steady
    /// state was "off, with occasional flashes" — and because each flip erases whatever
    /// is mid-flight, the user saw the text they had just typed blink out and come back.
    /// Stability is worth more here than reaction speed in either direction.
    fn record_echo_regime(&mut self, rtt: Duration) {
        let ms = rtt.as_secs_f32() * 1000.0;
        if ms >= DISPLAY_OPEN_MS {
            self.fast_samples = 0;
            self.slow_samples = self
                .slow_samples
                .saturating_add(1)
                .min(SLOW_SAMPLES_TO_DISPLAY);
            if self.slow_samples >= SLOW_SAMPLES_TO_DISPLAY {
                self.adaptive_slow = true;
            }
        } else if ms < DISPLAY_CLOSE_MS {
            self.slow_samples = 0;
            self.fast_samples = self
                .fast_samples
                .saturating_add(1)
                .min(FAST_SAMPLES_TO_HIDE);
            // Three conditions, all required, because closing is the destructive
            // direction: a sustained streak (not one lucky turn), agreement from the
            // SMOOTHED estimate (a single raw sample is exactly the jitter we are
            // filtering), and an EMPTY pending set — a gate flip with guesses in flight
            // erases pixels that are already on glass, which is the blink itself.
            let smoothed_fast = self.srtt.is_none_or(|s| s < DISPLAY_CLOSE_MS);
            if self.fast_samples >= FAST_SAMPLES_TO_HIDE && smoothed_fast && self.preds.is_empty() {
                self.adaptive_slow = false;
            }
        }
        // Samples inside [DISPLAY_CLOSE_MS, DISPLAY_OPEN_MS) are evidence for neither
        // regime: they leave the latch AND both streaks alone, so "consecutive" counts
        // decisive turns rather than being reset by every ambiguous one (a link
        // oscillating around one threshold would otherwise never reach either latch).
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny fake grid for `observe`: maps (row,col) → glyph.
    fn cell(map: &[((u16, u16), char)]) -> impl Fn(u16, u16) -> Option<char> + '_ {
        move |r, c| {
            map.iter()
                .find(|((rr, cc), _)| *rr == r && *cc == c)
                .map(|(_, ch)| *ch)
        }
    }

    fn t0() -> Instant {
        Instant::now()
    }

    /// One complete type→echo turn on row 0: arm `ch` at `col` at `at`, then confirm it
    /// `rtt` later with the cursor advanced. Returns the confirmation instant. Only
    /// valid with an empty pending set (it anchors on the passed cursor).
    fn echo_turn(p: &mut Predictor, ch: char, col: u16, at: Instant, rtt: Duration) -> Instant {
        assert!(p.predict_char(ch, (0, col), 80, at));
        let done = at + rtt;
        p.reconcile(Some((0, col + 1)), false, done, cell(&[((0, col), ch)]));
        done
    }

    #[test]
    fn off_mode_predicts_nothing() {
        let mut p = Predictor::new(PredictMode::Off);
        assert!(!p.predict_char('a', (0, 0), 80, t0()));
        assert!(!p.is_active());
    }

    #[test]
    fn sustained_slow_echo_retires_predictions_and_displays_next() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), 80, now)); // pred 'a' at (0,0)
        assert!(p.is_active());
        // One 60 ms sample retires the prediction but does not trust a possibly
        // isolated scheduler tail enough to paint the next one.
        let first = now + Duration::from_millis(60);
        p.reconcile(Some((0, 1)), false, first, cell(&[((0, 0), 'a')]));
        assert!(!p.is_active(), "confirmed prediction is retired");
        assert!(p.predict_char('b', (0, 1), 80, first));
        assert!(p.overlay(first).is_empty(), "one slow sample stays hidden");

        // A second consecutive slow echo establishes the remote-latency regime.
        let second = first + Duration::from_millis(60);
        p.reconcile(Some((0, 2)), false, second, cell(&[((0, 1), 'b')]));
        assert!(p.predict_char('c', (0, 2), 80, second));
        let shown = p.overlay(second);
        assert_eq!(shown.len(), 1);
        assert_eq!((shown[0].row, shown[0].col, shown[0].ch), (0, 2, 'c'));
    }

    #[test]
    fn types_ahead_extends_from_last_guess_before_echo() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        // Both typed before any echo: cursor is still (0,0) for the 2nd, but 'b'
        // must extend past 'a', not land on top of it.
        p.predict_char('a', (0, 0), 80, now);
        p.predict_char('b', (0, 0), 80, now);
        let shown = p.overlay(now);
        assert_eq!(shown.len(), 2);
        assert_eq!((shown[0].col, shown[0].ch), (0, 'a'));
        assert_eq!((shown[1].col, shown[1].ch), (1, 'b'));
    }

    #[test]
    fn adaptive_hides_on_fast_local_echo() {
        // On a fast local shell the real echo wins within one frame. Speculation has no
        // visible benefit and creates a failure mode where a later scheduler tail paints
        // a glyph and then erases it at GLITCH_MS.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        p.predict_char('a', (0, 0), 80, now);
        // Echo lands in 1 ms — local/instant — confirming the epoch but keeping the
        // Adaptive RTT gate closed.
        let fast = now + Duration::from_millis(1);
        p.reconcile(Some((0, 1)), false, fast, cell(&[((0, 0), 'a')]));
        p.predict_char('b', (0, 1), 80, fast);
        assert!(
            p.overlay(fast).is_empty(),
            "fast echo ⇒ nothing displayed (no benefit, no flicker)"
        );
    }

    #[test]
    fn adaptive_isolated_scheduler_tail_does_not_open_gate() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();

        // Establish the ordinary local regime, then inject one 60 ms callback tail.
        assert!(p.predict_char('a', (0, 0), 80, now));
        let fast = now + Duration::from_millis(1);
        p.reconcile(Some((0, 1)), false, fast, cell(&[((0, 0), 'a')]));
        assert!(p.predict_char('b', (0, 1), 80, fast));
        assert!(p.predict_char('c', (0, 1), 80, fast));
        let tail = fast + Duration::from_millis(60);
        p.reconcile(
            Some((0, 3)),
            false,
            tail,
            cell(&[((0, 1), 'b'), ((0, 2), 'c')]),
        );

        assert!(p.predict_char('d', (0, 3), 80, tail));
        assert!(
            p.overlay(tail).is_empty(),
            "one delayed scheduler turn must not manufacture speculative pixels, even when it retires a burst"
        );

        // The next fast confirmation clears the candidate streak as well as keeping
        // the display latch closed.
        let local_again = tail + Duration::from_millis(1);
        p.reconcile(Some((0, 4)), false, local_again, cell(&[((0, 3), 'd')]));
        assert!(p.predict_char('e', (0, 4), 80, local_again));
        assert!(p.overlay(local_again).is_empty());
    }

    #[test]
    fn adaptive_slow_to_fast_closes_only_after_a_sustained_fast_streak() {
        // Leaving ssh for a local foreground process must still stop painting — but
        // NOT on one fast sample. A single-sample close makes the steady state on any
        // jittery link "off with occasional flashes", and each flip erases whatever is
        // mid-flight, so the user watches their own typing blink out and return.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let fast = Duration::from_millis(1);

        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.predict_char('c', (0, 2), 80, at));
        assert_eq!(p.overlay(at).len(), 1, "sustained remote latency opens");
        p.reconcile(Some((0, 3)), false, at + fast, cell(&[((0, 2), 'c')]));
        at += fast;

        // Two more fast turns: the raw streak is now long enough, but the SMOOTHED
        // estimate is still remote-sized, so the latch holds.
        at = echo_turn(&mut p, 'd', 3, at, fast);
        at = echo_turn(&mut p, 'e', 4, at, fast);
        assert!(p.srtt.is_some_and(|s| s >= DISPLAY_CLOSE_MS));
        assert!(
            p.adaptive_slow,
            "a raw streak alone must not close the gate"
        );

        // Keep echoing locally: once the EWMA itself agrees, the gate closes.
        for col in 5..14 {
            at = echo_turn(&mut p, 'x', col, at, fast);
        }
        assert!(!p.adaptive_slow, "a settled local link stops painting");
        assert!(p.predict_char('z', (0, 14), 80, at));
        assert!(p.overlay(at).is_empty());
    }

    #[test]
    fn adaptive_gate_does_not_flap_across_a_single_fast_sample() {
        // REGRESSION (P2): the old classifier opened on two samples ≥ 6 ms and closed
        // on ONE below it — thresholds inside aterm's own local-echo distribution. A
        // link alternating 30 ms / 5 ms therefore spent most frames dark and erased a
        // ghost on every dip. Hysteresis: one fast dip in a slow link changes nothing.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(30);

        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.adaptive_slow, "control: the slow link opened the gate");

        at = echo_turn(&mut p, 'c', 2, at, Duration::from_millis(5)); // one jitter dip
        assert!(p.adaptive_slow, "one fast dip must not blank the overlay");
        at = echo_turn(&mut p, 'd', 3, at, slow);
        assert!(p.predict_char('e', (0, 4), 80, at));
        assert_eq!(
            p.overlay(at).len(),
            1,
            "the slow link keeps painting across its own jitter"
        );
    }

    #[test]
    fn adaptive_gate_never_closes_while_a_guess_is_in_flight() {
        // Closing is the destructive direction: it un-paints pixels that are already on
        // glass. Even a fully-settled fast streak must wait for the pending set to
        // drain, or the very turn that decides "prediction is not needed" is the turn
        // that blinks out the guess the user is currently looking at.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let fast = Duration::from_millis(1);

        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.adaptive_slow);

        // Every turn confirms one guess while the user is one glyph ahead, so the
        // pending set is never empty at classification time.
        let mut col = 2u16;
        for _ in 0..14 {
            assert!(p.predict_char('x', (0, col), 80, at));
            assert!(p.predict_char('y', (0, col + 1), 80, at));
            at += fast;
            p.reconcile(Some((0, col + 1)), false, at, cell(&[((0, col), 'x')]));
            assert!(p.predict_backspace(at), "retire the type-ahead glyph");
            col += 1;
        }
        assert!(
            p.srtt.is_some_and(|s| s < DISPLAY_CLOSE_MS),
            "control: the smoothed estimate is now unambiguously local"
        );
        assert!(
            p.adaptive_slow,
            "a gate flip with pixels in flight is exactly the visible blink"
        );

        // Drained: the same evidence now closes it.
        let _ = echo_turn(&mut p, 'q', col, at, fast);
        assert!(!p.adaptive_slow);
    }

    #[test]
    fn burst_is_classified_by_the_oldest_retired_guess() {
        // REGRESSION (P3): `preds` is in type order, so the LAST guess retired in a
        // turn is the newest and always the smallest sample. Classifying by the minimum
        // meant a five-glyph burst echoed in one 45 ms turn reported 5 ms — the faster
        // the user typed, the more certain the shutoff. The user waited 45 ms for the
        // FIRST unechoed glyph; that is the latency prediction exists to hide.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let burst = |p: &mut Predictor, start: Instant, base: u16| -> Instant {
            for i in 0..5u16 {
                assert!(p.predict_char(
                    'a',
                    (0, base + i),
                    80,
                    start + Duration::from_millis(u64::from(i) * 10)
                ));
            }
            let echoed: Vec<((u16, u16), char)> = (0..5u16).map(|i| ((0, base + i), 'a')).collect();
            let done = start + Duration::from_millis(45);
            p.reconcile(Some((0, base + 5)), false, done, cell(&echoed));
            done
        };

        let after = burst(&mut p, now, 0);
        let after = burst(&mut p, after, 5);
        assert!(
            p.adaptive_slow,
            "two 45 ms turns are a slow link no matter how fast the user types"
        );
        assert!(p.predict_char('z', (0, 10), 80, after));
        assert_eq!(p.overlay(after).len(), 1);
    }

    #[test]
    fn adaptive_local_tail_expires_without_a_visible_erase() {
        // Regression: aterm used to paint `b` immediately after one fast local echo,
        // then flush it on the 250 ms deadline if the PTY/main loop had a long tail.
        // To the user that looked like newly typed text blinking out. Under-threshold
        // Adaptive guesses remain tracked for reconciliation but are never pixels, so
        // crossing the deadline is an invisible state cleanup rather than an erase.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        p.predict_char('a', (0, 0), 80, now);
        let fast = now + Duration::from_millis(1);
        p.reconcile(Some((0, 1)), false, fast, cell(&[((0, 0), 'a')]));
        assert!(p.predict_char('b', (0, 1), 80, fast));
        assert!(!p.is_displaying(fast));
        assert!(
            p.overlay(fast).is_empty(),
            "no speculative pixel was painted"
        );

        let after_deadline = fast + Duration::from_millis(GLITCH_FLOOR_MS + 1);
        // `is_displaying` deliberately returns true for one cleanup frame once any
        // pending guess expires, even if the display gate stayed closed. The visual
        // contract is the overlay trace: empty before the deadline and still empty
        // while expiry removes the hidden bookkeeping state.
        assert!(p.overlay(after_deadline).is_empty());
        assert!(!p.is_active(), "the hidden stale guess still self-heals");
        assert!(
            p.next_deadline().is_none(),
            "invisible expiry disarms its timer"
        );
    }

    #[test]
    fn always_mode_shows_immediately_without_confirmation() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('x', (0, 0), 80, now);
        assert_eq!(p.overlay(now).len(), 1, "Always shows the first keystroke");
    }

    #[test]
    fn divergence_flushes_all() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), 80, now);
        // The program echoed a DIFFERENT glyph (e.g. a completion/masked char).
        p.reconcile(Some((0, 1)), false, now, cell(&[((0, 0), '*')]));
        assert!(
            !p.is_active(),
            "a wrong guess flushes the whole prediction set"
        );
    }

    #[test]
    fn alt_screen_flushes_predictions() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), 80, now);
        assert!(p.is_active());
        // Entering vim/less (alternate screen) drops predictions.
        p.reconcile(Some((10, 4)), true, now, cell(&[]));
        assert!(!p.is_active());
    }

    #[test]
    fn unconfirmed_prediction_self_expires() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), 80, now);
        assert_eq!(p.overlay(now).len(), 1);
        // No echo ever arrives; after the glitch window the guess is dropped.
        let late = now + Duration::from_millis(GLITCH_FLOOR_MS + 50);
        assert!(p.overlay(late).is_empty());
        assert!(!p.is_active());
    }

    #[test]
    fn expired_prediction_clears_next_deadline() {
        // REGRESSION (scrolled-back busy loop): a prediction in flight when the
        // user scrolls into history must still self-heal. `overlay`'s expiry flush
        // is the only thing that disarms `next_deadline()`; if it never runs, the
        // deadline stays pinned at a past instant and the host's WaitUntil-driven
        // repaint spins at 100% CPU. After the glitch window, overlay must flush
        // AND next_deadline() must become None (the wake self-disarms).
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), 80, now);
        assert!(
            p.next_deadline().is_some(),
            "a pending guess arms a deadline"
        );
        let late = now + Duration::from_millis(GLITCH_FLOOR_MS + 50);
        let _ = p.overlay(late); // the same flush the scrolled-back branch now runs
        assert!(!p.is_active(), "stale guess flushed past the glitch window");
        assert!(
            p.next_deadline().is_none(),
            "after the expiry flush the deadline self-disarms (no permanently-past wake)"
        );
    }

    #[test]
    fn backspace_cancels_pending_prediction() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), 80, now);
        p.predict_char('b', (0, 0), 80, now);
        assert_eq!(p.overlay(now).len(), 2);
        assert!(p.predict_backspace(now));
        assert_eq!(p.overlay(now).len(), 1, "backspace removes the last guess");
        assert_eq!(p.overlay(now)[0].ch, 'a');
    }

    #[test]
    fn height_blind_hosts_decline_at_the_margin_without_flushing() {
        // The legacy 4-argument seam has no grid HEIGHT, so it cannot know whether a
        // wrap lands on a real row; it still declines. What it must not do is flush:
        // the guesses to the LEFT of the margin are exactly what the program will echo.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('a', (0, 78), 80, now)); // col 78 ok
        assert!(p.predict_char('b', (0, 78), 80, now)); // extends to col 79 (last column)
        assert!(
            !p.predict_char('c', (0, 78), 80, now),
            "col 80 would wrap and no row height was supplied → declined"
        );
        assert_eq!(
            p.overlay(now).len(),
            2,
            "declining the wrap must not erase the two glyphs already on glass"
        );
    }

    #[test]
    fn wraps_to_the_next_row_instead_of_flushing() {
        // REGRESSION (P6): prediction used to stop at the right margin — on long
        // command lines, which is where an ssh user wants type-ahead MOST. The program
        // will echo the next glyph at column 0 of the following row; guessing that is
        // no more speculative than any other guess.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (0, 79), (80, 24), now));
        assert!(p.predict_char_in_grid('b', (0, 79), (80, 24), now));
        let shown = p.overlay(now);
        assert_eq!(shown.len(), 2);
        assert_eq!((shown[0].row, shown[0].col), (0, 79));
        assert_eq!(
            (shown[1].row, shown[1].col, shown[1].ch),
            (1, 0, 'b'),
            "the wrapped glyph continues on the next row"
        );
    }

    #[test]
    fn wrapped_guess_survives_the_deferred_wrap_cursor() {
        // A terminal DEFERS the wrap: after echoing the final column the cursor is
        // still parked there and only moves to (row+1, 0) when the next glyph arrives.
        // The strict cursor-consistency check read that as a divergence and flushed
        // every wrapped guess one output burst after it was armed.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (0, 79), (80, 24), now));
        assert!(p.predict_char_in_grid('b', (0, 79), (80, 24), now));
        // The echo of 'a' lands; the cursor stays in the deferred-wrap slot (0,79).
        p.reconcile(Some((0, 79)), false, now, cell(&[((0, 79), 'a')]));
        let shown = p.overlay(now);
        assert_eq!(shown.len(), 1, "'a' retires, the wrapped 'b' survives");
        assert_eq!((shown[0].row, shown[0].col, shown[0].ch), (1, 0, 'b'));
    }

    #[test]
    fn bottom_row_declines_without_flushing() {
        // The bottom edge is genuinely unmodellable — the grid SCROLLS, moving every
        // pending guess — so we decline there. The pending set stays: those cells are
        // above the wrap and unaffected.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (23, 79), (80, 24), now));
        assert!(
            !p.predict_char_in_grid('b', (23, 79), (80, 24), now),
            "there is no row 24 to wrap into"
        );
        assert_eq!(p.overlay(now).len(), 1, "the armed glyph keeps its pixels");
    }

    #[test]
    fn wide_glyphs_are_declined_but_narrow_accents_are_predicted() {
        // Width, not ASCII-ness, is the criterion. `é` occupies one cell and echoes
        // like any letter; CJK/emoji take two and combining marks alter the cell to
        // their left, so their echo geometry is not ours to place.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(
            p.predict_char('é', (0, 0), 80, now),
            "single-width accented Latin is ordinary self-echoing input"
        );
        assert!(p.predict_char('ü', (0, 0), 80, now));
        assert_eq!(p.overlay(now).len(), 2);
        assert!(
            !p.predict_char('日', (0, 0), 80, now),
            "wide/CJK glyphs are left to real echo"
        );
        assert!(
            !p.predict_char('\u{0301}', (0, 0), 80, now),
            "a combining mark modifies the cell to its LEFT — not a new cell"
        );
    }

    #[test]
    fn a_declined_glyph_does_not_erase_the_word_to_its_left() {
        // REGRESSION (P4): refusal used to flush, so on a French/German/Spanish/UK
        // layout essentially every word wiped the in-flight set AND the confirmation
        // epoch. A character we cannot model says nothing about the guesses anchored to
        // its left — but it does cost us the ANCHOR, so we stop extending until the set
        // drains and the live cursor (which the real echo moves) re-anchors us.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), 80, now));
        assert!(!p.predict_char('日', (0, 0), 80, now));
        assert_eq!(p.overlay(now).len(), 1, "'a' keeps its pixels");
        assert!(
            !p.predict_char('b', (0, 0), 80, now),
            "we cannot place a glyph past a wide echo we did not model"
        );
        assert_eq!(
            p.overlay(now).len(),
            1,
            "…and declining that changes nothing"
        );
        // The real echo lands and drains the set: the live cursor is authoritative
        // again, so prediction resumes.
        p.reconcile(Some((0, 3)), false, now, cell(&[((0, 0), 'a')]));
        assert!(p.predict_char('c', (0, 3), 80, now));
        assert_eq!(p.overlay(now)[0].col, 3);
    }

    #[test]
    fn mode_parse_is_fail_safe() {
        assert_eq!(PredictMode::parse("adaptive"), PredictMode::Adaptive);
        assert_eq!(PredictMode::parse("ALWAYS"), PredictMode::Always);
        assert_eq!(PredictMode::parse("off"), PredictMode::Off);
        assert_eq!(PredictMode::parse("nonsense"), PredictMode::Off);
    }

    #[test]
    fn fresh_line_resets_confirmation_no_leak() {
        // Adaptive confirms two guesses on row 0 (stable slow link), then the user reaches a
        // PASSWORD prompt on a NEW row where typing never echoes. The new line must
        // start a fresh epoch so the unechoed secret is never displayed.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        p.predict_char('a', (0, 0), 80, now);
        let t1 = now + Duration::from_millis(50);
        p.reconcile(Some((0, 1)), false, t1, cell(&[((0, 0), 'a')]));
        p.predict_char('b', (0, 1), 80, t1);
        let t2 = t1 + Duration::from_millis(50);
        p.reconcile(Some((0, 2)), false, t2, cell(&[((0, 1), 'b')]));
        assert!(
            p.adaptive_slow,
            "control: sustained slow echo opened the latch"
        );
        p.predict_char('s', (1, 9), 80, t2); // row 1 != epoch row 0 → fresh epoch
        assert!(
            p.overlay(t2).is_empty(),
            "an unechoed char on a new line must NOT display (no password leak)"
        );
    }

    #[test]
    fn submit_resets_epoch_same_row_no_password_leak() {
        // REGRESSION: on a terminal SCROLLED TO THE BOTTOM the cursor stays on ONE
        // physical row across logical lines, so the epoch-row reset never fires.
        // Type+confirm a command on a predictably slow link, press Enter
        // (`note_line_submit`), then a NON-echoing password prompt lands on the SAME
        // row. Its first keystroke must NOT display.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let r = 23u16; // bottom physical row, reused across BOTH logical lines
        // Line 1: two slow echoes establish the link, then 'c' shows.
        p.predict_char('a', (r, 0), 80, now);
        let first = now + Duration::from_millis(60);
        p.reconcile(Some((r, 1)), false, first, cell(&[((r, 0), 'a')]));
        p.predict_char('b', (r, 1), 80, first);
        let confirmed = first + Duration::from_millis(60);
        p.reconcile(Some((r, 2)), false, confirmed, cell(&[((r, 1), 'b')]));
        p.predict_char('c', (r, 2), 80, confirmed);
        assert_eq!(
            p.overlay(confirmed).len(),
            1,
            "confirmed slow epoch on row r shows the next keystroke"
        );
        // Submit (Enter): ends the epoch for FUTURE guesses. The already-painted 'c'
        // keeps its pixels (it belongs to a line that proved it echoes), which is the
        // whole point of not flushing here.
        p.note_line_submit();
        assert_eq!(
            p.overlay(confirmed).len(),
            1,
            "a guess from the confirmed line keeps its pixels across the submit"
        );
        // Line 2 (password prompt) on the SAME physical row r: never echoes.
        let t1 = confirmed + Duration::from_millis(50);
        assert!(p.predict_char('s', (r, 9), 80, t1)); // predicted (tracked)…
        let shown = p.overlay(t1);
        assert!(
            shown.iter().all(|g| g.ch != 's'),
            "after submit, an unechoed char on the SAME physical row must NOT display (no leak)"
        );
    }

    #[test]
    fn submit_does_not_blank_the_tail_of_the_command_line() {
        // REGRESSION (P5): Enter used to flush, so on a slow link the last few typed
        // characters vanished at the exact instant of commit and reappeared one RTT
        // later — the most attention-grabbing moment of the interaction, made to feel
        // laggier than having no prediction at all.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('l', (0, 0), 80, now));
        assert!(p.predict_char('s', (0, 0), 80, now));
        assert_eq!(p.overlay(now).len(), 2);
        p.note_line_submit();
        assert_eq!(
            p.overlay(now).len(),
            2,
            "the unechoed tail of the submitted line stays on glass"
        );
        assert!(
            p.next_deadline().is_some(),
            "…and still self-heals on its own deadline"
        );
    }

    #[test]
    fn a_stale_guess_confirming_after_submit_cannot_arm_the_new_line() {
        // The password guarantee, pinned against the P5 change: keeping in-flight
        // guesses across the boundary means one can CONFIRM after it. That confirmation
        // measures the link (legitimately) but must not arm the new line's display
        // gate, or the secret typed at the prompt below would show.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let r = 23u16; // bottom row, reused across both logical lines
        let slow = Duration::from_millis(60);
        // Establish a slow link on line 1 so the RTT latch is open.
        assert!(p.predict_char('a', (r, 0), 80, now));
        let t1 = now + slow;
        p.reconcile(Some((r, 1)), false, t1, cell(&[((r, 0), 'a')]));
        assert!(p.predict_char('b', (r, 1), 80, t1));
        let t2 = t1 + slow;
        p.reconcile(Some((r, 2)), false, t2, cell(&[((r, 1), 'b')]));
        assert!(p.adaptive_slow, "control: the link is classified slow");

        // Type one more glyph, hit Enter while it is still unechoed, then let its echo
        // land AFTER the boundary.
        assert!(p.predict_char('c', (r, 2), 80, t2));
        p.note_line_submit();
        let t3 = t2 + slow;
        p.reconcile(Some((r, 3)), false, t3, cell(&[((r, 2), 'c')]));
        assert!(p.idle(), "the stale guess retired normally");
        assert!(
            !p.confirmed_epoch,
            "a pre-submit confirmation must not arm the new line"
        );

        // The password prompt lands on the SAME physical row and never echoes.
        assert!(p.predict_char('s', (r, 9), 80, t3));
        assert!(
            p.overlay(t3).is_empty(),
            "an unechoed secret on the new line must never display"
        );
    }

    #[test]
    fn slow_link_expiry_window_grows_until_the_echo_can_confirm() {
        // REGRESSION (P1): with a FIXED 250 ms expiry, every guess on a link slower
        // than that was flushed before its echo arrived — and the flush also cleared
        // `confirmed_epoch`, so `reconcile` always found an empty set, no RTT sample
        // was ever recorded, and the adaptive latch could never open. The feature was
        // dead in exactly the regime it exists for. A timeout now WIDENS the probe
        // window until the echo fits inside it — but is NOT itself evidence about the
        // link: only real confirmations classify the regime, so opening the gate still
        // takes `SLOW_SAMPLES_TO_DISPLAY` genuine slow echoes. (Counting timeouts as
        // slow samples put a ghost on screen after two unechoed keystrokes at a
        // PASSWORD PROMPT on a 1 ms local link, and fed `3*srtt` back into `srtt` so
        // the window diverged to the 2 s ceiling.)
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let rtt = Duration::from_millis(400);

        // First keystroke on a cold 400 ms link: nothing confirms it in the floor
        // window, so it expires — recording a LOWER BOUND on the link's latency.
        assert!(p.predict_char('a', (0, 0), 80, now));
        let _ = p.overlay(now + Duration::from_millis(GLITCH_FLOOR_MS + 1));
        assert!(!p.is_active(), "the unconfirmed guess still self-heals");
        assert!(
            p.glitch_window() > Duration::from_millis(GLITCH_FLOOR_MS),
            "a timeout widens the window instead of repeating forever"
        );

        // Second keystroke: the widened window now outlasts the real 400 ms echo, so
        // the guess is still in flight when its echo lands and CONFIRMS.
        let armed = now + Duration::from_millis(500);
        assert!(p.predict_char('b', (0, 0), 80, armed));
        let _ = p.overlay(armed + Duration::from_millis(300)); // a frame mid-flight
        assert!(p.is_active(), "the guess outlives the old fixed window");
        p.reconcile(Some((0, 1)), false, armed + rtt, cell(&[((0, 0), 'b')]));
        assert!(p.confirmed_epoch, "the echo finally reaches a live guess");
        assert!(
            !p.adaptive_slow,
            "ONE confirmation is not a regime: silence must never substitute for the \
             second slow sample (that is what painted a ghost at a password prompt)"
        );
        // The confirmation also collapses the probe back to the floor — the estimate is
        // now doing the sizing, so the backoff must not keep the window inflated.
        assert_eq!(
            p.expiry_backoff, 0,
            "a real echo retires the cold-link probe"
        );

        // A SECOND genuine slow confirmation is what opens the gate.
        let mut at = armed + rtt;
        assert!(p.predict_char('c', (0, 1), 80, at));
        at += rtt;
        p.reconcile(Some((0, 2)), false, at, cell(&[((0, 1), 'c')]));
        assert!(
            p.adaptive_slow,
            "two real slow echoes classify the link — the honest evidence path"
        );

        // …and that is the whole point: the next keystroke is painted immediately.
        assert!(p.predict_char('d', (0, 2), 80, at));
        assert_eq!(p.overlay(at).len(), 1, "a 400 ms link finally predicts");
    }

    /// REGRESSION (P1 review): an endlessly non-echoing context — a password prompt,
    /// a raw-mode key — must never accumulate "slow" evidence. The real cursor does
    /// not move there, so `reconcile`'s consistency check matches and every guess
    /// reaches the expiry path; an earlier fix counted each of those as a slow echo
    /// sample and latched speculation ON after two keystrokes, on a LOCAL link.
    #[test]
    fn silence_never_opens_the_gate_or_diverges_the_estimate() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut now = t0();
        for _ in 0..12 {
            assert!(p.predict_char('x', (0, 0), 80, now));
            now += Duration::from_millis(GLITCH_CEILING_MS + 10);
            assert!(
                p.overlay(now).is_empty(),
                "an unechoed guess is never displayed on a link that never echoed"
            );
        }
        assert!(!p.adaptive_slow, "silence is not evidence of a slow link");
        assert!(!p.confirmed_epoch, "nothing was ever confirmed");
        assert_eq!(p.srtt, None, "timeouts must not seed the RTT estimate");
        assert!(
            p.glitch_window() <= Duration::from_millis(GLITCH_CEILING_MS),
            "the probe is bounded, never divergent"
        );
    }

    #[test]
    fn expiry_retires_only_the_oldest_guess_and_keeps_the_epoch() {
        // The whole set used to go, along with `confirmed_epoch`. The tail is YOUNGER
        // than the head and has not earned its timeout, and dropping the epoch made the
        // next line re-prove an echo that this line had already proved.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), 80, now));
        let later = now + Duration::from_millis(200);
        assert!(p.predict_char('b', (0, 0), 80, later));
        let shown = p.overlay(now + Duration::from_millis(GLITCH_FLOOR_MS + 10));
        assert_eq!(shown.len(), 1, "only the expired head is retired");
        assert_eq!(shown[0].ch, 'b');

        // Adaptive: the confirmation earned on this line survives a later expiry.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.confirmed_epoch && p.adaptive_slow);
        assert!(p.predict_char('c', (0, 2), 80, at));
        let expired = at + Duration::from_millis(GLITCH_FLOOR_MS + 10);
        assert!(p.overlay(expired).is_empty(), "the stale guess expired");
        assert!(
            p.confirmed_epoch,
            "expiry must not make this line re-prove an echo it already proved"
        );
        assert!(p.predict_char('d', (0, 2), 80, expired));
        assert_eq!(p.overlay(expired).len(), 1, "the next guess still paints");
    }

    #[test]
    fn session_switch_forgets_slow_link_rtt() {
        // Learn a useful RTT on a remote pane, then reuse the same Predictor for a
        // newly focused local pane. The first fast echo there may confirm the line,
        // but it must not inherit the remote pane's display eligibility.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), 80, now));
        let slow1 = now + Duration::from_millis(60);
        p.reconcile(Some((0, 1)), false, slow1, cell(&[((0, 0), 'a')]));
        assert!(p.predict_char('b', (0, 1), 80, slow1));
        let slow2 = slow1 + Duration::from_millis(60);
        p.reconcile(Some((0, 2)), false, slow2, cell(&[((0, 1), 'b')]));
        assert!(p.predict_char('c', (0, 2), 80, slow2));
        assert_eq!(p.overlay(slow2).len(), 1, "control: slow link displays");

        p.reset_session();
        let local = slow2 + Duration::from_millis(1);
        assert!(p.predict_char('x', (0, 0), 80, slow2));
        p.reconcile(Some((0, 1)), false, local, cell(&[((0, 0), 'x')]));
        assert!(p.predict_char('y', (0, 1), 80, local));
        assert!(
            p.overlay(local).is_empty(),
            "a new session learns its own fast RTT instead of inheriting the old slow link"
        );
    }

    #[test]
    fn the_deferred_wrap_slot_is_not_a_free_cell() {
        // The line reached the right margin and the echo caught up, so the pending set
        // is empty and the next keystroke anchors on the LIVE cursor — which the
        // terminal has parked in the deferred-wrap slot, ON TOP of the glyph it just
        // echoed. Guessing there painted a wrong character over a correct one on every
        // command line long enough to wrap (the case the wrap lane exists for), and
        // then flushed the whole set a burst later when the real cell diverged.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (0, 79), (80, 24), now));
        p.reconcile(Some((0, 79)), false, now, cell(&[((0, 79), 'a')]));
        assert!(p.idle(), "control: the echo retired the guess");

        assert!(p.predict_char_in_grid('b', (0, 79), (80, 24), now));
        let shown = p.overlay(now);
        assert_eq!(
            (shown[0].row, shown[0].col, shown[0].ch),
            (1, 0, 'b'),
            "a parked cursor wraps the next glyph to the following row"
        );
    }

    #[test]
    fn a_free_last_column_is_still_predicted_in_place() {
        // The complement, and the reason the parking is read from the CELL rather than
        // from the cursor column: same position, empty cell, nothing parked. The
        // program will put the next glyph exactly there, so wrapping it would leave a
        // hole at the margin and shift the whole tail of the line one cell.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (0, 78), (80, 24), now));
        p.reconcile(Some((0, 79)), false, now, cell(&[((0, 78), 'a')]));
        assert!(p.idle());
        assert!(p.predict_char_in_grid('b', (0, 79), (80, 24), now));
        let shown = p.overlay(now);
        assert_eq!((shown[0].row, shown[0].col), (0, 79));
    }

    #[test]
    fn a_deferred_wrap_on_the_last_row_declines_without_painting() {
        // There is no row to wrap INTO, and the parked cell is echoed content that is
        // not ours to overwrite — so the only honest answer is to decline. (The
        // anchor-extended twin of this edge is `bottom_row_declines_without_flushing`.)
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (23, 79), (80, 24), now));
        p.reconcile(Some((23, 79)), false, now, cell(&[((23, 79), 'a')]));
        assert!(
            !p.predict_char_in_grid('b', (23, 79), (80, 24), now),
            "no row 24 to wrap into, and the slot holds the echoed 'a'"
        );
        assert!(p.idle(), "declining paints nothing and erases nothing");
    }

    #[test]
    fn a_wrap_the_terminal_did_not_defer_keeps_the_wrapped_guess() {
        // Not every wrap is parked: a program that emits the next glyph in the same
        // burst (or a host sampling the cursor after it) reports (row+1, 0) directly.
        // The wrapped guess sits exactly there, so the PLAIN cursor-consistency arm
        // must accept it — the deferred-wrap tolerance is an addition, not a
        // replacement, and a wrapped guess must not depend on the parking to survive.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (0, 79), (80, 24), now));
        assert!(p.predict_char_in_grid('b', (0, 79), (80, 24), now)); // wraps to (1,0)
        p.reconcile(Some((1, 0)), false, now, cell(&[((0, 79), 'a')]));
        let shown = p.overlay(now);
        assert_eq!(
            shown.len(),
            1,
            "'a' retires; the wrapped 'b' is no divergence"
        );
        assert_eq!((shown[0].row, shown[0].col, shown[0].ch), (1, 0, 'b'));
    }

    #[test]
    fn a_scrolled_cursor_does_not_pass_for_the_deferred_wrap_slot() {
        // Guesses carry ABSOLUTE rows, so a scroll between arming and echo slides the
        // cell every one of them is waiting on. The cursor scrolls WITH the text, so
        // the deferred-wrap tolerance must keep insisting on the exact previous row:
        // a one-row-off match would hold a wrapped guess against a cell that now
        // belongs to a different line.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (5, 79), (80, 24), now));
        assert!(p.predict_char_in_grid('b', (5, 79), (80, 24), now)); // wraps to (6,0)
        p.reconcile(Some((5, 79)), false, now, cell(&[((5, 79), 'a')]));
        assert_eq!(p.overlay(now).len(), 1, "control: the park keeps 'b' alive");

        // One line of output scrolls the grid: the typed line — and the cursor parked
        // at its margin — are a row higher, and row 6 is still unwritten.
        p.reconcile(Some((4, 79)), false, now, cell(&[((4, 79), 'a')]));
        assert!(p.idle(), "a stale absolute row is retired, not tolerated");
    }

    #[test]
    fn a_scroll_retires_guesses_instead_of_confirming_them_against_moved_cells() {
        // The confirmation loop compares GLYPHS at absolute rows and runs BEFORE the
        // cursor-consistency check, so a scroll that slides another line's identical
        // character under a pending guess confirms an echo that never happened: a bogus
        // RTT sample, and (Adaptive) a display gate armed by a line that never echoed.
        // Only the host knows the grid scrolled; the guesses are not wrong, their
        // COORDINATES are, and nothing in this crate can re-derive them.
        let now = t0();
        let mut unaware = Predictor::new(PredictMode::Adaptive);
        assert!(unaware.predict_char_in_grid('a', (5, 0), (80, 24), now));
        unaware.reconcile(Some((5, 1)), false, now, cell(&[((5, 0), 'a')]));
        assert!(
            unaware.confirmed_epoch,
            "control: a glyph-equal cell really does confirm"
        );

        let mut p = Predictor::new(PredictMode::Adaptive);
        assert!(p.predict_char_in_grid('a', (5, 0), (80, 24), now));
        p.note_scroll();
        assert!(p.idle(), "a scrolled guess is retired, not re-aimed");
        p.reconcile(Some((5, 1)), false, now, cell(&[((5, 0), 'a')]));
        assert!(
            !p.confirmed_epoch,
            "…so a moved cell can never arm the display gate"
        );
    }

    #[test]
    fn a_backgrounded_session_keeps_its_link_estimate() {
        // The link RTT is a property of the SESSION, not of the window. The host calls
        // its front-change path on every tab switch, so CLEARING the estimate there
        // made a tab-heavy ssh workflow re-earn the display latch after each switch —
        // the first SLOW_SAMPLES_TO_DISPLAY characters of every line unpredicted, for a
        // link that never changed.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let fast = Duration::from_millis(1);
        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.predict_char('c', (0, 2), 80, at));
        assert_eq!(p.overlay(at).len(), 1, "control: the slow link paints");

        // The ssh session goes to the background; its estimate leaves with it.
        let ssh = p.take_link_estimate();
        assert!(
            p.idle(),
            "the outgoing pane's guesses do not follow the window"
        );
        // The local pane in front now speculates on its OWN evidence only.
        at = echo_turn(&mut p, 'x', 0, at, fast);
        assert!(p.predict_char('y', (0, 1), 80, at));
        assert!(p.overlay(at).is_empty(), "a fast local link stays dark");
        let local = p.take_link_estimate();

        // Back to the ssh session: ONE confirmed echo on the new line is enough,
        // because the LINK never stopped being slow — only the line has to re-prove
        // itself (the no-unechoed-flash guarantee is per line, and still earned here).
        p.restore_link_estimate(ssh);
        at = echo_turn(&mut p, 'a', 0, at, slow);
        assert!(p.predict_char('b', (0, 1), 80, at));
        assert_eq!(
            p.overlay(at).len(),
            1,
            "a restored slow-link estimate paints the second keystroke"
        );

        // …and the local session's own estimate comes back just as measured.
        p.restore_link_estimate(local);
        at = echo_turn(&mut p, 'x', 0, at, fast);
        assert!(p.predict_char('y', (0, 1), 80, at));
        assert!(
            p.overlay(at).is_empty(),
            "restoring a fast session's estimate must not paint either"
        );
    }

    /// REGRESSION (adversarial review): `restore_link_estimate` must RESET before it
    /// installs, and that line must be pinned by a test that fails without it.
    ///
    /// The sibling test drives the swap through `take_link_estimate` first — which
    /// already resets — so deleting `self.reset()` from `restore_link_estimate` left
    /// every test passing while the guarantee it protects was gone. This drives
    /// `restore` DIRECTLY on a pane with a displayed guess, which is what a host does
    /// when it installs a parked estimate into a predictor it did not just take from.
    /// Without the reset, pane A's ghost survives onto pane B's screen.
    #[test]
    fn restoring_an_estimate_clears_the_outgoing_panes_pixels() {
        let mut a = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(a.predict_char('x', (3, 4), 80, now));
        assert_eq!(a.overlay(now).len(), 1, "pane A has a ghost on glass");
        assert!(a.is_displaying(now));

        // Install some OTHER session's link estimate directly, with A's guess live.
        a.restore_link_estimate(LinkEstimate::default());

        assert!(
            a.overlay(now).is_empty(),
            "the outgoing pane's speculative pixels must not survive the swap"
        );
        assert!(!a.is_displaying(now));
        assert!(
            !a.confirmed_epoch,
            "nor may its confirmation license the incoming line"
        );
    }

    #[test]
    fn a_restored_guess_never_inherits_the_other_pane_s_confirmation() {
        // The swap carries the LINK, never the SCREEN. A session whose estimate says
        // "slow" still has to watch THIS line echo before anything is painted, or the
        // password prompt waiting in the tab we just switched to would show its first
        // secret keystroke on arrival.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        let ssh = p.take_link_estimate();
        p.restore_link_estimate(ssh);
        assert!(p.predict_char('s', (9, 0), 80, at));
        assert!(
            p.overlay(at).is_empty(),
            "an unechoed keystroke on the restored pane's line must not display"
        );
    }

    #[test]
    fn a_default_estimate_is_the_no_estimate_path() {
        // The contrast that makes the swap worth having, and the guard against the two
        // being confused: what the host holds for a session it has never measured is
        // `LinkEstimate::default()`, and restoring THAT must still re-earn the latch.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.adaptive_slow, "control: the latch is open");

        p.restore_link_estimate(LinkEstimate::default());
        at = echo_turn(&mut p, 'a', 0, at, slow);
        assert!(p.predict_char('b', (0, 1), 80, at));
        assert!(
            p.overlay(at).is_empty(),
            "an unmeasured session re-earns the latch from its own samples"
        );
        at += slow;
        p.reconcile(Some((0, 2)), false, at, cell(&[((0, 1), 'b')]));
        assert!(p.predict_char('c', (0, 2), 80, at));
        assert_eq!(p.overlay(at).len(), 1, "…and opens it on the second sample");
    }

    #[test]
    fn the_cold_link_probe_travels_with_the_session_estimate() {
        // The expiry backoff measures the same thing the RTT does — how long THIS link
        // makes a guess wait — so it belongs to the estimate. Left behind, a widened
        // window outlives its link and makes the next pane sit on a wrong guess for up
        // to the ceiling; re-walked from the floor, a slow link re-probes 250 → 2000 ms
        // after every tab switch before it can confirm anything at all.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), 80, now));
        let _ = p.overlay(now + Duration::from_millis(GLITCH_FLOOR_MS + 1));
        assert_eq!(
            p.expiry_backoff, 1,
            "control: the timeout widened the probe"
        );

        let est = p.take_link_estimate();
        assert_eq!(p.expiry_backoff, 0, "an unknown link probes from the floor");
        p.restore_link_estimate(est);
        assert_eq!(
            p.expiry_backoff, 1,
            "…the probing session gets its window back"
        );
    }

    #[test]
    fn session_reset_restarts_the_cold_link_probe() {
        // `reset_session` is the "nothing known about this link" path, so it must leave
        // NOTHING measured behind. A surviving backoff is a stale window: the new pane
        // would wait up to 2 s before a wrong guess self-heals on a link that never
        // timed out once.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), 80, now));
        let _ = p.overlay(now + Duration::from_millis(GLITCH_FLOOR_MS + 1));
        assert_eq!(
            p.expiry_backoff, 1,
            "control: the timeout widened the probe"
        );
        p.reset_session();
        assert_eq!(p.expiry_backoff, 0);
        assert_eq!(p.glitch_window(), Duration::from_millis(GLITCH_FLOOR_MS));
    }

    #[test]
    fn space_confirms_by_cursor_advance() {
        // A typed space echoes as a blank cell (observe → None), so it must confirm by
        // the cursor advancing past it, not wedge the queue + flush trailing type-ahead.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), 80, now);
        p.predict_char(' ', (0, 0), 80, now); // extends to (0,1)
        p.predict_char('b', (0, 0), 80, now); // extends to (0,2)
        // Echo: 'a' at (0,0); the space is blank; the cursor advanced to (0,2).
        p.reconcile(Some((0, 2)), false, now, cell(&[((0, 0), 'a')]));
        let shown = p.overlay(now);
        assert_eq!(shown.len(), 1, "'a' and the space retire; only 'b' remains");
        assert_eq!(shown[0].ch, 'b');
    }
}
