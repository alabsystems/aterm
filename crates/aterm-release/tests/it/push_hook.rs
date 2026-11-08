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
    #[cfg(unix)]
    native: std::cell::OnceCell<crate::publisher_origin::Git>,
    #[cfg(unix)]
    repo: PathBuf,
}

impl<F: FnMut(u32)> HookRunner<F> {
    pub(crate) fn new(repo: impl Into<PathBuf>, on_push: F) -> Self {
        let repo = repo.into();
        HookRunner {
            inner: Git::new(repo.clone()),
            pushes: Cell::new(0),
            on_push: RefCell::new(on_push),
            #[cfg(unix)]
            native: std::cell::OnceCell::new(),
            #[cfg(unix)]
            repo,
        }
    }

    pub(crate) fn before_write(&self) {
        let n = self.pushes.get() + 1;
        self.pushes.set(n);
        (self.on_push.borrow_mut())(n);
    }

    #[cfg(unix)]
    fn origin(&self) -> &crate::publisher_origin::Git {
        self.native
            .get_or_init(|| crate::publisher_origin::Git::new(&self.repo))
    }

    fn before(&self, args: &[&str]) {
        if args.first() == Some(&"push") {
            self.before_write();
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

// The race is injected at the typed mutation boundary, never by decoding Git
// argv. Existing local-plumbing hooks remain available to unrelated fixtures.
#[cfg(unix)]
impl<F: FnMut(u32)> crate::ledger::Origin for HookRunner<F> {
    fn refresh_main(&self) -> std::result::Result<String, String> {
        crate::ledger::Origin::refresh_main(self.origin())
    }
    fn get(&self, reference: &str) -> std::result::Result<Option<String>, String> {
        crate::ledger::Origin::get(self.origin(), reference)
    }
    fn import(&self, oid: &str) -> std::result::Result<(), String> {
        crate::ledger::Origin::import(self.origin(), oid)
    }
    fn peel(&self, oid: &str) -> std::result::Result<Option<String>, String> {
        crate::ledger::Origin::peel(self.origin(), oid)
    }
    fn tag_object(&self, oid: &str) -> std::result::Result<Vec<u8>, String> {
        crate::ledger::Origin::tag_object(self.origin(), oid)
    }
    fn write(
        &self,
        reference: &str,
        before: Option<&str>,
        after: &str,
    ) -> std::result::Result<crate::ledger::RefWrite, String> {
        self.before_write();
        crate::ledger::Origin::write(self.origin(), reference, before, after)
    }
    fn delete(
        &self,
        reference: &str,
        expected_oid: &str,
    ) -> std::result::Result<crate::ledger::RefWrite, String> {
        crate::ledger::Origin::delete(self.origin(), reference, expected_oid)
    }
    fn control_transaction(
        &self,
        transaction: crate::ledger::ControlRefTransaction,
    ) -> std::result::Result<crate::ledger::RefWrite, String> {
        crate::ledger::Origin::control_transaction(self.origin(), transaction)
    }
}
