// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Channel mirror tests over a fake release host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use atpkg_publish::artifact::upload;
use atpkg_publish::channel::mirror::{self, MirrorOptions, MirrorReport};
use atpkg_publish::ctx::HostChoice;
use atpkg_publish::error::{Error, Result};
use atpkg_publish::host::fake::{FakeFail, FakeHost, FakeOp, FakeRelease};
use atpkg_publish::host::{HostError, Hosts, Probe, Release, ReleaseHost, Replace, Slug};
use atpkg_publish::quad::{self, VerifiedQuad};

use super::{Doc, Rig};

const MAC: &str = "aarch64-apple-darwin";
const LINUX: &str = "x86_64-unknown-linux-gnu";
const LINUX_ARM: &str = "aarch64-unknown-linux-gnu";
const TARBALL: &str = "ay-8256-aarch64-apple-darwin.tar.zst";

fn public() -> Slug {
    Slug::new("alabsystems", "ay")
}

fn staging() -> Slug {
    Slug::new("alabsystems", "ay")
}

fn opts(dry_run: bool) -> MirrorOptions {
    MirrorOptions { dry_run }
}

fn setup(label: &str, doc: Doc) -> (Rig, VerifiedQuad) {
    let rig = Rig::new(label);
    rig.lay_pack("alabsystems", "ay", 8256, &[MAC]);
    let dir = rig.path("quad");
    rig.quad_dir(&dir, &doc);
    let q = quad::verify_dir(&rig.ctx, &dir).expect("the quad verifies");
    (rig, q)
}

fn index() -> Doc {
    Doc::new(7, &[("ay", 8256)])
}

fn run(rig: &Rig, q: &VerifiedQuad, dry: bool) -> (Result<MirrorReport>, String) {
    let mut log = Rig::log();
    let r = mirror::mirror(&rig.ctx, q, &opts(dry), &mut log);
    (r, log.text())
}

fn ok(rig: &Rig, q: &VerifiedQuad, dry: bool) -> (MirrorReport, String) {
    let (r, log) = run(rig, q, dry);
    (r.unwrap_or_else(|e| panic!("{e}\n{log}")), log)
}

fn err(rig: &Rig, q: &VerifiedQuad, dry: bool) -> String {
    match run(rig, q, dry) {
        (Ok(r), log) => panic!("mirrored, but must be refused: {r:?}\n{log}"),
        (Err(e), _) => e.to_string(),
    }
}

fn writes(host: &FakeHost) -> Vec<String> {
    host.calls()
        .into_iter()
        .filter(|c| c.starts_with("create") || c.starts_with("upload"))
        .collect()
}

#[test]
fn an_absent_pack_is_checked_and_planned_with_exactly_its_signed_set() {
    let (rig, q) = setup("mirror-plan", index());
    let (report, _) = ok(&rig, &q, true);
    assert_eq!(
        report.planned,
        vec![(
            "atpkg-ay-8256".to_string(),
            vec![
                "pkg-ay-8256.toml".to_string(),
                "pkg-ay-8256.toml.sig".to_string(),
                TARBALL.to_string()
            ]
        )]
    );
    assert!(
        rig.host
            .calls()
            .contains(&"fetch alabsystems/ay atpkg-ay-8256 pkg-ay-8256.toml".to_string()),
        "the public copy is read on the download host"
    );
    assert!(writes(&rig.host).is_empty());
}

#[test]
fn a_one_shot_http_500_on_a_staging_asset_is_retried_to_completion() {
    let (rig, q) = setup("mirror-500", index());
    rig.host
        .fail_once(&staging(), "atpkg-ay-8256", "pkg-ay-8256.toml.sig");
    let (report, _) = ok(&rig, &q, true);
    assert_eq!(report.planned.len(), 1);
    let downloads = rig
        .host
        .calls()
        .iter()
        .filter(|c| c.starts_with("download alabsystems/ay"))
        .count();
    assert_eq!(downloads, 2, "the 500 fired once and the retry finished");
}

#[test]
fn every_extra_staging_file_is_refused_by_name() {
    for extra in [
        "README.txt",
        "pkg.ay.8256.toml",
        "pkg-ay-8256.toml.*",
        "ay.8256-aarch64-apple-darwin.tar.zst",
        "ay-8256-aarch64-apple-darwin.tar.zst.*",
    ] {
        let (rig, q) = setup("mirror-extra", index());
        rig.host
            .put_asset(&staging(), "atpkg-ay-8256", extra, b"x".to_vec());
        let e = err(&rig, &q, true);
        assert!(
            e.contains(&format!("unverifiable extra file {extra}")),
            "{extra}: {e}"
        );
        assert!(writes(&rig.host).is_empty());
    }
}

#[test]
fn a_staging_pack_whose_bytes_or_bind_disagree_with_its_signature_is_refused() {
    let (rig, q) = setup("mirror-hash", index());
    rig.host
        .put_asset(&staging(), "atpkg-ay-8256", TARBALL, b"tampered".to_vec());
    assert!(err(&rig, &q, true).contains("hash mismatch vs the signed manifest"));

    let rig = Rig::new("mirror-bind");
    let other = rig.pack("ay", 8257, &[MAC]);
    for (name, bytes) in other.files {
        let name = name.replace("8257", "8256");
        rig.host
            .put_asset(&staging(), "atpkg-ay-8256", &name, bytes);
    }
    let dir = rig.path("quad");
    rig.quad_dir(&dir, &index());
    let q = quad::verify_dir(&rig.ctx, &dir).expect("quad");
    let e = err(&rig, &q, true);
    assert!(e.contains("binds program=ay build=8257"), "{e}");
}

#[test]
fn a_publish_creates_the_signed_set_and_confirms_it_then_a_rerun_skips() {
    let (rig, q) = setup("mirror-publish", index());
    let (report, _) = ok(&rig, &q, false);
    assert_eq!(report.created, ["atpkg-ay-8256"]);
    let rel = rig
        .host
        .release_of(&public(), "atpkg-ay-8256")
        .expect("created");
    assert!(rel.prerelease);
    assert_eq!(
        rel.assets.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            upload::VERSION_RESERVATION,
            TARBALL,
            "pkg-ay-8256.toml",
            "pkg-ay-8256.toml.sig"
        ]
    );
    assert_eq!(
        rig.host.notes_of(&public(), "atpkg-ay-8256").as_deref(),
        Some(mirror::PUBLIC_NOTES)
    );
    assert!(
        rig.host
            .calls()
            .contains(&format!("probe alabsystems/ay atpkg-ay-8256 {TARBALL}"))
    );

    let before = rig.host.calls().len();
    let (report, _) = ok(&rig, &q, false);
    assert_eq!(report.skipped, ["atpkg-ay-8256"]);
    let calls = &rig.host.calls()[before..];
    assert!(
        !calls.iter().any(|c| c.contains("alabsystems")),
        "{calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("fetch") && c.ends_with(".tar.zst")),
        "{calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("create") || c.starts_with("upload"))
    );
}

#[test]
fn a_missing_public_asset_is_uploaded_alone_and_never_replaces() {
    let (rig, q) = setup("mirror-converge", index());
    ok(&rig, &q, false);
    let mut rel = rig
        .host
        .release_of(&public(), "atpkg-ay-8256")
        .expect("there");
    rel.assets.remove(TARBALL);
    rig.host.put(&public(), "atpkg-ay-8256", rel);
    let before = rig.host.calls().len();
    let (report, _) = ok(&rig, &q, false);
    assert_eq!(
        report.converged,
        vec![("atpkg-ay-8256".to_string(), vec![TARBALL.to_string()])]
    );
    let uploads: Vec<String> = rig.host.calls()[before..]
        .iter()
        .filter(|c| c.starts_with("upload"))
        .cloned()
        .collect();
    assert_eq!(
        uploads, ["upload alabsystems/ay atpkg-ay-8256 Never"; 2],
        "reservation reconciliation and the missing payload both remain create-only"
    );
}

#[test]
fn matching_reservation_metadata_is_verified_and_a_marker_only_public_release_resumes() {
    let (rig, q) = setup("mirror-reservation", index());
    let tag = "atpkg-ay-8256";
    let staged = rig.host.release_of(&staging(), tag).unwrap();
    let dir = rig.path("reserved-staging");
    std::fs::create_dir_all(&dir).unwrap();
    let files: Vec<_> = staged
        .assets
        .iter()
        .map(|(name, bytes)| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        })
        .collect();
    upload::publish(
        &rig.ctx,
        "ay",
        tag,
        &dir.join("pkg-ay-8256.toml"),
        &files,
        "fixture",
    )
    .unwrap();
    let staged = rig.host.release_of(&staging(), tag).unwrap();
    let marker = staged.assets[upload::VERSION_RESERVATION].clone();
    let (report, _) = ok(&rig, &q, false);
    assert_eq!(report.created, [tag]);
    let mut interrupted = rig.host.release_of(&public(), tag).unwrap();
    interrupted
        .assets
        .retain(|name, _| name == upload::VERSION_RESERVATION);
    interrupted.draft = true;
    rig.host.put(&public(), tag, interrupted);
    let (report, _) = ok(&rig, &q, false);
    assert_eq!(report.created, [tag]);
    let restored = rig.host.release_of(&public(), tag).unwrap();
    assert!(!restored.draft);
    assert_eq!(restored.assets[upload::VERSION_RESERVATION], marker);
    assert_eq!(restored.assets[TARBALL], staged.assets[TARBALL]);

    // A marker is metadata only when its exact version agrees with the verified
    // signed manifest; malformed metadata must not relax the no-extras rule.
    let before = rig.host.calls().len();
    rig.host.put_asset(
        &staging(),
        tag,
        upload::VERSION_RESERVATION,
        b"unknown".to_vec(),
    );
    let mut incomplete = restored.clone();
    incomplete.assets.remove(TARBALL);
    rig.host.put(&public(), tag, incomplete.clone());
    assert!(err(&rig, &q, false).contains("reservation is malformed or differs"));
    assert_eq!(rig.host.release_of(&public(), tag), Some(incomplete));
    assert!(
        !rig.host.calls()[before..]
            .iter()
            .any(|call| call.starts_with("upload") || call.starts_with("create"))
    );

    // Restore the valid staged record, then make the owner-side record disagree.
    rig.host.put(&staging(), tag, staged);
    let mut conflicting = restored;
    conflicting.assets.remove(TARBALL);
    let mut other_version = marker;
    *other_version.last_mut().unwrap() = b'x';
    conflicting
        .assets
        .insert(upload::VERSION_RESERVATION.to_owned(), other_version);
    rig.host.put(&public(), tag, conflicting.clone());
    assert!(err(&rig, &q, false).contains("one build number names one version"));
    assert_eq!(rig.host.release_of(&public(), tag), Some(conflicting));
}

#[test]
fn interrupted_draft_payloads_resume_only_from_the_exact_signed_staging_set() {
    let tag = "atpkg-ay-8256";
    for (op, failure) in [
        (FakeOp::Upload, FakeFail::Partial),
        (FakeOp::PublishDraft, FakeFail::Refuse),
    ] {
        let (rig, q) = setup("mirror-partial-draft", index());
        rig.host.fail_next(op, failure);
        assert!(run(&rig, &q, false).0.is_err());
        let draft = rig.host.release_of(&public(), tag).unwrap();
        assert!(draft.draft);
        assert!(draft.assets.contains_key(upload::VERSION_RESERVATION));
        assert!(draft.assets.len() > 1);
        assert!(rig.host.published_releases(&public()).unwrap().is_empty());
        let before = writes(&rig.host).len();
        let planned = ok(&rig, &q, true).0;
        let missing: Vec<String> = ["pkg-ay-8256.toml", "pkg-ay-8256.toml.sig", TARBALL]
            .into_iter()
            .filter(|name| !draft.assets.contains_key(*name))
            .map(str::to_owned)
            .collect();
        assert_eq!(planned.planned, [(tag.to_string(), missing)]);
        assert_eq!(writes(&rig.host).len(), before);
        assert_eq!(rig.host.release_of(&public(), tag), Some(draft.clone()));

        for corruption in ["extra", "reservation", "missing-reservation", "payload"] {
            let mut other = draft.clone();
            match corruption {
                "extra" => {
                    other
                        .assets
                        .insert("unrelated.bin".into(), b"extra".to_vec());
                }
                "reservation" => {
                    other
                        .assets
                        .insert(upload::VERSION_RESERVATION.into(), b"wrong".to_vec());
                }
                "missing-reservation" => {
                    other.assets.remove(upload::VERSION_RESERVATION);
                }
                "payload" => {
                    other.assets.insert(TARBALL.into(), b"other bytes".to_vec());
                }
                _ => unreachable!(),
            }
            rig.host.put(&public(), tag, other.clone());
            let before = writes(&rig.host).len();
            assert!(run(&rig, &q, false).0.is_err(), "{corruption}");
            assert_eq!(writes(&rig.host).len(), before, "{corruption}");
            assert_eq!(rig.host.release_of(&public(), tag), Some(other));
        }

        rig.host.put(&public(), tag, draft);
        ok(&rig, &q, false);
        let complete = rig.host.release_of(&public(), tag).unwrap();
        assert!(!complete.draft);
        for (name, bytes) in rig.host.release_of(&staging(), tag).unwrap().assets {
            assert_eq!(complete.assets[&name], bytes);
        }
    }
}

#[test]
fn a_published_pack_is_immutable_and_must_verify() {
    let (rig, q) = setup("mirror-immutable", index());
    ok(&rig, &q, false);
    let mut differs = rig
        .host
        .release_of(&public(), "atpkg-ay-8256")
        .expect("there");
    differs.assets.remove(TARBALL);
    differs
        .assets
        .get_mut("pkg-ay-8256.toml")
        .expect("manifest")
        .extend_from_slice(b"# another build of the same number\n");
    rig.host.put(&public(), "atpkg-ay-8256", differs);
    let e = err(&rig, &q, true);
    assert!(
        e.contains("PKG SELF-CHECK FAILED"),
        "a manifest its signature does not cover: {e}"
    );

    let (rig, q) = setup("mirror-differs", index());
    let stranger = rig.pack("ay", 8256, &[MAC, LINUX]);
    for (name, bytes) in &stranger.files {
        if name.starts_with("pkg-") {
            rig.host
                .put_asset(&public(), "atpkg-ay-8256", name, bytes.clone());
        }
    }
    let e = err(&rig, &q, true);
    assert!(e.contains("DIFFERS from staging"), "{e}");
}

#[test]
fn a_public_release_serving_no_manifest_is_refused_with_its_remedy() {
    let (rig, q) = setup("mirror-orphan", index());
    rig.host.put(
        &public(),
        "atpkg-ay-8256",
        FakeRelease {
            draft: true,
            prerelease: true,
            ..FakeRelease::default()
        },
    );
    let e = err(&rig, &q, false);
    assert!(e.contains("exists but serves no pkg-ay-8256.toml"), "{e}");
    assert!(writes(&rig.host).is_empty());
}

struct Dark {
    inner: Arc<FakeHost>,
    probe: Probe,
    gate: Option<Arc<ReadGate>>,
    failures: BTreeMap<String, String>,
    probe_only: bool,
}

impl Dark {
    fn new(inner: Arc<FakeHost>, probe: Probe) -> Self {
        Self {
            inner,
            probe,
            gate: None,
            failures: BTreeMap::new(),
            probe_only: false,
        }
    }
}

#[derive(Default)]
struct ReadGate {
    required: usize,
    entered: Mutex<usize>,
    changed: Condvar,
}

impl ReadGate {
    fn enter(&self) -> bool {
        let mut entered = self.entered.lock().expect("read gate");
        *entered += 1;
        self.changed.notify_all();
        let (entered, timeout) = self
            .changed
            .wait_timeout_while(entered, Duration::from_secs(60), |n| *n < self.required)
            .expect("read gate detector");
        !timeout.timed_out() || *entered >= self.required
    }
}

impl ReleaseHost for Dark {
    fn releases(&self, slug: &Slug) -> std::result::Result<Vec<Release>, HostError> {
        self.inner.releases(slug)
    }
    fn release(&self, slug: &Slug, tag: &str) -> std::result::Result<Release, HostError> {
        self.inner.release(slug, tag)
    }
    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> std::result::Result<(), HostError> {
        self.inner.download(slug, tag, assets, dir)
    }
    fn create(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> std::result::Result<(), HostError> {
        self.inner.create(slug, tag, title, notes, files)
    }
    fn create_draft(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> std::result::Result<(), HostError> {
        self.inner.create_draft(slug, tag, title, notes, files)
    }
    fn upload(
        &self,
        slug: &Slug,
        tag: &str,
        files: &[PathBuf],
        replace: Replace,
    ) -> std::result::Result<(), HostError> {
        self.inner.upload(slug, tag, files, replace)
    }
    fn publish_draft(&self, slug: &Slug, tag: &str) -> std::result::Result<(), HostError> {
        self.inner.publish_draft(slug, tag)
    }
    fn probe_public(&self, _: &Slug, _: &str, _: &str) -> Probe {
        self.probe.clone()
    }
    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Probe {
        if asset.ends_with(".toml")
            && let Some(gate) = &self.gate
            && !gate.enter()
        {
            return Probe::Unknown(
                "independent reads did not enter before the hang detector".into(),
            );
        }
        if let Some(why) = self.failures.get(tag) {
            return Probe::Unknown(why.clone());
        }
        match &self.probe {
            Probe::Unknown(_) if !self.probe_only => self.probe.clone(),
            _ => self.inner.fetch_public(slug, tag, asset, dest, cap),
        }
    }
    fn clone_repo(&self, slug: &Slug, dest: &Path) -> std::result::Result<(), HostError> {
        self.inner.clone_repo(slug, dest)
    }
    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> std::result::Result<(), HostError> {
        self.inner.fetch_repo(slug, dir)
    }
}

#[test]
fn an_unknown_head_uses_the_signed_public_payload_without_staging_or_writes() {
    let (mut rig, q) = setup("mirror-head-unknown", index());
    ok(&rig, &q, false);
    let before = rig.host.calls().len();
    let mut host = Dark::new(rig.host.clone(), Probe::Unknown("HTTP 500".into()));
    host.probe_only = true;
    rig.route(Arc::new(host));
    let (report, _) = ok(&rig, &q, false);
    assert_eq!(report.skipped, ["atpkg-ay-8256"]);
    let calls = &rig.host.calls()[before..];
    assert!(
        calls
            .iter()
            .any(|c| c == &format!("fetch alabsystems/ay atpkg-ay-8256 {TARBALL}"))
    );
    assert!(
        !calls.iter().any(|c| c.contains("alabsystems")
            || c.starts_with("create")
            || c.starts_with("upload")),
        "{calls:?}"
    );
}

#[test]
fn an_unknown_head_cannot_accept_corrupt_or_truncated_public_payloads() {
    for truncate in [false, true] {
        let (mut rig, q) = setup("mirror-head-corrupt", index());
        ok(&rig, &q, false);
        let mut rel = rig.host.release_of(&public(), "atpkg-ay-8256").unwrap();
        let bytes = rel.assets.get_mut(TARBALL).unwrap();
        assert!(!bytes.is_empty());
        if truncate {
            bytes.pop();
        } else {
            bytes[0] ^= 1;
        }
        rig.host.put(&public(), "atpkg-ay-8256", rel);
        let before = writes(&rig.host);
        let mut host = Dark::new(rig.host.clone(), Probe::Unknown("HTTP 500".into()));
        host.probe_only = true;
        rig.route(Arc::new(host));
        assert!(err(&rig, &q, false).contains("size or hash mismatch vs the signed manifest"));
        assert_eq!(writes(&rig.host), before);
    }
}

#[test]
fn a_definitively_absent_head_does_not_fetch_the_payload() {
    let (mut rig, q) = setup("mirror-head-absent", index());
    ok(&rig, &q, false);
    let before = rig.host.calls().len();
    rig.route(Arc::new(Dark::new(rig.host.clone(), Probe::Absent)));
    let (report, _) = ok(&rig, &q, true);
    assert_eq!(report.planned.len(), 1);
    assert!(
        !rig.host.calls()[before..]
            .iter()
            .any(|c| c.starts_with("fetch") && c.ends_with(TARBALL))
    );
}

#[test]
fn an_unknown_head_does_not_make_a_private_or_draft_release_public() {
    for private in [false, true] {
        let (mut rig, q) = setup("mirror-head-private", index());
        ok(&rig, &q, false);
        if private {
            rig.host.set_private(&public(), true);
        } else {
            let mut rel = rig.host.release_of(&public(), "atpkg-ay-8256").unwrap();
            rel.draft = true;
            rig.host.put(&public(), "atpkg-ay-8256", rel);
        }
        let before = writes(&rig.host);
        let mut host = Dark::new(rig.host.clone(), Probe::Unknown("HTTP 500".into()));
        host.probe_only = true;
        rig.route(Arc::new(host));
        assert!(err(&rig, &q, false).contains("exists but serves no pkg-ay-8256.toml"));
        assert_eq!(writes(&rig.host), before);
    }
}

#[test]
fn a_create_the_download_host_never_serves_fails_the_run() {
    let (mut rig, q) = setup("mirror-unserved", index());
    let host = rig.host.clone();
    rig.route(Arc::new(Dark::new(host, Probe::Absent)));
    let e = err(&rig, &q, false);
    assert!(
        e.contains("after the upload, read 30 times — the public repo is private"),
        "{e}"
    );
}

#[test]
fn a_just_written_asset_the_download_route_lags_on_is_read_again_until_it_answers() {
    let (rig, q) = setup("mirror-lag", index());
    rig.host
        .lag_public(&public(), "atpkg-ay-8256", "pkg-ay-8256.toml", 12);
    let (report, _) = ok(&rig, &q, false);
    assert_eq!(report.created, ["atpkg-ay-8256"]);
    let probes = rig
        .host
        .calls()
        .iter()
        .filter(|c| *c == "probe alabsystems/ay atpkg-ay-8256 pkg-ay-8256.toml")
        .count();
    assert_eq!(probes, 13, "twelve 404s, then the answer");
}

#[test]
fn a_lag_past_the_bound_still_fails_the_run_after_exactly_the_bound() {
    let (rig, q) = setup("mirror-lag-bound", index());
    rig.host
        .lag_public(&public(), "atpkg-ay-8256", "pkg-ay-8256.toml", 1_000);
    let e = err(&rig, &q, false);
    assert!(
        e.contains("answers Absent after the upload, read 30 times"),
        "{e}"
    );
    let probes = rig
        .host
        .calls()
        .iter()
        .filter(|c| *c == "probe alabsystems/ay atpkg-ay-8256 pkg-ay-8256.toml")
        .count();
    assert_eq!(probes, 30);
}

#[test]
fn an_unreachable_download_host_decides_nothing() {
    let (mut rig, q) = setup("mirror-down", index());
    let host = rig.host.clone();
    rig.route(Arc::new(Dark::new(host, Probe::Unknown("no route".into()))));
    let e = err(&rig, &q, true);
    assert!(e.contains("nothing about atpkg-ay-8256 is known"), "{e}");
    assert!(!rig.host.calls().iter().any(|c| c.contains("alabsystems")));
}

#[test]
fn independent_public_reads_overlap_and_each_pack_is_still_verified_once() {
    let rig = Rig::new("mirror-overlap");
    let pins = [("ay", 8256), ("clean", 2), ("nn", 3), ("ny", 4), ("ty", 5)];
    for (program, build) in pins {
        rig.lay_pack("alabsystems", program, build, &[MAC]);
    }
    let dir = rig.path("quad");
    rig.quad_dir(&dir, &Doc::new(7, &pins));
    let q = quad::verify_dir(&rig.ctx, &dir).expect("quad");
    let gate = Arc::new(ReadGate {
        required: 2,
        ..ReadGate::default()
    });
    let mut host = Dark::new(rig.host.clone(), Probe::Present);
    host.gate = Some(gate.clone());
    let mut rig = rig;
    rig.route(Arc::new(host));
    let (report, _) = ok(&rig, &q, true);
    let tags: Vec<_> = pins.iter().map(|(p, b)| upload::tag(p, *b)).collect();
    assert_eq!(report.skipped, tags);
    assert_eq!(*gate.entered.lock().expect("read gate"), pins.len());
    assert!(writes(&rig.host).is_empty());
    let calls = rig.host.calls();
    for (program, build) in pins {
        let tag = upload::tag(program, build);
        let repo = atpkg_publish::programs::get(program)
            .expect("known program")
            .repo;
        for suffix in [".toml", ".toml.sig"] {
            let call = format!("fetch alabsystems/{repo} {tag} pkg-{program}-{build}{suffix}");
            assert_eq!(calls.iter().filter(|c| **c == call).count(), 1, "{call}");
        }
    }
    assert!(!q.dir.join(".mirror").exists());
}

#[test]
fn a_later_unknown_preserves_the_earlier_qualified_serial_write_prefix() {
    let (mut rig, _) = setup("mirror-prefix", index());
    let dir = rig.path("quad");
    rig.quad_dir(&dir, &Doc::new(7, &[("ay", 8256), ("nn", 192)]));
    let q = quad::verify_dir(&rig.ctx, &dir).expect("quad");
    let mut host = Dark::new(rig.host.clone(), Probe::Present);
    host.failures
        .insert("atpkg-nn-192".into(), "later read is unknown".into());
    rig.route(Arc::new(host));
    let e = err(&rig, &q, false);
    assert!(
        e.contains("atpkg-nn-192") && e.contains("later read is unknown"),
        "{e}"
    );
    assert!(rig.host.release_of(&public(), "atpkg-ay-8256").is_some());
    assert!(
        rig.host
            .release_of(&Slug::new("alabsystems", "nn"), "atpkg-nn-192")
            .is_none()
    );
    assert!(!q.dir.join(".mirror").exists());
}

#[test]
fn independent_unknowns_report_the_first_pin_and_all_readers_finish_before_cleanup() {
    let (mut rig, _) = setup("mirror-unknown-order", index());
    let dir = rig.path("quad");
    rig.quad_dir(&dir, &Doc::new(7, &[("ay", 8256), ("nn", 192)]));
    let q = quad::verify_dir(&rig.ctx, &dir).expect("quad");
    let gate = Arc::new(ReadGate {
        required: 2,
        ..ReadGate::default()
    });
    let mut host = Dark::new(rig.host.clone(), Probe::Present);
    host.gate = Some(gate.clone());
    host.failures
        .insert("atpkg-ay-8256".into(), "first pin is unknown".into());
    host.failures
        .insert("atpkg-nn-192".into(), "later pin is unknown".into());
    rig.route(Arc::new(host));
    let e = err(&rig, &q, true);
    assert!(
        e.contains("first pin is unknown") && !e.contains("later pin is unknown"),
        "{e}"
    );
    assert_eq!(*gate.entered.lock().expect("read gate"), 2);
    assert!(writes(&rig.host).is_empty());
    assert!(!q.dir.join(".mirror").exists());
}

#[test]
fn a_public_manifest_without_its_signature_keeps_its_original_byte_custody() {
    for differs in [false, true] {
        let (rig, q) = setup("mirror-partial-pair", index());
        let pack = rig.pack("ay", 8256, &[MAC]);
        let mf = "pkg-ay-8256.toml";
        let mut bytes = pack
            .files
            .iter()
            .find(|(name, _)| name == mf)
            .expect("manifest")
            .1
            .clone();
        if differs {
            bytes.extend_from_slice(b"# foreign public bytes\n");
        }
        rig.host
            .put_asset(&public(), "atpkg-ay-8256", mf, bytes.clone());
        if differs {
            let e = err(&rig, &q, false);
            assert!(e.contains("manifest that DIFFERS from staging"), "{e}");
            assert!(writes(&rig.host).is_empty());
        } else {
            let (report, _) = ok(&rig, &q, false);
            assert_eq!(report.converged.len(), 1);
            assert!(!report.converged[0].1.contains(&mf.to_string()));
        }
        assert_eq!(
            rig.host
                .release_of(&public(), "atpkg-ay-8256")
                .expect("public")
                .assets[mf],
            bytes
        );
    }
}

#[test]
fn public_reads_keep_case_distinct_signed_programs_in_separate_directories() {
    let rig = Rig::new("mirror-case-custody");
    let pins = [("AY", 1), ("ay", 1)];
    for (program, build) in pins {
        rig.lay_pack("alabsystems", program, build, &[MAC]);
    }
    let dir = rig.path("quad");
    rig.quad_dir(&dir, &Doc::new(7, &pins));
    let q = quad::verify_dir(&rig.ctx, &dir).expect("quad");
    let (report, _) = ok(&rig, &q, true);
    assert_eq!(report.skipped, ["atpkg-AY-1", "atpkg-ay-1"]);
    assert!(writes(&rig.host).is_empty());
}

#[test]
fn every_build_any_target_resolves_is_mirrored_once() {
    let doc = index()
        .overlay(LINUX, &[("ay", 9001)])
        .overlay(LINUX_ARM, &[("ay", 9002)]);
    let (rig, q) = setup("mirror-union", doc);
    rig.lay_pack("alabsystems", "ay", 9001, &[LINUX]);
    rig.lay_pack("alabsystems", "ay", 9002, &[LINUX_ARM]);
    let (report, _) = ok(&rig, &q, true);
    let tags: Vec<&str> = report.planned.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(tags, ["atpkg-ay-8256", "atpkg-ay-9001", "atpkg-ay-9002"]);
    assert_eq!(
        report.planned[1].1,
        [
            "pkg-ay-9001.toml",
            "pkg-ay-9001.toml.sig",
            "ay-9001-x86_64-unknown-linux-gnu.tar.zst"
        ]
    );

    let (rig, q) = setup("mirror-once", index().overlay(LINUX, &[("ay", 8256)]));
    let (report, _) = ok(&rig, &q, true);
    assert_eq!(report.planned.len(), 1);

    let (rig, q) = setup("mirror-missing", index().overlay(LINUX, &[("ay", 9003)]));
    let e = err(&rig, &q, true);
    assert!(
        e.contains("alabsystems/ay@atpkg-ay-9003 could not be downloaded"),
        "{e}"
    );
}

#[test]
fn a_quad_missing_its_roster_moves_no_pack() {
    let (rig, _) = setup("mirror-quad", index());
    let dir = rig.path("quad");
    let flag_err = mirror::run(&rig.ctx, &["-n".into(), dir.display().to_string()])
        .expect_err("single-dash flag is not a quad dir");
    assert_eq!(flag_err.exit_code(), 2, "{flag_err}");
    assert!(
        flag_err
            .to_string()
            .contains("mirror: unknown argument `-n`"),
        "{flag_err}"
    );

    // The `gh` arm's token check, said outright; under agh the channel writer is asked
    // instead (`publishing_asks_the_channel_writer_under_agh`).
    let gh = rig.ctx.clone().with_host_choice(HostChoice::Gh);
    std::fs::create_dir_all(gh.tokens.channel.parent().expect("secrets dir"))
        .expect("mkdir secrets");
    std::fs::write(&gh.tokens.channel, "  \n").expect("whitespace token");
    let blank_err = mirror::run(&gh, &[dir.display().to_string()])
        .expect_err("whitespace-only channel token refused before mirroring");
    assert!(
        blank_err
            .to_string()
            .contains("publishing needs the public owner's token at ~/.secrets/"),
        "{blank_err}"
    );
    assert!(rig.host.calls().is_empty());

    std::fs::remove_file(dir.join("aterm-machines.toml")).expect("rm");
    let e = mirror::run(&rig.ctx, &[dir.display().to_string(), "--dry-run".into()])
        .expect_err("refused");
    assert!(e.to_string().contains("missing aterm-machines.toml"), "{e}");
    assert!(rig.host.calls().is_empty());
}

#[test]
fn publishing_asks_the_channel_writer_under_agh() {
    let (mut rig, _) = setup("mirror-writer", index());
    let dir = rig.path("quad").display().to_string();
    rig.host.fail_next(FakeOp::Preflight, FakeFail::Refuse);
    let e = mirror::run(&rig.ctx, std::slice::from_ref(&dir)).expect_err("no channel writer");
    assert!(matches!(e, Error::Refused(_)), "{e}");
    let slug = rig.ctx.index_slug();
    let why = e.to_string();
    assert!(why.contains(&format!("writer on {slug}")), "{why}");
    assert!(why.contains("refused by the test"), "{why}");
    assert_eq!(rig.host.calls(), vec![format!("preflight {slug}")]);

    mirror::run(&rig.ctx, &[dir.clone(), "--dry-run".into()]).expect("the dry run plans");
    let calls = rig.host.calls();
    assert_eq!(
        calls.iter().filter(|c| c.starts_with("preflight")).count(),
        1,
        "{calls:?}"
    );
    assert!(writes(&rig.host).is_empty(), "{calls:?}");

    // Each pinned program's public repository is asked too, after the index repository's and
    // before any pack is read or written: under agh push and a grant are per repository.
    let ay = rig.ctx.public_slug("ay");
    rig.ctx.hosts = Hosts::single(Arc::new(RefusesOne {
        inner: rig.host.clone(),
        refused: ay.clone(),
    }));
    let before = rig.host.calls().len();
    let e = mirror::run(&rig.ctx, &[dir]).expect_err("no writer on the program repository");
    assert!(matches!(e, Error::Refused(_)), "{e}");
    let why = e.to_string();
    assert!(why.contains(&format!("writer on {ay}")), "{why}");
    assert!(why.contains("this writer is refused"), "{why}");
    assert_eq!(rig.host.calls()[before..], [format!("preflight {slug}")]);
}

/// A `FakeHost` whose `write_preflight` refuses one slug and delegates every other.
struct RefusesOne {
    inner: Arc<FakeHost>,
    refused: Slug,
}

impl ReleaseHost for RefusesOne {
    fn releases(&self, slug: &Slug) -> std::result::Result<Vec<Release>, HostError> {
        self.inner.releases(slug)
    }
    fn release(&self, slug: &Slug, tag: &str) -> std::result::Result<Release, HostError> {
        self.inner.release(slug, tag)
    }
    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> std::result::Result<(), HostError> {
        self.inner.download(slug, tag, assets, dir)
    }
    fn create(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> std::result::Result<(), HostError> {
        self.inner.create(slug, tag, title, notes, files)
    }
    fn create_draft(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> std::result::Result<(), HostError> {
        self.inner.create_draft(slug, tag, title, notes, files)
    }
    fn upload(
        &self,
        slug: &Slug,
        tag: &str,
        files: &[PathBuf],
        replace: Replace,
    ) -> std::result::Result<(), HostError> {
        self.inner.upload(slug, tag, files, replace)
    }
    fn publish_draft(&self, slug: &Slug, tag: &str) -> std::result::Result<(), HostError> {
        self.inner.publish_draft(slug, tag)
    }
    fn write_preflight(&self, slug: &Slug) -> std::result::Result<(), HostError> {
        if *slug == self.refused {
            Err(HostError::Refused("this writer is refused".into()))
        } else {
            self.inner.write_preflight(slug)
        }
    }
    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
        self.inner.probe_public(slug, tag, asset)
    }
    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Probe {
        self.inner.fetch_public(slug, tag, asset, dest, cap)
    }
    fn clone_repo(&self, slug: &Slug, dest: &Path) -> std::result::Result<(), HostError> {
        self.inner.clone_repo(slug, dest)
    }
    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> std::result::Result<(), HostError> {
        self.inner.fetch_repo(slug, dir)
    }
}
