// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One demand-driven drawable acquisition, with no caller-side wait or join.
//!
//! The worker posts its wake only where a frame waits for the drawable. A frame that finds none
//! in hand ([`AcquireWorker::await_result`]) sets `wanted`, then looks once more for a result;
//! the worker publishes a result, then takes `wanted` and posts the wake if it was set. So a
//! frame that goes on waiting either saw the result, or set `wanted` before the worker took it.
//! `MetalDrawableAcquire` (`aterm-models-render`) holds those two orders over every
//! interleaving: a recorded wait always has a wake coming (`NoStrandedWait`), and a wake is
//! posted only where a frame asked (`WakeOnlyWhenWanted`).
//!
//! A frame queues its own request only where its caller admits one. The surface admits none
//! while its layer still owes a reshape, so a drawable of the current generation always comes
//! from a layer that already had that generation's configuration (`ShapedBeforeRequested`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// What the two threads share beside the channels.
#[derive(Debug, Default)]
struct Signal {
    /// A frame waits for the outstanding request's result: the worker posts the wake after it.
    wanted: AtomicBool,
    /// Results the worker has published. The waiting frame reads it between setting `wanted` and
    /// looking for a result, and the worker adds to it between publishing and taking `wanted`:
    /// with both sequentially consistent, a worker that took `wanted` unset published before the
    /// frame's read, so the publication happens before the frame's look and the look finds it.
    published: AtomicU64,
}

/// Where a waiting frame stands inside [`AcquireWorker::await_result_with`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitStep {
    /// `wanted` is set and no result has been looked for.
    Wanted,
    /// The look found nothing the frame can use, and no request has been queued for it.
    Recollected,
}

/// What a frame with no drawable in hand gets from [`AcquireWorker::await_result`].
#[derive(Debug)]
pub(crate) enum Awaited<O> {
    /// The outstanding request's result, of the frame's generation or the worker's terminal
    /// failure: nothing is waited for and no wake is owed.
    Result(Completed<O>),
    /// A request is in flight, and the worker posts the wake once it publishes that result.
    /// `stale` is a result of an earlier generation this call took before queueing its own.
    Waiting { stale: Option<Completed<O>> },
    /// Nothing is in flight and the caller admitted no request (`input` answered `None`):
    /// nothing was queued and no wake is coming, so the caller owes its own retry.
    Unqueued { stale: Option<Completed<O>> },
    /// Nothing is in flight and no request could be queued: no wake is coming.
    Refused { stale: Option<Completed<O>> },
}

pub(crate) struct AcquireWorker<I, O, R> {
    commands: Option<mpsc::SyncSender<Command<I, R>>>,
    results: Option<mpsc::Receiver<Completed<O>>>,
    pending_generation: Option<u64>,
    closed: bool,
    signal: Arc<Signal>,
    /// Results this side has taken off the channel, against [`Signal::published`].
    taken: u64,
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
        let signal = Arc::new(Signal::default());
        let publisher = Publisher {
            completed,
            signal: Arc::clone(&signal),
            notify,
        };
        std::thread::Builder::new()
            .name("aterm-drawable-acquire".to_owned())
            .spawn(move || run_worker(acquire, requests, &publisher))?;
        Ok(Self {
            commands: Some(commands),
            results: Some(results),
            pending_generation: None,
            closed: false,
            signal,
            taken: 0,
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
            Ok(completed) => {
                self.taken += 1;
                completed
            }
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

    /// The step of a frame that has no drawable in hand: say that it waits, look once more for
    /// the outstanding request's result, and where that is nothing, or a result of an earlier
    /// generation than `generation`, wait for the request in flight or queue one from `input`.
    /// `input` is asked only where nothing is in flight, and answers `None` where the caller
    /// admits no request now: the answer is then [`Awaited::Unqueued`], with nothing queued.
    /// Never blocks. After [`Awaited::Waiting`] a wake follows: the worker posts `notify` when it
    /// next takes `wanted`, which is after it publishes the request in flight, or after an
    /// earlier publication it had not yet followed when this call set `wanted`. A frame woken
    /// with no result yet calls again.
    pub(crate) fn await_result(
        &mut self,
        generation: u64,
        input: impl FnOnce() -> Option<I>,
    ) -> Awaited<O> {
        self.await_result_with(generation, input, |_| {})
    }

    /// [`Self::await_result`], calling `between` at each [`WaitStep`]: where the worker's steps
    /// can fall between this side's, so a test runs them there.
    pub(crate) fn await_result_with(
        &mut self,
        generation: u64,
        input: impl FnOnce() -> Option<I>,
        mut between: impl FnMut(WaitStep),
    ) -> Awaited<O> {
        self.signal.wanted.store(true, Ordering::SeqCst);
        between(WaitStep::Wanted);
        // Read for its place in the order: see `Signal::published`. The count trails a result
        // this side took between the worker's publication and its count of it.
        let unseen = self.signal.published.load(Ordering::SeqCst) > self.taken;
        let taken = self.try_take();
        debug_assert!(
            taken.is_some() || !unseen,
            "a result published before this look was not found by it"
        );
        let usable = taken.as_ref().is_some_and(|completed| {
            !matches!(completed.outcome, AcquireOutcome::Ready(_))
                || result_is_current(completed.generation, generation, false)
        });
        if usable {
            self.signal.wanted.store(false, Ordering::SeqCst);
            return Awaited::Result(taken.expect("a usable result was taken"));
        }
        between(WaitStep::Recollected);
        if self.is_pending() {
            return Awaited::Waiting { stale: taken };
        }
        let Some(input) = input() else {
            self.signal.wanted.store(false, Ordering::SeqCst);
            return Awaited::Unqueued { stale: taken };
        };
        if self.try_request(generation, input).is_ok() {
            Awaited::Waiting { stale: taken }
        } else {
            self.signal.wanted.store(false, Ordering::SeqCst);
            Awaited::Refused { stale: taken }
        }
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

/// The worker's end of a result.
struct Publisher<O> {
    completed: mpsc::SyncSender<Completed<O>>,
    signal: Arc<Signal>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl<O> Publisher<O> {
    /// Hand `result` to the caller. False: the caller closed and dropped its receiver, so
    /// nothing waits for this result.
    fn publish(&self, result: Completed<O>) -> bool {
        let published = self.completed.try_send(result).is_ok();
        if published {
            self.signal.published.fetch_add(1, Ordering::SeqCst);
        }
        published
    }

    /// Post the wake where a frame waits. Only after [`Self::publish`]: a frame that set
    /// `wanted` too late for this call looks for the result after setting it, and finds it.
    fn wake_if_wanted(&self) {
        if self.signal.wanted.swap(false, Ordering::SeqCst) {
            (self.notify)();
        }
    }
}

fn run_worker<I, O, R>(
    mut acquire: impl FnMut(I) -> O,
    requests: mpsc::Receiver<Command<I, R>>,
    publisher: &Publisher<O>,
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
                if publisher.publish(Completed {
                    generation,
                    queue_ns,
                    outcome,
                }) {
                    publisher.wake_if_wanted();
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
#[path = "acquire_worker_tests.rs"]
pub(super) mod tests;
