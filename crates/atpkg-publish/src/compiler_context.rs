// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compiler-moving Cargo inputs at the actual native hop, without selecting an SDK again.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

const MAX_CONFIG_BYTES: u64 = 1 << 20;
const SETTINGS: [(&str, [&str; 2]); 4] = [
    ("rustc", ["RUSTC", "CARGO_BUILD_RUSTC"]),
    ("rustdoc", ["RUSTDOC", "CARGO_BUILD_RUSTDOC"]),
    (
        "rustc-wrapper",
        ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"],
    ),
    (
        "rustc-workspace-wrapper",
        [
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        ],
    ),
];

/// The publisher's explicit-target front preserves its selected compiler in every cwd.
/// Capabilities remain an interpreter- and configuration-independent query.
pub(crate) fn admit_hop(args: &[OsString]) -> Result<()> {
    if let [only] = args
        && (only == "--capabilities" || only == atpkg::trust_lock::LOCK_CAPABILITY_FLAG)
    {
        return Ok(());
    }
    for key in SETTINGS
        .iter()
        .flat_map(|(_, keys)| keys.iter().copied())
        .chain(["TRUST_STAGE2_BIN"])
    {
        if std::env::var_os(key).is_some_and(|value| !value.is_empty()) {
            return Err(Error::refused(format!(
                "{key} overrides the selected compiler at its native hop"
            )));
        }
    }
    let cwd = std::env::current_dir().map_err(|e| Error::io("read compiler cwd", &e))?;
    let cargo_home = std::env::var_os("CARGO_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".cargo"))
        })
        .ok_or_else(|| Error::refused("cannot discover the native hop's Cargo home"))?;
    let mut seen = BTreeSet::new();
    for dir in cwd
        .ancestors()
        .map(|dir| dir.join(".cargo"))
        .chain([cwd.join(cargo_home)])
    {
        for name in ["config", "config.toml"] {
            let path = dir.join(name);
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {
                    if seen.insert(path.clone()) {
                        admit_file(&path)?;
                    }
                    break; // Cargo prefers config when both spellings exist.
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(Error::io("inspect compiler Cargo config", &error)),
            }
        }
    }
    let target = if args.first().is_some_and(|word| word == "--shim") {
        args.get(3..).unwrap_or_default()
    } else {
        args
    };
    if let Some((program, words)) = target.split_first()
        && Path::new(program).file_stem().is_some_and(|name| {
            matches!(
                name.to_str(),
                Some("targo" | "cargo" | "targo-tippy" | "cargo-clippy")
            )
        })
    {
        admit_arguments(&cwd, words)?;
    }
    Ok(())
}

fn admit_arguments(cwd: &Path, args: &[OsString]) -> Result<()> {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            break; // The invoked program's arguments are not Cargo configuration.
        }
        if word.as_encoded_bytes().starts_with(b"-C")
            || word == "--directory"
            || word.as_encoded_bytes().starts_with(b"--directory=")
        {
            return Err(Error::refused(
                "Cargo directory overrides cannot change the native hop's admitted cwd",
            ));
        }
        let value = if word == "--config" {
            Some(
                words
                    .next()
                    .ok_or_else(|| Error::usage("--config needs a value"))?
                    .as_os_str(),
            )
        } else if let Some(text) = word
            .to_str()
            .and_then(|word| word.strip_prefix("--config="))
        {
            Some(OsStr::new(text))
        } else if word.as_encoded_bytes().starts_with(b"--config=") {
            return Err(Error::refused("Cargo --config input is not UTF-8"));
        } else {
            None
        };
        if let Some(value) = value {
            let text = value
                .to_str()
                .ok_or_else(|| Error::refused("Cargo --config input is not UTF-8"))?;
            if text.contains('=') {
                admit_document(text, "Cargo --config")?;
            } else {
                admit_file(&cwd.join(value))?;
            }
        }
    }
    Ok(())
}

fn admit_file(path: &Path) -> Result<()> {
    let text = crate::identity::read_regular_text(path, MAX_CONFIG_BYTES).map_err(|e| {
        Error::refused(format!("cannot admit Cargo config {}: {e}", path.display()))
    })?;
    admit_document(&text, &path.display().to_string())
}

fn admit_document(text: &str, source: &str) -> Result<()> {
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_CONFIG_BYTES {
        return Err(Error::refused(
            "Cargo config exceeds the native hop's 1 MiB bound",
        ));
    }
    let doc: aterm_toml::Value = aterm_toml::from_str(text)
        .map_err(|_| Error::refused(format!("cannot parse Cargo config {source}")))?;
    if doc.get("include").is_some() {
        return Err(Error::refused(format!(
            "Cargo config includes cannot be admitted at the compiler hop: {source}"
        )));
    }
    if let Some(build) = doc.get("build") {
        let build = build
            .as_table()
            .ok_or_else(|| Error::refused("Cargo build configuration is not a table"))?;
        for (key, _) in SETTINGS {
            if build.get(key).is_some() {
                return Err(Error::refused(format!(
                    "build.{key} in {source} overrides the selected compiler at its native hop"
                )));
            }
        }
    }
    Ok(())
}
