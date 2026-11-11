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
#[path = "acquire_worker_tests.rs"]
mod tests;
