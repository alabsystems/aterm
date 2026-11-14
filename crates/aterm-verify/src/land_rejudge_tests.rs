// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A local landing replay with real dependency resolution and compiled test binaries.

use super::*;

const SOURCE: &str = "pub fn value() -> u8 { dep::value() }\n\n\n\n\n\n\n\npub const MAIN: u8 = 0;\n\n#[test]\nfn contract() { assert_eq!(value(), 1); }\n";

/// Main changes unrelated crates and the branch's file. Dependency moves must still compile
/// and test the branch against the new dependency. NEGATIVE CONTROL: the branch freezes the
/// dependency's old value, violating the merged test, and the re-judge refuses the landing.
/// In those three main's change reaches the branch's whole cone, so the first judgment leaves
/// nothing standing and the merge is judged in full. In `narrow` the branch also changes
/// `fan0`, and main changes `fan1` and the shared file, neither of which reaches `fan0`: the
/// first judgment stands, and one round judges the merge over the crates of the branch's cone
/// that main's change reaches, `a` and `consumer`, by the real reach and the real selection of
/// a round. NEGATIVE CONTROLS: `narrow-conflict` freezes the dependency's old value there too,
/// and the round refuses the landing; in `narrow-wide` main changes `wide`, which `fan0`
/// builds on, so its change reaches the branch's every crate and the merge is judged in full.
/// Run explicitly: `targo --unverified test -p aterm-verify --lib landing_rejudge_replay
/// -- --ignored --nocapture` (the nested compiler is part of this measurement, not the unit set).
#[test]
#[ignore = "explicit local landing measurement with nested Trust compilation"]
fn landing_rejudge_replay() {
    for scenario in [
        "wide",
        "dependency",
        "semantic-conflict",
        "narrow",
        "narrow-conflict",
        "narrow-wide",
    ] {
        replay(scenario);
    }
}

fn replay(scenario: &str) {
    let mut world = world();
    world.work = std::fs::canonicalize(&world.work).expect("physical workspace");
    write(&world.work, ".gitignore", "/target.noindex/\n");
    write(
        &world.work,
        "Cargo.toml",
        "[workspace]\nresolver = '3'\nmembers = ['crates/*']\n",
    );
    for name in ["a", "consumer", "dep", "wide"]
        .into_iter()
        .map(str::to_owned)
        .chain((0..12).map(|i| format!("fan{i}")))
    {
        let dependency = match name.as_str() {
            "a" => "dep",
            "consumer" => "a",
            "dep" | "wide" => "",
            _ => "wide",
        };
        let mut manifest =
            format!("[package]\nname = '{name}'\nversion = '0.0.0'\nedition = '2024'\n");
        if !dependency.is_empty() {
            manifest.push_str(&format!(
                "[dependencies]\n{dependency} = {{ path = '../{dependency}' }}\n"
            ));
        }
        write(&world.work, &format!("crates/{name}/Cargo.toml"), &manifest);
        let source = if name == "a" {
            SOURCE.to_owned()
        } else if dependency.is_empty() {
            "pub fn value() -> u8 { 1 }\n".to_owned()
        } else {
            format!("pub fn value() -> u8 {{ {dependency}::value() }}\n")
        };
        write(&world.work, &format!("crates/{name}/src/lib.rs"), &source);
    }
    let env = crate::EnvSnapshot::capture();
    let tools = crate::preflight(&env, &world.work).expect("Trust toolchain");
    let path_env = tools.path_with_stage2_first(&env.path);
    let run_tests = |root: &Path, packages: &[String]| {
        let mut cmd = aterm_uds::spawnfd::command(&tools.targo);
        cmd.current_dir(root)
            .env("PATH", &path_env)
            .env("CARGO_TARGET_DIR", world.work.join("target.noindex"))
            .args(["--unverified", "test", "--offline", "--quiet", "-j", "1"]);
        for package in packages {
            cmd.args(["-p", package]);
        }
        aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
            .expect("fixture tests finish")
    };
    // Warm every package in the same stable path before moving main or judging either cone.
    assert!(run_tests(&world.work, &[]).status.success());
    git(&world.work, &["add", "-A"]);
    git(&world.work, &["commit", "-q", "-m", "fixture workspace"]);
    git(&world.work, &["push", "-q", "origin", "HEAD:main"]);
    let narrow = scenario.starts_with("narrow");
    let conflict = scenario.ends_with("conflict");
    // Whether a round answers for the merge: main's change reaches less than the branch's.
    let round = narrow && scenario != "narrow-wide";
    let branch_source = if conflict {
        SOURCE.replace("{ dep::value() }", "{ 1 }")
    } else {
        SOURCE.replace("{ dep::value() }", "{ dep::value() + 0 }")
    };
    commit(&world.work, "crates/a/src/lib.rs", &branch_source);
    if narrow {
        commit(
            &world.work,
            "crates/fan0/src/lib.rs",
            "pub fn value() -> u8 { wide::value() + 0 }\n",
        );
    }
    let mut rounds = Vec::new();
    let mut judged = Vec::new();
    let mut moved = String::new();
    let mut said = Vec::new();
    let mut previous = String::new();
    let code = land(
        &world.work,
        &own(),
        &mut Seams {
            git: &git_cmd,
            verifier: &mut |_| Ok(own_image()),
            judge: &mut |root, _, base, _| {
                if !judged.is_empty() && scenario == "wide" {
                    // Replay the old and new base choices on this exact merged checkout and
                    // stable target path. Warm the changed inputs before the ABBA takes.
                    assert!(run_tests(root, &[]).status.success());
                    for (arm, reference) in [
                        ("A", previous.as_str()),
                        ("B", moved.as_str()),
                        ("B", moved.as_str()),
                        ("A", previous.as_str()),
                    ] {
                        let began = Instant::now();
                        let load = aterm_sysprobe::load_average();
                        let selection = crate::changed::resolve(root, &tools, &path_env, reference);
                        let crate::changed::Selection::Narrowed { crates, .. } = selection else {
                            panic!("fixture cone widened: {selection:?}");
                        };
                        assert!(run_tests(root, &crates).status.success());
                        println!(
                            "landing-rejudge-abba arm={arm} crates={} wall_ms={} load_before={load:?} load_after={:?}",
                            crates.len(),
                            began.elapsed().as_millis(),
                            aterm_sysprobe::load_average()
                        );
                    }
                }
                let began = Instant::now();
                let load = aterm_sysprobe::load_average();
                let selection = crate::changed::resolve(root, &tools, &path_env, base);
                let crate::changed::Selection::Narrowed { crates, seeds, .. } = selection else {
                    panic!("fixture cone widened: {selection:?}");
                };
                let out = run_tests(root, &crates);
                let elapsed = began.elapsed();
                println!(
                    "landing-rejudge scenario={scenario} judgment={} crates={} seeds={seeds:?} wall_ms={} load_before={load:?} load_after={:?} base={base} success={}",
                    judged.len(),
                    crates.len(),
                    elapsed.as_millis(),
                    aterm_sysprobe::load_average(),
                    out.status.success()
                );
                judged.push((base.to_owned(), crates));
                if judged.len() == 1 {
                    previous = git(root, &["rev-parse", "HEAD"]);
                    assert!(
                        out.status.success(),
                        "{}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                    if round {
                        rival_moves(
                            &world,
                            "crates/fan1/src/lib.rs",
                            "pub fn value() -> u8 { wide::value() + 0 }\n",
                        );
                    } else {
                        rival_moves(
                            &world,
                            "crates/wide/src/lib.rs",
                            "pub fn value() -> u8 { 2 }\n",
                        );
                    }
                    if scenario != "wide" && scenario != "narrow" && scenario != "narrow-wide" {
                        // Main's own test changes with its dependency; the branch's distinct
                        // contract is the semantic conflict, invisible to a textual merge.
                        rival_moves(
                            &world,
                            "crates/dep/src/lib.rs",
                            "pub fn value() -> u8 { 2 }\n",
                        );
                    }
                    let main_source = SOURCE.replace("MAIN: u8 = 0", "MAIN: u8 = 1");
                    let main_source = if matches!(scenario, "wide" | "narrow" | "narrow-wide") {
                        main_source
                    } else {
                        main_source.replace("assert_eq!(value(), 1)", "assert_eq!(value(), 2)")
                    };
                    moved = rival_moves(&world, "crates/a/src/lib.rs", &main_source);
                }
                Ran::Ended(if out.status.success() {
                    ran(exit::PASS, vec![row("test", "ok")], elapsed.as_secs())
                } else {
                    ran(
                        exit::FAILED,
                        vec![red("test", &["-p a --lib -- contract"])],
                        elapsed.as_secs(),
                    )
                })
            },
            branch: &mut |_, _| crates(if narrow { &["a", "fan0"] } else { &["a"] }),
            lane: &mut |_, _| Lane::Pass,
            reach: &live_reach,
            recheck: &mut |root, _, base, delta| {
                assert!(
                    round,
                    "a main change that reaches the branch's whole cone is judged in full"
                );
                let began = Instant::now();
                // The round's own selection ([`crate::run`] under `--judged` and `--shared`).
                let selection = crate::changed::resolve_round(
                    root,
                    &tools,
                    &path_env,
                    base,
                    &delta.judged,
                    delta.shared.clone(),
                    delta.mechanical,
                );
                let crate::changed::Selection::Narrowed { crates, .. } = selection else {
                    panic!("fixture cone widened: {selection:?}");
                };
                let out = run_tests(root, &crates);
                println!(
                    "landing-rejudge scenario={scenario} round crates={} shared={:?} wall_ms={} base={base} success={}",
                    crates.len(),
                    delta.shared,
                    began.elapsed().as_millis(),
                    out.status.success()
                );
                rounds.push((base.to_owned(), crates));
                let mut rows = vec![
                    row("fmt", "ok"),
                    row("census", "ok"),
                    row("export-scan", "ok"),
                ];
                if out.status.success() {
                    rows.push(row("test", "ok"));
                    ran(exit::PASS, rows, began.elapsed().as_secs())
                } else {
                    rows.push(red("test", &["-p a --lib -- contract"]));
                    ran(exit::FAILED, rows, began.elapsed().as_secs())
                }
            },
            hand_over: &mut |_, _, _| panic!("no handoff was expected"),
            say: &mut |line| said.push(line.to_owned()),
            remote: &crate::remote::run,
            pause: &no_retry,
        },
    );
    if round {
        assert_eq!(judged.len(), 1, "the first judgment stands: {said:#?}");
        assert_eq!(judged[0].1, ["a", "consumer", "fan0"]);
        assert_eq!(rounds.len(), 1, "{said:#?}");
        assert_eq!(rounds[0].1, ["a", "consumer"]);
        assert_eq!(rounds[0].0, moved);
    } else {
        assert!(rounds.is_empty(), "{said:#?}");
        assert_eq!(judged.len(), 2, "{said:#?}");
        if narrow {
            assert_eq!(judged[1].1, ["a", "consumer", "fan0"]);
        } else {
            assert_eq!(judged[1].1, ["a", "consumer"]);
        }
        assert_eq!(judged[1].0, moved);
    }
    if conflict {
        assert_eq!(code, exit::FAILED, "{said:#?}");
        assert_eq!(origin_main(&world), moved, "semantic red pushes nothing");
    } else {
        assert_eq!(code, exit::PASS, "{said:#?}");
        let receipt = stated(&world, &origin_main(&world)).expect("receipt");
        assert_eq!(receipt.composed_onto, None);
        if round {
            assert_eq!(receipt.checked.len(), 1);
            assert_eq!(receipt.rechecked.len(), 1);
            assert_eq!(receipt.rechecked[0].base, moved);
        } else {
            assert_eq!(receipt.checked[1].base, moved);
        }
    }
}
