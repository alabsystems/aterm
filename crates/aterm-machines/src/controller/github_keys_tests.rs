// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Machines 5a and 5b (plan §17): the `[github] keys` row, and the key calls over agh's library
//! against agh-fake on loopback. The default row keeps the REST path, scripted on the `Fake`.

use super::github::{KeyCalls, Registered, TOKEN_HOST, register_via, revoke_via};
use super::inventory::{Github, KeysVia};
use crate::git_credential::script;
use crate::port::{Fake, GithubResponse, RestMethod, Stat};
use agh::config::Dirs;
use agh::identity::ProcessEnv;
use agh::wire::Endpoints;
use agh::wire::http::HttpTransport;
use agh_fake::{FakeGithub, Fault, Match, Seen};
use serde::{Deserialize, Serialize};
use std::path::Path;
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

/// A `Fake` whose agh client sends to `fake`, with CLT git for the keychain reads.
fn controller(fake: &FakeGithub) -> Fake {
    reaching(Fake::default(), fake)
}

/// `sys`, its agh client sending to `fake`, with CLT git for the keychain reads.
fn reaching(sys: Fake, fake: &FakeGithub) -> Fake {
    let sys = sys.with_github(
        Arc::new(HttpTransport::direct(Arc::new(ProcessEnv))),
        endpoints(fake),
    );
    sys.stat_of(
        crate::items::github::CLT_GIT,
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o755,
            dir: false,
            symlink: false,
            len: 1,
        },
    );
    sys
}

/// A controller whose login keychain holds the fleet token for [`LOGIN`], which agh-fake
/// knows; the token is returned for the scope test.
fn inline(fake: &FakeGithub) -> (Fake, String) {
    inline_on(Fake::default(), fake)
}

/// `sys` reaching `fake`, its login keychain holding the fleet token for [`LOGIN`]: the
/// controller on which the callers' tests (`commands.rs`, `enroll.rs`) run
/// `keys = "agh-inline"`, their `machines.toml` naming [`LOGIN`].
pub(super) fn inline_on(sys: Fake, fake: &FakeGithub) -> (Fake, String) {
    let token = agh_fake::token("fleet");
    fake.add_token(&token, LOGIN);
    let sys = reaching(sys, fake);
    script::holds(&sys, None, TOKEN_HOST, LOGIN, &token);
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

/// The `Fake`'s calls are keychain reads alone: no REST call, and no call carries `token`.
fn keychain_reads_only(sys: &Fake, token: &str) -> usize {
    let calls = sys.calls();
    let fill = script::credential(None, "fill");
    assert!(calls.iter().all(|c| *c == fill), "{calls:?}");
    assert!(calls.iter().all(|c| !c.contains(token)), "{calls:?}");
    calls.len()
}

/// Writes `bytes` to `path` (`0600`), its directory made `0700`.
fn private_file(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = path.parent().expect("a parent");
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).expect("the directory's parent");
    }
    aterm_uds::private_file::create_private_dir(dir).expect("a private directory");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .expect("a private file");
    file.write_all(bytes).expect("the file's bytes");
}

/// `agh.toml` under `dirs` with one profile, `name`, dev tier, bound to `login`, reading the
/// token file at `token_file` by its absolute path.
fn agh_toml(dirs: &Dirs, name: &str, login: &str, token_file: &Path) {
    let path = token_file
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let text = format!(
        "[profiles.{name}]\ntoken_file = \"{path}\"\ntier = \"dev\"\nlogin = \"{login}\"\n"
    );
    private_file(&dirs.config.join("agh.toml"), text.as_bytes());
}

/// `profile`'s token in a `0600` file under `root`, `{profile}.token`, known to `fake` as
/// `login`'s.
fn token_file(root: &Path, profile: &str, fake: &FakeGithub, login: &str) -> std::path::PathBuf {
    let token = agh_fake::token(profile);
    fake.add_token(&token, login);
    let file = root.join("secrets").join(format!("{profile}.token"));
    private_file(&file, format!("{token}\n").as_bytes());
    file
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("machines-keys-")
        .tempdir()
        .expect("a scratch directory")
}

#[test]
fn the_default_is_gh() {
    let parsed: Row = aterm_toml::from_str("[github]\naccount = \"fleetbot\"\n").unwrap();
    assert_eq!(parsed.github.keys, None);
    assert_eq!(parsed.github.keys_via(), KeysVia::Gh);
    let written = aterm_toml::to_string(&parsed).unwrap();
    assert!(!written.contains("keys"), "{written}");
    for (spelled, via) in [
        ("gh", KeysVia::Gh),
        ("agh-inline", KeysVia::AghInline),
        ("agh-fleet", KeysVia::AghFleet),
    ] {
        let text = format!("[github]\naccount = \"fleetbot\"\nkeys = \"{spelled}\"\n");
        let parsed: Row = aterm_toml::from_str(&text).unwrap();
        assert_eq!(parsed.github.keys_via(), via, "{spelled}");
    }
    let refused = aterm_toml::from_str::<Row>("[github]\naccount = \"fleetbot\"\nkeys = \"agh\"\n")
        .unwrap_err()
        .to_string();
    assert!(refused.contains("`agh`"), "{refused}");

    // The REST path, scripted as github.rs's own `a_new_key_is_posted_and_one_already_held_is_
    // only_recorded`, beside an agh-fake that hears nothing.
    let fake = FakeGithub::start();
    let sys = controller(&fake);
    let token = agh_fake::token("rest");
    script::holds(&sys, None, TOKEN_HOST, "fleetbot", &token);
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
    assert_eq!(calls[0], script::credential(None, "fill"));
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
    // One keychain read per command, and no REST call.
    assert_eq!(keychain_reads_only(&sys, &token), 2);
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
fn fleet_reads_agh_toml() {
    let root = scratch();
    let fake = FakeGithub::start();
    let dirs = Dirs::under(root.path());
    let file = token_file(root.path(), "fleet", &fake, LOGIN);
    agh_toml(&dirs, "fleet", LOGIN, &file);
    let sys = controller(&fake).with_agh_dirs(dirs);
    let alice = key("alice");
    let done = register_via(
        &sys,
        &row(KeysVia::AghFleet),
        "lab-07",
        &[reported("alice", &alice)],
    )
    .unwrap();
    let held = fake.keys(LOGIN);
    assert_eq!(held.len(), 1);
    assert_eq!(
        done,
        [Registered {
            user: "alice".into(),
            id: held[0].id
        }]
    );
    assert_eq!(count(&fake, "POST /user/keys"), 1);
    // The fleet token's login is read before any key call.
    assert_eq!(requests(&fake.seen())[0], "GET /user");
    assert_eq!(count(&fake, "GET /user"), 1);
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
}

#[test]
fn a_fleet_profile_that_cannot_be_used_sends_nothing() {
    let fleet = row(KeysVia::AghFleet);
    let alice = [reported("alice", &key("alice"))];
    let mut refusals: Vec<String> = Vec::new();

    let fake = FakeGithub::start();
    let sys = controller(&fake);
    let err = register_via(&sys, &fleet, "lab-07", &alice).unwrap_err();
    assert!(
        err.contains("agh-fleet") && err.ends_with("nothing was sent"),
        "{err}"
    );
    assert!(fake.seen().is_empty());
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    refusals.push(err);

    let root = scratch();
    let dirs = Dirs::under(root.path());
    let sys = controller(&fake).with_agh_dirs(dirs.clone());
    let err = register_via(&sys, &fleet, "lab-07", &alice).unwrap_err();
    let path = dirs.config.join("agh.toml").display().to_string();
    assert!(
        err.contains(&path) && err.ends_with("nothing was sent"),
        "{err}"
    );
    assert!(fake.seen().is_empty());
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    refusals.push(err);

    for (profile, login, cause) in [
        ("dev", LOGIN, "fleet"),
        ("fleet", "fixture-ext-bot", "fixture-ext-bot"),
    ] {
        let root = scratch();
        let dirs = Dirs::under(root.path());
        let file = token_file(root.path(), profile, &fake, login);
        agh_toml(&dirs, profile, login, &file);
        let sys = controller(&fake).with_agh_dirs(dirs.clone());
        let err = revoke_via(&sys, &fleet, &[20_000]).unwrap_err();
        assert!(
            err.contains(cause) && err.ends_with("nothing was sent"),
            "{err}"
        );
        assert!(
            !err.contains(&dirs.config.join("agh.toml").display().to_string()),
            "{err}"
        );
        assert!(fake.seen().is_empty(), "{:?}", requests(&fake.seen()));
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
        refusals.push(err);
    }
    // Four causes, four lines: no refusal stands in for another.
    for (at, refusal) in refusals.iter().enumerate() {
        assert!(
            refusals[at + 1..].iter().all(|other| other != refusal),
            "{refusals:?}"
        );
    }
    assert!(!refusals[0].contains(&path), "{}", refusals[0]);
}

#[test]
fn a_fleet_token_of_another_login_sends_nothing() {
    let root = scratch();
    let fake = FakeGithub::start();
    let dirs = Dirs::under(root.path());
    // agh.toml binds fleet to the machine user; the token file holds another login's token.
    let file = token_file(root.path(), "fleet", &fake, "fixture-ext-bot");
    agh_toml(&dirs, "fleet", LOGIN, &file);
    let sys = controller(&fake).with_agh_dirs(dirs);
    let fleet = row(KeysVia::AghFleet);
    let err =
        register_via(&sys, &fleet, "lab-07", &[reported("alice", &key("alice"))]).unwrap_err();
    assert!(
        err.contains("fixture-ext-bot") && err.ends_with("nothing was sent"),
        "{err}"
    );
    assert_eq!(requests(&fake.seen()), ["GET /user"]);
    let err = revoke_via(&sys, &fleet, &[20_000]).unwrap_err();
    assert!(err.contains("fixture-ext-bot"), "{err}");
    assert_eq!(requests(&fake.seen()), ["GET /user", "GET /user"]);
    assert!(fake.keys("fixture-ext-bot").is_empty());
    assert!(fake.keys(LOGIN).is_empty());
}

#[test]
fn an_inline_token_of_another_login_sends_nothing() {
    let fake = FakeGithub::start();
    let token = agh_fake::token("fleet");
    fake.add_token(&token, "fixture-ext-bot");
    let sys = controller(&fake);
    script::holds(&sys, None, TOKEN_HOST, LOGIN, &token);
    let err = register_via(
        &sys,
        &row(KeysVia::AghInline),
        "lab-07",
        &[reported("alice", &key("alice"))],
    )
    .unwrap_err();
    // The keychain's token is replaced with the keychain's verb; agh's own never sees it.
    assert!(err.contains("fixture-ext-bot"), "{err}");
    assert!(err.contains("aterm machines github login"), "{err}");
    assert!(!err.contains("agh auth"), "{err}");
    assert!(!err.contains(&token), "{err}");
    assert_eq!(requests(&fake.seen()), ["GET /user"]);
    assert!(fake.keys("fixture-ext-bot").is_empty());
}

#[test]
fn an_inline_token_github_refuses_names_the_keychain_verb() {
    let fake = FakeGithub::start();
    let token = agh_fake::token("unknown");
    let sys = controller(&fake);
    script::holds(&sys, None, TOKEN_HOST, LOGIN, &token);
    let err = revoke_via(&sys, &row(KeysVia::AghInline), &[20_000]).unwrap_err();
    assert!(err.contains("aterm machines github login"), "{err}");
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
    // The inline token lives in the login keychain, so the line names the verb that stores
    // the re-issued one, never agh's own.
    assert!(err.contains("aterm machines github login"), "{err}");
    assert!(!err.contains("agh auth"), "{err}");
    let err = revoke_via(&sys, &github, &[bob_id]).unwrap_err();
    assert!(err.starts_with("GET /user/keys"), "{err}");
    assert!(err.contains("admin:public_key"), "{err}");
    assert!(err.contains("aterm machines github login"), "{err}");
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

/// NEGATIVE CONTROL: `AghFleet` refuses an invalid GitHub account before reading `agh.toml`,
/// duplicate SSH keys shared by two host users in one `register_via` call send only one `POST
/// /user/keys` and reuse the created key id, and duplicate key ids in `revoke_via` send only one
/// `DELETE /user/keys/{id}` request.
#[test]
fn fleet_rejects_invalid_account_and_duplicate_keys_or_revoke_ids_send_once() {
    let fake = FakeGithub::start();
    let bad_fleet = Github {
        account: "bad\nlogin".to_owned(),
        keys: Some(KeysVia::AghFleet),
    };
    let err = register_via(
        &controller(&fake),
        &bad_fleet,
        "lab-07",
        &[reported("alice", &key("alice"))],
    )
    .unwrap_err();
    assert!(
        err.contains("not a GitHub login") && err.ends_with("nothing was sent"),
        "{err}"
    );
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
