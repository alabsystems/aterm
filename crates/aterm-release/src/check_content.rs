// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Declared content read by each release check across adjacent line hops.
//!
//! The FAIL record remains keyed by a tree in [`crate::check_record`].  This module answers a
//! different question: whether a successful check of the line's immediately preceding head read
//! exactly the same content as it would at the new head.  The declaration is deliberately
//! conservative.  A check with no complete declaration has no key and therefore runs.  In
//! particular, a list of source paths is not a declaration for a compiler check whose build
//! scripts may open arbitrary files, or for a policy check which consults an external cache.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::PathBuf;

use aterm_uds::spawnfd::Command;
use aterm_verify::checks::Check;

use crate::ledger::GitRunner;

const CONTEXT_DOMAIN: &str = "aterm-release-check-context-v1";
const ITEM_CONTEXT_DOMAIN: &str = "aterm-release-item-context-v1";
const INPUT_DOMAIN: &str = "aterm-release-check-input-v1";

/// The canonical verifier mode whose individual check verdict a content key represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseHalf {
    Tree,
    Bundle,
}

impl ReleaseHalf {
    const fn flag(self) -> &'static [u8] {
        match self {
            Self::Tree => b"--tree",
            Self::Bundle => b"--bundle",
        }
    }
}

/// One immutable entry in a commit's tree.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    mode: String,
    object: String,
    path: String,
}

/// How a check's content set is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Declaration {
    /// One content set for the check.
    Whole,
    /// No complete bound is declared.  This check always runs.
    Undeclared(&'static str),
}

/// The declared input identity of one check at one commit.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// Frozen verifier, toolchain/build-input seal and finalized verify-child command context.
    pub context: String,
    /// H(context, every declared entry), absent when the reader is not bounded.
    pub key: Option<String>,
    /// Stable words retained in the record and printed when a check cannot be carried.
    pub declaration: String,
}

impl Input {
    /// Whether this input set can supply cross-hop evidence.
    #[must_use]
    pub const fn bounded(&self) -> bool {
        self.key.is_some()
    }
}

/// Every release check's declared input identity at one commit, under one execution context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub context: String,
    pub checks: BTreeMap<String, Input>,
}

impl Plan {
    /// The input of `check`; an unknown check is deliberately undeclared.
    #[must_use]
    pub fn input(&self, check: &str) -> Input {
        self.checks.get(check).cloned().unwrap_or_else(|| Input {
            context: self.context.clone(),
            key: None,
            declaration: format!("{check}'s runtime inputs are undeclared"),
        })
    }
}

/// H(the frozen cutter/verifier set, build-input seal and the finalized verify child's program,
/// working directory and effective environment). Binding the cutter makes a change to these
/// declarations invalidate the old interpretation as well as a changed verifier. Environment
/// values are hashed without ever being recorded.
///
/// The release pool is coordination, not a whole-check input. `test` reads the complete tree and
/// therefore cannot be carried whole across a hop. Its child commands do not receive the release
/// pool; their receipts bind the planned width and actual scheduler environment instead. Every
/// other check is currently undeclared and always reruns. Omitting only the pool here makes the
/// pre-qualify comparison identical to the command recorded inside the qualify pool without
/// weakening a carryable verdict; the tree child's `nice` program and arguments remain bound.
#[must_use]
pub fn context(
    frozen_tools_sha256: &str,
    seal: &str,
    command: &Command,
    half: ReleaseHalf,
) -> String {
    let mut environment: BTreeMap<Vec<u8>, Vec<u8>> = std::env::vars_os()
        .map(|(name, value)| (os_bytes(&name), os_bytes(&value)))
        .collect();
    for (name, value) in command.get_envs() {
        let name = os_bytes(name);
        if let Some(value) = value {
            environment.insert(name, os_bytes(value));
        } else {
            environment.remove(&name);
        }
    }
    environment.remove(&os_bytes(OsStr::new(aterm_capacity::POOL_ENV)));
    let mut h = aterm_digest::Sha256::new();
    part(&mut h, CONTEXT_DOMAIN.as_bytes());
    part(&mut h, frozen_tools_sha256.as_bytes());
    part(&mut h, seal.as_bytes());
    part(&mut h, &os_bytes(command.get_program()));
    for argument in command.get_args() {
        part(&mut h, &os_bytes(argument));
    }
    // Selection is represented by each input key's check name. Event/proof paths are outputs,
    // and prior-binary arguments affect only binary receipts. The half is the one canonical
    // behavior-affecting verifier argument shared by every verdict in this plan.
    part(&mut h, b"release");
    part(&mut h, half.flag());
    let cwd = command.get_current_dir().map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        PathBuf::from,
    );
    part(&mut h, &os_bytes(cwd.as_os_str()));
    for (name, value) in environment {
        part(&mut h, &name);
        part(&mut h, &value);
    }
    aterm_codec::hex::encode_lower(&h.finalize())
}

/// The stable release identity salted into each test-binary and lint-package finer-grained key.
/// Each item key also binds its running verifier/compiler bytes, toolchain files and finalized
/// declared environment. A bounded lint key normalizes only the exact paired FIFO pathname of the
/// active verifier-owned jobserver; its width and unknown flags remain exact or unbounded. Those
/// process capabilities therefore do not belong in this hop-stable salt.
#[must_use]
pub fn item_context(frozen_tools_sha256: &str, seal: &str) -> String {
    let mut h = aterm_digest::Sha256::new();
    part(&mut h, ITEM_CONTEXT_DOMAIN.as_bytes());
    part(&mut h, frozen_tools_sha256.as_bytes());
    part(&mut h, seal.as_bytes());
    aterm_codec::hex::encode_lower(&h.finalize())
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;
    value.encode_wide().flat_map(u16::to_le_bytes).collect()
}

#[cfg(not(any(unix, windows)))]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

/// Build the plan for `commit` from git's immutable tree objects.
///
/// # Errors
/// Git cannot list the tree, or prints a record this reader cannot bind.
pub fn plan(git: &dyn GitRunner, commit: &str, context: &str) -> Result<Plan, String> {
    let out = git
        .git(&["ls-tree", "-r", "-z", "--full-tree", commit])
        .map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(format!(
            "git cannot list {}'s content: {}",
            short(commit),
            out.stderr_utf8().trim()
        ));
    }
    let entries = parse_entries(&out.stdout)?;
    Ok(from_entries(&entries, context))
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

fn parse_entries(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for raw in bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = raw
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| "git ls-tree entry has no path".to_string())?;
        let header = std::str::from_utf8(&raw[..tab])
            .map_err(|_| "git ls-tree entry header is not UTF-8".to_string())?;
        let path = std::str::from_utf8(&raw[tab + 1..])
            .map_err(|_| "git ls-tree path is not UTF-8".to_string())?;
        let mut fields = header.split(' ');
        let (Some(mode), Some(kind), Some(object), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err("git ls-tree entry header has the wrong shape".into());
        };
        if mode.is_empty()
            || !mode.bytes().all(|byte| byte.is_ascii_digit())
            || !matches!(kind, "blob" | "commit")
            || !(object.len() == 40 || object.len() == 64)
            || !object.bytes().all(|byte| byte.is_ascii_hexdigit())
            || path.is_empty()
        {
            return Err(format!("git ls-tree entry is not bounded: {header}"));
        }
        entries.push(Entry {
            mode: mode.to_owned(),
            object: object.to_ascii_lowercase(),
            path: path.to_owned(),
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

fn from_entries(entries: &[Entry], context: &str) -> Plan {
    let mut checks = BTreeMap::new();
    for check in aterm_verify::checks::RELEASE_TREE {
        let declaration = declaration(check);
        let input = match declaration {
            Declaration::Undeclared(why) => Input {
                context: context.to_owned(),
                key: None,
                declaration: why.to_owned(),
            },
            Declaration::Whole => {
                let selected: Vec<&Entry> = entries
                    .iter()
                    .filter(|entry| reads(check, &entry.path))
                    .collect();
                Input {
                    context: context.to_owned(),
                    key: Some(input_key(check.name(), context, selected.iter().copied())),
                    declaration: declaration_words(check).to_owned(),
                }
            }
        };
        checks.insert(check.name().to_owned(), input);
    }
    Plan {
        context: context.to_owned(),
        checks,
    }
}

fn declaration(check: Check) -> Declaration {
    match check {
        Check::Test => Declaration::Whole,
        Check::Fmt => Declaration::Undeclared(
            "fmt reads ignored/untracked Rust and configuration files plus Git exclude state",
        ),
        Check::Lint => Declaration::Undeclared(
            "lint has no one whole-tree key; bounded workspace packages carry through their own compiler/dependency content receipts",
        ),
        Check::Doc => Declaration::Undeclared(
            "doc's compiler, dependency and build-script input closure is not declared",
        ),
        Check::Census => Declaration::Undeclared(
            "census reads publication history and baselines outside the candidate tree",
        ),
        Check::Forge => Declaration::Undeclared(
            "forge reads vendored policy and an external pristine-source cache",
        ),
        Check::Libc => Declaration::Undeclared(
            "libc's oracle, dependency closure and registry inputs are not declared",
        ),
        Check::Spec => Declaration::Undeclared(
            "spec's executable, proof bundle and resolved ty/ay tools are not declared",
        ),
        _ => Declaration::Undeclared("the check has no bounded content declaration"),
    }
}

fn declaration_words(check: Check) -> &'static str {
    match check {
        Check::Test => {
            "the tracked tree selecting binaries; each binary separately binds its runtime inputs"
        }
        _ => "undeclared",
    }
}

fn reads(check: Check, _path: &str) -> bool {
    match check {
        // The binary-level proof narrows execution.  The check-level declaration stays
        // conservative so a changed fixture cannot make the whole test verdict appear complete
        // without consulting those binary proofs.
        Check::Test => true,
        _ => false,
    }
}

fn input_key<'a>(check: &str, context: &str, entries: impl Iterator<Item = &'a Entry>) -> String {
    let mut h = aterm_digest::Sha256::new();
    part(&mut h, INPUT_DOMAIN.as_bytes());
    part(&mut h, check.as_bytes());
    part(&mut h, context.as_bytes());
    for entry in entries {
        entry_part(&mut h, entry);
    }
    aterm_codec::hex::encode_lower(&h.finalize())
}

fn entry_part(h: &mut aterm_digest::Sha256, entry: &Entry) {
    part(h, entry.mode.as_bytes());
    part(h, entry.object.as_bytes());
    part(h, entry.path.as_bytes());
}

fn part(h: &mut aterm_digest::Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(path: &str, byte: char) -> Entry {
        Entry {
            mode: "100644".into(),
            object: byte.to_string().repeat(40),
            path: path.into(),
        }
    }

    fn plan(entries: &[Entry], context: &str) -> Plan {
        from_entries(entries, context)
    }

    /// A cfg(test)-only edit changes the bounded readers. Readers whose complete closure is not
    /// declared have no key at either tree and therefore must run. Negative control: salting the
    /// context differently moves every bounded key, as a verifier, harness, environment or
    /// toolchain change must.
    #[test]
    fn a_test_only_edit_moves_only_the_bounded_check_inputs() {
        let base = vec![
            e("Cargo.toml", '1'),
            e("Cargo.lock", '2'),
            e("crates/aterm-gui/Cargo.toml", '3'),
            e("crates/aterm-gui/src/app.rs", '4'),
            e("crates/aterm-db/Cargo.toml", '5'),
            e("crates/aterm-db/src/lib.rs", '6'),
            e("crates/aterm-forge/src/lib.rs", '7'),
            e("crates/aterm-libc/src/lib.rs", '8'),
            e("crates/aterm-models-gui/src/lib.rs", '9'),
        ];
        let mut hop = base.clone();
        hop[3].object = "a".repeat(40);
        let before = plan(&base, "context-a");
        let after = plan(&hop, "context-a");
        assert_ne!(before.input("test").key, after.input("test").key);
        for undeclared in ["fmt", "lint", "doc", "census", "forge", "libc", "spec"] {
            assert!(!before.input(undeclared).bounded(), "{undeclared}");
            assert!(!after.input(undeclared).bounded(), "{undeclared}");
        }

        let other_context = plan(&base, "context-b");
        for check in aterm_verify::checks::RELEASE_TREE {
            let a = before.input(check.name());
            let b = other_context.input(check.name());
            if a.bounded() {
                assert_ne!(a.key, b.key, "{} binds the execution context", check.name());
            } else {
                assert_eq!(a.key, None, "{} stays fail-closed", check.name());
                assert_eq!(b.key, None, "{} stays fail-closed", check.name());
            }
        }
    }

    /// A fixture is part of the aggregate test input even when no executable byte names it. It is
    /// outside fmt's declared reader; every check with an undeclared closure still runs. This is
    /// the check-level half of the binary fixture negative control: the binary key further
    /// narrows which reader runs.
    #[test]
    fn a_fixture_change_moves_the_test_input_without_pretending_it_is_source() {
        let base = vec![
            e("Cargo.toml", '1'),
            e("Cargo.lock", '2'),
            e("crates/aterm-db/Cargo.toml", '3'),
            e("crates/aterm-db/src/lib.rs", '4'),
            e("crates/aterm-db/tests/fixtures/rows.bin", '5'),
        ];
        let mut hop = base.clone();
        hop[4].object = "6".repeat(40);
        let before = plan(&base, "context");
        let after = plan(&hop, "context");
        assert_ne!(before.input("test").key, after.input("test").key);
        for undeclared in ["fmt", "lint", "doc", "census", "forge", "libc", "spec"] {
            assert!(!before.input(undeclared).bounded(), "{undeclared}");
            assert!(!after.input(undeclared).bounded(), "{undeclared}");
        }
    }

    /// `fmt` is deliberately unbounded: ignored/untracked source or config files and repository
    /// or global Git exclude state can change its reader set without changing the commit tree.
    #[test]
    fn fmt_has_no_cross_hop_key_for_a_commit_tree_alone() {
        let base = vec![
            e("Cargo.toml", '1'),
            e("crates/a/Cargo.toml", '2'),
            e("crates/a/src/lib.rs", '3'),
            e("crates/a/.gitignore", '4'),
        ];
        let mut hop = base.clone();
        hop[3].object = "5".repeat(40);
        assert!(!plan(&base, "context").input("fmt").bounded());
        assert!(!plan(&hop, "context").input("fmt").bounded());
    }

    /// Facts are compared before the qualify pool exists, while the actual verifier runs inside
    /// it. No currently bounded whole-check verdict can cross a changed tree, but the context is
    /// still recorded with the aggregate test receipt and must stay identical across this
    /// production boundary. NEGATIVE CONTROL: an ordinary effective-environment change moves it.
    #[test]
    fn release_pool_coordination_does_not_move_whole_check_context() {
        let command = |pool: Option<&str>, ordinary: &str| {
            let mut command = Command::new("verify-fixture");
            command
                .current_dir("fixture-repository")
                .env("ATERM_CONTENT_CONTROL", ordinary);
            match pool {
                Some(value) => {
                    command.env(aterm_capacity::POOL_ENV, value);
                }
                None => {
                    command.env_remove(aterm_capacity::POOL_ENV);
                }
            }
            command
        };
        let outside = context("tools", "seal", &command(None, "same"), ReleaseHalf::Tree);
        let inside = context(
            "tools",
            "seal",
            &command(Some("12:/tmp/qualify.jobs"), "same"),
            ReleaseHalf::Tree,
        );
        assert_eq!(outside, inside);
        assert_ne!(
            outside,
            context(
                "tools",
                "seal",
                &command(None, "changed"),
                ReleaseHalf::Tree,
            )
        );
        assert_ne!(
            inside,
            context(
                "tools",
                "seal",
                &command(Some("12:/tmp/qualify.jobs"), "same"),
                ReleaseHalf::Bundle,
            ),
            "a behavior-affecting verifier mode is part of the context"
        );
    }

    /// The Unix tree wrapper is part of the child that a whole-check context describes, not a
    /// scheduling decoration. NEGATIVE CONTROLS: changing only its program, or each one of its
    /// arguments in turn, must move the context independently.
    #[test]
    fn whole_check_context_binds_the_verifier_program_and_each_argument() {
        let command = |program: &str, arguments: &[&str]| {
            let mut command = Command::new(program);
            command
                .args(arguments)
                .current_dir("fixture-repository")
                .env("ATERM_CONTENT_CONTROL", "same");
            command
        };
        let arguments = ["-n", "10", "/frozen/verifier"];
        let base = context(
            "tools",
            "seal",
            &command("/usr/bin/nice", &arguments),
            ReleaseHalf::Tree,
        );
        assert_ne!(
            base,
            context(
                "tools",
                "seal",
                &command("/fixture/not-nice", &arguments),
                ReleaseHalf::Tree,
            ),
            "the wrapper program is an input even when every argument is equal"
        );
        for (index, replacement) in ["--adjustment", "11", "/other/verifier"]
            .into_iter()
            .enumerate()
        {
            let mut changed = arguments;
            changed[index] = replacement;
            assert_ne!(
                base,
                context(
                    "tools",
                    "seal",
                    &command("/usr/bin/nice", &changed),
                    ReleaseHalf::Tree,
                ),
                "wrapper argument {index} is an independent context input"
            );
        }
    }

    /// Fine-grained test and lint receipts share one hop-stable salt. A changed frozen tool set
    /// or build-input seal invalidates every item even when its own bytes are equal.
    #[test]
    fn item_context_binds_the_frozen_tools_and_build_inputs() {
        let base = item_context("tools-a", "seal-a");
        assert_eq!(base, item_context("tools-a", "seal-a"));
        assert_ne!(base, item_context("tools-b", "seal-a"));
        assert_ne!(base, item_context("tools-a", "seal-b"));
    }

    #[test]
    fn ls_tree_records_are_bounded_before_they_are_keys() {
        let text = format!(
            "100644 blob {}\tcrates/a/Cargo.toml\0100644 blob {}\tcrates/a/src/lib.rs\0",
            "a".repeat(40),
            "b".repeat(40)
        );
        let entries = parse_entries(text.as_bytes()).unwrap();
        assert_eq!(entries.len(), 2);
        for bad in [
            b"100644 blob abc\tp\0".as_slice(),
            b"100644 tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tp\0".as_slice(),
            b"100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa p\0".as_slice(),
        ] {
            assert!(parse_entries(bad).is_err(), "{bad:?}");
        }
    }
}
