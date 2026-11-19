// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::Tool;
use super::planner::*;
use crate::build_cache::Machine;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const SDK: &str = "/Developer SDKs/MacOSX.sdk";

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Executable(PathBuf),
    Run(PathBuf, Vec<String>),
    Directory(PathBuf),
}

struct Observations {
    unavailable: Option<PathBuf>,
    selected: bool,
    answer: Option<Vec<u8>>,
    directory: bool,
    events: Vec<Event>,
}

impl Default for Observations {
    fn default() -> Self {
        Self {
            unavailable: None,
            selected: true,
            answer: Some(format!("{SDK}\n").into_bytes()),
            directory: true,
            events: Vec::new(),
        }
    }
}

impl Probe for Observations {
    fn executable(&mut self, program: &Path) -> bool {
        self.events.push(Event::Executable(program.to_path_buf()));
        self.unavailable.as_deref() != Some(program)
    }

    fn output(&mut self, program: &Path, args: &[&str]) -> Option<Vec<u8>> {
        self.events.push(Event::Run(
            program.to_path_buf(),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
        ));
        if program == Path::new(SELECT) {
            self.selected.then(Vec::new)
        } else {
            assert_eq!(program, Path::new(XCRUN));
            self.answer.take()
        }
    }

    fn directory(&mut self, path: &Path) -> bool {
        self.events.push(Event::Directory(path.to_path_buf()));
        self.directory
    }
}

fn words(args: &[&str]) -> Vec<OsString> {
    args.iter().map(|arg| OsString::from(*arg)).collect()
}

fn plan(
    tool: Tool,
    args: &[OsString],
    environment: &[(&str, &str)],
    config: Option<&str>,
    observations: &mut Observations,
) -> Vec<(OsString, OsString)> {
    let env = |name: &str| {
        environment
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(*value))
    };
    let read = |path: &Path| {
        Ok(
            if path == Path::new("/sdk-planner/home/project/.cargo/config.toml") {
                config.map(str::to_owned)
            } else {
                None
            },
        )
    };
    let machine = Machine {
        home: Some(Path::new("/sdk-planner/home")),
        root: None,
        cwd: Some(Path::new("/sdk-planner/home/project")),
        env: &env,
        read: &read,
        read_manifest: &|_| Ok(None),
        writable: &|_| None,
    };
    environment_with(tool, args, &machine, observations)
}

fn overlay(root: &str) -> Vec<(OsString, OsString)> {
    vec![("SDKROOT".into(), root.into())]
}

#[test]
fn building_and_running_commands_receive_the_selected_sdk_input() {
    for (tool, args) in [
        (Tool::Targo, vec!["--unverified", "build"]),
        (Tool::Targo, vec!["--unverified", "test", "-p", "fixture"]),
        (Tool::Targo, vec!["--unverified", "run", "--", "--help"]),
        (Tool::Targo, vec!["--unverified", "b"]),
        (Tool::Targo, vec!["--unverified", "trust", "check"]),
        (Tool::Targo, vec!["--unverified", "tippy", "--all-targets"]),
        (Tool::Tippy, vec!["--all-targets"]),
        (Tool::Tippy, vec!["--all-targets", "--", "-Copt-level=2"]),
        (
            Tool::Tippy,
            vec!["--all-targets", "--", "-C", "opt-level=2"],
        ),
        (Tool::Cargo, vec!["+stable", "build"]),
    ] {
        let mut observed = Observations::default();
        assert_eq!(
            plan(tool, &words(&args), &[], None, &mut observed),
            overlay(SDK),
            "{tool:?} {args:?}"
        );
        assert_eq!(
            observed.events,
            vec![
                Event::Executable(SELECT.into()),
                Event::Executable(XCRUN.into()),
                Event::Run(SELECT.into(), vec!["-p".into()]),
                Event::Run(XCRUN.into(), vec!["--show-sdk-path".into()]),
                Event::Directory(SDK.into()),
            ]
        );
    }
}

#[test]
fn queries_and_nonbuilding_verbs_never_probe_developer_tools() {
    for (tool, args) in [
        (Tool::Targo, vec!["metadata"]),
        (Tool::Targo, vec!["--unverified", "fmt"]),
        (Tool::Targo, vec!["--unverified", "clean"]),
        (Tool::Targo, vec!["--unverified", "build", "--help"]),
        (Tool::Targo, vec!["--version"]),
        (Tool::Targo, vec!["trust", "--help"]),
        (Tool::Tippy, vec!["--version"]),
        (
            Tool::Targo,
            vec!["--unverified", "resource", "fixed", "--", "repo-prune"],
        ),
    ] {
        let mut observed = Observations::default();
        assert!(
            plan(tool, &words(&args), &[], None, &mut observed).is_empty(),
            "{tool:?} {args:?}"
        );
        assert!(observed.events.is_empty(), "{tool:?} {args:?}");
    }
}

#[test]
fn actual_config_and_environment_aliases_share_the_existing_command_reader() {
    let config = "[alias]\nverify = 'run -q -p aterm-verify --'\nship = ['run', '-p', 'aterm-release', '--']\nlab = 'verify'\ninspect = 'metadata'\n";
    for alias in ["verify", "ship", "lab"] {
        let mut observed = Observations::default();
        assert_eq!(
            plan(
                Tool::Targo,
                &words(&["--unverified", alias, "--help"]),
                &[],
                Some(config),
                &mut observed
            ),
            overlay(SDK),
            "{alias}"
        );
    }
    let mut observed = Observations::default();
    assert!(
        plan(
            Tool::Targo,
            &words(&["--unverified", "inspect"]),
            &[],
            Some(config),
            &mut observed
        )
        .is_empty()
    );
    assert!(observed.events.is_empty());
    let mut observed = Observations::default();
    assert!(
        plan(
            Tool::Targo,
            &words(&["--unverified", "verify"]),
            &[("CARGO_ALIAS_VERIFY", "metadata")],
            Some(config),
            &mut observed
        )
        .is_empty()
    );
    assert!(observed.events.is_empty());
}

#[test]
fn an_explicit_root_suppresses_discovery_and_empty_root_names_none() {
    let args = words(&["--unverified", "build"]);
    let mut observed = Observations::default();
    assert!(
        plan(
            Tool::Targo,
            &args,
            &[("SDKROOT", "/explicit/Other.sdk")],
            None,
            &mut observed
        )
        .is_empty()
    );
    assert!(observed.events.is_empty());
    let mut observed = Observations::default();
    assert_eq!(
        plan(Tool::Targo, &args, &[("SDKROOT", "")], None, &mut observed),
        overlay(SDK)
    );
}

#[test]
fn apple_mobile_arguments_and_both_target_variables_keep_platform_sdk_lookup() {
    for target in [
        "aarch64-apple-ios",
        "aarch64-apple-ios-sim",
        "aarch64-apple-tvos",
        "arm64_32-apple-watchos",
        "aarch64-apple-visionos",
    ] {
        for args in [
            words(&["--unverified", "build", "--target", target]),
            words(&["--unverified", "build", &format!("--target={target}")]),
        ] {
            let mut observed = Observations::default();
            assert!(
                plan(Tool::Targo, &args, &[], None, &mut observed).is_empty(),
                "{target}"
            );
            assert!(observed.events.is_empty());
        }
        for name in ["CARGO_BUILD_TARGET", "TARGO_BUILD_TARGET"] {
            let mut observed = Observations::default();
            assert!(
                plan(
                    Tool::Targo,
                    &words(&["--unverified", "build"]),
                    &[(name, target)],
                    None,
                    &mut observed
                )
                .is_empty()
            );
            assert!(observed.events.is_empty());
        }
    }
    let mut observed = Observations::default();
    assert!(
        plan(
            Tool::Targo,
            &words(&["--unverified", "ios"]),
            &[],
            Some("[alias]\nios = ['build', '--target', 'aarch64-apple-ios']\n"),
            &mut observed
        )
        .is_empty()
    );
    assert!(observed.events.is_empty());
    let mut observed = Observations::default();
    assert_eq!(
        plan(
            Tool::Targo,
            &words(&["--unverified", "build", "--target", "aarch64-apple-darwin"]),
            &[],
            None,
            &mut observed
        ),
        overlay(SDK)
    );
}

#[test]
fn configured_platforms_gate_sdk_probes_and_application_data_does_not() {
    for (config, native) in [
        (
            "[build]\nrustflags=['--cfg', 'native']\ntarget-dir='cache'",
            true,
        ),
        (
            "[build]\ntarget=['aarch64-apple-darwin', 'x86_64-apple-darwin']",
            true,
        ),
        ("[build]\ntarget='aarch64-apple-ios'", false),
        ("[build]\ntarget='custom-aarch64-apple-darwin.json'", false),
        (
            "[build]\ntarget=['aarch64-apple-darwin', 'aarch64-apple-ios']",
            false,
        ),
        ("[build]\ntarget=[", false),
    ] {
        let mut observed = Observations::default();
        let result = plan(
            Tool::Targo,
            &words(&["--unverified", "build"]),
            &[],
            Some(config),
            &mut observed,
        );
        assert_eq!(
            result,
            if native { overlay(SDK) } else { Vec::new() },
            "{config}"
        );
        assert_eq!(observed.events.is_empty(), !native, "{config}");
    }
    let mut observed = Observations::default();
    assert_eq!(
        plan(
            Tool::Targo,
            &words(&[
                "--unverified",
                "run",
                "--",
                "--target",
                "aarch64-apple-ios",
                "--config",
                "missing.toml"
            ]),
            &[],
            None,
            &mut observed
        ),
        overlay(SDK)
    );
}

#[test]
fn only_a_typed_directory_change_selects_the_platform_config() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    std::fs::create_dir_all(home.join("mobile")).unwrap();
    std::fs::create_dir_all(home.join("native")).unwrap();
    let home = home.canonicalize().unwrap();
    let mobile = home.join("mobile");
    let native = home.join("native");
    let read = |path: &Path| {
        Ok(if path == mobile.join(".cargo/config.toml") {
            Some(
                "[build]\ntarget='aarch64-apple-ios'\n[alias]\nnative='-C ../native build'"
                    .to_owned(),
            )
        } else if path == native.join(".cargo/config.toml") {
            Some("[alias]\nmobile='-C ../mobile build'".to_owned())
        } else {
            None
        })
    };
    let env = |name: &str| (name == "__CARGO_TEST_ROOT").then(|| home.clone().into_os_string());
    for (cwd, args, uses_macos) in [
        (mobile.as_path(), vec!["--unverified", "native"], false),
        (native.as_path(), vec!["--unverified", "mobile"], true),
        (
            mobile.as_path(),
            vec![
                "--unverified",
                "-Zunstable-options",
                "-C",
                "../native",
                "build",
            ],
            true,
        ),
        (
            native.as_path(),
            vec![
                "--unverified",
                "-Zunstable-options",
                "-C",
                "../mobile",
                "build",
            ],
            false,
        ),
        // The real typed change applies; the alias's later -C still does not.
        (
            mobile.as_path(),
            vec![
                "--unverified",
                "-Zunstable-options",
                "-C",
                "../native",
                "mobile",
            ],
            true,
        ),
    ] {
        let machine = Machine {
            home: Some(&home),
            root: None,
            cwd: Some(cwd),
            env: &env,
            read: &read,
            read_manifest: &|_| Ok(None),
            writable: &|_| None,
        };
        let mut observed = Observations::default();
        assert_eq!(
            environment_with(Tool::Targo, &words(&args), &machine, &mut observed),
            if uses_macos { overlay(SDK) } else { Vec::new() },
            "{cwd:?}: {args:?}"
        );
        assert_eq!(observed.events.is_empty(), !uses_macos, "{cwd:?}: {args:?}");
    }
}

#[test]
fn failed_tool_availability_or_selection_cannot_launch_xcrun() {
    for program in [SELECT, XCRUN] {
        let mut observed = Observations {
            unavailable: Some(program.into()),
            ..Observations::default()
        };
        assert!(
            plan(
                Tool::Targo,
                &words(&["--unverified", "build"]),
                &[],
                None,
                &mut observed
            )
            .is_empty()
        );
        assert!(
            !observed
                .events
                .iter()
                .any(|event| matches!(event, Event::Run(..)))
        );
    }
    let mut observed = Observations {
        selected: false,
        ..Observations::default()
    };
    assert!(
        plan(
            Tool::Targo,
            &words(&["--unverified", "build"]),
            &[],
            None,
            &mut observed
        )
        .is_empty()
    );
    assert!(
        !observed
            .events
            .iter()
            .any(|event| matches!(event, Event::Run(path, _) if path == Path::new(XCRUN)))
    );
}

#[test]
fn failed_or_noncanonical_sdk_answers_cannot_become_environment_inputs() {
    for answer in [
        None,
        Some(Vec::new()),
        Some(b"relative/SDK\n".to_vec()),
        Some(b"/\n".to_vec()),
        Some(b"/one.sdk\n/two.sdk\n".to_vec()),
        Some(b"/Some.sdk\0\n".to_vec()),
        Some(b"/Some.sdk\r\n".to_vec()),
        Some(vec![b'x'; OUTPUT_LIMIT + 1]),
        Some(vec![b'/', 0xff, b'\n']),
    ] {
        let mut observed = Observations {
            answer,
            ..Observations::default()
        };
        assert!(
            plan(
                Tool::Targo,
                &words(&["--unverified", "build"]),
                &[],
                None,
                &mut observed
            )
            .is_empty()
        );
        assert!(
            !observed
                .events
                .iter()
                .any(|event| matches!(event, Event::Directory(..)))
        );
    }
    let mut observed = Observations {
        directory: false,
        ..Observations::default()
    };
    assert!(
        plan(
            Tool::Targo,
            &words(&["--unverified", "build"]),
            &[],
            None,
            &mut observed
        )
        .is_empty()
    );
}

#[test]
fn discovery_preserves_sdk_path_spaces_and_does_not_cache_later_selection() {
    let args = words(&["--unverified", "build"]);
    let mut observed = Observations {
        answer: Some(b"/Developer SDKs/SDK \n\n".to_vec()),
        ..Observations::default()
    };
    assert_eq!(
        plan(Tool::Targo, &args, &[], None, &mut observed),
        overlay("/Developer SDKs/SDK ")
    );
    observed.answer = Some(b"/Another Developer/SDK\n".to_vec());
    assert_eq!(
        plan(
            Tool::Targo,
            &args,
            &[("DEVELOPER_DIR", "/Another Developer")],
            None,
            &mut observed
        ),
        overlay("/Another Developer/SDK")
    );
    assert_eq!(
        observed
            .events
            .iter()
            .filter(|event| matches!(event, Event::Run(path, _) if path == Path::new(XCRUN)))
            .count(),
        2
    );
}

#[test]
fn non_utf8_arguments_remain_an_untouched_pass_through_without_probes() {
    use std::os::unix::ffi::OsStringExt as _;
    let mut args = words(&["--unverified", "build", "--manifest-path"]);
    args.push(OsString::from_vec(vec![0xff]));
    let before = args.clone();
    let mut observed = Observations::default();
    assert!(plan(Tool::Targo, &args, &[], None, &mut observed).is_empty());
    assert_eq!(args, before);
    assert!(observed.events.is_empty());
}
