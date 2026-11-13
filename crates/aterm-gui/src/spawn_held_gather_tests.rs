// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    HELD_GATHER_ENOUGH, HELD_GATHER_MARGIN_NS, HELD_GATHER_MIN_NS, HELD_GATHER_STEP_NS,
    HELD_GATHER_WAIT_SLACK_NS, HoldRelease, OUTPUT_CADENCE_NS, PostCadence, STREAM_AFTER_NS,
    WAKE_LATCH_DIRTY, WAKE_LATCH_HELD, WAKE_LATCH_SENT, WAKE_LATCH_STREAMING, WakeGate,
    begin_held_gather, gated_output_wake, gather_held_output, output_latch_after_pass,
    release_output_hold,
};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};

const MS: u64 = 1_000_000;
const US: u64 = 1_000;

#[test]
fn a_gather_begins_only_on_a_held_latch_and_ends_before_its_due() {
    let now = 100 * MS;
    for word in [
        0,
        now,
        now | WAKE_LATCH_SENT,
        now | WAKE_LATCH_SENT | WAKE_LATCH_STREAMING,
    ] {
        let latch = AtomicU64::new(word);
        assert_eq!(begin_held_gather(&latch, now, 100, false), None);
        assert_eq!(latch.load(Ordering::Relaxed), word, "untouched");
    }
    let due = now + OUTPUT_CADENCE_NS;
    let held = WAKE_LATCH_HELD | due;
    for (filled, typing) in [(100, true), (HELD_GATHER_ENOUGH, false)] {
        let latch = AtomicU64::new(held);
        assert_eq!(begin_held_gather(&latch, now, filled, typing), None);
        assert_eq!(latch.load(Ordering::Relaxed), held, "untouched");
    }
    let latch = AtomicU64::new(held);
    assert_eq!(
        begin_held_gather(&latch, now, 100, false),
        Some(due - HELD_GATHER_MARGIN_NS)
    );
    assert_eq!(latch.load(Ordering::Relaxed), held | WAKE_LATCH_DIRTY);
    assert_eq!(
        begin_held_gather(&latch, now, 100, false),
        Some(due - HELD_GATHER_MARGIN_NS)
    );
    assert_eq!(latch.load(Ordering::Relaxed), held | WAKE_LATCH_DIRTY);
    for at in [due - HELD_GATHER_MARGIN_NS, due, due + 7 * MS] {
        let latch = AtomicU64::new(held);
        assert_eq!(
            begin_held_gather(&latch, at, 100, false),
            Some(at + HELD_GATHER_MIN_NS)
        );
    }
    let far = AtomicU64::new(WAKE_LATCH_HELD | (now + 50 * MS));
    assert_eq!(
        begin_held_gather(&far, now, 100, false),
        Some(now + OUTPUT_CADENCE_NS)
    );
}

struct Pipe {
    first: u64,
    gap: u64,
    len: usize,
    lines: usize,
    taken: Cell<usize>,
    clock: Cell<u64>,
}

impl Pipe {
    fn top_up(&self, buf: &mut [u8], filled: usize) -> usize {
        let written = if self.clock.get() < self.first {
            0
        } else {
            (((self.clock.get() - self.first) / self.gap) as usize + 1).min(self.lines)
        };
        let fit = (buf.len() - filled) / self.len;
        let n = written.saturating_sub(self.taken.get()).min(fit);
        self.taken.set(self.taken.get() + n);
        filled + n * self.len
    }
}

#[test]
fn a_gather_collects_until_its_deadline_a_key_or_a_batch() {
    let pipe = Pipe {
        first: 0,
        gap: 100 * US,
        len: 100,
        lines: 1_000,
        taken: Cell::new(1),
        clock: Cell::new(0),
    };
    let mut buf = vec![0u8; 65_536];
    let until = 3_500 * US;
    for overshoot in [HELD_GATHER_WAIT_SLACK_NS, 0] {
        pipe.taken.set(1);
        pipe.clock.set(0);
        let waits = RefCell::new(Vec::new());
        let filled = gather_held_output(
            &mut buf,
            100,
            until,
            || pipe.clock.get(),
            |b, f| pipe.top_up(b, f),
            |d| {
                waits.borrow_mut().push(d);
                pipe.clock
                    .set(pipe.clock.get() + d.as_nanos() as u64 + overshoot);
            },
            || true,
        );
        let end = pipe.clock.get();
        assert!(
            end <= until && until - end <= HELD_GATHER_WAIT_SLACK_NS,
            "overshoot {overshoot}: it ends on its deadline, not after (at {end})"
        );
        let written = (end / pipe.gap + 1) as usize;
        assert_eq!(
            filled,
            written * 100,
            "each line written by its end, in one batch"
        );
        assert!(
            waits
                .borrow()
                .iter()
                .all(|d| d.as_nanos() as u64 <= HELD_GATHER_STEP_NS),
            "{waits:?}"
        );
    }
    let pipe = Pipe {
        taken: Cell::new(1),
        clock: Cell::new(0),
        ..pipe
    };
    let steps = Cell::new(0);
    let filled = gather_held_output(
        &mut buf,
        100,
        until,
        || pipe.clock.get(),
        |b, f| pipe.top_up(b, f),
        |d| pipe.clock.set(pipe.clock.get() + d.as_nanos() as u64),
        || {
            steps.set(steps.get() + 1);
            steps.get() < 2
        },
    );
    assert_eq!(
        pipe.clock.get(),
        HELD_GATHER_STEP_NS,
        "one step, then the key"
    );
    assert_eq!(filled, 11 * 100);
    let fast = Pipe {
        first: 0,
        gap: US,
        len: 100,
        lines: 1_000,
        taken: Cell::new(1),
        clock: Cell::new(1_000 * US),
    };
    let filled = gather_held_output(
        &mut buf,
        100,
        until,
        || fast.clock.get(),
        |b, f| fast.top_up(b, f),
        |_| panic!("no wait: the pipe already held a batch"),
        || true,
    );
    assert!(filled >= HELD_GATHER_ENOUGH, "{filled}");
    assert_eq!(fast.clock.get(), 1_000 * US);
}

struct Ui {
    latch: AtomicU64,
    queued: Cell<Option<u64>>,
    deadline: Cell<Option<u64>>,
    late: u64,
    passes: RefCell<Vec<u64>>,
}

impl Ui {
    fn pass(&self, now: u64) {
        let word = self.latch.load(Ordering::Relaxed);
        self.latch
            .store(output_latch_after_pass(word, now), Ordering::Relaxed);
        self.passes.borrow_mut().push(now);
    }

    fn turns_until(&self, t: u64) {
        loop {
            let release_at = self.deadline.get().map(|d| d + self.late);
            let wake_turn = self
                .queued
                .get()
                .filter(|&q| release_at.is_none_or(|d| q <= d));
            let Some(now) = wake_turn.or(release_at).filter(|&at| at <= t) else {
                return;
            };
            if wake_turn.is_some() {
                self.queued.set(None);
                self.pass(now);
            }
            self.deadline
                .set(match release_output_hold(&self.latch, now) {
                    HoldRelease::Pending { due_ns } => Some(due_ns),
                    HoldRelease::Due => {
                        self.pass(now);
                        Some(now + OUTPUT_CADENCE_NS)
                    }
                    HoldRelease::NotHeld | HoldRelease::Released => None,
                });
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Timing {
    feed: u64,
    overshoot: u64,
    late: u64,
}

fn drive_reader(pipe: &Pipe, gather: bool, timing: Timing) -> (Vec<u64>, usize, Vec<u64>) {
    let ui = Ui {
        latch: AtomicU64::new(0),
        queued: Cell::new(None),
        deadline: Cell::new(None),
        late: timing.late,
        passes: RefCell::new(Vec::new()),
    };
    let mut cadence = PostCadence::default();
    let mut fed_at = Vec::new();
    let mut feeds = 0;
    let mut buf = vec![0u8; 65_536];
    while pipe.taken.get() < pipe.lines {
        let next = pipe.first + pipe.taken.get() as u64 * pipe.gap;
        pipe.clock.set(pipe.clock.get().max(next));
        ui.turns_until(pipe.clock.get());
        let before = pipe.taken.get();
        let mut filled = pipe.top_up(&mut buf, 0);
        if gather && let Some(until) = begin_held_gather(&ui.latch, pipe.clock.get(), filled, false)
        {
            filled = gather_held_output(
                &mut buf,
                filled,
                until,
                || pipe.clock.get(),
                |b, f| pipe.top_up(b, f),
                |d| {
                    pipe.clock
                        .set(pipe.clock.get() + d.as_nanos() as u64 + timing.overshoot);
                    ui.turns_until(pipe.clock.get());
                },
                || true,
            );
        }
        assert_eq!(filled, (pipe.taken.get() - before) * pipe.len);
        ui.turns_until(pipe.clock.get());
        let now = pipe.clock.get();
        fed_at.extend(std::iter::repeat_n(now, pipe.taken.get() - before));
        feeds += 1;
        let gate = gated_output_wake(&ui.latch, now, 0, cadence.streaming(now), || true, || now);
        match gate {
            WakeGate::Posted | WakeGate::Reposted => {
                cadence.posted(now);
                ui.queued.set(Some(now + 20 * US));
            }
            WakeGate::Held => cadence.held(now),
            WakeGate::Coalesced => {}
        }
        pipe.clock.set(now + timing.feed);
    }
    ui.turns_until(u64::MAX);
    assert_eq!(ui.latch.load(Ordering::Relaxed), 0, "quiet: nothing held");
    (fed_at, feeds, ui.passes.into_inner())
}

#[test]
fn a_held_stream_is_fed_once_per_hold_not_once_per_line() {
    let pipe = || Pipe {
        first: 1_000 * MS,
        gap: 100 * US,
        len: 100,
        lines: 2_000,
        taken: Cell::new(0),
        clock: Cell::new(0),
    };
    for (overshoot, late) in [
        (0, 0),
        (HELD_GATHER_WAIT_SLACK_NS, 0),
        (HELD_GATHER_WAIT_SLACK_NS, 500 * US),
        (0, MS),
    ] {
        let timing = Timing {
            feed: 20 * US,
            overshoot,
            late,
        };
        let p = pipe();
        let (fed_at, feeds, passes) = drive_reader(&p, true, timing);
        let bound = (STREAM_AFTER_NS / (100 * US)
            + 4 * (200 * MS - STREAM_AFTER_NS) / OUTPUT_CADENCE_NS
            + 8) as usize;
        assert!(
            feeds <= bound,
            "{timing:?}: {feeds} feeds for 2,000 lines (at most {bound})"
        );
        let lag = OUTPUT_CADENCE_NS + HELD_GATHER_MARGIN_NS + HELD_GATHER_MIN_NS + late;
        for (i, &fed) in fed_at.iter().enumerate() {
            let written = p.first + i as u64 * p.gap;
            let pass = passes.iter().find(|&&at| at > fed).copied();
            assert!(
                pass.is_some_and(|at| at - written <= lag),
                "{timing:?}: line {i}, written at {} µs and fed at {} µs, waits for {pass:?}",
                written / US,
                fed / US
            );
        }
        let (_, feeds_ungathered, _) = drive_reader(&pipe(), false, timing);
        assert_eq!(feeds_ungathered, 2_000, "{timing:?}");
    }
}

#[test]
fn a_typed_echo_is_never_gathered() {
    for key in 0..40u64 {
        let p = Pipe {
            first: 1_000 * MS + key * 80 * MS,
            gap: 40 * US,
            len: 12,
            lines: 7,
            taken: Cell::new(0),
            clock: Cell::new(0),
        };
        let timing = Timing {
            feed: 20 * US,
            overshoot: HELD_GATHER_WAIT_SLACK_NS,
            late: 0,
        };
        let (fed_at, feeds, _) = drive_reader(&p, true, timing);
        assert_eq!(feeds, 7, "key {key}: a feed per read");
        for (i, &fed) in fed_at.iter().enumerate() {
            assert_eq!(fed, p.first + i as u64 * p.gap, "key {key}: fed as read");
        }
    }
}
