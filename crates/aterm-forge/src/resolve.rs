// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cell matrix, and the one sanctioned way to obtain a resolved graph.
//!
//! # Why `cargo tree` and not `cargo metadata`
//!
//! `cargo metadata --filter-platform` resolves the whole workspace with
//! features unified across members and filters by target, not reachability, so
//! it would bill `aterm` for packages only other members pull in. `cargo tree
//! -p aterm` walks the actual resolve from one root.
//!
//! `--no-dedupe` is mandatory: cargo otherwise elides repeated subtrees behind
//! ` (*)`, and those shared edges are what make a dominator differ from a
//! subtree. The parser still accepts ` (*)` for pasted fixtures.
//!
//! Resolution needs no toolchain and no network: every cell is measured
//! offline, including triples this host cannot build for.

use crate::model::{Cell, Graph, PkgId};
use aterm_uds::output::CommandOutput as _;
use aterm_uds::spawnfd::{Command, command};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The cargo forge asks for `tree` and `metadata`, and the PATH its children
/// see: one value per run, so every child answers from one toolchain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cargo {
    program: OsString,
    path: Option<OsString>,
}

impl Cargo {
    /// `$CARGO` (targo sets it, so a nested run asks the toolchain that
    /// launched it), else Trust's `targo` off PATH: forge's own CLI.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            program: std::env::var_os("CARGO").unwrap_or_else(|| "targo".into()),
            path: None,
        }
    }

    /// `program` with `path` as its children's PATH: a caller that resolved
    /// its toolchain once hands it here, so `$CARGO` and PATH order never pick
    /// another.
    #[must_use]
    pub fn pinned(program: impl Into<OsString>, path: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            path: Some(path.into()),
        }
    }

    /// `<program> <verb>`, from `root`, under this cargo's PATH.
    pub(crate) fn command(&self, verb: &str, root: &Path) -> Command {
        let mut cmd = command(&self.program);
        cmd.arg(verb).current_dir(root);
        if let Some(path) = &self.path {
            cmd.env("PATH", path);
        }
        cmd
    }
}

/// The eight measurement cells, in report order.
///
/// # Every shipped triple is a cell
///
/// `atpkg::manifest::TARGETS` lists the triples this project publishes
/// artifacts for; each has a cell, including the second architecture of each
/// OS, where a cfg break otherwise hides. atpkg's
/// `every_published_triple_is_a_forge_cell` holds the shipped triples to a cell each.
///
/// # Every cell is rooted at something aterm ships
///
/// The six native cells are rooted at the binary package `aterm`. `aterm` is
/// never compiled for `wasm32`, so the wasm cells are rooted at the two shipped
/// browser modules, each compiled by name in `verify cells` and the `wasm`
/// lane of `verify perf`:
///
/// * `aterm-wasm`: the CPU/`putImageData` path (`aterm-render` rasterizer);
/// * `aterm-gpu-web`: the GPU path (`aterm-gpu` over `wgpu` -> WebGL2/WebGPU).
///
/// They are different downloads with very different surfaces (`wgpu` and its
/// subtree), so each has its own row and ratchet.
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
        // The CPU renderer module: `aterm-render`'s rasterizer to an RGBA
        // buffer, blitted with `putImageData`. The smallest cell in the matrix.
        Cell {
            name: "wasm-cpu".to_string(),
            triple: "wasm32-unknown-unknown".to_string(),
            package: "aterm-wasm".to_string(),
        },
        // The GPU renderer module: the engine plus `aterm-gpu` over `wgpu`.
        // Each browser module can also have dependencies its sibling omits.
        Cell {
            name: "wasm-gpu".to_string(),
            triple: "wasm32-unknown-unknown".to_string(),
            package: "aterm-gpu-web".to_string(),
        },
        // The second architecture of each shipped OS, rooted at `aterm`.
        // linux-arm and win-arm are no fleet box's own triple, so they run in
        // `verify cells`; mac-x64 is a fleet host triple.
        //
        // The Intel-Mac slice of the universal release binary
        // (`aterm-release`'s buildplan.rs builds it under `RUSTUP_TOOLCHAIN=
        // stable`); its artifacts ship inside the mac-arm `.dmg`.
        Cell {
            name: "mac-x64".to_string(),
            triple: "x86_64-apple-darwin".to_string(),
            package: "aterm".to_string(),
        },
        // ARM Linux: the triple the Linux lane (`atpkg-publish lane run linux`)
        // cross-packs beside x86_64, and the one an aarch64 cell would serve.
        Cell {
            name: "linux-arm".to_string(),
            triple: "aarch64-unknown-linux-gnu".to_string(),
            package: "aterm".to_string(),
        },
        // ARM Windows: the triple apps/aterm-win/build.ps1 selects on an ARM64
        // host.
        Cell {
            name: "win-arm".to_string(),
            triple: "aarch64-pc-windows-msvc".to_string(),
            package: "aterm".to_string(),
        },
    ]
}

/// Census roots need their own wasm measurement cells: their feature-selected
/// graphs can differ even when they share the terminal engine. A native cell
/// with the same package does not measure its wasm dependencies.
pub(crate) fn unmeasured_wasm_roots<'a>(cells: &[Cell], roots: &[&'a str]) -> Vec<&'a str> {
    roots
        .iter()
        .copied()
        .filter(|root| {
            let package = root.rsplit('/').next().unwrap_or(root);
            !cells
                .iter()
                .any(|cell| cell.triple == "wasm32-unknown-unknown" && cell.package == package)
        })
        .collect()
}

/// Map `--cell NAME` arguments onto cells. No names means every cell. Requested
/// order is preserved and repeats collapse.
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
/// the part most likely to drift with cargo's output, and dropped edges would
/// silently shrink every dominator.
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

/// Rebuild the graph from depth-prefixed output: a node at depth `d` attaches
/// to the most recent node at depth `d - 1`. `--prefix depth` avoids the
/// ambiguous box-drawing indentation.
///
/// Returns the graph plus the directories cargo printed, the authority on
/// where a path package lives.
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
             nothing; check the `-p` name against `targo metadata --no-deps`."
                .to_string(),
        );
    }
    Ok((graph, paths))
}

/// Resolve one cell: the graph plus every package directory cargo disclosed.
///
/// Runs `--locked --offline` first. On failure, one retry without `--offline`
/// is allowed and noted in `log`, since that number may not reproduce offline.
pub fn graph_and_paths(
    cargo: &Cargo,
    root: &Path,
    cell: &Cell,
    log: &mut String,
) -> Result<(Graph, BTreeMap<PkgId, PathBuf>), String> {
    let root = &abs_root(root)?;
    let text = match run_tree(cargo, root, cell, true) {
        Ok(text) => text,
        Err(offline_err) => {
            let online = run_tree(cargo, root, cell, false).map_err(|online_err| {
                format!(
                    "`targo tree` could not resolve cell `{}` ({}).\n  \
                     with --offline: {}\n  without --offline: {}\n  \
                     fix: run `targo fetch` in {}, then re-run.",
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
                 `targo fetch` to make this measurement reproducible offline.\n      \
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

/// Absolutise the workspace root once, before it is used twice.
///
/// [`run_tree`] sets the child's `current_dir` to the root and passes
/// `--manifest-path <root>/Cargo.toml`; a relative root would resolve twice
/// and point outside the workspace.
///
/// On Windows the canonical `\\?\C:\…` form is then un-verbatimed: cargo
/// accepts it as `--manifest-path` but then refuses `--locked` against the
/// lock file (`crates/aterm-update/src/which_copy.rs` handles the same
/// property).
fn abs_root(root: &Path) -> Result<PathBuf, String> {
    let abs = root.canonicalize().map_err(|e| {
        format!(
            "workspace root `{}` cannot be resolved: {e}. FIX: pass `--root` an existing \
             directory holding the workspace `Cargo.toml`, or omit `--root` and run from \
             anywhere inside the workspace.",
            root.display()
        )
    })?;
    Ok(strip_verbatim_prefix(abs))
}

/// `\\?\C:\x` -> `C:\x`. Only the verbatim-disk form is rewritten: verbatim UNC
/// has no shorter universally accepted spelling, and other prefixes are left
/// as is. A no-op off Windows.
pub fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};
    let verbatim_disk = matches!(
        path.components().next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::VerbatimDisk(_))
    );
    if !verbatim_disk {
        return path;
    }
    let text = path.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

fn run_tree(cargo: &Cargo, root: &Path, cell: &Cell, offline: bool) -> Result<String, String> {
    // `tree` takes no lane flag.
    let mut cmd = cargo.command("tree", root);
    cmd.arg("--manifest-path")
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
        .arg("--locked");
    if offline {
        cmd.arg("--offline");
    }
    let out = cmd.output_owned().map_err(|e| {
        format!(
            "could not execute `targo tree`: {e} — install the Trust toolchain (`aterm pkg \
             install trust`), or set CARGO to the targo to use"
        )
    })?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    String::from_utf8(out.stdout).map_err(|e| {
        format!(
            "`targo tree` emitted non-UTF-8 output for `{}`: {e}",
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

    /// Each browser root needs a wasm cell of its own. NEGATIVE CONTROLS: a
    /// root whose cell is gone, and one measured only by a native cell, are
    /// both unmeasured.
    #[test]
    fn independent_wasm_roots_each_need_a_wasm_cell() {
        let roots = ["crates/cpu", "crates/gpu"];
        let mut cells = vec![
            Cell {
                name: "cpu".into(),
                triple: "wasm32-unknown-unknown".into(),
                package: "cpu".into(),
            },
            Cell {
                name: "gpu".into(),
                triple: "wasm32-unknown-unknown".into(),
                package: "gpu".into(),
            },
        ];
        assert!(unmeasured_wasm_roots(&cells, &roots).is_empty());
        cells.remove(0);
        assert_eq!(unmeasured_wasm_roots(&cells, &roots), ["crates/cpu"]);
        cells.push(Cell {
            name: "native-cpu".into(),
            triple: "aarch64-apple-darwin".into(),
            package: "cpu".into(),
        });
        assert_eq!(unmeasured_wasm_roots(&cells, &roots), ["crates/cpu"]);
    }

    /// `cargo tree --prefix depth` lines, as cargo prints them: the depth, the
    /// id, the directory a path package resolved to, the ` (proc-macro)` and
    /// ` (*)` annotations, a directory that itself holds a paren, and a
    /// build-metadata version.
    #[test]
    fn tree_lines_parse_into_depth_id_path_and_annotations() {
        type Want = (
            usize,
            &'static str,
            &'static str,
            Option<&'static str>,
            bool,
            bool,
        );
        let cases: [(&str, Want); 7] = [
            (
                "8unicode-ident v1.0.24",
                (8, "unicode-ident", "1.0.24", None, false, false),
            ),
            (
                "0aterm v0.47.0 (/Users//a/aterm/crates/aterm)",
                (
                    0,
                    "aterm",
                    "0.47.0",
                    Some("/Users//a/aterm/crates/aterm"),
                    false,
                    false,
                ),
            ),
            (
                "6aterm-error-derive v0.47.0 (proc-macro) (/w/crates/aterm-error-derive)",
                (
                    6,
                    "aterm-error-derive",
                    "0.47.0",
                    Some("/w/crates/aterm-error-derive"),
                    true,
                    false,
                ),
            ),
            (
                "3serde v1.0.228 (*)",
                (3, "serde", "1.0.228", None, false, true),
            ),
            (
                "4indexmap v2.14.0 (/Users//a/aterm/vendor/indexmap) (*)",
                (
                    4,
                    "indexmap",
                    "2.14.0",
                    Some("/Users//a/aterm/vendor/indexmap"),
                    false,
                    true,
                ),
            ),
            (
                "2foo v1.0.0 (/Users//a b (1)/vendor/foo) (*)",
                (
                    2,
                    "foo",
                    "1.0.0",
                    Some("/Users//a b (1)/vendor/foo"),
                    false,
                    true,
                ),
            ),
            (
                "4zstd-sys v2.0.16+zstd.1.5.7",
                (4, "zstd-sys", "2.0.16+zstd.1.5.7", None, false, false),
            ),
        ];
        for (raw, (depth, name, version, path, pm, deduped)) in cases {
            let line = parse_line(raw).expect(raw).expect(raw);
            assert_eq!(line.depth, depth, "{raw}");
            assert_eq!(line.id, PkgId::new(name, version), "{raw}");
            assert_eq!(line.path.as_deref(), path.map(Path::new), "{raw}");
            assert_eq!((line.is_proc_macro, line.deduped), (pm, deduped), "{raw}");
        }
        assert_eq!(parse_line("   ").unwrap(), None, "a blank line is skipped");
    }

    /// A line the parser cannot read is an error, never a skip: a skipped line
    /// is a package dropped from every count.
    #[test]
    fn an_unreadable_tree_line_refuses() {
        for raw in ["|-- serde v1.0.228", "2serde", "2serde 1.0.228"] {
            assert!(parse_line(raw).is_err(), "{raw}");
        }
        for tree in ["0root v0.1.0\n2orphan v1.0.0\n", "1alpha v1.0.0\n"] {
            assert!(parse_tree(tree).is_err(), "{tree}");
        }
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
}
