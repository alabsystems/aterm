// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The complete production path from `verify -p <fork>` selection to Forge's OB-7 verdict.

use super::*;
use std::path::{Path, PathBuf};

const CASE: &str = "forge_pull_e2e_tests::a_dev_fork_selection_carries_an_added_file_to_ob7";
const CHILD: &str = "ATERM_FORGE_PULL_E2E_CHILD";
const ROOT: &str = "ATERM_FORGE_PULL_E2E_ROOT";
const ADDED: &str = "crates/aterm-window/src/ob7_negative.rs";

fn write(root: &Path, rel: &str, body: impl AsRef<[u8]>) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("fixture path has a parent"))
        .expect("create fixture directory");
    std::fs::write(path, body).expect("write fixture file");
}

fn git(root: &Path, args: &[&str]) {
    let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
        .current_dir(root)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture(root: &Path) {
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/aterm-window\"]\nresolver = \"2\"\n\
         \n[workspace.package]\nversion = \"0.1.0\"\n",
    );
    write(
        root,
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = \"aterm-window\"\nversion = \"0.30.13\"\n",
    );
    write(
        root,
        "NOTICE",
        "- winit 0.30.13, Apache-2.0 (`crates/aterm-window/`)\n",
    );
    write(
        root,
        "deny.toml",
        "[licenses]\nallow = [\"Apache-2.0\"]\n\n[bans]\ndeny = []\n\n[sources]\nallow-git = []\n",
    );
    write(
        root,
        "vendor/forge.toml",
        "[forge]\nloc_method = \"rs-physical-all-files-v1\"\n\
         graph_method = \"cargo-tree-normal-no-dedupe-locked-offline-v1\"\n",
    );
    write(
        root,
        "crates/aterm-window/Cargo.toml",
        "# Modified by aterm: winit is maintained here under its aterm package name.\n\
         [package]\nname = \"aterm-window\"\nversion = \"0.30.13\"\nlicense = \"Apache-2.0\"\n",
    );
    write(root, "crates/aterm-window/LICENSE", "Apache License\n");
    write(
        root,
        "crates/aterm-window/src/lib.rs",
        "pub fn upstream() {}\n",
    );
    write(root, ADDED, "pub fn added_by_aterm() {}\n");
    // `in_checkout`, which protects every production Forge stage, recognizes the fixture.
    write(
        root,
        "crates/aterm-gui/Cargo.toml",
        "[package]\nname = \"marker\"\n",
    );
    // Forge asks its pinned cargo for all eight cell graphs. This fixture has no patch rows, so a
    // root-only graph answers every graph-dependent obligation without weakening Forge itself.
    write(
        root,
        "tree",
        "pkg=aterm\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = -p ]; then pkg=$2; shift; fi\n  shift\ndone\nprintf '0%s v0.1.0\\n' \"$pkg\"\n",
    );
    git(root, &["init", "-q"]);
    git(root, &["add", "."]);
}

fn pristine(tool_home: &Path, root: &Path) {
    let cache = aterm_uds::host::user_cache(std::env::consts::OS, Some(tool_home), None, "forge")
        .expect("this platform has a user cache under an absolute tool home");
    let entry = cache.join("crates/winit-0.30.13");
    let package = entry.join("package");
    write(
        &package,
        "Cargo.toml",
        "[package]\nname = \"winit\"\nversion = \"0.30.13\"\nlicense = \"Apache-2.0\"\n",
    );
    write(&package, "LICENSE", "Apache License\n");
    write(&package, "src/lib.rs", "pub fn upstream() {}\n");
    std::fs::create_dir_all(&entry).expect("create pristine cache entry");
    std::fs::write(entry.join("sha256"), format!("{}\n", "00".repeat(32)))
        .expect("stamp pristine cache entry");

    assert!(
        !package.join("src/ob7_negative.rs").exists(),
        "the planted file must be added relative to pristine"
    );
    assert!(root.join(ADDED).is_file());
}

fn toolchain(root: &Path) -> Toolchain {
    Toolchain {
        stage2_dir: root.to_path_buf(),
        targo: PathBuf::from("/bin/sh"),
        trustdoc: PathBuf::from("/bin/false"),
        tippy: None,
        trustfmt: None,
        borrowed: Vec::new(),
        refused: None,
        store_bin: None,
        demoted: None,
        lock: None,
        lock_refusal: None,
        lock_override: None,
    }
}

fn forge_report(root: &Path) -> Report {
    let parsed = cli::parse(["-p", "aterm-window"]).expect("verify -p invocation parses");
    let scope = Scope::packages(&parsed.packages);
    let (wanted, _) = planned_checks(&parsed, &scope, root);
    assert!(
        wanted.contains(&Check::Forge),
        "production `verify -p aterm-window` plan did not pull Forge: {wanted:?}"
    );
    let scratch = root.join("scratch");
    std::fs::create_dir_all(&scratch).expect("create verifier scratch");
    let ctx = Ctx::new_with_tools(
        root.to_path_buf(),
        scope,
        EnvSnapshot {
            path: std::env::var_os("PATH").unwrap_or_default(),
            home: root.join("home"),
            ..EnvSnapshot::default()
        },
        scratch,
        toolchain(root),
    )
    .with_checks(&wanted)
    .with_no_windows(true);
    let plan = plan::plan(&ctx);
    let forge = plan
        .iter()
        .find(|stage| stage.id == plan::StageId::Forge)
        .expect("the production plan carries the pulled Forge stage");
    stages::run_stage(&ctx, forge)
}

fn child(root: &Path) {
    let report = forge_report(root);
    let rendered = report.render();
    assert!(
        report
            .outcomes()
            .any(|(outcome, _)| outcome == Outcome::Fail(Severity::GateFailed)),
        "an added file with no notice must red the production Forge stage:\n{rendered}"
    );
    assert!(
        rendered.contains("FAIL [OB-7]") && rendered.contains(ADDED),
        "the red must be the planted OB-7 finding, not another fixture defect:\n{rendered}"
    );

    write(
        root,
        ADDED,
        "// Added by aterm: production Forge-selection negative control.\n\
         pub fn added_by_aterm() {}\n",
    );
    let repaired = forge_report(root);
    let rendered = repaired.render();
    assert!(
        repaired
            .outcomes()
            .any(|(outcome, _)| outcome == Outcome::Ok),
        "the same production Forge stage must pass once the added file carries its notice:\n\
         {rendered}"
    );
    assert!(
        !repaired
            .outcomes()
            .any(|(outcome, _)| outcome == Outcome::Fail(Severity::GateFailed)),
        "no unrelated fixture red may make the negative control vacuous:\n{rendered}"
    );

    std::fs::remove_file(root.join("vendor/forge.toml")).expect("remove authoritative ledger");
    let missing_policy = forge_report(root);
    let rendered = missing_policy.render();
    assert!(
        missing_policy
            .outcomes()
            .any(|(outcome, _)| outcome == Outcome::Fail(Severity::GateFailed)),
        "an absorbed-only fork must not make the missing authoritative ledger optional:\n\
         {rendered}"
    );
    assert!(
        rendered.contains("FAIL [OB-17]") && rendered.contains("vendor/forge.toml"),
        "the missing-policy red must be Forge's ledger obligation, not an unrelated defect:\n\
         {rendered}"
    );
}

#[test]
fn a_dev_fork_selection_carries_an_added_file_to_ob7() {
    if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new("1")) {
        let root = PathBuf::from(std::env::var_os(ROOT).expect("child fixture root"));
        println!("entered Forge pull end-to-end child");
        child(&root);
        return;
    }

    let fixture_dir = aterm_tempfile::TempDir::new().expect("fixture root");
    let tool_home = aterm_tempfile::TempDir::new().expect("fixture tool home");
    fixture(fixture_dir.path());
    pristine(tool_home.path(), fixture_dir.path());
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
        .env(CHILD, "1")
        .env(ROOT, fixture_dir.path())
        .env("ATERM_TEST_TOOL_HOME", tool_home.path())
        .env_remove("XDG_CACHE_HOME");
    let output =
        aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
            .expect("isolated Forge pull child finishes");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("entered Forge pull end-to-end child"),
        "isolated Forge pull child did not enter and pass: {}\n{}\n{}",
        output.status,
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
}
