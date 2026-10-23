// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One demand-driven drawable acquisition, with no caller-side wait or join.

use std::sync::{Arc, mpsc};

/// `pending` covers a published result the caller has not taken yet.
#[inline]
pub(crate) const fn request_allowed(pending: bool, closed: bool) -> bool {
    !pending && !closed
}

/// The caller must additionally own a completed result.
#[inline]
pub(crate) const fn result_is_current(
    request_generation: u64,
    current_generation: u64,
    closed: bool,
) -> bool {
    !closed && request_generation == current_generation
}

#[derive(Debug)]
pub(crate) enum AcquireOutcome<O> {
    Ready(O),
    Panicked,
    Disconnected,
}

#[derive(Debug)]
pub(crate) struct Completed<O> {
    pub(crate) generation: u64,
    /// Time this request spent queued, from `try_request` until the worker dequeued it.
    pub(crate) queue_ns: u64,
    pub(crate) outcome: AcquireOutcome<O>,
}

enum Command<I, R> {
    Request {
        generation: u64,
        queued: aterm_time::Instant,
        input: I,
    },
    Retire(R),
}

pub(crate) struct AcquireWorker<I, O, R> {
    commands: Option<mpsc::SyncSender<Command<I, R>>>,
    results: Option<mpsc::Receiver<Completed<O>>>,
    pending_generation: Option<u64>,
    closed: bool,
}

impl<I, O, R> std::fmt::Debug for AcquireWorker<I, O, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcquireWorker")
            .field("pending_generation", &self.pending_generation)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl<I: Send + 'static, O: Send + 'static, R: Send + 'static> AcquireWorker<I, O, R> {
    /// `notify` must be a short, nonblocking event publication.
    pub(crate) fn spawn(
        acquire: impl FnMut(I) -> O + Send + 'static,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Self> {
        let (commands, requests) = mpsc::sync_channel(2);
        let (completed, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("aterm-drawable-acquire".to_owned())
            .spawn(move || run_worker(acquire, notify, requests, completed))?;
        Ok(Self {
            commands: Some(commands),
            results: Some(results),
            pending_generation: None,
            closed: false,
        })
    }

    #[inline]
    pub(crate) const fn is_pending(&self) -> bool {
        self.pending_generation.is_some()
    }

    /// Return the input unchanged on busy/closed/disconnected admission.
    pub(crate) fn try_request(&mut self, generation: u64, input: I) -> Result<(), I> {
        if !request_allowed(self.is_pending(), self.closed) {
            return Err(input);
        }
        let Some(commands) = &self.commands else {
            return Err(input);
        };
        match commands.try_send(Command::Request {
            generation,
            queued: aterm_time::Instant::now(),
            input,
        }) {
            Ok(()) => {
                self.pending_generation = Some(generation);
                Ok(())
            }
            Err(mpsc::TrySendError::Full(Command::Request { input, .. })) => Err(input),
            Err(mpsc::TrySendError::Disconnected(Command::Request { input, .. })) => {
                self.closed = true;
                Err(input)
            }
            Err(_) => unreachable!("try_request only sends Request"),
        }
    }

    /// Exactly one terminal result per accepted request.
    pub(crate) fn try_take(&mut self) -> Option<Completed<O>> {
        let generation = self.pending_generation?;
        let result = self.results.as_ref()?.try_recv();
        let completed = match result {
            Ok(completed) => completed,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Completed {
                generation,
                queue_ns: 0,
                outcome: AcquireOutcome::Disconnected,
            },
        };
        self.pending_generation = None;
        if !matches!(&completed.outcome, AcquireOutcome::Ready(_)) {
            self.closed = true;
        }
        Some(completed)
    }

    /// Close once, discard any unclaimed result, and retire after acquisition, without joining or
    /// blocking.
    pub(crate) fn close(mut self, retirement: R) -> Result<(), R> {
        self.closed = true;
        drop(self.results.take());
        let Some(commands) = self.commands.take() else {
            return Err(retirement);
        };
        match commands.try_send(Command::Retire(retirement)) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(Command::Retire(payload)))
            | Err(mpsc::TrySendError::Disconnected(Command::Retire(payload))) => Err(payload),
            Err(_) => unreachable!("close only sends Retire"),
        }
    }
}

// SAFETY: `pthread_set_qos_class_self_np` is in libpthread, part of `libSystem`, which every
// process links; `qos_class_t` is `unsigned int`.
unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;

fn declare_interactive_thread() {
    // SAFETY: sets this thread's own QoS class; takes no pointers and mutates no shared state.
    unsafe {
        pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

fn run_worker<I, O, R>(
    mut acquire: impl FnMut(I) -> O,
    notify: Arc<dyn Fn() + Send + Sync>,
    requests: mpsc::Receiver<Command<I, R>>,
    completed: mpsc::SyncSender<Completed<O>>,
) {
    declare_interactive_thread();
    let mut poisoned = false;
    while let Ok(command) = requests.recv() {
        match command {
            Command::Request {
                generation,
                queued,
                input,
            } => {
                let queue_ns = u64::try_from(queued.elapsed().as_nanos()).unwrap_or(u64::MAX);
                let outcome = if poisoned {
                    drop(input);
                    AcquireOutcome::Panicked
                } else {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| acquire(input)))
                    {
                        Ok(output) => AcquireOutcome::Ready(output),
                        Err(_) => {
                            poisoned = true;
                            AcquireOutcome::Panicked
                        }
                    }
                };
                if completed
                    .try_send(Completed {
                        generation,
                        queue_ns,
                        outcome,
                    })
                    .is_ok()
                {
                    notify();
                }
            }
            Command::Retire(payload) => {
                drop(payload);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    const LIMIT: Duration = Duration::from_secs(60);

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
        assert!(model.fire("Complete", &mut state));
        assert!(model.fire("Retire", &mut state));
        assert!(model.check_invariant("RetirementFollowsAcquisition", &state));
        assert_eq!(wakes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn close_queues_retirement_behind_a_request_that_has_not_started() {
        let (commands, requests) = mpsc::sync_channel(2);
        let (completed, results) = mpsc::sync_channel(1);
        let (start, started) = mpsc::channel();
        let (entered, entering) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (retired, retirement) = mpsc::channel();
        let acquiring = Arc::new(AtomicBool::new(false));
        let in_worker = Arc::clone(&acquiring);
        let mut worker = AcquireWorker {
            commands: Some(commands),
            results: Some(results),
            pending_generation: None,
            closed: false,
        };
        let thread = std::thread::spawn(move || {
            started.recv_timeout(LIMIT).unwrap();
            run_worker(
                move |()| {
                    in_worker.store(true, Ordering::SeqCst);
                    entered.send(()).unwrap();
                    released.recv_timeout(LIMIT).unwrap();
                    in_worker.store(false, Ordering::SeqCst);
                },
                Arc::new(|| panic!("a closed receiver must not publish a wake")),
                requests,
                completed,
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

    #[test]
    fn real_worker_admission_publication_and_stale_result_match_the_model() {
        let model = aterm_models_render::metal_drawable_acquire_model();
        let mut state = model.init_state();
        let (entered, entering) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (wake, woken) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in_worker = Arc::clone(&calls);
        let mut worker = AcquireWorker::<usize, usize, ()>::spawn(
            move |input| {
                calls_in_worker.fetch_add(1, Ordering::SeqCst);
                entered.send(()).unwrap();
                released.recv_timeout(LIMIT).unwrap();
                input + 1
            },
            Arc::new(move || wake.send(()).unwrap()),
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
        woken.recv_timeout(LIMIT).unwrap();
        assert!(model.fire("Complete", &mut state));
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

        assert!(worker.try_request(1, 20).is_ok());
        assert!(model.fire("Request", &mut state));
        entering.recv_timeout(LIMIT).unwrap();
        release.send(()).unwrap();
        woken.recv_timeout(LIMIT).unwrap();
        assert!(model.fire("Complete", &mut state));
        let current = worker.try_take().unwrap();
        assert!(matches!(current.outcome, AcquireOutcome::Ready(21)));
        assert!(model.fire("Take", &mut state));
        assert!(result_is_current(current.generation, 1, false));
        assert!(model.fire("AcceptResult", &mut state));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(state["published"], 2);
        assert_eq!(state["wakes"], 2);
        assert!(worker.try_take().is_none());
        assert!(matches!(
            entering.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(matches!(woken.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert!(worker.close(()).is_ok());
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
        let (wake, woken) = mpsc::channel();
        let mut worker = AcquireWorker::<(), (), ()>::spawn(
            |()| panic!("injected acquisition unwind"),
            Arc::new(move || wake.send(()).unwrap()),
        )
        .unwrap();
        worker.try_request(3, ()).unwrap();
        woken.recv_timeout(LIMIT).unwrap();
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
        let (commands, _requests) = mpsc::sync_channel::<Command<(), ()>>(2);
        let (producer, results) = mpsc::sync_channel(1);
        drop(producer);
        let mut worker = AcquireWorker {
            commands: Some(commands),
            results: Some(results),
            pending_generation: Some(9),
            closed: false,
        };
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
        let (wake, woken) = mpsc::channel();
        let mut worker = AcquireWorker::<(), u32, ()>::spawn(
            // SAFETY: see the extern block above; no arguments, no shared state.
            |()| unsafe { qos_class_self() },
            Arc::new(move || wake.send(()).unwrap()),
        )
        .unwrap();
        worker.try_request(1, ()).unwrap();
        woken.recv_timeout(LIMIT).unwrap();
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
}
