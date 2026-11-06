// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! A repository-bound compare-and-swap of a batch of refs. GitHub's `updateRefs`
//! applies the whole batch or none; every update keeps its expected old OID and
//! `force: false`. No caller supplies a GraphQL query or repository node ID.

use std::cell::Cell;
use std::collections::BTreeSet;

use aterm_json::{Value, json};

use super::Refs;
use crate::client::{Call, CallBody};
use crate::error::{Error, Kind, Result, SideEffect, Sub};
use crate::github::repo_root;
use crate::journal::IntentStore;
use crate::model::{RefObject, Sha256Digest};
use crate::path;
use crate::policy::{Permit, PermitTarget, RouteRequest, RouteTarget};
use crate::retry::Idempotency;
use crate::slug::Slug;
use crate::wire::{Method, Origin, WireBody};
use crate::write::{Effect, Ensured, Outcome, WriteOp};

/// One repository-wide intent bucket: an unknown batch blocks another batch,
/// including one whose inputs differ, until its intent is reconciled.
pub(crate) const ATOMIC_KEY: &str = "atomic refs";
const ZERO: &str = "0000000000000000000000000000000000000000";
const MAX_UPDATES: usize = 128;
const MAX_REF_BYTES: usize = 1024;
const QUERY: &str = "mutation AghUpdateRefs($input: UpdateRefsInput!) { updateRefs(input: $input) { clientMutationId } }";

/// A capability minted only here, after the authenticated repository guard.
/// Other modules can carry it but cannot construct or edit its binding.
#[derive(Clone, Debug)]
pub(crate) struct AtomicRefWrite {
    slug: Slug,
    api: Origin,
    body: Sha256Digest,
}

impl AtomicRefWrite {
    /// The exact non-streaming request this capability authorizes.
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
            && path_and_query == "/graphql"
            && matches!(permit.target(), PermitTarget::Repo(slug) if slug == &self.slug)
            && matches!(body, WireBody::Bytes(bytes) if Sha256Digest::of(bytes) == self.body)
    }
}

/// One non-forced ref update. A zero `before` creates a missing ref; deletion
/// (a zero `after`) is deliberately outside this operation.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefUpdate {
    /// A fully qualified Git ref, including custom namespaces such as `refs/attest/`.
    #[serde(rename = "ref")]
    pub reference: String,
    /// The exact expected old 40-digit lowercase hexadecimal OID, or all zeroes.
    pub before: String,
    /// The nonzero new 40-digit lowercase hexadecimal OID.
    pub after: String,
}

/// A validated, canonically ordered batch of 1–128 distinct refs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomicRefBatch {
    updates: Vec<RefUpdate>,
}

impl AtomicRefBatch {
    /// Validate and order a batch without reading credentials or the network.
    ///
    /// # Errors
    /// Usage for an empty or oversized batch, duplicate refs, refs over 1024
    /// bytes or invalid Git names, malformed OIDs, or deletion requests.
    pub fn new(mut updates: Vec<RefUpdate>) -> Result<Self> {
        if updates.is_empty() || updates.len() > MAX_UPDATES {
            return Err(Error::usage("an atomic ref update needs 1–128 updates"));
        }
        let mut seen = BTreeSet::new();
        for update in &updates {
            if update.reference.len() > MAX_REF_BYTES || !path::ref_name_ok(&update.reference) {
                return Err(Error::usage(
                    "an atomic update needs fully qualified Git refs of at most 1024 bytes",
                ));
            }
            if !seen.insert(&update.reference) {
                return Err(Error::usage("an atomic update names each ref only once"));
            }
            if !path::is_sha(&update.before) || !path::is_sha(&update.after) {
                return Err(Error::usage(
                    "atomic update OIDs must be 40 lowercase hex digits",
                ));
            }
            if update.after == ZERO {
                return Err(Error::usage("an atomic ref update cannot delete a ref"));
            }
        }
        updates.sort_unstable_by(|a, b| a.reference.cmp(&b.reference));
        Ok(Self { updates })
    }

    /// The exact updates, sorted by ref name.
    #[must_use]
    pub fn updates(&self) -> &[RefUpdate] {
        &self.updates
    }

    fn variables(&self, node: &str) -> Value {
        let updates: Vec<Value> = self
            .updates
            .iter()
            .map(|u| {
                json!({
                    "name": u.reference.as_str(), "beforeOid": u.before.as_str(),
                    "afterOid": u.after.as_str(), "force": false,
                })
            })
            .collect();
        json!({"repositoryId": node, "refUpdates": updates})
    }
}

impl Refs<'_> {
    /// Run the real batch's repository and expected-OID guard reads, as the
    /// writer, without posting or opening an intent. GitHub still judges
    /// object existence, ancestry and branch protections when a real call runs.
    ///
    /// # Errors
    /// Usage for a permit belonging to another repository; the writer read's
    /// errors; Protocol for mismatched repository metadata or no node ID;
    /// Conflict when any expected ref state differs.
    pub fn check_atomic(&self, permit: &Permit, batch: &AtomicRefBatch) -> Result<()> {
        self.atomic_node(permit, batch).map(|_| ())
    }

    /// Atomically update the batch once, with a durable intent and no retry.
    /// Only a response without GraphQL errors that echoes this request's
    /// repository-and-batch digest acknowledges the mutation. A lost, malformed
    /// or error-bearing response leaves the repository-wide intent open.
    ///
    /// # Errors
    /// As `check_atomic`; the write pipeline's routing, login and journal
    /// failures. Journal failures after an acknowledgement carry an applied
    /// side effect; unacknowledged writes carry an unknown side effect.
    pub fn update_atomic(
        &self,
        permit: &Permit,
        batch: &AtomicRefBatch,
        intents: &dyn IntentStore,
    ) -> Result<Outcome<Ensured<AtomicRefBatch>>> {
        let node = self.atomic_node(permit, batch)?;
        let mut input = batch.variables(&node);
        let digest = Sha256Digest::of(&crate::github::encode_body(&input)?).to_string();
        input
            .as_object_mut()
            .expect("the input is an object")
            .insert("clientMutationId".into(), Value::from(digest.as_str()));
        let body =
            crate::github::encode_body(&json!({"query": QUERY, "variables": {"input": input}}))?;
        let scope = AtomicRefWrite {
            slug: self.slug.clone(),
            api: self.gh.endpoints().api().clone(),
            body: Sha256Digest::of(&body),
        };
        let mut call = Call::write(
            Method::Post,
            "/graphql",
            RouteRequest::write(RouteTarget::Repo(self.slug.clone())),
            Idempotency::OneShot,
            CallBody::Json(body),
        );
        call.atomic_refs = Some(scope);
        let draft = self
            .gh
            .intent_draft(
                WriteOp::RefUpdateAtomic,
                &self.slug,
                &call,
                permit,
                ATOMIC_KEY,
            )?
            .with_body_digest(Some(digest.clone()));
        let acknowledged = Cell::new(false);
        let created = |answer: &crate::client::Answered| {
            let value = answer.json().ok()?;
            if value
                .get("errors")
                .is_some_and(|e| !e.as_array().is_some_and(Vec::is_empty))
            {
                return None;
            }
            let echo = value
                .get("data")?
                .get("updateRefs")?
                .get("clientMutationId")?
                .as_str()?;
            if echo != digest {
                return None;
            }
            acknowledged.set(true);
            Some((0, String::new()))
        };
        let outcome = self
            .gh
            .one_shot(&call, permit, &draft, intents, &created)
            .map_err(|e| {
                if !acknowledged.get() && e.side_effect == SideEffect::Applied {
                    e.with_side_effect(SideEffect::Unknown)
                } else {
                    e
                }
            })?;
        Ok(match outcome {
            Outcome::Done(_) => Outcome::Done(Ensured {
                object: batch.clone(),
                effect: if batch.updates.iter().all(|u| u.before == u.after) {
                    Effect::Unchanged
                } else {
                    Effect::Created
                },
            }),
            Outcome::Unknown { intent, error } => Outcome::Unknown {
                intent,
                error: error.with_side_effect(SideEffect::Unknown),
            },
            Outcome::Refused(error) => Outcome::Refused(error),
            Outcome::NeverSent(error) => Outcome::NeverSent(error),
        })
    }

    fn atomic_node(&self, permit: &Permit, batch: &AtomicRefBatch) -> Result<String> {
        self.write_checks(permit)?;
        let reader = self.writer_view(permit);
        let repo = reader.push_repo()?;
        if !self.slug.eq_full_name(&repo.full_name) {
            return Err(
                Error::protocol("repository metadata names another repository")
                    .with_repo(self.slug.to_string()),
            );
        }
        let node = repo
            .raw
            .get("node_id")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty() && n.len() <= 4096)
            .ok_or_else(|| {
                Error::protocol("repository metadata has no valid node ID")
                    .with_repo(self.slug.to_string())
            })?;
        for update in batch.updates() {
            let found = reader.atomic_ref(&update.reference);
            match found {
                Err(e) if e.kind == Kind::NotFound && update.before == ZERO => {}
                Ok(found) if found.object.sha == update.before => {}
                Ok(_) => return Err(self.atomic_conflict()),
                Err(e) if e.kind == Kind::NotFound => return Err(self.atomic_conflict()),
                Err(e) => return Err(e),
            }
        }
        Ok(node.to_string())
    }

    fn atomic_ref(&self, reference: &str) -> Result<RefObject> {
        let name = reference.strip_prefix("refs/").expect("a validated ref");
        let encoded = path::ref_path(name);
        let call = self.how.get(
            format!("{}/git/ref/{encoded}", repo_root(&self.slug)),
            RouteTarget::Repo(self.slug.clone()),
        );
        let found = self.ref_of(&self.gh.call(&call)?)?;
        if found.reference != reference
            || !path::is_sha(&found.object.sha)
            || found.object.sha == ZERO
        {
            return Err(
                Error::protocol("the ref guard returned a different or malformed ref")
                    .with_repo(self.slug.to_string()),
            );
        }
        Ok(found)
    }

    fn atomic_conflict(&self) -> Error {
        let what = "an atomic update's expected ref state differs".to_string();
        Error::new(Kind::Conflict, what.clone())
            .with_sub(Sub::GuardFailed { what })
            .with_repo(self.slug.to_string())
    }
}

#[cfg(all(test, unix))]
mod wire_scope_tests {
    use super::*;
    use crate::identity::{Credential, MapEnv};
    use crate::policy::{Presence, Router, Tier};
    use crate::wire::door::AuthHeader;
    use crate::wire::{Deadlines, Endpoints, Phase, Transport, WireRequest};
    use std::sync::Arc;

    #[test]
    fn the_wire_capability_cannot_move_to_other_bytes_paths_origins_methods_or_permits() {
        let root = crate::test_fixtures::scratch_dir("atomic-scope");
        let text = crate::test_fixtures::canonical_text(root.path());
        let parsed = crate::config::parse(&text).expect("config");
        let valid =
            crate::config::validate(&parsed, root.path(), &crate::config::file_id).expect("config");
        let router = Router::from_validated(valid);
        let slug = Slug::parse("fixture-dev/app").expect("repo");
        let make = |slug: Slug| {
            router
                .permit(&RouteRequest::write(RouteTarget::Repo(slug)), &|_| {
                    Presence::Present
                })
                .expect("permit")
        };
        let permit = make(slug.clone());
        let other = make(Slug::parse("fixture-dev/other").expect("repo"));
        let fake = agh_fake::FakeGithub::start();
        let p = fake.ports();
        let endpoints = Endpoints::loopback(p.api, p.uploads, p.web, p.cdn);
        let credential = Credential::for_tests("dev", Tier::Dev, agh_fake::token("dev").as_bytes());
        let api_auth =
            AuthHeader::for_origin(&credential, &endpoints, endpoints.api()).expect("auth");
        let uploads_auth =
            AuthHeader::for_origin(&credential, &endpoints, endpoints.uploads()).expect("auth");
        let body = b"validated request bytes";
        let scope = AtomicRefWrite {
            slug,
            api: endpoints.api().clone(),
            body: Sha256Digest::of(body),
        };
        let transport = crate::wire::http::HttpTransport::direct(Arc::new(MapEnv::new()));
        for case in [
            "body",
            "query",
            "path",
            "origin",
            "permit",
            "method",
            "missing-scope",
            "reader",
            "no-auth",
        ] {
            let mut request = WireRequest::new(
                Method::Post,
                format!("{}/graphql", endpoints.api().base()),
                Vec::new(),
                1024,
                Deadlines::JSON,
            );
            request.auth = Some(&api_auth);
            request.permit = Some(&permit);
            request.atomic_refs = Some(&scope);
            request.body = WireBody::Bytes(body);
            let mut reader = std::io::Cursor::new(body);
            match case {
                "body" => request.body = WireBody::Bytes(b"different bytes"),
                "query" => request.url.push_str("?query=another"),
                "path" => {
                    request.url =
                        format!("{}/repos/fixture-dev/app/git/refs", endpoints.api().base())
                }
                "origin" => {
                    request.url = format!("{}/graphql", endpoints.uploads().base());
                    request.auth = Some(&uploads_auth);
                }
                "permit" => request.permit = Some(&other),
                "method" => request.method = Method::Patch,
                "missing-scope" => request.atomic_refs = None,
                "reader" => {
                    request.body = WireBody::Reader {
                        len: body.len() as u64,
                        reader: &mut reader,
                    }
                }
                "no-auth" => request.auth = None,
                _ => unreachable!(),
            }
            let error = match transport.round_trip(request) {
                Err(error) => error,
                Ok(_) => panic!("the transport sent {case}"),
            };
            assert_eq!(error.phase, Phase::NotSent, "{case}");
        }
        assert!(fake.seen().is_empty());
    }
}
