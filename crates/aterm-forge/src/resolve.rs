// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Target cell matrix and offline `cargo tree` dependency graph resolution.

use crate::model::{Cell, Graph, PkgId};
use aterm_uds::output::CommandOutput as _;
use aterm_uds::spawnfd::{Command, command};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Cargo executable and child `PATH` used for `tree` and `metadata` queries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cargo {
    program: OsString,
    path: Option<OsString>,
    pub(crate) parallel_cells: bool,
}

impl Cargo {
    /// Resolves cargo from `$CARGO`, falling back to `targo` on `PATH`.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            program: std::env::var_os("CARGO").unwrap_or_else(|| "targo".into()),
            path: None,
            parallel_cells: true,
        }
    }

    /// Pins `program` with `path` as its child `PATH`.
    #[must_use]
    pub fn pinned(program: impl Into<OsString>, path: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            path: Some(path.into()),
            parallel_cells: true,
        }
    }

    /// Choose whether independent cell graph queries may overlap.
    #[must_use]
    pub fn with_parallel_cells(mut self, enabled: bool) -> Self {
        self.parallel_cells = enabled;
        self
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

/// The eight measurement cells (six native `aterm` targets plus two browser wasm modules).
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
            name: "wasm-cpu".to_string(),
            triple: "wasm32-unknown-unknown".to_string(),
            package: "aterm-wasm".to_string(),
        },
        Cell {
            name: "wasm-gpu".to_string(),
            triple: "wasm32-unknown-unknown".to_string(),
            package: "aterm-gpu-web".to_string(),
        },
        Cell {
            name: "mac-x64".to_string(),
            triple: "x86_64-apple-darwin".to_string(),
            package: "aterm".to_string(),
        },
        Cell {
            name: "linux-arm".to_string(),
            triple: "aarch64-unknown-linux-gnu".to_string(),
            package: "aterm".to_string(),
        },
        Cell {
            name: "win-arm".to_string(),
            triple: "aarch64-pc-windows-msvc".to_string(),
            package: "aterm".to_string(),
        },
    ]
}

/// Returns wasm crate roots in `roots` that lack a `wasm32-unknown-unknown` cell in `cells`.
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

/// Filters `cells` by `--cell` names, preserving order and deduplicating (empty selects all).
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

/// One parsed `cargo tree --prefix depth` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeLine {
    pub depth: usize,
    pub id: PkgId,
    /// Path printed in parentheses for workspace or path-patched packages.
    pub path: Option<PathBuf>,
    /// Whether cargo annotated the line with `(proc-macro)`.
    pub is_proc_macro: bool,
    /// Whether cargo annotated the line with `(*)`.
    pub deduped: bool,
}

/// Parses one depth-prefixed line (`Ok(None)` for blank lines).
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

/// Rebuilds the dependency graph and package path map from `--prefix depth` output.
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

/// Resolves one cell offline first, retrying once without `--offline` and logging on miss.
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

/// A Git package's checked-out source, as reported by the resolver itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPackage {
    /// Canonical Cargo source identity, including its resolved commit.
    pub source: String,
    /// Absolute directory containing the selected package manifest.
    pub directory: PathBuf,
}

/// Git annotations in `targo tree` are URLs, not local source directories.
/// Read their actual directories and full source IDs from the locked graph.
pub fn git_packages(cargo: &Cargo, root: &Path) -> Result<BTreeMap<PkgId, GitPackage>, String> {
    let mut cmd = cargo.command("metadata", root);
    cmd.args(["--format-version", "1", "--locked", "--offline"]);
    let out = cmd
        .output_owned()
        .map_err(|e| format!("cannot read Git package metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cannot read locked Git package metadata: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = std::str::from_utf8(&out.stdout)
        .map_err(|e| format!("Git package metadata is not UTF-8: {e}"))?;
    parse_git_packages(text)
}

fn parse_git_packages(text: &str) -> Result<BTreeMap<PkgId, GitPackage>, String> {
    let value: aterm_json::Value =
        aterm_json::from_str(text).map_err(|e| format!("invalid Git package metadata: {e}"))?;
    let packages = value
        .get("packages")
        .and_then(aterm_json::Value::as_array)
        .ok_or("Git package metadata has no package list")?;
    let mut out = BTreeMap::new();
    let mut identities = BTreeMap::new();
    for package in packages {
        let field = |key| {
            package
                .get(key)
                .and_then(aterm_json::Value::as_str)
                .ok_or_else(|| format!("Git package metadata has no {key}"))
        };
        let id = PkgId::new(field("name")?, field("version")?);
        let manifest = PathBuf::from(field("manifest_path")?);
        let source = package.get("source").and_then(aterm_json::Value::as_str);
        let identity = (source.map(str::to_owned), manifest.clone());
        if let Some(previous) = identities.insert(id.clone(), identity.clone())
            && previous != identity
        {
            return Err(format!("Package {id} has ambiguous sources"));
        }
        let Some(source) = source.filter(|source| source.starts_with("git+")) else {
            continue;
        };
        if !manifest.is_absolute() || manifest.file_name().is_none_or(|name| name != "Cargo.toml") {
            return Err(format!("Git package {id} has no absolute manifest path"));
        }
        let directory = strip_verbatim_prefix(
            manifest
                .parent()
                .ok_or("Git manifest has no parent")?
                .to_path_buf(),
        );
        let resolved = GitPackage {
            source: source.to_owned(),
            directory,
        };
        if let Some(previous) = out.insert(id.clone(), resolved.clone())
            && previous != resolved
        {
            return Err(format!("Git package {id} has ambiguous sources"));
        }
    }
    Ok(out)
}

/// The printed short commit must agree with the resolver's full identity.
pub(crate) fn matching_git_annotation(annotation: &Path, source: &str) -> bool {
    let printed = annotation.to_string_lossy();
    let Some(resolved) = source.strip_prefix("git+") else {
        return false;
    };
    let Some((printed_url, short)) = printed.rsplit_once('#') else {
        return false;
    };
    let Some((resolved_url, full)) = resolved.rsplit_once('#') else {
        return false;
    };
    printed_url == resolved_url
        && (7..=40).contains(&short.len())
        && full.len() == 40
        && full.bytes().all(|byte| byte.is_ascii_hexdigit())
        && full.starts_with(short)
}

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

/// Strips the Windows `\\?\` verbatim-disk prefix (`\\?\C:\x` to `C:\x`).
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
#[path = "resolve_tests.rs"]
mod tests;
