// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `public()` over agh-fake: the anonymous download host's probe and fetch.

#![cfg(unix)]

use agh::download::part_path;
use agh::{Github, Kind};
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
