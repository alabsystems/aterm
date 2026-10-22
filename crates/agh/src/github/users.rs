// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Users and the rate limit (design §6.7, §13.2): `GET /user` as one profile, `GET /users/{login}`
//! and the free `GET /rate_limit`.

use aterm_json::Value;

use crate::client::{Call, Github};
use crate::error::{Error, Result};
use crate::model::{UserObject, from_json};
use crate::path::{refuse_dot_segment, segment};
use crate::policy::{RouteRequest, RouteTarget};
use crate::rate::RateSnapshot;

/// Users and the rate limit.
pub struct Users<'a> {
    gh: &'a Github,
}

impl<'a> Users<'a> {
    /// Users over the client.
    pub(crate) fn new(gh: &'a Github) -> Users<'a> {
        Users { gh }
    }

    /// `GET /user`, as `profile` (an Identity route).
    ///
    /// # Errors
    ///
    /// The call's error.
    pub fn me(&self, profile: &str) -> Result<UserObject> {
        let route = RouteRequest::read(RouteTarget::Identity).with_as(Some(profile.to_string()));
        let answered = self.gh.call(&Call::get("/user", route))?;
        from_json(&answered.json()?)
    }

    /// `GET /users/{login}` (an Owner route, Read).
    ///
    /// # Errors
    ///
    /// Usage, before any request, for a login that is empty or a dot-segment (`.`, `..`),
    /// which a server that normalises paths would resolve off this Owner route
    /// (`path::has_dot_segment`'s contract); otherwise the call's error.
    pub fn get(&self, login: &str) -> Result<UserObject> {
        // `segment` encodes `/`, so only the whole text can be a dot-segment. A bot's login
        // (`name[bot]`) is no owner name, yet `/users/` answers it, so the owner rule is not
        // applied.
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

    /// `GET /rate_limit`: `None` anonymous, `Some(p)` as `p` (an Identity route). One snapshot
    /// per `resources` entry, sorted by resource. `/rate_limit` costs no quota.
    ///
    /// # Errors
    ///
    /// The call's error; CouldNotRun `Protocol` when the answer has no `resources` object or
    /// an entry lacks a numeric `limit`, `remaining` or `reset`.
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
