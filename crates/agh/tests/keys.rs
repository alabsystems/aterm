// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! SSH keys: `users().keys()` over agh-fake (WP8).

use std::sync::Arc;

use agh::error::{Kind, SideEffect, Sub};
use agh::fx::Fx;
use agh::identity::{InlineToken, MapEnv, Profile};
use agh::policy::{Permit, RouteRequest, RouteTarget, Router, Tier};
use agh::retry::RetryPolicy;
use agh::wire::http::HttpTransport;
use agh::write::{Deleted, Effect};
use agh::{Github, GithubBuilder};
use agh_fake::{FakeGithub, Fault, Match, Seen};
use aterm_uds::secret::Secret;

use crate::common;

/// The title machines give a key.
const TITLE: &str = "aterm-machines:lab-07:admin";

/// The fleet's token, registered as `fixture-dev-bot`'s.
fn fleet_token(fake: &FakeGithub) -> String {
    let token = agh_fake::token("fleet");
    fake.add_token(&token, "fixture-dev-bot");
    token
}

/// An `ssh-ed25519` key whose base64 body is built from `seed` at runtime.
fn key(seed: &str) -> String {
    let mut blob = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
    let mut body = seed.as_bytes().to_vec();
    body.resize(32, b'k');
    blob.extend_from_slice(&body);
    let base64 = aterm_codec::base64::encode(&blob).expect("base64");
    format!("ssh-ed25519 {base64}")
}

/// A client over `fake` whose router is `Router::single_identity` of the inline `fleet`
/// profile bound to `login`, retrying by `retry`.
fn fleet(fake: &FakeGithub, token: &str, login: &str, retry: RetryPolicy) -> Github {
    let inline = InlineToken::new(Secret::new(token.as_bytes())).expect("an inline token");
    let profile = Profile::inline("fleet", Tier::Dev, login, inline).expect("a dev profile");
    GithubBuilder::new(Router::single_identity(profile))
        .transport(Arc::new(HttpTransport::direct(Arc::new(MapEnv::new()))))
        .endpoints(common::endpoints(fake))
        .env(Arc::new(MapEnv::new()))
        .fx(Arc::new(Fx::simulated(common::SIM_START)))
        .caller("test")
        .retry(retry)
        .build()
}

/// The identity write permit of `gh`'s single-identity router.
fn permit(gh: &Github) -> Permit {
    common::permit(gh, &RouteRequest::write(RouteTarget::Identity))
}

/// `METHOD path` of each request, the query cut.
fn requests(seen: &[Seen]) -> Vec<String> {
    seen.iter()
        .map(|s| {
            let path = s.path_and_query.split('?').next().unwrap_or_default();
            format!("{} {path}", s.method)
        })
        .collect()
}

fn count(seen: &[Seen], request: &str) -> usize {
    requests(seen).iter().filter(|r| *r == request).count()
}

#[test]
fn keys_need_a_single_identity() {
    let root = common::scratch("keys-file");
    let fake = FakeGithub::start();
    common::canonical_tokens(root.path(), &fake, true, true);
    let (_dirs, router) = common::file_router(root.path(), "");
    let gh = common::github(router, &fake);
    let error = gh
        .users()
        .keys()
        .list()
        .expect_err("a file router has no keys");
    assert_eq!(error.kind, Kind::Usage);
    assert_eq!(error.exit(), 2);
    assert!(fake.seen().is_empty(), "{:?}", requests(&fake.seen()));
}

#[test]
fn list_add_delete() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::NONE);
    let keys = gh.users().keys();
    assert!(keys.list().expect("an empty listing").is_empty());
    assert_eq!(
        requests(&fake.seen())[..2],
        ["GET /user".to_string(), "GET /user/keys".to_string()]
    );

    let permit = permit(&gh);
    let key = key("list-add-delete");
    let added = keys.add(&permit, TITLE, &key).expect("a new key");
    assert_eq!(added.effect, Effect::Created);
    assert_eq!(added.object.id, 20000);
    assert_eq!(added.object.title, TITLE);
    let held = fake.keys("fixture-dev-bot");
    assert_eq!(held.len(), 1);
    assert_eq!((held[0].id, held[0].key.as_str()), (20000, key.as_str()));

    assert_eq!(
        keys.delete(&permit, 20000).expect("removed"),
        Deleted::Removed
    );
    assert!(fake.keys("fixture-dev-bot").is_empty());
    assert_eq!(keys.delete(&permit, 20000).expect("gone"), Deleted::Gone);
}

#[test]
fn a_key_still_listed_after_a_204_is_guard_failed() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::NONE);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    keys.add(&permit, TITLE, &key("still-listed"))
        .expect("a new key");
    fake.fault_always(
        Match::api("/user/keys/").method("DELETE"),
        Fault::Status {
            status: 204,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    let before = fake.seen().len();
    let error = keys.delete(&permit, 20000).expect_err("still listed");
    assert_eq!(error.kind, Kind::Conflict);
    assert_eq!(error.sub.slug().as_deref(), Some("guard_failed"));
    assert_eq!(error.exit(), 9);
    assert!(error.why.contains("20000"), "{}", error.why);
    let after = requests(&fake.seen()[before..]);
    assert_eq!(
        after,
        [
            "DELETE /user/keys/20000".to_string(),
            "GET /user/keys".to_string()
        ]
    );
    assert_eq!(fake.keys("fixture-dev-bot").len(), 1);
}

#[test]
fn a_held_key_under_another_title_conflicts() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::NONE);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    let key = key("another-title");
    keys.add(&permit, TITLE, &key).expect("a new key");

    let before = fake.seen().len();
    let error = keys
        .add(&permit, "another-title", &key)
        .expect_err("held under the first title");
    assert_eq!(error.kind, Kind::Conflict);
    assert_eq!(error.sub, Sub::AlreadyExists);
    assert_eq!(error.exit(), 9);
    assert_eq!(
        requests(&fake.seen()[before..]),
        ["POST /user/keys".to_string(), "GET /user/keys".to_string()]
    );

    let again = keys
        .add(&permit, TITLE, &key)
        .expect("the first title's key");
    assert_eq!(again.effect, Effect::Converged);
    assert_eq!(again.object.id, 20000);
    assert_eq!(fake.keys("fixture-dev-bot").len(), 1);
}

#[test]
fn a_422_whose_re_list_fails_is_outcome_unknown() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::NONE);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    let key = key("unsettled");
    keys.add(&permit, TITLE, &key).expect("a new key");
    fake.fault_always(
        Match::api("/user/keys").method("GET"),
        Fault::Status {
            status: 502,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    let before = fake.seen().len();
    let error = keys
        .add(&permit, "another-title", &key)
        .expect_err("a re-list that fails settles nothing");
    assert_eq!(error.kind, Kind::OutcomeUnknown, "{error:?}");
    assert_eq!(
        requests(&fake.seen()[before..]),
        ["POST /user/keys".to_string(), "GET /user/keys".to_string()]
    );
}

#[test]
fn a_lost_add_converges_on_the_re_list() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    fake.fault(
        Match::api("/user/keys").method("POST"),
        Fault::LandThenClose,
    );
    let added = keys
        .add(&permit, TITLE, &key("lost"))
        .expect("the landed key");
    assert_eq!(added.effect, Effect::Converged);
    assert_eq!(count(&fake.seen(), "POST /user/keys"), 1);
    assert_eq!(fake.keys("fixture-dev-bot").len(), 1);
}

#[test]
fn a_422_after_a_lost_add_converges() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    fake.fault(
        Match::api("/user/keys").method("POST"),
        Fault::LandThenStatus { status: 502 },
    );
    fake.fault(
        Match::api("/user/keys").method("GET"),
        Fault::Status {
            status: 200,
            body: "[]".into(),
            headers: Vec::new(),
        },
    );
    let added = keys
        .add(&permit, TITLE, &key("lagged"))
        .expect("the re-list holds it");
    assert_eq!(added.effect, Effect::Converged);
    let seen = fake.seen();
    assert_eq!(count(&seen, "POST /user/keys"), 2, "{:?}", requests(&seen));
    assert_eq!(count(&seen, "GET /user/keys"), 2, "{:?}", requests(&seen));
    assert_eq!(fake.keys("fixture-dev-bot").len(), 1);
}

#[test]
fn a_guard_listing_that_fails_after_a_lost_add_is_outcome_unknown() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    fake.fault(
        Match::api("/user/keys").method("POST"),
        Fault::LandThenClose,
    );
    fake.fault_always(Match::api("/user/keys").method("GET"), Fault::Close);
    let error = keys
        .add(&permit, TITLE, &key("guard-fails"))
        .expect_err("a listing that fails settles nothing");
    assert_eq!(error.kind, Kind::OutcomeUnknown, "{error:?}");
    assert_eq!(error.exit(), 13);
    assert_eq!(error.side_effect, SideEffect::Unknown);
    assert_eq!(count(&fake.seen(), "POST /user/keys"), 1);
    assert_eq!(fake.keys("fixture-dev-bot").len(), 1);
}

#[test]
fn a_201_that_is_not_a_key_still_says_applied() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::NONE);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    fake.fault(
        Match::api("/user/keys").method("POST"),
        Fault::Status {
            status: 201,
            body: "{\"title\":\"x\"}".into(),
            headers: Vec::new(),
        },
    );
    let error = keys
        .add(&permit, TITLE, &key("undecodable"))
        .expect_err("no key in the answer");
    assert_eq!(error.side_effect, SideEffect::Applied, "{error:?}");
}

#[test]
fn a_delete_without_the_scope_names_it() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::NONE);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    keys.add(&permit, TITLE, &key("scope")).expect("a new key");
    fake.fault(
        Match::api("/user/keys/").method("DELETE"),
        Fault::Status {
            status: 404,
            body: "{\"message\":\"Not Found\"}".into(),
            headers: vec![
                ("X-OAuth-Scopes".into(), "read:public_key".into()),
                ("X-Accepted-OAuth-Scopes".into(), "admin:public_key".into()),
            ],
        },
    );
    let error = keys
        .delete(&permit, 20000)
        .expect_err("the token cannot delete");
    assert_eq!(error.kind, Kind::NotFound, "{error:?}");
    assert!(error.next.contains("admin:public_key"), "{}", error.next);
    assert_eq!(fake.keys("fixture-dev-bot").len(), 1);
}

#[test]
fn a_re_list_that_fails_after_a_204_says_applied() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::NONE);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    keys.add(&permit, TITLE, &key("relist-fails"))
        .expect("a new key");
    fake.fault_always(Match::api("/user/keys").method("GET"), Fault::Close);
    let error = keys.delete(&permit, 20000).expect_err("the re-list fails");
    assert_eq!(error.side_effect, SideEffect::Applied, "{error:?}");
    assert!(fake.keys("fixture-dev-bot").is_empty());
}

#[test]
fn a_retried_delete_whose_first_landed_is_removed() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    keys.add(&permit, TITLE, &key("landed-502"))
        .expect("a new key");
    fake.fault(
        Match::api("/user/keys/").method("DELETE"),
        Fault::LandThenStatus { status: 502 },
    );
    let before = fake.seen().len();
    assert_eq!(
        keys.delete(&permit, 20000).expect("removed"),
        Deleted::Removed
    );
    assert_eq!(count(&fake.seen()[before..], "DELETE /user/keys/20000"), 1);
    assert!(fake.keys("fixture-dev-bot").is_empty());
}

#[test]
fn a_lost_delete_whose_key_stays_listed_is_outcome_unknown() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::NONE);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    keys.add(&permit, TITLE, &key("lost-delete"))
        .expect("a new key");
    fake.fault(
        Match::api("/user/keys/").method("DELETE"),
        Fault::Status {
            status: 502,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    let before = fake.seen().len();
    let error = keys
        .delete(&permit, 20000)
        .expect_err("a 502 settles nothing");
    assert_eq!(error.kind, Kind::OutcomeUnknown, "{error:?}");
    assert_eq!(error.side_effect, SideEffect::Unknown);
    assert_eq!(
        requests(&fake.seen()[before..]),
        [
            "DELETE /user/keys/20000".to_string(),
            "GET /user/keys".to_string()
        ]
    );
    assert_eq!(fake.keys("fixture-dev-bot").len(), 1);
}

#[test]
fn a_token_without_the_scope_names_it() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    fake.set_scopes(&token, &["repo"]);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::NONE);
    let error = gh.users().keys().list().expect_err("no admin:public_key");
    assert_eq!(error.exit(), 6, "{error:?}");
    assert!(error.next.contains("admin:public_key"), "{}", error.next);
}

#[test]
fn another_login_is_login_mismatch() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "someone-else", RetryPolicy::NONE);
    let error = gh.users().keys().list().expect_err("another login");
    assert_eq!(error.kind, Kind::Unauthenticated);
    assert_eq!(error.sub.slug().as_deref(), Some("login_mismatch"));
    assert_eq!(error.exit(), 4);
    assert_eq!(requests(&fake.seen()), ["GET /user".to_string()]);
}

#[test]
fn keys_are_read_every_page() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    for i in 0..150 {
        fake.add_key(
            "fixture-dev-bot",
            &format!("key-{i}"),
            &key(&format!("page-{i}")),
        );
    }
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::NONE);
    let listed = gh.users().keys().list().expect("every page");
    assert_eq!(listed.len(), 150);
    assert_eq!(count(&fake.seen(), "GET /user/keys"), 2);
}

#[test]
fn a_422_after_a_lost_add_whose_re_list_lags_is_outcome_unknown() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    // Attempt 1 lands with its answer lost; every listing lags, so attempt 2's 422 names
    // this call's own key and the re-list cannot show it.
    fake.fault(
        Match::api("/user/keys").method("POST"),
        Fault::LandThenStatus { status: 502 },
    );
    fake.fault_always(
        Match::api("/user/keys").method("GET"),
        Fault::Status {
            status: 200,
            body: "[]".into(),
            headers: Vec::new(),
        },
    );
    let error = keys
        .add(&permit, TITLE, &key("double-lag"))
        .expect_err("a lagging re-list settles nothing");
    assert_eq!(error.kind, Kind::OutcomeUnknown, "{error:?}");
    assert_eq!(error.exit(), 13);
    assert_eq!(error.side_effect, SideEffect::Unknown);
    let seen = fake.seen();
    assert_eq!(count(&seen, "POST /user/keys"), 2, "{:?}", requests(&seen));
    let held = fake.keys("fixture-dev-bot");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].title, TITLE);
}

/// `GET /user/keys`'s body when it still lists `id` under `TITLE`.
fn stale_listing(id: u64, key: &str) -> String {
    let listing = aterm_json::json!([{"id": id, "key": key, "title": TITLE}]);
    String::from_utf8(aterm_json::to_vec(&listing).expect("a listing")).expect("UTF-8")
}

#[test]
fn a_retried_delete_s_404_is_removed() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    let key = key("retried-404");
    keys.add(&permit, TITLE, &key).expect("a new key");
    // Attempt 1 removes the key with its answer lost; the guard's listing still holds it, so
    // attempt 2 is sent and gets agh-fake's 404.
    fake.fault(
        Match::api("/user/keys/").method("DELETE"),
        Fault::LandThenStatus { status: 502 },
    );
    fake.fault(
        Match::api("/user/keys").method("GET"),
        Fault::Status {
            status: 200,
            body: stale_listing(20000, &key),
            headers: Vec::new(),
        },
    );
    let before = fake.seen().len();
    assert_eq!(
        keys.delete(&permit, 20000).expect("removed"),
        Deleted::Removed
    );
    assert_eq!(count(&fake.seen()[before..], "DELETE /user/keys/20000"), 2);
    assert!(fake.keys("fixture-dev-bot").is_empty());
}

#[test]
fn a_delete_guard_listing_that_fails_is_outcome_unknown() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    keys.add(&permit, TITLE, &key("guard-fails-delete"))
        .expect("a new key");
    fake.fault(
        Match::api("/user/keys/").method("DELETE"),
        Fault::LandThenStatus { status: 502 },
    );
    fake.fault_always(Match::api("/user/keys").method("GET"), Fault::Close);
    let before = fake.seen().len();
    let error = keys
        .delete(&permit, 20000)
        .expect_err("a listing that fails settles nothing");
    assert_eq!(error.kind, Kind::OutcomeUnknown, "{error:?}");
    assert_eq!(error.side_effect, SideEffect::Unknown);
    assert_eq!(count(&fake.seen()[before..], "DELETE /user/keys/20000"), 1);
    assert!(fake.keys("fixture-dev-bot").is_empty());
}

#[test]
fn a_guard_listing_s_404_is_not_the_delete_s() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    keys.add(&permit, TITLE, &key("guard-404"))
        .expect("a new key");
    fake.fault(
        Match::api("/user/keys/").method("DELETE"),
        Fault::Status {
            status: 502,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    fake.fault(
        Match::api("/user/keys").method("GET"),
        Fault::Status {
            status: 404,
            body: "{\"message\":\"Not Found\"}".into(),
            headers: Vec::new(),
        },
    );
    let before = fake.seen().len();
    let error = keys
        .delete(&permit, 20000)
        .expect_err("one DELETE, answered 502");
    assert_eq!(error.kind, Kind::OutcomeUnknown, "{error:?}");
    assert_eq!(error.side_effect, SideEffect::Unknown);
    assert_eq!(count(&fake.seen()[before..], "DELETE /user/keys/20000"), 1);
    assert_eq!(fake.keys("fixture-dev-bot").len(), 1);
}

#[test]
fn a_delete_404_for_an_admin_token_is_guard_failed() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::NONE);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    keys.add(&permit, TITLE, &key("admin-404"))
        .expect("a new key");
    // The token holds the one scope the DELETE needs; the accepted header lists every scope
    // that may read keys, as GitHub sends it.
    fake.fault(
        Match::api("/user/keys/").method("DELETE"),
        Fault::Status {
            status: 404,
            body: "{\"message\":\"Not Found\"}".into(),
            headers: vec![
                ("X-OAuth-Scopes".into(), "admin:public_key, repo".into()),
                (
                    "X-Accepted-OAuth-Scopes".into(),
                    "admin:public_key, read:public_key, write:public_key".into(),
                ),
            ],
        },
    );
    let error = keys
        .delete(&permit, 20000)
        .expect_err("the key is still listed");
    assert_eq!(error.kind, Kind::Conflict, "{error:?}");
    assert_eq!(error.sub.slug().as_deref(), Some("guard_failed"));
    assert_eq!(error.exit(), 9);
    assert_eq!(fake.keys("fixture-dev-bot").len(), 1);
}

#[test]
fn a_refused_delete_of_an_absent_key_is_gone() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    // Attempt 1 is refused (a 4xx removes nothing), and the guard before attempt 2 does not
    // find the id: this call removed nothing.
    fake.fault(
        Match::api("/user/keys/").method("DELETE"),
        Fault::SecondaryLimit { retry_after: 1 },
    );
    let before = fake.seen().len();
    assert_eq!(keys.delete(&permit, 99999).expect("gone"), Deleted::Gone);
    let seen = fake.seen();
    assert_eq!(
        count(&seen[before..], "DELETE /user/keys/99999"),
        1,
        "{:?}",
        requests(&seen)
    );
}

#[test]
fn a_refused_add_whose_guard_listing_fails_is_the_refusal() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    fake.fault(
        Match::api("/user/keys").method("POST"),
        Fault::SecondaryLimit { retry_after: 1 },
    );
    fake.fault_always(Match::api("/user/keys").method("GET"), Fault::Close);
    let error = keys
        .add(&permit, TITLE, &key("refused-add"))
        .expect_err("refused, then a listing that fails");
    assert_ne!(error.kind, Kind::OutcomeUnknown, "{error:?}");
    assert_eq!(error.sub, Sub::Secondary, "{error:?}");
    assert_eq!(error.side_effect, SideEffect::None);
    assert_eq!(error.http.as_ref().map(|http| http.status), Some(403));
    assert_eq!(count(&fake.seen(), "POST /user/keys"), 1);
    assert!(fake.keys("fixture-dev-bot").is_empty());
}

#[test]
fn a_422_after_a_refused_add_is_already_exists() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    let key = key("held-elsewhere");
    fake.add_key("someone-else", "theirs", &key);
    // Attempt 1 is refused, so nothing of this call landed: attempt 2's 422 names a key
    // another account holds.
    fake.fault(
        Match::api("/user/keys").method("POST"),
        Fault::SecondaryLimit { retry_after: 1 },
    );
    let error = keys
        .add(&permit, TITLE, &key)
        .expect_err("held by another account");
    assert_eq!(error.kind, Kind::Conflict, "{error:?}");
    assert_eq!(error.sub, Sub::AlreadyExists);
    assert_eq!(error.exit(), 9);
    assert_eq!(count(&fake.seen(), "POST /user/keys"), 2);
    assert!(fake.keys("fixture-dev-bot").is_empty());
}

#[test]
fn a_retried_add_of_a_key_held_under_another_title_conflicts() {
    let fake = FakeGithub::start();
    let token = fleet_token(&fake);
    let gh = fleet(&fake, &token, "fixture-dev-bot", RetryPolicy::STANDARD);
    let keys = gh.users().keys();
    let permit = permit(&gh);
    let key = key("retried-other-title");
    keys.add(&permit, TITLE, &key).expect("a new key");
    // Attempt 1's 502 may have landed, but GitHub holds a key body once: the re-list holds it
    // under the first title, so no attempt of this call wrote it.
    fake.fault(
        Match::api("/user/keys").method("POST"),
        Fault::Status {
            status: 502,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    let before = fake.seen().len();
    let error = keys
        .add(&permit, "another-title", &key)
        .expect_err("held under the first title");
    assert_eq!(error.kind, Kind::Conflict, "{error:?}");
    assert_eq!(error.sub, Sub::AlreadyExists);
    assert_eq!(count(&fake.seen()[before..], "POST /user/keys"), 2);
    assert_eq!(fake.keys("fixture-dev-bot").len(), 1);
}
