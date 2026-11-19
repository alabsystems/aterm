// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The live-plan test waits for its actual start's progress, not the interactive launch budget.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::Attested;

thread_local! {
    static WORKING_START: Cell<bool> = const { Cell::new(false) };
}

/// Restore this thread's prior policy on every exit, including a failed assertion.
pub(crate) struct Scope {
    previous: bool,
    thread: PhantomData<Rc<()>>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        WORKING_START.with(|slot| slot.set(self.previous));
    }
}

pub(crate) fn while_working() -> Scope {
    Scope {
        previous: WORKING_START.with(|slot| slot.replace(true)),
        thread: PhantomData,
    }
}

#[derive(Debug)]
enum Failure {
    Stalled,
    Guard,
    Output,
    Watcher(String),
}

fn answer_after_wait(waited: Result<(), Failure>, answer: Option<Attested>) -> Option<Attested> {
    match waited {
        Ok(()) => answer,
        Err(why) => Some(Attested::Failed(match why {
            Failure::Stalled => format!(
                "the fixture's owned trust start made no CPU or I/O progress for {:?}",
                aterm_spec::wait::MUST_HAPPEN
            ),
            Failure::Guard => format!(
                "the fixture's owned trust start did not finish within the {:?} working guard",
                aterm_spec::wait::WORKING_GUARD
            ),
            Failure::Output => format!(
                "the fixture's trust start ended but its owned streams did not finish within {:?}",
                aterm_spec::wait::MUST_HAPPEN
            ),
            Failure::Watcher(why) => format!("the fixture's trust-start observer failed: {why}"),
        })),
    }
}

#[derive(Default)]
struct Control {
    done: AtomicBool,
    stopped: AtomicBool,
}

/// A watcher that unwinds before the owner finishes must still wake that owner's stop check.
struct WakeOwner(Arc<Control>);

impl Drop for WakeOwner {
    fn drop(&mut self) {
        if !self.0.done.load(Ordering::SeqCst) {
            self.0.stopped.store(true, Ordering::SeqCst);
        }
    }
}

pub(super) struct Wait {
    enabled: bool,
    control: Arc<Control>,
    watcher: RefCell<Option<std::thread::JoinHandle<Result<(), Failure>>>>,
    failure: RefCell<Option<Failure>>,
}

impl Wait {
    pub(super) fn new() -> Self {
        Self {
            enabled: WORKING_START.with(Cell::get),
            control: Arc::new(Control::default()),
            watcher: RefCell::new(None),
            failure: RefCell::new(None),
        }
    }

    pub(super) fn deadline(&self, ordinary: Option<Instant>) -> Option<Instant> {
        if self.enabled { None } else { ordinary }
    }

    /// Only the native stream's successful creation receipt supplies this root. The observer
    /// can request that stream's stop; the stream alone owns retirement of the child and group.
    pub(super) fn spawned(&self, root: u32) {
        if !self.enabled {
            return;
        }
        let control = Arc::clone(&self.control);
        let watcher = std::thread::Builder::new()
            .name("trust-start-fixture".into())
            .spawn(move || {
                let _wake = WakeOwner(Arc::clone(&control));
                let done = || control.done.load(Ordering::SeqCst).then_some(());
                match aterm_spec::wait::within_while_working(
                    root,
                    aterm_spec::wait::MUST_HAPPEN,
                    aterm_spec::wait::WORKING_GUARD,
                    Duration::from_millis(10),
                    done,
                ) {
                    Ok(()) => Ok(()),
                    Err(aterm_spec::wait::Unanswered::Stalled) => Err(Failure::Stalled),
                    Err(aterm_spec::wait::Unanswered::Guard) => Err(Failure::Guard),
                    // Natural exit can precede the owner's final pipe reads. It is neither
                    // completion nor a stall: leave bounded stream completion to that owner.
                    Err(aterm_spec::wait::Unanswered::Ended) => aterm_spec::wait::within(
                        aterm_spec::wait::MUST_HAPPEN,
                        Duration::from_millis(10),
                        done,
                    )
                    .ok_or(Failure::Output),
                }
            });
        match watcher {
            Ok(watcher) => *self.watcher.borrow_mut() = Some(watcher),
            Err(error) => {
                *self.failure.borrow_mut() = Some(Failure::Watcher(error.to_string()));
                self.control.stopped.store(true, Ordering::SeqCst);
            }
        }
    }

    pub(super) fn stopped(&self) -> bool {
        self.control.stopped.load(Ordering::SeqCst)
    }

    fn finish(&self) -> Result<(), Failure> {
        self.control.done.store(true, Ordering::SeqCst);
        if let Some(watcher) = self.watcher.borrow_mut().take() {
            match watcher.join() {
                Ok(result) => result?,
                Err(_) => return Err(Failure::Watcher("observer panicked".into())),
            }
        }
        self.failure.borrow_mut().take().map_or(Ok(()), Err)
    }

    pub(super) fn answer(&self, answer: Option<Attested>) -> Option<Attested> {
        answer_after_wait(self.finish(), answer)
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        self.control.stopped.store(true, Ordering::SeqCst);
        let _ = self.finish();
    }
}

#[cfg(test)]
#[path = "launch_trust_start_fixture_tests.rs"]
mod tests;
