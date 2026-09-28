// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! aterm's build-graph tasks. Four subcommands:
//!
//!   * `harness-manifest`: enumerate every REAL `#[kani::proof] fn` across the
//!     workspace `crates/` and write a `HarnessManifest` JSON to
//!     `target/trust/harness-manifest.json` in the shape `trust-ir spec-link
//!     --harness-manifest` expects (`{"harnesses":[{"name","span"}]}`) — the data
//!     trust-ir's L1 resolves `proof_name` against. aterm-gui's
//!     `spec_xref_closure` runs it.
//!   * `gate <verb>`: the checks the merge gate shells into this binary for
//!     (`gate.rs`).
//!   * `perf [--record]`: the measuring perf lanes and the same-box trend ledger
//!     (`perf.rs`). It has no automatic caller; it measures.
//!   * `verify [args…]`: `tools/verify.sh`, the `cargo verify` alias's target.

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
                 \x20                 (this is what the `cargo verify` alias dispatches to)",
                gate::verb_names().join("|")
            );
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// verify — the `cargo verify` verb
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
///     success — `cargo verify` must not be able to report "fine" without the
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
// harness-manifest (finding 1a)
// ---------------------------------------------------------------------------

/// One `#[kani::proof]` harness: its fn name + a `file:line` span (opaque to L1,
/// which matches only on `name`).
struct HarnessEntry {
    name: String,
    span: String,
}

/// Enumerate every `#[kani::proof] fn <name>` under the workspace `crates/` and write
/// the `HarnessManifest` JSON. Returns the path written. The scan is a line walk:
/// a `#[kani::proof]` attribute line arms the next `fn <ident>` (allowing intervening
/// `#[kani::…]` / `#[cfg(kani)]` attribute lines), exactly as the harnesses are
/// authored. Names are de-duplicated (a harness name is the L1 key, unique per build).
fn write_harness_manifest() -> std::io::Result<PathBuf> {
    let root = workspace_root();
    let mut entries: Vec<HarnessEntry> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates"), &mut files)?;
    files.sort();
    for file in &files {
        let text = std::fs::read_to_string(file)?;
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned();
        let lines: Vec<&str> = text.lines().collect();
        let mut armed = false;
        for (i, raw) in lines.iter().enumerate() {
            let line = raw.trim_start();
            if line.starts_with("#[kani::proof") {
                armed = true;
                continue;
            }
            if armed {
                // Skip further attribute lines (#[kani::should_panic], #[cfg(kani)], …)
                // and blank/comment lines between the attr and the fn.
                if line.starts_with("#[") || line.is_empty() || line.starts_with("//") {
                    continue;
                }
                if let Some(name) = parse_fn_name(line) {
                    if seen.insert(name.clone()) {
                        entries.push(HarnessEntry {
                            name,
                            span: format!("{rel}:{}:1", i + 1),
                        });
                    }
                    armed = false;
                } else {
                    // A non-attr, non-fn line after the attr — not a harness; disarm.
                    armed = false;
                }
            }
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    let out_dir = root.join("target").join("trust");
    std::fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join("harness-manifest.json");
    std::fs::write(&out_path, render_manifest_json(&entries))?;
    eprintln!("xtask: {} kani harness(es) enumerated", entries.len());
    Ok(out_path)
}

/// Recursive `*.rs` collection (skips `target/` + hidden dirs), the census
/// library's walk, so the manifest reads the tree the censuses read.
use aterm_census::collect_rs_files;

/// Extract `<ident>` from a `(pub )?(unsafe )?fn <ident>…` line; `None` otherwise.
fn parse_fn_name(line: &str) -> Option<String> {
    let mut rest = line;
    for kw in [
        "pub ",
        "pub(crate) ",
        "unsafe ",
        "const ",
        "async ",
        "extern ",
    ] {
        if let Some(s) = rest.strip_prefix(kw) {
            rest = s.trim_start();
        }
    }
    let rest = rest.strip_prefix("fn ")?;
    let ident: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() { None } else { Some(ident) }
}

/// Render the `HarnessManifest` JSON in the documented shape. Hand-rolled (no serde
/// dep): each `name`/`span` is JSON-escaped (both are plain identifiers / file paths
/// here, but escape defensively).
fn render_manifest_json(entries: &[HarnessEntry]) -> String {
    let mut s = String::from("{\n  \"harnesses\": [");
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        // Spelled as direct `push_str`s, byte-identical to the former
        // `format!("\n    {{ \"name\": {}, \"span\": {} }}", …)`:
        // `fmt::Arguments::new` is an unmodeled construct the strict gate's
        // native TrustIr lowering refuses, which failed this function's
        // panic-freedom proof outright.
        s.push_str("\n    { \"name\": ");
        s.push_str(&json_str(&e.name));
        s.push_str(", \"span\": ");
        s.push_str(&json_str(&e.span));
        s.push_str(" }");
    }
    if !entries.is_empty() {
        s.push_str("\n  ");
    }
    s.push_str("]\n}\n");
    s
}

fn json_str(s: &str) -> String {
    // Capacity is a pure allocation hint — the escaped output is identical
    // with any starting capacity (`push`/`push_str` grow on demand) — so
    // clamping it is behavior-preserving. The `len < 4096` check dominates
    // each branch-local `with_capacity` call (a joined `cap` variable would
    // lose the bound at the phi node), which discharges both the `len + 2`
    // overflow obligation and the L0 unbounded-allocation budget. Real
    // inputs are fn identifiers and `file:line` spans, far below 4 KiB, so
    // the hint stays exact for every input the callers produce.
    let len = s.len();
    let mut out = if len < 4096 {
        String::with_capacity(len + 2)
    } else {
        String::with_capacity(4096)
    };
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
