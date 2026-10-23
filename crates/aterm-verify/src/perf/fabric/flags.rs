// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compiler flag sanitization and inspection beside cargo's unit plan ([`super::units`]).
//! Filters [`is_env_source`] and rejects profile [`overrides`].

use aterm_json::Value;

/// Whether the environment variable `name` sets rustflags for a build.
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

/// A config `rustflags` value as cargo reads it.
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

/// Extracts configured `rustflags` from `targo config get --format json` output.
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

/// The codegen options the plan resolves per unit, which a rustflag must not set again.
const RESOLVED: [&str; 3] = ["opt-level", "debug-assertions", "overflow-checks"];

/// The flags in `flags` that set what the plan resolved.
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

/// The entry's words for the config rustflags: each key and its flags, or that there are none.
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

/// Why the config's rustflags make the plan's words untrue, or `None`.
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
        assert!(overrides(&strings(&["-Zthreads=4"])).is_empty());
        assert!(overrides(&strings(&["-C"])).is_empty());
    }

    #[test]
    fn config_rustflags_are_read_from_both_tables_as_cargo_takes_them() {
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
        assert_eq!(config_sources(None, None), Ok(Vec::new()));
        assert_eq!(describe(&[]), "rustflags from cargo config: none");
        for bad in [
            r#"{"build":{"rustflags":3}}"#,
            r#"{"build":{"rustflags":["-O",3]}}"#,
            "not json",
        ] {
            assert!(config_sources(Some(bad), None).is_err(), "{bad}");
        }
    }
}
