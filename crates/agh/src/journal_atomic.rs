// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conflict domains proved by the unchanged durable request declaration.
//!
//! The key and wire records stay repository-wide for older clients. Only a
//! canonical ordinary native batch without another ref's observation guard
//! proves a smaller domain. Every unsupported or incomplete record stays global.

use super::{Intent, IntentDraft, IntentId, IntentView, IntentWriter};
use crate::error::{Error, Result};
use crate::github::refs::{ATOMIC_KEY, AtomicRefBatch, RefUpdate};
use crate::model::Sha256Digest;
use crate::slug::Slug;
use crate::write::WriteOp;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConflictDomain {
    Global,
    Refs { repo: String, names: Vec<String> },
}

impl ConflictDomain {
    pub(crate) fn draft(draft: &IntentDraft) -> Self {
        Self::of(
            draft.op,
            &draft.slug,
            &draft.key,
            &draft.request,
            draft.body_digest.as_deref(),
            &draft.refs,
        )
    }

    fn intent(intent: &Intent) -> Self {
        Self::of(
            intent.op,
            &intent.slug,
            &intent.key,
            &intent.request,
            intent.body_digest.as_deref(),
            &intent.refs,
        )
    }

    fn of(
        op: WriteOp,
        slug: &Slug,
        key: &str,
        request: &str,
        body: Option<&str>,
        refs: &[RefUpdate],
    ) -> Self {
        if op != WriteOp::RefUpdateAtomic || key != ATOMIC_KEY {
            return Self::Global;
        }
        // This constructor also refuses missing/oversized/duplicate/malformed
        // refs and control deletes. It creates no credential or execution grant.
        let Ok(batch) = AtomicRefBatch::new(refs.to_vec()) else {
            return Self::Global;
        };
        // Choosing the generic batch API cannot remove the control protocol's
        // cross-ref lease/fence invariant. Their directory conflicts stay global
        // too; a nearby lexical name is an ordinary independent ref.
        if batch.updates().iter().any(|update| {
            [
                crate::git::RELEASE_LEASE_REF,
                crate::git::PUBLISHER_FENCE_REF,
            ]
            .iter()
            .any(|control| overlaps(&update.reference, control))
        }) {
            return Self::Global;
        }
        // Journal encoding retains the canonical repository key. A historical
        // declaration with another spelling remains global rather than guessing.
        let canonical = crate::fx::canon::native_refs_text(&slug.key(), &batch, false).into_bytes();
        if request != crate::fx::canon::request_digest(&canonical)
            || body != Some(Sha256Digest::of(&canonical).to_string().as_str())
        {
            return Self::Global;
        }
        Self::Refs {
            repo: slug.key(),
            names: batch
                .updates()
                .iter()
                .map(|u| u.reference.clone())
                .collect(),
        }
    }

    pub(crate) fn contains(&self, intent: &Intent) -> bool {
        match self {
            Self::Global => true,
            Self::Refs {
                repo,
                names: wanted,
            } => match Self::intent(intent) {
                Self::Global => true,
                Self::Refs {
                    repo: held_repo,
                    names: held,
                } => {
                    repo != &held_repo || wanted.iter().any(|a| held.iter().any(|b| overlaps(a, b)))
                }
            },
        }
    }
}

fn overlaps(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|tail| tail.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|tail| tail.starts_with('/'))
}

/// A read-only judgment of one exact draft. Its private admission binding is
/// consumed by `begin`; it confers no authentication, dispatch or replay grant.
#[derive(Clone, Debug)]
pub struct AtomicJudgment {
    pub(crate) admission: AtomicAdmission,
    blocking: Option<IntentView>,
}

impl AtomicJudgment {
    pub(crate) fn global(draft: &IntentDraft, latest: Option<IntentView>) -> Self {
        let blocking = latest.as_ref().filter(|view| view.open).cloned();
        Self::new(draft, ConflictDomain::Global, latest.as_ref(), blocking)
    }

    pub(crate) fn new(
        draft: &IntentDraft,
        domain: ConflictDomain,
        latest: Option<&IntentView>,
        blocking: Option<IntentView>,
    ) -> Self {
        Self {
            admission: AtomicAdmission {
                domain,
                binding: Binding::of(draft),
                after: latest.map(|view| view.intent.id.clone()),
            },
            blocking,
        }
    }

    /// An open conflicting intent, even when a newer unrelated or closed
    /// record exists. Custom stores retain their conservative latest-key view.
    #[must_use]
    pub fn blocking(&self) -> Option<&IntentView> {
        self.blocking.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AtomicAdmission {
    domain: ConflictDomain,
    binding: Binding,
    pub(crate) after: Option<IntentId>,
}

impl AtomicAdmission {
    pub(crate) fn domain(&self, draft: &IntentDraft) -> Result<&ConflictDomain> {
        if self.binding != Binding::of(draft) || self.after != draft.after {
            return Err(Error::usage(
                "the atomic journal judgment no longer matches its exact draft",
            ));
        }
        Ok(&self.domain)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Binding {
    op: WriteOp,
    slug: Slug,
    identity: String,
    fingerprint: String,
    key: String,
    request: String,
    target: Option<String>,
    body: Option<String>,
    grant: Option<String>,
    refs: Vec<RefUpdate>,
    writer: Option<IntentWriter>,
}

impl Binding {
    fn of(draft: &IntentDraft) -> Self {
        Self {
            op: draft.op,
            slug: draft.slug.clone(),
            identity: draft.identity.clone(),
            fingerprint: draft.fingerprint.clone(),
            key: draft.key.clone(),
            request: draft.request.clone(),
            target: draft.target_commitish.clone(),
            body: draft.body_digest.clone(),
            grant: draft.grant.clone(),
            refs: draft.refs.clone(),
            writer: draft.writer.clone(),
        }
    }
}
