// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

struct World {
    _scratch: aterm_tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    target: PathBuf,
    cache: PathBuf,
}

impl World {
    fn new() -> Self {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().canonicalize().unwrap();
        let root = home.join("source");
        let target = root.join(aterm_toolchain::build_cache::DEFAULT_TARGET);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&target)
            .unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[workspace]\n").unwrap();
        let cache = aterm_toolchain::build_cache::build_dir(
            &aterm_toolchain::build_cache::cache_root(&home),
            &root,
        );
        Self {
            _scratch: scratch,
            home,
            root,
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
            "Signature: 8a477f597d28d172789f06886806bc55\n\
             # This file is a cache directory tag created by cargo.\n\
             # For information about cache directory tags see https://bford.info/cachedir/\n",
        )
        .unwrap();
        std::fs::set_permissions(
            self.cache.join("CACHEDIR.TAG"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::write(self.cache.join(".rustc_info.json"), b"{}").unwrap();
        std::fs::write(self.cache.join("cached-unit"), [5; 16384]).unwrap();
    }

    fn ctx(&self, selected: &Path) -> Ctx {
        Ctx::new(
            self.root.clone(),
            crate::Scope::workspace(),
            crate::EnvSnapshot {
                home: self.home.clone(),
                cargo_target_dir: Some(self.target.clone()),
                cargo_layout: vec![
                    ("CARGO_TARGET_DIR", self.target.clone().into_os_string()),
                    ("CARGO_BUILD_BUILD_DIR", selected.as_os_str().to_owned()),
                ],
                ..crate::EnvSnapshot::default()
            },
            self.home.join("scratch"),
        )
        .with_checks(&[Check::Test])
    }

    fn no_custody(&self) {
        assert!(
            !self
                .home
                .join(atpkg::build_cache::registered::INDEX)
                .exists()
        );
        for root in [&self.target, &self.cache] {
            assert!(!root.join(atpkg::build_cache::registered::MARKER).exists());
        }
    }
}

#[test]
fn a_captured_workspace_cache_is_measured_and_kept_without_becoming_owned_output() {
    let world = World::new();
    world.built();
    let ctx = world.ctx(&world.cache);
    assert!(ctx.main_build_dir.is_none());
    let run = run_of(&ctx).unwrap().unwrap();
    assert_eq!(run.dirs, vec![world.cache.clone(), ctx.state_dir()]);
    assert_eq!(run.on, world.target);
    assert!(
        run.machine.is_some(),
        "this exact workspace cache has the same artifact scope"
    );
    let expected = aterm_capacity::allocated(&run.dirs);
    assert!(expected > 0);
    let tag = atpkg::admit_disk::dirs_tag(&run.dirs);
    let ledger = aterm_capacity::Ledger::at(world.home.join("ledger"));
    let mut reading = aterm_capacity::Reading::of_gib(48, 40, 14);
    reading.volume = Some(aterm_capacity::Volume {
        mount: world.home.clone(),
        total: 1000 * GIB,
        free: 900 * GIB,
        floor: 8 * GIB,
        room: 2 * GIB,
    });
    let held = admit_in(
        &ctx,
        &ledger,
        &aterm_capacity::Fixed::new(reading),
        std::process::id(),
        atpkg::admit_disk::Admit::Wait,
    )
    .unwrap();
    let Disk::Held(held) = held else {
        panic!("the real directory takes its own measured claim");
    };
    assert_eq!(held.estimate.holds, expected);
    let done = held.finish_whole();
    assert_eq!(done.run.dirs, Some(tag));
    assert!(done.here.is_ok());
    assert!(done.machine.is_some_and(|result| result.is_ok()));
    assert_eq!(
        aterm_capacity::Footprints::load(&ctx.state_dir().join(FOOTPRINT))
            .runs()
            .last()
            .unwrap()
            .dirs,
        Some(tag)
    );
    assert_eq!(
        std::fs::read(world.cache.join("cached-unit")).unwrap(),
        [5; 16384]
    );
    world.no_custody();
}

#[test]
fn fresh_known_namespaces_and_opaque_caller_folders_keep_distinct_accounting() {
    let world = World::new();
    let fresh = world.ctx(&world.cache);
    let run = run_of(&fresh).unwrap().unwrap();
    assert_eq!(run.dirs, vec![world.cache.clone(), fresh.state_dir()]);
    assert!(run.machine.is_some());
    assert!(
        !world.cache.exists(),
        "a measurement plan creates no directories"
    );
    let personal = world.home.join("personal");
    std::fs::create_dir(&personal).unwrap();
    std::fs::write(personal.join("keep"), b"personal data").unwrap();
    let opaque = world.ctx(&personal);
    let run = run_of(&opaque).unwrap().unwrap();
    assert_eq!(run.dirs, vec![opaque.state_dir()]);
    assert!(run.machine.is_none());
    assert_eq!(run.cold, COLD_BYTES);
    assert_eq!(
        std::fs::read(personal.join("keep")).unwrap(),
        b"personal data"
    );
    world.no_custody();
}

#[test]
fn unsafe_recognized_cache_choices_refuse_before_any_disk_admission() {
    for shape in [
        "false",
        "other-workspace",
        "root-link",
        "parent-link",
        "writable",
    ] {
        let world = World::new();
        world.built();
        let outside = world.home.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"outside data").unwrap();
        let mut selected = world.cache.clone();
        match shape {
            "false" => std::fs::remove_file(world.cache.join("CACHEDIR.TAG")).unwrap(),
            "other-workspace" => {
                selected = aterm_toolchain::build_cache::build_dir(
                    &aterm_toolchain::build_cache::cache_root(&world.home),
                    &world.home.join("another-source"),
                )
            }
            "root-link" | "parent-link" => {
                let entry = if shape == "root-link" {
                    &world.cache
                } else {
                    world.cache.parent().unwrap()
                };
                std::fs::rename(entry, world.home.join("moved-cache")).unwrap();
                std::os::unix::fs::symlink(&outside, entry).unwrap();
            }
            "writable" => {
                std::fs::set_permissions(&world.cache, std::fs::Permissions::from_mode(0o777))
                    .unwrap()
            }
            _ => unreachable!(),
        }
        let ctx = world.ctx(&selected);
        assert!(run_of(&ctx).is_err(), "{shape}");
        let ledger_path = world.home.join("ledger");
        assert!(
            admit_in(
                &ctx,
                &aterm_capacity::Ledger::at(ledger_path.clone()),
                &aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14)),
                std::process::id(),
                atpkg::admit_disk::Admit::Wait
            )
            .is_err(),
            "{shape}"
        );
        assert!(!ledger_path.exists(), "{shape}: no claim was attempted");
        assert_eq!(
            std::fs::read(outside.join("keep")).unwrap(),
            b"outside data"
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
        world.no_custody();
    }
}

#[test]
fn used_units_follow_the_captured_cache_and_reject_a_later_redirect() {
    let world = World::new();
    world.built();
    let ctx = world.ctx(&world.cache).with_checkers_in_tests(true);
    run_of(&ctx).unwrap().unwrap();
    note_used(
        &ctx,
        &[crate::shape::Artifact {
            filenames: vec![
                world
                    .cache
                    .join("debug/build/example/1111111111111111/out/example")
                    .display()
                    .to_string(),
            ],
            fresh: true,
            package: Some("example".to_owned()),
            test: true,
        }],
        &[],
    );
    assert!(keep_used(&ctx, true).is_some());
    let record = world.cache.join(atpkg::build_cache::stale::USED);
    let used = atpkg::build_cache::stale::Used::decode(&std::fs::read(&record).unwrap()).unwrap();
    assert_eq!(
        used.profiles["debug"],
        std::collections::BTreeSet::from(["1111111111111111".to_owned()])
    );
    assert!(!world.target.join(atpkg::build_cache::stale::USED).exists());
    let moved = world.home.join("moved-cache");
    std::fs::rename(&world.cache, &moved).unwrap();
    let outside = world.home.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"outside data").unwrap();
    std::os::unix::fs::symlink(&outside, &world.cache).unwrap();
    assert!(keep_used(&ctx, true).is_none());
    assert!(!outside.join(atpkg::build_cache::stale::USED).exists());
    assert_eq!(
        std::fs::read(outside.join("keep")).unwrap(),
        b"outside data"
    );
    world.no_custody();
}

#[test]
fn a_valid_cache_replacement_cannot_inherit_units_from_the_admitted_directory() {
    for before_note in [false, true] {
        let world = World::new();
        world.built();
        let ctx = world.ctx(&world.cache).with_checkers_in_tests(true);
        run_of(&ctx).unwrap().unwrap();
        let artifacts = [crate::shape::Artifact {
            filenames: vec![
                world
                    .cache
                    .join("debug/build/example/1111111111111111/out/example")
                    .display()
                    .to_string(),
            ],
            fresh: true,
            package: Some("example".to_owned()),
            test: true,
        }];
        if !before_note {
            note_used(&ctx, &artifacts, &[]);
        }
        let moved = world.home.join("original-cache");
        std::fs::rename(&world.cache, &moved).unwrap();
        world.built();
        if before_note {
            note_used(&ctx, &artifacts, &[]);
        }
        assert!(keep_used(&ctx, true).is_none(), "before_note={before_note}");
        assert!(!world.cache.join(atpkg::build_cache::stale::USED).exists());
        assert!(!moved.join(atpkg::build_cache::stale::USED).exists());
        assert_eq!(
            std::fs::read(moved.join("cached-unit")).unwrap(),
            [5; 16384]
        );
        world.no_custody();
    }
}
