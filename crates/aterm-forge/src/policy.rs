// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `vendor/forge.toml`.

use aterm_census::scan_set::{PatchTargetKind, classify_patch_target};
use aterm_toml::edit::{DocumentMut, Item, TableLike};
use std::path::Path;

/// The ledger's path, relative to the workspace root.
pub const POLICY_PATH: &str = "vendor/forge.toml";

/// The one LOC method forge implements.
pub const LOC_METHOD: &str = "rs-physical-all-files-v1";

/// The one graph method forge implements.
pub const GRAPH_METHOD: &str = "cargo-tree-normal-no-dedupe-locked-offline-v1";

/// How `aterm-census`'s lock-order census classifies a vendored fork.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CensusMode {
    /// Links into the shipped GUI process.
    Scanned,
}

impl CensusMode {
    fn parse(s: &str, ctx: &str) -> Result<Self, String> {
        match s {
            "scanned" => Ok(Self::Scanned),
            other => Err(format!(
                "{POLICY_PATH}: {ctx} census.mode = \"{other}\" is not a mode — write \
                 \"scanned\" (the crate links into the shipped process; then census.namespace \
                 is required)"
            )),
        }
    }
}

/// One vendored, `[patch.crates-io]`-redirected upstream crate that aterm now maintains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fork {
    /// The crates.io package this fork replaces ([`patched_package`]).
    pub name: String,
    /// The upstream version the fork keeps, so the existing `^` requirements still resolve.
    pub version: String,
    /// Repo-relative directory (`vendor/indexmap`).
    pub path: String,
    /// The SPDX expression from the vendored manifest, verbatim.
    pub license: String,
    /// `true` when this fork's license leaves no non-Apache option.
    pub apache_notice: bool,
    /// The lock-order census's classification.
    pub census_mode: CensusMode,
    /// The identity namespace (`indexmap` ⇒ `indexmap::…`).
    pub census_namespace: Option<String>,
}

/// The `[forge]` header: the methods every number in a report was produced by.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForgeHeader {
    pub loc_method: String,
    pub graph_method: String,
}

/// The parsed ledger.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub forge: ForgeHeader,
    pub forks: Vec<Fork>,
}

/// Read `<root>/vendor/forge.toml`.
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

/// Parse a ledger body.
pub fn parse(text: &str) -> Result<Policy, String> {
    let doc: DocumentMut = text.parse().map_err(|e| {
        format!("{POLICY_PATH} is not valid TOML: {e} — fix the syntax; forge will not guess")
    })?;

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
             graph_method = \"{GRAPH_METHOD}\""
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
    check_keys(t, &["loc_method", "graph_method"], "[forge]")?;

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

    Ok(ForgeHeader {
        loc_method,
        graph_method,
    })
}

fn fork_block(t: &dyn TableLike, n: usize) -> Result<Fork, String> {
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
             \"{name}\"`"
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
    if census_namespace.is_none() {
        return Err(format!(
            "{POLICY_PATH}: {ctx} is census.mode = \"scanned\" but has no \
             census.namespace — a scanned fork's lock identities MUST live in a \
             per-crate namespace or a foreign receiver name can merge with an aterm \
             one. Add `census.namespace = \"{name}\"`"
        ));
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

/// One `Cargo.lock` package record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockEntry {
    pub name: String,
    pub version: String,
    /// `None` for a path package: a workspace member, or a `[patch]` fork.
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

/// The package a `[patch.crates-io]` row replaces.
pub fn patched_package<'a>(key: &'a str, row: &'a Item) -> &'a str {
    row.get("package").and_then(Item::as_str).unwrap_or(key)
}

/// The semver-compatibility class of a version or a requirement's version part.
pub fn semver_class(version: &str) -> String {
    let core = version.trim_start_matches(|c: char| "^=~<> ".contains(c));
    let core = core.split(['-', '+', ',', ' ']).next().unwrap_or_default();
    let mut parts = core.split('.');
    let major = parts.next().unwrap_or_default();
    if major != "0" {
        return major.to_string();
    }
    let minor = parts.next().unwrap_or_default();
    if minor != "0" {
        return format!("0.{minor}");
    }
    match parts.next() {
        Some(patch) => format!("0.0.{patch}"),
        None => "0.0".to_string(),
    }
}

/// The semver class of a plain version or a bare, caret or exact requirement (the forms attest's
/// `req_satisfied` models), else `None`.
fn known_semver_class(version: &str) -> Option<String> {
    let version = version.trim();
    let number = version
        .strip_prefix('^')
        .or_else(|| version.strip_prefix('='))
        .unwrap_or(version)
        .trim_start();
    let core = number.split(['-', '+']).next()?;
    let parts: Vec<_> = core.split('.').collect();
    if parts.is_empty()
        || parts.len() > 3
        || parts.iter().any(|p| {
            p.is_empty()
                || !p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 1 && p.starts_with('0'))
                || p.parse::<u64>().is_err()
        })
    {
        return None;
    }
    Some(semver_class(number))
}

/// Whether `version` of a patched package falls to a row whose own version is `row_version`.
pub fn in_row_scope(renamed: bool, row_version: Option<&str>, version: &str) -> bool {
    match row_version {
        Some(row) if renamed => match (known_semver_class(row), known_semver_class(version)) {
            (Some(row), Some(other)) => row == other,
            _ => true,
        },
        _ => true,
    }
}

/// One `[patch.crates-io]` path entry, measured against the tree and the lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchEntry {
    /// The crates.io package being replaced ([`patched_package`]).
    pub name: String,
    /// The patch table key: [`Self::name`] unless the row is renamed.
    pub key: String,
    /// Repo-relative replacement path, as written in the manifest.
    pub path: String,
    /// `version` from the vendored manifest.
    pub manifest_version: String,
    /// The SPDX expression from the vendored manifest.
    pub license: String,
    /// The version `Cargo.lock` resolved for the path copy of this package.
    pub lock_version: Option<String>,
    /// Registry-sourced versions of the same name that coexist with the fork.
    pub shadowed_by: Vec<String>,
    /// Third-party source under `vendor/`.
    pub kind: PatchTargetKind,
}

impl PatchEntry {
    /// The patch is in effect.
    pub fn is_live(&self) -> bool {
        self.lock_version.as_deref() == Some(self.manifest_version.as_str())
    }

    /// A vendored fork.
    pub fn is_vendored(&self) -> bool {
        self.kind == PatchTargetKind::Vendored
    }

    /// The row is keyed apart from the package it replaces, and so claims one version line of it.
    pub fn is_renamed(&self) -> bool {
        self.key != self.name
    }
}

/// Read `[patch.crates-io]` from `<root>/Cargo.toml` and measure each entry against the vendored.
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
    for (key, item) in pt.iter() {
        let name = patched_package(key, item);
        let spec = item.as_table_like().ok_or_else(|| {
            format!(
                "{}: `[patch.crates-io] {key}` is not a table — forge models path patches \
                 only: `{key} = {{ path = \"vendor/{key}\" }}`",
                manifest.display()
            )
        })?;
        for (k, _) in spec.iter() {
            if k != "path" && k != "package" {
                return Err(format!(
                    "{}: `[patch.crates-io] {key}` has key `{k}` — forge models path patches \
                     only (a git or version patch has no vendored tree to notarize), optionally \
                     renamed with `package`. Vendor it under `vendor/{key}` and write `{key} = \
                     {{ path = \"vendor/{key}\" }}`, or teach {POLICY_PATH} a new fork shape \
                     first",
                    manifest.display()
                ));
            }
        }
        let path = spec.get("path").and_then(Item::as_str).ok_or_else(|| {
            format!(
                "{}: `[patch.crates-io] {key}` has no `path` — write `{key} = {{ path = \
                 \"vendor/{key}\" }}`",
                manifest.display()
            )
        })?;

        let kind = classify_patch_target(key, path, root)?;
        let (pkg_name, manifest_version, license) = patched_manifest(root, path, kind)?;
        if pkg_name != name {
            return Err(format!(
                "{}/Cargo.toml declares package `{pkg_name}` but it is patched in as \
                 `{name}` — a patch must replace the package it names. Rename the vendored \
                 package back to `{name}`, or point the patch entry at the right directory",
                root.join(path).display()
            ));
        }

        let renamed = key != name;
        let mut lock_version = None;
        let mut shadowed_by = Vec::new();
        for e in lock.iter().filter(|e| e.name == name) {
            if e.source.is_none() {
                if e.version == manifest_version {
                    lock_version = Some(e.version.clone());
                } else {
                    lock_version.get_or_insert_with(|| e.version.clone());
                }
            } else if in_row_scope(renamed, Some(&manifest_version), &e.version) {
                shadowed_by.push(e.version.clone());
            }
        }
        out.push(PatchEntry {
            name: name.to_string(),
            key: key.to_string(),
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
    let inherited = |key: &str| -> Option<String> {
        if kind == PatchTargetKind::Vendored {
            return None;
        }
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
            PatchTargetKind::FirstParty | PatchTargetKind::Absorbed => format!(
                "{}: `[package] {key}` is missing — a member patch target may state it \
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

    const SAMPLE: &str = r#"# SPDX-License-Identifier: Apache-2.0
# a comment block that IS the record

[forge]
loc_method = "rs-physical-all-files-v1"
graph_method = "cargo-tree-normal-no-dedupe-locked-offline-v1"

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

# smol_str is dual-licensed, so it owes no §4(b) notices.
[[fork]]
name = "smol_str"
version = "0.2.2"
path = "vendor/smol_str"
license = "MIT OR Apache-2.0"
census.mode = "scanned"
census.namespace = "smol_str"
"#;

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
        let ss = p
            .forks
            .iter()
            .find(|f| f.name == "smol_str")
            .expect("smol_str block");
        assert!(!ss.apache_notice, "apache_notice defaults to false");
        assert_eq!(ss.census_namespace.as_deref(), Some("smol_str"));
    }

    /// The ledger is fail-closed: each drift a hand edit can introduce is
    /// refused, never quietly reinterpreted.
    #[test]
    fn a_ledger_that_does_not_parse_cleanly_is_refused() {
        let dup = format!(
            "{SAMPLE}\n[[fork]]\nname = \"winit\"\nversion = \"0.30.13\"\n\
             path = \"vendor/winit\"\nlicense = \"Apache-2.0\"\ncensus.mode = \"scanned\"\n\
             census.namespace = \"winit\"\n"
        );
        let headerless = format!("# --- winit{}", SAMPLE.split("# --- winit").nth(1).unwrap());
        for (name, text) in [
            (
                "an unknown key",
                SAMPLE.replace("apache_notice = true", "apache_notices = true"),
            ),
            (
                "an unknown top-level table",
                format!("{SAMPLE}\n[carve]\nwhen = \"soon\"\n"),
            ),
            (
                "a scanned fork without a namespace",
                SAMPLE.replace("census.namespace = \"winit\"\n", ""),
            ),
            (
                "a retired census mode",
                SAMPLE.replace(
                    "census.mode = \"scanned\"\ncensus.namespace = \"smol_str\"",
                    "census.mode = \"build-dep-only\"",
                ),
            ),
            (
                "a drifted method string",
                SAMPLE.replace(LOC_METHOD, "rs-logical-v2"),
            ),
            ("no [forge] header", headerless),
            ("two blocks for one package", dup),
        ] {
            assert_ne!(text, SAMPLE, "{name}: the plant must change the sample");
            assert!(parse(&text).is_err(), "{name}");
        }
    }

    #[test]
    fn a_semver_class_is_the_version_line_cargo_resolves_within() {
        for (v, class) in [
            ("1.4.2", "1"),
            ("^1.4", "1"),
            ("0.52.0", "0.52"),
            ("0.52", "0.52"),
            ("=0.52.0", "0.52"),
            ("0.61", "0.61"),
            ("0.0.3", "0.0.3"),
            ("2.0.16+zstd.1.5.7", "2"),
            ("0.3.0-alpha.1", "0.3"),
        ] {
            assert_eq!(semver_class(v), class, "{v}");
        }
    }

    #[test]
    fn only_a_renamed_row_with_a_known_version_is_scoped_to_its_line() {
        assert!(in_row_scope(true, Some("0.52.0"), "0.52.3"));
        assert!(!in_row_scope(true, Some("0.52.0"), "0.61.2"));
        assert!(!in_row_scope(true, Some("0.52.0"), "0.61"));
        assert!(in_row_scope(false, Some("0.52.0"), "0.61.2"));
        assert!(in_row_scope(true, None, "0.61.2"));
    }
}
