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
