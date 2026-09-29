// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! aterm's build-graph tasks. Four subcommands:
//!
//!   * `harness-manifest`: `aterm_spec::harness_manifest` — every
//!     `#[kani::proof] fn` under `crates/`, written to
//!     `target/trust/harness-manifest.json` for `trust-ir spec-link`. aterm-gui's
//!     `spec_xref_closure` runs it.
//!   * `gate <verb>`: the checks the merge gate shells into this binary for
//!     (`gate.rs`).
//!   * `perf [--record]`: the measuring perf lanes and the same-box trend ledger
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
        Some("harness-manifest") => match write_harness_manifest() {
            Ok(path) => {
                eprintln!("xtask harness-manifest: wrote {}", path.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("xtask harness-manifest FAILED: {e}");
                ExitCode::FAILURE
            }
        },
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
                "usage: xtask <harness-manifest|gate <verb>|perf [--record]|verify [args…]>\n\
                 \n\
                 harness-manifest  enumerate #[kani::proof] fns -> target/trust/harness-manifest.json\n\
                 gate <verb>       the merge gate's xtask checks: {}\n\
                 perf [--record]   the perf lanes against tools/golden (--record rewrites them)\n\
                 verify [args…]    run THE gate, tools/verify.sh, forwarding every argument\n\
                 \x20                 (this is what the `targo --unverified verify` alias dispatches to)",
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
/// This exists ONLY because a cargo alias can expand to a cargo subcommand and
/// nothing else, so the repo's one gate needs a Rust hop to become a first-class
/// verb. It deliberately implements no policy: no default flags, no argument
/// rewriting, no "helpful" mode selection. Everything the gate means lives in
/// `tools/verify.sh`, and a second place that could disagree with it would
/// reintroduce exactly the ambiguity that script's header exists to remove.
///
/// Fail-closed in both directions that matter:
///   * a missing / unrunnable `tools/verify.sh` is a FAILURE, never a silent
///     success — `targo --unverified verify` must not be able to report "fine" without the
///     gate having run;
///   * a non-zero child status stays non-zero. A status that is non-zero but
///     not representable as a non-zero `u8` (a signal death, or an exit code
///     whose low byte is 0) maps to 1 rather than truncating to 0.
fn verify(args: &[String]) -> ExitCode {
    let script = workspace_root().join("tools").join("verify.sh");
    if !script.is_file() {
        eprintln!(
            "xtask verify: THE GATE IS MISSING — {} does not exist. Nothing was \
             verified; this is a failure, not a pass.",
            script.display()
        );
        return ExitCode::FAILURE;
    }
    let status = match Command::new(&script).args(args).status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "xtask verify: could not execute {}: {e}. Nothing was verified.",
                script.display()
            );
            return ExitCode::FAILURE;
        }
    };
    match status.code() {
        Some(0) => ExitCode::SUCCESS,
        // Preserve the gate's own exit code where it fits; never let a non-zero
        // status become 0 through a `as u8` truncation.
        Some(code) => match u8::try_from(code) {
            Ok(0) => ExitCode::FAILURE,
            Ok(byte) => ExitCode::from(byte),
            Err(_) => ExitCode::FAILURE,
        },
        // Killed by a signal: no exit code at all, and emphatically not a pass.
        None => {
            eprintln!("xtask verify: {} was killed by a signal", script.display());
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace layout
// ---------------------------------------------------------------------------

/// The workspace root (the dir that holds `crates/` and `target/`). `xtask`'s
/// manifest dir is `<root>/crates/xtask`, so the root is two levels up.
pub(crate) fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    // `<root>/crates/xtask` -> up two -> `<root>`. Handle the (type-reachable but
    // practically-impossible) shallow-path case without panicking: fall back to the
    // manifest dir itself rather than `.expect()`, so the function is panic-free.
    match manifest.parent().and_then(Path::parent) {
        Some(root) => root.to_path_buf(),
        None => manifest.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// harness-manifest
// ---------------------------------------------------------------------------

/// `aterm_spec::harness_manifest`, into `<root>/target/trust`, where
/// `aterm-gui`'s `spec_xref_closure` reads it.
fn write_harness_manifest() -> std::io::Result<PathBuf> {
    let root = workspace_root();
    aterm_spec::harness_manifest::write(&root, &root.join("target").join("trust"))
}
