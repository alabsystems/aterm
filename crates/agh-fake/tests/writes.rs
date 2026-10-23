// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Write route tests for release creation, editing, deletion, and asset management.

use agh_fake::{AssetSpec, FakeGithub, ReleaseSpec, RepoSpec, token};
use aterm_json::json;

use crate::raw::api;

const SLUG: &str = "fixture-dev/app";

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
