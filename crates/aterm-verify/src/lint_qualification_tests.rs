// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Real compiler controls and a matched, local scratch-clone measurement.

use super::*;
use crate::lint_content::{CarryProof, Passed};
use aterm_uds::output::CommandOutput as _;
use std::path::Path;

fn git(root: &Path, args: &[&str]) -> String {
    let output = aterm_uds::spawnfd::command("git")
        .current_dir(root)
        .args(args)
        .output_owned()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn run(ctx: &Ctx) -> (Passed, usize, usize, f64, String) {
    println!(
        "lint start: load={:?} pressure={:?} swap_kib={:?}",
        aterm_sysprobe::load_average(),
        aterm_sysprobe::memory_pressure(),
        aterm_sysprobe::swapped_kib()
    );
    let mut report = Report::new("qualification lint");
    let start = std::time::Instant::now();
    tippy(ctx, &mut report);
    let seconds = start.elapsed().as_secs_f64();
    let mut passed = Passed::new();
    let mut judged = 0;
    let mut reused = 0;
    let mut log = String::new();
    for entry in report.entries {
        match entry {
            crate::ladder::Entry::Raw(text) => {
                for line in text.lines() {
                    let Some(json) = line.strip_prefix("verify-event: ") else {
                        continue;
                    };
                    let row: aterm_json::Value = aterm_json::from_str(json).unwrap();
                    if row["event"].as_str() != Some("lint-package-finished") {
                        continue;
                    }
                    if row["carried"].as_bool() == Some(true) {
                        reused += 1;
                    } else {
                        judged += 1;
                    }
                    if row["outcome"].as_str() == Some("ok")
                        && row["inputs_bounded"].as_bool() == Some(true)
                    {
                        let proof = CarryProof {
                            schema: crate::lint_content::SCHEMA,
                            spec: row["spec"].as_str().unwrap().into(),
                            outcome: "ok".into(),
                            content_key: row["content_key"].as_str().map(str::to_owned),
                            inputs_bounded: true,
                            input_reason: None,
                            execution_s: row["execution_s"].as_f64().unwrap(),
                        };
                        passed.insert(proof.spec.clone(), proof);
                    }
                }
                log.push_str(&text);
                log.push('\n');
            }
            other => log.push_str(&format!("{other:?}\n")),
        }
    }
    (passed, judged, reused, seconds, log)
}

/// Explicitly invoked because it runs real Tippy repeatedly. The two measured trees live
/// in one scratch clone, keeping compiler paths, caches, commands and input roots matched.
#[test]
#[ignore = "real Tippy scratch-clone experiment; run with --ignored --nocapture"]
fn qualification_lint_scratch_clone() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let seed = scratch.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    write(
        &seed,
        "Cargo.toml",
        "[workspace]\nmembers=['a','b','c','d']\nresolver='2'\n",
    );
    write(&seed, "clippy.toml", "msrv = '1.85'\n");
    for name in ["a", "b", "c", "d"] {
        let dependency = if name == "b" {
            "[dependencies]\na={path='../a'}\n"
        } else {
            ""
        };
        write(
            &seed,
            &format!("{name}/Cargo.toml"),
            &format!("[package]\nname='{name}'\nversion='0.0.0'\nedition='2024'\n{dependency}"),
        );
    }
    write(&seed, "a/src/lib.rs", "pub fn value() -> u8 { 1 }\n");
    write(&seed, "b/src/lib.rs", "pub fn consumer() { a::value(); }\n");
    write(&seed, "c/src/lib.rs", "pub fn independent() -> u8 { 1 }\n");
    write(
        &seed,
        "d/src/lib.rs",
        "pub const TEXT: &str = include_str!(\"../../input.txt\");\n",
    );
    write(&seed, "input.txt", "external compile input\n");
    git(&seed, &["init", "--quiet"]);
    git(&seed, &["add", "."]);
    git(
        &seed,
        &[
            "-c",
            "user.name=Lint fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
    );
    let root = scratch.path().join("clone");
    git(
        scratch.path(),
        &[
            "clone",
            "--quiet",
            "--no-hardlinks",
            seed.to_str().unwrap(),
            root.to_str().unwrap(),
        ],
    );
    let base = git(&root, &["rev-parse", "HEAD"]);
    write(&root, "c/src/lib.rs", "pub fn independent() -> u8 { 2 }\n");
    git(&root, &["add", "."]);
    git(
        &root,
        &[
            "-c",
            "user.name=Lint fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "one unrelated crate",
        ],
    );
    let after = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["checkout", "--quiet", &base]);

    let original = aterm_tempfile::manifest_dir()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let environment = crate::EnvSnapshot::capture();
    let tools = Ctx::new(
        original,
        Scope::workspace(),
        environment.clone(),
        scratch.path().join("discovery"),
    )
    .tools;
    let mut environment = environment;
    environment.cargo_target_dir = Some(scratch.path().join("target.noindex"));
    environment.cargo_layout.clear();
    environment.build_slot_held = None;
    let mut ctx = Ctx::new_with_tools(
        root.clone(),
        Scope::workspace(),
        environment,
        scratch.path().join("run"),
        tools,
    );
    std::fs::create_dir_all(&ctx.scratch).unwrap();
    ctx.release = true;
    ctx.carry_context = Some("11".repeat(32));
    // Generate the lock before any content read; subsequent metadata is --locked.
    let lock = aterm_uds::spawnfd::command(&ctx.tools.targo)
        .current_dir(&root)
        .args(["generate-lockfile"])
        .output_owned()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    let warm = run(&ctx);
    assert_eq!((warm.1, warm.2, warm.0.len()), (4, 0, 3), "{}", warm.4);
    let first = run(&ctx);
    assert_eq!((first.1, first.2, first.0.len()), (4, 0, 3), "{}", first.4);
    git(&root, &["checkout", "--quiet", &after]);
    ctx.passed_lint_packages = Some(first.0.clone());
    let second = run(&ctx);
    assert_eq!(
        (second.1, second.2, second.0.len()),
        (2, 2, 3),
        "{}",
        second.4
    );
    println!(
        "qualification matched trees {base} -> {after}: before judged={} reused={} wall={:.6}s; after judged={} reused={} wall={:.6}s",
        first.1, first.2, first.3, second.1, second.2, second.3
    );

    // Same tree and cache, but no reuse: the matched before/after speed comparison.
    ctx.passed_lint_packages = None;
    let control = run(&ctx);
    assert_eq!((control.1, control.2), (4, 0), "{}", control.4);
    println!(
        "same-after-tree no-carry control: judged={} reused={} wall={:.6}s",
        control.1, control.2, control.3
    );
    ctx.passed_lint_packages = Some(second.0);
    write(
        &root,
        "a/src/lib.rs",
        "#[must_use]\npub fn value() -> u8 { 1 }\n",
    );
    let changed = run(&ctx);
    assert_eq!((changed.1, changed.2), (3, 1), "{}", changed.4);
    assert!(
        changed.4.contains("unused return value") && changed.4.contains("must be used"),
        "{}",
        changed.4
    );
    println!(
        "dependency public #[must_use] control: judged={} reused={}; dependent lint failed",
        changed.1, changed.2
    );
    git(&root, &["checkout", "--", "a/src/lib.rs"]);
    write(&root, "clippy.toml", "msrv = '1.86'\n");
    let config = run(&ctx);
    assert_eq!((config.1, config.2), (4, 0), "{}", config.4);
    git(&root, &["checkout", "--", "clippy.toml"]);
    // The declared toolchain identity is independently bound, even when the fixture
    // explicitly supplies the same compiler executables to isolate this negative control.
    write(
        &root,
        "rust-toolchain.toml",
        "[toolchain]\nchannel='trust'\n",
    );
    let toolchain = run(&ctx);
    assert_eq!((toolchain.1, toolchain.2), (4, 0), "{}", toolchain.4);
    println!("clippy.toml and toolchain identity controls each judged=4 reused=0");
}

#[test]
#[ignore = "read production workspace's qualification lint bounds"]
fn qualification_lint_inventory() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = aterm_tempfile::manifest_dir()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let mut ctx = Ctx::new(
        root,
        Scope::workspace(),
        crate::EnvSnapshot::capture(),
        scratch.path().to_path_buf(),
    );
    ctx.release = true;
    ctx.carry_context = Some("11".repeat(32));
    let bin = ctx.tools.tippy.clone().unwrap();
    let plan =
        crate::lint_content::plan(&ctx, |spec| exact_package_tippy_commands(&ctx, &bin, spec))
            .unwrap();
    let bounded = plan
        .packages
        .iter()
        .filter(|p| p.evidence.bounded())
        .count();
    println!(
        "qualification inventory: bounded={bounded} unbounded={} total={}",
        plan.packages.len() - bounded,
        plan.packages.len()
    );
    for package in plan.packages {
        println!(
            "{}\t{}",
            package.spec,
            package.evidence.reason.as_deref().unwrap_or("bounded")
        );
    }
}

/// The release-shaped measurement uses an independent clone and its own persistent target
/// directory across both trees. Its Git sources are local; it performs no release or publication.
#[test]
#[ignore = "full repository qualification lint: warm-up, baseline and changed-tree measurement"]
fn qualification_repository_clone_measurement() {
    let retained = std::env::var_os("ATERM_LINT_EXPERIMENT_ROOT").map(std::path::PathBuf::from);
    let transient = retained
        .is_none()
        .then(|| aterm_tempfile::tempdir().unwrap());
    let scratch = retained.unwrap_or_else(|| transient.as_ref().unwrap().path().to_path_buf());
    let scratch = scratch.canonicalize().unwrap();
    let original = aterm_tempfile::manifest_dir()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let root = scratch.join("repository");
    let _lease = aterm_toolchain::named_lock::take(
        &scratch.join("qualification-lint.lock"),
        &aterm_toolchain::named_lock::Holder::me("qualification-lint-measurement", &root),
        aterm_toolchain::named_lock::Wait::No,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    if root.exists() {
        assert_eq!(
            git(&root, &["remote", "get-url", "origin"]),
            original.to_str().unwrap()
        );
        assert!(git(&root, &["status", "--porcelain"]).is_empty());
        let revision = git(&original, &["rev-parse", "HEAD"]);
        git(&root, &["fetch", "--quiet", "origin", &revision]);
        git(&root, &["checkout", "--quiet", "--detach", &revision]);
    } else {
        git(
            &scratch,
            &[
                "clone",
                "--quiet",
                "--shared",
                original.to_str().unwrap(),
                root.to_str().unwrap(),
            ],
        );
    }
    let submodule = original.join("vendor/astream");
    git(
        &root,
        &[
            "config",
            "submodule.vendor/astream.url",
            submodule.to_str().unwrap(),
        ],
    );
    git(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "vendor/astream",
        ],
    );
    git(&root, &["config", "submodule.recurse", "true"]);
    git(
        &root,
        &["config", "--add", "submodule.active", "vendor/astream"],
    );
    let before_tree = git(&root, &["rev-parse", "HEAD^{tree}"]);
    let mut env = crate::EnvSnapshot::capture();
    env.cargo_target_dir = Some(scratch.join("repository-target.noindex"));
    env.cargo_layout.clear();
    env.build_slot_held = None;
    let mut ctx = Ctx::new(root.clone(), Scope::workspace(), env, scratch.join("run"));
    std::fs::create_dir_all(&ctx.scratch).unwrap();
    ctx.release = true;
    ctx.carry_context = Some("11".repeat(32));
    let warm = run(&ctx);
    println!(
        "repository warm-up: judged={} reused={} bounded={} wall={:.6}s failures={}",
        warm.1,
        warm.2,
        warm.0.len(),
        warm.3,
        warm.4.contains("outcome: Fail")
    );
    assert!(!warm.4.contains("outcome: Fail"), "{}", warm.4);
    let before = run(&ctx);
    assert!(!before.4.contains("outcome: Fail"), "{}", before.4);
    let source = root.join("crates/aterm-heap-count/src/lib.rs");
    let mut text = std::fs::read_to_string(&source).unwrap();
    let at = text.find("\n#[cfg(test)]\nmod tests").unwrap();
    text.insert_str(at, "\n/// Input for a scratch qualification experiment.\npub const QUALIFICATION_EXPERIMENT: u8 = 1;\n");
    std::fs::write(&source, text).unwrap();
    git(&root, &["add", "crates/aterm-heap-count/src/lib.rs"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Lint fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "unrelated one-crate qualification input",
        ],
    );
    let after_tree = git(&root, &["rev-parse", "HEAD^{tree}"]);
    ctx.passed_lint_packages = Some(before.0.clone());
    let after = run(&ctx);
    assert!(!after.4.contains("outcome: Fail"), "{}", after.4);
    assert!(after.2 > 0 && after.1 < before.1, "{}", after.4);
    println!(
        "repository matched trees {before_tree} -> {after_tree}: before judged={} reused={} bounded={} wall={:.6}s; after judged={} reused={} bounded={} wall={:.6}s",
        before.1,
        before.2,
        before.0.len(),
        before.3,
        after.1,
        after.2,
        after.0.len(),
        after.3
    );
}
