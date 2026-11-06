// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Native atomic ref publication preserves the original Git object graph,
//! including annotated tags, signatures and custom ref namespaces.

use std::path::Path;

use super::{AtomicRefBatch, Refs};
use crate::error::{Error, Result};
use crate::git::{GitOut, GitRun};
use crate::journal::IntentStore;
use crate::policy::Permit;
use crate::write::{Effect, Ensured, Outcome};

impl Refs<'_> {
    fn native_run(
        &self,
        permit: &Permit,
        dir: &Path,
        batch: &AtomicRefBatch,
        dry: bool,
    ) -> Result<GitRun> {
        self.write_checks(permit)?;
        let repo = self.writer_view(permit).push_repo()?;
        if !self.slug.eq_full_name(&repo.full_name) {
            return Err(Error::protocol(
                "repository metadata names another repository",
            ));
        }
        Ok(GitRun::push_refs(&self.slug, dir, batch.clone(), dry).with_named(permit.named()))
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
        self.gh.run_git(&self.native_run(permit, dir, batch, true)?)
    }

    /// Push exact local objects once with atomic leases and a durable intent.
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
        let run = self.native_run(permit, dir, batch, false)?;
        Ok(match self.gh.push_git_once(&run, permit, intents)? {
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
