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
        // A workspace as umask 022 makes it, whatever this process's: a target created under
        // it is laid through private folders alone.
        let owner = home.join("source");
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&owner)
            .unwrap();
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
            // Writable by no group whatever the umask: custody names only such a folder.
            std::fs::DirBuilder::new()
                .mode(0o755)
                .create(&target)
                .unwrap();
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
    let child = super::super::super::target_build_path(&world.target);
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(child.join("kept"), b"unmanaged intermediates").unwrap();
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
    std::fs::create_dir(&unmanaged).unwrap();
    std::fs::write(unmanaged.join("kept"), b"unmanaged intermediates").unwrap();
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
    assert!(!unmanaged.join(MARKER).exists());
    assert_eq!(
        std::fs::read(unmanaged.join("kept")).unwrap(),
        b"unmanaged intermediates"
    );
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

    // Each folder of HOME this platform never looks into (`~/Documents` among them on macOS),
    // but `~/Library`, whose caches hold the shared cache.
    for skipped in super::super::super::checkout::HOME_SKIP
        .iter()
        .filter(|name| **name != "Library")
    {
        let protected_home_target = world.home.join(skipped).join("unindexed.noindex");
        let mut protected_args = words("build");
        *protected_args.last_mut().unwrap() =
            format!("--target-dir={}", protected_home_target.display());
        assert!(
            super::super::observe_for_producer(&protected_args, tool, &world.home, &machine, true)
                .is_err(),
            "an unindexable explicit target inside HOME is refused: ~/{skipped}"
        );
    }

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

/// The decision behind the front's hand-on of `verify provision`'s fish build
/// (`registered::tests::the_front_hands_on_a_build_of_a_workspace_atpkg_never_manages`): a
/// package in a scratch under `~/Library/Caches`, a folder of HOME atpkg never looks into, is
/// never a workspace it manages, as one outside HOME is none, so a build run in it owes its
/// explicit target (named again in `CARGO_TARGET_DIR`) no managed custody, and none is refused or
/// claimed. A workspace atpkg manages owes it still: named through a link from there, read
/// through the link as atpkg reads its workspace, or with its target there, where atpkg cannot
/// keep one. A package in HOME itself is no managed workspace either.
#[test]
fn only_a_workspace_atpkg_manages_owes_custody_of_its_explicit_target() {
    let world = World::new();
    let scratch = world
        .home
        .join("Library/Caches/aterm/tooling.noindex/.provision-fish-1_0_0");
    let src = scratch.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("Cargo.toml"),
        "[package]\nname = \"fish\"\nversion = \"4.9.3\"\n",
    )
    .unwrap();
    let target = scratch.join("target.noindex");
    let named = target.clone().into_os_string();
    let env = |name: &str| (name == "CARGO_TARGET_DIR").then(|| named.clone());
    let machine = super::super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&src),
        env: &env,
        read: &|_| Ok(None),
        writable: &|_| Some(true),
    };
    let build = |manifest: Option<&Path>| {
        let mut args: Vec<String> = ["--unverified", "build", "--release"]
            .map(String::from)
            .into();
        if let Some(manifest) = manifest {
            args.extend(["--manifest-path".into(), manifest.display().to_string()]);
        }
        args.extend(["--target-dir".into(), target.display().to_string()]);
        args
    };
    let decide = |args: &[String]| {
        super::super::observe_for_producer(
            args,
            super::super::super::Tool::Targo,
            &world.home,
            &machine,
            true,
        )
    };
    let record = world
        .home
        .join(INDEX)
        .join(format!("{}.json", key(&target).unwrap()));

    assert!(
        !decide(&build(None)).unwrap(),
        "the package a build run in ~/Library finds is outside HOME cache custody"
    );
    assert!(
        !target.exists() && !record.exists(),
        "nothing is claimed for a build atpkg never manages"
    );

    std::os::unix::fs::symlink(&world.owner, scratch.join("linked")).unwrap();
    assert!(
        decide(&build(Some(&scratch.join("linked/Cargo.toml")))).is_err(),
        "a managed workspace named through a link in ~/Library owes its explicit target custody"
    );
    assert!(
        decide(&build(Some(&world.owner.join("Cargo.toml")))).is_err(),
        "a managed workspace's explicit target in ~/Library is one atpkg cannot keep"
    );
    assert!(!target.exists() && !record.exists());

    // HOME itself is no workspace atpkg manages either: a package there, built into a target
    // directly in HOME, owes that target no custody.
    let manifest = world.home.join("Cargo.toml");
    std::fs::write(
        &manifest,
        "[package]\nname = \"home\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let in_home = world.home.join("t");
    let args: Vec<String> = [
        "--unverified".to_owned(),
        "build".to_owned(),
        "--manifest-path".to_owned(),
        manifest.display().to_string(),
        "--target-dir".to_owned(),
        in_home.display().to_string(),
    ]
    .into();
    assert!(
        !decide(&args).unwrap(),
        "a package in HOME itself is outside HOME cache custody"
    );
    let record = world
        .home
        .join(INDEX)
        .join(format!("{}.json", key(&in_home).unwrap()));
    assert!(!in_home.exists() && !record.exists());
}

/// What cargo writes into `CACHEDIR.TAG` as it makes a build directory itself (it writes none
/// into a folder that stood before it was named), as cargo 1.47 onward and targo write it.
const CARGO_TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n\
                         # This file is a cache directory tag created by cargo.\n\
                         # For information about cache directory tags see https://bford.info/cachedir/\n";

/// `root` as a build left it before any registration: the folder cargo made, its tag, the
/// rustc facts it keeps, and one profile with its lock and an artifact, each with the mode
/// cargo gives it under umask 022, whatever this process's: adoption proves custody only of a
/// folder and a tag no group or other user can write.
fn cargo_laid(root: &Path) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let put = |at: &Path, bytes: &[u8]| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(at)
            .and_then(|mut file| file.write_all(bytes))
            .unwrap();
    };
    std::fs::DirBuilder::new().mode(0o755).create(root).unwrap();
    put(&root.join("CACHEDIR.TAG"), CARGO_TAG.as_bytes());
    put(&root.join(".rustc_info.json"), b"{}");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(root.join("debug/deps"))
        .unwrap();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(root.join("debug/.fingerprint/probe-0123456789abcdef"))
        .unwrap();
    put(
        &root.join("debug/.fingerprint/probe-0123456789abcdef/invoked.timestamp"),
        b"invoked",
    );
    put(&root.join("debug/.cargo-lock"), b"");
    put(&root.join("debug/deps/libprobe.rlib"), b"reusable output");
}

#[test]
fn compiler_probe_alone_never_grants_cache_custody() {
    let world = World::new();
    cargo_laid(&world.target);
    // `targo metadata` leaves precisely the tag and compiler probe in an existing root.
    std::fs::remove_dir_all(world.target.join("debug")).unwrap();
    let before = std::fs::read(world.target.join(".rustc_info.json")).unwrap();
    assert!(!create_checked(&world.home, &world.target, &world.owner).unwrap());
    assert!(!world.target.join(MARKER).exists());
    assert!(read(&world.record()).is_none());
    assert_eq!(
        std::fs::read(world.target.join(".rustc_info.json")).unwrap(),
        before
    );
}

#[test]
fn adoption_requires_unlinked_private_compilation_evidence_in_known_layouts() {
    use std::os::unix::fs::PermissionsExt as _;
    for layout in [
        "custom/.fingerprint/probe-hash",
        "some-target/custom/.fingerprint/probe-hash",
        "custom/build/probe/hash/fingerprint",
        "some-target/custom/build/probe/hash/fingerprint",
    ] {
        for case in [
            "built",
            "linked-file",
            "hardlinked-file",
            "linked-dir",
            "shared-dir",
            "shared-file",
            "directory-file",
            "missing",
        ] {
            let world = World::new();
            cargo_laid(&world.target);
            std::fs::remove_dir_all(world.target.join("debug/.fingerprint")).unwrap();
            let evidence = world.target.join(layout);
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(&evidence)
                .unwrap();
            let stamp = evidence.join("invoked.timestamp");
            std::fs::write(&stamp, b"This file has an mtime of when this was started.").unwrap();
            std::fs::set_permissions(&stamp, std::fs::Permissions::from_mode(0o600)).unwrap();
            match case {
                "built" => {}
                "linked-file" => {
                    let outside = world.home.join("stamp");
                    std::fs::rename(&stamp, &outside).unwrap();
                    std::os::unix::fs::symlink(&outside, &stamp).unwrap();
                }
                "hardlinked-file" => std::fs::hard_link(&stamp, world.home.join("stamp")).unwrap(),
                "linked-dir" => {
                    let outside = world.home.join("fingerprint");
                    std::fs::rename(&evidence, &outside).unwrap();
                    std::os::unix::fs::symlink(&outside, &evidence).unwrap();
                }
                "shared-dir" => {
                    std::fs::set_permissions(&evidence, std::fs::Permissions::from_mode(0o777))
                        .unwrap()
                }
                "shared-file" => {
                    std::fs::set_permissions(&stamp, std::fs::Permissions::from_mode(0o666))
                        .unwrap()
                }
                "directory-file" => {
                    std::fs::remove_file(&stamp).unwrap();
                    std::fs::create_dir(&stamp).unwrap();
                }
                "missing" => std::fs::remove_file(&stamp).unwrap(),
                _ => unreachable!(),
            }
            assert_eq!(world.admit().unwrap(), case == "built", "{layout}: {case}");
            assert_eq!(
                world.target.join(MARKER).exists(),
                case == "built",
                "{layout}: {case}"
            );
            assert_eq!(
                std::fs::read(world.target.join("debug/deps/libprobe.rlib")).unwrap(),
                b"reusable output"
            );
        }
    }
}

fn producer_machine(world: &World) -> super::super::super::Machine<'_> {
    super::super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &|_| Ok(None),
        writable: &|_| Some(true),
    }
}

fn build_into(world: &World, target: &Path) -> Vec<String> {
    vec![
        "--unverified".into(),
        "build".into(),
        format!(
            "--manifest-path={}",
            world.owner.join("Cargo.toml").display()
        ),
        format!("--target-dir={}", target.display()),
    ]
}

#[test]
fn quiet_intermediates_prepare_an_owned_target_without_changing_existing_data_or_modes() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let world = World::new();
    assert!(world.admit().unwrap());
    let quiet = world.target.join(".ubd");
    let build = super::super::super::target_build_path(&world.target);
    for (directory, mode) in [(&quiet, 0o755), (&build, 0o711)] {
        std::fs::create_dir(directory).unwrap();
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(std::fs::metadata(directory).unwrap().mode() & 0o777, mode);
    }
    let old = world.target.join("build.noindex");
    std::fs::create_dir(&old).unwrap();
    for directory in [&old, &build] {
        std::fs::write(directory.join("keep"), b"existing units").unwrap();
    }
    let receipt = std::fs::read(world.record()).unwrap();
    let identities = [identity(&quiet), identity(&build)];
    let args = build_into(&world, &world.target);
    assert!(
        super::super::observe_for_producer(
            &args,
            super::super::super::Tool::Targo,
            &world.home,
            &producer_machine(&world),
            true,
        )
        .unwrap()
    );
    assert_eq!([identity(&quiet), identity(&build)], identities);
    assert_eq!(std::fs::metadata(&quiet).unwrap().mode() & 0o777, 0o755);
    assert_eq!(std::fs::metadata(&build).unwrap().mode() & 0o777, 0o711);
    assert_eq!(std::fs::read(world.record()).unwrap(), receipt);
    for directory in [&old, &build] {
        assert_eq!(
            std::fs::read(directory.join("keep")).unwrap(),
            b"existing units"
        );
    }
    // An unsafe existing component is refused, never repaired into admission.
    std::fs::set_permissions(&quiet, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(
        super::super::observe_for_producer(
            &args,
            super::super::super::Tool::Targo,
            &world.home,
            &producer_machine(&world),
            true,
        )
        .is_err()
    );
    assert_eq!(std::fs::metadata(&quiet).unwrap().mode() & 0o777, 0o777);
    assert_eq!(std::fs::read(world.record()).unwrap(), receipt);
}

#[test]
fn quiet_intermediates_revalidate_the_owned_root_binding_before_the_next_child() {
    let world = World::new();
    assert!(world.admit().unwrap());
    let target = world.target.clone();
    let moved = world.home.join("old-target");
    let moved_for_hook = moved.clone();
    let mut replaced = false;
    faults::set(move |point| {
        if point == Point::Parent && !replaced {
            replaced = true;
            std::fs::rename(&target, &moved_for_hook).unwrap();
            std::fs::create_dir(&target).unwrap();
            std::fs::write(target.join("keep"), b"replacement output").unwrap();
        }
        Some(())
    });
    let fault = Fault;
    assert!(
        super::super::observe_for_producer(
            &build_into(&world, &world.target),
            super::super::super::Tool::Targo,
            &world.home,
            &producer_machine(&world),
            true,
        )
        .is_err()
    );
    drop(fault);
    assert!(
        moved.join(".ubd").is_dir(),
        "the deterministic replacement ran"
    );
    assert!(!super::super::super::target_build_path(&moved).exists());
    assert!(!world.target.join(".ubd").exists());
    assert_eq!(
        std::fs::read(world.target.join("keep")).unwrap(),
        b"replacement output"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn placed_argv_and_config_changes_cannot_bypass_selected_owned_preparation() {
    for shape in ["safe", "argv-mismatch", "config-changed"] {
        let world = World::new();
        assert!(world.admit().unwrap());
        let machine = super::super::super::Machine {
            read: &super::super::super::read_config,
            ..producer_machine(&world)
        };
        let mut args = build_into(&world, &world.target);
        args.insert(0, "+trust".into());
        let (_, mut placed) = super::super::super::placed_build_dir(
            &args,
            super::super::super::Tool::Cargo,
            &machine,
        )
        .unwrap();
        if shape == "argv-mismatch" {
            placed.push("--offline".into());
        }
        let config = world.owner.join(".cargo/config.toml");
        if shape == "config-changed" {
            std::fs::create_dir(config.parent().unwrap()).unwrap();
            let mut changed = false;
            faults::set(move |point| {
                if point == Point::Parent && !changed {
                    changed = true;
                    std::fs::write(&config, "[build]\nbuild-dir = 'caller-layout'\n").unwrap();
                }
                Some(())
            });
        }
        let fault = Fault;
        let result = super::super::observe_placed_command_with(
            &args,
            &placed,
            super::super::super::Tool::Cargo,
            &machine,
        );
        drop(fault);
        assert_eq!(result.is_ok(), shape == "safe", "{shape}: {result:?}");
        if shape != "safe" {
            assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        }
        let child = super::super::super::target_build_path(&world.target);
        assert_eq!(child.is_dir(), shape != "argv-mismatch");
        assert_eq!(roots(&world.home).len(), 1);
        assert!(!world.owner.join("caller-layout").exists());
    }
}

#[test]
fn quiet_intermediates_do_not_create_below_a_descendant_moved_outside_the_target() {
    let world = World::new();
    assert!(world.admit().unwrap());
    let quiet = world.target.join(".ubd");
    let moved = world.home.join("moved-parent");
    let moved_for_hook = moved.clone();
    let mut replaced = false;
    faults::set(move |point| {
        if point == Point::Parent && !replaced {
            replaced = true;
            std::fs::rename(&quiet, &moved_for_hook).unwrap();
            std::fs::create_dir(&quiet).unwrap();
            std::fs::write(moved_for_hook.join("keep"), b"moved output").unwrap();
        }
        Some(())
    });
    let fault = Fault;
    assert!(
        super::super::observe_for_producer(
            &build_into(&world, &world.target),
            super::super::super::Tool::Targo,
            &world.home,
            &producer_machine(&world),
            true,
        )
        .is_err()
    );
    drop(fault);
    assert_eq!(std::fs::read(moved.join("keep")).unwrap(), b"moved output");
    assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 1);
    assert!(!super::super::super::target_build_path(&world.target).exists());
}

#[cfg(target_os = "macos")]
#[test]
fn quiet_receiptless_preparation_revalidates_its_anchor_before_the_next_descendant() {
    let world = World::new();
    let target = world.owner.join("missing-target");
    let anchor = world.owner.clone();
    let moved = world.home.join("moved-anchor");
    let moved_for_hook = moved.clone();
    let mut replaced = false;
    faults::set(move |point| {
        if point == Point::Parent && !replaced {
            replaced = true;
            std::fs::rename(&anchor, &moved_for_hook).unwrap();
            std::fs::create_dir(&anchor).unwrap();
            std::fs::write(anchor.join("keep"), b"replacement workspace").unwrap();
        }
        Some(())
    });
    let fault = Fault;
    assert!(super::super::prepare_target_build_dir(&target).is_err());
    drop(fault);
    let created = moved.join("missing-target");
    assert!(
        created.is_dir(),
        "the anchor was replaced after its first child was prepared"
    );
    assert_eq!(std::fs::read_dir(&created).unwrap().count(), 0);
    assert!(!target.exists());
    assert_eq!(
        std::fs::read(world.owner.join("keep")).unwrap(),
        b"replacement workspace"
    );
    assert!(roots(&world.home).is_empty());
    assert!(!world.home.join(INDEX).exists());
}

#[test]
fn a_target_cargo_made_before_registration_is_adopted_where_it_stands() {
    // A build keeps its rustc facts at the target's top, or in the build leaf cargo also makes
    // when the shim names it.
    for facts in ["top", "leaf"] {
        let world = World::new();
        cargo_laid(&world.target);
        if facts == "leaf" {
            let leaf = super::super::super::target_build_path(&world.target);
            std::fs::DirBuilder::new()
                .mode(0o755)
                .recursive(true)
                .create(&leaf)
                .unwrap();
            std::fs::write(leaf.join("CACHEDIR.TAG"), CARGO_TAG).unwrap();
            std::fs::DirBuilder::new()
                .mode(0o755)
                .create(leaf.join("debug"))
                .unwrap();
            std::fs::rename(
                world.target.join("debug/.fingerprint"),
                leaf.join("debug/.fingerprint"),
            )
            .unwrap();
            std::fs::rename(
                world.target.join(".rustc_info.json"),
                leaf.join(".rustc_info.json"),
            )
            .unwrap();
        }
        let before = identity(&world.target).unwrap();
        let machine = producer_machine(&world);
        let tool = super::super::super::Tool::Targo;
        let args = build_into(&world, &world.target);
        assert!(
            super::super::observe_for_producer(&args, tool, &world.home, &machine, true).unwrap(),
            "{facts}: a target cargo made is admitted, not refused"
        );
        assert_eq!(
            identity(&world.target),
            Some(before.clone()),
            "{facts}: the same folder, neither moved nor replaced"
        );
        assert_eq!(
            std::fs::read(world.target.join("debug/deps/libprobe.rlib")).unwrap(),
            b"reusable output"
        );
        assert_eq!(
            std::fs::read_to_string(world.target.join("CACHEDIR.TAG")).unwrap(),
            CARGO_TAG,
            "{facts}: cargo's tag is left as cargo wrote it"
        );
        let receipt = read(&world.record()).unwrap();
        assert!(super::super::same(&world.home, &receipt), "{facts}");
        assert_eq!(receipt.identity, before);
        assert_eq!(receipt.owners, vec![world.owner.clone()]);
        assert!(!receipt.owners_unknown && receipt.package.is_none() && !receipt.retained);
        assert_eq!(roots(&world.home).len(), 1);
        let recorded = std::fs::read(world.record()).unwrap();
        assert!(
            super::super::observe_for_producer(&args, tool, &world.home, &machine, true).unwrap(),
            "{facts}: the next build is admitted from the record"
        );
        assert_eq!(std::fs::read(world.record()).unwrap(), recorded);
    }
}

/// `root` as cargo lays it under this process's own umask: its folders 0777 and its files 0666
/// less the umask, as cargo 1.96 lays a target (Ubuntu's umask 002 leaves the target 0775 and
/// its tag 0664).
fn cargo_laid_by_umask(root: &Path) {
    std::fs::create_dir(root).unwrap();
    std::fs::write(root.join("CACHEDIR.TAG"), CARGO_TAG).unwrap();
    std::fs::write(root.join(".rustc_info.json"), b"{}").unwrap();
    std::fs::create_dir_all(root.join("debug/deps")).unwrap();
    std::fs::create_dir_all(root.join("debug/.fingerprint/probe-0123456789abcdef")).unwrap();
    std::fs::write(
        root.join("debug/.fingerprint/probe-0123456789abcdef/invoked.timestamp"),
        b"invoked",
    )
    .unwrap();
    std::fs::write(root.join("debug/.cargo-lock"), b"").unwrap();
    std::fs::write(root.join("debug/deps/libprobe.rlib"), b"reusable output").unwrap();
}

/// Custody as a build under this process's real umask meets it, the workspace, the target cargo
/// laid and its tag each made with that umask's modes (the fixtures above lay umask 022's). A
/// umask with no group or other write (022, macOS's) has the target adopted where it stands and
/// a fresh one made in the workspace. One with it (002, the user-private-group default Ubuntu
/// sets) leaves both writable by the group, which custody never binds: a
/// producer that requires managed output is refused, and nothing is written into either. (Only
/// macOS's shim asks custody of a build: `observation_with`.)
#[test]
fn custody_under_this_process_umask_adopts_only_what_no_group_can_write() {
    use std::os::unix::fs::MetadataExt as _;
    let mut world = World::new();
    world.owner = world.home.join("workspace");
    std::fs::create_dir(&world.owner).unwrap();
    std::fs::write(
        world.owner.join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .unwrap();
    cargo_laid_by_umask(&world.target);
    let shared = |at: &Path| std::fs::symlink_metadata(at).unwrap().mode() & 0o022;
    let umask_shares = shared(&world.owner);
    for laid in [world.target.clone(), world.target.join("CACHEDIR.TAG")] {
        assert_eq!(shared(&laid), umask_shares, "{}", laid.display());
    }
    let machine = producer_machine(&world);
    let tool = super::super::super::Tool::Targo;
    let fresh = world.owner.join("target");
    for (target, what) in [
        (&world.target, "the target cargo laid"),
        (&fresh, "a fresh target in the workspace"),
    ] {
        let args = build_into(&world, target);
        let admitted = super::super::observe_for_producer(&args, tool, &world.home, &machine, true);
        if umask_shares == 0 {
            assert!(admitted.unwrap(), "{what}: in custody");
            assert!(target.join(MARKER).is_file(), "{what}: custody declared");
            continue;
        }
        assert!(
            admitted.is_err(),
            "{what}: a folder the group or others can write is refused: {admitted:?}"
        );
        assert!(!target.join(MARKER).exists(), "{what}: no custody declared");
    }
    if umask_shares != 0 {
        assert!(roots(&world.home).is_empty(), "nothing registered");
        assert!(
            !fresh.exists(),
            "no target made in a folder the group can write"
        );
        assert_eq!(
            std::fs::read(world.target.join("debug/deps/libprobe.rlib")).unwrap(),
            b"reusable output",
            "cargo's output stands"
        );
    }
}

#[test]
fn a_folder_cargo_did_not_make_as_its_build_directory_is_never_adopted() {
    use std::os::unix::fs::PermissionsExt as _;
    for case in [
        "untagged",
        "atpkg-tag",
        "other-tag",
        "linked-tag",
        "tag-group-writable",
        "tag-hardlinked",
        "never-built",
        "linked-rustc-info",
        "source",
        "shared",
        "marked",
        "coordinator",
        "leaf-marked",
        "leaf-linked",
        "quiet-linked",
        "nested",
        "shared-cache",
    ] {
        let mut world = World::new();
        if case == "shared-cache" {
            world.target = super::super::super::cache_root(&world.home).join("legacy.noindex");
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(world.target.parent().unwrap())
                .unwrap();
        }
        cargo_laid(&world.target);
        let tag = world.target.join("CACHEDIR.TAG");
        match case {
            "untagged" => std::fs::remove_file(&tag).unwrap(),
            "atpkg-tag" => std::fs::write(&tag, cache_tag_bytes()).unwrap(),
            "other-tag" => std::fs::write(
                &tag,
                "Signature: 8a477f597d28d172789f06886806bc55\n# made by another tool\n",
            )
            .unwrap(),
            "linked-tag" => {
                let elsewhere = world.home.join("CACHEDIR.TAG");
                std::fs::rename(&tag, &elsewhere).unwrap();
                std::os::unix::fs::symlink(&elsewhere, &tag).unwrap();
            }
            "tag-group-writable" => {
                std::fs::set_permissions(&tag, std::fs::Permissions::from_mode(0o664)).unwrap();
            }
            "tag-hardlinked" => std::fs::hard_link(&tag, world.home.join("CACHEDIR.TAG")).unwrap(),
            // Cargo tags its home's git and registry caches where they stand, and never builds
            // in them: `~/.cargo/git` holds the tag, `db` and `checkouts`, and no rustc facts.
            "never-built" => {
                std::fs::remove_file(world.target.join(".rustc_info.json")).unwrap();
                std::fs::create_dir(world.target.join("db")).unwrap();
                std::fs::create_dir(world.target.join("checkouts")).unwrap();
            }
            "linked-rustc-info" => {
                let facts = world.target.join(".rustc_info.json");
                let elsewhere = world.home.join("rustc_info.json");
                std::fs::rename(&facts, &elsewhere).unwrap();
                std::os::unix::fs::symlink(&elsewhere, &facts).unwrap();
            }
            "source" => std::fs::write(world.target.join("Cargo.lock"), b"").unwrap(),
            "shared" => {
                std::fs::set_permissions(&world.target, std::fs::Permissions::from_mode(0o775))
                    .unwrap();
            }
            "marked" => std::fs::write(world.target.join(MARKER), b"unknown schema").unwrap(),
            "coordinator" => {
                std::fs::write(world.target.join(super::super::package::COORDINATOR), b"").unwrap();
            }
            "leaf-marked" => {
                let leaf = super::super::super::target_build_path(&world.target);
                std::fs::create_dir_all(&leaf).unwrap();
                std::fs::write(leaf.join(super::super::package::COORDINATOR), b"").unwrap();
            }
            "leaf-linked" => {
                let elsewhere = world.home.join("leaf.noindex");
                std::fs::create_dir(&elsewhere).unwrap();
                std::fs::create_dir(world.target.join(".ubd")).unwrap();
                std::os::unix::fs::symlink(
                    &elsewhere,
                    super::super::super::target_build_path(&world.target),
                )
                .unwrap();
            }
            "quiet-linked" => {
                let elsewhere = world.home.join("quiet-elsewhere");
                std::fs::create_dir(&elsewhere).unwrap();
                std::os::unix::fs::symlink(&elsewhere, world.target.join(".ubd")).unwrap();
            }
            "nested" => {
                assert!(
                    create_checked(
                        &world.home,
                        &world.target.join("inner.noindex"),
                        &world.owner
                    )
                    .unwrap()
                );
            }
            "shared-cache" => {}
            _ => unreachable!(),
        }
        let held = std::fs::read(world.target.join("debug/deps/libprobe.rlib")).unwrap();
        let registered = roots(&world.home).len();
        let admitted = create_checked(&world.home, &world.target, &world.owner);
        if case == "marked" {
            assert!(
                admitted.is_err(),
                "{case}: declared custody it cannot record stops the build"
            );
        } else {
            assert!(!admitted.unwrap(), "{case}: not adopted");
        }
        if case != "marked" {
            assert!(
                !world.target.join(MARKER).exists(),
                "{case}: no custody declared"
            );
        }
        assert!(read(&world.record()).is_none(), "{case}: nothing recorded");
        assert_eq!(roots(&world.home).len(), registered, "{case}");
        assert_eq!(
            std::fs::read(world.target.join("debug/deps/libprobe.rlib")).unwrap(),
            held,
            "{case}: its contents stand"
        );
    }
}

#[test]
fn an_adopted_target_stays_while_its_workspace_does_and_is_retired_once_it_goes() {
    let world = World::new();
    cargo_laid(&world.target);
    assert!(world.admit().unwrap());
    let later = std::time::SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60);
    let pass = || {
        super::super::reclaim(
            &world.home,
            later,
            super::super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        )
    };
    assert!(pass().is_empty(), "its workspace stands");
    assert_eq!(
        std::fs::read(world.target.join("debug/deps/libprobe.rlib")).unwrap(),
        b"reusable output"
    );
    std::fs::remove_dir_all(&world.owner).unwrap();
    assert!(
        matches!(pass().as_slice(), [(path, super::super::super::Unused::Gone { .. })] if path == &world.target),
        "the registered authority retires what ordinary cleanup no longer may"
    );
    assert!(!world.target.exists());
}

#[test]
fn a_folder_renamed_away_while_it_is_adopted_keeps_no_marker() {
    let world = World::new();
    cargo_laid(&world.target);
    let target = world.target.clone();
    let aside = world.home.join("aside.noindex");
    let moved = aside.clone();
    faults::set(move |point| {
        if point == Point::Adopted {
            std::fs::rename(&target, &moved).unwrap();
        }
        Some(())
    });
    let fault = Fault;
    assert!(
        world.admit().is_err(),
        "no build admitted on a folder that moved"
    );
    drop(fault);
    assert!(
        !aside.join(MARKER).exists(),
        "no marker names a path its folder left"
    );
    assert_eq!(
        std::fs::read(aside.join("debug/deps/libprobe.rlib")).unwrap(),
        b"reusable output"
    );
    assert!(!super::super::same(
        &world.home,
        &read(&world.record()).unwrap()
    ));
}

#[test]
fn a_marker_that_appears_while_a_folder_is_adopted_is_never_replaced() {
    let world = World::new();
    cargo_laid(&world.target);
    let marker = world.target.join(MARKER);
    let written = marker.clone();
    faults::set(move |point| {
        if point == Point::Adopted {
            std::fs::write(&written, b"another custody").unwrap();
        }
        Some(())
    });
    let fault = Fault;
    assert!(world.admit().is_err());
    drop(fault);
    assert_eq!(std::fs::read(&marker).unwrap(), b"another custody");
    assert!(
        std::fs::read_dir(&world.target).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".aterm-adopting-")),
        "the unpublished marker file is gone"
    );
}

#[test]
fn an_adoption_on_record_is_never_finished_on_another_folder() {
    let world = World::new();
    cargo_laid(&world.target);
    let fault = stop_at(Point::Adopted);
    assert!(world.admit().is_err());
    drop(fault);
    let intent = read(&world.record()).unwrap();
    std::fs::rename(&world.target, world.home.join("first.noindex")).unwrap();
    cargo_laid(&world.target);
    let second = identity(&world.target).unwrap();
    assert!(
        world.admit().unwrap(),
        "the folder now at the path is adopted in its own right"
    );
    let adopted = read(&world.record()).unwrap();
    assert_eq!(adopted.identity, second);
    assert_ne!(
        adopted.nonce, intent.nonce,
        "a receipt of another inode finishes nothing here"
    );
    assert!(super::super::same(&world.home, &adopted));
    assert!(!world.home.join("first.noindex").join(MARKER).exists());
}

#[test]
fn a_registered_target_cargo_made_again_after_it_went_is_adopted_anew() {
    for case in ["rebuilt", "kept", "untagged"] {
        let world = World::new();
        assert!(
            world.admit().unwrap(),
            "{case}: a fresh creation registers it"
        );
        let first = read(&world.record()).unwrap();
        // The new folder is made while the first still stands, so it never reuses its inode.
        let gone = world.home.join("gone.noindex");
        std::fs::rename(&world.target, &gone).unwrap();
        cargo_laid(&world.target);
        std::fs::remove_dir_all(&gone).unwrap();
        match case {
            "rebuilt" => {}
            // A keep is a person's, never ordinary custody a build may replace.
            "kept" => {
                let mut kept = first.clone();
                kept.schema = 4;
                kept.retained = true;
                write(&world.record(), &kept).unwrap();
            }
            "untagged" => std::fs::remove_file(world.target.join("CACHEDIR.TAG")).unwrap(),
            _ => unreachable!(),
        }
        let rebuilt = identity(&world.target).unwrap();
        let recorded = std::fs::read(world.record()).unwrap();
        if case == "rebuilt" {
            assert!(
                world.admit().unwrap(),
                "a target a build outside atpkg made again is adopted, not refused"
            );
            let receipt = read(&world.record()).unwrap();
            assert!(super::super::same(&world.home, &receipt));
            assert_eq!(receipt.identity, rebuilt);
            assert_ne!(receipt.nonce, first.nonce);
            assert_eq!(receipt.owners, vec![world.owner.clone()]);
            assert_eq!(
                std::fs::read(world.target.join("debug/deps/libprobe.rlib")).unwrap(),
                b"reusable output"
            );
        } else {
            assert!(!world.admit().unwrap(), "{case}: not adopted");
            assert!(!world.target.join(MARKER).exists(), "{case}");
            assert_eq!(
                std::fs::read(world.record()).unwrap(),
                recorded,
                "{case}: the record stands"
            );
        }
    }
}

#[test]
fn a_record_of_this_very_folder_that_adoption_did_not_write_is_neither_finished_nor_replaced() {
    for case in ["owners-unknown", "no-owner"] {
        let world = World::new();
        cargo_laid(&world.target);
        let fault = stop_at(Point::Adopted);
        assert!(world.admit().is_err());
        drop(fault);
        let mut changed = read(&world.record()).unwrap();
        match case {
            "owners-unknown" => changed.owners_unknown = true,
            "no-owner" => changed.owners.clear(),
            _ => unreachable!(),
        }
        write(&world.record(), &changed).unwrap();
        let recorded = std::fs::read(world.record()).unwrap();
        assert!(!world.admit().unwrap(), "{case}: not adopted");
        assert!(!world.target.join(MARKER).exists(), "{case}");
        assert_eq!(
            std::fs::read(world.record()).unwrap(),
            recorded,
            "{case}: the record stands"
        );
    }
}

#[test]
fn an_adoption_stopped_before_its_marker_is_finished_by_the_next_build() {
    let world = World::new();
    cargo_laid(&world.target);
    let before = identity(&world.target).unwrap();
    let fault = stop_at(Point::Adopted);
    assert!(world.admit().is_err(), "no build admitted on failure");
    drop(fault);
    let intent = read(&world.record()).unwrap();
    assert!(!world.target.join(MARKER).exists());
    assert!(
        !super::super::same(&world.home, &intent),
        "a receipt without its marker grants nothing"
    );
    assert!(roots(&world.home).is_empty());
    assert!(
        ordinary_clearance(&world.home, &world.target).is_err(),
        "ordinary cleaners already stay out"
    );
    assert!(
        world.admit().unwrap(),
        "a later build finishes the adoption"
    );
    let after = read(&world.record()).unwrap();
    assert_eq!(after, intent, "the adoption on record is the one finished");
    assert!(super::super::same(&world.home, &after));
    assert_eq!(identity(&world.target), Some(before));

    // A receipt whose marker went while its folder no longer proves cargo made it stays unfinished.
    std::fs::remove_file(world.target.join(MARKER)).unwrap();
    std::fs::remove_file(world.target.join("CACHEDIR.TAG")).unwrap();
    assert!(!world.admit().unwrap());
    assert!(!world.target.join(MARKER).exists());
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
            let child = super::super::super::target_build_path(&world.target);
            std::fs::create_dir_all(&child).unwrap();
            std::fs::write(child.join("kept"), b"unmanaged intermediates").unwrap();
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

#[cfg(target_os = "macos")]
#[test]
fn the_shims_registrar_admits_a_target_cargo_made_before_registration() {
    for mode in ["legacy", "required"] {
        let world = World::new();
        cargo_laid(&world.target);
        let before = identity(&world.target).unwrap();
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
        assert_eq!(result.status.code(), Some(0), "{mode}: {result:?}");
        let receipt = read(&world.record()).unwrap();
        assert!(super::super::same(&world.home, &receipt), "{mode}");
        assert_eq!(receipt.identity, before, "{mode}");
        assert_eq!(
            std::fs::read(world.target.join("debug/deps/libprobe.rlib")).unwrap(),
            b"reusable output"
        );
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
