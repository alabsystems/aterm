// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The index PUBLISH over a fake channel: the number is the head's + 1, the baseline must
//! be published, the packs are made public BEFORE the index, and the upload is a
//! compare-and-swap that never clobbers another publisher's `atpkg-index-N` (create /
//! converge / draft / foreign / half-created / race / unreadable state / unserved), and a
//! moved pin never silently drops an Apple target.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use atpkg_publish::api::{Baseline, Delta, IndexRequest};
use atpkg_publish::channel::Log;
use atpkg_publish::channel::index::{Outcome, parse_delta, publish_with};
use atpkg_publish::error::{Error, Result};
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::host::{HostError, Probe, Release, ReleaseHost, Replace, Slug};
use atpkg_publish::quad::{QUAD_FILES, index_tag};
use atpkg_publish::testkit::FAR_FUTURE;

use super::{Doc, Rig};

const MAC: &str = "aarch64-apple-darwin";
const MAC_X86: &str = "x86_64-apple-darwin";
const LINUX: &str = "x86_64-unknown-linux-gnu";

/// The fake, with one failure a publisher meets in the field.
struct Faulty {
    inner: Arc<FakeHost>,
    /// `release(tag)` answers this instead (a racing read, an unreadable state).
    release_answer: Option<(String, HostError)>,
    /// The download host never serves this tag.
    unserved: Option<String>,
}

impl ReleaseHost for Faulty {
    fn releases(&self, slug: &Slug) -> std::result::Result<Vec<Release>, HostError> {
        self.inner.releases(slug)
    }
    fn release(&self, slug: &Slug, tag: &str) -> std::result::Result<Release, HostError> {
        if let Some((t, e)) = &self.release_answer
            && t == tag
        {
            return Err(e.clone());
        }
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
    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
        if self.unserved.as_deref() == Some(tag) {
            return Probe::Absent;
        }
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

/// A channel at index 21 pinning ay 5, with the pack public.
fn live(label: &str) -> Rig {
    let rig = Rig::new(label);
    rig.publish_head(&Doc::new(21, &[("ay", 5)]));
    rig.lay_pack("alabsystems", "ay", 5, &[MAC, MAC_X86, LINUX]);
    rig
}

fn request(rig: &Rig, out: &str, upload: bool, delta: Delta) -> IndexRequest {
    IndexRequest {
        baseline: Baseline::ChannelHead,
        delta,
        out: rig.path(out),
        upload,
        pack_dir: None,
    }
}

/// A re-run by the publisher still holding baseline 21 (the shell's `BASELINE=`): the
/// signed quad of the head, on disk, byte-identical to the one the channel serves.
fn rerun(rig: &Rig, out: &str) -> IndexRequest {
    let base = rig.path("held-21");
    rig.quad_dir(&base, &Doc::new(21, &[("ay", 5)]));
    IndexRequest {
        baseline: Baseline::Dir(base),
        delta: Delta::default(),
        out: rig.path(out),
        upload: true,
        pack_dir: None,
    }
}

fn publish(rig: &Rig, req: &IndexRequest) -> (Result<Outcome>, Log) {
    let mut log = Rig::log();
    let r = publish_with(&rig.ctx, req, &mut log);
    (r, log)
}

fn refused(rig: &Rig, req: &IndexRequest) -> (Error, Log) {
    let (r, log) = publish(rig, req);
    match r {
        Ok(o) => panic!(
            "published, but must be refused: {:?}\n{}",
            o.published,
            log.text()
        ),
        Err(e) => (e, log),
    }
}

fn writes(host: &FakeHost) -> Vec<String> {
    host.calls()
        .into_iter()
        .filter(|c| {
            c.starts_with("create") || c.starts_with("upload") || c.starts_with("publish-draft")
        })
        .collect()
}

#[test]
fn a_dry_run_over_a_quad_on_disk_makes_no_host_call_and_signs_the_next_number() {
    let rig = Rig::new("idx-dry");
    let base = rig.path("base");
    rig.quad_dir(&base, &Doc::new(21, &[("ay", 5)]));
    let req = IndexRequest {
        baseline: Baseline::Dir(base),
        delta: Delta::default(),
        out: rig.path("out"),
        upload: false,
        pack_dir: None,
    };
    let (r, log) = publish(&rig, &req);
    let o = r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    assert_eq!(o.published.index_build, 22);
    assert!(!o.published.uploaded);
    assert!(rig.host.calls().is_empty(), "{:?}", rig.host.calls());
    for f in QUAD_FILES {
        assert!(rig.path("out").join(f).is_file(), "{f}");
    }

    // Over the channel head, a dry run only READS the index repo.
    let (r, _) = publish(
        rig_with_head(&rig),
        &request(&rig, "out2", false, Delta::default()),
    );
    r.expect("dry run over the head");
    assert!(writes(&rig.host).is_empty());
    let slug = rig.index_slug().to_string();
    assert!(
        rig.host.calls().iter().all(|c| c.contains(&slug)),
        "only the index repo is read: {:?}",
        rig.host.calls()
    );
}

fn rig_with_head(rig: &Rig) -> &Rig {
    rig.publish_head(&Doc::new(21, &[("ay", 5)]));
    rig
}

#[test]
fn upload_needs_the_channel_token_before_anything_is_signed() {
    let rig = live("idx-token");
    std::fs::remove_file(&rig.ctx.tokens.channel).expect("rm token");
    let (e, _) = refused(&rig, &request(&rig, "out", true, Delta::default()));
    assert!(
        e.to_string().contains("needs the public owner's token"),
        "{e}"
    );
    assert!(rig.host.calls().is_empty(), "{:?}", rig.host.calls());
    assert!(!rig.path("out").join("index.toml").exists());
}

#[test]
fn the_baseline_must_be_published_on_the_download_host() {
    // A quad on disk whose release is a DRAFT, absent, or behind a private repo.
    let rig = Rig::new("idx-base");
    let doc = Doc::new(21, &[("ay", 5)]);
    let dir = rig.path("base");
    rig.quad_dir(&dir, &doc);
    rig.lay_pack("alabsystems", "ay", 5, &[MAC]);
    let req = IndexRequest {
        baseline: Baseline::Dir(dir),
        delta: Delta::default(),
        out: rig.path("out"),
        upload: true,
        pack_dir: None,
    };
    let (e, _) = refused(&rig, &req);
    assert!(
        e.to_string()
            .contains("the baseline atpkg-index-21 is not published on alabsystems/aterm"),
        "{e}"
    );
    rig.publish_head(&doc);
    rig.host.set_draft(&rig.index_slug(), &index_tag(21), true);
    let (e, _) = refused(&rig, &req);
    assert!(e.to_string().contains("is not published"), "{e}");
    rig.host.set_draft(&rig.index_slug(), &index_tag(21), false);
    rig.host.set_private(&rig.index_slug(), true);
    let (e, _) = refused(&rig, &req);
    assert!(e.to_string().contains("is not published"), "{e}");
    assert!(writes(&rig.host).is_empty(), "{:?}", writes(&rig.host));
    assert!(rig.index_release(22).is_none());
    // The control: published, it goes through.
    rig.host.set_private(&rig.index_slug(), false);
    let (r, log) = publish(&rig, &req);
    r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
}

#[test]
fn a_fresh_publish_mirrors_first_then_creates_one_prerelease_on_the_public_repo() {
    let rig = live("idx-fresh");
    let (r, log) = publish(&rig, &request(&rig, "out", true, Delta::default()));
    let o = r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    assert!(o.published.uploaded);
    let calls = rig.host.calls();
    let mirror_at = calls
        .iter()
        .position(|c| c.contains("alabsystems/ay "))
        .expect("the mirror reads the pack");
    let create_at = calls
        .iter()
        .position(|c| c.starts_with("create alabsystems/aterm atpkg-index-22"))
        .expect("one create");
    assert!(mirror_at < create_at, "packs first: {calls:?}");
    assert_eq!(
        calls.iter().filter(|c| c.starts_with("create")).count(),
        1,
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains("alabsystems/aterm")),
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c.contains("Clobber")), "{calls:?}");
    let rel = rig.index_release(22).expect("created");
    assert!(rel.prerelease && !rel.draft);
    assert_eq!(
        rel.assets.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "aterm-machines.toml",
            "aterm-machines.toml.sig",
            "index.toml",
            "index.toml.sig"
        ]
    );
    assert!(
        calls
            .iter()
            .any(|c| c == "probe alabsystems/aterm atpkg-index-22 index.toml"),
        "confirmed on the download host: {calls:?}"
    );
}

#[test]
fn packs_that_are_not_public_publish_no_index() {
    let rig = Rig::new("idx-mirror-fails");
    rig.publish_head(&Doc::new(21, &[("ay", 5)]));
    let (e, _) = refused(&rig, &request(&rig, "out", true, Delta::default()));
    assert!(e.to_string().contains("not all public"), "{e}");
    assert!(writes(&rig.host).is_empty(), "{:?}", writes(&rig.host));
    assert!(rig.index_release(22).is_none());
}

#[test]
fn an_index_the_download_host_does_not_serve_after_its_create_fails_the_run() {
    let mut rig = live("idx-unserved");
    let host = rig.host.clone();
    rig.route(Arc::new(Faulty {
        inner: host,
        release_answer: None,
        unserved: Some(index_tag(22)),
    }));
    let (e, _) = refused(&rig, &request(&rig, "out", true, Delta::default()));
    assert!(e.to_string().contains("is the repo public?"), "{e}");
}

/// Publish once (the fixture's own bytes), then lay `atpkg-index-22` as `shape` makes it.
fn own_then(label: &str, shape: impl Fn(&Rig, &Path)) -> Rig {
    let rig = live(label);
    let (r, log) = publish(&rig, &request(&rig, "own", true, Delta::default()));
    r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    let own = rig.path("own");
    rig.host.put(
        &rig.index_slug(),
        &index_tag(22),
        atpkg_publish::host::fake::FakeRelease::default(),
    );
    shape(&rig, &own);
    rig
}

fn lay(rig: &Rig, name: &str, bytes: Vec<u8>) {
    rig.host
        .put_asset(&rig.index_slug(), &index_tag(22), name, bytes);
}

fn own_file(own: &Path, name: &str) -> Vec<u8> {
    std::fs::read(own.join(name)).expect("own quad file")
}

#[test]
fn a_rerun_over_its_own_bytes_converges_without_clobbering() {
    let rig = own_then("idx-converge", |rig, own| {
        for f in &QUAD_FILES[..2] {
            lay(rig, f, own_file(own, f));
        }
    });
    let before = rig.host.calls().len();
    let (r, log) = publish(&rig, &rerun(&rig, "again"));
    r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    let calls = &rig.host.calls()[before..];
    let uploads: Vec<&String> = calls.iter().filter(|c| c.starts_with("upload")).collect();
    assert_eq!(
        uploads,
        ["upload alabsystems/aterm atpkg-index-22 Never"],
        "{calls:?}"
    );
    assert_eq!(rig.index_release(22).expect("there").assets.len(), 4);
    let mut left: Vec<String> = std::fs::read_dir(rig.path("again"))
        .expect("out")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            ".atpkg-publish.lock",
            "aterm-machines.toml",
            "aterm-machines.toml.sig",
            "index.toml",
            "index.toml.sig"
        ],
        "the output holds the quad, and no scratch of the publish"
    );
}

#[test]
fn another_publishers_index_is_refused_and_never_clobbered() {
    let rig = own_then("idx-foreign", |rig, own| {
        lay(rig, "index.toml", b"foreign bytes\n".to_vec());
        lay(rig, "index.toml.sig", own_file(own, "index.toml.sig"));
    });
    let (e, _) = refused(&rig, &rerun(&rig, "again"));
    assert!(
        e.to_string().contains("another publisher took index 22"),
        "{e}"
    );
    assert_eq!(
        rig.host
            .asset(&rig.index_slug(), &index_tag(22), "index.toml")
            .as_deref(),
        Some(&b"foreign bytes\n"[..])
    );

    let rig = own_then("idx-foreign-sig", |rig, own| {
        lay(rig, "index.toml", own_file(own, "index.toml"));
        lay(rig, "index.toml.sig", b"another machine's sig".to_vec());
    });
    let (e, _) = refused(&rig, &rerun(&rig, "again"));
    assert!(e.to_string().contains("(index.toml.sig)"), "{e}");
}

#[test]
fn a_half_created_release_is_refused() {
    let rig = own_then("idx-half", |rig, own| {
        lay(
            rig,
            "aterm-machines.toml",
            own_file(own, "aterm-machines.toml"),
        );
    });
    let before = writes(&rig.host).len();
    let (e, _) = refused(&rig, &rerun(&rig, "again"));
    assert!(e.to_string().contains("without an index.toml"), "{e}");
    assert_eq!(writes(&rig.host).len(), before);
}

#[test]
fn a_create_that_loses_a_race_never_falls_back_to_an_upload() {
    let mut rig = own_then("idx-race", |rig, own| {
        lay(rig, "index.toml", own_file(own, "index.toml"));
    });
    let host = rig.host.clone();
    // The read saw no release; the other publisher's create landed before ours.
    rig.route(Arc::new(Faulty {
        inner: host,
        release_answer: Some((index_tag(22), HostError::NotFound)),
        unserved: None,
    }));
    let before = writes(&rig.host).len();
    let (e, _) = refused(&rig, &rerun(&rig, "again"));
    assert!(e.to_string().contains("never clobbered"), "{e}");
    let after = writes(&rig.host);
    assert_eq!(
        after.len(),
        before + 1,
        "one create, no fallback: {after:?}"
    );
    assert!(after.last().expect("create").starts_with("create"));
}

#[test]
fn an_orphaned_draft_of_its_own_bytes_is_converged_and_published() {
    let rig = own_then("idx-draft", |rig, own| {
        for f in &QUAD_FILES[..2] {
            lay(rig, f, own_file(own, f));
        }
        rig.host.set_draft(&rig.index_slug(), &index_tag(22), true);
    });
    let (r, log) = publish(&rig, &rerun(&rig, "again"));
    r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    assert!(
        rig.host
            .calls()
            .contains(&"publish-draft alabsystems/aterm atpkg-index-22".to_string())
    );
    assert!(!rig.index_release(22).expect("there").draft);
}

#[test]
fn a_foreign_draft_is_refused_named_and_never_published() {
    let rig = own_then("idx-draft-foreign", |rig, _| {
        lay(rig, "index.toml", b"foreign bytes\n".to_vec());
        rig.host.set_draft(&rig.index_slug(), &index_tag(22), true);
    });
    let (e, _) = refused(&rig, &rerun(&rig, "again"));
    assert!(e.to_string().contains("orphaned DRAFT"), "{e}");
    assert!(
        !rig.host
            .calls()
            .iter()
            .any(|c| c.starts_with("publish-draft"))
    );
    assert!(rig.index_release(22).expect("there").draft);
}

#[test]
fn a_release_whose_state_cannot_be_read_is_refused() {
    let mut rig = own_then("idx-unreadable", |rig, own| {
        lay(rig, "index.toml", own_file(own, "index.toml"));
    });
    let host = rig.host.clone();
    rig.route(Arc::new(Faulty {
        inner: host,
        release_answer: Some((index_tag(22), HostError::Unreachable("HTTP 500".into()))),
        unserved: None,
    }));
    let before = writes(&rig.host).len();
    let (e, _) = refused(&rig, &rerun(&rig, "again"));
    assert!(
        e.to_string()
            .contains("could not read whether the existing atpkg-index-22"),
        "{e}"
    );
    assert_eq!(writes(&rig.host).len(), before);
}

/// A moved pin may not silently drop a darwin target the old pin served
/// (DESIGN-intel-just-works §3.2): refused before anything is made public, unless named
/// with a reason; a manifest nobody can read is a refusal, never a pass.
#[test]
fn a_moved_pin_may_not_silently_drop_a_darwin_target() {
    let rig = live("idx-triple");
    rig.lay_pack("alabsystems", "ay", 6, &[MAC, LINUX]);
    let move6 = || parse_delta(&["--pin".to_string(), "ay=6".to_string()]).expect("delta");
    let (e, _) = refused(&rig, &request(&rig, "out", true, move6()));
    let msg = e.to_string();
    assert!(
        msg.contains("REFUSING — ay 5->6 drops x86_64-apple-darwin"),
        "{msg}"
    );
    assert!(writes(&rig.host).is_empty(), "{:?}", writes(&rig.host));

    let allowed = parse_delta(
        &[
            "--pin",
            "ay=6",
            "--allow-triple-drop",
            "ay=x86_64-apple-darwin",
            "--reason",
            "the rustc group is Apple Silicon only",
        ]
        .map(String::from),
    )
    .expect("delta");
    let (r, log) = publish(&rig, &request(&rig, "allowed", true, allowed));
    r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    assert!(!rig.path("allowed/.triple-gate").exists());

    // The row kept, the new manifest on the PUBLIC owner: passes without a word.
    let rig = live("idx-triple-kept");
    rig.lay_pack("alabsystems", "ay", 6, &[MAC, MAC_X86]);
    let (r, log) = publish(&rig, &request(&rig, "out", true, move6()));
    r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));

    // The old manifest unreadable: refused.
    let rig = Rig::new("idx-triple-unread");
    rig.publish_head(&Doc::new(21, &[("ay", 5)]));
    rig.lay_pack("alabsystems", "ay", 6, &[MAC, MAC_X86]);
    let (e, _) = refused(&rig, &request(&rig, "out", true, move6()));
    assert!(
        e.to_string().contains("cannot read ay's head manifest"),
        "{e}"
    );
}

#[test]
fn a_first_publish_names_its_number_and_never_lands_on_a_live_channel() {
    let rig = Rig::new("idx-first");
    // The roster comes from the machine's installed pair.
    std::fs::create_dir_all(rig.ctx.identity.roster.parent().expect("dir")).expect("mkdir");
    std::fs::write(&rig.ctx.identity.roster, &rig.keys.roster_bytes).expect("roster");
    std::fs::write(
        format!("{}.sig", rig.ctx.identity.roster.display()),
        &rig.keys.roster_sig,
    )
    .expect("sig");
    let first = |upload| IndexRequest {
        baseline: Baseline::First {
            channel: "stable".into(),
            index_build: 1,
        },
        delta: parse_delta(
            &["--pin", "ay=5", "--valid-until", "2098-01-01T00:00:00Z"].map(String::from),
        )
        .expect("delta"),
        out: rig.path("first"),
        upload,
        pack_dir: None,
    };
    let (r, log) = publish(&rig, &first(false));
    let o = r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    assert_eq!(o.published.index_build, 1);
    assert!(rig.host.calls().is_empty());
    rig.publish_head(&Doc::new(4, &[("ay", 5)]));
    let (e, _) = refused(&rig, &first(true));
    assert!(e.to_string().contains("atpkg-index-4 is published"), "{e}");
}

/// What a publish does not name is carried from the verified head into the signed bytes:
/// the yanks, the floor and every overlay — nothing re-passed, nothing dropped.
#[test]
fn the_heads_yanks_floor_and_overlays_reach_the_signed_index() {
    let rig = Rig::new("idx-carry");
    let base = rig.path("base");
    rig.quad_dir(
        &base,
        &Doc::new(34, &[("ay", 8256), ("codex", 2_026_091_001)])
            .yanked(&["codex@2026090801"])
            .min_build(1)
            .overlay(LINUX, &[("ay", 9001)]),
    );
    let req = IndexRequest {
        baseline: Baseline::Dir(base),
        delta: parse_delta(&["--pin", "ay=8300"].map(String::from)).expect("delta"),
        out: rig.path("out"),
        upload: false,
        pack_dir: None,
    };
    let (r, log) = publish(&rig, &req);
    r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    let q = atpkg_publish::quad::verify_dir(&rig.ctx, &rig.path("out")).expect("verifies");
    let c = &q.index.channels[0];
    assert_eq!(q.index.index_build, 35);
    assert_eq!(c.yanked, ["codex@2026090801"]);
    assert_eq!(c.min_build, 1);
    assert_eq!(c.pin.get("ay"), Some(&8300));
    assert_eq!(c.pin.get("codex"), Some(&2_026_091_001));
    assert_eq!(c.pin_by_target[LINUX].get("ay"), Some(&9001));
}

/// A vendor-direct program's yank binds its client whether or not the index pins it: one
/// for a program no pin names is signed, and the client's own vendor policy reads it back
/// in both version spellings.
#[test]
fn a_vendor_yank_with_no_pin_behind_it_reaches_the_clients_policy() {
    let rig = Rig::new("idx-vendor-yank");
    let base = rig.path("base");
    rig.quad_dir(&base, &Doc::new(34, &[("ay", 8256)]));
    let req = IndexRequest {
        baseline: Baseline::Dir(base),
        delta: parse_delta(
            &["--yank", "agy@1.4.0", "--yank", "muse@1.4.9-R6297.1"].map(String::from),
        )
        .expect("delta"),
        out: rig.path("out"),
        upload: false,
        pack_dir: None,
    };
    let (r, log) = publish(&rig, &req);
    r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    let q = atpkg_publish::quad::verify_dir(&rig.ctx, &rig.path("out")).expect("verifies");
    let c = &q.index.channels[0];
    assert!(!c.pin.contains_key("agy") && !c.pin.contains_key("muse"));
    let policy = atpkg::vendor_direct::policy::Policy::from_index(&q.index);
    let v = |s| atpkg::vendor_direct::Version::parse(s).expect("a version");
    assert!(policy.is_yanked("agy", v("1.4.0")));
    assert!(policy.is_yanked("muse", v("1.4.9-R6297.1")));
}

/// A LAPSED head is the channel every client refuses, and re-signing it is the recovery:
/// it is a baseline like any other (from the channel or on disk), and only CARRYING its
/// horizon is refused — naming the flag that re-signs it.
#[test]
fn a_lapsed_head_is_re_signed_with_a_new_horizon_and_never_carried() {
    const LAPSED: &str = "2026-01-01T00:00:00Z";
    let rig = Rig::new("idx-lapsed");
    let mut doc = Doc::new(21, &[("ay", 5)]);
    doc.valid_until = LAPSED.to_string();
    rig.publish_head(&doc);
    rig.lay_pack("alabsystems", "ay", 5, &[MAC, MAC_X86, LINUX]);

    let (e, _) = refused(&rig, &request(&rig, "carried", true, Delta::default()));
    let e = e.to_string();
    assert!(
        e.contains(&format!("valid_until {LAPSED} has already LAPSED"))
            && e.contains("re-sign it with --valid-until"),
        "{e}"
    );
    assert!(!rig.path("carried").join("index.toml").exists());
    assert!(writes(&rig.host).is_empty(), "{:?}", writes(&rig.host));

    let fresh = || parse_delta(&["--valid-until", FAR_FUTURE].map(String::from)).expect("delta");
    let (r, log) = publish(&rig, &request(&rig, "resigned", true, fresh()));
    let o = r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    assert_eq!(o.published.index_build, 22);
    assert!(o.published.uploaded);
    assert!(rig.index_release(22).is_some());
    let q = atpkg_publish::quad::verify_dir(&rig.ctx, &rig.path("resigned"))
        .expect("the re-signed index is fresh: the client accepts it");
    assert_eq!(q.index.valid_until, FAR_FUTURE);
    assert_eq!(q.index.channels[0].pin.get("ay"), Some(&5));

    // A lapsed quad on disk is a baseline too.
    let held = rig.path("held-21");
    rig.quad_dir(&held, &doc);
    let req = IndexRequest {
        baseline: Baseline::Dir(held),
        delta: fresh(),
        out: rig.path("from-disk"),
        upload: false,
        pack_dir: None,
    };
    let (r, log) = publish(&rig, &req);
    assert_eq!(
        r.unwrap_or_else(|e| panic!("{e}\n{}", log.text()))
            .published
            .index_build,
        22
    );
}
