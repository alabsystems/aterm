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
use atpkg_publish::host::fake::{FakeHost, FakeRelease};
use atpkg_publish::host::rest::STAGED;
use atpkg_publish::host::{ReleaseHost as _, Slug};
use atpkg_publish::testkit::Keys;
use atpkg_publish::triple::{
    AARCH64_APPLE_DARWIN, Triple, X86_64_APPLE_DARWIN, X86_64_UNKNOWN_LINUX_GNU,
};

use super::fixture::{
    Answer, CPU_ARM64, CPU_X86_64, DORMANT_LEDGER, EM_AARCH64, EM_X86_64, FakeTools, MINTED_LEDGER,
    bare_origin, checkout as fixture_checkout, client_verifies, elf, macho, push_from_elsewhere,
    put, put_exe, ran, scratch, seed_mint, toolchain as fixture_toolchain, trust_repo,
};

#[path = "pack_build_diagnostics.rs"]
mod build_diagnostics;
#[path = "pack_upload_race.rs"]
mod upload_race;

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
            .map(|s| aterm_json::to_string(s).expect("metadata string"))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "{{\"packages\":[{{\"name\":\"ay\",\"version\":\"0.2.0\",\"manifest_path\":{}}}],\
         \"metadata\":{{\"atpkg\":{{\"expose\":[{}],\"bundle\":[{}],\"build-args\":[{}],\
         \"bundle-only\":{only}}}}}}}",
        aterm_json::to_string(&checkout.join("Cargo.toml").to_string_lossy().as_ref())
            .expect("manifest path json"),
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
        if argv.windows(2).any(|args| args == ["config", "get"]) {
            return Some(ran(0, "{}", ""));
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
             {{\"reason\":\"compiler-artifact\",\"manifest_path\":\"{}\",\"target\":{{\"kind\":\
             [\"bin\"],\"name\":\"test-bin\"}},\"profile\":{{\"test\":true}},\
             \"executable\":\"/elsewhere/dep/test-bin\"}}\n\
             {{\"reason\":\"compiler-artifact\",\"manifest_path\":\"/elsewhere/dep/Cargo.toml\",\
             \"target\":{{\"kind\":[\"bin\"]}},\"executable\":\"/elsewhere/dep/tool\"}}\n\
             {{\"reason\":\"build-finished\",\"success\":true}}\n",
            manifest.display(),
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

fn config_answer(t: &mut FakeTools, json: &str, exit: i32) {
    let answer = std::mem::replace(&mut t.answer, Box::new(|_| None));
    let json = json.to_string();
    t.answer = Box::new(move |run| {
        if run.argv().windows(2).any(|args| args == ["config", "get"]) {
            Some(ran(exit, &json, "configuration error"))
        } else {
            answer(run)
        }
    });
}

fn build_configs(run: &atpkg_publish::artifact::tools::Run) -> Vec<String> {
    run.argv()
        .windows(2)
        .filter(|pair| pair[0] == "--config")
        .map(|pair| pair[1].clone())
        .collect()
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
fn a_standalone_nested_workspace_packs_its_declared_serial_command() {
    let w = world("pack-serial-command");
    let checkout = w.checkout.join("crates/trust-resource-controller");
    put(
        &checkout.join("Cargo.toml"),
        b"[workspace]\n[package]\nname='trust-resource-controller'\nversion='0.13.0'\n\
          [package.metadata.atpkg]\nexpose=['trust-serial-command']\n\
          build-args=['--locked','--features','serial-command','--bin','trust-serial-command']\n",
    );
    let args = [
        "--locked",
        "--features",
        "serial-command",
        "--bin",
        "trust-serial-command",
    ];
    let meta = aterm_json::json!({
        "packages": [{
            "name": "trust-resource-controller",
            "version": "0.13.0",
            "manifest_path": checkout.join("Cargo.toml").to_string_lossy().into_owned(),
            "metadata": {"atpkg": {
                "expose": ["trust-serial-command"],
                "build-args": args,
            }},
        }],
        "metadata": {},
    });
    let mut t = FakeTools::new("aarch64-apple-darwin");
    t.answer = cargo(
        &checkout,
        aterm_json::to_string(&meta).expect("metadata"),
        vec![
            (
                "trust-serial-command",
                bin_bytes(macho(CPU_ARM64), "serial command"),
            ),
            (
                "internal-probe",
                bin_bytes(macho(CPU_ARM64), "unshipped probe"),
            ),
        ],
        false,
    );
    let mut req = w.request(AARCH64_APPLE_DARWIN);
    req.program = "trust-resource-controller".into();
    req.checkout = checkout.clone();
    req.build = None;
    let out = pack::pack_with(&w.ctx, &req, &t).expect("the standalone crate packs");
    assert_eq!(
        out.build,
        atpkg_publish::git::rev_count(&w.checkout, "HEAD").expect("count")
    );
    assert_eq!(out.version, "0.13.0");
    assert_eq!(out.exposes, ["trust-serial-command"]);
    let row = atpkg_publish::programs::get("trust-resource-controller").expect("row");
    assert_eq!(row.repo, "trust");
    assert!(row.group.is_none());
    assert!(row.is_plain());
    for verb in ["metadata", "config", "build"] {
        let runs = t.runs_of(verb);
        assert!(!runs.is_empty(), "{verb} ran");
        assert!(
            runs.iter()
                .all(|run| run.cwd.as_deref() == Some(checkout.as_path()))
        );
    }
    let build = &t.runs_of("build")[0];
    assert_eq!(&build.argv()[3..3 + args.len()], args);
    assert!(
        !build
            .argv()
            .iter()
            .any(|arg| arg == "--manifest-path" || arg == "--target")
    );
    assert!(build.env_of("RUSTFLAGS").is_none());
    client_verifies(
        &w.ctx,
        &out.manifest,
        out.signature.as_ref().expect("signature"),
    )
    .expect("the existing client verifies the package");
    let bin = w.root.join("out/trust-resource-controller/root/bin");
    assert!(bin.join("trust-serial-command").is_file());
    assert!(!bin.join("internal-probe").exists());
}

#[test]
fn native_pack_preserves_repository_policy_and_appends_its_distribution_floor() {
    let w = world("pack-native-policy");
    let mut t = tools(
        &w,
        "aarch64-apple-darwin",
        metadata_json(
            &w.checkout,
            &["ay"],
            &[],
            &[
                "--config",
                "target.aarch64-apple-darwin.rustflags=[\"--cfg\",\"declared_policy\",\"-C\",\"target-cpu=native\"]",
            ],
            false,
        ),
        vec![("ay", bin_bytes(macho(CPU_ARM64), "ay arm64"))],
    );
    config_answer(
        &mut t,
        r#"{"target":{"aarch64-apple-darwin":{"rustflags":["--cfg","declared_policy","-C","target-cpu=native"]},"cfg(all())":{"rustflags":["--cfg","cfg_policy","-C","target-cpu=native"]}},"host":{"rustflags":["--cfg","host_policy"]}}"#,
        0,
    );
    let out = pack::pack_with(&w.ctx, &w.request(AARCH64_APPLE_DARWIN), &t).expect("packed");
    client_verifies(
        &w.ctx,
        &out.manifest,
        out.signature.as_ref().expect("signature"),
    )
    .expect("native manifest and artifact pair");
    let build = &t.runs_of("build")[0];
    assert!(!build.argv().iter().any(|arg| arg == "--target"));
    assert!(build.env_of("RUSTFLAGS").is_none());
    for key in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
    ] {
        assert!(build.env_remove.iter().any(|removed| removed == key));
    }
    let configs = build_configs(build);
    assert_eq!(
        configs.len(),
        5,
        "declared policy, two target segments, fallback and host"
    );
    assert!(configs[0].contains("declared_policy"));
    for config in &configs[1..] {
        let (_, array) = config.split_once('=').expect("Cargo configuration");
        let flags: aterm_json::Value = aterm_json::from_str(array).expect("TOML-compatible array");
        assert_eq!(
            flags,
            aterm_json::json!(["-C", "target-cpu=apple-m1"]),
            "append only the suffix, never duplicate resolved policy"
        );
    }
    assert!(
        configs
            .iter()
            .any(|c| c.starts_with("target.\"cfg(all())\"."))
    );
    let query = t.runs_of("config");
    assert_eq!(query.len(), 1);
    assert_eq!(build_configs(&query[0]), configs[..1]);
    assert_eq!(query[0].cwd, build.cwd);
    assert_eq!(query[0].env_remove, build.env_remove);
}

#[test]
fn native_pack_keeps_build_fallback_and_selected_host_policy() {
    for (label, config, expected) in [
        (
            "fallback",
            r#"{"build":{"rustflags":["--cfg","build_policy"]},"target":{"aarch64-apple-darwin":{"rustflags":[]},"cfg(all())":{"rustflags":""}},"host":{"rustflags":["--cfg","shadowed_default"],"aarch64-apple-darwin":{"linker":"cc"}}}"#,
            vec![
                "build.rustflags=[\"-C\",\"target-cpu=apple-m1\"]",
                "host.\"aarch64-apple-darwin\".rustflags=[\"-C\",\"target-cpu=apple-m1\"]",
            ],
        ),
        (
            "scalar",
            r#"{"build":{"rustflags":"--cfg build_policy"},"host":{"rustflags":"--cfg host_policy"}}"#,
            vec![
                "build.rustflags=\"--cfg build_policy -C target-cpu=apple-m1\"",
                "host.rustflags=\"--cfg host_policy -C target-cpu=apple-m1\"",
            ],
        ),
    ] {
        let w = world(label);
        let mut t = tools(
            &w,
            "aarch64-apple-darwin",
            declared(&w),
            vec![("ay", bin_bytes(macho(CPU_ARM64), "ay arm64"))],
        );
        config_answer(&mut t, config, 0);
        let out = pack::pack_with(&w.ctx, &w.request(AARCH64_APPLE_DARWIN), &t).expect("packed");
        client_verifies(
            &w.ctx,
            &out.manifest,
            out.signature.as_ref().expect("signature"),
        )
        .expect("client accepts native artifact");
        assert_eq!(build_configs(&t.runs_of("build")[0]), expected);
    }
}

#[test]
fn native_pack_refuses_unreadable_or_malformed_policy_before_building() {
    for (json, exit) in [
        ("{}", 1),
        ("not JSON", 0),
        ("[]", 0),
        (r#"{"build":{"rustflags":[1]}}"#, 0),
        (r#"{"build":{"target":"x86_64-unknown-linux-gnu"}}"#, 0),
        (
            r#"{"build":{"target":["aarch64-apple-darwin","x86_64-unknown-linux-gnu"]}}"#,
            0,
        ),
    ] {
        let w = world("pack-bad-native-policy");
        let mut t = tools(
            &w,
            "aarch64-apple-darwin",
            declared(&w),
            vec![("ay", bin_bytes(macho(CPU_ARM64), "ay arm64"))],
        );
        config_answer(&mut t, json, exit);
        pack::pack_with(&w.ctx, &w.request(AARCH64_APPLE_DARWIN), &t)
            .expect_err("policy cannot be silently replaced");
        assert!(t.runs_of("build").is_empty());
        assert!(w.written(".toml").is_empty());
        assert!(w.written(".tar.zst").is_empty());
    }
}

#[test]
fn native_pack_refuses_metadata_that_reroutes_the_requested_target() {
    for args in [
        vec!["--target", "x86_64-unknown-linux-gnu"],
        vec!["--target=x86_64-unknown-linux-gnu"],
    ] {
        let w = world("pack-native-target-policy");
        let t = tools(
            &w,
            "aarch64-apple-darwin",
            metadata_json(&w.checkout, &["ay"], &[], &args, false),
            vec![("ay", bin_bytes(macho(CPU_ARM64), "ay arm64"))],
        );
        let error = pack::pack_with(&w.ctx, &w.request(AARCH64_APPLE_DARWIN), &t)
            .expect_err("native metadata must match the request");
        assert!(error.to_string().contains("different target"));
        assert!(t.runs_of("build").is_empty());
        assert!(w.written(".toml").is_empty());
    }
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

fn signing_tool_metadata(checkout: &Path, recipe: aterm_json::Value) -> String {
    aterm_json::to_string(&aterm_json::json!({
        "packages": [
            {"name":"aterm", "version":"0.170.0", "manifest_path":checkout.join("Cargo.toml").to_string_lossy().into_owned()},
            {"name":"atpkg-keys", "version":"0.0.0", "manifest_path":checkout.join("Cargo.toml").to_string_lossy().into_owned(),
             "metadata":{"atpkg":recipe}},
            {"name":"unrelated", "version":"1.0.0", "metadata":{"atpkg":{"expose":["unrelated-tool"],"bundle-only":true}}}
        ],
        "metadata":{"atpkg":{"expose":["aterm"],"bundle":["app-helper"],"build-args":["--workspace"]}}
    }))
    .expect("metadata JSON")
}

fn signing_tool_recipe() -> aterm_json::Value {
    aterm_json::json!({
        "expose":["atpkg-keys"],
        "build-args":["--locked","-p","atpkg-keys","--bin","atpkg-keys"]
    })
}

#[test]
fn a_program_package_recipe_packs_its_tool_without_the_workspace_application() {
    let w = world("pack-signing-tool");
    let metadata = signing_tool_metadata(&w.checkout, signing_tool_recipe());
    let parsed = pack::parse_metadata(metadata.as_bytes(), "atpkg-keys").expect("own recipe");
    assert_eq!(parsed.version, "0.0.0");
    assert!(!parsed.bundle_only && parsed.bundle.is_empty());
    let app = pack::parse_metadata(metadata.as_bytes(), "aterm").expect("workspace app");
    assert_eq!(app.expose, ["aterm"]);
    assert_eq!(app.build_args, ["--workspace"]);
    let t = tools(
        &w,
        "aarch64-apple-darwin",
        metadata,
        vec![
            (
                "atpkg-keys",
                bin_bytes(macho(CPU_ARM64), "owner signing tool"),
            ),
            ("aterm", bin_bytes(macho(CPU_ARM64), "workspace app")),
        ],
    );
    let mut request = w.request(AARCH64_APPLE_DARWIN);
    request.program = "atpkg-keys".into();
    let out = pack::pack_with(&w.ctx, &request, &t).expect("own signing-tool package");
    assert_eq!(out.exposes, ["atpkg-keys"]);
    let bin = w.root.join("out/atpkg-keys/root/bin");
    assert!(bin.join("atpkg-keys").is_file());
    assert!(!bin.join("aterm").exists());
    let build = &t.runs_of("build")[0];
    assert!(
        build
            .argv()
            .windows(5)
            .any(|a| a == ["--locked", "-p", "atpkg-keys", "--bin", "atpkg-keys"])
    );
    assert!(!build.argv().iter().any(|a| a == "--workspace"));
    client_verifies(
        &w.ctx,
        &out.manifest,
        out.signature.as_ref().expect("signed"),
    )
    .expect("normal client verifies the separate helper package");
    let program = atpkg_publish::programs::get("atpkg-keys").expect("program");
    assert_eq!(program.repo, "aterm");
    assert!(program.system.is_none() && program.group.is_none());
}

#[test]
fn a_malformed_program_recipe_refuses_instead_of_inheriting_workspace_tools() {
    for recipe in [
        aterm_json::json!(false),
        aterm_json::json!({}),
        aterm_json::json!({"expose":[]}),
        aterm_json::json!({"expose":"atpkg-keys"}),
        aterm_json::json!({"expose":["atpkg-keys",7]}),
        aterm_json::json!({"expose":["atpkg-keys"],"build-args":[7]}),
        aterm_json::json!({"expose":["atpkg-keys"],"bundle":"other"}),
        aterm_json::json!({"expose":["atpkg-keys"],"bundle-only":"false"}),
    ] {
        let w = world("pack-invalid-own-recipe");
        let t = tools(
            &w,
            "aarch64-apple-darwin",
            signing_tool_metadata(&w.checkout, recipe),
            Vec::new(),
        );
        let mut request = w.request(AARCH64_APPLE_DARWIN);
        request.program = "atpkg-keys".into();
        let error =
            pack::pack_with(&w.ctx, &request, &t).expect_err("own recipe must not fall back");
        assert!(error.to_string().contains("package.metadata.atpkg"));
        assert!(t.runs_of("build").is_empty());
        assert!(w.written(".toml").is_empty());
    }
}

#[test]
fn astream_ships_its_two_named_clis_without_workspace_tools() {
    let w = world("pack-astream");
    let meta = metadata_json(
        &w.checkout,
        &["asb", "aspump"],
        &[],
        &[
            "--locked",
            "-p",
            "astream-broker",
            "-p",
            "astream-pump",
            "--bin",
            "asb",
            "--bin",
            "aspump",
            "--features",
            "astream-broker/identity,astream-broker/relay,astream-broker/anti-rollback,astream-pump/cap,astream-pump/aead",
        ],
        false,
    );
    let bins = ["asb", "aspump", "astream-evidence", "astream-live"]
        .into_iter()
        .map(|name| (name, bin_bytes(elf(EM_X86_64, 2, 1), name)))
        .collect();
    let t = tools(&w, "aarch64-apple-darwin", meta, bins);
    let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
    req.program = "astream".into();
    req.signing = Signing::Unsigned;
    let out = pack::pack_with(&w.ctx, &req, &t).expect("the two declared CLIs pack");
    assert_eq!(
        &t.runs_of("build")[0].argv()[5..],
        [
            "--locked",
            "-p",
            "astream-broker",
            "-p",
            "astream-pump",
            "--bin",
            "asb",
            "--bin",
            "aspump",
            "--features",
            "astream-broker/identity,astream-broker/relay,astream-broker/anti-rollback,astream-pump/cap,astream-pump/aead",
            "--message-format=json",
        ],
        "the plain packer retains the complete checkout-owned selection"
    );
    assert_eq!(out.exposes, ["asb", "aspump"]);
    let stage = w.root.join("out/astream/root/bin");
    for name in ["asb", "aspump"] {
        assert!(stage.join(name).is_file(), "{name}");
    }
    for name in ["astream-evidence", "astream-live"] {
        assert!(
            !stage.join(name).exists(),
            "unshipped workspace tool {name}"
        );
    }
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
            upload::VERSION_RESERVATION,
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

/// Private dev staging can proceed when neither the identity nor an anonymous reader sees
/// the public repository (§11 D50). Two 404s cover both missing and private-hidden repos;
/// an anonymously visible public repo or an unanswered read keeps the could-not-run.
#[test]
fn an_upload_stages_when_neither_reader_observes_the_public_repository() {
    let w = world("pack-upload-no-public-repo");
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
    let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
    req.upload = true;
    let staging = Slug::new("alabsystems", "ay");
    let public = Slug::new("alabsystems", "ay");

    // Control first: this identity cannot read the public repository, but an anonymous
    // repository read sees it. Nothing may be staged.
    w.host.put(
        &public,
        "atpkg-ay-1",
        FakeRelease {
            prerelease: true,
            ..FakeRelease::default()
        },
    );
    w.host.set_not_visible(&public, true);
    assert_eq!(w.host.repo_seen(&public), Ok(true));
    let err = pack::pack_with(&w.ctx, &req, &t).expect_err("an anonymously visible repository");
    assert!(
        err.to_string()
            .contains("could not ask alabsystems/ay whether atpkg-ay-9 is public"),
        "{err}"
    );
    assert!(w.host.release_of(&staging, "atpkg-ay-9").is_none());

    // A repository read that does not answer keeps the could-not-run too.
    let w = world("pack-upload-public-repo-unanswered");
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
    let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
    req.upload = true;
    w.host.set_not_visible(&public, true);
    w.host.set_repo_unanswered(&public, true);
    let err = pack::pack_with(&w.ctx, &req, &t).expect_err("an unanswered repository read");
    assert!(
        err.to_string().contains("could not ask alabsystems/ay"),
        "{err}"
    );
    assert!(w.host.release_of(&staging, "atpkg-ay-9").is_none());

    // Missing and existing private-hidden repositories are indistinguishable to both
    // readers. Either permits private dev staging without touching that repository.
    for private_hidden in [false, true] {
        let w = world("pack-upload-unseen-public-repo-stages");
        let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
        let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
        req.upload = true;
        if private_hidden {
            w.host.put(&public, "atpkg-ay-1", FakeRelease::default());
            w.host.set_private(&public, true);
        }
        w.host.set_not_visible(&public, true);
        let before = w.host.release_of(&public, "atpkg-ay-1");
        assert_eq!(w.host.repo_seen(&public), Ok(false));
        let out = pack::pack_with(&w.ctx, &req, &t).expect("private dev staging");
        assert!(out.uploaded);
        let rel = w.host.release_of(&staging, "atpkg-ay-9").expect("staged");
        assert!(rel.prerelease && !rel.draft);
        assert_eq!(w.host.release_of(&public, "atpkg-ay-1"), before);
        assert!(w.host.release_of(&public, "atpkg-ay-9").is_none());
        assert!(
            w.host
                .calls()
                .iter()
                .any(|c| c == "repo_seen alabsystems/ay"),
            "the upload asked whether either reader observes the public repository"
        );
    }
}

/// A STAGED BUILD IS NEVER CLOBBERED BY ANOTHER VERSION (D51). The staging release
/// `atpkg-ay-9` holds a manifest of one version: an upload of the same build at another version
/// is refused and every staged asset stays as it was, as is one over a staged manifest that
/// records no readable version, and one over a replace the REST host cut short, which left
/// build 9's manifest only under `pkg-ay-9.toml.atpkg-staged`. The same bytes, other bytes at
/// the same version (a rebuild, a merged second target, the cut-short replace finished) and a
/// staging release with no manifest yet all converge. Through `pack_with`, `ay` 0.3.0 packed
/// as build 9 is refused over build 9's staged 0.2.0.
#[test]
fn a_staged_build_is_never_clobbered_by_another_version() {
    let w = world("upload-staged-version");
    let staging = Slug::new("alabsystems", "ay");
    let tag = "atpkg-ay-9";
    let tarball = "ay-9-aarch64-apple-darwin.tar.zst";
    let lay = |dir: &str, version: &str, note: &str| {
        let dir = w.root.join("up").join(dir);
        let pkg = dir.join("pkg-ay-9.toml");
        put(
            &pkg,
            format!(
                "# {note}\nschema = 1\nprogram = \"ay\"\nversion = \"{version}\"\n\
                 build_number = 9\n"
            )
            .as_bytes(),
        );
        put(&dir.join(tarball), note.as_bytes());
        (pkg.clone(), vec![dir.join(tarball), pkg])
    };
    let publish = |(pkg, files): &(PathBuf, Vec<PathBuf>)| {
        upload::publish(&w.ctx, "ay", tag, pkg, files, "notes")
    };
    let staged = || w.host.asset(&staging, tag, "pkg-ay-9.toml");
    let bytes = |(pkg, _): &(PathBuf, Vec<PathBuf>)| std::fs::read(pkg).expect("manifest");

    let first = lay("first", "0.1.0+gaaaaaaaaa", "the first pack");
    assert_eq!(publish(&first).expect("created"), upload::Uploaded::Staged);
    assert_eq!(
        publish(&first).expect("the same bytes converge"),
        upload::Uploaded::Staged
    );
    let rebuilt = lay("rebuilt", "0.1.0+gaaaaaaaaa", "the same commit rebuilt");
    assert_ne!(bytes(&first), bytes(&rebuilt));
    publish(&rebuilt).expect("other bytes at the same version converge");
    assert_eq!(staged(), Some(bytes(&rebuilt)));

    let refused = |upload: &(PathBuf, Vec<PathBuf>), says: &str| {
        let before = w.host.release_of(&staging, tag);
        let err = publish(upload).expect_err(says);
        assert!(matches!(err, Error::Refused(_)), "{says}: {err:?}");
        assert!(err.to_string().contains(says), "{says}: {err}");
        assert!(
            err.to_string()
                .contains("one build number names one version"),
            "{err}"
        );
        assert_eq!(
            w.host.release_of(&staging, tag),
            before,
            "{says}: nothing staged moved"
        );
    };
    let other = lay("other", "0.1.0+gbbbbbbbbb", "another commit at build 9");
    refused(
        &other,
        "atpkg-ay-9 on alabsystems/ay already stages pkg-ay-9.toml at version \
         `0.1.0+gaaaaaaaaa`, and this upload is version `0.1.0+gbbbbbbbbb`",
    );
    w.host
        .put_asset(&staging, tag, "pkg-ay-9.toml", b"not a manifest".to_vec());
    refused(
        &rebuilt,
        "already stages pkg-ay-9.toml recording no readable version",
    );

    let half = format!("pkg-ay-9.toml{STAGED}");
    w.host.put(
        &staging,
        tag,
        FakeRelease {
            prerelease: true,
            assets: [
                (tarball.to_string(), b"the first pack".to_vec()),
                (half.clone(), bytes(&first)),
            ]
            .into(),
            ..FakeRelease::default()
        },
    );
    refused(
        &other,
        &format!(
            "already stages {half} at version `0.1.0+gaaaaaaaaa`, and this upload is version \
             `0.1.0+gbbbbbbbbb`"
        ),
    );
    publish(&rebuilt).expect("the same version finishes the cut-short replace");
    assert_eq!(staged(), Some(bytes(&rebuilt)));

    w.host.put(
        &staging,
        tag,
        FakeRelease {
            prerelease: true,
            assets: [(tarball.to_string(), b"a partial upload".to_vec())].into(),
            ..FakeRelease::default()
        },
    );
    publish(&other).expect("no manifest staged yet: it converges");
    assert_eq!(staged(), Some(bytes(&other)));
    w.host.fail_once(&staging, tag, "pkg-ay-9.toml");
    let before = w.host.release_of(&staging, tag);
    let err = publish(&first).expect_err("an unread staged manifest is no pass");
    assert!(matches!(err, Error::CannotRun(_)), "{err:?}");
    assert_eq!(w.host.release_of(&staging, tag), before);

    let w = world("pack-upload-staged-version");
    let mut req = w.request(X86_64_UNKNOWN_LINUX_GNU);
    req.upload = true;
    let t = tools(&w, "aarch64-apple-darwin", declared(&w), linux_ay());
    pack::pack_with(&w.ctx, &req, &t).expect("0.2.0 staged as build 9");
    let before = w.host.release_of(&staging, tag);
    let meta = declared(&w).replace("\"0.2.0\"", "\"0.3.0\"");
    let t = tools(&w, "aarch64-apple-darwin", meta, linux_ay());
    let err = pack::pack_with(&w.ctx, &req, &t).expect_err("0.3.0 over build 9's 0.2.0");
    assert!(
        err.to_string()
            .contains("at version `0.2.0`, and this upload is version `0.3.0`"),
        "{err}"
    );
    assert_eq!(w.host.release_of(&staging, tag), before);
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
        pack::HARNESS_VERSION_ENV,
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
        pack::HARNESS_VERSION_ENV,
    ] {
        assert!(
            build.env_remove.iter().any(|k| k == scrubbed),
            "{scrubbed} is removed from the inherited environment"
        );
    }
    assert!(
        build.env_of(pack::HARNESS_VERSION_ENV).is_none(),
        "only a harness's build is handed a version"
    );
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
    std::fs::write(&ctx.tokens.staging, "  \n\t\r\n").expect("whitespace");
    assert!(
        upload::token_preflight(&ctx).is_err(),
        "a whitespace-only file is no token"
    );
    std::fs::write(&ctx.tokens.staging, b"t0ken with space\n").expect("invalid byte");
    let bad = upload::token_preflight(&ctx).expect_err("invalid byte refused");
    assert!(
        bad.to_string()
            .contains("--upload needs the staging owner's token at ~/.secrets/"),
        "{bad}"
    );
    assert!(
        !bad.to_string().contains(&ctx.home.display().to_string()),
        "home path redacted: {bad}"
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
/// `HEAD` to as `branch` and then fetches, and which the host serves as `<owner>/<repo>`.
fn harness_origin(w: &World, checkout: &Path, owner: &str, repo: &str, branch: &str) {
    let bare = w.root.join(format!("remotes/{owner}/{repo}.git"));
    if !bare.exists() {
        bare_origin(&bare);
    }
    w.host.add_clone_source(&Slug::new(owner, repo), &bare);
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
    let run = &t.runs_of("build")[0];
    assert_eq!(
        run.env
            .iter()
            .filter(|(k, _)| k == pack::HARNESS_VERSION_ENV)
            .map(|(_, v)| v.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        [format!("0.1.0+g{sha}")],
        "the build is handed the version the manifest signs (D46)"
    );
    assert!(
        run.env_remove
            .iter()
            .any(|k| k == pack::HARNESS_VERSION_ENV),
        "an inherited value is removed first"
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
    refuses(
        &ahead,
        2,
        "is not on alabsystems/muse-alab main as the host serves it",
    );
    harness_origin(&w, &ahead, "alabsystems", "muse-alab", "feature");
    refuses(
        &ahead,
        2,
        "is not on alabsystems/muse-alab main as the host serves it",
    );

    harness_origin(&w, &ahead, "alabsystems", "muse-alab", "main");
    let t = harness_tools(&ahead, meta(&ahead), arm_bins(&["muse-alab"]));
    let mut req = harness_request(&w, "muse-alab", &ahead);
    req.build = Some(2);
    let out = pack::pack_with(&w.ctx, &req, &t).expect("main holds HEAD: it packs");
    let sha = super::fixture::git(&ahead, &["rev-parse", "--short=9", "HEAD"]);
    assert_eq!((out.build, out.version), (2, format!("0.1.0+g{sha}")));
}

/// THE MAIN A HARNESS IS HELD TO IS THE HOST'S, NOT THE CHECKOUT'S (D25): a clone whose
/// origin is a local mirror at a path ending `alabsystems/muse-alab`, whose main holds a
/// commit the host's main never got, has that commit on its own `origin/main` and is still
/// refused, before anything is built, with the host's main named; the checkout keeps its own
/// remote-tracking refs and no record ref. An unreachable host is a could-not-run, never a
/// pass. The same commit packs once the host's main holds it.
#[test]
fn a_harness_is_held_to_the_hosts_main_not_its_checkouts_origin() {
    let w = world("pack-harness-record");
    let meta = |co: &Path| harness_metadata_json(co, "muse-alab", "0.1.0", &["muse-alab"], &[]);
    let attempt = |co: &Path, build: u64| {
        let t = harness_tools(co, meta(co), arm_bins(&["muse-alab"]));
        let mut req = harness_request(&w, "muse-alab", co);
        req.build = Some(build);
        let out = pack::pack_with(&w.ctx, &req, &t);
        (out, t)
    };
    let record = harness_checkout(&w, "muse-alab");
    let recorded = super::fixture::git(&record, &["rev-parse", "HEAD"]);

    let mirror = w.root.join("mirror/alabsystems/muse-alab.git");
    bare_origin(&mirror);
    let co = w.root.join("from-mirror");
    super::fixture::git(
        &w.root,
        &[
            "clone",
            "-q",
            &w.root
                .join("remotes/alabsystems/muse-alab.git")
                .to_string_lossy(),
            "from-mirror",
        ],
    );
    let git = |args: &[&str]| super::fixture::git(&co, args);
    git(&["remote", "set-url", "origin", &mirror.to_string_lossy()]);
    put(&co.join("README"), b"a commit only the mirror holds\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "mirror only"]);
    git(&["push", "-q", "origin", "HEAD:refs/heads/main"]);
    git(&["fetch", "-q", "origin"]);
    let head = git(&["rev-parse", "HEAD"]);
    assert_eq!(
        git(&["rev-parse", "refs/remotes/origin/main"]),
        head,
        "the checkout's own origin/main holds HEAD, so a check against it would pass"
    );

    let (err, t) = attempt(&co, 2);
    let err = err.expect_err("the host's main does not hold HEAD");
    let says = format!("is not on alabsystems/muse-alab main as the host serves it ({recorded})");
    assert!(err.to_string().contains(&says), "{err}");
    assert!(t.runs_of("build").is_empty(), "refused before the build");
    assert!(w.written(".toml").is_empty() && w.written(".tar.zst").is_empty());
    assert_eq!(git(&["rev-parse", "refs/remotes/origin/main"]), head);
    assert!(
        git(&["for-each-ref", "refs/atpkg-publish/"]).is_empty(),
        "no record ref is left behind"
    );
    assert!(
        w.host
            .calls()
            .iter()
            .any(|c| c == "fetch-main alabsystems/muse-alab")
    );

    w.host
        .set_not_visible(&Slug::new("alabsystems", "muse-alab"), true);
    let err = attempt(&co, 2)
        .0
        .expect_err("an unreachable host is no pass");
    assert!(matches!(err, Error::CannotRun(_)), "{err:?}");
    assert!(
        err.to_string()
            .contains("could not read alabsystems/muse-alab's main"),
        "{err}"
    );
    w.host
        .set_not_visible(&Slug::new("alabsystems", "muse-alab"), false);

    git(&[
        "push",
        "-q",
        &w.root
            .join("remotes/alabsystems/muse-alab.git")
            .to_string_lossy(),
        "HEAD:refs/heads/main",
    ]);
    let out = attempt(&co, 2)
        .0
        .expect("the host's main holds HEAD: it packs");
    let sha = git(&["rev-parse", "--short=9", "HEAD"]);
    assert_eq!((out.build, out.version), (2, format!("0.1.0+g{sha}")));
}

/// A HARNESS BUILD NAMES ONE COMMIT ACROSS A MERGE THAT MOVES MAIN'S FIRST-PARENT CHAIN (D51).
/// Main's tip `tip` (count 2) is packed and staged as build 2. A branch cut before it then
/// lands the usual way, by merging main into the branch and pushing the merge: its own
/// commit `landed` counts 2 as well, and the merge's first parent is `landed`, so `tip` leaves
/// main's first-parent chain as `landed` joins it (grok-alab's `53295526` and `6cc031a0`, both
/// count 15, did exactly this). Both are on main, so no rule over the count or the chain's
/// shape tells them apart over time: packing `landed` as build 2 is refused at the upload,
/// because build 2 already stages `tip`'s version, and staging is unchanged. `tip`, off the
/// chain now, still re-packs and converges (the Apple lane packs a signed release's ledger
/// commit, which a later merge moves off the chain the same way), and the merge packs as
/// build 4.
#[test]
fn a_harness_build_is_one_commit_across_a_merge_that_moves_mains_first_parent_chain() {
    let w = world("pack-harness-timeline");
    let co = harness_checkout(&w, "muse-alab");
    let git = |args: &[&str]| super::fixture::git(&co, args);
    let root = git(&["rev-parse", "HEAD"]);
    put(&co.join("TIP"), b"main's tip when build 2 is packed\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "tip"]);
    let tip = git(&["rev-parse", "HEAD"]);
    harness_origin(&w, &co, "alabsystems", "muse-alab", "main");

    let staging = Slug::new("alabsystems", "muse-alab");
    let tag = "atpkg-muse-alab-2";
    let attempt = |at: &str, build: u64| {
        git(&["checkout", "-q", "--detach", at]);
        let meta = harness_metadata_json(&co, "muse-alab", "0.1.0", &["muse-alab"], &[]);
        let t = harness_tools(&co, meta, arm_bins(&["muse-alab"]));
        let mut req = harness_request(&w, "muse-alab", &co);
        req.build = Some(build);
        req.upload = true;
        pack::pack_with(&w.ctx, &req, &t)
    };
    let version = |c: &str| format!("0.1.0+g{}", git(&["rev-parse", "--short=9", c]));
    let out = attempt(&tip, 2).expect("main's tip packs and stages as build 2");
    assert_eq!((out.build, &out.version), (2, &version(&tip)));
    let staged = w.host.release_of(&staging, tag).expect("build 2 is staged");

    git(&["checkout", "-q", "-b", "branch", &root]);
    put(&co.join("LANDED"), b"a branch cut before the tip\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "landed"]);
    let landed = git(&["rev-parse", "HEAD"]);
    git(&[
        "merge",
        "-q",
        "--no-ff",
        "--no-edit",
        "-m",
        "merge main into the branch",
        &tip,
    ]);
    let merge = git(&["rev-parse", "HEAD"]);
    harness_origin(&w, &co, "alabsystems", "muse-alab", "main");
    let count = |c: &str| git(&["rev-list", "--count", c]);
    assert_eq!(
        [count(&tip), count(&landed), count(&merge)],
        ["2", "2", "4"],
        "the landed commit repeats the packed tip's count"
    );
    assert_eq!(
        git(&["rev-list", "--first-parent", "refs/remotes/origin/main"]),
        [merge.as_str(), landed.as_str(), root.as_str()].join("\n"),
        "the merge moved the packed tip off main's first-parent chain"
    );
    git(&[
        "merge-base",
        "--is-ancestor",
        &tip,
        "refs/remotes/origin/main",
    ]);

    let err = attempt(&landed, 2).expect_err("build 2 already stages the tip's version");
    assert!(matches!(err, Error::Refused(_)), "{err:?}");
    let says = format!(
        "already stages pkg-muse-alab-2.toml at version `{}`, and this upload is version `{}`",
        version(&tip),
        version(&landed)
    );
    assert!(err.to_string().contains(&says), "{err}");
    assert_eq!(
        w.host.release_of(&staging, tag),
        Some(staged),
        "build 2's staged release is unchanged"
    );

    let out = attempt(&tip, 2).expect("the former tip, off the chain now, re-packs");
    assert_eq!((out.build, &out.version), (2, &version(&tip)));
    assert_eq!(
        w.host.asset(&staging, tag, "pkg-muse-alab-2.toml"),
        Some(std::fs::read(&out.manifest).expect("manifest")),
        "the same version converges"
    );
    let out = attempt(&merge, 4).expect("the merge packs as build 4");
    assert_eq!((out.build, &out.version), (4, &version(&merge)));
    assert!(
        w.host
            .asset(&staging, "atpkg-muse-alab-4", "pkg-muse-alab-4.toml")
            .is_some()
    );
}

/// A HARNESS TREE IS A FRESH CLONE'S, WHATEVER GIT'S CONFIG HIDES (D25): an untracked
/// `.cargo/config.toml` (whose `[env]` the build would read) is refused when
/// `status.showUntrackedFiles=no` hides it from a plain `git status`, and again when an
/// exclude rule ignores it. A tracked build input hidden by either index hint is refused
/// without clearing the hint; an ignored top-level `target/` is not a stray. Each refusal
/// comes before the build.
#[test]
fn a_harness_tree_is_a_fresh_clones_whatever_gits_config_hides() {
    let w = world("pack-harness-pristine");
    let co = harness_checkout(&w, "muse-alab");
    let meta = harness_metadata_json(&co, "muse-alab", "0.1.0", &["muse-alab"], &[]);
    let attempt = || {
        let t = harness_tools(&co, meta.clone(), arm_bins(&["muse-alab"]));
        let out = pack::pack_with(&w.ctx, &harness_request(&w, "muse-alab", &co), &t);
        (out, t)
    };
    let git = |args: &[&str]| super::fixture::git(&co, args);
    put(
        &co.join(".cargo/config.toml"),
        b"[env]\nALAB_HARNESS_VERSION = \"9.9.0\"\n",
    );

    git(&["config", "status.showUntrackedFiles", "no"]);
    assert!(
        git(&["status", "--porcelain"]).is_empty(),
        "a plain status hides it"
    );
    let (err, t) = attempt();
    let err = err.expect_err("a hidden untracked file");
    assert!(err.to_string().contains("?? .cargo/config.toml"), "{err}");
    assert!(t.runs_of("build").is_empty(), "refused before the build");

    git(&["config", "--unset", "status.showUntrackedFiles"]);
    put(&co.join(".git/info/exclude"), b".cargo/\n");
    assert!(
        git(&["status", "--porcelain"]).is_empty(),
        "an exclude rule hides it"
    );
    let (err, t) = attempt();
    let err = err.expect_err("an ignored build input");
    assert!(err.to_string().contains("!! .cargo/"), "{err}");
    assert!(t.runs_of("build").is_empty(), "refused before the build");

    std::fs::remove_dir_all(co.join(".cargo")).expect("rm .cargo");
    let manifest = std::fs::read(co.join("Cargo.toml")).expect("tracked manifest");
    for flags in [
        &["--assume-unchanged"][..],
        &["--skip-worktree"][..],
        &["--assume-unchanged", "--skip-worktree"][..],
    ] {
        for flag in flags {
            git(&["update-index", flag, "--", "Cargo.toml"]);
        }
        put(&co.join("Cargo.toml"), b"changed tracked build input\n");
        assert!(git(&["status", "--porcelain"]).is_empty(), "{flags:?}");
        let hinted = git(&["ls-files", "-v", "--", "Cargo.toml"]);
        let (err, t) = attempt();
        let err = err.expect_err("an index hint hides a changed tracked build input");
        assert!(
            err.to_string().contains("index hint hides tracked input")
                && err.to_string().contains("Cargo.toml"),
            "{err}"
        );
        assert!(t.runs_of("build").is_empty(), "refused before the build");
        assert_eq!(git(&["ls-files", "-v", "--", "Cargo.toml"]), hinted);
        assert_eq!(
            std::fs::read(co.join("Cargo.toml")).expect("retained input"),
            b"changed tracked build input\n"
        );
        put(&co.join("Cargo.toml"), &manifest);
        git(&["update-index", "--no-assume-unchanged", "--", "Cargo.toml"]);
        git(&["update-index", "--no-skip-worktree", "--", "Cargo.toml"]);
    }
    let dirty_co = co.clone();
    let base_answer = cargo(&co, meta.clone(), arm_bins(&["muse-alab"]), false);
    let mut dirty_tools = FakeTools::new("aarch64-apple-darwin");
    dirty_tools.answer = Box::new(move |run| {
        let res = base_answer(run);
        if run.argv().iter().any(|a| a == "build") {
            put(
                &dirty_co.join("during-build.txt"),
                b"dirtied during build\n",
            );
        }
        res
    });
    let err = pack::pack_with(&w.ctx, &harness_request(&w, "muse-alab", &co), &dirty_tools)
        .expect_err("a source change during build is refused");
    assert!(
        err.to_string()
            .contains("the harness source changed during the build; refusing its artifact"),
        "{err}"
    );
    assert!(w.written(".toml").is_empty());
    std::fs::remove_file(co.join("during-build.txt")).expect("rm during-build.txt");

    put(&co.join("target/release/muse-alab"), b"an earlier build\n");
    let out = attempt().0.expect("only target/ is ignored: it packs");
    assert_eq!(out.build, 1);
}

#[test]
fn a_pristine_checkout_checks_index_hints_in_active_submodules() {
    let tmp = scratch("pack-harness-submodule-hints");
    let source = tmp.path().join("dependency");
    let co = tmp.path().join("checkout");
    fixture_checkout(&source, &[("source.rs", b"original dependency\n")]);
    fixture_checkout(&co, &[("Cargo.toml", b"[workspace]\n")]);
    let git = |args: &[&str]| super::fixture::git(&co, args);
    git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "--",
        source.to_str().expect("fixture path"),
        "nested",
    ]);
    git(&["commit", "-qm", "Add dependency"]);
    assert_eq!(atpkg_publish::git::is_pristine(&co).unwrap(), None);
    let nested = co.join("nested");
    let child_git = |args: &[&str]| super::fixture::git(&nested, args);
    for (set, clear) in [
        ("--assume-unchanged", "--no-assume-unchanged"),
        ("--skip-worktree", "--no-skip-worktree"),
    ] {
        child_git(&["update-index", set, "--", "source.rs"]);
        put(&nested.join("source.rs"), b"changed dependency\n");
        assert!(git(&["status", "--porcelain"]).is_empty());
        let hinted = child_git(&["ls-files", "-v", "--", "source.rs"]);
        let blocked = atpkg_publish::git::is_pristine(&co)
            .unwrap()
            .expect("a hidden dependency change must prevent publication");
        assert!(blocked.contains("nested/source.rs"), "{blocked}");
        assert_eq!(child_git(&["ls-files", "-v", "--", "source.rs"]), hinted);
        assert_eq!(
            std::fs::read(nested.join("source.rs")).unwrap(),
            b"changed dependency\n"
        );
        put(&nested.join("source.rs"), b"original dependency\n");
        child_git(&["update-index", clear, "--", "source.rs"]);
    }
    assert_eq!(atpkg_publish::git::is_pristine(&co).unwrap(), None);
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
    assert!(
        pack::dev_version("0.1.0", &"a".repeat(40)).is_ok(),
        "40 hex is accepted"
    );
    assert!(
        pack::dev_version("0.1.0", &"a".repeat(41)).is_err(),
        "41 hex is too long"
    );
    assert!(pack::dev_version("0.1.2", "abcdef012").is_err());
}
