// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A local landing replay with real dependency resolution and compiled test binaries.

use super::*;

const SOURCE: &str = "pub fn value() -> u8 { dep::value() }\n\n\n\n\n\n\n\npub const MAIN: u8 = 0;\n\n#[test]\nfn contract() { assert_eq!(value(), 1); }\n";

/// Main changes unrelated crates and the branch's file. Dependency moves must still compile
/// and test the branch against the new dependency. NEGATIVE CONTROL: the branch freezes the
/// dependency's old value, violating the merged test, and the re-judge refuses the landing.
/// Run explicitly: `targo --unverified test -p aterm-verify --lib landing_rejudge_replay
/// -- --ignored --nocapture` (the nested compiler is part of this measurement, not the unit set).
#[test]
#[ignore = "explicit local landing measurement with nested Trust compilation"]
fn landing_rejudge_replay() {
    for scenario in ["wide", "dependency", "semantic-conflict"] {
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
    let branch_source = if scenario == "semantic-conflict" {
        SOURCE.replace("{ dep::value() }", "{ 1 }")
    } else {
        SOURCE.replace("{ dep::value() }", "{ dep::value() + 0 }")
    };
    commit(&world.work, "crates/a/src/lib.rs", &branch_source);
    let mut judged = Vec::new();
    let mut moved = String::new();
    let mut said = Vec::new();
    let mut previous = String::new();
    let code = land(
        &world.work,
        &mut Seams {
            git: &git_cmd,
            judge: &mut |root, base, _| {
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
                    rival_moves(
                        &world,
                        "crates/wide/src/lib.rs",
                        "pub fn value() -> u8 { 2 }\n",
                    );
                    if scenario != "wide" {
                        // Main's own test changes with its dependency; the branch's distinct
                        // contract is the semantic conflict, invisible to a textual merge.
                        rival_moves(
                            &world,
                            "crates/dep/src/lib.rs",
                            "pub fn value() -> u8 { 2 }\n",
                        );
                    }
                    let main_source = SOURCE.replace("MAIN: u8 = 0", "MAIN: u8 = 1");
                    let main_source = if scenario == "wide" {
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
            branch: &mut |_, _| crates(&["a"]),
            lane: &mut |_, _| Lane::Pass,
            recheck: &mut |_, _| panic!("a source edit is never rechecked"),
            say: &mut |line| said.push(line.to_owned()),
        },
    );
    assert_eq!(judged.len(), 2, "{said:#?}");
    assert_eq!(judged[1].1, ["a", "consumer"]);
    assert_eq!(judged[1].0, moved);
    if scenario == "semantic-conflict" {
        assert_eq!(code, exit::FAILED, "{said:#?}");
        assert_eq!(origin_main(&world), moved, "semantic red pushes nothing");
    } else {
        assert_eq!(code, exit::PASS, "{said:#?}");
        let receipt = stated(&world, &origin_main(&world)).expect("receipt");
        assert_eq!(receipt.checked[1].base, moved);
        assert_eq!(receipt.composed_onto, None);
    }
}
