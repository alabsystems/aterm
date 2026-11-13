// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg lane` — the Rust-lane reader over one shell command line.

use std::path::{Path, PathBuf};

use aterm_types::rust_lane;

use crate::reroute::Pin;

/// The most input one call reads.
pub const MAX_INPUT_BYTES: u64 = 8 * 1024 * 1024;

/// All of `input`, up to [`MAX_INPUT_BYTES`].
/// # Errors
/// The reader failed, or held more than [`MAX_INPUT_BYTES`].
pub fn read_input(input: impl std::io::Read) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let mut buf = Vec::new();
    input
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("could not read stdin: {e}"))?;
    if buf.len() as u64 > MAX_INPUT_BYTES {
        return Err(format!(
            "stdin is over {} MiB (the limit)",
            MAX_INPUT_BYTES / (1024 * 1024)
        ));
    }
    Ok(buf)
}

/// Where the command line comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The words after `--`, joined by single spaces.
    Words(Vec<String>),
    /// All of stdin (`-`).
    Stdin,
    /// The string at this dotted path of the JSON object on stdin (`--json`).
    Json(String),
}

/// A parsed `atpkg lane` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// The directory the line runs in (`--cwd`; the process's otherwise): it resolves a relative command word's directory and names the stock pin in a refusal.
    pub cwd: Option<PathBuf>,
    /// Where the command line comes from.
    pub source: Source,
}

/// Parse the argv after `lane`.
/// # Errors
/// An unknown option, a missing value, no source, or two sources.
pub fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut cwd = None;
    let mut source: Option<Source> = None;
    let set = |source: &mut Option<Source>, next: Source| -> Result<(), String> {
        if source.is_some() {
            return Err(String::from(
                "name the command line once: `-- <command…>`, `-`, or `--json <field>`",
            ));
        }
        *source = Some(next);
        Ok(())
    };
    let mut i = 0;
    while let Some(arg) = rest.get(i) {
        match arg.as_str() {
            "--" => {
                let words = rest[i + 1..].to_vec();
                if words.is_empty() {
                    return Err(String::from("`--` names no command"));
                }
                set(&mut source, Source::Words(words))?;
                break;
            }
            "-" => set(&mut source, Source::Stdin)?,
            "--cwd" | "--json" => {
                let Some(value) = rest.get(i + 1).filter(|v| !v.is_empty()) else {
                    return Err(format!("{arg} takes a value"));
                };
                if arg == "--cwd" {
                    cwd = Some(PathBuf::from(value));
                } else {
                    set(&mut source, Source::Json(value.clone()))?;
                }
                i += 1;
            }
            other => {
                if let Some(value) = other.strip_prefix("--cwd=").filter(|v| !v.is_empty()) {
                    cwd = Some(PathBuf::from(value));
                } else if let Some(value) = other.strip_prefix("--json=").filter(|v| !v.is_empty())
                {
                    set(&mut source, Source::Json(value.to_string()))?;
                } else {
                    return Err(format!("unknown argument {other:?}"));
                }
            }
        }
        i += 1;
    }
    let Some(source) = source else {
        return Err(String::from(
            "no command line: `-- <command…>`, `-` (stdin), or `--json <field>` (stdin)",
        ));
    };
    Ok(Args { cwd, source })
}

/// The command line `source` names, `input` being stdin's bytes (read only for the stdin sources).
/// # Errors
/// Input that is not UTF-8, or not JSON for [`Source::Json`].
pub fn command_line(source: &Source, input: &[u8]) -> Result<Option<String>, String> {
    match source {
        Source::Words(words) => Ok(Some(words.join(" "))),
        Source::Stdin => std::str::from_utf8(input)
            .map(|s| Some(s.to_string()))
            .map_err(|e| format!("stdin is not UTF-8: {e}")),
        Source::Json(path) => {
            let text =
                std::str::from_utf8(input).map_err(|e| format!("stdin is not UTF-8: {e}"))?;
            let doc: aterm_json::Value =
                aterm_json::from_str(text).map_err(|e| format!("stdin is not JSON: {e}"))?;
            let mut at = &doc;
            for key in path.split('.') {
                match at.get(key) {
                    Some(next) => at = next,
                    None => return Ok(None),
                }
            }
            Ok(at.as_str().map(str::to_string))
        }
    }
}

/// What the reader says of one command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// No stock Rust tool at command position.
    Clean,
    /// One, cleared by its `ATERM_STOCK_REASON`: the line to print.
    Escaped(String),
    /// One: the refusal to print.
    Refused(String),
}

/// The reader's answer for `command`, a refusal naming the stock pin of `cwd`.
#[must_use]
pub fn answer(
    command: &str,
    cwd: Option<&Path>,
    home: Option<&Path>,
    env_toolchain: Option<&str>,
) -> Answer {
    let beside = |dir: &str, name: &str| rust_lane::file_beside(dir, name, cwd, home);
    match rust_lane::verdict_in(command, &beside) {
        rust_lane::Verdict::Clean => Answer::Clean,
        rust_lane::Verdict::Escaped { hit, reason } => Answer::Escaped(format!(
            "aterm: {} `{}` let through — {}={reason:?}; say why in your reply.",
            if hit.compat { "compatibility" } else { "stock" },
            hit.used,
            rust_lane::STOCK_REASON
        )),
        rust_lane::Verdict::Stock(hit) => {
            let pin = Pin::for_dir(cwd, env_toolchain);
            let pin = pin.as_ref().map(|p| (&p.source, p.channel.as_str()));
            Answer::Refused(rust_lane::refusal(&hit, pin))
        }
    }
}

#[cfg(test)]
#[path = "lane_tests.rs"]
mod tests;
