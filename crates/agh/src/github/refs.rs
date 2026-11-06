// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Git refs (design §9.4): read one, create one (OneShot, journaled), update a
//! repository-bound batch atomically, and delete one only
//! if it still points where the caller expects, through `git push --force-with-lease`
//! (`DELETE git/refs` takes no expected sha).

use aterm_json::json;

use crate::client::{Answered, Call, CallBody, Github};
use crate::error::{Error, Kind, Result, Sub};
use crate::git::GitRun;
use crate::github::{How, repo_root};
use crate::journal::IntentStore;
use crate::model::{RefObject, from_json};
use crate::path;
use crate::policy::{Permit, RouteRequest, RouteTarget};
use crate::retry::Idempotency;
use crate::slug::Slug;
use crate::wire::Method;
use crate::write::{Deleted, Effect, Ensured, Outcome, WriteOp};

mod atomic;
mod native;
pub(crate) use atomic::ATOMIC_KEY;
pub(crate) use atomic::AtomicRefWrite;
pub use atomic::{AtomicRefBatch, RefUpdate};

/// GitHub's 422 message for a ref that exists (design §9.4).
const ALREADY_EXISTS: &str = "Reference already exists";

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
        self.check_reference(reference)?;
        let name = reference.strip_prefix("refs/").unwrap_or(reference);
        let call = self.how.get(
            format!("{}/git/ref/{}", repo_root(&self.slug), path::ref_path(name)),
            RouteTarget::Repo(self.slug.clone()),
        );
        let answered = self
            .gh
            .call(&call)
            .map_err(|e| e.with_repo(self.slug.to_string()))?;
        self.ref_of(&answered)
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
    /// Usage for `sha` that is not 40 lowercase hex; as `get`. A 2xx answer that is not a
    /// ref, and a journal failure while older intents close after a `Done`, carry
    /// `side_effect` applied. After a lost answer, a `get` or a journal step that fails is
    /// `Outcome::Unknown`, the intent open, the failure's why appended to the lost answer's;
    /// a ref found at another sha is Conflict `AlreadyExists`
    /// with `side_effect` unknown (the lost POST may have made it before it moved), next
    /// `agh intent show {repo} {intent}`, the intent open.
    pub fn create(
        &self,
        permit: &Permit,
        reference: &str,
        sha: &str,
        intents: &dyn IntentStore,
    ) -> Result<Outcome<Ensured<RefObject>>> {
        self.write_checks(permit)?;
        self.check_reference(reference)?;
        self.check_sha(sha)?;
        let body = super::encode_body(&json!({ "ref": reference, "sha": sha }))?;
        let digest = crate::model::Sha256Digest::of(&body).to_string();
        let call = Call::write(
            Method::Post,
            format!("{}/git/refs", repo_root(&self.slug)),
            RouteRequest::write(RouteTarget::Repo(self.slug.clone())),
            Idempotency::OneShot,
            CallBody::Json(body),
        );
        let draft = self
            .gh
            .intent_draft(WriteOp::RefCreate, &self.slug, &call, permit, reference)?
            .with_body_digest(Some(digest.clone()));
        let reader = self.writer_view(permit);
        let outcome = match self
            .gh
            .one_shot(&call, permit, &draft, intents, &created_ref)?
        {
            Outcome::Done(answered) => Outcome::Done(Ensured {
                object: self
                    .ref_of(&answered)
                    .map_err(|e| e.with_side_effect(crate::SideEffect::Applied))?,
                effect: Effect::Created,
            }),
            Outcome::Refused(error) if says_exists(&error) => {
                let there = reader.get(reference)?;
                if there.object.sha == sha {
                    Outcome::Done(Ensured {
                        object: there,
                        effect: Effect::Converged,
                    })
                } else {
                    Outcome::Refused(self.points_elsewhere(reference, &there.object.sha, sha))
                }
            }
            // The POST may have landed: a read or a journal step that fails after it leaves the
            // outcome unknown and the intent open, never a refusal (design §8, exit 13).
            Outcome::Unknown { intent, error } => {
                let unsettled = |failed: &Error| {
                    let mut error = error.clone();
                    let why = format!("{}; the read after it failed: {}", error.why, failed.why);
                    error.why = self.gh.redactor().apply(&why);
                    error
                };
                match reader.get(reference) {
                    Ok(there) if there.object.sha == sha => {
                        if let Err(failed) = intents.observe(&intent, Some(0)) {
                            let error = unsettled(&failed);
                            return Ok(Outcome::Unknown { intent, error });
                        }
                        Outcome::Done(Ensured {
                            object: there,
                            effect: Effect::Converged,
                        })
                    }
                    // The lost POST may have made the ref before something moved it: the
                    // Conflict says the write's effect is unknown, as a release create's
                    // ambiguous discovery does.
                    Ok(there) => {
                        intents
                            .observe(&intent, None)
                            .map_err(|e| e.with_side_effect(crate::SideEffect::Unknown))?;
                        return Err(self
                            .points_elsewhere(reference, &there.object.sha, sha)
                            .with_side_effect(crate::SideEffect::Unknown)
                            .with_next(format!("agh intent show {} {intent}", self.slug)));
                    }
                    Err(e) if e.kind == Kind::NotFound => match intents.observe(&intent, None) {
                        Ok(()) => Outcome::Unknown { intent, error },
                        Err(failed) => {
                            let error = unsettled(&failed);
                            Outcome::Unknown { intent, error }
                        }
                    },
                    Err(failed) => {
                        let error = unsettled(&failed);
                        Outcome::Unknown { intent, error }
                    }
                }
            }
            Outcome::Refused(error) => Outcome::Refused(error),
            Outcome::NeverSent(error) => Outcome::NeverSent(error),
        };
        if matches!(outcome, Outcome::Done(_)) {
            // The ref holds the sha now, so a failure here is a journal's, after a write that
            // took effect: it says so (`Applied`), and an older intent it left open is closed
            // by the next create's pass or retracted.
            let applied = |e: Error| e.with_side_effect(crate::SideEffect::Applied);
            for older in intents.open().map_err(applied)? {
                if older.op == WriteOp::RefCreate
                    && older.key == reference
                    && older.slug == self.slug
                    && older.body_digest.as_deref() == Some(digest.as_str())
                {
                    intents.observe(&older.id, Some(0)).map_err(applied)?;
                }
            }
        }
        Ok(outcome)
    }

    /// Delete `reference` only if it points at `expect`: the push check, `GitRun::push_delete`,
    /// then one `get`. NotFound before the push → `Gone`.
    ///
    /// # Errors
    ///
    /// The push check's: the repository read's error, Forbidden `NoPush` when the writer
    /// lacks push, or Forbidden `other` when the answer carries no permissions block.
    /// Conflict `GuardFailed { what: "the ref points at {sha}, not {expect}" }` when the lease
    /// is refused; the runner's errors. After a push that succeeded, a failed re-read, or a
    /// ref still there, carries `side_effect` applied.
    pub fn delete_expect(&self, permit: &Permit, reference: &str, expect: &str) -> Result<Deleted> {
        self.write_checks(permit)?;
        self.check_reference(reference)?;
        self.check_sha(expect)?;
        let reader = self.writer_view(permit);
        reader.push_check()?;
        match reader.get(reference) {
            Err(e) if e.kind == Kind::NotFound => return Ok(Deleted::Gone),
            Err(e) => return Err(e),
            Ok(there) if there.object.sha != expect => {
                return Err(self.lease_refused(&there.object.sha, expect));
            }
            Ok(_) => {}
        }
        self.gh.run_git(
            &GitRun::push_delete(&self.slug, reference, expect).with_named(permit.named()),
        )?;
        // The push succeeded, so every error after it says a write took effect.
        let applied = |e: Error| e.with_side_effect(crate::SideEffect::Applied);
        match reader.get(reference) {
            Err(e) if e.kind == Kind::NotFound => Ok(Deleted::Removed),
            Err(e) => Err(applied(e)),
            Ok(_) => Err(applied(
                self.guard_failed("the ref is still there after the push".to_string()),
            )),
        }
    }

    /// The checks every write method makes first (plan §9 body 1): unix only, and a permit
    /// for this repository (ASCII case-insensitive), else Usage with nothing sent.
    fn write_checks(&self, permit: &Permit) -> Result<()> {
        super::permit_covers(permit, &self.slug, "refs")
    }

    /// These refs read as a write's guard reads (design §11.3): WriterRead, the rate book not
    /// consulted, and the naming the permit was routed with.
    fn writer_view(&self, permit: &Permit) -> Refs<'a> {
        Refs {
            gh: self.gh,
            slug: self.slug.clone(),
            how: self
                .how
                .with_access(crate::policy::Access::WriterRead)
                .rate_book(false)
                .named(permit.named()),
        }
    }

    /// Design §6.4's push check, in the same call: `GET /repos/{o}/{r}` as these refs read,
    /// which must show `push`, so a 404 on a ref afterwards means absent, not hidden.
    /// `ref delete --dry-run` makes the same check, so it predicts the real run.
    ///
    /// # Errors
    ///
    /// The repository read's error; Forbidden `NoPush` when the answer says push is not
    /// granted, Forbidden `other` when it carries no permissions block.
    pub(crate) fn push_check(&self) -> Result<()> {
        self.push_repo().map(|_| ())
    }

    /// The repository whose writer permission the guard read established.
    fn push_repo(&self) -> Result<crate::model::RepoObject> {
        let repo = self.gh.repos().how(self.how.clone()).get(&self.slug)?;
        let (sub, why) = match &repo.permissions {
            Some(permissions) if permissions.push => return Ok(repo),
            Some(_) => (crate::error::Forbid::NoPush, "push is not granted"),
            None => (
                crate::error::Forbid::Other,
                "the answer carries no permissions block",
            ),
        };
        let meter = self.gh.meter();
        let label = meter.label();
        Err(Error::new(
            Kind::Forbidden,
            format!("as {label}: {why} on {}", self.slug),
        )
        .with_sub(Sub::Forbid(sub))
        .with_repo(self.slug.to_string()))
    }

    /// Usage unless `reference` passes the reference rule (plan §9 body 14).
    fn check_reference(&self, reference: &str) -> Result<()> {
        if crate::path::reference_ok(reference) {
            return Ok(());
        }
        Err(Error::usage(crate::path::reference_refusal(reference))
            .with_repo(self.slug.to_string()))
    }

    /// Usage unless `sha` is 40 lowercase hex.
    fn check_sha(&self, sha: &str) -> Result<()> {
        if crate::path::is_sha(sha) {
            return Ok(());
        }
        Err(Error::usage(crate::path::sha_refusal(sha)).with_repo(self.slug.to_string()))
    }

    /// The ref an answer holds.
    fn ref_of(&self, answered: &Answered) -> Result<RefObject> {
        answered
            .json()
            .and_then(|value| from_json(&value))
            .map_err(|e| e.with_repo(self.slug.to_string()))
    }

    /// Conflict `AlreadyExists`: `reference` points at `sha`, not `wanted` (a create's
    /// refusal, which `ref create --dry-run` predicts with it).
    pub(crate) fn points_elsewhere(&self, reference: &str, sha: &str, wanted: &str) -> Error {
        Error::new(
            Kind::Conflict,
            format!("{reference} points at {sha}, not {wanted}"),
        )
        .with_sub(Sub::AlreadyExists)
        .with_repo(self.slug.to_string())
    }

    /// Conflict `GuardFailed`: the delete's lease refused, the ref pointing at `there`, not
    /// `expect` (`delete_expect`'s refusal, which `ref delete --dry-run` predicts with it).
    pub(crate) fn lease_refused(&self, there: &str, expect: &str) -> Error {
        self.guard_failed(format!("the ref points at {there}, not {expect}"))
    }

    /// Conflict `GuardFailed { what }`, `what` redacted.
    fn guard_failed(&self, what: String) -> Error {
        let what = self.gh.redactor().apply(&what);
        Error::new(Kind::Conflict, what.clone())
            .with_sub(Sub::GuardFailed { what })
            .with_repo(self.slug.to_string())
    }
}

/// Whether a refused create is GitHub's 422 `Reference already exists`.
fn says_exists(error: &Error) -> bool {
    error.http.as_ref().is_some_and(|http| http.status == 422)
        && error
            .github
            .as_ref()
            .and_then(|github| github.message.as_deref())
            == Some(ALREADY_EXISTS)
}

/// A created ref's journal record: id 0 (a ref has no numeric id) and its API `url`.
fn created_ref(answered: &Answered) -> Option<(u64, String)> {
    let value = answered.json().ok()?;
    let url = value.get("url")?.as_str()?.to_string();
    Some((0, url))
}
