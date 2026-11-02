// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One durable action at a time. An unanswered write remains an observation task.

use super::model::{Action, Artifact, Journal, Next};
use crate::{Error, Result};
use std::time::Instant;

pub(super) enum Receipt {
    Signed(Artifact),
    /// Exact original unsigned payload retained during explicit cleanup; no sign applied.
    UnsignedRetained,
    Submitted(String),
    Accepted {
        submission: String,
        sha256: String,
    },
    Fetched {
        commit: String,
        principal: String,
    },
    Tag,
    Draft(u64),
    Asset(u64),
    Readback {
        sha256: String,
        bytes: u64,
    },
    AssetAbsent,
    DraftAbsent,
    TagAbsent,
}

pub(super) enum Outcome {
    Applied(Receipt),
    /// The child or request is positively known not to have started.
    NotStarted(&'static str),
    /// An unanswered mutation may already exist. Only observation may follow.
    Pending(&'static str),
}

pub(super) trait Host {
    /// Revalidate current admission, material, target and caller authority. No cached
    /// credential may outlive this check across a later external operation.
    fn authorize(&mut self, journal: &Journal, deadline: Instant) -> Result<()>;
    fn perform(&mut self, journal: &Journal, action: Action, deadline: Instant) -> Result<Outcome>;
    fn observe(&mut self, journal: &Journal, action: Action, deadline: Instant) -> Result<Outcome>;
    fn notary(&mut self, journal: &Journal, deadline: Instant) -> Result<Outcome>;
    /// Reobserve artifact identity, Apple evidence and owned remote-object absence. Stored
    /// completion flags alone never establish a current successful run or resume.
    fn final_check(&mut self, journal: &Journal, full: bool, deadline: Instant)
    -> Result<Finality>;
    fn now_ms(&self) -> u64;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Finality {
    Fresh,
    Historical,
}

pub(super) trait Persistence {
    fn save(&mut self, journal: &Journal) -> Result<()>;
}

impl Persistence for super::journal::Store {
    fn save(&mut self, journal: &Journal) -> Result<()> {
        self.save(journal)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Progress {
    Advanced,
    Pending(&'static str),
    Complete,
    HistoricalComplete,
    Clean,
}

pub(super) fn step(
    journal: &mut Journal,
    host: &mut dyn Host,
    store: &mut dyn Persistence,
    deadline: Instant,
) -> Result<Progress> {
    journal.validate()?;
    if Instant::now() >= deadline {
        return Ok(Progress::Pending("deadline"));
    }
    host.authorize(journal, deadline)?;
    if Instant::now() >= deadline {
        return Ok(Progress::Pending("deadline"));
    }
    let next = journal.next();
    let outcome = match next {
        Next::Complete | Next::Clean => {
            let finality = host.final_check(journal, next == Next::Complete, deadline)?;
            if Instant::now() >= deadline {
                return Ok(Progress::Pending("deadline"));
            }
            return Ok(
                if next == Next::Complete && finality == Finality::Historical {
                    Progress::HistoricalComplete
                } else if next == Next::Complete {
                    Progress::Complete
                } else {
                    Progress::Clean
                },
            );
        }
        Next::PollNotary => host.notary(journal, deadline)?,
        Next::Observe(action) => host.observe(journal, action, deadline)?,
        Next::Perform(action) => {
            let mut intent = journal.clone();
            intent.pending = Some(action);
            intent.updated_ms = host.now_ms().max(intent.updated_ms);
            store.save(&intent)?;
            *journal = intent;
            host.perform(journal, action, deadline)?
        }
    };
    match outcome {
        Outcome::Pending(why) => Ok(Progress::Pending(why)),
        Outcome::NotStarted(why) => {
            if matches!(next, Next::Observe(_)) {
                return Err(Error::new(
                    "an observation cannot prove a prior request never started",
                ));
            }
            let mut changed = journal.clone();
            changed.pending = None;
            changed.updated_ms = host.now_ms().max(changed.updated_ms);
            store.save(&changed)?;
            *journal = changed;
            Ok(Progress::Pending(why))
        }
        Outcome::Applied(receipt) => {
            // Do not clear an uncertain action when an unrelated or incomplete receipt
            // arrives. The persisted pending action still forces observation on resume.
            let mut changed = journal.clone();
            apply(&mut changed, next, receipt)?;
            changed.pending = None;
            changed.updated_ms = host.now_ms().max(changed.updated_ms);
            changed.validate()?;
            store.save(&changed)?;
            *journal = changed;
            Ok(Progress::Advanced)
        }
    }
}

fn apply(journal: &mut Journal, next: Next, receipt: Receipt) -> Result<()> {
    if matches!(next, Next::PollNotary) {
        let Receipt::Accepted { submission, sha256 } = receipt else {
            return Err(Error::new("notary observation has no acceptance receipt"));
        };
        if journal.submission.as_deref() != Some(submission.as_str())
            || journal.artifact.as_ref().map(|a| a.sha256.as_str()) != Some(sha256.as_str())
        {
            return Err(Error::new("notary acceptance names another probe artifact"));
        }
        journal.apple_accepted = true;
        return Ok(());
    }
    let action = match next {
        Next::Perform(action) | Next::Observe(action) => action,
        _ => return Err(Error::new("unexpected release probe receipt")),
    };
    match (action, receipt) {
        (Action::Sign, Receipt::UnsignedRetained)
            if matches!(next, Next::Observe(Action::Sign)) && journal.unsigned_cleanup() => {}
        (Action::Sign, Receipt::Signed(artifact)) => journal.artifact = Some(artifact),
        (Action::Submit, Receipt::Submitted(id)) if !id.is_empty() && id.len() <= 64 => {
            journal.submission = Some(id);
        }
        (Action::Fetch(index), Receipt::Fetched { commit, principal }) => {
            journal.repositories[index].commit = Some(commit);
            journal.repositories[index].principal = Some(principal);
        }
        (Action::Tag(index), Receipt::Tag) => journal.repositories[index].tag_owned = true,
        (Action::Draft(index), Receipt::Draft(id)) => {
            journal.repositories[index].release = Some(id)
        }
        (Action::Upload(index), Receipt::Asset(id)) => journal.repositories[index].asset = Some(id),
        (Action::Readback(index), Receipt::Readback { sha256, bytes }) => {
            if journal
                .artifact
                .as_ref()
                .is_none_or(|a| a.sha256 != sha256 || a.bytes != bytes)
            {
                return Err(Error::new(
                    "uploaded probe bytes do not match the signed artifact",
                ));
            }
            journal.repositories[index].verified = true;
        }
        (Action::DeleteAsset(index), Receipt::AssetAbsent) => {
            journal.repositories[index].asset_absent = true;
        }
        (Action::DeleteDraft(index), Receipt::DraftAbsent) => {
            journal.repositories[index].release_absent = true;
        }
        (Action::DeleteTag(index), Receipt::TagAbsent) => {
            journal.repositories[index].tag_absent = true;
        }
        _ => {
            return Err(Error::new(
                "receipt does not match the pending release probe action",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
