// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::time::{Duration, SystemTime};

#[cfg(target_os = "macos")]
#[test]
fn automatic_final_targets_preserve_modes_and_refuse_unsafe_descendants() {
    for shape in [
        "fresh",
        "existing",
        "target-link",
        "quiet-link",
        "final-link",
        "target-writable",
        "quiet-writable",
        "final-writable",
    ] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let root = home.join("source");
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&root)
            .unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outside = home.join("outside");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&outside)
            .unwrap();
        std::fs::write(outside.join("keep"), b"retained").unwrap();
        let target = root.join("target");
        let quiet = target.join(".ubd");
        let final_dir = root.join(aterm_toolchain::build_cache::DEFAULT_TARGET);
        if shape != "fresh" {
            for (level, path) in [
                ("target", &target),
                ("quiet", &quiet),
                ("final", &final_dir),
            ] {
                if shape.strip_suffix("-link") == Some(level) {
                    std::os::unix::fs::symlink(&outside, path).unwrap();
                    break;
                }
                std::fs::DirBuilder::new().mode(0o755).create(path).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
                if shape.strip_suffix("-writable") == Some(level) {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777)).unwrap();
                    break;
                }
            }
        }
        if shape == "existing" {
            std::fs::write(final_dir.join("keep"), b"existing output").unwrap();
        }
        let safe = matches!(shape, "fresh" | "existing");
        let result = prepare_default_target(&root);
        assert_eq!(result.is_ok(), safe, "{shape}: {result:?}");
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o755
        );
        if safe {
            assert!(final_dir.is_dir());
            let expected = if shape == "fresh" { 0o700 } else { 0o755 };
            for path in [&target, &quiet, &final_dir] {
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    expected
                );
                assert!(!path.join(MARKER).exists());
            }
            if shape == "existing" {
                assert_eq!(
                    std::fs::read(final_dir.join("keep")).unwrap(),
                    b"existing output"
                );
            }
        } else if shape.ends_with("writable") {
            let path = if shape.starts_with("target") {
                &target
            } else if shape.starts_with("quiet") {
                &quiet
            } else {
                &final_dir
            };
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o777
            );
        }
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"retained");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
        assert!(roots(&home).is_empty());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn automatic_final_targets_revalidate_each_binding_before_extending_it() {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            creation::faults::clear();
        }
    }
    for level in ["root", "target", "quiet"] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let root = home.join("source");
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&root)
            .unwrap();
        let outside = home.join("moved");
        let from = match level {
            "root" => root.clone(),
            "target" => root.join("target"),
            _ => root.join("target/.ubd"),
        };
        let replacement = from.clone();
        let replacement_check = replacement.clone();
        let moved = outside.clone();
        let mut steps = 0;
        creation::faults::set(move |point| {
            if point == creation::Point::Parent {
                steps += 1;
                if steps == if level == "quiet" { 2 } else { 1 } {
                    std::fs::rename(&from, &moved).unwrap();
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&replacement)
                        .unwrap();
                }
            }
            Some(())
        });
        let clear = Clear;
        assert!(prepare_default_target(&root).is_err(), "{level}");
        drop(clear);
        let next = if level == "quiet" {
            "main.noindex"
        } else if level == "target" {
            ".ubd"
        } else {
            "target/.ubd"
        };
        assert!(!outside.join(next).exists(), "moved {level}");
        let replacement_next = if level == "quiet" {
            root.join("target/.ubd/main.noindex")
        } else {
            root.join("target/.ubd")
        };
        assert!(!replacement_next.exists(), "replacement {level}");
        assert_eq!(
            std::fs::read_dir(replacement_check).unwrap().count(),
            0,
            "replacement {level}"
        );
        assert!(roots(&home).is_empty());
    }
}

#[test]
fn resource_capabilities_query_reaches_native_without_observing_cache_ownership() {
    let args = vec!["--resource-capabilities".to_owned()];
    let home = Path::new("/unreadable-home");
    let machine = super::super::Machine {
        home: Some(home),
        root: None,
        cwd: Some(home),
        env: &|_| panic!("a capability query must not inspect inherited cache settings"),
        read: &|_| panic!("a capability query must not read or register caches"),
        read_manifest: &|_| panic!("a capability query must not read or register caches"),
        writable: &|_| panic!("a capability query must not require cache writes"),
    };
    assert!(!observe_for_producer(&args, super::super::Tool::Targo, home, &machine, true).unwrap());
    assert!(crate::build_admit::classify(&args, super::super::Tool::Targo).is_none());
    for tool in [super::super::Tool::Cargo, super::super::Tool::Tippy] {
        assert!(!non_producing_command(&args, tool, &machine));
    }
    // A name no builtin command has is read as Targo reads it, an alias's or an external
    // command's, from the configs and the environment.
    let inert = super::super::Machine {
        env: &|_| None,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        ..machine
    };
    for args in [
        vec!["--resource-capabilities", "build"],
        vec!["--resource-capabilities", "--target-dir=elsewhere"],
        vec!["--unverified", "--resource-capabilities"],
        vec!["alias", "--resource-capabilities"],
    ] {
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let machine = if args[0] == "alias" { &inert } else { &machine };
        assert!(!non_producing_command(
            &args,
            super::super::Tool::Targo,
            machine
        ));
    }
}

struct World {
    _directory: aterm_tempfile::TempDir,
    home: PathBuf,
    owner: PathBuf,
    target: PathBuf,
}

#[cfg(unix)]
#[test]
fn an_unanswered_helper_query_is_not_a_cached_old_helper() {
    let directory = aterm_tempfile::tempdir().unwrap();
    for supported in [false, true] {
        let helper = directory.path().join(format!("helper-{supported}"));
        std::fs::write(&helper, b"fixed helper identity").unwrap();
        let error = helper_speaks_hop_with(&helper, |_| Err(std::io::ErrorKind::TimedOut.into()))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(
            helper_speaks_hop_with(&helper, |_| Ok(supported)).unwrap(),
            supported,
            "an unanswered query supplied no capability fact"
        );
        assert_eq!(
            helper_speaks_hop_with(&helper, |_| panic!("completed answer is cached")).unwrap(),
            supported
        );
    }
}

#[cfg(unix)]
#[test]
fn an_interrupted_helper_supplies_no_cached_capability() {
    use std::os::unix::process::ExitStatusExt as _;
    let directory = aterm_tempfile::tempdir().unwrap();
    for supported in [false, true] {
        let helper = directory.path().join(format!("interrupted-{supported}"));
        std::fs::write(&helper, b"fixed helper identity").unwrap();
        for _ in 0..2 {
            let error = helper_speaks_hop_with(&helper, |_| {
                helper_hop_answer(std::process::Output {
                    status: std::process::ExitStatus::from_raw(libc::SIGKILL),
                    stdout: format!("{}\n", crate::build_admit::HOP_CAPABILITY).into_bytes(),
                    stderr: Vec::new(),
                })
            })
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        }
        assert_eq!(
            helper_speaks_hop_with(&helper, |_| {
                helper_hop_answer(std::process::Output {
                    status: std::process::ExitStatus::from_raw(if supported { 0 } else { 2 << 8 }),
                    stdout: if supported {
                        format!("{}\n", crate::build_admit::HOP_CAPABILITY).into_bytes()
                    } else {
                        b"unsupported".to_vec()
                    },
                    stderr: Vec::new(),
                })
            })
            .unwrap(),
            supported
        );
        assert_eq!(
            helper_speaks_hop_with(&helper, |_| panic!("completed answer is cached")).unwrap(),
            supported
        );
    }
}

#[cfg(unix)]
#[test]
fn a_changed_helper_supplies_no_cached_capability() {
    let directory = aterm_tempfile::tempdir().unwrap();
    let helper = directory.path().join("helper");
    std::fs::write(&helper, b"first identity").unwrap();
    assert!(
        helper_speaks_hop_with(&helper, |_| {
            std::fs::write(&helper, b"a different helper identity")?;
            Ok(true)
        })
        .is_err()
    );
    assert!(!helper_speaks_hop_with(&helper, |_| Ok(false)).unwrap());
}

#[cfg(unix)]
#[test]
fn a_pending_helper_query_does_not_block_another_helper() {
    use std::sync::mpsc;
    let detector = std::time::Duration::from_secs(60);
    let directory = aterm_tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    std::fs::write(&first, b"first helper").unwrap();
    std::fs::write(&second, b"second helper").unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let held = std::thread::spawn(move || {
        helper_speaks_hop_with(&first, |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(detector).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "test release did not arrive")
            })?;
            Ok(true)
        })
    });
    entered_rx.recv_timeout(detector).unwrap();
    let (finished_tx, finished_rx) = mpsc::channel();
    let independent = std::thread::spawn(move || {
        let answer = helper_speaks_hop_with(&second, |_| Ok(false));
        finished_tx.send(answer).unwrap();
    });
    let independent_answer = finished_rx.recv_timeout(detector);
    let _ = release_tx.send(());
    let held_answer = held.join().unwrap();
    independent.join().unwrap();
    assert!(held_answer.unwrap());
    assert!(!independent_answer.unwrap().unwrap());
}

#[test]
fn native_build_tree_administration_does_not_read_or_adopt_build_caches() {
    let directory = aterm_tempfile::tempdir().unwrap();
    let home = directory.path();
    let machine = super::super::Machine {
        home: Some(home),
        root: None,
        cwd: Some(home),
        env: &|_| panic!("native tree administration must not inspect build-cache settings"),
        read: &|_| panic!("native tree administration must not read build-cache config"),
        read_manifest: &|_| panic!("native tree administration must not read build-cache config"),
        writable: &|_| panic!("native tree administration must not register build output"),
    };
    let inert = super::super::Machine {
        env: &|_| None,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        ..machine
    };
    for operation in [
        None,
        Some("--protocol"),
        Some("--help"),
        Some("-h"),
        Some("status"),
        Some("acquire"),
        Some("release"),
        Some("break"),
        Some("reclaim-incremental"),
    ] {
        let mut args = vec!["trust".into(), "repo".into(), "lock".into()];
        args.extend(operation.map(str::to_owned));
        assert!(
            !observe_for_producer(&args, super::super::Tool::Targo, home, &machine, true).unwrap(),
            "{args:?}"
        );
        assert!(crate::build_admit::classify(&args, super::super::Tool::Targo).is_none());
        // Another tool's `trust` is a command a user alias may take, whose config is read to
        // tell: on a machine that names none, it is no native operation.
        for tool in [super::super::Tool::Cargo, super::super::Tool::Tippy] {
            assert!(!non_producing_command(&args, tool, &inert));
        }
    }
    assert_eq!(std::fs::read_dir(home).unwrap().count(), 0);
    for words in [
        vec!["trust", "repo", "lock", "exec", "--", "build"],
        vec!["trust", "repo", "lock", "unknown"],
        vec!["trust", "repo", "locker", "status"],
        vec!["--config", "trust", "repo", "lock", "status"],
        vec!["alias", "trust", "repo", "lock", "status"],
        vec![
            "--unverified",
            "run",
            "--",
            "trust",
            "repo",
            "lock",
            "status",
        ],
        vec!["--unverified", "build", "trust", "repo", "lock", "status"],
    ] {
        let args = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
        // Each may produce, so its aliases are read (`repo`, `alias` may be one).
        assert!(
            !non_producing_command(&args, super::super::Tool::Targo, &inert),
            "{args:?}"
        );
    }
}

#[test]
fn trust_cargo_custody_reads_each_twin_with_a_toolchain_and_no_lane() {
    use super::super::Tool;
    for twin in ["TARGO_TARGET_DIR", "TARGO_BUILD_TARGET_DIR", "TARGO_HOME"] {
        for tool in [Tool::CargoCompat, Tool::Cargo] {
            let world = World::new();
            let package_home = world.home.join("package-home");
            let config = package_home.join("config.toml");
            let env = |name: &str| {
                (name == twin).then(|| {
                    if twin == "TARGO_HOME" {
                        package_home.clone()
                    } else {
                        world.target.clone()
                    }
                    .into_os_string()
                })
            };
            let read = |path: &Path| {
                if twin == "TARGO_HOME" && path == config {
                    Ok(Some(format!(
                        "[build]\ntarget-dir = '{}'\n",
                        world.target.display()
                    )))
                } else {
                    match std::fs::read_to_string(path) {
                        Ok(text) => Ok(Some(text)),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                        Err(_) => Err(()),
                    }
                }
            };
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&world.owner),
                env: &env,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let result = observe_for_producer(
                &["+trust".into(), "build".into()],
                tool,
                &world.home,
                &machine,
                true,
            );
            let recorded = roots(&world.home);
            if tool == Tool::CargoCompat {
                // Ambiguous package-home layouts may refuse, but must never run unobserved.
                assert!(
                    result.is_err() || recorded.iter().any(|r| r.root == world.target),
                    "{twin}: {result:?}"
                );
                if twin != "TARGO_HOME" {
                    assert!(result.unwrap(), "{twin}");
                }
            } else {
                assert!(result.is_ok(), "{twin}: {result:?}");
                assert!(!world.target.exists(), "stock Cargo must ignore {twin}");
                assert!(!recorded.iter().any(|r| r.root == world.target));
            }
        }
    }
}

/// TRUST'S CARGO-COMPATIBLE NAME READS A `-C` AS TARGO DOES: it is Targo's own program, on
/// Targo's `trust` channel, which allows the gate, its `+toolchain` read past as an upstream
/// cargo's, so a gated `cargo +trust -Zunstable-options -C <package> build` from a folder
/// with no package builds that package into its target, which the front records, owned by
/// it. NEGATIVE CONTROL: the same call with no gate, which Targo refuses for want of it, is
/// refused with nothing recorded, as is an upstream cargo's on a released channel
/// (`+stable`), which allows no nightly feature.
#[test]
fn trust_cargo_reads_a_gated_directory_as_targo_does() {
    use super::super::Tool;
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let produce = |tool: Tool, args: &[&str]| {
        let world = World::new();
        let notes = world.home.join("notes");
        std::fs::create_dir(&notes).unwrap();
        let owner = world.owner.display().to_string();
        let args: Vec<String> = args
            .iter()
            .map(|word| {
                if *word == "{owner}" {
                    owner.clone()
                } else {
                    (*word).to_owned()
                }
            })
            .collect();
        let env = |name: &str| {
            (name == "CARGO_TARGET_DIR").then(|| world.target.clone().into_os_string())
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&notes),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let verdict = observe_for_producer(&args, tool, &world.home, &machine, true).ok();
        let owners: Vec<PathBuf> = roots(&world.home)
            .into_iter()
            .flat_map(|root| root.owners)
            .collect();
        (verdict, owners, world)
    };
    let gated = ["+trust", "-Zunstable-options", "-C", "{owner}", "build"];
    let (verdict, owners, world) = produce(Tool::CargoCompat, &gated);
    assert_eq!(verdict, Some(true));
    assert_eq!(owners, std::slice::from_ref(&world.owner));
    let (verdict, owners, _world) =
        produce(Tool::CargoCompat, &["+trust", "-C", "{owner}", "build"]);
    assert_eq!((verdict, owners.len()), (None, 0), "ungated");
    let (verdict, owners, _world) = produce(
        Tool::Cargo,
        &["+stable", "-Zunstable-options", "-C", "{owner}", "build"],
    );
    assert_eq!((verdict, owners.len()), (None, 0), "upstream");
}

/// A RELATIVE `CARGO_HOME` UNDER A `-C` BY THE CARGO NAME IS REFUSED: Targo run directly joins
/// it to the `-C` directory, while a rustup proxy (which `cargo +trust` or a pinned `cargo`
/// goes through) exports it joined to the launcher's folder first, so the launcher's `.ch`
/// config, which names a target outside every recorded folder, may be the one the build
/// reads. Each tool by the cargo name, gated, is refused with nothing recorded, and so is a
/// user alias the `-C` directory's home reads as `clean` or as a query where the launcher's
/// reads it as a build: the alias is the home's, so the call is read in both.
/// NEGATIVE CONTROL: the same call with that home absolute records `out`, owned; a builtin
/// `clean`, or an alias both homes read as one, is not refused and records nothing; with no
/// `-C`, or by Targo's name, which reads the `-C` directory's home, it is not refused, and
/// Targo's records nothing of the launcher's.
#[test]
fn a_relative_cargo_home_under_a_directory_by_the_cargo_name_is_refused() {
    use super::super::Tool;
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let produce = |tool: Tool, args: &[&str], absolute: bool| {
        let world = World::new();
        let notes = world.home.join("notes");
        std::fs::create_dir_all(notes.join(".ch")).unwrap();
        let out = world.home.join("out");
        std::fs::write(
            notes.join(".ch/config.toml"),
            format!(
                "[build]\ntarget-dir = \"{}\"\n\
                 [alias]\nx = \"build\"\ny = \"build\"\nz = \"clean\"\n",
                out.display()
            ),
        )
        .unwrap();
        // The `-C` directory's home, which reads `x` as `clean` and `y` as a query.
        std::fs::create_dir_all(world.owner.join(".ch")).unwrap();
        std::fs::write(
            world.owner.join(".ch/config.toml"),
            "[alias]\nx = \"clean\"\ny = \"build --help\"\nz = \"clean\"\n",
        )
        .unwrap();
        let owner = world.owner.display().to_string();
        let args: Vec<String> = args
            .iter()
            .map(|word| {
                if *word == "{owner}" {
                    owner.clone()
                } else {
                    (*word).to_owned()
                }
            })
            .collect();
        let home = if absolute {
            notes.join(".ch").into_os_string()
        } else {
            ".ch".into()
        };
        let env = |name: &str| (name == "CARGO_HOME").then(|| home.clone());
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&notes),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let verdict = observe_for_producer(&args, tool, &world.home, &machine, true);
        let recorded: Vec<PathBuf> = roots(&world.home)
            .into_iter()
            .map(|root| root.root)
            .collect();
        (verdict, recorded, world, out)
    };
    for (tool, args) in [
        (
            Tool::CargoCompat,
            &["+trust", "-Zunstable-options", "-C", "{owner}", "build"][..],
        ),
        (
            Tool::CargoCompat,
            &["-Zunstable-options", "-C", "{owner}", "build"][..],
        ),
        (
            Tool::Cargo,
            &["+nightly", "-Zunstable-options", "-C", "{owner}", "build"][..],
        ),
        // A custom name rustup may link to a nightly cargo, whose channel is untold.
        (
            Tool::Cargo,
            &["+1dev", "-Zunstable-options", "-C", "{owner}", "build"][..],
        ),
        (
            Tool::Cargo,
            &["+beta-x", "-Zunstable-options", "-C", "{owner}", "build"][..],
        ),
        // An alias the `-C` directory's home reads as `clean`, and one it reads as a query,
        // which the launcher's reads as a build into `out`.
        (
            Tool::CargoCompat,
            &["+trust", "-Zunstable-options", "-C", "{owner}", "x"][..],
        ),
        (
            Tool::CargoCompat,
            &["+trust", "-Zunstable-options", "-C", "{owner}", "y"][..],
        ),
    ] {
        let (verdict, recorded, _world, out) = produce(tool, args, false);
        assert!(verdict.is_err(), "{tool:?} {args:?}: {verdict:?}");
        assert!(recorded.is_empty(), "{tool:?} {args:?}: {recorded:?}");
        assert!(!out.exists());
    }
    // An absolute home is the same from either folder: the launcher's config names `out`,
    // which this reader then reads and owes custody by.
    let gated = ["+trust", "-Zunstable-options", "-C", "{owner}", "build"];
    let (verdict, _, world, out) = produce(Tool::CargoCompat, &gated, true);
    assert!(
        verdict.as_ref().is_ok_and(|owned| *owned)
            && roots(&world.home).iter().any(|root| root.root == out),
        "{verdict:?}"
    );
    // A call that builds nothing in either home, a builtin `clean` or an alias both read
    // as one, is not refused, and records nothing.
    for verb in ["clean", "z"] {
        let args = ["+trust", "-Zunstable-options", "-C", "{owner}", verb];
        let (verdict, recorded, _world, _) = produce(Tool::CargoCompat, &args, false);
        assert!(matches!(verdict, Ok(false)), "{verb}: {verdict:?}");
        assert!(recorded.is_empty(), "{verb}: {recorded:?}");
    }
    // With no `-C`, the launcher's folder is the one both read the home against: the call,
    // from a folder that holds no package, is not refused.
    let (verdict, _, _world, _) = produce(Tool::CargoCompat, &["+trust", "build"], false);
    assert!(verdict.is_ok(), "{verdict:?}");
    // Targo's name reads the `-C` directory's home, which holds no config: no target is
    // named, nothing is refused and the launcher's `out` is never read.
    let (verdict, recorded, _world, _) = produce(
        Tool::Targo,
        &[
            "-Zunstable-options",
            "-C",
            "{owner}",
            "--unverified",
            "build",
        ],
        false,
    );
    assert!(verdict.is_ok(), "{verdict:?}");
    assert!(recorded.is_empty(), "{recorded:?}");
}

/// Explicit target custody follows the physical directory Cargo changes into. A linked
/// directory's `..` is its target's parent, not the launcher's lexical parent.
#[test]
fn directory_changed_producers_own_the_effective_package_target() {
    use super::super::Tool;
    for spelling in ["detached", "attached", "cluster", "linked"] {
        for tool in [Tool::Targo, Tool::CargoCompat, Tool::Cargo] {
            let world = World::new();
            let launcher = world.another_owner();
            std::fs::create_dir(world.owner.join("sub")).unwrap();
            std::os::unix::fs::symlink(world.owner.join("sub"), launcher.join("linked")).unwrap();
            let relative = format!("../{}", world.owner.file_name().unwrap().to_str().unwrap());
            let mut args: Vec<String> = match spelling {
                "attached" => vec![format!("-C{relative}")],
                "cluster" => vec!["-qC".into(), relative.clone()],
                "linked" => vec!["-C".into(), "linked/..".into()],
                _ => vec!["-C".into(), relative.clone()],
            };
            args.insert(0, "-Zunstable-options".into());
            if tool == Tool::Targo {
                args.push("--unverified".into());
            } else {
                args.insert(0, "+trust".into());
            }
            args.extend([
                "build".into(),
                "--target-dir".into(),
                "fresh.noindex".into(),
            ]);
            let read = |path: &Path| match std::fs::read_to_string(path) {
                Ok(text) => Ok(Some(text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(_) => Err(()),
            };
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&launcher),
                env: &|_| None,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            assert!(
                observe_for_producer(&args, tool, &world.home, &machine, true).unwrap(),
                "{spelling} {tool:?}"
            );
            let recorded = roots(&world.home);
            assert_eq!(recorded.len(), 1, "{spelling} {tool:?}");
            assert_eq!(recorded[0].root, world.owner.join("fresh.noindex"));
            assert_eq!(
                super::read(&recorded[0].receipt_path).unwrap().owners,
                [std::fs::canonicalize(&world.owner).unwrap()]
            );
            assert!(!launcher.join("fresh.noindex").exists());
        }
    }
}

/// A `-C` call whose custody the front cannot grant is refused: a directory Targo cannot
/// change into and a config it cannot read there with nothing recorded, and a foreign target
/// that already stands with its contents kept and never adopted, its quiet intermediates
/// recorded in it as the same call typed from the package records them (`existing-here`,
/// `package_registers_its_fresh_target_without_adopting_existing_output`); a query and a
/// package outside HOME owe nothing.
#[test]
fn directory_changed_producers_refuse_unknown_custody_and_leave_queries_alone() {
    use super::super::Tool;
    for case in [
        "missing",
        "file",
        "existing",
        "existing-here",
        "unknown-config",
        "query",
        "outside",
    ] {
        let world = World::new();
        let launcher = world.another_owner();
        let mut directory = world.owner.clone();
        if case == "missing" {
            directory = world.home.join("absent");
        }
        if case == "file" {
            directory = world.owner.join("Cargo.toml");
        }
        if case == "outside" {
            directory = world.home.parent().unwrap().join("foreign");
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        }
        let existing = case.starts_with("existing");
        let target = directory.join("fresh.noindex");
        if existing {
            std::fs::create_dir(&target).unwrap();
            std::fs::write(target.join("keep"), b"foreign output").unwrap();
        }
        let moved = ["-Zunstable-options", "-C", directory.to_str().unwrap()];
        let args: Vec<String> = moved[..if case == "existing-here" { 0 } else { 3 }]
            .iter()
            .copied()
            .chain([
                "--unverified",
                if case == "query" { "help" } else { "build" },
                "--target-dir",
                "fresh.noindex",
            ])
            .map(str::to_owned)
            .collect();
        let launcher = if case == "existing-here" {
            world.owner.clone()
        } else {
            launcher
        };
        let read = |path: &Path| {
            if case == "unknown-config" && path == world.owner.join(".cargo/config.toml") {
                return Err(());
            }
            match std::fs::read_to_string(path) {
                Ok(text) => Ok(Some(text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(_) => Err(()),
            }
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&launcher),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let result = observe_for_producer(&args, Tool::Targo, &world.home, &machine, true);
        if matches!(case, "query" | "outside") {
            assert!(!result.unwrap_or_else(|e| panic!("{case}: {e}")), "{case}");
        } else {
            assert!(result.is_err(), "{case}: {result:?}");
        }
        let recorded: Vec<PathBuf> = roots(&world.home)
            .into_iter()
            .map(|root| root.root)
            .collect();
        if existing {
            assert_eq!(
                std::fs::read(target.join("keep")).unwrap(),
                b"foreign output"
            );
            assert!(!target.join(MARKER).exists(), "{case}");
            assert_eq!(
                recorded,
                [super::super::target_build_path(&target)],
                "{case}"
            );
        } else {
            assert!(recorded.is_empty(), "{case}");
            assert!(!target.exists(), "{case}");
        }
    }
}

/// A CLI config file the front reads as Targo does keeps the typed target's custody, from
/// the launcher's folder and through a `-C`: one that names no directory, one whose target
/// directory the typed `--target-dir` outranks, and an empty `include`. NEGATIVE CONTROL: a
/// build directory, an `include` naming a file that is absent and not optional, text Targo
/// cannot parse, and a file the reader cannot read are refused with nothing recorded.
#[test]
fn cli_configs_preserve_selected_explicit_custody() {
    use super::super::Tool;
    for moved in [false, true] {
        for file_form in [false, true] {
            for (text, expect_owned) in [
                ("build.jobs = 2", true),
                (r#"env.ATPKG_NEUTRAL = 'a\b'"#, true),
                (
                    r#"env.ATPKG_NEUTRAL = 'target-dir build-dir include'"#,
                    true,
                ),
                (r#"build."target\u002ddir" = 'other'"#, true),
                (r#"build."build\u002ddir" = false"#, false),
                ("build.target-dir = false", false),
                ("include = []\n", true),
                // A build directory the config names is read with its definition base (the
                // working directory for an inline value, the folder above a file's): both
                // outputs are the call's, each recorded.
                ("[build]\nbuild-dir = 'other'\n", true),
                ("include = ['absent.toml']\n", false),
                ("include = ['nested/neutral.toml']\n", true),
                ("include = ['nested/unsafe.toml']\n", false),
                ("include = ['nested/cycle.toml']\n", false),
                ("include = [3]\n", false),
                ("[build", false),
                ("unreadable", false),
            ] {
                let world = World::new();
                std::fs::create_dir(world.owner.join("nested")).unwrap();
                for (name, body) in [
                    ("neutral.toml", "build.jobs = 2"),
                    ("unsafe.toml", "build.build-dir = false"),
                    ("cycle.toml", "include = ['cycle.toml']"),
                ] {
                    std::fs::write(world.owner.join("nested").join(name), body).unwrap();
                }
                let launcher = if moved {
                    world.another_owner()
                } else {
                    world.owner.clone()
                };
                let file = world.owner.join("settings.toml");
                if file_form {
                    std::fs::write(&file, text).unwrap();
                }
                let read = |path: &Path| {
                    if file_form && path == file && text == "unreadable" {
                        return Err(());
                    }
                    match std::fs::read_to_string(path) {
                        Ok(text) => Ok(Some(text)),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                        Err(_) => Err(()),
                    }
                };
                let machine = super::super::Machine {
                    home: Some(&world.home),
                    root: None,
                    cwd: Some(&launcher),
                    env: &|_| None,
                    read: &read,
                    read_manifest: &read,
                    writable: &|_| Some(true),
                };
                let mut args = vec!["--unverified".into()];
                if moved {
                    args.extend([
                        "-Zunstable-options".into(),
                        "-C".into(),
                        world.owner.display().to_string(),
                    ]);
                }
                args.extend([
                    "--config".into(),
                    if file_form {
                        "settings.toml".into()
                    } else {
                        text.into()
                    },
                    "build".into(),
                    "--target-dir".into(),
                    "fresh.noindex".into(),
                ]);
                let result = observe_for_producer(&args, Tool::Targo, &world.home, &machine, true);
                let recorded = roots(&world.home);
                if expect_owned {
                    assert!(result.unwrap(), "{text} moved={moved}");
                    let intermediates = text.contains("build-dir = 'other'").then(|| {
                        if file_form {
                            world.home.join("other")
                        } else {
                            world.owner.join("other")
                        }
                    });
                    assert_eq!(recorded.len(), 1 + usize::from(intermediates.is_some()));
                    assert!(
                        recorded
                            .iter()
                            .any(|entry| entry.root == world.owner.join("fresh.noindex"))
                    );
                    match &intermediates {
                        Some(built) => assert!(
                            built.is_dir()
                                && recorded.iter().any(|entry| built.starts_with(&entry.root)),
                            "{text} file={file_form} moved={moved}"
                        ),
                        None => assert!(
                            !world.owner.join("other").exists(),
                            "overridden config is unused"
                        ),
                    }
                    for entry in &recorded {
                        assert_eq!(
                            super::read(&entry.receipt_path).unwrap().owners,
                            [std::fs::canonicalize(&world.owner).unwrap()]
                        );
                    }
                } else {
                    assert!(result.is_err(), "{text} moved={moved}: {result:?}");
                    assert!(recorded.is_empty());
                    assert!(!world.owner.join("fresh.noindex").exists());
                }
            }
        }
    }
}

#[test]
fn cli_config_selected_targets_bind_provenance_to_receipts() {
    use super::super::Tool;
    for moved in [false, true] {
        for order in [
            "inline",
            "toml11-inline",
            "toml11-file",
            "file",
            "nested",
            "file-last",
            "inline-last",
            "neutral-last",
        ] {
            let world = World::new();
            let launcher = if moved {
                world.another_owner()
            } else {
                world.owner.clone()
            };
            std::fs::write(
                world.owner.join("settings.toml"),
                "build.target-dir='file.noindex'",
            )
            .unwrap();
            let toml11 =
                "build = {\n \"target\\x2Ddir\" = \"toml\\x31\\x31.noindex\", # target\n}\n";
            std::fs::write(world.owner.join("settings11.toml"), toml11).unwrap();
            std::fs::create_dir(world.owner.join("nested")).unwrap();
            std::fs::write(
                world.owner.join("nested/settings.toml"),
                "build.target-dir='nested.noindex'",
            )
            .unwrap();
            let configs = match order {
                "inline" => vec!["build.target-dir='inline.noindex'"],
                "toml11-inline" => vec!["build.\"target\\x2Ddir\" = \"toml\\x31\\x31.noindex\""],
                "toml11-file" => vec!["settings11.toml"],
                "file" => vec!["settings.toml"],
                "nested" => vec!["nested/settings.toml"],
                "file-last" => vec!["build.target-dir='unused.noindex'", "settings.toml"],
                "inline-last" => vec!["settings.toml", "build.target-dir='inline.noindex'"],
                _ => vec!["settings.toml", "build.jobs=2"],
            };
            let expected = match order {
                "inline" | "inline-last" => world.owner.join("inline.noindex"),
                "toml11-inline" => world.owner.join("toml11.noindex"),
                "toml11-file" => world.home.join("toml11.noindex"),
                "nested" => world.owner.join("nested.noindex"),
                _ => world.home.join("file.noindex"),
            };
            let read = |path: &Path| match std::fs::read_to_string(path) {
                Ok(text) => Ok(Some(text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(_) => Err(()),
            };
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&launcher),
                env: &|key| (key == "CARGO_BUILD_TARGET_DIR").then(|| "lower.noindex".into()),
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let mut args = vec!["--unverified".into()];
            if moved {
                args.extend([
                    "-Zunstable-options".into(),
                    "-C".into(),
                    world.owner.display().to_string(),
                ]);
            }
            args.push("build".into());
            for config in configs {
                args.extend(["--config".into(), config.into()]);
            }
            assert!(
                observe_for_producer(&args, Tool::Targo, &world.home, &machine, true).unwrap(),
                "{order} moved={moved}"
            );
            let recorded = roots(&world.home);
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0].root, expected, "{order} moved={moved}");
            assert_eq!(
                super::read(&recorded[0].receipt_path).unwrap().owners,
                [std::fs::canonicalize(&world.owner).unwrap()]
            );
            assert!(!world.owner.join("unused.noindex").exists());
            assert!(!world.owner.join("lower.noindex").exists());
        }
    }
}

#[test]
fn cli_include_targets_bind_the_winning_file_to_actual_custody() {
    use super::super::Tool;
    for moved in [false, true] {
        for file_form in [false, true] {
            for mode in ["included", "parent", "optional", "neutral"] {
                let world = World::new();
                let launcher = if moved {
                    world.another_owner()
                } else {
                    world.owner.clone()
                };
                let layers = world.owner.join("layers");
                std::fs::create_dir(&layers).unwrap();
                std::fs::write(
                    layers.join("target.toml"),
                    "build.target-dir='included.noindex'",
                )
                .unwrap();
                std::fs::write(layers.join("neutral.toml"), "build.jobs=2").unwrap();
                let text = match mode {
                    "included" => "include=['layers/neutral.toml','layers/target.toml']",
                    "parent" => "include=['layers/target.toml']\nbuild.target-dir='parent.noindex'",
                    "optional" => {
                        "include=[{path='missing.toml',optional=true},'layers/target.toml']"
                    }
                    _ => "include=['layers/neutral.toml']",
                };
                // Inline, Targo takes one dotted key per value (`toml_dotted_keys`): the parent's
                // two keys fail the call, so they are two values, the later one's target over
                // the include's.
                let inline: Vec<String> = match mode {
                    "parent" => vec![
                        "--config".into(),
                        "include=['layers/target.toml']".into(),
                        "--config".into(),
                        "build.target-dir='parent.noindex'".into(),
                    ],
                    _ => vec!["--config".into(), text.into()],
                };
                std::fs::write(world.owner.join("settings.toml"), text).unwrap();
                let read = |path: &Path| match std::fs::read_to_string(path) {
                    Ok(text) => Ok(Some(text)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(_) => Err(()),
                };
                let machine = super::super::Machine {
                    home: Some(&world.home),
                    root: None,
                    cwd: Some(&launcher),
                    env: &|_| None,
                    read: &read,
                    read_manifest: &read,
                    writable: &|_| Some(true),
                };
                let mut args = vec!["--unverified".into()];
                if moved {
                    args.extend([
                        "-Zunstable-options".into(),
                        "-C".into(),
                        world.owner.display().to_string(),
                    ]);
                }
                args.push("build".into());
                if !file_form && mode == "parent" {
                    let two_keys = [args.clone(), vec!["--config".into(), text.into()]].concat();
                    assert!(
                        observe_for_producer(&two_keys, Tool::Targo, &world.home, &machine, true)
                            .is_err(),
                        "two keys moved={moved}"
                    );
                    assert!(roots(&world.home).is_empty(), "two keys moved={moved}");
                }
                if file_form {
                    args.extend(["--config".into(), "settings.toml".into()]);
                } else {
                    args.extend(inline);
                }
                if mode == "neutral" {
                    args.extend(["--target-dir".into(), "direct.noindex".into()]);
                }
                assert!(
                    observe_for_producer(&args, Tool::Targo, &world.home, &machine, true).unwrap(),
                    "{mode} file={file_form} moved={moved}"
                );
                let recorded = roots(&world.home);
                let expected = match mode {
                    "parent" if file_form => world.home.join("parent.noindex"),
                    "parent" => world.owner.join("parent.noindex"),
                    "neutral" => world.owner.join("direct.noindex"),
                    _ => world.owner.join("included.noindex"),
                };
                assert_eq!(recorded.len(), 1);
                assert_eq!(
                    recorded[0].root, expected,
                    "{mode} file={file_form} moved={moved}"
                );
                assert_eq!(
                    super::read(&recorded[0].receipt_path).unwrap().owners,
                    [std::fs::canonicalize(&world.owner).unwrap()]
                );
                if mode == "parent" {
                    assert!(!world.owner.join("included.noindex").exists());
                }
            }
        }
    }
}

#[test]
fn configured_outputs_share_only_covering_custody_and_keep_the_actual_owner() {
    use super::super::Tool;
    for moved in [false, true] {
        for source in ["cli", "environment", "discovered"] {
            for layout in ["distinct", "equal", "target-parent", "build-parent"] {
                let world = World::new();
                let (target, build) = match layout {
                    "equal" => (world.target.clone(), world.target.clone()),
                    "target-parent" => (world.target.clone(), world.target.join("nested/build")),
                    "build-parent" => (world.target.join("nested/final"), world.target.clone()),
                    _ => (
                        world.target.clone(),
                        world.home.join("intermediate.noindex"),
                    ),
                };
                let launcher = if moved {
                    world.another_owner()
                } else {
                    world.owner.clone()
                };
                let read = |path: &Path| match std::fs::read_to_string(path) {
                    Ok(text) => Ok(Some(text)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(_) => Err(()),
                };
                let env = |name: &str| {
                    (source == "environment" && name == super::super::BUILD_DIR_ENV)
                        .then(|| build.clone().into_os_string())
                };
                let machine = super::super::Machine {
                    home: Some(&world.home),
                    root: None,
                    cwd: Some(&launcher),
                    env: &env,
                    read: &read,
                    read_manifest: &read,
                    writable: &|_| Some(true),
                };
                let mut args = vec!["--unverified".into()];
                if moved {
                    args.extend([
                        "-Zunstable-options".into(),
                        "-C".into(),
                        world.owner.display().to_string(),
                    ]);
                }
                args.extend([
                    "build".into(),
                    "--target-dir".into(),
                    target.display().to_string(),
                ]);
                if source == "cli" {
                    args.extend([
                        "--config".into(),
                        format!("build.build-dir='{}'", build.display()),
                    ]);
                }
                if source == "discovered" {
                    std::fs::create_dir_all(world.owner.join(".cargo")).unwrap();
                    std::fs::write(
                        world.owner.join(".cargo/config.toml"),
                        format!("build.build-dir='{}'", build.display()),
                    )
                    .unwrap();
                }
                for _ in 0..2 {
                    assert!(
                        observe_for_producer(&args, Tool::Targo, &world.home, &machine, true)
                            .unwrap(),
                        "{layout} source={source} moved={moved}"
                    );
                    assert!(target.is_dir() && build.is_dir());
                    let recorded = roots(&world.home);
                    assert_eq!(recorded.len(), if layout == "distinct" { 2 } else { 1 });
                    for output in [&target, &build] {
                        assert!(recorded.iter().any(|entry| output.starts_with(&entry.root)));
                    }
                    for entry in recorded {
                        let receipt = super::read(&entry.receipt_path).unwrap();
                        assert_eq!(
                            receipt.owners.as_slice(),
                            std::slice::from_ref(&world.owner)
                        );
                        assert!(!receipt.owners_unknown);
                    }
                }
            }
        }
    }
}

#[test]
fn configured_outputs_refuse_existing_unowned_roots_and_busy_gates() {
    use super::super::Tool;
    for blocked_target in [false, true] {
        let world = World::new();
        let build = world.home.join("intermediate.noindex");
        let blocked = if blocked_target {
            &world.target
        } else {
            &build
        };
        std::fs::create_dir(blocked).unwrap();
        std::fs::write(blocked.join("keep"), "caller data").unwrap();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &|p| Ok(std::fs::read_to_string(p).ok()),
            read_manifest: &|p| Ok(std::fs::read_to_string(p).ok()),
            writable: &|_| Some(true),
        };
        let args = vec![
            "--unverified".into(),
            "build".into(),
            "--target-dir".into(),
            world.target.display().to_string(),
            "--config".into(),
            format!("build.build-dir='{}'", build.display()),
        ];
        assert!(observe_for_producer(&args, Tool::Targo, &world.home, &machine, true).is_err());
        assert_eq!(
            std::fs::read_to_string(blocked.join("keep")).unwrap(),
            "caller data"
        );
        assert!(!blocked.join(MARKER).exists());
        assert!(
            roots(&world.home)
                .iter()
                .all(|entry| entry.root != *blocked)
        );
    }
    for blocked_target in [false, true] {
        let world = World::new();
        let build = world.home.join("intermediate.noindex");
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        assert!(create_checked(&world.home, &build, &world.owner).unwrap());
        let blocked = if blocked_target {
            &world.target
        } else {
            &build
        };
        let record = producer_record(&world.home, blocked).unwrap();
        let lock = open_gate(&gate_path(&record).unwrap()).unwrap();
        let held = crate::lock::Flock::lock(lock).unwrap();
        assert!(
            observe_configured_outputs(&world.home, &world.owner, &world.target, &build).is_err()
        );
        drop(held);
        assert_eq!(
            observe_configured_outputs(&world.home, &world.owner, &world.target, &build).unwrap(),
            Observation::Owned
        );
    }
}

#[test]
fn configured_nested_outputs_respect_every_existing_custody_boundary() {
    for layout in [
        "target-child",
        "build-child",
        "boundary-parent",
        "other-owner",
        "unknown-marker",
        "late-marker",
        "admission-busy",
    ] {
        let world = World::new();
        let boundary = world.target.join("nested");
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        if matches!(layout, "unknown-marker" | "late-marker") {
            std::fs::create_dir(&boundary).unwrap();
            if layout == "unknown-marker" {
                std::fs::write(boundary.join(MARKER), "unknown custody").unwrap();
            }
        } else {
            let owner = if layout == "other-owner" {
                world.another_owner()
            } else {
                world.owner.clone()
            };
            assert!(create_checked(&world.home, &boundary, &owner).unwrap());
        }
        let (target, build) = match layout {
            "build-child" => (world.target.clone(), boundary.clone()),
            "boundary-parent" => (boundary.join("deeper/final"), world.target.clone()),
            _ => (boundary.clone(), world.target.clone()),
        };
        if layout == "late-marker" {
            struct Clear;
            impl Drop for Clear {
                fn drop(&mut self) {
                    creation::faults::clear();
                }
            }
            let marked = boundary.join(MARKER);
            creation::faults::set(move |point| {
                if point == creation::Point::Parent {
                    std::fs::write(&marked, "late unknown custody").unwrap();
                }
                Some(())
            });
            let _clear = Clear;
            assert!(
                observe_configured_outputs(&world.home, &world.owner, &target, &build).is_err(),
                "{layout}"
            );
        } else if matches!(layout, "other-owner" | "unknown-marker") {
            assert!(
                observe_configured_outputs(&world.home, &world.owner, &target, &build).is_err(),
                "{layout}"
            );
        } else {
            let record = producer_record(&world.home, &boundary).unwrap();
            let gate = if layout == "admission-busy" {
                open_admission(record.parent().unwrap()).unwrap()
            } else {
                crate::metadata_io::open_regular(&gate_path(&record).unwrap()).unwrap()
            };
            let held = crate::lock::Flock::lock(gate).unwrap();
            assert!(
                observe_configured_outputs(&world.home, &world.owner, &target, &build).is_err(),
                "{layout}"
            );
            drop(held);
            assert_eq!(
                observe_configured_outputs(&world.home, &world.owner, &target, &build).unwrap(),
                Observation::Owned,
                "{layout}"
            );
        }
    }
}

/// `world`'s target built, and its quiet build directory (`<target>/.ubd/build.noindex`) once
/// registered as a root of its own and then gone from there, its ordinary record left on record
/// naming the folder that went, its owners unknown (as an unresolved producer leaves every
/// record: [`suppress_configured`]). The quiet build directory and its record's path.
///
/// The folder that went is moved out of home, not removed, and stands there as long as `world`
/// does, so no folder made at its old path later can be given its inode number back (ext4 hands
/// a freed one out again at once; APFS never does): the record would name such a folder, and so
/// be no stale record at all, on one filesystem and not another.
fn stale_quiet_record(world: &World) -> (PathBuf, PathBuf) {
    world.build();
    let build = super::super::target_build_path(&world.target);
    assert!(create_checked(&world.home, &build, &world.owner).unwrap());
    let path = producer_record(&world.home, &build).unwrap();
    let mut receipt = read(&path).unwrap();
    receipt.owners_unknown = true;
    write(&path, &receipt).unwrap();
    std::fs::rename(&build, world.home.with_file_name("gone.noindex")).unwrap();
    (build, path)
}

/// Whether the record at `path` names the folder standing at `build` now: never, for a record
/// [`stale_quiet_record`] left, whatever folder is made there.
fn names_folder_there(path: &Path, build: &Path) -> bool {
    read(path)
        .zip(identity(build))
        .is_some_and(|(receipt, now)| receipt.identity.names(&now))
}

/// A STALE RECORD OF A MANAGED TARGET'S QUIET BUILD DIRECTORY BINDS NOTHING THERE (seen on a Mac
/// 2026-11-13, every build into the target refused): the folder it named gone from there, and the
/// quiet build directory either made again empty and unmarked (as a refused preparation left it)
/// or still gone. Preparing it, directly or through the front a build takes, treats it as the
/// target's own descendant: it stands empty with no marker, still not the folder the record
/// names, no record changes and the target is the one root registered. Without the rule the
/// descendant was a custody boundary its record could never satisfy.
#[test]
fn a_stale_record_of_a_quiet_build_directory_no_longer_there_binds_nothing() {
    for shape in ["recreated", "gone", "front"] {
        let world = World::new();
        let (build, path) = stale_quiet_record(&world);
        if shape != "gone" {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&build)
                .unwrap();
            assert!(!names_folder_there(&path, &build), "{shape}");
        }
        let before = receipts(&world.home);
        if shape == "front" {
            let value = format!("build.build-dir='{}'", build.display());
            let environment = |name: &str| match name {
                "CARGO_TARGET_DIR" => Some(world.target.clone().into_os_string()),
                super::super::BUILD_DIR_ENV => Some(build.clone().into_os_string()),
                _ => None,
            };
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&world.owner),
                env: &environment,
                read: &super::super::read_config,
                read_manifest: &super::super::read_manifest,
                writable: &|_| Some(true),
            };
            let args = vec![
                "--config".into(),
                value,
                "--unverified".into(),
                "check".into(),
            ];
            assert!(
                observe_for_producer(
                    &args,
                    super::super::Tool::Targo,
                    &world.home,
                    &machine,
                    true
                )
                .unwrap()
            );
        } else {
            prepare_intermediates(&world.home, &world.target, &world.owner, &|| true).unwrap();
        }
        assert!(build.is_dir(), "{shape}");
        assert!(!names_folder_there(&path, &build), "{shape}");
        assert_eq!(std::fs::read_dir(&build).unwrap().count(), 0, "{shape}");
        assert_eq!(receipts(&world.home), before, "{shape}");
        let registered = roots(&world.home);
        assert_eq!(registered.len(), 1, "{shape}");
        assert_eq!(registered[0].root, world.target, "{shape}");
    }
}

/// WHAT STILL BINDS A QUIET BUILD DIRECTORY KEEPS IT A CUSTODY BOUNDARY: a record that names the
/// folder there but does not admit this producer (its marker gone, as an adoption interrupted
/// before its marker binds leaves it, or another workspace's), a record of the folder that went
/// that is pending, a package's, a keep or a creation, one unreadable, one of another root, and a
/// stale record beside a marker in the folder each stop the preparation, no record changed and
/// no marker added.
#[test]
fn a_quiet_build_directory_its_record_still_binds_stays_a_custody_boundary() {
    for shape in [
        "unmarked",
        "other-owner",
        "pending",
        "package",
        "retained",
        "creating",
        "unreadable",
        "other-root",
        "marked",
    ] {
        let world = World::new();
        let build = super::super::target_build_path(&world.target);
        if matches!(shape, "unmarked" | "other-owner") {
            world.build();
            let owner = if shape == "other-owner" {
                world.another_owner()
            } else {
                world.owner.clone()
            };
            assert!(create_checked(&world.home, &build, &owner).unwrap());
            if shape == "unmarked" {
                std::fs::remove_file(build.join(MARKER)).unwrap();
            }
        } else {
            let (_, path) = stale_quiet_record(&world);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&build)
                .unwrap();
            assert!(!names_folder_there(&path, &build), "{shape}");
            let mut receipt = read(&path).unwrap();
            match shape {
                "pending" => {
                    let stage = world.home.join("stage.noindex");
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&stage)
                        .unwrap();
                    receipt.pending = Some(Pending {
                        identity: identity(&stage).unwrap(),
                        directory: stage,
                        authorized: false,
                    });
                }
                "package" => {
                    let policy = format!(
                        r#"{{"declaration":{{"cache":{cache},"cache_identity":{identity},
                        "lock_identity":{identity},"minimum_bytes":1}},"learned_budget":null,
                        "active":null,"last_settled":null}}"#,
                        cache = aterm_json::to_string(&world.home).unwrap(),
                        identity = aterm_json::to_string(&receipt.identity).unwrap(),
                    );
                    receipt.package = Some(aterm_json::from_str(&policy).unwrap());
                    receipt.schema = 2;
                    receipt.owners_unknown = false;
                }
                "retained" => {
                    receipt.schema = 4;
                    receipt.retained = true;
                }
                "creating" => {
                    receipt.schema = 5;
                    receipt.creating = Some(creation::Creating {
                        directory: world.home.join("creating.noindex"),
                    });
                }
                "other-root" => receipt.root = world.home.join("elsewhere.noindex"),
                "marked" => std::fs::write(build.join(MARKER), "unknown custody").unwrap(),
                _ => {}
            }
            if shape == "unreadable" {
                std::fs::write(&path, b"{\"schema\":99}").unwrap();
            } else {
                let bytes = aterm_json::to_vec(&receipt).unwrap();
                assert_eq!(decode(&bytes), Some(receipt.clone()), "{shape}");
                write(&path, &receipt).unwrap();
            }
        }
        let marked = build.join(MARKER).exists();
        let before = receipts(&world.home);
        assert!(
            prepare_intermediates(&world.home, &world.target, &world.owner, &|| true).is_err(),
            "{shape}"
        );
        assert_eq!(receipts(&world.home), before, "{shape}");
        assert_eq!(build.join(MARKER).exists(), marked, "{shape}");
    }
}

/// A STALE RECORD THAT CHANGES WHILE A PREPARATION HOLDS IT STOPS THAT PREPARATION: rewritten once
/// the walk has read it (as the quiet build directory's first step ends), whether still of the
/// folder that went or naming the folder there now (as an adoption's receipt does before its
/// marker binds), the record stays as rewritten and no marker is added. The first shape passes
/// the walk's second read and is stopped only by the final recheck of what it read first.
#[test]
fn a_stale_quiet_record_rewritten_during_preparation_stops_it() {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            creation::faults::clear();
        }
    }
    for shape in ["rewritten", "adopted"] {
        let world = World::new();
        let (build, path) = stale_quiet_record(&world);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&build)
            .unwrap();
        assert!(!names_folder_there(&path, &build), "{shape}");
        let mut changed = read(&path).unwrap();
        changed.owners_unknown = false;
        if shape == "adopted" {
            changed.identity = identity(&build).unwrap();
        }
        let (record, rewritten) = (path.clone(), changed.clone());
        let mut steps = 0;
        creation::faults::set(move |point| {
            if point == creation::Point::Parent {
                steps += 1;
                // The quiet build directory is the walk's second component.
                if steps == 2 {
                    write(&record, &rewritten).unwrap();
                }
            }
            Some(())
        });
        let _clear = Clear;
        assert!(
            prepare_intermediates(&world.home, &world.target, &world.owner, &|| true).is_err(),
            "{shape}"
        );
        assert_eq!(read(&path), Some(changed), "{shape}");
        assert!(!build.join(MARKER).exists(), "{shape}");
    }
}

impl World {
    fn new() -> Self {
        let directory = aterm_tempfile::tempdir().unwrap();
        Self::in_directory(directory)
    }

    fn in_directory(directory: aterm_tempfile::TempDir) -> Self {
        let home = directory.path().canonicalize().unwrap().join("home");
        // Writable by its owner alone whatever the umask, as a person's home is: the index
        // binds only such folders, so a home umask 002 left group-writable registers nothing.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let owner = home.join("source");
        // Positive fixtures need private ancestors; shared-workspace cases set their modes.
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&owner)
            .unwrap();
        std::fs::write(owner.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        Self {
            _directory: directory,
            target: home.join("outputs.noindex"),
            home,
            owner,
        }
    }

    fn another_owner(&self) -> PathBuf {
        let owner = self.home.join("another-source");
        std::fs::create_dir(&owner).unwrap();
        std::fs::write(owner.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        owner
    }

    fn build(&self) {
        assert!(create_checked(&self.home, &self.target, &self.owner).unwrap());
        std::fs::create_dir_all(self.target.join("debug/deps")).unwrap();
        std::fs::write(self.target.join("debug/deps/keep"), b"reusable output").unwrap();
        std::fs::write(self.target.join("debug/.cargo-lock"), b"").unwrap();
    }

    fn path(&self) -> PathBuf {
        self.home
            .join(INDEX)
            .join(format!("{}.json", key(&self.target).unwrap()))
    }

    fn later(&self) -> SystemTime {
        SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60)
    }
}

#[test]
fn explicit_retirement_handles_a_tagless_legacy_roster_without_granting_automatic_deletion() {
    let world = World::new();
    world.build();
    let other = world.home.join("other-output.noindex");
    assert!(create_checked(&world.home, &other, &world.owner).unwrap());
    std::fs::create_dir_all(other.join("debug/deps")).unwrap();
    std::fs::write(other.join("debug/.cargo-lock"), b"").unwrap();
    std::fs::write(other.join("debug/deps/keep"), b"another cache").unwrap();
    let path = world.path();
    let mut receipt = read(&path).unwrap();
    receipt.owners.clear();
    write(&path, &receipt).unwrap();
    let tag = world.target.join("CACHEDIR.TAG");
    if tag.exists() {
        std::fs::remove_file(&tag).unwrap();
    }
    let processes = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    let fence = super::super::fence::Fence::new(&processes);
    assert!(
        reclaim_on(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
            &fence
        )
        .is_empty()
    );
    assert!(world.target.join("debug/deps/keep").is_file());
    let removed = retire_on(&world.home, &world.target, &mut |_, _| true, &fence).unwrap();
    assert!(
        matches!(removed.as_slice(), [(root, super::super::Unused::Requested)] if root == &world.target)
    );
    assert!(!world.target.exists());
    assert_eq!(
        std::fs::read(other.join("debug/deps/keep")).unwrap(),
        b"another cache"
    );
    assert!(world.owner.join("Cargo.toml").is_file());
}

#[test]
fn explicit_retirement_refuses_unregistered_retained_and_unknown_custody() {
    for kind in ["unregistered", "retained", "unknown"] {
        let world = World::new();
        if kind == "unregistered" {
            std::fs::create_dir(&world.target).unwrap();
            std::fs::write(world.target.join("keep"), b"unclaimed data").unwrap();
        } else {
            world.build();
            if kind == "retained" {
                retain(&world.home, &world.target).unwrap();
            } else {
                let path = world.path();
                let mut receipt = read(&path).unwrap();
                receipt.owners_unknown = true;
                write(&path, &receipt).unwrap();
            }
        }
        let processes = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
        assert!(
            retire_on(
                &world.home,
                &world.target,
                &mut |_, _| true,
                &super::super::fence::Fence::new(&processes)
            )
            .is_err(),
            "{kind}"
        );
        assert!(world.target.is_dir(), "{kind}");
    }
}

#[test]
fn explicit_retirement_reports_the_guard_that_keeps_an_unbuilt_cache() {
    let world = World::new();
    assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
    let marker = std::fs::read(world.target.join(MARKER)).unwrap();
    let receipt = std::fs::read(world.path()).unwrap();
    let original = identity(&world.target).unwrap();
    let reason = super::super::KeepReason::NoCargoLocks;
    let error = retire(&world.home, &world.target, &mut |_, _| {
        panic!("an unbuilt cache grants no removal intent")
    })
    .unwrap_err();
    assert!(error.contains(&reason.to_string()), "{error}");
    assert!(
        error.contains(&world.target.display().to_string()),
        "{error}"
    );
    assert_eq!(identity(&world.target).as_ref(), Some(&original));
    assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
    assert_eq!(std::fs::read(world.path()).unwrap(), receipt);

    world.build();
    let removed = retire(&world.home, &world.target, &mut |_, _| true).unwrap();
    assert!(matches!(
        removed.as_slice(),
        [(root, super::super::Unused::Requested)] if root == &world.target
    ));
    assert!(!world.target.exists());
}

#[test]
fn explicit_retirement_uses_only_this_attempts_observations() {
    let world = World::new();
    assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
    let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    let reporting = super::super::fence::Fence::new(&clear).with_reporting();
    let previous = super::super::KeepReason::ProcessUnavailable;
    reporting.note(&world.target, previous.clone());
    let unbuilt = retire_on(
        &world.home,
        &world.target,
        &mut |_, _| panic!("an unbuilt cache grants no removal intent"),
        &reporting,
    )
    .unwrap_err();
    let missing_locks = super::super::KeepReason::NoCargoLocks;
    assert!(unbuilt.contains(&missing_locks.to_string()), "{unbuilt}");
    assert!(!unbuilt.contains(&previous.to_string()), "{unbuilt}");
    assert!(reporting.retained().contains(&super::super::RetainedCheck {
        path: world.target.clone(),
        reason: missing_locks.clone(),
    }));
    let repeated = retire_on(
        &world.home,
        &world.target,
        &mut |_, _| panic!("an unbuilt cache still grants no removal intent"),
        &reporting,
    )
    .unwrap_err();
    assert_eq!(unbuilt, repeated);

    world.build();
    let original = identity(&world.target).unwrap();
    let marker = std::fs::read(world.target.join(MARKER)).unwrap();
    let receipt = std::fs::read(world.path()).unwrap();
    let mut asked = 0;
    let mut decline = |_: &Path, _: super::super::Unused| {
        asked += 1;
        false
    };
    let reused_error = retire_on(&world.home, &world.target, &mut decline, &reporting).unwrap_err();
    let fresh_error = retire_on(
        &world.home,
        &world.target,
        &mut decline,
        &super::super::fence::Fence::new(&clear),
    )
    .unwrap_err();
    assert_eq!(asked, 2);
    assert_eq!(reused_error, fresh_error);
    assert!(!reused_error.is_empty());
    assert!(
        !reused_error.contains(&previous.to_string()),
        "{reused_error}"
    );
    assert!(
        !reused_error.contains(&missing_locks.to_string()),
        "{reused_error}"
    );
    assert_eq!(identity(&world.target).as_ref(), Some(&original));
    assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
    assert_eq!(std::fs::read(world.path()).unwrap(), receipt);
    assert_eq!(
        std::fs::read(world.target.join("debug/deps/keep")).unwrap(),
        b"reusable output"
    );
    let removed = retire_on(&world.home, &world.target, &mut |_, _| true, &reporting).unwrap();
    assert!(matches!(
        removed.as_slice(),
        [(root, super::super::Unused::Requested)] if root == &world.target
    ));
    assert!(!world.target.exists());
}

#[test]
fn explicit_retirement_keeps_cargo_locks_and_unknown_or_live_processes_authoritative() {
    let world = World::new();
    world.build();
    let deep = world
        .target
        .join("verify/.ubd/cells.noindex/linux/triple/debug");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join(".cargo-build-lock"), b"").unwrap();
    let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    let held =
        crate::lock::Flock::try_lock(std::fs::File::open(deep.join(".cargo-build-lock")).unwrap())
            .unwrap();
    let reporting = super::super::fence::Fence::new(&clear).with_reporting();
    let error = retire_on(
        &world.home,
        &world.target,
        &mut |_, _| panic!("a held Cargo lock grants no removal intent"),
        &reporting,
    )
    .unwrap_err();
    let reason = super::super::KeepReason::CargoLocks;
    assert!(error.contains(&reason.to_string()), "{error}");
    assert!(reporting.retained().contains(&super::super::RetainedCheck {
        path: world.target.clone(),
        reason,
    }));
    drop(held);
    for unknown in [false, true] {
        let processes = |_: &[PathBuf], _: &[u32]| {
            if unknown {
                return Err(std::io::Error::other("process census unavailable"));
            }
            Ok(aterm_uds::holders::PathScan {
                users: vec![aterm_uds::holders::PathUse {
                    pid: 42,
                    how: "exe",
                    path: world.target.join("debug/deps/keep"),
                }],
                ..Default::default()
            })
        };
        let reporting = super::super::fence::Fence::new(&processes).with_reporting();
        let error = retire_on(
            &world.home,
            &world.target,
            &mut |_, _| panic!("an uncertain process scan grants no removal intent"),
            &reporting,
        )
        .unwrap_err();
        let reason = if unknown {
            super::super::KeepReason::ProcessUnavailable
        } else {
            super::super::KeepReason::ProcessUsed {
                pid: 42,
                how: "executable",
                observed: world.target.join("debug/deps/keep"),
            }
        };
        assert!(error.contains(&reason.to_string()), "{error}");
        assert!(reporting.retained().contains(&super::super::RetainedCheck {
            path: world.target.clone(),
            reason,
        }));
        assert_eq!(
            std::fs::read(world.target.join("debug/deps/keep")).unwrap(),
            b"reusable output"
        );
    }
    let removed = retire_on(
        &world.home,
        &world.target,
        &mut |_, _| true,
        &super::super::fence::Fence::new(&clear),
    )
    .unwrap();
    assert!(
        matches!(removed.as_slice(), [(root, super::super::Unused::Requested)] if root == &world.target)
    );
    assert!(
        !world.target.exists(),
        "the same deep inventory is reclaimable once clear"
    );
}

#[test]
fn explicit_retirement_keeps_a_late_holder_staged_and_resumes_only_the_identified_payload() {
    let world = World::new();
    world.build();
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        let moved = dirs.iter().any(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("atpkg-cache-retiring-"))
                && p.is_dir()
        });
        Ok(aterm_uds::holders::PathScan {
            users: moved
                .then(|| aterm_uds::holders::PathUse {
                    pid: 42,
                    how: "exe",
                    path: world.target.join("debug/deps/keep"),
                })
                .into_iter()
                .collect(),
            ..Default::default()
        })
    };
    let _ = retire_on(
        &world.home,
        &world.target,
        &mut |_, _| true,
        &super::super::fence::Fence::new(&processes),
    );
    let pending = read(&world.path()).unwrap().pending.unwrap();
    assert_eq!(
        std::fs::read(pending.directory.join(PAYLOAD).join("debug/deps/keep")).unwrap(),
        b"reusable output"
    );
    let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    let held = crate::lock::Flock::try_lock(
        std::fs::File::open(pending.directory.join(PAYLOAD).join("debug/.cargo-lock")).unwrap(),
    )
    .unwrap();
    let reporting = super::super::fence::Fence::new(&clear).with_reporting();
    assert!(retire_on(&world.home, &world.target, &mut |_, _| true, &reporting).is_err());
    assert!(
        reporting.retained().contains(&super::super::RetainedCheck {
            path: pending.directory.clone(),
            reason: super::super::KeepReason::CargoLocks,
        }),
        "a resumed-stage lock refusal must reach the cleanup diagnostic"
    );
    assert!(
        pending
            .directory
            .join(PAYLOAD)
            .join("debug/deps/keep")
            .is_file()
    );
    drop(held);
    let removed = retire_on(
        &world.home,
        &world.target,
        &mut |_, _| true,
        &super::super::fence::Fence::new(&clear),
    )
    .unwrap();
    assert!(matches!(
        removed.as_slice(),
        [(_, super::super::Unused::Unfinished)]
    ));
    assert!(!pending.directory.exists());
}

#[test]
fn explicit_retirement_does_not_take_a_replacement_published_after_its_intent() {
    let world = World::new();
    world.build();
    let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    let old = world.home.join("preserved.noindex");
    let mut record = |_: &Path, _: super::super::Unused| {
        std::fs::rename(&world.target, &old).unwrap();
        std::fs::create_dir(&world.target).unwrap();
        std::fs::write(world.target.join("keep"), b"replacement data").unwrap();
        true
    };
    let outcomes = retire_on(
        &world.home,
        &world.target,
        &mut record,
        &super::super::fence::Fence::new(&clear),
    )
    .unwrap();
    assert!(matches!(
        outcomes.as_slice(),
        [(_, super::super::Unused::Refused { .. })]
    ));
    assert_eq!(
        std::fs::read(world.target.join("keep")).unwrap(),
        b"replacement data"
    );
    assert_eq!(
        std::fs::read(old.join("debug/deps/keep")).unwrap(),
        b"reusable output"
    );
}

#[test]
fn explicit_retirement_rechecks_its_captured_snapshot_against_the_locked_fresh_receipt() {
    for replaced in [false, true] {
        let world = World::new();
        world.build();
        let wanted = roots(&world.home).pop().unwrap();
        let saved = world.home.join("saved-output.noindex");
        if replaced {
            std::fs::rename(&world.target, &saved).unwrap();
            world.build();
        } else {
            let other = world.another_owner();
            assert!(create_checked(&world.home, &world.target, &other).unwrap());
        }
        let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
        let removed = reclaim_selected_on(
            &world.home,
            SystemTime::now(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
            &super::super::fence::Fence::new(&clear),
            Selection::Requested(&wanted),
        );
        assert!(removed.is_empty(), "replaced={replaced}");
        assert_eq!(
            std::fs::read(world.target.join("debug/deps/keep")).unwrap(),
            b"reusable output"
        );
        if replaced {
            assert_eq!(
                std::fs::read(saved.join("debug/deps/keep")).unwrap(),
                b"reusable output"
            );
        }
    }
}

#[test]
fn ordinary_clearance_sets_aside_only_abandoned_private_receipt_stages() {
    let world = World::new();
    world.build();
    let index = world.home.join(INDEX);
    let unrelated = world.home.join("unrelated.noindex");
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .args([
            "-c",
            r#"set -eu
stage="$1/cache-receipt-$$_0000000000000000_0"
/bin/mkdir -m 700 "$stage"
printf '{partial' > "$stage/receipt"
chmod 600 "$stage/receipt"
printf 'ready\n'
read token
"#,
            "receipt-writer",
        ])
        .arg(&index)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .unwrap();
    aterm_uds::deadline::prints(&mut child, "ready", Duration::from_secs(60));
    let stage = index.join(format!("cache-receipt-{}_0000000000000000_0", child.id()));
    let live_refused = ordinary_clearance(&world.home, &unrelated).is_err();
    aterm_uds::deadline::kill_within(&mut child, aterm_uds::deadline::ECHO_HANG);
    let reaped = child.try_wait();
    if !matches!(&reaped, Ok(Some(_))) {
        let kept = world._directory.keep();
        panic!(
            "receipt writer did not reap after SIGKILL: {reaped:?}; kept {}",
            kept.display()
        );
    }
    assert!(live_refused);
    assert!(
        ordinary_clearance(&world.home, &unrelated)
            .unwrap()
            .current()
    );
    assert!(ordinary_clearance(&world.home, &world.target).is_err());
    assert_eq!(std::fs::read(stage.join("receipt")).unwrap(), b"{partial");

    std::fs::write(stage.join("receipt"), vec![0; MAX_RECORD + 1]).unwrap();
    assert!(ordinary_clearance(&world.home, &unrelated).is_err());
    std::fs::remove_file(stage.join("receipt")).unwrap();
    std::fs::hard_link(world.path(), stage.join("receipt")).unwrap();
    assert!(ordinary_clearance(&world.home, &unrelated).is_err());
    std::fs::remove_file(stage.join("receipt")).unwrap();
    assert!(ordinary_clearance(&world.home, &unrelated).is_ok());
    std::fs::write(stage.join("foreign"), b"keep").unwrap();
    assert!(ordinary_clearance(&world.home, &unrelated).is_err());
    std::fs::remove_file(stage.join("foreign")).unwrap();
    std::os::unix::fs::symlink(world.path(), stage.join("receipt")).unwrap();
    assert!(ordinary_clearance(&world.home, &unrelated).is_err());
    std::fs::remove_file(stage.join("receipt")).unwrap();
    std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(ordinary_clearance(&world.home, &unrelated).is_err());
    std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in [
        "cache-receipt-0_0000000000000000_0".to_owned(),
        format!("cache-receipt-0{}_0000000000000000_0", child.id()),
        format!("cache-receipt-{}_A000000000000000_0", child.id()),
        format!("cache-receipt-{}_0000000000000000_00", child.id()),
        format!("cache-receipt-{}_0000000000000000_0_extra", child.id()),
    ] {
        let moved = index.join(name);
        std::fs::rename(&stage, &moved).unwrap();
        assert!(ordinary_clearance(&world.home, &unrelated).is_err());
        std::fs::rename(&moved, &stage).unwrap();
    }
    let outside = world.home.join("saved-stage");
    std::fs::rename(&stage, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &stage).unwrap();
    assert!(ordinary_clearance(&world.home, &unrelated).is_err());
    assert!(outside.is_dir());
}

#[test]
fn ordinary_clearance_includes_partial_stages_and_refuses_unknown_index_state() {
    let world = World::new();
    world.build();
    let stage = world.home.join("retiring.noindex");
    stage_for_process_test(&world.home, &world.target, &stage);
    let payload = stage.join("cache.noindex");
    std::fs::remove_file(payload.join(MARKER)).unwrap();
    for held in [
        &stage,
        &payload,
        &payload.join("debug/deps/keep"),
        &world.target,
    ] {
        assert!(ordinary_clearance(&world.home, held).is_err());
    }
    let unrelated = world.home.join("unrelated.noindex");
    let clearance = ordinary_clearance(&world.home, &unrelated).unwrap();
    assert!(clearance.current());
    let index = world.home.join(INDEX);
    std::fs::rename(&index, world.home.join("former-index")).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&index)
        .unwrap();
    assert!(!clearance.current());
    std::fs::write(index.join("unknown.json"), br#"{"schema":999}"#).unwrap();
    assert!(ordinary_clearance(&world.home, &unrelated).is_err());
}

/// SYMLINKED OR ALIASED ANCESTORS CANNOT BYPASS `ordinary_clearance`: resolving the deepest
/// existing ancestor of `path` catches a symlinked parent directory, a symlink naming the
/// registered target, a non-existent child reached through that symlink, and a dangling
/// symlink. NEGATIVE CONTROL: a real sibling directory outside the registered root still clears.
#[test]
fn ordinary_clearance_refuses_symlinked_ancestors_and_aliases_into_registered_custody() {
    let world = World::new();
    world.build();
    let real_sibling = world.home.join("real-sibling.noindex");
    assert!(ordinary_clearance(&world.home, &real_sibling).is_ok());

    let parent_link = world.home.join("alias-home");
    std::os::unix::fs::symlink(&world.home, &parent_link).unwrap();
    let target_name = world.target.file_name().unwrap();
    let aliased_target = parent_link.join(target_name);
    let aliased_child = aliased_target.join("debug/deps/future-unit");
    assert!(
        ordinary_clearance(&world.home, &aliased_target).is_err(),
        "registered target named through a symlinked ancestor must be refused"
    );
    assert!(
        ordinary_clearance(&world.home, &aliased_child).is_err(),
        "non-existent descendant reached through a symlinked ancestor must be refused"
    );
    assert!(
        ordinary_clearance(&world.home, &parent_link).is_err(),
        "symlinked directory whose target contains a registered root must be refused"
    );

    let dangling = world.home.join("dangling.noindex");
    std::os::unix::fs::symlink(world.home.join("missing-target"), &dangling).unwrap();
    assert!(
        ordinary_clearance(&world.home, &dangling).is_err(),
        "dangling symlink cannot receive ordinary clearance"
    );
}

#[test]
fn an_unstarted_retirement_recovers_only_its_unchanged_empty_or_missing_stage() {
    #[derive(Clone, Copy, Debug)]
    enum Stage {
        Empty,
        Missing,
        Nonempty,
        HeldEmpty,
        OriginalInUse,
        Replaced,
        ChangedReceipt,
        ChangedRoot,
    }
    for state in [
        Stage::Empty,
        Stage::Missing,
        Stage::Nonempty,
        Stage::HeldEmpty,
        Stage::OriginalInUse,
        Stage::Replaced,
        Stage::ChangedReceipt,
        Stage::ChangedRoot,
    ] {
        let world = World::new();
        world.build();
        let path = world.path();
        let lock = crate::metadata_io::open_regular(&gate_path(&path).unwrap()).unwrap();
        let _held = crate::lock::Flock::try_lock(lock).unwrap();
        let mut receipt = read(&path).unwrap();
        let stage = aterm_tempfile::Builder::new()
            .prefix("atpkg-cache-retiring-")
            .tempdir_in(&world.home)
            .unwrap()
            .keep();
        receipt.pending = Some(Pending {
            directory: stage.clone(),
            identity: identity(&stage).unwrap(),
            authorized: false,
        });
        write(&path, &receipt).unwrap();
        match state {
            Stage::Empty | Stage::HeldEmpty | Stage::OriginalInUse => {}
            Stage::Missing => std::fs::remove_dir(&stage).unwrap(),
            Stage::Nonempty => std::fs::write(stage.join("keep"), b"owner data").unwrap(),
            Stage::Replaced => {
                std::fs::rename(&stage, stage.with_extension("original")).unwrap();
                std::fs::create_dir(&stage).unwrap();
            }
            Stage::ChangedReceipt => {
                let mut changed = receipt.clone();
                changed.owners_unknown = true;
                write(&path, &changed).unwrap();
            }
            Stage::ChangedRoot => {
                std::fs::rename(&world.target, world.home.join("original.noindex")).unwrap();
                std::fs::create_dir(&world.target).unwrap();
            }
        }
        let before = std::fs::read(&path).unwrap();
        let recover = matches!(state, Stage::Empty | Stage::Missing | Stage::OriginalInUse);
        let processes = |dirs: &[PathBuf], _: &[u32]| {
            assert!(dirs.contains(&stage));
            let reported = match state {
                Stage::HeldEmpty => Some(("cwd", stage.clone())),
                Stage::OriginalInUse => Some(("exe", world.target.join("debug/deps/keep"))),
                _ => None,
            };
            Ok(aterm_uds::holders::PathScan {
                users: reported
                    .filter(|(_, path)| dirs.iter().any(|dir| path.starts_with(dir)))
                    .map(|(how, path)| aterm_uds::holders::PathUse { pid: 34, how, path })
                    .into_iter()
                    .collect(),
                ..Default::default()
            })
        };
        assert_eq!(
            recover_unstarted(
                &world.home,
                &path,
                &mut receipt,
                &super::super::fence::Fence::new(&processes),
                None,
            ),
            recover,
            "{state:?}"
        );
        if recover {
            assert!(receipt.pending.is_none() && !stage.exists(), "{state:?}");
            assert!(roots(&world.home)[0].is_current(&world.home), "{state:?}");
        } else {
            assert_eq!(std::fs::read(&path).unwrap(), before, "{state:?}");
            assert!(receipt.pending.is_some() && stage.is_dir(), "{state:?}");
            if matches!(state, Stage::Nonempty) {
                assert_eq!(std::fs::read(stage.join("keep")).unwrap(), b"owner data");
            }
        }
        let original = if matches!(state, Stage::ChangedRoot) {
            world.home.join("original.noindex")
        } else {
            world.target.clone()
        };
        assert_eq!(
            std::fs::read(original.join("debug/deps/keep")).unwrap(),
            b"reusable output"
        );
    }
}

#[test]
fn fresh_creation_preserves_output_locations_and_never_adopts_a_folder_cargo_did_not_make() {
    let world = World::new();
    world.build();
    assert!(world.target.join("debug/deps/keep").is_file());
    assert!(!world.target.join(".ubd/build.noindex").exists());
    let unknown = world.home.join("existing.noindex");
    std::fs::create_dir(&unknown).unwrap();
    std::fs::write(unknown.join("keep"), b"existing data").unwrap();
    assert!(!create_checked(&world.home, &unknown, &world.owner).unwrap());
    assert_eq!(
        std::fs::read(unknown.join("keep")).unwrap(),
        b"existing data"
    );
    assert!(!unknown.join(MARKER).exists());
    assert_eq!(roots(&world.home).len(), 1);
}

#[test]
fn package_registers_its_fresh_target_without_adopting_existing_output() {
    for no_verify in [false, true] {
        let world = World::new();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &|path| {
                if path.starts_with(&world.home) {
                    super::super::read_config(path)
                } else {
                    Ok(None)
                }
            },
            read_manifest: &|path| {
                if path.starts_with(&world.home) {
                    super::super::read_manifest(path)
                } else {
                    Ok(None)
                }
            },
            writable: &|_| Some(true),
        };
        let mut args = vec![
            "--unverified".into(),
            "package".into(),
            format!("--target-dir={}", world.target.display()),
        ];
        if no_verify {
            args.push("--no-verify".into());
        }
        assert!(
            observe_for_producer(
                &args,
                super::super::Tool::Targo,
                &world.home,
                &machine,
                true
            )
            .unwrap()
        );
        let receipt = read(&world.path()).unwrap();
        assert_eq!(receipt.root, world.target);
        assert_eq!(receipt.owners, vec![world.owner.clone()]);
        assert!(world.target.join(MARKER).is_file());

        let existing = world.home.join("unmanaged-package.noindex");
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&existing)
            .unwrap();
        std::fs::write(existing.join("keep"), b"owner artifact").unwrap();
        args[2] = format!("--target-dir={}", existing.display());
        assert!(
            observe_for_producer(
                &args,
                super::super::Tool::Targo,
                &world.home,
                &machine,
                true
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(existing.join("keep")).unwrap(),
            b"owner artifact"
        );
        assert!(!existing.join(MARKER).exists());
        let child = super::super::target_build_path(&existing);
        assert!(child.join(MARKER).is_file());
        assert_eq!(roots(&world.home).len(), 2);
    }
}

#[test]
fn observations_distinguish_unmanaged_outputs_from_registered_roots() {
    let world = World::new();
    world.build();
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| Some(true),
    };
    let args = |manifest: &Path, target: &Path| {
        vec![
            "--unverified".into(),
            "build".into(),
            format!("--manifest-path={}", manifest.display()),
            format!("--target-dir={}", target.display()),
        ]
    };
    let original = std::fs::read(world.path()).unwrap();
    let other = world.home.join("other-managed.noindex");
    assert!(create_checked(&world.home, &other, &world.owner).unwrap());
    let other_receipt = world
        .home
        .join(INDEX)
        .join(format!("{}.json", key(&other).unwrap()));
    std::fs::copy(other_receipt, world.path()).unwrap();
    assert!(
        observe_on(
            &[
                "--unverified".into(),
                "unknown-alias".into(),
                format!("--target-dir={}", world.target.display())
            ],
            super::super::Tool::Targo,
            &world.home,
            &machine,
        )
        .is_err(),
        "another root's receipt cannot acknowledge the requested target"
    );
    std::fs::write(world.path(), original).unwrap();
    assert!(
        observe_on(
            &args(&world.owner.join("Cargo.toml"), &world.target),
            super::super::Tool::Targo,
            &world.home,
            &machine,
        )
        .unwrap()
    );
    let legacy = world.home.join("legacy.noindex");
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&legacy)
        .unwrap();
    assert!(
        observe_on(
            &args(&world.owner.join("Cargo.toml"), &legacy),
            super::super::Tool::Targo,
            &world.home,
            &machine,
        )
        .unwrap()
    );
    assert!(!legacy.join(MARKER).exists());
    assert!(
        super::super::target_build_path(&legacy)
            .join(MARKER)
            .is_file()
    );
    assert!(
        observe_for_producer(
            &args(&world.owner.join("Cargo.toml"), &legacy),
            super::super::Tool::Targo,
            &world.home,
            &machine,
            true,
        )
        .is_err(),
        "intermediate custody cannot acknowledge the unmanaged final target"
    );
    assert!(!legacy.join(MARKER).exists());

    // A target can be managed under one HOME, yet out of scope beneath a nested HOME's
    // protected roots. Exit zero for an unmanaged observation must not acknowledge it.
    let nested = world.home.join("nested-home");
    let protected = nested.join("Library/Containers/cache.noindex");
    // Private whatever the umask: a target is laid only through such folders.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(protected.parent().unwrap())
        .unwrap();
    assert!(create_checked(&world.home, &protected, &world.owner).unwrap());
    let project = nested.join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let nested_machine = super::super::Machine {
        home: Some(&nested),
        cwd: Some(&project),
        ..machine
    };
    assert!(
        !observe_on(
            &args(&project.join("Cargo.toml"), &protected),
            super::super::Tool::Targo,
            &nested,
            &nested_machine,
        )
        .unwrap()
    );
    assert!(already_recorded(&world.home, &protected, &world.owner));
}

/// THE INDEX'S FOLDERS ARE LAID PRIVATE BEFORE A SHARED-CACHE BUILD CAN MAKE THEM: a build
/// into the shared cache makes each folder of its path it does not find with its umask, and
/// under umask 002 that left `Library/Caches/aterm` and its `.ubd` writable by the group, so
/// no explicit target could be registered after it. The shim's call is observed before its
/// build runs: for a plain build in a home whose `Library/Caches` (private, as macOS makes
/// it) holds no `aterm`, the folders the index shares with the cache are made at 0o700, the
/// folders the build then makes (here as umask 002 makes them) stand below them, and an
/// explicit target is registered with its marker. NEGATIVE CONTROL: with the build's folders
/// made as umask 002 makes them before any call is observed, no target is registered.
#[test]
fn the_index_folders_are_laid_private_before_a_shared_cache_build() {
    let caches = |home: &Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(home.join("Library/Caches"))
            .unwrap();
    };
    let build_as_umask_002 = |home: &Path| {
        let mut at = home.to_path_buf();
        for component in Path::new(super::super::CACHE_UNDER_HOME).components() {
            at.push(component);
            if !at.exists() {
                std::fs::create_dir(&at).unwrap();
                std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o775)).unwrap();
            }
        }
    };
    let control = World::new();
    caches(&control.home);
    build_as_umask_002(&control.home);
    assert!(!create_checked(&control.home, &control.target, &control.owner).unwrap());
    assert!(!control.target.join(MARKER).exists());

    let world = World::new();
    caches(&world.home);
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| Some(true),
    };
    let plain = ["--unverified".to_owned(), "build".to_owned()];
    observe_on(&plain, super::super::Tool::Targo, &world.home, &machine).unwrap();
    for laid in ["Library/Caches/aterm", "Library/Caches/aterm/.ubd"] {
        let mode = std::fs::symlink_metadata(world.home.join(laid))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o700, "{laid}");
    }
    build_as_umask_002(&world.home);
    assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
    assert!(world.target.join(MARKER).is_file());
}

/// A HOME A BUILD ALREADY LEFT GROUP-WRITABLE IS MADE PRIVATE BY THE NEXT OBSERVED CALL: a build
/// before the fix above made `Library/Caches/aterm` and its `.ubd` at 0o775 (umask 002), so
/// the index could never be laid there and no explicit target registered on any later call.
/// The next observed shim call takes write from the group and others on each of those
/// folders that is this user's own (through the folder held, never through a link), and
/// leaves every other bit, and `Library/Caches` itself (the person's folder, not aterm's), as
/// it stands: an explicit target is registered with its marker. A link standing at
/// `Library/Caches/aterm` is never followed: the folder it names keeps its mode. NEGATIVE
/// CONTROL: before the call is observed, the same home registers no target.
#[test]
fn a_home_a_build_left_group_writable_is_made_private_by_the_next_observed_call() {
    let lay = |caches: u32| {
        let world = World::new();
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(world.home.join("Library/Caches"))
            .unwrap();
        let mut at = world.home.clone();
        for component in Path::new(super::super::CACHE_UNDER_HOME).components() {
            at.push(component);
            if !at.exists() {
                std::fs::create_dir(&at).unwrap();
                std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o775)).unwrap();
            }
        }
        let library_caches = world.home.join("Library/Caches");
        std::fs::set_permissions(&library_caches, std::fs::Permissions::from_mode(caches)).unwrap();
        world
    };
    let mode_of = |at: &Path| std::fs::symlink_metadata(at).unwrap().permissions().mode() & 0o7777;
    let observe = |world: &World| {
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &|_| Ok(None),
            read_manifest: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let plain = ["--unverified".to_owned(), "build".to_owned()];
        observe_on(&plain, super::super::Tool::Targo, &world.home, &machine).unwrap();
    };

    let world = lay(0o700);
    assert!(!create_checked(&world.home, &world.target, &world.owner).unwrap());
    assert!(!world.target.join(MARKER).exists());
    observe(&world);
    for laid in ["Library/Caches/aterm", "Library/Caches/aterm/.ubd"] {
        assert_eq!(mode_of(&world.home.join(laid)), 0o755, "{laid}");
    }
    assert_eq!(
        mode_of(
            &world
                .home
                .join("Library/Caches/aterm/.ubd/cargo-build.noindex")
        ),
        0o775
    );
    assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
    assert!(world.target.join(MARKER).is_file());

    let shared = lay(0o770);
    observe(&shared);
    assert_eq!(mode_of(&shared.home.join("Library/Caches")), 0o770);
    assert_eq!(mode_of(&shared.home.join("Library/Caches/aterm")), 0o755);

    let linked = lay(0o700);
    let aterm = linked.home.join("Library/Caches/aterm");
    let elsewhere = linked.home.join("elsewhere");
    std::fs::rename(&aterm, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &aterm).unwrap();
    observe(&linked);
    assert_eq!(mode_of(&elsewhere), 0o775);
    assert_eq!(mode_of(&elsewhere.join(".ubd")), 0o775);
}

#[test]
fn equivalent_inherited_placement_preserves_owners_through_nested_and_rust_calls() {
    let world = World::new();
    world.build();
    let manifest = world.owner.join("Cargo.toml");
    let manifest_text = std::fs::read_to_string(&manifest).unwrap();
    let read_manifest =
        |path: &Path| Ok((path == manifest.as_path()).then(|| manifest_text.clone()));
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &read_manifest,
        read_manifest: &read_manifest,
        writable: &|_| Some(true),
    };
    let typed = vec!["--unverified".into(), "tippy".into()];
    let cache = super::super::config_value(&super::super::cache_root(&world.home)).unwrap();
    let directory = super::super::directory_from_config(&cache).unwrap();
    let before = std::fs::read(world.path()).unwrap();
    // In either spelling Targo reads ([`super::super::cargo_var`]).
    for var in [super::super::BUILD_DIR_ENV, super::super::BUILD_DIR_TWIN] {
        let environment = |name: &str| (name == var).then(|| directory.into());
        let inherited = super::super::Machine {
            env: &environment,
            ..machine
        };
        for args in [
            typed.clone(),
            [vec!["--config".into(), cache.clone()], typed.clone()].concat(),
        ] {
            assert!(
                !observe_on(&args, super::super::Tool::Targo, &world.home, &inherited).unwrap()
            );
            assert_eq!(
                std::fs::read(world.path()).unwrap(),
                before,
                "equivalent default placement in {var} must not poison any existing receipt"
            );
        }
    }
    let target = world.home.join("ordinary-target");
    // Private whatever the umask: a build folder is laid only through such folders.
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&target)
        .unwrap();
    std::fs::write(target.join("keep"), b"unmanaged target output").unwrap();
    let child = super::super::target_build_path(&target);
    let typed = vec![
        "--unverified".into(),
        "build".into(),
        format!("--target-dir={}", target.display()),
    ];
    assert_eq!(
        super::super::selected_outputs(&typed, super::super::Tool::Targo, &machine, &|_| {
            super::super::UserAlias::None
        })
        .expect("the fixture exposes its explicit target and owning manifest")
        .manifests,
        vec![manifest.clone()]
    );
    let value = format!("build.build-dir='{}'", child.display());
    let environment =
        |name: &str| (name == super::super::BUILD_DIR_ENV).then(|| child.clone().into_os_string());
    let inherited = super::super::Machine {
        env: &environment,
        ..machine
    };
    for args in [
        typed.clone(),
        [vec!["--config".into(), value], typed].concat(),
    ] {
        assert!(observe_on(&args, super::super::Tool::Targo, &world.home, &inherited).unwrap());
        assert_eq!(
            std::fs::read(world.path()).unwrap(),
            before,
            "a resolved target must not poison unrelated receipts"
        );
    }
    let receipt = world
        .home
        .join(INDEX)
        .join(format!("{}.json", key(&child).unwrap()));
    assert_eq!(
        read(&receipt).unwrap().owners.as_slice(),
        std::slice::from_ref(&world.owner)
    );
    assert!(!read(&receipt).unwrap().owners_unknown);
    assert!(
        !target.join(MARKER).exists(),
        "the unmarked parent was never adopted"
    );
    assert_eq!(
        std::fs::read(target.join("keep")).unwrap(),
        b"unmanaged target output"
    );
    let mut unknown = read(&receipt).unwrap();
    unknown.owners_unknown = true;
    write(&receipt, &unknown).unwrap();
    observe_on(
        &[
            "--unverified".into(),
            "build".into(),
            format!("--target-dir={}", target.display()),
        ],
        super::super::Tool::Targo,
        &world.home,
        &inherited,
    )
    .unwrap();
    assert!(
        read(&receipt).unwrap().owners_unknown,
        "equivalence never repairs old suppression"
    );
}

/// The workspace's own `target`, which a local fallback writes, keeps what a cargo home's
/// cache keeps at its top only by chance (an xtask's `target/cache` downloads, a tool's
/// `target/db` file, a link named `index`): no cargo home's cache is a workspace root's
/// `target`, so its leaf is owned and the build runs, as it did before the quiet branch read
/// a cache by its contents.
#[test]
fn quiet_local_fallback_owns_only_its_child_in_an_existing_unmanaged_target() {
    for (trust, beside) in [false, true]
        .into_iter()
        .flat_map(|trust| ["none", "cache-folder", "db-file", "index-link"].map(|b| (trust, b)))
    {
        let world = World::new();
        let target = world.owner.join("target");
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&target)
            .unwrap();
        std::fs::write(target.join("keep"), b"unmanaged final output").unwrap();
        match beside {
            "none" => {}
            "cache-folder" => {
                std::fs::create_dir(target.join("cache")).unwrap();
                std::fs::write(target.join("cache/tool.tar"), b"x").unwrap();
            }
            "db-file" => std::fs::write(target.join("db"), b"x").unwrap(),
            "index-link" => std::os::unix::fs::symlink("keep", target.join("index")).unwrap(),
            _ => unreachable!(),
        }
        let args = vec![
            if trust { "trust" } else { "--unverified" }.into(),
            "check".into(),
            format!(
                "--manifest-path={}",
                world.owner.join("Cargo.toml").display()
            ),
        ];
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &super::super::read_config,
            read_manifest: &super::super::read_manifest,
            // A shared cache unavailable to a plain build takes the same local fallback.
            writable: &|_| Some(false),
        };
        for _ in 0..2 {
            let observed = observe_for_producer(
                &args,
                super::super::Tool::Targo,
                &world.home,
                &machine,
                true,
            );
            assert!(
                matches!(observed, Ok(false)),
                "{trust} {beside}: {observed:?}"
            );
            let records = roots(&world.home);
            assert_eq!(records.len(), 1, "{trust} {beside}");
            assert_eq!(records[0].root, super::super::target_build_path(&target));
            assert_eq!(
                records[0].owners.as_slice(),
                std::slice::from_ref(&world.owner)
            );
            assert!(!target.join(MARKER).exists());
            assert_eq!(
                std::fs::read(target.join("keep")).unwrap(),
                b"unmanaged final output"
            );
        }
    }
}

#[test]
fn quiet_physical_target_inherited_by_verify_prepares_the_owned_target() {
    let world = World::new();
    world.build();
    let link = world.owner.join("target");
    std::os::unix::fs::symlink(&world.target, &link).unwrap();
    let physical = link.canonicalize().unwrap();
    let directory = super::super::target_build_path(&physical);
    let value = format!("build.build-dir='{}'", directory.display());
    let environment = |name: &str| match name {
        "CARGO_TARGET_DIR" => Some(physical.clone().into_os_string()),
        super::super::BUILD_DIR_ENV => Some(directory.clone().into_os_string()),
        _ => None,
    };
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &environment,
        read: &super::super::read_config,
        read_manifest: &super::super::read_manifest,
        writable: &|_| Some(true),
    };
    let args = vec![
        "--config".into(),
        value,
        "--unverified".into(),
        "check".into(),
    ];
    let receipt = std::fs::read(world.path()).unwrap();
    assert!(
        observe_for_producer(
            &args,
            super::super::Tool::Targo,
            &world.home,
            &machine,
            true
        )
        .unwrap()
    );
    assert!(directory.is_dir());
    assert_eq!(std::fs::read(world.path()).unwrap(), receipt);
    assert_eq!(std::fs::read_link(&link).unwrap(), physical);
    assert_eq!(roots(&world.home).len(), 1);
}

#[test]
fn inherited_placement_keeps_known_choices_and_protects_unknown_ones() {
    for choice in [
        "mismatch",
        "opaque",
        "override",
        "file",
        "unknown-template",
        "unmarked",
    ] {
        let world = World::new();
        world.build();
        let target = world.home.join("ordinary-target");
        let child = super::super::target_build_path(&target);
        let cache = super::super::config_value(&super::super::cache_root(&world.home)).unwrap();
        let mut directory = super::super::directory_from_config(&cache)
            .unwrap()
            .to_owned();
        let mut args = vec!["--unverified".into(), "build".into()];
        let other_directory = (choice == "mismatch").then(|| {
            aterm_tempfile::Builder::new()
                .prefix("different-build-")
                .suffix(".noindex")
                .tempdir_in(world.home.parent().unwrap())
                .unwrap()
        });
        match choice {
            "mismatch" => {
                directory = other_directory
                    .as_ref()
                    .unwrap()
                    .path()
                    .display()
                    .to_string()
            }
            "opaque" => args.extend(["--config".into(), "choice.toml".into()]),
            "override" => args.extend(["--config".into(), "build.build-dir='user'".into()]),
            "file" => {
                std::fs::create_dir(world.owner.join(".cargo")).unwrap();
                std::fs::write(
                    world.owner.join(".cargo/config.toml"),
                    "[build]\nbuild-dir='user'\n",
                )
                .unwrap();
            }
            "unknown-template" => directory = "{unknown-template}/.ubd/build.noindex".into(),
            "unmarked" => {
                std::fs::create_dir_all(&child).unwrap();
                std::fs::write(child.join("keep"), b"existing unmarked cache").unwrap();
                directory = child.display().to_string();
                args.push(format!("--target-dir={}", target.display()));
            }
            _ => unreachable!(),
        }
        let environment =
            |name: &str| (name == super::super::BUILD_DIR_ENV).then(|| directory.clone().into());
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &environment,
            read: &super::super::read_config,
            read_manifest: &super::super::read_manifest,
            writable: &|_| Some(true),
        };
        let observed = observe_on(&args, super::super::Tool::Targo, &world.home, &machine);
        if choice == "unmarked" {
            assert!(observed.is_err());
        } else {
            observed.unwrap();
        }
        assert_eq!(
            read(&world.path()).unwrap().owners_unknown,
            matches!(choice, "opaque" | "file" | "unknown-template"),
            "{choice}"
        );
        if choice == "unmarked" {
            assert!(!child.join(MARKER).exists());
            assert_eq!(
                std::fs::read(child.join("keep")).unwrap(),
                b"existing unmarked cache"
            );
        }
        assert_eq!(
            std::fs::read(world.target.join("debug/deps/keep")).unwrap(),
            b"reusable output"
        );
    }
}

#[test]
fn a_registered_cache_without_cargo_locks_is_kept_until_they_exist() {
    let world = World::new();
    assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
    let marker = std::fs::read(world.target.join(MARKER)).unwrap();
    let receipt = std::fs::read(world.path()).unwrap();
    let original = identity(&world.target).unwrap();
    std::fs::remove_dir_all(&world.owner).unwrap();
    let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    let reporting = super::super::fence::Fence::new(&clear).with_reporting();
    let now = world.later();
    let removed = reclaim_on(
        &world.home,
        now,
        super::super::ReclaimAfter::CALM,
        None,
        &mut |_, _| panic!("an unbuilt cache grants no removal intent"),
        &reporting,
    );
    assert!(removed.is_empty());
    assert_eq!(identity(&world.target).as_ref(), Some(&original));
    assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
    assert_eq!(std::fs::read(world.path()).unwrap(), receipt);
    assert_eq!(
        reporting.retained(),
        [super::super::RetainedCheck {
            path: world.target.clone(),
            reason: super::super::KeepReason::NoCargoLocks,
        }]
    );
    assert!(plan(&world.home, now).is_empty());

    // The same registration and gone owner are eligible once Cargo left its lock.
    std::fs::create_dir_all(world.target.join("debug/deps")).unwrap();
    std::fs::write(world.target.join("debug/deps/output"), b"reusable output").unwrap();
    std::fs::write(world.target.join("debug/.cargo-lock"), b"").unwrap();
    let mut asked = Vec::new();
    let removed = reclaim_on(
        &world.home,
        now,
        super::super::ReclaimAfter::CALM,
        None,
        &mut |path, why| {
            asked.push((path.to_path_buf(), why));
            true
        },
        &super::super::fence::Fence::new(&clear),
    );
    assert_eq!(asked.len(), 1);
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].0, world.target);
    assert!(!world.target.exists());
}

#[test]
fn every_shared_owner_and_live_cargo_lock_retains_current_dependencies() {
    let world = World::new();
    world.build();
    let other = world.another_owner();
    assert!(create_checked(&world.home, &world.target, &other).unwrap());
    std::fs::remove_dir_all(&world.owner).unwrap();
    assert!(
        reclaim(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true
        )
        .is_empty()
    );
    assert!(world.target.join("debug/deps/keep").is_file());
    std::fs::remove_dir_all(&other).unwrap();
    let file = std::fs::File::open(world.target.join("debug/.cargo-lock")).unwrap();
    let held = crate::lock::Flock::try_lock(file).unwrap();
    assert!(
        reclaim(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true
        )
        .is_empty()
    );
    drop(held);
    let removed = reclaim(
        &world.home,
        world.later(),
        super::super::ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
    );
    assert_eq!(removed.len(), 1);
    assert!(!world.target.exists());
}

/// A live worktree of the shared cache, its crate folder as old as the fixture's clock says.
#[cfg(target_os = "macos")]
fn live_in_shared_cache(world: &World) -> PathBuf {
    let live = world.home.join("live");
    std::fs::create_dir(&live).unwrap();
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let root = super::super::cache_root(&world.home);
    let profile = super::super::build_dir(&root, &live).join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
    let folder = profile.join("incremental/ws-1k116trr0a0xf");
    std::fs::create_dir_all(folder.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
    folder
}

/// DEAD FIRST HOLDS ACROSS ROOTS: with room for two removals, a pass takes the root the cache
/// moved from and a registered cache whose owners are gone, and the shared cache's live
/// worktree keeps its warm folder. NEGATIVE CONTROL: the third ask is that folder's.
#[cfg(target_os = "macos")]
#[test]
fn a_pass_takes_every_roots_dead_before_any_live_part() {
    let world = World::new();
    world.build();
    std::fs::remove_dir_all(&world.owner).unwrap();
    let folder = live_in_shared_cache(&world);
    let moved = super::super::moved_from(&world.home).join("Users/gone");
    let moved_leaf = moved.join(super::super::LEAF);
    std::fs::create_dir_all(moved_leaf.join("debug/deps")).unwrap();

    let mut asked = Vec::new();
    super::super::reclaim_beside_sessions(&world.home, world.later(), true, None, &mut |_, why| {
        asked.push(why);
        asked.len() <= 2
    });

    assert!(
        matches!(
            asked.as_slice(),
            [
                super::super::Unused::Moved { .. },
                super::super::Unused::Gone { .. },
                super::super::Unused::Stale { .. },
            ]
        ),
        "{asked:?}"
    );
    assert!(!moved_leaf.exists() && !world.target.exists());
    assert!(folder.is_dir());
}

/// THE SHARED CACHE'S DECLINE SPEAKS FOR A REGISTERED CACHE'S PRUNE: a record that declines the
/// first is not asked of the second, whose parts would only move aside and back. A record
/// that takes them gets both, the shared cache's first.
#[cfg(target_os = "macos")]
#[test]
fn a_decline_of_a_shared_prune_spares_a_registered_cache_the_move() {
    let world = World::new();
    world.build();
    let unit = world.target.join("debug/incremental/old-unit");
    std::fs::create_dir_all(&unit).unwrap();
    let folder = live_in_shared_cache(&world);
    let pass = |record: &mut dyn FnMut(&Path, super::super::Unused) -> bool| {
        super::super::reclaim_beside_sessions(&world.home, world.later(), true, None, record)
    };

    let mut asked = Vec::new();
    pass(&mut |path, _| {
        asked.push(path.to_path_buf());
        false
    });
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(unit.is_dir() && folder.is_dir());

    let mut asked = Vec::new();
    pass(&mut |path, _| {
        asked.push(path.to_path_buf());
        true
    });
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(!asked[0].starts_with(&world.target), "{asked:?}");
    assert!(asked[1].starts_with(&world.target), "{asked:?}");
    assert!(!unit.exists() && !folder.exists());
    assert!(world.target.join("debug/deps/keep").is_file());
}

/// WHAT A PASS LEFT OF THE DEAD IT DOES NOT ASK ABOUT AGAIN: its prunes come after, and only
/// prune. NEGATIVE CONTROL: the shared cache's prune went, so the prunes ran.
#[cfg(target_os = "macos")]
#[test]
fn a_pass_asks_of_a_dead_cache_once() {
    let world = World::new();
    world.build();
    std::fs::remove_dir_all(&world.owner).unwrap();
    let folder = live_in_shared_cache(&world);
    let mut gone = 0;
    super::super::reclaim_beside_sessions(&world.home, world.later(), true, None, &mut |_, why| {
        let dead = matches!(why, super::super::Unused::Gone { .. });
        gone += usize::from(dead);
        !dead
    });
    assert_eq!(gone, 1);
    assert!(world.target.is_dir() && !folder.exists());
}

#[test]
fn an_interrupted_stale_stage_is_planned_and_retried_under_its_receipt_gate() {
    use std::os::unix::fs::MetadataExt as _;
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            super::super::remove::seams::clear();
        }
    }
    let world = World::new();
    world.build();
    let incremental = world.target.join("debug/incremental/old-unit");
    std::fs::create_dir_all(&incremental).unwrap();
    let held = incremental.join("held");
    std::fs::write(&held, b"unfinished output").unwrap();
    let _clear = Clear;
    super::super::remove::seams::other_user(std::fs::symlink_metadata(&held).unwrap().ino());
    let now = SystemTime::now() + super::super::RECLAIM_STALE + Duration::from_secs(60);
    let went = reclaim(
        &world.home,
        now,
        super::super::ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
    );
    assert!(
        went.iter()
            .any(|(_, why)| matches!(why, super::super::Unused::Stayed { .. })),
        "{went:?}"
    );
    let stages = super::super::stages_in(&world.target, std::ffi::OsStr::new(super::super::DOOMED));
    assert_eq!(stages.len(), 1, "the real stale prune left one stage");
    let stage = &stages[0];
    assert!(!incremental.exists());
    assert!(world.target.join("debug/deps/keep").is_file());
    super::super::remove::seams::clear();
    assert!(
        plan(&world.home, now)
            .iter()
            .any(|p| p.path == *stage && p.why == super::super::Unused::Unfinished)
    );

    let file = crate::metadata_io::open_regular(&gate_path(&world.path()).unwrap()).unwrap();
    let gate = crate::lock::Flock::try_lock(file).unwrap();
    assert!(
        plan(&world.home, now).is_empty(),
        "a busy receipt gate admits no stage plan"
    );
    assert!(
        reclaim(
            &world.home,
            now,
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true
        )
        .is_empty()
    );
    drop(gate);
    assert!(
        reclaim(
            &world.home,
            now,
            super::super::ReclaimAfter::CALM,
            None,
            &mut |p, _| p != stage
        )
        .is_empty()
    );
    assert!(stage.is_dir(), "a refused intent keeps the stage");
    let retried = reclaim(
        &world.home,
        now,
        super::super::ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
    );
    assert_eq!(retried, [(stage.clone(), super::super::Unused::Unfinished)]);
    assert!(!stage.exists());
    assert!(
        world.target.join("debug/deps/keep").is_file(),
        "the live owner's current dependencies stay"
    );
    assert!(plan(&world.home, now).is_empty());
}

#[test]
fn a_marked_cache_with_missing_or_corrupt_owner_records_refuses_another_producer() {
    let world = World::new();
    world.build();
    let other = world.another_owner();
    let before = std::fs::read(world.path()).unwrap();
    for record in [None, Some(b"{".as_slice())] {
        match record {
            None => std::fs::remove_file(world.path()).unwrap(),
            Some(bytes) => std::fs::write(world.path(), bytes).unwrap(),
        }
        let error = create_checked(&world.home, &world.target, &other).unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(world.target.join("debug/deps/keep").is_file());
        std::fs::write(world.path(), &before).unwrap();
    }
}

#[test]
fn an_advisory_inventory_does_not_exclude_a_new_producer() {
    use std::cell::RefCell;
    thread_local! {
        static INPUT: RefCell<Option<(PathBuf, PathBuf, PathBuf)>> = const { RefCell::new(None) };
        static RESULT: RefCell<Option<std::io::Result<bool>>> = const { RefCell::new(None) };
    }
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            INPUT.with(|input| input.borrow_mut().take());
            RESULT.with(|result| result.borrow_mut().take());
        }
    }
    fn register_during_inventory() {
        let input = INPUT.with(|input| input.borrow_mut().take());
        if let Some((home, target, owner)) = input {
            let result = create_checked(&home, &target, &owner);
            RESULT.with(|slot| *slot.borrow_mut() = Some(result));
        }
    }
    let world = World::new();
    world.build();
    let other = world.another_owner();
    let payload = std::fs::read(world.target.join("debug/deps/keep")).unwrap();
    let _clear = Clear;
    INPUT.with(|input| {
        *input.borrow_mut() = Some((world.home.clone(), world.target.clone(), other.clone()));
    });
    aterm_uds::progress::heard_by(register_during_inventory, || {
        let _ = plan(&world.home, world.later());
    });
    let result = RESULT.with(|result| result.borrow_mut().take());
    assert!(matches!(result, Some(Ok(true))), "{result:?}");
    assert!(read(&world.path()).unwrap().owners.contains(&other));
    assert_eq!(
        std::fs::read(world.target.join("debug/deps/keep")).unwrap(),
        payload
    );
}

#[test]
fn a_busy_producer_gate_refuses_direct_and_linked_targets_without_changing_custody() {
    for linked in [false, true] {
        let world = World::new();
        world.build();
        let other = world.another_owner();
        let named = if linked {
            let link = world.home.join("target-link");
            std::os::unix::fs::symlink(&world.target, &link).unwrap();
            link
        } else {
            world.target.clone()
        };
        let before = receipts(&world.home);
        let marker = std::fs::read(world.target.join(MARKER)).unwrap();
        let payload = std::fs::read(world.target.join("debug/deps/keep")).unwrap();
        let link_text = linked.then(|| std::fs::read_link(&named).unwrap());
        let observe = |owner: &Path| {
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(owner),
                env: &|_| None,
                read: &|_| Ok(None),
                read_manifest: &|_| Ok(None),
                writable: &|_| Some(true),
            };
            observe_for_producer(
                &build_into(owner, &named),
                super::super::Tool::Targo,
                &world.home,
                &machine,
                true,
            )
        };
        #[cfg(target_os = "macos")]
        let front = || {
            let words = [
                build_into(&other, &named),
                vec!["--package=busy-gate-tool-started".into()],
            ]
            .concat();
            through_clean_front_in(&world, &other, Path::new("/bin/echo"), &words, None)
        };
        let gate = crate::metadata_io::open_regular(&gate_path(&world.path()).unwrap()).unwrap();
        let held = crate::lock::Flock::try_lock(gate).unwrap();
        let opener = format!("pid {}", std::process::id());
        let gate_name = format!("gate={:?}", gate_path(&world.path()).unwrap());
        for owner in [&world.owner, &other] {
            let error = observe(owner).unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::WouldBlock,
                "{linked}: {error}"
            );
            assert!(
                error
                    .to_string()
                    .contains(&world.target.display().to_string())
            );
            assert!(error.to_string().contains(&gate_name), "{error}");
            assert!(error.to_string().contains(&opener), "{error}");
        }
        #[cfg(target_os = "macos")]
        {
            let out = front();
            assert_eq!(out.status.code(), Some(75), "{linked}: {out:?}");
            assert!(!String::from_utf8_lossy(&out.stdout).contains("busy-gate-tool-started"));
            assert!(
                String::from_utf8_lossy(&out.stderr).contains(&world.target.display().to_string()),
                "{linked}: {out:?}"
            );
            assert!(
                String::from_utf8_lossy(&out.stderr).contains(&opener),
                "{out:?}"
            );
            assert!(!world.home.join("ledger.noindex").exists());
        }
        assert_eq!(receipts(&world.home), before);
        assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
        assert_eq!(
            std::fs::read(world.target.join("debug/deps/keep")).unwrap(),
            payload
        );
        if let Some(text) = link_text {
            assert_eq!(std::fs::read_link(&named).unwrap(), text);
        }
        drop(held);
        assert!(observe(&other).unwrap());
        assert_eq!(
            read(&world.path()).unwrap().owners,
            [world.owner.clone(), other.clone()]
        );
        #[cfg(target_os = "macos")]
        {
            let out = front();
            assert!(out.status.success(), "{linked}: {out:?}");
            assert!(String::from_utf8_lossy(&out.stdout).contains("busy-gate-tool-started"));
        }
    }
}

/// A PENDING STAGE IS FORECAST, AND WHAT STAYED OF IT NAMED, BY ITS OWN REMOVAL'S RULES: a
/// pending stage holding, beside its payload, a folder its removal was never given (sorting
/// before the payload) is planned with the forecast that the stage itself will stay for what
/// it holds, its payload going; the pass then leaves just that, returned as what stayed, and
/// its readers name the stage with that reason, never the folder's entry. With an entry of
/// the payload read as another user's (the remover's seam), the forecast and the pass's
/// readers name that entry under the stage instead, the stage never tried. NEGATIVE CONTROL:
/// the plain forecast of the same stage says nothing stays, and a walk of what stands names
/// the folder's entry first.
#[test]
fn a_pending_stage_is_forecast_and_named_by_its_own_removal() {
    use std::os::unix::fs::MetadataExt as _;
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            super::super::remove::seams::clear();
        }
    }
    use super::super::remove::{self as remove, Why};
    for kept in [false, true] {
        let world = World::new();
        world.build();
        let mut receipt = read(&world.path()).unwrap();
        let stage = world.home.join("interrupted-retirement.noindex");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&stage)
            .unwrap();
        receipt.pending = Some(Pending {
            directory: stage.clone(),
            identity: identity(&stage).unwrap(),
            authorized: false,
        });
        write(&world.path(), &receipt).unwrap();
        std::fs::rename(&world.target, stage.join(PAYLOAD)).unwrap();
        std::fs::create_dir_all(stage.join("a-foreign/sub")).unwrap();
        std::fs::write(stage.join("a-foreign/sub/y"), b"never given").unwrap();
        let _clear = Clear;
        let theirs = stage.join(PAYLOAD).join("debug/deps/keep");
        if kept {
            remove::seams::other_user(std::fs::symlink_metadata(&theirs).unwrap().ino());
        }
        let (rel, why) = if kept {
            (
                PathBuf::from("cache.noindex/debug/deps/keep"),
                Why::AnotherUser,
            )
        } else {
            (PathBuf::new(), Why::HoldsOthers)
        };

        let planned = plan(&world.home, world.later());
        let one = planned
            .iter()
            .find(|p| p.path == stage)
            .unwrap_or_else(|| panic!("{planned:?}"));
        let stays = one.stays.as_ref().unwrap();
        assert_eq!((stays.count, &stays.first), (1, &vec![(rel.clone(), why)]));
        let line = crate::cli::cache_plan_line(one);
        assert!(line.contains(&why.words(&stage.join(&rel))), "{line}");
        assert_eq!(
            super::super::Planned::stage(stage.clone())
                .stays
                .unwrap()
                .count,
            usize::from(kept),
            "the plain forecast"
        );

        let removed = reclaim(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        );
        let Some((_, super::super::Unused::Stayed { stayed, .. })) =
            removed.iter().find(|(p, _)| *p == stage)
        else {
            panic!("{removed:?}");
        };
        let named = remove::named(&stage, *stayed);
        let at = if kept { rel.clone() } else { stage.clone() };
        assert_eq!(named.first, [(at, why)]);
        let words = remove::stayed_words(&stage, *stayed);
        assert!(!words.contains("a-foreign"), "{words}");
        assert_eq!(
            std::fs::read(stage.join("a-foreign/sub/y")).unwrap(),
            b"never given"
        );
        assert_eq!(stage.join(PAYLOAD).exists(), kept);
        assert_eq!(
            remove::stayed(&stage, 1),
            [stage.join("a-foreign/sub/y")],
            "a walk of what stands names the folder's entry first"
        );
    }
}

#[test]
fn pending_stage_survives_fresh_replacement_and_finishes_without_touching_new_outputs() {
    let world = World::new();
    world.build();
    let mut before = read(&world.path()).unwrap();
    let stage = world.home.join("interrupted-retirement.noindex");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&stage)
        .unwrap();
    before.pending = Some(Pending {
        directory: stage.clone(),
        identity: identity(&stage).unwrap(),
        authorized: false,
    });
    write(&world.path(), &before).unwrap();
    std::fs::rename(&world.target, stage.join("cache.noindex")).unwrap();
    let owner = world.another_owner();
    assert!(create_checked(&world.home, &world.target, &owner).unwrap());
    std::fs::write(world.target.join("new-output"), b"keep the new generation").unwrap();
    assert_eq!(
        roots(&world.home).len(),
        2,
        "the old stage has independent custody"
    );
    let removed = reclaim(
        &world.home,
        world.later(),
        super::super::ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
    );
    assert_eq!(removed.len(), 1);
    assert!(!stage.exists());
    assert_eq!(
        std::fs::read(world.target.join("new-output")).unwrap(),
        b"keep the new generation"
    );
    assert_eq!(roots(&world.home).len(), 1);
}

/// A REGISTERED CACHE WHOSE RENAME IS REFUSED ONCE ITS INTENT IS ON RECORD CLOSES THAT
/// INTENT: a gone owner's cache passes its checks and its intent is put on record; then its
/// root is flagged `uchg` (its owner may set it), so its rename into the private stage fails.
/// The pass returns it refused by the rename's error kind, nothing of it gone, its outputs
/// whole; with the flag cleared, the next pass recovers the empty stage the refused move left
/// and the one after it retires the cache. NEGATIVE CONTROL: without the outcome, the pass
/// returns nothing for the intent it put on record.
#[cfg(target_os = "macos")]
#[test]
fn a_refused_rename_after_the_intent_closes_it_with_nothing_gone() {
    struct Unflag(PathBuf);
    impl Drop for Unflag {
        fn drop(&mut self) {
            let _ = crate::platform::set_file_flags(&self.0, 0);
        }
    }
    let world = World::new();
    world.build();
    std::fs::remove_dir_all(&world.owner).unwrap();
    let unflag = Unflag(world.target.clone());
    let mut asked = Vec::new();
    let removed = reclaim(
        &world.home,
        world.later(),
        super::super::ReclaimAfter::CALM,
        None,
        &mut |path, _| {
            asked.push(path.to_path_buf());
            crate::platform::set_file_flags(path, libc::UF_IMMUTABLE).unwrap();
            true
        },
    );
    assert_eq!(asked, std::slice::from_ref(&world.target));
    assert_eq!(
        removed,
        [(
            world.target.clone(),
            super::super::Unused::Refused {
                by: super::super::Refusal::Rename(std::io::ErrorKind::PermissionDenied)
            }
        )]
    );
    assert!(world.target.join("debug/deps/keep").is_file());
    drop(unflag);
    let pass = || {
        reclaim(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        )
    };
    assert!(
        pass().is_empty(),
        "the empty stage recovered, nothing removed"
    );
    assert!(world.target.join("debug/deps/keep").is_file());
    assert!(
        matches!(pass().as_slice(), [(path, super::super::Unused::Gone { .. })] if path == &world.target)
    );
    assert!(!world.target.exists());
}

/// A REGISTERED CACHE WHOSE ROOT IS FLAGGED BEFORE THE PASS IS KEPT WITH NO INTENT (review
/// RV2, J2-1-R1): a gone owner's cache whose root its owner flagged `uchg` before any pass
/// cannot have its name taken out of its folder, which is read before its intent, as a leaf's
/// is (`movable`), so two passes ask nothing, return nothing, make no stage and write no
/// pending stage into its receipt, and its outputs stay whole; with the flag cleared, the
/// next pass retires it. NEGATIVE CONTROL (run): without that read, each pass asks its intent
/// and returns it refused at its rename (`Refusal::Rename`), the churn of a removing row and a
/// kept one every other pass.
#[cfg(target_os = "macos")]
#[test]
fn a_registered_root_flagged_before_the_pass_is_kept_with_no_intent() {
    struct Unflag(PathBuf);
    impl Drop for Unflag {
        fn drop(&mut self) {
            let _ = crate::platform::set_file_flags(&self.0, 0);
        }
    }
    let world = World::new();
    world.build();
    std::fs::remove_dir_all(&world.owner).unwrap();
    crate::platform::set_file_flags(&world.target, libc::UF_IMMUTABLE).unwrap();
    let unflag = Unflag(world.target.clone());
    let parent = world.target.parent().unwrap().to_path_buf();
    let stages = || {
        std::fs::read_dir(&parent)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("atpkg-cache-retiring-")
            })
            .count()
    };
    for pass in 0..2 {
        let mut asked = Vec::new();
        let removed = reclaim(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |path, _| {
                asked.push(path.to_path_buf());
                true
            },
        );
        assert!(asked.is_empty(), "pass {pass}: no intent asked: {asked:?}");
        assert!(removed.is_empty(), "pass {pass}: {removed:?}");
        assert_eq!(stages(), 0, "pass {pass}: no stage made");
        assert!(
            read(&world.path()).is_some_and(|receipt| receipt.pending.is_none()),
            "pass {pass}: no pending stage on record"
        );
        assert!(world.target.join("debug/deps/keep").is_file());
    }
    drop(unflag);
    let removed = reclaim(
        &world.home,
        world.later(),
        super::super::ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
    );
    assert!(
        matches!(removed.as_slice(), [(path, super::super::Unused::Gone { .. })] if path == &world.target),
        "{removed:?}"
    );
    assert!(!world.target.exists());
}

/// A REGISTERED CACHE STOPPED ONCE STAGED CLOSES AS ITS PENDING STAGE: a gone owner's cache
/// renamed into its private stage, then found in use there (the fence reports a process in a
/// stage that holds the payload, from the moment an intent is on record), is returned as that
/// pending stage with nothing of it gone ([`super::super::Refusal::Staged`]), never as its
/// root, which no longer holds it; the stage holds it all. The next pass, still held, resumes
/// that stage and is stopped the same way, `Staged`; the pass after, the process gone,
/// finishes it ([`super::super::Unused::Unfinished`]). NEGATIVE CONTROL: the move not marked
/// on the open intent, the pass returns the gone root as `Stopped`.
#[test]
fn a_cache_stopped_once_staged_closes_as_its_pending_stage() {
    let world = World::new();
    world.build();
    std::fs::remove_dir_all(&world.owner).unwrap();
    let (held, asked) = (std::cell::Cell::new(true), std::cell::Cell::new(false));
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        let users = dirs
            .iter()
            .filter(|d| held.get() && asked.get() && d.join(PAYLOAD).is_dir())
            .map(|stage| aterm_uds::holders::PathUse {
                pid: 36,
                how: "cwd",
                path: stage.join(PAYLOAD),
            })
            .collect();
        Ok(aterm_uds::holders::PathScan {
            users,
            ..aterm_uds::holders::PathScan::default()
        })
    };
    let fence = super::super::fence::Fence::new(&processes);
    let pass = || {
        asked.set(false);
        reclaim_on(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| {
                asked.set(true);
                true
            },
            &fence,
        )
    };
    let staged = super::super::Unused::Refused {
        by: super::super::Refusal::Staged,
    };
    let went = pass();
    let pending = read(&world.path())
        .and_then(|receipt| receipt.pending)
        .map(|pending| pending.directory)
        .expect("its pending stage");
    assert_eq!(
        went,
        [(pending.clone(), staged)],
        "its stage, nothing of it gone"
    );
    assert!(!world.target.exists(), "the root no longer holds it");
    assert!(
        pending.join(PAYLOAD).join("debug/deps/keep").is_file(),
        "the stage holds it all"
    );
    assert_eq!(
        pass(),
        [(pending.clone(), staged)],
        "resumed and stopped after its intent, the stage again"
    );
    held.set(false);
    assert_eq!(
        pass(),
        [(pending.clone(), super::super::Unused::Unfinished)],
        "the next pass finishes it"
    );
    assert!(!pending.exists() && !world.target.exists());
}

#[test]
fn replacement_and_symlink_races_never_authorize_unrecorded_bytes() {
    let world = World::new();
    world.build();
    std::fs::remove_dir_all(&world.owner).unwrap();
    let old = world.home.join("old-output.noindex");
    let mut swapped = false;
    let removed = reclaim(
        &world.home,
        world.later(),
        super::super::ReclaimAfter::CALM,
        None,
        &mut |path, _| {
            if path == world.target && !swapped {
                std::fs::rename(&world.target, &old).unwrap();
                std::fs::create_dir(&world.target).unwrap();
                std::fs::write(world.target.join("unowned"), b"preserve").unwrap();
                swapped = true;
            }
            true
        },
    );
    assert!(swapped);
    assert_eq!(
        removed,
        [(
            world.target.clone(),
            super::super::Unused::Refused {
                by: super::super::Refusal::Stopped
            }
        )],
        "its intent closed, nothing of it gone"
    );
    assert_eq!(
        std::fs::read(world.target.join("unowned")).unwrap(),
        b"preserve"
    );
    assert!(old.join("debug/deps/keep").exists());
    let alias = world.home.join("alias.noindex");
    std::os::unix::fs::symlink(&world.target, &alias).unwrap();
    let owner = world.another_owner();
    assert!(!create_checked(&world.home, &alias, &owner).unwrap());
}

#[test]
fn outside_home_is_not_an_explicit_output_directory() {
    let world = World::new();
    let outside = world.home.parent().unwrap().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&outside),
        env: &|_| None,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| Some(true),
    };
    let args = ["--unverified".into(), "build".into()];
    assert!(super::super::unresolved_directory(
        &args,
        super::super::Tool::Targo,
        &machine
    ));
    assert!(
        !observe_for_producer(
            &args,
            super::super::Tool::Targo,
            &world.home,
            &machine,
            true
        )
        .unwrap()
    );
    let scoped = super::super::Machine {
        cwd: Some(&world.owner),
        ..machine
    };
    for option in ["--config=build.build-dir='output'", "--config=other.toml"] {
        let args = ["--unverified".into(), "build".into(), option.into()];
        assert!(
            observe_for_producer(&args, super::super::Tool::Targo, &world.home, &scoped, true)
                .is_err(),
            "an explicit unsupported output must still be refused: {option}"
        );
    }
    assert!(!outside.join("output").exists());
    assert!(!world.owner.join("output").exists());
}

#[test]
fn workspace_owners_follow_complete_search_and_package_home_boundaries() {
    use super::super::Tool;
    for (depth, case) in [
        (34, "default"),
        (2, "cargo"),
        (2, "relative"),
        (2, "empty"),
        (2, "twin"),
        (2, "suppressed-twin"),
        (2, "moved-relative"),
    ] {
        let world = World::new();
        let mut member = world.owner.clone();
        for _ in 0..depth {
            member.push("n");
        }
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(
            world.owner.join("Cargo.toml"),
            format!(
                "[workspace]\nmembers=['{}']\n",
                member.strip_prefix(&world.owner).unwrap().display()
            ),
        )
        .unwrap();
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname='member'\nversion='0.0.0'\nedition='2024'\n",
        )
        .unwrap();
        let inside = member.parent().unwrap();
        let outside = world.home.join("cargo-home");
        let env = |name: &str| match (name, case) {
            ("CARGO_HOME", "cargo") => Some(inside.as_os_str().to_owned()),
            ("CARGO_HOME", "relative" | "moved-relative") => Some("..".into()),
            ("CARGO_HOME", "empty") => Some("".into()),
            ("CARGO_HOME", "suppressed-twin") => Some(outside.as_os_str().to_owned()),
            ("TARGO_HOME", "twin" | "suppressed-twin" | "empty") => {
                Some(inside.as_os_str().to_owned())
            }
            _ => None,
        };
        let read = |path: &Path| match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        };
        let launcher = world.home.join("launcher");
        std::fs::create_dir(&launcher).unwrap();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(if case == "moved-relative" {
                &launcher
            } else {
                &member
            }),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let mut args = Vec::new();
        if case == "moved-relative" {
            args.extend([
                "-Zunstable-options".into(),
                "-C".into(),
                member.display().to_string(),
            ]);
        }
        args.extend([
            "--unverified".into(),
            "build".into(),
            "--target-dir".into(),
            world.target.display().to_string(),
        ]);
        let result = observe_for_producer(&args, Tool::Targo, &world.home, &machine, true);
        if case == "twin" {
            assert!(
                result.is_err(),
                "different supported workspace owners: {case}"
            );
            assert!(!world.target.exists());
            assert!(roots(&world.home).is_empty());
            continue;
        }
        assert!(result.unwrap(), "{case}");
        let owner = if matches!(case, "cargo" | "relative" | "moved-relative") {
            &member
        } else {
            &world.owner
        };
        let recorded = roots(&world.home);
        assert_eq!(recorded.len(), 1, "{case}");
        assert_eq!(recorded[0].root, world.target, "{case}");
        assert_eq!(recorded[0].owners, std::slice::from_ref(owner), "{case}");
    }
}

/// A workspace's owner keeps Targo's logical spellings of the manifest and the package home: the
/// root walk stops after the home as spelled, and the owner is the root found, canonical. A
/// manifest that is a link is read through, its package the folder the link stands in, which
/// owns it. NEGATIVE CONTROL: a root that does not list the member fails Targo's member check
/// (`error_if_manifest_not_in_members`), and a linked manifest whose file lies outside HOME
/// grants no permission to read it: each is refused, nothing recorded.
#[test]
fn workspace_owners_keep_logical_manifest_and_home_spellings() {
    for case in [
        "home-alias",
        "both-alias",
        "manifest-alias",
        "file-link",
        "unlisted",
        "file-link-outside",
    ] {
        let world = World::new();
        let member = world.owner.join("n/member");
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname='member'\nversion='0.0.0'\n",
        )
        .unwrap();
        if case != "unlisted" {
            std::fs::write(
                world.owner.join("Cargo.toml"),
                "[workspace]\nmembers = ['n/member']\n",
            )
            .unwrap();
        }
        let alias = world.home.join("alias");
        std::os::unix::fs::symlink(&world.owner, &alias).unwrap();
        let (manifest, package_home, expected) = match case {
            "home-alias" | "unlisted" => (
                member.join("Cargo.toml"),
                alias.join("n"),
                world.owner.clone(),
            ),
            "both-alias" => (
                alias.join("n/member/Cargo.toml"),
                alias.join("n"),
                member.clone(),
            ),
            "manifest-alias" => (
                alias.join("n/member/Cargo.toml"),
                world.owner.join("n"),
                world.owner.clone(),
            ),
            "file-link" => {
                let logical = world.home.join("linked-member");
                std::fs::create_dir(&logical).unwrap();
                std::os::unix::fs::symlink(member.join("Cargo.toml"), logical.join("Cargo.toml"))
                    .unwrap();
                (logical.join("Cargo.toml"), world.home.clone(), logical)
            }
            "file-link-outside" => {
                let outside = world.home.parent().unwrap().join("outside");
                std::fs::create_dir(&outside).unwrap();
                std::fs::copy(member.join("Cargo.toml"), outside.join("Cargo.toml")).unwrap();
                let logical = world.home.join("linked-member");
                std::fs::create_dir(&logical).unwrap();
                std::os::unix::fs::symlink(outside.join("Cargo.toml"), logical.join("Cargo.toml"))
                    .unwrap();
                (logical.join("Cargo.toml"), world.home.clone(), logical)
            }
            _ => unreachable!(),
        };
        let env = |name: &str| (name == "CARGO_HOME").then(|| package_home.as_os_str().to_owned());
        let read = |path: &Path| match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = vec![
            "--unverified".into(),
            "build".into(),
            "--manifest-path".into(),
            manifest.display().to_string(),
            "--target-dir".into(),
            world.target.display().to_string(),
        ];
        let verdict = observe_for_producer(
            &args,
            super::super::Tool::Targo,
            &world.home,
            &machine,
            true,
        );
        if matches!(case, "unlisted" | "file-link-outside") {
            assert!(verdict.is_err(), "{case}");
            assert!(!world.target.exists(), "{case}");
            assert!(roots(&world.home).is_empty(), "{case}");
            continue;
        }
        assert!(verdict.unwrap(), "{case}");
        let recorded = roots(&world.home);
        assert_eq!(recorded.len(), 1, "{case}");
        assert_eq!(recorded[0].owners, [expected], "{case}");
    }
}

#[test]
fn workspace_owners_keep_unknown_ancestors_and_external_roots_unmanaged() {
    for case in [
        "malformed",
        "wrong-type",
        "wrong-members",
        "pointer-type",
        "missing-pointer",
        "unreadable",
        "external-root",
        "external-root-listed",
        "external-pointer",
        "redirected",
    ] {
        let world = World::new();
        let member = world.owner.join("n/member");
        std::fs::create_dir_all(&member).unwrap();
        let package = "[package]\nname='member'\nversion='0.0.0'\n";
        std::fs::write(member.join("Cargo.toml"), package).unwrap();
        let parent_manifest = world.owner.join("Cargo.toml");
        match case {
            "malformed" => std::fs::write(&parent_manifest, "[workspace").unwrap(),
            "wrong-type" => std::fs::write(&parent_manifest, "workspace=false").unwrap(),
            "wrong-members" => {
                std::fs::write(&parent_manifest, "[workspace]\nmembers=[1]").unwrap()
            }
            "pointer-type" => {
                std::fs::write(member.join("Cargo.toml"), format!("{package}workspace=1\n"))
                    .unwrap()
            }
            "missing-pointer" => std::fs::write(
                member.join("Cargo.toml"),
                format!("{package}workspace='../missing'\n"),
            )
            .unwrap(),
            "unreadable" => {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&parent_manifest, std::fs::Permissions::from_mode(0o000))
                    .unwrap();
            }
            "external-root" => {
                std::fs::write(
                    &parent_manifest,
                    "[package]\nname='parent'\nversion='0.0.0'\n",
                )
                .unwrap();
                std::fs::write(world.home.join("Cargo.toml"), "[workspace]\n").unwrap();
            }
            // A root that lists the member, at HOME or outside it, is one Targo builds and no
            // owner may be: the package is managed, so the target it names is refused.
            "external-root-listed" => {
                std::fs::write(
                    &parent_manifest,
                    "[package]\nname='parent'\nversion='0.0.0'\n",
                )
                .unwrap();
                std::fs::write(
                    world.home.join("Cargo.toml"),
                    "[workspace]\nmembers=['source/n/member']\n",
                )
                .unwrap();
            }
            "external-pointer" => {
                std::fs::write(
                    member.join("Cargo.toml"),
                    format!("{package}workspace='../../../..'\n"),
                )
                .unwrap();
                std::fs::write(
                    world.home.parent().unwrap().join("Cargo.toml"),
                    "[workspace]\nmembers=['home/source/n/member']\n",
                )
                .unwrap();
            }
            "redirected" => {
                let directory = world.home.parent().unwrap().join("elsewhere");
                std::fs::create_dir(&directory).unwrap();
                let outside = directory.join("Cargo.toml");
                std::fs::write(&outside, "[workspace]\n").unwrap();
                std::fs::remove_file(&parent_manifest).unwrap();
                std::os::unix::fs::symlink(outside, &parent_manifest).unwrap();
            }
            _ => unreachable!(),
        }
        let env = |name: &str| (name == "CARGO_HOME").then(|| world.home.as_os_str().to_owned());
        let read = |path: &Path| match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&member),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = vec![
            "--unverified".into(),
            "build".into(),
            "--target-dir".into(),
            world.target.display().to_string(),
        ];
        let verdict = observe_for_producer(
            &args,
            super::super::Tool::Targo,
            &world.home,
            &machine,
            true,
        );
        if case == "unreadable" {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&parent_manifest, std::fs::Permissions::from_mode(0o644))
                .unwrap();
        }
        assert!(verdict.is_err(), "{case}");
        assert!(!world.target.exists(), "{case}");
        assert!(roots(&world.home).is_empty(), "{case}");
    }
}

/// A workspace's `exclude` and a `target/package` folder bound its root: an excluded member and
/// a packaged source are their own roots, while an explicit `members` prefix wins over the
/// exclusion, and an ancestor's pointer names the root, each a root that lists the member.
/// NEGATIVE CONTROL: the same explicit prefix and pointer with a root that lists no such member
/// fail Targo's member check (`error_if_manifest_not_in_members`, or the load of a `members`
/// entry that is no package): each is refused, nothing recorded.
#[test]
fn workspace_owners_respect_exclusion_and_packaged_source_boundaries() {
    for case in [
        "excluded",
        "explicit-member",
        "packaged",
        "ancestor-pointer",
        "explicit-member-unlisted",
        "ancestor-pointer-unlisted",
    ] {
        let world = World::new();
        let member = world.owner.join(if case == "packaged" {
            "target/package/member"
        } else {
            "n/member"
        });
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname='member'\nversion='0.0.0'\n",
        )
        .unwrap();
        let definition = match case {
            "explicit-member" => "[workspace]\nmembers=['n/member']\nexclude=['n']\n",
            "explicit-member-unlisted" => "[workspace]\nmembers=['n']\nexclude=['n']\n",
            _ => "[workspace]\nexclude=['n']\n",
        };
        std::fs::write(world.owner.join("Cargo.toml"), definition).unwrap();
        if let Some(listed) = match case {
            "ancestor-pointer" => Some("[workspace]\nmembers=['n/member']\n"),
            "ancestor-pointer-unlisted" => Some("[workspace]\n"),
            _ => None,
        } {
            std::fs::write(world.owner.join("Cargo.toml"), listed).unwrap();
            std::fs::write(
                member.parent().unwrap().join("Cargo.toml"),
                "[package]\nname='parent'\nversion='0.0.0'\nworkspace='..'\n",
            )
            .unwrap();
        }
        let env = |name: &str| (name == "CARGO_HOME").then(|| world.home.as_os_str().to_owned());
        let read = |path: &Path| match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&member),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = vec![
            "--unverified".into(),
            "build".into(),
            "--target-dir".into(),
            world.target.display().to_string(),
        ];
        let verdict = observe_for_producer(
            &args,
            super::super::Tool::Targo,
            &world.home,
            &machine,
            true,
        );
        if case.ends_with("-unlisted") {
            assert!(verdict.is_err(), "{case}");
            assert!(!world.target.exists(), "{case}");
            assert!(roots(&world.home).is_empty(), "{case}");
            continue;
        }
        assert!(verdict.unwrap(), "{case}");
        let expected = if matches!(case, "explicit-member" | "ancestor-pointer") {
            &world.owner
        } else {
            &member
        };
        let recorded = roots(&world.home);
        assert_eq!(recorded.len(), 1, "{case}");
        assert_eq!(recorded[0].owners, std::slice::from_ref(expected), "{case}");
    }
}

#[test]
fn explicit_manifest_and_target_precedence_bind_the_real_workspace() {
    let world = World::new();
    let launcher = world.home.join("launcher");
    std::fs::create_dir(&launcher).unwrap();
    let target = world.home.join("chosen.noindex");
    let env_target = world.home.join("not-chosen.noindex");
    let env = |name: &str| (name == "CARGO_TARGET_DIR").then(|| env_target.as_os_str().to_owned());
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&launcher),
        env: &env,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let args = vec![
        "--unverified".into(),
        "test".into(),
        format!(
            "--manifest-path={}",
            world.owner.join("Cargo.toml").display()
        ),
        format!("--target-dir={}", target.display()),
    ];
    observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
    assert!(!env_target.exists());
    let recorded = roots(&world.home);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].root, target);
    assert_eq!(recorded[0].owners, [world.owner]);
}

/// A SHORT `-m` AND TARGO'S `TARGO_` TWINS NAME WHAT THEIR LONG AND `CARGO_` SPELLINGS NAME.
/// From a launcher with no manifest, `-m` in each spelling binds the target to the manifest's
/// workspace, and a producer naming an output there that cargo did not make is refused.
/// `TARGO_TARGET_DIR` is the target where `CARGO_TARGET_DIR` is unset, and a build directory
/// in `TARGO_BUILD_BUILD_DIR` owes custody as one in `CARGO_BUILD_BUILD_DIR` does. NEGATIVE
/// CONTROL: `-pm` names no manifest, a set `CARGO_TARGET_DIR` hides its twin, a build
/// directory outside HOME owes none, and an upstream cargo reads no twin.
#[test]
fn a_short_manifest_and_targo_twins_name_what_their_long_spellings_name() {
    use super::super::Tool;
    let words =
        |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_owned()).collect() };
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    for spelling in [
        &["-m", "{m}"][..],
        &["-m{m}"],
        &["-m={m}"],
        &["-qm", "{m}"],
        &["-vrm{m}"],
        &["-pm", "{m}"],
    ] {
        let world = World::new();
        let launcher = world.home.join("launcher");
        std::fs::create_dir(&launcher).unwrap();
        let manifest = world.owner.join("Cargo.toml").display().to_string();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&launcher),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let named = spelling[0] != "-pm";
        let mut args = words(&["--unverified", "test"]);
        args.extend(spelling.iter().map(|word| word.replace("{m}", &manifest)));
        // A folder made by hand, whose outputs atpkg never owns: at most the build
        // directory it places in it (its `.ubd/build.noindex`, recorded as such).
        let made = world.home.join("made-by-hand.noindex");
        std::fs::create_dir(&made).unwrap();
        let into_made = [
            args.clone(),
            vec![format!("--target-dir={}", made.display())],
        ]
        .concat();
        assert_eq!(
            observe_for_producer(&into_made, Tool::Targo, &world.home, &machine, true).is_err(),
            named,
            "{into_made:?}"
        );
        args.push(format!("--target-dir={}", world.target.display()));
        assert_eq!(
            observe_on(&args, Tool::Targo, &world.home, &machine).unwrap(),
            named,
            "{args:?}"
        );
        let recorded = roots(&world.home);
        if named {
            let intermediates = super::super::target_build_path(&made);
            assert!(
                recorded
                    .iter()
                    .all(|r| r.root == world.target || r.root == intermediates),
                "{args:?}"
            );
            let target: Vec<_> = recorded.iter().filter(|r| r.root == world.target).collect();
            assert_eq!(target.len(), 1, "{args:?}");
            assert_eq!(target[0].owners, std::slice::from_ref(&world.owner));
        } else {
            assert!(recorded.is_empty(), "{args:?}");
            assert!(!world.target.exists());
        }
    }

    let build = words(&["--unverified", "build"]);
    let in_owner = |world: &World, env: &dyn Fn(&str) -> Option<std::ffi::OsString>, tool: Tool| {
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = if tool.is_cargo() {
            words(&["build"])
        } else {
            build.clone()
        };
        (
            observe_on(&args, tool, &world.home, &machine).unwrap(),
            roots(&world.home),
        )
    };
    for var in ["TARGO_TARGET_DIR", "TARGO_BUILD_TARGET_DIR"] {
        for tool in [Tool::Targo, Tool::CargoCompat] {
            let world = World::new();
            let twin = |name: &str| (name == var).then(|| world.target.clone().into_os_string());
            let (owned, recorded) = in_owner(&world, &twin, tool);
            assert!(owned, "{var}");
            assert_eq!(recorded.len(), 1, "{var}");
            assert_eq!(recorded[0].root, world.target, "{var}");
        }
    }
    let world = World::new();
    let twin =
        |name: &str| (name == "TARGO_TARGET_DIR").then(|| world.target.clone().into_os_string());
    let (owned, recorded) = in_owner(&world, &twin, Tool::Cargo);
    assert!(!owned && recorded.is_empty());
    assert!(!world.target.exists());
    let world = World::new();
    let spelled = world.home.join("spelled.noindex");
    let both = |name: &str| match name {
        "CARGO_TARGET_DIR" => Some(spelled.clone().into_os_string()),
        "TARGO_TARGET_DIR" => Some(world.target.clone().into_os_string()),
        _ => None,
    };
    let (owned, recorded) = in_owner(&world, &both, Tool::Targo);
    assert!(owned);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].root, spelled);
    assert!(!world.target.exists());

    /// `f` on a machine in `cwd`, under `home`, with `env` and `read`.
    fn on_machine<R>(
        home: &Path,
        cwd: &Path,
        env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
        read: &dyn Fn(&Path) -> Result<Option<String>, ()>,
        f: impl FnOnce(&super::super::Machine<'_>) -> R,
    ) -> R {
        f(&super::super::Machine {
            home: Some(home),
            root: None,
            cwd: Some(cwd),
            env,
            read,
            read_manifest: read,
            writable: &|_| Some(true),
        })
    }
    let world = World::new();
    // `var` set to `dir` in the owner's folder, `extra` after the verb.
    let produce = |var: &'static str, dir: &Path, tool: Tool, extra: &[String]| {
        let env = |name: &str| (name == var).then(|| dir.as_os_str().to_owned());
        let mut args = if tool.is_cargo() {
            words(&["build"])
        } else {
            build.clone()
        };
        args.extend_from_slice(extra);
        on_machine(&world.home, &world.owner, &env, &read, |machine| {
            observe_for_producer(&args, tool, &world.home, machine, true)
        })
    };
    let inside = world.home.join("chosen-build");
    let outside = world.home.parent().unwrap().join("chosen-build");
    let fresh = world.home.join("fresh.noindex");
    let into_fresh = [format!("--target-dir={}", fresh.display())];
    let paired = world.home.join("paired-final.noindex");
    let into_paired = [format!("--target-dir={}", paired.display())];
    for var in [super::super::BUILD_DIR_ENV, super::super::BUILD_DIR_TWIN] {
        assert!(produce(var, &inside, Tool::Targo, &[]).unwrap(), "{var}");
        assert!(world.owner.join("target").is_dir(), "{var}");
        assert!(produce(var, &outside, Tool::Targo, &[]).is_err(), "{var}");
        // Both the workspace default and an explicit final target require paired custody.
        assert!(
            produce(var, &inside, Tool::Targo, &into_paired).unwrap(),
            "{var}"
        );
        assert!(paired.is_dir() && inside.is_dir(), "{var}");
    }
    // Upstream cargo ignores the twin but still admits its already registered default target.
    let ignored = world.home.join("ignored-build");
    assert!(produce(super::super::BUILD_DIR_TWIN, &ignored, Tool::Cargo, &[]).unwrap());
    assert!(!ignored.exists());
    assert!(!outside.exists());
    // A target in `TARGO_TARGET_DIR` that cargo did not make owes custody it cannot have.
    let made = world.home.join("made-by-hand.noindex");
    std::fs::create_dir(&made).unwrap();
    for var in ["TARGO_TARGET_DIR", "TARGO_BUILD_TARGET_DIR"] {
        assert!(produce(var, &made, Tool::Targo, &[]).is_err(), "{var}");
        assert!(produce(var, &made, Tool::Cargo, &[]).unwrap(), "{var}");
        assert!(!made.join(MARKER).exists(), "{var}");
        assert!(
            roots(&world.home)
                .iter()
                .all(|receipt| receipt.root != made)
        );
    }

    // The twins, a `-C` in a cluster and a `TARGO_HOME`'s config each leave a choice the
    // observer cannot resolve, so its caller keeps whole-cache ownership.
    let unresolved = |env: &dyn Fn(&str) -> Option<std::ffi::OsString>, args: &[&str], tool| {
        on_machine(&world.home, &world.owner, env, &read, |machine| {
            super::super::unresolved_directory(&words(args), tool, machine)
        })
    };
    let none = |_: &str| None;
    let twin_build =
        |name: &str| (name == super::super::BUILD_DIR_TWIN).then(|| std::ffi::OsString::from("/b"));
    assert!(!unresolved(&none, &["--unverified", "build"], Tool::Targo));
    assert!(unresolved(
        &twin_build,
        &["--unverified", "build"],
        Tool::Targo
    ));
    assert!(!unresolved(&twin_build, &["build"], Tool::Cargo));
    assert!(unresolved(
        &none,
        &["-qC/x", "--unverified", "build"],
        Tool::Targo
    ));
    assert!(!unresolved(
        &none,
        &["--unverified", "build", "-pC"],
        Tool::Targo
    ));
    let package_home = world.home.parent().unwrap().join("package-home");
    std::fs::create_dir(&package_home).unwrap();
    std::fs::write(
        package_home.join("config.toml"),
        "[build]\ntarget-dir = 't'\n",
    )
    .unwrap();
    let homed = |name: &str| (name == "TARGO_HOME").then(|| package_home.clone().into_os_string());
    assert!(unresolved(&homed, &["--unverified", "build"], Tool::Targo));
    assert!(unresolved(&homed, &["+trust", "build"], Tool::CargoCompat));
    assert!(!unresolved(&homed, &["build"], Tool::Cargo));
    // A `TARGO_HOME` whose config names a build directory: an explicit target is not
    // claimed, as with `CARGO_HOME`.
    std::fs::write(
        package_home.join("config.toml"),
        "[build]\nbuild-dir = 'b'\n",
    )
    .unwrap();
    assert!(produce("TARGO_HOME", &package_home, Tool::Targo, &into_fresh).is_err());
    assert!(produce("TARGO_HOME", &package_home, Tool::CargoCompat, &into_fresh).is_err());
    assert!(!fresh.exists());

    // A registered root named by `TARGO_TARGET_DIR` in a call the observer cannot resolve
    // keeps its owners unknown, as one named by `CARGO_TARGET_DIR` does.
    for (var, tool, unknown) in [
        ("TARGO_TARGET_DIR", Tool::Targo, true),
        ("TARGO_TARGET_DIR", Tool::CargoCompat, true),
        ("TARGO_TARGET_DIR", Tool::Cargo, false),
        ("CARGO_TARGET_DIR", Tool::Cargo, true),
    ] {
        let world = World::new();
        world.build();
        let env = |name: &str| (name == var).then(|| world.target.clone().into_os_string());
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = words(&["--unverified", "an-unknown-alias"]);
        observe_on(&args, tool, &world.home, &machine).unwrap();
        assert_eq!(
            super::read(&world.path()).unwrap().owners_unknown,
            unknown,
            "{var} {tool:?}"
        );
    }
}

/// A CLUSTER'S `-m` IS TARGO'S MANIFEST ONLY AMONG TARGO'S OWN WORDS, in the front as in the
/// placement ([`super::super::short_manifest_scope`]). An alias that ends in `--` hands its
/// words to its program, so `lab linux exec uname -m` keeps its owned target; a `run`'s
/// words after its binary are the program's, so a manifest outside HOME there leaves the
/// target the launcher's package owns; `owner -rmatt` removes the owner `matt` and names no
/// manifest, so a folder with no package owes no custody. A user alias of a builtin alias's
/// name (`rm`, `b`), in a config, `CARGO_ALIAS_R` or `TARGO_ALIAS_C`, takes the name and is
/// read through its words: `run -q --` hands the `-m` to its program, `build --release`
/// keeps it Targo's, from a folder with no package or another package's too, and words the
/// front cannot tell (an `include` Targo refuses, text it cannot parse) are refused where
/// either reading of the `-m` owes custody, as the launcher's package does. The alias comes from
/// the configs of the directory a `-C` names, as the kernel resolves it, and the package
/// homes resolved there, where the call is judged too. A short option's value left to the
/// next word (`-vZ script`, `-C dir`, `-qC dir`) is no verb, `update`'s `-b` takes none, and
/// an optional value (`package -p`, `tree -i`) is no word that starts with `-`, so the `-m`
/// or `--target-dir` after them is read as the long spelling is; a trailing verb's long
/// option takes its value (`--package demo`, `--jobs -1`, a digit taking none). NEGATIVE
/// CONTROL: before the program's words the cluster is
/// Targo's, and a manifest outside HOME owes none, as it does after a builtin alias no user
/// alias takes (an alias of another name proves nothing) and after a `--config` alias, which
/// Targo merges only after it has expanded the verb.
#[test]
fn the_front_reads_a_short_manifest_only_among_targos_own_words() {
    use super::super::Tool;
    let words =
        |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_owned()).collect() };
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let outside = |world: &World| {
        world
            .home
            .parent()
            .unwrap()
            .join("elsewhere/Cargo.toml")
            .display()
            .to_string()
    };
    // `args` from `cwd`, the target in `CARGO_TARGET_DIR` and `more` set: the producer's
    // verdict, and the roots recorded.
    let produce_with = |world: &World, cwd: &Path, more: &[(&str, &str)], args: &[String]| {
        let env = |name: &str| {
            (name == "CARGO_TARGET_DIR")
                .then(|| world.target.clone().into_os_string())
                .or_else(|| {
                    more.iter()
                        .find(|(key, _)| *key == name)
                        .map(|(_, value)| value.into())
                })
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(cwd),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let verdict = observe_for_producer(args, Tool::Targo, &world.home, &machine, true).ok();
        (verdict, roots(&world.home).len())
    };
    let produce = |world: &World, cwd: &Path, args: &[String]| produce_with(world, cwd, &[], args);

    let world = World::new();
    std::fs::create_dir(world.owner.join(".cargo")).unwrap();
    std::fs::write(
        world.owner.join(".cargo/config.toml"),
        "[alias]\nlab = \"run -q -p aterm-lab --\"\n",
    )
    .unwrap();
    let lab = words(&["--unverified", "lab", "linux", "exec", "uname", "-m"]);
    assert_eq!(produce(&world, &world.owner, &lab), (Some(true), 1));

    let world = World::new();
    let mut run = words(&["--unverified", "run", "demo", "-m"]);
    run.push(outside(&world));
    assert_eq!(produce(&world, &world.owner, &run), (Some(true), 1));
    let world = World::new();
    let mut before_program = words(&["--unverified", "run", "-m"]);
    before_program.push(outside(&world));
    before_program.push("demo".to_owned());
    assert_eq!(
        produce(&world, &world.owner, &before_program),
        (Some(false), 0)
    );
    assert!(!world.target.exists());

    // An upstream cargo's `+toolchain` leads its words and is no verb: its `-m` after
    // `build` names a manifest outside HOME, which owes no custody.
    let world = World::new();
    let env =
        |name: &str| (name == "CARGO_TARGET_DIR").then(|| world.target.clone().into_os_string());
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &env,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let mut cargo = words(&["+stable", "build", "-m"]);
    cargo.push(outside(&world));
    assert!(!requires_managed_explicit(
        &cargo,
        Tool::Cargo,
        &world.home,
        &machine
    ));
    assert!(requires_managed_explicit(
        &cargo[..2],
        Tool::Cargo,
        &world.home,
        &machine
    ));

    // A user alias takes a builtin alias's name, wherever Targo reads it; words the front
    // cannot tell (an `include` that names nothing, which Targo refuses, or text this reader
    // cannot parse) leave the `-m` Targo's or the program's, and the launcher's package owes
    // custody of the target, so the call is refused.
    for (verb, config, more, want) in [
        (
            "rm",
            "[alias]\nrm = \"run -q --\"\n",
            &[][..],
            (Some(true), 1),
        ),
        (
            "b",
            "alias.b = [\"run\", \"-q\", \"--\"]\n",
            &[],
            (Some(true), 1),
        ),
        ("r", "", &[("CARGO_ALIAS_R", "run -q --")], (Some(true), 1)),
        ("c", "", &[("TARGO_ALIAS_C", "run -q --")], (Some(true), 1)),
        ("d", "include = [\"aliases.toml\"]\n", &[], (None, 0)),
        (
            "rm",
            "[alias]\nrm = \"run -q --\"\nbroken =\n",
            &[],
            (None, 0),
        ),
    ] {
        let world = World::new();
        if !config.is_empty() {
            std::fs::create_dir(world.owner.join(".cargo")).unwrap();
            std::fs::write(world.owner.join(".cargo/config.toml"), config).unwrap();
        }
        let mut args = words(&["--unverified", verb, "-m"]);
        args.push(outside(&world));
        assert_eq!(
            produce_with(&world, &world.owner, more, &args),
            want,
            "{verb} {config:?}"
        );
    }
    let world = World::new();
    let mut args = words(&["--unverified", "--config", "alias.t='run -q --'", "t", "-m"]);
    args.push(outside(&world));
    assert_eq!(produce(&world, &world.owner, &args), (Some(false), 0));
    assert!(!world.target.exists());
    let world = World::new();
    std::fs::create_dir(world.owner.join(".cargo")).unwrap();
    std::fs::write(
        world.owner.join(".cargo/config.toml"),
        "[alias]\nbb = \"run -q --\"\n",
    )
    .unwrap();
    let mut args = words(&["--unverified", "b", "-m"]);
    args.push(outside(&world));
    assert_eq!(produce(&world, &world.owner, &args), (Some(false), 0));
    assert!(!world.target.exists());

    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let owner = words(&["owner", "-rmatt", "mycrate"]);
    assert_eq!(produce(&world, &notes, &owner), (Some(false), 0));
    assert!(!world.target.exists());
    // From a folder with no package, a user alias's `-m` names no manifest either.
    std::fs::create_dir(notes.join(".cargo")).unwrap();
    std::fs::write(
        notes.join(".cargo/config.toml"),
        "[alias]\nb = \"run -q --\"\n",
    )
    .unwrap();
    let mut aliased = words(&["--unverified", "b", "-m"]);
    aliased.push(world.owner.join("Cargo.toml").display().to_string());
    assert_eq!(produce(&world, &notes, &aliased), (Some(false), 0));
    std::fs::remove_dir_all(notes.join(".cargo")).unwrap();
    assert!(!world.target.exists());

    // Every verb that takes `--manifest-path` takes `-m`: a resolving verb's embedded
    // manifest puts its lock root in the target, and `publish` verifies its package there,
    // so each owes the custody its manifest names, from a folder with no package too.
    let script = world.owner.join("tool.rs").display().to_string();
    let manifest = world.owner.join("Cargo.toml").display().to_string();
    let packed = format!("-qm{script}");
    let owner_dir = world.owner.display().to_string();
    for args in [
        words(&["-Zscript", "fetch", "-m", &script]),
        words(&["-Zscript", "fetch", "--manifest-path", &script]),
        words(&["-Zscript", "tree", &packed]),
        words(&["-vZ", "script", "fetch", "-m", &script]),
        words(&[
            "-Zunstable-options",
            "-C",
            &owner_dir,
            "fetch",
            "-m",
            &script,
        ]),
        words(&[
            "-Zunstable-options",
            "-qC",
            &owner_dir,
            "fetch",
            "-m",
            &script,
        ]),
        words(&["-Zscript", "-Zunstable-options", "update", "-bm", &script]),
        words(&["-Zscript", "tree", "-i", "-m", &script, "-p", "tool"]),
        words(&[
            "--unverified",
            "publish",
            "--dry-run",
            "-p",
            "-m",
            &manifest,
        ]),
        words(&["--unverified", "publish", "--dry-run", "-m", &manifest]),
        words(&[
            "--unverified",
            "publish",
            "--dry-run",
            "--manifest-path",
            &manifest,
        ]),
    ] {
        assert_eq!(produce(&world, &notes, &args), (None, 0), "{args:?}");
    }
    assert!(!world.target.exists());

    // An alias that hands its words to Targo keeps its `-m` Targo's: the target is the
    // named package's, from a folder with no package and from another package's.
    let owners = |world: &World| -> Vec<PathBuf> {
        roots(&world.home)
            .into_iter()
            .flat_map(|root| root.owners)
            .map(|owner| std::fs::canonicalize(&owner).unwrap_or(owner))
            .collect()
    };
    for launcher in ["notes", "another-source"] {
        let world = World::new();
        let cwd = if launcher == "notes" {
            let notes = world.home.join("notes");
            std::fs::create_dir(&notes).unwrap();
            notes
        } else {
            world.another_owner()
        };
        std::fs::create_dir(world.home.join(".cargo")).unwrap();
        std::fs::write(
            world.home.join(".cargo/config.toml"),
            "[alias]\nb = \"build --release\"\n",
        )
        .unwrap();
        let mut built = words(&["--unverified", "b", "-m"]);
        built.push(world.owner.join("Cargo.toml").display().to_string());
        assert_eq!(produce(&world, &cwd, &built), (Some(true), 1), "{launcher}");
        assert_eq!(
            owners(&world),
            [std::fs::canonicalize(&world.owner).unwrap()],
            "{launcher}"
        );
    }
    // `package -p` takes no `-m` for its optional value, so the target is the named
    // package's, from a folder with no package too.
    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let named = world.owner.join("Cargo.toml").display().to_string();
    let packaged = words(&["--unverified", "package", "-p", "-m", &named]);
    assert_eq!(produce(&world, &notes, &packaged), (Some(true), 1));
    assert_eq!(
        owners(&world),
        [std::fs::canonicalize(&world.owner).unwrap()]
    );

    // Targo reads the alias in the directory a `-C` names, and the call is judged there: its
    // alias hands the `-m` to the program, so the target is that directory's package's.
    let world = World::new();
    let launcher = world.another_owner();
    std::fs::create_dir(world.owner.join(".cargo")).unwrap();
    std::fs::write(
        world.owner.join(".cargo/config.toml"),
        "[alias]\nb = \"run -q --\"\n",
    )
    .unwrap();
    let owner_dir = world.owner.display().to_string();
    let mut moved = words(&[
        "-Zunstable-options",
        "-C",
        &owner_dir,
        "--unverified",
        "b",
        "-m",
    ]);
    moved.push(outside(&world));
    assert_eq!(produce(&world, &launcher, &moved), (Some(true), 1));
    assert_eq!(owners(&world), std::slice::from_ref(&world.owner));
    // A `-C` that names the package builds it into its target, from a folder with none.
    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let owner_dir = world.owner.display().to_string();
    let moved = words(&[
        "-Zunstable-options",
        "-C",
        &owner_dir,
        "--unverified",
        "build",
    ]);
    assert_eq!(produce(&world, &notes, &moved), (Some(true), 1));
    assert_eq!(owners(&world), std::slice::from_ref(&world.owner));
    // The directory is the kernel's: one with no package owes nothing, one Targo cannot
    // change into owes custody, and a link's `..` is its target's parent, the package here.
    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    std::fs::create_dir(world.home.join("empty")).unwrap();
    let moved = words(&[
        "-Zunstable-options",
        "-C",
        "../empty",
        "--unverified",
        "build",
    ]);
    assert_eq!(produce(&world, &notes, &moved), (Some(false), 0));
    let moved = words(&["-Zunstable-options", "-C", "../ws", "--unverified", "build"]);
    assert_eq!(produce(&world, &notes, &moved), (None, 0));
    assert!(!world.target.exists());
    std::fs::create_dir(world.owner.join("sub")).unwrap();
    std::os::unix::fs::symlink(world.owner.join("sub"), notes.join("linked")).unwrap();
    let moved = words(&[
        "-Zunstable-options",
        "-C",
        "linked/..",
        "--unverified",
        "build",
    ]);
    assert_eq!(produce(&world, &notes, &moved), (Some(true), 1));
    assert_eq!(owners(&world), std::slice::from_ref(&world.owner));
    // A relative package home is the `-C` directory's, as Targo resolves it there: its
    // alias hands the `-m` to the program, so the target is the package's. With none there,
    // `b` builds the manifest outside HOME, which owes no custody.
    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    std::fs::create_dir_all(world.owner.join("hh")).unwrap();
    std::fs::write(
        world.owner.join("hh/config.toml"),
        "[alias]\nb = \"run -q --\"\n",
    )
    .unwrap();
    let owner_dir = world.owner.display().to_string();
    let mut moved = words(&[
        "-Zunstable-options",
        "-C",
        &owner_dir,
        "--unverified",
        "b",
        "-m",
    ]);
    moved.push(outside(&world));
    assert_eq!(produce(&world, &notes, &moved), (Some(false), 0));
    assert!(!world.target.exists());
    assert_eq!(
        produce_with(&world, &notes, &[("CARGO_HOME", "hh")], &moved),
        (Some(true), 1)
    );
    assert_eq!(owners(&world), std::slice::from_ref(&world.owner));

    // An optional value takes no word that starts with `-`, so the `--target-dir` after
    // `package -p` is Targo's; a trailing verb's program words are the program's, a
    // `--target-dir` among them too, so a run of `demo` names none.
    let world = World::new();
    let chosen = world.home.join("chosen");
    let packaged = words(&[
        "--unverified",
        "package",
        "-p",
        "--target-dir",
        &chosen.display().to_string(),
    ]);
    assert_eq!(produce(&world, &world.owner, &packaged), (Some(true), 1));
    assert_eq!(
        roots(&world.home)[0].root,
        std::fs::canonicalize(&chosen).unwrap()
    );
    let world = World::new();
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let program = words(&["--unverified", "run", "demo", "-o", "--target-dir", "sub"]);
    assert_eq!(
        observe_for_producer(&program, Tool::Targo, &world.home, &machine, true).ok(),
        Some(false)
    );
    assert!(roots(&world.home).is_empty());
    // `-j` takes even a word that starts with `-`: the reader that owes custody skips it as
    // the one that grants it does, so neither names a target.
    let world = World::new();
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let program = words(&["--unverified", "run", "demo", "-j", "--target-dir", "sub"]);
    assert_eq!(
        observe_for_producer(&program, Tool::Targo, &world.home, &machine, true).ok(),
        Some(false)
    );
    // A `-C` whose directory the front cannot resolve owes custody.
    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let blank = words(&["-Zunstable-options", "-C=", "--unverified", "build"]);
    assert_eq!(produce(&world, &notes, &blank), (None, 0));

    // A trailing verb's long option takes the word clap gives it: `--package demo` keeps the
    // `-m` after it Targo's, and `--jobs -1` the program after it the program's (a digit
    // takes no value).
    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let named = world.owner.join("Cargo.toml").display().to_string();
    let run = words(&["--unverified", "run", "--package", "demo", "-m", &named]);
    assert_eq!(produce(&world, &notes, &run), (Some(true), 1));
    assert_eq!(
        owners(&world),
        [std::fs::canonicalize(&world.owner).unwrap()]
    );
    let world = World::new();
    let mut run = words(&["--unverified", "run", "--jobs", "-1", "demo", "-m"]);
    run.push(outside(&world));
    assert_eq!(produce(&world, &world.owner, &run), (Some(true), 1));
    let world = World::new();
    let mut run = words(&["--unverified", "run", "-1", "demo", "-m"]);
    run.push(outside(&world));
    assert_eq!(produce(&world, &world.owner, &run), (Some(true), 1));
}

/// THE FRONT JUDGES THE LINE TARGO RUNS ([`super::super::targo_line`]): an alias's own
/// `--manifest-path` names the package, and its `--target-dir` the target, from a folder with
/// no package too; a `--` before the verb hands Targo the alias after it; an alias's name
/// is read under the key Targo reads, split at its dots and upper-cased as Unicode is.
/// NEGATIVE CONTROL: a name Targo may run as a manifest and a dash word after such a `--` are
/// lines the front cannot tell, so the call is refused.
#[test]
fn the_front_judges_the_line_targo_runs() {
    use super::super::Tool;
    let words =
        |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_owned()).collect() };
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let produce = |world: &World, cwd: &Path, more: &[(&str, String)], args: &[String]| {
        let env = |name: &str| {
            (name == "CARGO_TARGET_DIR")
                .then(|| world.target.clone().into_os_string())
                .or_else(|| {
                    more.iter()
                        .find(|(key, _)| *key == name)
                        .map(|(_, value)| value.into())
                })
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(cwd),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let verdict = observe_for_producer(args, Tool::Targo, &world.home, &machine, true).ok();
        (verdict, roots(&world.home).len())
    };
    let owned = |world: &World| -> Vec<PathBuf> {
        roots(&world.home)
            .into_iter()
            .flat_map(|root| root.owners)
            .map(|owner| std::fs::canonicalize(&owner).unwrap_or(owner))
            .collect()
    };
    let fresh = || {
        let world = World::new();
        let notes = world.home.join("notes");
        std::fs::create_dir_all(notes.join(".cargo")).unwrap();
        let manifest = world.owner.join("Cargo.toml").display().to_string();
        (world, notes, manifest)
    };
    // Each alias names the package in its words, from a folder with no package.
    for (config, more, args) in [
        (
            "[alias]\nxb = \"build --manifest-path {m}\"\n",
            None,
            "--unverified xb",
        ),
        (
            "[alias.x]\ny = \"build --manifest-path {m}\"\n",
            None,
            "--unverified x.y",
        ),
        ("", Some("CARGO_ALIAS_BÉ"), "--unverified bé"),
        ("", Some("CARGO_ALIAS_XB"), "--unverified -- xb"),
        ("", Some("CARGO_ALIAS_XB"), "--unverified -- b -m {m}"),
    ] {
        let (world, notes, manifest) = fresh();
        std::fs::write(
            notes.join(".cargo/config.toml"),
            config.replace("{m}", &manifest),
        )
        .unwrap();
        let more: Vec<(&str, String)> = more
            .map(|key| (key, format!("build --manifest-path {manifest}")))
            .into_iter()
            .collect();
        let args: Vec<String> = args
            .split_whitespace()
            .map(|word| word.replace("{m}", &manifest))
            .collect();
        assert_eq!(
            produce(&world, &notes, &more, &args),
            (Some(true), 1),
            "{args:?}"
        );
        assert_eq!(
            owned(&world),
            [std::fs::canonicalize(&world.owner).unwrap()],
            "{args:?}"
        );
    }
    // An alias's `--target-dir` is the target, over the variable.
    let (world, _, _) = fresh();
    let chosen = world.home.join("chosen");
    let more = [(
        "CARGO_ALIAS_XB",
        format!("build --target-dir {}", chosen.display()),
    )];
    let args = words(&["--unverified", "xb"]);
    assert_eq!(produce(&world, &world.owner, &more, &args), (Some(true), 1));
    assert_eq!(
        roots(&world.home)[0].root,
        std::fs::canonicalize(&chosen).unwrap()
    );
    assert!(!world.target.exists());
    // NEGATIVE CONTROL: Targo may run `x.rs` as a manifest, so that line cannot be told (a
    // `--` before a word that starts with `-` is
    // `the_front_judges_each_word_where_targo_reads_it`'s).
    let (world, _, _) = fresh();
    std::fs::create_dir_all(world.owner.join(".cargo")).unwrap();
    std::fs::write(
        world.owner.join(".cargo/config.toml"),
        "[alias.x]\nrs = \"build\"\nr = \"build\"\n",
    )
    .unwrap();
    assert_eq!(
        produce(&world, &world.owner, &[], &words(&["--unverified", "x.rs"])),
        (None, 0)
    );
    // Nor does a line Targo refuses grant one: an alias's own `--unverified` (which Targo
    // takes only from the words typed) and an external command.
    for (more, args) in [
        (&[("CARGO_ALIAS_XU", "build --unverified")][..], &["xu"][..]),
        (&[], &["--unverified", "nextest", "run"]),
    ] {
        let more: Vec<(&str, String)> = more
            .iter()
            .map(|(key, value)| (*key, (*value).to_owned()))
            .collect();
        assert_eq!(
            produce(&world, &world.owner, &more, &words(args)),
            (None, 0),
            "{args:?}"
        );
    }
    assert!(!world.target.exists());
    let args = words(&["--unverified", "x.r"]);
    assert_eq!(produce(&world, &world.owner, &[], &args), (Some(true), 1));
    // A key whose path meets a value that is no table is one Targo refuses to read.
    let (world, notes, manifest) = fresh();
    std::fs::write(notes.join(".cargo/config.toml"), "[alias]\nx = \"build\"\n").unwrap();
    let mut args = words(&["--unverified", "x.y", "-m"]);
    args.push(manifest);
    assert_eq!(produce(&world, &notes, &[], &args), (None, 0));
    // Where the line Targo runs grants no custody (no lane), the target it names is still the
    // one protected.
    let (world, _, _) = fresh();
    world.build();
    let machine_env = |name: &str| {
        (name == "CARGO_ALIAS_XB")
            .then(|| format!("build --target-dir {}", world.target.display()).into())
    };
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &machine_env,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let args = words(&["xb"]);
    assert!(!observe_for_producer(&args, Tool::Targo, &world.home, &machine, false).unwrap());
    assert!(roots(&world.home)[0].owners_unknown);
    // An unresolved included config may select an intermediate root the front cannot mark,
    // so it refuses before an observer or compiler can mutate anything. A known config with
    // no explicit intermediate choice remains an observation that grants no custody.
    for (config, unknown) in [("include = [\"aliases.toml\"]\n", true), ("", false)] {
        let (world, _, _) = fresh();
        world.build();
        let other = world.another_owner();
        std::fs::create_dir_all(other.join(".cargo")).unwrap();
        std::fs::write(other.join(".cargo/config.toml"), config).unwrap();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&other),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = words(&["--unverified", "xd"]);
        let before = std::fs::read(world.path()).unwrap();
        let observed = observe_for_producer(&args, Tool::Targo, &world.home, &machine, false);
        if unknown {
            assert_eq!(
                observed.unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput
            );
            assert_eq!(std::fs::read(world.path()).unwrap(), before);
        } else {
            assert!(!observed.unwrap());
        }
        assert!(!roots(&world.home)[0].owners_unknown, "{config:?}");
    }
}

/// A `-C` BEFORE THE VERB IS WHERE TARGO BUILDS: it changes into that directory, as the
/// kernel resolves it from the launcher's (a link's `..` is its target's parent), before it
/// reads a config, an alias, a relative path or a relative package home, so the front reads
/// the same call there without its `-C` and owns the target it names, recorded for that
/// directory's package. An alias's own `-C` Targo ignores, and the front with it. Only
/// behind its gate (`-Zunstable-options` among the `-Z` values clap keeps, on a channel that
/// allows it). NEGATIVE CONTROL: each row fails read from the launcher (its config, alias,
/// `--config` file, package home or manifest, a link's `..` read lexically); a `-C` Targo
/// refuses for want of its gate, a second one, which clap refuses, one the front cannot
/// resolve, an alias's that clap refuses and the lint's grant nothing, and the target each
/// names owes custody, so the call is refused, from the package itself too; a `--config` file
/// or a build directory only the `-C` directory has leaves its target ungranted.
#[test]
fn a_directory_before_the_verb_is_where_targo_builds() {
    use super::super::Tool;
    let words =
        |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_owned).collect() };
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    // `line` from `cwd` with `more` set, by `tool`: the producer's verdict, and the roots
    // recorded.
    let produce_by = |tool: Tool, world: &World, cwd: &Path, more: &[(&str, &str)], line: &str| {
        let env = |name: &str| {
            more.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.into())
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(cwd),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let verdict = observe_for_producer(&words(line), tool, &world.home, &machine, true).ok();
        (verdict, roots(&world.home).len())
    };
    let produce = |world: &World, cwd: &Path, more: &[(&str, &str)], line: &str| {
        produce_by(Tool::Targo, world, cwd, more, line)
    };
    let config = |dir: &Path, text: &str| {
        std::fs::create_dir_all(dir.join(".cargo")).unwrap();
        std::fs::write(dir.join(".cargo/config.toml"), text).unwrap();
    };
    let root = |world: &World| {
        roots(&world.home)
            .into_iter()
            .map(|root| root.root)
            .collect::<Vec<_>>()
    };
    let owners = |world: &World| -> Vec<PathBuf> {
        roots(&world.home)
            .into_iter()
            .flat_map(|root| root.owners)
            .map(|owner| std::fs::canonicalize(&owner).unwrap_or(owner))
            .collect()
    };
    // A world whose launcher is `notes`, beside the package `source` (the owner).
    let launched = || {
        let world = World::new();
        let notes = world.home.join("notes");
        std::fs::create_dir(&notes).unwrap();
        (world, notes)
    };
    let measured = "-Zunstable-options -C ../source --unverified build --target-dir fresh";

    // The measured call, and its clustered spelling.
    for line in [
        measured,
        "-Zunstable-options -qC../source --unverified build --target-dir fresh",
    ] {
        let (world, notes) = launched();
        assert_eq!(
            produce(&world, &notes, &[], line),
            (Some(true), 1),
            "{line}"
        );
        assert_eq!(root(&world), [world.owner.join("fresh")], "{line}");
        assert_eq!(owners(&world), std::slice::from_ref(&world.owner), "{line}");
        assert!(!notes.join("fresh").exists(), "{line}");
    }
    // A relative `CARGO_TARGET_DIR` is the `-C` directory's.
    let (world, notes) = launched();
    let call = "-Zunstable-options -C ../source --unverified build";
    assert_eq!(
        produce(&world, &notes, &[("CARGO_TARGET_DIR", "envout")], call),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [world.owner.join("envout")]);
    // A configured target is the `-C` directory's config's, never the launcher's.
    let (world, notes) = launched();
    config(&world.owner, "[build]\ntarget-dir = \"cfgout\"\n");
    config(&notes, "[build]\ntarget-dir = \"wrong\"\n");
    assert_eq!(produce(&world, &notes, &[], call), (Some(true), 1));
    assert_eq!(root(&world), [world.owner.join("cfgout")]);
    assert!(!notes.join("wrong").exists());
    // A link's `..` is its target's parent.
    let (world, notes) = launched();
    std::fs::create_dir(world.owner.join("sub")).unwrap();
    std::os::unix::fs::symlink(world.owner.join("sub"), notes.join("linked")).unwrap();
    let linked = "-Zunstable-options -C linked/.. --unverified build --target-dir fresh";
    assert_eq!(produce(&world, &notes, &[], linked), (Some(true), 1));
    assert_eq!(root(&world), [world.owner.join("fresh")]);
    assert!(!notes.join("fresh").exists());
    // A relative package home is the `-C` directory's: its config names the target, a
    // config the launcher's home of that name would not.
    let (world, notes) = launched();
    std::fs::create_dir_all(world.owner.join("hh")).unwrap();
    std::fs::write(
        world.owner.join("hh/config.toml"),
        "[build]\ntarget-dir = \"homeout\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(notes.join("hh")).unwrap();
    std::fs::write(
        notes.join("hh/config.toml"),
        "[build]\ntarget-dir = \"wrong\"\n",
    )
    .unwrap();
    assert_eq!(
        produce(&world, &notes, &[("CARGO_HOME", "hh")], call),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [world.owner.join("homeout")]);
    assert!(!notes.join("wrong").exists());
    // An alias's own `-C` Targo ignores: the target is the launcher's.
    let world = World::new();
    let another = world.another_owner();
    config(
        &world.owner,
        "[alias]\nob = \"-C ../another-source build\"\n",
    );
    assert_eq!(
        produce(
            &world,
            &world.owner,
            &[],
            "--unverified ob --target-dir fresh"
        ),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [world.owner.join("fresh")]);
    assert!(!another.join("fresh").exists());
    // The alias is the `-C` directory's, never the launcher's of the same name.
    let (world, notes) = launched();
    config(&notes, "[alias]\nob = \"build --target-dir wrong\"\n");
    config(&world.owner, "[alias]\nob = \"build --target-dir right\"\n");
    assert_eq!(
        produce(
            &world,
            &notes,
            &[],
            "-Zunstable-options -C ../source --unverified ob"
        ),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [world.owner.join("right")]);
    assert!(!notes.join("wrong").exists() && !world.owner.join("wrong").exists());
    // A relative manifest is the `-C` directory's, from a launcher with none.
    let (world, notes) = launched();
    let manifest = "-Zunstable-options -C ../source --unverified build --manifest-path \
                    Cargo.toml --target-dir fresh";
    assert_eq!(produce(&world, &notes, &[], manifest), (Some(true), 1));
    assert_eq!(owners(&world), std::slice::from_ref(&world.owner));
    // A build directory only the launcher's config names is no config Targo reads there.
    let (world, notes) = launched();
    config(&notes, "[build]\nbuild-dir = \"hidden\"\n");
    assert_eq!(produce(&world, &notes, &[], measured), (Some(true), 1));
    assert_eq!(root(&world), [world.owner.join("fresh")]);
    // An upstream cargo's `+toolchain` leads the words, the `-C` after it: a nightly cargo
    // honours it, and a stable one under `RUSTC_BOOTSTRAP=1`, whose channel is `dev`. One
    // whose channel this reader cannot tell (`+trust` by the cargo name, a custom name
    // rustup may link to a nightly cargo, or no toolchain) checks the gate before it changes
    // directory or reads anything (`cli.rs`'s `main`), so it refuses the call or builds
    // from the `-C` directory, and is read there.
    let bootstrap = [("RUSTC_BOOTSTRAP", "1")];
    for (toolchain, more) in [
        ("+nightly", &[][..]),
        ("+stable", &bootstrap),
        ("+trust", &[]),
        ("+1dev", &[]),
        ("+stable-mine", &[]),
        ("", &[]),
    ] {
        let (world, notes) = launched();
        let upstream =
            format!("{toolchain} -Zunstable-options -C ../source build --target-dir fresh");
        assert_eq!(
            produce_by(Tool::Cargo, &world, &notes, more, &upstream),
            (Some(true), 1),
            "{upstream}"
        );
        assert_eq!(root(&world), [world.owner.join("fresh")], "{upstream}");
    }
    // A builtin verb's own `-Z` values are the ones clap keeps.
    let (world, notes) = launched();
    let late = "-C ../source --unverified build -Zunstable-options --target-dir fresh";
    assert_eq!(produce(&world, &notes, &[], late), (Some(true), 1));
    assert_eq!(root(&world), [world.owner.join("fresh")]);

    // NEGATIVE CONTROL: Targo refuses a `-C` and builds nothing where the `-Z` values clap
    // keeps lack `unstable-options` (none typed; a builtin verb's own replacing those before
    // it; one after an alias's name, which the gate does not parse) or the channel allows no
    // nightly feature (a stable cargo's, a beta one's or a release number's).
    // The front grants nothing for it, and the target the call names in the `-C` directory
    // owes custody, so the call is refused, as it fails closed wherever the `-C` is unread.
    let gated = [
        (
            Tool::Targo,
            "-C ../source --unverified build --target-dir fresh",
        ),
        (
            Tool::Targo,
            "-Zunstable-options -C ../source --unverified build -Zno-index-update \
             --target-dir fresh",
        ),
        (
            Tool::Targo,
            "-C ../source --unverified b -Zunstable-options --target-dir fresh",
        ),
        (
            Tool::Cargo,
            "+stable -Zunstable-options -C ../source build --target-dir fresh",
        ),
        (
            Tool::Cargo,
            "+beta -Zunstable-options -C ../source build --target-dir fresh",
        ),
        (
            Tool::Cargo,
            "+1.91.0 -Zunstable-options -C ../source build --target-dir fresh",
        ),
        (
            Tool::Cargo,
            "+stable-2026-10-01 -Zunstable-options -C ../source build --target-dir fresh",
        ),
        (Tool::Cargo, "-C ../source build --target-dir fresh"),
        (Tool::Cargo, "+trust -C ../source build --target-dir fresh"),
    ];
    for (tool, line) in gated {
        let (world, notes) = launched();
        assert_eq!(
            produce_by(tool, &world, &notes, &[], line),
            (None, 0),
            "{line}"
        );
        assert!(!world.owner.join("fresh").exists(), "{line}");
        assert!(!notes.join("fresh").exists(), "{line}");
    }
    // A second `-C`, the same directory too, which clap refuses, grants nothing.
    let (world, notes) = launched();
    let twice =
        "-Zunstable-options -C ../source -C ../source --unverified build --target-dir fresh";
    assert_eq!(produce(&world, &notes, &[], twice), (None, 0));
    assert!(!world.owner.join("fresh").exists());
    // Launched from the package itself, a `-C` the front cannot resolve, or one Targo
    // refuses for want of its gate, grants nothing: never the launcher's own target.
    for line in [
        "-Zunstable-options -C . -C . --unverified build --target-dir fresh",
        "-Zunstable-options -C missing --unverified build --target-dir fresh",
        "-Zunstable-options -C= --unverified build --target-dir fresh",
        "-C . --unverified build --target-dir fresh",
    ] {
        let world = World::new();
        assert_eq!(
            produce(&world, &world.owner, &[], line),
            (None, 0),
            "{line}"
        );
        assert!(!world.owner.join("fresh").exists(), "{line}");
    }
    // An alias whose words hold a `-C` clap refuses when it parses them again (a second
    // one, one with no value) builds nothing, and a call through it is as wary as one whose
    // alias is unknown: the target it may name owes custody, so the call is refused.
    for alias in ["-C ../a -C ../b build", "-C= build", "-C -q build"] {
        let world = World::new();
        config(&world.owner, &format!("[alias]\nob = \"{alias}\"\n"));
        let line = "--unverified ob --target-dir fresh";
        assert_eq!(
            produce(&world, &world.owner, &[], line),
            (None, 0),
            "{alias}"
        );
        assert!(!world.owner.join("fresh").exists(), "{alias}");
    }
    // The lint's words are its own: a `-C` among them grants nothing, behind its gate too,
    // and the target it names owes custody, so the call is refused.
    let (world, notes) = launched();
    let lint = "-Zunstable-options -C ../source --target-dir fresh";
    assert_eq!(
        produce_by(Tool::Tippy, &world, &notes, &[], lint),
        (None, 0)
    );
    assert!(!world.owner.join("fresh").exists());
    // A build directory only the `-C` directory's config names is that directory's config's,
    // against its definition base: the call's target and it are each owned for its package.
    let (world, notes) = launched();
    config(&world.owner, "[build]\nbuild-dir = \"hidden\"\n");
    assert_eq!(produce(&world, &notes, &[], measured), (Some(true), 2));
    let mut owned = root(&world);
    owned.sort();
    assert_eq!(
        owned,
        [world.owner.join("fresh"), world.owner.join("hidden")]
    );
    assert!(!notes.join("hidden").exists());
    // A `--config` value that names a file only in the `-C` directory is that file's config,
    // whose build directory is read against its definition base, the folder above the file's:
    // the call's target and it are each owned (read as the inline value, the build directory
    // was unrecorded).
    let (world, notes) = launched();
    std::fs::write(
        world.owner.join("build.jobs=1"),
        "[build]\nbuild-dir = \"hidden\"\n",
    )
    .unwrap();
    let filed = "-Zunstable-options -C ../source --config build.jobs=1 --unverified build \
                 --target-dir fresh";
    assert_eq!(produce(&world, &notes, &[], filed), (Some(true), 2));
    let mut owned = root(&world);
    owned.sort();
    let mut both = vec![world.owner.join("fresh"), world.home.join("hidden")];
    both.sort();
    assert_eq!(owned, both);
}

/// THE OWNER IS READ FROM EACH MANIFEST AS TARGO READS IT, OR THE CALL IS REFUSED. Read by
/// the front's own reader ([`live_read`]), as in production: the package's manifest is the
/// first that stands from where Targo works up, through a final link (`exists`), and one
/// that stands and cannot be read is no reason to take a later one; each manifest on the
/// way to the root is read through a final link (`paths::read`), one that does not stand (a
/// link that names nothing) passed; a `[project]` is a package, the `[package]` beside one
/// taken first, and names its workspace as a package does, a root that holds it as a member.
/// A root whose manifest is a link is owned under the folder the link stands in. One that
/// stands and cannot be read (no UTF-8, a folder, past the 1 MiB bound) or that Targo
/// refuses (a `workspace` key that is no string, a package that names a workspace beside its
/// own `[workspace]`, a pointer to a root that does not hold the package) tells no owner:
/// the call whose target a config names is refused and nothing is recorded. NEGATIVE
/// CONTROL: before, the member's own manifest through a link, or past the bound, was
/// passed for its plain parent package's, which owned the target; a root through a link was
/// refused (creation took no link); a `[package]` beside a `[project]` was refused; and a
/// pointer to a root that does not hold the package was owned by it; and a member whose
/// manifest links outside HOME was judged by the link's target's folder, owing no custody,
/// so a target it could not own was left to Targo unrecorded. A manifest read outside HOME
/// through a link is no reading ([`workspace`]): such a member is refused.
#[test]
fn the_owner_is_read_from_each_manifest_as_targo_reads_it_or_the_call_is_refused() {
    use super::super::Tool;
    const ROOT: &str = "[workspace]\nmembers = [\"member\"]\n";
    const PACKAGE: &str = "[package]\nname = \"m\"\n";
    // A workspace at the world's owner whose member's config names the world's target; the
    // member's manifest is `package`, and `ancestor` lays the root's.
    let produce = |package: &str, ancestor: &dyn Fn(&Path)| {
        let world = World::new();
        let member = world.owner.join("member");
        std::fs::create_dir_all(member.join(".cargo")).unwrap();
        std::fs::write(member.join("Cargo.toml"), package).unwrap();
        std::fs::write(
            member.join(".cargo/config.toml"),
            format!("[build]\ntarget-dir = \"{}\"\n", world.target.display()),
        )
        .unwrap();
        std::fs::remove_file(world.owner.join("Cargo.toml")).unwrap();
        ancestor(&world.owner);
        let env = |_: &str| None;
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&member),
            env: &env,
            read: &live_read,
            read_manifest: &live_read,
            writable: &|_| Some(true),
        };
        let args = ["--unverified".to_owned(), "build".to_owned()];
        let verdict = observe_for_producer(&args, Tool::Targo, &world.home, &machine, true).ok();
        let owners: Vec<Vec<PathBuf>> = roots(&world.home)
            .into_iter()
            .map(|root| {
                root.owners
                    .iter()
                    .map(|owner| owner.strip_prefix(&world.owner).unwrap().to_path_buf())
                    .collect()
            })
            .collect();
        (verdict, owners)
    };
    let owned_by = |owner: &str| (Some(true), vec![vec![PathBuf::from(owner)]]);
    let refused = (None, Vec::new());
    let root = |dir: &Path| std::fs::write(dir.join("Cargo.toml"), ROOT).unwrap();
    assert_eq!(produce(PACKAGE, &root), owned_by(""));
    // Targo's root is the folder the link stands in, whose manifest it reads through it.
    let linked = |dir: &Path| {
        std::fs::write(dir.join("root.toml"), ROOT).unwrap();
        std::os::unix::fs::symlink("root.toml", dir.join("Cargo.toml")).unwrap();
    };
    assert_eq!(produce(PACKAGE, &linked), owned_by(""));
    let dangling = |dir: &Path| {
        std::os::unix::fs::symlink("gone.toml", dir.join("Cargo.toml")).unwrap();
    };
    assert_eq!(produce(PACKAGE, &dangling), owned_by("member"));
    let not_utf8 = |dir: &Path| {
        std::fs::write(
            dir.join("Cargo.toml"),
            [ROOT.as_bytes(), b"# \xff\n"].concat(),
        )
        .unwrap();
    };
    assert_eq!(produce(PACKAGE, &not_utf8), refused);
    let folder = |dir: &Path| std::fs::create_dir(dir.join("Cargo.toml")).unwrap();
    assert_eq!(produce(PACKAGE, &folder), refused);
    // The member's own manifest, through a link or past the reader's bound, is the one Targo
    // builds, never its parent package's: initial discovery reads no link (only absence lets
    // it go up), and one past the bound tells no owner, so either is refused.
    let parent = |dir: &Path| {
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
    };
    let own_link = |dir: &Path| {
        parent(dir);
        let member = dir.join("member");
        std::fs::rename(member.join("Cargo.toml"), member.join("real.toml")).unwrap();
        std::os::unix::fs::symlink("real.toml", member.join("Cargo.toml")).unwrap();
    };
    assert_eq!(produce(PACKAGE, &own_link), refused);
    let oversized = format!("{PACKAGE}# {}\n", "x".repeat(1 << 20));
    assert_eq!(produce(&oversized, &parent), refused);
    // A member whose manifest links to a file outside HOME is Targo's package in HOME, its
    // root the folder the link stands in, but the link grants no permission to read a file
    // outside HOME, so no owner is told: custody is owed and refused, the target its config
    // names never left unrecorded, and so is one atpkg cannot own (a folder no build made).
    let away = |dir: &Path| {
        let outside = dir.parent().unwrap().parent().unwrap().join("away");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("Cargo.toml"), PACKAGE).unwrap();
        let member = dir.join("member/Cargo.toml");
        std::fs::remove_file(&member).unwrap();
        std::os::unix::fs::symlink(outside.join("Cargo.toml"), member).unwrap();
    };
    assert_eq!(produce(PACKAGE, &away), refused);
    let stray = |dir: &Path| {
        away(dir);
        let target = dir.parent().unwrap().join("outputs.noindex");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("stray"), b"").unwrap();
    };
    assert_eq!(produce(PACKAGE, &stray), refused);
    for package in [
        "[package]\nname = \"m\"\nworkspace = 1\n",
        "[package]\nname = \"m\"\nworkspace = \"..\"\n[workspace]\n",
    ] {
        assert_eq!(produce(package, &root), refused, "{package}");
    }
    // Targo takes the `[package]` beside a `[project]`, and warns.
    assert_eq!(
        produce("[package]\nname = \"m\"\n[project]\nname = \"m\"\n", &root),
        owned_by("")
    );
    // A `[project]` names its workspace as a package does, a root that holds it; one that
    // does not hold it is refused, as Targo refuses it.
    let elsewhere = |members: &'static str| {
        move |dir: &Path| {
            root(dir);
            std::fs::create_dir(dir.join("member/other")).unwrap();
            std::fs::write(dir.join("member/other/Cargo.toml"), members).unwrap();
        }
    };
    let pointer = "[project]\nname = \"m\"\nworkspace = \"other\"\n";
    assert_eq!(
        produce(pointer, &elsewhere("[workspace]\nmembers = [\"..\"]\n")),
        owned_by("member/other")
    );
    assert_eq!(produce(pointer, &elsewhere("[workspace]\n")), refused);
}

/// WITHOUT A HOME ONLY A CALL THAT BUILDS NOTHING GOES ON. With HOME unset or relative the
/// front cannot tell which folders atpkg manages, while Targo, its home the account's, builds
/// into any target a word or a config names: a call that may build is refused, whether the
/// shim placed nothing or a producer observes it as typed, and a placement the block never
/// makes without a home is an invalid claim; help and a version query go on. NEGATIVE
/// CONTROL: before, every such call went on unobserved, its target unrecorded.
#[cfg(target_os = "macos")]
#[test]
fn without_a_home_only_a_call_that_builds_nothing_goes_on() {
    use super::super::Tool;
    let world = World::new();
    let env = |_: &str| None;
    let machine = super::super::Machine {
        home: None,
        root: None,
        cwd: Some(&world.owner),
        env: &env,
        read: &live_read,
        read_manifest: &live_read,
        writable: &|_| Some(true),
    };
    let target = world.target.display().to_string();
    let build = ["--unverified", "build", "--target-dir", target.as_str()].map(str::to_owned);
    for placed in [Some("-"), None] {
        assert!(
            observation_on(&build, Tool::Targo, &machine, true, placed).is_err(),
            "{placed:?}"
        );
        for quiet in [&["--version"][..], &["help"]] {
            let quiet: Vec<String> = quiet.iter().map(|word| (*word).to_owned()).collect();
            assert!(
                !observation_on(&quiet, Tool::Targo, &machine, true, placed).unwrap(),
                "{quiet:?} {placed:?}"
            );
        }
    }
    let placed = observation_on(&build, Tool::Targo, &machine, true, Some("env"));
    assert_eq!(placed.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    assert!(observe_command_with(&build, Tool::Targo, &machine).is_err());
    assert!(!world.target.exists());
    assert!(roots(&world.home).is_empty());
}

/// THE FRONT JUDGES EACH WORD WHERE TARGO READS IT. A help or version query is read on the
/// line Targo runs: a `--help` after an alias's `--` or a trailing verb's program goes to
/// the program, and a `-V` before an alias's name, a builtin alias's too, is dropped, so each
/// of these builds and its target is owned; `build --help`, an alias to it, and `-V build`
/// are queries, which build nothing. A trailing verb's program words are the program's, every
/// option among them, so `run serve --manifest-path <outside>` builds the launcher's package,
/// whose target is owned, and `--target-dir`, `--config` or `clean` there name nothing. A
/// `--` before a builtin command with words after it is a call Targo refuses, as is a `--`
/// before a word that starts with `-`: neither grants a target. An `include` is followed as
/// Targo merges it: optional ones that name nothing skipped, a later one over an earlier one,
/// the file's own value over all of them; the alias or target it names is owned, as is one a
/// config Targo parses as TOML 1.1 names, and one named for a workspace whose manifest Targo
/// parses as TOML 1.1 (an inline table across lines, a time without seconds). A package
/// home is the ancestor's whose config it is through a link, read once. A target's `..` is
/// the kernel's, through a link. A line the front cannot tell owes custody only where the
/// words typed name a target under HOME, under either reading of its alias. NEGATIVE
/// CONTROL: each row fails without its rule (an alias's or an untold line's target, a
/// program word read as Targo's, a typed query, an include read as untold, a link's `..`
/// read lexically, a manifest read as TOML 1.0 or with its seconds required).
#[test]
fn the_front_judges_each_word_where_targo_reads_it() {
    use super::super::Tool;
    let words =
        |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_owned()).collect() };
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    // `targeted`: whether `CARGO_TARGET_DIR` names the world's target, under HOME.
    let observe_response =
        |world: &World, cwd: &Path, targeted: bool, more: &[(&str, String)], args: &[String]| {
            let env = |name: &str| {
                (targeted && name == "CARGO_TARGET_DIR")
                    .then(|| world.target.clone().into_os_string())
                    .or_else(|| {
                        more.iter()
                            .find(|(key, _)| *key == name)
                            .map(|(_, value)| value.into())
                    })
            };
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(cwd),
                env: &env,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            observe_for_producer(args, Tool::Targo, &world.home, &machine, true)
        };
    let produce =
        |world: &World, cwd: &Path, targeted: bool, more: &[(&str, String)], args: &[String]| {
            let verdict = observe_response(world, cwd, targeted, more, args).ok();
            (verdict, roots(&world.home).len())
        };
    let config = |dir: &Path, name: &str, text: &str| {
        std::fs::create_dir_all(dir.join(".cargo")).unwrap();
        std::fs::write(dir.join(".cargo").join(name), text).unwrap();
    };
    let root = |world: &World| {
        roots(&world.home)
            .into_iter()
            .map(|root| root.root)
            .collect::<Vec<_>>()
    };
    let alias = |name: &'static str, value: &str| vec![(name, value.to_owned())];

    // Queries Targo answers on the line it runs; a `--help` or `-V` it hands on or drops.
    for (more, args) in [
        (
            alias("CARGO_ALIAS_XTASK", "run --"),
            "--unverified xtask --help",
        ),
        (vec![], "--unverified run serve --help"),
        (vec![], "--unverified -V b"),
        (
            alias("CARGO_ALIAS_XB", "build"),
            "--unverified --version xb",
        ),
    ] {
        let world = World::new();
        assert_eq!(
            produce(
                &world,
                &world.owner,
                true,
                &more,
                &words(&args.split(' ').collect::<Vec<_>>())
            ),
            (Some(true), 1),
            "{args}"
        );
        assert_eq!(root(&world), std::slice::from_ref(&world.target), "{args}");
    }
    for (more, args) in [
        (alias("CARGO_ALIAS_XH", "build --help"), "--unverified xh"),
        (vec![], "--unverified build --help"),
        (vec![], "--unverified -V build"),
    ] {
        let world = World::new();
        assert_eq!(
            produce(
                &world,
                &world.owner,
                true,
                &more,
                &words(&args.split(' ').collect::<Vec<_>>())
            ),
            (Some(false), 0),
            "{args}"
        );
    }

    // A trailing verb's program words are the program's.
    let world = World::new();
    let outside = world.home.parent().unwrap().join("elsewhere/Cargo.toml");
    let outside = outside.display().to_string();
    let served = words(&["--unverified", "run", "serve", "--manifest-path", &outside]);
    assert_eq!(
        produce(&world, &world.owner, true, &[], &served),
        (Some(true), 1)
    );
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    assert!(!requires_managed_explicit(
        &served,
        Tool::Targo,
        &world.home,
        &machine
    ));
    let env =
        |name: &str| (name == "CARGO_TARGET_DIR").then(|| world.target.clone().into_os_string());
    let targeted = super::super::Machine {
        env: &env,
        ..machine
    };
    assert!(requires_managed_explicit(
        &served,
        Tool::Targo,
        &world.home,
        &targeted
    ));
    let named = world.home.join("named").display().to_string();
    for args in [
        words(&["--unverified", "run", "serve", "--target-dir", &named]),
        words(&["--unverified", "run", "serve", "--config", "app.toml"]),
    ] {
        let world = World::new();
        assert_eq!(
            produce(&world, &world.owner, false, &[], &args),
            (Some(false), 0),
            "{args:?}"
        );
    }
    let world = World::new();
    let filtered = words(&["--unverified", "test", "clean"]);
    assert_eq!(
        produce(&world, &world.owner, true, &[], &filtered),
        (Some(true), 1)
    );

    // A `--` hands Targo a verb; it refuses words after a builtin command so handed, and a
    // word that starts with `-` it runs as no command.
    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let manifest = world.owner.join("Cargo.toml").display().to_string();
    for args in [
        words(&["--unverified", "--", "build", "--manifest-path", &manifest]),
        words(&["--unverified", "--", "--manifest-path", &manifest, "build"]),
    ] {
        assert_eq!(
            produce(&world, &notes, true, &[], &args),
            (Some(false), 0),
            "{args:?}"
        );
    }
    assert!(!world.target.exists());

    // An `include` is followed as Targo merges it.
    let xb = words(&["--unverified", "xb"]);
    let aliased = |world: &World, leaf: &str| {
        format!(
            "[alias]\nxb = \"build --target-dir {}\"\n",
            world.home.join(leaf).display()
        )
    };
    let world = World::new();
    config(
        &world.owner,
        "config.toml",
        "include = [\"aliases.toml\"]\n",
    );
    config(&world.owner, "aliases.toml", &aliased(&world, "included"));
    assert_eq!(
        produce(&world, &world.owner, true, &[], &xb),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [world.home.join("included")]);
    let world = World::new();
    config(
        &world.owner,
        "config.toml",
        "include = [{ path = \"missing.toml\", optional = true }, \"a.toml\", \"b.toml\"]\n",
    );
    config(&world.owner, "a.toml", &aliased(&world, "first"));
    config(&world.owner, "b.toml", &aliased(&world, "later"));
    assert_eq!(
        produce(&world, &world.owner, true, &[], &xb),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [world.home.join("later")]);
    let world = World::new();
    config(
        &world.owner,
        "config.toml",
        &format!("include = [\"a.toml\"]\n{}", aliased(&world, "own")),
    );
    config(&world.owner, "a.toml", &aliased(&world, "included"));
    assert_eq!(
        produce(&world, &world.owner, true, &[], &xb),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [world.home.join("own")]);
    // One Targo refuses (an include that names nothing, though not optional) is a config
    // whose directories the front cannot tell: the call owes custody whatever it names.
    let world = World::new();
    config(
        &world.owner,
        "config.toml",
        "include = [\"missing.toml\"]\n",
    );
    let tested = words(&["--unverified", "t"]);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &tested),
        (None, 0)
    );
    // A directory an include names is the config's: `b` builds into it, so the front owns
    // and records it, as Targo merges the include (before includes were followed, the call
    // was refused), for both command spellings, each on one world and each on a fresh one.
    // An independent neutral include selects no output and grants none: it leaves `b` free.
    let world = World::new();
    config(&world.owner, "config.toml", "include = [\"dirs.toml\"]\n");
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    config(&world.owner, "dirs.toml", &named);
    for typed in [["--unverified", "build"], ["--unverified", "b"]] {
        assert_eq!(
            produce(&world, &world.owner, false, &[], &words(&typed)),
            (Some(true), 1),
            "{typed:?}"
        );
    }
    assert_eq!(root(&world), std::slice::from_ref(&world.target));
    for typed in [["--unverified", "build"], ["--unverified", "b"]] {
        let world = World::new();
        config(&world.owner, "config.toml", "include = [\"dirs.toml\"]\n");
        let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
        config(&world.owner, "dirs.toml", &named);
        assert_eq!(
            produce(&world, &world.owner, false, &[], &words(&typed)),
            (Some(true), 1),
            "{typed:?}"
        );
        assert_eq!(
            root(&world),
            std::slice::from_ref(&world.target),
            "{typed:?}"
        );
    }
    let world = World::new();
    config(&world.owner, "config.toml", "include = [\"dirs.toml\"]\n");
    config(&world.owner, "dirs.toml", "[build]\njobs = 4\n");
    assert_eq!(
        produce(
            &world,
            &world.owner,
            false,
            &[],
            &words(&["--unverified", "b"])
        ),
        (Some(false), 0)
    );
    // A config in TOML 1.1 (an inline table across lines), which Targo parses, is read for
    // what it and its include name: nothing, so `b` is free, or the target, which the front
    // owns and records (read as TOML 1.0, the call was refused either way).
    let spread =
        "include = [\"dirs.toml\"]\n[env]\nA = { value = \"1\",\n      relative = true }\n";
    let world = World::new();
    config(&world.owner, "config.toml", spread);
    config(&world.owner, "dirs.toml", "[build]\njobs = 4\n");
    let b = words(&["--unverified", "b"]);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &b),
        (Some(false), 0)
    );
    let world = World::new();
    config(&world.owner, "config.toml", spread);
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    config(&world.owner, "dirs.toml", &named);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &b),
        (Some(true), 1)
    );
    assert_eq!(root(&world), std::slice::from_ref(&world.target));
    // An alias in a TOML 1.1 config is the one Targo reads, its target owned (read as TOML
    // 1.0, the alias was one the front could not tell).
    let world = World::new();
    let spread_alias = format!(
        "{}[env]\nA = {{ value = \"1\",\n relative = true }}\n",
        aliased(&world, "aliased")
    );
    config(&world.owner, "config.toml", &spread_alias);
    assert_eq!(
        produce(&world, &world.owner, true, &[], &xb),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [world.home.join("aliased")]);
    // Two configs the walk finds may include one file: each has a cycle check of its own.
    let world = World::new();
    std::fs::write(world.home.join("shared.toml"), aliased(&world, "shared")).unwrap();
    config(
        &world.owner,
        "config.toml",
        "include = [\"../../shared.toml\"]\n",
    );
    config(
        &world.home,
        "config.toml",
        "include = [\"../shared.toml\"]\n",
    );
    assert_eq!(
        produce(&world, &world.owner, true, &[], &xb),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [world.home.join("shared")]);

    // A line whose alias the front cannot tell (two package homes that read it apart: Trust's
    // main expands `xb` from the `TARGO_HOME`, Targo 16387 reads `~/.cargo`, which defines
    // none) is judged on each line it may run: the words after its name are Targo's up to a
    // `--`, after a `--` before it or a name a trailing verb has.
    let world = World::new();
    let twin = world.home.join("twin-home");
    std::fs::create_dir(&twin).unwrap();
    std::fs::write(
        twin.join("config.toml"),
        "[alias]\nxb = \"build\"\nr = \"build -p\"\nxi = \"install\"\n",
    )
    .unwrap();
    let homes = [("TARGO_HOME", twin.display().to_string())];
    let named = world.home.join("untold").display().to_string();
    let configured = format!("build.target-dir=\"{named}\"");
    for typed in [
        words(&["--unverified", "--config", &configured, "xb"]),
        words(&["--unverified", "--", "xb", "--target-dir", &named]),
        words(&["--unverified", "r", "demo", "--target-dir", &named]),
        words(&["--unverified", "xb", "-j", "2", "--target-dir", &named]),
    ] {
        assert_eq!(
            produce(&world, &world.owner, false, &homes, &typed),
            (None, 0),
            "{typed:?}"
        );
    }
    for typed in [
        words(&["--unverified", "xb"]),
        words(&["--unverified", "xb", "--", "--target-dir", &named]),
    ] {
        assert_eq!(
            produce(&world, &world.owner, false, &homes, &typed),
            (Some(false), 0),
            "{typed:?}"
        );
    }
    // An untold alias may be `install`, whose `--path` names the package it builds, from
    // wherever it is run.
    let launcher = world.home.parent().unwrap().to_path_buf();
    let owner = world.owner.display().to_string();
    let installed = words(&[
        "--unverified",
        "xi",
        "--path",
        &owner,
        "--target-dir",
        &named,
    ]);
    assert_eq!(
        produce(&world, &launcher, false, &homes, &installed),
        (None, 0)
    );
    assert!(!world.home.join("untold").exists());

    // A package home that is an ancestor's `.cargo` through a link is read once, so its
    // array is the alias's.
    let world = World::new();
    let arrays = world.home.join("arrays");
    config(
        &world.owner,
        "config.toml",
        &format!(
            "[alias]\nxb = [\"build\", \"--target-dir\", \"{}\"]\n",
            arrays.display()
        ),
    );
    let link = world.home.join("cargo-home");
    std::os::unix::fs::symlink(world.owner.join(".cargo"), &link).unwrap();
    let home = [("CARGO_HOME", link.display().to_string())];
    assert_eq!(
        produce(&world, &world.owner, true, &home, &xb),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [arrays]);

    // A target's `..` is the parent of the folder before it as it stands, through a link.
    let world = World::new();
    let elsewhere = world.home.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::create_dir(world.home.join("sub")).unwrap();
    std::os::unix::fs::symlink(world.home.join("sub"), elsewhere.join("link")).unwrap();
    let through = format!("{}/link/../linked", elsewhere.display());
    let args = words(&["--unverified", "build", "--target-dir", &through]);
    assert_eq!(produce(&world, &world.owner, false, &[], &args), (None, 0));
    assert!(!world.home.join("linked").exists());
    // So is a build directory's.
    let built = [(
        "CARGO_BUILD_BUILD_DIR",
        format!("{}/link/../built", elsewhere.display()),
    )];
    let args = words(&["--unverified", "build"]);
    assert_eq!(
        produce(&world, &world.owner, false, &built, &args),
        (None, 0)
    );
    // And the target a call names where it chooses no cache: the registered root it names
    // through a link's `..` is the one whose owners it can no longer vouch for.
    let world = World::new();
    world.build();
    let sub = world.home.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::os::unix::fs::symlink(&world.target, sub.join("link")).unwrap();
    let leaf = world.target.file_name().unwrap().to_str().unwrap();
    let through = format!("--target-dir={}/link/../{leaf}", sub.display());
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let observed = observe(
        &words(&["--unverified", "unknown-alias", &through]),
        Tool::Targo,
        &world.home,
        &machine,
    );
    assert_eq!(observed.unwrap(), Observation::Protected);
    assert!(super::read(&world.path()).unwrap().owners_unknown);

    // A verb an alias expands to is the line's: `pid = "pkgid"` writes nothing, so the
    // registered target it names keeps its owners.
    let world = World::new();
    world.build();
    let env = |name: &str| match name {
        "CARGO_TARGET_DIR" => Some(world.target.clone().into_os_string()),
        "CARGO_ALIAS_PID" => Some("pkgid".into()),
        _ => None,
    };
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &env,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let observed = observe(
        &words(&["--unverified", "pid"]),
        Tool::Targo,
        &world.home,
        &machine,
    );
    assert_eq!(observed.unwrap(), Observation::Unmanaged);
    assert!(!super::read(&world.path()).unwrap().owners_unknown);

    // A target is under HOME as the kernel reaches it: through a link from outside HOME, a
    // link before a folder Targo makes and leaves by `..`, or another name of HOME's own
    // folder (macOS's data-volume path).
    let world = World::new();
    let outside = world.home.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir(world.home.join("w")).unwrap();
    std::os::unix::fs::symlink(world.home.join("w"), outside.join("w")).unwrap();
    let mut spellings = vec![
        format!("{}/w/t", outside.display()),
        format!("{}/w/missing/../t", outside.display()),
    ];
    let data = Path::new("/System/Volumes/Data").join(world.home.strip_prefix("/").unwrap());
    if data.is_dir() {
        spellings.push(format!("{}/t", data.display()));
    }
    for target in &spellings {
        let args = words(&["--unverified", "build", "--target-dir", target]);
        assert_eq!(
            produce(&world, &world.owner, false, &[], &args),
            (None, 0),
            "{target}"
        );
    }
    assert!(!world.home.join("w/t").exists());
    // So are a manifest and a build directory spelled through that link, and `--path`
    // names a package for `install` alone (`add --path` names a dependency).
    std::fs::write(
        world.home.join("w/Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .unwrap();
    let manifest = format!("{}/w/Cargo.toml", outside.display());
    let parted = format!("{}/w/../parted", world.home.display());
    let args = words(&[
        "--unverified",
        "build",
        "--manifest-path",
        &manifest,
        "--target-dir",
        &parted,
    ]);
    assert_eq!(produce(&world, &world.owner, false, &[], &args), (None, 0));
    let built = [(
        "CARGO_BUILD_BUILD_DIR",
        format!("{}/w/bd", outside.display()),
    )];
    assert_eq!(
        produce(
            &world,
            &world.owner,
            false,
            &built,
            &words(&["--unverified", "build"])
        ),
        (None, 0)
    );
    let dependency = outside.join("dep").display().to_string();
    let args = words(&["add", "--path", &dependency]);
    assert_eq!(produce(&world, &world.owner, true, &[], &args), (None, 0));

    // `install --path` builds that folder's package, from wherever it is run.
    let world = World::new();
    let installed = world.home.join("installed").display().to_string();
    let owner = world.owner.display().to_string();
    let launcher = world.home.parent().unwrap().to_path_buf();
    for path in [
        words(&[
            "--unverified",
            "install",
            "--path",
            &owner,
            "--target-dir",
            &installed,
        ]),
        words(&[
            "--unverified",
            "install",
            &format!("--path={owner}"),
            "--target-dir",
            &installed,
        ]),
    ] {
        assert_eq!(
            produce(&world, &launcher, false, &[], &path),
            (None, 0),
            "{path:?}"
        );
    }
    // An empty `--path` is the working directory's package.
    for path in [
        words(&[
            "--unverified",
            "install",
            "--path",
            "",
            "--target-dir",
            &installed,
        ]),
        words(&[
            "--unverified",
            "install",
            "--path=",
            "--target-dir",
            &installed,
        ]),
    ] {
        assert_eq!(
            produce(&world, &world.owner, false, &[], &path),
            (None, 0),
            "{path:?}"
        );
    }
    // `install --path` reads the configs of the folder it names, never the launcher's.
    let noted = world.home.join("noted");
    let target = format!(
        "[build]\ntarget-dir = \"{}\"\n",
        world.home.join("noted-target").display()
    );
    config(&noted, "config.toml", &target);
    let args = words(&["--unverified", "install", "--path", &owner]);
    assert_eq!(produce(&world, &noted, false, &[], &args), (Some(false), 0));

    // A `-C` the front resolves is where it judges from: with no target named, none is owed;
    // with one named, it is owned for that directory's package.
    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let owner = world.owner.display().to_string();
    let moved = words(&["-Zunstable-options", "-C", &owner, "--unverified", "build"]);
    assert_eq!(
        produce(&world, &notes, false, &[], &moved),
        (Some(false), 0)
    );
    assert_eq!(produce(&world, &notes, true, &[], &moved), (Some(true), 1));
    assert_eq!(root(&world), std::slice::from_ref(&world.target));
    // A line that starts with `#` closes a multi-line string and goes on to name a directory
    // Targo builds into: the config names it, so that call owes custody, its target owned,
    // and a build directory owned beside the workspace's default `target`, which Targo writes
    // its final artifacts to where no target is named (read as a comment, the call was free
    // and its folder unrecorded).
    for (key, owned) in [("target-dir", true), ("build-dir", false)] {
        let world = World::new();
        let notes = world.home.join("notes");
        std::fs::create_dir(&notes).unwrap();
        let hidden = format!(
            "build = {{ dep-info-basedir = \"\"\"\n#\"\"\", {key} = \"{}\" }}\n",
            world.target.display()
        );
        config(&world.owner, "config.toml", &hidden);
        let owner = world.owner.display().to_string();
        let moved = words(&["-Zunstable-options", "-C", &owner, "--unverified", "build"]);
        let verdict = if owned {
            (Some(true), 1)
        } else {
            (Some(true), 2)
        };
        assert_eq!(
            produce(&world, &notes, false, &[], &moved),
            verdict,
            "{key}"
        );
        let mut recorded = root(&world);
        recorded.sort();
        let mut expected = vec![world.target.clone()];
        if !owned {
            expected.push(world.owner.join("target"));
        }
        expected.sort();
        assert_eq!(recorded, expected, "{key}");
    }
    // The same hidden build directory beside a target the call names is the config's, a
    // folder the target does not hold: both are the call's, each its own root (read as a
    // comment, the target alone was owned and the build directory unrecorded). Its visible
    // twin is owned alike.
    for hidden in [
        "build = { dep-info-basedir = \"\"\"\n#\"\"\", build-dir = \"{b}\" }\n",
        "[build]\nbuild-dir = \"{b}\"\n",
    ] {
        let world = World::new();
        let built = world.home.join("built");
        config(
            &world.owner,
            "config.toml",
            &hidden.replace("{b}", &built.display().to_string()),
        );
        assert_eq!(
            produce(
                &world,
                &world.owner,
                true,
                &[],
                &words(&["--unverified", "build"])
            ),
            (Some(true), 2),
            "{hidden}"
        );
        let mut owned = root(&world);
        owned.sort();
        let mut both = vec![world.target.clone(), built];
        both.sort();
        assert_eq!(owned, both, "{hidden}");
    }
    // An artifact directory is a folder Targo creates and links final artifacts into, which
    // no target the front records holds: each spelling is refused, the target it would have
    // owned unrecorded (read as no directory, the call was free with its target owned).
    let artifacts = world.home.join("artifacts").display().to_string();
    let flag = format!("--artifact-dir={artifacts}");
    let unstable = |rest: &[&str]| {
        let mut line = words(&["-Zunstable-options", "--unverified", "build"]);
        line.extend(words(rest));
        line
    };
    let artifact_env = vec![("CARGO_BUILD_ARTIFACT_DIR", artifacts.clone())];
    let configured = format!("[build]\nartifact-dir = \"{artifacts}\"\n");
    for (more, args, text) in [
        (vec![], unstable(&[&flag]), None),
        (vec![], unstable(&["--artifact-dir", &artifacts]), None),
        (
            vec![],
            unstable(&["--config", &format!("build.artifact-dir='{artifacts}'")]),
            None,
        ),
        (artifact_env, unstable(&[]), None),
        (vec![], unstable(&[]), Some(configured.as_str())),
    ] {
        let world = World::new();
        if let Some(text) = text {
            config(&world.owner, "config.toml", text);
        }
        assert_eq!(
            produce(&world, &world.owner, true, &more, &args),
            (None, 0),
            "{args:?} {more:?} {text:?}"
        );
    }
    let world = World::new();
    assert_eq!(
        produce(&world, &world.owner, true, &[], &unstable(&[])),
        (Some(true), 1)
    );
    // Only `build` reads one: a `check` or `test` in a workspace whose config or
    // environment names one builds as it would with none, its target owned.
    for (verb, more, text) in [
        ("check", vec![], Some(configured.as_str())),
        (
            "test",
            vec![("CARGO_BUILD_ARTIFACT_DIR", artifacts.clone())],
            None,
        ),
    ] {
        let world = World::new();
        if let Some(text) = text {
            config(&world.owner, "config.toml", text);
        }
        assert_eq!(
            produce(
                &world,
                &world.owner,
                true,
                &more,
                &words(&["--unverified", verb])
            ),
            (Some(true), 1),
            "{verb}"
        );
    }
    // A configured target holding a control character is refused, though this reader
    // decodes it as Targo does (its CRLF kept): no such folder is recorded.
    let world = World::new();
    let crlf = format!(
        "[build]\ntarget-dir = \"\"\"\r\n{}/a\r\nb\"\"\"\n",
        world.home.display()
    );
    config(&world.owner, "config.toml", &crlf);
    assert_eq!(
        produce(
            &world,
            &world.owner,
            false,
            &[],
            &words(&["--unverified", "build"])
        ),
        (None, 0)
    );

    // An include the walk cannot follow is a config whose directories the front cannot
    // tell, as Targo refuses it: text it cannot parse, an `include` that is no list, an
    // entry of another shape, a path that is no `.toml` file or holds a glob (each file there,
    // so only its spelling refuses it), a file the walk loads twice (Targo's cycle check spans
    // the walk), and one it cannot read.
    let world = World::new();
    for name in ["dirs.toml", "dirs.conf", "dir*.toml"] {
        config(&world.owner, name, "[build]\njobs = 4\n");
    }
    let built = words(&["--unverified", "build"]);
    for text in [
        "include = [\"dirs.toml\"\n",
        "include = \"dirs.toml\"\n",
        "include = [{ optional = true }]\n",
        "include = [1]\n",
        "include = [\"dirs.conf\"]\n",
        "include = [\"dir*.toml\"]\n",
        "include = [\"dirs.toml\", \"dirs.toml\"]\n",
    ] {
        config(&world.owner, "config.toml", text);
        assert_eq!(
            produce(&world, &world.owner, false, &[], &built),
            (None, 0),
            "{text}"
        );
    }
    config(&world.owner, "config.toml", "include = [\"dirs.toml\"]\n");
    assert_eq!(
        produce(&world, &world.owner, false, &[], &built),
        (Some(false), 0)
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        let shut = world.owner.join(".cargo/dirs.toml");
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unread = produce(&world, &world.owner, false, &[], &built);
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(unread, (None, 0));
    }
    // An include's `..` is folded as written, as Targo folds it, not through a link.
    let world = World::new();
    let elsewhere = world.home.join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("inner")).unwrap();
    std::fs::write(elsewhere.join("dirs.toml"), "[build]\njobs = 4\n").unwrap();
    config(
        &world.owner,
        "config.toml",
        "include = [\"sub/../dirs.toml\"]\n",
    );
    std::os::unix::fs::symlink(elsewhere.join("inner"), world.owner.join(".cargo/sub")).unwrap();
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    config(&world.owner, "dirs.toml", &named);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &built),
        (Some(true), 1)
    );
    assert_eq!(root(&world), std::slice::from_ref(&world.target));
    // Each config file the walk finds checks its own cycles: the package's and the package
    // home's may include one file.
    let world = World::new();
    config(&world.owner, "shared.toml", "[build]\njobs = 4\n");
    config(&world.owner, "config.toml", "include = [\"shared.toml\"]\n");
    let shared = world.owner.join(".cargo/shared.toml");
    let home_config = format!("include = [\"{}\"]\n", shared.display());
    config(&world.home, "config.toml", &home_config);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &built),
        (Some(false), 0)
    );
    // Targo loads the legacy `config` wherever one stands and never the `config.toml` beside
    // it, so what that file says, even text this reader cannot parse, names nothing (read,
    // the call was refused).
    let world = World::new();
    config(&world.owner, "config", "[env]\nA = \"1\"\n");
    config(
        &world.owner,
        "config.toml",
        "x = '''a'''\nt = { a = 1 b = 2 }\n",
    );
    assert_eq!(
        produce(&world, &world.owner, false, &[], &built),
        (Some(false), 0)
    );
    // A resolved included build directory and a known final target each get custody.
    let world = World::new();
    config(&world.owner, "config.toml", "include = [\"dirs.toml\"]\n");
    let build_dir = format!(
        "[build]\nbuild-dir = \"{}\"\n",
        world.home.join("built").display()
    );
    config(&world.owner, "dirs.toml", &build_dir);
    assert_eq!(
        produce(&world, &world.owner, true, &[], &built),
        (Some(true), 2)
    );
    let recorded = root(&world);
    assert!(recorded.contains(&world.target));
    assert!(recorded.contains(&world.home.join("built")));
    // A key spelled with an escape is the key Targo decodes: an `include`, and a directory.
    // The target the decoded include or key names is the call's, owned and recorded (read
    // as no key, the call would be free; before includes were followed, the include's was
    // refused).
    for escaped in [
        "\"incl\\u0075de\" = [\"dirs.toml\"]\n",
        "[build]\n\"target\\u002ddir\" = \"{target}\"\n",
    ] {
        let world = World::new();
        let target = world.target.display().to_string();
        let named = format!("[build]\ntarget-dir = \"{target}\"\n");
        config(&world.owner, "dirs.toml", &named);
        let text = escaped.replace("{target}", &target);
        config(&world.owner, "config.toml", &text);
        assert_eq!(
            produce(&world, &world.owner, false, &[], &built),
            (Some(true), 1),
            "{text}"
        );
        assert_eq!(root(&world), std::slice::from_ref(&world.target), "{text}");
    }
    // Both escaped spellings on one world: each selects the exact recorded target again.
    let world = World::new();
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    config(&world.owner, "dirs.toml", &named);
    let escaped = format!(
        "[build]\n\"target\\u002ddir\" = \"{}\"\n",
        world.target.display()
    );
    // Escaped include and target keys both select the exact recorded target; reading
    // either as no key would wrongly leave the call free.
    for (text, want, recorded) in [
        (
            "\"incl\\u0075de\" = [\"dirs.toml\"]\n",
            (Some(true), 1),
            vec![world.target.clone()],
        ),
        (
            escaped.as_str(),
            (Some(true), 1),
            vec![world.target.clone()],
        ),
    ] {
        config(&world.owner, "config.toml", text);
        assert_eq!(
            produce(&world, &world.owner, false, &[], &built),
            want,
            "{text}"
        );
        assert_eq!(root(&world), recorded, "{text}");
    }
    // An included file reached through a link is read through it, as Targo reads it,
    // though the front's own reader refuses a link; one that names nothing is a file that
    // does not stand, skipped where the include is optional.
    let world = World::new();
    let dotfiles = world.home.join("dotfiles");
    std::fs::create_dir(&dotfiles).unwrap();
    std::fs::write(dotfiles.join("local.toml"), "[build]\njobs = 2\n").unwrap();
    config(
        &world.owner,
        "config.toml",
        "include = [\"local.toml\", { path = \"gone.toml\", optional = true }]\n",
    );
    let cargo = world.owner.join(".cargo");
    std::os::unix::fs::symlink(dotfiles.join("local.toml"), cargo.join("local.toml")).unwrap();
    std::os::unix::fs::symlink(dotfiles.join("missing.toml"), cargo.join("gone.toml")).unwrap();
    let front_read =
        |path: &Path| match crate::metadata_io::read_bounded_regular_utf8(path, 1 << 20) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        };
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &front_read,
        read_manifest: &front_read,
        writable: &|_| Some(true),
    };
    assert_eq!(
        observe_for_producer(&built, Tool::Targo, &world.home, &machine, true).ok(),
        Some(false)
    );
    // The included target of an earlier world, outside this one's, grants no new owner.
    std::fs::write(dotfiles.join("local.toml"), &named).unwrap();
    assert_eq!(
        observe_for_producer(&built, Tool::Targo, &world.home, &machine, true).ok(),
        None
    );
    // This world's own target, read through the link, is owned and recorded.
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    std::fs::write(dotfiles.join("local.toml"), &named).unwrap();
    assert_eq!(
        observe_for_producer(&built, Tool::Targo, &world.home, &machine, true).ok(),
        Some(true)
    );
    assert_eq!(root(&world), std::slice::from_ref(&world.target));
    // So is a walk's own config reached through a link: the target it names is owned.
    let world = World::new();
    let dotfiles = world.home.join("dotfiles");
    std::fs::create_dir(&dotfiles).unwrap();
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    std::fs::write(dotfiles.join("c.toml"), &named).unwrap();
    std::fs::create_dir(world.owner.join(".cargo")).unwrap();
    std::os::unix::fs::symlink(
        dotfiles.join("c.toml"),
        world.owner.join(".cargo/config.toml"),
    )
    .unwrap();
    let machine = super::super::Machine {
        home: Some(&world.home),
        cwd: Some(&world.owner),
        ..machine
    };
    assert_eq!(
        observe_for_producer(&built, Tool::Targo, &world.home, &machine, true).ok(),
        Some(true)
    );
    assert_eq!(root(&world), std::slice::from_ref(&world.target));
    // A package home's `..` is folded as written, as Targo's `homedir` folds it, never
    // through a link: the home config Targo reads is the folded path's, whatever the link's
    // target holds, its includes with it.
    let world = World::new();
    std::fs::create_dir_all(world.home.join("z/y")).unwrap();
    std::fs::create_dir_all(world.home.join("z/h")).unwrap();
    std::fs::create_dir_all(world.home.join("h")).unwrap();
    std::os::unix::fs::symlink(world.home.join("z/y"), world.home.join("link")).unwrap();
    std::fs::write(
        world.home.join("z/h/config.toml"),
        "include = [\"x.toml\"]\n",
    )
    .unwrap();
    let wrong = world.home.join("wrong.noindex");
    let wrong_named = format!("[build]\ntarget-dir = \"{}\"\n", wrong.display());
    std::fs::write(world.home.join("h/x.toml"), &wrong_named).unwrap();
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    std::fs::write(world.home.join("h/config.toml"), &named).unwrap();
    let cargo_home = [("CARGO_HOME", format!("{}/link/../h", world.home.display()))];
    assert_eq!(
        produce(&world, &world.owner, false, &cargo_home, &built),
        (Some(true), 1)
    );
    assert_eq!(root(&world), std::slice::from_ref(&world.target));
    // The walk stops where `__CARGO_TEST_ROOT` stops Targo's, the package home read after
    // it: a farther ancestor's target is none Targo builds into.
    let world = World::new();
    let member = world.owner.join("member");
    std::fs::create_dir(&member).unwrap();
    let wrong = world.home.join("wrong.noindex");
    let wrong_named = format!("[build]\ntarget-dir = \"{}\"\n", wrong.display());
    config(&world.owner, "config.toml", &wrong_named);
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    config(&world.home, "config.toml", &named);
    let stop = [("__CARGO_TEST_ROOT", member.display().to_string())];
    assert_eq!(
        produce(&world, &member, false, &stop, &built),
        (Some(true), 1)
    );
    assert_eq!(root(&world), std::slice::from_ref(&world.target));
    // An alias config's include whose `..` climbs past the root is folded there, as Targo's
    // `normalize_path` folds it: an optional one where nothing stands is skipped, and the
    // alias told.
    let world = World::new();
    let past = format!(
        "include = [{{ path = \"{}x.toml\", optional = true }}]\n[alias]\nxb = \"build --target-dir {}\"\n",
        "../".repeat(40),
        world.home.join("past").display()
    );
    config(&world.owner, "config.toml", &past);
    assert_eq!(
        produce(
            &world,
            &world.owner,
            false,
            &[],
            &words(&["--unverified", "xb"])
        ),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [world.home.join("past")]);
    // A workspace manifest is read as Targo parses it, as TOML 1.1: a target a config names
    // in a workspace whose manifest holds an inline table across lines is owned.
    let world = World::new();
    std::fs::write(
        world.owner.join("Cargo.toml"),
        "[workspace]\nmembers = []\n[workspace.dependencies]\nserde = { version = \"1\",\n  default-features = false }\n",
    )
    .unwrap();
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    config(&world.owner, "config.toml", &named);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &built),
        (Some(true), 1)
    );
    assert_eq!(root(&world), std::slice::from_ref(&world.target));
    // A time without seconds is TOML 1.1 too, which a manifest's metadata may hold: the
    // workspace whose manifest holds one is read, and the target its config names owned.
    let world = World::new();
    std::fs::write(
        world.owner.join("Cargo.toml"),
        "[workspace]\nmembers = []\n[workspace.metadata]\nat = 07:32\n",
    )
    .unwrap();
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    config(&world.owner, "config.toml", &named);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &built),
        (Some(true), 1)
    );
    assert_eq!(root(&world), std::slice::from_ref(&world.target));
    // A `--config` value is the config file standing at that path, whatever it spells.
    for (value, judged) in [("o=1", (Some(true), 1)), ("o=2", (Some(false), 0))] {
        let world = World::new();
        let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
        std::fs::write(world.owner.join("o=1"), &named).unwrap();
        let args = words(&["--unverified", "build", "--config", value]);
        assert_eq!(
            produce(&world, &world.owner, false, &[], &args),
            judged,
            "{value}"
        );
    }

    // A user alias of a builtin command's name makes Targo run the command bare, the words a
    // `--` hands it ignored, into the target its config names, which the call owns; with
    // none Targo refuses the words. A protected tool so handed is run with its words, its
    // target the call's.
    let world = World::new();
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    config(&world.owner, "config.toml", &named);
    let shadowed = [("CARGO_ALIAS_BUILD", "check".to_owned())];
    let args = words(&["--unverified", "--", "build", "--release"]);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &args),
        (Some(false), 0)
    );
    assert_eq!(
        produce(&world, &world.owner, false, &shadowed, &args),
        (Some(true), 1)
    );
    assert_eq!(root(&world), std::slice::from_ref(&world.target));
    let world = World::new();
    let named = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    config(&world.owner, "config.toml", &named);
    let linted = world.home.join("linted");
    let args = words(&["--", "clippy", "--target-dir", linted.to_str().unwrap()]);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &args),
        (Some(true), 1)
    );
    assert_eq!(root(&world), [linted]);

    // A `..` after folders Targo makes beneath a link is read past the link's target, the
    // folders in the order written: through a link into the shared cache's parent, the
    // target lands in the cache, which owes nothing.
    let world = World::new();
    let root = super::super::cache_root(&world.home);
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(root.parent().unwrap(), world.home.join("c")).unwrap();
    let through = format!(
        "{}/c/{}/ws/made/../{}",
        world.home.display(),
        root.file_name().unwrap().to_str().unwrap(),
        super::super::LEAF
    );
    let args = words(&["--unverified", "build", "--target-dir", &through]);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &args),
        (Some(false), 0)
    );

    // A `-C` resolved through HOME's other name (macOS's data-volume path) is HOME's: the
    // target it names there is owned.
    let world = World::new();
    let data = Path::new("/System/Volumes/Data").join(world.owner.strip_prefix("/").unwrap());
    if data.is_dir() {
        let launcher = world.home.parent().unwrap().to_path_buf();
        let target = world.home.join("moved").display().to_string();
        let moved = words(&[
            "-Zunstable-options",
            "-C",
            data.to_str().unwrap(),
            "--unverified",
            "build",
            "--target-dir",
            &target,
        ]);
        assert_eq!(
            produce(&world, &launcher, false, &[], &moved),
            (Some(true), 1)
        );
        assert_eq!(
            roots(&world.home)
                .into_iter()
                .map(|root| root.root)
                .collect::<Vec<_>>(),
            [world.home.join("moved")]
        );
    }
    // And a `-C` call that owes nothing leaves the registered receipts as they stand: the
    // front judges it from that directory, not from where it was typed.
    let world = World::new();
    world.build();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let owner = world.owner.display().to_string();
    let moved = words(&["-Zunstable-options", "-C", &owner, "--unverified", "build"]);
    assert_eq!(
        produce(&world, &notes, false, &[], &moved),
        (Some(false), 1)
    );
    assert!(!super::read(&world.path()).unwrap().owners_unknown);

    // `miri` hands Targo the line it runs: with a help option, its words as typed, past
    // `run`'s program the program's; else its first `--target-dir` with `miri` joined to it,
    // ahead of the rest.
    let world = World::new();
    let outside = world.home.parent().unwrap().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let manifest = world.owner.join("Cargo.toml").display().to_string();
    let t = world.home.join("t").display().to_string();
    let handed = words(&[
        "--unverified",
        "miri",
        "run",
        "-m",
        &manifest,
        "--target-dir",
        &t,
        "demo",
        "-h",
    ]);
    assert_eq!(produce(&world, &outside, false, &[], &handed), (None, 0));
    let program = words(&[
        "--unverified",
        "miri",
        "run",
        "demo",
        "--target-dir",
        &t,
        "-h",
    ]);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &program),
        (Some(false), 0)
    );
    assert!(!world.home.join("t").exists());
    let forwarded = words(&["--unverified", "miri", "run", "demo", "--target-dir", &t]);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &forwarded),
        (Some(true), 1)
    );
    let made: Vec<_> = roots(&world.home)
        .into_iter()
        .map(|made| made.root)
        .collect();
    assert_eq!(made, [world.home.join("t")]);

    // With no `--target-dir` typed, `miri` builds into `cargo metadata`'s target joined with
    // `miri`: `CARGO_TARGET_DIR`'s, a config's, or the workspace root's `target`, the root
    // as Targo finds it. Each call has its sysroot (`MIRI_SYSROOT`), so it builds no other.
    let sysroot = || vec![("MIRI_SYSROOT", "/private/tmp/no-sysroot".to_owned())];
    let run = words(&["--unverified", "miri", "run"]);
    let made_roots = |world: &World| {
        roots(&world.home)
            .into_iter()
            .map(|made| made.root)
            .collect::<Vec<_>>()
    };
    let world = World::new();
    assert_eq!(
        produce(&world, &world.owner, true, &sysroot(), &run),
        (Some(true), 1)
    );
    assert_eq!(made_roots(&world), std::slice::from_ref(&world.target));
    let world = World::new();
    let cfg = world.home.join("cfg");
    config(
        &world.owner,
        "config.toml",
        &format!("[build]\ntarget-dir = \"{}\"\n", cfg.display()),
    );
    assert_eq!(
        produce(&world, &world.owner, false, &sysroot(), &run),
        (Some(true), 1)
    );
    assert_eq!(made_roots(&world), std::slice::from_ref(&cfg));
    let world = World::new();
    let test = words(&["--unverified", "miri", "test"]);
    assert_eq!(
        produce(&world, &world.owner, false, &sysroot(), &test),
        (Some(true), 1)
    );
    assert_eq!(made_roots(&world), [world.owner.join("target")]);
    // That folder is the one recorded, so a build that names it after the `miri` line takes
    // it: the variable's and the config's are owned, never left a plain folder that a root
    // inside it keeps every build from owning.
    for configured in [false, true] {
        let world = World::new();
        if configured {
            config(
                &world.owner,
                "config.toml",
                &format!("[build]\ntarget-dir = \"{}\"\n", world.target.display()),
            );
        }
        let row = format!("configured {configured}");
        assert_eq!(
            produce(&world, &world.owner, !configured, &sysroot(), &run),
            (Some(true), 1),
            "{row}"
        );
        let build = words(&["--unverified", "build"]);
        assert_eq!(
            produce(&world, &world.owner, !configured, &[], &build),
            (Some(true), 1),
            "{row}"
        );
        assert_eq!(
            made_roots(&world),
            std::slice::from_ref(&world.target),
            "{row}"
        );
    }
    // A target cargo made is adopted itself, whatever its `miri` holds (one an interrupted
    // `cargo-miri` left, which adoption may not take).
    let world = World::new();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&world.target)
        .unwrap();
    std::fs::write(
        world.target.join("CACHEDIR.TAG"),
        super::adoption::CARGO_TAG.as_bytes(),
    )
    .unwrap();
    std::fs::write(world.target.join(".rustc_info.json"), b"{}").unwrap();
    for prepared in ["build", "incremental", ".fingerprint/probe-hash"] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(world.target.join("debug").join(prepared))
            .unwrap();
    }
    let stamp = world
        .target
        .join("debug/.fingerprint/probe-hash/invoked.timestamp");
    std::fs::write(&stamp, b"invoked").unwrap();
    std::fs::set_permissions(&stamp, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::create_dir_all(world.target.join("miri/debug")).unwrap();
    assert_eq!(
        produce(&world, &world.owner, true, &sysroot(), &test),
        (Some(true), 1)
    );
    assert_eq!(made_roots(&world), std::slice::from_ref(&world.target));
    assert!(!world.target.join("miri").join(MARKER).exists());
    // A target named through a link to a folder atpkg owns is that folder, as a build's is:
    // the link's `miri` lies in it.
    let world = World::new();
    world.build();
    let linked = world.home.join("linked");
    std::os::unix::fs::symlink(&world.target, &linked).unwrap();
    let through = words(&[
        "--unverified",
        "miri",
        "test",
        "--target-dir",
        &linked.display().to_string(),
    ]);
    assert_eq!(
        produce(&world, &world.owner, false, &sysroot(), &through),
        (Some(true), 1)
    );
    assert_eq!(made_roots(&world), std::slice::from_ref(&world.target));
    assert!(!world.target.join("miri").exists());
    // Another workspace's `miri` line through the same link is recorded as an owner of the
    // folder it names, which it writes inside of: never left to the first owner alone,
    // whose going would retire the folder while this one's `miri` builds there.
    let other = world.another_owner();
    assert_eq!(
        produce(&world, &other, false, &sysroot(), &through),
        (Some(true), 1)
    );
    let recorded = roots(&world.home);
    assert_eq!(recorded[0].root, world.target);
    assert!(
        recorded[0].owners.contains(&other),
        "{:?}",
        recorded[0].owners
    );
    // A link to a folder atpkg does not own grants nothing: refused, nothing recorded and
    // nothing made in it.
    let world = World::new();
    let bare = world.home.join("bare");
    std::fs::create_dir(&bare).unwrap();
    let linked = world.home.join("linked");
    std::os::unix::fs::symlink(&bare, &linked).unwrap();
    let through = words(&[
        "--unverified",
        "miri",
        "test",
        "--target-dir",
        &linked.display().to_string(),
    ]);
    assert_eq!(
        produce(&world, &world.owner, false, &sysroot(), &through),
        (None, 0)
    );
    assert!(!bare.join("miri").exists());
    assert!(!bare.join(MARKER).exists());
    // A folder the line writes inside of that stands, holds no marker and cannot be owned
    // (no build of cargo's made it) leaves the line's own target, its `miri`, to be owned in
    // its place, so the build runs, recorded there.
    let world = World::new();
    let plain = world.home.join("plain");
    std::fs::create_dir(&plain).unwrap();
    let into_plain = words(&[
        "--unverified",
        "miri",
        "test",
        "--target-dir",
        &plain.display().to_string(),
    ]);
    assert_eq!(
        produce(&world, &world.owner, false, &sysroot(), &into_plain),
        (Some(true), 1)
    );
    assert_eq!(made_roots(&world), [plain.join("miri")]);
    assert!(!plain.join(MARKER).exists());
    // `Targo.toml` is a package's manifest, Targo's canonical name, found before
    // `Cargo.toml` in each folder: its package owns the target, a typed one's too.
    let world = World::new();
    let native = world.home.join("native");
    std::fs::create_dir(&native).unwrap();
    std::fs::write(native.join("Targo.toml"), "[package]\nname = \"native\"\n").unwrap();
    assert_eq!(
        produce(&world, &native, false, &sysroot(), &test),
        (Some(true), 1)
    );
    let recorded = roots(&world.home);
    assert_eq!(recorded[0].root, native.join("target"));
    assert_eq!(recorded[0].owners, std::slice::from_ref(&native));
    let typed_native = world.home.join("native-out.noindex");
    let build = words(&[
        "--unverified",
        "build",
        "--target-dir",
        &typed_native.display().to_string(),
    ]);
    assert_eq!(
        produce(&world, &native, false, &[], &build),
        (Some(true), 2)
    );
    // A member the root excludes is its own workspace's root.
    let world = World::new();
    std::fs::write(
        world.owner.join("Cargo.toml"),
        "[workspace]\nmembers = [\"m\"]\nexclude = [\"x\"]\n",
    )
    .unwrap();
    for member in ["m", "x"] {
        std::fs::create_dir(world.owner.join(member)).unwrap();
        std::fs::write(
            world.owner.join(member).join("Cargo.toml"),
            format!("[package]\nname = \"{member}\"\n"),
        )
        .unwrap();
    }
    assert_eq!(
        produce(&world, &world.owner.join("m"), false, &sysroot(), &run),
        (Some(true), 1)
    );
    assert_eq!(
        produce(&world, &world.owner.join("x"), false, &sysroot(), &run),
        (Some(true), 2)
    );
    let mut made = made_roots(&world);
    made.sort();
    assert_eq!(
        made,
        [world.owner.join("target"), world.owner.join("x/target")]
    );
    // A package the root neither lists nor excludes is no member, and Targo's metadata
    // fails on it: `cargo-miri` builds nothing, and nothing is made or recorded.
    let unlisted = world.owner.join("y");
    std::fs::create_dir(&unlisted).unwrap();
    std::fs::write(unlisted.join("Cargo.toml"), "[package]\nname = \"y\"\n").unwrap();
    assert_eq!(
        produce(&world, &unlisted, false, &sysroot(), &run),
        (None, 2)
    );
    assert!(!unlisted.join("target").exists());
    // `cargo-miri` hands `cargo metadata` its first `--manifest-path`, never `-m`: the line
    // Targo runs reads either as the package it builds, the cache's owner.
    for (flag, metadata_reads) in [("-m", false), ("--manifest-path", true)] {
        let world = World::new();
        let other = world.another_owner();
        let manifest = other.join("Cargo.toml").display().to_string();
        let args = words(&["--unverified", "miri", "run", flag, &manifest]);
        assert_eq!(
            produce(&world, &world.owner, false, &sysroot(), &args),
            (Some(true), 1),
            "{flag}"
        );
        let made = if metadata_reads { &other } else { &world.owner };
        let recorded = roots(&world.home);
        assert_eq!(recorded[0].root, made.join("target"), "{flag}");
        assert_eq!(recorded[0].owners, std::slice::from_ref(&other), "{flag}");
    }
    // A metadata target the reader cannot tell owes custody: a `--config` naming one, past
    // the program too, and a manifest above the package it cannot parse.
    let world = World::new();
    let aside = world.home.join("p").display().to_string();
    let config_target = format!("build.target-dir=\"{aside}\"");
    let told = words(&[
        "--unverified",
        "miri",
        "run",
        "demo",
        "--config",
        &config_target,
    ]);
    assert_eq!(
        produce(&world, &world.owner, false, &sysroot(), &told),
        (None, 0)
    );
    let world = World::new();
    let pkg = world.home.join("pkg");
    std::fs::create_dir(&pkg).unwrap();
    std::fs::write(pkg.join("Cargo.toml"), "[package]\nname = \"pkg\"\n").unwrap();
    std::fs::write(world.home.join("Cargo.toml"), "[").unwrap();
    assert_eq!(produce(&world, &pkg, false, &sysroot(), &run), (None, 0));
    assert!(!pkg.join("target").exists());
    // Where the package is one atpkg never manages, that call is no producer of its own,
    // and the unread target may be any registered root: each one's owners become unknown.
    let world = World::new();
    world.build();
    let never = world.home.join("Library/p");
    std::fs::create_dir_all(&never).unwrap();
    std::fs::write(never.join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
    let mut walled = sysroot();
    walled.push(("__CARGO_TEST_ROOT", "/x".to_owned()));
    assert!(!roots(&world.home)[0].owners_unknown);
    assert_eq!(
        produce(&world, &never, false, &walled, &run),
        (Some(false), 1)
    );
    assert!(roots(&world.home)[0].owners_unknown);
    // An alias the reader cannot tell on the verb `miri` hands back may build into the target
    // that line names: the call owes it custody, and with no line to judge it is refused.
    let world = World::new();
    let mut untold = sysroot();
    untold.push(("CARGO_ALIAS_T", "-C ../a -C ../b test".to_owned()));
    assert_eq!(
        produce(
            &world,
            &world.owner,
            false,
            &untold,
            &words(&["--unverified", "miri", "t"])
        ),
        (None, 0)
    );
    assert!(!world.owner.join("target").exists());
    // `nextest` is handed back with the metadata target too: a user alias of it Targo
    // expands (`nextest = "test"`) builds there, and that target is owned; with none, Targo
    // refuses the lane on the external `nextest`, and the front refuses the target that line
    // names, which it cannot judge after an external verb: nothing builds either way.
    let world = World::new();
    let nextest = words(&["--unverified", "miri", "nextest", "run"]);
    assert_eq!(
        produce(&world, &world.owner, false, &sysroot(), &nextest),
        (None, 0)
    );
    assert!(!world.owner.join("target").exists());
    let mut aliased = sysroot();
    aliased.push(("CARGO_ALIAS_NEXTEST", "test".to_owned()));
    assert_eq!(
        produce(&world, &world.owner, false, &aliased, &nextest),
        (Some(true), 1)
    );
    assert_eq!(made_roots(&world), [world.owner.join("target")]);
    // A call in no lane Targo refuses before `targo-miri` runs: it keeps the line its words
    // alone give, as before.
    let world = World::new();
    let unconsented = words(&["miri", "run"]);
    assert_eq!(
        produce(&world, &world.owner, false, &sysroot(), &unconsented),
        (Some(false), 0)
    );
    assert!(!world.owner.join("target").exists());
    // A child of a Targo inherits its lane (`TRUST_TARGO_NESTED_UNVERIFIED_ENDPOINT`), and
    // Targo runs `miri` there with no `--unverified` typed: its metadata target is owned, and
    // its setup judged, as a typed lane's are.
    let inherited = |more: Vec<(&'static str, String)>| {
        let mut more = more;
        more.push((
            super::super::NESTED_UNVERIFIED_ENDPOINT,
            "endpoint".to_owned(),
        ));
        more
    };
    assert_eq!(
        produce(
            &world,
            &world.owner,
            false,
            &inherited(sysroot()),
            &unconsented
        ),
        (Some(true), 1)
    );
    assert_eq!(made_roots(&world), [world.owner.join("target")]);
    let world = World::new();
    let sysroot_in_home = vec![(
        "MIRI_SYSROOT",
        world
            .owner
            .join("target/miri-sysroot")
            .display()
            .to_string(),
    )];
    let bare_setup = words(&["miri", "setup"]);
    assert_eq!(
        produce(&world, &world.owner, false, &sysroot_in_home, &bare_setup),
        (Some(false), 0)
    );
    assert_eq!(
        produce(
            &world,
            &world.owner,
            false,
            &inherited(sysroot_in_home.clone()),
            &bare_setup
        ),
        (None, 0)
    );
    assert!(!world.owner.join("target").exists());
    // A target atpkg owns is the folder the line `miri` hands back writes only inside of:
    // the call is one of its owners, as it was when its `miri` was read as the target
    // itself, whether a cargo-made `miri` (no folder adoption may take there) stands in it
    // or none yet does (a fresh root nested inside a registered one would split their
    // custody), and none is made there.
    let cargo_made = |dir: &Path| {
        std::fs::create_dir_all(dir.join("debug/build")).unwrap();
        std::fs::write(
            dir.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n\
             # This file is a cache directory tag created by cargo.\n\
             # For information about cache directory tags see https://bford.info/cachedir/\n",
        )
        .unwrap();
        std::fs::write(dir.join(".rustc_info.json"), b"{}").unwrap();
    };
    let target = |world: &World| world.target.display().to_string();
    // Setup, where `MIRI_SYSROOT` is unset, builds where no target names, the variable too.
    for typed in [false, true] {
        for set_up in [false, true] {
            for standing in [false, true] {
                let world = World::new();
                world.build();
                if standing {
                    cargo_made(&world.target.join("miri"));
                }
                let mut args = run.clone();
                if typed {
                    args.extend(["--target-dir".to_owned(), target(&world)]);
                }
                let other = world.another_owner();
                let more = if set_up { vec![] } else { sysroot() };
                let row = format!("typed {typed} set up {set_up} standing {standing}");
                assert_eq!(
                    produce(&world, &other, !typed, &more, &args),
                    (Some(true), 1),
                    "{row}"
                );
                let recorded = roots(&world.home);
                assert_eq!(recorded.len(), 1, "{row}");
                assert_eq!(recorded[0].root, world.target, "{row}");
                assert!(recorded[0].owners.contains(&other), "{row}");
                assert!(!world.target.join("miri").join(MARKER).exists(), "{row}");
            }
        }
    }
    // So where a user alias takes a builtin alias's name (`t = "miri test"`), which the
    // placement reads as the builtin's and places under the target `CARGO_TARGET_DIR` names:
    // the line Targo runs is the `miri` one, an external command that never receives a global
    // `--config`, so no build writes the placed folder, and the call owns the target it
    // builds inside of, with no intermediates prepared under a `miri` record it has none of.
    for standing in [false, true] {
        let world = World::new();
        world.build();
        if standing {
            cargo_made(&world.target.join("miri"));
        }
        let other = world.another_owner();
        let mut more = sysroot();
        more.push(("CARGO_ALIAS_T", "miri test".to_owned()));
        let row = format!("standing {standing}");
        assert_eq!(
            produce(&world, &other, true, &more, &words(&["--unverified", "t"])),
            (Some(true), 1),
            "{row}"
        );
        let recorded = roots(&world.home);
        assert_eq!(recorded[0].root, world.target, "{row}");
        assert!(recorded[0].owners.contains(&other), "{row}");
    }

    // `miri` sets up its sysroot first where `MIRI_SYSROOT` is unset, and `miri setup`
    // always: a Targo of its own builds the standard library in a fresh workspace in the
    // temporary folder, its target and build directory that workspace's `target`, which the
    // builder sets in the variables that outrank every config (`rustc-build-sysroot`'s
    // `build_from_source`), and stages the result inside the sysroot. Where either folder
    // lies in one atpkg manages (HOME itself, a folder in it through any spelling, the shared
    // cache's, the working directory an empty or relative value names), or a variable or a
    // config Targo reads from where it works names an artifact directory, which copies that
    // build's output, the call is refused before any build. A target or build directory,
    // which that build never reads, leaves it clear.
    // Each row: the variables beside the world's and the words after `--unverified`, `@H`
    // spelling HOME, `@O` a folder outside it, and the verdict.
    let typed = "miri run --target-dir @H/t";
    let alias = ("CARGO_ALIAS_XB", "-C ../a -C ../b build");
    let cases = [
        (vec![("TMPDIR", "@H/tmp")], typed, (None, 0)),
        (
            vec![("TMPDIR", "@H/tmp"), ("MIRI_SYSROOT", "/private/tmp/s")],
            typed,
            (Some(true), 1),
        ),
        (vec![("TMPDIR", "@H")], typed, (None, 0)),
        (vec![("TMPDIR", "")], typed, (None, 0)),
        (vec![("TMPDIR", "rel")], typed, (None, 0)),
        (vec![], typed, (Some(true), 1)),
        (vec![("TMPDIR", "/private/tmp")], typed, (Some(true), 1)),
        (vec![("TMPDIR", "@O/tmp")], typed, (Some(true), 1)),
        // A folder outside HOME that links into it is HOME's.
        (vec![("TMPDIR", "@O/link")], typed, (None, 0)),
        (
            vec![("TMPDIR", "@H/Library/Caches/aterm/t")],
            typed,
            (None, 0),
        ),
        (vec![("MIRI_SYSROOT", "@H")], "miri setup", (None, 0)),
        // A sysroot outside HOME is clear, unless its `lib` or `lib/rustlib`, which setup
        // makes and renames its result into, is a link into HOME.
        (
            vec![("MIRI_SYSROOT", "@O/sys")],
            "miri setup",
            (Some(false), 0),
        ),
        (
            vec![("MIRI_SYSROOT", "@O/lib-link")],
            "miri setup",
            (None, 0),
        ),
        (vec![("MIRI_SYSROOT", "")], "miri setup", (None, 0)),
        (vec![("MIRI_SYSROOT", "rel")], "miri setup", (None, 0)),
        (vec![], "miri setup", (Some(false), 0)),
        // A spelling under a folder of HOME atpkg never looks into is clear only as the
        // kernel resolves it: one that is, or whose `lib` is, a link into a folder atpkg
        // manages is that folder's, as one from outside HOME is.
        (
            vec![("MIRI_SYSROOT", "@H/Library/Caches/plain")],
            "miri setup",
            (Some(false), 0),
        ),
        (
            vec![("MIRI_SYSROOT", "@H/Library/Caches/tmp-link")],
            "miri setup",
            (None, 0),
        ),
        (
            vec![("MIRI_SYSROOT", "@H/Library/Caches/lib-link")],
            "miri setup",
            (None, 0),
        ),
        (
            vec![("TMPDIR", "@H/Library/Caches/tmp-link")],
            typed,
            (None, 0),
        ),
        (
            vec![("TMPDIR", "@H/Library/Caches/plain")],
            typed,
            (Some(true), 1),
        ),
        (
            vec![("CARGO_BUILD_ARTIFACT_DIR", "@O/a")],
            "miri setup",
            (None, 0),
        ),
        (
            vec![("TARGO_BUILD_ARTIFACT_DIR", "@O/a")],
            "miri setup",
            (None, 0),
        ),
        // The variables setup's builder sets itself, which outrank the ones inherited.
        (
            vec![("CARGO_BUILD_BUILD_DIR", "@H/bd")],
            "miri setup",
            (Some(false), 0),
        ),
        (
            vec![("TARGO_BUILD_BUILD_DIR", "@H/bd")],
            "miri setup",
            (Some(false), 0),
        ),
        (
            vec![("CARGO_TARGET_DIR", "@H/x")],
            "miri setup",
            (Some(false), 0),
        ),
        (
            vec![("CARGO_BUILD_TARGET_DIR", "@H/x")],
            "miri setup",
            (Some(false), 0),
        ),
        // A target variable moves the line `miri` hands back, never setup's build.
        (
            vec![("CARGO_TARGET_DIR", "@H/x")],
            "miri run",
            (Some(true), 1),
        ),
        (
            vec![("CARGO_TARGET_DIR", "/private/tmp/x")],
            "miri test",
            (Some(false), 0),
        ),
        (
            vec![("CARGO_TARGET_DIR", "@H/x")],
            "miri run --target-dir @H/t",
            (Some(true), 1),
        ),
        // An alias this reader cannot tell may run `miri` on a call that consents.
        (vec![("TMPDIR", "@H/tmp"), alias], "xb", (None, 0)),
        (vec![alias], "xb", (Some(false), 0)),
    ];
    let setup_refusals = [
        ("TMPDIR", "@H/tmp", "TMPDIR"),
        ("MIRI_SYSROOT", "@H/sysroot", "MIRI_SYSROOT"),
        (
            "MIRI_SYSROOT",
            "@O/rustlib-link",
            "MIRI_SYSROOT/lib/rustlib",
        ),
    ];
    let cases = cases
        .into_iter()
        .map(|(vars, line, judged)| (vars, line, judged, None));
    let setup_refusals = setup_refusals
        .into_iter()
        .map(|(var, path, cause)| (vec![(var, path)], "miri setup", (None, 0), Some(cause)));
    for (more, line, judged, cause) in cases.chain(setup_refusals) {
        let world = World::new();
        let home = world.home.display().to_string();
        let outside = world.home.parent().unwrap().join("outside");
        std::fs::create_dir_all(outside.join("tmp")).unwrap();
        std::fs::create_dir_all(world.home.join("tmp")).unwrap();
        if !outside.join("link").exists() {
            std::os::unix::fs::symlink(world.home.join("tmp"), outside.join("link")).unwrap();
        }
        let caches = world.home.join("Library/Caches");
        for link in [
            outside.join("lib-link/lib"),
            outside.join("rustlib-link/lib/rustlib"),
            caches.join("tmp-link"),
            caches.join("lib-link/lib"),
        ] {
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            if std::fs::symlink_metadata(&link).is_err() {
                std::os::unix::fs::symlink(world.home.join("tmp"), link).unwrap();
            }
        }
        std::fs::create_dir_all(caches.join("plain")).unwrap();
        let outside = outside.display().to_string();
        let spelled = |text: &str| text.replace("@H", &home).replace("@O", &outside);
        let more: Vec<(&str, String)> = more
            .iter()
            .map(|(name, value)| (*name, spelled(value)))
            .collect();
        let args: Vec<String> = spelled(&format!("--unverified {line}"))
            .split(' ')
            .map(str::to_owned)
            .collect();
        let response = observe_response(&world, &world.owner, false, &more, &args);
        if let Some(cause) = cause {
            let why = response.as_ref().unwrap_err().to_string();
            assert!(
                why.split_whitespace().any(|word| word == cause) && why.contains("protects"),
                "{why}"
            );
        }
        assert_eq!(
            (response.ok(), roots(&world.home).len()),
            judged,
            "{more:?} {line}"
        );
        if judged.0.is_none() {
            assert!(!world.home.join("t").exists(), "{more:?} {line}");
        }
    }
    // miri's own cache folder, the sysroot where `MIRI_SYSROOT` is unset, is judged as the
    // kernel resolves it too: a link there, or at its `lib`, into a folder atpkg manages
    // refuses the call.
    for link in ["", "lib"] {
        let world = World::new();
        let sysroot = world.home.join("Library/Caches/org.rust-lang.miri");
        let at = if link.is_empty() {
            sysroot
        } else {
            sysroot.join(link)
        };
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&world.target, &at).unwrap();
        world.build();
        let setup = words(&["--unverified", "miri", "setup"]);
        assert_eq!(
            produce(&world, &world.owner, false, &[], &setup),
            (None, 1),
            "{link}"
        );
        assert!(!world.target.join("lib").exists(), "{link}");
        assert!(!world.target.join("rustlib").exists(), "{link}");
    }
    // A config Targo reads where it works that names an artifact directory, or that this
    // reader cannot read, leaves `setup`'s output unread; one that names a target or build
    // directory, which that build never reads, leaves the call to the line handed back: a
    // `miri test` owns the config's target, and the build directory beside its own target.
    let setup = words(&["--unverified", "miri", "setup"]);
    let test = words(&["--unverified", "miri", "test"]);
    for (text, setup_judged, test_judged) in [
        ("[build]\nartifact-dir = \"a\"\n", (None, 0), (None, 0)),
        ("include = [\"missing.toml\"]\n", (None, 0), (None, 0)),
        (
            "[build]\nbuild-dir = \"bd\"\n",
            (Some(false), 0),
            (Some(true), 2),
        ),
        (
            "[build]\ntarget-dir = \"out\"\n",
            (Some(false), 0),
            (Some(true), 1),
        ),
    ] {
        let world = World::new();
        config(&world.owner, "config.toml", text);
        assert_eq!(
            produce(&world, &world.owner, false, &[], &setup),
            setup_judged,
            "{text}"
        );
        assert_eq!(
            produce(&world, &world.owner, false, &[], &test),
            test_judged,
            "{text}"
        );
        if text.contains("build-dir") {
            let bd = world.owner.join("bd");
            assert!(
                roots(&world.home)
                    .iter()
                    .any(|registered| registered.root == bd),
                "{text}"
            );
        }
    }
    // A `miri` line's target is held through the folder it writes only inside of also where a
    // build directory is named beside it, by a config file, a `--config` or the variable, the
    // target typed or not: that folder is recorded, never its `miri`, so a later build that
    // names it owns it.
    let sorted_roots = |world: &World| {
        let mut made = made_roots(world);
        made.sort();
        made
    };
    for typed in [false, true] {
        for spelling in ["walk", "cli", "env"] {
            let row = format!("{spelling} typed {typed}");
            let world = World::new();
            let bd = world.owner.join("bd");
            let target = if typed {
                world.home.join("t")
            } else {
                world.owner.join("target")
            };
            let mut args = test.clone();
            if typed {
                args.extend(["--target-dir".to_owned(), target.display().to_string()]);
            }
            let mut more = sysroot();
            match spelling {
                "walk" => config(&world.owner, "config.toml", "[build]\nbuild-dir = \"bd\"\n"),
                "cli" => args.extend([
                    "--config".to_owned(),
                    format!("build.build-dir='{}'", bd.display()),
                ]),
                _ => more.push((super::super::BUILD_DIR_ENV, bd.display().to_string())),
            }
            assert_eq!(
                produce(&world, &world.owner, false, &more, &args),
                (Some(true), 2),
                "{row}"
            );
            let mut want = vec![bd.clone(), target.clone()];
            want.sort();
            assert_eq!(sorted_roots(&world), want, "{row}");
            assert!(!target.join("miri").join(MARKER).exists(), "{row}");
            let _ = std::fs::remove_dir_all(world.owner.join(".cargo"));
            let build = words(&[
                "--unverified",
                "build",
                "--target-dir",
                &target.display().to_string(),
            ]);
            assert_eq!(
                produce(&world, &world.owner, false, &[], &build),
                (Some(true), 2),
                "{row}"
            );
        }
    }
    // A recorded target, a cargo-made `miri` standing in it or none, beside a build directory:
    // the call is its owner, and no root is made inside it.
    for standing in [false, true] {
        let world = World::new();
        world.build();
        if standing {
            cargo_made(&world.target.join("miri"));
        }
        config(&world.owner, "config.toml", "[build]\nbuild-dir = \"bd\"\n");
        let mut args = test.clone();
        args.extend([
            "--target-dir".to_owned(),
            world.target.display().to_string(),
        ]);
        assert_eq!(
            produce(&world, &world.owner, false, &sysroot(), &args),
            (Some(true), 2),
            "standing {standing}"
        );
        assert!(
            !world.target.join("miri").join(MARKER).exists(),
            "standing {standing}"
        );
    }
    // A plain folder the line writes inside of, which no build of cargo's made, leaves the
    // line's own target, its `miri`, to be admitted beside the build directory in its place.
    let world = World::new();
    let plain = world.home.join("plain");
    std::fs::create_dir(&plain).unwrap();
    config(&world.owner, "config.toml", "[build]\nbuild-dir = \"bd\"\n");
    let mut args = test.clone();
    args.extend(["--target-dir".to_owned(), plain.display().to_string()]);
    assert_eq!(
        produce(&world, &world.owner, false, &sysroot(), &args),
        (Some(true), 2)
    );
    let mut want = vec![world.owner.join("bd"), plain.join("miri")];
    want.sort();
    assert_eq!(sorted_roots(&world), want);
    assert!(!plain.join(MARKER).exists());
    // A build directory that cannot be owned (a plain folder no build of cargo's made) refuses
    // the call, and the fresh folder admitted before it takes no root inside it.
    let world = World::new();
    let t = world.home.join("t");
    let bd = world.home.join("x/y/bd");
    std::fs::create_dir_all(&bd).unwrap();
    let mut more = sysroot();
    more.push((super::super::BUILD_DIR_ENV, bd.display().to_string()));
    let mut args = test.clone();
    args.extend(["--target-dir".to_owned(), t.display().to_string()]);
    assert_eq!(
        produce(&world, &world.owner, false, &more, &args),
        (None, 1)
    );
    assert_eq!(made_roots(&world), std::slice::from_ref(&t));
    assert!(!t.join("miri").exists());
    // So does a `TARGO_HOME`'s, the home Trust's main reads where `CARGO_HOME` is unset.
    // NEGATIVE CONTROL: a set `CARGO_HOME` outranks it.
    let world = World::new();
    let twin_home = world.home.join("th");
    std::fs::create_dir(&twin_home).unwrap();
    std::fs::write(
        twin_home.join("config.toml"),
        "[build]\nartifact-dir = \"a\"\n",
    )
    .unwrap();
    let twin = ("TARGO_HOME", twin_home.display().to_string());
    let cargo_home = (
        "CARGO_HOME",
        world.home.join(".cargo").display().to_string(),
    );
    for (more, judged) in [
        (vec![twin.clone()], (None, 0)),
        (vec![twin, cargo_home], (Some(false), 0)),
    ] {
        assert_eq!(
            produce(&world, &world.owner, false, &more, &setup),
            judged,
            "{more:?}"
        );
    }

    // A `--config` value is read as Targo reads it: a file that names nothing, or a target
    // the call's own outranks (the typed `--target-dir`, or `CARGO_TARGET_DIR`), and an inline
    // value whose backslash spells no key, leave the call's target owned, and nothing else. A
    // build directory any value names, hidden from a line scan or not, before the verb or
    // after it, is the call's beside its target, each owned (read as a comment, or the value
    // before the verb alone, it was unrecorded). A folder standing where a value points
    // refuses the call.
    let jobs = "[build]\njobs = 3\n";
    let aside = "[build]\ntarget-dir = \"@ASIDE\"\n";
    let build_dir = "[build]\nbuild-dir = \"@BD\"\n";
    let hidden = "build = { x = \"\"\"\n#\"\"\", build-dir = \"@BD\" }\n";
    let mentions = "[alias]\nd = \"build --artifact-dir d\"\n[build]\njobs = 4 # build-dir\n";
    let long = format!(
        "build --target-dir @T --config env.LONG=\"{}\"",
        "x".repeat(300)
    );
    let mut wrong = Vec::new();
    // Each row: the files in the working directory (a name ending in `/` a folder), the
    // words after `--unverified`, whether `CARGO_TARGET_DIR` names the target, and the
    // verdict.
    for (files, line, targeted, judged) in [
        (
            vec![("o=3", jobs)],
            "build --target-dir @T --config o=3",
            false,
            (Some(true), 1),
        ),
        (
            vec![("aside.toml", aside)],
            "build --target-dir @T --config aside.toml",
            false,
            (Some(true), 1),
        ),
        (
            vec![],
            "build --target-dir @T --config env.X=\"a\\\\b\"",
            false,
            (Some(true), 1),
        ),
        (
            vec![("o=3", jobs)],
            "build --config o=3",
            false,
            (Some(false), 0),
        ),
        (
            vec![("aside.toml", aside)],
            "build --config aside.toml",
            true,
            (Some(true), 1),
        ),
        (
            vec![],
            "build --target-dir @T --config build.\"build-dir\"=\"@BD\"",
            false,
            (Some(true), 2),
        ),
        (
            vec![("hid.toml", hidden)],
            "build --target-dir @T --config hid.toml",
            false,
            (Some(true), 2),
        ),
        (
            vec![("bd.toml", build_dir)],
            "--config bd.toml build --target-dir @T --config o=2",
            false,
            (Some(true), 2),
        ),
        (
            vec![("o=4/", "")],
            "build --target-dir @T --config o=4",
            false,
            (None, 0),
        ),
        // A file is read whole, what its parse names; an inline value too long to name a
        // file is parsed (Targo's `Path::exists` finds nothing at it), and an empty one,
        // which Targo refuses, refuses the call.
        (
            vec![("ci.toml", mentions)],
            "build --target-dir @T --config ci.toml",
            false,
            (Some(true), 1),
        ),
        (vec![], long.as_str(), false, (Some(true), 1)),
        (vec![], "build --target-dir @T --config ", false, (None, 0)),
        // An empty `--target-dir` is the working directory, the project's own folder,
        // which atpkg never makes or adopts.
        (vec![], "build --target-dir=", false, (None, 0)),
        (vec![], "build --target-dir ", false, (None, 0)),
        // `miri` builds into `cargo metadata`'s target, which reads a `--config` past
        // `run`'s program too.
        (
            vec![("aside.toml", aside)],
            "miri run demo --config aside.toml",
            false,
            (None, 0),
        ),
    ] {
        let world = World::new();
        let spelled = |text: &str| {
            text.replace("@T", &world.target.display().to_string())
                .replace("@BD", &world.home.join("bd").display().to_string())
                .replace("@ASIDE", &world.home.join("aside").display().to_string())
        };
        for (name, text) in &files {
            match name.strip_suffix('/') {
                Some(folder) => std::fs::create_dir(world.owner.join(folder)).unwrap(),
                None => std::fs::write(world.owner.join(name), spelled(text)).unwrap(),
            }
        }
        let line = spelled(&format!("--unverified {line}"));
        let args: Vec<String> = line.split(' ').map(str::to_owned).collect();
        let got = produce(&world, &world.owner, targeted, &[], &args);
        if got != judged {
            wrong.push(format!("{line}: {got:?}, not {judged:?}"));
            continue;
        }
        let mut owned = match judged.1 {
            1 => vec![world.target.clone()],
            2 => vec![world.target.clone(), world.home.join("bd")],
            _ => Vec::new(),
        };
        owned.sort();
        let mut made: Vec<_> = roots(&world.home)
            .into_iter()
            .map(|made| made.root)
            .collect();
        made.sort();
        assert_eq!(made, owned, "{line}");
        if judged.1 < 2 {
            assert!(!world.home.join("bd").exists(), "{line}");
        }
        assert!(!world.home.join("aside").exists(), "{line}");
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));

    // A `--config` file outranks inline syntax. An invalid output setting in that file
    // grants no custody; a dotted key no file stands at is neutral.
    let world = World::new();
    let bd = "[build]\nbuild-dir = false\n";
    std::fs::write(world.owner.join("o=1"), bd).unwrap();
    let target = world.target.display().to_string();
    for (value, judged) in [("o=1", (None, 0)), ("o=2", (Some(true), 1))] {
        let args = words(&[
            "--unverified",
            "build",
            "--target-dir",
            &target,
            "--config",
            value,
        ]);
        assert_eq!(
            produce(&world, &world.owner, false, &[], &args),
            judged,
            "{value}"
        );
    }

    // `install` reads the configs of the root it reloads them from: its `--path` joined to
    // the working directory as typed (`..` walked as written), or with none the package home;
    // a `--config` file it reads against the working directory.
    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let noted = format!(
        "[build]\ntarget-dir = \"{}\"\n",
        world.home.join("noted").display()
    );
    std::fs::write(notes.join("o=1"), &noted).unwrap();
    let owner = world.owner.display().to_string();
    let filed = words(&[
        "--unverified",
        "install",
        "--path",
        &owner,
        "--config",
        "o=1",
    ]);
    assert_eq!(produce(&world, &notes, false, &[], &filed), (None, 0));
    config(&notes, "config.toml", &noted);
    let climbing = words(&["--unverified", "install", "--path", "../source"]);
    assert_eq!(produce(&world, &notes, false, &[], &climbing), (None, 0));
    let plain = world.home.join("plain");
    std::fs::create_dir(&plain).unwrap();
    config(&world.owner.join("sub"), "config.toml", &noted);
    let folded = words(&[
        "--unverified",
        "install",
        "--path",
        &format!("{owner}/sub/.."),
    ]);
    assert_eq!(produce(&world, &plain, false, &[], &folded), (None, 0));
    let straight = words(&["--unverified", "install", "--path", &owner]);
    assert_eq!(
        produce(&world, &plain, false, &[], &straight),
        (Some(false), 0)
    );
    // A registry install reads the package home's, never the working directory's.
    let world = World::new();
    world.build();
    let out = format!(
        "[build]\ntarget-dir = \"{}\"\n",
        world.owner.join("out").display()
    );
    config(&world.owner, "config.toml", &out);
    let registry = words(&["--unverified", "install", "ripgrep"]);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &registry),
        (Some(false), 1)
    );
    assert!(!super::read(&world.path()).unwrap().owners_unknown);
    let cargo_home = world.home.join(".local/share/cargo");
    std::fs::create_dir_all(&cargo_home).unwrap();
    let registered = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    config(&world.home.join(".local/share"), "config.toml", &registered);
    let homed = [("CARGO_HOME", cargo_home.display().to_string())];
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    assert_eq!(
        produce(&world, &notes, false, &homed, &registry),
        (Some(false), 1)
    );
    assert!(super::read(&world.path()).unwrap().owners_unknown);
    // A `TARGO_HOME` is the home Trust's main reloads it from where `CARGO_HOME` is unset
    // (Targo 16387 reloads it from `~/.cargo`), so that walk is read too.
    let world = World::new();
    world.build();
    let twin_home = world.home.join(".local/share/targo");
    std::fs::create_dir_all(&twin_home).unwrap();
    let registered = format!("[build]\ntarget-dir = \"{}\"\n", world.target.display());
    config(&world.home.join(".local/share"), "config.toml", &registered);
    let twinned = [("TARGO_HOME", twin_home.display().to_string())];
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    assert_eq!(
        produce(&world, &notes, false, &twinned, &registry),
        (Some(false), 1)
    );
    assert!(super::read(&world.path()).unwrap().owners_unknown);
    // An install from a folder outside HOME reads that folder's configs, which it cannot
    // tell, so every registered root's owners turn unknown.
    let world = World::new();
    world.build();
    let away = world.home.parent().unwrap().join("away");
    std::fs::create_dir(&away).unwrap();
    std::fs::write(away.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    config(
        &away,
        "config.toml",
        &format!("[build]\ntarget-dir = \"{}\"\n", world.target.display()),
    );
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let args = words(&["--unverified", "install", "--path", away.to_str().unwrap()]);
    assert_eq!(produce(&world, &notes, false, &[], &args), (Some(false), 1));
    assert!(super::read(&world.path()).unwrap().owners_unknown);

    // A target named through a link from outside HOME is the folder the kernel reaches: a
    // registered root there loses its known owners though the call owes no custody.
    let world = World::new();
    world.build();
    let away = world.home.parent().unwrap().join("away");
    std::fs::create_dir(&away).unwrap();
    std::fs::write(away.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let link = world.home.parent().unwrap().join("into-home");
    std::os::unix::fs::symlink(&world.target, &link).unwrap();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let args = words(&[
        "--unverified",
        "build",
        "--manifest-path",
        away.join("Cargo.toml").to_str().unwrap(),
        "--target-dir",
        link.to_str().unwrap(),
    ]);
    assert_eq!(produce(&world, &notes, false, &[], &args), (Some(false), 1));
    assert!(super::read(&world.path()).unwrap().owners_unknown);

    // A config's escapes are decoded only where one may spell a key's character: TOML 1.1's
    // `\e` spells none, so its file is read line by line; its `\x` may, and is decoded as
    // Targo decodes it: the key is the target's, which the call's own target outranks, and
    // which is owned where none does.
    let recorded = |world: &World| {
        roots(&world.home)
            .into_iter()
            .map(|record| record.root)
            .collect::<Vec<_>>()
    };
    let built = words(&["--unverified", "build"]);
    let world = World::new();
    config(&world.owner, "config.toml", "[env]\nBOLD = \"\\e[1m\"\n");
    assert_eq!(
        produce(&world, &world.owner, true, &[], &built),
        (Some(true), 1)
    );
    let world = World::new();
    config(
        &world.owner,
        "config.toml",
        "[build]\n\"target\\x2ddir\" = \"t\"\n",
    );
    assert_eq!(
        produce(&world, &world.owner, true, &[], &built),
        (Some(true), 1)
    );
    assert_eq!(recorded(&world), std::slice::from_ref(&world.target));
    let world = World::new();
    config(
        &world.owner,
        "config.toml",
        "[build]\n\"target\\x2ddir\" = \"t\"\n",
    );
    assert_eq!(
        produce(&world, &world.owner, false, &[], &built),
        (Some(true), 1)
    );
    assert_eq!(recorded(&world), [world.owner.join("t")]);

    // An alias config's optional include of a link that names nothing is skipped, as Targo
    // skips it, through the front's own reader, which refuses a link.
    let world = World::new();
    config(
        &world.owner,
        "config.toml",
        "include = [{ path = \"local.toml\", optional = true }]\n[alias]\nxb = \"build\"\n",
    );
    std::os::unix::fs::symlink(
        world.home.join("dotfiles/missing.toml"),
        world.owner.join(".cargo/local.toml"),
    )
    .unwrap();
    let no_follow = |path: &Path| match crate::metadata_io::read_bounded_regular_utf8(path, 1 << 20)
    {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &no_follow,
        read_manifest: &no_follow,
        writable: &|_| Some(true),
    };
    let fresh = world.home.join("fresh").display().to_string();
    let args = words(&["--unverified", "xb", "--target-dir", &fresh]);
    assert_eq!(
        observe_for_producer(&args, Tool::Targo, &world.home, &machine, true).ok(),
        Some(true)
    );
    // Nor does TOML 1.1's `\e`, which spells no key, make an alias untold: the alias a
    // farther config gives is read past a nearer one, read as TOML 1.1 as Targo reads it.
    let world = World::new();
    config(&world.home, "config.toml", "[alias]\nxb = \"build\"\n");
    config(&world.owner, "config.toml", "[env]\nBOLD = \"\\e[1m\"\n");
    let fresh = world.home.join("fresh").display().to_string();
    let args = words(&["--unverified", "xb", "--target-dir", &fresh]);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &args),
        (Some(true), 1)
    );

    // `miri`'s line keeps none of the words typed before its name (Targo hands an external
    // command its own words alone), so a `--config` there names nothing it builds.
    let world = World::new();
    let bd = format!("build.build-dir=\"{}\"", world.home.join("bd").display());
    let t = world.home.join("t").display().to_string();
    let args = words(&[
        "--config",
        &bd,
        "--unverified",
        "miri",
        "run",
        "--target-dir",
        &t,
    ]);
    assert_eq!(
        produce(&world, &world.owner, false, &[], &args),
        (Some(true), 1)
    );
    // A registry install reads the walk from a package home outside HOME as Targo does.
    let world = World::new();
    let away = world.home.parent().unwrap().join("cargo-home");
    std::fs::create_dir(&away).unwrap();
    let homed = [("CARGO_HOME", away.display().to_string())];
    let registry = words(&["--unverified", "install", "ripgrep"]);
    assert_eq!(
        produce(&world, &world.owner, false, &homed, &registry),
        (Some(false), 0)
    );
    // A target that owes no custody, for a package atpkg never manages (in a folder of HOME
    // this platform never looks into), is the folder the kernel reaches: a registered root
    // behind a link from outside HOME loses its known owners.
    let world = World::new();
    world.build();
    let skipped = world
        .home
        .join(crate::build_cache::checkout::HOME_SKIP[0].trim_end_matches('*'))
        .join("proj");
    std::fs::create_dir_all(&skipped).unwrap();
    std::fs::write(skipped.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let link = world.home.parent().unwrap().join("into-home");
    std::os::unix::fs::symlink(&world.target, &link).unwrap();
    let args = words(&[
        "--unverified",
        "build",
        "--target-dir",
        link.to_str().unwrap(),
    ]);
    assert_eq!(
        produce(&world, &skipped, false, &[], &args),
        (Some(false), 1)
    );
    assert!(super::read(&world.path()).unwrap().owners_unknown);
}

/// A USER ALIAS IS READ AS TARGO'S `aliased_command` READS IT: `.cargo/config` over
/// `.cargo/config.toml` in one folder, the nearest config's string over a farther one's, the
/// variable over a config's string; arrays in two configs (which Targo joins), package homes
/// whose readings differ (Targo 16387 reads `CARGO_HOME`'s, Trust's main
/// `TARGO_HOME`'s) and a twin a tool observed as an upstream cargo may read are words the
/// front cannot tell, so the call owes custody. NEGATIVE CONTROL: the plain variable, which
/// every tool reads, is its words.
#[test]
fn a_user_alias_is_read_as_targo_reads_it() {
    use super::super::{Tool, UserAlias};
    let words =
        |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_owned()).collect() };
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let produce = |world: &World, cwd: &Path, more: &[(&str, String)], args: &[String]| {
        let env = |name: &str| {
            (name == "CARGO_TARGET_DIR")
                .then(|| world.target.clone().into_os_string())
                .or_else(|| {
                    more.iter()
                        .find(|(key, _)| *key == name)
                        .map(|(_, value)| value.into())
                })
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(cwd),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let verdict = observe_for_producer(args, Tool::Targo, &world.home, &machine, true).ok();
        (verdict, roots(&world.home).len())
    };
    let config = |dir: &Path, name: &str, text: &str| {
        std::fs::create_dir_all(dir.join(".cargo")).unwrap();
        std::fs::write(dir.join(".cargo").join(name), text).unwrap();
    };
    let outside = |world: &World| {
        let mut args = words(&["--unverified", "b", "-m"]);
        args.push(
            world
                .home
                .parent()
                .unwrap()
                .join("elsewhere/Cargo.toml")
                .display()
                .to_string(),
        );
        args
    };
    // `run -q --` hands the `-m` to the program, so the launcher's package owns the target;
    // `build` keeps it Targo's, outside HOME, which owes none.
    let world = World::new();
    config(&world.owner, "config", "[build]\njobs = 2\n");
    config(&world.owner, "config.toml", "[alias]\nb = \"run -q --\"\n");
    assert_eq!(
        produce(&world, &world.owner, &[], &outside(&world)),
        (Some(false), 0)
    );
    let world = World::new();
    config(
        &world.owner,
        "config.toml",
        "[alias]\nb = \"build --release\"\n",
    );
    config(&world.home, "config.toml", "[alias]\nb = \"run -q --\"\n");
    assert_eq!(
        produce(&world, &world.owner, &[], &outside(&world)),
        (Some(false), 0)
    );
    let world = World::new();
    config(&world.owner, "config.toml", "[alias]\nb = \"build\"\n");
    let variable = [("CARGO_ALIAS_B", "run -q --".to_owned())];
    assert_eq!(
        produce(&world, &world.owner, &variable, &outside(&world)),
        (Some(true), 1)
    );
    let world = World::new();
    config(&world.owner, "config.toml", "alias.b = [\"build\"]\n");
    config(&world.home, "config.toml", "alias.b = [\"build\"]\n");
    assert_eq!(
        produce(&world, &world.owner, &[], &outside(&world)),
        (None, 0)
    );
    // From a folder with no package, a line the front cannot tell owes custody.
    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let twin_home = world.home.join("twin-home");
    config(&world.home, "config.toml", "[build]\njobs = 2\n");
    std::fs::create_dir(&twin_home).unwrap();
    std::fs::write(
        twin_home.join("config.toml"),
        "[alias]\nb = \"run -q --\"\n",
    )
    .unwrap();
    let named = world.owner.join("Cargo.toml").display().to_string();
    let built = words(&["--unverified", "b", "-m", &named]);
    let homes = [("TARGO_HOME", twin_home.display().to_string())];
    assert_eq!(produce(&world, &notes, &homes, &built), (None, 0));
    assert!(!world.target.exists());
    let elsewhere = world
        .home
        .parent()
        .unwrap()
        .join("elsewhere/Cargo.toml")
        .display()
        .to_string();
    // The variable's text is words, whatever they spell, as Targo reads it as a string;
    // one that is no UTF-8 text is one Targo does not see, so a config's string is read,
    // and with none the alias cannot be told. A file included twice is a cycle Targo
    // refuses.
    {
        let world = World::new();
        let unreadable =
            || <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(vec![0xff]);
        let strings =
            |words: &[&str]| UserAlias::Words(words.iter().map(|w| (*w).to_owned()).collect());
        for (value, configured, want) in [
            (std::ffi::OsString::from("1"), false, strings(&["1"])),
            (std::ffi::OsString::from("true"), false, strings(&["true"])),
            (unreadable(), true, strings(&["build"])),
            (unreadable(), false, UserAlias::Unknown),
        ] {
            let cargo = world.owner.join(".cargo");
            if configured {
                config(&world.owner, "config.toml", "[alias]\nxb = \"build\"\n");
            } else if cargo.exists() {
                std::fs::remove_dir_all(&cargo).unwrap();
            }
            let lookup = |key: &str| (key == "CARGO_ALIAS_XB").then(|| value.clone());
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&world.owner),
                env: &lookup,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            assert_eq!(
                user_alias("xb", &[], Tool::Targo, &machine),
                want,
                "{value:?}"
            );
        }
        config(
            &world.owner,
            "config.toml",
            "include = [\"a.toml\", \"a.toml\"]\n",
        );
        config(&world.owner, "a.toml", "[alias]\nxb = \"build\"\n");
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        assert_eq!(
            user_alias("xb", &[], Tool::Targo, &machine),
            UserAlias::Unknown
        );
        config(&world.owner, "config.toml", "include = [\"a.toml\"]\n");
        assert_eq!(
            user_alias("xb", &[], Tool::Targo, &machine),
            strings(&["build"])
        );
        // An include Targo refuses (`include_paths`): no list, a path that is no `.toml`
        // or holds a glob, a table with no string path or a non-boolean `optional`. A
        // table's other keys it ignores.
        config(&world.owner, "a.conf", "[alias]\nxb = \"build\"\n");
        config(&world.owner, "a*.toml", "[alias]\nxb = \"build\"\n");
        for (include, want) in [
            ("include = \"a.toml\"\n", UserAlias::Unknown),
            ("include = [\"a.conf\"]\n", UserAlias::Unknown),
            ("include = [\"a*.toml\"]\n", UserAlias::Unknown),
            ("include = [{ optional = true }]\n", UserAlias::Unknown),
            (
                "include = [{ path = \"a.toml\", optional = \"yes\" }]\n",
                UserAlias::Unknown,
            ),
            (
                "include = [{ path = \"a.toml\", note = \"x\" }]\n",
                strings(&["build"]),
            ),
        ] {
            config(&world.owner, "config.toml", include);
            assert_eq!(
                user_alias("xb", &[], Tool::Targo, &machine),
                want,
                "{include}"
            );
        }
        // An upstream cargo reads `include` only from 1.93: what one defines is untold.
        config(&world.owner, "config.toml", "include = [\"a.toml\"]\n");
        assert_eq!(
            user_alias("xb", &[], Tool::Cargo, &machine),
            UserAlias::Unknown
        );
    }
    for (tool, variable, cwd, manifest, owes) in [
        (Tool::Cargo, "TARGO_ALIAS_B", &notes, &named, true),
        (Tool::Cargo, "CARGO_ALIAS_B", &notes, &named, false),
        (Tool::Targo, "TARGO_ALIAS_B", &notes, &named, false),
        // A twin the compat `cargo` may read cannot be told from none, which would read the
        // `-m` outside HOME as Targo's.
        (Tool::Cargo, "TARGO_ALIAS_B", &world.owner, &elsewhere, true),
        (
            Tool::Cargo,
            "CARGO_ALIAS_X",
            &world.owner,
            &elsewhere,
            false,
        ),
    ] {
        let target = world.target.clone().into_os_string();
        let lookup = |key: &str| match key {
            "CARGO_TARGET_DIR" => Some(target.clone()),
            _ if key == variable => Some("run -q --".into()),
            _ => None,
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(cwd),
            env: &lookup,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let line = if tool == Tool::Cargo {
            words(&["+stable", "b", "-m", manifest])
        } else {
            words(&["--unverified", "b", "-m", manifest])
        };
        assert_eq!(
            requires_managed_explicit(&line, tool, &world.home, &machine),
            owes,
            "{tool:?} {variable}"
        );
    }
}

/// A FREE VERB STAYS FREE WITH ITS `-C` OR `-Z` IN A CLUSTER, which reads as the spaced
/// spelling (`-qC dir pkgid` is `-q -C dir pkgid`), and with a cluster of flags before it.
/// NEGATIVE CONTROL: a building verb after one, a cluster's `-C` with no value, and a cluster
/// ending in another letter that takes a value prove nothing free.
#[test]
fn a_free_verb_reads_its_options_clustered_as_spaced() {
    use super::super::Tool;
    let world = World::new();
    let other = world.another_owner().display().to_string();
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| Some(true),
    };
    let free = |args: &[&str]| {
        let args: Vec<String> = args
            .iter()
            .map(|word| word.replace("{o}", &other))
            .collect();
        non_producing_command(&args, Tool::Targo, &machine)
    };
    for args in [
        &["-qC", "{o}", "pkgid"][..],
        &["-vC", "{o}", "clean"],
        &["-qC", "{o}", "help"],
        &["-qC/x", "locate-project"],
        &["-vZ", "unstable-options", "version"],
        &["-qvZunstable-options", "pkgid"],
        &["-qv", "verify-project"],
    ] {
        assert!(free(args), "{args:?}");
    }
    // A resolving verb run in another directory reads that directory's configs, which may
    // put its lock root in a target: `-C` in a cluster is the spaced `-C` there too.
    for args in [
        &["-qC", "{o}", "build"][..],
        &["-qC"],
        &["-qp", "x", "pkgid"],
        &["-qC", "{o}", "generate-lockfile"],
        &["-vC{o}", "fetch"],
        &["-qC/x", "tree"],
        &["-q", "-C", "{o}", "metadata"],
        &["-qC", "{o}", "metadata"],
    ] {
        assert!(!free(args), "{args:?}");
    }
    let args: Vec<String> = ["-qC", &other, "pkgid"].map(str::to_owned).to_vec();
    assert!(!observe_for_producer(&args, Tool::Targo, &world.home, &machine, true).unwrap());
}

/// THE CONFIGS A CALL NAMING A MANIFEST OWES CUSTODY BY ARE THE ONES TARGO READS: the working
/// directory's, whatever manifest it names, and the package homes resolved against the
/// working directory, not the manifest's folder. A build directory in the launcher's config
/// or a target in a relative `CARGO_HOME`'s config (or `TARGO_HOME`'s) owes custody with a
/// manifest named in each spelling. NEGATIVE CONTROL: a config only in a home of that name
/// beside the manifest, which Targo never reads, owes none.
#[test]
fn a_named_manifest_owes_custody_by_the_configs_targo_reads() {
    use super::super::Tool;
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let owes = |home: &Path,
                cwd: &Path,
                env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
                args: &[String]| {
        let machine = super::super::Machine {
            home: Some(home),
            root: None,
            cwd: Some(cwd),
            env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        requires_managed_explicit(args, Tool::Targo, home, &machine)
    };
    let named = |manifest: &Path| -> [Vec<String>; 3] {
        let manifest = manifest.display().to_string();
        [
            vec![
                "--unverified".into(),
                "build".into(),
                "--manifest-path".into(),
                manifest.clone(),
            ],
            vec![
                "--unverified".into(),
                "build".into(),
                "-m".into(),
                manifest.clone(),
            ],
            vec![
                "--unverified".into(),
                "build".into(),
                format!("-qm{manifest}"),
            ],
        ]
    };
    let none = |_: &str| None;

    // The launcher's own config names a build directory; the manifest is another package's.
    let world = World::new();
    let other = world.another_owner().join("Cargo.toml");
    for args in named(&other) {
        assert!(!owes(&world.home, &world.owner, &none, &args), "{args:?}");
    }
    std::fs::create_dir(world.owner.join(".cargo")).unwrap();
    std::fs::write(
        world.owner.join(".cargo/config.toml"),
        "[build]\nbuild-dir = 'b'\n",
    )
    .unwrap();
    for args in named(&other) {
        assert!(owes(&world.home, &world.owner, &none, &args), "{args:?}");
    }

    // A relative package home, from a launcher outside HOME: Targo reads it there.
    let world = World::new();
    let launcher = world.home.parent().unwrap().join("launcher");
    std::fs::create_dir_all(launcher.join(".ch")).unwrap();
    let manifest = world.owner.join("Cargo.toml");
    for var in ["CARGO_HOME", "TARGO_HOME"] {
        let relative = |name: &str| (name == var).then(|| std::ffi::OsString::from(".ch"));
        for args in named(&manifest) {
            assert!(
                !owes(&world.home, &launcher, &relative, &args),
                "{var} {args:?}"
            );
        }
        std::fs::create_dir_all(world.owner.join(".ch")).unwrap();
        std::fs::write(
            world.owner.join(".ch/config.toml"),
            "[build]\ntarget-dir = 't'\n",
        )
        .unwrap();
        for args in named(&manifest) {
            assert!(
                !owes(&world.home, &launcher, &relative, &args),
                "{var} {args:?}"
            );
        }
        std::fs::remove_file(world.owner.join(".ch/config.toml")).unwrap();
        std::fs::write(
            launcher.join(".ch/config.toml"),
            "[build]\ntarget-dir = 't'\n",
        )
        .unwrap();
        for args in named(&manifest) {
            assert!(
                owes(&world.home, &launcher, &relative, &args),
                "{var} {args:?}"
            );
        }
        std::fs::remove_file(launcher.join(".ch/config.toml")).unwrap();
    }
}

#[test]
fn builtin_queries_leave_registered_receipts_unchanged_even_when_writes_are_refused() {
    struct RestorePermissions<'a>(&'a Path, std::fs::Permissions);
    impl Drop for RestorePermissions<'_> {
        fn drop(&mut self) {
            std::fs::set_permissions(self.0, self.1.clone()).unwrap();
        }
    }

    let world = World::new();
    world.build();
    let path = world.path();
    let before = std::fs::read(&path).unwrap();
    let marker = std::fs::read(world.target.join(MARKER)).unwrap();
    let fresh = world.home.join("query-only.noindex");
    let config = world.owner.join(".cargo/config.toml");
    let env =
        |name: &str| (name == "CARGO_TARGET_DIR").then(|| world.target.as_os_str().to_owned());
    let read_config = |path: &Path| {
        Ok((path == config)
            .then(|| format!("[build]\ntarget-dir = '{}'\n", world.target.display())))
    };
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &env,
        read: &read_config,
        read_manifest: &read_config,
        writable: &|_| Some(false),
    };
    let index = path.parent().unwrap();
    let permissions = RestorePermissions(index, std::fs::metadata(index).unwrap().permissions());
    std::fs::set_permissions(index, std::fs::Permissions::from_mode(0o500)).unwrap();
    // The held gate also makes a mistaken owner observation fail before writing, even
    // when a test runner has privileges that let it ignore the directory's mode.
    let gate = crate::metadata_io::open_regular(&gate_path(&path).unwrap()).unwrap();
    let held = crate::lock::Flock::try_lock(gate).unwrap();
    for (tool, args) in [
        (super::super::Tool::Targo, vec!["metadata".into()]),
        (super::super::Tool::Targo, vec!["-vV".into()]),
        (super::super::Tool::Targo, vec!["-Vv".into()]),
        (super::super::Tool::Targo, vec!["-vvV".into()]),
        // No verb: Targo answers with its help.
        (super::super::Tool::Targo, vec!["-vV-not-a-query".into()]),
        (super::super::Tool::Targo, vec!["--list".into()]),
        (
            super::super::Tool::Targo,
            vec!["--explain".into(), "E0499".into(), "build".into()],
        ),
        (
            super::super::Tool::Targo,
            vec!["--unverified".into(), "build".into(), "-Zhelp".into()],
        ),
        (
            super::super::Tool::Targo,
            vec!["--version".into(), "--verbose".into()],
        ),
        (
            super::super::Tool::Cargo,
            vec!["+nightly".into(), "-vV".into()],
        ),
        (
            super::super::Tool::Targo,
            vec![
                "--unverified".into(),
                "metadata".into(),
                "--format-version=1".into(),
                format!("--target-dir={}", fresh.display()),
            ],
        ),
        (
            super::super::Tool::Cargo,
            vec![
                "+nightly".into(),
                "--config".into(),
                "net.offline=true".into(),
                "-Z".into(),
                "unstable-options".into(),
                "metadata".into(),
            ],
        ),
        (
            super::super::Tool::Targo,
            vec!["locate-project".into(), "--workspace".into()],
        ),
        (super::super::Tool::Cargo, vec!["version".into()]),
        (
            super::super::Tool::Targo,
            vec!["help".into(), "build".into()],
        ),
        // A protected tool answers its own words: the lint its version.
        (
            super::super::Tool::Targo,
            vec!["tippy".into(), "-vV".into()],
        ),
        (super::super::Tool::Tippy, vec!["-vV".into()]),
    ] {
        assert!(
            !observe_on(&args, tool, &world.home, &machine).unwrap(),
            "{tool:?} {args:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "{args:?}");
        assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
        assert!(!fresh.exists());
    }
    for (tool, words) in [
        (
            super::super::Tool::Targo,
            vec!["--unverified", "metadata-alias"],
        ),
        (
            super::super::Tool::Targo,
            vec!["--config", "metadata", "build"],
        ),
        (
            super::super::Tool::Targo,
            vec!["--unknown-option", "metadata"],
        ),
        (
            super::super::Tool::Targo,
            vec!["--unverified", "run", "--", "metadata"],
        ),
        (super::super::Tool::Tippy, vec!["metadata"]),
        // A config file, or another directory's configs, may place the lockfile in a target.
        (
            super::super::Tool::Cargo,
            vec![
                "+nightly",
                "--config",
                "elsewhere.toml",
                "-Z",
                "unstable-options",
                "-C",
                "another-directory",
                "metadata",
            ],
        ),
        (super::super::Tool::Targo, vec!["--color", "-vV", "build"]),
        (super::super::Tool::Targo, vec!["--list", "b"]),
        (
            super::super::Tool::Targo,
            vec!["--unverified", "an-unknown-alias", "-vV"],
        ),
        (
            super::super::Tool::Targo,
            vec!["--unverified", "run", "--", "-vV"],
        ),
        // `miri run` hands `-h` to the program after its name; `--` hands Targo a protected
        // tool, which it runs.
        (
            super::super::Tool::Targo,
            vec!["--unverified", "miri", "run", "demo", "-h"],
        ),
        (super::super::Tool::Targo, vec!["--", "clippy", "-q"]),
        (
            super::super::Tool::Targo,
            vec!["--config", "clean", "--unverified", "build"],
        ),
        (
            super::super::Tool::Targo,
            vec!["--unverified", "clean-alias"],
        ),
        (
            super::super::Tool::Targo,
            vec!["--unverified", "run", "--", "clean"],
        ),
        (super::super::Tool::Tippy, vec!["clean"]),
    ] {
        let args = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(
            observe_on(&args, tool, &world.home, &machine).is_err(),
            "a possible producer must still observe its owners: {tool:?} {args:?}"
        );
    }
    drop(held);
    drop(permissions);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!read(&path).unwrap().owners_unknown);
}

/// THE VERBS THAT WRITE NO BUILD OUTPUT OWE NO OWNERSHIP, in every spelling of a target: each
/// of [`READING_VERBS`] and [`RESOLVING_VERBS`], named literally here, with a registered
/// target in the environment, a fresh one by `--target-dir` (both forms), a `-Z` option,
/// `--config build.target-dir=…` (both forms) or `build.build-dir`, upstream cargo's
/// `+toolchain`, is observed as nothing: its receipt and marker unchanged and no target made,
/// under a gate held and an index that refuses writes, so a mistaken observation fails.
/// NEGATIVE CONTROLS: the same verb as an option's value, past `--`, after a building verb,
/// behind `trust`, as a lint's argument, or a name that only begins like it, is still
/// observed; so is a resolving verb whose lockfile may land in a target, by an embedded
/// manifest or a `resolver.lockfile-path` in each place a call can name one, while a reading
/// verb with the same words stays free.
#[test]
fn output_free_verbs_owe_no_ownership_in_every_target_spelling() {
    struct RestorePermissions<'a>(&'a Path, std::fs::Permissions);
    impl Drop for RestorePermissions<'_> {
        fn drop(&mut self) {
            std::fs::set_permissions(self.0, self.1.clone()).unwrap();
        }
    }

    let world = World::new();
    world.build();
    let path = world.path();
    let before = std::fs::read(&path).unwrap();
    let marker = std::fs::read(world.target.join(MARKER)).unwrap();
    let fresh = world.home.join("fetch-only.noindex");
    let env =
        |name: &str| (name == "CARGO_TARGET_DIR").then(|| world.target.as_os_str().to_owned());
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &env,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| Some(false),
    };
    let index = path.parent().unwrap();
    let permissions = RestorePermissions(index, std::fs::metadata(index).unwrap().permissions());
    std::fs::set_permissions(index, std::fs::Permissions::from_mode(0o500)).unwrap();
    let gate = crate::metadata_io::open_regular(&gate_path(&path).unwrap()).unwrap();
    let held = crate::lock::Flock::try_lock(gate).unwrap();
    let fresh_at = fresh.display().to_string();
    let config = format!("build.target-dir='{fresh_at}'");
    for verb in [
        "fetch",
        "tree",
        "metadata",
        "generate-lockfile",
        "update",
        "pkgid",
        "verify-project",
        "read-manifest",
        "locate-project",
        "version",
    ] {
        let spellings: [(super::super::Tool, Vec<String>); 10] = [
            (
                super::super::Tool::Targo,
                vec![verb.into(), "--locked".into()],
            ),
            (
                super::super::Tool::Targo,
                vec![
                    "--unverified".into(),
                    verb.into(),
                    "--target-dir".into(),
                    fresh_at.clone(),
                ],
            ),
            (
                super::super::Tool::Targo,
                vec![verb.into(), format!("--target-dir={fresh_at}")],
            ),
            (
                super::super::Tool::Targo,
                vec![
                    "-Z".into(),
                    "unstable-options".into(),
                    verb.into(),
                    "--target-dir".into(),
                    fresh_at.clone(),
                ],
            ),
            (
                super::super::Tool::Targo,
                vec![
                    "-Zbuild-std".into(),
                    "--offline".into(),
                    verb.into(),
                    "-Zgc".into(),
                ],
            ),
            (
                super::super::Tool::Targo,
                vec!["--config".into(), config.clone(), verb.into()],
            ),
            (
                super::super::Tool::Targo,
                vec![
                    format!("--config={config}"),
                    "--unverified".into(),
                    verb.into(),
                ],
            ),
            (
                super::super::Tool::Targo,
                vec![
                    "--config".into(),
                    format!("build.build-dir='{fresh_at}'"),
                    verb.into(),
                    "--config".into(),
                    config.clone(),
                ],
            ),
            (
                super::super::Tool::Cargo,
                vec![
                    "+nightly".into(),
                    verb.into(),
                    format!("--target-dir={fresh_at}"),
                ],
            ),
            (
                super::super::Tool::Cargo,
                vec![
                    "-v".into(),
                    "--frozen".into(),
                    verb.into(),
                    "--target-dir".into(),
                    fresh_at.clone(),
                ],
            ),
        ];
        for (tool, args) in spellings {
            assert!(
                non_producing_command(&args, tool, &machine),
                "{tool:?} {args:?}"
            );
            assert!(
                !observe_for_producer(&args, tool, &world.home, &machine, true).unwrap(),
                "{tool:?} {args:?}"
            );
            assert!(
                !observe_on(&args, tool, &world.home, &machine).unwrap(),
                "{args:?}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before, "{args:?}");
            assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
            assert!(!fresh.exists(), "{args:?}");
        }
    }
    // Targo's `miri clean` (`cargo-miri`'s `MiriCommand::Clean`) runs `targo metadata
    // --no-deps` and removes its target's `miri` and miri's own sysroot cache: it writes
    // nothing, whatever target or `--config` the call names, as `clean` writes nothing.
    for words in [
        vec!["--unverified", "miri", "clean"],
        vec!["--unverified", "miri", "clean", "--target-dir", &fresh_at],
        vec!["--config", &config, "--unverified", "miri", "clean"],
        vec!["miri", "clean"],
    ] {
        let args = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let tool = super::super::Tool::Targo;
        assert!(non_producing_command(&args, tool, &machine), "{args:?}");
        assert!(
            !observe_for_producer(&args, tool, &world.home, &machine, true).unwrap(),
            "{args:?}"
        );
        assert!(
            !observe_on(&args, tool, &world.home, &machine).unwrap(),
            "{args:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "{args:?}");
        assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
        assert!(!fresh.exists(), "{args:?}");
    }
    // Targo's `miri setup` builds only miri's sysroot, from the standard library's sources,
    // through a Targo of its own whose workspace, target and build directory lie in the
    // temporary folder, which its builder sets over every variable and config, the
    // `CARGO_TARGET_DIR` naming the registered target here included, and whose result it
    // stages inside the sysroot: where both folders lie outside every folder atpkg manages
    // and nothing names an artifact directory, it writes nothing atpkg could record, and a
    // call in no lane Targo refuses.
    for words in [
        vec!["--unverified", "miri", "setup"],
        vec!["--unverified", "miri", "setup", "--print-sysroot"],
        vec!["miri", "setup"],
    ] {
        let args = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let tool = super::super::Tool::Targo;
        assert!(non_producing_command(&args, tool, &machine), "{args:?}");
        assert!(
            !observe_for_producer(&args, tool, &world.home, &machine, true).unwrap(),
            "{args:?}"
        );
        assert!(
            !observe_on(&args, tool, &world.home, &machine).unwrap(),
            "{args:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "{args:?}");
        assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
    }
    for (tool, words) in [
        // `cargo-miri` reads its subcommand only as the word after `miri`, and an upstream
        // cargo's `miri` is any `cargo-miri` on PATH, which this reader does not read.
        (
            super::super::Tool::Targo,
            vec!["--unverified", "miri", "-v", "clean"],
        ),
        (
            super::super::Tool::Targo,
            vec!["--unverified", "miri", "cleanup"],
        ),
        (super::super::Tool::Cargo, vec!["miri", "clean"]),
        (super::super::Tool::Cargo, vec!["miri", "setup"]),
        (
            super::super::Tool::Targo,
            vec!["--config", "fetch", "build"],
        ),
        (
            super::super::Tool::Targo,
            vec!["--unverified", "run", "--", "fetch"],
        ),
        (
            super::super::Tool::Targo,
            vec!["--unverified", "build", "fetch"],
        ),
        (super::super::Tool::Targo, vec!["trust", "fetch"]),
        (
            super::super::Tool::Targo,
            vec!["--unverified", "fetch-alias"],
        ),
        (super::super::Tool::Targo, vec!["--unverified", "treehouse"]),
        (super::super::Tool::Tippy, vec!["fetch"]),
        (super::super::Tool::Tippy, vec!["tree"]),
    ] {
        let args = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(
            !non_producing_command(&args, tool, &machine),
            "{tool:?} {args:?}"
        );
        assert!(
            observe_on(&args, tool, &world.home, &machine).is_err(),
            "a possible producer must still observe its owners: {tool:?} {args:?}"
        );
    }
    let lockfile = fresh.join("Cargo.lock").display().to_string();
    let config_file = world.owner.join(".cargo").join("config.toml");
    let named_file = world.owner.join("net.retry=2");
    let benign_file = world.owner.join("ci.toml");
    let words = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect();
    // Words before the verb, words after it, `CARGO_RESOLVER_LOCKFILE_PATH`, and what the
    // workspace's own config reads as.
    type Case = (
        Vec<String>,
        Vec<String>,
        Option<String>,
        Result<Option<String>, ()>,
    );
    let cases: Vec<Case> = vec![
        (
            vec![],
            words(&["--manifest-path", "script.rs"]),
            None,
            Ok(None),
        ),
        (
            vec![],
            words(&["--manifest-path=tools/helper"]),
            None,
            Ok(None),
        ),
        (
            words(&["--manifest-path", "/elsewhere/build.rs"]),
            vec![],
            None,
            Ok(None),
        ),
        (
            vec![],
            vec![
                "--config".into(),
                format!("resolver.lockfile-path='{lockfile}'"),
            ],
            None,
            Ok(None),
        ),
        (
            vec![format!(
                "--config=resolver.\"\\u006cockfile-path\"='{lockfile}'"
            )],
            vec![],
            None,
            Ok(None),
        ),
        (
            vec![],
            words(&["--config", "include='more.toml'"]),
            None,
            Ok(None),
        ),
        (vec![], words(&["--config", "net.retry=2"]), None, Ok(None)),
        (vec![], words(&["--config"]), None, Ok(None)),
        (
            words(&["-Z", "unstable-options", "-C", "elsewhere"]),
            vec![],
            None,
            Ok(None),
        ),
        (words(&["-Celsewhere"]), vec![], None, Ok(None)),
        (
            vec![],
            words(&["--lockfile-path", "x/Cargo.lock"]),
            None,
            Ok(None),
        ),
        (vec![], vec![], Some(lockfile.clone()), Ok(None)),
        (
            vec![],
            vec![],
            None,
            Ok(Some(format!("[resolver]\nlockfile-path = '{lockfile}'\n"))),
        ),
        (
            vec![],
            vec![],
            None,
            Ok(Some("include = ['more.toml']\n".into())),
        ),
        (
            vec![],
            vec![],
            None,
            Ok(Some("[resolver]\n\"\\u006cockfile-path\" = 'x'\n".into())),
        ),
        (vec![], vec![], None, Err(())),
        (vec![], words(&["-m", "script.rs"]), None, Ok(None)),
        (vec![], words(&["-mtools/helper"]), None, Ok(None)),
        (vec![], words(&["-m=helper.rs"]), None, Ok(None)),
        (vec![], words(&["-qm", "sub/Cargo.toml"]), None, Ok(None)),
        (
            vec![],
            vec![],
            None,
            Ok(Some("[resolver\nlockfile-path = 'x'\n".into())),
        ),
    ];
    // Calls whose words or configs only look like those: an ordinary manifest by either
    // spelling and either name, a long flag or a `-Z` option holding an `m`, another resolver
    // key, a config file that sets neither, a value naming nothing that stands and no lock
    // root, a backslash in a value (as aterm's own `.cargo/config.toml` has one), a key named
    // only in a comment, and TOML 1.1 that names neither.
    let free: Vec<Case> = vec![
        (vec![], words(&["-m", "sub/Cargo.toml"]), None, Ok(None)),
        (vec![], words(&["-m", "sub/Targo.toml"]), None, Ok(None)),
        (vec![], words(&["--manifest-path", "sub/other.toml"]), None, Ok(None)),
        (words(&["-Zminimal-versions"]), vec![], None, Ok(None)),
        (words(&["--config", "ci.toml"]), vec![], None, Ok(None)),
        (words(&["--config", "extra.toml"]), vec![], None, Ok(None)),
        (vec![], vec![], None, Ok(Some("net = { retry = 2, }\n".into()))),
        (
            vec![],
            words(&["--manifest-path=sub/Cargo.toml", "--format-version=1"]),
            None,
            Ok(None),
        ),
        (
            vec![],
            words(&["--config", "resolver.feature-unification=\"workspace\""]),
            None,
            Ok(None),
        ),
        (
            vec![],
            vec![],
            None,
            Ok(Some(
                "[alias]\nverify = \"run --config resolver.feature-unification=\\\"workspace\\\" -p aterm-verify --\"\n"
                    .into(),
            )),
        ),
        (
            vec![],
            vec![],
            None,
            Ok(Some(
                "[resolver]\nfeature-unification = \"workspace\" # lockfile-path = 'x'\n".into(),
            )),
        ),
    ];
    let cases = cases
        .into_iter()
        .map(|case| (case, true))
        .chain(free.into_iter().map(|case| (case, false)));
    for ((before_verb, after_verb, lock_env, config), placed) in cases {
        let env = |name: &str| match name {
            "CARGO_TARGET_DIR" => Some(world.target.as_os_str().to_owned()),
            "CARGO_RESOLVER_LOCKFILE_PATH" => lock_env.clone().map(Into::into),
            _ => None,
        };
        let read = |path: &Path| -> Result<Option<String>, ()> {
            if path == config_file {
                config.clone()
            } else if path == named_file {
                Ok(Some(format!("[resolver]\nlockfile-path = '{lockfile}'\n")))
            } else {
                Ok((path == benign_file).then(|| "[net]\nretry = 2\n".into()))
            }
        };
        let placing = super::super::Machine {
            env: &env,
            read: &read,
            read_manifest: &read,
            ..machine
        };
        for verb in READING_VERBS.iter().chain(RESOLVING_VERBS) {
            let mut args = before_verb.clone();
            args.push((*verb).to_owned());
            args.extend(after_verb.iter().cloned());
            let observed_as_producer = placed && RESOLVING_VERBS.contains(verb);
            assert_eq!(
                non_producing_command(&args, super::super::Tool::Targo, &placing),
                !observed_as_producer,
                "{args:?} {lock_env:?} {config:?}"
            );
            let observed = observe_for_producer(
                &args,
                super::super::Tool::Targo,
                &world.home,
                &placing,
                true,
            );
            assert_eq!(
                observed.is_err(),
                observed_as_producer,
                "{args:?} {observed:?}"
            );
            assert!(observed.is_err() || !observed.unwrap(), "{args:?}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{args:?}");
            assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
            assert!(!fresh.exists(), "{args:?}");
        }
    }
    // Each package home's config counts too: `CARGO_HOME` (against the working directory), or
    // `~/.cargo` where it is empty or unset, and its `TARGO_HOME` twin where it is unset.
    // Trust's main reads that twin's, Targo 16387 `~/.cargo`'s, so both count.
    let package_home = world.home.join("package-home");
    let package_home_at = package_home.as_os_str().to_owned();
    // Read from outside the home, so no ancestor's config stands in for the package home's.
    let elsewhere = world.home.parent().unwrap().join("elsewhere");
    let dot_cargo = world.home.join(".cargo").join("config");
    for (cargo_home, targo_home, config) in [
        (
            Some(package_home_at.clone()),
            None,
            package_home.join("config.toml"),
        ),
        (
            Some("../home/package-home".into()),
            None,
            package_home.join("config.toml"),
        ),
        (
            None,
            Some(package_home_at.clone()),
            package_home.join("config"),
        ),
        (None, Some(package_home_at.clone()), dot_cargo.clone()),
        (
            Some("".into()),
            Some(package_home_at.clone()),
            dot_cargo.clone(),
        ),
        (None, None, dot_cargo.clone()),
    ] {
        let env = |name: &str| match name {
            "CARGO_HOME" => cargo_home.clone(),
            "TARGO_HOME" => targo_home.clone(),
            _ => None,
        };
        // Paths compare as Targo would read them: the relative home joined to the cwd.
        let read = |path: &Path| {
            let path = normalize_with_cwd(path.to_path_buf(), None).unwrap();
            Ok((path == config).then(|| "[resolver]\nlockfile-path = 'x/Cargo.lock'\n".into()))
        };
        let homed = super::super::Machine {
            cwd: Some(&elsewhere),
            env: &env,
            read: &read,
            read_manifest: &read,
            ..machine
        };
        assert!(
            lockfile_may_land_in_a_target(&words(&["fetch"]), &homed),
            "{cargo_home:?} {targo_home:?}"
        );
        let read = |_: &Path| Ok(None);
        let plain = super::super::Machine {
            read: &read,
            read_manifest: &read,
            ..homed
        };
        assert!(!lockfile_may_land_in_a_target(&words(&["fetch"]), &plain));
    }
    // An explicit `CARGO_HOME` hides its `TARGO_HOME` twin, as in Targo.
    let env = |name: &str| match name {
        "CARGO_HOME" => Some(world.home.join("chosen").into_os_string()),
        "TARGO_HOME" => Some(package_home_at.clone()),
        _ => None,
    };
    let read = |path: &Path| {
        Ok(path
            .starts_with(&package_home)
            .then(|| "include = ['x.toml']\n".into()))
    };
    let twinned = super::super::Machine {
        cwd: Some(&elsewhere),
        env: &env,
        read: &read,
        read_manifest: &read,
        ..machine
    };
    assert!(!lockfile_may_land_in_a_target(&words(&["fetch"]), &twinned));
    // A lockfile path in the `TARGO_` twin of its variable, and a call with no working
    // directory to read configs from, may place the lockfile too.
    let env = |name: &str| (name == "TARGO_RESOLVER_LOCKFILE_PATH").then(|| "x/Cargo.lock".into());
    let read = |_: &Path| Ok(None);
    let twin = super::super::Machine {
        env: &env,
        read: &read,
        read_manifest: &read,
        ..machine
    };
    assert!(lockfile_may_land_in_a_target(&words(&["fetch"]), &twin));
    let env = |_: &str| None;
    let nowhere = super::super::Machine {
        cwd: None,
        env: &env,
        read: &read,
        read_manifest: &read,
        ..machine
    };
    assert!(lockfile_may_land_in_a_target(&words(&["fetch"]), &nowhere));
    // A config that is a link (`.cargo/config -> config.toml`), which atpkg's own reader
    // refuses, is read at its canonical path, as Targo reads it; a dangling one is a file that
    // does not stand, as Targo's `get_file_path` opens it (`same_file::Handle::from_path`).
    let linked = world.home.join("linked-config");
    std::fs::create_dir_all(linked.join(".cargo")).unwrap();
    std::os::unix::fs::symlink("config.toml", linked.join(".cargo").join("config")).unwrap();
    let no_follow = |path: &Path| match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(()),
        Ok(_) => Ok(Some(std::fs::read_to_string(path).unwrap())),
        Err(_) => Ok(None),
    };
    let env = |_: &str| None;
    let through = super::super::Machine {
        cwd: Some(&linked),
        env: &env,
        read: &no_follow,
        read_manifest: &no_follow,
        ..machine
    };
    for (text, placed) in [
        ("[resolver]\nfeature-unification = 'workspace'\n", false),
        ("[resolver]\nlockfile-path = 'x/Cargo.lock'\n", true),
    ] {
        std::fs::write(linked.join(".cargo").join("config.toml"), text).unwrap();
        assert_eq!(
            lockfile_may_land_in_a_target(&words(&["fetch"]), &through),
            placed,
            "{text}"
        );
    }
    std::fs::remove_file(linked.join(".cargo").join("config.toml")).unwrap();
    assert!(!lockfile_may_land_in_a_target(&words(&["fetch"]), &through));
    // Text is read as TOML 1.1, as Targo parses it: `\e` spells no key, a key `\x` spells is
    // the lockfile's root, and a word in a comment of an inline table across lines sets
    // nothing (read as TOML 1.0, the text could not be parsed, and the word might).
    for (text, placed) in [
        ("[env]\nBOLD = \"\\e[1m\"\n", false),
        ("[resolver]\n\"lock\\x66ile-path\" = 'x/Cargo.lock'\n", true),
        ("[env]\nA = { B = \"1\", # no include\n}\n", false),
    ] {
        std::fs::write(linked.join(".cargo").join("config.toml"), text).unwrap();
        assert_eq!(
            lockfile_may_land_in_a_target(&words(&["fetch"]), &through),
            placed,
            "{text}"
        );
    }
    drop(held);
    drop(permissions);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn ordinary_update_aliases_keep_manifest_writing_and_unknown_shapes_observed() {
    let world = World::new();
    world.build();
    let before = std::fs::read(world.path()).unwrap();
    let gate = crate::metadata_io::open_regular(&gate_path(&world.path()).unwrap()).unwrap();
    let _held = crate::lock::Flock::try_lock(gate).unwrap();
    for (alias, free) in [
        ("update --offline", true),
        ("update --breaking", false),
        ("update --breaking=true", false),
        ("update --dry-run --breaking", false),
        ("update -b", false),
        ("update -qvb", false),
        ("update -bn", false),
        ("update -pb", true),
        ("update --config resolver.lockfile-path='Cargo.lock'", false),
        ("!echo update", false),
        ("unknown update", false),
        ("build update", false),
    ] {
        let env = |name: &str| match name {
            "CARGO_TARGET_DIR" => Some(world.target.as_os_str().to_owned()),
            "CARGO_ALIAS_REFRESH" => Some(alias.into()),
            _ => None,
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &env,
            read: &|_| Ok(None),
            read_manifest: &|_| Ok(None),
            writable: &|_| Some(false),
        };
        let args = vec!["refresh".into()];
        assert_eq!(
            non_producing_command(&args, super::super::Tool::Targo, &machine),
            free,
            "{alias}"
        );
        let observed = observe_for_producer(
            &args,
            super::super::Tool::Targo,
            &world.home,
            &machine,
            true,
        );
        assert_eq!(observed.is_err(), !free, "{alias}: {observed:?}");
        assert!(observed.is_err() || !observed.unwrap(), "{alias}");
        assert_eq!(std::fs::read(world.path()).unwrap(), before, "{alias}");
    }
}

/// The `*.json` receipts of `home`'s index, by name, with their bytes.
fn receipts(home: &Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
    let mut found = std::fs::read_dir(home.join(INDEX))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|path| {
            (
                path.file_name().unwrap().to_owned(),
                std::fs::read(&path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    found.sort();
    found
}

/// The producer call of `owner`'s workspace building into `target`.
fn build_into(owner: &Path, target: &Path) -> Vec<String> {
    vec![
        "--unverified".into(),
        "build".into(),
        format!("--manifest-path={}", owner.join("Cargo.toml").display()),
        format!("--target-dir={}", target.display()),
    ]
}

/// A TARGET NAMED THROUGH A LINK TO A FOLDER ATPKG OWNS IS THAT FOLDER: a build of another
/// workspace through a link (its text relative, as Macs set up before 2026-09-10 have
/// `crates/target -> target.noindex`, or absolute) is observed as owned, its workspace joins
/// the folder's own receipt, and nothing else changes: the marker, the link, the count of
/// registered roots, no receipt keyed by the link and none for a build leaf under it. A second
/// build reads its owner on record and writes nothing. An unresolved producer through the link
/// protects the folder's receipt as its own name would. Each fails without link resolution:
/// the link's name has no receipt of its own while the folder's marker shows through it.
#[test]
fn a_target_named_through_a_link_to_an_owned_folder_is_that_folder() {
    for text in [PathBuf::from("outputs.noindex"), PathBuf::new()] {
        let world = World::new();
        world.build();
        let text = if text.as_os_str().is_empty() {
            world.target.clone()
        } else {
            text
        };
        let link = world.home.join("target");
        std::os::unix::fs::symlink(&text, &link).unwrap();
        let marker = std::fs::read(world.target.join(MARKER)).unwrap();
        let other = world.another_owner();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&other),
            env: &|_| None,
            read: &|_| Ok(None),
            read_manifest: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let args = build_into(&other, &link);
        assert_eq!(
            observe(&args, super::super::Tool::Targo, &world.home, &machine).unwrap(),
            Observation::Owned,
            "{text:?}"
        );
        let receipt = read(&world.path()).unwrap();
        assert_eq!(receipt.root, world.target);
        assert_eq!(receipt.owners, [world.owner.clone(), other.clone()]);
        assert!(!receipt.owners_unknown);
        assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
        assert_eq!(std::fs::read_link(&link).unwrap(), text);
        assert!(super::super::target_build_path(&link).is_dir());
        assert!(!link.join(super::super::TARGET_BUILD_LEAF).exists());
        let registered = roots(&world.home);
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].root, world.target);
        let keyed_by_link = world
            .home
            .join(INDEX)
            .join(format!("{}.json", key(&link).unwrap()));
        assert!(std::fs::symlink_metadata(keyed_by_link).is_err());

        let recorded = receipts(&world.home);
        assert!(
            observe_for_producer(
                &args,
                super::super::Tool::Targo,
                &world.home,
                &machine,
                true
            )
            .unwrap()
        );
        assert_eq!(
            receipts(&world.home),
            recorded,
            "an owner on record writes nothing"
        );

        let unresolved = [
            "--unverified".into(),
            "unknown-alias".into(),
            format!("--target-dir={}", link.display()),
        ];
        assert_eq!(
            observe(
                &unresolved,
                super::super::Tool::Targo,
                &world.home,
                &machine
            )
            .unwrap(),
            Observation::Protected
        );
        assert!(read(&world.path()).unwrap().owners_unknown);
        assert_eq!(roots(&world.home).len(), 1);
    }
}

/// A LINK TO A FOLDER ATPKG DOES NOT OWN IS REFUSED, EACH CASE IN ITS OWN WORDS, and nothing is
/// written: no marker in the folder, no receipt in the index, the link as it was. A folder
/// with no custody; one cargo made that atpkg never registered (never adopted through the
/// link, though at its own name it would be); one cargo tagged that holds only the rustc facts
/// a resolving verb wrote, no profile a build prepared (no folder cargo built in, so as
/// unowned as any); a link to nothing; a link to a link to an owned
/// folder, or to an owned folder through a linked folder on the way, or in a folder reached
/// through a link; text that climbs past the root; a folder on another volume (`/dev`); a folder whose marker no receipt binds; a
/// file. NEGATIVE CONTROL: the cargo-made folder named by its own path is adopted.
#[test]
fn links_to_folders_atpkg_does_not_own_refuse_each_in_its_own_words() {
    use std::os::unix::fs::MetadataExt as _;
    let world = World::new();
    world.build();
    let other = world.another_owner();
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&other),
        env: &|_| None,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| Some(true),
    };
    let folder = |name: &str| {
        let at = world.home.join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&at).unwrap();
        at
    };
    let plain = folder("plain.noindex");
    let cargo_made = folder("cargo-made.noindex");
    let tag = cargo_made.join("CACHEDIR.TAG");
    std::fs::write(
        &tag,
        "Signature: 8a477f597d28d172789f06886806bc55\n\
         # This file is a cache directory tag created by cargo.\n\
         # For information about cache directory tags see https://bford.info/cachedir/\n",
    )
    .unwrap();
    std::fs::set_permissions(&tag, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(cargo_made.join(".rustc_info.json"), b"{}").unwrap();
    // A profile its build prepared, and a unit it invoked, beside the facts.
    for prepared in ["build", "incremental"] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(cargo_made.join("debug").join(prepared))
            .unwrap();
    }
    let fingerprint = cargo_made.join("debug/.fingerprint/probe-hash");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&fingerprint)
        .unwrap();
    let stamp = fingerprint.join("invoked.timestamp");
    std::fs::write(&stamp, b"invoked").unwrap();
    std::fs::set_permissions(&stamp, std::fs::Permissions::from_mode(0o600)).unwrap();
    // The facts a resolving verb wrote into a tagged folder no build prepared.
    let resolved = folder("resolved.noindex");
    std::fs::copy(&tag, resolved.join("CACHEDIR.TAG")).unwrap();
    std::fs::write(resolved.join(".rustc_info.json"), b"{}").unwrap();
    let foreign = folder("foreign.noindex");
    std::fs::copy(world.target.join(MARKER), foreign.join(MARKER)).unwrap();
    let file = world.home.join("a-file");
    std::fs::write(&file, b"not a folder").unwrap();
    let hop = world.home.join("hop");
    std::os::unix::fs::symlink("outputs.noindex", &hop).unwrap();
    std::os::unix::fs::symlink(".", world.home.join("via")).unwrap();
    assert_ne!(
        std::fs::metadata("/dev").unwrap().dev(),
        std::fs::metadata(&world.home).unwrap().dev(),
        "the other volume must be one"
    );
    let index = receipts(&world.home);
    for (name, text, refusal) in [
        (
            "unowned",
            PathBuf::from("plain.noindex"),
            LinkRefusal::Unowned,
        ),
        (
            "cargo",
            PathBuf::from("cargo-made.noindex"),
            LinkRefusal::CargoMade,
        ),
        (
            "resolved",
            PathBuf::from("resolved.noindex"),
            LinkRefusal::Unowned,
        ),
        (
            "dangling",
            PathBuf::from("nothing.noindex"),
            LinkRefusal::Dangling,
        ),
        ("chain", PathBuf::from("hop"), LinkRefusal::Chain),
        (
            "detour",
            PathBuf::from("via/outputs.noindex"),
            LinkRefusal::Chain,
        ),
        ("volume", PathBuf::from("/dev"), LinkRefusal::OtherVolume),
        (
            "foreign",
            PathBuf::from("foreign.noindex"),
            LinkRefusal::Foreign,
        ),
        ("file", PathBuf::from("a-file"), LinkRefusal::NotFolder),
        // Named as `via/link-parent`: a link to an owned folder, in a folder reached
        // through a link.
        (
            "parent",
            PathBuf::from("outputs.noindex"),
            LinkRefusal::Chain,
        ),
        (
            "above-root",
            PathBuf::from(format!("{}dev", "../".repeat(64))),
            LinkRefusal::Unreachable,
        ),
    ] {
        let link = world.home.join(format!("link-{name}"));
        std::os::unix::fs::symlink(&text, &link).unwrap();
        let named = if name == "parent" {
            world.home.join("via").join(format!("link-{name}"))
        } else {
            link.clone()
        };
        let args = build_into(&other, &named);
        assert_eq!(
            observe(&args, super::super::Tool::Targo, &world.home, &machine).unwrap(),
            Observation::Refused(refusal),
            "{name}"
        );
        let refused = observe_for_producer(
            &args,
            super::super::Tool::Targo,
            &world.home,
            &machine,
            true,
        )
        .unwrap_err();
        assert_ne!(refused.kind(), std::io::ErrorKind::WouldBlock, "{name}");
        assert_eq!(refused.to_string(), refusal.why(), "{name}");
        assert_eq!(receipts(&world.home), index, "{name}");
        assert_eq!(std::fs::read_link(&link).unwrap(), text, "{name}");
    }
    let messages = [
        LinkRefusal::Unowned,
        LinkRefusal::CargoMade,
        LinkRefusal::Dangling,
        LinkRefusal::Chain,
        LinkRefusal::OtherVolume,
        LinkRefusal::Foreign,
        LinkRefusal::Renumbered,
        LinkRefusal::NotFolder,
        LinkRefusal::Unreachable,
        LinkRefusal::Changed,
    ]
    .map(LinkRefusal::why);
    assert_eq!(
        messages
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        messages.len(),
        "each refusal has its own words"
    );
    for untouched in [&plain, &cargo_made, &resolved] {
        assert!(!untouched.join(MARKER).exists());
    }
    assert!(!world.home.join("nothing.noindex").exists());

    assert!(create_checked(&world.home, &cargo_made, &other).unwrap());
    assert!(cargo_made.join(MARKER).is_file());
}

/// A LINK RETARGETED BETWEEN ITS OBSERVATION AND THE RE-CHECK UNDER THE FOLDER'S GATE IS
/// REFUSED: a link to one owned folder, moved to another owned folder at the first re-check
/// (the shared gate's, for an owner already on record) or the second (the exclusive gate's,
/// for a new one), or with the link left as it was and another folder renamed into its
/// folder's path, is refused as changed, and neither folder's receipt gains the producer. An
/// unresolved call through the link, moved at its re-check under the folder's gate, is refused
/// as changed too, and the first folder's receipt is left as it was. NEGATIVE CONTROL: with no
/// move, the same calls are owned, and the unresolved one suppresses the folder's retirement.
#[test]
fn a_link_retargeted_between_observation_and_the_gate_is_refused() {
    for (moved_at, recorded, swapped) in [
        (Some(1), true, false),
        (Some(1), false, false),
        (Some(2), false, false),
        (Some(1), true, true),
        (Some(2), false, true),
        (None, false, false),
    ] {
        let world = World::new();
        world.build();
        let second = world.home.join("second.noindex");
        assert!(create_checked(&world.home, &second, &world.owner).unwrap());
        let producer = if recorded {
            world.owner.clone()
        } else {
            world.another_owner()
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&producer),
            env: &|_| None,
            read: &|_| Ok(None),
            read_manifest: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let link = world.home.join("target");
        std::os::unix::fs::symlink("outputs.noindex", &link).unwrap();
        let index = receipts(&world.home);
        let rechecks = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = rechecks.clone();
        let (first, aside) = (world.target.clone(), world.home.join("aside.noindex"));
        let swap = second.clone();
        link_seam::on_recheck(move |link| {
            seen.set(seen.get() + 1);
            if Some(seen.get()) != moved_at {
            } else if swapped {
                // The link as it was, its folder's path now another folder's.
                std::fs::rename(&first, &aside).unwrap();
                std::fs::rename(&swap, &first).unwrap();
            } else {
                std::fs::remove_file(link).unwrap();
                std::os::unix::fs::symlink("second.noindex", link).unwrap();
            }
        });
        let observed = observe(
            &build_into(&producer, &link),
            super::super::Tool::Targo,
            &world.home,
            &machine,
        );
        link_seam::clear();
        let label = format!("moved at {moved_at:?}, owner on record {recorded}, swapped {swapped}");
        if moved_at.is_some() {
            assert_eq!(
                observed.unwrap(),
                Observation::Refused(LinkRefusal::Changed),
                "{label}"
            );
            assert_eq!(receipts(&world.home), index, "{label}");
        } else {
            assert_eq!(observed.unwrap(), Observation::Owned, "{label}");
            assert!(rechecks.get() >= 1, "{label}");
            assert!(read(&world.path()).unwrap().owners.contains(&producer));
        }
    }
    for moved in [true, false] {
        let world = World::new();
        world.build();
        let second = world.home.join("second.noindex");
        assert!(create_checked(&world.home, &second, &world.owner).unwrap());
        let link = world.home.join("target");
        std::os::unix::fs::symlink("outputs.noindex", &link).unwrap();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &|_| Ok(None),
            read_manifest: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let index = receipts(&world.home);
        if moved {
            link_seam::on_recheck(|link| {
                std::fs::remove_file(link).unwrap();
                std::os::unix::fs::symlink("second.noindex", link).unwrap();
            });
        }
        let unresolved = [
            "--unverified".into(),
            "unknown-alias".into(),
            format!("--target-dir={}", link.display()),
        ];
        let observed = observe(
            &unresolved,
            super::super::Tool::Targo,
            &world.home,
            &machine,
        );
        link_seam::clear();
        if moved {
            assert_eq!(
                observed.unwrap_err().to_string(),
                LinkRefusal::Changed.why()
            );
            assert_eq!(receipts(&world.home), index);
        } else {
            assert_eq!(observed.unwrap(), Observation::Protected);
            assert!(read(&world.path()).unwrap().owners_unknown);
        }
    }
    // Shapes only one part of the re-check sees: the link replaced by a new link of the same
    // text (its own inode alone differs), and the link's folder moved deeper with a link left
    // in its place (the same link, text and folder, the kernel resolving it elsewhere).
    for shape in ["relinked", "parent moved"] {
        let world = World::new();
        world.build();
        let producer = world.another_owner();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&producer),
            env: &|_| None,
            read: &|_| Ok(None),
            read_manifest: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let links = world.home.join("links");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&links)
            .unwrap();
        let link = links.join("target");
        std::os::unix::fs::symlink("../outputs.noindex", &link).unwrap();
        let index = receipts(&world.home);
        let home = world.home.clone();
        let mut moved = false;
        link_seam::on_recheck(move |link| {
            if std::mem::replace(&mut moved, true) {
                return;
            }
            if shape == "relinked" {
                // A new link renamed over the old, so the replacement is a node of its own:
                // one made after the old is removed can take the freed inode number back
                // (Linux's ext4 reuses it at once), and no stat tells that from the original.
                let fresh = link.with_extension("fresh");
                std::os::unix::fs::symlink("../outputs.noindex", &fresh).unwrap();
                std::fs::rename(&fresh, link).unwrap();
            } else {
                let deeper = home.join("deeper");
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&deeper)
                    .unwrap();
                std::fs::rename(home.join("links"), deeper.join("links")).unwrap();
                std::os::unix::fs::symlink("deeper/links", home.join("links")).unwrap();
            }
        });
        let observed = observe(
            &build_into(&producer, &link),
            super::super::Tool::Targo,
            &world.home,
            &machine,
        );
        link_seam::clear();
        assert_eq!(
            observed.unwrap(),
            Observation::Refused(LinkRefusal::Changed),
            "{shape}"
        );
        assert_eq!(receipts(&world.home), index, "{shape}");
    }
}

#[cfg(target_os = "macos")]
const CLEAN_FRONT_HOME: &str = "ATPKG_TEST_CLEAN_FRONT_HOME";
#[cfg(target_os = "macos")]
const CLEAN_FRONT_TOOL: &str = "ATPKG_TEST_CLEAN_FRONT_TOOL";
#[cfg(target_os = "macos")]
const CLEAN_FRONT_WORDS: &str = "ATPKG_TEST_CLEAN_FRONT_WORDS";
#[cfg(target_os = "macos")]
const CLEAN_FRONT_PLACED: &str = "ATPKG_TEST_CLEAN_FRONT_PLACED";

/// This original test image enters the real shim front, which execs the requested
/// underlying tool. A refusal returns before that tool can print or remove anything.
#[cfg(target_os = "macos")]
#[test]
fn clean_front_child_if_asked() {
    if std::env::var_os(CLEAN_FRONT_HOME).is_none() {
        return;
    }
    let words: Vec<String> =
        aterm_json::from_str(&std::env::var(CLEAN_FRONT_WORDS).unwrap()).unwrap();
    let mut args: Vec<std::ffi::OsString> = vec![
        "--shim".into(),
        "targo".into(),
        std::env::var_os(CLEAN_FRONT_PLACED).unwrap_or_else(|| "-".into()),
        std::env::var_os(CLEAN_FRONT_TOOL).unwrap(),
    ];
    args.extend(words.into_iter().map(std::ffi::OsString::from));
    let code = crate::build_admit::run_hidden(&args);
    std::process::exit(if code == std::process::ExitCode::SUCCESS {
        0
    } else {
        75
    });
}

#[cfg(target_os = "macos")]
fn through_clean_front(world: &World, tool: &Path, words: &[String]) -> std::process::Output {
    through_clean_front_in(world, &world.owner, tool, words, None)
}

/// [`through_clean_front`] run in `cwd`, with `CARGO_TARGET_DIR` set to `target_env` when one
/// is given.
#[cfg(target_os = "macos")]
fn through_clean_front_in(
    world: &World,
    cwd: &Path,
    tool: &Path,
    words: &[String],
    target_env: Option<&Path>,
) -> std::process::Output {
    let environment = |name: &str| {
        (name == "CARGO_TARGET_DIR")
            .then(|| target_env.map(|target| target.as_os_str().to_owned()))
            .flatten()
    };
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(cwd),
        env: &environment,
        read: &super::super::read_config,
        read_manifest: &super::super::read_manifest,
        writable: &super::super::writable,
    };
    let placement = (!non_producing_command(words, super::super::Tool::Targo, &machine))
        .then(|| super::super::decide(words, super::super::Tool::Targo, &machine))
        .flatten();
    match placement {
        Some(super::super::BuildDir::Cache(value) | super::super::BuildDir::Target(value)) => {
            let handed = [vec!["--config".into(), value], words.to_vec()].concat();
            let env = super::super::loses_config(words, super::super::Tool::Targo)
                .then(|| super::super::directory_from_placed_args(&handed))
                .flatten();
            through_placed_front(world, cwd, tool, &handed, target_env, "config", env)
        }
        Some(super::super::BuildDir::TrustEnv(directory)) => {
            through_placed_front(world, cwd, tool, words, target_env, "env", Some(&directory))
        }
        None => through_placed_front(world, cwd, tool, words, target_env, "-", None),
    }
}

#[cfg(target_os = "macos")]
fn output_while_working(
    command: &mut aterm_uds::spawnfd::Command,
    what: &str,
) -> std::process::Output {
    let guard = std::time::Instant::now() + aterm_spec::wait::WORKING_GUARD;
    let mut guard_expired = false;
    let result = aterm_uds::deadline::output_from_while_process_progressing_unless(
        command,
        aterm_uds::spawnfd::Stdio::null(),
        aterm_spec::wait::MUST_HAPPEN,
        what,
        &mut || {
            guard_expired |= std::time::Instant::now() >= guard;
            guard_expired
        },
    );
    assert!(
        !guard_expired,
        "{what} did not finish before its {:?} working guard",
        aterm_spec::wait::WORKING_GUARD
    );
    result.unwrap_or_else(|error| panic!("{what}: {error:?}"))
}

#[cfg(target_os = "macos")]
fn through_placed_front(
    world: &World,
    cwd: &Path,
    tool: &Path,
    words: &[String],
    target_env: Option<&Path>,
    placed: &str,
    build_env: Option<&str>,
) -> std::process::Output {
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    // Terse progress leaves the exec'd tool's first output line unprefixed.
    command
        .args([
            "--exact",
            "build_cache::registered::tests::clean_front_child_if_asked",
            "--quiet",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(cwd)
        .env_clear()
        .env("HOME", &world.home)
        .env(CLEAN_FRONT_HOME, &world.home)
        .env(CLEAN_FRONT_TOOL, tool)
        .env(CLEAN_FRONT_WORDS, aterm_json::to_string(words).unwrap())
        .env(CLEAN_FRONT_PLACED, placed)
        .env(
            crate::build_admit::LEDGER_ENV,
            world.home.join("ledger.noindex"),
        );
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    if let Some(target) = target_env {
        command.env("CARGO_TARGET_DIR", target);
    }
    if let Some(directory) = build_env {
        command.env(super::super::BUILD_DIR_ENV, directory);
    }
    output_while_working(
        &mut command,
        "the registered cache front completes its tool",
    )
}

#[cfg(target_os = "macos")]
#[test]
fn quiet_front_checks_exact_claim_and_owned_descendants_before_starting_the_tool() {
    use std::os::unix::fs::MetadataExt as _;
    // The first call in a new home must choose the same plan before and after its own
    // registrar creates the shared parents. No earlier failed call warms this fixture.
    let cold = World::new();
    assert!(!cold.home.join("Library").exists());
    let words = vec!["--unverified".into(), "build".into(), "--jobs=1".into()];
    let out = through_clean_front(&cold, Path::new("/bin/echo"), &words);
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.ends_with(&words.join(" ")))
    );
    for path in [
        "Library/Caches",
        "Library/Caches/aterm",
        "Library/Caches/aterm/.ubd",
    ] {
        assert_eq!(
            std::fs::metadata(cold.home.join(path)).unwrap().mode() & 0o777,
            0o700
        );
    }
    assert!(!cold.owner.join("target").exists());
    for shape in [
        "safe",
        "stale",
        "mismatch",
        "unplaced",
        "unexpected-env",
        "parent-link",
        "parent-file",
        "parent-writable",
        "leaf-link",
        "leaf-writable",
    ] {
        let world = World::new();
        world.build();
        let quiet = world.target.join(".ubd");
        let build = super::super::target_build_path(&world.target);
        let outside = world.home.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"outside output").unwrap();
        let mut claimed = build.clone();
        match shape {
            "stale" => claimed = world.target.join("build.noindex"),
            "mismatch" => claimed = outside.join(".ubd/build.noindex"),
            "parent-link" => std::os::unix::fs::symlink(&outside, &quiet).unwrap(),
            "parent-file" => std::fs::write(&quiet, b"existing file").unwrap(),
            "parent-writable" => {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&quiet)
                    .unwrap();
                std::fs::set_permissions(&quiet, std::fs::Permissions::from_mode(0o777)).unwrap();
            }
            "leaf-link" | "leaf-writable" => {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&quiet)
                    .unwrap();
                if shape == "leaf-link" {
                    std::os::unix::fs::symlink(&outside, &build).unwrap();
                } else {
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&build)
                        .unwrap();
                    std::fs::set_permissions(&build, std::fs::Permissions::from_mode(0o777))
                        .unwrap();
                }
            }
            _ => {}
        }
        let receipt = std::fs::read(world.path()).unwrap();
        let words = [
            if shape == "unplaced" {
                vec![]
            } else {
                vec![
                    "--config".into(),
                    format!("build.build-dir='{}'", claimed.display()),
                ]
            },
            build_into(&world.owner, &world.target),
            vec!["--jobs=1".into(), "--package=compiler-started".into()],
        ]
        .concat();
        let out = through_placed_front(
            &world,
            &world.owner,
            Path::new("/bin/echo"),
            &words,
            None,
            if shape == "unplaced" { "-" } else { "config" },
            (shape == "unexpected-env").then_some("/unclaimed"),
        );
        let started = String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line == words.join(" "));
        assert_eq!(out.status.success(), shape == "safe", "{shape}: {out:?}");
        assert_eq!(started, shape == "safe", "{shape}: {out:?}");
        assert_eq!(std::fs::read(world.path()).unwrap(), receipt, "{shape}");
        assert_eq!(
            std::fs::read(outside.join("keep")).unwrap(),
            b"outside output"
        );
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            1,
            "{shape}: no outside mutation"
        );
        assert_eq!(
            std::fs::read(world.target.join("debug/deps/keep")).unwrap(),
            b"reusable output"
        );
        if shape == "safe" {
            assert!(build.is_dir());
        } else if matches!(shape, "stale" | "mismatch" | "unplaced" | "unexpected-env") {
            assert!(!quiet.exists(), "{shape}: reject before preparation");
        } else if shape == "parent-writable" {
            assert_eq!(std::fs::metadata(&quiet).unwrap().mode() & 0o777, 0o777);
            assert!(!build.exists());
        } else if shape == "leaf-writable" {
            assert_eq!(std::fs::metadata(&build).unwrap().mode() & 0o777, 0o777);
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn quiet_front_prepares_foreign_manifest_targets_without_claiming_custody() {
    use std::os::unix::fs::MetadataExt as _;
    for trust in [false, true] {
        for shape in [
            "safe",
            "outside-target",
            "parent-link",
            "parent-writable",
            "override",
        ] {
            let world = World::new();
            let foreign = world.home.join("Library/ForeignSource");
            std::fs::create_dir_all(&foreign).unwrap();
            std::fs::write(foreign.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
            let outside = world.home.parent().unwrap().join("retained-output");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("keep"), b"retained output").unwrap();
            let target = if shape == "outside-target" {
                world.home.parent().unwrap().join("foreign-target")
            } else {
                world.target.clone()
            };
            let quiet = target.join(".ubd");
            let build = super::super::target_build_path(&target);
            if matches!(shape, "parent-link" | "parent-writable") {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&target)
                    .unwrap();
                if shape == "parent-link" {
                    std::os::unix::fs::symlink(&outside, &quiet).unwrap();
                } else {
                    std::fs::create_dir(&quiet).unwrap();
                    std::fs::set_permissions(&quiet, std::fs::Permissions::from_mode(0o777))
                        .unwrap();
                }
            }
            let mut words = build_into(&foreign, &target);
            if trust {
                words[0] = "trust".into();
                words[1] = "check".into();
            }
            words.push("--jobs=1".into());
            if shape == "override" {
                words.push("--config=build.build-dir='caller-layout'".into());
            }
            let out = through_clean_front(&world, Path::new("/bin/echo"), &words);
            // Trust's separate explicit-config frontend cannot establish a marked root.
            let admitted =
                matches!(shape, "safe" | "outside-target") || (shape == "override" && !trust);
            let started = String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.ends_with(&words.join(" ")));
            assert_eq!(out.status.success(), admitted, "{trust} {shape}: {out:?}");
            assert_eq!(started, admitted, "{trust} {shape}: {out:?}");
            assert!(
                roots(&world.home).is_empty(),
                "no deletion authority acquired"
            );
            assert!(!target.join(MARKER).exists());
            assert!(!build.join(MARKER).exists());
            assert_eq!(
                std::fs::read(outside.join("keep")).unwrap(),
                b"retained output"
            );
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
            if matches!(shape, "safe" | "outside-target") {
                assert!(build.is_dir());
                assert_eq!(std::fs::metadata(&quiet).unwrap().mode() & 0o777, 0o700);
                assert_eq!(std::fs::metadata(&build).unwrap().mode() & 0o777, 0o700);
            } else {
                assert!(!build.exists());
                if shape == "override" {
                    assert!(!target.exists());
                } else if shape == "parent-writable" {
                    assert_eq!(std::fs::metadata(&quiet).unwrap().mode() & 0o777, 0o777);
                }
            }
        }
    }
}

#[test]
fn a_default_final_target_requires_agreement_even_with_foreign_intermediates() {
    for tool in [super::super::Tool::CargoCompat, super::super::Tool::Tippy] {
        let world = World::new();
        let member = world.owner.join("member");
        std::fs::create_dir(&member).unwrap();
        std::fs::write(
            world.owner.join("Cargo.toml"),
            "[workspace]\nmembers=['member']\nresolver='3'\n",
        )
        .unwrap();
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname='compat'\nversion='0.0.0'\nworkspace='..'\n",
        )
        .unwrap();
        std::fs::write(
            member.join("Targo.toml"),
            "[package]\nname='native'\nversion='0.0.0'\n[workspace]\n",
        )
        .unwrap();
        let intermediate = world
            .home
            .parent()
            .unwrap()
            .join("foreign-intermediate.noindex");
        let environment = |name: &str| {
            (name == super::super::BUILD_DIR_ENV).then(|| intermediate.as_os_str().to_owned())
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&member),
            env: &environment,
            read: &super::super::read_config,
            read_manifest: &super::super::read_manifest,
            writable: &|_| Some(true),
        };
        let words = if tool == super::super::Tool::Tippy {
            Vec::new()
        } else {
            vec!["build".into()]
        };
        let choice = selected_call_outputs(&words, tool, &world.home, &machine)
            .expect("the output graph is known even though manifest readings disagree");
        assert_eq!(choice.target, super::super::FinalTarget::WorkspaceDefault);
        assert!(
            observe_for_producer(&words, tool, &world.home, &machine, true).is_err(),
            "{tool:?}"
        );
        assert!(roots(&world.home).is_empty());
        assert!(!world.owner.join("target").exists());
        assert!(!member.join("target").exists());
        assert!(!intermediate.exists());
    }
}

#[test]
fn a_foreign_manifest_default_target_preserves_unrelated_managed_receipts() {
    let world = World::new();
    world.build();
    let foreign = world.home.parent().unwrap().join("foreign-source");
    std::fs::create_dir(&foreign).unwrap();
    let manifest = foreign.join("Cargo.toml");
    std::fs::write(
        &manifest,
        "[package]\nname='foreign'\nversion='0.0.0'\n[workspace]\n",
    )
    .unwrap();
    let logical_manifest = world.owner.join("Cargo.toml");
    std::fs::remove_file(&logical_manifest).unwrap();
    std::os::unix::fs::symlink(&manifest, &logical_manifest).unwrap();
    let intermediate = foreign.join("intermediate.noindex");
    let environment = |name: &str| {
        (name == super::super::BUILD_DIR_ENV).then(|| intermediate.as_os_str().to_owned())
    };
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &environment,
        read: &super::super::read_config,
        read_manifest: &super::super::read_manifest,
        writable: &|_| Some(true),
    };
    let words = vec!["--unverified".into(), "build".into()];
    // Targo's root is the folder the manifest is named in (`Workspace::root`, the found
    // manifest's parent, never resolved through its link), so this call builds into the
    // HOME package's `target`: it owes custody, and an owner read through a link out of HOME
    // cannot be told, so it is refused. The managed receipt stays recorded, its outputs kept.
    assert!(
        observe_for_producer(
            &words,
            super::super::Tool::Targo,
            &world.home,
            &machine,
            true,
        )
        .is_err()
    );
    assert_eq!(
        roots(&world.home)
            .into_iter()
            .map(|root| root.root)
            .collect::<Vec<_>>(),
        std::slice::from_ref(&world.target)
    );
    assert!(world.target.join("debug/deps/keep").is_file());
    assert!(!world.owner.join("target").exists());
    assert!(!foreign.join("target").exists());
    assert!(!intermediate.exists());
}

#[cfg(target_os = "macos")]
#[test]
fn a_default_final_target_cannot_escape_custody_through_a_foreign_intermediate() {
    let world = World::new();
    let intermediate = world.home.parent().unwrap().join("foreign-build.noindex");
    let words = vec!["--unverified".into(), "build".into()];
    let out = through_placed_front(
        &world,
        &world.owner,
        Path::new("/bin/echo"),
        &words,
        None,
        "-",
        Some(intermediate.to_str().unwrap()),
    );
    assert_eq!(out.status.code(), Some(75), "{out:?}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("--unverified build"));
    assert!(!world.owner.join("target").exists());
    assert!(!intermediate.exists());
    assert!(roots(&world.home).is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn a_default_final_target_refuses_existing_unowned_outputs() {
    let world = World::new();
    let target = world.owner.join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep"), b"caller output").unwrap();
    let intermediate = world.home.join("intermediate.noindex");
    let words = vec![
        "--unverified".into(),
        "build".into(),
        "--config".into(),
        format!("build.build-dir='{}'", intermediate.display()),
    ];
    let out = through_placed_front(
        &world,
        &world.owner,
        Path::new("/bin/echo"),
        &words,
        None,
        "-",
        None,
    );
    assert_eq!(out.status.code(), Some(75), "{out:?}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("--unverified build"));
    assert_eq!(
        std::fs::read(target.join("keep")).unwrap(),
        b"caller output"
    );
    assert!(!target.join(MARKER).exists());
    // Pair admission may keep a fresh empty owned first root, but never adopts the
    // existing final output or starts the producer before both outputs have custody.
    for receipt in roots(&world.home) {
        assert_eq!(receipt.root, intermediate);
        assert_eq!(receipt.owners, vec![world.owner.clone()]);
        assert!(!receipt.owners_unknown);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_intermediate_only_build_claims_the_workspace_default_target() {
    for (member, nested, explicit_manifest, effective_cwd) in [
        (false, false, false, false),
        (true, false, false, false),
        (true, true, false, false),
        (true, false, true, false),
        (true, false, false, true),
    ] {
        let world = World::new();
        let package = if member {
            std::fs::write(
                world.owner.join("Cargo.toml"),
                "[workspace]\nmembers=['member']\nresolver='3'\n",
            )
            .unwrap();
            let package = world.owner.join("member");
            std::fs::create_dir(&package).unwrap();
            std::fs::write(
                package.join("Cargo.toml"),
                "[package]\nname='member'\nversion='0.0.0'\nedition='2024'\n",
            )
            .unwrap();
            package
        } else {
            std::fs::write(
                world.owner.join("Cargo.toml"),
                "[package]\nname='standalone'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
            )
            .unwrap();
            world.owner.clone()
        };
        let target = world.owner.join("target");
        let intermediate = if nested {
            super::super::target_build_path(&target)
        } else {
            world.home.join("intermediate.noindex")
        };
        let launcher = world.home.join("launcher");
        std::fs::create_dir(&launcher).unwrap();
        let mut words = vec!["--unverified".into()];
        if effective_cwd {
            words.extend([
                "-Zunstable-options".into(),
                "-C".into(),
                package.display().to_string(),
            ]);
        }
        words.extend([
            "build".into(),
            "--config".into(),
            format!("build.build-dir='{}'", intermediate.display()),
        ]);
        if explicit_manifest {
            words.push(format!(
                "--manifest-path={}",
                package.join("Cargo.toml").display()
            ));
        }
        let out = through_placed_front(
            &world,
            if explicit_manifest || effective_cwd {
                &launcher
            } else {
                &package
            },
            Path::new("/bin/echo"),
            &words,
            None,
            "-",
            None,
        );
        assert!(out.status.success(), "member={member}: {out:?}");
        assert!(!String::from_utf8_lossy(&out.stdout).contains("--target-dir="));
        let registered = roots(&world.home);
        assert!(target.is_dir() && intermediate.is_dir());
        assert_eq!(
            registered.len(),
            if nested { 1 } else { 2 },
            "member={member}"
        );
        let expected_roots = if nested {
            vec![&target]
        } else {
            vec![&target, &intermediate]
        };
        for path in expected_roots {
            let receipt = registered
                .iter()
                .find(|receipt| receipt.root.as_path() == path.as_path())
                .unwrap();
            assert_eq!(receipt.owners, vec![world.owner.clone()]);
            assert!(!receipt.owners_unknown);
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_manifest_discovery_binds_both_outputs_to_the_actual_owner() {
    for shape in [
        "native-only",
        "native-preferred",
        "nearest-cargo",
        "explicit-cargo",
        "deep-native",
        "deep-cargo",
        "preferred-unreadable",
        "deep-unreadable-cargo",
        "cargo-directory",
        "preferred-directory",
        "preferred-directory-link",
    ] {
        let world = World::new();
        let member = if shape.starts_with("deep-") {
            let mut member = world.owner.clone();
            for _ in 0..40 {
                member.push("nested");
            }
            member
        } else {
            world.owner.join("member")
        };
        std::fs::create_dir_all(&member).unwrap();
        let native = member.join("Targo.toml");
        let cargo = member.join("Cargo.toml");
        let standalone = "[package]\nname='member'\nversion='0.0.0'\nedition='2024'\n[workspace]\n";
        let mut unreadable = None;
        let expected = match shape {
            "cargo-directory" => {
                std::fs::create_dir(&cargo).unwrap();
                None
            }
            "preferred-directory" | "preferred-directory-link" => {
                std::fs::write(&cargo, standalone).unwrap();
                if shape == "preferred-directory-link" {
                    let directory = world.home.join("manifest-directory");
                    std::fs::create_dir(&directory).unwrap();
                    std::os::unix::fs::symlink(directory, &native).unwrap();
                } else {
                    std::fs::create_dir(&native).unwrap();
                }
                None
            }
            "deep-cargo" => Some(&world.owner),
            "deep-unreadable-cargo" => {
                unreadable = Some(world.owner.join("Cargo.toml"));
                None
            }
            "nearest-cargo" => {
                std::fs::write(world.owner.join("Targo.toml"), "[workspace]\n").unwrap();
                std::fs::write(&cargo, standalone).unwrap();
                Some(&member)
            }
            _ => {
                std::fs::write(&native, standalone).unwrap();
                if matches!(shape, "native-preferred" | "explicit-cargo") {
                    std::fs::write(
                        world.owner.join("Cargo.toml"),
                        "[workspace]\nmembers=['member']\nresolver='3'\n",
                    )
                    .unwrap();
                    std::fs::write(
                        &cargo,
                        "[package]\nname='compat'\nversion='0.0.0'\nedition='2024'\nworkspace='..'\n",
                    )
                    .unwrap();
                }
                if shape == "preferred-unreadable" {
                    std::fs::write(&cargo, standalone).unwrap();
                    unreadable = Some(native.clone());
                    None
                } else if shape == "explicit-cargo" {
                    unreadable = Some(native.clone());
                    Some(&world.owner)
                } else {
                    Some(&member)
                }
            }
        };
        if let Some(path) = &unreadable {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let intermediate = world.home.join("intermediate.noindex");
        let mut words = vec![
            "--unverified".into(),
            "build".into(),
            format!("--target-dir={}", world.target.display()),
            "--config".into(),
            format!("build.build-dir='{}'", intermediate.display()),
        ];
        if shape == "explicit-cargo" {
            words.push(format!("--manifest-path={}", cargo.display()));
        }
        let out = through_placed_front(
            &world,
            &member,
            Path::new("/bin/echo"),
            &words,
            None,
            "-",
            None,
        );
        if let Some(path) = &unreadable {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        if let Some(owner) = expected {
            assert!(out.status.success(), "{shape}: {out:?}");
            let registered = roots(&world.home);
            assert_eq!(registered.len(), 2, "{shape}");
            for root in [&world.target, &intermediate] {
                let receipt = registered
                    .iter()
                    .find(|receipt| receipt.root.as_path() == root.as_path())
                    .unwrap();
                assert_eq!(receipt.owners, vec![owner.clone()], "{shape}");
                assert!(!receipt.owners_unknown, "{shape}");
            }
        } else {
            assert_eq!(out.status.code(), Some(75), "{shape}: {out:?}");
            assert!(!String::from_utf8_lossy(&out.stdout).contains("--target-dir="));
            assert!(!world.target.exists(), "{shape}");
            assert!(!intermediate.exists(), "{shape}");
            assert!(roots(&world.home).is_empty(), "{shape}");
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn implicit_manifest_errors_refuse_paired_output_custody_before_exec() {
    for shape in [
        "readable",
        "absent",
        "unreadable",
        "linked",
        "directory",
        "oversized",
        "invalid-utf8",
    ] {
        let world = World::new();
        std::fs::write(
            world.owner.join("Cargo.toml"),
            "[workspace]\nmembers=[]\nexclude=['member']\n",
        )
        .unwrap();
        let member = world.owner.join("member");
        std::fs::create_dir(&member).unwrap();
        let manifest = member.join("Cargo.toml");
        let text = "[package]\nname='member'\nversion='0.0.0'\n[workspace]\n";
        match shape {
            "absent" => (),
            "linked" => {
                let definition = world.home.join("definition/Cargo.toml");
                std::fs::create_dir(definition.parent().unwrap()).unwrap();
                std::fs::write(&definition, text).unwrap();
                std::os::unix::fs::symlink(definition, &manifest).unwrap();
            }
            "directory" => std::fs::create_dir(&manifest).unwrap(),
            "oversized" => std::fs::write(&manifest, vec![b' '; (1 << 20) + 1]).unwrap(),
            "invalid-utf8" => std::fs::write(&manifest, [0xff]).unwrap(),
            _ => std::fs::write(&manifest, text).unwrap(),
        }
        if shape == "unreadable" {
            std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let intermediate = world.home.join("intermediate.noindex");
        let words = vec![
            "--unverified".into(),
            "build".into(),
            format!("--target-dir={}", world.target.display()),
            "--config".into(),
            format!("build.build-dir='{}'", intermediate.display()),
        ];
        let out = through_placed_front(
            &world,
            &member,
            Path::new("/bin/echo"),
            &words,
            None,
            "-",
            None,
        );
        if shape == "unreadable" {
            std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        if matches!(shape, "readable" | "absent") {
            assert!(out.status.success(), "{shape}: {out:?}");
            let owner = if shape == "readable" {
                &member
            } else {
                &world.owner
            };
            let registered = roots(&world.home);
            assert_eq!(registered.len(), 2, "{shape}");
            for root in [&world.target, &intermediate] {
                let receipt = registered
                    .iter()
                    .find(|receipt| receipt.root.as_path() == root.as_path())
                    .unwrap();
                assert_eq!(receipt.owners, vec![owner.clone()], "{shape}");
                assert!(!receipt.owners_unknown, "{shape}");
            }
        } else {
            assert_eq!(out.status.code(), Some(75), "{shape}: {out:?}");
            assert!(
                !String::from_utf8_lossy(&out.stdout).contains("--target-dir="),
                "{shape}: {out:?}"
            );
            assert!(!world.target.exists(), "{shape}");
            assert!(!intermediate.exists(), "{shape}");
            assert!(roots(&world.home).is_empty(), "{shape}");
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_deep_native_manifest_never_confers_a_parent_workspaces_custody() {
    let world = World::new();
    let mut member = world.owner.clone();
    for _ in 0..40 {
        member.push("nested");
    }
    std::fs::create_dir_all(&member).unwrap();
    std::fs::write(
        member.join("Targo.toml"),
        "[package]\nname='native-member'\nversion='0.0.0'\n[workspace]\n",
    )
    .unwrap();
    let intermediate = world.home.join("intermediate.noindex");
    let words = vec![
        "--unverified".into(),
        "build".into(),
        format!("--target-dir={}", world.target.display()),
        "--config".into(),
        format!("build.build-dir='{}'", intermediate.display()),
    ];
    let out = through_placed_front(
        &world,
        &member,
        Path::new("/bin/echo"),
        &words,
        None,
        "-",
        None,
    );
    // Resolving native manifests may admit this member; an unresolved reading must
    // refuse before exec. Neither may attribute its outputs to the outer Cargo.toml.
    if out.status.success() {
        let registered = roots(&world.home);
        assert_eq!(registered.len(), 2, "{out:?}");
        for root in [&world.target, &intermediate] {
            let receipt = registered
                .iter()
                .find(|receipt| receipt.root.as_path() == root.as_path())
                .unwrap();
            assert_eq!(receipt.owners, vec![member.clone()]);
            assert!(!receipt.owners_unknown);
        }
    } else {
        assert_eq!(out.status.code(), Some(75), "{out:?}");
        assert!(!String::from_utf8_lossy(&out.stdout).contains("--target-dir="));
        assert!(!world.target.exists());
        assert!(!intermediate.exists());
        assert!(roots(&world.home).is_empty());
    }
}

/// A foreign workspace's `{workspace-root}` template (a manifest under `~/Library`, where atpkg
/// manages no workspace) is prepared under the root Targo finds for it, with no custody; one
/// whose root this reader cannot tell (a package its ancestor workspace does not hold as a
/// member, which Targo refuses) is refused before exec, nothing prepared.
#[cfg(target_os = "macos")]
#[test]
fn quiet_front_refuses_an_unresolved_foreign_workspace_template_before_exec() {
    use std::os::unix::fs::MetadataExt as _;
    let world = World::new();
    let foreign = world.home.join("Library/ForeignSource");
    std::fs::create_dir_all(foreign.join("sub")).unwrap();
    std::fs::write(foreign.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::write(
        foreign.join("sub/Cargo.toml"),
        "[package]\nname = \"sub\"\n",
    )
    .unwrap();
    let cache = super::super::cache_root(&world.home);
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o555)).unwrap();
    let words = |manifest: &Path| -> Vec<String> {
        vec![
            "--unverified".into(),
            "build".into(),
            format!("--manifest-path={}", manifest.display()),
            "--jobs=1".into(),
        ]
    };
    let unresolved = words(&foreign.join("sub/Cargo.toml"));
    let refused = through_clean_front(&world, Path::new("/bin/echo"), &unresolved);
    let resolved = words(&foreign.join("Cargo.toml"));
    let admitted = through_clean_front(&world, Path::new("/bin/echo"), &resolved);
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(refused.status.code(), Some(75), "{refused:?}");
    assert!(
        !String::from_utf8_lossy(&refused.stdout)
            .lines()
            .any(|line| line.ends_with(&unresolved.join(" ")))
    );
    assert!(!foreign.join("sub/target").exists());
    assert!(admitted.status.success(), "{admitted:?}");
    assert!(
        String::from_utf8_lossy(&admitted.stdout)
            .lines()
            .any(|line| line.ends_with(&resolved.join(" ")))
    );
    let quiet = super::super::target_build_path(&foreign.join("target"));
    assert_eq!(std::fs::metadata(&quiet).unwrap().mode() & 0o777, 0o700);
    assert!(roots(&world.home).is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn quiet_front_admits_equivalent_spellings_in_argv_and_environment_targets() {
    for relative in [false, true] {
        for in_env in [false, true] {
            for spelling in [
                "outputs.noindex/",
                "outputs.noindex///",
                "outputs.noindex//./",
                "./outputs.noindex",
            ] {
                let mut world = World::new();
                world.target = world.owner.join("outputs.noindex");
                world.build();
                let target = if relative {
                    spelling.to_owned()
                } else {
                    format!("{}/{spelling}", world.owner.display())
                };
                let directory = super::super::target_build_path(&world.target);
                let emitted_target = if relative {
                    format!("{}/{target}", world.owner.display())
                } else {
                    target.clone()
                };
                let mut words = vec![
                    "--config".into(),
                    format!("build.build-dir='{emitted_target}/.ubd/build.noindex'"),
                    "--unverified".into(),
                    "check".into(),
                    "--jobs=1".into(),
                ];
                if !in_env {
                    words.extend(["--target-dir".into(), target.clone()]);
                }
                let out = through_placed_front(
                    &world,
                    &world.owner,
                    Path::new("/bin/echo"),
                    &words,
                    in_env.then(|| Path::new(&target)),
                    "config",
                    None,
                );
                assert!(
                    out.status.success(),
                    "relative={relative} env={in_env}: {out:?}"
                );
                assert!(
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .any(|line| line == words.join(" ")),
                    "{out:?}"
                );
                assert!(directory.is_dir());
                assert_eq!(roots(&world.home).len(), 1);
            }
        }
    }
}

/// `verify provision`'s fish build through the real shim front, its words as the shim hands
/// them on with its quiet placement: the source and its
/// `--target-dir`, named again in `CARGO_TARGET_DIR`, in one scratch under
/// `~/Library/Caches`, run from that scratch. HOME's `Library` is a folder atpkg never manages
/// a workspace in, as it manages none outside HOME, so the front does not register its cache.
/// An explicit one-job request keeps the resource bound independent of the host's load.
#[cfg(target_os = "macos")]
#[test]
fn the_front_hands_on_a_build_of_a_workspace_atpkg_never_manages() {
    let world = World::new();
    let scratch = world
        .home
        .join("Library/Caches/aterm/tooling.noindex/.provision-fish-1_0_0");
    let src = scratch.join("src");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&src)
        .unwrap();
    std::fs::write(
        src.join("Cargo.toml"),
        "[package]\nname = 'fish'\nversion = '4.9.3'\n",
    )
    .unwrap();
    let target = scratch.join("target.noindex");
    let manifest = src.join("Cargo.toml").display().to_string();
    let target_dir = target.display().to_string();
    let words: Vec<String> = [
        "--unverified",
        "build",
        "--release",
        "--locked",
        "--no-default-features",
        "--jobs=1",
        "--manifest-path",
        manifest.as_str(),
        "--target-dir",
        target_dir.as_str(),
    ]
    .map(String::from)
    .into();
    // Echo is an in-place tool: its line proves the front handed it these words, not 75.
    let out = through_clean_front_in(
        &world,
        &scratch,
        Path::new("/bin/echo"),
        &words,
        Some(&target),
    );
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.ends_with(&words.join(" "))),
        "{out:?}"
    );
    assert!(super::super::target_build_path(&target).is_dir());
    assert!(
        !target.join(MARKER).exists(),
        "no managed cache was created for it"
    );
    assert!(
        !world
            .home
            .join(INDEX)
            .join(format!("{}.json", key(&target).unwrap()))
            .exists()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn clean_reaches_the_tool_without_adopting_unknown_or_changing_registered_output() {
    for registered in [false, true] {
        let world = World::new();
        if registered {
            world.build();
        } else {
            std::fs::create_dir(&world.target).unwrap();
        }
        let sentinel = world.target.join("keep");
        std::fs::write(&sentinel, b"clean itself owns removal").unwrap();
        let before = std::fs::read(world.path()).ok();
        let marker = std::fs::read(world.target.join(MARKER)).ok();
        let target = format!("--target-dir={}", world.target.display());
        for words in [
            vec!["clean".into(), target.clone()],
            vec![
                "--config".into(),
                "build.build-dir='custom-output'".into(),
                "clean".into(),
                target.clone(),
            ],
            vec!["metadata".into(), target.clone()],
            vec![
                "--unverified".into(),
                "miri".into(),
                "clean".into(),
                target.clone(),
            ],
        ] {
            // Echo is an in-place tool, not a copied image or a script. Its output
            // proves the actual front handed it these words and did not return 75.
            let out = through_clean_front(&world, Path::new("/bin/echo"), &words);
            assert!(out.status.success(), "{registered}: {out:?}");
            assert!(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .any(|line| line == words.join(" ")),
                "{registered}: words={words:?}, output={out:?}"
            );
            assert_eq!(std::fs::read(world.path()).ok(), before);
            assert_eq!(std::fs::read(world.target.join(MARKER)).ok(), marker);
            assert_eq!(
                std::fs::read(&sentinel).unwrap(),
                b"clean itself owns removal"
            );
        }
        let words = vec![
            "--config".into(),
            "build.build-dir='{unknown}/custom-output'".into(),
            "--unverified".into(),
            "check".into(),
            target.clone(),
        ];
        let out = through_clean_front(&world, Path::new("/bin/echo"), &words);
        assert_eq!(out.status.code(), Some(75), "{out:?}");
        assert!(
            !String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line == words.join(" "))
        );
        assert_eq!(std::fs::read(world.path()).ok(), before);
        assert_eq!(std::fs::read(world.target.join(MARKER)).ok(), marker);
    }
}

/// The real driver must update lock metadata even while its existing cache's
/// custody gate is held. Invalid Rust and a failing build script prove that
/// this operation cannot have obtained success by compiling the fixture.
#[cfg(target_os = "macos")]
#[test]
fn ordinary_update_reaches_the_real_driver_without_building_or_changing_cache_custody() {
    use std::os::unix::fs::MetadataExt as _;
    for shape in ["fresh", "unregistered", "registered"] {
        let world = World::new();
        std::fs::write(
            world.owner.join("Cargo.toml"),
            "[package]\nname='update-fixture'\nversion='0.0.0'\nedition='2024'\n\
             [workspace]\nmembers=['dep']\nresolver='3'\n\
             [dependencies]\nupdate-fixture-dependency={path='dep',version='*'}\n",
        )
        .unwrap();
        let dep = world.owner.join("dep");
        let root_manifest = std::fs::read(world.owner.join("Cargo.toml")).unwrap();
        std::fs::create_dir_all(dep.join("src")).unwrap();
        std::fs::create_dir(world.owner.join("src")).unwrap();
        std::fs::write(world.owner.join("src/lib.rs"), "this is invalid Rust\n").unwrap();
        std::fs::write(
            world.owner.join("build.rs"),
            "compile_error!(\"update must not compile\");\n",
        )
        .unwrap();
        std::fs::write(dep.join("src/lib.rs"), "this is also invalid Rust\n").unwrap();
        let manifest = |version: &str| {
            format!(
                "[package]\nname='update-fixture-dependency'\nversion='{version}'\nedition='2024'\n"
            )
        };
        std::fs::write(dep.join("Cargo.toml"), manifest("0.1.0")).unwrap();
        if shape == "registered" {
            world.build();
        } else if shape == "unregistered" {
            std::fs::create_dir(&world.target).unwrap();
            std::fs::write(world.target.join("keep"), b"unregistered output").unwrap();
        }
        let snapshot = |path: &Path| match std::fs::symlink_metadata(path) {
            Ok(m) => Some((
                std::fs::read(path).unwrap(),
                m.dev(),
                m.ino(),
                m.mode(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("snapshot {}: {error}", path.display()),
        };
        let receipt = snapshot(&world.path());
        let marker = snapshot(&world.target.join(MARKER));
        let cache_tag = snapshot(&world.target.join("CACHEDIR.TAG"));
        let reusable = snapshot(&world.target.join("debug/deps/keep"));
        let unregistered = snapshot(&world.target.join("keep"));
        let held = (shape == "registered").then(|| {
            let gate =
                crate::metadata_io::open_regular(&gate_path(&world.path()).unwrap()).unwrap();
            crate::lock::Flock::try_lock(gate).unwrap()
        });
        // Cargo supplies the actual Trust driver used to compile this image.
        let driver = Path::new(env!("CARGO"));
        let manifest_path = world.owner.join("Cargo.toml").display().to_string();
        let generate = vec![
            "generate-lockfile".into(),
            "--offline".into(),
            "--manifest-path".into(),
            manifest_path.clone(),
        ];
        let out =
            through_clean_front_in(&world, &world.owner, driver, &generate, Some(&world.target));
        assert!(
            out.status.success(),
            "{shape}: initial lock generation: {out:?}"
        );
        let lock = world.owner.join("Cargo.lock");
        let mut expected: aterm_toml::Value =
            aterm_toml::from_str(&std::fs::read_to_string(&lock).unwrap()).unwrap();
        let table = expected.as_table_mut().unwrap();
        let aterm_toml::Value::Array(mut packages) = table.remove("package").unwrap() else {
            panic!("the actual lock has a package array")
        };
        let mut selected = 0;
        for package in &mut packages {
            if package["name"].as_str() == Some("update-fixture-dependency") {
                assert_eq!(package["version"].as_str(), Some("0.1.0"));
                package
                    .as_table_mut()
                    .unwrap()
                    .insert("version".into(), aterm_toml::Value::String("0.2.0".into()));
                selected += 1;
            }
        }
        assert_eq!(selected, 1);
        table.insert("package".into(), aterm_toml::Value::Array(packages));
        std::fs::write(dep.join("Cargo.toml"), manifest("0.2.0")).unwrap();
        let update = vec![
            "update".into(),
            "--offline".into(),
            "--manifest-path".into(),
            manifest_path,
            "-p".into(),
            "update-fixture-dependency".into(),
        ];
        let out =
            through_clean_front_in(&world, &world.owner, driver, &update, Some(&world.target));
        assert!(out.status.success(), "{shape}: actual lock update: {out:?}");
        let actual: aterm_toml::Value =
            aterm_toml::from_str(&std::fs::read_to_string(&lock).unwrap()).unwrap();
        assert_eq!(
            actual, expected,
            "{shape}: only the dependency lock version changes"
        );
        assert_eq!(
            std::fs::read_to_string(dep.join("Cargo.toml")).unwrap(),
            manifest("0.2.0")
        );
        assert_eq!(
            std::fs::read(world.owner.join("Cargo.toml")).unwrap(),
            root_manifest,
            "{shape}: root manifest unchanged"
        );
        assert_eq!(snapshot(&world.path()), receipt, "{shape}: custody record");
        assert_eq!(
            snapshot(&world.target.join(MARKER)),
            marker,
            "{shape}: owner marker"
        );
        assert_eq!(
            snapshot(&world.target.join("CACHEDIR.TAG")),
            cache_tag,
            "{shape}: cache tag"
        );
        assert_eq!(
            snapshot(&world.target.join("debug/deps/keep")),
            reusable,
            "{shape}: reusable output"
        );
        assert_eq!(
            snapshot(&world.target.join("keep")),
            unregistered,
            "{shape}: unregistered output"
        );
        assert_eq!(roots(&world.home).len(), usize::from(shape == "registered"));
        if shape == "fresh" {
            assert!(!world.target.exists(), "no build directory created");
        }
        drop(held);
    }
}

/// A real compiler, run through the clean front with its two outputs configured by `source`,
/// writes the library under the target directory and its intermediates under the selected
/// build directory, and both roots are recorded with the owner as their custody.
#[cfg(target_os = "macos")]
fn a_real_compiler_writes_both_configured_outputs_under_recorded_custody(source: &str) {
    let world = World::new();
    std::fs::write(
        world.owner.join("Cargo.toml"),
        "[package]\nname='configured-output-probe'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
    )
    .unwrap();
    std::fs::create_dir(world.owner.join("src")).unwrap();
    std::fs::write(
        world.owner.join("src/lib.rs"),
        "pub fn value() -> usize { 42 }\n",
    )
    .unwrap();
    let intermediates = match source {
        "cli" | "toml11-cli" => world.home.join("intermediates.noindex"),
        "nested" => world.owner.join(".cargo/intermediates.noindex"),
        _ => world.owner.join("intermediates.noindex"),
    };
    let mut args = vec![
        "--unverified".into(),
        "build".into(),
        "--offline".into(),
        "-j1".into(),
    ];
    if matches!(source, "cli" | "toml11-cli") {
        args.extend([
            "--target-dir".into(),
            world.target.display().to_string(),
            "--config".into(),
            if source == "toml11-cli" {
                format!("build.\"build\\x2Ddir\"='{}'", intermediates.display())
            } else {
                format!("build.build-dir='{}'", intermediates.display())
            },
        ]);
    } else {
        let config = world.owner.join(".cargo");
        let leaf = if source == "nested" {
            "layers/layout.toml"
        } else {
            "layout.toml"
        };
        std::fs::create_dir_all(config.join(leaf).parent().unwrap()).unwrap();
        let root = if source == "legacy" {
            "config"
        } else {
            "config.toml"
        };
        std::fs::write(config.join(root), format!("include=['{leaf}']")).unwrap();
        std::fs::write(
                config.join(leaf),
                if source == "toml11-discovered" {
                    format!("build = {{\n target-dir = '{}',\n \"build\\x2Ddir\" = 'intermediates.noindex',\n}}\n", world.target.display())
                } else {
                    format!(
                        "build.target-dir='{}'\nbuild.build-dir='intermediates.noindex'",
                        world.target.display()
                    )
                },
            )
            .unwrap();
        if source == "legacy" {
            std::fs::write(
                config.join("config.toml"),
                "build.build-dir='ignored-build'",
            )
            .unwrap();
        }
    }
    let out = through_clean_front(&world, Path::new(env!("CARGO")), &args);
    assert!(
        out.status.success(),
        "real {source} configured build: {out:?}"
    );
    assert!(
        world
            .target
            .join("debug/libconfigured_output_probe.rlib")
            .is_file()
    );
    // The driver may use legacy deps/ or per-unit build directories. Inspect actual
    // compiler output without binding this custody test to either disposable layout.
    let mut pending = vec![intermediates.clone()];
    let mut visited = 0;
    let mut compiled = false;
    while let Some(dir) = pending.pop() {
        visited += 1;
        assert!(visited < 128, "the dependency-free fixture stays bounded");
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else if entry.file_name().to_str().is_some_and(|name| {
                name.starts_with("libconfigured_output_probe")
                    && (name.ends_with(".rmeta") || name.ends_with(".rlib"))
            }) {
                compiled = true;
            }
        }
    }
    assert!(
        compiled,
        "the compiler wrote intermediates in the selected root: {out:?}"
    );
    let recorded = roots(&world.home);
    assert_eq!(recorded.len(), 2);
    for output in [&world.target, &intermediates] {
        let entry = recorded.iter().find(|entry| entry.root == *output).unwrap();
        let receipt = super::read(&entry.receipt_path).unwrap();
        assert_eq!(
            receipt.owners.as_slice(),
            std::slice::from_ref(&world.owner)
        );
        assert!(!receipt.owners_unknown && same(&world.home, &receipt));
    }
}

// One test per configuration source, so each real build is judged on its own within the
// test runner's one-minute hang detector: all six in one test took 58.7 s alone and passed
// the detector on a loaded machine.
#[cfg(target_os = "macos")]
#[test]
fn a_real_compiler_writes_both_configured_outputs_from_the_command_line() {
    a_real_compiler_writes_both_configured_outputs_under_recorded_custody("cli");
}

#[cfg(target_os = "macos")]
#[test]
fn a_real_compiler_writes_both_configured_outputs_from_a_toml_1_1_command_line() {
    a_real_compiler_writes_both_configured_outputs_under_recorded_custody("toml11-cli");
}

#[cfg(target_os = "macos")]
#[test]
fn a_real_compiler_writes_both_configured_outputs_from_a_discovered_config() {
    a_real_compiler_writes_both_configured_outputs_under_recorded_custody("discovered");
}

#[cfg(target_os = "macos")]
#[test]
fn a_real_compiler_writes_both_configured_outputs_from_a_discovered_toml_1_1_config() {
    a_real_compiler_writes_both_configured_outputs_under_recorded_custody("toml11-discovered");
}

#[cfg(target_os = "macos")]
#[test]
fn a_real_compiler_writes_both_configured_outputs_from_a_nested_include() {
    a_real_compiler_writes_both_configured_outputs_under_recorded_custody("nested");
}

#[cfg(target_os = "macos")]
#[test]
fn a_real_compiler_writes_both_configured_outputs_from_a_legacy_config() {
    a_real_compiler_writes_both_configured_outputs_under_recorded_custody("legacy");
}

#[cfg(target_os = "macos")]
#[test]
fn a_real_clean_and_rebuild_publish_fresh_owned_output_over_only_a_missing_root() {
    use std::os::unix::fs::MetadataExt as _;
    let world = World::new();
    std::fs::write(
        world.owner.join("Cargo.toml"),
        "[package]\nname = 'cache-clean-probe'\nversion = '0.0.0'\nedition = '2024'\n[workspace]\n",
    )
    .unwrap();
    std::fs::create_dir(world.owner.join("src")).unwrap();
    std::fs::write(
        world.owner.join("src/lib.rs"),
        "pub const BUILT: usize = 1;\n",
    )
    .unwrap();
    let target = format!("--target-dir={}", world.target.display());
    let build = vec![
        "--unverified".into(),
        "check".into(),
        "--offline".into(),
        "--message-format=json".into(),
        "-j1".into(),
        target.clone(),
    ];
    // Cargo supplies the actual driver used to compile this image. Managed creation,
    // both clean calls and the final rebuild enter the production shim front.
    let driver = Path::new(env!("CARGO"));
    let out = through_clean_front(&world, driver, &build);
    assert!(out.status.success(), "initial managed compilation: {out:?}");
    let before = read(&world.path()).unwrap();
    let recorded = std::fs::read(world.path()).unwrap();
    let tag = std::fs::read(world.target.join("CACHEDIR.TAG")).unwrap();
    assert_eq!(
        tag,
        format!("{}\n", aterm_toolchain::lasting::CACHEDIR_TAG_SIGNATURE).as_bytes()
    );
    assert!(same(&world.home, &before));
    let out = through_clean_front(&world, driver, &["clean".into(), target.clone()]);
    assert!(out.status.success(), "registered cleanup: {out:?}");
    assert!(!world.target.exists(), "Cargo removed this private target");
    assert_eq!(std::fs::read(world.path()).unwrap(), recorded);

    // Model another producer by having Cargo itself recreate generated output and its
    // standard tag, outside managed admission. The historical receipt binds nothing in
    // this replacement. A private replacement is adopted where it stands; output the
    // inherited umask leaves group- or other-writable must stay unmanaged until clean.
    let mut foreign = aterm_uds::spawnfd::command(driver);
    foreign
        .args(&build)
        .current_dir(&world.owner)
        .env_clear()
        .env("HOME", &world.home);
    if let Some(path) = std::env::var_os("PATH") {
        foreign.env("PATH", path);
    }
    let out = output_while_working(
        &mut foreign,
        "the unregistered compiler writes replacement output",
    );
    assert!(
        out.status.success(),
        "unregistered compiler output: {out:?}"
    );
    let foreign_tag = std::fs::read(world.target.join("CACHEDIR.TAG")).unwrap();
    assert!(foreign_tag.starts_with(aterm_toolchain::lasting::CACHEDIR_TAG_SIGNATURE.as_bytes()));
    assert!(!world.target.join(MARKER).exists());
    assert_eq!(std::fs::read(world.path()).unwrap(), recorded);
    std::fs::write(world.target.join("keep"), b"unknown replacement").unwrap();
    let replacement = std::fs::symlink_metadata(&world.target).unwrap();
    let tag_mode = std::fs::symlink_metadata(world.target.join("CACHEDIR.TAG"))
        .unwrap()
        .mode();
    let shared = (replacement.mode() | tag_mode) & 0o022 != 0;
    let admitted = through_clean_front(&world, Path::new("/bin/echo"), &build);
    let adopted_nonce = if shared {
        assert_eq!(
            admitted.status.code(),
            Some(75),
            "shared output: {admitted:?}"
        );
        assert!(!world.target.join(MARKER).exists());
        assert_eq!(std::fs::read(world.path()).unwrap(), recorded);
        None
    } else {
        assert!(admitted.status.success(), "private output: {admitted:?}");
        let receipt = read(&world.path()).unwrap();
        assert_eq!(receipt.identity, identity(&world.target).unwrap());
        assert!(same(&world.home, &receipt));
        Some(receipt.nonce)
    };
    let still = std::fs::symlink_metadata(&world.target).unwrap();
    assert_eq!(
        (still.dev(), still.ino()),
        (replacement.dev(), replacement.ino())
    );
    assert_eq!(still.mode(), replacement.mode());
    assert_eq!(
        std::fs::symlink_metadata(world.target.join("CACHEDIR.TAG"))
            .unwrap()
            .mode(),
        tag_mode
    );
    assert_eq!(
        std::fs::read(world.target.join("CACHEDIR.TAG")).unwrap(),
        foreign_tag
    );
    assert_eq!(
        std::fs::read(world.target.join("keep")).unwrap(),
        b"unknown replacement"
    );
    let recorded = std::fs::read(world.path()).unwrap();
    let out = through_clean_front(&world, driver, &["clean".into(), target]);
    assert!(out.status.success(), "explicit cleanup: {out:?}");
    assert!(!world.target.exists());
    assert_eq!(std::fs::read(world.path()).unwrap(), recorded);

    let out = through_clean_front(&world, driver, &build);
    assert!(out.status.success(), "{out:?}");
    let after = read(&world.path()).unwrap();
    assert!(
        after.nonce != before.nonce
            && adopted_nonce
                .as_ref()
                .is_none_or(|nonce| after.nonce != *nonce),
        "a new creation, not a historical or adopted receipt"
    );
    assert_eq!(after.identity, identity(&world.target).unwrap());
    assert_eq!(after.owners, [world.owner]);
    assert!(!after.owners_unknown && after.creating.is_none() && after.pending.is_none());
    assert!(world.target.join(MARKER).is_file());
    assert_eq!(
        std::fs::read(world.target.join("CACHEDIR.TAG")).unwrap(),
        tag
    );
    assert!(same(&world.home, &after));
    let mut artifacts = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(message) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        if message.get("reason").and_then(aterm_json::Value::as_str) == Some("compiler-artifact") {
            for artifact in message
                .get("filenames")
                .and_then(aterm_json::Value::as_array)
                .expect("compiler artifact paths")
            {
                artifacts.push(PathBuf::from(artifact.as_str().expect("artifact path")));
            }
        }
    }
    assert!(
        !artifacts.is_empty(),
        "the actual compiler reported its output: {out:?}"
    );
    let target = world.target.canonicalize().unwrap();
    for artifact in artifacts {
        assert!(
            artifact.is_file() && artifact.canonicalize().unwrap().starts_with(&target),
            "actual compiler output in the managed target: {}",
            artifact.display()
        );
    }
}

/// KI-1111-15: WHAT A REAL `targo metadata` LEAVES IN A TARGET IT NAMES IS NO BUILD'S FOLDER.
/// In a folder that stands with cargo's own tag, as Targo tags a cargo home's cache where it
/// stands (`~/.cargo/git`, fetched, and one under another name), the real driver, which the
/// front runs as it runs every resolving verb, writes the rustc facts and prepares no profile;
/// a producer naming that folder next is refused (75), with nothing recorded and no marker
/// written, and what the cache keeps stands.
#[cfg(target_os = "macos")]
#[test]
fn a_real_metadata_into_a_tagged_folder_leaves_nothing_adoptable() {
    use std::os::unix::fs::OpenOptionsExt as _;
    for at in [".cargo/git", "tagged.noindex"] {
        let mut world = World::new();
        std::fs::write(
            world.owner.join("Cargo.toml"),
            "[package]\nname = 'metadata-probe'\nversion = '0.0.0'\nedition = '2024'\n[workspace]\n",
        )
        .unwrap();
        std::fs::create_dir(world.owner.join("src")).unwrap();
        std::fs::write(world.owner.join("src/lib.rs"), "").unwrap();
        world.target = world.home.join(at);
        std::fs::create_dir_all(&world.target).unwrap();
        std::io::Write::write_all(
            &mut std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .open(world.target.join("CACHEDIR.TAG"))
                .unwrap(),
            super::adoption::CARGO_TAG.as_bytes(),
        )
        .unwrap();
        let kept = world.target.join("db/dep-0123456789abcdef");
        if at == ".cargo/git" {
            std::fs::create_dir_all(&kept).unwrap();
            std::fs::write(kept.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        }
        let driver = Path::new(env!("CARGO"));
        let out = through_clean_front_in(
            &world,
            &world.owner,
            driver,
            &[
                "metadata".into(),
                "--offline".into(),
                "--format-version=1".into(),
            ],
            Some(&world.target),
        );
        assert!(
            out.status.success(),
            "{at}: the resolving verb ran: {out:?}"
        );
        assert!(
            world.target.join(".rustc_info.json").is_file(),
            "{at}: the real driver wrote the rustc facts"
        );
        let mut laid = std::fs::read_dir(&world.target)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        laid.sort();
        let mut expected = vec![".rustc_info.json", "CACHEDIR.TAG"];
        if at == ".cargo/git" {
            expected.push("db");
        }
        assert_eq!(laid, expected, "{at}: it prepared no profile");
        let build = vec![
            "--unverified".to_owned(),
            "check".to_owned(),
            "--offline".to_owned(),
            format!("--target-dir={}", world.target.display()),
        ];
        let refused = through_clean_front(&world, Path::new("/bin/echo"), &build);
        assert_eq!(
            refused.status.code(),
            Some(75),
            "{at}: refused: {refused:?}"
        );
        assert!(!world.path().exists(), "{at}: nothing recorded");
        assert!(
            !world.target.join(MARKER).exists(),
            "{at}: no custody declared"
        );
        if at == ".cargo/git" {
            assert_eq!(
                std::fs::read(kept.join("HEAD")).unwrap(),
                b"ref: refs/heads/main\n",
                "{at}: what the cache keeps stands"
            );
        }
    }
}

#[test]
fn configured_suppression_refuses_an_overfull_index_before_changing_receipts() {
    let world = World::new();
    world.build();
    let before = std::fs::read(world.path()).unwrap();
    let directory = world.path().parent().unwrap().to_path_buf();
    for n in 0..MAX_ROOTS {
        std::fs::write(directory.join(format!("extra-{n}")), b"").unwrap();
    }
    assert!(suppress_configured(&world.home).is_err());
    assert_eq!(std::fs::read(world.path()).unwrap(), before);
}

#[test]
fn repeated_unknown_owners_preserve_the_published_receipt() {
    use std::os::unix::fs::MetadataExt as _;

    for configured in [false, true] {
        let world = World::new();
        world.build();
        let suppress = || {
            if configured {
                suppress_configured(&world.home)
            } else {
                suppress_unresolved(&world.home, &world.target).map(|protected| {
                    assert!(protected);
                })
            }
        };
        assert!(!read(&world.path()).unwrap().owners_unknown);
        suppress().unwrap();
        assert!(read(&world.path()).unwrap().owners_unknown);
        // Keep the inode open: replacement cannot accidentally reuse its number.
        let published = std::fs::File::open(world.path()).unwrap();
        let before = published.metadata().unwrap();
        let bytes = std::fs::read(world.path()).unwrap();
        suppress().unwrap();
        let after = std::fs::metadata(world.path()).unwrap();
        assert_eq!(std::fs::read(world.path()).unwrap(), bytes);
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());

        let gate = gate::open(&gate_path(&world.path()).unwrap()).unwrap();
        let _held = gate::try_exclusive(gate).unwrap();
        assert!(
            suppress().is_err(),
            "known suppression still honors the root gate"
        );
    }
}

#[test]
fn unknown_invocations_suppress_retirement_but_never_claim_fresh_targets() {
    let world = World::new();
    world.build();
    let env =
        |name: &str| (name == "CARGO_TARGET_DIR").then(|| world.target.as_os_str().to_owned());
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &env,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| Some(true),
    };
    let args = ["--unverified".into(), "an-unknown-alias".into()];
    observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
    assert!(read(&world.path()).unwrap().owners_unknown);
    let unknown = std::fs::read(world.path()).unwrap();
    assert!(
        !observe_on(
            &["metadata".into()],
            super::super::Tool::Targo,
            &world.home,
            &machine,
        )
        .unwrap()
    );
    assert_eq!(std::fs::read(world.path()).unwrap(), unknown);
    let fresh = world.home.join("unknown-fresh.noindex");
    let env = |name: &str| (name == "CARGO_TARGET_DIR").then(|| fresh.as_os_str().to_owned());
    let machine = super::super::Machine {
        env: &env,
        ..machine
    };
    observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
    assert!(!fresh.exists());
}

#[test]
fn configured_targets_preserve_an_unobserved_shared_owner() {
    let world = World::new();
    world.build();
    let owner = world.another_owner();
    let config = owner.join(".cargo/config.toml");
    let read_config = |path: &Path| {
        Ok((path == config)
            .then(|| format!("[build]\ntarget-dir = '{}'\n", world.target.display())))
    };
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&owner),
        env: &|_| None,
        read: &read_config,
        read_manifest: &read_config,
        writable: &|_| Some(true),
    };
    observe_on(
        &["--unverified".into(), "build".into()],
        super::super::Tool::Targo,
        &world.home,
        &machine,
    )
    .unwrap();
    std::fs::remove_dir_all(&world.owner).unwrap();
    assert!(read(&world.path()).unwrap().owners_unknown);
    assert!(
        reclaim(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true
        )
        .is_empty()
    );
    assert!(world.target.join("debug/deps/keep").exists());
    let args = [
        "--unverified".into(),
        "build".into(),
        "--config".into(),
        format!("build.target-dir='{}'", world.target.display()),
    ];
    observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
    assert!(!owner.join("target").exists());
    let mut receipt = read(&world.path()).unwrap();
    receipt.owners_unknown = false;
    write(&world.path(), &receipt).unwrap();
    let args = [
        "-C".into(),
        "another-launcher".into(),
        "test".into(),
        "--target-dir".into(),
        "../outputs.noindex".into(),
    ];
    observe_on(&args, super::super::Tool::Cargo, &world.home, &machine).unwrap();
    assert!(read(&world.path()).unwrap().owners_unknown);
}

#[test]
fn interrupted_staging_never_authorizes_an_unrelated_payload() {
    let world = World::new();
    world.build();
    // Two interrupted attempts can belong to the same root generation. Each stage must
    // retain its own custody without making the next producer permanently unavailable.
    for name in ["interrupted-a.noindex", "interrupted-b.noindex"] {
        let mut receipt = read(&world.path()).unwrap();
        let stage = world.home.join(name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&stage)
            .unwrap();
        let payload = stage.join("cache.noindex");
        std::fs::create_dir(&payload).unwrap();
        std::fs::write(payload.join("unowned"), b"preserve").unwrap();
        receipt.pending = Some(Pending {
            directory: stage.clone(),
            identity: identity(&stage).unwrap(),
            authorized: false,
        });
        write(&world.path(), &receipt).unwrap();
        assert!(
            reclaim(
                &world.home,
                world.later(),
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| true
            )
            .is_empty()
        );
        assert_eq!(std::fs::read(payload.join("unowned")).unwrap(), b"preserve");
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        assert!(read(&world.path()).unwrap().pending.is_none());
        assert!(payload.exists());
    }
    assert_eq!(roots(&world.home).len(), 3);
    assert!(world.target.join("debug/deps/keep").exists());
}

/// THE HOP HANDS ITS FRONT THE CALL, AND NO BUILD STARTS WITHOUT IT: the front gets
/// `__build_admit --shim targo <placed> <program>` and the words, and the shim reaches no line
/// after it (the front became the build); with no helper there the shim refuses (exit 75,
/// said) and starts nothing. NEGATIVE CONTROL: the same lines with the helper's `exec` taken
/// off run on past the helper into the refusal, so the front's end is not the shim's.
#[test]
fn the_hop_hands_its_front_the_call_and_nothing_runs_without_it() {
    let world = World::new();
    let helper = world.home.join("atpkg");
    let shim = world.home.join("targo");
    let log = world.home.join("ran");
    std::fs::write(
        &helper,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$ATPKG_TEST_CACHE_LOG\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let hop = shim_hop(&helper, "targo", "'/store/targo'", None);
    let run = |hop: &str| {
        let _ = std::fs::remove_file(&log);
        std::fs::write(
            &shim,
            format!("#!/bin/sh\n__atb=\n{hop}printf 'compiler\\n' >> \"$ATPKG_TEST_CACHE_LOG\"\n"),
        )
        .unwrap();
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command
            .arg(&shim)
            .args(["--unverified", "check"])
            .env_clear()
            .env("ATPKG_TEST_CACHE_LOG", &log);
        let result =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        (result, std::fs::read_to_string(&log).unwrap_or_default())
    };
    let (result, ran) = run(&hop);
    assert!(result.status.success(), "{result:?}");
    assert_eq!(
        ran,
        "__build_admit\n--shim\ntargo\n-\n/store/targo\n--unverified\ncheck\n"
    );
    let (result, ran) = run(&hop.replacen(" && exec ", " && ", 1));
    assert_eq!(result.status.code(), Some(75), "the control runs on: {ran}");
    assert!(ran.starts_with("__build_admit\n"), "{ran}");
    std::fs::remove_file(&helper).unwrap();
    let (result, ran) = run(&hop);
    assert_eq!(result.status.code(), Some(75));
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("the cache registrar is unavailable"),
        "{result:?}"
    );
    assert_eq!(ran, "", "no build started");
}

/// The front reads the call as typed: a `config` placement's two leading words and derived
/// environment come off, an `env` placement hides its variable, and `-` changes nothing.
/// A `config` placement whose leading words are not a build directory is malformed, as is
/// a placement the block never says.
#[test]
fn the_front_reads_the_call_as_typed() {
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    let placed = words(&[
        "--config",
        "build.build-dir='/c/ws/.cargo-build'",
        "test",
        "-q",
    ]);
    assert_eq!(
        typed_call(&placed, "config"),
        Some((&placed[2..], Some(super::super::BUILD_DIR_ENV)))
    );
    assert_eq!(
        typed_call(&placed[2..], "env"),
        Some((&placed[2..], Some(super::super::BUILD_DIR_ENV)))
    );
    assert_eq!(typed_call(&placed, "-"), Some((&placed[..], None)));
    let own = words(&["--config", "net.offline=true", "test"]);
    assert_eq!(typed_call(&own, "config"), None);
    assert_eq!(typed_call(&own, "both"), None);
}

fn retain_cold_inputs_on_failure(test: impl FnOnce(&World)) {
    let world = World::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| test(&world)));
    if let Err(failure) = result {
        eprintln!(
            "cold placement failure inputs: {}",
            world._directory.keep().display()
        );
        std::panic::resume_unwind(failure);
    }
}

#[test]
fn cold_home_keeps_the_selected_cache_and_rechecks_configuration() {
    for shim in [false, true] {
        for changed_config in [false, true] {
            retain_cold_inputs_on_failure(|world| {
                let args = vec!["--unverified".into(), "build".into()];
                let config = world.owner.join(".cargo/config.toml");
                let inserted = std::cell::Cell::new(false);
                let read = |path: &Path| {
                    if changed_config
                        && world.home.join("Library/Caches/aterm/.ubd").is_dir()
                        && !inserted.replace(true)
                    {
                        std::fs::create_dir(world.owner.join(".cargo")).unwrap();
                        std::fs::write(&config, "[build]\nbuild-dir = 'changed-output'\n").unwrap();
                    }
                    super::super::read_config(path)
                };
                let machine = super::super::Machine {
                    home: Some(&world.home),
                    root: None,
                    cwd: Some(&world.owner),
                    env: &|_| None,
                    read: &read,
                    read_manifest: &super::super::read_manifest,
                    writable: &super::super::writable,
                };
                assert!(!world.home.join("Library").exists());
                let (selected, placed) =
                    super::super::placed_build_dir(&args, super::super::Tool::Targo, &machine)
                        .unwrap();
                // Current placement walks actual existing writable ancestors;
                // a cold private HOME can already support this shared cache.
                assert!(matches!(selected, super::super::BuildDir::Cache(_)));
                let result = if shim {
                    observe_shim_on(
                        &placed,
                        super::super::Tool::Targo,
                        "config",
                        &world.home,
                        &machine,
                        true,
                    )
                    .map(|_| ())
                } else {
                    observe_placed_command_with(&args, &placed, super::super::Tool::Targo, &machine)
                };
                if !shim && !cfg!(target_os = "macos") {
                    // A native reroute's admission claims nothing outside macOS: no cache laid,
                    // so no configuration read again. The shim's front (only macOS's shims call
                    // it) is the same code on every platform.
                    result.unwrap();
                    assert!(!world.home.join("Library").exists());
                    assert!(!inserted.get());
                    return;
                }
                assert!(world.home.join("Library/Caches/aterm/.ubd").is_dir());
                if changed_config {
                    assert!(inserted.get());
                    assert!(
                        result.is_err(),
                        "a new configured build directory must invalidate placement; no build started"
                    );
                } else {
                    result.unwrap();
                    assert!(
                        !world.owner.join("target").exists(),
                        "cold cache placement must not become a local fallback"
                    );
                    assert_eq!(
                        super::super::decide(&args, super::super::Tool::Targo, &machine),
                        Some(selected)
                    );
                }
            });
        }
    }
}

#[test]
fn cold_trust_shim_keeps_its_local_placement_and_rechecks_configured_targets() {
    for changed_config in [false, true] {
        retain_cold_inputs_on_failure(|world| {
            let args = vec!["trust".into(), "check".into()];
            let config = world.owner.join(".cargo/config.toml");
            let inserted = std::cell::Cell::new(false);
            let read = |path: &Path| {
                if changed_config
                    && world.home.join("Library/Caches/aterm/.ubd").is_dir()
                    && !inserted.replace(true)
                {
                    std::fs::create_dir(world.owner.join(".cargo")).unwrap();
                    std::fs::write(&config, "[build]\ntarget-dir = 'changed-output'\n").unwrap();
                }
                super::super::read_config(path)
            };
            let original = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&world.owner),
                env: &|_| None,
                read: &read,
                read_manifest: &super::super::read_manifest,
                writable: &super::super::writable,
            };
            assert!(!world.home.join("Library").exists());
            let selected =
                super::super::decide(&args, super::super::Tool::Targo, &original).unwrap();
            let super::super::BuildDir::TrustEnv(directory) = &selected else {
                panic!("the actual trust lane must select its local environment placement");
            };
            let environment = |name: &str| {
                (name == super::super::BUILD_DIR_ENV).then(|| std::ffi::OsString::from(directory))
            };
            let machine = super::super::Machine {
                env: &environment,
                ..original
            };
            let result = observe_shim_on(
                &args,
                super::super::Tool::Targo,
                "env",
                &world.home,
                &machine,
                true,
            );
            assert!(world.home.join("Library/Caches/aterm/.ubd").is_dir());
            if changed_config {
                assert!(inserted.get());
                assert!(
                    result.is_err(),
                    "a changed target configuration must refuse before any build starts"
                );
            } else {
                result.unwrap();
                let records = roots(&world.home);
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].root, world.owner.join("target"));
                assert!(super::super::target_build_path(&records[0].root).is_dir());
                assert_eq!(
                    super::super::decide(&args, super::super::Tool::Targo, &original),
                    Some(selected)
                );
            }
        });
    }
}

/// The workspace fallback a call was placed in is the directory admission judges, though
/// admission itself makes the shared cache's parents in a home that had no `Library/Caches`.
/// The probe answers only for folders standing under `Library/Caches` (one that cannot read the
/// home itself): before admission it answers nothing, so the cache is not writable and the call
/// takes `{workspace-root}/target`; after `lay_shared` it answers the cache writable, and a fresh
/// decision would name the cache instead. The negative control keeps the guard against a plan
/// that really changed: a config that names a target during admission, which no admission
/// writes, is refused by the final check, which a held writability answer does not blind.
#[test]
fn a_cold_home_fallback_is_judged_by_the_directory_it_was_placed_in() {
    for changed in [false, true] {
        let world = World::new();
        let caches = world.home.join("Library/Caches");
        assert!(!world.home.join("Library").exists());
        let writable = |path: &Path| (path.starts_with(&caches) && path.is_dir()).then_some(true);
        let config = world.owner.join(".cargo/config.toml");
        let read = |path: &Path| {
            // Change during admission, after its own cache-parent creation.
            // Before that point the exact initially selected plan still holds.
            Ok((changed && path == config && caches.is_dir()).then(|| {
                format!(
                    "[build]\ntarget-dir = '{}'\n",
                    world.home.join("configured.noindex").display()
                )
            }))
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &|_| Ok(None),
            read_manifest: &super::super::read_manifest,
            writable: &writable,
        };
        let args: Vec<String> = vec!["--unverified".into(), "build".into()];
        let tool = super::super::Tool::Targo;
        let (selected, _) = super::super::placed_build_dir(&args, tool, &machine).unwrap();
        let fallback = super::super::target_build_path(Path::new("{workspace-root}/target"));
        assert_eq!(
            selected,
            super::super::BuildDir::Target(format!("build.build-dir='{}'", fallback.display()))
        );
        let during = super::super::Machine {
            read: &read,
            ..machine
        };
        let result = observe_selected_placement(&args, tool, &world.home, &during, &selected, true);
        assert!(
            world.home.join(INDEX).parent().unwrap().is_dir(),
            "{changed}: admission laid the shared cache's parents"
        );
        assert!(matches!(
            super::super::decide(&args, tool, &machine),
            Some(super::super::BuildDir::Cache(_))
        ));
        if changed {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{error}");
            assert!(
                error.to_string().contains("changed during admission"),
                "{error}"
            );
            assert!(
                !super::super::target_build_path(&world.owner.join("target")).exists(),
                "a refused admission wrote no build directory"
            );
        } else {
            result.unwrap();
            assert!(super::super::target_build_path(&world.owner.join("target")).is_dir());
        }
    }
}

#[test]
fn quiet_claim_recomputation_requires_the_matching_environment_and_resolvable_machine() {
    for tool in [super::super::Tool::Targo, super::super::Tool::Tippy] {
        for lane in ["trust", "tippy"] {
            if tool == super::super::Tool::Tippy && lane == "trust" {
                continue;
            }
            for shape in ["safe", "stale", "missing", "no-home", "no-cwd"] {
                let world = World::new();
                world.build();
                let mut typed = build_into(&world.owner, &world.target);
                typed[0] = if lane == "trust" {
                    "trust"
                } else {
                    "--unverified"
                }
                .into();
                typed[1] = if lane == "trust" { "check" } else { "tippy" }.into();
                let directory = super::super::target_build_path(&world.target);
                let original = super::super::Machine {
                    home: Some(&world.home),
                    root: None,
                    cwd: Some(&world.owner),
                    env: &|_| None,
                    read: &super::super::read_config,
                    read_manifest: &super::super::read_manifest,
                    writable: &|_| Some(true),
                };
                let (placed, handed) = if lane == "trust" {
                    ("env", typed)
                } else {
                    (
                        "config",
                        [
                            vec![
                                "--config".into(),
                                format!("build.build-dir='{}'", directory.display()),
                            ],
                            typed,
                        ]
                        .concat(),
                    )
                };
                let claimed = if shape == "stale" {
                    world.target.join("build.noindex")
                } else {
                    directory.clone()
                };
                let environment = |name: &str| {
                    (shape != "missing" && name == super::super::BUILD_DIR_ENV)
                        .then(|| claimed.clone().into_os_string())
                };
                let machine = super::super::Machine {
                    home: if shape == "no-home" {
                        None
                    } else {
                        original.home
                    },
                    cwd: if shape == "no-cwd" {
                        None
                    } else {
                        original.cwd
                    },
                    env: &environment,
                    ..original
                };
                let result = observe_shim_on(&handed, tool, placed, &world.home, &machine, true);
                if shape == "safe" {
                    assert!(result.unwrap(), "{tool:?} {lane}");
                    assert!(directory.is_dir());
                } else {
                    assert_eq!(
                        result.unwrap_err().kind(),
                        std::io::ErrorKind::InvalidInput,
                        "{tool:?} {lane} {shape}"
                    );
                    assert!(!world.target.join(".ubd").exists());
                }
            }
        }
    }
}

/// A `-C` BUILD'S INTERMEDIATES ARE OWNED UNDER ITS TARGET, out of Spotlight: the shim's own
/// block, run from a launcher folder on a call whose `-C` names the package (relative and
/// absolute, behind Targo's gate), places it as typed there, and the front admits the
/// placement, recording the target and making `<target>/.ubd/build.noindex` under its
/// record: an absolute target, a relative one (the package's folder's, as Targo resolves it
/// there), and none, the local fallback under the root Targo finds from that folder, which
/// the front reads there whatever `$PWD` the launcher left it. From HOME itself too, and
/// with the package named through the data volume's firmlink. The world's cache folder is one
/// its person cannot write, where the local fallback is the placement. NEGATIVE CONTROL:
/// without the gate the block leaves the call as typed, and the front, handed it so, makes
/// nothing.
#[cfg(target_os = "macos")]
#[test]
fn a_directory_builds_intermediates_owned_under_its_target() {
    use super::super::Tool;
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
    // What the shim's block hands its front for `args` typed at `cwd`.
    let block = |world: &World, cwd: &Path, args: &[String]| -> Vec<String> {
        let out = aterm_uds::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg(format!(
                "{}[ $# -eq 0 ] || printf '%s\\n' \"$@\"",
                super::super::shim_block("targo").unwrap()
            ))
            .arg("sh")
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &world.home)
            .env("PWD", cwd)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(out.status.success() && out.stderr.is_empty(), "{out:?}");
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    };
    // The front as the shim starts it: in the launcher's folder, with its `$PWD`.
    let front = |world: &World, cwd: &Path, placed: &[String], claim: &str| {
        let pwd = cwd.as_os_str().to_owned();
        let env = |name: &str| (name == "PWD").then(|| pwd.clone());
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(cwd),
            env: &env,
            read: &super::super::read_config,
            read_manifest: &super::super::read_config,
            writable: &|_| None,
        };
        observe_shim_on(placed, Tool::Targo, claim, &world.home, &machine, true)
    };
    // A world whose shared cache folder its person cannot write, so a call that names no
    // target takes the local fallback: with none there the block writes the shared cache under
    // the nearest writable ancestor of it (`cache_writable`), as the front does. The folders
    // above it are laid private, as the index beside it needs them.
    let unwritable_cache = || {
        let world = World::new();
        let cache = world.home.join(super::super::CACHE_UNDER_HOME);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(cache.parent().unwrap())
            .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o555)
            .create(&cache)
            .unwrap();
        world
    };
    let owner = |world: &World, root: &Path| -> Vec<PathBuf> {
        roots(&world.home)
            .into_iter()
            .filter(|registered| registered.root == root)
            .flat_map(|registered| registered.owners)
            .collect()
    };
    // From a launcher folder and from HOME itself, the package named relative to it, by its
    // own spelling and by the data volume's firmlink spelling, which Targo reads back as its
    // own (`getcwd`) and `realpath` does not.
    for (from, directory) in [
        ("launch", "../source"),
        ("launch", "source-abs"),
        ("launch", "source-data"),
        ("", "source"),
        ("", "source-data"),
    ] {
        for target in ["absolute", "out", "none"] {
            let world = unwritable_cache();
            let launcher = world.home.join(from);
            std::fs::create_dir_all(&launcher).unwrap();
            let named = match directory {
                "source-abs" => world.owner.display().to_string(),
                "source-data" => Path::new("/System/Volumes/Data")
                    .join(world.owner.strip_prefix("/").unwrap())
                    .display()
                    .to_string(),
                _ => directory.to_owned(),
            };
            let mut typed = words(&["-Zunstable-options", "-C", &named, "--unverified"]);
            typed.push("build".into());
            let root = match target {
                "absolute" => {
                    typed.extend(["--target-dir".into(), world.target.display().to_string()]);
                    world.target.clone()
                }
                "out" => {
                    typed.extend(words(&["--target-dir", "out"]));
                    world.owner.join("out")
                }
                _ => world.owner.join("target"),
            };
            let quiet = super::super::target_build_path(&root);
            let placed = block(&world, &launcher, &typed);
            let directory_value = if target == "none" {
                super::super::target_build_path(Path::new("{workspace-root}/target"))
            } else {
                quiet.clone()
            };
            assert_eq!(
                placed[..2],
                [
                    "--config".to_owned(),
                    format!("build.build-dir='{}'", directory_value.display())
                ],
                "{typed:?}"
            );
            assert_eq!(placed[2..], typed);
            assert!(
                front(&world, &launcher, &placed, "config").unwrap(),
                "{typed:?}"
            );
            assert!(quiet.is_dir(), "{typed:?}");
            assert_eq!(
                owner(&world, &root),
                std::slice::from_ref(&world.owner),
                "{typed:?}"
            );
        }
    }
    // A package where atpkg manages no workspace (in `~/Documents`) gets the intermediates
    // its `-C` build writes prepared, read from that folder, with no custody.
    for target in [&[][..], &["--target-dir", "out"]] {
        let world = unwritable_cache();
        let launcher = world.home.join("launch");
        std::fs::create_dir(&launcher).unwrap();
        let documents = world.home.join("Documents/proj");
        std::fs::create_dir_all(&documents).unwrap();
        std::fs::write(documents.join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
        let mut typed = words(&[
            "-Zunstable-options",
            "-C",
            "../Documents/proj",
            "--unverified",
            "build",
        ]);
        typed.extend(words(target));
        let placed = block(&world, &launcher, &typed);
        assert_eq!(placed[2..], typed);
        assert!(
            !front(&world, &launcher, &placed, "config").unwrap(),
            "{typed:?}"
        );
        let root = documents.join(if target.is_empty() { "target" } else { "out" });
        assert!(super::super::target_build_path(&root).is_dir(), "{typed:?}");
        assert!(roots(&world.home).is_empty(), "{typed:?}");
    }
    let world = unwritable_cache();
    let launcher = world.home.join("launch");
    std::fs::create_dir(&launcher).unwrap();
    let typed = words(&["-C", "../source", "--unverified", "build"]);
    assert_eq!(block(&world, &launcher, &typed), typed);
    // The front, handed the call as typed, which Targo refuses for want of its gate, makes
    // and records nothing.
    front(&world, &launcher, &typed, "-").unwrap();
    assert!(!world.owner.join("target").exists());
    assert!(roots(&world.home).is_empty());
}

/// THE FRONT PREPARES WHERE TARGO WRITES THE SHIM'S PLACEMENT, and nothing for a call that
/// builds nothing, read by the front's own reader ([`live_read`]). The local fallback's
/// `{workspace-root}` is the root Targo finds, and the owner recorded: a package its
/// ancestor's workspace excludes, or that lies in a `target/package` folder, is its own root,
/// so its own `target` is recorded under it and no other made; a package whose manifest is a
/// link is that manifest's, never its parent's. A package where atpkg manages no workspace
/// (in `~/Documents`) gets its intermediates prepared, with no custody; one whose manifest
/// cannot be read is refused, nothing prepared. A query Targo answers (`-Zhelp`,
/// `--explain`) is admitted with nothing prepared. NEGATIVE CONTROL: the excluding root's own
/// member still writes the root's `target`; before, the owner walk read past
/// `target/package` (refusing the call where that manifest was past the bound, recording the
/// workspace above as owner where not), and a member's manifest through a link or past the
/// bound was passed for its parent's, whose `target` was recorded while Targo wrote the
/// member's.
#[cfg(target_os = "macos")]
#[test]
fn the_fronts_placement_lands_where_targo_writes_it() {
    use super::super::{BUILD_DIR_ENV, Tool};
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
    let fallback = super::super::target_build_path(Path::new("{workspace-root}/target"))
        .display()
        .to_string();
    let shim = |world: &World, cwd: &Path, placed: &str, typed: &[String], hidden: bool| {
        let env = |name: &str| {
            (hidden && name == BUILD_DIR_ENV).then(|| std::ffi::OsString::from(&fallback))
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(cwd),
            env: &env,
            read: &live_read,
            read_manifest: &live_read,
            writable: &|_| Some(true),
        };
        observe_shim_on(typed, Tool::Targo, placed, &world.home, &machine, true)
    };
    let trust = words(&["trust", "check"]);
    let world = World::new();
    let w = world.home.join("w");
    for dir in ["a", "pkg"] {
        std::fs::create_dir_all(w.join(dir)).unwrap();
        std::fs::write(
            w.join(dir).join("Cargo.toml"),
            format!("[package]\nname = \"{dir}\"\n"),
        )
        .unwrap();
    }
    std::fs::write(
        w.join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\"]\nexclude = [\"pkg\"]\n",
    )
    .unwrap();
    assert!(shim(&world, &w.join("pkg"), "env", &trust, true).unwrap());
    assert!(super::super::target_build_path(&w.join("pkg/target")).is_dir());
    assert!(!w.join("target").exists());
    assert!(shim(&world, &w.join("a"), "env", &trust, true).unwrap());
    assert!(super::super::target_build_path(&w.join("target")).is_dir());
    let mut made: Vec<_> = roots(&world.home).into_iter().map(|r| r.root).collect();
    made.sort();
    assert_eq!(made, [w.join("pkg/target"), w.join("target")]);

    // The root is the one Targo's walk finds, which stops before a folder that ends in
    // `target/package`: a package there is its own root, whatever the workspace above holds,
    // one past the reader's bound included, which that walk never reads.
    for above in [
        format!("[workspace]\n# {}\n", "x".repeat(1 << 20)),
        "[workspace]\nmembers = [\"target/package/q\"]\n".to_owned(),
    ] {
        let world = World::new();
        let packaged = world.home.join("w/target/package/q");
        std::fs::create_dir_all(&packaged).unwrap();
        std::fs::write(packaged.join("Cargo.toml"), "[package]\nname = \"q\"\n").unwrap();
        std::fs::write(world.home.join("w/Cargo.toml"), above).unwrap();
        assert!(shim(&world, &packaged, "env", &trust, true).unwrap());
        assert!(super::super::target_build_path(&packaged.join("target")).is_dir());
        let recorded = roots(&world.home);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].root, packaged.join("target"));
        assert_eq!(recorded[0].owners, std::slice::from_ref(&packaged));
    }

    // The package's manifest is the first that stands from where Targo works up, and only
    // absence lets the walk go up: a member whose manifest is a link, or one past the reader's
    // bound, tells no root, and the call is refused with nothing prepared, never handed to its
    // parent's.
    let world = World::new();
    let w = world.home.join("w");
    std::fs::create_dir_all(w.join("pkg")).unwrap();
    std::fs::write(w.join("Cargo.toml"), "[package]\nname = \"w\"\n").unwrap();
    std::fs::write(w.join("pkg/real.toml"), "[package]\nname = \"p\"\n").unwrap();
    std::os::unix::fs::symlink("real.toml", w.join("pkg/Cargo.toml")).unwrap();
    assert!(shim(&world, &w.join("pkg"), "env", &trust, true).is_err());
    assert!(!w.join("target").exists());
    assert!(!w.join("pkg/target").exists());
    assert!(roots(&world.home).is_empty());
    let world = World::new();
    let w = world.home.join("w");
    std::fs::create_dir_all(w.join("pkg")).unwrap();
    std::fs::write(w.join("Cargo.toml"), "[package]\nname = \"w\"\n").unwrap();
    let oversized = format!("[package]\nname = \"p\"\n# {}\n", "x".repeat(1 << 20));
    std::fs::write(w.join("pkg/Cargo.toml"), oversized).unwrap();
    assert!(shim(&world, &w.join("pkg"), "env", &trust, true).is_err());
    assert!(!w.join("target").exists());
    assert!(!w.join("pkg/target").exists());
    assert!(roots(&world.home).is_empty());

    let world = World::new();
    let documents = world.home.join("Documents/proj");
    std::fs::create_dir_all(&documents).unwrap();
    std::fs::write(documents.join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
    assert!(!shim(&world, &documents, "env", &trust, true).unwrap());
    assert!(super::super::target_build_path(&documents.join("target")).is_dir());
    assert!(roots(&world.home).is_empty());

    let world = World::new();
    let out = world.owner.join("out");
    let explained = words(&[
        "--unverified",
        "--explain",
        "E0308",
        "build",
        "--target-dir",
        "out",
    ]);
    let placed = [
        words(&[
            "--config",
            &format!(
                "build.build-dir='{}'",
                super::super::target_build_path(&out).display()
            ),
        ]),
        explained,
    ]
    .concat();
    for (placed, typed, hidden) in [
        ("env", words(&["-Zhelp", "trust", "check"]), true),
        ("config", placed, false),
    ] {
        assert!(
            !shim(&world, &world.owner, placed, &typed, hidden).unwrap(),
            "{typed:?}"
        );
    }
    assert!(!world.owner.join("target").exists());
    assert!(!out.exists());
    assert!(roots(&world.home).is_empty());
}

/// A workspace rooted at HOME, which atpkg never manages, builds into `~/target` whenever
/// its call names no target: the local fallback's `{workspace-root}/target` and a cache
/// placement's final artifacts alike. Where that folder is a root recorded for another
/// workspace, its sole recorded owner going would retire it while the HOME workspace still
/// builds there, so the call holds it in custody (`owners_unknown`); the same member naming
/// it explicitly is refused, as it owes custody no owner may take. NEGATIVE CONTROL: where
/// nothing is recorded there, nothing is.
#[test]
fn a_cache_placed_build_keeps_its_default_target_after_the_previous_owner_goes() {
    for shape in ["recorded", "linked", "corrupt", "ordinary", "fresh"] {
        let world = World::new();
        let owner = world.another_owner();
        let target = owner.join("target");
        let root = if shape == "linked" {
            world.target.clone()
        } else {
            target.clone()
        };
        let recorded = matches!(shape, "recorded" | "linked" | "corrupt");
        if recorded {
            assert!(create_checked(&world.home, &root, &world.owner).unwrap());
        }
        if shape != "fresh" {
            std::fs::create_dir_all(root.join("debug/deps")).unwrap();
            std::fs::write(root.join("debug/deps/keep"), b"reusable output").unwrap();
            std::fs::write(root.join("debug/.cargo-lock"), b"").unwrap();
        }
        if shape == "linked" {
            std::os::unix::fs::symlink(&root, &target).unwrap();
        }
        if shape == "corrupt" {
            let path = world
                .home
                .join(INDEX)
                .join(format!("{}.json", key(&root).unwrap()));
            std::fs::write(path, b"invalid receipt").unwrap();
        }
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&owner),
            env: &|_| None,
            read: &live_read,
            read_manifest: &live_read,
            writable: &|_| Some(true),
        };
        let args = ["--unverified".into(), "build".into()];
        assert!(matches!(
            super::super::decide(&args, super::super::Tool::Targo, &machine),
            Some(super::super::BuildDir::Cache(_))
        ));
        let observed = observe_for_producer(
            &args,
            super::super::Tool::Targo,
            &world.home,
            &machine,
            true,
        );
        if shape == "corrupt" {
            assert!(observed.is_err(), "{shape}: {observed:?}");
            assert!(target.join("debug/deps/keep").is_file());
            continue;
        }
        assert_eq!(observed.unwrap(), recorded, "{shape}");
        std::fs::remove_dir_all(&world.owner).unwrap();
        let processes = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
        let fence = super::super::fence::Fence::new(&processes);
        assert!(
            reclaim_on(
                &world.home,
                world.later(),
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| true,
                &fence,
            )
            .is_empty(),
            "{shape}"
        );
        if recorded {
            assert!(target.join("debug/deps/keep").is_file());
            let receipt = roots(&world.home)
                .into_iter()
                .find(|r| r.root == root)
                .unwrap();
            assert!(receipt.owners.contains(&owner));
            assert!(!receipt.owners_unknown);
        } else {
            assert_eq!(target.exists(), shape != "fresh");
            assert!(
                roots(&world.home).is_empty(),
                "{shape}: ordinary targets remain unowned"
            );
        }
    }
}

#[test]
fn a_home_rooted_workspace_holds_a_recorded_home_target_in_custody() {
    use super::super::{BUILD_DIR_ENV, Tool};
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
    let fallback = super::super::target_build_path(Path::new("{workspace-root}/target"))
        .display()
        .to_string();
    let call = |world: &World, cwd: &Path, typed: &[String], hop: bool| {
        let env = |name: &str| {
            (hop && name == BUILD_DIR_ENV).then(|| std::ffi::OsString::from(&fallback))
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(cwd),
            env: &env,
            read: &live_read,
            read_manifest: &live_read,
            writable: &|_| Some(true),
        };
        if hop {
            observe_shim_on(typed, Tool::Targo, "env", &world.home, &machine, true)
        } else {
            observe_for_producer(typed, Tool::Targo, &world.home, &machine, true)
        }
    };
    let package = |dir: &Path, text: &str| {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), text).unwrap();
    };
    let fallback_check = words(&["trust", "check"]);
    let cache_build = words(&["--unverified", "build"]);
    for (typed, hop, recorded) in [
        (&fallback_check, true, true),
        (&cache_build, false, true),
        (&fallback_check, true, false),
        (&cache_build, false, false),
    ] {
        let world = World::new();
        let home_target = world.home.join("target");
        if recorded {
            // Another workspace, its own root, records `~/target` as its explicit target.
            let w = world.home.join("w");
            package(&w, "[package]\nname = \"w\"\n[workspace]\n");
            let mut explicit = words(&["--unverified", "build", "--target-dir"]);
            explicit.push(home_target.display().to_string());
            assert!(call(&world, &w, &explicit, false).unwrap());
            let receipt = &roots(&world.home)[0];
            assert_eq!(receipt.root, home_target);
            assert_eq!(receipt.owners, std::slice::from_ref(&w));
            assert!(!receipt.owners_unknown);
        }
        std::fs::write(
            world.home.join("Cargo.toml"),
            "[workspace]\nmembers = [\"proj\"]\n",
        )
        .unwrap();
        package(&world.home.join("proj"), "[package]\nname = \"p\"\n");
        let observed = call(&world, &world.home.join("proj"), typed, hop);
        assert!(matches!(observed, Ok(false)), "{typed:?}: {observed:?}");
        let records = roots(&world.home);
        if recorded {
            assert_eq!(records.len(), 1, "{typed:?}");
            assert_eq!(records[0].root, home_target);
            assert!(
                records[0].owners_unknown,
                "{typed:?}: the HOME workspace's target is held in custody"
            );
        } else {
            assert!(records.is_empty(), "{typed:?}: nothing is recorded");
            assert!(!home_target.join(MARKER).exists());
        }
    }
}

/// A call whose owner atpkg keeps no custody of builds into the build directory it names as
/// into its target, so a root recorded there for another workspace is held in custody
/// (`owners_unknown`) as its target would be, never retired once that workspace goes while
/// this call still builds there: the variable, a `--config` and a config file's spelling, for
/// a package at HOME itself named by `--manifest-path` from a folder in HOME (a producer, which
/// owes no custody; run from HOME itself it reads no explicit target), and for a member of a
/// workspace rooted at HOME as observed (a producer of it owes custody no owner may take, and
/// is refused). A package outside HOME named so is held too, though not by this arm: its line
/// names a manifest the explicit reader does not take (`Line::parse`), so the call holds every
/// recorded root. NEGATIVE CONTROL: where nothing is recorded, nothing is, nor made.
#[test]
fn an_ownerless_call_holds_a_recorded_build_dir_in_custody() {
    use super::super::{BUILD_DIR_ENV, Tool};
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
    let call = |world: &World, cwd: &Path, bd: Option<&Path>, typed: &[String], producer| {
        let env = |name: &str| {
            (name == BUILD_DIR_ENV)
                .then_some(bd)
                .flatten()
                .map(|bd| bd.as_os_str().to_owned())
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(cwd),
            env: &env,
            read: &live_read,
            read_manifest: &live_read,
            writable: &|_| Some(true),
        };
        observe_for_producer(typed, Tool::Targo, &world.home, &machine, producer)
    };
    let package = |dir: &Path, text: &str| {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), text).unwrap();
    };
    for recorded in [true, false] {
        for owner in ["home", "outside", "home-rooted"] {
            for spelling in ["env", "cli", "file"] {
                let row = format!("{owner} {spelling} recorded {recorded}");
                let world = World::new();
                let bd = world.home.join("bd");
                let out = world.home.join("out");
                if recorded {
                    // Another workspace, its own root, records `~/bd` as its explicit target.
                    let w = world.home.join("w");
                    package(&w, "[package]\nname = \"w\"\n[workspace]\n");
                    let mut explicit = words(&["--unverified", "build", "--target-dir"]);
                    explicit.push(bd.display().to_string());
                    assert!(call(&world, &w, None, &explicit, true).unwrap(), "{row}");
                    let receipt = &roots(&world.home)[0];
                    assert_eq!(receipt.root, bd, "{row}");
                    assert_eq!(receipt.owners, std::slice::from_ref(&w), "{row}");
                    assert!(!receipt.owners_unknown, "{row}");
                }
                let notes = world.home.join("notes");
                let (cwd, manifest) = match owner {
                    "home" => {
                        package(&world.home, "[package]\nname = \"h\"\n");
                        std::fs::create_dir(&notes).unwrap();
                        (notes, Some(world.home.join("Cargo.toml")))
                    }
                    "outside" => {
                        let elsewhere = world.home.parent().unwrap().join("elsewhere");
                        package(&elsewhere, "[package]\nname = \"e\"\n");
                        std::fs::create_dir(&notes).unwrap();
                        (notes, Some(elsewhere.join("Cargo.toml")))
                    }
                    _ => {
                        std::fs::write(
                            world.home.join("Cargo.toml"),
                            "[workspace]\nmembers = [\"proj\"]\n",
                        )
                        .unwrap();
                        package(&world.home.join("proj"), "[package]\nname = \"p\"\n");
                        (world.home.join("proj"), None)
                    }
                };
                let mut typed = words(&["--unverified", "build"]);
                if let Some(manifest) = &manifest {
                    typed.push("--manifest-path".to_owned());
                    typed.push(manifest.display().to_string());
                }
                typed.push("--target-dir".to_owned());
                typed.push(out.display().to_string());
                match spelling {
                    "env" => {}
                    "cli" => {
                        typed.push("--config".to_owned());
                        typed.push(format!("build.build-dir='{}'", bd.display()));
                    }
                    _ => {
                        std::fs::create_dir_all(cwd.join(".cargo")).unwrap();
                        std::fs::write(
                            cwd.join(".cargo/config.toml"),
                            format!("[build]\nbuild-dir = \"{}\"\n", bd.display()),
                        )
                        .unwrap();
                    }
                }
                let variable = (spelling == "env").then_some(bd.as_path());
                let home_rooted = owner == "home-rooted";
                if home_rooted {
                    let refused = call(&world, &cwd, variable, &typed, true);
                    assert!(refused.is_err(), "{row}: {refused:?}");
                    assert!(!out.exists(), "{row}");
                }
                let observed = call(&world, &cwd, variable, &typed, !home_rooted);
                assert!(matches!(observed, Ok(false)), "{row}: {observed:?}");
                let records = roots(&world.home);
                if recorded {
                    assert_eq!(records.len(), 1, "{row}");
                    assert_eq!(records[0].root, bd, "{row}");
                    assert!(
                        records[0].owners_unknown,
                        "{row}: the build directory is held in custody"
                    );
                } else {
                    assert!(records.is_empty(), "{row}: nothing is recorded");
                    assert!(!bd.join(MARKER).exists(), "{row}");
                    assert!(!out.join(MARKER).exists(), "{row}");
                }
            }
        }
    }
}

#[test]
fn the_fronts_owner_is_the_root_targo_finds_where_it_works() {
    use super::super::{BUILD_DIR_ENV, Tool};
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
    let fallback = super::super::target_build_path(Path::new("{workspace-root}/target"))
        .display()
        .to_string();
    // `typed` from `cwd` with `more` set: through the hop with the fallback exported
    // (`env`), or as a producer with nothing placed.
    let call = |world: &World, cwd: &Path, more: &[(&str, String)], typed: &[String], hop| {
        let env = |name: &str| {
            if hop && name == BUILD_DIR_ENV {
                return Some(std::ffi::OsString::from(&fallback));
            }
            more.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.into())
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(cwd),
            env: &env,
            read: &live_read,
            read_manifest: &live_read,
            writable: &|_| Some(true),
        };
        if hop {
            observe_shim_on(typed, Tool::Targo, "env", &world.home, &machine, true)
        } else {
            observe_for_producer(typed, Tool::Targo, &world.home, &machine, true)
        }
    };
    let package = |dir: &Path, text: &str| {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), text).unwrap();
    };
    let recorded = |world: &World| -> Vec<(PathBuf, Vec<PathBuf>)> {
        let mut made: Vec<_> = roots(&world.home)
            .into_iter()
            .map(|root| (root.root, root.owners))
            .collect();
        made.sort();
        made
    };
    let fresh = |world: &World| world.home.join("fresh");

    // A relative package home is the `-C` directory's, as Targo's `reload_cwd` takes it,
    // so its root walk stops there. With the launcher's, the walk stops before the
    // workspace that holds the package, which would record the member as the sole owner of
    // the workspace's target (freed once that member goes, while the workspace builds).
    let world = World::new();
    let w = world.home.join("w");
    package(&w, "[workspace]\nmembers = [\"crates/p\"]\n");
    package(&w.join("crates/p"), "[package]\nname = \"p\"\n");
    let dot = [("CARGO_HOME", ".".to_owned())];
    let from_crates = words(&[
        "-Zunstable-options",
        "-C",
        "..",
        "trust",
        "check",
        "--manifest-path",
        "crates/p/Cargo.toml",
    ]);
    assert!(call(&world, &w.join("crates"), &dot, &from_crates, true).unwrap());
    assert!(super::super::target_build_path(&w.join("target")).is_dir());
    assert_eq!(recorded(&world), [(w.join("target"), vec![w.clone()])]);
    // The explicit spelling records the same owner.
    let world = World::new();
    let w = world.home.join("w");
    package(&w, "[workspace]\nmembers = [\"crates/p\"]\n");
    package(&w.join("crates/p"), "[package]\nname = \"p\"\n");
    let mut explicit = words(&[
        "-Zunstable-options",
        "-C",
        "..",
        "--unverified",
        "build",
        "--manifest-path",
        "crates/p/Cargo.toml",
        "--target-dir",
    ]);
    explicit.push(fresh(&world).display().to_string());
    assert!(call(&world, &w.join("crates"), &dot, &explicit, false).unwrap());
    assert_eq!(recorded(&world), [(fresh(&world), vec![w.clone()])]);
    // The reverse: Targo's walk stops at the `-C` directory, where the launcher's would
    // pass on to a workspace that does not list the package. Read so, the fallback's owner
    // could not be told and its target was prepared with nothing recorded; the explicit
    // target was refused, a call Targo builds.
    let world = World::new();
    let w = world.home.join("w");
    package(&w, "[workspace]\nmembers = [\"crates/*\"]\n");
    package(&w.join("h/p"), "[package]\nname = \"p\"\n");
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let from_notes = words(&[
        "-Zunstable-options",
        "-C",
        "../w/h",
        "trust",
        "check",
        "--manifest-path",
        "p/Cargo.toml",
    ]);
    assert!(call(&world, &notes, &dot, &from_notes, true).unwrap());
    assert_eq!(
        recorded(&world),
        [(w.join("h/p/target"), vec![w.join("h/p")])]
    );
    let world = World::new();
    let w = world.home.join("w");
    package(&w, "[workspace]\nmembers = [\"crates/*\"]\n");
    package(&w.join("h/p"), "[package]\nname = \"p\"\n");
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let mut explicit = words(&[
        "-Zunstable-options",
        "-C",
        "../w/h",
        "--unverified",
        "build",
        "--manifest-path",
        "p/Cargo.toml",
        "--target-dir",
    ]);
    explicit.push(fresh(&world).display().to_string());
    assert!(call(&world, &notes, &dot, &explicit, false).unwrap());
    assert_eq!(recorded(&world), [(fresh(&world), vec![w.join("h/p")])]);
    // NEGATIVE CONTROL: with no `-C`, the launcher's home is Targo's, and the walk from the
    // package passes on to that workspace, which does not list it.
    let world = World::new();
    let w = world.home.join("w");
    package(&w, "[workspace]\nmembers = [\"crates/*\"]\n");
    package(&w.join("h/p"), "[package]\nname = \"p\"\n");
    let mut plain = words(&["--unverified", "build", "--target-dir"]);
    plain.push(fresh(&world).display().to_string());
    assert!(call(&world, &w.join("h/p"), &dot, &plain, false).is_err());
    assert!(roots(&world.home).is_empty());

    // Where `CARGO_HOME` is unset, Targo 16387's walk stops after `~/.cargo` and Trust's
    // main's after `TARGO_HOME`: a root they find apart is none either may take, so the
    // call is refused with nothing prepared, never recorded under one build's root while
    // the other writes another's. Where they agree, it is that root.
    let world = World::new();
    let w = world.home.join("w");
    package(&w, "[workspace]\nmembers = [\"th/tool\"]\n");
    package(&w.join("th/tool"), "[package]\nname = \"tool\"\n");
    let twin = [("TARGO_HOME", w.join("th").display().to_string())];
    let check = words(&["trust", "check"]);
    assert!(call(&world, &w.join("th/tool"), &twin, &check, true).is_err());
    assert!(!w.join("target").exists());
    assert!(!w.join("th/tool/target").exists());
    assert!(roots(&world.home).is_empty());
    let elsewhere = [("TARGO_HOME", world.home.join("th").display().to_string())];
    assert!(call(&world, &w.join("th/tool"), &elsewhere, &check, true).unwrap());
    assert_eq!(recorded(&world), [(w.join("target"), vec![w.clone()])]);

    // A package reached through a linked folder builds into its root's `target` as Targo
    // spells it, which is the folder the link names: that folder is the one recorded and
    // prepared, as the owner is the root's own.
    let world = World::new();
    let real = world.home.join("real/a");
    package(&real, "[package]\nname = \"a\"\n");
    let x = world.home.join("x");
    std::fs::create_dir(&x).unwrap();
    std::os::unix::fs::symlink(&real, x.join("link")).unwrap();
    let linked = words(&["trust", "check", "--manifest-path", "link/Cargo.toml"]);
    assert!(call(&world, &x, &[], &linked, true).unwrap());
    assert!(super::super::target_build_path(&real.join("target")).is_dir());
    assert_eq!(
        recorded(&world),
        [(real.join("target"), vec![real.clone()])]
    );
    // Again, now that it is recorded.
    assert!(call(&world, &x, &[], &linked, true).unwrap());
    assert_eq!(
        recorded(&world),
        [(real.join("target"), vec![real.clone()])]
    );

    // A member of a workspace rooted at HOME, which no owner may be, still names a folder it
    // writes: it is refused, nothing recorded or made.
    let world = World::new();
    std::fs::write(
        world.home.join("Cargo.toml"),
        "[workspace]\nmembers = [\"proj\"]\n",
    )
    .unwrap();
    package(&world.home.join("proj"), "[package]\nname = \"p\"\n");
    let mut out = words(&["--unverified", "build", "--target-dir"]);
    out.push(world.home.join("out").display().to_string());
    assert!(call(&world, &world.home.join("proj"), &[], &out, false).is_err());
    assert!(roots(&world.home).is_empty());
    assert!(!world.home.join("out").exists());
    // NEGATIVE CONTROL: one folder down, the workspace is one atpkg manages.
    let world = World::new();
    let w = world.home.join("w");
    package(&w, "[workspace]\nmembers = [\"proj\"]\n");
    package(&w.join("proj"), "[package]\nname = \"p\"\n");
    let mut out = words(&["--unverified", "build", "--target-dir"]);
    out.push(world.home.join("out").display().to_string());
    assert!(call(&world, &w.join("proj"), &[], &out, false).unwrap());

    // A path dependency of a member a glob names is a member too (`find_members` follows
    // each glob's folders' path dependencies): its root is the workspace's.
    let world = World::new();
    let w = world.home.join("w");
    package(&w, "[workspace]\nmembers = [\"crates/*\"]\n");
    package(
        &w.join("crates/a"),
        "[package]\nname = \"a\"\n[dependencies]\nb = { path = \"../../libs/b\" }\n",
    );
    package(&w.join("libs/b"), "[package]\nname = \"b\"\n");
    package(&w.join("libs/u"), "[package]\nname = \"u\"\n");
    assert!(call(&world, &w.join("libs/b"), &[], &check, true).unwrap());
    assert_eq!(recorded(&world), [(w.join("target"), vec![w.clone()])]);
    let mut explicit = words(&["--unverified", "build", "--target-dir"]);
    explicit.push(fresh(&world).display().to_string());
    assert!(call(&world, &w.join("libs/b"), &[], &explicit, false).unwrap());
    // NEGATIVE CONTROL: a package no member depends on is none.
    assert!(call(&world, &w.join("libs/u"), &[], &explicit, false).is_err());
}

/// A NEW HOME'S FIRST BUILD IS ADMITTED IN THE SHARED CACHE: in a home with no `Library`, read
/// with the real writability probe, the block selects the shared cache (the home itself may be
/// written, so the cache's folders can be made); the front admits that claim and lays the
/// cache's folders, and the same claim is admitted again once they stand. 0.199 selected the
/// in-tree fallback there and refused every build (`the selected quiet build target could not
/// be resolved safely`): admission's own write of those folders made the plan it read again the
/// shared cache. The selector now reads the folder admission writes in, so the plan cannot move.
/// NEGATIVE CONTROL: the in-tree fallback claimed in the same home is no placement the block
/// selects there and is refused as a mismatched claim, with nothing prepared. A native
/// reroute's selected argv is admitted on the same reading, twice.
#[cfg(target_os = "macos")]
#[test]
fn a_new_homes_first_build_is_admitted_in_the_shared_cache() {
    let world = World::new();
    assert!(!world.home.join("Library").exists());
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &|_| None,
        read: &super::super::read_config,
        writable: &super::super::writable,
        read_manifest: &|p| Ok(std::fs::read_to_string(p).ok()),
    };
    let typed = vec!["--unverified".to_owned(), "build".to_owned()];
    let tool = super::super::Tool::Targo;
    let Some(super::super::BuildDir::Cache(claim)) = super::super::decide(&typed, tool, &machine)
    else {
        panic!("the block selects the shared cache in a new home");
    };
    let handed = [vec!["--config".to_owned(), claim], typed.clone()].concat();
    for call in ["first", "second"] {
        let owned = observe_shim_on(&handed, tool, "config", &world.home, &machine, true);
        assert!(
            !owned.unwrap(),
            "{call}: admitted, the shared cache owned by no receipt"
        );
        assert!(
            world.home.join(CACHES).is_dir(),
            "{call}: its folders stand"
        );
    }

    let fallback = super::super::target_build_path(Path::new("{workspace-root}/target"));
    let fallback = format!("build.build-dir='{}'", fallback.display());
    let quiet = super::super::target_build_path(&world.owner.join("target"));
    let other = World::new();
    for home in [&other, &world] {
        let machine = super::super::Machine {
            home: Some(&home.home),
            cwd: Some(&home.owner),
            ..machine
        };
        let handed = [vec!["--config".to_owned(), fallback.clone()], typed.clone()].concat();
        let stale = observe_shim_on(&handed, tool, "config", &home.home, &machine, true);
        assert_eq!(
            stale.unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput,
            "a fallback claim where the cache may be written is mismatched"
        );
        assert!(
            !super::super::target_build_path(&home.owner.join("target")).exists(),
            "a refused claim prepares nothing"
        );
    }
    assert!(!quiet.exists());

    let world = World::new();
    let machine = super::super::Machine {
        home: Some(&world.home),
        cwd: Some(&world.owner),
        ..machine
    };
    let (selected, placed) = super::super::placed_build_dir(&typed, tool, &machine).unwrap();
    assert!(matches!(selected, super::super::BuildDir::Cache(_)));
    observe_placed_command_with(&typed, &placed, tool, &machine).unwrap();
    observe_placed_command_with(&typed, &placed, tool, &machine).unwrap();
}

/// THE SHIM'S NO-CACHE FALLBACK OWES NO RECEIPT IN A HOME THAT CAN RECORD NONE: where
/// `Library/Caches` stands and may not be written (`read-only`), the block selects the in-tree
/// fallback and no index can be laid; the front admits the claim unowned, prepares the
/// workspace's quiet build directory and writes nothing under the caches folder. Before, it
/// refused every build there (`the target's quiet build directory could not be owned`).
/// NEGATIVE CONTROLS, each refused in those words with the directory left as it stood: the same
/// home whose caches folder reaches aterm's own through a link (`linked`), which says nothing
/// of what the home can record; and a home that records owners (`foreign`), where the trust
/// lane's fallback names a directory somebody else already filled.
#[cfg(target_os = "macos")]
#[test]
fn the_shims_no_cache_fallback_owes_no_receipt_in_a_home_that_can_record_none() {
    let fallback = super::super::target_build_path(Path::new("{workspace-root}/target"));
    let fallback = fallback.to_str().unwrap();
    for shape in ["read-only", "linked", "foreign"] {
        let world = World::new();
        let caches = world.home.join(CACHES);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&caches)
            .unwrap();
        let quiet = super::super::target_build_path(&world.owner.join("target"));
        match shape {
            "linked" => {
                let elsewhere = world.home.join("elsewhere");
                std::fs::create_dir(&elsewhere).unwrap();
                std::os::unix::fs::symlink(&elsewhere, caches.join("aterm")).unwrap();
            }
            "foreign" => {
                std::fs::create_dir_all(&quiet).unwrap();
                std::fs::write(quiet.join("keep"), b"somebody's").unwrap();
            }
            _ => {}
        }
        let (typed, placed): (&[&str], _) = if shape == "foreign" {
            (&["trust", "check"], "env")
        } else {
            std::fs::set_permissions(&caches, std::fs::Permissions::from_mode(0o500)).unwrap();
            (&["--unverified", "build"], "config")
        };
        let typed: Vec<String> = typed.iter().map(|word| (*word).to_owned()).collect();
        let environment = |name: &str| {
            (placed == "env" && name == super::super::BUILD_DIR_ENV).then(|| fallback.into())
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &environment,
            read: &super::super::read_config,
            writable: &super::super::writable,
            read_manifest: &|p| Ok(std::fs::read_to_string(p).ok()),
        };
        let handed = if placed == "env" {
            typed
        } else {
            let claim = format!("build.build-dir='{fallback}'");
            [vec!["--config".to_owned(), claim], typed].concat()
        };
        let tool = super::super::Tool::Targo;
        let result = observe_shim_on(&handed, tool, placed, &world.home, &machine, true);
        std::fs::set_permissions(&caches, std::fs::Permissions::from_mode(0o700)).unwrap();
        if shape == "read-only" {
            assert!(!result.unwrap(), "admitted, and owned by no receipt");
            assert!(quiet.is_dir(), "its quiet build directory is prepared");
            assert_eq!(std::fs::read_dir(&caches).unwrap().count(), 0);
            continue;
        }
        let refusal = result.unwrap_err().to_string();
        assert!(refusal.contains("could not be owned"), "{shape}: {refusal}");
        assert_eq!(quiet.is_dir(), shape == "foreign", "{shape}");
        assert_eq!(roots(&world.home).len(), 0, "{shape}");
    }
}

#[test]
fn helper_failure_or_interruption_starts_no_compiler() {
    let world = World::new();
    let helper = world.home.join("registrar");
    let shim = world.home.join("targo");
    let log = world.home.join("ran");
    for exit in [75, 130, 2] {
        std::fs::write(&helper, format!("#!/bin/sh\nexit {exit}\n")).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\n{}\nprintf 'compiler\\n' >> \"$ATPKG_TEST_CACHE_LOG\"\n",
                shim_call(&helper, "targo")
            ),
        )
        .unwrap();
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command
            .arg(&shim)
            .env_clear()
            .env("ATPKG_TEST_CACHE_LOG", &log);
        let result =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        assert_eq!(result.status.code(), Some(75));
        assert!(!log.exists());
    }
    std::fs::write(&helper, "#!/bin/sh\nexit 0\n").unwrap();
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command
        .arg(&shim)
        .env_clear()
        .env("ATPKG_TEST_CACHE_LOG", &log);
    let result = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
    assert!(result.status.success());
    assert_eq!(std::fs::read_to_string(log).unwrap(), "compiler\n");
}

#[test]
fn a_contained_recorded_owner_needs_no_index_write_authority() {
    let world = World::new();
    world.build();
    let original = std::fs::read(world.path()).unwrap();
    let other_root = world.home.join("another-root.noindex");
    assert!(create_checked(&world.home, &other_root, &world.owner).unwrap());
    let other_path = world
        .home
        .join(INDEX)
        .join(format!("{}.json", key(&other_root).unwrap()));
    std::fs::copy(other_path, world.path()).unwrap();
    assert!(create_checked(&world.home, &world.target, &world.owner).is_err());
    std::fs::write(world.path(), original).unwrap();
    let directory = world.home.join(INDEX);
    let gates = directory.join("locks.noindex");
    let gate = gate_path(&world.path()).unwrap();
    for path in [&directory, &gates] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
    }
    for path in [&world.path(), &gate] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400)).unwrap();
    }
    assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
    let other = world.another_owner();
    assert!(create_checked(&world.home, &world.target, &other).is_err());
    for path in [&directory, &gates] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

/// THE RECORD OF `world`'s TARGET, ITS RECEIPT AND ITS MARKER, AS KEEPING `identity`: as an
/// earlier boot, an older atpkg or another volume recorded it (VOLUME-IDENTITY). The marker is
/// written through the folder held, as atpkg writes one.
fn recorded_as(world: &World, identity: &Identity) {
    let path = world.path();
    let mut receipt = read(&path).unwrap();
    receipt.identity = identity.clone();
    write(&path, &receipt).unwrap();
    let held = creation::pin(&world.target).unwrap();
    let mut marker = retention::receipt_at(&held).unwrap();
    marker.identity = identity.clone();
    retention::write_marker(&held, &marker).unwrap();
}

/// The identity an earlier boot recorded of the folder read now as `now`: the same volume and
/// inode, the volume numbered otherwise, as a restart renumbers one. Only the tests that read
/// a volume's UUID (macOS) ask for one.
#[cfg(target_os = "macos")]
fn an_earlier_boots(now: &Identity) -> Identity {
    Identity {
        device: now.device.wrapping_add(1),
        ..now.clone()
    }
}

/// WHETHER A RECORD NAMES A FOLDER, AS [`Identity::names`] DECIDES IT (VOLUME-IDENTITY): a
/// record that keeps the volume's UUID names the same inode on the volume of that UUID
/// whatever its device number now (a), and nothing on another volume, its device number and
/// inode those recorded (b), nor on a volume that reads with no UUID now; nothing of another
/// inode (c); a record that keeps none names by its device number alone (d), and is witnessed
/// (gains the UUID, its device number and inode kept) only while that number still names it.
/// Two records name one folder by their UUIDs where both keep one, else by device number.
#[test]
fn a_record_names_a_folder_by_its_volumes_uuid_else_by_its_device_number() {
    let (one, another) = ([7; 16], [8; 16]);
    let at = |device, inode, volume| Identity {
        device,
        inode,
        volume,
    };
    let recorded = at(16_777_233, 42, Some(one));
    assert!(
        recorded.names(&at(16_777_232, 42, Some(one))),
        "(a) renumbered"
    );
    assert!(recorded.names(&at(16_777_233, 42, Some(one))));
    assert!(!recorded.names(&at(16_777_233, 42, Some(another))), "(b)");
    assert!(!recorded.names(&at(16_777_233, 42, None)), "no UUID now");
    assert!(!recorded.names(&at(16_777_233, 43, Some(one))), "(c)");
    let legacy = at(16_777_233, 42, None);
    assert!(
        legacy.names(&at(16_777_233, 42, Some(one))),
        "(d) as before"
    );
    assert!(
        !legacy.names(&at(16_777_232, 42, Some(one))),
        "(d) renumbered"
    );
    assert!(!legacy.names(&at(16_777_233, 43, Some(one))));
    assert_eq!(
        legacy.witnessed(&at(16_777_233, 42, Some(one))),
        Some(at(16_777_233, 42, Some(one)))
    );
    assert_eq!(legacy.witnessed(&at(16_777_232, 42, Some(one))), None);
    assert_eq!(legacy.witnessed(&at(16_777_233, 42, None)), None);
    assert_eq!(recorded.witnessed(&at(16_777_233, 42, Some(one))), None);
    assert!(
        legacy.same_record(&recorded),
        "witnessed beside unwitnessed"
    );
    assert!(recorded.same_record(&at(16_777_232, 42, Some(one))));
    assert!(!recorded.same_record(&at(16_777_233, 42, Some(another))));
    assert!(!legacy.same_record(&at(16_777_232, 42, None)));
    assert!(!legacy.same_record(&at(16_777_233, 43, None)));
    // A record that keeps no UUID is written as an older atpkg wrote one, and reads back.
    let bytes = aterm_json::to_vec(&legacy).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("volume"));
    assert_eq!(aterm_json::from_slice::<Identity>(&bytes).unwrap(), legacy);
    let bytes = aterm_json::to_vec(&recorded).unwrap();
    assert_eq!(
        aterm_json::from_slice::<Identity>(&bytes).unwrap(),
        recorded
    );
}

/// (a) A RECEIPT AN EARLIER BOOT RECORDED BINDS ITS FOLDER BY ITS VOLUME'S UUID: a build
/// records the volume's UUID beside the device number; recorded with the volume numbered
/// otherwise (as before a restart: the same UUID and inode, another device number), the cache
/// is still the registered root it was, its owner admitted with nothing written, another
/// owner recorded, the device number kept as recorded, and its marker binds it, read by its
/// path and through the folder held. Before receipts kept the UUID, this was the restart's
/// `the managed build cache's owner could not be recorded`, exit 75.
#[cfg(target_os = "macos")]
#[test]
fn a_receipt_an_earlier_boot_recorded_binds_its_folder_by_its_volumes_uuid() {
    let world = World::new();
    world.build();
    let now = identity(&world.target).unwrap();
    assert!(now.volume.is_some(), "the scratch volume has a UUID");
    assert_eq!(read(&world.path()).unwrap().identity, now);
    let earlier = an_earlier_boots(&now);
    recorded_as(&world, &earlier);
    assert!(same(&world.home, &read(&world.path()).unwrap()));
    let index = receipts(&world.home);
    assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
    assert_eq!(
        receipts(&world.home),
        index,
        "an owner on record writes nothing"
    );
    let other = world.another_owner();
    assert!(create_checked(&world.home, &world.target, &other).unwrap());
    let receipt = read(&world.path()).unwrap();
    assert_eq!(
        receipt.identity, earlier,
        "the device number stays as recorded"
    );
    assert_eq!(receipt.owners, [world.owner.clone(), other]);
    assert!(
        roots(&world.home)
            .iter()
            .any(|cache| cache.root == world.target)
    );
    assert_eq!(retention::kept_by(&world.target), None);
    assert!(retention::allows_at(creation::pin(&world.target).unwrap()));
}

/// (b) A RECEIPT OF ANOTHER VOLUME BINDS NOTHING, THOUGH ITS DEVICE NUMBER AND INODE MATCH: a
/// record whose UUID is another volume's, with the device number and inode the folder reads
/// with now (another volume reusing that number, as a restart can leave it), names nothing
/// here: the producer is refused and nothing is written, it is no registered root, and its
/// marker binds nothing, so the ordinary cleaners keep the folder. MUTANT: with the UUID
/// dropped from [`Identity::names`] (the device number alone deciding), the producer is
/// admitted and this fails.
#[cfg(target_os = "macos")]
#[test]
fn a_receipt_of_another_volume_binds_nothing_though_its_device_number_and_inode_match() {
    let world = World::new();
    world.build();
    let now = identity(&world.target).unwrap();
    let mut volume = now.volume.unwrap();
    volume[0] ^= 0xff;
    let elsewhere = Identity {
        volume: Some(volume),
        ..now.clone()
    };
    assert_eq!((elsewhere.device, elsewhere.inode), (now.device, now.inode));
    recorded_as(&world, &elsewhere);
    assert!(!same(&world.home, &read(&world.path()).unwrap()));
    let index = receipts(&world.home);
    let refused = create_checked(&world.home, &world.target, &world.another_owner())
        .expect_err("another volume's receipt admits no producer");
    assert_ne!(refused.kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(
        refused.to_string(),
        "the managed build cache's owner could not be recorded"
    );
    assert_eq!(receipts(&world.home), index, "nothing is written");
    assert!(
        !roots(&world.home)
            .iter()
            .any(|cache| cache.root == world.target)
    );
    assert_eq!(
        retention::kept_by(&world.target),
        Some(world.target.as_path())
    );
    assert!(!retention::allows_at(creation::pin(&world.target).unwrap()));
}

/// (c) A FOLDER SWAPPED IN AT A RECEIPT'S PATH BINDS NOTHING: the cache moved aside and
/// another private folder made at its path, holding a copy of its marker: the same volume and
/// device number, another inode. The producer is refused, nothing is written, and the copied
/// marker binds nothing there, so the ordinary cleaners keep it; the cache moved aside binds
/// nothing either (its receipt names its path).
#[test]
fn a_folder_swapped_in_at_a_receipts_path_binds_nothing() {
    let world = World::new();
    world.build();
    let receipt = read(&world.path()).unwrap();
    let aside = world.home.join("aside.noindex");
    std::fs::rename(&world.target, &aside).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&world.target)
        .unwrap();
    for name in [MARKER, "CACHEDIR.TAG"] {
        std::fs::copy(aside.join(name), world.target.join(name)).unwrap();
    }
    let now = identity(&world.target).unwrap();
    assert_eq!(
        (now.device, &now.volume),
        (receipt.identity.device, &receipt.identity.volume)
    );
    assert_ne!(now.inode, receipt.identity.inode);
    assert!(!same(&world.home, &receipt));
    let index = receipts(&world.home);
    let refused = create_checked(&world.home, &world.target, &world.another_owner())
        .expect_err("a swapped folder admits no producer");
    assert_ne!(refused.kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(receipts(&world.home), index, "nothing is written");
    assert_eq!(
        retention::kept_by(&world.target),
        Some(world.target.as_path())
    );
    assert!(
        !roots(&world.home)
            .iter()
            .any(|cache| cache.root == world.target)
    );
}

/// (d) A RECEIPT AN OLDER ATPKG WROTE (NO VOLUME UUID) IS WITNESSED WHILE THIS BOOT NUMBERS ITS
/// VOLUME AS IT WAS RECORDED: it reads back as written; its owner's next build is admitted,
/// and the receipt and its marker gain the volume's UUID, the device number and inode they
/// recorded kept; so once a restart renumbers the volume the cache still binds, and the next
/// build is admitted. NEGATIVE CONTROL: without the witness, the renumbered receipt is refused
/// as [`renumbered`].
#[cfg(target_os = "macos")]
#[test]
fn a_receipt_an_older_atpkg_wrote_is_witnessed_while_its_volume_is_numbered_alike() {
    let world = World::new();
    world.build();
    let now = identity(&world.target).unwrap();
    let legacy = Identity::unrecorded(now.device, now.inode);
    recorded_as(&world, &legacy);
    assert!(!String::from_utf8_lossy(&std::fs::read(world.path()).unwrap()).contains("volume"));
    assert!(same(&world.home, &read(&world.path()).unwrap()));
    assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
    assert_eq!(read(&world.path()).unwrap().identity, now, "the receipt");
    let marker = retention::receipt_at(creation::pin(&world.target).unwrap()).unwrap();
    assert_eq!(marker.identity, now, "the marker");
    recorded_as(&world, &an_earlier_boots(&now));
    assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
    assert!(create_checked(&world.home, &world.target, &world.another_owner()).unwrap());
}

/// (d) A RECEIPT AN OLDER ATPKG WROTE ON A VOLUME A RESTART HAS RENUMBERED SINCE STAYS REFUSED,
/// AND SAYS WHAT TO DO: its folder stands at its path with the inode it recorded and the
/// marker that binds it, but nothing it recorded proves the volume the one it was written on,
/// so it is never witnessed: the producer is refused naming the cache, both device numbers
/// and the remedy; nothing is written, and the ordinary cleaners keep the folder. Once the
/// person removes it, as the words say, the next build makes it again.
#[test]
fn a_receipt_an_older_atpkg_wrote_on_a_renumbered_volume_stays_refused_and_says_what_to_do() {
    let world = World::new();
    world.build();
    let now = identity(&world.target).unwrap();
    let legacy = Identity::unrecorded(now.device.wrapping_add(1), now.inode);
    recorded_as(&world, &legacy);
    let receipt = read(&world.path()).unwrap();
    assert_eq!(renumbered(&world.home, &receipt), Some(now.device));
    let index = receipts(&world.home);
    for owner in [world.owner.clone(), world.another_owner()] {
        let refused = create_checked(&world.home, &world.target, &owner)
            .expect_err("a renumbered receipt that keeps no UUID admits no producer");
        let words = refused.to_string();
        assert_eq!(words, renumbered_words(&receipt, now.device));
        for part in [
            format!(
                "{} was recorded on device {}",
                world.target.display(),
                legacy.device
            ),
            format!("reads as device {} now", now.device),
            format!("remove {}", world.target.display()),
            "or build into another target directory".to_owned(),
        ] {
            assert!(words.contains(&part), "{words}: {part}");
        }
    }
    assert_eq!(
        receipts(&world.home),
        index,
        "nothing is written or witnessed"
    );
    assert_eq!(
        retention::kept_by(&world.target),
        Some(world.target.as_path())
    );
    std::fs::remove_dir_all(&world.target).unwrap();
    assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
    assert!(same(&world.home, &read(&world.path()).unwrap()));
}

/// THE RECLAIM PASS WITNESSES A RECEIPT NO BUILD TOUCHED: a receipt an older atpkg wrote, its
/// volume numbered as recorded, gains its UUID in a pass that removes nothing (its owner
/// stands and it is fresh), so the cache binds after a restart though no build ran before it.
#[cfg(target_os = "macos")]
#[test]
fn the_reclaim_pass_witnesses_a_receipt_no_build_touched() {
    let world = World::new();
    world.build();
    let now = identity(&world.target).unwrap();
    recorded_as(&world, &Identity::unrecorded(now.device, now.inode));
    let removed = reclaim(
        &world.home,
        SystemTime::now(),
        super::super::ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
    );
    assert!(removed.is_empty(), "{removed:?}");
    assert_eq!(read(&world.path()).unwrap().identity, now);
    recorded_as(&world, &an_earlier_boots(&now));
    assert!(create_checked(&world.home, &world.target, &world.another_owner()).unwrap());
}

/// A RETIREMENT STAGED IN AN EARLIER BOOT IS FINISHED, AND ONE WHOSE STAGE IS ANOTHER VOLUME'S
/// STAYS: the receipt, its marker and its pending stage recorded with the volume numbered
/// otherwise (the same UUIDs and inodes), the next pass removes the staged cache and its
/// stage by their recorded identities, read through the folders held. NEGATIVE CONTROL: the
/// stage recorded with another volume's UUID is no stage of this receipt's, and stays whole.
#[cfg(target_os = "macos")]
#[test]
fn a_retirement_staged_in_an_earlier_boot_finishes_and_another_volumes_stays() {
    for elsewhere in [false, true] {
        let world = World::new();
        world.build();
        let root = an_earlier_boots(&identity(&world.target).unwrap());
        recorded_as(&world, &root);
        let stage = world.home.join("interrupted-retirement.noindex");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&stage)
            .unwrap();
        let mut staged = an_earlier_boots(&identity(&stage).unwrap());
        if elsewhere {
            staged.volume.as_mut().unwrap()[0] ^= 0xff;
        }
        let mut receipt = read(&world.path()).unwrap();
        receipt.pending = Some(Pending {
            directory: stage.clone(),
            identity: staged,
            authorized: false,
        });
        write(&world.path(), &receipt).unwrap();
        std::fs::rename(&world.target, stage.join(PAYLOAD)).unwrap();
        assert_eq!(
            pending_owned(&world.home, &receipt, receipt.pending.as_ref().unwrap()),
            !elsewhere
        );
        let removed = reclaim(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        );
        if elsewhere {
            assert!(removed.is_empty(), "{removed:?}");
            assert!(stage.join(PAYLOAD).join("debug/deps/keep").is_file());
            assert!(read(&world.path()).unwrap().pending.is_some());
        } else {
            assert_eq!(removed.len(), 1, "{removed:?}");
            assert!(!stage.exists());
        }
    }
}

/// A LINK TO A CACHE AN OLDER ATPKG RECORDED ON A VOLUME RENUMBERED SINCE IS REFUSED IN ITS
/// OWN WORDS ([`LinkRefusal::Renumbered`]), with nothing written; NEGATIVE CONTROL: with the
/// device number its volume has now, the same link is owned.
#[test]
fn a_link_to_a_cache_recorded_on_a_renumbered_volume_says_so() {
    for renumbered in [true, false] {
        let world = World::new();
        world.build();
        let now = identity(&world.target).unwrap();
        let device = now.device.wrapping_add(u64::from(renumbered));
        recorded_as(&world, &Identity::unrecorded(device, now.inode));
        let link = world.home.join("target");
        std::os::unix::fs::symlink("outputs.noindex", &link).unwrap();
        let other = world.another_owner();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&other),
            env: &|_| None,
            read: &|_| Ok(None),
            read_manifest: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let index = receipts(&world.home);
        let observed = observe(
            &build_into(&other, &link),
            super::super::Tool::Targo,
            &world.home,
            &machine,
        )
        .unwrap();
        if renumbered {
            assert_eq!(observed, Observation::Refused(LinkRefusal::Renumbered));
            assert_eq!(receipts(&world.home), index);
        } else {
            assert_eq!(observed, Observation::Owned);
        }
    }
}

#[test]
fn compatibility_and_proof_manifest_readings_require_canonical_owner_agreement() {
    use super::super::{Machine, Tool};
    for agree in [false, true] {
        for tool in [Tool::CargoCompat, Tool::Cargo, Tool::Targo] {
            let world = World::new();
            let member = world.owner.join("member");
            std::fs::create_dir(&member).unwrap();
            let standalone = "[package]\nname='member'\nversion='0.0.0'\n[workspace]\n";
            std::fs::write(member.join("Targo.toml"), standalone).unwrap();
            std::fs::write(
                world.owner.join("Cargo.toml"),
                "[workspace]\nmembers=['member']\n",
            )
            .unwrap();
            std::fs::write(
                member.join("Cargo.toml"),
                if agree {
                    standalone
                } else {
                    "[package]\nname='member'\nversion='0.0.0'\nworkspace='..'\n"
                },
            )
            .unwrap();
            let intermediate = world.home.join("intermediate.noindex");
            let mut args = vec![
                "build".into(),
                format!("--target-dir={}", world.target.display()),
            ];
            if tool == Tool::Targo {
                args.splice(0..1, ["trust".into(), "check".into()]);
            }
            if tool == Tool::CargoCompat {
                args.extend([
                    "--config".into(),
                    format!("build.build-dir='{}'", intermediate.display()),
                ]);
            }
            let machine = Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&member),
                env: &|_| None,
                read: &super::super::read_config,
                read_manifest: &super::super::read_manifest,
                writable: &|_| Some(true),
            };
            let result = observe_for_producer(&args, tool, &world.home, &machine, true);
            if !agree && tool != Tool::Cargo {
                assert!(result.is_err());
                assert!(!world.target.exists());
                assert!(!intermediate.exists());
                assert!(roots(&world.home).is_empty());
            } else {
                assert!(result.unwrap(), "{tool:?}, agree={agree}");
                let receipts = roots(&world.home);
                assert_eq!(
                    receipts.len(),
                    if tool == Tool::CargoCompat { 2 } else { 1 }
                );
                let owner = if agree { &member } else { &world.owner };
                let outputs = if tool == Tool::CargoCompat {
                    vec![&world.target, &intermediate]
                } else {
                    vec![&world.target]
                };
                for root in outputs {
                    let receipt = receipts
                        .iter()
                        .find(|receipt| receipt.root.as_path() == root.as_path())
                        .unwrap();
                    assert_eq!(receipt.owners, vec![owner.clone()]);
                    assert!(!receipt.owners_unknown);
                }
            }
        }
    }
}

#[test]
fn native_linked_manifest_never_confers_a_managed_cargo_ancestors_custody() {
    let world = World::new();
    let member = world.owner.join("member");
    let foreign = world.home.parent().unwrap().join("foreign.toml");
    std::fs::create_dir(&member).unwrap();
    std::fs::write(
        &foreign,
        "[package]\nname='foreign'\nversion='0.0.0'\n[workspace]\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&foreign, member.join("Targo.toml")).unwrap();
    let args = vec![
        "--unverified".into(),
        "build".into(),
        format!("--target-dir={}", world.target.display()),
    ];
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&member),
        env: &|_| None,
        read: &super::super::read_config,
        read_manifest: &super::super::read_manifest,
        writable: &|_| Some(true),
    };
    // Targo's package is the folder its manifest is named in, wherever the link names: a
    // reader that follows the link finds that folder its own root, which owes custody and
    // owns the target, never the Cargo ancestor beside it.
    assert!(requires_managed_explicit(
        &args,
        super::super::Tool::Targo,
        &world.home,
        &machine
    ));
    assert!(
        observe_for_producer(
            &args,
            super::super::Tool::Targo,
            &world.home,
            &machine,
            true
        )
        .unwrap()
    );
    let registered = roots(&world.home);
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].root, world.target);
    assert_eq!(registered[0].owners, vec![member.clone()]);
}

#[cfg(target_os = "macos")]
#[test]
fn native_manifest_cannot_acquire_a_cargo_workspace_membership_by_directory() {
    for pointer in [false, true] {
        for cargo_member in [false, true] {
            let world = World::new();
            let member = world.owner.join("member");
            std::fs::create_dir(&member).unwrap();
            std::fs::write(
                world.owner.join("Cargo.toml"),
                "[workspace]\nmembers=['member']\nresolver='3'\n",
            )
            .unwrap();
            let mut package =
                "[package]\nname='member'\nversion='0.0.0'\nedition='2024'\n".to_owned();
            if pointer {
                package.push_str("workspace='..'\n");
            }
            std::fs::write(member.join("Targo.toml"), &package).unwrap();
            if cargo_member {
                std::fs::write(member.join("Cargo.toml"), &package).unwrap();
            }
            let intermediate = world.home.join("intermediate.noindex");
            let args = vec![
                "--unverified".into(),
                "build".into(),
                format!("--target-dir={}", world.target.display()),
                "--config".into(),
                format!("build.build-dir='{}'", intermediate.display()),
            ];
            let out = through_placed_front(
                &world,
                &member,
                Path::new("/bin/echo"),
                &args,
                None,
                "-",
                None,
            );
            assert_eq!(
                out.status.code(),
                Some(75),
                "pointer={pointer}, cargo_member={cargo_member}: {out:?}"
            );
            assert!(!String::from_utf8_lossy(&out.stdout).contains("--target-dir="));
            assert!(!world.target.exists());
            assert!(!intermediate.exists());
            assert!(roots(&world.home).is_empty());
        }
    }
}

/// A `MIRI` LINE HOLDS THE ROOT ITS TARGET LIES INSIDE OF: `cargo-miri` builds into
/// `<base>/miri`, so a root recorded at `<base>` (here `CARGO_TARGET_DIR`'s, for another
/// workspace) is held in custody, its owners unknown and never retired once that workspace goes,
/// where the call selects no output (a package outside HOME) and where its package's root is one
/// atpkg records no owner for (HOME's own). NEGATIVE CONTROL: the same calls without `miri`, whose
/// target is `<base>` itself, hold it the same way.
#[test]
fn a_miri_line_holds_the_root_its_target_lies_inside_of() {
    use super::super::Tool;
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let package = "[package]\nname = \"p\"\nversion = \"0.1.0\"\n";
    let unknown = |world: &World| {
        roots(&world.home)
            .into_iter()
            .find(|recorded| recorded.root == world.target)
            .and_then(|recorded| super::read(&recorded.receipt_path))
            .expect("the world's root is recorded")
            .owners_unknown
    };
    for (row, at_home, verb) in [
        ("outside", false, "miri"),
        ("outside, built", false, "build"),
        ("home", true, "miri"),
        ("home, built", true, "build"),
    ] {
        let world = World::new();
        world.build();
        assert!(!unknown(&world), "{row}");
        let folder = if at_home {
            world.home.clone()
        } else {
            let outside = world.home.parent().unwrap().join("outside");
            std::fs::create_dir(&outside).unwrap();
            outside
        };
        std::fs::write(folder.join("Cargo.toml"), package).unwrap();
        let manifest = folder.join("Cargo.toml").display().to_string();
        let mut args = vec!["--unverified".to_owned(), verb.to_owned()];
        if verb == "miri" {
            args.push("run".to_owned());
        }
        args.extend(["--manifest-path".to_owned(), manifest]);
        let env = |name: &str| match name {
            "CARGO_TARGET_DIR" => Some(world.target.clone().into_os_string()),
            "MIRI_SYSROOT" => Some("/private/tmp/no-sysroot".into()),
            _ => None,
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let verdict = observe_for_producer(&args, Tool::Targo, &world.home, &machine, true);
        assert_eq!(verdict.ok(), Some(false), "{row}: {args:?}");
        assert!(
            unknown(&world),
            "{row}: {args:?} left the root it builds inside of with its owners known"
        );
    }
}

/// A `-C` TARGO REFUSES FOR WANT OF ITS GATE, NAMING AN OUTPUT UNDER HOME, IS REFUSED NAMING THE
/// GATE: such a call is judged as the same call honoured, since a gate the reader misread would
/// let a build Targo honours through unrecorded, and the refusal says what Targo wants rather
/// than send the person after fresh output directories. NEGATIVE CONTROL: the same call behind
/// its gate is owned.
#[test]
fn an_ungated_directory_that_names_an_output_is_refused_naming_the_gate() {
    use super::super::Tool;
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let world = World::new();
    let notes = world.home.join("notes");
    std::fs::create_dir(&notes).unwrap();
    let owner = world.owner.display().to_string();
    let env = |_: &str| None;
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&notes),
        env: &env,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let words = |words: &[&str]| -> Vec<String> { words.iter().map(|w| (*w).to_owned()).collect() };
    let ungated = words(&["-C", &owner, "--unverified", "build", "--target-dir", "out"]);
    let error = observe_for_producer(&ungated, Tool::Targo, &world.home, &machine, true)
        .expect_err("an ungated -C naming an output under HOME is refused");
    assert!(
        error.to_string().contains("-Z unstable-options"),
        "the refusal names the gate: {error}"
    );
    assert!(roots(&world.home).is_empty());
    let gated = words(&[
        "-C",
        &owner,
        "-Zunstable-options",
        "--unverified",
        "build",
        "--target-dir",
        "out",
    ]);
    assert_eq!(
        observe_for_producer(&gated, Tool::Targo, &world.home, &machine, true).ok(),
        Some(true)
    );
}

/// THE FRONT RECORDS THE TARGET OF THE `--config` LIST TARGO MERGES: clap keeps a verb's own
/// `--config` list in place of the one before it, so `xb --config net.offline=true` over
/// `xb = --config build.target-dir=A build` builds into the config walk's target, never `A`, and
/// a typed `--config` the hop moves past an alias's `--` is its program's; the front owns the
/// config walk's target there and records no `A`. A help call through such an alias builds
/// nothing and is not refused. NEGATIVE CONTROLS: the same alias with nothing after its name, and
/// one whose `--config` follows its verb, record `A`.
#[test]
fn the_front_records_the_target_of_the_config_list_targo_merges() {
    use super::super::Tool;
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let before = "\"--config\", \"build.target-dir='A'\", \"build\"";
    let after = "\"build\", \"--config\", \"build.target-dir='A'\"";
    let program = "\"run\", \"--package\", \"xtask\", \"--\"";
    for (row, alias, typed, recorded) in [
        (
            "before its verb",
            before,
            "--unverified xb --config net.offline=true",
            false,
        ),
        ("before its verb, alone", before, "--unverified xb", true),
        (
            "after its verb",
            after,
            "--unverified xb --config net.offline=true",
            true,
        ),
        (
            "moved past its --",
            program,
            "--config build.target-dir='A' --unverified xb demo --config net.offline=true",
            false,
        ),
    ] {
        let world = World::in_directory(
            aterm_tempfile::Builder::new()
                .prefix("alias-A-")
                .tempdir()
                .unwrap(),
        );
        let named = world.home.join("A");
        let named_config = format!("build.target-dir='{}'", named.display());
        let alias = alias.replace("build.target-dir='A'", &named_config);
        std::fs::create_dir_all(world.owner.join(".cargo")).unwrap();
        std::fs::write(
            world.owner.join(".cargo/config.toml"),
            format!(
                "[build]\ntarget-dir = \"{}\"\n[alias]\nxb = [{}]\n",
                world.target.display(),
                alias,
            ),
        )
        .unwrap();
        let env = |_: &str| None;
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args: Vec<String> = typed
            .replace("build.target-dir='A'", &named_config)
            .split(' ')
            .map(str::to_owned)
            .collect();
        let verdict = observe_for_producer(&args, Tool::Targo, &world.home, &machine, true);
        let made: Vec<_> = roots(&world.home)
            .into_iter()
            .map(|made| made.root)
            .collect();
        let expected = if recorded { &named } else { &world.target };
        assert!(
            matches!(&verdict, Ok(true)),
            "{row}: {verdict:?}; roots={made:?}"
        );
        assert_eq!(made.as_slice(), std::slice::from_ref(expected), "{row}");
    }
    // A help call through the alias builds nothing, whatever a variable names.
    let world = World::new();
    std::fs::create_dir_all(world.owner.join(".cargo")).unwrap();
    std::fs::write(
        world.owner.join(".cargo/config.toml"),
        "[alias]\nxb = [\"--config\", \"profile.dev.debug=0\", \"build\"]\n",
    )
    .unwrap();
    let env =
        |name: &str| (name == "CARGO_TARGET_DIR").then(|| world.target.clone().into_os_string());
    let machine = super::super::Machine {
        home: Some(&world.home),
        root: None,
        cwd: Some(&world.owner),
        env: &env,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let help: Vec<String> = ["xb", "--help"]
        .iter()
        .map(|word| (*word).to_owned())
        .collect();
    assert_eq!(
        observe_for_producer(&help, Tool::Targo, &world.home, &machine, true).ok(),
        Some(false)
    );
    assert!(roots(&world.home).is_empty());
}

/// A CHECKOUT WHOSE CONFIG NAMES A TARGET ONLY IN AN ALIAS'S WORDS BUILDS UNREFUSED, as a fresh
/// clone of trust-mc does (its `.cargo/config.toml` of 2026-11-15: `jobs`, a relative
/// `rustc-wrapper`, `[net]`, `[env]` inline tables, a `cfg` target's `rustflags`, and
/// `build-dev = "run --target-dir target/tools …"`): `test -p`, `check` and `fmt` name no
/// output, so the shim's front places each in the shared cache, or leaves it as typed, and admits
/// it owing no custody, making no `target`; `build-dev`, the alias run, names its target, which
/// is made and owned. The protection stands: a call still owes custody where a config's `build`
/// table names a target under HOME, and one standing unowned is refused, never adopted; and a
/// line an alias this reader cannot tell may run (Trust's `TARGO_HOME` and Targo 16387's
/// `CARGO_HOME` read apart) owes custody where a config's text spells a directory's key, as that
/// alias's own words, unread, may. NEGATIVE CONTROL: before a config was read for what its
/// `build` table names, the alias's words made every call from the checkout owe custody of a
/// target it never named, and the front refused `test`, `check` and `fmt` alike: "build outputs
/// are unmanaged or unsupported"; the untold row is admitted once its home's config spells no
/// directory's key.
#[test]
fn a_checkout_whose_alias_names_a_target_builds_unrefused() {
    use super::super::Tool;
    const TRUST_MC: &str = "[build]\njobs = 12\nrustc-wrapper = \"scripts/trustc-private-wrapper.sh\"\n\n\
                            [net]\ngit-fetch-with-cli = true\n\n\
                            [alias]\n# Build trust-mc with development configuration.\n\
                            build-dev = \"run --target-dir target/tools -p build-trust-mc -- build-dev\"\n\
                            bundle = \"run -p build-trust-mc -- bundle\"\n\n\
                            [env]\nRAYON_NUM_THREADS = { value = \"2\", force = true }\n\
                            RUST_TEST_THREADS = { value = \"2\", force = true }\n\
                            [target.'cfg(all())']\nrustflags = [ # Global lints.\n\
                            \x20   \"-Aclippy::expect_fun_call\",\n]\n";
    let words =
        |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_owned()).collect() };
    let checkout = |text: &str| {
        let world = World::new();
        std::fs::create_dir(world.owner.join(".cargo")).unwrap();
        std::fs::write(world.owner.join(".cargo/config.toml"), text).unwrap();
        world
    };
    // The shim's front: the call as the block placed it, or as typed where it placed none.
    let front = |world: &World, typed: &[&str]| {
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &super::super::read_config,
            read_manifest: &super::super::read_manifest,
            writable: &super::super::writable,
        };
        let args = words(typed);
        match super::super::placed_build_dir(&args, Tool::Targo, &machine) {
            Some((_, placed)) => {
                observe_shim_on(&placed, Tool::Targo, "config", &world.home, &machine, true)
            }
            None => observe_shim_on(&args, Tool::Targo, "-", &world.home, &machine, true),
        }
    };
    let world = checkout(TRUST_MC);
    for typed in [
        &[
            "--unverified",
            "test",
            "-p",
            "build-trust-mc",
            "--",
            "a_sysroot_library_missing_a_path_dependency_is_not_chosen",
        ][..],
        &["--unverified", "check"],
        &["fmt", "--check"],
    ] {
        assert!(!front(&world, typed).unwrap(), "{typed:?}");
    }
    assert!(roots(&world.home).is_empty());
    assert!(!world.owner.join("target").exists());
    // The alias run names its target: made and owned.
    assert!(front(&world, &["--unverified", "build-dev"]).unwrap());
    let records = roots(&world.home);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].root, world.owner.join("target/tools"));

    // A `build` table naming a target that stands unowned refuses the plain call, adopting none.
    let world = checkout(TRUST_MC);
    let stands = world.home.join("stands");
    std::fs::create_dir(&stands).unwrap();
    std::fs::write(stands.join("keep"), b"another build's output").unwrap();
    std::fs::create_dir(world.home.join(".cargo")).unwrap();
    std::fs::write(
        world.home.join(".cargo/config.toml"),
        format!("[build]\ntarget-dir = \"{}\"\n", stands.display()),
    )
    .unwrap();
    assert!(front(&world, &["--unverified", "check"]).is_err());
    assert!(roots(&world.home).is_empty());
    assert_eq!(
        std::fs::read(stands.join("keep")).unwrap(),
        b"another build's output"
    );

    // An alias this reader cannot tell, read beside a config whose text spells a directory's key.
    for (text, owes) in [(TRUST_MC, true), ("[build]\njobs = 12\n", false)] {
        let world = checkout(text);
        let twin = world.home.join("twin-home");
        std::fs::create_dir(&twin).unwrap();
        std::fs::write(twin.join("config.toml"), "[alias]\nxb = \"build\"\n").unwrap();
        let env = |name: &str| (name == "TARGO_HOME").then(|| twin.clone().into_os_string());
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &env,
            read: &super::super::read_config,
            read_manifest: &super::super::read_manifest,
            writable: &|_| Some(true),
        };
        let typed = words(&["--unverified", "xb"]);
        assert_eq!(
            requires_managed_explicit(&typed, Tool::Targo, &world.home, &machine),
            owes,
            "{text:?}"
        );
        assert_eq!(
            observe_for_producer(&typed, Tool::Targo, &world.home, &machine, true).is_err(),
            owes,
            "{text:?}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn explicit_intermediate_roots_require_noindex_before_creation_and_are_marked_on_reuse() {
    use aterm_uds::backup::Backups;
    for source in ["cargo-env", "targo-env", "cli", "config"] {
        for protected in [false, true] {
            let world = World::in_directory(
                aterm_tempfile::Builder::new()
                    .prefix("explicit-build-")
                    .heard()
                    .tempdir_in("/tmp")
                    .unwrap(),
            );
            let target = world.home.join("final-output");
            let intermediate = world.home.join(if protected {
                ".ubd/intermediates.noindex"
            } else {
                ".ubd/intermediates"
            });
            let mut args = vec![
                "--unverified".into(),
                "build".into(),
                "--target-dir".into(),
                target.display().to_string(),
            ];
            if source == "cli" {
                args.extend([
                    "--config".into(),
                    format!("build.build-dir='{}'", intermediate.display()),
                ]);
            } else if source == "config" {
                std::fs::create_dir(world.owner.join(".cargo")).unwrap();
                std::fs::write(
                    world.owner.join(".cargo/config.toml"),
                    format!("[build]\nbuild-dir='{}'\n", intermediate.display()),
                )
                .unwrap();
            }
            let environment = |name: &str| {
                ((source == "cargo-env" && name == super::super::BUILD_DIR_ENV)
                    || (source == "targo-env" && name == super::super::BUILD_DIR_TWIN))
                    .then(|| intermediate.as_os_str().to_owned())
            };
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&world.owner),
                env: &environment,
                read: &super::super::read_config,
                read_manifest: &super::super::read_manifest,
                writable: &|_| Some(true),
            };
            let verdict = observe_for_producer(
                &args,
                super::super::Tool::Targo,
                &world.home,
                &machine,
                true,
            );
            if !protected {
                let error = verdict.unwrap_err();
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::InvalidInput,
                    "{source}: {error}"
                );
                assert!(error.to_string().contains(".noindex"), "{source}: {error}");
                assert!(!intermediate.exists() && !target.exists(), "{source}");
                assert!(roots(&world.home).is_empty(), "{source}");
                continue;
            }
            assert!(verdict.unwrap(), "{source}");
            assert!(Backups::SYSTEM.excluded(&std::fs::File::open(&intermediate).unwrap()));
            assert!(!Backups::SYSTEM.excluded(&std::fs::File::open(&target).unwrap()));
            std::fs::write(intermediate.join("kept-object"), b"existing output").unwrap();
            assert!(
                observe_for_producer(
                    &args,
                    super::super::Tool::Targo,
                    &world.home,
                    &machine,
                    true,
                )
                .unwrap(),
                "{source}"
            );
            assert!(Backups::SYSTEM.excluded(&std::fs::File::open(&intermediate).unwrap()));
            assert_eq!(
                std::fs::read(intermediate.join("kept-object")).unwrap(),
                b"existing output"
            );
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn an_existing_explicit_build_folder_is_marked_without_changing_its_contents() {
    use aterm_uds::backup::Backups;
    let directory = aterm_tempfile::Builder::new()
        .heard()
        .tempdir_in("/tmp")
        .unwrap();
    let build = directory
        .path()
        .canonicalize()
        .unwrap()
        .join(".ubd/build.noindex");
    std::fs::create_dir_all(&build).unwrap();
    std::fs::write(build.join("kept-object"), b"existing output").unwrap();
    assert!(!Backups::SYSTEM.excluded(&std::fs::File::open(&build).unwrap()));
    for _ in 0..2 {
        prepare_explicit_build_dir(&build).unwrap();
        assert!(Backups::SYSTEM.excluded(&std::fs::File::open(&build).unwrap()));
        assert_eq!(
            std::fs::read(build.join("kept-object")).unwrap(),
            b"existing output"
        );
    }
}

#[test]
fn explicit_build_templates_need_a_known_unindexed_boundary_without_parent_traversal() {
    let world = World::in_directory(aterm_tempfile::Builder::new().heard().tempdir().unwrap());
    let cargo_home = world.home.join("cargo-home");
    for (value, expected) in [
        (
            "{workspace-root}/build.noindex/{workspace-path-hash}",
            world.owner.join("build.noindex"),
        ),
        (
            "{cargo-cache-home}/build.noindex/{workspace-path-hash}/objects",
            cargo_home.join("build.noindex"),
        ),
    ] {
        assert_eq!(
            explicit_build_root(Path::new(value), Some(&world.owner), Some(&cargo_home)).unwrap(),
            expected,
        );
        assert!(!expected.exists(), "validation creates nothing");
    }
    for value in [
        "{workspace-root}/indexed-{workspace-path-hash}.noindex",
        "{workspace-root}/build.noindex/{workspace-path-hash}/../../escape",
        "{workspace-root}/build.noindex/{unknown}",
        "{workspace-root}/build.noindex/}",
    ] {
        assert!(
            explicit_build_root(Path::new(value), Some(&world.owner), Some(&cargo_home)).is_err(),
            "{value}",
        );
    }
    assert!(explicit_build_root(Path::new("{workspace-root}/build.noindex"), None, None).is_err());
    let unknown_source = world.home.join("repo.noindex");
    assert!(
        explicit_build_root(
            &unknown_source.join("{workspace-path-hash}"),
            None,
            Some(&cargo_home),
        )
        .is_err()
    );
    assert!(
        !unknown_source.exists(),
        "an unknown source boundary is never created or claimed"
    );
    use std::os::unix::ffi::OsStrExt as _;
    let literal = world
        .home
        .join(std::ffi::OsStr::from_bytes(b"build-\xff.noindex"));
    assert_eq!(explicit_build_root(&literal, None, None).unwrap(), literal);
    assert!(!literal.exists());
}

#[cfg(target_os = "macos")]
#[test]
fn foreign_workspace_template_intermediates_are_protected_or_refused_before_creation() {
    use aterm_uds::backup::Backups;
    for (workspace_name, plain, protected_value) in [
        (
            "foreign-source",
            "{workspace-root}/build-{workspace-path-hash}.noindex",
            "{workspace-root}/build.noindex/{workspace-path-hash}",
        ),
        (
            "repo.noindex",
            "{workspace-path-hash}",
            "build.noindex/{workspace-path-hash}",
        ),
    ] {
        for source in ["environment", "cli"] {
            for protected in [false, true] {
                let world = World::in_directory(
                    aterm_tempfile::Builder::new()
                        .heard()
                        .tempdir_in("/tmp")
                        .unwrap(),
                );
                let foreign = world.home.parent().unwrap().join(workspace_name);
                std::fs::create_dir(&foreign).unwrap();
                std::fs::write(
                    foreign.join("Cargo.toml"),
                    "[package]\nname='foreign'\nversion='0.0.0'\n[workspace]\n",
                )
                .unwrap();
                let value = if protected { protected_value } else { plain };
                let environment = |name: &str| {
                    (source == "environment" && name == super::super::BUILD_DIR_ENV)
                        .then(|| std::ffi::OsString::from(value))
                };
                let mut args = vec!["--unverified".into(), "build".into()];
                if source == "cli" {
                    args.extend(["--config".into(), format!("build.build-dir='{value}'")]);
                }
                let machine = super::super::Machine {
                    home: Some(&world.home),
                    root: None,
                    cwd: Some(&foreign),
                    env: &environment,
                    read: &super::super::read_config,
                    read_manifest: &super::super::read_manifest,
                    writable: &|_| Some(true),
                };
                let verdict = observe_for_producer(
                    &args,
                    super::super::Tool::Targo,
                    &world.home,
                    &machine,
                    true,
                );
                let build = foreign.join("build.noindex");
                if protected {
                    assert!(
                        !verdict.unwrap(),
                        "no custody is granted outside the managed home"
                    );
                    assert!(Backups::SYSTEM.excluded(&std::fs::File::open(&build).unwrap()));
                    assert_eq!(
                        std::fs::read_dir(&build).unwrap().count(),
                        0,
                        "the hash stays Cargo's to compute"
                    );
                } else {
                    assert_eq!(
                        verdict.unwrap_err().kind(),
                        std::io::ErrorKind::InvalidInput
                    );
                    assert!(!build.exists());
                    assert_eq!(
                        std::fs::read_dir(&foreign).unwrap().count(),
                        1,
                        "no output was created"
                    );
                }
                assert!(
                    !Backups::SYSTEM.excluded(&std::fs::File::open(&foreign).unwrap()),
                    "source workspace stays in backups: {workspace_name} {source}"
                );
                assert!(!foreign.join("target").exists());
                assert!(roots(&world.home).is_empty());
            }
        }
    }
}
