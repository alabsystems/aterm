// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The sysroot-bundle pack: the compiler admitted before the stage, the closure frozen,
//! hygiene over the staged copy only, one compiler, the strip pass once per hard-link
//! group, the gates over the final bytes, and the version and target derived from what is
//! packed.

#![cfg(unix)]

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use atpkg_publish::api::{BundleRequest, HelloGate, Signing};
use atpkg_publish::artifact::bundle::{self, HelloPlan, OneCompiler, RETIRED_BINS};
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::Error;
use atpkg_publish::host::Slug;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::programs::{self, Shape, TRUST_EXPOSES};
use atpkg_publish::testkit::Keys;
use atpkg_publish::triple::Triple;

use super::fixture::{
    CPU_ARM64, CPU_X86_64, DORMANT_LEDGER, EM_AARCH64, FakeTools, MINTED_LEDGER, SEAL_COMMIT,
    SEAL_DRIVER, SEAL_TRIPLE, checkout as fixture_checkout, client_verifies, elf, fat, git, macho,
    put, put_exe, ran, scratch, seal_fixture, seed_mint, set_mode, toolchain as fixture_toolchain,
    trust_repo, version_text,
};

fn native(body: &str) -> Vec<u8> {
    let mut b = elf(EM_AARCH64, 2, 1);
    b.extend(body.as_bytes());
    b
}

fn seal_triple() -> Triple {
    Triple::parse(SEAL_TRIPLE).expect("t")
}

struct World {
    _tmp: aterm_tempfile::TempDir,
    root: PathBuf,
    ctx: Ctx,
    host: Arc<FakeHost>,
    seal: PathBuf,
    private: PathBuf,
    src: PathBuf,
}

/// A rostered machine with a channel head, a dormant `$HOME/trust`, a promoter-shaped trust
/// seal (closure beside it, rustc and rustdoc hard links, cargo) and a clean source
/// checkout.
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
        &keys.index_toml(41, &[("trust", "trust")], &[("trust", 1)]),
    );
    trust_repo(&home.join("trust"), DORMANT_LEDGER);
    let (seal, private) = seal_fixture(&root);
    std::fs::hard_link(seal.join("bin/trustc"), seal.join("bin/rustc")).expect("rustc");
    put_exe(&seal.join("bin/trustdoc"), &native("trustdoc"));
    std::fs::hard_link(seal.join("bin/trustdoc"), seal.join("bin/rustdoc")).expect("rustdoc");
    put_exe(&seal.join("bin/cargo"), &native("cargo"));
    for tool in TRUST_EXPOSES {
        let bin = seal.join("bin").join(tool);
        if !bin.exists() {
            put_exe(&bin, &native(tool));
        }
    }
    std::fs::create_dir_all(seal.join("libexec")).expect("libexec");
    let src = root.join("trust-src");
    fixture_checkout(&src, &[("README", b"trust\n")]);
    git(&src, &["config", "core.excludesFile", "/dev/null"]);
    World {
        _tmp: tmp,
        root,
        ctx,
        host,
        seal,
        private,
        src,
    }
}

impl World {
    fn request(&self) -> BundleRequest {
        BundleRequest {
            program: "trust".into(),
            sysroot: self.seal.clone(),
            src_repo: self.src.clone(),
            build: Some(1),
            version: Some("0.0.0-test".into()),
            triple: Some(seal_triple()),
            hello: HelloGate {
                enabled: false,
                ..HelloGate::default()
            },
            donor_roots: Vec::new(),
            keep_targets: Vec::new(),
            allow_dirty: false,
            smoke: false,
            strip: true,
            merge_from: None,
            out: self.root.join("out"),
            signing: Signing::Unsigned,
            upload: false,
        }
    }

    fn extract(&self, asset: &Path) -> PathBuf {
        let x = self.root.join("x");
        std::fs::create_dir_all(&x).expect("mkdir");
        set_mode(&x, 0o700);
        atpkg::extract_tar_zst(asset, &x, 1 << 30, 100_000).expect("the client extracts it");
        x
    }

    fn stages_left(&self) -> usize {
        std::fs::read_dir(self.root.join("out")).map_or(0, |rd| {
            rd.flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with(".atpkg-bundle-")
                })
                .count()
        })
    }
}

fn tools() -> FakeTools {
    let mut t = FakeTools::new(SEAL_TRIPLE);
    t.default_version = Some(version_text(SEAL_COMMIT, SEAL_TRIPLE));
    t
}

fn ino(p: &Path) -> (u64, u64) {
    let m = std::fs::metadata(p).expect("stat");
    (m.dev(), m.ino())
}

#[test]
fn a_trust_bundle_ships_its_frozen_closure_and_one_inode_per_alias() {
    let w = world("bundle-trust");
    let t = tools();
    let out = bundle::pack_bundle_with(&w.ctx, &w.request(), &t).expect("packed");
    assert_eq!(
        out.asset.file_name().and_then(|n| n.to_str()),
        Some("trust-1-aarch64-unknown-linux-gnu.tar.zst")
    );
    assert_eq!(out.exposes, TRUST_EXPOSES);
    assert!(out.signature.is_none());
    assert_eq!(w.stages_left(), 0, "the stage is removed with the pack");
    let x = w.extract(&out.asset);
    let target = x.join(format!("lib/rustlib/{SEAL_TRIPLE}/lib"));
    assert_eq!(
        std::fs::read(target.join("librustc_middle-fixture.rmeta")).expect("private"),
        b"matching private metadata\n"
    );
    assert_eq!(
        ino(&target.join(SEAL_DRIVER)),
        ino(&x.join("lib").join(SEAL_DRIVER))
    );
    let src_rs = "rust/compiler/rustc_driver/src/lib.rs";
    assert_eq!(
        ino(&x.join("lib/rustlib/src").join(src_rs)),
        ino(&x.join("lib/rustlib/rustc-src").join(src_rs))
    );
    assert_eq!(ino(&x.join("bin/rustc")), ino(&x.join("bin/trustc")));
    assert_eq!(
        ino(&x.join("bin/rustdoc")),
        ino(&x.join("bin/trustdoc")),
        "the required alias"
    );
    assert!(x.join("bin/cargo").is_file());
    let text = std::fs::read_to_string(&out.manifest).expect("pkg");
    assert!(
        text.contains("kind = \"sysroot-bundle\"") && text.contains("reloc = \"self-contained\"")
    );
    assert!(
        std::fs::symlink_metadata(w.seal.join("lib/rustlib/rustc-src/rust"))
            .expect("alias")
            .file_type()
            .is_symlink(),
        "the seal itself is never written"
    );
    let log = t.log();
    let first_vv = log
        .iter()
        .position(|l| l.ends_with("trustc -vV"))
        .expect("admitted");
    let reloc = log
        .iter()
        .position(|l| l.starts_with("relocate "))
        .expect("relocated");
    assert!(first_vv < reloc, "{log:#?}");
}

#[test]
fn relative_output_paths_keep_the_staged_toolchain_reachable_from_the_hello_crate() {
    let w = world("bundle-relative-out");
    let scratch = aterm_tempfile::Builder::new()
        .prefix(".atpkg-relative-out-")
        .tempdir()
        .expect("scratch");
    let here = std::env::current_dir().expect("this process's folder");
    let up: PathBuf = here.components().skip(1).map(|_| "..").collect();
    let local = up.join(
        scratch
            .path()
            .strip_prefix("/")
            .expect("the scratch is absolute"),
    );
    assert!(local.is_relative());
    let source = local.join("src");
    put(&source.join("README"), b"trust\n");
    git(&source, &["init", "-q", "."]);
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-q", "-m", "one"]);

    for extra in [
        vec!["--out".to_string(), local.join("out").display().to_string()],
        Vec::new(),
    ] {
        let mut args: Vec<String> = vec![
            "trust".into(),
            "--sysroot".into(),
            w.seal.display().to_string(),
            "--src-repo".into(),
            source.display().to_string(),
            "--build".into(),
            "1".into(),
            "--version".into(),
            "0.0.0-test".into(),
            "--no-hello".into(),
            "--no-strip".into(),
            "--unsigned".into(),
        ];
        args.extend(extra);
        let req = bundle::parse_args(&mut w.ctx.clone(), &args).expect("request");
        assert!(req.out.is_relative());
        let mut t = tools();
        t.answer = Box::new(|run| {
            run.cwd.as_ref().map(|cwd| {
                let reachable = cwd.join(&run.program).is_file()
                    && run
                        .env_of("HOME")
                        .is_some_and(|h| Path::new(h).is_absolute())
                    && run
                        .env_of("PATH")
                        .is_some_and(|p| Path::new(p).is_absolute());
                ran(if reachable { 0 } else { 127 }, "", "")
            })
        });
        let out = bundle::pack_bundle_with(&w.ctx, &req, &t).expect("relative output packs");
        assert!(out.asset.is_file());
        assert!(t.runs().iter().any(|run| run.cwd.is_some()), "tippy ran");
    }
}

#[test]
fn a_refused_compiler_or_closure_stops_the_pack_before_relocation() {
    let w = world("bundle-refused");
    std::fs::remove_dir_all(w.ctx.home.join("trust")).expect("rm");
    std::fs::remove_dir_all(w.ctx.home.join("trust.git")).expect("rm");
    trust_repo(&w.ctx.home.join("trust"), MINTED_LEDGER);
    let t = tools();
    let err = bundle::pack_bundle_with(&w.ctx, &w.request(), &t).expect_err("live, no receipt");
    assert!(
        err.to_string()
            .contains("carries neither LINEAGE nor SEED-MINT"),
        "{err}"
    );
    assert!(!t.log().iter().any(|l| l.starts_with("relocate")));
    assert_eq!(w.stages_left(), 0);

    let w = world("bundle-no-private");
    std::fs::rename(&w.private, w.private.with_file_name("different-root")).expect("mv");
    let t = tools();
    let err = bundle::pack_bundle_with(&w.ctx, &w.request(), &t).expect_err("no closure");
    assert!(
        err.to_string()
            .contains("REFUSING incomplete Trust closure"),
        "{err}"
    );
    assert!(!t.log().iter().any(|l| l.starts_with("relocate")));
}

#[test]
fn retired_entrypoints_leave_the_stage_and_never_the_seal() {
    let w = world("bundle-retired");
    for name in RETIRED_BINS {
        put_exe(&w.seal.join("bin").join(name), &native(name));
    }
    put(
        &w.seal.join("libexec/rust-analyzer-proc-macro-srv"),
        b"retired",
    );
    let out = bundle::pack_bundle_with(&w.ctx, &w.request(), &tools()).expect("packed");
    let x = w.extract(&out.asset);
    for name in RETIRED_BINS {
        assert!(!x.join("bin").join(name).exists(), "{name} shipped");
        assert!(
            w.seal.join("bin").join(name).exists(),
            "{name} removed from the seal"
        );
    }
    assert!(!x.join("libexec/rust-analyzer-proc-macro-srv").exists());
}

#[test]
fn an_exposes_list_naming_a_retired_entrypoint_is_refused() {
    let tmp = scratch("bundle-retired-exposed");
    for name in RETIRED_BINS {
        let err = bundle::strip_retired(tmp.path(), &["trustc", "targo", name]).expect_err(name);
        assert!(err.to_string().contains(&format!("'{name}'")), "{err}");
    }
    bundle::strip_retired(tmp.path(), &["trustc", "rustdoc"])
        .expect("rustdoc is required, not retired");
}

#[test]
fn dev_source_links_are_pruned_and_any_other_link_refused() {
    let tmp = scratch("bundle-links");
    let root = tmp.path();
    put(&root.join("bin/trustc"), b"x");
    std::fs::create_dir_all(root.join("lib/rustlib/src")).expect("mkdir");
    std::fs::create_dir_all(root.join("lib/rustlib/rustc-src")).expect("mkdir");
    std::os::unix::fs::symlink("/builder/trust", root.join("lib/rustlib/src/rust")).expect("l");
    std::os::unix::fs::symlink("/builder/trust", root.join("lib/rustlib/rustc-src/rust"))
        .expect("l");
    let pruned = bundle::prune_links(root).expect("pruned");
    assert_eq!(
        pruned,
        ["lib/rustlib/rustc-src/rust", "lib/rustlib/src/rust"]
    );
    assert!(!root.join("lib/rustlib/src").exists(), "no husk ships");
    std::os::unix::fs::symlink("trustc", root.join("bin/rustc")).expect("l");
    let err = bundle::prune_links(root).expect_err("another link");
    assert!(
        err.to_string()
            .contains("unexpected symlink in the staged sysroot: bin/rustc"),
        "{err}"
    );
}

#[test]
fn foreign_target_sysroots_are_pruned_unless_kept_and_each_kept_one_carries_a_std() {
    let tmp = scratch("bundle-foreign");
    let root = tmp.path();
    let rustlib = root.join("lib/rustlib");
    for t in [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "wasm32-unknown-unknown",
    ] {
        put(&rustlib.join(t).join("lib/libstd-x.rlib"), b"std");
    }
    put(&rustlib.join("etc/gdb_load.py"), b"rider");
    let darwin = Triple::parse("aarch64-apple-darwin").expect("t");
    let pruned = bundle::prune_foreign_targets(root, darwin, &["wasm32-unknown-unknown".into()])
        .expect("pruned");
    assert_eq!(pruned, ["x86_64-apple-darwin"]);
    assert!(rustlib.join("etc").is_dir(), "a non-triple rider stays");
    assert!(
        rustlib.join("wasm32-unknown-unknown").is_dir(),
        "kept on purpose"
    );
    bundle::require_target_std(root).expect("every kept target has its std");
    std::fs::remove_file(rustlib.join("wasm32-unknown-unknown/lib/libstd-x.rlib")).expect("rm");
    let err = bundle::require_target_std(root).expect_err("a target it cannot compile for");
    assert!(err.to_string().contains("wasm32-unknown-unknown"), "{err}");
}

#[test]
fn a_fat_mach_o_is_thinned_through_its_inode_and_one_without_the_slice_refused() {
    let tmp = scratch("bundle-thin");
    let root = tmp.path();
    let mut both = fat(&[CPU_X86_64, CPU_ARM64]);
    both.resize(4096, 0);
    put_exe(&root.join("lib/libdriver.dylib"), &both);
    std::fs::hard_link(
        root.join("lib/libdriver.dylib"),
        root.join("lib/rustlib/libdriver.dylib"),
    )
    .or_else(|_| {
        std::fs::create_dir_all(root.join("lib/rustlib"))?;
        std::fs::hard_link(
            root.join("lib/libdriver.dylib"),
            root.join("lib/rustlib/libdriver.dylib"),
        )
    })
    .expect("link");
    put_exe(&root.join("bin/thin"), &macho(CPU_ARM64));
    let t = tools();
    let darwin = Triple::parse("aarch64-apple-darwin").expect("t");
    let thinned = bundle::thin_fat(&t, root, darwin).expect("thinned");
    assert_eq!(
        thinned,
        ["lib/libdriver.dylib"],
        "a hard link's group is thinned once"
    );
    assert_eq!(
        atpkg_publish::artifact::arch::read(&root.join("lib/rustlib/libdriver.dylib"))
            .map(|o| o.to_string()),
        Some("macho arm64".into()),
        "written through the inode: the link group still agrees"
    );
    assert_eq!(
        ino(&root.join("lib/libdriver.dylib")),
        ino(&root.join("lib/rustlib/libdriver.dylib"))
    );
    put_exe(&root.join("bin/x86-only"), &fat(&[CPU_X86_64, 7]));
    let err = bundle::thin_fat(&t, root, darwin).expect_err("no arm64 slice");
    assert!(err.to_string().contains("has no arm64 slice"), "{err}");
    assert!(
        bundle::thin_fat(&t, root, seal_triple())
            .expect("linux")
            .is_empty(),
        "a Linux bundle has no Mach-O to thin"
    );
}

#[test]
fn the_one_compiler_step_links_a_copy_or_a_resigned_twin_and_refuses_two_programs() {
    let tmp = scratch("bundle-one-compiler");
    let tree = |name: &str, rustc: Option<&[u8]>| {
        let r = tmp.path().join(name);
        put(&r.join("bin/trustc"), b"one compiler\n");
        if let Some(b) = rustc {
            put(&r.join("bin/rustc"), b);
        }
        r
    };
    let t = tools();
    let copy = tree("copy", Some(b"one compiler\n"));
    assert_eq!(
        bundle::one_compiler(&t, &copy).expect("copy"),
        OneCompiler::LinkedCopy
    );
    assert_eq!(ino(&copy.join("bin/rustc")), ino(&copy.join("bin/trustc")));
    assert_eq!(
        bundle::one_compiler(&t, &copy).expect("again"),
        OneCompiler::AlreadyLinked
    );

    let lone = tree("lone", None);
    assert_eq!(
        bundle::one_compiler(&t, &lone).expect("lone"),
        OneCompiler::NotApplicable
    );
    assert!(!lone.join("bin/rustc").exists(), "no bin/rustc is invented");

    let two = tree("two", Some(b"compiler B\n"));
    let err = bundle::one_compiler(&t, &two).expect_err("two programs");
    assert!(
        err.to_string()
            .contains("different programs (first difference at byte 1"),
        "{err}"
    );
    assert_ne!(
        ino(&two.join("bin/rustc")),
        ino(&two.join("bin/trustc")),
        "not linked anyway"
    );

    let mut twins = tools();
    twins.same_unsigned = true;
    let signed = tree("signed", Some(b"one compiler\n signature rustc-uuid"));
    assert_eq!(
        bundle::one_compiler(&twins, &signed).expect("twins"),
        OneCompiler::LinkedResigned
    );
    assert_eq!(
        ino(&signed.join("bin/rustc")),
        ino(&signed.join("bin/trustc"))
    );
}

#[test]
fn the_one_compiler_gate_reads_the_final_bytes() {
    let tmp = scratch("bundle-compiler-gate");
    let root = tmp.path();
    put(&root.join("bin/trustc"), b"compiler A\n");
    assert_eq!(
        bundle::compiler_gate(root).expect("n/a"),
        OneCompiler::NotApplicable
    );
    put(&root.join("bin/rustc"), b"compiler B\n");
    let err = bundle::compiler_gate(root).expect_err("mismatch");
    assert!(
        err.to_string()
            .contains("not byte-identical (first difference at byte 10)"),
        "{err}"
    );
    put(&root.join("bin/rustc"), b"compiler A\n");
    assert_eq!(
        bundle::compiler_gate(root).expect("copy"),
        OneCompiler::LinkedCopy
    );
    std::fs::remove_file(root.join("bin/rustc")).expect("rm");
    std::fs::hard_link(root.join("bin/trustc"), root.join("bin/rustc")).expect("ln");
    assert_eq!(
        bundle::compiler_gate(root).expect("link"),
        OneCompiler::AlreadyLinked
    );
    std::fs::remove_file(root.join("bin/rustc")).expect("rm");
    std::os::unix::fs::symlink("trustc", root.join("bin/rustc")).expect("ln -s");
    let err = bundle::compiler_gate(root).expect_err("a link");
    assert!(err.to_string().contains("regular files"), "{err}");
}

#[test]
fn the_strip_pass_signs_each_hard_link_group_once_and_leaves_rlibs_alone() {
    let tmp = scratch("bundle-strip");
    let root = tmp.path();
    put_exe(&root.join("lib/librustc_driver-x.dylib"), &macho(CPU_ARM64));
    std::fs::create_dir_all(root.join("lib/rustlib/t/lib")).expect("mkdir");
    std::fs::hard_link(
        root.join("lib/librustc_driver-x.dylib"),
        root.join("lib/rustlib/t/lib/librustc_driver-x.dylib"),
    )
    .expect("ln");
    put_exe(&root.join("bin/trustc"), &macho(CPU_ARM64));
    let mut rlib_head = macho(CPU_ARM64);
    rlib_head.extend(b"archive");
    put(&root.join("lib/rustlib/t/lib/libstd-x.rlib"), &rlib_head);
    put_exe(&root.join("bin/elf-tool"), &elf(EM_AARCH64, 2, 1));
    let t = tools();
    assert_eq!(bundle::strip_symbols(&t, root).expect("stripped"), (2, 1));
    let log = t.log();
    let strips: Vec<&String> = log
        .iter()
        .filter(|l| l.starts_with("strip -x -S"))
        .collect();
    let signs: Vec<&String> = log.iter().filter(|l| l.starts_with("resign")).collect();
    assert_eq!((strips.len(), signs.len()), (2, 2), "{log:#?}");
    assert!(
        !log.iter()
            .any(|l| l.contains("libstd-x.rlib") || l.contains("elf-tool"))
    );
    assert_eq!(
        ino(&root.join("lib/librustc_driver-x.dylib")),
        ino(&root.join("lib/rustlib/t/lib/librustc_driver-x.dylib"))
    );
}

#[test]
fn the_smoke_refuses_a_loader_error_and_a_bin_that_never_ran() {
    let tmp = scratch("bundle-smoke");
    let (root, home) = (tmp.path().join("root"), tmp.path().join("home"));
    put_exe(&root.join("bin/trustc"), &native("trustc"));
    std::fs::create_dir_all(&home).expect("home");
    let mut t = tools();
    t.answer = Box::new(|run| {
        let name = run.program.file_name()?.to_string_lossy().into_owned();
        Some(match name.as_str() {
            "trustc" => ran(2, "", "error: unknown flag"),
            "dyld-broken" => ran(134, "", "dyld[1]: Library not loaded: @rpath/libLLVM.dylib"),
            _ => ran(127, "", "exec format error"),
        })
    });
    bundle::smoke(&t, &root, &home, "trustc").expect("a usage error still ran");
    let run = t
        .runs()
        .into_iter()
        .find(|r| r.argv() == ["--version"])
        .expect("probed");
    assert!(run.clear_env);
    assert_eq!(run.env_of("HOME"), Some(&home.clone().into_os_string()));
    assert_eq!(
        run.env_of("PATH").map(|p| p.to_string_lossy().into_owned()),
        Some(format!("{}:/usr/bin:/bin", root.join("bin").display()))
    );
    put_exe(&root.join("bin/dyld-broken"), &native("x"));
    let err = bundle::smoke(&t, &root, &home, "dyld-broken").expect_err("loader");
    assert!(
        err.to_string().contains("loader error on '--version'"),
        "{err}"
    );
    put_exe(&root.join("bin/never"), &native("x"));
    let err = bundle::smoke(&t, &root, &home, "never").expect_err("never ran");
    assert!(err.to_string().contains("never executed cleanly"), "{err}");
    let err = bundle::smoke(&t, &root, &home, "absent").expect_err("missing");
    assert!(err.to_string().contains("exposed bin 'absent'"), "{err}");
}

#[test]
fn the_hello_compiles_strict_else_verify_off_against_the_sysroot_it_is_handed() {
    let tmp = scratch("bundle-hello");
    let (root, home) = (tmp.path().join("root"), tmp.path().join("home"));
    std::fs::create_dir_all(&home).expect("home");
    let compiler = put_exe(&root.join("bin/rustc"), &native("rustc"));
    let sysroot = tmp.path().join("trust-sysroot");
    let hello_out = |strict_ok: bool, verify_off_ok: bool, says: &'static str| {
        let mut t = tools();
        t.answer = Box::new(move |run| {
            let argv = run.argv();
            if argv.iter().any(|a| a == "--sysroot") {
                let off = argv.iter().any(|a| a == "-Ztrust-verify=off");
                let ok = if off { verify_off_ok } else { strict_ok };
                return Some(if ok {
                    ran(0, "", "")
                } else {
                    ran(
                        1,
                        "",
                        "error: unproven obligation in hello.rs\nerror: aborting",
                    )
                });
            }
            Some(ran(0, says, ""))
        });
        t
    };
    let t = hello_out(true, true, "hello, atpkg sysroot\n");
    assert_eq!(
        bundle::hello(&t, &root, &home, &compiler, &sysroot).expect("strict"),
        "strict"
    );
    let first = &t.runs()[0];
    assert_eq!(
        first.argv()[..2],
        ["--sysroot".to_string(), sysroot.display().to_string()]
    );
    let t = hello_out(false, true, "hello, atpkg sysroot\n");
    assert_eq!(
        bundle::hello(&t, &root, &home, &compiler, &sysroot).expect("verify off"),
        "-Ztrust-verify=off"
    );
    let t = hello_out(false, false, "");
    let err = bundle::hello(&t, &root, &home, &compiler, &sysroot).expect_err("neither");
    assert!(
        err.to_string().contains("strict AND verify-off") && err.to_string().contains("aborting"),
        "{err}"
    );
    let t = hello_out(true, true, "goodbye\n");
    assert!(
        bundle::hello(&t, &root, &home, &compiler, &sysroot).is_err(),
        "the wrong output"
    );
}

#[test]
fn the_tippy_gate_lints_a_hello_crate_and_quotes_a_setup_error() {
    let tmp = scratch("bundle-tippy");
    let (root, home) = (tmp.path().join("root"), tmp.path().join("home"));
    std::fs::create_dir_all(&home).expect("home");
    let t = tools();
    assert!(!bundle::tippy_gate(&t, &root, &home).expect("no tippy: n/a"));
    put_exe(&root.join("bin/tippy"), &native("tippy"));
    let mut t = tools();
    let stage_bin = root.join("bin");
    t.answer = Box::new(move |_| {
        Some(ran(
            1,
            "",
            &format!(
                "tippy: setup error: rustc-compatible sibling `{0}/rustc` is not the selected \
                 Trust compiler `{0}/trustc`; repair or reinstall the toolchain",
                stage_bin.display()
            ),
        ))
    });
    let err = bundle::tippy_gate(&t, &root, &home).expect_err("setup error");
    assert!(
        err.to_string().contains(
            "tippy gate FAIL (exit 1) — tippy: setup error: rustc-compatible sibling `bin/rustc` \
             is not the selected Trust compiler `bin/trustc`"
        ),
        "{err}"
    );
    let mut t = tools();
    t.answer = Box::new(|_| {
        Some(ran(
            101,
            "warming up\n",
            "error: could not compile tippy-hello\n",
        ))
    });
    let err = bundle::tippy_gate(&t, &root, &home).expect_err("other failure");
    assert!(
        err.to_string()
            .contains("exit 101) — error: could not compile tippy-hello"),
        "{err}"
    );
    let t = tools();
    assert!(bundle::tippy_gate(&t, &root, &home).expect("clean"));
    let run = &t.runs()[0];
    assert_eq!(run.argv(), ["--offline"]);
    let krate = home.join("tippy-hello");
    assert_eq!(run.cwd.as_deref(), Some(krate.as_path()));
    assert!(krate.join("Cargo.toml").is_file() && krate.join("src/main.rs").is_file());
    assert_eq!(run.env_of("HOME"), Some(&home.clone().into_os_string()));
}

#[test]
fn the_hello_plan_follows_the_program_and_refuses_a_misplaced_sysroot() {
    let def = |name: &str| match programs::get(name).expect("a row").shape {
        Shape::Bundle(b) => b,
        _ => panic!("{name} is a bundle"),
    };
    let tmp = scratch("bundle-hello-plan");
    let good = std::fs::canonicalize(tmp.path())
        .expect("abs")
        .join("sysroot");
    std::fs::create_dir_all(good.join("lib/rustlib")).expect("mkdir");
    assert_eq!(
        bundle::hello_plan(&def("trust"), &HelloGate::default()).expect("p"),
        HelloPlan::Staged
    );
    assert_eq!(
        bundle::hello_plan(
            &def("trust"),
            &HelloGate {
                enabled: false,
                ..HelloGate::default()
            },
        )
        .expect("p"),
        HelloPlan::Skip
    );
    for (name, gate) in [
        (
            "trust",
            HelloGate {
                sysroot: Some(good.clone()),
                ..HelloGate::default()
            },
        ),
        ("trust-mc", HelloGate::default()),
        (
            "trust-mc",
            HelloGate {
                sysroot: Some("relative/sysroot".into()),
                ..HelloGate::default()
            },
        ),
        (
            "trust-mc",
            HelloGate {
                sysroot: Some(tmp.path().join("nothing-here")),
                ..HelloGate::default()
            },
        ),
    ] {
        assert!(
            matches!(bundle::hello_plan(&def(name), &gate), Err(Error::Usage(_))),
            "{name} {gate:?}"
        );
    }
    assert_eq!(
        bundle::hello_plan(
            &def("trust-mc"),
            &HelloGate {
                sysroot: Some(good.clone()),
                ..HelloGate::default()
            },
        )
        .expect("p"),
        HelloPlan::Trust {
            sysroot: good,
            compiler: "trust-mc-compiler".into()
        }
    );
}

/// trust-mc: no bin/trustc of its own, so the hello sysroot's compiler admits the payload,
/// and it must be the commit the packed compiler reports.
fn trust_mc(w: &World) -> (BundleRequest, PathBuf) {
    let mc = w.root.join("mc-sysroot");
    put_exe(&mc.join("bin/trust-mc-driver"), &native("driver"));
    put_exe(&mc.join("bin/trust-mc-compiler"), &native("compiler"));
    let hs = w.root.join("hello-sysroot");
    fixture_toolchain(&hs);
    std::fs::create_dir_all(hs.join("lib/rustlib")).expect("mkdir");
    let mut req = w.request();
    req.program = "trust-mc".into();
    req.sysroot = mc;
    req.hello = HelloGate {
        sysroot: Some(hs.clone()),
        ..HelloGate::default()
    };
    req.triple = Some(Triple::parse("aarch64-unknown-linux-gnu").expect("t"));
    (req, hs)
}

#[test]
fn trust_mc_proves_its_hello_against_the_trust_sysroot_its_compiler_came_from() {
    let w = world("bundle-trust-mc");
    let (req, hs) = trust_mc(&w);
    let mut t = FakeTools::new(SEAL_TRIPLE);
    t.answer = Box::new(|run| {
        if run.argv().iter().any(|a| a == "--sysroot") {
            return Some(ran(0, "", ""));
        }
        if run.program.ends_with("hello") {
            return Some(ran(0, "hello, atpkg sysroot\n", ""));
        }
        None
    });
    let out = bundle::pack_bundle_with(&w.ctx, &req, &t).expect("packed");
    let compile = t
        .runs()
        .into_iter()
        .find(|r| r.argv().iter().any(|a| a == "--sysroot"))
        .expect("the hello compiled");
    assert!(compile.program.ends_with("bin/trust-mc-compiler"));
    assert_eq!(compile.argv()[1], hs.display().to_string());
    assert_eq!(out.exposes, ["trust-mc-driver"]);

    let t = FakeTools::new(SEAL_TRIPLE);
    t.version(
        &req.sysroot.join("bin/trust-mc-compiler"),
        &version_text("9999999999999999999999999999999999999999", SEAL_TRIPLE),
    );
    let err = bundle::pack_bundle_with(&w.ctx, &req, &t).expect_err("another commit");
    assert!(err.to_string().contains("built from"), "{err}");

    let rustup = w
        .root
        .join("fake/.rustup/toolchains/nightly-aarch64-unknown-linux-gnu");
    fixture_toolchain(&rustup);
    std::fs::create_dir_all(rustup.join("lib/rustlib")).expect("mkdir");
    let mut bad = req.clone();
    bad.hello.sysroot = Some(rustup);
    let err =
        bundle::pack_bundle_with(&w.ctx, &bad, &FakeTools::new(SEAL_TRIPLE)).expect_err("rustup");
    assert!(err.to_string().contains("rustup's directory"), "{err}");
}

/// A hello sysroot that is a store build is leased beside the packed sysroot for as long
/// as the pack lives: the hello runs its compiler, and gc must not reclaim it then.
#[test]
fn a_store_built_hello_sysroot_is_leased_while_the_pack_lives() {
    let w = world("bundle-hello-lease");
    let (mut req, _) = trust_mc(&w);
    let hs = w.ctx.pkg_prefix.join("store/trust/7");
    fixture_toolchain(&hs);
    std::fs::create_dir_all(hs.join("lib/rustlib")).expect("mkdir");
    req.hello.sysroot = Some(hs);
    let subject = atpkg::lease::Subject::build("trust", 7).expect("a store subject");
    let in_use = || atpkg::lease::holders(&w.ctx.pkg_prefix, &subject).in_use();
    assert!(!in_use(), "nobody holds it before the pack");
    let tools = FakeTools::new(SEAL_TRIPLE);
    let admitted = bundle::admit(&w.ctx, &req, &tools).expect("admitted");
    assert!(in_use(), "the pack holds the hello sysroot it will run");
    drop(admitted);
    assert!(!in_use(), "and lets it go with the pack");
}

/// Fail closed: a store-built hello sysroot whose lease cannot be laid refuses the pack,
/// saying why, rather than run its compiler unleased.
#[test]
fn a_store_built_hello_sysroot_that_cannot_be_leased_refuses_the_pack() {
    let w = world("bundle-hello-unleased");
    let (mut req, _) = trust_mc(&w);
    let hs = w.ctx.pkg_prefix.join("store/trust/7");
    fixture_toolchain(&hs);
    std::fs::create_dir_all(hs.join("lib/rustlib")).expect("mkdir");
    req.hello.sysroot = Some(hs);
    std::fs::write(w.ctx.pkg_prefix.join(atpkg::lease::DIR), b"").expect("plant");
    let tools = FakeTools::new(SEAL_TRIPLE);
    let Err(err) = bundle::admit(&w.ctx, &req, &tools) else {
        panic!("a hello sysroot that cannot be leased must refuse the pack");
    };
    let said = err.to_string();
    assert!(
        said.contains("no lease on")
            && said.contains("could reclaim it, so nothing runs on it unleased"),
        "{said}"
    );
}

#[test]
fn skipping_trust_mcs_hello_keeps_its_admission_sysroot_in_either_flag_order() {
    let w = world("bundle-trust-mc-skip");
    let (request, hs) = trust_mc(&w);
    for flags in [
        vec![
            "--hello-sysroot".into(),
            hs.display().to_string(),
            "--no-hello".into(),
        ],
        vec![
            "--no-hello".into(),
            "--hello-sysroot".into(),
            hs.display().to_string(),
        ],
    ] {
        let mut args: Vec<String> = vec![
            "trust-mc".into(),
            "--sysroot".into(),
            request.sysroot.display().to_string(),
            "--src-repo".into(),
            w.src.display().to_string(),
            "--out".into(),
            request.out.display().to_string(),
            "--build".into(),
            "1".into(),
            "--version".into(),
            "0.0.0-test".into(),
            "--triple".into(),
            SEAL_TRIPLE.into(),
            "--no-smoke".into(),
            "--no-strip".into(),
            "--unsigned".into(),
        ];
        args.extend(flags);
        let req = bundle::parse_args(&mut w.ctx.clone(), &args).expect("request");
        let mut t = FakeTools::new(SEAL_TRIPLE);
        t.answer = Box::new(|run| {
            assert!(
                !run.argv().iter().any(|a| a == "--sysroot"),
                "hello was skipped"
            );
            None
        });
        let out = bundle::pack_bundle_with(&w.ctx, &req, &t).expect("admitted without hello");
        assert!(out.asset.is_file());
        assert!(
            t.runs()
                .iter()
                .any(|run| run.program == hs.join("bin/trustc"))
        );

        t.version(
            &req.sysroot.join("bin/trust-mc-compiler"),
            &version_text("9999999999999999999999999999999999999999", SEAL_TRIPLE),
        );
        let err = bundle::pack_bundle_with(&w.ctx, &req, &t).expect_err("foreign compiler");
        assert!(err.to_string().contains("built from"), "{err}");
    }
}

#[test]
fn the_version_and_target_are_derived_from_what_is_packed() {
    let w = world("bundle-derive");
    let t = tools();
    assert_eq!(
        bundle::derive_version(&t, &w.src, &w.seal).expect("trust: line"),
        "0.1.0"
    );
    put(
        &w.src.join("release/trust-version.toml"),
        b"[release]\ntrust_product_version = \"0.7.2\"\n",
    );
    git(&w.src, &["add", "-A"]);
    git(&w.src, &["commit", "-q", "-m", "declare the version"]);
    assert_eq!(
        bundle::derive_version(&t, &w.src, &w.seal).expect("declared"),
        "0.7.2"
    );
    let mc = w.root.join("mc");
    put_exe(&mc.join("bin/trust-mc-driver"), &native("d"));
    let mut t2 = tools();
    t2.answer = Box::new(|run| {
        (run.argv() == ["--version"]).then(|| ran(0, "trust-mc-driver 0.4.2\n", ""))
    });
    assert_eq!(
        bundle::derive_version(&t2, &w.root.join("no-src"), &mc).expect("driver"),
        "0.4.2"
    );
    assert!(bundle::derive_version(&t, &w.root.join("no-src"), &w.root.join("nothing")).is_err());

    let mut req = w.request();
    req.triple = None;
    req.version = None;
    let out = bundle::pack_bundle_with(&w.ctx, &req, &tools()).expect("derived");
    assert_eq!(out.triple, seal_triple(), "the staged compiler's -vV host");
    assert_eq!(out.version, "0.7.2");
    let mut mute = tools();
    mute.default_version = None;
    let err = bundle::pack_bundle_with(&w.ctx, &req, &mute).expect_err("mute compiler");
    assert!(matches!(err, Error::Refused(_)), "{err}");
}

#[test]
fn a_dirty_checkout_is_refused_or_stamped_and_never_uploaded() {
    let w = world("bundle-dirty");
    put(&w.src.join("uncommitted"), b"change");
    let err = bundle::pack_bundle_with(&w.ctx, &w.request(), &tools()).expect_err("dirty");
    assert!(err.to_string().contains("DIRTY checkout"), "{err}");
    let mut req = w.request();
    req.allow_dirty = true;
    let out = bundle::pack_bundle_with(&w.ctx, &req, &tools()).expect("stamped");
    assert_eq!(out.version, "0.0.0-test-dirty");
    req.signing = Signing::MachineKey;
    req.upload = true;
    let err = bundle::pack_bundle_with(&w.ctx, &req, &tools()).expect_err("dirty upload");
    assert!(err.to_string().contains("never uploaded"), "{err}");
}

#[test]
fn an_upload_that_cannot_sign_is_refused_before_the_stage() {
    let w = world("bundle-no-key");
    let mut req = w.request();
    req.upload = true;
    assert!(matches!(
        bundle::pack_bundle_with(&w.ctx, &req, &tools()),
        Err(Error::Usage(_))
    ));
    req.signing = Signing::MachineKey;
    std::fs::remove_file(&w.ctx.identity.key).expect("rm key");
    let t = tools();
    let err = bundle::pack_bundle_with(&w.ctx, &req, &t).expect_err("no key");
    assert!(err.to_string().contains("no machine key"), "{err}");
    assert!(!t.log().iter().any(|l| l.starts_with("relocate")));
}

#[test]
fn a_signed_upload_is_verified_by_the_client_and_staged() {
    let w = world("bundle-upload");
    let mut req = w.request();
    req.signing = Signing::MachineKey;
    req.upload = true;
    let out = bundle::pack_bundle_with(&w.ctx, &req, &tools()).expect("uploaded");
    assert!(out.uploaded);
    let parsed = client_verifies(&w.ctx, &out.manifest, out.signature.as_ref().expect("sig"))
        .expect("the client accepts it");
    assert_eq!(parsed.artifacts[0].kind, "sysroot-bundle");
    assert_eq!(parsed.artifacts[0].reloc, "self-contained");
    let rel = w
        .host
        .release_of(&Slug::new("alabsystems", "trust"), "atpkg-trust-1")
        .expect("staged");
    assert_eq!(rel.assets.len(), 3);
}

#[test]
fn a_donor_that_is_no_trust_toolchain_and_a_relocator_refusal_stop_the_pack() {
    let w = world("bundle-donor");
    let mut req = w.request();
    req.donor_roots = vec![w.seal.join("lib")];
    let err = bundle::pack_bundle_with(&w.ctx, &req, &tools()).expect_err("donor");
    assert!(err.to_string().contains("REFUSED"), "{err}");
    let mut t = tools();
    t.relocate_refusal = Some("a stock toolchain library cannot be vendored".into());
    let err = bundle::pack_bundle_with(&w.ctx, &w.request(), &t).expect_err("relocate");
    assert!(err.to_string().contains("cannot be vendored"), "{err}");
    assert!(std::fs::read_dir(w.root.join("out")).map_or(true, |rd| {
        rd.flatten()
            .all(|e| !e.file_name().to_string_lossy().ends_with(".tar.zst"))
    }));
}

fn live_switch(w: &World) {
    std::fs::remove_dir_all(w.ctx.home.join("trust")).expect("rm");
    std::fs::remove_dir_all(w.ctx.home.join("trust.git")).expect("rm");
    trust_repo(&w.ctx.home.join("trust"), MINTED_LEDGER);
}

/// A seed-mint seal under a live switch: admitted by its receipt, `LINEAGE` stamped over
/// the shipped bytes, the payload's provenance its own lineage, the extracted tree admitted
/// by it — and an edit to that record both moves the signed tree_root and is refused by
/// the checker.
#[test]
fn a_seed_mint_seal_ships_its_lineage_stamped_over_the_shipped_bytes() {
    let w = world("bundle-lineage");
    live_switch(&w);
    seed_mint(&w.seal, SEAL_COMMIT);
    let t = tools();
    let out = bundle::pack_bundle_with(&w.ctx, &w.request(), &t).expect("a proven seal packs");
    assert_eq!(out.lineage, atpkg_publish::api::Lineage::Lineage);
    let text = std::fs::read_to_string(&out.manifest).expect("pkg");
    assert!(text.contains("lineage = \"lineage\""), "{text}");
    let x = w.extract(&out.asset);
    let probe = |p: &Path| atpkg_publish::artifact::tools::version(&t, p);
    let admitted = atpkg::lineage::check_compiler_with(
        &x.join("bin"),
        atpkg::lineage::LineagePolicy::Require,
        &probe,
    )
    .expect("the shipped tree is admitted by its own LINEAGE");
    assert!(matches!(
        admitted.lineage,
        atpkg::lineage::LineageProof::Lineage { .. }
    ));
    assert!(text.contains(&format!(
        "trustc_sha256 = \"{}\"",
        admitted.content.trustc_sha256
    )));
    let signed_root = text
        .lines()
        .find_map(|l| l.strip_prefix("tree_root = \""))
        .map(|r| r.trim_end_matches('"').to_string())
        .expect("tree_root");
    assert_eq!(atpkg::tree_root(&x).expect("root"), signed_root);
    let lineage = x.join("LINEAGE");
    let record = std::fs::read_to_string(&lineage).expect("LINEAGE");
    std::fs::write(
        &lineage,
        record.replace(
            "ddc_seed_relation=pin-ancestor",
            "ddc_seed_relation=lineage-disjoint",
        ),
    )
    .expect("edit");
    assert_ne!(
        atpkg::tree_root(&x).expect("root"),
        signed_root,
        "an edit is drift"
    );
    let refused = atpkg::lineage::check_compiler_with(
        &x.join("bin"),
        atpkg::lineage::LineagePolicy::Require,
        &probe,
    )
    .expect_err("an edited LINEAGE");
    assert!(
        refused.iter().any(|r| r.contains("ddc_seed_relation")),
        "{refused:?}"
    );
}

#[test]
fn a_trust_bundles_hello_compiles_against_its_staged_root() {
    let w = world("bundle-hello-staged");
    let mut req = w.request();
    req.hello = HelloGate::default();
    let mut t = tools();
    t.answer = Box::new(|run| {
        if run.argv().iter().any(|a| a == "--sysroot") {
            return Some(ran(0, "", ""));
        }
        run.program
            .ends_with("hello")
            .then(|| ran(0, "hello, atpkg sysroot\n", ""))
    });
    bundle::pack_bundle_with(&w.ctx, &req, &t).expect("packed");
    let compile = t
        .runs()
        .into_iter()
        .find(|r| r.argv().iter().any(|a| a == "--sysroot"))
        .expect("compiled");
    let root = PathBuf::from(&compile.argv()[1]);
    assert!(root.ends_with("root"), "{}", root.display());
    assert!(
        root.parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n.to_string_lossy().starts_with(".atpkg-bundle-trust-")),
        "the stage's own root: {}",
        root.display()
    );
    assert_eq!(compile.program, root.join("bin/rustc"));
}

/// Every phase reads the sysroot admission resolved and leased: a `--sysroot` link
/// repointed after admission (as `aterm pkg install trust` repoints
/// `store/trust/current`) still stages, freezes, stamps and records the admitted build.
#[test]
fn a_sysroot_link_repointed_after_admission_still_packs_the_admitted_build() {
    let w = world("bundle-repoint");
    live_switch(&w);
    seed_mint(&w.seal, SEAL_COMMIT);
    let other = w.root.join("other-seal");
    super::fixture::copy_tree(&w.seal, &other);
    put_exe(&other.join("bin/trustc"), &native("another build's trustc"));
    let link = w.root.join("current");
    std::os::unix::fs::symlink(&w.seal, &link).expect("link");
    let mut req = w.request();
    req.sysroot = link.clone();
    let t = tools();
    let admitted = bundle::admit(&w.ctx, &req, &t).expect("admitted");
    std::fs::remove_file(&link).expect("unlink");
    std::os::unix::fs::symlink(&other, &link).expect("repoint");
    let out = admitted
        .stage()
        .and_then(bundle::Staged::mutate)
        .and_then(bundle::Mutated::relocate)
        .and_then(bundle::Relocated::gate)
        .and_then(bundle::Gated::pack)
        .expect("the admitted build packs");
    assert_eq!(out.lineage, atpkg_publish::api::Lineage::Lineage);
    let x = w.extract(&out.asset);
    assert_eq!(
        std::fs::read(x.join("bin/trustc")).expect("shipped"),
        std::fs::read(w.seal.join("bin/trustc")).expect("admitted"),
        "the admitted build's bytes, not the link's new target"
    );
    let probe = |p: &Path| atpkg_publish::artifact::tools::version(&t, p);
    atpkg::lineage::check_compiler_with(
        &x.join("bin"),
        atpkg::lineage::LineagePolicy::Require,
        &probe,
    )
    .expect("LINEAGE stamped from the admitted build's receipt");
}

#[test]
fn the_switch_is_read_from_the_trust_repo_the_context_names() {
    let w = world("bundle-trust-repo");
    std::fs::remove_dir_all(w.ctx.home.join("trust")).expect("rm");
    std::fs::remove_dir_all(w.ctx.home.join("trust.git")).expect("rm");
    let err = bundle::pack_bundle_with(&w.ctx, &w.request(), &tools())
        .expect_err("no $HOME/trust: the receipt is required");
    assert!(
        err.to_string().contains("no trust checkout at $HOME/trust"),
        "{err}"
    );
    let mut ctx = w.ctx.clone();
    ctx.trust_repo = w.root.join("src/trust");
    trust_repo(&ctx.trust_repo, DORMANT_LEDGER);
    let out = bundle::pack_bundle_with(&ctx, &w.request(), &tools()).expect("dormant there");
    assert_eq!(out.lineage, atpkg_publish::api::Lineage::Unproven);
}

#[test]
fn an_exposed_tool_the_stage_lacks_is_refused_even_without_the_smoke() {
    let w = world("bundle-exposed");
    std::fs::remove_file(w.seal.join("bin/trust-analyzer")).expect("rm");
    let req = w.request();
    assert!(!req.smoke);
    let err = bundle::pack_bundle_with(&w.ctx, &req, &tools()).expect_err("a shim to nothing");
    assert!(
        err.to_string()
            .contains("exposed bin(s) not in the staged sysroot: trust-analyzer"),
        "{err}"
    );
    assert!(std::fs::read_dir(w.root.join("out")).map_or(true, |rd| {
        rd.flatten()
            .all(|e| !e.file_name().to_string_lossy().ends_with(".tar.zst"))
    }));
    assert_eq!(
        w.stages_left(),
        0,
        "a gate that only read the tree keeps no stage"
    );
}
