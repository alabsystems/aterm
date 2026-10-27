// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Regenerate rerouted Brew help from an independent offline capture.

mod legacy;

use crate::Outcome;
use aterm_census::seal;
use aterm_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The checked-in input, independent of the generated Rust output.
pub const INPUT: &str = "crates/aterm-forge/data/abrew-help.json";
/// The only generated source this command writes.
pub const OUTPUT: &str = "crates/atpkg/src/abrew/help_texts.rs";
const LEGACY_COMMIT: &str = "f15a698638e026b4ecc927b555dcdf985632f072";
const LEGACY_SHA256: &str = "23f6acdbdaf6d2557b6db2b479d32f289f57bed1a0adae3faf0f9a30c654db9a";
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// A normal check/write, or the explicit one-time migration of the sealed f15 capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Compare without changing the generated source.
    Check,
    /// Publish after validation, formatting and sealing succeed.
    Write,
    /// Write an independent snapshot; refuse any existing destination.
    ImportLegacy { output: PathBuf },
}

/// Typed arguments for the help generator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub input: PathBuf,
    pub mode: Mode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Tables {
    top: String,
    texts: BTreeMap<String, String>,
    aliases: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Capture {
    reference: String,
    columns: u64,
    provenance: String,
    source_sha256: String,
    source_commit: Option<String>,
    tables: Tables,
}

fn object_fields(value: &Value, allowed: &[&str]) -> Result<(), String> {
    let object = value.as_object().ok_or("expected a capture object")?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("unknown capture field {key:?}"));
    }
    Ok(())
}

fn string(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing string field {key:?}"))
}

fn records<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], String> {
    let records = value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing record array {key:?}"))?;
    if records.len() > 4096 {
        return Err(format!("too many {key} records"));
    }
    Ok(records)
}

fn foreign_user_path(text: &str) -> bool {
    if text.contains("/Users/") || text.to_ascii_lowercase().contains("\\users\\") {
        return true;
    }
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("/home/") {
        // `./home/` is a relative example in the captured help, not a host home.
        if let Some(prefix) = before.strip_suffix('.')
            && prefix
                .chars()
                .last()
                .is_none_or(|ch| ch.is_whitespace() || "(['\"`".contains(ch))
        {
            rest = after;
            continue;
        }
        let path = after
            .split([' ', '\n', '\r', '\t', '\'', '"', '`', ')', ','])
            .next()
            .unwrap_or("");
        if path.split('/').next() != Some("linuxbrew") || path.split('/').any(|part| part == "..") {
            return true;
        }
        rest = after;
    }
    false
}

fn command_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

impl Capture {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: Value = aterm_json::from_slice(bytes).map_err(|e| e.to_string())?;
        object_fields(
            &value,
            &[
                "schema",
                "reference",
                "columns",
                "mode",
                "provenance",
                "source_sha256",
                "source_commit",
                "top",
                "canonical",
                "aliases",
            ],
        )?;
        if value.get("schema").and_then(Value::as_u64) != Some(1)
            || value.get("mode").and_then(Value::as_str) != Some("piped")
        {
            return Err("expected schema 1 and piped capture mode".into());
        }
        let mut tables = Tables {
            top: string(&value, "top")?,
            texts: BTreeMap::new(),
            aliases: BTreeMap::new(),
        };
        for row in records(&value, "canonical")? {
            object_fields(row, &["name", "stdout"])?;
            let name = string(row, "name")?;
            if tables
                .texts
                .insert(name.clone(), string(row, "stdout")?)
                .is_some()
            {
                return Err(format!("duplicate canonical name {name:?}"));
            }
        }
        for row in records(&value, "aliases")? {
            object_fields(row, &["name", "target", "stdout"])?;
            let name = string(row, "name")?;
            let target = string(row, "target")?;
            if let Some(stdout) = row.get("stdout")
                && stdout.as_str() != tables.texts.get(&target).map(String::as_str)
            {
                return Err(format!(
                    "alias {name:?} stdout differs from its explicit target"
                ));
            }
            if tables.aliases.insert(name.clone(), target).is_some() {
                return Err(format!("duplicate alias {name:?}"));
            }
        }
        let capture = Self {
            reference: string(&value, "reference")?,
            columns: value
                .get("columns")
                .and_then(Value::as_u64)
                .ok_or("missing capture columns")?,
            provenance: string(&value, "provenance")?,
            source_sha256: string(&value, "source_sha256")?,
            source_commit: match value.get("source_commit") {
                None | Some(Value::Null) => None,
                Some(Value::String(commit)) => Some(commit.clone()),
                Some(_) => return Err("source_commit must be a commit string or null".into()),
            },
            tables,
        };
        capture.validate()?;
        Ok(capture)
    }

    fn validate(&self) -> Result<(), String> {
        if !self.reference.starts_with("Homebrew ")
            || self.reference == "Homebrew "
            || self.reference.chars().any(char::is_control)
            || self.provenance.is_empty()
            || self.provenance.chars().any(char::is_control)
            || !(1..=1000).contains(&self.columns)
            || self.source_sha256.len() != 64
            || !self
                .source_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid reference capture metadata".into());
        }
        if self.source_commit.as_ref().is_some_and(|commit| {
            commit.len() != 40
                || !commit
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err("invalid source capture commit".into());
        }
        if self.tables.top.is_empty() || self.tables.texts.is_empty() {
            return Err("capture must contain TOP and canonical help".into());
        }
        for (name, text) in &self.tables.texts {
            if !command_name(name) || text.is_empty() {
                return Err(format!("invalid canonical help {name:?}"));
            }
        }
        for (alias, target) in &self.tables.aliases {
            if !command_name(alias)
                || self.tables.texts.contains_key(alias)
                || !self.tables.texts.contains_key(target)
            {
                return Err(format!(
                    "alias {alias:?} collides or has no canonical target"
                ));
            }
        }
        for text in std::iter::once(&self.tables.top)
            .chain(self.tables.texts.values())
            .chain([&self.reference, &self.provenance])
        {
            if foreign_user_path(text) {
                return Err("capture embeds a foreign user home path".into());
            }
        }
        Ok(())
    }

    fn json(&self) -> Result<Vec<u8>, String> {
        let canonical: Vec<Value> = self
            .tables
            .texts
            .iter()
            .map(|(name, stdout)| json!({"name": name, "stdout": stdout}))
            .collect();
        let aliases: Vec<Value> = self
            .tables
            .aliases
            .iter()
            .map(|(name, target)| json!({"name": name, "target": target}))
            .collect();
        let value = json!({"schema": 1, "reference": self.reference, "mode": "piped", "columns": self.columns,
            "provenance": self.provenance, "source_sha256": self.source_sha256, "source_commit": self.source_commit, "top": self.tables.top,
            "canonical": canonical, "aliases": aliases});
        let mut encoded = aterm_json::to_vec(&value).map_err(|e| e.to_string())?;
        encoded.push(b'\n');
        Ok(encoded)
    }
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(format!(
            "{} is not a bounded regular capture file",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("capture grew beyond the size limit".into());
    }
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    aterm_digest::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn literal(text: &str) -> String {
    if text
        .chars()
        .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    {
        return format!("{text:?}");
    }
    for hashes in 1..=255 {
        let fence = "#".repeat(hashes);
        if !text.contains(&format!("\"{fence}")) {
            return format!("r{fence}\"{text}\"{fence}");
        }
    }
    format!("{text:?}")
}

fn render(capture: &Capture, generator: &str, input_hash: &str) -> String {
    let mut out = format!(
        "// Copyright 2026 Andrew Yates\n// SPDX-License-Identifier: Apache-2.0\n{}\n//\n// Recorded {} help, piped at {} columns.\n// Input SHA-256: {input_hash}\n\npub(super) const TOP: &str = {};\n\npub(super) const TEXTS: &[(&str, &str)] = &[\n",
        seal::line("//", generator),
        capture.reference,
        capture.columns,
        literal(&capture.tables.top)
    );
    for (name, text) in &capture.tables.texts {
        out.push_str(&format!("    ({name:?}, {}),\n", literal(text)));
    }
    out.push_str("];\n\npub(super) const ALIASES: &[(&str, &str)] = &[\n");
    for (alias, target) in &capture.tables.aliases {
        out.push_str(&format!("    ({alias:?}, {target:?}),\n"));
    }
    out.push_str("];\n");
    out
}

fn format(root: &Path, path: &Path) -> Result<(), String> {
    let mut command = aterm_uds::spawnfd::command("trustfmt");
    command.args(["--edition", "2024"]).current_dir(root);
    for name in ["rustfmt.toml", ".rustfmt.toml"] {
        let config = root.join(name);
        if config.is_file() {
            command.arg("--config-path").arg(config);
            break;
        }
    }
    let result = command
        .arg(path)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(format!(
            "Trust formatter failed: {}",
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    Ok(())
}

/// Check or regenerate from the captured input. The only child is Trust's formatter;
/// no reference program, package manager or network is invoked.
/// # Errors
/// Invalid input, an unsupported legacy artifact, formatter failure or filesystem I/O.
pub fn run(root: &Path, options: &Options) -> Result<Outcome, String> {
    run_with(root, options, format)
}

fn destination(root: &Path) -> Result<PathBuf, String> {
    let logical = root.join(OUTPUT);
    let parent = logical.parent().ok_or("generated output has no parent")?;
    let mut ancestor = parent;
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let physical = ancestor.canonicalize().map_err(|e| e.to_string())?;
                if !physical.starts_with(root) {
                    return Err("generated output parent escapes the workspace".into());
                }
                let remaining = parent.strip_prefix(ancestor).map_err(|e| e.to_string())?;
                return Ok(physical.join(remaining).join("help_texts.rs"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .ok_or("generated output has no existing parent")?;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn run_with(
    root: &Path,
    options: &Options,
    formatter: impl FnOnce(&Path, &Path) -> Result<(), String>,
) -> Result<Outcome, String> {
    let physical = root.canonicalize().map_err(|e| e.to_string())?;
    let root = physical.as_path();
    let input = root.join(&options.input);
    if let Mode::ImportLegacy { output } = &options.mode {
        let bytes = read(&input)?;
        if digest(&bytes) != LEGACY_SHA256 {
            return Err("legacy import accepts only the sealed f15 capture artifact".into());
        }
        let source = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        if !matches!(seal::check(source), seal::Seal::Holds { .. })
            || !source
                .lines()
                .take(16)
                .any(|line| line.starts_with("// @generated by tools/gen-abrew-help-texts.py "))
            || !source
                .lines()
                .take(16)
                .any(|line| line.contains("`Homebrew 7.0.10`"))
        {
            return Err("legacy capture seal or generator identity does not match".into());
        }
        let capture = Capture {
            reference: "Homebrew 7.0.10".into(),
            columns: 80,
            provenance: format!(
                "Migrated from the already-sealed capture at aterm commit {LEGACY_COMMIT}; no new reference measurement"
            ),
            source_sha256: LEGACY_SHA256.into(),
            source_commit: Some(LEGACY_COMMIT.into()),
            tables: legacy::tables(source)?,
        };
        capture.validate()?;
        let encoded = capture.json()?;
        if Capture::parse(&encoded)? != capture {
            return Err("snapshot encoding changed legacy capture values".into());
        }
        let path = root.join(output);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        file.write_all(&encoded).map_err(|e| e.to_string())?;
        return Ok(Outcome {
            ok: true,
            log: format!("imported sealed capture into {}\n", path.display()),
        });
    }
    let absolute = input.canonicalize().map_err(|e| e.to_string())?;
    let relative = absolute
        .strip_prefix(root)
        .map_err(|_| "place the offline snapshot inside the workspace before generating")?;
    let relative = relative.to_str().ok_or("snapshot path is not UTF-8")?;
    if !relative
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/\\-_.".contains(&byte))
    {
        return Err("snapshot path must use portable ASCII path components".into());
    }
    let destination = destination(root)?;
    if absolute == destination {
        return Err("the offline input must differ from the generated output".into());
    }
    let bytes = read(&input)?;
    let capture = Capture::parse(&bytes)?;
    let quoted = format!("'{}'", relative.replace('\'', "'\"'\"'"));
    let generator = format!("`targo --unverified forge abrew-help --input {quoted} --write`");
    let scratch = if options.mode == Mode::Write {
        let parent = destination
            .parent()
            .ok_or("generated output has no parent")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        aterm_tempfile::Builder::new().tempdir_in(parent)
    } else {
        aterm_tempfile::tempdir()
    }
    .map_err(|e| e.to_string())?;
    let candidate = scratch.path().join("help_texts.rs");
    std::fs::write(&candidate, render(&capture, &generator, &digest(&bytes)))
        .map_err(|e| e.to_string())?;
    formatter(root, &candidate)?;
    let formatted = std::fs::read_to_string(&candidate).map_err(|e| e.to_string())?;
    if legacy::tables(&formatted)? != capture.tables {
        return Err("formatting changed captured help values".into());
    }
    let stamped = seal::stamp(&formatted)?;
    let same = match std::fs::symlink_metadata(&destination) {
        Ok(_) => read(&destination)? == stamped.as_bytes(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.to_string()),
    };
    if options.mode == Mode::Write && !same {
        std::fs::write(&candidate, &stamped).map_err(|e| e.to_string())?;
        std::fs::rename(&candidate, &destination).map_err(|e| e.to_string())?;
    }
    Ok(Outcome {
        ok: same || options.mode == Mode::Write,
        log: format!(
            "abrew help: {} ({} canonical texts, {} aliases)\n",
            if same {
                "current"
            } else if options.mode == Mode::Write {
                "regenerated"
            } else {
                "differs; rerun with --write"
            },
            capture.tables.texts.len(),
            capture.tables.aliases.len()
        ),
    })
}

#[cfg(test)]
mod tests;
