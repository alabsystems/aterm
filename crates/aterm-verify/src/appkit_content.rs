// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Executable-byte evidence for carrying one AppKit driver group across a release hop.
//!
//! A source-path cone cannot bound a compiler whose build scripts or procedural macros may open
//! arbitrary files.  Each hop therefore builds the current drivers first and compares the exact
//! executable bytes it would drive. A prior success is usable only under the same frozen caller
//! context, exact ordered target set, and finalized child runtime (program, argv, private cwd and
//! complete environment). The two groups stay separate so an unchanged group need not open
//! windows when the other one changed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The proof and JSONL wire schema.
pub const SCHEMA: u32 = 1;
/// The redraw-conformance executable group.
pub const REDRAW_GROUP: &str = "redraw-conformance";
/// The live Objective-C example group.
pub const OBJC_GROUP: &str = "objc-drives";

const KEY_DOMAIN: &str = "aterm-appkit-group-content-v2";
const RUNTIME_DOMAIN: &str = "aterm-appkit-finalized-runtime-v1";
/// Public digest-only construction is useful to parse and exercise the wire, but cannot match a
/// verifier-produced proof because it says nothing about the child runtime.
const UNBOUND_RUNTIME_DOMAIN: &str = "aterm-appkit-runtime-unbound-v1";

/// The child-visible part of one finalized command. These bytes are deliberately not on the wire:
/// the next hop independently reconstructs its would-be command and must produce the same opaque
/// content key. Keeping them out of the event also keeps paths and environment values out of logs.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimeCommand {
    program: Vec<u8>,
    args: Vec<Vec<u8>>,
    cwd: Vec<u8>,
    env: Vec<(Vec<u8>, Vec<u8>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Runtime {
    commands: Vec<RuntimeCommand>,
}

impl Runtime {
    fn from_commands(
        paths: &[(&str, PathBuf)],
        commands: &[crate::exec::Cmd],
    ) -> Result<Self, String> {
        if commands.len() != paths.len() {
            return Err(format!(
                "AppKit runtime has {} commands for {} executables",
                commands.len(),
                paths.len()
            ));
        }
        let mut receipts = Vec::with_capacity(commands.len());
        for ((name, path), command) in paths.iter().zip(commands) {
            if !command.runtime_environment_is_finalized() {
                return Err(format!(
                    "AppKit target {name:?} has no finalized runtime-environment receipt"
                ));
            }
            if !command.exact_env || !command.waits_count || command.demoted {
                return Err(format!(
                    "AppKit target {name:?} is not a stable exact-environment command"
                ));
            }
            if command.program != *path || !command.program.is_absolute() {
                return Err(format!(
                    "AppKit target {name:?} runtime program {:?} is not its absolute built path {}",
                    command.program,
                    path.display()
                ));
            }
            let cwd = command.cwd.as_ref().ok_or_else(|| {
                format!("AppKit target {name:?} has no explicit working directory")
            })?;
            if !cwd.is_absolute() {
                return Err(format!(
                    "AppKit target {name:?} working directory is not absolute"
                ));
            }
            let mut names = std::collections::BTreeSet::new();
            let mut environment = Vec::with_capacity(command.envs.len());
            for (key, value) in &command.envs {
                let key = key.as_os_str().as_encoded_bytes().to_vec();
                if !names.insert(key.clone()) {
                    return Err(format!(
                        "AppKit target {name:?} runtime repeats environment key {:?}",
                        String::from_utf8_lossy(&key)
                    ));
                }
                environment.push((key, value.as_os_str().as_encoded_bytes().to_vec()));
            }
            receipts.push(RuntimeCommand {
                program: command.program.as_os_str().as_encoded_bytes().to_vec(),
                args: command
                    .args
                    .iter()
                    .map(|arg| arg.as_os_str().as_encoded_bytes().to_vec())
                    .collect(),
                cwd: cwd.as_os_str().as_encoded_bytes().to_vec(),
                env: environment,
            });
        }
        Ok(Self { commands: receipts })
    }
}

/// One executable in a group's canonical order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Executable {
    pub name: String,
    pub sha256: String,
}

/// One current or prior AppKit group identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupProof {
    pub schema: u32,
    pub group: String,
    pub content_key: String,
    pub executables: Vec<Executable>,
}

/// Prior successful group proofs, keyed by group.
pub type Passed = BTreeMap<String, GroupProof>;

impl GroupProof {
    /// Hash binaries at `paths` into an explicitly runtime-unbound wire fixture.
    ///
    /// A verifier carry decision never uses this constructor: it cannot match
    /// [`Self::from_paths_and_commands`]'s content key.
    ///
    /// # Errors
    /// The context is not a canonical digest, the group/target order is not canonical, or an
    /// executable cannot be read as a regular file.
    pub fn from_paths(
        group: &str,
        carry_context: &str,
        paths: &[(&str, PathBuf)],
    ) -> Result<Self, String> {
        let executables = paths
            .iter()
            .map(|(name, path)| {
                crate::test_index::digest(path)
                    .map(|sha256| Executable {
                        name: (*name).to_owned(),
                        sha256,
                    })
                    .map_err(|error| format!("hash AppKit driver {}: {error}", path.display()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_executables(group, carry_context, executables)
    }

    /// Construct an explicitly runtime-unbound wire fixture from measured executable digests.
    ///
    /// A verifier carry decision never uses this constructor: it cannot match
    /// [`Self::from_paths_and_commands`]'s content key.
    ///
    /// # Errors
    /// The context, group, target order, or digest is not canonical.
    pub fn from_executables(
        group: &str,
        carry_context: &str,
        executables: Vec<Executable>,
    ) -> Result<Self, String> {
        if !aterm_codec::hex::is_lower(carry_context, 64) {
            return Err("AppKit carry context is not 64 lowercase hex digits".into());
        }
        validate_targets(group, &executables)?;
        let content_key = content_key(group, carry_context, &executables, None);
        Ok(Self {
            schema: SCHEMA,
            group: group.to_owned(),
            content_key,
            executables,
        })
    }

    /// Hash exact binaries together with the finalized commands which would drive them.
    ///
    /// # Errors
    /// As [`Self::from_paths`], or a command is not the exact finalized runtime for its matching
    /// executable. A verifier carry decision uses only this constructor, never the digest-only
    /// public constructor.
    pub(crate) fn from_paths_and_commands(
        group: &str,
        carry_context: &str,
        paths: &[(&str, PathBuf)],
        commands: &[crate::exec::Cmd],
    ) -> Result<Self, String> {
        let executables = paths
            .iter()
            .map(|(name, path)| {
                crate::test_index::digest(path)
                    .map(|sha256| Executable {
                        name: (*name).to_owned(),
                        sha256,
                    })
                    .map_err(|error| format!("hash AppKit driver {}: {error}", path.display()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !aterm_codec::hex::is_lower(carry_context, 64) {
            return Err("AppKit carry context is not 64 lowercase hex digits".into());
        }
        validate_targets(group, &executables)?;
        let runtime = Runtime::from_commands(paths, commands)?;
        let content_key = content_key(group, carry_context, &executables, Some(&runtime));
        Ok(Self {
            schema: SCHEMA,
            group: group.to_owned(),
            content_key,
            executables,
        })
    }

    fn validate(&self, line: usize) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!(
                "line {line} has AppKit proof schema {}, expected {SCHEMA}",
                self.schema
            ));
        }
        if !aterm_codec::hex::is_lower(&self.content_key, 64) {
            return Err(format!("line {line} has an invalid AppKit content key"));
        }
        validate_targets(&self.group, &self.executables)
            .map_err(|why| format!("line {line}: {why}"))
    }
}

fn expected(group: &str) -> Option<Vec<&'static str>> {
    match group {
        REDRAW_GROUP => Some(vec![crate::stages::REDRAW_CONFORMANCE_BIN]),
        OBJC_GROUP => Some(
            crate::stages::OBJC_DRIVES
                .iter()
                .map(|drive| drive.example)
                .collect(),
        ),
        _ => None,
    }
}

fn validate_targets(group: &str, executables: &[Executable]) -> Result<(), String> {
    let expected = expected(group).ok_or_else(|| format!("unknown AppKit group {group:?}"))?;
    let actual: Vec<&str> = executables
        .iter()
        .map(|executable| executable.name.as_str())
        .collect();
    if actual != expected {
        return Err(format!(
            "AppKit group {group:?} has targets {actual:?}, expected {expected:?} in that order"
        ));
    }
    if let Some(executable) = executables
        .iter()
        .find(|executable| !aterm_codec::hex::is_lower(&executable.sha256, 64))
    {
        return Err(format!(
            "AppKit target {:?} has an invalid executable SHA-256",
            executable.name
        ));
    }
    Ok(())
}

fn content_key(
    group: &str,
    carry_context: &str,
    executables: &[Executable],
    runtime: Option<&Runtime>,
) -> String {
    let mut hash = aterm_digest::Sha256::new();
    part(&mut hash, KEY_DOMAIN.as_bytes());
    part(&mut hash, carry_context.as_bytes());
    part(&mut hash, group.as_bytes());
    for executable in executables {
        part(&mut hash, executable.name.as_bytes());
        part(&mut hash, executable.sha256.as_bytes());
    }
    match runtime {
        Some(runtime) => {
            part(&mut hash, RUNTIME_DOMAIN.as_bytes());
            part(&mut hash, &(runtime.commands.len() as u64).to_le_bytes());
            for command in &runtime.commands {
                part(&mut hash, &command.program);
                part(&mut hash, &(command.args.len() as u64).to_le_bytes());
                for argument in &command.args {
                    part(&mut hash, argument);
                }
                part(&mut hash, &command.cwd);
                part(&mut hash, &(command.env.len() as u64).to_le_bytes());
                for (name, value) in &command.env {
                    part(&mut hash, name);
                    part(&mut hash, value);
                }
            }
        }
        None => part(&mut hash, UNBOUND_RUNTIME_DOMAIN.as_bytes()),
    }
    aterm_codec::hex::encode_lower(&hash.finalize())
}

fn part(hash: &mut aterm_digest::Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

/// Read versioned JSONL proofs.
///
/// # Errors
/// A line is not a canonical schema-1 proof or repeats a group.
pub fn read_passed(text: &str) -> Result<Passed, String> {
    let mut passed = Passed::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_number = index + 1;
        let proof: GroupProof = aterm_json::from_str(line)
            .map_err(|error| format!("line {line_number} is not an AppKit group proof: {error}"))?;
        proof.validate(line_number)?;
        let group = proof.group.clone();
        if passed.insert(group.clone(), proof).is_some() {
            return Err(format!("line {line_number} repeats AppKit group {group:?}"));
        }
    }
    Ok(passed)
}

/// Encode proofs in group order as versioned JSONL.
///
/// # Errors
/// A proof is invalid, its map key disagrees with its group, or JSON serialization fails.
pub fn encode_passed(passed: &Passed) -> Result<String, String> {
    let mut text = String::new();
    for (group, proof) in passed {
        if group != &proof.group {
            return Err(format!(
                "AppKit proof map key {group:?} disagrees with group {:?}",
                proof.group
            ));
        }
        proof.validate(text.lines().count() + 1)?;
        text.push_str(&aterm_json::to_string(proof).map_err(|error| error.to_string())?);
        text.push('\n');
    }
    Ok(text)
}

/// A completed current group, for the release cutter's record.
#[must_use]
pub(crate) fn event(proof: &GroupProof, reused: bool) -> String {
    format!(
        "verify-event: {}\n",
        crate::event_log::event_value(
            crate::event_log::EventKind::AppkitGroupFinished,
            aterm_json::json!({
                "group": proof.group,
                "content_key": proof.content_key,
                "executables": proof.executables,
                "reused": reused,
            })
        )
    )
}

#[cfg(test)]
#[path = "appkit_content_tests.rs"]
mod tests;
