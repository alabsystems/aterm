// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! aterm's build-graph tasks. Three subcommands:
//!
//!   * `gate <verb>`: the checks the merge gate shells into this binary for
//!     (`gate.rs`).
//!   * `perf [--record]`: the perf measurements and the same-box trend ledger
//!     (`perf.rs`). It has no automatic caller; it measures.
//!   * `verify [args…]`: `tools/verify.sh`, the `targo --unverified verify` alias's target.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod driver;
mod gate;
mod perf;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str);
    match cmd {
        Some("gate") => gate::run(
            args.get(2).map(String::as_str),
            args.get(3..).unwrap_or_default(),
        ),
        Some("perf") => {
            if perf::run() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("verify") => verify(&args[2..]),
        _ => {
            eprintln!(
                "usage: xtask <gate <verb>|perf [--record]|verify [args…]>\n\
                 \n\
                 gate <verb>       the merge gate's xtask checks: {}\n\
                 perf [--record]   measure against tools/golden (--record rewrites the baselines)\n\
                 verify [args…]    run tools/verify.sh (the `targo --unverified verify` alias)",
                gate::verb_names().join("|")
            );
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// verify — the `targo --unverified verify` verb
// ---------------------------------------------------------------------------

/// Dispatch to `tools/verify.sh`, forwarding every argument verbatim.
///
/// It exists only because a cargo alias can expand to nothing but a cargo
/// subcommand, and it holds no policy: everything the gate means lives in
/// `tools/verify.sh`.
///
/// Fail-closed: a missing or unrunnable script is a FAILURE, and a non-zero
/// status stays non-zero (a signal death, or a code whose low byte is 0, maps
/// to 1 rather than truncating to 0).
fn verify(args: &[String]) -> ExitCode {
    let script = workspace_root().join("tools").join("verify.sh");
    if !script.is_file() {
        eprintln!(
            "xtask verify: FAILED — {} missing; nothing verified",
            script.display()
        );
        return ExitCode::FAILURE;
    }
    let status = match Command::new(&script).args(args).status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "xtask verify: FAILED — could not run {}: {e}; nothing verified",
                script.display()
            );
            return ExitCode::FAILURE;
        }
    };
    match status.code() {
        Some(0) => ExitCode::SUCCESS,
        // Keep the gate's own code where it fits; never truncate non-zero to 0.
        Some(code) => match u8::try_from(code) {
            Ok(0) => ExitCode::FAILURE,
            Ok(byte) => ExitCode::from(byte),
            Err(_) => ExitCode::FAILURE,
        },
        None => {
            eprintln!("xtask verify: {} was killed by a signal", script.display());
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace layout
// ---------------------------------------------------------------------------

/// The workspace root: two levels above `<root>/crates/xtask`.
pub(crate) fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    // Panic-free: a too-shallow path falls back to the manifest dir.
    match manifest.parent().and_then(Path::parent) {
        Some(root) => root.to_path_buf(),
        None => manifest.to_path_buf(),
    }
}
