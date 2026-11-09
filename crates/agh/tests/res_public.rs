// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `public()` over agh-fake: the anonymous download host's probe and fetch, and the fetch's
//! API asset route while the download host fails.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use agh::download::part_path;
use agh::fx::Fx;
use agh::github::public::PATIENCE;
use agh::retry::RetryPolicy;
use agh::wire::http::HttpTransport;
use agh::{Github, GithubBuilder, Kind};
use agh_fake::{AssetSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec};

use crate::common;

const APP: &str = "fixture-dev/app";
const BYTES: &[u8] = b"public bytes";

struct World {
    root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    gh: Github,
}

impl World {
    fn new(label: &str) -> World {
        let root = common::scratch(label);
        let fake = FakeGithub::start();
        common::canonical_tokens(root.path(), &fake, true, false);
        fake.add_repo(APP, RepoSpec::public());
        let release = fake.add_release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
        fake.add_asset(release, AssetSpec::uploaded("app.bin", BYTES));
        let router = common::router(&common::canonical_config(root.path()), root.path());
        let gh = common::github(router, &fake);
        World { root, fake, gh }
    }

    fn anonymous_throughout(&self) {
        assert!(self.gh.meter().rest_requests.is_empty());
        assert_eq!(
            self.gh.meter().as_,
            Some(agh::error::IdentityRef::Anonymous),
            "a request was sent as a profile"
        );
        let seen = self.fake.seen();
        assert!(!seen.is_empty());
        assert!(
            seen.iter().all(|s| !s.had_authorization),
            "{:?}",
            seen.iter()
                .map(|s| (s.origin, s.path_and_query.clone(), s.had_authorization))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn probe_answers_present_absent_and_unknown() {
    let w = World::new("pub-probe");
    let public = w.gh.public();
    assert_eq!(
        public.probe(&common::slug(APP), "v1", "app.bin"),
        agh::github::Probe::Present
    );
    assert_eq!(
        public.probe(&common::slug(APP), "v1", "nothing.bin"),
        agh::github::Probe::Absent
    );
    w.fake.fault(
        Match::web("/"),
        Fault::Status {
            status: 307,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    assert_eq!(
        public.probe(&common::slug(APP), "v1", "app.bin"),
        agh::github::Probe::Present
    );
    w.fake.fault(
        Match::web("/"),
        Fault::Status {
            status: 500,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    assert!(matches!(
        public.probe(&common::slug(APP), "v1", "app.bin"),
        agh::github::Probe::Unknown(_)
    ));
    assert!(w.fake.seen().iter().all(|s| s.method == "HEAD"));
    w.anonymous_throughout();
    w.fake.assert_clean();
}

#[test]
fn fetch_writes_the_bytes_through_one_hop_with_no_credential() {
    let w = World::new("pub-fetch");
    let dest = w.root.path().join("app.bin");
    let written =
        w.gh.public()
            .fetch(&common::slug(APP), "v1", "app.bin", &dest, 1024)
            .expect("the fetch");
    assert_eq!(written, BYTES.len() as u64);
    assert_eq!(std::fs::read(&dest).expect("the file"), BYTES);
    assert!(!part_path(&dest).exists());
    let seen = w.fake.seen();
    let origins: Vec<FakeOrigin> = seen.iter().map(|s| s.origin).collect();
    assert_eq!(origins, [FakeOrigin::Web, FakeOrigin::Cdn]);
    w.anonymous_throughout();
    w.fake.assert_clean();
}

#[test]
fn fetch_over_the_cap_is_invalid_and_leaves_no_part_file() {
    let w = World::new("pub-cap");
    let dest = w.root.path().join("app.bin");
    let error =
        w.gh.public()
            .fetch(&common::slug(APP), "v1", "app.bin", &dest, 4)
            .expect_err("over the cap");
    assert_eq!(error.kind, Kind::Invalid);
    assert!(!dest.exists());
    assert!(!part_path(&dest).exists());
    w.anonymous_throughout();
    w.fake.assert_clean();
}

#[test]
fn fetch_of_an_absent_asset_is_not_found() {
    let w = World::new("pub-absent");
    let dest = w.root.path().join("nothing.bin");
    let error =
        w.gh.public()
            .fetch(&common::slug(APP), "v1", "nothing.bin", &dest, 1024)
            .expect_err("absent");
    assert_eq!(error.kind, Kind::NotFound);
    assert!(error.why.contains("nothing.bin"), "{}", error.why);
    assert!(!dest.exists());
    w.anonymous_throughout();
    w.fake.assert_clean();
}

#[test]
fn a_dot_segment_tag_or_asset_is_refused_before_any_request() {
    let w = World::new("pub-dots");
    let public = w.gh.public();
    let dest = w.root.path().join("app.bin");
    for (tag, asset) in [
        ("..", "app.bin"),
        ("v1/../../../../fixture-dev/other", "app.bin"),
        ("v1/", "app.bin"),
        ("v1", "x/../../../../fixture-dev/other/app.bin"),
        ("v1", "./app.bin"),
        ("v1", ""),
        ("v1", "/app.bin"),
        ("v1", "app.bin/"),
        ("v1", "app\n.bin"),
    ] {
        let error = public
            .fetch(&common::slug(APP), tag, asset, &dest, 1024)
            .expect_err("a dot-segment");
        assert_eq!(error.kind, Kind::Usage, "{tag} {asset}: {}", error.why);
        assert!(matches!(
            public.probe(&common::slug(APP), tag, asset),
            agh::github::Probe::Unknown(_)
        ));
    }
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    assert!(!dest.exists());
    w.fake.assert_clean();
}

/// NEGATIVE CONTROL: `Public::fetch` refuses `.agh-part` destinations, directories, and live or
/// dangling symlinks with `Kind::Usage` before issuing any network request.
#[test]
fn a_part_name_or_non_regular_dest_is_refused_before_any_request() {
    let w = World::new("pub-dest-guard");
    let public = w.gh.public();
    let slug = common::slug(APP);
    let target = w.root.path().join("target.bin");
    std::fs::write(&target, b"keep").expect("write target");
    let live_link = w.root.path().join("live.bin");
    std::os::unix::fs::symlink(&target, &live_link).expect("live link");
    let dangling_link = w.root.path().join("dangling.bin");
    std::os::unix::fs::symlink(w.root.path().join("absent.bin"), &dangling_link)
        .expect("dangling link");
    let dir_dest = w.root.path().join("dir.bin");
    std::fs::create_dir(&dir_dest).expect("dir dest");
    let part_dest = w.root.path().join("app.bin.agh-part");

    for bad_dest in [&part_dest, &dir_dest, &live_link, &dangling_link] {
        let error = public
            .fetch(&slug, "v1", "app.bin", bad_dest, 1024)
            .expect_err("non-regular or part-name dest must fail");
        assert_eq!(
            error.kind,
            Kind::Usage,
            "{}: {}",
            bad_dest.display(),
            error.why
        );
    }
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    assert_eq!(std::fs::read(&target).expect("target intact"), b"keep");
    w.fake.assert_clean();
}

#[test]
fn rest_meter_counts_both_identities_and_excludes_public_transfers() {
    let w = World::new("pub-rest-meter");
    let slug = common::slug(APP);
    w.gh.repos().get(&slug).unwrap();
    w.gh.repos()
        .how(agh::How::default().anonymous(true))
        .get(&slug)
        .unwrap();
    let before = w.gh.meter();
    assert_eq!(before.rest_requests.get("dev"), Some(&1));
    assert_eq!(before.rest_requests.get("anonymous"), Some(&1));
    let observed_api = w
        .fake
        .seen()
        .iter()
        .filter(|s| s.origin == FakeOrigin::Api)
        .count();
    assert_eq!(observed_api, 2);
    assert_eq!(before.rest_requests.values().sum::<u32>(), 2);
    // Negative control for classifying all HTTP requests as REST: a web/CDN
    // download increments total requests but must leave both REST counters fixed.
    w.gh.public()
        .fetch(&slug, "v1", "app.bin", &w.root.path().join("public"), 1024)
        .unwrap();
    let after = w.gh.meter();
    assert!(after.requests > before.requests);
    assert_eq!(after.rest_requests, before.rest_requests);
}

/// `status` with an empty body.
fn status(status: u16) -> Fault {
    Fault::Status {
        status,
        body: String::new(),
        headers: Vec::new(),
    }
}

/// The origins `fake` saw, in order.
fn origins(fake: &FakeGithub) -> Vec<FakeOrigin> {
    fake.seen().iter().map(|s| s.origin).collect()
}

/// Every request `fake` saw went without a credential.
fn no_credential(fake: &FakeGithub) {
    let seen = fake.seen();
    assert!(
        seen.iter().all(|s| !s.had_authorization),
        "{:?}",
        seen.iter()
            .map(|s| (s.origin, s.path_and_query.clone(), s.had_authorization))
            .collect::<Vec<_>>()
    );
}

/// A 504 from the download host sends the fetch to the API asset route (the tag's release,
/// then the asset as octet-stream and its CDN hop), which delivers the bytes, anonymous although
/// a dev identity is routed. Negative control: `a_web_404_is_absent_even_where_the_api_route_
/// would_serve`, where the same asset on the same API asks nothing of the API.
#[test]
fn a_download_host_504_reads_the_asset_on_the_api_route_with_no_credential() {
    let w = World::new("pub-504-api");
    w.fake.fault_always(Match::web("/"), status(504));
    let dest = w.root.path().join("app.bin");
    let written =
        w.gh.public()
            .fetch(&common::slug(APP), "v1", "app.bin", &dest, 1024)
            .expect("the API asset route serves");
    assert_eq!(written, BYTES.len() as u64);
    assert_eq!(std::fs::read(&dest).expect("the file"), BYTES);
    assert!(!part_path(&dest).exists());
    assert_eq!(
        origins(&w.fake),
        [
            FakeOrigin::Web,
            FakeOrigin::Api,
            FakeOrigin::Api,
            FakeOrigin::Cdn
        ]
    );
    let paths: Vec<String> = w
        .fake
        .seen()
        .iter()
        .map(|s| s.path_and_query.clone())
        .collect();
    assert_eq!(paths[1], format!("/repos/{APP}/releases/tags/v1"));
    assert!(
        paths[2].starts_with(&format!("/repos/{APP}/releases/assets/")),
        "{paths:?}"
    );
    no_credential(&w.fake);
    let meter = w.gh.meter();
    assert_eq!(meter.rest_requests.get("anonymous"), Some(&2), "{meter:?}");
    assert_eq!(meter.rest_requests.get("dev"), None, "{meter:?}");
    assert!(w.gh.fx().slept().is_empty());
    w.fake.assert_clean();
}

/// A connection the download host drops unanswered is a failure in transit too.
#[test]
fn a_dropped_download_host_connection_reads_the_asset_on_the_api_route() {
    let w = World::new("pub-close-api");
    w.fake.fault_always(Match::web("/"), Fault::Close);
    let dest = w.root.path().join("app.bin");
    w.gh.public()
        .fetch(&common::slug(APP), "v1", "app.bin", &dest, 1024)
        .expect("the API asset route serves");
    assert_eq!(std::fs::read(&dest).expect("the file"), BYTES);
    assert!(origins(&w.fake).contains(&FakeOrigin::Cdn));
    no_credential(&w.fake);
}

/// When both routes keep failing, the fetch alternates them until no byte has arrived for
/// [`PATIENCE`] (its pauses add up to exactly that, whatever the round count), and the error
/// names both routes; it is never the asset's absence. Negative control: the 504 test above,
/// where one round's API leg ends the read.
#[test]
fn both_routes_failing_names_both_and_is_never_absence() {
    let w = World::new("pub-both-fail");
    w.fake.fault_always(Match::web("/"), status(504));
    w.fake.fault_always(Match::api("/repos/"), status(503));
    let dest = w.root.path().join("app.bin");
    let error =
        w.gh.public()
            .fetch(&common::slug(APP), "v1", "app.bin", &dest, 1024)
            .expect_err("neither route serves");
    assert_eq!(error.kind, Kind::Server, "{}", error.why);
    for part in [
        "the download host:",
        "the API asset route:",
        "no route delivered a byte for 300s",
    ] {
        assert!(error.why.contains(part), "{part}: {}", error.why);
    }
    assert_eq!(w.gh.fx().slept().iter().sum::<Duration>(), PATIENCE);
    let seen = origins(&w.fake);
    let web = seen.iter().filter(|o| **o == FakeOrigin::Web).count();
    let api = seen.iter().filter(|o| **o == FakeOrigin::Api).count();
    assert!(web > 1 && web == api, "{seen:?}");
    assert_eq!(w.gh.fx().retry_notes().len(), web - 1);
    assert!(!dest.exists());
    assert!(!part_path(&dest).exists());
    no_credential(&w.fake);
}

/// A 404 from the download host is the asset's absence: the API is never asked, though it holds
/// the asset (the 504 test above reads it there).
#[test]
fn a_web_404_is_absent_even_where_the_api_route_would_serve() {
    let w = World::new("pub-404-absent");
    w.fake.fault(Match::web("/"), status(404));
    let dest = w.root.path().join("app.bin");
    let error =
        w.gh.public()
            .fetch(&common::slug(APP), "v1", "app.bin", &dest, 1024)
            .expect_err("absent");
    assert_eq!(error.kind, Kind::NotFound);
    assert!(error.why.contains("app.bin"), "{}", error.why);
    assert_eq!(origins(&w.fake), [FakeOrigin::Web]);
    assert!(w.gh.fx().slept().is_empty());
    assert!(!dest.exists());
    w.anonymous_throughout();
    w.fake.assert_clean();
}

/// Once the download host failed, a release the API lacks the asset in is the asset's absence,
/// named with the download host's failure.
#[test]
fn an_asset_the_api_route_lacks_is_absent_once_the_download_host_fails() {
    let w = World::new("pub-api-absent");
    w.fake.fault_always(Match::web("/"), status(504));
    let dest = w.root.path().join("nothing.bin");
    let error =
        w.gh.public()
            .fetch(&common::slug(APP), "v1", "nothing.bin", &dest, 1024)
            .expect_err("absent");
    assert_eq!(error.kind, Kind::NotFound);
    assert!(
        error.why.contains("the download host failed")
            && error.why.contains("release v1 has no uploaded nothing.bin"),
        "{}",
        error.why
    );
    assert_eq!(origins(&w.fake), [FakeOrigin::Web, FakeOrigin::Api]);
    assert!(w.gh.fx().slept().is_empty());
    assert!(!dest.exists());
    no_credential(&w.fake);
}

/// Under a zero anonymous API budget (`--no-anonymous-api`) the API leg is refused before it is
/// sent: the fetch keeps to the download host, and its error names the refusal. Negative
/// control: the same outage under the default budget, which the 504 test reads on the API.
#[test]
fn a_zero_anonymous_api_budget_keeps_the_fetch_on_the_download_host() {
    let w = World::new("pub-zero-budget");
    let gh = GithubBuilder::new(common::router(
        &common::canonical_config(w.root.path()),
        w.root.path(),
    ))
    .transport(Arc::new(HttpTransport::direct(Arc::new(common::env()))))
    .endpoints(common::endpoints(&w.fake))
    .env(Arc::new(common::env()))
    .fx(Arc::new(Fx::simulated(common::SIM_START)))
    .caller("test")
    .retry(RetryPolicy::NONE)
    .refuse_anonymous_api(true)
    .build();
    w.fake.fault_always(Match::web("/"), status(504));
    let dest = w.root.path().join("app.bin");
    let error = gh
        .public()
        .fetch(&common::slug(APP), "v1", "app.bin", &dest, 1024)
        .expect_err("the download host never serves");
    assert!(
        error.why.contains("anonymous API budget is zero")
            && error.why.contains("no route delivered a byte for 300s"),
        "{}",
        error.why
    );
    assert!(origins(&w.fake).iter().all(|o| *o == FakeOrigin::Web));
    assert_eq!(gh.fx().slept().iter().sum::<Duration>(), PATIENCE);
    assert!(gh.meter().rest_requests.is_empty());
}

/// An asset the API route lists over the cap ends the fetch at once as Invalid, before its
/// bytes are asked for. Negative control: the 504 test above, whose cap admits the asset.
#[test]
fn an_asset_the_api_route_lists_over_the_cap_is_invalid_at_once() {
    let w = World::new("pub-api-cap");
    w.fake.fault_always(Match::web("/"), status(504));
    let dest = w.root.path().join("app.bin");
    let error =
        w.gh.public()
            .fetch(&common::slug(APP), "v1", "app.bin", &dest, 4)
            .expect_err("over the cap");
    assert_eq!(error.kind, Kind::Invalid, "{}", error.why);
    assert_eq!(origins(&w.fake), [FakeOrigin::Web, FakeOrigin::Api]);
    assert!(w.gh.fx().slept().is_empty());
    assert!(!dest.exists());
    assert!(!part_path(&dest).exists());
}
