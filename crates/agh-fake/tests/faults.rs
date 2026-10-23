// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Each fault, change and violation.

use std::time::Duration;

use agh_fake::{
    AssetSpec, Change, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec,
    fine_grained_token, token,
};
use aterm_json::json;

use crate::raw::{api, plain, send_bytes, send_cut};

fn fake() -> (FakeGithub, String, u64) {
    let fake = FakeGithub::start();
    let dev = token("dev");
    fake.add_token(&dev, "fixture-dev-bot");
    fake.add_repo(
        "fixture-dev/app",
        RepoSpec::public().push("fixture-dev-bot"),
    );
    let release = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"),
    );
    (fake, dev, release)
}

fn status_500() -> Fault {
    Fault::Status {
        status: 500,
        body: String::new(),
        headers: vec![],
    }
}

#[test]
fn a_fault_fires_once_and_fault_always_on_every_match() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    fake.fault(Match::api("/repos/fixture-dev/app"), status_500());
    let first = api("/repos/fixture-dev/app").send(port);
    assert_eq!(first.status, 500);
    assert!(first.body.is_empty());
    assert_eq!(first.header("x-github-request-id"), Some("FAKE:1"));
    assert_eq!(first.header("x-ratelimit-remaining"), Some("59"));
    assert_eq!(api("/repos/fixture-dev/app").send(port).status, 200);
    fake.fault_always(
        Match::api("/repos/fixture-dev/app/releases"),
        Fault::Status {
            status: 503,
            body: "{}".into(),
            headers: vec![("x-extra".into(), "1".into())],
        },
    );
    for _ in 0..3 {
        let r = api("/repos/fixture-dev/app/releases").send(port);
        assert_eq!((r.status, r.header("x-extra")), (503, Some("1")));
    }
    assert_eq!(api("/repos/fixture-dev/app").send(port).status, 200);
    assert!(fake.violations().is_empty());
}

#[test]
fn a_once_fault_fires_before_an_overlapping_always_fault() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    let status = |s: u16| Fault::Status {
        status: s,
        body: String::new(),
        headers: vec![],
    };
    fake.fault_always(Match::api("/"), status(503));
    fake.fault(Match::api("/repos/"), status(500));
    let statuses: Vec<u16> = (0..3)
        .map(|_| api("/repos/fixture-dev/app").send(port).status)
        .collect();
    assert_eq!(statuses, [500, 503, 503]);
}

#[test]
fn a_status_fault_s_own_headers_replace_the_fake_s() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    fake.fault(
        Match::api("/repos/"),
        Fault::Status {
            status: 429,
            body: "<html>busy</html>".into(),
            headers: vec![
                ("x-ratelimit-remaining".into(), "0".into()),
                ("Content-Type".into(), "text/html".into()),
                ("X-GitHub-Request-Id".into(), "MINE".into()),
                ("Content-Length".into(), "999".into()),
                ("Connection".into(), "keep-alive".into()),
            ],
        },
    );
    let r = api("/repos/fixture-dev/app").send(port);
    assert_eq!(r.status, 429);
    assert_eq!(r.all("x-ratelimit-remaining"), ["0"]);
    assert_eq!(r.all("content-type"), ["text/html"]);
    assert_eq!(r.all("x-github-request-id"), ["MINE"]);
    assert_eq!(r.all("content-length"), [r.body.len().to_string()]);
    assert_eq!(r.all("connection"), ["close"]);
    assert_eq!(r.all("x-ratelimit-limit"), ["60"]);
    assert_eq!(r.body, b"<html>busy</html>");
    fake.fault(Match::api("/repos/"), status_500());
    assert_eq!(
        api("/repos/fixture-dev/app").send(port).all("content-type"),
        ["application/json; charset=utf-8"]
    );
}

#[test]
fn a_match_names_its_origin_method_and_prefix() {
    let (fake, _, _) = fake();
    let ports = fake.ports();
    fake.fault(Match::api("/repos/").method("HEAD"), status_500());
    fake.fault(Match::web("/fixture-dev/"), status_500());
    assert_eq!(api("/repos/fixture-dev/app").send(ports.api).status, 200);
    assert_eq!(api("/user").send(ports.api).status, 401);
    assert_eq!(
        api("/repos/fixture-dev/app")
            .method("HEAD")
            .send(ports.api)
            .status,
        500
    );
    assert_eq!(plain("/other/x").send(ports.web).status, 404);
    assert_eq!(plain("/fixture-dev/app").send(ports.web).status, 500);
    fake.fault(Match::uploads("/"), status_500());
    assert_eq!(api("/x").send(ports.uploads).status, 500);
    assert_eq!(api("/x").send(ports.uploads).status, 404);
}

#[test]
fn close_and_hang_answer_no_byte() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    fake.fault(Match::api("/repos/"), Fault::Close);
    let closed = api("/repos/fixture-dev/app").send(port);
    assert_eq!((closed.status, closed.raw.len()), (0, 0));
    fake.fault(
        Match::api("/repos/"),
        Fault::Hang(Duration::from_millis(50)),
    );
    let hung = api("/repos/fixture-dev/app").send(port);
    assert_eq!((hung.status, hung.raw.len()), (0, 0));
    assert_eq!(api("/repos/fixture-dev/app").send(port).status, 200);
}

#[test]
fn a_closed_or_hung_request_uses_up_rate() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    fake.fault(Match::api("/repos/"), Fault::Close);
    assert_eq!(api("/repos/fixture-dev/app").send(port).status, 0);
    let after_close = api("/repos/fixture-dev/app").send(port);
    assert_eq!(after_close.header("x-ratelimit-used"), Some("2"));
    assert_eq!(after_close.header("x-ratelimit-remaining"), Some("58"));
    fake.fault(
        Match::api("/repos/"),
        Fault::Hang(Duration::from_millis(50)),
    );
    assert_eq!(api("/repos/fixture-dev/app").send(port).status, 0);
    let after_hang = api("/repos/fixture-dev/app").send(port);
    assert_eq!(after_hang.header("x-ratelimit-used"), Some("4"));
}

#[test]
fn truncate_body_sends_the_true_length_and_fewer_bytes() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    let whole = api("/repos/fixture-dev/app").send(port);
    fake.fault(Match::api("/repos/"), Fault::TruncateBody { at: 10 });
    let cut = api("/repos/fixture-dev/app").send(port);
    assert_eq!(cut.status, 200);
    assert_eq!(
        cut.header("content-length"),
        Some(whole.body.len().to_string().as_str())
    );
    assert_eq!(cut.body, whole.body[..10]);
}

#[test]
fn rate_limit_faults_carry_their_headers() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    fake.fault(Match::api("/"), Fault::SecondaryLimit { retry_after: 7 });
    let secondary = api("/repos/fixture-dev/app").token(&dev).send(port);
    assert_eq!(secondary.status, 403);
    assert_eq!(secondary.header("retry-after"), Some("7"));
    let message = secondary.json()["message"]
        .as_str()
        .expect("a message")
        .to_string();
    assert!(message.contains("secondary rate limit"));
    fake.fault(
        Match::api("/"),
        Fault::PrimaryExhausted {
            reset: 1_760_009_999,
        },
    );
    let primary = api("/repos/fixture-dev/app").token(&dev).send(port);
    assert_eq!(primary.status, 403);
    assert_eq!(primary.header("x-ratelimit-remaining"), Some("0"));
    assert_eq!(primary.header("x-ratelimit-reset"), Some("1760009999"));
    assert_eq!(primary.header("x-ratelimit-limit"), Some("5000"));
    assert_eq!(primary.header("x-ratelimit-used"), Some("5000"));
    fake.fault(
        Match::uploads("/"),
        Fault::PrimaryExhausted {
            reset: 1_760_008_888,
        },
    );
    let uploads = api("/x").token(&dev).send(fake.ports().uploads);
    assert_eq!(uploads.status, 403);
    assert_eq!(uploads.header("x-ratelimit-remaining"), Some("0"));
    assert_eq!(uploads.header("x-ratelimit-reset"), Some("1760008888"));
}

#[test]
fn redirect_to_and_second_hop_answer_their_location() {
    let (fake, dev, release) = fake();
    let ports = fake.ports();
    fake.fault(
        Match::api("/repos/"),
        Fault::RedirectTo {
            status: 307,
            location: "http://127.0.0.1:1/elsewhere".into(),
        },
    );
    let redirect = api("/repos/fixture-dev/app").send(ports.api);
    assert_eq!(redirect.status, 307);
    assert_eq!(
        redirect.header("location"),
        Some("http://127.0.0.1:1/elsewhere")
    );
    let id = fake.add_asset(release, AssetSpec::uploaded("a.bin", b"a"));
    let hop = api(&format!("/repos/fixture-dev/app/releases/assets/{id}"))
        .token(&dev)
        .header("Accept", "application/octet-stream")
        .send(ports.api);
    let location = hop.header("location").expect("a location").to_string();
    let target = location
        .trim_start_matches(&format!("http://127.0.0.1:{}", ports.cdn))
        .to_string();
    fake.fault(Match::cdn(), Fault::SecondHop);
    let again = plain(&target).send(ports.cdn);
    assert_eq!(again.status, 302);
    assert_eq!(again.header("location"), Some(location.as_str()));
    assert_eq!(again.header("x-github-request-id"), None);
    assert_eq!(plain(&target).send(ports.cdn).body, b"a");
    fake.fault(Match::api("/repos/"), Fault::SecondHop);
    let api_hop = api("/repos/fixture-dev/app").send(ports.api);
    assert_eq!(api_hop.status, 302);
    assert_eq!(
        api_hop.header("location"),
        Some(format!("http://127.0.0.1:{}/repos/fixture-dev/app", ports.cdn).as_str())
    );
    let n = fake.seen().len();
    assert_eq!(
        api_hop.header("x-github-request-id"),
        Some(format!("FAKE:{n}").as_str())
    );
    assert_eq!(api_hop.header("x-ratelimit-resource"), Some("core"));
    let used: u64 = api_hop
        .header("x-ratelimit-used")
        .expect("a rate header")
        .parse()
        .expect("a number");
    let next = api("/repos/fixture-dev/app").send(ports.api);
    assert_eq!(
        next.header("x-ratelimit-used"),
        Some((used + 1).to_string().as_str())
    );
}

#[test]
fn torn_answers_then_deletes_the_newest_release() {
    let (fake, _, first) = fake();
    let port = fake.ports().api;
    let newest = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::draft("v2", "2026-10-05T00:00:00Z"),
    );
    let ids = |r: &crate::raw::Reply| -> Vec<aterm_json::Value> {
        r.json()
            .as_array()
            .expect("an array")
            .iter()
            .map(|v| v["id"].clone())
            .collect()
    };
    fake.fault(Match::api("/repos/fixture-dev/app/releases"), Fault::Torn);
    let dev = token("dev");
    let torn = api("/repos/fixture-dev/app/releases")
        .token(&dev)
        .send(port);
    assert_eq!(ids(&torn), [json!(newest), json!(first)]);
    let after = api("/repos/fixture-dev/app/releases")
        .token(&dev)
        .send(port);
    assert_eq!(ids(&after), [json!(first)]);
}

#[test]
fn after_applies_a_change_once_its_count_is_answered() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    let draft = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::draft("v2", "2026-10-02T00:00:00Z"),
    );
    fake.after(
        Match::api("/repos/fixture-dev/app/releases/tags/"),
        2,
        Change::Publish { release_id: draft },
    );
    let tag = || {
        api("/repos/fixture-dev/app/releases/tags/v2")
            .token(&dev)
            .send(port)
    };
    assert_eq!(api("/repos/fixture-dev/app").send(port).status, 200);
    assert_eq!(tag().status, 404);
    assert_eq!(tag().status, 404);
    let published = tag();
    assert_eq!(published.status, 200);
    assert_eq!(
        published.json()["published_at"],
        json!("2026-10-02T00:00:00Z")
    );
    assert_eq!(published.json()["draft"], json!(false));
}

#[test]
fn a_request_closed_or_hung_unanswered_does_not_count_for_after() {
    let (fake, dev, _) = fake();
    let port = fake.ports().api;
    let draft = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::draft("v2", "2026-10-02T00:00:00Z"),
    );
    let m = Match::api("/repos/fixture-dev/app/releases/tags/");
    fake.fault(m.clone(), Fault::Close);
    fake.fault(m.clone(), Fault::Hang(Duration::from_millis(1)));
    fake.after(m, 1, Change::Publish { release_id: draft });
    let tag = || {
        api("/repos/fixture-dev/app/releases/tags/v2")
            .token(&dev)
            .send(port)
    };
    let draft_of = || {
        api(&format!("/repos/fixture-dev/app/releases/{draft}"))
            .token(&dev)
            .send(port)
            .json()["draft"]
            .clone()
    };
    assert_eq!(tag().status, 0);
    assert_eq!(tag().status, 0);
    assert_eq!(draft_of(), json!(true));
    assert_eq!(tag().status, 404);
    assert_eq!(draft_of(), json!(false));
}

#[test]
fn an_after_change_follows_its_repo_through_a_rename() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    fake.add_repo("fixture-dev/old", RepoSpec::public());
    fake.after(
        Match::api("/repos/fixture-dev/app"),
        1,
        Change::Hide {
            slug: "fixture-dev/old".into(),
        },
    );
    fake.rename_repo("fixture-dev/old", "fixture-dev/new");
    assert_eq!(api("/repos/fixture-dev/app").send(port).status, 200);
    assert_eq!(api("/repos/fixture-dev/new").send(port).status, 404);
    assert_eq!(api("/repos/fixture-dev/app").send(port).status, 200);
    assert!(fake.violations().is_empty(), "{:?}", fake.violations());
}

#[test]
fn an_after_change_gone_stale_is_a_violation_and_the_answer_still_comes() {
    let (fake, dev, release) = fake();
    let port = fake.ports().api;
    let draft = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::draft("v2", "2026-10-02T00:00:00Z"),
    );
    let m = Match::api("/repos/fixture-dev/app");
    fake.after(
        m.clone(),
        1,
        Change::DeleteRelease {
            release_id: release,
        },
    );
    fake.after(m, 1, Change::Publish { release_id: draft });
    fake.apply(Change::DeleteRelease {
        release_id: release,
    });
    assert_eq!(api("/repos/fixture-dev/app").send(port).status, 200);
    let v = fake.violations();
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains(&release.to_string()), "{}", v[0]);
    let published = api(&format!("/repos/fixture-dev/app/releases/{draft}"))
        .token(&dev)
        .send(port);
    assert_eq!(published.json()["draft"], json!(false));
}

#[test]
#[should_panic(expected = "424242")]
fn after_naming_an_unknown_release_panics_at_the_call() {
    let (fake, _, _) = fake();
    fake.after(
        Match::api("/repos/"),
        1,
        Change::Publish {
            release_id: 424_242,
        },
    );
}

#[test]
#[should_panic(expected = "424242")]
fn deleting_an_unknown_release_panics() {
    let (fake, _, _) = fake();
    fake.apply(Change::DeleteRelease {
        release_id: 424_242,
    });
}

#[test]
fn an_asset_replaced_in_place_keeps_its_id() {
    let (fake, _, release) = fake();
    let ports = fake.ports();
    let id = fake.add_asset(release, AssetSpec::uploaded("a.bin", b"first"));
    let path = format!("/repos/fixture-dev/app/releases/assets/{id}");
    let cdn = api(&path)
        .header("Accept", "application/octet-stream")
        .send(ports.api)
        .header("location")
        .expect("a location")
        .trim_start_matches(&format!("http://127.0.0.1:{}", ports.cdn))
        .to_string();
    fake.after(
        Match::cdn(),
        1,
        Change::AddAsset {
            release_id: release,
            spec: AssetSpec {
                updated_at: "2026-10-09T00:00:00Z".into(),
                ..AssetSpec::uploaded("a.bin", b"second!")
            },
        },
    );
    assert_eq!(plain(&cdn).send(ports.cdn).body, b"first");
    let v = api(&path).send(ports.api).json();
    assert_eq!(v["id"], json!(id));
    assert_eq!(v["size"], json!(7));
    assert_eq!(v["updated_at"], json!("2026-10-09T00:00:00Z"));
    assert_eq!(v["download_count"], json!(1));
    assert_eq!(plain(&cdn).send(ports.cdn).body, b"second!");
    let open = fake.add_asset(
        release,
        AssetSpec {
            state: "open".into(),
            ..AssetSpec::uploaded("b.bin", b"b")
        },
    );
    assert_eq!(open, id + 1);
}

#[test]
fn delete_release_removes_it() {
    let (fake, dev, release) = fake();
    let port = fake.ports().api;
    let path = format!("/repos/fixture-dev/app/releases/{release}");
    assert_eq!(api(&path).token(&dev).send(port).status, 200);
    fake.apply(Change::DeleteRelease {
        release_id: release,
    });
    assert_eq!(api(&path).token(&dev).send(port).status, 404);
    assert_eq!(
        api("/repos/fixture-dev/app/releases/latest")
            .send(port)
            .status,
        404
    );
}

#[test]
fn each_wire_breach_is_one_violation() {
    let (fake, dev, release) = fake();
    let ports = fake.ports();
    let id = fake.add_asset(release, AssetSpec::uploaded("a.bin", b"a"));
    let cdn = api(&format!("/repos/fixture-dev/app/releases/assets/{id}"))
        .header("Accept", "application/octet-stream")
        .send(ports.api)
        .header("location")
        .expect("a location")
        .trim_start_matches(&format!("http://127.0.0.1:{}", ports.cdn))
        .to_string();
    assert!(fake.violations().is_empty());
    let web_path = "/fixture-dev/app/releases/download/v1/a.bin";
    assert_eq!(plain(web_path).token(&dev).send(ports.web).status, 302);
    assert_eq!(plain(&cdn).token(&dev).send(ports.cdn).status, 400);
    api("/repos/fixture-dev/app")
        .without("User-Agent")
        .send(ports.api);
    api("/repos/fixture-dev/app")
        .without("X-GitHub-Api-Version")
        .send(ports.api);
    api("/x")
        .without("X-GitHub-Api-Version")
        .send(ports.uploads);
    api(web_path).without("Accept").send(ports.web);
    api(&cdn).without("Accept").send(ports.cdn);
    api("/repos/fixture-dev/app").body(b"").send(ports.api);
    api("/repos/fixture-dev/app")
        .method("HEAD")
        .body(b"")
        .send(ports.api);
    let over = "/repos/fixture-dev/app/releases?per_page=101";
    api(over).send(ports.api);
    let v = fake.violations();
    assert_eq!(v.len(), 10, "{v:?}");
    let has = |line: &str, data: &[&str]| data.iter().all(|d| line.contains(d));
    let app = "/repos/fixture-dev/app";
    assert!(has(&v[0], &["web", "GET", web_path]), "{}", v[0]);
    assert!(has(&v[1], &["cdn", "GET", &cdn]), "{}", v[1]);
    assert!(has(&v[2], &["User-Agent", "GET", app]), "{}", v[2]);
    assert!(
        has(&v[3], &["X-GitHub-Api-Version", "api", "GET", app]),
        "{}",
        v[3]
    );
    assert!(
        has(&v[4], &["X-GitHub-Api-Version", "uploads", "GET", "/x"]),
        "{}",
        v[4]
    );
    assert!(
        has(&v[5], &["X-GitHub-Api-Version", "web", "GET", web_path]),
        "{}",
        v[5]
    );
    assert!(
        has(&v[6], &["X-GitHub-Api-Version", "cdn", "GET", &cdn]),
        "{}",
        v[6]
    );
    assert!(has(&v[7], &["Content-Length", "GET", app]), "{}", v[7]);
    assert!(has(&v[8], &["Content-Length", "HEAD", app]), "{}", v[8]);
    assert!(has(&v[9], &["101", "100", over]), "{}", v[9]);
}

#[test]
fn a_chunked_request_answers_400_and_its_body_is_drained() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    let chunk = vec![b'x'; 4096];
    let mut body = Vec::new();
    for _ in 0..64 {
        body.extend_from_slice(b"1000\r\n");
        body.extend_from_slice(&chunk);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"0\r\n\r\n");
    let chunked = api("/repos/fixture-dev/app")
        .method("POST")
        .header("Transfer-Encoding", "chunked")
        .bare_body(&body)
        .send(port);
    assert_eq!(chunked.status, 400);
    assert_eq!(fake.seen().len(), 1);
}

#[test]
fn a_malformed_head_answers_400_and_is_logged() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    let app = "/repos/fixture-dev/app";
    let mut lossy = format!("GET {app} HTTP/1.1\r\nUser-Agent: probe ").into_bytes();
    lossy.extend_from_slice(b"\xff\r\nX-GitHub-Api-Version: 2022-11-28\r\n\r\n");
    assert_eq!(send_bytes(port, &lossy).status, 200);
    assert!(fake.violations().is_empty());
    let colonless = format!(
        "GET {app} HTTP/1.1\r\nUser-Agent: probe\r\nX-GitHub-Api-Version: 2022-11-28\r\nno-colon-here\r\n\r\n"
    );
    assert_eq!(send_bytes(port, colonless.as_bytes()).status, 400);
    let long = format!(
        "GET {app} HTTP/1.1\r\nUser-Agent: probe\r\nX-GitHub-Api-Version: 2022-11-28\r\nX-Big: {}\r\n\r\n",
        "y".repeat(70 * 1024)
    );
    assert_eq!(send_bytes(port, long.as_bytes()).status, 400);
    let seen = fake.seen();
    assert_eq!(seen.len(), 3);
    assert!(seen.iter().all(|s| s.path_and_query == app));
    let v = fake.violations();
    assert_eq!(v.len(), 2, "{v:?}");
    assert!(
        v.iter()
            .all(|line| line.contains("GET") && line.contains(app))
    );
    let endless = format!("GET {app}/{}", "z".repeat(70 * 1024));
    assert_eq!(send_bytes(port, endless.as_bytes()).status, 400);
    let seen = fake.seen();
    assert_eq!(seen.len(), 4);
    assert!(seen[3].path_and_query.starts_with(app));
    assert_eq!(fake.violations().len(), 3);
}

#[test]
fn every_credential_header_at_the_web_and_cdn_is_a_violation() {
    let (fake, dev, release) = fake();
    let ports = fake.ports();
    let id = fake.add_asset(release, AssetSpec::uploaded("a.bin", b"a"));
    let cdn = api(&format!("/repos/fixture-dev/app/releases/assets/{id}"))
        .header("Accept", "application/octet-stream")
        .send(ports.api)
        .header("location")
        .expect("a location")
        .trim_start_matches(&format!("http://127.0.0.1:{}", ports.cdn))
        .to_string();
    let web_path = "/fixture-dev/app/releases/download/v1/a.bin";
    fake.clear_seen();
    let carriers = [
        ("Cookie", format!("user_session={dev}")),
        ("Proxy-Authorization", format!("Basic {dev}")),
        ("X-Anything", dev.clone()),
    ];
    for (name, value) in &carriers {
        let web = plain(web_path).header(name, value).send(ports.web);
        assert_eq!(web.status, 302, "{name}");
        let at_cdn = plain(&cdn).header(name, value).send(ports.cdn);
        assert_eq!(at_cdn.status, 400, "{name}");
    }
    let v = fake.violations();
    assert_eq!(v.len(), 6, "{v:?}");
    for (i, line) in v.iter().enumerate() {
        let (origin, path) = if i % 2 == 0 {
            ("web", web_path)
        } else {
            ("cdn", cdn.as_str())
        };
        assert!(line.contains(origin) && line.contains(path), "{line}");
        assert!(!line.contains(&dev), "{line}");
    }
    for seen in fake.seen() {
        assert!(seen.had_authorization);
        assert!(seen.headers.iter().all(|(_, v)| !v.contains(&dev)));
        assert!(
            seen.headers
                .iter()
                .all(|(n, _)| !n.eq_ignore_ascii_case("cookie")
                    && !n.eq_ignore_ascii_case("proxy-authorization"))
        );
    }
    api("/repos/fixture-dev/app")
        .header("Cookie", "a=b")
        .send(ports.api);
    assert_eq!(fake.violations().len(), 6);
}

#[test]
fn seen_logs_every_request_with_the_credential_reduced() {
    let (fake, dev, _) = fake();
    let ports = fake.ports();
    api("/repos/fixture-dev/app").token(&dev).send(ports.api);
    api("/repos/fixture-dev/app?x=1")
        .token(&fine_grained_token("unknown"))
        .send(ports.api);
    plain("/fixture-dev/app/releases/download/v1/none").send(ports.web);
    let seen = fake.seen();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].origin, FakeOrigin::Api);
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].path_and_query, "/repos/fixture-dev/app");
    assert!(seen[0].had_authorization);
    assert_eq!(seen[0].as_login.as_deref(), Some("fixture-dev-bot"));
    assert!(
        seen[0]
            .headers
            .iter()
            .all(|(n, v)| { !n.eq_ignore_ascii_case("authorization") && !v.contains(&dev) })
    );
    assert!(seen[0].headers.iter().any(|(n, _)| n == "User-Agent"));
    assert_eq!(seen[1].path_and_query, "/repos/fixture-dev/app?x=1");
    assert!(seen[1].had_authorization);
    assert_eq!(seen[1].as_login, None);
    assert_eq!(seen[2].origin, FakeOrigin::Web);
    assert!(!seen[2].had_authorization);
    fake.clear_seen();
    assert!(fake.seen().is_empty());
    api("/rate_limit").send(ports.api);
    assert_eq!(fake.seen().len(), 1);
}

#[test]
fn tokens_are_seeded_hex_of_their_shape() {
    let classic = token("dev");
    assert_eq!(classic.len(), 40);
    assert!(classic.starts_with("ghp_"));
    assert!(aterm_codec::hex::is_lower(&classic[4..], 36));
    assert_eq!(classic, token("dev"));
    assert_ne!(classic, token("release"));
    let fine = fine_grained_token("dev");
    assert!(fine.starts_with("github_pat_"));
    assert!(aterm_codec::hex::is_lower(&fine[11..], 64));
}

fn fake_with_asset() -> (FakeGithub, String, String) {
    let (fake, dev, release) = fake();
    let ports = fake.ports();
    let id = fake.add_asset(release, AssetSpec::uploaded("a.bin", b"a"));
    let cdn = api(&format!("/repos/fixture-dev/app/releases/assets/{id}"))
        .header("Accept", "application/octet-stream")
        .send(ports.api)
        .header("location")
        .expect("a location")
        .trim_start_matches(&format!("http://127.0.0.1:{}", ports.cdn))
        .to_string();
    fake.clear_seen();
    (fake, dev, cdn)
}

#[test]
fn a_token_in_the_target_is_a_violation_at_every_origin() {
    let (fake, dev, cdn) = fake_with_asset();
    let ports = fake.ports();
    let web_path = format!("/fixture-dev/app/releases/download/v1/a.bin?token={dev}");
    assert_eq!(plain(&web_path).send(ports.web).status, 302);
    let at_cdn = plain(&format!("{cdn}&access_token={dev}")).send(ports.cdn);
    assert_eq!(at_cdn.status, 400);
    let at_api = api(&format!("/repos/fixture-dev/app?access_token={dev}")).send(ports.api);
    assert_eq!(at_api.status, 200);
    let v = fake.violations();
    assert_eq!(v.len(), 3, "{v:?}");
    for (line, origin) in v.iter().zip(["web", "cdn", "api"]) {
        assert!(
            line.contains(origin) && line.contains("GET") && line.contains("<token>"),
            "{line}"
        );
        assert!(!line.contains(&dev), "{line}");
    }
    let seen = fake.seen();
    assert_eq!(seen.len(), 3);
    for s in &seen {
        assert!(
            !s.path_and_query.contains(&dev) && s.path_and_query.contains("<token>"),
            "{}",
            s.path_and_query
        );
    }
    let carried: Vec<bool> = seen.iter().map(|s| s.had_authorization).collect();
    assert_eq!(carried, [true, true, false]);
}

#[test]
fn two_credential_headers_at_the_api_or_uploads_origin_are_refused() {
    let (fake, dev, _) = fake();
    let release = token("release");
    fake.add_token(&release, "fixture-release-bot");
    let ports = fake.ports();
    let (name, scheme) = ("Authorization", "Bearer");
    let app = "/repos/fixture-dev/app";
    for port in [ports.api, ports.uploads] {
        let head = format!(
            "GET {app} HTTP/1.1\r\nUser-Agent: probe\r\nX-GitHub-Api-Version: 2022-11-28\r\n{name}: {scheme} {dev}\r\n{name}: {scheme} {release}\r\n\r\n"
        );
        assert_eq!(send_bytes(port, head.as_bytes()).status, 400);
    }
    let v = fake.violations();
    assert_eq!(v.len(), 2, "{v:?}");
    for (line, origin) in v.iter().zip(["api", "uploads"]) {
        assert!(
            line.contains(origin) && line.contains("GET") && line.contains(app),
            "{line}"
        );
    }
    assert_eq!(api(app).token(&dev).send(ports.api).status, 200);
    assert_eq!(fake.violations().len(), 2);
}

#[test]
fn an_empty_user_agent_or_api_version_is_a_violation() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    api("/rate_limit").header("User-Agent", "").send(port);
    api("/rate_limit")
        .header("X-GitHub-Api-Version", "")
        .send(port);
    let v = fake.violations();
    assert_eq!(v.len(), 2, "{v:?}");
    assert!(
        v[0].contains("User-Agent") && v[0].contains("/rate_limit"),
        "{}",
        v[0]
    );
    assert!(
        v[1].contains("X-GitHub-Api-Version") && v[1].contains("api"),
        "{}",
        v[1]
    );
}

#[test]
fn a_head_cut_before_its_final_lf_is_malformed() {
    let (fake, _, _) = fake();
    let port = fake.ports().api;
    let app = "/repos/fixture-dev/app";
    let head = format!(
        "GET {app} HTTP/1.1\r\nUser-Agent: probe\r\nX-GitHub-Api-Version: 2022-11-28\r\n\r\n"
    );
    let bytes = head.as_bytes();
    assert_eq!(send_cut(port, &bytes[..bytes.len() - 1]).status, 400);
    let v = fake.violations();
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("GET") && v[0].contains(app), "{}", v[0]);
    assert_eq!(send_cut(port, bytes).status, 200);
    assert_eq!(fake.violations().len(), 1);
}

#[test]
fn torn_reads_its_repository_as_the_router_does() {
    let (fake, dev, first) = fake();
    let port = fake.ports().api;
    let newest = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v2", "2026-10-05T00:00:00Z"),
    );
    let ids = |r: &crate::raw::Reply| -> Vec<aterm_json::Value> {
        r.json()
            .as_array()
            .expect("an array")
            .iter()
            .map(|v| v["id"].clone())
            .collect()
    };
    fake.fault(Match::api("/repos/"), Fault::Torn);
    let torn = api("/repos//fixture-dev/app/releases")
        .token(&dev)
        .send(port);
    assert_eq!(ids(&torn), [json!(newest), json!(first)]);
    let after = api("/repos/fixture-dev/app/releases")
        .token(&dev)
        .send(port);
    assert_eq!(ids(&after), [json!(first)]);
}
