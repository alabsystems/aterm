// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The in-process `aterm machines storage` command; no repository discovery,
//! credential reads, network clients, subprocesses, or writes. Filesystem calls
//! can still trigger OS-managed remote or materialization I/O.

use crate::{Limits, Plan};
use std::ffi::OsString;
use std::io::{self, Write as _};
use std::path::PathBuf;

pub const USAGE: &str = "aterm machines storage plan --git-common-dir ABS [--json] [--verify-io-bytes N]\nRead-only: byte equality does not authorize deletion or predict physical savings.";

fn parse(args: &[OsString]) -> Result<(PathBuf, Limits, bool), String> {
    if args.first().and_then(|a| a.to_str()) != Some("plan") {
        return Err("expected plan".into());
    }
    let mut root = None;
    let mut verify = None;
    let mut json = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].to_str() {
            Some("--git-common-dir") if root.is_none() => {
                index += 1;
                root = Some(PathBuf::from(
                    args.get(index).ok_or("missing common directory")?,
                ));
            }
            Some("--verify-io-bytes") if verify.is_none() => {
                index += 1;
                let raw = args
                    .get(index)
                    .and_then(|a| a.to_str())
                    .ok_or("missing byte budget")?;
                if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("verification budget must be an unsigned decimal integer".into());
                }
                verify = Some(
                    raw.parse::<u64>()
                        .map_err(|_| "verification budget overflow")?,
                );
            }
            Some("--json") if !json => json = true,
            _ => return Err("unknown or repeated storage option".into()),
        }
        index += 1;
    }
    let limits = Limits {
        verify_io_bytes: verify.unwrap_or(0),
        ..Limits::default()
    };
    Ok((root.ok_or("--git-common-dir is required")?, limits, json))
}

fn render(plan: &Plan, json: bool) -> Result<String, String> {
    if json {
        return aterm_json::to_string(plan).map_err(|e| e.to_string());
    }
    let mut text = format!(
        "repository storage: {}\nobservation complete: {}\n{} unique pack inodes; {} hardlink aliases\n{} candidate duplicate logical bytes; {} byte-equal logical bytes\n{} verification bytes read; physical savings: unknown\nmutation unavailable: {}\n",
        plan.git_common_dir,
        plan.observation_complete,
        plan.unique_pack_inodes,
        plan.hardlink_alias_paths,
        plan.candidate_duplicate_logical_bytes,
        plan.byte_equal_duplicate_logical_bytes,
        plan.verification_io_bytes,
        plan.mutation_reason,
    );
    for pair in &plan.pairs {
        text.push_str(&format!(
            "{:?} {} -> {} ({} bytes): {}\n",
            pair.state, pair.source, pair.target, pair.bytes, pair.reason
        ));
    }
    for issue in &plan.issues {
        text.push_str(&format!("incomplete: {issue}\n"));
    }
    text.push_str(plan.observation_scope);
    Ok(text)
}

/// Run the planner using the arguments after `storage`. Exit 0 is a complete
/// read-only observation; 1 is an incomplete plan or operational refusal; 2 is
/// invalid syntax. None of these statuses grants mutation authority.
pub fn main_entry(args: &[OsString]) -> i32 {
    if args.len() == 1 && matches!(args[0].to_str(), Some("help" | "-h" | "--help")) {
        println!("{USAGE}");
        return 0;
    }
    let (root, limits, json) = match parse(args) {
        Ok(parsed) => parsed,
        Err(reason) => {
            eprintln!("storage: {reason}\n{USAGE}");
            return 2;
        }
    };
    match crate::plan(&root, &limits) {
        Ok(plan) => match render(&plan, json) {
            Ok(text) => {
                if writeln!(io::stdout().lock(), "{text}").is_err() {
                    return 1;
                }
                i32::from(!plan.observation_complete)
            }
            Err(reason) => {
                eprintln!("storage: {reason}");
                1
            }
        },
        Err(reason) => {
            eprintln!("storage: {reason}");
            1
        }
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
