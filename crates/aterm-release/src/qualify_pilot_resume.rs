// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Continue a local experiment from validated receipts, retaining its resource-history seed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use aterm_json::{Value, json};

use super::Options;

const MANIFEST: &str = "experiment.json";
const HISTORY: &str = "test-history-seed.json";
const COMPLETE: &str = "complete.json";

pub(super) struct Plan {
    pub(super) history: String,
    pub(super) next: usize,
    spec: Value,
    history_sha256: String,
}

pub(super) fn spec(
    repo: &Path,
    source: &str,
    opts: &Options,
    slots: u32,
    order: &[&str],
) -> Result<Value, String> {
    let image = std::env::current_exe().map_err(|error| error.to_string())?;
    let environment = [
        "CARGO_HOME",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_BUILD_BUILD_DIR",
        "CARGO_BUILD_JOBS",
        "TRUST_STAGE2_BIN",
        "RUSTUP_TOOLCHAIN",
    ]
    .into_iter()
    .map(|key| match std::env::var(key) {
        Ok(value) => Ok((key, Some(value))),
        Err(std::env::VarError::NotPresent) => Ok((key, None)),
        Err(_) => Err(format!("non-text experiment environment variable {key}")),
    })
    .collect::<Result<Vec<_>, String>>()?;
    let takes = order
        .iter()
        .map(|mode| {
            let take = opts.take(mode)?;
            Ok(json!({
                "source":source, "slots":slots, "arm":mode, "mode":take.mode,
                "measured":*mode!="warmup", "verify":take.verify,
                "verify_sha256":digest(&take.verify)?,
                "tree_work":take.tree_work.as_str(),
                "build_return":take.build_return.as_str(),
                "queued_tree_share":take.queued_tree_share.as_str(),
                "slice_cache":if take.warm_slices {"warm"} else {"cold"},
                "tree_cache":if take.cold_tree {"cold"} else {"warm"},
                "expected_red":take.planted_red,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(json!({
        "context":{
            "source":source, "repo":absolute(repo)?, "slots":slots,
            "controller_sha256":digest(&image)?,
            "os":std::env::consts::OS, "arch":std::env::consts::ARCH,
            "environment":environment,
        },
        "takes":takes,
    }))
}

impl Plan {
    pub(super) fn prepare(
        root: &Path,
        spec: Value,
        previous: Option<&Path>,
        seed_from: Option<&Path>,
        default_history: String,
    ) -> Result<Self, String> {
        let parent = previous.map(absolute).transpose()?;
        let next = parent
            .as_deref()
            .map(|previous| completed_prefix(previous, &spec))
            .transpose()?
            .unwrap_or(0);
        if next == recipes(&spec)?.len() {
            return Err("the resumed experiment is already complete; no take started".into());
        }
        let history = match parent.as_deref().or(seed_from) {
            Some(origin) => frozen_history(origin, &spec["context"])?,
            None => default_history,
        };
        let _: Value = aterm_json::from_str(&history)
            .map_err(|error| format!("invalid experiment history seed: {error}"))?;
        crate::durable::write_fresh(&root.join(HISTORY), &history)
            .map_err(|error| error.to_string())?;
        let history_sha256 = digest(&root.join(HISTORY))?;
        let manifest = json!({
            "schema":1, "spec":spec, "parent":parent, "start_index":next,
            "history_sha256":history_sha256,
        });
        crate::durable::write_fresh(&root.join(MANIFEST), manifest.to_string())
            .map_err(|error| error.to_string())?;
        eprintln!("qualify-pilot-experiment: {manifest}");
        Ok(Self {
            history,
            next,
            spec,
            history_sha256,
        })
    }

    pub(super) fn complete(&self, index: usize, at: &Path) -> Result<(), String> {
        let receipt = receipt(at, &self.spec, index, &self.history_sha256)?;
        crate::durable::write_fresh(&at.join(COMPLETE), receipt.to_string())
            .map_err(|error| error.to_string())
    }
}

fn completed_prefix(previous: &Path, spec: &Value) -> Result<usize, String> {
    let mut chain = Vec::new();
    let mut seen = BTreeSet::new();
    let mut cursor = Some(previous.to_owned());
    let mut history_sha256 = None;
    while let Some(root) = cursor {
        let root = absolute(&root)?;
        if chain.len() == 64 || !seen.insert(root.clone()) {
            return Err("invalid experiment resume chain; no take started".into());
        }
        let manifest = read_manifest(&root)?;
        if manifest["spec"] != *spec {
            return Err(
                "resumed source, tools, workload or core width changed; no take started".into(),
            );
        }
        let seed = manifest["history_sha256"]
            .as_str()
            .ok_or("experiment has no history digest")?;
        if history_sha256
            .as_deref()
            .is_some_and(|expected| expected != seed)
        {
            return Err("experiment lineage changed its frozen history; no take started".into());
        }
        history_sha256 = Some(seed.to_owned());
        let _ = frozen_history(&root, &spec["context"])?;
        cursor = match &manifest["parent"] {
            Value::Null => None,
            Value::String(path) => Some(PathBuf::from(path)),
            _ => return Err("invalid experiment parent; no take started".into()),
        };
        chain.push((root, manifest));
    }
    let mut next = 0;
    for (root, manifest) in chain.into_iter().rev() {
        if manifest["start_index"].as_u64() != Some(next as u64) {
            return Err("experiment prefix changed; no take started".into());
        }
        let seed = manifest["history_sha256"]
            .as_str()
            .ok_or("experiment has no history digest")?;
        let mut gap = false;
        for (index, recipe) in recipes(spec)?.iter().enumerate().skip(next) {
            let at = take_dir(&root, index, recipe)?;
            if !at.join(COMPLETE).exists() {
                gap = true;
                continue;
            }
            if gap || read_json(&at.join(COMPLETE))? != receipt(&at, spec, index, seed)? {
                return Err(
                    "experiment completion receipt changed or has a gap; no take started".into(),
                );
            }
            next += 1;
        }
    }
    Ok(next)
}

fn receipt(at: &Path, spec: &Value, index: usize, history_sha256: &str) -> Result<Value, String> {
    let recipe = recipes(spec)?
        .get(index)
        .and_then(Value::as_object)
        .ok_or("invalid experiment take")?;
    let result = read_json(&at.join("result.json"))?;
    if recipe
        .iter()
        .any(|(key, value)| key != "expected_red" && result[key.as_str()] != *value)
    {
        return Err("experiment result identity changed".into());
    }
    let red = recipe
        .get("expected_red")
        .and_then(Value::as_bool)
        .ok_or("experiment take has no expected verdict")?;
    require_passing_result(&result, red)?;
    Ok(json!({
        "schema":1, "spec":spec, "index":index,
        "history_sha256":history_sha256,
        "result_sha256":digest(&at.join("result.json"))?,
    }))
}

fn require_passing_result(result: &Value, red: bool) -> Result<(), String> {
    let tree_ok = result["tree"].as_array().is_some_and(|rows| {
        !rows.is_empty()
            && rows.iter().all(|row| {
                row["success"].as_bool() == Some(!red) && row["expected_red"].as_bool() == Some(red)
            })
    });
    let serial_ok = if red {
        result["serial_export"].is_null()
            && result["planted_red"].as_str().is_some()
            && result["planted_failure_marker_s"]
                .as_f64()
                .is_some_and(|seconds| seconds >= 0.0)
    } else {
        result["planted_red"].is_null()
            && result["serial_export"]["success"].as_bool() == Some(true)
            && result["serial_export"]["staging_push"].as_bool() == Some(false)
    };
    if !tree_ok
        || !serial_ok
        || ["appkit", "export", "fixtures"]
            .iter()
            .any(|part| result[*part]["success"].as_bool() != Some(true))
        || result["export"]["staging_push"].as_bool() != Some(false)
        || !result["serial_staging_push_s"].is_null()
    {
        return Err("an incomplete or failed gate cannot receive a completion receipt".into());
    }
    super::require_unsigned_build(
        &result["build_child"],
        result["source"]
            .as_str()
            .ok_or("missing experiment source")?,
    )
}

fn frozen_history(root: &Path, context: &Value) -> Result<String, String> {
    let manifest = read_manifest(root)?;
    if manifest["spec"]["context"] != *context
        || manifest["history_sha256"].as_str() != Some(digest(&root.join(HISTORY))?.as_str())
    {
        return Err(
            "experiment history or its source/core context changed; no take started".into(),
        );
    }
    crate::durable::read_text(&root.join(HISTORY)).map_err(|error| error.to_string())
}

fn read_manifest(root: &Path) -> Result<Value, String> {
    let manifest = read_json(&root.join(MANIFEST))?;
    if manifest["schema"].as_u64() != Some(1) {
        return Err("unsupported experiment receipt schema; no take started".into());
    }
    Ok(manifest)
}

fn read_json(path: &Path) -> Result<Value, String> {
    aterm_json::from_str(&crate::durable::read_text(path).map_err(|error| error.to_string())?)
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn recipes(spec: &Value) -> Result<&Vec<Value>, String> {
    spec["takes"]
        .as_array()
        .filter(|takes| !takes.is_empty())
        .ok_or_else(|| "experiment has no takes".into())
}

fn take_dir(root: &Path, index: usize, recipe: &Value) -> Result<PathBuf, String> {
    let mode = recipe["arm"].as_str().ok_or("experiment take has no arm")?;
    if !["before", "after", "warmup", "current", "lent", "resumed"].contains(&mode) {
        return Err("invalid experiment arm".into());
    }
    Ok(root.join(format!("take{}-{mode}", index + 1)))
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn digest(path: &Path) -> Result<String, String> {
    crate::dmg::sha256_and_size(path).map(|(hash, _)| hash)
}

#[cfg(test)]
#[path = "qualify_pilot_resume_tests.rs"]
mod tests;
