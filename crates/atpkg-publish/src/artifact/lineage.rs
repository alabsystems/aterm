// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The producer's half of LINEAGE: the payload scan over a staged tree ([`scan_tree`]) and
//! the `LINEAGE` stamp a signed bundle carries ([`stamp_with`]).
//!
//! The rules are the client's (`atpkg::lineage`): one object's evidence is
//! `atpkg::lineage::scan_object`, a compiler is admitted by
//! `atpkg::lineage::check_compiler_with`, and the record written here is the one that
//! function admits against the shipped bytes. Only the walk over a whole tree and the
//! writing of the record are producer work, so only they live here.

use std::io::Read as _;
use std::path::Path;

use atpkg::lineage::{
    Finding, LINEAGE_FILE, LINEAGE_SCHEMA, LineagePolicy, LineageProof, SEED_MINT_FILE,
    admit_content_with, check_compiler_with, receipt_sha256, scan_object,
};

/// What [`scan_tree`] read.
#[derive(Debug, Default)]
pub struct ScanReport {
    /// Compiled objects read.
    pub objects: usize,
    /// Evidence of any compiler but the admitted one. Empty ⇒ the tree passes.
    pub findings: Vec<Finding>,
}

/// Whether `path` is compiled output the scan reads: by head (Mach-O, ELF, PE, wasm, an
/// ar archive, crate metadata) or by name (`.rlib`/`.rmeta`/`.a`/`.o`/`.so`/`.dylib`/
/// `.dll`/`.wasm`).
fn is_compiled_output(path: &Path) -> bool {
    let named = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e,
            "rlib" | "rmeta" | "a" | "o" | "so" | "dylib" | "dll" | "wasm"
        )
    }) || path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains(".so."));
    if named {
        return true;
    }
    let mut head = Vec::with_capacity(8);
    if std::fs::File::open(path)
        .and_then(|f| f.take(8).read_to_end(&mut head))
        .is_err()
    {
        return false;
    }
    atpkg::macho::is_macho_header(&head)
        || head.starts_with(b"\x7fELF")
        || head.starts_with(b"MZ")
        || head.starts_with(b"\0asm")
        || head.starts_with(b"!<arch>\n")
        || head.starts_with(b"rust\0\0\0")
}

/// Scan every compiled object under `root` (regular files only, no symlink followed)
/// for evidence of any compiler but the one whose commit-hash is `admitted`.
///
/// # Errors
/// A directory or file that cannot be read — never a silent skip.
pub fn scan_tree(root: &Path, admitted: &str) -> Result<ScanReport, String> {
    let mut report = ScanReport::default();
    for rel in super::relocate::walk_files(root)? {
        let path = root.join(&rel);
        if !is_compiled_output(&path) {
            continue;
        }
        report.objects += 1;
        let hits = scan_object(&path, admitted).map_err(|e| format!("{}: {e}", rel.display()))?;
        for (evidence, count) in hits {
            report.findings.push(Finding {
                file: rel.to_string_lossy().into_owned(),
                evidence,
                count,
            });
        }
    }
    Ok(report)
}

/// Write `LINEAGE` into a STAGED toolchain tree after its last byte-changing pass
/// (strip, relocate, re-sign), so the signed tree carries a record the client's
/// `check_compiler_with` admits against the SHIPPED bytes. `probe` answers `trustc -vV`.
///
/// `source` must be admitted with its lineage PROVEN (a SEED-MINT checked against the
/// unstripped seal, or a store build's own LINEAGE); the staged compiler must be the
/// same commit, and the staged tree's SEED-MINT the source's.
///
/// # Errors
/// Every refusal, one line each; nothing is written then.
pub fn stamp_with(
    stage_root: &Path,
    source_bin: &Path,
    probe: &dyn Fn(&Path) -> Result<String, String>,
) -> Result<String, Vec<String>> {
    let source = check_compiler_with(source_bin, LineagePolicy::Require, probe)?;
    let (receipt_sha256_of_source, relation) = match &source.lineage {
        LineageProof::Lineage {
            receipt_sha256,
            relation,
        }
        | LineageProof::SeedMint {
            receipt_sha256,
            relation,
        } => (receipt_sha256.clone(), relation.clone()),
        LineageProof::Unproven => {
            return Err(vec![
                "the source's lineage is unproven: nothing to stamp".to_string(),
            ]);
        }
    };
    let staged = admit_content_with(&stage_root.join("bin"), probe).map_err(|lines| {
        lines
            .into_iter()
            .map(|l| format!("the staged tree: {l}"))
            .collect::<Vec<_>>()
    })?;
    let mut refusals = Vec::new();
    if staged.version.commit != source.content.version.commit {
        refusals.push(format!(
            "the staged bin/trustc reports {}, the source {}",
            staged.version.commit, source.content.version.commit
        ));
    }
    match receipt_sha256(&staged.root.join(SEED_MINT_FILE)) {
        Some(mint) if mint == receipt_sha256_of_source => {}
        Some(mint) => refusals.push(format!(
            "the staged SEED-MINT is {mint}, not the source's {receipt_sha256_of_source}"
        )),
        None => refusals.push("the staged tree carries no SEED-MINT".to_string()),
    }
    if !refusals.is_empty() {
        return Err(refusals);
    }
    let text = format!(
        "# {LINEAGE_FILE}: the compiler this tree ships and the pack-cut receipt that proves \
         its lineage,\n\
         # written after strip and relocate by atpkg-publish pack-bundle;\n\
         # the signed tree_root covers it, and the client's lineage check admits it.\n\
         schema={LINEAGE_SCHEMA}\n\
         compiler_commit={}\n\
         compiler_release={}\n\
         lineage_receipt={SEED_MINT_FILE}\n\
         lineage_receipt_sha256={receipt_sha256_of_source}\n\
         ddc_seed_relation={relation}\n\
         trustc_sha256={}\n\
         targo_sha256={}\n\
         driver={} {}\n",
        staged.version.commit,
        staged.version.release,
        staged.trustc_sha256,
        staged.targo_sha256,
        staged.driver_sha256,
        staged.driver_rel,
    );
    let dest = staged.root.join(LINEAGE_FILE);
    let tmp = staged.root.join(format!(".{LINEAGE_FILE}.atpkg-stamp"));
    std::fs::write(&tmp, &text)
        .and_then(|()| std::fs::rename(&tmp, &dest))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            vec![format!("write {}: {e}", dest.display())]
        })?;
    Ok(text)
}
