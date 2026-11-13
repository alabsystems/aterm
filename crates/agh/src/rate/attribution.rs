// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process ancestry without argv, environment, shell text or paths in the record.

use crate::redact::Redactor;
use aterm_json::{Value, json};

/// Allowlisted command words distinguish aterm verbs and engine/cutter invocations.
/// All other arguments (including a shell's -c text) are discarded before logging.
fn role(program: &str, argv: &[std::ffi::OsString]) -> String {
    let mut role = program.to_owned();
    if matches!(program, "bash" | "sh" | "zsh" | "node" | "python3") {
        for arg in argv.iter().skip(1).take(3).filter_map(|a| a.to_str()) {
            // Shell command text is never interpreted, even when it looks like
            // an engine path. Only an actual script argument has a role.
            if arg.starts_with('-') {
                break;
            }
            let script = std::path::Path::new(arg)
                .file_name()
                .and_then(|s| s.to_str());
            let known = match script {
                Some("engine.sh" | "publish.sh" | "atpkg-publish") => Some("publication-engine"),
                Some("cut.sh" | "aterm-release") => Some("release-cutter"),
                Some("cli.js") if arg.contains("/claude-code/") => Some("claude-worker"),
                Some("codex.js") => Some("codex-worker"),
                _ => None,
            };
            if let Some(known) = known {
                role.push(' ');
                role.push_str(known);
                break;
            }
        }
    }
    if matches!(
        program,
        "aterm" | "agh" | "targo" | "aterm-release" | "atpkg-publish" | "aterm-verify"
    ) {
        for arg in argv.iter().skip(1).take(4) {
            let Some(arg) = arg.to_str() else { break };
            if !matches!(
                arg,
                "--unverified"
                    | "verify"
                    | "land"
                    | "main"
                    | "ship"
                    | "release"
                    | "cut"
                    | "lane"
                    | "run"
                    | "status"
                    | "check"
                    | "agh"
                    | "api"
                    | "repo"
                    | "fetch"
                    | "fetch-main"
                    | "refs"
                    | "rate"
                    | "latest"
                    | "newest"
                    | "view"
                    | "list"
                    | "ensure"
                    | "assets"
                    | "download"
                    | "publish"
                    | "stage"
                    | "promote"
                    | "harness"
                    | "worker"
                    | "engine"
                    | "test"
                    | "build"
            ) {
                break;
            }
            role.push(' ');
            role.push_str(arg);
        }
    }
    role
}

/// At most 16 kernel-checked ancestors. Failure is explicit, never an invented caller.
pub(crate) fn parent_chain(redactor: &Redactor) -> Value {
    use aterm_uds::process::{
        read_process_argv, read_process_parent, read_process_path, read_process_start,
    };
    let mut pid = std::process::id();
    let mut rows = Vec::new();
    for _ in 0..16 {
        let Ok(native) = i32::try_from(pid) else {
            break;
        };
        let start = read_process_start(native);
        let parent = read_process_parent(native);
        let program = read_process_path(native)
            .and_then(|path| path.file_name().map(|s| s.to_string_lossy().into_owned()));
        let argv = read_process_argv(native);
        if start.is_none() || read_process_start(native) != start || program.is_none() {
            rows.push(json!({"pid":pid,"role":"unread","complete":false}));
            return Value::Array(rows);
        }
        let program = program.unwrap_or_default();
        rows.push(
            json!({"pid":pid,"start":start.map(|s| s.boot.0),"program":redactor.apply(&program),
            "role":redactor.apply(&role(&program, &argv.unwrap_or_default()))}),
        );
        let Some(parent) = parent.filter(|parent| *parent > 1 && *parent != pid) else {
            return Value::Array(rows);
        };
        if rows
            .iter()
            .any(|row| row["pid"].as_u64() == Some(u64::from(parent)))
        {
            break;
        }
        pid = parent;
    }
    rows.push(json!({"role":"truncated","complete":false}));
    Value::Array(rows)
}

#[cfg(test)]
#[path = "attribution_tests.rs"]
mod tests;
