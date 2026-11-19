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
    let mut script_targets = BTreeMap::new();
    let mut selected = BTreeSet::new();
    let mut scripts = Vec::new();
    let mut executed_scripts = BTreeSet::new();
    let mut all_fresh = true;
    for line in output.split(|b| *b == b'\n') {
        let Ok(mut row) = aterm_json::from_slice::<Value>(line) else {
            continue;
        };
        match row["reason"].as_str() {
            Some("compiler-artifact") => {
                all_fresh &= row["fresh"].as_bool().ok_or("missing Cargo freshness")?;
                let id = string(&row, "package_id")?.to_owned();
                let (fingerprint_name, is_script) = selected_target(&row)?;
                if is_script
                    && script_targets
                        .insert(id.clone(), fingerprint_name.clone())
                        .is_some_and(|previous| previous != fingerprint_name)
                {
                    return Err("ambiguous selected build-script target".into());
                }
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
                let files: BTreeSet<PathBuf> = files
                    .iter()
                    .map(|file| {
                        file.as_str()
                            .map(PathBuf::from)
                            .ok_or("non-string compiler output")
                    })
                    .collect::<Result<_, _>>()?;
                if files.is_empty() {
                    return Err("no compiler output directory".into());
                }
                for file in &files {
                    let dir = file.parent().ok_or("no compiler output directory")?;
                    let has_fingerprint = dir
                        .parent()
                        .is_some_and(|unit| unit.join("fingerprint").is_dir());
                    let dep = if has_fingerprint {
                        fingerprint(dir, &fingerprint_name, &mut graph, None, &mut roots)?;
                        selected_dep_info(&row, file, false)?
                    // Published executables have aggregate dep-info in target/. Their
                    // producer identity authenticates this shape; sources and config
                    // still enter the digest, including every script's control output.
                    } else if row["target"]["kind"]
                        .as_array()
                        .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin")))
                        && row["executable"]
                            .as_str()
                            .is_some_and(|path| Path::new(path) == file)
                        && row["trust_unit_identity_sha256"]
                            .as_str()
                            .is_some_and(|id| aterm_codec::hex::is_lower(id, 64))
                    {
                        selected_dep_info(&row, file, true)?
                    } else {
                        published_library(&row, &files, file, &mut graph, &mut roots)?;
                        file.with_extension("d")
                    };
                    let text = crate::durable::read_text(&dep)
                        .map_err(|e| format!("{}: {e}", dep.display()))?;
                    graph.insert(text.clone());
                    roots.extend(
                        crate::cutter::declared_sources(&text)?
                            .into_iter()
                            .map(|path| {
                                if path.is_absolute() {
                                    path
                                } else {
                                    repo.join(path)
                                }
                            }),
                    );
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
        let id = string(&script, "package_id")?;
        let package = packages.get(id).ok_or("unknown build-script package")?;
        let target = script_targets
            .get(id)
            .ok_or("no selected build-script compiler artifact")?;
        executed_scripts.insert(id.to_owned());
        let out = PathBuf::from(string(&script, "out_dir")?);
        fingerprint(
            &out,
            &format!("run-{target}"),
            &mut graph,
            Some(package),
            &mut roots,
        )?;
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
    if script_targets
        .keys()
        .any(|id| !executed_scripts.contains(id))
    {
        return Err("selected build-script has no execution record".into());
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
    digest.update(b"cutter-inputs/2\0");
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

/// The pinned driver's fingerprint_file_path uses the target description and literal
/// target name, not the crate name or an arbitrary JSON sibling. This capture is for
/// the cutter's build mode; other modes and unknown target kinds decline adoption.
fn selected_target(row: &Value) -> Result<(String, bool), String> {
    if row["profile"]["test"].as_bool() != Some(false) {
        return Err("unknown cutter compiler mode".into());
    }
    let target = &row["target"];
    let name = string(target, "name")?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("unknown compiler target name".into());
    }
    let kinds = target["kind"]
        .as_array()
        .ok_or("missing compiler target kind")?;
    let (kind, script) = match kinds.as_slice() {
        [kind] if kind.as_str() == Some("bin") => ("bin", false),
        [kind] if kind.as_str() == Some("custom-build") => ("build-script", true),
        kinds
            if !kinds.is_empty()
                && kinds.iter().all(|kind| {
                    matches!(
                        kind.as_str(),
                        Some("lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro")
                    )
                }) =>
        {
            ("lib", false)
        }
        _ => return Err("unknown cutter compiler target kind".into()),
    };
    Ok((format!("{kind}-{name}"), script))
}

fn selected_dep_info(row: &Value, artifact: &Path, aggregate: bool) -> Result<PathBuf, String> {
    let name = string(&row["target"], "name")?;
    if aggregate {
        if artifact.file_name().and_then(|name| name.to_str()) != Some(name) {
            return Err("unknown published compiler output name".into());
        }
        return Ok(artifact.with_extension("d"));
    }
    let (target, _) = selected_target(row)?;
    let stem = artifact
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or("unknown compiler output name")?;
    let stem = if target.starts_with("lib-") {
        stem.strip_prefix("lib")
            .ok_or("unknown library output name")?
    } else {
        stem
    };
    let crate_name = name.replace('-', "_");
    if stem != crate_name
        && !stem
            .strip_prefix(&format!("{crate_name}-"))
            .is_some_and(|hash| aterm_codec::hex::is_lower(hash, 16))
    {
        return Err("compiler output names another target".into());
    }
    Ok(artifact
        .parent()
        .ok_or("no compiler output directory")?
        .join(format!("{stem}.d")))
}

/// Cargo uplifts a requested rlib but leaves its metadata private. The same artifact
/// row must name that metadata; its exact selected receipt must cover both original
/// outputs, and the published archive must still have those producer bytes.
fn published_library(
    row: &Value,
    files: &BTreeSet<PathBuf>,
    published: &Path,
    graph: &mut BTreeSet<String>,
    roots: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    let name = string(&row["target"], "name")?;
    let kinds = row["target"]["kind"]
        .as_array()
        .ok_or("missing library kind")?;
    let published_name = format!("lib{}.rlib", name.replace('-', "_"));
    if !matches!(kinds.as_slice(), [kind] if matches!(kind.as_str(), Some("lib" | "rlib")))
        || row["trust_compile_mode"].as_str() != Some("build")
        || row["trust_compile_kind"].as_str() != Some("host")
        || !row["trust_unit_identity_sha256"]
            .as_str()
            .is_some_and(|id| aterm_codec::hex::is_lower(id, 64))
        || !published.is_absolute()
        || published.file_name().and_then(|name| name.to_str()) != Some(published_name.as_str())
    {
        return Err("no selected private evidence for published library".into());
    }
    let mut metadata = files
        .iter()
        .filter(|file| file.extension().is_some_and(|ext| ext == "rmeta"));
    let metadata = match (metadata.next(), metadata.next()) {
        (Some(metadata), None) if metadata.is_absolute() => metadata,
        _ => return Err("published library has no unambiguous metadata companion".into()),
    };
    let (selected, _) = selected_target(row)?;
    let dir = metadata.parent().ok_or("no private library directory")?;
    if dir.file_name().is_none_or(|name| name != "out") {
        return Err("unknown private library output layout".into());
    }
    fingerprint(dir, &selected, graph, None, roots)?;
    // The same row's ordinary metadata pass still requires its exact selected
    // dep-info. The published archive also contributes its own aggregate dep-info.
    let receipt = fingerprint_record(
        &dir.parent()
            .ok_or("no library unit")?
            .join("fingerprint")
            .join(format!("outputs-{selected}"))
            .with_extension("json"),
    )?;
    let receipt: LibraryOutputs = aterm_json::from_value(receipt).map_err(|e| e.to_string())?;
    if receipt.schema != 4
        || receipt.compiler["mode"].as_str() != Some("build")
        || receipt.compiler["workspace"].as_bool() != Some(true)
        || receipt.compiler["build_script"].as_bool() != Some(false)
        || receipt.compiler_outputs.len() != 2
        || receipt.outputs.len() != 2
    {
        return Err("unknown published library output inventory".into());
    }
    let original = metadata.with_extension("rlib");
    let mut archive = None;
    for (path, emit) in [
        (original.as_path(), "link"),
        (metadata.as_path(), "metadata"),
    ] {
        let path_sha256 =
            aterm_codec::hex::encode_lower(&Sha256::digest(path.as_os_str().as_encoded_bytes()));
        if receipt
            .compiler_outputs
            .iter()
            .filter(|output| output.path_sha256 == path_sha256 && output.emit == emit)
            .count()
            != 1
        {
            return Err("published library compiler coverage differs".into());
        }
        let matches: Vec<_> = receipt
            .outputs
            .iter()
            .filter(|output| output.path_sha256 == path_sha256)
            .collect();
        let [recorded] = matches.as_slice() else {
            return Err("published library producer inventory differs".into());
        };
        let actual = output_bytes(path)?;
        if actual != (recorded.size, recorded.sha256.clone()) {
            return Err("private library bytes differ from selected producer".into());
        }
        if emit == "link" {
            archive = Some(actual);
        }
    }
    let published_bytes = output_bytes(published)?;
    if Some(&published_bytes) != archive.as_ref() {
        return Err("published library bytes differ from selected producer".into());
    }
    graph.insert(
        aterm_json::json!({"published-library/1": {
            "path":published, "private_archive":original,
            "size":published_bytes.0, "sha256":published_bytes.1,
        }})
        .to_string(),
    );
    Ok(())
}

#[derive(Deserialize)]
struct LibraryOutputs {
    schema: u32,
    compiler: Value,
    compiler_outputs: Vec<CompilerOutput>,
    outputs: Vec<RecordedOutput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompilerOutput {
    path_sha256: String,
    emit: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedOutput {
    path_sha256: String,
    size: u64,
    sha256: String,
}

fn output_bytes(path: &Path) -> Result<(u64, String), String> {
    use aterm_toolchain::stat_digest::StatKey;
    let (digest, before) = crate::durable::sha256_file_with_meta(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let after = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !StatKey::of(&before).unchanged(&StatKey::of(&after)) {
        return Err("library output changed while reading its producer evidence".into());
    }
    Ok((before.len(), digest))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum ScriptInput {
    RerunIfChanged {
        output: PathBuf,
        paths: Vec<PathBuf>,
    },
    RerunIfChangedContent {
        output: PathBuf,
        paths: Vec<PathBuf>,
        snapshot: Option<ContentSnapshot>,
    },
    RerunIfEnvChanged {
        var: String,
        val: Value,
    },
    Precalculated(String),
}

/// Exact Trust 5062 Snapshot wire and bounds. A missing snapshot is uncacheable,
/// not evidence of an empty declared input set.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContentSnapshot {
    version: u32,
    entries: u64,
    bytes: u64,
    sha256: String,
}

impl ContentSnapshot {
    pub(crate) fn valid(&self) -> bool {
        self.version == 1
            && self.entries <= 100_000
            && self.bytes <= 512 * 1024 * 1024
            && aterm_codec::hex::is_lower(&self.sha256, 64)
    }
}

/// Trust's pinned layout keeps each unit's fingerprint beside its output directory.
/// Missing/unknown records fail closed; no guessed source list licenses adoption.
fn fingerprint(
    out: &Path,
    selected: &str,
    graph: &mut BTreeSet<String>,
    package: Option<&Path>,
    roots: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    let folder = out.parent().ok_or("no unit directory")?.join("fingerprint");
    let value = fingerprint_record(&folder.join(selected).with_extension("json"))?;
    let local = value["local"]
        .as_array()
        .ok_or("missing selected fingerprint declarations")?;
    graph.insert(value.to_string());
    if let Some(package) = package {
        for input in local {
            let input: ScriptInput = aterm_json::from_value(input.clone())
                .map_err(|e| format!("unknown build-script input declaration: {e}"))?;
            let (output, paths) = match input {
                ScriptInput::RerunIfChanged { output, paths } => (output, paths),
                ScriptInput::RerunIfChangedContent {
                    output,
                    paths,
                    snapshot,
                } => {
                    if !snapshot.is_some_and(|snapshot| snapshot.valid()) {
                        return Err("unavailable build-script content snapshot".into());
                    }
                    (output, paths)
                }
                ScriptInput::RerunIfEnvChanged { var, val } => {
                    let valid_value = match val {
                        Value::Null => true,
                        Value::String(value) => !value.contains('\0'),
                        _ => false,
                    };
                    if var.is_empty() || var.contains('\0') || !valid_value {
                        return Err("invalid build-script environment declaration".into());
                    }
                    // Cargo checked this environment; retain its exact recorded value in graph.
                    continue;
                }
                ScriptInput::Precalculated(_value) => {
                    roots.insert(package.to_owned());
                    continue;
                }
            };
            if !output.ends_with("run/stdout")
                || output
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
                || !out
                    .parent()
                    .ok_or("no build-script unit")?
                    .join("run/stdout")
                    .ends_with(&output)
            {
                return Err("build-script control output names another unit".into());
            }
            roots.extend(paths.into_iter().map(|path| package.join(path)));
        }
    } else {
        // Pinned Trust build units also publish their output-integrity receipt. Keep
        // that selected receipt in the graph, but never let it replace the fingerprint.
        let receipt = fingerprint_record(
            &folder
                .join(format!("outputs-{selected}"))
                .with_extension("json"),
        )?;
        if receipt["schema"].as_u64() != Some(4) || !receipt["outputs"].is_array() {
            return Err("unknown selected compiler output receipt".into());
        }
        graph.insert(receipt.to_string());
    }
    Ok(())
}

fn fingerprint_record(path: &Path) -> Result<Value, String> {
    let text = crate::durable::read_regular_text(path, 16 * 1024 * 1024)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    aterm_json::from_str(&text).map_err(|e| e.to_string())
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
