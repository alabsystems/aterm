// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! GitHub user profile (`GET /user`, `GET /users/{login}`), rate-limit queries, and the SSH
//! keys of a single identity (`/user/keys`, design §10.2).

use aterm_json::Value;

use crate::client::{Answered, Call, CallBody, Ended, Github};
use crate::error::{Error, IdentityRef, Kind, Result, SideEffect, Sub};
use crate::github::How;
use crate::model::{KeyObject, UserObject, from_json_owned};
use crate::page::{Consistency, LinkScope, ListOpts};
use crate::path::{refuse_dot_segment, segment};
use crate::policy::{Permit, RouteRequest, RouteTarget};
use crate::rate::RateSnapshot;
use crate::retry::Idempotency;
use crate::wire::Method;
use crate::write::{Deleted, Effect, Ensured, WriteOp};

/// The deadlines of `me_once`: connect 5 s, idle 10 s, the whole request 10 s (`--timeout`
/// replaces the backstop, as it does on every call).
#[cfg(unix)]
const LEARN_DEADLINES: crate::wire::Deadlines = crate::wire::Deadlines {
    connect: std::time::Duration::from_secs(5),
    idle: std::time::Duration::from_secs(10),
    backstop: Some(std::time::Duration::from_secs(10)),
};

/// The bound a test set on each of `LEARN_DEADLINES`, in milliseconds (0: none set).
#[cfg(all(unix, any(test, debug_assertions)))]
static LEARN_DEADLINES_FOR_TEST_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Bound `me_once`'s connect, idle and whole request by `bound` instead of `LEARN_DEADLINES`,
/// for this process.
///
/// A test whose loopback GitHub must answer `GET /user` sets the hang detector here: a loaded
/// machine that outlasts the product's 10 s would otherwise read the fake as unreachable.
#[cfg(all(unix, any(test, debug_assertions)))]
#[doc(hidden)]
pub fn set_learn_deadlines_for_test(bound: std::time::Duration) {
    let ms = u64::try_from(bound.as_millis()).unwrap_or(u64::MAX).max(1);
    LEARN_DEADLINES_FOR_TEST_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

/// `LEARN_DEADLINES`, or the bound a test set with [`set_learn_deadlines_for_test`].
#[cfg(unix)]
fn learn_deadlines() -> crate::wire::Deadlines {
    #[cfg(any(test, debug_assertions))]
    match LEARN_DEADLINES_FOR_TEST_MS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {}
        ms => {
            let bound = std::time::Duration::from_millis(ms);
            return crate::wire::Deadlines {
                connect: bound,
                idle: bound,
                backstop: Some(bound),
            };
        }
    }
    LEARN_DEADLINES
}

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
        from_json_owned(answered.json()?)
    }

    /// `me`, sent once under `learn_deadlines()`: no retry, no secondary-limit wait. Vault
    /// adoption learns a login with it (design §6.10), and its next call asks again.
    ///
    /// # Errors
    /// The call's error.
    #[cfg(unix)]
    pub(crate) fn me_once(&self, profile: &str) -> Result<UserObject> {
        let route = RouteRequest::read(RouteTarget::Identity).with_as(Some(profile.to_string()));
        let call = Call::get("/user", route)
            .retry(Some(crate::retry::RetryPolicy::NONE))
            .deadlines(learn_deadlines());
        let answered = self.gh.call(&call)?;
        from_json_owned(answered.json()?)
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
        from_json_owned(answered.json()?)
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
        snapshots.sort_unstable_by(|a, b| a.resource.cmp(&b.resource));
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
        self.single_identity()?;
        self.read(&self.how)
    }

    /// `POST /user/keys {title, key}`: Converging, with a guard listing before every attempt
    /// after the first, which ends the loop `Converged` when it finds a key with the same
    /// title and key body. After any answer that is not a 201, or no answer, it re-lists once
    /// (design §10.2), and such a key → `Converged`. So when an earlier attempt landed with
    /// its answer lost, and the guard's listing before the next attempt lagged, that
    /// attempt's 422 still converges. An attempt may have landed when its failure's side
    /// effect is unknown (no answer read, a 5xx); one GitHub refused with a 4xx (a secondary
    /// limit's 403 included) or that was never sent did not. Once an earlier attempt may have
    /// landed, whatever the last one says (a 422 included), a guard listing that fails, or a
    /// last attempt that was never sent, is settled by the re-list, and a re-list that lacks
    /// the key is OutcomeUnknown, never answered as nothing written. When no earlier attempt
    /// may have landed, a guard listing that fails ends the call with the attempt before it,
    /// as GitHub answered it. A 422 `key is already in use` whose re-list holds no key with
    /// the same title and body → Conflict `AlreadyExists` (another title or another account
    /// holds the key) when no attempt may have landed, and whenever the re-list holds the key
    /// body under another title, which no attempt of this call can have written.
    ///
    /// # Errors
    ///
    /// - As `list` (Usage on a router that is not single-identity, before anything is sent);
    /// - could_not_run, why `agh writes need the write journal, which is unix-only in M2`,
    ///   off unix;
    /// - Conflict `AlreadyExists`, why `the key is already on a GitHub account`, as above;
    /// - OutcomeUnknown, why `outcome unknown: key.add on /user/keys; the guard read could
    ///   not settle it: {detail}`, when an attempt may have landed and the re-list does not
    ///   settle it (it fails, or lacks the key), and when a 422 `key is already in use`'s
    ///   re-list fails;
    /// - GitHub's other refusals of the POST, and an attempt that was never sent, returned as
    ///   they came, side effect none, when no attempt may have landed;
    /// - a 201 whose body is not a key: that error with side effect applied.
    pub fn add(&self, permit: &Permit, title: &str, key: &str) -> Result<Ensured<KeyObject>> {
        self.single_identity()?;
        if cfg!(not(unix)) {
            return Err(crate::write::unix_only());
        }
        let body = super::encode_body(&aterm_json::json!({"key": key, "title": title}))?;
        let call = Call::write(
            Method::Post,
            KEYS_PATH,
            How::write().route(RouteTarget::Identity),
            Idempotency::Converging,
            CallBody::Json(body),
        );
        let guard = self.guard_how();
        let wanted_body = key_body(key);
        let ours = |held: &KeyObject| held.title == title && key_body(&held.key) == wanted_body;
        let mut found: Option<KeyObject> = None;
        let sent = self.attempts(&call, permit, &mut || {
            let listed = self.read(&guard)?;
            Ok(match listed.into_iter().find(|held| ours(held)) {
                Some(held) => {
                    found = Some(held);
                    true
                }
                None => false,
            })
        });
        if let Some(object) = found {
            return Ok(Ensured {
                object,
                effect: Effect::Converged,
            });
        }
        let earlier = sent.earlier;
        let error = match sent.result {
            Ok(answered) if answered.status == 201 => {
                // GitHub created the key: an answer agh cannot read does not unwrite it.
                let object = answered
                    .json()
                    .and_then(from_json_owned)
                    .map_err(|error| error.with_side_effect(SideEffect::Applied))?;
                return Ok(Ensured {
                    object,
                    effect: Effect::Created,
                });
            }
            Ok(answered) => Error::new(
                Kind::OutcomeUnknown,
                format!(
                    "GitHub answered {} where a new key is a 201",
                    answered.status
                ),
            )
            .with_side_effect(SideEffect::Unknown),
            // A last attempt that was never sent proves nothing was written only when no
            // earlier attempt may have landed.
            Err(error) if !earlier && never_sent(&error) => return Err(error),
            Err(error) => error,
        };
        let may_have_landed = earlier || lost(&error);
        // Design §10.2: any answer that is not a 201, or none, re-lists once.
        let mut relisted = match self.read(&guard) {
            Ok(keys) => keys,
            // A 422 names a key that may be ours, from an earlier attempt whose answer was
            // lost: only a re-list can tell, so a failed one settles nothing.
            Err(relist) if already_in_use(&error) || may_have_landed => {
                let detail = format!(
                    "the key's POST may have landed ({}), and the re-list failed: {}",
                    error.why, relist.why
                );
                return Err(unsettled(error, WriteOp::KeyAdd, &detail));
            }
            Err(_) => return Err(error),
        };
        if let Some(found) = relisted.iter().position(ours) {
            return Ok(Ensured {
                object: relisted.swap_remove(found),
                effect: Effect::Converged,
            });
        }
        // GitHub holds a key body once: under another title, no attempt of this call wrote it.
        if already_in_use(&error)
            && relisted
                .iter()
                .any(|held| key_body(&held.key) == wanted_body)
        {
            return Err(already_held(error));
        }
        // Before the 422's rule: once an earlier attempt may have landed, a 422 may name the
        // key it landed, and a re-list that lags hides it, so nothing here proves nothing was
        // written.
        if may_have_landed {
            let detail = format!(
                "the key's POST may have landed ({}), and the re-list does not hold it",
                error.why
            );
            return Err(unsettled(error, WriteOp::KeyAdd, &detail));
        }
        if already_in_use(&error) {
            return Err(already_held(error));
        }
        Err(error)
    }

    /// `DELETE /user/keys/{id}`, Converging, with a guard listing before every attempt after
    /// the first. An attempt may have landed when its failure's side effect is unknown (no
    /// answer read, a 5xx); one GitHub refused with a 4xx (a secondary limit's 403 included)
    /// or that was never sent did not. An id the guard no longer finds ends the loop:
    /// `Removed` when an earlier attempt may have removed it, else `Gone`. A 204 or a 404
    /// re-lists once: after a 204, absent → `Removed`; after a 404, absent → `Gone`, or
    /// `Removed` when an earlier attempt may have removed it; present after either →
    /// Conflict `GuardFailed`, why `key {id} is still listed after its DELETE`, and no DELETE
    /// is sent after a 204. Only the DELETE's own 404 counts: a guard listing's 404 is a
    /// guard failure. A DELETE 404 that carries both scope headers, and whose
    /// `x-oauth-scopes` lacks `admin:public_key` (no other scope implies it), with the key
    /// still listed, is that 404's own error, its next naming `admin:public_key`. Any other
    /// answer that may have landed (a 5xx, no answer, or any failure once an earlier attempt
    /// may have landed) re-lists once too: absent → `Removed`. When no earlier attempt may
    /// have landed, a guard listing that fails ends the call with the attempt before it, as
    /// GitHub answered it.
    ///
    /// # Errors
    ///
    /// - As `list` (Usage on a router that is not single-identity, before anything is sent);
    /// - could_not_run, why `agh writes need the write journal, which is unix-only in M2`,
    ///   off unix;
    /// - Conflict `GuardFailed` when the re-list still holds the id after a 204 or a 404;
    /// - NotFound with a scope `next` when that 404 carries both scope headers, its
    ///   `x-oauth-scopes` lacks `admin:public_key`, and the key is still listed;
    /// - OutcomeUnknown, why `outcome unknown: key.delete on /user/keys; the guard read could
    ///   not settle it: {detail}`, when a DELETE may have landed and the re-list fails or
    ///   still holds the id;
    /// - GitHub's other refusals of the DELETE (a 401, 403 or 422), and an attempt that was
    ///   never sent, returned as they came, side effect none, when no attempt may have landed;
    /// - a re-list that fails after a 204: its error with side effect applied (after a 404,
    ///   unknown when an earlier attempt may have landed, else as it came).
    pub fn delete(&self, permit: &Permit, id: u64) -> Result<Deleted> {
        self.single_identity()?;
        if cfg!(not(unix)) {
            return Err(crate::write::unix_only());
        }
        let call = Call::write(
            Method::Delete,
            format!("{KEYS_PATH}/{id}"),
            How::write().route(RouteTarget::Identity),
            Idempotency::Converging,
            CallBody::Empty,
        );
        let guard = self.guard_how();
        let mut absent = false;
        let sent = self.attempts(&call, permit, &mut || {
            let listed = self.read(&guard)?;
            absent = !listed.iter().any(|held| held.id == id);
            Ok(absent)
        });
        let earlier = sent.earlier;
        if absent {
            // Only an attempt that may have landed can have removed it.
            return Ok(if earlier {
                Deleted::Removed
            } else {
                Deleted::Gone
            });
        }
        // What the DELETE did, the side effect a still-listed key's Conflict carries, the side
        // effect a failed re-list carries, and the 404's own error.
        let (deleted, listed_effect, relist_effect, refused) = match sent.result {
            Ok(_) => (
                Deleted::Removed,
                SideEffect::Unknown,
                SideEffect::Applied,
                None,
            ),
            Err(error) if is_delete_404(&error) && earlier => (
                Deleted::Removed,
                SideEffect::Unknown,
                SideEffect::Unknown,
                Some(error),
            ),
            Err(error) if is_delete_404(&error) => (
                Deleted::Gone,
                SideEffect::None,
                SideEffect::None,
                Some(error),
            ),
            Err(error) if !earlier && (never_sent(&error) || !lost(&error)) => return Err(error),
            Err(error) => return self.settle_lost_delete(&guard, id, error),
        };
        let listed = self.read(&guard).map_err(|read| {
            if relist_effect == SideEffect::None {
                read
            } else {
                read.with_side_effect(relist_effect)
            }
        })?;
        if listed.iter().any(|held| held.id == id) {
            // A token that can list keys but lacks admin:public_key gets a 404 for the
            // DELETE: that 404 names the scope to add, which a Conflict would hide.
            if let Some(refused) = refused
                && lacks_key_admin(&refused)
            {
                return Err(refused
                    .with_side_effect(listed_effect)
                    .with_next(crate::classify::scope_next(KEY_ADMIN)));
            }
            let why = format!("key {id} is still listed after its DELETE");
            return Err(Error::new(Kind::Conflict, why)
                .with_sub(Sub::GuardFailed {
                    what: format!("key {id} listed"),
                })
                .with_side_effect(listed_effect)
                .with_next(format!(
                    "list the keys again (GitHub may still be removing key {id}); a key that \
                     stays listed is deleted on GitHub's settings page"
                )));
        }
        Ok(deleted)
    }

    /// A key DELETE that may have landed with its answer lost (`error`): one re-list settles
    /// it. Absent → `Removed`; present, or a re-list that fails → OutcomeUnknown.
    fn settle_lost_delete(&self, guard: &How, id: u64, error: Error) -> Result<Deleted> {
        let detail = match self.read(guard) {
            Ok(listed) if !listed.iter().any(|held| held.id == id) => return Ok(Deleted::Removed),
            Ok(_) => format!(
                "the DELETE of key {id} may have landed ({}), and the re-list still holds it",
                error.why
            ),
            Err(relist) => format!(
                "the DELETE of key {id} may have landed ({}), and the re-list failed: {}",
                error.why, relist.why
            ),
        };
        Err(unsettled(error, WriteOp::KeyDelete, &detail))
    }

    /// A Converging key write's attempts: `Github::converging`'s, as `Releases::converge` runs
    /// them, so what each failed attempt may have written is known (its side effect unknown).
    /// `guard` runs before every attempt after the first: `Ok(true)` ends the loop (the
    /// write's state is already right), `Ok(false)` sends. A guard that fails once an earlier
    /// attempt may have landed ends the loop with its error at side effect unknown; else with
    /// the attempt before it, as GitHub answered it.
    fn attempts(
        &self,
        call: &Call,
        permit: &Permit,
        guard: &mut dyn FnMut() -> Result<bool>,
    ) -> Attempts {
        let sent = self.gh.converging(call, permit, &self.how, &mut |look| {
            if look.attempt == 1 {
                return Ok(None);
            }
            Ok(guard()?.then(converged_marker))
        });
        let earlier = sent.earlier;
        let result = match (sent.result, sent.ended) {
            (Err(error), Ended::Guard) => Err(match sent.previous {
                Some(previous) if !earlier => previous,
                _ => possibly_landed(error),
            }),
            (result, _) => result,
        };
        Attempts { result, earlier }
    }

    /// Usage `keys need Router::single_identity` unless the client's router is one.
    fn single_identity(&self) -> Result<()> {
        if self.gh.router().is_single_identity() {
            Ok(())
        } else {
            Err(Error::usage("keys need Router::single_identity")
                .with_next("build the client over Router::single_identity(profile)"))
        }
    }

    /// The reads that guard a key write: this routing, the rate book not consulted (design
    /// §7.4, §11.3).
    fn guard_how(&self) -> How {
        self.how.clone().rate_book(false)
    }

    /// Every page of `GET /user/keys` under `how`, as read, the first of each id kept.
    fn read(&self, how: &How) -> Result<Vec<KeyObject>> {
        let first = how.get(KEYS_PATH, RouteTarget::Identity);
        let opts = ListOpts::STANDARD.with_consistency(Consistency::AsRead);
        let walked = crate::page::walk(self.gh, first, &opts, &LinkScope::Exact)?;
        let listing = walked.map(from_json_owned::<KeyObject>)?;
        let mut seen = std::collections::BTreeSet::new();
        Ok(listing
            .items
            .into_iter()
            .filter(|held| seen.insert(held.id))
            .collect())
    }
}

/// The SSH keys' path.
const KEYS_PATH: &str = "/user/keys";

/// A key's type and base64, its comment dropped: what GitHub keeps of a key.
fn key_body(key: &str) -> (&str, &str) {
    let mut parts = key.split_whitespace();
    (parts.next().unwrap_or(""), parts.next().unwrap_or(""))
}

/// What a key write's attempts came to (`Keys::attempts`).
struct Attempts {
    /// The last attempt's answer or failure; the guard's marker when it ended the loop; a
    /// failed guard's error, or the attempt before it when nothing may have landed.
    result: Result<Answered>,
    /// Whether an attempt before the one `result` answers may have written.
    earlier: bool,
}

/// The answer a key write's guard ends its loop with once it found the state right: `add` and
/// `delete` read what the guard found, never this answer.
fn converged_marker() -> Answered {
    Answered {
        status: 200,
        headers: Vec::new(),
        body: Vec::new(),
        as_: IdentityRef::Anonymous,
        request_id: None,
        etag: None,
        moved: None,
        requests: 0,
        rate: None,
    }
}

/// Whether `error` proves nothing was sent (`write::settle_table`'s NeverSent rule).
fn never_sent(error: &Error) -> bool {
    error.side_effect == SideEffect::None
        && error.http.is_none()
        && error.kind != Kind::OutcomeUnknown
}

/// Whether the write may have landed with its answer lost: no answer, a 5xx, or an unknown
/// side effect.
fn lost(error: &Error) -> bool {
    error.kind == Kind::OutcomeUnknown
        || error.side_effect == SideEffect::Unknown
        || error.http.as_ref().is_none_or(|http| http.status >= 500)
}

/// Whether `error` is GitHub's 422 naming a key already in use.
fn already_in_use(error: &Error) -> bool {
    error.http.as_ref().is_some_and(|http| http.status == 422)
        && error.github.as_ref().is_some_and(|body| {
            body.errors.iter().any(|field| {
                field
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("key is already in use"))
            })
        })
}

/// Conflict `AlreadyExists`: another title or another account holds the key.
fn already_held(error: Error) -> Error {
    error
        .recast(Kind::Conflict, "the key is already on a GitHub account")
        .with_sub(Sub::AlreadyExists)
        .with_next("the key is held under another title or by another account: remove it there, or add another key")
}

/// OutcomeUnknown: a lost `op` (`KeyAdd` or `KeyDelete`) the re-list could not settle. The why
/// is §4.6.7's, `/user/keys` in place of the repository (a key has none), then `detail`.
fn unsettled(error: Error, op: WriteOp, detail: &str) -> Error {
    let verb = if op == WriteOp::KeyDelete {
        "delete"
    } else {
        "add"
    };
    let why = format!(
        "outcome unknown: {} on {KEYS_PATH}; the guard read could not settle it: {detail}",
        op.slug()
    );
    error
        .recast(Kind::OutcomeUnknown, why)
        .with_side_effect(SideEffect::Unknown)
        .with_next(format!("list the keys again before another {verb}"))
}

/// A guard listing's failure once an earlier attempt may have landed: the write's state is
/// unknown, so the error never says nothing was written.
fn possibly_landed(read: Error) -> Error {
    read.with_side_effect(SideEffect::Unknown)
}

/// Whether `error` is GitHub's 404 to the DELETE itself, not to a guard listing.
fn is_delete_404(error: &Error) -> bool {
    error
        .http
        .as_ref()
        .is_some_and(|http| http.status == 404 && http.method == Method::Delete.as_str())
}

/// The scope a key DELETE needs.
const KEY_ADMIN: &str = "admin:public_key";

/// Whether `error`'s answer carried both scope headers and its `x-oauth-scopes` lacks
/// `admin:public_key`. The DELETE needs that one scope, and none implies it (`admin:X`
/// implies `write:X` and `read:X`, never the reverse), so `x-accepted-oauth-scopes`, which
/// lists every scope that may read keys, is not read as a list the token must hold all of.
fn lacks_key_admin(error: &Error) -> bool {
    error.github.as_ref().is_some_and(|body| {
        body.accepted_oauth_scopes.is_some()
            && body
                .oauth_scopes
                .as_deref()
                .is_some_and(|held| !crate::classify::scopes(held).any(|scope| scope == KEY_ADMIN))
    })
}
