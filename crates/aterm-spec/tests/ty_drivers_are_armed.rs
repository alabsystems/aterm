// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every `ty check` in this workspace must arm the whole state space.
//!
//! A STRUCTURAL gate: an unarmed driver was found twice, in a different driver
//! each time, so the rule is enforced over the source itself — if you invoke
//! `ty check`, `arm_whole_space_check` must be right there with it. The
//! discovered `ty` can have an unsound reduction that prints `No errors found
//! (exhaustive).` and exits 0 on a spec violated three steps from `Init` (see
//! `verify::arm_whole_space_check`), so an unarmed driver answers "proved"
//! about a space it never entered. A source scan, not a runtime hook: the
//! failure is a driver that never calls the shared helper at all.
//!
//! **The second rule: every `ty` spawn goes through `verify::ty_output`.** `ty`
//! sizes its memory budget by the live `ty`/`cargo`/`rustc` processes, and a
//! search the budget stops still exits 0 under an `exhaustive` header.
//! `ty_output` classifies that stop (exit 5, or a `Model checking stopped:`
//! line) as an INCOMPLETE `Err`; a driver's own `Command::output()` reads it
//! as a pass. It covers `ty trace validate` drivers too. The one other door is
//! `verify::ty_child`, the `--cases` co-process spawn: it shares `ty_output`'s
//! environment, and its client reads no verdict it was not sent as one.

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
    /// A `ty check` (held to the arming rule); `false` = `ty trace validate`.
    check: bool,
    armed: bool,
    exempt: bool,
    /// The spawn runs through `verify::ty_output` (the fail-closed wrapper).
    routed: bool,
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

/// Whether a region names the CHECKER — `ty` as a word or an identifier prefix
/// (`ty`, `"ty"`, `ty_bin`, `ty_path`) — rather than merely containing the two
/// letters: a substring test reads "type error" or "exactly" in a `cargo
/// check` driver's comments as an unarmed model check.
fn mentions_ty(region: &str) -> bool {
    region
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| w == "ty" || w.starts_with("ty_"))
}

/// Whether a region CALLS `ty_output` — or `ty_child`, the `--cases`
/// co-process door with the same environment, whose client protocol fails
/// closed on its own — the name as a whole identifier, so
/// `pty_output(` or `dirty_output(` beside an unrouted driver never reads as
/// its routing.
fn calls_ty_output(region: &str) -> bool {
    ["ty_output(", "ty_child("].iter().any(|name| {
        region.match_indices(name).any(|(at, _)| {
            !region[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    })
}

fn scan(path: &Path, rel: &str) -> Vec<Driver> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        // `ty trace validate` drivers: held to the routing rule only (a
        // two-state trace takes no whole-space arming). A comment that quotes
        // the shape is not a driver.
        if l.contains(".arg(\"trace\")") && !l.trim_start().starts_with("//") {
            let lo = i.saturating_sub(WINDOW);
            let hi = (i + WINDOW).min(lines.len());
            let region = lines[lo..hi].join("\n");
            if mentions_ty(&region) {
                found.push(Driver {
                    file: rel.to_string(),
                    line: i + 1,
                    check: false,
                    armed: false,
                    exempt: false,
                    routed: calls_ty_output(&region),
                });
            }
            continue;
        }
        // The shape every `ty check` driver shares: a `check` subcommand
        // argument. A comment quoting that shape (this file's own `Driver` doc
        // does) is not a driver — counted, its window can borrow this file's
        // exemption marker and read as a second exempt driver.
        if !l.contains(".arg(\"check\")") || l.trim_start().starts_with("//") {
            continue;
        }
        // `cargo check` drivers are not model checks.
        let lo = i.saturating_sub(WINDOW);
        let hi = (i + WINDOW).min(lines.len());
        let region = lines[lo..hi].join("\n");
        // A `cargo` driver is one that RUNS cargo — `Command::new("cargo")`,
        // `$CARGO`, or the workspace's cargo_bin helper — not one whose
        // comments mention the word, which a ty driver's prose could carry.
        let runs_cargo = region.contains("Command::new(\"cargo\")")
            || region.contains("env!(\"CARGO\")")
            || region.contains("var_os(\"CARGO\")")
            || region.contains("var(\"CARGO\")")
            || region.contains("cargo_bin(")
            // xtask's cross lanes: a rustup toolchain's own cargo, by path.
            || region.contains(".cargo()");
        if runs_cargo && !mentions_ty(&region) {
            continue;
        }
        found.push(Driver {
            file: rel.to_string(),
            line: i + 1,
            check: true,
            armed: region.contains("arm_whole_space_check"),
            exempt: region.contains(EXEMPTION),
            routed: calls_ty_output(&region),
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
    // this gate into a no-op that passes forever. Only the `ty check` drivers
    // count: the scan returns `ty trace validate` drivers too (for the routing
    // rule), and those alone could satisfy a count of every driver while
    // this rule's pattern found nothing.
    let checks = drivers.iter().filter(|d| d.check).count();
    assert!(
        checks >= 5,
        "expected the workspace's several `ty check` drivers, found {checks} — the scan \
         pattern has drifted and this gate is now vacuous"
    );

    // Exemptions are allowed but BOUNDED: exactly one driver may test the
    // checker itself. A second appearing without this gate being edited would
    // mean the marker had become a way to opt out of verification.
    let exempt: Vec<&Driver> = drivers
        .iter()
        .filter(|d| d.check && d.exempt && !d.armed)
        .collect();
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
        .filter(|d| d.check && !d.armed && !d.exempt)
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

/// The routing scan reads a call, not a substring: `pty_output(` (a real name
/// in aterm-gui) is not `ty_output(`.
#[test]
fn routing_is_read_off_the_whole_name() {
    assert!(calls_ty_output("let out = verify::ty_output(&mut cmd)"));
    assert!(calls_ty_output("aterm_spec::verify::ty_output(&mut cmd)?"));
    assert!(calls_ty_output("ty_output(&mut cmd)"));
    assert!(
        calls_ty_output("let child = ty_child(&mut cmd)?;") && !calls_ty_output("pty_child(&x)")
    );
    assert!(!calls_ty_output(
        "fn later_pty_output() {}\nlet o = pty_output(&x);"
    ));
    assert!(!calls_ty_output("let out = cmd.output();"));
}

/// THE ROUTING RULE: every `ty check` and `ty trace validate` driver under
/// `crates/` spawns through `aterm_spec::verify::ty_output`. None is exempt —
/// the checker canary tests `ty`'s own reduction, and a search stopped short
/// of the violation it expects must fail as INCOMPLETE like any other.
#[test]
fn every_ty_spawn_is_routed_through_ty_output() {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_sources(&root.join("crates"), &mut files);
    let mut drivers = Vec::new();
    for f in &files {
        let rel = f
            .strip_prefix(&root)
            .unwrap_or(f)
            .to_string_lossy()
            .into_owned();
        drivers.extend(scan(f, &rel));
    }
    // Both shapes must still be found, or the scan has drifted into a no-op.
    let checks = drivers.iter().filter(|d| d.check).count();
    let traces = drivers.iter().filter(|d| !d.check).count();
    assert!(
        checks >= 5 && traces >= 3,
        "expected the workspace's `ty check` and `ty trace validate` drivers, found {checks} \
         and {traces} — the scan pattern has drifted and this gate is now vacuous"
    );
    let unrouted: Vec<String> = drivers
        .iter()
        .filter(|d| !d.routed)
        .map(|d| format!("  {}:{}", d.file, d.line))
        .collect();
    assert!(
        unrouted.is_empty(),
        "`ty` spawned outside `aterm_spec::verify::ty_output` — every `ty` spawn must be \
         routed for fail-closed classification: a search `ty`'s memory budget stops still \
         exits 0 (a false green) unless `ty_output` turns it into an INCOMPLETE `Err`. \
         Replace `cmd.output()` with `aterm_spec::verify::ty_output(&mut cmd)`:\n{}",
        unrouted.join("\n")
    );
}
