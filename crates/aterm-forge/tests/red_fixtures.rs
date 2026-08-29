// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NON-VACUITY PROOFS for `cargo forge check`.
//!
//! `crates/xtask/src/gate.rs`'s `NON_VACUITY_REGISTRY` requires every roster
//! gate to name a real test that PLANTS A VIOLATION and asserts the gate goes
//! RED. These four do exactly that, and each one drives the VERB —
//! [`aterm_forge::check::check_report`], the symbol the roster calls — not a
//! helper inside it. A fixture that exercises a component and is scored as
//! proof of the verb is the specific over-claim that registry exists to stop.
//!
//! # How each fixture is built
//!
//! Every test constructs a MINIATURE ATERM WORKSPACE in its own directory under
//! the cargo target tmpdir (never in the repository, never in `/tmp`): a
//! `crates/aterm` root package so the four cells have something to resolve, a
//! REAL copy of `vendor/indexmap` so the provenance obligations have real
//! provenance files to find, the repository's own `deny.toml`, a `NOTICE`
//! naming exactly the forks present, and a generated `Cargo.lock`.
//!
//! Two details of that construction are load-bearing rather than incidental:
//!
//!   * `git init`. Attest's `[OB-10]` asks `git check-ignore` whether anything
//!     under `vendor/` is swallowed by an ignore rule. A fixture under
//!     `target/` inherits THIS repository's `.gitignore`, whose `target` rule
//!     matches every path in it — MEASURED: `git check-ignore -v --no-index --
//!     vendor/indexmap` prints `.gitignore:4:target` and exits 0. Without its own
//!     repository the fixture would be RED before any violation was planted,
//!     and every test below would pass vacuously in the worst way: by being
//!     unable to be green.
//!   * `exclude = ["vendor", "other"]`. The vendored crates carry the empty
//!     `[workspace]` stub that attest's `[OB-3]` requires, and a stub-bearing
//!     manifest reached as a PATH DEPENDENCY inside the workspace directory is
//!     "multiple workspace roots found in the same workspace" — measured, not
//!     guessed. The real repository never hits this because it reaches its
//!     forks through `[patch.crates-io]` alone.
//!
//! EVERY test asserts GREEN FIRST. Without that, a fixture that is red for some
//! unrelated reason would "prove" the gate can go red while proving nothing.

use aterm_forge::check::check_report;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// The fixture workspace
// ---------------------------------------------------------------------------

/// A miniature aterm workspace, rebuilt from scratch for each test.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    /// The GREEN baseline: one reviewed, fully-provenanced fork (`indexmap`),
    /// live in every cell, named in NOTICE, with no carve ledger and no
    /// ratchet file yet.
    fn baseline(name: &str) -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("forge-red-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("fixture root");
        let fx = Self { root };

        copy_tree(
            &repo_root().join("vendor/indexmap"),
            &fx.path("vendor/indexmap"),
        );
        std::fs::copy(repo_root().join("deny.toml"), fx.path("deny.toml"))
            .expect("the real deny.toml is the license policy under test");

        fx.write(
            "Cargo.toml",
            "[workspace]\n\
             members = [\"crates/aterm\"]\n\
             exclude = [\"vendor\", \"other\"]\n\
             resolver = \"2\"\n\
             \n\
             [patch.crates-io]\n\
             indexmap = { path = \"vendor/indexmap\" }\n",
        );
        fx.write(
            "crates/aterm/Cargo.toml",
            "[package]\n\
             name = \"aterm\"\n\
             version = \"0.47.0\"\n\
             edition = \"2021\"\n\
             publish = false\n\
             \n\
             [dependencies]\n\
             indexmap = { path = \"../../vendor/indexmap\" }\n",
        );
        fx.write(
            "crates/aterm/src/lib.rs",
            "// SPDX-License-Identifier: Apache-2.0\n",
        );
        fx.write_notice(&[("indexmap", "2.14.0", "Apache-2.0 OR MIT")]);

        // See the module docs: without its own repository, this fixture lives
        // inside the aterm checkout's ignored `target/` and attest's [OB-10]
        // reports every vendored path as swallowed.
        let _ = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&fx.root)
            .status();

        fx.regen_lock();
        fx
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.path(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture parent dir");
        }
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }

    fn remove(&self, rel: &str) {
        let path = self.path(rel);
        if path.is_dir() {
            std::fs::remove_dir_all(&path).expect("remove fixture dir");
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// The NOTICE file, in the exact shape attest's `[OB-6]` parses.
    fn write_notice(&self, forks: &[(&str, &str, &str)]) {
        let mut text = String::from(
            "aterm (forge red-fixture workspace)\n\
             Copyright 2026 Andrew Yates\n\
             \n\
             This source distribution includes modified copies of these upstream crates:\n\
             \n",
        );
        for (name, version, license) in forks {
            text.push_str(&format!(
                "- {name} {version}, {license} (`vendor/{name}/`)\n"
            ));
        }
        self.write("NOTICE", &text);
    }

    /// Re-resolve after a manifest edit. `check` resolves with `--locked`, so a
    /// stale lockfile would fail the cell rather than the obligation under test.
    fn regen_lock(&self) {
        let exe = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let out = Command::new(exe)
            .args(["generate-lockfile", "--offline", "--manifest-path"])
            .arg(self.path("Cargo.toml"))
            .current_dir(&self.root)
            .output()
            .expect("cargo must be runnable to build a fixture workspace");
        assert!(
            out.status.success(),
            "fixture `{}` did not resolve:\n{}",
            self.root.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A complete, well-formed fork kit — everything attest asks of a vendored
    /// crate — so that when a test plants one, the ONLY thing wrong with it is
    /// the thing the test is about.
    fn add_wellformed_fork(&self, name: &str, version: &str) {
        let dir = format!("vendor/{name}");
        self.write(
            &format!("{dir}/Cargo.toml"),
            &format!(
                "[package]\n\
                 name = \"{name}\"\n\
                 version = \"{version}\"\n\
                 edition = \"2021\"\n\
                 license = \"MIT\"\n\
                 \n\
                 [workspace]\n"
            ),
        );
        self.write(
            &format!("{dir}/Cargo.toml.orig"),
            "[package]\nname = \"pristine\"\n",
        );
        self.write(
            &format!("{dir}/.cargo_vcs_info.json"),
            "{\n  \"git\": {\n    \"sha1\": \
             \"0000000000000000000000000000000000000000\"\n  },\n  \"path_in_vcs\": \"\"\n}\n",
        );
        self.write(
            &format!("{dir}/LICENSE-MIT"),
            "MIT License\n\nCopyright (c) upstream\n",
        );
        self.write(
            &format!("{dir}/src/lib.rs"),
            "// aterm-trust: fixture fork — one discharged obligation, so [OB-8] holds\n",
        );
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/aterm-forge sits two levels under the workspace root")
        .to_path_buf()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("fixture dir");
    for entry in std::fs::read_dir(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display()))
    {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

// ---------------------------------------------------------------------------
// The proofs
// ---------------------------------------------------------------------------

/// `[OB-13]` — the carve ledger. A path the ledger records as DELETED is
/// present again: GREEN before, RED after, GREEN again once it is removed.
///
/// The carved path is `vendor/indexmap/src/rayon`, a real module of the real
/// fork — feature-gated behind `rayon`, which no aterm build enables — so the
/// reinstatement is a real one rather than a placeholder file.
#[test]
fn a_reinstated_carved_module_reds_the_forge_verb() {
    let fx = Fixture::baseline("carve-ledger");
    fx.remove("vendor/indexmap/src/rayon");
    fx.write(
        "vendor/forge.toml",
        "# The carve ledger: paths this repository has deleted and undertakes to\n\
         # keep deleted.\n\
         [[carved]]\n\
         path = \"vendor/indexmap/src/rayon\"\n\
         reason = \"feature-gated rayon integration; no aterm build enables it\"\n",
    );

    let (ok, log) = check_report(fx.root());
    assert!(
        ok,
        "the baseline fixture must be GREEN or this test proves nothing:\n{log}"
    );
    assert!(
        log.contains("✓ vendor/indexmap/src/rayon still absent"),
        "{log}"
    );

    // Plant the violation: the carved module is back.
    fx.write(
        "vendor/indexmap/src/rayon/mod.rs",
        "// reinstated by hand\n",
    );

    let (ok, log) = check_report(fx.root());
    assert!(
        !ok,
        "a reinstated carved path must turn the forge verb RED:\n{log}"
    );
    assert!(
        log.contains("[OB-13]"),
        "the RED must be the carve-ledger obligation:\n{log}"
    );
    assert!(
        log.contains("vendor/indexmap/src/rayon") && log.contains("EXISTS again"),
        "the refusal must name the reinstated path:\n{log}"
    );
    assert!(
        log.contains("feature-gated rayon integration"),
        "the refusal must quote the ledger's reason:\n{log}"
    );
    assert!(
        log.contains("PRECISION"),
        "a RED report carries the precision note:\n{log}"
    );

    // And it goes green again the moment the tree agrees with the ledger.
    fx.remove("vendor/indexmap/src/rayon");
    let (ok, log) = check_report(fx.root());
    assert!(
        ok,
        "removing the reinstated path must restore GREEN:\n{log}"
    );
}

/// `[OB-11]` — the census cross-check. A `[patch.crates-io]` entry with no
/// `REVIEWED_VENDORED_CRATES` row is a fork nobody reviewed.
///
/// The planted fork is otherwise FLAWLESS — stub, `.cargo_vcs_info.json`,
/// `Cargo.toml.orig`, retained LICENSE, a trust marker, a NOTICE line, an
/// allowed SPDX arm, live in the graph — so the RED can only come from the
/// missing review.
#[test]
fn an_unreviewed_patch_entry_reds_the_forge_verb() {
    let fx = Fixture::baseline("unreviewed-fork");

    let (ok, log) = check_report(fx.root());
    assert!(
        ok,
        "the baseline fixture must be GREEN or this test proves nothing:\n{log}"
    );

    // Plant the violation: a second fork, complete in every respect except
    // that no reviewed row names it.
    fx.add_wellformed_fork("forge_fixture_fork", "0.1.0");
    fx.write_notice(&[
        ("indexmap", "2.14.0", "Apache-2.0 OR MIT"),
        ("forge_fixture_fork", "0.1.0", "MIT"),
    ]);
    fx.write(
        "Cargo.toml",
        "[workspace]\n\
         members = [\"crates/aterm\"]\n\
         exclude = [\"vendor\", \"other\"]\n\
         resolver = \"2\"\n\
         \n\
         [patch.crates-io]\n\
         indexmap = { path = \"vendor/indexmap\" }\n\
         forge_fixture_fork = { path = \"vendor/forge_fixture_fork\" }\n",
    );
    fx.write(
        "crates/aterm/Cargo.toml",
        "[package]\n\
         name = \"aterm\"\n\
         version = \"0.47.0\"\n\
         edition = \"2021\"\n\
         publish = false\n\
         \n\
         [dependencies]\n\
         indexmap = { path = \"../../vendor/indexmap\" }\n\
         forge_fixture_fork = { path = \"../../vendor/forge_fixture_fork\" }\n",
    );
    fx.regen_lock();

    let (ok, log) = check_report(fx.root());
    assert!(
        !ok,
        "an unreviewed path fork must turn the forge verb RED:\n{log}"
    );
    assert!(
        log.contains("[OB-11]"),
        "the RED must be the review-registration obligation:\n{log}"
    );
    assert!(
        log.contains("forge_fixture_fork") && log.contains("REVIEWED_VENDORED_CRATES"),
        "the refusal must name the fork and the registry it is missing from:\n{log}"
    );
    assert!(
        log.contains("crates/aterm-census/src/scan_set.rs"),
        "the refusal must name the file to edit:\n{log}"
    );
}

/// `[OB-6]`, reached through the verb — NOTICE agreement. A fork that ships in
/// the source distribution without a NOTICE line is a redistribution failure,
/// and `check` must not be green while attest says so.
#[test]
fn a_notice_that_omits_a_registered_fork_reds_the_forge_verb() {
    let fx = Fixture::baseline("notice-omission");

    let (ok, log) = check_report(fx.root());
    assert!(
        ok,
        "the baseline fixture must be GREEN or this test proves nothing:\n{log}"
    );

    // Plant the violation: the fork is still vendored, patched and live — it
    // has simply been dropped from NOTICE.
    fx.write_notice(&[]);

    let (ok, log) = check_report(fx.root());
    assert!(
        !ok,
        "a NOTICE that omits a shipped fork must turn the forge verb RED:\n{log}"
    );
    assert!(
        log.contains("[OB-6]") && log.contains("NOTICE does not list fork `indexmap`"),
        "the RED must be the NOTICE-agreement obligation, naming the fork:\n{log}"
    );
    assert!(
        log.contains("[OB-1..OB-10]"),
        "check must report it as a DELEGATED obligation, not silently:\n{log}"
    );

    // Restoring the line restores GREEN: the fixture is not stuck red.
    fx.write_notice(&[("indexmap", "2.14.0", "Apache-2.0 OR MIT")]);
    let (ok, log) = check_report(fx.root());
    assert!(ok, "restoring the NOTICE line must restore GREEN:\n{log}");
}

/// `[OB-12]` — patch liveness, the obligation that justifies this gate. A
/// second, UNPATCHED copy of a patched crate resolving beside the fork.
///
/// This is the shape the real tree carried on Linux until 2026-08-27: `winnow
/// 1.0.3` from the registry beside the `winnow 0.7.15` aterm forked, reached
/// through `accesskit_winit → … → toml_edit 0.25`. Retiring `toml_edit` for the
/// first-party `aterm-toml` retired the fork, so the live instance is gone —
/// which is exactly why it is synthesized here, reduced to two path packages
/// that can be planted and removed. cargo reports the shape as nothing at all:
/// the build is green and `cargo metadata` says the patch is in force.
#[test]
fn an_unpatched_sibling_version_reds_the_forge_verb() {
    let fx = Fixture::baseline("unpatched-sibling");

    let (ok, log) = check_report(fx.root());
    assert!(
        ok,
        "the baseline fixture must be GREEN or this test proves nothing:\n{log}"
    );
    assert!(log.contains("✓ indexmap live in all 4 cell(s)"), "{log}");

    // Plant the violation: an intermediate dependency drags in a second
    // `indexmap` at another major, exactly as toml_edit 0.25 did for winnow.
    fx.write(
        "other/indexmap/Cargo.toml",
        "[package]\n\
         name = \"indexmap\"\n\
         version = \"3.0.0\"\n\
         edition = \"2021\"\n\
         license = \"MIT\"\n\
         \n\
         [workspace]\n",
    );
    fx.write(
        "other/indexmap/src/lib.rs",
        "// the unpatched upstream copy\n",
    );
    fx.write(
        "crates/dep_b/Cargo.toml",
        "[package]\n\
         name = \"dep_b\"\n\
         version = \"0.1.0\"\n\
         edition = \"2021\"\n\
         publish = false\n\
         \n\
         [dependencies]\n\
         indexmap = { path = \"../../other/indexmap\" }\n",
    );
    fx.write(
        "crates/dep_b/src/lib.rs",
        "// SPDX-License-Identifier: Apache-2.0\n",
    );
    fx.write(
        "crates/aterm/Cargo.toml",
        "[package]\n\
         name = \"aterm\"\n\
         version = \"0.47.0\"\n\
         edition = \"2021\"\n\
         publish = false\n\
         \n\
         [dependencies]\n\
         indexmap = { path = \"../../vendor/indexmap\" }\n\
         dep_b = { path = \"../dep_b\" }\n",
    );
    fx.write(
        "Cargo.toml",
        "[workspace]\n\
         members = [\"crates/aterm\", \"crates/dep_b\"]\n\
         exclude = [\"vendor\", \"other\"]\n\
         resolver = \"2\"\n\
         \n\
         [patch.crates-io]\n\
         indexmap = { path = \"vendor/indexmap\" }\n",
    );
    fx.regen_lock();

    let (ok, log) = check_report(fx.root());
    assert!(
        !ok,
        "an unpatched sibling of a patched crate must turn the forge verb RED:\n{log}"
    );
    assert!(
        log.contains("[OB-12]"),
        "the RED must be the patch-liveness obligation:\n{log}"
    );
    assert!(
        log.contains("UNPATCHED `indexmap`") && log.contains("indexmap@3.0.0"),
        "the refusal must name the crate and the sibling version:\n{log}"
    );
    assert!(
        log.contains("cargo tree --target") && log.contains("-i indexmap@3.0.0"),
        "the refusal must name the command that finds the requiring edge:\n{log}"
    );
    // Every cell, not just one: the fork is patched for all four.
    for cell in ["mac-arm", "linux", "win", "wasm"] {
        assert!(
            log.contains(&format!("cell `{cell}`")),
            "cell `{cell}` must be scored:\n{log}"
        );
    }
}
