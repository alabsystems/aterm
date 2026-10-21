// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The development build: the plan for each policy, every refusal before a build, and a
//! build that lays its artifact, record and digests and hands it to the client's dev-link
//! lane.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use atpkg_publish::artifact::build_local::{self, Policy, Profile, Request};
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::Error;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::testkit::Keys;

use super::fixture::{
    Answer, EM_AARCH64, EM_X86_64, FakeTools, elf, git, put, put_exe, ran, scratch,
    toolchain as fixture_toolchain,
};

const HOST: &str = "x86_64-unknown-linux-gnu";

struct World {
    _tmp: aterm_tempfile::TempDir,
    root: PathBuf,
    ctx: Ctx,
    checkout: PathBuf,
    bin: PathBuf,
}

fn world(label: &str) -> World {
    let tmp = scratch(label);
    let root = std::fs::canonicalize(tmp.path()).expect("root");
    let ctx = Keys::mint("m3", &[]).ctx(&root.join("home"), Arc::new(FakeHost::new()));
    let bin = fixture_toolchain(&root.join("toolchains/trust-7d5533b2"));
    put_exe(&bin.join("cargo"), &elf(EM_X86_64, 2, 1));
    put_exe(&bin.join("trustdoc"), &elf(EM_X86_64, 2, 1));
    let checkout = root.join("trust-ir");
    put(&checkout.join("Cargo.toml"), b"[workspace]\n");
    git(&checkout, &["init", "-q", "."]);
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-q", "-m", "one"]);
    World {
        _tmp: tmp,
        root,
        ctx,
        checkout,
        bin,
    }
}

impl World {
    fn request(&self) -> Request {
        Request {
            program: "trust-ir".into(),
            checkout: self.checkout.clone(),
            plan: false,
            policy: Policy::Repository,
            profile: Profile::Release,
            jobs: 4,
            link: false,
            toolchain: Some(self.bin.clone()),
            out: Some(self.root.join("dev-builds")),
        }
    }
}

fn metadata(expose: &[&str], bundle: &[&str], only: bool) -> String {
    let list = |v: &[&str]| {
        v.iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "{{\"packages\":[{{\"name\":\"trust-ir\",\"version\":\"0.3.0\",\"manifest_path\":\"/x/Cargo.toml\"}}],\
         \"metadata\":{{\"atpkg\":{{\"expose\":[{}],\"bundle\":[{}],\"build-args\":[\"--features\",\"cli\"],\"bundle-only\":{only}}}}}}}",
        list(expose),
        list(bundle)
    )
}

/// `metadata` answers `meta`; the build lays `bins` and reports each by target name;
/// `--help` succeeds unless the bin is named in `broken`.
fn driver(
    out: &Path,
    meta: String,
    bins: Vec<(&'static str, Vec<u8>)>,
    broken: &'static [&'static str],
) -> Answer {
    let out = out.to_path_buf();
    Box::new(move |run| {
        let argv = run.argv();
        if argv.first().map(String::as_str) == Some("metadata") {
            return Some(ran(0, &meta, ""));
        }
        if argv.iter().any(|a| a == "build") {
            let mut s = String::new();
            for (name, bytes) in &bins {
                let exe = put_exe(
                    &out.join("targets.noindex/trust-ir/release").join(name),
                    bytes,
                );
                s.push_str(&format!(
                    "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"{name}\",\"kind\":[\"bin\"]}},\"executable\":\"{}\"}}\n",
                    exe.display()
                ));
            }
            return Some(ran(0, &s, "   Finished"));
        }
        let name = run.program.file_name()?.to_string_lossy().into_owned();
        Some(if broken.contains(&name.as_str()) {
            ran(132, "", "Illegal instruction")
        } else {
            ran(0, "usage: …", "")
        })
    })
}

fn tools(
    w: &World,
    meta: String,
    bins: Vec<(&'static str, Vec<u8>)>,
    broken: &'static [&'static str],
) -> FakeTools {
    let mut t = FakeTools::new(HOST);
    t.answer = driver(&w.root.join("dev-builds"), meta, bins, broken);
    t
}

fn x86(body: &str) -> Vec<u8> {
    let mut b = elf(EM_X86_64, 2, 1);
    b.extend(body.as_bytes());
    b
}

#[test]
fn each_policy_plans_its_lane_with_the_seals_own_compiler() {
    let w = world("local-plan");
    let t = tools(
        &w,
        metadata(&["trust-ir"], &["trust-ir-helper"], false),
        vec![],
        &[],
    );
    let plan = build_local::plan(&w.ctx, &w.request(), &t).expect("planned");
    assert_eq!(
        plan.command.program,
        w.bin.join("cargo"),
        "the seal's own cargo"
    );
    assert_eq!(
        plan.command.argv(),
        [
            "build",
            "--locked",
            "--profile",
            "release",
            "-j",
            "4",
            "--features",
            "cli",
            "--bin",
            "trust-ir",
            "--bin",
            "trust-ir-helper"
        ]
    );
    assert_eq!(
        plan.command.env_of("RUSTC"),
        Some(&w.bin.join("trustc").into_os_string())
    );
    assert_eq!(
        plan.command.env_of("RUSTDOC"),
        Some(&w.bin.join("trustdoc").into_os_string())
    );
    assert_eq!(
        plan.command.env_of("CARGO_TARGET_DIR"),
        Some(
            &w.root
                .join("dev-builds/targets.noindex/trust-ir")
                .into_os_string()
        )
    );
    assert_eq!(
        plan.triple.as_str(),
        HOST,
        "a native build for the compiler's host"
    );
    assert!(t.runs_of("build").is_empty(), "a plan builds nothing");
    let mut req = w.request();
    req.policy = Policy::Unverified;
    req.profile = Profile::Dev;
    let plan = build_local::plan(&w.ctx, &req, &t).expect("planned");
    assert_eq!(plan.command.program, w.bin.join("targo"));
    assert_eq!(
        plan.command.argv()[..5],
        ["--unverified", "build", "--locked", "--profile", "dev"]
    );
}

#[test]
fn a_build_that_could_not_be_trusted_is_refused_before_it_starts() {
    let w = world("local-refusals");
    let t = tools(&w, metadata(&["trust-ir"], &[], false), vec![], &[]);
    let refused = |req: &Request, t: &FakeTools, want: &str| {
        let err = build_local::plan(&w.ctx, req, t).expect_err(want);
        assert!(err.to_string().contains(want), "{want}: {err}");
    };

    let stage = fixture_toolchain(&w.root.join("trust/build/host/stage2"));
    put_exe(&stage.join("cargo"), b"\x7fELF");
    put_exe(&stage.join("trustdoc"), b"\x7fELF");
    let mut req = w.request();
    req.toolchain = Some(stage);
    refused(&req, &t, "mutable bootstrap stage");

    let thin = fixture_toolchain(&w.root.join("thin"));
    let mut req = w.request();
    req.toolchain = Some(thin);
    refused(&req, &t, "lacks cargo");

    put(&w.checkout.join("Cargo.toml"), b"[workspace]\n# edited\n");
    refused(&w.request(), &t, "tracked source changes");
    git(&w.checkout, &["checkout", "--", "Cargo.toml"]);

    let only = tools(&w, metadata(&["trust-ir"], &[], true), vec![], &[]);
    refused(&w.request(), &only, "pack-bundle");
    let none = tools(&w, metadata(&[], &[], false), vec![], &[]);
    refused(&w.request(), &none, "no [workspace.metadata.atpkg] expose");

    let mut req = w.request();
    req.program = "Trust_IR".into();
    assert!(matches!(
        build_local::plan(&w.ctx, &req, &t),
        Err(Error::Usage(_))
    ));
}

#[test]
fn a_build_lays_its_artifact_record_and_digests_and_links_it() {
    let w = world("local-build");
    let bins = vec![
        ("trust-ir", x86("trust-ir")),
        ("trust-ir-helper", x86("helper")),
    ];
    let t = tools(
        &w,
        metadata(&["trust-ir"], &["trust-ir-helper"], false),
        bins,
        &[],
    );
    let req = w.request();
    let plan = build_local::plan(&w.ctx, &req, &t).expect("plan");
    let built = build_local::build(&w.ctx, &req, &plan, &t).expect("built");
    assert!(
        built.dest.join("bin/trust-ir").is_file()
            && built.dest.join("bin/trust-ir-helper").is_file()
    );
    let name = built
        .dest
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    assert!(
        name.ends_with(&format!(
            "-{}-release-repository",
            &super::fixture::COMMIT[..12]
        )),
        "{name}"
    );
    let record: aterm_json::Value = aterm_json::from_str(
        &std::fs::read_to_string(built.dest.join("local-build.json")).expect("record"),
    )
    .expect("JSON");
    assert_eq!(
        record.get("program").and_then(aterm_json::Value::as_str),
        Some("trust-ir")
    );
    assert_eq!(
        record.get("target").and_then(aterm_json::Value::as_str),
        Some(HOST)
    );
    assert_eq!(
        record.get("policy").and_then(aterm_json::Value::as_str),
        Some("repository")
    );
    assert_eq!(
        record
            .get("exposes")
            .and_then(aterm_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    let sums = std::fs::read_to_string(built.dest.join("SHA256SUMS")).expect("sums");
    for line in sums.lines() {
        let (sha, rel) = line.split_once("  ").expect("sha256sum format");
        assert_eq!(
            atpkg::sha256_file(&built.dest.join(rel)).expect("hash"),
            sha,
            "{rel}"
        );
    }
    assert_eq!(built.link[..4], ["aterm", "pkg", "link", "trust-ir"]);
    assert_eq!(built.link.last().map(String::as_str), Some("bin/trust-ir"));
    let linked =
        build_local::link(&w.ctx, "trust-ir", &built, &plan.metadata.expose).expect("linked");
    assert_eq!(linked, ["trust-ir"]);

    let err = build_local::build(&w.ctx, &req, &plan, &t).expect_err("again");
    assert!(err.to_string().contains("already exists"), "{err}");
}

#[test]
fn a_foreign_binary_or_one_that_will_not_start_is_never_staged() {
    let w = world("local-checks");
    let arm = tools(
        &w,
        metadata(&["trust-ir"], &[], false),
        vec![("trust-ir", {
            let mut b = elf(EM_AARCH64, 2, 1);
            b.extend(b"arm");
            b
        })],
        &[],
    );
    let req = w.request();
    let plan = build_local::plan(&w.ctx, &req, &arm).expect("plan");
    let err = build_local::build(&w.ctx, &req, &plan, &arm).expect_err("foreign");
    assert!(err.to_string().contains("ARCHITECTURE MISMATCH"), "{err}");

    let sick = tools(
        &w,
        metadata(&["trust-ir"], &[], false),
        vec![("trust-ir", x86("x"))],
        &["trust-ir"],
    );
    let plan = build_local::plan(&w.ctx, &req, &sick).expect("plan");
    let err = build_local::build(&w.ctx, &req, &plan, &sick).expect_err("will not start");
    assert!(err.to_string().contains("startup smoke check"), "{err}");

    let missing = tools(
        &w,
        metadata(&["trust-ir", "trust-ir-lsp"], &[], false),
        vec![("trust-ir", x86("x"))],
        &[],
    );
    let plan = build_local::plan(&w.ctx, &req, &missing).expect("plan");
    let err = build_local::build(&w.ctx, &req, &plan, &missing).expect_err("no executable");
    assert!(
        err.to_string()
            .contains("no unique executable for trust-ir-lsp"),
        "{err}"
    );
}

#[test]
fn a_source_change_during_the_build_is_refused() {
    let w = world("local-moved");
    let checkout = w.checkout.clone();
    let inner = driver(
        &w.root.join("dev-builds"),
        metadata(&["trust-ir"], &[], false),
        vec![("trust-ir", x86("x"))],
        &[],
    );
    let mut t = FakeTools::new(HOST);
    t.answer = Box::new(move |run| {
        if run.argv().iter().any(|a| a == "build") {
            put(
                &checkout.join("Cargo.toml"),
                b"[workspace]\n# edited mid-build\n",
            );
        }
        inner(run)
    });
    let req = w.request();
    let plan = build_local::plan(&w.ctx, &req, &t).expect("plan");
    let err = build_local::build(&w.ctx, &req, &plan, &t).expect_err("moved");
    assert!(
        err.to_string().contains("source changed during the build"),
        "{err}"
    );
}
