// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Interrupted creation, identity replacement and producer-admission controls.

use super::super::{INDEX, create_checked, key, ordinary_clearance, roots};
use super::*;
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::time::Duration;

const HANG: Duration = Duration::from_secs(60);

struct World {
    _dir: aterm_tempfile::TempDir,
    home: PathBuf,
    owner: PathBuf,
    target: PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap().join("home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let owner = home.join("source");
        std::fs::create_dir(&owner).unwrap();
        std::fs::write(owner.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        Self {
            target: home.join("target.noindex"),
            home,
            owner,
            _dir: dir,
        }
    }
    fn record(&self) -> PathBuf {
        self.home
            .join(INDEX)
            .join(format!("{}.json", key(&self.target).unwrap()))
    }
    fn admit(&self) -> std::io::Result<bool> {
        create_checked(&self.home, &self.target, &self.owner)
    }
}

struct Fault;
impl Drop for Fault {
    fn drop(&mut self) {
        faults::clear();
    }
}
fn stop_at(point: Point) -> Fault {
    faults::set(move |seen| if seen == point { None } else { Some(()) });
    Fault
}

#[test]
fn every_creation_checkpoint_preserves_ownership_or_a_recoverable_exact_intent() {
    for point in [
        Point::Created,
        Point::Tag,
        Point::Marker,
        Point::Intent,
        Point::Publishing,
        Point::Renamed,
        Point::Committed,
    ] {
        let world = World::new();
        let fault = stop_at(point);
        assert!(
            world.admit().is_err(),
            "{point:?}: no build admitted on failure"
        );
        drop(fault);
        let before = read(&world.record());
        if matches!(point, Point::Created | Point::Tag | Point::Marker) {
            assert!(before.is_none() && !world.target.exists());
        } else if point != Point::Committed {
            let pending = before.as_ref().unwrap();
            assert_eq!(pending.schema, 5);
            let stage = &pending.creating.as_ref().unwrap().directory;
            assert!(ordinary_clearance(&world.home, stage).is_err());
            assert!(super::super::plan(&world.home, std::time::SystemTime::now()).is_empty());
        }
        assert!(
            world.admit().unwrap(),
            "{point:?}: a later invocation recovers"
        );
        let after = read(&world.record()).unwrap();
        assert_eq!(after.schema, 1);
        assert!(after.creating.is_none() && super::super::same(&world.home, &after));
        assert!(valid_cache_tag(&pin(&world.target).unwrap()).unwrap());
        if let Some(before) = before {
            assert_eq!(
                after.identity, before.identity,
                "recovery keeps the created inode"
            );
            assert_eq!(after.nonce, before.nonce);
        }
        assert_eq!(roots(&world.home).len(), 1);
        std::fs::write(world.target.join("kept"), b"reusable").unwrap();
        assert!(world.admit().unwrap());
        assert_eq!(
            std::fs::read(world.target.join("kept")).unwrap(),
            b"reusable"
        );
    }
}

#[test]
fn a_failed_index_or_gate_sync_never_publishes_a_target() {
    for point in [Point::Index, Point::Gate] {
        let world = World::new();
        let fault = stop_at(point);
        assert!(!world.admit().unwrap_or(false), "{point:?}");
        drop(fault);
        assert!(!world.target.exists());
        assert!(read(&world.record()).is_none());
        assert!(
            world.admit().unwrap(),
            "{point:?}: a later call reestablishes durable custody"
        );
        assert!(super::super::same(
            &world.home,
            &read(&world.record()).unwrap()
        ));
    }
}

#[test]
fn an_existing_parent_chain_is_flushed_again_after_an_interrupted_creation() {
    let mut world = World::new();
    world.target = world.home.join("first/second/target.noindex");
    let mut calls = 0;
    faults::set(move |point| {
        if point == Point::Parent {
            calls += 1;
            if calls == 3 {
                return None;
            }
        }
        Some(())
    });
    let fault = Fault;
    assert!(world.admit().is_err());
    drop(fault);
    assert!(world.target.parent().unwrap().is_dir());
    assert!(!world.target.exists() && read(&world.record()).is_none());

    // The deepest parent now exists. Retrying must still reestablish every ancestor's
    // durability instead of treating existence as proof that its earlier mkdir was flushed.
    let fault = stop_at(Point::Parent);
    assert!(world.admit().is_err());
    drop(fault);
    assert!(!world.target.exists() && read(&world.record()).is_none());
    assert!(world.admit().unwrap());
    assert!(super::super::same(
        &world.home,
        &read(&world.record()).unwrap()
    ));
}

#[test]
fn an_interrupted_creation_never_repairs_or_publishes_an_uncertain_cache_tag() {
    use std::os::unix::fs::PermissionsExt as _;
    for change in [
        "missing",
        "bytes",
        "trailing",
        "link",
        "directory",
        "hardlink",
        "writable",
    ] {
        let world = World::new();
        let fault = stop_at(Point::Intent);
        assert!(world.admit().is_err());
        drop(fault);
        let intent = std::fs::read(world.record()).unwrap();
        let stage = read(&world.record()).unwrap().creating.unwrap().directory;
        let tag = stage.join("CACHEDIR.TAG");
        match change {
            "missing" => std::fs::remove_file(&tag).unwrap(),
            "bytes" => std::fs::write(&tag, b"not a cache tag\n").unwrap(),
            "trailing" => std::fs::write(&tag, format!("{}extra", cache_tag_bytes())).unwrap(),
            "link" => {
                let original = world.home.join("original-tag");
                std::fs::rename(&tag, &original).unwrap();
                std::os::unix::fs::symlink(&original, &tag).unwrap();
            }
            "directory" => {
                std::fs::remove_file(&tag).unwrap();
                std::fs::create_dir(&tag).unwrap();
            }
            "hardlink" => std::fs::hard_link(&tag, world.home.join("shared-tag")).unwrap(),
            "writable" => {
                std::fs::set_permissions(&tag, std::fs::Permissions::from_mode(0o620)).unwrap()
            }
            _ => unreachable!(),
        }
        let before = std::fs::symlink_metadata(&tag).ok();
        let bytes = std::fs::read(&tag).ok();
        assert!(world.admit().is_err(), "{change}");
        assert!(!world.target.exists(), "{change}");
        assert_eq!(std::fs::read(world.record()).unwrap(), intent, "{change}");
        assert_eq!(
            std::fs::read(&tag).ok(),
            bytes,
            "{change}: never repair a tag"
        );
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(
            std::fs::symlink_metadata(&tag)
                .ok()
                .map(|meta| (meta.dev(), meta.ino())),
            before.map(|meta| (meta.dev(), meta.ino())),
            "{change}: never replace uncertain metadata"
        );
    }
}

#[test]
fn missing_or_replaced_creation_state_never_adopts_or_changes_foreign_payload() {
    for change in ["marker", "payload", "stage", "target"] {
        let world = World::new();
        let fault = stop_at(Point::Intent);
        assert!(world.admit().is_err());
        drop(fault);
        let pending = read(&world.record()).unwrap();
        let stage = pending.creating.as_ref().unwrap().directory.clone();
        let foreign = match change {
            "marker" => {
                std::fs::remove_file(stage.join(MARKER)).unwrap();
                stage.clone()
            }
            "payload" => stage.clone(),
            "stage" => {
                std::fs::rename(&stage, stage.with_extension("held.noindex")).unwrap();
                std::fs::create_dir(&stage).unwrap();
                stage.clone()
            }
            "target" => {
                std::fs::create_dir(&world.target).unwrap();
                world.target.clone()
            }
            _ => unreachable!(),
        };
        if change != "marker" {
            std::fs::write(foreign.join("foreign"), b"must stay").unwrap();
        }
        let intent = std::fs::read(world.record()).unwrap();
        assert!(world.admit().is_err(), "{change}");
        if change == "marker" {
            assert!(
                !stage.join(MARKER).exists(),
                "a missing marker was not repaired"
            );
        } else {
            assert_eq!(
                std::fs::read(foreign.join("foreign")).unwrap(),
                b"must stay"
            );
        }
        assert_eq!(std::fs::read(world.record()).unwrap(), intent);
        assert!(read(&world.record()).unwrap().creating.is_some());
    }
}

#[test]
fn a_target_that_appears_before_publish_is_never_overwritten() {
    let world = World::new();
    let target = world.target.clone();
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = std::sync::Arc::clone(&captured);
    faults::set(move |point| {
        if point == Point::Publishing {
            std::fs::create_dir(&target).unwrap();
            *seen.lock().unwrap() = identity(&target);
        }
        Some(())
    });
    let guard = Fault;
    assert!(world.admit().is_err());
    drop(guard);
    assert!(
        captured.lock().unwrap().is_some(),
        "the collision happened at the rename"
    );
    assert_eq!(
        identity(&world.target),
        *captured.lock().unwrap(),
        "even an empty foreign directory stays its own inode"
    );
    assert!(!world.target.join(MARKER).exists());
    assert!(
        read(&world.record())
            .unwrap()
            .creating
            .as_ref()
            .unwrap()
            .directory
            .is_dir()
    );
}

#[test]
fn an_explicit_managed_producer_refuses_unknown_output_but_queries_and_defaults_stay_queries() {
    let world = World::new();
    let machine = super::super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &|_| Ok(None),
        writable: &|_| Some(true),
    };
    let words = |command: &str| {
        vec![
            "--unverified".into(),
            command.into(),
            format!(
                "--manifest-path={}",
                world.owner.join("Cargo.toml").display()
            ),
            format!("--target-dir={}", world.target.display()),
        ]
    };
    let tool = super::super::super::Tool::Targo;
    std::fs::create_dir(&world.target).unwrap();
    std::fs::write(world.target.join("kept"), b"unmanaged").unwrap();
    std::fs::write(world.target.join("CACHEDIR.TAG"), cache_tag_bytes()).unwrap();
    assert!(
        super::super::observe_for_producer(&words("build"), tool, &world.home, &machine, true)
            .is_err()
    );
    assert!(!world.target.join(MARKER).exists());
    assert_eq!(
        std::fs::read(world.target.join("kept")).unwrap(),
        b"unmanaged"
    );
    assert!(
        !super::super::observe_for_producer(&words("help"), tool, &world.home, &machine, true)
            .unwrap()
    );
    let default = vec!["--unverified".into(), "check".into()];
    assert!(
        !super::super::observe_for_producer(&default, tool, &world.home, &machine, true).unwrap()
    );
    let placed = match super::super::super::decide(&default, tool, &machine).unwrap() {
        super::super::super::BuildDir::Cache(config) => {
            super::super::super::directory_from_config(&config)
                .unwrap()
                .to_string()
        }
        other => panic!("the default fixture uses its mirror cache: {other:?}"),
    };
    let env =
        |name: &str| (name == super::super::super::BUILD_DIR_ENV).then(|| placed.clone().into());
    let inherited = super::super::super::Machine {
        env: &env,
        ..machine
    };
    assert!(
        !super::super::observe_for_producer(&default, tool, &world.home, &inherited, true).unwrap(),
        "a derived default placement never becomes an explicit-target requirement"
    );
    let fresh = world.home.join("fresh.noindex");
    let mut args = words("build");
    *args.last_mut().unwrap() = format!("--target-dir={}", fresh.display());
    assert!(super::super::observe_for_producer(&args, tool, &world.home, &machine, true).unwrap());
    assert!(fresh.join(MARKER).is_file());
    let record = world
        .home
        .join(INDEX)
        .join(format!("{}.json", key(&fresh).unwrap()));
    let before = std::fs::read(&record).unwrap();
    let unmanaged = world.home.join("custom-output.noindex");
    args.extend([
        "--config".into(),
        format!("build.build-dir='{}'", unmanaged.display()),
    ]);
    assert!(
        super::super::observe_for_producer(&args, tool, &world.home, &machine, true).is_err(),
        "a protected known target cannot acknowledge unmanaged custom intermediates"
    );
    assert_eq!(
        std::fs::read(&record).unwrap(),
        before,
        "refusal does not taint ownership"
    );
    assert!(!unmanaged.exists());
    assert!(!read(&record).unwrap().owners_unknown);

    let nested_fresh = world.owner.join("target/mine.noindex");
    let mut nested_args = words("build");
    *nested_args.last_mut().unwrap() = format!("--target-dir={}", nested_fresh.display());
    assert!(
        super::super::observe_for_producer(&nested_args, tool, &world.home, &machine, true)
            .unwrap(),
        "a fresh explicit target under a missing workspace subdirectory establishes private custody"
    );
    assert!(nested_fresh.join(MARKER).is_file());

    let protected_home_target = world.home.join("Documents/unindexed.noindex");
    let mut protected_args = words("build");
    *protected_args.last_mut().unwrap() =
        format!("--target-dir={}", protected_home_target.display());
    assert!(
        super::super::observe_for_producer(&protected_args, tool, &world.home, &machine, true)
            .is_err(),
        "an unindexable explicit target inside HOME is refused"
    );

    let outside_cwd = world.home.parent().unwrap();
    let outside_machine = super::super::super::Machine {
        cwd: Some(outside_cwd),
        ..machine
    };
    assert!(
        !super::super::observe_for_producer(&default, tool, &world.home, &outside_machine, true)
            .unwrap(),
        "a plain build outside HOME is outside HOME cache custody"
    );

    let external_target = outside_cwd.join("scratch-target.noindex");
    let mut external_args = words("build");
    *external_args.last_mut().unwrap() = format!("--target-dir={}", external_target.display());
    assert!(
        !super::super::observe_for_producer(&external_args, tool, &world.home, &machine, true)
            .unwrap(),
        "an explicit target outside HOME leaves nothing unmanaged in HOME"
    );

    let shared_leaf =
        super::super::super::build_dir(&super::super::super::cache_root(&world.home), &world.owner)
            .join("debug/build/outer/out/nested-shim");
    let mut shared_args = words("build");
    *shared_args.last_mut().unwrap() = format!("--target-dir={}", shared_leaf.display());
    assert!(
        !super::super::observe_for_producer(&shared_args, tool, &world.home, &machine, true)
            .unwrap(),
        "a build.rs target inside the shared cache's .cargo-build leaf is already bounded there"
    );
}

const CHILD_HOME: &str = "ATPKG_TEST_CREATION_HOME";
const CHILD_POINT: &str = "ATPKG_TEST_CREATION_POINT";
const CHILD_REGISTRAR: &str = "ATPKG_TEST_CREATION_REGISTRAR";

#[test]
fn creation_crash_child_if_asked() {
    let Some(home) = std::env::var_os(CHILD_HOME) else {
        return;
    };
    let home = PathBuf::from(home);
    if let Ok(mode) = std::env::var(CHILD_REGISTRAR) {
        let mut args: Vec<std::ffi::OsString> = Vec::new();
        if mode == "required" {
            args.push("--require-owned-if-explicit".into());
        }
        args.extend([
            "targo".into(),
            "--unverified".into(),
            if mode == "query" {
                "help".into()
            } else {
                "check".into()
            },
            format!("--target-dir={}", home.join("target.noindex").display()).into(),
        ]);
        let code = super::super::run_hidden(&args);
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            75
        });
    }
    let want = std::env::var(CHILD_POINT).unwrap();
    faults::set(move |point| {
        if format!("{point:?}") == want {
            use std::io::Write as _;
            println!("creation-checkpoint-{point:?}");
            std::io::stdout().flush().unwrap();
            aterm_uds::process::kill_self_now();
        }
        Some(())
    });
    let _ = create_checked(&home, &home.join("target.noindex"), &home.join("source"));
    panic!("the requested creation checkpoint did not kill the child");
}

#[cfg(target_os = "macos")]
#[test]
fn legacy_and_required_shim_argv_cannot_acknowledge_unmanaged_explicit_output() {
    for mode in ["legacy", "required", "fresh", "query"] {
        let world = World::new();
        let unowned = matches!(mode, "legacy" | "required");
        if unowned {
            std::fs::create_dir(&world.target).unwrap();
            std::fs::write(world.target.join("kept"), b"unmanaged").unwrap();
        }
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "build_cache::registered::creation::tests::creation_crash_child_if_asked",
                "--nocapture",
            ])
            .current_dir(&world.owner)
            .env_clear()
            .env("HOME", &world.home)
            .env(CHILD_HOME, &world.home)
            .env(CHILD_REGISTRAR, mode);
        let result = aterm_uds::deadline::ends_within(&mut command, HANG);
        assert_eq!(
            result.status.code(),
            Some(if unowned { 75 } else { 0 }),
            "{mode}: {result:?}"
        );
        if unowned {
            assert_eq!(
                std::fs::read(world.target.join("kept")).unwrap(),
                b"unmanaged"
            );
            assert!(!world.target.join(MARKER).exists());
        } else if mode == "fresh" {
            assert!(super::super::same(
                &world.home,
                &read(&world.record()).unwrap()
            ));
        } else {
            assert!(!world.target.exists(), "a help query creates no target");
        }
    }
}

#[test]
fn a_killed_creator_never_leaves_a_public_unmanaged_cache() {
    for point in [
        Point::Index,
        Point::Gate,
        Point::Created,
        Point::Tag,
        Point::Marker,
        Point::Intent,
        Point::Publishing,
        Point::Renamed,
        Point::Committed,
    ] {
        let world = World::new();
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "build_cache::registered::creation::tests::creation_crash_child_if_asked",
                "--nocapture",
            ])
            .env_clear()
            .env(CHILD_HOME, &world.home)
            .env(CHILD_POINT, format!("{point:?}"));
        let result = aterm_uds::deadline::ends_within(&mut command, HANG);
        assert_eq!(result.status.signal(), Some(9), "{point:?}: {result:?}");
        assert!(
            String::from_utf8_lossy(&result.stdout)
                .contains(&format!("creation-checkpoint-{point:?}"))
        );
        let before = read(&world.record());
        if world.target.exists() {
            assert!(world.target.join(MARKER).is_file());
        }
        assert!(world.admit().unwrap(), "{point:?}");
        let after = read(&world.record()).unwrap();
        if let Some(before) = before {
            assert_eq!(before.identity, after.identity);
        }
        assert!(super::super::same(&world.home, &after));
    }
}
