// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End to end, through a REAL nested `cargo build`: a copy of this crate driven
//! by a rustc WRAPPER whose `-vV` is upstream-shaped (the real compiler's
//! output minus its `trust:` line) must stop in the build script with the
//! refusal — state-aware in both directions — and the same compiler with its
//! line intact must build. This is the wiring the unit tests in src/lib.rs
//! cannot see: that cargo runs the script, that `RUSTC` reaches it, that the
//! message surfaces in cargo's error block, and that the exit status is 1.
//!
//! Unix only (the wrapper is a `sh` script); on Windows the unit tests stand.

#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const REFUSAL_HEAD: &str = "aterm compiles ONLY with the Trust toolchain";
const INSTALL_URL: &str =
    "https://raw.githubusercontent.com/alabsystems/aterm/HEAD/tools/install.sh";

/// The compiler running THIS test: `RUSTC` if the caller pinned one, else the
/// rustc beside the cargo that spawned us (cargo sets `CARGO`; with rustup that
/// is the toolchain's own binary), else whatever `rustc` PATH gives.
fn real_rustc() -> PathBuf {
    if let Some(r) = env::var_os("RUSTC") {
        return PathBuf::from(r);
    }
    if let Some(cargo) = env::var_os("CARGO") {
        let beside = Path::new(&cargo).with_file_name("rustc");
        if beside.is_file() {
            return beside;
        }
    }
    PathBuf::from("rustc")
}

fn cargo() -> PathBuf {
    env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from)
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).expect("write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A standalone package holding this crate's build.rs + src/gate.rs, with an
/// empty lib and the repo's compiler-scoped opt-out (so a Trust compiler builds
/// the script exactly as the workspace does — batteries-off). Fresh per case.
fn scaffold(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("trust-gate-e2e-{name}"));
    let _ = fs::remove_dir_all(&root);
    let pkg = root.join("pkg");
    fs::create_dir_all(pkg.join("src")).unwrap();
    fs::create_dir_all(pkg.join(".cargo")).unwrap();
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::copy(here.join("build.rs"), pkg.join("build.rs")).unwrap();
    fs::copy(here.join("src/gate.rs"), pkg.join("src/gate.rs")).unwrap();
    fs::write(pkg.join("src/lib.rs"), "").unwrap();
    fs::write(
        pkg.join("Cargo.toml"),
        "[package]\nname = \"trust-gate-e2e\"\nversion = \"0.0.0\"\nedition = \"2024\"\nbuild = \"build.rs\"\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        pkg.join(".cargo/config.toml"),
        "[target.'cfg(trust_verify)']\nrustflags = [\"-Ztrust-verify=off\"]\n",
    )
    .unwrap();
    root
}

/// `rustc` wrapper: `-vV` is answered by the real compiler with every `trust:`
/// line removed (upstream-shaped); everything else is forwarded verbatim, so
/// cargo really compiles with the compiler it has.
fn upstream_shaped_wrapper(root: &Path) -> PathBuf {
    let real = real_rustc();
    let wrapper = root.join("rustc-upstream-shaped");
    write_exec(
        &wrapper,
        &format!(
            // `exit 0` AFTER the pipeline, and it is the whole fixture. `exec cmd
            // | grep` does NOT replace this shell: POSIX runs each pipeline stage
            // in a subshell, so `exec` only replaces the subshell and the script
            // FALLS THROUGH to the `exec "$real" "$@"` below — which runs `-vV` a
            // second time, unfiltered. The wrapper then printed the version block
            // twice, the second copy carrying `trust: 0.1.0`, so
            // `is_trust_compiler` saw a Trust compiler and the gate correctly did
            // not refuse. The two tests below were failing because this fixture
            // never simulated an upstream rustc, not because the gate is wrong.
            "#!/bin/sh\nif [ \"$1\" = \"-vV\" ]; then\n  \"{}\" -vV | grep -v '^trust:'\n  exit 0\nfi\nexec \"{}\" \"$@\"\n",
            real.display(),
            real.display()
        ),
    );
    wrapper
}

/// Run the nested build. `path` is the child's ENTIRE PATH (what decides the
/// remedy); the compiler is named absolutely so PATH only has to reach `cc`.
fn build(root: &Path, rustc: &Path, path: &str) -> (bool, String) {
    let out = Command::new(cargo())
        .args(["build", "-q"])
        .current_dir(root.join("pkg"))
        .env("RUSTC", rustc)
        .env("PATH", path)
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("spawn nested cargo");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

const SYSTEM_PATH: &str = "/usr/bin:/bin";

#[test]
fn upstream_shaped_compiler_is_refused_and_names_the_installer_without_aterm() {
    let root = scaffold("installer");
    let wrapper = upstream_shaped_wrapper(&root);
    let (ok, text) = build(&root, &wrapper, SYSTEM_PATH);
    assert!(!ok, "the nested build must FAIL:\n{text}");
    assert!(
        text.contains(REFUSAL_HEAD),
        "refusal must surface in cargo's output:\n{text}"
    );
    assert!(
        text.contains(INSTALL_URL),
        "no aterm on PATH ⇒ the installer:\n{text}"
    );
    assert!(!text.contains("aterm pkg doctor"), "not doctor:\n{text}");
    assert!(
        !text.contains("only accepted on the nightly compiler"),
        "the -Z flag must never be the error:\n{text}"
    );
}

#[test]
fn upstream_shaped_compiler_is_refused_and_names_doctor_with_aterm_on_path() {
    let root = scaffold("doctor");
    let wrapper = upstream_shaped_wrapper(&root);
    // An `aterm` on the child's PATH — any executable file by that name.
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_exec(&bin.join("aterm"), "#!/bin/sh\nexit 0\n");
    let path = format!("{}:{SYSTEM_PATH}", bin.display());
    let (ok, text) = build(&root, &wrapper, &path);
    assert!(!ok, "the nested build must FAIL:\n{text}");
    assert!(text.contains(REFUSAL_HEAD), "{text}");
    assert!(
        text.contains("aterm pkg doctor"),
        "aterm on PATH ⇒ doctor:\n{text}"
    );
    assert!(!text.contains(INSTALL_URL), "not the installer:\n{text}");
}

#[test]
fn the_real_compiler_builds_when_it_is_trust() {
    // Only meaningful under the compiler this repo pins; if the test suite is
    // ever run on an upstream toolchain, the gate is expected to refuse it and
    // the assertion says which world we are in rather than passing vacuously.
    let root = scaffold("trust");
    let real = real_rustc();
    let vv = Command::new(&real).arg("-vV").output().expect("rustc -vV");
    let is_trust = String::from_utf8_lossy(&vv.stdout)
        .lines()
        .any(|l| l.starts_with("trust:"));
    let (ok, text) = build(&root, &real, SYSTEM_PATH);
    if is_trust {
        assert!(ok, "a Trust compiler must pass the gate silently:\n{text}");
        assert!(!text.contains(REFUSAL_HEAD), "{text}");
    } else {
        assert!(!ok, "an upstream compiler must be refused:\n{text}");
        assert!(text.contains(REFUSAL_HEAD), "{text}");
    }
}
