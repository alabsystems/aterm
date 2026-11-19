// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

type Config<'a> = (&'a str, Result<&'a str, ()>);

fn fixture<R>(
    environment: &[(&str, &str)],
    files: &[Config<'_>],
    inspect: impl FnOnce(&Machine<'_>) -> R,
) -> R {
    let root = PathBuf::from(if cfg!(windows) {
        "Z:/sdk-platform"
    } else {
        "/sdk-platform"
    });
    let home = root.join("home");
    let cwd = home.join("project");
    let env = |name: &str| {
        environment
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(*value))
            .or_else(|| (name == "__CARGO_TEST_ROOT").then(|| root.clone().into_os_string()))
    };
    let read = |path: &Path| {
        files
            .iter()
            .find(|(file, _)| path == root.join(file))
            .map_or(Ok(None), |(_, value)| {
                value.map(|text| Some(text.to_owned()))
            })
    };
    inspect(&Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &env,
        read: &read,
        read_manifest: &|_| Ok(None),
        writable: &|_| None,
    })
}

fn permitted(
    tool: Tool,
    args: &[&str],
    environment: &[(&str, &str)],
    files: &[Config<'_>],
) -> bool {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    fixture(environment, files, |machine| {
        macos_sdk_platform(&args, tool, machine)
    })
}

#[test]
fn neutral_configs_and_exact_macos_targets_allow_an_sdk_input() {
    for config in [
        "[build]\nrustflags=['--cfg', 'native']\ntarget-dir='cache'",
        "[alias]\nverify='run -p verifier --'",
        "[build]\ntarget='aarch64-apple-darwin'",
        "[build]\ntarget='x86_64-apple-darwin'",
        "[build]\ntarget=['aarch64-apple-darwin', 'x86_64-apple-darwin']",
    ] {
        for tool in [Tool::Targo, Tool::CargoCompat, Tool::Cargo] {
            assert!(
                permitted(
                    tool,
                    &["build"],
                    &[],
                    &[("home/project/.cargo/config.toml", Ok(config)),]
                ),
                "{tool:?}: {config}"
            );
        }
    }
}

#[test]
fn other_platforms_custom_targets_and_unknown_definitions_keep_compiler_discovery() {
    for config in [
        "[build]\ntarget='aarch64-apple-ios'",
        "[build]\ntarget='aarch64-apple-tvos'",
        "[build]\ntarget='arm64_32-apple-watchos'",
        "[build]\ntarget='aarch64-apple-visionos'",
        "[build]\ntarget='wasm32-unknown-unknown'",
        "[build]\ntarget='custom-aarch64-apple-darwin.json'",
        "[build]\ntarget='aarch64-apple-darwin ' ",
        "[build]\ntarget=''",
        "[build]\ntarget=[]",
        "[build]\ntarget=['aarch64-apple-darwin', 'aarch64-apple-ios']",
        "[build]\ntarget=['aarch64-apple-darwin', 1]",
        "[build]\ntarget=true",
        "[build]\ntarget={triple='aarch64-apple-darwin'}",
        "build=1",
        "[build\ntarget='aarch64-apple-darwin'",
        r#"["bu\u0069ld"]
"ta\u0072get"='aarch64-apple-ios'"#,
    ] {
        assert!(
            !permitted(
                Tool::Targo,
                &["build"],
                &[],
                &[("home/project/.cargo/config.toml", Ok(config)),]
            ),
            "{config}"
        );
    }
    assert!(!permitted(
        Tool::Targo,
        &["build"],
        &[],
        &[("home/project/.cargo/config.toml", Err(())),]
    ));
}

#[test]
fn every_walked_or_package_home_config_contributes_to_the_conservative_platform_reading() {
    for mobile in ["home/.cargo/config.toml", "home/project/other/config.toml"] {
        let environment = if mobile.contains("other") {
            vec![("CARGO_HOME", "other")]
        } else {
            Vec::new()
        };
        assert!(
            !permitted(
                Tool::Targo,
                &["build", "--target", "aarch64-apple-darwin"],
                &environment,
                &[
                    (
                        "home/project/.cargo/config.toml",
                        Ok("[build]\ntarget='aarch64-apple-darwin'\ntarget-dir='cache'")
                    ),
                    (mobile, Ok("[build]\ntarget='aarch64-apple-ios'")),
                ]
            ),
            "{mobile}"
        );
    }
    assert!(!permitted(
        Tool::Targo,
        &["build"],
        &[("TARGO_HOME", "other")],
        &[(
            "home/project/other/config.toml",
            Ok("[build]\ntarget='aarch64-apple-ios'")
        ),]
    ));
    assert!(permitted(
        Tool::Targo,
        &["build"],
        &[],
        &[
            (
                "home/project/.cargo/config",
                Ok("[build]\ntarget='aarch64-apple-darwin'")
            ),
            (
                "home/project/.cargo/config.toml",
                Ok("[build]\ntarget='aarch64-apple-ios'")
            ),
        ]
    ));
    assert!(!permitted(
        Tool::Targo,
        &["build"],
        &[],
        &[
            (
                "home/project/.cargo/config",
                Ok("[build]\ntarget='aarch64-apple-ios'")
            ),
            (
                "home/project/.cargo/config.toml",
                Ok("[build]\ntarget='aarch64-apple-darwin'")
            ),
        ]
    ));
}

#[test]
fn file_and_inline_cli_configs_use_the_same_decoded_graph() {
    for args in [
        vec!["build", "--config", "mobile.toml"],
        vec!["build", "--config=mobile.toml"],
        vec!["build", "--config", "build.target='aarch64-apple-ios'"],
        vec![
            "build",
            "--config",
            r#""bu\u0069ld"."ta\u0072get"='aarch64-apple-ios'"#,
        ],
        vec![
            "build",
            "--config",
            "mobile.toml",
            "--config",
            "build.target='aarch64-apple-darwin'",
        ],
    ] {
        assert!(
            !permitted(
                Tool::Targo,
                &args,
                &[],
                &[(
                    "home/project/mobile.toml",
                    Ok("[build]\ntarget='aarch64-apple-ios'")
                ),]
            ),
            "{args:?}"
        );
    }
    assert!(permitted(
        Tool::Targo,
        &["build", "--config", "native.toml"],
        &[],
        &[(
            "home/project/native.toml",
            Ok("[build]\ntarget=['aarch64-apple-darwin']")
        ),]
    ));
    assert!(!permitted(
        Tool::Targo,
        &["build", "--config", "unreadable.toml"],
        &[],
        &[("home/project/unreadable.toml", Err(())),]
    ));
    for args in [
        vec!["build", "--config"],
        vec!["build", "--config=missing.toml"],
        vec!["build", "--config", "build.target=["],
    ] {
        assert!(!permitted(Tool::Targo, &args, &[], &[]), "{args:?}");
    }
}

#[test]
fn includes_preserve_native_targets_and_refuse_unknown_or_mobile_graphs() {
    for (config, expected) in [
        ("include=['native.toml']", true),
        ("include=[{path='absent.toml',optional=true}]", true),
        ("include=['absent.toml']", false),
        ("include=['unreadable.toml']", false),
        (
            "include=['mobile.toml']\nbuild.target='aarch64-apple-darwin'",
            false,
        ),
        ("include=['cycle.toml']", false),
        ("include='native.toml'", false),
        ("include=[{path='absent*.toml',optional=true}]", false),
    ] {
        let files = [
            ("home/project/.cargo/config.toml", Ok(config)),
            (
                "home/project/.cargo/native.toml",
                Ok("[build]\ntarget='x86_64-apple-darwin'"),
            ),
            (
                "home/project/.cargo/mobile.toml",
                Ok("[build]\ntarget='aarch64-apple-ios'"),
            ),
            ("home/project/.cargo/unreadable.toml", Err(())),
            (
                "home/project/.cargo/cycle.toml",
                Ok("include=['config.toml']"),
            ),
        ];
        for tool in [Tool::Targo, Tool::CargoCompat] {
            assert_eq!(
                permitted(tool, &["build"], &[], &files),
                expected,
                "{tool:?}: {config}"
            );
        }
    }
    // An upstream Cargo's include capability is unknown, so even a native included target
    // is no permission to add a macOS SDK.
    assert!(!permitted(
        Tool::Cargo,
        &["build"],
        &[],
        &[
            (
                "home/project/.cargo/config.toml",
                Ok("include=['native.toml']\nbuild.target-dir='cache'")
            ),
            (
                "home/project/.cargo/native.toml",
                Ok("[build]\ntarget='aarch64-apple-darwin'")
            ),
        ]
    ));
    assert!(!permitted(
        Tool::Targo,
        &["build", "--config", "outer.toml"],
        &[],
        &[
            ("home/project/outer.toml", Ok("include=['mobile.toml']")),
            (
                "home/project/mobile.toml",
                Ok("[build]\ntarget='aarch64-apple-ios'")
            ),
        ]
    ));
}

#[test]
fn target_flags_and_environment_are_strict_but_application_words_are_not_settings() {
    for target in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
        assert!(permitted(
            Tool::Targo,
            &["build", "--target", target],
            &[],
            &[]
        ));
        assert!(permitted(
            Tool::Targo,
            &["build", &format!("--target={target}")],
            &[],
            &[]
        ));
        for name in ["CARGO_BUILD_TARGET", "TARGO_BUILD_TARGET"] {
            assert!(permitted(Tool::Targo, &["build"], &[(name, target)], &[]));
        }
    }
    for target in [
        "aarch64-apple-ios",
        "wasm32-unknown-unknown",
        "aarch64-apple-darwin.json",
        "",
    ] {
        assert!(!permitted(
            Tool::Targo,
            &["build", "--target", target],
            &[],
            &[]
        ));
        for name in ["CARGO_BUILD_TARGET", "TARGO_BUILD_TARGET"] {
            assert!(!permitted(Tool::Targo, &["build"], &[(name, target)], &[]));
        }
    }
    assert!(!permitted(Tool::Targo, &["build", "--target"], &[], &[]));
    assert!(!permitted(
        Tool::Targo,
        &["build"],
        &[
            ("CARGO_BUILD_TARGET", "aarch64-apple-darwin"),
            ("TARGO_BUILD_TARGET", "aarch64-apple-ios"),
        ],
        &[]
    ));
    assert!(permitted(
        Tool::Targo,
        &[
            "run",
            "--",
            "--target",
            "aarch64-apple-ios",
            "--config",
            "missing.toml"
        ],
        &[],
        &[]
    ));
    assert!(permitted(
        Tool::Targo,
        &["run", "program-argument", "--target=aarch64-apple-ios"],
        &[],
        &[]
    ));
}

#[test]
fn platform_observation_does_not_change_output_directory_selection() {
    for target in [
        "'aarch64-apple-ios'",
        "['aarch64-apple-ios', 'aarch64-apple-darwin']",
    ] {
        let text = format!("[build]\ntarget={target}\ntarget-dir='cache'");
        fixture(
            &[],
            &[("home/project/.cargo/config.toml", Ok(&text))],
            |machine| {
                assert_eq!(
                    cargo_target_hint_with(machine),
                    Ok(Some(machine.cwd.unwrap().join("cache")))
                );
            },
        );
    }
}

const NATIVE_CONFIG: &str = "build.target='aarch64-apple-darwin'\n";
const NEUTRAL_CONFIG: &str = "build.jobs=1\n";
const INLINE_INCLUDE: &str = "include=['native.toml']";

/// Repeated inline configs have independent Cargo cycle sets, but one SDK observer budget.
/// The injected counter observes real reader calls; no large files or expensive parsing.
fn observed_inline_configs(budget: ConfigGraphBudget, values: &[&str]) -> (bool, usize, usize) {
    let root = PathBuf::from(if cfg!(windows) {
        "Z:/sdk-observer-budget"
    } else {
        "/sdk-observer-budget"
    });
    let home = root.join("home");
    let cwd = home.join("project");
    let reads = std::cell::Cell::new(0);
    let native_reads = std::cell::Cell::new(0);
    let read = |path: &Path| {
        reads.set(reads.get() + 1);
        Ok(if path == cwd.join("native.toml") {
            native_reads.set(native_reads.get() + 1);
            Some(NATIVE_CONFIG.to_owned())
        } else if path == cwd.join(".cargo/config.toml") || path == home.join(".cargo/config.toml")
        {
            Some(NEUTRAL_CONFIG.to_owned())
        } else {
            None
        })
    };
    let env = |name: &str| (name == "__CARGO_TEST_ROOT").then(|| cwd.clone().into_os_string());
    let machine = Machine {
        home: Some(&home),
        root: None,
        cwd: Some(&cwd),
        env: &env,
        read: &read,
        read_manifest: &|_| Ok(None),
        writable: &|_| None,
    };
    let mut args = vec!["build".to_owned()];
    for value in values {
        args.extend(["--config".to_owned(), (*value).to_owned()]);
    }
    let permitted = macos_sdk_platform_budgeted(&args, Tool::Targo, &machine, budget);
    (permitted, reads.get(), native_reads.get())
}

#[test]
fn sdk_observer_text_budget_spans_all_roots_inline_values_and_repeated_includes() {
    let remaining = 2 * NEUTRAL_CONFIG.len() + 2 * (INLINE_INCLUDE.len() + NATIVE_CONFIG.len());
    let budget = || ConfigGraphBudget {
        reads: 0,
        text_bytes: CONFIG_GRAPH_MAX_BYTES - remaining,
    };
    assert_eq!(
        observed_inline_configs(budget(), &[INLINE_INCLUDE; 2]),
        (true, 8, 2)
    );
    assert_eq!(
        observed_inline_configs(budget(), &[INLINE_INCLUDE; 3]),
        (false, 8, 2)
    );
    // The same valid command fits under the real allowance; exhaustion is conservative
    // observer refusal, not a fabricated graph error or an invalid Cargo include.
    assert_eq!(
        observed_inline_configs(ConfigGraphBudget::default(), &[INLINE_INCLUDE; 3]),
        (true, 10, 3)
    );
}

#[test]
fn sdk_observer_read_budget_includes_discovered_roots_and_later_cli_graphs() {
    let budget = || ConfigGraphBudget {
        reads: CONFIG_GRAPH_MAX_READS - 8,
        text_bytes: 0,
    };
    assert_eq!(
        observed_inline_configs(budget(), &[INLINE_INCLUDE; 2]),
        (true, 8, 2)
    );
    assert_eq!(
        observed_inline_configs(budget(), &[INLINE_INCLUDE; 3]),
        (false, 8, 2)
    );
    // CLI path-read refusal ordinarily falls back to valid inline TOML. It must not turn
    // budget exhaustion into permission merely because this final graph has no includes.
    assert_eq!(
        observed_inline_configs(budget(), &[INLINE_INCLUDE, INLINE_INCLUDE, "build.jobs=2"]),
        (false, 8, 2)
    );
}
