// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared writer's batching, how it takes a streaming reader's slices ([`super::serve`]), and
//! how it spends its quiet periods judging closed chains ([`super::WriterChannel`]).

use super::urgency::{Demands, Reason};
use super::{
    ChainJudge, FORK_EVERY, IdleJudge, Inbox, InputSlice, Mailbox, QUIET_WINDOW, RewindPool,
    TemporalMsg, TemporalRecorder, WriterChannel, keyframe_of, replay_hook, serve,
};
use aterm_core::terminal::{ClockReading, Terminal};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};

/// An inbox whose messages arrive on a script: each wait on the channel, and each nap, lets the
/// next arrival reach the queue, the way a reader streaming output keeps queueing slices while
/// the writer is busy or napping.
struct Script {
    arrivals: VecDeque<Vec<u32>>,
    queued: VecDeque<u32>,
    waits: usize,
    naps: usize,
}

impl Script {
    fn new(arrivals: impl IntoIterator<Item = Vec<u32>>) -> Self {
        Self {
            arrivals: arrivals.into_iter().collect(),
            queued: VecDeque::new(),
            waits: 0,
            naps: 0,
        }
    }

    fn arrive(&mut self) {
        if let Some(batch) = self.arrivals.pop_front() {
            self.queued.extend(batch);
        }
    }
}

impl Inbox<u32> for Script {
    fn wait(&mut self) -> Option<u32> {
        self.waits += 1;
        if self.queued.is_empty() {
            self.arrive();
        }
        self.queued.pop_front()
    }

    fn take(&mut self) -> Option<u32> {
        self.queued.pop_front()
    }

    fn nap(&mut self) {
        self.naps += 1;
        self.arrive();
    }
}

#[test]
fn a_streaming_reader_queues_batches_and_the_writer_waits_on_the_channel_only_when_it_stops() {
    // Two queued slices start a batch; six more arrive during naps, then the stream stops.
    let mut inbox = Script::new(
        [vec![1, 2]]
            .into_iter()
            .chain((3..=8).map(|n| vec![n]))
            .chain([vec![]]),
    );
    let mut written = Vec::new();
    serve(&mut inbox, || false, |n| written.push(n));
    assert_eq!(
        written,
        (1..=8).collect::<Vec<_>>(),
        "every slice, in order"
    );
    assert_eq!(
        inbox.waits, 2,
        "the writer waited on the channel, where each send wakes it, only to start and once the \
         stream stopped, not once per slice"
    );
    assert_eq!(
        inbox.naps, 7,
        "it napped after emptying the backlog each time, the last nap bringing nothing"
    );
}

#[test]
fn a_batch_queued_during_a_nap_is_written_whole_before_the_next_nap() {
    let mut inbox = Script::new([vec![1, 2], vec![3, 4], vec![5, 6], vec![]]);
    let mut written = Vec::new();
    serve(&mut inbox, || false, |n| written.push(n));
    assert_eq!(written, vec![1, 2, 3, 4, 5, 6]);
    assert_eq!((inbox.waits, inbox.naps), (2, 3));
}

#[test]
fn isolated_arrivals_never_schedule_a_batch_nap() {
    let mut inbox = Script::new((1..=4).map(|n| vec![n]));
    let mut written = Vec::new();
    serve(&mut inbox, || false, |n| written.push(n));
    assert_eq!(written, vec![1, 2, 3, 4]);
    assert_eq!((inbox.waits, inbox.naps), (5, 0));
}

#[test]
fn an_empty_nap_ends_batching_before_the_next_isolated_arrival() {
    let mut inbox = Script::new([vec![1, 2], vec![3], vec![], vec![4]]);
    let mut written = Vec::new();
    serve(&mut inbox, || false, |n| written.push(n));
    assert_eq!(written, vec![1, 2, 3, 4]);
    assert_eq!((inbox.waits, inbox.naps), (3, 2));
}

#[test]
fn an_urgent_writer_never_naps_and_waits_on_the_channel_for_each_arrival() {
    let mut inbox = Script::new([vec![1, 2], vec![3, 4]]);
    let mut written = Vec::new();
    serve(&mut inbox, || true, |n| written.push(n));
    assert_eq!(written, vec![1, 2, 3, 4]);
    assert_eq!(
        (inbox.waits, inbox.naps),
        (3, 0),
        "a drain, a view or queue pressure is served as it arrives, never after a nap"
    );
}

#[test]
fn each_demand_requests_a_wake_after_publishing_urgency() {
    let demands = Demands::new();
    let writer = std::thread::current();
    let mut woken = Vec::new();
    let early = demands.acquire_with(Reason::Drain, |thread| woken.push(thread.id()));
    assert!(woken.is_empty(), "no writer has adopted the demands yet");
    drop(early);

    demands.adopt_writer(writer.clone());
    let mut held = Vec::new();
    for (index, reason) in [Reason::Drain, Reason::View, Reason::Pressure]
        .into_iter()
        .enumerate()
    {
        held.push(demands.acquire_with(reason, |thread| {
            assert!(demands.is_urgent(), "publish urgency before waking");
            assert_eq!(demands.snapshot().counts.iter().sum::<usize>(), index + 1);
            woken.push(thread.id());
        }));
    }
    assert_eq!(woken, vec![writer.id(); 3], "every demand wakes its writer");
    drop(held);
    assert!(!demands.is_urgent());
}

/// How the writer waited on its channel: at most [`QUIET_WINDOW`], or as long as it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Waited {
    Timed,
    Untimed,
}

/// A channel a test queues messages on, recording each wait the writer makes on it. A wait with
/// no timeout takes the next message, or ends serving when none is queued.
#[derive(Clone, Default)]
struct Channel {
    queued: Rc<RefCell<VecDeque<u32>>>,
    waits: Rc<RefCell<Vec<Waited>>>,
}

impl Mailbox for Channel {
    type Msg = u32;

    fn recv(&mut self) -> Option<u32> {
        self.waits.borrow_mut().push(Waited::Untimed);
        self.queued.borrow_mut().pop_front()
    }

    fn recv_timeout(&mut self, timeout: std::time::Duration) -> Result<u32, RecvTimeoutError> {
        assert_eq!(timeout, QUIET_WINDOW, "the writer's one quiet window");
        self.waits.borrow_mut().push(Waited::Timed);
        self.queued
            .borrow_mut()
            .pop_front()
            .ok_or(RecvTimeoutError::Timeout)
    }

    fn try_recv(&mut self) -> Option<u32> {
        self.queued.borrow_mut().pop_front()
    }
}

/// A recording seeded at `live`'s screen, charged to `pool`, keeping its costs.
fn recording(live: &Terminal, pool: &Arc<RewindPool>) -> Arc<Mutex<TemporalRecorder>> {
    let mut rec = TemporalRecorder::new();
    Arc::get_mut(&mut rec.spine)
        .expect("an unshared spine")
        .costs = crate::recording_cost::Owner::on();
    rec.record_keyframe(keyframe_of(live, usize::MAX).expect("keyable"));
    let rec = Arc::new(Mutex::new(rec));
    pool.join(1, &rec);
    rec
}

/// Feed `bytes` to `live` and record them as one slice.
fn put(live: &mut Terminal, rec: &mut TemporalRecorder, bytes: &[u8]) {
    let at = ClockReading::now();
    live.process_at(bytes, at);
    rec.apply(TemporalMsg::RawIn(InputSlice::at(bytes, at)));
}

/// Record `live`'s screen as a witness.
fn see(live: &Terminal, rec: &mut TemporalRecorder) {
    let seen = live.checkpoint_visible().expect("visible");
    rec.apply(TemporalMsg::Witness(Arc::new(seen)));
}

/// End the newest chain with a keyframe of `live`.
fn close(live: &Terminal, rec: &mut TemporalRecorder) {
    rec.record_keyframe(keyframe_of(live, usize::MAX).expect("keyable"));
}

/// One 4080-byte slice of full lines.
fn slice() -> Vec<u8> {
    format!("{}\r\n", "x".repeat(78)).repeat(51).into_bytes()
}

/// Record on `live` a chain of 48 slices with a witness after every eighth, which a judge folds
/// in three passes, two witnesses each, then close it.
fn three_pass_chain(live: &mut Terminal, rec: &mut TemporalRecorder) {
    for n in 1..=48 {
        put(live, rec, &slice());
        if n % 8 == 0 {
            see(live, rec);
        }
    }
    close(live, rec);
}

/// With no demand, one quiet period judges every closed chain that holds a pending witness, the
/// newest first and each with the recorder unlocked, leaves the open chain's witness for a judge
/// on demand, clears the due judge, and the writer then waits with no timeout.
#[test]
fn the_idle_writer_judges_closed_chains_then_blocks() {
    let pool = RewindPool::new(usize::MAX);
    let mut live = Terminal::new(6, 30);
    let recorder = recording(&live, &pool);
    {
        let mut rec = recorder.lock().unwrap();
        // Three closed chains holding one, two and three pending witnesses, then the open one.
        for (chain, witnesses) in [1, 2, 3, 1].into_iter().enumerate() {
            if chain > 0 {
                close(&live, &mut rec);
            }
            for n in 0..witnesses {
                put(
                    &mut live,
                    &mut rec,
                    format!("chain {chain} line {n}\r\n").as_bytes(),
                );
                see(&live, &mut rec);
            }
        }
        assert_eq!(
            (rec.keyframe_count(), rec.pending(), rec.witnessed()),
            (4, 7, 0),
            "fixture: four chains, every witness pending"
        );
    }
    assert!(
        pool.judge_due(),
        "closing a chain that holds a pending witness makes a judge due"
    );
    let seen = Rc::new(RefCell::new(Vec::new()));
    let _hook = replay_hook::install({
        let (recorder, seen) = (recorder.clone(), seen.clone());
        move || {
            let rec = recorder
                .try_lock()
                .expect("a chain is judged with its recorder unlocked");
            seen.borrow_mut().push(rec.pending());
        }
    });
    let channel = Channel::default();
    let mut inbox = WriterChannel::new(channel.clone(), pool.clone(), Demands::new());
    assert_eq!(inbox.wait(), None, "no message came");
    assert_eq!(
        *seen.borrow(),
        [7, 4, 2],
        "the newest closed chain first: before its three witnesses, then the next's two, then the oldest's one"
    );
    {
        let rec = recorder.lock().unwrap();
        assert_eq!(
            (rec.pending(), rec.witnessed(), rec.diverged()),
            (1, 6, 0),
            "only the open chain's witness is left pending"
        );
    }
    assert_eq!(
        *channel.waits.borrow(),
        [Waited::Timed, Waited::Untimed],
        "one quiet period, then a wait with no timeout"
    );
    assert!(!pool.relief_due(), "fixture: no allocator relief is owed");
    assert_eq!(inbox.wait(), None);
    assert_eq!(
        *channel.waits.borrow(),
        [Waited::Timed, Waited::Untimed, Waited::Untimed],
        "with nothing left due, the next wait has no timeout"
    );
    assert!(!pool.judge_due(), "no closed chain is left to judge");
}

/// A message that arrives while the idle writer judges is served after the pass under way, which
/// folds at most FORK_EVERY bytes and one slice; that pass's verdicts are posted first, and the
/// parked job carries on in the next quiet period without restoring its chain again.
#[test]
fn an_idle_judge_yields_to_a_message_within_one_pass() {
    let pool = RewindPool::new(usize::MAX);
    let mut live = Terminal::new(24, 80);
    let recorder = recording(&live, &pool);
    {
        let mut rec = recorder.lock().unwrap();
        three_pass_chain(&mut live, &mut rec);
        put(&mut live, &mut rec, b"open\r\n");
        see(&live, &mut rec);
        assert_eq!(
            rec.pending(),
            7,
            "fixture: six witnesses closed in, one open"
        );
    }
    let costs = recorder
        .lock()
        .unwrap()
        .spine
        .costs
        .handle()
        .expect("costs kept");
    let channel = Channel::default();
    let restores = Rc::new(Cell::new(0));
    let injected = Rc::new(Cell::new(None));
    let _hook = replay_hook::install({
        let (queued, costs, restores, injected) = (
            channel.queued.clone(),
            costs.clone(),
            restores.clone(),
            injected.clone(),
        );
        move || {
            restores.set(restores.get() + 1);
            if injected.get().is_none() {
                queued.borrow_mut().push_back(7);
                injected.set(costs.judge.work());
            }
        }
    });
    let mut inbox = WriterChannel::new(channel.clone(), pool.clone(), Demands::new());
    assert_eq!(inbox.wait(), Some(7), "the message is served");
    let folded = costs.judge.work().expect("each pass books its bytes")
        - injected.get().expect("the judge restored a chain");
    assert!(
        folded <= (FORK_EVERY + slice().len()) as u64,
        "{folded} bytes folded between the message's arrival and its service"
    );
    {
        let rec = recorder.lock().unwrap();
        assert_eq!(
            (rec.witnessed(), rec.pending()),
            (2, 5),
            "the pass's verdicts are posted before the message is served"
        );
    }
    assert!(pool.judge_due(), "the parked job keeps a judge due");
    assert_eq!(*channel.waits.borrow(), [Waited::Timed]);

    assert_eq!(inbox.wait(), None);
    assert_eq!(restores.get(), 1, "the parked job carried on");
    assert_eq!(
        (costs.judge.calls(), costs.judge.work()),
        (Some(3), Some(48 * slice().len() as u64)),
        "three passes folded the chain once"
    );
    {
        let rec = recorder.lock().unwrap();
        assert_eq!((rec.witnessed(), rec.diverged(), rec.pending()), (6, 0, 1));
    }
    assert!(!pool.judge_due());
    assert_eq!(
        *channel.waits.borrow(),
        [Waited::Timed, Waited::Timed, Waited::Untimed]
    );
}

/// While a demand stands (an open view), the writer neither times out on its channel nor judges;
/// the demand's end starts nothing, and the judge runs in the quiet period after the writer's next
/// message.
#[test]
fn no_idle_judging_while_a_demand_stands() {
    let pool = RewindPool::new(usize::MAX);
    let mut live = Terminal::new(6, 30);
    let recorder = recording(&live, &pool);
    {
        let mut rec = recorder.lock().unwrap();
        put(&mut live, &mut rec, b"one\r\n");
        see(&live, &mut rec);
        close(&live, &mut rec);
    }
    assert!(pool.judge_due(), "fixture: a judge is due");
    let restores = Rc::new(Cell::new(0));
    let _hook = replay_hook::install({
        let restores = restores.clone();
        move || restores.set(restores.get() + 1)
    });
    let demands = Demands::new();
    let channel = Channel::default();
    let mut inbox = WriterChannel::new(channel.clone(), pool.clone(), demands.clone());
    let view = demands.acquire(Reason::View);
    channel.queued.borrow_mut().push_back(1);
    assert_eq!(inbox.wait(), Some(1));
    assert_eq!(
        *channel.waits.borrow(),
        [Waited::Untimed],
        "an open view keeps the writer off any timeout"
    );
    assert_eq!(restores.get(), 0, "and off judging");
    drop(view);
    assert_eq!(restores.get(), 0, "the view's end starts no judge");
    assert_eq!(recorder.lock().unwrap().pending(), 1);
    assert_eq!(inbox.wait(), None);
    assert_eq!(
        *channel.waits.borrow(),
        [Waited::Untimed, Waited::Timed, Waited::Untimed],
        "the wait after the message spends its quiet period judging"
    );
    assert_eq!(restores.get(), 1);
    assert_eq!(recorder.lock().unwrap().pending(), 0);
}

/// A job whose recording is forgotten, or whose base is evicted, between passes is dropped at the
/// end of the pass under way: no later pass, no verdict posted, the next generation's witness left
/// alone, and its engine's bytes reported freed to the pool.
#[test]
fn an_idle_judge_drops_a_stale_job_and_frees_its_engine() {
    for forget in [true, false] {
        let pool = RewindPool::new(usize::MAX);
        let mut live = Terminal::new(24, 80);
        let recorder = recording(&live, &pool);
        three_pass_chain(&mut live, &mut recorder.lock().unwrap());
        let costs = recorder
            .lock()
            .unwrap()
            .spine
            .costs
            .handle()
            .expect("costs kept");
        // What the job's engine holds after its second pass, the one the change lands in.
        let freed = {
            let plan = recorder
                .lock()
                .unwrap()
                .judge_plan()
                .expect("a closed chain");
            let mut judge = ChainJudge::new(plan);
            let _ = judge.step();
            let _ = judge.step();
            judge.engine_bytes()
        };
        let (mut stops, mut witnessed, mut unrelieved) = (0, 0, 0);
        IdleJudge::default().run(&pool, || {
            stops += 1;
            if stops == 2 {
                let mut rec = recorder.lock().unwrap();
                witnessed = rec.witnessed();
                if forget {
                    rec.forget();
                    close(&live, &mut rec);
                    put(&mut live, &mut rec, b"next generation\r\n");
                    see(&live, &mut rec);
                } else {
                    assert!(rec.pop_keyframe(), "the job's base is evicted");
                }
                unrelieved = pool.unrelieved.load(Ordering::Relaxed);
            }
            false
        });
        assert_eq!(witnessed, 2, "fixture: the first pass posted its verdicts");
        assert_eq!(
            costs.judge.calls(),
            Some(2),
            "forget={forget}: no pass after the one under way"
        );
        let rec = recorder.lock().unwrap();
        assert_eq!(
            rec.witnessed(),
            witnessed,
            "forget={forget}: no verdict posted"
        );
        if forget {
            assert_eq!(
                rec.pending(),
                1,
                "the next generation's witness is left alone"
            );
        } else {
            assert_eq!(
                (rec.pending(), rec.unjudged_evicted()),
                (0, 4),
                "the eviction took the chain's unjudged witnesses"
            );
        }
        assert_eq!(
            pool.unrelieved.load(Ordering::Relaxed) - unrelieved,
            freed,
            "forget={forget}: the dropped engine is reported freed"
        );
        assert!(freed > 0);
        assert!(!pool.judge_due(), "forget={forget}: nothing closed is left");
    }
}

/// A judging pass books a resize, which reflows the whole history its engine keeps, as a whole
/// pass of work, and each other step that is not a slice as a small fixed work, so a chain of
/// resizes or host steps with hardly a byte of output yields between them as a chain of slices
/// does, rather than folding the whole chain in one pass.
#[test]
fn a_judge_pass_books_resizes_and_host_steps_as_work() {
    let passes = |judge: &mut ChainJudge| {
        let mut passes = 0_usize;
        loop {
            passes += 1;
            let (_, _, more) = judge.step();
            if !more {
                return passes;
            }
            assert!(passes < 10_000, "a judge that never ends its chain");
        }
    };
    let pool = RewindPool::new(usize::MAX);
    let mut live = Terminal::new(6, 30);
    let recorder = recording(&live, &pool);
    let mut rec = recorder.lock().unwrap();
    for n in 0..40_u16 {
        put(&mut live, &mut rec, b"x");
        let cols = 30 + n % 2;
        live.resize(6, cols);
        rec.apply(TemporalMsg::Resize { rows: 6, cols });
    }
    see(&live, &mut rec);
    let plan = rec.replay_plan(None).expect("the chain replays");
    drop(rec);
    let resized = passes(&mut ChainJudge::new(plan));
    assert!(
        resized >= 40,
        "one resize a pass: {resized} passes for 40 resizes and 40 bytes"
    );

    let mut live = Terminal::new(6, 30);
    let recorder = recording(&live, &pool);
    let mut rec = recorder.lock().unwrap();
    put(&mut live, &mut rec, b"x");
    for n in 0..256 {
        rec.apply(TemporalMsg::Host(
            aterm_core::terminal::HostOp::HolderRetire(n),
            None,
        ));
    }
    see(&live, &mut rec);
    let plan = rec.replay_plan(None).expect("the chain replays");
    drop(rec);
    let hosted = passes(&mut ChainJudge::new(plan));
    assert!(
        hosted >= 4,
        "256 host steps of 1 KiB of work each take four 64 KiB passes: {hosted}"
    );
}
