// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The plain-program pack end to end over a fake build driver: the compiler admitted before
//! anything is read, the build's lane flag, target, CPU floor and compiler, the arch gate
//! and the payload scan refusing before a manifest exists, the expose declaration, the
//! receipt switch asked again before an `unproven` row is signed, a merge for a second
//! target, the signed pair verified by the client, and the staging upload with the
//! public pack immutable.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use atpkg_publish::api::{Lineage, PackRequest, Signing};
use atpkg_publish::artifact::pack::{self, ExposeMode, Metadata};
use atpkg_publish::artifact::{pkgdoc, sign, upload};
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::Error;
use atpkg_publish::host::Slug;
use atpkg_publish::host::fake::{FakeHost, FakeRelease};
use atpkg_publish::testkit::Keys;
use atpkg_publish::triple::{
    AARCH64_APPLE_DARWIN, Triple, X86_64_APPLE_DARWIN, X86_64_UNKNOWN_LINUX_GNU,
};

use super::fixture::{
    Answer, CPU_ARM64, CPU_X86_64, DORMANT_LEDGER, EM_AARCH64, EM_X86_64, FakeTools, MINTED_LEDGER,
    bare_origin, checkout as fixture_checkout, client_verifies, elf, macho, push_from_elsewhere,
    put, put_exe, ran, scratch, seed_mint, toolchain as fixture_toolchain, trust_repo,
};

/// A machine with a rostered key, a channel head, a dormant trust checkout at `$HOME/trust`,
/// a fixture Trust toolchain and an `ay` checkout.
struct World {
    _tmp: aterm_tempfile::TempDir,
    root: PathBuf,
    ctx: Ctx,
    host: Arc<FakeHost>,
    checkout: PathBuf,
    bin: PathBuf,
}

fn world(label: &str) -> World {
    let tmp = scratch(label);
    let root = std::fs::canonicalize(tmp.path()).expect("root");
    let home = root.join("home");
    let keys = Keys::mint("m3", &[]);
    let host = Arc::new(FakeHost::new());
    let ctx = keys.ctx(&home, host.clone());
    keys.publish_quad(
        &host,
        &ctx.index_slug(),
        41,
        &keys.index_toml(41, &[("ay", "ay")], &[("ay", 8)]),
    );
    trust_repo(&home.join("trust"), DORMANT_LEDGER);
    let bin = fixture_toolchain(&root.join("seal"));
    let checkout = root.join("ay");
    fixture_checkout(&checkout, &[("Cargo.toml", b"[package]\nname = \"ay\"\n")]);
    World {
        _tmp: tmp,
        root,
        ctx,
        host,
        checkout,
        bin,
    }
}

impl World {
    fn request(&self, triple: Triple) -> PackRequest {
        PackRequest {
            program: "ay".into(),
            checkout: self.checkout.clone(),
            triple,
            build: Some(9),
            merge_from: None,
            toolchain: Some(self.bin.clone()),
            out: self.root.join("out"),
            signing: Signing::MachineKey,
            upload: false,
            env: Vec::new(),
        }
    }

    fn written(&self, suffix: &str) -> Vec<PathBuf> {
        atpkg_publish::artifact::relocate::walk_files(&self.root.join("out"))
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.to_string_lossy().ends_with(suffix))
            .collect()
    }
}

/// `targo metadata` JSON for the `ay` checkout.
fn metadata_json(
    checkout: &Path,
    expose: &[&str],
    bundle: &[&str],
    args: &[&str],
    only: bool,
) -> String {
    let list = |v: &[&str]| {
        v.iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "{{\"packages\":[{{\"name\":\"ay\",\"version\":\"0.2.0\",\"manifest_path\":\"{}\"}}],\
         \"metadata\":{{\"atpkg\":{{\"expose\":[{}],\"bundle\":[{}],\"build-args\":[{}],\
         \"bundle-only\":{only}}}}}}}",
        checkout.join("Cargo.toml").display(),
        list(expose),
        list(bundle),
        list(args)
    )
}

/// A build driver: `metadata` answers `meta`; `build` lays `bins` under
/// `target/<triple>/release/` and reports them (with a library and a non-member bin it
/// must not collect), or fails.
fn cargo(checkout: &Path, meta: String, bins: Vec<(&'static str, Vec<u8>)>, fail: bool) -> Answer {
    let checkout = checkout.to_path_buf();
    Box::new(move |run| {
        let argv = run.argv();
        if argv.first().map(String::as_str) == Some("metadata") {
            return Some(ran(0, &meta, ""));
        }
        if !argv.iter().any(|a| a == "build") {
            return None;
        }
        if fail {
            return Some(ran(
                101,
                "",
                "   Compiling ay v0.2.0\nerror[E0425]: cannot find value `x`\nerror: could not \
                 compile `ay`",
            ));
        }
        let triple = argv
            .iter()
            .skip_while(|a| *a != "--target")
            .nth(1)
            .cloned()
            .unwrap_or_default();
        let dir = checkout.join("target").join(triple).join("release");
        let manifest = checkout.join("Cargo.toml");
        let mut out = String::new();
        for (name, bytes) in &bins {
            let exe = put_exe(&dir.join(name), bytes);
            out.push_str(&format!(
                "{{\"reason\":\"compiler-artifact\",\"manifest_path\":\"{}\",\"target\":\
                 {{\"kind\":[\"bin\"],\"name\":\"{name}\"}},\"executable\":\"{}\"}}\n",
                manifest.display(),
                exe.display()
            ));
        }
        out.push_str(&format!(
            "{{\"reason\":\"compiler-artifact\",\"manifest_path\":\"{}\",\"target\":{{\"kind\":\
             [\"lib\"]}},\"executable\":null}}\n\
             {{\"reason\":\"compiler-artifact\",\"manifest_path\":\"/elsewhere/dep/Cargo.toml\",\
             \"target\":{{\"kind\":[\"bin\"]}},\"executable\":\"/elsewhere/dep/tool\"}}\n\
             {{\"reason\":\"build-finished\",\"success\":true}}\n",
            manifest.display()
        ));
        Some(ran(0, &out, "    Finished `release` profile"))
    })
}

fn bin_bytes(head: Vec<u8>, body: &str) -> Vec<u8> {
    let mut b = head;
    b.extend(body.as_bytes());
    b
}

fn linux_ay() -> Vec<(&'static str, Vec<u8>)> {
    vec![("ay", bin_bytes(elf(EM_X86_64, 2, 1), "ay x86_64"))]
}

fn tools(w: &World, host: &str, meta: String, bins: Vec<(&'static str, Vec<u8>)>) -> FakeTools {
    let mut t = FakeTools::new(host);
    t.answer = cargo(&w.checkout, meta, bins, false);
    t
}

fn declared(w: &World) -> String {
    metadata_json(&w.checkout, &["ay"], &[], &[], false)
}

#[test]
fn a_signed_pack_is_one_the_client_verifies_and_extracts_to_its_signed_root() {
    let w = world("pack-signed");
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
    let out = pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t).expect("packed");
    assert_eq!(out.build, 9);
    assert_eq!(out.version, "0.2.0");
    assert_eq!(out.exposes, vec!["ay"]);
    assert_eq!(
        out.lineage,
        Lineage::Unproven,
        "a dormant switch records it"
    );
    assert!(!out.uploaded);
    let sig = out.signature.clone().expect("signed");
    let parsed = client_verifies(&w.ctx, &out.manifest, &sig).expect("the client accepts the pair");
    let row = &parsed.artifacts[0];
    assert_eq!(row.target, "x86_64-unknown-linux-gnu");
    assert_eq!(row.asset, "ay-9-x86_64-unknown-linux-gnu.tar.zst");
    let prov = row.provenance.as_ref().expect("provenance");
    assert_eq!(prov.compiler_commit, super::fixture::COMMIT);
    assert_eq!(prov.lineage, "unproven");
    let x = w.root.join("x");
    std::fs::create_dir_all(&x).expect("mkdir");
    super::fixture::set_mode(&x, 0o700);
    atpkg::extract_tar_zst(
        &out.asset,
        &x,
        row.cost.disk_installed.max(1 << 20) * 2,
        1000,
    )
    .expect("extracts");
    assert_eq!(atpkg::tree_root(&x).expect("root"), row.tree_root);
    assert!(x.join("bin/ay").is_file());

    let meta = &t.runs_of("metadata")[0];
    assert_eq!(meta.program, w.bin.join("targo"));
    assert!(!meta.argv().contains(&"--unverified".to_string()));
    let build = &t.runs_of("build")[0];
    assert_eq!(
        build.argv()[..5],
        [
            "--unverified",
            "build",
            "--release",
            "--target",
            "x86_64-unknown-linux-gnu"
        ]
    );
    assert_eq!(
        build.env_of("RUSTC"),
        Some(&w.bin.join("trustc").into_os_string())
    );
    assert_eq!(
        build
            .env_of("RUSTFLAGS")
            .map(|v| v.to_string_lossy().into_owned()),
        Some("-C target-cpu=x86-64-v2".into())
    );
    assert_eq!(build.cwd.as_deref(), Some(w.checkout.as_path()));
    assert!(
        t.log()
            .iter()
            .any(|l| l.starts_with("strip ") && l.ends_with("bin/ay")),
        "{:?}",
        t.log()
    );
}

#[test]
fn every_target_builds_at_its_distribution_cpu_floor() {
    let want = [
        ("aarch64-apple-darwin", "-C target-cpu=apple-m1"),
        ("x86_64-apple-darwin", "-C target-cpu=x86-64-v2"),
        ("x86_64-unknown-linux-gnu", "-C target-cpu=x86-64-v2"),
        ("aarch64-unknown-linux-gnu", ""),
        ("x86_64-pc-windows-msvc", ""),
        ("aarch64-pc-windows-msvc", ""),
    ];
    assert_eq!(Triple::all().count(), want.len());
    for (t, flags) in want {
        assert_eq!(pack::rustflags(Triple::parse(t).expect("t")), flags, "{t}");
    }
}

#[test]
fn a_mislabelled_build_is_refused_by_the_arch_gate_before_any_manifest() {
    let w = world("pack-arch");
    let arm = vec![("ay", bin_bytes(elf(EM_AARCH64, 2, 1), "ay arm64"))];
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), arm);
    let err = pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t)
        .expect_err("an AArch64 ELF claimed as x86_64");
    assert!(err.to_string().contains("ARCHITECTURE MISMATCH"), "{err}");
    assert!(w.written(".toml").is_empty(), "{:?}", w.written(".toml"));
    assert!(w.written(".tar.zst").is_empty());
}

#[test]
fn a_bundle_only_program_and_a_declared_bin_that_did_not_build_are_refused() {
    let w = world("pack-decl");
    let only = metadata_json(&w.checkout, &["ay"], &[], &[], true);
    let t = tools(&w, "aarch64-apple-darwin", only, linux_ay());
    let err =
        pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t).expect_err("bundle-only");
    assert!(err.to_string().contains("bundle-only"), "{err}");
    assert!(t.runs_of("build").is_empty(), "refused before the build");

    let missing = metadata_json(&w.checkout, &["ay", "ay-lsp"], &[], &[], false);
    let t = tools(&w, "aarch64-apple-darwin", missing, linux_ay());
    let err = pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t)
        .expect_err("ay-lsp never built");
    assert!(err.to_string().contains("not built: ay-lsp"), "{err}");
}

/// A shell is never shimmed, so a pack exposing `fish` is refused: before
/// anything is built when the declaration names it, before anything is staged when the
/// name-token fallback picks it, and nothing is tarred or signed either way.
#[test]
fn a_pack_exposing_fish_is_refused_and_the_same_build_exposing_tools_packs() {
    let w = world("pack-fish");
    let bins = || {
        vec![
            ("ay", bin_bytes(elf(EM_X86_64, 2, 1), "ay x86_64")),
            ("fish", bin_bytes(elf(EM_X86_64, 2, 1), "fish x86_64")),
        ]
    };
    let nothing_written = |w: &World| {
        assert!(w.written(".toml").is_empty(), "{:?}", w.written(".toml"));
        assert!(w.written(".tar.zst").is_empty());
        assert!(w.written(".sig").is_empty());
    };

    let fish = metadata_json(&w.checkout, &["ay", "fish"], &[], &[], false);
    let t = tools(&w, "aarch64-apple-darwin", fish, bins());
    let err = pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t)
        .expect_err("a declared shell");
    assert!(err.to_string().contains("ay exposes `fish`"), "{err}");
    assert!(t.runs_of("build").is_empty(), "refused before the build");
    nothing_written(&w);

    let undeclared = metadata_json(&w.checkout, &[], &[], &[], false);
    let t = tools(&w, "aarch64-apple-darwin", undeclared, bins());
    let err = pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t)
        .expect_err("a shell the fallback picked");
    assert!(err.to_string().contains("ay exposes `fish`"), "{err}");
    assert_eq!(
        t.runs_of("build").len(),
        1,
        "the fallback picks after the build"
    );
    nothing_written(&w);

    let t = tools(&w, "aarch64-apple-darwin", declared(&w), bins());
    let out =
        pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t).expect("tool names pack");
    assert_eq!(out.exposes, vec!["ay"]);
    let sig = out.signature.clone().expect("signed");
    let parsed = client_verifies(&w.ctx, &out.manifest, &sig).expect("the client accepts it");
    assert_eq!(parsed.exposes, vec!["ay"]);
}

#[test]
fn the_declared_bundle_ships_unshimmed_and_build_args_reach_the_build() {
    let w = world("pack-bundle-decl");
    let meta = metadata_json(
        &w.checkout,
        &["ay"],
        &["ay-helper"],
        &["-p", "ay", "--features", "cli"],
        false,
    );
    let bins = vec![
        ("ay", bin_bytes(elf(EM_X86_64, 2, 1), "ay")),
        ("ay-helper", bin_bytes(elf(EM_X86_64, 2, 1), "helper")),
        ("ay-bench", bin_bytes(elf(EM_X86_64, 2, 1), "bench")),
    ];
    let t = tools(&w, "aarch64-apple-darwin", meta, bins);
    let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
    req.signing = Signing::Unsigned;
    let out = pack::pack_with(&w.ctx, &req, &t).expect("packed");
    assert_eq!(out.exposes, vec!["ay"]);
    assert!(out.signature.is_none());
    let stage = w.root.join("out/ay/root/bin");
    assert!(stage.join("ay-helper").is_file(), "bundled, not shimmed");
    assert!(
        !stage.join("ay-bench").exists(),
        "built, not declared: dropped"
    );
    let build = &t.runs_of("build")[0];
    assert!(
        build
            .argv()
            .windows(4)
            .any(|w| w == ["-p", "ay", "--features", "cli"]),
        "{:?}",
        build.argv()
    );
}

#[test]
fn with_no_declaration_the_name_token_fallback_drops_internal_bins() {
    for (name, internal) in [
        ("ay", false),
        ("ay-lsp", false),
        ("ay-bench", true),
        ("gen-tables", true),
        ("schema_gen", true),
        ("gen", false),
        ("trust-ir-tests", true),
        ("contest", false),
        ("Mock_Server", true),
    ] {
        assert_eq!(pack::looks_internal(name), internal, "{name}");
    }
    let meta = Metadata::default();
    let exes = [PathBuf::from("/b/ay"), PathBuf::from("/b/ay-fixture")];
    let sel = pack::select(&meta, &exes, X86_64_UNKNOWN_LINUX_GNU).expect("select");
    assert_eq!(sel.mode, ExposeMode::Heuristic);
    assert_eq!(sel.exposes, vec!["ay"]);
    assert_eq!(sel.dropped, vec!["ay-fixture"]);
    let none = pack::select(
        &meta,
        &[PathBuf::from("/b/ay-test")],
        X86_64_UNKNOWN_LINUX_GNU,
    );
    assert!(none.is_err(), "an empty exposed set is refused");
    let win = Triple::parse("x86_64-pc-windows-msvc").expect("t");
    let declared = Metadata {
        expose: vec!["ay".into()],
        ..Metadata::default()
    };
    let sel = pack::select(&declared, &[PathBuf::from("/b/ay.exe")], win).expect("windows");
    assert_eq!(sel.exposes, vec!["ay"]);
    assert_eq!(sel.shipped[0].1, "ay.exe");
}

#[test]
fn a_failed_build_is_refused_quoting_its_tail() {
    let w = world("pack-build-fail");
    let mut t = FakeTools::new("aarch64-apple-darwin");
    t.answer = cargo(&w.checkout, declared(&w), linux_ay(), true);
    let err = pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t)
        .expect_err("the build failed");
    let text = err.to_string();
    assert!(
        text.contains("build FAILED") && text.contains("could not compile `ay`"),
        "{text}"
    );
}

#[test]
fn a_payload_carrying_a_stock_std_path_is_refused_before_anything_is_tarred() {
    let w = world("pack-scan");
    let stock = bin_bytes(
        elf(EM_X86_64, 2, 1),
        "/rustc/8bab26f4f68e0e26f0bb7960be334d5b520ea452/library/std/src/panicking.rs",
    );
    let t = tools(
        &w,
        "aarch64-apple-darwin",
        declared(&w),
        vec![("ay", stock)],
    );
    let err = pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t)
        .expect_err("stock-built bytes");
    assert!(
        err.to_string()
            .contains("bin/ay: 1× /rustc/8bab26f4f68e0e26f0bb7960be334d5b520ea452/"),
        "{err}"
    );
    assert!(w.written(".tar.zst").is_empty());
    assert!(w.written(".toml").is_empty());
}

#[test]
fn a_live_switch_refuses_a_receiptless_compiler_before_the_checkout_is_read() {
    let w = world("pack-live");
    std::fs::remove_dir_all(w.ctx.home.join("trust")).expect("rm");
    std::fs::remove_dir_all(w.ctx.home.join("trust.git")).expect("rm");
    trust_repo(&w.ctx.home.join("trust"), MINTED_LEDGER);
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
    let err = pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t)
        .expect_err("no receipt, switch live");
    assert!(
        err.to_string()
            .contains("carries neither LINEAGE nor SEED-MINT"),
        "{err}"
    );
    assert!(t.runs_of("metadata").is_empty(), "nothing read or built");

    let genesis = w.root.join("genesis");
    fixture_toolchain(&genesis);
    put_exe(
        &genesis.join("bin/trustc"),
        b"#!/bin/sh\nexec \"$HOME/.cargo/bin/rustc\" \"$@\"\n",
    );
    let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
    req.toolchain = Some(genesis.join("bin"));
    let err = pack::pack_with(&w.ctx, &req, &t).expect_err("a genesis-style wrapper");
    assert!(err.to_string().contains("interpreter script"), "{err}");
}

#[test]
fn an_unproven_row_is_not_signed_once_the_mint_lands_mid_pack() {
    let w = world("pack-mint-mid-pack");
    let bare = w.ctx.home.join("trust.git");
    let pusher = w.root.join("minter");
    std::fs::create_dir_all(&pusher).expect("mkdir");
    let inner = cargo(&w.checkout, declared(&w), linux_ay(), false);
    let mut t = FakeTools::new("aarch64-apple-darwin");
    t.answer = Box::new(move |run| {
        if run.argv().iter().any(|a| a == "build") {
            push_from_elsewhere(&bare, &pusher, Some(MINTED_LEDGER));
        }
        inner(run)
    });
    let err = pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t)
        .expect_err("the switch flipped during the build");
    let text = err.to_string();
    assert!(
        text.contains("lineage=unproven") && text.contains("receipt-carrying compiler"),
        "{text}"
    );
    assert!(w.written(".toml").is_empty(), "nothing signed or staged");
}

#[test]
fn a_merge_extends_the_first_targets_manifest_for_the_second() {
    let w = world("pack-merge");
    let arm = vec![("ay", bin_bytes(macho(CPU_ARM64), "ay arm64"))];
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), arm);
    let first = pack::pack_with(&w.ctx, &w.request(AARCH64_APPLE_DARWIN), &t).expect("arm");
    assert_eq!(
        first.asset.file_name().and_then(|n| n.to_str()),
        Some("ay-9.tar.zst"),
        "the historical bare name"
    );
    let base = w.root.join("base.toml");
    std::fs::copy(&first.manifest, &base).expect("copy the base");
    let base_bytes = std::fs::read(&base).expect("base");

    let x86 = vec![("ay", bin_bytes(macho(CPU_X86_64), "ay x86_64"))];
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), x86);
    let mut req = w.request(X86_64_APPLE_DARWIN);
    req.merge_from = Some(base);
    let second = pack::pack_with(&w.ctx, &req, &t).expect("x86_64, merged");
    let bytes = std::fs::read(&second.manifest).expect("pkg");
    assert!(bytes.starts_with(&base_bytes), "the base is a byte prefix");
    let parsed = client_verifies(&w.ctx, &second.manifest, &second.signature.expect("signed"))
        .expect("the merged pair verifies");
    let targets: Vec<&str> = parsed.artifacts.iter().map(|a| a.target.as_str()).collect();
    assert_eq!(targets, ["aarch64-apple-darwin", "x86_64-apple-darwin"]);
}

#[test]
fn an_upload_stages_a_prerelease_and_a_public_pack_is_immutable() {
    let w = world("pack-upload");
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
    let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
    req.upload = true;
    let out = pack::pack_with(&w.ctx, &req, &t).expect("uploaded");
    assert!(out.uploaded);
    let staging = Slug::new("alabsystems", "ay");
    let rel = w.host.release_of(&staging, "atpkg-ay-9").expect("staged");
    assert!(rel.prerelease && !rel.draft);
    let names: Vec<&str> = rel.assets.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "ay-9-x86_64-unknown-linux-gnu.tar.zst",
            "pkg-ay-9.toml",
            "pkg-ay-9.toml.sig"
        ]
    );
    pack::pack_with(&w.ctx, &req, &t).expect("converged");

    let public = Slug::new("alabsystems", "ay");
    let pkg = std::fs::read(&out.manifest).expect("pkg");
    w.host
        .put_asset(&public, "atpkg-ay-9", "pkg-ay-9.toml", pkg.clone());
    let again = pack::pack_with(&w.ctx, &req, &t).expect("already public");
    assert!(!again.uploaded);
    w.host.put(
        &public,
        "atpkg-ay-9",
        FakeRelease {
            prerelease: true,
            assets: [("pkg-ay-9.toml".to_string(), b"other bytes".to_vec())].into(),
            ..FakeRelease::default()
        },
    );
    let before = w.host.release_of(&staging, "atpkg-ay-9");
    let err = pack::pack_with(&w.ctx, &req, &t).expect_err("immutable");
    assert!(
        err.to_string().contains("a public pack is immutable"),
        "{err}"
    );
    assert_eq!(w.host.release_of(&staging, "atpkg-ay-9"), before);
}

#[test]
fn nothing_unsigned_publishes_and_an_unsigned_pack_leaves_no_signature() {
    let w = world("pack-unsigned");
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
    let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
    req.signing = Signing::Unsigned;
    req.upload = true;
    assert!(matches!(
        pack::pack_with(&w.ctx, &req, &t),
        Err(Error::Usage(_))
    ));
    req.upload = false;
    let stale = w.root.join("out/ay/pkg-ay-9.toml.sig");
    let out = pack::pack_with(&w.ctx, &req, &t).expect("staged unsigned");
    assert!(out.signature.is_none());
    assert!(!stale.exists());
    sign::sign_all(&w.ctx, std::slice::from_ref(&out.manifest))
        .expect("the ceremony signs what the keyless pack staged");

    for (program, why) in [("trust", "a bundle"), ("nope", "unknown")] {
        let mut r = w.request(X86_64_UNKNOWN_LINUX_GNU);
        r.program = program.into();
        assert!(
            matches!(pack::pack_with(&w.ctx, &r, &t), Err(Error::Usage(_))),
            "{why}"
        );
    }
}

#[test]
fn the_pack_lock_serializes_publishes_of_one_program() {
    let w = world("pack-lock");
    let held = pack::pack_lock(&w.ctx, "ay", Duration::ZERO).expect("free");
    let Err(err) = pack::pack_lock(&w.ctx, "ay", Duration::ZERO) else {
        panic!("a held lock was taken twice");
    };
    assert!(err.to_string().contains("another publish of ay"), "{err}");
    pack::pack_lock(&w.ctx, "ny", Duration::ZERO).expect("another program's is its own");
    drop(held);
    pack::pack_lock(&w.ctx, "ay", Duration::ZERO).expect("released with its holder");
}

#[test]
fn preflight_refuses_bytes_that_no_longer_match_the_signed_row() {
    let tmp = scratch("pack-preflight");
    let asset = tmp.path().join("ay-9.tar.zst");
    put(&asset, b"the packed bytes");
    let sha = atpkg::sha256_file(&asset).expect("hash");
    upload::preflight(&asset, &sha, &[]).expect("matches");
    put(&asset, b"a rebuild raced the upload");
    let err = upload::preflight(&asset, &sha, &[]).expect_err("raced");
    assert!(
        err.to_string().contains("do not match the signed manifest"),
        "{err}"
    );
    assert_eq!(upload::tag("ay", 9), "atpkg-ay-9");
    assert_eq!(pkgdoc::manifest_name("ay", 9), "pkg-ay-9.toml");
}

#[test]
fn a_receipt_carrying_compiler_packs_under_a_live_switch_and_says_so() {
    let w = world("pack-seed-mint");
    std::fs::remove_dir_all(w.ctx.home.join("trust")).expect("rm");
    std::fs::remove_dir_all(w.ctx.home.join("trust.git")).expect("rm");
    trust_repo(&w.ctx.home.join("trust"), MINTED_LEDGER);
    seed_mint(
        w.bin.parent().expect("the toolchain root"),
        super::fixture::COMMIT,
    );
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
    let out = pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t).expect("packed");
    assert_eq!(out.lineage, Lineage::SeedMint);
    let text = std::fs::read_to_string(&out.manifest).expect("pkg");
    assert!(
        text.contains("lineage = \"seed-mint\"")
            && text.contains("ddc_seed_relation = \"pin-ancestor\""),
        "{text}"
    );
}

#[test]
fn the_switch_is_read_from_the_trust_repo_the_context_names_start_and_finish() {
    let w = world("pack-trust-repo");
    std::fs::remove_dir_all(w.ctx.home.join("trust")).expect("rm");
    std::fs::remove_dir_all(w.ctx.home.join("trust.git")).expect("rm");
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
    let err = pack::pack_with(&w.ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t)
        .expect_err("no $HOME/trust: the receipt is required");
    let text = err.to_string();
    assert!(
        text.contains("carries neither LINEAGE nor SEED-MINT")
            && text.contains("there is no trust checkout at $HOME/trust"),
        "the refusal names why the receipt was required: {text}"
    );

    let mut ctx = w.ctx.clone();
    ctx.trust_repo = w.root.join("src/trust");
    trust_repo(&ctx.trust_repo, DORMANT_LEDGER);
    let out = pack::pack_with(&ctx, &w.request(X86_64_UNKNOWN_LINUX_GNU), &t)
        .expect("dormant where the context points");
    assert_eq!(out.lineage, Lineage::Unproven);
    assert!(out.signature.is_some());
}

#[test]
fn a_caller_adds_a_cross_linker_only_and_never_outranks_the_seals_compiler() {
    let w = world("pack-env");
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
    for key in [
        "RUSTFLAGS",
        "RUSTC",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
        "CARGO_TARGET__LINKER",
    ] {
        let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
        req.env = vec![(key.into(), "-C target-cpu=native".into())];
        assert!(
            matches!(pack::pack_with(&w.ctx, &req, &t), Err(Error::Usage(_))),
            "{key}"
        );
    }
    assert!(t.runs_of("build").is_empty(), "refused before any build");

    let linker = "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER";
    let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
    req.env = vec![(linker.into(), "x86_64-linux-gnu-gcc".into())];
    pack::pack_with(&w.ctx, &req, &t).expect("a cross linker is the caller's to name");
    let build = &t.runs_of("build")[0];
    assert_eq!(
        build
            .env_of(linker)
            .map(|v| v.to_string_lossy().into_owned()),
        Some("x86_64-linux-gnu-gcc".into())
    );
    let at = |k: &str| build.env.iter().position(|(n, _)| n == k).expect(k);
    assert!(
        at(linker) < at("RUSTC") && at(linker) < at("RUSTFLAGS"),
        "set last"
    );
    for scrubbed in [
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTUP_TOOLCHAIN",
    ] {
        assert!(
            build.env_remove.iter().any(|k| k == scrubbed),
            "{scrubbed} is removed from the inherited environment"
        );
    }
}

#[test]
fn an_out_whose_stage_holds_the_checkout_is_refused_and_the_checkout_survives() {
    let w = world("pack-out-holds-checkout");
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
    let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
    req.out = w.root.clone();
    let err = pack::pack_with(&w.ctx, &req, &t).expect_err("would delete the checkout");
    assert!(err.to_string().contains("holds the checkout"), "{err}");
    assert!(w.checkout.join("Cargo.toml").is_file());
    assert!(w.checkout.join(".git").is_dir());
}

/// A `--upload` pack on a box without the staging token is refused before it builds:
/// its uploads never go anonymously.
#[test]
fn an_upload_without_the_staging_token_is_refused_before_anything_is_built() {
    let tmp = scratch("token-preflight");
    let ctx = Keys::mint("m3", &[]).ctx(tmp.path(), Arc::new(FakeHost::new()));
    let e = upload::token_preflight(&ctx).expect_err("no token file");
    assert!(
        e.to_string()
            .contains("--upload needs the staging owner's token at ~/.secrets/"),
        "{e}"
    );
    std::fs::create_dir_all(ctx.tokens.staging.parent().expect("dir")).expect("mkdir");
    std::fs::write(&ctx.tokens.staging, "").expect("empty");
    assert!(
        upload::token_preflight(&ctx).is_err(),
        "an empty file is no token"
    );
    std::fs::write(&ctx.tokens.staging, "t0ken\n").expect("token");
    upload::token_preflight(&ctx).expect("a token file");
}

/// A harness checkout at `w.root/<dir>` with no origin: one commit, `/target/` ignored, as
/// the five harness repos are.
fn harness_tree(w: &World, dir: &str) -> PathBuf {
    let dir = w.root.join(dir);
    fixture_checkout(
        &dir,
        &[
            (".gitignore", b"/target/\n"),
            ("Cargo.toml", b"[package]\nname = \"harness\"\n"),
        ],
    );
    dir
}

/// Give `checkout` the origin `w.root/remotes/<owner>/<repo>.git`, a bare it pushes its
/// `HEAD` to as `branch` and then fetches.
fn harness_origin(w: &World, checkout: &Path, owner: &str, repo: &str, branch: &str) {
    let bare = w.root.join(format!("remotes/{owner}/{repo}.git"));
    if !bare.exists() {
        bare_origin(&bare);
    }
    let git = |args: &[&str]| super::fixture::git(checkout, args);
    if git(&["remote"]).is_empty() {
        git(&["remote", "add", "origin", &bare.to_string_lossy()]);
    }
    git(&["push", "-q", "origin", &format!("HEAD:refs/heads/{branch}")]);
    git(&["fetch", "-q", "origin"]);
}

/// A harness checkout at `w.root/<name>` as a roster machine clones it: one commit, on the
/// `main` of its origin `alabsystems/<name>` (the staging owner's repo).
fn harness_checkout(w: &World, name: &str) -> PathBuf {
    let dir = harness_tree(w, name);
    harness_origin(w, &dir, "alabsystems", name, "main");
    dir
}

/// `targo metadata` JSON for a one-package harness workspace: package `package` at
/// `version`, declaring `expose` and `bundle` (no `[workspace.metadata.atpkg]` when both are
/// empty).
fn harness_metadata_json(
    checkout: &Path,
    package: &str,
    version: &str,
    expose: &[&str],
    bundle: &[&str],
) -> String {
    let list = |v: &[&str]| {
        v.iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(",")
    };
    let atpkg = if expose.is_empty() && bundle.is_empty() {
        "null".to_string()
    } else {
        format!(
            "{{\"atpkg\":{{\"expose\":[{}],\"bundle\":[{}]}}}}",
            list(expose),
            list(bundle)
        )
    };
    format!(
        "{{\"packages\":[{{\"name\":\"{package}\",\"version\":\"{version}\",\
         \"manifest_path\":\"{}\"}}],\"metadata\":{atpkg}}}",
        checkout.join("Cargo.toml").display()
    )
}

fn harness_request(w: &World, program: &str, checkout: &Path) -> PackRequest {
    PackRequest {
        program: program.into(),
        checkout: checkout.to_path_buf(),
        build: Some(1),
        ..w.request(AARCH64_APPLE_DARWIN)
    }
}

fn arm_bins(names: &[&'static str]) -> Vec<(&'static str, Vec<u8>)> {
    names
        .iter()
        .map(|n| (*n, bin_bytes(macho(CPU_ARM64), n)))
        .collect()
}

fn harness_tools(checkout: &Path, meta: String, bins: Vec<(&'static str, Vec<u8>)>) -> FakeTools {
    let mut t = FakeTools::new("aarch64-apple-darwin");
    t.answer = cargo(checkout, meta, bins, false);
    t
}

/// A HARNESS PACK SHIPS ITS ONE TOOL, CALLED BY ITS RELEASE LINE AND THE COMMIT BUILT: the
/// repo declares `muse-alab`, a second bin it builds is not shipped, the build is `--locked`,
/// and the signed manifest the client verifies says `0.1.0+g<9-hex HEAD>` at build 1 (the
/// checkout's one commit).
#[test]
fn a_harness_pack_ships_its_one_tool_at_its_release_line_and_the_commit_built() {
    let w = world("pack-harness");
    let co = harness_checkout(&w, "muse-alab");
    let meta = harness_metadata_json(&co, "muse-alab", "0.1.0", &["muse-alab"], &[]);
    let t = harness_tools(&co, meta, arm_bins(&["muse-alab", "muse-alab-stand-in"]));
    let out = pack::pack_with(&w.ctx, &harness_request(&w, "muse-alab", &co), &t)
        .expect("a harness packs");
    let sha = super::fixture::git(&co, &["rev-parse", "--short=9", "HEAD"]);
    assert_eq!(sha.len(), 9);
    assert_eq!(out.version, format!("0.1.0+g{sha}"));
    assert_eq!(
        (out.build, out.exposes.clone()),
        (1, vec!["muse-alab".to_string()])
    );
    let parsed = client_verifies(
        &w.ctx,
        &out.manifest,
        &out.signature.clone().expect("signed"),
    )
    .expect("the client accepts it");
    assert_eq!(parsed.program, "muse-alab");
    assert_eq!(parsed.version, format!("0.1.0+g{sha}"));
    assert_eq!(parsed.exposes, vec!["muse-alab"]);
    assert_eq!(parsed.artifacts.len(), 1);
    assert_eq!(parsed.artifacts[0].target, "aarch64-apple-darwin");
    let staged: Vec<String> = std::fs::read_dir(w.root.join("out/muse-alab/root/bin"))
        .expect("staged bin/")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(staged, ["muse-alab"], "only the tool ships");
    assert!(
        t.runs_of("build")[0].argv().iter().any(|a| a == "--locked"),
        "a harness builds the lock it committed: {:?}",
        t.runs_of("build")[0].argv()
    );
}

/// A HARNESS REPO THAT DECLARES NO `expose` SHIPS ITS ROW'S TOOL, never the name-token
/// fallback's pick: kimi-alab declares none, and the helper bin its build also makes (which
/// the fallback would ship and shim) stays out.
#[test]
fn a_harness_repo_declaring_no_expose_ships_its_rows_tool_and_nothing_else() {
    let w = world("pack-harness-undeclared");
    let co = harness_checkout(&w, "kimi-alab");
    let meta = harness_metadata_json(&co, "kimi-alab", "0.1.0", &[], &[]);
    let t = harness_tools(&co, meta, arm_bins(&["kimi-alab", "kimi-alab-helper"]));
    let out = pack::pack_with(&w.ctx, &harness_request(&w, "kimi-alab", &co), &t)
        .expect("the row's tool packs");
    assert_eq!(out.exposes, vec!["kimi-alab"]);
    assert!(w.root.join("out/kimi-alab/root/bin/kimi-alab").is_file());
    assert!(
        !w.root
            .join("out/kimi-alab/root/bin/kimi-alab-helper")
            .exists(),
        "built, not the row's tool: dropped"
    );
    let parsed = client_verifies(
        &w.ctx,
        &out.manifest,
        &out.signature.clone().expect("signed"),
    )
    .expect("the client accepts it");
    assert_eq!(parsed.exposes, vec!["kimi-alab"]);
}

/// A HARNESS PACK REFUSES, BEFORE ANYTHING IS BUILT OR WRITTEN: an `expose` naming anything
/// but the row's tool (the vendor's own name, or a second tool), a `bundle`, a version that
/// is not `MAJOR.MINOR.0`, no program-named package, a build number that is not the
/// checkout's HEAD count, and a checkout with an untracked file.
#[test]
fn a_harness_pack_refuses_every_departure_from_its_rule_before_the_build() {
    let w = world("pack-harness-refusals");
    let co = harness_checkout(&w, "muse-alab");
    let refuses = |meta: String, req: PackRequest, says: &str| {
        let t = harness_tools(&co, meta, arm_bins(&["muse-alab"]));
        let err = pack::pack_with(&w.ctx, &req, &t).expect_err(says);
        assert!(err.to_string().contains(says), "{says}: {err}");
        assert!(
            t.runs_of("build").is_empty(),
            "{says}: refused before the build"
        );
        assert!(w.written(".toml").is_empty() && w.written(".tar.zst").is_empty());
    };
    let req = || harness_request(&w, "muse-alab", &co);
    let meta = |package: &str, version: &str, expose: &[&str], bundle: &[&str]| {
        harness_metadata_json(&co, package, version, expose, bundle)
    };
    refuses(
        meta("muse-alab", "0.1.0", &["muse"], &[]),
        req(),
        "expose names muse, and its harness row (programs::PROGRAMS) ships exactly `muse-alab`",
    );
    refuses(
        meta("muse-alab", "0.1.0", &["muse-alab", "muse-alab-lsp"], &[]),
        req(),
        "ships exactly `muse-alab`",
    );
    refuses(
        meta("muse-alab", "0.1.0", &["muse-alab"], &["muse-alab-helper"]),
        req(),
        "bundles muse-alab-helper besides",
    );
    for v in [
        "0.1.1",
        "0.1",
        "0.1.0-dev.3+gabcdef012",
        "0.1.0+gabcdef012",
        "00.1.0",
    ] {
        refuses(
            meta("muse-alab", v, &["muse-alab"], &[]),
            req(),
            &format!("version `{v}` is not MAJOR.MINOR.0"),
        );
    }
    refuses(
        meta("muse-alab-core", "0.1.0", &["muse-alab"], &[]),
        req(),
        "has no package named muse-alab",
    );
    let mut later = req();
    later.build = Some(9);
    refuses(
        meta("muse-alab", "0.1.0", &["muse-alab"], &[]),
        later,
        "build 9 is not the checkout's HEAD (it counts 1 commits)",
    );
    std::fs::write(co.join("notes.txt"), "uncommitted").expect("untracked");
    refuses(
        meta("muse-alab", "0.1.0", &["muse-alab"], &[]),
        req(),
        "has changes or untracked files",
    );
}

/// A HARNESS IS PACKED ONLY FROM ITS REPO'S MAIN, as codex's package is
/// (`package::check_on_main`): a clean checkout at its own commit count is still refused,
/// before anything is built or written, when it has no origin, when its origin is a fork or
/// another harness's repo, or when its `HEAD` is a commit main does not hold (unpushed, or
/// pushed to a branch only). Each would mint a signed `0.1.0+g<sha>` at a build number main
/// never gives that commit. The same checkout packs once main holds its `HEAD`.
#[test]
fn a_harness_pack_is_minted_only_from_its_repos_main() {
    let w = world("pack-harness-source");
    let meta = |co: &Path| harness_metadata_json(co, "muse-alab", "0.1.0", &["muse-alab"], &[]);
    let refuses = |co: &Path, build: u64, says: &str| {
        let t = harness_tools(co, meta(co), arm_bins(&["muse-alab"]));
        let mut req = harness_request(&w, "muse-alab", co);
        req.build = Some(build);
        let err = pack::pack_with(&w.ctx, &req, &t).expect_err(says);
        assert!(err.to_string().contains(says), "{says}: {err}");
        assert!(
            t.runs_of("build").is_empty(),
            "{says}: refused before the build"
        );
        assert!(w.written(".toml").is_empty() && w.written(".tar.zst").is_empty());
    };

    let orphan = harness_tree(&w, "orphan/muse-alab");
    refuses(&orphan, 1, "has no origin to read");

    let fork = harness_tree(&w, "fork/muse-alab");
    harness_origin(&w, &fork, "someone", "muse-alab", "main");
    refuses(&fork, 1, "not alabsystems/muse-alab");

    let other = harness_tree(&w, "other/muse-alab");
    harness_origin(&w, &other, "alabsystems", "agy-alab", "main");
    refuses(&other, 1, "not alabsystems/muse-alab");

    let ahead = harness_checkout(&w, "muse-alab");
    put(&ahead.join("README"), b"a commit main does not hold\n");
    super::fixture::git(&ahead, &["add", "-A"]);
    super::fixture::git(&ahead, &["commit", "-q", "-m", "local"]);
    refuses(&ahead, 2, "is not on alabsystems/muse-alab origin/main");
    harness_origin(&w, &ahead, "alabsystems", "muse-alab", "feature");
    refuses(&ahead, 2, "is not on alabsystems/muse-alab origin/main");

    harness_origin(&w, &ahead, "alabsystems", "muse-alab", "main");
    let t = harness_tools(&ahead, meta(&ahead), arm_bins(&["muse-alab"]));
    let mut req = harness_request(&w, "muse-alab", &ahead);
    req.build = Some(2);
    let out = pack::pack_with(&w.ctx, &req, &t).expect("main holds HEAD: it packs");
    let sha = super::fixture::git(&ahead, &["rev-parse", "--short=9", "HEAD"]);
    assert_eq!((out.build, out.version), (2, format!("0.1.0+g{sha}")));
}

#[test]
fn a_release_line_is_major_minor_zero_and_a_dev_version_adds_the_short_sha() {
    for (v, ok) in [
        ("0.1.0", true),
        ("1.20.0", true),
        ("0.0.0", true),
        ("0.1.1", false),
        ("0.1", false),
        ("0.1.0.0", false),
        ("01.1.0", false),
        ("0.01.0", false),
        ("0.1.00", false),
        ("v0.1.0", false),
        ("0.1.0-dev.1", false),
        ("0.1.0+g123456789", false),
        ("", false),
        ("0..0", false),
    ] {
        assert_eq!(pack::is_release_line(v), ok, "{v:?}");
    }
    assert_eq!(
        pack::dev_version("0.1.0", "abcdef012").expect("dev"),
        "0.1.0+gabcdef012"
    );
    assert!(
        pack::dev_version("0.1.0", "abcdef01").is_err(),
        "8 hex is short"
    );
    assert!(
        pack::dev_version("0.1.0", "ABCDEF012").is_err(),
        "lowercase only"
    );
    assert!(pack::dev_version("0.1.2", "abcdef012").is_err());
}
