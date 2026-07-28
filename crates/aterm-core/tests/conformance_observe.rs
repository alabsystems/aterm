// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the Observation Kernel (L0) — exercise the REAL engine
//! against the BEHAVIOR the abstract `watcher_latch_model` /
//! `idle_deadline_model` (`aterm-spec`) prove: drive a genuine [`Terminal`]
//! through [`Terminal::process_at`] with armed watchers and check the real latch
//! decisions, plus a negative control so a pass is never vacuous. (These are
//! behavioral conformance tests — they do not import or trace-validate the
//! abstract `Model`; that lives in `aterm-spec`'s own `ty` checks.)
//!
//! The headline property is **IdleFor-under-replay determinism**: feeding the
//! same `(bytes, ClockReading)` schedule and expiring at *different* instants (a
//! prompt live wake vs a lazy replay tick) latches the byte-identical
//! [`Satisfaction`](aterm_core::terminal::Satisfaction) — because the kernel
//! latches at the computed deadline, never the observation instant, and reads no
//! wall clock. This is what lets the kernel coexist with the hydratable temporal
//! buffer without perturbing `conformance_recording`.

use std::time::{Duration, Instant};

use aterm_core::terminal::{ClockReading, HostBindings, Terminal, WatcherSpec};

/// A fixed clock reading at `base + off_ms` — the injected-clock seam that makes
/// replay independent of real wall-clock pacing (mirrors `replay_offset_*`).
fn clock_at(base: Instant, off_ms: u64) -> ClockReading {
    ClockReading {
        monotonic: base + Duration::from_millis(off_ms),
        wall_ms: Some(off_ms),
    }
}

#[test]
fn seq_advanced_latches_on_real_engine_output() {
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);
    let seq0 = t.content_seq();
    let id = t
        .watch(WatcherSpec::SeqAdvanced { after: seq0 }, base)
        .expect("arm");
    assert!(t.watch_poll(id).is_none(), "pending before any output");

    // Real program output advances content_seq through the real pipeline.
    t.process_at(b"hello", clock_at(base, 10));

    let sat = t
        .watch_poll(id)
        .expect("real output advanced content_seq -> latched at the post_process seam");
    assert!(
        sat.seq > seq0,
        "latched seq reflects the real content advance"
    );
}

#[test]
fn seq_advanced_latches_at_arm_when_content_already_moved() {
    // REGRESSION: `SeqAdvanced` was edge-triggered — it was evaluated only from
    // `post_process` after the NEXT batch, so a watcher armed against a STALE
    // seq reported "nothing changed" while the advance was already on screen.
    // Observed over the control socket as:
    //     seq before = 2; send 'echo MARKER-A\n' -> seq after = 6
    //     await seq 2 timeout 2000  ->  OK timeout   (WRONG)
    // That is exactly a turn-based agent's dirty check ("did anything change
    // since the seq I recorded last turn?"), and on a quiet session no further
    // batch ever arrives, so it hung until the timeout and answered "no".
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);
    let stale = t.content_seq();

    // Content advances BEFORE the watcher is armed.
    t.process_at(b"MARKER-A\r\n", clock_at(base, 10));
    let after_output = t.content_seq();
    assert!(
        after_output > stale,
        "precondition: real output advanced content_seq ({stale} -> {after_output})"
    );

    // Arming against the pre-output seq must latch AT ARM — no further batch.
    let id = t
        .watch(WatcherSpec::SeqAdvanced { after: stale }, base)
        .expect("arm");
    let sat = t
        .watch_poll(id)
        .expect("an ALREADY-advanced content_seq must latch at arm, not on the next batch");
    assert_eq!(
        sat.seq, after_output,
        "latched at the live content_seq, not the stale anchor"
    );
}

#[test]
fn seq_advanced_arm_eval_does_not_disturb_a_concurrent_idle_deadline() {
    // The arm-time evaluation runs `observe_at`, which must stay ACTIVITY-NEUTRAL:
    // `Terminal::watch` seeds the activity baseline first, so the pass sees
    // `advanced == false` and cannot stamp phantom activity that would push out a
    // concurrently-armed `IdleFor` deadline.
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);
    t.process_at(b"prior output\r\n", clock_at(base, 5));

    let dur = Duration::from_millis(300);
    let arm_at = base + Duration::from_millis(10);
    let idle = t.watch(WatcherSpec::IdleFor { dur }, arm_at).expect("arm");
    let deadline_before = t.watch_next_deadline().expect("idle deadline armed");

    // Arm a SeqAdvanced that WILL latch at arm (stale anchor) — the interesting case.
    let _seq_watch = t
        .watch(WatcherSpec::SeqAdvanced { after: 0 }, arm_at)
        .expect("arm");

    assert_eq!(
        t.watch_next_deadline(),
        Some(deadline_before),
        "arm-time evaluation must not reset the idle deadline"
    );
    assert!(
        t.watch_poll(idle).is_none(),
        "the idle watcher itself must not latch early"
    );
}

#[test]
fn negative_control_non_content_batch_does_not_latch() {
    // A batch that produces NO content mutation must NOT latch a SeqAdvanced
    // watcher — proving the kernel is bound to the real `content_seq` clock, not
    // merely to "a process_at happened". (Vacuity guard.)
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);
    let id = t
        .watch(
            WatcherSpec::SeqAdvanced {
                after: t.content_seq(),
            },
            base,
        )
        .expect("arm");
    // A bare cursor-position query (DSR) emits a reply but paints no cells.
    t.process_at(b"\x1b[6n", clock_at(base, 10));
    assert!(
        t.watch_poll(id).is_none(),
        "a non-content batch must not latch a content watcher"
    );
}

#[test]
fn idle_latches_identically_live_vs_replay_on_the_real_engine() {
    // THE determinism property, end-to-end through the real engine: same
    // (bytes, clock) schedule, two different expire instants -> identical latch.
    let base = Instant::now();
    let schedule: &[(&[u8], u64)] = &[(b"a", 10), (b"b", 20), (b"cc", 35)];
    let dur = Duration::from_millis(250);

    let run = |expire_off_ms: u64| {
        let mut t = Terminal::new(24, 80);
        let id = t.watch(WatcherSpec::IdleFor { dur }, base).expect("arm");
        for (bytes, off) in schedule {
            t.process_at(bytes, clock_at(base, *off));
        }
        t.watch_expire(base + Duration::from_millis(expire_off_ms));
        t.watch_poll(id)
    };

    // Live: host wakes just after the deadline (last activity 35ms + 250ms + 1).
    let live = run(35 + 250 + 1);
    // Replay: a single lazy tick far in the "future" of the recorded schedule.
    let replay = run(100_000);

    assert_eq!(
        live, replay,
        "live and replay must latch the byte-identical Satisfaction"
    );
    assert_eq!(
        live.expect("latched").at,
        base + Duration::from_millis(35) + dur,
        "latched instant is the exact deadline (last activity + dur), not the wake"
    );
}

#[test]
fn idle_does_not_fire_before_the_deadline() {
    // Negative control for IdleFor: still-streaming output keeps pushing the
    // deadline out, so an expire mid-stream must NOT latch.
    let base = Instant::now();
    let dur = Duration::from_millis(100);
    let mut t = Terminal::new(24, 80);
    let id = t.watch(WatcherSpec::IdleFor { dur }, base).expect("arm");
    t.process_at(b"streaming", clock_at(base, 50));
    // Only 70ms since the last activity at 50ms (< 100ms): not idle yet.
    assert!(!t.watch_expire(base + Duration::from_millis(120)));
    assert!(
        t.watch_poll(id).is_none(),
        "must not latch before the deadline"
    );
    // After a full quiet window it latches.
    assert!(t.watch_expire(base + Duration::from_millis(150)));
    assert!(t.watch_poll(id).is_some());
}

#[test]
fn watchers_are_excluded_from_checkpoint_hydration() {
    // The replay-safety keystone: a checkpoint carries no watcher state, so a
    // hydrated engine starts with an EMPTY kernel — armed watchers never travel
    // through a keyframe and so cannot perturb replay determinism.
    let base = Instant::now();
    let mut t = Terminal::new(6, 20);
    t.process_at(b"seed\r\n", clock_at(base, 1));
    let _id = t
        .watch(WatcherSpec::SeqAdvanced { after: 0 }, base)
        .expect("arm");
    assert!(t.watchers_armed(), "armed before checkpoint");

    // Hydrate a fresh engine from this one's checkpoint.
    let cp = t.checkpoint();
    let hydrated = Terminal::from_checkpoint(&cp, HostBindings::none());
    assert!(
        !hydrated.watchers_armed(),
        "hydrated engine has an empty kernel — watchers are not checkpointed"
    );
}

#[test]
fn idle_baseline_is_arm_relative_not_reset_by_a_phantom_advance() {
    // Regression: the activity clock's `last_seq` defaults to 0. Arming an
    // `IdleFor` against a surface whose `content_seq` is ALREADY > 0 must NOT make
    // the first post-arm batch look like a fresh content advance and reset the
    // idle deadline. `Terminal::watch` seeds the baseline at arm to close this.
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);
    // Pre-existing content pushes content_seq well past 0.
    t.process_at(b"hello world\r\nmore output\r\n", clock_at(base, 5));
    assert!(
        t.content_seq() > 0,
        "precondition: content already advanced"
    );

    let dur = Duration::from_millis(300);
    let arm_at = base + Duration::from_millis(10);
    let id = t.watch(WatcherSpec::IdleFor { dur }, arm_at).expect("arm");

    // A content-LESS batch (a DSR cursor query paints no cells, so content_seq
    // does NOT advance) must not phantom-reset the idle deadline.
    t.process_at(b"\x1b[6n", clock_at(base, 50));

    // The deadline is ARM-relative: it fires at arm+dur, not at (50ms batch)+dur.
    assert!(
        t.watch_expire(arm_at + dur),
        "idle must latch at arm+dur — a phantom advance would have pushed it later"
    );
    assert_eq!(t.watch_poll(id).expect("latched").at, arm_at + dur);
}

/// REGRESSION (alt-screen blindness): `content_seq` is a **per-grid** counter, so
/// a 1049 swap makes it incomparable — entering restarts it at 1, leaving restores
/// the main grid's stale-but-higher value. The kernel used to compare it as if it
/// were a session clock, so for the entire alt-screen lifetime `advanced` was
/// false and EVERY predicate starved. That is precisely the advertised case: an
/// alt-screen agent TUI (Claude, vim, htop).
#[test]
fn row_match_still_latches_after_entering_the_alt_screen() {
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);

    // Build a high main-grid watermark, then seed the kernel clock to it (any
    // prior await/turn does this).
    for i in 0..40 {
        t.process_at(format!("line {i}\r\n").as_bytes(), clock_at(base, 1 + i));
    }
    let main_seq = t.content_seq();
    let seeded = t
        .watch(WatcherSpec::SeqAdvanced { after: main_seq }, base)
        .expect("arm");
    t.watch_disarm(seeded);
    assert!(main_seq > 1, "precondition: main grid counter is high");

    // Enter the alt screen: a FRESH grid whose content_gen restarts near 1.
    t.process_at(b"\x1b[?1049h", clock_at(base, 100));
    assert!(t.is_alternate_screen(), "precondition: on the alt screen");
    assert!(
        t.content_seq() < main_seq,
        "precondition: alt counter restarted below the main watermark ({} < {main_seq})",
        t.content_seq()
    );

    // Arm a row match, then paint the row on the ALT grid.
    let id = t
        .watch_rows(
            std::sync::Arc::new(ContainsReady),
            aterm_core::terminal::RowRange::All,
            clock_at(base, 101).monotonic,
        )
        .expect("arm");
    t.process_at(b"\x1b[HREADY\r\n", clock_at(base, 102));

    assert!(
        t.watch_poll(id).is_some(),
        "a row painted on the ALT grid must latch; the kernel used to compare the \
         alt counter against the main grid's watermark and never scan at all"
    );
}

/// The idle deadline must be reset by alt-screen output. Before the fix,
/// `advanced` was false for every alt batch, so `await idle`/`turn` reported
/// `settled` while the TUI was still streaming.
#[test]
fn idle_deadline_is_reset_by_alt_screen_output() {
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);
    for i in 0..40 {
        t.process_at(format!("line {i}\r\n").as_bytes(), clock_at(base, 1 + i));
    }
    let prime = t
        .watch(
            WatcherSpec::SeqAdvanced {
                after: t.content_seq(),
            },
            base,
        )
        .expect("arm");
    t.watch_disarm(prime);

    t.process_at(b"\x1b[?1049h", clock_at(base, 100));

    let dur = Duration::from_millis(300);
    let arm_at = base + Duration::from_millis(101);
    let idle = t.watch(WatcherSpec::IdleFor { dur }, arm_at).expect("arm");

    // Continuous alt-screen output well past the original deadline.
    for i in 0..10 {
        t.process_at(
            format!("\x1b[H chunk {i}\r\n").as_bytes(),
            clock_at(base, 150 + i * 50),
        );
    }
    let last_out = base + Duration::from_millis(150 + 9 * 50);

    // At the ORIGINAL deadline the watcher must NOT have fired: output kept
    // pushing it out.
    t.watch_expire(arm_at + dur);
    assert!(
        t.watch_poll(idle).is_none(),
        "idle latched despite continuous alt-screen output — the deadline was \
         never reset because the alt counter looked like it never advanced"
    );

    // It fires only after real quiescence past the LAST output.
    t.watch_expire(last_out + dur + Duration::from_millis(1));
    assert!(
        t.watch_poll(idle).is_some(),
        "idle must still fire once quiet"
    );
}

/// A buffer swap is itself an observable change, so a `SeqAdvanced` armed on one
/// grid latches when the other is installed — rather than starving (enter) or
/// firing on a stale high-water counter with no content change (exit).
#[test]
fn seq_watcher_latches_on_the_buffer_swap_itself() {
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);
    for i in 0..20 {
        t.process_at(format!("line {i}\r\n").as_bytes(), clock_at(base, 1 + i));
    }

    let id = t
        .watch(
            WatcherSpec::SeqAdvanced {
                after: t.content_seq(),
            },
            base,
        )
        .expect("arm");
    assert!(t.watch_poll(id).is_none(), "pending before the swap");

    t.process_at(b"\x1b[?1049h", clock_at(base, 100));
    assert!(
        t.watch_poll(id).is_some(),
        "entering the alt screen is a real surface change and must latch, not starve"
    );
}

#[derive(Debug)]
struct ContainsReady;
impl aterm_core::terminal::RowMatch for ContainsReady {
    fn matches(&self, row: &str) -> bool {
        row.contains("READY")
    }
}
