// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `download::fetch`: routed and anonymous downloads, their digests and part files.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agh::download::{DigestCheck, Expect, Sink, part_path};
use agh::model::{AssetObject, Sha256Digest};
use agh::wire::WireKind;
use agh::wire::scripted::ScriptedTransport;
use agh::{Github, How, Kind, Sub};
use agh_fake::{
    AssetSpec, Change, DigestSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec,
};

use crate::common;

const APP: &str = "fixture-dev/app";
const BYTES: &[u8] = b"the asset's bytes, all of them";

struct World {
    root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    gh: Github,
    release: u64,
}

impl World {
    fn new(label: &str) -> World {
        let root = common::scratch(label);
        let fake = FakeGithub::start();
        common::canonical_tokens(root.path(), &fake, true, false);
        fake.add_repo(APP, RepoSpec::public().push("fixture-dev-bot"));
        let release = fake.add_release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
        let router = common::router(&common::canonical_config(root.path()), root.path());
        let gh = common::github(router, &fake);
        World {
            root,
            fake,
            gh,
            release,
        }
    }

    fn asset(&self, spec: AssetSpec) -> AssetObject {
        let id = self.fake.add_asset(self.release, spec);
        self.gh
            .releases(&common::slug(APP))
            .asset(id)
            .expect("the asset's object")
    }

    fn dest(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn file(
        &self,
        dest: &Path,
        asset: &AssetObject,
        clobber: bool,
    ) -> agh::Result<agh::download::Downloaded> {
        let expect = Expect::from_asset(asset).expect("an expectation");
        self.gh.releases(&common::slug(APP)).download(
            asset,
            Sink::File {
                path: dest.to_path_buf(),
                clobber,
            },
            expect,
        )
    }
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(aterm_digest::Sha256::digest(bytes))
}

fn nothing_remains(dest: &Path) {
    assert!(!dest.exists(), "{} remains", dest.display());
    assert!(!part_path(dest).exists(), "the part file remains");
}

#[test]
fn a_routed_download_streams_checks_and_renames() {
    let w = World::new("dl-routed");
    let asset = w.asset(AssetSpec::uploaded("a.bin", BYTES));
    w.fake.clear_seen();
    let dest = w.dest("a.bin");
    let done = w.file(&dest, &asset, false).expect("the download");
    assert_eq!(std::fs::read(&dest).expect("the file"), BYTES);
    assert_eq!(done.bytes, BYTES.len() as u64);
    assert_eq!(done.sha256, digest(BYTES));
    assert_eq!(done.digest, DigestCheck::Matched);
    assert!(done.written);
    assert_eq!(done.path.as_deref(), Some(dest.as_path()));
    assert_eq!(done.asset_id, asset.id);
    assert!(!part_path(&dest).exists());

    let seen = w.fake.seen();
    let asset_path = format!("/repos/{APP}/releases/assets/{}", asset.id);
    let api_gets: Vec<_> = seen
        .iter()
        .filter(|s| s.origin == FakeOrigin::Api && s.method == "GET")
        .filter(|s| {
            s.path_and_query
                .starts_with(&format!("/repos/{APP}/releases/assets/"))
        })
        .collect();
    assert_eq!(api_gets.len(), 1);
    assert_eq!(api_gets[0].path_and_query, asset_path);
    assert_eq!(
        api_gets[0].header("accept"),
        Some("application/octet-stream")
    );
    assert!(api_gets[0].had_authorization);
    assert_eq!(api_gets[0].as_login.as_deref(), Some("fixture-dev-bot"));
    assert!(seen.iter().all(|s| s.origin != FakeOrigin::Web));
    let cdn: Vec<_> = seen
        .iter()
        .filter(|s| s.origin == FakeOrigin::Cdn)
        .collect();
    assert_eq!(cdn.len(), 1);
    assert!(!cdn[0].had_authorization);

    let again = w.file(&dest, &asset, false).expect("the second download");
    assert!(!again.written);
    assert_eq!(again.requests, 0);
    w.fake.assert_clean();
}

#[test]
fn a_wrong_digest_is_a_conflict_and_leaves_nothing() {
    let w = World::new("dl-wrong");
    let mut spec = AssetSpec::uploaded("a.bin", BYTES);
    spec.digest = DigestSpec::Wrong;
    let asset = w.asset(spec);
    let dest = w.dest("a.bin");
    let error = w.file(&dest, &asset, false).expect_err("a mismatch");
    assert_eq!(error.kind, Kind::Conflict);
    assert_eq!(error.exit(), 9);
    match &error.sub {
        Sub::DigestMismatch { got, .. } => assert_eq!(got, &digest(BYTES).to_string()),
        other => panic!("not a digest mismatch: {other:?}"),
    }
    assert_eq!(error.repo.as_deref(), Some(APP));
    nothing_remains(&dest);
    w.fake.assert_clean();
}

#[test]
fn a_null_digest_is_absent_after_the_bracket_recheck() {
    let w = World::new("dl-null");
    let mut spec = AssetSpec::uploaded("a.bin", BYTES);
    spec.digest = DigestSpec::Null;
    let asset = w.asset(spec);
    let dest = w.dest("a.bin");
    let done = w.file(&dest, &asset, false).expect("the download");
    assert_eq!(done.digest, DigestCheck::Absent);
    assert_eq!(done.sha256, digest(BYTES));
    assert_eq!(std::fs::read(&dest).expect("the file"), BYTES);
    w.fake.assert_clean();
}

#[test]
fn an_asset_replaced_mid_download_is_a_conflict_and_leaves_nothing() {
    let w = World::new("dl-replaced");
    let mut spec = AssetSpec::uploaded("a.bin", BYTES);
    spec.digest = DigestSpec::Null;
    let asset = w.asset(spec.clone());
    let mut replaced = spec;
    replaced.bytes = BYTES.iter().rev().copied().collect();
    replaced.updated_at = "2026-10-02T00:00:00Z".to_string();
    w.fake.after(
        Match::cdn(),
        1,
        Change::AddAsset {
            release_id: w.release,
            spec: replaced,
        },
    );
    let dest = w.dest("a.bin");
    let error = w.file(&dest, &asset, false).expect_err("changed");
    assert_eq!(error.kind, Kind::Conflict);
    nothing_remains(&dest);
    w.fake.assert_clean();
}

#[test]
fn an_open_asset_is_not_uploaded() {
    let w = World::new("dl-open");
    let mut spec = AssetSpec::uploaded("a.bin", BYTES);
    spec.state = "open".to_string();
    let asset = w.asset(spec);
    let dest = w.dest("a.bin");
    let error = w.file(&dest, &asset, false).expect_err("not uploaded");
    assert_eq!(error.kind, Kind::Incomplete);
    assert_eq!(
        error.sub,
        Sub::NotUploaded {
            state: "open".into()
        }
    );
    nothing_remains(&dest);
    w.fake.assert_clean();
}

#[test]
fn a_truncated_cdn_body_restarts_when_the_range_is_ignored() {
    let w = World::new("dl-truncated");
    let asset = w.asset(AssetSpec::uploaded("a.bin", BYTES));
    w.fake.clear_seen();
    w.fake.fault(Match::cdn(), Fault::TruncateBody { at: 4 });
    let dest = w.dest("a.bin");
    let done = w
        .file(&dest, &asset, false)
        .expect("the full 200 replaces the partial prefix");
    assert_eq!(std::fs::read(&dest).expect("the completed asset"), BYTES);
    assert_eq!(done.bytes, BYTES.len() as u64);
    assert_eq!(w.gh.fx().retry_notes().len(), 1);
    assert!(!part_path(&dest).exists());
    w.fake.assert_clean();
}

#[test]
fn a_destination_with_other_bytes_needs_clobber() {
    let w = World::new("dl-clobber");
    let asset = w.asset(AssetSpec::uploaded("a.bin", BYTES));
    let dest = w.dest("a.bin");
    std::fs::write(&dest, b"other bytes").expect("the old file");
    w.fake.clear_seen();
    let error = w.file(&dest, &asset, false).expect_err("different bytes");
    assert_eq!(error.kind, Kind::Conflict);
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    assert_eq!(std::fs::read(&dest).expect("kept"), b"other bytes");
    assert!(!part_path(&dest).exists());
    let done = w.file(&dest, &asset, true).expect("clobbered");
    assert!(done.written);
    assert_eq!(std::fs::read(&dest).expect("replaced"), BYTES);
    w.fake.assert_clean();
}

#[test]
fn bounded_and_writer_sinks_hold_the_bytes() {
    let w = World::new("dl-sinks");
    let asset = w.asset(AssetSpec::uploaded("a.bin", BYTES));
    let releases = w.gh.releases(&common::slug(APP));
    let expect = Expect::from_asset(&asset).expect("an expectation");
    let held = releases
        .download(&asset, Sink::Bounded(1024), expect.clone())
        .expect("into memory");
    assert_eq!(held.data.as_deref(), Some(BYTES));
    assert_eq!(held.path, None);

    let mut out: Vec<u8> = Vec::new();
    let written = releases
        .download(&asset, Sink::Writer(&mut out), expect)
        .expect("into a writer");
    assert_eq!(out, BYTES);
    assert_eq!(written.bytes, BYTES.len() as u64);
    assert_eq!(written.data, None);
    w.fake.assert_clean();
}

#[test]
fn a_writer_resumes_its_spool_and_exposes_only_the_complete_body() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer_body_fault(200, &[], b"first ", WireKind::Reset);
    transport.answer(206, &[("Content-Range", "bytes 6-9/10")], b"body");
    let gh = common::scripted(&transport, 47_201);
    let got = digest(b"first body");
    let asset = AssetObject {
        id: 7,
        name: "a.bin".to_string(),
        size: 10,
        digest: Some(got.to_string()),
        state: "uploaded".to_string(),
        content_type: "application/octet-stream".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        browser_download_url: String::new(),
        download_count: 0,
        raw: aterm_json::json!({}),
    };
    let mut out = Vec::new();
    let done = gh
        .releases(&common::slug(APP))
        .download(
            &asset,
            Sink::Writer(&mut out),
            Expect {
                size: 10,
                digest: Some(got),
                limit: 10,
            },
        )
        .expect("the range completes the spool");
    assert_eq!(out, b"first body");
    assert_eq!(done.requests, 2);
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1]
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("range"))
            .map(|(_, value)| value.as_str()),
        Some("bytes=6-")
    );
    assert_eq!(gh.fx().retry_notes().len(), 1);
}

#[test]
fn a_bounded_memory_asset_resumes_from_its_completed_prefix() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer_body_fault(200, &[], b"first ", WireKind::Timeout);
    transport.answer(206, &[("Content-Range", "bytes 6-9/10")], b"body");
    let gh = common::scripted(&transport, 47_203);
    let got = digest(b"first body");
    let asset = AssetObject {
        id: 9,
        name: "memory.bin".to_string(),
        size: 10,
        digest: Some(got.to_string()),
        state: "uploaded".to_string(),
        content_type: "application/octet-stream".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        browser_download_url: String::new(),
        download_count: 0,
        raw: aterm_json::json!({}),
    };
    let done = gh
        .releases(&common::slug(APP))
        .download(
            &asset,
            Sink::Bounded(10),
            Expect {
                size: 10,
                digest: Some(got),
                limit: 10,
            },
        )
        .expect("the range completes the memory body");
    assert_eq!(done.data.as_deref(), Some(b"first body".as_slice()));
    assert_eq!(done.requests, 2);
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1]
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("range"))
            .map(|(_, value)| value.as_str()),
        Some("bytes=6-")
    );
    assert_eq!(gh.fx().retry_notes().len(), 1);
}

#[test]
fn a_file_asset_resumes_and_hashes_the_complete_prefix_plus_range() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer_body_fault(200, &[], b"first ", WireKind::Reset);
    transport.answer(206, &[("Content-Range", "bytes 6-9/10")], b"body");
    let gh = common::scripted(&transport, 47_202);
    let bytes = b"first body";
    let got = digest(bytes);
    let asset = AssetObject {
        id: 8,
        name: "range.bin".to_string(),
        size: 10,
        digest: Some(got.to_string()),
        state: "uploaded".to_string(),
        content_type: "application/octet-stream".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        browser_download_url: String::new(),
        download_count: 0,
        raw: aterm_json::json!({}),
    };
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dest = root.path().join("range.bin");
    let done = gh
        .releases(&common::slug(APP))
        .download(
            &asset,
            Sink::File {
                path: dest.clone(),
                clobber: false,
            },
            Expect {
                size: 10,
                digest: Some(got),
                limit: 10,
            },
        )
        .expect("the range completes the asset");
    assert_eq!(std::fs::read(&dest).expect("asset"), bytes);
    assert_eq!(done.sha256, got);
    assert_eq!(done.requests, 2);
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1]
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("range"))
            .map(|(_, value)| value.as_str()),
        Some("bytes=6-")
    );
    assert_eq!(gh.fx().retry_notes().len(), 1);
}

#[test]
fn an_anonymous_download_goes_through_the_web_url_and_still_checks_the_digest() {
    let w = World::new("dl-anonymous");
    let asset = w.asset(AssetSpec::uploaded("a.bin", BYTES));
    w.fake.clear_seen();
    let dest = w.dest("a.bin");
    let expect = Expect::from_asset(&asset).expect("an expectation");
    let done =
        w.gh.releases(&common::slug(APP))
            .how(How::read().anonymous(true))
            .download(
                &asset,
                Sink::File {
                    path: dest.clone(),
                    clobber: false,
                },
                expect,
            )
            .expect("the download");
    assert_eq!(done.digest, DigestCheck::Matched);
    assert_eq!(std::fs::read(&dest).expect("the file"), BYTES);
    let seen = w.fake.seen();
    let origins: Vec<FakeOrigin> = seen.iter().map(|s| s.origin).collect();
    assert_eq!(origins, [FakeOrigin::Web, FakeOrigin::Cdn]);
    assert!(seen.iter().all(|s| !s.had_authorization));
    assert!(w.gh.meter().hopped);
    w.fake.assert_clean();
}

#[test]
fn a_bracket_recheck_that_retries_counts_every_request() {
    let w = World::new("dl-recheck-retry");
    let mut spec = AssetSpec::uploaded("a.bin", BYTES);
    spec.digest = DigestSpec::Null;
    let asset = w.asset(spec);
    w.fake.clear_seen();
    w.fake.fault(
        Match::api("/repos/fixture-dev/app/releases/assets/"),
        Fault::Status {
            status: 502,
            body: "{\"message\":\"Bad Gateway\"}".to_string(),
            headers: Vec::new(),
        },
    );
    let dest = w.dest("a.bin");
    let expect = Expect::from_asset(&asset).expect("an expectation");
    let done =
        w.gh.releases(&common::slug(APP))
            .how(
                How::read()
                    .anonymous(true)
                    .retry_policy(agh::retry::RetryPolicy::STANDARD),
            )
            .download(
                &asset,
                Sink::File {
                    path: dest.clone(),
                    clobber: false,
                },
                expect,
            )
            .expect("the download");
    assert_eq!(done.digest, DigestCheck::Absent);
    let seen = w.fake.seen();
    let rechecks = seen
        .iter()
        .filter(|s| s.origin == FakeOrigin::Api && s.path_and_query.contains("/assets/"))
        .count();
    assert_eq!(rechecks, 2, "{seen:?}");
    assert_eq!(done.requests as usize, seen.len(), "{seen:?}");
    w.fake.assert_clean();
}

#[test]
fn a_destination_named_like_a_part_file_is_usage_before_any_request() {
    let w = World::new("dl-part-name");
    let asset = w.asset(AssetSpec::uploaded("x.agh-part", BYTES));
    let finished = w.dest("x.agh-part");
    std::fs::write(&finished, BYTES).expect("a finished file");
    w.fake.clear_seen();
    let error = w.file(&finished, &asset, true).expect_err("a part name");
    assert_eq!(error.kind, Kind::Usage, "{}", error.why);
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    assert_eq!(std::fs::read(&finished).expect("still there"), BYTES);
    let upper = w.asset(AssetSpec::uploaded("Y.AGH-PART", BYTES));
    let shouted = w.dest("Y.AGH-PART");
    std::fs::write(&shouted, BYTES).expect("a finished file");
    w.fake.clear_seen();
    let error = w.file(&shouted, &upper, true).expect_err("a part name");
    assert_eq!(error.kind, Kind::Usage, "{}", error.why);
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    assert_eq!(std::fs::read(&shouted).expect("still there"), BYTES);
    w.fake.assert_clean();
}

#[test]
fn a_destination_that_is_not_a_regular_file_is_usage_before_any_request() {
    let w = World::new("dl-not-a-file");
    let mut spec = AssetSpec::uploaded("a.bin", BYTES);
    spec.digest = DigestSpec::Null;
    let null = w.asset(spec);
    let known = w.asset(AssetSpec::uploaded("b.bin", BYTES));
    let dir = w.dest("a-directory");
    std::fs::create_dir(&dir).expect("the directory");
    for (asset, clobber) in [(&null, false), (&null, true), (&known, false)] {
        w.fake.clear_seen();
        let error = w.file(&dir, asset, clobber).expect_err("a directory");
        assert_eq!(error.kind, Kind::Usage, "{}", error.why);
        assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
        assert!(!part_path(&dir).exists());
        assert!(dir.is_dir());
    }
    w.fake.assert_clean();
}

/// NEGATIVE CONTROL: A live symlink (even pointing to a regular file with matching SHA-256) or a
/// dangling symlink at `dest` is rejected with `Kind::Usage` before any request is sent, rather
/// than leaving `dest` as a symlink or overwriting a dangling symlink without `--clobber`.
#[test]
fn a_symlink_destination_is_usage_before_any_request() {
    let w = World::new("dl-symlink-dest");
    let known = w.asset(AssetSpec::uploaded("b.bin", BYTES));
    let mut spec = AssetSpec::uploaded("a.bin", BYTES);
    spec.digest = DigestSpec::Null;
    let null = w.asset(spec);

    let target = w.dest("outside.bin");
    std::fs::write(&target, BYTES).expect("target with matching bytes");
    let live_link = w.dest("live-link.bin");
    std::os::unix::fs::symlink(&target, &live_link).expect("live symlink");

    let missing_target = w.dest("missing-target.bin");
    let dangling_link = w.dest("dangling-link.bin");
    std::os::unix::fs::symlink(&missing_target, &dangling_link).expect("dangling symlink");

    for link in [&live_link, &dangling_link] {
        for (asset, clobber) in [
            (&known, false),
            (&known, true),
            (&null, false),
            (&null, true),
        ] {
            w.fake.clear_seen();
            let error = w
                .file(link, asset, clobber)
                .expect_err("symlink destination");
            assert_eq!(error.kind, Kind::Usage, "{}", error.why);
            assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
            assert!(!part_path(link).exists());
            assert!(
                std::fs::symlink_metadata(link)
                    .expect("link metadata")
                    .file_type()
                    .is_symlink()
            );
        }
    }
    assert!(!missing_target.exists());
    assert_eq!(std::fs::read(&target).expect("target kept"), BYTES);
    w.fake.assert_clean();
}
