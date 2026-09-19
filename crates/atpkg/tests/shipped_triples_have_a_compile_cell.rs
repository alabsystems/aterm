// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SHIPPED TRIPLE => COMPILED TRIPLE: every target this client publishes rows for must be a
//! cell some compiler is made to read.
//!
//! THE CLASS THIS GUARDS. [`atpkg::TARGETS`] is the schema's roster of served triples, and
//! `aterm_forge::resolve::default_cells()` is the matrix `xtask gate cells` and
//! `xtask gate cells-foreign` type-check — the only lanes in this repository that compile
//! aterm for a triple the box in front of you is not. Nothing made the two lists agree, and on
//! 2026-09-18 they did not:
//!
//!   * `aarch64-unknown-linux-gnu`, `aarch64-pc-windows-msvc` and `x86_64-apple-darwin` were
//!     THREE OF THE SIX shipped triples, each with a `[[artifact]]` row the index can name and
//!     a `cli::current_triple` arm that reports it, and NONE of them was a cell. So a break
//!     visible only on one of those three reached no gate on any box: `gate cells` was green
//!     over a matrix that never mentioned them, and `gate all` — which runs
//!     `cells-foreign` — was green with them. That is the same shape as the four Windows
//!     breaks of 2026-09-16 (see `shipped_triples_have_an_abi_cell.rs`), one level up: there
//!     the triple could not compile at all, here nothing ever tried.
//!
//! WHY HERE, beside the ABI-cell law. That file guards "a shipped triple the first-party graph
//! REFUSES"; this one guards "a shipped triple no compiler ever READS". They are the two ends
//! of the same seam — `atpkg::TARGETS` on one side, the build matrix on the other — and the
//! second is worthless without the first: a triple admitted by `aterm-libc` and compiled by
//! nobody is exactly the state three of the six were in.
//!
//! WHY NOT IN `gate cells` ITSELF. The gate's own matrix audit (`cell_matrix_audit` in
//! crates/xtask/src/gate.rs) already refuses a cell with no coverage floor and a floor with no
//! cell — but it cannot ask this question, because `xtask` does not depend on `atpkg` and the
//! roster of SHIPPED triples is atpkg's to own. Asking it from here costs nothing: pure `std`,
//! no subprocess, no network, no new dependency, reading one committed source file under the
//! workspace root, on whatever box the change is being written on.
//!
//! THE LAW. For every triple in [`atpkg::TARGETS`], `aterm_forge::resolve::default_cells()`
//! must carry a cell whose `triple` is that spelling. ONE-DIRECTIONAL, like its neighbour: a
//! cell for a triple nothing ships (`wasm32-unknown-unknown`, the two browser modules) is not
//! a violation — aterm ships those as `.wasm` inside the app, not as an atpkg artifact row.
//! The direction with teeth is the one that was violated.
//!
//! SCOPE AND LIMITS, said out loud so a green run is not read as more than it is:
//!
//!   * This is a STRING law over committed text, not a compile. It proves a cell EXISTS for
//!     every shipped triple; `xtask gate cells` is the authority on whether that cell's graph
//!     type-checks, and stays it. What this file makes impossible is the failure that was
//!     live — a shipped triple with no cell to type-check at all.
//!   * A `default_cells()` body this file cannot parse is an ERROR, never a silent pass: the
//!     question asked here is "is this matrix complete", so a matrix it cannot read is one it
//!     must not vouch for.
//!   * Only the cell LIST is read. Which package each cell is rooted at, and what its floor
//!     is, are `tools/cross-cell-gate.tsv`'s business and the gate's.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// THE SOURCE UNDER TEST
// ---------------------------------------------------------------------------

/// The function whose body IS the matrix. Quoted once, so a rename is a loud failure here
/// rather than a guard that quietly stops finding its subject.
const MATRIX_FN: &str = "pub fn default_cells() -> Vec<Cell> {";

fn repo_root() -> PathBuf {
    // …/crates/atpkg -> …/crates -> …
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/atpkg must sit two levels under the workspace root")
        .to_path_buf()
}

fn resolve_source() -> String {
    let path = repo_root().join("crates/aterm-forge/src/resolve.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {} ({e})", path.display()))
}

// ---------------------------------------------------------------------------
// READING THE MATRIX
// ---------------------------------------------------------------------------

/// One cell as the committed source spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cell {
    name: String,
    triple: String,
}

/// The body of [`MATRIX_FN`]: from the opening brace to the brace that closes it, counted
/// rather than guessed, so a `Cell { … }` literal inside cannot end it early.
fn matrix_body(src: &str) -> Result<&str, String> {
    let start = src.find(MATRIX_FN).ok_or_else(|| {
        format!("`{MATRIX_FN}` is no longer in crates/aterm-forge/src/resolve.rs")
    })? + MATRIX_FN.len();
    let mut depth = 1usize;
    for (i, ch) in src[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&src[start..start + i]);
                }
            }
            _ => {}
        }
    }
    Err("`default_cells()`'s body never closes — the file cannot be parsed".to_string())
}

/// The string literal that follows `key:` at or after `from`, with the offset just past it.
fn field_after(body: &str, from: usize, key: &str) -> Option<(String, usize)> {
    let at = body[from..].find(key)? + from + key.len();
    let open = body[at..].find('"')? + at + 1;
    let close = body[open..].find('"')? + open;
    Some((body[open..close].to_string(), close))
}

/// Every `Cell { name: "…", triple: "…", … }` literal in the matrix, in source order.
///
/// Deliberately shallow: the matrix is a `vec![]` of struct literals written out by hand,
/// which is the shape this reads. A cell built any other way would not be found — so the
/// count is pinned against the committed file by `the_scan_still_sees_the_matrix`, and a
/// literal that names no `triple` is an error rather than a skip.
fn cells(src: &str) -> Result<Vec<Cell>, String> {
    let body = matrix_body(src)?;
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(off) = body[at..].find("Cell {") {
        let start = at + off + "Cell {".len();
        let (name, after_name) = field_after(body, start, "name:")
            .ok_or_else(|| format!("a `Cell {{` literal at byte {start} names no `name:`"))?;
        let (triple, after_triple) = field_after(body, after_name, "triple:")
            .ok_or_else(|| format!("cell `{name}` names no `triple:`"))?;
        out.push(Cell { name, triple });
        at = after_triple;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// THE LAW
// ---------------------------------------------------------------------------

/// THE DEFECT THIS WAS WRITTEN FOR: three of the six shipped triples were in no cell, so no
/// gate on any box ever compiled aterm for them.
#[test]
fn every_shipped_triple_is_a_cell_some_compiler_reads() {
    let src = resolve_source();
    let cells = cells(&src).unwrap_or_else(|e| panic!("{e}"));
    let missing: Vec<&str> = atpkg::TARGETS
        .iter()
        .copied()
        .filter(|t| !cells.iter().any(|c| c.triple == *t))
        .collect();
    assert!(
        missing.is_empty(),
        "{missing:?} are shipped triples with NO cell in \
         `aterm_forge::resolve::default_cells()`, which is the matrix `xtask gate cells` and \
         `xtask gate cells-foreign` type-check. atpkg publishes artifact rows for them and \
         `cli::current_triple` reports them, so a break visible only there reaches no gate on \
         any box — the state `aarch64-unknown-linux-gnu`, `aarch64-pc-windows-msvc` and \
         `x86_64-apple-darwin` were in until 2026-09-18. Add the cell (with its `floor` row in \
         tools/cross-cell-gate.tsv and its baseline in aterm_forge::measured), or take the \
         triple out of TARGETS. One story, not two.\n\nthe matrix reads: {cells:?}"
    );
}

// ---------------------------------------------------------------------------
// NON-VACUITY
// ---------------------------------------------------------------------------

/// The scanner's own obligation, pinned against the committed file rather than a fixture: a
/// reader that quietly matched nothing would pass the law above forever.
#[test]
fn the_scan_still_sees_the_matrix() {
    let src = resolve_source();
    let body = matrix_body(&src).unwrap_or_else(|e| panic!("{e}"));
    let literals = body.matches("Cell {").count();
    let cells = cells(&src).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        cells.len(),
        literals,
        "the matrix body holds {literals} `Cell {{` literal(s) and the scan read {}: a reader \
         that stops part way through would vouch for a list it never saw",
        cells.len()
    );
    assert!(
        cells.len() >= 8,
        "the matrix carries the six shipped triples plus the two browser cells; the scan saw \
         {} cell(s): {cells:?}",
        cells.len()
    );
    for want in ["mac-arm", "linux", "win", "wasm-cpu", "wasm-gpu"] {
        assert!(
            cells.iter().any(|c| c.name == want),
            "cell `{want}` must still be seen; saw {cells:?}"
        );
    }
}

/// THE RED PROOF, against the real shape rather than a mutated constant: the matrix exactly as
/// it stood before 2026-09-18 — five cells, three shipped triples unmentioned — and the law
/// must name those three and no others.
#[test]
fn the_law_names_the_three_triples_the_matrix_forgot() {
    let before = "\
pub fn default_cells() -> Vec<Cell> {
    vec![
        Cell {
            name: \"mac-arm\".to_string(),
            triple: \"aarch64-apple-darwin\".to_string(),
            package: \"aterm\".to_string(),
        },
        Cell {
            name: \"linux\".to_string(),
            triple: \"x86_64-unknown-linux-gnu\".to_string(),
            package: \"aterm\".to_string(),
        },
        Cell {
            name: \"win\".to_string(),
            triple: \"x86_64-pc-windows-msvc\".to_string(),
            package: \"aterm\".to_string(),
        },
        Cell {
            name: \"wasm-cpu\".to_string(),
            triple: \"wasm32-unknown-unknown\".to_string(),
            package: \"aterm-wasm\".to_string(),
        },
        Cell {
            name: \"wasm-gpu\".to_string(),
            triple: \"wasm32-unknown-unknown\".to_string(),
            package: \"aterm-gpu-web\".to_string(),
        },
    ]
}
";
    let cells = cells(before).expect("the pre-fix body parses");
    assert_eq!(
        cells.len(),
        5,
        "the pre-fix matrix had five cells: {cells:?}"
    );
    let missing: Vec<&str> = atpkg::TARGETS
        .iter()
        .copied()
        .filter(|t| !cells.iter().any(|c| c.triple == *t))
        .collect();
    assert_eq!(
        missing,
        vec![
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "aarch64-pc-windows-msvc",
        ],
        "the pre-fix matrix left exactly these shipped triples uncompiled, and the law must \
         name them"
    );
}

/// A body this file cannot read is an ERROR, not a pass. A completeness law that shrugs at a
/// matrix it never parsed reports green on nothing at all.
#[test]
fn an_unreadable_matrix_stops_the_test() {
    let err = cells("fn something_else() {}").expect_err("no matrix function");
    assert!(err.contains("default_cells"), "{err}");
    let err = cells("pub fn default_cells() -> Vec<Cell> {\n    vec![Cell {\n")
        .expect_err("the body never closes");
    assert!(err.contains("never closes"), "{err}");
    let err = cells("pub fn default_cells() -> Vec<Cell> {\n    vec![Cell { name: \"x\" }]\n}\n")
        .expect_err("a cell with no triple");
    assert!(err.contains("names no `triple:`"), "{err}");
}
