// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Captured command output: the ordinary spawn unless the caller supplies an
//! executor to own its lifetime. Delegation is local to the calling thread;
//! worker threads need an explicit executor of their own.

use crate::spawnfd::Command;
use std::cell::RefCell;
use std::io;
use std::process::Output;
use std::sync::Arc;

/// A caller's command executor, including its cancellation and reaping policy.
pub type Executor = dyn Fn(&mut Command) -> io::Result<Output> + Send + Sync;

thread_local! {
    static EXECUTOR: RefCell<Option<Arc<Executor>>> = const { RefCell::new(None) };
}

/// Run `command`, reading all of stdout and stderr, through the current
/// [`with_executor`] if one is installed. The executor must spawn directly
/// rather than recursively calling this function.
///
/// # Errors
/// The executor's error, or the command's spawn, read or wait error.
pub fn output(command: &mut Command) -> io::Result<Output> {
    let executor = EXECUTOR.with(|slot| slot.borrow().clone());
    match executor {
        Some(executor) => executor(command),
        None => command.output(),
    }
}

/// Run `walk` under a caller-owned command executor on this thread. A nested
/// executor restores the previous one, including during unwinding.
pub fn with_executor<T>(executor: Arc<Executor>, walk: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<Executor>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            EXECUTOR.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(EXECUTOR.with(|slot| slot.replace(Some(executor))));
    walk()
}

/// Extension spelling for commands whose output is owned by the current
/// executor. Outside a scope this has the ordinary `Command::output` behavior.
pub trait CommandOutput {
    /// Run this command through [`output`].
    ///
    /// # Errors
    /// As [`output`].
    fn output_owned(&mut self) -> io::Result<Output>;
}

impl CommandOutput for Command {
    fn output_owned(&mut self) -> io::Result<Output> {
        output(self)
    }
}
