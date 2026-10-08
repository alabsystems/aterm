// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `vendor/forge.toml`: the fork ledger, and the `[patch.crates-io]` facts the
//! rest of forge measures against.
//!
//! # Why this file exists
//!
//! Upstream crates live under `vendor/`, redirected by `[patch.crates-io]`,
//! because Trust's verification refused latent panics/overflows in them. aterm
//! maintains that code, under several licenses, with obligations (Apache-2.0
//! §4(b) notices, retained LICENSE files, upstream provenance) no compiler
//! enforces.
//!
//! The ledger has one `[[fork]]` block per vendored patch entry plus a
//! `[forge]` header pinning the measurement methods. `targo --unverified forge
//! check` holds it to the tree (`[OB-17]`): it must parse, its header must name
//! the measured cell matrix, and its fork blocks must be exactly the vendored
//! patches, with path, version and license as the tree has them,
//! `apache_notice` as `[OB-7]` reads that license, and the census class as
//! `aterm_census::scan_set::REVIEWED_VENDORED_CRATES` records it.
//!
//! # The comments are the record
//!
//! A fork's reason to exist is the comment block above it, not a key. The file
//! is edited by hand and read with `aterm-toml`'s document model.
//!
//! # Fail-closed
//!
//! An unknown key is an error naming the key, so a typo cannot read as
//! "obligation not claimed". A missing file is not a parse error: [`load`]
//! returns an empty [`Policy`], and `[OB-17]` fails only once a vendored fork
//! is patched in.
//!
//! # Version drift silently un-uses a patch
//!
//! A patch takes effect only while the vendored manifest's version satisfies
//! the graph's requirement; otherwise the registry copy resolves again with no
//! error. [`patch_entries`] reads the lock and reports, per entry, the version
//! resolved for the path package and any coexisting registry copies.

use crate::model::Cell;
use aterm_census::scan_set::{PatchTargetKind, classify_patch_target};
use aterm_toml::edit::{DocumentMut, Item, TableLike};
use std::path::Path;

/// The ledger's path, relative to the workspace root.
pub const POLICY_PATH: &str = "vendor/forge.toml";

/// The one LOC method forge implements: physical lines over every `*.rs` under
/// the package root, including its tests and examples. A ledger naming another
/// method is refused.
pub const LOC_METHOD: &str = "rs-physical-all-files-v1";

/// The one graph method forge implements: `cargo tree -p <package> -e normal
/// --target <triple> --prefix depth --no-dedupe --locked --offline`, parsed by
/// depth prefix. Not `cargo metadata --filter-platform`, whose resolve is
/// feature-unified across workspace members and over-counts.
pub const GRAPH_METHOD: &str = "cargo-tree-normal-no-dedupe-locked-offline-v1";

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// How `aterm-census`'s lock-order census classifies a vendored fork: the
/// ledger's spelling of [`aterm_census::scan_set::VendoredMode`], without its
/// payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CensusMode {
    /// Links into the shipped GUI process: scanned, with every lock identity in
    /// a per-crate namespace so foreign names never merge with aterm's.
    Scanned,
    /// Runs only inside a build script (a host tool), so it never links into a
    /// shipped process and is classified out with a written justification.
    BuildDepOnly,
}

impl CensusMode {
    fn parse(s: &str, ctx: &str) -> Result<Self, String> {
        match s {
            "scanned" => Ok(Self::Scanned),
            "build-dep-only" => Ok(Self::BuildDepOnly),
            other => Err(format!(
                "{POLICY_PATH}: {ctx} census.mode = \"{other}\" is not a mode — write \
                 \"scanned\" (the crate links into the shipped process; then census.namespace \
                 is required) or \"build-dep-only\" (it only runs inside a build script)"
            )),
        }
    }
}

/// One vendored, `[patch.crates-io]`-redirected upstream crate that aterm now
/// maintains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fork {
    /// The crates.io package this fork replaces (== the patch table key).
    pub name: String,
    /// The upstream version the fork keeps, so the existing `^` requirements
    /// still resolve. Held to the vendored manifest by `targo --unverified forge check`'s `[OB-17]`.
    pub version: String,
    /// Repo-relative directory (`vendor/winit`).
    pub path: String,
    /// The SPDX expression from the vendored manifest, verbatim.
    pub license: String,
    /// `true` when this fork's license leaves no non-Apache option, so §4(b)
    /// modification notices bind every file aterm modified. Must agree with
    /// `[OB-7]`'s verdict (`targo --unverified forge check` `[OB-17]`).
    pub apache_notice: bool,
    /// The lock-order census's classification.
    pub census_mode: CensusMode,
    /// The identity namespace (`winit` ⇒ `winit::…`). Required for
    /// [`CensusMode::Scanned`], refused for [`CensusMode::BuildDepOnly`].
    pub census_namespace: Option<String>,
}

/// The `[forge]` header: the methods and the cell matrix every number in a
/// report was produced by.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForgeHeader {
    pub loc_method: String,
    pub graph_method: String,
    pub cells: Vec<Cell>,
}

/// The parsed ledger.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub forge: ForgeHeader,
    pub forks: Vec<Fork>,
}

// ---------------------------------------------------------------------------
// Reading the ledger
// ---------------------------------------------------------------------------

/// Read `<root>/vendor/forge.toml`. An ABSENT file is an empty [`Policy`], not
/// an error: whether absence is a finding is `[OB-17]`'s call.
pub fn load(root: &Path) -> Result<Policy, String> {
    let path = root.join(POLICY_PATH);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Policy::default()),
        Err(e) => Err(format!(
            "cannot read {} ({e}) — fix the permissions, or delete the file to fall back \
             to the empty ledger",
            path.display()
        )),
    }
}

/// Parse a ledger body. Split out from [`load`] so the fail-closed key rules
/// are testable without a filesystem.
pub fn parse(text: &str) -> Result<Policy, String> {
    let doc: DocumentMut = text.parse().map_err(|e| {
        format!("{POLICY_PATH} is not valid TOML: {e} — fix the syntax; forge will not guess")
    })?;

    // `[[carved]]` rows are the carve ledger, read by `targo --unverified forge check`'s
    // `[OB-13]`; this reader only admits the key.
    for (key, _) in doc.as_table().iter() {
        if key != "forge" && key != "fork" && key != "carved" {
            return Err(format!(
                "{POLICY_PATH}: unknown top-level table `{key}` — this file holds exactly \
                 `[forge]` (the method header), `[[fork]]` blocks and `[[carved]]` rows. \
                 Delete it, or move the note into a `#` comment: keys are not free"
            ));
        }
    }

    let forge_item = doc.get("forge").ok_or_else(|| {
        format!(
            "{POLICY_PATH}: no `[forge]` header — a ledger that does not say how it measured \
             cannot be re-derived. Add:\n\n[forge]\nloc_method = \"{LOC_METHOD}\"\n\
             graph_method = \"{GRAPH_METHOD}\"\ncells = [{{ name = \"mac-arm\", \
             triple = \"aarch64-apple-darwin\", package = \"aterm\" }}]"
        )
    })?;
    let forge = header(forge_item)?;

    let mut forks = Vec::new();
    if let Some(item) = doc.get("fork") {
        let arr = item.as_array_of_tables().ok_or_else(|| {
            format!(
                "{POLICY_PATH}: `fork` must be a sequence of `[[fork]]` blocks — write each \
                 fork as its own `[[fork]]` table so its reason can live in the comment \
                 above it"
            )
        })?;
        for (n, t) in arr.iter().enumerate() {
            let fork = fork_block(t, n)?;
            if forks.iter().any(|f: &Fork| f.name == fork.name) {
                return Err(format!(
                    "{POLICY_PATH}: two `[[fork]]` blocks both name `{}` — one block per \
                     `[patch.crates-io]` entry; merge them",
                    fork.name
                ));
            }
            forks.push(fork);
        }
    }

    Ok(Policy { forge, forks })
}

fn header(item: &Item) -> Result<ForgeHeader, String> {
    let t = item.as_table_like().ok_or_else(|| {
        format!("{POLICY_PATH}: `forge` must be a table — write it as a `[forge]` block")
    })?;
    check_keys(t, &["loc_method", "graph_method", "cells"], "[forge]")?;

    let loc_method = req_str(t, "loc_method", "[forge]")?;
    if loc_method != LOC_METHOD {
        return Err(format!(
            "{POLICY_PATH}: [forge] loc_method = \"{loc_method}\" but this build of forge \
             measures \"{LOC_METHOD}\" — the numbers in the ledger were produced by a method \
             forge no longer implements. Re-measure with `targo --unverified forge survey` and set \
             loc_method = \"{LOC_METHOD}\", or check out the forge that wrote this file"
        ));
    }
    let graph_method = req_str(t, "graph_method", "[forge]")?;
    if graph_method != GRAPH_METHOD {
        return Err(format!(
            "{POLICY_PATH}: [forge] graph_method = \"{graph_method}\" but this build of forge \
             resolves with \"{GRAPH_METHOD}\" — set graph_method = \"{GRAPH_METHOD}\" only \
             after re-measuring; the counts differ by target, not by taste"
        ));
    }

    let cells_item = t.get("cells").ok_or_else(|| {
        format!(
            "{POLICY_PATH}: [forge] has no `cells` — list the measurement matrix, e.g.\n\
             cells = [{{ name = \"mac-arm\", triple = \"aarch64-apple-darwin\", \
             package = \"aterm\" }}]"
        )
    })?;
    let arr = cells_item
        .as_array()
        .ok_or_else(|| format!("{POLICY_PATH}: [forge] cells must be an array of inline tables"))?;
    let mut cells = Vec::new();
    for (n, v) in arr.iter().enumerate() {
        let ct = v.as_inline_table().ok_or_else(|| {
            format!(
                "{POLICY_PATH}: [forge] cells[{n}] is not a table — each cell is \
                 {{ name = \"…\", triple = \"…\", package = \"…\" }}"
            )
        })?;
        let ctx = format!("[forge] cells[{n}]");
        check_keys(ct, &["name", "triple", "package"], &ctx)?;
        cells.push(Cell {
            name: req_str(ct, "name", &ctx)?,
            triple: req_str(ct, "triple", &ctx)?,
            package: req_str(ct, "package", &ctx)?,
        });
    }
    if cells.is_empty() {
        return Err(format!(
            "{POLICY_PATH}: [forge] cells is empty — a ledger measuring no target measures \
             nothing; list at least one cell"
        ));
    }

    Ok(ForgeHeader {
        loc_method,
        graph_method,
        cells,
    })
}

fn fork_block(t: &dyn TableLike, n: usize) -> Result<Fork, String> {
    // Named by package once we have the name; by index until then, because an
    // error about "block 3" in a file of near-identical blocks is not a fix.
    let ctx0 = format!("[[fork]] #{}", n + 1);
    let name = req_str(t, "name", &ctx0)?;
    let ctx = format!("[[fork]] `{name}`");
    check_keys(
        t,
        &[
            "name",
            "version",
            "path",
            "license",
            "apache_notice",
            "census",
        ],
        &ctx,
    )?;

    let version = req_str(t, "version", &ctx)?;
    let path = req_str(t, "path", &ctx)?;
    if !path.starts_with("vendor/") {
        return Err(format!(
            "{POLICY_PATH}: {ctx} path = \"{path}\" is not under `vendor/` — this ledger \
             records vendored forks; a patch pointing anywhere else needs its own review \
             before it can be recorded here"
        ));
    }
    let license = req_str(t, "license", &ctx)?;
    let apache_notice = match t.get("apache_notice") {
        None => false,
        Some(item) => item.as_bool().ok_or_else(|| {
            format!(
                "{POLICY_PATH}: {ctx} apache_notice must be `true` or `false` (unquoted), \
                 not {}",
                render_item(item)
            )
        })?,
    };

    let census = t.get("census").ok_or_else(|| {
        format!(
            "{POLICY_PATH}: {ctx} has no `census` — every fork must say how the lock-order \
             census treats it. Add `census.mode = \"scanned\"` plus `census.namespace = \
             \"{name}\"`, or `census.mode = \"build-dep-only\"`"
        )
    })?;
    let ct = census.as_table_like().ok_or_else(|| {
        format!(
            "{POLICY_PATH}: {ctx} census must be a table — write `census.mode = \"…\"` or a \
             `[fork.census]` block"
        )
    })?;
    check_keys(ct, &["mode", "namespace"], &format!("{ctx} census"))?;
    let census_mode = CensusMode::parse(&req_str(ct, "mode", &ctx)?, &ctx)?;
    let census_namespace = opt_str(ct, "namespace", &ctx)?;
    match (census_mode, &census_namespace) {
        (CensusMode::Scanned, None) => {
            return Err(format!(
                "{POLICY_PATH}: {ctx} is census.mode = \"scanned\" but has no \
                 census.namespace — a scanned fork's lock identities MUST live in a \
                 per-crate namespace or a foreign receiver name can merge with an aterm \
                 one. Add `census.namespace = \"{name}\"`"
            ));
        }
        (CensusMode::BuildDepOnly, Some(ns)) => {
            return Err(format!(
                "{POLICY_PATH}: {ctx} is census.mode = \"build-dep-only\" but declares \
                 census.namespace = \"{ns}\" — a crate that never links into a shipped \
                 process contributes no identities. Delete the namespace, or change the \
                 mode to \"scanned\""
            ));
        }
        _ => {}
    }

    Ok(Fork {
        name,
        version,
        path,
        license,
        apache_notice,
        census_mode,
        census_namespace,
    })
}

// --- small typed accessors, each refusal naming the fix ---------------------

fn check_keys(t: &dyn TableLike, allowed: &[&str], ctx: &str) -> Result<(), String> {
    for (key, _) in t.iter() {
        if !allowed.contains(&key) {
            return Err(format!(
                "{POLICY_PATH}: {ctx} has unknown key `{key}` — allowed keys are {}. A typo'd \
                 key must never read as an unclaimed obligation, so this is refused rather \
                 than ignored; put prose in a `#` comment instead (comments survive every \
                 round-trip)",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

fn req_str(t: &dyn TableLike, key: &str, ctx: &str) -> Result<String, String> {
    match opt_str(t, key, ctx)? {
        Some(s) => Ok(s),
        None => Err(format!(
            "{POLICY_PATH}: {ctx} has no `{key}` — add `{key} = \"…\"`"
        )),
    }
}

fn opt_str(t: &dyn TableLike, key: &str, ctx: &str) -> Result<Option<String>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(item) => match item.as_str() {
            Some(s) => Ok(Some(s.to_string())),
            None => Err(format!(
                "{POLICY_PATH}: {ctx} `{key}` must be a quoted string, not {}",
                render_item(item)
            )),
        },
    }
}

fn render_item(item: &Item) -> String {
    let t = match item {
        Item::None => "nothing",
        Item::Value(v) => v.type_name(),
        Item::Table(_) => "a table",
        Item::ArrayOfTables(_) => "an array of tables",
    };
    t.to_string()
}

// ---------------------------------------------------------------------------
// The measured `[patch.crates-io]` facts
// ---------------------------------------------------------------------------

/// One `Cargo.lock` package record. The lock is TOML, so it is read with the
/// same parser as everything else rather than scanned by hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockEntry {
    pub name: String,
    pub version: String,
    /// `None` for a path package: a workspace member, or a `[patch]` fork
    /// (hence no checksum, and cargo-deny's `yanked = "deny"` never fires).
    pub source: Option<String>,
}

/// Every `[[package]]` in `<root>/Cargo.lock`, in file order.
pub fn lock_entries(root: &Path) -> Result<Vec<LockEntry>, String> {
    let path = root.join("Cargo.lock");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {} ({e}) — run `targo --unverified metadata --locked --offline` from the \
             workspace root to regenerate it",
            path.display()
        )
    })?;
    let doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    let Some(item) = doc.get("package") else {
        return Err(format!(
            "{}: no `[[package]]` entries — the lock is empty",
            path.display()
        ));
    };
    let arr = item.as_array_of_tables().ok_or_else(|| {
        format!(
            "{}: `package` is not a sequence of `[[package]]` tables",
            path.display()
        )
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for t in arr {
        let name = t
            .get("name")
            .and_then(Item::as_str)
            .ok_or_else(|| format!("{}: a `[[package]]` block has no `name`", path.display()))?;
        let version = t
            .get("version")
            .and_then(Item::as_str)
            .ok_or_else(|| format!("{}: `[[package]] {name}` has no `version`", path.display()))?;
        out.push(LockEntry {
            name: name.to_string(),
            version: version.to_string(),
            source: t.get("source").and_then(Item::as_str).map(str::to_string),
        });
    }
    Ok(out)
}

/// One `[patch.crates-io]` path entry, measured against the tree and the lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchEntry {
    /// The crates.io package being replaced (the patch table key).
    pub name: String,
    /// Repo-relative replacement path, as written in the manifest.
    pub path: String,
    /// `version` from the vendored manifest.
    pub manifest_version: String,
    /// The SPDX expression from the vendored manifest.
    pub license: String,
    /// The version `Cargo.lock` resolved for the path copy of this package, or
    /// `None` when there is none (the patch is not in effect).
    pub lock_version: Option<String>,
    /// Registry-sourced versions of the same name that coexist with the fork,
    /// read from `Cargo.lock` alone.
    ///
    /// Non-empty means the fix may be absent from the copy that compiles; the
    /// lock records no edge kinds, so whether a sibling ships is decided
    /// against the per-cell `--edges normal` graph by `[OB-12]` in
    /// [`crate::check`].
    pub shadowed_by: Vec<String>,
    /// Third-party source under `vendor/` or a first-party member under
    /// `crates/`, per [`aterm_census::scan_set::classify_patch_target`].
    pub kind: PatchTargetKind,
}

impl PatchEntry {
    /// The patch is in effect: the lock resolved the path copy at exactly the
    /// version the patched manifest declares.
    pub fn is_live(&self) -> bool {
        self.lock_version.as_deref() == Some(self.manifest_version.as_str())
    }

    /// A vendored fork: redistributed third-party source, owing provenance
    /// obligations and a fork-ledger block. False for a first-party
    /// replacement.
    pub fn is_vendored(&self) -> bool {
        self.kind == PatchTargetKind::Vendored
    }
}

/// Read `[patch.crates-io]` from `<root>/Cargo.toml` and measure each entry
/// against the vendored manifest and `Cargo.lock`.
///
/// Measurement only: drift is recorded here for `forge budget` and
/// `forge check`; `[OB-12]` refuses a patch that does not take.
pub fn patch_entries(root: &Path) -> Result<Vec<PatchEntry>, String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("cannot read {} ({e})", manifest.display()))?;
    let doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", manifest.display()))?;

    let Some(patch) = doc.get("patch").and_then(|i| i.get("crates-io")) else {
        return Ok(Vec::new());
    };
    let pt = patch
        .as_table_like()
        .ok_or_else(|| format!("{}: `[patch.crates-io]` is not a table", manifest.display()))?;

    let lock = lock_entries(root)?;
    let mut out = Vec::new();
    for (name, item) in pt.iter() {
        let spec = item.as_table_like().ok_or_else(|| {
            format!(
                "{}: `[patch.crates-io] {name}` is not a table — forge models path patches \
                 only: `{name} = {{ path = \"vendor/{name}\" }}`",
                manifest.display()
            )
        })?;
        for (k, _) in spec.iter() {
            if k != "path" {
                return Err(format!(
                    "{}: `[patch.crates-io] {name}` has key `{k}` — forge models path patches \
                     only (a git or version patch has no vendored tree to notarize). Vendor \
                     it under `vendor/{name}` and write `{name} = {{ path = \
                     \"vendor/{name}\" }}`, or teach {POLICY_PATH} a new fork shape first",
                    manifest.display()
                ));
            }
        }
        let path = spec.get("path").and_then(Item::as_str).ok_or_else(|| {
            format!(
                "{}: `[patch.crates-io] {name}` has no `path` — write `{name} = {{ path = \
                 \"vendor/{name}\" }}`",
                manifest.display()
            )
        })?;

        let kind = classify_patch_target(name, path, root)?;
        let (pkg_name, manifest_version, license) = patched_manifest(root, path, kind)?;
        if pkg_name != name {
            return Err(format!(
                "{}/Cargo.toml declares package `{pkg_name}` but it is patched in as \
                 `{name}` — a patch must replace the package it names. Rename the vendored \
                 package back to `{name}`, or point the patch entry at the right directory",
                root.join(path).display()
            ));
        }

        let mut lock_version = None;
        let mut shadowed_by = Vec::new();
        for e in lock.iter().filter(|e| e.name == name) {
            if e.source.is_none() {
                lock_version.get_or_insert_with(|| e.version.clone());
            } else {
                shadowed_by.push(e.version.clone());
            }
        }
        out.push(PatchEntry {
            name: name.to_string(),
            path: path.to_string(),
            manifest_version,
            license,
            lock_version,
            shadowed_by,
            kind,
        });
    }
    Ok(out)
}

/// `(name, version, license)` from a patched crate's own manifest.
///
/// A vendored fork is outside the workspace, so an inherited
/// (`workspace = true`) key there is a named error. A first-party target is a
/// member, so inheritance is resolved from `[workspace.package]`.
fn patched_manifest(
    root: &Path,
    rel: &str,
    kind: PatchTargetKind,
) -> Result<(String, String, String), String> {
    let path = root.join(rel).join("Cargo.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {} ({e}) — `[patch.crates-io]` points at `{rel}`, so that directory \
             must contain the patched crate. Restore it, or delete the patch entry",
            path.display()
        )
    })?;
    let doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    let pkg = doc
        .get("package")
        .and_then(Item::as_table_like)
        .ok_or_else(|| format!("{}: no `[package]` table", path.display()))?;
    // `[workspace.package]` of the ROOT, read lazily and only for the
    // first-party arm.
    let inherited = |key: &str| -> Option<String> {
        if kind != PatchTargetKind::FirstParty {
            return None;
        }
        // `key.workspace = true` is the only inheritance form cargo accepts.
        let claims_inheritance = pkg
            .get(key)
            .and_then(Item::as_table_like)
            .and_then(|t| t.get("workspace"))
            .and_then(Item::as_bool)
            .unwrap_or(false);
        if !claims_inheritance {
            return None;
        }
        let root_text = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
        let root_doc: DocumentMut = root_text.parse().ok()?;
        root_doc
            .get("workspace")
            .and_then(Item::as_table_like)
            .and_then(|w| w.get("package"))
            .and_then(Item::as_table_like)
            .and_then(|p| p.get(key))
            .and_then(Item::as_str)
            .map(str::to_string)
    };
    let get = |key: &str| -> Result<String, String> {
        if let Some(v) = pkg.get(key).and_then(Item::as_str) {
            return Ok(v.to_string());
        }
        if let Some(v) = inherited(key) {
            return Ok(v);
        }
        Err(match kind {
            PatchTargetKind::Vendored => format!(
                "{}: `[package] {key}` is missing or not a plain string — a vendored fork's \
                 manifest must state it literally (workspace inheritance does not reach \
                 across into `vendor/`)",
                path.display()
            ),
            PatchTargetKind::FirstParty => format!(
                "{}: `[package] {key}` is missing — a first-party patch target may state it \
                 literally or inherit it with `{key}.workspace = true`, but the root \
                 `[workspace.package]` has no `{key}` to inherit",
                path.display()
            ),
        })
    };
    Ok((get("name")?, get("version")?, get("license")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::repo_root;

    const SAMPLE: &str = r#"# SPDX-License-Identifier: Apache-2.0
# a comment block that IS the record

[forge]
loc_method = "rs-physical-all-files-v1"
graph_method = "cargo-tree-normal-no-dedupe-locked-offline-v1"
cells = [
    { name = "mac-arm", triple = "aarch64-apple-darwin", package = "aterm" },
]

# --- winit 0.30.13 -----------------------------------------------------------
# The Wayland DnD patch. THIS PARAGRAPH is why the fork exists, and a round-trip
# that eats it defeats the file.
[[fork]]
name = "winit"
version = "0.30.13"
path = "vendor/winit"
license = "Apache-2.0"
apache_notice = true
census.mode = "scanned"
census.namespace = "winit"

# pkg-config only ever runs inside a build script.
[[fork]]
name = "pkg-config"
version = "0.3.33"
path = "vendor/pkg-config"
license = "MIT OR Apache-2.0"
census.mode = "build-dep-only"
"#;

    #[test]
    fn a_missing_file_is_an_empty_policy_not_an_error() {
        let p = load(std::path::Path::new("/nonexistent/aterm-forge-test-root"))
            .expect("an absent ledger is not an error");
        assert!(p.forks.is_empty());
        assert!(p.forge.cells.is_empty());
    }

    #[test]
    fn forks_are_modeled_including_census_and_notice() {
        let p = parse(SAMPLE).unwrap();
        assert_eq!(p.forks.len(), 2);
        let winit = p
            .forks
            .iter()
            .find(|f| f.name == "winit")
            .expect("winit block");
        assert_eq!(winit.version, "0.30.13");
        assert_eq!(winit.license, "Apache-2.0");
        assert!(winit.apache_notice);
        assert_eq!(winit.census_mode, CensusMode::Scanned);
        assert_eq!(winit.census_namespace.as_deref(), Some("winit"));
        let pc = p
            .forks
            .iter()
            .find(|f| f.name == "pkg-config")
            .expect("pkg-config block");
        assert!(!pc.apache_notice, "apache_notice defaults to false");
        assert_eq!(pc.census_mode, CensusMode::BuildDepOnly);
        assert_eq!(pc.census_namespace, None);
        assert_eq!(p.forge.cells.len(), 1);
        assert_eq!(p.forge.cells[0].triple, "aarch64-apple-darwin");
    }

    #[test]
    fn an_unknown_key_is_refused_by_name() {
        let bad = SAMPLE.replace("apache_notice = true", "apache_notices = true");
        let e = parse(&bad).unwrap_err();
        assert!(e.contains("apache_notices"), "{e}");
        assert!(e.contains("allowed keys are"), "{e}");
    }

    #[test]
    fn an_unknown_top_level_table_is_refused_by_name() {
        let e = parse(&format!("{SAMPLE}\n[carve]\nwhen = \"soon\"\n")).unwrap_err();
        assert!(e.contains("carve"), "{e}");
        assert!(e.contains("comment"), "{e}");
    }

    #[test]
    fn a_scanned_fork_without_a_namespace_is_refused() {
        let bad = SAMPLE.replace("census.namespace = \"winit\"\n", "");
        let e = parse(&bad).unwrap_err();
        assert!(e.contains("census.namespace"), "{e}");
        assert!(e.contains("namespace"), "{e}");
    }

    #[test]
    fn a_build_dep_only_fork_with_a_namespace_is_refused() {
        let bad = SAMPLE.replace(
            "census.mode = \"build-dep-only\"",
            "census.mode = \"build-dep-only\"\ncensus.namespace = \"pkg_config\"",
        );
        let e = parse(&bad).unwrap_err();
        assert!(e.contains("build-dep-only"), "{e}");
    }

    #[test]
    fn a_drifted_method_string_is_refused_rather_than_reinterpreted() {
        let bad = SAMPLE.replace(LOC_METHOD, "rs-logical-v2");
        let e = parse(&bad).unwrap_err();
        assert!(e.contains("rs-logical-v2"), "{e}");
        assert!(e.contains(LOC_METHOD), "{e}");
    }

    #[test]
    fn a_missing_forge_header_names_the_block_to_add() {
        let body = SAMPLE.split("# --- winit").nth(1).unwrap();
        let e = parse(&format!("# --- winit{body}")).unwrap_err();
        assert!(e.contains("[forge]"), "{e}");
        assert!(e.contains("loc_method"), "{e}");
    }

    #[test]
    fn two_blocks_for_one_package_are_refused() {
        let dup = format!(
            "{SAMPLE}\n[[fork]]\nname = \"winit\"\nversion = \"0.30.13\"\n\
             path = \"vendor/winit\"\nlicense = \"Apache-2.0\"\ncensus.mode = \"scanned\"\n\
             census.namespace = \"winit\"\n"
        );
        let e = parse(&dup).unwrap_err();
        assert!(e.contains("winit"), "{e}");
    }

    // --- tests that walk the REAL tree (the house norm) ---------------------

    #[test]
    fn the_real_patch_table_is_five_live_vendored_forks_and_seven_first_party_targets() {
        let root = repo_root();
        let all = patch_entries(&root).expect("the real patch table reads");
        let (vendored, first_party): (Vec<_>, Vec<_>) = all.iter().partition(|e| e.is_vendored());
        assert_eq!(
            vendored.len(),
            5,
            "vendored forks: {:?}",
            vendored.iter().map(|e| &e.name).collect::<Vec<_>>()
        );
        for e in &vendored {
            assert!(e.path.starts_with("vendor/"), "{} -> {}", e.name, e.path);
        }
        // The `vendor/` prefix belongs to the vendored arm; first-party
        // replacements live under crates/.
        assert_eq!(
            first_party
                .iter()
                .map(|e| (e.name.as_str(), e.path.as_str()))
                .collect::<Vec<_>>(),
            // Manifest order (`attest::partition_patches` sorts instead).
            [
                ("tracing", "crates/aterm-tracing"),
                ("profiling", "crates/aterm-profiling"),
                ("cfg-if", "crates/aterm-cfg-if"),
                ("arrayvec", "crates/aterm-arrayvec"),
                ("libc", "crates/aterm-libc"),
                ("log", "crates/aterm-log-shim"),
                ("core_maths", "crates/aterm-core-maths"),
                ("once_cell", "crates/aterm-once-cell"),
            ]
        );
        // LIVENESS, on the other hand, is owed by every entry alike: a patch
        // that does not take compiles into nothing, whoever wrote it.
        for e in &all {
            assert!(
                e.is_live(),
                "{} is not live: manifest {} vs lock {:?}",
                e.name,
                e.manifest_version,
                e.lock_version
            );
        }
    }

    /// A first-party patch target inheriting `license` from
    /// `[workspace.package]` is resolved, not refused.
    #[test]
    fn a_first_party_patch_target_may_inherit_its_license_from_the_workspace() {
        let root = repo_root();
        let all = patch_entries(&root).expect("the real patch table reads");
        let shims: Vec<_> = all.iter().filter(|e| !e.is_vendored()).collect();
        assert_eq!(
            shims.len(),
            8,
            "this tree has eight first-party patch targets"
        );
        // Every one, not the first found.
        for e in &shims {
            assert_eq!(e.license, "Apache-2.0", "{} inherited, not literal", e.name);
        }
        // The versions are semver contracts with third-party consumers, not
        // aterm release numbers, so each is pinned by name.
        let versions: Vec<(&str, &str)> = shims
            .iter()
            .map(|e| (e.name.as_str(), e.manifest_version.as_str()))
            .collect();
        assert_eq!(
            versions,
            [
                ("tracing", "0.1.44"),
                ("profiling", "1.0.18"),
                ("cfg-if", "1.0.4"),
                ("arrayvec", "0.7.8"),
                ("libc", "0.2.186"),
                ("log", "0.4.32"),
                ("core_maths", "0.1.1"),
                ("once_cell", "1.21.4"),
            ]
        );
    }

    #[test]
    fn the_real_lock_is_read_as_toml() {
        let root = repo_root();
        let lock = lock_entries(&root).expect("the real lock reads");
        assert!(lock.len() > 300, "{} entries", lock.len());
        let forks = lock
            .iter()
            .filter(|e| e.source.is_none() && e.name == "winit")
            .count();
        assert_eq!(
            forks, 1,
            "the vendored winit is a path package with no source/checksum"
        );
    }
}
