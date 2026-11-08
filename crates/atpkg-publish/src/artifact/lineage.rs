// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The producer's half of LINEAGE: the payload scan over a staged tree ([`scan_tree`]) and the
//! `LINEAGE` stamp a signed bundle carries ([`stamp_with`]).

use std::path::Path;

use atpkg::lineage::{
    Finding, LINEAGE_FILE, LINEAGE_SCHEMA, LineagePolicy, LineageProof, SEED_MINT_FILE,
    admit_content_with, check_compiler_with_components, receipt_sha256, scan_if_compiled_object,
};

/// What [`scan_tree`] read.
#[derive(Debug, Default)]
pub struct ScanReport {
    /// Compiled objects read.
    pub objects: usize,
    /// Evidence of any compiler but the admitted one.
    pub findings: Vec<Finding>,
}

/// Scan every compiled object under `root` (regular files only, no symlink followed) for evidence
/// of any compiler but the one whose commit-hash is `admitted`.
/// # Errors
/// A directory or file that cannot be read — never a silent skip.
pub fn scan_tree(root: &Path, admitted: &str) -> Result<ScanReport, String> {
    let mut report = ScanReport::default();
    for rel in super::relocate::walk_files(root)? {
        let path = root.join(&rel);
        let Some(hits) = scan_if_compiled_object(&path, admitted)
            .map_err(|e| format!("{}: {e}", rel.display()))?
        else {
            continue;
        };
        report.objects += 1;
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

/// Write `LINEAGE` into a STAGED toolchain tree after its last byte-changing pass (strip, relocate,
/// re-sign), so the signed tree carries a record the client's `check_compiler_with` admits against
/// the SHIPPED bytes.
/// # Errors
/// Every refusal, one line each; nothing is written then.
pub fn stamp_with(
    stage_root: &Path,
    source_bin: &Path,
    probe: &dyn Fn(&Path) -> Result<String, String>,
) -> Result<String, Vec<String>> {
    stamp_with_frontend(stage_root, source_bin, probe, None)
}

/// [`stamp_with`] after revalidating the explicitly selected raw-source frontend.
/// Final staged bytes are still recorded only after every transform.
/// # Errors
/// Every refusal, one line each; nothing is written then.
pub fn stamp_with_frontend(
    stage_root: &Path,
    source_bin: &Path,
    probe: &dyn Fn(&Path) -> Result<String, String>,
    frontend: Option<&str>,
) -> Result<String, Vec<String>> {
    stamp_with_components(stage_root, source_bin, probe, frontend, None)
}

/// [`stamp_with`] after validating both explicitly selected raw components.
/// Final LINEAGE still binds the ordinary transformed output bytes.
/// # Errors
/// Every refusal, one line each; nothing is written then.
pub fn stamp_with_components(
    stage_root: &Path,
    source_bin: &Path,
    probe: &dyn Fn(&Path) -> Result<String, String>,
    frontend: Option<&str>,
    targo: Option<&str>,
) -> Result<String, Vec<String>> {
    let source =
        check_compiler_with_components(source_bin, LineagePolicy::Require, probe, frontend, targo)?;
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
    crate::lane::report::write_atomic(&dest, &text)
        .map_err(|e| vec![format!("write {}: {e}", dest.display())])?;
    Ok(text)
}
