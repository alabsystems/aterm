// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Machines 5a and 5b (plan §17): the `[github] keys` row, and the key calls over agh's library
//! against agh-fake on loopback. The default row keeps the REST path, scripted on the `Fake`.

use super::github::{KeyCalls, Registered, register_via, revoke_via};
use super::inventory::{Github, KeysVia};
use crate::port::{Fake, GithubResponse, RestMethod};
use agh::identity::ProcessEnv;
use agh::wire::Endpoints;
use agh::wire::http::HttpTransport;
use agh_fake::{FakeGithub, Fault, Match, Seen};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// The machine user agh-fake knows the fleet token as.
pub(super) const LOGIN: &str = "fixture-dev-bot";

/// `machines.toml`'s `[github]` table, alone.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Row {
    github: Github,
}

/// An `ssh-ed25519` key whose base64 body is built from `seed` at runtime: `ssh-ed25519 <body>`.
fn key(seed: &str) -> String {
    let mut blob = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
    let mut body = seed.as_bytes().to_vec();
    body.resize(32, b'k');
    blob.extend_from_slice(&body);
    let base64 = aterm_codec::base64::encode(&blob).expect("base64");
    format!("ssh-ed25519 {base64}")
}

/// `user`'s key as the host reports it, with the comment machines gives it.
fn reported(user: &str, key: &str) -> (String, String) {
    (
        user.to_owned(),
        format!("{key} aterm-machines:lab-07:{user}"),
    )
}

/// agh-fake's four loopback origins.
fn endpoints(fake: &FakeGithub) -> Endpoints {
    let ports = fake.ports();
    Endpoints::loopback(ports.api, ports.uploads, ports.web, ports.cdn)
}

/// `token` as the vault's `gh:fleet` token, `sys` signed in to the scratch vault home.
pub(super) fn holds_token(sys: &Fake, token: &str) {
    super::credential::tests::in_vault(
        sys,
        aterm_secrets::need::catalog::FLEET_GH_TOKEN_ENTRY,
        Some(format!("{token}\n").as_bytes()),
    );
}

/// A `Fake` whose agh client sends to `fake`.
fn controller(fake: &FakeGithub) -> Fake {
    reaching(Fake::default(), fake)
}

/// `sys`, its agh client sending to `fake`.
fn reaching(sys: Fake, fake: &FakeGithub) -> Fake {
    sys.with_github(
        Arc::new(HttpTransport::direct(Arc::new(ProcessEnv))),
        endpoints(fake),
    )
}

/// A controller whose vault holds the `gh:fleet` token for [`LOGIN`], which agh-fake knows;
/// the token is returned for the scope test.
fn inline(fake: &FakeGithub) -> (Fake, String) {
    inline_on(Fake::default(), fake)
}

/// `sys` reaching `fake`, its vault holding the `gh:fleet` token for [`LOGIN`]: the controller
/// on which the callers' tests (`commands.rs`, `enroll.rs`) run `keys = "agh-inline"`, their
/// `machines.toml` naming [`LOGIN`].
pub(super) fn inline_on(sys: Fake, fake: &FakeGithub) -> (Fake, String) {
    let token = agh_fake::token("fleet");
    fake.add_token(&token, LOGIN);
    let sys = reaching(sys, fake);
    holds_token(&sys, &token);
    (sys, token)
}

fn row(keys: KeysVia) -> Github {
    Github {
        account: LOGIN.to_owned(),
        keys: Some(keys),
    }
}

/// `METHOD path` of each request agh-fake saw, the query cut.
pub(super) fn requests(seen: &[Seen]) -> Vec<String> {
    seen.iter()
        .map(|s| {
            let path = s.path_and_query.split('?').next().unwrap_or_default();
            format!("{} {path}", s.method)
        })
        .collect()
}

fn count(fake: &FakeGithub, request: &str) -> usize {
    requests(&fake.seen())
        .iter()
        .filter(|r| *r == request)
        .count()
}

/// The `Fake` saw no call: no REST call and no git run (the token file is read, never asked of
/// git or the login keychain).
fn no_calls(sys: &Fake) {
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
}

#[test]
fn the_default_is_gh() {
    let parsed: Row = aterm_toml::from_str("[github]\naccount = \"fleetbot\"\n").unwrap();
    assert_eq!(parsed.github.keys, None);
    assert_eq!(parsed.github.keys_via(), KeysVia::Gh);
    let written = aterm_toml::to_string(&parsed).unwrap();
    assert!(!written.contains("keys"), "{written}");
    for (spelled, via) in [("gh", KeysVia::Gh), ("agh-inline", KeysVia::AghInline)] {
        let text = format!("[github]\naccount = \"fleetbot\"\nkeys = \"{spelled}\"\n");
        let parsed: Row = aterm_toml::from_str(&text).unwrap();
        assert_eq!(parsed.github.keys_via(), via, "{spelled}");
    }
    for gone in ["agh", "agh-fleet"] {
        let text = format!("[github]\naccount = \"fleetbot\"\nkeys = \"{gone}\"\n");
        let refused = aterm_toml::from_str::<Row>(&text).unwrap_err().to_string();
        assert!(refused.contains(&format!("`{gone}`")), "{refused}");
    }

    // The REST path, scripted as github.rs's own `a_new_key_is_posted_and_one_already_held_is_
    // only_recorded`, beside an agh-fake that hears nothing.
    let fake = FakeGithub::start();
    let sys = controller(&fake);
    let token = agh_fake::token("rest");
    holds_token(&sys, &token);
    let answer = |status: u16, body: &str| {
        Ok(GithubResponse {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        })
    };
    sys.on_github_rest(
        RestMethod::Get,
        "/user",
        answer(200, r#"{"login":"fleetbot"}"#),
    );
    let alice = key("alice");
    let bob = key("bob");
    let listing =
        aterm_json::to_string(&aterm_json::json!([{"id": 100_000_102_u64, "key": bob}])).unwrap();
    sys.on_github_rest(
        RestMethod::Get,
        "/user/keys?per_page=100&page=1",
        answer(200, &listing),
    );
    sys.on_github_rest(
        RestMethod::Post,
        "/user/keys",
        answer(201, r#"{"id":100000104,"key":"ssh-ed25519 x"}"#),
    );
    let github = Github {
        account: "fleetbot".to_owned(),
        keys: None,
    };
    let done = register_via(
        &sys,
        &github,
        "lab-07",
        &[reported("alice", &alice), reported("bob", &bob)],
    )
    .unwrap();
    assert_eq!(
        done,
        [
            Registered {
                user: "alice".into(),
                id: 100_000_104
            },
            Registered {
                user: "bob".into(),
                id: 100_000_102
            },
        ]
    );
    let calls = sys.calls();
    assert_eq!(calls[0], "github GET /user");
    let posted = format!(
        r#"github POST /user/keys {{"title":"aterm-machines:lab-07:alice","key":"{alice}"}}"#
    );
    assert!(calls.contains(&posted), "{calls:?}");
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("github POST"))
            .count(),
        1
    );
    assert!(fake.seen().is_empty(), "{:?}", requests(&fake.seen()));
}

#[test]
fn inline_registers_and_revokes_over_agh() {
    let fake = FakeGithub::start();
    let (sys, token) = inline(&fake);
    let alice = key("alice");
    let bob = key("bob");
    let bob_id = fake.add_key(LOGIN, "aterm-machines:lab-07:bob", &bob);
    let github = row(KeysVia::AghInline);

    let done = register_via(
        &sys,
        &github,
        "lab-07",
        &[reported("alice", &alice), reported("bob", &bob)],
    )
    .unwrap();
    let held = fake.keys(LOGIN);
    let alice_held = held
        .iter()
        .find(|k| k.key == alice)
        .expect("alice's key registered");
    assert_eq!(alice_held.title, "aterm-machines:lab-07:alice");
    assert!(held.iter().any(|k| k.key == bob && k.id == bob_id));
    assert_eq!(held.len(), 2);
    assert_eq!(
        done,
        [
            Registered {
                user: "alice".into(),
                id: alice_held.id
            },
            Registered {
                user: "bob".into(),
                id: bob_id
            },
        ]
    );
    assert_eq!(count(&fake, "POST /user/keys"), 1);
    let seen = fake.seen();
    assert!(
        seen.iter().all(|s| s.as_login.as_deref() == Some(LOGIN)),
        "{seen:?}"
    );

    assert_eq!(
        revoke_via(&sys, &github, &[alice_held.id]),
        Ok(vec![alice.clone()])
    );
    assert_eq!(
        count(&fake, &format!("DELETE /user/keys/{}", alice_held.id)),
        1
    );
    assert_eq!(
        requests(&fake.seen())
            .iter()
            .filter(|r| r.starts_with("DELETE"))
            .count(),
        1
    );
    let left: Vec<String> = fake.keys(LOGIN).into_iter().map(|k| k.key).collect();
    assert_eq!(left, [bob]);
    // The token file read for each command, no git run, and no REST call.
    no_calls(&sys);
    assert!(!token.is_empty());
}

#[test]
fn key_writes_are_paced_one_second_apart() {
    let fake = FakeGithub::start();
    let (sys, _token) = inline(&fake);
    let done = register_via(
        &sys,
        &row(KeysVia::AghInline),
        "lab-07",
        &[
            reported("alice", &key("alice")),
            reported("bob", &key("bob")),
        ],
    )
    .unwrap();
    assert_eq!(done.len(), 2);
    assert_eq!(count(&fake, "POST /user/keys"), 2);
    assert_eq!(
        crate::port::System::github_fx(&sys).slept(),
        [Duration::from_secs(1)]
    );
}

#[test]
fn an_inline_token_of_another_login_sends_nothing() {
    let fake = FakeGithub::start();
    let token = agh_fake::token("fleet");
    fake.add_token(&token, "fixture-ext-bot");
    let sys = controller(&fake);
    holds_token(&sys, &token);
    let err = register_via(
        &sys,
        &row(KeysVia::AghInline),
        "lab-07",
        &[reported("alice", &key("alice"))],
    )
    .unwrap_err();
    // The token is named as the vault's gh:fleet; agh's own never sees it.
    assert!(err.contains("fixture-ext-bot"), "{err}");
    assert!(err.contains("gh:fleet token for"), "{err}");
    assert!(!err.contains("agh auth"), "{err}");
    assert!(!err.contains(&token), "{err}");
    assert_eq!(requests(&fake.seen()), ["GET /user"]);
    assert!(fake.keys("fixture-ext-bot").is_empty());
}

#[test]
fn an_inline_token_github_refuses_names_the_controllers_token() {
    let fake = FakeGithub::start();
    let token = agh_fake::token("unknown");
    let sys = controller(&fake);
    holds_token(&sys, &token);
    let err = revoke_via(&sys, &row(KeysVia::AghInline), &[20_000]).unwrap_err();
    assert!(err.contains("GitHub refuses the gh:fleet token"), "{err}");
    assert!(!err.contains("agh auth"), "{err}");
    assert!(!err.contains(&token), "{err}");
    assert!(
        !requests(&fake.seen())
            .iter()
            .any(|r| r.starts_with("DELETE") || r.starts_with("GET /user/keys")),
        "{:?}",
        requests(&fake.seen())
    );
}

#[test]
fn a_key_another_account_holds_leaves_the_others_registered() {
    let fake = FakeGithub::start();
    let (sys, _token) = inline(&fake);
    let alice = key("alice");
    let bob = key("bob");
    fake.add_key("fixture-ext-bot", "elsewhere", &bob);
    let registration = KeyCalls::new(&sys).register(
        &row(KeysVia::AghInline),
        "lab-07",
        &[reported("alice", &alice), reported("bob", &bob)],
    );
    let held = fake.keys(LOGIN);
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].key, alice);
    assert_eq!(
        registration.done,
        [Registered {
            user: "alice".into(),
            id: held[0].id
        }]
    );
    assert_eq!(registration.failed.len(), 1, "{:?}", registration.failed);
    assert!(
        registration.failed[0].starts_with("bob: POST /user/keys"),
        "{:?}",
        registration.failed
    );
}

#[test]
fn one_commands_key_calls_share_one_client_and_its_pacing() {
    let fake = FakeGithub::start();
    let (sys, _token) = inline(&fake);
    let github = row(KeysVia::AghInline);
    let bob_id = fake.add_key(LOGIN, "aterm-machines:lab-07:bob", &key("bob"));
    let calls = KeyCalls::new(&sys);
    let registration = calls.register(&github, "lab-07", &[reported("alice", &key("alice"))]);
    assert!(registration.failed.is_empty(), "{:?}", registration.failed);
    assert_eq!(calls.revoke(&github, &[bob_id]), Ok(vec![key("bob")]));
    // A rotation's DELETE waits out the second after its POST, and the login is bound once.
    assert_eq!(
        crate::port::System::github_fx(&sys).slept(),
        [Duration::from_secs(1)]
    );
    assert_eq!(count(&fake, "GET /user"), 1);
    assert_eq!(count(&fake, "POST /user/keys"), 1);
    assert_eq!(count(&fake, &format!("DELETE /user/keys/{bob_id}")), 1);
}

#[test]
fn a_token_without_the_scope_names_it() {
    let fake = FakeGithub::start();
    let (sys, token) = inline(&fake);
    fake.set_scopes(&token, &["repo"]);
    let bob_id = fake.add_key(LOGIN, "aterm-machines:lab-07:bob", &key("bob"));
    let github = row(KeysVia::AghInline);
    let err =
        register_via(&sys, &github, "lab-07", &[reported("alice", &key("alice"))]).unwrap_err();
    assert!(err.starts_with("GET /user/keys"), "{err}");
    assert!(err.contains("admin:public_key"), "{err}");
    // The inline token is the vault's gh:fleet, so the line names that token, never agh's own.
    assert!(err.contains("gh:fleet token for"), "{err}");
    assert!(!err.contains("agh auth"), "{err}");
    let err = revoke_via(&sys, &github, &[bob_id]).unwrap_err();
    assert!(err.starts_with("GET /user/keys"), "{err}");
    assert!(err.contains("admin:public_key"), "{err}");
    assert!(err.contains("gh:fleet token for"), "{err}");
    assert!(
        !requests(&fake.seen())
            .iter()
            .any(|r| r.starts_with("POST") || r.starts_with("DELETE")),
        "{:?}",
        requests(&fake.seen())
    );
    assert!(!err.contains(&token) && fake.keys(LOGIN).len() == 1);
}

#[test]
fn revoke_reports_a_key_still_listed() {
    let fake = FakeGithub::start();
    let (sys, _token) = inline(&fake);
    let alice = key("alice");
    let alice_id = fake.add_key(LOGIN, "aterm-machines:lab-07:alice", &alice);
    fake.fault_always(
        Match::api("/user/keys/").method("DELETE"),
        Fault::Status {
            status: 204,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    let err = revoke_via(&sys, &row(KeysVia::AghInline), &[alice_id]).unwrap_err();
    assert!(
        err.starts_with(&format!("key {alice_id}: still registered (")),
        "{err}"
    );
    assert!(
        err.contains(&format!("DELETE /user/keys/{alice_id}")),
        "{err}"
    );
    assert_eq!(count(&fake, &format!("DELETE /user/keys/{alice_id}")), 1);
    let left: Vec<String> = fake.keys(LOGIN).into_iter().map(|k| k.key).collect();
    assert_eq!(left, [alice]);
}

/// NEGATIVE CONTROL: the agh path refuses an invalid GitHub account before reading its token,
/// duplicate SSH keys shared by two host users in one `register_via` call send only one `POST
/// /user/keys` and reuse the created key id, and duplicate key ids in `revoke_via` send only one
/// `DELETE /user/keys/{id}` request.
#[test]
fn fleet_rejects_invalid_account_and_duplicate_keys_or_revoke_ids_send_once() {
    let fake = FakeGithub::start();
    let bad_fleet = Github {
        account: "bad\nlogin".to_owned(),
        keys: Some(KeysVia::AghInline),
    };
    let err = register_via(
        &controller(&fake),
        &bad_fleet,
        "lab-07",
        &[reported("alice", &key("alice"))],
    )
    .unwrap_err();
    assert!(err.contains("not a GitHub login"), "{err}");
    assert!(fake.seen().is_empty());

    let (sys, _token) = inline(&fake);
    let shared_key = key("shared");
    let github = row(KeysVia::AghInline);
    let done = register_via(
        &sys,
        &github,
        "lab-07",
        &[reported("alice", &shared_key), reported("bob", &shared_key)],
    )
    .expect("shared key registered once for both users");
    assert_eq!(done.len(), 2);
    assert_eq!(done[0].id, done[1].id);
    assert_eq!(count(&fake, "POST /user/keys"), 1);

    let key_id = done[0].id;
    let revoked = revoke_via(&sys, &github, &[key_id, key_id, key_id])
        .expect("duplicate revoke ids deduplicated");
    assert_eq!(revoked, vec![shared_key]);
    assert_eq!(count(&fake, &format!("DELETE /user/keys/{key_id}")), 1);
}
