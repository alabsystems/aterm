// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The bundle pack end to end on the real tools: a sysroot of `cc`-built Mach-Os whose
//! `trustc` is a fixture Trust compiler, packed by [`PackBundle`] with the host's `strip`,
//! `codesign`, `otool` (relocation) and the client's lineage checker. The published shape
//! — `bin/rustc` and `bin/trustc` the same program under two ad-hoc signatures — must ship
//! as one inode, one signature, a compiler that runs.
//!
//! macOS only: it needs `cc`, `codesign`, `otool` and Mach-O.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use atpkg_publish::api::{BundlePacker, BundleRequest, HelloGate, Signing};
use atpkg_publish::artifact::bundle::PackBundle;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::testkit::Keys;

use super::fixture::{
    DORMANT_LEDGER, checkout as fixture_checkout, put, put_exe, scratch, set_mode, trust_repo,
};

const COMMIT: &str = "1234567890abcdef1234567890abcdef12345678";
const DRIVER: &str = "librustc_driver-0123456789abcdef";

fn sh(dir: &Path, program: &str, args: &[&str]) {
    let out = aterm_uds::spawnfd::command(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {program}: {e}"));
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn identifier(file: &Path) -> String {
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("codesign").arg("-dv").arg(file),
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .expect("macOS's code-signing service (trustd) answered within 60 s");
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("Identifier=").map(str::to_string))
        .unwrap_or_default()
}

fn ino(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(p).expect("stat").ino()
}

#[test]
fn a_cc_built_sysroot_ships_one_compiler_under_one_signature() {
    crate::shared::need_compressor();
    let tmp = scratch("e2e-bundle");
    let root = std::fs::canonicalize(tmp.path()).expect("root");
    let host = if std::env::consts::ARCH == "aarch64" {
        "aarch64-apple-darwin"
    } else {
        "x86_64-apple-darwin"
    };
    let src = root.join("src");
    put(&src.join("driver.c"), b"int driver(void) { return 42; }\n");
    put(
        &src.join("compiler.c"),
        format!(
            "#include <stdio.h>\n#include <string.h>\nint driver(void);\n\
             int main(int argc, char **argv) {{\n\
               if (argc > 1 && strcmp(argv[1], \"-vV\") == 0) {{\n\
                 printf(\"rustc 1.99.0-dev (123456789 2026-09-30) (trustc 0.1.0)\\nbinary: \
             trustc\\ncommit-hash: {COMMIT}\\ncommit-date: 2026-09-30\\nhost: {host}\\nrelease: \
             1.99.0-dev\\ntrust: 0.1.0\\n\");\n    return 0;\n  }}\n\
               printf(\"tiny-compiler %d\\n\", driver());\n  return 0;\n}}\n"
        )
        .as_bytes(),
    );
    put(
        &src.join("tool.c"),
        b"#include <stdio.h>\nint main(void) { puts(\"trust-mc-driver 0.4.2\"); return 0; }\n",
    );
    let lib = format!("-l{}", DRIVER.trim_start_matches("lib"));
    sh(
        &src,
        "cc",
        &[
            "-dynamiclib",
            "-o",
            &format!("{DRIVER}.dylib"),
            "-install_name",
            &format!("@rpath/{DRIVER}.dylib"),
            "driver.c",
        ],
    );
    sh(
        &src,
        "cc",
        &[
            "-o",
            "compiler",
            "compiler.c",
            "-L.",
            &lib,
            "-Wl,-rpath,@loader_path/../lib",
            "-Wl,-headerpad_max_install_names",
        ],
    );
    sh(
        &src,
        "cc",
        &["-o", "tool", "tool.c", "-Wl,-headerpad_max_install_names"],
    );

    // The sysroot: trustc and a separately signed copy of it as rustc (the published
    // two-signature shape), targo, the one driver (linked into the target lib), the
    // exposed driver tool, and a tippy holding the real one's sibling rule.
    let sysroot = root.join("sysroot");
    let put_file = |rel: &str, from: &Path| {
        let dest = sysroot.join(rel);
        std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
        std::fs::copy(from, &dest).expect("copy");
        dest
    };
    put_file("bin/trustc", &src.join("compiler"));
    put_file("bin/rustc", &src.join("compiler"));
    put_file("bin/targo", &src.join("tool"));
    put_file("bin/trust-mc-driver", &src.join("tool"));
    put_file(
        &format!("lib/{DRIVER}.dylib"),
        &src.join(format!("{DRIVER}.dylib")),
    );
    std::fs::create_dir_all(sysroot.join(format!("lib/rustlib/{host}/lib"))).expect("mkdir");
    std::fs::hard_link(
        sysroot.join(format!("lib/{DRIVER}.dylib")),
        sysroot.join(format!("lib/rustlib/{host}/lib/{DRIVER}.dylib")),
    )
    .expect("link");
    for name in ["rustc", "trustc"] {
        sh(
            &sysroot,
            "codesign",
            &["-f", "-s", "-", &format!("bin/{name}")],
        );
    }
    assert_ne!(
        std::fs::read(sysroot.join("bin/rustc")).expect("r"),
        std::fs::read(sysroot.join("bin/trustc")).expect("t"),
        "the fixture pair differs inside its signatures"
    );
    put_exe(
        &sysroot.join("bin/tippy"),
        b"#!/bin/sh\nd=$(dirname \"$0\"); cmp -s \"$d/rustc\" \"$d/trustc\" || { echo \"tippy: setup error: rustc is not trustc\" >&2; exit 1; }; exit 0\n",
    );

    let home = root.join("home");
    let keys = Keys::mint("m3", &[]);
    let ctx = keys.ctx(&home, Arc::new(FakeHost::new()));
    trust_repo(&home.join("trust"), DORMANT_LEDGER);
    let checkout = root.join("trust-mc");
    fixture_checkout(&checkout, &[("README", b"trust-mc\n")]);
    let out_dir: PathBuf = root.join("out");
    let request = BundleRequest {
        program: "trust-mc".into(),
        sysroot: sysroot.clone(),
        src_repo: checkout,
        build: Some(1),
        version: None,
        triple: None,
        hello: HelloGate {
            enabled: false,
            ..HelloGate::default()
        },
        donor_roots: Vec::new(),
        keep_targets: Vec::new(),
        allow_dirty: false,
        smoke: true,
        strip: true,
        merge_from: None,
        out: out_dir.clone(),
        signing: Signing::Unsigned,
        upload: false,
    };
    let outcome = PackBundle
        .pack_bundle(&ctx, &request)
        .expect("the real pack");
    assert_eq!(
        outcome.triple.as_str(),
        host,
        "the staged compiler's own -vV host"
    );
    assert_eq!(outcome.version, "0.1.0", "the staged trustc's trust: line");

    let x = root.join("x");
    std::fs::create_dir_all(&x).expect("mkdir");
    set_mode(&x, 0o700);
    atpkg::extract_tar_zst(&outcome.asset, &x, 1 << 30, 10_000).expect("the client extracts it");
    assert_eq!(
        ino(&x.join("bin/rustc")),
        ino(&x.join("bin/trustc")),
        "one inode"
    );
    let id = identifier(&x.join("bin/trustc"));
    assert!(!id.is_empty());
    assert_eq!(identifier(&x.join("bin/rustc")), id, "one signature");
    let ran = aterm_uds::spawnfd::command(x.join("bin/trustc"))
        .arg("-vV")
        .output()
        .expect("the shipped compiler runs");
    assert!(String::from_utf8_lossy(&ran.stdout).contains(COMMIT));
    let text = std::fs::read_to_string(&outcome.manifest).expect("pkg");
    let tree_root = text
        .lines()
        .find_map(|l| l.strip_prefix("tree_root = \""))
        .map(|r| r.trim_end_matches('"').to_string())
        .expect("a tree_root");
    assert_eq!(atpkg::tree_root(&x).expect("root"), tree_root);
    assert!(
        std::fs::read(sysroot.join("bin/rustc")).expect("r")
            != std::fs::read(sysroot.join("bin/trustc")).expect("t"),
        "the input sysroot is never written"
    );
}
