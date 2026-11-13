// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unclaimed supervision distinguishes no child from a child whose observation failed.

use super::*;
use aterm_uds::{deadline, spawnfd};
use spawnfd::Tether as _;

const WORLD: &str = "ATPKG_UNCLAIMED_OBSERVATION_WORLD";
const ACTOR: &str =
    "build_admit::tests::unclaimed_observation::unclaimed_observation_actor_if_requested";
const ROOT: &str =
    "build_admit::tests::unclaimed_observation::unclaimed_observation_root_if_requested";

#[test]
fn unclaimed_observation_root_if_requested() {
    let Some(world) = std::env::var_os(WORLD) else {
        return;
    };
    std::fs::write(
        PathBuf::from(world).join("probe"),
        "executed retained image\n",
    )
    .unwrap();
}

#[test]
fn unclaimed_observation_actor_if_requested() {
    let Some(world) = std::env::var_os(WORLD) else {
        return;
    };
    let world = PathBuf::from(world);
    aterm_uds::tether::image_cache_for_tests(world.join("helper-images")).unwrap();
    aterm_uds::tether::through(aterm_tempfile::bin_exe("atpkg-status-fixture"));
    assert!(aterm_uds::tether::prepared_executable().unwrap().is_some());

    for case in ["healthy", "prebirth", "postbirth"] {
        let root = world.join(case);
        std::fs::create_dir(&root).unwrap();
        let mut command = spawnfd::transparent(std::env::current_exe().unwrap()).into_command();
        command
            .args(["--exact", ROOT, "--test-threads=1"])
            .env(WORLD, &root)
            .tether();
        if case == "prebirth" {
            command.arg("invalid\0argument");
        }
        let mut observed = false;
        let mut supervise = |command: &mut spawnfd::Command| {
            supervise_with(
                None,
                command,
                &mut |_| {},
                front::Front::arm,
                |child, meter| {
                    observed = true;
                    assert!(
                        meter.is_none(),
                        "an unclaimed child has no reservation meter"
                    );
                    assert!(
                        !child.is_reaped(),
                        "supervision receives the original child"
                    );
                },
            )
        };
        let outcome = if case == "postbirth" {
            spawnfd::with_exec_status_observation_fault_for_tests(
                &root.join("probe"),
                &mut command,
                supervise,
            )
        } else {
            supervise(&mut command)
        };
        let proved = std::fs::read_to_string(root.join("probe"))
            .is_ok_and(|s| s == "executed retained image\n");
        assert_eq!(
            observed,
            case != "prebirth",
            "{case}: actual child ownership"
        );
        assert_eq!(proved, case != "prebirth", "{case}: execution proof");
        match case {
            "healthy" => assert!(outcome.unwrap().success()),
            "prebirth" => assert_eq!(
                outcome.unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput
            ),
            "postbirth" => assert_eq!(
                outcome.unwrap_err().to_string(),
                "fixture: exec-status observation lost after execution proof"
            ),
            _ => unreachable!(),
        }
    }
}

#[test]
fn unclaimed_supervision_keeps_postbirth_ownership_and_prebirth_classification() {
    // Retain the helper image directory even if its first exec never proves it ran.
    let world = aterm_tempfile::tempdir().unwrap().keep();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", ACTOR, "--test-threads=1", "--nocapture"])
        .env(WORLD, &world)
        .setsid(true);
    let output = deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        output.status.success(),
        "isolated front failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
