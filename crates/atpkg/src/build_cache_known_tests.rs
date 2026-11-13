// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

struct World {
    _scratch: aterm_tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    target: PathBuf,
    cache: PathBuf,
}

impl World {
    fn new() -> Self {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().canonicalize().unwrap().join("account");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let workspace = home.join("source");
        let target = workspace.join("target/.ubd/main.noindex");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&target)
            .unwrap();
        let cache = aterm_toolchain::build_cache::build_dir(
            &aterm_toolchain::build_cache::cache_root(&home),
            &workspace,
        );
        Self {
            _scratch: scratch,
            home,
            workspace,
            target,
            cache,
        }
    }

    fn built(&self) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.cache)
            .unwrap();
        std::fs::set_permissions(&self.cache, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            self.cache.join("CACHEDIR.TAG"),
            super::super::adoption::CARGO_TAG,
        )
        .unwrap();
        std::fs::set_permissions(
            self.cache.join("CACHEDIR.TAG"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        // Presence is a Cargo layout observation, not a compiler-freshness attestation.
        std::fs::write(self.cache.join(".rustc_info.json"), b"{}").unwrap();
        std::fs::write(self.cache.join("keep"), b"cached units").unwrap();
    }

    fn observe(&self) -> std::io::Result<Option<KnownCargoCache>> {
        KnownCargoCache::observe(&self.home, &self.workspace, &self.cache, &self.target)
    }

    fn no_receipts(&self) {
        assert!(!self.home.join(super::super::INDEX).exists());
        assert!(!self.cache.join(super::super::MARKER).exists());
    }
}

#[test]
fn known_workspace_cache_observations_distinguish_built_and_fresh_without_mutation() {
    for shape in ["missing", "empty", "tag-only", "built"] {
        let world = World::new();
        if shape != "missing" {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&world.cache)
                .unwrap();
        }
        if matches!(shape, "tag-only" | "built") {
            std::fs::write(
                world.cache.join("CACHEDIR.TAG"),
                super::super::adoption::CARGO_TAG,
            )
            .unwrap();
            std::fs::set_permissions(
                world.cache.join("CACHEDIR.TAG"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        if shape == "built" {
            world.built();
        }
        let before = std::fs::symlink_metadata(&world.cache)
            .ok()
            .map(|metadata| (metadata.permissions().mode(), metadata.len()));
        let known = world.observe().unwrap().unwrap();
        assert_eq!(known.path().unwrap(), world.cache, "{shape}");
        let after = std::fs::symlink_metadata(&world.cache)
            .ok()
            .map(|metadata| (metadata.permissions().mode(), metadata.len()));
        assert_eq!(before, after, "{shape}");
        if shape == "built" {
            assert_eq!(
                std::fs::read(world.cache.join("keep")).unwrap(),
                b"cached units"
            );
        }
        world.no_receipts();
    }
}

#[test]
fn false_other_workspace_linked_and_writable_caches_never_gain_measurement_authority() {
    for shape in [
        "false",
        "source",
        "other-workspace",
        "root-link",
        "parent-link",
        "writable",
        "info-link",
    ] {
        let world = World::new();
        world.built();
        let outside = world.home.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"personal data").unwrap();
        match shape {
            "false" => std::fs::remove_file(world.cache.join("CACHEDIR.TAG")).unwrap(),
            "source" => std::fs::write(world.cache.join("Cargo.toml"), b"[workspace]").unwrap(),
            "root-link" | "parent-link" => {
                let entry = if shape == "root-link" {
                    &world.cache
                } else {
                    world.cache.parent().unwrap()
                };
                std::fs::rename(entry, world.home.join("original-cache")).unwrap();
                std::os::unix::fs::symlink(&outside, entry).unwrap();
            }
            "writable" => {
                std::fs::set_permissions(&world.cache, std::fs::Permissions::from_mode(0o777))
                    .unwrap()
            }
            "info-link" => {
                std::fs::remove_file(world.cache.join(".rustc_info.json")).unwrap();
                std::os::unix::fs::symlink(
                    outside.join("keep"),
                    world.cache.join(".rustc_info.json"),
                )
                .unwrap();
            }
            _ => {}
        }
        let workspace = if shape == "other-workspace" {
            world.home.join("another-source")
        } else {
            world.workspace.clone()
        };
        assert!(
            KnownCargoCache::observe(&world.home, &workspace, &world.cache, &world.target).is_err(),
            "{shape}"
        );
        assert_eq!(
            std::fs::read(outside.join("keep")).unwrap(),
            b"personal data"
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
        world.no_receipts();
    }
}

#[test]
fn a_known_cache_guard_refuses_replaced_parent_bindings() {
    let world = World::new();
    world.built();
    let known = world.observe().unwrap().unwrap();
    let parent = world.cache.parent().unwrap();
    std::fs::rename(parent, world.home.join("moved-cache-parent")).unwrap();
    std::fs::create_dir(parent).unwrap();
    assert!(known.path().is_err());
    assert!(!parent.join(aterm_toolchain::build_cache::LEAF).exists());
    world.no_receipts();
}

#[test]
fn a_fresh_cache_matures_only_below_its_original_anchor_and_never_rebinds_existing_units() {
    for replace_anchor in [false, true] {
        let world = World::new();
        let unbuilt = world.observe().unwrap().unwrap();
        if replace_anchor {
            std::fs::rename(&world.home, world.home.with_file_name("moved-home")).unwrap();
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&world.home)
                .unwrap();
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&world.target)
                .unwrap();
        }
        world.built();
        let built = world.observe().unwrap().unwrap();
        assert_eq!(built.continue_from(&unbuilt).is_ok(), !replace_anchor);
        std::fs::rename(&world.cache, world.home.join("moved-cache")).unwrap();
        world.built();
        let replacement = world.observe().unwrap().unwrap();
        assert!(replacement.continue_from(&built).is_err());
        world.no_receipts();
    }
}

#[test]
fn a_known_cache_on_another_volume_is_refused_without_inspecting_an_opaque_caller_path() {
    let world = World::new();
    world.built();
    let real = super::target_volume(&world.target).unwrap();
    let same = |_: &Path| Some(real.clone());
    assert!(
        KnownCargoCache::observe_unix(
            &world.home,
            &world.workspace,
            &world.cache,
            &world.target,
            &same
        )
        .unwrap()
        .is_some()
    );
    let mut another_volume = real.volume.unwrap_or([0; 16]);
    another_volume[0] ^= 1;
    let other = |_: &Path| {
        Some(super::super::Identity {
            device: real.device.wrapping_add(1),
            volume: Some(another_volume),
            ..real.clone()
        })
    };
    assert!(
        KnownCargoCache::observe_unix(
            &world.home,
            &world.workspace,
            &world.cache,
            &world.target,
            &other
        )
        .is_err()
    );
    assert!(
        KnownCargoCache::observe(
            &world.home,
            &world.workspace,
            &world.home.join("personal"),
            &world.target
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        std::fs::read(world.cache.join("keep")).unwrap(),
        b"cached units"
    );
    world.no_receipts();
}
