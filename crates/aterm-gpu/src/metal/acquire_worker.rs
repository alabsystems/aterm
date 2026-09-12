// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One demand-driven drawable acquisition, with no caller-side wait or join.
//!
//! This module deliberately knows nothing about Objective-C. Inputs, outputs,
//! and retirement payloads must own their resources and perform any required
//! autorelease-pool cleanup themselves. A retirement payload is processed only
//! after the preceding acquisition has returned, including when that request
//! had not started when the caller closed the worker.

use std::sync::{Arc, mpsc};

/// Admission includes a published result that the caller has not taken yet.
pub(crate) const fn request_allowed(pending: bool, closed: bool) -> bool {
    !pending && !closed
}

/// The caller must additionally own a completed result. Epochs identify the
/// complete surface configuration/loss domain, not merely its pixel dimensions.
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
    pub(crate) outcome: AcquireOutcome<O>,
}

enum Command<I, R> {
    Request { generation: u64, input: I },
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
    /// `notify` must be a short, nonblocking event publication. Neither callback
    /// runs under a mutex shared with the caller. The acquisition callback may
    /// block; its caller never waits for it. A callback panic becomes a terminal
    /// result rather than an indefinitely outstanding request.
    pub(crate) fn spawn(
        acquire: impl FnMut(I) -> O + Send + 'static,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Self> {
        // At most one Request is admitted. The second slot is reserved for the
        // ONE consuming close, so retirement can never wait for acquisition.
        let (commands, requests) = mpsc::sync_channel(2);
        let (completed, results) = mpsc::sync_channel(1);
        // THE MARKER IS FOR THE CENSUS, and the fact it states is already true.
        // `mod metal` is `#[cfg(target_os = "macos")]` at its declaration
        // (aterm-gpu/src/lib.rs:83), so nothing in this module can reach
        // wasm32 — but OB-12's wasm-closure scan is LEXICAL over the crates in
        // the closure, not a walk of the module graph, so it cannot see that
        // cfg from here and reported the posture as "broken or unproven". The
        // honest repair the diagnostic itself names for code that never ships
        // to wasm is the recognized marker, so the true fact is written where
        // the scanner reads. It is redundant to the compiler and load-bearing
        // to the gate; if the census ever learns to follow a module's own cfg,
        // this line is the one to delete.
        #[cfg(not(target_arch = "wasm32"))]
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

    pub(crate) const fn is_pending(&self) -> bool {
        self.pending_generation.is_some()
    }

    /// Return the input unchanged on busy/closed/disconnected admission. This
    /// never waits for a queue slot, acquisition, or a result notification.
    pub(crate) fn try_request(&mut self, generation: u64, input: I) -> Result<(), I> {
        if !request_allowed(self.is_pending(), self.closed) {
            return Err(input);
        }
        let Some(commands) = &self.commands else {
            return Err(input);
        };
        match commands.try_send(Command::Request { generation, input }) {
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

    /// Exactly one terminal result per accepted request. In particular, a
    /// disconnected result producer clears pending and closes future admission.
    pub(crate) fn try_take(&mut self) -> Option<Completed<O>> {
        let generation = self.pending_generation?;
        let result = self.results.as_ref()?.try_recv();
        let completed = match result {
            Ok(completed) => completed,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Completed {
                generation,
                outcome: AcquireOutcome::Disconnected,
            },
        };
        self.pending_generation = None;
        if !matches!(&completed.outcome, AcquireOutcome::Ready(_)) {
            self.closed = true;
        }
        Some(completed)
    }

    /// Close once, discard any unclaimed result, and retire after acquisition.
    /// No join or blocking send occurs on the caller. Resource destructors
    /// retain their cleanup obligations. On failure the caller retains the
    /// retirement payload. With the private one-request /
    /// one-close protocol, capacity two cannot be full; a disconnected worker
    /// has already ceased acquisition and no longer needs deferred retirement.
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

fn run_worker<I, O, R>(
    mut acquire: impl FnMut(I) -> O,
    notify: Arc<dyn Fn() + Send + Sync>,
    requests: mpsc::Receiver<Command<I, R>>,
    completed: mpsc::SyncSender<Completed<O>>,
) {
    let mut poisoned = false;
    while let Ok(command) = requests.recv() {
        match command {
            Command::Request { generation, input } => {
                let outcome = if poisoned {
                    // Normal admission closes when the panic result
                    // is taken. Do not reuse an unwound callback if
                    // that contract is accidentally weakened.
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
                // The owner remains pending through publication,
                // so a second result cannot fill the one-slot queue.
                // Closing drops the receiver; rejection then drops
                // the output here, before the Retire command runs.
                if completed
                    .try_send(Completed {
                        generation,
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

    const LIMIT: Duration = Duration::from_secs(5);

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
        let model = aterm_spec::derive::metal_drawable_acquire_model();
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
        // The fake acquire cannot finish until AFTER all caller operations,
        // including close, return. This pins progress without a timing guess.
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
        // Close disconnected the result receiver before acquisition returned:
        // no invisible result can create a spurious ready wake.
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
            // Gate only entry into the actual shipping worker loop. Both FIFO
            // commands must be queued before that loop may receive either.
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
        // Joining is test cleanup AFTER retirement; AcquireWorker itself never
        // owns a JoinHandle and its caller cannot accidentally join on close.
        thread.join().unwrap();
    }

    #[test]
    fn real_worker_admission_publication_and_stale_result_match_the_model() {
        let model = aterm_spec::derive::metal_drawable_acquire_model();
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
        // Ready still occupies the one logical slot until it is consumed.
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
        // Historical stale acceptance (checking dimensions / open alone) is
        // rejected by the same guard that the shipping caller invokes.
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
        let model = aterm_spec::derive::metal_drawable_acquire_model();
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
}
