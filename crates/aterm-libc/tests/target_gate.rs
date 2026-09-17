// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! TARGET-GATE PARITY: every triple this repository configures a build lane for must be a
//! cell `crates/aterm-libc/src/lib.rs` accepts.
//!
//! THE CLASS THIS GUARDS. `libc` is at the bottom of aterm's graph, so its target gate is the
//! FIRST thing a cross build meets and the last word on which triples exist. The gate is a
//! closed list ending in `compile_error!`, on purpose — an unlisted target must stop rather
//! than inherit some other cell's numbers. The cost of a closed list is that adding a lane
//! somewhere else in the repo does not add a cell here, and the failure is not subtle: the
//! lane dies on line one of the graph, before a single line of the platform code it was
//! opened to validate is read. Nothing about the lane's own configuration looks wrong
//! afterwards, which is why it can stay dead for days.
//!
//! IT DID. `.cargo/config.toml` has carried a `[target.x86_64-pc-windows-gnu]` linker/ar pair
//! for the Windows cfg-validation cross-build since long before this crate existed, and
//! `rust-toolchain.toml` names that build among the lanes that ride upstream stable. When
//! aterm's first-party `libc` replaced the crates.io package, `x86_64-pc-windows-gnu` was not
//! among the cells it listed, and the lane died on this gate. `crates/aterm/Cargo.toml` had
//! recorded it as "dead since the libc swap" — a comment, read by nobody with a compiler.
//! Measured on x86_64 Linux, 2026-09-16, at the commit before this file:
//!
//! ```text
//! $ cargo +stable check --target x86_64-pc-windows-gnu -p aterm
//! error: aterm-libc has no generated ABI cell for this target
//!   --> crates/aterm-libc/src/lib.rs:129:1
//! error: could not compile `libc` (lib) due to 1 previous error
//! ```
//!
//! THE LAW. Read the triples this repository names as build lanes out of its own committed
//! configuration, and require the gate to accept every one of them:
//!
//!   * `.cargo/config.toml` — every `[target.<triple>]` table (the `cfg(...)` tables are not
//!     triples and are skipped). A linker or an `ar` configured for a triple is this repo
//!     saying it builds for that triple.
//!   * `crates/aterm-forge/src/resolve.rs` — the `default_cells()` triples, which is what
//!     `xtask gate cells` really compiles.
//!   * `tools/cross-cell-gate.tsv` — the triple column of every `cshim` row, each of which
//!     exists so that a named triple can be type-checked on a box without its C toolchain.
//!
//! ONE-DIRECTIONAL, deliberately. A cell the gate accepts and no lane names is not a
//! violation: `x86_64-apple-darwin` (the release's compat slice) and
//! `aarch64-unknown-linux-gnu` are real targets whose lanes live in `aterm-release`'s build
//! plan and in the shipped Linux release script rather than in a config table, and a law that
//! demanded a `[target.…]` entry for them would be a law about where lanes are written down.
//! The direction that has teeth is the one that was violated.
//!
//! SCOPE AND LIMITS, said out loud so a green run is not read as more than it is:
//!
//!   * This is a STRING law over committed text, not a compile. It proves the gate would not
//!     reject the lane; it does not prove the lane builds. `xtask gate cells` is the repo's
//!     authority on that and stays it — this file is the cheap standing guard that rides
//!     along with `cargo test` on whatever box the change is being written on, the same
//!     bargain `crates/atpkg/tests/platform_cfg_parity.rs` states for its own class.
//!   * The triple is decomposed the way rustc spells its `target_*` cfgs
//!     (`<arch>-<vendor>-<os>[-<env>]`, `darwin` reading as `macos`), because the gate is
//!     written in those cfgs and a triple is the only thing the lanes write down. A cell
//!     predicate this file cannot parse is treated as MATCHING NOTHING, so an unreadable gate
//!     fails loudly rather than passing vacuously.
//!   * Only this crate's gate. The class is repo-wide; this is where a lane dies first.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// THE REPOSITORY
// ---------------------------------------------------------------------------

/// The workspace root: `crates/aterm-libc/..`/`..`.
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("crates/aterm-libc has two ancestors")
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("target-gate guard: cannot read {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// THE TRIPLE
// ---------------------------------------------------------------------------

/// The `target_*` cfg values rustc derives from a triple, as far as this gate's predicates
/// ever ask: arch, os and env. `env` is empty when the triple names none.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetFacts {
    arch: String,
    os: String,
    env: String,
}

/// `<arch>-<vendor>-<os>[-<env>]`, with the two spellings rustc does not take literally:
/// `*-apple-darwin` is `target_os = "macos"`, and a two-field triple (`<arch>-<os>`) has no
/// vendor. Returns `None` for anything this file cannot decompose, which then matches no
/// cell and fails the law loudly.
fn facts_of(triple: &str) -> Option<TargetFacts> {
    let parts: Vec<&str> = triple.split('-').collect();
    let (arch, os, env) = match parts.as_slice() {
        [arch, os] => (*arch, *os, ""),
        [arch, _vendor, os] => (*arch, *os, ""),
        [arch, _vendor, os, env] => (*arch, *os, *env),
        _ => return None,
    };
    let os = if os == "darwin" { "macos" } else { os };
    Some(TargetFacts {
        arch: arch.to_string(),
        os: os.to_string(),
        env: env.to_string(),
    })
}

// ---------------------------------------------------------------------------
// THE GATE
// ---------------------------------------------------------------------------

/// The marker a cell predicate carries when this file could not read it. Chosen so no real
/// `target_arch` value can collide with it.
const UNPARSED: &str = "\u{0}unparsed";

/// One accepted cell: the `target_*` keys an `all(...)` arm of the gate requires.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cell {
    arch: Option<String>,
    os: Option<String>,
    env: Option<String>,
}

impl Cell {
    /// Does this cell admit `facts`? Every key the cell NAMES must be equal; keys it does not
    /// name are unconstrained (`all(target_os = "macos", target_arch = "aarch64")` names no
    /// env, and `aarch64-apple-darwin` has none).
    fn admits(&self, facts: &TargetFacts) -> bool {
        if self.arch.as_deref() == Some(UNPARSED) {
            return false;
        }
        let want = |k: &Option<String>, have: &str| k.as_deref().is_none_or(|v| v == have);
        want(&self.arch, &facts.arch) && want(&self.os, &facts.os) && want(&self.env, &facts.env)
    }

    /// The cell an `all(...)` arm this file cannot read becomes: it admits nothing, so an
    /// unreadable gate fails the law rather than widening it.
    fn unreadable() -> Self {
        Cell {
            arch: Some(UNPARSED.to_string()),
            os: None,
            env: None,
        }
    }
}

/// The cells `src/lib.rs`'s closed list accepts.
///
/// Read out of the `#[cfg(not(any(…)))]` that guards the `compile_error!`, which is the one
/// place the policy is stated. Panics rather than returning an empty set if the shape it
/// expects is gone: a parser that quietly stops finding cells would make this whole file pass
/// forever, and `the_scan_still_sees_the_gate` is the standing check on that.
fn gate_cells() -> Vec<Cell> {
    let src = read("crates/aterm-libc/src/lib.rs");
    let marker = "compile_error!(\"aterm-libc has no generated ABI cell for this target\")";
    let at = src
        .find(marker)
        .expect("target-gate guard: src/lib.rs no longer carries the no-cell compile_error!");
    let head = &src[..at];
    let open = head.rfind("#[cfg(not(any(").expect(
        "target-gate guard: the no-cell compile_error! is no longer guarded by \
         `#[cfg(not(any(`. If the gate changed shape, teach this file the new one.",
    );
    let body_start = open + "#[cfg(not(any(".len();
    let body_end = head[body_start..]
        .find(")))]")
        .map(|i| body_start + i)
        .expect("target-gate guard: unterminated `#[cfg(not(any(` before the compile_error!");
    let body = &head[body_start..body_end];

    let mut cells = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("all(") {
        let inner_start = i + "all(".len();
        let inner_end = rest[inner_start..]
            .find(')')
            .map(|j| inner_start + j)
            .expect("target-gate guard: unterminated `all(` inside the gate");
        cells.push(parse_cell(&rest[inner_start..inner_end]));
        rest = &rest[inner_end + 1..];
    }
    cells
}

/// `target_os = "windows", target_env = "gnu", target_arch = "x86_64"` -> a [`Cell`].
/// A key this file does not know makes the cell admit nothing, so a gate that grows a new
/// axis fails the law instead of silently widening it.
fn parse_cell(inner: &str) -> Cell {
    let mut cell = Cell {
        arch: None,
        os: None,
        env: None,
    };
    for clause in inner.split(',') {
        let clause = clause.trim();
        if clause.is_empty() {
            continue;
        }
        let Some((key, value)) = clause.split_once('=') else {
            return Cell::unreadable();
        };
        let value = value.trim().trim_matches('"').to_string();
        match key.trim() {
            "target_arch" => cell.arch = Some(value),
            "target_os" => cell.os = Some(value),
            "target_env" => cell.env = Some(value),
            _ => return Cell::unreadable(),
        }
    }
    cell
}

// ---------------------------------------------------------------------------
// THE LANES
// ---------------------------------------------------------------------------

/// Every `[target.<triple>]` table in `.cargo/config.toml`. The `cfg(...)` tables are
/// rustflags policy, not triples, and are skipped by name.
fn cargo_config_triples() -> BTreeSet<String> {
    read(".cargo/config.toml")
        .lines()
        .filter_map(|l| l.trim().strip_prefix("[target."))
        .filter_map(|l| l.strip_suffix(']'))
        .filter(|k| !k.starts_with('\'') && !k.starts_with('"') && !k.contains("cfg("))
        .map(str::to_string)
        .collect()
}

/// The triples `aterm_forge::resolve::default_cells()` names — what `xtask gate cells`
/// really compiles. Read from source rather than linked, so this crate (the bottom of the
/// graph) gains no dependency on a crate above it.
fn forge_cell_triples() -> BTreeSet<String> {
    let src = read("crates/aterm-forge/src/resolve.rs");
    let start = src
        .find("pub fn default_cells()")
        .expect("target-gate guard: aterm-forge no longer defines `default_cells`");
    src[start..]
        .lines()
        .take_while(|l| !l.starts_with('}'))
        .filter_map(|l| l.trim().strip_prefix("triple: \""))
        .filter_map(|l| l.split('"').next())
        .map(str::to_string)
        .collect()
}

/// The triple column of every `cshim` row in the cross-cell policy. A row exists so that its
/// triple can be type-checked on a box without that triple's C toolchain; a triple worth a
/// row is a triple this repo builds for.
fn cross_cell_gate_triples() -> BTreeSet<String> {
    read("tools/cross-cell-gate.tsv")
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let mut cols = l.split('\t');
            if cols.next()? != "cshim" {
                return None;
            }
            cols.nth(1).map(str::to_string)
        })
        .collect()
}

/// Every lane triple, with the file that names it, for a failure message that points at the
/// thing to read.
fn declared_lanes() -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for (triples, source) in [
        (cargo_config_triples(), ".cargo/config.toml"),
        (forge_cell_triples(), "crates/aterm-forge/src/resolve.rs"),
        (cross_cell_gate_triples(), "tools/cross-cell-gate.tsv"),
    ] {
        for t in triples {
            if !out.iter().any(|(seen, _)| *seen == t) {
                out.push((t, source));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// THE TESTS
// ---------------------------------------------------------------------------

#[test]
fn every_configured_build_lane_is_a_cell_this_crate_accepts() {
    let cells = gate_cells();
    let mut dead: Vec<String> = Vec::new();
    for (triple, source) in declared_lanes() {
        let Some(facts) = facts_of(&triple) else {
            dead.push(format!(
                "  {triple}  (named by {source}) — this guard cannot decompose that triple"
            ));
            continue;
        };
        if !cells.iter().any(|c| c.admits(&facts)) {
            dead.push(format!(
                "  {triple}  (named by {source}) — target_arch = \"{}\", target_os = \"{}\", \
                 target_env = \"{}\"",
                facts.arch, facts.os, facts.env
            ));
        }
    }
    assert!(
        dead.is_empty(),
        "these triples have a build lane in this repository and NO cell in \
         crates/aterm-libc/src/lib.rs, so every build of that lane dies on `aterm-libc has no \
         generated ABI cell for this target` before one line of platform code is read:\n{}\n\n\
         Add the cell to the `#[cfg(not(any(…)))]` list. A Windows or wasm cell needs no \
         generated module — nothing in aterm's graph names a libc item there — but it does \
         need to be LISTED, because the list is the whole of the policy.",
        dead.join("\n")
    );
}

#[test]
fn the_scan_still_sees_the_gate() {
    // A parser that quietly stopped finding anything would make the law above pass forever.
    let cells = gate_cells();
    assert!(
        cells.len() >= 6,
        "the gate parser found only {} cell(s) in crates/aterm-libc/src/lib.rs; it has carried \
         at least six since the Windows-gnu cell landed. The parser or the gate changed shape.",
        cells.len()
    );
    assert!(
        cells.iter().all(|c| c.arch.as_deref() != Some(UNPARSED)),
        "the gate carries an `all(…)` arm this guard cannot read: {cells:?}"
    );
    // Each reader on its own, NOT the merged list: the merge is de-duplicated, and every
    // triple `tools/cross-cell-gate.tsv` names is also a forge cell, so a broken tsv reader
    // would be invisible in the merged attribution.
    for (triples, source) in [
        (cargo_config_triples(), ".cargo/config.toml"),
        (forge_cell_triples(), "crates/aterm-forge/src/resolve.rs"),
        (cross_cell_gate_triples(), "tools/cross-cell-gate.tsv"),
    ] {
        assert!(
            !triples.is_empty(),
            "no build-lane triple was read out of {source}; the reader is broken or the file \
             changed shape, and a lane list that silently empties makes this guard vacuous"
        );
        for t in &triples {
            assert!(
                facts_of(t).is_some(),
                "{source} names `{t}`, which this guard cannot decompose into target_* cfgs"
            );
        }
    }
}

#[test]
fn the_law_can_say_no() {
    // The matcher must be able to REFUSE, or a green run means nothing. The example has to
    // be a triple this product genuinely does not ship: `aarch64-pc-windows-msvc` was the
    // original one, and it stopped being honest the day that cell landed (2026-09-17) —
    // which is exactly the trap this assertion exists to catch, so it now names two triples
    // no lane, no cell and no artifact row in this repository mentions.
    let cells = gate_cells();
    for refused in ["i686-unknown-linux-gnu", "aarch64-pc-windows-gnu"] {
        let facts = facts_of(refused).expect("a four-field triple decomposes");
        assert!(
            !cells.iter().any(|c| c.admits(&facts)),
            "the gate now accepts `{refused}`. If that is deliberate, this test is the place \
             to say so — and `the_law_can_say_no` needs a different refused triple."
        );
    }
    let x64_win_gnu = facts_of("x86_64-pc-windows-gnu").expect("a four-field triple decomposes");
    assert!(
        cells.iter().any(|c| c.admits(&x64_win_gnu)),
        "the gate no longer accepts x86_64-pc-windows-gnu — the cell this file was written for"
    );
}

#[test]
fn a_triple_decomposes_the_way_rustc_spells_its_cfgs() {
    for (triple, arch, os, env) in [
        ("aarch64-apple-darwin", "aarch64", "macos", ""),
        ("x86_64-apple-darwin", "x86_64", "macos", ""),
        ("x86_64-unknown-linux-gnu", "x86_64", "linux", "gnu"),
        ("aarch64-unknown-linux-gnu", "aarch64", "linux", "gnu"),
        ("x86_64-pc-windows-msvc", "x86_64", "windows", "msvc"),
        ("x86_64-pc-windows-gnu", "x86_64", "windows", "gnu"),
        ("wasm32-unknown-unknown", "wasm32", "unknown", ""),
    ] {
        let facts = facts_of(triple).unwrap_or_else(|| panic!("{triple} decomposes"));
        assert_eq!(
            (facts.arch.as_str(), facts.os.as_str(), facts.env.as_str()),
            (arch, os, env),
            "{triple}"
        );
    }
}
