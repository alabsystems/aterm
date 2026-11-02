// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The loopback git smart-HTTP endpoints: advertisements, Basic credentials and the
//! delete-only receive-pack (WP2).

use agh_fake::{FakeGithub, RepoSpec, basic, token};

use crate::raw::{Reply, Req, plain};

const SLUG: &str = "fixture-dev/app";
const MAIN_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const V1_SHA: &str = "89abcdef0123456789abcdef0123456789abcdef";
const ZERO: &str = "0000000000000000000000000000000000000000";
const RECEIVE: &str = "/fixture-dev/app.git/info/refs?service=git-receive-pack";
const UPLOAD: &str = "/fixture-dev/app.git/info/refs?service=git-upload-pack";

/// A fake with `SLUG` (public, push for `fixture-dev-bot`, pull for `fixture-reader`) at
/// `main` = `MAIN_SHA` and tag `v1` = `V1_SHA`, and a private `fixture-dev/secret`.
fn fake() -> (FakeGithub, String, String) {
    let fake = FakeGithub::start();
    let (dev, reader) = (token("dev"), token("reader"));
    fake.add_token(&dev, "fixture-dev-bot");
    fake.add_token(&reader, "fixture-reader");
    fake.add_repo(
        SLUG,
        RepoSpec::public()
            .push("fixture-dev-bot")
            .pull("fixture-reader"),
    );
    fake.add_repo(
        "fixture-dev/secret",
        RepoSpec::private().pull("fixture-reader"),
    );
    fake.set_branch(SLUG, "main", MAIN_SHA);
    fake.set_tag(SLUG, "v1", V1_SHA);
    (fake, dev, reader)
}

/// `req` carrying `token` as git sends it through agh's helper.
fn as_git(req: Req, token: &str) -> Req {
    req.header("Authorization", &format!("Basic {}", basic(token)))
}

/// One pkt-line.
fn pkt(payload: &str) -> String {
    format!("{:04x}{payload}", payload.len() + 4)
}

/// A receive-pack request of `commands`, the first carrying git's capabilities, then a flush.
fn receive_pack(commands: &[String]) -> Vec<u8> {
    let mut body = String::new();
    for (i, command) in commands.iter().enumerate() {
        if i == 0 {
            body.push_str(&pkt(&format!(
                "{command}\0report-status delete-refs agent=git/2\n"
            )));
        } else {
            body.push_str(&pkt(&format!("{command}\n")));
        }
    }
    body.push_str("0000");
    body.into_bytes()
}

/// `body` POSTed to `SLUG`'s receive-pack under `token`.
fn push(fake: &FakeGithub, token: &str, body: &[u8]) -> Reply {
    as_git(
        plain("/fixture-dev/app.git/git-receive-pack").method("POST"),
        token,
    )
    .header("Content-Type", "application/x-git-receive-pack-request")
    .body(body)
    .send(fake.ports().web)
}

/// A report's pkt-lines, without their LFs, up to the flush.
fn report(reply: &Reply) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = reply.body.as_slice();
    while rest.len() >= 4 {
        let len = usize::from_str_radix(std::str::from_utf8(&rest[..4]).expect("hex"), 16)
            .expect("a length");
        if len == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&rest[4..len]);
        lines.push(line.trim_end_matches('\n').to_string());
        rest = &rest[len..];
    }
    lines
}

#[test]
fn info_refs_without_basic_is_401_with_the_realm() {
    let (fake, dev, reader) = fake();
    let web = fake.ports().web;
    let realm = |reply: &Reply| {
        assert_eq!(reply.status, 401);
        assert_eq!(
            reply.header("WWW-Authenticate"),
            Some("Basic realm=\"GitHub\"")
        );
    };
    realm(&plain(RECEIVE).send(web));
    realm(&as_git(plain(RECEIVE), &reader).send(web));
    let secret = "/fixture-dev/secret.git/info/refs?service=git-upload-pack";
    realm(&plain(secret).send(web));
    realm(&as_git(plain(secret), &dev).send(web));
    assert_eq!(plain(UPLOAD).send(web).status, 200);
    assert_eq!(as_git(plain(secret), &reader).send(web).status, 200);
    assert_eq!(as_git(plain(RECEIVE), &dev).send(web).status, 200);
    let stranger = aterm_codec::base64::encode(format!("someone:{dev}").as_bytes()).expect("b64");
    realm(
        &plain(RECEIVE)
            .header("Authorization", &format!("Basic {stranger}"))
            .send(web),
    );
    realm(&as_git(plain(RECEIVE), &token("unknown")).send(web));
    realm(
        &plain(UPLOAD)
            .header("Authorization", &format!("Basic {stranger}"))
            .send(web),
    );
    realm(&as_git(plain(UPLOAD), &token("unknown")).send(web));
    let basic = format!("Basic {stranger}");
    realm(
        &plain("/fixture-dev/app.git/git-upload-pack")
            .method("POST")
            .header("Authorization", &basic)
            .body(b"0000")
            .send(web),
    );
    realm(
        &plain("/fixture-dev/app.git/info/refs?service=git-other")
            .header("Authorization", &basic)
            .send(web),
    );
    realm(
        &plain(RECEIVE)
            .method("POST")
            .header("Authorization", &basic)
            .send(web),
    );
}

/// A registered token under another username is refused, yet `seen()` names whose token it
/// was: a test can tell that a credential reached the fake. NEGATIVE CONTROL: the same token
/// under `x-access-token` is the authorized read.
#[test]
fn a_token_under_another_username_is_401_and_still_seen() {
    let (fake, dev, reader) = fake();
    let web = fake.ports().web;
    let secret = "/fixture-dev/secret.git/info/refs?service=git-upload-pack";
    let other = aterm_codec::base64::encode(format!("x:{reader}").as_bytes()).expect("b64");
    let refused = plain(secret)
        .header("Authorization", &format!("Basic {other}"))
        .send(web);
    assert_eq!(refused.status, 401);
    let seen = fake.seen();
    assert!(seen[0].had_authorization);
    assert_eq!(seen[0].as_login.as_deref(), Some("fixture-reader"));
    assert_eq!(as_git(plain(secret), &reader).send(web).status, 200);
    let other = aterm_codec::base64::encode(format!("x:{dev}").as_bytes()).expect("b64");
    plain(UPLOAD)
        .header("Authorization", &format!("Basic {other}"))
        .send(web);
    assert_eq!(fake.seen()[2].as_login.as_deref(), Some("fixture-dev-bot"));
}

/// A registered token that reaches a git path as a Bearer header or as the Basic username
/// (GitHub's `{token}:x-oauth-basic`) is refused and still names its login in `as_login`.
/// NEGATIVE CONTROL: the same path with no credential names none.
#[test]
fn a_token_in_any_credential_form_is_seen() {
    let (fake, dev, _) = fake();
    let web = fake.ports().web;
    assert_eq!(plain(RECEIVE).send(web).status, 401);
    assert_eq!(plain(RECEIVE).token(&dev).send(web).status, 401);
    let user = aterm_codec::base64::encode(format!("{dev}:x-oauth-basic").as_bytes()).expect("b64");
    let as_user = plain(RECEIVE)
        .header("Authorization", &format!("Basic {user}"))
        .send(web);
    assert_eq!(as_user.status, 401);
    let logins: Vec<Option<String>> = fake.seen().into_iter().map(|s| s.as_login).collect();
    let bot = Some("fixture-dev-bot".to_string());
    assert_eq!(logins, [None, bot.clone(), bot]);
}

/// A command whose ref holds a space addresses no ref: 400, and nothing lands. NEGATIVE
/// CONTROL: the same delete without the second word lands.
#[test]
fn a_ref_with_a_space_is_400_and_lands_nothing() {
    let (fake, dev, _) = fake();
    let spaced = push(
        &fake,
        &dev,
        &receive_pack(&[format!("{V1_SHA} {ZERO} refs/tags/v1 b")]),
    );
    assert_eq!(spaced.status, 400);
    assert_eq!(fake.git_refs(SLUG).len(), 2);
    let plain_delete = push(
        &fake,
        &dev,
        &receive_pack(&[format!("{V1_SHA} {ZERO} refs/tags/v1")]),
    );
    assert_eq!(report(&plain_delete), ["unpack ok", "ok refs/tags/v1"]);
}

#[test]
fn a_malformed_command_lands_no_delete() {
    let (fake, dev, _) = fake();
    let reply = push(
        &fake,
        &dev,
        &receive_pack(&[format!("{V1_SHA} {ZERO} refs/tags/v1"), "garbage".into()]),
    );
    assert_eq!(reply.status, 400);
    assert!(
        fake.git_refs(SLUG)
            .contains(&("refs/tags/v1".to_string(), V1_SHA.to_string()))
    );
}

#[test]
fn the_advertisement_is_v0_pkt_lines() {
    let (fake, dev, _) = fake();
    let reply = as_git(plain(RECEIVE), &dev).send(fake.ports().web);
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.header("Content-Type"),
        Some("application/x-git-receive-pack-advertisement")
    );
    let expected = format!(
        "001f# service=git-receive-pack\n0000\
         0057{MAIN_SHA} refs/heads/main\0report-status delete-refs\n\
         003a{V1_SHA} refs/tags/v1\n0000"
    );
    assert_eq!(String::from_utf8_lossy(&reply.body), expected);
    let upload = plain(UPLOAD).send(fake.ports().web);
    assert_eq!(
        upload.header("Content-Type"),
        Some("application/x-git-upload-pack-advertisement")
    );
    let expected = format!(
        "001e# service=git-upload-pack\n0000\
         0059{MAIN_SHA} refs/heads/main\0symref=HEAD:refs/heads/main\n\
         003a{V1_SHA} refs/tags/v1\n0000"
    );
    assert_eq!(String::from_utf8_lossy(&upload.body), expected);
    fake.add_repo(
        "fixture-dev/empty",
        RepoSpec::public().push("fixture-dev-bot"),
    );
    let empty = as_git(
        plain("/fixture-dev/empty.git/info/refs?service=git-receive-pack"),
        &dev,
    )
    .send(fake.ports().web);
    let expected = format!(
        "001f# service=git-receive-pack\n0000\
         0057{ZERO} capabilities^{{}}\0report-status delete-refs\n0000"
    );
    assert_eq!(String::from_utf8_lossy(&empty.body), expected);
}

#[test]
fn receive_pack_deletes_only_with_the_right_old_sha() {
    let (fake, dev, _) = fake();
    let reply = push(
        &fake,
        &dev,
        &receive_pack(&[format!("{V1_SHA} {ZERO} refs/tags/v1")]),
    );
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.header("Content-Type"),
        Some("application/x-git-receive-pack-result")
    );
    assert_eq!(report(&reply), ["unpack ok", "ok refs/tags/v1"]);
    assert!(reply.body.ends_with(b"0000"));
    assert_eq!(
        fake.git_refs(SLUG),
        [("refs/heads/main".to_string(), MAIN_SHA.to_string())]
    );
}

#[test]
fn a_stale_old_sha_is_ng_stale_info() {
    let (fake, dev, _) = fake();
    let reply = push(
        &fake,
        &dev,
        &receive_pack(&[format!("{MAIN_SHA} {ZERO} refs/tags/v1")]),
    );
    assert_eq!(report(&reply), ["unpack ok", "ng refs/tags/v1 stale info"]);
    assert!(
        fake.git_refs(SLUG)
            .contains(&("refs/tags/v1".to_string(), V1_SHA.to_string()))
    );
}

#[test]
fn receive_pack_refuses_a_non_delete() {
    let (fake, dev, _) = fake();
    let reply = push(
        &fake,
        &dev,
        &receive_pack(&[
            format!("{ZERO} {MAIN_SHA} refs/heads/x"),
            format!("{MAIN_SHA} {ZERO} refs/heads/main"),
        ]),
    );
    assert_eq!(
        report(&reply),
        [
            "unpack ok",
            "ng refs/heads/x only deletes",
            "ok refs/heads/main"
        ]
    );
    assert_eq!(
        fake.git_refs(SLUG),
        [("refs/tags/v1".to_string(), V1_SHA.to_string())]
    );
}

#[test]
fn receive_pack_needs_push() {
    let (fake, _, reader) = fake();
    let reply = push(
        &fake,
        &reader,
        &receive_pack(&[format!("{V1_SHA} {ZERO} refs/tags/v1")]),
    );
    assert_eq!(reply.status, 401);
    assert_eq!(fake.git_refs(SLUG).len(), 2);
}

#[test]
fn upload_pack_post_is_501() {
    let (fake, dev, _) = fake();
    let reply = as_git(
        plain("/fixture-dev/app.git/git-upload-pack").method("POST"),
        &dev,
    )
    .body(b"0000")
    .send(fake.ports().web);
    assert_eq!(reply.status, 501);
}

#[test]
fn basic_on_the_git_paths_is_no_violation() {
    let (fake, dev, _) = fake();
    let web = fake.ports().web;
    assert_eq!(as_git(plain(RECEIVE), &dev).send(web).status, 200);
    assert!(fake.violations().is_empty(), "{:?}", fake.violations());
    let seen = fake.seen();
    assert!(seen[0].had_authorization);
    assert_eq!(seen[0].as_login.as_deref(), Some("fixture-dev-bot"));
    assert!(seen[0].header("authorization").is_none());
    as_git(plain("/fixture-dev/app/releases"), &dev).send(web);
    let violations = fake.violations();
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("/fixture-dev/app/releases"));
    plain(RECEIVE).token(&dev).send(web);
    assert_eq!(fake.violations().len(), 2);
}

#[test]
fn basic_is_the_padded_base64_of_the_helper_credential() {
    let t = token("helper");
    let encoded = basic(&t);
    let decoded = aterm_codec::base64::decode(&encoded).expect("base64");
    assert_eq!(decoded, format!("x-access-token:{t}").into_bytes());
    assert_eq!(encoded.len() % 4, 0);
    assert!(encoded.ends_with('='));
}
