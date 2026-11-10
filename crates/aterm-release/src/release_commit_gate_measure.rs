// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Opt-in real-backend prefix instrument. No release authority is constructed:
//! native tag reads terminate at a local bare, and no publishing verb is called.

use super::*;

fn dry_mode(real: bool, dry: bool) -> Option<bool> {
    match (real, dry) {
        (true, false) => Some(false),
        (false, true) => Some(true),
        _ => None,
    }
}

/// Negative controls: a measured invocation cannot combine modes or default to
/// both, which would let one mode inherit the other's process-local inputs seal.
#[test]
fn measured_invocations_cannot_share_a_process_between_modes() {
    assert_eq!(dry_mode(false, false), None);
    assert_eq!(dry_mode(true, true), None);
    assert_eq!(dry_mode(true, false), Some(false));
    assert_eq!(dry_mode(false, true), Some(true));
}

fn git(repo: &Path, args: &[&str]) -> String {
    crate::ledger::git_ok(&GitCli::new(repo), args)
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned()
}

fn commit(repo: &Path, message: &str) -> String {
    git(repo, &["add", "-A"]);
    git(
        repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            message,
        ],
    );
    git(repo, &["rev-parse", "HEAD"])
}

fn policy(root: &Path) {
    let source = PathBuf::from(
        std::env::var_os("ATERM_COMMIT_GATE_POLICY").expect("read-only policy checkout"),
    );
    let revision = git(&source, &["rev-parse", "HEAD"]);
    let engine = root.join("engine");
    for path in ["baseline/forbidden-content.txt", "registry/repos.toml"] {
        let file = engine.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let bytes = crate::ledger::git_ok(
            &GitCli::new(&source),
            &["show", &format!("{revision}:{path}")],
        )
        .unwrap()
        .stdout;
        std::fs::write(file, bytes).unwrap();
    }
    commit(
        &engine,
        "Copy immutable read-only export policy; no executable engine",
    );
    std::fs::write(root.join("policy-source"), revision).unwrap();
}

fn prepare(root: &Path, source: &Path) {
    assert!(
        !root.exists(),
        "a fixture never overwrites an existing root"
    );
    std::fs::create_dir_all(root).unwrap();
    let bare = root.join("source.git");
    git(
        source,
        &[
            "clone",
            "--mirror",
            source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let repo = root.join("clone");
    git(
        source,
        &[
            "clone",
            "--shared",
            bare.to_str().unwrap(),
            repo.to_str().unwrap(),
        ],
    );
    git(&repo, &["switch", "-qc", "freeze-fixture-commit-gates"]);
    tests::bind_native_engine_fixture(&repo, &bare);
    git(
        &repo,
        &[
            "config",
            "submodule.vendor/astream.url",
            source.join("vendor/astream").to_str().unwrap(),
        ],
    );
    git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "vendor/astream",
        ],
    );
    let base = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("base"), base).unwrap();
    let changelog = repo.join("CHANGELOG.md");
    let text = std::fs::read_to_string(&changelog).unwrap();
    let section = text.find("\n## ").unwrap();
    let text = format!(
        "{}\n- Planted orphan for gate-order measurement.\n{}",
        &text[..section],
        &text[section..]
    );
    std::fs::write(changelog, text).unwrap();
    std::fs::write(root.join("tip"), commit(&repo, "Plant commit-only orphan")).unwrap();
    let engine = root.join("engine");
    crate::ledger::testing::init(
        &engine,
        &[
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
        ],
    );
    std::fs::create_dir_all(engine.join("bin")).unwrap();
    std::fs::write(
        engine.join("bin/pub"),
        "# Read-only fixture policy: run_pre_promote_hook\n",
    )
    .unwrap();
    commit(&engine, "Local policy identity; no executable engine");
    policy(root);
}

#[test]
#[ignore = "explicit real freeze/seal/input/commit-gate ABBA measurement"]
fn real_backend_commit_gate_prefix() {
    let invocation_start = std::time::Instant::now();
    let root =
        PathBuf::from(std::env::var_os("ATERM_COMMIT_GATE_ROOT").expect("private fixture root"));
    assert!(root.is_absolute() && root.components().any(|c| c.as_os_str() == ".ubd"));
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("commit-gates-real-")
    );
    let sample = std::env::var("ATERM_COMMIT_GATE_SAMPLE").expect("prepare, warm, A1, B1, B2, A2");
    if sample == "prepare" {
        prepare(
            &root,
            &std::env::current_dir().unwrap().canonicalize().unwrap(),
        );
        return;
    }
    if sample == "policy" {
        policy(&root);
        return;
    }
    assert!(matches!(
        sample.as_str(),
        "warm" | "A1" | "B1" | "B2" | "A2" | "census" | "red-test"
    ));
    let dry = dry_mode(
        std::env::var_os("ATERM_COMMIT_GATE_REAL_ONLY").is_some(),
        std::env::var_os("ATERM_COMMIT_GATE_DRY_ONLY").is_some(),
    )
    .expect("select exactly one real or dry mode; each needs a fresh process");
    let repo = root.join("clone");
    let mut tip = std::fs::read_to_string(root.join("tip")).unwrap();
    if sample == "census" {
        assert!(
            !root.join("census-tip").exists(),
            "a failed tree is never retried"
        );
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), tip);
        let path = repo.join("crates/aterm-db/src/lib.rs");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("\n#[cfg(test)]\nconst _: &str = env!(\"CARGO_MANIFEST_DIR\");\n");
        std::fs::write(path, text).unwrap();
        tip = commit(&repo, "Plant census control beside the orphan");
        std::fs::write(root.join("census-tip"), &tip).unwrap();
    }
    if sample == "red-test" {
        assert!(
            !root.join("red-test-tip").exists(),
            "a red test is never retried"
        );
        assert_eq!(
            git(&repo, &["rev-parse", "HEAD"]),
            std::fs::read_to_string(root.join("census-tip")).unwrap()
        );
        let path = repo.join("crates/aterm-db/src/lib.rs");
        let text = std::fs::read_to_string(&path).unwrap();
        let text = text
            .strip_suffix("\n#[cfg(test)]\nconst _: &str = env!(\"CARGO_MANIFEST_DIR\");\n")
            .expect("only the owned census control is removed");
        std::fs::write(path, format!("{text}\n#[test]\nfn commit_gate_qualify_red_control() {{\n    let actual = std::hint::black_box(0_u8);\n    assert_eq!(actual, 1);\n}}\n")).unwrap();
        let changelog = repo.join("CHANGELOG.md");
        let text = std::fs::read_to_string(&changelog).unwrap();
        let marker = "\n- Planted orphan for gate-order measurement.\n";
        assert_eq!(text.matches(marker).count(), 1);
        std::fs::write(changelog, text.replace(marker, "")).unwrap();
        tip = commit(
            &repo,
            "Repair source controls and plant a qualify-only red test",
        );
        std::fs::write(root.join("red-test-tip"), &tip).unwrap();
    }
    let base = std::fs::read_to_string(root.join("base")).unwrap();
    crate::release_tools::clean_at(&repo, &tip).unwrap();
    let mut real = Real::new(&repo, Some(&root.join("engine")), false, false).unwrap();
    real.release = root.join("release");
    real.state = root.join("state");
    real.logs = root.join(format!("logs-{sample}"));
    real.engine = root.join("engine");
    real.engine_synced
        .store(true, std::sync::atomic::Ordering::Release);
    std::fs::create_dir_all(&real.logs).unwrap();
    let prover = real.prover(None).unwrap();
    let c = Candidate {
        commit: tip.clone(),
        version: real.version_at(&tip).unwrap(),
        why: "private explicit commit fixture".into(),
        bump_from: None,
        carry: Vec::new(),
    };
    // Warm all takes at the same owned input path. Explicit file transport is
    // confined to preparation; the production input update sees the initialized pin.
    let checkout = real.release.join("input-checkout.noindex");
    crate::input_record::checkout(&real.git(), &checkout, &tip).unwrap();
    git(
        &checkout,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "vendor/astream",
        ],
    );
    let before = sample.starts_with('A');
    // Remove only this fixture's receipt, never an executable or compiler unit.
    // A warm baseline must still pay its actual freshness, sealing and graph work.
    let receipt = crate::release_tools::cutter_record_path(&real.release);
    if receipt.exists() {
        std::fs::remove_file(receipt).unwrap();
    }
    let start = std::time::Instant::now();
    let timing =
        crate::cut_timing::Timer::start(&real.step_log(Step::Checks), "real-commit-gate-prefix");
    let mut rows = Vec::new();
    {
        let started = std::time::Instant::now();
        let mut input = None;
        let work = sample == "warm" || before;
        let gates = |real: &mut Real| {
            let origin = crate::github_remote::claim_fixture(&repo, &root.join("source.git"));
            real.commit_gates_from_origin(&c, dry, Ok(origin))
        };
        let early = (!work).then(|| gates(&mut real));
        let early_available = early
            .as_ref()
            .map(|_| invocation_start.elapsed().as_secs_f64());
        if work && !dry {
            real.frozen_cutter(&tip, &prover).unwrap();
            real.seal_inputs().unwrap();
        }
        if work || dry || sample == "red-test" {
            input = Some(real.input_checks(&base, &tip, &prover));
        }
        if sample != "census"
            && let Some(Err(refusal)) = &input
        {
            panic!("the orphan-only input must pass before comparing gate order: {refusal:?}");
        }
        if sample == "census" && dry {
            let Err(found) = input.as_ref().unwrap() else {
                panic!("the planted census control passed");
            };
            assert!(
                !found.could_not_run && found.why.contains("build-path-value"),
                "{found:?}"
            );
        }
        let found = early.unwrap_or_else(|| gates(&mut real));
        let gate_findings_s =
            early_available.unwrap_or_else(|| invocation_start.elapsed().as_secs_f64());
        let mut qualify_code = None;
        if sample == "red-test" {
            assert!(found.is_empty(), "{found:?}");
            let mut test = real.child(prover.bin.join("targo"), Some(&prover));
            test.current_dir(&checkout).args([
                "--unverified",
                "test",
                "-p",
                "aterm-db",
                "--lib",
                "commit_gate_qualify_red_control",
                "--",
                "--exact",
                "-Z",
                "unstable-options",
                "--format",
                "json",
            ]);
            let ran = real.logged(Step::Qualify, &mut test);
            assert_ne!(ran.code, 0, "the qualify-only mutant passed");
            assert!(
                ran.detail
                    .iter()
                    .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
                    .any(|event| event["type"].as_str() == Some("test")
                        && event["name"].as_str() == Some("commit_gate_qualify_red_control")
                        && event["event"].as_str() == Some("failed")),
                "{ran:?}"
            );
            qualify_code = Some(ran.code);
        } else {
            assert!(
                found.iter().any(|f| f.why.starts_with("changelog")),
                "{found:?}"
            );
        }
        let input = input.map(|r| match r {
            Ok(note) => aterm_json::json!({"outcome": "ok", "note": note}),
            Err(f) => aterm_json::json!({"outcome": "refused", "why": f.why}),
        });
        rows.push(aterm_json::json!({ "dry": dry, "execution_s": started.elapsed().as_secs_f64(), "launch_to_gate_findings_s": gate_findings_s, "launch_to_verdict_s": invocation_start.elapsed().as_secs_f64(), "froze": work && !dry, "input": input, "findings": found.iter().map(|f| &f.why).collect::<Vec<_>>(), "qualify_code": qualify_code }));
    }
    println!(
        "real-commit-order-sample {}",
        aterm_json::json!({ "sample": sample, "rows": rows, "execution_s": start.elapsed().as_secs_f64(), "commit": tip, "base": base, "authority": "local-bare tag transport; no publication entry point" })
    );
    drop(timing);
}
