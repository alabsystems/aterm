// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Package receipts from a completed Cargo freshness validation. These never bypass Cargo:
//! build-script rerun rules, external inputs and dependency fingerprints are checked on every
//! preflight. A fresh compiler unit reuses its prior Tippy judgment without running Tippy again.

use std::collections::BTreeMap;
use std::path::Path;

use crate::{Ctx, compiler_message::UnitMessage, input_content::Proof};

#[derive(Default)]
pub(crate) struct Packages {
    packages: BTreeMap<String, Package>,
    tainted: bool,
}

#[derive(Default)]
struct Package {
    units: BTreeMap<String, String>,
    fresh: bool,
    initialized: bool,
    complete: bool,
}

impl Packages {
    pub(crate) fn observe(&mut self, ctx: &Ctx, text: &str) {
        for line in text.lines() {
            let value = match aterm_json::from_str::<UnitMessage>(line) {
                Ok(UnitMessage::Unit(value)) => value,
                Ok(UnitMessage::Ambiguous {
                    artifact_reason_seen: true,
                    ..
                }) => {
                    self.tainted = true;
                    continue;
                }
                Err(_) if line.contains("compiler-artifact") => {
                    self.tainted = true;
                    continue;
                }
                _ => continue,
            };
            if value["reason"].as_str() != Some("compiler-artifact") {
                continue;
            }
            let Some(manifest) = value["manifest_path"].as_str() else {
                continue;
            };
            let Some(root) = Path::new(manifest).parent() else {
                continue;
            };
            if !root.starts_with(&ctx.root) {
                continue;
            }
            let Some(package) = value["package_id"].as_str() else {
                continue;
            };
            let entry = self.packages.entry(package.into()).or_default();
            if !entry.initialized {
                entry.fresh = true;
                entry.complete = true;
                entry.initialized = true;
            }
            entry.fresh &= value["fresh"].as_bool() == Some(true);
            let Some(unit) = value["trust_unit_identity_sha256"].as_str() else {
                entry.complete = false;
                continue;
            };
            if !aterm_codec::hex::is_lower(unit, 64) {
                entry.complete = false;
                continue;
            }
            let Some(files) = value["filenames"].as_array() else {
                entry.complete = false;
                continue;
            };
            let mut hash = aterm_digest::Sha256::new();
            let mut any = false;
            for file in files.iter().filter_map(|v| v.as_str()) {
                let file = Path::new(file);
                let Some(fingerprint) = fingerprint(ctx, file) else {
                    entry.complete = false;
                    continue;
                };
                hash.update(fingerprint.as_bytes());
                any = true;
            }
            if any {
                let digest = aterm_codec::hex::encode_lower(&hash.finalize());
                if entry
                    .units
                    .insert(unit.into(), digest.clone())
                    .is_some_and(|old| old != digest)
                {
                    entry.complete = false;
                }
            } else {
                entry.complete = false;
            }
        }
    }

    pub(crate) fn finish(self, ctx: &Ctx, report: &mut crate::ladder::Report) {
        let mut reused = 0;
        let mut judged = 0;
        for (package, state) in self.packages {
            let inputs = state
                .units
                .into_iter()
                .map(|(unit, digest)| ("compiler-unit".into(), unit, digest))
                .collect();
            let proof = (state.complete && !self.tainted)
                .then(|| Proof::new(ctx, "lint", &package, inputs))
                .flatten();
            let carried = state.fresh
                && proof
                    .as_ref()
                    .is_some_and(|proof| ctx.input_items.iter().any(|old| old == proof));
            if carried {
                reused += 1;
            } else {
                judged += 1;
            }
            if let Some(proof) = proof {
                report.raw(crate::input_content::event(ctx, &proof, carried));
            }
        }
        report.pass(format!(
            "lint packages: {reused} reused by Cargo-validated fingerprints, {judged} judged"
        ));
    }
}

/// The pinned Cargo layout names each compiler output under `build/<package>/<unit>/out`.
/// Unknown layouts stay unbounded. Both the actual output bytes and Cargo's complete local
/// fingerprint files participate; current Cargo freshness is separately mandatory for reuse.
fn fingerprint(ctx: &Ctx, artifact: &Path) -> Option<String> {
    let physical = artifact.canonicalize().ok()?;
    let lane = crate::plan::lane_dir(ctx, crate::plan::Lane::TippyTarget);
    let allowed = std::iter::once(&ctx.target_dir)
        .chain(lane.iter())
        .filter_map(|path| path.canonicalize().ok())
        .any(|root| physical.starts_with(root));
    if !allowed {
        return None;
    }
    let out = physical.parent()?;
    if out.file_name()? != "out" {
        return None;
    }
    let directory = out.parent()?.join("fingerprint");
    if !directory.symlink_metadata().ok()?.file_type().is_dir() {
        return None;
    }
    let mut files = std::fs::read_dir(directory)
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    files.sort_by_key(std::fs::DirEntry::file_name);
    let mut hash = aterm_digest::Sha256::new();
    hash.update(crate::test_index::digest(&physical).ok()?.as_bytes());
    for file in files {
        if !file.file_type().ok()?.is_file() {
            return None;
        }
        hash.update(file.file_name().as_encoded_bytes());
        hash.update(crate::test_index::digest(&file.path()).ok()?.as_bytes());
    }
    Some(aterm_codec::hex::encode_lower(&hash.finalize()))
}
