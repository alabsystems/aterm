// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn launch(world: &World, headless: bool) -> Launch {
    Launch {
        aterm: world.dir.join("must-not-start"),
        headless,
        args: Vec::new(),
        env: world.env(),
        cwd: world.home.clone(),
        sandbox: None,
        reroute: false,
    }
}

#[test]
fn headless_work_takes_no_window_lane_or_capacity_claim() {
    let world = World::new(CONFIG_OFF).unwrap();
    assert!(
        windowed_admission(&launch(&world, true), WindowWork::Timing)
            .unwrap()
            .is_none()
    );
    assert!(!world.dir.join("aterm.log").exists());
}

#[test]
fn functional_window_rejects_headless_and_zero_before_starting() {
    let world = World::new(CONFIG_OFF).unwrap();
    for (headless, bytes) in [(true, 1024), (false, 0)] {
        for start in [
            Instance::start_functional,
            Instance::start_ordinary_functional,
        ] {
            let why = start(&world, launch(&world, headless), bytes)
                .err()
                .expect("invalid functional request refused before spawn");
            assert!(why.contains("started nothing"), "{why}");
        }
        assert!(!world.dir.join("aterm.log").exists());
    }
}

#[test]
fn cli_window_selection_defaults_to_timing_and_preserves_checked_budget() {
    assert!(matches!(
        instance_window_work(&mut vec![], true).unwrap(),
        WindowWork::Timing
    ));
    assert!(matches!(
        instance_window_work(&mut vec![], false).unwrap(),
        WindowWork::Timing
    ));
    let mut args = vec!["--functional-memory-mib".into(), "2048".into()];
    assert!(matches!(
        instance_window_work(&mut args, true).unwrap(),
        WindowWork::Functional {
            memory_bytes: 2_147_483_648,
            retain: false
        }
    ));
    assert!(args.is_empty());
    let max_mib = u64::MAX / (1024 * 1024);
    let mut args = vec!["--functional-memory-mib".into(), max_mib.to_string()];
    assert!(matches!(instance_window_work(&mut args, true).unwrap(),
        WindowWork::Functional { memory_bytes, retain: false } if memory_bytes == max_mib * 1024 * 1024));
}

#[test]
fn cli_rejects_invalid_functional_modes_before_resolving_or_spawning_app() {
    for flags in [
        vec!["--functional-memory-mib"],
        vec!["--functional-memory-mib", "0"],
        vec!["--functional-memory-mib", "-1"],
        vec!["--functional-memory-mib", "+1"],
        vec!["--functional-memory-mib", "1.5"],
        vec!["--functional-memory-mib", "abc"],
        vec!["--functional-memory-mib", "18446744073709551616"],
        vec!["--functional-memory-mib", "17592186044416"],
        vec![
            "--functional-memory-mib",
            "1",
            "--functional-memory-mib",
            "2",
        ],
        vec!["--functional-memory-mib", "1", "--headless"],
    ] {
        let mut args: Vec<String> = flags.iter().map(|s| (*s).into()).collect();
        args.extend([
            "--window".into(),
            "--aterm".into(),
            "/must-not-resolve/aterm".into(),
        ]);
        assert_eq!(cmd_instance(&args).unwrap_err().code, 2, "{flags:?}");
    }
    let args = [
        "--functional-memory-mib",
        "1",
        "--aterm",
        "/must-not-resolve/aterm",
    ]
    .map(String::from);
    assert_eq!(cmd_instance(&args).unwrap_err().code, 2);
}
