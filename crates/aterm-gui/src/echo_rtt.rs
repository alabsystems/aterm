// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Echo round trip: how long the program on the far side of the PTY takes to answer a keystroke.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::WindowId;

/// Samples the published percentiles are computed over.
pub const WINDOW: usize = 1024;

/// How long an unanswered write holds the single arm slot; past it the write is retired as
/// expired (never a sample) so the next keystroke can be measured. Long on purpose: a slow
/// answer is what this measures.
const ARM_TTL_US: u64 = 2_000_000;

const US_MASK: u64 = (1 << 48) - 1;

static ARMED: AtomicU64 = AtomicU64::new(0);
/// Writes that took the slot (the denominator: `arms == samples + expired`, plus at most one
/// outstanding).
static ARMS: AtomicU64 = AtomicU64::new(0);
/// Writes that found the slot busy — the size of the keep-oldest bias.
static COALESCED: AtomicU64 = AtomicU64::new(0);
/// Arms retired without an answer. Climbing = the program is not answering.
static EXPIRED: AtomicU64 = AtomicU64::new(0);
/// All-time samples (the window holds only the last [`WINDOW`]).
static TOTAL: AtomicU64 = AtomicU64::new(0);
static LAST_US: AtomicU64 = AtomicU64::new(0);
static MAX_US: AtomicU64 = AtomicU64::new(0);
/// Samples whose ring push lost `try_lock`: the PTY reader never blocks on the metrics reader.
static DROPPED_LOCKED: AtomicU64 = AtomicU64::new(0);

const BURST_SLOTS: usize = 64;

/// Each session's latest output burst, packed like [`ARMED`], in the slot its id picks; a
/// session that shares a slot reads another's burst as none of its own. An arm reads it back
/// to catch an answer that arrived before the arm did ([`arm_catching`]).
static BURSTS: [AtomicU64; BURST_SLOTS] = [const { AtomicU64::new(0) }; BURST_SLOTS];

fn burst_slot(session: u64) -> &'static AtomicU64 {
    &BURSTS[usize::try_from(session % BURST_SLOTS as u64).unwrap_or(0)]
}

thread_local! {
    /// When this thread's first PTY write since [`forget_write_start`] began, in
    /// [`crate::metrics::now_us`] microseconds (0: none).
    static WRITE_START_US: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// This thread is about to hand input bytes to a PTY: the first such write since
/// [`forget_write_start`] is where an echo's clock starts.
pub(crate) fn note_write_start() {
    WRITE_START_US.with(|start| {
        if start.get() == 0 {
            start.set(crate::metrics::now_us().max(1));
        }
    });
}

/// Drop this thread's write start, before the work whose first write is to be timed.
pub(crate) fn forget_write_start() {
    WRITE_START_US.with(|start| start.set(0));
}

fn take_write_start() -> Option<u64> {
    WRITE_START_US.with(|start| match start.replace(0) {
        0 => None,
        us => Some(us),
    })
}

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

    const fn clear(&mut self) {
        self.len = 0;
        self.next = 0;
    }

    /// Copy the live window out. This fixed 4 KiB copy is all a reader does
    /// under the lock: no allocation and no sort, so the PTY reader's
    /// `try_lock` in [`close_at`] has almost nothing to lose to.
    const fn copy_out(&self) -> RingCopy {
        RingCopy {
            buf: self.buf,
            len: self.len,
        }
    }
}

/// A reader's private copy of the window, taken by [`Ring::copy_out`] and
/// ordered after the lock is released.
struct RingCopy {
    buf: [u32; WINDOW],
    len: usize,
}

impl RingCopy {
    const EMPTY: Self = Ring::new().copy_out();

    /// Exact order statistics (nearest-rank) over the copied window — a real
    /// observed sample, not a bucket edge, so the number published is one that
    /// actually happened. Sorts the copy in place.
    fn percentiles(&mut self) -> (u64, u64, u64) {
        let v = &mut self.buf[..self.len];
        if v.is_empty() {
            return (0, 0, 0);
        }
        v.sort_unstable();
        let n = v.len();
        let at = |q: f64| -> u64 {
            let rank = ((q * n as f64).ceil() as usize).clamp(1, n);
            u64::from(v[rank - 1])
        };
        (at(0.50), at(0.95), at(0.99))
    }
}

const fn pack(session: u64, now_us: u64) -> u64 {
    ((session & 0xFFFF) << 48) | (now_us & US_MASK)
}

/// A keystroke's bytes entered `session`'s PTY at `now_us`: start the clock, unless an
/// older unanswered write still holds the slot (keep-oldest). Returns the slot's new value
/// when this write took it.
fn arm_at(session: u64, now_us: u64) -> Option<u64> {
    let cur = ARMED.load(Ordering::Acquire);
    if cur != 0 {
        if now_us.saturating_sub(cur & US_MASK) < ARM_TTL_US {
            COALESCED.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        if ARMED
            .compare_exchange(cur, 0, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            EXPIRED.fetch_add(1, Ordering::Relaxed);
        } else {
            COALESCED.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    }
    let armed = pack(session, now_us.max(1));
    if ARMED
        .compare_exchange(0, armed, Ordering::SeqCst, Ordering::Relaxed)
        .is_ok()
    {
        ARMS.fetch_add(1, Ordering::Relaxed);
        Some(armed)
    } else {
        COALESCED.fetch_add(1, Ordering::Relaxed);
        None
    }
}

/// Arm for a write to `session` that began at `start_us`, and close at once on an answer that
/// arrived before the arm. The PTY reader stores its burst and then looks for an arm; this arms
/// and then looks for a burst. All four steps are `SeqCst`, so at least one of the two sees the
/// other, and the slot's compare-exchange lets only one of them record the sample.
fn arm_catching(session: u64, start_us: u64) {
    let Some(armed) = arm_at(session, start_us) else {
        return;
    };
    let burst = burst_slot(session).load(Ordering::SeqCst);
    if burst >> 48 == session & 0xFFFF && burst & US_MASK >= start_us {
        let _ = close_armed(armed, burst & US_MASK);
    }
}

/// `session` produced output: if it owes us an echo, close the clock. Returns
/// the recorded microseconds, for the tests.
fn close_at(session: u64, now_us: u64) -> Option<u64> {
    let cur = ARMED.load(Ordering::Acquire);
    if cur == 0 || cur >> 48 != (session & 0xFFFF) {
        return None;
    }
    close_armed(cur, now_us)
}

/// Close the clock if the slot still holds `cur`, recording the interval from its write to
/// `now_us`.
fn close_armed(cur: u64, now_us: u64) -> Option<u64> {
    if ARMED
        .compare_exchange(cur, 0, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return None;
    }
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

/// One output burst arrived from `session`.
pub(crate) fn note_output_burst(session: u64) {
    note_output_burst_at(session, crate::metrics::now_us());
}

fn note_output_burst_at(session: u64, now_us: u64) {
    burst_slot(session).store(pack(session, now_us), Ordering::SeqCst);
    if ARMED.load(Ordering::SeqCst) == 0 {
        return;
    }
    let _ = close_at(session, now_us);
}

/// A key queued behind a paste was just accepted by `session`'s `sink` on the ordered-egress
/// writer thread: start the clock where that write began (keep-oldest, like every arm).
pub(crate) fn arm_deferred(session: u64, sink: &aterm_session::sink::SinkWriter) {
    let start_us = take_write_start().unwrap_or_else(crate::metrics::now_us);
    if !arms_reach_the_statics(sink) {
        return;
    }
    arm_catching(session, start_us);
}

/// Whether an arm for `sink` may reach the process-global statics: always, outside tests.
#[cfg(not(test))]
const fn arms_reach_the_statics(_sink: &aterm_session::sink::SinkWriter) -> bool {
    true
}

/// Whether an arm for `sink` may reach the process-global statics: in tests, only the sink
/// [`arms_for_test`] lets through.
#[cfg(test)]
fn arms_reach_the_statics(sink: &aterm_session::sink::SinkWriter) -> bool {
    ARM_SINK.load(Ordering::Acquire) == std::ptr::from_ref(sink).addr()
}

/// Tests: the one sink whose arms may reach the process-global statics. Every other test's
/// keys arm too, on the UI thread and on ordered writer threads, and must not perturb the
/// tests that read these counters exactly (this module's own).
#[cfg(test)]
static ARM_SINK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Let `sink`'s arms through for as long as the guard lives.
#[cfg(all(test, unix))]
pub(crate) fn arms_for_test(sink: &aterm_session::sink::SinkWriter) -> impl Drop + use<> {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            ARM_SINK.store(0, Ordering::Release);
        }
    }
    ARM_SINK.store(std::ptr::from_ref(sink).addr(), Ordering::Release);
    Guard
}

thread_local! {
    /// This thread's current input dispatch handed a write to the ordered egress writer. Set
    /// by [`note_deferred_write`], cleared by [`crate::App::echo_probe_open`], read by the
    /// close, all on the UI thread.
    static DEFERRED_IN_BRACKET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The dispatch in progress on this thread queued a write for the ordered egress writer.
pub(crate) fn note_deferred_write() {
    DEFERRED_IN_BRACKET.with(|deferred| deferred.set(true));
}

/// Zero every counter and the window.
pub(crate) fn reset() {
    ARMED.store(0, Ordering::Relaxed);
    ARMS.store(0, Ordering::Relaxed);
    COALESCED.store(0, Ordering::Relaxed);
    EXPIRED.store(0, Ordering::Relaxed);
    TOTAL.store(0, Ordering::Relaxed);
    LAST_US.store(0, Ordering::Relaxed);
    MAX_US.store(0, Ordering::Relaxed);
    DROPPED_LOCKED.store(0, Ordering::Relaxed);
    for burst in &BURSTS {
        burst.store(0, Ordering::Relaxed);
    }
    if let Ok(mut ring) = RING.lock() {
        ring.clear();
    }
}

/// Everything the `metrics percentiles` verb publishes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EchoSnapshot {
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

/// Read the live echo facts.
#[must_use]
pub(crate) fn snapshot() -> EchoSnapshot {
    let mut window = RING
        .lock()
        .map_or_else(|_| RingCopy::EMPTY, |ring| ring.copy_out());
    let (p50_us, p95_us, p99_us) = window.percentiles();
    EchoSnapshot {
        n: window.len as u64,
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

/// The echo fields of the `metrics` summary and of `metrics percentiles`.
pub(crate) fn emit_metrics(s: &mut dyn crate::metrics::wire::FieldSink) {
    use crate::metrics::wire::us_ms;
    let w = snapshot();
    s.count("n_echo", w.n);
    s.decimal("echo_p50_ms", us_ms(w.p50_us));
    s.decimal("echo_p95_ms", us_ms(w.p95_us));
    s.decimal("echo_p99_ms", us_ms(w.p99_us));
    s.decimal("echo_last_ms", us_ms(w.last_us));
    s.decimal("echo_max_ms", us_ms(w.max_us));
    s.count("echo_total", w.total);
    s.count("echo_arms", w.arms);
    s.count("echo_coalesced", w.coalesced);
    s.count("echo_expired", w.expired);
    s.count("echo_dropped_locked", w.dropped_locked);
}

/// An open bracket around one input dispatch.
pub(crate) struct EchoWriteProbe {
    session: u64,
    epoch: aterm_session::sink::InputEpoch,
}

impl crate::App {
    /// Open the echo bracket for input about to be routed to `session`.
    pub(crate) fn echo_probe_open(
        &self,
        wid: WindowId,
        session: Option<u64>,
    ) -> Option<EchoWriteProbe> {
        DEFERRED_IN_BRACKET.with(|deferred| deferred.set(false));
        forget_write_start();
        let session = session.or_else(|| self.focused_session_id(wid))?;
        let epoch = self.session_by_id(session)?.ctx.sink.input_epoch();
        Some(EchoWriteProbe { session, epoch })
    }

    /// Close the bracket. A write it made starts the clock where the write began, and an
    /// answer that came back while the dispatch was still running is caught here rather than
    /// left for the session's next output.
    pub(crate) fn echo_probe_close(&self, probe: Option<EchoWriteProbe>) {
        let deferred = DEFERRED_IN_BRACKET.with(|deferred| deferred.replace(false));
        let start_us = take_write_start();
        let Some(probe) = probe else { return };
        let Some(session) = self.session_by_id(probe.session) else {
            return;
        };
        if !deferred
            && session.ctx.sink.input_epoch() != probe.epoch
            && arms_reach_the_statics(&session.ctx.sink)
        {
            arm_catching(
                probe.session,
                start_us.unwrap_or_else(crate::metrics::now_us),
            );
        }
    }
}

/// The statics are process-global: every test that reads them takes turns (this module's and
/// the ordered writer's in `app_input`).
#[cfg(test)]
static SERIAL: Mutex<()> = Mutex::new(());

/// Take the echo statics for one test, zeroed.
#[cfg(test)]
pub(crate) fn fresh_for_test() -> std::sync::MutexGuard<'static, ()> {
    let g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    reset();
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> std::sync::MutexGuard<'static, ()> {
        fresh_for_test()
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

    /// Another session's output cannot close the clock, or a background flood would answer a
    /// keystroke typed into a quiet pane.
    #[test]
    fn another_sessions_output_cannot_close_the_clock() {
        let _g = fresh();
        arm_at(7, 1_000);
        assert_eq!(close_at(8, 2_000), None);
        assert_eq!(snapshot().total, 0);
        assert_eq!(
            close_at(7, 3_000),
            Some(2_000),
            "the real session still can"
        );
    }

    /// With nothing armed, output records nothing: a pure `cat`/`yes` flood stays out.
    #[test]
    fn output_with_no_outstanding_write_records_nothing() {
        let _g = fresh();
        for t in 0..1_000 {
            note_output_burst(7);
            assert_eq!(close_at(7, t), None);
        }
        assert_eq!(snapshot().total, 0);
    }

    /// Keep-oldest: a second key while one is outstanding does not re-arm, and is counted.
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

    /// Past the TTL the next keystroke takes the slot and the unanswered write is booked as
    /// expired, never as a sample.
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

    /// A session no other test's reader can be: its tag is not a small id.
    const QUIET: u64 = 0xBEE7;

    /// The echo came back while the dispatch that wrote it was still running, before the arm:
    /// it is the sample, measured from where the write began, and the session's next output
    /// (say, the next keystroke's echo half a second later) answers nothing.
    #[test]
    fn an_answer_that_beat_the_arm_is_its_sample_not_the_next_output() {
        let _g = fresh();
        note_output_burst_at(QUIET, 1_400);
        arm_catching(QUIET, 1_000);
        let s = snapshot();
        assert_eq!((s.arms, s.total, s.last_us), (1, 1, 400), "{s:?}");
        note_output_burst_at(QUIET, 500_000);
        assert_eq!(snapshot().total, 1, "nothing was left armed for it");
    }

    /// Output from before the write began is no answer to it: the clock stays armed for the
    /// session's next output.
    #[test]
    fn output_from_before_the_write_is_no_answer() {
        let _g = fresh();
        note_output_burst_at(QUIET, 900);
        arm_catching(QUIET, 1_000);
        assert_eq!(snapshot().total, 0);
        assert_eq!(close_at(QUIET, 1_250), Some(250));
    }

    /// A session that shares the burst slot cannot answer for this one.
    #[test]
    fn a_slot_sharing_sessions_output_is_not_this_sessions_answer() {
        let _g = fresh();
        let other = QUIET + BURST_SLOTS as u64;
        assert!(std::ptr::eq(burst_slot(other), burst_slot(QUIET)));
        note_output_burst_at(other, 1_400);
        arm_catching(QUIET, 1_000);
        assert_eq!(snapshot().total, 0);
        assert_eq!(
            close_at(QUIET, 1_600),
            Some(600),
            "its own output still does"
        );
    }

    /// A metrics reset forgets the bursts too, so an old one cannot answer a new write.
    #[test]
    fn a_reset_forgets_the_last_bursts() {
        let _g = fresh();
        note_output_burst_at(QUIET, 9_000);
        reset();
        arm_catching(QUIET, 1_000);
        assert_eq!(snapshot().total, 0);
    }

    /// A thread's write start is its first write's since it was forgotten.
    #[test]
    fn a_write_start_is_the_first_writes_until_forgotten() {
        forget_write_start();
        assert_eq!(take_write_start(), None);
        note_write_start();
        let first = WRITE_START_US.with(std::cell::Cell::get);
        note_write_start();
        assert_eq!(
            take_write_start(),
            Some(first),
            "a later write keeps the first"
        );
        assert_eq!(take_write_start(), None, "taking it clears it");
        note_write_start();
        forget_write_start();
        assert_eq!(take_write_start(), None);
    }

    /// A slow echo is recorded at its true length, not truncated.
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
        assert_eq!(s.p50_us, 501 + 511);
    }

    /// The nearest rank written as its definition, the smallest sample with at
    /// least `q·n` samples at or below it, rather than as the `ceil(q·n) - 1`
    /// index the production walk uses.
    fn nearest_rank(sorted: &[u32], q: f64) -> u64 {
        let need = q * sorted.len() as f64;
        let i = (0..sorted.len())
            .find(|&i| (i + 1) as f64 >= need)
            .expect("q <= 1 always reaches a rank");
        u64::from(sorted[i])
    }

    /// A copied window's percentiles equal a sorted reference over exactly the
    /// samples the window holds: one sample, two, an odd length, a full window,
    /// past the wrap, and refilled after a clear that left the old samples in
    /// the array beyond `len`.
    #[test]
    fn copied_window_percentiles_match_a_sorted_reference() {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut sample = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % 50_000
        };
        let check = |ring: &Ring, held: &[u64], case: &str| {
            let mut sorted: Vec<u32> = held
                .iter()
                .map(|&us| u32::try_from(us).expect("samples fit the ring's u32"))
                .collect();
            sorted.sort_unstable();
            let mut copy = ring.copy_out();
            assert_eq!(copy.len, held.len(), "{case}: the copy holds the window");
            let want = (
                nearest_rank(&sorted, 0.50),
                nearest_rank(&sorted, 0.95),
                nearest_rank(&sorted, 0.99),
            );
            assert_eq!(copy.percentiles(), want, "{case}");
        };
        for n in [1, 2, 63, WINDOW, WINDOW + 37] {
            let mut ring = Ring::new();
            let pushed: Vec<u64> = (0..n).map(|_| sample()).collect();
            for &us in &pushed {
                ring.push(us);
            }
            check(
                &ring,
                &pushed[n.saturating_sub(WINDOW)..],
                &format!("n={n}"),
            );
        }
        let mut ring = Ring::new();
        for _ in 0..WINDOW {
            ring.push(1_000_000 + sample());
        }
        ring.clear();
        let refill: Vec<u64> = (0..5).map(|_| sample()).collect();
        for &us in &refill {
            ring.push(us);
        }
        check(&ring, &refill, "refilled after a clear");

        let mut two = Ring::new();
        two.push(20);
        two.push(10);
        assert_eq!(two.copy_out().percentiles(), (10, 20, 20));
        let off_by_one = u64::from([10_u32, 20][(0.50_f64 * 2.0).ceil() as usize]);
        assert_ne!(off_by_one, nearest_rank(&[10, 20], 0.50));
    }
}
