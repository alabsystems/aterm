// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `pkg-<program>-<build>.toml` writer.
//!
//! One typed row (`target`, `kind` = binary | sysroot-bundle, `protocol =
//! "github-release"`, `asset`, `sha256`, `tree_root`, `size`, `reloc` for a bundle,
//! `[artifact.cost]`, `[artifact.provenance]` from the scan record), rendered once and used
//! by both modes:
//!
//! * fresh: the header (`schema`, `program`, `version`, `build_number`, `exposes`) and the
//!   row;
//! * merge: a published manifest's bytes verbatim, a newline, the row. Refused unless the
//!   base is this program, build and version, shims exactly this pack's tools (one build has
//!   one exposed set), carries no row for this target and names no asset of the same name.
//!   The result is a byte-prefix extension by construction, which the Linux lane's "no
//!   other machine extended it" check relies on.
//!
//! A signed manifest is parsed back through the client's verifier
//! ([`super::sign::Authority::verify_pkg`]) and held to what was packed ([`check_parsed`])
//! before it is written. This writer authors no `https` vendor rows or `shim_env`; a merge
//! carries a base's existing ones verbatim.

use atpkg::manifest::Provenance;

use crate::api::{Build, Lineage};
use crate::error::{Error, Result};
use crate::triple::{AARCH64_APPLE_DARWIN, Triple};

/// The pkg document schema this writer produces (`parse_pkg` rejects only newer ones).
pub const SCHEMA: i64 = 1;

/// A row's payload shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A plain program's `bin/` (shimmed by its `exposes`).
    Binary,
    /// A whole sysroot, relocated at pack time (`reloc = "self-contained"`).
    SysrootBundle,
}

impl Kind {
    /// The manifest's spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::SysrootBundle => "sysroot-bundle",
        }
    }
}

/// `pkg-<program>-<build>.toml`.
#[must_use]
pub fn file_name(program: &str, build: Build) -> String {
    format!("pkg-{program}-{build}.toml")
}

/// The asset name of one target's pack. `aarch64-apple-darwin` keeps the bare
/// `<program>-<build>.tar.zst` that published packs use; every other target embeds the
/// triple, so a second target at the same build lands beside it on the one release tag.
#[must_use]
pub fn asset_name(program: &str, build: Build, triple: Triple) -> String {
    if triple == AARCH64_APPLE_DARWIN {
        format!("{program}-{build}.tar.zst")
    } else {
        format!("{program}-{build}-{triple}.tar.zst")
    }
}

/// A manifest's header.
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
    /// `[artifact.provenance]`: the compiler that built the payload, as its scan recorded.
    pub provenance: Provenance,
}

/// A rendered manifest: the base it extends (empty when fresh) and the bytes, of which
/// the base is the prefix by construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    bytes: Vec<u8>,
    base_len: usize,
}

impl Doc {
    /// The document's exact bytes — what is signed.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The base it extends, verbatim (empty for a fresh document).
    #[must_use]
    pub fn base(&self) -> &[u8] {
        &self.bytes[..self.base_len]
    }

    /// Whether these bytes extend `base` without changing a byte of it.
    #[must_use]
    pub fn extends(&self, base: &[u8]) -> bool {
        self.bytes.starts_with(base)
    }
}

/// The provenance record of a payload: the admitted compiler's record and the scan's
/// count (a signed row records zero findings, always).
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

/// How a provenance record says its compiler's lineage was proven.
///
/// # Errors
///
/// A refusal for a `lineage` that is none of the three.
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

/// The record a release row is signed with must be one: every key a plain value, a
/// 40-hex compiler commit, a known lineage, and zero findings — a payload carrying
/// another compiler's evidence is never signed.
///
/// # Errors
///
/// The first thing wrong with it.
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

/// A fresh manifest: the header and this row.
///
/// # Errors
///
/// A refusal for a header or row value that is not plain, a record that is not one, or
/// an empty `exposes`.
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

/// `base` (a published manifest, verbatim) extended with this target's row, so one build
/// number serves every target.
///
/// # Errors
///
/// A refusal when the base does not parse, is another schema, program, build or version,
/// shims another set of tools than this pack's `header` (one build, one exposed set: a
/// shim the appended target cannot serve would break there), already carries a row for
/// this target, or already names this asset (two rows, one release asset: their uploads
/// would replace each other).
pub fn merge(base: Vec<u8>, header: &Header, row: &Row) -> Result<Doc> {
    let (program, build) = (header.program.as_str(), header.build);
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
    // A lone newline separates without reflowing: the base bytes stay untouched, and a
    // base lacking a trailing newline still ends cleanly.
    bytes.push(b'\n');
    bytes.extend_from_slice(rendered.as_bytes());
    Ok(Doc { bytes, base_len })
}

/// The self-check after a manifest is parsed back: its header (program, build, version,
/// exposed set) and its `triple` row (asset, digests, size, kind, provenance) must be what
/// was packed.
///
/// # Errors
///
/// A refusal naming what differs.
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
