// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`HookRunner`], the runner the race tests hand the code under test.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use crate::ledger::testing::Git;
use crate::ledger::{GitRunner, Result, RunOut};

/// [`Git`] that counts the `git push`es it runs and calls `on_push` with each one's number just
/// before it: a test's rival, moving the remote between the code's fetch and its push.
pub(crate) struct HookRunner<F: FnMut(u32)> {
    inner: Git,
    /// The pushes run so far.
    pub(crate) pushes: Cell<u32>,
    on_push: RefCell<F>,
}

impl<F: FnMut(u32)> HookRunner<F> {
    pub(crate) fn new(repo: impl Into<PathBuf>, on_push: F) -> Self {
        HookRunner {
            inner: Git::new(repo),
            pushes: Cell::new(0),
            on_push: RefCell::new(on_push),
        }
    }

    fn before(&self, args: &[&str]) {
        if args.first() == Some(&"push") {
            let n = self.pushes.get() + 1;
            self.pushes.set(n);
            (self.on_push.borrow_mut())(n);
        }
    }
}

impl<F: FnMut(u32)> GitRunner for HookRunner<F> {
    fn git(&self, args: &[&str]) -> Result<RunOut> {
        self.before(args);
        self.inner.git(args)
    }

    fn git_env(&self, args: &[&str], env: &[(&str, &str)]) -> Result<RunOut> {
        self.before(args);
        self.inner.git_env(args, env)
    }

    fn git_input(&self, args: &[&str], input: &[u8]) -> Result<RunOut> {
        self.before(args);
        self.inner.git_input(args, input)
    }
}
