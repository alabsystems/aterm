// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit native Targo component receipt validation.
//!
//! Only an explicitly selected component can issue the private pair binding;
//! ordinary source/tool-map checks never infer authority from file presence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use aterm_json::{Map, Value as V};

use super::component::{absolute, bytes, digest, document, fields, hash, need, oid, text};

type R<T> = Result<T, String>;
const COMPILER: &str = "82bb867eddb94599f68194f701c766f2b6f5a02f";
const COMPILER_SHA: &str = "5d505579da7eeaac54868d794d5e7fb4c142de28ebf06c35d3dbb92d07a3671e";
const PORT_BASE: &str = "32a1d954b4e6024194976d8cb9b9642fee49242c";
const WITNESS_SHA: &str = "cc84dcdd76c821c9ab65164e4edbcf1183a4dd7dc36d100df69d0e73cdfa9e64";
const BASE_PATHS: [&str; 15] = [
    "crates/trust-coordinator/src/coordinator.rs",
    "crates/trust-resource-controller/Cargo.lock",
    "crates/trust-resource-controller/Cargo.toml",
    "crates/trust-resource-controller/examples/probe-scheduler-control.rs",
    "crates/trust-resource-controller/src/client.rs",
    "crates/trust-resource-controller/src/client/probe_scheduler.rs",
    "crates/trust-resource-controller/src/client/sdk.rs",
    "docs/DECISION_QUEUE.md",
    "scripts/tests/doctor_probe_custody_test.py",
    "targo-trust/Cargo.toml",
    "targo-trust/src/main.rs",
    "targo-trust/src/pipeline/probe.rs",
    "targo-trust/src/pipeline/run.rs",
    "targo-trust/src/probe_scheduler.rs",
    "targo-trust/src/tool_identity.rs",
];

const PRODUCTION: [&str; 3] = [
    "src/jobserver_utils/src/lib.rs",
    "src/tools/targo/crates/targo-core/src/compiler/compilation.rs",
    "src/tools/targo/crates/targo-core/src/ops/cargo_run.rs",
];
const TEST: &str = "src/jobserver_utils/src/tests/transport_witnesses.rs";
const DOCUMENTATION: &str = "docs/DECISION_QUEUE.md";
const DETAILS: &str = "TARGO-COMPONENT.evidence";
const EVIDENCE: [&str; 31] = [
    "base-stage-authority.json",
    "base-inventory.json",
    "source-relation.json",
    "build.context.json",
    "build.result.json",
    "build.stdout.jsonl",
    "build.stderr",
    "compiler-version.stdout",
    "compiler-version.stderr",
    "targo-version.stdout",
    "targo-version.stderr",
    "cargo-version.stdout",
    "cargo-version.stderr",
    "image.json",
    "image.codesign.stdout",
    "image.codesign.stderr",
    "transport.context.json",
    "transport.result.json",
    "transport.stdout",
    "transport.stderr",
    "installation.json",
    "installed-inventory.json",
    "native-doctor.stdout",
    "native-doctor.stderr",
    "native-targo-version.stdout",
    "native-targo-version.stderr",
    "native-cargo-version.stdout",
    "native-cargo-version.stderr",
    "native-qualified.context.json",
    "native-qualified.result.json",
    "native-inventory.json",
];
const CONTROLS: [&str; 4] = [
    "descendant_owner_recovers_exact_pipe_without_replenishing_borrowed_credit",
    "descendant_clone_retains_recovery_until_last_scheduler_owner_drops",
    "native_targo_run_keeps_selected_recovery_owner",
    "native_targo_run_refuses_unavailable_or_malformed_transport",
];

/// Only this module can construct the qualified, exact-pair projection.
pub(super) struct Binding {
    root: PathBuf,
    compiler: String,
    compiler_sha256: String,
    frontend_artifact_sha256: String,
    original: BTreeMap<String, String>,
    replacement: String,
    observed_inventory: V,
    footprint: BTreeMap<PathBuf, String>,
    pub(super) seed_mint_sha256: String,
    pub(super) frontend_component_sha256: String,
}

impl Binding {
    pub(super) fn pair_hashes(&self) -> BTreeMap<String, (String, String)> {
        self.original
            .iter()
            .map(|(name, old)| (name.clone(), (old.clone(), self.replacement.clone())))
            .collect()
    }

    pub(super) fn project_frontend_tools(
        &self,
        root: &Path,
        compiler: &str,
        frontend_sha: &str,
        actual: &Map,
    ) -> R<Map> {
        need(
            root == self.root.as_path()
                && compiler == self.compiler
                && frontend_sha == self.frontend_component_sha256,
            "native Targo binding belongs to another seal/compiler/frontend",
        )?;
        physical_directory(root)?;
        need(
            inventory(root)? == self.observed_inventory,
            "native Targo bound hierarchy changed",
        )?;
        for (path, expected) in &self.footprint {
            need(
                hash(&bytes(path, super::component::LIMIT)?) == *expected,
                "native Targo evidence changed after validation",
            )?;
        }
        let frontend = document(&root.join("FRONTEND-COMPONENT.json"), Some(frontend_sha))?;
        need(
            digest(&frontend["base"]["original_mint"]["sha256"])? == self.seed_mint_sha256
                && digest(&frontend["base"]["compiler_sha256"])? == self.compiler_sha256
                && digest(&frontend["artifact"]["sha256"])? == self.frontend_artifact_sha256,
            "native Targo binding differs from frontend mint/compiler/artifact",
        )?;
        let mut projected = actual.clone();
        for (name, old) in &self.original {
            need(
                actual.get(name).and_then(V::as_str) == Some(self.replacement.as_str()),
                "native Targo current pair differs",
            )?;
            projected.insert(name.clone(), V::from(old.clone()));
        }
        Ok(projected)
    }
}

fn relative(path: &str) -> R<()> {
    need(
        !path.is_empty()
            && !path.contains(['\\', '\0'])
            && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
            && Path::new(path)
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "native Targo path is not canonical relative data",
    )
}

fn hashes(value: &V, exact: Option<&[&str]>, tools: bool) -> R<()> {
    let map = value.as_object().ok_or("native Targo digest map absent")?;
    need(!map.is_empty(), "native Targo digest map empty")?;
    if let Some(exact) = exact {
        need(
            map.len() == exact.len() && exact.iter().all(|key| map.contains_key(*key)),
            "native Targo digest map members differ",
        )?;
    }
    for (name, value) in map {
        relative(name)?;
        need(
            !tools || !name.contains('/'),
            "native Targo tool name differs",
        )?;
        digest(value)?;
    }
    Ok(())
}

fn strings(value: &V, sorted: bool) -> R<Vec<&str>> {
    let values = value.as_array().ok_or("native Targo string array absent")?;
    let result = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or("native Targo array member is not text")
        })
        .collect::<Result<Vec<_>, _>>()?;
    if sorted {
        need(
            result.windows(2).all(|pair| pair[0] < pair[1]),
            "native Targo array is not sorted and unique",
        )?;
    }
    Ok(result)
}

fn fixed(value: &V, name: &str) -> R<()> {
    fields(value, "path sha256")?;
    need(
        text(value, "path")? == name,
        "native Targo evidence role differs",
    )?;
    digest(&value["sha256"])?;
    Ok(())
}

fn member_mut<'a>(value: &'a mut V, name: &str) -> R<&'a mut V> {
    value
        .as_object_mut()
        .and_then(|map| map.get_mut(name))
        .ok_or_else(|| format!("native Targo object member absent: {name}"))
}

fn phase(value: &V, name: &str) -> R<()> {
    fields(value, "name exit argv stdout stderr")?;
    need(
        text(value, "name")? == name && value["exit"].as_i64() == Some(0),
        "native Targo phase is not typed actual0",
    )?;
    need(
        !strings(&value["argv"], false)?.is_empty(),
        "native Targo argv empty",
    )?;
    fixed(&value["stdout"], &format!("{name}.stdout"))?;
    fixed(&value["stderr"], &format!("{name}.stderr"))
}

fn retirement(value: &V) -> R<()> {
    fields(value, "completed actual_exit owned_command_drained")?;
    need(
        value["completed"].as_bool() == Some(true)
            && value["actual_exit"].as_i64() == Some(0)
            && value["owned_command_drained"].as_bool() == Some(true),
        "native Targo command is not normally completed and retired",
    )
}

fn recipe(value: &V) -> R<()> {
    fields(
        value,
        "mode static_policy source_recipe_sha256 config_sha256",
    )?;
    need(
        text(value, "mode")? == "ToolTarget"
            && matches!(
                text(value, "static_policy")?,
                "ordinary-vendored" | "all-static"
            ),
        "native Targo build recipe differs",
    )?;
    digest(&value["source_recipe_sha256"])?;
    digest(&value["config_sha256"])?;
    Ok(())
}

fn shape(record: &V, compiler: &str) -> R<()> {
    fields(
        record,
        "schema status name aliases base source artifact build transport retained_inputs_unchanged",
    )?;
    need(
        text(record, "schema")? == "trust.sdk-targo-component.v1"
            && text(record, "status")? == "prepared"
            && text(record, "name")? == "targo"
            && record["aliases"] == aterm_json::json!(["targo", "cargo"])
            && record["retained_inputs_unchanged"].as_bool() == Some(true),
        "native Targo component identity differs",
    )?;
    let base = &record["base"];
    fields(
        base,
        "compiler_source host compiler_sha256 mint_sha256 frontend_component_sha256 frontend_qualified_sha256 frontend_artifact_sha256 stage_binding_sha256 original_pair before_tools stage_authority inventory",
    )?;
    need(
        compiler == COMPILER
            && oid(&base["compiler_source"])? == compiler
            && digest(&base["compiler_sha256"])? == COMPILER_SHA
            && text(base, "host")? == "aarch64-apple-darwin",
        "native Targo compiler or host differs",
    )?;
    for name in [
        "compiler_sha256",
        "mint_sha256",
        "frontend_component_sha256",
        "frontend_qualified_sha256",
        "frontend_artifact_sha256",
        "stage_binding_sha256",
    ] {
        digest(&base[name])?;
    }
    fields(&base["original_pair"], "targo cargo")?;
    hashes(&base["before_tools"], None, true)?;
    for name in ["targo", "cargo"] {
        digest(&base["original_pair"][name])?;
        need(
            base["original_pair"][name] == base["before_tools"][name],
            "native Targo original alias differs",
        )?;
    }
    need(
        base["original_pair"]["targo"] == base["original_pair"]["cargo"]
            && base["before_tools"]["trustc"] == base["compiler_sha256"]
            && base["before_tools"]["targo-trust"] == base["frontend_artifact_sha256"],
        "native Targo retained tool identities differ",
    )?;
    fixed(&base["stage_authority"], "base-stage-authority.json")?;
    fixed(&base["inventory"], "base-inventory.json")?;
    let source = &record["source"];
    fields(
        source,
        "base port_base commit tree root files production_delta test_delta documentation_delta outside_delta_sha256 locks gitlinks relation",
    )?;
    need(
        oid(&source["base"])? == compiler && oid(&source["port_base"])? == PORT_BASE,
        "native Targo source base differs",
    )?;
    for name in ["port_base", "commit", "tree"] {
        oid(&source[name])?;
    }
    absolute(&source["root"])?;
    hashes(&source["files"], None, false)?;
    let wanted: BTreeSet<_> = BASE_PATHS
        .into_iter()
        .chain(PRODUCTION)
        .chain([TEST])
        .collect();
    let actual: BTreeSet<_> = source["files"]
        .as_object()
        .ok_or("source readset absent")?
        .keys()
        .map(String::as_str)
        .collect();
    need(
        actual == wanted,
        "native Targo complete source allowlist differs",
    )?;
    hashes(&source["production_delta"], Some(&PRODUCTION), false)?;
    hashes(&source["test_delta"], Some(&[TEST]), false)?;
    hashes(
        &source["documentation_delta"],
        Some(&[DOCUMENTATION]),
        false,
    )?;
    hashes(
        &source["locks"],
        Some(&["Cargo.lock", "src/tools/targo/Cargo.lock"]),
        false,
    )?;
    digest(&source["outside_delta_sha256"])?;
    for name in ["production_delta", "test_delta", "documentation_delta"] {
        for (path, hash) in source[name]
            .as_object()
            .ok_or("native Targo source delta absent")?
        {
            need(
                source["files"][path.as_str()] == *hash,
                "native Targo cumulative source readset differs",
            )?;
        }
    }
    let links = source["gitlinks"]
        .as_object()
        .ok_or("native Targo Git closure absent")?;
    for (path, commit) in links {
        relative(path)?;
        oid(commit)?;
    }
    fixed(&source["relation"], "source-relation.json")?;
    let artifact = &record["artifact"];
    fields(artifact, "sha256 bytes mode system_libraries")?;
    digest(&artifact["sha256"])?;
    need(
        artifact["bytes"]
            .as_u64()
            .is_some_and(|size| size > 0 && size <= 1 << 30)
            && artifact["mode"].as_u64() == Some(0o755),
        "native Targo artifact size or mode differs",
    )?;
    let libraries = strings(&artifact["system_libraries"], true)?;
    need(!libraries.is_empty(), "native Targo native libraries empty")?;
    for library in libraries {
        need(
            (library.starts_with("/usr/lib/")
                || library.starts_with("/System/Library/Frameworks/"))
                && !library.contains("//")
                && Path::new(library)
                    .components()
                    .all(|part| !matches!(part, Component::ParentDir | Component::CurDir)),
            "native Targo library is not a system library",
        )?;
    }
    let build = &record["build"];
    fields(
        build,
        "compiler target argv features recipe context result cargo_json cargo_stderr compiler_version targo_version cargo_version image",
    )?;
    fields(
        &build["compiler"],
        "source sha256 observed_path sysroot_inventory_sha256",
    )?;
    need(
        build["compiler"]["source"] == base["compiler_source"]
            && build["compiler"]["sha256"] == base["compiler_sha256"]
            && build["target"] == base["host"],
        "native Targo was not built by the retained compiler",
    )?;
    absolute(&build["compiler"]["observed_path"])?;
    digest(&build["compiler"]["sysroot_inventory_sha256"])?;
    need(
        !strings(&build["argv"], false)?.is_empty(),
        "native Targo build argv absent",
    )?;
    strings(&build["features"], true)?;
    recipe(&build["recipe"])?;
    for (key, file) in [
        ("context", "build.context.json"),
        ("result", "build.result.json"),
        ("cargo_json", "build.stdout.jsonl"),
        ("cargo_stderr", "build.stderr"),
        ("image", "image.json"),
    ] {
        fixed(&build[key], file)?;
    }
    for name in ["compiler-version", "targo-version", "cargo-version"] {
        phase(&build[name.replace('-', "_").as_str()], name)?;
    }
    let transport = &record["transport"];
    fields(transport, "context result stdout stderr controls")?;
    for (key, file) in [
        ("context", "transport.context.json"),
        ("result", "transport.result.json"),
        ("stdout", "transport.stdout"),
        ("stderr", "transport.stderr"),
    ] {
        fixed(&transport[key], file)?;
    }
    need(
        strings(&transport["controls"], false)? == CONTROLS,
        "native Targo transport controls differ",
    )?;
    Ok(())
}

fn physical_directory(path: &Path) -> R<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    need(
        path.is_absolute()
            && metadata.is_dir()
            && path.canonicalize().is_ok_and(|actual| actual == path),
        "native Targo directory is not physical",
    )
}

fn evidence_directory(root: &Path) -> R<()> {
    physical_directory(root)?;
    let directory = root.join(DETAILS);
    physical_directory(&directory)?;
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(&directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "native Targo evidence name is not UTF-8")?;
        let metadata =
            std::fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
        need(
            metadata.is_file() && EVIDENCE.contains(&name.as_str()),
            "native Targo evidence is redirected or unexpected",
        )?;
        need(names.insert(name), "native Targo evidence repeated")?;
    }
    need(
        names.len() == EVIDENCE.len(),
        "native Targo evidence set incomplete",
    )
}

fn parse(raw: &[u8]) -> R<V> {
    aterm_json::from_slice::<super::component::Unique>(raw)
        .map(|parsed| parsed.0)
        .map_err(|error| format!("native Targo evidence JSON: {error}"))
}

struct Evidence {
    raw: BTreeMap<&'static str, Vec<u8>>,
}

impl Evidence {
    fn load(root: &Path) -> R<Self> {
        evidence_directory(root)?;
        let raw = EVIDENCE
            .into_iter()
            .map(|name| {
                bytes(&root.join(DETAILS).join(name), super::component::LIMIT)
                    .map(|raw| (name, raw))
            })
            .collect::<R<_>>()?;
        Ok(Self { raw })
    }

    fn literal(&self, name: &str) -> R<&[u8]> {
        self.raw
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| "unknown native Targo evidence role".to_owned())
    }

    fn role(&self, reference: &V, name: &str) -> R<&[u8]> {
        fixed(reference, name)?;
        let raw = self.literal(name)?;
        need(
            hash(raw) == digest(&reference["sha256"])?,
            "native Targo evidence bytes differ",
        )?;
        Ok(raw)
    }

    fn json(&self, reference: &V, name: &str) -> R<V> {
        parse(self.role(reference, name)?)
    }

    fn phase(&self, value: &V, name: &str, prefix: &str) -> R<(&[u8], &[u8])> {
        fields(value, "name exit argv stdout stderr")?;
        need(
            text(value, "name")? == name && value["exit"].as_i64() == Some(0),
            "phase is not typed actual0",
        )?;
        need(
            !strings(&value["argv"], false)?.is_empty(),
            "empty phase argv",
        )?;
        Ok((
            self.role(&value["stdout"], &format!("{prefix}.stdout"))?,
            self.role(&value["stderr"], &format!("{prefix}.stderr"))?,
        ))
    }
}

fn canonical(value: &V) -> R<Vec<u8>> {
    fn string(value: &str, out: &mut String) -> R<()> {
        use std::fmt::Write as _;
        out.push('"');
        for code in value.encode_utf16() {
            match code {
                0x22 => out.push_str("\\\""),
                0x5c => out.push_str("\\\\"),
                8 => out.push_str("\\b"),
                9 => out.push_str("\\t"),
                10 => out.push_str("\\n"),
                12 => out.push_str("\\f"),
                13 => out.push_str("\\r"),
                0x20..=0x7e => {
                    out.push(char::from_u32(u32::from(code)).ok_or("invalid ASCII scalar")?)
                }
                _ => write!(out, "\\u{code:04x}").map_err(|error| error.to_string())?,
            }
        }
        out.push('"');
        Ok(())
    }
    fn append(value: &V, out: &mut String) -> R<()> {
        match value {
            V::Null => out.push_str("null"),
            V::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            V::String(value) => string(value, out)?,
            V::Number(_) => {
                need(
                    value.as_i64().is_some() || value.as_u64().is_some(),
                    "canonical component metadata has noninteger number",
                )?;
                out.push_str(&value.to_string());
            }
            V::Array(rows) => {
                out.push('[');
                for (index, row) in rows.iter().enumerate() {
                    if index != 0 {
                        out.push(',');
                    }
                    append(row, out)?;
                }
                out.push(']');
            }
            V::Object(map) => {
                out.push('{');
                let mut rows: Vec<_> = map.iter().collect();
                rows.sort_by(|a, b| a.0.cmp(b.0));
                for (index, (key, row)) in rows.into_iter().enumerate() {
                    if index != 0 {
                        out.push(',');
                    }
                    string(key, out)?;
                    out.push(':');
                    append(row, out)?;
                }
                out.push('}');
            }
        }
        Ok(())
    }
    let mut out = String::new();
    append(value, &mut out)?;
    Ok(out.into_bytes())
}

fn canonical_hash(value: &V) -> R<String> {
    Ok(hash(&canonical(value)?))
}

fn inventory_shape(value: &V) -> R<()> {
    fields(value, "schema entries")?;
    need(
        text(value, "schema")? == "trust.sdk-component-inventory.v1",
        "native Targo inventory schema differs",
    )?;
    let entries = value["entries"]
        .as_array()
        .ok_or("native Targo inventory entries absent")?;
    need(!entries.is_empty(), "native Targo inventory empty")?;
    let mut previous = None;
    for entry in entries {
        let path = text(entry, "path")?;
        relative(path)?;
        need(
            previous.is_none_or(|old| old < path),
            "native Targo inventory is not sorted and unique",
        )?;
        previous = Some(path);
        need(
            entry["mode"].as_u64().is_some_and(|mode| mode <= 0o777),
            "native Targo inventory mode differs",
        )?;
        match text(entry, "kind")? {
            "file" => {
                fields(entry, "path kind mode bytes sha256")?;
                need(
                    entry["bytes"].as_u64().is_some(),
                    "native Targo inventory file length differs",
                )?;
                digest(&entry["sha256"])?;
            }
            "directory" => fields(entry, "path kind mode")?,
            "symlink" => {
                fields(entry, "path kind mode target")?;
                need(
                    !text(entry, "target")?.contains('\0'),
                    "native Targo inventory link target differs",
                )?;
            }
            _ => return Err("native Targo inventory entry kind differs".to_owned()),
        }
    }
    let roots: BTreeSet<_> = entries
        .iter()
        .map(|row| text(row, "path").map(|path| path.split('/').next().unwrap_or_default()))
        .collect::<R<_>>()?;
    need(
        roots.len() == 2 && roots.contains("rustc-private-sysroots"),
        "inventory omits SDK/sibling private hierarchy",
    )?;
    Ok(())
}

fn inventory_stage(value: &V) -> R<&str> {
    inventory_shape(value)?;
    value["entries"]
        .as_array()
        .ok_or("inventory entries absent")?
        .iter()
        .filter_map(|entry| {
            entry["path"]
                .as_str()
                .and_then(|path| path.split('/').next())
        })
        .find(|name| *name != "rustc-private-sysroots")
        .ok_or_else(|| "inventory SDK basename absent".to_owned())
}

fn permissions(metadata: &std::fs::Metadata) -> R<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = metadata.permissions().mode();
        need(
            mode & 0o7000 == 0,
            "native Targo inventory has special permission bits",
        )?;
        Ok(mode & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err("raw native Targo inventory requires Unix file modes".to_owned())
    }
}

fn inventory(seal: &Path) -> R<V> {
    fn walk(root: &Path, directory: &Path, excluded: &[PathBuf; 3], entries: &mut Vec<V>) -> R<()> {
        for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let relative_path = path.strip_prefix(root).map_err(|error| error.to_string())?;
            let name = relative_path
                .to_str()
                .ok_or("native Targo inventory path is not UTF-8")?;
            relative(name)?;
            if excluded.iter().any(|excluded| relative_path == excluded) {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            let mode = permissions(&metadata)?;
            let row = if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(&path).map_err(|error| error.to_string())?;
                aterm_json::json!({"path":name,"kind":"symlink","mode":mode,"target":target.to_str().ok_or("native Targo link target is not UTF-8")?})
            } else if metadata.is_dir() {
                let row = aterm_json::json!({"path":name,"kind":"directory","mode":mode});
                walk(root, &path, excluded, entries)?;
                row
            } else if metadata.is_file() {
                let digest = crate::tree::sha256_file(&path).map_err(|error| error.to_string())?;
                aterm_json::json!({"path":name,"kind":"file","mode":mode,"bytes":metadata.len(),"sha256":digest})
            } else {
                return Err("native Targo inventory contains a special file".to_owned());
            };
            entries.push(row);
        }
        Ok(())
    }

    physical_directory(seal)?;
    let root = seal.parent().ok_or("SDK has no whole hierarchy parent")?;
    physical_directory(root)?;
    physical_directory(&root.join("rustc-private-sysroots"))?;
    let stage = seal
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("SDK basename is not UTF8")?;
    let names = std::fs::read_dir(root)
        .map_err(|error| error.to_string())?
        .map(|entry| {
            entry.map_err(|error| error.to_string()).and_then(|entry| {
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| "nonUTF8 hierarchy root".to_owned())
            })
        })
        .collect::<R<BTreeSet<_>>>()?;
    need(
        names
            == [stage.to_owned(), "rustc-private-sysroots".to_owned()]
                .into_iter()
                .collect::<BTreeSet<_>>(),
        "hierarchy has unexpected roots",
    )?;
    let excluded = ["TARGO-COMPONENT.json", "TARGO-QUALIFIED.json", DETAILS]
        .map(|name| Path::new(stage).join(name));
    let mut entries = Vec::new();
    walk(root, root, &excluded, &mut entries)?;
    entries.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    let value = aterm_json::json!({"schema":"trust.sdk-component-inventory.v1","entries":entries});
    inventory_shape(&value)?;
    Ok(value)
}

fn inventory_pair(before: &V, after: &V, artifact: &V) -> R<()> {
    inventory_shape(before)?;
    inventory_shape(after)?;
    let mut expected = before.clone();
    let stage = inventory_stage(before)?;
    need(
        inventory_stage(after)? == stage,
        "SDK hierarchy basename changed",
    )?;
    let pair = [format!("{stage}/bin/targo"), format!("{stage}/bin/cargo")];
    let mut replaced = BTreeSet::new();
    for entry in member_mut(&mut expected, "entries")?
        .as_array_mut()
        .ok_or("native Targo inventory entries absent")?
    {
        if entry["path"]
            .as_str()
            .is_some_and(|path| pair.iter().any(|name| path == name))
        {
            need(
                entry["kind"].as_str() == Some("file"),
                "native Targo original alias is not regular",
            )?;
            replaced.insert(
                entry["path"]
                    .as_str()
                    .ok_or("native Targo alias absent")?
                    .to_owned(),
            );
            *member_mut(entry, "mode")? = artifact["mode"].clone();
            *member_mut(entry, "bytes")? = artifact["bytes"].clone();
            *member_mut(entry, "sha256")? = artifact["sha256"].clone();
        }
    }
    need(
        replaced.len() == 2 && expected == *after,
        "native Targo changed more than its exact alias pair",
    )
}

fn base_inventory(record: &V, evidence: &Evidence, seal: &Path) -> R<V> {
    let base = &record["base"];
    let authority = evidence.json(&base["stage_authority"], "base-stage-authority.json")?;
    need(
        authority["schema"].as_str() == Some("trust-sdk-wp-component-ready-v1")
            && authority["status"].as_str() == Some("strict-qualified")
            && authority["ready"].as_bool() == Some(true)
            && authority["source"] == base["compiler_source"]
            && authority["compiler_commit"] == base["compiler_source"]
            && authority["host"] == base["host"],
        "native Targo base stage authority differs",
    )?;
    let mut immutable = authority.clone();
    immutable
        .as_object_mut()
        .ok_or("base authority is not object")?
        .remove("gate_only");
    need(
        canonical_hash(&immutable)? == digest(&base["stage_binding_sha256"])?,
        "base immutable binding differs",
    )?;
    let frontend = &authority["frontend_qualified"];
    need(
        frontend["component_sha256"] == base["frontend_component_sha256"]
            && frontend["qualification_sha256"] == base["frontend_qualified_sha256"]
            && frontend["replacement_sha256"] == base["frontend_artifact_sha256"],
        "base frontend differs",
    )?;
    let tools = authority["executables_sha256"]
        .as_object()
        .ok_or("base authority tool map absent")?;
    need(
        !tools.is_empty()
            && tools
                .iter()
                .all(|(name, hash)| base["before_tools"][name.as_str()] == *hash),
        "base authority tool map differs",
    )?;
    let before = evidence.json(&base["inventory"], "base-inventory.json")?;
    let named = absolute(&authority["stage_path"])?
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("authority SDK basename absent")?;
    need(
        seal.file_name().and_then(|name| name.to_str()) == Some(named)
            && inventory_stage(&before)? == named,
        "native Targo hierarchy basename differs from authority",
    )?;
    Ok(before)
}

fn source_relation(record: &V, evidence: &Evidence) -> R<()> {
    let source = &record["source"];
    let relation = evidence.json(&source["relation"], "source-relation.json")?;
    fields(
        &relation,
        "schema compiler_source port_base candidate_commit candidate_tree causal_fix_commit production_delta_before production_delta_after test_delta documentation_delta protected_tree_before_sha256 protected_tree_after_sha256 locks_before locks_after gitlinks_before gitlinks_after source_readset_sha256 completed",
    )?;
    need(
        relation["schema"].as_str() == Some("trust.native-targo-source-relation.v1")
            && relation["compiler_source"] == source["base"]
            && relation["port_base"] == source["port_base"]
            && relation["candidate_commit"] == source["commit"]
            && relation["candidate_tree"] == source["tree"]
            && relation["causal_fix_commit"].as_str()
                == Some("00d02c75809a974263b66e4ab2dabfc443bc0625")
            && relation["completed"].as_bool() == Some(true),
        "native Targo source relation differs",
    )?;
    hashes(
        &relation["production_delta_before"],
        Some(&PRODUCTION),
        false,
    )?;
    for (actual, selected) in [
        ("production_delta_after", "production_delta"),
        ("test_delta", "test_delta"),
        ("documentation_delta", "documentation_delta"),
    ] {
        need(
            relation[actual] == source[selected],
            "native Targo source delta differs",
        )?;
    }
    for name in [
        "protected_tree_before_sha256",
        "protected_tree_after_sha256",
    ] {
        need(
            relation[name] == source["outside_delta_sha256"],
            "native Targo protected source differs",
        )?;
    }
    for (before, after, selected) in [
        ("locks_before", "locks_after", "locks"),
        ("gitlinks_before", "gitlinks_after", "gitlinks"),
    ] {
        need(
            relation[before] == source[selected] && relation[after] == source[selected],
            "native Targo input closure changed",
        )?;
    }
    need(
        canonical_hash(&source["files"])? == digest(&relation["source_readset_sha256"])?,
        "native Targo source readset digest differs",
    )
}

fn version(raw: &[u8], source: &str, name: &str, release: &str, product: &str) -> R<()> {
    let raw = std::str::from_utf8(raw).map_err(|error| error.to_string())?;
    let lines: Vec<_> = raw.lines().collect();
    need(
        lines
            .first()
            .is_some_and(|line| line.starts_with(&format!("{name} "))),
        "native Targo version branding differs",
    )?;
    for (key, expected) in [
        ("commit-hash: ", source),
        ("trust-repo-commit-hash: ", source),
        ("release: ", release),
        ("trust: ", product),
    ] {
        let selected: Vec<_> = lines
            .iter()
            .filter_map(|line| line.strip_prefix(key))
            .collect();
        need(
            selected == [expected],
            "native Targo version identity differs",
        )?;
    }
    Ok(())
}

fn version_environment<'a>(value: &'a V, source: &str) -> R<(&'a str, &'a str)> {
    fields(
        value,
        "CFG_VER_HASH CFG_COMMIT_HASH CFG_SHORT_COMMIT_HASH CFG_COMMIT_DATE CFG_RELEASE CFG_TRUST_VERSION",
    )?;
    need(
        text(value, "CFG_VER_HASH")? == source
            && text(value, "CFG_COMMIT_HASH")? == source
            && text(value, "CFG_SHORT_COMMIT_HASH")?
                == source.get(..9).ok_or("short source commit")?,
        "native Targo source version tuple differs",
    )?;
    let date = text(value, "CFG_COMMIT_DATE")?.as_bytes();
    need(
        date.len() == 10
            && date.iter().enumerate().all(|(index, byte)| {
                if matches!(index, 4 | 7) {
                    *byte == b'-'
                } else {
                    byte.is_ascii_digit()
                }
            }),
        "native Targo commit date malformed",
    )?;
    let release = text(value, "CFG_RELEASE")?;
    let product = text(value, "CFG_TRUST_VERSION")?;
    need(
        !release.is_empty() && !product.is_empty(),
        "native Targo release/product absent",
    )?;
    Ok((release, product))
}

fn build(record: &V, evidence: &Evidence) -> R<V> {
    let selected = &record["build"];
    let base = &record["base"];
    let source = &record["source"];
    let context = evidence.json(&selected["context"], "build.context.json")?;
    fields(
        &context,
        "schema source_commit source_tree port_base source_root target_directory original_artifact_path compiler frontend host argv features recipe input_inventory_sha256 lock_sha256 generated_lean_manifest_sha256 version_environment",
    )?;
    need(
        context["schema"].as_str() == Some("trust.native-targo-build-context.v1")
            && context["source_commit"] == source["commit"]
            && context["source_tree"] == source["tree"]
            && context["port_base"] == source["port_base"]
            && context["source_root"] == source["root"]
            && context["host"] == base["host"],
        "native Targo build context source differs",
    )?;
    let source_root = absolute(&context["source_root"])?;
    let target = absolute(&context["target_directory"])?;
    let original = absolute(&context["original_artifact_path"])?;
    need(
        original.starts_with(target)
            && original != target
            && !target.starts_with(source_root)
            && !source_root.starts_with(target),
        "native Targo artifact target overlaps source or lies outside target",
    )?;
    let compiler = &context["compiler"];
    fields(
        compiler,
        "observed_path source_commit sha256 observed_sysroot sysroot_inventory_sha256",
    )?;
    let sysroot = absolute(&compiler["observed_sysroot"])?;
    need(
        absolute(&compiler["observed_path"])? == sysroot.join("bin/trustc")
            && compiler["source_commit"] == base["compiler_source"]
            && compiler["sha256"] == base["compiler_sha256"]
            && compiler["observed_path"] == selected["compiler"]["observed_path"]
            && compiler["sysroot_inventory_sha256"]
                == selected["compiler"]["sysroot_inventory_sha256"]
            && compiler["sysroot_inventory_sha256"] == base["inventory"]["sha256"]
            && context["input_inventory_sha256"] == base["inventory"]["sha256"],
        "native Targo build compiler/private hierarchy differs",
    )?;
    let parent = sysroot
        .parent()
        .ok_or("build SDK hierarchy parent absent")?;
    need(
        !target.starts_with(parent) && !parent.starts_with(target),
        "build target overlaps retained hierarchy",
    )?;
    fields(&context["frontend"], "observed_path sha256")?;
    need(
        absolute(&context["frontend"]["observed_path"])? == sysroot.join("bin/targo")
            && context["frontend"]["sha256"] == base["before_tools"]["targo"],
        "native Targo build driver differs",
    )?;
    need(
        context["lock_sha256"] == source["locks"]["src/tools/targo/Cargo.lock"]
            && context["recipe"] == selected["recipe"]
            && context["features"] == selected["features"],
        "native Targo build recipe/lock differs",
    )?;
    digest(&context["generated_lean_manifest_sha256"])?;
    let (release, product) =
        version_environment(&context["version_environment"], oid(&source["commit"])?)?;
    let requested = if selected["recipe"]["static_policy"].as_str() == Some("all-static") {
        vec!["all-static"]
    } else {
        vec!["vendored-openssl", "vendored-libgit2"]
    };
    let features = strings(&selected["features"], true)?;
    need(
        requested.iter().all(|name| features.contains(name)),
        "native Targo requested features absent",
    )?;
    let argv = vec![
        text(&context["frontend"], "observed_path")?.to_owned(),
        "--unverified".to_owned(),
        "build".to_owned(),
        "--release".to_owned(),
        "--locked".to_owned(),
        "--offline".to_owned(),
        "--manifest-path".to_owned(),
        source_root
            .join("src/tools/targo/Cargo.toml")
            .to_string_lossy()
            .into_owned(),
        "--bin".to_owned(),
        "targo".to_owned(),
        "--features".to_owned(),
        requested.join(","),
        "--message-format=json-render-diagnostics".to_owned(),
    ];
    let argv = aterm_json::to_value(argv).map_err(|error| error.to_string())?;
    need(
        selected["argv"] == argv && context["argv"] == argv,
        "native Targo build is not exact direct recipe",
    )?;
    let result = evidence.json(&selected["result"], "build.result.json")?;
    fields(
        &result,
        "schema source_commit source_tree artifact_sha256 compiler_sha256 argv context_sha256 cargo_json_sha256 exit completed retirement",
    )?;
    need(
        result["schema"].as_str() == Some("trust.native-targo-build-result.v1")
            && result["source_commit"] == source["commit"]
            && result["source_tree"] == source["tree"]
            && result["artifact_sha256"] == record["artifact"]["sha256"]
            && result["compiler_sha256"] == base["compiler_sha256"]
            && result["argv"] == argv
            && result["context_sha256"] == selected["context"]["sha256"]
            && result["exit"].as_i64() == Some(0)
            && result["completed"].as_bool() == Some(true),
        "native Targo build not actual completed0",
    )?;
    retirement(&result["retirement"])?;
    let raw = evidence.role(&selected["cargo_json"], "build.stdout.jsonl")?;
    need(
        hash(raw) == digest(&result["cargo_json_sha256"])?,
        "native Targo Cargo JSON digest differs",
    )?;
    evidence.role(&selected["cargo_stderr"], "build.stderr")?;
    let mut executables = Vec::new();
    for line in raw
        .split(|byte| *byte == b'\n')
        .filter(|line| line.first() == Some(&b'{'))
    {
        let item = parse(line)?;
        if item["reason"].as_str() == Some("compiler-artifact")
            && item["target"]["name"].as_str() == Some("targo")
            && !item["executable"].is_null()
        {
            need(
                item["target"]["kind"] == aterm_json::json!(["bin"])
                    && item["profile"]["test"].as_bool() == Some(false),
                "native Targo Cargo artifact is not non-test bin",
            )?;
            let mut actual = strings(&item["features"], false)?;
            actual.sort_unstable();
            need(
                actual == features,
                "native Targo artifact feature population differs",
            )?;
            executables.push(text(&item, "executable")?.to_owned());
        }
    }
    need(
        executables == [text(&context, "original_artifact_path")?],
        "native Targo Cargo artifact missing/ambiguous/foreign",
    )?;
    let (compiler_stdout, _) = evidence.phase(
        &selected["compiler_version"],
        "compiler-version",
        "compiler-version",
    )?;
    need(
        selected["compiler_version"]["argv"]
            == aterm_json::json!([text(compiler, "observed_path")?, "-vV"]),
        "build compiler observation command differs",
    )?;
    let compiler_stdout =
        std::str::from_utf8(compiler_stdout).map_err(|error| error.to_string())?;
    for (prefix, expected) in [("commit-hash: ", COMPILER), ("release: ", release)] {
        need(
            compiler_stdout
                .lines()
                .filter_map(|line| line.strip_prefix(prefix))
                .collect::<Vec<_>>()
                == [expected],
            "retained compiler version differs",
        )?;
    }
    for name in ["targo", "cargo"] {
        let value = &selected[format!("{name}_version").as_str()];
        let (stdout, _) = evidence.phase(
            value,
            &format!("{name}-version"),
            &format!("{name}-version"),
        )?;
        let args = strings(&value["argv"], false)?;
        need(
            args.len() == 3
                && Path::new(args[0])
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some(name)
                && args[1..] == ["--version", "--verbose"],
            "native alias version argv differs",
        )?;
        version(stdout, oid(&source["commit"])?, name, release, product)?;
    }
    Ok(context)
}

fn image(record: &V, evidence: &Evidence, context: &V, payload: &[u8]) -> R<()> {
    let artifact = &record["artifact"];
    need(
        hash(payload) == digest(&artifact["sha256"])?
            && artifact["bytes"].as_u64() == u64::try_from(payload.len()).ok(),
        "native Targo payload bytes differ",
    )?;
    let actual =
        super::component_image::native_targo_image(payload, text(&record["base"], "host")?)?;
    need(
        payload.get(8..12).is_some_and(|part| part == [0, 0, 0, 0]),
        "native Targo ARM subtype differs",
    )?;
    let libraries =
        aterm_json::to_value(&actual.system_libraries).map_err(|error| error.to_string())?;
    need(
        libraries == artifact["system_libraries"],
        "native Targo actual library closure differs",
    )?;
    let value = evidence.json(&record["build"]["image"], "image.json")?;
    fields(
        &value,
        "schema artifact_sha256 bytes mode host format cpu_type file_type system_libraries code_signature_present codesign",
    )?;
    need(
        value["schema"].as_str() == Some("trust.native-targo-image.v1")
            && value["artifact_sha256"] == artifact["sha256"]
            && value["bytes"] == artifact["bytes"]
            && value["mode"] == artifact["mode"]
            && value["host"] == record["base"]["host"]
            && value["format"].as_str() == Some("mach-o64-le")
            && value["cpu_type"].as_u64() == Some(16777228)
            && value["file_type"].as_u64() == Some(2)
            && value["system_libraries"] == libraries
            && value["code_signature_present"].as_bool() == Some(actual.code_signature_present),
        "native Targo image observation differs from bytes",
    )?;
    evidence.phase(
        &value["codesign"],
        "artifact-code-signature",
        "image.codesign",
    )?;
    need(
        value["codesign"]["argv"]
            == aterm_json::json!([
                "/usr/bin/codesign",
                "--verify",
                "--strict",
                text(context, "original_artifact_path")?
            ]),
        "native Targo signature command selected another artifact",
    )
}

fn retained_frontend(root: &Path, record: &V, context: &V) -> R<()> {
    let base = &record["base"];
    need(
        hash(&bytes(&root.join("bin/trustc"), 1 << 30)?) == digest(&base["compiler_sha256"])?,
        "retained native compiler bytes changed",
    )?;
    need(
        hash(&bytes(&root.join("bin/targo-trust"), 1 << 30)?)
            == digest(&base["frontend_artifact_sha256"])?,
        "retained frontend bytes changed",
    )?;
    let frontend = document(
        &root.join("FRONTEND-COMPONENT.json"),
        Some(digest(&base["frontend_component_sha256"])?),
    )?;
    need(
        frontend["schema"].as_str() == Some("trust.sdk-frontend-component.v2")
            && frontend["name"].as_str() == Some("targo-trust")
            && frontend["base"]["compiler_source"] == base["compiler_source"]
            && frontend["base"]["original_mint"]["sha256"] == base["mint_sha256"],
        "native/frontend mint/compiler selection differs",
    )?;
    need(
        hash(&bytes(
            &root.join("FRONTEND-QUALIFIED.json"),
            super::component::LIMIT,
        )?) == digest(&base["frontend_qualified_sha256"])?,
        "retained frontend qualification changed",
    )?;
    let built = document(
        &root.join("FRONTEND-COMPONENT.evidence/build.json"),
        Some(digest(&frontend["build_receipt"]["sha256"])?),
    )?;
    let table = &built["generated_inputs"]["clean_olean_manifest"];
    need(
        table["original"]["sha256"] == table["candidate"]["sha256"]
            && digest(&table["original"]["sha256"])?
                == digest(&context["generated_lean_manifest_sha256"])?,
        "native component changed retained Lean datum",
    )
}

fn tool_map(root: &Path) -> R<V> {
    physical_directory(&root.join("bin"))?;
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(root.join("bin")).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if !std::fs::symlink_metadata(&path)
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            paths.push(path);
        }
    }
    let helper = root.join("libexec/trust-analyzer-proc-macro-srv");
    match std::fs::symlink_metadata(&helper) {
        Ok(_) => paths.push(helper),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let mut map = Map::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("native tool basename not UTF8")?;
        relative(name)?;
        need(
            map.insert(name.to_owned(), V::from(hash(&bytes(&path, 1 << 30)?)))
                .is_none(),
            "duplicate native tool basename",
        )?;
    }
    Ok(V::Object(map))
}

fn installation(record: &V, evidence: &Evidence, before: &V) -> R<(V, V)> {
    let value = parse(evidence.literal("installation.json")?)?;
    fields(
        &value,
        "schema component_sha256 compiler_source mint_sha256 frontend_component_sha256 before_tools after_tools before_inventory_sha256 after_inventory complete",
    )?;
    let base = &record["base"];
    let mut expected = base["before_tools"].clone();
    for name in ["targo", "cargo"] {
        *member_mut(&mut expected, name)? = record["artifact"]["sha256"].clone();
    }
    need(
        value["schema"].as_str() == Some("trust.sdk-targo-install.v1")
            && value["complete"].as_bool() == Some(true)
            && value["compiler_source"] == base["compiler_source"]
            && value["mint_sha256"] == base["mint_sha256"]
            && value["frontend_component_sha256"] == base["frontend_component_sha256"]
            && value["before_tools"] == base["before_tools"]
            && value["after_tools"] == expected
            && value["before_inventory_sha256"] == base["inventory"]["sha256"],
        "native Targo installation incomplete/foreign",
    )?;
    let after = evidence.json(&value["after_inventory"], "installed-inventory.json")?;
    inventory_pair(before, &after, &record["artifact"])?;
    Ok((value, after))
}

fn doctor(value: &V, subject: &Path) -> R<()> {
    let compiler = &value["compiler"];
    let expected = subject.join("bin/trustc");
    let daily = &compiler["daily_driver"];
    need(
        value["ready"].as_bool() == Some(true)
            && compiler["trust_verify"].as_bool() == Some(true)
            && compiler["json_transport"].as_bool() == Some(true)
            && absolute(&compiler["path"])? == expected
            && absolute(&compiler["linked_toolchain_path"])? == expected
            && compiler["discovery_source"].as_str() == Some("sibling_trustc")
            && compiler["linked_toolchain_status"].as_str() == Some("visible")
            && daily["ready"].as_bool() == Some(true)
            && absolute(&daily["linked_targo_path"])? == subject.join("bin/targo")
            && absolute(&daily["linked_targo_trust_path"])? == subject.join("bin/targo-trust"),
        "new native doctor lacks actual selected compiler/frontend readiness",
    )
}

fn qualified(
    record: &V,
    installed: &V,
    evidence: &Evidence,
    value: &V,
    expected: &str,
    build: &V,
) -> R<()> {
    fields(
        value,
        "schema status name component_sha256 installation_sha256 compiler_source compiler_sha256 mint_sha256 frontend_component_sha256 frontend_artifact_sha256 artifact_sha256 doctor targo_version cargo_version context result inventory completed",
    )?;
    let base = &record["base"];
    need(
        value["schema"].as_str() == Some("trust.sdk-targo-qualified.v1")
            && value["status"].as_str() == Some("qualified")
            && value["name"].as_str() == Some("targo")
            && value["component_sha256"].as_str() == Some(expected)
            && value["installation_sha256"].as_str()
                == Some(hash(evidence.literal("installation.json")?).as_str())
            && value["artifact_sha256"] == record["artifact"]["sha256"]
            && value["completed"].as_bool() == Some(true),
        "native qualification identity differs",
    )?;
    for name in [
        "compiler_source",
        "compiler_sha256",
        "mint_sha256",
        "frontend_component_sha256",
        "frontend_artifact_sha256",
    ] {
        need(
            value[name] == base[name],
            "native qualification base differs",
        )?;
    }
    let context = evidence.json(&value["context"], "native-qualified.context.json")?;
    fields(
        &context,
        "schema source_commit component_sha256 installation_sha256 selected_subject compiler aliases frontend mint_sha256 base_stage_binding_sha256 commands",
    )?;
    need(
        context["schema"].as_str() == Some("trust.native-targo-qualified-context.v1")
            && context["source_commit"] == record["source"]["commit"]
            && context["component_sha256"] == value["component_sha256"]
            && context["installation_sha256"] == value["installation_sha256"]
            && context["mint_sha256"] == base["mint_sha256"]
            && context["base_stage_binding_sha256"] == base["stage_binding_sha256"],
        "native qualification context differs",
    )?;
    let subject = absolute(&context["selected_subject"])?;
    let path = |name: &str| {
        subject
            .join("bin")
            .join(name)
            .to_string_lossy()
            .into_owned()
    };
    need(
        context["compiler"]
            == aterm_json::json!({"observed_path":path("trustc"), "source_commit":base["compiler_source"], "sha256":base["compiler_sha256"]})
            && context["aliases"]
                == aterm_json::json!({"targo":{"observed_path":path("targo"),"sha256":record["artifact"]["sha256"]}, "cargo":{"observed_path":path("cargo"),"sha256":record["artifact"]["sha256"]}})
            && context["frontend"]
                == aterm_json::json!({"observed_path":path("targo-trust"), "component_sha256":base["frontend_component_sha256"], "artifact_sha256":base["frontend_artifact_sha256"]}),
        "native qualification selected images differ",
    )?;
    let commands = aterm_json::json!({"doctor":[path("targo"),"trust","doctor","--format","json"],
        "targo_version":[path("targo"),"--version","--verbose"], "cargo_version":[path("cargo"),"--version","--verbose"]});
    need(
        context["commands"] == commands,
        "native qualification commands differ",
    )?;
    let (release, product) = version_environment(
        &build["version_environment"],
        oid(&record["source"]["commit"])?,
    )?;
    for (name, role) in [
        ("doctor", "native-doctor"),
        ("targo_version", "native-targo-version"),
        ("cargo_version", "native-cargo-version"),
    ] {
        let (stdout, _) = evidence.phase(&value[name], role, role)?;
        need(
            value[name]["argv"] == commands[name],
            "native qualification phase argv differs",
        )?;
        if name == "doctor" {
            doctor(&parse(stdout)?, subject)?;
        } else {
            version(
                stdout,
                oid(&record["source"]["commit"])?,
                name.strip_suffix("_version").ok_or("version role absent")?,
                release,
                product,
            )?;
        }
    }
    let result = evidence.json(&value["result"], "native-qualified.result.json")?;
    fields(
        &result,
        "schema source_commit component_sha256 artifact_sha256 compiler_sha256 context_sha256 installation_sha256 inventory_sha256 phase_names exit completed retirement",
    )?;
    need(
        result["schema"].as_str() == Some("trust.native-targo-qualified-result.v1")
            && result["source_commit"] == record["source"]["commit"]
            && result["component_sha256"] == value["component_sha256"]
            && result["artifact_sha256"] == record["artifact"]["sha256"]
            && result["compiler_sha256"] == base["compiler_sha256"]
            && result["context_sha256"] == value["context"]["sha256"]
            && result["installation_sha256"] == value["installation_sha256"]
            && result["inventory_sha256"] == value["inventory"]["sha256"]
            && result["phase_names"]
                == aterm_json::json!([
                    "native-doctor",
                    "native-targo-version",
                    "native-cargo-version"
                ])
            && result["exit"].as_i64() == Some(0)
            && result["completed"].as_bool() == Some(true),
        "native qualification actual result differs",
    )?;
    retirement(&result["retirement"])?;
    need(
        evidence.json(&value["inventory"], "native-inventory.json")?
            == evidence.json(&installed["after_inventory"], "installed-inventory.json")?,
        "native qualification inventory changed",
    )
}

fn manifest(source: &Path) -> R<Vec<u8>> {
    let dependency = source.join("src/jobserver_utils");
    let quoted = canonical(&V::from(
        dependency.to_str().ok_or("mini source root is not UTF8")?,
    ))?;
    let quoted = std::str::from_utf8(&quoted).map_err(|error| error.to_string())?;
    Ok(format!("[package]\nname = \"native-targo-custody-witness\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n[dependencies]\njobserver_utils = {{ path = {quoted} }}\nlibc = \"0.2\"\n").into_bytes())
}

fn transport_commands(record: &V, subject: &Path, project: &Path) -> R<V> {
    let source = absolute(&record["source"]["root"])?;
    let targo = subject.join("bin/targo").to_string_lossy().into_owned();
    let compiler = subject.join("bin/trustc").to_string_lossy().into_owned();
    let test = vec![
        targo.clone(),
        "--unverified".to_owned(),
        "test".to_owned(),
        "--manifest-path".to_owned(),
        source.join("Cargo.toml").to_string_lossy().into_owned(),
        "--locked".to_owned(),
        "--offline".to_owned(),
        "-p".to_owned(),
        "jobserver_utils".to_owned(),
        "--lib".to_owned(),
    ];
    let run = vec![
        targo,
        "--unverified".to_owned(),
        "run".to_owned(),
        "--locked".to_owned(),
        "--offline".to_owned(),
        "--manifest-path".to_owned(),
        project.join("Cargo.toml").to_string_lossy().into_owned(),
        "--bin".to_owned(),
        "native-targo-custody-witness".to_owned(),
        "--".to_owned(),
    ];
    let mut commands = Vec::new();
    for name in &CONTROLS[..2] {
        let mut args = test.clone();
        args.extend([
            format!("transport_witness_tests::{name}"),
            "--".to_owned(),
            "--exact".to_owned(),
            "--nocapture".to_owned(),
            "--test-threads=1".to_owned(),
        ]);
        commands.push(aterm_json::json!({"name":name,"argv":[args]}));
    }
    let mut positive = run.clone();
    positive.extend(["positive".to_owned(), compiler.clone()]);
    commands.push(aterm_json::json!({"name":CONTROLS[2],"argv":[positive]}));
    let negative: Vec<_> = ["missing", "malformed"]
        .into_iter()
        .map(|mode| {
            let mut args = run.clone();
            args.extend([mode.to_owned(), compiler.clone()]);
            args
        })
        .collect();
    commands.push(aterm_json::json!({"name":CONTROLS[3],"argv":negative}));
    Ok(V::Array(commands))
}

struct Frame {
    code: i64,
    raw: Vec<u8>,
}

fn frames(raw: &[u8], commands: &V) -> R<Vec<Frame>> {
    let commands = commands.as_array().ok_or("transport commands absent")?;
    let expected = commands
        .iter()
        .flat_map(|command| {
            command["argv"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(move |(index, argv)| (command, index, argv))
        })
        .collect::<Vec<_>>();
    let raw = std::str::from_utf8(raw).map_err(|error| error.to_string())?;
    let lines: Vec<_> = raw.lines().collect();
    need(
        lines.len() == expected.len(),
        "transport raw frame population differs",
    )?;
    lines
        .into_iter()
        .zip(expected)
        .map(|(line, (command, index, argv))| {
            let row = parse(line.as_bytes())?;
            fields(&row, "name index argv exit raw_base64")?;
            let code = row["exit"]
                .as_i64()
                .ok_or("raw transport exit is not integer")?;
            need(
                row["name"] == command["name"]
                    && row["index"].as_u64() == u64::try_from(index).ok()
                    && row["argv"] == *argv
                    && (0..128).contains(&code),
                "raw transport command/status differs",
            )?;
            let raw = aterm_codec::base64::decode_strict(text(&row, "raw_base64")?.as_bytes())
                .map_err(|error| error.to_string())?;
            Ok(Frame { code, raw })
        })
        .collect()
}

fn pipe(value: &V) -> R<()> {
    need(
        value.as_array().is_some_and(|items| {
            items.len() == 4 && items.iter().all(|item| item.as_u64().is_some())
        }),
        "pipe identity is not four typed u64s",
    )
}

fn mini(raw: &[u8]) -> R<V> {
    let raw = std::str::from_utf8(raw).map_err(|error| error.to_string())?;
    let selected: Vec<_> = raw
        .lines()
        .filter_map(|line| line.strip_prefix("NATIVE_TARGO_CUSTODY "))
        .collect();
    need(
        selected.len() == 1,
        "missing/ambiguous actual mini observation",
    )?;
    parse(selected[0].as_bytes())
}

fn transport_observation(number: usize, observed: &V, outputs: &[Frame], subject: &Path) -> R<()> {
    if number < 2 {
        need(
            outputs.len() == 1 && outputs[0].code == 0,
            "owner test did not exit0",
        )?;
        let case = format!("transport_witness_tests::{}", CONTROLS[number]);
        let raw = std::str::from_utf8(&outputs[0].raw).map_err(|error| error.to_string())?;
        need(
            raw.lines()
                .any(|line| line == format!("test {case} ... ok"))
                && raw.lines().any(|line| {
                    line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")
                }),
            "owner test lacks exact single named pass",
        )?;
        return need(
            *observed
                == aterm_json::json!({"kind":"source-test","test_case":case,"passed":1,"failed":0,"ignored":0}),
            "owner test finite observation differs",
        );
    }
    if number == 2 {
        need(
            outputs.len() == 1 && outputs[0].code == 0,
            "native Targo positive child failed",
        )?;
        let child = mini(&outputs[0].raw)?;
        fields(
            &child,
            "kind observed_pipe available_before available_after compiler_exit compiler_stdout_hex compiler_stderr_hex sysroot marker",
        )?;
        pipe(&child["observed_pipe"])?;
        pipe(&observed["original_pipe"])?;
        need(
            child["kind"].as_str() == Some("native-run")
                && child["observed_pipe"] == observed["original_pipe"]
                && child["available_before"].as_u64() == Some(0)
                && child["available_after"].as_u64() == Some(0)
                && child["compiler_exit"].as_i64() == Some(0)
                && absolute(&child["sysroot"])? == subject
                && child["marker"].as_str() == Some("native-targo-custody-positive-v1"),
            "native run lost original pipe/credit/compiler",
        )?;
        let mut streams = Vec::new();
        for name in ["compiler_stdout_hex", "compiler_stderr_hex"] {
            let value = text(&child, name)?;
            need(
                value.len().is_multiple_of(2)
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "mini compiler raw is not lower hex",
            )?;
            streams.push(
                aterm_codec::hex::decode_vec(value.as_bytes())
                    .ok_or("mini compiler raw hex invalid")?,
            );
        }
        need(
            std::str::from_utf8(&streams[0])
                .map_err(|error| error.to_string())?
                .trim()
                == subject.to_str().ok_or("subject notUTF8")?,
            "plain compiler selected another sysroot",
        )?;
        let mut expected = child.as_object().ok_or("mini object absent")?.clone();
        expected.remove("compiler_stdout_hex");
        expected.remove("compiler_stderr_hex");
        expected.insert("child_exit".to_owned(), V::from(0));
        expected.insert(
            "original_pipe".to_owned(),
            observed["original_pipe"].clone(),
        );
        expected.insert(
            "compiler_stdout_sha256".to_owned(),
            V::from(hash(&streams[0])),
        );
        expected.insert(
            "compiler_stderr_sha256".to_owned(),
            V::from(hash(&streams[1])),
        );
        return need(
            *observed == V::Object(expected),
            "native run finite observation differs from raw",
        );
    }
    need(outputs.len() == 2, "native negative population differs")?;
    let mut expected = Vec::new();
    for (output, (mode, reason)) in outputs.iter().zip([
        ("missing", "checked-import-unavailable"),
        ("malformed", "checked-import-error"),
    ]) {
        let child = mini(&output.raw)?;
        let value = aterm_json::json!({"mode":mode,"child_exit":75,"reason":reason,"compiler_started":false,"marker":false});
        need(
            output.code == 75 && child == value,
            "mini negative did not preserve refusal75 before compiler",
        )?;
        expected.push(value);
    }
    need(
        *observed == aterm_json::json!({"kind":"native-run-refusal","cases":expected}),
        "negative finite observations differ",
    )
}

fn transport(record: &V, evidence: &Evidence, before: &V) -> R<()> {
    let selected = &record["transport"];
    let context = evidence.json(&selected["context"], "transport.context.json")?;
    fields(
        &context,
        "schema source_commit artifact_sha256 compiler_sha256 base_stage_binding_sha256 selected_subject aliases compiler frontend subject_inventory_sha256 witness commands",
    )?;
    let base = &record["base"];
    need(
        context["schema"].as_str() == Some("trust.native-targo-transport-context.v1")
            && context["source_commit"] == record["source"]["commit"]
            && context["artifact_sha256"] == record["artifact"]["sha256"]
            && context["compiler_sha256"] == base["compiler_sha256"]
            && context["base_stage_binding_sha256"] == base["stage_binding_sha256"],
        "transport context binding differs",
    )?;
    let subject = absolute(&context["selected_subject"])?;
    need(
        subject.file_name().and_then(|name| name.to_str()) == Some(inventory_stage(before)?),
        "transport renamed SDK hierarchy",
    )?;
    let path = |name: &str| {
        subject
            .join("bin")
            .join(name)
            .to_string_lossy()
            .into_owned()
    };
    need(
        context["aliases"]
            == aterm_json::json!({"targo":{"observed_path":path("targo"),"sha256":record["artifact"]["sha256"]},"cargo":{"observed_path":path("cargo"),"sha256":record["artifact"]["sha256"]}})
            && context["compiler"]
                == aterm_json::json!({"observed_path":path("trustc"),"source_commit":base["compiler_source"],"sha256":base["compiler_sha256"]})
            && context["frontend"]
                == aterm_json::json!({"observed_path":path("targo-trust"),"sha256":base["frontend_artifact_sha256"]}),
        "transport selected images differ",
    )?;
    let mut after = before.clone();
    let stage = inventory_stage(before)?;
    let pairs = [format!("{stage}/bin/targo"), format!("{stage}/bin/cargo")];
    for row in member_mut(&mut after, "entries")?
        .as_array_mut()
        .ok_or("transport inventory absent")?
    {
        if row["path"]
            .as_str()
            .is_some_and(|path| pairs.iter().any(|selected| path == selected))
        {
            *member_mut(row, "mode")? = record["artifact"]["mode"].clone();
            *member_mut(row, "bytes")? = record["artifact"]["bytes"].clone();
            *member_mut(row, "sha256")? = record["artifact"]["sha256"].clone();
        }
    }
    inventory_pair(before, &after, &record["artifact"])?;
    need(
        canonical_hash(&after)? == digest(&context["subject_inventory_sha256"])?,
        "transport prospective hierarchy changed other data",
    )?;
    let witness = &context["witness"];
    fields(witness, "project_root source_sha256 manifest_sha256")?;
    let project = absolute(&witness["project_root"])?;
    need(
        digest(&witness["source_sha256"])? == WITNESS_SHA
            && digest(&witness["manifest_sha256"])?
                == hash(&manifest(absolute(&record["source"]["root"])?)?),
        "transport witness source differs",
    )?;
    let commands = transport_commands(record, subject, project)?;
    need(
        context["commands"] == commands,
        "transport command route differs",
    )?;
    let result = evidence.json(&selected["result"], "transport.result.json")?;
    fields(
        &result,
        "schema source_commit artifact_sha256 compiler_sha256 context_sha256 stdout_sha256 stderr_sha256 controls completed retirement",
    )?;
    need(
        result["schema"].as_str() == Some("trust.native-targo-transport.v1")
            && result["source_commit"] == record["source"]["commit"]
            && result["artifact_sha256"] == record["artifact"]["sha256"]
            && result["compiler_sha256"] == base["compiler_sha256"]
            && result["context_sha256"] == selected["context"]["sha256"]
            && result["completed"].as_bool() == Some(true),
        "transport actual result differs",
    )?;
    retirement(&result["retirement"])?;
    let out = evidence.role(&selected["stdout"], "transport.stdout")?;
    let err = evidence.role(&selected["stderr"], "transport.stderr")?;
    need(
        hash(out) == digest(&result["stdout_sha256"])?
            && hash(err) == digest(&result["stderr_sha256"])?,
        "transport raw result binding differs",
    )?;
    let stdout = frames(out, &commands)?;
    let stderr = frames(err, &commands)?;
    need(
        stdout
            .iter()
            .zip(&stderr)
            .all(|(out, err)| out.code == err.code),
        "raw transport stdout/stderr status differs",
    )?;
    let rows = result["controls"]
        .as_array()
        .ok_or("transport result controls absent")?;
    need(
        rows.len() == CONTROLS.len(),
        "transport control count differs",
    )?;
    let mut offset = 0;
    for (number, row) in rows.iter().enumerate() {
        fields(row, "name exit argv observed")?;
        need(
            row["name"] == commands[number]["name"]
                && row["argv"] == commands[number]["argv"]
                && row["exit"].as_i64() == Some(0),
            "transport control is not typed exact actual0",
        )?;
        let count = if number == 3 { 2 } else { 1 };
        transport_observation(
            number,
            &row["observed"],
            &stdout[offset..offset + count],
            subject,
        )?;
        offset += count;
    }
    Ok(())
}

/// Validate independent native evidence first. The caller must separately run
/// full historical frontend validation with this private exact-pair binding.
pub(super) fn validate(seal: &Path, compiler: &str, component_sha: &str) -> R<Binding> {
    need(
        cfg!(unix),
        "raw native Targo hierarchy validation requires Unix file modes",
    )?;
    physical_directory(seal)?;
    need(
        aterm_codec::hex::is_lower(component_sha, 64),
        "invalid selected native Targo component digest",
    )?;
    let record_raw = bytes(&seal.join("TARGO-COMPONENT.json"), super::component::LIMIT)?;
    need(
        hash(&record_raw) == component_sha,
        "native Targo component digest differs",
    )?;
    let record = parse(&record_raw)?;
    shape(&record, compiler)?;
    let evidence = Evidence::load(seal)?;
    let before = base_inventory(&record, &evidence, seal)?;
    source_relation(&record, &evidence)?;
    let context = build(&record, &evidence)?;
    transport(&record, &evidence, &before)?;
    let payload = bytes(&seal.join("bin/targo"), 1 << 30)?;
    image(&record, &evidence, &context, &payload)?;
    for name in ["targo", "cargo"] {
        let path = seal.join("bin").join(name);
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        need(
            metadata.is_file()
                && permissions(&metadata)? == 0o755
                && bytes(&path, 1 << 30)? == payload,
            "native Targo pair is not identical regular executable payload",
        )?;
    }
    retained_frontend(seal, &record, &context)?;
    let (installed, after) = installation(&record, &evidence, &before)?;
    need(
        installed["component_sha256"].as_str() == Some(component_sha),
        "installation belongs to another native component",
    )?;
    need(
        inventory(seal)? == after && tool_map(seal)? == installed["after_tools"],
        "actual installed native hierarchy/tool map differs",
    )?;
    let qualified_raw = bytes(&seal.join("TARGO-QUALIFIED.json"), super::component::LIMIT)?;
    let actual = parse(&qualified_raw)?;
    qualified(
        &record,
        &installed,
        &evidence,
        &actual,
        component_sha,
        &context,
    )?;
    let subject = absolute(
        &evidence.json(&actual["context"], "native-qualified.context.json")?["selected_subject"],
    )?
    .to_owned();
    need(
        subject.file_name().and_then(|name| name.to_str()) == Some(inventory_stage(&before)?),
        "qualified subject renamed SDK hierarchy",
    )?;
    let mut footprint = evidence
        .raw
        .iter()
        .map(|(name, raw)| (seal.join(DETAILS).join(name), hash(raw)))
        .collect::<BTreeMap<_, _>>();
    footprint.insert(seal.join("TARGO-COMPONENT.json"), hash(&record_raw));
    footprint.insert(seal.join("TARGO-QUALIFIED.json"), hash(&qualified_raw));
    let base = &record["base"];
    let original = ["targo", "cargo"]
        .into_iter()
        .map(|name| {
            Ok((
                name.to_owned(),
                digest(&base["original_pair"][name])?.to_owned(),
            ))
        })
        .collect::<R<_>>()?;
    Ok(Binding {
        root: seal.to_owned(),
        compiler: compiler.to_owned(),
        compiler_sha256: digest(&base["compiler_sha256"])?.to_owned(),
        frontend_artifact_sha256: digest(&base["frontend_artifact_sha256"])?.to_owned(),
        original,
        replacement: digest(&record["artifact"]["sha256"])?.to_owned(),
        observed_inventory: after,
        footprint,
        seed_mint_sha256: digest(&base["mint_sha256"])?.to_owned(),
        frontend_component_sha256: digest(&base["frontend_component_sha256"])?.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_retirement_rejects_bool_incomplete_and_normalized_signal() {
        retirement(
            &aterm_json::json!({"completed":true,"actual_exit":0,"owned_command_drained":true}),
        )
        .unwrap();
        for exit in [
            V::from(false),
            V::from(true),
            V::from(130),
            V::from(-15),
            V::Null,
        ] {
            assert!(retirement(&aterm_json::json!({"completed":true,"actual_exit":exit,"owned_command_drained":true})).is_err());
        }
        assert!(
            retirement(
                &aterm_json::json!({"completed":true,"actual_exit":0,"owned_command_drained":false})
            )
            .is_err()
        );
        assert!(retirement(&aterm_json::json!({"completed":true,"actual_exit":0,"owned_command_drained":true,"unknown":0})).is_err());
    }

    #[test]
    fn exact_refs_and_versions_refuse_foreign_or_partial_authority() {
        let reference = aterm_json::json!({"path":"transport.stdout","sha256":"a".repeat(64)});
        fixed(&reference, "transport.stdout").unwrap();
        for name in [
            "../transport.stdout",
            "/transport.stdout",
            "transport.stderr",
        ] {
            assert!(
                fixed(
                    &aterm_json::json!({"path":name,"sha256":"a".repeat(64)}),
                    "transport.stdout"
                )
                .is_err()
            );
        }
        assert!(parse(br#"{"exit":0,"exit":0}"#).is_err());
        let source = "123456789aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let value = aterm_json::json!({"CFG_VER_HASH":source,"CFG_COMMIT_HASH":source,"CFG_SHORT_COMMIT_HASH":"123456789",
            "CFG_COMMIT_DATE":"2026-11-08","CFG_RELEASE":"1.101.0-dev","CFG_TRUST_VERSION":"0.13.0"});
        version_environment(&value, source).unwrap();
        let mut wrong = value.clone();
        *member_mut(&mut wrong, "CFG_SHORT_COMMIT_HASH").unwrap() = V::from("1234567");
        assert!(version_environment(&wrong, source).is_err());
        let mut wrong = value;
        *member_mut(&mut wrong, "CFG_VER_HASH").unwrap() = V::from(COMPILER);
        assert!(version_environment(&wrong, source).is_err());
        assert!(
            version(
                b"targo 1.101.0-dev\n",
                source,
                "targo",
                "1.101.0-dev",
                "0.13.0"
            )
            .is_err()
        );
    }

    #[test]
    fn whole_parent_pair_preserves_private_abi_and_rejects_partial_install() {
        let before = aterm_json::json!({"schema":"trust.sdk-component-inventory.v1","entries":[
            {"path":"rustc-private-sysroots","kind":"directory","mode":448},
            {"path":"rustc-private-sysroots/abi","kind":"file","mode":384,"bytes":1,"sha256":"a".repeat(64)},
            {"path":"sdk","kind":"directory","mode":493},
            {"path":"sdk/bin/cargo","kind":"file","mode":493,"bytes":1,"sha256":"b".repeat(64)},
            {"path":"sdk/bin/targo","kind":"file","mode":493,"bytes":1,"sha256":"b".repeat(64)}]});
        let artifact = aterm_json::json!({"mode":493,"bytes":2,"sha256":"c".repeat(64)});
        let mut after = before.clone();
        for row in member_mut(&mut after, "entries")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .skip(3)
        {
            *member_mut(row, "bytes").unwrap() = V::from(2);
            *member_mut(row, "sha256").unwrap() = artifact["sha256"].clone();
        }
        inventory_pair(&before, &after, &artifact).unwrap();
        let mut partial = after.clone();
        member_mut(&mut partial, "entries")
            .unwrap()
            .as_array_mut()
            .unwrap()[3] = before["entries"][3].clone();
        assert!(inventory_pair(&before, &partial, &artifact).is_err());
        *member_mut(
            &mut member_mut(&mut after, "entries")
                .unwrap()
                .as_array_mut()
                .unwrap()[1],
            "sha256",
        )
        .unwrap() = V::from("d".repeat(64));
        assert!(inventory_pair(&before, &after, &artifact).is_err());
        let mut sdk_only = before;
        drop(
            member_mut(&mut sdk_only, "entries")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .drain(..2),
        );
        assert!(inventory_shape(&sdk_only).is_err());
    }

    #[test]
    fn portable_canonical_hash_uses_python_ascii_and_utf16_escape_contract() {
        let value = aterm_json::json!({"z":0,"a":"β🙂"});
        assert_eq!(
            canonical(&value).unwrap(),
            br#"{"a":"\u03b2\ud83d\ude42","z":0}"#
        );
    }

    #[test]
    fn transport_frames_require_finite_matching_typed_status_and_canonical_bytes() {
        let commands = aterm_json::json!([{"name":"case","argv":[["selected","argument"]]}]);
        let row = aterm_json::json!({"name":"case","index":0,"argv":["selected","argument"],"exit":0,"raw_base64":"b2s="});
        let raw = aterm_json::to_vec(&row).unwrap();
        assert_eq!(frames(&raw, &commands).unwrap()[0].raw, b"ok");
        for value in [V::from(false), V::from(130), V::from(-15)] {
            let mut bad = row.clone();
            *member_mut(&mut bad, "exit").unwrap() = value;
            assert!(frames(&aterm_json::to_vec(&bad).unwrap(), &commands).is_err());
        }
        let mut bad = row;
        *member_mut(&mut bad, "raw_base64").unwrap() = V::from("b2t=");
        assert!(frames(&aterm_json::to_vec(&bad).unwrap(), &commands).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn finite_transport_rejects_wrong_pipe_false_zero_and_false_refusal() {
        let subject = Path::new("/unqualified/sdk");
        let child = aterm_json::json!({"kind":"native-run","observed_pipe":[1,2,1,2],"available_before":0,"available_after":0,
            "compiler_exit":0,"compiler_stdout_hex":aterm_codec::hex::encode_lower(b"/unqualified/sdk\n"),
            "compiler_stderr_hex":"","sysroot":"/unqualified/sdk","marker":"native-targo-custody-positive-v1"});
        let raw = |child: &V| format!("NATIVE_TARGO_CUSTODY {}\n", child).into_bytes();
        let mut observed = child.as_object().unwrap().clone();
        observed.remove("compiler_stdout_hex");
        observed.remove("compiler_stderr_hex");
        observed.insert("child_exit".to_owned(), V::from(0));
        observed.insert("original_pipe".to_owned(), aterm_json::json!([1, 2, 1, 2]));
        observed.insert(
            "compiler_stdout_sha256".to_owned(),
            V::from(hash(b"/unqualified/sdk\n")),
        );
        observed.insert("compiler_stderr_sha256".to_owned(), V::from(hash(b"")));
        let observed = V::Object(observed);
        transport_observation(
            2,
            &observed,
            &[Frame {
                code: 0,
                raw: raw(&child),
            }],
            subject,
        )
        .unwrap();
        let mut wrong = child.clone();
        *member_mut(&mut wrong, "available_after").unwrap() = V::from(false);
        assert!(
            transport_observation(
                2,
                &observed,
                &[Frame {
                    code: 0,
                    raw: raw(&wrong)
                }],
                subject
            )
            .is_err()
        );
        let mut wrong = child;
        *member_mut(&mut wrong, "observed_pipe").unwrap() = aterm_json::json!([9, 9, 9, 9]);
        assert!(
            transport_observation(
                2,
                &observed,
                &[Frame {
                    code: 0,
                    raw: raw(&wrong)
                }],
                subject
            )
            .is_err()
        );
        assert!(transport_observation(3, &aterm_json::json!({}), &[], subject).is_err());
    }
}
