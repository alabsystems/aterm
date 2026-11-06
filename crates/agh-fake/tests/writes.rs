// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The write routes: releases (drafts, the tag rule, the listing lag), assets (digests,
//! collisions, starters, immutable releases), refs and keys; who may write; the faults that
//! land a write and lose its answer; and body capture.

use agh_fake::{
    AssetSpec, Change, CommitSpec, FakeGithub, Fault, Match, ReleaseSpec, RepoSpec, token,
};
use aterm_json::{Value, json};

use crate::raw::{Reply, api, send_cut};

const SLUG: &str = "fixture-dev/app";
/// The default branch's head.
const MAIN_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
/// Branch `rel`'s head.
const REL_SHA: &str = "89abcdef0123456789abcdef0123456789abcdef";
/// A commit `add_commit` gives, that no ref names.
const COMMIT_SHA: &str = "fedcba9876543210fedcba9876543210fedcba98";

/// `fake()` with `main` at `MAIN_SHA`.
fn with_main() -> (FakeGithub, String, String) {
    let (fake, dev, reader) = fake();
    fake.set_branch(SLUG, "main", MAIN_SHA);
    (fake, dev, reader)
}

/// `body` sent as JSON by `method` to `target` on the api origin, under `token`.
fn send_json(fake: &FakeGithub, method: &str, target: &str, token: &str, body: &Value) -> Reply {
    api(target)
        .method(method)
        .token(token)
        .header("Content-Type", "application/json")
        .body(body.to_string().as_bytes())
        .send(fake.ports().api)
}

/// A release of `SLUG` created by `dev` with `body`.
fn create(fake: &FakeGithub, dev: &str, body: &Value) -> Reply {
    send_json(fake, "POST", "/repos/fixture-dev/app/releases", dev, body)
}

/// `bytes` uploaded by `dev` as asset `name` of release `id`.
fn upload(fake: &FakeGithub, dev: &str, id: u64, name: &str, bytes: &[u8]) -> Reply {
    api(&format!(
        "/repos/fixture-dev/app/releases/{id}/assets?name={name}"
    ))
    .method("POST")
    .token(dev)
    .header("Content-Type", "application/octet-stream")
    .body(bytes)
    .send(fake.ports().uploads)
}

/// The reply's numeric `id`.
fn id_of(reply: &Reply) -> u64 {
    reply.json()["id"].as_u64().expect("an id")
}

/// The reply's first error's `code`.
fn code(reply: &Reply) -> Value {
    reply.json()["errors"][0]["code"].clone()
}

/// `sha256:{hex}` of `bytes`.
fn sha256(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
    )
}

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
    (fake, dev, reader)
}

#[test]
fn user_repo_create_preserves_visibility_and_lands_before_a_lost_answer() {
    let (fake, dev, _) = fake();
    let body = json!({"name": "new-repo", "private": false, "auto_init": false});
    fake.fault(
        Match::api("/user/repos").method("POST"),
        Fault::LandThenClose,
    );
    let lost = send_json(&fake, "POST", "/user/repos", &dev, &body);
    assert_eq!(lost.status, 0);
    let read = api("/repos/fixture-dev-bot/new-repo").send(fake.ports().api);
    assert_eq!(read.status, 200);
    assert_eq!(read.json()["full_name"], json!("fixture-dev-bot/new-repo"));
    assert_eq!(read.json()["owner"]["type"], json!("User"));
    assert_eq!(read.json()["visibility"], json!("public"));
    assert_eq!(
        send_json(&fake, "POST", "/user/repos", &dev, &body).status,
        422
    );
    let private = send_json(
        &fake,
        "POST",
        "/user/repos",
        &dev,
        &json!({"name": "private-repo", "private": true, "auto_init": false}),
    );
    assert_eq!(private.status, 201);
    assert_eq!(private.json()["private"], json!(true));
    assert_eq!(private.json()["permissions"]["push"], json!(true));
    assert_eq!(
        api("/repos/fixture-dev-bot/private-repo")
            .send(fake.ports().api)
            .status,
        404
    );
    assert!(fake.violations().is_empty());
}

#[test]
fn user_repo_create_requires_authentication_safe_name_and_explicit_flags() {
    let (fake, dev, _) = fake();
    let body = json!({"name": "new-repo", "private": false, "auto_init": false});
    let anonymous = api("/user/repos")
        .method("POST")
        .body(body.to_string().as_bytes())
        .send(fake.ports().api);
    assert_eq!(anonymous.status, 401);
    for bad in [
        json!({"name": "../escape", "private": false, "auto_init": false}),
        json!({"name": "..", "private": false, "auto_init": false}),
        json!({"name": "new-repo", "auto_init": false}),
        json!({"name": "new-repo", "private": false}),
        json!({"name": "new-repo", "private": false, "auto_init": true}),
    ] {
        assert_eq!(
            send_json(&fake, "POST", "/user/repos", &dev, &bad).status,
            422
        );
    }
    for method in ["PATCH", "DELETE"] {
        assert_eq!(
            send_json(&fake, method, "/user/repos", &dev, &body).status,
            404
        );
    }
    let listed = api("/user/repos").token(&dev).send(fake.ports().api);
    assert_eq!(listed.status, 200);
    assert!(
        listed
            .json()
            .as_array()
            .expect("repository list")
            .iter()
            .all(|repo| repo["full_name"] != json!("fixture-dev-bot/new-repo"))
    );
    assert_eq!(
        api("/user/repos")
            .method("HEAD")
            .token(&dev)
            .send(fake.ports().api)
            .status,
        200
    );
    assert!(fake.violations().is_empty());
}

#[test]
fn a_push_login_creates_uploads_publishes_and_deletes() {
    let (fake, dev, _) = fake();
    let (api_port, uploads) = (fake.ports().api, fake.ports().uploads);
    let create = json!({"tag_name": "v1", "name": "one", "draft": true, "prerelease": true});
    let made = api("/repos/fixture-dev/app/releases")
        .method("POST")
        .token(&dev)
        .header("Content-Type", "application/json")
        .body(create.to_string().as_bytes())
        .send(api_port);
    assert_eq!(made.status, 201, "{}", made.json());
    let id = made.json()["id"].as_u64().expect("an id");
    assert_eq!(made.json()["draft"], json!(true));
    let upload = |name: &str, bytes: &[u8]| {
        api(&format!(
            "/repos/fixture-dev/app/releases/{id}/assets?name={name}"
        ))
        .method("POST")
        .token(&dev)
        .header("Content-Type", "application/octet-stream")
        .body(bytes)
        .send(uploads)
    };
    let asset = upload("a.bin", b"the bytes");
    assert_eq!(asset.status, 201);
    assert_eq!(asset.json()["size"], json!(9));
    let again = upload("a.bin", b"other bytes");
    assert_eq!(again.status, 422);
    assert_eq!(again.json()["errors"][0]["code"], json!("already_exists"));
    let (spec, assets) = fake.release_state(id).expect("the release");
    assert!(spec.draft);
    assert_eq!(assets, [("a.bin".to_string(), b"the bytes".to_vec())]);
    let edit = api(&format!("/repos/fixture-dev/app/releases/{id}"))
        .method("PATCH")
        .token(&dev)
        .header("Content-Type", "application/json")
        .body(
            json!({"draft": false, "make_latest": "false"})
                .to_string()
                .as_bytes(),
        )
        .send(api_port);
    assert_eq!(edit.status, 200);
    assert_eq!(edit.json()["draft"], json!(false));
    assert_eq!(edit.json()["prerelease"], json!(true));
    let twice = api("/repos/fixture-dev/app/releases")
        .method("POST")
        .token(&dev)
        .body(json!({"tag_name": "v1"}).to_string().as_bytes())
        .send(api_port);
    assert_eq!(twice.status, 422);
    let asset_id = asset.json()["id"].as_u64().expect("an asset id");
    let second = upload("b.bin", b"second").json()["id"]
        .as_u64()
        .expect("an asset id");
    let rename = |to: &str| {
        api(&format!("/repos/fixture-dev/app/releases/assets/{second}"))
            .method("PATCH")
            .token(&dev)
            .header("Content-Type", "application/json")
            .body(json!({ "name": to }).to_string().as_bytes())
            .send(api_port)
    };
    let taken = rename("a.bin");
    assert_eq!(taken.status, 422);
    assert_eq!(taken.json()["errors"][0]["code"], json!("already_exists"));
    let renamed = rename("c.bin");
    assert_eq!(renamed.status, 200);
    assert_eq!(renamed.json()["name"], json!("c.bin"));
    assert_eq!(renamed.json()["id"], json!(second));
    let gone = api(&format!("/repos/fixture-dev/app/releases/assets/{second}"))
        .method("DELETE")
        .token(&dev)
        .send(api_port);
    assert_eq!(gone.status, 204);
    let gone = api(&format!(
        "/repos/fixture-dev/app/releases/assets/{asset_id}"
    ))
    .method("DELETE")
    .token(&dev)
    .send(api_port);
    assert_eq!(gone.status, 204);
    assert_eq!(fake.release_state(id).expect("the release").1, []);
    let deleted = api(&format!("/repos/fixture-dev/app/releases/{id}"))
        .method("DELETE")
        .token(&dev)
        .send(api_port);
    assert_eq!(deleted.status, 204);
    assert!(fake.release_state(id).is_none());
    assert!(fake.release_ids(SLUG).is_empty());
    fake.assert_clean();
}

#[test]
fn anonymous_a_hidden_repository_and_pull_alone_are_refused() {
    let (fake, _, reader) = fake();
    let port = fake.ports().api;
    let id = fake.add_release(SLUG, ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"));
    fake.add_asset(id, AssetSpec::uploaded("a.bin", b"ay"));
    let body = json!({"draft": true}).to_string();
    let edit = |req: crate::raw::Req| req.method("PATCH").body(body.as_bytes()).send(port).status;
    let target = format!("/repos/fixture-dev/app/releases/{id}");
    assert_eq!(edit(api(&target)), 401);
    assert_eq!(edit(api(&target).token(&reader)), 403);
    fake.add_repo("fixture-dev/hidden", RepoSpec::private());
    let hidden = api("/repos/fixture-dev/hidden/releases")
        .method("POST")
        .token(&reader)
        .body(json!({"tag_name": "v1"}).to_string().as_bytes())
        .send(port);
    assert_eq!(hidden.status, 404);
    assert!(!fake.release_state(id).expect("the release").0.draft);
    let upload = api(&format!(
        "/repos/fixture-dev/app/releases/{id}/assets?name=b.bin"
    ))
    .method("POST")
    .token(&reader)
    .header("Content-Type", "application/octet-stream")
    .body(b"bee")
    .send(fake.ports().uploads);
    assert_eq!(upload.status, 403);
    assert_eq!(fake.release_state(id).expect("the release").1.len(), 1);
}

/// `releases_of` reports the `target_commitish` a create sent, not the default branch the
/// fake answers on the wire for one that sent none. NEGATIVE CONTROL: the wire answer for the
/// create without one names the default branch.
#[test]
fn releases_of_reports_only_the_target_a_create_sent() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    let create = |body: aterm_json::Value| {
        api("/repos/fixture-dev/app/releases")
            .method("POST")
            .token(&dev)
            .header("Content-Type", "application/json")
            .body(body.to_string().as_bytes())
            .send(port)
    };
    let bare = create(json!({"tag_name": "v1", "draft": true}));
    assert_eq!(bare.status, 201);
    assert_eq!(bare.json()["target_commitish"], json!("main"));
    let aimed = create(json!({"tag_name": "v2", "draft": true, "target_commitish": "main"}));
    assert_eq!(aimed.status, 201);
    let sent: Vec<(String, Option<String>)> = fake
        .releases_of(SLUG)
        .into_iter()
        .map(|r| (r.tag, r.target_commitish))
        .collect();
    assert_eq!(
        sent,
        [
            ("v1".to_string(), None),
            ("v2".to_string(), Some("main".to_string()))
        ]
    );
    let patched = api(&format!(
        "/repos/fixture-dev/app/releases/{}",
        bare.json()["id"]
    ))
    .method("PATCH")
    .token(&dev)
    .header("Content-Type", "application/json")
    .body(json!({"target_commitish": "rel"}).to_string().as_bytes())
    .send(port);
    assert_eq!(patched.status, 200);
    assert_eq!(patched.json()["target_commitish"], json!("rel"));
    assert_eq!(fake.releases_of(SLUG)[0].target_commitish, None);
}

#[test]
fn a_write_needs_push() {
    let (fake, _, reader) = fake();
    fake.add_repo("fixture-dev/hidden", RepoSpec::private());
    let refused = create(&fake, &reader, &json!({"tag_name": "v1", "draft": true}));
    assert_eq!(refused.status, 403);
    assert_eq!(
        refused.json()["message"],
        json!("Resource not accessible by personal access token")
    );
    assert_eq!(
        refused.header("X-Accepted-GitHub-Permissions"),
        Some("contents=write")
    );
    let unseen = send_json(
        &fake,
        "POST",
        "/repos/fixture-dev/hidden/releases",
        &reader,
        &json!({"tag_name": "v1"}),
    );
    assert_eq!(unseen.status, 404);
    let anonymous = api("/repos/fixture-dev/app/releases")
        .method("POST")
        .body(json!({"tag_name": "v1"}).to_string().as_bytes())
        .send(fake.ports().api);
    assert_eq!(anonymous.status, 401);
    assert!(fake.releases_of(SLUG).is_empty());
    assert!(fake.releases_of("fixture-dev/hidden").is_empty());
}

#[test]
fn drafts_share_a_tag_and_a_published_tag_is_422_a() {
    let (fake, dev, _) = with_main();
    let first = create(&fake, &dev, &json!({"tag_name": "v1", "draft": true}));
    let second = create(&fake, &dev, &json!({"tag_name": "v1", "draft": true}));
    assert_eq!((first.status, second.status), (201, 201));
    let drafts: Vec<(String, bool)> = fake
        .releases_of(SLUG)
        .into_iter()
        .map(|r| (r.tag, r.draft))
        .collect();
    assert_eq!(drafts, [("v1".into(), true), ("v1".into(), true)]);
    let publish = send_json(
        &fake,
        "PATCH",
        &format!("/repos/fixture-dev/app/releases/{}", id_of(&first)),
        &dev,
        &json!({"draft": false}),
    );
    assert_eq!(publish.status, 200);
    let third = create(&fake, &dev, &json!({"tag_name": "v1", "draft": true}));
    assert_eq!(third.status, 422);
    assert_eq!(third.json()["message"], json!("Validation Failed"));
    assert_eq!(code(&third), json!("already_exists"));
    assert_eq!(third.json()["errors"][0]["field"], json!("tag_name"));
    assert_eq!(fake.releases_of(SLUG).len(), 2);
    let missing = create(&fake, &dev, &json!({"draft": true}));
    assert_eq!(missing.status, 422);
    assert_eq!(code(&missing), json!("missing_field"));
}

#[test]
fn publish_tags_the_target_or_the_default_branch() {
    let (fake, dev, _) = with_main();
    fake.set_branch(SLUG, "rel", REL_SHA);
    fake.add_commit(
        SLUG,
        CommitSpec {
            sha: COMMIT_SHA.into(),
            message: "a commit".into(),
            author_login: None,
            author_name: "a".into(),
            date: "2026-10-01T00:00:00Z".into(),
            paths: Vec::new(),
        },
    );
    let publish = |body: Value| {
        let draft = create(&fake, &dev, &body);
        assert_eq!(draft.status, 201, "{}", draft.json());
        let id = id_of(&draft);
        let patched = send_json(
            &fake,
            "PATCH",
            &format!("/repos/fixture-dev/app/releases/{id}"),
            &dev,
            &json!({"draft": false}),
        );
        (id, patched)
    };
    let tag = |name: &str| {
        fake.git_refs(SLUG)
            .into_iter()
            .find(|(r, _)| *r == format!("refs/tags/{name}"))
            .map(|(_, sha)| sha)
    };
    assert_eq!(tag("v1"), None);
    let (_, bare) = publish(json!({"tag_name": "v1", "draft": true}));
    assert_eq!(bare.status, 200);
    assert_eq!(tag("v1").as_deref(), Some(MAIN_SHA));
    let (_, aimed) = publish(json!({"tag_name": "v2", "draft": true, "target_commitish": "rel"}));
    assert_eq!(aimed.status, 200);
    assert_eq!(tag("v2").as_deref(), Some(REL_SHA));
    let (_, by_sha) =
        publish(json!({"tag_name": "v3", "draft": true, "target_commitish": COMMIT_SHA}));
    assert_eq!(by_sha.status, 200);
    assert_eq!(tag("v3").as_deref(), Some(COMMIT_SHA));
    let (stray, unknown) =
        publish(json!({"tag_name": "v4", "draft": true, "target_commitish": "nope"}));
    assert_eq!(unknown.status, 422);
    assert_eq!(unknown.json()["message"], json!("Validation Failed"));
    assert_eq!(
        unknown.json()["errors"],
        json!([{"resource": "Release", "code": "invalid", "field": "target_commitish"}])
    );
    assert_eq!(tag("v4"), None);
    let still = fake.releases_of(SLUG);
    assert!(still.iter().any(|r| r.id == stray && r.draft));
    let at_once = create(
        &fake,
        &dev,
        &json!({"tag_name": "v5", "target_commitish": "nope"}),
    );
    assert_eq!(at_once.status, 422);
    assert_eq!(code(&at_once), json!("invalid"));
    assert!(!fake.releases_of(SLUG).iter().any(|r| r.tag == "v5"));
    let published = create(
        &fake,
        &dev,
        &json!({"tag_name": "v6", "target_commitish": "rel"}),
    );
    assert_eq!(published.status, 201);
    assert_eq!(published.json()["draft"], json!(false));
    assert_eq!(tag("v6").as_deref(), Some(REL_SHA));
    fake.set_tag(SLUG, "v7", COMMIT_SHA);
    let (_, kept) = publish(json!({"tag_name": "v7", "draft": true}));
    assert_eq!(kept.status, 200);
    assert_eq!(tag("v7").as_deref(), Some(COMMIT_SHA));
}

#[test]
fn the_listing_lag_hides_until_revealed() {
    let (fake, dev, _) = with_main();
    fake.set_listing_lag(SLUG, true);
    let made = create(&fake, &dev, &json!({"tag_name": "v1", "draft": true}));
    assert_eq!(made.status, 201);
    let id = id_of(&made);
    let added = fake.add_release(SLUG, ReleaseSpec::published("v2", "2026-10-01T00:00:00Z"));
    let drafted = fake.add_release(SLUG, ReleaseSpec::draft("v9", "2026-10-01T00:00:00Z"));
    let port = fake.ports().api;
    let listed = || -> Vec<u64> {
        api("/repos/fixture-dev/app/releases")
            .token(&dev)
            .send(port)
            .json()
            .as_array()
            .expect("a listing")
            .iter()
            .filter_map(|r| r["id"].as_u64())
            .collect()
    };
    let by_tag = |tag: &str| {
        api(&format!("/repos/fixture-dev/app/releases/tags/{tag}"))
            .token(&dev)
            .send(port)
            .status
    };
    let by_id = |id: u64| {
        api(&format!("/repos/fixture-dev/app/releases/{id}"))
            .token(&dev)
            .send(port)
            .status
    };
    fake.add_asset(added, AssetSpec::uploaded("x.bin", b"x"));
    let latest = || {
        api("/repos/fixture-dev/app/releases/latest")
            .token(&dev)
            .send(port)
            .status
    };
    let download = || {
        crate::raw::plain("/fixture-dev/app/releases/download/v2/x.bin")
            .send(fake.ports().web)
            .status
    };
    assert_eq!(listed(), Vec::<u64>::new());
    assert_eq!(by_tag("v2"), 404);
    assert_eq!((latest(), download()), (404, 404));
    assert_eq!((by_id(id), by_id(added), by_id(drafted)), (200, 200, 200));
    let hidden: Vec<bool> = fake.releases_of(SLUG).iter().map(|r| r.hidden).collect();
    assert_eq!(hidden, [true, true, true]);
    fake.apply(Change::Reveal { release_id: id });
    assert_eq!(listed(), [id]);
    fake.apply(Change::Reveal { release_id: added });
    assert_eq!(by_tag("v2"), 200);
    assert_eq!((latest(), download()), (200, 302));
    assert_eq!(listed().len(), 2);
    assert!(
        !listed().contains(&drafted),
        "a push login sees drafts, but not under the lag"
    );
    fake.apply(Change::Reveal {
        release_id: drafted,
    });
    assert!(listed().contains(&drafted));
    fake.set_listing_lag(SLUG, false);
    let later = create(&fake, &dev, &json!({"tag_name": "v3", "draft": true}));
    assert!(listed().contains(&id_of(&later)));
}

#[test]
fn uploads_digest_and_collide() {
    let (fake, dev, _) = fake();
    let id = id_of(&create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "draft": true}),
    ));
    let up = upload(&fake, &dev, id, "a.bin", b"the bytes");
    assert_eq!(up.status, 201);
    assert_eq!(up.json()["digest"], json!(sha256(b"the bytes")));
    assert_eq!(up.json()["state"], json!("uploaded"));
    assert_eq!(up.json()["size"], json!(9));
    let again = upload(&fake, &dev, id, "a.bin", b"the bytes");
    assert_eq!(again.status, 422);
    assert_eq!(again.json()["message"], json!("Validation Failed"));
    assert_eq!(
        again.json()["errors"],
        json!([{"resource": "ReleaseAsset", "code": "already_exists", "field": "name"}])
    );
    assert_eq!(upload(&fake, &dev, 4242, "b.bin", b"b").status, 404);
    let assets = &fake.releases_of(SLUG)[0].assets;
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].3, sha256(b"the bytes"));
}

/// The listing the upload guard reads before each attempt: by id, paged, a starter included,
/// a draft's hidden from a pull login.
#[test]
fn a_release_lists_its_assets_by_page() {
    let (fake, dev, reader) = fake();
    let id = id_of(&create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "draft": true}),
    ));
    let first = id_of(&upload(&fake, &dev, id, "a.bin", b"a"));
    fake.fault(
        Match::uploads("/repos/fixture-dev/app/releases").method("POST"),
        Fault::StarterThen { status: 502 },
    );
    assert_eq!(upload(&fake, &dev, id, "b.bin", b"b").status, 502);
    let port = fake.ports().api;
    let target = format!("/repos/fixture-dev/app/releases/{id}/assets?per_page=1");
    let page_one = api(&target).token(&dev).send(port);
    assert_eq!(page_one.status, 200);
    assert_eq!(page_one.json()[0]["id"], json!(first));
    assert_eq!(page_one.json()[0]["state"], json!("uploaded"));
    let next = page_one.link("next").expect("a next page");
    let rest = next.trim_start_matches(&format!("http://127.0.0.1:{port}"));
    let page_two = api(rest).token(&dev).send(port);
    assert_eq!(page_two.json().as_array().map(Vec::len), Some(1));
    assert_eq!(page_two.json()[0]["name"], json!("b.bin"));
    assert_eq!(page_two.json()[0]["state"], json!("starter"));
    assert!(page_two.link("next").is_none());
    assert_eq!(api(&target).token(&reader).send(port).status, 404);
    let unknown = api("/repos/fixture-dev/app/releases/4242/assets")
        .token(&dev)
        .send(port);
    assert_eq!(unknown.status, 404);
}

#[test]
fn starter_then_502_leaves_a_starter_asset() {
    let (fake, dev, _) = fake();
    let id = id_of(&create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "draft": true}),
    ));
    fake.fault(
        Match::uploads("/repos/fixture-dev/app/releases").method("POST"),
        Fault::StarterThen { status: 502 },
    );
    let cut = upload(&fake, &dev, id, "a.bin", b"the bytes");
    assert_eq!(cut.status, 502);
    assert_eq!(cut.json()["status"], json!("502"));
    let assets = &fake.releases_of(SLUG)[0].assets;
    assert_eq!(assets.len(), 1);
    assert_eq!(
        (assets[0].1.as_str(), assets[0].2.as_str()),
        ("a.bin", "starter")
    );
    assert_eq!(assets[0].3, sha256(b""));
}

#[test]
fn land_then_close_applies_the_write() {
    let (fake, dev, _) = fake();
    let releases = Match::api("/repos/fixture-dev/app/releases").method("POST");
    fake.fault(releases.clone(), Fault::LandThenClose);
    let closed = create(&fake, &dev, &json!({"tag_name": "v1", "draft": true}));
    assert_eq!(closed.status, 0);
    assert!(closed.raw.is_empty());
    assert_eq!(fake.releases_of(SLUG).len(), 1);
    fake.fault(releases, Fault::LandThenStatus { status: 502 });
    let lost = create(&fake, &dev, &json!({"tag_name": "v2", "draft": true}));
    assert_eq!(lost.status, 502);
    assert!(lost.header("X-GitHub-Request-Id").is_some());
    let tags: Vec<String> = fake.releases_of(SLUG).into_iter().map(|r| r.tag).collect();
    assert_eq!(tags, ["v1", "v2"]);
}

#[test]
fn immutable_refuses_asset_writes() {
    let (fake, dev, _) = fake();
    let id = id_of(&create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "draft": true}),
    ));
    let asset = id_of(&upload(&fake, &dev, id, "a.bin", b"a"));
    fake.set_immutable(id, true);
    let before = fake.releases_of(SLUG)[0].assets.clone();
    let is_body_i = |reply: &Reply| {
        assert_eq!(reply.status, 422);
        assert_eq!(code(reply), json!("custom"));
        assert_eq!(reply.json()["errors"][0]["resource"], json!("Release"));
    };
    is_body_i(&upload(&fake, &dev, id, "b.bin", b"b"));
    let target = format!("/repos/fixture-dev/app/releases/assets/{asset}");
    is_body_i(&send_json(
        &fake,
        "PATCH",
        &target,
        &dev,
        &json!({"name": "c.bin"}),
    ));
    is_body_i(
        &api(&target)
            .method("DELETE")
            .token(&dev)
            .send(fake.ports().api),
    );
    assert_eq!(fake.releases_of(SLUG)[0].assets, before);
    assert!(fake.releases_of(SLUG)[0].immutable);
    fake.set_immutable(id, false);
    assert_eq!(upload(&fake, &dev, id, "b.bin", b"b").status, 201);
}

#[test]
fn refs_create_read_and_collide() {
    let (fake, dev, _) = with_main();
    let refs = "/repos/fixture-dev/app/git/refs";
    let made = send_json(
        &fake,
        "POST",
        refs,
        &dev,
        &json!({"ref": "refs/tags/v1", "sha": MAIN_SHA}),
    );
    assert_eq!(made.status, 201);
    assert_eq!(made.json()["ref"], json!("refs/tags/v1"));
    assert_eq!(made.json()["object"]["sha"], json!(MAIN_SHA));
    assert_eq!(made.json()["object"]["type"], json!("commit"));
    let read = api("/repos/fixture-dev/app/git/ref/tags/v1")
        .token(&dev)
        .send(fake.ports().api);
    assert_eq!(read.status, 200);
    assert_eq!(read.json()["object"]["sha"], json!(MAIN_SHA));
    assert_eq!(read.json()["ref"], json!("refs/tags/v1"));
    let again = send_json(
        &fake,
        "POST",
        refs,
        &dev,
        &json!({"ref": "refs/tags/v1", "sha": MAIN_SHA}),
    );
    assert_eq!(again.status, 422);
    assert_eq!(again.json()["message"], json!("Reference already exists"));
    let unknown = send_json(
        &fake,
        "POST",
        refs,
        &dev,
        &json!({"ref": "refs/tags/v2", "sha": REL_SHA}),
    );
    assert_eq!(unknown.status, 422);
    assert_eq!(unknown.json()["message"], json!("Object does not exist"));
    let missing = api("/repos/fixture-dev/app/git/ref/tags/v2")
        .token(&dev)
        .send(fake.ports().api);
    assert_eq!(missing.status, 404);
    assert_eq!(
        fake.git_refs(SLUG),
        [
            ("refs/heads/main".to_string(), MAIN_SHA.to_string()),
            ("refs/tags/v1".to_string(), MAIN_SHA.to_string()),
        ]
    );
}

#[test]
fn keys_need_the_scope() {
    let (fake, dev, _) = fake();
    let narrow = token("narrow");
    fake.add_token(&narrow, "fixture-dev-bot");
    fake.set_scopes(&narrow, &["repo"]);
    fake.set_scopes(&dev, &["repo", "admin:public_key"]);
    let port = fake.ports().api;
    let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeKeyForTheFake";
    let post = |t: &str| {
        send_json(
            &fake,
            "POST",
            "/user/keys",
            t,
            &json!({"title": "lab", "key": key}),
        )
    };
    let accepted = "admin:public_key, read:public_key, write:public_key";
    for refused in [
        api("/user/keys").token(&narrow).send(port),
        post(&narrow),
        api("/user/keys/20000")
            .method("DELETE")
            .token(&narrow)
            .send(port),
    ] {
        assert_eq!(refused.status, 404);
        assert_eq!(refused.header("X-Accepted-OAuth-Scopes"), Some(accepted));
        assert_eq!(refused.header("X-OAuth-Scopes"), Some("repo"));
    }
    assert!(fake.keys("fixture-dev-bot").is_empty());
    let made = post(&dev);
    assert_eq!(made.status, 201);
    assert_eq!(made.json()["id"], json!(20000));
    assert_eq!(made.json()["key"], json!(key));
    assert_eq!(made.json()["title"], json!("lab"));
    assert!(made.json()["created_at"].as_str().is_some());
    assert_eq!(
        made.header("X-OAuth-Scopes"),
        Some("repo, admin:public_key")
    );
    let listed = api("/user/keys").token(&dev).send(port);
    assert_eq!(listed.status, 200);
    assert_eq!(listed.json()[0]["id"], json!(20000));
    let twice = post(&dev);
    assert_eq!(twice.status, 422);
    assert_eq!(twice.json()["message"], json!("Validation Failed"));
    assert_eq!(
        twice.json()["errors"],
        json!([{"resource": "PublicKey", "code": "custom", "field": "key",
            "message": "key is already in use"}])
    );
    assert_eq!(fake.keys("fixture-dev-bot").len(), 1);
    let delete = || {
        api("/user/keys/20000")
            .method("DELETE")
            .token(&dev)
            .send(port)
            .status
    };
    assert_eq!(delete(), 204);
    assert_eq!(delete(), 404);
    assert!(fake.keys("fixture-dev-bot").is_empty());
    assert_eq!(api("/user/keys").send(port).status, 401);
}

#[test]
fn keys_page_and_belong_to_their_login() {
    let (fake, dev, reader) = fake();
    for n in 0..3 {
        fake.add_key(
            "fixture-dev-bot",
            &format!("k{n}"),
            &format!("ssh-ed25519 AAAA{n}"),
        );
    }
    let theirs = fake.add_key("fixture-reader", "r", "ssh-ed25519 BBBB");
    let port = fake.ports().api;
    let first = api("/user/keys?per_page=2").token(&dev).send(port);
    assert_eq!(first.json().as_array().map(Vec::len), Some(2));
    let next = first.link("next").expect("a next page");
    let rest = next.trim_start_matches(&format!("http://127.0.0.1:{port}"));
    assert_eq!(
        api(rest).token(&dev).send(port).json()[0]["title"],
        json!("k2")
    );
    let not_mine = api(&format!("/user/keys/{theirs}"))
        .method("DELETE")
        .token(&dev)
        .send(port);
    assert_eq!(not_mine.status, 404);
    assert_eq!(fake.keys("fixture-reader").len(), 1);
    let held = send_json(
        &fake,
        "POST",
        "/user/keys",
        &reader,
        &json!({"key": "ssh-ed25519 AAAA0 a comment"}),
    );
    assert_eq!(held.status, 422);
    let commented = fake.add_key("fixture-dev-bot", "c", "ssh-ed25519 CCCC host:alice");
    let kept = fake.keys("fixture-dev-bot");
    let kept = kept.iter().find(|k| k.id == commented).expect("the key");
    assert_eq!(kept.key, "ssh-ed25519 CCCC");
    let listed = api(&format!("/user/keys/{commented}"))
        .token(&dev)
        .send(port);
    assert_eq!(listed.json()["key"], json!("ssh-ed25519 CCCC"));
    let again = send_json(
        &fake,
        "POST",
        "/user/keys",
        &reader,
        &json!({"title": "c", "key": "ssh-ed25519 CCCC"}),
    );
    assert_eq!(again.status, 422);
    assert_eq!(
        again.json()["errors"][0]["message"],
        json!("key is already in use")
    );
    assert_eq!(fake.keys("fixture-reader").len(), 1);
}

#[test]
fn bodies_are_captured() {
    let (fake, dev, _) = fake();
    let sent = json!({"tag_name": "v1", "draft": true}).to_string();
    let made = api("/repos/fixture-dev/app/releases")
        .method("POST")
        .token(&dev)
        .header("Content-Type", "application/json")
        .body(sent.as_bytes())
        .send(fake.ports().api);
    let id = id_of(&made);
    upload(&fake, &dev, id, "a.bin", b"\x00not json\xff");
    let bodies = fake.bodies();
    assert_eq!(bodies.len(), 2);
    let seen = fake.seen();
    assert_eq!(seen[bodies[0].seen].method, "POST");
    assert_eq!(bodies[0].seen, 0);
    assert_eq!(bodies[0].json.as_deref(), Some(sent.as_str()));
    assert_eq!(bodies[0].len, u64::try_from(sent.len()).expect("a length"));
    assert_eq!(bodies[0].sha256, sha256(sent.as_bytes()));
    assert_eq!(bodies[1].seen, 1);
    assert_eq!(bodies[1].len, 10);
    assert_eq!(bodies[1].sha256, sha256(b"\x00not json\xff"));
    assert_eq!(bodies[1].json, None);
    fake.clear_seen();
    assert!(fake.bodies().is_empty());
    api("/repos/fixture-dev/app")
        .token(&dev)
        .send(fake.ports().api);
    let later = json!({"tag_name": "v2", "draft": true});
    create(&fake, &dev, &later);
    let bodies = fake.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0].seen, 1);
    assert_eq!(fake.seen()[bodies[0].seen].method, "POST");
    assert_eq!(bodies[0].json, Some(later.to_string()));
}

#[test]
fn a_body_over_the_bound_is_413_and_lands_nothing() {
    let (fake, dev, _) = fake();
    let id = id_of(&create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "draft": true}),
    ));
    fake.set_scopes(&dev, &["repo"]);
    let uploads = Match::uploads("/repos/fixture-dev/app/releases").method("POST");
    fake.after(uploads, 2, Change::Publish { release_id: id });
    let bound = 16 * 1024 * 1024;
    let whole = upload(&fake, &dev, id, "a.bin", &vec![7_u8; bound]);
    assert_eq!(whole.status, 201);
    assert!(fake.violations().is_empty(), "{:?}", fake.violations());
    assert!(fake.releases_of(SLUG)[0].draft);
    let over = upload(&fake, &dev, id, "b.bin", &vec![7_u8; bound + 1]);
    assert_eq!(over.status, 413);
    assert_eq!(over.header("X-OAuth-Scopes"), Some("repo"));
    assert!(over.header("X-GitHub-Request-Id").is_some());
    assert!(!fake.releases_of(SLUG)[0].draft, "the 413 was counted");
    let names: Vec<String> = fake.releases_of(SLUG)[0]
        .assets
        .iter()
        .map(|a| a.1.clone())
        .collect();
    assert_eq!(names, ["a.bin"]);
    assert_eq!(fake.violations().len(), 1);
    assert_eq!(fake.bodies().len(), 2);
    let on_api = api(&format!("/repos/fixture-dev/app/releases?seed={dev}"))
        .method("POST")
        .token(&dev)
        .body(&vec![b' '; bound + 1])
        .send(fake.ports().api);
    assert_eq!(on_api.status, 413);
    assert!(on_api.header("x-ratelimit-used").is_some());
    let violations = fake.violations();
    assert_eq!(
        violations.len(),
        3,
        "the token in the target, the bound: {violations:?}"
    );
    assert!(
        violations.iter().all(|v| !v.contains(&dev)),
        "{violations:?}"
    );
    assert_eq!(fake.releases_of(SLUG).len(), 1);
}

#[test]
fn a_body_cut_short_closes_and_lands_nothing() {
    let (fake, dev, _) = fake();
    let body = json!({"tag_name": "v1", "draft": true}).to_string();
    let head = format!(
        "POST /repos/fixture-dev/app/releases HTTP/1.1\r\nHost: x\r\nUser-Agent: s\r\n\
         X-GitHub-Api-Version: 2022-11-28\r\nAuthorization: Bearer {dev}\r\n\
         Content-Length: {}\r\n\r\n",
        body.len() + 5
    );
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body.as_bytes());
    let reply = send_cut(fake.ports().api, &bytes);
    assert_eq!(reply.status, 0);
    assert!(fake.releases_of(SLUG).is_empty());
    assert!(fake.bodies().is_empty());
    assert_eq!(fake.seen().len(), 1);
}

#[test]
fn make_latest_and_the_notes_are_what_was_sent() {
    let (fake, dev, _) = with_main();
    let id = id_of(&create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "draft": true, "body": "notes", "make_latest": "false"}),
    ));
    let target = format!("/repos/fixture-dev/app/releases/{id}");
    send_json(&fake, "PATCH", &target, &dev, &json!({"body": "new notes"}));
    let release = &fake.releases_of(SLUG)[0];
    assert_eq!(release.make_latest.as_deref(), Some("false"));
    assert_eq!(release.body, "new notes");
    send_json(
        &fake,
        "PATCH",
        &target,
        &dev,
        &json!({"make_latest": "true", "draft": false}),
    );
    let release = &fake.releases_of(SLUG)[0];
    assert_eq!(release.make_latest.as_deref(), Some("true"));
    assert!(!release.draft);
}

/// `DELETE git/refs/{name}` removes the ref (204), and a second is 422 "Reference does not
/// exist"; the `url` a create answers reads back (200). NEGATIVE CONTROL: the ref is listed
/// until the delete.
#[test]
fn refs_delete_and_read_by_their_url() {
    let (fake, dev, _) = with_main();
    let port = fake.ports().api;
    let made = send_json(
        &fake,
        "POST",
        "/repos/fixture-dev/app/git/refs",
        &dev,
        &json!({"ref": "refs/tags/v1", "sha": MAIN_SHA}),
    );
    assert_eq!(made.status, 201);
    let url = made.json()["url"].as_str().expect("a url").to_string();
    let path = url
        .strip_prefix(&format!("http://127.0.0.1:{port}"))
        .expect("the api origin");
    let read = api(path).token(&dev).send(port);
    assert_eq!(read.status, 200);
    assert_eq!(read.json()["object"]["sha"], json!(MAIN_SHA));
    assert_eq!(fake.git_refs(SLUG).len(), 2);
    let delete = || api(path).method("DELETE").token(&dev).send(port);
    assert_eq!(delete().status, 204);
    assert_eq!(
        fake.git_refs(SLUG),
        [("refs/heads/main".to_string(), MAIN_SHA.to_string())]
    );
    let again = delete();
    assert_eq!(again.status, 422);
    assert_eq!(again.json()["message"], json!("Reference does not exist"));
    assert_eq!(api(path).token(&dev).send(port).status, 404);
}

/// A ref name git's `check-ref-format` refuses, or one over the length cap, is 422
/// "Reference name is invalid" and lands nothing. NEGATIVE CONTROL: a well-formed nested name
/// is 201.
#[test]
fn a_malformed_ref_name_is_invalid() {
    let (fake, dev, _) = with_main();
    let long = format!("refs/heads/{}", "x".repeat(2000));
    for name in [
        "refs/heads/a b",
        "refs/heads/x.lock",
        "refs/heads/a..b",
        "refs/heads/a/",
        "refs/heads/.a",
        "refs/heads/a~1",
        "refs/heads/a.",
        long.as_str(),
    ] {
        let made = send_json(
            &fake,
            "POST",
            "/repos/fixture-dev/app/git/refs",
            &dev,
            &json!({"ref": name, "sha": MAIN_SHA}),
        );
        assert_eq!(made.status, 422, "{name}");
        assert_eq!(made.json()["message"], json!("Reference name is invalid"));
    }
    assert_eq!(fake.git_refs(SLUG).len(), 1);
    let nested = send_json(
        &fake,
        "POST",
        "/repos/fixture-dev/app/git/refs",
        &dev,
        &json!({"ref": "refs/heads/rel/v1.x", "sha": MAIN_SHA}),
    );
    assert_eq!(nested.status, 201);
}

/// A write past the token's rate limit is refused 403 before it is applied. NEGATIVE
/// CONTROL: the same POST one request earlier lands. The token stands one short of its limit
/// by `set_rate_used`, so the test makes two requests where climbing there made 5,001, each
/// a TIME_WAIT socket for 30 s (each request's own charge is `faults.rs`'s).
#[test]
fn a_write_past_the_limit_lands_nothing() {
    let (fake, dev, _) = fake();
    fake.set_rate_used(&dev, 4999);
    let landed = create(&fake, &dev, &json!({"tag_name": "v1", "draft": true}));
    assert_eq!(landed.status, 201);
    assert_eq!(landed.header("x-ratelimit-remaining"), Some("0"));
    let refused = create(&fake, &dev, &json!({"tag_name": "v2", "draft": true}));
    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["message"], json!("API rate limit exceeded."));
    let tags: Vec<String> = fake.releases_of(SLUG).into_iter().map(|r| r.tag).collect();
    assert_eq!(tags, ["v1"]);
    assert_eq!(fake.seen().len(), 2, "the test's own requests");
}

/// `make_latest:"false"` on a draft changes nothing, and a later publish with no
/// `make_latest` takes a latest that a `"false"` pinned. NEGATIVE CONTROL: a published
/// `"false"` keeps the latest where it was.
#[test]
fn make_latest_false_pins_no_later_publish() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    let latest = || {
        let reply = api("/repos/fixture-dev/app/releases/latest")
            .token(&dev)
            .send(port);
        (reply.status == 200).then(|| id_of(&reply))
    };
    let draft = create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "draft": true, "make_latest": "false"}),
    );
    assert_eq!(latest(), None);
    let v2 = id_of(&create(&fake, &dev, &json!({"tag_name": "v2"})));
    assert_eq!(latest(), Some(v2));
    let target = format!("/repos/fixture-dev/app/releases/{}", id_of(&draft));
    assert_eq!(
        api(&target).method("DELETE").token(&dev).send(port).status,
        204
    );
    assert_eq!(latest(), Some(v2));
    create(
        &fake,
        &dev,
        &json!({"tag_name": "v3", "make_latest": "false"}),
    );
    assert_eq!(latest(), Some(v2));
    let v4 = id_of(&create(&fake, &dev, &json!({"tag_name": "v4"})));
    assert_eq!(latest(), Some(v4));
}

/// A fixture release stays the latest past a published `"false"`, and a later publish with
/// no `make_latest` takes it.
#[test]
fn a_pinned_fixture_latest_yields_to_a_later_publish() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    let v0 = fake.add_release(SLUG, ReleaseSpec::published("v0", "2026-10-01T00:00:00Z"));
    let latest = || {
        id_of(
            &api("/repos/fixture-dev/app/releases/latest")
                .token(&dev)
                .send(port),
        )
    };
    create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "make_latest": "false"}),
    );
    assert_eq!(latest(), v0);
    let v2 = id_of(&create(&fake, &dev, &json!({"tag_name": "v2"})));
    assert_eq!(latest(), v2);
}

/// An upload's `name` is form-decoded: `+` is a space, `%2B` a `+`.
#[test]
fn an_upload_name_is_form_decoded() {
    let (fake, dev, _) = fake();
    let id = id_of(&create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "draft": true}),
    ));
    assert_eq!(
        upload(&fake, &dev, id, "a+b", b"1").json()["name"],
        json!("a b")
    );
    assert_eq!(
        upload(&fake, &dev, id, "a%2Bb", b"2").json()["name"],
        json!("a+b")
    );
}

/// The `releases/latest` answer of `SLUG` to `dev`.
fn latest_id(fake: &FakeGithub, dev: &str) -> u64 {
    id_of(
        &api("/repos/fixture-dev/app/releases/latest")
            .token(dev)
            .send(fake.ports().api),
    )
}

/// A published `"false"` pins the latest `releases/latest` answers now, not a newer
/// release the listing lag hides.
#[test]
fn make_latest_false_pins_the_latest_the_lag_shows() {
    let (fake, dev, _) = fake();
    let v1 = fake.add_release(SLUG, ReleaseSpec::published("v1", "2026-09-01T00:00:00Z"));
    fake.set_listing_lag(SLUG, true);
    fake.add_release(SLUG, ReleaseSpec::published("v0", "2026-10-01T00:00:00Z"));
    fake.set_listing_lag(SLUG, false);
    assert_eq!(latest_id(&fake, &dev), v1);
    let v2 = create(
        &fake,
        &dev,
        &json!({"tag_name": "v2", "make_latest": "false"}),
    );
    assert_eq!(v2.status, 201);
    assert_eq!(latest_id(&fake, &dev), v1);
}

/// A PATCH that publishes a draft sets `published_at` to `created_at`, whatever the draft
/// held.
#[test]
fn a_publish_by_patch_sets_published_at_to_created_at() {
    let (fake, dev, _) = fake();
    let spec = ReleaseSpec {
        published_at: Some("2026-09-15T00:00:00Z".to_string()),
        ..ReleaseSpec::draft("v1", "2026-09-01T00:00:00Z")
    };
    let id = fake.add_release(SLUG, spec);
    let target = format!("/repos/fixture-dev/app/releases/{id}");
    let published = send_json(&fake, "PATCH", &target, &dev, &json!({"draft": false}));
    assert_eq!(published.status, 200);
    assert_eq!(
        published.json()["published_at"].as_str(),
        Some("2026-09-01T00:00:00Z")
    );
}

/// A write that makes the pinned latest a prerelease hands the latest back to the
/// automatic rule; a fixture's pin of a prerelease outlasts a PATCH that leaves it one.
#[test]
fn a_pinned_latest_made_a_prerelease_is_no_longer_the_latest() {
    let (fake, dev, _) = fake();
    let v0 = fake.add_release(SLUG, ReleaseSpec::published("v0", "2026-10-01T00:00:00Z"));
    let v1 = id_of(&create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "make_latest": "true"}),
    ));
    assert_eq!(latest_id(&fake, &dev), v1);
    let target = format!("/repos/fixture-dev/app/releases/{v1}");
    let made = send_json(&fake, "PATCH", &target, &dev, &json!({"prerelease": true}));
    assert_eq!(made.status, 200);
    assert_eq!(latest_id(&fake, &dev), v0);
    fake.set_latest(SLUG, Some(v1));
    let renamed = send_json(&fake, "PATCH", &target, &dev, &json!({"name": "one"}));
    assert_eq!(renamed.status, 200);
    assert_eq!(latest_id(&fake, &dev), v1);
}

/// A publish with no `make_latest` takes an automatic latest that names another release,
/// and `"legacy"` hands the latest back to the automatic rule.
#[test]
fn a_publish_with_no_make_latest_takes_the_latest() {
    let (fake, dev, _) = fake();
    let v1 = id_of(&create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "draft": true}),
    ));
    let v2 = id_of(&create(&fake, &dev, &json!({"tag_name": "v2"})));
    assert_eq!(latest_id(&fake, &dev), v2);
    let first = format!("/repos/fixture-dev/app/releases/{v1}");
    let published = send_json(&fake, "PATCH", &first, &dev, &json!({"draft": false}));
    assert_eq!(published.status, 200);
    assert_eq!(latest_id(&fake, &dev), v1);
    let second = format!("/repos/fixture-dev/app/releases/{v2}");
    let legacy = send_json(
        &fake,
        "PATCH",
        &second,
        &dev,
        &json!({"make_latest": "legacy"}),
    );
    assert_eq!(legacy.status, 200);
    assert_eq!(latest_id(&fake, &dev), v2);
}

/// A publish whose `tag_name` no ref can carry is 422 `invalid`, and makes no ref.
#[test]
fn a_tag_name_no_ref_can_carry_is_invalid() {
    let (fake, dev, _) = with_main();
    let refs = fake.git_refs(SLUG);
    let at_once = create(&fake, &dev, &json!({"tag_name": "v1 b"}));
    assert_eq!(at_once.status, 422);
    assert_eq!(
        at_once.json()["errors"][0]["field"].as_str(),
        Some("tag_name")
    );
    assert_eq!(code(&at_once), json!("invalid"));
    assert!(fake.releases_of(SLUG).is_empty());
    let id = id_of(&create(
        &fake,
        &dev,
        &json!({"tag_name": "v1 b", "draft": true}),
    ));
    let target = format!("/repos/fixture-dev/app/releases/{id}");
    let publish = send_json(&fake, "PATCH", &target, &dev, &json!({"draft": false}));
    assert_eq!(publish.status, 422);
    assert!(fake.releases_of(SLUG)[0].draft);
    assert_eq!(fake.git_refs(SLUG), refs);
}

/// A body more than twice the read bound still gets its 413: the fake drops the rest of
/// the declared length before it hangs up.
#[test]
fn a_body_far_over_the_bound_still_gets_its_413() {
    let (fake, dev, _) = fake();
    let id = id_of(&create(
        &fake,
        &dev,
        &json!({"tag_name": "v1", "draft": true}),
    ));
    let over = upload(&fake, &dev, id, "big.bin", &vec![7_u8; 40 << 20]);
    assert_eq!(over.status, 413);
    assert!(fake.releases_of(SLUG)[0].assets.is_empty());
}
