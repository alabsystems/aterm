// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shipped triple => compilable triple: every target this client publishes rows for must be
//! one `crates/aterm-libc` will compile at all.
//!
//! [`atpkg::TARGETS`] is quoted in lanes nobody compiles — `cli::current_triple`,
//! the vendor-direct asset rules, `apps/aterm-win/build.ps1` — and none
//! of them can tell whether the triple it names can be built. `crates/aterm-libc` (published
//! into the build as `libc`, which `[patch.crates-io]` puts under every consumer) admits a
//! fixed list of targets and answers every other one with `compile_error!`, so a shipped
//! triple missing from that list fails to type-check the whole aterm graph.
//!
//! Two laws. (1) For every triple in [`atpkg::TARGETS`] the `compile_error!` predicate must
//! evaluate false. (2) For every shipped Unix triple some `#[cfg(...)] mod <cell>;` must
//! evaluate true: a bare escape row claims the target needs no POSIX ABI declarations, true
//! for Windows and wasm32 but never on Unix, where those declarations are the crate. Without
//! law 2 the cheap way to green law 1 on a future `aarch64-unknown-linux-musl` is a bare row,
//! which ships a libc with no libc in it.
//!
//! `cargo xtask gate cells` really cross-compiles and stays the authority; this guard is pure
//! `std`, reads only committed sources under `CARGO_MANIFEST_DIR`, and proves only that the
//! escape list does not refuse a shipped triple outright.
//!
//! The cfg grammar read here is `all` / `any` / `not`, bare `unix`/`windows`, and
//! `target_arch`/`target_os`/`target_env`/`target_family`/`target_vendor` equalities; a
//! predicate outside it, or a triple token [`TargetCfg::derive`] does not know, is an error
//! rather than a silent pass. Only `crates/aterm-libc` is judged — the bottom of the
//! first-party graph, so a triple it refuses is one nothing above it can be built for.
//! `the_evaluator_agrees_with_the_compiler_that_built_it` is the non-vacuity anchor: this box
//! compiled, so the escape list admits its triple, and the evaluator must say so too.
//! agree — a parser that quietly matched nothing fails there.

use std::path::{Path, PathBuf};

// The source under test

/// The `compile_error!` string `aterm-libc` answers an unsupported target with. Quoted so the
/// attribute above it can be found by what it guards rather than by line number.
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

// Lexing: `#[cfg(...)]` attributes and the item each one gates

/// One column-0 `#[cfg(…)]` attribute and the item line it is attached to.
///
/// Column 0 only: everything this file judges — the cell `mod`s, the `compile_error!` — is a
/// top-level item of `crates/aterm-libc/src/lib.rs`, and an indented `cfg` in that file would
/// be inside a generated cell module, which is not this guard's business.
#[derive(Debug, Clone)]
struct Gated {
    /// The predicate inside `cfg(…)`, with interior newlines collapsed to spaces.
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

// The cfg algebra

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
    /// Derive a triple's cfg values from `<arch>-<vendor>-<os>[-<env>]`, over the os/vendor
    /// token table below. Derived rather than tabulated on purpose: a table of six triples
    /// would have to be edited in lockstep with [`atpkg::TARGETS`], and a guard whose own
    /// roster can go stale beside the roster it guards is the defect it prevents. An unknown
    /// token is an error, so nothing is guessed either.
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
            // `wasm32-unknown-unknown`: `target_os = "unknown"` and no `target_family`.
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

/// Split a predicate list on its top-level commas: `all(a, b), c` is two arguments, not three.
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
/// `Err` for anything outside the grammar in this file's header — fail-closed on purpose: a
/// row the evaluator cannot read must stop the test rather than be waved through as covered.
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

/// The cfg values of the target this test binary was compiled for, read from the compiler
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

// Law 1 — a shipped triple is never refused outright

/// The defect this was written for: a triple three shipping lanes served, answered by
/// `crates/aterm-libc` with `compile_error!` — so nothing first-party could compile for it.
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
         `cli::current_triple` and the vendor-direct asset rules would all be serving a target \
         that does not compile. Either add the target to the escape list (with a generated \
         cell if it is a Unix target — see law 2) or take it out of TARGETS. One story, not \
         two.\n\nescape predicate: {escape}"
    );
}

// Law 2 — a shipped Unix triple has a real cell, not just an escape row

/// A bare escape row says "this target needs no POSIX ABI declarations at all". True on
/// Windows and wasm32, where every `libc::` consumer is `cfg`-gated away and the
/// unconditional `core::ffi` re-exports are the whole crate; never true on Unix.
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

// Non-vacuity

/// The strongest check available without a second machine: this test binary exists, so the
/// compiler that built it did not hit the `compile_error!`, and the evaluator must agree on
/// this very box. A parser that matched nothing, or one that answers `true` to everything,
/// dies here.
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

/// The red proof, against the shape of the real defect rather than a mutated constant: the
/// escape list as it stood before the fix, with the x86_64 Windows row and no aarch64 twin.
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

/// An unreadable row is an error, not a pass. A guard about completeness that shrugs at the
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
    // …while the grammar it does know answers.
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
    // The committed spellings all derive, and each one derives differently.
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
