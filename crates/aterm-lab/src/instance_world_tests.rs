// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn ordinary_roots_reject_a_foreign_state_root_or_a_later_foreign_override() {
    let world = World::new(CONFIG_OFF).expect("fresh private world");
    let env = world.env();
    assert!(ordinary_roots_match(&world, &env));
    let mut unmarked = env.clone();
    unmarked.retain(|(name, _)| name != "ATERM_STATE_HOME");
    assert!(ordinary_roots_match(&world, &unmarked));
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "ATERM_STATE_HOME",
    ] {
        let mut overridden = env.clone();
        overridden.push((
            name.into(),
            world.dir.join("elsewhere").display().to_string(),
        ));
        assert!(!ordinary_roots_match(&world, &overridden), "{name}");
        let why = Instance::start_ordinary_functional(
            &world,
            Launch {
                aterm: world.dir.join("must-not-start"),
                headless: false,
                args: Vec::new(),
                env: overridden,
                cwd: world.home.clone(),
                sandbox: None,
                reroute: false,
            },
            1024,
        )
        .err()
        .expect("foreign roots refused before admission or spawn");
        assert!(why.contains("private home, config and runtime"), "{why}");
        assert!(!world.dir.join("aterm.log").exists());
    }
}

#[test]
fn a_released_world_goes_when_it_drops() {
    let mut kept = World::new(CONFIG_OFF).expect("fresh private world");
    let dir = kept.dir.clone();
    assert_eq!(dir.extension().and_then(|s| s.to_str()), Some("noindex"));
    let observer = dir.join("cfg/aterm/network-state/observer.sock");
    std::fs::create_dir(observer.parent().unwrap()).expect("network state");
    let listener = std::os::unix::net::UnixListener::bind(&observer)
        .expect("the longest world socket path fits after canonicalization");
    drop(listener);
    kept.keep();
    drop(kept);
    assert!(dir.is_dir(), "kept");
    std::fs::remove_dir_all(&dir).unwrap();
    let mut released = World::new(CONFIG_OFF).expect("fresh private world");
    let dir = released.dir.clone();
    released.keep();
    released.release();
    drop(released);
    assert!(!dir.exists(), "released");
}
