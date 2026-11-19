// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content-checked test executables shared by the tree compile and release assembly.

use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const SOURCE_READ_WINDOW: Duration = Duration::from_secs(60);

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

/// Read a complete identity from a successful child while its output or process tree progresses.
/// A minute of confirmed inactivity or a run ending stops the read; oversized stdout is refused.
fn identity(program: &Path, root: &Path, args: &[&str]) -> std::io::Result<String> {
    let mut command = aterm_uds::spawnfd::command(program);
    command.current_dir(root).args(args);
    read_identity(&mut command, &program.display().to_string())
}

fn read_identity(
    command: &mut aterm_uds::spawnfd::Command,
    label: &str,
) -> std::io::Result<String> {
    let out = aterm_uds::deadline::output_while_process_progressing_complete_stdout_unless(
        command,
        SOURCE_READ_WINDOW,
        format!("test executable identity from {label}"),
        &mut crate::exec::ending,
    )
    .map_err(|error| {
        std::io::Error::other(format!("test executable identity from {label}: {error}"))
    })?;
    if !out.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn source_git(root: &Path, path_env: &OsStr) -> aterm_uds::spawnfd::Command {
    let mut command = crate::changed::git(root, path_env);
    command
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ALLOW_PROTOCOL", "")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.ignoreStat=false",
            "-c",
            "core.trustctime=true",
            "-c",
            "core.checkStat=default",
        ]);
    command
}

fn git_identity(root: &Path, path_env: &OsStr, args: &[&str]) -> std::io::Result<String> {
    read_identity(source_git(root, path_env).args(args), "Git source")
}

fn clean_source(root: &Path, path_env: &OsStr) -> std::io::Result<()> {
    let status = [
        "status",
        "--porcelain=v1",
        "--untracked-files=normal",
        "--ignore-submodules=none",
    ];
    if !git_identity(root, path_env, &status)?.is_empty() {
        return Err(std::io::Error::other(
            "test executable evidence requires a clean candidate",
        ));
    }

    // A normal status can omit assume-unchanged or skip-worktree entries. Read the
    // whole index to EOF, keeping only entries that cannot be accepted on that basis.
    let mut git = source_git(root, path_env);
    git.args(["ls-files", "-v", "--stage", "-z"]);
    let index =
        aterm_uds::deadline::output_while_process_progressing_complete_stdout_records_unless(
            &mut git,
            SOURCE_READ_WINDOW,
            "test source index",
            0,
            aterm_uds::deadline::OUTPUT_KEEP,
            |entry| {
                ![b"H 100644 ", b"H 100755 ", b"H 120000 "]
                    .iter()
                    .any(|prefix| entry.starts_with(*prefix))
            },
            &mut crate::exec::ending,
        )
        .map_err(std::io::Error::other)?;
    if !index.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&index.stderr).into_owned(),
        ));
    }
    for entry in index
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = std::str::from_utf8(entry).map_err(std::io::Error::other)?;
        let (object, path) = entry
            .strip_prefix("H 160000 ")
            .and_then(|entry| entry.split_once(" 0\t"))
            .ok_or_else(|| {
                std::io::Error::other("test source index contains a hidden or unmerged entry")
            })?;
        let path = Path::new(path);
        let child = root.join(path);
        let head = ["rev-parse", "--verify", "HEAD"];
        if path.as_os_str().is_empty()
            || !path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            || child.canonicalize()? != child
            || !git_identity(&child, path_env, &["rev-parse", "--show-prefix"])?.is_empty()
            || git_identity(&child, path_env, &head)? != object
        {
            return Err(std::io::Error::other(
                "test source submodule is not at its recorded checkout",
            ));
        }
        clean_source(&child, path_env)?;
        if git_identity(&child, path_env, &head)? != object {
            return Err(std::io::Error::other(
                "test source submodule changed during observation",
            ));
        }
    }
    Ok(())
}

/// A clean source tree, including inputs not yet tracked by Git.
fn clean_tree(root: &Path) -> std::io::Result<String> {
    let root = root.canonicalize()?;
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    if !git_identity(&root, &path_env, &["rev-parse", "--show-prefix"])?.is_empty() {
        return Err(std::io::Error::other("test source is not a checkout root"));
    }
    let head = ["rev-parse", "--verify", "HEAD^{tree}"];
    let tree = git_identity(&root, &path_env, &head)?;
    clean_source(&root, &path_env)?;
    if git_identity(&root, &path_env, &head)? != tree {
        return Err(std::io::Error::other(
            "test source tree changed during observation",
        ));
    }
    Ok(tree)
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
        Self::from_messages_with(tree, toolchain, messages, digest)
    }

    pub(crate) fn from_messages_with(
        tree: String,
        toolchain: String,
        messages: &str,
        mut inspect: impl FnMut(&Path) -> std::io::Result<String>,
    ) -> std::io::Result<Self> {
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
                sha256: inspect(Path::new(path))?,
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
