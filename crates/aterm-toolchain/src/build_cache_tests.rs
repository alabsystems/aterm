// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

impl FinalTarget {
    fn explicit_path(self) -> PathBuf {
        match self {
            Self::Explicit(path) => path,
            Self::WorkspaceDefault => panic!("expected an explicitly selected final target"),
        }
    }
}

#[test]
fn only_an_external_frontend_loses_the_config_words() {
    let words = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    for line in [
        "tippy",
        "clippy -p demo",
        "--unverified tippy",
        "+trust --config build.jobs=1 clippy",
        "--color always tippy",
        // A short option's value left to the next word is no subcommand.
        "-vZ unstable-options clippy",
        "-Z tippy clippy",
    ] {
        assert!(loses_config(&words(line), Tool::Cargo), "{line}");
    }
    for line in [
        "",
        "--unverified",
        "--unverified build",
        "--config build.jobs=1 test -q",
        "+trust --target-dir tippy check",
        "-- tippy",
        "fix",
        "clean",
        "--unverified verify",
        "an-alias",
        "-qC tippy build",
    ] {
        assert!(!loses_config(&words(line), Tool::Cargo), "{line}");
    }
    assert!(loses_config(&words("-p demo"), Tool::Tippy));
}

use super::*;

fn none_set(_: &str) -> Option<OsString> {
    None
}

fn none_told(_: &[String]) -> Option<PathBuf> {
    None
}

/// A [`Miri`] that reads no variable and tells no metadata target.
const NO_MIRI: Miri<'static> = Miri {
    env: &none_set,
    metadata_target: &none_told,
};
#[test]
fn unreadable_initial_manifests_do_not_select_a_compatibility_file_or_parent() {
    for (name, directory) in [
        ("Cargo.toml", true),
        ("Targo.toml", true),
        ("Cargo.toml", false),
        ("Targo.toml", false),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path();
        let parent = home.join("source");
        let child = parent.join("member");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(parent.join("Cargo.toml"), "[workspace]\n").unwrap();
        if name == "Targo.toml" {
            std::fs::write(child.join("Cargo.toml"), "[workspace]\n").unwrap();
        }
        let manifest = child.join(name);
        if directory {
            std::fs::create_dir(&manifest).unwrap();
        } else {
            std::fs::write(&manifest, [0xff]).unwrap();
        }
        // Configuration discovery ignores non-files, but initial manifest discovery
        // stops on the same existing path the driver selects before trying to read it.
        #[cfg(unix)]
        if directory {
            assert_eq!(read_config(&manifest), Ok(None));
        }
        let machine = Machine {
            home: Some(home),
            root: None,
            cwd: Some(&child),
            env: &|_| None,
            read: &read_config,
            read_manifest: &read_manifest,
            writable: &|_| Some(true),
        };
        assert_eq!(
            initial_manifest(&machine, true),
            InitialManifest::Unreadable(manifest),
            "{name}, directory={directory}"
        );
    }
}

#[test]
fn native_manifest_discovery_keeps_directory_priority_and_explicit_overrides() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let parent = home.join("workspace");
    let member = parent.join("member");
    for shape in [
        "native-only",
        "native-preferred",
        "nearest-cargo",
        "preferred-unreadable",
        "explicit-cargo",
    ] {
        let read = |path: &Path| {
            if path == member.join("Targo.toml") {
                return match shape {
                    "nearest-cargo" => Ok(None),
                    "preferred-unreadable" | "explicit-cargo" => Err(()),
                    _ => Ok(Some("[workspace]".into())),
                };
            }
            Ok((path == parent.join("Targo.toml")
                || path == parent.join("Cargo.toml")
                || (shape != "native-only" && path == member.join("Cargo.toml")))
            .then(|| "[workspace]".into()))
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&member),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let mut args = vec![
            "--unverified".into(),
            "build".into(),
            "--target-dir".into(),
            home.join("output").display().to_string(),
        ];
        if shape == "explicit-cargo" {
            args.extend([
                "--manifest-path".into(),
                member.join("Cargo.toml").display().to_string(),
            ]);
        }
        let selected = selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None);
        let expected = match shape {
            "preferred-unreadable" => None,
            "nearest-cargo" | "explicit-cargo" => Some(member.join("Cargo.toml")),
            _ => Some(member.join("Targo.toml")),
        };
        assert_eq!(
            selected.map(|cache| cache.manifests),
            expected.map(|path| vec![path]),
            "{shape}"
        );
    }
}

#[test]
fn native_manifest_discovery_reaches_a_distant_compatibility_manifest() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let owner = home.join("workspace");
    let mut cwd = owner.clone();
    for _ in 0..40 {
        cwd.push("nested");
    }
    let read = |path: &Path| Ok((path == owner.join("Cargo.toml")).then(|| "[workspace]".into()));
    let machine = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let args = vec![
        "--unverified".into(),
        "build".into(),
        "--target-dir".into(),
        home.join("output").display().to_string(),
    ];
    let selected = selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None).unwrap();
    assert_eq!(selected.manifests, vec![owner.join("Cargo.toml")]);
}

#[test]
fn implicit_manifest_selection_distinguishes_absence_from_read_errors() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let parent = home.join("workspace");
    let cwd = parent.join("member");
    for tool in [Tool::Targo, Tool::CargoCompat, Tool::Cargo] {
        for shape in ["readable", "absent", "unreadable"] {
            let read = |path: &Path| {
                if path == cwd.join("Cargo.toml") {
                    return match shape {
                        "readable" => Ok(Some("[workspace]".into())),
                        "absent" => Ok(None),
                        _ => Err(()),
                    };
                }
                Ok((path == parent.join("Cargo.toml")).then(|| "[workspace]".into()))
            };
            let machine = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &|_| None,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let args = vec![
                "--unverified".into(),
                "build".into(),
                "--target-dir".into(),
                home.join("output").display().to_string(),
            ];
            let selected = selected_outputs(&args, tool, &machine, &|_| UserAlias::None);
            let expected = match shape {
                "readable" => Some(cwd.join("Cargo.toml")),
                "absent" => Some(parent.join("Cargo.toml")),
                _ => None,
            };
            assert_eq!(
                selected.map(|selection| selection.manifests),
                expected.map(|path| vec![path]),
                "{tool:?}: {shape}"
            );
        }
    }
}

#[test]
fn explicit_outputs_keep_independent_winners_and_definition_bases() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let cwd = home.join("project");
    let file = cwd.join("settings/root.toml");
    let child = cwd.join("settings/nested/child.toml");
    let read = |path: &Path| {
        Ok(if path == cwd.join("Cargo.toml") {
            Some("[workspace]".into())
        } else if path == file {
            Some("include=['nested/child.toml']\nbuild.target-dir='final'".into())
        } else if path == child {
            Some("build.target-dir='overridden'\nbuild.build-dir='intermediate'".into())
        } else {
            None
        })
    };
    for tool in [Tool::Targo, Tool::CargoCompat] {
        for (extra, target, intermediate) in [
            (vec![], cwd.join("final"), cwd.join("settings/intermediate")),
            (
                vec!["--target-dir", "command"],
                cwd.join("command"),
                cwd.join("settings/intermediate"),
            ),
            (
                vec!["--config", "build.build-dir='later'"],
                cwd.join("final"),
                cwd.join("later"),
            ),
        ] {
            let env = |name: &str| match name {
                BUILD_DIR_ENV => Some("environment".into()),
                _ => None,
            };
            let machine = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &env,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let mut args = vec![
                "--unverified".into(),
                "build".into(),
                "--config".into(),
                "settings/root.toml".into(),
            ];
            args.extend(extra.into_iter().map(str::to_owned));
            let selected = selected_outputs(&args, tool, &machine, &|_| UserAlias::None).unwrap();
            assert_eq!(selected.target.explicit_path(), target);
            assert_eq!(selected.intermediates, Some(intermediate));
            assert_eq!(selected.manifests, vec![cwd.join("Cargo.toml")]);
        }
    }
}

#[test]
fn explicit_intermediates_require_a_known_path_and_frontend() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let cwd = home.join("project");
    let read = |p: &Path| Ok((p == cwd.join("Cargo.toml")).then(|| "[workspace]".into()));
    for tool in [Tool::Targo, Tool::CargoCompat, Tool::Cargo] {
        for raw in [
            "intermediate",
            "",
            "{workspace-root}/build",
            "../outside",
            "cache/{unknown}",
        ] {
            let env = |name: &str| (name == BUILD_DIR_ENV).then(|| OsString::from(raw));
            let machine = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &env,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let args = [
                "--unverified".into(),
                "build".into(),
                "--target-dir".into(),
                "final".into(),
            ];
            let selected = selected_outputs(&args, tool, &machine, &|_| UserAlias::None);
            if raw == "intermediate" && tool != Tool::Cargo {
                assert_eq!(selected.unwrap().intermediates, Some(cwd.join(raw)));
            } else {
                assert!(selected.is_none(), "{tool:?}: {raw}");
            }
        }
    }
}

#[test]
fn proof_frontend_output_selection_requires_its_own_provenance() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let cwd = home.join("project");
    let read = |p: &Path| Ok((p == cwd.join("Cargo.toml")).then(|| "[workspace]".into()));
    let machine = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    for config in [
        "build.build-dir='intermediate'",
        "build.target-dir='configured'",
    ] {
        for global in [false, true] {
            let mut args = if global {
                vec![
                    "--config".into(),
                    config.into(),
                    "trust".into(),
                    "check".into(),
                ]
            } else {
                vec![
                    "trust".into(),
                    "check".into(),
                    "--config".into(),
                    config.into(),
                ]
            };
            if config.contains("build-dir") {
                args.extend(["--target-dir".into(), "final".into()]);
            }
            assert!(
                selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None).is_none(),
                "global={global} {config}"
            );
        }
    }
    let args = [
        "trust".into(),
        "check".into(),
        "--target-dir".into(),
        "final".into(),
    ];
    let env = |name: &str| (name == BUILD_DIR_ENV).then(|| OsString::from("separate"));
    assert!(
        selected_outputs(
            &args,
            Tool::Targo,
            &Machine {
                env: &env,
                ..machine
            },
            &|_| UserAlias::None
        )
        .is_none()
    );
    let selected = selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None).unwrap();
    assert_eq!(selected.target.explicit_path(), cwd.join("final"));
    assert!(
        selected.intermediates.is_none(),
        "the known explicit proof target keeps its existing custody path"
    );
}

/// A configured target is the one every Targo reads ([`package_homes`]: the store's reads
/// `~/.cargo`, Trust's main a `TARGO_HOME` where `CARGO_HOME` is unset, and both walk `~/.cargo`
/// first as an ancestor's): `~/.cargo`'s wins over a twin home's. NEGATIVE CONTROL: a target only
/// the twin home names, which Targo 16387 would not build into, is none.
#[test]
fn a_configured_target_is_the_one_every_targo_reads() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let cwd = home.join("project");
    let twin = scratch.path().join("twin");
    let outer = home.join(".cargo/config.toml");
    let twin_config = twin.join("config.toml");
    let manifest = cwd.join("Cargo.toml");
    let args = ["--unverified".into(), "build".into()];
    // Absolute as this platform spells one: `/t/a` is not, on Windows.
    let (a, b) = (native("/t/a"), native("/t/b"));
    let (a, b) = (a.as_str(), b.as_str());
    for (in_outer, in_twin, expected) in [
        (Some(a), Some(a), Some(PathBuf::from(a))),
        (Some(a), Some(b), Some(PathBuf::from(a))),
        (Some(a), None, Some(PathBuf::from(a))),
        (None, Some(b), None),
    ] {
        let text = |target: Option<&str>| target.map(|t| format!("build.target-dir = '{t}'"));
        let read = |path: &Path| {
            Ok(if path == outer {
                text(in_outer)
            } else if path == twin_config {
                text(in_twin)
            } else if path == manifest {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let env = |key: &str| (key == "TARGO_HOME").then(|| twin.clone().into_os_string());
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        assert_eq!(
            selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None)
                .map(|c| c.target.explicit_path()),
            expected,
            "{in_outer:?} {in_twin:?}"
        );
        assert_eq!(target_hint(&machine, Tool::Cargo).unwrap(), expected);
        match &expected {
            Some(target) => assert_eq!(
                target_hint(&machine, Tool::Targo).unwrap().as_ref(),
                Some(target)
            ),
            None => assert!(
                target_hint(&machine, Tool::Targo).is_err(),
                "a secondary-only target is ambiguous"
            ),
        }
    }
}

/// THE CONFIG WALK STOPS WHERE TARGO STOPS IT: `__CARGO_TEST_ROOT`, which Targo reads in every
/// call (`paths::ancestors`), ends the walk at the folder it names, after that folder's own
/// `.cargo`, and the package home is read after it, so a farther ancestor's config is none
/// Targo loads: neither its target nor its build directory is the call's. A value that names
/// no ancestor, or is no UTF-8 text (Targo's `env::var` does not see it), stops nothing.
/// NEGATIVE CONTROL: a walk past the folder owns the farther ancestor's target where Targo
/// builds into the home's, and owes custody for a build directory Targo never reads.
#[test]
fn the_config_walk_stops_where_targo_stops_it() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let root = home.join("project");
    let cwd = root.join("member");
    let config = root.join(".cargo/config.toml");
    let outer = home.join(".cargo/config.toml");
    let manifest = root.join("Cargo.toml");
    let args = ["--unverified".into(), "build".into()];
    #[cfg_attr(not(unix), expect(unused_mut, reason = "only unix pushes a row below"))]
    let mut rows = vec![
        (None, root.join("near")),
        (Some(cwd.clone().into_os_string()), home.join("outer")),
        (Some(root.clone().into_os_string()), root.join("near")),
        (Some(OsString::from("member")), root.join("near")),
    ];
    #[cfg(unix)]
    rows.push((
        Some(std::os::unix::ffi::OsStringExt::from_vec({
            let mut bytes = cwd.clone().into_os_string().into_encoded_bytes();
            bytes.push(0xff);
            bytes
        })),
        root.join("near"),
    ));
    // Where the nearer config names none, every walk reaches the home's.
    for near in ["build.target-dir = 'near'", ""] {
        for (stop, expected) in &rows {
            let read = |path: &Path| {
                Ok(if path == config {
                    Some(near.to_owned())
                } else if path == outer {
                    Some("build.target-dir = 'outer'".into())
                } else if path == manifest {
                    Some("[workspace]".into())
                } else {
                    None
                })
            };
            let env = |key: &str| (key == "__CARGO_TEST_ROOT").then(|| stop.clone()).flatten();
            let machine = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &env,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let want = if near.is_empty() {
                home.join("outer")
            } else {
                expected.clone()
            };
            assert_eq!(
                selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None)
                    .map(|c| c.target.explicit_path()),
                Some(want.clone()),
                "{stop:?} {near:?}"
            );
            assert_eq!(
                target_hint(&machine, Tool::Cargo),
                Ok(Some(want)),
                "{stop:?} {near:?}"
            );
        }
    }
    // A build directory only a farther ancestor's config names owes no custody where the walk
    // stops before it, and does where it does not.
    for (stop, owed) in [(None, true), (Some(cwd.clone().into_os_string()), false)] {
        let read = |path: &Path| Ok((path == config).then(|| "build.build-dir = 'b'".to_owned()));
        let env = |key: &str| (key == "__CARGO_TEST_ROOT").then(|| stop.clone()).flatten();
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        assert_eq!(
            configs_unresolved(&cwd, Tool::Targo, &machine, true),
            owed,
            "{stop:?}"
        );
    }
}

/// A CONFIGURED TARGET FOLLOWS CARGO'S PRECEDENCE WITHOUT GUESSING: the nearest config's (the
/// legacy `config` over `config.toml` beside it), its includes merged as Targo merges them
/// (`load_includes`: the file's own value over a later include's, over an earlier one's; an
/// optional include where nothing stands skipped), each read as TOML 1.1, and a relative target
/// resolved against the folder above the one the defining file is in (`Definition::root`). Only
/// the winning value is judged among the scalars Targo's merge swaps (an integer outranked by a
/// string). A config Targo refuses (a missing include, a cycle or a diamond, an `include` that is
/// no list, a file with no grandparent, absolute target or not, a list, table or float its merge
/// meets, a `build` that is no table) grants none; the first four, and an include path holding a
/// control character, name both directories, so even a caller's choice is not owned for them. Nor
/// is it for a file whose includes give a winning target of no string, or a list, table or float
/// its merge meets: Targo loads the whole `build` table (`build_config`), an outranked target
/// with it, and fails the call.
/// NEGATIVE CONTROL: before includes were followed and TOML 1.1 read, each include row but the
/// two whose own target wins, and the two TOML 1.1 rows, granted none; a reader resolving against
/// the including file's root or the defining file's own folder, or keeping the last include's
/// value even where it names none, owns another folder than the one Targo writes; one that skips
/// the grandparent of an absolute target, or keeps a later string over a list, table or float,
/// owns a folder Targo never builds into. The two rows whose own target wins guard that the
/// integer an include gives is not judged.
#[test]
fn configured_final_targets_follow_cargo_precedence_without_guessing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let root = home.join("project");
    let cwd = root.join("member");
    let dot = root.join(".cargo");
    let config = dot.join("config.toml");
    let legacy = dot.join("config");
    let outer = home.join(".cargo/config.toml");
    let manifest = root.join("Cargo.toml");
    let args = ["--unverified".into(), "build".into()];
    let named = |target: &str| format!("build.target-dir = '{target}'");
    let (early, later, opt, rel) = (named("early"), named("later"), named("opt"), named("rel"));
    let (from_include, four) = (named("from-include"), "build.target-dir = 4".to_owned());
    #[expect(
        clippy::type_complexity,
        reason = "a row: config, legacy config, included files, target, overridable"
    )]
    let rows: Vec<(&str, Option<&str>, Vec<(&str, &str)>, Option<PathBuf>, bool)> = vec![
        (
            "[build]\ntarget-dir = 'target/.ubd/main.noindex'",
            None,
            vec![],
            Some(root.join(DEFAULT_TARGET)),
            true,
        ),
        (
            "build.target-dir = 'inner'",
            None,
            vec![],
            Some(root.join("inner")),
            true,
        ),
        (
            "build.target-dir = 'inner'",
            Some("build.target-dir = 'legacy'"),
            vec![],
            Some(root.join("legacy")),
            true,
        ),
        (
            "# target-dir = 'comment'",
            None,
            vec![],
            Some(home.join("outer")),
            true,
        ),
        ("build.target-dir = 4", None, vec![], None, true),
        ("build.target-dir = [", None, vec![], None, true),
        // An include that names none leaves the walk to a farther config.
        (
            "include = ['layout.toml']",
            None,
            vec![("layout.toml", "")],
            Some(home.join("outer")),
            true,
        ),
        (
            "include = ['layout.toml']",
            None,
            vec![("layout.toml", &from_include)],
            Some(root.join("from-include")),
            true,
        ),
        (
            "include = ['layout.toml']\nbuild.target-dir = 'local'",
            None,
            vec![("layout.toml", &from_include)],
            Some(root.join("local")),
            true,
        ),
        // Only the winning value is judged: the own target outranks an include's of no string.
        (
            "include = ['layout.toml']\nbuild.target-dir = 'local'",
            None,
            vec![("layout.toml", &four)],
            Some(root.join("local")),
            true,
        ),
        (
            "include = ['layout.toml']",
            None,
            vec![("layout.toml", &four)],
            None,
            false,
        ),
        // A later include over an earlier one, and an earlier one where the later names none.
        (
            "include = ['layout.toml', 'later.toml']",
            None,
            vec![("layout.toml", &early), ("later.toml", &later)],
            Some(root.join("later")),
            true,
        ),
        (
            "include = ['layout.toml', 'later.toml']",
            None,
            vec![("layout.toml", &early), ("later.toml", "")],
            Some(root.join("early")),
            true,
        ),
        // An optional include is read where it stands and skipped where nothing does.
        (
            "include = [{ path = 'opt.toml', optional = true }]",
            None,
            vec![("opt.toml", &opt)],
            Some(root.join("opt")),
            true,
        ),
        (
            "include = [{ path = 'absent.toml', optional = true }]",
            None,
            vec![],
            Some(home.join("outer")),
            true,
        ),
        // A relative target is the defining file's grandparent's: `.cargo` for `.cargo/sub`'s,
        // `.cargo/sub` for a nested include's in `.cargo/sub/more`.
        (
            "include = ['sub/deep.toml']",
            None,
            vec![("sub/deep.toml", &rel)],
            Some(dot.join("rel")),
            true,
        ),
        (
            "include = ['layout.toml']",
            None,
            vec![
                ("layout.toml", "include = ['sub/deep.toml']"),
                ("sub/deep.toml", "include = ['more/deeper.toml']"),
                ("sub/more/deeper.toml", &rel),
            ],
            Some(dot.join("sub/rel")),
            true,
        ),
        // What Targo refuses: a missing include, a cycle, a diamond (one cycle check spans the
        // file's includes), an `include` that is no list, and an included file with no
        // grandparent (`Definition::root` unwraps it, and Targo panics).
        ("include = ['absent.toml']", None, vec![], None, false),
        (
            "include = ['layout.toml']",
            None,
            vec![("layout.toml", "include = ['config.toml']")],
            None,
            false,
        ),
        (
            "include = ['layout.toml', 'later.toml']",
            None,
            vec![
                ("layout.toml", "include = ['sub/deep.toml']"),
                ("later.toml", "include = ['sub/deep.toml']"),
                ("sub/deep.toml", &rel),
            ],
            None,
            false,
        ),
        (
            "include = 'layout.toml'",
            None,
            vec![("layout.toml", "")],
            None,
            false,
        ),
        (
            "include = ['../../../../../../../../../../../../../../../../../../../../x.toml']",
            None,
            vec![],
            None,
            true,
        ),
        // `Definition::root` unwraps the defining file's grandparent before Targo asks whether
        // the target is absolute, so an absolute one there panics too.
        ("include = ['/y.toml']", None, vec![], None, true),
        // What Targo's merge refuses (`merge_helper`: a list or a table meets another value) or
        // cannot load (a float or a date): such a value anywhere in a file's chain, though a
        // later or the file's own value is a string.
        (
            "include = ['layout.toml', 'later.toml']",
            None,
            vec![
                ("layout.toml", "build.target-dir = ['x']"),
                ("later.toml", &later),
            ],
            None,
            false,
        ),
        (
            "include = ['layout.toml']\nbuild.target-dir = 'local'",
            None,
            vec![("layout.toml", "build.target-dir = { a = 'x' }")],
            None,
            false,
        ),
        (
            "include = ['layout.toml']\nbuild.target-dir = 'local'",
            None,
            vec![("layout.toml", "build.target-dir = 4.5")],
            None,
            false,
        ),
        // A nearer `build` that is no table Targo cannot merge with a farther one's table.
        ("build = 4", None, vec![], None, true),
        // An include path holding a control character is refused, and names both.
        (
            "include = [\"a\\tb.toml\"]",
            None,
            vec![("a\tb.toml", &rel)],
            None,
            false,
        ),
        // TOML 1.1, which Targo parses: an inline table's newline, a key `\x` spells.
        (
            "build = { target-dir = 'inline',\n}",
            None,
            vec![],
            Some(root.join("inline")),
            true,
        ),
        (
            "build.\"target\\x2ddir\" = 'escaped'",
            None,
            vec![],
            Some(root.join("escaped")),
            true,
        ),
    ];
    // The root of the scratch's filesystem, where an include `..` walks past (`/` on Unix, the
    // scratch's drive on Windows, where `/y.toml` names that drive's root too).
    let top = scratch.path().ancestors().last().unwrap();
    for (text, legacy_text, files, expected, overridable) in rows {
        let files: BTreeMap<PathBuf, String> = files
            .iter()
            .map(|&(name, text)| (dot.join(name), text.to_owned()))
            .chain([
                (top.join("x.toml"), rel.clone()),
                (top.join("y.toml"), named("/abs/out")),
            ])
            .collect();
        let read = |path: &Path| {
            Ok(if path == config {
                Some(text.into())
            } else if path == legacy {
                legacy_text.map(str::to_owned)
            } else if path == outer {
                Some("build.target-dir = 'outer'".into())
            } else if path == manifest {
                Some("[workspace]".into())
            } else {
                files.get(path).cloned()
            })
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let row = format!("{text:?} {files:?}");
        assert_eq!(
            selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None)
                .map(|c| c.target.explicit_path()),
            expected,
            "{row}"
        );
        match &expected {
            Some(path) => assert_eq!(
                target_hint(&machine, Tool::Cargo),
                Ok(Some(path.clone())),
                "{row}"
            ),
            None => assert!(target_hint(&machine, Tool::Cargo).is_err(), "{row}"),
        }
        // A caller's choice is owned where no config names a build directory, or may.
        let overridden = root.join("caller-choice");
        let env =
            |key: &str| (key == "CARGO_TARGET_DIR").then(|| overridden.clone().into_os_string());
        assert_eq!(
            selected_outputs(
                &args,
                Tool::Targo,
                &Machine {
                    env: &env,
                    ..machine
                },
                &|_| UserAlias::None,
            )
            .map(|c| c.target.explicit_path()),
            overridable.then(|| overridden.clone()),
            "{row}"
        );
    }
}

/// An upstream cargo reads `include` only from 1.93, and before it under `-Zconfig-include`
/// alone, so the front never follows one for it: a config whose include names the target grants
/// none, and its own target is the one granted. NEGATIVE CONTROL: Targo, which always loads
/// includes, gets the include's target.
#[test]
fn an_upstream_cargo_follows_no_include() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let root = home.join("project");
    let config = root.join(".cargo/config.toml");
    let layout = root.join(".cargo/layout.toml");
    let manifest = root.join("Cargo.toml");
    for (text, cargo, targo) in [
        (
            "include = ['layout.toml']",
            None,
            Some(root.join("from-include")),
        ),
        (
            "include = ['layout.toml']\nbuild.target-dir = 'own'",
            Some(root.join("own")),
            Some(root.join("own")),
        ),
    ] {
        let read = |path: &Path| {
            Ok(if path == config {
                Some(text.to_owned())
            } else if path == layout {
                Some("build.target-dir = 'from-include'".to_owned())
            } else if path == manifest {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&root),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        for (tool, words, want) in [
            (Tool::Cargo, vec!["build".to_owned()], &cargo),
            (
                Tool::Targo,
                vec!["--unverified".to_owned(), "build".to_owned()],
                &targo,
            ),
        ] {
            assert_eq!(
                &selected_outputs(&words, tool, &machine, &|_| UserAlias::None)
                    .map(|c| c.target.explicit_path()),
                want,
                "{tool:?} {text:?}"
            );
        }
    }
}

/// [`config_dirs`] READS EACH FILE AS TARGO PARSES IT, as TOML 1.1, wherever its lines cannot
/// tell (an include, an escape that can spell a key, a multi-line string): a key `\x` spells is
/// the key it decodes to, and a file whose include holds TOML 1.1 is read for what it names.
/// Text neither reader parses names both; a file whose lines tell is told by its lines.
/// NEGATIVE CONTROL: the TOML 1.0 reader named both directories for each of the first five
/// rows (the fifth's included file holds a multi-line string, so it is parsed, not scanned).
#[test]
fn config_dirs_read_each_file_as_targo_parses_it() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let dir = home.join("project/.cargo");
    let config = dir.join("config.toml");
    let more = dir.join("more.toml");
    let inline = "include = [\"more.toml\"]\nenv = { A = \"1\",\n}\n";
    for (text, included, expected) in [
        (
            "[build]\n\"target\\x2ddir\" = \"/t\"\n",
            None,
            (true, false),
        ),
        (
            "[build]\n\"target\\x2edir\" = \"/t\"\n",
            None,
            (false, false),
        ),
        (inline, Some("[build]\njobs = 4\n"), (false, false)),
        (inline, Some("[build]\nbuild-dir = \"/b\"\n"), (false, true)),
        (
            "include = [\"more.toml\"]\n",
            Some("build = {\n  target-dir = \"/t\", x = '''a''', # here\n}\n"),
            (true, false),
        ),
        ("x = '''a'''\nt = { a = 1 b = 2 }\n", None, (true, true)),
        ("[env]\nE = \"\\e\"\n", None, (false, false)),
        ("build.target-dir = [\n", None, (true, false)),
    ] {
        let read = |path: &Path| {
            Ok(if path == config {
                Some(text.to_owned())
            } else if path == more {
                included.map(str::to_owned)
            } else {
                None
            })
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&home),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| None,
        };
        let named = config_dirs(&dir, &machine);
        assert_eq!(
            (named.target, named.build),
            expected,
            "{text:?} {included:?}"
        );
    }
}

/// A CONFIG NAMES WHAT ITS `build` TABLE NAMES, not what its lines mention ([`config_dirs`]): a
/// line that spells a directory's key in an alias's words (trust-mc's `build-dev = "run
/// --target-dir target/tools …"`), a comment after a value or another string names no
/// directory, and leaves a call from the folder unresolved by no config
/// ([`configs_unresolved`]); only a call whose alias words this reader cannot tell reads the
/// mention ([`configs_mention_dirs`]). Text no reader parses whose lines tell names what they
/// spell, as before it was parsed. NEGATIVE CONTROL: the line scan named a target, build or
/// artifact directory for each of the first four rows, so every call from such a folder owed
/// custody of a directory no config named, and its front refused it; a `build` table that
/// names one, in either spelling, still names it, and a file that mentions none mentions none.
#[test]
fn a_config_names_what_its_build_table_names_not_what_its_lines_mention() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let root = home.join("project");
    let dir = root.join(".cargo");
    let config = dir.join("config.toml");
    let trust_mc = "[build]\njobs = 12\nrustc-wrapper = \"scripts/trustc-private-wrapper.sh\"\n\
                    [alias]\nbuild-dev = \"run --target-dir target/tools -p build-trust-mc -- \
                    build-dev\"\n[env]\nRAYON_NUM_THREADS = { value = \"2\", force = true }\n";
    // (target, build, artifact, mentioned)
    for (text, expected) in [
        (trust_mc, (false, false, false, true)),
        (
            "[build]\njobs = 4 # target-dir, build-dir\n",
            (false, false, false, true),
        ),
        (
            "[env]\nNOTE = \"target-dir\"\n",
            (false, false, false, true),
        ),
        (
            "[alias]\ndist = \"build --artifact-dir dist\"\n",
            (false, false, false, true),
        ),
        ("[build]\ntarget-dir = \"t\"\n", (true, false, false, true)),
        ("build.build-dir = \"b\"\n", (false, true, false, true)),
        (
            "[build]\nartifact-dir = \"a\"\n",
            (false, false, true, true),
        ),
        ("build.target-dir = [\n", (true, false, false, true)),
        ("[build]\njobs = 4\n", (false, false, false, false)),
    ] {
        let read = |path: &Path| Ok((path == config).then(|| text.to_owned()));
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&root),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| None,
        };
        let named = config_dirs(&dir, &machine);
        assert_eq!(
            (named.target, named.build, named.artifact, named.mentioned),
            expected,
            "{text:?}"
        );
        let (target, build, artifact, mentioned) = expected;
        assert_eq!(
            configs_unresolved(&root, Tool::Targo, &machine, true),
            target || build || artifact,
            "{text:?}"
        );
        assert_eq!(
            configs_mention_dirs(&root, Tool::Targo, &machine),
            mentioned,
            "{text:?}"
        );
    }
}

/// A `--config` VALUE IS READ AS TARGO READS IT (`context/mod.rs`'s `cli_args_as_table`): the
/// config file standing at its path against the working directory, parsed whole with the files
/// it includes against its own folder, or else an inline dotted key, with an `include` against
/// the working directory, each read where Targo reads it. A target directory it selects is the
/// call's where neither a typed `--target-dir` nor `CARGO_TARGET_DIR` (or its twin) outranks it
/// (`target_dir`: a `--config` value outranks `CARGO_BUILD_TARGET_DIR`), and a build directory
/// it selects is the call's too, each against its definition base: the folder above the
/// defining file's, the working directory for an inline value's. NEGATIVE CONTROL: each refused
/// row is a text this reader cannot read (a cycle, a value naming no file that is no dotted key,
/// a file the reader fails on or says is none where something stands, text no TOML 1.1 reader
/// parses), one whose directory Targo refuses (a control character, here `\e`'s), or one Targo
/// refuses (no one dotted key); each owned row was refused before the reading (the TOML 1.1 rows
/// before it read TOML 1.1, the rows that name a directory before it read one with its
/// definition base), the walk's for a folder its reader says is none, and each row Targo
/// refuses was owned.
#[test]
fn a_config_value_is_read_as_targo_reads_it() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let root = home.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let fresh = Some(root.join("fresh"));
    let env_target = root.join("env");
    let jobs = "[build]\njobs = 2\n";
    let named_target = "[build]\ntarget-dir = 'elsewhere'\n";
    let named_build = "[build]\nbuild-dir = 'b'\n";
    let hidden_build = "build = { x = \"\"\"\n#\"\"\", build-dir = 'b' }\n";
    let hidden_target = "build = { x = '''\n#''', target-dir = 'elsewhere' }\n";
    // Something stands at `o=5` that the reader says is no file (a folder, a FIFO, a device
    // `read_config` declines): Targo loads it as a config file all the same.
    std::fs::create_dir(root.join("o=5")).unwrap();
    // A file stands at `o=4` that the reader fails on; nothing can stand at a name longer than
    // a path component may be, which the reader fails on too (atpkg's does, ENAMETOOLONG), and
    // Targo, whose `Path::exists` finds nothing there, parses inline.
    std::fs::write(root.join("o=4"), "").unwrap();
    let long = format!("env.LONG=\"{}\"", "x".repeat(300));
    // A file whose only mentions of a directory's key are an alias's words and a comment.
    let mentions = "[alias]\ndist = \"build --artifact-dir dist\"\n[build]\njobs = 4 # build-dir\n";
    let words = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let f = "--target-dir fresh";
    #[expect(
        clippy::type_complexity,
        reason = "a row: words, files, environment, the target owned and the build directory"
    )]
    let rows: Vec<(
        String,
        Vec<(&str, Result<&str, ()>)>,
        Option<&str>,
        Option<PathBuf>,
        Option<PathBuf>,
    )> = vec![
        // A file the call's target outranks, or that names nothing.
        (
            format!("--config extra.toml --unverified build {f}"),
            vec![("extra.toml", Ok(jobs))],
            None,
            fresh.clone(),
            None,
        ),
        (
            format!("--config extra.toml --unverified build {f}"),
            vec![("extra.toml", Ok(named_target))],
            None,
            fresh.clone(),
            None,
        ),
        (
            "--config extra.toml --unverified build".into(),
            vec![("extra.toml", Ok(named_target))],
            Some("CARGO_TARGET_DIR"),
            Some(env_target.clone()),
            None,
        ),
        (
            "--config extra.toml --unverified build".into(),
            vec![("extra.toml", Ok(named_target))],
            Some("TARGO_TARGET_DIR"),
            Some(env_target.clone()),
            None,
        ),
        // A `--config` outranks `CARGO_BUILD_TARGET_DIR`, in either spelling.
        (
            "--config extra.toml --unverified build".into(),
            vec![("extra.toml", Ok(named_target))],
            Some("CARGO_BUILD_TARGET_DIR"),
            Some(home.join("elsewhere")),
            None,
        ),
        (
            "--config extra.toml --unverified build".into(),
            vec![("extra.toml", Ok(named_target))],
            Some("TARGO_BUILD_TARGET_DIR"),
            Some(home.join("elsewhere")),
            None,
        ),
        (
            "--config extra.toml --unverified build".into(),
            vec![("extra.toml", Ok(jobs))],
            Some("CARGO_BUILD_TARGET_DIR"),
            Some(env_target.clone()),
            None,
        ),
        (
            format!("--config extra.toml --unverified build {f}"),
            vec![("extra.toml", Ok(named_build))],
            None,
            fresh.clone(),
            Some(home.join("b")),
        ),
        // A key a multi-line string hides from a line scan; a file in TOML 1.1, which Targo
        // reads, that names nothing; and text no reader parses.
        (
            format!("--config extra.toml --unverified build {f}"),
            vec![("extra.toml", Ok(hidden_build))],
            None,
            fresh.clone(),
            Some(home.join("b")),
        ),
        (
            "--config extra.toml --unverified build".into(),
            vec![("extra.toml", Ok(hidden_target))],
            Some("CARGO_BUILD_TARGET_DIR"),
            Some(home.join("elsewhere")),
            None,
        ),
        (
            format!("--config extra.toml --unverified build {f}"),
            vec![("extra.toml", Ok("t = { a = 1, }\n"))],
            None,
            fresh.clone(),
            None,
        ),
        (
            format!("--config extra.toml --unverified build {f}"),
            vec![("extra.toml", Ok("t = { a = 1 b = 2 }\n"))],
            None,
            None,
            None,
        ),
        // A file's include is read against its own folder.
        (
            format!("--config conf/extra.toml --unverified build {f}"),
            vec![
                ("conf/extra.toml", Ok("include = ['more.toml']\n")),
                ("conf/more.toml", Ok(named_build)),
            ],
            None,
            fresh.clone(),
            Some(root.join("b")),
        ),
        (
            format!("--config conf/extra.toml --unverified build {f}"),
            vec![
                ("conf/extra.toml", Ok("include = ['more.toml']\n")),
                ("conf/more.toml", Ok(jobs)),
                ("more.toml", Ok(named_build)),
            ],
            None,
            fresh.clone(),
            None,
        ),
        // One cycle check spans the list, as Targo's one set does.
        (
            format!("--config extra.toml --config extra.toml --unverified build {f}"),
            vec![("extra.toml", Ok(jobs))],
            None,
            None,
            None,
        ),
        // Every value is read, before and after the verb.
        (
            format!("--config bd.toml --unverified build {f} --config build.jobs=1"),
            vec![("bd.toml", Ok(named_build))],
            None,
            fresh.clone(),
            Some(home.join("b")),
        ),
        // A value naming no file that is no dotted key, one the reader fails on, and one where
        // something stands that the reader says is none.
        (
            format!("--config other.toml --unverified build {f}"),
            vec![],
            None,
            None,
            None,
        ),
        (
            format!("--config o=4 --unverified build {f}"),
            vec![("o=4", Err(()))],
            None,
            None,
            None,
        ),
        (
            format!("--config o=5 --unverified build {f}"),
            vec![],
            None,
            None,
            None,
        ),
        (
            format!("--config {long} --unverified build {f}"),
            vec![(long.as_str(), Err(()))],
            None,
            fresh.clone(),
            None,
        ),
        // A file is read whole: what its parse names, not what its lines mention.
        (
            format!("--config ci.toml --unverified build {f}"),
            vec![("ci.toml", Ok(mentions))],
            None,
            fresh.clone(),
            None,
        ),
        // A value read as TOML 1.1, as Targo reads it (`\e`, `\x`, an inline table's trailing
        // comma, here in a list of include tables), and one no reader parses that spells a
        // directory's key by its letters.
        (
            format!("--config build.build-dir=\"b\\e\" --unverified build {f}"),
            vec![],
            None,
            None,
            None,
        ),
        (
            format!("--config include=[{{path=\"more.toml\",}}] --unverified build {f}"),
            vec![("more.toml", Ok(named_build))],
            None,
            fresh.clone(),
            Some(home.join("b")),
        ),
        (
            format!("--config env.X=\"\\x41\" --unverified build {f}"),
            vec![],
            None,
            fresh.clone(),
            None,
        ),
        (
            format!("--config build.\"target\\x2ddir\"=\"x\" --unverified build {f}"),
            vec![],
            None,
            fresh.clone(),
            None,
        ),
        (
            format!("--config build.build-dir=\"b\\q\" --unverified build {f}"),
            vec![],
            None,
            None,
            None,
        ),
        // Inline values: a backslash escapes no key, a quoted key is its name, an include is
        // read against the working directory.
        (
            format!("--config env.X=\"a\\\\b\" --unverified build {f}"),
            vec![],
            None,
            fresh.clone(),
            None,
        ),
        (
            format!("--config env.X=\"\\e\" --unverified build {f}"),
            vec![],
            None,
            fresh.clone(),
            None,
        ),
        (
            format!("--config build.\"build-dir\"=\"b\" --unverified build {f}"),
            vec![],
            None,
            fresh.clone(),
            Some(root.join("b")),
        ),
        (
            "--config build.\"target-dir\"=\"x\" --unverified build".into(),
            vec![],
            Some("CARGO_BUILD_TARGET_DIR"),
            Some(root.join("x")),
            None,
        ),
        (
            format!("--config build.\"target-dir\"=\"x\" --unverified build {f}"),
            vec![],
            None,
            fresh.clone(),
            None,
        ),
        (
            "--config build.\"target\\x2ddir\"=\"x\" --unverified build".into(),
            vec![],
            Some("CARGO_BUILD_TARGET_DIR"),
            Some(root.join("x")),
            None,
        ),
        (
            format!("--config include=[\"more.toml\"] --unverified build {f}"),
            vec![("more.toml", Ok(named_build))],
            None,
            fresh.clone(),
            Some(home.join("b")),
        ),
        (
            format!("--config include=[\"more.toml\"] --unverified build {f}"),
            vec![("more.toml", Ok(jobs))],
            None,
            fresh.clone(),
            None,
        ),
        // Each inline value's includes load with a cycle check of their own, apart from the one
        // the file values share (`cli_args_as_table`'s fresh `HashSet` per value): a file two
        // values include, or one a file value already loaded, is no cycle.
        (
            format!(
                "--config include=[\"more.toml\"] --config include=[\"more.toml\"] \
                 --unverified build {f}"
            ),
            vec![("more.toml", Ok(jobs))],
            None,
            fresh.clone(),
            None,
        ),
        (
            format!("--config more.toml --config include=[\"more.toml\"] --unverified build {f}"),
            vec![("more.toml", Ok(jobs))],
            None,
            fresh.clone(),
            None,
        ),
        (
            format!("--config include=[\"more.toml\"] --config more.toml --unverified build {f}"),
            vec![("more.toml", Ok(jobs))],
            None,
            fresh.clone(),
            None,
        ),
        // A walk config's hidden build directory, beside a value the shim leaves as typed.
        (
            format!("--config env.X=\"a\\\\b\" --unverified build {f}"),
            vec![(".cargo/config.toml", Ok(hidden_build))],
            None,
            fresh.clone(),
            Some(root.join("b")),
        ),
    ];
    let mut wrong = Vec::new();
    for (line, files, var, expected, intermediates) in rows {
        let files: BTreeMap<PathBuf, Result<String, ()>> = files
            .iter()
            .map(|&(name, text)| (root.join(name), text.map(str::to_owned)))
            .chain([(root.join("Cargo.toml"), Ok("[workspace]".to_owned()))])
            .collect();
        let read = |path: &Path| files.get(path).cloned().transpose();
        let env = |name: &str| (Some(name) == var).then(|| env_target.clone().into_os_string());
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&root),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let got = selected_outputs(&words(&line), Tool::Targo, &machine, &|_| UserAlias::None)
            .map(|c| (c.target.explicit_path(), c.intermediates));
        let expected = expected.map(|target| (target, intermediates));
        if got != expected {
            wrong.push(format!("{line} {var:?}: {got:?}, not {expected:?}"));
        }
    }
    // An inline value that is no one dotted key and its value, which Targo refuses
    // (`toml_dotted_keys`): an empty one, blank, a comment, a table's header.
    let read = |path: &Path| Ok((path == root.join("Cargo.toml")).then(|| "[workspace]".into()));
    let machine = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&root),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    for value in ["", " ", "#", "[build]", "a = 1\nb = 2"] {
        let line = [
            "--unverified",
            "build",
            "--target-dir",
            "fresh",
            "--config",
            value,
        ];
        let line = line.map(str::to_owned);
        let got = selected_outputs(&line, Tool::Targo, &machine, &|_| UserAlias::None);
        if let Some(got) = got {
            wrong.push(format!("--config {value:?}: {:?}, not None", got.target));
        }
    }
    // The configs a walk finds are the reader's to read: what stands on the disk beside them
    // is no file to it (atpkg's reader fails on a folder there, and so names both).
    std::fs::create_dir_all(root.join(".cargo/config.toml")).unwrap();
    let line = words(&format!("--unverified build {f}"));
    let got = selected_outputs(&line, Tool::Targo, &machine, &|_| UserAlias::None)
        .map(|c| c.target.explicit_path());
    if got != fresh {
        wrong.push(format!(
            "a walk read past its reader: {got:?}, not {fresh:?}"
        ));
    }
    std::fs::remove_dir(root.join(".cargo/config.toml")).unwrap();
    // The words alone: a file's target directory moves a build no typed target outranks, the
    // program's `--target-dir` among them; an empty one is the working directory, which
    // outranks it.
    for (line, text, unresolved) in [
        ("--unverified build --config extra.toml", jobs, false),
        ("--unverified build --config extra.toml", named_target, true),
        (
            "--unverified build --target-dir x --config extra.toml",
            named_target,
            false,
        ),
        (
            "--unverified run --config extra.toml serve --target-dir x",
            named_target,
            true,
        ),
        (
            "--unverified build --target-dir= --config extra.toml",
            named_target,
            false,
        ),
    ] {
        let extra = root.join("extra.toml");
        let read = |path: &Path| Ok((path == extra).then(|| text.to_owned()));
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&root),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        if unresolved_words(&words(line), Tool::Targo, &machine) != unresolved {
            wrong.push(format!("{line} {text:?}: unresolved is not {unresolved}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A line that starts with `#` is a comment only outside a multi-line string: one that closes the
/// string there can go on to name a directory, which Targo reads and the line scan skips, so a
/// file holding a multi-line string is parsed, and one the parser cannot read names both. A file
/// it includes is read the same way, as TOML 1.1. NEGATIVE CONTROL: the first four rows read as
/// naming nothing without the parse, and the unparseable row too, though Targo may build into
/// what it names; a TOML 1.1 row that names nothing, a `#` line inside the string, and one with
/// no string, still name nothing.
#[test]
fn config_dirs_read_a_key_a_multiline_string_hides_from_the_line_scan() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let dir = home.join("project/.cargo");
    let config = dir.join("config.toml");
    let more = dir.join("more.toml");
    let hidden_target = "build = { dep-info-basedir = \"\"\"\n#\"\"\", target-dir = \"/t\" }\n";
    for (text, included, expected) in [
        (hidden_target, None, (true, false)),
        (
            "build = { dep-info-basedir = \"\"\"\n#\"\"\", build-dir = \"/b\" }\n",
            None,
            (false, true),
        ),
        (
            "build = { dep-info-basedir = '''\n  #''', target-dir = '/t' }\n",
            None,
            (true, false),
        ),
        (
            "include = [\"more.toml\"]\n",
            Some(hidden_target),
            (true, false),
        ),
        // TOML 1.1's trailing comma, which Targo parses, names nothing; text no reader parses
        // may name either.
        ("x = '''a'''\nt = { a = 1, }\n", None, (false, false)),
        ("x = '''a'''\nt = { a = 1 b = 2 }\n", None, (true, true)),
        (
            "x = \"\"\"\n# target-dir = \"/t\"\n\"\"\"\n",
            None,
            (false, false),
        ),
        ("# target-dir = \"/t\"\n", None, (false, false)),
    ] {
        let read = |path: &Path| {
            Ok(if path == config {
                Some(text.to_owned())
            } else if path == more {
                included.map(str::to_owned)
            } else {
                None
            })
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&home),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| None,
        };
        let named = config_dirs(&dir, &machine);
        assert_eq!(
            (named.target, named.build),
            expected,
            "{text:?} {included:?}"
        );
    }
}

/// An artifact directory (`--artifact-dir`, `build.artifact-dir` in a `--config`, the
/// environment or a config file) is a folder Targo creates and links final artifacts into
/// (`commands/build.rs`, `compiler/mod.rs`), which no target this front grants holds: it leaves
/// the call's directories unresolved, so the call owes custody and no explicit target is granted.
/// Only `build` reads one (`commands/build.rs` alone sets `export_dir`): another command Targo
/// runs itself (`check`, `test`, `doc`) reads no config, variable or `--config` value naming one,
/// while `b` (`build`'s alias) and an external command, which may run `build`, do; a typed
/// `--artifact-dir`, which only `build` takes (clap refuses it elsewhere), leaves any call
/// unresolved. NEGATIVE CONTROL: the same words after `--` or in a trailing verb's program are
/// the program's, and a config naming none names none.
#[test]
fn an_artifact_directory_leaves_the_call_unresolved() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let root = home.join("project");
    let config = root.join(".cargo/config.toml");
    let manifest = root.join("Cargo.toml");
    let words = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let target = ("CARGO_TARGET_DIR", "/t");
    for (line, var, text, unresolved) in [
        (
            "-Zunstable-options --unverified build --artifact-dir out",
            None,
            None,
            true,
        ),
        (
            "-Zunstable-options --unverified build --artifact-dir=out",
            None,
            None,
            true,
        ),
        (
            "--unverified build --config build.artifact-dir='out'",
            None,
            None,
            true,
        ),
        (
            "--unverified --config=build.artifact-dir=\"out\" build",
            None,
            None,
            true,
        ),
        (
            "--unverified build",
            Some("CARGO_BUILD_ARTIFACT_DIR"),
            None,
            true,
        ),
        (
            "--unverified build",
            Some("TARGO_BUILD_ARTIFACT_DIR"),
            None,
            true,
        ),
        (
            "--unverified build",
            None,
            Some("[build]\nartifact-dir = \"out\"\n"),
            true,
        ),
        (
            "--unverified build",
            None,
            Some("build.artifact-dir = 'out'\n"),
            true,
        ),
        (
            "--unverified build",
            None,
            Some("[build]\n\"artifact\\u002ddir\" = \"o\"\n"),
            true,
        ),
        ("--unverified run -- --artifact-dir out", None, None, false),
        (
            "--unverified run serve --artifact-dir out",
            None,
            None,
            false,
        ),
        (
            "--unverified build",
            None,
            Some("[build]\njobs = 2\n"),
            false,
        ),
        (
            "--unverified check",
            None,
            Some("[build]\nartifact-dir = \"out\"\n"),
            false,
        ),
        (
            "--unverified test",
            Some("CARGO_BUILD_ARTIFACT_DIR"),
            None,
            false,
        ),
        (
            "--unverified doc --config build.artifact-dir='out'",
            None,
            None,
            false,
        ),
        (
            "--unverified b",
            None,
            Some("[build]\nartifact-dir = \"out\"\n"),
            true,
        ),
        (
            "--unverified xtask",
            None,
            Some("[build]\nartifact-dir = \"out\"\n"),
            true,
        ),
        (
            "-Zunstable-options --unverified check --artifact-dir out",
            None,
            None,
            true,
        ),
    ] {
        let read = |path: &Path| {
            Ok(if path == config {
                text.map(str::to_owned)
            } else if path == manifest {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let env = |name: &str| {
            if name == target.0 {
                Some(target.1.into())
            } else {
                (Some(name) == var).then(|| "out".into())
            }
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&root),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = words(line);
        let named = unresolved_words(&args, Tool::Targo, &machine)
            || configs_unresolved(&root, Tool::Targo, &machine, reads_artifacts(&args));
        assert_eq!(named, unresolved, "{line} {var:?} {text:?}");
        assert_eq!(
            selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None).is_none(),
            unresolved,
            "{line} {var:?} {text:?}"
        );
    }
    // A `TARGO_HOME`'s config is the home Trust's main reads where `CARGO_HOME` is unset: an
    // artifact or build directory it names leaves the call unresolved, whichever build runs it.
    // NEGATIVE CONTROL: a set `CARGO_HOME` outranks it, and a config naming none names none.
    let twin_config = scratch.path().join("th/config.toml");
    for (text, cargo_home, unresolved) in [
        ("[build]\nartifact-dir = \"out\"\n", None, true),
        ("[build]\nbuild-dir = \"bd\"\n", None, true),
        ("[build]\nartifact-dir = \"out\"\n", Some("/ch"), false),
        ("[build]\njobs = 2\n", None, false),
    ] {
        let read = |path: &Path| {
            Ok(if path == twin_config {
                Some(text.to_owned())
            } else if path == manifest {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let twin_home = scratch.path().join("th").into_os_string();
        let env = |name: &str| match name {
            "CARGO_TARGET_DIR" => Some(target.1.into()),
            "TARGO_HOME" => Some(twin_home.clone()),
            "CARGO_HOME" => cargo_home.map(OsString::from),
            _ => None,
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&root),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = words("--unverified build");
        assert_eq!(
            configs_unresolved(&root, Tool::Targo, &machine, true),
            unresolved,
            "{text} {cargo_home:?}"
        );
        assert_eq!(
            selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None).is_none(),
            unresolved,
            "{text} {cargo_home:?}"
        );
    }
}

/// Targo loads one config file per folder: the legacy `config` wherever one stands, else
/// `config.toml` (`context/mod.rs`'s `get_file_path`), so a `config.toml` beside a `config` names
/// nothing, even one this reader cannot parse. NEGATIVE CONTROL: with no `config` there, the
/// same `config.toml` names what it names, and a `config` read as text names its own keys.
#[test]
fn config_dirs_read_only_the_config_file_targo_loads() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let dir = home.join("project/.cargo");
    let legacy = dir.join("config");
    let config = dir.join("config.toml");
    for (shadowed, beside, expected) in [
        (
            "x = '''a'''\nt = { a = 1 b = 2 }\n",
            Some("[env]\nA = \"1\"\n"),
            (false, false),
        ),
        (
            "[build]\ntarget-dir = \"/t\"\n",
            Some("[env]\nA = \"1\"\n"),
            (false, false),
        ),
        ("x = '''a'''\nt = { a = 1 b = 2 }\n", None, (true, true)),
        ("[build]\ntarget-dir = \"/t\"\n", None, (true, false)),
        (
            "[env]\nA = \"1\"\n",
            Some("[build]\nbuild-dir = \"/b\"\n"),
            (false, true),
        ),
    ] {
        let read = |path: &Path| {
            Ok(if path == config {
                Some(shadowed.to_owned())
            } else if path == legacy {
                beside.map(str::to_owned)
            } else {
                None
            })
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&home),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| None,
        };
        let named = config_dirs(&dir, &machine);
        assert_eq!(
            (named.target, named.build),
            expected,
            "{shadowed:?} {beside:?}"
        );
    }
}

/// A configured target holding a control character is refused, as [`decide`] refuses to place
/// under one: a string this reader cannot decode as Targo does (a multi-line string's CRLF, which
/// `toml` keeps and `aterm_toml` folds) is not read at all ([`config_toml`]), which refuses too.
/// NEGATIVE CONTROL: without the refusal each row but the CRLF one owns the folder it names.
#[test]
fn a_configured_target_holding_a_control_character_is_refused() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let root = home.join("project");
    let config = root.join(".cargo/config.toml");
    let manifest = root.join("Cargo.toml");
    let args = ["--unverified".into(), "build".into()];
    for text in [
        "build.target-dir = \"\"\"\r\nx\r\ny\"\"\"\n",
        "build.target-dir = '''\nx\ny'''\n",
        "build.target-dir = \"x\\ny\"\n",
        "build.target-dir = \"x\\ty\"\n",
        "build.target-dir = \"x\\u007fy\"\n",
    ] {
        let read = |path: &Path| {
            Ok(if path == config {
                Some(text.to_owned())
            } else if path == manifest {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&root),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        assert_eq!(
            selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None),
            None,
            "{text:?}"
        );
        assert!(target_hint(&machine, Tool::Cargo).is_err(), "{text:?}");
        // The config names a target, so the call stays one whose directories need custody.
        assert!(
            configs_unresolved(&root, Tool::Targo, &machine, true),
            "{text:?}"
        );
    }
    // Only the target Targo takes is judged: the nearest config's, which outranks a farther one
    // holding a control character, so the call keeps the target it names.
    let shadowed = home.join(".cargo/config.toml");
    let read = |path: &Path| {
        Ok(if path == config {
            Some("[build]\ntarget-dir = \"out\"\n".to_owned())
        } else if path == shadowed {
            Some("[build]\ntarget-dir = \"a\\tb\"\n".to_owned())
        } else if path == manifest {
            Some("[workspace]".into())
        } else {
            None
        })
    };
    let machine = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&root),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    assert_eq!(
        selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None),
        Some(SelectedOutputs {
            target: FinalTarget::Explicit(root.join("out")),
            manifests: vec![manifest.clone()],
            within: None,
            intermediates: None,
        })
    );
    assert_eq!(
        target_hint(&machine, Tool::Cargo),
        Ok(Some(root.join("out")))
    );
}

#[test]
fn target_lookup_distinguishes_cargo_defaults_and_invalid_overrides() {
    let cwd = Path::new("/workspace/member");
    let machine = Machine {
        home: None,
        root: None,
        cwd: Some(cwd),
        env: &|_| None,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| None,
    };
    assert_eq!(target_hint(&machine, Tool::Cargo).unwrap(), None);
    let selected = |name: &str| match name {
        "CARGO_TARGET_DIR" => Some("selected".into()),
        "CARGO_BUILD_TARGET_DIR" => Some("other".into()),
        _ => None,
    };
    assert_eq!(
        target_hint(
            &Machine {
                env: &selected,
                ..machine
            },
            Tool::Cargo
        )
        .unwrap(),
        Some(cwd.join("selected"))
    );
    assert!(
        target_hint(
            &Machine {
                env: &|name| (name == "CARGO_TARGET_DIR").then(OsString::new),
                ..machine
            },
            Tool::Cargo
        )
        .is_err()
    );
    assert!(
        target_hint(
            &Machine {
                read: &|_| Err(()),
                read_manifest: &|_| Err(()),
                ..machine
            },
            Tool::Cargo
        )
        .is_err()
    );

    for (vars, expected) in [
        (vec![("TARGO_TARGET_DIR", "twin")], Some("twin")),
        (
            vec![
                ("CARGO_BUILD_TARGET_DIR", "build"),
                ("TARGO_TARGET_DIR", "twin"),
            ],
            Some("twin"),
        ),
        (
            vec![("CARGO_TARGET_DIR", "cargo"), ("TARGO_TARGET_DIR", "twin")],
            Some("cargo"),
        ),
        (vec![("TARGO_BUILD_TARGET_DIR", "build")], Some("build")),
        (
            vec![("CARGO_TARGET_DIR", ""), ("TARGO_TARGET_DIR", "twin")],
            None,
        ),
        (vec![("TARGO_TARGET_DIR", "")], None),
        (
            vec![
                ("CARGO_BUILD_TARGET_DIR", ""),
                ("TARGO_BUILD_TARGET_DIR", "build"),
            ],
            None,
        ),
    ] {
        let env = |name: &str| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        };
        let read = target_hint(
            &Machine {
                env: &env,
                ..machine
            },
            Tool::Targo,
        );
        match expected {
            Some(path) => assert_eq!(read.unwrap(), Some(cwd.join(path)), "{vars:?}"),
            None => assert!(read.is_err(), "{vars:?}"),
        }
    }
    let twin = |name: &str| (name == "TARGO_TARGET_DIR").then(|| OsString::from("ignored"));
    assert_eq!(
        target_hint(
            &Machine {
                env: &twin,
                ..machine
            },
            Tool::Cargo
        )
        .unwrap(),
        None
    );

    let home = Path::new("/account");
    let primary = home.join(".cargo/config.toml");
    let secondary = Path::new("/targo/config.toml");
    for (cargo, targo, expected) in [
        (None, None, Ok(None)),
        (
            Some("build.target-dir='/same'"),
            Some("build.target-dir='/same'"),
            Ok(Some(PathBuf::from("/same"))),
        ),
        (
            Some("build.target-dir='/one'"),
            Some("build.target-dir='/two'"),
            Err(()),
        ),
        (Some("build.target-dir='/one'"), None, Err(())),
        (None, Some("build.target-dir='/two'"), Err(())),
        (None, Some("build.target-dir=["), Err(())),
        (None, Some("include=['other.toml']"), Err(())),
    ] {
        let read = |path: &Path| {
            Ok(if path == primary {
                cargo.map(str::to_owned)
            } else if path == secondary {
                targo.map(str::to_owned)
            } else {
                None
            })
        };
        let env = |name: &str| (name == "TARGO_HOME").then(|| OsString::from("/targo"));
        let picked = target_hint(
            &Machine {
                home: Some(home),
                env: &env,
                read: &read,
                read_manifest: &read,
                ..machine
            },
            Tool::Targo,
        )
        .map_err(|_| ());
        assert_eq!(picked, expected, "{cargo:?} {targo:?}");
    }
    let read = |path: &Path| {
        if path == primary {
            Ok(Some("build.target-dir='selected'".into()))
        } else if path == secondary {
            Err(())
        } else {
            Ok(None)
        }
    };
    for selected in ["", "/account/.cargo"] {
        let env = |name: &str| match name {
            "CARGO_HOME" => Some(OsString::from(selected)),
            "TARGO_HOME" => Some(OsString::from("/targo")),
            _ => None,
        };
        assert_eq!(
            target_hint(
                &Machine {
                    home: Some(home),
                    env: &env,
                    read: &read,
                    read_manifest: &read,
                    ..machine
                },
                Tool::Targo
            )
            .unwrap(),
            Some(home.join("selected"))
        );
    }
}
use std::collections::BTreeMap;

/// A SHORT-OPTION CLUSTER READS AS CLAP READS IT: letters that take no value, then one letter
/// whose value is the rest of the word (one `=` dropped) or else the next word, so `-qm X`,
/// `-mX` and `-m=X` each name a manifest. NEGATIVE CONTROL: a letter that takes a value ends
/// the run, so `-pm` (package `m`) names none; a long option, `--`, `-` and a positional are
/// no cluster.
#[test]
fn a_short_option_cluster_reads_as_clap_reads_it() {
    fn m(word: &str) -> Option<Option<&str>> {
        shorts(word).and_then(|cluster| cluster.value_of('m'))
    }
    for (word, value) in [
        ("-m", None),
        ("-mX", Some("X")),
        ("-m=X", Some("X")),
        ("-m=", Some("")),
        ("-m==X", Some("=X")),
        ("-mq", Some("q")),
        ("-qm", None),
        ("-vrqmX", Some("X")),
        ("-nwladVhm=X", Some("X")),
    ] {
        assert_eq!(m(word), Some(value), "{word}");
    }
    for word in [
        "-pm",
        "-jm",
        "-Fm",
        "-Zm",
        "-Cm",
        "-em",
        "-q",
        "-qv",
        "--manifest-path",
        "--m",
        "-",
        "--",
        "m",
        "x-m",
    ] {
        assert_eq!(m(word), None, "{word}");
    }
    let cluster = |flags, valued| Some(Shorts { flags, valued });
    assert_eq!(shorts("-qrv"), cluster("qrv", None));
    assert_eq!(shorts("-rpx"), cluster("r", Some(('p', Some("x")))));
    assert_eq!(shorts("-qC/w"), cluster("q", Some(('C', Some("/w")))));
    assert_eq!(shorts("-vZ"), cluster("v", Some(('Z', None))));
    assert_eq!(
        shorts(TEST_GRAPH_FLAG),
        cluster("", Some(('Z', Some("feature-unification"))))
    );
    assert_eq!(shorts("-qé"), cluster("q", Some(('é', None))));
}

/// A cluster's `-m` is Targo's after a verb that takes one and before `--`, a trailing verb's
/// only before its program's first word; never before the verb, after an external command's
/// or a user alias's name, or after another verb's. A short option's value left to the next
/// word is no word of its own where clap takes it (`-p x`, `-vZ x`, `-qC dir`, even `-j -m`,
/// never `-p -m` nor a digit's), and `--color` and `--explain` take one before the verb, as
/// the placement reader skips them; after it, a trailing verb's long option takes one as clap
/// gives it one (`--package demo`, `--jobs -1`, never `--package -m`).
#[test]
fn a_short_manifest_is_read_only_among_targos_own_words() {
    fn scope(tool: Tool, words: &[&str]) -> Vec<String> {
        let args: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
        short_manifest_scope(&args, tool)
            .into_iter()
            .zip(words)
            .filter(|(reads, _)| *reads)
            .map(|(_, word)| (*word).to_owned())
            .collect()
    }
    let t = Tool::Targo;
    assert_eq!(
        scope(t, &["--unverified", "build", "-qm", "x", "-m"]),
        ["-qm", "-m"]
    );
    assert_eq!(
        scope(t, &["-m", "x", "--unverified", "build"]),
        [] as [&str; 0]
    );
    assert_eq!(scope(t, &["build", "-m", "x", "--", "-m"]), ["-m", "--"]);
    assert_eq!(scope(t, &["trust", "check", "-m"]), ["check", "-m"]);
    assert_eq!(
        scope(t, &["--unverified", "run", "-m", "x", "-q", "demo", "-m"]),
        ["-m", "-q", "demo"]
    );
    assert_eq!(
        scope(t, &["run", "--profile", "p", "-p", "x", "-m"]),
        ["--profile", "-p", "-m"]
    );
    assert_eq!(scope(t, &["run", "-j", "-m", "x", "-m"]), ["-j", "x"]);
    assert_eq!(scope(t, &["run", "-j", "-1", "-m"]), ["-j", "-m"]);
    // An optional value is no word that starts with `-`; a character that is no ASCII letter
    // takes none.
    assert_eq!(
        scope(t, &["package", "-p", "-m", "x", "-m"]),
        ["-p", "-m", "-m"]
    );
    assert_eq!(
        scope(t, &["tree", "-i", "-m", "s", "-p", "y"]),
        ["-i", "-m", "-p"]
    );
    assert_eq!(
        scope(t, &["run", "-p", "-m", "x", "demo", "-m"]),
        ["-p", "-m", "demo"]
    );
    assert_eq!(scope(t, &["run", "-p", "-", "-m"]), ["-p", "-m"]);
    assert_eq!(scope(t, &["build", "-é", "-m", "x"]), ["-é", "-m"]);
    assert_eq!(scope(t, &["build", "-é", "x", "-m"]), ["-é", "x", "-m"]);
    assert_eq!(scope(t, &["run", "-1", "demo", "-m"]), ["-1", "demo"]);
    assert_eq!(scope(t, &["update", "-bm", "x", "-wbm"]), ["-bm", "-wbm"]);
    // A trailing verb's long option takes the word clap gives it, so the program starts
    // after it.
    assert_eq!(
        scope(t, &["run", "--package", "demo", "-m", "x", "y", "-m"]),
        ["--package", "-m", "y"]
    );
    assert_eq!(
        scope(t, &["run", "--package", "-m", "x", "demo", "-m"]),
        ["--package", "-m", "demo"]
    );
    assert_eq!(
        scope(t, &["run", "--jobs", "-1", "demo", "-m"]),
        ["--jobs", "demo"]
    );
    assert_eq!(
        scope(t, &["run", "--color", "always", "-m", "x"]),
        ["--color", "-m"]
    );
    assert_eq!(
        scope(t, &["rustc", "--print", "cfg", "-m"]),
        ["--print", "-m"]
    );
    assert_eq!(
        scope(t, &["run", "--release", "demo", "-m"]),
        ["--release", "demo"]
    );
    for option in VALUED_LONGS {
        assert_eq!(
            scope(t, &["run", option, "v", "-m", "x"]),
            [*option, "-m"],
            "{option}"
        );
        assert_eq!(
            scope(t, &["run", option, "-m", "x", "demo", "-m"]),
            [*option, "-m", "demo"],
            "{option}"
        );
    }
    for verb in ["r", "rustc", "rustdoc"] {
        assert_eq!(scope(t, &[verb, "x", "-m"]), ["x"], "{verb}");
    }
    assert_eq!(scope(t, &["-Z", "x", "test", "-m"]), ["-m"]);
    assert_eq!(scope(t, &["-vZ", "x", "fetch", "-m"]), ["-m"]);
    assert_eq!(scope(t, &["-C", "d", "fetch", "-m"]), ["-m"]);
    assert_eq!(scope(t, &["-qC", "d", "publish", "-m"]), ["-m"]);
    assert_eq!(scope(t, &["--color", "x", "build", "-m"]), ["-m"]);
    assert_eq!(
        scope(t, &["--explain", "E1", "run", "-Z", "x", "-m"]),
        ["-Z", "-m"]
    );
    // No user alias's name is read: the front reads its line ([`targo_line`]).
    assert_eq!(
        scope(t, &["--unverified", "lab", "uname", "-m"]),
        [] as [&str; 0]
    );
    assert_eq!(scope(t, &["owner", "-rmatt"]), [] as [&str; 0]);
    assert_eq!(scope(t, &["run", "-", "-m"]), ["-"]);
    assert_eq!(scope(t, &["build", "-", "-m"]), ["-", "-m"]);
    assert_eq!(scope(Tool::Tippy, &["-m", "x", "y"]), ["-m", "y"]);
    for verb in BUILD_VERBS.iter().chain(MANIFEST_VERBS).chain(&["trust"]) {
        assert_eq!(scope(t, &[*verb, "-m"]), ["-m"], "{verb}");
    }
    for verb in [
        "install",
        "new",
        "init",
        "search",
        "yank",
        "login",
        "uninstall",
        "info",
    ] {
        assert_eq!(scope(t, &[verb, "-m"]), [] as [&str; 0], "{verb}");
    }
}

/// TARGO RUNS THE LINE ITS ALIASES EXPAND TO: the words before the verb, the alias's words,
/// then those after its name, read again the same way; a user alias takes a builtin alias's
/// name, and no builtin command's but a protected tool's under the compat `cargo`. A `--`
/// before the verb hands Targo the word after it. NEGATIVE CONTROL: words the reader cannot
/// tell, an alias with none, a word starting with `-` after such a `--` and aliases that name
/// one another leave no line; a name no alias takes, and the lint's words, are the line typed.
#[test]
fn targo_runs_the_line_its_aliases_expand_to() {
    let aliases = [
        ("b", "build --release"),
        ("lab", "run -q -p aterm-lab --"),
        ("xt", "test --unverified"),
        ("outer", "-v xt -q"),
        ("again", "again"),
        ("t", "run -q --"),
        ("clippy", "build"),
        ("check", "build --release"),
        ("empty", ""),
    ];
    let alias = |verb: &str| {
        if verb == "unknown" {
            return UserAlias::Unknown;
        }
        aliases
            .iter()
            .find(|(name, _)| *name == verb)
            .map_or(UserAlias::None, |(_, words)| {
                UserAlias::Words(words.split_whitespace().map(str::to_owned).collect())
            })
    };
    let line = |tool, words: &str| {
        let args: Vec<String> = words.split_whitespace().map(str::to_owned).collect();
        targo_line(&args, tool, &alias, &NO_MIRI).map(|line| line.join(" "))
    };
    let t = Tool::Targo;
    // `cargo-miri` pushes `miri` onto the directory (`get_target_dir`): the host's separator.
    let miri = |dir: &str| Path::new(dir).join("miri").display().to_string();
    let run_in_t = format!("--unverified run --target-dir {} demo", miri("t"));
    let test_in_a = format!(
        "--unverified test --target-dir {} -p x -- --target-dir u",
        miri("a")
    );
    let nextest_in_t = format!("--unverified nextest run --target-dir {} x", miri("t"));
    let configured_in_t = format!(
        "--unverified run --target-dir {} demo --config a.toml",
        miri("t")
    );
    for (typed, runs) in [
        ("--unverified b -m x", "build --release -m x"),
        // A nested alias's line keeps the typed globals alone: the `-v` the outer alias puts
        // before `xt` Targo drops with the inner `GlobalArgs` (`cli.rs`'s `expand_aliases`).
        ("-q outer -m x", "-q test --unverified -q -m x"),
        (
            "--unverified lab uname -m",
            "run -q -p aterm-lab -- uname -m",
        ),
        ("-- b -m x", "build --release -m x"),
        ("--unverified -- t x", "run -q -- x"),
        ("-C d b", "-C d build --release"),
        ("--config k=v b", "--config k=v build --release"),
        ("build -m x", "build -m x"),
        ("clippy -m x", "clippy -m x"),
        ("nextest run -m x", "nextest run -m x"),
        // A builtin alias no user alias takes is its command, read the same way: Targo
        // keeps only the global options before its name, so `-V c` checks.
        ("r x", "run x"),
        ("-V c", "check"),
        ("-vV --version --list --explain E0001 d -p x", "-v doc -p x"),
        (
            "-v -q --frozen --locked --offline --color always -Zx -vqZ y --config=k=v rm z",
            "-v -q --frozen --locked --offline --color always -Zx -vqZ y --config=k=v remove z",
        ),
        ("-qC d c", "-qC d check"),
        // A cluster keeps its global letters and its `-C` or `-Z`, whatever else it holds.
        ("-qVC d c", "-qC d check"),
        ("-VC d b", "-C d build --release"),
        ("-vVZx c", "-vZ=x check"),
        ("-V build", "-V build"),
        ("-- build", "build"),
        // A user alias of a builtin command's name makes Targo ignore it and the words after
        // the name a `--` hands over: it runs the command bare.
        ("-- check --release", "check"),
        ("-q -- check --target-dir t", "-q check"),
        // A protected tool's name so handed is an external command's, the words its own.
        ("-- clippy --target-dir t", "clippy --target-dir t"),
        ("-- tippy -q", "tippy -q"),
        // `miri` hands a verb back to Targo (`cargo-miri`'s `phase_cargo_miri`): with a help
        // option its words as typed, else its first `--target-dir` with `miri` joined first.
        (
            "--unverified miri run -m x --target-dir t demo -h",
            "--unverified run -m x --target-dir t demo -h",
        ),
        (
            "--unverified miri run demo --target-dir t",
            run_in_t.as_str(),
        ),
        (
            "miri test --target-dir=a --target-dir b -p x -- --target-dir u",
            test_in_a.as_str(),
        ),
        ("miri nextest run --target-dir t x", nextest_in_t.as_str()),
        (
            "miri run --target-dir= x",
            "--unverified run --target-dir miri x",
        ),
        // With no `--target-dir`, the target is `cargo metadata`'s, which reads every
        // `--config` before `--` (`cargo_extra_flags`): one past `run`'s program joins Targo's
        // own words, before the program; a typed target, and a `--config` with no value, leave
        // the program's words its own.
        (
            "miri run demo --config a.toml",
            "--unverified run --config a.toml demo --config a.toml",
        ),
        (
            "miri r --config=k=v demo --config=a.toml -- --config b",
            "run --config=k=v --config a.toml demo --config=a.toml -- --config b",
        ),
        (
            "miri test demo --config a.toml",
            "--unverified test demo --config a.toml",
        ),
        (
            "miri run --target-dir t demo --config a.toml",
            configured_in_t.as_str(),
        ),
        ("miri run demo --config", "--unverified run demo --config"),
        ("-v miri t -q", "run -q -- -q"),
        ("--config k=v -Zx miri run x", "--unverified run x"),
        ("-qC d miri run x", "-qC d --unverified run x"),
        ("-C d -v miri run x", "-C d --unverified run x"),
        ("miri setup", "miri setup"),
        ("--", "--"),
        ("", ""),
    ] {
        assert_eq!(line(t, typed).as_deref(), Some(runs), "{typed}");
    }
    assert_eq!(
        line(Tool::Cargo, "clippy -m x").as_deref(),
        Some("build -m x")
    );
    // A hand-back an alias turns into `miri` again is no line past the depth, as `cargo-miri`
    // execs Targo on it without end.
    let looping = |verb: &str| {
        if verb == "r" {
            UserAlias::Words(vec!["miri".to_owned(), "r".to_owned()])
        } else {
            UserAlias::None
        }
    };
    let args: Vec<String> = ["--unverified", "r", "-h"].map(str::to_owned).to_vec();
    assert_eq!(targo_line(&args, Tool::Targo, &looping, &NO_MIRI), None);
    assert_eq!(line(Tool::Tippy, "b -m x").as_deref(), Some("b -m x"));
    // Targo refuses any word after a builtin command it runs itself a `--` hands it where no
    // user alias of the name stands (`weird_check`).
    for typed in [
        "unknown -m x",
        "-v unknown",
        "again",
        "empty x",
        "-- -m x",
        "-- build --target-dir t",
    ] {
        assert_eq!(line(t, typed), None, "{typed}");
    }
    // The scope reads the line run.
    let read = |words: &str| {
        let run: Vec<String> = line(t, words)
            .unwrap()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        short_manifest_scope(&run, t)
            .into_iter()
            .zip(&run)
            .filter(|(reads, _)| *reads)
            .map(|(_, word)| word.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(read("b -m x -qm y"), ["--release", "-m", "-qm"]);
    assert_eq!(read("--unverified lab uname -m"), ["-q", "-p", "--"]);
    assert_eq!(read("-- t -m x"), ["-q", "--"]);
    assert_eq!(read("outer -m x"), ["--unverified", "-q", "-m"]);
}

/// MIRI HANDS BACK ITS METADATA TARGET, joined with `miri` (`get_target_dir`), where none is
/// typed, ahead of the words it forwards, as it puts it there (`phase_cargo_miri`), and says
/// where it sets up first (`setup.rs`): on `miri setup`, and on a verb it hands back where
/// `MIRI_SYSROOT` is unset; a call that inherits the lane (Targo's nested-unverified endpoint)
/// runs `miri` as one that types `--unverified` does, and `nextest` is handed back with its
/// target too. NEGATIVE CONTROL: a typed target, a help option, `setup` and `clean` never ask the
/// metadata target; a call in no lane, which Targo refuses before `targo-miri` runs, keeps the
/// line the words alone give; `build` is handed back only under
/// `MIRI_BUILD_TEST_DEPS`; a target the reader cannot tell names none and says so.
#[test]
fn miri_hands_back_its_metadata_target_and_says_where_it_sets_up() {
    let alias = |verb: &str| match verb {
        "t" => UserAlias::Words(["run", "-q", "--"].map(str::to_owned).to_vec()),
        "nextest" => UserAlias::Words(vec!["test".to_owned()]),
        "unknown" => UserAlias::Unknown,
        _ => UserAlias::None,
    };
    let told = |_: &[String]| Some(PathBuf::from("/m"));
    let asked = |_: &[String]| -> Option<PathBuf> { panic!("the metadata target is not read") };
    let read = |typed: &str,
                env: &[(&str, &str)],
                metadata_target: &dyn Fn(&[String]) -> Option<PathBuf>| {
        let args: Vec<String> = typed.split_whitespace().map(str::to_owned).collect();
        let env = |name: &str| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        };
        let miri = Miri {
            env: &env,
            metadata_target,
        };
        let line = targo_line(&args, Tool::Targo, &alias, &miri).map(|line| line.join(" "));
        (line, miri_call(&args, Tool::Targo, &alias, &miri))
    };
    let call = |setup: bool, target_unread: bool, base: Option<&str>| MiriCall {
        setup,
        target_unread,
        base: base.map(str::to_owned),
    };
    let sysroot = [("MIRI_SYSROOT", "")];
    let deps = [("MIRI_BUILD_TEST_DEPS", "1")];
    let inherited = [(NESTED_UNVERIFIED_ENDPOINT, "endpoint")];
    #[expect(
        clippy::type_complexity,
        reason = "a row: the words typed, the variables set, the metadata target, the line, and \
                  what miri does besides"
    )]
    let rows: Vec<(
        &str,
        &[(&str, &str)],
        &dyn Fn(&[String]) -> Option<PathBuf>,
        Option<&str>,
        MiriCall,
    )> = vec![
        (
            "--unverified miri run x",
            &[],
            &told,
            Some("--unverified run --target-dir /m/miri x"),
            call(true, false, Some("/m")),
        ),
        // A `--config` past the program is the program's in the line Targo runs: only
        // `cargo metadata` reads it, and the metadata target says what it made of it.
        (
            "--unverified miri run demo --config a.toml -- --config b",
            &[],
            &told,
            Some("--unverified run --target-dir /m/miri demo --config a.toml -- --config b"),
            call(true, false, Some("/m")),
        ),
        (
            "-qC d --unverified miri test x",
            &sysroot,
            &told,
            Some("-qC d --unverified test --target-dir /m/miri x"),
            call(false, false, Some("/m")),
        ),
        // An alias of the verb handed back moves the flag as Targo expands it: past its `--`,
        // among the program's words.
        (
            "--unverified -v miri t -q",
            &[],
            &told,
            Some("run -q -- --target-dir /m/miri -q"),
            call(true, false, Some("/m")),
        ),
        (
            "--unverified miri run x",
            &[],
            &none_told,
            Some("--unverified run x"),
            call(true, true, None),
        ),
        (
            "--unverified miri run --target-dir t x",
            &[],
            &asked,
            Some("--unverified run --target-dir t/miri x"),
            call(true, false, Some("t")),
        ),
        (
            "--unverified miri run x -h",
            &[],
            &asked,
            Some("--unverified run x -h"),
            call(false, false, None),
        ),
        (
            "--unverified miri setup",
            &sysroot,
            &asked,
            Some("--unverified miri setup"),
            call(true, false, None),
        ),
        (
            "--unverified miri setup --print-sysroot",
            &[],
            &asked,
            Some("--unverified miri setup --print-sysroot"),
            call(true, false, None),
        ),
        (
            "--unverified miri setup -h",
            &[],
            &asked,
            Some("--unverified miri setup -h"),
            call(false, false, None),
        ),
        (
            "--unverified miri clean",
            &[],
            &asked,
            Some("--unverified miri clean"),
            call(false, false, None),
        ),
        // `nextest` is handed back with its target as the other verbs are: a user alias of it
        // Targo expands (`nextest = "test"`) builds into that target.
        (
            "--unverified miri nextest run x",
            &[],
            &told,
            Some("test run --target-dir /m/miri x"),
            call(true, false, Some("/m")),
        ),
        (
            "--unverified miri build x",
            &[],
            &asked,
            Some("--unverified miri build x"),
            call(false, false, None),
        ),
        (
            "--unverified miri build x",
            &deps,
            &told,
            Some("--unverified build --target-dir /m/miri x"),
            call(true, false, Some("/m")),
        ),
        (
            "miri run x",
            &[],
            &asked,
            Some("--unverified run x"),
            call(false, false, None),
        ),
        (
            "miri setup",
            &[],
            &asked,
            Some("miri setup"),
            call(false, false, None),
        ),
        // An alias the reader cannot tell may run `miri`, and set up, on a call that consents.
        (
            "--unverified unknown x",
            &[],
            &asked,
            None,
            call(true, false, None),
        ),
        ("unknown x", &[], &asked, None, call(false, false, None)),
        // A Targo's child inherits its unverified lane (`TRUST_TARGO_NESTED_UNVERIFIED_ENDPOINT`,
        // which Targo authenticates and the reader cannot): Targo runs `miri` with none typed.
        (
            "miri run x",
            &inherited,
            &told,
            Some("--unverified run --target-dir /m/miri x"),
            call(true, false, Some("/m")),
        ),
        (
            "miri setup",
            &inherited,
            &asked,
            Some("miri setup"),
            call(true, false, None),
        ),
        (
            "unknown x",
            &inherited,
            &asked,
            None,
            call(true, false, None),
        ),
    ];
    // A row spells the `miri` `cargo-miri` pushes onto the directory (`get_target_dir`) after a
    // `/`; the line holds the host's separator there, as `targo_runs_the_line_its_aliases_expand_to`
    // spells it.
    let host = |line: &str| line.replace("/miri", &format!("{}miri", std::path::MAIN_SEPARATOR));
    for (typed, env, metadata_target, runs, does) in rows {
        assert_eq!(
            read(typed, env, metadata_target),
            (runs.map(host), does),
            "{typed} {env:?}"
        );
    }
}

/// THE METADATA TARGET `cargo-miri` READS (`get_cargo_metadata`: `targo metadata --no-deps`, with
/// every `--config` and the first `--manifest-path` before `--`, where Targo works) is Targo's
/// target for that workspace (`GlobalContext::target_dir`, `Workspace::target_dir`):
/// `CARGO_TARGET_DIR` (or its twin) against the working directory, else `build.target-dir`
/// (`CARGO_BUILD_TARGET_DIR`, then the configs), else the workspace root's `target`, the root
/// found as `find_root` finds it. NEGATIVE CONTROL: a member a root excludes is its own root, a
/// `members` entry cancelling that; `-m` is no flag `cargo-miri` forwards; a pointer met first
/// wins over a farther root; the walk stops at `target/package` and after the package home, and
/// goes on past HOME; `Targo.toml` is the working directory's manifest before `Cargo.toml`; a
/// manifest holding a time without seconds (TOML 1.1) is read, as Targo reads it, where a
/// reading that refuses one tells none. A `--config` that names a target, a config or manifest
/// this reader cannot read, an empty
/// target, `__CARGO_TEST_ROOT`, and a `--manifest-path` Targo refuses tell none.
#[test]
fn the_metadata_target_is_the_one_targo_metadata_names() {
    let w = Path::new("/w");
    let home = w.join("home");
    let root = home.join("root");
    let base: Vec<(PathBuf, &str)> = vec![
        (
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"member\"]\nexclude = [\"away\"]\n",
        ),
        (root.join("member/Cargo.toml"), "[package]\nname = \"m\"\n"),
        (root.join("away/Cargo.toml"), "[package]\nname = \"a\"\n"),
        (
            home.join("other/Cargo.toml"),
            "[workspace]\nmembers = [\"../p\", \"../root/mid/inner/x\"]\n",
        ),
        (
            home.join("p/Cargo.toml"),
            "[package]\nname = \"p\"\nworkspace = \"../other\"\n",
        ),
        (
            root.join("target/package/q/Cargo.toml"),
            "[package]\nname = \"q\"\n",
        ),
        (
            root.join("ch/reg/r/Cargo.toml"),
            "[package]\nname = \"r\"\n",
        ),
        (
            home.join("both/Targo.toml"),
            "[package]\nname = \"b\"\nworkspace = \"../other\"\n",
        ),
        (home.join("both/Cargo.toml"), "[package]\nname = \"b\"\n"),
        (
            root.join("mid/Cargo.toml"),
            "[package]\nname = \"mid\"\nworkspace = \"../../other\"\n",
        ),
        (
            root.join("mid/inner/x/Cargo.toml"),
            "[package]\nname = \"x\"\n",
        ),
        (home.join("solo/Cargo.toml"), "[package]\nname = \"s\"\n"),
    ];
    let target = |files: &[(PathBuf, &str)],
                  cwd: &Path,
                  env: &[(&str, &str)],
                  words: &str|
     -> Option<PathBuf> {
        let files: BTreeMap<PathBuf, String> = base
            .iter()
            .chain(files)
            .map(|(path, text)| (path.clone(), (*text).to_owned()))
            .collect();
        let read = |path: &Path| Ok(files.get(path).cloned());
        let env = |name: &str| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(cwd),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| None,
        };
        let words: Vec<String> = words.split_whitespace().map(str::to_owned).collect();
        miri_metadata_target(&words, Tool::Targo, &machine)
    };
    let member = root.join("member");
    let away = root.join("away");
    let none: &[(PathBuf, &str)] = &[];
    assert_eq!(
        target(none, &member, &[], "run x"),
        Some(root.join("target"))
    );
    assert_eq!(target(none, &away, &[], "run x"), Some(away.join("target")));
    let listed = [(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\", \"away\"]\nexclude = [\"away\"]\n",
    )];
    assert_eq!(
        target(&listed, &away, &[], "run"),
        Some(root.join("target"))
    );
    assert_eq!(
        target(none, &home.join("p"), &[], "run"),
        Some(home.join("other/target"))
    );
    for (name, value, at) in [
        ("CARGO_TARGET_DIR", "rel", member.join("rel")),
        ("TARGO_TARGET_DIR", "rel", member.join("rel")),
        ("CARGO_BUILD_TARGET_DIR", "b", member.join("b")),
        ("CARGO_TARGET_DIR", "/abs", PathBuf::from("/abs")),
    ] {
        assert_eq!(
            target(none, &member, &[(name, value)], "run"),
            Some(at),
            "{name}"
        );
    }
    // `CARGO_TARGET_DIR` outranks a `--config` target, which outranks `CARGO_BUILD_TARGET_DIR`.
    assert_eq!(
        target(
            none,
            &member,
            &[("CARGO_TARGET_DIR", "e")],
            "run --config build.target-dir='x'"
        ),
        Some(member.join("e"))
    );
    let config = [(root.join(".cargo/config.toml"), "build.target-dir = 't'\n")];
    assert_eq!(target(&config, &member, &[], "run"), Some(root.join("t")));
    assert_eq!(
        target(none, &member, &[], "run --manifest-path ../away/Cargo.toml"),
        Some(away.join("target"))
    );
    assert_eq!(
        target(
            none,
            &member,
            &[],
            "run x --manifest-path=../away/Cargo.toml"
        ),
        Some(away.join("target"))
    );
    assert_eq!(
        target(none, &member, &[], "run -m ../away/Cargo.toml"),
        Some(root.join("target"))
    );
    // Each flag is scanned apart: a `--config` whose value is spelled as a manifest flag is
    // both.
    assert_eq!(
        target(
            none,
            &member,
            &[],
            "run --config --manifest-path=../away/Cargo.toml"
        ),
        None
    );
    assert_eq!(
        target(none, &member, &[], "run demo --config build.target-dir='x'"),
        None
    );
    assert_eq!(
        target(none, &member, &[], "run --config build.jobs=2"),
        Some(root.join("target"))
    );
    assert_eq!(
        target(none, &member, &[], "run -- --config build.target-dir='x'"),
        Some(root.join("target"))
    );
    // Manifests are read as Targo parses them, as TOML 1.1 (`parse_document`, `toml` 1.1.5): a
    // metadata time without seconds, in the root's and the member's, is read, the root told.
    let seconds_less = [
        (
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"member\"]\n[workspace.metadata]\nat = 07:32\n",
        ),
        (
            root.join("member/Cargo.toml"),
            "[package]\nname = \"m\"\n[package.metadata]\nat = 1979-05-27T07:32Z\n",
        ),
    ];
    assert_eq!(
        target(&seconds_less, &member, &[], "run"),
        Some(root.join("target"))
    );
    let broken = [(home.join("Cargo.toml"), "[")];
    assert_eq!(target(&broken, &home.join("solo"), &[], "run"), None);
    let include = [(
        root.join(".cargo/config.toml"),
        "include = ['missing.toml']\n",
    )];
    assert_eq!(target(&include, &member, &[], "run"), None);
    assert_eq!(
        target(none, &member, &[("CARGO_TARGET_DIR", "")], "run"),
        None
    );
    assert_eq!(
        target(none, &member, &[("CARGO_BUILD_TARGET_DIR", "")], "run"),
        None
    );
    assert_eq!(
        target(none, &member, &[("__CARGO_TEST_ROOT", "/w")], "run"),
        None
    );
    let packaged = root.join("target/package/q");
    assert_eq!(
        target(none, &packaged, &[], "run"),
        Some(packaged.join("target"))
    );
    let registry = root.join("ch/reg/r");
    for cargo_home in ["/w/home/root/ch", "../.."] {
        assert_eq!(
            target(none, &registry, &[("CARGO_HOME", cargo_home)], "run"),
            Some(registry.join("target")),
            "{cargo_home}"
        );
    }
    let registered = [(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\", \"ch/reg/r\"]\nexclude = [\"away\"]\n",
    )];
    assert_eq!(
        target(&registered, &registry, &[], "run"),
        Some(root.join("target"))
    );
    // A `TARGO_HOME` is the home Trust's main reads where `CARGO_HOME` is unset (Targo 16387
    // reads `~/.cargo`): the root walk stops after it for one build, not the other, so a
    // manifest under it tells no root, and a target its config names none, whichever build a
    // lock runs. NEGATIVE CONTROL: a set `CARGO_HOME` outranks it for both.
    let twin = [("TARGO_HOME", "/w/home/root/ch")];
    assert_eq!(target(&registered, &registry, &twin, "run"), None);
    let named = [(
        PathBuf::from("/w/twin/config.toml"),
        "build.target-dir = 'x'\n",
    )];
    assert_eq!(
        target(&named, &member, &[("TARGO_HOME", "/w/twin")], "run"),
        None
    );
    for both in [
        [
            ("CARGO_HOME", "/w/home/.cargo"),
            ("TARGO_HOME", "/w/home/root/ch"),
        ],
        [("CARGO_HOME", "/w/home/.cargo"), ("TARGO_HOME", "/w/twin")],
    ] {
        assert_eq!(
            target(&registered, &registry, &both, "run"),
            Some(root.join("target"))
        );
        assert_eq!(
            target(&named, &member, &both, "run"),
            Some(root.join("target"))
        );
    }
    // A package its workspace's root does not list, nor reaches through a path dependency, is
    // no member: Targo's metadata fails (`error_if_manifest_not_in_members`), building nothing.
    assert_eq!(target(none, &registry, &[], "run"), None);
    // `Targo.toml` is read first, and Targo's members are each folder's `Cargo.toml`, so a
    // `Targo.toml` that is not its own root is no member.
    assert_eq!(target(none, &home.join("both"), &[], "run"), None);
    assert_eq!(
        target(none, &root.join("mid/inner/x"), &[], "run"),
        Some(home.join("other/target"))
    );
    let unlisted = [(home.join("other/Cargo.toml"), "[workspace]\n")];
    assert_eq!(target(&unlisted, &home.join("p"), &[], "run"), None);
    assert_eq!(
        target(&unlisted, &root.join("mid/inner/x"), &[], "run"),
        None
    );
    // A pointer to a manifest that is no root, or a member the root excludes, is refused.
    let pointed = [(home.join("other/Cargo.toml"), "[package]\nname = \"o\"\n")];
    assert_eq!(target(&pointed, &home.join("p"), &[], "run"), None);
    let excluded = [(
        home.join("other/Cargo.toml"),
        "[workspace]\nexclude = [\"../p\"]\n",
    )];
    assert_eq!(target(&excluded, &home.join("p"), &[], "run"), None);
    let above = [(
        w.join("Cargo.toml"),
        "[workspace]\nmembers = [\"home/solo\"]\n",
    )];
    assert_eq!(
        target(&above, &home.join("solo"), &[], "run"),
        Some(w.join("target"))
    );
    let bare_above = [(w.join("Cargo.toml"), "[workspace]\n")];
    assert_eq!(target(&bare_above, &home.join("solo"), &[], "run"), None);
    // Members as Targo finds them (`find_members`): a `members` glob, one component per `*`, and
    // the path dependencies of the root and of each member, followed from one to the next, a
    // `workspace = true` one through the root's `[workspace.dependencies]`.
    let globbed = root.join("crates/c");
    let deep = root.join("crates/c/d");
    let depended = root.join("deps/a");
    let transitive = root.join("deps/b");
    let inherited = root.join("deps/i");
    let unreached = root.join("deps/u");
    let workspace = [
        (
            root.join("Cargo.toml"),
            "[package]\nname = \"root\"\n[workspace]\nmembers = [\"crates/*\"]\n\
             [workspace.dependencies]\ni = { path = \"deps/i\" }\n\
             [dependencies]\na = { path = \"deps/a\" }\n\
             [target.'cfg(unix)'.dev-dependencies]\ni = { workspace = true }\n",
        ),
        (globbed.join("Cargo.toml"), "[package]\nname = \"c\"\n"),
        (deep.join("Cargo.toml"), "[package]\nname = \"d\"\n"),
        (
            depended.join("Cargo.toml"),
            "[package]\nname = \"a\"\n[build-dependencies]\nb = { path = \"../b\" }\n",
        ),
        (transitive.join("Cargo.toml"), "[package]\nname = \"b\"\n"),
        (inherited.join("Cargo.toml"), "[package]\nname = \"i\"\n"),
        (unreached.join("Cargo.toml"), "[package]\nname = \"u\"\n"),
    ];
    for (cwd, told) in [
        (&globbed, true),
        (&depended, true),
        (&transitive, true),
        (&inherited, true),
        (&deep, false),
        (&unreached, false),
    ] {
        assert_eq!(
            target(&workspace, cwd, &[], "run"),
            told.then(|| root.join("target")),
            "{}",
            cwd.display()
        );
    }
    assert_eq!(
        target(none, &home.join("solo"), &[], "run"),
        Some(home.join("solo/target"))
    );
    for manifest in [
        "../missing/Cargo.toml",
        "../away/main.rs",
        "../../both/Targo.toml",
        "../away",
    ] {
        assert_eq!(
            target(
                none,
                &member,
                &[],
                &format!("run --manifest-path {manifest}")
            ),
            None,
            "{manifest}"
        );
    }
    // A manifest with neither a package nor a workspace, or with both keys, Targo refuses.
    let virtual_less = [(root.join("member/Cargo.toml"), "[dependencies]\n")];
    assert_eq!(target(&virtual_less, &member, &[], "run"), None);
    let both_keys = [(
        root.join("member/Cargo.toml"),
        "[package]\nname = \"m\"\nworkspace = \"..\"\n[workspace]\n",
    )];
    assert_eq!(target(&both_keys, &member, &[], "run"), None);
}

/// A `members` GLOB NAMES THE FOLDERS `glob` LISTS, and the root Targo finds holds each
/// package `find_members` reaches through them (laid on disk: a glob is listed there, whatever
/// reader the machine holds for its manifests). A folder a glob names that the root's
/// `exclude` takes is no member, its manifest never read (Targo returns before it loads one);
/// a trailing `**` names every folder below its base, never the base; a `*` matches a leading
/// `.`; a glob's folders are listed through their links; and a glob this reader cannot list (a
/// `[` class, a leading `.` component, past its listing bound) adds nothing, while a literal
/// entry or a path dependency still proves its member. Every package is read before a budget
/// stops the walk, the root's first. NEGATIVE CONTROL: a package reached only through a
/// member past the listing bound is not told. Unix: one folder is a symbolic link.
#[cfg(unix)]
#[test]
fn a_members_glob_names_the_folders_glob_lists() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(scratch.path()).unwrap();
    let home = base.join("home");
    let package = |dir: &Path, text: &str| {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), text).unwrap();
    };
    let named = |name: &str| format!("[package]\nname = \"{name}\"\n");
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    };
    let root_of = |dir: &Path| {
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(dir),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| None,
        };
        workspace_root(&dir.join("Cargo.toml"), Tool::Targo, &machine)
    };

    // An excluded glob folder with no manifest, which Targo never loads; the root's own path
    // dependency is its member.
    let w = home.join("excluded");
    package(
        &w,
        "[package]\nname = \"w\"\n[dependencies]\nb = { path = \"libs/b\" }\n\
         [workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/old\", \"crates/gone\"]\n",
    );
    package(&w.join("crates/a"), &named("a"));
    std::fs::create_dir_all(w.join("crates/old")).unwrap();
    package(
        &w.join("crates/gone"),
        "[package]\nname = \"g\"\n[dependencies]\no = { path = \"../../libs/o\" }\n",
    );
    package(&w.join("libs/b"), &named("b"));
    package(&w.join("libs/o"), &named("o"));
    assert_eq!(root_of(&w.join("libs/b")), Some(w.clone()), "excluded");
    assert_eq!(root_of(&w.join("crates/a")), Some(w.clone()), "excluded");
    // An excluded folder's own dependency is none: its manifest is never loaded.
    assert_eq!(root_of(&w.join("libs/o")), None, "excluded dependency");

    // A `[` class this reader does not list, beside a literal member and a root dependency.
    let w = home.join("class");
    package(
        &w,
        "[package]\nname = \"w\"\n[dependencies]\nr = { path = \"libs/r\" }\n\
         [workspace]\nmembers = [\"crates/[ab]*\", \"tools/x\"]\n",
    );
    package(&w.join("crates/a"), &named("a"));
    package(&w.join("tools/x"), &named("x"));
    package(&w.join("libs/r"), &named("r"));
    assert_eq!(
        root_of(&w.join("tools/x")),
        Some(w.clone()),
        "class literal"
    );
    assert_eq!(
        root_of(&w.join("libs/r")),
        Some(w.clone()),
        "class dependency"
    );

    // A trailing `**` names the folders below `crates`, not `crates` itself, which holds no
    // manifest: a glob member's path dependency is a member.
    let w = home.join("recursive");
    package(&w, "[workspace]\nmembers = [\"crates/**\"]\n");
    package(
        &w.join("crates/a"),
        "[package]\nname = \"a\"\n[lib]\npath = \"lib.rs\"\n\
         [dependencies]\nx = { path = \"../../libs/x\" }\n",
    );
    package(
        &w.join("crates/b"),
        "[package]\nname = \"b\"\n[lib]\npath = \"lib.rs\"\n",
    );
    package(
        &w.join("crates/b/inner"),
        "[package]\nname = \"inner\"\n[dependencies]\nv = { path = \"../../../libs/v\" }\n",
    );
    package(&w.join("libs/x"), &named("x"));
    package(&w.join("libs/v"), &named("v"));
    assert_eq!(root_of(&w.join("libs/x")), Some(w.clone()), "trailing **");
    assert_eq!(
        root_of(&w.join("libs/v")),
        Some(w.clone()),
        "** below a member"
    );
    // The base is no member, so a package there has its dependencies none.
    package(
        &w.join("crates"),
        "[package]\nname = \"base\"\n[lib]\npath = \"lib.rs\"\n\
         [dependencies]\nn = { path = \"../libs/n\" }\n",
    );
    package(&w.join("libs/n"), &named("n"));
    assert_eq!(
        root_of(&w.join("libs/n")),
        None,
        "the base of a trailing **"
    );

    // A `*` matches a leading `.`; a folder a glob names through a link is listed through it.
    let w = home.join("dots");
    package(&w, "[workspace]\nmembers = [\"crates/*\"]\n");
    package(
        &w.join("crates/.hidden"),
        "[package]\nname = \"h\"\n[dependencies]\nz = { path = \"../../libs/z\" }\n",
    );
    package(
        &home.join("elsewhere/l"),
        "[package]\nname = \"l\"\n[dependencies]\ny = { path = \"../../libs/y\" }\n",
    );
    std::os::unix::fs::symlink(home.join("elsewhere/l"), w.join("crates/l")).unwrap();
    package(&w.join("libs/z"), &named("z"));
    package(&w.join("libs/y"), &named("y"));
    assert_eq!(root_of(&w.join("crates/.hidden")), Some(w.clone()), "dot");
    assert_eq!(
        root_of(&w.join("libs/z")),
        Some(w.clone()),
        "dot dependency"
    );
    assert_eq!(root_of(&w.join("libs/y")), Some(w.clone()), "linked");

    // A leading `.` component, which `glob` also expands to `.` and `..`, is not listed here:
    // only a literal member proves one.
    let w = home.join("special");
    package(&w, "[workspace]\nmembers = [\"crates/.*\", \"tools/t\"]\n");
    package(
        &w.join("crates/.h"),
        "[package]\nname = \"h\"\n[dependencies]\nq = { path = \"../../libs/q\" }\n",
    );
    package(&w.join("crates"), &named("c"));
    package(&w.join("tools/t"), &named("t"));
    package(&w.join("libs/q"), &named("q"));
    assert_eq!(
        root_of(&w.join("crates/.h")),
        Some(w.clone()),
        "special match"
    );
    assert_eq!(
        root_of(&w.join("tools/t")),
        Some(w.clone()),
        "special literal"
    );
    assert_eq!(root_of(&w.join("libs/q")), None, "special unlisted");

    // Past the listing bound, a glob adds nothing: the root's dependency is still read first
    // and told, and a package reached only through a member that glob names is not.
    let w = home.join("bounded");
    package(
        &w,
        "[package]\nname = \"w\"\n[dependencies]\nr = { path = \"libs/r\" }\n\
         [workspace]\nmembers = [\"crates/**/pkg\"]\n",
    );
    for i in 0..300 {
        std::fs::create_dir_all(w.join(format!("crates/d{i}"))).unwrap();
    }
    package(
        &w.join("crates/d0/pkg"),
        "[package]\nname = \"p\"\n[dependencies]\nq = { path = \"../../../libs/q\" }\n",
    );
    package(&w.join("libs/r"), &named("r"));
    package(&w.join("libs/q"), &named("q"));
    assert_eq!(
        root_of(&w.join("crates/d0/pkg")),
        Some(w.clone()),
        "bounded match"
    );
    assert_eq!(
        root_of(&w.join("libs/r")),
        Some(w.clone()),
        "bounded root dependency"
    );
    assert_eq!(root_of(&w.join("libs/q")), None, "bounded unlisted");
    // More members than the read budget: the root is read first, so its dependency is told.
    let w = home.join("many");
    package(
        &w,
        "[package]\nname = \"w\"\n[dependencies]\nr = { path = \"libs/r\" }\n\
         [workspace]\nmembers = [\"crates/*\"]\n",
    );
    for i in 0..300 {
        package(&w.join(format!("crates/m{i}")), &named(&format!("m{i}")));
    }
    package(&w.join("libs/r"), &named("r"));
    assert_eq!(root_of(&w.join("libs/r")), Some(w.clone()), "many");
    // NEGATIVE CONTROL: within the bound, the same member's dependency is told.
    let w = home.join("bounded");
    for i in 1..300 {
        std::fs::remove_dir(w.join(format!("crates/d{i}"))).unwrap();
    }
    assert_eq!(
        root_of(&w.join("libs/q")),
        Some(w.clone()),
        "within the bound"
    );
}

/// TARGO ANSWERS HELP AND ITS VERSION WHERE CLAP READS THEM, and builds nothing: a help
/// option (or a cluster's `-h`) before the verb of any line it reads or among the final
/// builtin verb's own words, the `help` verb, a line with no verb, a version option (or a
/// cluster's `-V`), `--list` or `--explain` before the final verb, and the final parse's `-Z`
/// values all `help`; a protected tool where it answers its own words. NEGATIVE CONTROL: a
/// version, list or explain option before an alias's name, which Targo drops (`-V b`
/// builds), a typed `-Z help` before one (not in the final parse), a `-Z help` beside another
/// `-Z`, a help option after an alias's `--` (`xtask = "run -p xtask --"`), in a trailing
/// verb's program words (`miri run demo -h` among them) or an external command's, an
/// option's value, and a `help` that is no verb build.
#[test]
fn targo_answers_help_and_version_where_clap_reads_them() {
    let alias = |verb: &str| match verb {
        "xtask" => UserAlias::Words(["run", "-p", "xtask", "--"].map(str::to_owned).to_vec()),
        "xh" => UserAlias::Words(["build", "--help"].map(str::to_owned).to_vec()),
        "unknown" => UserAlias::Unknown,
        _ => UserAlias::None,
    };
    let query = |tool, words: &str| {
        let args: Vec<String> = words.split_whitespace().map(str::to_owned).collect();
        targo_reading(&args, tool, &alias, &NO_MIRI).map(|reading| reading.query)
    };
    let t = Tool::Targo;
    for typed in [
        "-h",
        "--help",
        "-V",
        "--version --verbose",
        "-vV",
        "-Vv",
        "help build",
        "--unverified build --help",
        "build -p x -h",
        "--unverified -V build",
        "-V nextest",
        "-h b",
        "--unverified xh",
        "-h xtask",
        "run --help",
        "-vV-not-a-query",
        "",
        "--unverified",
        "--list",
        "--explain E0499",
        "--explain=E0499 build",
        "-Z help",
        "-Zhelp build",
        "--unverified build -Zhelp",
        "-qV build",
        "-vh b",
        "--unverified build -rh",
        "-VC d build",
        "--list build",
        "-Z help build",
        "-Z help nextest",
        // A protected tool answers as it reads its own words.
        "tippy -vV",
        "clippy --version",
        "tippy --explain needless_return",
        "-- clippy --help",
        "trust",
        "trust -V",
        "trust help",
        "fmt --help",
        "--unverified miri --version",
        "--unverified miri setup --help",
        "--unverified miri run -h",
    ] {
        assert_eq!(query(t, typed), Some(true), "{typed}");
    }
    for typed in [
        "-V b",
        "--version c",
        "--unverified -vV xtask",
        "--unverified xtask --help",
        "--unverified run serve --help",
        "--unverified run -- --help",
        "--unverified test -- -h",
        "nextest run --help",
        "--unverified build -p help",
        "--color -vV build",
        "build",
        "--list b",
        "--explain E0499 xtask",
        "-Z help b",
        "-Z unstable-options -Z help build",
        "-qVC d b",
        // `miri` hands a help option to the Targo line it runs, where the program's is the
        // program's; it reads no `-Z`, and `trust check --help` is left to custody.
        "--unverified miri run demo -h",
        "--unverified miri r demo --help",
        "--unverified miri run demo -Z help",
        "--unverified miri run",
        "trust check --help",
        "-- tippy -q",
    ] {
        assert_eq!(query(t, typed), Some(false), "{typed}");
    }
    assert_eq!(query(t, "unknown --help"), None);
    assert_eq!(query(Tool::Tippy, "-V"), Some(true));
    assert_eq!(query(Tool::Tippy, "-vV"), Some(true));
    assert_eq!(query(Tool::Tippy, "--explain x"), Some(true));
    assert_eq!(query(Tool::Tippy, "x -- -h"), Some(false));
}

/// A `-C` BEFORE THE VERB NAMES TARGO'S DIRECTORY in each spelling clap reads, past the
/// values before it (a `-Z` takes no word that starts with `-`); one after the verb or `--`
/// is none of Targo's, and one whose value is missing names none, as does a second one before
/// the verb, which clap refuses. NEGATIVE CONTROL: in `-pC` the `C` is `-p`'s value.
#[test]
fn a_directory_before_the_verb_is_targos() {
    let dir = |line: &str| {
        let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        directory_arg(&args).map(|dir| dir.map(str::to_owned))
    };
    let named = Some(Some("d".to_owned()));
    for line in [
        "-C d build",
        "-Cd build",
        "-C=d build",
        "-qC d build",
        "-Z x -C d build",
        "-Z -C d build",
        "--config c -C d build",
        "+stable -C d build",
    ] {
        assert_eq!(dir(line), named, "{line}");
    }
    assert_eq!(dir("-C -q build"), Some(None));
    assert_eq!(dir("-C"), Some(None));
    // A second `-C` before the verb is one clap's Set action refuses, the same directory twice
    // too: it names none.
    for line in ["-C d -C e build", "-Cd -qC=e build", "-C d -C d build"] {
        assert_eq!(dir(line), Some(None), "{line}");
    }
    for line in ["build -C d", "-- -C d", "-pC build", "-Z -C"] {
        assert_eq!(dir(line).flatten(), None, "{line}");
    }
}

/// AN ALIAS'S OWN DIRECTORY IS NONE, A TYPED ONE THE CALLER'S. Targo changes directory only from
/// the `-C` typed before the verb (`cli.rs` reads `directory` once, before it expands an alias):
/// a `-C` an alias puts before its verb, or one typed after the name of an alias whose words
/// name no verb, it parses and ignores, so the target is the launcher's. A `-C` typed before the
/// verb names a directory only the caller resolves, so [`selected_outputs`] grants none for it.
/// An alias whose words, with those typed after its name, hold a `-C` clap refuses when it parses
/// them again (a second one, one with no value) is a line the reader does not tell: Targo builds
/// nothing, and the call is kept as wary as one whose alias is unknown ([`untold_lines`]).
/// NEGATIVE CONTROL: a typed `-C`, alone or in a cluster, grants none; read as the alias's, the
/// strip would grant the launcher's `t` for each. A `-C` after the verb, which clap refuses,
/// grants none either (`Line::read` refuses it, with or without the typed one's rule). A refused
/// alias `-C`, nested a level too, grants none; stripped as a well-formed one is, it would grant
/// `t`.
#[test]
fn an_aliases_own_directory_is_none_and_a_typed_one_is_the_callers() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let cwd = home.join("project");
    let manifest = cwd.join("Cargo.toml");
    let read = |path: &Path| Ok((path == manifest).then(|| "[workspace]".to_owned()));
    let machine = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let words =
        |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_owned).collect() };
    let alias = |verb: &str| match verb {
        "ob" => UserAlias::Words(words("-C ../elsewhere build")),
        "pre" => UserAlias::Words(words("--locked")),
        "twice" => UserAlias::Words(words("-C ../a -C ../b build")),
        "same" => UserAlias::Words(words("-C ../a -qC../a build")),
        "bare" => UserAlias::Words(words("-C= build")),
        "dashed" => UserAlias::Words(words("-C -q build")),
        "empty" => UserAlias::Words(vec!["-C".to_owned(), String::new(), "build".to_owned()]),
        "outer" => UserAlias::Words(words("-C ../a -C ../b inner")),
        "inner" => UserAlias::Words(words("build")),
        _ => UserAlias::None,
    };
    let chosen = Some(SelectedOutputs {
        target: FinalTarget::Explicit(cwd.join("t")),
        manifests: vec![manifest.clone()],
        within: None,
        intermediates: None,
    });
    for line in [
        "--unverified ob --target-dir t",
        "--unverified pre -C ../elsewhere build --target-dir t",
    ] {
        assert_eq!(
            selected_outputs(&words(line), Tool::Targo, &machine, &alias),
            chosen,
            "{line}"
        );
    }
    for line in [
        "-Zunstable-options -C ../elsewhere --unverified build --target-dir t",
        "-qC../elsewhere --unverified build --target-dir t",
        "--unverified build --target-dir t -C x",
    ] {
        assert_eq!(
            selected_outputs(&words(line), Tool::Targo, &machine, &alias),
            None,
            "{line}"
        );
    }
    for line in [
        "--unverified twice --target-dir t",
        "--unverified same --target-dir t",
        "--unverified bare --target-dir t",
        "--unverified dashed --target-dir t",
        "--unverified empty --target-dir t",
        "--unverified outer --target-dir t",
        "--unverified pre -C ../a -C ../b build --target-dir t",
        "--unverified pre -C= build --target-dir t",
    ] {
        let args = words(line);
        assert_eq!(
            selected_outputs(&args, Tool::Targo, &machine, &alias),
            None,
            "{line}"
        );
        assert_eq!(
            targo_line(&args, Tool::Targo, &alias, &NO_MIRI),
            None,
            "{line}"
        );
        assert!(
            untold_lines(&args, Tool::Targo, &alias, &NO_MIRI).is_some(),
            "{line}"
        );
    }
}

/// TARGO MERGES THE `--config` LIST CLAP KEEPS (`cli.rs`'s `configure_gctx`, clap's
/// `fill_in_global_values`), ON THE WORDS IT RUNS: by Targo's name the hop's, every `--config`
/// before the verb moved behind it where the words after it hold one ([`configs_after_subcommand`]).
/// Targo merges the typed globals an alias expanded past, then the final parse's list at its
/// deepest level: the verb's own `--config`s replace those before it, so the line keeps none of
/// the final parse before such a verb; one the hop moves past an alias's `--` is its program's;
/// a nested alias's own `--config`s stay in order with the moved ones. NEGATIVE CONTROLS: a verb
/// with no `--config` of its own keeps those before it (the alias's, as a help call reads it too),
/// and the typed globals an alias expands past are kept ahead of its own.
#[test]
fn targo_keeps_the_config_list_clap_keeps() {
    let aliases = [
        ("pre", "--config build.target-dir='a' build"),
        ("own", "build --config build.target-dir='a'"),
        ("xtask", "run --package xtask --"),
        ("xo", "--offline"),
        ("nest", "b --config build.target-dir='a'"),
    ];
    let alias = |verb: &str| {
        aliases
            .iter()
            .find(|(name, _)| *name == verb)
            .map_or(UserAlias::None, |(_, words)| {
                UserAlias::Words(words.split_whitespace().map(str::to_owned).collect())
            })
    };
    let args =
        |words: &str| -> Vec<String> { words.split_whitespace().map(str::to_owned).collect() };
    let line = |tool, words: &str| {
        targo_line(&args(words), tool, &alias, &NO_MIRI).map(|line| line.join(" "))
    };
    let t = "--config build.target-dir='t'";
    let x = "--config net.offline=true";
    for (tool, typed, runs) in [
        // A typed line: by a Cargo name as typed, by Targo's through the hop.
        (Tool::Cargo, format!("{t} build {x}"), format!("build {x}")),
        (
            Tool::CargoCompat,
            format!("{t} build {x}"),
            format!("build {x}"),
        ),
        (
            Tool::Targo,
            format!("{t} --unverified build {x}"),
            format!("--unverified build {t} {x}"),
        ),
        (Tool::Cargo, format!("{t} build"), format!("{t} build")),
        (
            Tool::Targo,
            format!("{t} --unverified build"),
            format!("{t} --unverified build"),
        ),
        // An alias's `--config` before its verb: replaced by the verb's own, else kept.
        (
            Tool::Targo,
            format!("--unverified pre {x}"),
            format!("build {x}"),
        ),
        (Tool::CargoCompat, format!("pre {x}"), format!("build {x}")),
        (
            Tool::Targo,
            "--unverified pre".to_owned(),
            "--config build.target-dir='a' build".to_owned(),
        ),
        // The typed globals ahead of an alias's own, unless the hop moves them behind it.
        (
            Tool::Targo,
            format!("{x} --unverified own"),
            format!("{x} build --config build.target-dir='a'"),
        ),
        (
            Tool::Targo,
            format!("{t} --unverified own {x}"),
            format!("build --config build.target-dir='a' {t} {x}"),
        ),
        // Moved past an alias's `--`, the program's; behind a verb-less alias, before the verb.
        (
            Tool::Targo,
            format!("{t} --unverified xtask dist {x}"),
            format!("run --package xtask -- {t} dist {x}"),
        ),
        (
            Tool::Targo,
            format!("{t} --unverified xo build {x}"),
            format!("--offline build {x}"),
        ),
        // A nested alias's own `--config`, then the moved and the typed ones.
        (
            Tool::Targo,
            format!("{t} --unverified nest {x}"),
            format!("build --config build.target-dir='a' {t} {x}"),
        ),
    ] {
        assert_eq!(
            line(tool, &typed).as_deref(),
            Some(runs.as_str()),
            "{tool:?} {typed}"
        );
    }
    // A help call through an alias with a `--config` before its verb is read, as a query.
    let help = args("--unverified pre --help");
    assert!(
        targo_reading(&help, Tool::Targo, &alias, &NO_MIRI).is_some_and(|reading| reading.query)
    );
}

/// A `-C` BEFORE THE VERB IS HONOURED ONLY BEHIND ITS GATE (`cli.rs`'s `main`, measured on Trust
/// 16387): on a channel that allows nightly features (Trust's own, a `+nightly` toolchain's,
/// or any under `RUSTC_BOOTSTRAP=1`), where the `-Z` values clap keeps hold `unstable-options`.
/// Those are a builtin verb's own where it has any (a global option's values after the verb
/// replace those before it), else those before the verb, each cluster read with the verb's own
/// value-less letters (`install`'s `-f` is a flag, `tree`'s takes the rest of the word); after an
/// alias's or an external command's name nothing is parsed at the gate. NEGATIVE CONTROL: each
/// refused row is honoured once the gate is read from every `-Z` typed, or from no channel.
#[test]
fn a_directory_is_honoured_only_behind_its_gate() {
    let words =
        |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_owned).collect() };
    let honoured = |tool: Tool, env: &[(&str, &str)], line: &str| {
        let env = |name: &str| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        };
        let machine = Machine {
            home: None,
            root: None,
            cwd: None,
            env: &env,
            read: &|_| Ok(None),
            read_manifest: &|_| Ok(None),
            writable: &|_| None,
        };
        directory_honoured(&words(line), tool, &machine)
    };
    let bootstrap = [("RUSTC_BOOTSTRAP", "1")];
    for (tool, env, line) in [
        (Tool::Targo, &[][..], "-Zunstable-options -C d build"),
        (Tool::Targo, &[], "-Z unstable-options -C d build"),
        (Tool::Targo, &[], "-Z=unstable-options -C d build"),
        (Tool::Targo, &[], "-qZunstable-options -C d build"),
        (
            Tool::Targo,
            &[],
            "-Zno-index-update -Zunstable-options -C d build",
        ),
        (Tool::Targo, &[], "-C d build -Zunstable-options"),
        (Tool::Targo, &[], "-C d build -j -1 -Zunstable-options"),
        (Tool::Targo, &[], "-Zunstable-options -C d b"),
        (Tool::Cargo, &[], "+nightly -Zunstable-options -C d build"),
        (
            Tool::Cargo,
            &[],
            "+nightly-2026-10-01 -Zunstable-options -C d build",
        ),
        (
            Tool::Cargo,
            &bootstrap,
            "+stable -Zunstable-options -C d build",
        ),
        (Tool::Cargo, &bootstrap, "-Zunstable-options -C d build"),
        (
            Tool::CargoCompat,
            &[],
            "+trust -Zunstable-options -C d build",
        ),
        (
            Tool::CargoCompat,
            &[],
            "+trust -C d build -Zunstable-options",
        ),
        (Tool::CargoCompat, &[], "-Zunstable-options -C d build"),
        (
            Tool::Targo,
            &[],
            "-C d install -fZunstable-options --path .",
        ),
        (
            Tool::Targo,
            &[],
            "-C d install -qfZunstable-options --path .",
        ),
    ] {
        assert!(honoured(tool, env, line), "{tool:?} {env:?} {line}");
    }
    let stable = [("__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS", "stable")];
    for (tool, env, line) in [
        (Tool::Targo, &[][..], "-C d build"),
        (Tool::Targo, &[], "-Zno-index-update -C d build"),
        (
            Tool::Targo,
            &[],
            "-Zunstable-options -C d build -Zno-index-update",
        ),
        (Tool::Targo, &[], "-C d b -Zunstable-options"),
        (Tool::Targo, &[], "-C d run prog -Zunstable-options"),
        (Tool::Targo, &[], "-C d build -- -Zunstable-options"),
        (Tool::Targo, &[], "-C d build --jobs -Zunstable-options"),
        (Tool::Targo, &[], "-C d tree -fZunstable-options"),
        (Tool::Targo, &stable, "-Zunstable-options -C d build"),
        (Tool::Cargo, &[], "+stable -Zunstable-options -C d build"),
        (Tool::Cargo, &[], "+beta -Zunstable-options -C d build"),
        (Tool::Cargo, &[], "-Zunstable-options -C d build"),
        (
            Tool::Cargo,
            &[("RUSTC_BOOTSTRAP", "0")],
            "+stable -Zunstable-options -C d build",
        ),
        (Tool::CargoCompat, &[], "+trust -C d build"),
        (
            Tool::CargoCompat,
            &stable,
            "+trust -Zunstable-options -C d build",
        ),
    ] {
        assert!(!honoured(tool, env, line), "{tool:?} {env:?} {line}");
    }
}

/// A `-C` IS REFUSED ONLY WHERE THE PROGRAM SURELY REFUSES IT: its gate is not kept, or its
/// channel allows no nightly feature (`+stable`, `+beta`, a release number, a stable override).
/// An upstream cargo whose channel is untold (`+trust` by the cargo name, or no `+toolchain`)
/// checks the gate before it changes directory or reads anything, so it refuses or runs from
/// the directory: it is not refused, though it is not honoured either, and so is one whose
/// `+toolchain` is a custom name rustup may link to a nightly cargo (`+1dev`, `+stable-mine`),
/// or an official one this reader does not tell (a host triple after the channel). NEGATIVE
/// CONTROL: the same untold channels with no gate are refused, and a nightly one is honoured.
#[test]
fn a_directory_is_refused_only_where_its_program_refuses_it() {
    let words =
        |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_owned).collect() };
    let read = |env: &[(&str, &str)], line: &str| {
        let env = |name: &str| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        };
        let machine = Machine {
            home: None,
            root: None,
            cwd: None,
            env: &env,
            read: &|_| Ok(None),
            read_manifest: &|_| Ok(None),
            writable: &|_| None,
        };
        (
            directory_refused(&words(line), Tool::Cargo, &machine),
            directory_honoured(&words(line), Tool::Cargo, &machine),
        )
    };
    let stable = [("__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS", "stable")];
    for (env, line, refused, honoured) in [
        (
            &[][..],
            "+trust -Zunstable-options -C d build",
            false,
            false,
        ),
        (&[], "-Zunstable-options -C d build", false, false),
        (&[], "+nightly -Zunstable-options -C d build", false, true),
        (&[], "+trust -C d build", true, false),
        (&[], "-C d build", true, false),
        (&[], "+stable -Zunstable-options -C d build", true, false),
        (
            &[],
            "+beta-2026-10-01 -Zunstable-options -C d build",
            true,
            false,
        ),
        (&[], "+1.91.0 -Zunstable-options -C d build", true, false),
        (&stable, "+trust -Zunstable-options -C d build", true, false),
        (&[], "+1dev -Zunstable-options -C d build", false, false),
        (
            &[],
            "+stable-mine -Zunstable-options -C d build",
            false,
            false,
        ),
        (&[], "+beta-x -Zunstable-options -C d build", false, false),
        (
            &[],
            "+stable-aarch64-apple-darwin -Zunstable-options -C d build",
            false,
            false,
        ),
        (&[], "+1dev -C d build", true, false),
    ] {
        assert_eq!(read(env, line), (refused, honoured), "{env:?} {line}");
    }
}

/// A RELEASED TOOLCHAIN IS ONE RUSTUP NEVER LINKS A CUSTOM BUILD AS: each name counted is one
/// `rustup toolchain link` refused as "invalid custom toolchain name" (rustup 1.29.0, measured
/// in a scratch `RUSTUP_HOME`), so its cargo is the released channel's. NEGATIVE CONTROL: each
/// name that rustup linked (a leading digit alone, a leading zero, a part too long, a word
/// after the channel) is not counted, nor is an official name with a host triple after it, nor
/// a nightly one.
#[test]
fn a_released_toolchain_is_one_rustup_never_links_a_custom_build_as() {
    for name in [
        "stable",
        "beta",
        "stable-2026-10-01",
        "beta-2026-10-01",
        "stable-0000-00-00",
        "0.1",
        "1.0",
        "1.91",
        "1.91.0",
        "1.0.0",
        "1.10.10",
        "1.100.0",
        "9.999.99",
        "1.91-beta",
        "1.91.0-beta",
        "1.91.0-beta.0",
        "1.91.0-beta.2",
        "1.91.0-beta.12",
        "1.91.0-2026-10-01",
        "1.91.0-beta-2026-10-01",
        "1.91-beta.1-2026-10-01",
    ] {
        assert!(released_toolchain(name), "{name}");
    }
    for name in [
        "1dev",
        "1",
        "1.x",
        "12.0",
        "01.91",
        "1.09",
        "1.091",
        "1.1000",
        "1.91.00",
        "1.91.05",
        "1.91.123",
        "1.91.0.1",
        "1.91.0-mine",
        "1.91.0-beta.",
        "1.91.0-beta.01",
        "1.91.0-beta.123",
        "1.91.0-beta.2-mine",
        "1.91.0-2026-10-01-beta",
        "stable-mine",
        "beta-x",
        "beta.2",
        "Stable",
        "stable-2026",
        "stable-2026-10-1",
        "stable-2026-10-01-2026-10-01",
        "beta-2026-10-01-mine",
        "stable-aarch64-apple-darwin",
        "1.91-aarch64-apple-darwin",
        "nightly",
        "nightly-2026-10-01",
        "",
    ] {
        assert!(!released_toolchain(name), "{name}");
    }
}

/// `f` on a machine whose environment is `env`, its values spelled as this platform spells
/// an absolute path ([`native`]), in `cwd`, `home` being `/h`.
fn with_env<R>(env: &[(&str, &str)], cwd: Option<&Path>, f: impl FnOnce(&Machine<'_>) -> R) -> R {
    let env: BTreeMap<String, OsString> = env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OsString::from(native(v))))
        .collect();
    let lookup = |k: &str| env.get(k).cloned();
    let home = PathBuf::from(native("/h"));
    f(&Machine {
        home: Some(&home),
        root: None,
        cwd,
        env: &lookup,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| Some(true),
    })
}

/// TARGO READS EACH TWIN WHERE ITS CARGO SPELLING IS UNSET: `TARGO_<X>` stands for an unset
/// `CARGO_<X>`, a set one wins even empty, and the package homes add a non-empty `TARGO_HOME`
/// beside `~/.cargo` where `CARGO_HOME` is unset (Targo 16387 reads only the first,
/// Trust's main only the second, so both are scanned). NEGATIVE CONTROL: an upstream cargo
/// reads no twin, and a name
/// outside `CARGO_` has none.
#[test]
fn targo_reads_each_twin_where_its_cargo_spelling_is_unset() {
    let var = |env: &[(&str, &str)], tool| {
        with_env(env, None, |m| cargo_var("CARGO_TARGET_DIR", tool, m))
            .map(|value| value.into_string().unwrap())
    };
    let t = Some(native("/t"));
    assert_eq!(var(&[("TARGO_TARGET_DIR", "/t")], Tool::Targo), t);
    assert_eq!(var(&[("TARGO_TARGET_DIR", "/t")], Tool::Tippy), t);
    assert_eq!(var(&[("TARGO_TARGET_DIR", "/t")], Tool::CargoCompat), t);
    assert_eq!(var(&[("TARGO_TARGET_DIR", "/t")], Tool::Cargo), None);
    let both = [("CARGO_TARGET_DIR", "/c"), ("TARGO_TARGET_DIR", "/t")];
    assert_eq!(var(&both, Tool::Targo), Some(native("/c")));
    assert_eq!(var(&both, Tool::CargoCompat), Some(native("/c")));
    let empty = [("CARGO_TARGET_DIR", ""), ("TARGO_TARGET_DIR", "/t")];
    assert_eq!(var(&empty, Tool::Targo), Some(String::new()));
    assert_eq!(var(&empty, Tool::CargoCompat), Some(String::new()));
    assert_eq!(
        with_env(&[("TARGO_RUSTC", "/r")], None, |m| cargo_var(
            "RUSTC",
            Tool::Targo,
            m
        )),
        None
    );

    let cwd = PathBuf::from(native("/w"));
    let homes = |env: &[(&str, &str)], tool| with_env(env, Some(&cwd), |m| package_homes(tool, m));
    let at = |path: &str| PathBuf::from(native(path));
    let th = [("TARGO_HOME", "/th")];
    assert_eq!(homes(&[], Tool::Targo), [at("/h/.cargo")]);
    assert_eq!(homes(&th, Tool::Targo), [at("/h/.cargo"), at("/th")]);
    assert_eq!(homes(&th, Tool::Tippy), [at("/h/.cargo"), at("/th")]);
    assert_eq!(homes(&th, Tool::CargoCompat), [at("/h/.cargo"), at("/th")]);
    assert_eq!(
        homes(&[("TARGO_HOME", "rel")], Tool::Targo),
        [at("/h/.cargo"), at("/w/rel")]
    );
    assert_eq!(homes(&[("CARGO_HOME", "/ch")], Tool::Targo), [at("/ch")]);
    assert_eq!(homes(&[("CARGO_HOME", "rel")], Tool::Cargo), [at("/w/rel")]);
    let hidden = [("CARGO_HOME", "/ch"), ("TARGO_HOME", "/th")];
    assert_eq!(homes(&hidden, Tool::Targo), [at("/ch")]);
    let emptied = [("CARGO_HOME", ""), ("TARGO_HOME", "/th")];
    assert_eq!(homes(&emptied, Tool::Targo), [at("/h/.cargo")]);
    assert_eq!(homes(&[("TARGO_HOME", "")], Tool::Targo), [at("/h/.cargo")]);
    assert_eq!(homes(&th, Tool::Cargo), [at("/h/.cargo")]);
    // Each home's `.` and `..` folded as written, as Targo's `homedir` folds its own
    // (`normalize_path`), never through a link the kernel would follow.
    let folded = [("CARGO_HOME", "/l/../ch/.")];
    assert_eq!(homes(&folded, Tool::Targo), [at("/ch")]);
    assert_eq!(homes(&[("CARGO_HOME", "../ch")], Tool::Cargo), [at("/ch")]);
    assert_eq!(
        homes(&[("TARGO_HOME", "/l/../th")], Tool::Targo),
        [at("/h/.cargo"), at("/th")]
    );
}

/// A PACKAGE HOME'S `..` IS FOLDED AS TARGO FOLDS IT, NEVER THROUGH A LINK: a relative
/// `CARGO_HOME` of `link/../h` is the working directory's `h` to Targo (`homedir` joins it to the
/// working directory, `normalize_path` folds it as written), whatever `link` points at, so the
/// target that home's config names is the one Targo builds into, and the one every reader of the
/// front takes (the target an explicit cache owns, the target hint, the target `cargo metadata`
/// names for a `miri` line). NEGATIVE CONTROL: the kernel's resolution, `link`'s target's
/// sibling `h`, names another target, which a reader that opened the home as written would take.
#[cfg(unix)]
#[test]
fn a_package_home_climbing_through_a_link_is_the_folded_one_targo_reads() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let cwd = home.join("project");
    std::fs::create_dir_all(cwd.join("z/y")).unwrap();
    std::fs::create_dir_all(cwd.join("z/h")).unwrap();
    std::fs::create_dir_all(cwd.join("h")).unwrap();
    std::os::unix::fs::symlink(cwd.join("z/y"), cwd.join("link")).unwrap();
    let folded = home.join("folded");
    let linked = home.join("linked");
    let named = |target: &Path| format!("build.target-dir = '{}'\n", target.display());
    std::fs::write(cwd.join("h/config.toml"), named(&folded)).unwrap();
    std::fs::write(cwd.join("z/h/config.toml"), named(&linked)).unwrap();
    std::fs::write(cwd.join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
    let env = |name: &str| (name == "CARGO_HOME").then(|| OsString::from("link/../h"));
    let machine = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &env,
        read: &read_config,
        read_manifest: &read_config,
        writable: &|_| Some(true),
    };
    let args = ["--unverified".into(), "build".into()];
    assert_eq!(
        selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None)
            .map(|c| c.target.explicit_path()),
        Some(folded.clone())
    );
    assert_eq!(
        miri_metadata_target(&["test".into()], Tool::Targo, &machine),
        Some(folded.clone())
    );
    for tool in [Tool::Targo, Tool::Cargo] {
        assert_eq!(
            target_hint(&machine, tool),
            Ok(Some(folded.clone())),
            "{tool:?}"
        );
        assert_eq!(package_homes(tool, &machine), [cwd.join("h")], "{tool:?}");
    }
    assert_eq!(cargo_home_hint_with(&machine), Ok(Some(cwd.join("h"))));
}

#[test]
fn caller_intermediates_follow_cargo_precedence_and_original_config_bases() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = home.join("project");
    let cwd = root.join("member");
    for (near, legacy, environment, custom_home, expected) in [
        (
            Some("build.build-dir='configured'"),
            None,
            None,
            false,
            root.join("configured"),
        ),
        (
            Some("build.build-dir='configured'"),
            Some("build.build-dir='legacy'"),
            None,
            false,
            root.join("legacy"),
        ),
        (
            Some("build.build-dir='configured'"),
            None,
            Some("environment"),
            false,
            cwd.join("environment"),
        ),
        (None, None, None, true, cwd.join("global")),
    ] {
        let env = |name: &str| match name {
            BUILD_DIR_ENV => environment.map(OsString::from),
            "CARGO_HOME" if custom_home => Some("tool-cache".into()),
            _ => None,
        };
        let read = |path: &Path| {
            Ok(if path == root.join(".cargo/config.toml") {
                near.map(str::to_owned)
            } else if path == root.join(".cargo/config") {
                legacy.map(str::to_owned)
            } else if custom_home && path == cwd.join("tool-cache/config.toml") {
                Some("build.build-dir='global'".into())
            } else {
                None
            })
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| None,
        };
        let hint = cargo_build_dir_hint_with(&machine).unwrap().unwrap();
        assert_eq!(hint.path(), Some(expected.as_path()));
        assert_eq!(hint.value(), expected.as_os_str());
        assert_eq!(
            cargo_home_hint_with(&machine).unwrap(),
            Some(if custom_home {
                cwd.join("tool-cache")
            } else {
                home.join(".cargo")
            })
        );
    }
}

/// The caller's build directory is read as Targo loads its configs ([`discovered_config_outputs`]):
/// an included file's value, resolved against that file's own `Definition::root`, and the
/// including file's own value over it; the package home is the one Targo folds
/// ([`cargo_home_hint_with`]). NEGATIVE CONTROL: an include that names nothing, though not
/// optional, refuses, and so does an included `build-dir` Targo cannot merge.
#[test]
fn caller_intermediates_follow_includes_and_a_folded_home() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = home.join("project");
    let cwd = root.join("member");
    let config = root.join(".cargo/config.toml");
    let layout = root.join(".cargo/sub/layout.toml");
    for (near, included, expected) in [
        (
            "include = ['sub/layout.toml']",
            Some("build.build-dir = 'out'"),
            Ok(Some(root.join(".cargo/out"))),
        ),
        (
            "include = ['sub/layout.toml']\nbuild.build-dir = 'mine'",
            Some("build.build-dir = 'out'"),
            Ok(Some(root.join("mine"))),
        ),
        ("include = ['sub/layout.toml']", None, Err(())),
        (
            "include = ['sub/layout.toml']\nbuild.build-dir = 'mine'",
            Some("build.build-dir = ['out']"),
            Err(()),
        ),
    ] {
        let read = |path: &Path| {
            Ok(if path == config {
                Some(near.to_owned())
            } else if path == layout {
                included.map(str::to_owned)
            } else {
                None
            })
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| None,
        };
        let hint = cargo_build_dir_hint_with(&machine)
            .map(|hint| hint.and_then(|hint| hint.path().map(Path::to_path_buf)))
            .map_err(|_| ());
        assert_eq!(hint, expected, "{near:?} {included:?}");
    }
    let folded = cwd.join("cache-home");
    let env = |name: &str| (name == "CARGO_HOME").then(|| OsString::from("tool/../cache-home"));
    let read = |path: &Path| {
        Ok((path == folded.join("config.toml")).then(|| "build.build-dir = 'global'".to_owned()))
    };
    let machine = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &env,
        read: &read,
        read_manifest: &read,
        writable: &|_| None,
    };
    assert_eq!(
        cargo_home_hint_with(&machine).unwrap(),
        Some(folded.clone())
    );
    assert_eq!(
        cargo_build_dir_hint_with(&machine)
            .unwrap()
            .and_then(|hint| hint.path().map(Path::to_path_buf)),
        Some(cwd.join("global"))
    );
}

#[test]
fn caller_intermediates_preserve_valid_templates_and_refuse_unresolved_choices() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    let cwd = root.join("member");
    for (raw, expected) in [
        (
            "{workspace-root}/build.noindex",
            PathBuf::from("{workspace-root}/build.noindex"),
        ),
        (
            "{cargo-cache-home}/build.noindex",
            PathBuf::from("{cargo-cache-home}/build.noindex"),
        ),
        (
            "cache/{workspace-path-hash}",
            root.join("cache/{workspace-path-hash}"),
        ),
        (
            "cache{workspace-root}/build.noindex",
            root.join("cache{workspace-root}/build.noindex"),
        ),
    ] {
        let text = format!("[build]\nbuild-dir='{raw}'\n");
        let read =
            |path: &Path| Ok((path == root.join(".cargo/config.toml")).then(|| text.clone()));
        let machine = Machine {
            home: None,
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| None,
        };
        let hint = cargo_build_dir_hint_with(&machine).unwrap().unwrap();
        assert_eq!(hint.value(), expected.as_os_str());
        assert!(hint.path().is_none(), "Cargo owns template expansion");
    }
    for text in [
        "build=1",
        "build.build-dir=3",
        "build.build-dir=''",
        "build.build-dir=[",
        "include=['layout.toml']",
        "build.build-dir='{unknown}'",
        "build.build-dir='cache{'",
        "build.build-dir='cache}'",
    ] {
        let read =
            |path: &Path| Ok((path == root.join(".cargo/config.toml")).then(|| text.to_owned()));
        let machine = Machine {
            home: None,
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| None,
        };
        assert!(cargo_build_dir_hint_with(&machine).is_err(), "{text}");
    }
    let machine = Machine {
        home: None,
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| None,
    };
    assert_eq!(cargo_build_dir_hint_with(&machine).unwrap(), None);
    assert!(
        cargo_build_dir_hint_with(&Machine {
            env: &|name| (name == BUILD_DIR_ENV).then(OsString::new),
            ..machine
        })
        .is_err()
    );
    assert!(
        cargo_build_dir_hint_with(&Machine {
            read: &|_| Err(()),
            read_manifest: &|_| Err(()),
            ..machine
        })
        .is_err()
    );
}

#[test]
fn global_and_command_configs_share_one_parser_without_moving_program_words() {
    let words = |s: &str| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(
        configs_after_subcommand(&words(
            "--config a=1 --color always --unverified test --config b=2"
        )),
        words("--color always --unverified test --config a=1 --config b=2")
    );
    assert_eq!(
        configs_after_subcommand(&words("--config=a=1 --unverified test --config b=2")),
        words("--unverified test --config=a=1 --config b=2")
    );
    for line in [
        "--config a=1 --unverified test -p x",
        "--config a=1 --unverified --config b=2 test -Zfeature-unification",
        "--config a=1",
        "--config a=1 --unverified run -- --config b=2",
        "--config a=1 trust check --config b=2",
    ] {
        assert_eq!(configs_after_subcommand(&words(line)), words(line));
    }
}

/// The test graph's `--config` goes right before the subcommand and its flag right after it,
/// past every flag and flag value before it, once. NEGATIVE CONTROL: a line naming no
/// subcommand before `--`, or one that chose its unification already, is left as it is.
#[test]
fn the_test_graph_wraps_the_subcommand() {
    let words = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let c = TEST_GRAPH_CONFIG.join(" ");
    let f = TEST_GRAPH_FLAG;
    for (line, want) in [
        (
            "--unverified test -p x",
            format!("--unverified {c} test {f} -p x"),
        ),
        (
            "--config build.jobs=2 --unverified build -p x",
            format!("--config build.jobs=2 --unverified {c} build {f} -p x"),
        ),
        (
            "-Z unstable-options --color always run -q -- -x",
            format!("-Z unstable-options --color always {c} run {f} -q -- -x"),
        ),
        (
            "+trust --manifest-path m/Cargo.toml metadata --no-deps",
            format!("+trust --manifest-path m/Cargo.toml {c} metadata {f} --no-deps"),
        ),
        (
            "test -- -Zfeature-unification",
            format!("{c} test {f} -- -Zfeature-unification"),
        ),
    ] {
        assert_eq!(with_test_graph(&words(line)), words(&want), "{line}");
    }
    for line in [
        "",
        "--unverified",
        "--config build.jobs=2",
        "-- test",
        "--unverified test -Zfeature-unification -p x",
    ] {
        assert_eq!(with_test_graph(&words(line)), words(line), "{line}");
    }
}

/// The shared cache sits in a quiet folder fseventsd drops, still named `*.noindex`, and so
/// does every build directory under it. NEGATIVE CONTROL: the root it moved from, the same
/// name without the folder, is no quiet place.
#[test]
fn the_cache_root_is_quiet_and_noindex() {
    let home = Path::new("/Users//u");
    let root = cache_root(home);
    assert!(aterm_tempfile::is_quiet(&root), "{root:?}");
    assert!(
        root.file_name()
            .unwrap()
            .as_encoded_bytes()
            .ends_with(NOINDEX.as_bytes()),
        "{root:?}"
    );
    assert_eq!(
        root.parent().and_then(Path::file_name),
        Some(std::ffi::OsStr::new(aterm_tempfile::QUIET))
    );
    let leaf = build_dir(&root, Path::new("/Users//u/Developer/aterm"));
    assert!(
        leaf.to_str().is_some_and(|p| p.contains("/.ubd/")),
        "{leaf:?}"
    );
    let old = moved_from(home);
    assert!(!aterm_tempfile::is_quiet(&old), "{old:?}");
    assert_eq!(old.file_name(), root.file_name());
    assert_ne!(old, root);

    let home = PathBuf::from(native("/h"));
    let root = cache_root(&home);
    let cwd = home.join("source");
    let args = ["--unverified".into(), "build".into()];
    for existing in [
        home.clone(),
        home.join("Library"),
        home.join("Library/Caches"),
        root.parent().unwrap().to_owned(),
        root.clone(),
    ] {
        let writable = |path: &Path| {
            assert!(
                path.starts_with(&home),
                "never inspect above the selected home"
            );
            (path == existing).then_some(true)
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &|_| Ok(None),
            read_manifest: &|_| Ok(None),
            writable: &writable,
        };
        assert!(
            matches!(
                super::decide(&args, Tool::Targo, &machine),
                Some(BuildDir::Cache(_))
            ),
            "{existing:?}"
        );
    }
    for blocked in [
        home.clone(),
        home.join("Library"),
        home.join("Library/Caches"),
        root.parent().unwrap().to_owned(),
        root.clone(),
    ] {
        let writable = |path: &Path| {
            if path == blocked {
                Some(false)
            } else {
                (path == home).then_some(true)
            }
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &|_| Ok(None),
            read_manifest: &|_| Ok(None),
            writable: &writable,
        };
        assert!(
            matches!(
                super::decide(&args, Tool::Targo, &machine),
                Some(BuildDir::Target(_))
            ),
            "{blocked:?}"
        );
    }
    let unknown = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| None,
    };
    assert!(matches!(
        super::decide(&args, Tool::Targo, &unknown),
        Some(BuildDir::Target(_))
    ));
}

/// A shim's `__atb_c` line names the root it builds in: this version's quiet one, or the
/// one before the move. NEGATIVE CONTROL: any other root, no such line, and no shim on the
/// path leave the quiet root.
#[test]
fn the_root_plain_targo_builds_in_is_the_one_its_shim_names() {
    let home = Path::new("/Users//u");
    let shim =
        |under: &str| format!("#!/bin/sh\n# atpkg shim\n__atb_t=\n__atb_c=${{HOME-}}/{under}\n");
    assert_eq!(
        root_named_by_shim(home, &shim(MOVED_FROM_UNDER_HOME)),
        Some(moved_from(home))
    );
    assert_eq!(
        root_named_by_shim(home, &shim(CACHE_UNDER_HOME)),
        Some(cache_root(home))
    );
    assert_eq!(root_named_by_shim(home, &shim("Library/elsewhere")), None);
    assert_eq!(
        root_named_by_shim(home, "#!/bin/sh\nexec targo \"$@\"\n"),
        None
    );
    assert_eq!(shim_root(home, std::ffi::OsStr::new("")), cache_root(home));
}

#[test]
fn only_what_cargo_run_says_about_its_child_is_dropped() {
    for name in [
        "CARGO_MANIFEST_DIR",
        "CARGO_PKG_NAME",
        "CARGO_PKG_VERSION_MAJOR",
        "CARGO_BIN_NAME",
        "OUT_DIR",
    ] {
        assert!(said_by_cargo_run(name), "{name}");
    }
    // The caller's own layout and toolchain choices, and a near miss, stay.
    for name in [
        "CARGO",
        "CARGO_HOME",
        "CARGO_TARGET_DIR",
        BUILD_DIR_ENV,
        "CARGO_PKG",
        "XCARGO_PKG_NAME",
        "OUT_DIRS",
    ] {
        assert!(!said_by_cargo_run(name), "{name}");
    }
}

/// `unix`, a fixture's absolute path, as this platform spells one (`C:\w\repo` on
/// Windows, where `/w/repo` is not absolute).
fn native(unix: &str) -> String {
    if cfg!(windows) && unix.starts_with('/') {
        format!("C:{}", unix.replace('/', "\\"))
    } else {
        unix.to_owned()
    }
}

/// A machine whose environment is `env` and whose config files are `files`, every
/// absolute path spelled as this platform spells it ([`native`]).
fn decide(
    target: &str,
    cwd: Option<&str>,
    env: &[(&str, &str)],
    files: &[(&str, &str)],
) -> Option<String> {
    let env: BTreeMap<String, OsString> = env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OsString::from(native(v))))
        .collect();
    let files: BTreeMap<PathBuf, Result<String, ()>> = files
        .iter()
        .map(|(p, text)| {
            let text = if *text == "<unreadable>" {
                Err(())
            } else {
                Ok((*text).to_owned())
            };
            (PathBuf::from(native(p)), text)
        })
        .collect();
    let lookup = |k: &str| env.get(k).cloned();
    let read = |p: &Path| files.get(p).cloned().transpose();
    let writable = |_: &Path| Some(true);
    let cwd = cwd.map(native);
    let home = native("/h");
    let m = Machine {
        home: Some(Path::new(&home)),
        root: None,
        cwd: cwd.as_deref().map(Path::new),
        env: &lookup,
        read: &read,
        read_manifest: &read,
        writable: &writable,
    };
    target_build_dir(Path::new(&native(target)), &m)
}

/// A build pinned to a target directory keeps its intermediates in
/// `<target>/.ubd/build.noindex`; a caller's own build directory, an unknown working
/// directory and a path the literal cannot hold leave the layout alone.
#[test]
fn a_pinned_target_keeps_its_intermediates_out_of_spotlight() {
    let w = Some("/w/repo");
    assert_eq!(
        decide("/w/repo/target", w, &[], &[]),
        Some(format!(
            "build.build-dir='{}'",
            native("/w/repo/target/.ubd/build.noindex")
        ))
    );
    assert!(
        decide(
            "/t",
            w,
            &[],
            &[("/w/repo/.cargo/config.toml", "[build]\ntarget-dir = 'x'\n")]
        )
        .is_some()
    );
    assert!(
        decide(
            "/t",
            w,
            &[],
            &[("/w/.cargo/config.toml", "# build-dir = 'x'\n")]
        )
        .is_some()
    );
    for (why, got) in [
        ("relative", decide("target", w, &[], &[])),
        ("no cwd", decide("/t", None, &[], &[])),
        (
            "env",
            decide("/t", w, &[("CARGO_BUILD_BUILD_DIR", "")], &[]),
        ),
        (
            "ancestor config",
            decide(
                "/t",
                w,
                &[],
                &[("/w/.cargo/config.toml", "[build]\nbuild-dir = 'x'\n")],
            ),
        ),
        (
            "home config",
            decide("/t", w, &[], &[("/h/.cargo/config", "build.build-dir='x'")]),
        ),
        (
            "CARGO_HOME config",
            decide(
                "/t",
                w,
                &[("CARGO_HOME", "/ch")],
                &[("/ch/config.toml", "  build-dir = 'x'")],
            ),
        ),
        (
            "unreadable config",
            decide("/t", w, &[], &[("/w/repo/.cargo/config", "<unreadable>")]),
        ),
        ("quote", decide("/t'q", w, &[], &[])),
        ("brace", decide("/t{workspace-root}", w, &[], &[])),
        ("control", decide("/t\nx", w, &[], &[])),
    ] {
        assert_eq!(got, None, "{why}");
    }
    assert!(
        decide(
            "/t",
            w,
            &[("CARGO_HOME", "/ch")],
            &[("/h/.cargo/config.toml", "build-dir = 'x'")]
        )
        .is_some()
    );

    let dir = aterm_tempfile::Builder::new()
        .prefix("atc-read-config-")
        .tempdir()
        .expect("scratch dir");
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "target-dir = 'x'\n").unwrap();
    assert_eq!(read_config(&cfg), Ok(Some("target-dir = 'x'\n".to_owned())));
    std::fs::write(&cfg, vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert_eq!(
        read_config(&cfg),
        Err(()),
        "oversized config must fail closed"
    );
    #[cfg(unix)]
    {
        let fifo = dir.path().join("fifo.toml");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `c_fifo` is a valid NUL-terminated path in our scratch directory.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(
            read_config(&fifo),
            Ok(None),
            "a FIFO config must not hang or count as a regular file"
        );
    }
}

#[test]
fn cli_include_graphs_preserve_target_custody_only_when_every_layer_does() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    for (leaf, accepted) in [
        (Some("[build]\njobs = 2"), true),
        (Some("include = []"), true),
        (Some("[build]\ntarget-dir = 'unused'"), true),
        (Some("[build]\nbuild-dir = 'separate'"), true),
        (Some(r#"build."build\u002ddir" = 'separate'"#), true),
        (Some("include = ['sub/one.toml']"), false),
        (Some("include = ['root.toml']"), false),
        (Some("include = ['wrong.txt']"), false),
        (Some("include = [3]"), false),
        (Some("include = ['literal[one].toml']"), false),
        (Some("include = ['literal{one}.toml']"), false),
        (
            Some("include = [{ path = 'absent*.toml', optional = true }]"),
            false,
        ),
        (
            Some("include = [{ path = 'absent?.toml', optional = true }]"),
            false,
        ),
        (
            Some(r#"include = [{ path = "literal\u005bone\u005d.toml", optional = true }]"#),
            false,
        ),
        (
            Some("include = [{ path = 'absent.toml', optional = true }]"),
            true,
        ),
        (Some("include = ['absent.toml']"), false),
        (
            Some("include = [{ path = 'unreadable.toml', optional = true }]"),
            false,
        ),
        (Some("[build"), false),
        (None, false),
    ] {
        let read = |path: &Path| {
            if path == cwd.join("cfg/unreadable.toml") {
                return Err(());
            }
            if path == cwd.join("cfg/leaf.toml") {
                return leaf.map(|text| Some(text.into())).ok_or(());
            }
            Ok(if path == cwd.join("cfg/root.toml") {
                Some("include = ['sub/one.toml']".into())
            } else if path == cwd.join("cfg/sub/one.toml") {
                Some("include = ['../leaf.toml']".into())
            } else if ["literal[one].toml", "literal{one}.toml"]
                .iter()
                .any(|name| path == cwd.join("cfg").join(name))
            {
                Some("build.jobs = 2".into())
            } else if path == cwd.join("Cargo.toml") {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        for config in [
            "cfg/root.toml",
            "include = ['cfg/sub/one.toml']",
            "\"incl\\u0075de\" = ['cfg/sub/one.toml']",
        ] {
            let args = [
                "--unverified",
                "build",
                "--target-dir",
                "fresh",
                "--config",
                config,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
            let selected = selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None);
            assert_eq!(selected.is_some(), accepted, "{config}: {leaf:?}");
            if let Some(selected) = selected {
                assert_eq!(selected.target.explicit_path(), cwd.join("fresh"));
                if leaf.is_some_and(|text| text.contains("separate")) {
                    assert_eq!(selected.intermediates, Some(cwd.join("separate")));
                }
            }
        }
    }
}

#[test]
fn included_targets_keep_definition_bases_and_cargo_merge_order() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    for own in [false, true] {
        let root = if own {
            "include=[{path='missing.toml',optional=true}, 'first/a.toml', 'second/b.toml']\nbuild.target-dir='own'"
        } else {
            "include=[{path='missing.toml',optional=true}, 'first/a.toml', 'second/b.toml']"
        };
        let read = |path: &Path| {
            Ok(if path == cwd.join("cfg/root.toml") {
                Some(root.into())
            } else if path == cwd.join("cfg/first/a.toml") {
                Some("build.target-dir='one'".into())
            } else if path == cwd.join("cfg/second/b.toml") {
                Some("build.target-dir='two'".into())
            } else if path == cwd.join("Cargo.toml") {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = ["--unverified", "build", "--config", "cfg/root.toml"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let selected =
            selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None).unwrap();
        assert_eq!(
            selected.target.explicit_path(),
            cwd.join(if own { "own" } else { "cfg/two" })
        );
        let inline = [
            "--unverified",
            "build",
            "--config",
            "include=['cfg/first/a.toml', 'cfg/second/b.toml']",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        assert_eq!(
            selected_outputs(&inline, Tool::Targo, &machine, &|_| UserAlias::None)
                .unwrap()
                .target
                .explicit_path(),
            cwd.join("cfg/two")
        );
    }
}

/// A CLI file that cannot select an output directory leaves an explicit target readable.
/// The file interpretation wins even when its name looks like an inline config assignment.
#[test]
fn explicit_targets_accept_only_proven_directory_neutral_config_files() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let manifest = cwd.join("Cargo.toml");
    for name in ["settings.toml", "build.target-dir=looks-inline"] {
        for text in [
            "[build]\njobs = 2\n",
            "# target-dir is only a comment\n[net]\noffline = true\n",
        ] {
            let file = cwd.join(name);
            let read = |path: &Path| {
                Ok(if path == file {
                    Some(text.into())
                } else if path == manifest {
                    Some("[workspace]".into())
                } else {
                    None
                })
            };
            let m = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &|_| None,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            for config in [
                vec!["--config".into(), name.into()],
                vec![format!("--config={name}")],
            ] {
                let args = [
                    config,
                    vec![
                        "--unverified".into(),
                        "build".into(),
                        "--target-dir".into(),
                        "fresh.noindex".into(),
                    ],
                ]
                .concat();
                let selected = selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None)
                    .expect("readable directory-neutral file preserves explicit target custody");
                assert_eq!(selected.target.explicit_path(), cwd.join("fresh.noindex"));
                assert_eq!(selected.manifests, vec![manifest.clone()]);
                // Observation grants no new placement behavior and never rewrites argv.
                assert!(super::decide(&args, Tool::Targo, &m).is_none());
            }
        }
    }
}

/// A CLI file whose output choice this reader cannot read, or that Targo refuses, grants no
/// explicit target: an `include` that names a file absent and not optional or is no list of
/// paths, text Targo cannot parse, a `build` that is no table, an output value Targo's `build`
/// table cannot take (though the typed target outranks it), and a file the reader cannot read.
/// A build directory the file names (spelled by an escape too) is read with the file's
/// definition base, the folder above its own, and both outputs are the call's. NEGATIVE
/// CONTROL: a target directory in the file, spelled either way, is outranked by the typed
/// `--target-dir` (Targo's `target_dir` takes that word before `CARGO_TARGET_DIR` and every
/// config), and an empty `include` loads nothing, so each keeps the typed target.
#[test]
fn config_files_with_unresolved_output_choices_grant_no_explicit_target() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let file = cwd.join("settings.toml");
    for (text, granted, intermediates) in [
        (Some("[build]\ntarget-dir = 'other'"), true, None),
        (Some(r#"build."target-dir" = 'other'"#), true, None),
        (Some("include = []"), true, None),
        // A build directory the file names is read with its definition base, the folder above
        // the file's: both outputs are the call's.
        (Some("[build]\nbuild-dir = 'other'"), true, Some("other")),
        (Some(r#"build."build-dir" = 'other'"#), true, Some("other")),
        (Some("include = ['absent.toml']"), false, None),
        (Some("include = 3"), false, None),
        (Some("include = [3]"), false, None),
        (Some("[build"), false, None),
        (Some("build = 3"), false, None),
        // A value Targo's `build` table cannot take, though outranked or unused.
        (Some("[build]\ntarget-dir = false"), false, None),
        (Some(r#"build."target-dir" = false"#), false, None),
        (Some("[build]\nbuild-dir = false"), false, None),
        (None, false, None),
    ] {
        let read = |path: &Path| {
            if path == file {
                text.map(|t| Some(t.into())).ok_or(())
            } else {
                Ok((path == cwd.join("Cargo.toml")).then(|| "[workspace]".into()))
            }
        };
        let m = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = [
            "--config",
            "settings.toml",
            "--unverified",
            "build",
            "--target-dir",
            "fresh",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let selected = selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None);
        assert_eq!(
            selected.as_ref().map(|chosen| (
                chosen.target.clone().explicit_path(),
                chosen.intermediates.clone()
            )),
            granted.then(|| (cwd.join("fresh"), intermediates.map(|dir| home.join(dir)))),
            "{text:?}"
        );
    }
}

/// Inline values are data: backslashes and output-key words in an unrelated value cannot
/// choose the target. Existing files with that exact name still outrank inline syntax.
#[test]
fn explicit_target_custody_parses_neutral_inline_config_values() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    for value in [
        r#"env.ATPKG_NEUTRAL = 'a\b'"#,
        r#"env.ATPKG_NEUTRAL = "a\\b""#,
        r#"env.ATPKG_NEUTRAL = 'target-dir build-dir include'"#,
    ] {
        for file_exists in [false, true] {
            let read = |path: &Path| {
                Ok(if file_exists && path == cwd.join(value) {
                    Some("[build]\nbuild-dir = false".into())
                } else if path == cwd.join("Cargo.toml") {
                    Some("[workspace]".into())
                } else {
                    None
                })
            };
            let m = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &|_| None,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            for config in [
                vec!["--config".into(), value.into()],
                vec![format!("--config={value}")],
            ] {
                let args = [
                    config,
                    vec![
                        "--unverified".into(),
                        "build".into(),
                        "--target-dir".into(),
                        "fresh.noindex".into(),
                    ],
                ]
                .concat();
                let selected = selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None);
                if file_exists {
                    assert!(selected.is_none(), "file takes precedence: {value}");
                } else {
                    let selected =
                        selected.expect("neutral parsed inline data preserves target custody");
                    assert_eq!(selected.target.explicit_path(), cwd.join("fresh.noindex"));
                    assert_eq!(selected.manifests, vec![cwd.join("Cargo.toml")]);
                }
            }
        }
    }
}

#[test]
fn inline_configs_with_unknown_or_escaped_output_selection_grant_no_target() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let read = |p: &Path| Ok((p == cwd.join("Cargo.toml")).then(|| "[workspace]".into()));
    let m = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    for value in [
        r#"build."target\u002ddir"=false"#,
        r#"build."target\x2ddir"=false"#,
        r#"build."build\u002ddir"=false"#,
        r#"build."build\x2ddir"=false"#,
        r#""incl\u0075de"=[3]"#,
        "include=[3]",
        "build=3",
        "env.TEST='unterminated",
        "",
        "missing.toml",
    ] {
        let args = [
            "--config",
            value,
            "--unverified",
            "build",
            "--target-dir",
            "fresh",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        assert!(
            selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None).is_none(),
            "{value}"
        );
    }
}

/// Command and environment target choices outrank config, whose relative target is unused.
/// A build-directory key or include can still change output and cannot be disregarded.
#[test]
fn explicit_target_selection_outranks_config_target_keys() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let manifest = cwd.join("Cargo.toml");
    for file_form in [false, true] {
        for source in ["argv", "CARGO_TARGET_DIR", "TARGO_TARGET_DIR"] {
            let text = r#"build."target\u002ddir" = 'unused'"#;
            let file = cwd.join("settings.toml");
            let read = |p: &Path| {
                Ok(if file_form && p == file {
                    Some(text.into())
                } else if p == manifest {
                    Some("[workspace]".into())
                } else {
                    None
                })
            };
            let env = |key: &str| {
                if source == "argv" && key == "CARGO_TARGET_DIR" {
                    Some("wrong-env.noindex".into())
                } else {
                    (source != "argv" && key == source).then(|| "chosen.noindex".into())
                }
            };
            let m = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &env,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let mut args = vec![
                "--config".into(),
                if file_form {
                    "settings.toml".into()
                } else {
                    text.into()
                },
                "--unverified".into(),
                "build".into(),
            ];
            if source == "argv" {
                args.extend(["--target-dir".into(), "chosen.noindex".into()]);
            }
            let chosen = selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None)
                .expect("explicit target overrides config target-dir");
            assert_eq!(chosen.target.explicit_path(), cwd.join("chosen.noindex"));
            assert_eq!(chosen.manifests, vec![manifest.clone()]);
        }
    }
}

#[test]
fn neutral_or_overridden_cli_files_need_no_target_path_base() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let root_config = native("/settings.toml");
    for text in ["build.jobs=2", "build.target-dir='unused'"] {
        let read = |p: &Path| {
            Ok(if p == Path::new(&root_config) {
                Some(text.into())
            } else if p == cwd.join("Cargo.toml") {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let m = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = [
            "--unverified",
            "build",
            "--config",
            &root_config,
            "--target-dir",
            "chosen",
        ]
        .map(str::to_owned);
        assert_eq!(
            selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None)
                .unwrap()
                .target
                .explicit_path(),
            cwd.join("chosen")
        );
    }
}

/// A CLI include graph this reader cannot resolve, or that Targo refuses, grants no custody: an
/// `include` that is no list, an entry Targo refuses, a glob or template path, a file two entries
/// load, and a target no include outranks that is no string. Only the winning value is judged
/// (`merge_helper` swaps a boolean for a string): a file's own target over an include's `false`
/// is the call's, against the file's definition base. NEGATIVE CONTROL: the same include with no
/// target of the file's own over it leaves `false` the winner, which Targo cannot load.
#[test]
fn unresolved_cli_include_graphs_never_grant_custody() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    for direct in [false, true] {
        for include in [
            "true",
            "[{path='child.toml',optional='yes'}]",
            "['*.toml']",
            "['{target}.toml']",
            "['target.toml']",
            "['neutral.toml','neutral.toml']",
            "['left.toml','right.toml']",
            "['target.toml'] # none of its own",
        ] {
            let text = match include.strip_suffix(" # none of its own") {
                Some(include) => format!("include={include}"),
                None => format!("include={include}\nbuild.target-dir='root.noindex'"),
            };
            let read = |p: &Path| {
                Ok(if p == cwd.join("settings.toml") {
                    Some(text.clone())
                } else if p == cwd.join("target.toml") {
                    Some("build.target-dir=false".into())
                } else if p == cwd.join("neutral.toml") || p == cwd.join("shared.toml") {
                    Some("build.jobs=2".into())
                } else if p == cwd.join("left.toml") || p == cwd.join("right.toml") {
                    Some("include=['shared.toml']".into())
                } else if p == cwd.join("Cargo.toml") {
                    Some("[workspace]".into())
                } else {
                    None
                })
            };
            let m = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &|_| None,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let mut args = vec![
                "--unverified".into(),
                "build".into(),
                "--config".into(),
                "settings.toml".into(),
            ];
            if direct {
                args.extend(["--target-dir".into(), "chosen".into()]);
            }
            let granted = (include == "['target.toml']").then(|| {
                if direct {
                    cwd.join("chosen")
                } else {
                    home.join("root.noindex")
                }
            });
            assert_eq!(
                selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None)
                    .map(|c| c.target.explicit_path()),
                granted,
                "{include} direct={direct}"
            );
        }
    }
}

#[test]
fn cli_include_seen_sets_match_file_and_inline_argument_scopes() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let text = "include=['child.toml']";
    let read = |p: &Path| {
        Ok(if p == cwd.join("settings.toml") {
            Some(text.into())
        } else if p == cwd.join("child.toml") {
            Some("build.target-dir='chosen'".into())
        } else if p == cwd.join("Cargo.toml") {
            Some("[workspace]".into())
        } else {
            None
        })
    };
    let m = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    for value in ["settings.toml", text] {
        let args = [
            "--unverified",
            "build",
            "--config",
            value,
            "--config",
            value,
        ]
        .map(str::to_owned);
        let selected = selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None);
        if value == "settings.toml" {
            assert!(selected.is_none());
        } else {
            assert_eq!(
                selected.unwrap().target.explicit_path(),
                home.join("chosen")
            );
        }
    }
}

#[test]
fn cli_include_observation_bounds_aggregate_text_before_retaining_layers() {
    use std::cell::Cell;
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let reads = Cell::new(0);
    let read = |p: &Path| {
        let number = p
            .strip_prefix(&cwd)
            .ok()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<usize>().ok());
        Ok(number.map(|n| {
            reads.set(reads.get() + 1);
            let mut text = format!("include=['{}.toml']\n#", n + 1);
            text.push_str(&"x".repeat((1 << 20) - text.len()));
            text
        }))
    };
    let m = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let args = [
        "--unverified",
        "build",
        "--target-dir",
        "chosen",
        "--config",
        "include=['0.toml']",
    ]
    .map(str::to_owned);
    assert!(selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None).is_none());
    assert!(
        reads.get() <= 16,
        "stop before retaining an unbounded large-file chain"
    );
}

#[test]
fn cli_include_observation_bounds_depth_and_optional_missing_reads() {
    use std::cell::Cell;
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    for optional in [false, true] {
        let reads = Cell::new(0);
        let read = |p: &Path| {
            let number = p
                .strip_prefix(&cwd)
                .ok()
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<usize>().ok());
            Ok(number.and_then(|n| {
                reads.set(reads.get() + 1);
                (!optional).then(|| format!("include=['{}.toml']", n + 1))
            }))
        };
        let text = if optional {
            format!(
                "include=[{}]",
                (0..1100)
                    .map(|n| format!("{{path='{n}.toml',optional=true}}"))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        } else {
            "include=['0.toml']".into()
        };
        let m = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = [
            "--unverified",
            "build",
            "--target-dir",
            "chosen",
            "--config",
            &text,
        ]
        .map(str::to_owned);
        assert!(selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None).is_none());
        assert!(
            reads.get() <= 1024,
            "bounded include reads, got {}",
            reads.get()
        );
    }
}

#[test]
fn discovered_include_observation_bounds_optional_missing_reads() {
    use std::cell::Cell;
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let dir = cwd.join(".cargo");
    let root = dir.join("config.toml");
    let text = format!(
        "include=[{}]",
        (0..1100)
            .map(|n| format!("{{path='{n}.toml',optional=true}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let reads = Cell::new(0);
    let read = |p: &Path| {
        if p == root {
            return Ok(Some(text.clone()));
        }
        if p.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.parse::<usize>().is_ok())
        {
            reads.set(reads.get() + 1);
        }
        Ok(None)
    };
    let m = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let named = config_dirs(&dir, &m);
    assert!(
        reads.get() <= 1024,
        "bounded missing reads, got {}",
        reads.get()
    );
    assert!(
        named.build && named.target,
        "an incomplete graph names both outputs"
    );
    let args = ["--unverified", "build", "--target-dir", "chosen"].map(str::to_owned);
    assert!(selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None).is_none());
}

#[test]
fn discovered_include_observation_bounds_aggregate_text() {
    use std::cell::Cell;
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let dir = cwd.join(".cargo");
    let reads = Cell::new(0);
    let read = |p: &Path| {
        let n = if p == dir.join("config.toml") {
            Some(0)
        } else {
            p.file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<usize>().ok())
        };
        Ok(n.map(|n| {
            reads.set(reads.get() + 1);
            let mut text = if n < 20 {
                format!("include=['{}.toml']\n#", n + 1)
            } else {
                "#".into()
            };
            text.push_str(&"x".repeat((1 << 20) - text.len()));
            text
        }))
    };
    let m = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let named = config_dirs(&dir, &m);
    assert!(
        reads.get() <= 17,
        "stop after the first layer beyond 16 MiB, got {}",
        reads.get()
    );
    assert!(
        named.build && named.target,
        "an incomplete graph names both outputs"
    );
}

#[test]
fn discovered_include_observation_bounds_deep_and_wide_graphs_before_loading_more() {
    use std::cell::Cell;
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let dir = cwd.join(".cargo");
    for wide in [false, true] {
        let reads = Cell::new(0);
        let read = |p: &Path| {
            if p == dir.join("config") {
                return Ok(None);
            }
            let n = if p == dir.join("config.toml") {
                Some(0)
            } else {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<usize>().ok())
            };
            Ok(n.map(|n| {
                reads.set(reads.get() + 1);
                if wide {
                    format!(
                        "include=[{}]",
                        (0..5000)
                            .map(|n| format!("'{n}.toml'"))
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                } else if n < 5000 {
                    format!("include=['{}.toml']", n + 1)
                } else {
                    String::new()
                }
            }))
        };
        let m = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let named = config_dirs(&dir, &m);
        assert!(named.build && named.target);
        if wide {
            assert_eq!(
                reads.get(),
                1,
                "oversized pending work loads no included file"
            );
        } else {
            assert!(
                reads.get() <= 1024,
                "an acyclic graph has a finite read bound"
            );
        }
    }
}

#[test]
fn discovered_include_observation_preserves_conservative_markers_and_fresh_root_cycles() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let dir = cwd.join(".cargo");
    for (root, included, build, target) in [
        ("build.jobs=2", "", false, false),
        ("# include build-dir target-dir", "", false, false),
        ("build.target-dir='chosen'", "", false, true),
        (
            "include=['nested/../shared.toml']",
            "build.build-dir='built'",
            true,
            false,
        ),
        (
            "include=['shared.toml']",
            "build.target-dir='chosen'",
            false,
            true,
        ),
        (
            "include=['shared.toml']",
            "include=['shared.toml']",
            true,
            true,
        ),
        ("include=['shared.toml','shared.toml']", "", true, true),
        ("include=['absent.toml']", "", true, true),
        (
            "include=[{path='absent.toml',optional=true}]",
            "",
            false,
            false,
        ),
        (
            "include=[{path='unreadable.toml',optional=true}]",
            "",
            true,
            true,
        ),
        ("include = [", "", true, true),
        (
            "include=['shared.toml']",
            "build.\"build\\u002ddir\"='built'",
            true,
            false,
        ),
    ] {
        let read = |p: &Path| {
            if p == dir.join("unreadable.toml") {
                return Err(());
            }
            Ok(if p == dir.join("config.toml") || p == dir.join("config") {
                Some(root.into())
            } else if p == dir.join("shared.toml") {
                Some(included.into())
            } else {
                None
            })
        };
        let m = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let named = config_dirs(&dir, &m);
        assert_eq!(
            (named.build, named.target),
            (build, target),
            "{root} / {included}"
        );
    }
}

#[test]
fn discovered_outputs_keep_independent_winners_and_definition_bases() {
    let home = PathBuf::from(native("/home/person"));
    let project = home.join("project");
    let cwd = project.join("member");
    for legacy in [false, true] {
        for explicit in [false, true] {
            let selected = project.join(if legacy {
                ".cargo/config"
            } else {
                ".cargo/config.toml"
            });
            let ignored = project.join(".cargo/config.toml");
            let read = |p: &Path| {
                if legacy && p == ignored {
                    return Err(());
                }
                Ok(if p == selected {
                    Some("include=['layers/build.toml']\nbuild.target-dir='far-final'".into())
                } else if p == project.join(".cargo/layers/build.toml") {
                    Some("build.build-dir='selected-build'".into())
                } else if p == cwd.join(".cargo/config.toml") {
                    Some("build.target-dir='near-final'".into())
                } else if p == home.join(".cargo/config.toml") {
                    Some("build.target-dir='home-final'\nbuild.build-dir='home-build'".into())
                } else if p == cwd.join("Cargo.toml") {
                    Some("[workspace]".into())
                } else {
                    None
                })
            };
            let m = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &|_| None,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let mut args = vec!["--unverified".into(), "build".into()];
            if explicit {
                args.extend(["--target-dir".into(), "explicit-final".into()]);
            }
            let chosen = selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None)
                .expect("complete discovered outputs retain independent winners");
            assert_eq!(
                chosen.target.explicit_path(),
                cwd.join(if explicit {
                    "explicit-final"
                } else {
                    "near-final"
                })
            );
            assert_eq!(
                chosen.intermediates,
                Some(project.join(".cargo/selected-build"))
            );
            assert_eq!(chosen.manifests, vec![cwd.join("Cargo.toml")]);
        }
    }
}

#[test]
fn discovered_output_precedence_is_independent_for_each_selected_path() {
    let home = PathBuf::from(native("/home/person"));
    let project = home.join("project");
    let cwd = project.join("member");
    for tool in [Tool::Targo, Tool::CargoCompat] {
        for level in 0..5 {
            let read = |p: &Path| {
                Ok(if p == project.join(".cargo/config.toml") {
                    Some(
                        "build.target-dir='discovered-final'\nbuild.build-dir='discovered-build'"
                            .into(),
                    )
                } else if p == cwd.join("Cargo.toml") {
                    Some("[workspace]".into())
                } else {
                    None
                })
            };
            let env = |name: &str| match name {
                "CARGO_BUILD_TARGET_DIR" if level >= 1 => Some("env-final".into()),
                "CARGO_BUILD_BUILD_DIR" if level >= 1 => Some("env-build".into()),
                "CARGO_TARGET_DIR" if level >= 3 => Some("direct-final".into()),
                "TARGO_BUILD_TARGET_DIR" if level >= 1 => Some("hidden-twin-final".into()),
                "TARGO_BUILD_BUILD_DIR" if level >= 1 => Some("hidden-twin-build".into()),
                _ => None,
            };
            let m = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &env,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let mut args = vec!["--unverified".into(), "build".into()];
            if level >= 2 {
                // Targo takes one dotted key per inline value (`toml_dotted_keys`): two keys in
                // one value fail the call, so grant nothing.
                let two_keys = [
                    args.clone(),
                    vec![
                        "--config".into(),
                        "build.target-dir='cli-final'\nbuild.build-dir='cli-build'".into(),
                    ],
                ]
                .concat();
                assert!(
                    selected_outputs(&two_keys, tool, &m, &|_| UserAlias::None).is_none(),
                    "{tool:?} level={level}"
                );
                args.extend([
                    "--config".into(),
                    "build.target-dir='cli-final'".into(),
                    "--config".into(),
                    "build.build-dir='cli-build'".into(),
                ]);
            }
            if level >= 4 {
                args.extend(["--target-dir".into(), "argv-final".into()]);
            }
            let chosen = selected_outputs(&args, tool, &m, &|_| UserAlias::None).unwrap();
            let (target, build) = match level {
                0 => (
                    project.join("discovered-final"),
                    project.join("discovered-build"),
                ),
                1 => (cwd.join("env-final"), cwd.join("env-build")),
                2 => (cwd.join("cli-final"), cwd.join("cli-build")),
                3 => (cwd.join("direct-final"), cwd.join("cli-build")),
                _ => (cwd.join("argv-final"), cwd.join("cli-build")),
            };
            assert_eq!(
                (chosen.target.explicit_path(), chosen.intermediates),
                (target, Some(build)),
                "{tool:?} level={level}"
            );
        }
    }
}

#[test]
fn discovered_package_home_variants_must_agree_after_higher_choices() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("project");
    let twin = home.join("twin");
    let custom = home.join("packages/cargo");
    for mode in ["ambiguous", "override", "custom", "empty-cargo-home"] {
        let read = |p: &Path| {
            if mode == "custom" && p.starts_with(&twin) {
                panic!("CARGO_HOME suppresses the twin");
            }
            Ok(if p == twin.join("config.toml") {
                Some("build.build-dir='twin-build'".into())
            } else if p == custom.join("config.toml") {
                Some("build.build-dir='custom-build'".into())
            } else if p == cwd.join("Cargo.toml") {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let env = |name: &str| match name {
            "TARGO_HOME" => Some(twin.clone().into_os_string()),
            "CARGO_HOME" if mode == "custom" => Some(custom.clone().into_os_string()),
            "CARGO_HOME" if mode == "empty-cargo-home" => Some(OsString::new()),
            "CARGO_BUILD_BUILD_DIR" if mode == "override" => Some("env-build".into()),
            _ => None,
        };
        let m = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = ["--unverified", "build", "--target-dir", "chosen"].map(str::to_owned);
        let selected = selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None);
        if mode == "ambiguous" {
            assert!(selected.is_none());
            continue;
        }
        let chosen = selected.unwrap();
        assert_eq!(chosen.target.explicit_path(), cwd.join("chosen"));
        assert_eq!(
            chosen.intermediates,
            match mode {
                "override" => Some(cwd.join("env-build")),
                "custom" => Some(home.join("packages/custom-build")),
                _ => None,
            }
        );
    }
}

#[test]
fn a_default_final_target_requires_a_complete_native_output_graph() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("project");
    for (text, complete) in [
        ("build.jobs=2", true),
        ("include=[{path='missing.toml',optional=true}]", true),
        ("include=['missing.toml']", false),
        ("include=['config.toml']", false),
        ("include=[3]", false),
    ] {
        let read = |path: &Path| {
            Ok(if path == cwd.join(".cargo/config.toml") {
                Some(text.into())
            } else if path == cwd.join("Cargo.toml") {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = [
            "--unverified",
            "build",
            "--config",
            "build.build-dir='intermediate'",
        ]
        .map(str::to_owned);
        let choice = selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None);
        if complete {
            let choice = choice.expect(text);
            assert_eq!(choice.target, FinalTarget::WorkspaceDefault);
            assert_eq!(choice.intermediates, Some(cwd.join("intermediate")));
            assert_eq!(choice.manifests, vec![cwd.join("Cargo.toml")]);
        } else {
            assert!(choice.is_none(), "{text}");
        }
    }
}

#[test]
fn discovered_output_grants_keep_unknown_graphs_frontends_and_templates_refused() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("project");
    for text in [
        "include=['missing.toml']",
        "include=['config.toml']",
        "include=[3]",
        "include=[{path='bad*.toml',optional=true}]",
        "include=[",
        "build.build-dir=false",
        "build.build-dir=''",
        "build.build-dir='{workspace-root}/b'",
        "build.build-dir='../outside'",
        "build.target-dir=3\nbuild.build-dir='b'",
    ] {
        let read = |p: &Path| {
            Ok(if p == cwd.join(".cargo/config.toml") {
                Some(text.into())
            } else if p == cwd.join("Cargo.toml") {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let m = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = ["--unverified", "build", "--target-dir", "known"].map(str::to_owned);
        assert!(
            selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None).is_none(),
            "{text}"
        );
    }
    let read = |p: &Path| {
        Ok(if p == cwd.join(".cargo/config.toml") {
            Some("build.build-dir='discovered'".into())
        } else if p == cwd.join("Cargo.toml") {
            Some("[workspace]".into())
        } else {
            None
        })
    };
    let m = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    for (tool, words) in [
        (Tool::Cargo, vec!["build", "--target-dir", "known"]),
        (Tool::Targo, vec!["trust", "check", "--target-dir", "known"]),
    ] {
        let args: Vec<_> = words.into_iter().map(str::to_owned).collect();
        assert!(
            selected_outputs(&args, tool, &m, &|_| UserAlias::None).is_none(),
            "{tool:?}"
        );
    }
    let env = |name: &str| (name == BUILD_DIR_ENV).then(|| "higher-build".into());
    let read = |p: &Path| {
        Ok(if p == cwd.join(".cargo/config.toml") {
            Some("build.build-dir='{workspace-root}/unused'".into())
        } else if p == cwd.join("Cargo.toml") {
            Some("[workspace]".into())
        } else {
            None
        })
    };
    let m = Machine {
        env: &env,
        read: &read,
        read_manifest: &read,
        ..m
    };
    let args = ["--unverified", "build", "--target-dir", "known"].map(str::to_owned);
    assert_eq!(
        selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None)
            .unwrap()
            .intermediates,
        Some(cwd.join("higher-build"))
    );
}

#[cfg(unix)]
#[test]
fn discovered_package_home_alias_of_an_ancestor_is_not_loaded_again() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let project = home.join("project");
    let cwd = project.join("member");
    let config = project.join(".cargo");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir(&config).unwrap();
    std::fs::write(config.join("config.toml"), "build.build-dir='chosen-build'").unwrap();
    let alias = home.join("package-alias");
    std::os::unix::fs::symlink(&config, &alias).unwrap();
    let read = |p: &Path| {
        assert!(
            !p.starts_with(&alias),
            "a canonical ancestor package home is skipped"
        );
        match std::fs::read_to_string(p) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        }
    };
    let env = |name: &str| (name == "TARGO_HOME").then(|| alias.clone().into_os_string());
    let m = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &env,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let args = [
        "--unverified",
        "build",
        "--manifest-path",
        "Cargo.toml",
        "--target-dir",
        "known",
    ]
    .map(str::to_owned);
    let chosen = selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None).unwrap();
    assert_eq!(chosen.intermediates, Some(project.join("chosen-build")));
}

#[test]
fn cli_include_support_requires_a_known_frontend_capability() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let read = |p: &Path| {
        Ok(if p == cwd.join("layers/target.toml") {
            Some("build.target-dir='included'".into())
        } else if p == cwd.join("Cargo.toml") {
            Some("[workspace]".into())
        } else {
            None
        })
    };
    let m = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|_| None,
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    for tool in [Tool::Cargo, Tool::CargoCompat, Tool::Targo] {
        for direct in [false, true] {
            let mut args = vec![
                "build".into(),
                "--config".into(),
                "include=['layers/target.toml']".into(),
            ];
            if tool == Tool::Targo {
                args.insert(0, "--unverified".into());
            }
            if direct {
                args.extend(["--target-dir".into(), "chosen".into()]);
            }
            let selected = selected_outputs(&args, tool, &m, &|_| UserAlias::None);
            if tool == Tool::Cargo {
                assert!(selected.is_none());
            } else {
                assert_eq!(
                    selected.unwrap().target.explicit_path(),
                    cwd.join(if direct { "chosen" } else { "included" })
                );
            }
        }
    }
    let args = ["build", "--config", "include=[]", "--target-dir", "chosen"].map(str::to_owned);
    assert_eq!(
        selected_outputs(&args, Tool::Cargo, &m, &|_| UserAlias::None)
            .unwrap()
            .target
            .explicit_path(),
        cwd.join("chosen")
    );
}

#[test]
fn cli_config_targets_keep_the_winning_values_path_base() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let file = cwd.join("settings.toml");
    let nested = cwd.join("nested/settings.toml");
    let manifest = cwd.join("Cargo.toml");
    for (configs, expected) in [
        (
            vec!["build.target-dir='inline.noindex'"],
            cwd.join("inline.noindex"),
        ),
        (
            vec!["build.\"target\\x2Ddir\" = \"toml\\x31\\x31.noindex\""],
            cwd.join("toml11.noindex"),
        ),
        (vec!["settings.toml"], home.join("file.noindex")),
        (vec!["nested/settings.toml"], cwd.join("nested.noindex")),
        (
            vec!["settings.toml", "build.target-dir='last.noindex'"],
            cwd.join("last.noindex"),
        ),
        (
            vec!["build.target-dir='first.noindex'", "settings.toml"],
            home.join("file.noindex"),
        ),
        (
            vec!["settings.toml", "build.jobs=2"],
            home.join("file.noindex"),
        ),
    ] {
        let read = |p: &Path| {
            Ok(if p == file {
                Some("build.target-dir='file.noindex'".into())
            } else if p == nested {
                Some("build.\"target\\u002ddir\"='nested.noindex'".into())
            } else if p == manifest {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let m = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|key| (key == "CARGO_BUILD_TARGET_DIR").then(|| "lower.noindex".into()),
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let mut args = vec!["--unverified".into(), "build".into()];
        for config in configs {
            args.extend(["--config".into(), config.into()]);
        }
        let selected = selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None)
            .expect("parsed CLI config selects its target before config-level environment");
        assert_eq!(selected.target.explicit_path(), expected);
        assert_eq!(selected.manifests, vec![manifest.clone()]);
    }
}

#[test]
fn config_targets_preserve_higher_override_and_program_boundaries() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    for source in [
        "absent",
        "empty-argv",
        "empty-env",
        "program-word",
        "lower-env",
        "lower-twin",
    ] {
        let read = |p: &Path| Ok((p == cwd.join("Cargo.toml")).then(|| "[workspace]".into()));
        let env = |key: &str| {
            if source == "empty-env" && key == "CARGO_TARGET_DIR" {
                Some("".into())
            } else if ((source == "empty-env" || source == "lower-env")
                && key == "CARGO_BUILD_TARGET_DIR")
                || (source == "lower-twin" && key == "TARGO_BUILD_TARGET_DIR")
            {
                Some("lower-priority.noindex".into())
            } else {
                None
            }
        };
        let m = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let mut args = vec![
            "--config".into(),
            "build.target-dir='configured'".into(),
            "--unverified".into(),
            if source == "program-word" {
                "run".into()
            } else {
                "build".into()
            },
        ];
        if source == "empty-argv" {
            args.extend(["--target-dir".into(), "".into()]);
        }
        if source == "program-word" {
            args.extend(["serve".into(), "--target-dir".into(), "program-only".into()]);
        }
        let selected = selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None);
        if source.starts_with("empty-") {
            assert!(selected.is_none(), "{source}");
        } else {
            assert_eq!(
                selected.expect(source).target.explicit_path(),
                cwd.join("configured"),
                "{source}"
            );
        }
    }
}

#[test]
fn initial_manifest_readings_preserve_frontend_grammar() {
    let home = PathBuf::from(native("/home/me"));
    let cwd = home.join("workspace");
    for unreadable_native in [false, true] {
        let read = |path: &Path| {
            if path == cwd.join("Targo.toml") {
                if unreadable_native {
                    Err(())
                } else {
                    Ok(Some("[workspace]".into()))
                }
            } else {
                Ok((path == cwd.join("Cargo.toml")).then(|| "[workspace]".into()))
            }
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &|_| None,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        for tool in [Tool::Targo, Tool::Cargo, Tool::CargoCompat, Tool::Tippy] {
            let mut args = match tool {
                Tool::Targo => vec!["--unverified".into(), "build".into()],
                Tool::Tippy => vec![],
                _ => vec!["build".into()],
            };
            args.push("--target-dir=output".into());
            let selected = selected_outputs(&args, tool, &machine, &|_| UserAlias::None);
            let expected = if tool == Tool::Cargo {
                Some(vec![cwd.join("Cargo.toml")])
            } else if unreadable_native {
                None
            } else if tool == Tool::Targo {
                Some(vec![cwd.join("Targo.toml")])
            } else {
                Some(vec![cwd.join("Targo.toml"), cwd.join("Cargo.toml")])
            };
            assert_eq!(
                selected.map(|cache| cache.manifests),
                expected,
                "{tool:?}, unreadable={unreadable_native}"
            );
        }
        let proof = vec!["trust".into(), "check".into(), "--target-dir=output".into()];
        let expected =
            (!unreadable_native).then(|| vec![cwd.join("Targo.toml"), cwd.join("Cargo.toml")]);
        assert_eq!(
            selected_outputs(&proof, Tool::Targo, &machine, &|_| UserAlias::None)
                .map(|cache| cache.manifests),
            expected,
            "proof frontend, unreadable={unreadable_native}"
        );
    }
}

#[test]
fn raw_build_dir_hints_keep_selected_templates_and_path_bases() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("project");
    let file = home.join("configs/nested/choices.toml");
    for source in ["config", "environment", "cli", "file"] {
        for value in [
            "{workspace-root}/build.noindex/{workspace-path-hash}",
            "{cargo-cache-home}/build.noindex/{workspace-path-hash}",
            "build.noindex/{workspace-root}/slot",
            "/cache.noindex{workspace-root}/slot",
            "build.noindex/{workspace-path-hash}",
            "plain",
        ] {
            let read = |path: &Path| {
                Ok(
                    if (source == "config" && path == cwd.join(".cargo/config.toml"))
                        || (source == "file" && path == file)
                    {
                        Some(format!("build.build-dir='{value}'"))
                    } else if path == cwd.join("Cargo.toml") {
                        Some("[workspace]".into())
                    } else {
                        None
                    },
                )
            };
            let env = |name: &str| {
                (source == "environment" && name == BUILD_DIR_ENV).then(|| value.into())
            };
            let machine = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &env,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let mut args = ["--unverified", "build", "--target-dir", "chosen"]
                .map(str::to_owned)
                .to_vec();
            if source == "cli" {
                args.extend(["--config".into(), format!("build.build-dir='{value}'")]);
            } else if source == "file" {
                args.extend(["--config".into(), file.display().to_string()]);
            }
            let selected =
                selected_build_dir_hint(&args, Tool::Targo, &machine, &|_| UserAlias::None)
                    .expect("resolved build-directory choice")
                    .expect(value);
            let base = if source == "file" {
                file.parent().unwrap().parent().unwrap()
            } else {
                cwd.as_path()
            };
            let expected = if value.starts_with("{workspace-root}")
                || value.starts_with("{cargo-cache-home}")
                || Path::new(value).is_absolute()
            {
                PathBuf::from(value)
            } else {
                base.join(value)
            };
            assert_eq!(selected.value, expected, "{source}: {value}");
            assert_eq!(selected.manifests, Some(vec![cwd.join("Cargo.toml")]));
            assert_eq!(
                selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None).is_some(),
                !value.contains('{'),
                "only the hint reader accepts Cargo templates: {source}: {value}"
            );
        }
    }
}

#[test]
fn raw_build_dir_hints_share_alias_and_config_precedence() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("project");
    let alias = |name: &str| match name {
        "before" => UserAlias::Words(
            [
                "--config",
                "build.build-dir='alias.noindex/{workspace-path-hash}'",
                "build",
            ]
            .map(str::to_owned)
            .to_vec(),
        ),
        "after" => UserAlias::Words(
            [
                "build",
                "--config",
                "build.build-dir='alias.noindex/{workspace-path-hash}'",
            ]
            .map(str::to_owned)
            .to_vec(),
        ),
        _ => UserAlias::None,
    };
    for (words, environment, configured, expected) in [
        (
            "--unverified build --config build.build-dir='cli.noindex/{workspace-path-hash}'",
            true,
            "config.noindex/{workspace-path-hash}",
            "cli.noindex/{workspace-path-hash}",
        ),
        (
            "--unverified before --config net.offline=true",
            false,
            "config.noindex/{workspace-path-hash}",
            "config.noindex/{workspace-path-hash}",
        ),
        (
            "--unverified before",
            false,
            "config.noindex/{workspace-path-hash}",
            "alias.noindex/{workspace-path-hash}",
        ),
        (
            "--unverified after --config net.offline=true",
            false,
            "config.noindex/{workspace-path-hash}",
            "alias.noindex/{workspace-path-hash}",
        ),
        (
            "--unverified run -- --config build.build-dir='program.noindex/{workspace-path-hash}'",
            true,
            "{unknown}",
            "env.noindex/{workspace-path-hash}",
        ),
    ] {
        let read = |path: &Path| {
            Ok(if path == cwd.join(".cargo/config.toml") {
                Some(format!("build.build-dir='{configured}'"))
            } else if path == cwd.join("Cargo.toml") {
                Some("[workspace]".into())
            } else {
                None
            })
        };
        let env = |name: &str| {
            (environment && name == BUILD_DIR_ENV)
                .then(|| "env.noindex/{workspace-path-hash}".into())
        };
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &env,
            read: &read,
            read_manifest: &read,
            writable: &|_| Some(true),
        };
        let args = words
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let selected = selected_build_dir_hint(&args, Tool::Targo, &machine, &alias)
            .unwrap_or_else(|error| panic!("{words}: {error}"))
            .unwrap_or_else(|| panic!("{words}"));
        assert_eq!(selected.value, cwd.join(expected), "{words}");
    }
}

#[test]
fn raw_build_dir_hints_observe_foreign_paths_without_granting_custody() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = PathBuf::from(native("/foreign/project"));
    let machine = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|name| (name == BUILD_DIR_ENV).then(|| "build.noindex/{workspace-path-hash}".into()),
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| Some(true),
    };
    let args = ["--unverified", "build"].map(str::to_owned);
    let selected = selected_build_dir_hint(&args, Tool::Targo, &machine, &|_| UserAlias::None)
        .expect("the command is resolved")
        .expect("a foreign producer still selects its intermediate directory");
    assert_eq!(
        selected.value,
        cwd.join("build.noindex/{workspace-path-hash}")
    );
    assert_eq!(selected.manifests, None);
    assert!(selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None).is_none());
    for words in ["--unverified build --help", "--unverified clean"] {
        let args = words
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(
            matches!(
                selected_build_dir_hint(&args, Tool::Targo, &machine, &|_| UserAlias::None),
                Ok(None)
            ),
            "{words}"
        );
    }
}

#[test]
fn raw_build_dir_hints_refuse_unresolved_explicit_frontend_choices() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("project");
    for (tool, words) in [
        (Tool::Targo, "trust check --target-dir chosen"),
        (Tool::Cargo, "build --target-dir chosen"),
    ] {
        for source in ["none", "environment", "config", "cli"] {
            let env = |name: &str| {
                (source == "environment" && name == BUILD_DIR_ENV).then(|| "build".into())
            };
            let read = |path: &Path| {
                Ok(
                    if source == "config" && path == cwd.join(".cargo/config.toml") {
                        Some("build.build-dir='build'".into())
                    } else if path == cwd.join("Cargo.toml") {
                        Some("[workspace]".into())
                    } else {
                        None
                    },
                )
            };
            let machine = Machine {
                home: Some(&home),
                root: None,
                cwd: Some(&cwd),
                env: &env,
                read: &read,
                read_manifest: &read,
                writable: &|_| Some(true),
            };
            let mut args = words
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if source == "cli" {
                args.extend(["--config".into(), "build.build-dir='build'".into()]);
            }
            let selected = selected_build_dir_hint(&args, tool, &machine, &|_| UserAlias::None);
            if source == "none" {
                assert!(matches!(selected, Ok(None)), "{tool:?}: {selected:?}");
            } else {
                assert!(selected.is_err(), "{tool:?} {source}: {selected:?}");
            }
            args.insert(0, "--help".into());
            assert!(
                matches!(
                    selected_build_dir_hint(&args, tool, &machine, &|_| UserAlias::None),
                    Ok(None)
                ),
                "a query writes no build directory: {tool:?} {source}"
            );
        }
    }
    let machine = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|name| (name == BUILD_DIR_ENV).then(|| "build.noindex".into()),
        read: &|_| Ok(None),
        read_manifest: &|_| Ok(None),
        writable: &|_| Some(true),
    };
    assert!(
        selected_build_dir_hint(
            &["--unverified".into(), "unknown".into()],
            Tool::Targo,
            &machine,
            &|_| UserAlias::Unknown,
        )
        .is_err(),
        "an untold alias cannot hide a possible explicit root"
    );
}

#[test]
fn intermediate_observation_does_not_require_a_known_final_target_or_owner() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("notes");
    let manifest = PathBuf::from(native("/foreign/missing/Cargo.toml"));
    for explicit in [false, true] {
        let read = |path: &Path| {
            Ok((path == cwd.join(".cargo/config.toml"))
                .then(|| "build.target-dir=['not-a-custody-target']".into()))
        };
        let env = |name: &str| (explicit && name == BUILD_DIR_ENV).then(|| "build.noindex".into());
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(&cwd),
            env: &env,
            read: &read,
            read_manifest: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let args = vec![
            "--unverified".into(),
            "build".into(),
            "--manifest-path".into(),
            manifest.display().to_string(),
        ];
        let selected =
            selected_build_dir_hint(&args, Tool::Targo, &machine, &|_| UserAlias::None).unwrap();
        assert_eq!(
            selected.as_ref().map(|hint| &hint.value),
            explicit.then(|| cwd.join("build.noindex")).as_ref()
        );
        assert!(selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None).is_none());
    }
}

#[test]
fn raw_workspace_discovery_observes_foreign_manifests_without_granting_custody() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = PathBuf::from(native("/foreign/project/member"));
    let manifest = cwd.parent().unwrap().join("Cargo.toml");
    let read = |path: &Path| Ok((path == manifest).then(|| "[workspace]\nmembers=[]".into()));
    let machine = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &|name| {
            (name == BUILD_DIR_ENV)
                .then(|| "{workspace-root}/build.noindex/{workspace-path-hash}".into())
        },
        read: &read,
        read_manifest: &read,
        writable: &|_| Some(true),
    };
    let args = ["--unverified", "build"].map(str::to_owned);
    let selected = selected_build_dir_hint(&args, Tool::Targo, &machine, &|_| UserAlias::None)
        .unwrap()
        .unwrap();
    assert_eq!(selected.manifests, Some(vec![manifest.clone()]));
    assert!(selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None).is_none());
}
