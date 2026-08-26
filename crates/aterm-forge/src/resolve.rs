// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cell matrix, and the one sanctioned way to obtain a resolved graph.
//!
//! # Why `cargo tree` and not `cargo metadata`
//!
//! MEASURED on this checkout: `cargo metadata --filter-platform
//! aarch64-apple-darwin` reports 271 packages for the `aterm` root against
//! `cargo tree -p aterm`'s 212 — a 28% over-count, because `cargo metadata`
//! resolves the WHOLE workspace (67 members) with features unified across all
//! of them and then filters only by target, never by reachability from one
//! root. A survey built on it would bill `aterm` for packages that only
//! `aterm-bench` or `xtask` pull in. `cargo tree` walks the actual resolve from
//! a single `-p` root, which is the question this program asks.
//!
//! `--no-dedupe` is mandatory: without it cargo elides repeated subtrees behind
//! a ` (*)` marker, and the elided edges are exactly the shared dependencies
//! that make a dominator differ from a subtree. The parser still recognises
//! ` (*)` so that a hand-run command pasted into a fixture does not explode.
//!
//! Resolution needs no toolchain and no network: every cell is measured
//! offline, including the two triples this host cannot build for.

use crate::model::{Cell, Graph, PkgId};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The four measurement cells, in report order. All rooted at the shipped
/// binary package `aterm`, because the question is "what does the terminal
/// ship", not "what does the workspace resolve".
pub fn default_cells() -> Vec<Cell> {
    vec![
        Cell {
            name: "mac-arm".to_string(),
            triple: "aarch64-apple-darwin".to_string(),
            package: "aterm".to_string(),
        },
        Cell {
            name: "linux".to_string(),
            triple: "x86_64-unknown-linux-gnu".to_string(),
            package: "aterm".to_string(),
        },
        Cell {
            name: "win".to_string(),
            triple: "x86_64-pc-windows-msvc".to_string(),
            package: "aterm".to_string(),
        },
        Cell {
            name: "wasm".to_string(),
            triple: "wasm32-unknown-unknown".to_string(),
            package: "aterm".to_string(),
        },
    ]
}

/// Map `--cell NAME` arguments onto cells. No names at all means every cell —
/// the default a gate wants, because a surface that only shrinks on one target
/// has not shrunk. Requested order is preserved and repeats collapse, so
/// `--cell linux --cell linux` measures Linux once.
pub fn select(cells: &[Cell], names: &[String]) -> Result<Vec<Cell>, String> {
    if names.is_empty() {
        return Ok(cells.to_vec());
    }
    let known = || {
        cells
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out: Vec<Cell> = Vec::new();
    for want in names {
        let Some(cell) = cells.iter().find(|c| c.name == *want) else {
            return Err(format!(
                "unknown cell `{want}` — the cells are {}. Type `--cell {}` (repeatable), \
                 or drop `--cell` entirely to measure all {}.",
                known(),
                cells.first().map_or("mac-arm", |c| c.name.as_str()),
                cells.len()
            ));
        };
        if !out.iter().any(|c| c.name == cell.name) {
            out.push(cell.clone());
        }
    }
    Ok(out)
}

/// One parsed `cargo tree --prefix depth` line. Exposed because the parser is
/// the part most likely to drift when cargo changes its output, and a drift
/// that silently drops edges would silently shrink every dominator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeLine {
    pub depth: usize,
    pub id: PkgId,
    /// The directory cargo printed in parentheses — present for workspace
    /// members and for `[patch.crates-io]` path packages, absent for registry
    /// packages.
    pub path: Option<PathBuf>,
    /// cargo's ` (proc-macro)` annotation. Kept separate from the manifest's
    /// `[lib] proc-macro = true` so the two can be cross-checked.
    pub is_proc_macro: bool,
    /// cargo's ` (*)` "subtree shown above" marker. Never emitted under
    /// `--no-dedupe`; parsed anyway so pasted output is not a landmine.
    pub deduped: bool,
}

/// Parse one line. `Ok(None)` is a blank line. An unparseable line is an error
/// rather than a skip: silently ignoring a line loses a package.
pub fn parse_line(line: &str) -> Result<Option<TreeLine>, String> {
    let text = line.trim_end();
    if text.trim().is_empty() {
        return Ok(None);
    }
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return Err(format!(
            "cargo tree line `{text}` has no leading depth number — forge parses \
             `--prefix depth` output; re-run with `--prefix depth` (the tool passes it \
             itself, so seeing this means cargo's output format changed)."
        ));
    }
    let depth: usize = text[..digits]
        .parse()
        .map_err(|_| format!("cargo tree line `{text}` has a depth prefix that is not a usize"))?;

    let rest = text[digits..].trim_start();
    let mut parts = rest.splitn(3, ' ');
    let name = parts.next().unwrap_or_default();
    let Some(version_token) = parts.next() else {
        return Err(format!(
            "cargo tree line `{text}` has no ` v<version>` field — expected \
             `<depth><name> v<version>`"
        ));
    };
    let Some(version) = version_token.strip_prefix('v') else {
        return Err(format!(
            "cargo tree line `{text}` has `{version_token}` where `v<version>` was expected"
        ));
    };
    if name.is_empty() || version.is_empty() {
        return Err(format!(
            "cargo tree line `{text}` names an empty package or version"
        ));
    }

    let mut tail = parts.next().unwrap_or("").trim();
    let mut out = TreeLine {
        depth,
        id: PkgId::new(name, version),
        path: None,
        is_proc_macro: false,
        deduped: false,
    };
    while !tail.is_empty() {
        let (inner, remainder) = parenthesised(tail, text)?;
        match inner {
            "*" => out.deduped = true,
            "proc-macro" => out.is_proc_macro = true,
            other => out.path = Some(PathBuf::from(other)),
        }
        tail = remainder.trim_start();
    }
    Ok(Some(out))
}

/// Split the leading `(...)` group off `tail`. The closing paren is the first
/// one that ends the line or is followed by ` (`, so a directory whose name
/// contains `)` survives — the annotations cargo appends (` (*)`,
/// ` (proc-macro)`) never do.
fn parenthesised<'a>(tail: &'a str, whole: &str) -> Result<(&'a str, &'a str), String> {
    let bytes = tail.as_bytes();
    if bytes.first() != Some(&b'(') {
        return Err(format!(
            "cargo tree line `{whole}` has trailing text `{tail}` that is not a \
             `(...)` annotation"
        ));
    }
    for (i, b) in bytes.iter().enumerate().skip(1) {
        if *b == b')' && (i + 1 == bytes.len() || tail[i + 1..].starts_with(" (")) {
            return Ok((&tail[1..i], &tail[i + 1..]));
        }
    }
    Err(format!(
        "cargo tree line `{whole}` has an unclosed `(` in `{tail}`"
    ))
}

/// Rebuild the graph from depth-prefixed output. A node at depth `d` attaches
/// to the most recent node at depth `d - 1`; that is the whole trick, and it is
/// why `--prefix depth` is used instead of the box-drawing default (whose
/// indentation is ambiguous once a package name contains the same glyphs).
///
/// Returns the graph plus the directories cargo printed, which are the only
/// authority on where a path package actually lives.
pub fn parse_tree(text: &str) -> Result<(Graph, BTreeMap<PkgId, PathBuf>), String> {
    let mut graph = Graph::default();
    let mut paths: BTreeMap<PkgId, PathBuf> = BTreeMap::new();
    let mut stack: Vec<PkgId> = Vec::new();
    let mut have_root = false;

    for raw in text.lines() {
        let Some(line) = parse_line(raw)? else {
            continue;
        };
        if line.depth > stack.len() {
            return Err(format!(
                "cargo tree depth jumped to {} with only {} ancestors open, at `{}` — \
                 the output is not a well-formed `--prefix depth` tree",
                line.depth,
                stack.len(),
                raw.trim()
            ));
        }
        if let Some(dir) = line.path.clone() {
            paths.entry(line.id.clone()).or_insert(dir);
        }
        graph.nodes.insert(line.id.clone());
        if line.depth == 0 {
            if have_root && graph.root != line.id {
                return Err(format!(
                    "cargo tree printed a second root `{}` after `{}` — pass exactly one \
                     `-p <package>` so the survey has one root to measure",
                    line.id, graph.root
                ));
            }
            graph.root = line.id.clone();
            have_root = true;
        } else {
            let parent = stack[line.depth - 1].clone();
            graph
                .edges
                .entry(parent)
                .or_default()
                .insert(line.id.clone());
        }
        stack.truncate(line.depth);
        stack.push(line.id);
    }

    if !have_root {
        return Err(
            "cargo tree printed no depth-0 line — the requested root package resolved to \
             nothing; check the `-p` name against `cargo metadata --no-deps`."
                .to_string(),
        );
    }
    Ok((graph, paths))
}

/// Resolve one cell: the graph plus every package directory cargo disclosed.
///
/// `--locked --offline` first, because a survey that quietly re-resolves is
/// measuring a graph the repository does not ship. If that fails, ONE retry
/// without `--offline` is allowed and is written into `log` — a note nobody can
/// miss, since the number it produced may not be reproducible offline.
pub fn graph_and_paths(
    root: &Path,
    cell: &Cell,
    log: &mut String,
) -> Result<(Graph, BTreeMap<PkgId, PathBuf>), String> {
    let root = &abs_root(root)?;
    let text = match run_tree(root, cell, true) {
        Ok(text) => text,
        Err(offline_err) => {
            let online = run_tree(root, cell, false).map_err(|online_err| {
                format!(
                    "`cargo tree` could not resolve cell `{}` ({}).\n  \
                     with --offline: {}\n  without --offline: {}\n  \
                     fix: run `cargo fetch` in {} (or `cargo metadata --offline >/dev/null` \
                     if Cargo.lock is merely stale), then re-run `cargo forge`.",
                    cell.name,
                    cell.triple,
                    first_line(&offline_err),
                    first_line(&online_err),
                    root.display()
                )
            })?;
            log.push_str(&format!(
                "    NOTE: cell `{}` ({}) did not resolve with `--offline`; forge retried \
                 once WITHOUT it and succeeded. The registry cache is incomplete — run \
                 `cargo fetch` to make this measurement reproducible offline.\n      \
                 offline error: {}\n",
                cell.name,
                cell.triple,
                first_line(&offline_err)
            ));
            online
        }
    };
    parse_tree(&text)
}

/// The contract signature: just the graph. Any retry note goes to stderr,
/// because a `Result<Graph, _>` has nowhere else to put it — call
/// [`graph_and_paths`] when the note must be captured into a report.
pub fn graph(root: &Path, cell: &Cell) -> Result<Graph, String> {
    let mut log = String::new();
    let out = graph_and_paths(root, cell, &mut log);
    if !log.is_empty() {
        eprint!("{log}");
    }
    Ok(out?.0)
}

/// Absolutise the workspace root ONCE, before it is used twice.
///
/// THE MEASURED DEFECT THIS EXISTS TO PREVENT: [`run_tree`] both sets the child
/// `current_dir` to the root AND passes `--manifest-path <root>/Cargo.toml`.
/// With a RELATIVE root those two COMPOSE — cargo resolves the manifest against
/// the cwd it was just given — so `--root ..` from `crates/` sent cargo looking
/// for `/Users//…/Cargo.toml` one level above the workspace and every cell died
/// with "manifest path `../Cargo.toml` does not exist". Worse than the failure
/// was the diagnosis: the retry path reported it as an incomplete registry
/// cache and told the operator to run `cargo fetch`, which cannot help.
///
/// Canonicalising HERE fixes it for every caller at once, rather than once per
/// verb — `check` had already grown its own private workaround.
fn abs_root(root: &Path) -> Result<PathBuf, String> {
    root.canonicalize().map_err(|e| {
        format!(
            "workspace root `{}` cannot be resolved: {e}. FIX: pass `--root` an existing              directory holding the workspace `Cargo.toml`, or omit `--root` and run from              anywhere inside the workspace.",
            root.display()
        )
    })
}

fn run_tree(root: &Path, cell: &Cell, offline: bool) -> Result<String, String> {
    // `CARGO` is set by cargo itself, so a nested invocation uses the very
    // toolchain that launched forge rather than whatever `cargo` is on PATH.
    let exe = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(exe);
    cmd.arg("tree")
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .arg("-p")
        .arg(&cell.package)
        .arg("--edges")
        .arg("normal")
        .arg("--target")
        .arg(&cell.triple)
        .arg("--prefix")
        .arg("depth")
        .arg("--no-dedupe")
        .arg("--locked")
        .current_dir(root);
    if offline {
        cmd.arg("--offline");
    }
    let out = cmd.output().map_err(|e| {
        format!(
            "could not execute `cargo tree`: {e} — install a cargo on PATH, or set CARGO \
             to the binary to use"
        )
    })?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    String::from_utf8(out.stdout).map_err(|e| {
        format!(
            "`cargo tree` emitted non-UTF-8 output for `{}`: {e}",
            cell.name
        )
    })
}

fn first_line(s: &str) -> &str {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or(s).trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measured;

    pub(crate) fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-forge sits two levels under the workspace root")
            .to_path_buf()
    }

    fn names(cells: &[Cell]) -> Vec<&str> {
        cells.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn the_matrix_is_four_cells_in_report_order() {
        let cells = default_cells();
        assert_eq!(names(&cells), ["mac-arm", "linux", "win", "wasm"]);
        assert!(
            cells.iter().all(|c| c.package == "aterm"),
            "every cell measures the binary"
        );
        assert_eq!(cells[0].triple, "aarch64-apple-darwin");
        assert_eq!(cells[3].triple, "wasm32-unknown-unknown");
    }

    #[test]
    fn no_cell_flag_means_every_cell() {
        let cells = default_cells();
        assert_eq!(select(&cells, &[]).unwrap(), cells);
    }

    #[test]
    fn selection_keeps_request_order_and_collapses_repeats() {
        let cells = default_cells();
        let want = ["wasm", "linux", "wasm"].map(String::from);
        assert_eq!(names(&select(&cells, &want).unwrap()), ["wasm", "linux"]);
    }

    #[test]
    fn an_unknown_cell_names_every_valid_cell() {
        let err = select(&default_cells(), &["macos".to_string()]).unwrap_err();
        for good in ["mac-arm", "linux", "win", "wasm"] {
            assert!(err.contains(good), "refusal must list `{good}`: {err}");
        }
        assert!(err.contains("--cell"), "refusal must name the fix: {err}");
    }

    #[test]
    fn a_plain_registry_line_parses() {
        let line = parse_line("8unicode-ident v1.0.24").unwrap().unwrap();
        assert_eq!(line.depth, 8);
        assert_eq!(line.id, PkgId::new("unicode-ident", "1.0.24"));
        assert_eq!(line.path, None);
        assert!(!line.is_proc_macro && !line.deduped);
    }

    #[test]
    fn a_workspace_line_carries_its_path() {
        let line = parse_line("0aterm v0.47.0 (/Users//example/aterm/crates/aterm)")
            .unwrap()
            .unwrap();
        assert_eq!(line.depth, 0);
        assert_eq!(line.id, PkgId::new("aterm", "0.47.0"));
        assert_eq!(
            line.path.as_deref(),
            Some(Path::new("/Users//example/aterm/crates/aterm"))
        );
    }

    #[test]
    fn proc_macro_annotation_precedes_the_path() {
        let raw = "6aterm-error-derive v0.47.0 (proc-macro) (/w/crates/aterm-error-derive)";
        let line = parse_line(raw).unwrap().unwrap();
        assert!(line.is_proc_macro);
        assert_eq!(
            line.path.as_deref(),
            Some(Path::new("/w/crates/aterm-error-derive"))
        );
        assert_eq!(line.id, PkgId::new("aterm-error-derive", "0.47.0"));
    }

    #[test]
    fn the_dedupe_marker_is_recognised_alone_and_after_a_path() {
        let bare = parse_line("3serde v1.0.228 (*)").unwrap().unwrap();
        assert!(bare.deduped && bare.path.is_none());
        let pathed = parse_line("4winnow v0.7.15 (/Users//example/aterm/vendor/winnow) (*)")
            .unwrap()
            .unwrap();
        assert!(pathed.deduped);
        assert_eq!(
            pathed.path.as_deref(),
            Some(Path::new("/Users//example/aterm/vendor/winnow"))
        );
    }

    #[test]
    fn a_directory_containing_a_paren_still_parses() {
        let line = parse_line("2foo v1.0.0 (/Users//a b (1)/vendor/foo) (*)")
            .unwrap()
            .unwrap();
        assert_eq!(
            line.path.as_deref(),
            Some(Path::new("/Users//a b (1)/vendor/foo"))
        );
        assert!(line.deduped);
    }

    #[test]
    fn a_build_metadata_version_survives() {
        let line = parse_line("4zstd-sys v2.0.16+zstd.1.5.7").unwrap().unwrap();
        assert_eq!(line.id.version, "2.0.16+zstd.1.5.7");
        assert_eq!(line.id.spec(), "zstd-sys@2.0.16+zstd.1.5.7");
    }

    #[test]
    fn blank_lines_are_skipped_not_failed() {
        assert_eq!(parse_line("").unwrap(), None);
        assert_eq!(parse_line("   ").unwrap(), None);
    }

    #[test]
    fn a_line_without_a_depth_prefix_refuses_and_names_the_flag() {
        let err = parse_line("|-- serde v1.0.228").unwrap_err();
        assert!(err.contains("--prefix depth"), "{err}");
    }

    #[test]
    fn a_line_without_a_version_refuses() {
        assert!(parse_line("2serde").is_err());
        assert!(parse_line("2serde 1.0.228").is_err());
    }

    const FIXTURE: &str = "\
0root v0.1.0 (/w/crates/root)
1alpha v1.0.0
2shared v9.9.9
1beta v2.0.0 (proc-macro)
2shared v9.9.9 (*)

2gamma v3.0.0 (/w/vendor/gamma)
";

    #[test]
    fn the_depth_stack_reconstructs_parents() {
        let (g, paths) = parse_tree(FIXTURE).unwrap();
        assert_eq!(g.root, PkgId::new("root", "0.1.0"));
        assert_eq!(g.nodes.len(), 5, "root, alpha, beta, shared, gamma");
        let kids = |n: &str, v: &str| {
            g.edges
                .get(&PkgId::new(n, v))
                .map(|s| s.iter().map(ToString::to_string).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        assert_eq!(kids("root", "0.1.0"), ["alpha 1.0.0", "beta 2.0.0"]);
        assert_eq!(kids("alpha", "1.0.0"), ["shared 9.9.9"]);
        // The `(*)` child under beta is still a real edge: dedupe is a display
        // trick, and dropping it would make `shared` look uniquely alpha's.
        assert_eq!(kids("beta", "2.0.0"), ["gamma 3.0.0", "shared 9.9.9"]);
        assert_eq!(
            paths[&PkgId::new("gamma", "3.0.0")],
            PathBuf::from("/w/vendor/gamma")
        );
        assert!(!paths.contains_key(&PkgId::new("alpha", "1.0.0")));
    }

    #[test]
    fn a_depth_jump_refuses() {
        let err = parse_tree("0root v0.1.0\n2orphan v1.0.0\n").unwrap_err();
        assert!(err.contains("depth jumped"), "{err}");
    }

    #[test]
    fn output_with_no_root_refuses() {
        assert!(parse_tree("1alpha v1.0.0\n").is_err());
    }

    #[test]
    fn mac_arm_resolves_to_the_baseline_node_graph() {
        let root = repo_root();
        let cells = default_cells();
        let want = measured::MAC_ARM;
        let g = graph(&root, &cells[0]).expect("mac-arm must resolve offline");
        assert_eq!(g.root.name, "aterm", "the cell roots at the shipped binary");
        assert_eq!(
            g.nodes.len(),
            want.resolved,
            "packages for aterm on mac-arm"
        );
        assert_eq!(
            g.reach(None).len(),
            want.resolved,
            "every node is reachable from the root"
        );
    }

    #[test]
    fn linux_resolves_to_the_baseline_node_graph() {
        let g = graph(&repo_root(), &default_cells()[1]).expect("linux must resolve offline");
        assert_eq!(g.nodes.len(), measured::LINUX.resolved);
    }

    /// REGRESSION: a relative root used to be resolved TWICE — once as the
    /// child's `current_dir` and again inside `--manifest-path <root>/…` — so
    /// `crates/aterm-forge/../..` looked one level ABOVE the workspace and
    /// every cell failed with "manifest path does not exist", misreported as a
    /// stale registry cache. The root is canonicalised once now, so a relative
    /// spelling resolves the identical graph as the absolute one.
    #[test]
    fn a_relative_root_resolves_the_same_graph_as_an_absolute_one() {
        let cell = &default_cells()[3]; // wasm — the smallest cell.
        let absolute = graph(&repo_root(), cell).expect("absolute root must resolve");
        let relative = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        assert!(relative.is_relative() || relative.components().count() > 3);
        let via_relative = graph(&relative, cell).expect("a relative root must resolve too");
        assert_eq!(absolute.nodes, via_relative.nodes, "same tree, same graph");
    }

    /// A root that does not exist is a NAMED refusal, never an empty survey.
    #[test]
    fn a_root_that_does_not_exist_is_refused_by_name() {
        let err = graph(Path::new("/no/such/workspace"), &default_cells()[0]).unwrap_err();
        assert!(err.contains("/no/such/workspace"), "{err}");
        assert!(err.contains("FIX:"), "a refusal must name the fix: {err}");
    }
}
