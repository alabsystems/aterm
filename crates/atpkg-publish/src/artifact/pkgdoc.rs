// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Writer and verifier for `pkg-<program>-<build>.toml` manifests.

use atpkg::manifest::Provenance;

use crate::api::{Build, Lineage};
use crate::error::{Error, Result};
use crate::triple::{AARCH64_APPLE_DARWIN, Triple};

/// Package manifest schema version produced by this writer.
pub const SCHEMA: i64 = 1;

/// Artifact payload shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Plain program `bin/` directory shimmed by `exposes`.
    Binary,
    /// Self-contained sysroot bundle relocated at pack time.
    SysrootBundle,
}

impl Kind {
    /// Manifest token for this payload kind.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::SysrootBundle => "sysroot-bundle",
        }
    }
}

/// Returns `pkg-<program>-<build>.toml`.
#[must_use]
pub fn manifest_name(program: &str, build: Build) -> String {
    format!("pkg-{program}-{build}.toml")
}

/// Returns `pkg-<program>-<build>.toml.sig`.
#[must_use]
pub fn sig_name(program: &str, build: Build) -> String {
    format!("pkg-{program}-{build}.toml.sig")
}

/// Returns the tarball asset name for `(program, build, triple)`.
#[must_use]
pub fn asset_name(program: &str, build: Build, triple: Triple) -> String {
    if triple == AARCH64_APPLE_DARWIN {
        format!("{program}-{build}.tar.zst")
    } else {
        format!("{program}-{build}-{triple}.tar.zst")
    }
}

/// Package manifest header fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// The program.
    pub program: String,
    /// Its version.
    pub version: String,
    /// Its build number.
    pub build: Build,
    /// The tools it shims.
    pub exposes: Vec<String>,
}

/// One target's `[[artifact]]` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The target.
    pub triple: Triple,
    /// The payload shape.
    pub kind: Kind,
    /// The release asset's file name.
    pub asset: String,
    /// The asset's sha256.
    pub sha256: String,
    /// The extracted tree's `tree_root`.
    pub tree_root: String,
    /// The asset's size.
    pub size: u64,
    /// `[artifact.cost] disk_installed`.
    pub disk_installed: u64,
    /// `[artifact.provenance]` compiler and scan record.
    pub provenance: Provenance,
}

/// Rendered manifest bytes and the prefix length of any base manifest it extends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    bytes: Vec<u8>,
    base_len: usize,
}

impl Doc {
    /// Exact manifest bytes to be signed.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Verbatim base manifest prefix (empty for a fresh manifest).
    #[must_use]
    pub fn base(&self) -> &[u8] {
        &self.bytes[..self.base_len]
    }

    /// Returns whether this document extends `base` as an exact byte prefix.
    #[must_use]
    pub fn extends(&self, base: &[u8]) -> bool {
        self.bytes.starts_with(base)
    }
}

/// Builds a [`Provenance`] record from the admitted `compiler` and `scan_objects` count.
#[must_use]
pub fn provenance(compiler: &atpkg::lineage::Compiler, scan_objects: usize) -> Provenance {
    let mut p = Provenance::default();
    for (k, v) in compiler.record() {
        let slot = match k {
            "compiler_release" => &mut p.compiler_release,
            "compiler_commit" => &mut p.compiler_commit,
            "compiler_trust" => &mut p.compiler_trust,
            "compiler_host" => &mut p.compiler_host,
            "trustc_sha256" => &mut p.trustc_sha256,
            "targo_sha256" => &mut p.targo_sha256,
            "driver_sha256" => &mut p.driver_sha256,
            "lineage" => &mut p.lineage,
            "lineage_receipt_sha256" => &mut p.lineage_receipt_sha256,
            "ddc_seed_relation" => &mut p.ddc_seed_relation,
            _ => continue,
        };
        *slot = v;
    }
    p.scan_objects = scan_objects.to_string();
    p.scan_findings = "0".to_string();
    p
}

/// Parses the [`Lineage`] variant from `p.lineage`.
///
/// # Errors
/// Returns an error if `p.lineage` is not a recognized lineage token.
pub fn lineage_of(p: &Provenance) -> Result<Lineage> {
    match p.lineage.as_str() {
        "lineage" => Ok(Lineage::Lineage),
        "seed-mint" => Ok(Lineage::SeedMint),
        "unproven" => Ok(Lineage::Unproven),
        other => Err(Error::refused(format!(
            "the provenance record's lineage `{other}` is not lineage | seed-mint | unproven"
        ))),
    }
}

fn provenance_fields(p: &Provenance) -> [(&'static str, &str); 12] {
    [
        ("compiler_release", &p.compiler_release),
        ("compiler_commit", &p.compiler_commit),
        ("compiler_trust", &p.compiler_trust),
        ("compiler_host", &p.compiler_host),
        ("trustc_sha256", &p.trustc_sha256),
        ("targo_sha256", &p.targo_sha256),
        ("driver_sha256", &p.driver_sha256),
        ("lineage", &p.lineage),
        ("lineage_receipt_sha256", &p.lineage_receipt_sha256),
        ("ddc_seed_relation", &p.ddc_seed_relation),
        ("scan_objects", &p.scan_objects),
        ("scan_findings", &p.scan_findings),
    ]
}

/// A value rendered into a TOML string without escaping: letters, digits and `._+-`.
fn plain(what: &str, v: &str) -> Result<()> {
    if !v.is_empty()
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
    {
        Ok(())
    } else {
        Err(Error::refused(format!(
            "{what} `{v}` is not a plain value (letters, digits, `._+-`)"
        )))
    }
}

/// Validates that `p` has plain TOML fields, a 40-hex commit, a known lineage, and zero findings.
///
/// # Errors
/// Returns an error describing the first invalid provenance field.
pub fn check_provenance(p: &Provenance) -> Result<()> {
    for (k, v) in provenance_fields(p) {
        plain(&format!("provenance `{k}`"), v)?;
    }
    if p.compiler_commit.len() != 40
        || !p
            .compiler_commit
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(Error::refused(
            "the provenance record's compiler_commit is not 40 hex",
        ));
    }
    lineage_of(p)?;
    if p.scan_findings != "0" {
        return Err(Error::refused(format!(
            "the provenance record carries {} scan finding(s) — a payload carrying another \
             compiler's evidence is never signed",
            p.scan_findings
        )));
    }
    Ok(())
}

fn render_row(row: &Row) -> Result<String> {
    check_provenance(&row.provenance)?;
    plain("the asset name", &row.asset)?;
    for (what, hex) in [("sha256", &row.sha256), ("tree_root", &row.tree_root)] {
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(Error::refused(format!(
                "the row's {what} `{hex}` is not 64 hex"
            )));
        }
    }
    if row.size == 0 {
        return Err(Error::refused(format!("{} is empty", row.asset)));
    }
    let mut s = format!(
        "[[artifact]]\ntarget = \"{}\"\nkind = \"{}\"\nprotocol = \"github-release\"\n\
         asset = \"{}\"\nsha256 = \"{}\"\ntree_root = \"{}\"\nsize = {}\n",
        row.triple,
        row.kind.as_str(),
        row.asset,
        row.sha256,
        row.tree_root,
        row.size
    );
    if row.kind == Kind::SysrootBundle {
        // The only relocation policy the client accepts, inside the signed bytes.
        s.push_str("reloc = \"self-contained\"\n");
    }
    s.push_str(&format!(
        "[artifact.cost]\ndownload_bytes = {}\ndisk_installed = {}\nbuild_seconds = 0\n\
         [artifact.provenance]\n",
        row.size, row.disk_installed
    ));
    for (k, v) in provenance_fields(&row.provenance) {
        s.push_str(&format!("{k} = \"{v}\"\n"));
    }
    Ok(s)
}

/// Validates that `exposes` is non-empty and every tool name is permitted by `shim_allowed`.
///
/// # Errors
/// Returns an error listing any disallowed tool names.
pub fn check_exposes(program: &str, exposes: &[String]) -> Result<()> {
    let refused: Vec<String> = exposes
        .iter()
        .filter(|e| !atpkg::store::shim_allowed(e))
        .map(|e| format!("`{}`", e.escape_debug()))
        .collect();
    if refused.is_empty() {
        return Ok(());
    }
    Err(Error::refused(format!(
        "{program} exposes {}: no shim may carry {} (a shell, another core command, or no \
         tool name; atpkg::store::shim_allowed), so a client would lay none and no manifest \
         naming {} is signed — expose only tool names",
        refused.join(", "),
        if refused.len() == 1 {
            "that name"
        } else {
            "those names"
        },
        if refused.len() == 1 { "it" } else { "them" },
    )))
}

/// Renders a fresh manifest containing `header` and `row`.
///
/// # Errors
/// Returns an error if any field, `exposes` list, or provenance record is invalid.
pub fn fresh(header: &Header, row: &Row) -> Result<Doc> {
    plain("the program", &header.program)?;
    plain("the version", &header.version)?;
    if header.exposes.is_empty() {
        return Err(Error::refused(format!(
            "{} exposes nothing: a pack that shims no tool is never signed",
            header.program
        )));
    }
    for e in &header.exposes {
        plain("an exposed tool", e)?;
    }
    check_exposes(&header.program, &header.exposes)?;
    let exposes = header
        .exposes
        .iter()
        .map(|e| format!("\"{e}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut s = format!(
        "# Generated by atpkg-publish — DO NOT edit (the signature covers the exact bytes).\n\
         schema = {SCHEMA}\nprogram = \"{}\"\nversion = \"{}\"\nbuild_number = {}\n\
         exposes = [{exposes}]\n\n",
        header.program, header.version, header.build
    );
    s.push_str(&render_row(row)?);
    Ok(Doc {
        bytes: s.into_bytes(),
        base_len: 0,
    })
}

/// The tools a manifest shims, as a set: the order a list was written in is no part of
/// what a client lays.
fn tool_set<'a>(tools: impl IntoIterator<Item = &'a str>) -> std::collections::BTreeSet<&'a str> {
    tools.into_iter().collect()
}

/// Appends `row` to an existing published manifest `base` as a byte-prefix extension.
///
/// # Errors
/// Returns an error if `base` fails to parse or conflicts with `header` or `row`.
pub fn merge(base: Vec<u8>, header: &Header, row: &Row) -> Result<Doc> {
    let (program, build) = (header.program.as_str(), header.build);
    check_exposes(program, &header.exposes)?;
    let text = std::str::from_utf8(&base)
        .map_err(|_| Error::refused("the merge base is not UTF-8 — not a manifest"))?;
    let doc: aterm_toml::Value = text
        .parse()
        .map_err(|e| Error::refused(format!("the merge base does not parse as TOML: {e}")))?;
    let schema = doc.get("schema").and_then(aterm_toml::Value::as_integer);
    if schema != Some(SCHEMA) {
        return Err(Error::refused(format!(
            "the merge base's schema is {schema:?}, not {SCHEMA} — not a document this \
             writer produces; refusing to append to it"
        )));
    }
    let base_program = doc.get("program").and_then(aterm_toml::Value::as_str);
    if base_program != Some(program) {
        return Err(Error::refused(format!(
            "the merge base is for program {base_program:?} but this pack is `{program}`"
        )));
    }
    let base_build = doc
        .get("build_number")
        .and_then(aterm_toml::Value::as_integer);
    if base_build.and_then(|b| u64::try_from(b).ok()) != Some(build) {
        return Err(Error::refused(format!(
            "the merge base is build {base_build:?} but this pack is build {build} — a merged \
             manifest describes ONE build"
        )));
    }
    let base_version = doc.get("version").and_then(aterm_toml::Value::as_str);
    if base_version != Some(header.version.as_str()) {
        return Err(Error::refused(format!(
            "the merge base is version {base_version:?} but this pack is `{}` — a merged \
             manifest describes ONE build",
            header.version
        )));
    }
    let base_exposes: Vec<&str> = doc
        .get("exposes")
        .and_then(aterm_toml::Value::as_array)
        .map(|a| a.iter().filter_map(aterm_toml::Value::as_str).collect())
        .unwrap_or_default();
    if tool_set(base_exposes.iter().copied()) != tool_set(header.exposes.iter().map(String::as_str))
    {
        return Err(Error::refused(format!(
            "the merge base shims [{}] but this {} pack shims [{}] — one build has one \
             exposed set, and a shim a target cannot serve breaks there",
            base_exposes.join(", "),
            row.triple,
            header.exposes.join(", ")
        )));
    }
    let rows = doc
        .get("artifact")
        .and_then(aterm_toml::Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let field = |r: &aterm_toml::Value, k: &str| {
        r.get(k)
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_string)
    };
    if rows
        .iter()
        .any(|r| field(r, "target").as_deref() == Some(row.triple.as_str()))
    {
        return Err(Error::refused(format!(
            "the merge base already carries an [[artifact]] row for {} — a duplicate leaves \
             which row a client honours undefined",
            row.triple
        )));
    }
    if rows
        .iter()
        .any(|r| field(r, "asset").as_deref() == Some(row.asset.as_str()))
    {
        return Err(Error::refused(format!(
            "the merge base already names asset `{}` — two rows, one file name: their \
             uploads would replace each other",
            row.asset
        )));
    }
    let rendered = render_row(row)?;
    let base_len = base.len();
    let mut bytes = base;
    bytes.push(b'\n');
    bytes.extend_from_slice(rendered.as_bytes());
    Ok(Doc { bytes, base_len })
}

/// Verifies that `parsed` matches `header` and `row`.
///
/// # Errors
/// Returns an error naming the first field that differs.
pub fn check_parsed(parsed: &atpkg::PkgManifest, header: &Header, row: &Row) -> Result<()> {
    if parsed.program != header.program || parsed.build_number != header.build {
        return Err(Error::refused(format!(
            "the parsed manifest is {}@{}, not {}@{}",
            parsed.program, parsed.build_number, header.program, header.build
        )));
    }
    if parsed.version != header.version
        || tool_set(parsed.exposes.iter().map(String::as_str))
            != tool_set(header.exposes.iter().map(String::as_str))
    {
        return Err(Error::refused(format!(
            "the client parses {} as version {} shimming [{}], not the packed {} shimming [{}]",
            header.program,
            parsed.version,
            parsed.exposes.join(", "),
            header.version,
            header.exposes.join(", ")
        )));
    }
    let got = parsed
        .artifacts
        .iter()
        .find(|a| a.target == row.triple.as_str())
        .ok_or_else(|| Error::refused(format!("the parsed manifest has no {} row", row.triple)))?;
    let same = got.asset == row.asset
        && got.sha256 == row.sha256
        && got.tree_root == row.tree_root
        && got.size == row.size
        && got.kind == row.kind.as_str()
        && got.cost.disk_installed == row.disk_installed
        && got.provenance.as_ref() == Some(&row.provenance);
    if same {
        Ok(())
    } else {
        Err(Error::refused(format!(
            "the client parses {}'s {} row differently from what was packed",
            header.program, row.triple
        )))
    }
}
