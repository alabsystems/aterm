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
//!   1. [`Predictor::predict_char`] / [`Predictor::predict_backspace`] on a keypress,
//!   2. [`Predictor::reconcile`] after child output is applied to the grid,
//!   3. [`Predictor::overlay`] when composing a frame (the glyphs to paint), and
//!   4. [`Predictor::reset`] when the coordinate space changes (resize / pane swap).
//!
//! ## Safety (why this never corrupts the screen)
//! * **Adaptive display.** Predictions are always *tracked* (to measure echo RTT),
//!   but in the default `Adaptive` mode they are only *shown* after consecutive slow
//!   confirmations establish a stable high-latency link AND at least one prediction
//!   has been confirmed this epoch. One delayed scheduler turn cannot enable pixels,
//!   and one fast confirmation closes the gate again. On a local shell the real echo
//!   is already effectively instant, so speculative pixels add visual risk without a
//!   perceptible latency win.
//! * **Alt-screen gate.** In the alternate screen (vim/less/htop) the app owns the
//!   cursor and does not line-echo; [`Predictor::reconcile`] flushes and predicting
//!   is refused.
//! * **No unechoed flash (Adaptive).** Adaptive display requires a *confirmed* echo
//!   in the CURRENT line's epoch, so a password prompt (a line that never echoes)
//!   never displays a predicted character. The epoch is reset at the SUBMIT boundary
//!   ([`Predictor::note_line_submit`], on Enter) — not merely on a physical-row change,
//!   which the cursor does NOT undergo across logical lines on a terminal scrolled to
//!   the bottom — so a prompt inheriting a prior command's confirmation on the same
//!   bottom row cannot flash the secret. `Always` is the explicit power-user opt-in and
//!   does NOT carry this guarantee: it can briefly show an unechoed glyph until the
//!   `GLITCH_MS` flush, so it is unsuitable at a password prompt.
//! * **Self-healing.** Any prediction unconfirmed for `GLITCH_MS` is flushed, and
//!   any divergence (the app drew a different glyph, or the cursor jumped) flushes
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

/// One predicted glyph at an absolute grid cell, awaiting confirmation from output.
#[derive(Clone, Copy, Debug)]
pub struct Prediction {
    /// Grid row the guess was typed on (active-grid coords).
    pub row: u16,
    /// Grid column the guess occupies.
    pub col: u16,
    /// The predicted glyph.
    pub ch: char,
    /// When predicted — drives the unconfirmed-glitch timeout (`GLITCH_MS`).
    born: Instant,
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
        }
    }
}

/// Flush a prediction that the program has not echoed within this window — the app
/// is not line-echoing as we assumed (raw-mode key, password, swallowed input).
const GLITCH_MS: u64 = 250;
/// `Adaptive` classifies a confirmed echo as slow at or above this latency.
/// Below it the real echo is effectively instant, so a rare >250 ms scheduler tail
/// can expire invisibly instead of painting and later erasing a speculative glyph.
const DISPLAY_SRTT_MS: f32 = 6.0;
/// A single delayed event-loop turn is not evidence of a slow link. Require two
/// consecutive slow confirmations before Adaptive may paint; a fast confirmation
/// closes the latch immediately.
const SLOW_CONFIRMATIONS_TO_DISPLAY: u8 = 2;
/// EWMA weight for a freshly-confirmed echo sample into the smoothed RTT.
const SRTT_ALPHA: f32 = 0.3;

/// The speculative-echo state machine for one terminal pane. See the crate docs.
#[derive(Default)]
pub struct Predictor {
    mode: PredictMode,
    /// Pending predictions in type order (oldest = `first`); each on the row it was
    /// typed. A wrap or row change flushes them (the same-row model bows out).
    preds: Vec<Prediction>,
    /// Smoothed echo round-trip estimate in ms (EWMA of confirmed predictions). The
    /// link property `Adaptive` consults. Coordinate-only [`reset`](Self::reset)
    /// preserves it; [`reset_session`](Self::reset_session) clears it so a slow
    /// remote pane can never make a newly focused local pane speculate visibly.
    srtt: Option<f32>,
    /// Consecutive raw echo samples at or above [`DISPLAY_SRTT_MS`]. This filters
    /// isolated scheduler tails that would otherwise seed the EWMA above the visual
    /// threshold after one sample.
    slow_confirmations: u8,
    /// Stable Adaptive display classification. Two consecutive slow confirmations
    /// open it; the first fast confirmation closes it, so returning from a remote
    /// foreground process to a local shell cannot inherit a long EWMA tail.
    adaptive_slow: bool,
    /// Have we confirmed ≥1 prediction on the CURRENT line (epoch)? Gates display so an
    /// unechoed context never shows a guess. Reset when typing starts on a new row.
    confirmed_epoch: bool,
    /// The row the current confirmation epoch belongs to. Typing on a NEW row (e.g. a
    /// password prompt after a command's Enter) starts a fresh epoch with
    /// `confirmed_epoch = false`, so a no-echo line never inherits a prior line's
    /// confirmation and displays an unechoed secret.
    epoch_row: Option<u16>,
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
    /// the PTY). `cursor` is the live real cursor (the anchor when nothing is pending);
    /// while typing ahead, each new glyph extends from the previous prediction. `cols`
    /// is the grid width, so we never predict past a wrap (a wrap moves rows — outside
    /// the same-row model, so we decline + flush). Returns `true` if a guess was added.
    pub fn predict_char(&mut self, ch: char, cursor: (u16, u16), cols: u16, now: Instant) -> bool {
        if self.mode == PredictMode::Off {
            return false;
        }
        // Only single-width, self-echoing printables. Control/combining/wide glyphs
        // (CJK, emoji, ESC sequences) have ambiguous echo geometry → let the real
        // output handle them, and resync from the next reconcile.
        if !(ch == ' ' || ch.is_ascii_graphic()) {
            self.flush();
            return false;
        }
        // A new line is a fresh confirmation epoch: a guess only displays (Adaptive)
        // after an echo is confirmed ON THIS LINE. So a no-echo prompt on a new row
        // (a password prompt after a command) never inherits the prior line's
        // confirmation and never shows an unechoed glyph.
        if self.preds.is_empty() && self.epoch_row != Some(cursor.0) {
            self.confirmed_epoch = false;
            self.epoch_row = Some(cursor.0);
        }
        // Anchor: extend the last pending guess, else the live cursor.
        let (row, col) = match self.preds.last() {
            Some(p) => (p.row, p.col.saturating_add(1)),
            None => cursor,
        };
        if col >= cols {
            self.flush(); // would wrap to a new row — outside the same-row model
            return false;
        }
        self.preds.push(Prediction {
            row,
            col,
            ch,
            born: now,
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
        self.preds.pop().is_some()
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
    pub fn note_line_submit(&mut self) {
        self.flush(); // drops pending guesses AND clears confirmed_epoch
        self.epoch_row = None; // the next predict_char (re)starts a fresh epoch
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
        observe: impl Fn(u16, u16) -> Option<char>,
    ) {
        // The alternate screen owns the cursor and does not line-echo: never predict.
        if alt_screen {
            self.flush();
            return;
        }

        // Classify latency once per independent output/reconcile TURN, not once per
        // character. One delayed scheduler callback can retire a whole type-ahead
        // burst; counting every retired glyph as separate slow evidence would let
        // that single tail satisfy the two-confirmation latch by itself. The fastest
        // confirmed glyph is the conservative current-link sample: if output caught
        // up to any newly typed glyph quickly, speculative pixels provide no benefit.
        let mut fastest_confirmed = None;

        // Retire confirmed leading predictions; a different glyph at the head ⇒ the
        // program diverged from our guess ⇒ flush. A still-blank head ⇒ not echoed
        // yet ⇒ stop and wait for the next burst.
        while let Some(p) = self.preds.first().copied() {
            match observe(p.row, p.col) {
                Some(c) if c == p.ch => {
                    let sample = now.saturating_duration_since(p.born);
                    self.record_rtt(sample);
                    fastest_confirmed = Some(
                        fastest_confirmed.map_or(sample, |current: Duration| current.min(sample)),
                    );
                    self.confirmed_epoch = true;
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
                        fastest_confirmed = Some(
                            fastest_confirmed
                                .map_or(sample, |current: Duration| current.min(sample)),
                        );
                        self.confirmed_epoch = true;
                        self.preds.remove(0);
                    } else {
                        break;
                    }
                }
            }
        }
        if let Some(sample) = fastest_confirmed {
            self.record_echo_regime(sample);
        }

        // Cursor-consistency: if anything is still pending, the real cursor must sit
        // exactly at the first unconfirmed prediction (the program echoed up to it).
        // Anything else (a row change, completion rewrite, reflow) ⇒ flush.
        if let Some(&p) = self.preds.first() {
            match real_cursor {
                Some(rc) if rc == (p.row, p.col) => {}
                _ => self.flush(),
            }
        }
    }

    /// The predictions to OVERLAY this frame. Expires stale unconfirmed guesses first
    /// (self-healing), then applies the display gate: `Off` ⇒ none; `Always` ⇒ all
    /// pending; `Adaptive` ⇒ all pending iff an echo is confirmed this epoch and
    /// consecutive slow samples established a stable high-latency link.
    pub fn overlay(&mut self, now: Instant) -> &[Prediction] {
        if let Some(oldest) = self.preds.first()
            && now.saturating_duration_since(oldest.born) > Duration::from_millis(GLITCH_MS)
        {
            self.flush();
        }
        if self.show_gate() { &self.preds } else { &[] }
    }

    /// The display gate shared by [`overlay`](Self::overlay) and
    /// [`is_displaying`](Self::is_displaying): `Off` ⇒ never; `Always` ⇒ always;
    /// `Adaptive` ⇒ once an echo is confirmed this epoch and consecutive slow echo
    /// samples established that prediction can help.
    fn show_gate(&self) -> bool {
        match self.mode {
            PredictMode::Off => false,
            PredictMode::Always => true,
            PredictMode::Adaptive => self.confirmed_epoch && self.adaptive_slow,
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
            Some(oldest) => {
                now.saturating_duration_since(oldest.born) > Duration::from_millis(GLITCH_MS)
                    || self.show_gate()
            }
        }
    }

    /// The instant the oldest pending guess self-expires (the glitch timeout). The host
    /// arms a `WaitUntil` on this so a stale prediction is cleaned up (and, if it was
    /// visible, repainted away) via [`overlay`](Self::overlay)'s expiry flush even when
    /// no further input or output arrives.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.preds
            .first()
            .map(|p| p.born + Duration::from_millis(GLITCH_MS))
    }

    /// Drop all in-flight predictions (coordinate space changed: resize, font zoom,
    /// pane ⇄ split). The RTT estimate is retained; the confirmation epoch is forgotten
    /// so the next line re-confirms before displaying.
    pub fn reset(&mut self) {
        self.flush();
        self.epoch_row = None;
    }

    /// Drop all predictor state when the host switches to a different terminal
    /// session. Unlike [`reset`](Self::reset), this also forgets the learned echo
    /// RTT: latency belongs to the PTY/link that produced it, not to the window
    /// containing whichever pane happens to be focused next.
    pub fn reset_session(&mut self) {
        self.reset();
        self.srtt = None;
        self.slow_confirmations = 0;
        self.adaptive_slow = false;
    }

    /// Clear pending predictions and end the confirmation epoch (display re-arms only
    /// after a fresh confirmation on the next line).
    fn flush(&mut self) {
        self.preds.clear();
        self.confirmed_epoch = false;
    }

    /// Fold a confirmed echo latency into the smoothed RTT estimate.
    fn record_rtt(&mut self, rtt: Duration) {
        let ms = rtt.as_secs_f32() * 1000.0;
        self.srtt = Some(match self.srtt {
            None => ms,
            Some(s) => s * (1.0 - SRTT_ALPHA) + ms * SRTT_ALPHA,
        });
    }

    /// Fold one independent output turn into the stable visual classification.
    fn record_echo_regime(&mut self, rtt: Duration) {
        let ms = rtt.as_secs_f32() * 1000.0;
        if ms >= DISPLAY_SRTT_MS {
            self.slow_confirmations = self
                .slow_confirmations
                .saturating_add(1)
                .min(SLOW_CONFIRMATIONS_TO_DISPLAY);
            if self.slow_confirmations >= SLOW_CONFIRMATIONS_TO_DISPLAY {
                self.adaptive_slow = true;
            }
        } else {
            // Fast evidence is decisive: speculative pixels provide no benefit on
            // the live path even if an old remote EWMA takes many samples to decay.
            self.slow_confirmations = 0;
            self.adaptive_slow = false;
        }
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
    fn adaptive_slow_to_fast_closes_on_first_local_confirmation() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();

        assert!(p.predict_char('a', (0, 0), 80, now));
        let slow1 = now + Duration::from_millis(60);
        p.reconcile(Some((0, 1)), false, slow1, cell(&[((0, 0), 'a')]));
        assert!(p.predict_char('b', (0, 1), 80, slow1));
        let slow2 = slow1 + Duration::from_millis(60);
        p.reconcile(Some((0, 2)), false, slow2, cell(&[((0, 1), 'b')]));
        assert!(p.predict_char('c', (0, 2), 80, slow2));
        assert_eq!(p.overlay(slow2).len(), 1, "sustained remote latency opens");

        // The EWMA is still far above 6 ms here, but current fast evidence wins:
        // returning from ssh to a local foreground must close after one echo.
        let local = slow2 + Duration::from_millis(1);
        p.reconcile(Some((0, 3)), false, local, cell(&[((0, 2), 'c')]));
        assert!(p.srtt.is_some_and(|sample| sample >= DISPLAY_SRTT_MS));
        assert!(p.predict_char('d', (0, 3), 80, local));
        assert!(
            p.overlay(local).is_empty(),
            "one fast confirmation closes despite the stale remote EWMA"
        );
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

        let after_deadline = fast + Duration::from_millis(GLITCH_MS + 1);
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
        let late = now + Duration::from_millis(GLITCH_MS + 50);
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
        let late = now + Duration::from_millis(GLITCH_MS + 50);
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
    fn does_not_predict_past_the_right_margin() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('a', (0, 78), 80, now)); // col 78 ok
        assert!(p.predict_char('b', (0, 78), 80, now)); // extends to col 79 (last column)
        assert!(
            !p.predict_char('c', (0, 78), 80, now),
            "col 80 would wrap → declined"
        );
        assert!(!p.is_active(), "the wrap attempt flushed pending guesses");
    }

    #[test]
    fn non_ascii_is_not_predicted() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(
            !p.predict_char('日', (0, 0), 80, now),
            "wide/CJK glyphs are left to real echo"
        );
        assert!(!p.is_active());
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
        // Submit (Enter): ends the epoch and flushes the pending guess.
        p.note_line_submit();
        assert!(
            p.overlay(confirmed).is_empty(),
            "submit flushes pending guesses"
        );
        // Line 2 (password prompt) on the SAME physical row r: never echoes.
        let t1 = confirmed + Duration::from_millis(50);
        assert!(p.predict_char('s', (r, 9), 80, t1)); // predicted (tracked)…
        assert!(
            p.overlay(t1).is_empty(),
            "after submit, an unechoed char on the SAME physical row must NOT display (no leak)"
        );
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
