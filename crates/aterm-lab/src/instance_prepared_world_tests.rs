// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CONFIG_OFF, World, world_parent};

#[test]
fn prepared_world_reopens_without_overwriting_configuration_or_identity() {
    let parent = world_parent();
    let root = parent.path().join("world");
    let world = World::create_at(&root, CONFIG_OFF).unwrap();
    let config = world.cfg.join("aterm/aterm.toml");
    let original = std::fs::read(&config).unwrap();
    let identity = world.home.join("fixture-identity");
    std::fs::write(&identity, b"retained identity").unwrap();
    assert!(World::create_at(&root, "different configuration").is_err());
    drop(world);
    let reopened = World::open_at(&root).unwrap();
    assert_eq!(reopened.cfg.join("aterm/aterm.toml"), config);
    assert_eq!(std::fs::read(&config).unwrap(), original);
    assert_eq!(std::fs::read(&identity).unwrap(), b"retained identity");
    assert!(World::open_at(parent.path()).is_err());
    let long = parent.path().join("x".repeat(100));
    assert!(World::create_at(&long, CONFIG_OFF).is_err());
    assert!(!long.exists());
    drop(reopened);
    assert!(identity.exists());
}

/// A WORLD LAID OUT ALREADY REFUSES A CONFIGURATION IT WOULD DROP: `instance --world DIR` on
/// a world an earlier run laid out keeps that world's `aterm.toml`, so a `--toml`,
/// `--toml-after`, `--harness` or `--harness-key` given with it is refused, never dropped
/// without a word, and the world reopens as it is with none. NEGATIVE CONTROL: reopen the
/// world whatever is configured, and the configured run is not refused.
#[test]
fn a_laid_out_world_refuses_a_configuration_it_would_drop() {
    let parent = world_parent();
    let root = parent.path().join("world");
    let config = World::create_at(&root, CONFIG_OFF)
        .unwrap()
        .cfg
        .join("aterm/aterm.toml");
    let original = std::fs::read(&config).unwrap();
    let refused = super::instance_world(Some(&root), "x = 1", true).err();
    assert_eq!(
        refused.map(|fail| fail.code),
        Some(2),
        "a configuration the world would drop is a usage error"
    );
    assert_eq!(std::fs::read(&config).unwrap(), original);
    let reopened = super::instance_world(Some(&root), CONFIG_OFF, false).expect("reopened");
    assert_eq!(reopened.cfg.join("aterm/aterm.toml"), config);
}

#[cfg(unix)]
#[test]
fn world_config_is_never_group_or_world_writable() {
    use std::os::unix::fs::PermissionsExt as _;
    // A loose pre-existing file is the umask-002 case (664) made host-independent:
    // the writer must pin it to a mode its reader accepts, not keep the old one.
    let parent = world_parent();
    let loose = parent.path().join("aterm.toml");
    std::fs::write(&loose, "old").unwrap();
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o664)).unwrap();
    super::write_aterm_toml(&loose, "new").unwrap();
    assert_eq!(std::fs::read_to_string(&loose).unwrap(), "new");
    let mode = std::fs::metadata(&loose).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o022,
        0,
        "mode {mode:o} would be refused by aterm_config_refusal"
    );

    let world = World::create_at(&parent.path().join("world"), CONFIG_OFF).unwrap();
    let mode = std::fs::metadata(world.cfg.join("aterm/aterm.toml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o022, 0, "world config mode {mode:o} is writable");
}
