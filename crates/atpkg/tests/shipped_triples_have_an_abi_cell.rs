// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SHIPPED TRIPLE => COMPILABLE TRIPLE: every target this client publishes rows for must be
//! one `crates/aterm-libc` will compile at all.
//!
//! THE CLASS THIS GUARDS. [`atpkg::TARGETS`] is the schema's roster of served triples, and it
//! is quoted three more times in lanes nobody compiles: `cli::current_triple` reports one of
//! them for the running client, `tools/atpkg-auto-vendor.sh` maps each to a vendor platform
//! token, and `apps/aterm-win/build.ps1` picks an `aarch64-pc-windows-` prefix by itself when
//! it detects ARM64 Windows. None of those four lanes can tell whether the triple it names can
//! be BUILT, and on 2026-09-16 one of them could not:
//!
//!   * `aarch64-pc-windows-msvc` — a `TARGETS` row, a `current_triple` arm and a vendor-map
//!     entry since each of those lists was written — stopped in the first first-party crate
//!     the graph reaches. `crates/aterm-libc` (published into the build as the package `libc`,
//!     which `[patch.crates-io]` puts under every consumer in the workspace) admits a fixed
//!     list of targets and answers every other one with
//!     `compile_error!("aterm-libc has no generated ABI cell for this target")`. The list
//!     carried `all(target_os = "windows", target_env = "msvc", target_arch = "x86_64")` and
//!     no aarch64 twin, so the ENTIRE aterm graph failed to type-check for that triple before
//!     a line of aterm was read. The index schema, the client's self-report and the vendor
//!     authoring lane all served a triple no first-party crate could be built for.
//!
//! WHY HERE. `crates/aterm-libc`'s escape list is the one place in the tree that ENUMERATES
//! the targets aterm's own code may be compiled for, and `atpkg::TARGETS` is the one place
//! that enumerates the targets aterm SHIPS to. A guard is only worth writing where two such
//! lists must agree and nothing makes them; this file is that seam, and it sits beside
//! `platform_cfg_parity.rs`, which guards the neighbouring class (a reference that stops
//! resolving on the other side of a `cfg`) for the same reason and on the same terms.
//!
//! WHY NOT ONLY IN `gate cells`. `cargo xtask gate cells` really cross-compiles, and it stays
//! the authority — but its matrix is `aterm_forge::resolve::default_cells()`, which is
//! `mac-arm`, `linux`, `win` and the two wasm rows: FOUR of the six shipped triples have no
//! cell at all, `aarch64-pc-windows-msvc` among them, and the two that do are the x86_64
//! Windows and x86_64 Linux ones. The gate is also opt-in, needs each triple's std installed
//! and costs minutes. This file is the cheap standing guard that rides along with
//! `cargo test -p atpkg` on whatever box the change is being written on: pure `std`, no
//! subprocess, no network, no new dependency, reading only committed sources under
//! `CARGO_MANIFEST_DIR`. It cannot replace a real cross-compile and does not claim to — what
//! it proves is that the escape list does not REFUSE a shipped triple outright, which is the
//! failure that was live.
//!
//! THE TWO LAWS.
//!
//!   1. For every triple in [`atpkg::TARGETS`], `aterm-libc`'s `compile_error!` predicate must
//!      evaluate FALSE. A triple the shipping lanes name may not be one the build refuses.
//!   2. For every SHIPPED UNIX triple, some `#[cfg(...)] mod <cell>;` in that same file must
//!      evaluate TRUE. An escape-list row with no cell behind it claims the target needs no
//!      POSIX ABI declarations whatsoever — true for Windows and wasm32, where every `libc::`
//!      consumer is already `cfg`-gated away and the unconditional `core::ffi` re-exports are
//!      the whole of the crate, and false for every Unix target, where the declarations ARE
//!      the crate. Without this law the cheapest way to make law 1 green on a future
//!      `aarch64-unknown-linux-musl` would be to add a bare escape row, which compiles and
//!      silently ships a libc with no libc in it.
//!
//! SCOPE AND LIMITS, said out loud so a green run is not read as more than it is:
//!
//!   * The cfg algebra understood here is `all` / `any` / `not`, the bare `unix` and `windows`
//!     predicates, and `target_arch` / `target_os` / `target_env` / `target_family` /
//!     `target_vendor` equalities. A predicate outside that grammar is an ERROR, never a
//!     silent pass: this guard is about a list that must be complete, so a row it cannot read
//!     is a row it must not vouch for.
//!   * A triple's cfg values are DERIVED from its spelling by [`TargetCfg::derive`], from a
//!     table of the os/vendor tokens this repository ships. An unknown token is likewise an
//!     error, so a seventh `TARGETS` row cannot be added without either being understood or
//!     being noticed.
//!   * Only `crates/aterm-libc` is judged. It is the bottom of the first-party graph — the
//!     crate every other one reaches through `[patch.crates-io]` — so a triple it refuses is
//!     a triple nothing above it can be compiled for. A crate HIGHER up that fails on one
//!     target is the class `platform_cfg_parity.rs` and `gate cells` cover.
//!
//! `the_evaluator_agrees_with_the_compiler_that_built_it` is the non-vacuity anchor: whatever
//! box runs this test is itself a proof that the escape list admits that box's triple, and the
//! evaluator has to agree with it. A parser that quietly matched nothing would fail there.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// THE SOURCE UNDER TEST
// ---------------------------------------------------------------------------

/// The `compile_error!` string `aterm-libc` answers an unsupported target with. Quoted so the
/// attribute above it can be found by what it GUARDS rather than by line number.
const REFUSAL: &str = "aterm-libc has no generated ABI cell for this target";

fn repo_root() -> PathBuf {
    // …/crates/atpkg -> …/crates -> …
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/atpkg must sit two levels under the workspace root")
        .to_path_buf()
}

fn libc_source() -> String {
    let path = repo_root().join("crates/aterm-libc/src/lib.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {} ({e})", path.display()))
}

// ---------------------------------------------------------------------------
// LEXING: `#[cfg(...)]` attributes and the item each one gates
// ---------------------------------------------------------------------------

/// One column-0 `#[cfg(…)]` attribute and the item line it is attached to.
///
/// Column 0 only: everything this file judges — the cell `mod`s, the `compile_error!` — is a
/// top-level item of `crates/aterm-libc/src/lib.rs`, and an indented `cfg` in that file would
/// be inside a generated cell module, which is not this guard's business.
#[derive(Debug, Clone)]
struct Gated {
    /// The predicate INSIDE `cfg(…)`, with interior newlines collapsed to spaces.
    predicate: String,
    /// The first non-attribute, non-comment, non-blank line beneath the attribute.
    item: String,
    /// 1-based line of the attribute, for a failure message that can be jumped to.
    line: usize,
}

/// Every column-0 `#[cfg(…)]` in `src`, paired with the item it gates.
fn gated_items(src: &str) -> Vec<Gated> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if !line.starts_with("#[cfg(") {
            i += 1;
            continue;
        }
        let start = i;
        // Accumulate until the attribute's parentheses balance: the escape list is seven
        // lines long, so a single-line reader would see `not(any(` and stop.
        let mut text = String::new();
        let mut depth = 0i32;
        let mut closed = false;
        while i < lines.len() {
            let l = lines[i];
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(l.trim());
            for ch in l.chars() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            closed = true;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
            if closed {
                break;
            }
        }
        if !closed {
            continue;
        }
        // `#[cfg(` … `)]` -> the inside.
        let Some(inner) = text
            .strip_prefix("#[cfg(")
            .and_then(|t| t.trim_end().strip_suffix(")]"))
        else {
            continue;
        };
        // The gated item: skip further attributes, comments and blank lines.
        let mut j = i;
        while j < lines.len() {
            let t = lines[j].trim();
            if t.is_empty() || t.starts_with("//") || t.starts_with('#') {
                j += 1;
                continue;
            }
            break;
        }
        let item = lines
            .get(j)
            .map(|l| l.trim().to_string())
            .unwrap_or_default();
        out.push(Gated {
            predicate: inner.trim().to_string(),
            item,
            line: start + 1,
        });
    }
    out
}

/// The `not(any(…))` predicate guarding the `compile_error!`, and the line it is on.
fn escape_predicate(src: &str) -> (String, usize) {
    let mut found: Vec<Gated> = gated_items(src)
        .into_iter()
        .filter(|g| g.item.contains(REFUSAL))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one `#[cfg(…)] compile_error!(\"{REFUSAL}\")` in \
         crates/aterm-libc/src/lib.rs, found {}. If the refusal moved or was reworded, this \
         guard must be re-pointed at it — a guard that cannot find its subject passes forever.",
        found.len()
    );
    let g = found.remove(0);
    (g.predicate, g.line)
}

/// The `#[cfg(…)] mod <name>;` rows: one generated ABI cell each.
fn cell_mods(src: &str) -> Vec<Gated> {
    gated_items(src)
        .into_iter()
        .filter(|g| g.item.starts_with("mod ") && g.item.ends_with(';'))
        .collect()
}

// ---------------------------------------------------------------------------
// THE CFG ALGEBRA
// ---------------------------------------------------------------------------

/// The cfg values one target triple presents to `#[cfg(…)]`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetCfg {
    triple: String,
    arch: String,
    os: String,
    env: String,
    vendor: String,
    family: String,
}

impl TargetCfg {
    /// Derive a triple's cfg values from its spelling.
    ///
    /// `<arch>-<vendor>-<os>[-<env>]`, with the os/vendor token table below covering exactly
    /// what this repository ships. Deriving rather than tabulating is deliberate: a table of
    /// six triples would have to be edited in lockstep with [`atpkg::TARGETS`], and a guard
    /// whose own roster can go stale beside the roster it guards is the defect it is here to
    /// prevent. An UNKNOWN token is an error, so nothing is guessed either.
    fn derive(triple: &str) -> Result<Self, String> {
        let parts: Vec<&str> = triple.split('-').collect();
        if parts.len() < 3 || parts.len() > 4 {
            return Err(format!(
                "`{triple}` is not `<arch>-<vendor>-<os>[-<env>]`; teach TargetCfg::derive its \
                 shape before shipping it"
            ));
        }
        let arch = parts[0].to_string();
        let vendor_token = parts[1];
        let os_token = parts[2];
        let env = parts.get(3).copied().unwrap_or("").to_string();
        // The os spelling in a triple is not always the `target_os` value: `darwin` is
        // `target_os = "macos"`, which is the whole reason this mapping is written down.
        let (os, family) = match os_token {
            "darwin" => ("macos", "unix"),
            "linux" => ("linux", "unix"),
            "windows" => ("windows", "windows"),
            // `wasm32-unknown-unknown`: `target_os = "unknown"` and NO `target_family`.
            "unknown" => ("unknown", ""),
            other => {
                return Err(format!(
                    "`{triple}`: this guard does not know what `target_os`/`target_family` the \
                     os token `{other}` denotes. Add it to TargetCfg::derive — silently \
                     guessing is how a triple ships unbuilt."
                ));
            }
        };
        let vendor = match vendor_token {
            "apple" => "apple",
            "pc" => "pc",
            "unknown" => "unknown",
            other => {
                return Err(format!(
                    "`{triple}`: unknown vendor token `{other}`. Add it to TargetCfg::derive."
                ));
            }
        };
        Ok(Self {
            triple: triple.to_string(),
            arch,
            os: os.to_string(),
            env,
            vendor: vendor.to_string(),
            family: family.to_string(),
        })
    }

    fn is_unix(&self) -> bool {
        self.family == "unix"
    }
}

/// Split a predicate list on its TOP-LEVEL commas: `all(a, b), c` is two arguments, not three.
fn arguments(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for ch in inner.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if depth == 0 => {
                out.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

/// Does `predicate` hold for `target`?
///
/// `Err` for anything outside the grammar in this file's header. FAIL-CLOSED ON PURPOSE: the
/// question asked here is "is this list complete", so a row the evaluator cannot read must
/// stop the test rather than be waved through as covered.
fn holds(predicate: &str, target: &TargetCfg) -> Result<bool, String> {
    let p = predicate.trim();
    if let Some(inner) = p.strip_prefix("all(").and_then(|s| s.strip_suffix(')')) {
        for arg in arguments(inner) {
            if !holds(&arg, target)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    if let Some(inner) = p.strip_prefix("any(").and_then(|s| s.strip_suffix(')')) {
        for arg in arguments(inner) {
            if holds(&arg, target)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if let Some(inner) = p.strip_prefix("not(").and_then(|s| s.strip_suffix(')')) {
        return Ok(!holds(inner, target)?);
    }
    if p == "unix" {
        return Ok(target.family == "unix");
    }
    if p == "windows" {
        return Ok(target.family == "windows");
    }
    if let Some((key, value)) = p.split_once('=') {
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        let actual = match key {
            "target_arch" => &target.arch,
            "target_os" => &target.os,
            "target_env" => &target.env,
            "target_vendor" => &target.vendor,
            "target_family" => &target.family,
            _ => {
                return Err(format!(
                    "cfg key `{key}` is outside this guard's grammar (predicate `{p}`). Teach \
                     `holds` what it means for a target triple, or the list it appears in \
                     cannot be judged complete."
                ));
            }
        };
        return Ok(actual == value);
    }
    Err(format!(
        "cfg predicate `{p}` is outside this guard's grammar. Teach `holds` what it means for \
         a target triple rather than letting an unreadable row count as covered."
    ))
}

/// Would `aterm-libc` refuse `target` outright?
fn refuses(escape: &str, target: &TargetCfg) -> Result<bool, String> {
    holds(escape, target)
}

/// The cfg values of the target this test binary was COMPILED for, read from the compiler
/// rather than from any table here. The non-vacuity anchor's other half.
fn compiled_target() -> TargetCfg {
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "other"
    };
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "other"
    };
    let env = if cfg!(target_env = "gnu") {
        "gnu"
    } else if cfg!(target_env = "msvc") {
        "msvc"
    } else if cfg!(target_env = "musl") {
        "musl"
    } else {
        ""
    };
    let family = if cfg!(unix) {
        "unix"
    } else if cfg!(windows) {
        "windows"
    } else {
        ""
    };
    let vendor = if cfg!(target_vendor = "apple") {
        "apple"
    } else if cfg!(target_vendor = "pc") {
        "pc"
    } else {
        "unknown"
    };
    TargetCfg {
        triple: format!("(the compiler's own: {arch}/{os}/{env})"),
        arch: arch.to_string(),
        os: os.to_string(),
        env: env.to_string(),
        vendor: vendor.to_string(),
        family: family.to_string(),
    }
}

// ---------------------------------------------------------------------------
// LAW 1 — a shipped triple is never refused outright
// ---------------------------------------------------------------------------

/// THE DEFECT THIS WAS WRITTEN FOR. `aarch64-pc-windows-msvc` is a `TARGETS` row, a
/// `current_triple` arm and a vendor-map entry, and `crates/aterm-libc` answered it with
/// `compile_error!` — so nothing in the first-party graph could be compiled for a triple three
/// shipping lanes served.
#[test]
fn every_shipped_triple_is_one_aterm_libc_will_compile() {
    let src = libc_source();
    let (escape, line) = escape_predicate(&src);
    let mut refused: Vec<String> = Vec::new();
    for triple in atpkg::TARGETS {
        let target = TargetCfg::derive(triple).unwrap_or_else(|e| panic!("{e}"));
        if refuses(&escape, &target).unwrap_or_else(|e| panic!("{e}")) {
            refused.push((*triple).to_string());
        }
    }
    assert!(
        refused.is_empty(),
        "crates/aterm-libc/src/lib.rs:{line} refuses {refused:?} with \
         `compile_error!(\"{REFUSAL}\")`, and atpkg::TARGETS publishes artifact rows for \
         them. Nothing first-party can be built for a refused triple, so the index, \
         `cli::current_triple` and tools/atpkg-auto-vendor.sh would all be serving a target \
         that does not compile. Either add the target to the escape list (with a generated \
         cell if it is a Unix target — see law 2) or take it out of TARGETS. One story, not \
         two.\n\nescape predicate: {escape}"
    );
}

// ---------------------------------------------------------------------------
// LAW 2 — a shipped Unix triple has a real cell, not just an escape row
// ---------------------------------------------------------------------------

/// A bare escape row says "this target needs no POSIX ABI declarations at all". True on
/// Windows and wasm32, where every `libc::` consumer in the graph is `cfg`-gated away and the
/// unconditional `core::ffi` re-exports are the whole crate. Never true on Unix — so the
/// cheap way to satisfy law 1 for a future Unix triple must not be available.
#[test]
fn every_shipped_unix_triple_has_a_generated_cell() {
    let src = libc_source();
    let mods = cell_mods(&src);
    assert!(
        mods.len() >= 4,
        "only {} `#[cfg(…)] mod …;` cells found in crates/aterm-libc/src/lib.rs — the four \
         Unix cells are committed, so this lexer has stopped reading the file",
        mods.len()
    );
    let mut cell_less: Vec<String> = Vec::new();
    for triple in atpkg::TARGETS {
        let target = TargetCfg::derive(triple).unwrap_or_else(|e| panic!("{e}"));
        if !target.is_unix() {
            continue;
        }
        let covered = mods.iter().any(|m| {
            holds(&m.predicate, &target).unwrap_or_else(|e| {
                panic!("crates/aterm-libc/src/lib.rs:{}: {e}", m.line);
            })
        });
        if !covered {
            cell_less.push((*triple).to_string());
        }
    }
    assert!(
        cell_less.is_empty(),
        "{cell_less:?} are shipped UNIX triples with no `mod <cell>;` behind them in \
         crates/aterm-libc/src/lib.rs. An escape-list row alone compiles and ships a libc \
         with no libc in it: every `libc::` name a Unix consumer reaches would be missing, \
         or worse, silently answered by the wrong cell. Generate the cell (the crate header \
         says how) rather than widening the escape list."
    );
}

// ---------------------------------------------------------------------------
// NON-VACUITY
// ---------------------------------------------------------------------------

/// The strongest check available without a second machine: THIS test binary exists, so the
/// compiler that built it did not hit the `compile_error!` — and the evaluator must agree with
/// that verdict on this very box. A parser that quietly matched nothing, or an evaluator that
/// answered `true` for everything, dies here.
#[test]
fn the_evaluator_agrees_with_the_compiler_that_built_it() {
    let src = libc_source();
    let (escape, line) = escape_predicate(&src);
    let me = compiled_target();
    assert!(
        !refuses(&escape, &me).unwrap_or_else(|e| panic!("{e}")),
        "crates/aterm-libc/src/lib.rs:{line} evaluates to a REFUSAL for the target this test \
         was compiled for ({me:?}) — but it compiled, so the evaluator is wrong, not the \
         file.\n\nescape predicate: {escape}"
    );
    // And the derivation has to reach the same values the compiler reports, for whichever
    // shipped triple this box is. (A box outside TARGETS — a musl or a 32-bit host — is not
    // a failure; it simply has nothing to compare.)
    if let Some(mine) = atpkg::TARGETS
        .iter()
        .filter_map(|t| TargetCfg::derive(t).ok())
        .find(|t| t.arch == me.arch && t.os == me.os && t.env == me.env)
    {
        assert_eq!(
            (mine.family.as_str(), mine.vendor.as_str()),
            (me.family.as_str(), me.vendor.as_str()),
            "TargetCfg::derive(\"{}\") disagrees with the compiler's own cfg values on this \
             box; the derivation table is wrong",
            mine.triple
        );
    }
}

/// The lexer's own obligation, pinned against the committed file rather than a fixture: the
/// escape list has a row per shipped OS/arch pair plus wasm32, and the four Unix rows each
/// have a `mod` behind them.
#[test]
fn the_scan_still_sees_the_escape_list() {
    let src = libc_source();
    let (escape, _) = escape_predicate(&src);
    let inner = escape
        .strip_prefix("not(any(")
        .and_then(|s| s.strip_suffix("))"))
        .unwrap_or_else(|| {
            panic!(
                "the refusal's guard is no longer `not(any(…))` but `{escape}` — re-read this \
                 file's laws before reshaping it"
            )
        });
    let rows = arguments(inner);
    assert!(
        rows.len() >= atpkg::TARGETS.len(),
        "{} escape rows for {} shipped triples — a row is per (os, env, arch), so it cannot \
         be fewer",
        rows.len(),
        atpkg::TARGETS.len()
    );
    assert!(
        rows.iter().any(|r| r.contains("wasm32")),
        "the wasm32 row is committed and must still be read: {rows:?}"
    );
    let mods = cell_mods(&src);
    let names: Vec<&str> = mods.iter().map(|m| m.item.as_str()).collect();
    for want in [
        "mod darwin_aarch64;",
        "mod darwin_x86_64;",
        "mod linux_gnu_x86_64;",
        "mod linux_gnu_aarch64;",
    ] {
        assert!(
            names.contains(&want),
            "`{want}` must still be seen with its own gate; saw {names:?}"
        );
    }
}

/// THE RED PROOF, against the shape of the real defect rather than a mutated constant: the
/// escape list as it stood before 2026-09-16, with the x86_64 Windows row and no aarch64 twin.
#[test]
fn the_law_names_the_triple_the_list_forgot() {
    let before = "\
#[cfg(not(any(
    all(target_os = \"macos\", target_arch = \"aarch64\"),
    all(target_os = \"macos\", target_arch = \"x86_64\"),
    all(target_os = \"linux\", target_env = \"gnu\", target_arch = \"x86_64\"),
    all(target_os = \"linux\", target_env = \"gnu\", target_arch = \"aarch64\"),
    all(target_os = \"windows\", target_env = \"msvc\", target_arch = \"x86_64\"),
    all(target_os = \"unknown\", target_arch = \"wasm32\")
)))]
compile_error!(\"aterm-libc has no generated ABI cell for this target\");
";
    let (escape, _) = escape_predicate(before);
    let refused: Vec<&&str> = atpkg::TARGETS
        .iter()
        .filter(|t| {
            let target = TargetCfg::derive(t).expect("derive");
            refuses(&escape, &target).expect("grammar")
        })
        .collect();
    assert_eq!(
        refused,
        vec![&"aarch64-pc-windows-msvc"],
        "the pre-fix escape list refused exactly one shipped triple, and law 1 must name it"
    );
}

/// The same for law 2: a Unix triple admitted by the escape list with no cell behind it is
/// caught, so "just add a row" cannot become the fix for the next musl or FreeBSD target.
#[test]
fn a_unix_escape_row_with_no_cell_behind_it_is_caught() {
    let planted = "\
#[cfg(all(target_os = \"linux\", target_env = \"gnu\", target_arch = \"x86_64\"))]
mod linux_gnu_x86_64;

#[cfg(all(target_os = \"linux\", target_env = \"gnu\", target_arch = \"aarch64\"))]
mod linux_gnu_aarch64;

#[cfg(not(any(
    all(target_os = \"linux\", target_env = \"gnu\", target_arch = \"x86_64\"),
    all(target_os = \"linux\", target_env = \"gnu\", target_arch = \"aarch64\"),
    all(target_os = \"macos\", target_arch = \"aarch64\")
)))]
compile_error!(\"aterm-libc has no generated ABI cell for this target\");
";
    let (escape, _) = escape_predicate(planted);
    let mac = TargetCfg::derive("aarch64-apple-darwin").expect("derive");
    assert!(
        !refuses(&escape, &mac).expect("grammar"),
        "the planted list admits the Darwin triple — that is the point of the fixture"
    );
    let mods = cell_mods(planted);
    assert!(
        !mods
            .iter()
            .any(|m| holds(&m.predicate, &mac).expect("grammar")),
        "…and has no cell behind it, which law 2 is what notices"
    );
}

/// An unreadable row is an ERROR, not a pass. A guard about completeness that shrugs at the
/// rows it cannot parse reports green on a list it never read.
#[test]
fn a_predicate_outside_the_grammar_stops_the_test() {
    let linux = TargetCfg::derive("x86_64-unknown-linux-gnu").expect("derive");
    let err = holds("target_has_atomic = \"64\"", &linux).expect_err("must not be judged");
    assert!(err.contains("target_has_atomic"), "{err}");
    let err = holds("feature = \"std\"", &linux).expect_err("must not be judged");
    assert!(err.contains("outside this guard's grammar"), "{err}");
    let err = holds("all(unix, target_pointer_width = \"64\")", &linux)
        .expect_err("an unreadable conjunct poisons the `all`");
    assert!(err.contains("target_pointer_width"), "{err}");
    // …while the grammar it DOES know answers.
    assert!(holds("all(unix, target_arch = \"x86_64\")", &linux).expect("grammar"));
    assert!(!holds("any(windows, target_os = \"macos\")", &linux).expect("grammar"));
    assert!(holds("not(windows)", &linux).expect("grammar"));
}

/// And a triple spelled in a way the derivation does not know is an error too — so a seventh
/// `TARGETS` row cannot be added without either being understood or being noticed.
#[test]
fn an_unknown_triple_spelling_is_refused_rather_than_guessed() {
    let err = TargetCfg::derive("x86_64-unknown-freebsd").expect_err("unknown os token");
    assert!(err.contains("freebsd"), "{err}");
    let err = TargetCfg::derive("nonsense").expect_err("not a triple");
    assert!(err.contains("arch"), "{err}");
    // The six committed spellings all derive, and each one derives DIFFERENTLY.
    let mut seen: Vec<TargetCfg> = Vec::new();
    for t in atpkg::TARGETS {
        let d = TargetCfg::derive(t).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            !seen
                .iter()
                .any(|s| s.arch == d.arch && s.os == d.os && s.env == d.env),
            "two shipped triples derive to the same cfg values: {d:?}"
        );
        seen.push(d);
    }
    assert_eq!(seen.len(), atpkg::TARGETS.len());
}
