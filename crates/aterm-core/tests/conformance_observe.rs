// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Observation kernel conformance tests on a live [`Terminal`].

use std::time::{Duration, Instant};

use aterm_core::terminal::{ClockReading, Terminal, WatcherSpec};

#[expect(
    clippy::disallowed_methods,
    reason = "test code: the one instant a test mints and feeds in"
)]
fn minted_now() -> Instant {
    Instant::now()
}

fn clock_at(base: Instant, off_ms: u64) -> ClockReading {
    ClockReading {
        monotonic: base + Duration::from_millis(off_ms),
        wall_ms: Some(off_ms),
    }
}

#[test]
fn seq_advanced_latches_on_real_engine_output() {
    let base = minted_now();
    let mut t = Terminal::new(24, 80);
    let seq0 = t.content_seq();
    let id = t
        .watch(WatcherSpec::SeqAdvanced { after: seq0 }, base)
        .expect("arm");
    assert!(t.watch_poll(id).is_none(), "pending before any output");

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
    let base = minted_now();
    let mut t = Terminal::new(24, 80);
    let stale = t.content_seq();

    t.process_at(b"MARKER-A\r\n", clock_at(base, 10));
    let after_output = t.content_seq();
    assert!(
        after_output > stale,
        "precondition: real output advanced content_seq ({stale} -> {after_output})"
    );

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
    let base = minted_now();
    let mut t = Terminal::new(24, 80);
    t.process_at(b"prior output\r\n", clock_at(base, 5));

    let dur = Duration::from_millis(300);
    let arm_at = base + Duration::from_millis(10);
    let idle = t.watch(WatcherSpec::IdleFor { dur }, arm_at).expect("arm");
    let deadline_before = t.watch_next_deadline().expect("idle deadline armed");

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
fn idle_latches_identically_live_vs_replay_on_the_real_engine() {
    let base = minted_now();
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

    let live = run(35 + 250 + 1);
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
    let base = minted_now();
    let dur = Duration::from_millis(100);
    let mut t = Terminal::new(24, 80);
    let id = t.watch(WatcherSpec::IdleFor { dur }, base).expect("arm");
    t.process_at(b"streaming", clock_at(base, 50));
    assert!(!t.watch_expire(base + Duration::from_millis(120)));
    assert!(
        t.watch_poll(id).is_none(),
        "must not latch before the deadline"
    );
    assert!(t.watch_expire(base + Duration::from_millis(150)));
    assert!(t.watch_poll(id).is_some());
}

#[test]
fn watchers_are_excluded_from_checkpoint_hydration() {
    let base = minted_now();
    let mut t = Terminal::new(6, 20);
    t.process_at(b"seed\r\n", clock_at(base, 1));
    let _id = t
        .watch(WatcherSpec::SeqAdvanced { after: 0 }, base)
        .expect("arm");
    assert!(t.watchers_armed(), "armed before checkpoint");

    let cp = t.checkpoint();
    let hydrated = Terminal::from_checkpoint(&cp);
    assert!(
        !hydrated.watchers_armed(),
        "hydrated engine has an empty kernel — watchers are not checkpointed"
    );
}

#[test]
fn idle_baseline_is_arm_relative_not_reset_by_a_phantom_advance() {
    let base = minted_now();
    let mut t = Terminal::new(24, 80);
    t.process_at(b"hello world\r\nmore output\r\n", clock_at(base, 5));
    assert!(
        t.content_seq() > 0,
        "precondition: content already advanced"
    );

    let dur = Duration::from_millis(300);
    let arm_at = base + Duration::from_millis(10);
    let id = t.watch(WatcherSpec::IdleFor { dur }, arm_at).expect("arm");

    t.process_at(b"\x1b[6n", clock_at(base, 50));

    assert!(
        t.watch_expire(arm_at + dur),
        "idle must latch at arm+dur — a phantom advance would have pushed it later"
    );
    assert_eq!(t.watch_poll(id).expect("latched").at, arm_at + dur);
}

#[test]
fn row_match_still_latches_after_entering_the_alt_screen() {
    let base = minted_now();
    let mut t = Terminal::new(24, 80);

    for i in 0..40 {
        t.process_at(format!("line {i}\r\n").as_bytes(), clock_at(base, 1 + i));
    }
    let main_seq = t.content_seq();
    let seeded = t
        .watch(WatcherSpec::SeqAdvanced { after: main_seq }, base)
        .expect("arm");
    t.watch_disarm(seeded);
    assert!(main_seq > 1, "precondition: main grid counter is high");

    t.process_at(b"\x1b[?1049h", clock_at(base, 100));
    assert!(t.is_alternate_screen(), "precondition: on the alt screen");
    assert!(
        t.content_seq() < main_seq,
        "precondition: alt counter restarted below the main watermark ({} < {main_seq})",
        t.content_seq()
    );

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
        "a row painted on the ALT grid must latch"
    );
}

#[test]
fn idle_deadline_is_reset_by_alt_screen_output() {
    let base = minted_now();
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

    for i in 0..10 {
        t.process_at(
            format!("\x1b[H chunk {i}\r\n").as_bytes(),
            clock_at(base, 150 + i * 50),
        );
    }
    let last_out = base + Duration::from_millis(150 + 9 * 50);

    t.watch_expire(arm_at + dur);
    assert!(
        t.watch_poll(idle).is_none(),
        "idle latched despite continuous alt-screen output — the deadline was \
         never reset because the alt counter looked like it never advanced"
    );

    t.watch_expire(last_out + dur + Duration::from_millis(1));
    assert!(
        t.watch_poll(idle).is_some(),
        "idle must still fire once quiet"
    );
}

#[test]
fn seq_watcher_latches_on_the_buffer_swap_itself() {
    let base = minted_now();
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
    fn verdict(&self, row: &str) -> aterm_core::terminal::RowVerdict {
        row.contains("READY").into()
    }
}

fn dialog(label: &str) -> Vec<u8> {
    format!("\x1b[H\x1b[2J Bash command\r\n\r\n   {label}\r\n\r\n ❯ 1. Yes\r\n   2. No\r\n")
        .into_bytes()
}

#[derive(Debug)]
struct ContainsBoxB;
impl aterm_core::terminal::RowMatch for ContainsBoxB {
    fn verdict(&self, row: &str) -> aterm_core::terminal::RowVerdict {
        row.contains("rm-box-B").into()
    }
}

#[test]
fn a_row_match_latches_across_an_equal_seq_alt_screen_re_entry() {
    for split in [false, true] {
        let base = minted_now();
        let mut t = Terminal::new(24, 80);
        t.process_at(b"\x1b[?1049h", clock_at(base, 1));
        t.process_at(&dialog("ls-box-A"), clock_at(base, 2));
        let seq_a = t.content_seq();
        let epoch_a = t.content_scroll_state().invalidation_epoch;

        let id = t
            .watch_rows(
                std::sync::Arc::new(ContainsBoxB),
                aterm_core::terminal::RowRange::All,
                clock_at(base, 3).monotonic,
            )
            .expect("arm");
        let seq_watch = t
            .watch(WatcherSpec::SeqAdvanced { after: seq_a }, base)
            .expect("arm");
        assert!(t.watch_poll(id).is_none(), "box B is not on screen yet");
        assert!(t.watch_poll(seq_watch).is_none(), "nothing moved yet");

        let mut reentry = b"\x1b[?1049l\x1b[?1049h".to_vec();
        if split {
            t.process_at(&reentry, clock_at(base, 4));
            t.process_at(&dialog("rm-box-B"), clock_at(base, 5));
        } else {
            reentry.extend_from_slice(&dialog("rm-box-B"));
            t.process_at(&reentry, clock_at(base, 4));
        }

        assert!(t.is_alternate_screen(), "split={split}");
        assert_eq!(t.content_seq(), seq_a, "split={split}: the seq repeats");
        assert!(
            t.content_scroll_state().invalidation_epoch > epoch_a,
            "split={split}: the screen switch advanced the generation"
        );
        assert!(
            (0..t.rows() as usize).any(|i| t.row_text(i).is_some_and(|r| r.contains("rm-box-B"))),
            "split={split}: box B is on screen"
        );

        assert!(
            t.watch_poll(id).is_some(),
            "split={split}: box B is on screen and `await match` must latch on it"
        );
        assert!(
            t.watch_poll(seq_watch).is_some(),
            "split={split}: a re-entered screen is a surface change for `await seq`"
        );
    }
}

#[test]
fn the_kernel_change_test_conforms_to_the_screen_generation_model() {
    use aterm_models_core::observation_screen_generation_model;
    use aterm_spec::interp::{State, with_buggy};

    let model = observation_screen_generation_model();
    let old = with_buggy(&model, 1);
    let base = minted_now();
    let mut t = Terminal::new(24, 80);
    t.process_at(b"\x1b[?1049h", clock_at(base, 1));

    let mut reentry_paint = b"\x1b[?1049l\x1b[?1049h".to_vec();
    reentry_paint.extend_from_slice(&dialog("rm-box-B"));
    let batches: Vec<Vec<u8>> = vec![
        dialog("ls-box-A"),
        reentry_paint,
        b"x".to_vec(),
        b"\x1b[?1049l\x1b[?1049h".to_vec(),
        dialog("ls-box-A"),
        b"\x1b[3;3H".to_vec(),
    ];
    let mut old_disagreed = false;
    for (i, bytes) in batches.iter().enumerate() {
        let (seq0, epoch0) = (t.content_seq(), t.content_scroll_state().invalidation_epoch);
        let id = t
            .watch(WatcherSpec::SeqAdvanced { after: seq0 }, base)
            .expect("arm");
        t.process_at(bytes, clock_at(base, 10 + i as u64));
        let (seq1, epoch1) = (t.content_seq(), t.content_scroll_state().invalidation_epoch);
        let real = t.watch_poll(id).is_some();
        t.watch_disarm(id);

        let state = |seq: u64, epoch: u64| -> State {
            let mut s = model.init_state();
            for (k, v) in [
                ("seq", seq),
                ("epoch", epoch),
                ("seen_seq", seq0),
                ("seen_epoch", epoch0),
            ] {
                s.insert(k, i64::try_from(v).expect("small"));
            }
            s.insert("dirty", 1);
            s
        };
        let changed = seq1 != seq0 || epoch1 != epoch0;
        if !changed {
            assert!(!real, "batch {i}: an unchanged surface latched");
            continue;
        }
        let mut s = state(seq1, epoch1);
        assert!(model.fire("Observe", &mut s));
        assert_eq!(real, s["missed"] == 0, "batch {i}: kernel vs model");
        assert!(model.check_invariant("EveryChangeIsSeen", &s), "batch {i}");
        let mut o = state(seq1, epoch1);
        assert!(old.fire("Observe", &mut o));
        old_disagreed |= (o["missed"] == 0) != real;
    }
    assert!(
        old_disagreed,
        "the schedule must contain a batch the old (seq, alt) test misses"
    );
}

#[test]
fn row_reads_follow_the_live_screen_not_the_scrolled_view() {
    use aterm_core::terminal::{RowRange, first_matching_row};
    let base = minted_now();
    let mut t = Terminal::new(8, 40);
    for i in 0..30 {
        t.process_at(
            format!("history line {i}\r\n").as_bytes(),
            clock_at(base, 1 + i),
        );
    }
    t.process_at(b" READY to proceed?\r\n  2. No", clock_at(base, 40));
    let live = first_matching_row(&t, &ContainsReady, RowRange::All);
    assert!(live.is_some(), "precondition: the box is on screen");
    t.scroll_display(3);
    assert_eq!(
        first_matching_row(&t, &ContainsReady, RowRange::All),
        live,
        "scrolled back, the live row still matches"
    );
    let id = t
        .watch_rows(
            std::sync::Arc::new(ContainsReady),
            RowRange::All,
            clock_at(base, 41).monotonic,
        )
        .expect("arm");
    assert!(t.watch_poll(id).is_some(), "the live row latches at arm");

    let mut t = Terminal::new(8, 40);
    t.process_at(b" READY to proceed?\r\n", clock_at(base, 1));
    for i in 0..20 {
        t.process_at(
            format!("later line {i}\r\n").as_bytes(),
            clock_at(base, 2 + i),
        );
    }
    t.scroll_display(20);
    assert_eq!(
        first_matching_row(&t, &ContainsReady, RowRange::All),
        None,
        "a history copy is not on the live screen"
    );
}
