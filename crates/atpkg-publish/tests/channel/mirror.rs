// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Channel mirror tests over a fake release host.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use atpkg_publish::channel::mirror::{self, MirrorOptions, MirrorReport};
use atpkg_publish::error::Result;
use atpkg_publish::host::fake::{FakeHost, FakeRelease};
use atpkg_publish::host::{HostError, Probe, Release, ReleaseHost, Replace, Slug};
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
        [TARBALL, "pkg-ay-8256.toml", "pkg-ay-8256.toml.sig"]
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
    assert_eq!(uploads, ["upload alabsystems/ay atpkg-ay-8256 Never"]);
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
        match &self.probe {
            Probe::Unknown(_) => self.probe.clone(),
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
fn a_create_the_download_host_never_serves_fails_the_run() {
    let (mut rig, q) = setup("mirror-unserved", index());
    let host = rig.host.clone();
    rig.route(Arc::new(Dark {
        inner: host,
        probe: Probe::Absent,
    }));
    let e = err(&rig, &q, false);
    assert!(
        e.contains("after the upload — the public repo is private"),
        "{e}"
    );
}

#[test]
fn an_unreachable_download_host_decides_nothing() {
    let (mut rig, q) = setup("mirror-down", index());
    let host = rig.host.clone();
    rig.route(Arc::new(Dark {
        inner: host,
        probe: Probe::Unknown("no route".into()),
    }));
    let e = err(&rig, &q, true);
    assert!(e.contains("nothing about atpkg-ay-8256 is known"), "{e}");
    assert!(!rig.host.calls().iter().any(|c| c.contains("alabsystems")));
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

    std::fs::write(&rig.ctx.tokens.channel, "  \n").expect("whitespace token");
    let blank_err = mirror::run(&rig.ctx, &[dir.display().to_string()])
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
