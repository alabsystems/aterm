// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The reviewed astream direct-path bundle. This is a separate, named
//! provenance obligation, not an exemption for directories outside the patch
//! table. Its hashes pin a reviewed copy; they are not an upstream signature.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_json::Value;
use aterm_toml::edit::{DocumentMut, Item};

pub(crate) const DIRECTORY: &str = "astream";
const BUNDLE: &str = "vendor/astream";
const INVENTORY: &str = "UPSTREAM.toml";
// Re-synchronizing requires a new review of the inventory as well as its bytes.
const RECORD_SHA256: &str = "95dfe0c8ca1198da6a82f6ad7a0863c60c624274e49f9653b6be331f859be298";
const UPSTREAM: &str = "https://github.com/alabsystems/astream";
const REVISION: &str = "bb98d610894afebdbe0ab741a7ad155b942a4375";
const PACKAGES: [&str; 4] = [
    "astream-aead",
    "astream-broker",
    "astream-cap",
    "astream-wire",
];

fn read_doc(path: &Path) -> Result<DocumentMut, String> {
    std::fs::read_to_string(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn metadata(root: &Path) -> Result<Value, String> {
    let exe = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(exe)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--locked",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot read Cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Cargo metadata refused: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    aterm_json::from_slice(&out.stdout).map_err(|e| format!("invalid Cargo metadata: {e}"))
}

fn tracked_files(root: &Path) -> Result<BTreeSet<String>, String> {
    let out = Command::new("git")
        .args(["ls-files", "-z", "--", BUNDLE])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot read tracked bundle inventory: {e}"))?;
    if !out.status.success() {
        return Err("git could not read the tracked bundle inventory".into());
    }
    String::from_utf8(out.stdout)
        .map_err(|e| format!("non-UTF-8 tracked path: {e}"))?
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.strip_prefix(&format!("{BUNDLE}/"))
                .map(str::to_owned)
                .ok_or_else(|| format!("tracked path outside the bundle: {s}"))
        })
        .collect()
}

/// Called only for the named bundle or its live consumer. Other vendor
/// directories still owe the ordinary patch-table obligation.
pub(crate) fn review(root: &Path) -> Result<bool, String> {
    let consumer = root.join("crates/aterm-link/Cargo.toml");
    if !root.join(BUNDLE).exists() && !consumer.exists() {
        return Ok(false);
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("workspace root: {e}"))?;
    let record_path = root.join(BUNDLE).join(INVENTORY);
    let bytes = std::fs::read(&record_path)
        .map_err(|e| format!("review inventory {}: {e}", record_path.display()))?;
    if crate::mirror::sha256_hex(&bytes) != RECORD_SHA256 {
        return Err("the source inventory changed without a renewed review".into());
    }
    let record = read_doc(&record_path)?;
    let tracked = tracked_files(&root)?;
    let metadata = metadata(&root)?;
    validate(&root, &record, &tracked, &metadata)?;
    Ok(true)
}

fn validate(
    root: &Path,
    record: &DocumentMut,
    tracked: &BTreeSet<String>,
    metadata: &Value,
) -> Result<(), String> {
    let upstream = record
        .get("upstream")
        .and_then(Item::as_table_like)
        .ok_or("missing [upstream] review record")?;
    for (key, want) in [
        ("repository", UPSTREAM),
        ("revision", REVISION),
        ("license", "Apache-2.0"),
    ] {
        if upstream.get(key).and_then(Item::as_str) != Some(want) {
            return Err(format!("unreviewed upstream {key}; expected {want}"));
        }
    }
    let hashes = record
        .get("files")
        .and_then(Item::as_table_like)
        .ok_or("missing reviewed [files] inventory")?;
    let mut expected = BTreeSet::from([INVENTORY.to_owned()]);
    let bundle = root
        .join(BUNDLE)
        .canonicalize()
        .map_err(|e| format!("missing bundle: {e}"))?;
    if bundle != root.join(BUNDLE) {
        return Err("bundle is redirected outside its reviewed path".into());
    }
    for (rel, hash) in hashes.iter() {
        let path = Path::new(rel);
        if path
            .components()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
        {
            return Err(format!("invalid reviewed relative path: {rel}"));
        }
        expected.insert(rel.to_owned());
        let file = bundle.join(path);
        if file.canonicalize().map_err(|e| format!("{rel}: {e}"))? != file {
            return Err(format!("reviewed file is redirected: {rel}"));
        }
        let bytes = std::fs::read(&file).map_err(|e| format!("{rel}: {e}"))?;
        if hash.as_str() != Some(crate::mirror::sha256_hex(&bytes).as_str()) {
            return Err(format!("reviewed bytes changed: {BUNDLE}/{rel}"));
        }
    }
    if &expected != tracked {
        return Err(format!(
            "tracked inventory differs: missing {:?}; unreviewed {:?}",
            expected.difference(tracked).collect::<Vec<_>>(),
            tracked.difference(&expected).collect::<Vec<_>>()
        ));
    }
    for required in ["README.md", "LICENSE"] {
        if !expected.contains(required) {
            return Err(format!("review omitted required {required}"));
        }
    }
    let license = std::fs::read_to_string(bundle.join("LICENSE")).map_err(|e| e.to_string())?;
    if license != std::fs::read_to_string(root.join("LICENSE")).map_err(|e| e.to_string())? {
        return Err("supplied Apache-2.0 text differs from the workspace license".into());
    }
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("Cargo metadata has no packages")?;
    let members = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .ok_or("Cargo metadata has no workspace members")?;
    for name in PACKAGES {
        let rel = format!("crates/{name}/Cargo.toml");
        if !expected.contains(&rel) || !expected.contains(&format!("crates/{name}/src/lib.rs")) {
            return Err(format!("review omitted {name}'s manifest or library"));
        }
        let manifest = bundle.join(&rel);
        let doc = read_doc(&manifest)?;
        let package = doc
            .get("package")
            .and_then(Item::as_table_like)
            .ok_or("missing [package]")?;
        for (key, want) in [
            ("name", name),
            ("version", "0.1.0"),
            ("license", "Apache-2.0"),
        ] {
            if package.get(key).and_then(Item::as_str) != Some(want) {
                return Err(format!("{name} manifest has unreviewed {key}"));
            }
        }
        let matches: Vec<_> = packages
            .iter()
            .filter(|p| p.get("name").and_then(Value::as_str) == Some(name))
            .collect();
        if matches.len() != 1 {
            return Err(format!("Cargo metadata must resolve exactly one {name}"));
        }
        let p = matches[0];
        let actual = p
            .get("manifest_path")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        if actual.as_ref() != Some(&manifest)
            || p.get("source").is_none_or(|s| !s.is_null())
            || p.get("version").and_then(Value::as_str) != Some("0.1.0")
            || !members.iter().any(|id| Some(id) == p.get("id"))
        {
            return Err(format!(
                "Cargo metadata redirects {name} away from its reviewed source"
            ));
        }
    }
    let consumer = packages
        .iter()
        .find(|p| p.get("name").and_then(Value::as_str) == Some("aterm-link"))
        .ok_or("the reviewed bundle has no aterm-link consumer")?;
    if consumer
        .get("manifest_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        != Some(root.join("crates/aterm-link/Cargo.toml"))
    {
        return Err("aterm-link is outside its reviewed workspace path".into());
    }
    let deps = consumer
        .get("dependencies")
        .and_then(Value::as_array)
        .ok_or("aterm-link has no dependencies")?;
    for name in ["astream-broker", "astream-cap"] {
        let dep = deps
            .iter()
            .find(|d| d.get("name").and_then(Value::as_str) == Some(name))
            .ok_or_else(|| format!("aterm-link no longer uses {name}"))?;
        if dep.get("path").and_then(Value::as_str).map(PathBuf::from)
            != Some(bundle.join("crates").join(name))
        {
            return Err(format!(
                "aterm-link redirects {name} away from its reviewed path"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_direct_bundle_has_reviewed_bytes_and_resolved_paths() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert_eq!(review(root), Ok(true));
    }

    #[test]
    fn the_review_rejects_changed_bytes_missing_tracking_and_redirected_cargo_paths() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        let record = read_doc(&root.join(BUNDLE).join(INVENTORY)).unwrap();
        let tracked = tracked_files(&root).unwrap();
        let metadata = metadata(&root).unwrap();
        assert!(validate(&root, &record, &tracked, &metadata).is_ok());
        let mut changed = record.clone();
        *changed["files"]
            .get_mut("crates/astream-cap/src/lib.rs")
            .unwrap() = Item::Value("00".into());
        assert!(
            validate(&root, &changed, &tracked, &metadata)
                .unwrap_err()
                .contains("reviewed bytes changed")
        );
        let mut missing = tracked.clone();
        missing.remove("crates/astream-cap/src/lib.rs");
        assert!(
            validate(&root, &record, &missing, &metadata)
                .unwrap_err()
                .contains("tracked inventory differs")
        );
        let mut redirected = metadata.clone();
        for p in redirected
            .as_object_mut()
            .unwrap()
            .get_mut("packages")
            .unwrap()
            .as_array_mut()
            .unwrap()
        {
            if p["name"].as_str() == Some("astream-cap") {
                *p.as_object_mut().unwrap().get_mut("manifest_path").unwrap() =
                    Value::String("/outside/Cargo.toml".into());
            }
        }
        assert!(
            validate(&root, &record, &tracked, &redirected)
                .unwrap_err()
                .contains("redirects astream-cap")
        );
        let mut dependency = metadata.clone();
        for p in dependency
            .as_object_mut()
            .unwrap()
            .get_mut("packages")
            .unwrap()
            .as_array_mut()
            .unwrap()
        {
            if p["name"].as_str() == Some("aterm-link") {
                for d in p
                    .as_object_mut()
                    .unwrap()
                    .get_mut("dependencies")
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
                {
                    if d["name"].as_str() == Some("astream-cap") {
                        *d.as_object_mut().unwrap().get_mut("path").unwrap() =
                            Value::String("/outside".into());
                    }
                }
            }
        }
        assert!(
            validate(&root, &record, &tracked, &dependency)
                .unwrap_err()
                .contains("aterm-link redirects astream-cap")
        );
        let mut unknown = tracked;
        unknown.insert("unreviewed.rs".into());
        assert!(
            validate(&root, &record, &unknown, &metadata)
                .unwrap_err()
                .contains("tracked inventory differs")
        );
    }
}
