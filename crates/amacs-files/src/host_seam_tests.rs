// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;

/// The step a commit is about to take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Open the target's folder, which every later step works in ([`super::Folder`]).
    Folder,
    /// Check by path the binding the folder was just opened by.
    Opened,
    /// Open (or make) the save lock in it.
    Lock,
    /// Create the temporary that holds the new bytes.
    Temporary,
    /// Check the binding a last time before publishing.
    Validate,
    /// Publish the temporary at the target's name.
    Publish,
    /// Check, once it is published, that the path still reaches the folder it is in.
    Observe,
}

type Hook = Box<dyn FnMut(Step)>;

thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

/// Run `f` with `hook` called at every step a commit on this thread takes, cleared after
/// it even on unwind. Unix's alone: the tests that act at the steps swap folders for links.
#[cfg(unix)]
pub(crate) fn with<T>(hook: impl FnMut(Step) + 'static, f: impl FnOnce() -> T) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            HOOK.with(|h| h.replace(None));
        }
    }
    HOOK.with(|h| h.replace(Some(Box::new(hook))));
    let _clear = Clear;
    f()
}

/// A commit on this thread is about to take `step`.
pub(crate) fn reached(step: Step) {
    HOOK.with(|h| {
        if let Some(hook) = h.borrow_mut().as_mut() {
            hook(step);
        }
    });
}
