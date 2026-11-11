// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Owner-directed public repository creation. The ordinary repository permit
//! grants the destination; a separate exact-body capability grants this endpoint.

use super::Repos;
use crate::client::{Call, CallBody};
use crate::error::{Error, Kind, Result, SideEffect};
use crate::github::{How, encode_body};
use crate::journal::{IntentStore, IntentView, Settled};
use crate::model::{RepoObject, Sha256Digest, from_json_owned};
use crate::policy::{Permit, PermitTarget, RouteRequest, RouteTarget};
use crate::retry::{Idempotency, RetryPolicy};
use crate::slug::Slug;
use crate::wire::{Method, Origin, WireBody};
use crate::write::{Effect, Ensured, Outcome, WriteOp};

/// One bucket per prospective repository, independent of request options.
pub const CREATE_KEY: &str = "repo create";

/// Explicit visibility. Private and organization creation are not supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoVisibility {
    /// Public from its first creation; no later visibility change is made.
    Public,
}

/// Authority to create this repository with this visibility, not a general
/// permission to write `/user`. Not Clone: one routing decision grants one use.
#[derive(Debug)]
pub struct RepoCreatePermit {
    permit: Permit,
    slug: Slug,
    visibility: RepoVisibility,
}

impl RepoCreatePermit {
    /// The prospective destination.
    #[must_use]
    pub fn slug(&self) -> &Slug {
        &self.slug
    }

    /// The explicitly admitted visibility.
    #[must_use]
    pub const fn visibility(&self) -> RepoVisibility {
        self.visibility
    }

    fn body(&self) -> Result<Vec<u8>> {
        encode_body(&aterm_json::json!({
            "name": self.slug.repo(), "private": false, "auto_init": false,
        }))
    }
}

/// Transport proof, constructible only by the typed creation operation.
#[derive(Clone, Debug)]
pub(crate) struct RepoCreateWrite {
    slug: Slug,
    api: Origin,
    body: Sha256Digest,
}

impl RepoCreateWrite {
    /// Bind the permit, endpoint, method and complete non-streaming body.
    pub(crate) fn covers(
        &self,
        permit: &Permit,
        method: Method,
        origin: Option<&Origin>,
        path_and_query: &str,
        body: &WireBody<'_>,
    ) -> bool {
        method == Method::Post
            && origin == Some(&self.api)
            && path_and_query == "/user/repos"
            && matches!(permit.target(), PermitTarget::Repo(slug) if *slug == self.slug)
            && matches!(body, WireBody::Bytes(bytes) if Sha256Digest::of(bytes) == self.body)
    }
}

impl Repos<'_> {
    /// Authorize a prospective repository through the same writer, tier,
    /// literal-target and file-backed caller-grant rules as existing writes.
    ///
    /// # Errors
    /// Usage for an owner/identity route; the normal writer-policy refusals.
    pub fn permit_create(
        &self,
        route: &RouteRequest,
        visibility: RepoVisibility,
    ) -> Result<RepoCreatePermit> {
        if cfg!(not(unix)) {
            return Err(crate::write::unix_only());
        }
        let RouteTarget::Repo(slug) = &route.target else {
            return Err(Error::usage(
                "repository creation needs an explicit OWNER/NAME",
            ));
        };
        Ok(RepoCreatePermit {
            permit: self.gh.permit(route)?,
            slug: slug.clone(),
            visibility,
        })
    }

    /// Authenticate the selected writer and inspect the prospective repository.
    /// No POST, journal or identity-learning write. A missing result means only
    /// not visible; GitHub still decides whether creation is allowed.
    ///
    /// # Errors
    /// The read errors; refused for a different authenticated owner or a
    /// non-User account; conflict for an existing repository of another shape.
    pub fn check_create(&self, permit: &RepoCreatePermit) -> Result<Option<RepoObject>> {
        let user = self.gh.users().me(permit.permit.profile())?;
        if user.kind != "User" || !user.login.eq_ignore_ascii_case(permit.slug.owner()) {
            return Err(Error::refused(
                "repository creation requires the authenticated User to be the requested owner",
            )
            .with_repo(permit.slug.to_string()));
        }
        self.created_repo(permit)
    }

    /// The non-mutating preview respects the same closed history as create;
    /// an open intent is left for a real reconciliation, never advertised as a POST.
    pub(crate) fn preview_create(
        &self,
        permit: &RepoCreatePermit,
        latest: Option<&IntentView>,
    ) -> Result<Option<Ensured<RepoObject>>> {
        check_history(permit, latest)?;
        if let Some(view) = latest.filter(|view| view.open) {
            return Err(crate::write::unknown_error(
                &view.intent.id,
                Error::new(
                    Kind::OutcomeUnknown,
                    "a creation intent is open; dry-run does not reconcile or repeat it",
                )
                .with_repo(permit.slug.to_string()),
            ));
        }
        closed_result(permit, latest, self.check_create(permit)?)
    }

    fn created_repo(&self, permit: &RepoCreatePermit) -> Result<Option<RepoObject>> {
        let how = How::writer()
            .as_profile(Some(permit.permit.profile().to_string()))
            .retry_policy(RetryPolicy::NONE);
        match self.read(&permit.slug, &how) {
            Ok(repo) => {
                validate_repo(permit, &repo)?;
                Ok(Some(repo))
            }
            Err(error) if matches!(error.kind, Kind::NotFound | Kind::NotVisible) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Create once, or reconcile an earlier attempt by authenticated readback.
    /// An open intent is never retried after a missing/invisible readback.
    ///
    /// # Errors
    /// The admission, identity, read and journal errors. A failed readback after
    /// a sent request preserves Unknown rather than declaring it unapplied.
    pub fn create(
        &self,
        permit: &RepoCreatePermit,
        intents: &dyn IntentStore,
    ) -> Result<Outcome<Ensured<RepoObject>>> {
        let latest = intents.latest(WriteOp::RepoCreate, CREATE_KEY)?;
        check_history(permit, latest.as_ref())?;
        let found = match self.check_create(permit) {
            Ok(found) => found,
            Err(error) if latest.as_ref().is_some_and(|v| v.open) => {
                return Ok(still_unknown(
                    latest.as_ref().expect("an open intent"),
                    &permit.slug,
                    &format!("creation readback failed: {}", error.why),
                ));
            }
            Err(error) => return Err(error),
        };
        let body = permit.body()?;
        let digest = Sha256Digest::of(&body).to_string();
        if let Some(view) = &latest
            && view.open
        {
            if view.intent.body_digest.as_deref() != Some(&digest) {
                return Ok(still_unknown(
                    view,
                    &permit.slug,
                    "the open creation intent describes another request",
                ));
            }
            if let Some(repo) = found {
                return Ok(observed(intents, &view.intent.id, repo, &permit.slug));
            }
            return Ok(still_unknown(
                view,
                &permit.slug,
                "the repository is not visible; the earlier POST is never repeated",
            ));
        }
        if let Some(ensured) = closed_result(permit, latest.as_ref(), found)? {
            return Ok(Outcome::Done(ensured));
        }
        let mut call = Call::write(
            Method::Post,
            "/user/repos",
            RouteRequest::write(RouteTarget::Repo(permit.slug.clone())),
            Idempotency::OneShot,
            CallBody::Json(body.clone()),
        );
        call.repo_create = Some(RepoCreateWrite {
            slug: permit.slug.clone(),
            api: self.gh.endpoints().api().clone(),
            body: Sha256Digest::of(&body),
        });
        let draft = self
            .gh
            .intent_draft(
                WriteOp::RepoCreate,
                &permit.slug,
                &call,
                &permit.permit,
                CREATE_KEY,
            )?
            .with_body_digest(Some(digest))
            .with_after(latest.map(|view| view.intent.id));
        let created = |answer: &crate::client::Answered| {
            if answer.status != 201 {
                return None;
            }
            let repo: RepoObject = from_json_owned(answer.json().ok()?).ok()?;
            validate_repo(permit, &repo).ok()?;
            Some((repo.id, repo_url(&repo).to_string()))
        };
        match self
            .gh
            .one_shot_judged(&call, &permit.permit, &draft, intents, &created)?
        {
            Outcome::Done(answer) => {
                let repo = from_json_owned(answer.json()?)?;
                validate_repo(permit, &repo)?;
                Ok(done(repo, Effect::Created))
            }
            Outcome::Unknown { intent, error } => match self.created_repo(permit) {
                Ok(Some(repo)) => Ok(observed(intents, &intent, repo, &permit.slug)),
                Ok(None) => Ok(Outcome::Unknown { intent, error }),
                Err(readback) => Ok(Outcome::Unknown {
                    intent,
                    error: Error::new(
                        Kind::OutcomeUnknown,
                        format!("{}; creation readback failed: {}", error.why, readback.why,),
                    )
                    .with_repo(permit.slug.to_string())
                    .with_side_effect(SideEffect::Unknown),
                }),
            },
            Outcome::Refused(error) if error.http.as_ref().is_some_and(|h| h.status == 422) => {
                match self.created_repo(permit) {
                    Ok(Some(repo)) => Ok(done(repo, Effect::Unchanged)),
                    Ok(None) => Ok(Outcome::Refused(error)),
                    Err(readback) => Ok(Outcome::Refused(readback)),
                }
            }
            Outcome::Refused(error) => Ok(Outcome::Refused(error)),
            Outcome::NeverSent(error) => Ok(Outcome::NeverSent(error)),
        }
    }
}

fn validate_repo(permit: &RepoCreatePermit, repo: &RepoObject) -> Result<()> {
    if repo.id == 0
        || !permit.slug.eq_full_name(&repo.full_name)
        || !repo.name.eq_ignore_ascii_case(permit.slug.repo())
        || !repo.owner.login.eq_ignore_ascii_case(permit.slug.owner())
        || repo.owner.kind != "User"
        || repo.raw.get("private").and_then(|v| v.as_bool()) != Some(false)
        || repo.visibility.as_deref() != Some("public")
    {
        return Err(Error::new(
            Kind::Conflict,
            "repository metadata does not match the requested public owner/name",
        )
        .with_repo(permit.slug.to_string()));
    }
    Ok(())
}

pub(super) fn repo_url(repo: &RepoObject) -> &str {
    repo.raw
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

fn created_id(view: &IntentView) -> Option<u64> {
    view.observed
        .iter()
        .rev()
        .find_map(|(id, _)| *id)
        .or(match &view.settled {
            Some(Settled::Created { id, .. }) => Some(*id),
            _ => None,
        })
}

fn check_history(permit: &RepoCreatePermit, latest: Option<&IntentView>) -> Result<()> {
    if latest.is_some_and(|view| view.intent.slug != permit.slug) {
        return Err(Error::usage(
            "the creation intent belongs to another repository",
        ));
    }
    Ok(())
}

fn closed_result(
    permit: &RepoCreatePermit,
    latest: Option<&IntentView>,
    found: Option<RepoObject>,
) -> Result<Option<Ensured<RepoObject>>> {
    if let Some(expected) = latest.and_then(created_id) {
        return match found {
            Some(repo) if repo.id == expected => Ok(Some(Ensured {
                object: repo,
                effect: Effect::Converged,
            })),
            _ => Err(Error::new(
                Kind::Conflict,
                "the repository recorded by the creation intent is missing or replaced",
            )
            .with_repo(permit.slug.to_string())),
        };
    }
    Ok(found.map(|object| Ensured {
        object,
        effect: Effect::Unchanged,
    }))
}

fn done(repo: RepoObject, effect: Effect) -> Outcome<Ensured<RepoObject>> {
    Outcome::Done(Ensured {
        object: repo,
        effect,
    })
}

fn observed(
    intents: &dyn IntentStore,
    intent: &crate::journal::IntentId,
    repo: RepoObject,
    slug: &Slug,
) -> Outcome<Ensured<RepoObject>> {
    match intents.observe(intent, Some(repo.id)) {
        Ok(()) => done(repo, Effect::Converged),
        Err(error) => Outcome::Unknown {
            intent: intent.clone(),
            error: Error::new(
                Kind::OutcomeUnknown,
                format!(
                    "the repository is visible but its observation could not be journaled: {}",
                    error.why
                ),
            )
            .with_repo(slug.to_string())
            .with_side_effect(SideEffect::Unknown),
        },
    }
}

fn still_unknown(view: &IntentView, slug: &Slug, why: &str) -> Outcome<Ensured<RepoObject>> {
    Outcome::Unknown {
        intent: view.intent.id.clone(),
        error: Error::new(Kind::OutcomeUnknown, why)
            .with_repo(slug.to_string())
            .with_side_effect(SideEffect::Unknown),
    }
}

#[cfg(all(test, unix))]
#[path = "create_wire_scope_tests.rs"]
mod wire_scope_tests;
