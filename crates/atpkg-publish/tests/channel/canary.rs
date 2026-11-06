// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native installation with injected execution, and signed-byte publication negative controls.

use std::collections::BTreeMap;
use std::path::Path;

use atpkg_publish::canary::{self, ConsumerReport, Installed, Runner};
use atpkg_publish::channel::mirror;
use atpkg_publish::error::Result;
use atpkg_publish::quad;
use atpkg_publish::triple::Triple;

use super::{Doc, Rig};

struct Runs {
    fail: bool,
}
impl Runner for Runs {
    fn compiler(&self, bin: &Path, rustup_home: &Path) -> Result<String> {
        assert!(rustup_home.is_dir());
        assert_eq!(
            std::fs::read(bin.join("trustc")).unwrap(),
            b"compiler fixture"
        );
        if self.fail {
            return Err(atpkg_publish::error::Error::refused("compiler did not run"));
        }
        Ok("a".repeat(40))
    }
    fn consumer(
        &self,
        name: &str,
        _: &Path,
        bin: &Path,
        _: &Path,
        _: &Installed,
    ) -> ConsumerReport {
        assert!(bin.join("trustc").is_file());
        ConsumerReport {
            name: name.into(),
            commit: "b".repeat(40),
            locked_build: Some(7),
            verdict: "red".into(),
            detail: "new lint".into(),
        }
    }
}

fn setup() -> (Rig, quad::VerifiedQuad, std::path::PathBuf) {
    let rig = Rig::new("canary");
    let qdir = rig.path("quad");
    rig.quad_dir(&qdir, &Doc::new(7, &[("trust", 7)]));
    let q = quad::verify_dir(&rig.ctx, &qdir).unwrap();
    let root = rig.path("tree");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/trustc"), b"compiler fixture").unwrap();
    let source = rig.path("source");
    std::fs::create_dir(&source).unwrap();
    let triple = Triple::host().unwrap();
    let asset = format!("trust-8-{triple}.tar.zst");
    let pack = atpkg_publish::artifact::tarwrite::write(&root, &source.join(&asset)).unwrap();
    let text = format!(
        "schema = 1\nprogram = \"trust\"\nbuild_number = 8\nversion = \"0.1.0\"\nexposes = [\"trustc\"]\n\n[[artifact]]\ntarget = \"{triple}\"\nkind = \"sysroot-bundle\"\nasset = \"{asset}\"\nsha256 = \"{}\"\ntree_root = \"{}\"\nsize = {}\nreloc = \"none\"\n",
        pack.sha256, pack.tree_root, pack.size
    );
    std::fs::write(source.join("pkg-trust-8.toml"), &text).unwrap();
    std::fs::write(
        source.join("pkg-trust-8.toml.sig"),
        rig.keys.sign(text.as_bytes()),
    )
    .unwrap();
    (rig, q, source)
}

fn install(rig: &Rig, q: &quad::VerifiedQuad, source: &Path) -> std::path::PathBuf {
    let out = rig.path("canary");
    let consumers = BTreeMap::from([("aterm".into(), rig.path("consumer"))]);
    let receipt = canary::install(
        &rig.ctx,
        q,
        source,
        8,
        &out,
        &consumers,
        &Runs { fail: false },
    )
    .unwrap();
    assert_eq!(
        receipt.consumers[0].verdict, "red",
        "a consumer red remains a signed report"
    );
    out
}

fn assets(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| e.file_name().into_string().unwrap())
        .collect()
}

#[test]
fn installed_bytes_and_consumer_reds_are_bound_to_the_mirrored_signed_set() {
    let (rig, q, source) = setup();
    let out = install(&rig, &q, &source);
    let (expected, _) =
        mirror::check_staged(&rig.ctx, &q, &out, &assets(&out), "trust", 8).unwrap();
    assert!(expected.contains(&canary::RECEIPT.to_string()));
    let receipt = canary::check_receipt(&rig.ctx, &q, &out, 8).unwrap();
    assert_eq!(receipt.installed.len(), 1);
    assert_eq!(receipt.consumers[0].locked_build, Some(7));
    let artifact = out.join(&receipt.installed[0].asset);
    std::fs::write(&artifact, b"tampered archive").unwrap();
    assert!(
        mirror::check_staged(&rig.ctx, &q, &out, &assets(&out), "trust", 8).is_err(),
        "negative control: changed published bytes must be refused"
    );
}

#[test]
fn a_different_validly_signed_manifest_cannot_reuse_the_canary() {
    let (rig, q, source) = setup();
    let out = install(&rig, &q, &source);
    let path = out.join("pkg-trust-8.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("0.1.0", "0.2.0");
    std::fs::write(&path, &text).unwrap();
    std::fs::write(
        out.join("pkg-trust-8.toml.sig"),
        rig.keys.sign(text.as_bytes()),
    )
    .unwrap();
    let e = mirror::check_staged(&rig.ctx, &q, &out, &assets(&out), "trust", 8).unwrap_err();
    assert!(e.to_string().contains("does not bind"), "{e}");
}

#[test]
fn an_unsigned_receipt_or_absent_native_execution_never_admits_a_pack() {
    let (rig, q, source) = setup();
    let failed = rig.path("failed");
    assert!(
        canary::install(
            &rig.ctx,
            &q,
            &source,
            8,
            &failed,
            &BTreeMap::new(),
            &Runs { fail: true }
        )
        .is_err()
    );
    assert!(!failed.join(canary::RECEIPT).exists());
    let out = install(&rig, &q, &source);
    std::fs::write(out.join(canary::RECEIPT), b"schema = 1\n").unwrap();
    assert!(canary::check_receipt(&rig.ctx, &q, &out, 8).is_err());
}

#[test]
fn client_install_refuses_a_signed_wrong_tree_before_execution() {
    let (rig, q, source) = setup();
    let path = source.join("pkg-trust-8.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let pkg = q
        .verify_pkg(
            text.as_bytes(),
            &std::fs::read(source.join("pkg-trust-8.toml.sig")).unwrap(),
        )
        .unwrap();
    let text = text.replace(&pkg.artifacts[0].tree_root, &"0".repeat(64));
    std::fs::write(&path, &text).unwrap();
    std::fs::write(
        source.join("pkg-trust-8.toml.sig"),
        rig.keys.sign(text.as_bytes()),
    )
    .unwrap();
    let e = canary::install(
        &rig.ctx,
        &q,
        &source,
        8,
        &rig.path("out"),
        &BTreeMap::new(),
        &Runs { fail: false },
    )
    .unwrap_err();
    assert!(e.to_string().contains("tree_root mismatch"), "{e}");
}

fn lay(rig: &Rig, dir: &Path, public: bool) {
    let slug = if public {
        rig.ctx.public_slug("trust")
    } else {
        rig.ctx.staging_slug("trust")
    };
    for name in assets(dir) {
        rig.host.put_asset(
            &slug,
            "atpkg-trust-8",
            &name,
            std::fs::read(dir.join(&name)).unwrap(),
        );
    }
}

#[test]
fn index_rechecks_even_an_already_public_pack_against_its_canary() {
    let (rig, q, source) = setup();
    let out = install(&rig, &q, &source);
    lay(&rig, &out, true);
    canary::verify_pinned(&rig.ctx, &q, 8).unwrap();
    // Keep signatures valid and all archive digests unchanged. A package-only check
    // accepts this replacement, but the index must require the canaried manifest.
    let text = std::fs::read_to_string(out.join("pkg-trust-8.toml"))
        .unwrap()
        .replace("0.1.0", "0.2.0");
    rig.host.put_asset(
        &rig.ctx.public_slug("trust"),
        "atpkg-trust-8",
        "pkg-trust-8.toml",
        text.as_bytes(),
    );
    rig.host.put_asset(
        &rig.ctx.public_slug("trust"),
        "atpkg-trust-8",
        "pkg-trust-8.toml.sig",
        rig.keys.sign(text.as_bytes()),
    );
    assert!(canary::verify_pinned(&rig.ctx, &q, 8).is_err());
}

struct Index(std::cell::Cell<usize>);
impl atpkg_publish::api::Indexer for Index {
    fn publish(
        &self,
        _: &atpkg_publish::ctx::Ctx,
        request: &atpkg_publish::api::IndexRequest,
    ) -> Result<atpkg_publish::api::Published> {
        assert!(request.upload);
        assert_eq!(request.delta.pins.get("trust"), Some(&8));
        self.0.set(self.0.get() + 1);
        Ok(atpkg_publish::api::Published {
            index_build: 8,
            dir: request.out.clone(),
            uploaded: true,
        })
    }
}

#[test]
fn staged_resume_never_repacks_and_is_idempotent_but_refuses_changed_staging() {
    let (rig, q, source) = setup();
    let out = install(&rig, &q, &source);
    lay(&rig, &source, false);
    rig.publish_head(&Doc::new(7, &[("trust", 7)]));
    std::fs::write(rig.path("staged-builds.toml"), "trust = 8\n").unwrap();
    let index = Index(std::cell::Cell::new(0));
    let mut log = Rig::log();
    canary::publish_staged(&rig.ctx, rig.tmp.path(), &index, &mut log).unwrap();
    canary::publish_staged(&rig.ctx, rig.tmp.path(), &index, &mut log).unwrap();
    assert_eq!(index.0.get(), 2);
    let text = std::fs::read_to_string(out.join("pkg-trust-8.toml"))
        .unwrap()
        .replace("0.1.0", "0.2.0");
    rig.host.put_asset(
        &rig.ctx.staging_slug("trust"),
        "atpkg-trust-8",
        "pkg-trust-8.toml",
        text.as_bytes(),
    );
    rig.host.put_asset(
        &rig.ctx.staging_slug("trust"),
        "atpkg-trust-8",
        "pkg-trust-8.toml.sig",
        rig.keys.sign(text.as_bytes()),
    );
    assert!(canary::publish_staged(&rig.ctx, rig.tmp.path(), &index, &mut log).is_err());
    assert_eq!(
        index.0.get(),
        2,
        "changed staging never reaches the indexer"
    );
}

#[test]
fn consumer_lock_and_command_modes_are_explicit() {
    let rig = Rig::new("canary-mode");
    let words = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    let req = atpkg_publish::channel::group::parse_request(
        &rig.ctx,
        &words("rustc --publish-staged --out group"),
    )
    .unwrap();
    assert_eq!(req.mode, atpkg_publish::channel::group::Mode::PublishStaged);
    assert!(
        atpkg_publish::channel::group::parse_request(
            &rig.ctx,
            &words("rustc --publish-staged --seal seal --out group")
        )
        .is_err()
    );
    assert!(
        atpkg_publish::channel::group::parse_request(
            &rig.ctx,
            &words("rustc --stage-upload --publish-staged --seal seal --out group")
        )
        .is_err()
    );
    let report = canary::Native.consumer(
        "ay",
        &rig.path("missing"),
        &rig.path("bin"),
        &rig.path("rustup"),
        &Installed {
            target: Triple::host().unwrap().to_string(),
            asset: "archive".into(),
            sha256: "a".repeat(64),
            tree_root: "b".repeat(64),
            compiler_commit: "c".repeat(40),
        },
    );
    assert_eq!(report.verdict, "could-not-run");
    assert_eq!(report.locked_build, None);
    assert!(!report.detail.is_empty());
}

#[test]
fn index_upload_refuses_an_uncanaried_public_build_before_publishing_the_index() {
    use atpkg_publish::api::{Baseline, Delta, IndexRequest};
    let (rig, q, source) = setup();
    let out = install(&rig, &q, &source);
    rig.publish_head(&Doc::new(7, &[("trust", 7)]));
    rig.lay_pack(
        &rig.ctx.accounts.public,
        "trust",
        7,
        &[Triple::host().unwrap().as_str()],
    );
    lay(&rig, &out, true);
    let text = std::fs::read_to_string(out.join("pkg-trust-8.toml"))
        .unwrap()
        .replace("0.1.0", "0.2.0");
    rig.host.put_asset(
        &rig.ctx.public_slug("trust"),
        "atpkg-trust-8",
        "pkg-trust-8.toml",
        text.as_bytes(),
    );
    rig.host.put_asset(
        &rig.ctx.public_slug("trust"),
        "atpkg-trust-8",
        "pkg-trust-8.toml.sig",
        rig.keys.sign(text.as_bytes()),
    );
    let request = IndexRequest {
        baseline: Baseline::ChannelHead,
        delta: Delta {
            pins: BTreeMap::from([("trust".into(), 8)]),
            ..Delta::default()
        },
        out: rig.path("index-out"),
        upload: true,
        pack_dir: None,
    };
    let e = atpkg_publish::channel::index::publish_with(&rig.ctx, &request, &mut Rig::log())
        .unwrap_err();
    assert!(e.to_string().contains("does not bind"), "{e}");
    assert!(rig.index_release(8).is_none());
    lay(&rig, &out, true);
    atpkg_publish::channel::index::publish_with(&rig.ctx, &request, &mut Rig::log()).unwrap();
    assert!(rig.index_release(8).is_some());
}

#[test]
fn a_source_replaced_during_native_execution_cannot_be_signed_as_the_installed_pack() {
    struct Replaces<'a> {
        source: &'a Path,
        keys: &'a atpkg_publish::testkit::Keys,
    }
    impl Runner for Replaces<'_> {
        fn compiler(&self, bin: &Path, rustup: &Path) -> Result<String> {
            let commit = Runs { fail: false }.compiler(bin, rustup)?;
            let path = self.source.join("pkg-trust-8.toml");
            let text = std::fs::read_to_string(&path)
                .unwrap()
                .replace("0.1.0", "0.2.0");
            std::fs::write(path, &text).unwrap();
            std::fs::write(
                self.source.join("pkg-trust-8.toml.sig"),
                self.keys.sign(text.as_bytes()),
            )
            .unwrap();
            Ok(commit)
        }
        fn consumer(&self, _: &str, _: &Path, _: &Path, _: &Path, _: &Installed) -> ConsumerReport {
            panic!("no consumers in this fixture")
        }
    }
    let (rig, q, source) = setup();
    let out = rig.path("canary");
    let error = canary::install(
        &rig.ctx,
        &q,
        &source,
        8,
        &out,
        &BTreeMap::new(),
        &Replaces {
            source: &source,
            keys: &rig.keys,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("source changed"), "{error}");
    assert!(!out.join(canary::RECEIPT).exists());
    assert!(!out.join(canary::SIGNATURE).exists());
}
