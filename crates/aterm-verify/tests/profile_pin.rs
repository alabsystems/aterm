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
/// deps) is compiled at debuginfo 0 in a plain dev build but at the dev level
/// when the same crate is also a normal dependency of a test — so the gate's
/// compiles each built a DIFFERENT variant of syn/quote/proc-macro2 and
/// everything downstream of them. Unit graphs at 18f19eea6 with the pin: the
/// doctest stage needs 0 compile units beyond `test --workspace --no-run` (was
/// 117); the smoke build 22 (was 49). Since 2026-09-25 both levels are line
/// tables (`[profile.dev] debug = "line-tables-only"`), so this pins the dev
/// level itself and the override EQUAL to it — and refuses any other `debug`
/// under `[profile.test*]` (which `cargo test` builds with, inheriting
/// `[profile.dev]`) or `[profile.dev.package*]` (which outranks both for the
/// crates it names), either of which moves units off the one pinned level.
#[test]
fn the_dev_profile_builds_line_tables() {
    let manifest = workspace_root().join("Cargo.toml");
    let toml = fs::read_to_string(&manifest).expect("read the root Cargo.toml");
    let dev = table_value(&toml, "profile.dev", "debug");
    let host = table_value(&toml, "profile.dev.build-override", "debug");
    assert_eq!(
        (dev.as_deref(), host.as_deref()),
        (Some("\"line-tables-only\""), Some("\"line-tables-only\"")),
        "{}: [profile.dev] and [profile.dev.build-override] must both set \
         `debug = \"line-tables-only\"` (file:line backtraces, a fraction of full DWARF's size)",
        manifest.display()
    );
    let mut table = "";
    for raw in toml.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            table = line;
            continue;
        }
        let splits =
            table.starts_with("[profile.test") || table.starts_with("[profile.dev.package");
        // `debug` as a whole key — a dotted path's last part, or one inside an
        // inline table — never `debug-assertions`, which moves no debuginfo.
        let sets_debug = line
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .any(|word| word == "debug");
        assert!(
            !(splits && sets_debug),
            "{}: `{line}` under {table} moves units off the pinned debuginfo level — host \
             units (build scripts, proc-macros, syn/quote) would again be compiled in two \
             variants, once per gate stage",
            manifest.display()
        );
    }
}

/// 2026-09-26. `tools/verify.sh` compiles the gate before every invocation —
/// `--help` included — and until this date it did so with
/// `--release`: fat LTO and one codegen unit, the shipping engine's settings,
/// for a std-only program that orchestrates child processes. The gate now
/// builds under its own `[profile.gate]`, which keeps release's semantics
/// (it inherits them: no debug-assertions, no overflow checks) and drops LTO
/// and the single codegen unit. Three facts have to hold together, and a
/// regression in any one is silent — a slower start, or (for the last) a gate
/// that execs a stale binary from an older profile's directory:
///  * the profile exists with those values;
///  * every build of aterm-verify in the shim names it, never `--release`;
///  * the shim execs the binary from THAT profile's output directory, both
///    halves read from one variable.
#[test]
fn the_gate_builds_itself_under_the_lean_gate_profile_and_runs_what_it_built() {
    let root = workspace_root();
    let manifest = root.join("Cargo.toml");
    let toml = fs::read_to_string(&manifest).expect("read the root Cargo.toml");
    let gate = |key| table_value(&toml, "profile.gate", key);
    assert_eq!(
        (gate("inherits"), gate("lto"), gate("opt-level")),
        (
            Some("\"release\"".to_string()),
            Some("false".to_string()),
            Some("1".to_string())
        ),
        "{}: [profile.gate] must inherit release with lto = false and opt-level = 1",
        manifest.display()
    );
    let units: u32 = gate("codegen-units")
        .and_then(|v| v.parse().ok())
        .expect("[profile.gate] sets codegen-units");
    assert!(
        units > 1,
        "[profile.gate] codegen-units = {units}: one unit serializes codegen"
    );

    let shim = root.join("tools/verify.sh");
    let text = fs::read_to_string(&shim).expect("read tools/verify.sh");
    let code: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert!(
        code.iter().any(|l| l.trim() == "GATE_PROFILE=gate"),
        "{}: GATE_PROFILE=gate is the one place the profile is named",
        shim.display()
    );
    let builds: Vec<&&str> = code
        .iter()
        .filter(|l| l.contains(" build ") && l.contains("-p aterm-verify"))
        .collect();
    assert!(
        !builds.is_empty(),
        "{}: no gate build found",
        shim.display()
    );
    for b in &builds {
        assert!(
            b.contains("--profile \"$GATE_PROFILE\"") && !b.contains("--release"),
            "{}: the gate build must name the gate profile, never --release: {b}",
            shim.display()
        );
    }
    let exec = code
        .iter()
        .find(|l| l.trim_start().starts_with("exec "))
        .expect("the shim execs the gate");
    assert!(
        exec.contains("\"$GATE_TARGET_DIR/$GATE_PROFILE/aterm-verify\""),
        "{}: the shim must run what it built: {exec}",
        shim.display()
    );
}
