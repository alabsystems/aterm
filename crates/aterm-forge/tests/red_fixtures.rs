// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-vacuity proofs for `targo --unverified forge check`: each test plants a
//! violation and asserts the gate goes red.
//!
//! Each drives the verb, not a helper: the carve, fork-ledger and liveness
//! fixtures call [`aterm_forge::check::check_report`] over the whole cell
//! matrix; the review-registration and NOTICE fixtures call
//! [`aterm_forge::check::run`] with `--cell mac-arm` (`check_one_cell`), since
//! their obligations are judged before the per-cell loop.
//!
//! # How each fixture is built
//!
//! Each test builds a miniature aterm workspace in its own directory under the
//! cargo target tmpdir (never in the repository or `/tmp`): a `crates/aterm`
//! root plus the web modules `crates/aterm-wasm` and `crates/aterm-gpu-web` (so
//! every cell has a root to resolve; the wasm cells are rooted at the web
//! modules), a real copy of `vendor/indexmap` with its provenance files, the
//! repository's `deny.toml`, a `NOTICE` naming exactly the forks present, a
//! `vendor/forge.toml` ledger over the measured matrix, and a generated
//! `Cargo.lock`. All roots carry the `indexmap` edge, so the patch is live in
//! every cell.
//!
//! Two details matter:
//!
//!   * `git init`: otherwise the fixture inherits this repository's
//!     `.gitignore`, whose `target` rule makes `[OB-10]` report every vendored
//!     path as ignored.
//!   * `exclude = ["vendor", "other"]`: the vendored crates carry the
//!     `[workspace]` stub `[OB-3]` requires, and reaching one as a path
//!     dependency inside the workspace directory is "multiple workspace roots
//!     found". The real repository reaches forks through `[patch.crates-io]`
//!     only.
//!
//! Every test asserts green first, so a red can only come from the plant.

use aterm_forge::check::check_report;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The forge verb over one cell (`targo --unverified forge check --cell
/// mac-arm`, i.e. [`aterm_forge::check::run`]), folded to `(green, transcript)`
/// as `check_report` does: could-not-run is red, with the reason and the
/// transcript so far.
///
/// Used by the `[OB-11]` and `[OB-6]` fixtures, whose violations are judged
/// before the per-cell loop. The `[OB-13]`, `[OB-17]` and `[OB-12]` fixtures
/// keep the whole matrix.
fn check_one_cell(root: &Path) -> (bool, String) {
    match aterm_forge::check::run(root, &["mac-arm".to_string()]) {
        Ok(outcome) => (outcome.ok, outcome.log),
        Err(why) => (false, why),
    }
}

// ---------------------------------------------------------------------------
// The fixture workspace
// ---------------------------------------------------------------------------

/// A miniature aterm workspace, rebuilt from scratch for each test.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    /// The green baseline: one reviewed, fully-provenanced fork (`indexmap`),
    /// live in every cell, named in NOTICE and in the fork ledger, with no
    /// carve rows and no ratchet file.
    fn baseline(name: &str) -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("forge-red-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        // `target/` exists so nested `targo tree` runs keep cargo's rustc info
        // cache (`target/.rustc_info.json`, written only into an EXISTING
        // target dir): 0.75 s per `tree` cold against 0.01 s warm. The real
        // checkout has a `target/` too, and forge never reads the root
        // listing, so no obligation sees it.
        std::fs::create_dir_all(root.join("target")).expect("fixture root");
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
             members = [\"crates/aterm\", \"crates/aterm-wasm\", \"crates/aterm-gpu-web\"]\n\
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
        // The two wasm cells are rooted at the web modules, not at `aterm`.
        for web in ["aterm-wasm", "aterm-gpu-web"] {
            fx.write(
                &format!("crates/{web}/Cargo.toml"),
                &format!(
                    "[package]\n\
                     name = \"{web}\"\n\
                     version = \"0.47.0\"\n\
                     edition = \"2021\"\n\
                     publish = false\n\
                     \n\
                     [lib]\n\
                     crate-type = [\"cdylib\", \"rlib\"]\n\
                     \n\
                     [dependencies]\n\
                     indexmap = {{ path = \"../../vendor/indexmap\" }}\n"
                ),
            );
            fx.write(
                &format!("crates/{web}/src/lib.rs"),
                "// SPDX-License-Identifier: Apache-2.0\n",
            );
        }
        fx.write_notice(&[("indexmap", "2.14.0", "Apache-2.0 OR MIT")]);
        fx.write_ledger("");

        // Own repository, so the parent checkout's `target/` ignore rule does
        // not apply (see the module docs).
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

    /// The fork ledger `[OB-17]` holds to the tree: the measured cell matrix,
    /// one block for `indexmap`, then `extra` (more blocks, or carve rows).
    fn write_ledger(&self, extra: &str) {
        let mut text = format!(
            "[forge]\n\
             loc_method = \"{}\"\n\
             graph_method = \"{}\"\n\
             cells = [\n",
            aterm_forge::policy::LOC_METHOD,
            aterm_forge::policy::GRAPH_METHOD
        );
        for c in aterm_forge::resolve::default_cells() {
            text.push_str(&format!(
                "    {{ name = \"{}\", triple = \"{}\", package = \"{}\" }},\n",
                c.name, c.triple, c.package
            ));
        }
        text.push_str(
            "]\n\
             \n\
             [[fork]]\n\
             name = \"indexmap\"\n\
             version = \"2.14.0\"\n\
             path = \"vendor/indexmap\"\n\
             license = \"Apache-2.0 OR MIT\"\n\
             census.mode = \"scanned\"\n\
             census.namespace = \"indexmap\"\n\
             \n",
        );
        text.push_str(extra);
        self.write("vendor/forge.toml", &text);
    }

    /// Re-resolve after a manifest edit. `check` resolves with `--locked`, so a
    /// stale lockfile would fail the cell rather than the obligation under test.
    fn regen_lock(&self) {
        // `$CARGO` (targo under `targo --unverified test`), else Trust's `targo`;
        // `generate-lockfile` takes no lane flag.
        let exe = std::env::var_os("CARGO").unwrap_or_else(|| "targo".into());
        let out = Command::new(exe)
            .args(["generate-lockfile", "--offline", "--manifest-path"])
            .arg(self.path("Cargo.toml"))
            .current_dir(&self.root)
            .output()
            .expect("targo must be runnable to build a fixture workspace");
        assert!(
            out.status.success(),
            "fixture `{}` did not resolve:\n{}",
            self.root.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A complete, well-formed fork kit, so a planted fork is wrong only in
    /// the way the test is about.
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

/// `[OB-13]` the carve ledger: a path recorded as deleted is present again.
/// Green before, red after, green again once removed.
///
/// The carved path is `vendor/indexmap/src/rayon`, a real module of the fork
/// behind the `rayon` feature, which no aterm build enables.
#[test]
fn a_reinstated_carved_module_reds_the_forge_verb() {
    let fx = Fixture::baseline("carve-ledger");
    fx.remove("vendor/indexmap/src/rayon");
    fx.write_ledger(
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

/// `[OB-11]` the census cross-check: a `[patch.crates-io]` entry with no
/// `REVIEWED_VENDORED_CRATES` row. The planted fork is otherwise complete, so
/// the red can only come from the missing review.
#[test]
fn an_unreviewed_patch_entry_reds_the_forge_verb() {
    let fx = Fixture::baseline("unreviewed-fork");

    let (ok, log) = check_one_cell(fx.root());
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
    fx.write_ledger(
        "[[fork]]\n\
         name = \"forge_fixture_fork\"\n\
         version = \"0.1.0\"\n\
         path = \"vendor/forge_fixture_fork\"\n\
         license = \"MIT\"\n\
         census.mode = \"scanned\"\n\
         census.namespace = \"forge_fixture_fork\"\n",
    );
    fx.write(
        "Cargo.toml",
        "[workspace]\n\
         members = [\"crates/aterm\", \"crates/aterm-wasm\", \"crates/aterm-gpu-web\"]\n\
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

    let (ok, log) = check_one_cell(fx.root());
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

/// `[OB-6]` through the verb: a fork without a NOTICE line keeps `check` red.
#[test]
fn a_notice_that_omits_a_registered_fork_reds_the_forge_verb() {
    let fx = Fixture::baseline("notice-omission");

    let (ok, log) = check_one_cell(fx.root());
    assert!(
        ok,
        "the baseline fixture must be GREEN or this test proves nothing:\n{log}"
    );

    // Plant the violation: the fork is still vendored, patched and live — it
    // has simply been dropped from NOTICE.
    fx.write_notice(&[]);

    let (ok, log) = check_one_cell(fx.root());
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
    let (ok, log) = check_one_cell(fx.root());
    assert!(ok, "restoring the NOTICE line must restore GREEN:\n{log}");
}

/// `[OB-17]` the fork ledger: a `[forge] cells` row forge does not measure, or
/// a `[[fork]]` block describing another copy, turns the verb red.
#[test]
fn a_ledger_that_disagrees_with_the_tree_reds_the_forge_verb() {
    let fx = Fixture::baseline("fork-ledger");

    let (ok, log) = check_report(fx.root());
    assert!(
        ok,
        "the baseline fixture must be GREEN or this test proves nothing:\n{log}"
    );

    // Plant the violation: a header row nothing measures.
    let good = std::fs::read_to_string(fx.path("vendor/forge.toml")).expect("ledger");
    fx.write(
        "vendor/forge.toml",
        &good.replacen(
            "cells = [\n",
            "cells = [\n    { name = \"TOTALLY-BOGUS\", triple = \"sparc64-unknown-none\", \
             package = \"does-not-exist\" },\n",
            1,
        ),
    );
    let (ok, log) = check_report(fx.root());
    assert!(
        !ok,
        "a ledger row forge does not measure must turn the forge verb RED:\n{log}"
    );
    assert!(
        log.contains("[OB-17]") && log.contains("TOTALLY-BOGUS"),
        "the RED must be the fork-ledger obligation, naming the row:\n{log}"
    );

    // Its twin: a fork block recording a version the tree does not carry.
    fx.write(
        "vendor/forge.toml",
        &good.replace("version = \"2.14.0\"", "version = \"2.13.0\""),
    );
    let (ok, log) = check_report(fx.root());
    assert!(
        !ok,
        "a fork block describing another copy must turn the forge verb RED:\n{log}"
    );
    assert!(
        log.contains("[OB-17]") && log.contains("[[fork]] indexmap"),
        "the RED must name the fork block:\n{log}"
    );

    // And the two fields the manifest does not carry: a §4(b) flag the license
    // does not owe (indexmap is `Apache-2.0 OR MIT`), and a census namespace
    // REVIEWED_VENDORED_CRATES does not give it.
    for (planted, names) in [
        (
            good.replace(
                "license = \"Apache-2.0 OR MIT\"\n",
                "license = \"Apache-2.0 OR MIT\"\napache_notice = true\n",
            ),
            "apache_notice = true",
        ),
        (
            good.replace(
                "census.namespace = \"indexmap\"",
                "census.namespace = \"not-indexmap\"",
            ),
            "not-indexmap",
        ),
    ] {
        assert_ne!(planted, good, "the plant must change the ledger");
        fx.write("vendor/forge.toml", &planted);
        let (ok, log) = check_report(fx.root());
        assert!(
            !ok,
            "a fork block that misstates `{names}` must turn the forge verb RED:\n{log}"
        );
        assert!(
            log.contains("[OB-17]") && log.contains(names),
            "the RED must be [OB-17], naming `{names}`:\n{log}"
        );
    }

    fx.write("vendor/forge.toml", &good);
    let (ok, log) = check_report(fx.root());
    assert!(ok, "restoring the ledger must restore GREEN:\n{log}");
}

/// `[OB-12]` patch liveness: a second, unpatched copy of a patched crate
/// resolving beside the fork. Cargo reports nothing (the build is green and
/// `cargo metadata` says the patch is in force), so it is synthesized here as
/// two path packages.
#[test]
fn an_unpatched_sibling_version_reds_the_forge_verb() {
    let fx = Fixture::baseline("unpatched-sibling");

    let (ok, log) = check_report(fx.root());
    assert!(
        ok,
        "the baseline fixture must be GREEN or this test proves nothing:\n{log}"
    );
    // The count comes from the matrix, not a literal.
    let live_all = format!(
        "✓ indexmap live in all {} cell(s)",
        aterm_forge::resolve::default_cells().len()
    );
    assert!(log.contains(&live_all), "{log}");
    // Every obligation REPORTS, GREEN included. A block dropped from the report
    // is no FAIL line, and no fixture plants `[OB-14]`, so nothing else would
    // notice one gone.
    for tag in [
        "[OB-1..OB-10]",
        "[OB-11]",
        "[OB-12]",
        "[OB-13]",
        "[OB-14]",
        "[OB-17]",
    ] {
        assert!(
            log.contains(tag),
            "every obligation must report; `{tag}` did not:\n{log}"
        );
    }

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
    // The wasm cells are rooted at the web modules, so the planted sibling
    // must reach them too, or the every-cell assertion would be vacuous there.
    for web in ["aterm-wasm", "aterm-gpu-web"] {
        fx.write(
            &format!("crates/{web}/Cargo.toml"),
            &format!(
                "[package]\n\
                 name = \"{web}\"\n\
                 version = \"0.47.0\"\n\
                 edition = \"2021\"\n\
                 publish = false\n\
                 \n\
                 [lib]\n\
                 crate-type = [\"cdylib\", \"rlib\"]\n\
                 \n\
                 [dependencies]\n\
                 indexmap = {{ path = \"../../vendor/indexmap\" }}\n\
                 dep_b = {{ path = \"../dep_b\" }}\n"
            ),
        );
    }
    fx.write(
        "Cargo.toml",
        "[workspace]\n\
         members = [\"crates/aterm\", \"crates/aterm-wasm\", \"crates/aterm-gpu-web\", \"crates/dep_b\"]\n\
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
        log.contains("targo tree --target") && log.contains("-i indexmap@3.0.0"),
        "the refusal must name the command that finds the requiring edge:\n{log}"
    );
    // Every cell, not just one: the fork is patched for all five.
    for cell in ["mac-arm", "linux", "win", "wasm-cpu", "wasm-gpu"] {
        assert!(
            log.contains(&format!("cell `{cell}`")),
            "cell `{cell}` must be scored:\n{log}"
        );
    }
}
