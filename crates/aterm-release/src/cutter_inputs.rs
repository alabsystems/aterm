// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Adoption evidence from the selected Cargo graph. Cargo still checks freshness;
//! this digest never replaces its build or licenses an unknown build-script input.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aterm_digest::Sha256;
use aterm_json::Value;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Inputs {
    pub digest: String,
    pub sources: String,
    pub seal: String,
}

pub(crate) struct Observation {
    pub inputs: Inputs,
    pub all_fresh: bool,
}

/// Read every declared source and build-script input of a successful Cargo build.
/// Unknown evidence refuses adoption, while the caller can still freeze that build normally.
pub(crate) fn capture(repo: &Path, output: &[u8], seal: &str) -> Result<Observation, String> {
    let mut graph = BTreeSet::new();
    let mut roots = BTreeSet::new();
    let mut packages = BTreeMap::new();
    let mut selected = BTreeSet::new();
    let mut scripts = Vec::new();
    let mut all_fresh = true;
    for line in output.split(|b| *b == b'\n') {
        let Ok(mut row) = aterm_json::from_slice::<Value>(line) else {
            continue;
        };
        match row["reason"].as_str() {
            Some("compiler-artifact") => {
                all_fresh &= row["fresh"].as_bool().ok_or("missing Cargo freshness")?;
                let id = string(&row, "package_id")?.to_owned();
                let manifest = PathBuf::from(string(&row, "manifest_path")?);
                let package = manifest.parent().ok_or("no package directory")?.to_owned();
                let text = std::fs::read_to_string(&manifest).map_err(|e| e.to_string())?;
                let toml: aterm_toml::Value =
                    aterm_toml::from_str(&text).map_err(|e| e.to_string())?;
                let name = toml["package"]["name"].as_str().ok_or("no package name")?;
                let version = id
                    .rsplit('#')
                    .next()
                    .and_then(|s| s.rsplit('@').next())
                    .filter(|s| s.as_bytes().first().is_some_and(u8::is_ascii_digit))
                    .ok_or("unknown Cargo package identity")?;
                selected.insert((name.to_owned(), version.to_owned()));
                packages.insert(id, package);
                roots.insert(manifest);
                let files = row["filenames"].as_array().ok_or("no compiler outputs")?;
                let dirs: BTreeSet<_> = files
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|p| Path::new(p).parent().map(Path::to_owned))
                    .collect();
                if dirs.is_empty() {
                    return Err("no compiler output directory".into());
                }
                for dir in dirs {
                    let mut found = false;
                    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
                        let path = entry.map_err(|e| e.to_string())?.path();
                        if path.extension().is_some_and(|e| e == "d") {
                            found = true;
                            let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                            graph.insert(text.clone());
                            roots.extend(crate::cutter::declared_sources(&text)?.into_iter().map(
                                |path| {
                                    if path.is_absolute() {
                                        path
                                    } else {
                                        repo.join(path)
                                    }
                                },
                            ));
                        }
                    }
                    if !found {
                        return Err("compiler output has no source dep-info".into());
                    }
                    let has_fingerprint = dir
                        .parent()
                        .is_some_and(|unit| unit.join("fingerprint").is_dir());
                    if has_fingerprint {
                        fingerprint(&dir, &mut graph, None, &mut roots)?;
                    // Published executables have aggregate dep-info in target/. Their
                    // producer identity authenticates this shape; sources and config
                    // still enter the digest, including every script's control output.
                    } else if !(row["target"]["kind"]
                        .as_array()
                        .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin")))
                        && row["executable"]
                            .as_str()
                            .is_some_and(|path| Path::new(path).parent() == Some(dir.as_path()))
                        && row["trust_unit_identity_sha256"]
                            .as_str()
                            .is_some_and(|id| aterm_codec::hex::is_lower(id, 64)))
                    {
                        return Err(format!(
                            "no compiler fingerprint or producer identity for {}",
                            dir.display()
                        ));
                    }
                }
                let artifact = row.as_object_mut().ok_or("artifact is not an object")?;
                artifact.remove("fresh");
                // This is the producer's whole-source observation identity, not a
                // compiler-input key: a runtime-only workspace version changes it.
                // Keep the actual package/configuration/fingerprint/source evidence.
                artifact.remove("trust_unit_identity_sha256");
                graph.insert(row.to_string());
            }
            Some("build-script-executed") => scripts.push(row),
            _ => {}
        }
    }
    if packages.is_empty() {
        return Err("no compiler graph".into());
    }
    for script in scripts {
        let package = packages
            .get(string(&script, "package_id")?)
            .ok_or("unknown build-script package")?;
        let out = PathBuf::from(string(&script, "out_dir")?);
        fingerprint(&out, &mut graph, Some(package), &mut roots)?;
        // Cargo's control output and generated tree are build inputs too. The pinned
        // driver does not advertise native integrity checks for these cached files.
        roots.insert(
            out.parent()
                .ok_or("no build-script unit")?
                .join("run/stdout"),
        );
        roots.insert(out);
        graph.insert(script.to_string());
    }
    // Workspace identity is runtime data. All other manifest data remains an input.
    // An actual include/rerun read of this manifest also enters `roots` unnormalized.
    let text = std::fs::read_to_string(repo.join("Cargo.toml")).map_err(|e| e.to_string())?;
    let mut manifest: aterm_toml::Value = aterm_toml::from_str(&text).map_err(|e| e.to_string())?;
    if let Some(workspace) = manifest
        .get("workspace")
        .and_then(aterm_toml::Value::as_table)
    {
        let mut workspace = workspace.clone();
        if let Some(package) = workspace
            .get("package")
            .and_then(aterm_toml::Value::as_table)
        {
            let mut package = package.clone();
            package.remove("version");
            workspace.insert("package".into(), aterm_toml::Value::Table(package));
        }
        if let aterm_toml::Value::Table(root) = &mut manifest {
            root.insert("workspace".into(), aterm_toml::Value::Table(workspace));
        }
    }
    graph.insert(aterm_toml::to_string(&manifest).map_err(|e| e.to_string())?);
    let lock: aterm_toml::Value = aterm_toml::from_str(
        &std::fs::read_to_string(repo.join("Cargo.lock")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut locked = BTreeSet::new();
    for package in lock
        .get("package")
        .and_then(aterm_toml::Value::as_array)
        .ok_or("no locked packages")?
    {
        let name = package
            .get("name")
            .and_then(aterm_toml::Value::as_str)
            .ok_or("no locked name")?;
        let version = package
            .get("version")
            .and_then(aterm_toml::Value::as_str)
            .ok_or("no locked version")?;
        if selected.contains(&(name.to_owned(), version.to_owned())) {
            locked.insert((name.to_owned(), version.to_owned()));
            graph.insert(aterm_toml::to_string(package).map_err(|e| e.to_string())?);
        }
    }
    if selected != locked {
        return Err("compiler graph is not covered by the lockfile".into());
    }
    for name in ["trust.lock", "rust-toolchain.toml"] {
        if repo.join(name).exists() {
            roots.insert(repo.join(name));
        }
    }
    for folder in repo
        .ancestors()
        .map(Path::to_owned)
        .chain(std::env::var_os("HOME").map(PathBuf::from))
    {
        for name in [".cargo/config", ".cargo/config.toml"] {
            if folder.join(name).exists() {
                roots.insert(folder.join(name));
            }
        }
    }
    let mut sources = Sha256::new();
    sources.update(b"cutter-sources/1\0");
    for root in roots {
        hash_path(&root, &mut sources, &mut BTreeSet::new())?;
    }
    let sources = aterm_codec::hex::encode_lower(&sources.finalize());
    let mut digest = Sha256::new();
    digest.update(b"cutter-inputs/1\0");
    frame(&mut digest, seal.as_bytes());
    frame(&mut digest, sources.as_bytes());
    for row in graph {
        frame(&mut digest, row.as_bytes());
    }
    Ok(Observation {
        inputs: Inputs {
            digest: aterm_codec::hex::encode_lower(&digest.finalize()),
            sources,
            seal: seal.to_owned(),
        },
        all_fresh,
    })
}

fn string<'a>(row: &'a Value, key: &str) -> Result<&'a str, String> {
    row[key]
        .as_str()
        .ok_or_else(|| format!("missing Cargo {key}"))
}

fn frame(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

/// Trust's pinned layout keeps each unit's fingerprint beside its output directory.
/// Missing/unknown records fail closed; no guessed source list licenses adoption.
fn fingerprint(
    out: &Path,
    graph: &mut BTreeSet<String>,
    package: Option<&Path>,
    roots: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    let folder = out.parent().ok_or("no unit directory")?.join("fingerprint");
    let mut found = false;
    for entry in std::fs::read_dir(folder).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if !path.extension().is_some_and(|e| e == "json") {
            continue;
        }
        found = true;
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let value: Value = aterm_json::from_str(&text).map_err(|e| e.to_string())?;
        graph.insert(value.to_string());
        if let Some(package) = package {
            for local in value["local"]
                .as_array()
                .ok_or("missing build-script input declarations")?
            {
                if let Some(changed) = local.get("RerunIfChanged") {
                    for path in changed["paths"]
                        .as_array()
                        .ok_or("unknown build-script paths")?
                    {
                        roots.insert(
                            package.join(path.as_str().ok_or("non-string build-script path")?),
                        );
                    }
                } else if local.get("RerunIfEnvChanged").is_some() {
                    // Cargo checked the child environment; its recorded value is in `graph`.
                } else if local.get("Precalculated").is_some() {
                    roots.insert(package.to_owned());
                } else {
                    return Err("unknown build-script input declaration".into());
                }
            }
        }
    }
    if !found {
        return Err("no compiler fingerprint".into());
    }
    Ok(())
}

fn hash_path(path: &Path, hash: &mut Sha256, active: &mut BTreeSet<PathBuf>) -> Result<(), String> {
    use aterm_toolchain::stat_digest::StatKey;
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let before = StatKey::of(&meta);
    frame(hash, path.to_str().ok_or("non-UTF8 input path")?.as_bytes());
    frame(hash, &before.mode.to_le_bytes());
    if meta.file_type().is_symlink() {
        frame(hash, b"link");
        frame(
            hash,
            std::fs::read_link(path)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("non-UTF8 link target")?
                .as_bytes(),
        );
        let target = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        hash_path(&target, hash, active)?;
    } else if meta.is_file() {
        frame(hash, b"file");
        frame(hash, crate::dmg::sha256_file(path)?.as_bytes());
    } else if meta.is_dir() {
        frame(hash, b"directory");
        let physical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        if !active.insert(physical.clone()) {
            return Err("input directory cycle".into());
        }
        let entries: Result<BTreeSet<_>, _> = std::fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect();
        for entry in entries.map_err(|e| e.to_string())? {
            hash_path(&entry, hash, active)?;
        }
        active.remove(&physical);
    } else {
        return Err("non-file compiler input".into());
    }
    let after = StatKey::of(&std::fs::symlink_metadata(path).map_err(|e| e.to_string())?);
    if !before.unchanged(&after) {
        return Err("compiler input changed while hashing".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "cutter_inputs_tests.rs"]
mod tests;
