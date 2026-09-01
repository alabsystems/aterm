// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `vendor/forge.toml` — THE FORK LEDGER, and the `[patch.crates-io]` facts the
//! rest of forge measures against.
//!
//! # Why this file exists at all
//!
//! Six upstream crates now live under `vendor/`, redirected by
//! `[patch.crates-io]`, because Trust's in-compilation verification correctly
//! refused to compile latent panics/overflows in them. That makes aterm the
//! maintainer of ~1.1 MB of code it did not write, under five different license
//! strings, with obligations (Apache-2.0 §4(b) modification notices, retained
//! LICENSE files, upstream provenance) that no compiler enforces.
//!
//! `vendor/forge.toml` is the checked-in record of that: one `[[fork]]` block
//! per patch entry, plus a `[forge]` header pinning the measurement methods so a
//! number in a report can be re-derived years later.
//!
//! # THE COMMENTS ARE THE RECORD
//!
//! A fork's REASON to exist is not a key in this file — it is the comment block
//! above it. That is deliberate: a reason is prose, and prose crammed into a
//! TOML string loses its line breaks and its editability. It also means every
//! read/write of this file MUST preserve comments, which is exactly why the
//! parser is `aterm-toml`'s document model and not a hand-rolled reader:
//! [`Policy::render`] returns the parsed document byte-for-byte, so a future
//! `--update` that adds a key cannot silently eat the paragraph explaining why
//! `winit` is forked.
//!
//! # Fail-closed, both ways
//!
//! An UNKNOWN KEY is an error naming the key (the discipline
//! `aterm_census::scan_set` already applies to the vendored-crate registry): a
//! typo'd `apache_notices = true` must never read as "obligation not claimed".
//! A MISSING FILE is *not* an error — Stage 0 ships before the file does, and
//! [`load`] returns an empty [`Policy`] so the gate can report "no ledger yet"
//! instead of failing to run.
//!
//! # Version drift silently un-uses a patch
//!
//! `[patch.crates-io] indexmap = { path = "vendor/indexmap" }` only takes
//! effect while the vendored manifest's version still satisfies the requirement
//! the graph asks for. Bump `vendor/indexmap/Cargo.toml` to `3.0.0` and every
//! `indexmap = "2"` in the graph quietly resolves to the *registry* copy again —
//! the fix is gone, nothing fails, and the only visible trace is a line in
//! `Cargo.lock`. [`patch_entries`] therefore reads the lock and reports, per
//! entry, the version the lock actually resolved for the path package and any
//! registry-sourced copies of the same name that coexist with it.

use crate::model::Cell;
use aterm_census::scan_set::{
    PatchTargetKind, REVIEWED_VENDORED_CRATES, VendoredMode, classify_patch_target,
};
use aterm_toml::edit::{DocumentMut, Item, TableLike};
use std::fmt::Write as _;
use std::path::Path;

/// The ledger's path, relative to the workspace root.
pub const POLICY_PATH: &str = "vendor/forge.toml";

/// The one LOC method forge implements: physical lines over every `*.rs` under
/// the package root, that package's own tests and examples included. A ledger
/// naming a different method is refused rather than silently re-interpreted.
pub const LOC_METHOD: &str = "rs-physical-all-files-v1";

/// The one graph method forge implements: `cargo tree -p <package> -e normal
/// --target <triple> --prefix depth --no-dedupe --locked --offline`, parsed by
/// depth prefix. Deliberately NOT `cargo metadata --filter-platform`, whose
/// resolve is feature-unified across all workspace members and over-counts the
/// macOS root by 28% (271 nodes against cargo tree's 212).
pub const GRAPH_METHOD: &str = "cargo-tree-normal-no-dedupe-locked-offline-v1";

/// Comment width used by the emitted seed body.
const WRAP: usize = 92;

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// How `aterm-census`'s lock-order census classifies a vendored fork. Mirrors
/// [`aterm_census::scan_set::VendoredMode`] without duplicating its payload:
/// the registry stays the single definition, this is just the ledger's spelling
/// of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CensusMode {
    /// Links into the shipped GUI process: scanned, with every lock identity in
    /// a per-crate namespace so a foreign receiver name can never merge with an
    /// aterm identity.
    Scanned,
    /// Runs only inside a build script (a host tool), so it never links into a
    /// shipped process and is classified out with a written justification.
    BuildDepOnly,
}

impl CensusMode {
    /// The ledger spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scanned => "scanned",
            Self::BuildDepOnly => "build-dep-only",
        }
    }

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
    /// still resolve. Cross-checked against `Cargo.lock` by [`seed_from_vendor`].
    pub version: String,
    /// Repo-relative directory (`vendor/winit`).
    pub path: String,
    /// The SPDX expression from the vendored manifest, verbatim.
    pub license: String,
    /// `true` when this fork's license leaves no non-Apache option, so the
    /// Apache-2.0 §4(b) "carry prominent notices stating that You changed the
    /// files" obligation binds every file aterm modified. `cargo forge attest`
    /// is what checks the notices; this flag is what says they are owed.
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

/// The parsed ledger. `doc` is retained so [`Policy::render`] can return the
/// file byte-for-byte — see the module docs on why comment preservation is not
/// optional here.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub forge: ForgeHeader,
    pub forks: Vec<Fork>,
    doc: Option<DocumentMut>,
}

impl Policy {
    /// `true` when no ledger file exists yet (Stage 0 ships before it does).
    pub fn is_absent(&self) -> bool {
        self.doc.is_none()
    }

    /// The document as text, byte-identical to what [`load`] read. `None` when
    /// no file was read.
    pub fn render(&self) -> Option<String> {
        self.doc.as_ref().map(std::string::ToString::to_string)
    }

    /// The block for one package name.
    pub fn fork(&self, name: &str) -> Option<&Fork> {
        self.forks.iter().find(|f| f.name == name)
    }
}

// ---------------------------------------------------------------------------
// Reading the ledger
// ---------------------------------------------------------------------------

/// Read `<root>/vendor/forge.toml`. An ABSENT file is an empty [`Policy`], not
/// an error: Stage 0 of forge ships before the ledger it will write.
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

/// Parse a ledger body. Split out from [`load`] so the round-trip and the
/// fail-closed key rules are testable without a filesystem.
pub fn parse(text: &str) -> Result<Policy, String> {
    let doc: DocumentMut = text.parse().map_err(|e| {
        format!("{POLICY_PATH} is not valid TOML: {e} — fix the syntax; forge will not guess")
    })?;

    for (key, _) in doc.as_table().iter() {
        if key != "forge" && key != "fork" {
            return Err(format!(
                "{POLICY_PATH}: unknown top-level table `{key}` — this file holds exactly \
                 `[forge]` (the method header) and `[[fork]]` blocks. Delete it, or move the \
                 note into a `#` comment: comments are preserved verbatim, keys are not free"
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

    Ok(Policy {
        forge,
        forks,
        doc: Some(doc),
    })
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
             forge no longer implements. Re-measure with `cargo forge survey` and set \
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
    /// `None` for a path package: a workspace member, or a `[patch]` fork.
    /// That absence is exactly why a fork has no checksum and cargo-deny's
    /// `yanked = "deny"` can never fire on one.
    pub source: Option<String>,
}

/// Every `[[package]]` in `<root>/Cargo.lock`, in file order.
pub fn lock_entries(root: &Path) -> Result<Vec<LockEntry>, String> {
    let path = root.join("Cargo.lock");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {} ({e}) — run `cargo metadata --locked --offline` from the \
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
    /// The version `Cargo.lock` resolved for the PATH copy of this package
    /// (the source-less entry). `None` when the lock has no path copy at all —
    /// i.e. the patch is not in effect.
    pub lock_version: Option<String>,
    /// Registry-sourced versions of the SAME NAME that coexist with the fork,
    /// read from `Cargo.lock` alone.
    ///
    /// Non-empty is a QUESTION, not a verdict, and the distinction was learned
    /// from a false positive. It was the shape that held on Linux until
    /// 2026-08-27, when registry `winnow 1.0.3` rode in beside the
    /// `winnow 0.7.15` this repository forked and the fix was absent from the
    /// copy that compiled. It is ALSO the shape of a dev-only differential
    /// oracle — `crates/aterm-alloc` dev-depends on registry `arrayvec =0.7.7`
    /// precisely so its differential is not the shim compared with itself — and
    /// that copy compiles into nothing aterm ships.
    ///
    /// The lock records no edge kinds, so this field cannot tell them apart.
    /// Whether a sibling is a DEFECT is decided against the per-cell
    /// `--edges normal` graph, by `[OB-12]` in [`crate::check`] and by
    /// `no_fork_is_shadowed_by_an_unpatched_registry_copy`.
    pub shadowed_by: Vec<String>,
    /// What the replacement IS: third-party source under `vendor/`, or a
    /// first-party workspace member under `crates/`. Read from the path by
    /// [`aterm_census::scan_set::classify_patch_target`], so forge and the
    /// census cannot disagree about which obligations apply.
    pub kind: PatchTargetKind,
}

impl PatchEntry {
    /// The patch is in effect: the lock resolved the path copy at exactly the
    /// version the patched manifest declares.
    pub fn is_live(&self) -> bool {
        self.lock_version.as_deref() == Some(self.manifest_version.as_str())
    }

    /// A VENDORED fork: third-party source this repository redistributes, and
    /// therefore owes the provenance obligations and a fork-ledger block.
    /// FALSE for a first-party replacement, which owes neither — that is the
    /// whole distinction this type now carries.
    pub fn is_vendored(&self) -> bool {
        self.kind == PatchTargetKind::Vendored
    }
}

/// Read `[patch.crates-io]` from `<root>/Cargo.toml` and measure each entry
/// against the vendored manifest and `Cargo.lock`.
///
/// Measurement, not judgement: drift is RECORDED here (so `cargo forge budget`
/// can count live entries and `cargo forge attest` can report on them) and
/// REFUSED in [`seed_from_vendor`], which is the authoring path.
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
/// The LITERAL requirement is a `vendor/` rule, not a patch rule. A vendored
/// fork is outside the workspace — `version.workspace = true` in `vendor/x`
/// resolves against nothing, so an inherited key there is a defect worth a
/// named error. A FIRST-PARTY patch target is a workspace MEMBER, where
/// inheritance is the house style and every other crate uses it; demanding
/// literals there would be demanding that one member be written unlike all the
/// others for no reason anyone could state. So inheritance is resolved from
/// `[workspace.package]` for the first-party arm, and refused for the
/// vendored one.
///
/// `version` stays literal even first-party in practice: the shim's version is
/// a SEMVER CONTRACT with its third-party consumers (`^0.1.4x`), not an aterm
/// release number — but that is the crate's business to state, not this
/// function's to enforce.
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

// ---------------------------------------------------------------------------
// Seeding the ledger from the real tree
// ---------------------------------------------------------------------------

/// Produce a complete `vendor/forge.toml` body from the live tree: the real
/// `[patch.crates-io]` table, the real vendored manifests, and the real census
/// registry. RETURNS the text — writing it is the caller's decision, because a
/// hand-edited ledger's comments are the record and forge will not overwrite
/// them behind anyone's back.
///
/// Refuses (naming the fix) when a patch is not in effect: a vendored version
/// that has drifted from what the lock resolved means the fork is not being
/// compiled, and a ledger asserting obligations over dead code is worse than no
/// ledger.
pub fn seed_from_vendor(root: &Path) -> Result<String, String> {
    let all = patch_entries(root)?;
    // FIRST-PARTY patch targets are not forks and must not be seeded. The
    // ledger's own parser refuses a `path` outside `vendor/` — correctly, it
    // records vendored forks — so a seeder that emitted one would produce a
    // file its own reader rejects. They are named in a comment instead, so the
    // ledger's silence about them is a stated fact rather than an omission.
    let (patches, first_party): (Vec<PatchEntry>, Vec<PatchEntry>) =
        all.into_iter().partition(PatchEntry::is_vendored);
    if patches.is_empty() {
        return Err(format!(
            "{}: no VENDORED `[patch.crates-io]` entries — there are no forks to record. \
             Delete {POLICY_PATH} rather than seeding an empty ledger",
            root.join("Cargo.toml").display()
        ));
    }

    let mut s = String::new();
    s.push_str("# SPDX-License-Identifier: Apache-2.0\n# Copyright 2026 Andrew Yates\n#\n");
    s.push_str(&comment(
        "# ",
        "# ",
        "vendor/forge.toml — THE FORK LEDGER. One [[fork]] block per [patch.crates-io] entry \
         in the workspace root manifest: what aterm vendored, at which upstream version, \
         under which license, and how the lock-order census classifies it.",
    ));
    s.push_str("#\n");
    s.push_str(&comment(
        "# ",
        "# ",
        "THE COMMENTS ARE THE RECORD. A fork's reason to exist is not a key here — it is the \
         comment block above it. forge round-trips this file with aterm-toml precisely so that \
         no future write can eat one. Unknown KEYS are refused by name (fail-closed, the way \
         aterm-census's vendored-crate registry is); unknown COMMENTS are kept verbatim.",
    ));
    s.push_str("#\n");
    s.push_str(&comment(
        "# ",
        "# ",
        "Every version below is cross-checked against Cargo.lock: a vendored version that \
         drifts from the requirement the graph asks for silently un-uses the patch, and the \
         only trace is one line in the lock.",
    ));
    s.push_str("#\n");
    s.push_str(&comment(
        "# ",
        "# ",
        "Regenerate this skeleton with aterm_forge::policy::seed_from_vendor and DIFF it \
         against the checked-in file. Never overwrite: the diff is the review.",
    ));
    if !first_party.is_empty() {
        s.push_str("#\n");
        s.push_str(&comment(
            "# ",
            "# ",
            &format!(
                "NOT RECORDED HERE, deliberately: {} — [patch.crates-io] entr{} pointing at a \
                 FIRST-PARTY workspace member. This ledger records what aterm VENDORED, and a \
                 crate aterm wrote is not a redistribution of anyone's work: it has no \
                 upstream version, no upstream license to retain and no §4(b) obligation, so \
                 a [[fork]] block for it could only state falsehoods. The patch is still \
                 checked — `cargo forge attest` [OB-1]/[OB-2] and `cargo forge check` \
                 [OB-12] cover that it exists and that it is live in every cell.",
                first_party
                    .iter()
                    .map(|p| format!("`{}` → {}", p.name, p.path))
                    .collect::<Vec<_>>()
                    .join(", "),
                if first_party.len() == 1 { "y" } else { "ies" }
            ),
        ));
    }
    s.push_str("\n[forge]\n");
    s.push_str(&comment(
        "# ",
        "# ",
        "The methods every number in a forge report was produced by, recorded so a report can \
         be re-derived years later and so a change of method lands as a diff instead of as a \
         silent shift in the ratchet.",
    ));
    s.push_str(&comment(
        "# ",
        "#   ",
        &format!(
            "loc_method \"{LOC_METHOD}\": physical lines over every *.rs under the package \
             root, that package's own tests and examples included. It measures the source \
             aterm would OWN on vendoring, not the code that reaches codegen.",
        ),
    ));
    s.push_str(&comment(
        "# ",
        "#   ",
        &format!(
            "graph_method \"{GRAPH_METHOD}\": cargo tree -p <package> -e normal --target \
             <triple> --prefix depth --no-dedupe --locked --offline. Deliberately NOT cargo \
             metadata --filter-platform, whose resolve is feature-unified across every \
             workspace member and over-counts the macOS root by 28%.",
        ),
    ));
    let _ = writeln!(s, "loc_method = \"{LOC_METHOD}\"");
    let _ = writeln!(s, "graph_method = \"{GRAPH_METHOD}\"");
    s.push_str(&comment(
        "# ",
        "# ",
        "The measurement matrix: resolution needs no toolchain, so every cell is mandatory and \
         offline. A cell that cannot resolve is named and skipped, never passed.",
    ));
    s.push_str("cells = [\n");
    for c in crate::resolve::default_cells() {
        let _ = writeln!(
            s,
            "    {{ name = \"{}\", triple = \"{}\", package = \"{}\" }},",
            c.name, c.triple, c.package
        );
    }
    s.push_str("]\n");

    for p in &patches {
        if !p.is_live() {
            return Err(drift_message(p));
        }
        let (mode, namespace, note) = census_classification(&p.name, &p.path)?;
        let notice = apache_notice_binds(&p.license);

        let _ = writeln!(
            s,
            "\n# --- {} {} {}",
            p.name,
            p.manifest_version,
            "-".repeat(72usize.saturating_sub(p.name.len() + p.manifest_version.len()))
        );
        s.push_str(&comment("# census review: ", "#   ", note));
        if notice {
            s.push_str(&comment(
                "# LICENSE: ",
                "#   ",
                &format!(
                    "{} — no non-Apache option, so Apache-2.0 §4(b) binds: every file aterm \
                     modified must carry a prominent notice stating that it changed it. \
                     apache_notice = true is the assertion; `cargo forge attest` is what \
                     checks the notices are actually there.",
                    p.license
                ),
            ));
        } else if p.license.to_ascii_uppercase().contains("APACHE-2.0") {
            s.push_str(&comment(
                "# LICENSE: ",
                "#   ",
                &format!(
                    "{} — dual-licensed, so a copy distributed under the non-Apache option \
                     carries no §4(b) modification-notice obligation. Set apache_notice = \
                     true here the day that election changes.",
                    p.license
                ),
            ));
        } else {
            s.push_str(&comment(
                "# LICENSE: ",
                "#   ",
                &format!(
                    "{} — no Apache-2.0 term applies, so there is no §4(b) \
                     modification-notice obligation. The retained LICENSE file and the \
                     copyright notice still are obligations; `cargo forge attest` checks \
                     those.",
                    p.license
                ),
            ));
        }
        if !p.shadowed_by.is_empty() {
            s.push_str(&comment(
                "# PATCH LIVENESS: ",
                "#   ",
                &format!(
                    "the fork is live at {}, but the lock ALSO carries registry {} {} — the \
                     fix is not everywhere the name is. Anything resolving that other major \
                     runs unpatched.",
                    p.manifest_version,
                    p.name,
                    p.shadowed_by.join(", ")
                ),
            ));
        }
        s.push_str("[[fork]]\n");
        let _ = writeln!(s, "name = \"{}\"", p.name);
        let _ = writeln!(s, "version = \"{}\"", p.manifest_version);
        let _ = writeln!(s, "path = \"{}\"", p.path);
        let _ = writeln!(s, "license = \"{}\"", p.license);
        let _ = writeln!(s, "apache_notice = {notice}");
        let _ = writeln!(s, "census.mode = \"{}\"", mode.as_str());
        if let Some(ns) = namespace {
            let _ = writeln!(s, "census.namespace = \"{ns}\"");
        }
    }
    Ok(s)
}

/// `true` when the license expression leaves no non-Apache option, so the
/// Apache-2.0 §4(b) modification-notice obligation binds every changed file.
/// A dual `MIT OR Apache-2.0` fork can be distributed under MIT, which carries
/// no such clause.
fn apache_notice_binds(license: &str) -> bool {
    let l = license.to_ascii_uppercase();
    l.contains("APACHE-2.0") && !l.contains("MIT") && !l.contains("BSD") && !l.contains("ZLIB")
}

fn drift_message(p: &PatchEntry) -> String {
    match &p.lock_version {
        None => format!(
            "`[patch.crates-io] {name}` is NOT in effect: Cargo.lock has no path copy of \
             `{name}` at all, so every dependent resolves the registry crate and the vendored \
             fix in `{path}` compiles into nothing. Fix: run `cargo update -p {name}` (or \
             `cargo metadata --offline`) to re-resolve, and if the lock still refuses, the \
             vendored version {ver} no longer satisfies the requirement the graph asks for — \
             set `{path}/Cargo.toml` back to the version upstream published",
            name = p.name,
            path = p.path,
            ver = p.manifest_version
        ),
        Some(lock) => format!(
            "`[patch.crates-io] {name}` has DRIFTED: `{path}/Cargo.toml` says version = \
             \"{ver}\" but Cargo.lock resolved the patched `{name}` at \"{lock}\". One of the \
             two is stale, and a drift here silently un-uses the patch. Fix: set the vendored \
             version back to \"{lock}\", or run `cargo update -p {name}` to re-resolve the \
             lock against \"{ver}\"",
            name = p.name,
            path = p.path,
            ver = p.manifest_version,
            lock = lock
        ),
    }
}

/// The census's classification of one vendored crate, read from
/// [`REVIEWED_VENDORED_CRATES`] rather than restated — one definition of the
/// vendor registry, fail-closed both ways exactly as the census is.
fn census_classification(
    name: &str,
    path: &str,
) -> Result<(CensusMode, Option<&'static str>, &'static str), String> {
    let Some(v) = REVIEWED_VENDORED_CRATES.iter().find(|v| v.package == name) else {
        return Err(format!(
            "`{name}` is patched in from `{path}` but is NOT registered in \
             aterm_census::scan_set::REVIEWED_VENDORED_CRATES — an unreviewed vendored crate \
             is a hard error there and here. Review it and add a VendoredCrate entry (mode \
             Scanned with a namespace if it links into the GUI process, BuildDepOnly with a \
             written justification if it only runs inside a build script)"
        ));
    };
    if v.path != path {
        return Err(format!(
            "`{name}` is patched in from `{path}` but REVIEWED_VENDORED_CRATES registers it \
             at `{}` — a stale review. Update the registry entry's `path`",
            v.path
        ));
    }
    Ok(match &v.mode {
        VendoredMode::Scanned {
            namespace, audit, ..
        } => (CensusMode::Scanned, Some(namespace), *audit),
        VendoredMode::BuildDepOnly { justification } => {
            (CensusMode::BuildDepOnly, None, *justification)
        }
    })
}

/// Word-wrap `text` into `#` comment lines: `first` opens the block, `cont`
/// opens every continuation line.
fn comment(first: &str, cont: &str, text: &str) -> String {
    let mut out = String::new();
    let mut line = String::from(first);
    let mut any = false;
    for word in text.split_whitespace() {
        if any && line.chars().count() + 1 + word.chars().count() > WRAP {
            out.push_str(line.trim_end());
            out.push('\n');
            line = String::from(cont);
            any = false;
        }
        if any {
            line.push(' ');
        }
        line.push_str(word);
        any = true;
    }
    if any {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> std::path::PathBuf {
        // crates/aterm-forge/ -> the workspace root.
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("the crate lives two levels under the workspace root")
            .to_path_buf()
    }

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
    fn round_trip_is_byte_identical_including_every_comment() {
        let p = parse(SAMPLE).expect("sample parses");
        let back = p.render().expect("a parsed policy renders");
        assert_eq!(
            back, SAMPLE,
            "the document round-trip must not move a single byte"
        );
        assert!(back.contains("THIS PARAGRAPH is why the fork exists"));
    }

    #[test]
    fn a_missing_file_is_an_empty_policy_not_an_error() {
        let p = load(std::path::Path::new("/nonexistent/aterm-forge-test-root"))
            .expect("an absent ledger is not an error");
        assert!(p.forks.is_empty());
        assert!(p.is_absent());
        assert!(p.render().is_none());
    }

    #[test]
    fn forks_are_modeled_including_census_and_notice() {
        let p = parse(SAMPLE).unwrap();
        assert_eq!(p.forks.len(), 2);
        let winit = p.fork("winit").expect("winit block");
        assert_eq!(winit.version, "0.30.13");
        assert_eq!(winit.license, "Apache-2.0");
        assert!(winit.apache_notice);
        assert_eq!(winit.census_mode, CensusMode::Scanned);
        assert_eq!(winit.census_namespace.as_deref(), Some("winit"));
        let pc = p.fork("pkg-config").expect("pkg-config block");
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

    #[test]
    fn apache_notice_binds_only_without_a_permissive_option() {
        assert!(apache_notice_binds("Apache-2.0"));
        assert!(apache_notice_binds("Apache-2.0 WITH LLVM-exception"));
        assert!(!apache_notice_binds("MIT OR Apache-2.0"));
        assert!(!apache_notice_binds("Apache-2.0 OR MIT"));
        assert!(!apache_notice_binds("MIT"));
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
            "measured 2026-08-27, when retiring `toml_edit` retired the winnow \
             fork with it: {:?}",
            vendored.iter().map(|e| &e.name).collect::<Vec<_>>()
        );
        for e in &vendored {
            assert!(e.path.starts_with("vendor/"), "{} -> {}", e.name, e.path);
        }
        // The `vendor/` prefix is a property of the VENDORED arm, not of being
        // patched: a first-party replacement is a workspace member and lives
        // under crates/. Asserting it over the whole table made "is patched"
        // mean "is somebody else's code".
        assert_eq!(
            first_party
                .iter()
                .map(|e| (e.name.as_str(), e.path.as_str()))
                .collect::<Vec<_>>(),
            // MANIFEST ORDER here, not sorted: `patch_entries` walks the table
            // as written, where `attest::partition_patches` sorts. Both orders
            // are asserted literally, in their own module, rather than being
            // normalised — the order IS part of what each function returns.
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

    /// A first-party patch target inherits `license` from `[workspace.package]`
    /// like every other member, and `patched_manifest` resolves it. The
    /// literal-only rule is a `vendor/` rule (inheritance reaches nothing out
    /// there), and applying it to a workspace member would have made
    /// `patch_entries` — and with it the whole `cargo forge budget` ratchet —
    /// a hard could-not-run.
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
        // EVERY one, not the first one found. The original read
        // `.find(|e| !e.is_vendored())` and asserted `0.1.44` — which happened
        // to be right only because `tracing` sat first in the manifest, so four
        // later replacements could have inherited nothing and it would still
        // have passed.
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

    /// POLARITY: this asserts the CLEAN state, and did not always.
    ///
    /// Until 2026-08-27 it required `winnow` to be shadowed — `winnow 1.0.3`
    /// from the registry rode beside the `winnow 0.7.15` fork on Linux, and the
    /// test named that as the tree's one known defect. Retiring `toml_edit` for
    /// the first-party `aterm-toml` removed aterm's only winnow 0.7 edge, so
    /// the fork went with it and there is nothing left for the registry copy to
    /// shadow. A test that requires a defect to be present goes red the day
    /// someone fixes it, so this states the property instead: NO patched name
    /// runs unpatched. The detector itself stays proved by
    /// `tests/red_fixtures.rs::an_unpatched_sibling_version_reds_the_forge_verb`.
    ///
    /// # Why the lock alone is the wrong instrument, and what replaced it
    ///
    /// This asserted `shadowed_by.is_empty()` over `Cargo.lock`, and that read
    /// went WRONG the day a patch target kept a differential oracle:
    /// `crates/aterm-alloc` dev-depends on registry `arrayvec =0.7.7` so its
    /// differential compares aterm's `ArrayVec` against real upstream code
    /// rather than against the shim (the same shape `aterm-digest` uses for
    /// sha2/hmac and `aterm-toml` for toml/toml_edit). That copy IS a
    /// registry-sourced sibling in the lock — and it is in NO shipped graph, so
    /// nothing about it is a shadow. `shadowed_by` cannot tell the two apart:
    /// the lock records no edge kinds, so a dev-only node and a shipped one
    /// look identical there.
    ///
    /// The obligation was always about what COMPILES INTO ATERM, so it is
    /// asserted where that is decidable — the per-cell `--edges normal` graph,
    /// which is `[OB-12]`'s own instrument. A lock-level sibling that appears
    /// in any cell's shipped graph still fails here, by name and cell.
    #[test]
    fn no_fork_is_shadowed_by_an_unpatched_registry_copy() {
        let root = repo_root();
        let entries = patch_entries(&root).unwrap();
        let siblings: Vec<_> = entries
            .iter()
            .filter(|e| !e.shadowed_by.is_empty())
            .map(|e| (e.name.clone(), e.shadowed_by.clone()))
            .collect();

        // The lock-level facts, pinned so a NEW sibling is a visible diff and
        // not something this test silently tolerates.
        assert_eq!(
            siblings,
            vec![("arrayvec".to_string(), vec!["0.7.7".to_string()])],
            "a registry copy of a patched name appeared in Cargo.lock. If it is              another dev-only oracle, add it here WITH the cell check below              proving it ships nowhere; if it is reached by a normal edge, the              fork's fix is absent from the copy that compiles."
        );

        // THE OBLIGATION: no sibling is in any cell's shipped graph.
        for cell in crate::resolve::default_cells() {
            let graph = crate::resolve::graph(&root, &cell)
                .unwrap_or_else(|e| panic!("cell `{}` must resolve: {e}", cell.name));
            for (name, versions) in &siblings {
                for v in versions {
                    let id = crate::model::PkgId::new(name.clone(), v.clone());
                    assert!(
                        !graph.nodes.contains(&id),
                        "cell `{}` ({}) resolves an UNPATCHED `{name} {v}` in its                          `--edges normal` graph, so the patch's replacement is absent                          from the copy that compiles. Find the edge with                          `cargo tree -p aterm -e normal --target {} -i {name}@{v}`.",
                        cell.name,
                        cell.triple,
                        cell.triple
                    );
                }
            }
        }
    }

    #[test]
    fn the_real_lock_is_read_as_toml() {
        let root = repo_root();
        let lock = lock_entries(&root).expect("the real lock reads");
        assert!(lock.len() > 500, "{} entries", lock.len());
        let forks = lock
            .iter()
            .filter(|e| e.source.is_none() && e.name == "winit")
            .count();
        assert_eq!(
            forks, 1,
            "the vendored winit is a path package with no source/checksum"
        );
    }

    #[test]
    fn the_seed_body_parses_back_as_a_policy_over_the_real_tree() {
        let root = repo_root();
        let body = seed_from_vendor(&root).expect("the real tree seeds");
        let p = parse(&body).expect("the emitted body is a valid ledger");
        // FIVE, not ten: the five first-party targets are deliberately not
        // seeded. The ledger parser refuses a `path` outside `vendor/`, so a
        // seeder that emitted one would write a file its own reader rejects —
        // which is exactly how this landed before the partition.
        assert_eq!(p.forks.len(), 5);
        for name in ["tracing", "profiling", "cfg-if", "arrayvec", "log"] {
            assert!(
                p.forks.iter().all(|f| f.name != name),
                "a first-party replacement is not a vendored fork: {name}"
            );
        }
        assert!(body.contains("NOT RECORDED HERE"), "{body}");
        // Named INDIVIDUALLY, not counted: the failure this guards against is a
        // seeder that omits one silently, and a length check would pass while
        // the omitted crate's patch entry went unrecorded and unexplained.
        for path in [
            "crates/aterm-tracing",
            "crates/aterm-profiling",
            "crates/aterm-cfg-if",
            "crates/aterm-arrayvec",
            "crates/aterm-log-shim",
        ] {
            assert!(
                body.contains(path),
                "the seed must SAY which patch entries it left out, and why; \
                 `{path}` is missing:\n{body}"
            );
        }
        let winit = p.fork("winit").expect("winit");
        assert!(winit.apache_notice, "winit is Apache-2.0 only");
        assert_eq!(winit.census_namespace.as_deref(), Some("winit"));
        let pc = p.fork("pkg-config").expect("pkg-config");
        assert_eq!(pc.census_mode, CensusMode::BuildDepOnly);
        assert!(!pc.apache_notice, "pkg-config is dual MIT OR Apache-2.0");
        // The seed carries the census's own review notes as comments.
        assert!(body.contains("# census review:"), "{body}");
        assert!(
            !body.contains("PATCH LIVENESS"),
            "no fork is shadowed today, so the seed must not claim one is:\n{body}"
        );
        // And the round-trip of the emitted body is byte-identical too.
        assert_eq!(p.render().unwrap(), body);
    }
}
