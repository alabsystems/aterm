// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native installation with injected execution, and signed-byte publication negative controls.

use std::collections::BTreeMap;
use std::path::Path;

use atpkg_publish::canary::{self, ConsumerReport, Installed, Runner};
use atpkg_publish::channel::{index, mirror};
use atpkg_publish::error::Result;
use atpkg_publish::quad;
use atpkg_publish::triple::Triple;

use super::{Doc, Rig, sha256_hex};

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

fn cohort() -> BTreeMap<String, u64> {
    BTreeMap::from([
        ("trust".into(), 8),
        ("trust-ir".into(), 18),
        ("trust-cg".into(), 28),
        ("trust-vc".into(), 38),
    ])
}

fn staged_cohort(rig: &Rig, targets: &[&str]) {
    std::fs::write(
        rig.path("staged-builds.toml"),
        aterm_toml::to_string(&cohort()).unwrap(),
    )
    .unwrap();
    for (program, build) in cohort() {
        if program == "trust" {
            continue;
        }
        let mut pack = rig.pack(&program, build, targets);
        let mf = format!("pkg-{program}-{build}.toml");
        let text = String::from_utf8(pack.files.iter().find(|(n, _)| n == &mf).unwrap().1.clone())
            .unwrap();
        let mut bound = String::new();
        for line in text.lines() {
            bound.push_str(line);
            bound.push('\n');
            if line.starts_with("size = ") {
                bound.push_str(&format!(
                    "[artifact.provenance]\ncompiler_commit = {:?}\n",
                    "a".repeat(40)
                ));
            }
        }
        for (name, bytes) in &mut pack.files {
            if name == &mf {
                *bytes = bound.as_bytes().to_vec();
            } else if name == &format!("{mf}.sig") {
                *bytes = rig.keys.sign(bound.as_bytes());
            }
        }
        let slug = rig.ctx.staging_slug(&pack.repo);
        for (name, bytes) in pack.files {
            rig.host.put_asset(&slug, &pack.tag, &name, bytes);
        }
    }
}

// Signed inert train inputs exercise selection on any test host. Foreign-host
// compiler execution is not claimed by this metadata fixture.
fn train_targets(rig: &Rig, dir: &Path, targets: &[&str]) {
    let mut receipt: canary::Receipt =
        aterm_toml::from_str(&std::fs::read_to_string(dir.join(canary::RECEIPT)).unwrap()).unwrap();
    let first = receipt.installed[0].clone();
    let bytes = std::fs::read(dir.join(&first.asset)).unwrap();
    let mut text = "schema = 1\nprogram = \"trust\"\nbuild_number = 8\nversion = \"0.1.0\"\nexposes = [\"trustc\"]\n".to_string();
    receipt.installed.clear();
    for target in targets {
        let asset = format!("trust-8-{target}.tar.zst");
        std::fs::write(dir.join(&asset), &bytes).unwrap();
        text.push_str(&format!("\n[[artifact]]\ntarget = {target:?}\nkind = \"sysroot-bundle\"\nasset = {asset:?}\nsha256 = {:?}\ntree_root = {:?}\nsize = {}\nreloc = \"none\"\n", first.sha256, first.tree_root, bytes.len()));
        receipt.installed.push(Installed {
            target: (*target).into(),
            asset,
            ..first.clone()
        });
    }
    if !receipt.installed.iter().any(|i| i.asset == first.asset) {
        std::fs::remove_file(dir.join(&first.asset)).unwrap();
    }
    std::fs::write(dir.join("pkg-trust-8.toml"), &text).unwrap();
    std::fs::write(
        dir.join("pkg-trust-8.toml.sig"),
        rig.keys.sign(text.as_bytes()),
    )
    .unwrap();
    receipt.manifest_sha256 = sha256_hex(text.as_bytes());
    let text = aterm_toml::to_string(&receipt).unwrap();
    std::fs::write(dir.join(canary::RECEIPT), &text).unwrap();
    std::fs::write(dir.join(canary::SIGNATURE), rig.keys.sign(text.as_bytes())).unwrap();
}

#[test]
fn an_arm_only_staged_cohort_preserves_global_intel_and_unrelated_selections() {
    let (rig, q, source) = setup();
    let out = install(&rig, &q, &source);
    let arm = Triple::parse("aarch64-apple-darwin").unwrap();
    train_targets(&rig, &out, &[arm.as_str()]);
    let intel = "x86_64-apple-darwin";
    let linux = "x86_64-unknown-linux-gnu";
    let globals = [
        ("trust", 7),
        ("trust-ir", 17),
        ("trust-cg", 27),
        ("trust-vc", 37),
        ("ay", 21),
        ("ny", 14),
    ];
    let intel_pins = [
        ("trust", 6),
        ("trust-ir", 16),
        ("trust-cg", 26),
        ("trust-vc", 36),
        ("ay", 23),
    ];
    let base = Doc::new(7, &globals)
        .overlay(intel, &intel_pins)
        .overlay(arm.as_str(), &[("ay", 22), ("ny", 15)])
        .overlay(linux, &[("ny", 16)]);
    rig.publish_head(&base);
    for (program, build) in globals {
        rig.lay_pack(
            &rig.ctx.accounts.public,
            program,
            build,
            &[arm.as_str(), intel, linux],
        );
    }
    for (program, build) in intel_pins {
        rig.lay_pack(&rig.ctx.accounts.public, program, build, &[intel]);
    }
    for (program, build, target) in [
        ("ay", 22, arm.as_str()),
        ("ny", 15, arm.as_str()),
        ("ny", 16, linux),
    ] {
        rig.lay_pack(&rig.ctx.accounts.public, program, build, &[target]);
    }
    lay(&rig, &out, false);
    staged_cohort(&rig, &[arm.as_str()]);
    canary::publish_staged(&rig.ctx, rig.tmp.path(), &index::Index, &mut Rig::log()).unwrap();
    let (_, published) =
        quad::fetch_head(&rig.ctx, &rig.ctx.index_slug(), &rig.path("result")).unwrap();
    let channel = &published.index.channels[0];
    assert_eq!(
        channel.pin,
        globals
            .into_iter()
            .map(|(p, b)| (p.to_string(), b))
            .collect()
    );
    assert_eq!(
        channel.pin_by_target[intel],
        intel_pins
            .into_iter()
            .map(|(p, b)| (p.to_string(), b))
            .collect()
    );
    assert_eq!(
        channel.pin_by_target[linux],
        BTreeMap::from([("ny".into(), 16)])
    );
    let mut expected = cohort();
    expected.extend([("ay".into(), 22), ("ny".into(), 15)]);
    assert_eq!(channel.pin_by_target[arm.as_str()], expected);
}

#[derive(Default)]
struct Requests(std::cell::RefCell<Vec<atpkg_publish::api::IndexRequest>>);

impl atpkg_publish::api::Indexer for Requests {
    fn publish(
        &self,
        _: &atpkg_publish::ctx::Ctx,
        request: &atpkg_publish::api::IndexRequest,
    ) -> Result<atpkg_publish::api::Published> {
        self.0.borrow_mut().push(request.clone());
        Ok(atpkg_publish::api::Published {
            index_build: 8,
            dir: request.out.clone(),
            uploaded: request.upload,
        })
    }
}

#[test]
fn every_canaried_target_gets_the_same_complete_group_tuple() {
    let (rig, q, source) = setup();
    let out = install(&rig, &q, &source);
    let targets = ["x86_64-apple-darwin", "aarch64-apple-darwin"];
    train_targets(&rig, &out, &targets);
    lay(&rig, &out, false);
    staged_cohort(&rig, &targets);
    rig.publish_head(&Doc::new(
        7,
        &[
            ("trust", 7),
            ("trust-ir", 17),
            ("trust-cg", 27),
            ("trust-vc", 37),
        ],
    ));
    let requests = Requests::default();
    canary::publish_staged(&rig.ctx, rig.tmp.path(), &requests, &mut Rig::log()).unwrap();
    let requests = requests.0.borrow();
    let delta = &requests[0].delta;
    assert!(delta.pins.is_empty());
    assert_eq!(delta.target_pins.len(), targets.len());
    for target in targets {
        assert_eq!(delta.target_pins[&Triple::parse(target).unwrap()], cohort());
    }
}

#[test]
fn an_incomplete_group_target_or_foreign_compiler_never_reaches_the_indexer() {
    for wrong_compiler in [false, true] {
        let (rig, q, source) = setup();
        let out = install(&rig, &q, &source);
        let targets = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
        train_targets(&rig, &out, &targets);
        lay(&rig, &out, false);
        staged_cohort(
            &rig,
            if wrong_compiler {
                &targets
            } else {
                &targets[..1]
            },
        );
        rig.publish_head(&Doc::new(7, &[("trust", 7)]));
        if wrong_compiler {
            let slug = rig.ctx.staging_slug("trust-ir");
            let name = "pkg-trust-ir-18.toml";
            let text = String::from_utf8(rig.host.asset(&slug, "atpkg-trust-ir-18", name).unwrap())
                .unwrap()
                .replace(&"a".repeat(40), &"b".repeat(40));
            rig.host
                .put_asset(&slug, "atpkg-trust-ir-18", name, text.as_bytes());
            rig.host.put_asset(
                &slug,
                "atpkg-trust-ir-18",
                &format!("{name}.sig"),
                rig.keys.sign(text.as_bytes()),
            );
        }
        let requests = Requests::default();
        let error = canary::publish_staged(&rig.ctx, rig.tmp.path(), &requests, &mut Rig::log())
            .unwrap_err();
        assert!(
            error.to_string().contains(if wrong_compiler {
                "canaried compiler"
            } else {
                "no unique artifact"
            }),
            "{error}"
        );
        assert!(requests.0.borrow().is_empty());
    }
}

#[test]
fn a_partial_or_foreign_staged_tuple_never_reaches_the_indexer() {
    let (rig, q, source) = setup();
    install(&rig, &q, &source);
    lay(&rig, &source, false);
    staged_cohort(&rig, &[Triple::host().unwrap().as_str()]);
    let mut builds = cohort();
    builds.remove("trust-vc");
    builds.insert("ay".into(), 22);
    std::fs::write(
        rig.path("staged-builds.toml"),
        aterm_toml::to_string(&builds).unwrap(),
    )
    .unwrap();
    rig.publish_head(&Doc::new(7, &[("trust", 7)]));
    let requests = Requests::default();
    let error =
        canary::publish_staged(&rig.ctx, rig.tmp.path(), &requests, &mut Rig::log()).unwrap_err();
    assert!(error.to_string().contains("must select exactly"), "{error}");
    assert!(requests.0.borrow().is_empty());
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
        assert!(request.delta.pins.is_empty());
        assert_eq!(
            request.delta.target_pins[&Triple::host().unwrap()],
            cohort()
        );
        self.0.set(self.0.get() + 1);
        Ok(atpkg_publish::api::Published {
            index_build: 8,
            dir: request.out.clone(),
            uploaded: true,
        })
    }
}

fn stage_existing(rig: &Rig, out: &Path) -> Result<()> {
    canary::run(
        &rig.ctx,
        &[
            "trust=8".into(),
            "--out".into(),
            out.to_str().unwrap().into(),
            "--stage-receipt".into(),
        ],
    )
}

#[test]
fn staging_canary_evidence_leaves_the_index_and_existing_receipts_unchanged() {
    let (rig, q, source) = setup();
    let out = install(&rig, &q, &source);
    lay(&rig, &source, false);
    rig.publish_head(&Doc::new(7, &[("trust", 7)]));
    stage_existing(&rig, &out).unwrap();
    let slug = rig.ctx.staging_slug("trust");
    for name in [canary::RECEIPT, canary::SIGNATURE] {
        assert_eq!(
            rig.host.asset(&slug, "atpkg-trust-8", name).unwrap(),
            std::fs::read(out.join(name)).unwrap()
        );
    }
    let changes = rig.host.changes();
    stage_existing(&rig, &out).unwrap();
    assert_eq!(rig.host.changes(), changes, "an exact retry writes nothing");
    let (head, _) = quad::fetch_head(&rig.ctx, &rig.ctx.index_slug(), &rig.path("head")).unwrap();
    assert_eq!(head, 7, "staging evidence never publishes an index");

    rig.host
        .put_asset(&slug, "atpkg-trust-8", canary::RECEIPT, b"other evidence");
    let changes = rig.host.changes();
    let error = stage_existing(&rig, &out).unwrap_err();
    assert!(
        error.to_string().contains("different canary evidence"),
        "{error}"
    );
    assert_eq!(
        rig.host.changes(),
        changes,
        "conflicting evidence is preserved"
    );
    assert_eq!(
        rig.host
            .asset(&slug, "atpkg-trust-8", canary::RECEIPT)
            .unwrap(),
        b"other evidence"
    );
}

#[test]
fn staging_canary_evidence_refuses_changed_pack_bytes_and_invalid_signatures() {
    for corrupt_archive in [true, false] {
        let (rig, q, source) = setup();
        let out = install(&rig, &q, &source);
        lay(&rig, &source, false);
        rig.publish_head(&Doc::new(7, &[("trust", 7)]));
        let slug = rig.ctx.staging_slug("trust");
        if corrupt_archive {
            let receipt = canary::check_receipt(&rig.ctx, &q, &out, 8).unwrap();
            rig.host.put_asset(
                &slug,
                "atpkg-trust-8",
                &receipt.installed[0].asset,
                b"changed after the canary ran",
            );
        } else {
            std::fs::write(out.join(canary::SIGNATURE), b"invalid signature").unwrap();
        }
        let changes = rig.host.changes();
        assert!(stage_existing(&rig, &out).is_err());
        assert_eq!(
            rig.host.changes(),
            changes,
            "a refused stage writes nothing"
        );
        assert!(
            rig.host
                .asset(&slug, "atpkg-trust-8", canary::RECEIPT)
                .is_none()
        );
    }
}

#[test]
fn staged_resume_never_repacks_and_is_idempotent_but_refuses_changed_staging() {
    let (rig, q, source) = setup();
    let out = install(&rig, &q, &source);
    lay(&rig, &source, false);
    rig.publish_head(&Doc::new(7, &[("trust", 7)]));
    staged_cohort(&rig, &[Triple::host().unwrap().as_str()]);
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
