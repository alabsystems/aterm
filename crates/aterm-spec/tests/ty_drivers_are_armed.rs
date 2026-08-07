// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every `ty check` in this workspace must arm the whole state space.
//!
//! This is a STRUCTURAL gate, and it exists because the same defect has now been
//! found twice, in a different driver each time:
//!
//! * **2026-07-30** — `ty_check_derived` ran with partial-order reduction on and
//!   booked a false proof; fixed by adding `arm_whole_space_check`, in one file.
//! * **2026-08-06** — `spec_xref_closure` hand-rolled its own argument list,
//!   never got that fix, and measured a dead-action set on a space POR had
//!   collapsed from 128 states to 1. Auditing that turned up **five more**
//!   unarmed `ty check` drivers, including the isolation family's
//!   security-boundary specs.
//!
//! Both fixes were correct and neither was structural: they patched the driver
//! that happened to be noticed. A third driver would have been found by a third
//! incident. So the rule is enforced over the source itself — if you invoke
//! `ty check`, `arm_whole_space_check` must be right there with it.
//!
//! Why it matters more than tidiness: the `ty` this machine discovers has an
//! unsound reduction. It prints `Model checking complete: No errors found
//! (exhaustive).` and exits 0 on a spec whose invariant is violated three steps
//! from `Init` (see `verify::arm_whole_space_check`). An unarmed driver is not
//! slightly weaker — it is a checker that answers "proved" about a space it
//! never entered.
//!
//! Deliberately a source scan and not a runtime hook: the failure mode is a
//! driver that never calls the shared helper at all, which no amount of
//! instrumentation inside the helper can observe.

use std::path::{Path, PathBuf};

/// The workspace root (this crate's manifest dir, one level up).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/.. is the workspace root")
        .to_path_buf()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        let p = e.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if p.is_dir() {
            // `target/` is build output; `vendor/` is not ours to police.
            if !matches!(name, "target" | "vendor" | ".git" | "node_modules") {
                rust_sources(&p, out);
            }
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// A `ty check` invocation: the file, the 1-based line of the `.arg("check")`,
/// and whether the enclosing region arms the whole space.
struct Driver {
    file: String,
    line: usize,
    armed: bool,
    exempt: bool,
}

/// The ONE legitimate reason to run `ty check` unarmed: a driver whose subject is
/// the CHECKER rather than a model. Spelled as a marker at the call site so the
/// exemption is visible where it applies and greppable from anywhere, and
/// counted below so exemptions cannot quietly multiply into the hole this gate
/// exists to close.
const EXEMPTION: &str = "ty-driver-unarmed:";

/// The window, in lines, within which the arming must appear. A driver builds
/// its `Command` and runs it in one place; 40 lines is generous for that and far
/// short of reaching an unrelated function.
const WINDOW: usize = 40;

fn scan(path: &Path, rel: &str) -> Vec<Driver> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        // The shape every driver shares: a `check` subcommand argument.
        if !l.contains(".arg(\"check\")") {
            continue;
        }
        // `cargo check` drivers are not model checks.
        let lo = i.saturating_sub(WINDOW);
        let hi = (i + WINDOW).min(lines.len());
        let region = lines[lo..hi].join("\n");
        if region.contains("cargo") && !region.contains("ty") {
            continue;
        }
        found.push(Driver {
            file: rel.to_string(),
            line: i + 1,
            armed: region.contains("arm_whole_space_check"),
            exempt: region.contains(EXEMPTION),
        });
    }
    found
}

#[test]
fn every_ty_check_driver_arms_the_whole_space() {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_sources(&root.join("crates"), &mut files);
    assert!(
        files.len() > 100,
        "the source scan found only {} files — it is not looking where it thinks it is",
        files.len()
    );

    let mut drivers = Vec::new();
    for f in &files {
        let rel = f
            .strip_prefix(&root)
            .unwrap_or(f)
            .to_string_lossy()
            .into_owned();
        drivers.extend(scan(f, &rel));
    }

    // The scan must actually find the known drivers, or a rename silently turns
    // this gate into a no-op that passes forever.
    assert!(
        drivers.len() >= 5,
        "expected the workspace's several `ty check` drivers, found {} — the scan pattern \
         has drifted and this gate is now vacuous",
        drivers.len()
    );

    // Exemptions are allowed but BOUNDED: exactly one driver may test the
    // checker itself. A second appearing without this gate being edited would
    // mean the marker had become a way to opt out of verification.
    let exempt: Vec<&Driver> = drivers.iter().filter(|d| d.exempt && !d.armed).collect();
    assert_eq!(
        exempt.len(),
        1,
        "expected exactly ONE `{EXEMPTION}` driver (the checker canary), found {}: {:?}",
        exempt.len(),
        exempt
            .iter()
            .map(|d| format!("{}:{}", d.file, d.line))
            .collect::<Vec<_>>()
    );
    assert!(
        exempt[0].file.contains("ty_checker_canary"),
        "the sole unarmed-driver exemption must be the checker canary, not {}:{}",
        exempt[0].file,
        exempt[0].line
    );

    let unarmed: Vec<String> = drivers
        .iter()
        .filter(|d| !d.armed && !d.exempt)
        .map(|d| format!("  {}:{}", d.file, d.line))
        .collect();
    assert!(
        unarmed.is_empty(),
        "UNARMED `ty check` driver(s) — each runs the model checker with its default \
         partial-order reduction, and the `ty` this workspace discovers is UNSOUND under it \
         (it prints \"No errors found (exhaustive)\", exit 0, on a spec whose invariant is \
         violated three steps from Init). Route the command through \
         `aterm_spec::verify::arm_whole_space_check(&mut cmd)`:\n{}",
        unarmed.join("\n")
    );
}
