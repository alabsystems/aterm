// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Opt-in real-backend prefix instrument. No release authority is constructed:
//! native tag reads terminate at a local bare, and no publishing verb is called.
//! Preparation derives a gate-clean announcement before planting the orphan;
//! measurements from older fixture roots retain their original gate vectors.

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

fn repository_root(cwd: &Path) -> PathBuf {
    PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"]))
        .canonicalize()
        .unwrap()
}

#[test]
fn measurement_source_resolves_the_checkout_from_a_nested_package() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().join("source");
    crate::ledger::testing::init(&repo, &[]);
    let package = repo.join("crates/release");
    std::fs::create_dir_all(&package).unwrap();
    assert_eq!(repository_root(&package), repo.canonicalize().unwrap());
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

// Only the fixture's announcement is derived. Actual source versions and tags
// remain inputs: a source that owes a bump is not a usable measurement baseline.
fn baseline_notes(repo: &Path, bare: &Path) -> Result<String, String> {
    if !git(repo, &["status", "--porcelain"]).is_empty() {
        return Err("measurement baseline must start clean".into());
    }
    let runner = GitCli::new(repo);
    let source = git(repo, &["rev-parse", "HEAD"]);
    let version = crate::publish::workspace_version(&git(repo, &["show", "HEAD:Cargo.toml"]))
        .map_err(|e| e.to_string())?;
    let origin = crate::github_remote::claim_fixture(repo, bare);
    let tags = crate::ledger::Origin::tag_catalog(&origin)?;
    let (release, released) = crate::harness_versions::previous_release(
        &git(repo, &["show", "HEAD:RELEASES.ledger"]),
        &version,
        &tags,
    )
    .map_err(|e| e.to_string())?
    .ok_or("measurement needs an actual public predecessor")?;
    let owed = crate::harness_versions::owed_between(&runner, &released, &source)
        .map_err(|e| e.to_string())?;
    if !owed.is_empty() {
        return Err(format!("measurement source owes harness bumps: {owed:?}"));
    }
    let text = std::fs::read_to_string(repo.join("CHANGELOG.md")).map_err(|e| e.to_string())?;
    let entries: Vec<_> = crate::changelog::note_entries(&text, "Unreleased")
        .into_iter()
        .filter(|entry| entry.starts_with(crate::changelog::HARNESS_ENTRY_HEAD))
        .collect();
    let (start, end) =
        crate::changelog::section_span(&text, "Unreleased").ok_or("missing Unreleased section")?;
    if entries.len() > 1
        || entries.len() == 1
            && !text.lines().take(end).skip(start + 1).any(|line| {
                line.starts_with(crate::changelog::HARNESS_ENTRY_HEAD)
                    && line.split_whitespace().collect::<Vec<_>>().join(" ") == entries[0]
            })
    {
        return Err("measurement requires one complete unambiguous announcement".into());
    }
    if crate::harness_versions::notes_gate_at(&runner, (&release, &released), &source, &version)
        .is_ok()
    {
        return Ok(text);
    }
    if entries.is_empty() {
        return Err("invalid notes have no unambiguous announcement to normalize".into());
    }
    let bumped = crate::harness_versions::bump(repo, &runner, (&release, &released))
        .map_err(|e| e.to_string())?;
    if !bumped.is_empty() || git(repo, &["diff", "--name-only", "HEAD"]) != "CHANGELOG.md" {
        return Err(
            "baseline normalization must change only CHANGELOG without version bumps".into(),
        );
    }
    std::fs::read_to_string(repo.join("CHANGELOG.md")).map_err(|e| e.to_string())
}

fn fixture_gates(root: &Path, tip: &str) -> Vec<Refusal> {
    let repo = root.join("clone");
    let mut real = Real::new(&repo, Some(&root.join("engine")), false, false).unwrap();
    real.release = root.join("release");
    real.state = root.join("state");
    real.logs = root.join("logs-prepare");
    real.engine_synced
        .store(true, std::sync::atomic::Ordering::Release);
    std::fs::create_dir_all(&real.logs).unwrap();
    let c = Candidate {
        commit: tip.into(),
        version: real.version_at(tip).unwrap(),
        why: "private baseline validation".into(),
        bump_from: None,
        carry: Vec::new(),
    };
    real.commit_gates_from_origin(
        &c,
        // Setup checks the candidate without taking a seal or writing a real
        // gate record. Timed samples still use their explicitly selected mode.
        true,
        Ok(crate::github_remote::claim_fixture(
            &repo,
            &root.join("source.git"),
        )),
    )
}

fn orphan_only(found: &[Refusal]) -> bool {
    found.len() == 1 && !found[0].could_not_run && found[0].why.starts_with("changelog — ")
}

#[test]
fn baseline_announcement_uses_real_tagged_history_without_changing_versions() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    crate::ledger::testing::init(&repo, &[]);
    std::fs::write(
        repo.join("Cargo.toml"),
        "[workspace.package]\nversion = \"0.3.0\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("RELEASES.ledger"), "1 0.1.0\n2 0.2.0\n").unwrap();
    let notes = "## [Unreleased]\n\n### Changed\n\n- **Harness versions since v0.2.0: stale.**\n\n## [0.1.0]\n\n### Fixed\n\n- Shipped history.\n";
    std::fs::write(repo.join("CHANGELOG.md"), notes).unwrap();
    for h in aterm_phase::CliHarness::ALL {
        let path = repo.join(h.version_file());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, crate::harness_versions::version_line((0, 1))).unwrap();
    }
    commit(&repo, "Actual public source");
    git(&repo, &["tag", "v0.1.0"]);
    let h = aterm_phase::CliHarness::ALL[0];
    let version_path = repo.join(h.version_file());
    std::fs::write(&version_path, crate::harness_versions::version_line((0, 2))).unwrap();
    commit(&repo, "Advanced source version");
    let bare = dir.path().join("origin.git");
    git(
        &repo,
        &[
            "clone",
            "--mirror",
            repo.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    tests::bind_native_engine_fixture(&repo, &bare);
    let derived = baseline_notes(&repo, &bare).unwrap();
    let announcement = derived
        .lines()
        .find(|line| line.starts_with(crate::changelog::HARNESS_ENTRY_HEAD))
        .unwrap();
    assert!(announcement.starts_with("- **Harness versions since v0.1.0: "));
    assert!(announcement.contains(&format!("`alab {} harness 0.2`", h.cli())));
    assert_eq!(
        derived,
        notes.replace("- **Harness versions since v0.2.0: stale.**", announcement)
    );
    assert_eq!(
        std::fs::read_to_string(&version_path).unwrap(),
        crate::harness_versions::version_line((0, 2))
    );
    std::fs::write(repo.join("CHANGELOG.md"), &derived).unwrap();
    commit(&repo, "Already valid fixture announcement");
    let valid_head = git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(baseline_notes(&repo, &bare).unwrap(), derived);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), valid_head);
    for body in [
        format!("{announcement}\n{announcement}\n"),
        format!("{announcement}\n  continuation\n"),
        format!("{announcement}\n*italic continuation*\n"),
        format!("{announcement}\n#literal continuation\n"),
    ] {
        let ambiguous = format!("## [Unreleased]\n\n### Changed\n\n{body}\n## [0.1.0]\n");
        std::fs::write(repo.join("CHANGELOG.md"), &ambiguous).unwrap();
        let head = commit(&repo, "Ambiguous note must refuse before normalization");
        assert!(baseline_notes(&repo, &bare).is_err());
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            std::fs::read_to_string(repo.join("CHANGELOG.md")).unwrap(),
            ambiguous
        );
    }
    std::fs::write(repo.join("CHANGELOG.md"), &derived).unwrap();
    commit(&repo, "Restore unambiguous baseline");
    let unbumped = format!(
        "{}\nfn changed_source() {{}}\n",
        crate::harness_versions::version_line((0, 1))
    );
    std::fs::write(&version_path, &unbumped).unwrap();
    commit(&repo, "Source changes without its required bump");
    assert!(
        baseline_notes(&repo, &bare)
            .unwrap_err()
            .contains("owes harness bumps")
    );
    assert_eq!(std::fs::read_to_string(&version_path).unwrap(), unbumped);
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
    std::fs::write(root.join("source"), git(&repo, &["rev-parse", "HEAD"])).unwrap();
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
    let changelog = repo.join("CHANGELOG.md");
    let before = std::fs::read_to_string(&changelog).unwrap();
    let notes = baseline_notes(&repo, &bare).unwrap();
    if notes != before {
        std::fs::write(&changelog, notes).unwrap();
        commit(
            &repo,
            "Derive fixture announcement from actual release inputs",
        );
    }
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let baseline = fixture_gates(root, &base);
    assert!(baseline.is_empty(), "invalid baseline: {baseline:?}");
    std::fs::write(root.join("base"), &base).unwrap();
    let text = std::fs::read_to_string(&changelog).unwrap();
    let section = text.find("\n## ").unwrap();
    std::fs::write(
        &changelog,
        format!(
            "{}\n- Planted orphan for gate-order measurement.\n{}",
            &text[..section],
            &text[section..]
        ),
    )
    .unwrap();
    let tip = commit(&repo, "Plant commit-only orphan");
    let found = fixture_gates(root, &tip);
    assert!(orphan_only(&found), "not an orphan-only fixture: {found:?}");
    std::fs::write(root.join("tip"), &tip).unwrap();
    println!(
        "real-commit-order-prepared {}",
        aterm_json::json!({
            "source": std::fs::read_to_string(root.join("source")).unwrap(),
        "base": base, "tip": tip, "setup_gate_dry": true, "baseline_findings": [],
            "mutated_findings": found.iter().map(|f| &f.why).collect::<Vec<_>>()
        })
    );
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
        prepare(&root, &repository_root(&std::env::current_dir().unwrap()));
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
    let checkout = crate::input_record::directory(&real.release);
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
            assert!(orphan_only(&found), "{found:?}");
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
