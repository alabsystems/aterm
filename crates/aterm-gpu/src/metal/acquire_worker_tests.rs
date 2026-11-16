// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(60);

/// Wait, under the hang detector, until the worker has published `count` results.
#[track_caller]
fn until_published<I, O, R>(worker: &AcquireWorker<I, O, R>, count: u64) {
    aterm_spec::wait::until_within(LIMIT, "the worker to publish its result", || {
        (worker.signal.published.load(Ordering::SeqCst) >= count).then_some(())
    });
}

/// [`ends`]' three: the caller's side, then the worker's requests and its publisher.
type Ends<I, O, R> = (
    AcquireWorker<I, O, R>,
    mpsc::Receiver<Command<I, R>>,
    Publisher<O>,
);

/// A worker's two ends over channels the test holds, with no thread: the caller's side, and the
/// worker's side for the test to run a step at a time.
fn ends<I, O, R>(notify: Arc<dyn Fn() + Send + Sync>) -> Ends<I, O, R> {
    let (commands, requests) = mpsc::sync_channel(2);
    let (completed, results) = mpsc::sync_channel(1);
    let signal = Arc::new(Signal::default());
    let publisher = Publisher {
        completed,
        signal: Arc::clone(&signal),
        notify,
    };
    let worker = AcquireWorker {
        commands: Some(commands),
        results: Some(results),
        pending_generation: None,
        closed: false,
        signal,
        taken: 0,
    };
    (worker, requests, publisher)
}

/// The worker's side of a worker that has no thread, for the surface's tests: each queued
/// request runs where the test says.
pub(in crate::metal) struct WorkerSide<I, O, R> {
    requests: mpsc::Receiver<Command<I, R>>,
    publisher: Publisher<O>,
}

/// A worker with no thread, and its other side.
pub(in crate::metal) fn stepped<I, O, R>(
    notify: Arc<dyn Fn() + Send + Sync>,
) -> (AcquireWorker<I, O, R>, WorkerSide<I, O, R>) {
    let (worker, requests, publisher) = ends(notify);
    (
        worker,
        WorkerSide {
            requests,
            publisher,
        },
    )
}

impl<I, O, R> WorkerSide<I, O, R> {
    /// The worker's whole turn for the next queued request, in its order: run `acquire`,
    /// publish the result, then post the wake where a frame waits. False, with nothing run,
    /// where no request is queued.
    pub(in crate::metal) fn run_next(&self, acquire: impl FnOnce(I) -> O) -> bool {
        let Ok(Command::Request {
            generation, input, ..
        }) = self.requests.try_recv()
        else {
            return false;
        };
        assert!(
            self.publisher.publish(Completed {
                generation,
                queue_ns: 0,
                outcome: AcquireOutcome::Ready(acquire(input)),
            }),
            "the caller's side is open and holds no unclaimed result"
        );
        self.publisher.wake_if_wanted();
        true
    }
}

struct Retirement {
    acquiring: Arc<AtomicBool>,
    done: mpsc::Sender<bool>,
}

impl Drop for Retirement {
    fn drop(&mut self) {
        let _ = self.done.send(self.acquiring.load(Ordering::SeqCst));
    }
}

#[test]
fn blocked_acquire_keeps_request_read_and_close_nonblocking_and_retires_afterward() {
    let model = aterm_models_render::metal_drawable_acquire_model();
    let mut state = model.init_state();
    let (entered, entering) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (retired, retirement) = mpsc::channel();
    let acquiring = Arc::new(AtomicBool::new(false));
    let in_worker = Arc::clone(&acquiring);
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake_count = Arc::clone(&wakes);
    let mut worker = AcquireWorker::spawn(
        move |input: usize| {
            in_worker.store(true, Ordering::SeqCst);
            entered.send(()).unwrap();
            released.recv_timeout(LIMIT).unwrap();
            in_worker.store(false, Ordering::SeqCst);
            input
        },
        Arc::new(move || {
            wake_count.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();
    worker.try_request(7, 41).unwrap();
    assert!(model.fire("Request", &mut state));
    entering.recv_timeout(LIMIT).unwrap();
    for input in 0..256 {
        assert_eq!(worker.try_request(8, input), Err(input));
        assert!(worker.try_take().is_none());
        assert!(model.fire("MainProgress", &mut state));
    }
    assert!(
        worker
            .close(Retirement {
                acquiring,
                done: retired
            })
            .is_ok()
    );
    assert!(model.fire("Close", &mut state));
    assert!(!model.action_enabled("Retire", &state));
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut premature = state.clone();
    assert!(buggy.fire("Retire", &mut premature));
    assert!(!buggy.check_invariant("RetirementFollowsAcquisition", &premature));
    assert!(matches!(
        retirement.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    assert!(!retirement.recv_timeout(LIMIT).unwrap());
    assert!(model.fire("Publish", &mut state));
    assert!(
        !model.action_enabled("Swap", &state),
        "a result nobody can take brings no wake"
    );
    assert!(model.fire("Retire", &mut state));
    assert!(model.check_invariant("RetirementFollowsAcquisition", &state));
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
}

#[test]
fn close_queues_retirement_behind_a_request_that_has_not_started() {
    let (start, started) = mpsc::channel();
    let (entered, entering) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (retired, retirement) = mpsc::channel();
    let acquiring = Arc::new(AtomicBool::new(false));
    let in_worker = Arc::clone(&acquiring);
    let (mut worker, requests, publisher) = ends::<(), (), Retirement>(Arc::new(|| {
        panic!("a closed receiver must not publish a wake")
    }));
    // A frame waited as the surface closed: the wake it asked for has nobody to reach.
    worker.signal.wanted.store(true, Ordering::SeqCst);
    let thread = std::thread::spawn(move || {
        started.recv_timeout(LIMIT).unwrap();
        run_worker(
            move |()| {
                in_worker.store(true, Ordering::SeqCst);
                entered.send(()).unwrap();
                released.recv_timeout(LIMIT).unwrap();
                in_worker.store(false, Ordering::SeqCst);
            },
            requests,
            &publisher,
        );
    });
    worker.try_request(0, ()).unwrap();
    assert!(
        worker
            .close(Retirement {
                acquiring,
                done: retired
            })
            .is_ok()
    );
    assert!(matches!(
        retirement.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    start.send(()).unwrap();
    entering.recv_timeout(LIMIT).unwrap();
    assert!(matches!(
        retirement.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    release.send(()).unwrap();
    assert!(!retirement.recv_timeout(LIMIT).unwrap());
    thread.join().unwrap();
}

/// Signals, when the worker drops it, that every request queued before the close is behind it.
struct Retired(mpsc::Sender<()>);

impl Drop for Retired {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[test]
fn real_worker_admission_publication_and_stale_result_match_the_model() {
    let model = aterm_models_render::metal_drawable_acquire_model();
    let mut state = model.init_state();
    let (entered, entering) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake_count = Arc::clone(&wakes);
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_worker = Arc::clone(&calls);
    let mut worker = AcquireWorker::<usize, usize, Retired>::spawn(
        move |input| {
            calls_in_worker.fetch_add(1, Ordering::SeqCst);
            entered.send(()).unwrap();
            released.recv_timeout(LIMIT).unwrap();
            input + 1
        },
        Arc::new(move || {
            wake_count.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();
    assert_eq!(
        worker.try_request(0, 10).is_ok(),
        model.action_enabled("Request", &state)
    );
    assert!(model.fire("Request", &mut state));
    entering.recv_timeout(LIMIT).unwrap();
    assert_eq!(state["pending"], i64::from(worker.is_pending()));
    assert_eq!(
        worker.try_request(0, 11).is_ok(),
        model.action_enabled("Request", &state)
    );
    assert!(model.fire("MainProgress", &mut state));
    assert!(worker.try_take().is_none());
    assert!(model.fire("Invalidate", &mut state));
    release.send(()).unwrap();
    until_published(&worker, 1);
    assert!(model.fire("Publish", &mut state));
    assert_eq!(
        worker.try_request(1, 12).is_ok(),
        model.action_enabled("Request", &state)
    );
    let result = worker.try_take().unwrap();
    assert!(matches!(result.outcome, AcquireOutcome::Ready(11)));
    assert!(model.fire("Take", &mut state));
    assert_eq!(state["pending"], i64::from(worker.is_pending()));
    assert_eq!(
        result_is_current(result.generation, 1, false),
        model.action_enabled("AcceptResult", &state)
    );
    assert!(!result_is_current(result.generation, 1, false));
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    assert!(buggy.action_enabled("AcceptResult", &state));
    assert!(!model.action_enabled("AcceptResult", &state));
    let mut bad = state.clone();
    assert!(buggy.fire("AcceptResult", &mut bad));
    assert!(!buggy.check_invariant("NoInvalidAcceptance", &bad));

    // The second request is dequeued only once the worker is past the first result's wake
    // decision: it is serial.
    assert!(worker.try_request(1, 20).is_ok());
    entering.recv_timeout(LIMIT).unwrap();
    assert!(model.fire("Swap", &mut state));
    assert!(model.fire("Request", &mut state));
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        0,
        "nobody waited for the first"
    );
    release.send(()).unwrap();
    until_published(&worker, 2);
    assert!(model.fire("Publish", &mut state));
    let current = worker.try_take().unwrap();
    assert!(matches!(current.outcome, AcquireOutcome::Ready(21)));
    assert!(model.fire("Take", &mut state));
    assert!(result_is_current(current.generation, 1, false));
    assert!(model.fire("AcceptResult", &mut state));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(state["published"], 2);
    assert!(worker.try_take().is_none());
    assert!(matches!(
        entering.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let (retired, retirement) = mpsc::channel();
    assert!(worker.close(Retired(retired)).is_ok());
    retirement.recv_timeout(LIMIT).unwrap();
    assert!(model.fire("Swap", &mut state));
    assert!(model.fire("Close", &mut state));
    assert_eq!(state["wakes"], 0);
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        0,
        "two results taken by frames that never waited bring no wake"
    );
}

#[test]
fn production_guards_match_all_bounded_model_decisions_and_reject_mutants() {
    let model = aterm_models_render::metal_drawable_acquire_model();
    let mut rejected = 0;
    for closed in [false, true] {
        for pending in [false, true] {
            let mut state = model.init_state();
            state.insert("closed", i64::from(closed));
            state.insert("pending", i64::from(pending));
            assert_eq!(
                request_allowed(pending, closed),
                model.action_enabled("Request", &state)
            );
        }
        for request in 0..=2 {
            for current in 0..=2 {
                let mut state = model.init_state();
                state.insert("closed", i64::from(closed));
                state.insert("request_epoch", request);
                state.insert("epoch", current);
                let actual = result_is_current(
                    u64::try_from(request).unwrap(),
                    u64::try_from(current).unwrap(),
                    closed,
                );
                assert_eq!(actual, model.action_enabled("AcceptResult", &state));
                if !actual {
                    assert!(
                        aterm_spec::interp::with_buggy(&model, 1)
                            .action_enabled("AcceptResult", &state)
                    );
                    rejected += 1;
                }
            }
        }
    }
    assert_eq!(
        rejected, 15,
        "the closed/stale negative control must be live"
    );
}

#[test]
fn acquire_panic_publishes_terminal_failure_and_closes_admission() {
    let mut worker = AcquireWorker::<(), (), ()>::spawn(
        |()| panic!("injected acquisition unwind"),
        Arc::new(|| {}),
    )
    .unwrap();
    worker.try_request(3, ()).unwrap();
    until_published(&worker, 1);
    let result = worker.try_take().unwrap();
    assert_eq!(result.generation, 3);
    assert!(matches!(result.outcome, AcquireOutcome::Panicked));
    assert!(!worker.is_pending());
    assert_eq!(worker.try_request(4, ()), Err(()));
    assert!(worker.try_take().is_none());
    assert!(worker.close(()).is_ok());
}

#[test]
fn disconnected_producer_is_terminal_instead_of_permanently_pending() {
    let (mut worker, _requests, publisher) = ends::<(), (), ()>(Arc::new(|| {}));
    drop(publisher);
    worker.pending_generation = Some(9);
    let result: Completed<()> = worker.try_take().unwrap();
    assert!(matches!(result.outcome, AcquireOutcome::Disconnected));
    assert_eq!(result.generation, 9);
    assert!(!worker.is_pending());
    assert_eq!(worker.try_request(10, ()), Err(()));
    assert!(worker.try_take().is_none());
}

// SAFETY: `qos_class_self` is in libpthread (`libSystem`) and reads only the calling thread's
// class.
unsafe extern "C" {
    fn qos_class_self() -> u32;
}

const USER_INTERACTIVE_RAW: u32 = 0x21;

#[test]
fn acquisition_runs_at_user_interactive_so_the_waiting_ui_thread_is_not_outranked() {
    let mut worker = AcquireWorker::<(), u32, ()>::spawn(
        // SAFETY: see the extern block above; no arguments, no shared state.
        |()| unsafe { qos_class_self() },
        Arc::new(|| {}),
    )
    .unwrap();
    worker.try_request(1, ()).unwrap();
    until_published(&worker, 1);
    let result = worker.try_take().unwrap();
    let AcquireOutcome::Ready(class) = result.outcome else {
        panic!("acquisition did not complete: {:?}", result.outcome);
    };
    assert_eq!(QOS_CLASS_USER_INTERACTIVE, USER_INTERACTIVE_RAW);
    assert_eq!(
        class, USER_INTERACTIVE_RAW,
        "aterm-drawable-acquire ran at QoS class {class:#x}, not USER_INTERACTIVE"
    );
    assert!(worker.close(()).is_ok());
}

/// A gated acquisition on the real worker thread: `entering` hears each acquisition start, and
/// it returns once `release` is sent to. `wakes` counts the wakes posted, `woken` hears each.
struct Gated {
    worker: AcquireWorker<u64, u64, Retired>,
    entering: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    wakes: Arc<AtomicUsize>,
    woken: mpsc::Receiver<()>,
}

fn gated() -> Gated {
    let (entered, entering) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (wake, woken) = mpsc::channel();
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake_count = Arc::clone(&wakes);
    let worker = AcquireWorker::spawn(
        move |input: u64| {
            entered.send(()).unwrap();
            released.recv_timeout(LIMIT).unwrap();
            input
        },
        Arc::new(move || {
            wake_count.fetch_add(1, Ordering::SeqCst);
            wake.send(()).unwrap();
        }),
    )
    .unwrap();
    Gated {
        worker,
        entering,
        release,
        wakes,
        woken,
    }
}

/// The count law of the wake: on the real worker thread, results taken by frames that never
/// waited bring no wake, and a frame that waits is woken exactly once. Each count is read once
/// the next acquisition has started: the worker is serial, so it is past the earlier result's
/// wake decision.
#[test]
fn results_nobody_waited_for_bring_no_wake_and_a_waiting_frame_exactly_one() {
    const UNWAITED: u64 = 5;
    let mut g = gated();
    for n in 0..UNWAITED {
        g.worker.try_request(0, n).unwrap();
        g.entering.recv_timeout(LIMIT).unwrap();
        g.release.send(()).unwrap();
        until_published(&g.worker, n + 1);
        let taken = g.worker.try_take().unwrap();
        assert!(matches!(taken.outcome, AcquireOutcome::Ready(tag) if tag == n));
    }
    g.worker.try_request(0, UNWAITED).unwrap();
    g.entering.recv_timeout(LIMIT).unwrap();
    assert_eq!(
        g.wakes.load(Ordering::SeqCst),
        0,
        "{UNWAITED} results were taken and no frame waited for one"
    );

    // A frame finds no drawable while that acquisition is in flight: it waits.
    let awaited = g
        .worker
        .await_result(0, || unreachable!("a request is in flight"));
    assert!(
        matches!(awaited, Awaited::Waiting { stale: None }),
        "{awaited:?}"
    );
    assert_eq!(
        g.wakes.load(Ordering::SeqCst),
        0,
        "nothing is published yet"
    );
    g.release.send(()).unwrap();
    g.woken.recv_timeout(LIMIT).unwrap();
    let taken = g.worker.try_take().expect("the wake follows the result");
    assert!(matches!(taken.outcome, AcquireOutcome::Ready(UNWAITED)));
    g.worker.try_request(0, UNWAITED + 1).unwrap();
    g.entering.recv_timeout(LIMIT).unwrap();
    assert_eq!(g.wakes.load(Ordering::SeqCst), 1, "one wait, one wake");

    g.release.send(()).unwrap();
    let (retired, retirement) = mpsc::channel();
    assert!(g.worker.close(Retired(retired)).is_ok());
    retirement.recv_timeout(LIMIT).unwrap();
    assert_eq!(
        g.wakes.load(Ordering::SeqCst),
        1,
        "a closed surface is not woken"
    );
}

/// The worker's order, on its own loop: the wake is posted with the result already published,
/// so the frame it wakes finds the result. The wake itself reads how many results were
/// published as it was posted.
#[test]
fn the_worker_posts_the_wake_after_it_publishes_the_result() {
    let shared: Arc<std::sync::OnceLock<Arc<Signal>>> = Arc::default();
    let at_wake = Arc::clone(&shared);
    let (seen, published_at_wake) = mpsc::channel();
    let (mut worker, requests, publisher) = ends::<u64, u64, ()>(Arc::new(move || {
        let signal = at_wake.get().expect("set before the worker runs");
        seen.send(signal.published.load(Ordering::SeqCst)).unwrap();
    }));
    assert!(shared.set(Arc::clone(&worker.signal)).is_ok());
    worker.try_request(0, 7).unwrap();
    let awaited = worker.await_result(0, || unreachable!("a request is in flight"));
    assert!(
        matches!(awaited, Awaited::Waiting { stale: None }),
        "{awaited:?}"
    );
    let thread = std::thread::spawn(move || run_worker(|input| input, requests, &publisher));
    assert_eq!(
        published_at_wake.recv_timeout(LIMIT).unwrap(),
        1,
        "the result was published when its wake was posted"
    );
    assert!(matches!(
        worker.try_take().map(|taken| taken.outcome),
        Some(AcquireOutcome::Ready(7))
    ));
    assert!(worker.close(()).is_ok());
    thread.join().unwrap();
}

fn counted() -> (Arc<AtomicUsize>, Arc<dyn Fn() + Send + Sync>) {
    let wakes = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&wakes);
    (
        wakes,
        Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }),
    )
}

/// The worker's side of the next queued request, run on the test's thread: publish its result.
fn publish_next(requests: &mpsc::Receiver<Command<u64, ()>>, publisher: &Publisher<u64>) -> bool {
    let Ok(Command::Request {
        generation, input, ..
    }) = requests.try_recv()
    else {
        panic!("no request is queued for the worker");
    };
    publisher.publish(Completed {
        generation,
        queue_ns: 0,
        outcome: AcquireOutcome::Ready(input),
    })
}

/// A result published, and its wake decision made, before the frame asks: the frame gets the
/// result, waits for nothing and is owed no wake.
#[test]
fn a_frame_that_finds_the_result_already_published_takes_it_and_is_not_woken() {
    let (wakes, notify) = counted();
    let (mut worker, requests, publisher) = ends::<u64, u64, ()>(notify);
    worker.try_request(4, 40).unwrap();
    assert!(publish_next(&requests, &publisher));
    publisher.wake_if_wanted();
    let awaited = worker.await_result(4, || unreachable!("the result is there"));
    assert!(
        matches!(
            &awaited,
            Awaited::Result(Completed {
                generation: 4,
                outcome: AcquireOutcome::Ready(40),
                ..
            })
        ),
        "{awaited:?}"
    );
    assert!(!worker.is_pending());
    assert!(!worker.signal.wanted.load(Ordering::SeqCst));
    publisher.wake_if_wanted();
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
}

/// The stranded wait a resize could leave: the request in flight completes for an earlier
/// generation, and its wake decision is made, before the frame asks. The frame takes that
/// result, cannot use it, and queues its own request, whose completion wakes it.
#[test]
fn a_frame_that_takes_a_stale_result_queues_its_own_request_and_is_woken_for_it() {
    let (wakes, notify) = counted();
    let (mut worker, requests, publisher) = ends::<u64, u64, ()>(notify);
    worker.try_request(0, 10).unwrap();
    assert!(publish_next(&requests, &publisher));
    publisher.wake_if_wanted();
    assert_eq!(wakes.load(Ordering::SeqCst), 0, "nobody waited for it");

    let awaited = worker.await_result(1, || Some(11));
    let Awaited::Waiting { stale: Some(stale) } = awaited else {
        panic!("the frame waits, holding the stale result it took: {awaited:?}");
    };
    assert_eq!(stale.generation, 0);
    assert!(
        worker.is_pending(),
        "a frame that waits has a request in flight"
    );
    assert!(worker.signal.wanted.load(Ordering::SeqCst));
    assert!(publish_next(&requests, &publisher));
    publisher.wake_if_wanted();
    assert_eq!(wakes.load(Ordering::SeqCst), 1, "its own request wakes it");

    let awaited = worker.await_result(1, || unreachable!("the result is there"));
    assert!(
        matches!(
            &awaited,
            Awaited::Result(Completed {
                generation: 1,
                outcome: AcquireOutcome::Ready(11),
                ..
            })
        ),
        "{awaited:?}"
    );
    publisher.wake_if_wanted();
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
}

/// A frame that can neither take a result nor queue a request is refused, and leaves no wake
/// owed.
#[test]
fn a_frame_with_nothing_in_flight_and_no_admission_is_refused() {
    let (wakes, notify) = counted();
    let (mut worker, requests, publisher) = ends::<u64, u64, ()>(notify);
    drop(requests);
    let awaited = worker.await_result(0, || Some(1));
    assert!(
        matches!(awaited, Awaited::Refused { stale: None }),
        "{awaited:?}"
    );
    assert!(!worker.signal.wanted.load(Ordering::SeqCst));
    publisher.wake_if_wanted();
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
}

/// The reshape a resize owes: the request in flight completes for the generation before it, and
/// its wake decision is made, before the frame asks. The frame's layer still has the earlier
/// configuration, so its caller admits no request: the frame takes the stale result, queues
/// nothing against that layer, and is told that no wake is coming. Once the layer is reshaped
/// the caller admits the request, and its completion wakes the frame.
#[test]
fn a_frame_whose_caller_admits_no_request_queues_nothing_and_is_owed_no_wake() {
    let (wakes, notify) = counted();
    let (mut worker, requests, publisher) = ends::<u64, u64, ()>(notify);
    worker.try_request(0, 10).unwrap();
    assert!(publish_next(&requests, &publisher));
    publisher.wake_if_wanted();

    let awaited = worker.await_result(1, || None);
    let Awaited::Unqueued { stale: Some(stale) } = awaited else {
        panic!("the frame is told nothing was queued, holding the stale result: {awaited:?}");
    };
    assert_eq!(stale.generation, 0);
    assert!(
        !worker.is_pending(),
        "a frame whose caller admits no request has none in flight"
    );
    assert!(
        matches!(requests.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "nothing reached the worker"
    );
    assert!(
        !worker.signal.wanted.load(Ordering::SeqCst),
        "no wake is asked for where none can come"
    );
    publisher.wake_if_wanted();
    assert_eq!(wakes.load(Ordering::SeqCst), 0);

    // A request in flight is waited for whatever the caller admits: it is not asked.
    worker.try_request(1, 11).unwrap();
    let awaited = worker.await_result(2, || unreachable!("a request is in flight"));
    assert!(
        matches!(awaited, Awaited::Waiting { stale: None }),
        "{awaited:?}"
    );
    assert!(publish_next(&requests, &publisher));
    publisher.wake_if_wanted();
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "the request in flight wakes it"
    );

    // The layer is reshaped: the caller admits the frame's own request.
    let awaited = worker.await_result(2, || Some(12));
    assert!(
        matches!(awaited, Awaited::Waiting { stale: Some(_) }),
        "{awaited:?}"
    );
    assert!(publish_next(&requests, &publisher));
    publisher.wake_if_wanted();
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
}

/// What one run of the production worker shows, against the model's state after the same
/// actions.
#[derive(Debug, PartialEq, Eq)]
struct Shown {
    wanted: i64,
    wakes: i64,
    posted: i64,
    /// `(pending, ready, waiting)`, between a frame's steps on an open surface.
    at_rest: Option<(i64, i64, i64)>,
}

fn shown_by_model(state: &aterm_spec::interp::State) -> Shown {
    Shown {
        wanted: state["wanted"],
        wakes: state["wakes"],
        posted: state["posted"],
        at_rest: (state["closed"] == 0 && state["step"] == 0)
            .then(|| (state["pending"], state["ready"], state["waiting"])),
    }
}

/// Run `path`, a sequence of the model's actions, on the production worker: the caller's side
/// through [`AcquireWorker`], the worker's side through [`Publisher`] on this thread, in the
/// path's order. A waiting frame is one `await_result_with` call, and the worker's actions the
/// path puts between its steps run at its [`WaitStep`]s.
fn run_on_production(path: &[&'static str]) -> Shown {
    fn worker_side(
        action: &str,
        requests: &mpsc::Receiver<Command<u64, ()>>,
        publisher: &Publisher<u64>,
        closed: bool,
    ) -> bool {
        match action {
            "Publish" => assert_eq!(
                publish_next(requests, publisher),
                !closed,
                "a result reaches an open surface and only that"
            ),
            "Swap" => publisher.wake_if_wanted(),
            "Retire" => assert!(matches!(requests.try_recv(), Ok(Command::Retire(())))),
            // The caller's thread goes on, and an accepted result is the guard's own law.
            "MainProgress" | "AcceptResult" => {}
            _ => return false,
        }
        true
    }

    let (wakes, notify) = counted();
    let (worker, requests, publisher) = ends::<u64, u64, ()>(notify);
    let signal = Arc::clone(&worker.signal);
    let mut worker = Some(worker);
    let (mut epoch, mut handled, mut waiting) = (0u64, 0usize, false);
    // Whether the layer has the wanted configuration: what the surface admits a request on.
    let mut shaped = true;
    // Where the path ends inside a frame's step: the flag and the wakes as they stood there.
    let mut cut: Option<(bool, usize)> = None;
    let mut at = 0;
    while let Some(&action) = path.get(at) {
        at += 1;
        if worker_side(action, &requests, &publisher, worker.is_none()) {
            continue;
        }
        match action {
            "Request" => {
                assert!(shaped, "a request is queued only on a shaped layer");
                worker
                    .as_mut()
                    .expect("an open surface")
                    .try_request(epoch, epoch)
                    .expect("the model admits this request");
            }
            "Take" => {
                let taken = worker.as_mut().expect("an open surface").try_take();
                assert!(taken.is_some(), "the model has a result ready");
            }
            "Invalidate" => epoch += 1,
            "Reshape" => {
                epoch += 1;
                shaped = false;
            }
            "Revert" => {
                epoch += 1;
                shaped = true;
            }
            "Reconfigure" => {
                assert!(
                    !worker.as_ref().expect("an open surface").is_pending(),
                    "the layer is reshaped with nothing outstanding"
                );
                shaped = true;
            }
            "HandleWake" => {
                handled += 1;
                waiting = false;
            }
            "Close" => {
                assert!(worker.take().expect("an open surface").close(()).is_ok());
                waiting = false;
            }
            "Want" => {
                let awaited = worker.as_mut().expect("an open surface").await_result_with(
                    epoch,
                    || shaped.then_some(epoch),
                    |step| {
                        let next = match step {
                            WaitStep::Wanted => "Recollect",
                            WaitStep::Recollected => "Settle",
                        };
                        while cut.is_none() {
                            match path.get(at) {
                                None => {
                                    cut = Some((
                                        signal.wanted.load(Ordering::SeqCst),
                                        wakes.load(Ordering::SeqCst),
                                    ));
                                }
                                Some(&action) => {
                                    at += 1;
                                    if !worker_side(action, &requests, &publisher, false) {
                                        assert_eq!(
                                            action, next,
                                            "only the worker moves inside a frame's step"
                                        );
                                        return;
                                    }
                                }
                            }
                        }
                    },
                );
                if cut.is_some() {
                    break;
                }
                waiting = match awaited {
                    Awaited::Result(_) | Awaited::Unqueued { .. } => false,
                    Awaited::Waiting { .. } => true,
                    Awaited::Refused { .. } => panic!("the model admits the frame's request"),
                };
            }
            other => panic!("{other} outside a frame's step"),
        }
    }
    let (wanted, wakes) = cut.unwrap_or_else(|| {
        (
            signal.wanted.load(Ordering::SeqCst),
            wakes.load(Ordering::SeqCst),
        )
    });
    Shown {
        wanted: i64::from(wanted),
        wakes: i64::try_from(wakes).unwrap(),
        posted: i64::try_from(wakes - handled).unwrap(),
        at_rest: worker.as_ref().filter(|_| cut.is_none()).map(|worker| {
            (
                i64::from(worker.is_pending()),
                i64::try_from(signal.published.load(Ordering::SeqCst) - worker.taken).unwrap(),
                i64::from(waiting),
            )
        }),
    }
}

/// The worker and the waiting frame, driven together: every action of every state the model
/// reaches is run on the production worker, the worker's steps falling between the frame's
/// exactly where the model puts them, and after each one production shows what the model says:
/// the flag, the wakes posted and still on their way, the request outstanding, the result ready
/// and the wait. The model's laws hold in each of those states (`NoStrandedWait`,
/// `WakeOnlyWhenWanted`), so they hold of the worker.
#[test]
fn the_worker_and_a_waiting_frame_refine_the_model_over_every_interleaving() {
    let model = aterm_models_render::metal_drawable_acquire_model();
    let start = model.init_state();
    let mut seen = std::collections::BTreeSet::from([start.clone()]);
    let mut frontier = std::collections::VecDeque::from([(start, Vec::new())]);
    let (mut steps, mut waits) = (0usize, 0usize);
    while let Some((state, path)) = frontier.pop_front() {
        for law in &model.invariants {
            assert!(
                model.check_invariant(law.name, &state),
                "{} is broken after {path:?}",
                law.name
            );
        }
        for action in &model.actions {
            let mut next = state.clone();
            if !model.fire(action.name, &mut next) {
                continue;
            }
            let mut path = path.clone();
            path.push(action.name);
            assert_eq!(
                run_on_production(&path),
                shown_by_model(&next),
                "after {path:?}"
            );
            steps += 1;
            waits += usize::from(action.name == "Settle");
            if seen.insert(next.clone()) {
                frontier.push_back((next, path));
            }
        }
    }
    eprintln!(
        "MetalDrawableAcquire: {} states, {steps} steps run on the production worker, {waits} of \
         them a frame left waiting",
        seen.len()
    );
    assert!(waits > 0 && seen.len() > 1000, "the walk covered the model");
}
