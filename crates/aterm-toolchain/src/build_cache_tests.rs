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
/// the twin home names, which the store's Targo would not build into, is none.
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

#[test]
fn configured_final_targets_follow_cargo_precedence_without_guessing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let root = home.join("project");
    let cwd = root.join("member");
    let config = root.join(".cargo/config.toml");
    let legacy = root.join(".cargo/config");
    let outer = home.join(".cargo/config.toml");
    // The included file is present and neutral. Known Trust compile fronts can resolve
    // the outer target through it; the upstream target hint still has no include capability.
    let layout = root.join(".cargo/layout.toml");
    let manifest = root.join("Cargo.toml");
    let args = ["--unverified".into(), "build".into()];
    for (text, legacy_text, expected) in [
        (
            "[build]\ntarget-dir = 'target/.ubd/main.noindex'",
            None,
            Some(root.join(DEFAULT_TARGET)),
        ),
        ("build.target-dir = 'inner'", None, Some(root.join("inner"))),
        (
            "build.target-dir = 'inner'",
            Some("build.target-dir = 'legacy'"),
            Some(root.join("legacy")),
        ),
        ("# target-dir = 'comment'", None, Some(home.join("outer"))),
        ("build.target-dir = 4", None, None),
        ("build.target-dir = [", None, None),
        ("include = ['layout.toml']", None, None),
        (
            "include = ['layout.toml']\nbuild.target-dir = 'local'",
            None,
            Some(root.join("local")),
        ),
    ] {
        let read = |path: &Path| {
            Ok(if path == config {
                Some(text.into())
            } else if path == legacy {
                legacy_text.map(str::to_owned)
            } else if path == outer {
                Some("build.target-dir = 'outer'".into())
            } else if path == layout {
                Some(String::new())
            } else if path == manifest {
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
        let trust_expected = if text == "include = ['layout.toml']" {
            Some(home.join("outer"))
        } else {
            expected.clone()
        };
        assert_eq!(
            selected_outputs(&args, Tool::Targo, &machine, &|_| UserAlias::None)
                .map(|c| c.target.explicit_path()),
            trust_expected,
            "{text}"
        );
        match &expected {
            Some(path) => assert_eq!(
                target_hint(&machine, Tool::Cargo).unwrap().as_ref(),
                Some(path)
            ),
            None => assert!(target_hint(&machine, Tool::Cargo).is_err(), "{text}"),
        }
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
            .unwrap()
            .target
            .explicit_path(),
            overridden
        );
    }
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
        targo_line(&args, tool, &alias).map(|line| line.join(" "))
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
    assert_eq!(targo_line(&args, Tool::Targo, &looping), None);
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
        targo_reading(&args, tool, &alias).map(|reading| reading.query)
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
/// is none of Targo's, and one whose value is missing names none. NEGATIVE CONTROL: in `-pC`
/// the `C` is `-p`'s value.
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
    for line in ["build -C d", "-- -C d", "-pC build", "-Z -C"] {
        assert_eq!(dir(line).flatten(), None, "{line}");
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
/// beside `~/.cargo` where `CARGO_HOME` is unset (the store's Targo reads only the first,
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

#[test]
fn config_files_with_unresolved_output_choices_grant_no_explicit_target() {
    let home = PathBuf::from(native("/home/person"));
    let cwd = home.join("workspace");
    let file = cwd.join("settings.toml");
    for text in [
        Some("[build]\ntarget-dir = false"),
        Some("[build]\nbuild-dir = false"),
        Some("include = [3]"),
        Some("[build"),
        Some("build = 3"),
        Some(r#"build."target\u002ddir" = false"#),
        None,
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
        assert!(
            selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None).is_none(),
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
        r#"build."build\u002ddir"=false"#,
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
        ] {
            let text = format!("include={include}\nbuild.target-dir='root.noindex'");
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
            assert!(
                selected_outputs(&args, Tool::Targo, &m, &|_| UserAlias::None).is_none(),
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
                args.extend([
                    "--config".into(),
                    "build.target-dir='cli-final'\nbuild.build-dir='cli-build'".into(),
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
    let home = Path::new("/home/me");
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
            home: Some(home),
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
