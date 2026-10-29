// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `attest`: the provenance and license notary over `vendor/`.

use aterm_uds::output::CommandOutput as _;
use aterm_uds::spawnfd::command;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use aterm_toml::edit::{DocumentMut, Item, TableLike};

use crate::pristine::Pristine;
use crate::{Outcome, PRECISION_NOTE};

/// The marker each Trust-discharged fix inside a fork carries.
pub const TRUST_MARKER: &str = "// aterm-trust:";
/// The marker each behavioural local patch inside a fork carries.
pub const LOCAL_PATCH_MARKER: &str = "// LOCAL PATCH (aterm):";
/// Leading lines of a file that count as its "header" for the Apache-2.0 §4(b) notice check.
const HEADER_LINES: usize = 20;

/// The `attest` verb.
pub fn run(root: &Path) -> Result<Outcome, String> {
    if !root.join("Cargo.toml").is_file() {
        return Err(format!(
            "no Cargo.toml at {} — run from inside the workspace or pass `--root <workspace root>`",
            root.display()
        ));
    }
    let attested = report(root);
    match attested.could_not_run {
        Some(why) => Err(format!(
            "{why}\n\nthe transcript up to that point:\n{}",
            attested.log
        )),
        None => Ok(Outcome {
            ok: attested.ok,
            log: attested.log,
        }),
    }
}

/// What [`report_with`] found.
pub struct Attestation {
    /// No obligation was found violated.
    pub ok: bool,
    /// The transcript, complete on its own.
    pub log: String,
    /// Why an obligation could not be judged here, if one could not.
    pub could_not_run: Option<String>,
    /// The Apache-only forks `[OB-7]` could not diff here, for want of a pristine copy.
    pub unverified: Vec<String>,
}

/// One vendored fork, as attest sees it on disk.
#[derive(Debug, Clone)]
struct VendoredFork {
    /// The crates-io package being replaced ([`crate::policy::patched_package`]).
    name: String,
    /// The patch row is renamed, so it claims one version line of `name`.
    renamed: bool,
    /// The replacement path exactly as the patch table spells it.
    rel: String,
    dir: PathBuf,
    /// `package.version` from the vendored manifest; empty when unreadable.
    version: String,
    /// `package.license` from the vendored manifest; empty when unreadable.
    license: String,
    /// `[workspace]` present and empty in the vendored manifest.
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
    const fn markers(&self) -> u64 {
        self.trust_markers + self.patch_markers
    }
}

/// Read the root `[patch.crates-io]` table as rows naming the package each replaces.
fn patch_paths(root: &Path) -> Result<PatchTable, String> {
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
        let name = crate::policy::patched_package(key, value);
        out.push(PatchRow {
            name: name.to_string(),
            rel: path,
            renamed: name != key,
        });
    }
    out.sort_by_key(PatchRow::pair);
    Ok(out)
}

/// One `[patch.crates-io]` row as attest reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PatchRow {
    /// The package replaced ([`crate::policy::patched_package`]).
    name: String,
    /// The replacement path as written; empty for a git/registry row.
    rel: String,
    /// Keyed apart from `name` (`package = "…"`).
    renamed: bool,
}

impl PatchRow {
    fn pair(&self) -> (String, String) {
        (self.name.clone(), self.rel.clone())
    }
}

/// The patch table's rows, sorted by package and path.
type PatchTable = Vec<PatchRow>;

/// The patch table split by replacement path.
struct Partition {
    vendored: PatchTable,
    first_party: PatchTable,
    absorbed: PatchTable,
}

/// Split the patch table by replacement path.
fn partition_patches(patch: &[PatchRow], root: &Path) -> Partition {
    use aterm_census::scan_set::PatchTargetKind;
    let mut out = Partition {
        vendored: Vec::new(),
        first_party: Vec::new(),
        absorbed: Vec::new(),
    };
    for row in patch {
        let entry = row.clone();
        match aterm_census::scan_set::classify_patch_target(&row.name, &row.rel, root) {
            Ok(PatchTargetKind::FirstParty) => out.first_party.push(entry),
            Ok(PatchTargetKind::Absorbed) => out.absorbed.push(entry),
            _ => out.vendored.push(entry),
        }
    }
    out
}

/// The absorbed crates renamed off their upstream name.
fn absorbed_members(root: &Path, patch: &[PatchRow]) -> PatchTable {
    let text = std::fs::read_to_string(root.join("NOTICE")).unwrap_or_default();
    let mut out: PatchTable = parse_notice(&text)
        .into_iter()
        .filter(|e| e.dir.starts_with("crates/") && !patch.iter().any(|row| row.rel == e.dir))
        .map(|e| PatchRow {
            name: e.name,
            rel: e.dir,
            renamed: false,
        })
        .collect();
    out.sort_by_key(PatchRow::pair);
    out
}

/// Directory names directly under `vendor/`, sorted.
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
fn survey_forks(root: &Path, patch: &[PatchRow]) -> Vec<VendoredFork> {
    let mut forks = Vec::new();
    for row in patch {
        let name = &row.name;
        let rel = if row.rel.is_empty() {
            format!("vendor/{name}")
        } else {
            row.rel.clone()
        };
        let dir = root.join(&rel);
        let mut fork = VendoredFork {
            name: name.clone(),
            renamed: row.renamed,
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
                if is_licence_text(&name) {
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

/// Count both markers over every `*.rs` under a fork, using the shared `aterm_census` walk.
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

/// Split an SPDX expression into its individual license identifiers.
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

fn spdx_tokens(expr: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = expr;
    while let Some(idx) = rest.find(|c: char| !c.is_whitespace()) {
        rest = &rest[idx..];
        if let Some(after) = rest.strip_prefix('(') {
            out.push("(");
            rest = after;
        } else if let Some(after) = rest.strip_prefix(')') {
            out.push(")");
            rest = after;
        } else {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '(' || c == ')')
                .unwrap_or(rest.len());
            out.push(&rest[..end]);
            rest = &rest[end..];
        }
    }
    out
}

fn eval_spdx_or(tokens: &[&str], pos: &mut usize) -> Option<bool> {
    let mut mandatory = eval_spdx_and(tokens, pos)?;
    while tokens.get(*pos) == Some(&"OR") {
        *pos += 1;
        let rhs = eval_spdx_and(tokens, pos)?;
        mandatory = mandatory && rhs;
    }
    Some(mandatory)
}

fn eval_spdx_and(tokens: &[&str], pos: &mut usize) -> Option<bool> {
    let mut mandatory = eval_spdx_atom(tokens, pos)?;
    while tokens.get(*pos) == Some(&"AND") {
        *pos += 1;
        let rhs = eval_spdx_atom(tokens, pos)?;
        mandatory = mandatory || rhs;
    }
    Some(mandatory)
}

fn eval_spdx_atom(tokens: &[&str], pos: &mut usize) -> Option<bool> {
    match *tokens.get(*pos)? {
        "(" => {
            *pos += 1;
            let inner = eval_spdx_or(tokens, pos)?;
            if tokens.get(*pos) != Some(&")") {
                return None;
            }
            *pos += 1;
            Some(inner)
        }
        ")" | "OR" | "AND" | "WITH" => None,
        id => {
            *pos += 1;
            if tokens.get(*pos) == Some(&"WITH") {
                *pos += 1;
                let exc = *tokens.get(*pos)?;
                if matches!(exc, "(" | ")" | "OR" | "AND" | "WITH") {
                    return None;
                }
                *pos += 1;
            }
            Some(id == "Apache-2.0")
        }
    }
}

/// Is the Apache-2.0 grant unavoidable for this expression?
pub(crate) fn apache_is_mandatory(expr: &str) -> bool {
    let tokens = spdx_tokens(expr);
    if tokens.is_empty() {
        return false;
    }
    let mut pos = 0;
    match eval_spdx_or(&tokens, &mut pos) {
        Some(mandatory) if pos == tokens.len() => mandatory,
        _ => license_arms(expr).iter().any(|l| l == "Apache-2.0"),
    }
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

/// A parsed dotted version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ver {
    major: u64,
    minor: u64,
    patch: u64,
    stated_minor: bool,
    stated_patch: bool,
}

impl Ver {
    pub(crate) const fn triple(self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }

    const fn wide(self) -> (u128, u128, u128) {
        (self.major as u128, self.minor as u128, self.patch as u128)
    }
}

/// Parse a dotted version, tolerant of omitted components and of a `-prerelease` / `+build` suffix.
pub(crate) fn parse_version(text: &str) -> Option<Ver> {
    let core = text.split(['-', '+']).next().unwrap_or(text);
    let mut parts = core.split('.');
    let major = parts.next()?.trim().parse().ok()?;
    let minor_text = parts.next();
    let patch_text = parts.next();
    if parts.next().is_some() {
        return None;
    }
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

/// Does `version` satisfy `req`?
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
    let upper = if r.major > 0 {
        (u128::from(r.major) + 1, 0, 0)
    } else if !r.stated_minor {
        (1, 0, 0)
    } else if r.minor > 0 {
        (0, u128::from(r.minor) + 1, 0)
    } else if r.stated_patch {
        (0, 0, u128::from(r.patch) + 1)
    } else {
        (0, 1, 0)
    };
    Some(v.wide() >= r.wide() && v.wide() < upper)
}

/// Every version requirement the workspace states on `name`, as `(manifest, requirement)`.
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
        if let Some(targets) = doc.get("target").and_then(Item::as_table_like) {
            for (_, gated) in targets.iter() {
                if let Some(gated) = gated.as_table_like() {
                    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                        if let Some(t) = gated.get(key).and_then(Item::as_table_like) {
                            tables.push(t);
                        }
                    }
                }
            }
        }
        for table in tables {
            for (key, entry) in table.iter() {
                let declared = entry
                    .as_table_like()
                    .and_then(|t| t.get("package"))
                    .and_then(Item::as_str)
                    .unwrap_or(key);
                if declared != name {
                    continue;
                }
                if let Some(req) = entry.as_str() {
                    out.push((rel_display(root, &manifest), req.to_string()));
                    continue;
                }
                let Some(entry) = entry.as_table_like() else {
                    continue;
                };
                if entry.get("path").is_some()
                    || entry.get("git").is_some()
                    || entry.get("workspace").is_some()
                {
                    continue;
                }
                if let Some(req) = entry.get("version").and_then(Item::as_str) {
                    out.push((rel_display(root, &manifest), req.to_string()));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Every `[[package]]` entry in `Cargo.lock` for `name`, as `(version, has_source)`.
pub(crate) fn lock_entries(root: &Path, name: &str) -> Result<Vec<(String, bool)>, String> {
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

/// One `` - <name> <version>, <SPDX> (`<dir>/`) `` line of `NOTICE`; `dir` is repo-relative,
/// under `vendor/` for a fork or `crates/` for an absorbed crate.
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
        let Some((head, tail)) = rest.split_once("(`") else {
            continue;
        };
        let Some(dir) = tail.trim().strip_suffix("/`)") else {
            continue;
        };
        if !(dir.starts_with("vendor/") || dir.starts_with("crates/")) {
            continue;
        }
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

/// One `git check-ignore -v --no-index` verdict.
#[derive(Debug, Clone)]
struct IgnoreVerdict {
    /// `<file>:<line>:<pattern>` exactly as git printed it.
    rule: String,
    /// `true` when the matching rule is a `!` re-include, i.e. the path is kept.
    reincluded: bool,
    path: String,
}

/// Ask git which of `paths` its ignore rules match.
fn check_ignore(root: &Path, paths: &[String]) -> Option<Vec<IgnoreVerdict>> {
    if paths.is_empty() {
        return Some(Vec::new());
    }
    let output = command(aterm_uds::os_path!("git"))
        .current_dir(root)
        .args(["check-ignore", "-v", "--no-index", "--"])
        .args(paths)
        .output_owned()
        .ok()?;
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

/// The gitlinks (mode 160000 entries) the index records under `vendor/`, repo-relative.
fn vendor_gitlinks(root: &Path) -> Option<BTreeSet<String>> {
    let output = command(aterm_uds::os_path!("git"))
        .current_dir(root)
        .args(["ls-files", "-s", "-z", "--", "vendor"])
        .output_owned()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(
        text.split('\0')
            .filter_map(|entry| {
                let (meta, path) = entry.split_once('\t')?;
                meta.starts_with("160000 ").then(|| path.to_string())
            })
            .collect(),
    )
}

/// Every repo-relative path under `vendor/` (files and dirs), except under a submodule in `skip`.
fn vendor_paths(root: &Path, dir: &Path, skip: &BTreeSet<String>, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let rel = rel_display(root, &path);
        let descend = path.is_dir() && !path.is_symlink() && !skip.contains(&rel);
        out.push(rel);
        if descend {
            vendor_paths(root, &path, skip, out);
        }
    }
}

/// Run every obligation ([`Attestation`]).
pub fn report(root: &Path) -> Attestation {
    report_with(&crate::resolve::Cargo::from_env(), root)
}

/// [`report`], asking `cargo` for the vendored submodule's metadata.
pub fn report_with(cargo: &crate::resolve::Cargo, root: &Path) -> Attestation {
    report_using(cargo, &Pristine::host(), root)
}

/// [`report_with`], `[OB-7]` taking its pristine copies from `pristine`.
fn report_using(cargo: &crate::resolve::Cargo, pristine: &Pristine, root: &Path) -> Attestation {
    let mut log = String::new();
    let mut fails = 0usize;
    let mut could_not_run = None;
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
            return Attestation {
                ok: false,
                log,
                could_not_run,
                unverified: Vec::new(),
            };
        }
    };
    let partition = partition_patches(&patch, root);
    let forks = survey_forks(root, &partition.vendored);
    let first_party_forks = survey_forks(root, &partition.first_party);
    let absorbed = survey_forks(root, &partition.absorbed);
    let members = survey_forks(root, &absorbed_members(root, &patch));
    let mut redistributed: Vec<VendoredFork> = forks
        .iter()
        .chain(&absorbed)
        .chain(&members)
        .cloned()
        .collect();
    redistributed.sort_by(|a, b| a.name.cmp(&b.name));
    let dirs = vendor_dirs(root);
    let _ = writeln!(
        log,
        "    forks: {} vendored from [patch.crates-io]; {} director(y/ies) under vendor/, of \
         which {} are FIRST-PARTY vendored path dependencies; {} FIRST-PARTY patch target(s) \
         under crates/ (aterm's own code — provenance obligations do not apply, and \
         [OB-1]/[OB-2] say so by name); {} ABSORBED patch target(s) and {} renamed ABSORBED \
         member(s) under crates/ (upstream source aterm owns — the license obligations \
         [OB-5]/[OB-6]/[OB-7]/[OB-9] follow it)",
        forks.len(),
        dirs.len(),
        crate::provenance::roster_len(root),
        first_party_forks.len(),
        absorbed.len(),
        members.len()
    );

    let (ob1_fails, ob1_refused) = ob1_patch_vendor_agreement(
        cargo,
        root,
        &forks,
        &first_party_forks,
        (&absorbed, &members),
        &dirs,
        &mut log,
    );
    fails += ob1_fails;
    could_not_run = ob1_refused;
    let mut ob2_all: Vec<VendoredFork> = Vec::new();
    ob2_all.extend(forks.iter().cloned());
    ob2_all.extend(absorbed.iter().cloned());
    ob2_all.extend(first_party_forks.iter().cloned());
    ob2_all.sort_by(|a, b| a.name.cmp(&b.name));
    fails += ob2_version_equality(root, &ob2_all, &mut log);
    fails += ob3_workspace_stub(&forks, &mut log);
    fails += ob4_provenance_files(&forks, &mut log);
    fails += ob5_license_files(&redistributed, &mut log);
    fails += ob6_notice_agreement(root, &redistributed, &mut log);
    let (ob7_fails, unverified) =
        ob7_apache_modification_notices(root, pristine, &redistributed, &mut log);
    fails += ob7_fails;
    fails += ob8_marker_census(&forks, &mut log);
    fails += ob9_spdx_allowlist(root, &redistributed, &mut log);
    fails += ob10_gitignore(root, &forks, &mut log);

    if fails == 0 {
        let over = format!(
            "over {} vendored fork(s) (+ [OB-1]/[OB-2]/[OB-5]/[OB-6]/[OB-7]/[OB-9] over {} \
             absorbed patch target(s), [OB-1]/[OB-5]/[OB-6]/[OB-7]/[OB-9] over {} renamed \
             absorbed member(s), [OB-1]/[OB-2] over {} first-party patch target(s))",
            forks.len(),
            absorbed.len(),
            members.len(),
            first_party_forks.len()
        );
        if unverified.is_empty() {
            let _ = writeln!(
                log,
                "cargo forge attest: PASS — 10 obligations held {over}."
            );
        } else {
            let _ = writeln!(
                log,
                "cargo forge attest: PASS, [OB-7] UNVERIFIED — nothing violated {over}, but \
                 [OB-7] diffed no file of {}: it got no pristine copy (its NOTE above says \
                 why).",
                unverified.join(", ")
            );
        }
        return Attestation {
            ok: could_not_run.is_none(),
            log,
            could_not_run,
            unverified,
        };
    }
    let _ = writeln!(
        log,
        "cargo forge attest: FAILED — {fails} obligation violation(s)."
    );
    log.push_str(PRECISION_NOTE);
    log.push('\n');
    Attestation {
        ok: false,
        log,
        could_not_run,
        unverified,
    }
}

/// `[OB-1]` Every vendored patch key resolves to a real `vendor/<dir>` with sources.
fn ob1_patch_vendor_agreement(
    cargo: &crate::resolve::Cargo,
    root: &Path,
    forks: &[VendoredFork],
    first_party: &[VendoredFork],
    (absorbed, members): (&[VendoredFork], &[VendoredFork]),
    dirs: &[String],
    log: &mut String,
) -> (usize, Option<String>) {
    let mut fails = 0;
    let mut could_not_run = None;
    let submodule = match crate::direct_vendor::review(cargo, root) {
        Ok(Some(pin)) => {
            let _ = writeln!(
                log,
                "    [OB-1] vendor/astream is the astream SUBMODULE, pinned by this tree's gitlink \
                 at {} ({}): .gitmodules declares it, the index tracks that one gitlink and \
                 nothing beneath it, the checkout is initialised at exactly that commit with no \
                 tracked modification, and cargo resolves every astream crate it uses ({}) \
                 from vendor/astream/crates/<name> \
                 as Apache-2.0 path packages outside this workspace (the root manifest excludes \
                 vendor/astream), with aterm-link's broker and cap edges pointing there. The \
                 gitlink IS the pin: forge compiles in no astream revision and no file digest, \
                 and a bump is reviewed in astream as the range it moves over. It does NOT \
                 decide provenance: what takes these crates out of forge's third-party totals \
                 is the reviewed roster aterm_forge::provenance::FIRST_PARTY_VENDORED, and this \
                 notary infers no crates.io patch and no general vendor-directory exemption.",
                pin.commit,
                pin.url,
                pin.crates.join(", ")
            );
            Some(pin)
        }
        Ok(None) => None,
        Err(why) if crate::direct_vendor::machine_refusal(&why) => {
            let _ = writeln!(
                log,
                "  ? COULD NOT RUN [OB-1] astream submodule (vendor/astream): {why}"
            );
            could_not_run = Some(format!("[OB-1] {why}"));
            None
        }
        Err(why) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] astream submodule (vendor/astream): {why}"
            );
            fails += 1;
            None
        }
    };
    for fp in first_party {
        if !fp.dir.is_dir() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] [patch.crates-io] `{}` points at first-party `{}`, which does \
                 not exist — cargo cannot resolve this workspace at all. Restore the crate or \
                 remove the patch entry from {}/Cargo.toml.",
                fp.name,
                fp.rel,
                root.display()
            );
            fails += 1;
            continue;
        }
        if !fp.dir.join("src").is_dir() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] first-party patch target `{}` has no `src/` — a replacement \
                 with no sources replaces nothing.",
                fp.rel
            );
            fails += 1;
            continue;
        }
        let _ = writeln!(
            log,
            "    [OB-1] `{}` → `{}` is a FIRST-PARTY patch target (code aterm wrote, under \
             crates/). It is not a redistribution, so [OB-3]..[OB-10] — the [workspace] \
             stub, .cargo_vcs_info.json/Cargo.toml.orig, a retained upstream LICENSE, a \
             NOTICE row, the Apache §4(b) pristine diff, the fork-marker census, the SPDX \
             allowlist and the vendor/ ignore sweep — do NOT apply to it. [OB-2] does, and \
             it is the one that matters: the patch must actually take.",
            fp.name, fp.rel
        );
    }
    for crate_ in absorbed {
        if !crate_.dir.join("src").is_dir() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] absorbed patch target `{}` has no `src/` — a replacement with \
                 no sources replaces nothing.",
                crate_.rel
            );
            fails += 1;
            continue;
        }
        let _ = writeln!(
            log,
            "    [OB-1] `{}` → `{}` is an ABSORBED patch target: upstream source aterm owns as \
             a workspace member (it keeps the upstream license file and NOTICE names it). It \
             owes the license obligations [OB-5]/[OB-6]/[OB-7]/[OB-9] wherever it lives, and \
             [OB-2]; the fork-upkeep ones ([OB-3]/[OB-4]/[OB-8]/[OB-10]) ended with its \
             vendoring.",
            crate_.name, crate_.rel
        );
    }
    for crate_ in members {
        if !crate_.dir.join("Cargo.toml").is_file() || !crate_.dir.join("src").is_dir() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] NOTICE lists `{}` at `{}/`, which no patch row targets and \
                 which is not a crate with sources there — the redistribution record names \
                 nothing this tree ships. Restore the crate or correct the NOTICE line.",
                crate_.name, crate_.rel
            );
            fails += 1;
            continue;
        }
        let _ = writeln!(
            log,
            "    [OB-1] `{}` is a renamed ABSORBED member: upstream `{}` source aterm owns \
             under its own package name, reached by path and in no patch row; NOTICE lists it \
             under the upstream name. It owes the license obligations \
             [OB-5]/[OB-6]/[OB-7]/[OB-9]; [OB-2] asks whether a patch took, which a member \
             is not.",
            crate_.rel, crate_.name
        );
    }
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
    let roster_applies = crate::provenance::applies_to(root);
    let gitlinks = vendor_gitlinks(root).unwrap_or_default();
    if !roster_applies {
        let _ = writeln!(
            log,
            "  • NOTE [OB-1] the FIRST_PARTY_VENDORED roster was NOT applied: that constant \
             describes the aterm workspace and this root is not it. Every vendor/ directory \
             here is held to patch ↔ vendor/ agreement alone."
        );
    }
    for row in crate::provenance::stale_rows(root) {
        if !roster_applies {
            break;
        }
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-1] aterm_forge::provenance::FIRST_PARTY_VENDORED registers `{}` but \
             that directory does not exist — the review is stale. Drop the row, or restore the \
             directory.",
            row.dir
        );
        fails += 1;
    }
    for dir in dirs {
        let rel = format!("vendor/{dir}");
        if forks.iter().any(|f| f.rel == rel) {
            continue;
        }
        if let Some(row) = crate::provenance::roster_row(&rel).filter(|_| roster_applies) {
            if let Some(fork) = forks.iter().find(|f| f.rel == rel) {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-1] `{rel}` is on the FIRST_PARTY_VENDORED roster AND is \
                     patched in as `{}` — a directory cannot be both aterm's own code and a \
                     redistributed fork. Drop one.",
                    fork.name
                );
                fails += 1;
                continue;
            }
            let dependants = crate::provenance::path_dependants(root, &rel);
            if dependants.is_empty() {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-1] `{rel}` is a FIRST-PARTY vendored path dependency on the \
                     roster, but NO workspace member under crates/ carries a `path = …` that \
                     resolves inside it — so it is exactly the dead weight this obligation \
                     refuses, first-party or not. Add the edge back, or delete the directory \
                     and its roster row."
                );
                fails += 1;
                continue;
            }
            let pin = submodule
                .as_ref()
                .filter(|_| dir == crate::direct_vendor::DIRECTORY)
                .map(|p| format!(" at {}", p.commit))
                .unwrap_or_default();
            let ob10 = if gitlinks.contains(&rel) {
                "[OB-10] does not descend into it: it is a git submodule, another repository's \
                 working tree, so nothing beneath it can be tracked here or lost to an ignore \
                 rule here — the submodule check above holds it to its pinned commit instead."
            } else {
                "[OB-10] DOES: unlike a patch target under crates/, this directory is under \
                 vendor/, and the ignore sweep walks every path there — a rule that swallows \
                 first-party source loses it just as silently."
            };
            let _ = writeln!(
                log,
                "    [OB-1] `{rel}` is a FIRST-PARTY VENDORED PATH DEPENDENCY (upstream {}{pin}), \
                 reached by path from: {}. It is not a redistribution, so [OB-3]..[OB-9] — \
                 the [workspace] stub, .cargo_vcs_info.json/Cargo.toml.orig, a retained \
                 upstream LICENSE, a NOTICE row, the Apache §4(b) pristine diff, the \
                 fork-marker census and the SPDX allowlist — do NOT apply to it, for the same \
                 reason they do not apply to crates/. {ob10} {}",
                row.upstream,
                dependants.join(", "),
                row.why
            );
            continue;
        }
        if submodule.is_some() && dir == crate::direct_vendor::DIRECTORY {
            continue;
        }
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-1] `{rel}` is not named by any [patch.crates-io] entry, is not \
             on aterm_forge::provenance::FIRST_PARTY_VENDORED, and did not pass the astream \
             submodule check — it has no reviewed source classification and is dead weight \
             that ships in the source distribution. Add the patch entry (a redistributed \
             fork), add the roster row (aterm's own code, vendored so a clean clone builds), or \
             delete the directory. A direct path dependency must not be given a fictitious \
             crates.io patch."
        );
        fails += 1;
    }

    if root.join("crates/aterm-census/src/scan_set.rs").is_file() {
        let ledger = aterm_census::scan_set::FORK_LEDGER;
        let registry = aterm_census::scan_set::reviewed_forks(root).unwrap_or_else(|e| {
            let _ = writeln!(log, "  ✗ FAIL [OB-1] the census review is unreadable: {e}");
            fails += 1;
            Vec::new()
        });
        for fork in forks {
            let Some(entry) = registry.iter().find(|r| r.package == fork.name) else {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-1] fork `{}` has no `[[fork]]` block in {ledger} — a vendored \
                     fork links into the process and must be reviewed and classified there \
                     before it can be attested.",
                    fork.name
                );
                fails += 1;
                continue;
            };
            if entry.path != fork.rel {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-1] fork `{}` is patched from `{}` but {ledger} reviews it at \
                     `{}` — make the two agree.",
                    fork.name, fork.rel, entry.path
                );
                fails += 1;
            }
        }
        for entry in &registry {
            if !forks.iter().any(|f| f.name == entry.package) {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-1] {ledger} reviews `{}` but no [patch.crates-io] entry \
                     names it — the review is stale; drop the block.",
                    entry.package
                );
                fails += 1;
            }
        }
        for entry in &registry {
            if let Some(row) = crate::provenance::roster_row(&entry.path).filter(|_| roster_applies)
            {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-1] {ledger} reviews `{}` at `{}`, which is inside the \
                     FIRST-PARTY vendored root `{}`. A review block is for third-party code; \
                     drop one of the two registrations.",
                    entry.package, entry.path, row.dir
                );
                fails += 1;
            }
        }
        let rows = aterm_census::scan_set::ABSORBED_CRATES;
        for crate_ in absorbed.iter().chain(members) {
            if !rows
                .iter()
                .any(|r| r.upstream == crate_.name && r.path == crate_.rel)
            {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-1] absorbed crate `{}` (`{}`) has no matching \
                     aterm_census::scan_set::ABSORBED_CRATES row — the lock-order census \
                     needs its identity namespace and platform slices.",
                    crate_.name, crate_.rel
                );
                fails += 1;
            }
        }
        for row in rows {
            if !absorbed
                .iter()
                .chain(members)
                .any(|c| c.name == row.upstream && c.rel == row.path)
            {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-1] ABSORBED_CRATES registers `{}` at `{}` but neither an \
                     absorbed [patch.crates-io] target nor a NOTICE-listed absorbed member \
                     names it — the row is stale.",
                    row.upstream, row.path
                );
                fails += 1;
            }
        }
        let _ = writeln!(
            log,
            "    [OB-1] cross-checked against the census review in vendor/forge.toml ({} \
             reviewed fork(s)), \
             ABSORBED_CRATES ({} absorbed) and aterm_forge::provenance::FIRST_PARTY_VENDORED \
             ({} first-party vendored root(s)).",
            registry.len(),
            rows.len(),
            crate::provenance::roster_len(root)
        );
    } else {
        let _ = writeln!(
            log,
            "  • NOTE [OB-1] the census-review cross-check was SKIPPED: the compiled-in \
             ABSORBED_CRATES rows describe the aterm workspace and this root has no \
             crates/aterm-census/src/scan_set.rs. Patch ↔ vendor/ agreement itself was still \
             checked."
        );
    }
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-1] patch ↔ vendor/ agreement holds in both directions ({} vendored \
             fork(s); {} first-party target(s) present with sources; {} first-party vendored \
             root(s), each reached by a workspace member's path dependency).",
            forks.len(),
            first_party.len(),
            crate::provenance::roster_len(root)
        );
    }
    (fails, could_not_run)
}

/// `[OB-2]` The patched crate's version must satisfy every requirement the workspace states.
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
            // A renamed row answers for its own version line only: a
            // requirement on another line is another row's, or the registry's.
            if !crate::policy::in_row_scope(fork.renamed, Some(&fork.version), &req) {
                continue;
            }
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
                     took. Run `targo generate-lockfile` (or `targo metadata --offline`) first."
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
                         compiling the registry copy. Re-run `targo metadata --offline` after \
                         making the vendored version satisfy the stated requirement.",
                        fork.name, fork.rel
                    );
                    fails += 1;
                } else if !patched.iter().any(|(v, _)| *v == fork.version) {
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-2] Cargo.lock's path entry for `{}` is version {} but \
                         `{}/Cargo.toml` says {} — refresh the lockfile with \
                         `targo metadata --offline`.",
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
                    .filter(|(v, has_source)| {
                        *has_source
                            && crate::policy::in_row_scope(fork.renamed, Some(&fork.version), v)
                    })
                    .map(|(v, _)| v.as_str())
                    .collect();
                if !registry.is_empty() {
                    let _ = writeln!(
                        log,
                        "  • NOTE [OB-2] `{}` also resolves UNFORKED from the registry at {} \
                         beside the fork at {}. The fork's fixes do not apply to those copies; \
                         `targo --unverified forge check` scores that patch-liveness gap per cell.",
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
            "  ✓ [OB-2] every patched crate's version satisfies the workspace requirements \
             and appears source-less in Cargo.lock (the patch is live): {}",
            forks
                .iter()
                .map(|f| format!("{} {}", f.name, f.version))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    fails
}

/// `[OB-3]` The empty `[workspace]` stub, without which the fork cannot be driven standalone.
fn ob3_workspace_stub(forks: &[VendoredFork], log: &mut String) -> usize {
    let mut fails = 0;
    for fork in forks {
        if fork.manifest_error.is_some() || fork.workspace_stub {
            continue;
        }
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-3] `{}/Cargo.toml` has no empty `[workspace]` table, so the crate \
             cannot be driven standalone. MEASURED on this tree: `targo metadata` with cwd \
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

/// `[OB-6]` `NOTICE` lists every fork and absorbed crate.
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
        let dir = fork.rel.as_str();
        let matches: Vec<&NoticeEntry> = entries.iter().filter(|e| e.name == fork.name).collect();
        let Some(entry) = matches.first() else {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-6] NOTICE does not list fork `{}` — the source distribution ships \
                 it without saying so. FIX: add \"- {} {}, {} (`{}/`)\" to the redistributed \
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
                "  ✗ FAIL [OB-6] NOTICE:{} points `{}` at `{}/` but the patch table \
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
            "  ✗ FAIL [OB-6] NOTICE:{} claims a redistributed crate `{}` (`{}/`) that is not a \
             [patch.crates-io] fork or absorbed crate — a NOTICE that over-claims is as wrong \
             as one that under-claims. Remove the line, or restore the crate.",
            entry.line, entry.name, entry.dir
        );
        fails += 1;
    }
    if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-6] NOTICE agrees with the forks and absorbed crates in both directions ({} \
             listed).",
            entries.len()
        );
    }
    fails
}

/// `[OB-7]` Apache-2.0 §4(b), each fork diffed against the copy crates.io published
/// ([`Pristine::copy`]).
fn ob7_apache_modification_notices(
    root: &Path,
    pristine: &Pristine,
    forks: &[VendoredFork],
    log: &mut String,
) -> (usize, Vec<String>) {
    let mut fails = 0;
    let mut scoped = Vec::new();
    let mut unverified = Vec::new();
    // OB-6 judges a NOTICE it cannot read; here an unread one names no licence text.
    let notice = std::fs::read_to_string(root.join("NOTICE")).unwrap_or_default();
    for fork in forks {
        if !apache_is_mandatory(&fork.license) {
            continue;
        }
        scoped.push(fork.name.clone());

        let held = match pristine.copy(&fork.name, &fork.version) {
            Ok(held) => held,
            Err(why) => {
                let _ = writeln!(
                    log,
                    "  • NOTE [OB-7] fork `{}` cannot be diffed: there is no pristine copy of `{} \
                     {}`: {why}. UNVERIFIED for it, not satisfied.",
                    fork.name, fork.name, fork.version
                );
                unverified.push(fork.name.clone());
                continue;
            }
        };
        let Some(Divergence { modified, added }) = diff_against_pristine(&held.dir, &fork.dir)
        else {
            let _ = writeln!(
                log,
                "  • NOTE [OB-7] fork `{}` could not be walked for a pristine diff; UNVERIFIED \
                 for it, not satisfied.",
                fork.name
            );
            unverified.push(fork.name.clone());
            continue;
        };
        let _ = writeln!(
            log,
            "    [OB-7] `{}` vs pristine `{} {}` ({}): {} modified file(s), {} added.",
            fork.rel,
            fork.name,
            fork.version,
            held.provenance(),
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
            // An added licence text is kept as its holder wrote it: its notice is the NOTICE
            // passage that names it by path, not a header in it.
            if kind == "added" && is_licence_text(rel) {
                if notice.contains(&format!("`{}/{rel}`", fork.rel)) {
                    continue;
                }
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-7] `{}/{rel}` is a licence text added relative to pristine `{} \
                     {}`, and NOTICE does not name it. A licence text stays as its holder wrote \
                     it, so its notice is NOTICE naming it by path; without that nothing says \
                     aterm added it, and NOTICE's assertion that \"modified and added files carry \
                     prominent notices\" is false. FIX: name `{}/{rel}` in NOTICE beside the code \
                     it governs.",
                    fork.rel, fork.name, fork.version, fork.rel
                );
                fails += 1;
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
         a BYTE DIFF against the pristine copy — the `.crate` crates.io published, fetched by \
         attest, held to the sha256 the crates.io index records and unpacked into its cache, \
         not aterm's own markers — so a file edited without leaving a marker is still caught. \
         An added licence text (`LICENSE*`, `LICENCE*`) stays as its holder wrote it, and its \
         notice is NOTICE naming it by path. A fork attest gets no pristine copy of (offline, \
         or a download the index does not vouch for, which is refused) is reported \
         UNVERIFIED, never passed.",
        if scoped.is_empty() {
            "none".to_string()
        } else {
            scoped.join(", ")
        }
    );
    if !unverified.is_empty() {
        let _ = writeln!(
            log,
            "  • UNVERIFIED [OB-7] no file of {} was diffed, so this run says nothing of its \
             §4(b) notices.",
            unverified.join(", ")
        );
    } else if fails == 0 {
        let _ = writeln!(
            log,
            "  ✓ [OB-7] every file diverging from pristine upstream in an Apache-only fork \
             carries a modification notice."
        );
    }
    (fails, unverified)
}

/// What a fork changed relative to the copy the registry published.
struct Divergence {
    /// Repo-relative-to-the-fork paths present in both trees with differing bytes.
    modified: Vec<String>,
    /// Paths present only in the fork.
    added: Vec<String>,
}

/// Byte-compare `fork` against `pristine` (skipping `.cargo-ok`, `.git` and `target`).
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

/// Is this fork-relative path a licence text: a file named `LICENSE*` or `LICENCE*`, as
/// `[OB-5]` counts one?
fn is_licence_text(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.starts_with("LICENSE") || name.starts_with("LICENCE")
}

/// Does this file's header claim aterm changed it?
fn has_modification_notice(text: &str) -> bool {
    text.lines().take(HEADER_LINES).any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("aterm")
            && (lower.contains("modif")
                || lower.contains("local patch")
                || lower.contains("change")
                || lower.contains("added by"))
    })
}

/// `[OB-8]` The marker census: what each fork exists for.
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
    let submodules = vendor_gitlinks(root).unwrap_or_default();
    let mut existing = Vec::new();
    vendor_paths(root, &root.join("vendor"), &submodules, &mut existing);
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
                 is kept for any vendored crate shipping a real `debug` source module.",
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
    for submodule in &submodules {
        let _ = writeln!(
            log,
            "    [OB-10] `{submodule}` is a git submodule (a gitlink in the index): the sweep \
             probes its path but does not descend into another repository's working tree — \
             [OB-1] holds that checkout to its pinned commit."
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pristine::tests::{Served, crate_file};

    /// [`report`] with `[OB-7]`'s pristine copies from `served`, cached in a folder of its own.
    fn report_served(root: &Path, served: Served) -> Attestation {
        let cache = aterm_tempfile::tempdir().unwrap();
        let pristine = Pristine::at(Some(cache.path().to_path_buf()), Box::new(served));
        report_using(&crate::resolve::Cargo::from_env(), &pristine, root)
    }

    /// The absorbed fixtures' upstream manifest.
    const ABSORBED_MANIFEST: &str =
        "[package]\nname = \"absorbed\"\nversion = \"2.0.0\"\nlicense = \"Apache-2.0\"\n";

    /// crates.io's `absorbed 2.0.0`, the upstream the absorbed fixtures diverge from.
    fn absorbed_upstream() -> Served {
        let archive = crate_file(
            "absorbed-2.0.0",
            &[
                ("Cargo.toml", ABSORBED_MANIFEST.as_bytes()),
                ("LICENSE", b"Apache\n"),
                ("src/lib.rs", b"pub fn upstream() {}\n"),
            ],
        );
        Served::honest("absorbed", "2.0.0", archive)
    }

    #[test]
    fn apache_is_only_mandatory_when_no_other_arm_exists() {
        assert!(apache_is_mandatory("Apache-2.0"));
        assert!(apache_is_mandatory("Apache-2.0 AND MIT"));
        assert!(apache_is_mandatory("Apache-2.0 AND (MIT OR BSD-3-Clause)"));
        assert!(apache_is_mandatory(
            "(Apache-2.0 AND MIT) OR (Apache-2.0 AND BSD-3-Clause)"
        ));
        assert!(apache_is_mandatory(
            "Apache-2.0 WITH LLVM-exception AND (MIT OR Zlib)"
        ));
        assert!(!apache_is_mandatory("Apache-2.0 OR MIT"));
        assert!(!apache_is_mandatory("MIT OR Apache-2.0"));
        assert!(!apache_is_mandatory("(Apache-2.0 OR MIT) AND BSD-3-Clause"));
        assert!(!apache_is_mandatory("MIT"));
        assert!(!apache_is_mandatory(""));
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
        assert_eq!(req_satisfied(">=1, <3", "2.0.0"), None);
        assert!(parse_version("1.2.3.4").is_none());
        assert_eq!(req_satisfied("1.2.3.4", "1.2.3"), None);
        assert_eq!(req_satisfied("1.2.3", "1.2.3.4"), None);
        let max = u64::MAX;
        assert_eq!(
            req_satisfied(&format!("^{max}.0.0"), &format!("{max}.{max}.{max}")),
            Some(true)
        );
        assert_eq!(
            req_satisfied(&format!("^0.{max}.0"), &format!("0.{max}.{max}")),
            Some(true)
        );
        assert_eq!(
            req_satisfied(&format!("^0.0.{max}"), &format!("0.0.{max}")),
            Some(true)
        );
    }

    #[test]
    fn notice_lines_parse_into_name_version_license_and_dir() {
        let entries = parse_notice(
            "prose\n- libm 0.2.16, MIT (`vendor/libm/`)\n- DejaVu: x\n\
             - winit 0.30.13, Apache-2.0 (`crates/aterm-window/`)\n- elsewhere 1.0, MIT (`x/y/`)\n",
        );
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!(entries[0].name, "libm");
        assert_eq!(entries[0].version, "0.2.16");
        assert_eq!(entries[0].license, "MIT");
        assert_eq!(entries[0].dir, "vendor/libm");
        assert_eq!(entries[0].line, 2);
        assert_eq!(entries[1].name, "winit");
        assert_eq!(entries[1].dir, "crates/aterm-window");
        assert_eq!(entries[1].line, 4);
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

    struct Fixture(aterm_tempfile::TempDir);

    fn good_fixture(tag: &str) -> Fixture {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-forge-attest-{tag}-"))
            .tempdir()
            .unwrap();
        let fork = dir.path().join("vendor/goodfork/src");
        std::fs::create_dir_all(&fork).unwrap();
        let w = |rel: &str, body: &str| std::fs::write(dir.path().join(rel), body).unwrap();
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
    fn every_planted_fork_defect_reds_its_own_obligation() {
        let Attestation { ok, log, .. } = report(good_fixture("green").0.path());
        assert!(
            ok,
            "the baseline must attest clean or the plants prove nothing:\n{log}"
        );
        let cases: [(&str, &str, &str, &str); 5] = [
            (
                "a NOTICE that over-claims",
                "NOTICE",
                "- goodfork 1.2.3, MIT (`vendor/goodfork/`)\n- ghost 9.9.9, MIT (`vendor/ghost/`)\n",
                "FAIL [OB-6]",
            ),
            (
                "no workspace stub",
                "vendor/goodfork/Cargo.toml",
                "[package]\nname = \"goodfork\"\nversion = \"1.2.3\"\nlicense = \"MIT\"\n",
                "FAIL [OB-3]",
            ),
            (
                "a license off the allowlist",
                "vendor/goodfork/Cargo.toml",
                "[package]\nname = \"goodfork\"\nversion = \"1.2.3\"\nlicense = \"GPL-3.0\"\n\n[workspace]\n",
                "FAIL [OB-9]",
            ),
            (
                "a fork with no marker",
                "vendor/goodfork/src/lib.rs",
                "pub fn f() {}\n",
                "FAIL [OB-8]",
            ),
            (
                "a patch the lock resolves from the registry",
                "Cargo.lock",
                "version = 4\n\n[[package]]\nname = \"goodfork\"\nversion = \"1.2.3\"\n\
                 source = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
                "FAIL [OB-2]",
            ),
        ];
        for (name, rel, body, tag) in cases {
            let fixture = good_fixture(&name.replace(' ', "-"));
            std::fs::write(fixture.0.path().join(rel), body).unwrap();
            let Attestation { ok, log, .. } = report(fixture.0.path());
            assert!(!ok && log.contains(tag), "{name} must fail {tag}:\n{log}");
        }
    }

    #[test]
    fn renamed_forks_still_refuse_unmodelled_workspace_requirements() {
        let fixture = good_fixture("renamed-requirements");
        let root = fixture.0.path();
        let require = |req: &str| {
            std::fs::write(
                root.join("Cargo.toml"),
                format!(
                    "[workspace]\nmembers = []\n\n[workspace.dependencies]\ngoodfork = \
                     \"{req}\"\n\n[patch.crates-io]\ngoodfork-v1 = {{ package = \
                     \"goodfork\", path = \"vendor/goodfork\" }}\n"
                ),
            )
            .unwrap();
        };
        require("2");
        let Attestation { ok, log, .. } = report(root);
        assert!(ok, "a valid other-major requirement stays excluded:\n{log}");

        let mut accepted = Vec::new();
        for req in ["*", ">=0, <3", ">2", "0.*", "~2"] {
            require(req);
            let Attestation { ok, log, .. } = report(root);
            if ok || !log.contains("requirement form attest does not model") {
                accepted.push((req, log));
            }
        }
        assert!(
            accepted.is_empty(),
            "renaming a row must not bypass the requirement reader:\n{accepted:#?}"
        );

        // Wide zero-major requirements (`0`, `0.0`) span multiple semver lines and stay in scope,
        // and renamed or target-gated dependencies (`alias = { package = "goodfork", ... }`) are read too.
        for req in ["0", "0.0"] {
            require(req);
            let Attestation { ok, log, .. } = report(root);
            assert!(
                !ok && log.contains("FAIL [OB-2]"),
                "`{req}` must not be dropped out of scope:\n{log}"
            );
        }
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/consumer\"]\n\n[patch.crates-io]\ngoodfork = { path = \"vendor/goodfork\" }\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("crates/consumer")).unwrap();
        std::fs::write(
            root.join("crates/consumer/Cargo.toml"),
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n[target.'cfg(unix)'.dependencies]\nalias = { package = \"goodfork\", version = \"2.0\" }\n",
        )
        .unwrap();
        let Attestation { ok, log, .. } = report(root);
        assert!(
            !ok && log.contains("FAIL [OB-2]"),
            "renamed target-gated dependency must be checked by OB-2:\n{log}"
        );
    }

    fn absorbed_fixture(tag: &str, renamed: bool) -> Fixture {
        let fixture = good_fixture(tag);
        let root = fixture.0.path().to_path_buf();
        let w = |rel: &str, body: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        };
        let append = |rel: &str, extra: &str| {
            let path = root.join(rel);
            let mut body = std::fs::read_to_string(&path).unwrap();
            body.push_str(extra);
            std::fs::write(path, body).unwrap();
        };
        if !renamed {
            append(
                "Cargo.toml",
                "absorbed = { path = \"crates/aterm-absorbed\" }\n",
            );
            append(
                "Cargo.lock",
                "\n[[package]]\nname = \"absorbed\"\nversion = \"2.0.0\"\n",
            );
        }
        append(
            "NOTICE",
            "- absorbed 2.0.0, Apache-2.0 (`crates/aterm-absorbed/`)\n",
        );
        w(
            "deny.toml",
            "[licenses]\nallow = [\"MIT\", \"Apache-2.0\"]\n",
        );
        if renamed {
            w(
                "crates/aterm-absorbed/Cargo.toml",
                "# Modified by the aterm project: renamed.\n[package]\nname = \"aterm-absorbed\"\n\
                 version = \"2.0.0\"\nlicense = \"Apache-2.0\"\n",
            );
        } else {
            w("crates/aterm-absorbed/Cargo.toml", ABSORBED_MANIFEST);
        }
        w("crates/aterm-absorbed/LICENSE", "Apache\n");
        w(
            "crates/aterm-absorbed/src/lib.rs",
            "// Derived from absorbed 2.0.0 src/lib.rs, modified by the aterm project.\n\
             pub fn upstream() {}\n",
        );
        w(
            "crates/aterm-absorbed/src/added.rs",
            "// Added by the aterm project.\npub fn ours() {}\n",
        );
        fixture
    }

    #[test]
    fn an_absorbed_crate_owes_the_license_obligations() {
        let fixture = absorbed_fixture("absorbed-green", false);
        let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
        assert!(ok, "a complete absorbed crate must attest clean:\n{log}");

        let fixture = absorbed_fixture("absorbed-no-notice", false);
        std::fs::write(
            fixture.0.path().join("crates/aterm-absorbed/src/lib.rs"),
            "pub fn upstream() { /* edited */ }\n",
        )
        .unwrap();
        let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
        assert!(!ok && log.contains("FAIL [OB-7]"), "{log}");

        let fixture = absorbed_fixture("absorbed-no-notice-line", false);
        let notice = fixture.0.path().join("NOTICE");
        let text = std::fs::read_to_string(&notice).unwrap().replace(
            "- absorbed 2.0.0, Apache-2.0 (`crates/aterm-absorbed/`)\n",
            "",
        );
        std::fs::write(&notice, text).unwrap();
        let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
        assert!(!ok && log.contains("FAIL [OB-1]"), "{log}");
    }

    #[test]
    fn a_renamed_absorbed_member_owes_the_license_obligations() {
        let fixture = absorbed_fixture("member-green", true);
        let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
        assert!(
            ok,
            "a complete renamed absorbed member must attest clean:\n{log}"
        );
        assert!(
            log.contains("renamed ABSORBED member")
                && log.contains("[OB-7] `crates/aterm-absorbed`"),
            "the member must be attested, not skipped:\n{log}"
        );

        let fixture = absorbed_fixture("member-no-notice", true);
        std::fs::write(
            fixture.0.path().join("crates/aterm-absorbed/src/lib.rs"),
            "pub fn upstream() { /* edited */ }\n",
        )
        .unwrap();
        let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
        assert!(!ok && log.contains("FAIL [OB-7]"), "{log}");

        let fixture = absorbed_fixture("member-no-license", true);
        std::fs::remove_file(fixture.0.path().join("crates/aterm-absorbed/LICENSE")).unwrap();
        let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
        assert!(!ok && log.contains("FAIL [OB-5]"), "{log}");

        let fixture = absorbed_fixture("member-gone", true);
        std::fs::remove_dir_all(fixture.0.path().join("crates/aterm-absorbed")).unwrap();
        let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
        assert!(!ok && log.contains("FAIL [OB-1]"), "{log}");
    }

    /// An added licence text stays as its holder wrote it, so NOTICE naming it by path is its
    /// notice. Unnamed, it fails; and the naming discharges neither an added file of another
    /// kind nor an edited upstream licence.
    #[test]
    fn an_added_licence_text_takes_its_notice_from_notice() {
        let plant = |tag: &str, rel: &str, notice: &str| {
            let fixture = absorbed_fixture(tag, true);
            let root = fixture.0.path();
            std::fs::write(root.join(rel), "MIT License\n\nCopyright (c) 2022 Other\n").unwrap();
            let path = root.join("NOTICE");
            let mut text = std::fs::read_to_string(&path).unwrap();
            text.push_str(notice);
            std::fs::write(path, text).unwrap();
            report_served(root, absorbed_upstream())
        };
        let licence = "crates/aterm-absorbed/LICENSE-MIT-other";
        let named = |rel: &str| format!("Its shadow is other's, MIT: see `{rel}`.\n");

        let Attestation { ok, log, .. } = plant("licence-named", licence, &named(licence));
        assert!(ok, "a licence text NOTICE names needs no header:\n{log}");

        let Attestation { ok, log, .. } = plant("licence-unnamed", licence, "");
        assert!(
            !ok && log.contains(&format!("FAIL [OB-7] `{licence}` is a licence text added")),
            "{log}"
        );

        let data = "crates/aterm-absorbed/other.tsv";
        let Attestation { ok, log, .. } = plant("licence-not-data", data, &named(data));
        assert!(
            !ok && log.contains(&format!("FAIL [OB-7] `{data}` is added")),
            "{log}"
        );

        let upstream = "crates/aterm-absorbed/LICENSE";
        let Attestation { ok, log, .. } = plant("licence-edited", upstream, &named(upstream));
        assert!(
            !ok && log.contains(&format!("FAIL [OB-7] `{upstream}` is modified")),
            "{log}"
        );
    }

    /// Attest fetches the copy it diffs. Offline, or handed a download the index does not vouch
    /// for, it has none: the member is UNVERIFIED, unticked and not refused, and the NOTE says
    /// why.
    #[test]
    fn a_member_without_a_pristine_copy_is_unverified_never_ticked() {
        let fixture = absorbed_fixture("member-pristine", true);
        let Attestation {
            ok,
            log,
            unverified,
            ..
        } = report_served(fixture.0.path(), absorbed_upstream());
        assert!(ok && unverified.is_empty(), "{log}");
        assert!(
            log.contains("✓ [OB-7]")
                && log.contains("`absorbed-2.0.0.crate`, fetched from crates.io by this run"),
            "a diffed member earns the tick:\n{log}"
        );

        let mut wrong = absorbed_upstream();
        wrong.sha256 = "ab".repeat(32);
        for (case, served, why) in [
            (
                "offline",
                Served::offline(),
                "GET https://index.crates.io/ab/so/absorbed",
            ),
            (
                "wrong sha",
                wrong,
                "the download is refused and nothing was unpacked",
            ),
        ] {
            let fixture = absorbed_fixture(&format!("member-{}", case.replace(' ', "-")), true);
            std::fs::write(
                fixture.0.path().join("crates/aterm-absorbed/src/lib.rs"),
                "pub fn upstream() { /* edited */ }\n",
            )
            .unwrap();
            let Attestation {
                ok,
                log,
                unverified,
                ..
            } = report_served(fixture.0.path(), served);
            assert_eq!(unverified, ["absorbed"], "{case}: {log}");
            assert!(
                !log.contains("✓ [OB-7]")
                    && log.contains("UNVERIFIED [OB-7] no file of absorbed")
                    && log.contains(why),
                "{case}: an undiffed member must not be ticked, and the NOTE says why:\n{log}"
            );
            assert!(
                ok && log.contains("cargo forge attest: PASS, [OB-7] UNVERIFIED"),
                "{case}: the verdict names what was not judged, and finds nothing wrong:\n{log}"
            );
        }
    }

    #[test]
    fn an_unreviewed_vendor_directory_still_fails_the_astream_submodule_check() {
        let fixture = good_fixture("unreviewed-bundle");
        let root = fixture.0.path();
        std::fs::create_dir_all(root.join("vendor/another-bus/src")).unwrap();
        let Attestation { ok, log, .. } = report(root);
        assert!(!ok && log.contains("vendor/another-bus"), "{log}");
        std::fs::remove_dir_all(root.join("vendor/another-bus")).unwrap();
        git(root, &["init", "-q"]);
        std::fs::create_dir_all(root.join("vendor/astream/src")).unwrap();
        let Attestation { ok, log, .. } = report(root);
        assert!(!ok && log.contains("FAIL [OB-1]"), "{log}");
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = command(aterm_uds::os_path!("git"))
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args([
                "-c",
                "user.name=aterm-forge test",
                "-c",
                "user.email=forge@invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output_owned()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn the_ignore_sweep_does_not_descend_into_a_submodule_working_tree() {
        let fixture = good_fixture("ob10-submodule");
        let root = fixture.0.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join(".gitignore"), "target\n").unwrap();
        let sub = root.join("vendor/sub");
        std::fs::create_dir_all(&sub).unwrap();
        git(&sub, &["init", "-q"]);
        std::fs::write(
            sub.join("lib.rs"),
            "// SPDX-License-Identifier: Apache-2.0\n",
        )
        .unwrap();
        git(&sub, &["add", "lib.rs"]);
        git(&sub, &["commit", "-q", "-m", "one"]);
        let head = git(&sub, &["rev-parse", "HEAD"]);
        git(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{head},vendor/sub"),
            ],
        );
        assert_eq!(
            vendor_gitlinks(root),
            Some(BTreeSet::from(["vendor/sub".to_string()]))
        );
        std::fs::create_dir_all(sub.join("target/debug")).unwrap();
        std::fs::write(sub.join("target/debug/out"), "").unwrap();
        let mut log = String::new();
        assert_eq!(ob10_gitignore(root, &[], &mut log), 0, "{log}");
        std::fs::create_dir_all(root.join("vendor/goodfork/target")).unwrap();
        std::fs::write(root.join("vendor/goodfork/target/out"), "").unwrap();
        let mut log = String::new();
        assert_eq!(ob10_gitignore(root, &[], &mut log), 1, "{log}");
    }

    #[test]
    fn an_apache_only_fork_needs_header_notices() {
        let fixture = good_fixture("apache");
        let root = fixture.0.path();
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
        let mut upstream: Vec<(&str, Vec<u8>)> = [
            "Cargo.toml",
            "Cargo.toml.orig",
            ".cargo_vcs_info.json",
            "LICENSE-MIT",
        ]
        .into_iter()
        .map(|rel| {
            (
                rel,
                std::fs::read(root.join("vendor/goodfork").join(rel)).unwrap(),
            )
        })
        .collect();
        upstream.push(("src/lib.rs", b"// upstream\n".to_vec()));
        let files: Vec<(&str, &[u8])> = upstream.iter().map(|(r, b)| (*r, b.as_slice())).collect();
        let served = || Served::honest("goodfork", "1.2.3", crate_file("goodfork-1.2.3", &files));
        let Attestation { ok, log, .. } = report_served(root, served());
        assert!(!ok && log.contains("FAIL [OB-7]"), "{log}");

        std::fs::write(
            root.join("vendor/goodfork/src/lib.rs"),
            "// Modified by aterm: bounds proof discharged here.\n// aterm-trust: ok\n",
        )
        .unwrap();
        let Attestation { ok, log, .. } = report_served(root, served());
        assert!(ok, "a header notice must discharge §4(b):\n{log}");
    }
}
