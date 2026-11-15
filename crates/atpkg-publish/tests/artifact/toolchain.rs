// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The pack's toolchain: an explicit bin dir wins, and a wrong one is refused rather than set
//! aside; the store's `current` resolves to its physical build dir, which a later flip cannot
//! move; no store is a refusal naming the product's fix first; the lease is taken once on the
//! physical build; the client's own checker admits the compiler before anything is built.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use atpkg_publish::artifact::lineage_switch::Switch;
use atpkg_publish::artifact::relocate::Report;
use atpkg_publish::artifact::tarwrite::Compressor;
use atpkg_publish::artifact::toolchain::{self, Held, Source};
use atpkg_publish::artifact::tools::{self, Ran, Run, RunError, Tools};
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::Error;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::testkit::Keys;
use atpkg_publish::triple::Triple;

use super::fixture::{
    COMMIT, FakeTools, put, put_exe, scratch, toolchain as fixture_toolchain, version_text,
};

/// The ordinary collector keeps a tail; complete collection preserves the fake's full report.
struct TailCapture(FakeTools);

impl Tools for TailCapture {
    fn run(&self, run: &Run) -> Result<Ran, RunError> {
        let mut result = self.0.run(run)?;
        let discarded = result
            .stdout
            .len()
            .saturating_sub(aterm_uds::deadline::OUTPUT_KEEP);
        result.stdout.drain(..discarded);
        Ok(result)
    }

    fn run_complete(&self, run: &Run, limit: usize) -> Result<Ran, RunError> {
        self.0.run_complete(run, limit)
    }

    fn strip(&self, file: &Path, locals_and_debug: bool) -> Result<(), String> {
        self.0.strip(file, locals_and_debug)
    }

    fn resign(&self, file: &Path) -> Result<(), String> {
        self.0.resign(file)
    }

    fn same_unsigned(&self, a: &Path, b: &Path) -> Result<bool, String> {
        self.0.same_unsigned(a, b)
    }

    fn thin(&self, file: &Path, arch: &str, out: &Path) -> Result<(), String> {
        self.0.thin(file, arch, out)
    }

    fn relocate(&self, stage: &Path, donors: &[PathBuf]) -> Result<Report, String> {
        self.0.relocate(stage, donors)
    }

    fn compressor(&self) -> &dyn Compressor {
        self.0.compressor()
    }
}

#[test]
fn compiler_identity_cannot_replace_an_invalid_first_line_with_a_valid_tail() {
    let compiler = Path::new("fixture-compiler");
    let report = version_text(COMMIT, "aarch64-apple-darwin");
    let runner = TailCapture(FakeTools::new("aarch64-apple-darwin"));
    runner.0.version(compiler, &report);
    let observed = tools::version(&runner, compiler).expect("ordinary complete report");
    assert!(atpkg::lineage::parse_version_verbose(&observed).is_ok());

    let tail = format!(
        "{report}{}",
        "\n".repeat(aterm_uds::deadline::OUTPUT_KEEP - report.len())
    );
    let whole = format!("not a Trust compiler report\n{tail}");
    runner.0.version(compiler, &whole);

    let truncated = runner
        .run(&Run::new(compiler, ["-vV"]).bounded())
        .expect("negative control: ordinary tail capture");
    assert!(
        atpkg::lineage::parse_version_verbose(
            std::str::from_utf8(&truncated.stdout).expect("ASCII report")
        )
        .is_ok(),
        "discarding the real first line changes identity admission"
    );
    let observed = tools::version(&runner, compiler).expect("complete report within the bound");
    assert!(
        atpkg::lineage::parse_version_verbose(&observed).is_err(),
        "the compiler's actual first line must be judged"
    );
    assert!(observed == whole, "the report must remain complete");
}

#[test]
fn compiler_identity_capture_has_an_inclusive_output_bound() {
    let compiler = Path::new("fixture-compiler");
    let runner = FakeTools::new("aarch64-apple-darwin");
    for extra in [0, 1] {
        let report = "x".repeat(aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES + extra);
        runner.version(compiler, &report);
        assert_eq!(tools::version(&runner, compiler).is_ok(), extra == 0);
    }
}

#[test]
fn compiler_identity_refuses_invalid_utf8_in_stdout() {
    let compiler = Path::new("fixture-compiler");
    let runner = FakeTools::new("aarch64-apple-darwin");
    let report = version_text(COMMIT, "aarch64-apple-darwin");
    let index = report.find("trust: ").expect("identity field") + "trust: ".len();
    let mut bytes = report.into_bytes();
    bytes[index] = 0xff;
    assert!(atpkg::lineage::parse_version_verbose(&String::from_utf8_lossy(&bytes)).is_ok());
    runner.version_bytes(compiler, &bytes);
    assert!(tools::version(&runner, compiler).is_err());
}

fn ctx(home: &Path, xdg: Option<&Path>) -> Ctx {
    let mut c = Keys::mint("m3", &[]).ctx(home, Arc::new(FakeHost::new()));
    c.pkg_prefix = atpkg::toolchain::atpkg_prefix(home, xdg);
    c
}

/// A store build `<prefix>/store/trust/<build>` and `current` → it.
fn store(prefix: &Path, build: u64) {
    fixture_toolchain(&prefix.join(format!("store/trust/{build}")));
    let current = prefix.join("store/trust/current");
    let _ = std::fs::remove_file(&current);
    #[cfg(unix)]
    std::os::unix::fs::symlink(build.to_string(), current).expect("current");
}

#[test]
#[cfg(unix)]
fn the_store_resolves_physically_and_a_later_flip_does_not_move_the_pack() {
    let tmp = scratch("tc-store");
    let c = ctx(tmp.path(), None);
    store(&c.pkg_prefix, 7);
    store(&c.pkg_prefix, 8);
    store(&c.pkg_prefix, 7);
    let tc = toolchain::resolve(&c, None).expect("the store answers");
    assert_eq!(tc.source, Source::Store);
    let physical = std::fs::canonicalize(c.pkg_prefix.join("store/trust/7/bin")).expect("7");
    assert_eq!(tc.bin, physical);
    assert_eq!(tc.trustc(), physical.join("trustc"));
    store(&c.pkg_prefix, 8);
    assert_eq!(
        tc.bin, physical,
        "resolved once: `current` moving is not the pack's"
    );
}

#[test]
#[cfg(unix)]
fn the_configured_prefix_is_the_store_the_pack_reads() {
    let tmp = scratch("tc-prefix");
    let xdg = tmp.path().join("xdg");
    let prefix = tmp.path().join("elsewhere/pkg");
    put(
        &xdg.join("aterm/aterm.toml"),
        format!("[packages]\nprefix = \"{}\"\n", prefix.display()).as_bytes(),
    );
    let c = ctx(tmp.path(), Some(&xdg));
    store(&prefix, 9);
    let tc = toolchain::resolve(&c, None).expect("the configured store");
    assert!(
        tc.bin.ends_with("store/trust/9/bin"),
        "{}",
        tc.bin.display()
    );
}

#[test]
#[cfg(unix)]
fn an_explicit_bin_dir_wins_and_a_wrong_one_is_refused_never_set_aside() {
    let tmp = scratch("tc-explicit");
    let c = ctx(tmp.path(), None);
    store(&c.pkg_prefix, 7);
    let seal = tmp.path().join("seal");
    fixture_toolchain(&seal);
    std::os::unix::fs::symlink(&seal, tmp.path().join("seal-link")).expect("link");
    let tc = toolchain::resolve(&c, Some(&tmp.path().join("seal-link/bin"))).expect("explicit");
    assert_eq!(tc.source, Source::Explicit);
    assert_eq!(
        tc.bin,
        std::fs::canonicalize(seal.join("bin")).expect("seal")
    );

    let half = tmp.path().join("half-built/bin");
    put_exe(&half.join("trustc"), b"\x7fELF");
    let err = toolchain::resolve(&c, Some(&half)).expect_err("no targo");
    assert!(
        err.to_string().contains("holds no executable targo")
            && err.to_string().contains("not set aside"),
        "{err}"
    );

    let lonely = tmp.path().join("lonely/bin");
    put_exe(&lonely.join("targo"), b"\x7fELF");
    let err = toolchain::resolve(&c, Some(&lonely)).expect_err("no trustc");
    assert!(err.to_string().contains("no sibling trustc"), "{err}");
}

/// No store and no `--toolchain`: a refusal, never a stock-cargo fallback.
#[test]
fn no_store_is_a_refusal() {
    let tmp = scratch("tc-none");
    let c = ctx(tmp.path(), None);
    let err = toolchain::resolve(&c, None).expect_err("no toolchain anywhere");
    assert!(matches!(err, Error::Refused(_)), "{err}");
}

#[test]
#[cfg(unix)]
fn the_lease_is_taken_on_the_physical_store_build_and_none_outside_the_store() {
    let tmp = scratch("tc-lease");
    let c = ctx(tmp.path(), None);
    store(&c.pkg_prefix, 7);
    let tc = toolchain::resolve(&c, None).expect("store");
    let held = toolchain::lease(&c, &tc.bin, "atpkg-publish pack ay").expect("leased");
    let Held::Lease(lease) = &held else {
        panic!("a store build is leased: {held:?}");
    };
    assert_eq!(
        Some(lease.subject().clone()),
        atpkg::lease::Subject::build("trust", 7)
    );
    assert!(
        atpkg::lease::holders(&c.pkg_prefix, lease.subject()).in_use(),
        "the lease is visible to gc"
    );
    drop(held);
    let seal = tmp.path().join("seal");
    fixture_toolchain(&seal);
    assert!(matches!(
        toolchain::lease(&c, &seal.join("bin"), "x").expect("unmanaged"),
        Held::Nothing
    ));
    assert!(toolchain::lease(&c, &tmp.path().join("gone"), "x").is_err());
}

#[test]
fn the_compiler_is_admitted_by_the_clients_checker_under_the_switch() {
    let tmp = scratch("tc-admit");
    let bin = fixture_toolchain(&tmp.path().join("seal"));
    let tools = FakeTools::new("aarch64-apple-darwin");
    let c = toolchain::admit(&tools, &bin, &Switch::Dormant).expect("dormant: unproven");
    assert_eq!(c.lineage, atpkg::lineage::LineageProof::Unproven);
    let err = toolchain::admit(&tools, &bin, &Switch::Live).expect_err("live: no receipt");
    assert!(
        err.to_string()
            .contains("carries neither LINEAGE nor SEED-MINT"),
        "{err}"
    );
    let err = toolchain::admit(&tools, &bin, &Switch::Unknown("no answer".into()))
        .expect_err("unknown is required");
    assert!(err.to_string().contains("carries neither"), "{err}");

    let genesis = tmp.path().join("genesis");
    fixture_toolchain(&genesis);
    put_exe(
        &genesis.join("bin/trustc"),
        b"#!/bin/sh\nexec \"$HOME/.cargo/bin/rustc\" \"$@\"\n",
    );
    let err = toolchain::admit(&tools, &genesis.join("bin"), &Switch::Dormant)
        .expect_err("a genesis-style wrapper");
    assert!(err.to_string().contains("interpreter script"), "{err}");

    let rustup = tmp
        .path()
        .join("fake/.rustup/toolchains/stable-aarch64-apple-darwin");
    fixture_toolchain(&rustup);
    let err = toolchain::admit(&tools, &rustup.join("bin"), &Switch::Dormant)
        .expect_err("rustup's directory");
    assert!(err.to_string().contains("rustup's directory"), "{err}");

    assert_eq!(
        toolchain::host_of(&tools, &bin.join("trustc")).expect("host"),
        Triple::parse("aarch64-apple-darwin").expect("t")
    );
}
