// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Native atomic ref publication preserves the original Git object graph,
//! including annotated tags, signatures and custom ref namespaces.

use std::path::Path;

use super::{AtomicRefBatch, Refs};
use crate::error::{Error, Result};
use crate::git::ControlRefTransaction;
use crate::git::{GitOut, GitRun};
use crate::journal::IntentStore;
use crate::policy::Permit;
use crate::write::{Effect, Ensured, Outcome};

impl Refs<'_> {
    fn native_control_run(
        &self,
        permit: &Permit,
        dir: &Path,
        transaction: &ControlRefTransaction,
        dry_run: bool,
        budget: std::time::Duration,
        progress_window: Option<std::time::Duration>,
    ) -> Result<GitRun> {
        self.write_checks(permit)?;
        let repo = self.writer_view(permit).push_repo()?;
        if !self.slug.eq_full_name(&repo.full_name) {
            return Err(Error::protocol(
                "repository metadata names another repository",
            ));
        }
        let run = GitRun::push_control(&self.slug, dir, transaction.clone(), dry_run)
            .with_named(permit.named())
            .with_budget(budget);
        Ok(match progress_window {
            Some(window) => run.with_progress_window(window),
            None => run,
        })
    }

    /// Validate the canonical lease/fence transition and native atomic support
    /// without changing refs or opening a write intent. The supplied budget is
    /// each native child's backstop, not the compound operation's elapsed limit.
    ///
    /// # Errors
    /// Writer/repository admission, source object and control-owner validation,
    /// expected-state mismatch, or the native atomic dry-run refusal.
    pub fn check_control(
        &self,
        permit: &Permit,
        dir: &Path,
        transaction: &ControlRefTransaction,
        budget: std::time::Duration,
    ) -> Result<GitOut> {
        self.gh
            .run_git(&self.native_control_run(permit, dir, transaction, true, budget, None)?)
    }

    /// Execute one canonical control transaction with the repository-wide
    /// durable atomic-write intent. Unknown outcomes fence later writes.
    ///
    /// # Errors
    /// As [`Self::check_control`], plus intent/journal failures. A transient
    /// receive failure is resolved by reading the guarded remote refs; only a
    /// complete AFTER view establishes success without a wire acknowledgement.
    pub fn push_control(
        &self,
        permit: &Permit,
        dir: &Path,
        transaction: &ControlRefTransaction,
        intents: &dyn IntentStore,
        budget: std::time::Duration,
    ) -> Result<Outcome<Ensured<ControlRefTransaction>>> {
        let run = self.native_control_run(permit, dir, transaction, false, budget, None)?;
        self.push_control_run(permit, transaction, intents, &run)
    }

    /// Execute one canonical control transaction with a native-progress
    /// inactivity window instead of a wall-clock execution bound. The ordinary
    /// Git budget remains only the credential grant's lifetime.
    ///
    /// # Errors
    /// As [`Self::push_control`].
    pub fn push_control_with_progress_window(
        &self,
        permit: &Permit,
        dir: &Path,
        transaction: &ControlRefTransaction,
        intents: &dyn IntentStore,
        progress_window: std::time::Duration,
    ) -> Result<Outcome<Ensured<ControlRefTransaction>>> {
        let run = self.native_control_run(
            permit,
            dir,
            transaction,
            false,
            crate::git::GIT_BUDGET,
            Some(progress_window),
        )?;
        self.push_control_run(permit, transaction, intents, &run)
    }

    fn push_control_run(
        &self,
        permit: &Permit,
        transaction: &ControlRefTransaction,
        intents: &dyn IntentStore,
        run: &GitRun,
    ) -> Result<Outcome<Ensured<ControlRefTransaction>>> {
        Ok(match self.gh.push_git_once(run, permit, intents)? {
            Outcome::Done(_) => Outcome::Done(Ensured {
                object: transaction.clone(),
                effect: Effect::Created,
            }),
            Outcome::Unknown { intent, error } => Outcome::Unknown { intent, error },
            Outcome::Refused(error) => Outcome::Refused(error),
            Outcome::NeverSent(error) => Outcome::NeverSent(error),
        })
    }

    fn native_run(
        &self,
        permit: &Permit,
        dir: &Path,
        batch: &AtomicRefBatch,
        dry: bool,
        budget: std::time::Duration,
        progress_window: Option<std::time::Duration>,
    ) -> Result<GitRun> {
        self.write_checks(permit)?;
        let repo = self.writer_view(permit).push_repo()?;
        if !self.slug.eq_full_name(&repo.full_name) {
            return Err(Error::protocol(
                "repository metadata names another repository",
            ));
        }
        let run = GitRun::push_refs(&self.slug, dir, batch.clone(), dry)
            .with_named(permit.named())
            .with_budget(budget);
        Ok(match progress_window {
            Some(window) => run.with_progress_window(window),
            None => run,
        })
    }

    /// Validate the source graph, exact remote values and native atomic push
    /// support without changing refs or opening a write intent.
    ///
    /// # Errors
    /// The writer routing, repository guard, local config, object and lease
    /// checks, or Git's dry-run refusal.
    pub fn check_push(
        &self,
        permit: &Permit,
        dir: &Path,
        batch: &AtomicRefBatch,
    ) -> Result<GitOut> {
        self.gh
            .run_git(&self.native_run(permit, dir, batch, true, crate::git::GIT_BUDGET, None)?)
    }

    /// Push exact local objects with atomic leases and one durable intent.
    /// Transient receive failures re-read remote refs before any retry.
    /// A journaled unknown outcome blocks every later atomic batch for this
    /// repository until explicitly reconciled.
    ///
    /// # Errors
    /// As `check_push`, plus the journal's errors. A missing acknowledgement
    /// retains an open intent and an unknown side effect.
    pub fn push_atomic(
        &self,
        permit: &Permit,
        dir: &Path,
        batch: &AtomicRefBatch,
        intents: &dyn IntentStore,
    ) -> Result<Outcome<Ensured<AtomicRefBatch>>> {
        self.push_atomic_with_budget(permit, dir, batch, intents, crate::git::GIT_BUDGET)
    }

    /// The same journaled atomic operation with its caller's native per-child
    /// backstop. API admission phases retain their own client deadlines.
    ///
    /// # Errors
    /// As [`Self::push_atomic`].
    pub fn push_atomic_with_budget(
        &self,
        permit: &Permit,
        dir: &Path,
        batch: &AtomicRefBatch,
        intents: &dyn IntentStore,
        budget: std::time::Duration,
    ) -> Result<Outcome<Ensured<AtomicRefBatch>>> {
        let run = self.native_run(permit, dir, batch, false, budget, None)?;
        self.push_atomic_run(permit, batch, intents, &run)
    }

    /// The same journaled atomic operation with a native-progress inactivity
    /// window instead of a wall-clock execution bound. The ordinary Git budget
    /// remains only the credential grant's lifetime.
    ///
    /// # Errors
    /// As [`Self::push_atomic`].
    pub fn push_atomic_with_progress_window(
        &self,
        permit: &Permit,
        dir: &Path,
        batch: &AtomicRefBatch,
        intents: &dyn IntentStore,
        progress_window: std::time::Duration,
    ) -> Result<Outcome<Ensured<AtomicRefBatch>>> {
        let run = self.native_run(
            permit,
            dir,
            batch,
            false,
            crate::git::GIT_BUDGET,
            Some(progress_window),
        )?;
        self.push_atomic_run(permit, batch, intents, &run)
    }

    fn push_atomic_run(
        &self,
        permit: &Permit,
        batch: &AtomicRefBatch,
        intents: &dyn IntentStore,
        run: &GitRun,
    ) -> Result<Outcome<Ensured<AtomicRefBatch>>> {
        Ok(match self.gh.push_git_once(run, permit, intents)? {
            Outcome::Done(_) => Outcome::Done(Ensured {
                object: batch.clone(),
                effect: Effect::Created,
            }),
            Outcome::Unknown { intent, error } => Outcome::Unknown { intent, error },
            Outcome::Refused(e) => Outcome::Refused(e),
            Outcome::NeverSent(e) => Outcome::NeverSent(e),
        })
    }
}
