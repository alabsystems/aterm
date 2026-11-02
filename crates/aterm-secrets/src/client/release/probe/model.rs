// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public evidence and the next action of one explicitly requested capability probe.
//! Credentials and their commitments never enter this journal.

use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Subject {
    pub uid: u32,
    pub node: String,
    pub member: String,
    pub writer: String,
    pub incarnation: String,
    pub network: String,
    pub authority: String,
    pub genesis: String,
    pub vault: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Artifact {
    pub sha256: String,
    pub executable_sha256: String,
    pub bytes: u64,
    pub signing_identity: String,
    pub team: String,
    pub executed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Repository {
    pub owner: String,
    pub commit: Option<String>,
    pub principal: Option<String>,
    pub tag_owned: bool,
    pub release: Option<u64>,
    pub asset: Option<u64>,
    pub verified: bool,
    pub asset_absent: bool,
    pub release_absent: bool,
    pub tag_absent: bool,
}

impl Repository {
    pub(super) fn new(owner: String) -> Self {
        Self {
            owner,
            commit: None,
            principal: None,
            tag_owned: false,
            release: None,
            asset: None,
            verified: false,
            asset_absent: false,
            release_absent: false,
            tag_absent: false,
        }
    }

    #[cfg(test)]
    pub(super) fn clean(&self) -> bool {
        (!self.tag_owned || self.tag_absent)
            && (self.release.is_none() || self.release_absent)
            && (self.asset.is_none() || self.asset_absent)
    }
}

/// A pending action may have happened. Recovery observes it before considering another write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Sign,
    Submit,
    Fetch(usize),
    Tag(usize),
    Draft(usize),
    Upload(usize),
    Readback(usize),
    DeleteAsset(usize),
    DeleteDraft(usize),
    DeleteTag(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Next {
    Perform(Action),
    Observe(Action),
    PollNotary,
    Complete,
    Clean,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Journal {
    pub nonce: String,
    pub subject: Subject,
    pub repositories: Vec<Repository>,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub artifact: Option<Artifact>,
    pub submission: Option<String>,
    pub apple_accepted: bool,
    pub pending: Option<Action>,
    pub cleanup_only: bool,
}

impl Journal {
    pub(super) fn unsigned_cleanup(&self) -> bool {
        self.cleanup_only
            && self.pending == Some(Action::Sign)
            && self.artifact.is_none()
            && self.submission.is_none()
            && !self.apple_accepted
            && self
                .repositories
                .iter()
                .all(|repo| repo == &Repository::new(repo.owner.clone()))
    }

    pub(super) fn tag(&self) -> String {
        format!("aterm-verification/{}", self.nonce)
    }

    /// Validate the journal's shape, not its authority. The live admission and every remote
    /// object are checked separately; an editable file cannot grant access to either.
    pub(super) fn validate(&self) -> Result<()> {
        if !hex(&self.nonce, 32)
            || !hex(&self.subject.node, 32)
            || !hex(&self.subject.member, 32)
            || !hex(&self.subject.writer, 64)
            || !hex(&self.subject.incarnation, 32)
            || !hex(&self.subject.network, 32)
            || !hex(&self.subject.authority, 64)
            || !hex(&self.subject.genesis, 64)
            || !hex(&self.subject.vault, 64)
            || self.created_ms == 0
            || self.updated_ms < self.created_ms
            || self.repositories.is_empty()
            || self.repositories.len() > 2
        {
            return Err(Error::new("invalid release probe journal"));
        }
        for (index, repo) in self.repositories.iter().enumerate() {
            if !owner(&repo.owner)
                || self.repositories[..index]
                    .iter()
                    .any(|old| old.owner.eq_ignore_ascii_case(&repo.owner))
                || repo.commit.as_deref().is_some_and(|oid| !hex(oid, 40))
                || repo.principal.as_deref().is_some_and(|name| !owner(name))
                || (repo.commit.is_some() != repo.principal.is_some())
                || (repo.commit.is_some() && !self.apple_accepted)
                || repo.release == Some(0)
                || repo.asset == Some(0)
                || (repo.tag_owned && repo.commit.is_none())
                || (repo.release.is_some() && !repo.tag_owned)
                || (repo.asset.is_some() && repo.release.is_none())
                || (repo.verified && repo.asset.is_none())
                || (repo.asset_absent && repo.asset.is_none())
                || (repo.release_absent && repo.release.is_none())
                || (repo.tag_absent && !repo.tag_owned)
                || (repo.release_absent && !repo.asset_absent && repo.asset.is_some())
                || (repo.tag_absent && repo.release.is_some() && !repo.release_absent)
            {
                return Err(Error::new("invalid release probe repository evidence"));
            }
        }
        if self.apple_accepted && (self.submission.is_none() || self.artifact.is_none()) {
            return Err(Error::new(
                "Apple acceptance lacks its artifact or submission",
            ));
        }
        if self.submission.is_some() && self.artifact.is_none() {
            return Err(Error::new("notary submission lacks its signed artifact"));
        }
        if self
            .submission
            .as_deref()
            .is_some_and(|id| !submission_id(id))
        {
            return Err(Error::new("invalid notary submission identity"));
        }
        if let Some(artifact) = &self.artifact
            && (!hex(&artifact.sha256, 64)
                || !hex(&artifact.executable_sha256, 64)
                || artifact.bytes == 0
                || artifact.bytes > 1024 * 1024
                || !hex_case(&artifact.signing_identity, 40)
                || artifact.team != aterm_update_core::pins::APPLE_TEAM_ID
                || !artifact.executed)
        {
            return Err(Error::new("invalid signed release probe evidence"));
        }
        if self.pending.is_some_and(|action| {
            action
                .index()
                .is_some_and(|index| index >= self.repositories.len())
                || !self.admits_pending(action)
        }) {
            return Err(Error::new("release probe action names no repository"));
        }
        Ok(())
    }

    fn admits_pending(&self, action: Action) -> bool {
        let repo = action.index().and_then(|i| self.repositories.get(i));
        match action {
            Action::Sign => self.artifact.is_none() && self.submission.is_none(),
            Action::Submit => self.artifact.is_some() && self.submission.is_none(),
            Action::Fetch(_) => self.apple_accepted && repo.is_some_and(|r| r.commit.is_none()),
            Action::Tag(_) => {
                self.apple_accepted && repo.is_some_and(|r| r.commit.is_some() && !r.tag_owned)
            }
            Action::Draft(_) => {
                self.apple_accepted && repo.is_some_and(|r| r.tag_owned && r.release.is_none())
            }
            Action::Upload(_) => {
                self.apple_accepted
                    && repo.is_some_and(|r| r.release.is_some() && r.asset.is_none())
            }
            Action::Readback(_) => {
                self.apple_accepted && repo.is_some_and(|r| r.asset.is_some() && !r.verified)
            }
            Action::DeleteAsset(_) => repo.is_some_and(|r| r.asset.is_some() && !r.asset_absent),
            Action::DeleteDraft(_) => repo.is_some_and(|r| {
                r.release.is_some() && !r.release_absent && (r.asset.is_none() || r.asset_absent)
            }),
            Action::DeleteTag(_) => repo.is_some_and(|r| {
                r.tag_owned && !r.tag_absent && (r.release.is_none() || r.release_absent)
            }),
        }
    }

    /// The request identity is immutable even when an effect has not yet begun. The
    /// credential source's private bytes are intentionally absent from this comparison.
    pub(super) fn same_plan(&self, other: &Self) -> bool {
        self.nonce == other.nonce
            && self.subject == other.subject
            && self.created_ms == other.created_ms
            && self
                .repositories
                .iter()
                .map(|r| &r.owner)
                .eq(other.repositories.iter().map(|r| &r.owner))
    }

    pub(super) fn follows(&self, before: &Self) -> bool {
        self.same_plan(before)
            && self.updated_ms >= before.updated_ms
            && keeps(&before.artifact, &self.artifact)
            && keeps(&before.submission, &self.submission)
            && (!before.apple_accepted || self.apple_accepted)
            && (!before.cleanup_only || self.cleanup_only)
            && self
                .repositories
                .iter()
                .zip(&before.repositories)
                .all(|(now, old)| {
                    keeps(&old.commit, &now.commit)
                        && keeps(&old.principal, &now.principal)
                        && keeps(&old.release, &now.release)
                        && keeps(&old.asset, &now.asset)
                        && (!old.tag_owned || now.tag_owned)
                        && (!old.verified || now.verified)
                        && (!old.asset_absent || now.asset_absent)
                        && (!old.release_absent || now.release_absent)
                        && (!old.tag_absent || now.tag_absent)
                })
    }

    pub(super) fn next(&self) -> Next {
        if let Some(action) = self.pending {
            return Next::Observe(action);
        }
        if self.cleanup_only {
            return self.cleanup().map_or(Next::Clean, Next::Perform);
        }
        if self.artifact.is_none() {
            return Next::Perform(Action::Sign);
        }
        if self.submission.is_none() {
            return Next::Perform(Action::Submit);
        }
        if !self.apple_accepted {
            return Next::PollNotary;
        }
        for (index, repo) in self.repositories.iter().enumerate() {
            let action = if repo.commit.is_none() {
                Some(Action::Fetch(index))
            } else if !repo.tag_owned {
                Some(Action::Tag(index))
            } else if repo.release.is_none() {
                Some(Action::Draft(index))
            } else if repo.asset.is_none() {
                Some(Action::Upload(index))
            } else if !repo.verified {
                Some(Action::Readback(index))
            } else {
                None
            };
            if let Some(action) = action {
                return Next::Perform(action);
            }
        }
        self.cleanup().map_or(Next::Complete, Next::Perform)
    }

    fn cleanup(&self) -> Option<Action> {
        self.repositories
            .iter()
            .enumerate()
            .find_map(|(index, repo)| {
                if repo.asset.is_some() && !repo.asset_absent {
                    Some(Action::DeleteAsset(index))
                } else if repo.release.is_some() && !repo.release_absent {
                    Some(Action::DeleteDraft(index))
                } else if repo.tag_owned && !repo.tag_absent {
                    Some(Action::DeleteTag(index))
                } else {
                    None
                }
            })
    }
}

fn keeps<T: PartialEq>(old: &Option<T>, new: &Option<T>) -> bool {
    old.as_ref().is_none_or(|old| new.as_ref() == Some(old))
}

impl Action {
    pub(super) const fn index(self) -> Option<usize> {
        match self {
            Self::Sign | Self::Submit => None,
            Self::Fetch(i)
            | Self::Tag(i)
            | Self::Draft(i)
            | Self::Upload(i)
            | Self::Readback(i)
            | Self::DeleteAsset(i)
            | Self::DeleteDraft(i)
            | Self::DeleteTag(i) => Some(i),
        }
    }
}

pub(super) fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(super) fn submission_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}

fn hex_case(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(super) fn owner(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 39
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

#[cfg(test)]
mod tests;
