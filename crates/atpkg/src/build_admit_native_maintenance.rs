// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native maintenance is not a compiler producer. Old/plugin dispatch remains
//! conservative unless the SAME signed selected executable owns native fixed v1.

use crate::{
    build_cache::Tool,
    prerequisite::{self, Prerequisite},
};
use std::{
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};

fn command(args: &[String], tool: Tool, placed: &str) -> bool {
    if tool != Tool::Targo || placed != "-" {
        return false;
    }
    let [lane, resource, fixed, rest @ ..] = args else {
        return false;
    };
    if lane != "--unverified" || resource != "resource" || fixed != "fixed" {
        return false;
    }
    let mut words = rest.iter().map(String::as_str);
    let mut seen = 0u8;
    while let Some(word) = words.next() {
        if word == "--" {
            // Everything remaining is parsed by the native repo-prune builtin;
            // it never dispatches a trailing word as a compiler or plugin.
            return words.next() == Some("repo-prune");
        }
        let (flag, inline) = word
            .split_once('=')
            .map_or((word, None), |(f, v)| (f, Some(v)));
        let bit = match flag {
            "--cpus" => 1,
            "--slots" => 2,
            "--memory-mib" => 4,
            "--wait-seconds" => 8,
            "--exclusive" if inline.is_none() => 16,
            _ => return false,
        };
        if seen & bit != 0 {
            return false;
        }
        seen |= bit;
        if bit != 16 {
            let Some(value) = inline.or_else(|| words.next()) else {
                return false;
            };
            if matches!(bit, 1 | 2) {
                if value.parse::<std::num::NonZeroU32>().is_err() {
                    return false;
                }
            } else if value.parse::<std::num::NonZeroU64>().is_err() {
                return false;
            }
        }
    }
    false
}

fn answer(output: &std::process::Output) -> bool {
    output.status.success()
        && output.stdout == b"{\"version\":1,\"fixed_admission\":\"original-fixed-v1\"}\n"
        && output.stderr.is_empty()
}

// Admission failure is no exception: the caller takes the ordinary cache-owner
// path. Once actual maintenance starts, its status is returned exactly once.
pub(super) fn run(target: &Path, tool: Tool, placed: &str, args: &[String]) -> Option<ExitCode> {
    if !command(args, tool, placed) {
        return None;
    }
    // Do not change native ownership locators or other inherited inputs. Ambient
    // loader replacement cannot satisfy the held program/runtime profile.
    if std::env::vars_os().any(|(name, _)| name.to_string_lossy().starts_with("DYLD_")) {
        return None;
    }
    let target = target.canonicalize().ok()?;
    if target.file_name()? != "targo" || target.parent()?.file_name()? != "bin" {
        return None;
    }
    let build = target
        .parent()?
        .parent()?
        .file_name()?
        .to_str()?
        .strip_suffix(".noindex")
        .unwrap_or(target.parent()?.parent()?.file_name()?.to_str()?)
        .parse()
        .ok()?;
    // Authenticate this exact build through the existing signed package owner.
    // No current/default substitution, PATH candidate, version or caller JSON.
    let original =
        prerequisite::configured_tool(Prerequisite::Targo, build, "native repo-prune").ok()?;
    if original.physical_path() != target {
        return None;
    }
    let deadline = Instant::now().checked_add(Duration::from_secs(7 * 24 * 60 * 60))?;
    let native = original.into_native(deadline).ok()?;
    let mut probe = native.original().ok()?.command().ok()?;
    probe
        .env_clear()
        .args(["--resource-capabilities", "--fixed-admission-v1"]);
    for (name, value) in native.program().ok()?.runtime_environment().ok()? {
        probe.env(name, value);
    }
    let output = aterm_uds::deadline::output_within(&mut probe, Duration::from_secs(10)).ok()?;
    native.check_current().ok()?;
    if !answer(&output) {
        return None;
    }
    // The native v1 fixed dispatcher owns a closed inner-command whitelist.
    // A version with fixed v1 but no repo-prune refuses it before any execution.
    let mut child = aterm_uds::spawnfd::transparent(&target).into_command();
    child.args(args);
    native.check_current().ok()?;
    let status = super::supervise(None, &mut child, &mut |line| eprintln!("{line}"));
    if let Err(error) = native.check_current() {
        eprintln!("atpkg: native maintenance input changed: {error}");
        return Some(ExitCode::from(75));
    }
    Some(match status {
        Ok(status) => super::end_like(status),
        Err(error) => {
            eprintln!("atpkg: native maintenance failed: {error}");
            ExitCode::from(127)
        }
    })
}

#[cfg(test)]
#[path = "build_admit_native_maintenance_tests.rs"]
mod tests;
