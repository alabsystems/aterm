// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The commands the tests run through the agent: its own debug executable in a stand-in
//! mode (`aterm-vz-agent stand-in <mode> …`), never an Apple binary (AGENTS.md). Absent
//! from a release build, so the guest's agent has no such mode.

use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::process::ExitCode;

/// Wait to be signalled while process `test` (the test that started this stand-in) lives, so
/// a test that fails before its kill strands no stand-in. It watches that pid, not its parent,
/// so a group kill that misses a grandchild leaves it running for the test to see.
fn park(test: &str) -> ExitCode {
    let Ok(test) = test.parse::<u32>() else {
        return ExitCode::from(2);
    };
    while crate::sys::alive(test) {
        std::thread::park_timeout(std::time::Duration::from_millis(250));
    }
    ExitCode::from(4)
}

/// Run stand-in `args` (its mode first).
#[must_use]
pub fn run(args: &[OsString]) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let arg = |i: usize| args.get(i).map_or("", String::as_str);
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    match arg(0) {
        // Exit with this code.
        "exit" => ExitCode::from(arg(1).parse::<u8>().unwrap_or(2)),
        // Copy stdin to stdout until its EOF.
        "echo" => {
            let mut all = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut all);
            let _ = out.write_all(&all);
            ExitCode::SUCCESS
        }
        // `out i` on stdout and `err i` on stderr, alternately, each flushed.
        "interleave" => {
            for i in 0..arg(1).parse::<u32>().unwrap_or(0) {
                let _ = writeln!(out, "out {i}");
                let _ = out.flush();
                let _ = writeln!(err, "err {i}");
                let _ = err.flush();
            }
            ExitCode::SUCCESS
        }
        // Its whole environment, sorted, then its working directory.
        "env" => {
            let mut vars: Vec<String> = std::env::vars_os()
                .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
                .collect();
            vars.sort();
            for v in vars {
                let _ = writeln!(out, "{v}");
            }
            let cwd = std::env::current_dir().map(|d| d.display().to_string());
            let _ = writeln!(out, "cwd={}", cwd.unwrap_or_default());
            ExitCode::SUCCESS
        }
        // Die by SIGKILL.
        "kill-self" => crate::sys::kill_self(),
        // `hang TEST`: say its pid, then wait to be signalled while the test lives.
        "hang" => {
            let _ = writeln!(out, "pid {}", std::process::id());
            let _ = out.flush();
            park(arg(1))
        }
        // `group TEST`: start a second stand-in that hangs in its process group, say both
        // pids, and wait as `hang` does.
        "group" => {
            let Ok(exe) = std::env::current_exe() else {
                return ExitCode::from(3);
            };
            let grandchild = crate::serve::command(exe.as_os_str())
                .args(["stand-in", "hang", arg(1)])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            let Ok(grandchild) = grandchild else {
                return ExitCode::from(3);
            };
            let _ = writeln!(out, "pids {} {}", std::process::id(), grandchild.id());
            let _ = out.flush();
            park(arg(1))
        }
        // The kernel's view of its credentials (Linux): `Uid:`, `Gid:` and `Groups:`.
        "ids" => {
            let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            for line in status.lines() {
                if ["Uid:", "Gid:", "Groups:"]
                    .iter()
                    .any(|k| line.starts_with(k))
                {
                    let _ = writeln!(out, "{line}");
                }
            }
            ExitCode::SUCCESS
        }
        other => {
            let _ = writeln!(err, "aterm-vz-agent stand-in: no mode `{other}`");
            ExitCode::from(2)
        }
    }
}
