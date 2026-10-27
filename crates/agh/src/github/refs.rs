// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Git refs (design §9.4): read one, create one (OneShot, journaled), and delete one only
//! if it still points where the caller expects, through `git push --force-with-lease`
//! (`DELETE git/refs` takes no expected sha).

use crate::client::Github;
use crate::error::{Error, Result};
use crate::github::How;
use crate::journal::IntentStore;
use crate::model::RefObject;
use crate::policy::Permit;
use crate::slug::Slug;
use crate::write::{Deleted, Ensured, Outcome};

/// The refs of one repository.
pub struct Refs<'a> {
    /// The client.
    gh: &'a Github,
    /// The repository.
    slug: Slug,
    /// How its calls are routed.
    how: How,
}

impl Github {
    /// Refs.
    #[must_use]
    pub fn refs(&self, slug: &Slug) -> Refs<'_> {
        Refs {
            gh: self,
            slug: slug.clone(),
            how: How::read(),
        }
    }
}

impl<'a> Refs<'a> {
    /// The same refs under `how`.
    #[must_use]
    pub fn how(self, how: How) -> Refs<'a> {
        Refs { how, ..self }
    }

    /// `GET /repos/{o}/{r}/git/ref/{name}`, where `reference` is `refs/heads/…` or
    /// `refs/tags/…` and `{name}` is it without `refs/`.
    ///
    /// # Errors
    ///
    /// Usage for a reference outside `refs/heads/` and `refs/tags/`, or one
    /// `git check-ref-format` would refuse (the reference rule); NotFound.
    pub fn get(&self, reference: &str) -> Result<RefObject> {
        let _ = reference;
        Err(self.not_yet("ref get"))
    }

    /// `POST /repos/{o}/{r}/git/refs {ref, sha}`: OneShot, journaled (`WriteOp::RefCreate`,
    /// key the reference, `body_digest` the sha256 of the JSON body). A 422 whose message is
    /// `Reference already exists` is followed by one `get`. The same sha → `Done(Converged)`;
    /// the intent stays settled `Refused { status: 422 }`, because this POST created nothing.
    /// Another sha → Refused, Conflict `AlreadyExists`. A ref has no numeric id, so a create
    /// settles `Created { id: 0, url }`.
    ///
    /// GitHub's 422 is why `IntentStore::begin` lets a `RefCreate` begin beside an open intent
    /// of its reference: a second POST of a ref that exists cannot create a second one. On
    /// every `Done`, an older open `RefCreate` intent of the same reference is closed with
    /// `observe(.., Some(0))` only when its `body_digest` is this request's, so it asked for
    /// this same sha. An older intent for another sha stays open, so the audit ledger never
    /// records it found.
    ///
    /// # Errors
    ///
    /// Usage for `sha` that is not 40 lowercase hex; as `get`.
    pub fn create(
        &self,
        permit: &Permit,
        reference: &str,
        sha: &str,
        intents: &dyn IntentStore,
    ) -> Result<Outcome<Ensured<RefObject>>> {
        let _ = (permit, reference, sha, intents);
        Err(self.not_yet("ref create"))
    }

    /// Delete `reference` only if it points at `expect`: `GitRun::push_delete`, then one
    /// `get`. NotFound before the push → `Gone`.
    ///
    /// # Errors
    ///
    /// Conflict `GuardFailed { what: "the ref points at {sha}, not {expect}" }` when the lease
    /// is refused; the runner's errors.
    pub fn delete_expect(&self, permit: &Permit, reference: &str, expect: &str) -> Result<Deleted> {
        let _ = (permit, reference, expect);
        Err(self.not_yet("ref delete"))
    }

    /// A stub's refusal, reading every field so none is dead until WP5 writes the bodies
    /// (plan §1, rule 14).
    fn not_yet(&self, what: &str) -> Error {
        let _ = (self.gh, &self.slug, &self.how);
        Error::not_yet(what, "WP5")
    }
}
