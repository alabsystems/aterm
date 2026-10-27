// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! GitHub user profile (`GET /user`, `GET /users/{login}`) and rate-limit queries.

use aterm_json::Value;

use crate::client::{Call, Github};
use crate::error::{Error, Result};
use crate::github::How;
use crate::model::{KeyObject, UserObject, from_json};
use crate::path::{refuse_dot_segment, segment};
use crate::policy::{Permit, RouteRequest, RouteTarget};
use crate::rate::RateSnapshot;
use crate::write::{Deleted, Ensured};

/// The deadlines of `me_once`: connect 5 s, idle 10 s, the whole request 10 s (`--timeout`
/// replaces the backstop, as it does on every call).
#[cfg(unix)]
const LEARN_DEADLINES: crate::wire::Deadlines = crate::wire::Deadlines {
    connect: std::time::Duration::from_secs(5),
    idle: std::time::Duration::from_secs(10),
    backstop: std::time::Duration::from_secs(10),
};

/// Users and the rate limit.
pub struct Users<'a> {
    gh: &'a Github,
}

impl<'a> Users<'a> {
    /// Users over the client.
    pub(crate) const fn new(gh: &'a Github) -> Users<'a> {
        Users { gh }
    }

    /// `GET /user`, as `profile` (an Identity route).
    ///
    /// # Errors
    /// The call's error.
    pub fn me(&self, profile: &str) -> Result<UserObject> {
        let route = RouteRequest::read(RouteTarget::Identity).with_as(Some(profile.to_string()));
        let answered = self.gh.call(&Call::get("/user", route))?;
        from_json(&answered.json()?)
    }

    /// `me`, sent once under `LEARN_DEADLINES`: no retry, no secondary-limit wait. Vault
    /// adoption learns a login with it (design §6.10), and its next call asks again.
    ///
    /// # Errors
    /// The call's error.
    #[cfg(unix)]
    pub(crate) fn me_once(&self, profile: &str) -> Result<UserObject> {
        let route = RouteRequest::read(RouteTarget::Identity).with_as(Some(profile.to_string()));
        let call = Call::get("/user", route)
            .retry(Some(crate::retry::RetryPolicy::NONE))
            .deadlines(LEARN_DEADLINES);
        let answered = self.gh.call(&call)?;
        from_json(&answered.json()?)
    }

    /// `GET /users/{login}` (an Owner route, Read).
    ///
    /// # Errors
    /// Usage error for an empty or dot-segment login, or the call's error.
    pub fn get(&self, login: &str) -> Result<UserObject> {
        if login.is_empty() {
            return Err(Error::usage("an empty login: agh sends no empty path part")
                .with_next("name a GitHub login"));
        }
        refuse_dot_segment("login", login).map_err(|e| e.with_next("name a GitHub login"))?;
        let route = RouteRequest::read(RouteTarget::Owner(login.to_string()));
        let path = format!("/users/{}", segment(login));
        let answered = self.gh.call(&Call::get(path, route))?;
        from_json(&answered.json()?)
    }

    /// `GET /rate_limit`.
    ///
    /// # Errors
    /// The call's error; a protocol error when the answer lacks `resources` or an entry's `limit`,
    /// `remaining` or `reset`.
    pub fn rate(&self, who: Option<&str>) -> Result<Vec<RateSnapshot>> {
        let route = match who {
            Some(profile) => {
                RouteRequest::read(RouteTarget::Identity).with_as(Some(profile.to_string()))
            }
            None => RouteRequest::read(RouteTarget::Identity).with_anonymous(true),
        };
        let answered = self.gh.call(&Call::get("/rate_limit", route))?;
        let value = answered.json()?;
        let resources = value
            .get("resources")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                Error::protocol("GitHub's /rate_limit answer has no resources object")
            })?;
        let mut snapshots = Vec::with_capacity(resources.len());
        for (resource, entry) in resources {
            let number = |key: &str| entry.get(key).and_then(Value::as_u64);
            let (Some(limit), Some(remaining), Some(reset)) =
                (number("limit"), number("remaining"), number("reset"))
            else {
                return Err(Error::protocol(format!(
                    "GitHub's /rate_limit entry {resource} lacks limit, remaining or reset"
                )));
            };
            snapshots.push(RateSnapshot {
                resource: resource.clone(),
                limit,
                remaining,
                reset,
                used: number("used").unwrap_or(limit.saturating_sub(remaining)),
            });
        }
        snapshots.sort_by(|a, b| a.resource.cmp(&b.resource));
        Ok(snapshots)
    }
}

/// The SSH keys of the identity a `single_identity` router speaks as (design §10.2).
pub struct Keys<'a> {
    /// The client.
    gh: &'a Github,
    /// How its calls are routed.
    how: How,
}

impl<'a> Users<'a> {
    /// `/user/keys`.
    #[must_use]
    pub fn keys(&self) -> Keys<'a> {
        Keys {
            gh: self.gh,
            how: How::read(),
        }
    }
}

impl Keys<'_> {
    /// `GET /user/keys`, every page, as Read, deduplicated by id.
    ///
    /// # Errors
    ///
    /// Usage `keys need Router::single_identity` on any other router, before anything is
    /// sent; the read's errors, with `GithubBody`'s scope headers kept.
    pub fn list(&self) -> Result<Vec<KeyObject>> {
        Err(self.not_yet("keys list"))
    }

    /// `POST /user/keys {title, key}`: Converging. After any answer that is not a 201, or no
    /// answer, it re-lists once (design §10.2), and a key with the same title and key body →
    /// `Converged`. A 422 `key is already in use` whose re-list holds no such key → Conflict
    /// `AlreadyExists`: another title or another account holds the key. So when an earlier
    /// attempt landed with its answer lost, and the guard's listing before the next attempt
    /// lagged, that attempt's 422 still converges.
    ///
    /// # Errors
    ///
    /// As `list`; OutcomeUnknown when the re-list does not settle it.
    pub fn add(&self, permit: &Permit, title: &str, key: &str) -> Result<Ensured<KeyObject>> {
        let _ = (permit, title, key);
        Err(self.not_yet("keys add"))
    }

    /// `DELETE /user/keys/{id}`. A 204 or a 404 re-lists once: after a 204, absent →
    /// `Removed`; after a 404, absent → `Gone`; present after either → Conflict `GuardFailed`,
    /// why `key {id} is still listed after its DELETE`, and no DELETE is sent after a 204.
    ///
    /// # Errors
    ///
    /// As `list`; Conflict `GuardFailed` when the re-list still holds the id.
    pub fn delete(&self, permit: &Permit, id: u64) -> Result<Deleted> {
        let _ = (permit, id);
        Err(self.not_yet("keys delete"))
    }

    /// A stub's refusal, reading every field so none is dead until WP8 writes the bodies
    /// (plan §1, rule 14).
    fn not_yet(&self, what: &str) -> Error {
        let _ = (self.gh, &self.how);
        Error::not_yet(what, "WP8")
    }
}
