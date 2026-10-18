// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The compiler flags the fabric lane's build gets BESIDE its profile, which
//! cargo's plan ([`super::units`]) cannot show.
//!
//! WHY. `--unit-graph` resolves the profile (`opt_level`, `debug_assertions`,
//! `overflow_checks`) and nothing else. Cargo then appends the run's
//! `rustflags` to every rustc line, and rustc takes the LAST `-C opt-level`:
//! under `RUSTFLAGS="-C opt-level=1"` the plan of the opt arm still says 3 for
//! every unit while trustc compiles at 1. So a label read off the plan is a fact
//! about the build only when no rustflags override what the plan resolved.
//!
//! WHERE RUSTFLAGS COME FROM (cargo's precedence, first one set wins):
//! `CARGO_ENCODED_RUSTFLAGS`, `RUSTFLAGS`, then every matching
//! `target.<triple>.rustflags` and `target.<cfg>.rustflags` joined, then
//! `build.rustflags`. The two config keys are also set by the environment
//! (`CARGO_TARGET_<TRIPLE>_RUSTFLAGS`, `CARGO_BUILD_RUSTFLAGS`).
//!
//! WHAT THE LANE DOES. Every environment source is the caller's, never the
//! checkout's, and AGENTS.md forbids setting one (it replaces
//! `.cargo/config.toml`'s table): the lane removes them all from every targo
//! it runs ([`is_env_source`]), as the wasm lane removes `RUSTFLAGS`. What is
//! left is the config files — the checkout's own `.cargo/config.toml`, and any
//! above it or in `$CARGO_HOME`, a user's included — which the lane reads with
//! `targo config get` and holds to one rule: no flag there may set what the
//! plan resolved ([`overrides`]). The ones it finds go on the entry's Build
//! line, so the entry names every flag beside the profile.

use aterm_json::Value;

/// Whether the environment variable `name` sets rustflags for a build: the two
/// that replace every config source, and the two config keys' environment
/// forms. Pure (unit-tested).
pub(crate) fn is_env_source(name: &str) -> bool {
    matches!(
        name,
        "RUSTFLAGS" | "CARGO_ENCODED_RUSTFLAGS" | "CARGO_BUILD_RUSTFLAGS"
    ) || name
        .strip_prefix("CARGO_TARGET_")
        .and_then(|rest| rest.strip_suffix("_RUSTFLAGS"))
        .is_some_and(|triple| !triple.is_empty())
}

/// One config key's rustflags: where they are set and the flags, split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Source {
    /// `build.rustflags`, `target.cfg(trust_verify).rustflags`, ….
    pub key: String,
    pub flags: Vec<String>,
}

/// A config `rustflags` value as cargo reads it: a string split on
/// whitespace, or an array of strings.
fn flags_of(value: &Value) -> Result<Vec<String>, String> {
    if let Some(text) = value.as_str() {
        return Ok(text.split_whitespace().map(str::to_string).collect());
    }
    value
        .as_array()
        .ok_or_else(|| {
            format!("a rustflags value that is neither a string nor an array: {value:?}")
        })?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("a rustflags entry that is not a string: {v:?}"))
        })
        .collect()
}

/// Every `rustflags` in cargo's config, from `targo config get --format json`
/// of the `build` table and of the `target` table (`None` where that table is
/// not set). Every `target.*` key is listed, matching or not: the lane does not
/// evaluate a `cfg(…)`, so a key it lists may not reach this host. Pure
/// (unit-tested).
///
/// # Errors
///
/// Text that is not the JSON `config get` prints, or a rustflags value cargo
/// would not take.
pub(crate) fn config_sources(
    build: Option<&str>,
    target: Option<&str>,
) -> Result<Vec<Source>, String> {
    let parse = |text: &str| {
        aterm_json::from_str::<Value>(text)
            .map_err(|e| format!("`targo config get` did not print JSON: {e}"))
    };
    let mut sources = Vec::new();
    if let Some(text) = build {
        let doc = parse(text)?;
        if let Some(value) = doc.get("build").and_then(|b| b.get("rustflags")) {
            sources.push(Source {
                key: "build.rustflags".to_string(),
                flags: flags_of(value)?,
            });
        }
    }
    if let Some(text) = target {
        let doc = parse(text)?;
        if let Some(tables) = doc.get("target").and_then(Value::as_object) {
            for (name, table) in tables {
                if let Some(value) = table.get("rustflags") {
                    sources.push(Source {
                        key: format!("target.{name}.rustflags"),
                        flags: flags_of(value)?,
                    });
                }
            }
        }
    }
    Ok(sources)
}

/// The codegen options the plan resolves per unit, which a rustflag must not
/// set again: rustc normalizes `_` to `-` in an option's name.
const RESOLVED: [&str; 3] = ["opt-level", "debug-assertions", "overflow-checks"];

/// The flags in `flags` that set what the plan resolved: `-O`, and `-C` /
/// `--codegen` with `opt-level`, `debug-assertions` or `overflow-checks`, in
/// every spelling rustc takes (`-C x=v`, `-Cx=v`, `--codegen x=v`,
/// `--codegen=x=v`). Pure (unit-tested).
pub(crate) fn overrides(flags: &[String]) -> Vec<String> {
    let resolved = |option: &str| {
        let name = option.split_once('=').map_or(option, |(name, _)| name);
        RESOLVED.contains(&name.replace('_', "-").as_str())
    };
    let mut found = Vec::new();
    let mut i = 0;
    while let Some(flag) = flags.get(i) {
        let (option, width) = match flag.as_str() {
            "-O" => {
                found.push(flag.clone());
                i += 1;
                continue;
            }
            "-C" | "--codegen" => (flags.get(i + 1).map(String::as_str), 2),
            other => (
                other
                    .strip_prefix("--codegen=")
                    .or_else(|| other.strip_prefix("-C")),
                1,
            ),
        };
        if let Some(option) = option.filter(|o| resolved(o)) {
            found.push(if width == 2 {
                format!("{flag} {option}")
            } else {
                flag.clone()
            });
        }
        i += width;
    }
    found
}

/// The entry's words for the config rustflags: each key and its flags, or
/// that there are none. Pure (unit-tested).
pub(crate) fn describe(sources: &[Source]) -> String {
    if sources.iter().all(|s| s.flags.is_empty()) {
        return "rustflags from cargo config: none".to_string();
    }
    let keys: Vec<String> = sources
        .iter()
        .filter(|s| !s.flags.is_empty())
        .map(|s| format!("{} = `{}`", s.key, s.flags.join(" ")))
        .collect();
    format!("rustflags from cargo config: {}", keys.join(", "))
}

/// Why the config's rustflags make the plan's words untrue, or `None`. Pure
/// (unit-tested).
pub(crate) fn refusal(sources: &[Source]) -> Option<String> {
    let found: Vec<String> = sources
        .iter()
        .filter_map(|s| {
            let bad = overrides(&s.flags);
            (!bad.is_empty()).then(|| format!("{} sets `{}`", s.key, bad.join(" ")))
        })
        .collect();
    (!found.is_empty()).then(|| {
        format!(
            "cargo config passes rustc flags that override what the plan resolved, so the plan's \
             opt-level and checks would not be what trustc compiled: {} (remove them, or move \
             them under a profile)",
            found.join("; ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(flags: &[&str]) -> Vec<String> {
        flags.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn every_environment_source_of_rustflags_is_named() {
        for name in [
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_BUILD_RUSTFLAGS",
            "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
        ] {
            assert!(is_env_source(name), "{name}");
        }
        for name in [
            "RUSTDOCFLAGS",
            "CARGO_TARGET_DIR",
            "CARGO_TARGET__RUSTFLAGS",
            "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER",
            "CARGO_BUILD_JOBS",
            "XRUSTFLAGS",
        ] {
            assert!(!is_env_source(name), "{name}");
        }
    }

    #[test]
    fn a_flag_that_sets_a_resolved_option_is_found_in_every_spelling() {
        let flags = strings(&[
            "-Zthreads=4",
            "-C",
            "opt-level=1",
            "-Cdebug-assertions=off",
            "--codegen",
            "overflow_checks=on",
            "--codegen=opt-level=s",
            "-O",
            "-C",
            "target-cpu=native",
            "-Cincremental=x",
        ]);
        assert_eq!(
            overrides(&flags),
            strings(&[
                "-C opt-level=1",
                "-Cdebug-assertions=off",
                "--codegen overflow_checks=on",
                "--codegen=opt-level=s",
                "-O",
            ])
        );
        // The checkout's own table sets nothing the plan resolved.
        assert!(overrides(&strings(&["-Zthreads=4"])).is_empty());
        // A trailing `-C` with nothing after it sets nothing.
        assert!(overrides(&strings(&["-C"])).is_empty());
    }

    #[test]
    fn config_rustflags_are_read_from_both_tables_as_cargo_takes_them() {
        // What `targo config get --format json target` printed for this
        // checkout on 2026-10-17.
        let target = r#"{"target":{"cfg(trust_verify)":{"rustflags":["-Zthreads=4"]},"x86_64-pc-windows-gnu":{"ar":"x86_64-w64-mingw32-ar","linker":"x86_64-w64-mingw32-gcc"}}}"#;
        let sources = config_sources(None, Some(target)).expect("parses");
        assert_eq!(
            sources,
            [Source {
                key: "target.cfg(trust_verify).rustflags".to_string(),
                flags: strings(&["-Zthreads=4"]),
            }]
        );
        assert_eq!(refusal(&sources), None);
        assert_eq!(
            describe(&sources),
            "rustflags from cargo config: target.cfg(trust_verify).rustflags = `-Zthreads=4`"
        );
        // A user-level `build.rustflags` string is split as cargo splits it, and
        // refused for what it sets.
        let build = r#"{"build":{"jobs":4,"rustflags":"-C  opt-level=1 -Zthreads=8"}}"#;
        let sources = config_sources(Some(build), Some(target)).expect("parses");
        assert_eq!(
            sources[0].flags,
            strings(&["-C", "opt-level=1", "-Zthreads=8"])
        );
        let why = refusal(&sources).expect("refused");
        assert!(
            why.contains("build.rustflags sets `-C opt-level=1`"),
            "{why}"
        );
        assert!(!why.contains("trust_verify"), "{why}");
        // Nothing set at all.
        assert_eq!(config_sources(None, None), Ok(Vec::new()));
        assert_eq!(describe(&[]), "rustflags from cargo config: none");
        // What cargo would not take is an error, never a pass.
        for bad in [
            r#"{"build":{"rustflags":3}}"#,
            r#"{"build":{"rustflags":["-O",3]}}"#,
            "not json",
        ] {
            assert!(config_sources(Some(bad), None).is_err(), "{bad}");
        }
    }
}
