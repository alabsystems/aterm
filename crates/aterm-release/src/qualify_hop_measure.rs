// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Opt-in, local-only reproduction of the 0.193.0 run-6/run-8 source delta.
//! No release backend, credential, bundle execution or publication is constructed.

use super::*;
use crate::ledger::GitCli;

const HEADS: [&str; 2] = [
    "47c27ef09d7b0de08103c2f930b58229bfb610a5",
    "5a297ec3fd7ceeb0046c815df2768f92dd13805d",
];

fn git(repo: &Path, args: &[&str]) -> String {
    crate::ledger::git_ok(&GitCli::new(repo), args)
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned()
}

fn write(path: &Path, value: &aterm_json::Value) {
    crate::durable::write_fresh(path, value.to_string()).unwrap();
}

fn compile(
    repo: &Path,
    target: &Path,
    out: &Path,
    label: &str,
    args: &[&str],
) -> aterm_json::Value {
    let driver = crate::gates::trust_stage2_bin().unwrap().join("targo");
    let mut cmd = command(driver);
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let args = aterm_toolchain::build_cache::with_test_graph(&args);
    cmd.current_dir(repo)
        .args(aterm_toolchain::build_cache::configs_after_subcommand(
            &args,
        ))
        .arg("--locked")
        .arg("--message-format=json-render-diagnostics")
        .env("CARGO_TARGET_DIR", target)
        .env(
            aterm_toolchain::build_cache::BUILD_DIR_ENV,
            target.join("build.noindex"),
        )
        .env(
            "CARGO_LOG",
            "targo::core::compiler::fingerprint=info,cargo::core::compiler::fingerprint=info",
        );
    for (name, _) in std::env::vars_os() {
        if aterm_toolchain::build_cache::said_by_cargo_run(&name.to_string_lossy()) {
            cmd.env_remove(name);
        }
    }
    let stdout = crate::durable::create_fresh(&out.join(format!("{label}.jsonl"))).unwrap();
    let stderr = crate::durable::create_fresh(&out.join(format!("{label}.stderr"))).unwrap();
    cmd.stdout(stdout).stderr(stderr);
    let start = Instant::now();
    let usage = cmd.spawn().unwrap().wait_with_rusage().unwrap();
    let messages = std::fs::read_to_string(out.join(format!("{label}.jsonl"))).unwrap();
    let units = units_of(messages.as_bytes()).unwrap();
    let tests: Vec<_> = messages
        .lines()
        .filter_map(|line| {
            let row: aterm_json::Value = aterm_json::from_str(line).ok()?;
            if row["reason"].as_str() != Some("compiler-artifact")
                || row["profile"]["test"].as_bool() != Some(true)
            {
                return None;
            }
            let exe = row["executable"].as_str()?;
            Some(aterm_json::json!({
                "package":row["package_id"], "target":row["target"]["name"],
                "fresh":row["fresh"], "executable":exe,
                "sha256":crate::dmg::sha256_file(Path::new(exe)).unwrap()
            }))
        })
        .collect();
    let result = aterm_json::json!({
        "label":label,"success":usage.status.success(),"wall_s":start.elapsed().as_secs_f64(),
        "cpu_s":usage.cpu.as_secs_f64(),"fresh":units.fresh,"compiled":units.compiled,
        "unknown":units.unknown,"test_binaries":tests
    });
    write(&out.join(format!("{label}.result.json")), &result);
    assert!(usage.status.success(), "{result}; see {}", out.display());
    result
}

/// Run explicitly with ATERM_QUALIFY_HOP_ROOT naming a new absolute .ubd/*.noindex
/// directory. ATERM_QUALIFY_HOP_MODE is `tree` or `slices`; each mode has separate output
/// and can run once on the same fixture. Historical source is never edited.
#[test]
#[ignore = "real two-tree builds and affected test executions; local-only, minutes class"]
fn two_historical_trees() {
    let root = PathBuf::from(std::env::var_os("ATERM_QUALIFY_HOP_ROOT").unwrap());
    assert!(root.is_absolute() && root.components().any(|c| c.as_os_str() == ".ubd"));
    assert_eq!(root.extension().and_then(|s| s.to_str()), Some("noindex"));
    let mode = std::env::var("ATERM_QUALIFY_HOP_MODE").unwrap();
    assert!(matches!(mode.as_str(), "tree" | "slices"));
    let source = aterm_tempfile::manifest_dir()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.join("clone");
    if !repo.exists() {
        git(
            &source,
            &[
                "clone",
                "--shared",
                "--no-checkout",
                source.to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &[
                "config",
                "submodule.vendor/astream.url",
                source.join("vendor/astream").to_str().unwrap(),
            ],
        );
    }
    let output = root.join(&mode);
    std::fs::create_dir(&output)
        .expect("retain prior measurements; choose a new root for a repeat");
    let started = Instant::now();
    let mut rows = Vec::new();
    let mut prior_binaries = aterm_verify::testrun::Passed::new();
    for (index, head) in HEADS.into_iter().enumerate() {
        git(&repo, &["checkout", "--detach", head]);
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
        let commit = git(&repo, &["rev-parse", "HEAD"]);
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]);
        let out = output.join(format!("take{}", index + 1));
        std::fs::create_dir(&out).unwrap();
        println!("hop measurement: {mode} {commit} -> {}", out.display());
        let start = Instant::now();
        let result = if mode == "slices" {
            let take = Take::prepare(&repo, &commit).unwrap();
            let pool = own_slice_pool(&take.temporary_dir).unwrap();
            let results: Vec<_> = [(ARM64, None), (X86_64, Some(X86_64))]
                .into_iter()
                .map(|(triple, target)| {
                    require_rustup_target(X86_64).unwrap();
                    // Sequential to bound memory. Use the cut's exact warm target, source,
                    // release profile and sealed environment; only omit its executable wrapper.
                    let _memory = aterm_capacity::admit(
                        crate::slice_units::slice_need(triple, None),
                        aterm_capacity::Admit::Wait,
                    )
                    .unwrap();
                    let clock = Instant::now();
                    let usage = run_slice_profile(
                        &take,
                        &SliceRun {
                            verb: "build",
                            pkg: "aterm",
                            target,
                            target_dir: take.slice_target(target),
                            build_number: index as u64 + 1,
                            index_hint: None,
                            static_crt: false,
                            units: None,
                        },
                        Some(&out.join(format!("{triple}.log"))),
                        None,
                        Some(&pool.pool),
                        false,
                    )
                    .unwrap();
                    let binary = if target.is_some() {
                        target_bin(&take.slice_target(target), triple, "aterm")
                    } else {
                        take.slice_target(target).join("release/aterm")
                    };
                    let units = usage.units.unwrap();
                    aterm_json::json!({"triple":triple,"wall_s":clock.elapsed().as_secs_f64(),
                    "cpu_ns":usage.cpu_ns,"fresh":units.fresh,"compiled":units.compiled,
                    "unknown":units.unknown,"sha256":crate::dmg::sha256_file(&binary).unwrap()})
                })
                .collect();
            take.verify().unwrap();
            aterm_json::json!({"slices":results})
        } else {
            let target = root.join("tree-target.noindex");
            let tools = compile(
                &repo,
                &target,
                &out,
                "tools",
                &[
                    "--unverified",
                    "build",
                    "--profile",
                    "test",
                    "-p",
                    "aterm-release",
                    "-p",
                    "aterm-verify",
                ],
            );
            let cutter = target.join("debug/aterm-release");
            let verifier = target.join("debug/aterm-verify");
            let cutter_sha = crate::dmg::sha256_file(&cutter).unwrap();
            let verifier_sha = crate::dmg::sha256_file(&verifier).unwrap();
            let tool_set = crate::release_tools::set_digest(&cutter_sha, &verifier_sha);
            let compile = compile(
                &repo,
                &target,
                &out,
                "tests",
                &["--unverified", "test", "--tests", "--no-run", "--workspace"],
            );
            // Offer even the base's candidates directly to hop 2: a stronger adversarial
            // input than the immediate-predecessor rule permits. Changed context must reject
            // every one; the affected unbounded packages must execute regardless of context.
            let offered = prior_binaries.len();
            let proofs = out.join("prior-binaries.jsonl");
            crate::durable::write_fresh(
                &proofs,
                aterm_verify::testrun::encode_passed(&prior_binaries).unwrap(),
            )
            .unwrap();
            let mut cmd = command(&verifier);
            cmd.current_dir(&repo)
                .args(["release", "--tree", "--checks=test"])
                .args(["--events"])
                .arg(out.join("test.events.jsonl"))
                .env("CARGO_TARGET_DIR", &target)
                .env(
                    aterm_toolchain::build_cache::BUILD_DIR_ENV,
                    target.join("build.noindex"),
                )
                .arg("--passed-binaries")
                .arg(proofs)
                .arg("--carry-context")
                .arg(crate::check_content::item_context(&tool_set, "local-audit"));
            let log = crate::durable::create_fresh(&out.join("test.log")).unwrap();
            cmd.stdout(log.try_clone().unwrap()).stderr(log);
            let clock = Instant::now();
            let usage = cmd.spawn().unwrap().wait_with_rusage().unwrap();
            let execution_s = clock.elapsed().as_secs_f64();
            let events = crate::check_record::events(&out.join("test.events.jsonl")).unwrap();
            assert!(events.whole());
            let executed = events.binaries.len();
            let binary_outcomes = events.binaries.clone();
            prior_binaries = events
                .binaries
                .into_iter()
                .filter_map(|b| b.proof().map(|p| (p.spec.clone(), p)))
                .collect();
            let carried = std::fs::read_to_string(out.join("test.events.jsonl"))
                .unwrap()
                .lines()
                .map(|s| s.strip_prefix("verify-event: ").unwrap_or(s))
                .filter_map(|s| aterm_json::from_str::<aterm_json::Value>(s).ok())
                .filter(|e| {
                    e["event"].as_str() == Some("test-binary-finished")
                        && e["carried"].as_bool() == Some(true)
                })
                .count();
            aterm_json::json!({"tools":tools,"compile":compile,"cutter_sha256":cutter_sha,
                "verifier_sha256":verifier_sha,"tool_set_sha256":tool_set,"test_wall_s":execution_s,
                "test_cpu_s":usage.cpu.as_secs_f64(),"test_success":usage.status.success(),
                "offered_receipts":offered,"binary_rows":executed,"carried_binaries":carried,
                "binary_outcomes":binary_outcomes})
        };
        let row = aterm_json::json!({"commit":commit,"tree":tree,"wall_s":start.elapsed().as_secs_f64(),"result":result});
        write(&out.join("result.json"), &row);
        println!(
            "hop measurement finished: {commit}; report {}",
            out.join("result.json").display()
        );
        rows.push(row);
        assert!(git(&repo, &["status", "--porcelain"]).is_empty());
    }
    write(
        &output.join("result.json"),
        &aterm_json::json!({"mode":mode,"wall_s":started.elapsed().as_secs_f64(),"takes":rows}),
    );
    validate_results(&mode, &rows);
}

fn validate_results(mode: &str, rows: &[aterm_json::Value]) {
    assert_eq!(rows.len(), HEADS.len());
    for (row, head) in rows.iter().zip(HEADS) {
        assert_eq!(row["commit"].as_str(), Some(head));
    }
    if mode == "slices" {
        for i in 0..2 {
            assert_eq!(rows[1]["result"]["slices"][i]["compiled"].as_u64(), Some(0));
            assert!(rows[1]["result"]["slices"][i]["fresh"].as_u64().unwrap() > 0);
            assert_eq!(
                rows[0]["result"]["slices"][i]["sha256"],
                rows[1]["result"]["slices"][i]["sha256"]
            );
        }
    } else {
        assert_ne!(
            rows[0]["result"]["cutter_sha256"],
            rows[1]["result"]["cutter_sha256"]
        );
        for row in rows {
            let result = &row["result"];
            assert_eq!(
                result["tool_set_sha256"].as_str(),
                Some(
                    crate::release_tools::set_digest(
                        result["cutter_sha256"].as_str().unwrap(),
                        result["verifier_sha256"].as_str().unwrap(),
                    )
                    .as_str()
                )
            );
            let binaries: Vec<crate::check_record::Binary> =
                aterm_json::from_str(&result["binary_outcomes"].to_string()).unwrap();
            assert_eq!(result["binary_rows"].as_u64(), Some(binaries.len() as u64));
            if result["test_success"].as_bool() == Some(true) {
                assert!(!binaries.is_empty());
                assert!(
                    binaries
                        .iter()
                        .all(|b| b.outcome == crate::check_record::OK)
                );
            }
        }
        let previous: Vec<crate::check_record::Binary> =
            aterm_json::from_str(&rows[0]["result"]["binary_outcomes"].to_string()).unwrap();
        let candidates = previous.iter().filter(|b| b.proof().is_some()).count();
        // A fail-fast base can stop before any bounded reader runs. Zero candidates is
        // evidence, not an invalid experiment; it must never be invented into a carry.
        assert_eq!(
            rows[1]["result"]["offered_receipts"].as_u64(),
            Some(candidates as u64)
        );
        assert_eq!(rows[1]["result"]["carried_binaries"].as_u64(), Some(0));
        assert_eq!(rows[1]["result"]["test_success"].as_bool(), Some(true));
        let current: Vec<crate::check_record::Binary> =
            aterm_json::from_str(&rows[1]["result"]["binary_outcomes"].to_string()).unwrap();
        let affected = |spec: &str| {
            ["atpkg-publish", "aterm-release", "aterm-lab"]
                .iter()
                .any(|package| spec.starts_with(&format!("-p {package} ")))
        };
        let ran = current.iter().filter(|b| affected(&b.spec)).count();
        assert_eq!(
            ran, 13,
            "every affected test target, including lab's integration tests"
        );
        assert!(
            current
                .iter()
                .filter(|b| affected(&b.spec))
                .all(|b| b.execution_s > 0.0)
        );
    }
}

/// Read back the immutable reports without repeating compilation or any test execution.
/// Negative controls corrupt a slice digest, a carried count and a failed base's pass flag.
#[test]
#[ignore = "explicit readback of two_historical_trees output"]
fn saved_historical_reports() {
    let root = PathBuf::from(std::env::var_os("ATERM_QUALIFY_HOP_ROOT").unwrap());
    let mode = std::env::var("ATERM_QUALIFY_HOP_MODE").unwrap();
    assert!(matches!(mode.as_str(), "tree" | "slices"));
    let output = root.join(&mode);
    let report: aterm_json::Value =
        aterm_json::from_str(&std::fs::read_to_string(output.join("result.json")).unwrap())
            .unwrap();
    let rows = report["takes"].as_array().unwrap();
    validate_results(&mode, rows);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(
            row["tree"].as_str(),
            Some(
                git(
                    &root.join("clone"),
                    &["rev-parse", &format!("{}^{{tree}}", HEADS[index]),]
                )
                .as_str()
            )
        );
        if mode == "tree" {
            let event_path = output
                .join(format!("take{}", index + 1))
                .join("test.events.jsonl");
            let events = crate::check_record::events(&event_path).unwrap();
            assert!(events.whole());
            let stored: Vec<crate::check_record::Binary> =
                aterm_json::from_str(&row["result"]["binary_outcomes"].to_string()).unwrap();
            assert_eq!(stored, events.binaries);
            let carried = std::fs::read_to_string(&event_path)
                .unwrap()
                .lines()
                .map(|s| s.strip_prefix("verify-event: ").unwrap_or(s))
                .filter_map(|s| aterm_json::from_str::<aterm_json::Value>(s).ok())
                .filter(|e| {
                    e["event"].as_str() == Some("test-binary-finished")
                        && e["carried"].as_bool() == Some(true)
                })
                .count();
            assert_eq!(
                row["result"]["carried_binaries"].as_u64(),
                Some(carried as u64)
            );
        }
    }
    let mut wrong = rows.to_vec();
    let result = wrong[1]
        .as_object_mut()
        .unwrap()
        .get_mut("result")
        .unwrap()
        .as_object_mut()
        .unwrap();
    if mode == "slices" {
        result.get_mut("slices").unwrap().as_array_mut().unwrap()[0]
            .as_object_mut()
            .unwrap()
            .insert("sha256".into(), aterm_json::json!("00"));
    } else {
        result.insert("carried_binaries".into(), aterm_json::json!(1));
    }
    assert!(std::panic::catch_unwind(|| validate_results(&mode, &wrong)).is_err());
    if mode == "tree" && rows[0]["result"]["test_success"].as_bool() == Some(false) {
        let mut laundered = rows.to_vec();
        laundered[0]
            .as_object_mut()
            .unwrap()
            .get_mut("result")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("test_success".into(), aterm_json::json!(true));
        assert!(std::panic::catch_unwind(|| validate_results(&mode, &laundered)).is_err());
    }
}
