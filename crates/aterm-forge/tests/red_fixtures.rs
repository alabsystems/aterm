// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-vacuity red-fixture tests for `targo --unverified forge check`
//! ([`aterm_forge::check::run`], [`FORK`]).

#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the third-party survey behind targo --unverified forge"
)]

use aterm_forge::model::PkgId;
use std::path::{Path, PathBuf};
use std::process::Command;

/// unpacked. Its license, `Apache-2.0 OR MIT`, elects the MIT arm ([OB-7]).
const FORK: &str = "zeroize";
const FORK_LICENSE: &str = "Apache-2.0 OR MIT";

/// [`FORK`]'s version, read off the repository's lock rather than written here.
fn fork_version() -> String {
    let lock = std::fs::read_to_string(repo_root().join("Cargo.lock")).expect("Cargo.lock");
    let name = format!("name = \"{FORK}\"");
    let mut rows = lock.lines();
    rows.find(|row| *row == name)
        .and_then(|_| rows.next())
        .and_then(|row| row.strip_prefix("version = \""))
        .and_then(|v| v.strip_suffix('"'))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("Cargo.lock carries no `{FORK}` package"))
}

/// The root manifest: the three roots, `members_extra`, and the patch rows.
fn workspace_manifest(members_extra: &str, patch_extra: &str) -> String {
    format!(
        "[workspace]\n\
         members = [\"crates/aterm\", \"crates/aterm-wasm\", \"crates/aterm-gpu-web\"{members_extra}]\n\
         exclude = [\"vendor\", \"other\"]\n\
         resolver = \"2\"\n\
         \n\
         [patch.crates-io]\n\
         {FORK} = {{ path = \"vendor/{FORK}\" }}\n\
         {patch_extra}"
    )
}

/// could-not-run is red, with the reason and the transcript so far.
fn check_one_cell(root: &Path) -> (bool, String) {
    match aterm_forge::check::run(
        &aterm_forge::resolve::Cargo::from_env(),
        root,
        &["mac-arm".to_string()],
    ) {
        Ok(outcome) => (outcome.ok, outcome.log),
        Err(why) => (false, why),
    }
}

/// cargo cannot resolve, is could-not-run, so red.
fn check_every_cell(root: &Path) -> (bool, String) {
    match aterm_forge::check::run(&aterm_forge::resolve::Cargo::from_env(), root, &[]) {
        Ok(outcome) => (outcome.ok, outcome.log),
        Err(why) => (false, why),
    }
}

/// beside a hand run) never build in, or remove, one another's.
struct Fixture {
    root: PathBuf,
    _dir: aterm_tempfile::TempDir,
}

impl Fixture {
    /// carve rows and no ratchet file.
    fn baseline(name: &str) -> Self {
        let tmp = env!("CARGO_TARGET_TMPDIR");
        std::fs::create_dir_all(tmp).expect("the target tmpdir");
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("forge-red-{name}-"))
            .tempdir_in(tmp)
            .expect("fixture root");
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("target")).expect("fixture root");
        let fx = Self { root, _dir: dir };

        let upstream =
            aterm_forge::loc::package_dir(&repo_root(), &PkgId::new(FORK, fork_version()), None)
                .unwrap_or_else(|| panic!("{FORK} is unpacked in this checkout's registry"));
        let fork = fx.path(&format!("vendor/{FORK}"));
        copy_tree(&upstream, &fork);
        fx.remove(&format!("vendor/{FORK}/.cargo-ok"));
        fx.remove(&format!("vendor/{FORK}/Cargo.lock"));
        let manifest = std::fs::read_to_string(fork.join("Cargo.toml")).expect("fork manifest");
        fx.write(
            &format!("vendor/{FORK}/Cargo.toml"),
            &format!("{manifest}\n[workspace]\n"),
        );
        let lib = std::fs::read_to_string(fork.join("src/lib.rs")).expect("fork lib.rs");
        fx.write(
            &format!("vendor/{FORK}/src/lib.rs"),
            &format!("{lib}\n// LOCAL PATCH (aterm): the red fixtures' one marked change\n"),
        );
        std::fs::copy(repo_root().join("deny.toml"), fx.path("deny.toml"))
            .expect("the real deny.toml is the license policy under test");

        fx.write("Cargo.toml", &workspace_manifest("", ""));
        fx.write("crates/aterm/Cargo.toml", &aterm_manifest(""));
        fx.write(
            "crates/aterm/src/lib.rs",
            "// SPDX-License-Identifier: Apache-2.0\n",
        );
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
                     {FORK} = {{ path = \"../../vendor/{FORK}\" }}\n"
                ),
            );
            fx.write(
                &format!("crates/{web}/src/lib.rs"),
                "// SPDX-License-Identifier: Apache-2.0\n",
            );
        }
        fx.write_notice(&[(FORK, &fork_version(), FORK_LICENSE)]);
        fx.write_ledger("");

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

    /// block for [`FORK`], then `extra` (more blocks, or carve rows).
    fn write_ledger(&self, extra: &str) {
        let mut text = format!(
            "[forge]\n\
             loc_method = \"{}\"\n\
             graph_method = \"{}\"\n",
            aterm_forge::policy::LOC_METHOD,
            aterm_forge::policy::GRAPH_METHOD
        );
        text.push_str(&format!(
            "\n\
             [[fork]]\n\
             name = \"{FORK}\"\n\
             version = \"{}\"\n\
             path = \"vendor/{FORK}\"\n\
             license = \"{FORK_LICENSE}\"\n\
             census.mode = \"scanned\"\n\
             census.namespace = \"{FORK}\"\n\
             \n",
            fork_version()
        ));
        text.push_str(extra);
        self.write("vendor/forge.toml", &text);
    }

    /// stale lockfile would fail the cell rather than the obligation under test.
    fn regen_lock(&self) {
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

/// `crates/aterm`'s manifest: the [`FORK`] edge, then `extra` dependency rows.
fn aterm_manifest(extra: &str) -> String {
    format!(
        "[package]\n\
         name = \"aterm\"\n\
         version = \"0.47.0\"\n\
         edition = \"2021\"\n\
         publish = false\n\
         \n\
         [dependencies]\n\
         {FORK} = {{ path = \"../../vendor/{FORK}\" }}\n\
         {extra}"
    )
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

/// only for x86 targets; the fixture deletes it.
#[test]
fn a_reinstated_carved_module_reds_the_forge_verb() {
    let fx = Fixture::baseline("carve-ledger");
    let carved = format!("vendor/{FORK}/src/x86.rs");
    fx.remove(&carved);
    fx.write_ledger(&format!(
        "# The carve ledger: paths this repository has deleted and undertakes to\n\
         # keep deleted.\n\
         [[carved]]\n\
         path = \"{carved}\"\n\
         reason = \"target-gated x86 module; the fixture keeps it deleted\"\n",
    ));

    let (ok, log) = check_every_cell(fx.root());
    assert!(
        ok,
        "the baseline fixture must be GREEN in every default cell, or a cell no longer \
         resolves what aterm ships (and this test proves nothing):\n{log}"
    );

    fx.write(&carved, "// reinstated by hand\n");

    let (ok, log) = check_one_cell(fx.root());
    assert!(
        !ok,
        "a reinstated carved path must turn the forge verb RED:\n{log}"
    );
    assert!(
        log.contains("FAIL [OB-13]"),
        "the RED must be the carve-ledger obligation:\n{log}"
    );
}

/// missing review.
#[test]
fn an_unreviewed_patch_entry_reds_the_forge_verb() {
    let fx = Fixture::baseline("unreviewed-fork");

    let (ok, log) = check_one_cell(fx.root());
    assert!(
        ok,
        "the baseline fixture must be GREEN or this test proves nothing:\n{log}"
    );

    fx.add_wellformed_fork("forge_fixture_fork", "0.1.0");
    fx.write_notice(&[
        (FORK, &fork_version(), FORK_LICENSE),
        ("forge_fixture_fork", "0.1.0", "MIT"),
    ]);
    fx.write(
        "Cargo.toml",
        &workspace_manifest(
            "",
            "forge_fixture_fork = { path = \"vendor/forge_fixture_fork\" }\n",
        ),
    );
    fx.write(
        "crates/aterm/Cargo.toml",
        &aterm_manifest("forge_fixture_fork = { path = \"../../vendor/forge_fixture_fork\" }\n"),
    );
    fx.regen_lock();

    let (ok, log) = check_one_cell(fx.root());
    assert!(
        !ok,
        "an unreviewed path fork must turn the forge verb RED:\n{log}"
    );
    assert!(
        log.contains("FAIL [OB-11]"),
        "the RED must be the review-registration obligation:\n{log}"
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

    fx.write_notice(&[]);

    let (ok, log) = check_one_cell(fx.root());
    assert!(
        !ok,
        "a NOTICE that omits a shipped fork must turn the forge verb RED:\n{log}"
    );
    assert!(
        log.contains("FAIL [OB-6]"),
        "the RED must be the NOTICE-agreement obligation:\n{log}"
    );
}

/// the verb red.
#[test]
fn a_ledger_that_disagrees_with_the_tree_reds_the_forge_verb() {
    let fx = Fixture::baseline("fork-ledger");

    let (ok, log) = check_one_cell(fx.root());
    assert!(
        ok,
        "the baseline fixture must be GREEN or this test proves nothing:\n{log}"
    );

    let good = std::fs::read_to_string(fx.path("vendor/forge.toml")).expect("ledger");
    let recorded = format!("version = \"{}\"", fork_version());
    assert!(
        good.contains(&recorded),
        "the ledger records the fork's version"
    );
    fx.write(
        "vendor/forge.toml",
        &good.replace(&recorded, "version = \"0.0.1-another-copy\""),
    );
    let (ok, log) = check_one_cell(fx.root());
    assert!(
        !ok,
        "a fork block describing another copy must turn the forge verb RED:\n{log}"
    );
    assert!(
        log.contains("FAIL [OB-17]"),
        "the RED must be [OB-17]:\n{log}"
    );

    let planted = good.replace(
        "license = \"Apache-2.0 OR MIT\"\n",
        "license = \"Apache-2.0 OR MIT\"\napache_notice = true\n",
    );
    assert_ne!(planted, good, "the plant must change the ledger");
    fx.write("vendor/forge.toml", &planted);
    let (ok, log) = check_one_cell(fx.root());
    assert!(!ok && log.contains("FAIL [OB-17]"), "{planted}\n{log}");
}

/// two path packages.
#[test]
fn an_unpatched_sibling_version_reds_the_forge_verb() {
    let fx = Fixture::baseline("unpatched-sibling");

    let (ok, log) = check_one_cell(fx.root());
    assert!(
        ok,
        "the baseline fixture must be GREEN or this test proves nothing:\n{log}"
    );

    fx.write(
        &format!("other/{FORK}/Cargo.toml"),
        &format!(
            "[package]\n\
             name = \"{FORK}\"\n\
             version = \"3.0.0\"\n\
             edition = \"2021\"\n\
             license = \"MIT\"\n\
             \n\
             [workspace]\n"
        ),
    );
    fx.write(
        &format!("other/{FORK}/src/lib.rs"),
        "// the unpatched upstream copy\n",
    );
    fx.write(
        "crates/dep_b/Cargo.toml",
        &format!(
            "[package]\n\
             name = \"dep_b\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\
             publish = false\n\
             \n\
             [dependencies]\n\
             {FORK} = {{ path = \"../../other/{FORK}\" }}\n"
        ),
    );
    fx.write(
        "crates/dep_b/src/lib.rs",
        "// SPDX-License-Identifier: Apache-2.0\n",
    );
    fx.write(
        "crates/aterm/Cargo.toml",
        &aterm_manifest("dep_b = { path = \"../dep_b\" }\n"),
    );
    fx.write("Cargo.toml", &workspace_manifest(", \"crates/dep_b\"", ""));
    fx.regen_lock();

    let (ok, log) = check_one_cell(fx.root());
    assert!(
        !ok,
        "an unpatched sibling of a patched crate must turn the forge verb RED:\n{log}"
    );
    assert!(
        log.contains("FAIL [OB-12]"),
        "the RED must be the patch-liveness obligation:\n{log}"
    );
}
