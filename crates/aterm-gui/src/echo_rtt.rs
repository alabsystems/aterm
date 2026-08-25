// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ECHO ROUND-TRIP — "how long does the PROGRAM take to answer my keystroke?"
//! (responsiveness audit item 5, tier i).
//!
//! WHY THIS EXISTS. Every latency instrument aterm shipped measures aterm:
//! `key_write` is key → bytes on the PTY (our input path), `input_present` is
//! key → present returned (our whole pipeline). None of them can tell the owner
//! whether a session that *feels* laggy is aterm being slow or the TUI on the
//! far side of the PTY being slow — which is the actual question on a loaded
//! machine, where a busy Claude/vim/tmux can take tens of milliseconds to echo
//! a character that aterm then paints in one frame. This module measures the
//! slice in between, and it belongs to the CHILD, not to us:
//!
//! ```text
//!   key_write     (metrics)   key → bytes handed to the PTY      aterm input path
//!   echo_rtt      (HERE)      bytes out → first bytes back       the TUI process
//!   input_present (metrics)   key → present returned             aterm, end to end
//! ```
//!
//! Read together, a sick number names its own culprit: `echo_rtt` high with
//! `input_present` low means the program is behind; the reverse means we are.
//!
//! HONESTY BOUNDS — stated up front because a latency instrument that hides its
//! own contamination is worse than no instrument at all (the observer rule in
//! `docs/RELEASE-PROOF-DISCIPLINE.md`):
//!
//!   * LOWER BOUND ON THE FELT ECHO. The clock stops when the PTY reader has
//!     the bytes, not when the glyph is on glass. The remaining parse → compose
//!     → present tail is `present_latency`'s slice, deliberately not folded in
//!     here: mixing them back together is exactly what made "is it us or them?"
//!     unanswerable. Tier (ii) of the audit item closes the last gap — see the
//!     TIER (ii) note below.
//!   * IT ARMS ONLY ON A REAL WRITE. The probe brackets input dispatch and arms
//!     only when the target session's sink INPUT EPOCH advanced, i.e. bytes
//!     actually entered that PTY. A UI shortcut, a bare modifier, a key an
//!     overlay swallowed — none of them arm. Without that gate a `cat` flood
//!     would satisfy every keypress within microseconds and the instrument
//!     would publish its BEST numbers in precisely the window where the
//!     terminal is at its WORST, which is the failure mode this audit exists to
//!     stop repeating.
//!   * KEEP-OLDEST WITHIN A BURST, like `INPUT_STAMP_NS`. While one write is
//!     outstanding a second keystroke does not re-arm (it increments
//!     `echo_coalesced` instead), so a fast typist's burst is measured from the
//!     key that is actually still waiting rather than from the newest one. That
//!     is conservative-HIGH per burst, and the count of suppressed arms is
//!     published so the bias is visible rather than hidden.
//!   * NO CAUSAL PROOF, BY CONSTRUCTION. "The first output after our bytes" is
//!     not provably the ANSWER to our bytes: a program printing on its own
//!     (a spinner, a build log) can close the clock early. This is a round-trip
//!     LOWER bound, not a causal one; the sample count, the coalesced count and
//!     the expired count are all published so a reader can see the shape of the
//!     evidence. A causally-proven variant needs tier (ii)'s content proof.
//!   * WHAT IT DOES NOT SEE, SAID PLAINLY. Two write paths do not arm it, by
//!     construction rather than by oversight: a PASTE deferred to the per-session
//!     ordered-egress writer bumps the sink epoch on THAT thread, after the
//!     bracket has already closed; and a CROSS-session (`@other`) control verb
//!     writes straight from the control thread with no App seam to bracket. Both
//!     are absences (no sample), never wrong samples — the instrument under-counts
//!     rather than lying. A flagless `key`/`send` at the front tab DOES arm: it
//!     posts `Wake::Input` through the App seam, which is what makes this metric
//!     drivable by a proof harness instead of readable only by a human at the
//!     glass.
//!   * WINDOWED, AND SAID SO. The percentiles are exact order statistics over
//!     the last [`WINDOW`] samples (published as `n_echo`), not the all-time
//!     distribution: what the owner asks is "how does it feel NOW", and an
//!     all-time histogram lets an hour of healthy typing bury the ten minutes
//!     that hurt. `echo_total` carries the all-time sample count.
//!
//! COST. One relaxed load per output burst when nothing is armed (the flood
//! case), one CAS per armed keystroke, and a `try_lock`ed 4 KiB ring push per
//! recorded sample — at most one per keystroke. The PTY reader never blocks:
//! a contended window is dropped and counted (`echo_dropped_locked`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::WindowId;

/// Samples the published percentiles are computed over — the trailing window.
/// 1024 keystrokes is minutes of real typing, small enough (4 KiB) to sort on
/// demand and short enough that a bad stretch cannot be diluted by an old good
/// one.
pub const WINDOW: usize = 1024;

/// How long an unanswered write keeps the single arm slot. Past this the write
/// is retired as EXPIRED (never as a sample) so the NEXT keystroke can be
/// measured — an unanswered write must not blind the instrument forever.
/// Deliberately long: a slow answer is the very thing we are hunting, so the
/// slot is only released once "the program is not answering at all" is the
/// better explanation.
const ARM_TTL_US: u64 = 2_000_000;

// TIER (ii), DESIGNED, NOT WIRED — key-arrival → typed-confirm-present.
//
// The exact echo (this keystroke's GLYPH is proven on the cursor row) is
// already computed by the cursor-glow content proof:
// `aterm_effects::cursor_glow::ContentCandidateDecision::Confirmed { at, .. }`,
// projected onto the trail in `app_render::confirm_cursor_move_candidate`.
// Wiring is two lines — arm at the same place tier (i) arms, and in the
// `Confirmed` branch record `at - arm`. It is NOT wired here because
// `app_render.rs`'s cursor-fx seam is under an explicit change fence for this
// release (it carries the landed trail-blackout fix) and is owned by another
// work package this cycle. See the package report for the call site.

// ---------------------------------------------------------------------------
// The single arm slot.
//
// ONE `AtomicU64` holds both halves — `(session_tag << 48) | micros` — so the
// pair can never be observed torn. Two atomics would let the PTY reader read a
// session id from one keystroke and a stamp from another and publish the
// difference as a latency, which is the class of quiet lie this whole audit is
// about. 48 bits of microseconds is 8.9 years of uptime on the shared
// `metrics::now_us` clock; the low 16 bits of the session id are enough to tell
// live sessions apart (a collision needs 65 536 spawns and costs one
// mislabelled sample, never a torn one).
const US_MASK: u64 = (1 << 48) - 1;

static ARMED: AtomicU64 = AtomicU64::new(0);
/// Writes that took the slot (the denominator: `arms == samples + expired + at
/// most one outstanding`).
static ARMS: AtomicU64 = AtomicU64::new(0);
/// Writes that found the slot busy — the size of the keep-oldest bias.
static COALESCED: AtomicU64 = AtomicU64::new(0);
/// Arms retired without an answer. Climbing = the program is not answering.
static EXPIRED: AtomicU64 = AtomicU64::new(0);
/// All-time samples (the window holds only the last [`WINDOW`]).
static TOTAL: AtomicU64 = AtomicU64::new(0);
static LAST_US: AtomicU64 = AtomicU64::new(0);
static MAX_US: AtomicU64 = AtomicU64::new(0);
/// Samples whose ring push lost `try_lock` — the reader thread declines to
/// block on the metrics reader, and says so rather than pretending.
static DROPPED_LOCKED: AtomicU64 = AtomicU64::new(0);

/// The trailing sample window, in microseconds. Written at most once per
/// keystroke, read by the `metrics percentiles` verb.
static RING: Mutex<Ring> = Mutex::new(Ring::new());

struct Ring {
    buf: [u32; WINDOW],
    len: usize,
    next: usize,
}

impl Ring {
    const fn new() -> Self {
        Self {
            buf: [0; WINDOW],
            len: 0,
            next: 0,
        }
    }

    fn push(&mut self, us: u64) {
        self.buf[self.next] = u32::try_from(us).unwrap_or(u32::MAX);
        self.next = (self.next + 1) % WINDOW;
        self.len = (self.len + 1).min(WINDOW);
    }

    fn clear(&mut self) {
        self.len = 0;
        self.next = 0;
    }

    /// Exact order statistics (nearest-rank) over the live window — a real
    /// observed sample, not a bucket edge, so the number published is one that
    /// actually happened.
    fn percentiles(&self) -> (u64, u64, u64) {
        if self.len == 0 {
            return (0, 0, 0);
        }
        let mut v: Vec<u32> = self.buf[..self.len].to_vec();
        v.sort_unstable();
        let at = |q: f64| -> u64 {
            let n = v.len();
            // ceil(q * n) - 1, clamped: p50 of one sample is that sample.
            let rank = ((q * n as f64).ceil() as usize).clamp(1, n);
            u64::from(v[rank - 1])
        };
        (at(0.50), at(0.95), at(0.99))
    }
}

const fn pack(session: u64, now_us: u64) -> u64 {
    ((session & 0xFFFF) << 48) | (now_us & US_MASK)
}

// ---------------------------------------------------------------------------
// The state machine. The `_at` forms take the clock so the tests can drive it.

/// A keystroke's bytes just entered `session`'s PTY: start the clock, unless an
/// older unanswered write still holds the slot (keep-oldest).
fn arm_at(session: u64, now_us: u64) {
    let cur = ARMED.load(Ordering::Acquire);
    if cur != 0 {
        if now_us.saturating_sub(cur & US_MASK) < ARM_TTL_US {
            COALESCED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // Stale: that write was never answered. Retire it as EXPIRED — never as
        // a sample — and take the slot. Losing this CAS means somebody else
        // already moved the slot on; treat it as an ordinary busy slot.
        if ARMED
            .compare_exchange(cur, 0, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            EXPIRED.fetch_add(1, Ordering::Relaxed);
        } else {
            COALESCED.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    if ARMED
        .compare_exchange(0, pack(session, now_us.max(1)), Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
    {
        ARMS.fetch_add(1, Ordering::Relaxed);
    } else {
        COALESCED.fetch_add(1, Ordering::Relaxed);
    }
}

/// `session` produced output: if it owes us an echo, close the clock. Returns
/// the recorded microseconds, for the tests.
fn close_at(session: u64, now_us: u64) -> Option<u64> {
    let cur = ARMED.load(Ordering::Acquire);
    if cur == 0 || cur >> 48 != (session & 0xFFFF) {
        return None;
    }
    // Win the slot before reading the stamp out of it: the stamp lives in the
    // same word, so a successful CAS is proof this thread owns that sample.
    if ARMED
        .compare_exchange(cur, 0, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return None;
    }
    // Recorded WHATEVER the duration is, including one past `ARM_TTL_US` that
    // no later keystroke happened to displace. Truncating the tail is the exact
    // dishonesty (contaminated distributions, healthy medians over sick tails)
    // this audit round is repairing.
    let d = now_us.saturating_sub(cur & US_MASK);
    TOTAL.fetch_add(1, Ordering::Relaxed);
    LAST_US.store(d, Ordering::Relaxed);
    MAX_US.fetch_max(d, Ordering::Relaxed);
    match RING.try_lock() {
        Ok(mut ring) => ring.push(d),
        Err(_) => {
            DROPPED_LOCKED.fetch_add(1, Ordering::Relaxed);
        }
    }
    Some(d)
}

/// One output burst arrived from `session` — call this on the LEADING edge of
/// the burst, before any parse work, so the number stays the child's round trip
/// and never absorbs aterm's own ingest backlog.
///
/// Hot path: a single relaxed load when no keystroke is outstanding, which is
/// every burst of a flood.
pub fn note_output_burst(session: u64) {
    if ARMED.load(Ordering::Relaxed) == 0 {
        return;
    }
    let _ = close_at(session, crate::metrics::now_us());
}

/// Zero every counter and the window. Wired to `metrics reset` so the echo
/// facts obey the same window semantics as the rest of the verb.
pub fn reset() {
    ARMED.store(0, Ordering::Relaxed);
    ARMS.store(0, Ordering::Relaxed);
    COALESCED.store(0, Ordering::Relaxed);
    EXPIRED.store(0, Ordering::Relaxed);
    TOTAL.store(0, Ordering::Relaxed);
    LAST_US.store(0, Ordering::Relaxed);
    MAX_US.store(0, Ordering::Relaxed);
    DROPPED_LOCKED.store(0, Ordering::Relaxed);
    if let Ok(mut ring) = RING.lock() {
        ring.clear();
    }
}

/// Everything the `metrics percentiles` verb publishes. Microseconds
/// throughout; the verb converts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EchoSnapshot {
    /// Samples in the trailing window the percentiles were computed over.
    pub n: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    /// All-time samples since the last [`reset`].
    pub total: u64,
    /// Writes that took the arm slot.
    pub arms: u64,
    /// Writes suppressed by an older outstanding one (the keep-oldest bias).
    pub coalesced: u64,
    /// Arms retired unanswered.
    pub expired: u64,
    pub last_us: u64,
    pub max_us: u64,
    /// Samples dropped rather than block the PTY reader on the window lock.
    pub dropped_locked: u64,
}

/// Read the live echo facts. Takes the window lock (never held across anything
/// else); a poisoned lock reports an empty window rather than panicking a
/// control connection.
#[must_use]
pub fn snapshot() -> EchoSnapshot {
    let (n, p50_us, p95_us, p99_us) = RING.lock().map_or((0, 0, 0, 0), |ring| {
        let (a, b, c) = ring.percentiles();
        (ring.len as u64, a, b, c)
    });
    EchoSnapshot {
        n,
        p50_us,
        p95_us,
        p99_us,
        total: TOTAL.load(Ordering::Relaxed),
        arms: ARMS.load(Ordering::Relaxed),
        coalesced: COALESCED.load(Ordering::Relaxed),
        expired: EXPIRED.load(Ordering::Relaxed),
        last_us: LAST_US.load(Ordering::Relaxed),
        max_us: MAX_US.load(Ordering::Relaxed),
        dropped_locked: DROPPED_LOCKED.load(Ordering::Relaxed),
    }
}

/// The `metrics percentiles` TEXT fragment (leading space, no trailing
/// newline). Built HERE, not in the verb, so the whole echo surface — fields,
/// units, and the honesty counters that must never be separated from the
/// percentiles they qualify — lives in one file with the state machine.
///
/// `n_echo` is the trailing WINDOW, not the all-time count (`echo_total`): see
/// the module note on why the felt question is answered by recent samples.
/// `echo_arms`/`echo_coalesced`/`echo_expired` are published beside them so a
/// reader can always tell a quiet instrument (`arms=0`, nothing typed) from a
/// blind one (`expired` climbing, the program is not answering) from a biased
/// one (`coalesced` high, keys outrunning echoes).
#[must_use]
pub fn percentile_fields_text() -> String {
    let s = snapshot();
    let ms = |us: u64| us as f64 / 1e3;
    format!(
        " n_echo={} echo_p50_ms={:.2} echo_p95_ms={:.2} echo_p99_ms={:.2} \
         echo_last_ms={:.2} echo_max_ms={:.2} echo_total={} echo_arms={} \
         echo_coalesced={} echo_expired={} echo_dropped_locked={}",
        s.n,
        ms(s.p50_us),
        ms(s.p95_us),
        ms(s.p99_us),
        ms(s.last_us),
        ms(s.max_us),
        s.total,
        s.arms,
        s.coalesced,
        s.expired,
        s.dropped_locked,
    )
}

/// The JSON twin of [`percentile_fields_text`] — a leading comma, so it splices
/// straight in before the closing brace. Field-for-field identical to the text
/// form; automation must never have to scrape the line to read a fact the text
/// verb has.
#[must_use]
pub fn percentile_fields_json() -> String {
    let s = snapshot();
    let ms = |us: u64| us as f64 / 1e3;
    format!(
        ",\"n_echo\":{},\"echo_p50_ms\":{:.2},\"echo_p95_ms\":{:.2},\
         \"echo_p99_ms\":{:.2},\"echo_last_ms\":{:.2},\"echo_max_ms\":{:.2},\
         \"echo_total\":{},\"echo_arms\":{},\"echo_coalesced\":{},\
         \"echo_expired\":{},\"echo_dropped_locked\":{}",
        s.n,
        ms(s.p50_us),
        ms(s.p95_us),
        ms(s.p99_us),
        ms(s.last_us),
        ms(s.max_us),
        s.total,
        s.arms,
        s.coalesced,
        s.expired,
        s.dropped_locked,
    )
}

// ---------------------------------------------------------------------------
// The write probe: the only thing that arms the clock.

/// An open bracket around one input dispatch: the session the input is routed
/// to and its sink's input epoch BEFORE dispatch. Closing the bracket arms the
/// echo clock iff that epoch moved, i.e. iff bytes really entered the PTY.
pub(crate) struct EchoWriteProbe {
    session: u64,
    epoch: aterm_session::sink::InputEpoch,
}

impl crate::App {
    /// Open the echo bracket for input about to be routed to `session` (or, for
    /// `None`, to `wid`'s focused pane — the hardware-keyboard case). `None` if
    /// there is no terminal session to write to (native focus, dead pane), in
    /// which case closing is a no-op.
    pub(crate) fn echo_probe_open(
        &self,
        wid: WindowId,
        session: Option<u64>,
    ) -> Option<EchoWriteProbe> {
        let session = session.or_else(|| self.focused_session_id(wid))?;
        let epoch = self.session_by_id(session)?.ctx.sink.input_epoch();
        Some(EchoWriteProbe { session, epoch })
    }

    /// Close the bracket. Arms the echo clock only if the sink's input epoch
    /// advanced — see the module note on why an ungated arm would make this
    /// instrument flatter itself exactly when the terminal is worst.
    pub(crate) fn echo_probe_close(&self, probe: Option<EchoWriteProbe>) {
        let Some(probe) = probe else { return };
        let Some(session) = self.session_by_id(probe.session) else {
            return;
        };
        if session.ctx.sink.input_epoch() != probe.epoch {
            arm_at(probe.session, crate::metrics::now_us());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The statics are process-global, so these tests take turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn fresh() -> std::sync::MutexGuard<'static, ()> {
        let g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset();
        g
    }

    /// The basic contract: a write arms, that session's next burst closes, and
    /// the recorded number is the interval between them.
    #[test]
    fn a_write_then_its_sessions_burst_records_the_interval() {
        let _g = fresh();
        arm_at(7, 1_000);
        assert_eq!(close_at(7, 6_000), Some(5_000));
        let s = snapshot();
        assert_eq!((s.n, s.total, s.arms), (1, 1, 1));
        assert_eq!((s.p50_us, s.p95_us, s.p99_us), (5_000, 5_000, 5_000));
        assert_eq!(s.expired, 0, "an answered write is not an expiry");
    }

    /// A DIFFERENT session's output must not close our clock — that mislabel is
    /// how a background flood would "answer" a keystroke typed into a quiet
    /// pane and publish a microsecond echo.
    #[test]
    fn another_sessions_output_cannot_close_the_clock() {
        let _g = fresh();
        arm_at(7, 1_000);
        assert_eq!(close_at(8, 2_000), None);
        assert_eq!(snapshot().total, 0);
        assert_eq!(close_at(7, 3_000), Some(2_000), "the real session still can");
    }

    /// Nothing armed ⇒ output records nothing at all. This is the property that
    /// keeps a pure `cat`/`yes` flood out of the distribution.
    #[test]
    fn output_with_no_outstanding_write_records_nothing() {
        let _g = fresh();
        for t in 0..1_000 {
            note_output_burst(7);
            assert_eq!(close_at(7, t), None);
        }
        assert_eq!(snapshot().total, 0);
    }

    /// Keep-oldest: while a write is outstanding a second keystroke does not
    /// re-arm, so the burst is measured from the key still waiting — and the
    /// suppressed arm is COUNTED, not hidden.
    #[test]
    fn a_second_key_coalesces_and_the_bias_is_published() {
        let _g = fresh();
        arm_at(7, 1_000);
        arm_at(7, 2_000);
        arm_at(7, 3_000);
        assert_eq!(
            close_at(7, 11_000),
            Some(10_000),
            "measured from the OLDEST unanswered write"
        );
        let s = snapshot();
        assert_eq!((s.arms, s.coalesced), (1, 2));
    }

    /// An unanswered write must not blind the instrument forever: past the TTL
    /// the next keystroke takes the slot and the dead one is booked as EXPIRED,
    /// never as a sample.
    #[test]
    fn an_unanswered_write_expires_and_frees_the_slot() {
        let _g = fresh();
        arm_at(7, 1_000);
        arm_at(7, 1_000 + ARM_TTL_US);
        let s = snapshot();
        assert_eq!((s.expired, s.arms, s.total), (1, 2, 0));
        assert_eq!(
            close_at(7, 1_000 + ARM_TTL_US + 42),
            Some(42),
            "the NEW write owns the slot"
        );
    }

    /// A slow echo is recorded at its true length. Truncating the tail would
    /// reproduce the very defect (healthy median over a sick tail) this audit
    /// round exists to repair.
    #[test]
    fn a_very_slow_echo_is_recorded_not_truncated() {
        let _g = fresh();
        arm_at(7, 1_000);
        let slow = ARM_TTL_US * 3;
        assert_eq!(close_at(7, 1_000 + slow), Some(slow));
        assert_eq!(snapshot().max_us, slow);
    }

    /// Percentiles are exact order statistics over the trailing window, and the
    /// window rolls: 1..=WINDOW+500 leaves the oldest 500 out of the p50.
    #[test]
    fn percentiles_are_exact_over_the_trailing_window() {
        let _g = fresh();
        // Arm at 1, close at 1+i, so the recorded sample is exactly `i` µs.
        for i in 1..=100u64 {
            arm_at(7, 1);
            assert_eq!(close_at(7, 1 + i), Some(i));
        }
        let s = snapshot();
        assert_eq!((s.n, s.total), (100, 100));
        assert_eq!((s.p50_us, s.p95_us, s.p99_us), (50, 95, 99));

        for i in 101..=(WINDOW as u64 + 500) {
            arm_at(7, 1);
            assert_eq!(close_at(7, 1 + i), Some(i));
        }
        let s = snapshot();
        assert_eq!(s.n, WINDOW as u64, "the window is bounded");
        assert_eq!(s.total, WINDOW as u64 + 500, "the all-time count is not");
        // The window now holds 501..=1524, so p50 is its 512th value.
        assert_eq!(s.p50_us, 501 + 511);
    }

    /// NEGATIVE CONTROL — this gate must be able to FAIL for the reason it
    /// exists. Model the design WITHOUT the input-epoch gate (arm on every
    /// keypress, written or not) and show it diverges: under a flood every
    /// keypress is closed by the next unrelated burst, so the instrument
    /// publishes a flat, flattering number precisely when the terminal is at
    /// its worst. The shipped path arms nothing for a key that wrote nothing,
    /// so the identical trace records no samples at all.
    #[test]
    fn the_ungated_design_would_harvest_a_flood_as_its_own_echo() {
        let _g = fresh();
        let mut t = 1u64;
        for _ in 0..50 {
            // What an ungated probe would do on a UI shortcut / swallowed chord.
            arm_at(7, t);
            assert_eq!(close_at(7, t + 50), Some(50));
            t += 1_000;
        }
        let ungated = snapshot();
        assert_eq!(ungated.total, 50);
        assert_eq!(
            (ungated.p50_us, ungated.p99_us),
            (50, 50),
            "the ungated design reports a flat 50µs echo for keys that never wrote"
        );

        reset();
        // The shipped path: the same 50 non-writing keys arm nothing, so the
        // same 50 flood bursts are recorded as nothing.
        for _ in 0..50 {
            note_output_burst(7);
        }
        let shipped = snapshot();
        assert_eq!(
            (shipped.total, shipped.arms),
            (0, 0),
            "a flood with no write behind it must contribute no samples"
        );
    }

    /// The packed slot round-trips both halves, which is the whole reason it is
    /// one word: a torn read would publish one keystroke's session against
    /// another's stamp.
    #[test]
    fn the_arm_slot_packs_session_and_stamp_without_tearing() {
        let packed = pack(0xABCD_1234_5678_9007, 123_456_789);
        assert_eq!(packed >> 48, 0x9007);
        assert_eq!(packed & US_MASK, 123_456_789);
    }
}
