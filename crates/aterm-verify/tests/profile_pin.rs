// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build-graph facts the gate's wall clock rests on, pinned so they cannot
//! silently regress.
//!
//! They live OUTSIDE this crate (the root manifest, build scripts), and none
//! of them breaks a test when it regresses — it only makes `tools/verify.sh`
//! slower by recompiling what it already compiled. That is exactly how the 14 h
//! `--fast` run of 2026-09-10 went unnoticed, so the facts are asserted here,
//! where the gate's own tests run on every merge.
//!
//! No manifest parser: this crate has no dependencies on purpose (see its
//! Cargo.toml), and a line scan is all these facts need.

use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/aterm-verify sits two levels below the workspace root")
        .to_path_buf()
}

/// The value of `key` in the `[table]` of a TOML file, as written (quotes kept),
/// with any trailing comment removed. `None` if the table or the key is absent.
fn table_value(toml: &str, table: &str, key: &str) -> Option<String> {
    let header = format!("[{table}]");
    let mut in_table = false;
    for raw in toml.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_table = line == header;
            continue;
        }
        if !in_table {
            continue;
        }
        if let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// 2026-09-13. Without this pin, host code (build scripts, proc-macros and their
/// deps) is compiled at debuginfo 0 in a plain dev build but at full debuginfo
/// when the same crate is also a normal dependency of a test — so `build`,
/// `test`, `test --doc`, the smokes and the drivers each compiled a DIFFERENT
/// variant of syn/quote/proc-macro2 and everything downstream of them. Unit
/// graphs at 18f19eea6 with the pin: the doctest stage needs 0 compile units
/// beyond `test --workspace --no-run` (was 117); the smoke build 22 (was 49).
/// The pin changes debuginfo only — no feature, opt-level or assertion moves.
#[test]
fn the_dev_build_override_pins_host_debuginfo() {
    let manifest = workspace_root().join("Cargo.toml");
    let toml = fs::read_to_string(&manifest).expect("read the root Cargo.toml");
    let debug = table_value(&toml, "profile.dev.build-override", "debug");
    assert!(
        matches!(debug.as_deref(), Some("2" | "true" | "\"full\"")),
        "{}: [profile.dev.build-override] must set `debug = 2` (full debuginfo, the \
         level the same crates get as ordinary deps), found {debug:?} — without it every \
         gate stage recompiles its own proc-macro variants",
        manifest.display()
    );
}

/// The debuginfo level a `debug = ...` value names, spelled one way: `0`, `1`,
/// `2`, or the string for the two line-table levels. `None` for a spelling
/// cargo does not accept, so an unrecognised value can never compare equal.
fn debug_level(raw: &str) -> Option<&'static str> {
    match raw {
        "0" | "false" | "\"none\"" => Some("0"),
        "1" | "\"limited\"" => Some("1"),
        "2" | "true" | "\"full\"" => Some("2"),
        "\"line-directives-only\"" => Some("line-directives-only"),
        "\"line-tables-only\"" => Some("line-tables-only"),
        _ => None,
    }
}

/// A TOML header or dotted key as one dotted path, spaces and quotes dropped:
/// `profile . dev.package."*"` is `profile.dev.package.*`.
fn key_path(raw: &str) -> String {
    raw.split('.')
        .map(|part| part.trim().trim_matches('"'))
        .collect::<Vec<_>>()
        .join(".")
}

/// Every `debug` setting written below a table header, as (the table that owns
/// it, the value as written). Sees the three line-level spellings: `debug = 1`
/// under `[profile.dev.package.syn]`, `package.syn.debug = 1` under
/// `[profile.dev]`, and `syn = { debug = 1 }` under `[profile.dev.package]`.
fn debug_settings(toml: &str) -> Vec<(String, String)> {
    let mut table = String::new();
    let mut found = Vec::new();
    for raw in toml.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            table = key_path(line.trim_matches(|c| c == '[' || c == ']'));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let path = format!("{table}.{}", key_path(key));
        let value = value.trim();
        if let Some(owner) = path.strip_suffix(".debug") {
            found.push((owner.to_string(), value.to_string()));
        } else if let Some(inner) = value.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
            for pair in inner.split(',') {
                if let Some((k, v)) = pair.split_once('=')
                    && key_path(k) == "debug"
                {
                    found.push((path.clone(), v.trim().to_string()));
                }
            }
        }
    }
    found
}

/// Why the manifest text `toml` compiles host units at a different debuginfo
/// level than the normal units a test build shares them with, or `None` if it
/// does not.
fn host_debug_split(toml: &str) -> Option<String> {
    // Cargo's defaults: `[profile.dev]` is full debuginfo, and its build-override
    // is debuginfo 0 — the split the pin exists to close.
    let dev_raw = table_value(toml, "profile.dev", "debug");
    let host_raw = table_value(toml, "profile.dev.build-override", "debug");
    let dev = dev_raw.as_deref().map_or(Some("2"), debug_level);
    let host = host_raw.as_deref().map_or(Some("0"), debug_level);
    if dev.is_none() || host.is_none() || dev != host {
        return Some(format!(
            "[profile.dev.build-override] debug = {} but [profile.dev] debug = {}",
            host_raw.as_deref().unwrap_or("<absent, so 0>"),
            dev_raw.as_deref().unwrap_or("<absent, so 2>"),
        ));
    }
    for (owner, value) in debug_settings(toml) {
        // `cargo test` builds under `[profile.test]`, which inherits
        // `[profile.dev]`; any `debug` there (or in its build-override / package
        // tables) moves the test build's units away from the ones `build` and
        // the smokes share.
        if owner == "profile.test" || owner.starts_with("profile.test.") {
            return Some(format!(
                "[{owner}] overrides debuginfo with `debug = {value}`"
            ));
        }
        // A `[profile.dev.package.*]` table (or anything nested under the
        // build-override) outranks the build-override for the crates it names.
        // Measured 2026-09-13 on a unit graph: cargo applies such a table to a
        // crate's host AND target copies alike, so on its own it does not split
        // one crate in two — but it moves those crates off the single pinned
        // level this scan models, and a line scan cannot tell which crates `"*"`
        // or a name covers. Refused whenever the level differs from the pin.
        if (owner.starts_with("profile.dev.package.")
            || owner.starts_with("profile.dev.build-override."))
            && debug_level(&value) != host
        {
            return Some(format!(
                "[{owner}] sets `debug = {value}` but host units are pinned at debuginfo {}",
                host_raw.as_deref().unwrap_or("<absent, so 0>"),
            ));
        }
    }
    None
}

/// The pin above holds only while it EQUALS what the same crates get as
/// ordinary deps. A later `[profile.dev] debug = 1` (a common build-speed
/// tweak), a `debug` under `[profile.test]`, or a per-package `debug` at another
/// level would move units off `debug = 2` with the pin still sitting here, and
/// the first test could not see it.
#[test]
fn host_debuginfo_equals_the_debuginfo_of_the_units_it_is_shared_with() {
    let manifest = workspace_root().join("Cargo.toml");
    let toml = fs::read_to_string(&manifest).expect("read the root Cargo.toml");
    if let Some(why) = host_debug_split(&toml) {
        panic!(
            "{}: {why} — host units (build scripts, proc-macros, syn/quote) would again \
             be compiled in two variants, once per gate stage",
            manifest.display()
        );
    }
}

/// The check has teeth: each manifest shape that re-splits the host units is
/// named, and the shapes that do not are passed.
#[test]
fn the_host_debuginfo_check_refuses_every_split() {
    const PIN: &str = "[profile.dev.build-override]\ndebug = 2\n";
    let split = [
        format!("[profile.dev]\ndebug = 1\n\n{PIN}"),
        format!("[profile.dev]\ndebug = \"line-tables-only\" # faster links\n{PIN}"),
        format!("[profile.dev]\nincremental = true\n{PIN}[profile.test]\ndebug = 1\n"),
        format!("{PIN}[profile.test]\ndebug = 2\n"),
        format!("{PIN}[profile.test.build-override]\ndebug = 0\n"),
        "[profile.dev]\nopt-level = 0\n".to_string(),
        "[profile.dev.build-override]\ndebug = \"bogus\"\n".to_string(),
        format!("{PIN}[profile.dev.package.\"*\"]\ndebug = 1\n"),
        format!("{PIN}[profile.dev.package.syn]\ndebug = 0\n"),
        format!("{PIN}[profile.dev.package.syn]\ndebug = \"bogus\"\n"),
        format!("{PIN}[profile.dev]\npackage.\"*\".debug = 1\n"),
        format!("{PIN}[profile.dev.package]\nsyn = {{ opt-level = 0, debug = 0 }}\n"),
    ];
    for toml in &split {
        assert!(
            host_debug_split(toml).is_some(),
            "a split manifest passed:\n{toml}"
        );
    }
    let aligned = [
        PIN.to_string(),
        format!("[profile.dev]\nopt-level = 0\n{PIN}"),
        format!("[profile.dev]\ndebug = true\n\n{PIN}"),
        "[profile.dev]\ndebug = 1\n[profile.dev.build-override]\ndebug = \"limited\"\n".to_string(),
        format!("{PIN}[profile.profiling]\ninherits = \"release\"\ndebug = 1\n"),
        // The real manifest's only per-package dev table.
        format!("[profile.dev.package.aterm-census]\nopt-level = 2\n\n{PIN}"),
        format!("{PIN}[profile.dev.package.\"*\"]\ndebug = \"full\"\n"),
        format!("{PIN}[profile.release.package.syn]\ndebug = 0\n"),
    ];
    for toml in &aligned {
        assert_eq!(
            host_debug_split(toml),
            None,
            "an aligned manifest was refused:\n{toml}"
        );
    }
}
