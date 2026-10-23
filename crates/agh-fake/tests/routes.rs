// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Each route's answer, visibility, ETags, rate and pagination.

use agh_fake::{
    AssetSpec, Change, CommitSpec, CompareSpec, DigestSpec, FakeGithub, ReleaseSpec, RepoSpec,
    token,
};
use aterm_json::{Value, json};

use crate::raw::{api, plain};

/// A fake with `fixture-dev-bot` (push) and `fixture-reader` (pull) on the public
/// `fixture-dev/app`, their tokens registered.
fn fake() -> (FakeGithub, String, String) {
    let fake = FakeGithub::start();
    let (dev, reader) = (token("dev"), token("reader"));
    fake.add_token(&dev, "fixture-dev-bot");
    fake.add_token(&reader, "fixture-reader");
    fake.add_repo(
        "fixture-dev/app",
        RepoSpec::public()
            .push("fixture-dev-bot")
            .pull("fixture-reader"),
    );
    (fake, dev, reader)
}

/// The GET's array as a list of `key` values.
fn values(reply: &crate::raw::Reply, key: &str) -> Vec<Value> {
    reply
        .json()
        .as_array()
        .expect("an array")
        .iter()
        .map(|v| v[key].clone())
        .collect()
}

#[test]
fn repo_answers_ids_owner_and_permissions_per_login() {
    let (fake, dev, reader) = fake();
    let api_port = fake.ports().api;
    let push = api("/repos/fixture-dev/app").token(&dev).send(api_port);
    assert_eq!(push.status, 200);
    let v = push.json();
    assert_eq!(v["id"], json!(1000));
    assert_eq!(v["full_name"], json!("fixture-dev/app"));
    assert_eq!(v["owner"]["login"], json!("fixture-dev"));
    assert_eq!(v["owner"]["type"], json!("Organization"));
    assert_eq!(v["default_branch"], json!("main"));
    assert_eq!(v["visibility"], json!("public"));
    assert_eq!(
        v["permissions"],
        json!({"admin": false, "maintain": false, "push": true, "triage": true, "pull": true})
    );
    let pull = api("/repos/fixture-dev/app")
        .token(&reader)
        .send(api_port)
        .json();
    assert_eq!(pull["permissions"]["push"], json!(false));
    assert_eq!(pull["permissions"]["triage"], json!(false));
    assert_eq!(pull["permissions"]["pull"], json!(true));
    let anonymous = api("/repos/fixture-dev/app").send(api_port).json();
    assert!(anonymous.get("permissions").is_none());
    assert_eq!(anonymous["owner"]["id"], json!(102));
    let numeric = api("/repositories/1000").send(api_port).json();
    assert_eq!(numeric["full_name"], json!("fixture-dev/app"));
    fake.add_repo("fixture-dev-bot/own", RepoSpec::public());
    let own = api("/repos/fixture-dev-bot/own").send(api_port).json();
    assert_eq!(own["owner"]["type"], json!("User"));
    assert_eq!(own["owner"]["id"], json!(100));
    assert!(fake.violations().is_empty());
}

#[test]
fn a_private_repo_is_404_to_anonymous_and_to_strangers() {
    let (fake, dev, reader) = fake();
    let stranger = token("stranger");
    fake.add_token(&stranger, "fixture-ext-bot");
    fake.add_repo(
        "fixture-dev/secret",
        RepoSpec::private().pull("fixture-reader"),
    );
    let port = fake.ports().api;
    let not_found = json!({"message": "Not Found", "documentation_url": "https://docs.github.com/rest", "status": "404"});
    let anonymous = api("/repos/fixture-dev/secret").send(port);
    assert_eq!(
        (anonymous.status, anonymous.json()),
        (404, not_found.clone())
    );
    let other = api("/repos/fixture-dev/secret").token(&stranger).send(port);
    assert_eq!((other.status, other.json()), (404, not_found));
    assert_eq!(
        api("/repos/fixture-dev/secret/releases")
            .token(&dev)
            .send(port)
            .status,
        404
    );
    let member = api("/repos/fixture-dev/secret").token(&reader).send(port);
    assert_eq!(member.status, 200);
    assert_eq!(member.json()["private"], json!(true));
    assert_eq!(member.json()["visibility"], json!("private"));
}

#[test]
fn an_unknown_token_or_scheme_is_bad_credentials() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    let bad = json!({"message": "Bad credentials", "documentation_url": "https://docs.github.com/rest", "status": "401"});
    let unknown = api("/repos/fixture-dev/app")
        .token(&token("unknown"))
        .send(port);
    assert_eq!((unknown.status, unknown.json()), (401, bad.clone()));
    let basic = api("/repos/fixture-dev/app")
        .scheme("Basic", &dev)
        .send(port);
    assert_eq!((basic.status, basic.json()), (401, bad.clone()));
    let lower = api("/repos/fixture-dev/app")
        .scheme("bearer", &dev)
        .send(port);
    assert_eq!((lower.status, lower.json()), (401, bad.clone()));
    let two_spaces = api("/repos/fixture-dev/app")
        .scheme("Bearer", &format!(" {dev}"))
        .send(port);
    assert_eq!((two_spaces.status, two_spaces.json()), (401, bad));
    let token_scheme = api("/repos/fixture-dev/app")
        .scheme("token", &dev)
        .send(port);
    assert_eq!(token_scheme.status, 200);
    assert_eq!(token_scheme.json()["permissions"]["push"], json!(true));
}

#[test]
fn api_answers_count_request_ids_from_one() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    let ids: Vec<String> = (0..3)
        .map(|_| {
            api("/repos/fixture-dev/app")
                .send(port)
                .header("x-github-request-id")
                .expect("a request id")
                .to_string()
        })
        .collect();
    assert_eq!(ids, ["FAKE:1", "FAKE:2", "FAKE:3"]);
}

#[test]
fn rate_headers_decrement_per_token_and_resource() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    let rate = |r: &crate::raw::Reply| {
        [
            "x-ratelimit-limit",
            "x-ratelimit-remaining",
            "x-ratelimit-reset",
            "x-ratelimit-used",
            "x-ratelimit-resource",
        ]
        .map(|h| r.header(h).expect("a rate header").to_string())
    };
    assert_eq!(
        rate(&api("/repos/fixture-dev/app").send(port)),
        ["60", "59", "1760003600", "1", "core"]
    );
    assert_eq!(rate(&api("/repos/fixture-dev/app").send(port))[1], "58");
    assert_eq!(
        rate(&api("/repos/fixture-dev/app").token(&dev).send(port)),
        ["5000", "4999", "1760003600", "1", "core"]
    );
    assert_eq!(
        rate(&api("/search/code?q=x").send(port)),
        ["60", "59", "1760003600", "1", "search"]
    );
    let limits = api("/rate_limit").send(port);
    assert_eq!(rate(&limits)[1], "58");
    let v = limits.json();
    assert_eq!(v["resources"]["core"]["remaining"], json!(58));
    assert_eq!(v["resources"]["core"]["used"], json!(2));
    assert_eq!(v["resources"]["search"]["remaining"], json!(59));
    assert_eq!(v["resources"]["graphql"]["limit"], json!(0));
    assert_eq!(v["rate"], v["resources"]["core"]);
    assert_eq!(rate(&api("/rate_limit").send(port))[1], "58");
    let mine = api("/rate_limit").token(&dev).send(port).json();
    assert_eq!(mine["resources"]["core"]["remaining"], json!(4999));
    assert_eq!(mine["resources"]["graphql"]["limit"], json!(5000));
}

#[test]
fn a_json_get_carries_an_etag_and_a_match_answers_304() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    let first = api("/repos/fixture-dev/app").token(&dev).send(port);
    let digest = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&first.body));
    let etag = format!("W/\"{}\"", &digest[..16]);
    assert_eq!(first.header("etag"), Some(etag.as_str()));
    assert_eq!(
        first.header("content-length"),
        Some(first.body.len().to_string().as_str())
    );
    let again = api("/repos/fixture-dev/app")
        .token(&dev)
        .header("If-None-Match", &etag)
        .send(port);
    assert_eq!(again.status, 304);
    assert!(again.body.is_empty());
    assert_eq!(again.header("content-length"), None);
    assert_eq!(again.header("etag"), Some(etag.as_str()));
    assert_eq!(again.header("x-ratelimit-remaining"), Some("4999"));
    let anon = api("/repos/fixture-dev/app").send(port);
    let anon_tag = anon.header("etag").expect("an etag").to_string();
    let anon_304 = api("/repos/fixture-dev/app")
        .header("If-None-Match", &anon_tag)
        .send(port);
    assert_eq!(anon_304.status, 304);
    assert_eq!(anon_304.header("x-ratelimit-remaining"), Some("58"));
    let head = api("/repos/fixture-dev/app")
        .token(&dev)
        .method("HEAD")
        .send(port);
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(
        head.header("content-length"),
        Some(first.body.len().to_string().as_str())
    );
    assert!(fake.violations().is_empty());
}

#[test]
fn releases_are_newest_first_drafts_to_push_only_and_paginated_by_id() {
    let (fake, dev, reader) = fake();
    let port = fake.ports().api;
    let r1 = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"),
    );
    let r2 = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v2", "2026-10-02T00:00:00Z"),
    );
    let r3 = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v2b", "2026-10-02T00:00:00Z"),
    );
    let d = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::draft("v3", "2026-10-03T00:00:00Z"),
    );
    assert_eq!((r1, d), (5000, 5003));
    let anonymous = api("/repos/fixture-dev/app/releases").send(port);
    assert_eq!(values(&anonymous, "id"), [json!(r3), json!(r2), json!(r1)]);
    assert_eq!(anonymous.header("link"), None);
    let pull = api("/repos/fixture-dev/app/releases")
        .token(&reader)
        .send(port);
    assert_eq!(values(&pull, "id").len(), 3);
    let first = api("/repos/fixture-dev/app/releases?per_page=2")
        .token(&dev)
        .send(port);
    assert_eq!(values(&first, "id"), [json!(d), json!(r3)]);
    let next = format!("http://127.0.0.1:{port}/repositories/1000/releases?per_page=2&page=2");
    assert_eq!(first.link("next"), Some(next.clone()));
    assert_eq!(first.link("last"), Some(next.clone()));
    assert_eq!(first.link("prev"), None);
    let path = next.trim_start_matches(&format!("http://127.0.0.1:{port}"));
    let second = api(path).token(&dev).send(port);
    assert_eq!(values(&second, "id"), [json!(r2), json!(r1)]);
    assert_eq!(second.link("next"), None);
    assert_eq!(
        second.link("first"),
        Some(format!(
            "http://127.0.0.1:{port}/repositories/1000/releases?per_page=2&page=1"
        ))
    );
}

#[test]
fn a_release_is_read_by_id_by_tag_and_as_latest() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    let old = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"),
    );
    let pre = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v2-rc", "2026-10-02T00:00:00Z").prerelease(true),
    );
    let draft = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::draft("v3", "2026-10-03T00:00:00Z"),
    );
    let twin = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::draft("v3", "2026-10-03T00:00:00Z"),
    );
    let by_id = |id: u64, t: Option<&str>| {
        let req = api(&format!("/repos/fixture-dev/app/releases/{id}"));
        t.map_or(req.clone(), |t| req.token(t)).send(port)
    };
    assert_eq!(by_id(old, None).json()["tag_name"], json!("v1"));
    assert_eq!(by_id(draft, None).status, 404);
    assert_eq!(by_id(draft, Some(&dev)).json()["draft"], json!(true));
    assert_eq!(by_id(twin, Some(&dev)).json()["tag_name"], json!("v3"));
    assert_eq!(by_id(9, None).status, 404);
    let tag = |t: &str| {
        api(&format!("/repos/fixture-dev/app/releases/tags/{t}"))
            .token(&dev)
            .send(port)
    };
    assert_eq!(tag("v1").json()["id"], json!(old));
    assert_eq!(tag("v2-rc").json()["id"], json!(pre));
    assert_eq!(tag("v3").status, 404);
    let latest = || api("/repos/fixture-dev/app/releases/latest").send(port);
    assert_eq!(latest().json()["id"], json!(old));
    fake.set_latest("fixture-dev/app", Some(pre));
    assert_eq!(latest().json()["id"], json!(pre));
    fake.set_latest("fixture-dev/app", None);
    assert_eq!(latest().status, 404);
    fake.apply(Change::SetLatest {
        slug: "fixture-dev/app".into(),
        release_id: Some(old),
    });
    assert_eq!(latest().json()["id"], json!(old));
    let slashed = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("pkg/v1 b", "2026-10-04T00:00:00Z"),
    );
    assert_eq!(tag("pkg/v1%20b").json()["id"], json!(slashed));
}

#[test]
fn release_and_asset_objects_carry_their_fields() {
    let (fake, dev, reader) = fake();
    let ports = fake.ports();
    let release = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::draft("v1", "2026-10-01T00:00:00Z"),
    );
    let bytes = b"agh asset bytes";
    let a = fake.add_asset(release, AssetSpec::uploaded("a.tar.gz", bytes));
    let null = fake.add_asset(
        release,
        AssetSpec {
            digest: DigestSpec::Null,
            ..AssetSpec::uploaded("b.bin", b"b")
        },
    );
    let wrong = fake.add_asset(
        release,
        AssetSpec {
            digest: DigestSpec::Wrong,
            ..AssetSpec::uploaded("c.bin", b"c")
        },
    );
    assert_eq!((a, null, wrong), (9000, 9001, 9002));
    let v = api(&format!("/repos/fixture-dev/app/releases/{release}"))
        .token(&dev)
        .send(ports.api)
        .json();
    assert_eq!(v["published_at"], Value::Null);
    assert_eq!(v["created_at"], json!("2026-10-01T00:00:00Z"));
    assert_eq!(v["prerelease"], json!(false));
    assert_eq!(
        v["url"],
        json!(format!(
            "http://127.0.0.1:{}/repos/fixture-dev/app/releases/{release}",
            ports.api
        ))
    );
    assert_eq!(v["immutable"], json!(false));
    assert_eq!(v["target_commitish"], json!("main"));
    assert_eq!(v["name"], Value::Null);
    assert_eq!(v["body"], Value::Null);
    assert_eq!(
        v["html_url"],
        json!(format!(
            "http://127.0.0.1:{}/fixture-dev/app/releases/tag/v1",
            ports.web
        ))
    );
    let assets = v["assets"].as_array().expect("assets");
    let first = &assets[0];
    let digest = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes));
    assert_eq!(first["digest"], json!(format!("sha256:{digest}")));
    assert_eq!(first["size"], json!(bytes.len()));
    assert_eq!(first["state"], json!("uploaded"));
    assert_eq!(first["content_type"], json!("application/octet-stream"));
    assert_eq!(first["updated_at"], json!("2026-10-01T00:00:00Z"));
    assert_eq!(first["created_at"], json!("2026-10-01T00:00:00Z"));
    assert_eq!(first["download_count"], json!(0));
    assert_eq!(
        first["browser_download_url"],
        json!(format!(
            "http://127.0.0.1:{}/fixture-dev/app/releases/download/v1/a.tar.gz",
            ports.web
        ))
    );
    assert_eq!(assets[1]["digest"], Value::Null);
    let other = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(b"c"));
    let wrong_digest = assets[2]["digest"].as_str().expect("a digest");
    assert!(wrong_digest.starts_with("sha256:"));
    assert_ne!(wrong_digest, format!("sha256:{other}"));
    let path = format!("/repos/fixture-dev/app/releases/assets/{a}");
    let octet = |r: crate::raw::Req| r.header("Accept", "application/octet-stream");
    assert_eq!(api(&path).send(ports.api).status, 404);
    assert_eq!(api(&path).token(&reader).send(ports.api).status, 404);
    assert_eq!(api(&path).token(&dev).send(ports.api).status, 200);
    assert_eq!(octet(api(&path)).send(ports.api).status, 404);
    assert_eq!(octet(api(&path)).token(&reader).send(ports.api).status, 404);
    assert_eq!(octet(api(&path)).token(&dev).send(ports.api).status, 302);
}

#[test]
fn an_asset_answers_json_or_a_302_to_the_cdn_which_serves_and_counts() {
    let (fake, dev, _) = fake();
    let ports = fake.ports();
    let release = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"),
    );
    let id = fake.add_asset(release, AssetSpec::uploaded("a.bin", b"0123456789"));
    let path = format!("/repos/fixture-dev/app/releases/assets/{id}");
    assert_eq!(api(&path).send(ports.api).json()["name"], json!("a.bin"));
    let hop = api(&path)
        .token(&dev)
        .header("Accept", "application/octet-stream")
        .send(ports.api);
    assert_eq!(hop.status, 302);
    let location = hop.header("location").expect("a location").to_string();
    let prefix = format!("http://127.0.0.1:{}/assets/{id}?sig=", ports.cdn);
    let sig = location.strip_prefix(&prefix).expect("the CDN form");
    assert!(aterm_codec::hex::is_lower(sig, 8));
    let target = location.trim_start_matches(&format!("http://127.0.0.1:{}", ports.cdn));
    let head = plain(target).method("HEAD").send(ports.cdn);
    assert_eq!(
        (head.status, head.header("content-length")),
        (200, Some("10"))
    );
    let bytes = plain(target).send(ports.cdn);
    assert_eq!(bytes.status, 200);
    assert_eq!(bytes.body, b"0123456789");
    assert_eq!(
        bytes.header("content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(
        plain(&format!("/assets/{id}?sig=00000000"))
            .send(ports.cdn)
            .status,
        403
    );
    assert_eq!(
        api(&path).send(ports.api).json()["download_count"],
        json!(1)
    );
    assert!(fake.violations().is_empty());
}

#[test]
fn the_web_download_hops_to_the_cdn_only_for_a_published_public_release() {
    let (fake, _, _) = fake();
    let ports = fake.ports();
    let published = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"),
    );
    let draft = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::draft("v2", "2026-10-02T00:00:00Z"),
    );
    let id = fake.add_asset(published, AssetSpec::uploaded("a.bin", b"a"));
    fake.add_asset(draft, AssetSpec::uploaded("a.bin", b"a"));
    fake.add_repo("fixture-dev/secret", RepoSpec::private());
    let hidden = fake.add_release(
        "fixture-dev/secret",
        ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"),
    );
    fake.add_asset(hidden, AssetSpec::uploaded("a.bin", b"a"));
    let web = |p: &str, m: &str| plain(p).method(m).send(ports.web);
    for method in ["GET", "HEAD"] {
        let hop = web("/fixture-dev/app/releases/download/v1/a.bin", method);
        assert_eq!(hop.status, 302);
        let location = hop.header("location").expect("a location");
        assert!(location.starts_with(&format!("http://127.0.0.1:{}/assets/{id}?sig=", ports.cdn)));
    }
    assert_eq!(
        web("/fixture-dev/app/releases/download/v2/a.bin", "GET").status,
        404
    );
    assert_eq!(
        web("/fixture-dev/app/releases/download/v1/b.bin", "GET").status,
        404
    );
    assert_eq!(
        web("/fixture-dev/secret/releases/download/v1/a.bin", "GET").status,
        404
    );
    let slashed = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("pkg/v1", "2026-10-03T00:00:00Z"),
    );
    let slashed_id = fake.add_asset(slashed, AssetSpec::uploaded("a b.bin", b"s"));
    let v = crate::raw::api("/repos/fixture-dev/app/releases/tags/pkg/v1").send(ports.api);
    let url = v.json()["assets"][0]["browser_download_url"]
        .as_str()
        .expect("a download url")
        .to_string();
    let path = url
        .strip_prefix(&format!("http://127.0.0.1:{}", ports.web))
        .expect("the web origin");
    assert_eq!(
        path,
        "/fixture-dev/app/releases/download/pkg%2Fv1/a%20b.bin"
    );
    assert_eq!(
        v.json()["html_url"],
        json!(format!(
            "http://127.0.0.1:{}/fixture-dev/app/releases/tag/pkg%2Fv1",
            ports.web
        ))
    );
    let cdn = format!("http://127.0.0.1:{}/assets/{slashed_id}?sig=", ports.cdn);
    for p in [path, "/fixture-dev/app/releases/download/pkg/v1/a%20b.bin"] {
        let hop = web(p, "GET");
        assert_eq!(hop.status, 302, "{p}");
        assert!(
            hop.header("location")
                .expect("a location")
                .starts_with(&cdn)
        );
    }
    assert!(fake.violations().is_empty());
}

#[test]
fn contents_answer_raw_bytes_a_base64_object_or_a_directory() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    let long: Vec<u8> = (0u8..100).collect();
    fake.put_file("fixture-dev/app", "main", "README.md", b"hello\n");
    fake.put_file("fixture-dev/app", "main", "src/lib.rs", &long);
    fake.put_file("fixture-dev/app", "main", "src/a b.txt", b"spaced");
    fake.put_file("fixture-dev/app", "main", "src/deep/x.rs", b"x");
    fake.put_file("fixture-dev/app", "v1", "README.md", b"old\n");
    let raw = |p: &str| {
        api(p)
            .header("Accept", "application/vnd.github.raw+json")
            .send(port)
    };
    let readme = raw("/repos/fixture-dev/app/contents/README.md");
    assert_eq!(readme.body, b"hello\n");
    assert_eq!(
        readme.header("content-type"),
        Some("application/vnd.github.raw+json; charset=utf-8")
    );
    assert_eq!(
        raw("/repos/fixture-dev/app/contents/README.md?ref=v1").body,
        b"old\n"
    );
    assert_eq!(
        raw("/repos/fixture-dev/app/contents/src/a%20b.txt").body,
        b"spaced"
    );
    let object = api("/repos/fixture-dev/app/contents/src/lib.rs")
        .send(port)
        .json();
    assert_eq!(object["type"], json!("file"));
    assert_eq!(object["name"], json!("lib.rs"));
    assert_eq!(object["path"], json!("src/lib.rs"));
    assert_eq!(object["size"], json!(100));
    assert_eq!(object["encoding"], json!("base64"));
    assert!(aterm_codec::hex::is_lower(
        object["sha"].as_str().expect("a sha"),
        40
    ));
    let web = format!("http://127.0.0.1:{}/", fake.ports().web);
    assert!(
        object["download_url"]
            .as_str()
            .expect("a download url")
            .starts_with(&web)
    );
    let content = object["content"].as_str().expect("content");
    let widths: Vec<usize> = content
        .trim_end_matches('\n')
        .split('\n')
        .map(str::len)
        .collect();
    assert_eq!(widths, [60, 60, 16]);
    let joined: String = content.split('\n').collect();
    assert_eq!(aterm_codec::base64::decode(&joined).expect("base64"), long);
    for listing in [
        api("/repos/fixture-dev/app/contents/src").send(port),
        raw("/repos/fixture-dev/app/contents/src"),
    ] {
        assert_eq!(
            listing.header("content-type"),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(
            values(&listing, "name"),
            [json!("a b.txt"), json!("deep"), json!("lib.rs")]
        );
        assert_eq!(
            values(&listing, "type"),
            [json!("file"), json!("dir"), json!("file")]
        );
    }
    let root = api("/repos/fixture-dev/app/contents/").send(port);
    assert_eq!(values(&root, "path"), [json!("README.md"), json!("src")]);
    assert_eq!(
        api("/repos/fixture-dev/app/contents/nope.md")
            .send(port)
            .status,
        404
    );
    let unknown = api("/repos/fixture-dev/app/contents/?ref=nope").send(port);
    assert_eq!(unknown.status, 404);
    let message = unknown.json()["message"]
        .as_str()
        .expect("a message")
        .to_string();
    assert!(message.contains("nope"));
}

#[test]
fn a_tree_reads_a_branch_a_tag_or_a_sha_with_truncated() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    let sha = "a".repeat(40);
    fake.set_branch("fixture-dev/app", "main", &sha);
    fake.set_tag("fixture-dev/app", "v1", &sha);
    fake.put_file("fixture-dev/app", "main", "src/lib.rs", b"x");
    fake.put_file("fixture-dev/app", "main", "Cargo.toml", b"y");
    let paths = |r: &crate::raw::Reply| -> Vec<Value> {
        r.json()["tree"]
            .as_array()
            .expect("a tree")
            .iter()
            .map(|e| e["path"].clone())
            .collect()
    };
    for reference in ["main", "v1", sha.as_str()] {
        let tree = api(&format!(
            "/repos/fixture-dev/app/git/trees/{reference}?recursive=1"
        ))
        .send(port);
        assert_eq!(tree.status, 200, "{reference}");
        assert_eq!(
            paths(&tree),
            [json!("Cargo.toml"), json!("src"), json!("src/lib.rs")]
        );
        assert_eq!(tree.json()["truncated"], json!(false));
    }
    let src_sha = |r: &str| -> Value {
        api(&format!("/repos/fixture-dev/app/git/trees/{r}?recursive=1"))
            .send(port)
            .json()["tree"][1]["sha"]
            .clone()
    };
    let by_sha = src_sha(&sha);
    assert_eq!(src_sha("main"), by_sha);
    assert_eq!(src_sha("v1"), by_sha);
    for r in ["main", sha.as_str()] {
        let listing = api(&format!("/repos/fixture-dev/app/contents/?ref={r}")).send(port);
        assert_eq!(listing.json()[1]["sha"], by_sha, "{r}");
    }
    for value in ["0", "false", ""] {
        let tree = api(&format!(
            "/repos/fixture-dev/app/git/trees/main?recursive={value}"
        ))
        .send(port);
        assert_eq!(
            paths(&tree),
            [json!("Cargo.toml"), json!("src"), json!("src/lib.rs")],
            "{value:?}"
        );
    }
    let top = api("/repos/fixture-dev/app/git/trees/main").send(port);
    assert_eq!(paths(&top), [json!("Cargo.toml"), json!("src")]);
    assert_eq!(top.json()["tree"][1]["type"], json!("tree"));
    assert_eq!(top.json()["tree"][0]["type"], json!("blob"));
    fake.set_tree_truncated("fixture-dev/app", true);
    let truncated = api("/repos/fixture-dev/app/git/trees/main?recursive=1").send(port);
    assert_eq!(truncated.json()["truncated"], json!(true));
    assert_eq!(
        api("/repos/fixture-dev/app/git/trees/nope?recursive=1")
            .send(port)
            .status,
        404
    );
}

#[test]
fn a_contents_url_encodes_its_path_and_ref_and_leads_back() {
    let (fake, _, _) = fake();
    let ports = fake.ports();
    fake.put_file("fixture-dev/app", "main", "docs/a b#c.md", b"odd");
    fake.put_file("fixture-dev/app", "pkg/v1", "x y.md", b"tagged");
    let object = api("/repos/fixture-dev/app/contents/docs/a%20b%23c.md")
        .send(ports.api)
        .json();
    assert_eq!(object["path"], json!("docs/a b#c.md"));
    let url = object["url"].as_str().expect("a url").to_string();
    let base = format!("http://127.0.0.1:{}", ports.api);
    assert_eq!(
        url,
        format!("{base}/repos/fixture-dev/app/contents/docs/a%20b%23c.md?ref=main")
    );
    assert_eq!(
        object["download_url"],
        json!(format!(
            "http://127.0.0.1:{}/fixture-dev/app/raw/main/docs/a%20b%23c.md",
            ports.web
        ))
    );
    let again = api(url.trim_start_matches(&base)).send(ports.api);
    assert_eq!(again.status, 200);
    assert_eq!(again.json(), object);
    let listing = api("/repos/fixture-dev/app/contents/docs").send(ports.api);
    assert_eq!(listing.json()[0]["url"], json!(url));
    let tagged = api("/repos/fixture-dev/app/contents/x%20y.md?ref=pkg%2Fv1")
        .send(ports.api)
        .json();
    let tagged_url = tagged["url"].as_str().expect("a url").to_string();
    assert_eq!(
        tagged_url,
        format!("{base}/repos/fixture-dev/app/contents/x%20y.md?ref=pkg%2Fv1")
    );
    assert_eq!(
        api(tagged_url.trim_start_matches(&base))
            .send(ports.api)
            .json(),
        tagged
    );
    assert!(
        tagged["download_url"]
            .as_str()
            .expect("a download url")
            .ends_with("/fixture-dev/app/raw/pkg/v1/x%20y.md")
    );
}

#[test]
fn the_default_branch_with_no_sha_set_reads_as_its_newest_commit() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    fake.add_commit(
        "fixture-dev/app",
        commit('a', "2026-10-01T00:00:00Z", None, "x"),
    );
    fake.add_commit(
        "fixture-dev/app",
        commit('b', "2026-10-02T00:00:00Z", None, "src/y.rs"),
    );
    fake.put_file("fixture-dev/app", "main", "src/y.rs", b"y");
    let head = api("/repos/fixture-dev/app/commits?sha=main")
        .send(port)
        .json()[0]["sha"]
        .as_str()
        .expect("a sha")
        .to_string();
    assert_eq!(head, "b".repeat(40));
    fake.put_file("fixture-dev/app", &head, "src/y.rs", b"y2");
    let tree = |r: &str| {
        api(&format!("/repos/fixture-dev/app/git/trees/{r}?recursive=1"))
            .send(port)
            .json()
    };
    let (by_name, by_sha) = (tree("main"), tree(&head));
    assert_eq!(by_name["sha"], by_sha["sha"]);
    assert_eq!(by_name["tree"], by_sha["tree"]);
    let paths: Vec<Value> = by_name["tree"]
        .as_array()
        .expect("a tree")
        .iter()
        .map(|e| e["path"].clone())
        .collect();
    assert_eq!(paths, [json!("src"), json!("src/y.rs")]);
    for r in ["main", head.as_str()] {
        let raw = api(&format!("/repos/fixture-dev/app/contents/src/y.rs?ref={r}"))
            .header("Accept", "application/vnd.github.raw+json")
            .send(port);
        assert_eq!((raw.status, raw.body), (200, b"y2".to_vec()), "{r}");
    }
}

/// A commit touching `path`.
fn commit(sha: char, date: &str, login: Option<&str>, path: &str) -> CommitSpec {
    CommitSpec {
        sha: sha.to_string().repeat(40),
        message: format!("change {sha}\n\nbody"),
        author_login: login.map(str::to_string),
        author_name: format!("Name {sha}"),
        date: date.into(),
        paths: vec![path.into()],
    }
}

#[test]
fn commits_list_newest_first_from_the_ref_with_filters_and_numeric_links() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    fake.add_commit(
        "fixture-dev/app",
        commit(
            'a',
            "2026-10-01T00:00:00Z",
            Some("fixture-dev-bot"),
            "src/a.rs",
        ),
    );
    fake.add_commit(
        "fixture-dev/app",
        commit('b', "2026-10-02T00:00:00Z", None, "docs/b.md"),
    );
    fake.add_commit(
        "fixture-dev/app",
        commit(
            'c',
            "2026-10-03T00:00:00Z",
            Some("fixture-reader"),
            "src/c.rs",
        ),
    );
    fake.set_branch("fixture-dev/app", "old", &"b".repeat(40));
    let shas = |q: &str| -> Vec<String> {
        let r = api(&format!("/repos/fixture-dev/app/commits{q}")).send(port);
        values(&r, "sha")
            .iter()
            .map(|v| v.as_str().expect("a sha")[..1].to_string())
            .collect()
    };
    assert_eq!(shas(""), ["c", "b", "a"]);
    assert_eq!(shas("?sha=old"), ["b", "a"]);
    assert_eq!(shas(&format!("?sha={}", "b".repeat(40))), ["b", "a"]);
    assert_eq!(shas("?path=src"), ["c", "a"]);
    assert_eq!(shas("?since=2026-10-02T00:00:00Z"), ["c", "b"]);
    assert_eq!(shas("?author=fixture-reader"), ["c"]);
    assert_eq!(shas("?author=Name%20b"), ["b"]);
    let one = api("/repos/fixture-dev/app/commits?sha=main&per_page=1").send(port);
    let v = one.json();
    assert_eq!(v[0]["commit"]["message"], json!("change c\n\nbody"));
    assert_eq!(
        v[0]["commit"]["committer"]["date"],
        json!("2026-10-03T00:00:00Z")
    );
    assert_eq!(v[0]["author"]["login"], json!("fixture-reader"));
    assert_eq!(
        one.link("next"),
        Some(format!(
            "http://127.0.0.1:{port}/repositories/1000/commits?sha=main&per_page=1&page=2"
        ))
    );
    let b = api(&format!(
        "/repos/fixture-dev/app/commits?sha={}",
        "b".repeat(40)
    ))
    .send(port);
    assert_eq!(b.json()[0]["author"], Value::Null);
    assert_eq!(
        api("/repos/fixture-dev/app/commits?sha=nope")
            .send(port)
            .status,
        404
    );
}

#[test]
fn compare_answers_its_spec_or_no_common_ancestor() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    fake.add_commit(
        "fixture-dev/app",
        commit('a', "2026-10-01T00:00:00Z", None, "x"),
    );
    fake.add_commit(
        "fixture-dev/app",
        commit('b', "2026-10-02T00:00:00Z", None, "y"),
    );
    fake.set_tag("fixture-dev/app", "v1", &"a".repeat(40));
    fake.set_tag("fixture-dev/app", "v2", &"b".repeat(40));
    fake.set_compare(
        "fixture-dev/app",
        "v1",
        "v2",
        CompareSpec::Ok {
            status: "ahead".into(),
            ahead_by: 1,
            behind_by: 0,
            total_commits: 1,
            merge_base: "a".repeat(40),
        },
    );
    fake.set_compare("fixture-dev/app", "x", "y", CompareSpec::NoCommonAncestor);
    let v = api("/repos/fixture-dev/app/compare/v1...v2")
        .send(port)
        .json();
    assert_eq!(v["status"], json!("ahead"));
    assert_eq!(v["ahead_by"], json!(1));
    assert_eq!(v["behind_by"], json!(0));
    assert_eq!(v["total_commits"], json!(1));
    assert_eq!(v["merge_base_commit"]["sha"], json!("a".repeat(40)));
    assert_eq!(v["commits"][0]["sha"], json!("b".repeat(40)));
    assert_eq!(v["files"][0]["filename"], json!("y"));
    let none = api("/repos/fixture-dev/app/compare/x...y").send(port);
    assert_eq!(none.status, 404);
    assert_eq!(
        none.json()["message"],
        json!("No common ancestor between x and y.")
    );
    assert_eq!(
        api("/repos/fixture-dev/app/compare/p...q")
            .send(port)
            .status,
        404
    );
}

#[test]
fn compare_pages_with_repo_links_and_unpaged_lists_the_last_250_to_head() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    let sha = |i: usize| format!("{i:040x}");
    for i in 1..=300 {
        fake.add_commit(
            "fixture-dev/app",
            CommitSpec {
                sha: sha(i),
                message: format!("change {i}"),
                author_login: None,
                author_name: "Name".into(),
                date: "2026-10-01T00:00:00Z".into(),
                paths: vec![format!("f{i}")],
            },
        );
    }
    fake.set_tag("fixture-dev/app", "v1", &sha(1));
    fake.set_tag("fixture-dev/app", "v2", &sha(4));
    fake.set_tag("fixture-dev/app", "v9", &sha(300));
    for head in ["v2", "v9"] {
        fake.set_compare(
            "fixture-dev/app",
            "v1",
            head,
            CompareSpec::Ok {
                status: "ahead".into(),
                ahead_by: 0,
                behind_by: 0,
                total_commits: 0,
                merge_base: sha(1),
            },
        );
    }
    let commit_shas = |r: &crate::raw::Reply| -> Vec<Value> {
        r.json()["commits"]
            .as_array()
            .expect("commits")
            .iter()
            .map(|c| c["sha"].clone())
            .collect()
    };
    let first = api("/repos/fixture-dev/app/compare/v1...v2?per_page=1").send(port);
    assert_eq!(commit_shas(&first), [json!(sha(2))]);
    let next =
        format!("http://127.0.0.1:{port}/repos/fixture-dev/app/compare/v1...v2?per_page=1&page=2");
    assert_eq!(first.link("next"), Some(next.clone()));
    assert_eq!(
        first.link("last"),
        Some(format!(
            "http://127.0.0.1:{port}/repos/fixture-dev/app/compare/v1...v2?per_page=1&page=3"
        ))
    );
    assert_eq!(
        first.json()["files"],
        json!([{"filename": "f2", "status": "modified"}, {"filename": "f3", "status": "modified"},
            {"filename": "f4", "status": "modified"}])
    );
    let second = api(next.trim_start_matches(&format!("http://127.0.0.1:{port}"))).send(port);
    assert_eq!(commit_shas(&second), [json!(sha(3))]);
    assert_eq!(second.json()["files"], json!([]));
    let unpaged = api("/repos/fixture-dev/app/compare/v1...v9").send(port);
    let listed = commit_shas(&unpaged);
    assert_eq!(listed.len(), 250);
    assert_eq!(listed[0], json!(sha(51)));
    assert_eq!(listed[249], json!(sha(300)));
    assert_eq!(unpaged.header("link"), None);
    assert_eq!(
        unpaged.json()["files"].as_array().expect("files").len(),
        299
    );
}

#[test]
fn branches_and_tags_paginate_with_repo_links() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    for (name, c) in [("main", 'a'), ("dev", 'b'), ("zeta", 'c')] {
        fake.set_branch("fixture-dev/app", name, &c.to_string().repeat(40));
        fake.set_tag(
            "fixture-dev/app",
            &format!("t-{name}"),
            &c.to_string().repeat(40),
        );
    }
    let branches = api("/repos/fixture-dev/app/branches?per_page=2").send(port);
    assert_eq!(values(&branches, "name"), [json!("dev"), json!("main")]);
    assert_eq!(branches.json()[0]["commit"]["sha"], json!("b".repeat(40)));
    assert_eq!(branches.json()[0]["protected"], json!(false));
    assert_eq!(
        branches.link("next"),
        Some(format!(
            "http://127.0.0.1:{port}/repos/fixture-dev/app/branches?per_page=2&page=2"
        ))
    );
    let first_tags = api("/repos/fixture-dev/app/tags?per_page=2").send(port);
    assert_eq!(values(&first_tags, "name").len(), 2);
    let tags_next = format!("http://127.0.0.1:{port}/repos/fixture-dev/app/tags?per_page=2&page=2");
    assert_eq!(first_tags.link("next"), Some(tags_next.clone()));
    let tags = api(tags_next.trim_start_matches(&format!("http://127.0.0.1:{port}"))).send(port);
    assert_eq!(values(&tags, "name").len(), 1);
    assert_eq!(
        tags.link("prev"),
        Some(format!(
            "http://127.0.0.1:{port}/repos/fixture-dev/app/tags?per_page=2&page=1"
        ))
    );
    assert_eq!(tags.link("next"), None);
}

#[test]
fn user_needs_a_token_and_users_name_their_type() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    let anonymous = api("/user").send(port);
    assert_eq!(anonymous.status, 401);
    assert_eq!(
        anonymous.json()["message"],
        json!("Requires authentication")
    );
    let me = api("/user").token(&dev).send(port).json();
    assert_eq!(
        (me["login"].clone(), me["id"].clone()),
        (json!("fixture-dev-bot"), json!(100))
    );
    assert_eq!(me["type"], json!("User"));
    let org = api("/users/fixture-dev").send(port).json();
    assert_eq!(
        (org["type"].clone(), org["id"].clone()),
        (json!("Organization"), json!(102))
    );
    assert_eq!(api("/users/nobody").send(port).status, 404);
}

#[test]
fn owner_repo_lists_follow_their_owner_kind_and_numeric_links() {
    let (fake, dev, reader) = fake();
    let port = fake.ports().api;
    fake.add_repo("fixture-dev/lib", RepoSpec::public());
    fake.add_repo(
        "fixture-dev/secret",
        RepoSpec::private().pull("fixture-reader"),
    );
    fake.add_repo("fixture-dev-bot/own", RepoSpec::public());
    fake.add_repo(
        "fixture-dev-bot/hidden",
        RepoSpec::private().push("fixture-dev-bot"),
    );
    fake.add_repo("fixture-dev-bot/two", RepoSpec::public());
    fake.add_repo("fixture-dev-bot/nameless", RepoSpec::private());
    let names = |r: &crate::raw::Reply| values(r, "full_name");
    let org = api("/orgs/fixture-dev/repos?type=all&per_page=2")
        .token(&reader)
        .send(port);
    assert_eq!(
        names(&org),
        [json!("fixture-dev/app"), json!("fixture-dev/lib")]
    );
    let next =
        format!("http://127.0.0.1:{port}/organizations/102/repos?type=all&per_page=2&page=2");
    assert_eq!(org.link("next"), Some(next.clone()));
    let page2 = api(next.trim_start_matches(&format!("http://127.0.0.1:{port}")))
        .token(&reader)
        .send(port);
    assert_eq!(names(&page2), [json!("fixture-dev/secret")]);
    assert_eq!(names(&api("/orgs/fixture-dev/repos").send(port)).len(), 2);
    let private = api("/orgs/fixture-dev/repos?type=private")
        .token(&reader)
        .send(port);
    assert_eq!(names(&private), [json!("fixture-dev/secret")]);
    assert_eq!(api("/orgs/fixture-dev-bot/repos").send(port).status, 404);
    let user = api("/users/fixture-dev-bot/repos?type=owner&per_page=1").send(port);
    assert_eq!(names(&user), [json!("fixture-dev-bot/own")]);
    let user_next = format!("http://127.0.0.1:{port}/user/100/repos?type=owner&per_page=1&page=2");
    assert_eq!(user.link("next"), Some(user_next.clone()));
    let user2 = api(user_next.trim_start_matches(&format!("http://127.0.0.1:{port}"))).send(port);
    assert_eq!(names(&user2), [json!("fixture-dev-bot/two")]);
    assert_eq!(user2.link("next"), None);
    let mine = api("/user/repos?affiliation=owner&per_page=1")
        .token(&dev)
        .send(port);
    assert_eq!(names(&mine), [json!("fixture-dev-bot/own")]);
    assert_eq!(
        mine.link("next"),
        Some(format!(
            "http://127.0.0.1:{port}/user/repos?affiliation=owner&per_page=1&page=2"
        ))
    );
    let mine_private = api("/user/repos?affiliation=owner&visibility=private")
        .token(&dev)
        .send(port);
    assert_eq!(names(&mine_private), [json!("fixture-dev-bot/hidden")]);
    assert_eq!(api("/user/repos").send(port).status, 401);
    let numeric = api("/user/100/repos").send(port);
    assert_eq!(
        names(&numeric),
        [json!("fixture-dev-bot/own"), json!("fixture-dev-bot/two")]
    );
    fake.apply(Change::Hide {
        slug: "fixture-dev-bot/own".into(),
    });
    let hidden = api("/user/repos?affiliation=owner").token(&dev).send(port);
    assert_eq!(
        names(&hidden),
        [
            json!("fixture-dev-bot/hidden"),
            json!("fixture-dev-bot/two")
        ]
    );
}

#[test]
fn a_renamed_repo_answers_301_to_its_numeric_path() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"),
    );
    fake.rename_repo("fixture-dev/app", "fixture-dev/app2");
    let moved = api("/repos/fixture-dev/app/releases?per_page=5")
        .token(&dev)
        .send(port);
    assert_eq!(moved.status, 301);
    assert_eq!(
        moved.header("location"),
        Some(format!("http://127.0.0.1:{port}/repositories/1000/releases?per_page=5").as_str())
    );
    let bare = api("/repos/fixture-dev/app").send(port);
    assert_eq!(
        bare.header("location"),
        Some(format!("http://127.0.0.1:{port}/repositories/1000").as_str())
    );
    let query = api("/repos/fixture-dev/app?x=1").send(port);
    assert_eq!(
        query.header("location"),
        Some(format!("http://127.0.0.1:{port}/repositories/1000?x=1").as_str())
    );
    let now = api("/repos/fixture-dev/app2").send(port).json();
    assert_eq!(now["full_name"], json!("fixture-dev/app2"));
    assert_eq!(
        values(
            &api("/repositories/1000/releases?per_page=5").send(port),
            "tag_name"
        ),
        [json!("v1")]
    );
}

#[test]
fn hide_makes_a_repo_404_to_everyone() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    assert_eq!(
        api("/repos/fixture-dev/app").token(&dev).send(port).status,
        200
    );
    fake.apply(Change::Hide {
        slug: "fixture-dev/app".into(),
    });
    assert_eq!(
        api("/repos/fixture-dev/app").token(&dev).send(port).status,
        404
    );
    assert_eq!(api("/repos/fixture-dev/app").send(port).status, 404);
}

#[test]
fn a_request_past_the_limit_is_refused_as_github_refuses_it() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    for _ in 0..60 {
        assert_eq!(api("/repos/fixture-dev/app").send(port).status, 200);
    }
    let past = api("/repos/fixture-dev/app").send(port);
    assert_eq!(past.status, 403);
    assert_eq!(past.header("x-ratelimit-remaining"), Some("0"));
    assert_eq!(past.header("x-ratelimit-used"), Some("60"));
    assert_eq!(past.header("x-ratelimit-reset"), Some("1760003600"));
    let limits = api("/rate_limit").send(port);
    assert_eq!(limits.status, 200);
    assert_eq!(limits.json()["resources"]["core"]["remaining"], json!(0));
    assert_eq!(
        api("/repos/fixture-dev/app").token(&dev).send(port).status,
        200
    );
    assert!(fake.violations().is_empty());
}

#[test]
fn a_target_with_empty_segments_is_read_as_the_router_reads_it() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    for _ in 0..2 {
        let limits = api("/rate_limit/").send(port);
        assert_eq!(limits.status, 200);
        assert_eq!(limits.header("x-ratelimit-used"), Some("0"));
    }
    fake.rename_repo("fixture-dev/app", "fixture-dev/app2");
    let releases = format!("http://127.0.0.1:{port}/repositories/1000/releases");
    for target in [
        "//repos/fixture-dev/app/releases",
        "/repos/fixture-dev//app/releases",
        "/repos/fixture-dev/app//releases",
    ] {
        let moved = api(target).send(port);
        assert_eq!(moved.status, 301, "{target}");
        assert_eq!(
            moved.header("location"),
            Some(releases.as_str()),
            "{target}"
        );
    }
    let query = api("/repos/fixture-dev/app/releases/?page=2").send(port);
    assert_eq!(
        query.header("location"),
        Some(format!("{releases}?page=2").as_str())
    );
}
