// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

use std::collections::BTreeSet;
use std::hash::Hasher as _;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    anchor_rules::emit();
    rules_digest();
}

/// `ATERM_SECRETS_RULES`: a digest of every source this crate's build reads its rules from (this
/// crate's `src`, and astream-aead's, whose strict Ed25519 check verifies every record), test
/// modules left out, and of the release of ring that `Cargo.lock` locks, whose verifier that check
/// ends in (`astream_aead::signature::verify`). The vault's head (`membership_log::head`) is read
/// only under the rules it was written under: a build whose resolve, record checks or head layout
/// differ in any byte, or that verifies signatures with another ring, reads no head an earlier
/// build wrote. Each source file is named to Cargo, so a change to a test module alone leaves the
/// build unit fresh. `Cargo.lock` is not named: an edit of it that moves no ring leaves the rules
/// as they were, and one that moves ring gives ring another package id, and with it another
/// `links` name (`ring_core_<version>_`), whose build script's run this one's run follows, so
/// Cargo runs this script again. Every input's name is written to `OUT_DIR/rules_inputs.txt`,
/// which a test reads.
fn rules_digest() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by Cargo"));
    let roots = [
        ("aterm-secrets", manifest.join("src")),
        (
            "astream-aead",
            manifest.join("../../vendor/astream/crates/astream-aead/src"),
        ),
    ];
    let mut files = Vec::new();
    for (label, root) in &roots {
        let mut test_only = BTreeSet::new();
        let mut all = Vec::new();
        sources(root, &mut all);
        for path in &all {
            test_only_modules(path, &mut test_only);
        }
        for path in all {
            if test_only.iter().any(|excluded| path.starts_with(excluded)) {
                continue;
            }
            let relative = path.strip_prefix(root).unwrap_or(&path);
            files.push((format!("{label}:{}", relative.display()), path.clone()));
        }
    }
    files.sort();
    let ring = locked_ring(&manifest.join("../../Cargo.lock"));
    // Two independent 64-bit SipHash runs: a digest for telling builds apart, not a key.
    let mut hashes = [
        std::hash::DefaultHasher::new(),
        std::hash::DefaultHasher::new(),
    ];
    hashes[1].write(b"aterm-secrets rules");
    let mut inputs = String::new();
    for (name, path) in &files {
        println!("cargo::rerun-if-changed={}", path.display());
        let bytes = std::fs::read(path).expect("a source file reads");
        for hash in &mut hashes {
            hash.write(name.as_bytes());
            hash.write_u8(0);
            hash.write_usize(bytes.len());
            hash.write(&bytes);
        }
        inputs.push_str(name);
        inputs.push('\n');
    }
    let name = format!("Cargo.lock:ring {ring}");
    for hash in &mut hashes {
        hash.write(name.as_bytes());
        hash.write_u8(0);
    }
    inputs.push_str(&name);
    inputs.push('\n');
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("set by Cargo"));
    std::fs::write(out.join("rules_inputs.txt"), inputs).expect("OUT_DIR writes");
    println!(
        "cargo::rustc-env=ATERM_SECRETS_RULES={:016x}{:016x}",
        hashes[0].finish(),
        hashes[1].finish()
    );
}

/// Every `.rs` file under `dir` but a test module by its name (`tests.rs`, `*_tests.rs`, or one
/// under a `tests` folder): no build without tests reads one.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("a source folder reads");
    for entry in entries {
        let path = entry.expect("a source folder lists").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if name != "tests" {
                sources(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            out.push(path);
        }
    }
}

/// The files (and the folders of their own modules) of the modules `file` declares only for a
/// test build: a `mod name;` at the start of a line, its attributes each at the start of the lines
/// above it, one of them a `cfg` that holds only under `test` (`cfg(test)`, `cfg(all(…, test, …))`).
/// A declaration in any other shape (indented, in an inline module, behind a `cfg_attr`) leaves
/// its file among the rules: what is left out is only what a build without tests cannot read.
fn test_only_modules(file: &Path, out: &mut BTreeSet<PathBuf>) {
    let text = std::fs::read_to_string(file).expect("a source file reads");
    let dir = file.parent().expect("a source file has a folder");
    let stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let owns_dir = matches!(stem, "lib" | "main" | "mod");
    let mut attributes: Vec<String> = Vec::new();
    let mut open: Option<String> = None;
    for line in text.lines() {
        if let Some(attribute) = open.as_mut() {
            attribute.push(' ');
            attribute.push_str(line.trim());
            if line.trim_end().ends_with(']') {
                attributes.push(open.take().unwrap_or_default());
            }
            continue;
        }
        if line.starts_with("#[") {
            if line.trim_end().ends_with(']') {
                attributes.push(line.trim_end().to_owned());
            } else {
                open = Some(line.trim_end().to_owned());
            }
            continue;
        }
        if line.starts_with("//") {
            continue;
        }
        if let Some(name) = declared_module(line)
            && attributes.iter().any(|attribute| test_only_cfg(attribute))
        {
            let module = attributes
                .iter()
                .find_map(|attribute| path_attribute(attribute))
                .map(|path| vec![dir.join(path)])
                .unwrap_or_else(|| {
                    let base = if owns_dir {
                        dir.to_path_buf()
                    } else {
                        dir.join(stem)
                    };
                    vec![
                        base.join(format!("{name}.rs")),
                        base.join(&name).join("mod.rs"),
                    ]
                });
            for path in module {
                // The module's own modules are in the folder named for it beside its file
                // (`mod.rs` aside, whose folder is its own).
                let children = if path.file_name().is_some_and(|n| n == "mod.rs") {
                    path.parent().map(Path::to_path_buf)
                } else {
                    path.file_stem().map(|stem| path.with_file_name(stem))
                };
                out.insert(path);
                if let Some(children) = children {
                    out.insert(children);
                }
            }
        }
        attributes.clear();
    }
}

/// The name a line declares a module of in its own file: `mod name;`, with any visibility, at the
/// start of the line.
fn declared_module(line: &str) -> Option<String> {
    let mut rest = line.trim_end();
    if let Some(after) = rest.strip_prefix("pub") {
        rest = after;
        if let Some(scoped) = rest.strip_prefix('(') {
            rest = &scoped[scoped.find(')')? + 1..];
        }
        rest = rest.strip_prefix(' ')?;
    }
    let name = rest.strip_prefix("mod ")?.strip_suffix(';')?.trim();
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
        .then(|| name.to_owned())
}

/// The file a `#[path = "…"]` attribute names.
fn path_attribute(attribute: &str) -> Option<&str> {
    let inner = attribute.strip_prefix("#[")?.strip_suffix(']')?.trim();
    let value = inner
        .strip_prefix("path")?
        .trim_start()
        .strip_prefix('=')?
        .trim();
    value.strip_prefix('"')?.strip_suffix('"')
}

/// Whether `attribute` is a `cfg` whose predicate holds only where `test` does.
fn test_only_cfg(attribute: &str) -> bool {
    let Some(inner) = attribute
        .strip_prefix("#[")
        .and_then(|a| a.strip_suffix(']'))
        .map(str::trim)
        .and_then(|a| a.strip_prefix("cfg("))
        .and_then(|a| a.strip_suffix(')'))
    else {
        return false;
    };
    requires_test(inner.trim())
}

/// Whether the `cfg` predicate `predicate` implies `test`: it is `test`, an `all` of which one
/// part implies it, or an `any` of which every part does.
fn requires_test(predicate: &str) -> bool {
    if predicate == "test" {
        return true;
    }
    let (all, parts) = if let Some(parts) = predicate.strip_prefix("all(") {
        (true, parts)
    } else if let Some(parts) = predicate.strip_prefix("any(") {
        (false, parts)
    } else {
        return false;
    };
    let Some(parts) = parts.strip_suffix(')') else {
        return false;
    };
    let parts = split_top(parts);
    if all {
        parts.iter().any(|part| requires_test(part))
    } else {
        !parts.is_empty() && parts.iter().all(|part| requires_test(part))
    }
}

/// `list` split at its commas outside parentheses, each part trimmed, empty parts dropped.
fn split_top(list: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    for (at, c) in list.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(list[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(list[start..].trim());
    parts.retain(|part| !part.is_empty());
    parts
}

/// Every `ring` package `lock` (the workspace's `Cargo.lock`) holds, as `<version> <checksum>`,
/// joined by `;`.
fn locked_ring(lock: &Path) -> String {
    let text = std::fs::read_to_string(lock).expect("the workspace's Cargo.lock reads");
    let mut found = Vec::new();
    for package in text.split("[[package]]") {
        let field = |key: &str| {
            package.lines().find_map(|line| {
                line.strip_prefix(key)?
                    .trim_start()
                    .strip_prefix('=')?
                    .trim()
                    .strip_prefix('"')?
                    .strip_suffix('"')
                    .map(str::to_owned)
            })
        };
        if field("name ").as_deref() == Some("ring") {
            found.push(format!(
                "{} {}",
                field("version ").unwrap_or_default(),
                field("checksum ").unwrap_or_default()
            ));
        }
    }
    assert!(!found.is_empty(), "Cargo.lock locks ring");
    found.join(";")
}
