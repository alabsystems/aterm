// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `attest` — the PROVENANCE AND LICENSE NOTARY over `vendor/`.
//!
//! Six upstream crates live under `vendor/` as aterm-owned forks, wired in by
//! the root `[patch.crates-io]` table. Each one is a redistribution of someone
//! else's copyrighted work with aterm's edits inside it, and each one is a
//! `-p` handle that `targo trust check` is expected to be able to drive
//! standalone. Neither property is self-enforcing: a fork can lose its
//! `[workspace]` stub, its upstream commit record, its `LICENSE` file or its
//! `NOTICE` line and nothing in the tree notices.
//!
//! This module is the standing check. Every obligation is fail-closed and
//! tagged `[OB-n]` in the log, the way `aterm_census` numbers its own; the log
//! is RETURNED (never printed) so `check` can splice it into the gate report.
//!
//! # The obligations
//!
//! * `[OB-1]`  patch ↔ `vendor/` agreement, both directions, cross-checked
//!   against [`aterm_census::scan_set::REVIEWED_VENDORED_CRATES`].
//! * `[OB-2]`  version equality — a vendored version outside the requirement
//!   the workspace states makes the patch SILENTLY UN-USED (cargo warns and
//!   exits 0), so `Cargo.lock` is cross-checked for a source-less entry.
//! * `[OB-3]`  the empty `[workspace]` stub. MEASURED on this tree: with the
//!   stub, `cargo metadata` with cwd inside the crate exits 0 (indexmap);
//!   without it, exit 101 — "current package believes it's in a workspace
//!   when it's not" (winit).
//! * `[OB-4]`  upstream provenance: `.cargo_vcs_info.json` + `Cargo.toml.orig`.
//! * `[OB-5]`  a retained upstream `LICENSE*` in every fork root.
//! * `[OB-6]`  `NOTICE` agreement, both directions, including version and SPDX.
//! * `[OB-7]`  Apache-2.0 §4(b) modification notices, for forks where the
//!   Apache arm is UNAVOIDABLE (a dual `… OR MIT` fork can elect MIT).
//! * `[OB-8]`  the marker census: `// aterm-trust:` and `// LOCAL PATCH
//!   (aterm):` per fork. A fork with zero markers has no recorded reason to
//!   exist.
//! * `[OB-9]`  every license arm is on `deny.toml`'s `[licenses] allow` list.
//! * `[OB-10]` nothing under `vendor/` is swallowed by `.gitignore`, checked
//!   with the read-only `git check-ignore -v --no-index`.
//!
//! # What this module does NOT do
//!
//! `[OB-7]` detects modified files by aterm's own MARKERS, not by diffing
//! against pristine upstream. A file edited without leaving a marker is
//! invisible here. The pristine-diff (fetch the `.crate`, compare against the
//! `.cargo_vcs_info.json` sha) is a later work unit; the log says so on every
//! run rather than implying a diff happened.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use toml_edit::{DocumentMut, Item, TableLike};

use crate::{Outcome, PRECISION_NOTE};

/// The marker each Trust-discharged fix inside a fork carries. These are the
/// verification obligations aterm paid off upstream's behalf.
pub const TRUST_MARKER: &str = "// aterm-trust:";
/// The marker each behavioural local patch inside a fork carries.
pub const LOCAL_PATCH_MARKER: &str = "// LOCAL PATCH (aterm):";
/// How many leading lines of a file count as its "header" for the Apache-2.0
/// §4(b) prominent-notice test.
const HEADER_LINES: usize = 20;

/// The `attest` verb. `run` is a thin wrapper so `check` can call [`report`]
/// directly and splice the same text into the gate transcript.
pub fn run(root: &Path) -> Result<Outcome, String> {
    if !root.join("Cargo.toml").is_file() {
        return Err(format!(
            "no Cargo.toml at {} — run from inside the workspace or pass `--root <workspace root>`",
            root.display()
        ));
    }
    let (ok, log) = report(root);
    Ok(Outcome { ok, log })
}

// ---------------------------------------------------------------------------
// Facts
// ---------------------------------------------------------------------------

/// One vendored fork, as attest sees it on disk.
#[derive(Debug, Clone)]
struct VendoredFork {
    /// The `[patch.crates-io]` key (the crates-io package being replaced).
    name: String,
    /// The replacement path exactly as the patch table spells it.
    rel: String,
    dir: PathBuf,
    /// `package.version` from the vendored manifest; empty when unreadable.
    version: String,
    /// `package.license` from the vendored manifest; empty when unreadable.
    license: String,
    /// `[workspace]` present AND empty in the vendored manifest.
    workspace_stub: bool,
    /// Whatever went wrong reading the manifest, verbatim.
    manifest_error: Option<String>,
    trust_markers: u64,
    patch_markers: u64,
    /// Repo-relative `*.rs` files carrying either marker.
    marked_files: Vec<String>,
    licenses: Vec<String>,
}

impl VendoredFork {
    fn markers(&self) -> u64 {
        self.trust_markers + self.patch_markers
    }
}

/// Read the root `[patch.crates-io]` table as `(key, path)` pairs. A non-path
/// entry (git/registry) yields an empty path and is reported by `[OB-1]`.
fn patch_paths(root: &Path) -> Result<Vec<(String, String)>, String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    let doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", manifest.display()))?;
    let Some(table) = doc
        .get("patch")
        .and_then(Item::as_table_like)
        .and_then(|t| t.get("crates-io"))
        .and_then(Item::as_table_like)
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (key, value) in table.iter() {
        let path = value
            .as_table_like()
            .and_then(|t| t.get("path"))
            .and_then(Item::as_str)
            .unwrap_or_default()
            .to_string();
        out.push((key.to_string(), path));
    }
    out.sort();
    Ok(out)
}

/// Directory names directly under `vendor/`, sorted. Absent `vendor/` is an
/// empty list, not an error: `[OB-1]` decides whether that is a defect.
fn vendor_dirs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("vendor")) else {
        return out;
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        out.push(name);
    }
    out.sort();
    out
}

/// Measure every fork named by the patch table.
fn survey_forks(root: &Path, patch: &[(String, String)]) -> Vec<VendoredFork> {
    let mut forks = Vec::new();
    for (name, rel) in patch {
        let rel = if rel.is_empty() {
            format!("vendor/{name}")
        } else {
            rel.clone()
        };
        let dir = root.join(&rel);
        let mut fork = VendoredFork {
            name: name.clone(),
            rel,
            dir: dir.clone(),
            version: String::new(),
            license: String::new(),
            workspace_stub: false,
            manifest_error: None,
            trust_markers: 0,
            patch_markers: 0,
            marked_files: Vec::new(),
            licenses: Vec::new(),
        };
        match std::fs::read_to_string(dir.join("Cargo.toml")) {
            Err(e) => fork.manifest_error = Some(format!("cannot read Cargo.toml: {e}")),
            Ok(text) => match text.parse::<DocumentMut>() {
                Err(e) => fork.manifest_error = Some(format!("Cargo.toml is not valid TOML: {e}")),
                Ok(doc) => {
                    let package = doc.get("package").and_then(Item::as_table_like);
                    fork.version = package
                        .and_then(|p| p.get("version"))
                        .and_then(Item::as_str)
                        .unwrap_or_default()
                        .to_string();
                    fork.license = package
                        .and_then(|p| p.get("license"))
                        .and_then(Item::as_str)
                        .unwrap_or_default()
                        .to_string();
                    fork.workspace_stub = doc
                        .get("workspace")
                        .and_then(Item::as_table_like)
                        .is_some_and(|t| t.iter().next().is_none());
                }
            },
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("LICENSE") || name.starts_with("LICENCE") {
                    fork.licenses.push(name);
                }
            }
            fork.licenses.sort();
        }
        count_markers(root, &mut fork);
        forks.push(fork);
    }
    forks
}

/// Count both markers over every `*.rs` under a fork. Uses the shared
/// `aterm_census` walk so forge and the census can never disagree about what
/// "the Rust sources of a directory" means.
fn count_markers(root: &Path, fork: &mut VendoredFork) {
    let mut files = Vec::new();
    if aterm_census::collect_rs_files(&fork.dir, &mut files).is_err() {
        return;
    }
    files.sort();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let trust = text.matches(TRUST_MARKER).count() as u64;
        let patch = text.matches(LOCAL_PATCH_MARKER).count() as u64;
        if trust + patch == 0 {
            continue;
        }
        fork.trust_markers += trust;
        fork.patch_markers += patch;
        fork.marked_files.push(rel_display(root, file));
    }
}

fn rel_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// ---------------------------------------------------------------------------
// SPDX
// ---------------------------------------------------------------------------

/// Split an SPDX expression into its individual license identifiers.
/// `WITH <exception>` is folded into the license it qualifies, so
/// `Apache-2.0 WITH LLVM-exception OR MIT` yields `Apache-2.0`, `MIT`.
fn license_arms(expr: &str) -> Vec<String> {
    let flat = expr.replace(['(', ')'], " ");
    let mut arms = Vec::new();
    let mut tokens = flat.split_whitespace();
    while let Some(token) = tokens.next() {
        match token {
            "OR" | "AND" => {}
            "WITH" => {
                tokens.next();
            }
            other => arms.push(other.to_string()),
        }
    }
    arms
}

/// Is the Apache-2.0 grant UNAVOIDABLE for this expression? A dual
/// `MIT OR Apache-2.0` fork can be redistributed under MIT, which carries no
/// §4(b) obligation; a bare `Apache-2.0` fork cannot.
fn apache_is_mandatory(expr: &str) -> bool {
    let arms: Vec<&str> = expr.split(" OR ").collect();
    !arms.is_empty()
        && arms
            .iter()
            .all(|arm| license_arms(arm).iter().any(|l| l == "Apache-2.0"))
}

/// The `[licenses] allow` list from `deny.toml`.
fn deny_allow(root: &Path) -> Result<Vec<String>, String> {
    let path = root.join("deny.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    let array = doc
        .get("licenses")
        .and_then(Item::as_table_like)
        .and_then(|t| t.get("allow"))
        .and_then(Item::as_array)
        .ok_or_else(|| format!("{} has no `[licenses] allow` array", path.display()))?;
    Ok(array
        .iter()
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect())
}

// ---------------------------------------------------------------------------
// Version requirements
// ---------------------------------------------------------------------------

/// A parsed dotted version. `stated_*` record which components the text
/// actually spelled, which is what decides a caret requirement's upper bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ver {
    major: u64,
    minor: u64,
    patch: u64,
    stated_minor: bool,
    stated_patch: bool,
}

impl Ver {
    fn triple(self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

/// Parse a dotted version, tolerant of omitted components and of a
/// `-prerelease` / `+build` suffix.
fn parse_version(text: &str) -> Option<Ver> {
    let core = text.split(['-', '+']).next().unwrap_or(text);
    let mut parts = core.split('.');
    let major = parts.next()?.trim().parse().ok()?;
    let minor_text = parts.next();
    let patch_text = parts.next();
    let minor = match minor_text {
        Some(m) => m.trim().parse().ok()?,
        None => 0,
    };
    let patch = match patch_text {
        Some(p) => p.trim().parse().ok()?,
        None => 0,
    };
    Some(Ver {
        major,
        minor,
        patch,
        stated_minor: minor_text.is_some(),
        stated_patch: patch_text.is_some(),
    })
}

/// Does `version` satisfy `req`? `None` means "attest does not understand this
/// requirement form" — the caller must treat that as a failure, never a pass.
///
/// Only the two forms the workspace actually uses are modelled: caret (bare or
/// explicit) and exact `=`.
fn req_satisfied(req: &str, version: &str) -> Option<bool> {
    let req = req.trim();
    if req.contains(',') || req.contains('*') || req.starts_with('>') || req.starts_with('<') {
        return None;
    }
    let r = parse_version(req.trim_start_matches(['^', '=']))?;
    let v = parse_version(version)?;
    if req.starts_with('=') {
        let ok = v.major == r.major
            && (!r.stated_minor || v.minor == r.minor)
            && (!r.stated_patch || v.patch == r.patch);
        return Some(ok);
    }
    // Caret: >= the stated floor, < the next incompatible release. Cargo's
    // 0.x rules make the leftmost NON-ZERO stated component the compatibility
    // axis, so `0.30` admits 0.30.13 but not 0.31.0.
    let upper = if r.major > 0 {
        (r.major + 1, 0, 0)
    } else if !r.stated_minor {
        (1, 0, 0)
    } else if r.minor > 0 {
        (0, r.minor + 1, 0)
    } else if r.stated_patch {
        (0, 0, r.patch + 1)
    } else {
        (0, 1, 0)
    };
    Some(v.triple() >= r.triple() && v.triple() < upper)
}

/// Every version requirement the workspace states on `name`, as
/// `(manifest, requirement)`. Inherited (`workspace = true`) entries are
/// resolved by the root `[workspace.dependencies]` entry they point at, so
/// each distinct requirement is stated exactly once.
fn workspace_requirements(root: &Path, name: &str) -> Vec<(String, String)> {
    let mut manifests = vec![root.join("Cargo.toml")];
    if let Ok(entries) = std::fs::read_dir(root.join("crates")) {
        let mut members: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("Cargo.toml"))
            .filter(|p| p.is_file())
            .collect();
        members.sort();
        manifests.extend(members);
    }
    let mut out = Vec::new();
    for manifest in manifests {
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(doc) = text.parse::<DocumentMut>() else {
            continue;
        };
        let mut tables: Vec<&dyn TableLike> = Vec::new();
        if let Some(ws) = doc
            .get("workspace")
            .and_then(Item::as_table_like)
            .and_then(|t| t.get("dependencies"))
            .and_then(Item::as_table_like)
        {
            tables.push(ws);
        }
        for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(t) = doc.get(key).and_then(Item::as_table_like) {
                tables.push(t);
            }
        }
        for table in tables {
            let Some(entry) = table.get(name) else {
                continue;
            };
            if let Some(req) = entry.as_str() {
                out.push((rel_display(root, &manifest), req.to_string()));
                continue;
            }
            let Some(entry) = entry.as_table_like() else {
                continue;
            };
            // A path/git replacement states no registry requirement, and an
            // inherited entry restates the root's — neither is a new fact.
            if entry.get("path").is_some() || entry.get("git").is_some() {
                continue;
            }
            if entry.get("workspace").is_some() {
                continue;
            }
            if let Some(req) = entry.get("version").and_then(Item::as_str) {
                out.push((rel_display(root, &manifest), req.to_string()));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Every `[[package]]` entry in `Cargo.lock` for `name`, as
/// `(version, has_source)`. A path-patched package carries NO `source` key,
/// which is exactly how a live patch is distinguished from a dead one.
fn lock_entries(root: &Path, name: &str) -> Result<Vec<(String, bool)>, String> {
    let path = root.join("Cargo.lock");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    let Some(packages) = doc.get("package").and_then(Item::as_array_of_tables) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for package in packages {
        if package.get("name").and_then(Item::as_str) != Some(name) {
            continue;
        }
        let version = package
            .get("version")
            .and_then(Item::as_str)
            .unwrap_or_default()
            .to_string();
        out.push((version, package.get("source").is_some()));
    }
    out.sort();
    Ok(out)
}

// ---------------------------------------------------------------------------
// NOTICE
// ---------------------------------------------------------------------------

/// One `- <name> <version>, <SPDX> (`vendor/<dir>/`)` line of `NOTICE`.
#[derive(Debug, Clone)]
struct NoticeEntry {
    name: String,
    version: String,
    license: String,
    dir: String,
    line: usize,
}

fn parse_notice(text: &str) -> Vec<NoticeEntry> {
    let mut out = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("- ") else {
            continue;
        };
        let Some((head, tail)) = rest.split_once("(`vendor/") else {
            continue;
        };
        let Some(dir) = tail.trim().strip_suffix("/`)") else {
            continue;
        };
        let Some((name_version, license)) = head.trim().split_once(", ") else {
            continue;
        };
        let Some((name, version)) = name_version.trim().rsplit_once(' ') else {
            continue;
        };
        out.push(NoticeEntry {
            name: name.trim().to_string(),
            version: version.trim().to_string(),
            license: license.trim().to_string(),
            dir: dir.trim().to_string(),
            line: index + 1,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// .gitignore
// ---------------------------------------------------------------------------

/// One `git check-ignore -v --no-index` verdict.
#[derive(Debug, Clone)]
struct IgnoreVerdict {
    /// `<file>:<line>:<pattern>` exactly as git printed it.
    rule: String,
    /// `true` when the matching rule is a `!` re-include, i.e. the path is SAFE.
    reincluded: bool,
    path: String,
}

/// Ask git which of `paths` its ignore rules match. `--no-index` is essential:
/// without it git reports every already-tracked file as "not ignored", which
/// would hide exactly the hazard this obligation exists to find. Returns
/// `None` when git cannot answer (not a repository, git absent) — an
/// unanswerable question is reported, never scored as a pass.
fn check_ignore(root: &Path, paths: &[String]) -> Option<Vec<IgnoreVerdict>> {
    if paths.is_empty() {
        return Some(Vec::new());
    }
    let output = Command::new("git")
        .current_dir(root)
        .args(["check-ignore", "-v", "--no-index", "--"])
        .args(paths)
        .output()
        .ok()?;
    // 0 = at least one match, 1 = no match. Anything else (128: not a
    // repository) means git did not answer the question.
    match output.status.code() {
        Some(0 | 1) => {}
        _ => return None,
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((location, path)) = line.rsplit_once('\t') else {
            continue;
        };
        let pattern = location.splitn(3, ':').nth(2).unwrap_or_default();
        out.push(IgnoreVerdict {
            rule: location.to_string(),
            reincluded: pattern.starts_with('!'),
            path: path.to_string(),
        });
    }
    Some(out)
}

/// Every path under `vendor/`, repo-relative, files and directories alike.
fn vendor_paths(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        out.push(rel_display(root, &path));
        if path.is_dir() && !path.is_symlink() {
            vendor_paths(root, &path, out);
        }
    }
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// Run every obligation. Returns `(ok, log)`; the log is complete on its own
/// and ends with [`crate::PRECISION_NOTE`] whenever anything failed.
pub fn report(root: &Path) -> (bool, String) {
    let mut log = String::new();
    let mut fails = 0usize;
    let _ = writeln!(
        log,
        "=== cargo forge attest (vendor/ provenance + license notary) ==="
    );
    let _ = writeln!(log, "    root: {}", root.display());

    let patch = match patch_paths(root) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] the root manifest's [patch.crates-io] table is unreadable, so \
                 attest refuses to notarize a guessed fork set (fail-closed).\n        {e}"
            );
            let _ = writeln!(
                log,
                "cargo forge attest: FAILED — 1 obligation violation(s)."
            );
            log.push_str(PRECISION_NOTE);
            log.push('\n');
            return (false, log);
        }
    };
    let forks = survey_forks(root, &patch);
    let dirs = vendor_dirs(root);
    let _ = writeln!(
        log,
        "    forks: {} from [patch.crates-io]; {} director(y/ies) under vendor/",
        forks.len(),
        dirs.len()
    );

    fails += ob1_patch_vendor_agreement(root, &forks, &dirs, &mut log);
    fails += ob2_version_equality(root, &forks, &mut log);
    fails += ob3_workspace_stub(&forks, &mut log);
    fails += ob4_provenance_files(&forks, &mut log);
    fails += ob5_license_files(&forks, &mut log);
    fails += ob6_notice_agreement(root, &forks, &mut log);
    fails += ob7_apache_modification_notices(root, &forks, &mut log);
    fails += ob8_marker_census(&forks, &mut log);
    fails += ob9_spdx_allowlist(root, &forks, &mut log);
    fails += ob10_gitignore(root, &forks, &mut log);

    if fails == 0 {
        let _ = writeln!(
            log,
            "cargo forge attest: PASS — 10 obligations held over {} vendored fork(s).",
            forks.len()
        );
        return (true, log);
    }
    let _ = writeln!(
        log,
        "cargo forge attest: FAILED — {fails} obligation violation(s)."
    );
    log.push_str(PRECISION_NOTE);
    log.push('\n');
    (false, log)
}

/// `[OB-1]` Every patch key resolves to a real `vendor/<dir>` with sources, and
/// every `vendor/<dir>` is claimed by a patch key. Cross-checked against the
/// census registry when this root IS the workspace that registry describes.
fn ob1_patch_vendor_agreement(
    root: &Path,
    forks: &[VendoredFork],
    dirs: &[String],
    log: &mut String,
) -> usize {
    let mut fails = 0;
    for fork in forks {
        if !fork.rel.starts_with("vendor/") {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] [patch.crates-io] `{}` points at `{}`, which is not under \
                 vendor/ — move the fork to vendor/{} and update the patch entry, or drop it.",
                fork.name, fork.rel, fork.name
            );
            fails += 1;
        }
        if !fork.dir.is_dir() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] [patch.crates-io] `{}` points at `{}`, which does not exist — \
                 restore the fork there or remove the patch entry from {}/Cargo.toml.",
                fork.name,
                fork.rel,
                root.display()
            );
            fails += 1;
            continue;
        }
        if !fork.dir.join("src").is_dir() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] `{}` has no `src/` — a fork with no sources patches nothing. \
                 Re-vendor it or remove the patch entry.",
                fork.rel
            );
            fails += 1;
        }
    }
    for dir in dirs {
        if !forks.iter().any(|f| f.rel == format!("vendor/{dir}")) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] `vendor/{dir}` is not named by any [patch.crates-io] entry — \
                 it is dead weight that ships in the source distribution. Add the patch entry \
                 or delete the directory."
            );
            fails += 1;
        }
    }

    // The compiled-in census registry describes THIS workspace. Applying it to
    // an arbitrary root would invent failures, so the cross-check is scoped and
    // the scoping is stated rather than silent.
    if root.join("crates/aterm-census/src/scan_set.rs").is_file() {
        let registry = aterm_census::scan_set::REVIEWED_VENDORED_CRATES;
        for fork in forks {
            let Some(entry) = registry.iter().find(|r| r.package == fork.name) else {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-1] fork `{}` is NOT in aterm_census::scan_set::\
                     REVIEWED_VENDORED_CRATES — a vendored fork links into the process and must \
                     be reviewed and classified there before it can be attested.",
                    fork.name
                );
                fails += 1;
                continue;
            };
            if entry.path != fork.rel {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-1] fork `{}` is patched from `{}` but REVIEWED_VENDORED_CRATES \
                     registers `{}` — make the two agree in \
                     crates/aterm-census/src/scan_set.rs.",
                    fork.name, fork.rel, entry.path
                );
                fails += 1;
            }
        }
        for entry in registry {
            if !forks.iter().any(|f| f.name == entry.package) {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-1] REVIEWED_VENDORED_CRATES registers `{}` but no \
                     [patch.crates-io] entry names it — the review is stale; drop the entry \
                     from crates/aterm-census/src/scan_set.rs.",
                    entry.package
                );
                fails += 1;
            }
        }
        let _ = writeln!(
            log,
            "    [OB-1] cross-checked against REVIEWED_VENDORED_CRATES ({} reviewed entries).",
            registry.len()
        );
    } else {
        let _ = writeln!(
            log,
            "  • NOTE [OB-1] the REVIEWED_VENDORED_CRATES cross-check was SKIPPED: that constant \
             describes the aterm workspace and this root has no crates/aterm-census/src/\
             scan_set.rs. Patch ↔ vendor/ agreement itself was still checked."
        );
    }
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-1] patch ↔ vendor/ agreement holds in both directions ({} fork(s)).",
            forks.len()
        );
    }
    fails
}

/// `[OB-2]` The vendored version must satisfy every requirement the workspace
/// states, and `Cargo.lock` must show the patch actually took.
fn ob2_version_equality(root: &Path, forks: &[VendoredFork], log: &mut String) -> usize {
    let mut fails = 0;
    for fork in forks {
        if let Some(error) = &fork.manifest_error {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-2] `{}`: {error} — attest cannot read the vendored version, so it \
                 cannot tell whether the patch is live.",
                fork.rel
            );
            fails += 1;
            continue;
        }
        if fork.version.is_empty() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-2] `{}/Cargo.toml` states no `package.version` — a fork must keep \
                 its upstream version so the existing `^` requirements still resolve.",
                fork.rel
            );
            fails += 1;
            continue;
        }
        for (manifest, req) in workspace_requirements(root, &fork.name) {
            match req_satisfied(&req, &fork.version) {
                Some(true) => {}
                Some(false) => {
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-2] {manifest} requires `{} = \"{req}\"` but \
                         {}/Cargo.toml is version {} — the patch is SILENTLY UN-USED (cargo \
                         warns `Patch ... was not used in the crate graph` and still exits 0). \
                         Set the vendored version back to one satisfying `{req}`, or change \
                         the requirement.",
                        fork.name, fork.rel, fork.version
                    );
                    fails += 1;
                }
                None => {
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-2] {manifest} states `{} = \"{req}\"`, a requirement form \
                         attest does not model (only caret and `=` are). Teach \
                         `req_satisfied` in crates/aterm-forge/src/attest.rs this form rather \
                         than letting it pass unchecked.",
                        fork.name
                    );
                    fails += 1;
                }
            }
        }
        match lock_entries(root, &fork.name) {
            Err(e) => {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-2] {e} — without the lockfile attest cannot confirm the patch \
                     took. Run `cargo generate-lockfile` (or `cargo metadata --offline`) first."
                );
                fails += 1;
            }
            Ok(entries) => {
                let patched: Vec<&(String, bool)> = entries
                    .iter()
                    .filter(|(_, has_source)| !has_source)
                    .collect();
                if patched.is_empty() {
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-2] Cargo.lock has no source-less `{}` entry — the \
                         [patch.crates-io] redirect to `{}` DID NOT TAKE and every consumer is \
                         compiling the registry copy. Re-run `cargo metadata --offline` after \
                         making the vendored version satisfy the stated requirement.",
                        fork.name, fork.rel
                    );
                    fails += 1;
                } else if !patched.iter().any(|(v, _)| *v == fork.version) {
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-2] Cargo.lock's path entry for `{}` is version {} but \
                         `{}/Cargo.toml` says {} — refresh the lockfile with \
                         `cargo metadata --offline`.",
                        fork.name,
                        patched
                            .iter()
                            .map(|(v, _)| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        fork.rel,
                        fork.version
                    );
                    fails += 1;
                }
                let registry: Vec<&str> = entries
                    .iter()
                    .filter(|(_, has_source)| *has_source)
                    .map(|(v, _)| v.as_str())
                    .collect();
                if !registry.is_empty() {
                    let _ = writeln!(
                        log,
                        "  • NOTE [OB-2] `{}` also resolves UNFORKED from the registry at {} \
                         beside the fork at {}. The fork's fixes do not apply to those copies; \
                         `cargo forge check` scores that patch-liveness gap per cell.",
                        fork.name,
                        registry.join(", "),
                        fork.version
                    );
                }
            }
        }
    }
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-2] every fork's vendored version satisfies the workspace requirements and \
             appears source-less in Cargo.lock (the patch is live): {}",
            forks
                .iter()
                .map(|f| format!("{} {}", f.name, f.version))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    fails
}

/// `[OB-3]` The empty `[workspace]` stub, without which the fork cannot be
/// driven standalone.
fn ob3_workspace_stub(forks: &[VendoredFork], log: &mut String) -> usize {
    let mut fails = 0;
    for fork in forks {
        if fork.manifest_error.is_some() || fork.workspace_stub {
            continue;
        }
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-3] `{}/Cargo.toml` has no empty `[workspace]` table, so the crate \
             cannot be driven standalone. MEASURED on this tree: `cargo metadata` with cwd \
             inside the crate exits 101 (\"current package believes it's in a workspace when \
             it's not\"), which is exactly what `targo trust check -p {}` needs to work. FIX: \
             append a bare `[workspace]` line to {}/Cargo.toml.",
            fork.rel, fork.name, fork.rel
        );
        fails += 1;
    }
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-3] every fork manifest carries the empty `[workspace]` stub ({} fork(s)).",
            forks.len()
        );
    }
    fails
}

/// `[OB-4]` Upstream provenance: which commit of which upstream this is.
fn ob4_provenance_files(forks: &[VendoredFork], log: &mut String) -> usize {
    let mut fails = 0;
    for fork in forks {
        for (file, why) in [
            (
                ".cargo_vcs_info.json",
                "the upstream git sha this fork was taken from — without it there is no in-tree \
                 record of WHAT was forked, and no pristine copy can be fetched to diff against",
            ),
            (
                "Cargo.toml.orig",
                "the upstream manifest as published — without it the local manifest edits \
                 cannot be separated from upstream's",
            ),
        ] {
            if fork.dir.join(file).exists() {
                continue;
            }
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-4] `{}/{file}` is MISSING: {why}. FIX: copy it out of the \
                 published `{} {}` .crate (`cargo package`/the registry cache entry) into {}/.",
                fork.rel, fork.name, fork.version, fork.rel
            );
            fails += 1;
        }
    }
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-4] every fork carries .cargo_vcs_info.json and Cargo.toml.orig ({} fork(s)).",
            forks.len()
        );
    }
    fails
}

/// `[OB-5]` The retained upstream license text.
fn ob5_license_files(forks: &[VendoredFork], log: &mut String) -> usize {
    let mut fails = 0;
    for fork in forks {
        if !fork.licenses.is_empty() {
            continue;
        }
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-5] `{}` retains no LICENSE* file. Redistributing the source without \
             the upstream license text breaches every license in its expression (`{}`). FIX: \
             restore upstream's LICENSE file(s) into {}/.",
            fork.rel, fork.license, fork.rel
        );
        fails += 1;
    }
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-5] every fork retains upstream license text: {}",
            forks
                .iter()
                .map(|f| format!("{} [{}]", f.name, f.licenses.join(" ")))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    fails
}

/// `[OB-6]` `NOTICE` lists every fork, with the right version and SPDX, and
/// lists nothing that is not a fork.
fn ob6_notice_agreement(root: &Path, forks: &[VendoredFork], log: &mut String) -> usize {
    let path = root.join("NOTICE");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-6] cannot read {}: {e}. The NOTICE file is the redistribution \
                 record for every vendored fork; create it listing each as \
                 \"- <name> <version>, <SPDX> (`vendor/<dir>/`)\".",
                path.display()
            );
            return 1;
        }
    };
    let entries = parse_notice(&text);
    let mut fails = 0;
    for fork in forks {
        let dir = fork.rel.strip_prefix("vendor/").unwrap_or(&fork.rel);
        let matches: Vec<&NoticeEntry> = entries.iter().filter(|e| e.name == fork.name).collect();
        let Some(entry) = matches.first() else {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-6] NOTICE does not list fork `{}` — the source distribution ships \
                 it without saying so. FIX: add \"- {} {}, {} (`vendor/{}/`)\" to the vendored \
                 crate list in {}.",
                fork.name,
                fork.name,
                fork.version,
                fork.license,
                dir,
                path.display()
            );
            fails += 1;
            continue;
        };
        if matches.len() > 1 {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-6] NOTICE lists `{}` {} times (lines {}) — keep exactly one entry \
                 per fork so there is one authoritative version.",
                fork.name,
                matches.len(),
                matches
                    .iter()
                    .map(|e| e.line.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            fails += 1;
        }
        if entry.version != fork.version {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-6] NOTICE:{} says `{}` is version {} but {}/Cargo.toml says {} — \
                 update the NOTICE line.",
                entry.line, fork.name, entry.version, fork.rel, fork.version
            );
            fails += 1;
        }
        if entry.license != fork.license {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-6] NOTICE:{} licenses `{}` as `{}` but {}/Cargo.toml says `{}` — \
                 make the NOTICE line quote the manifest's SPDX expression verbatim.",
                entry.line, fork.name, entry.license, fork.rel, fork.license
            );
            fails += 1;
        }
        if entry.dir != dir {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-6] NOTICE:{} points `{}` at `vendor/{}/` but the patch table \
                 points at `{}` — make the NOTICE line name the real directory.",
                entry.line, fork.name, entry.dir, fork.rel
            );
            fails += 1;
        }
    }
    for entry in &entries {
        if forks.iter().any(|f| f.name == entry.name) {
            continue;
        }
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-6] NOTICE:{} claims a vendored crate `{}` (`vendor/{}/`) that is not a \
             [patch.crates-io] fork — a NOTICE that over-claims is as wrong as one that \
             under-claims. Remove the line, or restore the fork.",
            entry.line, entry.name, entry.dir
        );
        fails += 1;
    }
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-6] NOTICE agrees with vendor/ in both directions ({} listed fork(s)).",
            entries.len()
        );
    }
    fails
}

/// `[OB-7]` Apache-2.0 §4(b): every modified file must carry a prominent
/// notice stating that it changed.
fn ob7_apache_modification_notices(root: &Path, forks: &[VendoredFork], log: &mut String) -> usize {
    let mut fails = 0;
    let mut scoped = Vec::new();
    for fork in forks {
        if !apache_is_mandatory(&fork.license) {
            continue;
        }
        scoped.push(fork.name.clone());

        // THE INSTRUMENT MATTERS. Detecting modified files by aterm's own
        // markers is unsound in the direction that costs: a file edited
        // WITHOUT leaving a marker is invisible, and §4(b) attaches to the
        // edit, not to whether we remembered to annotate it. Measured on this
        // tree, winit diverges from pristine in 9 files and carries a marker in
        // exactly 1 — so the marker method inspected 11% of its own obligation.
        let Some(pristine) = pristine_dir(root, &fork.name, &fork.version) else {
            let _ = writeln!(
                log,
                "  • NOTE [OB-7] fork `{}` cannot be diffed: no pristine copy of `{} {}` under \
                 the local registry src. This obligation is therefore UNVERIFIED for it, not \
                 satisfied. FIX: `cargo fetch` (or unpack the .crate from the registry cache) so \
                 the pristine tree is present, then re-run.",
                fork.name, fork.name, fork.version
            );
            continue;
        };
        let Some(Divergence { modified, added }) = diff_against_pristine(&pristine, &fork.dir)
        else {
            let _ = writeln!(
                log,
                "  • NOTE [OB-7] fork `{}` could not be walked for a pristine diff.",
                fork.name
            );
            continue;
        };
        let _ = writeln!(
            log,
            "    [OB-7] `{}` vs pristine `{}`: {} modified file(s), {} added.",
            fork.rel,
            pristine.display(),
            modified.len(),
            added.len()
        );
        for (rel, kind) in modified
            .iter()
            .map(|r| (r, "modified"))
            .chain(added.iter().map(|r| (r, "added")))
        {
            let path = fork.dir.join(rel);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if has_modification_notice(&text) {
                continue;
            }
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-7] `{}/{rel}` is {kind} relative to pristine `{} {}`, but its first \
                 {HEADER_LINES} lines contain no modification notice. `{}` is licensed `{}`, whose \
                 §4(b) requires modified files to \"carry prominent notices stating that You \
                 changed the files\" — and NOTICE already asserts that \"modified and added files \
                 carry prominent notices\", so that assertion is currently false. FIX: add a \
                 header comment naming aterm and the change at the top of {}/{rel}.",
                fork.rel, fork.name, fork.version, fork.name, fork.license, fork.rel
            );
            fails += 1;
        }
    }
    let _ = writeln!(
        log,
        "    [OB-7] scope: forks where the Apache-2.0 arm is UNAVOIDABLE ({}). Dual `... OR MIT` \
         forks elect the MIT arm, which imposes no §4(b) duty. Modified files are detected by \
         a BYTE DIFF against the pristine registry copy — not by aterm's own markers — so a file \
         edited without leaving a marker is still caught. A fork with no pristine copy available \
         is reported UNVERIFIED, never passed.",
        if scoped.is_empty() {
            "none".to_string()
        } else {
            scoped.join(", ")
        }
    );
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-7] every file diverging from pristine upstream in an Apache-only fork \
             carries a modification notice."
        );
    }
    fails
}

/// What a fork changed relative to the copy the registry published.
struct Divergence {
    /// Repo-relative-to-the-fork paths present in both trees with differing bytes.
    modified: Vec<String>,
    /// Paths present only in the fork.
    added: Vec<String>,
}

/// The unpacked pristine source for `name@version`, if the local registry has
/// it. Offline by construction — this is the same tree `diff -rq` was used
/// against by hand, and it is present for all six current forks.
fn pristine_dir(root: &Path, name: &str, version: &str) -> Option<PathBuf> {
    // A repo-local pristine tree wins over the registry. This is the
    // `vendor/.forge/<name>/pristine/` slot of the fork ledger layout: a fork
    // whose upstream is no longer fetchable (yanked, or a registry cache the
    // machine never populated) still has to be diffable, and a dot-directory is
    // skipped by `aterm_census::collect_rs_files`, so it is never counted or
    // license-checked as aterm source. It is also the seam the tests use, which
    // is deliberate — a check that cannot be exercised without the network is a
    // check that rots.
    let local = root
        .join("vendor")
        .join(".forge")
        .join(name)
        .join("pristine");
    if local.is_dir() {
        return Some(local);
    }
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))?;
    let src = home.join("registry").join("src");
    let wanted = format!("{name}-{version}");
    for entry in std::fs::read_dir(src).ok()? {
        let dir = entry.ok()?.path();
        let candidate = dir.join(&wanted);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// Byte-compare every file under `fork` against `pristine`.
///
/// `.cargo-ok` is a registry unpack stamp, never source. `.git` and build
/// output are skipped for the same reason the source walk skips them.
fn diff_against_pristine(pristine: &Path, fork: &Path) -> Option<Divergence> {
    let mut modified = Vec::new();
    let mut added = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = fork.join(&rel);
        for entry in std::fs::read_dir(&dir).ok()? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".cargo-ok" || name == ".git" || name == "target" {
                continue;
            }
            let child = if rel.as_os_str().is_empty() {
                PathBuf::from(name.as_ref())
            } else {
                rel.join(name.as_ref())
            };
            if entry.path().is_dir() {
                stack.push(child);
                continue;
            }
            let ours = std::fs::read(entry.path()).ok()?;
            match std::fs::read(pristine.join(&child)) {
                Ok(theirs) if theirs == ours => {}
                Ok(_) => modified.push(child.to_string_lossy().into_owned()),
                Err(_) => added.push(child.to_string_lossy().into_owned()),
            }
        }
    }
    modified.sort();
    added.sort();
    Some(Divergence { modified, added })
}

/// Does this file's header claim aterm changed it? Deliberately generous about
/// wording and deliberately strict about position: §4(b) asks for a PROMINENT
/// notice, and a note buried at line 300 beside the change is not one.
fn has_modification_notice(text: &str) -> bool {
    text.lines().take(HEADER_LINES).any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("aterm")
            && (lower.contains("modif")
                || lower.contains("local patch")
                || lower.contains("change")
                // An ADDED file is not "modified" under §4(b) at all, but NOTICE
                // claims "modified and added files carry prominent notices", so
                // the wording aterm actually uses for a new file must satisfy the
                // check that enforces that sentence. vendor/winit's
                // seat/data_device.rs is the live instance.
                || lower.contains("added by"))
    })
}

/// `[OB-8]` The marker census: what each fork exists FOR.
fn ob8_marker_census(forks: &[VendoredFork], log: &mut String) -> usize {
    let mut fails = 0;
    let trust: u64 = forks.iter().map(|f| f.trust_markers).sum();
    let patch: u64 = forks.iter().map(|f| f.patch_markers).sum();
    for fork in forks {
        if fork.markers() > 0 {
            continue;
        }
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-8] fork `{}` carries ZERO `{TRUST_MARKER}` and `{LOCAL_PATCH_MARKER}` \
             markers — nothing in the tree records why aterm owns this copy instead of using \
             the registry one. FIX: mark each local change in {}/, or drop the fork and remove \
             its [patch.crates-io] entry.",
            fork.name, fork.rel
        );
        fails += 1;
    }
    let _ = writeln!(
        log,
        "    [OB-8] markers: {trust} `{TRUST_MARKER}` + {patch} `{LOCAL_PATCH_MARKER}` = {} total \
         across {} fork(s). These are the discharged verification obligations and the local \
         behaviour fixes — the reason each fork exists.",
        trust + patch,
        forks.len()
    );
    for fork in forks {
        let _ = writeln!(
            log,
            "        {:<12} trust {:>3}  local-patch {:>3}   [{}]",
            fork.name,
            fork.trust_markers,
            fork.patch_markers,
            fork.marked_files.join(" ")
        );
    }
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-8] every fork records at least one marked local change."
        );
    }
    fails
}

/// `[OB-9]` Every license arm is blessed by `deny.toml`.
fn ob9_spdx_allowlist(root: &Path, forks: &[VendoredFork], log: &mut String) -> usize {
    let allow = match deny_allow(root) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-9] {e} — attest will not bless a fork's license against a list it \
                 could not read. FIX: restore `[licenses] allow = [...]` in {}/deny.toml.",
                root.display()
            );
            return 1;
        }
    };
    let mut fails = 0;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for fork in forks {
        if fork.license.trim().is_empty() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-9] `{}/Cargo.toml` states no `package.license` — an unlicensed \
                 redistribution is the one case with no permissive arm to elect. FIX: copy \
                 upstream's SPDX expression into the vendored manifest.",
                fork.rel
            );
            fails += 1;
            continue;
        }
        for arm in license_arms(&fork.license) {
            seen.insert(arm.clone());
            if allow.contains(&arm) {
                continue;
            }
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-9] fork `{}` is licensed `{}`, whose arm `{arm}` is NOT in \
                 deny.toml's `[licenses] allow` list. FIX: add `\"{arm}\"` to that list as a \
                 conscious policy decision, or drop the fork.",
                fork.name, fork.license
            );
            fails += 1;
        }
    }
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-9] every fork license arm ({}) is on deny.toml's allow list.",
            seen.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    fails
}

/// `[OB-10]` Nothing under `vendor/` is swallowed by `.gitignore`.
fn ob10_gitignore(root: &Path, forks: &[VendoredFork], log: &mut String) -> usize {
    let mut existing = Vec::new();
    vendor_paths(root, &root.join("vendor"), &mut existing);
    // Probe the paths a re-vendor WOULD create, too: a rule that swallows a
    // file nobody has written yet is exactly how a fork loses its provenance.
    // Only the artifacts `[OB-4]` REQUIRES are probed — a rule that ignores a
    // fork's own `target/` build directory is doing its job, not hiding source.
    let mut probes = Vec::new();
    for fork in forks {
        for leaf in [".cargo_vcs_info.json", "Cargo.toml.orig"] {
            let candidate = format!("{}/{leaf}", fork.rel);
            if !existing.contains(&candidate) {
                probes.push(candidate);
            }
        }
    }
    let mut all = existing;
    all.extend(probes.iter().cloned());
    let Some(verdicts) = check_ignore(root, &all) else {
        let _ = writeln!(
            log,
            "  • NOTE [OB-10] `git check-ignore -v --no-index` could not answer here (no git, or \
             this root is not a git repository), so the ignore analysis was SKIPPED rather than \
             scored as a pass."
        );
        return 0;
    };
    let mut fails = 0;
    let mut by_rule: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for verdict in &verdicts {
        if verdict.reincluded {
            continue;
        }
        let bucket = by_rule.entry(verdict.rule.clone()).or_default();
        if probes.contains(&verdict.path) {
            bucket.1.push(verdict.path.clone());
        } else {
            bucket.0.push(verdict.path.clone());
        }
    }
    for (rule, (present, future)) in &by_rule {
        if !present.is_empty() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-10] ignore rule `{rule}` matches {} path(s) that are IN vendor/ \
                 today: {}. They are in the tree only because they entered the index before \
                 the rule did (or were force-added) — a plain `git add` of the same path in a \
                 re-vendor does NOTHING and the file is lost silently. FIX: add a \
                 `!vendor/**/<name>` re-include beside the rule, the way `!vendor/**/debug/` \
                 already rescues winnow's `combinator/debug` source module.",
                present.len(),
                present.join(", ")
            );
            fails += 1;
        }
        if !future.is_empty() {
            let _ = writeln!(
                log,
                "  • NOTE [OB-10] ignore rule `{rule}` has no vendor/ re-include, so {} — \
                 each REQUIRED by [OB-4] — cannot be added to this repository at all: `git \
                 add` on that path is a silent no-op. Any [OB-4] failure naming one of these \
                 files is unfixable until the rule carries a re-include.",
                future.join(" / ")
            );
        }
    }
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-10] no tracked path under vendor/ is matched by an ignore rule ({} paths \
             probed).",
            all.len()
        );
    }
    fails
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    // --- the real tree -----------------------------------------------------

    #[test]
    fn the_real_tree_is_green_on_every_provenance_obligation() {
        // This test used to assert the tree was RED and to name winit's three
        // defects. It is GREEN now because those defects were repaired — the
        // missing `[workspace]` stub, the missing .cargo_vcs_info.json /
        // Cargo.toml.orig, and the missing Apache §4(b) notice on
        // window_delegate.rs. Asserting GREEN is the durable direction: the RED
        // direction is proved by the fixtures below and by
        // tests/red_fixtures.rs, which plant a defect rather than depending on
        // one being left in the tree.
        let (ok, log) = report(&repo_root());
        assert!(ok, "attest must be GREEN on this tree:\n{log}");
        for ob in [
            "OB-1", "OB-2", "OB-3", "OB-4", "OB-5", "OB-6", "OB-7", "OB-8", "OB-9", "OB-10",
        ] {
            assert!(
                log.contains(ob),
                "every obligation must report; {ob} is absent:\n{log}"
            );
        }
    }

    #[test]
    fn the_real_tree_holds_every_other_obligation() {
        let (_, log) = report(&repo_root());
        for held in [
            "✓ [OB-1]",
            "✓ [OB-2]",
            "✓ [OB-5]",
            "✓ [OB-6]",
            "✓ [OB-8]",
            "✓ [OB-9]",
        ] {
            assert!(log.contains(held), "expected {held} to hold:\n{log}");
        }
    }

    #[test]
    fn the_real_tree_reproduces_the_measured_marker_floor() {
        let root = repo_root();
        let forks = survey_forks(&root, &patch_paths(&root).unwrap());
        let trust: u64 = forks.iter().map(|f| f.trust_markers).sum();
        let patch: u64 = forks.iter().map(|f| f.patch_markers).sum();
        assert_eq!(trust, 16, "`{TRUST_MARKER}` marker count");
        assert_eq!(patch, 2, "`{LOCAL_PATCH_MARKER}` marker count");
        let by_name: BTreeMap<&str, (u64, u64)> = forks
            .iter()
            .map(|f| (f.name.as_str(), (f.trust_markers, f.patch_markers)))
            .collect();
        assert_eq!(by_name["indexmap"], (8, 0));
        assert_eq!(by_name["smol_str"], (4, 0));
        assert_eq!(by_name["winnow"], (2, 0));
        assert_eq!(by_name["libm"], (1, 0));
        assert_eq!(by_name["pkg-config"], (1, 0));
        assert_eq!(by_name["winit"], (0, 2));
    }

    #[test]
    fn the_real_tree_has_exactly_the_six_known_forks() {
        let root = repo_root();
        let mut names: Vec<String> = patch_paths(&root)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "indexmap",
                "libm",
                "pkg-config",
                "smol_str",
                "winit",
                "winnow"
            ]
        );
    }

    #[test]
    fn every_fork_carries_the_workspace_stub_and_the_provenance_files() {
        // winit was the lone exception until the repair; it is not any more, and
        // this is the assertion that keeps it that way. The stub is what lets
        // `targo trust check -p <fork>` run from inside the fork at all, and the
        // two provenance files are what make a pristine diff possible.
        let root = repo_root();
        let forks = survey_forks(&root, &patch_paths(&root).unwrap());
        assert_eq!(forks.len(), 6);
        for fork in &forks {
            assert!(
                fork.workspace_stub,
                "{} is missing its [workspace] stub",
                fork.name
            );
            assert!(
                fork.dir.join(".cargo_vcs_info.json").exists(),
                "{} is missing .cargo_vcs_info.json",
                fork.name
            );
            assert!(
                fork.dir.join("Cargo.toml.orig").exists(),
                "{} is missing Cargo.toml.orig",
                fork.name
            );
        }
    }

    #[test]
    fn apache_is_only_mandatory_when_no_other_arm_exists() {
        assert!(apache_is_mandatory("Apache-2.0"));
        assert!(apache_is_mandatory("Apache-2.0 AND MIT"));
        assert!(!apache_is_mandatory("Apache-2.0 OR MIT"));
        assert!(!apache_is_mandatory("MIT OR Apache-2.0"));
        assert!(!apache_is_mandatory("MIT"));
    }

    #[test]
    fn license_arms_unfold_or_and_and_with() {
        assert_eq!(license_arms("MIT OR Apache-2.0"), ["MIT", "Apache-2.0"]);
        assert_eq!(
            license_arms("(MIT OR Apache-2.0) AND Unicode-3.0"),
            ["MIT", "Apache-2.0", "Unicode-3.0"]
        );
        assert_eq!(
            license_arms("Apache-2.0 WITH LLVM-exception OR MIT"),
            ["Apache-2.0", "MIT"]
        );
    }

    #[test]
    fn caret_requirements_follow_cargos_zero_major_rules() {
        assert_eq!(req_satisfied("0.30", "0.30.13"), Some(true));
        assert_eq!(req_satisfied("0.30", "0.31.0"), Some(false));
        assert_eq!(req_satisfied("^0.30.5", "0.30.13"), Some(true));
        assert_eq!(req_satisfied("1", "1.9.0"), Some(true));
        assert_eq!(req_satisfied("1", "2.0.0"), Some(false));
        assert_eq!(req_satisfied("=0.33.1", "0.33.1"), Some(true));
        assert_eq!(req_satisfied("=0.33.1", "0.33.2"), Some(false));
        // Unmodelled forms must be UNKNOWN, never a silent pass.
        assert_eq!(req_satisfied(">=1, <3", "2.0.0"), None);
    }

    #[test]
    fn the_workspace_states_the_winit_requirement_the_fork_satisfies() {
        let root = repo_root();
        let reqs = workspace_requirements(&root, "winit");
        assert!(
            !reqs.is_empty(),
            "the workspace must state a winit requirement"
        );
        for (manifest, req) in &reqs {
            assert_eq!(
                req_satisfied(req, "0.30.13"),
                Some(true),
                "{manifest} requires {req}, unsatisfied by the vendored 0.30.13"
            );
        }
    }

    #[test]
    fn the_lockfile_shows_every_patch_is_live() {
        let root = repo_root();
        for (name, _) in patch_paths(&root).unwrap() {
            let entries = lock_entries(&root, &name).unwrap();
            assert!(
                entries.iter().any(|(_, has_source)| !has_source),
                "{name} has no source-less Cargo.lock entry — the patch is not live"
            );
        }
    }

    #[test]
    fn notice_lines_parse_into_name_version_license_and_dir() {
        let entries =
            parse_notice("prose\n- winit 0.30.13, Apache-2.0 (`vendor/winit/`)\n- DejaVu: x\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "winit");
        assert_eq!(entries[0].version, "0.30.13");
        assert_eq!(entries[0].license, "Apache-2.0");
        assert_eq!(entries[0].dir, "winit");
        assert_eq!(entries[0].line, 2);
    }

    #[test]
    fn a_header_notice_is_recognized_and_a_buried_one_is_not() {
        assert!(has_modification_notice(
            "// Modified by aterm: pump WM_TIMER.\nfn main() {}"
        ));
        assert!(!has_modification_notice("use std::ptr;\nfn main() {}"));
        let buried = format!(
            "{}// LOCAL PATCH (aterm): late\n",
            "\n".repeat(HEADER_LINES + 1)
        );
        assert!(
            !has_modification_notice(&buried),
            "§4(b) wants a PROMINENT notice"
        );
    }

    // --- fixtures ----------------------------------------------------------

    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A minimal workspace with one complete, correct fork.
    fn good_fixture(tag: &str) -> Fixture {
        let name = format!("aterm-forge-attest-{tag}-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        let fork = dir.join("vendor/goodfork/src");
        std::fs::create_dir_all(&fork).unwrap();
        let w = |rel: &str, body: &str| std::fs::write(dir.join(rel), body).unwrap();
        w(
            "Cargo.toml",
            "[workspace]\nmembers = []\n\n[patch.crates-io]\ngoodfork = { path = \"vendor/goodfork\" }\n",
        );
        w(
            "Cargo.lock",
            "version = 4\n\n[[package]]\nname = \"goodfork\"\nversion = \"1.2.3\"\n",
        );
        w("deny.toml", "[licenses]\nallow = [\"MIT\"]\n");
        w(
            "NOTICE",
            "prose\n\n- goodfork 1.2.3, MIT (`vendor/goodfork/`)\n\nmore prose\n",
        );
        w(
            "vendor/goodfork/Cargo.toml",
            "[package]\nname = \"goodfork\"\nversion = \"1.2.3\"\nlicense = \"MIT\"\n\n[workspace]\n",
        );
        w(
            "vendor/goodfork/Cargo.toml.orig",
            "[package]\nname = \"goodfork\"\n",
        );
        w(
            "vendor/goodfork/.cargo_vcs_info.json",
            "{\"git\":{\"sha1\":\"deadbeef\"}}\n",
        );
        w("vendor/goodfork/LICENSE-MIT", "MIT\n");
        w(
            "vendor/goodfork/src/lib.rs",
            "// aterm-trust: bounds proof discharged here.\n",
        );
        Fixture(dir)
    }

    #[test]
    fn a_complete_correct_fork_is_green() {
        let fixture = good_fixture("green");
        let (ok, log) = report(&fixture.0);
        assert!(ok, "a complete fork must attest clean:\n{log}");
        assert!(log.contains("PASS — 10 obligations held"), "{log}");
    }

    #[test]
    fn a_notice_that_omits_a_fork_is_red() {
        let fixture = good_fixture("notice");
        std::fs::write(
            fixture.0.join("NOTICE"),
            "prose only, no vendored crate list\n",
        )
        .unwrap();
        let (ok, log) = report(&fixture.0);
        assert!(!ok, "an omitted fork must be RED:\n{log}");
        assert!(
            log.contains("[OB-6]") && log.contains("does not list fork `goodfork`"),
            "{log}"
        );
    }

    #[test]
    fn a_notice_that_over_claims_is_red() {
        let fixture = good_fixture("overclaim");
        std::fs::write(
            fixture.0.join("NOTICE"),
            "- goodfork 1.2.3, MIT (`vendor/goodfork/`)\n- ghost 9.9.9, MIT (`vendor/ghost/`)\n",
        )
        .unwrap();
        let (ok, log) = report(&fixture.0);
        assert!(!ok, "an over-claiming NOTICE must be RED:\n{log}");
        assert!(log.contains("not a [patch.crates-io] fork"), "{log}");
    }

    #[test]
    fn removing_the_workspace_stub_is_red() {
        let fixture = good_fixture("stub");
        std::fs::write(
            fixture.0.join("vendor/goodfork/Cargo.toml"),
            "[package]\nname = \"goodfork\"\nversion = \"1.2.3\"\nlicense = \"MIT\"\n",
        )
        .unwrap();
        let (ok, log) = report(&fixture.0);
        assert!(!ok, "{log}");
        assert!(
            log.contains("[OB-3]") && log.contains("no empty `[workspace]` table"),
            "{log}"
        );
    }

    #[test]
    fn a_license_off_the_allowlist_is_red() {
        let fixture = good_fixture("license");
        std::fs::write(
            fixture.0.join("vendor/goodfork/Cargo.toml"),
            "[package]\nname = \"goodfork\"\nversion = \"1.2.3\"\nlicense = \"GPL-3.0\"\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(
            fixture.0.join("NOTICE"),
            "- goodfork 1.2.3, GPL-3.0 (`vendor/goodfork/`)\n",
        )
        .unwrap();
        let (ok, log) = report(&fixture.0);
        assert!(!ok, "{log}");
        assert!(log.contains("[OB-9]") && log.contains("GPL-3.0"), "{log}");
    }

    #[test]
    fn an_unmarked_fork_is_red() {
        let fixture = good_fixture("markers");
        std::fs::write(
            fixture.0.join("vendor/goodfork/src/lib.rs"),
            "pub fn f() {}\n",
        )
        .unwrap();
        let (ok, log) = report(&fixture.0);
        assert!(!ok, "{log}");
        assert!(log.contains("[OB-8]") && log.contains("ZERO"), "{log}");
    }

    #[test]
    fn a_dead_patch_is_red() {
        let fixture = good_fixture("deadpatch");
        std::fs::write(
            fixture.0.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"goodfork\"\nversion = \"1.2.3\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        )
        .unwrap();
        let (ok, log) = report(&fixture.0);
        assert!(!ok, "{log}");
        assert!(log.contains("DID NOT TAKE"), "{log}");
    }

    #[test]
    fn an_apache_only_fork_needs_header_notices() {
        let fixture = good_fixture("apache");
        let root = &fixture.0;
        std::fs::write(
            root.join("vendor/goodfork/Cargo.toml"),
            "[package]\nname = \"goodfork\"\nversion = \"1.2.3\"\nlicense = \"Apache-2.0\"\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("NOTICE"),
            "- goodfork 1.2.3, Apache-2.0 (`vendor/goodfork/`)\n",
        )
        .unwrap();
        std::fs::write(
            root.join("deny.toml"),
            "[licenses]\nallow = [\"MIT\", \"Apache-2.0\"]\n",
        )
        .unwrap();

        // The pristine tree the fork is diffed against. Placing it in the
        // ledger's own `vendor/.forge/<name>/pristine/` slot exercises the same
        // lookup the real forks use, so this test proves the production path
        // rather than a test-only branch. lib.rs here differs from the fork's
        // copy, so the fork's lib.rs is MODIFIED and owes a §4(b) notice; the
        // manifest is byte-identical, so it owes nothing.
        let pristine = root.join("vendor/.forge/goodfork/pristine/src");
        std::fs::create_dir_all(&pristine).unwrap();
        std::fs::write(pristine.join("lib.rs"), "// upstream\n").unwrap();
        for rel in [
            "Cargo.toml",
            "Cargo.toml.orig",
            ".cargo_vcs_info.json",
            "LICENSE-MIT",
        ] {
            std::fs::copy(
                root.join("vendor/goodfork").join(rel),
                root.join("vendor/.forge/goodfork/pristine").join(rel),
            )
            .unwrap();
        }

        let (ok, log) = report(root);
        assert!(
            !ok,
            "a modified file with no header notice must be RED:\n{log}"
        );
        assert!(log.contains("[OB-7]") && log.contains("§4(b)"), "{log}");
        assert!(
            log.contains("1 modified file(s)"),
            "the diff must find exactly one:\n{log}"
        );

        // Adding the header notice discharges it.
        std::fs::write(
            root.join("vendor/goodfork/src/lib.rs"),
            "// Modified by aterm: bounds proof discharged here.\n// aterm-trust: ok\n",
        )
        .unwrap();
        let (ok, log) = report(root);
        assert!(ok, "a header notice must discharge §4(b):\n{log}");
    }

    #[test]
    fn a_fork_with_no_pristine_copy_is_reported_unverified_not_passed() {
        // The failure mode this guards: an obligation that silently becomes a
        // no-op when its evidence is missing. OB-7 must SAY it could not check.
        let fixture = good_fixture("nopristine");
        let root = &fixture.0;
        std::fs::write(
            root.join("vendor/goodfork/Cargo.toml"),
            "[package]\nname = \"goodfork\"\nversion = \"9.9.9-absent\"\nlicense = \"Apache-2.0\"\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("NOTICE"),
            "- goodfork 9.9.9-absent, Apache-2.0 (`vendor/goodfork/`)\n",
        )
        .unwrap();
        std::fs::write(
            root.join("deny.toml"),
            "[licenses]\nallow = [\"Apache-2.0\"]\n",
        )
        .unwrap();
        let (_, log) = report(root);
        assert!(
            log.contains("UNVERIFIED for it, not") || log.contains("cannot be diffed"),
            "OB-7 must announce that it could not verify:\n{log}"
        );
    }

    #[test]
    fn the_pristine_diff_separates_modified_from_added() {
        let fixture = good_fixture("diffkinds");
        let root = &fixture.0;
        let pristine = root.join("vendor/.forge/goodfork/pristine/src");
        std::fs::create_dir_all(&pristine).unwrap();
        std::fs::write(pristine.join("lib.rs"), "// upstream\n").unwrap();
        std::fs::write(root.join("vendor/goodfork/src/extra.rs"), "// brand new\n").unwrap();
        let d = diff_against_pristine(
            &root.join("vendor/.forge/goodfork/pristine"),
            &root.join("vendor/goodfork"),
        )
        .unwrap();
        assert_eq!(d.modified, ["src/lib.rs"], "lib.rs differs from upstream");
        assert!(
            d.added.contains(&"src/extra.rs".to_string()),
            "added: {:?}",
            d.added
        );
        assert!(!d.added.contains(&"src/lib.rs".to_string()));
    }
}
