// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content-checked test executables shared by the tree compile and release assembly.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

/// One Cargo test artifact, including the manifest needed by a direct invocation.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    /// Cargo package identity.
    pub package: String,
    /// Cargo target kind.
    pub kind: String,
    /// Cargo target name.
    pub name: String,
    /// Package manifest, as reported by Cargo.
    pub manifest_path: PathBuf,
    /// Executable, as reported by Cargo.
    pub path: PathBuf,
    /// SHA-256 of the executable's bytes.
    pub sha256: String,
}

/// A successful compile's executable evidence. It is never a test verdict.
#[derive(Debug, Deserialize, Serialize)]
pub struct Index {
    /// Candidate tree identity.
    pub tree: String,
    /// Compiler identity (`trustc -vV`).
    pub toolchain: String,
    /// Test executables built by the compile.
    pub binaries: Vec<Entry>,
}

/// Streaming digest of an executable.
pub fn digest(path: &Path) -> std::io::Result<String> {
    let mut input = crate::open_regular(path)?;
    let mut hash = aterm_digest::Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Read an identity from a child that exited successfully.
fn identity(program: &Path, root: &Path, args: &[&str]) -> std::io::Result<String> {
    let out = aterm_uds::spawnfd::command(program)
        .current_dir(root)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// A clean source tree, including inputs not yet tracked by Git.
fn clean_tree(root: &Path) -> std::io::Result<String> {
    if !identity(
        Path::new("git"),
        root,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?
    .is_empty()
    {
        return Err(std::io::Error::other(
            "test executable evidence requires a clean candidate",
        ));
    }
    identity(Path::new("git"), root, &["rev-parse", "HEAD^{tree}"])
}

/// The tree and compiler under which a compile or fixture run takes place.
pub fn identities(root: &Path, targo: &Path) -> std::io::Result<(String, String)> {
    Ok((
        clean_tree(root)?,
        identity(&targo.with_file_name("trustc"), root, &["-vV"])?,
    ))
}

impl Index {
    /// Build evidence from unambiguous Cargo compiler messages.
    pub fn from_messages(tree: String, toolchain: String, messages: &str) -> std::io::Result<Self> {
        let mut binaries = Vec::new();
        for line in messages.lines() {
            let Ok(crate::compiler_message::UnitMessage::Unit(v)) = aterm_json::from_str(line)
            else {
                continue;
            };
            if v.get("reason").and_then(aterm_json::Value::as_str) != Some("compiler-artifact")
                || v.get("profile")
                    .and_then(|p| p.get("test"))
                    .and_then(aterm_json::Value::as_bool)
                    != Some(true)
            {
                continue;
            }
            let text = |key| v.get(key).and_then(aterm_json::Value::as_str);
            let Some(path) = text("executable") else {
                continue;
            };
            let Some(package) = text("package_id") else {
                continue;
            };
            let Some(manifest) = text("manifest_path") else {
                continue;
            };
            let Some(name) = v
                .get("target")
                .and_then(|t| t.get("name"))
                .and_then(aterm_json::Value::as_str)
            else {
                continue;
            };
            let Ok(kinds) = crate::compiler_message::artifact_target_kinds(&v) else {
                continue;
            };
            let Some(kind) = kinds.first() else {
                continue;
            };
            binaries.push(Entry {
                package: package.into(),
                kind: (*kind).into(),
                name: name.into(),
                manifest_path: manifest.into(),
                path: path.into(),
                sha256: digest(Path::new(path))?,
            });
        }
        Ok(Self {
            tree,
            toolchain,
            binaries,
        })
    }

    /// Select only the candidate's unchanged GUI library test binary.
    pub fn fixture_binary(&self, tree: &str, toolchain: &str, root: &Path) -> Option<&Entry> {
        if self.tree != tree || self.toolchain != toolchain {
            return None;
        }
        let manifest = root.join("crates/aterm-gui/Cargo.toml");
        self.binaries.iter().find(|e| {
            e.kind == "lib"
                && e.name == "aterm_gui"
                && e.manifest_path == manifest
                && digest(&e.path).ok().as_deref() == Some(&e.sha256)
        })
    }

    /// Save the index beside verify's logs. Atomic replacement keeps readers off torn JSON.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("index has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let tmp = aterm_tempfile::TempDir::new_in(parent)?;
        let mut combined = Self {
            tree: self.tree.clone(),
            toolchain: self.toolchain.clone(),
            binaries: self.binaries.clone(),
        };
        if let Some(previous) =
            Self::read(path).filter(|i| i.tree == self.tree && i.toolchain == self.toolchain)
        {
            for entry in previous.binaries {
                if !combined.binaries.iter().any(|e| {
                    e.manifest_path == entry.manifest_path
                        && e.kind == entry.kind
                        && e.name == entry.name
                }) {
                    combined.binaries.push(entry);
                }
            }
        }
        let bytes = aterm_json::to_vec(&combined).map_err(std::io::Error::other)?;
        let staged = tmp.path().join("index.json");
        std::fs::write(&staged, bytes)?;
        std::fs::rename(staged, path)?;
        Ok(())
    }

    /// Missing, trimmed or unreadable evidence is a cache miss.
    pub fn read(path: &Path) -> Option<Self> {
        aterm_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
}

#[cfg(test)]
#[path = "test_index_tests.rs"]
mod tests;
