// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The manifest writer, proven by the client: fresh and merged manifests
//! signed with testkit keys pass `VerifiedQuad::verify_pkg`, a merge keeps its base as a
//! byte prefix, and every merge and provenance refusal fires before anything is signed.

use std::path::Path;
use std::sync::Arc;

use atpkg::manifest::Provenance;
use atpkg_publish::artifact::pkgdoc::{self, Doc, Header, Kind, Row};
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::quad::{self, VerifiedQuad};
use atpkg_publish::testkit::Keys;
use atpkg_publish::triple::{AARCH64_APPLE_DARWIN, Triple, X86_64_APPLE_DARWIN};

use super::fixture::{COMMIT, scratch};

pub fn provenance(lineage: &str) -> Provenance {
    Provenance {
        compiler_release: "1.99.0-dev".into(),
        compiler_commit: COMMIT.into(),
        compiler_trust: "0.1.0".into(),
        compiler_host: "aarch64-apple-darwin".into(),
        trustc_sha256: "a".repeat(64),
        targo_sha256: "b".repeat(64),
        driver_sha256: "c".repeat(64),
        lineage: lineage.into(),
        lineage_receipt_sha256: "none".into(),
        ddc_seed_relation: "none".into(),
        scan_objects: "1".into(),
        scan_findings: "0".into(),
    }
}

pub fn row(triple: Triple, kind: Kind) -> Row {
    Row {
        triple,
        kind,
        asset: pkgdoc::asset_name("ay", 9, triple),
        sha256: "1".repeat(64),
        tree_root: "2".repeat(64),
        size: 1234,
        disk_installed: 4567,
        provenance: provenance("seed-mint"),
    }
}

pub fn header() -> Header {
    Header {
        program: "ay".into(),
        version: "0.2.0".into(),
        build: 9,
        exposes: vec!["ay".into(), "ay-lsp".into()],
    }
}

fn head(keys: &Keys, home: &Path) -> VerifiedQuad {
    let ctx = keys.ctx(home, Arc::new(FakeHost::new()));
    let dir = home.join("quad");
    keys.write_quad(&dir, &keys.index_toml(41, &[("ay", "ay")], &[("ay", 9)]));
    quad::verify_dir(&ctx, &dir).expect("the head verifies")
}

fn verified(keys: &Keys, q: &VerifiedQuad, doc: &Doc) -> atpkg::PkgManifest {
    q.verify_pkg(doc.bytes(), &keys.sign(doc.bytes()))
        .expect("the client accepts what was signed")
}

#[test]
fn a_fresh_manifest_signed_by_a_rostered_machine_is_what_the_client_parses() {
    let tmp = scratch("pkgdoc-fresh");
    let keys = Keys::mint("m3", &[]);
    let q = head(&keys, tmp.path());
    let r = row(AARCH64_APPLE_DARWIN, Kind::Binary);
    let doc = pkgdoc::fresh(&header(), &r).expect("render");
    assert!(doc.base().is_empty());
    let parsed = verified(&keys, &q, &doc);
    pkgdoc::check_parsed(&parsed, &header(), &r).expect("the row reads back as packed");
    assert_eq!(parsed.exposes, vec!["ay", "ay-lsp"]);
    assert_eq!(parsed.version, "0.2.0");
    assert_eq!(parsed.artifacts[0].protocol, "github-release");

    let bundle = row(AARCH64_APPLE_DARWIN, Kind::SysrootBundle);
    let doc = pkgdoc::fresh(&header(), &bundle).expect("render a bundle");
    let parsed = verified(&keys, &q, &doc);
    assert_eq!(parsed.artifacts[0].kind, "sysroot-bundle");
    assert!(
        String::from_utf8_lossy(doc.bytes()).contains("\nreloc = \"self-contained\"\n"),
        "the relocation policy is inside the signed bytes"
    );
}

#[test]
fn a_merge_appends_one_row_and_keeps_the_base_as_a_byte_prefix() {
    let tmp = scratch("pkgdoc-merge");
    let keys = Keys::mint("m3", &[]);
    let q = head(&keys, tmp.path());
    let first = row(AARCH64_APPLE_DARWIN, Kind::Binary);
    let base = pkgdoc::fresh(&header(), &first)
        .expect("base")
        .bytes()
        .to_vec();
    let second = row(X86_64_APPLE_DARWIN, Kind::Binary);
    let doc = pkgdoc::merge(base.clone(), &header(), &second).expect("merge");
    assert_eq!(doc.base(), &base[..]);
    assert!(doc.extends(&base));
    let parsed = verified(&keys, &q, &doc);
    assert_eq!(parsed.artifacts.len(), 2);
    pkgdoc::check_parsed(&parsed, &header(), &first).expect("the base row");
    pkgdoc::check_parsed(&parsed, &header(), &second).expect("the appended row");

    // A base that ends without a newline still extends cleanly.
    let mut trimmed = base.clone();
    while trimmed.last() == Some(&b'\n') {
        trimmed.pop();
    }
    let doc = pkgdoc::merge(trimmed.clone(), &header(), &second).expect("merge");
    assert!(doc.extends(&trimmed));
    assert_eq!(verified(&keys, &q, &doc).artifacts.len(), 2);
}

#[test]
fn a_merge_onto_a_foreign_or_conflicting_base_is_refused() {
    let base = pkgdoc::fresh(&header(), &row(AARCH64_APPLE_DARWIN, Kind::Binary))
        .expect("base")
        .bytes()
        .to_vec();
    let x86 = row(X86_64_APPLE_DARWIN, Kind::Binary);
    let refused = |base: Vec<u8>, h: &Header, r: &Row, want: &str| {
        let err = pkgdoc::merge(base, h, r).expect_err(want);
        assert!(err.to_string().contains(want), "{want}: {err}");
        assert_eq!(err.exit_code(), 1);
    };
    let with = |edit: &dyn Fn(&mut Header)| {
        let mut h = header();
        edit(&mut h);
        h
    };
    refused(
        base.clone(),
        &with(&|h| h.program = "ny".into()),
        &x86,
        "for program",
    );
    refused(
        base.clone(),
        &with(&|h| h.build = 10),
        &x86,
        "describes ONE build",
    );
    refused(
        base.clone(),
        &with(&|h| h.version = "0.3.0".into()),
        &x86,
        "is version Some(\"0.2.0\")",
    );
    // One build, one exposed set: a target that lacks a shimmed tool cannot join it,
    // and neither can one that shims a tool the base's targets lack.
    refused(
        base.clone(),
        &with(&|h| h.exposes = vec!["ay".into()]),
        &x86,
        "one build has one exposed set",
    );
    refused(
        base.clone(),
        &with(&|h| h.exposes.push("ay-fmt".into())),
        &x86,
        "one build has one exposed set",
    );
    let schema2 = String::from_utf8(base.clone())
        .expect("utf8")
        .replace("schema = 1", "schema = 2");
    refused(schema2.into_bytes(), &header(), &x86, "schema");
    refused(
        base.clone(),
        &header(),
        &row(AARCH64_APPLE_DARWIN, Kind::Binary),
        "already carries an [[artifact]] row for aarch64-apple-darwin",
    );
    let mut same_asset = x86.clone();
    same_asset.asset = pkgdoc::asset_name("ay", 9, AARCH64_APPLE_DARWIN);
    refused(base.clone(), &header(), &same_asset, "already names asset");
    refused(b"schema = = 1".to_vec(), &header(), &x86, "does not parse");
    // The exposed set is a set: the order a list was written in makes no difference.
    let reordered = with(&|h| h.exposes.reverse());
    pkgdoc::merge(base, &reordered, &x86).expect("the same set, another order");
}

#[test]
fn the_self_check_holds_the_parsed_header_to_the_packed_one() {
    let tmp = scratch("pkgdoc-header");
    let keys = Keys::mint("m3", &[]);
    let q = head(&keys, tmp.path());
    let r = row(AARCH64_APPLE_DARWIN, Kind::Binary);
    let parsed = verified(&keys, &q, &pkgdoc::fresh(&header(), &r).expect("render"));
    for (what, h) in [
        ("version", {
            let mut h = header();
            h.version = "0.2.1".into();
            h
        }),
        ("exposes", {
            let mut h = header();
            h.exposes.pop();
            h
        }),
    ] {
        let err = pkgdoc::check_parsed(&parsed, &h, &r).expect_err(what);
        assert!(err.to_string().contains("not the packed"), "{what}: {err}");
    }
}

#[test]
fn only_a_clean_scan_record_of_an_admitted_compiler_is_rendered() {
    let r = row(AARCH64_APPLE_DARWIN, Kind::Binary);
    for (field, value, want) in [
        ("scan_findings", "3", "scan finding"),
        ("compiler_commit", "abc", "40 hex"),
        ("lineage", "maybe", "lineage | seed-mint | unproven"),
        ("compiler_release", "1.99 dev", "not a plain value"),
    ] {
        let mut bad = r.clone();
        let p = &mut bad.provenance;
        let slot = match field {
            "scan_findings" => &mut p.scan_findings,
            "compiler_commit" => &mut p.compiler_commit,
            "lineage" => &mut p.lineage,
            _ => &mut p.compiler_release,
        };
        *slot = value.to_string();
        let err = pkgdoc::fresh(&header(), &bad).expect_err(field);
        assert!(err.to_string().contains(want), "{field}: {err}");
    }
    let mut none = header();
    none.exposes.clear();
    assert!(
        pkgdoc::fresh(&none, &r).is_err(),
        "a pack that shims nothing"
    );
    for l in ["lineage", "seed-mint", "unproven"] {
        pkgdoc::fresh(
            &header(),
            &Row {
                provenance: provenance(l),
                ..r.clone()
            },
        )
        .expect(l);
    }
}

#[test]
fn a_manifest_a_stranger_signed_is_refused_by_the_client() {
    let tmp = scratch("pkgdoc-stranger");
    let keys = Keys::mint("m3", &[]);
    let q = head(&keys, tmp.path());
    let doc = pkgdoc::fresh(&header(), &row(AARCH64_APPLE_DARWIN, Kind::Binary)).expect("render");
    let stranger = Keys::mint("m4", &[]);
    assert!(
        q.verify_pkg(doc.bytes(), &stranger.sign(doc.bytes()))
            .is_err()
    );
    let mut edited = doc.bytes().to_vec();
    edited.extend(b"\n");
    assert!(
        q.verify_pkg(&edited, &keys.sign(doc.bytes())).is_err(),
        "a signature covers the exact bytes"
    );
}

#[test]
fn the_aarch64_darwin_asset_keeps_its_bare_name_and_every_other_target_names_itself() {
    assert_eq!(
        pkgdoc::asset_name("ay", 9, AARCH64_APPLE_DARWIN),
        "ay-9.tar.zst"
    );
    for t in Triple::all().filter(|t| *t != AARCH64_APPLE_DARWIN) {
        assert_eq!(pkgdoc::asset_name("ay", 9, t), format!("ay-9-{t}.tar.zst"));
    }
    assert_eq!(pkgdoc::manifest_name("ay", 9), "pkg-ay-9.toml");
}
